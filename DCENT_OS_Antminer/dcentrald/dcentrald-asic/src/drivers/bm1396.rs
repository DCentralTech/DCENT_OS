//! BM1396 driver scaffold for the S17+ / T17+ sub-family (chip ID
//! `0x1396`, 7 nm Gen-2, BM1397-era command-header family).
//!
//! NOTE: BM1396 hosts the **S17+ / T17+** products. The S17e / T17e
//! products carry **BM1397** (chip ID `0x1397`), NOT BM1396 — an
//! earlier "T17e/S17e-era" label on this scaffold was a stray
//! BM1396↔S17e/T17e mis-attribution. See
//!
//! (PR-056 / R11-14) for the corpus resolution; it is regression-pinned
//! by the `pr056_bm1396_vs_bm1397_disambiguation` test in `mod.rs`.
//!
//! Important constraints:
//! - The workspace does not yet contain a verified live BM1396 chip ID
//!   (no S17+/T17+ unit on the fleet — `UNKNOWN — needs hardware`).
//! - Board-control evidence is split: S17+ appears dsPIC-based, while
//!   T17+ appears PIC16-based.
//! - Until that is validated on real hardware, this module is intentionally not
//!   registered in `ChipRegistry` and does not implement `ChipDriver`. A
//!   chip enumerating `0x1396` therefore falls through
//!   `ChipRegistry::detect()` to `None` and is **never** silently mapped
//!   onto the registered BM1397 (`0x1397`) driver.

use crate::Result;
use dcentrald_hal::fpga_chain::FpgaChain;

use super::bm139x;

pub struct Bm1396Driver;

impl Default for Bm1396Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1396Driver {
    pub fn new() -> Self {
        Self
    }

    /// BM1396 is expected to share the BM139x FPGA work-time math.
    pub fn calculate_work_time(freq_mhz: u16, midstate_count: u32) -> u32 {
        bm139x::calculate_work_time(freq_mhz, midstate_count)
    }

    /// Shared helper for future BM1396 PLL readback once the register map is
    /// confirmed on live or extracted hardware.
    pub fn read_pll_register(
        chain: &mut FpgaChain,
        chip_addr: u8,
        pll_reg_addr: u8,
    ) -> Result<Option<u32>> {
        bm139x::read_pll_register(chain, chip_addr, pll_reg_addr)
    }
}
