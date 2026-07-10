//! BM1362 stock-firmware wire format codec for `uart_trans.ko` transport.
//!
//! W14.B (R4-CONFIRMED): Stock Bitmain firmware on CV1835 / AM335x BB
//! decodes BM1362 wire frames as the 86-byte `asic_work_t` structure
//! described in W4 RE `bm1362_frames_v2.h` + `_summary.md`. This module
//! provides codec-only round-trip serialization for parity validation
//! against stock-firmware-emitted frames.
//!
//! ## CODEC ONLY — this module is the byte codec; the live transport
//! lives in [`super::uart_transport`]
//!
//! - NO `uart_trans.ko` kernel-module port (per
//!    +  decision #2:
//!   UIO + userspace, no kernel modules).
//! - NO `uart_trans.ko` IOCTL adapter (W4 handoff did not provide
//!   numeric IOCTL ordinals; covered by
//!   ).
//! - The on-wire frame is the 86-byte `asic_work_t`
//!   ([`AsicWorkFrame::to_bytes`] / [`wire_frame_bytes`]). The
//!   "168-byte `pack_asic_work`" in the W4 handoff was the
//!   *kernel-internal* buffer (86 B wire frame + 82 B kernel metadata:
//!   chain index, dispatch timestamp, retry count, ring-slot index) per
//!    — NOT what goes on the
//!   UART wire. [`PACK_WORK_SIZE`] is exposed for parity diagnostics
//!   only; there is no 168-byte send-buffer writer.
//! - This module (the byte codec) is NOT itself linked into any
//!   sustained-mining cold-boot path. The clean-room AM335x BB live
//!   work-dispatch transport that *uses* this codec is
//!   [`super::uart_transport::Am335xUartTransport`].
//!
//! ## Sealed-trait marker
//!
//! [`UartTransTransport`] is a sealed trait — only [`StockUartTrans`]
//! implements it within this crate. External crates cannot add new
//! impls; this prevents the new codec from leaking into the existing
//! direct-UART path (`drivers::bm1362::send_work`) by accident.
//!
//! ## CRC provenance — TWO DIFFERENT CRC SURFACES, ONE POLYNOMIAL (R11-3)
//!
//! There are two distinct CRC-bearing surfaces in the BM1362 stack and
//! they must never be conflated. PR-007 / R11-3 exists because a memory
//! note made it *look* like they contradict; they do not.
//!
//! 1. **Live chip-UART work/nonce frames** (the 5-platform-proven path,
//!    NOT this module): the 88-byte `[0x55 0xAA][0x21][0x56][82-byte
//!    full-header payload][CRC16 BE]` frame built by
//!    `dcentrald_asic::drivers::bm1362::build_serial_work_frame` /
//!    `dcentrald::am3_bb_mining::build_bm1362_serial_work_frame`. Its
//!    CRC is **CRC-16/CCITT-FALSE** (poly `0x1021`, init `0xFFFF`,
//!    refin=false, refout=false, xorout=`0x0000`). Accepted-share
//!    proven on am3-aml / am3-bb `a lab unit` / am2-XIL `a lab unit`. DO NOT TOUCH.
//!
//! 2. **Stock `uart_trans.ko` kernel-internal 86-byte `asic_work_t`
//!    framing** (what THIS codec mirrors, for byte-parity diagnostics
//!    against a stock-firmware bench unit): the stock kernel module's
//!    decompiled CRC lookup table was read directly out of `.rodata`
//!    during RE — `[0x0000, 0x1021, 0x2042, 0x3063, …]` (BINARY_RE_
//!    REPORT.md §2.3, `re_uart_text.py:51-54`). That is the **forward,
//!    NON-reflected CRC-CCITT table** (poly `0x1021`). A reflected
//!    IBM-SDLC / CRC-16/X-25 table would instead start `[0x0000,
//!    0x1189, 0x2312, …]`. The Linux *symbol name* is `crc_itu_t_table`
//!    — but the kernel's `crc_itu_t()` is a forward (non-reflected)
//!    poly-0x1021 routine; the "ITU-T" name does NOT imply the reflected
//!    IBM-SDLC parameter set.
//!
//! Therefore: **both surfaces use the same CCITT-family poly `0x1021`,
//! non-reflected.** This module deliberately reuses
//! [`crate::protocol::crc16`] (CCITT-FALSE) — that matches the
//! decompiled stock table. The claim in
//!  that the stock module is
//! "IBM-SDLC (refin/refout=true, xorout=0xFFFF)" is an *inference from
//! the Linux symbol name*, NOT a DWARF/decompiled fact — the only hard
//! CRC evidence in the RE corpus (the `.rodata` table dump) is the
//! forward CCITT table.
//! §"CRC: CCITT-FALSE, NOT IBM-SDLC" and the R7-3 wire capture
//! (:166`).
//! The CRC variant is pinned by [`tests::crc_is_ccitt_false_matches_
//! decompiled_stock_uart_trans_table`] below. The remaining unknown is
//! the stock module's *init* constant (no decompiled init constant was
//! captured); CCITT-FALSE init `0xFFFF` is the byte-parity assumption
//! consistent with the live chip-UART path and BraiinsOS verification.
//! Resolving init byte-exactly against stock-firmware frames is the
//! only open R11-3 follow-up — and it CANNOT change surface (1).
//!
//! ## Cross-references
//!
//! - W4 handoff: `DCENT_OS_WAVE4_HANDOFF/bm1362_frames_v2.{h,c}` +
//!   `bm1362_nonce_format_update_summary.md`.
//! -  — kernel-internal layout;
//!   its IBM-SDLC CRC line is a symbol-name inference (see above), not
//!   DWARF-confirmed. Does NOT describe the live chip-UART frame.
//! -  — the proven live CRC is
//!   CCITT-FALSE, NOT IBM-SDLC (5-platform accepted-share evidence).
//! -  — the kernel module itself
//!   is NOT ported.
//! -  — no IOCTL adapter
//!   without numeric ordinals.
//! -  — codec-only
//!   intent.
//! -  — POR-vs-write
//!   disambiguation for the two MiscCtrl constants exposed below.

