//! 9 (2026-05-09): ASIC chip catalog spanning the full Antminer line.
//!
//! Source-cite: `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/`
//! `DCENT_OS_HARDWARE_CATALOG.md` §4 (lines 381-456) and §7 (lines 565-588).
//!
//! This module is the single source of truth for chip-family metadata that
//! does NOT live inside a per-chip `bm13xx.rs` silicon-profile module.
//! Per-SKU geometry, freq/voltage tables, and PLL formulas remain in the
//! richer per-chip modules (e.g. `bm1362.rs`); this module is the lightweight
//! catalog used by registry consumers, dashboard surfaces, and platform-
//! routing code that only needs to ask "what chip is this and how does it
//! talk?".
//!
//! The W11.5 `bm1362.rs` profile is re-exported via `Bm1362::PROFILE_MODULE`
//! so callers that want the deeper per-SKU tables don't have to know which
//! crate-level path to import.

use serde::{Deserialize, Serialize};

/// Process-node families seen across the Bitmain ASIC line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessNode {
    /// 16 nm — BM1387 only (S9 / S9i / T9). CORRECTED 2026-07-02: S11/S15 use
    /// the 7nm BM1391 and S17/T17 use the 7nm BM1397 — all classified `Nm7`
    /// below (the chip→node DATA map is already correct; this doc previously
    /// mis-listed those SKUs here as a BM1387-template copy-paste).
    Nm16,
    /// 7 nm — BM1391 (S11/S15/T15) / BM1393 / BM1396 / BM1397
    /// (S17/T17) / BM1398 (S19/S19 Pro/T19).
    Nm7,
    /// 5 nm — BM1362 / BM1366 / BM1368.
    Nm5,
    /// 3 nm — BM1370 (S21 Pro / S21 XP / S21 XP-Hydro). Bleeding-edge
    /// BM137x family. Matches `dcentrald-api-types::MinerModel::AntminerS21Pro`
    /// ("BM1370 — 3 nm") and the `bm1370.rs` silicon profile
    /// ("BM1370 (3nm process)"). A11 (goldmine 2026-06-10).
    Nm3,
}

impl ProcessNode {
    /// Numeric process-node value in nanometers.
    pub const fn nm(self) -> u8 {
        match self {
            ProcessNode::Nm16 => 16,
            ProcessNode::Nm7 => 7,
            ProcessNode::Nm5 => 5,
            ProcessNode::Nm3 => 3,
        }
    }
}

/// Wire-protocol family — how the host sends commands and receives nonces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireInterface {
    /// Custom 3-wire serial (BM1387 over FPGA bridge — distinct from any
    /// standard async UART). RE2 §4.1 line 387: "Custom 3-wire serial /
    /// UART".
    Custom3Wire,
    /// Standard asynchronous UART (BM139x / BM136x). 8N1 framing.
    Uart,
    /// SPI or UART (BM1368 supports both; S21 stock uses SPI on the
    /// Amlogic control board, UART on Zynq variants).
    SpiOrUart,
}

/// CRC algorithm used to validate command framing.
///
/// Notes:
/// - **HwCrc**: BM1387's hardware-computed CRC inside the chip's UART
///   controller — host firmware doesn't compute it explicitly.
/// - **Crc8** (no specified poly): legacy BM139x catalog rows that don't
///   pin a polynomial in RE2; left as `Crc8` here pending live capture.
/// - **Crc8Poly31**: BM1362 + BM1368 share `0x31` per RE2 §4.1 line 394
///   ("CRC8 poly 0x31").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrcAlgorithm {
    HwCrc,
    Crc8,
    Crc8Poly31,
}

/// Bit-width of a chip-side `work_id` field.
///
/// Important reconciliation note: BM1362's *FPGA dispatch* `work_id` is
/// 8-bit, but its *ASIC-side*
/// `work_id` carried in WORK_RX is 16-bit per RE2 §4.2 line 430. Both
/// numbers are real and live at different layers of the hash pipeline.
/// This enum represents the **chip-side** width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkIdWidth {
    Bits8,
    Bits16,
}

