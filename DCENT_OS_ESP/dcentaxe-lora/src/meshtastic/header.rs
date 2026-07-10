// SPDX-License-Identifier: GPL-3.0-or-later
//! The 16-byte Meshtastic over-the-air packet header (`PacketHeader`).
//!
//! Every Meshtastic LoRa packet is a fixed 16-byte header followed by the
//! (usually encrypted) protobuf payload. Getting the byte layout exactly right
//! is the difference between "a genuine Router on the mesh" and "a node nobody
//! hears", so the layout is pinned by tests:
//!
//! | bytes | field        | encoding                                   |
//! |-------|--------------|--------------------------------------------|
//! | 0..4  | `to`         | destination NodeNum, little-endian         |
//! | 4..8  | `from`       | source NodeNum, little-endian              |
//! | 8..12 | `id`         | packet id, little-endian                   |
//! | 12    | `flags`      | bit0-2 hop_limit, bit3 want_ack, bit4 via_mqtt, bit5-7 hop_start |
//! | 13    | `channel`    | channel hash (see [`super::channel`])      |
//! | 14    | `next_hop`   | low byte of the intended next-hop NodeNum (0 = unset) |
//! | 15    | `relay_node` | low byte of the relaying NodeNum (0 = unset)          |
//!
//! `next_hop`/`relay_node` are the v2.5+ next-hop routing bytes; older firmware
//! sent them as 0 and ignores them, so writing them stays backward-compatible.

/// The broadcast destination NodeNum (`to == 0xFFFFFFFF`).
pub const BROADCAST_ADDR: u32 = 0xFFFF_FFFF;

/// Fixed on-air header length in bytes.
pub const HEADER_LEN: usize = 16;

/// A decoded Meshtastic packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Destination NodeNum ([`BROADCAST_ADDR`] for a flood).
    pub to: u32,
    /// Source NodeNum (the originator).
    pub from: u32,
    /// Packet id (also the AES-CTR nonce input; unique per originated packet).
    pub id: u32,
    /// Remaining hop budget (0..=7). A relay decrements it; 0 = do not relay.
    pub hop_limit: u8,
    /// Sender wants an ack.
    pub want_ack: bool,
    /// Packet was seen via an MQTT gateway.
    pub via_mqtt: bool,
    /// Hop budget the originator started with (0..=7) — lets a receiver compute
    /// how many hops a packet has travelled (`hop_start - hop_limit`).
    pub hop_start: u8,
    /// Channel hash (see [`super::channel::Channel::hash`]).
    pub channel: u8,
    /// Low byte of the intended next-hop NodeNum (v2.5+; 0 = unset/flood).
    pub next_hop: u8,
    /// Low byte of the relaying NodeNum (v2.5+; 0 = unset).
    pub relay_node: u8,
}

impl PacketHeader {
    /// A broadcast header from `from`/`id` on channel `channel` with `hop_limit`
    /// hops (also recorded as `hop_start`); next-hop bytes unset.
    pub fn broadcast(from: u32, id: u32, channel: u8, hop_limit: u8) -> Self {
        // Saturating clamp into the 3-bit field: an out-of-range request means
        // "as many hops as allowed" (7), which is safer than masking it (200 &
        // 0x07 == 0 would silently make the packet un-relayable).
        let h = hop_limit.min(0x07);
        Self {
            to: BROADCAST_ADDR,
            from,
            id,
            hop_limit: h,
            want_ack: false,
            via_mqtt: false,
            hop_start: h,
            channel,
            next_hop: 0,
            relay_node: 0,
        }
    }

    /// `true` when this packet is addressed to everyone.
    pub fn is_broadcast(&self) -> bool {
        self.to == BROADCAST_ADDR
    }

    /// Hops travelled so far (`hop_start - hop_limit`, saturating).
    pub fn hops_away(&self) -> u8 {
        self.hop_start.saturating_sub(self.hop_limit)
    }

    /// Pack the flags byte: `hop_limit(0-2) | want_ack(3) | via_mqtt(4) | hop_start(5-7)`.
    pub fn flags_byte(&self) -> u8 {
        (self.hop_limit & 0x07)
            | ((self.want_ack as u8) << 3)
            | ((self.via_mqtt as u8) << 4)
            | ((self.hop_start & 0x07) << 5)
    }

