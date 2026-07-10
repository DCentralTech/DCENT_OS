//! BM1387 protocol reference (S9, S17, T9+, S11; 16nm).
//!
//! W11.10 (DCENT_OS Dev Kit Integration, 2026-05-09): codifies the BM1387
//! UART/serial command parameters from the RE2  §8.1
//! (BM1387 Protocol) and `DCENT_OS_HARDWARE_CATALOG.md` §4.1 (chip family
//! catalog) and §7.1 (protocol matrix). This module is **reference-only**.
//! The live BM1387 driver lives at `drivers/bm1387.rs` and the on-fleet
//! am1-s9 sustained-mining path is sustained-validated since 2026-04-19
//!. No live-path code is
//! changed here; we only add a top-level reference catalog the way
//! `bm1362.rs` (orchestration) sits next to `drivers/bm1362.rs` (driver).
//!
//! ## Why a separate module instead of growing `drivers/bm1387.rs`?
//!
//! `drivers/bm1387.rs` is the [`crate::drivers::ChipDriver`] implementation
//! — it owns register layouts, frame builders, MiscCtrl/PLL constants, and
//! per-chain state. The reference module here pins the **parameter
//! envelope** (baud range, work_id width, nonce width, CRC type) so that
//! cross-platform code (autotuner, dashboard, FPGA-side work-id math) can
//! depend on the catalog without pulling the entire driver. That mirrors
//! how `bm1393.rs` codifies the BM1393 catalog ahead of any live driver.
//!
//! ## Catalog source
//!
//! - RE2 §8.1 — BM1387 protocol summary: ~63 ASICs/chain on S9, ~162
//!   ASICs/chain on T9+, daisy-chained through FPGA/Altera C5, 32-bit
//!   header (type, address, length) + payload.
//! - RE2 `DCENT_OS_HARDWARE_CATALOG.md` §4.1 — 16nm, 32 cores, custom
//!   3-wire serial / UART, baud 115200-937500, HW CRC, 8-bit work_id,
//!   32-bit nonce, used in S9, S17, T17, S11.
//! - RE2 `DCENT_OS_HARDWARE_CATALOG.md` §7.1 — protocol matrix:
//!   BM1387 uses the legacy custom-3-wire fixed-128-bit framing with HW
//!   CRC, NOT the BM139x-style register-prefix UART frame. The on-fleet
//!   S9 firmware uses the UART path with FPGA-side framing (see
//!   `drivers/bm1387.rs`). The legacy 3-wire fallback is documented here
//!   for reference only — it appears in early S9 control-board firmware
//!   and in datasheet-level documentation.
//!
//! ## Hard rules (NEVER violate when reading from this catalog)
//!
//! - The **on-fleet am1-s9 BM1387 path is load-bearing and live-validated
//!   since 2026-04-19**. Do NOT change anything in `drivers/bm1387.rs`,
//!   `s9_hybrid_mining.rs`, the PIC heartbeat path, or the MiscCtrl
//!   triple-write path (
//!   ). This module only exposes
//!   constants; it does not provide an alternative driver.
//! - BM1387 CMD IDs are `0x04` (GetAddress) and `0x05` (ChainInactive) —
//!   NOT `0x02`/`0x03` (those are BM1397+).
//!   .
//! - BM1387 SETCONFIG header is `0x58` (broadcast) / `0x48` (single chip),
//!   distinct from BM1397+ `0x51`/`0x41` WRITE_ALL/WRITE_SINGLE.
//! - Open-core requires 114 dummy work items with `gate_block=1` in
//!   MiscCtrl; without it, all 114 SHA-256 cores remain blocked and
//!   produce zero nonces.
//! - 8-bit work_id MUST match the FPGA work_id slot —
//!   . Do not store as `u16`.

#![allow(dead_code)]

/// BM1387 chip ID slot (per BM13xx family 16-bit ID convention).
///
/// Already exposed by `drivers::bm1387::CHIP_ID`; re-pinned here for
/// catalog completeness alongside [`crate::bm1393::CHIP_ID`].
pub const CHIP_ID: u16 = 0x1387;

/// BM1387 baud rate envelope, low end (per RE2 §4.1).
///
/// 115200 is the cold-boot/enumeration baud before the FPGA-driven baud
/// upgrade. The on-fleet S9 path uses this for chip enumeration and
/// register configuration before MiscCtrl bumps the chain to fast baud.
pub const BM1387_BAUD_MIN: u32 = 115_200;

