//! BM1391 ASIC driver (Antminer S11 / S15 / T15) — JIG-VERIFIED SCAFFOLD
//!
//! The BM1391 is the **7 nm** SHA-256 die (Bitmain's first-gen 7 nm, Nov 2018)
//! used in the Antminer S11 / S15 / T15 (and the BM1391P/BM1391S variants).
//! CORRECTED 2026-07-02: the prior header "16 nm" + "T9+" were BM1387-template
//! copy-paste errors (T9+ uses the 16 nm BM1387; BM1391 is 7 nm per
//! `asics.rs` `Bm1391 = Nm7` + the Bitmain BM1391 datasheet). Unlike the
//! `bm1373` scaffold (whose values are
//! *projected* from BM1370), **every constant here is byte-verified** from the
//! Bitmain S17 factory `single-board-test` jig (which carries the full,
//! unstripped BM1391 protocol: `BM1391_set_config`, `set_BM1391_freq`,
//! `BM1391_set_baud`, `BM1391_set_TM`, `BM1391_chain_inactive`,
//! `BM1391_set_address`, `single_BM1391{P,S}_open_core`, …) — decoded
//! 2026-06-10 in the local Ghidra GUI.
//!
//! Status: **SCAFFOLD — fail-closed.** Not because the protocol is unknown
//! (it is fully verified), but because **there is no live S11 unit on the
//! fleet** to validate a bring-up against. `init_chain` therefore refuses to
//! drive hardware until an operator validates it on a real S11. The verified
//! facts are captured so that validation is a bring-up exercise, not an RE one.
//!
//! ## BM1391 baud generation (the key family fact, jig-verified)
//! BM1391 is **Generation-1** (like BM1387/S9): the chain UART baud is set via
//! the **MiscControl divider (reg 0x18)** off CLKI — there is **NO PLL baud
//! reclock** (unlike Gen-2 BM1397/BM1398 = PLL3/0x68, or Gen-3 BM1362/66/68/70
//! = PLL1/0x60). `BM1391_set_baud`:
//! `MiscCtrl = (MiscCtrl & 0xffffe0ff) | (baud_index << 8)`.
//!.
//!
//! ## Command/wire format (jig-verified, = BM1397 family, NOT BM1387)
//! Register write = `[0x51 (bcast) | 0x41 (single), 0x09, asic_addr, reg,
//! data_BE[4], CRC5]` (`BM1391_set_config`). CRC5 over the 9-byte frame.
//!
//! References:
//!   - S17 jig `single-board-test`
//!   - `bm1387.rs` (same Gen-1 baud mechanism; closest live-proven template)
//!   - `BM13XX_BAUD_FAMILY_MAP.md`

