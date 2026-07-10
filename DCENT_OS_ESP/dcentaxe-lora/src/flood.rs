// SPDX-License-Identifier: GPL-3.0-or-later
//! Managed-flood rebroadcast planning for the `$DCM` mesh — the resilience core
//! that turns the naive dedup-flood ([`RelayCache`](crate::relay::RelayCache))
//! into a Meshtastic-style, link-aware flood that scales to a dense, always-on
//! DCENT_axe relay swarm without broadcast storms.
//!
//! Three defects this closes (per the 2026-07 mesh maturity audit):
//!   1. **Correlated collisions.** The task layer previously relayed every fresh
//!      frame after a fixed ~200 ms poll, so two axes that heard the same frame
//!      re-transmitted at nearly the same instant and stomped each other. Here a
//!      relay is delayed by an **SNR-weighted contention window** plus a
//!      **per-node deterministic jitter**, so co-located nodes decorrelate.
//!   2. **Redundant floods.** A node **cancels its own pending rebroadcast** the
//!      moment it hears a neighbour relay the same `(src, seq)` — a duplicate
//!      arriving before our scheduled TX means the frame is already covered, so
//!      staying quiet is the whole point of a managed flood.
//!   3. **Every node shouts.** The relay decision is gated by the node's
//!      [`RelayRole`]: a leaf [`Client`](RelayRole::Client) never floods while a
//!      mains-powered [`Router`](RelayRole::Router)/[`Repeater`](RelayRole::Repeater)
//!      does the backbone work — the "army of relays" model.
//!
//! **Clock-free by construction.** The planner never reads a clock. The caller
//! (the esp-idf `lora_task`) passes a monotonic `now_ms` into every method and
//! owns the actual timer; the planner only decides *whether* and computes *when*
//! (a delay) to rebroadcast, so every decision is deterministic and host-unit-
//! testable — the same split the rest of this crate uses (see `relay.rs`).
//!
//! The SNR→delay mapping follows Meshtastic's insight: the node that heard a
//! frame **weakest** (lowest SNR, i.e. farthest away and most likely to extend
//! coverage) rebroadcasts **first**; nearer nodes wait longer and are the ones
//! whose pending rebroadcast gets cancelled when the far node's relay arrives.

use crate::mesh::{MeshFrame, NodeId};
use std::collections::VecDeque;

/// One contention slot, in milliseconds. The SNR window is `WINDOW_SLOTS` of
/// these; a full window (`WINDOW_SLOTS * SLOT_MS`) is the max SNR-driven delay.
pub const SLOT_MS: u64 = 60;

/// Number of SNR-mapped contention slots (0 = weakest/first, `WINDOW_SLOTS-1` =
/// strongest/last).
pub const WINDOW_SLOTS: u32 = 8;

/// SNR (dB) that maps to the earliest slot — the weakest link we still relay for
/// (farthest node, rebroadcasts first to extend range).
pub const SNR_FLOOR_DB: f32 = -20.0;

/// SNR (dB) that maps to the latest slot — a very strong (close) link, most
/// likely to be covered by a farther node's earlier rebroadcast.
pub const SNR_CEIL_DB: f32 = 10.0;

/// Maximum per-node deterministic jitter (ms) added on top of the SNR window so
/// two nodes that computed the *same* SNR slot still don't fire simultaneously.
pub const JITTER_MS: u64 = 40;

/// Extra fixed delay (ms) a [`RelayRole::RouterLate`] adds so it only fills gaps
/// the primary routers leave — one full contention window later.
pub const ROUTER_LATE_BIAS_MS: u64 = WINDOW_SLOTS as u64 * SLOT_MS;

/// Default cap on concurrently-pending rebroadcasts (bounded so a relay storm
/// can never grow the queue without limit; the radio drains it far faster).
pub const DEFAULT_PENDING_CAP: usize = 32;

/// Default cap on the `(src, seq)` seen-set used for dedup (FIFO ring).
pub const DEFAULT_SEEN_CAP: usize = 64;

