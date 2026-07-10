//! PIC1704 stock bmminer reflash protocol — GHIDRA-EXTRACTED (W15.B).
//!
//! Implements the wire format extracted from Ghidra-decompiled stock
//! bmminer source for the PIC1704 reflash sequence. This module ships
//! ALONGSIDE [`super::programmer_v2`] (W14.C V2 theoretical/inferred
//! REG_CMD 0x10-0x15 framed protocol) — the two are routed by
//! [`super::reflash::route_by_seek_ack`].
//!
//! ## Source of truth
//!
//! Decoded from:
//!
//! - `Handoffs/DCENT_OS_GHIDRA_OUTPUTS/_bitmain_pic_seek_1704.c`
//! - `Handoffs/DCENT_OS_GHIDRA_OUTPUTS/_bitmain_pic_erase_1704.c`
//! - `Handoffs/DCENT_OS_GHIDRA_OUTPUTS/_bitmain_pic_write_1704.c`
//! - `Handoffs/DCENT_OS_GHIDRA_OUTPUTS/_update_pic_app_program_1704.c`
//! - `Handoffs/DCENT_OS_FULL_HANDOFF/DCENT_OS_HANDOFF/RE_TEAM_WAVE5B_HANDOFF.md` Q4
//!
//! ## Wire format vs W14.C V2
//!
//! | Aspect            | W14.C V2 (programmer_v2)        | Stock (programmer_stock)        |
//! |-------------------|---------------------------------|---------------------------------|
//! | Magic             | none (REG_CMD prefix only)      | `0x55` first byte               |
//! | Address layout    | 24-bit LE (3 bytes after 0x10)  | 16-bit address packed in packet |
//! | Erase marker      | REG_CMD 0x11                    | `0xAA` second byte              |
//! | Write phases      | single phase (`0x12 + count + data`) | TWO phases (data + commit) |
//! | Checksum          | CRC-ITU-T V.41 (poly 0x1021)    | Additive sum over payload       |
//! | Inter-phase delay | 5 ms                            | **300 ms** (NOT 300 µs)         |
//! | Erase batch       | 512-word page                   | 32 words at a time              |
//! | Write batch       | up to 16 words (3 bytes each)   | 16 BYTES at a time              |
//! | ACK signature     | 2 bytes pattern-specific        | 2 bytes pattern-specific        |
//!
//! ## Critical errata
//!
//! 1. `usleep(300000)` is **300 milliseconds**, NOT 300 microseconds.
//!    The W4 handoff `dspic_framed_reflash_protocol.md` was wrong on
//!    this point — the Ghidra source is ground truth.
//! 2. WRITE phase 1 transfers **16 BYTES** (not 16 words). Each outer
//!    iteration of `_update_pic_app_program_1704` packs 256 bytes
//!    (16 batches × 16 bytes) — see Q4 errata in W5b handoff.
//! 3. Stock SEEK address is in **WORDS** (per Ghidra start address `0x600`
//!    in `_update_pic_app_program_1704` mapped to a 1536-word firmware
//!    region).
//! 4. Each 16-bit firmware word is packed **MSB-first** as 2 bytes
//!    (NOT 24-bit / 3 bytes) in the program-memory buffer
//!    (`uStack_13b8`), per `_update_pic_app_program_1704.c` lines 64-65.
//!
//! ## Recovery-tool feature gate (TRIPLE)
//!
//! 1. Module wrapped in `#![cfg(feature = "recovery-tool")]`.
//! 2. Production `dcentrald` Cargo.toml does NOT enable `recovery-tool`.
//! 3. CLI subcommand requires `--confirm-bricked` token + `--manifest`.
//!
//! ## REG_VOLTAGE_L collision guard
//!
//! Stock SEEK leading byte `0x55` does NOT collide with REG_VOLTAGE_L
//! (`0x10`) — but the W14.C V2 collision guard contract still applies
//! at the CLI layer. Re-export [`super::programmer_v2::collision_guard`]
//! so callers can pre-read REG_VERSION before any framed transaction.

#![cfg(feature = "recovery-tool")]

use std::path::Path;

// === Magic bytes + framing constants ===

/// Stock-protocol magic byte (first byte of every packet).
pub const STOCK_MAGIC: u8 = 0x55;

/// ERASE marker byte (second byte of ERASE packet).
pub const STOCK_ERASE_MARKER: u8 = 0xAA;

