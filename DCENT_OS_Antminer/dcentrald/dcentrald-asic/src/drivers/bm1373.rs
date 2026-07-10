//! BM1373 ASIC driver (Antminer S23 series) — SCAFFOLD (pre-hardware).
//!
//! The BM1373 is the SHA-256 ASIC used in the Bitmain Antminer S23 family.
//! Bitmain has not publicly disclosed the chip name; "BM1373" comes from
//! internal intel (2026-04-14).
//!
//! Status: PRE-HARDWARE SCAFFOLD — DO NOT USE FOR PRODUCTION. `init_chain`,
//! `set_frequency`, and `send_work` are FAIL-CLOSED (return `Err`) — the
//! register facts below are captured so a real bring-up is a validation
//! exercise, not an RE one, but NOTHING here has been proven on live S23
//! silicon and no method drives hardware today.
//!
//! ## Lineage — BM1373 is a BM1370 descendant
//! The shufps/ESP-Miner-NerdQAxePlus bring-up declares `class BM1373 : public
//! BM1370`, so the S23 chip is a BM1370 (S21 Pro) derivative and this driver
//! mirrors `drivers/bm1370.rs` structure/protocol where the RE doesn't diverge.
//!
//! ## ⚠️ Enumeration chip-id — 0x1372 vs 0x1373 (DUAL-KEYED per operator decision)
//! NerdQAxePlus reads `chip_id[6] = {0xAA,0x55,0x13,0x72,0x00,0x00}` on the
//! chip-address register — i.e. the silicon SELF-REPORTS **`0x1372`** on
//! enumeration, NOT `0x1373`. DCENT keys this scaffold's canonical id on
//! `0x1373` ([`CHIP_ID`]) and records the enumerated value as [`ENUM_CHIP_ID`]
//! (`0x1372`). **Operator decision 2026-07-08: key the scaffold under BOTH ids**
//! (both resolve to this driver + the S23 profile in `drivers::mod`) until a
//! live S23 confirms which is real. This stays FAIL-CLOSED + double-env-gated
//! (scaffold registry only — both ids fail closed for a real production detect;
//! `init_chain` refuses live bring-up regardless of the key). The 0x1372≠0x1373
//! discrepancy is still regression-pinned so the two ids can't be silently
//! collapsed into one before live silicon resolves it.
//!
//! ## Core counts (NerdQAxePlus early bring-up, corrected down)
//!   - 128 big cores/chip ([`CORES_PER_CHIP`])
//!   - 6860 small (nonce-attribution) cores/chip ([`SMALL_CORE_COUNT`],
//!     their `BM1373_SMALL_CORE_COUNT`, corrected from 7000)
//!
//! ## Address interval = 16 ([`ADDR_INTERVAL`]) — their latest; the older
//! 8 / `256/next_pow2` schemes were removed.
//!
//! ## nonceToAsic override = `(bswap32(nonce) >> 24) & 0x03` ([`nonce_to_asic`]),
//! marked TODO in NerdQAxePlus: verify with different chip counts.
//!
//! ## Init register sequence (NerdQAxePlus, clean-room from register facts)
//! Broadcast unless noted (CMD_WRITE_ALL); per-chip = CMD_WRITE_SINGLE:
//!   1. version-rolling mask PRE-ENUMERATE, mask-only WITHOUT the enable bit:
//!      reg 0xA4 = `0x8000FFFF` (NOT `0x9000…`), written 4× before `count_asics`
//!      + 1× after
//!   2. 0xA8 = `0x00070000`
//!   3. MiscCtrl 0x18 = `0xFF00C100`
//!   4. chain-inactive
//!   5. set chip addresses (interval 16)
//!   6. CoreRegCtrl 0x3C = `0x8000800C`
//!   7. job-difficulty-mask (0x14)
//!   8. IO-driver 0x58 = `0x00011111`
//!   9. PLL3 0x68 = `0x5AA55AA5`
//!   10. per-chip (WRITE_SINGLE) over each addr: 0xA8=`0x000701F0`,
//!       0x18=`0xFF00C100`, 0x3C=`0x8000800C`, 0x3C=`0x800082AA`
//!   11. Analog-Mux 0x54 = `0x00000002`
//! The newer commit DELETED the redundant `0xB9=0x00004480` writes + the
//! duplicate version-rolling writes BM1370 carries — so BM1373 does NOT write
//! 0xB9 and does NOT use BM1370's `0x80008B00` core-reg value.
//!
//! Source (GPL-3.0, clean-room — register facts / protocol only, no C++ copied):
//!   - shufps/ESP-Miner-NerdQAxePlus commit `67dc677a` ("new bm1373 init
//!     sequence", newer) + `36124e1e` ("some fixes")
//!   - These are EARLY bring-up values (NerdQAxePlus marks several TODO) →
//!     treat as "NerdQAxePlus early bring-up, needs live S23 verification",
//!     NOT final.
//!   - DCENT `drivers/bm1370.rs` (verified BM1370 predecessor structure)
//!

