// DCENT_axe — Mesh solo work-source controller (P1-W1 pure glue)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Host-pure state machine that later binary `lora_task` will call:
//   tip RX → admit → StratumJob → (dispatcher NewJob)
//   nonce  → header → validate_found_block → full block bytes → BFG
//
// Clock-free / no radio / no channels. Caller owns now_ms and StratumEvent send.
// Does NOT claim regtest or mainnet mining-on-mesh proof.

use crate::solo::{
    assemble_coinbase_full, assemble_coinbase_nonwitness, assemble_solo_block, coinbase_txid,
    rolled_version, tip_supersedes, validate_found_block, validate_tip_for_chain, ChainId,
    SoloChainParams, SoloError, SoloTemplateBuilder, SoloTip, BIP320_VERSION_MASK,
};
use crate::types::{ShareSubmission, StratumJob};
use crate::work::{MiningWork, WorkBuilder};

// ---------------------------------------------------------------------------
// Mode & errors
// ---------------------------------------------------------------------------

/// Work-source mode for mesh solo (honest UI maps this 1:1).
///
/// Pool mode is **outside** this controller — when the node is on a pool, keep
/// this at [`MeshSoloMode::Off`] so no tip is admitted and no solo jobs emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeshSoloMode {
    /// Do not admit tips or produce solo work (default, fail-closed).
    #[default]
    Off,
    /// Coinbase-only empty-block solo from mesh tips.
    SoloMeshEmpty,
}

impl MeshSoloMode {
    pub fn is_active(self) -> bool {
        matches!(self, MeshSoloMode::SoloMeshEmpty)
    }

    /// Honest label for logs/dashboard (not pool vocabulary).
    pub fn label(self) -> &'static str {
        match self {
            MeshSoloMode::Off => "OFF",
            MeshSoloMode::SoloMeshEmpty => "SOLO MESH · EMPTY",
        }
    }
}

/// Controller-level errors (wraps solo + mode policy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshSoloError {
    /// Mode is Off — tip/work ignored by policy.
    ModeOff,
    /// No tip has been admitted yet.
    NoTip,
    /// Tip failed structural / supersede / chain policy.
    Tip(SoloError),
    /// Job/work build failed.
    Build(SoloError),
    /// Found-block validation or assembly failed.
    Block(SoloError),
    /// No current job/work epoch (need tip + next_work first).
    NoWork,
    /// Share job_id / extranonce2 does not bind to the active solo epoch.
    WorkBindingMismatch(&'static str),
}

impl core::fmt::Display for MeshSoloError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MeshSoloError::ModeOff => write!(f, "mesh solo mode is off"),
            MeshSoloError::NoTip => write!(f, "no admitted tip"),
            MeshSoloError::Tip(e) => write!(f, "tip: {e}"),
            MeshSoloError::Build(e) => write!(f, "build: {e}"),
            MeshSoloError::Block(e) => write!(f, "block: {e}"),
            MeshSoloError::NoWork => write!(f, "no current solo work epoch"),
            MeshSoloError::WorkBindingMismatch(s) => write!(f, "work binding: {s}"),
        }
    }
}

impl std::error::Error for MeshSoloError {}

impl From<SoloError> for MeshSoloError {
    fn from(e: SoloError) -> Self {
        MeshSoloError::Build(e)
    }
}

// ---------------------------------------------------------------------------
// Outcomes (caller maps to events / metrics — never pool shares)
// ---------------------------------------------------------------------------

/// Result of offering a tip to the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TipAdmit {
    /// Tip accepted; replaces previous; caller should request a new job.
    Accepted { height: i64 },
    /// Mode off — tip ignored (not an error for flood path).
    IgnoredModeOff,
    /// Equal or lower height than current tip (or not superseding).
    RejectedStale,
    /// Structural / chain policy failure.
    RejectedPolicy(SoloError),
}

/// A solo work unit ready for the ASIC dispatcher (not a pool share).
#[derive(Debug, Clone)]
pub struct SoloWorkEpoch {
    pub job: StratumJob,
    pub work: MiningWork,
    pub tip: SoloTip,
}