/// BM1387 baud rate envelope, high end (per RE2 §4.1).
///
/// 937500 is the BM139x-family fast baud. The on-fleet S9 path actually
/// runs at 1.5625 MHz (FPGA divisor 0x07) after MiscCtrl baud upgrade —
/// see `drivers::bm1387::BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST`. RE2
/// pins the catalog upper bound at 937500; the FPGA-side overclocked
/// baud is a DCENT_OS / BraiinsOS implementation detail.
pub const BM1387_BAUD_MAX: u32 = 937_500;

/// BM1387 baud rate envelope as a `(min, max)` tuple — convenient for
/// catalog table lookups without exporting a struct.
pub const BM1387_BAUD_RANGE: (u32, u32) = (BM1387_BAUD_MIN, BM1387_BAUD_MAX);

/// BM1387 work_id width (per RE2 §4.1). 8 bits — MUST match the FPGA
/// work_id slot.
pub const BM1387_WORK_ID_BITS: u32 = 8;

/// BM1387 nonce width (per RE2 §4.1). 32 bits.
pub const BM1387_NONCE_BITS: u32 = 32;

/// BM1387 CRC type per RE2 §7.1.
///
/// BM1387 is the only chip in the catalog documented as "HW CRC" rather
/// than CRC8. The host-side BM1387 wire framing uses CRC5 over the 5-byte
/// command and CRC16 over the 12-word job (`crate::protocol::crc5`,
/// `crate::protocol::crc16`). The "HW CRC" label refers to the
/// chip-internal CRC engine that validates frames at the ASIC's serial
/// receiver — distinct from the host-computed CRC5/CRC16 used to build
/// frames on the FPGA side.
pub const BM1387_CRC_KIND: BmCrcKind = BmCrcKind::HardwareCrc;

/// BM1387 cores per chip, on the canonical 16nm die (per RE2 §4.1 and
/// AMTC test jig `Config.ini`).
///
/// BM1387 = 114 cores (32 columns × ~3.5 rows averaged, see AMTC for
/// exact layout). BM1387P = 128 cores (S9+ variant). The on-fleet
/// `drivers::bm1387` codepath uses 114 in the open-core dummy-work
/// dispatch.
pub const BM1387_CORES_PER_CHIP: u16 = 114;

/// BM1387P (S9+ variant) cores per chip — newer 16nm die revision per
/// AMTC test jig data.
pub const BM1387P_CORES_PER_CHIP: u16 = 128;

/// CRC type marker enum for the BM13xx catalog. Kept lightweight and
/// `Copy` so it can sit in `const` slots without touching the existing
/// driver crates. Not used at runtime today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmCrcKind {
    /// Chip-internal HW CRC (BM1387). Host computes CRC5/CRC16 on the
    /// FPGA side; the ASIC validates internally.
    HardwareCrc,
    /// Software CRC8, polynomial pinned at the chip-driver level.
    /// (BM139x family — see `crate::bm1393::BM1393_CRC8_POLY_TBD`.)
    Crc8,
}

/// BM1387 legacy 3-wire serial fallback (per RE2 §7.1 protocol matrix).
///
/// Early S9 control-board firmware and datasheet-level documentation
/// describe a custom 3-wire serial path with fixed 128-bit frames and
/// HW CRC, distinct from the UART-over-FPGA path used by the on-fleet
/// DCENT_OS S9 driver. We do **not** implement this fallback in
/// DCENT_OS — the FPGA UART path is the production path. This module
/// pins the constant as a documentation marker only.
///
/// If a future repair scenario needs the 3-wire fallback (e.g. driving
/// a hash board from a non-Zynq host without an FPGA), this constant is
/// the catalog entry point. Implementation work would live in a new
/// `drivers/bm1387_legacy_3wire.rs` and would be sealed-trait-gated at
/// platform startup.
pub const BM1387_LEGACY_3WIRE_FRAME_BITS: u16 = 128;

#[cfg(test)]
mod tests {
    use super::*;

    /// Catalog baud range includes the BM139x-family fast baud.
    #[test]
    fn baud_range_includes_937500() {
        let (lo, hi) = BM1387_BAUD_RANGE;
        assert_eq!(lo, 115_200, "low end is the cold-boot enumeration baud");
        assert_eq!(hi, 937_500, "high end is the BM139x-family fast baud");
        assert!(
            (lo..=hi).contains(&937_500),
            "fast baud 937500 must be inside the documented envelope"
        );
        assert!(
            (lo..=hi).contains(&115_200),
            "enumeration baud 115200 must be inside the documented envelope"
        );
    }

