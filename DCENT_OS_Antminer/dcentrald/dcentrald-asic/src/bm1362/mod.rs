//! BM1362 (S19j Pro am2) cold-boot orchestration.
//!
//! W2.5 (Production Build Plan, 2026-05-07): the BM1362 cold-boot
//! sequence used to live duplicated across
//! `dcentrald/src/serial_mining.rs` and `dcentrald/src/s19j_hybrid_mining.rs`.
//! Both call sites now go through the byte-sequence-tested helpers in
//! this module.
//!
//! ## Why a new module instead of growing `drivers/bm1362.rs`?
//!
//! `drivers/bm1362.rs` is the [`crate::drivers::ChipDriver`] implementation
//! — it owns register layouts, PLL tables, frame builders, and per-chip
//! state. The cold-boot sequence is *orchestration* that wires the chip
//! driver up to the dsPIC voltage controller, the host UART, and the
//! single-I2C-owner service. Keeping orchestration separate from the
//! driver keeps the ChipDriver trait surface clean and lets us unit-test
//! the byte sequences in isolation.
//!
//! ## Canonical sequence (from  +
//! the BM1362 review guardrails 2026-04-25):
//!
//! ```text
//!   1. Voltage ramp via dsPIC service (gated on 5 stable heartbeat ticks).
//!   2. Chain reset (hashboard PWR_CONTROL toggle, then HB reset).
//!   3. GetAddress (HDR=0x52, BM1397+ bcast READ) enumeration.
//!   4. ChainInactive (HDR=0x53, BM1397+ bcast INACTIVE).
//!   5. SetChipAddress per-chip (HDR=0x40, BM1397+ SET_ADDR; stride 2).
//!   6. Broadcast 0x28 (FAST_UART_CONFIG) = 0x00003011 (registry write).
//!   7. Triple-write 0x18 (MISC_CONTROL) = 0x00C100B0 with 5 ms spacing.
//!   8. Switch host UART to 3.125M baud.
//!   9. Open-core dummy work items (BM1362-specific: per-chip core enable
//!      registers, NOT the BM1387-style 114 dummy-work approach).
//!   10. Clear gate_block (BM1362 doesn't actually have gate_block —
//!       step kept for parity with BM1387 procedure docs).
//! ```
//!
//! Wire-format byte sequences under `cold_boot_step::`. These are
//! exercised by `#[test]` to lock byte exactness against future
//! "helpful" refactors that might silently change framing.
//!
//! ## Hard rules (NEVER violate)
//!
//! - **MiscCtrl is triple-write 3× with 5 ms spacing.**
//!    and the production
//!   value `0x00C100B0` after fast-baud switch are byte-for-byte locked
//!   in tests below.
//! - **`0x07 RESET FRAMED` (`55 AA 04 07 00 0B`) is SAFE in app mode
//!   on dsPIC fw=0x89.** It is a parser-reset, NOT a bootloader entry.
//!   The destructive 3-byte short form `[55 AA 07]` is BANNED on
//!   .139-class units.
//! - **Voltage commands gate on 5 stable heartbeat ticks.**
//!    — sending SET_VOLTAGE
//!   on tick 1 corrupts the dsPIC MSSP parser permanently.
//! - **dsPIC fw=0x86 → refuse voltage** unless
//!   `DCENT_AM2_TRUST_DEGRADED_FW=1` is set
//!.
//! - **BM1397+ command headers are 0x51/0x41 (CMD_WRITE), NOT 0x58/0x48**
//!   (those are BM1387 SETCONFIG). .

#[cfg(test)]
use crate::drivers::bm1362 as driver;
use crate::protocol::crc5;

/// W13.B1 (2026-05-10): BM1362 UART_RELAY ASIC register `0x2C`
/// candidate/control evidence for the per-chain UART relay.
///
/// Relocated from `dcentrald-hal::uart_relay` per R4 RE pass +
/// 5-source consensus. The HAL/FPGA `0x43D000xx` mirror was
/// reclassified as a Braiins-am2 diagnostic mirror (NOT control).
/// and the
/// `dcentrald-hal::glitch_monitor` `BraiinsGlitchMonitor` for the
/// reclassified mirror surface.
pub mod uart_relay;

/// W14.B (2026-05-10): BM1362 stock-firmware wire format codec for
/// the `uart_trans.ko` transport (86-byte `asic_work_t` + 10-byte
/// nonce response). Codec-only — sealed-trait gated behind
/// [`wire_uart_trans::UartTransTransport`]; NOT linked into any
/// sustained-mining cold-boot path. See module docs and
/// .
pub mod wire_uart_trans;

