//! BM1491 silicon characterization table â€” **NAMED ONLY** (Wave 7 W7-A).
//!
//! `[GAP]` markers throughout: every numeric field below is `Reconstructed`
//! placeholder pending live-unit acquisition. BM1491 is the **most
//! universal** of the wave-6-surfaced new chip names â€” it appears in
//! **64/64** unique decoded VNish cgminer ELFs (vs BM1360's 36/64).
//! Universality across binaries from ALL platforms (S19/S19j/S19jPro/
//! S19kPro/S19xp/S21/S21Pro/S21Plus/S21XP/T19/T21/L7/L9) suggests
//! BM1491 is a reserved chip-name slot in the VNish chip-name enum
//! shared across all builds, NOT a chip exclusive to one product line.
//!
//! ## Source provenance (what we actually know)
//!
//! - **L7 1.2.7 `usr/bin/hwscan`** ELF (Rust, statically linked,
//!   stripped): raw ASCII strings dump contains the literal chip-name
//!   array `[BM1360, BM1362, BM1366, BM1368, BM1370, BM1398, BM1489,
//!   BM1491, UNK]`. BM1491 immediately follows BM1489 in the array.
//!   File:
//!   (BM1491 string near offset 0x4165f4; surrounding window proves
//!   it's a discriminator enum entry, NOT a driver block).
//! - **92 VNish cgminer ELFs decoded by wave-6 Phase C** (per-string
//!   XOR): BM1491 surfaces in **64/64 unique-binary lists** â€” universal
//!   coverage. Cite:
//!   :14`
//!   plus `_decoded_strings/cgminer.l9.1.2.6-rc5.aml.vnish.0b4556b6.txt:22945`
//!   (BM1491 follows BM1489 in the enum block).
//! - **No `0x1491` literal** in any decoded binary; the chip-name
//!   strings are the only evidence of BM1491's existence.
//!
//! ## What we DO NOT know (HONEST gaps)
//!
//! - Numeric chip_id byte pair (no `0x1491` in any binary).
//! - cores_per_chip â€” no `BM1491_CORE_NUM` literal anywhere.
//! - Default freq / voltage / wattage â€” runtime-derived, not baked.
//! - PLL / MiscCtrl / TicketMask register addresses â€” sealed.
//! - Host platform â€” speculation only. Two competing hypotheses:
//!   (a) BM1489 successor for next-gen Scrypt L7/L9 (because BM1491
//!       sits adjacent to BM1489 in the chip-name array), or
//!   (b) Reserved enum slot used by VNish across all builds for
//!       future-chip placeholder (because it appears universally even
//!       in pure SHA-256 binaries like S19/S21/T19/T21).
//!   Hypothesis (b) is currently better-supported by the 64/64
//!   universality.
//! - Mining algorithm â€” Scrypt OR SHA-256 (unknown given universality).
//! - Hashrate per chip â€” unknown.
//!
//! ## + resolution path
//!
//! 1. **VNish 1.2.8+ firmware leak** â€” most likely path; if BM1491
//!    is a VNish forward-looking placeholder, it'll surface in plain
//!    text once Bitmain ships actual silicon.
//! 2. **L9 next-gen unit acquisition** â€” confirms hypothesis (a).
//! 3. **Vendor datasheet RE** â€” Bitmain BM1491 datasheet.
//!
//! Until then, this file ships as a Reconstructed-only stub so the
//! autotuner does NOT silently fall through to BM1489 defaults if
//! BM1491 is detected at runtime.
//!
//! ## Relationship to silicon profile registry
//!
//! `BM1491_TABLE` is exported as a `SiliconTable` keyed by
//! `chip_family = "BM1491"` so the registry can route a BM1491 chip
//! detection to a placeholder profile rather than crashing or
//! returning `BM1489` as a silent fallback.
//!
//! Cross-references:
//! -  Â§10.1
//! -  Â§5.2
//! -  (memory rule)

use crate::{Profile, ProfileSource, SiliconTable};

