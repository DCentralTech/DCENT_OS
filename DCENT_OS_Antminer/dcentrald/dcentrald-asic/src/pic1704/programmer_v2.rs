//! PIC1704 framed-protocol programmer v2 (W14.C, R4 inferred).
//!
//! Implements the framed-protocol commands documented in W4 handoff
//! `pic1704_v2.{c,h}` + `pic1704_reflash_update_summary.md`:
//!
//! | REG_CMD | Mnemonic    | Action                            |
//! |---------|-------------|-----------------------------------|
//! | 0x10    | SEEK        | Set program memory address ptr    |
//! | 0x11    | ERASE_PAGE  | Erase 512-word flash page         |
//! | 0x12    | WRITE_WORDS | Write N × 24-bit instruction words|
//! | 0x13    | VERIFY_CRC  | Compute & return CRC-16           |
//! | 0x14    | START_APP   | Jump to app at 0x000200           |
//! | 0x15    | READ_VERSION| Read bootloader/app version       |
//!
//! ## Distinct from W11.7 `programmer.rs`
//!
//! W11.7 `programmer.rs` uses BraiinsOS-shared register-style opcodes
//! (`[0x09, 0x01, ...]` etc — REG_CONTROL prefix). W14.C uses framed
//! ordinals (no register prefix). Both are retained as compile-gated protocol
//! research; neither has a shipped transport consumer.
//!
//! ## Research-only boundary
//!
//! 1. Module wrapped in `#![cfg(feature = "recovery-tool")]`.
//! 2. No shipped package enables `recovery-tool`; the diagnostic-only
//!    `pic-recovery` package is not a consumer.
//! 3. Historical CLI confirmation flags are not deployment authority. A
//!    future executor requires the separate controller-recovery authority
//!    architecture and must preserve the uncertainty labels below.
//!
//! ## REG_VOLTAGE_L collision guard (MANDATORY)
//!
//! REG_CMD `0x10` (SEEK) collides with REG_VOLTAGE_L address `0x10` in
//! the PIC1704 register map. If an app-mode chip is misclassified as
//! bootloader, sending `[0x10, addr×3]` could be interpreted as a
//! voltage write to register 0x10 → silent overvolt risk. Every framed
//! transaction in this module MUST pre-read REG_VERSION and refuse if
//! not 0x86 (bootloader version).
//!
//! ## Honest confidence label
//!
//! CRC ground-truth values supplied by  Q2 (see
//! `Handoffs/DCENT_OS_FULL_HANDOFF/DCENT_OS_HANDOFF/RE_TEAM_FINDINGS_WAVE5.md`
//! lines 78-97): S11 firmware (1274 application words, no FFFFFF pad)
//! computes to [`PIC1704_CRC_S11`] = `0xE638`; T9 firmware (1590
//! application words, no FFFFFF pad) computes to [`PIC1704_CRC_T9`] =
//! `0xF9F4`. Single-word + multi-word test vectors land at
//! [`PIC1704_CRC_TV1`] / [`PIC1704_CRC_T9_TV2`] / [`PIC1704_CRC_T9_TV3`]
//! / [`PIC1704_CRC_T9_TV4`] / [`PIC1704_CRC_T9_TV5`] /
//! [`PIC1704_CRC_T9_TV7`] / [`PIC1704_CRC_S11_TV1`].
//!
//! Wire-format byte sequences for the framed protocol remain
//! bench-untested (W4 V2 protocol theoretical/inferred from dsPIC33EP
//! bootloader spec; the W15.B `programmer_stock` module covers the
//! GHIDRA-EXTRACTED stock bmminer protocol). Both protocols accept the
//! same CRC algorithm (CRC-ITU-T V.41, poly `0x1021`, init `0x0000`,
//! no XOR-out), so the CRC test vectors below pin algorithm correctness
//! independently of which framed-vs-stock wire format is in use. Do NOT
//! label `--90-percent`.
//!
//! ## CRC algorithm
//!
//! CRC-ITU-T V.41 (poly 0x1021, init 0x0000, no final XOR), MSB-first
//! per 24-bit word: byte[23:16], byte[15:8], byte[7:0]. NOT the same
//! as `crate::protocol::crc16` which is CCITT-FALSE (init 0xFFFF).
//! Different init value → different CRC values. Do not substitute.