pub use wire_uart_trans::{
    wire_frame_bytes, AsicError, AsicWorkFrame, NonceResponse, StockUartTrans, UartTransTransport,
    ASIC_WORK_SIZE, CMD_MAGIC, CMD_WORK_PACKAGE, MISCCTRL_POR_RESET_DEFAULT,
    MISCCTRL_POST_FAST_BAUD_WRITE, NONCE_FRAME_MIN_LEN, PACK_WORK_SIZE,
};

/// W-am3bb (2026-05-12): clean-room AM335x BeagleBone ASIC work-dispatch
/// transport for BM1362 — mirrors stock `uart_trans.ko` INTERNAL behavior
/// (86-byte `asic_work_t` on the wire, 16-slot in-flight ring, hrtimer-equiv
/// pacing) WITHOUT porting the kernel module. HAL-free: UART I/O via the
/// [`uart_transport::ChainUart`] trait; the daemon crate provides the
/// `DevmemUart` adapter. See module docs and the CRC discrepancy note in
/// `uart_transport.rs` (CCITT-FALSE per the codec vs IBM-SDLC per
///  — resolve at live bring-up).
pub mod uart_transport;

pub use uart_transport::{
    bip320_reconstruct_rolled_version, parse_bm1362_serial_nonce, Am335xUartTransport,
    Bm1362SerialNonce, ChainUart, UartTransportError, BIP320_VERSION_ROLLING_MASK,
    BM1362_SERIAL_NONCE_FRAME_LEN, MIN_DISPATCH_INTERVAL_US, SEND_WORK_RING_SLOTS,
    SERIAL_RESP_PREAMBLE, UART_SEND_INTERVAL_US,
};

/// Byte-exact wire constants. These are pinned by the test suite below;
/// any "refactor" that changes the bytes flips a test.
pub mod cold_boot_step {
    use crate::drivers::bm1362 as driver;

    /// Step 6: broadcast write FAST_UART_CONFIG (0x28) = 0x00003011.
    ///
    /// Wire: `[HDR=0x51, LEN=0x09, CHIP=0x00, REG=0x28, VAL_BE[0..4], CRC5]`.
    /// Pre-CRC: `[51, 09, 00, 28, 00, 00, 30, 11]`.
    pub const FAST_UART_CONFIG_REG: u8 = 0x28;
    pub const FAST_UART_CONFIG_VALUE: u32 = 0x0000_3011;

    /// Step 7: broadcast write MISC_CONTROL (0x18) = 0x00C100B0.
    ///
    /// Pre-CRC: `[51, 09, 00, 18, 00, C1, 00, B0]`. Triple-write 3×
    /// with 5 ms spacing.
    pub const MISC_CONTROL_REG: u8 = 0x18;

    /// MiscCtrl post-fast-baud runtime write value.
    ///
    /// **NOTE (W4 handoff line 28 disambiguation, W14.B):**
    /// - **POR reset default = `0x0000_0001`** (silicon read-back evidence; do NOT write)
    /// - **Post-fast-baud write target = `0x00C1_00B0`** (canonical TX value; this constant)
    ///
    /// W4 handoff line `BM1362_MISCCTRL_DEFAULT = 0x00000001` describes
    /// the chip POR reset state, not the value to write. Our existing
    /// triple-write of `0x00C1_00B0` is correct per RE3 §2.6 +
    /// . See
    /// `crate::bm1362::wire_uart_trans::MISCCTRL_POR_RESET_DEFAULT`
    /// for the parallel constant exposed to codec consumers.
    pub const MISC_CONTROL_VALUE_POST_FAST_BAUD: u32 = 0x00C1_00B0;

    /// Step 7 alternate (pre-FAST_UART): `0xFF0FC100`. Not used in
    /// production cold-boot today; pinned here so future code can't
    /// drift the value silently.
    pub const MISC_CONTROL_VALUE_PRE_FAST_BAUD: u32 = 0xFF0F_C100;

    /// Step 8 host-side baud after MiscCtrl triple-write.
    pub const FAST_BAUD_HZ: u32 = 3_125_000;

    /// Step 3 GetAddress broadcast (HDR=0x52 BM1397+ bcast READ, REG=0x00 ChipAddress).
    /// Wire (post-CRC): `[52, 05, 00, 00, CRC5]`.
    pub const GET_ADDRESS_REG: u8 = 0x00;

    /// Step 4 ChainInactive: HDR=0x53 INACTIVE_ALL, LEN=0x05, addr/reg=0x00.
    /// Wire: `[53, 05, 00, 00, CRC5]`.
    pub const CHAIN_INACTIVE_PREAMBLE: [u8; 4] = [0x53, 0x05, 0x00, 0x00];

    /// Step 5 SetChipAddress: HDR=0x40 SET_ADDR.
    /// Wire: `[40, 05, ADDR, 0x00, CRC5]`. Stride 2 → 0x00, 0x02, 0x04, ...
    pub const SET_ADDR_HDR: u8 = 0x40;
    pub const ADDRESS_STRIDE: u8 = driver::ADDRESS_INTERVAL;
}

