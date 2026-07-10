//!  uart-A — BB-platform `uart_trans` frame layout DTOs (HAL-free).
//!
//! Source RE evidence:
//! -  (workspace memory, RE'd 2026-04-29
//!   from `lib_modules_uart_trans.ko` 54.7 KB pulled live from `a lab unit`).
//! - DWARF debug info preserved in the binary: Jenkins build path
//!   `Antminer_BHB42XXX_BBCtrl_release_J/build/beaglebone-kernel/drivers/misc/uart_trans/uart_trans.c`.
//!
//! `uart_trans.ko` is the Bitmain BB-platform (Antminer BHB42XXX BBCtrl,
//! Linux 3.8.13) batched-write framing layer over `/dev/ttyO{1,2,4,5}`.
//! It does NOT touch UART hardware registers — the omap-serial mainline
//! driver owns the hardware. Char dev `/dev/uart_trans`, misc class
//! major 10 minor 59. mmaps a contiguous-pages buffer for zero-copy
//! work submission and runs an hrtimer that batch-sends 86-byte ASIC
//! work frames with CRC-CCITT-ITU-T (poly 0x1021) tag at offset +84.
//!
//! DCENT_OS port strategy: **clean-room userspace reimpl** (Strategy B
//!). This module pins the wire-side
//! contract — frame layout, ioctl op IDs, char-dev info, chain map —
//! so the userspace replacement and any blob-reuse fallback share the
//! same DTO surface.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Frame layout — `struct asic_work_t` (DWARF-recovered)
// ---------------------------------------------------------------------------

/// Total wire size of one ASIC work frame (header + data + CRC).
pub const ASIC_WORK_FRAME_SIZE: usize = 86;

/// Per-field byte offsets inside the 86-byte frame.
pub const OFFSET_TYPE: usize = 0;
pub const OFFSET_RSVD0: usize = 1;
pub const OFFSET_JOB_ID: usize = 2;
pub const OFFSET_RSVD1: usize = 3;
/// Sequence number is little-endian u32 starting at offset 4.
pub const OFFSET_SNO: usize = 4;
/// Length of `sno` field in bytes.
pub const SNO_LEN: usize = 4;
/// 12-byte data2 region (BM1362 prehash midstate carry).
pub const OFFSET_DATA2: usize = 8;
pub const DATA2_LEN: usize = 12;
/// 64-byte data region (work header + nbits + ntime).
pub const OFFSET_DATA: usize = 20;
pub const DATA_LEN: usize = 64;
/// CRC-CCITT-ITU-T tag at offset 84 (last 2 bytes).
pub const OFFSET_CRC: usize = 84;
pub const CRC_LEN: usize = 2;

/// CRC polynomial (CRC-CCITT-ITU-T).
pub const CRC_POLY: u16 = 0x1021;

/// HAL-free description of the work-frame layout. `Serialize`-only —
/// constructed from constants, decoded by callers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AsicWorkFrameLayout {
    pub frame_size: usize,
    pub type_offset: usize,
    pub job_id_offset: usize,
    pub sno_offset: usize,
    pub sno_len: usize,
    pub data2_offset: usize,
    pub data2_len: usize,
    pub data_offset: usize,
    pub data_len: usize,
    pub crc_offset: usize,
    pub crc_len: usize,
    pub crc_poly: u16,
}

/// The canonical layout, populated from the const table.
pub const ASIC_WORK_FRAME_LAYOUT: AsicWorkFrameLayout = AsicWorkFrameLayout {
    frame_size: ASIC_WORK_FRAME_SIZE,
    type_offset: OFFSET_TYPE,
    job_id_offset: OFFSET_JOB_ID,
    sno_offset: OFFSET_SNO,
    sno_len: SNO_LEN,
    data2_offset: OFFSET_DATA2,
    data2_len: DATA2_LEN,
    data_offset: OFFSET_DATA,
    data_len: DATA_LEN,
    crc_offset: OFFSET_CRC,
    crc_len: CRC_LEN,
    crc_poly: CRC_POLY,
};

// ---------------------------------------------------------------------------
// ioctl operations
// ---------------------------------------------------------------------------

/// Magic byte for the ioctl request type (`'u'` per DWARF).
pub const UART_TRANS_IOCTL_MAGIC: u8 = b'u';