/// WRITE phase 2 commit trailer byte (also acts as the additive-sum
/// initial value for phase 2: `local_e = 9` per Ghidra
/// `_bitmain_pic_write_1704.c:59`).
pub const STOCK_WRITE_PHASE2_TRAILER: u8 = 0x09;

/// Inter-phase wait between every TX and the response read.
///
/// **CRITICAL**: this is 300 milliseconds, NOT 300 microseconds.
/// Source: every Ghidra `usleep(300000)` call across SEEK / ERASE /
/// WRITE in the stock bmminer 1704 reflash sequence. The W4 handoff
/// `dspic_framed_reflash_protocol.md` doc had this wrong.
pub const STOCK_INTER_PHASE_MS: u64 = 300;

/// ERASE outer-loop stride: each ERASE iteration covers 32 words of
/// program memory (`local_828 -= 0x20` per Ghidra
/// `_bitmain_pic_erase_1704.c:69`).
pub const STOCK_ERASE_BATCH_WORDS: u32 = 32;

/// WRITE phase 1 batch size in **BYTES** (NOT words). Each phase 1
/// packet carries 16 raw bytes (`local_14 < 0x10` per Ghidra
/// `_bitmain_pic_write_1704.c:41`).
///
/// W4 handoff erratum: the stock protocol uses 16 BYTES, not 16 words
/// like the W14.C V2 framed protocol. Q4 errata in W5b handoff confirms.
pub const STOCK_WRITE_BATCH_BYTES: usize = 16;

/// SEEK initial additive-sum offset.
/// Source: `local_e = (ushort)local_12 + (ushort)local_13 + 7;` in
/// `_bitmain_pic_seek_1704.c:30`. The literal `+ 7` corresponds to the
/// number of zero bytes in the packet skeleton plus the magic.
pub const STOCK_SEEK_CK_INIT: u16 = 7;

/// ERASE per-iteration additive-sum increment offset.
/// Source: `local_12 = local_13 + 4;` in `_bitmain_pic_erase_1704.c:29`.
pub const STOCK_ERASE_CK_INC: u8 = 4;

/// ERASE first-iteration `local_13` constant.
/// Source: `local_13 = 4;` in `_bitmain_pic_erase_1704.c:19`.
pub const STOCK_ERASE_LOCAL_13_INIT: u8 = 4;

/// WRITE phase 1 initial additive-sum value.
/// Source: `local_e = 0x16;` in `_bitmain_pic_write_1704.c:36`.
/// `0x16` (= 22) corresponds to the total packet length (1 magic + 3
/// zero + 16 data + 2 ck = 22 bytes).
pub const STOCK_WRITE_PHASE1_CK_INIT: u16 = 0x16;

/// WRITE phase 1 packet total length in bytes.
pub const STOCK_WRITE_PHASE1_BYTES: usize = 22;

/// WRITE phase 2 packet total length in bytes.
pub const STOCK_WRITE_PHASE2_BYTES: usize = 6;

/// SEEK packet total length in bytes.
pub const STOCK_SEEK_BYTES: usize = 8;

/// ERASE packet total length in bytes.
pub const STOCK_ERASE_BYTES: usize = 6;

// === ACK signatures ===
//
// All four come from the post-`usleep(300000)` 2-byte read-back checks
// in the Ghidra source — see `_bitmain_pic_seek_1704.c:41`,
// `_bitmain_pic_erase_1704.c:62`, and `_bitmain_pic_write_1704.c:57`
// (phase 1) + `:70` (phase 2).

/// Expected SEEK ACK: `[0x01, 0x01]`.
pub const ACK_SEEK: [u8; 2] = [0x01, 0x01];

/// Expected ERASE ACK: `[0x04, 0x01]`.
pub const ACK_ERASE: [u8; 2] = [0x04, 0x01];

/// Expected WRITE phase 1 ACK: `[0x02, 0x01]`.
pub const ACK_WRITE_PHASE1: [u8; 2] = [0x02, 0x01];

/// Expected WRITE phase 2 ACK: `[0x05, 0x01]`.
pub const ACK_WRITE_PHASE2: [u8; 2] = [0x05, 0x01];

// === Geometry constants (mirror W14.C for cross-protocol agreement) ===

