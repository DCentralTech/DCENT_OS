//! BM1393 protocol reference (S9k, 7nm).
//!
//! W11.10 (DCENT_OS Dev Kit Integration, 2026-05-09): codifies the BM1393
//! UART command set and core register catalog from the RE2
//!  §8.3 (UART Command Set) and §8.5 (Core
//! Registers). This module is **reference-only** — it is not yet wired to
//! any live driver. The on-fleet S9 path (am1-s9, BM1387) is unaffected.
//!
//! ## Why a separate module instead of growing `drivers/bm139x.rs`?
//!
//! `drivers/bm139x.rs` is the shared command framing helper for the
//! BM1397 / BM1398 / BM1396 / BM1362 / BM1366 / BM1368 line. BM1393 is the
//! 7nm S9k chip, in the same BM139x family but with its own documented
//! 14-opcode catalog and 8-register core map. We pin the catalog here so
//! a future S9k driver implementation can pull from a single source of
//! truth that matches RE2 exactly, instead of re-reading the doc every
//! time.
//!
//! ## Catalog source
//!
//! - RE2 §8.3 — 14 UART opcodes (`SET_BAUD` through `GENERAL_I2C`).
//! - RE2 §8.5 — 8 core registers (`CLOCK_DELAY_CTRL` through
//!   `SWEEP_CLOCK_CTRL`).
//! - RE2 `DCENT_OS_HARDWARE_CATALOG.md` §4.1 — chip family table:
//!   BM1393 = 7nm, UART 937500 baud, CRC8, 8-bit work_id, 32-bit nonce,
//!   used in S9k.
//! - RE2 `DCENT_OS_HARDWARE_CATALOG.md` §7.1 — protocol matrix:
//!   BM1391/3/6/7 share the register-prefix UART frame with CRC8. The
//!   exact CRC8 polynomial for the BM1391/3/6/7 sub-family is **not**
//!   pinned in RE2 R2; only BM1362 and BM1368 are explicitly confirmed
//!   `poly=0x31`. We therefore treat the BM1393 polynomial as `TBD` here
//!   and defer to RE2 R3 to confirm whether BM1393 also uses 0x31 or a
//!   different CRC8 polynomial (e.g. SMBus 0x07).
//!
//! ## Hard rules (NEVER violate when wiring this to a future driver)
//!
//! - `BM1393_WORK_ID_BITS = 8`. The FPGA work_id is also 8 bits — see
//!   . Storing as `u16` and truncating to
//!   `u8` later is the documented bug pattern that loses 89% of nonces.
//! - `BM1393_BAUD_DEFAULT = 937500`. RE2 §8.3 lists `SET_BAUD = 0x06`,
//!   but the operating baud is the BM139x family default 937500. Don't
//!   confuse the *opcode* with the *value*.
//! - `BM1393_NONCE_BITS = 32`. The 64-bit nonces apply to BM1362/1366
//!   (S19j Pro / S19 / S21), NOT BM1393.
//! - The 14 opcodes here are the **command catalog only**. Wire-frame
//!   layout (preamble, length, payload, CRC8) is in
//!   `drivers/bm139x.rs::fifo_*` for BM139x-style chips. BM1393 has not
//!   been live-tested against those helpers.

#![allow(dead_code)]

/// BM1393 UART command set per RE2 §8.3.
///
/// The 14 opcodes are pinned by `tests::all_14_opcodes_present` so a future
/// "helpful" refactor that drops one trips a compile-time test failure.
pub mod cmd {
    /// `0x06` SET_BAUD — set ASIC UART baud rate.
    pub const SET_BAUD: u8 = 0x06;
    /// `0x02` GET_STATUS — read ASIC status word.
    pub const GET_STATUS: u8 = 0x02;
    /// `0x10` SET_VOLTAGE — set ASIC core voltage (PIC-routed).
    pub const SET_VOLTAGE: u8 = 0x10;
    /// `0x11` SET_VOLTAGE_TIME — set voltage with timing/ramp parameter.
    pub const SET_VOLTAGE_TIME: u8 = 0x11;
    /// `0xCB` WRITE_REG — core register write.
    pub const WRITE_REG: u8 = 0xCB;
    /// `0xCA` READ_REG — core register read.
    pub const READ_REG: u8 = 0xCA;
    /// `0x34` RESET_HASHBOARD — issue a hash board reset frame.
    pub const RESET_HASHBOARD: u8 = 0x34;
    /// `0x38` BMC_COUNTER — BMC command counter (sequencing).
    pub const BMC_COUNTER: u8 = 0x38;
    /// `0x30` IIC — I²C pass-through (chip-side proxied I²C transaction).
    pub const IIC: u8 = 0x30;
    /// `0xC0` BC_WRITE — broadcast write.
    pub const BC_WRITE: u8 = 0xC0;
    /// `0xC4` BC_BUFFER — broadcast buffer (pre-stage broadcast payload).
    pub const BC_BUFFER: u8 = 0xC4;
    /// `0x80` QN_WRITE — QN write (per-chip queue/work write).
    pub const QN_WRITE: u8 = 0x80;
    /// `0x40` TW_WRITE — TW write (per-chip work transfer).
    pub const TW_WRITE: u8 = 0x40;
    /// `0x1C` GENERAL_I2C — general I²C transfer (separate from chip-proxied IIC).
    pub const GENERAL_I2C: u8 = 0x1C;

