// SPDX-License-Identifier: GPL-3.0-or-later
//! Meshtastic managed-flood router: decide whether/when to rebroadcast a
//! received Meshtastic packet so a DCENT_axe is a well-behaved **Router** on the
//! mesh, not a storm source.
//!
//! This is the Meshtastic-packet twin of [`crate::flood`] (which routes the
//! native `$DCM` frames). The **timing policy is shared** — it reuses
//! [`crate::flood::contention_delay_ms`] so both protocols back off with the same
//! SNR-weighted "weakest-hears-relays-first" curve — but the **dedup key and hop
//! model differ**: Meshtastic dedups on the 64-bit `(from, id)` pair and carries
//! the hop budget in the header's `hop_limit`, decrementing it and stamping
//! `relay_node` on each forward (per the upstream algorithm).
//!
//! Clock-free by construction, exactly like [`crate::flood`]: the caller passes a
//! monotonic `now_ms`, and the router only decides *whether* and computes *when*.

use crate::flood::{contention_delay_ms, RelayRole};
use crate::meshtastic::header::PacketHeader;
use std::collections::VecDeque;

/// Default `(from, id)` dedup ring capacity.
pub const DEFAULT_SEEN_CAP: usize = 48;
/// Default cap on concurrently-pending rebroadcasts. Lower than the `$DCM`
/// planner's because each Meshtastic pending entry also holds the packet payload
/// (bounded RAM on the 300 KB entry board).
pub const DEFAULT_PENDING_CAP: usize = 16;

/// Why a received Meshtastic packet was not turned into a pending rebroadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    /// We originated this packet — never relay our own traffic.
    OwnOrigin,
    /// Already seen this `(from, id)` and nothing was pending for it.
    Duplicate,
    /// This node's [`RelayRole`] does not rebroadcast (a `Client` leaf).
    RoleClient,
    /// `hop_limit == 0`; the packet has used its whole hop budget.
    HopExhausted,
    /// A direct message addressed to us — it reached its destination, so we
    /// process it locally but do not forward it.
    AddressedToUs,
}

/// The outcome of feeding a received packet to the router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayDecision {
    /// A rebroadcast was scheduled; emit it via [`MeshtasticRouter::due`] at/after
    /// `due_at_ms`.
    Scheduled { due_at_ms: u64 },
    /// A duplicate arrived that cancelled a still-pending rebroadcast — a
    /// neighbour already covered this packet, so this node stays quiet.
    Canceled,
    /// Not scheduled; see [`SuppressReason`].
    Suppressed(SuppressReason),
}

/// A rebroadcast ready to transmit: the hop-decremented header + the verbatim
/// (still-encrypted) payload a relay forwards untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPacket {
    pub header: PacketHeader,
    pub payload: Vec<u8>,
}

impl RelayPacket {
    /// The full on-air bytes: 16-byte header followed by the payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(crate::meshtastic::header::HEADER_LEN + self.payload.len());
        v.extend_from_slice(&self.header.encode());
        v.extend_from_slice(&self.payload);
        v
    }
}

struct Pending {
    key: (u32, u32),
    due_at_ms: u64,
    packet: RelayPacket,
}

/// Per-node deterministic jitter (0..=[`crate::flood::JITTER_MS`]) from
/// `(from, id, own)` — FNV-1a, no RNG/clock. `own` is folded in so two receivers
/// of the same packet compute different jitter and never fire in lockstep.
pub fn jitter_ms(from: u32, id: u32, own: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let feed = |h: &mut u64, b: u8| {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for b in from.to_le_bytes() {
        feed(&mut h, b);
    }
    for b in id.to_le_bytes() {
        feed(&mut h, b);
    }
    for b in own.to_le_bytes() {
        feed(&mut h, b);
    }
    h % (crate::flood::JITTER_MS + 1)
}

/// Managed-flood router for Meshtastic packets. Bounded + clock-free.
#[derive(Debug, Clone)]
pub struct MeshtasticRouter {
    own: u32,
    role: RelayRole,
    seen: VecDeque<(u32, u32)>,
    pending: VecDeque<Pending>,
    seen_cap: usize,
    pending_cap: usize,
}

// Manual Clone for Pending (RelayPacket is Clone; derive on the router needs it).
impl Clone for Pending {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            due_at_ms: self.due_at_ms,
            packet: self.packet.clone(),
        }
    }
}

