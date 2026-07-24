//! Pure serial work-engine building blocks (ADR-0009 strangler).
//!
//! Full mining loops stay in `s19j_hybrid_mining` / `serial_mining` / `am3_bb_mining`
//! until extracted. This module holds **host-testable** state-machine pieces:
//! work-history rings, share dedup sets, and job-id stepping — so three engines
//! stop re-implementing the same bookkeeping.
//!
//! I/O (UART, Stratum, BIP320) remains outside this module.

use crate::serial_work_policy::{
    next_asic_job_id, serial_share_dedup_key, should_clear_seen_shares, DEFAULT_SEEN_SHARES_CAP,
    DEFAULT_SERIAL_JOB_ID_STEP, DEFAULT_WORK_HISTORY_PER_ID,
};
use std::collections::{BTreeSet, VecDeque};

/// One stored work candidate keyed by ASIC job-id slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkHistoryEntry {
    pub pool_job_id: String,
    pub extranonce2: String,
    pub ntime: u32,
    /// Base version before chip rolling (when tracked).
    pub version: u32,
}

/// Per-slot ring of recent work for matching nonces back to pool jobs.
#[derive(Debug, Clone)]
pub struct WorkHistoryRing {
    /// Indexed by low 8 bits of ASIC job id (or echoed job id).
    slots: Vec<VecDeque<WorkHistoryEntry>>,
    depth: usize,
}

impl WorkHistoryRing {
    pub fn new(depth: usize) -> Self {
        let depth = depth.max(1);
        Self {
            slots: (0..256).map(|_| VecDeque::with_capacity(depth)).collect(),
            depth,
        }
    }

