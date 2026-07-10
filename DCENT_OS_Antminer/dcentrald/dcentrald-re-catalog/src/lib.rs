//! `dcentrald-re-catalog` — INTENDED typed Rust mirror of the +6
//! master research catalogs.
//!
//! ## ⚠️ STATUS (as of 2026-05-29): STUB — not populated, no consumers, no drift tests yet.
//!
//! This crate is the *intended* home of the KB-Wires-To-Code mirror (per
//! ), but **the catalog content has
//! NOT been ported.** All three modules below are empty `pub mod` stubs, the
//! `pub use` re-exports are commented out, and the crate currently exports
//! **zero** types / statics / functions. **There are NO consumers today** — in
//! particular `dcentrald-api`'s re-catalog route sources its data from
//! `dcentrald-api-types`, NOT from this crate, and no `dcentrald-thermal` /
//! `dcentrald-asic` path imports from here. Do not `use dcentrald_re_catalog::*`
//! expecting populated data — it will not compile.
//!
//! | KB document (PLANNED source — not yet ported)      | Module (stub)      | Rows when ported   |
//! |----------------------------------------------------|--------------------|--------------------|
//! |                              | [`fan_curves`]     | ~118 / 20+ curves  |
//! |                      | [`pll_bible`]      | 13 chip families   |
//! |                           | [`model_catalog`]  | ~94 model rows     |
//!
//! ## When the catalog content IS ported (TODO — not done yet)
//!
//! Keep this crate pure no-HAL (so Windows host tests run without an ARM
//! cross-compile and a catalog typo can't reach `mmap(/dev/mem)`). **At that
//! time**, add a `tests/` suite that pins row counts + representative rows so a
//! one-sided edit of a KB master doc vs the Rust mirror fails CI (the
//! KB-Wires-To-Code drift guard). **Until the content lands that drift guard
//! does NOT exist** — there is currently no `tests/` dir and no paired Rust
//! mirror to update when a KB master doc changes. (gap-swarm no-HAL hunt
//! findings #4+#5: the prior header asserted populated catalogs, working host
//! tests, live consumers, and a mechanical drift guard — none of which exist.)

#![forbid(unsafe_code)]

// W10.11 dx-stability: the three +6 catalog modules were declared
// in W7.1+W7.2+W7.4 but their per-module `.rs` files were never landed.
// This left the crate broken on master HEAD (`cargo build --workspace`
// fails with E0583 "file not found for module"). To unblock workspace
// builds without making up catalog content, the modules are stubbed
// inline as empty `pub mod`s. The `pub use` re-exports stay commented
// out until the catalog content lands. See
//  for the KB-Wires-To-Code
// expectation.

pub mod fan_curves {
    //! Stub.  catalog content (118 rows, 20+ curves) to be ported
    //! from . See parent crate doc for the wiring
    //! pattern.
}

pub mod model_catalog {
    //! Stub.  catalog content (94 model rows) to be ported from
    //! .
}

pub mod pll_bible {
    //! Stub.  catalog content (13 chip families) to be ported
    //! from .
}

// pub use fan_curves::{
//     for_platform_sku, CurveSource, FanCurve, MASTER_FAN_CURVES,
// };
// pub use model_catalog::{ModelEntry, MASTER_MODELS};
// pub use pll_bible::{pll_table_for_chip, PllRegister};
