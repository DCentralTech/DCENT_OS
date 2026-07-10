//! BM1360 silicon characterization table â€” **NAMED ONLY** (Wave 7 W7-A).
//!
//! `[GAP]` markers throughout: every numeric field below is `Reconstructed`
//! placeholder pending live-unit acquisition or a 1.2.8+ VNish firmware
//! that exposes BM1360 register addresses in plaintext.
//!
//! ## Source provenance (what we actually know)
//!
//! - **L7 1.2.7 `usr/bin/hwscan`** ELF (Rust, statically linked, stripped):
//!   raw ASCII strings dump contains the literal chip-name array
//!   `[BM1360, BM1362, BM1366, BM1368, BM1370, BM1398, BM1489, BM1491, UNK]`
//!   followed by JSON config field discriminators (`num_cores`,
//!   `default_freq`, `default_volt`, `pulse_mode`, `pulse_width`,
//!   `ver_roll_mask`, `ticket_mask`, `uart_speed`, `io_drive_strength`,
//!   `chip_addr_interval`, `num_chips`, `num_chips_per_domain`,
//!   `num_domains`, `startup_freq`, `startup_volt`, `presets`).
//!   File:
//!   (BM1360 string at file offset 0x4165e1; surrounding window proves
//!   it's a discriminator enum, NOT a chip-driver block).
//! - **92 VNish cgminer ELFs decoded by wave-6 Phase C** (per-string XOR):
//!   BM1360 surfaces in **36/64 unique-binary lists** as part of the
//!   same chip-name array (see
//!   :7`
//!   and `_decoded_strings/cgminer.l7.1.2.6-rc5.unknown.vnish.6491bb6f.txt:23305`).
//!
//! ## What we DO NOT know (HONEST gaps)
//!
//! - Numeric chip_id byte pair (no `0x1360` literal in ANY decoded
//!   binary; chip_init.rs entry will use `[0x13, 0x60]` as a placeholder
//!   based on the chip-name pattern, not a verified bus capture).
//! - cores_per_chip â€” no `BM1360_CORE_NUM` literal anywhere.
//! - Default freq / voltage / wattage â€” runtime-derived from
//!   `/etc/factory/cgminer.conf` + `levels.json`, NOT baked.
//! - PLL / MiscCtrl / TicketMask register addresses â€” sealed by the
//!   XOR-encoded freq tables inside cgminer (per W6-Phase C verdict).
//! - Host platform â€” speculation (BM1366 predecessor for
//!   BitAxe-Touch / home-class) based on consecutive enum position in
//!   the chip-name array adjacent to BM1362; NOT confirmed.
//! - Hashrate per chip â€” unknown.
//!
//! ## + resolution path
//!
//! 1. **Live unit acquisition** â€” BitAxe Touch (if it ships with BM1360)
//!    OR newer Bitmain home-line model.
//! 2. **VNish 1.2.8+ firmware leak** â€” may expose BM1360 in plaintext
//!    `levels.json` or per-chip `init_*` arrays.
//! 3. **Vendor datasheet RE** â€” Bitmain BM1360 datasheet.
//!
//! Until any of those land, this file ships as a Reconstructed-only
//! stub with placeholder rows so the autotuner does NOT silently fall
//! through to BM1366 defaults if BM1360 is detected at runtime.
//!
//! ## Relationship to silicon profile registry
//!
//! `BM1360_TABLE` is exported as a `SiliconTable` keyed by
//! `chip_family = "BM1360"` so the registry can route a BM1360 chip
//! detection to a placeholder profile rather than crashing or
//! returning `BM1366` as a silent fallback. Per
//!  style, an unknown chip should
//! refuse mining and surface a diagnostic, not fake-it-til-you-make-it.
//!
//! Cross-references:
//! -  Â§10.1
//! -  Â§5.1
//! -  (memory rule)

use crate::{Profile, ProfileSource, SiliconTable};