use crate::drivers::{ChipDriver, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::FpgaChain;

/// BM1373 chip ID used by DCENT's production auto-detect + this scaffold.
///
/// ⚠️ See [`ENUM_CHIP_ID`]: real S23 silicon self-reports **0x1372** on
/// enumeration per NerdQAxePlus RE. DCENT keys 0x1373. Do NOT change the
/// production detect ID without resolving the discrepancy on a live S23.
pub const CHIP_ID: u16 = 0x1373;

/// The chip-address value BM1373 silicon actually reports on enumeration:
/// `{0xAA,0x55,0x13,0x72,0x00,0x00}` → **0x1372** (NerdQAxePlus `chip_id[6]`,
/// commit `67dc677a`). Per operator decision 2026-07-08 the scaffold registry
/// keys the BM1373 driver + the S23 profile under BOTH this id AND [`CHIP_ID`]
/// (0x1373), so both resolve to the same fail-closed scaffold until a live S23
/// confirms which is real (see `drivers::mod::ChipRegistry::register_alias`,
/// `is_scaffold_driver_chip`, and `MinerProfile::for_chip`). Both ids still fail
/// closed for a real production detect (scaffold-only).
pub const ENUM_CHIP_ID: u16 = 0x1372;

/// BM1373 default chips per chain (PROJECTED from S23 specs — verify on hardware).
/// S23 air: 318 TH/s, 3 boards. Estimated 80-100 chips per chain.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 90; // PLACEHOLDER — verify on hardware

/// BM1373 response size: 11 bytes (same as BM1366/BM1368/BM1370).
pub const RESPONSE_BYTES: usize = 11;

/// Number of BIG SHA-256 cores per BM1373 chip.
/// NerdQAxePlus early bring-up: 128 big cores/chip. Needs live S23 verify.
const CORES_PER_CHIP: u32 = 128;

/// Small-core (nonce-attribution) count per BM1373 chip: **6860**
/// (NerdQAxePlus `BM1373_SMALL_CORE_COUNT`, corrected down from 7000, commit
/// `36124e1e`). Used for nonce-attribution / hashrate math, NOT engine-init.
/// Needs live S23 verify.
pub const SMALL_CORE_COUNT: u32 = 6860;

/// Chip-address interval used during enumeration/address assignment: **16**
/// (NerdQAxePlus latest; the older 8 / `256/next_pow2` schemes were removed).
pub const ADDR_INTERVAL: u16 = 16;

/// Job ID increment step (PROJECTED from BM1368/BM1370 pattern).
const JOB_ID_STEP: u8 = 24;

/// Job ID modulus.
const JOB_ID_MOD: u8 = 128;

/// Full header work size in 32-bit words (same as BM1370).
const WORK_WORDS: usize = 21;

/// Crystal oscillator reference frequency (MHz).
const FREQ_MULT: f64 = 25.0;

/// BM1373 FB_DIV minimum (PROJECTED — BM1370 uses 160, may be wider).
const FB_DIV_MIN: u16 = 160;

/// BM1373 FB_DIV maximum (PROJECTED — BM1370 uses 239, may be wider).
const FB_DIV_MAX: u16 = 250; // PROJECTED wider range for higher clocks

/// Frequency ramp step size (MHz) — same as BM1370.
const FREQ_RAMP_STEP: f64 = 6.25;

/// Frequency ramp start (MHz).
const FREQ_RAMP_START: f64 = 56.25;

/// Delay between frequency ramp steps (ms).
const FREQ_RAMP_DELAY_MS: u64 = 100;

// ---------------------------------------------------------------------------
// BM1373 register addresses (BM1370-family; the init VALUES below are the
// NerdQAxePlus RE facts, verify on hardware)
// ---------------------------------------------------------------------------

/// BM1373 register addresses (BM1370-family conserved map).
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    /// ⚠️ Real silicon reports `0x1372` here on enumeration (see
    /// [`super::ENUM_CHIP_ID`]).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 parameter register (hash clock PLL).
    pub const PLL0: u8 = 0x08;
    /// Hash counting number register (nonce range / chip distribution).
    pub const HASH_COUNTING: u8 = 0x10;
    /// Ticket mask register (hardware difficulty filter / job-difficulty-mask).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc control register (baud, clock config).
    pub const MISC_CONTROL: u8 = 0x18;
    /// Fast UART configuration register (baud rate for BM1366+).
    pub const FAST_UART: u8 = 0x28;
    /// UART relay register (multi-chip relay).
    pub const UART_RELAY: u8 = 0x2C;
    /// Core register control (indirect core access).
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// Nonce error counter.
    pub const NONCE_ERROR: u8 = 0x4C;
    /// Analog mux control (temperature diode).
    pub const ANALOG_MUX: u8 = 0x54;
    /// IO driver strength.
    pub const IO_DRIVER: u8 = 0x58;
    /// PLL1 parameter (baud clock, used via FAST_UART).
    pub const PLL1: u8 = 0x60;
    /// PLL3 parameter (domain config). NerdQAxePlus BM1373 writes `0x5AA55AA5`.
    pub const PLL3: u8 = 0x68;
    /// Version rolling mask register.
    pub const VERSION_ROLLING: u8 = 0xA4;
    /// Init control register (Reg_A8).
    pub const REG_A8: u8 = 0xA8;
    /// BM1370-inherited misc settings register. **NerdQAxePlus BM1373 does NOT
    /// write this** — the newer commit deleted the redundant `0xB9=0x00004480`
    /// writes. Address retained for reference only.
    pub const MISC_SETTINGS_B9: u8 = 0xB9;
}

