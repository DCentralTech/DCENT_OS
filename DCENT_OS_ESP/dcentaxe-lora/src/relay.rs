// SPDX-License-Identifier: GPL-3.0-or-later
//! Store-and-forward relay primitives for the `$DCM` mesh — shared by the
//! DCENT_Raven module and the DCENT_axe on-board radio so both flood the mesh
//! identically.
//!
//! Two host-pure pieces:
//!   * [`RelayCache`] — a bounded seen-set keyed by `(src, seq)` that decides
//!     whether an over-the-air frame should be re-transmitted (new, not ours,
//!     hop budget remaining) and hands back the hop-decremented copy to send.
//!   * [`TxQueue`] — a bounded, priority-aware transmit queue so a block-found
//!     (`BLK`) beacon is never dropped to make room for routine telemetry.
//!
//! Both are clock-free and `no_std`-friendly in spirit (they use `Vec`/`VecDeque`
//! from `std`, matching the rest of the crate) so every decision is unit-testable
//! on the host with no radio.

use crate::mesh::{MeshFrame, MeshKind};
use crate::NodeId;
use std::collections::VecDeque;

/// Default number of `(src, seq)` observations retained for dedup. Bounded so a
/// busy mesh cannot grow the cache without limit; large enough that a frame is
/// very unlikely to be re-flooded within its lifetime on a home/site swarm.
pub const DEFAULT_RELAY_CACHE: usize = 64;

/// Default depth of the transmit queue. Small — the radio drains it far faster
/// than beacons are produced; the bound only matters under a relay storm.
pub const DEFAULT_TX_QUEUE: usize = 8;

/// A bounded, clock-free seen-set for store-and-forward dedup, keyed by
/// `(src, seq)`. When full, the oldest observation is evicted (FIFO ring).
#[derive(Debug, Clone)]
pub struct RelayCache {
    seen: VecDeque<(NodeId, u8)>,
    capacity: usize,
}

impl Default for RelayCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_RELAY_CACHE)
    }
}

impl RelayCache {
    /// A cache retaining [`DEFAULT_RELAY_CACHE`] observations.
    pub fn new() -> Self {
        Self::default()
    }

    /// A cache with an explicit capacity (clamped to ≥ 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            seen: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// `true` if `(src, seq)` has already been observed.
    pub fn contains(&self, src: NodeId, seq: u8) -> bool {
        self.seen.iter().any(|&(s, q)| s == src && q == seq)
    }

    /// Number of observations currently retained.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` when nothing has been observed yet.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    fn record(&mut self, src: NodeId, seq: u8) {
        if self.seen.len() >= self.capacity {
            self.seen.pop_front();
        }
        self.seen.push_back((src, seq));
    }

    /// Decide whether to relay `frame`, given this node's own id `own`.
    ///
    /// Returns the hop-decremented copy to re-transmit, or `None` when the frame:
    /// was already seen (dedup), originated here (`frame.src == own`, never relay
    /// our own traffic), or has no hop budget left (`ttl <= 1`). A first-seen
    /// frame is always recorded — including our own and hop-exhausted ones — so a
    /// later echo of the same `(src, seq)` is suppressed regardless of outcome.
    pub fn consider(&mut self, frame: &MeshFrame, own: NodeId) -> Option<MeshFrame> {
        if self.contains(frame.src, frame.seq) {
            return None;
        }
        self.record(frame.src, frame.seq);
        if frame.src == own {
            return None;
        }
        frame.next_hop()
    }
}

/// Transmit priority. A block-found beacon (`High`) must reach the mesh even
/// under congestion; routine telemetry/identify (`Low`) yields to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TxPriority {
    Low,
    Normal,
    High,
}

/// The transmit priority of a frame kind: `BlockFound` is `High`, control/ack is
/// `Normal`, telemetry/identify is `Low`.
pub fn tx_priority(kind: &MeshKind) -> TxPriority {
    match kind {
        MeshKind::BlockFound(_) => TxPriority::High,
        MeshKind::Command(_) | MeshKind::Ack(_) => TxPriority::Normal,
        MeshKind::Telemetry(_) | MeshKind::Identify(_) => TxPriority::Low,
    }
}

/// Outcome of enqueuing onto a full [`TxQueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    /// Accepted with room to spare.
    Ok,
    /// Accepted by evicting a lower-priority queued frame.
    Displaced,
    /// Rejected — the queue was full of equal-or-higher-priority frames.
    Dropped,
}

/// A bounded, priority-aware transmit queue. `pop` yields the
/// highest-priority frame, FIFO within a priority. When full, `push` evicts the
/// oldest strictly-lower-priority frame to admit a higher-priority one, and
/// refuses a frame that is not strictly higher-priority than everything queued —
/// so a `BLK` never loses its slot to a `TLM`, and a `TLM` never displaces a
/// `BLK`.
#[derive(Debug, Clone)]
pub struct TxQueue {
    items: VecDeque<(TxPriority, MeshFrame)>,
    capacity: usize,
}

impl Default for TxQueue {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_TX_QUEUE)
    }
}

impl TxQueue {
    /// A queue holding [`DEFAULT_TX_QUEUE`] frames.
    pub fn new() -> Self {
        Self::default()
    }