/// A node's participation role in the flood — Meshtastic-style. An always-on,
/// mains-powered DCENT_axe defaults to [`Router`](Self::Router) (the backbone
/// "army of relays"); a battery/leaf node is a [`Client`](Self::Client) and
/// never rebroadcasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelayRole {
    /// Full backbone relay (default for a mains-powered axe).
    #[default]
    Router,
    /// Infrastructure relay — same rebroadcast behaviour as [`Router`](Self::Router)
    /// (kept distinct so the role can carry different app-traffic policy later).
    Repeater,
    /// Relays, but defers (a full window later) to dedicated routers so it only
    /// fills coverage gaps.
    RouterLate,
    /// Leaf node — never rebroadcasts others' traffic.
    Client,
}

impl RelayRole {
    /// Whether this role rebroadcasts other nodes' frames at all.
    pub fn rebroadcasts(self) -> bool {
        !matches!(self, RelayRole::Client)
    }

    /// Fixed delay bias (ms) added to the contention window for this role.
    fn delay_bias_ms(self) -> u64 {
        match self {
            RelayRole::RouterLate => ROUTER_LATE_BIAS_MS,
            _ => 0,
        }
    }

    /// Lowercase wire/config token.
    pub fn as_str(self) -> &'static str {
        match self {
            RelayRole::Router => "router",
            RelayRole::Repeater => "repeater",
            RelayRole::RouterLate => "router_late",
            RelayRole::Client => "client",
        }
    }

    /// Parse a config token (case-insensitive); `None` if unrecognized.
    pub fn from_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "router" => Some(RelayRole::Router),
            "repeater" => Some(RelayRole::Repeater),
            "router_late" | "router-late" | "routerlate" => Some(RelayRole::RouterLate),
            "client" => Some(RelayRole::Client),
            _ => None,
        }
    }
}

/// Why a received frame was not turned into a pending rebroadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    /// Already seen this `(src, seq)` and nothing was pending for it.
    Duplicate,
    /// The frame originated here — never relay our own traffic.
    OwnOrigin,
    /// Hop budget exhausted (`ttl <= 1`); this was the last hop.
    HopExhausted,
    /// This node's [`RelayRole`] does not rebroadcast (a [`Client`](RelayRole::Client)).
    RoleClient,
}

/// The outcome of feeding a received frame to the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxAction {
    /// A rebroadcast was scheduled; the caller should emit the frame returned by
    /// [`RebroadcastPlanner::due`] at or after `due_at_ms`.
    Scheduled { due_at_ms: u64 },
    /// A duplicate arrived that cancelled a still-pending rebroadcast — a
    /// neighbour already covered this frame, so this node stays quiet.
    Canceled,
    /// The frame was not scheduled; see [`SuppressReason`].
    Suppressed(SuppressReason),
}

/// A rebroadcast waiting for its contention slot. `frame` is already
/// hop-decremented and ready to transmit verbatim.
#[derive(Debug, Clone)]
struct Pending {
    src: NodeId,
    seq: u8,
    due_at_ms: u64,
    frame: MeshFrame,
}

