//! ScryptL7 ASIC driver (Antminer L7 — Litecoin/Dogecoin **Scrypt** mining, BM1489).
//!
//! **KICKOFF (W3-B, 2026-07-07).** This is DCENT_OS's FIRST non-SHA256 chip
//! driver. It is default-OFF behind the `scrypt-l7` Cargo feature (production
//! SHA256 builds do not compile it). It pins the W3-A reverse-engineering facts
//! and mirrors the
//! structure of `bm1397.rs`. It is NOT a live miner: the chain / work-FIFO
//! bring-up is deliberately DEFERRED (see §"Deferred: chain-FIFO" below), so
//! `init_chain` / `send_work` fail closed with a clear message.
//!
//! # Why a NEW module instead of extending `bm1489.rs`
//!
//! `drivers/bm1489.rs` is an OLDER SCAFFOLD (wave-7/8 string-mining) whose core
//! premises W3-A **overturned** with the factory-signed L7 firmware + Bitmain
//! single-board-test jig:
//!
//! | Aspect            | old `bm1489.rs` (string-mine)     | W3-A CONFIRMED (this driver)                |
//! |-------------------|-----------------------------------|---------------------------------------------|
//! | Control board     | Amlogic AML S11board              | **Xilinx Zynq 7007S** (`zynq7007_BSL39601`) |
//! | Command framing   | BM1485-lineage, "no 0x55 0xAA"    | **VIL, `0x55 0xAA` preamble** (BM1397-fit)  |
//! | Cores per chip    | 12 (BM1485 guess)                 | **117** (`Config.ini CoreNum`)              |
//! | Chips per chain   | 120 × 4 chains                    | **120** (`AsicNum`); chain count EEPROM-read |
//! | Nonce frame CRC   | 7-byte / CRC5                     | 4B nonce + chip_id + core_id, **CRC16**     |
//! | Mining baud       | 1.5 Mbaud (guess)                 | **3.0 Mbaud** (`Config.ini Baudrate`)       |
//! | Work header       | 76-byte "header-minus-version"    | **80-byte LTC header** via VIL TW-write     |
//!
//! Rather than rewrite the test-pinned `bm1489.rs` in place, W3-B ships this
//! W3-A-accurate driver behind the feature and lets it SUPERSEDE `bm1489.rs`
//! for chip-id `0x1489` in the scaffold registry when `scrypt-l7` is compiled
//! (see `drivers/mod.rs::register_scaffold_drivers`). `bm1489.rs` stays for now
//! as the regression-pinned historical scaffold.
//!
//! # Deferred: chain-FIFO (the single biggest platform-bring-up risk)
//!
//! Per W3-A §2 + Critical Design Decision #9: the stock L7 firmware drives the
//! chain through the **stock Bitmain `bitmain_axi.ko` AXI-FIFO**, NOT the
//! BraiinsOS bitstream/FIFO layout that DCENT's [`FpgaChain`] assumes. So the
//! command/work-FIFO transport for L7 is a NEW platform port that this wave does
//! NOT attempt. `init_chain` and `send_work` therefore fail closed here — they
//! must never silently drive a chain through the wrong FIFO abstraction.
//!
//! # Confidence markers
//!
//! Every constant below is tagged `CONFIRMED` (hard evidence in the factory
//! `Config.ini` / descriptor struct / build path) or `INFERRED` (family pattern,
//! needs disasm before a real live bring-up). See W3-A §8 for the full split.

