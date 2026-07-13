//! Shared BM139x helper code.
//!
//! This module intentionally stays small. BM1397, BM1398, and future BM1396
//! work share the unified BM13xx+ command framing and the FPGA work-time math,
//! but their actual init flows still diverge enough that we do not want a large
//! inheritance-style abstraction yet.

use crate::protocol::{pack_lsb_first, unpack_lsb_first};
use crate::Result;
use dcentrald_hal::fpga_chain::FpgaChain;

const HDR_WRITE_ALL: u8 = 0x51;
const HDR_READ_ALL: u8 = 0x52;
const HDR_INACTIVE_ALL: u8 = 0x53;
const HDR_SET_ADDR: u8 = 0x40;
const HDR_WRITE_SINGLE: u8 = 0x41;
const HDR_READ_SINGLE: u8 = 0x42;

pub const CHAIN_INACTIVE_CMD: u32 = HDR_INACTIVE_ALL as u32 | (0x05u32 << 8);

pub fn fifo_write_reg_bcast(reg: u8, value: u32) -> (u32, u32) {
    let word0 = pack_lsb_first(&[HDR_WRITE_ALL, 0x09, 0x00, reg]);
    let word1 = pack_lsb_first(&value.to_be_bytes());
    (word0, word1)
}

pub fn fifo_write_reg_single(chip_addr: u8, reg: u8, value: u32) -> (u32, u32) {
    let word0 = pack_lsb_first(&[HDR_WRITE_SINGLE, 0x09, chip_addr, reg]);
    let word1 = pack_lsb_first(&value.to_be_bytes());
    (word0, word1)
}

pub fn fifo_read_reg_bcast(reg: u8) -> u32 {
    pack_lsb_first(&[HDR_READ_ALL, 0x05, 0x00, reg])
}

pub fn fifo_set_address(addr: u8) -> u32 {
    pack_lsb_first(&[HDR_SET_ADDR, 0x05, addr, 0x00])
}

pub fn fifo_read_reg_single(chip_addr: u8, reg: u8) -> u32 {
    pack_lsb_first(&[HDR_READ_SINGLE, 0x05, chip_addr, reg])
}

pub fn calculate_work_time(freq_mhz: u16, midstate_count: u32) -> u32 {
    const FPGA_WORK_CLK: f64 = 100_000_000.0;
    let freq_hz = freq_mhz as f64 * 1_000_000.0;
    let nonce_range = midstate_count as f64 * 524_288.0;
    let work_time = (0.9 * nonce_range / freq_hz * FPGA_WORK_CLK) as u32;
    work_time.max(1)
}

pub fn read_pll_register(
    chain: &mut FpgaChain,
    chip_addr: u8,
    pll_reg_addr: u8,
) -> Result<Option<u32>> {
    while chain.cmd_rx_has_data() {
        let _ = chain.read_cmd_response();
    }

    chain.write_cmd(fifo_read_reg_single(chip_addr, pll_reg_addr));
    std::thread::sleep(std::time::Duration::from_millis(20));

    if !chain.cmd_rx_has_data() {
        return Ok(None);
    }

    let Some(r0) = chain.read_cmd_response() else {
        return Ok(None);
    };
    let _ = chain.read_cmd_response();
    let bytes = unpack_lsb_first(r0);
    Ok(Some(u32::from_be_bytes(bytes)))
}

#[cfg(all(test, feature = "sim-hal"))]
mod tests {
    use super::*;
    use dcentrald_hal::platform::sim::SimModel;

    #[test]
    fn concrete_fpga_accessor_reads_virtual_broadcast_register_state() {
        let mut chain = FpgaChain::open_sim_for_model(0, SimModel::S19Pro).unwrap();
        let expected = 0x4068_0221;
        let (w0, w1) = fifo_write_reg_bcast(0x08, expected);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        assert_eq!(
            read_pll_register(&mut chain, 0, 0x08).unwrap(),
            Some(expected)
        );
    }
}