#![cfg(feature = "recovery-tool")]

// === REG_CMD framed-protocol ordinals ===

/// SEEK: set program memory address pointer. Wire: `[0x10, addr_LE×3]`.
pub const FP_SEEK: u8 = 0x10;
/// ERASE_PAGE: erase one 512-word flash page. Wire: `[0x11, addr_LE×3]`.
pub const FP_ERASE_PAGE: u8 = 0x11;
/// WRITE_WORDS: write N × 24-bit instruction words. Wire:
/// `[0x12, count_LE_u16, word_MSB×3 × N]`.
pub const FP_WRITE_WORDS: u8 = 0x12;
/// VERIFY_CRC: compute & return CRC-16. Wire write `[0x13]`, then read 2 bytes LE.
pub const FP_VERIFY_CRC: u8 = 0x13;
/// START_APP: jump to app at 0x000200. Wire: `[0x14, 0x01]`.
pub const FP_START_APP: u8 = 0x14;
/// READ_VERSION: read bootloader/app version byte. Wire write `[0x15]`, then read 1 byte.
pub const FP_READ_VERSION: u8 = 0x15;

// === Geometry constants ===

/// Flash page size in 24-bit instruction words (dsPIC33EP16GS202).
pub const FLASH_PAGE_WORDS: u32 = 512;

/// Application start address. Bootloader occupies 0x000000..0x000200.
pub const FLASH_APP_START: u32 = 0x000200;

/// Maximum erase address = 8 pages × 512 words = 4096 words. Bootloader
/// occupies 0x000000..0x000200; never erase below.
pub const FLASH_MAX_WORDS: u32 = 8 * FLASH_PAGE_WORDS - 256; // = 3840

/// Maximum words per WRITE_WORDS batch (I²C RX buffer limit per W4 handoff).
pub const BATCH_MAX: u16 = 16;

/// Polling interval (ms) for post-START_APP version reads.
pub const POLL_MS: u64 = 100;

/// Total timeout (ms) for post-START_APP version polling.
pub const TIMEOUT_MS: u64 = 5000;

// === Expected version bytes ===

/// Bootloader version byte. Required for every framed transaction
/// (collision guard against REG_VOLTAGE_L=0x10 in app mode).
pub const VERSION_BOOTLOADER: u8 = 0x86;
/// Application revision A.
pub const VERSION_APP_88: u8 = 0x88;
/// Application revision (canonical post-reflash).
pub const VERSION_APP_89: u8 = 0x89;
/// Application revision B.
pub const VERSION_APP_8A: u8 = 0x8A;

// ===  Q2 ground-truth CRC values (W15.A4) ===
//
// CRC-ITU-T V.41 (poly 0x1021, init 0x0000, no final XOR), MSB-first per
// 24-bit word. Source:
// `Handoffs/DCENT_OS_FULL_HANDOFF/DCENT_OS_HANDOFF/RE_TEAM_FINDINGS_WAVE5.md`
// §Q2 lines 78-97. Computed against the verified-good firmware files
// `dsPIC33EP16GS202_app.txt` extracted from the S11 and T9 stock rootfs
// images respectively. Values pinned here so VERIFY_CRC parity tests can
// reference a single source-of-truth without re-running the host CRC.

///  Q2 ground-truth CRC for **S11** `dsPIC33EP16GS202_app.txt`
/// (1274 application words, no `0xFFFFFF` fill).
pub const PIC1704_CRC_S11: u16 = 0xE638;

///  Q2 ground-truth CRC for **T9** `dsPIC33EP16GS202_app.txt`
/// (1590 application words, no `0xFFFFFF` fill).
pub const PIC1704_CRC_T9: u16 = 0xF9F4;

///  Q2 TV1 — single 24-bit word `0xFA0000` → CRC `0x2493`.
/// Pins the algorithm choice + per-word MSB-first byte order without
/// requiring the firmware hex file to be present at test time.
pub const PIC1704_CRC_TV1: u16 = 0x2493;