    pub fn with_default_depth() -> Self {
        Self::new(DEFAULT_WORK_HISTORY_PER_ID)
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    // `slots` always holds exactly 256 deques (one per possible u8 slot, built
    // in `new`), so indexing by a `u8` slot is provably in-bounds.
    #[allow(clippy::indexing_slicing)]
    pub fn push(&mut self, slot: u8, entry: WorkHistoryEntry) {
        let q = &mut self.slots[slot as usize];
        if q.len() >= self.depth {
            q.pop_front();
        }
        q.push_back(entry);
    }

    /// Most recent entry for a slot (nonce match starts here).
    #[allow(clippy::indexing_slicing)] // slots is always 256 (one per u8 slot)
    pub fn latest(&self, slot: u8) -> Option<&WorkHistoryEntry> {
        self.slots[slot as usize].back()
    }

    /// Iterate newest-first for a slot.
    #[allow(clippy::indexing_slicing)] // slots is always 256 (one per u8 slot)
    pub fn iter_newest_first(&self, slot: u8) -> impl Iterator<Item = &WorkHistoryEntry> {
        self.slots[slot as usize].iter().rev()
    }

    pub fn clear_all(&mut self) {
        for q in &mut self.slots {
            q.clear();
        }
    }
}

/// Bounded share-dedup set for serial paths.
#[derive(Debug, Clone)]
pub struct SeenShareSet {
    inner: BTreeSet<(u8, u32, u16)>,
    cap: usize,
}

impl Default for SeenShareSet {
    // Manual, NOT derived: a derived Default sets cap = 0, bypassing new()'s
    // `cap.max(1)` invariant. With cap = 0 the clear-at-cap check
    // (`should_clear_seen_shares(len, 0)` = `len > 0`) fires on every insert
    // BEFORE the membership test, so every share — including an immediate
    // duplicate — reads as new and the dedup is fully defeated (duplicate
    // submits to the pool). Route through the real constructor instead.
    fn default() -> Self {
        Self::with_default_cap()
    }
}

impl SeenShareSet {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: BTreeSet::new(),
            cap: cap.max(1),
        }
    }

    pub fn with_default_cap() -> Self {
        Self::new(DEFAULT_SEEN_SHARES_CAP)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns true if this is a **new** share (inserted).
    pub fn insert(&mut self, asic_job_id: u8, nonce: u32, version_bits: u16) -> bool {
        if should_clear_seen_shares(self.inner.len(), self.cap) {
            self.inner.clear();
        }
        self.inner
            .insert(serial_share_dedup_key(asic_job_id, nonce, version_bits))
    }

    pub fn contains(&self, asic_job_id: u8, nonce: u32, version_bits: u16) -> bool {
        self.inner
            .contains(&serial_share_dedup_key(asic_job_id, nonce, version_bits))
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

/// Job-id stepper for serial ASIC dispatch.
#[derive(Debug, Clone, Copy)]
pub struct AsicJobIdCursor {
    current: u8,
    step: u8,
}

impl AsicJobIdCursor {
    pub fn new(start: u8, step: u8) -> Self {
        Self {
            current: start,
            step: step.max(1),
        }
    }

    pub fn default_serial() -> Self {
        Self::new(0, DEFAULT_SERIAL_JOB_ID_STEP)
    }

    pub fn current(self) -> u8 {
        self.current
    }

    /// Return current id, then advance.
    pub fn take_and_advance(&mut self) -> u8 {
        let id = self.current;
        self.current = next_asic_job_id(self.current, self.step);
        id
    }
}

/// Pure engine bookkeeping bundle (no I/O).
#[derive(Debug, Clone)]
pub struct SerialWorkBookkeeping {
    pub history: WorkHistoryRing,
    pub seen: SeenShareSet,
    pub job_ids: AsicJobIdCursor,
}

impl SerialWorkBookkeeping {
    pub fn hybrid_defaults() -> Self {
        Self {
            history: WorkHistoryRing::with_default_depth(),
            seen: SeenShareSet::with_default_cap(),
            job_ids: AsicJobIdCursor::default_serial(),
        }
    }

    pub fn with_history_depth(depth: usize) -> Self {
        Self {
            history: WorkHistoryRing::new(depth),
            seen: SeenShareSet::with_default_cap(),
            job_ids: AsicJobIdCursor::default_serial(),
        }
    }

    /// On clean_jobs / new block: drop history and dedup.
    pub fn on_clean_jobs(&mut self) {
        self.history.clear_all();
        self.seen.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_share_set_default_dedups_like_new() {
        // D1: the derived Default gave cap=0, which defeated dedup entirely
        // (the clear-at-cap check fired on every insert BEFORE the membership
        // test, so every share read as new). The manual Default routes through
        // the real constructor, so an immediate duplicate is caught.
        let mut s = SeenShareSet::default();
        assert!(s.insert(1, 42, 0), "first insert is new");
        assert!(
            !s.insert(1, 42, 0),
            "an immediate duplicate must be caught, not reported as new"
        );
    }

    #[test]
    fn history_ring_evicts_oldest() {
        let mut ring = WorkHistoryRing::new(2);
        ring.push(
            8,
            WorkHistoryEntry {
                pool_job_id: "a".into(),
                extranonce2: "00".into(),
                ntime: 1,
                version: 0x20000000,
            },
        );
        ring.push(
            8,
            WorkHistoryEntry {
                pool_job_id: "b".into(),
                extranonce2: "01".into(),
                ntime: 2,
                version: 0x20000000,
            },
        );
        ring.push(
            8,
            WorkHistoryEntry {
                pool_job_id: "c".into(),
                extranonce2: "02".into(),
                ntime: 3,
                version: 0x20000000,
            },
        );
        assert_eq!(ring.latest(8).unwrap().pool_job_id, "c");
        let ids: Vec<_> = ring
            .iter_newest_first(8)
            .map(|e| e.pool_job_id.as_str())
            .collect();
        assert_eq!(ids, vec!["c", "b"]);
    }

    #[test]
    fn seen_set_dedups() {
        let mut seen = SeenShareSet::new(64);
        assert!(seen.insert(8, 1, 0));
        assert!(!seen.insert(8, 1, 0));
        assert!(seen.insert(8, 2, 0));
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn seen_set_clears_when_over_cap() {
        let mut seen = SeenShareSet::new(2);
        assert!(seen.insert(1, 1, 0));
        assert!(seen.insert(1, 2, 0));
        assert_eq!(seen.len(), 2);
        // len is not yet > cap, so third insert does not clear first.
        assert!(seen.insert(1, 3, 0));
        assert_eq!(seen.len(), 3);
        // Now len > cap: next insert clears then adds one.
        assert!(seen.insert(1, 4, 0));
        assert_eq!(seen.len(), 1);
        assert!(seen.contains(1, 4, 0));
    }

    #[test]
    fn job_cursor_steps_by_eight() {
        let mut c = AsicJobIdCursor::default_serial();
        assert_eq!(c.take_and_advance(), 0);
        assert_eq!(c.take_and_advance(), 8);
        assert_eq!(c.take_and_advance(), 16);
    }

    #[test]
    fn clean_jobs_resets_bookkeeping() {
        let mut bk = SerialWorkBookkeeping::hybrid_defaults();
        bk.history.push(
            0,
            WorkHistoryEntry {
                pool_job_id: "j".into(),
                extranonce2: "ee".into(),
                ntime: 1,
                version: 0,
            },
        );
        assert!(bk.seen.insert(0, 9, 0));
        bk.on_clean_jobs();
        assert!(bk.history.latest(0).is_none());
        assert!(bk.seen.is_empty());
    }

    #[test]
    fn bb_depth_preset() {
        let bk = SerialWorkBookkeeping::with_history_depth(
            crate::serial_work_policy::AM3_BB_WORK_HISTORY_PER_ID,
        );
        assert_eq!(bk.history.depth(), 128);
    }
}