// ---------------------------------------------------------------------------
// BM1373 register init VALUES — NerdQAxePlus RE (commits 67dc677a + 36124e1e).
// EARLY bring-up; needs live S23 verification. Clean-room: values only.
// ---------------------------------------------------------------------------

/// Version rolling mask PRE-ENUMERATE — reg 0xA4 = `0x8000FFFF`, **mask-only,
/// NO enable bit** (contrast BM1370's `0x9000FFFF` which sets the 0x90 enable).
/// Written 4× before `count_asics` + 1× after.
const VERSION_MASK_PRE_ENUM: u32 = 0x8000_FFFF;

/// Reg_A8 broadcast init value (0xA8 = `0x00070000`).
const REG_A8_BCAST: u32 = 0x0007_0000;

/// Reg_A8 per-chip init value (0xA8 = `0x000701F0`).
const REG_A8_PER_CHIP: u32 = 0x0007_01F0;

/// Misc Control value (0x18 = `0xFF00C100`, broadcast and per-chip). NOTE the
/// high byte is `0xFF` on BM1373 vs `0xF0` on the BM1370 S21 Pro.
const MISC_CTRL_VALUE: u32 = 0xFF00_C100;

/// Core Register Control clock-order write (0x3C = `0x8000800C`, broadcast and
/// per-chip). NerdQAxePlus BM1373 does NOT use BM1370's `0x80008B00`.
const CORE_REG_CLK_ORDER: u32 = 0x8000_800C;

/// Core Register Control AsicBoost/version-rolling enable (0x3C = `0x800082AA`,
/// per-chip only), common to all BM1366+.
const CORE_REG_ASICBOOST: u32 = 0x8000_82AA;

/// IO Driver Strength (0x58 = `0x00011111`).
const IO_DRIVER_VALUE: u32 = 0x0001_1111;

/// PLL3 domain config (0x68 = `0x5AA55AA5`) — NerdQAxePlus BM1373-specific.
const PLL3_VALUE: u32 = 0x5AA5_5AA5;

