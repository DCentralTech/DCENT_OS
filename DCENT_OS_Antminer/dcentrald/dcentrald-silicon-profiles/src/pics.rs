//! 9 (2026-05-09): PIC microcontroller catalog.
//!
//! Source-cite: `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/`
//! `DCENT_OS_HARDWARE_CATALOG.md` §6 (lines 531-561).
//!
//! Two PIC families are deployed across the Antminer line:
//!
//! - **dsPIC33EP16GS202** — 16-bit dsPIC33E core, 16 KB program / 2 KB
//!   RAM. Used on S9 / T9 / S11 / S15 / T17. I²C address `0x20`.
//! - **PIC1704** — PIC16F1704 (8-bit) per gpdasm. Used on S19 / S19j
//!   Pro / S19i / S19 XP and the S21 family. I²C address `0x20`.
//!   (A43 — goldmine 2026-06-10: corrected from the earlier RE2 guess of
//!   "likely dsPIC33CH/PIC24F"; gpdasm of the on-disk image proves an
//!   8-bit PIC16F1704.)
//!
//! Both PICs **share the same register map** (RE2 §6.2) — DCENT_OS treats
//! them as protocol-compatible at the register level, with platform
//! routing in `dcentrald-asic::pic1704` deciding which sealed-trait
//! marker (CV1835 / AM335x BB / Amlogic S19j Pro) is allowed to drive
//! the chip.
//!
//! ## Critical exception: S21 Amlogic NoPic
//!
//!, the S21 Amlogic carrier
//! does NOT use a PIC at all — voltage regulation is handled by
//! repurposed TAS5782M audio DACs. **GPIO-mediated PIC reset on S21
//! Amlogic kills the DAC voltage output.** The catalog flags this via
//! `Pic::S21AmlogicNoPic` so routing code can refuse to dispatch a PIC
//! sequence on that platform.
//!
//! See also  and
//!  for the full
//! authoritative routing rules.

use serde::{Deserialize, Serialize};

/// Architecture of a PIC family. dsPIC33E/CH and PIC24F all expose the
/// same I²C register map per RE2 §6.2; the architecture distinction is
/// recorded for tooling that fingerprints a PIC by program-memory dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicArchitecture {
    /// dsPIC33E — 16-bit DSP-enhanced core (S9 / T9 / S11 / S15 / T17).
    DsPic33E,
    /// PIC16F1704 — 8-bit baseline-enhanced PIC core. Used for the
    /// `Pic1704` voltage controller on S19 / S19j Pro / S19i / S19 XP.
    /// A38 (goldmine 2026-06-10): gpdasm of the on-disk PIC1704 image
    /// proves an 8-bit PIC16F1704, NOT the dsPIC33CH/PIC24F class RE2
    /// had guessed.
    Pic16F,
    /// dsPIC33CH or PIC24F (RE2 lists "likely dsPIC33CH/PIC24F").
    /// Retained only as the placeholder arch label for the S21 Amlogic
    /// `NoPic` sentinel (no real PIC on that platform). A38 re-pointed
    /// `Pic1704` off this label onto `Pic16F`.
    DsPic33ChOrPic24F,
}

/// Catalog row for one PIC family. Lightweight metadata only; protocol
/// drivers live in `dcentrald-asic::dspic` (dsPIC) and
/// `dcentrald-asic::pic1704` (PIC1704).
///
/// `Deserialize` is intentionally NOT derived — `used_in` borrows from
/// a `&'static` table which serde can't reconstruct from JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PicCatalogEntry {
    /// Vendor part number / silicon name.
    pub part_number: &'static str,
    /// Architecture family.
    pub architecture: PicArchitecture,
    /// Program memory in bytes. 0 if not pinned by RE2 (PIC1704 catalog
    /// row leaves program/data unspecified — RE2 §6.1 line 538).
    pub program_memory_bytes: u32,
    /// Data RAM in bytes. 0 if not pinned by RE2.
    pub data_ram_bytes: u32,
    /// 7-bit I²C slave address.
    pub i2c_address: u8,
    /// Antminer models that ship this PIC.
    pub used_in: &'static [&'static str],
}

/// PIC enum — catalog key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pic {
    /// dsPIC33EP16GS202 — S9 / T9 / S11 / S15 / T17.
    Dspic33Ep16Gs202,
    /// PIC1704 — S19 / S19j Pro / S19i / S19 XP / S21 (NOT S21
    /// Amlogic).
    Pic1704,
    /// **NOT A PIC** — sentinel for the S21 Amlogic NoPic exception.
    /// Voltage is handled by TAS5782M DACs; routing code MUST refuse
    /// PIC sequences when this catalog entry is selected. Per
    /// .
    S21AmlogicNoPic,
}