use crate::protocol::crc16; // REUSE — do NOT duplicate.

// ---------------------------------------------------------------------------
// Constants (mirrored from W4 handoff `bm1362_frames_v2.h`)
// ---------------------------------------------------------------------------

/// Stock-firmware `asic_work_t` size in bytes (W4 handoff).
pub const ASIC_WORK_SIZE: usize = 86;

/// Size of `uart_trans.ko`'s *kernel-internal* `pack_asic_work` buffer
/// (bytes) — NOT the on-wire frame size.
///
///: the 168-byte
/// `pack_asic_work` is the kernel-side buffer = the 86-byte on-wire
/// `asic_work_t` frame ([`ASIC_WORK_SIZE`]) **plus** 82 bytes of kernel
/// metadata (chain index, dispatch timestamp, retry count, ring-slot
/// index). What actually gets written to the UART is the 86-byte frame
/// (see [`wire_frame_bytes`] / [`AsicWorkFrame::to_bytes`]).
///
/// We expose this constant for parity diagnostics only; there is no
/// 168-byte send-buffer writer in DCENT_OS — see the module-level
/// "CODEC ONLY" note and [`super::uart_transport`].
pub const PACK_WORK_SIZE: usize = 168;

/// Minimum nonce response frame length (10 bytes: magic + len + chain
/// + job_id + nonce(4) + crc(2)).
pub const NONCE_FRAME_MIN_LEN: usize = 10;

/// RX magic byte for nonce frames (`0x55`, per W4 handoff).
pub const CMD_MAGIC: u8 = 0x55;

/// Stock-firmware TX type byte for work packages (`0xAA`).
pub const CMD_WORK_PACKAGE: u8 = 0xAA;

/// RX type byte for nonce submissions from BM1362 (`0x02`).
pub const CMD_NONCE_SUBMIT: u8 = 0x02;

/// RX type byte for chain configuration messages (`0x01`).
pub const CMD_CHAIN_CFG: u8 = 0x01;

/// RX type byte for error reports (`0x03`).
pub const CMD_ERROR_REPORT: u8 = 0x03;

// ---------------------------------------------------------------------------
// uart_trans.ko IOCTL ordinals ( Q1, AM335x BB)
// ---------------------------------------------------------------------------
//
// Decoded from `uart_trans_ioctl` switch dispatch in BBCtrl
// `/lib/modules/uart_trans.ko` at .text offset 0x819 (Thumb mode).
// IOCTL type='u' (0x75), NR range 0-10. See
// `Handoffs/DCENT_OS_FULL_HANDOFF/DCENT_OS_HANDOFF/RE_TEAM_FINDINGS_WAVE5.md`
// §Q1 for the full per-ordinal handler-action table.
//
// CV1835 analysis pass 2026-05-21 confirms the same dispatch shape and
// canonical 0x75xx family. The transport device is still board-specific at
// the UART path layer: CV1835 uses /dev/ttyS1..4, BB uses /dev/ttyO1/2/4/5.
//
// Codec in this module remains sealed via `UartTransTransport`; these
// IOCTL consts are NOT wired into any live transport adapter — they are
// captured here for FUTURE link-up to a kernel-driver adapter, NOT
// integrated yet..

/// `_IOW('u', 0x01, 4)` — MMAP_CONFIG: stores arg to g_uart_trans_info+4.
pub const UART_TRANS_IOCTL_MMAP_CONFIG: u32 = 0x40047501;
/// `_IOW('u', 0x02, 4)` — SET_BAUD: clamps arg ≤ 0x300, stores to struct+0.
pub const UART_TRANS_IOCTL_SET_BAUD: u32 = 0x40047502;
/// `_IOR('u', 0x03, 4)` — GET_BAUD.
pub const UART_TRANS_IOCTL_GET_BAUD: u32 = 0x80047503;
/// `_IO('u', 0x04)` — RESET_FIFO.
pub const UART_TRANS_IOCTL_RESET_FIFO: u32 = 0x00007504;
/// `_IO('u', 0x05)` — FLUSH_TX.
pub const UART_TRANS_IOCTL_FLUSH_TX: u32 = 0x00007505;
/// `_IO('u', 0x06)` — FLUSH_RX.
pub const UART_TRANS_IOCTL_FLUSH_RX: u32 = 0x00007506;
/// `_IOR('u', 0x09, 8)` — GET_NONCE.
pub const UART_TRANS_IOCTL_GET_NONCE: u32 = 0x80087509;
/// `_IOW('u', 0x0A, 4)` — SEND_WORK: calls uart_trans_send_work_once (relocation 0x88A).
pub const UART_TRANS_IOCTL_SEND_WORK: u32 = 0x4004750A;

// ---------------------------------------------------------------------------
// MiscCtrl POR-vs-write disambiguation
// ---------------------------------------------------------------------------

