//! Antminer T15 (am1-class Zynq XC7Z020) platform constants.
//!
//! The T15 is **not** in the live fleet — every value here is sourced from the
//! DCENT_OS RE Dev Kit (RE2/RE3) porting work order, not from a live probe. The
//! module is intentionally **data-only**: it pins hardware facts as `const`s so
//! a future T15 bring-up has a cited starting point. There are NO flash
//! operations, NO callers, and NO live behavior wired through it.
//!
//! Source corpus: `findings/s20-devkit-re.md` (knowledge-goldmine 2026-06-10),
//! mined from `DCENT_OS_DEVELOPMENT_KITRE2/.../WORKSPACES/firmware_work/porting/`.
//!
//! Cross-reference:
//! implementation candidate IC-3 ("Pin T15 NAND write offsets as constants").

pub mod nand;