impl Pic {
    /// Catalog row. Returns a synthetic "no PIC" row for the S21
    /// Amlogic exception so routing code has a uniform shape.
    pub const fn catalog(self) -> PicCatalogEntry {
        match self {
            Pic::Dspic33Ep16Gs202 => PicCatalogEntry {
                part_number: "dsPIC33EP16GS202",
                architecture: PicArchitecture::DsPic33E,
                program_memory_bytes: 16 * 1024,
                data_ram_bytes: 2 * 1024,
                i2c_address: 0x20,
                // CORRECTED 2026-07-02: dsPIC33EP16GS202 is the S17-class
                // voltage controller (AMTC: /mnt/mmc1/dsPIC33EP16GS202_app.txt
                // is the S17 PIC-update path). Removed S9/T9 — the S9 uses a
                // PIC16F1704 @ I2C 0x55-0x57 (7 fw versions v1.9..V5.1; see the
                // S9 PIC protocol in DCENT_OS_Antminer/), NOT this
                // dsPIC. Removed S11/S15 — the S11 factory jig (BM1391) shows a
                // PIC16-class I2C DAC + AT24C02, not this dsPIC (S15's PIC is
                // UNCONFIRMED pending a live gpdasm dump).
                used_in: &["S17", "S17 Pro", "T17"],
            },
            Pic::Pic1704 => PicCatalogEntry {
                part_number: "PIC1704",
                // A38 (goldmine 2026-06-10): gpdasm proves PIC16F1704 (8-bit),
                // not the dsPIC33CH/PIC24F class RE2 guessed.
                architecture: PicArchitecture::Pic16F,
                program_memory_bytes: 0,
                data_ram_bytes: 0,
                // A41 (goldmine 2026-06-10): the 7-bit I²C address is strapped
                // from PORTC[2:0] via `addr = (PORTC | 0x40) >> 1`, yielding
                // 0x20 / 0x21 / 0x22 for PORTC[2:0] = 0 / 1 / 2 — i.e. the
                // per-chain 0x20/0x21/0x22 controllers seen on `a lab unit`/`a lab unit`.
                // 0x20 here is the chain-0 (PORTC[2:0]=0) canonical address.
                i2c_address: 0x20,
                // S21 deliberately absent: S21 Amlogic uses NoPic
                // (TAS5782M DAC).
                // and `dcentrald-asic::pic1704::service::platforms` —
                // no S21* marker exists by design.
                //
                // NOTE: the S9's voltage controller is ALSO a PIC16F1704, but a
                // DISTINCT instantiation @ I2C 0x55-0x57 (not the 0x20/0x21/0x22
                // per-chain map above). It is handled in the S9 platform path,
                // not enumerated as a catalog row here — its absence from this
                // list does NOT mean "S9 has no PIC".
                used_in: &["S19", "S19i", "S19j Pro", "S19 XP", "T19"],
            },
            Pic::S21AmlogicNoPic => PicCatalogEntry {
                part_number: "<S21 Amlogic NoPic>",
                architecture: PicArchitecture::DsPic33ChOrPic24F,
                program_memory_bytes: 0,
                data_ram_bytes: 0,
                i2c_address: 0x00,
                used_in: &["S21 Amlogic"],
            },
        }
    }

    /// Whether routing code is allowed to dispatch a PIC sequence
    /// against this catalog entry. False for the S21 Amlogic NoPic
    /// exception.
    pub const fn is_pic_sequence_allowed(self) -> bool {
        !matches!(self, Pic::S21AmlogicNoPic)
    }
}

/// Shared register map (RE2 §6.2 lines 542-550). **Identical between
/// dsPIC33EP16GS202 and PIC1704** — that's the load-bearing fact for
/// the W11.3 PIC1704 driver.
pub mod registers {
    /// 0x00 — VERSION. Reads:
    /// - `0x86` = bootloader (post-RESET corruption state on am2 Zynq;
    /// writes are HAL-denied
    ///   by default).
    /// - `0x89` = application running.
    /// - `0x88` = Rev A.
    /// - `0x8A` = Rev B.
    pub const REG_VERSION: u8 = 0x00;

    /// 0x01 — TEMPERATURE (signed, 0.1°C resolution).
    pub const REG_TEMPERATURE: u8 = 0x01;

    /// 0x02-0x03 — VOLTAGE in millivolts, little-endian.
    pub const REG_VOLTAGE_LO: u8 = 0x02;
    pub const REG_VOLTAGE_HI: u8 = 0x03;

    /// 0x04-0x05 — CURRENT in milliamps, little-endian.
    pub const REG_CURRENT_LO: u8 = 0x04;
    pub const REG_CURRENT_HI: u8 = 0x05;

    /// 0x06 — alternate temperature sensor.
    pub const REG_TEMP_ALT: u8 = 0x06;

