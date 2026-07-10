//! FPGA chain register access.
//!
//! Wraps 4 UioDevice instances representing one hash chain's register blocks:
//! common, cmd, work_rx, and work_tx. Each block is 4 KB (one UIO device).
//!
//! Register layout verified from live S9 probe with Braiins s9io v1.0.2.
//!
//! Chain base addresses (production Zynq):
//!   Chain 6: 0x43C00000 - 0x43C03FFF
//!   Chain 7: 0x43C10000 - 0x43C13FFF
//!   Chain 8: 0x43C20000 - 0x43C23FFF

use crate::uio::UioDevice;
use crate::Result;

// ---------------------------------------------------------------------------
// Common Block Register Offsets (+0x0000)
// ---------------------------------------------------------------------------

/// IP core version register (read-only). S9 reads 0x00901002.
pub const REG_VERSION: u32 = 0x00;

/// Bitstream build timestamp (read-only).
pub const REG_BUILD_ID: u32 = 0x04;

/// Control register.
/// Bit 4: BM139X mode (0=BM1387, 1=BM139X/BM136X)
/// Bit 3: ENABLE chain
/// Bits 2:1: MIDSTATE_CNT (0=1, 1=2, 2=4)
/// Bit 0: ERR_CLR (write 1 to clear error counter)
pub const REG_CTRL: u32 = 0x08;

/// Status register (read-only, reserved).
pub const REG_STAT: u32 = 0x0C;

/// Baud rate divisor register.
/// Baud rate = FPGA_CLK_HZ / (16 * (BAUD_REG + 1))
/// FPGA clock is 200 MHz (100 MHz FCLK doubled by PL PLL).
pub const REG_BAUD: u32 = 0x10;

/// Inter-work delay register (reset value = 1).
pub const REG_WORK_TIME: u32 = 0x14;

/// CRC error counter (read-only, cleared by CTRL bit 0).
pub const REG_ERR_COUNTER: u32 = 0x18;

// ---------------------------------------------------------------------------
// CMD Block Register Offsets (+0x1000)
// ---------------------------------------------------------------------------

/// Command RX FIFO (read). Returns 32-bit words, 2 words per response.
pub const REG_CMD_RX_FIFO: u32 = 0x00;

/// Command TX FIFO (write). Accepts 32-bit words, LSB-first byte packing.
pub const REG_CMD_TX_FIFO: u32 = 0x04;

/// CMD control register. Bit 1: RST_TX, Bit 0: RST_RX.
pub const REG_CMD_CTRL: u32 = 0x08;

/// CMD status register (read-only).
/// Bit 4: IRQ, Bit 3: TX_FULL, Bit 2: TX_EMPTY,
/// Bit 1: RX_FULL, Bit 0: RX_EMPTY
pub const REG_CMD_STAT: u32 = 0x0C;

// ---------------------------------------------------------------------------
// Work RX Block Register Offsets (+0x2000)
// ---------------------------------------------------------------------------

/// Work RX FIFO (read). Nonce data: 2 x 32-bit words per nonce.
pub const REG_WORK_RX_FIFO: u32 = 0x00;

/// Work RX control register. Bit 0: RST_RX.
pub const REG_WORK_RX_CTRL: u32 = 0x08;

/// Work RX status register (read-only).
/// Bit 4: IRQ, Bit 1: RX_FULL, Bit 0: RX_EMPTY
pub const REG_WORK_RX_STAT: u32 = 0x0C;

// ---------------------------------------------------------------------------
// Work TX Block Register Offsets (+0x3000)
// ---------------------------------------------------------------------------

/// Work TX FIFO (write). Mining work data (12, 36 or 68 words per job).
/// Verified: both offset 0x00 and 0x04 accept writes on am2 (neither produces nonces).
/// Keeping 0x04 which is proven working on S9.
pub const REG_WORK_TX_FIFO: u32 = 0x04;

/// Work TX control register. Bit 1: RST_TX.
pub const REG_WORK_TX_CTRL: u32 = 0x08;

/// Work TX status register (read-only).
/// Bit 4: IRQ, Bit 3: TX_FULL, Bit 2: TX_EMPTY
pub const REG_WORK_TX_STAT: u32 = 0x0C;

/// Work TX IRQ threshold. Fire IRQ when FIFO level drops below this value.
pub const REG_WORK_TX_THR: u32 = 0x10;

/// Last work ID sent (read-only).
pub const REG_WORK_TX_LAST: u32 = 0x14;

// ---------------------------------------------------------------------------
// FPGA clock and baud constants
// ---------------------------------------------------------------------------

/// FPGA fabric clock frequency in Hz (100 MHz FCLK doubled by PL PLL).
pub const FPGA_CLK_HZ: u32 = 200_000_000;

/// BAUD_REG value for 115200 baud (default for enumeration).
pub const BAUD_REG_115200: u32 = 0x6C;

/// BAUD_REG value for 1.5625 Mbaud (operational speed).
pub const BAUD_REG_1_5M: u32 = 0x07;

/// BAUD_REG value for 3.125 Mbaud (maximum tested).
pub const BAUD_REG_3M: u32 = 0x03;

// ---------------------------------------------------------------------------
// CTRL_REG bit definitions
// ---------------------------------------------------------------------------

/// BM139X mode bit (set for BM1397/BM1366/BM1368/BM1370).
pub const CTRL_BM139X: u32 = 1 << 4;

/// Chain enable bit.
pub const CTRL_ENABLE: u32 = 1 << 3;

/// Error counter clear bit.
pub const CTRL_ERR_CLR: u32 = 1 << 0;

/// CMD CTRL IRQ enable bit (must be set after FIFO reset for proper operation).
pub const CMD_CTRL_IRQ_EN: u32 = 0x04;

/// Default WORK_TIME register value (verified from asic_comm_test.c and bosminer).
pub const WORK_TIME_DEFAULT: u32 = 0x0004_0507;

/// Midstate count field shift.
///
/// **S9 (am1) ONLY.** The am2 (S19/S19j/S19k) bitstream uses a completely
/// different CTRL layout — see `ctrl_am2` module below. Do NOT use this
/// constant on am2 code paths (it will set unrelated bits).
pub const CTRL_MIDSTATE_SHIFT: u32 = 1;

/// am2-s17 FPGA bitstream CTRL_REG bit layout.
///
/// Lives at `chain_base + 0x00` (common block). Captured live from bosminer
/// on S19j Pro .139 on 2026-04-20 during Phase 4A; both populated chains
/// (chain1 @ 0x43C00000 and chain4 @ 0x43C30000) read the identical value
/// `0x00901002` while sustaining ~69 TH/s with zero HW errors.
///
///.
///
/// S9 (am1) uses a different layout (`CTRL_BM139X | CTRL_ENABLE |
/// CTRL_MIDSTATE_SHIFT`) and is NOT compatible with these bits. Keep both
/// layouts separate — do NOT refactor into a shared struct.
pub mod ctrl_am2 {
    /// IP_ENABLE / MINER_EN — core enabled.
    pub const IP_ENABLE: u32 = 1 << 1;

    /// MIDSTATE_CNT (single bit on am2): 0 = 1 midstate, 1 = 2 midstates.
    /// BM1362 ASICBoost uses 2-midstate mode in production.
    pub const MIDSTATE_CNT: u32 = 1 << 20;

    /// Unknown am2-specific feature bit (possibly clock-enable or EXT_BAUD
    /// pre-stage). Live value = 1. See Phase 4A report.
    pub const EXT_BAUD_OR_CLKEN_12: u32 = 1 << 12;

    /// Unknown am2-specific feature bit (possibly BAUD_DIV_SET or
    /// EXT_BAUD_ENABLE post-baud-switch flag). Live value = 1.
    pub const EXT_BAUD_OR_CLKEN_23: u32 = 1 << 23;

    /// Authoritative CTRL value bosminer writes for am2 BM1362 in run state.
    /// = 0x00901002
    pub const BM1362_DEFAULT: u32 =
        IP_ENABLE | EXT_BAUD_OR_CLKEN_12 | MIDSTATE_CNT | EXT_BAUD_OR_CLKEN_23;

    // Compile-time sanity check — breaks the build if the bit math ever drifts.
    const _: () = assert!(BM1362_DEFAULT == 0x0090_1002);
}