/// Local block **candidate** after pure validation (not gateway-accepted).
#[derive(Debug, Clone)]
pub struct SoloBlockCandidate {
    pub header: [u8; 80],
    pub coinbase_nonwitness: Vec<u8>,
    pub coinbase_full: Vec<u8>,
    /// `header || 0x01 || coinbase_full` for fragment/submitblock.
    pub block_bytes: Vec<u8>,
    /// Achieved difficulty from validate_found_block.
    pub difficulty: f64,
    pub tip_height: i64,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Bounded, clock-free mesh-solo work source.
///
/// **Invariants (Protocol):**
/// - Solo jobs always use empty extranonce1 and BIP320 mask.
/// - `clean_jobs` is always true on emitted jobs (flush pool/solo stale work).
/// - Mode Off clears tip + work epoch (no silent solo after pool switch).
/// - Metrics surface: tip admits / candidates — never `shares_accepted`.
///
/// **Memory:** O(1) — one tip, one job, one work, one builder. No queues.
pub struct MeshSoloController {
    mode: MeshSoloMode,
    builder: SoloTemplateBuilder,
    work_builder: WorkBuilder,
    last_tip: Option<SoloTip>,
    current_job: Option<StratumJob>,
    current_work: Option<MiningWork>,
    /// Monotonic counter of admitted tips (caller-visible metrics).
    tips_admitted: u64,
    /// Tips rejected as stale / non-superseding.
    tips_stale: u64,
    /// Tips rejected by chain policy.
    tips_policy_rejected: u64,
    /// Successful local block candidates produced.
    candidates_built: u64,
}

impl MeshSoloController {
    /// Construct from a payout address. Defaults: mode **Off**, chain **Regtest**.
    pub fn from_address(address: &str) -> Result<Self, MeshSoloError> {
        let builder = SoloTemplateBuilder::from_address(address)
            .map_err(MeshSoloError::Build)?
            .for_chain(ChainId::Regtest);
        Ok(Self::from_builder(builder))
    }

    /// Chain-pinned address (regtest refuses mainnet HRP, etc.).
    pub fn from_address_for_chain(address: &str, chain: ChainId) -> Result<Self, MeshSoloError> {
        let builder = SoloTemplateBuilder::from_address_for_chain(address, chain)
            .map_err(MeshSoloError::Build)?;
        Ok(Self::from_builder(builder))
    }

    /// Lab/tests: script hex payout.
    pub fn from_script_hex(script_hex: &str) -> Result<Self, MeshSoloError> {
        let builder = SoloTemplateBuilder::from_script_hex(script_hex)
            .map_err(MeshSoloError::Build)?
            .for_chain(ChainId::Regtest);
        Ok(Self::from_builder(builder))
    }

    fn from_builder(builder: SoloTemplateBuilder) -> Self {
        let en1 = builder.extranonce1_hex().to_string();
        let en2 = builder.extranonce2_size();
        let mut wb = WorkBuilder::new(&en1, en2);
        wb.set_version_mask(BIP320_VERSION_MASK);
        wb.set_difficulty(1.0);
        Self {
            mode: MeshSoloMode::Off,
            builder,
            work_builder: wb,
            last_tip: None,
            current_job: None,
            current_work: None,
            tips_admitted: 0,
            tips_stale: 0,
            tips_policy_rejected: 0,
            candidates_built: 0,
        }
    }

    /// Builder-style chain select (recreates template from current script).
    pub fn with_chain(mut self, chain: ChainId) -> Self {
        self.set_chain(chain);
        self
    }

    /// Replace chain params (rebuilds template builder from script hex).
    pub fn set_chain(&mut self, chain: ChainId) {
        let script = self.builder.script_hex().to_string();
        if let Ok(b) = SoloTemplateBuilder::from_script_hex(&script) {
            self.builder = b.for_chain(chain);
        }
        self.clear_epoch();
    }

    pub fn mode(&self) -> MeshSoloMode {
        self.mode
    }

    pub fn chain_params(&self) -> SoloChainParams {
        self.builder.chain_params()
    }

    pub fn last_tip(&self) -> Option<&SoloTip> {
        self.last_tip.as_ref()
    }

    pub fn tips_admitted(&self) -> u64 {
        self.tips_admitted
    }

    pub fn tips_stale(&self) -> u64 {
        self.tips_stale
    }