impl WorkIdWidth {
    pub const fn bits(self) -> u8 {
        match self {
            WorkIdWidth::Bits8 => 8,
            WorkIdWidth::Bits16 => 16,
        }
    }
}

/// Single ASIC chip catalog entry — light metadata only. Per-chip
/// silicon-characterization tables (freq/voltage, PLL formulas, per-SKU
/// hashboard geometry) live in dedicated `bm13xx.rs` modules.
///
/// Note: `Deserialize` is intentionally NOT derived — `used_in` is a
/// `&'static [&'static str]` borrowed slice, which serde can't borrow
/// during a deserialize round-trip. The catalog is meant to be a
/// compile-time const table; callers serialize it for telemetry but
/// don't need to deserialize it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AsicCatalogEntry {
    /// Canonical chip name as it appears in `cgminer-API` / vendor docs.
    pub name: &'static str,
    /// Process node.
    pub process: ProcessNode,
    /// Wire interface family.
    pub interface: WireInterface,
    /// Minimum baud rate the chip will accept on cold boot. Most BM139x+
    /// chips boot at 115200, BM1387 also boots at 115200; baud rate is
    /// then upgraded post-enumeration to the operating value
    /// (`baud_max`).
    pub baud_min: u32,
    /// Maximum (post-upgrade) baud rate.
    pub baud_max: u32,
    /// CRC algorithm guarding command framing.
    pub crc: CrcAlgorithm,
    /// Bit-width of the chip-side `work_id` field.
    pub work_id_width: WorkIdWidth,
    /// Bit-width of the nonce field returned in WORK_RX.
    pub nonce_bits: u8,
    /// Cores per chip (when known from RE2). 0 means catalog-only entry
    /// (legacy chip, BM1387 = 32 small cores; modern chips encode this
    /// inside the per-chip silicon profile module).
    pub cores: u16,
    /// Antminer models that ship this chip. Free-form for now; future
    /// waves can replace with a `MinerModel` enum slice.
    pub used_in: &'static [&'static str],
}

/// Top-level chip enum used to look up a catalog entry. Mirrors
/// `dcentrald-api-types::ChipFamily` but lives here so callers can stay
/// HAL-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsicChip {
    /// BM1387 — S9 / S9i / S9 SE / T9 / T9+ / S11 / S15 / S17 / T17.
    /// 16 nm, 32 cores, custom 3-wire / UART, hardware CRC.
    Bm1387,
    /// BM1387_54 — S11 voltage-table variant. Same silicon family as
    /// BM1387 but the S11 levels.json table assigns 54 active cores
    /// instead of 32 (silicon-binning aggressively at the high end).
    Bm1387_54,
    /// BM1391 — T15 / S11 (7 nm).
    Bm1391,
    /// BM1393 — S9k (7 nm). Dual-arch S9k pairs Zynq with a BM1880
    /// RISC-V coprocessor; from the chip's perspective it's still a
    /// standard BM139x family member.
    Bm1393,
    /// BM1396 — S17+ / T17+ (7 nm).
    Bm1396,
    /// BM1397 — S17 / T17 / S17e / T17e (7 nm).
    Bm1397,
    /// BM1398 — S19 / S19 Pro / S19+ / T19-class (7 nm).
    /// Logical ASIC work IDs remain 8-bit; some FPGA carriers extend the echo
    /// with low midstate-slot bits and must not redefine the logical ring.
    Bm1398,
    /// BM1366 — S19j / S19 variants / S19k Pro (5 nm).
    /// **EEPROM preamble is `0x05 0x11`** on BHB56902 (S19k Pro)
    /// hashboards, distinct from the `0x04 0x11` BHB42xxx family — see
    ///  and
    /// `hashboards::Bhb56902`.
    Bm1366,
    /// BM1368 — S21 / S21 Pro / S21 XP (5 nm). Packet-based protocol
    /// with 19 commands (RE2 §4.3).
    Bm1368,
    /// BM1362 — S19j Pro (Zynq am2 / Amlogic am3-aml / CV1835 / BB
    /// AM335x). The reference live-driven chip; full per-SKU geometry
    /// + PLL formula in `bm1362.rs`.
    Bm1362,
    /// BM1370 — S21 Pro / S21 XP / S21 XP-Hydro (3 nm). Newest BM137x
    /// packet-protocol family; ID `0x1370` is already in
    /// `dcentrald-asic::chain::KNOWN_CHIP_IDS`. Full per-SKU geometry +
    /// init ORDER live in `bm1370.rs` / `dcentrald-asic::drivers::bm1370`.
    /// A11 (goldmine 2026-06-10, findings/s3-bm1370-s21pro.md).
    Bm1370,
}

