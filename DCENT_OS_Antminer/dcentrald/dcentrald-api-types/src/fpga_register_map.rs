//!  fpga-A — Zynq FPGA register-map catalog (HAL-free).
//!
//! Source RE evidence:
//!  (the
//! Bitmain S9 + BraiinsOS s9io v0.2 / v1.0.2 register map).
//!
//! Two distinct FPGA designs ship in the field:
//! - **Bitmain stock** — proprietary; covered in the doc but NOT
//!   the implementation target (DCENT_OS uses BraiinsOS bitstream).
//! - **BraiinsOS s9io v0.2** — 9 hash-chain instances at
//!   `0x43C00000..0x43C80000` (each 64 KB).
//! - **BraiinsOS s9io v1.0.2** — production-deployed; relocated to 3
//!   instances at `0x43C00000`, `0x43C10000`, `0x43C20000`.
//!
//! Each s9io instance has 4 register windows (Common / Cmd / Work-RX /
//! Work-TX) at 4 KB offsets within a 16 KB block per RE doc §3.
//!
//! HAL-free: pure data + lookup. The runtime adapter inside
//! `dcentrald-hal::fpga` consumes these offsets to compose
//! UIO/devmem reads/writes.

use serde::{Deserialize, Serialize};

/// Production v1.0.2 chain base addresses.
pub const S9IO_V102_CHAIN_BASES: [u32; 3] = [0x43C0_0000, 0x43C1_0000, 0x43C2_0000];

/// Per-window byte offset within the 64 KB s9io instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum S9ioWindow {
    /// Common registers (VERSION, BUILD_ID, CTRL_REG, BAUD_REG, etc.).
    Common,
    /// Cmd RX/TX FIFOs and control.
    Cmd,
    /// Work RX FIFO (nonce responses).
    WorkRx,
    /// Work TX FIFO (work dispatch) — production-critical path.
    WorkTx,
}

impl S9ioWindow {
    pub const fn base_offset(&self) -> u32 {
        match self {
            S9ioWindow::Common => 0x0000,
            S9ioWindow::Cmd => 0x1000,
            S9ioWindow::WorkRx => 0x2000,
            S9ioWindow::WorkTx => 0x3000,
        }
    }
}

/// One s9io register entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct FpgaRegister {
    /// Sub-window byte offset within the s9io window.
    pub offset: u32,
    /// Operator-facing register name.
    pub name: &'static str,
    /// Access mode for the runtime: R / W / RW.
    pub access: AccessMode,
    /// One-line description for dashboard / docs.
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    R,
    W,
    Rw,
}

/// Common-window registers per RE doc §3 lines 339-349.
pub const COMMON_REGISTERS: &[FpgaRegister] = &[
    FpgaRegister {
        offset: 0x00,
        name: "VERSION",
        access: AccessMode::R,
        description: "[31:28]MINER_TYPE [27:20]MODEL [15:12]MAJOR [11:8]MINOR [7:0]PATCH",
    },
    FpgaRegister {
        offset: 0x04,
        name: "BUILD_ID",
        access: AccessMode::R,
        description: "Unix timestamp of bitstream build",
    },
    FpgaRegister {
        offset: 0x08,
        name: "CTRL_REG",
        access: AccessMode::Rw,
        description: "[4]BM139X [3]ENABLE [2:1]MIDSTATE_CNT [0]ERR_CNT_CLEAR",
    },
    FpgaRegister {
        offset: 0x0C,
        name: "STAT_REG",
        access: AccessMode::R,
        description: "Reserved",
    },
    FpgaRegister {
        offset: 0x10,
        name: "BAUD_REG",
        access: AccessMode::Rw,
        description: "Baud rate divisor (200 MHz / (16 * (N+1)))",
    },
    FpgaRegister {
        offset: 0x14,
        name: "WORK_TIME",
        access: AccessMode::Rw,
        description: "Work delay counter (0-335 ms range)",
    },
    FpgaRegister {
        offset: 0x18,
        name: "ERR_COUNTER",
        access: AccessMode::R,
        description: "CRC error count",
    },
];

/// Command RX/TX registers per RE doc §3 lines 364-371.
pub const CMD_REGISTERS: &[FpgaRegister] = &[
    FpgaRegister {
        offset: 0x000,
        name: "CMD_RX_FIFO",
        access: AccessMode::R,
        description: "Response data (2 words per response)",
    },
    FpgaRegister {
        offset: 0x004,
        name: "CMD_TX_FIFO",
        access: AccessMode::W,
        description: "Command data without CRC (max 9 bytes, 4-byte aligned)",
    },
    FpgaRegister {
        offset: 0x008,
        name: "CMD_CTRL_REG",
        access: AccessMode::Rw,
        description: "[2]IRQ_EN [1]RST_TX_FIFO [0]RST_RX_FIFO",
    },
    FpgaRegister {
        offset: 0x00C,
        name: "CMD_STAT_REG",
        access: AccessMode::R,
        description: "[4]IRQ_PEND [3]TX_FULL [2]TX_EMPTY [1]RX_FULL [0]RX_EMPTY",
    },
];