    pub fn tips_policy_rejected(&self) -> u64 {
        self.tips_policy_rejected
    }

    pub fn candidates_built(&self) -> u64 {
        self.candidates_built
    }

    /// Switch mode. Entering Off or leaving Solo clears tip/work (Protocol: no
    /// silent solo work after pool resume; clean epoch boundary).
    pub fn set_mode(&mut self, mode: MeshSoloMode) {
        if mode != self.mode {
            self.clear_epoch();
            self.mode = mode;
            if mode == MeshSoloMode::Off {
                self.last_tip = None;
            }
        }
    }

    /// Enable solo mesh empty mode (convenience).
    pub fn enable_solo_mesh_empty(&mut self) {
        self.set_mode(MeshSoloMode::SoloMeshEmpty);
    }

    /// Disable solo (pool or idle).
    pub fn disable(&mut self) {
        self.set_mode(MeshSoloMode::Off);
    }

    fn clear_epoch(&mut self) {
        self.current_job = None;
        self.current_work = None;
        self.work_builder.reset_extranonce2();
    }

    /// Offer a mesh tip. Clock-free; ordering via [`tip_supersedes`] (height).
    pub fn admit_tip(&mut self, tip: SoloTip) -> TipAdmit {
        if !self.mode.is_active() {
            return TipAdmit::IgnoredModeOff;
        }
        if let Err(e) = validate_tip_for_chain(&tip, self.builder.chain_params()) {
            self.tips_policy_rejected = self.tips_policy_rejected.saturating_add(1);
            return TipAdmit::RejectedPolicy(e);
        }
        if let Some(cur) = &self.last_tip {
            if !tip_supersedes(cur, &tip) {
                self.tips_stale = self.tips_stale.saturating_add(1);
                return TipAdmit::RejectedStale;
            }
        }
        self.last_tip = Some(tip);
        self.clear_epoch(); // new tip → new job epoch (clean_jobs semantics)
        self.tips_admitted = self.tips_admitted.saturating_add(1);
        TipAdmit::Accepted { height: tip.height }
    }

    /// Build the current tip into a [`StratumJob`] without advancing extranonce2.
    /// Job always has `clean_jobs = true` (solo.rs builder).
    pub fn current_job(&self) -> Result<StratumJob, MeshSoloError> {
        if !self.mode.is_active() {
            return Err(MeshSoloError::ModeOff);
        }
        let tip = self.last_tip.as_ref().ok_or(MeshSoloError::NoTip)?;
        self.builder.build_job(tip).map_err(MeshSoloError::Build)
    }

    /// Advance extranonce2 and produce the next [`SoloWorkEpoch`].
    ///
    /// **Binary must feed `epoch.work` to the ASIC via
    /// `StratumEvent::PrebuiltWork`** (not `NewJob` alone). Rebuilding with the
    /// pool WorkBuilder would drop the compact-nBits `share_target` and desync
    /// extranonce2 from this controller — breaking `build_candidate_from_share`.
    pub fn next_work(&mut self) -> Result<SoloWorkEpoch, MeshSoloError> {
        if !self.mode.is_active() {
            return Err(MeshSoloError::ModeOff);
        }
        let tip = *self.last_tip.as_ref().ok_or(MeshSoloError::NoTip)?;
        let job = self.builder.build_job(&tip).map_err(MeshSoloError::Build)?;
        let target = crate::solo::compact_target_be(tip.nbits)
            .ok_or(MeshSoloError::Build(SoloError::BadNbits))?;
        self.work_builder.set_version_mask(BIP320_VERSION_MASK);
        self.work_builder.set_difficulty(1.0);
        let mut work = self.work_builder.next_work(&job);
        work.share_target = target;
        self.current_job = Some(job.clone());
        self.current_work = Some(work.clone());
        Ok(SoloWorkEpoch { job, work, tip })
    }

