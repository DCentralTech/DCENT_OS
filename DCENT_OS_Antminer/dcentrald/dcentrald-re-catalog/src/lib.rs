//! Pure, no-HAL mirror of the reverse-engineering facts consumed by offline
//! Antminer bring-up proofs.
//!
//! Every hardware value carries a repository-relative provenance path. The
//! catalog deliberately distinguishes exact evidence, structural evidence,
//! and scaffolds so an emulator result cannot silently become a live-hardware
//! claim.
//!
//! # Adding a new ASIC model / chip (contributor guide)
//!
//! Support for a new Antminer is **data first**: add rows here, not a new code
//! path. Two sibling tables must stay consistent — [`model_catalog`] (per-board
//! rows: chip_id, geometry, baud, voltage envelope, evidence strength) and
//! [`pll_bible`] (per-chip PLL register facts). The `#[test]`s in each module
//! ENFORCE the following rules, so a row that breaks them fails CI, not a miner:
//!
//! 1. **Honest provenance.** Every row carries a repo-relative source path, and
//!    its `EvidenceStrength` (or the pll_bible `SCAFFOLD_NO_GROUND_TRUTH`
//!    marker) reflects what you actually have. **Never fabricate or project** a
//!    frequency/voltage/geometry you have not measured or RE'd — leave it `None`
//!    (see the S23/`0x1372` scaffold row, which is deliberately field-free).
//! 2. **chip_id agreement.** Use the id the silicon SELF-REPORTS on enumeration
//!    (e.g. BM1373/S23 enumerates as `0x1372` even though its canonical id is
//!    `0x1373` — the driver dual-keys, but the catalog uses the enumerated id).
//!    A `pll_bible` chip_id with a representative frequency must equal the
//!    `default_frequency_mhz` of at least one `model_catalog` board with that
//!    chip_id (`representative_freq_matches_a_catalogued_board`).
//! 3. **Ordered voltage bounds.** If you set both, `voltage_min_mv <
//!    voltage_max_mv` (`voltage_bounds_are_ordered_when_present`).
//! 4. **PLL coverage for proven chips.** An `Exact` model row that carries a
//!    proven `default_frequency_mhz` must also have a `pll_bible` entry for its
//!    chip_id (`exact_models_with_a_frequency_have_pll_facts`) — a proven
//!    operating point implies you have the PLL register facts to program it.
//!
//! For the runtime chip-driver side of a new chip (init sequence, PLL encode,
//! nonce decode), see `dcentrald-asic::drivers` and its `ChipDriver` trait.

#![forbid(unsafe_code)]

pub mod fan_curves {
    //! Fan-curve port remains deferred; simulator fan behavior is model-state
    //! based and does not claim a golden curve yet.
}
pub mod model_catalog;
pub mod pll_bible;

pub use model_catalog::{model_evidence, EvidenceStrength, ModelEvidence, ANTMINER_MODELS};
pub use pll_bible::{pll_expectation, PllExpectation, PLL_EXPECTATIONS};