impl core::fmt::Debug for Pending {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pending")
            .field("key", &self.key)
            .field("due_at_ms", &self.due_at_ms)
            .finish()
    }
}

impl MeshtasticRouter {
    /// A router for `own` node in `role` with default caps.
    pub fn new(own: u32, role: RelayRole) -> Self {
        Self::with_caps(own, role, DEFAULT_SEEN_CAP, DEFAULT_PENDING_CAP)
    }

    /// A router with explicit dedup / pending caps (each clamped to ≥ 1).
    pub fn with_caps(own: u32, role: RelayRole, seen_cap: usize, pending_cap: usize) -> Self {
        Self {
            own,
            role,
            seen: VecDeque::new(),
            pending: VecDeque::new(),
            seen_cap: seen_cap.max(1),
            pending_cap: pending_cap.max(1),
        }
    }

    pub fn role(&self) -> RelayRole {
        self.role
    }

    pub fn set_role(&mut self, role: RelayRole) {
        self.role = role;
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn seen_contains(&self, key: (u32, u32)) -> bool {
        self.seen.iter().any(|&k| k == key)
    }

    fn pending_pos(&self, key: (u32, u32)) -> Option<usize> {
        self.pending.iter().position(|p| p.key == key)
    }

    fn record_seen(&mut self, key: (u32, u32)) {
        if self.seen.len() >= self.seen_cap {
            self.seen.pop_front();
        }
        self.seen.push_back(key);
    }

    fn push_pending(&mut self, entry: Pending) {
        if self.pending.len() >= self.pending_cap {
            self.pending.pop_front();
        }
        self.pending.push_back(entry);
    }

    /// Feed a received packet (header + still-encrypted `payload`, with its
    /// measured `snr_db`) to the router at monotonic time `now_ms`.
    pub fn on_receive(
        &mut self,
        header: &PacketHeader,
        payload: &[u8],
        snr_db: f32,
        now_ms: u64,
    ) -> RelayDecision {
        let key = (header.from, header.id);

        // 1. A duplicate of a packet we were about to relay ⇒ a neighbour covered
        //    it; cancel our pending rebroadcast and stay quiet.
        if let Some(pos) = self.pending_pos(key) {
            self.pending.remove(pos);
            return RelayDecision::Canceled;
        }

        // 2. Already seen, nothing pending ⇒ plain duplicate.
        if self.seen_contains(key) {
            return RelayDecision::Suppressed(SuppressReason::Duplicate);
        }

        // 3. First sighting — record for dedup regardless of what we decide next.
        self.record_seen(key);

        // 4. Never relay our own origination.
        if header.from == self.own {
            return RelayDecision::Suppressed(SuppressReason::OwnOrigin);
        }

        // 5. Role gate — a Client observes but never floods.
        if !self.role.rebroadcasts() {
            return RelayDecision::Suppressed(SuppressReason::RoleClient);
        }

        // 6. Hop budget exhausted.
        if header.hop_limit == 0 {
            return RelayDecision::Suppressed(SuppressReason::HopExhausted);
        }

        // 7. A DM that reached us is at its destination — process, don't forward.
        //    (A broadcast's `to` is 0xFFFFFFFF and never equals our node id.)
        if header.to == self.own {
            return RelayDecision::Suppressed(SuppressReason::AddressedToUs);
        }

        // 8. Schedule the hop-decremented relay after the SNR window + jitter.
        let mut relayed = *header;
        relayed.hop_limit -= 1;
        relayed.relay_node = (self.own & 0xFF) as u8;
        let delay =
            contention_delay_ms(snr_db, self.role) + jitter_ms(header.from, header.id, self.own);
        let due_at_ms = now_ms.saturating_add(delay);
        self.push_pending(Pending {
            key,
            due_at_ms,
            packet: RelayPacket {
                header: relayed,
                payload: payload.to_vec(),
            },
        });
        RelayDecision::Scheduled { due_at_ms }
    }

    /// Remove and return every pending rebroadcast whose slot has arrived
    /// (`due_at_ms <= now_ms`), in ascending due order — ready to hand to the
    /// radio TX path.
    pub fn due(&mut self, now_ms: u64) -> Vec<RelayPacket> {
        let mut ready: Vec<(u64, usize)> = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, p)| p.due_at_ms <= now_ms)
            .map(|(i, p)| (p.due_at_ms, i))
            .collect();
        ready.sort_by_key(|&(due, i)| (due, i));
        let out: Vec<RelayPacket> = ready
            .iter()
            .map(|&(_, i)| self.pending[i].packet.clone())
            .collect();
        let mut idxs: Vec<usize> = ready.iter().map(|&(_, i)| i).collect();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        for i in idxs {
            self.pending.remove(i);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flood::{JITTER_MS, SLOT_MS, SNR_CEIL_DB, SNR_FLOOR_DB, WINDOW_SLOTS};
    use crate::meshtastic::header::{PacketHeader, BROADCAST_ADDR};

    const OWN: u32 = 0x0000_00a1;
    const SRC: u32 = 0x0000_00b2;

    fn bcast(from: u32, id: u32, hop: u8) -> PacketHeader {
        PacketHeader::broadcast(from, id, 0x08, hop)
    }

    #[test]
    fn schedules_fresh_broadcast_hop_decremented_and_stamped() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        match r.on_receive(&bcast(SRC, 1, 3), b"payload", 0.0, 1_000) {
            RelayDecision::Scheduled { due_at_ms } => assert!(due_at_ms >= 1_000),
            other => panic!("expected Scheduled, got {other:?}"),
        }
        assert_eq!(r.pending_len(), 1);
        let out = r.due(1_000_000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].header.hop_limit, 2, "hop_limit 3 -> 2");
        assert_eq!(out[0].header.hop_start, 3, "hop_start preserved");
        assert_eq!(
            out[0].header.relay_node,
            (OWN & 0xFF) as u8,
            "relay_node stamped"
        );
        assert_eq!(out[0].payload, b"payload", "payload forwarded verbatim");
        assert_eq!(out[0].header.from, SRC, "originator preserved");
    }

    #[test]
    fn relay_packet_to_bytes_is_header_plus_payload() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        r.on_receive(&bcast(SRC, 9, 3), b"XY", -5.0, 0);
        let out = r.due(1_000_000);
        let bytes = out[0].to_bytes();
        assert_eq!(bytes.len(), 16 + 2);
        assert_eq!(&bytes[16..], b"XY");
        // Header round-trips out of the serialized bytes.
        let hdr = PacketHeader::decode(&bytes).unwrap();
        assert_eq!(hdr.hop_limit, 2);
        assert_eq!(hdr.relay_node, (OWN & 0xFF) as u8);
    }

    #[test]
    fn weaker_snr_relays_before_stronger() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        r.on_receive(&bcast(SRC, 1, 3), b"a", SNR_CEIL_DB, 0); // strong → later
        r.on_receive(&bcast(0xC3, 2, 3), b"b", SNR_FLOOR_DB, 0); // weak → slot 0
                                                                 // Only the weakest-SNR (slot 0 + jitter ≤ JITTER_MS) is due early.
        let early = r.due(SLOT_MS + JITTER_MS);
        assert_eq!(early.len(), 1);
        assert_eq!(early[0].header.id, 2, "weakest link relays first");
        let rest = r.due(1_000_000);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].header.id, 1);
    }

    #[test]
    fn duplicate_before_due_cancels_pending() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        assert!(matches!(
            r.on_receive(&bcast(SRC, 5, 3), b"p", 0.0, 1_000),
            RelayDecision::Scheduled { .. }
        ));
        assert_eq!(r.pending_len(), 1);
        // A neighbour's relay of the same (from,id) arrives before our slot.
        assert_eq!(
            r.on_receive(&bcast(SRC, 5, 2), b"p", -5.0, 1_010),
            RelayDecision::Canceled
        );
        assert_eq!(r.pending_len(), 0);
        assert!(r.due(1_000_000).is_empty());
    }

    #[test]
    fn duplicate_after_flush_is_suppressed() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        r.on_receive(&bcast(SRC, 5, 3), b"p", 0.0, 1_000);
        r.due(1_000_000); // flush pending, keep seen record
        assert_eq!(
            r.on_receive(&bcast(SRC, 5, 3), b"p", 0.0, 2_000),
            RelayDecision::Suppressed(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn own_origin_and_role_and_hop_and_addressed_gates() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        // Our own reflected packet.
        assert_eq!(
            r.on_receive(&bcast(OWN, 1, 3), b"p", 0.0, 0),
            RelayDecision::Suppressed(SuppressReason::OwnOrigin)
        );
        // Hop budget exhausted.
        assert_eq!(
            r.on_receive(&bcast(SRC, 2, 0), b"p", 0.0, 0),
            RelayDecision::Suppressed(SuppressReason::HopExhausted)
        );
        // A DM addressed to us: process locally, don't relay.
        let mut dm = bcast(SRC, 3, 3);
        dm.to = OWN;
        assert_eq!(
            r.on_receive(&dm, b"p", 0.0, 0),
            RelayDecision::Suppressed(SuppressReason::AddressedToUs)
        );
        // A DM to a THIRD party is relayed (help it along the flood).
        let mut dm_other = bcast(SRC, 4, 3);
        dm_other.to = 0x0000_00cc;
        assert!(matches!(
            r.on_receive(&dm_other, b"p", 0.0, 0),
            RelayDecision::Scheduled { .. }
        ));
        assert_eq!(r.pending_len(), 1);
    }

    #[test]
    fn client_role_never_relays_but_dedups() {
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Client);
        assert_eq!(
            r.on_receive(&bcast(SRC, 1, 3), b"p", 0.0, 0),
            RelayDecision::Suppressed(SuppressReason::RoleClient)
        );
        assert_eq!(r.pending_len(), 0);
        // Seen was recorded → a repeat is a plain duplicate.
        assert_eq!(
            r.on_receive(&bcast(SRC, 1, 3), b"p", 0.0, 1),
            RelayDecision::Suppressed(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn broadcast_is_never_treated_as_addressed_to_us() {
        // Even if our node id somehow collided with a low value, the broadcast
        // sentinel (0xFFFFFFFF) can never equal a real node id → always relayed.
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        let h = bcast(SRC, 7, 3);
        assert_eq!(h.to, BROADCAST_ADDR);
        assert!(matches!(
            r.on_receive(&h, b"p", 0.0, 0),
            RelayDecision::Scheduled { .. }
        ));
    }

    #[test]
    fn jitter_is_bounded_and_decorrelates_nodes() {
        for own in 0u32..200 {
            assert!(jitter_ms(SRC, 7, own) <= JITTER_MS);
        }
        let vals: std::collections::HashSet<u64> =
            (0u32..200).map(|o| jitter_ms(SRC, 7, o)).collect();
        assert!(vals.len() > 5, "jitter must vary across nodes");
    }

    #[test]
    fn pending_and_seen_are_bounded() {
        let mut r = MeshtasticRouter::with_caps(OWN, RelayRole::Router, 2, 2);
        for id in 0u32..5 {
            r.on_receive(&bcast(SRC, id, 3), b"p", 0.0, 1_000);
        }
        assert!(r.pending_len() <= 2, "pending queue is bounded");
        r.due(1_000_000);
        // seen cap 2 → the newest (id 4) is still remembered.
        assert_eq!(
            r.on_receive(&bcast(SRC, 4, 3), b"p", 0.0, 2_000),
            RelayDecision::Suppressed(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn max_hop_window_bounds_the_delay() {
        // A strongest-SNR relay at the widest window still fires within one full
        // window + jitter — the flood can never schedule unboundedly far out.
        let mut r = MeshtasticRouter::new(OWN, RelayRole::Router);
        if let RelayDecision::Scheduled { due_at_ms } =
            r.on_receive(&bcast(SRC, 1, 3), b"p", SNR_CEIL_DB, 0)
        {
            let max = (WINDOW_SLOTS as u64 - 1) * SLOT_MS + JITTER_MS;
            assert!(due_at_ms <= max, "due {due_at_ms} within max window {max}");
        } else {
            panic!("expected Scheduled");
        }
    }
}