    /// Full opcode catalog — pinned for `tests::all_14_opcodes_present`.
    pub const ALL: [u8; 14] = [
        SET_BAUD,
        GET_STATUS,
        SET_VOLTAGE,
        SET_VOLTAGE_TIME,
        WRITE_REG,
        READ_REG,
        RESET_HASHBOARD,
        BMC_COUNTER,
        IIC,
        BC_WRITE,
        BC_BUFFER,
        QN_WRITE,
        TW_WRITE,
        GENERAL_I2C,
    ];
}

/// BM1393 core registers `0x0..=0x7` per RE2 §8.5.
pub mod core_reg {
    /// `0x0` CLOCK_DELAY_CTRL — clock delay configuration.
    pub const CLOCK_DELAY_CTRL: u8 = 0x0;
    /// `0x1` PROCESS_MONITOR_CTRL — process monitor control.
    pub const PROCESS_MONITOR_CTRL: u8 = 0x1;
    /// `0x2` PROCESS_MONITOR_DATA — process monitor data readback.
    pub const PROCESS_MONITOR_DATA: u8 = 0x2;
    /// `0x3` CORE_ERROR — core error flags.
    pub const CORE_ERROR: u8 = 0x3;
    /// `0x4` CORE_ENABLE — per-core enable mask (BIT[7:0] CORE_EN_I).
    pub const CORE_ENABLE: u8 = 0x4;
    /// `0x5` HASH_CLOCK_CTRL — hash clock control.
    pub const HASH_CLOCK_CTRL: u8 = 0x5;
    /// `0x6` HASH_CLOCK_COUNTER — hash clock counter readback.
    pub const HASH_CLOCK_COUNTER: u8 = 0x6;
    /// `0x7` SWEEP_CLOCK_CTRL — clock sweep control.
    pub const SWEEP_CLOCK_CTRL: u8 = 0x7;

    /// Full core register catalog (8 entries) — pinned for tests.
    pub const ALL: [u8; 8] = [
        CLOCK_DELAY_CTRL,
        PROCESS_MONITOR_CTRL,
        PROCESS_MONITOR_DATA,
        CORE_ERROR,
        CORE_ENABLE,
        HASH_CLOCK_CTRL,
        HASH_CLOCK_COUNTER,
        SWEEP_CLOCK_CTRL,
    ];
}

/// BM1393 default operating UART baud rate (per RE2
/// `DCENT_OS_HARDWARE_CATALOG.md` §4.1).
pub const BM1393_BAUD_DEFAULT: u32 = 937_500;

/// BM1393 work_id width (per RE2 §4.1 chip table). MUST match the FPGA
/// 8-bit `work_id` slot —.
pub const BM1393_WORK_ID_BITS: u32 = 8;

/// BM1393 nonce width (per RE2 §4.1 chip table). 32-bit, NOT 64-bit (the
/// 64-bit nonce family is BM1362 / BM1366 on S19j Pro / S19 variants).
pub const BM1393_NONCE_BITS: u32 = 32;