///  Q2 TV2 — first 4 words of T9 firmware → CRC `0xDB23`. Requires
/// the T9 hex file at runtime to actually compute against; pinned here
/// so any future host-side fixture loader can compare to the canonical
/// expected value.
pub const PIC1704_CRC_T9_TV2: u16 = 0xDB23;

///  Q2 TV3 — first 16 words (1 I2C batch) of T9 firmware →
/// CRC `0xFAB2`.
pub const PIC1704_CRC_T9_TV3: u16 = 0xFAB2;

///  Q2 TV4 — first 32 words (2 I2C batches) of T9 firmware →
/// CRC `0x6056`.
pub const PIC1704_CRC_T9_TV4: u16 = 0x6056;

///  Q2 TV5 — first 512 words (1 flash page) of T9 firmware →
/// CRC `0x34F5`.
pub const PIC1704_CRC_T9_TV5: u16 = 0x34F5;

///  Q2 TV7 — full T9 hex file (3520 words including `0xFFFFFF`
/// pad to file end) → CRC `0x7119`. Distinct from [`PIC1704_CRC_T9`]
/// (1590 application words, no pad) — the device CRC is computed over
/// the application range, the file CRC over the full padded file.
pub const PIC1704_CRC_T9_TV7: u16 = 0x7119;

///  Q2 S11-TV1 — first 4 words of S11 firmware → CRC `0xCAED`.
pub const PIC1704_CRC_S11_TV1: u16 = 0xCAED;

// === Errors ===

/// Errors emitted by host-side wire-format helpers in this module.
///
/// This enum is for pre-wire validation that catches malformed inputs before
/// any byte could reach a bus. A future authorized transport must surface I²C
/// errors separately and fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpError {
    /// Refuses any seek/erase below 0x000200 (bootloader region sacred).
    AddressBelowAppStart,
    /// Address exceeds the writable flash region.
    AddressAboveFlashMax,
    /// Erase requires page-aligned address (lower 9 bits zero).
    UnalignedPageAddress,
    /// Batch size exceeds BATCH_MAX (16 words).
    BatchTooLarge,
    /// Batch is empty.
    BatchEmpty,
    /// Pre-write VERSION read returned a non-bootloader byte. Collision
    /// guard against REG_VOLTAGE_L=0x10 in app mode.
    NotInBootloader,
    /// VERIFY_CRC returned a different value than `compute_crc_host()`.
    CrcMismatch,
    /// Post-VERIFY_CRC read timed out.
    VerifyTimeout,
    /// Post-START_APP version poll timed out without observing 0x88/0x89/0x8A.
    StartAppTimeout,
    /// Word in WRITE_WORDS payload exceeds 24-bit width.
    InvalidWordWidth,
    /// A future authorized invocation is missing its signed target manifest.
    NoManifest,
    /// The confirmed target serial does not match the manifest's
    /// `target_serial` field.
    SerialMismatch,
}

// === Wire-format helpers ===

/// SEEK command bytes: `[0x10, addr_LO, addr_MID, addr_HI]`.
/// 24-bit address LE on wire. Refuses addr < FLASH_APP_START OR > FLASH_MAX_WORDS.
pub fn seek_steps_v2(address: u32) -> Result<Vec<u8>, FpError> {
    if address < FLASH_APP_START {
        return Err(FpError::AddressBelowAppStart);
    }
    if address > FLASH_MAX_WORDS {
        return Err(FpError::AddressAboveFlashMax);
    }
    Ok(vec![
        FP_SEEK,
        (address & 0xFF) as u8,
        ((address >> 8) & 0xFF) as u8,
        ((address >> 16) & 0xFF) as u8,
    ])
}