/// ioctl operation code (just the `nr` byte; the `_IOW` macro is
/// platform-specific and runs in the runtime adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UartTransIoctl {
    /// `_IOW('u', 0x01, int)` - configure the mmap buffer.
    MmapConfig,
    /// `_IOW('u', 0x02, int)` - set the UART baud (no caller in this
    /// build but the symbol exists; reserved).
    SetBaud,
    /// `_IOR('u', 0x03, int)` - read back the UART baud.
    GetBaud,
    /// `_IO('u', 0x04)` - reset FIFO state.
    ResetFifo,
    /// `_IO('u', 0x05)` - flush TX.
    FlushTx,
    /// `_IO('u', 0x06)` - flush RX.
    FlushRx,
    /// `_IOR('u', 0x09, 8)` - poll one nonce.
    GetNonce,
    /// `_IOW('u', 0x0A, int)` - send one work frame.
    SendWork,
}

impl UartTransIoctl {
    /// `nr` byte for the ioctl request.
    pub const fn nr(&self) -> u8 {
        match self {
            Self::MmapConfig => 0x01,
            Self::SetBaud => 0x02,
            Self::GetBaud => 0x03,
            Self::ResetFifo => 0x04,
            Self::FlushTx => 0x05,
            Self::FlushRx => 0x06,
            Self::GetNonce => 0x09,
            Self::SendWork => 0x0A,
        }
    }
}

// ---------------------------------------------------------------------------
// Char device + chain map
// ---------------------------------------------------------------------------

/// Path the kernel module exposes to userspace.
pub const UART_TRANS_DEV_PATH: &str = "/dev/uart_trans";

/// Misc-class major number (Linux convention).
pub const UART_TRANS_MAJOR: u32 = 10;
/// Misc-class minor number.
pub const UART_TRANS_MINOR: u32 = 59;

/// The 4 BB-platform tty paths that carry one ASIC chain each. Index
/// here is the chain index; the byte at `(chain_exist_bits >> idx) & 1`
/// is the chain-active flag.
pub const UART_TRANS_TTY_PATHS: [&str; 4] =
    ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"];

/// Number of supported chains.
pub const UART_TRANS_CHAIN_COUNT: usize = 4;