/// Defense-in-depth for the load-bearing "never write 0 to FPGA CTRL_REG after
/// UART traffic" brick class (2026-03-12 A/B-proven: zeroing/disabling CTRL on
/// am2 after UART traffic permanently breaks the FPGA UART state machine). A
/// safe am2 CTRL value MUST have IP_ENABLE set; `write_ctrl` refuses 0 (or any
/// IP_ENABLE-clear value) on am2, mirroring the `set_enabled`/`reset_ip_core`
/// am2 refusals. Pure + host-testable. (gap-swarm HAL-safety #5)
#[inline]
pub fn am2_ctrl_value_is_safe(value: u32) -> bool {
    value & ctrl_am2::IP_ENABLE != 0
}

#[cfg(test)]
mod am2_ctrl_guard_tests {
    use super::*;

    #[test]
    fn am2_ctrl_value_is_safe_requires_ip_enable() {
        // The authoritative run-state value + the minimal IP_ENABLE-only value pass.
        assert!(am2_ctrl_value_is_safe(ctrl_am2::BM1362_DEFAULT)); // 0x00901002
        assert!(am2_ctrl_value_is_safe(ctrl_am2::IP_ENABLE));
        // The brick value (0) and any IP_ENABLE-clear value are refused.
        assert!(!am2_ctrl_value_is_safe(0));
        assert!(!am2_ctrl_value_is_safe(ctrl_am2::MIDSTATE_CNT)); // bit 20 set, IP_ENABLE clear
    }
}

/// am2-s17 FPGA bitstream register offsets (common block, +0x0000).
///
/// The am2 bitstream uses a DIFFERENT common-block layout than S9 (am1).
/// Phase 5A live probe (2026-04-20) confirmed on S19j Pro .139:
///
///   - `common + 0x00` = `0x00901002`  (CTRL — live, active)
///   - `common + 0x04` = `0x63848B7B`  (BUILD — bitstream timestamp)
///   - `common + 0x08` = `0x00000000`  (UNUSED on am2)
///   - `common + 0x14` = `0x00000001`  (MIDSTATE_CNT flag, RO, NOT WORK_TIME)
///
/// S9 (am1) layout for comparison:
///
///   - `common + 0x00` = VERSION (0x00901002)
///   - `common + 0x08` = CTRL
///   - `common + 0x14` = WORK_TIME
///
/// Root cause of Phase 4 failure: HAL read/wrote CTRL at +0x08 (S9 offset)
/// on am2, where nothing is mapped. See
///
/// and `19-pre-phase5-baseline.md` for the hypothesis confirmation.
pub mod am2_regs {
    /// CTRL register on am2 lives at the base of the common block.
    pub const REG_CTRL: u32 = 0x00;

    /// Bitstream build timestamp on am2 (equivalent to S9's REG_BUILD_ID).
    pub const REG_BUILD: u32 = 0x04;

    /// Read-only MIDSTATE_CNT status flag (lives where S9 had WORK_TIME).
    /// Writing here on am2 would clobber the flag — callers must never issue
    /// WORK_TIME writes on am2 (WORK_TIME is inline in the work-tx payload).
    pub const REG_MIDSTATE_CNT_FLAG: u32 = 0x14;
}

/// Where WORK_TIME belongs for a chain's FPGA generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkTimePlacement {
    /// S9/am1 exposes WORK_TIME as a common-block MMIO register.
    CommonMmio(u32),
    /// am2 embeds WORK_TIME in the WORK_TX frame; common+0x14 is MIDSTATE_CNT.
    InlineWorkFrame,
}

pub const fn work_time_placement(is_am2: bool) -> WorkTimePlacement {
    if is_am2 {
        WorkTimePlacement::InlineWorkFrame
    } else {
        WorkTimePlacement::CommonMmio(REG_WORK_TIME)
    }
}

// ---------------------------------------------------------------------------
// CMD_STAT and WORK_*_STAT bit definitions
// ---------------------------------------------------------------------------

/// IRQ pending bit.
pub const STAT_IRQ: u32 = 1 << 4;

/// TX FIFO full.
pub const STAT_TX_FULL: u32 = 1 << 3;

/// TX FIFO empty.
pub const STAT_TX_EMPTY: u32 = 1 << 2;

/// RX FIFO full.
pub const STAT_RX_FULL: u32 = 1 << 1;

/// RX FIFO empty.
pub const STAT_RX_EMPTY: u32 = 1 << 0;

/// Chain 7 physical base address (used for early boot detection via /dev/mem).
pub const CHAIN7_PHYS_BASE: u64 = 0x43C1_0000;

/// Size of each `DevmemFpgaChain` sub-block mmap region (one 4 KB page).
/// Each of the 4 `ptrs` entries maps exactly one page in `open`. FPGA-3 bounds
/// checks gate every register access against this window.
const DEVMEM_BLOCK_MAP_SIZE: usize = 4096;

/// Number of mmap'd sub-blocks per `DevmemFpgaChain` (common, cmd, work_rx, work_tx).
const DEVMEM_BLOCK_COUNT: usize = 4;

/// FPGA-3: pure, host-testable predicate for a `DevmemFpgaChain` register
/// access. A `(block, offset)` is valid only if `block` indexes one of the 4
/// mapped sub-block pages AND `offset` is an aligned, fully-in-bounds 32-bit
/// word within that page. `read_reg`/`write_reg` gate every volatile access on
/// this so an out-of-range access can never fault the AXI bus.
#[inline]
fn devmem_access_in_bounds(block: usize, offset: u32) -> bool {
    block < DEVMEM_BLOCK_COUNT && crate::uio::offset_in_bounds(offset, DEVMEM_BLOCK_MAP_SIZE)
}

/// Sub-block indices for DevmemFpgaChain mmap regions.
const BLOCK_COMMON: usize = 0;
#[allow(dead_code)]
const BLOCK_CMD: usize = 1;
const BLOCK_WORK_RX: usize = 2;
const BLOCK_WORK_TX: usize = 3;

