// SPDX-License-Identifier: GPL-3.0-or-later
//! Gateway-side fragment collector for solo found-block relay (P1-W2 pure).
//!
//! Thin wrapper over [`BlockReassembler`]: clock-free, bounded. Does **not**
//! validate Bitcoin headers (that is `dcentaxe_stratum::gateway_solo`) — keeps
//! the lora ↛ stratum dependency edge clean.
//!
//! Binary composition:
//! ```text
//! GatewayFragmentCollector::ingest(frag, now_ms)
//!   → Complete(bytes)
//! dcentaxe_stratum::GatewaySoloSubmit::prepare(bytes)  // after set_tip
//! ```

use crate::mesh::BlockFragment;
use crate::reassembly::{
    BlockReassembler, ReassemblyOutcome, RejectReason, DEFAULT_STALE_MS, MAX_IN_FLIGHT,
};

/// Outcome of gateway fragment ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayIngest {
    Pending {
        have: u8,
        total: u8,
    },
    /// Full block payload: `header(80) || 0x01 || coinbase_full`.
    Complete(Vec<u8>),
    Duplicate,
    Rejected(RejectReason),
}

/// Bounded multi-block fragment collector for a gateway node.
#[derive(Debug, Clone)]
pub struct GatewayFragmentCollector {
    reassembler: BlockReassembler,
    completes: u64,
    rejects: u64,
}

impl Default for GatewayFragmentCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl GatewayFragmentCollector {
    pub fn new() -> Self {
        Self::with_capacity(MAX_IN_FLIGHT, DEFAULT_STALE_MS)
    }

    pub fn with_capacity(capacity: usize, stale_ms: u64) -> Self {
        Self {
            reassembler: BlockReassembler::with_capacity(capacity, stale_ms),
            completes: 0,
            rejects: 0,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.reassembler.in_flight()
    }

    pub fn completes(&self) -> u64 {
        self.completes
    }

    pub fn rejects(&self) -> u64 {
        self.rejects
    }

    pub fn expire(&mut self, now_ms: u64) -> usize {
        self.reassembler.expire(now_ms)
    }

    /// Ingest one air-received [`BlockFragment`].
    pub fn ingest(&mut self, frag: &BlockFragment, now_ms: u64) -> GatewayIngest {
        match self.reassembler.ingest(frag, now_ms) {
            ReassemblyOutcome::Pending { have, total } => GatewayIngest::Pending { have, total },
            ReassemblyOutcome::Complete(bytes) => {
                self.completes = self.completes.saturating_add(1);
                GatewayIngest::Complete(bytes)
            }
            ReassemblyOutcome::Duplicate => GatewayIngest::Duplicate,
            ReassemblyOutcome::Rejected(r) => {
                self.rejects = self.rejects.saturating_add(1);
                GatewayIngest::Rejected(r)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reassembly::fragment_block;

    #[test]
    fn collector_reassembles_out_of_order() {
        let block: Vec<u8> = (0u8..200).collect();
        let frags = fragment_block(0x99, &block, 64).unwrap();
        let mut g = GatewayFragmentCollector::new();
        let mut done = None;
        for f in frags.iter().rev() {
            match g.ingest(f, 100) {
                GatewayIngest::Complete(b) => done = Some(b),
                GatewayIngest::Pending { .. } | GatewayIngest::Duplicate => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(done.unwrap(), block);
        assert_eq!(g.completes(), 1);
    }

    #[test]
    fn collector_counts_rejects() {
        let mut g = GatewayFragmentCollector::new();
        let bad = BlockFragment {
            id: 1,
            seq: 0,
            total: 0,
            bytes: vec![1],
        };
        assert!(matches!(
            g.ingest(&bad, 0),
            GatewayIngest::Rejected(RejectReason::BadTotal)
        ));
        assert_eq!(g.rejects(), 1);
    }
}