    /// 8-bit work_id (regression pin for ).
    #[test]
    fn work_id_is_8_bits() {
        assert_eq!(BM1387_WORK_ID_BITS, 8);
        assert_eq!(1u32 << BM1387_WORK_ID_BITS, 256);
    }

    /// 32-bit nonce — same envelope as BM1393 (BM1362/1366 are the 64-bit
    /// outliers).
    #[test]
    fn nonce_is_32_bits() {
        assert_eq!(BM1387_NONCE_BITS, 32);
    }

    /// Chip ID slot.
    #[test]
    fn chip_id_is_bm1387() {
        assert_eq!(CHIP_ID, 0x1387);
    }

    /// CRC kind is HW CRC (chip-internal), distinct from BM139x CRC8.
    #[test]
    fn crc_kind_is_hardware_crc() {
        assert_eq!(BM1387_CRC_KIND, BmCrcKind::HardwareCrc);
        assert_ne!(BM1387_CRC_KIND, BmCrcKind::Crc8);
    }

    /// Cores per chip — pinned for catalog use; the live driver value at
    /// `drivers::bm1387` already uses 114 for open-core dummy-work.
    #[test]
    fn cores_per_chip_is_114() {
        assert_eq!(BM1387_CORES_PER_CHIP, 114);
        assert_eq!(BM1387P_CORES_PER_CHIP, 128);
        assert!(BM1387P_CORES_PER_CHIP > BM1387_CORES_PER_CHIP);
    }

    /// Legacy 3-wire fallback frame is documented as 128 bits — reference
    /// constant only, not implemented in DCENT_OS.
    #[test]
    fn legacy_3wire_frame_is_128_bits() {
        assert_eq!(BM1387_LEGACY_3WIRE_FRAME_BITS, 128);
    }

    /// PR-054 — BM1391 / BM1393 vs BM1387 register-level disambiguation.
    ///
    /// These pins encode the §1 verdict of
    /// :
    /// the S9-family refinement chips (S9 SE/BM1391, S9j-S9k/BM1393, T15,
    /// S11) are **register-compatible with BM1387** — same `0x1387` chip
    /// ID over GetAddress, same BM1387 command-header / register / HW-CRC
    /// surface, shared `drivers::bm1387` path. Closing the
    /// "assumed-identical-to-BM1387" caveat with corpus evidence is itself
    /// the deliverable; this module pins the contract so a future refactor
    /// can't silently regress it.
    ///
    /// Provenance for every assertion is in the doc §6 citations index;
    /// the load-bearing one is
    /// :384-398`, where the
    /// canonical BraiinsOS driver recognizes exactly one S9-family
    /// `ChipRev` — `Bm1387 = 0x1387` — with no BM1391/BM1393 value.
    mod pr054_s9_family_disambiguation {
        /// The S9-family runtime chip ID is `0x1387` for *all* S9-family
        /// silicon (S9 / S9i / S9j / S9 SE / S9k / T15 / S11). Mirrors
        /// `braiins_bm1387.rs:387` (`Bm1387 = 0x1387` is the only
        /// `ChipRev`) and `drivers::bm1387::CHIP_ID`. BM1391/BM1393 do NOT
        /// get a distinct runtime chip ID — the caveat is closed by
        /// confirmed register-compatibility, not by a new ID.
        #[test]
        fn s9_family_runtime_chip_id_is_0x1387() {
            assert_eq!(
                crate::drivers::bm1387::CHIP_ID,
                0x1387,
                "S9-family runtime keys on 0x1387 only; BM1391/BM1393 are \
                 register-compatible refinements, NOT distinct runtime IDs \
                 (see 2026-05-16-bm1391-bm1393-vs-bm1387-disambiguation.md §1)"
            );
            // The reference-catalog CHIP_ID for this module is also 0x1387.
            assert_eq!(super::super::CHIP_ID, 0x1387);
        }