/// `DCENT_AM2_WRITE_WORK_TIME_MMIO` — am2 WORK_TIME experiment gate.
///
/// The established Phase 4A position is that `common+0x14` on am2 is a
/// read-only MIDSTATE_CNT flag and WORK_TIME is inline in the work-tx payload,
/// so `set_work_time` is a no-op on am2. But the `a lab unit` live probe
/// `19-pre-phase5-baseline.md` recorded `common+0x14 = 0x0002BE2B` on a
/// *mining* chain (a WORK_TIME-shaped value) vs `0x1` on an *idle* chain —
/// i.e. `0x1` may just be the idle reset value, not a status flag. When this
/// env flag is set, `set_work_time` on am2 reads `common+0x14`, writes the
/// computed WORK_TIME, and reads it back so a bench run can resolve the
/// contradiction definitively (writing a read-only register is a harmless
/// no-op; if the readback sticks, it is a real RW WORK_TIME register).
/// Default-off — does not change `a lab unit` / `a lab unit` behaviour.
fn am2_write_work_time_mmio_enabled() -> bool {
    std::env::var("DCENT_AM2_WRITE_WORK_TIME_MMIO")
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// FPGA chain access via /dev/mem mmap (no UIO required).
///
/// Used when UIO devices are not available for a chain (e.g., S19j Pro chain 2
/// has no UIO devices, but the FPGA registers exist at 0x43C10000).
/// Provides the same register read/write API as `FpgaChain` but via direct
/// physical memory mapping.
pub struct DevmemFpgaChain {
    ptrs: [*mut u8; 4], // [common, cmd, work_rx, work_tx]
    pub chain_id: u8,
    /// True if this chain is on the am2 (S19/S19j/S19k) bitstream variant.
    /// When true, CTRL_REG uses the `ctrl_am2` bit layout and WORK_TIME is
    /// embedded in the work-tx FIFO payload rather than a dedicated MMIO reg.
    /// See `ctrl_am2` module and Phase 4A report.
    pub is_am2: bool,
}

// SAFETY: DevmemFpgaChain uses raw pointers to mmap'd memory which is
// thread-safe for volatile reads/writes (same as UioDevice).
unsafe impl Send for DevmemFpgaChain {}
unsafe impl Sync for DevmemFpgaChain {}

impl DevmemFpgaChain {
    /// Open an am2 (S19/S19j/S19k) FPGA chain via /dev/mem.
    ///
    /// Sets `is_am2 = true` so callers can route to the am2 CTRL layout and
    /// skip the S9-only WORK_TIME MMIO write (WORK_TIME is inline in the
    /// work-tx FIFO payload on am2 — see Phase 4A report).
    pub fn open_am2(chain_id: u8, phys_base: u64) -> Result<Self> {
        let mut chain = Self::open(chain_id, phys_base)?;
        chain.is_am2 = true;
        // Re-read CTRL and BUILD now that the am2 routing is active so the log
        // line actually reflects the am2 register map (+0x00 CTRL / +0x04 BUILD).
        let ctrl = chain.read_ctrl();
        let build = chain.read_build();
        tracing::info!(
            chain_id,
            ctrl = format_args!("0x{:08X}", ctrl),
            build = format_args!("0x{:08X}", build),
            "Marked FPGA chain as am2 (CTRL at common+0x00, BUILD at common+0x04, inline WORK_TIME)"
        );
        Ok(chain)
    }

    /// Open an FPGA chain by its physical base address via /dev/mem.
    ///
    /// Maps 4 x 4KB pages at phys_base + {0x0000, 0x1000, 0x2000, 0x3000}
    /// corresponding to common, cmd, work_rx, work_tx register blocks.
    ///
    /// Defaults `is_am2 = false` (S9/am1 layout). Use `open_am2` for am2
    /// hardware, or set the flag directly if needed.
    pub fn open(chain_id: u8, phys_base: u64) -> Result<Self> {
        use nix::sys::mman::{MapFlags, ProtFlags};
        use std::num::NonZeroUsize;

        tracing::info!(
            chain_id,
            phys_base = format_args!("0x{:08X}", phys_base),
            "Opening FPGA chain via /dev/mem"
        );

        let mem_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(crate::HalError::Io)?;

        let page_size = NonZeroUsize::new(4096).unwrap();
        let mut ptrs = [std::ptr::null_mut::<u8>(); 4];

        for (i, slot) in ptrs.iter_mut().enumerate() {
            let offset = phys_base + (i as u64 * 0x1000);
            let ptr = unsafe {
                nix::sys::mman::mmap(
                    None,
                    page_size,
                    ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                    MapFlags::MAP_SHARED,
                    &mem_file,
                    offset as nix::libc::off_t,
                )
            }
            .map_err(|e| {
                crate::HalError::Other(format!("mmap failed at 0x{:08X}: {}", offset, e))
            })?;
            *slot = ptr.as_ptr() as *mut u8;
        }

        let chain = Self {
            ptrs,
            chain_id,
            is_am2: false,
        };

        let version = chain.read_version();
        tracing::info!(
            chain_id,
            version = format_args!("0x{:08X}", version),
            "FPGA chain opened (devmem)"
        );

        Ok(chain)
    }

    /// Read a 32-bit register from a sub-block.
    ///
    /// FPGA-3 safety: each sub-block is a single 4 KB `/dev/mem` mmap page (see
    /// `open`). Unlike `UioDevice`, this path previously had NO bounds guard at
    /// all — an out-of-range `offset` would read past the mapped page and fault
    /// the AXI bus (process crash). We now validate `block` and `offset` against
    /// the mapped region before the volatile access and return `0` (skip) on a
    /// bad access. All real callers use the fixed `BLOCK_*` indices + small
    /// const offsets, so the valid path is unchanged.
    fn read_reg(&self, block: usize, offset: u32) -> u32 {
        if !devmem_access_in_bounds(block, offset) {
            tracing::error!(
                chain_id = self.chain_id,
                block,
                offset = format_args!("0x{:04X}", offset),
                "DevmemFpgaChain read_reg refused: block/offset out of bounds (returning 0)"
            );
            return 0;
        }
        unsafe { std::ptr::read_volatile(self.ptrs[block].add(offset as usize) as *const u32) }
    }

    /// Write a 32-bit register to a sub-block.
    ///
    /// FPGA-3 safety: bounds-checked exactly like `read_reg` (was previously
    /// unguarded). On an out-of-range `block`/`offset` the write is logged and
    /// skipped rather than corrupting unmapped FPGA space / faulting the bus.
    fn write_reg(&self, block: usize, offset: u32, value: u32) {
        if !devmem_access_in_bounds(block, offset) {
            tracing::error!(
                chain_id = self.chain_id,
                block,
                offset = format_args!("0x{:04X}", offset),
                value = format_args!("0x{:08X}", value),
                "DevmemFpgaChain write_reg refused: block/offset out of bounds (skipping write)"
            );
            return;
        }
        unsafe {
            std::ptr::write_volatile(self.ptrs[block].add(offset as usize) as *mut u32, value);
        }
    }

    /// Read the FPGA IP core version / build marker.
    ///
    /// **S9 (am1)**: common+0x00 holds the IP core version (e.g. `0x00901002`).
    /// **am2 (S19/S19j/S19k)**: common+0x00 is actually CTRL, and the bitstream
    /// build timestamp lives at common+0x04 (see `am2_regs::REG_BUILD`). This
    /// helper returns the "identity" word most callers expect:
    ///   - am1 → VERSION at +0x00
    ///   - am2 → BUILD at +0x04
    /// Callers that want the raw CTRL on am2 must use `read_ctrl()`.
    pub fn read_version(&self) -> u32 {
        if self.is_am2 {
            self.read_reg(BLOCK_COMMON, am2_regs::REG_BUILD)
        } else {
            self.read_reg(BLOCK_COMMON, REG_VERSION)
        }
    }

    /// Read the bitstream build timestamp (am2 only).
    ///
    /// On am1 (S9) the equivalent register is `REG_BUILD_ID` at common+0x04 and
    /// this helper returns it too for symmetry. Both variants have BUILD at
    /// +0x04 — only the CTRL offset differs between platforms.
    pub fn read_build(&self) -> u32 {
        self.read_reg(BLOCK_COMMON, REG_BUILD_ID)
    }

    /// Read the CTRL register.
    ///
    /// Routes to the correct offset based on `is_am2`:
    ///   - am1 (S9):  common + 0x08 (`REG_CTRL`)
    ///   - am2 (S19): common + 0x00 (`am2_regs::REG_CTRL`)
    ///
    /// This is the #1 Phase 4 root cause fix — previously read-modify-write on
    /// CTRL hit the wrong offset on am2, so the bitstream never saw
    /// IP_ENABLE/MIDSTATE_CNT updates and nonces never came back.
    pub fn read_ctrl(&self) -> u32 {
        let offset = if self.is_am2 {
            am2_regs::REG_CTRL
        } else {
            REG_CTRL
        };
        self.read_reg(BLOCK_COMMON, offset)
    }

    /// Write the CTRL register.
    ///
    /// Routes to the correct offset based on `is_am2` (see `read_ctrl`).
    /// On am2, writes go to common+0x00 — the bit layout must be the
    /// `ctrl_am2` module's (IP_ENABLE | MIDSTATE_CNT | two clk/ext-baud bits),
    /// NOT the S9 `CTRL_BM139X | CTRL_ENABLE | ...` layout.
    pub fn write_ctrl(&self, value: u32) {
        if self.is_am2 && !am2_ctrl_value_is_safe(value) {
            tracing::error!(
                chain_id = self.chain_id,
                value = format_args!("0x{:08X}", value),
                "refusing am2 CTRL write without IP_ENABLE — zeroing/disabling CTRL after UART traffic permanently bricks the FPGA UART state machine (gap-swarm HAL-safety #5; see set_enabled)"
            );
            return;
        }
        let offset = if self.is_am2 {
            am2_regs::REG_CTRL
        } else {
            REG_CTRL
        };
        self.write_reg(BLOCK_COMMON, offset, value);
    }

    /// Read a raw 32-bit word at an arbitrary offset in the common block.
    ///
    /// Diagnostic helper for bring-up and probe scripts. Does not interpret
    /// the value — the caller is responsible for applying the right register
    /// map (S9 vs am2). Offset must be 4-byte aligned and within the 4 KB
    /// common-block window.
    pub fn read_raw(&self, offset: u32) -> u32 {
        debug_assert!(
            offset.is_multiple_of(4),
            "read_raw offset must be 4-byte aligned"
        );
        debug_assert!(
            (offset as usize) < 4096,
            "read_raw offset out of common block"
        );
        self.read_reg(BLOCK_COMMON, offset)
    }

    /// Set the UART baud rate divisor.
    pub fn set_baud(&self, divisor: u32) {
        self.write_reg(BLOCK_COMMON, REG_BAUD, divisor);
    }

    /// Read the BAUD register.
    pub fn read_baud(&self) -> u32 {
        self.read_reg(BLOCK_COMMON, REG_BAUD)
    }

    /// Set the WORK_TIME register.
    ///
    /// **am2 (S19/S19j/S19k)**: this is a no-op. Phase 4A live probe proved
    /// there is no dedicated WORK_TIME MMIO on am2 — common+0x14 reads a
    /// constant `0x00000001` (midstate-count flag, RO) and work-tx+0x14 is
    /// empty. WORK_TIME on am2 is embedded in the work-tx FIFO payload.
    /// Writing to common+0x14 would clobber the flag bit.
    ///
    /// **S9 (am1)**: retained — REG_WORK_TIME is a real register at common+0x14
    /// that controls inter-work delay (reset value = 1, BraiinsOS writes
    /// 0x0004_0507 during operation).
    pub fn set_work_time(&self, value: u32) {
        if self.is_am2 {
            // XXX: am2 WORK_TIME experiment — see am2_write_work_time_mmio_enabled().
            if am2_write_work_time_mmio_enabled() {
                let pre = self.read_reg(BLOCK_COMMON, REG_WORK_TIME);
                self.write_reg(BLOCK_COMMON, REG_WORK_TIME, value);
                let post = self.read_reg(BLOCK_COMMON, REG_WORK_TIME);
                tracing::warn!(
                    chain_id = self.chain_id,
                    pre = format_args!("0x{:08X}", pre),
                    wrote = format_args!("0x{:08X}", value),
                    post = format_args!("0x{:08X}", post),
                    sticks = (post == value),
                    "DCENT_AM2_WRITE_WORK_TIME_MMIO: am2 common+0x14 WORK_TIME experiment (XXX: contradicts Phase 4A RO-flag reading)"
                );
                return;
            }
            tracing::warn!(
                chain_id = self.chain_id,
                value = format_args!("0x{:08X}", value),
                "set_work_time is a no-op on am2 (WORK_TIME is inline in work frame) — ignoring write"
            );
            return;
        }
        self.write_reg(BLOCK_COMMON, REG_WORK_TIME, value);
    }

    /// Read the WORK_TIME register.
    pub fn read_work_time(&self) -> u32 {
        self.read_reg(BLOCK_COMMON, REG_WORK_TIME)
    }

    /// Write work data words to the WORK TX FIFO.
    ///
    /// CRITICAL (2026-03-23): Insert a register READ every 8 words to force the
    /// AXI write buffer to flush and create arbitration gaps. Without this, the
    /// tight 36-word write loop monopolizes the AXI interconnect, causing the
    /// xiic I2C controller's interrupt handler to miss its timing window. BraiinsOS
    /// avoids this by using IRQ-driven dispatch that yields between every word.
    pub fn write_work(&self, words: &[u32]) {
        for (i, &word) in words.iter().enumerate() {
            self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_FIFO, word);
            if i % 4 == 3 && i + 1 < words.len() {
                // AXI write buffer flush every 4 words (was 8). More frequent barriers
                // create arbitration gaps for the AXI IIC controller during I2C heartbeats.
                // Cost: 4 extra AXI reads per 36-word work item (~400ns total, negligible).
                let _ = self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_STAT);
            }
        }
    }

    /// Check if the WORK RX FIFO has data (nonces available).
    pub fn work_rx_has_data(&self) -> bool {
        self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_STAT) & STAT_RX_EMPTY == 0
    }

    /// Read a nonce result from the WORK RX FIFO.
    /// Returns (word0, word1) — the nonce and metadata.
    pub fn read_nonce(&self) -> Option<(u32, u32)> {
        if !self.work_rx_has_data() {
            return None;
        }
        let w0 = self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_FIFO);
        let w1 = self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_FIFO);
        Some((w0, w1))
    }

    /// Check if the WORK TX FIFO is full.
    pub fn work_tx_full(&self) -> bool {
        self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_STAT) & STAT_TX_FULL != 0
    }

    /// Reset WORK RX and WORK TX FIFOs.
    pub fn reset_work_fifos(&self) {
        // Reset Work RX FIFO
        self.write_reg(BLOCK_WORK_RX, REG_WORK_RX_CTRL, 0x01); // RST_RX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.write_reg(BLOCK_WORK_RX, REG_WORK_RX_CTRL, CMD_CTRL_IRQ_EN);

        // Set Work TX IRQ threshold before reset (matches BraiinsOS)
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_THR, 1848);

        // Reset Work TX FIFO
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL, CMD_CTRL_IRQ_EN);
    }

    /// Flush all pending nonces from the WORK_RX_FIFO.
    pub fn flush_work_rx(&self) {
        let mut count = 0;
        while self.work_rx_has_data() {
            let _ = self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_FIFO);
            let _ = self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_FIFO);
            count += 1;
            if count > 1000 {
                break;
            }
        }
        if count > 0 {
            tracing::info!(
                chain_id = self.chain_id,
                flushed = count,
                "Flushed {} stale nonces from WORK_RX_FIFO",
                count
            );
        }
    }

    /// Flush all pending work from the WORK_TX_FIFO.
    pub fn flush_work_tx(&self) {
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL, CMD_CTRL_IRQ_EN);
        tracing::info!(
            chain_id = self.chain_id,
            "Flushed stale work from WORK_TX_FIFO"
        );
    }

    /// Read the CRC error counter.
    pub fn read_error_count(&self) -> u32 {
        self.read_reg(BLOCK_COMMON, REG_ERR_COUNTER)
    }

    // -----------------------------------------------------------------
    // Phase 4B observability helpers
    // -----------------------------------------------------------------

    /// Read the raw ERR_CNT register at common + 0x18.
    ///
    /// Alias for `read_error_count` kept under the name used by Phase 4A
    /// probe scripts and diagnostic tooling. On am2, Phase 4A found this
    /// slot reads a static 0x0000_0000 during live mining — ERR_CNT appears
    /// to live in the Braiins glitch-monitor IP (0x43D00000) on am2
    /// Braiins-am2 bitstream only. On S9 this offset is the real CRC error
    /// counter.
    pub fn read_err_cnt(&self) -> u32 {
        self.read_reg(BLOCK_COMMON, REG_ERR_COUNTER)
    }

    /// Read the work-tx FIFO status register (work-tx + 0x0C).
    pub fn read_work_tx_status(&self) -> u32 {
        self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_STAT)
    }

    /// Read the work-tx FIFO control register (work-tx + 0x08).
    pub fn read_work_tx_ctrl(&self) -> u32 {
        self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL)
    }

    /// Read the work-tx IRQ threshold register (work-tx + 0x10).
    pub fn read_work_tx_threshold(&self) -> u32 {
        self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_THR)
    }

    /// Write the work-tx FIFO control register (work-tx + 0x08).
    pub fn write_work_tx_ctrl(&self, value: u32) {
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_CTRL, value);
    }

    /// Write the work-tx IRQ threshold register (work-tx + 0x10).
    pub fn write_work_tx_threshold(&self, value: u32) {
        self.write_reg(BLOCK_WORK_TX, REG_WORK_TX_THR, value);
    }

    /// Read the work-rx FIFO status register (work-rx + 0x0C).
    pub fn read_work_rx_status(&self) -> u32 {
        self.read_reg(BLOCK_WORK_RX, REG_WORK_RX_STAT)
    }

    /// Read the work-tx last accepted work-id register (work-tx + 0x14).
    pub fn read_work_tx_last(&self) -> u32 {
        self.read_reg(BLOCK_WORK_TX, REG_WORK_TX_LAST)
    }

    /// Format a CTRL register value for logging, using the am2 bit layout.
    ///
    /// Decodes the four named am2 bits (IP_ENABLE at 1, bit 12, MIDSTATE_CNT
    /// at 20, bit 23) from `ctrl_am2`. On S9 this will misreport meaning —
    /// use only on am2 chains.
    ///
    /// Example: `format_ctrl(0x00901002)` ->
    /// `"CTRL=0x00901002 [IP_EN=1, bit12=1, MIDSTATE_CNT=1, bit23=1]"`.
    pub fn format_ctrl(ctrl: u32) -> String {
        let ip_en = (ctrl & ctrl_am2::IP_ENABLE != 0) as u8;
        let bit12 = (ctrl & ctrl_am2::EXT_BAUD_OR_CLKEN_12 != 0) as u8;
        let midstate = (ctrl & ctrl_am2::MIDSTATE_CNT != 0) as u8;
        let bit23 = (ctrl & ctrl_am2::EXT_BAUD_OR_CLKEN_23 != 0) as u8;
        format!(
            "CTRL=0x{:08X} [IP_EN={}, bit12={}, MIDSTATE_CNT={}, bit23={}]",
            ctrl, ip_en, bit12, midstate, bit23
        )
    }
}