/// Application start address (in WORDS, per stock SEEK semantics).
/// `_update_pic_app_program_1704` sends `local_2c = 0x600` which is the
/// stock app-region word index. Pinned here as a sanity reference; the
/// host-side `seek_steps_stock` validator does NOT enforce it because
/// the stock protocol's address space differs from W14.C V2 (which
/// validates against [`super::programmer_v2::FLASH_APP_START`]).
pub const STOCK_APP_START_WORDS: u32 = 0x0600;

/// Maximum word value the stock SEEK can accept (16-bit unsigned cap).
/// Stock packet only carries 16 bits of address (high byte at packet
/// offset 4, low byte not separately stored — see `seek_steps_stock`).
pub const STOCK_SEEK_MAX_WORDS: u32 = 0xFFFF;

// === Errors ===

/// Errors emitted by host-side wire-format helpers in this module.
///
/// I²C transport errors (NACK / EIO / etc.) are surfaced via the CLI
/// transport layer; this enum is for pre-wire validation only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockError {
    /// SEEK address exceeds the 16-bit address envelope the stock
    /// packet can carry.
    SeekAddressOutOfRange,
    /// ERASE iteration index would overflow the per-iteration `local_13`
    /// increment counter (8-bit unsigned).
    EraseIterationOverflow,
    /// WRITE batch is not exactly 16 bytes long.
    WriteBatchWrongSize,
    /// Bootloader version pre-check returned a non-bootloader byte.
    /// Mirrors [`super::programmer_v2::FpError::NotInBootloader`] so
    /// the CLI layer can use a unified error path.
    NotInBootloader,
    /// Hex application file failed to parse.
    HexParseError,
    /// Hex application file exceeded the firmware-region word count
    /// expected by `_update_pic_app_program_1704` (1536 words).
    HexTooLarge,
}

// === Wire-format helpers ===

/// SEEK command bytes (8 bytes total).
///
/// Wire layout per Ghidra `_bitmain_pic_seek_1704.c`:
///
/// ```text
/// byte 0: 0x55           (STOCK_MAGIC; line 32 `local_24._0_1_ = 0x55`)
/// byte 1: 0              (zero from `local_24 = 0` init)
/// byte 2: 0              (zero from `local_24 = 0` init)
/// byte 3: 0              (zero from `local_24 = 0` init)
/// byte 4: addr_hi        (line 34 `local_20._0_1_ = local_12 = addr>>8`)
/// byte 5: 0              (zero from `local_20 = 0` init)
/// byte 6: ck_hi          (additive-sum high byte)
/// byte 7: ck_lo          (additive-sum low byte)
/// ```
///
/// Checksum: `local_e = addr_hi + addr_lo + STOCK_SEEK_CK_INIT (7)`
/// per Ghidra line 30. The Ghidra decompilation has WARNINGs about
/// "Ignoring partial resolution of indirect" — the checksum store is
/// elided in the C view but the W15.B task spec confirms ck lands at
/// packet bytes 6-7 (the trailing 2 bytes of the 8-byte packet).
pub fn seek_steps_stock(address: u32) -> Result<Vec<u8>, StockError> {
    if address > STOCK_SEEK_MAX_WORDS {
        return Err(StockError::SeekAddressOutOfRange);
    }
    let addr_hi = ((address >> 8) & 0xFF) as u8;
    let addr_lo = (address & 0xFF) as u8;
    let ck = (addr_hi as u16)
        .wrapping_add(addr_lo as u16)
        .wrapping_add(STOCK_SEEK_CK_INIT);
    let ck_hi = ((ck >> 8) & 0xFF) as u8;
    let ck_lo = (ck & 0xFF) as u8;
    Ok(vec![
        STOCK_MAGIC, // byte 0
        0,           // byte 1
        0,           // byte 2
        0,           // byte 3
        addr_hi,     // byte 4 (per Ghidra line 34)
        0,           // byte 5
        ck_hi,       // byte 6
        ck_lo,       // byte 7
    ])
}