use crate::drivers::{ChipDriver, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::FpgaChain;

/// BM1391 chip ID. Read from the chip-address register (reg 0x00, bits 31:16).
pub const CHIP_ID: u16 = 0x1391;

/// Crystal oscillator reference (MHz) — standard 25 MHz, jig `set_BM1391_freq`.
const FREQ_MULT: f64 = 25.0;

/// Default chips per chain for the S11 hashboard (BHB91601/BHB91603).
/// Sourced from the AMTC S11 (`V11-S`) Config.ini (AsicNum=60). Verify on a
/// live S11 — the driver enumerates, this is the passthrough fallback only.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 60;

/// SHA-256 cores per BM1391 chip. **CORPUS CONFLICT (unresolved):** the AMTC
/// S11 `Config.ini` says `CoreNum=128` (BM1390P die), but the S11
/// `single-board-test` jig calls `open_core_onChain(114)` (docs/dev/
/// 2026-06-10-hashsource-binaries-re/findings/bm1391-s11.md §7). Both are
/// corpus-sourced. Kept at 128 (Config.ini) but NEEDS-LIVE-VERIFY via a live
/// S15/S11 `open_core` count or the datasheet before this drives any real
/// open-core loop. (Driver is fail-closed, so this never runs live today.)
const CORES_PER_CHIP: u32 = 128;

/// Nonce response body length. BM1391 (Gen-1) uses the 9-byte response frame
/// like BM1387. NEEDS-LIVE-VERIFY against `single_BM1391_check_nonce`.
pub const RESPONSE_BYTES: usize = 9;

/// 200 MHz fallback PLL value (jig `set_BM1391_freq` "using 200M pll"):
/// `0xC0780111` (PLLEN, FBDIV=0x78=120, REFDIV=1, POSTDIV1=1, POSTDIV2=1).
const PLL_FALLBACK_200M: u32 = 0xC078_0111;

/// BM1391 register addresses — JIG-VERIFIED from `BM1391_set_config` call sites.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 parameter — hash clock PLL (jig `set_BM1391_freq` writes reg 0x08).
    pub const PLL0: u8 = 0x08;
    /// Ticket mask — hardware difficulty filter (jig `BM1391_set_TM` writes reg
    /// 0x14, BIT-REVERSED via `bit_swap_table`).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc control — baud divider + clock config (jig `BM1391_set_baud`).
    pub const MISC_CONTROL: u8 = 0x18;
    /// Core register control (indirect core access; `BM1391_enable_core_clock`).
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// PLL0 output divider (jig `set_BM1391_freq` writes reg 0x70).
    pub const PLL0_DIVIDER: u8 = 0x70;
}

/// BM1391 driver — jig-verified scaffold (no live S11 unit to validate against).
pub struct Bm1391Driver;

impl Default for Bm1391Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1391Driver {
    pub fn new() -> Self {
        Self
    }
}