    /// 0x08 — STATUS bitfield.
    /// - bit 0: DC-DC ON
    /// - bit 1: APP RUNNING
    /// - bit 2: FAULT
    /// - bit 3: OTP (one-time-programmable lockout)
    pub const REG_STATUS: u8 = 0x08;

    /// 0x09 — CONTROL byte (write-only).
    /// - 0x00 = OFF
    /// - 0x01 = ON
    /// - 0x02 = HEARTBEAT
    /// - 0x80 = RESET
    pub const REG_CONTROL: u8 = 0x09;

    /// CONTROL byte value: turn DC-DC OFF.
    pub const CONTROL_OFF: u8 = 0x00;
    /// CONTROL byte value: turn DC-DC ON.
    pub const CONTROL_ON: u8 = 0x01;
    /// CONTROL byte value: heartbeat tick (must be sent ≤ 2 s per
    /// ).
    pub const CONTROL_HEARTBEAT: u8 = 0x02;
    /// CONTROL byte value: RESET. Destructive on am2 Zynq dsPIC fw=0x86;
    /// `dcentrald` doesn't link this opcode by default (gated by the
    /// `recovery-tool` Cargo feature on `dspic_flash`).
    pub const CONTROL_RESET: u8 = 0x80;

    /// VERSION byte that indicates application mode (chip is healthy).
    pub const VERSION_APP: u8 = 0x89;

    /// VERSION byte that indicates bootloader / corrupted state.
    pub const VERSION_BOOTLOADER: u8 = 0x86;

    /// Bootloader-jump magic byte (write to REG_VERSION).
    pub const BOOTLOADER_JUMP_MAGIC: u8 = 0x5A;
}

/// Bootloader-to-application jump sequence (RE2 §6.2 line 552).
///
/// `Write 0x5A → REG_VERSION, then 0x01 → REG_CONTROL, then poll
/// REG_VERSION until it reads 0x89 (app mode).`
///
/// Wrap up the magic so callers don't have to memorize the byte
/// sequence.
pub const BOOTLOADER_JUMP_SEQUENCE: &[(u8, u8)] = &[
    (registers::REG_VERSION, registers::BOOTLOADER_JUMP_MAGIC),
    (registers::REG_CONTROL, registers::CONTROL_ON),
];

/// A26 (goldmine 2026-06-10): S21 PIC reset-timing constants lifted from the
/// Bitmain S21 single-board-test jig `reset_pic@C96E0`.
///
/// ⚠️ DATA ONLY — recorded for a future S21 PIC-reset path and deliberately
/// **NOT** referenced by any live path in this crate or `dcentrald`. They
/// pre-stage the timing budget without touching live S21 hardware. The
/// destructive PIC RESET itself remains gated behind the `recovery-tool`
/// Cargo feature in `dcentrald-asic`.
///
/// Delay between asserting PIC RESET and the follow-up write.
pub const S21_PIC_RESET_WRITE_WAIT_MS: u32 = 300;
/// Settle delay after the post-reset unlock sequence before the PIC is
/// considered ready.
pub const S21_PIC_RESET_POST_UNLOCK_MS: u32 = 500;