impl AsicChip {
    /// Catalog row for this chip. Lightweight metadata only; for
    /// per-SKU freq/voltage tables and PLL formulas, see the dedicated
    /// `bm1362::*` / `bm1366::*` / `bm1368::*` modules.
    pub const fn catalog(self) -> AsicCatalogEntry {
        match self {
            AsicChip::Bm1387 => AsicCatalogEntry {
                name: "BM1387",
                process: ProcessNode::Nm16,
                interface: WireInterface::Custom3Wire,
                baud_min: 115_200,
                baud_max: 937_500,
                crc: CrcAlgorithm::HwCrc,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                // 114 cores (AMTC Config.ini; == bm1387::BM1387_CORES_PER_CHIP,
                // the live open-core requirement and diagnostics value). Was 32.
                cores: 114,
                // BM1387 ships in the S9/T9 family ONLY (this file's Nm16 doc +
                // the 2026-07-02 correction). NOT S11 (BM1391), S15, S17 (BM1397,
                // per the BhbS17 hashboards.rs fix), or T17.
                used_in: &["S9", "S9i", "S9 SE", "T9", "T9+"],
            },
            AsicChip::Bm1387_54 => AsicCatalogEntry {
                name: "BM1387_54",
                process: ProcessNode::Nm16,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                baud_max: 937_500,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                // Still a BM1387 (114 cores). The "54" in the name is the S9++
                // 54-chips-per-chain binning variant (AMTC
                // `set_Voltage_S9_plus_plus_BM1387_54`), NOT a core count — it was
                // misread as 54 cores and misattributed to the S11 (BM1391).
                cores: 114,
                used_in: &["S9++"],
            },
            AsicChip::Bm1391 => AsicCatalogEntry {
                name: "BM1391",
                process: ProcessNode::Nm7,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                baud_max: 937_500,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                // `cores` HELD at 0 (unknown until a live S11). A12 (goldmine
                // 2026-06-10): the two RE sources DISAGREE on BM1391 geometry,
                // so neither populates `cores`:
                //   - BINARY (HashSource S11 single-board-test jig,
                //     findings/s1-bm1391-s11.md F08/F10/F42): 84 chips/chain /
                //     114 cores / 930 mV init, BM1387-byte-identical FIL chain
                //     with NO out-of-band model-ID (byte-exact CONFIRMS PR-054).
                //   - AMTC ("BM1390" Config.ini): 60-ASIC / 128-core / 1060 mV.
                // Unresolved until a live S11 unit; do not populate `cores` from
                // either source yet. See findings/s1-bm1391-s11.md +
                //
                cores: 0,
                used_in: &["T15", "S11"],
            },
            AsicChip::Bm1393 => AsicCatalogEntry {
                name: "BM1393",
                process: ProcessNode::Nm7,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                baud_max: 937_500,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                cores: 0,
                used_in: &["S9k"],
            },
            AsicChip::Bm1396 => AsicCatalogEntry {
                name: "BM1396",
                process: ProcessNode::Nm7,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                baud_max: 937_500,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                cores: 0,
                used_in: &["S17+", "T17+"],
            },
            AsicChip::Bm1397 => AsicCatalogEntry {
                name: "BM1397",
                process: ProcessNode::Nm7,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                // BM1397 runs at 6.25 Mbaud post-enum (BM1397_OPERATIONAL_BAUD =
                // 6_250_000); 937_500 was a stale placeholder < the operating baud.
                baud_max: 6_250_000,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                cores: 0,
                used_in: &["S17", "T17", "S17e", "T17e"],
            },
            AsicChip::Bm1398 => AsicCatalogEntry {
                name: "BM1398",
                process: ProcessNode::Nm7,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                // Current production recipes omit the composition-specific
                // PLL3 transition and fail-safe at 3.125 Mbaud.
                baud_max: 3_125_000,
                crc: CrcAlgorithm::Crc8,
                // Logical job ID is one byte. A carrier may echo a 16-bit
                // extended field after appending low midstate-slot bits.
                work_id_width: WorkIdWidth::Bits8,
                nonce_bits: 32,
                cores: 0,
                used_in: &["S19", "S19 Pro", "S19+", "T19"],
            },
            AsicChip::Bm1366 => AsicCatalogEntry {
                name: "BM1366",
                process: ProcessNode::Nm5,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                // RE-confirmed operational baud is 3.125 Mbaud (see
                // BM1366_OPERATIONAL_BAUD = 3_125_000); the old 937_500 contradicted
                // it (catalog max < operating baud).  2026-06-30 host-tested
                // metadata pins BM1366/BM1370 at 3.125 Mbaud.
                baud_max: 3_125_000,
                crc: CrcAlgorithm::Crc8,
                work_id_width: WorkIdWidth::Bits16,
                nonce_bits: 32,
                cores: 0,
                used_in: &["S19j", "S19k Pro", "S19 XP"],
            },
            AsicChip::Bm1368 => AsicCatalogEntry {
                name: "BM1368",
                process: ProcessNode::Nm5,
                interface: WireInterface::SpiOrUart,
                baud_min: 115_200,
                // BM1368 (S21) runs at 3.125 Mbaud (BM1368_OPERATIONAL_BAUD =
                // 3_125_000; ttyS2@3M on the fleet); 937_500 was a stale placeholder
                // < the operating baud.
                baud_max: 3_125_000,
                crc: CrcAlgorithm::Crc8Poly31,
                work_id_width: WorkIdWidth::Bits16,
                nonce_bits: 32,
                cores: 0,
                // F-5 (Sweep-v3 PR-082): "S19 Pro" removed — the S19 Pro
                // is a BM1398 product (see AsicChip::Bm1398 above), NOT
                // BM1368. This was a catalog over-attribution of the same
                // class as the v1/v2 T21=BM1390 / BM1396 corrections.
                // `used_in` is catalog metadata only (grep-confirmed: it
                // is not consumed by ChipID dispatch), so this is a
                // documentation-correctness fix, not a routing change.
                // "S19i" retained — no corpus evidence contradicts it.
                used_in: &["S21", "S21 Pro", "S21 XP", "S19i"],
            },
            AsicChip::Bm1362 => AsicCatalogEntry {
                name: "BM1362",
                process: ProcessNode::Nm5,
                interface: WireInterface::Uart,
                baud_min: 115_200,
                // BM1362 (S19j Pro) runs at 3.125 Mbaud (dcentrald-asic
                // bm1362::OPERATIONAL_BAUD = 3_125_000, the .25/.109 proven path);
                // 937_500 was a stale placeholder < the operating baud.
                baud_max: 3_125_000,
                crc: CrcAlgorithm::Crc8Poly31,
                work_id_width: WorkIdWidth::Bits16,
                nonce_bits: 32,
                // Per `bm1362::chip::FPGA_VISIBLE_BIG_CORES`. Internal
                // die geometry is 65 cores * 514 small-cores; the
                // catalog reports the FPGA-visible big-core count.
                cores: 4,
                used_in: &["S19j Pro", "S19j Pro+", "S19j Pro-A"],
            },
            AsicChip::Bm1370 => AsicCatalogEntry {
                name: "BM1370",
                process: ProcessNode::Nm3,
                // S21 Pro / S21 XP family — same Amlogic-class platform as
                // the BM1368 S21, packet-based protocol. Modeled on the
                // BM1368 arm, EXCEPT the baud ceiling (RE-confirmed higher).
                interface: WireInterface::SpiOrUart,
                baud_min: 115_200,
                // RE-confirmed operational baud is 3.125 Mbaud (see
                // BM1370_OPERATIONAL_BAUD = 3_125_000). The old 937_500 was a stale
                // copy from the BM1368 arm and contradicted the operational const
                // (catalog max < operating baud).  2026-06-30 host-tested
                // metadata pins BM1366/BM1370 at 3.125 Mbaud.
                baud_max: 3_125_000,
                crc: CrcAlgorithm::Crc8Poly31,
                work_id_width: WorkIdWidth::Bits16,
                nonce_bits: 32,
                // Catalog-only (0). Per-SKU die geometry (1280 cores =
                // 80 domains × 16 small per S21 Pro) lives in the bm1370
                // silicon profile + `dcentrald-asic::drivers::bm1370`.
                // findings/s3-bm1370-s21pro.md F60-F61: the jig computes
                // core count at runtime from JSON config, not a hard const.
                cores: 0,
                used_in: &["S21 Pro", "S21 XP", "S21 XP Hydro"],
            },
        }
    }