/// ERASE command bytes for one outer-loop iteration (6 bytes total).
///
/// Wire layout per Ghidra `_bitmain_pic_erase_1704.c` (lines 32-47):
///
/// ```text
/// byte 0: 0x55           (STOCK_MAGIC)
/// byte 1: 0xAA           (STOCK_ERASE_MARKER)
/// byte 2: local_13       (iteration counter; starts at 4, increments by 4)
/// byte 3: 0x04           (literal `4` per line 41)
/// byte 4: ck_hi          (high byte of `local_12 = local_13 + 4`)
/// byte 5: ck_lo          (low byte of `local_12 = local_13 + 4`)
/// ```
///
/// `iteration_index` is the 0-based outer-loop index (0, 1, 2, ...).
/// The `local_13` byte placed at packet offset 2 is computed as
/// `STOCK_ERASE_LOCAL_13_INIT (4) + iteration_index * STOCK_ERASE_CK_INC (4)`.
pub fn erase_steps_stock(iteration_index: u8) -> Result<Vec<u8>, StockError> {
    let local_13 = (STOCK_ERASE_LOCAL_13_INIT as u32)
        .checked_add(
            (iteration_index as u32)
                .checked_mul(STOCK_ERASE_CK_INC as u32)
                .ok_or(StockError::EraseIterationOverflow)?,
        )
        .ok_or(StockError::EraseIterationOverflow)?;
    if local_13 > u8::MAX as u32 {
        return Err(StockError::EraseIterationOverflow);
    }
    let local_13 = local_13 as u8;
    let local_12 = (local_13 as u16).wrapping_add(STOCK_ERASE_CK_INC as u16);
    let ck_hi = ((local_12 >> 8) & 0xFF) as u8;
    let ck_lo = (local_12 & 0xFF) as u8;
    Ok(vec![
        STOCK_MAGIC,        // byte 0
        STOCK_ERASE_MARKER, // byte 1
        local_13,           // byte 2
        0x04,               // byte 3 (literal `4` per Ghidra line 41)
        ck_hi,              // byte 4
        ck_lo,              // byte 5
    ])
}

/// WRITE phase 1 command bytes (22 bytes total).
///
/// Wire layout per Ghidra `_bitmain_pic_write_1704.c` (lines 36-51):
///
/// ```text
/// byte 0:  0x55          (STOCK_MAGIC; line 38)
/// byte 1:  0             (cleared by memset line 40)
/// byte 2:  0             (cleared by memset line 40)
/// byte 3:  0             (cleared by memset line 40)
/// byte 4:  data[0]       (loop line 41-45 copies 16 bytes from caller)
/// byte 5:  data[1]
/// ...
/// byte 19: data[15]
/// byte 20: ck_hi         (line 48: `local_e >> 8` at offset `local_16 + 0x10 = 0x14`)
/// byte 21: ck_lo         (line 51: `local_e` at offset `local_16 + 1 = 0x15`)
/// ```
///
/// Initial sum: `local_e = STOCK_WRITE_PHASE1_CK_INIT (0x16 = 22)`,
/// then accumulate every data byte: `local_e = data[i] + local_e;`
/// per line 44.
pub fn write_phase1_steps_stock(data: &[u8; 16]) -> Result<Vec<u8>, StockError> {
    if data.len() != STOCK_WRITE_BATCH_BYTES {
        // Compile-time guarantee from `&[u8; 16]`, but defensive in
        // case caller has a mutable view.
        return Err(StockError::WriteBatchWrongSize);
    }
    let mut packet = vec![0u8; STOCK_WRITE_PHASE1_BYTES];
    packet[0] = STOCK_MAGIC;
    // bytes 1..=3 stay zero (memset)
    packet[4..20].copy_from_slice(data);
    let mut ck: u16 = STOCK_WRITE_PHASE1_CK_INIT;
    for &b in data.iter() {
        ck = ck.wrapping_add(b as u16);
    }
    packet[20] = ((ck >> 8) & 0xFF) as u8;
    packet[21] = (ck & 0xFF) as u8;
    Ok(packet)
}

/// WRITE phase 2 commit packet (6 bytes, fixed contents).
///
/// Wire layout per Ghidra `_bitmain_pic_write_1704.c` (lines 58-65):
///
/// ```text
/// byte 0: 0x55                          (STOCK_MAGIC; line 61)
/// byte 1: 0                             (line 63 `local_34._0_1_ = 0`)
/// byte 2: 0                             (zero from prior memset)
/// byte 3: 0                             (zero from prior memset)
/// byte 4: 0                             (zero from prior memset)
/// byte 5: STOCK_WRITE_PHASE2_TRAILER    (= 0x09; corresponds to `local_e = 9` initial per line 59,
///                                        which the protocol places at the trailing byte)
/// ```
///
/// Total packet length is 6 bytes (`local_16 = 6` line 64).
pub fn write_phase2_steps_stock() -> Vec<u8> {
    vec![
        STOCK_MAGIC,                // byte 0
        0,                          // byte 1
        0,                          // byte 2
        0,                          // byte 3
        0,                          // byte 4
        STOCK_WRITE_PHASE2_TRAILER, // byte 5
    ]
}

