//! Stock Bitmain FPGA register access.
//!
//! Provides direct mmap access to the stock Bitmain S9 FPGA register block
//! at physical address 0x43C00000 (352 bytes) via /dev/axi_fpga_dev (major 245).
//!
//! This is the stock FPGA backend -- fundamentally different from the BraiinsOS
//! s9io FPGA used by `FpgaChain`:
//!
//!   - **Single flat register space** (352 bytes) for ALL 3 chains, vs BraiinsOS's
//!     per-chain register blocks (4 KB each).
//!   - **ASIC commands** via BC_WRITE_COMMAND (offset 0x0C0), vs BraiinsOS's
//!     per-chain CMD TX/RX FIFOs.
//!   - **Work dispatch** via DHASH accelerator + DMA (0x1F000000), vs BraiinsOS's
//!     per-chain Work TX FIFOs.
//!   - **Nonce collection** via shared RETURN_NONCE FIFO (offset 0x010), vs
//!     BraiinsOS's per-chain Work RX FIFOs.
//!   - **PIC I2C** via FPGA IIC_COMMAND register (offset 0x030), vs BraiinsOS's
//!     Xilinx AXI IIC IP at 0x41600000.
//!   - **Fan control** via FPGA FAN_CONTROL register (offset 0x084), vs BraiinsOS's
//!     AXI Timer PWM at 0x42800000.
//!
//! Register map source: S9_STOCK_FPGA_REGISTER_MAP.md (live probe of .82, V1.3.39)
//! and S9_STOCK_BMMINER_RE.md (bmminer symbol extraction).
//!
//! Device files:
//!   /dev/axi_fpga_dev  (major 245) - FPGA registers, mmap to 0x43C00000 (0x160 bytes)
//!   /dev/fpga_mem      (major 244) - DMA buffers, mmap to 0x1F000000 (16 MB)

use std::num::NonZeroUsize;

use nix::sys::mman::{MapFlags, ProtFlags};

use crate::{HalError, Result};

// ---------------------------------------------------------------------------
// Physical addresses and sizes
// ---------------------------------------------------------------------------

/// FPGA register block physical base address.
pub const FPGA_REGS_PHYS: u64 = 0x43C0_0000;

/// FPGA register block size (352 bytes = 0x160).
pub const FPGA_REGS_SIZE: usize = 0x160;

/// DMA buffer physical base address (512 MB boards).
/// For 256 MB boards, this is 0x0F000000. For 1 GB boards, 0x3F000000.
pub const DMA_PHYS_BASE_512M: u64 = 0x1F00_0000;

/// DMA buffer total size (16 MB).
pub const DMA_SIZE: usize = 0x0100_0000;

/// Device path for FPGA register access.
pub const DEV_AXI_FPGA: &str = "/dev/axi_fpga_dev";

/// Device path for DMA buffer access.
pub const DEV_FPGA_MEM: &str = "/dev/fpga_mem";

// ---------------------------------------------------------------------------
// Register offsets (byte offsets from 0x43C00000)
// ---------------------------------------------------------------------------

/// FPGA version register. Stock S9 reads 0x0000C51E (V1.3.39).
/// Byte 1 (0xC5) = board type (Zynq S9), Byte 0 (0x1E) = version.
pub const REG_HARDWARE_VERSION: u32 = 0x000;

/// Fan speed readback (cycles through fans on successive reads).
/// Bits [15:8] = fan ID, Bits [7:0] = speed data.
pub const REG_FAN_SPEED: u32 = 0x004;

/// Hash board plug detect. Bit 7=J8, Bit 6=J7, Bit 5=J6.
/// 0xE0 = all 3 boards present.
pub const REG_HASH_ON_PLUG: u32 = 0x008;

/// Available work buffer space (mirrors HASH_ON_PLUG when idle).
pub const REG_BUFFER_SPACE: u32 = 0x00C;

/// Nonce FIFO read port (32-bit nonce value).
pub const REG_RETURN_NONCE: u32 = 0x010;

/// Extended nonce data (solution_idx + work_id).
pub const REG_RETURN_NONCE_EXT: u32 = 0x014;

/// Pending nonce count in FIFO (max 0x1FF).
pub const REG_NONCE_NUMBER_IN_FIFO: u32 = 0x018;

/// Nonce FIFO interrupt control.
/// Bit 23 = IRQ enable, Bit 16 = flush FIFO.
pub const REG_NONCE_FIFO_INTERRUPT: u32 = 0x01C;

