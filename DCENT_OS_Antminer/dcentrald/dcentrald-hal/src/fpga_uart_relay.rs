//! AM2 XIL `a lab unit` FPGA UART **return-relay** enable (`gpio@41220000`).
//!
//! ## What this is (LIVE-PINNED 2026-06-11)
//!
//! On the am2 Zynq S19j Pro, the chain command/response UARTs are PL
//! soft-16550 cores (`/dev/ttyS1` @ `0x41001000`, `/dev/ttyS3` @ `0x41021000`).
//! The BM1362 daisy-chain **RETURN** line (chip RO → chip-id/enum/nonce frames)
//! is NOT wired straight to the soft-UART RX pin — it is routed **through the
//! FPGA fabric**, gated by a 2-bit AXI-GPIO IP at physical `0x41220000`
//! (`gpio@41220000` in the device tree):
//!
//! - **bit 0 = `co_relay_en`** (Command-Output relay enable)
//! - **bit 1 = `ro_relay_en`** (Return-Output relay enable — the one that lets
//!   chip replies reach the host UART RX)
//!
//! This GPIO is the **only `/dev/mem` mapping bosminer makes** (RE-018 cold
//! strace: `mmap2(NULL, 65536, …, /dev/mem, 0x41220000)`). bosminer drives the
//! two bits HIGH (`DATA = 0x3`) as **outputs** (`TRI = 0x0`) during chain
//! bring-up — an mmap store that is invisible to strace, which is why six
//! months of RE never saw it.
//!
//! Important ambiguity: [`crate::psu_gpio_i2c`] also live-pins `0x41220000` as
//! the `gpio895/896` PSU SMBus AXI-GPIO bank. Treat this helper as a way to
//! match bosminer's observed low-bit GPIO state, not as proof that this bank is
//! solely or sufficiently a chain-return relay control.
//!
//! ## Why this was a plausible `a lab unit` standalone enum=0 root-cause
//!
//! The device-tree reset default for this GPIO is `tri-default = 0xffffffff`
//! (both bits **input/floating** ⇒ relay DISABLED). DCENT's `a lab unit` standalone
//! path drives the chain over the plain kernel `ttyS1`/`ttyS3` and **never
//! mapped this GPIO**, so on a cold boot the RETURN relay stayed open:
//! commands went out (UART TX drained, `lsr=0x60`) and **zero** bytes came
//! back (`rx_bytes_pre_init=0`, `unique_chip_replies=0`). The 2026-06-11 live
//! v+1 run proved every other layer GREEN (dsPIC `real_ack`, rail ~13 V, OUT2
//! asserted) with enum still 0, which made the open RETURN relay the top
//! candidate before v+2 falsified it as sufficient.
//!
//! ## Live falsification boundary (2026-06-11)
//!
//! The v+2 live run enabled this GPIO and read it back as byte-identical to the
//! bosminer-engaged state (`DATA=0x3`, `TRI=0x0`, `relay_confirmed=true`), but
//! enum was still 0 (`rx_bytes_pre_init=0`, `unique_chip_replies=0`) and the
//! board stayed cold / LM75 unavailable. So this helper is necessary to match
//! bosminer state, but not sufficient and must not be treated as the complete
//! root cause. The remaining discriminator is a bosminer-engaged vs
//! DCENT-standalone register diff, or chip-physical rail/clock proof.
//!
//! ## Live ground truth (2026-06-11, read-only probe on a bosminer-engaged `a lab unit`)
//!
//! ```text
//! devmem 0x41220000 32  →  0x00000003   (DATA: bit0 co_relay_en + bit1 ro_relay_en, BOTH SET)
//! devmem 0x41220004 32  →  0x00000000   (TRI : bits[1:0] = OUTPUT, driving)
//! ```
//!
//! Reconciliation with prior RE: the `miner-glitch-monitor` at `0x43d00000`
//! (see [`crate::glitch_monitor`]) is a **read-only diagnostic mirror** of
//! relay state — a DIFFERENT region that the Phase-9A live tests correctly
//! found rejects writes. The real control surface is this AXI-GPIO at
//! `0x41220000`. The 32-bit `UartRelayReg{gap_cnt,nonce_gap_en,ro/co_relay_en}`
//! struct in bosminer maps its low 2 bits onto this 2-bit GPIO; `gap_cnt` /
//! `nonce_gap_en` are vestigial for the relay-enable purpose on am2.
//!
//! ## Scope / safety
//!
//! This is a 2-bit GPIO config write — pure FPGA-fabric logic, no power/thermal
//! surface. The caller gates it `DCENT_AM2_FPGA_UART_RELAY_COLD=1` + `a lab unit`
//! fingerprint + NOT-handoff (`!DCENT_AM2_TRUST_RAIL_FALLBACK`). When that gate
//! is unset, the caller is a no-op. When the gate is set, the caller treats any
//! `/dev/mem`, mmap, or readback failure as fail-closed before enum. This helper
//! does NOT touch the dsPIC, the PSU, the chip rail, fans, or any EEPROM.
//!  handoff can inherit this GPIO from bosminer before `killall`, but the
//! v+2 run proved the relay alone does not wake the cold standalone path.

