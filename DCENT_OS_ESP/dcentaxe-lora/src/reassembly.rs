// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, clock-free reassembly of [`BlockFragment`](crate::mesh::BlockFragment)
//! frames into a complete found-block payload (80-byte header + coinbase tx).
//!
//! Pure / host-testable: the caller passes `now_ms` for eviction; no wall clock
//! or RNG is read here. Capacity is hard-capped so hostile mesh traffic cannot
//! grow RAM without bound.

use crate::mesh::BlockFragment;

/// Maximum fragments per block (a solo coinbase block is ~200–300 B → a few
/// LoRa payloads). Hostile `total` above this is refused.
pub const MAX_BLOCK_FRAGMENTS: u8 = 16;

/// Maximum raw bytes in a single fragment (wire hex already bounded by
/// [`crate::mesh::MAX_MESH_PAYLOAD`]; this is the decoded-byte ceiling).
pub const MAX_FRAGMENT_BYTES: usize = 96;

/// Maximum total reassembled block size (header 80 + generous coinbase).
pub const MAX_BLOCK_BYTES: usize = 1024;

/// Maximum concurrent in-flight reassemblies (one per `id`).
pub const MAX_IN_FLIGHT: usize = 4;

/// Default age (caller ticks) after which an incomplete reassembly is dropped.
pub const DEFAULT_STALE_MS: u64 = 60_000;

/// Outcome of feeding one fragment into a [`BlockReassembler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassemblyOutcome {
    /// Fragment stored; still waiting for more.
    Pending { have: u8, total: u8 },
    /// All fragments present — payload is complete.
    Complete(Vec<u8>),
    /// Duplicate of an already-held fragment (no state change).
    Duplicate,
    /// Rejected (bounds / inconsistency / capacity).
    Rejected(RejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// `total == 0` or `total > MAX_BLOCK_FRAGMENTS`.
    BadTotal,
    /// `seq >= total`.
    BadSeq,
    /// Fragment payload empty or over [`MAX_FRAGMENT_BYTES`].
    BadPayload,
    /// Same `id` already has a different `total`.
    TotalMismatch,
    /// Accepting this fragment would exceed [`MAX_BLOCK_BYTES`].
    Oversize,
    /// No room for a new `id` and nothing stale to evict.
    Capacity,
}

#[derive(Debug, Clone)]
struct Slot {
    id: u32,
    total: u8,
    /// `parts[i] = Some(bytes)` when fragment `i` has arrived.
    parts: Vec<Option<Vec<u8>>>,
    last_ms: u64,
    bytes_so_far: usize,
}

impl Slot {
    fn new(id: u32, total: u8, now_ms: u64) -> Self {
        Self {
            id,
            total,
            parts: (0..total).map(|_| None).collect(),
            last_ms: now_ms,
            bytes_so_far: 0,
        }
    }

    fn have_count(&self) -> u8 {
        self.parts.iter().filter(|p| p.is_some()).count() as u8
    }

    fn try_complete(&self) -> Option<Vec<u8>> {
        if self.have_count() != self.total {
            return None;
        }
        let mut out = Vec::with_capacity(self.bytes_so_far);
        for p in &self.parts {
            out.extend_from_slice(p.as_ref()?);
        }
        Some(out)
    }
}

/// Bounded multi-block fragment reassembler.
///
/// Clock-free: pass `now_ms` into [`ingest`](Self::ingest) /
/// [`expire`](Self::expire). When full, the least-recently-updated incomplete
/// slot is evicted to admit a new `id`.
#[derive(Debug, Clone)]
pub struct BlockReassembler {
    slots: Vec<Slot>,
    capacity: usize,
    stale_ms: u64,
}

impl Default for BlockReassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockReassembler {
    pub fn new() -> Self {
        Self::with_capacity(MAX_IN_FLIGHT, DEFAULT_STALE_MS)
    }