    /// Encode to the 16 on-air header bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.to.to_le_bytes());
        b[4..8].copy_from_slice(&self.from.to_le_bytes());
        b[8..12].copy_from_slice(&self.id.to_le_bytes());
        b[12] = self.flags_byte();
        b[13] = self.channel;
        b[14] = self.next_hop;
        b[15] = self.relay_node;
        b
    }

    /// Decode the header from the front of a received packet. Returns `None` if
    /// there are fewer than [`HEADER_LEN`] bytes (a runt frame — never panic).
    pub fn decode(buf: &[u8]) -> Option<PacketHeader> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let to = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let from = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let id = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let flags = buf[12];
        Some(PacketHeader {
            to,
            from,
            id,
            hop_limit: flags & 0x07,
            want_ack: flags & 0x08 != 0,
            via_mqtt: flags & 0x10 != 0,
            hop_start: (flags >> 5) & 0x07,
            channel: buf[13],
            next_hop: buf[14],
            relay_node: buf[15],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_header_round_trips_and_layout_is_pinned() {
        let h = PacketHeader::broadcast(0x1122_3344, 0xaabb_ccdd, 0x08, 3);
        let bytes = h.encode();
        assert_eq!(
            bytes,
            [
                0xff,
                0xff,
                0xff,
                0xff, // to = broadcast (LE)
                0x44,
                0x33,
                0x22,
                0x11, // from (LE)
                0xdd,
                0xcc,
                0xbb,
                0xaa,        // id (LE)
                0b0110_0011, // flags: hop_start=3 (bits5-7), hop_limit=3 (bits0-2)
                0x08,        // channel hash
                0x00,        // next_hop
                0x00,        // relay_node
            ]
        );
        assert_eq!(PacketHeader::decode(&bytes), Some(h));
    }

    #[test]
    fn flags_pack_all_bits() {
        let h = PacketHeader {
            to: 1,
            from: 2,
            id: 3,
            hop_limit: 5,
            want_ack: true,
            via_mqtt: true,
            hop_start: 7,
            channel: 0xAB,
            next_hop: 0x10,
            relay_node: 0x20,
        };
        // hop_limit=5 (0b101), want_ack (bit3), via_mqtt (bit4), hop_start=7 (bits5-7).
        assert_eq!(h.flags_byte(), 0b1111_1101);
        assert_eq!(PacketHeader::decode(&h.encode()), Some(h));
    }

    #[test]
    fn hop_fields_clamped_and_masked_to_three_bits() {
        // The broadcast constructor saturating-clamps an out-of-range hop_limit.
        let h = PacketHeader::broadcast(1, 2, 0, 200);
        assert_eq!(h.hop_limit, 0x07, "hop_limit clamped to 3 bits");
        assert_eq!(h.hop_start, 0x07);
        // And a hostile all-ones flags byte decodes to masked 3-bit values without
        // panicking (untrusted radio bytes).
        let mut bytes = h.encode();
        bytes[12] = 0xFF;
        let d = PacketHeader::decode(&bytes).unwrap();
        assert_eq!(d.hop_limit, 0x07);
        assert_eq!(d.hop_start, 0x07);
        assert!(d.want_ack && d.via_mqtt);
    }

    #[test]
    fn hops_away_is_start_minus_limit() {
        let mut h = PacketHeader::broadcast(1, 2, 0, 3);
        assert_eq!(h.hops_away(), 0, "freshly originated: 0 hops away");
        h.hop_limit = 1; // travelled 2 hops
        assert_eq!(h.hops_away(), 2);
    }

    #[test]
    fn decode_rejects_runt_frame() {
        assert_eq!(PacketHeader::decode(&[0u8; 15]), None);
        assert_eq!(PacketHeader::decode(&[]), None);
        // Exactly 16 bytes is the minimum accepted.
        assert!(PacketHeader::decode(&[0u8; 16]).is_some());
    }

    #[test]
    fn is_broadcast_detects_broadcast_addr() {
        assert!(PacketHeader::broadcast(1, 2, 0, 3).is_broadcast());
        let mut h = PacketHeader::broadcast(1, 2, 0, 3);
        h.to = 0xdead_beef;
        assert!(!h.is_broadcast());
    }
}