/// ERASE_PAGE command bytes. Address must be page-aligned (lower 9 bits zero).
pub fn erase_steps_v2(page_addr: u32) -> Result<Vec<u8>, FpError> {
    if page_addr < FLASH_APP_START {
        return Err(FpError::AddressBelowAppStart);
    }
    if page_addr > FLASH_MAX_WORDS {
        return Err(FpError::AddressAboveFlashMax);
    }
    if page_addr & (FLASH_PAGE_WORDS - 1) != 0 {
        return Err(FpError::UnalignedPageAddress);
    }
    Ok(vec![
        FP_ERASE_PAGE,
        (page_addr & 0xFF) as u8,
        ((page_addr >> 8) & 0xFF) as u8,
        ((page_addr >> 16) & 0xFF) as u8,
    ])
}

/// WRITE_WORDS command bytes. Each word is 24 bits, MSB-first on wire:
/// `[byte[23:16], byte[15:8], byte[7:0]]`. Count is LE u16.
pub fn write_steps_v2(words: &[u32]) -> Result<Vec<u8>, FpError> {
    if words.is_empty() {
        return Err(FpError::BatchEmpty);
    }
    if words.len() > BATCH_MAX as usize {
        return Err(FpError::BatchTooLarge);
    }
    for &w in words {
        if w > 0x00FF_FFFF {
            return Err(FpError::InvalidWordWidth);
        }
    }
    let count = words.len() as u16;
    let mut out = Vec::with_capacity(3 + 3 * words.len());
    out.push(FP_WRITE_WORDS);
    out.extend_from_slice(&count.to_le_bytes());
    for &w in words {
        out.push(((w >> 16) & 0xFF) as u8);
        out.push(((w >> 8) & 0xFF) as u8);
        out.push((w & 0xFF) as u8);
    }
    Ok(out)
}