use std::fs;
use std::num::NonZeroUsize;

use nix::sys::mman::{MapFlags, ProtFlags};

use crate::{HalError, Result};

/// Physical base of the am2 `gpio@41220000` AXI-GPIO IP — the FPGA UART
/// return-relay control. LIVE-PINNED 2026-06-11 (the only `/dev/mem` map
/// bosminer makes). 4 KB-page-aligned.
pub const AM2_UART_RELAY_GPIO_BASE: u64 = 0x4122_0000;

/// AXI-GPIO channel-1 DATA register offset (drives the 2 relay bits).
pub const AXI_GPIO_DATA_OFFSET: u32 = 0x00;

/// AXI-GPIO channel-1 TRI (direction) register offset. Per Xilinx AXI-GPIO:
/// bit = 1 → input, bit = 0 → output. We must drive the relay bits, so their
/// TRI bits must be 0 (output).
pub const AXI_GPIO_TRI_OFFSET: u32 = 0x04;

/// The two relay-enable bits in the DATA register: bit0 `co_relay_en`,
/// bit1 `ro_relay_en`. Live-read as `0x3` on a bosminer-engaged `a lab unit`.
pub const UART_RELAY_ENABLE_BITS: u32 = 0b11;

/// One page is enough — all AXI-GPIO registers live below +0x12C.
const MAP_SIZE: usize = 4096;

/// Pre/post readback of the relay enable, for the caller's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartRelayEnableResult {
    pub data_pre: u32,
    pub tri_pre: u32,
    pub data_post: u32,
    pub tri_post: u32,
}

impl UartRelayEnableResult {
    /// `true` iff both relay bits read back as driven outputs
    /// (DATA bits[1:0] = 0b11 and TRI bits[1:0] = 0b00) — i.e. the relay is
    /// confirmed enabled exactly like the live bosminer state.
    pub fn relay_confirmed(&self) -> bool {
        (self.data_post & UART_RELAY_ENABLE_BITS) == UART_RELAY_ENABLE_BITS
            && (self.tri_post & UART_RELAY_ENABLE_BITS) == 0
    }
}