/// Map an SNR reading (dB) + role to a contention delay (ms): weaker signal →
/// earlier slot (rebroadcast sooner to extend range), stronger → later slot.
pub fn contention_delay_ms(snr_db: f32, role: RelayRole) -> u64 {
    let span = SNR_CEIL_DB - SNR_FLOOR_DB;
    let norm = if span > 0.0 {
        ((snr_db - SNR_FLOOR_DB) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let slot = (norm * (WINDOW_SLOTS - 1) as f32).round() as u64;
    slot * SLOT_MS + role.delay_bias_ms()
}

/// Deterministic per-node jitter (0..=`JITTER_MS`) from `(src, seq, own)`. FNV-1a
/// over the three inputs — no RNG, no clock — so it is reproducible in tests yet
/// **decorrelated between nodes**: `own` is folded in, so two different receivers
/// compute different jitter for the same `(src, seq)` and never fire in lockstep.
pub fn jitter_ms(src: NodeId, seq: u8, own: NodeId) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let feed = |h: &mut u64, b: u8| {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for b in src.0.to_le_bytes() {
        feed(&mut h, b);
    }
    feed(&mut h, seq);
    for b in own.0.to_le_bytes() {
        feed(&mut h, b);
    }
    h % (JITTER_MS + 1)
}

/// Plans rebroadcasts for the managed flood: dedups, gates by role/hop/origin,
/// schedules a fresh frame after an SNR-weighted + jittered delay, and cancels a
/// pending rebroadcast when a neighbour beats us to it. Bounded and clock-free.
#[derive(Debug, Clone)]
pub struct RebroadcastPlanner {
    own: NodeId,
    role: RelayRole,
    seen: VecDeque<(NodeId, u8)>,
    pending: VecDeque<Pending>,
    seen_cap: usize,
    pending_cap: usize,
}

impl RebroadcastPlanner {
    /// A planner for node `own` in `role` with default caps.
    pub fn new(own: NodeId, role: RelayRole) -> Self {
        Self::with_caps(own, role, DEFAULT_SEEN_CAP, DEFAULT_PENDING_CAP)
    }

    /// A planner with explicit dedup / pending caps (each clamped to ≥ 1).
    pub fn with_caps(own: NodeId, role: RelayRole, seen_cap: usize, pending_cap: usize) -> Self {
        Self {
            own,
            role,
            seen: VecDeque::new(),
            pending: VecDeque::new(),
            seen_cap: seen_cap.max(1),
            pending_cap: pending_cap.max(1),
        }
    }

    /// This node's current relay role.
    pub fn role(&self) -> RelayRole {
        self.role
    }

    /// Change the relay role (e.g. from a runtime config update).
    pub fn set_role(&mut self, role: RelayRole) {
        self.role = role;
    }

    /// Number of rebroadcasts currently waiting for their slot.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn seen_contains(&self, src: NodeId, seq: u8) -> bool {
        self.seen.iter().any(|&(s, q)| s == src && q == seq)
    }

    fn pending_pos(&self, src: NodeId, seq: u8) -> Option<usize> {
        self.pending
            .iter()
            .position(|p| p.src == src && p.seq == seq)
    }

    fn record_seen(&mut self, src: NodeId, seq: u8) {
        if self.seen.len() >= self.seen_cap {
            self.seen.pop_front();
        }
        self.seen.push_back((src, seq));
    }

    fn push_pending(&mut self, entry: Pending) {
        if self.pending.len() >= self.pending_cap {
            self.pending.pop_front();
        }
        self.pending.push_back(entry);
    }

    /// Feed a received frame (with its measured `snr_db`) to the planner at
    /// monotonic time `now_ms`. See [`RxAction`] for the outcomes.
    pub fn on_receive(&mut self, frame: &MeshFrame, snr_db: f32, now_ms: u64) -> RxAction {
        let (src, seq) = (frame.src, frame.seq);

        // 1. A duplicate of something we were about to relay ⇒ a neighbour
        //    already covered it; cancel our pending rebroadcast and stay quiet.
        if let Some(pos) = self.pending_pos(src, seq) {
            self.pending.remove(pos);
            return RxAction::Canceled;
        }

        // 2. Already seen, nothing pending ⇒ plain duplicate.
        if self.seen_contains(src, seq) {
            return RxAction::Suppressed(SuppressReason::Duplicate);
        }

        // 3. First sighting — always record for dedup, whatever we decide next.
        self.record_seen(src, seq);

        // 4. Never relay our own origination.
        if src == self.own {
            return RxAction::Suppressed(SuppressReason::OwnOrigin);
        }

        // 5. Role gate — a Client observes but never floods.
        if !self.role.rebroadcasts() {
            return RxAction::Suppressed(SuppressReason::RoleClient);
        }

        // 6. Hop budget — the relayed copy is hop-decremented, or None on the
        //    last hop.
        let relayed = match frame.next_hop() {
            Some(f) => f,
            None => return RxAction::Suppressed(SuppressReason::HopExhausted),
        };

        // 7. Schedule after the SNR-weighted window + per-node jitter.
        let delay = contention_delay_ms(snr_db, self.role) + jitter_ms(src, seq, self.own);
        let due_at_ms = now_ms.saturating_add(delay);
        self.push_pending(Pending {
            src,
            seq,
            due_at_ms,
            frame: relayed,
        });
        RxAction::Scheduled { due_at_ms }
    }

    /// Remove and return every pending rebroadcast whose slot has arrived
    /// (`due_at_ms <= now_ms`), in ascending due order. Each returned frame is
    /// already hop-decremented and ready to hand to the radio TX queue.
    pub fn due(&mut self, now_ms: u64) -> Vec<MeshFrame> {
        // Stable partition: collect due indices, emit in due-time order.
        let mut ready: Vec<(u64, usize)> = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, p)| p.due_at_ms <= now_ms)
            .map(|(i, p)| (p.due_at_ms, i))
            .collect();
        ready.sort_by_key(|&(due, i)| (due, i));
        let out: Vec<MeshFrame> = ready
            .iter()
            .map(|&(_, i)| self.pending[i].frame.clone())
            .collect();
        // Remove the emitted entries (highest index first so positions stay valid).
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
    use crate::mesh::{MeshFrame, MeshKind, Telemetry};

    const OWN: NodeId = NodeId(0x0000_00a1);
    const SRC: NodeId = NodeId(0x0000_00b2);

    fn frame(src: NodeId, seq: u8, ttl: u8) -> MeshFrame {
        MeshFrame {
            src,
            seq,
            ttl,
            kind: MeshKind::Telemetry(Telemetry {
                hashrate_ghs: 1.0,
                chip_temp_c: 50.0,
                power_w: 15.0,
                shares_accepted: 1,
                shares_rejected: 0,
                best_diff: "1k".into(),
                block_height: 1,
            }),
        }
    }

    #[test]
    fn router_schedules_fresh_frame_hop_decremented() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        match p.on_receive(&frame(SRC, 5, 3), 0.0, 1_000) {
            RxAction::Scheduled { due_at_ms } => assert!(due_at_ms >= 1_000),
            other => panic!("expected Scheduled, got {other:?}"),
        }
        assert_eq!(p.pending_len(), 1);
        // The emitted frame is hop-decremented (ttl 3 -> 2).
        let out = p.due(1_000_000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ttl, 2);
        assert_eq!(out[0].src, SRC);
        assert_eq!(out[0].seq, 5);
    }

    #[test]
    fn weaker_snr_rebroadcasts_earlier_than_stronger() {
        let far = contention_delay_ms(SNR_FLOOR_DB, RelayRole::Router); // weakest
        let near = contention_delay_ms(SNR_CEIL_DB, RelayRole::Router); // strongest
        assert_eq!(far, 0, "weakest link takes slot 0 (relays first)");
        assert!(near > far, "stronger link waits longer ({near} > {far})");
        assert_eq!(near, (WINDOW_SLOTS as u64 - 1) * SLOT_MS);
        // Out-of-range SNRs clamp, never panic.
        assert_eq!(contention_delay_ms(-100.0, RelayRole::Router), 0);
        assert_eq!(
            contention_delay_ms(100.0, RelayRole::Router),
            (WINDOW_SLOTS as u64 - 1) * SLOT_MS
        );
    }

    #[test]
    fn router_late_defers_a_full_window() {
        let router = contention_delay_ms(0.0, RelayRole::Router);
        let late = contention_delay_ms(0.0, RelayRole::RouterLate);
        assert_eq!(late, router + ROUTER_LATE_BIAS_MS);
    }

    #[test]
    fn jitter_is_bounded_and_decorrelates_nodes() {
        // Bounded 0..=JITTER_MS for any input.
        for own in 0u32..200 {
            let j = jitter_ms(SRC, 7, NodeId(own));
            assert!(j <= JITTER_MS);
        }
        // Not constant across nodes — different `own` yields a spread of values,
        // so co-located nodes do not fire in lockstep.
        let vals: std::collections::HashSet<u64> =
            (0u32..200).map(|o| jitter_ms(SRC, 7, NodeId(o))).collect();
        assert!(
            vals.len() > 5,
            "jitter must vary across nodes, got {vals:?}"
        );
    }

    #[test]
    fn duplicate_before_due_cancels_pending() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        assert!(matches!(
            p.on_receive(&frame(SRC, 5, 3), 0.0, 1_000),
            RxAction::Scheduled { .. }
        ));
        assert_eq!(p.pending_len(), 1);
        // A neighbour's relay of the same (src,seq) arrives before our slot.
        assert_eq!(
            p.on_receive(&frame(SRC, 5, 2), -5.0, 1_010),
            RxAction::Canceled
        );
        assert_eq!(p.pending_len(), 0, "our pending rebroadcast is cancelled");
        // And nothing is emitted later.
        assert!(p.due(1_000_000).is_empty());
    }

    #[test]
    fn duplicate_after_seen_without_pending_is_suppressed() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        p.on_receive(&frame(SRC, 5, 3), 0.0, 1_000);
        // Flush the pending so only the seen-record remains.
        p.due(1_000_000);
        assert_eq!(
            p.on_receive(&frame(SRC, 5, 3), 0.0, 2_000),
            RxAction::Suppressed(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn client_never_rebroadcasts_but_records_seen() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Client);
        assert_eq!(
            p.on_receive(&frame(SRC, 1, 3), 0.0, 1_000),
            RxAction::Suppressed(SuppressReason::RoleClient)
        );
        assert_eq!(p.pending_len(), 0);
        assert!(p.due(1_000_000).is_empty());
        // Seen was recorded (dedup still works for a client's own observability).
        assert_eq!(
            p.on_receive(&frame(SRC, 1, 3), 0.0, 1_001),
            RxAction::Suppressed(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn own_origin_suppressed() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        assert_eq!(
            p.on_receive(&frame(OWN, 9, 3), 0.0, 1_000),
            RxAction::Suppressed(SuppressReason::OwnOrigin)
        );
        assert_eq!(p.pending_len(), 0);
    }

    #[test]
    fn hop_exhausted_suppressed() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        assert_eq!(
            p.on_receive(&frame(SRC, 1, 1), 0.0, 1_000),
            RxAction::Suppressed(SuppressReason::HopExhausted)
        );
        assert_eq!(p.pending_len(), 0);
    }

    #[test]
    fn due_returns_only_elapsed_in_order() {
        let mut p = RebroadcastPlanner::new(OWN, RelayRole::Router);
        // Two frames with different SNR → different due times.
        p.on_receive(&frame(SRC, 1, 3), SNR_CEIL_DB, 0); // later slot
        p.on_receive(&frame(NodeId(0xC3), 2, 3), SNR_FLOOR_DB, 0); // slot 0, earliest
        assert_eq!(p.pending_len(), 2);
        // At t=0 nothing is due yet if the earliest has any jitter; flush far future.
        let early = p.due(SLOT_MS); // only the slot-0 (+jitter ≤ 40) frame
        assert_eq!(early.len(), 1);
        assert_eq!(early[0].seq, 2, "earliest (weakest SNR) emitted first");
        let rest = p.due(1_000_000);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].seq, 1);
        assert_eq!(p.pending_len(), 0);
    }

    #[test]
    fn pending_and_seen_are_bounded() {
        let mut p = RebroadcastPlanner::with_caps(OWN, RelayRole::Router, 2, 2);
        for seq in 0u8..5 {
            p.on_receive(&frame(SRC, seq, 3), 0.0, 1_000);
        }
        assert!(p.pending_len() <= 2, "pending queue is bounded");
        // Seen cap is 2, so the oldest (src,seq) is evictable and re-relays fresh.
        p.due(1_000_000);
        // seq 0/1 evicted from the 2-entry seen ring; seq 4 still remembered.
        assert!(matches!(
            p.on_receive(&frame(SRC, 4, 3), 0.0, 2_000),
            RxAction::Suppressed(SuppressReason::Duplicate)
        ));
    }
}