    pub fn with_capacity(capacity: usize, stale_ms: u64) -> Self {
        Self {
            slots: Vec::new(),
            capacity: capacity.max(1),
            stale_ms,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.slots.len()
    }

    /// Drop incomplete reassemblies older than `stale_ms` (by `last_ms`).
    /// Returns how many were removed.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let before = self.slots.len();
        let stale = self.stale_ms;
        self.slots
            .retain(|s| now_ms.saturating_sub(s.last_ms) <= stale);
        before - self.slots.len()
    }

    /// Ingest one fragment. On `Complete`, the slot is removed and the full
    /// payload is returned. Never panics on hostile input.
    pub fn ingest(&mut self, frag: &BlockFragment, now_ms: u64) -> ReassemblyOutcome {
        if frag.total == 0 || frag.total > MAX_BLOCK_FRAGMENTS {
            return ReassemblyOutcome::Rejected(RejectReason::BadTotal);
        }
        if frag.seq >= frag.total {
            return ReassemblyOutcome::Rejected(RejectReason::BadSeq);
        }
        if frag.bytes.is_empty() || frag.bytes.len() > MAX_FRAGMENT_BYTES {
            return ReassemblyOutcome::Rejected(RejectReason::BadPayload);
        }

        // Opportunistic expire before capacity decisions.
        let _ = self.expire(now_ms);

        if let Some(idx) = self.slots.iter().position(|s| s.id == frag.id) {
            return self.ingest_existing(idx, frag, now_ms);
        }

        // New id — build the slot and check for IMMEDIATE completion before any
        // capacity eviction. A single-fragment block (total == 1) completes here
        // and is never stored, so it must not evict a legitimate in-flight
        // reassembly to make room it never uses — otherwise a near-zero-cost
        // stream of valid single-fragment frames flushes every in-progress
        // multi-fragment found-block reassembly (a cheap DoS on the gateway
        // solo-block submitblock path).
        let mut slot = Slot::new(frag.id, frag.total, now_ms);
        if frag.bytes.len() > MAX_BLOCK_BYTES {
            return ReassemblyOutcome::Rejected(RejectReason::Oversize);
        }
        slot.bytes_so_far = frag.bytes.len();
        slot.parts[frag.seq as usize] = Some(frag.bytes.clone());
        if let Some(done) = slot.try_complete() {
            // Completes without occupying a slot — return before eviction.
            return ReassemblyOutcome::Complete(done);
        }

        // The fragment must be retained — only NOW ensure capacity, evicting the
        // least-recently-updated slot because we are actually storing this one.
        if self.slots.len() >= self.capacity {
            if let Some((idx, _)) = self.slots.iter().enumerate().min_by_key(|(_, s)| s.last_ms) {
                self.slots.remove(idx);
            } else {
                return ReassemblyOutcome::Rejected(RejectReason::Capacity);
            }
        }
        self.slots.push(slot);
        ReassemblyOutcome::Pending {
            have: 1,
            total: frag.total,
        }
    }

    fn ingest_existing(
        &mut self,
        idx: usize,
        frag: &BlockFragment,
        now_ms: u64,
    ) -> ReassemblyOutcome {
        let slot = &mut self.slots[idx];
        if slot.total != frag.total {
            return ReassemblyOutcome::Rejected(RejectReason::TotalMismatch);
        }
        if slot.parts[frag.seq as usize].is_some() {
            // H3: do NOT refresh last_ms on pure duplicates — otherwise an
            // attacker can pin all MAX_IN_FLIGHT slots forever by replaying
            // fragment 0 and blocking legitimate reassembly via expire/LRU.
            return ReassemblyOutcome::Duplicate;
        }
        let new_total = slot.bytes_so_far.saturating_add(frag.bytes.len());
        if new_total > MAX_BLOCK_BYTES {
            return ReassemblyOutcome::Rejected(RejectReason::Oversize);
        }
        slot.bytes_so_far = new_total;
        slot.parts[frag.seq as usize] = Some(frag.bytes.clone());
        slot.last_ms = now_ms;
        let have = slot.have_count();
        let total = slot.total;
        if let Some(done) = slot.try_complete() {
            self.slots.remove(idx);
            return ReassemblyOutcome::Complete(done);
        }
        ReassemblyOutcome::Pending { have, total }
    }
}