/// Drive the am2 FPGA UART return relay (`co_relay_en` + `ro_relay_en`) HIGH,
/// configured as outputs, by mmapping `/dev/mem` at `0x41220000` and doing a
/// read-modify-write of the AXI-GPIO DATA + TRI registers.
///
/// Order: set DATA bits high first, then clear the TRI bits (output-enable
/// last so the output transition drives the already-prepared value). All other
/// bits are preserved via read-modify-write (the IP is only 2-bit, but RMW is
/// the safe form). Reads back both registers and returns them for logging.
///
/// Any `/dev/mem` open or mmap error is returned as `Err`;  treats that
/// as fail-closed when the relay env gate is set.
pub fn enable_am2_uart_relay_cold() -> Result<UartRelayEnableResult> {
    let path = "/dev/mem";
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| HalError::DeviceOpen {
            path: path.to_string(),
            source: e,
        })?;

    let len = NonZeroUsize::new(MAP_SIZE).expect("MAP_SIZE is a non-zero compile-time constant");

    // SAFETY: standard /dev/mem MMIO mapping of one page at a 4 KB-aligned
    // physical base. The fd stays open through all the volatile ops below.
    let mapped = unsafe {
        nix::sys::mman::mmap(
            None,
            len,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &file,
            AM2_UART_RELAY_GPIO_BASE as nix::libc::off_t,
        )
    }
    .map_err(|e| HalError::MmapFailed {
        device: format!("{}@0x{:08X}", path, AM2_UART_RELAY_GPIO_BASE),
        source: e,
    })?;

    let base = mapped.as_ptr() as *mut u8;

    // SAFETY: `base` is a 4 KB RW MMIO mapping; offsets 0x00/0x04 are in-bounds
    // and 4-byte aligned. Volatile ops only — no thread-local state.
    let result = unsafe {
        let data_ptr = base.add(AXI_GPIO_DATA_OFFSET as usize) as *mut u32;
        let tri_ptr = base.add(AXI_GPIO_TRI_OFFSET as usize) as *mut u32;

        let data_pre = std::ptr::read_volatile(data_ptr as *const u32);
        let tri_pre = std::ptr::read_volatile(tri_ptr as *const u32);

        // Drive the relay bits HIGH (preserve any other DATA bits).
        std::ptr::write_volatile(data_ptr, data_pre | UART_RELAY_ENABLE_BITS);
        // Configure the relay bits as OUTPUT (TRI bit = 0; preserve others).
        std::ptr::write_volatile(tri_ptr, tri_pre & !UART_RELAY_ENABLE_BITS);

        let data_post = std::ptr::read_volatile(data_ptr as *const u32);
        let tri_post = std::ptr::read_volatile(tri_ptr as *const u32);

        UartRelayEnableResult {
            data_pre,
            tri_pre,
            data_post,
            tri_post,
        }
    };

    // The device register retains its value after unmap; release the mapping
    // (this is a one-shot, unlike the long-lived UIO mappings elsewhere).
    // SAFETY: `mapped` is the exact pointer/len returned by mmap above; no
    // further dereference happens after this point.
    let _ = unsafe { nix::sys::mman::munmap(mapped, MAP_SIZE) };

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_gpio_base_is_0x41220000() {
        // LIVE-PINNED 2026-06-11: the only /dev/mem map bosminer makes.
        assert_eq!(AM2_UART_RELAY_GPIO_BASE, 0x4122_0000);
    }

    #[test]
    fn axi_gpio_offsets_are_data0_tri4() {
        assert_eq!(AXI_GPIO_DATA_OFFSET, 0x00);
        assert_eq!(AXI_GPIO_TRI_OFFSET, 0x04);
    }

    #[test]
    fn enable_bits_are_co_bit0_ro_bit1() {
        // co_relay_en = bit0, ro_relay_en = bit1 → both set = 0x3 (live value).
        assert_eq!(UART_RELAY_ENABLE_BITS, 0x3);
        assert_eq!(UART_RELAY_ENABLE_BITS & 0x1, 0x1, "co_relay_en bit0");
        assert_eq!((UART_RELAY_ENABLE_BITS >> 1) & 0x1, 0x1, "ro_relay_en bit1");
    }

    #[test]
    fn relay_confirmed_matches_live_bosminer_state() {
        // Live bosminer state: DATA=0x3 (both high), TRI=0x0 (both output).
        let live = UartRelayEnableResult {
            data_pre: 0x0000_0000,
            tri_pre: 0xFFFF_FFFF,
            data_post: 0x0000_0003,
            tri_post: 0x0000_0000,
        };
        assert!(
            live.relay_confirmed(),
            "DATA=0x3 + TRI=0x0 must read as confirmed"
        );

        // Relay still disabled (cold-boot reset default tri=0xffffffff = input).
        let cold = UartRelayEnableResult {
            data_pre: 0,
            tri_pre: 0xFFFF_FFFF,
            data_post: 0x0000_0003,
            tri_post: 0xFFFF_FFFF, // bits still inputs → NOT driving
        };
        assert!(
            !cold.relay_confirmed(),
            "TRI bits still input ⇒ not confirmed"
        );
    }

    #[test]
    fn map_size_is_one_page() {
        assert_eq!(MAP_SIZE, 4096);
    }

    #[test]
    fn doc_keeps_live_pinned_provenance() {
        // Regression-pin the LIVE-PINNED provenance so a future agent does not
        // delete the 0x41220000 / 0x43d00000-mirror reconciliation or the
        // H1-falsified boundary as "noise".
        let src = include_str!("fpga_uart_relay.rs");
        assert!(src.contains("LIVE-PINNED 2026-06-11"));
        assert!(src.contains("not sufficient"));
        assert!(src.contains("0x41220000"));
        assert!(
            src.contains("0x43d00000") || src.contains("miner-glitch-monitor"),
            "must keep the mirror-vs-control reconciliation"
        );
    }
}