/// Returns the chain-active flag for chain index `idx` given the bitmap
/// the operator supplied as a chain-present bitmap.
pub const fn is_chain_present(chain_exist_bits: u32, idx: u32) -> bool {
    if idx >= UART_TRANS_CHAIN_COUNT as u32 {
        return false;
    }
    (chain_exist_bits >> idx) & 0x1 == 0x1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_is_exactly_86_bytes() {
        // RE doc: "86-byte ASIC work frames" — pin the size; a refactor
        // that bumped to 88 (BM1362 full-header) or 84 (BM1387 short
        // form) would break the BB-platform TX path silently.
        assert_eq!(ASIC_WORK_FRAME_SIZE, 86);
    }

    #[test]
    fn field_offsets_are_contiguous_and_total_86() {
        // type(1) + rsvd(1) + job_id(1) + rsvd(1) + sno(4) + data2(12)
        // + data(64) + crc(2) = 86.
        assert_eq!(OFFSET_TYPE, 0);
        assert_eq!(OFFSET_JOB_ID, 2);
        assert_eq!(OFFSET_SNO, 4);
        assert_eq!(OFFSET_DATA2, 8);
        assert_eq!(OFFSET_DATA, 20);
        assert_eq!(OFFSET_CRC, 84);
        assert_eq!(OFFSET_DATA + DATA_LEN, OFFSET_CRC);
        assert_eq!(OFFSET_CRC + CRC_LEN, ASIC_WORK_FRAME_SIZE);
    }

    #[test]
    fn data2_immediately_precedes_data() {
        assert_eq!(OFFSET_DATA2 + DATA2_LEN, OFFSET_DATA);
    }

    #[test]
    fn sno_is_4_bytes() {
        assert_eq!(SNO_LEN, 4);
        assert_eq!(OFFSET_SNO + SNO_LEN, OFFSET_DATA2);
    }

    #[test]
    fn crc_uses_ccitt_itu_t_polynomial() {
        // RE doc: "CRC-ITU-T (poly 0x1021)".
        assert_eq!(CRC_POLY, 0x1021);
    }

    #[test]
    fn ioctl_magic_byte_is_lowercase_u() {
        assert_eq!(UART_TRANS_IOCTL_MAGIC, b'u');
    }

    #[test]
    fn ioctl_nrs_match_re_doc() {
        assert_eq!(UartTransIoctl::MmapConfig.nr(), 0x01);
        assert_eq!(UartTransIoctl::SetBaud.nr(), 0x02);
        assert_eq!(UartTransIoctl::GetBaud.nr(), 0x03);
        assert_eq!(UartTransIoctl::ResetFifo.nr(), 0x04);
        assert_eq!(UartTransIoctl::FlushTx.nr(), 0x05);
        assert_eq!(UartTransIoctl::FlushRx.nr(), 0x06);
        assert_eq!(UartTransIoctl::GetNonce.nr(), 0x09);
        assert_eq!(UartTransIoctl::SendWork.nr(), 0x0A);
    }

    #[test]
    fn char_dev_path_and_minor_pinned() {
        assert_eq!(UART_TRANS_DEV_PATH, "/dev/uart_trans");
        assert_eq!(UART_TRANS_MAJOR, 10);
        assert_eq!(UART_TRANS_MINOR, 59);
    }

    #[test]
    fn four_chain_tty_paths_match_re_doc() {
        // /dev/ttyO{1,2,4,5} — note the gap (no ttyO3).
        assert_eq!(UART_TRANS_TTY_PATHS.len(), 4);
        assert_eq!(UART_TRANS_TTY_PATHS[0], "/dev/ttyO1");
        assert_eq!(UART_TRANS_TTY_PATHS[1], "/dev/ttyO2");
        assert_eq!(UART_TRANS_TTY_PATHS[2], "/dev/ttyO4");
        assert_eq!(UART_TRANS_TTY_PATHS[3], "/dev/ttyO5");
        assert_eq!(UART_TRANS_CHAIN_COUNT, 4);
    }

    #[test]
    fn ttyO3_is_intentionally_absent() {
        // Defensive pin: ttyO3 is reserved (debug console on BB) and
        // MUST NOT appear in the chain list. Live RE confirmed only
        // {1,2,4,5}.
        assert!(!UART_TRANS_TTY_PATHS.contains(&"/dev/ttyO3"));
    }

    #[test]
    fn chain_exist_bits_helper_decodes_per_index() {
        // Bit n maps to chain index n. 0b0101 = chains 0 and 2 present.
        assert!(is_chain_present(0b0101, 0));
        assert!(!is_chain_present(0b0101, 1));
        assert!(is_chain_present(0b0101, 2));
        assert!(!is_chain_present(0b0101, 3));
        // Out-of-range index returns false (defensive).
        assert!(!is_chain_present(0xFFFF_FFFF, 4));
        assert!(!is_chain_present(0xFFFF_FFFF, 99));
    }

    #[test]
    fn chain_exist_bits_zero_means_no_chains() {
        for idx in 0..UART_TRANS_CHAIN_COUNT as u32 {
            assert!(!is_chain_present(0, idx));
        }
    }

    #[test]
    fn chain_exist_bits_full_mask_lights_all_chains() {
        for idx in 0..UART_TRANS_CHAIN_COUNT as u32 {
            assert!(is_chain_present(0x0F, idx));
        }
    }

    #[test]
    fn layout_struct_mirrors_constants() {
        // Defensive pin so a refactor of the const table doesn't
        // diverge from the descriptor surface.
        let l = ASIC_WORK_FRAME_LAYOUT;
        assert_eq!(l.frame_size, ASIC_WORK_FRAME_SIZE);
        assert_eq!(l.crc_offset, OFFSET_CRC);
        assert_eq!(l.crc_poly, CRC_POLY);
        assert_eq!(l.data_len, DATA_LEN);
        assert_eq!(l.data2_len, DATA2_LEN);
    }

    #[test]
    fn ioctl_round_trips_through_serde() {
        for op in [
            UartTransIoctl::MmapConfig,
            UartTransIoctl::SetBaud,
            UartTransIoctl::GetBaud,
            UartTransIoctl::ResetFifo,
            UartTransIoctl::FlushTx,
            UartTransIoctl::FlushRx,
            UartTransIoctl::GetNonce,
            UartTransIoctl::SendWork,
        ] {
            let json = serde_json::to_string(&op).unwrap();
            let back: UartTransIoctl = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
    }

    #[test]
    fn ioctl_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&UartTransIoctl::MmapConfig).unwrap(),
            "\"mmap_config\""
        );
        assert_eq!(
            serde_json::to_string(&UartTransIoctl::SendWork).unwrap(),
            "\"send_work\""
        );
    }

    #[test]
    fn layout_serializes_to_explicit_fields() {
        let json = serde_json::to_value(&ASIC_WORK_FRAME_LAYOUT).unwrap();
        assert_eq!(json["frame_size"].as_u64(), Some(86));
        assert_eq!(json["crc_offset"].as_u64(), Some(84));
        assert_eq!(json["crc_poly"].as_u64(), Some(0x1021));
        assert_eq!(json["data_len"].as_u64(), Some(64));
    }
}