/// MiscCtrl POR (power-on reset) default value — silicon read-back
/// evidence of a cold reset.
///
/// **DO NOT WRITE THIS VALUE.** Per W4 handoff line 28, the BM1362
/// silicon `MiscCtrl` register reads back `0x0000_0001` immediately
/// after power-on reset. This is a *read* sentinel, NOT the runtime
/// configuration write target. The runtime write target is
/// [`MISCCTRL_POST_FAST_BAUD_WRITE`].
///
/// Confusing the two would silently revert MiscCtrl to its POR state
/// and re-enter the BM1362 cold-reset stall described in
/// .
pub const MISCCTRL_POR_RESET_DEFAULT: u32 = 0x0000_0001;

/// MiscCtrl post-fast-baud canonical write value.
///
/// This is the runtime configuration value written ×3 with 5 ms
/// spacing after the fast-baud switch (per RE3 §2.6 + dev-kit HAL
/// `s19j_init.c::s19j_misctrl_triple_write(0x00C100B0)`).
///
/// Cross-references the existing constants:
/// - `dcentrald_asic::bm1362::cold_boot_step::MISC_CONTROL_VALUE_POST_FAST_BAUD`
/// - `dcentrald_hal::platform::cvitek_cold_boot::MISCCTRL_VALUE`
/// - `dcentrald_hal::platform::cvitek_cold_boot::MISCCTRL_ASIC_REG`
///
/// All three already use this same value; this codec re-exports it so
/// parity tests can pin the disambiguation in one place.
pub const MISCCTRL_POST_FAST_BAUD_WRITE: u32 = 0x00C1_00B0;

// ---------------------------------------------------------------------------
// Sealed-trait transport marker
// ---------------------------------------------------------------------------

mod sealed {
    /// Crate-private supertrait — external crates cannot impl
    /// [`super::UartTransTransport`] for their own types.
    pub trait Sealed {}
}

/// Marker trait for `uart_trans.ko`-shape transports.
///
/// Sealed: only [`StockUartTrans`] implements it within this crate.
/// External crates cannot add new impls. This prevents the codec
/// from leaking into the existing direct-UART path
/// (`drivers::bm1362::send_work`) by accident.
///
/// COMPILE-CHECK: external `impl UartTransTransport for SomeOtherStruct {}`
/// fails because `Sealed` is crate-private.
pub trait UartTransTransport: sealed::Sealed {}

/// Stock Bitmain `uart_trans.ko`-shape transport marker.
///
/// This is a zero-sized type — there's no live writer behind it. It
/// exists only to gate the codec API at the type system so future
/// generic code that needs the codec must explicitly opt in via this
/// marker.
pub struct StockUartTrans;

impl sealed::Sealed for StockUartTrans {}
impl UartTransTransport for StockUartTrans {}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Codec error type for `wire_uart_trans` round-trip operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsicError {
    /// Computed CRC did not match the frame's trailing CRC.
    CrcMismatch,
    /// First byte was not the expected magic (`0x55`).
    BadMagic,
    /// Input slice was shorter than the minimum frame length.
    ShortFrame,
    /// Length field disagreed with the slice length, or some other
    /// structural mismatch.
    InvalidLength,
}

// ---------------------------------------------------------------------------
// asic_work_t (86 bytes)
// ---------------------------------------------------------------------------

/// 86-byte `asic_work_t` view.
///
/// Layout per W4 handoff `bm1362_frames_v2.h` lines 100-109
/// (`__attribute__((packed))` C struct):
///
/// | Offset  | Field      | Size | Notes                                |
/// |---------|------------|------|--------------------------------------|
/// | `[0]`   | `type`     | 1    | command byte (e.g. `0xAA`)           |
/// | `[1]`   | `rsvd1`    | 1    | reserved                             |
/// | `[2]`   | `job_id`   | 1    | low 8 bits of job id                 |
/// | `[3]`   | `rsvd2`    | 1    | reserved                             |
/// | `[4..8]`| `sno`      | 4    | sequence number (LE)                 |
/// | `[8..20]`| `data2`   | 12   | auxiliary data (ntime/nbits/job_hi)  |
/// | `[20..84]`| `data`   | 64   | midstate(32) + merkle(32)            |
/// | `[84..86]`| `crc16`  | 2    | CRC over bytes `[0..84]` (LE)        |
///
/// CRC: poly `0x1021`, computed by [`crate::protocol::crc16`]
/// (CRC-16/CCITT-FALSE — init `0xFFFF`, refin=false, refout=false,
/// xorout=`0x0000`). This matches the **forward, non-reflected**
/// CRC-CCITT table decompiled out of the stock `uart_trans.ko`
/// `.rodata` (`[0x0000, 0x1021, 0x2042, …]`) — see the module-level
/// "CRC provenance" section (R11-3). It is NOT IBM-SDLC; the
///  IBM-SDLC line is a Linux
/// symbol-name inference, not decompiled fact. Re-use the existing
/// implementation; do NOT duplicate or re-parameterize the CRC here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsicWorkFrame {
    pub type_byte: u8,
    pub rsvd1: u8,
    pub job_id: u8,
    pub rsvd2: u8,
    pub sno: u32,
    pub data2: [u8; 12],
    pub data: [u8; 64],
}