/// Work RX (nonce response) registers per RE doc §3 lines 376-381.
pub const WORK_RX_REGISTERS: &[FpgaRegister] = &[
    FpgaRegister {
        offset: 0x000,
        name: "WORK_RX_FIFO",
        access: AccessMode::R,
        description: "Nonce response (2 words per response)",
    },
    FpgaRegister {
        offset: 0x008,
        name: "WORK_RX_CTRL_REG",
        access: AccessMode::Rw,
        description: "[2]IRQ_EN [0]RST_RX_FIFO",
    },
    FpgaRegister {
        offset: 0x00C,
        name: "WORK_RX_STAT_REG",
        access: AccessMode::R,
        description: "[4]IRQ_PEND [1]RX_FULL [0]RX_EMPTY",
    },
];

/// Work TX (mining work submission) registers per RE doc §3 lines 386-393.
pub const WORK_TX_REGISTERS: &[FpgaRegister] = &[
    FpgaRegister {
        offset: 0x004,
        name: "WORK_TX_FIFO",
        access: AccessMode::W,
        description: "Mining work submission",
    },
    FpgaRegister {
        offset: 0x008,
        name: "WORK_TX_CTRL_REG",
        access: AccessMode::Rw,
        description: "[2]IRQ_EN [1]RST_TX_FIFO",
    },
    FpgaRegister {
        offset: 0x00C,
        name: "WORK_TX_STAT_REG",
        access: AccessMode::R,
        description: "[4]IRQ_PEND [3]TX_FULL [2]TX_EMPTY",
    },
    FpgaRegister {
        offset: 0x010,
        name: "WORK_TX_IRQ_THR",
        access: AccessMode::Rw,
        description: "IRQ trigger threshold (default ~200 words)",
    },
    FpgaRegister {
        offset: 0x014,
        name: "WORK_TX_LAST_ID",
        access: AccessMode::R,
        description: "Last transmitted work ID",
    },
];

/// Look up the register slice for a window.
pub fn registers_for(window: S9ioWindow) -> &'static [FpgaRegister] {
    match window {
        S9ioWindow::Common => COMMON_REGISTERS,
        S9ioWindow::Cmd => CMD_REGISTERS,
        S9ioWindow::WorkRx => WORK_RX_REGISTERS,
        S9ioWindow::WorkTx => WORK_TX_REGISTERS,
    }
}

/// Compute the absolute physical address for a (chain, window, register).
pub fn absolute_address(chain_idx: usize, window: S9ioWindow, register_offset: u32) -> Option<u32> {
    let base = *S9IO_V102_CHAIN_BASES.get(chain_idx)?;
    Some(base + window.base_offset() + register_offset)
}

/// Convert a 32-bit `BAUD_REG` divisor to actual UART baud per the
/// production v1.0.2 200 MHz formula:
/// `baudrate = 200_000_000 / (16 * (divisor + 1))`.
pub fn baud_from_divisor(divisor: u32) -> u32 {
    if divisor == u32::MAX {
        return 0;
    }
    200_000_000 / (16 * (divisor + 1))
}