/// Temperature registers (not used on S9 -- temp via ASIC I2C passthrough).
pub const REG_TEMPERATURE_0_3: u32 = 0x020;

/// IIC_COMMAND register for PIC I2C communication.
/// This is the FPGA-integrated I2C master, NOT the AXI IIC controller.
/// Volatile -- changes with PIC heartbeat activity.
pub const REG_IIC_COMMAND: u32 = 0x030;

/// Hash board reset control.
pub const REG_RESET_HASHBOARD: u32 = 0x034;

/// BMC command counter.
pub const REG_BMC_CMD_COUNTER: u32 = 0x038;

/// QN write data command (chain enables + board config).
/// Idle: 0x0080800F = all chains enabled.
pub const REG_QN_WRITE_DATA_COMMAND: u32 = 0x080;

/// Fan PWM control register.
/// Bits [23:16] = PWM duty (0-255), Bits [7:0] = fan scale/prescaler.
pub const REG_FAN_CONTROL: u32 = 0x084;

/// ASIC response timeout.
/// Bit 31 = enabled, Bits [15:0] = timeout cycles.
pub const REG_TIME_OUT_CONTROL: u32 = 0x088;

/// FPGA-level difficulty ticket mask.
/// 0x00 = disabled, 0x0F = diff16.
pub const REG_TICKET_MASK: u32 = 0x08C;

/// Hash counting number (FPGA hash rate counter).
pub const REG_HASH_COUNTING_NUMBER: u32 = 0x090;

/// Broadcast command write register.
/// Used for ASIC register reads/writes (frequency, chip config).
pub const REG_BC_WRITE_COMMAND: u32 = 0x0C0;

/// Broadcast command data buffer.
pub const REG_BC_COMMAND_BUFFER: u32 = 0x0C4;

/// FPGA chip ID (64-bit, split across two registers).
pub const REG_FPGA_CHIP_ID_LO: u32 = 0x0F0;
pub const REG_FPGA_CHIP_ID_HI: u32 = 0x0F4;

/// CRC error counter.
pub const REG_CRC_ERROR_CNT: u32 = 0x0F8;

/// DHASH accelerator control register.
/// Idle: 0x00000020 (init flag), Mining: 0x00008160 (VIL + run).
/// Bit 15 = VIL mode, Bit 12 = multi-midstate (AsicBoost), Bit 8 = run.
pub const REG_DHASH_ACC_CONTROL: u32 = 0x100;

/// Coinbase length + nonce2 length packed register.
pub const REG_COINBASE_AND_NONCE2_LENGTH: u32 = 0x104;

/// Current nonce2 counter value.
pub const REG_WORK_NONCE2: u32 = 0x108;

/// DMA base address for nonce2/jobid storage.
/// Default: 0x1F000000.
pub const REG_NONCE2_AND_JOBID_STORE_ADDRESS: u32 = 0x110;

/// Number of merkle branches.
pub const REG_MERKLE_BIN_NUMBER: u32 = 0x114;

/// Physical DDR address of job data in DMA buffer.
/// Points to one of the two double-buffer slots.
pub const REG_JOB_START_ADDRESS: u32 = 0x118;

/// Job data length in bytes (e.g., 0x340 = 832 bytes).
pub const REG_JOB_LENGTH: u32 = 0x11C;

/// Write 1 to signal FPGA that new job data is ready at JOB_START_ADDRESS.
pub const REG_JOB_DATA_READY: u32 = 0x120;

/// Current job ID (incremented by software for each new job).
pub const REG_JOB_ID: u32 = 0x124;

/// Block header version (with AsicBoost version bits).
/// 4 consecutive registers for 4-way AsicBoost: 0x130, 0x134, 0x138, 0x13C.
pub const REG_BLOCK_HEADER_VERSION: u32 = 0x130;

/// ntime value.
pub const REG_TIME_STAMP: u32 = 0x134;

/// nbits (compact difficulty target).
pub const REG_TARGET_BITS: u32 = 0x138;

/// Midstate / previous block hash (8 x 32-bit words).
/// Registers 0x140 through 0x15C.
pub const REG_PRE_HEADER_HASH_BASE: u32 = 0x140;

/// Number of midstate words.
pub const PRE_HEADER_HASH_WORDS: usize = 8;

// ---------------------------------------------------------------------------
// DHASH_ACC_CONTROL bit definitions
// ---------------------------------------------------------------------------

/// VIL (Variable Input Length) mode bit.
pub const DHASH_VIL_MODE: u32 = 1 << 15;

/// New block flag bit.
pub const DHASH_NEW_BLOCK: u32 = 1 << 13;