        /// BM1387 (and therefore the register-compatible BM1391/BM1393)
        /// use the chip-internal hardware CRC, NOT a software CRC8. The
        /// W11.10 `asics.rs` BM1391/BM1393 catalog rows currently encode
        /// `Crc8` (documented over-differentiation, doc §3/§5); the
        /// runtime/reference contract here is `HardwareCrc`.
        #[test]
        fn s9_family_crc_is_hardware_crc_not_crc8() {
            assert_eq!(
                super::super::BM1387_CRC_KIND,
                super::super::BmCrcKind::HardwareCrc
            );
            assert_ne!(super::super::BM1387_CRC_KIND, super::super::BmCrcKind::Crc8);
        }

        /// The W11.10 reference-only `bm1393` catalog constant
        /// `CHIP_ID = 0x1393` is an RE2 "family 16-bit ID convention"
        /// marker — it is explicitly NOT a runtime registry key
        /// (`bm1393.rs:170-174`, `lib.rs:21`). Pin that it stays distinct
        /// from the runtime S9-family ID so a future "cleanup" can't
        /// silently promote `0x1393` into driver dispatch (which would
        /// re-open the caveat in the wrong direction).
        #[test]
        fn bm1393_() {
            assert_eq!(crate::bm1393::CHIP_ID, 0x1393);
            assert_ne!(
                crate::bm1393::CHIP_ID,
                crate::drivers::bm1387::CHIP_ID,
                "bm1393::CHIP_ID is a reference-only RE2 catalog marker; the \
                 S9-family runtime ID is drivers::bm1387::CHIP_ID (0x1387). \
                 Do NOT wire 0x1393 into drivers::mod::for_chip dispatch — \
                 see 2026-05-16-bm1391-bm1393-vs-bm1387-disambiguation.md §2/§5"
            );
        }

        /// The W11.10 BM1393 CRC8 polynomial is an explicit unwired
        /// placeholder (`bm1393.rs:149-166`, "TBD per RE2 R3 … do not rely
        /// on the value at runtime"). It is NOT a confirmed BM1393 delta
        /// vs BM1387. Pin that it stays the documented `0x00`/TBD sentinel
        /// so nobody hardcodes a value and mistakes it for a real
        /// disambiguation result.
        #[test]
        fn bm1393_crc8_poly_remains_explicit_tbd_placeholder() {
            assert_eq!(
                crate::bm1393::BM1393_CRC8_POLY_TBD,
                0x00,
                "BM1393 CRC8 poly is an unwired RE2-R3 placeholder, not a \
                 live-confirmed BM1387 delta; pinning a value here requires \
                 hardware evidence per the disambiguation doc §3 UNKNOWN list"
            );
        }

        /// F-3 (Sweep-v3 PR-080) — chain-enumeration-boundary contract.
        ///
        /// `chain.rs` rejects any GetAddress chip-id not in its
        /// (function-local) `KNOWN_CHIP_IDS` allowlist as UART noise.
        /// That allowlist is, by contract, exactly the set the
        /// production `ChipRegistry` can drive. PR-054 closes the
        /// BM1391/BM1393 caveat by register-compatibility (they enumerate
        /// as `0x1387`), NOT by adding distinct runtime IDs — so
        /// `0x1391`/`0x1393` MUST stay undriveable (hence correctly
        /// rejected-as-noise at the chain boundary) while `0x1387` stays
        /// driveable. The Sweep-v3 chip-survey audit flagged that this
        /// "they report 0x1387" contract was doc-asserted but NOT
        /// enforced at the enum boundary; this pins the enforceable half
        /// (KNOWN_CHIP_IDS is a function-local const and cannot be
        /// asserted directly, but the registry is the contract behind
        /// it). A future edit that adds a 0x1391/0x1393 driver — and thus
        /// would let chain.rs's allowlist accept them — fails here.
        #[test]
        fn chain_enum_boundary_never_drives_bm1391_or_bm1393() {
            let reg = crate::drivers::ChipRegistry::production();
            assert!(
                reg.detect(0x1387).is_some(),
                "0x1387 must remain driveable (chain.rs accepts it)"
            );
            assert!(
                reg.detect(0x1391).is_none(),
                "0x1391 must stay undriveable — BM1391 enumerates as \
                 0x1387 (PR-054 register-compat); chain.rs must reject a \
                 raw 0x1391 as noise, NOT mis-accept it"
            );
            assert!(
                reg.detect(0x1393).is_none(),
                "0x1393 must stay undriveable — see the bm1393 \
                 reference-only catalog pin above; do NOT add it to \
                 driver dispatch (re-opens the caveat in the wrong way)"
            );
        }
    }
}