/// Compute the additive-sum checksum used by stock SEEK / ERASE /
/// WRITE-phase-1 packets.
///
/// `initial` is the per-packet initial value (`STOCK_SEEK_CK_INIT` /
/// `STOCK_ERASE_CK_INC` / `STOCK_WRITE_PHASE1_CK_INIT`). Returns the
/// 16-bit additive sum (no XOR-out, wrapping addition). The high byte
/// goes into the packet's `ck_hi` slot, low byte into `ck_lo`.
pub fn compute_checksum_stock(payload: &[u8], initial: u16) -> u16 {
    let mut ck = initial;
    for &b in payload {
        ck = ck.wrapping_add(b as u16);
    }
    ck
}

/// Pack a slice of 16-bit firmware words into a byte vector,
/// MSB-first per word.
///
/// Source: `_update_pic_app_program_1704.c` lines 64-65:
///
/// ```text
/// *(char *)((int)&uStack_13b8 + local_10 * 2)     = (char)(local_20 >> 8);
/// *(char *)((int)&uStack_13b8 + local_10 * 2 + 1) = (char)local_20;
/// ```
///
/// Each 16-bit word `0xABCD` becomes the two bytes `[0xAB, 0xCD]`.
/// Returns a vector of length `2 * words.len()` bytes.
///
/// **Errata**: this is 16-bit / 2-byte packing, NOT 24-bit / 3-byte
/// packing as the W4 V2 protocol uses. Q4 of W5b handoff confirms.
pub fn pack_words_msb_first(words: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * words.len());
    for &w in words {
        out.push(((w >> 8) & 0xFF) as u8);
        out.push((w & 0xFF) as u8);
    }
    out
}

/// Parse a stock-format hex application file (one 16-bit hex word per
/// line, blank and comment lines skipped). Used by
/// [`super::reflash`] CLI as a host-side helper for tests; production
/// CLI in pic-recovery binary loads the file directly.
///
/// Returns the raw 16-bit words in order. Caller is responsible for
/// `pack_words_msb_first` to convert to wire bytes.
pub fn parse_hex_app_file(path: &Path) -> Result<Vec<u16>, StockError> {
    let text = std::fs::read_to_string(path).map_err(|_| StockError::HexParseError)?;
    let mut words = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let body = line.trim_start_matches(':');
        let hex4: String = body
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(4)
            .collect();
        if hex4.len() != 4 {
            return Err(StockError::HexParseError);
        }
        let w = u16::from_str_radix(&hex4, 16).map_err(|_| StockError::HexParseError)?;
        words.push(w);
    }
    Ok(words)
}

/// Re-export the W14.C V2 collision guard so CLI callers don't need to
/// reach across modules for the shared safety check. The guard refuses
/// any framed transaction unless REG_VERSION reads as bootloader (0x86).
pub use super::programmer_v2::collision_guard;