/// Reverse: pick the divisor closest to a target baud.
pub fn divisor_from_baud(target_baud: u32) -> u32 {
    if target_baud == 0 {
        return u32::MAX;
    }
    // 16*(N+1) = 200_000_000 / baud  →  N = 200_000_000 / (16*baud) - 1
    let raw = 200_000_000u64 / (16u64 * target_baud as u64);
    if raw == 0 {
        0
    } else {
        (raw - 1) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s9io_v102_has_three_chain_bases() {
        assert_eq!(S9IO_V102_CHAIN_BASES.len(), 3);
        assert_eq!(
            S9IO_V102_CHAIN_BASES,
            [0x43C0_0000, 0x43C1_0000, 0x43C2_0000]
        );
    }

    #[test]
    fn window_offsets_match_re_doc() {
        assert_eq!(S9ioWindow::Common.base_offset(), 0x0000);
        assert_eq!(S9ioWindow::Cmd.base_offset(), 0x1000);
        assert_eq!(S9ioWindow::WorkRx.base_offset(), 0x2000);
        assert_eq!(S9ioWindow::WorkTx.base_offset(), 0x3000);
    }

    #[test]
    fn common_registers_table_has_seven_entries() {
        assert_eq!(COMMON_REGISTERS.len(), 7);
        // Sanity: VERSION at 0x00, ERR_COUNTER at 0x18.
        assert_eq!(COMMON_REGISTERS[0].name, "VERSION");
        assert_eq!(COMMON_REGISTERS[0].offset, 0x00);
        assert_eq!(COMMON_REGISTERS.last().unwrap().name, "ERR_COUNTER");
    }

    #[test]
    fn ctrl_reg_is_at_offset_0x08() {
        // Critical: CTRL_REG bit 3 = ENABLE (per  B1 fpga.rs fix).
        let ctrl = registers_for(S9ioWindow::Common)
            .iter()
            .find(|r| r.name == "CTRL_REG")
            .unwrap();
        assert_eq!(ctrl.offset, 0x08);
        assert_eq!(ctrl.access, AccessMode::Rw);
    }

    fn approx_eq(actual: u32, expected: u32, tol: u32) -> bool {
        actual.abs_diff(expected) <= tol
    }

    #[test]
    fn baud_divisor_115200_matches_re_doc() {
        // RE doc line 357: BAUD_REG=0x6C (108) → 114,679 baud
        // (≈ 115200). Allow ±1 for integer truncation
        // (200,000,000 / 1744 = 114,678.89...).
        let baud = baud_from_divisor(0x6C);
        assert!(
            approx_eq(baud, 114_679, 1),
            "got {} baud; expected ~114,679 (±1)",
            baud
        );
    }

    #[test]
    fn baud_divisor_1mhz_matches_re_doc() {
        // RE doc line 358: BAUD_REG=0x07 (7) → 1,562,500 baud (S9
        // operational speed).
        let baud = baud_from_divisor(0x07);
        // 200_000_000 / (16 * 8) = 1,562,500.
        assert_eq!(baud, 1_562_500);
    }

    #[test]
    fn baud_divisor_3mhz_matches_re_doc() {
        // RE doc line 359: BAUD_REG=0x03 (3) → 3,125,000 baud (max tested).
        let baud = baud_from_divisor(0x03);
        // 200_000_000 / (16 * 4) = 3,125,000.
        assert_eq!(baud, 3_125_000);
    }

    #[test]
    fn divisor_from_target_baud_round_trips_for_canonical_rates() {
        // Symmetric round-trip for the canonical S9 rates.
        // 1.5625 Mbaud → divisor 7
        let d = divisor_from_baud(1_562_500);
        assert_eq!(d, 7);
        // 3.125 Mbaud → divisor 3
        let d = divisor_from_baud(3_125_000);
        assert_eq!(d, 3);
    }

    #[test]
    fn absolute_address_for_chain_0_work_tx_fifo() {
        // Chain 0 (base 0x43C0_0000) + WorkTx window (0x3000) +
        // WORK_TX_FIFO offset (0x004) = 0x43C0_3004.
        let addr = absolute_address(0, S9ioWindow::WorkTx, 0x004).unwrap();
        assert_eq!(addr, 0x43C0_3004);
    }

    #[test]
    fn absolute_address_for_chain_2_work_rx_fifo() {
        // Chain 2 (base 0x43C2_0000) + WorkRx (0x2000) +
        // WORK_RX_FIFO (0x000) = 0x43C2_2000.
        let addr = absolute_address(2, S9ioWindow::WorkRx, 0x000).unwrap();
        assert_eq!(addr, 0x43C2_2000);
    }

    #[test]
    fn absolute_address_returns_none_for_invalid_chain() {
        assert!(absolute_address(3, S9ioWindow::Common, 0).is_none());
        assert!(absolute_address(99, S9ioWindow::Common, 0).is_none());
    }

    #[test]
    fn work_tx_fifo_lives_at_canonical_offset_0x004() {
        // The runtime mining loop writes to this register; pin the
        // offset so a refactor doesn't accidentally move it.
        let work_tx = registers_for(S9ioWindow::WorkTx)
            .iter()
            .find(|r| r.name == "WORK_TX_FIFO")
            .unwrap();
        assert_eq!(work_tx.offset, 0x004);
    }

    #[test]
    fn s9io_window_round_trips_through_serde() {
        for w in [
            S9ioWindow::Common,
            S9ioWindow::Cmd,
            S9ioWindow::WorkRx,
            S9ioWindow::WorkTx,
        ] {
            let json = serde_json::to_string(&w).unwrap();
            let back: S9ioWindow = serde_json::from_str(&json).unwrap();
            assert_eq!(w, back);
        }
    }

    #[test]
    fn fpga_register_serializes_to_documented_shape() {
        let r = registers_for(S9ioWindow::WorkTx)
            .iter()
            .find(|r| r.name == "WORK_TX_FIFO")
            .unwrap();
        let json = serde_json::to_string(r).unwrap();
        assert!(json.contains("\"name\":\"WORK_TX_FIFO\""));
        assert!(json.contains("\"offset\":4"));
        assert!(json.contains("\"access\":\"w\""));
    }
}