impl Drop for DevmemFpgaChain {
    fn drop(&mut self) {
        // Device-backed mappings are reclaimed by the kernel when the daemon
        // exits. Skipping explicit unmap here avoids late-teardown crashes.
    }
}

/// Peek at an S9/am1 FPGA chain CTRL register via /dev/mem (no UIO required).
///
/// Used during Phase 0 (before UIO devices are opened) to detect whether
/// the FPGA IP cores are already configured by previous firmware (hot start)
/// or uninitialized (cold boot / fresh DCENTos).
///
/// **S9 only.** Reads `common + 0x08`. On am2 this offset is unused and
/// always reads zero — use `peek_ctrl_devmem_am2` for S19/S19j/S19k.
///
/// Returns CTRL register value, or 0 on any failure (treated as cold boot).
pub fn peek_ctrl_devmem(chain_phys_base: u64) -> u32 {
    peek_common_word(chain_phys_base, REG_CTRL)
}

/// Peek at an am2 (S19/S19j/S19k) FPGA chain CTRL register via /dev/mem.
///
/// am2 places CTRL at `common + 0x00` (see `am2_regs::REG_CTRL`). During
/// sustained bosminer mining Phase 5A captured `0x00901002` at this offset.
/// Returns 0 on any failure (treated as cold boot / missing bitstream).
pub fn peek_ctrl_devmem_am2(chain_phys_base: u64) -> u32 {
    peek_common_word(chain_phys_base, am2_regs::REG_CTRL)
}