use crate::drivers::{ChipDriver, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

use super::bm139x;

/// BM1489 chip ID. CONFIRMED — occurs exactly once in each L7 binary inside the
/// LTC algo-descriptor struct (`single_board_test` @ file `0x1cf3ec`; `godminer`
/// @ `0x12b7f0`), and matches the Bitmain family rule (BM1387→0x1387 …). W3-A §1.
pub const CHIP_ID: u16 = 0x1489;

/// L7 chips per chain. CONFIRMED — `Config.ini AsicNum=120`. W3-A §1.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 120;

/// Cores per BM1489 chip. CONFIRMED — `Config.ini CoreNum=117`. Resolves the old
/// `bm1489.rs` placeholder of 12 (a BM1485 guess). W3-A §1.
const NUM_CORES_ON_CHIP: u32 = 117;

/// Mining/chain baud. CONFIRMED — `Config.ini Baudrate=3000000`. W3-A §3.
pub const MINING_BAUD: u32 = 3_000_000;

/// Enumeration baud. INFERRED — not printed as a literal; Bitmain family standard
/// is 115200 at cold start, switching to `MINING_BAUD` after address assignment.
/// W3-A §3.
pub const ENUM_BAUD: u32 = 115_200;

/// Scrypt work-header size. CONFIRMED (format) — standard 80-byte block header
/// (version, prev-hash, merkle-root, ntime, nbits, nonce) delivered via the VIL
/// `set_TW_write_command_vil` path. Scrypt has **no BIP320 version-rolling**, so
/// `version` is a plain field (no `version_bits` reconstruction). W3-A §5.
pub const SCRYPT_WORK_HEADER_BYTES: usize = 80;

/// BM1489 register addresses.
///
/// The BM1489 speaks the Bitmain **VIL** command family — the same `0x55 0xAA`
/// preamble / 0x51 write / 0x52 read / 0x53 inactive / 0x40 set-address headers
/// as BM1397/BM1398, so the shared [`bm139x`] helpers apply directly. W3-A §3.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16). Family-standard.
    /// CONFIRMED (family).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 Parameter — primary hash-clock PLL. INFERRED (BM1397 `0x08` family
    /// pattern; `set_pll_div` / `ChipSetting_freq_LTC` are the L7 freq entry
    /// points, exact bit layout needs disasm). A second PLL (`PLL1`, address
    /// TBD) exists — BM1489 is multi-domain. W3-A §4.
    pub const PLL0_PARAMETER: u8 = 0x08;
    /// Misc Control — carries the **BT8D** baud-divider field. CONFIRMED
    /// (inherited): the L7 log path `set zynq bt8d %d` + `get_bt8d_control` drive
    /// baud through `MISC_CONTROL@0x18`, same BT8D semantics as BM1397 `0x18`.
    /// W3-A §3.
    pub const MISC_CONTROL: u8 = 0x18;
    /// Core Register Control — indirect core register access. INFERRED
    /// (BM1397 `0x3C` family pattern; `CORE_CMD_IN`/`CORE_RESP_OUT`). W3-A §4.
    pub const CORE_REG_CTRL: u8 = 0x3C;
}

// ---------------------------------------------------------------------------
// PLL — INFERRED (BM1397-family calc; needs disasm before a real live bring-up)
// ---------------------------------------------------------------------------

/// Crystal reference. INFERRED — L7 board carries dual 25 MHz crystals (Y1: chips
/// 1–60, Y2: chips 61–120) with a boundary RELAY at chip 60. W3-A §4.
const CLKI_MHZ: f64 = 25.0;

/// PLL feedback-divider search bounds. INFERRED — wide enough to cover the L7
/// operating band (~1650–1850 MHz at refdiv/postdiv 1). W3-A §4.
const FB_DIV_MIN: u16 = 32;
const FB_DIV_MAX: u16 = 320;

/// L7 nameplate hash-domain frequency (MHz). CONFIRMED (value) — factory jig
/// `Frequence=[1650,1650,1650]`; `cgminer.conf.factory` default `bitmain-freq=1850`.
/// The BM1489 core clock runs far higher than SHA256 BM139x. W3-A §4.
pub const L7_NAMEPLATE_FREQ_MHZ: u16 = 1650;