impl AsicWorkFrame {
    /// Decode an 86-byte `asic_work_t` buffer, validating the CRC over
    /// bytes `[0..84]` against the LE CRC at `[84..86]`.
    ///
    /// Returns [`AsicError::CrcMismatch`] if the trailing CRC does not
    /// match the computed CRC over the leading 84 bytes.
    pub fn from_bytes(buf: &[u8; ASIC_WORK_SIZE]) -> Result<Self, AsicError> {
        let computed = crc16(&buf[..84]);
        let frame_crc = u16::from_le_bytes([buf[84], buf[85]]);
        if computed != frame_crc {
            return Err(AsicError::CrcMismatch);
        }
        let mut data2 = [0u8; 12];
        data2.copy_from_slice(&buf[8..20]);
        let mut data = [0u8; 64];
        data.copy_from_slice(&buf[20..84]);
        Ok(Self {
            type_byte: buf[0],
            rsvd1: buf[1],
            job_id: buf[2],
            rsvd2: buf[3],
            sno: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            data2,
            data,
        })
    }

    /// Encode this frame as 86 bytes, computing the trailing CRC over
    /// the leading 84 bytes and writing it LE to `[84..86]`.
    pub fn to_bytes(&self) -> [u8; ASIC_WORK_SIZE] {
        let mut out = [0u8; ASIC_WORK_SIZE];
        out[0] = self.type_byte;
        out[1] = self.rsvd1;
        out[2] = self.job_id;
        out[3] = self.rsvd2;
        out[4..8].copy_from_slice(&self.sno.to_le_bytes());
        out[8..20].copy_from_slice(&self.data2);
        out[20..84].copy_from_slice(&self.data);
        let crc = crc16(&out[..84]);
        out[84..86].copy_from_slice(&crc.to_le_bytes());
        out
    }
}

/// Serialize an [`AsicWorkFrame`] as the 86-byte on-wire `asic_work_t`
/// buffer that gets written to the chain UART.
///
/// This is a clearly-named alias for [`AsicWorkFrame::to_bytes`] — it
/// exists so the codec → transport boundary is explicit at the call
/// site in [`super::uart_transport`]. There is intentionally nothing
/// more to it: the wire format IS the 86-byte frame; the 168-byte
/// `pack_asic_work` ([`PACK_WORK_SIZE`]) was the kernel-internal buffer
/// (86 B wire + 82 B metadata), never the wire format itself
///.
#[inline]
pub fn wire_frame_bytes(frame: &AsicWorkFrame) -> [u8; ASIC_WORK_SIZE] {
    frame.to_bytes()
}

// ---------------------------------------------------------------------------
// Nonce response (10 bytes)
// ---------------------------------------------------------------------------

/// 10-byte nonce response frame from BM1362 over `uart_trans.ko`
/// transport.
///
/// Layout per W4 handoff `bm1362_frames_v2.h` lines 115-122:
///
/// | Offset    | Field       | Size | Notes                       |
/// |-----------|-------------|------|-----------------------------|
/// | `[0]`     | `magic`     | 1    | `0x55`                      |
/// | `[1]`     | `len`       | 1    | payload length              |
/// | `[2]`     | `chain_id`  | 1    | chain/ASIC identifier       |
/// | `[3]`     | `job_id`    | 1    | job identifier              |
/// | `[4..8]`  | `nonce`     | 4    | found nonce (LE)            |
/// | `[8..10]` | `crc16`     | 2    | CRC over bytes `[0..8]` (LE)|
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonceResponse {
    pub chain_id: u8,
    pub job_id: u8,
    pub nonce: u32,
}

/// Parse a 10-byte (or longer) nonce response frame.
///
/// Validates magic byte (`0x55`) and CRC over the leading 8 bytes
/// against the LE CRC at `[8..10]`.
pub fn parse_nonce_frame(raw: &[u8]) -> Result<NonceResponse, AsicError> {
    if raw.len() < NONCE_FRAME_MIN_LEN {
        return Err(AsicError::ShortFrame);
    }
    if raw[0] != CMD_MAGIC {
        return Err(AsicError::BadMagic);
    }
    let computed = crc16(&raw[..8]);
    let frame_crc = u16::from_le_bytes([raw[8], raw[9]]);
    if computed != frame_crc {
        return Err(AsicError::CrcMismatch);
    }
    Ok(NonceResponse {
        chain_id: raw[2],
        job_id: raw[3],
        nonce: u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
    })
}