// === Tests (host-safe, no I²C bus required) ===

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    // --- Magic + framing constants ---

    #[test]
    fn stock_magic_pinned() {
        assert_eq!(STOCK_MAGIC, 0x55);
    }

    #[test]
    fn stock_erase_marker_pinned() {
        assert_eq!(STOCK_ERASE_MARKER, 0xAA);
    }

    #[test]
    fn inter_phase_delay_is_milliseconds_not_microseconds() {
        // Sanity-check the const and document the W4 handoff erratum:
        // `usleep(300000)` is 300 ms, NOT 300 µs. Future refactor that
        // changes this to a Duration::from_micros call would silently
        // accelerate the protocol 1000×.
        assert_eq!(STOCK_INTER_PHASE_MS, 300);
        assert!(STOCK_INTER_PHASE_MS >= 100, "must be at least 100 ms");
    }

    #[test]
    fn erase_batch_size_is_32_words() {
        assert_eq!(STOCK_ERASE_BATCH_WORDS, 32);
    }

    #[test]
    fn write_batch_size_is_16_bytes_not_words() {
        // W4 handoff erratum (Q4 of W5b handoff): stock uses 16 BYTES,
        // not 16 WORDS like the W14.C V2 framed protocol. Pinning this
        // const to its byte-semantics catches a future drift to
        // `BATCH_MAX (16 words = 48 bytes for V2)`.
        assert_eq!(STOCK_WRITE_BATCH_BYTES, 16);
    }

    #[test]
    fn seek_address_in_words_not_bytes() {
        // Stock SEEK accepts a word index. The chip multiplies internally
        // (16-bit words → 2-byte stride). Validate here that 0x0600
        // (the canonical _update_pic_app_program_1704 start) is below
        // STOCK_SEEK_MAX_WORDS so the validator accepts it.
        assert_eq!(STOCK_APP_START_WORDS, 0x0600);
        assert!(STOCK_APP_START_WORDS <= STOCK_SEEK_MAX_WORDS);
    }

    // --- ACK signatures ---

    #[test]
    fn ack_signatures_pinned() {
        assert_eq!(ACK_SEEK, [0x01, 0x01]);
        assert_eq!(ACK_ERASE, [0x04, 0x01]);
        assert_eq!(ACK_WRITE_PHASE1, [0x02, 0x01]);
        assert_eq!(ACK_WRITE_PHASE2, [0x05, 0x01]);
    }

    // --- SEEK ---

    #[test]
    fn seek_steps_stock_for_0x600_matches_ghidra() {
        // Per Ghidra _bitmain_pic_seek_1704.c with address = 0x600:
        //   local_12 = 0x06 (high byte)
        //   local_13 = 0x00 (low byte)
        //   local_e  = 0x06 + 0x00 + 7 = 0x0D
        //
        // Packet:
        //   byte 0: 0x55
        //   byte 1-3: 0 (uninit)
        //   byte 4: 0x06 (addr_hi at line 34 `local_20._0_1_ = local_12`)
        //   byte 5: 0
        //   byte 6: 0x00 (ck_hi)
        //   byte 7: 0x0D (ck_lo)
        let bytes = seek_steps_stock(0x0600).expect("0x600 in range");
        assert_eq!(bytes, vec![0x55, 0, 0, 0, 0x06, 0, 0x00, 0x0D]);
        assert_eq!(bytes.len(), STOCK_SEEK_BYTES);
    }

    #[test]
    fn seek_steps_stock_checksum_includes_offset_7() {
        // Address 0x0000 → ck = 0 + 0 + 7 = 7. Pins the +7 offset.
        let bytes = seek_steps_stock(0x0000).unwrap();
        assert_eq!(bytes[6], 0x00);
        assert_eq!(bytes[7], 0x07);
    }

    #[test]
    fn seek_steps_stock_byte_0_is_magic_and_byte_4_is_addr_hi() {
        let bytes = seek_steps_stock(0xAB12).unwrap();
        assert_eq!(bytes[0], STOCK_MAGIC);
        assert_eq!(bytes[4], 0xAB);
    }

    #[test]
    fn seek_steps_stock_refuses_above_16bit() {
        // 0x10000 is one above the 16-bit max — stock packet only carries
        // 16 bits of address envelope.
        assert_eq!(
            seek_steps_stock(0x1_0000),
            Err(StockError::SeekAddressOutOfRange)
        );
    }

    // --- ERASE ---

    #[test]
    fn erase_steps_stock_first_iteration_matches_ghidra() {
        // Iteration 0 → local_13 = 4, local_12 = 8. Packet:
        //   byte 0: 0x55
        //   byte 1: 0xAA
        //   byte 2: 0x04 (local_13)
        //   byte 3: 0x04 (literal)
        //   byte 4: 0x00 (ck_hi)
        //   byte 5: 0x08 (ck_lo)
        let bytes = erase_steps_stock(0).expect("iter 0");
        assert_eq!(bytes, vec![0x55, 0xAA, 0x04, 0x04, 0x00, 0x08]);
        assert_eq!(bytes.len(), STOCK_ERASE_BYTES);
    }

    #[test]
    fn erase_steps_stock_increments_iteration_marker() {
        // Iteration 1 → local_13 = 8, local_12 = 12 (0x0C).
        let bytes = erase_steps_stock(1).expect("iter 1");
        assert_eq!(bytes[2], 0x08, "byte 2 = local_13 = 4 + 1*4 = 8");
        assert_eq!(bytes[5], 0x0C, "ck_lo = local_12 = 8 + 4 = 12");

        // Iteration 5 → local_13 = 24 (0x18), local_12 = 28 (0x1C).
        let bytes5 = erase_steps_stock(5).expect("iter 5");
        assert_eq!(bytes5[2], 0x18);
        assert_eq!(bytes5[5], 0x1C);
    }

    #[test]
    fn erase_steps_stock_overflows_at_iteration_63() {
        // local_13 = 4 + 63*4 = 256, which overflows u8. Refuse.
        // (Stock bmminer never erases that many pages — 1536 words /
        // 32-word batch = 48 outer iterations max — but the validator
        // catches a future caller bug.)
        assert_eq!(
            erase_steps_stock(63),
            Err(StockError::EraseIterationOverflow)
        );
    }

    // --- WRITE phase 1 ---

    #[test]
    fn write_phase1_steps_stock_22_bytes() {
        let data = [0u8; 16];
        let bytes = write_phase1_steps_stock(&data).unwrap();
        assert_eq!(bytes.len(), STOCK_WRITE_PHASE1_BYTES);
        assert_eq!(bytes.len(), 22);
    }

    #[test]
    fn write_phase1_steps_stock_byte_layout() {
        // Data = [0x10, 0x11, ..., 0x1F]. After the loop:
        //   byte 0: 0x55
        //   bytes 1-3: 0 (memset)
        //   bytes 4-19: data verbatim
        //   bytes 20-21: ck (initial 0x16 + sum of data 0x10..0x1F)
        let data: [u8; 16] = [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D,
            0x1E, 0x1F,
        ];
        let bytes = write_phase1_steps_stock(&data).unwrap();
        assert_eq!(bytes[0], 0x55);
        assert_eq!(bytes[1], 0);
        assert_eq!(bytes[2], 0);
        assert_eq!(bytes[3], 0);
        assert_eq!(&bytes[4..20], &data[..]);
        // Sum: 0x10 + 0x11 + ... + 0x1F = 16 bytes summing to 0x178.
        // Plus initial 0x16. Total: 0x178 + 0x16 = 0x18E.
        let expected_ck: u16 = data.iter().map(|&b| b as u16).sum::<u16>() + 0x16;
        assert_eq!(expected_ck, 0x018E);
        assert_eq!(bytes[20], ((expected_ck >> 8) & 0xFF) as u8); // ck_hi
        assert_eq!(bytes[21], (expected_ck & 0xFF) as u8); // ck_lo
        assert_eq!(bytes[20], 0x01);
        assert_eq!(bytes[21], 0x8E);
    }

    #[test]
    fn write_phase1_steps_stock_checksum_initial_22() {
        // All-zero data → ck = 0x16 (initial only).
        let data = [0u8; 16];
        let bytes = write_phase1_steps_stock(&data).unwrap();
        assert_eq!(bytes[20], 0x00, "ck_hi for zero data + 0x16 init");
        assert_eq!(bytes[21], 0x16, "ck_lo = STOCK_WRITE_PHASE1_CK_INIT");
    }

    #[test]
    fn write_phase1_steps_stock_checksum_includes_data() {
        // Single-bit data difference must shift ck by exactly 1.
        let mut data = [0u8; 16];
        let zero_bytes = write_phase1_steps_stock(&data).unwrap();
        data[7] = 1; // flip one byte
        let one_bytes = write_phase1_steps_stock(&data).unwrap();
        let zero_ck = ((zero_bytes[20] as u16) << 8) | zero_bytes[21] as u16;
        let one_ck = ((one_bytes[20] as u16) << 8) | one_bytes[21] as u16;
        assert_eq!(one_ck.wrapping_sub(zero_ck), 1);
    }

    // --- WRITE phase 2 ---

    #[test]
    fn write_phase2_steps_stock_fixed_6_bytes() {
        let bytes = write_phase2_steps_stock();
        assert_eq!(bytes, vec![0x55, 0, 0, 0, 0, 0x09]);
        assert_eq!(bytes.len(), STOCK_WRITE_PHASE2_BYTES);
    }

    #[test]
    fn write_phase2_steps_stock_trailer_is_local_e_initial() {
        // Per Ghidra _bitmain_pic_write_1704.c:59 `local_e = 9;`. The
        // 0x09 trailer corresponds to that initial sum value being
        // committed at the packet's trailing byte position.
        let bytes = write_phase2_steps_stock();
        assert_eq!(bytes[5], STOCK_WRITE_PHASE2_TRAILER);
        assert_eq!(STOCK_WRITE_PHASE2_TRAILER, 0x09);
    }

    // --- compute_checksum_stock ---

    #[test]
    fn compute_checksum_additive_sum_pinned() {
        // [1, 2, 3] with initial 7 → 1 + 2 + 3 + 7 = 13.
        assert_eq!(compute_checksum_stock(&[1, 2, 3], 7), 13);
        // Empty payload returns the initial value.
        assert_eq!(compute_checksum_stock(&[], 22), 22);
        // Wrapping behaviour: 0xFFFF + 1 wraps to 0.
        assert_eq!(compute_checksum_stock(&[1], 0xFFFF), 0);
    }

    // --- pack_words_msb_first ---

    #[test]
    fn pack_words_msb_first_byte_order() {
        // 0x1234 → [0x12, 0x34] (MSB first per Ghidra
        // _update_pic_app_program_1704 lines 64-65).
        assert_eq!(pack_words_msb_first(&[0x1234]), vec![0x12, 0x34]);
        assert_eq!(
            pack_words_msb_first(&[0xABCD, 0xEF01]),
            vec![0xAB, 0xCD, 0xEF, 0x01]
        );
        assert_eq!(pack_words_msb_first(&[]), Vec::<u8>::new());
    }

    #[test]
    fn pack_words_msb_first_length_is_2x() {
        // Pin the 2-byte stride. Q4 W5b handoff erratum: stock uses
        // 16-bit words / 2 bytes per word, NOT 24-bit / 3 bytes per
        // word like the W14.C V2 framed protocol.
        let words = vec![0u16; 1024];
        assert_eq!(pack_words_msb_first(&words).len(), 2048);
    }

    // --- parse_hex_app_file (fixture-driven) ---

    #[test]
    fn parse_hex_app_file_basic() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("w15b_stock_hex_basic_{}.txt", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp hex");
        writeln!(f, "1234").unwrap();
        writeln!(f, "ABCD").unwrap();
        writeln!(f, "# comment line").unwrap();
        writeln!(f).unwrap(); // blank line
        writeln!(f, "0001").unwrap();
        drop(f);
        let words = parse_hex_app_file(&path).expect("parse ok");
        assert_eq!(words, vec![0x1234, 0xABCD, 0x0001]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_hex_app_file_rejects_invalid() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("w15b_stock_hex_invalid_{}.txt", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp hex");
        writeln!(f, "ZZZZ").unwrap();
        drop(f);
        let r = parse_hex_app_file(&path);
        assert_eq!(r, Err(StockError::HexParseError));
        let _ = std::fs::remove_file(&path);
    }

    // --- collision_guard re-export ---

    #[test]
    fn collision_guard_re_export_works() {
        // Same contract as super::programmer_v2: refuse non-bootloader.
        // VERSION_BOOTLOADER (0x86) accepted, app versions refused.
        use super::super::programmer_v2::{VERSION_APP_88, VERSION_APP_89, VERSION_BOOTLOADER};
        assert!(collision_guard(VERSION_BOOTLOADER).is_ok());
        assert!(collision_guard(VERSION_APP_88).is_err());
        assert!(collision_guard(VERSION_APP_89).is_err());
    }

    // --- Cross-checks ---

    #[test]
    fn packet_lengths_consistent_with_constants() {
        assert_eq!(seek_steps_stock(0).unwrap().len(), STOCK_SEEK_BYTES);
        assert_eq!(erase_steps_stock(0).unwrap().len(), STOCK_ERASE_BYTES);
        assert_eq!(
            write_phase1_steps_stock(&[0u8; 16]).unwrap().len(),
            STOCK_WRITE_PHASE1_BYTES
        );
        assert_eq!(write_phase2_steps_stock().len(), STOCK_WRITE_PHASE2_BYTES);
    }

    #[test]
    fn stock_protocol_distinct_from_w14c_v2() {
        // Sanity guard: if a future refactor merges the two modules,
        // these constants would silently collide. Pin both.
        use super::super::programmer_v2::{FP_SEEK, FP_WRITE_WORDS};
        assert_ne!(
            STOCK_MAGIC, FP_SEEK,
            "STOCK_MAGIC (0x55) must differ from V2 FP_SEEK (0x10)"
        );
        assert_ne!(
            STOCK_MAGIC, FP_WRITE_WORDS,
            "STOCK_MAGIC (0x55) must differ from V2 FP_WRITE_WORDS (0x12)"
        );
    }
}