/// Placeholder BM1360 silicon profile rows. **EVERY ROW IS A GAP** â€”
/// values are NOT vendor-extracted or live-confirmed. They exist to
/// give the registry a deterministic shape so an unknown-chip detection
/// is routed to a refuse-with-diagnostic path, NOT silently mapped to
/// BM1366.
///
/// All `Reconstructed` â€” DO NOT consume these in autotuner / dashboard
/// without first verifying the chip detection path branches on
/// `Profile.source == Reconstructed` and presents a "PLACEHOLDER â€”
/// awaiting live unit" warning.
pub const BM1360_PROFILES: [Profile; 3] = [
    Profile {
        step: -1,
        // [GAP] Frequency placeholder. Speculation: home-class chip
        // would underclock to ~400 MHz for efficiency. Verify on first
        // live BitAxe Touch / BM1360-host capture.
        freq_mhz: 400,
        // [GAP] Voltage placeholder â€” assumed chip-rail (sub-2V) like
        // BM1366/BM1362 since BM1360 sits in the same chip-name enum
        // block as BM1362. Verify on first live capture.
        voltage_v: 1.10,
        // [GAP] Wattage placeholder.
        wall_watts: Some(0),
        // [GAP] Hashrate placeholder.
        hashrate_ths: Some(0.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        // [GAP] Default placeholder â€” assumed home-class nominal.
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
        // [GAP] Overclock placeholder.
        freq_mhz: 600,
        // [GAP]
        voltage_v: 1.30,
        // [GAP]
        wall_watts: Some(0),
        // [GAP]
        hashrate_ths: Some(0.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1360 silicon table.
///
/// **Status: NAMED ONLY** â€” every row is a `Reconstructed` placeholder.
/// Consumers MUST check `BM1360_TABLE.profiles[i].source` before relying
/// on numeric values; see module docs for the gap inventory.
pub const BM1360_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1360",
    profiles: &BM1360_PROFILES,
    default_step: 0,
    sweet_spot_step: -1,
    // W7.4 (2026-05-07): register set + PLL formula lifted from
    //  §4 (BM1366+ family — BM1360 sits
    // adjacent to BM1362 in the cgminer chip-name enum and shares
    // the byte-segmented PLL register encoding). No live BM1360 unit
    // on the fleet. Refuse mining by default; lab unlock via
    // `experimental_chips` Cargo feature.
    live_status: crate::ChipStatus::RegisterMappedFromRE,
};

/// Cores per BM1360 chip â€” **UNKNOWN** ([GAP]). Set to `0` so any
/// hashrate-projection consumer that multiplies by this constant
/// produces an obviously-wrong zero rather than a plausible-but-fake
/// number. The autotuner / driver dispatch path MUST refuse to mine
/// when this is `0`.
///
///  resolution: live BitAxe Touch capture or vendor datasheet.
pub const BM1360_CORES_PER_CHIP: u32 = 0;

/// Whether any BM1360 row in the table is live-confirmed. Always
/// `false` until wave 8+ delivers live data.
pub const BM1360_HAS_LIVE_DATA: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_three_placeholder_rows() {
        assert_eq!(BM1360_TABLE.profiles.len(), 3);
        assert_eq!(BM1360_TABLE.min_step(), -1);
        assert_eq!(BM1360_TABLE.max_step(), 1);
    }

    #[test]
    fn every_row_is_reconstructed_placeholder() {
        // Hard rule: NO row in BM1360_PROFILES may claim
        // LiveConfirmed / OperatorConfirmed / VendorExtracted until
        // wave-8+ replaces a row with real data and updates this test.
        for p in BM1360_PROFILES.iter() {
            assert_eq!(
                p.source,
                ProfileSource::Reconstructed,
                "BM1360 step {} must remain Reconstructed (NAMED ONLY chip)",
                p.step
            );
        }
        assert!(!BM1360_HAS_LIVE_DATA);
    }

    #[test]
    fn cores_per_chip_is_zero_until_live_capture() {
        // Forcing this to 0 makes hashrate projections obviously wrong
        // rather than fake-plausible. Replace ONLY when live BitAxe
        // Touch (or other BM1360 host) capture lands.
        assert_eq!(BM1360_CORES_PER_CHIP, 0);
    }

    #[test]
    fn chip_family_label_matches_genealogy_bible() {
        // Stable label for downstream consumers (registry, dashboard,
        // toolbox `dcent-toolbox` chip dispatch). Synced with
        //  Â§5.1.
        assert_eq!(BM1360_TABLE.chip_family, "BM1360");
    }

    #[test]
    fn watts_are_zero_so_efficiency_is_undefined() {
        // Every row has wall_watts=0 + hashrate_ths=0; watts_per_ths
        // returns None. Sanity: no consumer can compute a fake
        // efficiency from BM1360 placeholder data.
        for p in BM1360_PROFILES.iter() {
            assert!(
                p.watts_per_ths().is_none(),
                "BM1360 step {} efficiency must remain undefined",
                p.step
            );
        }
    }
}