/// Multi-midstate enable (AsicBoost).
pub const DHASH_MULTI_MIDSTATE: u32 = 1 << 12;

/// Run bit (DHASH accelerator running).
pub const DHASH_RUN: u32 = 1 << 8;

/// Init/ready flag.
pub const DHASH_INIT: u32 = 1 << 5;

/// Typical mining control value: VIL + run + init.
/// From live probe: 0x8160 = VIL(15) | run(8) | bits 6:5.
pub const DHASH_MINING_VIL: u32 = 0x8160;

// ---------------------------------------------------------------------------
// NONCE_FIFO_INTERRUPT bit definitions
// ---------------------------------------------------------------------------

/// IRQ enable bit.
pub const NONCE_IRQ_ENABLE: u32 = 1 << 23;

/// Flush FIFO bit (write 1 to flush).
pub const NONCE_FIFO_FLUSH: u32 = 1 << 16;

// ---------------------------------------------------------------------------
// Board version identifiers
// ---------------------------------------------------------------------------

/// Expected HARDWARE_VERSION high byte for Zynq S9.
pub const BOARD_TYPE_C5: u16 = 0xC5;

#[inline]
pub(crate) fn stock_fpga_register_offset_valid(offset: u32, region_size: usize) -> bool {
    offset.is_multiple_of(4)
        && (offset as usize)
            .checked_add(std::mem::size_of::<u32>())
            .is_some_and(|end| end <= region_size)
}

// ---------------------------------------------------------------------------
// StockFpga — core register access
// ---------------------------------------------------------------------------

/// Stock Bitmain FPGA register access via /dev/axi_fpga_dev.
///
/// Provides volatile read/write access to the 352-byte FPGA register block
/// at physical address 0x43C00000. All 3 hash chains share this single
/// register space (unlike BraiinsOS which has per-chain register blocks).
pub struct StockFpga {
    /// mmap'd pointer to FPGA registers (352 bytes).
    regs: *mut u32,
    /// File handle for /dev/axi_fpga_dev (kept open for mmap lifetime).
    _regs_file: std::fs::File,
    /// Register region size.
    regs_size: usize,
}

// SAFETY: StockFpga holds an mmap'd pointer that is process-global.
// Register access concurrency is the caller's responsibility.
unsafe impl Send for StockFpga {}
unsafe impl Sync for StockFpga {}

impl StockFpga {
    /// Open the stock FPGA register interface.
    ///
    /// Opens /dev/axi_fpga_dev and mmaps the 352-byte register block.
    /// Verifies the FPGA is responding by reading HARDWARE_VERSION.
    pub fn open() -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(DEV_AXI_FPGA)
            .map_err(|e| HalError::DeviceOpen {
                path: DEV_AXI_FPGA.to_string(),
                source: e,
            })?;