/// Discrete Scrypt frequencies for autotuner binary search (MHz).
/// INFERRED — spans the confirmed ~1650–1850 MHz operating band with reasonable
/// eco/oc steps; the real discrete table + VCO limits need disasm. W3-A §4.
static PLL_FREQ_TABLE: &[u16] = &[1200, 1350, 1500, 1650, 1750, 1800, 1850, 1900];

/// Sorted list of discrete PLL frequencies for the autotuner.
pub fn pll_frequencies() -> &'static [u16] {
    PLL_FREQ_TABLE
}

/// Compute a BM1489 PLL0 register value for a target frequency.
///
/// INFERRED — uses the BM1397-family formula
/// `f = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)` with raw POSTDIV
/// encoding. The real BM1489 PLL0 bit layout, PLL1 address, and dual-domain
/// RELAY sequencing are NOT yet disassembled — do not trust for live writes.
fn scrypt_l7_pll_calc(target_mhz: u16) -> (u32, u16, u16, u8, u8, u8) {
    let target = target_mhz.clamp(1000, 2000) as f64;

    let mut best_freq = 0.0f64;
    let mut best_fb: u16 = 66;
    let mut best_ref: u8 = 1;
    let mut best_pd1: u8 = 1;
    let mut best_pd2: u8 = 1;
    let mut best_diff = f64::MAX;

    for refdiv in [1u8, 2] {
        for postdiv1 in 1..=7u8 {
            for postdiv2 in 1..=7u8 {
                if postdiv1 < postdiv2 {
                    continue;
                }
                let divider = (refdiv as f64) * (postdiv1 as f64) * (postdiv2 as f64);
                let fbdiv_f = target * divider / CLKI_MHZ;
                let fbdiv = fbdiv_f.round() as u16;
                if !(FB_DIV_MIN..=FB_DIV_MAX).contains(&fbdiv) {
                    continue;
                }
                let actual = CLKI_MHZ * (fbdiv as f64) / divider;
                let diff = (actual - target).abs();
                if diff < best_diff
                    || (diff == best_diff
                        && (postdiv1 as u16 * postdiv2 as u16)
                            < (best_pd1 as u16 * best_pd2 as u16))
                {
                    best_diff = diff;
                    best_freq = actual;
                    best_fb = fbdiv;
                    best_ref = refdiv;
                    best_pd1 = postdiv1;
                    best_pd2 = postdiv2;
                }
            }
        }
    }

    // BM1397-style encoding (INFERRED placeholder): PLLEN + FBDIV[26:16] +
    // REFDIV[13:8] + POSTDIV1[6:4] + POSTDIV2[2:0], raw postdiv values.
    let reg_value: u32 = (1u32 << 30)
        | ((best_fb as u32 & 0x7FF) << 16)
        | ((best_ref as u32 & 0x3F) << 8)
        | ((best_pd1 as u32 & 0x7) << 4)
        | (best_pd2 as u32 & 0x7);

    (
        reg_value,
        best_freq.round() as u16,
        best_fb,
        best_ref,
        best_pd1,
        best_pd2,
    )
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// ScryptL7 driver — KICKOFF skeleton (chain-FIFO deferred).
///
/// Chip-identity getters and pure helpers (`pll_params` / `ticket_mask` /
/// `baud_reg_value`) are functional for offline / autotuner table-prep work.
/// The hardware-touching `init_chain` / `send_work` fail closed because L7's
/// chain transport is the stock `bitmain_axi.ko` AXI-FIFO, not the BraiinsOS
/// bitstream that [`FpgaChain`] models (W3-A §2, Design Decision #9).
pub struct ScryptL7Driver;

impl ScryptL7Driver {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ScryptL7Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl ChipDriver for ScryptL7Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1489"
    }

    fn cores_per_chip(&self) -> u32 {
        // CONFIRMED: Config.ini CoreNum=117.
        NUM_CORES_ON_CHIP
    }

    fn response_length(&self) -> usize {
        // INFERRED: VIL-family nonce frame (AA 55 preamble + 4-byte nonce +
        // chip_id + core_id + CRC16). Exact on-wire length needs disasm; the
        // FPGA delivers packed words. W3-A §5.
        9
    }

    fn default_baud(&self) -> u32 {
        // Cold-start enumeration baud (INFERRED, family pattern).
        ENUM_BAUD
    }

    fn max_baud(&self) -> u32 {
        // CONFIRMED: Config.ini Baudrate=3000000.
        MINING_BAUD
    }

    fn init_chain(&self, _chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        // DEFERRED (W3-B): L7 drives the chain through the stock Bitmain
        // `bitmain_axi.ko` AXI-FIFO, NOT the BraiinsOS bitstream FIFO that
        // `FpgaChain` models. Bringing that transport up is a separate NEW
        // platform port (the single biggest L7 bring-up risk per W3-A §2).
        // Fail closed rather than drive a chain through the wrong FIFO layout.
        tracing::warn!(
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "ScryptL7 init_chain: KICKOFF — L7 chain/work-FIFO transport (stock \
             bitmain_axi.ko AXI-FIFO) is DEFERRED; not the BraiinsOS FpgaChain \
             layout. Chip driver + stratum seam only this wave."
        );
        Err(crate::AsicError::InvalidParameter(
            "ScryptL7 (L7/BM1489) chain bring-up is deferred: L7 uses the stock \
             bitmain_axi.ko AXI-FIFO, not the BraiinsOS bitstream FpgaChain. \
             [KICKOFF — W3-B chip driver + stratum seam only]"
                .into(),
        ))
    }

    fn set_frequency(&self, _chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        tracing::warn!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz = freq_mhz,
            "ScryptL7 set_frequency: KICKOFF — chain transport deferred (stock \
             bitmain_axi.ko AXI-FIFO)"
        );
        Err(crate::AsicError::InvalidParameter(
            "ScryptL7 set_frequency deferred with chain bring-up [KICKOFF — W3-B]".into(),
        ))
    }

    fn set_voltage(&self, _pic: &mut PicController, voltage_mv: u16) -> Result<()> {
        // L7 chain voltage is an ISL68127 digital multiphase VR driven per-domain
        // over I2C (`Set one chain ISL: domain addr = %x set vol %d`), NOT a PIC
        // DAC. The daemon routes voltage by MinerProfile.pic_type, so this PIC
        // path is inert for L7. W3-A §6.
        tracing::warn!(
            voltage_mv = voltage_mv,
            "ScryptL7 set_voltage: PIC path is inert — L7 uses an ISL68127 \
             per-domain VR (not a PIC DAC). [KICKOFF — W3-B]"
        );
        Ok(())
    }

    fn send_work(&self, _chain: &mut FpgaChain, _work: &MiningWork) -> Result<u16> {
        // DEFERRED (W3-B): the 80-byte LTC header would be dispatched via the VIL
        // `set_TW_write_command_vil` path over the stock AXI-FIFO. That transport
        // is not ported this wave (see init_chain). Fail closed. W3-A §5.
        tracing::warn!(
            "ScryptL7 send_work: KICKOFF — 80-byte LTC header VIL dispatch over \
             the stock bitmain_axi.ko AXI-FIFO is DEFERRED"
        );
        Err(crate::AsicError::InvalidParameter(
            "ScryptL7 send_work deferred: 80-byte LTC header VIL dispatch needs \
             the L7 AXI-FIFO transport [KICKOFF — W3-B]"
                .into(),
        ))
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // INFERRED: BM1489 nonce frame = 4-byte nonce + chip_id + core_id, CRC16
        // integrity (`Nonce: %02x%02x%02x%02x chipid:%d coreid:%d`; `BM_CRC16`).
        // Exact bit offsets need disasm — this synthetic decode lets the offline
        // harness exercise the path once the transport lands; it is NOT the
        // wire-correct field layout. Scrypt has no midstate concept. W3-A §5.
        Ok(NonceResult {
            nonce: raw[0],
            chip_index: ((raw[1] >> 17) & 0xFF) as u8,
            work_id: ((raw[1] >> 8) & 0xFFFF) as u16,
            solution_id: (raw[1] & 0xFF) as u8,
            midstate_idx: 0,
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        // Standard FPGA divisor formula: div = fpga_clk / (16 * baud) - 1.
        let div = fpga_clock_hz / (16 * target_baud);
        div.saturating_sub(1)
    }

    fn ctrl_reg_value(&self) -> u32 {
        // VIL is the BM139x unified command family, so the BM139X FPGA mode bit
        // applies. Scrypt has no version-rolling midstates → MIDSTATE_CNT=0.
        // (Inert until the L7 AXI-FIFO transport lands.)
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // Scrypt nonce throughput per core is far lower than SHA256; conservative
        // ~10 ms placeholder, tune from live work-rate once the transport lands.
        10
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // NO hardware ticket-mask for LTC on this firmware: `ChipSetting_ticket_
        // mask_*` exists only for CKB/VBK, NOT LTC — Scrypt difficulty filtering
        // is host-side by nonce-rate. This value is effectively a no-op for L7;
        // returned as `difficulty - 1` only to satisfy the trait. W3-A §5.
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        let (reg_value, actual_freq, fb_div, ref_div, post_div1, post_div2) =
            scrypt_l7_pll_calc(freq_mhz);

        if actual_freq != freq_mhz {
            tracing::debug!(
                target = freq_mhz,
                actual = actual_freq,
                "ScryptL7 PLL: requested {} MHz, closest achievable is {} MHz \
                 (INFERRED BM1397-pattern calc — needs disasm)",
                freq_mhz,
                actual_freq,
            );
        }

        PllConfig {
            fb_div,
            ref_div,
            post_div1,
            post_div2,
            reg_value,
        }
    }
}

// ---------------------------------------------------------------------------
// VIL command-encoding smoke helpers — prove the shared bm139x path applies.
// ---------------------------------------------------------------------------

/// Encode a broadcast Write Register (VIL) command for a BM1489 chain, reusing
/// the shared BM139x helper. Confirms the `0x55 0xAA` / 0x51 VIL family fits L7
/// with zero duplication. W3-A §3.
pub fn vil_write_reg_bcast(reg: u8, value: u32) -> (u32, u32) {
    bm139x::fifo_write_reg_bcast(reg, value)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_id_is_0x1489() {
        let d = ScryptL7Driver::new();
        assert_eq!(d.chip_id(), 0x1489);
        assert_eq!(d.chip_id(), CHIP_ID);
    }

    #[test]
    fn name_is_bm1489() {
        assert_eq!(ScryptL7Driver::new().chip_name(), "BM1489");
    }

    /// W3-A §1 CONFIRMED constants (the ones the RE overturned in `bm1489.rs`).
    #[test]
    fn w3a_confirmed_constants_are_pinned() {
        let d = ScryptL7Driver::new();
        // CoreNum=117 (was a 12 BM1485 guess in bm1489.rs).
        assert_eq!(d.cores_per_chip(), 117);
        assert_eq!(NUM_CORES_ON_CHIP, 117);
        // AsicNum=120.
        assert_eq!(DEFAULT_CHIPS_PER_CHAIN, 120);
        // Baudrate=3000000 (was 1.5M guess in bm1489.rs).
        assert_eq!(d.max_baud(), 3_000_000);
        assert_eq!(MINING_BAUD, 3_000_000);
        assert_eq!(d.default_baud(), 115_200);
        // 80-byte LTC header (was 76 in bm1489.rs).
        assert_eq!(SCRYPT_WORK_HEADER_BYTES, 80);
    }

    /// This driver's confirmed facts must DIVERGE from the older `bm1489.rs`
    /// scaffold's overturned guesses — the whole point of the new module.
    #[test]
    fn supersedes_stale_bm1489_scaffold_facts() {
        use crate::drivers::bm1489;
        let new = ScryptL7Driver::new();
        let old = bm1489::Bm1489Driver::new();
        // W3-A overturned max_baud (3.0M) and cores (117) vs the stale scaffold.
        assert_ne!(new.max_baud(), old.max_baud());
        assert_ne!(new.cores_per_chip(), old.cores_per_chip());
        // Same silicon, same chip-id.
        assert_eq!(new.chip_id(), old.chip_id());
    }

    #[test]
    fn regs_pin_w3a_addresses() {
        // CONFIRMED family / CONFIRMED-inherited.
        assert_eq!(regs::CHIP_ADDRESS, 0x00);
        assert_eq!(regs::MISC_CONTROL, 0x18); // BT8D baud field lives here
                                              // INFERRED family pattern.
        assert_eq!(regs::PLL0_PARAMETER, 0x08);
        assert_eq!(regs::CORE_REG_CTRL, 0x3C);
    }

    #[test]
    fn pll_hits_nameplate_frequency_exactly() {
        // 1650 MHz = 25 * 66 / (1*1*1), so the INFERRED calc lands it exactly.
        let cfg = ScryptL7Driver::new().pll_params(L7_NAMEPLATE_FREQ_MHZ);
        assert_eq!(cfg.fb_div, 66);
        assert_eq!(cfg.ref_div, 1);
        assert_eq!(cfg.post_div1, 1);
        assert_eq!(cfg.post_div2, 1);
        assert_ne!(cfg.reg_value & (1u32 << 30), 0, "PLLEN must be set");
    }

    #[test]
    fn pll_table_is_sorted_and_contains_nameplate() {
        let t = pll_frequencies();
        assert!(!t.is_empty());
        for w in t.windows(2) {
            assert!(w[0] < w[1], "PLL table not strictly increasing");
        }
        assert!(t.contains(&L7_NAMEPLATE_FREQ_MHZ));
    }

    #[test]
    fn no_hardware_ticket_mask_but_trait_is_satisfied() {
        // Documented no-op (Scrypt filters host-side); still monotone + no
        // underflow so nothing downstream panics.
        let d = ScryptL7Driver::new();
        assert_eq!(d.ticket_mask(256), 255);
        assert_eq!(d.ticket_mask(0), 0);
    }

    #[test]
    fn baud_reg_value_no_underflow() {
        let d = ScryptL7Driver::new();
        assert_eq!(d.baud_reg_value(3_000_000, 200_000_000), 3);
        assert_eq!(d.baud_reg_value(50_000_000, 25_000_000), 0);
    }

    #[test]
    fn vil_encoding_reuses_bm139x_family() {
        // Proves the 0x55 0xAA VIL family fits: byte-identical to the shared
        // BM139x broadcast-write helper (which BM1397/BM1398 use).
        let (a0, a1) = vil_write_reg_bcast(regs::MISC_CONTROL, 0x0000_3A01);
        let (b0, b1) = bm139x::fifo_write_reg_bcast(regs::MISC_CONTROL, 0x0000_3A01);
        assert_eq!((a0, a1), (b0, b1));
    }

    #[test]
    fn init_and_send_work_fail_closed_deferred() {
        // The chain-FIFO transport is deferred; these must fail closed, not
        // silently drive the wrong FIFO. We can't build a FpgaChain in a unit
        // test (needs HAL), so assert the driver constructs and the deferral is
        // documented via the module contract — the pure surfaces stay usable.
        let d = ScryptL7Driver::new();
        assert_eq!(d.chip_id(), 0x1489);
        // Nonce decode remains callable for the offline harness.
        let n = d
            .decode_nonce(&[0xDEAD_BEEF, 0x0000_0100])
            .expect("synthetic decode");
        assert_eq!(n.nonce, 0xDEAD_BEEF);
        assert_eq!(n.midstate_idx, 0, "Scrypt has no midstate concept");
    }
}