/// Placeholder BM1491 silicon profile rows. **EVERY ROW IS A GAP** â€”
/// values are NOT vendor-extracted or live-confirmed. They exist to
/// give the registry a deterministic shape so an unknown-chip detection
/// is routed to a refuse-with-diagnostic path.
///
/// The profile shape leaves open both the SHA-256 (TH/s) and Scrypt
/// (MH/s Ã— 1e-6 in `hashrate_ths`) interpretations. Until algorithm
/// is confirmed, all rows are zeroed.
pub const BM1491_PROFILES: [Profile; 3] = [
    Profile {
        step: -1,
        // [GAP] If Scrypt L9-class: ~400 MHz eco. If SHA-256 next-gen:
        // ~600 MHz eco. Verify on first live capture.
        freq_mhz: 400,
        // [GAP] If Scrypt: chain-rail ~12V. If SHA-256 chip-rail:
        // ~1.10V. Voltage axis genuinely unknown.
        voltage_v: 1.10,
        // [GAP]
        wall_watts: Some(0),
        // [GAP]
        hashrate_ths: Some(0.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        // [GAP] Default nominal placeholder.
        freq_mhz: 525,
        // [GAP]
        voltage_v: 1.20,
        // [GAP]
        wall_watts: Some(0),
        // [GAP]
        hashrate_ths: Some(0.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 1,
        // [GAP]
        freq_mhz: 650,
        // [GAP]
        voltage_v: 1.30,
        // [GAP]
        wall_watts: Some(0),
        // [GAP]
        hashrate_ths: Some(0.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1491 silicon table.
///
/// **Status: NAMED ONLY** â€” every row is a `Reconstructed` placeholder.
/// Consumers MUST check `BM1491_TABLE.profiles[i].source` before
/// relying on numeric values; see module docs for gap inventory.
pub const BM1491_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1491",
    profiles: &BM1491_PROFILES,
    default_step: 0,
    sweet_spot_step: -1,
    // W7.4 (2026-05-07): chip name appears in 64/64 decoded VNish
    // cgminer ELFs but no register addresses surfaced (XOR-sealed in
    // freq tables). Best-supported hypothesis: BM1489 successor
    // (Scrypt-family — sits adjacent to BM1489 in the chip-name
    // array). Algorithm flagged Scrypt for routing purposes; refuse
    // mining by default; lab unlock via `experimental_chips`.
    live_status: crate::ChipStatus::RegisterMappedFromRE,
};

/// Cores per BM1491 chip â€” **UNKNOWN** ([GAP]). Set to `0` so any
/// hashrate-projection consumer that multiplies by this constant
/// produces zero rather than a plausible-but-fake number. The
/// autotuner / driver dispatch path MUST refuse to mine when this
/// is `0`.
pub const BM1491_CORES_PER_CHIP: u32 = 0;

/// Whether any BM1491 row in the table is live-confirmed. Always
/// `false` until wave 8+ delivers live data.
pub const BM1491_HAS_LIVE_DATA: bool = false;

/// Whether BM1491 is confirmed Scrypt or SHA-256. **UNKNOWN** until
/// live capture. Tracked separately because the chip-name's
/// 64/64-binary universality (including pure SHA-256 binaries like
/// S19, S21, T19, T21) makes the Scrypt-only assumption load-bearing
/// risky. Two competing hypotheses logged in module docs.
pub const BM1491_ALGORITHM_CONFIRMED: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_three_placeholder_rows() {
        assert_eq!(BM1491_TABLE.profiles.len(), 3);
        assert_eq!(BM1491_TABLE.min_step(), -1);
        assert_eq!(BM1491_TABLE.max_step(), 1);
    }

    #[test]
    fn every_row_is_reconstructed_placeholder() {
        for p in BM1491_PROFILES.iter() {
            assert_eq!(
                p.source,
                ProfileSource::Reconstructed,
                "BM1491 step {} must remain Reconstructed (NAMED ONLY chip)",
                p.step
            );
        }
        assert!(!BM1491_HAS_LIVE_DATA);
    }

    #[test]
    fn cores_per_chip_is_zero_until_live_capture() {
        assert_eq!(BM1491_CORES_PER_CHIP, 0);
    }

    #[test]
    fn algorithm_unconfirmed_until_live_capture() {
        // Hard rule: until a live BM1491 unit is probed, treat algorithm
        // as unknown. Universal 64/64-binary appearance does not pin
        // it to Scrypt OR SHA-256.
        assert!(!BM1491_ALGORITHM_CONFIRMED);
    }

    #[test]
    fn chip_family_label_matches_genealogy_bible() {
        assert_eq!(BM1491_TABLE.chip_family, "BM1491");
    }

    #[test]
    fn watts_are_zero_so_efficiency_is_undefined() {
        for p in BM1491_PROFILES.iter() {
            assert!(
                p.watts_per_ths().is_none(),
                "BM1491 step {} efficiency must remain undefined",
                p.step
            );
        }
    }
}