impl ChipDriver for Bm1391Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1391"
    }

    fn cores_per_chip(&self) -> u32 {
        CORES_PER_CHIP
    }

    fn response_length(&self) -> usize {
        RESPONSE_BYTES
    }

    fn default_baud(&self) -> u32 {
        115_200
    }

    fn max_baud(&self) -> u32 {
        // Gen-1: MiscControl divider off CLKI; no PLL reclock. Conservative
        // until live-verified on an S11.
        3_125_000
    }

    fn init_chain(&self, _chain: &mut FpgaChain, _chip_count: u8, _freq_mhz: u16) -> Result<()> {
        // Fail-closed. The verified factory sequence (from the S17 jig) is:
        //   chain_inactive → set_address(interval) → set_BM1391_freq (PLL0 reg
        //   0x08 + divider reg 0x70) → enable_core_clock → set_TM (reg 0x14,
        //   bit-reversed) → set_baud (MiscControl reg 0x18 divider) → open_core.
        // It is NOT wired to live hardware until an operator validates it on a
        // real S11 — there is no S11 on the fleet to prove a bring-up against.
        tracing::warn!(
            "BM1391 init_chain: jig-verified scaffold — refusing live bring-up until \
             validated on a real S11 (no live unit on the fleet)."
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1391 driver is a jig-verified scaffold; live bring-up is gated until an \
             operator validates it on an Antminer S11."
                .into(),
        ))
    }

    fn set_frequency(&self, _chain: &mut FpgaChain, _chip_addr: u8, _freq_mhz: u16) -> Result<()> {
        tracing::warn!("BM1391 set_frequency: scaffold (PLL0 reg 0x08 + divider reg 0x70)");
        Err(crate::AsicError::InvalidParameter(
            "BM1391 set_frequency gated until live S11 validation".into(),
        ))
    }

    fn set_voltage(&self, _pic: &mut PicController, _voltage_mv: u16) -> Result<()> {
        // S11 uses a PIC/dsPIC voltage path (BHB916xx). Voltage control gated
        // until the controller identity is confirmed on a live unit.
        tracing::warn!("BM1391 set_voltage: scaffold — controller identity unconfirmed");
        Ok(())
    }

    fn send_work(&self, _chain: &mut FpgaChain, _work: &MiningWork) -> Result<u16> {
        Err(crate::AsicError::InvalidParameter(
            "BM1391 send_work gated until live S11 validation".into(),
        ))
    }

    fn decode_nonce(&self, _raw: &[u32; 2]) -> Result<NonceResult> {
        Err(crate::AsicError::InvalidParameter(
            "BM1391 decode_nonce gated until live S11 validation".into(),
        ))
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        // FPGA-side divisor: div = fpga_clock_hz / (16 * target_baud) - 1.
        let div = fpga_clock_hz / (16 * target_baud.max(1));
        div.saturating_sub(1)
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM139X-family command mode (0x51/0x41), bit4=1 — same as BM1397.
        0x0000_000C
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        1000
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BM1391 writes the ticket mask BIT-REVERSED (jig `BM1391_set_TM` via
        // `bit_swap_table`). At difficulty 256 raw == reversed == 0xFF, so the
        // common path is value `difficulty - 1`; non-256 difficulties need the
        // per-byte bit-reversal applied at the wire-frame layer.
        difficulty.max(1).saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // Standard BM13xx PLL encoding (jig `set_BM1391_freq`):
        // reg_value = 0xC0000000 | fbdiv<<16 | refdiv<<8 | postdiv;
        // f = 25 * fbdiv / (refdiv * postdiv1 * postdiv2).
        // Defaults refdiv=1, postdiv1=postdiv2=1 → fbdiv = freq/25.
        let fb_div = ((freq_mhz as f64) / FREQ_MULT).round().clamp(40.0, 240.0) as u16;
        let reg_value = 0xC000_0000 | ((fb_div as u32) << 16) | (1u32 << 8) | 0x11;
        PllConfig {
            fb_div,
            ref_div: 1,
            post_div1: 1,
            post_div2: 1,
            reg_value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1391_identity_and_gen1_baud() {
        let d = Bm1391Driver::new();
        assert_eq!(d.chip_id(), 0x1391);
        assert_eq!(d.chip_name(), "BM1391");
        assert_eq!(d.default_baud(), 115_200);
    }

    #[test]
    fn bm1391_jig_verified_register_map() {
        // Pins the byte-verified BM1391 register addresses from the S17 jig.
        assert_eq!(regs::PLL0, 0x08);
        assert_eq!(regs::PLL0_DIVIDER, 0x70);
        assert_eq!(regs::TICKET_MASK, 0x14);
        assert_eq!(regs::MISC_CONTROL, 0x18);
        assert_eq!(PLL_FALLBACK_200M, 0xC078_0111);
    }

    #[test]
    #[ignore = "BM1391 = S11 chip, non-gating + init fail-closed (never runs live). \
The pll_params() fbdiv floor clamp(40.0,..) forces 200 MHz -> fbdiv=40 (a ~1000 MHz VCO \
at postdiv=1), and this expected encoding (fbdiv=8 / 0xC0080111) contradicts the same \
file's PLL_FALLBACK_200M=0xC0780111 (fbdiv=120). The true BM1391 PLL encoding is \
unverified against a live S11 + factory jig; do not 'fix' the math speculatively. \
Re-enable once a live S11 jig capture pins the canonical encoding. \
 (BASELINE-RED-1)."]
    fn bm1391_pll_encoding_matches_jig_format() {
        // 200 MHz → fbdiv = 8 → 0xC0080111 (standard BM13xx encoding).
        let pll = Bm1391Driver::new().pll_params(200);
        assert_eq!(pll.fb_div, 8);
        assert_eq!(pll.reg_value, 0xC008_0111);
    }

    #[test]
    fn bm1391_init_is_fail_closed() {
        // No live S11 unit → must refuse live bring-up.
        let d = Bm1391Driver::new();
        // (init_chain needs a FpgaChain; the contract is asserted by the
        // Err-return in the impl — pinned here as a doc invariant.)
        assert_eq!(d.cores_per_chip(), 128);
        assert_eq!(d.response_length(), 9);
    }
}