/// Analog Mux Control (0x54 = `0x00000002`).
const ANALOG_MUX_VALUE: u32 = 0x0000_0002;

/// Hash Counting Number (PLACEHOLDER — will depend on actual chip count;
/// NerdQAxePlus does not pin a broadcast value for the S23 topology yet).
const HASH_COUNTING_VALUE: u32 = 0x0000_1EB5; // PLACEHOLDER

/// Fast UART register value for 1 Mbps baud (PROJECTED from BM1370).
const FAST_UART_1M: u32 = 0x1130_0200;

/// Ticket mask difficulty (PLACEHOLDER — may be 128 like BM1368 instead of 256).
const DEFAULT_TICKET_DIFFICULTY: u32 = 256; // TODO: verify — could be 128

// ---------------------------------------------------------------------------
// nonce → ASIC-index attribution
// ---------------------------------------------------------------------------

/// BM1373 `nonceToAsic` override from NerdQAxePlus (`BM1373 : public BM1370`,
/// commit `67dc677a`): `(bswap32(nonce) >> 24) & 0x03`.
///
/// TODO(live-S23): NerdQAxePlus marks this "verify with different chip counts"
/// — the `& 0x03` (2-bit) span fits their small NerdQAxe topology and may not
/// match a full S23 hashboard. Do not rely on it for production attribution
/// until validated on a live S23.
pub fn nonce_to_asic(nonce: u32) -> u8 {
    ((nonce.swap_bytes() >> 24) & 0x03) as u8
}

// ---------------------------------------------------------------------------
// PLL helpers
// ---------------------------------------------------------------------------

/// Discrete frequency table for BM1373 (PROJECTED from BM1370 with wider range).
/// Steps of 6.25 MHz from 56.25 to the max.
/// TODO: Replace with actual PLL table from hardware verification.
static PLL_FREQ_TABLE: &[u16] = &[
    56, 63, 69, 75, 81, 88, 94, 100, 106, 113, 119, 125, 131, 138, 144, 150, 156, 163, 169, 175,
    181, 188, 194, 200, 206, 213, 219, 225, 231, 238, 244, 250, 256, 263, 269, 275, 281, 288, 294,
    300, 306, 313, 319, 325, 331, 338, 344, 350, 356, 363, 369, 375, 381, 388, 394, 400, 406, 413,
    419, 425, 431, 438, 444, 450, 456, 463, 469, 475, 481, 488, 494, 500, 506, 513, 519, 525, 531,
    538, 544, 550, 556, 563, 569, 575, 581, 588, 594, 600, 606, 613, 619, 625,
];

/// Return the discrete PLL frequency table.
pub fn pll_frequencies() -> &'static [u16] {
    PLL_FREQ_TABLE
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// BM1373 driver — SCAFFOLD (pre-hardware).
///
/// BM1370-derived (`BM1373 : public BM1370`). Register VALUES are the
/// NerdQAxePlus early bring-up RE facts; ALL init sequences MUST be verified on
/// live S23 hardware before this driver can drive production mining. The
/// hardware-touching methods are fail-closed until then.
pub struct Bm1373Driver;

impl Default for Bm1373Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1373Driver {
    pub fn new() -> Self {
        Self
    }
}