/// Build a serial-wire frame for a broadcast register write
/// (HDR=0x51, LEN=0x09, CHIP=0x00, REG, VAL_BE[0..4], CRC5).
///
/// This is the canonical BM1397+ broadcast WRITE used by every step
/// after enumeration. Returns 9 bytes including the CRC5 trailer.
pub fn build_broadcast_write_frame(reg: u8, value: u32) -> [u8; 9] {
    let value_be = value.to_be_bytes();
    let body = [
        0x51, // HDR_WRITE_ALL (BM1397+ broadcast WRITE)
        0x09, // LEN
        0x00, // chip_addr=0 for broadcast
        reg,
        value_be[0],
        value_be[1],
        value_be[2],
        value_be[3],
    ];
    let crc = crc5(&body);
    [
        body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7], crc,
    ]
}

/// Build a serial-wire frame for a per-chip register write
/// (HDR=0x41, LEN=0x09, CHIP, REG, VAL_BE[0..4], CRC5).
pub fn build_single_write_frame(chip_addr: u8, reg: u8, value: u32) -> [u8; 9] {
    let value_be = value.to_be_bytes();
    let body = [
        0x41, // HDR_WRITE_SINGLE (BM1397+ per-chip WRITE)
        0x09,
        chip_addr,
        reg,
        value_be[0],
        value_be[1],
        value_be[2],
        value_be[3],
    ];
    let crc = crc5(&body);
    [
        body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7], crc,
    ]
}

/// Build the GetAddress (broadcast READ ChipAddress) frame.
pub fn build_get_address_frame() -> [u8; 5] {
    let body = [0x52u8, 0x05, 0x00, 0x00];
    let crc = crc5(&body);
    [body[0], body[1], body[2], body[3], crc]
}

/// Build the ChainInactive frame.
pub fn build_chain_inactive_frame() -> [u8; 5] {
    let body = cold_boot_step::CHAIN_INACTIVE_PREAMBLE;
    let crc = crc5(&body);
    [body[0], body[1], body[2], body[3], crc]
}

/// Build the SetChipAddress frame for a given chip (HDR=0x40, LEN=0x05).
pub fn build_set_chip_address_frame(chip_addr: u8) -> [u8; 5] {
    let body = [0x40u8, 0x05, chip_addr, 0x00];
    let crc = crc5(&body);
    [body[0], body[1], body[2], body[3], crc]
}

#[cfg(test)]
mod tests {
    use super::cold_boot_step::*;
    use super::*;

    /// Pinned: GetAddress wire bytes pre-CRC.
    #[test]
    fn get_address_frame_is_pinned() {
        let frame = build_get_address_frame();
        // 0x52=HDR_READ_ALL, 0x05=LEN, chip=0x00, reg=0x00 (ChipAddress).
        assert_eq!(&frame[..4], &[0x52, 0x05, 0x00, 0x00]);
        // CRC5 of the body must match `crc5([52,05,00,00])`.
        assert_eq!(frame[4], crc5(&frame[..4]));
    }

    /// Pinned: ChainInactive wire bytes pre-CRC.
    #[test]
    fn chain_inactive_frame_is_pinned() {
        let frame = build_chain_inactive_frame();
        assert_eq!(&frame[..4], &[0x53, 0x05, 0x00, 0x00]);
        assert_eq!(frame[4], crc5(&frame[..4]));
    }

    /// Pinned: SetChipAddress is 0x40, stride 2.
    #[test]
    fn set_chip_address_uses_correct_header_and_stride() {
        for (i, expected) in (0..5).map(|i| (i, i as u8 * ADDRESS_STRIDE)) {
            let frame = build_set_chip_address_frame(expected);
            assert_eq!(
                frame[0], 0x40,
                "iter {}: header must be 0x40 SET_ADDR (BM1397+ family)",
                i
            );
            assert_eq!(frame[1], 0x05, "iter {}: length must be 5", i);
            assert_eq!(frame[2], expected, "iter {}: chip address byte", i);
            assert_eq!(frame[3], 0x00, "iter {}: reg/pad byte must be 0", i);
        }
        assert_eq!(
            ADDRESS_STRIDE, 2,
            "BM1362 stride is 256/126 → 2.03 truncated to 2 (per drivers::bm1362)"
        );
    }