/// Read a single 4-byte word from the FPGA common block via /dev/mem.
/// Shared helper for the `peek_*_devmem` functions.
fn peek_common_word(chain_phys_base: u64, offset: u32) -> u32 {
    use nix::sys::mman::{MapFlags, ProtFlags};
    use std::num::NonZeroUsize;

    let mem_file = match std::fs::OpenOptions::new().read(true).open("/dev/mem") {
        Ok(f) => f,
        Err(_) => return 0,
    };

    let page_size = NonZeroUsize::new(4096).unwrap();
    let ptr = match unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ,
            MapFlags::MAP_SHARED,
            &mem_file,
            chain_phys_base as nix::libc::off_t,
        )
    } {
        Ok(p) => p,
        Err(_) => return 0,
    };

    let base = ptr.as_ptr() as *const u8;
    let word = unsafe { std::ptr::read_volatile(base.add(offset as usize) as *const u32) };
    unsafe {
        let _ = nix::sys::mman::munmap(ptr, 4096);
    }

    word
}

/// One hash chain connected to the FPGA via 4 register blocks.
pub struct FpgaChain {
    /// Common registers (VERSION, CTRL, BAUD, WORK_TIME, ERR_COUNTER).
    pub common: UioDevice,
    /// CMD registers (CMD_RX_FIFO, CMD_TX_FIFO, CMD_STAT).
    pub cmd: UioDevice,
    /// Work RX registers (WORK_RX_FIFO, WORK_RX_STAT) for nonce responses.
    pub work_rx: UioDevice,
    /// Work TX registers (WORK_TX_FIFO, WORK_TX_STAT) for job submission.
    pub work_tx: UioDevice,
    /// Chain ID (6, 7, or 8 on S9, matching connector numbering).
    pub chain_id: u8,
    /// True if this chain uses the am2 common-block layout.
    pub is_am2: bool,
}

impl FpgaChain {
    /// Open an FPGA chain by its chain ID using the given UIO device numbers.
    ///
    /// Each chain uses 4 consecutive UIO devices:
    ///   uio_base+0 = common, +1 = cmd, +2 = work_rx, +3 = work_tx
    ///
    /// On S9, the mapping is (uio0 = fan-control):
    ///   Chain 6: uio1-uio4  (0x43C00000-0x43C03FFF)
    ///   Chain 7: uio5-uio8  (0x43C10000-0x43C13FFF)
    ///   Chain 8: uio9-uio12 (0x43C20000-0x43C23FFF)
    pub fn open(chain_id: u8, uio_base: u8) -> Result<Self> {
        tracing::info!(chain_id, uio_base, "Opening FPGA chain");

        let common = UioDevice::open(uio_base)?;
        let cmd = UioDevice::open(uio_base + 1)?;
        let work_rx = UioDevice::open(uio_base + 2)?;
        let work_tx = UioDevice::open(uio_base + 3)?;

        let chain = Self {
            common,
            cmd,
            work_rx,
            work_tx,
            chain_id,
            is_am2: false,
        };

        // Verify FPGA is responding by reading version register
        let version = chain.read_version();
        tracing::info!(
            chain_id,
            version = format_args!("0x{:08X}", version),
            "FPGA chain opened"
        );

        Ok(chain)
    }

    /// Open an am2 chain via UIO.
    pub fn open_am2(chain_id: u8, uio_base: u8) -> Result<Self> {
        let mut chain = Self::open(chain_id, uio_base)?;
        chain.is_am2 = true;
        let ctrl = chain.read_ctrl();
        let build = chain.read_build_id();
        tracing::info!(
            chain_id,
            uio_base,
            ctrl = format_args!("0x{:08X}", ctrl),
            build = format_args!("0x{:08X}", build),
            "Opened am2 FPGA chain via UIO"
        );
        Ok(chain)
    }

    /// Read the FPGA IP core version register.
    /// Expected value for s9io v1.0.2: 0x00901002
    pub fn read_version(&self) -> u32 {
        if self.is_am2 {
            self.common.read_reg(am2_regs::REG_BUILD)
        } else {
            self.common.read_reg(REG_VERSION)
        }
    }

    /// Read the build ID (bitstream timestamp).
    pub fn read_build_id(&self) -> u32 {
        self.common.read_reg(REG_BUILD_ID)
    }

    /// Read the BAUD register.
    pub fn read_baud(&self) -> u32 {
        self.common.read_reg(REG_BAUD)
    }

    /// Read the common-block CTRL register, routing am1/am2 correctly.
    pub fn read_ctrl(&self) -> u32 {
        if self.is_am2 {
            self.common.read_reg(am2_regs::REG_CTRL)
        } else {
            self.common.read_reg(REG_CTRL)
        }
    }

    /// Write the common-block CTRL register, routing am1/am2 correctly.
    pub fn write_ctrl(&self, value: u32) {
        if self.is_am2 && !am2_ctrl_value_is_safe(value) {
            tracing::error!(
                chain_id = self.chain_id,
                value = format_args!("0x{:08X}", value),
                "refusing am2 CTRL write without IP_ENABLE — zeroing/disabling CTRL after UART traffic permanently bricks the FPGA UART state machine (gap-swarm HAL-safety #5; see set_enabled). reconfigure() routes here too."
            );
            return;
        }
        if self.is_am2 {
            self.common.write_reg(am2_regs::REG_CTRL, value);
        } else {
            self.common.write_reg(REG_CTRL, value);
        }
    }

    /// Read a raw common-block word.
    pub fn read_raw(&self, offset: u32) -> u32 {
        self.common.read_reg(offset)
    }

    /// Enable or disable this chain.
    ///
    /// When enabled, sets MIDSTATE_CNT=0 (1 midstate per work item).
    /// The chip driver's `ctrl_reg_value()` determines the actual MIDSTATE_CNT
    /// during `reconfigure()` — this method is only used for disable.
    ///
    /// **WARNING**: Calling `set_enabled(false)` (writing 0 to CTRL_REG) after
    /// UART traffic has flowed puts the FPGA UART state machine into a broken
    /// state that does NOT recover when the chain is subsequently re-enabled.
    /// This was proven by A/B testing on live S9 hardware (2026-03-12).
    ///
    /// In practice, Phase 4c hot-start detection sends GetAddress UART traffic
    /// to ALL chains (including ones that turn out to be cold). This means UART
    /// traffic has ALWAYS flowed by the time cold boot runs, making
    /// `set_enabled(false)` unsafe in all real scenarios. Use `reset_fifos()`
    /// to clear stale data, or `reconfigure()` for a full FIFO+baud reset.
    pub fn set_enabled(&self, enabled: bool) {
        if self.is_am2 {
            tracing::warn!(
                chain_id = self.chain_id,
                enabled,
                "set_enabled ignored on am2: S9 CTRL bit layout is not valid for S19j"
            );
            return;
        }
        let ctrl = if enabled {
            CTRL_ENABLE | (2 << CTRL_MIDSTATE_SHIFT) // 0x0C: ENABLE + MIDSTATE_CNT=2
        } else {
            0
        };
        self.write_ctrl(ctrl);
    }