    /// A queue with an explicit capacity (clamped to ≥ 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Number of queued frames.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Enqueue `frame` at its kind's priority. See [`TxQueue`] for the full drop
    /// policy.
    pub fn push(&mut self, frame: MeshFrame) -> Enqueued {
        let prio = tx_priority(&frame.kind);
        if self.items.len() < self.capacity {
            self.items.push_back((prio, frame));
            return Enqueued::Ok;
        }
        // Full: find the oldest strictly-lower-priority frame to evict.
        let victim = self.items.iter().position(|(p, _)| *p < prio);
        match victim {
            Some(idx) => {
                self.items.remove(idx);
                self.items.push_back((prio, frame));
                Enqueued::Displaced
            }
            None => Enqueued::Dropped,
        }
    }

    /// Remove and return the highest-priority frame (FIFO within a priority).
    pub fn pop(&mut self) -> Option<MeshFrame> {
        let idx = self
            .items
            .iter()
            .enumerate()
            .max_by_key(|(i, (p, _))| (*p, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)?;
        self.items.remove(idx).map(|(_, f)| f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{BlockFound, Telemetry};

    const A: NodeId = NodeId(0x0000_000a);
    const B: NodeId = NodeId(0x0000_000b);

    fn tlm(src: NodeId, seq: u8, ttl: u8) -> MeshFrame {
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

    fn blk(src: NodeId, seq: u8, ttl: u8) -> MeshFrame {
        MeshFrame {
            src,
            seq,
            ttl,
            kind: MeshKind::BlockFound(BlockFound {
                block_height: 900_000,
                best_diff: "21M".into(),
            }),
        }
    }

    // ---- RelayCache ----

    #[test]
    fn relays_fresh_frame_with_decremented_ttl() {
        let mut c = RelayCache::new();
        let out = c.consider(&tlm(B, 5, 3), A).expect("fresh frame relays");
        assert_eq!((out.src, out.seq, out.ttl), (B, 5, 2));
    }

    #[test]
    fn suppresses_duplicate() {
        let mut c = RelayCache::new();
        assert!(c.consider(&tlm(B, 5, 3), A).is_some());
        assert!(
            c.consider(&tlm(B, 5, 3), A).is_none(),
            "second copy suppressed"
        );
        // A different seq from the same src still relays.
        assert!(c.consider(&tlm(B, 6, 3), A).is_some());
    }

    #[test]
    fn never_relays_own_origination_and_dedups_the_echo() {
        let mut c = RelayCache::new();
        assert!(
            c.consider(&tlm(A, 9, 3), A).is_none(),
            "own frame not relayed"
        );
        // Recorded, so an echo of our own frame is also suppressed (not relayed).
        assert!(c.consider(&tlm(A, 9, 3), A).is_none());
    }

    #[test]
    fn does_not_relay_hop_exhausted_frame_but_records_it() {
        let mut c = RelayCache::new();
        assert!(c.consider(&tlm(B, 1, 1), A).is_none(), "ttl 1 = last hop");
        assert!(c.contains(B, 1), "still recorded for dedup");
    }

    #[test]
    fn cache_is_bounded_and_evicts_oldest() {
        let mut c = RelayCache::with_capacity(2);
        c.consider(&tlm(B, 1, 3), A);
        c.consider(&tlm(B, 2, 3), A);
        c.consider(&tlm(B, 3, 3), A); // evicts (B,1)
        assert!(!c.contains(B, 1), "oldest evicted");
        assert!(c.contains(B, 2) && c.contains(B, 3));
        // (B,1) is now treated as fresh again.
        assert!(c.consider(&tlm(B, 1, 3), A).is_some());
    }

    // ---- TxQueue ----

    #[test]
    fn pops_highest_priority_first_fifo_within() {
        let mut q = TxQueue::new();
        q.push(tlm(A, 1, 3));
        q.push(blk(A, 2, 3));
        q.push(tlm(A, 3, 3));
        // BLK first, then the two TLMs in FIFO order.
        assert!(matches!(q.pop().unwrap().kind, MeshKind::BlockFound(_)));
        assert_eq!(q.pop().unwrap().seq, 1);
        assert_eq!(q.pop().unwrap().seq, 3);
        assert!(q.pop().is_none());
    }

    #[test]
    fn full_queue_evicts_low_priority_for_high() {
        let mut q = TxQueue::with_capacity(2);
        assert_eq!(q.push(tlm(A, 1, 3)), Enqueued::Ok);
        assert_eq!(q.push(tlm(A, 2, 3)), Enqueued::Ok);
        // Full of TLMs; a BLK displaces the oldest TLM.
        assert_eq!(q.push(blk(A, 3, 3)), Enqueued::Displaced);
        assert_eq!(q.len(), 2);
        assert!(matches!(q.pop().unwrap().kind, MeshKind::BlockFound(_)));
        // The surviving TLM is the newer one (seq 2); seq 1 was evicted.
        assert_eq!(q.pop().unwrap().seq, 2);
    }

    #[test]
    fn full_queue_never_drops_high_for_low() {
        let mut q = TxQueue::with_capacity(2);
        q.push(blk(A, 1, 3));
        q.push(blk(A, 2, 3));
        // Full of BLKs; a TLM cannot displace either → dropped.
        assert_eq!(q.push(tlm(A, 3, 3)), Enqueued::Dropped);
        assert_eq!(q.len(), 2);
        assert!(matches!(q.pop().unwrap().kind, MeshKind::BlockFound(_)));
        assert!(matches!(q.pop().unwrap().kind, MeshKind::BlockFound(_)));
    }

    #[test]
    fn priority_ordering_is_total() {
        assert!(TxPriority::High > TxPriority::Normal);
        assert!(TxPriority::Normal > TxPriority::Low);
        assert_eq!(tx_priority(&blk(A, 0, 1).kind), TxPriority::High);
        assert_eq!(tx_priority(&tlm(A, 0, 1).kind), TxPriority::Low);
    }
}