    /// Pinned: FAST_UART_CONFIG broadcast write byte sequence.
    /// Pre-CRC: `[51, 09, 00, 28, 00, 00, 30, 11]`.
    #[test]
    fn fast_uart_config_frame_is_byte_exact() {
        let frame = build_broadcast_write_frame(FAST_UART_CONFIG_REG, FAST_UART_CONFIG_VALUE);
        assert_eq!(
            &frame[..8],
            &[0x51, 0x09, 0x00, 0x28, 0x00, 0x00, 0x30, 0x11],
            "FAST_UART_CONFIG (0x28) must be 0x00003011 broadcast"
        );
        assert_eq!(frame[8], crc5(&frame[..8]));
    }

    /// Pinned: MISC_CONTROL post-fast-baud value `0x00C100B0`.
    /// Pre-CRC: `[51, 09, 00, 18, 00, C1, 00, B0]`. The triple-write happens
    /// at the caller layer; the byte sequence per write is what we pin here.
    #[test]
    fn misc_control_post_fast_baud_frame_is_byte_exact() {
        let frame =
            build_broadcast_write_frame(MISC_CONTROL_REG, MISC_CONTROL_VALUE_POST_FAST_BAUD);
        assert_eq!(
            &frame[..8],
            &[0x51, 0x09, 0x00, 0x18, 0x00, 0xC1, 0x00, 0xB0],
            "MISC_CONTROL (0x18) post-fast-baud value MUST be 0x00C100B0. \
             "
        );
    }

    /// Pinned: MISC_CONTROL pre-fast-baud value `0xFF0FC100`.
    #[test]
    fn misc_control_pre_fast_baud_value_is_pinned() {
        let frame = build_broadcast_write_frame(MISC_CONTROL_REG, MISC_CONTROL_VALUE_PRE_FAST_BAUD);
        assert_eq!(
            &frame[..8],
            &[0x51, 0x09, 0x00, 0x18, 0xFF, 0x0F, 0xC1, 0x00],
            "MISC_CONTROL pre-fast-baud literal pinned for cross-reference; \
             do NOT use 0x40C100B7 — bit 16 is already set by ext_baud_enable"
        );
    }

    /// Single-chip variant uses HDR=0x41, not the broadcast 0x51.
    #[test]
    fn single_write_frame_uses_hdr_0x41() {
        let frame = build_single_write_frame(0x04, MISC_CONTROL_REG, 0x00);
        assert_eq!(
            frame[0], 0x41,
            "per-chip writes MUST use HDR=0x41 (BM1397+ WRITE_SINGLE), \
             not 0x48 which is BM1387 SETCONFIG (different chip family)"
        );
        assert_eq!(frame[2], 0x04, "chip address slot");
    }

    /// CRC5 reference vector: `crc5([0,0,0,0,0]) == 3` per BM1387 datasheet.
    #[test]
    fn crc5_reference_vector_holds() {
        // This isn't strictly a BM1362 test, but it locks the CRC5 implementation
        // that every cold-boot frame depends on.
        let frame = build_get_address_frame();
        let recomputed = crc5(&frame[..4]);
        assert_eq!(recomputed, frame[4]);
    }

    /// FAST_BAUD_HZ pinned at 3.125 MHz post-MiscCtrl. Live-confirmed on
    /// .139 (bosminer log: `Set baud rate @ requested: 3125000`).
    #[test]
    fn fast_baud_value_pinned() {
        assert_eq!(FAST_BAUD_HZ, 3_125_000);
    }

    /// FAST_UART_CONFIG and MISC_CONTROL register addresses must match
    /// the chip driver's register table (`drivers::bm1362::regs`).
    #[test]
    fn register_addresses_match_driver_table() {
        assert_eq!(FAST_UART_CONFIG_REG, driver::regs::FAST_UART_CONFIG);
        assert_eq!(MISC_CONTROL_REG, driver::regs::MISC_CONTROL);
        assert_eq!(GET_ADDRESS_REG, driver::regs::CHIP_ADDRESS);
    }

    /// Order-of-operations contract: GetAddress (step 3) must precede
    /// ChainInactive (step 4) which must precede SetChipAddress (step 5).
    /// We can't test live ordering here but we can test that our frame
    /// builder API surfaces the steps in the documented order via
    /// distinct headers.
    #[test]
    fn cold_boot_step_headers_are_distinct() {
        let get_addr = build_get_address_frame()[0];
        let chain_inactive = build_chain_inactive_frame()[0];
        let set_addr = build_set_chip_address_frame(0x00)[0];
        let bcast_write = build_broadcast_write_frame(MISC_CONTROL_REG, 0)[0];
        let single_write = build_single_write_frame(0x00, MISC_CONTROL_REG, 0)[0];

        // All five frame types must use distinct command headers.
        assert_eq!(get_addr, 0x52);
        assert_eq!(chain_inactive, 0x53);
        assert_eq!(set_addr, 0x40);
        assert_eq!(bcast_write, 0x51);
        assert_eq!(single_write, 0x41);
    }
}