impl ChipDriver for Bm1373Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1373"
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
        1_000_000 // PROJECTED — may be higher on BM1373
    }

    fn init_chain(&self, _chain: &mut FpgaChain, _chip_count: u8, _freq_mhz: u16) -> Result<()> {
        // FAIL-CLOSED. The NerdQAxePlus RE init sequence (BM1370-family) is:
        //   0xA4=0x8000FFFF (mask-only, WITHOUT enable) ×4 → count_asics →
        //   0xA4=0x8000FFFF ×1 → 0xA8=0x00070000 → 0x18=0xFF00C100 →
        //   chain-inactive → set addresses (interval 16) → 0x3C=0x8000800C →
        //   ticket/job-difficulty-mask (0x14) → 0x58=0x00011111 →
        //   0x68=0x5AA55AA5 → per-chip{0xA8=0x000701F0, 0x18=0xFF00C100,
        //   0x3C=0x8000800C, 0x3C=0x800082AA} → 0x54=0x00000002.
        // NO 0xB9 write and NO 0x80008B00 (deleted in commit 36124e1e).
        // These are EARLY bring-up values needing live S23 verification, so the
        // scaffold refuses to drive hardware.
        tracing::warn!(
            "BM1373 init_chain: SCAFFOLD — NerdQAxePlus early bring-up register facts \
             captured but NOT live-verified on S23. Refusing live bring-up."
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1373 driver is a pre-hardware scaffold (NerdQAxePlus early bring-up values, \
             needs live S23 verification). Cannot init."
                .into(),
        ))
    }

    fn set_frequency(&self, _chain: &mut FpgaChain, _chip_addr: u8, _freq_mhz: u16) -> Result<()> {
        // FAIL-CLOSED until PLL parameters verified on live S23 hardware.
        tracing::warn!("BM1373 set_frequency: SCAFFOLD — not implemented");
        Err(crate::AsicError::InvalidParameter(
            "BM1373 set_frequency gated until live S23 verification".into(),
        ))
    }

    fn set_voltage(&self, _pic: &mut PicController, _voltage_mv: u16) -> Result<()> {
        // BM1373 is NoPic (BM1368/BM1370 lineage) — PIC controller not applicable.
        // Voltage will be controlled via I2C DAC in the platform HAL.
        tracing::warn!("BM1373 set_voltage: NoPic model — voltage via I2C DAC, not PIC");
        Ok(()) // No-op for PIC path; real voltage control is in the platform HAL
    }

    fn send_work(&self, _chain: &mut FpgaChain, _work: &MiningWork) -> Result<u16> {
        // Full header format, same as BM1370. FAIL-CLOSED until live-verified.
        tracing::warn!("BM1373 send_work: SCAFFOLD — not implemented");
        Err(crate::AsicError::InvalidParameter(
            "BM1373 send_work gated until live S23 verification".into(),
        ))
    }

    fn decode_nonce(&self, _raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1370-family 11-byte response expected; the BM1373-specific
        // nonce→ASIC attribution is captured in [`nonce_to_asic`]
        // (`(bswap32(nonce) >> 24) & 0x03`). FAIL-CLOSED until live-verified.
        tracing::warn!("BM1373 decode_nonce: SCAFFOLD — see nonce_to_asic() for the RE override");
        Err(crate::AsicError::InvalidParameter(
            "BM1373 decode_nonce gated until live S23 verification".into(),
        ))
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        // Same formula as BM1370: div = fpga_clock_hz / (16 * target_baud) - 1
        let div = fpga_clock_hz / (16 * target_baud.max(1));
        div.saturating_sub(1)
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM139X mode (bit4=1), same as BM1370
        0x0000_000C
    }

    fn job_interval_ms(&self, chip_count: u8, _freq_mhz: u16) -> u32 {
        // Mirror BM1370: interval = 500ms / asic_count (min 1ms).
        let interval = 500u32 / (chip_count as u32).max(1);
        interval.max(1)
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // Same as BM1366/BM1368/BM1370: (difficulty - 1).
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // PROJECTED from BM1370 PLL formula (fb_div = freq / 25, dividers 1).
        // Not live-verified — set_frequency is fail-closed. Kept for metadata
        // (verify_frequency / pll table decode).
        let fb_div = ((freq_mhz as f64) / FREQ_MULT).round() as u16;
        let fb_div = fb_div.clamp(FB_DIV_MIN, FB_DIV_MAX);
        let reg_value = (fb_div as u32) << 16;

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
    fn bm1373_identity() {
        let d = Bm1373Driver::new();
        assert_eq!(d.chip_id(), 0x1373);
        assert_eq!(d.chip_name(), "BM1373");
        assert_eq!(d.response_length(), 11);
        assert_eq!(d.default_baud(), 115_200);
    }

    /// Pins the 0x1372-vs-0x1373 discrepancy. Real BM1373 silicon self-reports
    /// 0x1372 on enumeration; DCENT's canonical key is 0x1373. Per operator
    /// decision 2026-07-08 the scaffold is keyed under BOTH ids (dual-key
    /// wiring is pinned in `drivers::mod`), but the two id VALUES must stay
    /// distinct so they can't be silently collapsed before a live S23 resolves
    /// which is real.
    #[test]
    fn bm1373_enumeration_id_discrepancy_is_pinned() {
        assert_eq!(
            ENUM_CHIP_ID, 0x1372,
            "NerdQAxePlus chip_id[6] = {{0xAA,0x55,0x13,0x72,..}} → enumeration reports 0x1372"
        );
        assert_eq!(CHIP_ID, 0x1373, "DCENT canonical scaffold key");
        assert_ne!(
            ENUM_CHIP_ID, CHIP_ID,
            "0x1372 (enumerated) vs 0x1373 (canonical) stay distinct — dual-keyed for now, \
             live S23 confirms which is real"
        );
    }

    #[test]
    fn bm1373_core_counts_match_nerdqaxeplus() {
        // NerdQAxePlus early bring-up (commits 67dc677a + 36124e1e).
        assert_eq!(CORES_PER_CHIP, 128, "128 big cores/chip");
        assert_eq!(
            SMALL_CORE_COUNT, 6860,
            "6860 small cores (corrected from 7000)"
        );
        assert_eq!(
            ADDR_INTERVAL, 16,
            "address interval 16 (older 8 / 256/pow2 removed)"
        );
        // Driver-facing engine count mirrors the big-core count.
        assert_eq!(Bm1373Driver::new().cores_per_chip(), 128);
    }

    #[test]
    fn bm1373_nerdqaxeplus_register_values() {
        // Clean-room register facts (values only) from commits 67dc677a + 36124e1e.
        assert_eq!(VERSION_MASK_PRE_ENUM, 0x8000_FFFF); // 0xA4 mask-only, NO enable bit
        assert_ne!(
            VERSION_MASK_PRE_ENUM, 0x9000_FFFF,
            "BM1373 pre-enum mask must NOT set the 0x90 enable bit (that's BM1370)"
        );
        assert_eq!(REG_A8_BCAST, 0x0007_0000); // 0xA8
        assert_eq!(REG_A8_PER_CHIP, 0x0007_01F0); // 0xA8 per-chip
        assert_eq!(MISC_CTRL_VALUE, 0xFF00_C100); // 0x18 (FF high byte, vs BM1370 F0)
        assert_eq!(CORE_REG_CLK_ORDER, 0x8000_800C); // 0x3C
        assert_eq!(CORE_REG_ASICBOOST, 0x8000_82AA); // 0x3C per-chip
        assert_eq!(IO_DRIVER_VALUE, 0x0001_1111); // 0x58
        assert_eq!(PLL3_VALUE, 0x5AA5_5AA5); // 0x68
        assert_eq!(ANALOG_MUX_VALUE, 0x0000_0002); // 0x54
                                                   // Register address map (BM1370-family).
        assert_eq!(regs::VERSION_ROLLING, 0xA4);
        assert_eq!(regs::REG_A8, 0xA8);
        assert_eq!(regs::MISC_CONTROL, 0x18);
        assert_eq!(regs::CORE_REG_CTRL, 0x3C);
        assert_eq!(regs::IO_DRIVER, 0x58);
        assert_eq!(regs::PLL3, 0x68);
        assert_eq!(regs::ANALOG_MUX, 0x54);
    }

    #[test]
    fn bm1373_nonce_to_asic_matches_re_override() {
        // (bswap32(nonce) >> 24) & 0x03
        assert_eq!(nonce_to_asic(0x0000_0000), 0);
        assert_eq!(nonce_to_asic(0x0000_0001), 1);
        assert_eq!(nonce_to_asic(0x0000_0002), 2);
        assert_eq!(nonce_to_asic(0x0000_00FF), 3); // low byte 0xFF & 0x03 = 3
        assert_eq!(nonce_to_asic(0x1234_5600), 0); // low byte 0x00
    }

    /// Scaffold contract: the hardware-touching methods are fail-closed. The
    /// `Err` returns are asserted by the impl; this pins the metadata that a
    /// scaffold must keep exposing (cores/response) so it can't drift.
    #[test]
    fn bm1373_is_fail_closed_scaffold() {
        let d = Bm1373Driver::new();
        assert_eq!(d.cores_per_chip(), 128);
        assert_eq!(d.response_length(), 11);
    }
}