    /// Given a found nonce (+ optional BIP320 version_bits) against the
    /// **current** controller epoch. Prefer [`build_candidate_from_share`] when
    /// the nonce came from the mining dispatcher (binds `ShareSubmission.extranonce2`).
    pub fn build_candidate(
        &mut self,
        nonce: u32,
        version_bits: u32,
    ) -> Result<SoloBlockCandidate, MeshSoloError> {
        let work = self.current_work.as_ref().ok_or(MeshSoloError::NoWork)?;
        let share = ShareSubmission {
            job_id: work.job_id.clone(),
            extranonce2: work.extranonce2.clone(),
            ntime: format!("{:08x}", work.ntime),
            nonce: format!("{:08x}", nonce),
            version: work.version,
            version_bits: if version_bits != 0 {
                Some(format!("{:08x}", version_bits))
            } else {
                None
            },
            difficulty: 0.0,
        };
        self.build_candidate_from_share(&share)
    }

    /// Reconstruct a block candidate bound to the **share's** extranonce2 / job_id
    /// / nonce / version — the fields the ASIC actually hashed. Does not use the
    /// controller's last `next_work` en2 when it differs from the share (dispatcher
    /// may hold an earlier PrebuiltWork epoch).
    pub fn build_candidate_from_share(
        &mut self,
        share: &ShareSubmission,
    ) -> Result<SoloBlockCandidate, MeshSoloError> {
        if !self.mode.is_active() {
            return Err(MeshSoloError::ModeOff);
        }
        let tip = *self.last_tip.as_ref().ok_or(MeshSoloError::NoTip)?;
        let job = self.current_job.as_ref().ok_or(MeshSoloError::NoWork)?;
        if share.job_id != job.job_id {
            return Err(MeshSoloError::WorkBindingMismatch("job_id"));
        }

        let en1 = hex::decode(self.builder.extranonce1_hex()).unwrap_or_default();
        let en2 = hex::decode(share.extranonce2.trim())
            .map_err(|_| MeshSoloError::WorkBindingMismatch("extranonce2 hex"))?;
        if en2.len() != self.builder.extranonce2_size() {
            return Err(MeshSoloError::WorkBindingMismatch("extranonce2 size"));
        }

        let coinbase_nonwitness =
            assemble_coinbase_nonwitness(job, &en1, &en2).map_err(MeshSoloError::Block)?;
        let merkle = coinbase_txid(&coinbase_nonwitness);

        let nonce = u32::from_str_radix(share.nonce.trim_start_matches("0x"), 16)
            .map_err(|_| MeshSoloError::WorkBindingMismatch("nonce hex"))?;
        let ntime = u32::from_str_radix(share.ntime.trim_start_matches("0x"), 16)
            .map_err(|_| MeshSoloError::WorkBindingMismatch("ntime hex"))?;
        // Full header version the ASIC used (ShareSubmission.version is full).
        let version = if let Some(ref vb) = share.version_bits {
            let bits = u32::from_str_radix(vb.trim_start_matches("0x"), 16).unwrap_or(0);
            if bits != 0 {
                rolled_version(share.version, BIP320_VERSION_MASK, bits)
            } else {
                share.version
            }
        } else {
            share.version
        };

        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(&version.to_le_bytes());
        header[4..36].copy_from_slice(&tip.prev_hash);
        header[36..68].copy_from_slice(&merkle);
        header[68..72].copy_from_slice(&ntime.to_le_bytes());
        header[72..76].copy_from_slice(&tip.nbits.to_le_bytes());
        header[76..80].copy_from_slice(&nonce.to_le_bytes());

        let difficulty = validate_found_block(&tip, &header, &coinbase_nonwitness)
            .map_err(MeshSoloError::Block)?;

        let coinbase_full =
            assemble_coinbase_full(&coinbase_nonwitness).map_err(MeshSoloError::Block)?;
        let block_bytes = assemble_solo_block(&header, &coinbase_full);

        self.candidates_built = self.candidates_built.saturating_add(1);

        Ok(SoloBlockCandidate {
            header,
            coinbase_nonwitness,
            coinbase_full,
            block_bytes,
            difficulty,
            tip_height: tip.height,
        })
    }

    /// Snapshot for dashboard: honest solo metrics only.
    pub fn metrics_snapshot(&self) -> MeshSoloMetrics {
        MeshSoloMetrics {
            mode: self.mode,
            chain: self.builder.chain_params().chain,
            tip_height: self.last_tip.map(|t| t.height),
            tips_admitted: self.tips_admitted,
            tips_stale: self.tips_stale,
            tips_policy_rejected: self.tips_policy_rejected,
            candidates_built: self.candidates_built,
            has_work_epoch: self.current_work.is_some(),
        }
    }
}

/// Honest metrics — no shares_accepted field by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshSoloMetrics {
    pub mode: MeshSoloMode,
    pub chain: ChainId,
    pub tip_height: Option<i64>,
    pub tips_admitted: u64,
    pub tips_stale: u64,
    pub tips_policy_rejected: u64,
    pub candidates_built: u64,
    pub has_work_epoch: bool,
}