/// BM1393 CRC8 polynomial.
///
/// **Status:** TBD per RE2 R3.
///
/// RE2 `DCENT_OS_HARDWARE_CATALOG.md` §4.1 documents BM1393 as `CRC8`
/// without pinning a polynomial. The BM139x family table in §7.1 lists
/// "BM1391/3/6/7" with a generic `CRC8` and only confirms `poly=0x31`
/// for BM1362 and BM1368. The two most likely candidates are:
///
/// - **0x31** — Maxim/Dallas 1-Wire / BM1362+BM1368 family poly. Most
///   likely if the BM139x family is internally consistent.
/// - **0x07** — SMBus / CRC-8/I-CODE poly. Alternative if the older 7nm
///   chips diverge from the 5nm BM136x family.
///
/// The future S9k driver implementation MUST live-verify against an
/// S9k unit before pinning. Until then, treat this as a documentation
/// placeholder; do not rely on the value at runtime.
pub const BM1393_CRC8_POLY_TBD: u8 = 0x00;

/// BM1393 chip ID (per BM139x family 16-bit ID convention).
///
/// Documented for completeness; not yet used as a registry key. The
/// BM139x family chip IDs are:
/// `0x1391` (T15/S11), `0x1393` (S9k), `0x1396` (S17+/T17+),
/// `0x1397` (S17/T17), `0x1398` (S19/S19+/S21).
pub const CHIP_ID: u16 = 0x1393;

#[cfg(test)]
mod tests {
    use super::*;

    /// All 14 opcodes from RE2 §8.3 are present in the catalog.
    #[test]
    fn all_14_opcodes_present() {
        assert_eq!(
            cmd::ALL.len(),
            14,
            "RE2 §8.3 lists 14 UART opcodes for BM1393"
        );
        // Spot-check the four highest-risk opcodes (voltage + register IO).
        assert_eq!(cmd::SET_VOLTAGE, 0x10);
        assert_eq!(cmd::SET_VOLTAGE_TIME, 0x11);
        assert_eq!(cmd::WRITE_REG, 0xCB);
        assert_eq!(cmd::READ_REG, 0xCA);
        // No duplicates in the catalog.
        let mut sorted = cmd::ALL.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 14, "opcode catalog must have no duplicates");
    }

    /// All 8 core registers from RE2 §8.5 are present (`0x0..=0x7`).
    #[test]
    fn core_register_count_is_8() {
        assert_eq!(
            core_reg::ALL.len(),
            8,
            "RE2 §8.5 lists 8 core registers (0x0..=0x7) for BM1393"
        );
        // Pinned addresses 0..7 inclusive, in order.
        for (i, reg) in core_reg::ALL.iter().enumerate() {
            assert_eq!(
                *reg as usize, i,
                "core register {} must be address 0x{:X}",
                i, i
            );
        }
        assert_eq!(core_reg::CORE_ENABLE, 0x4);
        assert_eq!(core_reg::HASH_CLOCK_CTRL, 0x5);
    }

    /// Regression pin for . The BM1393
    /// work_id is 8 bits — using `u16` and truncating breaks 89% of
    /// nonce attribution.
    #[test]
    fn work_id_is_8_bits() {
        assert_eq!(BM1393_WORK_ID_BITS, 8);
        // Sanity: 8 bits = 256 distinct work_id slots.
        assert_eq!(1u32 << BM1393_WORK_ID_BITS, 256);
    }

    /// Default operating baud is 937500 per RE2 §4.1.
    #[test]
    fn baud_default_is_937500() {
        assert_eq!(BM1393_BAUD_DEFAULT, 937_500);
    }

    /// Nonce width is 32 bits — NOT the 64-bit BM1362/1366 family value.
    #[test]
    fn nonce_is_32_bits() {
        assert_eq!(BM1393_NONCE_BITS, 32);
    }

    /// CRC8 polynomial is documented as TBD until RE2 R3 pins it from
    /// live S9k traffic. Test exists to flag the day someone "fixes" the
    /// placeholder by hardcoding a value without RE2 evidence.
    #[test]
    fn crc8_poly_is_explicitly_tbd() {
        assert_eq!(
            BM1393_CRC8_POLY_TBD, 0x00,
            "If you are pinning this to 0x31 or 0x07, update the doc-comment \
             on BM1393_CRC8_POLY_TBD with the live-verified RE2 R3 evidence \
             before merging."
        );
    }

    /// Chip ID slot follows BM139x family convention (`0x139_`).
    #[test]
    fn chip_id_is_bm1393() {
        assert_eq!(CHIP_ID, 0x1393);
    }
}