/// Slice a complete block payload into [`BlockFragment`]s that fit the mesh
/// payload budget. `chunk_bytes` is clamped to `1..=MAX_FRAGMENT_BYTES`.
/// Returns `None` if the block is empty, oversize, or would need more than
/// [`MAX_BLOCK_FRAGMENTS`] pieces.
pub fn fragment_block(id: u32, block: &[u8], chunk_bytes: usize) -> Option<Vec<BlockFragment>> {
    if block.is_empty() || block.len() > MAX_BLOCK_BYTES {
        return None;
    }
    let chunk = chunk_bytes.clamp(1, MAX_FRAGMENT_BYTES);
    let total_usize = (block.len() + chunk - 1) / chunk;
    if total_usize == 0 || total_usize > MAX_BLOCK_FRAGMENTS as usize {
        return None;
    }
    let total = total_usize as u8;
    let mut out = Vec::with_capacity(total_usize);
    for (seq, piece) in block.chunks(chunk).enumerate() {
        out.push(BlockFragment {
            id,
            seq: seq as u8,
            total,
            bytes: piece.to_vec(),
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(id: u32, seq: u8, total: u8, bytes: &[u8]) -> BlockFragment {
        BlockFragment {
            id,
            seq,
            total,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn single_fragment_block_must_not_evict_legit_reassembly() {
        // At capacity with two legitimate in-flight (multi-fragment)
        // reassemblies, a valid single-fragment block completes immediately and
        // occupies no slot — it must NOT evict a stored reassembly. Before the
        // fix the LRU eviction ran unconditionally on any new id, so a cheap
        // stream of single-fragment frames flushed every in-progress found-block
        // reassembly (lost submitblock / forfeited solo reward).
        let mut r = BlockReassembler::with_capacity(2, 1_000_000);
        assert!(matches!(
            r.ingest(&frag(1, 0, 2, b"a"), 10),
            ReassemblyOutcome::Pending { have: 1, total: 2 }
        ));
        assert!(matches!(
            r.ingest(&frag(2, 0, 2, b"b"), 20),
            ReassemblyOutcome::Pending { have: 1, total: 2 }
        ));
        assert_eq!(r.in_flight(), 2);
        match r.ingest(&frag(99, 0, 1, b"z"), 30) {
            ReassemblyOutcome::Complete(p) => assert_eq!(p, b"z"),
            other => panic!("single-fragment block must complete, got {other:?}"),
        }
        assert_eq!(
            r.in_flight(),
            2,
            "single-fragment completion must not evict in-flight reassemblies"
        );
        // And both stored reassemblies still complete correctly afterwards.
        match r.ingest(&frag(1, 1, 2, b"a"), 40) {
            ReassemblyOutcome::Complete(p) => assert_eq!(p, b"aa"),
            other => panic!("reassembly 1 must survive, got {other:?}"),
        }
    }

    #[test]
    fn reassemble_in_order() {
        let mut r = BlockReassembler::new();
        assert!(matches!(
            r.ingest(&frag(1, 0, 3, b"aaa"), 10),
            ReassemblyOutcome::Pending { have: 1, total: 3 }
        ));
        assert!(matches!(
            r.ingest(&frag(1, 1, 3, b"bbb"), 11),
            ReassemblyOutcome::Pending { have: 2, total: 3 }
        ));
        match r.ingest(&frag(1, 2, 3, b"ccc"), 12) {
            ReassemblyOutcome::Complete(p) => assert_eq!(p, b"aaabbbccc"),
            other => panic!("expected complete, got {other:?}"),
        }
        assert_eq!(r.in_flight(), 0);
    }

    #[test]
    fn reassemble_out_of_order_and_duplicate() {
        let mut r = BlockReassembler::new();
        r.ingest(&frag(7, 2, 3, b"CC"), 1);
        r.ingest(&frag(7, 0, 3, b"AA"), 2);
        assert_eq!(
            r.ingest(&frag(7, 0, 3, b"AA"), 3),
            ReassemblyOutcome::Duplicate
        );
        match r.ingest(&frag(7, 1, 3, b"BB"), 4) {
            ReassemblyOutcome::Complete(p) => assert_eq!(p, b"AABBCC"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_fragment_stays_pending_until_expire() {
        let mut r = BlockReassembler::with_capacity(2, 100);
        r.ingest(&frag(1, 0, 2, b"x"), 0);
        assert_eq!(r.in_flight(), 1);
        assert_eq!(r.expire(50), 0);
        assert_eq!(r.expire(101), 1);
        assert_eq!(r.in_flight(), 0);
    }

    #[test]
    fn rejects_bad_bounds() {
        let mut r = BlockReassembler::new();
        assert_eq!(
            r.ingest(&frag(1, 0, 0, b"x"), 0),
            ReassemblyOutcome::Rejected(RejectReason::BadTotal)
        );
        assert_eq!(
            r.ingest(&frag(1, 5, 3, b"x"), 0),
            ReassemblyOutcome::Rejected(RejectReason::BadSeq)
        );
        assert_eq!(
            r.ingest(&frag(1, 0, 1, b""), 0),
            ReassemblyOutcome::Rejected(RejectReason::BadPayload)
        );
        let big = vec![0u8; MAX_FRAGMENT_BYTES + 1];
        assert_eq!(
            r.ingest(&frag(1, 0, 1, &big), 0),
            ReassemblyOutcome::Rejected(RejectReason::BadPayload)
        );
    }

    #[test]
    fn total_mismatch_rejected() {
        let mut r = BlockReassembler::new();
        r.ingest(&frag(1, 0, 2, b"a"), 0);
        assert_eq!(
            r.ingest(&frag(1, 1, 3, b"b"), 1),
            ReassemblyOutcome::Rejected(RejectReason::TotalMismatch)
        );
    }

    #[test]
    fn fragment_block_round_trips_through_reassembler() {
        let block: Vec<u8> = (0u8..200).collect();
        let frags = fragment_block(0x42, &block, 64).expect("fragment");
        assert!(frags.len() >= 3);
        let mut r = BlockReassembler::new();
        let mut done = None;
        // Feed reverse order to stress reordering.
        for f in frags.iter().rev() {
            match r.ingest(f, 100) {
                ReassemblyOutcome::Complete(p) => done = Some(p),
                ReassemblyOutcome::Pending { .. } | ReassemblyOutcome::Duplicate => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(done.expect("complete"), block);
    }

    #[test]
    fn fragment_block_rejects_oversize() {
        let huge = vec![0u8; MAX_BLOCK_BYTES + 1];
        assert!(fragment_block(1, &huge, 64).is_none());
        assert!(fragment_block(1, &[], 64).is_none());
    }

    #[test]
    fn capacity_evicts_lru() {
        let mut r = BlockReassembler::with_capacity(2, 1_000_000);
        r.ingest(&frag(1, 0, 2, b"a"), 10);
        r.ingest(&frag(2, 0, 2, b"b"), 20);
        // Third id should evict id=1 (older last_ms).
        r.ingest(&frag(3, 0, 2, b"c"), 30);
        assert_eq!(r.in_flight(), 2);
        // Completing id=1 is now a *new* slot (old was evicted).
        match r.ingest(&frag(1, 0, 2, b"a"), 40) {
            ReassemblyOutcome::Pending { have: 1, total: 2 } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn duplicate_fragment_does_not_refresh_ttl() {
        let mut r = BlockReassembler::with_capacity(1, 100);
        r.ingest(&frag(1, 0, 2, b"a"), 0);
        // Spam duplicates at late timestamps — must NOT pin the slot.
        for t in 1..50 {
            assert_eq!(
                r.ingest(&frag(1, 0, 2, b"a"), t),
                ReassemblyOutcome::Duplicate
            );
        }
        // Past stale_ms from original last_ms (=0), slot expires.
        assert_eq!(r.expire(101), 1);
        assert_eq!(r.in_flight(), 0);
    }
}