        // The kernel module maps 0x160 bytes. We request a full page (4096)
        // for mmap alignment, but only access the first 0x160 bytes.
        let map_size = 4096;
        let ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                NonZeroUsize::new(map_size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &file,
                0,
            )
            .map_err(|e| HalError::MmapFailed {
                device: DEV_AXI_FPGA.to_string(),
                source: e,
            })?
        };

        let fpga = Self {
            regs: ptr.as_ptr() as *mut u32,
            _regs_file: file,
            regs_size: FPGA_REGS_SIZE,
        };

        // Verify FPGA is alive
        let version = fpga.read_reg(REG_HARDWARE_VERSION);
        let board_type = ((version >> 8) & 0xFF) as u16;

        tracing::info!(
            version = format_args!("0x{:08X}", version),
            board_type = format_args!("0x{:02X}", board_type),
            "Stock FPGA opened: HARDWARE_VERSION=0x{:08X}",
            version,
        );

        if board_type != BOARD_TYPE_C5 {
            tracing::warn!(
                expected = format_args!("0x{:02X}", BOARD_TYPE_C5),
                actual = format_args!("0x{:02X}", board_type),
                "FPGA board type mismatch (expected C5 for S9, got 0x{:02X})",
                board_type,
            );
        }

        Ok(fpga)
    }

    /// Read a 32-bit register at the given byte offset.
    ///
    /// Invalid offsets are rejected in every build and read as zero rather
    /// than entering the volatile load.
    #[inline]
    pub fn read_reg(&self, offset: u32) -> u32 {
        if !stock_fpga_register_offset_valid(offset, self.regs_size) {
            tracing::error!(
                offset = format_args!("0x{:04X}", offset),
                size = format_args!("0x{:X}", self.regs_size),
                "stock FPGA register read rejected: offset out of bounds or misaligned"
            );
            return 0;
        }

        unsafe {
            let ptr = self.regs.add((offset / 4) as usize);
            std::ptr::read_volatile(ptr)
        }
    }

    /// Write a 32-bit value to a register at the given byte offset.
    ///
    /// Invalid offsets are rejected in every build and the write is skipped
    /// rather than entering the volatile store.
    #[inline]
    pub fn write_reg(&self, offset: u32, value: u32) {
        if !stock_fpga_register_offset_valid(offset, self.regs_size) {
            tracing::error!(
                offset = format_args!("0x{:04X}", offset),
                size = format_args!("0x{:X}", self.regs_size),
                value = format_args!("0x{:08X}", value),
                "stock FPGA register write rejected: offset out of bounds or misaligned"
            );
            return;
        }

        unsafe {
            let ptr = self.regs.add((offset / 4) as usize);
            std::ptr::write_volatile(ptr, value);
        }
    }

    /// Read the FPGA hardware version register.
    pub fn read_version(&self) -> u32 {
        self.read_reg(REG_HARDWARE_VERSION)
    }

    /// Read the FPGA chip ID (64-bit unique identifier).
    pub fn read_chip_id(&self) -> u64 {
        let lo = self.read_reg(REG_FPGA_CHIP_ID_LO) as u64;
        let hi = self.read_reg(REG_FPGA_CHIP_ID_HI) as u64;
        (hi << 32) | lo
    }

    /// Read the hash board plug detect register.
    /// Returns a bitmask where bit 5=J6, bit 6=J7, bit 7=J8.
    pub fn read_hash_on_plug(&self) -> u32 {
        self.read_reg(REG_HASH_ON_PLUG)
    }

    /// Check if a specific chain connector has a board plugged in.
    ///
    /// Stock firmware uses chains 5, 6, 7 (bits 5, 6, 7 in HASH_ON_PLUG).
    /// These map to physical connectors J6, J7, J8 respectively.
    pub fn is_board_present(&self, stock_chain_id: u8) -> bool {
        let plug = self.read_hash_on_plug();
        plug & (1 << stock_chain_id) != 0
    }

    /// Read the CRC error counter.
    pub fn read_crc_errors(&self) -> u32 {
        self.read_reg(REG_CRC_ERROR_CNT)
    }

    /// Read the hash counting number (FPGA hash rate counter).
    pub fn read_hash_count(&self) -> u32 {
        self.read_reg(REG_HASH_COUNTING_NUMBER)
    }

    /// Set the ASIC response timeout.
    ///
    /// Stock default: 0x9C40 (40000 cycles). Mining: 0x800002E4 (bit 31 = enabled).
    pub fn set_timeout(&self, value: u32) {
        self.write_reg(REG_TIME_OUT_CONTROL, value);
    }

    /// Set the FPGA ticket mask (difficulty filter).
    ///
    /// 0x00 = disabled, 0x0F = diff16. The FPGA filters nonces below this
    /// difficulty before putting them in the nonce FIFO.
    pub fn set_ticket_mask(&self, mask: u32) {
        self.write_reg(REG_TICKET_MASK, mask);
    }

    /// Reset all hash boards via RESET_HASHBOARD register.
    pub fn reset_all_hashboards(&self) {
        self.write_reg(REG_RESET_HASHBOARD, 0x0000_FFFF);
    }

    /// Set the QN write data command (chain enables + board config).
    pub fn set_qn_write_data(&self, value: u32) {
        self.write_reg(REG_QN_WRITE_DATA_COMMAND, value);
    }
}

impl Drop for StockFpga {
    fn drop(&mut self) {
        // Leave register unmapping to process exit cleanup.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_fpga_register_offset_guard_rejects_oob_and_misaligned_offsets() {
        assert!(stock_fpga_register_offset_valid(0, FPGA_REGS_SIZE));
        assert!(stock_fpga_register_offset_valid(
            (FPGA_REGS_SIZE - std::mem::size_of::<u32>()) as u32,
            FPGA_REGS_SIZE
        ));

        assert!(!stock_fpga_register_offset_valid(1, FPGA_REGS_SIZE));
        assert!(!stock_fpga_register_offset_valid(
            FPGA_REGS_SIZE as u32,
            FPGA_REGS_SIZE
        ));
        assert!(!stock_fpga_register_offset_valid(
            u32::MAX - 1,
            FPGA_REGS_SIZE
        ));
    }
}