    /// Canonical chip name (matches the `cgminer-API` chip-id field).
    pub const fn name(self) -> &'static str {
        self.catalog().name
    }
}

/// BM1368 packet command set (RE2 §4.3 lines 438-456). Catalog-only —
/// driver code lives in `dcentrald-asic::drivers::bm1368`. 19 distinct
/// commands.
pub mod bm1368_commands {
    /// Initialize the chip and assign an address.
    pub const CMD_INIT: u8 = 0x01;
    /// Set per-chip core frequency.
    pub const CMD_SET_FREQ: u8 = 0x02;
    /// Set per-chip voltage.
    pub const CMD_SET_VOLT: u8 = 0x03;
    /// Begin mining.
    pub const CMD_START_HASH: u8 = 0x04;
    /// Stop mining.
    pub const CMD_STOP_HASH: u8 = 0x05;
    /// Set chip address (post-enumeration).
    pub const CMD_SET_ADDR: u8 = 0x09;
    /// Set per-chip difficulty.
    pub const CMD_SET_DIFF: u8 = 0x0A;
    /// Read chip version.
    pub const CMD_READ_VER: u8 = 0x0B;
    /// Enter low-power sleep.
    pub const CMD_SLEEP: u8 = 0x0C;
    /// Wake from sleep.
    pub const CMD_WAKE: u8 = 0x0D;
    /// Run on-die self test.
    pub const CMD_SELF_TEST: u8 = 0x0E;
    /// Read on-die temperature (Celsius * 100).
    pub const CMD_GET_TEMP: u8 = 0x10;
    /// Read on-die hashrate counter.
    pub const CMD_GET_HASHRATE: u8 = 0x11;
    /// Read hardware error stats.
    pub const CMD_GET_ERRORS: u8 = 0x12;
    /// Set nonce range.
    pub const CMD_SET_NONCE: u8 = 0x20;
    /// Read chip identity (type=0x68, rev, UID, cores, max freq).
    pub const CMD_GET_CHIP_INFO: u8 = 0x30;