    /// Reset the FPGA IP core by toggling only the ENABLE bit.
    ///
    /// This matches BraiinsOS's `disable_ip_core(); enable_ip_core();` sequence
    /// which uses read-modify-write to preserve MIDSTATE_CNT and other bits.
    /// When ENABLE is cleared, the FPGA UART TX line goes idle/low, sending a
    /// BREAK condition to the BM1387 ASICs which resets all their internal
    /// registers to power-on defaults (115200 baud, gate_block=0, default PLL).
    ///
    /// **IMPORTANT**: Uses `modify` style (read-modify-write), NOT `write(0)`.
    /// Writing 0 clears MIDSTATE_CNT which permanently breaks the FPGA UART
    /// state machine (proven 2026-03-12). Preserving MIDSTATE_CNT during the
    /// disable/enable cycle avoids this bug.
    pub fn reset_ip_core(&self) {
        if self.is_am2 {
            tracing::warn!(
                chain_id = self.chain_id,
                "reset_ip_core ignored on am2: S9 ENABLE-bit BREAK semantics are not valid for S19j"
            );
            return;
        }
        let ctrl = self.read_ctrl();
        // Clear ONLY the ENABLE bit, keep MIDSTATE_CNT and other config bits
        self.write_ctrl(ctrl & !CTRL_ENABLE);
        std::thread::sleep(std::time::Duration::from_millis(1));
        // Re-enable with all original bits restored
        self.write_ctrl(ctrl | CTRL_ENABLE);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    /// Safely reconfigure the chain without disabling it.
    ///
    /// This resets all FIFOs, sets the baud rate, configures WORK_TIME,
    /// and clears the error counter -- all while keeping the chain ENABLED.
    /// The CTRL_REG is written with the provided value (which must include
    /// CTRL_ENABLE) at the end to ensure consistent state.
    ///
    /// This method exists because `set_enabled(false)` permanently breaks
    /// the FPGA UART state machine (proven on live hardware 2026-03-12).
    /// Bosminer never disables the chain during normal operation -- it only
    /// reconfigures FIFOs and baud while the chain stays enabled.
    pub fn reconfigure(&self, ctrl_value: u32, baud_divisor: u32) {
        if self.is_am2 {
            self.reset_fifos();
            self.set_baud(baud_divisor);
            self.write_ctrl(ctrl_value);
            return;
        }

        // Reset FIFOs first (safe while chain is enabled)
        self.reset_fifos();

        // Set baud rate
        self.set_baud(baud_divisor);

        // Set WORK_TIME
        self.set_work_time(WORK_TIME_DEFAULT);

        // Clear error counter
        self.clear_error_count();

        // Ensure chain is enabled with the correct mode bits
        // (This is a no-op if CTRL_REG already has this value, but guarantees
        // correct state after FIFO reset)
        self.write_ctrl(ctrl_value);
    }

    /// Set the UART baud rate via the BAUD_REG divisor.
    ///
    /// Baud rate = FPGA_CLK_HZ / (16 * (divisor + 1))
    pub fn set_baud(&self, divisor: u32) {
        self.common.write_reg(REG_BAUD, divisor);
    }

    /// Read the WORK_TIME/common+0x14 word.
    pub fn read_work_time(&self) -> u32 {
        self.common.read_reg(REG_WORK_TIME)
    }

    /// Calculate baud rate from a divisor value.
    pub fn baud_from_divisor(divisor: u32) -> u32 {
        FPGA_CLK_HZ / (16 * (divisor + 1))
    }

    /// Calculate divisor from a target baud rate.
    pub fn divisor_from_baud(baud: u32) -> u32 {
        (FPGA_CLK_HZ / (16 * baud)) - 1
    }

    /// Read the CRC error counter.
    pub fn read_error_count(&self) -> u32 {
        self.common.read_reg(REG_ERR_COUNTER)
    }

    /// Set the WORK_TIME register.
    pub fn set_work_time(&self, value: u32) {
        if self.is_am2 {
            // XXX: am2 WORK_TIME experiment — see am2_write_work_time_mmio_enabled().
            if am2_write_work_time_mmio_enabled() {
                let pre = self.common.read_reg(REG_WORK_TIME);
                self.common.write_reg(REG_WORK_TIME, value);
                let post = self.common.read_reg(REG_WORK_TIME);
                tracing::warn!(
                    chain_id = self.chain_id,
                    pre = format_args!("0x{:08X}", pre),
                    wrote = format_args!("0x{:08X}", value),
                    post = format_args!("0x{:08X}", post),
                    sticks = (post == value),
                    "DCENT_AM2_WRITE_WORK_TIME_MMIO: am2 common+0x14 WORK_TIME experiment (XXX: contradicts Phase 4A RO-flag reading)"
                );
                return;
            }
            tracing::warn!(
                chain_id = self.chain_id,
                value = format_args!("0x{:08X}", value),
                "set_work_time is a no-op on am2 (WORK_TIME is inline in work frame) — ignoring write"
            );
        } else {
            self.common.write_reg(REG_WORK_TIME, value);
        }
    }

    /// Clear the CRC error counter.
    pub fn clear_error_count(&self) {
        if self.is_am2 {
            tracing::warn!(
                chain_id = self.chain_id,
                "clear_error_count ignored on am2: common+0x18 is not the S9 CRC-clear CTRL path"
            );
            return;
        }
        let ctrl = self.read_ctrl();
        self.write_ctrl(ctrl | CTRL_ERR_CLR);
    }

    /// Reset all FIFOs (CMD TX/RX, WORK TX/RX).
    ///
    /// Matches the verified sequence from asic_comm_test.c:
    ///   CMD_CTRL = 0x03 (RST_TX | RST_RX) → delay → CMD_CTRL = 0x04 (IRQ_EN)
    pub fn reset_fifos(&self) {
        // Reset CMD FIFOs
        self.cmd.write_reg(REG_CMD_CTRL, 0x03); // RST_TX | RST_RX
                                                // Brief delay for FIFO reset to complete (1ms in C test tool)
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.cmd.write_reg(REG_CMD_CTRL, CMD_CTRL_IRQ_EN); // Clear reset, enable IRQ

        // Reset Work RX FIFO
        self.work_rx.write_reg(REG_WORK_RX_CTRL, 0x01); // RST_RX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.work_rx.write_reg(REG_WORK_RX_CTRL, CMD_CTRL_IRQ_EN); // Clear reset, enable IRQ

        // Set Work TX IRQ threshold BEFORE reset (matches BraiinsOS init order).
        // BraiinsOS: work_tx_irq_thr = FIFO_SIZE(2048) - BIGGEST_WORK(200) = 1848.
        // Without this, the FPGA work scheduler may not properly dispatch work to
        // the UART serializer. THR=0 (hardware default) is undefined behavior.
        self.work_tx.write_reg(REG_WORK_TX_THR, 1848);

        // Reset Work TX FIFO
        self.work_tx.write_reg(REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.work_tx.write_reg(REG_WORK_TX_CTRL, CMD_CTRL_IRQ_EN); // Clear reset, enable IRQ
    }

    /// Write a 32-bit word to the CMD TX FIFO.
    pub fn write_cmd(&self, word: u32) {
        self.cmd.write_reg(REG_CMD_TX_FIFO, word);
    }

    /// Read a 32-bit word from the CMD RX FIFO.
    /// Returns None if the FIFO is empty.
    pub fn read_cmd_response(&self) -> Option<u32> {
        let stat = self.cmd.read_reg(REG_CMD_STAT);
        if stat & STAT_RX_EMPTY != 0 {
            return None;
        }
        Some(self.cmd.read_reg(REG_CMD_RX_FIFO))
    }

    /// Check if the CMD RX FIFO has data.
    pub fn cmd_rx_has_data(&self) -> bool {
        self.cmd.read_reg(REG_CMD_STAT) & STAT_RX_EMPTY == 0
    }

    /// Write work data words to the WORK TX FIFO.
    ///
    /// Insert a register READ every 4 words to create AXI arbitration gaps.
    /// Without this, 36 consecutive AXI writes monopolize the GP0 interconnect,
    /// corrupting concurrent AXI IIC transactions (PIC MSSP death spiral).
    /// Matches the devmem path (DevmemFpgaChain::write_work).
    pub fn write_work(&self, words: &[u32]) {
        for (i, &word) in words.iter().enumerate() {
            self.work_tx.write_reg(REG_WORK_TX_FIFO, word);
            if i % 4 == 3 && i + 1 < words.len() {
                let _ = self.work_tx.read_reg(REG_WORK_TX_STAT);
            }
        }
    }

    /// Read a nonce result from the WORK RX FIFO.
    /// Returns None if the FIFO is empty.
    /// Returns (word0, word1) -- the nonce and metadata.
    pub fn read_nonce(&self) -> Option<(u32, u32)> {
        let stat = self.work_rx.read_reg(REG_WORK_RX_STAT);
        if stat & STAT_RX_EMPTY != 0 {
            return None;
        }
        let word0 = self.work_rx.read_reg(REG_WORK_RX_FIFO);
        let word1 = self.work_rx.read_reg(REG_WORK_RX_FIFO);
        Some((word0, word1))
    }

    /// Check if the WORK TX FIFO is full.
    pub fn work_tx_full(&self) -> bool {
        self.work_tx.read_reg(REG_WORK_TX_STAT) & STAT_TX_FULL != 0
    }

    /// Check if the WORK RX FIFO has data (nonces available).
    pub fn work_rx_has_data(&self) -> bool {
        self.work_rx.read_reg(REG_WORK_RX_STAT) & STAT_RX_EMPTY == 0
    }

    /// Read the WORK RX status register.
    pub fn read_work_rx_status(&self) -> u32 {
        self.work_rx.read_reg(REG_WORK_RX_STAT)
    }

    /// Read the WORK TX status register.
    pub fn read_work_tx_status(&self) -> u32 {
        self.work_tx.read_reg(REG_WORK_TX_STAT)
    }

    /// Read the WORK TX control register.
    pub fn read_work_tx_ctrl(&self) -> u32 {
        self.work_tx.read_reg(REG_WORK_TX_CTRL)
    }

    /// Read the WORK TX threshold register.
    pub fn read_work_tx_threshold(&self) -> u32 {
        self.work_tx.read_reg(REG_WORK_TX_THR)
    }

    /// Write the WORK TX control register.
    pub fn write_work_tx_ctrl(&self, value: u32) {
        self.work_tx.write_reg(REG_WORK_TX_CTRL, value);
    }

    /// Write the WORK TX threshold register.
    pub fn write_work_tx_threshold(&self, value: u32) {
        self.work_tx.write_reg(REG_WORK_TX_THR, value);
    }

    /// Read the WORK TX last register.
    pub fn read_work_tx_last(&self) -> u32 {
        self.work_tx.read_reg(REG_WORK_TX_LAST)
    }

    /// Flush all pending nonces from the WORK_RX_FIFO.
    /// Called on clean_jobs to discard stale nonces from the previous block.
    pub fn flush_work_rx(&self) {
        let mut count = 0;
        while self.work_rx_has_data() {
            let _ = self.work_rx.read_reg(REG_WORK_RX_FIFO);
            let _ = self.work_rx.read_reg(REG_WORK_RX_FIFO);
            count += 1;
            if count > 1000 {
                break;
            } // safety limit
        }
        if count > 0 {
            tracing::info!(
                chain_id = self.chain_id,
                flushed = count,
                "Flushed {} stale nonces from WORK_RX_FIFO",
                count
            );
        }
    }

    /// Flush all pending work from the WORK_TX_FIFO.
    pub fn flush_work_tx(&self) {
        self.work_tx.write_reg(REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(1));
        self.work_tx.write_reg(REG_WORK_TX_CTRL, CMD_CTRL_IRQ_EN);
        tracing::info!(
            chain_id = self.chain_id,
            "Flushed stale work from WORK_TX_FIFO"
        );
    }
}

/// Flush WORK_TX FIFOs on all chains via devmem.
///
/// Safe to call from any thread — opens/closes /dev/mem per call.
/// Used by the mining heartbeat thread to quiet FPGA AXI traffic before I2C.
///
/// Writes RST_TX (0x02) to WORK_TX_CTRL at chain_base + 0x3008, waits 100us,
/// then writes IRQ_EN (0x04) to clear the reset and re-enable.
///
/// S9 chain bases: 0x43C00000 (ch6), 0x43C10000 (ch7), 0x43C20000 (ch8).
/// S19 (am2) uses the SAME FPGA IP at the same base addresses.
///
/// `chain_bases` parameter allows platform-specific addresses. Pass `None`
/// for S9 defaults (backward compatible).
/// Settle behaviour between the RST_TX (0x02) and IRQ_EN (0x04) writes in
/// `flush_all_work_tx_devmem_with_bases`.
///
/// CE-011: an audit flagged the 100µs `std::thread::sleep` between asserting
/// RST_TX and clearing it as "just a register-ordering barrier, not a real
/// delay". That is only partly true: the two writes are already
/// `write_volatile`, which the compiler cannot reorder, so a fence adds nothing
/// for *ordering*. But a FIFO reset is a real hardware event — the s9io block
/// needs the RST_TX pulse to be observed before IRQ_EN clears it. On live
/// hardware the short settle is what guarantees the flush actually happens.
///
/// Because shortening this could weaken a live-hardware behaviour, the real
/// settle stays the compiled DEFAULT. `DCENT_FPGA_TX_FLUSH_FENCE_ONLY=1`
/// (default OFF, lab/perf only) swaps it for a memory fence — for operators who
/// want to A/B whether the pulse-width can be removed without losing flushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxFlushSettle {
    /// Real ~100µs hardware settle (compiled default; live-proven on S9).
    SleepUs(u64),
    /// Memory-fence-only (no delay). Default-OFF perf opt-in.
    FenceOnly,
}

/// Pure decision helper (host-testable, no env read) — maps the raw env value
/// to a settle mode. `None` (gate unset) → the real settle default.
fn tx_flush_settle_mode_for(env_value: Option<&str>) -> TxFlushSettle {
    match env_value {
        Some("1" | "true" | "TRUE" | "yes" | "on") => TxFlushSettle::FenceOnly,
        _ => TxFlushSettle::SleepUs(100),
    }
}

/// Reads the env gate and returns the configured settle mode.
fn tx_flush_settle_mode() -> TxFlushSettle {
    tx_flush_settle_mode_for(
        std::env::var("DCENT_FPGA_TX_FLUSH_FENCE_ONLY")
            .ok()
            .as_deref(),
    )
}

/// Apply the configured settle between the RST_TX and IRQ_EN writes.
#[inline]
fn apply_tx_flush_settle(mode: TxFlushSettle) {
    match mode {
        TxFlushSettle::SleepUs(us) => {
            std::thread::sleep(std::time::Duration::from_micros(us));
        }
        TxFlushSettle::FenceOnly => {
            // A full SeqCst fence orders the two volatile register writes
            // around it without any wall-clock delay. (The writes are already
            // `write_volatile` so they are not reordered relative to each
            // other; this is the strict-ordering equivalent the audit asked
            // for, minus the pulse-width.)
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        }
    }
}

pub fn flush_all_work_tx_devmem_with_bases(chain_bases: &[u64]) {
    let settle = tx_flush_settle_mode();
    // SAFETY: We open /dev/mem, mmap the FPGA register space, write two
    // volatile u32 values (RST_TX then IRQ_EN), and immediately munmap.
    // The addresses are verified FPGA register offsets for the BraiinsOS
    // s9io bitstream (proven on live S9 hardware, 2026-04-06).
    unsafe {
        use std::os::fd::AsRawFd;
        let mem_fd = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem");
        if let Ok(ref mem) = mem_fd {
            for &chain_base in chain_bases {
                let mapped = libc::mmap(
                    std::ptr::null_mut(),
                    0x4000, // 16KB covers all sub-blocks (common, cmd, work_rx, work_tx)
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    mem.as_raw_fd(),
                    chain_base as libc::off_t,
                );
                if mapped != libc::MAP_FAILED {
                    let base = mapped as *mut u8;
                    let wtx_ctrl = base.add(0x3008) as *mut u32;
                    // RST_TX = 0x02 (flush FIFO)
                    std::ptr::write_volatile(wtx_ctrl, 0x02);
                    // Settle so the s9io block observes the RST_TX pulse before
                    // IRQ_EN clears it (CE-011: real settle by default, fence
                    // via DCENT_FPGA_TX_FLUSH_FENCE_ONLY=1).
                    apply_tx_flush_settle(settle);
                    // IRQ_EN = 0x04 (clear reset, re-enable)
                    std::ptr::write_volatile(wtx_ctrl, 0x04);
                    libc::munmap(mapped, 0x4000);
                }
            }
        }
    }
}

/// Backward-compatible S9 wrapper — flushes all 3 S9 chains (ch6/ch7/ch8).
pub fn flush_all_work_tx_devmem() {
    const S9_CHAIN_BASES: [u64; 3] = [0x43C00000, 0x43C10000, 0x43C20000];
    flush_all_work_tx_devmem_with_bases(&S9_CHAIN_BASES);
}

// ---------------------------------------------------------------------------
// Unit tests (Phase 4B)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_am2_default_matches_live_probe() {
        // Live-captured CTRL value from S19j Pro .139 bosminer on 2026-04-20.
        //
        assert_eq!(ctrl_am2::BM1362_DEFAULT, 0x0090_1002);
    }

    #[test]
    fn tx_flush_settle_default_is_real_sleep() {
        // CE-011: with the gate unset (None), the compiled default MUST be the
        // real ~100µs hardware settle — never the fence-only fast path. Pins
        // the live-hardware-default contract (a fence does not flush the s9io
        // FIFO; the pulse-width does). Pure mapping — no process env touched.
        assert_eq!(tx_flush_settle_mode_for(None), TxFlushSettle::SleepUs(100));
        // Any non-truthy value also keeps the safe default.
        assert_eq!(
            tx_flush_settle_mode_for(Some("0")),
            TxFlushSettle::SleepUs(100)
        );
        assert_eq!(
            tx_flush_settle_mode_for(Some("false")),
            TxFlushSettle::SleepUs(100)
        );
    }

    #[test]
    fn tx_flush_settle_fence_only_when_opted_in() {
        // The default-OFF perf opt-in selects the fence-only path.
        for truthy in ["1", "true", "TRUE", "yes", "on"] {
            assert_eq!(
                tx_flush_settle_mode_for(Some(truthy)),
                TxFlushSettle::FenceOnly,
                "{truthy} must select fence-only"
            );
        }
    }

    #[test]
    fn apply_tx_flush_settle_fence_is_noop_fast() {
        // FenceOnly must not panic and must not sleep. Exercising the branch
        // guards against a future edit that accidentally routes FenceOnly
        // through a sleep.
        apply_tx_flush_settle(TxFlushSettle::FenceOnly);
    }

    #[test]
    fn ctrl_am2_bits_are_disjoint_and_exactly_four() {
        let bits = ctrl_am2::IP_ENABLE
            | ctrl_am2::EXT_BAUD_OR_CLKEN_12
            | ctrl_am2::MIDSTATE_CNT
            | ctrl_am2::EXT_BAUD_OR_CLKEN_23;
        assert_eq!(bits, ctrl_am2::BM1362_DEFAULT);
        assert_eq!(ctrl_am2::BM1362_DEFAULT.count_ones(), 4);
    }

    #[test]
    fn format_ctrl_decodes_live_value() {
        let s = DevmemFpgaChain::format_ctrl(0x0090_1002);
        assert!(s.contains("IP_EN=1"), "missing IP_EN=1 in {}", s);
        assert!(
            s.contains("MIDSTATE_CNT=1"),
            "missing MIDSTATE_CNT=1 in {}",
            s
        );
        assert!(s.contains("bit12=1"), "missing bit12=1 in {}", s);
        assert!(s.contains("bit23=1"), "missing bit23=1 in {}", s);
        assert!(s.contains("0x00901002"), "missing hex value in {}", s);
    }

    #[test]
    fn format_ctrl_decodes_zero() {
        let s = DevmemFpgaChain::format_ctrl(0);
        assert!(s.contains("IP_EN=0"));
        assert!(s.contains("MIDSTATE_CNT=0"));
        assert!(s.contains("bit12=0"));
        assert!(s.contains("bit23=0"));
    }

    #[test]
    fn am2_ctrl_offset_matches_phase5a_probe() {
        // Phase 5A live-probe authoritative ground truth (2026-04-20):
        // am2 CTRL is at common+0x00, NOT common+0x08 (S9 layout).
        assert_eq!(am2_regs::REG_CTRL, 0x00);
        assert_eq!(am2_regs::REG_BUILD, 0x04);
        assert_eq!(am2_regs::REG_MIDSTATE_CNT_FLAG, 0x14);
    }

    #[test]
    fn am2_ctrl_offset_differs_from_s9() {
        // Core fix: do NOT alias S9 REG_CTRL onto am2.
        assert_ne!(REG_CTRL, am2_regs::REG_CTRL);
        // S9 VERSION at +0x00 is aliased over by am2 CTRL — they share the
        // slot but with totally different semantics.
        assert_eq!(REG_VERSION, am2_regs::REG_CTRL);
    }

    /// FPGA-3 regression: `DevmemFpgaChain` register access used to have NO
    /// bounds guard — an out-of-range `block` or `offset` would read/write past
    /// the mapped 4 KB page and fault the AXI bus (daemon crash). The access
    /// predicate must accept the real `(BLOCK_*, const-offset)` calls and reject
    /// anything out of range. `DevmemFpgaChain` needs `/dev/mem`, so we test the
    /// pure extracted predicate the guards route through.
    #[test]
    fn fpga3_devmem_access_in_bounds_accepts_real_register_accesses() {
        // Every (block, offset) pair the HAL actually issues must stay valid.
        for &block in &[BLOCK_COMMON, BLOCK_CMD, BLOCK_WORK_RX, BLOCK_WORK_TX] {
            for off in [0x00u32, 0x04, 0x08, 0x0C, 0x10, 0x14, 0x18] {
                assert!(
                    devmem_access_in_bounds(block, off),
                    "block {block} offset 0x{off:04X} must be a valid devmem access"
                );
            }
        }
        // Last fully-in-bounds aligned word of the 4 KB page.
        assert!(devmem_access_in_bounds(BLOCK_WORK_TX, 0xFFC));
    }

    #[test]
    fn fpga3_devmem_access_in_bounds_rejects_out_of_range() {
        // Block index past the 4 mapped pages.
        assert!(!devmem_access_in_bounds(DEVMEM_BLOCK_COUNT, 0x00));
        assert!(!devmem_access_in_bounds(usize::MAX, 0x00));
        // Offset at/past the end of a page.
        assert!(!devmem_access_in_bounds(
            BLOCK_COMMON,
            DEVMEM_BLOCK_MAP_SIZE as u32
        )); // 0x1000
        assert!(!devmem_access_in_bounds(BLOCK_COMMON, 0x2000));
        // A word that starts in-bounds but whose last byte overruns the page.
        assert!(!devmem_access_in_bounds(BLOCK_COMMON, 0xFFE));
        // Misaligned offsets (non-4-byte-aligned volatile u32 access is UB).
        assert!(!devmem_access_in_bounds(BLOCK_COMMON, 0x01));
        assert!(!devmem_access_in_bounds(BLOCK_COMMON, 0x0A));
        // Extreme value that would wrap a naive `offset + 4`.
        assert!(!devmem_access_in_bounds(BLOCK_COMMON, u32::MAX));
    }

    #[test]
    fn work_time_placement_distinguishes_s9_mmio_from_am2_inline_frame() {
        assert_eq!(
            work_time_placement(false),
            WorkTimePlacement::CommonMmio(REG_WORK_TIME)
        );
        assert_eq!(
            work_time_placement(true),
            WorkTimePlacement::InlineWorkFrame
        );
        assert_ne!(REG_WORK_TIME, am2_regs::REG_CTRL);
        assert_eq!(REG_WORK_TIME, am2_regs::REG_MIDSTATE_CNT_FLAG);
    }
}