/// Every PIC catalog entry, including the S21 Amlogic NoPic sentinel.
pub const ALL_PICS: &[Pic] = &[Pic::Dspic33Ep16Gs202, Pic::Pic1704, Pic::S21AmlogicNoPic];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pic1704_and_dspic33ep_share_register_map() {
        // Pin: RE2 §6.2 — register map is IDENTICAL across the two
        // families. Both share I²C address 0x20.
        let dspic = Pic::Dspic33Ep16Gs202.catalog();
        let pic1704 = Pic::Pic1704.catalog();
        assert_eq!(dspic.i2c_address, 0x20);
        assert_eq!(pic1704.i2c_address, 0x20);
        // Architecture distinction is real (dsPIC33E vs 8-bit PIC16F1704);
        // both still expose the same I²C register map. A38 (goldmine
        // 2026-06-10): PIC1704 is a PIC16F1704 per gpdasm, not dsPIC33CH/PIC24F.
        assert_eq!(dspic.architecture, PicArchitecture::DsPic33E);
        assert_eq!(pic1704.architecture, PicArchitecture::Pic16F);
    }

    #[test]
    fn pic1704_architecture_is_pic16f() {
        // A38 (goldmine 2026-06-10): gpdasm proves the PIC1704 voltage
        // controller is an 8-bit PIC16F1704. Pin it so the old
        // dsPIC33CH/PIC24F guess can't creep back in.
        assert_eq!(Pic::Pic1704.catalog().architecture, PicArchitecture::Pic16F);
    }

    #[test]
    fn s21_pic_reset_timing_constants_pinned() {
        // A26 (goldmine 2026-06-10): jig reset_pic@C96E0 timings. DATA ONLY —
        // pinned so a future S21 reset path inherits the exact values; no live
        // caller references them yet.
        assert_eq!(S21_PIC_RESET_WRITE_WAIT_MS, 300);
        assert_eq!(S21_PIC_RESET_POST_UNLOCK_MS, 500);
    }

    #[test]
    fn s21_amlogic_nopic_blocks_pic_dispatch() {
        //: GPIO-mediated PIC
        // RESET on S21 Amlogic kills the TAS5782M DAC voltage output.
        // The catalog must flag this so routing refuses dispatch.
        let nopic = Pic::S21AmlogicNoPic;
        assert!(!nopic.is_pic_sequence_allowed());
        assert!(Pic::Dspic33Ep16Gs202.is_pic_sequence_allowed());
        assert!(Pic::Pic1704.is_pic_sequence_allowed());
    }

    #[test]
    fn dspic33ep_memory_pinned() {
        // Pin: RE2 §6.1 line 537 — 16 KB program (5461 words on a
        // dsPIC33E with 24-bit instructions; we record the BYTE size),
        // 2 KB data RAM.
        let cat = Pic::Dspic33Ep16Gs202.catalog();
        assert_eq!(cat.program_memory_bytes, 16 * 1024);
        assert_eq!(cat.data_ram_bytes, 2 * 1024);
    }

    #[test]
    fn register_map_constants_pinned() {
        // RE2 §6.2 lines 543-550. Pin every register address so an
        // accidental refactor surfaces immediately.
        assert_eq!(registers::REG_VERSION, 0x00);
        assert_eq!(registers::REG_TEMPERATURE, 0x01);
        assert_eq!(registers::REG_VOLTAGE_LO, 0x02);
        assert_eq!(registers::REG_VOLTAGE_HI, 0x03);
        assert_eq!(registers::REG_CURRENT_LO, 0x04);
        assert_eq!(registers::REG_CURRENT_HI, 0x05);
        assert_eq!(registers::REG_TEMP_ALT, 0x06);
        assert_eq!(registers::REG_STATUS, 0x08);
        assert_eq!(registers::REG_CONTROL, 0x09);
    }

    #[test]
    fn version_bytes_pinned() {
        // Pin: bootloader = 0x86, application = 0x89.
        assert_eq!(registers::VERSION_BOOTLOADER, 0x86);
        assert_eq!(registers::VERSION_APP, 0x89);
    }

    #[test]
    fn control_byte_constants_pinned() {
        assert_eq!(registers::CONTROL_OFF, 0x00);
        assert_eq!(registers::CONTROL_ON, 0x01);
        assert_eq!(registers::CONTROL_HEARTBEAT, 0x02);
        assert_eq!(registers::CONTROL_RESET, 0x80);
    }

    #[test]
    fn bootloader_jump_sequence_writes_magic_then_control() {
        // RE2 §6.2 line 552 — the canonical jump sequence is:
        //   1) write 0x5A to REG_VERSION (0x00)
        //   2) write 0x01 to REG_CONTROL (0x09)
        //   3) poll REG_VERSION for 0x89.
        // Pin step 1+2 in the constant.
        assert_eq!(BOOTLOADER_JUMP_SEQUENCE.len(), 2);
        assert_eq!(BOOTLOADER_JUMP_SEQUENCE[0], (0x00, 0x5A));
        assert_eq!(BOOTLOADER_JUMP_SEQUENCE[1], (0x09, 0x01));
    }

    #[test]
    fn all_pics_present_with_nopic_sentinel() {
        // Two real PICs + one NoPic sentinel = 3 catalog entries.
        assert_eq!(ALL_PICS.len(), 3);
    }

    #[test]
    fn dspic33ep_used_in_s17_class_not_s9() {
        // CORRECTED 2026-07-02: dsPIC33EP16GS202 is the S17-class voltage
        // controller (AMTC S17 PIC-update path). The S9 uses a PIC16F1704 @
        // I2C 0x55-0x57 — NOT this dsPIC — so the prior "used_in S9" was a
        // stale RE2 §6.1 grouping. PIC1704 ships on the S19/S21 family.
        let dspic = Pic::Dspic33Ep16Gs202.catalog();
        assert!(
            dspic.used_in.iter().any(|m| *m == "S17"),
            "S17 (dsPIC33EP16GS202 host) must be present"
        );
        assert!(
            !dspic.used_in.iter().any(|m| *m == "S9"),
            "S9 uses PIC16F1704 @ 0x55-0x57, not this dsPIC"
        );
        let pic1704 = Pic::Pic1704.catalog();
        assert!(pic1704.used_in.iter().any(|m| *m == "S19j Pro"));
    }
}