// ---------------------------------------------------------------------------
// Tests (host-safe, no hardware)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm1362::uart_relay::UartRelayReg;

    /// Helper: build a valid 10-byte nonce frame with valid CRC for a
    /// given (chain_id, job_id, nonce).
    fn make_nonce_frame(chain_id: u8, job_id: u8, nonce: u32) -> [u8; 10] {
        let mut f = [0u8; 10];
        f[0] = CMD_MAGIC;
        f[1] = 0x0A; // length sentinel (W4 handoff doesn't enforce a
                     // specific value — codec validates CRC, not len)
        f[2] = chain_id;
        f[3] = job_id;
        f[4..8].copy_from_slice(&nonce.to_le_bytes());
        let crc = crc16(&f[..8]);
        f[8..10].copy_from_slice(&crc.to_le_bytes());
        f
    }

    /// Helper: build a sample `AsicWorkFrame` with deterministic bytes
    /// driven from a single seed.
    fn make_frame(seed: u32) -> AsicWorkFrame {
        let mut data2 = [0u8; 12];
        for (i, b) in data2.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u32) as u8;
        }
        let mut data = [0u8; 64];
        for (i, b) in data.iter_mut().enumerate() {
            *b = seed.wrapping_mul(31).wrapping_add(i as u32) as u8;
        }
        AsicWorkFrame {
            type_byte: CMD_WORK_PACKAGE,
            rsvd1: 0,
            job_id: (seed & 0xFF) as u8,
            rsvd2: 0,
            sno: seed,
            data2,
            data,
        }
    }

    // ------------------------------------------------------------------
    // 1. crc_itu_table_known_vector_empty
    // ------------------------------------------------------------------

    /// `crate::protocol::crc16` is CRC-CCITT-FALSE (init `0xFFFF`).
    /// Empty input → `0xFFFF` (init value, no rounds executed).
    #[test]
    fn crc_itu_table_known_vector_empty() {
        assert_eq!(crc16(b""), 0xFFFF);
    }

    // ------------------------------------------------------------------
    // 2. crc_itu_table_known_vector_123456789
    // ------------------------------------------------------------------

    /// CCITT-FALSE reference vector for `b"123456789"` → `0x29B1`.
    /// Pins the algorithm choice in `crate::protocol::crc16`. If a
    /// future refactor switches the init value to `0x0000` (XMODEM /
    /// CRC-ITU V.41 init 0), this test fails loud.
    #[test]
    fn crc_itu_table_known_vector_123456789() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    // ------------------------------------------------------------------
    // 2b. crc_is_ccitt_false_matches_decompiled_stock_uart_trans_table
    //     (R11-3 / PR-007 resolution pin)
    // ------------------------------------------------------------------

    /// **R11-3 / PR-007 load-bearing pin.** Proves the codec's CRC is
    /// the *forward, non-reflected* CRC-CCITT family — byte-for-byte the
    /// table that was decompiled out of the stock `uart_trans.ko`
    /// `.rodata` during RE: `[0x0000, 0x1021, 0x2042, 0x3063, …]`
    /// (`BINARY_RE_REPORT.md` §2.3 + `re_uart_text.py:51-54`).
    ///
    /// The CRC-CCITT lookup table entry `T[b]` is, by definition, the
    /// forward poly-`0x1021` applied MSB-first to the single byte `b`
    /// with a zero accumulator. We reconstruct it from the SAME
    /// algorithm `crate::protocol::crc16` uses (init-independent inner
    /// loop) and assert it equals the decompiled stock table prefix.
    ///
    /// It then asserts the codec is **NOT** IBM-SDLC / CRC-16/X-25:
    /// that reflected variant's table would start `[0x0000, 0x1189,
    /// 0x2312, …]` and its `"123456789"` check value is `0x906E`, not
    /// the CCITT-FALSE `0x29B1`. If a future refactor ever swaps
    /// `crate::protocol::crc16` to the reflected IBM-SDLC parameter set
    /// (the variant
    /// *inferred* from the Linux symbol name), this fails loud — and so
    /// would byte-parity against a real stock-firmware bench unit.
    #[test]
    fn crc_is_ccitt_false_matches_decompiled_stock_uart_trans_table() {
        // Forward (non-reflected) CRC-CCITT table entry for byte `b`:
        // poly 0x1021, MSB-first, zero accumulator. This is exactly the
        // per-byte transform inside `crate::protocol::crc16` minus the
        // 0xFFFF init — so it is a faithful witness of the algorithm.
        fn ccitt_forward_table_entry(b: u8) -> u16 {
            let mut crc: u16 = (b as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
            crc
        }

        // The exact bytes read out of stock `uart_trans.ko` `.rodata`.
        let decompiled_stock_table_prefix: [u16; 4] = [0x0000, 0x1021, 0x2042, 0x3063];
        for (b, &expected) in decompiled_stock_table_prefix.iter().enumerate() {
            assert_eq!(
                ccitt_forward_table_entry(b as u8),
                expected,
                "codec CRC table entry [{}] must match the decompiled \
                 stock uart_trans.ko forward CRC-CCITT .rodata table \
                 (R11-3). A mismatch means the CRC was re-parameterized \
                 away from byte-parity with stock firmware.",
                b
            );
        }

        // Discriminator: a reflected IBM-SDLC / CRC-16/X-25 table would
        // start [0x0000, 0x1189, 0x2312, …] — assert we are NOT that.
        assert_ne!(
            ccitt_forward_table_entry(1),
            0x1189,
            "codec must NOT be the reflected IBM-SDLC/X-25 table — that \
             is the parameter set  \
             *inferred* from the Linux `crc_itu_t` symbol name, which the \
             decompiled .rodata table contradicts."
        );

        // And the public check value stays CCITT-FALSE (0x29B1), never
        // the IBM-SDLC/X-25 check value (0x906E).
        assert_eq!(crc16(b"123456789"), 0x29B1, "CCITT-FALSE check value");
        assert_ne!(
            crc16(b"123456789"),
            0x906E,
            "0x906E is the IBM-SDLC/X-25 check value — must never appear \
             here; the live 5-platform-proven path is CCITT-FALSE."
        );
    }

    // ------------------------------------------------------------------
    // 3-7. Constant pinning
    // ------------------------------------------------------------------

    #[test]
    fn asic_work_size_is_86() {
        assert_eq!(ASIC_WORK_SIZE, 86);
    }

    #[test]
    fn pack_work_size_is_168() {
        assert_eq!(PACK_WORK_SIZE, 168);
    }

    /// The on-wire frame is the 86-byte `asic_work_t` — NOT the 168-byte
    /// kernel-internal `pack_asic_work` buffer
    ///. If a future change
    /// makes `wire_frame_bytes` emit 168 bytes (e.g. by accidentally
    /// wrapping the kernel metadata), this test fails loud.
    #[test]
    fn wire_frame_is_86_bytes_not_168() {
        let frame = make_frame(0x1234_5678);
        let wire = wire_frame_bytes(&frame);
        assert_eq!(wire.len(), 86, "on-wire asic_work_t is 86 bytes");
        assert_ne!(
            wire.len(),
            PACK_WORK_SIZE,
            "the 168-byte pack_asic_work is the KERNEL-INTERNAL buffer \
             (86 B wire + 82 B metadata), never what goes on the UART wire"
        );
    }

    /// `wire_frame_bytes` is exactly `AsicWorkFrame::to_bytes` — it's an
    /// explicit-name alias for the codec → transport boundary, nothing
    /// more. Byte-for-byte identical for several seeds.
    #[test]
    fn wire_frame_bytes_equals_to_bytes() {
        for seed in [0u32, 1, 0x42, 0xDEAD_BEEF, 0xA5A5_5A5A, 0xFFFF_FFFF] {
            let frame = make_frame(seed);
            assert_eq!(
                wire_frame_bytes(&frame),
                frame.to_bytes(),
                "seed {:#x}: wire_frame_bytes must be byte-identical to to_bytes",
                seed
            );
        }
    }

    #[test]
    fn nonce_min_len_is_10() {
        assert_eq!(NONCE_FRAME_MIN_LEN, 10);
    }

    #[test]
    fn cmd_magic_is_0x55() {
        assert_eq!(CMD_MAGIC, 0x55);
    }

    #[test]
    fn cmd_work_package_is_0xaa() {
        assert_eq!(CMD_WORK_PACKAGE, 0xAA);
    }

    // ------------------------------------------------------------------
    // 8. asic_work_serialize_round_trip — fuzz 8 vectors
    // ------------------------------------------------------------------

    #[test]
    fn asic_work_serialize_round_trip() {
        for seed in [
            0u32,
            1,
            0x42,
            0xDEAD_BEEF,
            0xCAFE_BABE,
            0xA5A5_5A5A,
            0xFFFF_FFFF,
            0x1234_5678,
        ] {
            let original = make_frame(seed);
            let bytes = original.to_bytes();
            let decoded = AsicWorkFrame::from_bytes(&bytes)
                .expect("round-trip should validate CRC for own output");

            assert_eq!(
                decoded.type_byte, original.type_byte,
                "seed {:#x} type",
                seed
            );
            assert_eq!(decoded.rsvd1, original.rsvd1, "seed {:#x} rsvd1", seed);
            assert_eq!(decoded.job_id, original.job_id, "seed {:#x} job_id", seed);
            assert_eq!(decoded.rsvd2, original.rsvd2, "seed {:#x} rsvd2", seed);
            assert_eq!(decoded.sno, original.sno, "seed {:#x} sno", seed);
            assert_eq!(decoded.data2, original.data2, "seed {:#x} data2", seed);
            assert_eq!(decoded.data, original.data, "seed {:#x} data", seed);
        }
    }

    // ------------------------------------------------------------------
    // 9. asic_work_crc_offset_is_84
    // ------------------------------------------------------------------

    #[test]
    fn asic_work_crc_offset_is_84() {
        let original = make_frame(0xABCD_1234);
        let mut bytes = original.to_bytes();
        // Flip a bit in the CRC field at [84]; decoder must reject.
        bytes[84] ^= 0x01;
        assert_eq!(
            AsicWorkFrame::from_bytes(&bytes),
            Err(AsicError::CrcMismatch)
        );

        // Restore + flip [85].
        let mut bytes = original.to_bytes();
        bytes[85] ^= 0x80;
        assert_eq!(
            AsicWorkFrame::from_bytes(&bytes),
            Err(AsicError::CrcMismatch)
        );
    }

    // ------------------------------------------------------------------
    // 10. asic_work_rejects_bad_crc_data
    // ------------------------------------------------------------------

    #[test]
    fn asic_work_rejects_bad_crc_data() {
        let original = make_frame(0x5A5A_A5A5);
        let mut bytes = original.to_bytes();
        // Flip a data byte at [20] WITHOUT recomputing the CRC.
        // Decoder must reject.
        bytes[20] ^= 0xFF;
        assert_eq!(
            AsicWorkFrame::from_bytes(&bytes),
            Err(AsicError::CrcMismatch)
        );
    }

    // ------------------------------------------------------------------
    // 11. nonce_extraction_byte_exact
    // ------------------------------------------------------------------

    #[test]
    fn nonce_extraction_byte_exact() {
        let frame = make_nonce_frame(0xCC, 0x3F, 0xDEAD_BEEF);
        let parsed = parse_nonce_frame(&frame).expect("valid frame must parse");
        assert_eq!(parsed.chain_id, 0xCC);
        assert_eq!(parsed.job_id, 0x3F);
        assert_eq!(parsed.nonce, 0xDEAD_BEEF);

        // Verify byte-for-byte that nonce is LE-encoded at [4..8].
        assert_eq!(&frame[4..8], &[0xEF, 0xBE, 0xAD, 0xDE]);
    }

    // ------------------------------------------------------------------
    // 12. nonce_rejects_wrong_magic
    // ------------------------------------------------------------------

    #[test]
    fn nonce_rejects_wrong_magic() {
        let mut frame = make_nonce_frame(0x00, 0x00, 0);
        frame[0] = 0xAA; // not 0x55
        assert_eq!(parse_nonce_frame(&frame), Err(AsicError::BadMagic));
    }

    // ------------------------------------------------------------------
    // 13. nonce_rejects_short_frame
    // ------------------------------------------------------------------

    #[test]
    fn nonce_rejects_short_frame() {
        for short_len in 0..NONCE_FRAME_MIN_LEN {
            let buf = vec![0x55u8; short_len];
            assert_eq!(
                parse_nonce_frame(&buf),
                Err(AsicError::ShortFrame),
                "len {} must be rejected as short",
                short_len
            );
        }
    }

    // ------------------------------------------------------------------
    // 14. nonce_rejects_bad_crc
    // ------------------------------------------------------------------

    #[test]
    fn nonce_rejects_bad_crc() {
        let mut frame = make_nonce_frame(0x01, 0x02, 0x03040506);
        frame[8] ^= 0xFF;
        assert_eq!(parse_nonce_frame(&frame), Err(AsicError::CrcMismatch));

        let mut frame = make_nonce_frame(0x01, 0x02, 0x03040506);
        frame[9] ^= 0x10;
        assert_eq!(parse_nonce_frame(&frame), Err(AsicError::CrcMismatch));
    }

    // ------------------------------------------------------------------
    // 15. miscctrl_constants_distinguish_por_vs_write
    // ------------------------------------------------------------------

    /// W4 handoff line 28 disambiguation: `BM1362_MISCCTRL_DEFAULT =
    /// 0x00000001` describes the chip POR reset state, NOT the value
    /// to write. The runtime write target is `0x00C100B0`. These two
    /// constants MUST stay distinct or future code will silently
    /// regress MiscCtrl back to its POR state and re-trigger the
    /// BM1362 cold-reset stall.
    #[test]
    fn miscctrl_constants_distinguish_por_vs_write_w4_handoff_line_28() {
        assert_eq!(MISCCTRL_POR_RESET_DEFAULT, 0x0000_0001);
        assert_eq!(MISCCTRL_POST_FAST_BAUD_WRITE, 0x00C1_00B0);
        assert_ne!(
            MISCCTRL_POR_RESET_DEFAULT, MISCCTRL_POST_FAST_BAUD_WRITE,
            "POR-reset read sentinel must NEVER equal the runtime write \
             target.."
        );
    }

    // ------------------------------------------------------------------
    // 16. urelay_bitfield_matches_w13_layout
    // ------------------------------------------------------------------

    /// Cross-check that the W13.B1 `bm1362::uart_relay::UartRelayReg`
    /// bitfield positions match the W4 handoff
    /// `bm1362_frames_v2.h` lines 64-68 documented layout:
    ///   - chip_address [7:0]
    ///   - gap_cnt      [11:8]   (4 bits, max 15)
    ///   - nonce_gap_en [12]
    ///   - ro_relay_en  [13]
    ///   - co_relay_en  [14]
    /// Pin via XOR-of-set-bit-only constructions (same approach as
    /// `uart_relay.rs::tests::uart_relay_reg_*_bit_*`).
    #[test]
    fn urelay_bitfield_matches_w13_layout() {
        // chip_address shift = 0 → setting addr=0x80 lights bit 7 only.
        let addr = UartRelayReg::new(0x80, 0, false, false, false);
        assert_eq!(addr.raw(), 1u32 << 7);

        // gap_cnt shift = 8, 4-bit width → setting gap=0x0F lights
        // bits [11:8] only (= 0x0000_0F00).
        let gap = UartRelayReg::new(0, 0x0F, false, false, false);
        assert_eq!(gap.raw(), 0x0000_0F00);

        // nonce_gap_en bit = 12.
        let off = UartRelayReg::new(0, 0, false, false, false);
        let on = UartRelayReg::new(0, 0, true, false, false);
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 12);

        // ro_relay_en bit = 13.
        let on = UartRelayReg::new(0, 0, false, true, false);
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 13);

        // co_relay_en bit = 14.
        let on = UartRelayReg::new(0, 0, false, false, true);
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 14);

        // gap_cnt width is 4 bits — values above 15 must truncate.
        assert_eq!(UartRelayReg::GAP_CNT_WIDTH, 4);
        assert_eq!(UartRelayReg::GAP_CNT_MAX, 15);
    }

    // ------------------------------------------------------------------
    // 17. transport_marker_is_sealed_compile_check
    // ------------------------------------------------------------------

    /// COMPILE-CHECK: external `impl UartTransTransport for SomeOtherStruct`
    /// is forbidden because the supertrait `sealed::Sealed` is
    /// crate-private. We can't write a `compile_fail` doctest from a
    /// regular test; instead we pin the TWO documented properties:
    ///   1. `StockUartTrans` does implement the marker (proven by
    ///      `impls_marker::<StockUartTrans>()` compiling).
    ///   2. The trait's `Sealed` supertrait is private (proven by the
    ///      grep below — search would fail if `pub mod sealed`).
    #[test]
    fn transport_marker_is_sealed_compile_check() {
        // Property 1: StockUartTrans implements the marker.
        fn impls_marker<T: UartTransTransport>() {}
        impls_marker::<StockUartTrans>();

        // Property 2: `mod sealed` is private (not `pub`). If a future
        // refactor exposes it, the marker is no longer sealed and
        // external crates can extend it.
        let src = include_str!("wire_uart_trans.rs");
        let code = src
            .lines()
            .map(|line| line.split("//").next().unwrap_or("").trim())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.lines().any(|line| line == "mod sealed {"),
            "wire_uart_trans.rs MUST keep the private `mod sealed` block. \
             Exposing it (pub mod sealed) breaks the sealed-trait \
             guarantee."
        );
        assert!(
            !code.lines().any(|line| line.starts_with("pub mod sealed")),
            "wire_uart_trans.rs MUST NOT make the `sealed` module public — \
             that breaks the sealed-trait guarantee that prevents the codec \
             from leaking into the direct-UART path."
        );
    }

    // ------------------------------------------------------------------
    // W15/R6 uart_trans.ko IOCTL ordinal pinning
    // ------------------------------------------------------------------

    /// W15/R6 catalog: SET_BAUD = `_IOW('u', 0x02, 4)`
    /// = `0x40047502`. Decoded from BBCtrl `uart_trans.ko::uart_trans_ioctl`
    /// switch dispatch at .text 0x819 (Thumb mode), literal pool at 0x940.
    #[test]
    fn uart_trans_ioctl_set_baud_is_0x40047502() {
        assert_eq!(UART_TRANS_IOCTL_SET_BAUD, 0x40047502);
    }

    /// W15/R6 catalog: MMAP_CONFIG = `_IOW('u', 0x01, 4)`
    /// = `0x40047501`. Stores arg to g_uart_trans_info+4; configures the
    /// mmap buffer.
    #[test]
    fn uart_trans_ioctl_mmap_config_is_0x40047501() {
        assert_eq!(UART_TRANS_IOCTL_MMAP_CONFIG, 0x40047501);
    }

    /// W15/R6 catalog: GET_BAUD = `_IOR('u', 0x03, 4)` = `0x80047503`.
    #[test]
    fn uart_trans_ioctl_get_baud_is_0x80047503() {
        assert_eq!(UART_TRANS_IOCTL_GET_BAUD, 0x80047503);
    }

    #[test]
    fn uart_trans_ioctl_reset_and_flush_ordinals_match_w15() {
        assert_eq!(UART_TRANS_IOCTL_RESET_FIFO, 0x00007504);
        assert_eq!(UART_TRANS_IOCTL_FLUSH_TX, 0x00007505);
        assert_eq!(UART_TRANS_IOCTL_FLUSH_RX, 0x00007506);
    }

    #[test]
    fn uart_trans_ioctl_get_nonce_is_0x80087509() {
        assert_eq!(UART_TRANS_IOCTL_GET_NONCE, 0x80087509);
    }

    /// W15/R6 catalog: SEND_WORK = `_IOW('u', 0x0A, 4)`
    /// = `0x4004750A`. Relocation 0x88A → `uart_trans_send_work_once`.
    #[test]
    fn uart_trans_ioctl_send_work_is_0x4004750a() {
        assert_eq!(UART_TRANS_IOCTL_SEND_WORK, 0x4004750A);
    }

    /// All pinned IOCTL ordinals must be unique. If two collide, an issued IOCTL
    /// would dispatch to the wrong handler, silently corrupting either FIFO
    /// state, the mmap buffer pointer, nonce polling, or the work-send path.
    #[test]
    fn uart_trans_ioctl_ordinals_are_unique() {
        let all = [
            UART_TRANS_IOCTL_MMAP_CONFIG,
            UART_TRANS_IOCTL_SET_BAUD,
            UART_TRANS_IOCTL_GET_BAUD,
            UART_TRANS_IOCTL_RESET_FIFO,
            UART_TRANS_IOCTL_FLUSH_TX,
            UART_TRANS_IOCTL_FLUSH_RX,
            UART_TRANS_IOCTL_GET_NONCE,
            UART_TRANS_IOCTL_SEND_WORK,
        ];
        for (i, &a) in all.iter().enumerate() {
            for (j, &b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        a, b,
                        "uart_trans IOCTL ordinals collide at indices {} and {} (both = 0x{:08X})",
                        i, j, a
                    );
                }
            }
        }
        // Pin the count too — adding a 10th ordinal without updating this
        // test would slip past the uniqueness check.
        assert_eq!(
            all.len(),
            8,
            "W15/R6 catalog enumerates 8 pinned IOCTL ordinals"
        );
    }

    // ------------------------------------------------------------------
    // 18. existing_direct_uart_path_does_not_implement_marker
    // ------------------------------------------------------------------

    /// The existing direct-UART path lives at
    /// `dcentrald/src/drivers/bm1362::send_work` (in the parent
    /// daemon crate, NOT this asic crate). We can't directly reach
    /// across crate boundaries, so we pin the property via two
    /// regex/grep checks against this module's source:
    ///   1. This file does NOT add an `impl UartTransTransport for ...`
    ///      anywhere except for `StockUartTrans`.
    ///   2. The `// CODEC ONLY — no live writer` documentation is
    ///      preserved (load-bearing intent marker for future agents).
    #[test]
    fn existing_direct_uart_path_does_not_implement_marker() {
        let src = include_str!("wire_uart_trans.rs");
        let code = src
            .lines()
            .map(|line| line.split("//").next().unwrap_or("").trim())
            .collect::<Vec<_>>()
            .join("\n");

        // Count `impl UartTransTransport for ...` occurrences. There
        // should be exactly ONE — for StockUartTrans.
        let impl_count = code
            .lines()
            .filter(|line| line.starts_with("impl UartTransTransport for "))
            .count();
        assert_eq!(
            impl_count, 1,
            "exactly ONE `impl UartTransTransport for ...` allowed in \
             this module (the StockUartTrans marker). Found {} — adding \
             more would weaken the sealed-trait gate.",
            impl_count
        );

        // The single impl must be StockUartTrans.
        assert!(
            code.lines()
                .any(|line| line == "impl UartTransTransport for StockUartTrans {}"),
            "the single sealed impl must be `StockUartTrans` — if you \
             changed the marker name, update this test."
        );

        // Load-bearing intent marker.
        assert!(
            src.contains("CODEC ONLY"),
            "wire_uart_trans.rs MUST keep the `CODEC ONLY — no live \
             writer` doc marker. Future agents who add a live writer \
             must re-evaluate the sustained-mining safety story \
             documented in ."
        );
    }
}