    /// All 19 BM1368 command opcodes, sorted ascending.
    /// (16 listed in RE2 §4.3 explicitly; the remaining 3 — 0x06, 0x07,
    /// 0x08 — are reserved/internal in the dev-kit doc and excluded
    /// from this list to keep the catalog honest. Future RE work can
    /// promote them once meaning is confirmed.)
    pub const ALL_COMMANDS: &[u8] = &[
        CMD_INIT,
        CMD_SET_FREQ,
        CMD_SET_VOLT,
        CMD_START_HASH,
        CMD_STOP_HASH,
        CMD_SET_ADDR,
        CMD_SET_DIFF,
        CMD_READ_VER,
        CMD_SLEEP,
        CMD_WAKE,
        CMD_SELF_TEST,
        CMD_GET_TEMP,
        CMD_GET_HASHRATE,
        CMD_GET_ERRORS,
        CMD_SET_NONCE,
        CMD_GET_CHIP_INFO,
    ];
}

/// Re-export of the W11.5 BM1362 silicon-profile module — present as a
/// pointer so callers using `asics::Bm1362` for lightweight catalog data
/// can also reach the deeper per-SKU tables via the same
/// silicon-profiles crate root.
pub use crate::bm1362 as bm1362_profile;

/// Every chip in the catalog, ordered roughly by silicon generation.
pub const ALL_CHIPS: &[AsicChip] = &[
    AsicChip::Bm1387,
    AsicChip::Bm1387_54,
    AsicChip::Bm1391,
    AsicChip::Bm1393,
    AsicChip::Bm1396,
    AsicChip::Bm1397,
    AsicChip::Bm1398,
    AsicChip::Bm1366,
    AsicChip::Bm1368,
    AsicChip::Bm1362,
    AsicChip::Bm1370,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_chips_present() {
        // Catalog must list all 11 chips (A11 added BM1370 — goldmine
        // 2026-06-10).
        assert_eq!(ALL_CHIPS.len(), 11);
    }

    #[test]
    fn each_chip_has_unique_name() {
        let mut seen = std::collections::HashSet::new();
        for chip in ALL_CHIPS {
            assert!(
                seen.insert(chip.name()),
                "duplicate chip name: {}",
                chip.name()
            );
        }
    }

    #[test]
    fn process_nodes_pinned() {
        // Pin the process-node assignments per RE2 §4.1.
        assert_eq!(AsicChip::Bm1387.catalog().process, ProcessNode::Nm16);
        assert_eq!(AsicChip::Bm1387_54.catalog().process, ProcessNode::Nm16);
        assert_eq!(AsicChip::Bm1391.catalog().process, ProcessNode::Nm7);
        assert_eq!(AsicChip::Bm1393.catalog().process, ProcessNode::Nm7);
        assert_eq!(AsicChip::Bm1397.catalog().process, ProcessNode::Nm7);
        assert_eq!(AsicChip::Bm1396.catalog().process, ProcessNode::Nm7);
        assert_eq!(AsicChip::Bm1398.catalog().process, ProcessNode::Nm7);
        assert_eq!(AsicChip::Bm1362.catalog().process, ProcessNode::Nm5);
        assert_eq!(AsicChip::Bm1366.catalog().process, ProcessNode::Nm5);
        assert_eq!(AsicChip::Bm1368.catalog().process, ProcessNode::Nm5);
        // A11: BM1370 (S21 Pro) is 3 nm.
        assert_eq!(AsicChip::Bm1370.catalog().process, ProcessNode::Nm3);
    }

    #[test]
    fn work_id_width_split_pinned() {
        // Logical ASIC job IDs are 8-bit through BM1398. Do not confuse the
        // BM1398 carrier's 16-bit extended echo (job ID plus low slot bits)
        // with a 16-bit logical ring.
        for chip in [
            AsicChip::Bm1387,
            AsicChip::Bm1387_54,
            AsicChip::Bm1391,
            AsicChip::Bm1393,
            AsicChip::Bm1396,
            AsicChip::Bm1397,
            AsicChip::Bm1398,
        ] {
            assert_eq!(
                chip.catalog().work_id_width,
                WorkIdWidth::Bits8,
                "{} should be 8-bit work_id",
                chip.name()
            );
        }
        for chip in [
            AsicChip::Bm1362,
            AsicChip::Bm1366,
            AsicChip::Bm1368,
            // A11: BM1370 joins the modern 16-bit work_id family.
            AsicChip::Bm1370,
        ] {
            assert_eq!(
                chip.catalog().work_id_width,
                WorkIdWidth::Bits16,
                "{} should be 16-bit work_id",
                chip.name()
            );
        }
    }

    #[test]
    fn bm1362_and_bm1368_share_crc8_poly_31() {
        // RE2 §4.1 + §4.3 line 436: both BM1362 and BM1368 specify CRC8
        // polynomial 0x31.
        assert_eq!(AsicChip::Bm1362.catalog().crc, CrcAlgorithm::Crc8Poly31);
        assert_eq!(AsicChip::Bm1368.catalog().crc, CrcAlgorithm::Crc8Poly31);
    }

    #[test]
    fn bm1387_uses_hardware_crc() {
        assert_eq!(AsicChip::Bm1387.catalog().crc, CrcAlgorithm::HwCrc);
    }

    #[test]
    fn bm1368_command_set_has_16_published_opcodes() {
        // RE2 §4.3 lists 19 commands of which 16 have published meanings.
        // Pin the ALL_COMMANDS length so a future addition can't silently
        // drift.
        assert_eq!(bm1368_commands::ALL_COMMANDS.len(), 16);
        // Spot-check a couple of opcodes per RE2 line 441-456.
        assert_eq!(bm1368_commands::CMD_INIT, 0x01);
        assert_eq!(bm1368_commands::CMD_SET_FREQ, 0x02);
        assert_eq!(bm1368_commands::CMD_SET_VOLT, 0x03);
        assert_eq!(bm1368_commands::CMD_START_HASH, 0x04);
        assert_eq!(bm1368_commands::CMD_STOP_HASH, 0x05);
        assert_eq!(bm1368_commands::CMD_GET_CHIP_INFO, 0x30);
    }

    #[test]
    fn bm1387_baud_range_pinned() {
        // S9 boots at 115200, then upgrades to 937500 post-enumeration.
        let cat = AsicChip::Bm1387.catalog();
        assert_eq!(cat.baud_min, 115_200);
        assert_eq!(cat.baud_max, 937_500);
    }

    #[test]
    fn all_bm13xx_catalog_baud_max_is_never_below_the_operating_baud() {
        // The catalog baud_max is the chip's supported ceiling; it must NEVER be
        // below the RE-confirmed operational baud the driver actually runs at, or a
        // future consumer that clamps the chain baud to baud_max would down-clock the
        // chain below its operating point and break first-light UART comms. SIX chips
        // — every target-SKU ASIC with an in-code *_OPERATIONAL_BAUD — carried a
        // stale 937_500 (inherited from the BM1368 "Modeled on" arm or copied) that
        // was BELOW the operating baud. This pins the whole class.
        let check = |chip: AsicChip, expected_max: u32, op_baud: u32| {
            let cat = chip.catalog();
            assert_eq!(
                cat.baud_max, expected_max,
                "{} catalog baud_max should be {expected_max}",
                cat.name
            );
            assert!(
                cat.baud_max >= op_baud,
                "{} catalog baud_max {} < operating baud {op_baud}",
                cat.name,
                cat.baud_max
            );
        };
        check(
            AsicChip::Bm1366,
            3_125_000,
            crate::bm1366::BM1366_OPERATIONAL_BAUD,
        ); // S19k Pro
        check(
            AsicChip::Bm1368,
            3_125_000,
            crate::bm1368::BM1368_OPERATIONAL_BAUD,
        ); // S21
        check(
            AsicChip::Bm1370,
            3_125_000,
            crate::bm1370::BM1370_OPERATIONAL_BAUD,
        ); // S21 Pro/XP
        check(
            AsicChip::Bm1397,
            6_250_000,
            crate::bm1397::BM1397_OPERATIONAL_BAUD,
        ); // S17
        check(
            AsicChip::Bm1398,
            3_125_000,
            crate::bm1398::BM1398_OPERATIONAL_BAUD,
        ); // S19 / S19 Pro
           // BM1362 (S19j Pro): its operating baud lives in dcentrald-asic
           // (bm1362::OPERATIONAL_BAUD = 3_125_000, the .25/.109 proven path), a
           // different crate — pin the value directly here.
        check(AsicChip::Bm1362, 3_125_000, 3_125_000);

        // The BM1387 (S9) legitimately caps at 937_500 (boots 115200 -> 937500
        // post-enum), so the ceiling is per-chip, not a blanket bump.
        assert_eq!(AsicChip::Bm1387.catalog().baud_max, 937_500);
    }

    #[test]
    fn bm1366_used_in_s19k_pro() {
        // Per memory rule
        // — BM1366 ships on the S19k Pro. Catalog must reflect.
        let cat = AsicChip::Bm1366.catalog();
        assert!(cat.used_in.iter().any(|m| m.contains("S19k")));
    }

    #[test]
    fn s19_xp_is_bm1366_not_bm1398() {
        let bm1366 = AsicChip::Bm1366.catalog();
        let bm1398 = AsicChip::Bm1398.catalog();

        assert!(bm1366.used_in.iter().any(|m| *m == "S19 XP"));
        assert!(!bm1398.used_in.iter().any(|m| *m == "S19 XP"));
        assert!(bm1398.used_in.iter().any(|m| *m == "S19 Pro"));
        assert!(bm1398.used_in.iter().any(|m| *m == "T19"));
    }

    #[test]
    fn bm1368_does_not_claim_s19_pro() {
        // F-5 (Sweep-v3 PR-082) regression pin: the S19 Pro is a BM1398
        // product, NOT BM1368 — same over-attribution class as the v1/v2
        // T21=BM1390 / BM1396 corrections. BM1368 must NOT claim
        // "S19 Pro" (exact match — "S21 Pro" is legitimately retained
        // and must not be caught by a substring test), and the genuine
        // S21-family entries must survive the edit.
        let cat = AsicChip::Bm1368.catalog();
        assert!(
            !cat.used_in.iter().any(|m| *m == "S19 Pro"),
            "BM1368.used_in must not claim 'S19 Pro' (that is a BM1398 product)"
        );
        assert!(cat.used_in.iter().any(|m| *m == "S21"));
        // The S19 Pro must instead be attributed to BM1398.
        assert!(
            !AsicChip::Bm1398.catalog().used_in.is_empty(),
            "BM1398 catalog entry must exist as the real S19 Pro chip"
        );
    }

    #[test]
    fn metadata_contradictions_reconciled_to_ground_truth() {
        // D4: BM1387 and its S9++ 54-chips-per-chain binning variant BM1387_54
        // (the "54" is chips/chain, NOT cores) are BOTH 114-core BM1387s
        // (== BM1387_CORES_PER_CHIP), differing in wire/CRC/used_in — not cores.
        // Was mis-modeled as 32 vs 54 cores with S11 (BM1391) attribution.
        let std = AsicChip::Bm1387.catalog();
        let v54 = AsicChip::Bm1387_54.catalog();
        assert_eq!(std.process, v54.process);
        assert_eq!(std.cores, 114);
        assert_eq!(v54.cores, 114);
        assert_eq!(crate::bm1387::BM1387_CORES_PER_CHIP, 114);
        assert!(
            !std
                .used_in
                .iter()
                .any(|m| ["S11", "S15", "S17", "T17"].contains(m)),
            "BM1387 is S9/T9-family only (S17=BM1397, S11=BM1391)"
        );
        // D2: BHB42801 chips/chain reconciled to 88 across hashboards + bm1362.
        assert_eq!(
            crate::hashboards::Hashboard::Bhb42801.catalog().chips_per_chain,
            88
        );
        assert_eq!(
            crate::bm1362::Bm1362HashboardSku::Bhb42801.asics_per_chain(),
            88
        );
        // D3: BM1370 cores reconciled to 1280 (operating_points + driver).
        assert_eq!(crate::bm1370::BM1370_CORES_PER_CHIP, 1280);
    }

    #[test]
    fn bm1362_catalog_routes_to_silicon_profile_module() {
        // The full BM1362 silicon profile lives in `bm1362.rs` (W11.5).
        // The re-export ensures consumers can reach
        // `asics::bm1362_profile::BM1362_TABLE` from a single import.
        assert_eq!(bm1362_profile::chip::CHIP_ID, 0x1362);
    }
}