/// CRC-ITU-T V.41 (poly 0x1021, init 0x0000, no final XOR) over a byte
/// stream. Distinct from `crate::protocol::crc16` (CCITT-FALSE init
/// 0xFFFF) — do NOT substitute.
fn crc_itu_t_v41(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Compute host-side CRC-16 over an array of 24-bit words.
/// MSB-first per word, init=0x0000, no final XOR.
/// Returns the same value the device should report on VERIFY_CRC.
pub fn compute_crc_host(words: &[u32]) -> u16 {
    let mut buf = Vec::with_capacity(3 * words.len());
    for &w in words {
        buf.push(((w >> 16) & 0xFF) as u8);
        buf.push(((w >> 8) & 0xFF) as u8);
        buf.push((w & 0xFF) as u8);
    }
    crc_itu_t_v41(&buf)
}

/// Decode VERIFY_CRC response bytes. Wire format is LITTLE-ENDIAN per
/// W4 handoff `pic1704_v2.c:194`: `*crc = rbuf[0] | (rbuf[1] << 8)`.
pub fn decode_verify_response(rbuf: &[u8; 2]) -> u16 {
    u16::from_le_bytes(*rbuf)
}

/// START_APP command bytes: `[0x14, 0x01]`.
pub fn start_app_steps_v2() -> Vec<u8> {
    vec![FP_START_APP, 0x01]
}

/// READ_VERSION command byte: `[0x15]`. Response is 1 byte.
pub fn read_version_step_v2() -> u8 {
    FP_READ_VERSION
}

/// Pre-write REG_VERSION collision guard. Caller MUST invoke before
/// every framed transaction. Returns Err(NotInBootloader) if version
/// is not 0x86.
pub fn collision_guard(version: u8) -> Result<(), FpError> {
    if version != VERSION_BOOTLOADER {
        return Err(FpError::NotInBootloader);
    }
    Ok(())
}

// === Tests (host-safe, no I²C bus required) ===

#[cfg(test)]
mod tests {
    use super::*;

    // --- seek_steps_v2 -----------------------------------------------------

    #[test]
    fn seek_steps_v2_emit_correct_24bit_le_address() {
        // FLASH_APP_START = 0x000200 → LE = [0x00, 0x02, 0x00].
        let bytes = seek_steps_v2(0x000200).expect("FLASH_APP_START is valid");
        assert_eq!(bytes, vec![0x10, 0x00, 0x02, 0x00]);
    }

    #[test]
    fn seek_steps_v2_refuses_below_app_start() {
        assert_eq!(seek_steps_v2(0x0001FF), Err(FpError::AddressBelowAppStart));
        assert_eq!(seek_steps_v2(0), Err(FpError::AddressBelowAppStart));
    }

    #[test]
    fn seek_steps_v2_refuses_above_max() {
        assert_eq!(
            seek_steps_v2(FLASH_MAX_WORDS + 1),
            Err(FpError::AddressAboveFlashMax)
        );
    }

    // --- erase_steps_v2 ----------------------------------------------------

    #[test]
    fn erase_steps_v2_refuses_unaligned() {
        // 0x000201 is in app region but not page-aligned (0x1FF mask).
        assert_eq!(erase_steps_v2(0x000201), Err(FpError::UnalignedPageAddress));
    }

    #[test]
    fn erase_steps_v2_refuses_below_bootloader() {
        // Bootloader sectors sacred — no erase below FLASH_APP_START
        // even if address would otherwise be page-aligned.
        assert_eq!(erase_steps_v2(0x000000), Err(FpError::AddressBelowAppStart));
    }

    #[test]
    fn erase_steps_v2_accepts_valid_page() {
        // 0x000400 = 1024 words = page-aligned, in app region.
        let bytes = erase_steps_v2(0x000400).expect("valid page");
        assert_eq!(bytes, vec![0x11, 0x00, 0x04, 0x00]);
    }

    // --- write_steps_v2 ----------------------------------------------------

    #[test]
    fn write_steps_v2_correct_msb_first_byte_order() {
        // Two words: 0x123456 and 0xABCDEF.
        // Wire: [0x12, count_LE=0x02 0x00, 0x12, 0x34, 0x56, 0xAB, 0xCD, 0xEF].
        let bytes = write_steps_v2(&[0x12_3456, 0xAB_CDEF]).expect("valid words");
        assert_eq!(
            bytes,
            vec![0x12, 0x02, 0x00, 0x12, 0x34, 0x56, 0xAB, 0xCD, 0xEF]
        );
    }

    #[test]
    fn write_steps_v2_refuses_batch_above_16() {
        let words = vec![0u32; 17];
        assert_eq!(write_steps_v2(&words), Err(FpError::BatchTooLarge));
    }

    #[test]
    fn write_steps_v2_refuses_batch_zero() {
        assert_eq!(write_steps_v2(&[]), Err(FpError::BatchEmpty));
    }

    #[test]
    fn write_steps_v2_refuses_word_above_24bit() {
        assert_eq!(
            write_steps_v2(&[0x0100_0000]),
            Err(FpError::InvalidWordWidth)
        );
        assert_eq!(write_steps_v2(&[u32::MAX]), Err(FpError::InvalidWordWidth));
    }

    // --- CRC ---------------------------------------------------------------

    #[test]
    fn crc_host_msb_first_per_word_byte_order() {
        // compute_crc_host([0x12_3456]) MUST equal crc_itu_t_v41(&[0x12, 0x34, 0x56]).
        let host_crc = compute_crc_host(&[0x12_3456]);
        let raw_crc = crc_itu_t_v41(&[0x12, 0x34, 0x56]);
        assert_eq!(host_crc, raw_crc, "CRC must use MSB-first per 24-bit word");
    }

    // --- W15.A4  Q2 CRC ground-truth pinning -------------------------

    ///  Q2 TV1: `compute_crc_host(&[0xFA_0000])` must equal
    /// [`PIC1704_CRC_TV1`] = `0x2493`. Single-word vector pins the
    /// algorithm + per-word MSB-first byte order without needing any
    /// firmware hex file at test time.
    #[test]
    fn pic1704_crc_tv1_single_word_matches_re_finding() {
        assert_eq!(compute_crc_host(&[0x00FA_0000]), PIC1704_CRC_TV1);
        assert_eq!(PIC1704_CRC_TV1, 0x2493);
    }

    /// Algorithm self-consistency: `compute_crc_host(&[])` must equal
    /// the init value (`0x0000`) since CRC-ITU-T V.41 init is `0x0000`
    /// and there's no XOR-out. Catches a future refactor that flips the
    /// init to `0xFFFF` (which would make this CRC equivalent to
    /// `crate::protocol::crc16` CCITT-FALSE — a silent algorithm swap).
    #[test]
    fn pic1704_crc_empty_input_is_zero() {
        assert_eq!(compute_crc_host(&[]), 0x0000);
    }

    /// Pin the two known-good firmware CRC consts. The actual byte-exact
    /// match against the firmware hex file requires loading T9 / S11
    /// `dsPIC33EP16GS202_app.txt` at runtime — see the
    /// `PIC1704_CRC_T9_TV2`-`TV7` consts which encode the multi-word
    /// vectors that a future fixture-driven test can run against. This
    /// test simply pins the WAVE 5 Q2 numbers so a future "refactor"
    /// can't silently change them.
    #[test]
    fn pic1704_crc_known_good_consts_pinned() {
        assert_eq!(PIC1704_CRC_S11, 0xE638);
        assert_eq!(PIC1704_CRC_T9, 0xF9F4);
    }

    /// Pin every  Q2 numeric value to its const so a future
    /// re-export drift gets caught.
    #[test]
    fn pic1704_crc_test_vector_consts_pinned() {
        assert_eq!(PIC1704_CRC_TV1, 0x2493);
        assert_eq!(PIC1704_CRC_T9_TV2, 0xDB23);
        assert_eq!(PIC1704_CRC_T9_TV3, 0xFAB2);
        assert_eq!(PIC1704_CRC_T9_TV4, 0x6056);
        assert_eq!(PIC1704_CRC_T9_TV5, 0x34F5);
        assert_eq!(PIC1704_CRC_T9_TV7, 0x7119);
        assert_eq!(PIC1704_CRC_S11_TV1, 0xCAED);
    }

    #[test]
    fn verify_crc_response_decoded_le() {
        // Response bytes [0x12, 0x34] LE → 0x3412.
        // CRITICAL: response is LE per handoff line `pic1704_v2.c:194`:
        // `*crc = rbuf[0] | (rbuf[1] << 8)`.
        assert_eq!(decode_verify_response(&[0x12, 0x34]), 0x3412);
    }

    // --- START_APP + collision guard --------------------------------------

    #[test]
    fn start_app_v2_emits_0x14_0x01() {
        // Pin distinct from W11.7 unlock+jump pair (REG_VERSION=0x5A then
        // REG_CONTROL=0x01). Framed variant is single 2-byte write.
        assert_eq!(start_app_steps_v2(), vec![0x14, 0x01]);
    }

    #[test]
    fn collision_guard_refuses_app_version() {
        // App-mode versions MUST be refused — sending FP_SEEK (0x10) to an
        // app-mode chip would be interpreted as REG_VOLTAGE_L=0x10 write,
        // silent overvolt risk.
        assert!(collision_guard(VERSION_APP_88).is_err());
        assert!(collision_guard(VERSION_APP_89).is_err());
        assert!(collision_guard(VERSION_APP_8A).is_err());
        assert!(collision_guard(VERSION_BOOTLOADER).is_ok());
    }

    // --- Sanity cross-checks (host-only, no extra count contribution) -----

    #[test]
    fn ordinals_are_canonical() {
        // W4 handoff `pic1704_v2.h` ordinals — load-bearing protocol
        // fixtures. If any flips, a future executor's wire format would break
        // silently.
        assert_eq!(FP_SEEK, 0x10);
        assert_eq!(FP_ERASE_PAGE, 0x11);
        assert_eq!(FP_WRITE_WORDS, 0x12);
        assert_eq!(FP_VERIFY_CRC, 0x13);
        assert_eq!(FP_START_APP, 0x14);
        assert_eq!(FP_READ_VERSION, 0x15);
    }

    #[test]
    fn flash_geometry_constants_match_handoff() {
        assert_eq!(FLASH_PAGE_WORDS, 512);
        assert_eq!(FLASH_APP_START, 0x000200);
        assert_eq!(FLASH_MAX_WORDS, 8 * 512 - 256);
        assert_eq!(BATCH_MAX, 16);
    }
}