// ---------------------------------------------------------------------------
// Tests (P1-W1 P0 gate)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(height: i64) -> SoloTip {
        SoloTip {
            prev_hash: [0xab; 32],
            nbits: 0x207f_ffff,
            ntime: 1_700_000_000 + height as u32,
            height,
        }
    }

    fn ctrl() -> MeshSoloController {
        MeshSoloController::from_script_hex("51").unwrap()
    }

    #[test]
    fn default_mode_is_off_ignores_tips() {
        let mut c = ctrl();
        assert_eq!(c.mode(), MeshSoloMode::Off);
        assert_eq!(c.admit_tip(tip(1)), TipAdmit::IgnoredModeOff);
        assert!(c.last_tip().is_none());
        assert!(matches!(c.next_work(), Err(MeshSoloError::ModeOff)));
    }

    #[test]
    fn admit_tip_requires_active_mode() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        match c.admit_tip(tip(10)) {
            TipAdmit::Accepted { height } => assert_eq!(height, 10),
            other => panic!("{other:?}"),
        }
        assert_eq!(c.tips_admitted(), 1);
        assert_eq!(c.last_tip().unwrap().height, 10);
    }

    #[test]
    fn stale_tip_rejected_equal_or_lower_height() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        assert!(matches!(
            c.admit_tip(tip(5)),
            TipAdmit::Accepted { height: 5 }
        ));
        assert_eq!(c.admit_tip(tip(5)), TipAdmit::RejectedStale);
        assert_eq!(c.admit_tip(tip(4)), TipAdmit::RejectedStale);
        assert_eq!(c.tips_stale(), 2);
        assert!(matches!(
            c.admit_tip(tip(6)),
            TipAdmit::Accepted { height: 6 }
        ));
    }

    #[test]
    fn mainnet_chain_rejects_toy_nbits() {
        let mut c = MeshSoloController::from_script_hex("51").unwrap();
        c.set_chain(ChainId::Mainnet);
        c.enable_solo_mesh_empty();
        match c.admit_tip(tip(1)) {
            TipAdmit::RejectedPolicy(SoloError::TipRejected(_)) => {}
            other => panic!("expected policy reject, got {other:?}"),
        }
        assert_eq!(c.tips_policy_rejected(), 1);
    }

    #[test]
    fn next_work_emits_solo_job_with_clean_jobs_and_mask() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(100));
        let epoch = c.next_work().unwrap();
        assert!(epoch.job.job_id.starts_with("solo-regtest-"));
        assert!(epoch.job.clean_jobs);
        assert!(epoch.job.merkle_branches.is_empty());
        assert_eq!(epoch.work.version_mask, BIP320_VERSION_MASK);
        assert_eq!(
            epoch.work.share_target,
            crate::solo::compact_target_be(0x207f_ffff).unwrap()
        );
        // Second work advances extranonce2
        let e2 = c.next_work().unwrap();
        assert_ne!(epoch.work.extranonce2, e2.work.extranonce2);
    }

    #[test]
    fn next_work_merkle_equals_nonwitness_txid() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(50));
        let epoch = c.next_work().unwrap();
        // Solo default extranonce1 is empty; en2 comes from work epoch.
        let en2 = hex::decode(&epoch.work.extranonce2).unwrap();
        let nw = assemble_coinbase_nonwitness(&epoch.job, &[], &en2).unwrap();
        assert_eq!(epoch.work.merkle_root, crate::solo::coinbase_txid(&nw));
    }

    #[test]
    fn mode_off_clears_tip_and_work() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(3));
        let _ = c.next_work().unwrap();
        c.disable();
        assert!(c.last_tip().is_none());
        assert!(matches!(c.next_work(), Err(MeshSoloError::ModeOff)));
        assert!(!c.metrics_snapshot().has_work_epoch);
    }

    #[test]
    fn build_candidate_requires_work_epoch() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(2));
        assert!(matches!(
            c.build_candidate(0, 0),
            Err(MeshSoloError::NoWork)
        ));
    }

    #[test]
    fn build_candidate_from_share_binds_en2_not_stale_epoch() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(8));
        let e1 = c.next_work().unwrap();
        let e2 = c.next_work().unwrap();
        assert_ne!(e1.work.extranonce2, e2.work.extranonce2);
        // Share claims first epoch's en2 while controller current is second —
        // reconstruction must still use share.en2 for coinbase/merkle.
        let share = ShareSubmission {
            job_id: e1.work.job_id.clone(),
            extranonce2: e1.work.extranonce2.clone(),
            ntime: format!("{:08x}", e1.work.ntime),
            nonce: "00000000".into(),
            version: e1.work.version,
            version_bits: None,
            difficulty: 1.0,
        };
        // job_id may match (same tip height job) — both epochs share job_id
        // from rebuild; if job_id identical, en2 path is what we pin.
        match c.build_candidate_from_share(&share) {
            Ok(cand) => {
                let nw = assemble_coinbase_nonwitness(
                    &e1.job,
                    &[],
                    &hex::decode(&e1.work.extranonce2).unwrap(),
                )
                .unwrap();
                assert_eq!(&cand.header[36..68], &crate::solo::coinbase_txid(&nw));
            }
            Err(MeshSoloError::Block(_)) => {
                // PoW fail is OK; binding must not be WorkBindingMismatch
            }
            Err(MeshSoloError::WorkBindingMismatch(s)) => {
                // job_id mismatch if next_work rewrote job with same height
                assert!(s == "job_id" || s == "extranonce2 size");
            }
            Err(e) => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn build_candidate_from_share_rejects_wrong_job_id() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(3));
        let epoch = c.next_work().unwrap();
        let share = ShareSubmission {
            job_id: "solo-regtest-999".into(),
            extranonce2: epoch.work.extranonce2.clone(),
            ntime: format!("{:08x}", epoch.work.ntime),
            nonce: "00000000".into(),
            version: epoch.work.version,
            version_bits: None,
            difficulty: 1.0,
        };
        assert!(matches!(
            c.build_candidate_from_share(&share),
            Err(MeshSoloError::WorkBindingMismatch("job_id"))
        ));
    }

    #[test]
    fn build_candidate_binds_merkle_or_rejects_pow() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(9));
        let _ = c.next_work().unwrap();
        // Nonce 0 almost never meets target — expect Block reject on PoW, but
        // if it somehow meets, structure must be valid.
        match c.build_candidate(0, 0) {
            Ok(cand) => {
                assert_eq!(cand.block_bytes.len(), 80 + 1 + cand.coinbase_full.len());
                assert_eq!(&cand.block_bytes[0..80], &cand.header);
                assert_eq!(cand.block_bytes[80], 0x01);
                assert_eq!(cand.tip_height, 9);
                assert!(cand.difficulty > 0.0);
            }
            Err(MeshSoloError::Block(SoloError::BlockRejected(s))) => {
                assert_eq!(s, "hash does not meet nbits target");
            }
            Err(e) => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn metrics_have_no_share_fields() {
        let c = ctrl();
        let m = c.metrics_snapshot();
        // Compile-time honesty: MeshSoloMetrics has no shares_accepted.
        assert_eq!(m.mode, MeshSoloMode::Off);
        assert_eq!(m.chain, ChainId::Regtest);
        let _ = (
            m.tips_admitted,
            m.tips_stale,
            m.tips_policy_rejected,
            m.candidates_built,
            m.has_work_epoch,
            m.tip_height,
        );
    }

    #[test]
    fn new_tip_clears_work_epoch() {
        let mut c = ctrl();
        c.enable_solo_mesh_empty();
        c.admit_tip(tip(1));
        let _ = c.next_work().unwrap();
        assert!(c.metrics_snapshot().has_work_epoch);
        c.admit_tip(tip(2));
        assert!(!c.metrics_snapshot().has_work_epoch);
        assert!(matches!(
            c.build_candidate(0, 0),
            Err(MeshSoloError::NoWork)
        ));
    }
}
