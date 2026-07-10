//! BM1366 silicon characterization table (Antminer S19k Pro / S19 XP â€”
//! third-generation BM136x family). The BM1366 ships on **multiple
//! control-board carriers** â€” Zynq ("xil"), Amlogic A113D ("aml"), and
//! Cvitek CV1835 ("cv") â€” NOT Amlogic-exclusive. "NoPic" (no PIC voltage
//! controller) is a voltage-architecture property of the BHB56xxx
//! hashboard, independent of the control-board carrier. See PR-057 /
//! R11-13:
//!
//! (corpus-confirmed Zynq + Amlogic + CVitek; regression-pinned by
//! `pr057_bm1366_carrier_is_multi_carrier_not_amlogic_only`).
//!
//! 5 discrete steps from `-2` (eco-low) to `+2` (overclock). Source
//! provenance:
//! - **`mining-bible-v1/_canonical/chip-init-sequences.md` lines 175-200**
//!   â€” BM1366 register-init sequence: `cores_per_chip = 894`, op baud
//!   1 Mbaud (ESP-Miner) or 3.125 Mbaud (Bitmain), HASHCOUNTING
//!   `0x0000115A = 76 chips/chain` (S19k Pro stock).
//! - **`baud-switching-analysis.md` line 36** â€” BM1366 op baud
//!   3.125 Mbaud (Bitmain), MiscCtrl `@ 0x28 = 0x00003001`.
//! - **S19k Pro nameplate**: ~120 TH/s @ ~3,420 W (â‰ˆ 28.5 W/TH) with
//!   3 chains Ã— 76 chips = 228 chips total.
//! - **Reconstructed**: linear extrapolation; live cgminer-API capture
//!   from an S19k Pro running BraiinsOS+ is queued for re-verification.
//!
//! Sweet spot at Step -2 (~2,950 W / 109 TH/s â‰ˆ 27.1 J/TH) â€” slightly
//! better than nameplate.
//!
//! ## Harvest cross-reference (2026-06-14) — VNish measured curve is leaner
//! than the reconstructed nameplate watts here.
//!
//! The crate nameplate above (120 TH @ ~3420 W = 28.5 J/TH) is
//! `Reconstructed` and **over-states wall power ~10-24%** vs the operator's
//! live VNish RE: POWER_PROFILES_CATALOG §2.6 puts stock at 120 TH @ ~2760 W
//! (23 J/TH) and the entire VNish Normal curve at a flat ~25.8 J/TH (so
//! 120 TH ≈ 3120 W). Using the 3420 W reconstructed value over-estimates
//! breaker headroom. These step rows are intentionally **left unchanged**
//! (autotuner step ladder + pinned tests); the authoritative per-MODEL
//! S19k Pro watt curve (34 VNish rows + the live .78 BHB56902 measured point)
//! now lives in [`crate::operating_points::S19K_PRO`] as `VendorExtracted` /
//! `Measured`. Power-estimate consumers should prefer those rows. The live
//! .78 point (670 MHz / 13.90 V / 46.12 TH/board) is the only MEASURED
//! freq/voltage/hashrate anchor; its watts are still inferred (no wall
//! meter) → a wattmeter A/B on a live S19k Pro remains the missing anchor.
//!
//! BM1366 industrial hashboards (BHB56xxx) are NoPic (no PIC voltage
//! controller; `"Has_Pic": false` per the S19k Pro fixture `Config.ini`).
//! This is independent of the carrier â€” the same SKU is RE'd on both
//! Zynq and Amlogic control boards (PR-057 §2). Voltage is set by
//! LDO/op-amp; the `voltage_v` here is the chain-rail voltage as
//! commanded by the autotune target (post open-core trim from 14.8V
//! overshoot) â€” carrier-independent.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 5 BM1366 silicon profile rows, ordered by `step`.
///
/// Voltage column is chain-rail voltage in volts. Hashrate column is
/// in TH/s summed across 3 boards Ã— 76 chips.
pub const BM1366_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 540,
        voltage_v: 13.4,
        wall_watts: Some(2950),
        hashrate_ths: Some(109.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 575,
        voltage_v: 13.6,
        wall_watts: Some(3180),
        hashrate_ths: Some(114.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 605,
        voltage_v: 13.8,
        wall_watts: Some(3420),
        hashrate_ths: Some(120.0), // S19k Pro nameplate
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 645,
        voltage_v: 14.0,
        wall_watts: Some(3700),
        hashrate_ths: Some(127.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 685,
        voltage_v: 14.2,
        wall_watts: Some(4000),
        hashrate_ths: Some(134.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1366 silicon table. Default = nameplate (Step 0).
/// Sweet spot at Step -2 (~2,950 W / 109 TH/s â‰ˆ 27.1 J/TH).
pub const BM1366_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1366",
    profiles: &BM1366_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    // BitAxe Hex Supra first share 2026-03-19 (4.4 TH/s); used in
    // Antminer S19 XP / S19k Pro production builds.
    live_status: crate::ChipStatus::LiveConfirmed,
};

/// Per-chip `cores_per_chip` per chip-init-sequences.md.
pub const BM1366_CORES_PER_CHIP: u32 = 894;

/// Standard Antminer S19k Pro chips per chain (76).
pub const BM1366_CHIPS_PER_CHAIN_S19K_PRO: u32 = 76;

/// Standard S19k Pro chain count (3).
pub const BM1366_CHAIN_COUNT_S19K_PRO: u32 = 3;

/// Operational baud after baud-upgrade (Bitmain firmware path).
pub const BM1366_OPERATIONAL_BAUD: u32 = 3_125_000;

/// Canonical MiscCtrl value to write at register 0x28 to upgrade to
/// the Bitmain operational baud (3.125 Mbaud).
pub const BM1366_MISCCTRL_BAUD_VALUE: u32 = 0x0000_3001;

/// Alternate ESP-Miner-style 1-Mbaud value at register 0x28.
pub const BM1366_MISCCTRL_BAUD_VALUE_1MBAUD: u32 = 0x1130_0200;

/// HASHCOUNTING register value for stock S19k Pro (76 chips/chain).
/// Per chip-init-sequences.md line 197.
pub const BM1366_HASHCOUNTING_S19K_PRO_STOCK: u32 = 0x0000_115A;

// ===========================================================================
// PR-053 (2026-05-16): BM1366 depth-parity with the BM1362 reference module
// (W11.5 geometry / W12.4 algorithmic PLL). Every constant below is sourced
// from the in-repo RE corpus — NO fabricated values:
//   -  §5
//     (BM1366) + §13 cheat-sheet + §6.3 cores + §14 coverage table.
//
//     "## BM1366 (S19k Pro / S19 XP)" SPEC BLOCK (chip ID, cores, stride,
//     baud, HASHCOUNTING) + family-preamble / PLL / MISCCTRL steps.
//   -  §1 chip
//     table (BM1366 ≈ 21.5 J/TH nameplate) + §2.2 voltage-domain table.
//   - Driver register source-of-truth `dcentrald-asic/src/drivers/bm1366.rs`
//     (CHIP_ID, FastUART, response bytes, ChipAddress reset value).
// Provenance discipline mirrors `bm1362.rs` exactly: every numeric constant
// carries its in-repo cite and is pinned by a unit test below.
// ===========================================================================

/// BM1366 chip-family fixed constants. These are die-fixed across every
/// BM1366 hashboard SKU (S19k Pro / S19 XP / S19j XP / BitAxe Hex Supra).
///
/// Mirrors the `bm1362::chip` module idiom (CHIP_ID / CRC / stride /
/// cores) so downstream consumers can dispatch uniformly across the
/// BM136x family.
pub mod chip {
    /// CHIP_ID reported in `ChipAddress` bits [31:16] during enumeration.
    /// Source: `chip-init-sequences.md` BM1366 SPEC BLOCK
    /// (`Chip ID: 0x1366`) + driver `bm1366.rs:41` (`CHIP_ID: u16 =
    /// 0x1366`) + PLL bible §5.1 ChipAddress reset `0x13660000`.
    pub const CHIP_ID: u16 = 0x1366;

    /// Process node — Bitmain/TSMC 5 nm (PLL bible §5 header:
    /// "Gen 4, TSMC 5nm"). Same node as BM1362.
    pub const PROCESS_NM: u8 = 5;

    /// CRC5 polynomial used for BM136x-family command framing.
    /// Source: `DCENT_OS_Antminer/ "ASIC Chip Communication
    /// Protocol" ("CRC: CRC5 (poly 0x05, init 0x1F) for commands") —
    /// the BM139x+ unified command family (PLL bible §13 "Cmd headers
    /// (broadcast) … 0x51/0x52/0x53 (BM139x+ unified)") shares this
    /// command CRC across BM1362/1366/1368.
    pub const CRC5_CMD_POLY: u8 = 0x05;

    /// CRC5 command-CRC init value (`DCENT_OS_Antminer/,
    /// "CRC5 (poly 0x05, init 0x1F)").
    pub const CRC5_CMD_INIT: u8 = 0x1F;

    /// Per-chip core count. Source: `chip-init-sequences.md` BM1366
    /// SPEC BLOCK (`Cores per chip: 894`) + PLL bible §13 cheat-sheet
    /// ("894 (BM1366 small)"). NOTE: PLL bible §4.6 records that the
    /// BM1362 driver was *wrongly* given this 894 value once — 894 is
    /// the genuine BM1366 small-core count, not a BM1362 number.
    pub const CORES_PER_CHIP: u32 = 894;

    /// Address stride mask between sequential chips on a chain.
    /// Source: `chip-init-sequences.md` BM1366 SPEC BLOCK
    /// ("Address stride: 256 / N (BM1366: 0xF8 mask, JOBID step +8)").
    /// `0xF8` ⇒ upper-5-bit chip-address field; the job-id rolls by 8.
    pub const JOBID_STEP: u8 = 8;

    /// Chip-address field mask (upper 5 bits) per the BM1366 SPEC BLOCK
    /// `0xF8 mask`. Used when extracting chip index from a nonce frame
    /// (driver `bm1366.rs:721`: `job_id = byte7 & 0xF8`).
    pub const CHIP_ADDR_MASK: u8 = 0xF8;

    /// Hardware difficulty for BM1366. Source: PLL bible §13 cheat-sheet
    /// ("Hardware difficulty … 256 (BM1362/1366)").
    pub const HARDWARE_DIFFICULTY: u32 = 256;

    /// Open-core requirement. Source: `chip-init-sequences.md` BM1366
    /// SPEC BLOCK ("Open-core: NOT required") + PLL bible §13
    /// ("Open-core needed … NO"). The BM136x family activates cores
    /// without the BM1387-style 114-dummy-work sweep.
    pub const OPEN_CORE_REQUIRED: bool = false;
}

/// BM1366 wire-format constants (response framing + canonical register
/// addresses). Byte counts and register addresses do NOT vary per
/// hashboard SKU. Mirrors `bm1362::work_layout`.
pub mod work_layout {
    /// Reverse-frame (nonce) length in bytes. Source:
    /// `chip-init-sequences.md` BM1366 SPEC BLOCK (`Response bytes: 11`)
    /// + driver `bm1366.rs:49` (`RESPONSE_BYTES: usize = 11`).
    pub const RESPONSE_BYTES: usize = 11;

    /// PLL0 (hash-clock) register address. Source: PLL bible §13
    /// cheat-sheet ("PLL register … `0x08` (byte-segmented …)") — the
    /// BM1362/1366/1368/1370 family all place the hash-clock PLL at
    /// `0x08` (vs BM1387's `0x0c`).
    pub const PLL0_REG: u8 = 0x08;

    /// MiscControl register address. Source: PLL bible §13 cheat-sheet
    /// ("MiscCtrl register … `0x18`" for the BM1362/1366/1368 column).
    pub const MISC_CTRL_REG: u8 = 0x18;

    /// FastUART / baud-config register. Source: `chip-init-sequences.md`
    /// BM1366 step 8 ("reg 0x28") + PLL bible §5 header ("Fast UART
    /// config `0x28 = 0x00003001`").
    pub const FAST_UART_REG: u8 = 0x28;

    /// HASHCOUNTING register. Source: `chip-init-sequences.md` BM1366
    /// step 11 ("register 0x10 — value depends on chip count").
    pub const HASHCOUNTING_REG: u8 = 0x10;
}

/// One row of the per-SKU freq/voltage table — frequency in MHz,
/// chip-rail voltage in millivolts. Same shape as
/// [`crate::bm1362::Bm1362FreqVoltRow`].
pub type Bm1366FreqVoltRow = (u16, u16);

/// S19k Pro standard freq/voltage levels.
///
/// Source: `chip-init-sequences.md` BM1366 nameplate (S19k Pro ~120
/// TH/s @ ~3,420 W) + PLL bible §5.1 default factory frequency
/// **400 MHz** / default chain voltage **14.20 V** + the
/// `BM1366_PROFILES` step table above (the canonical operating points
/// this profile module already pins; each row here mirrors a
/// `BM1366_PROFILES` (freq, voltage) pair, mV ⇄ V).
///
/// Voltages are **chain-rail millivolts** (whole-board), matching the
/// `BM1366_PROFILES.voltage_v` units — NOT chip-rail mV like the
/// BHB42xxx tables in `bm1362.rs`. Whole-board cadence is therefore
/// `freq↑ ⟹ voltage↑` (the opposite of the BHB42xxx chip-rail
/// silicon-floor cadence): top of table = highest freq + highest
/// voltage.
pub const BM1366_S19K_PRO_FREQ_VOLT_TABLE: &[Bm1366FreqVoltRow] = &[
    (685, 14200),
    (645, 14000),
    (605, 13800),
    (575, 13600),
    (540, 13400),
];

/// S19 XP higher-grade freq/voltage levels (HASHCOUNTING tuned per
/// `chip-init-sequences.md` BM1366 step 11: "S19XP Luxos: 0x00001446
/// (tuned)", "S19XP stock: 0x0000151C (110 chips/chain)").
///
/// Source: PLL bible §5.1 default factory frequency **400 MHz** /
/// chain voltage **14.20 V** for the BM1366 S19-XP class; the higher
/// chip count (110 vs 76) is reflected in the SKU geometry, not the
/// rail voltage. This table reuses the S19k Pro operating-point ladder
/// (same BM1366 silicon-floor cadence) — the SKU difference is chip
/// count + HASHCOUNTING, not the freq/voltage envelope.
pub const BM1366_S19_XP_FREQ_VOLT_TABLE: &[Bm1366FreqVoltRow] = BM1366_S19K_PRO_FREQ_VOLT_TABLE;

/// HASHCOUNTING register value for stock S19 XP (110 chips/chain).
/// Source: `chip-init-sequences.md` BM1366 step 11
/// ("S19XP stock: 0x0000151C (110 chips/chain)").
pub const BM1366_HASHCOUNTING_S19XP_STOCK: u32 = 0x0000_151C;

/// HASHCOUNTING register value for LuxOS-tuned S19 XP.
/// Source: `chip-init-sequences.md` BM1366 step 11
/// ("S19XP Luxos: 0x00001446 (tuned)").
pub const BM1366_HASHCOUNTING_S19XP_LUXOS: u32 = 0x0000_1446;

/// Per-hashboard SKU geometry for the BM1366 family. Mirrors the
/// `bm1362::Bm1362HashboardSku` enum idiom (each variant ships its own
/// chip-count + HASHCOUNTING + freq/voltage table).
///
/// Source: `chip-init-sequences.md` BM1366 SPEC BLOCK + step 11
/// HASHCOUNTING variants + PLL bible §5 header model list
/// ("S19 XP / S19j XP / S19k Pro / BitAxe Hex Supra").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bm1366HashboardSku {
    /// **S19k Pro** — 76 chips/chain × 3 chains. Stock HASHCOUNTING
    /// `0x0000115A`. Most common production BM1366 SKU; default
    /// fallback for unrecognised BM1366 boards.
    S19kPro,
    /// **S19 XP** — 110 chips/chain × 3 chains. Stock HASHCOUNTING
    /// `0x0000151C` (LuxOS-tuned `0x00001446`). Higher chip count, same
    /// BM1366 silicon-floor freq/voltage envelope.
    S19Xp,
}

impl Bm1366HashboardSku {
    /// Chips per chain for this SKU. Source: driver `bm1366.rs:25,27`
    /// (`DEFAULT_CHIPS_PER_CHAIN_S19XP = 110`,
    /// `DEFAULT_CHIPS_PER_CHAIN_S19K = 77`) + `chip-init-sequences.md`
    /// BM1366 step 11 (S19k Pro stock = 76 chips/chain, S19 XP stock =
    /// 110 chips/chain). The 76 vs 77 divergence is documented: the
    /// HASHCOUNTING-derived value is 76 (`0x115A`); the driver default
    /// rounds to 77. We pin the HASHCOUNTING-derived 76 here to stay
    /// consistent with [`BM1366_CHIPS_PER_CHAIN_S19K_PRO`].
    pub const fn chips_per_chain(self) -> u8 {
        match self {
            Bm1366HashboardSku::S19kPro => 76,
            Bm1366HashboardSku::S19Xp => 110,
        }
    }

    /// Chain count for this SKU (3 for every BM1366 board — S19k Pro
    /// and S19 XP both ship 3 hashing chains).
    /// Source: `chip-init-sequences.md` + nameplate (3 chains).
    pub const fn chain_count(self) -> u8 {
        3
    }

    /// Stock HASHCOUNTING register value for this SKU.
    /// Source: `chip-init-sequences.md` BM1366 step 11.
    pub const fn hashcounting_stock(self) -> u32 {
        match self {
            Bm1366HashboardSku::S19kPro => BM1366_HASHCOUNTING_S19K_PRO_STOCK,
            Bm1366HashboardSku::S19Xp => BM1366_HASHCOUNTING_S19XP_STOCK,
        }
    }

    /// Per-SKU freq/voltage table (chain-rail mV). Both SKUs share the
    /// BM1366 silicon-floor ladder; the SKU difference is chip-count +
    /// HASHCOUNTING, not the freq/voltage envelope.
    pub const fn freq_voltage_table(self) -> &'static [Bm1366FreqVoltRow] {
        match self {
            Bm1366HashboardSku::S19kPro => BM1366_S19K_PRO_FREQ_VOLT_TABLE,
            Bm1366HashboardSku::S19Xp => BM1366_S19_XP_FREQ_VOLT_TABLE,
        }
    }

    /// Hashboard string identifier (matches `/etc/subtype` / EEPROM
    /// model-name conventions used elsewhere in the workspace).
    pub const fn hashboard_id(self) -> &'static str {
        match self {
            Bm1366HashboardSku::S19kPro => "s19kpro",
            Bm1366HashboardSku::S19Xp => "s19xp",
        }
    }

    /// Default fallback for an unrecognised BM1366 SKU string.
    /// Returns [`Bm1366HashboardSku::S19kPro`] (the most common
    /// production BM1366 board). **DO NOT** treat this as a
    /// "synthesise on missing data" path — callers must be deliberate
    /// about routing an unknown SKU; a wrong chip-count is a
    /// correctness hazard.
    pub const fn default_for_unrecognized_sku() -> Self {
        Bm1366HashboardSku::S19kPro
    }

    /// Look up a SKU by its lower-case ID string. `None` for unknowns.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "s19kpro" => Bm1366HashboardSku::S19kPro,
            "s19xp" => Bm1366HashboardSku::S19Xp,
            _ => return None,
        })
    }
}

/// All BM1366 hashboard SKUs (catalog use / parameterised tests).
pub const ALL_BM1366_HASHBOARD_SKUS: &[Bm1366HashboardSku] =
    &[Bm1366HashboardSku::S19kPro, Bm1366HashboardSku::S19Xp];

/// BM1366 nameplate efficiency from the canonical chip table.
/// Source: `HASHBOARD_DIAGNOSTICS.md` §1 chip table line 58
/// ("BM1366 | … | 21.5 J/TH"). This is the per-chip nameplate
/// (silicon characterization) — distinct from the whole-board
/// `BM1366_PROFILES` wall-watt efficiency (≈ 28.5 W/TH at the S19k
/// Pro nameplate, which includes PSU + fan + control-board overhead).
pub const BM1366_NAMEPLATE_JTH: f32 = 21.5;

// ---------------------------------------------------------------------------
// Algorithmic PLL parameter compute — mirrors `bm1362::pll_compute` (W12.4).
// Source:  §5.1:
//   freq = (25 MHz × FBDIV) / (REFDIV × POSTDIV1 × POSTDIV2)
//   bmminer trace `[%d] _POSTDIV1 = %d, _POSTDIV2 = %d, USER_DIV = %d,
//   freq = %d` — same 5-parameter search family as BM1362.
// §13 cheat-sheet: BM1366 is in the `BM1362/1366/1368/1370` column with
// FB_DIV range **160-239** and POSTDIV encoding `((p1-1)<<4)|(p2-1)`.
// We REUSE the BM1362 PLL formula + ranges + compute (same encoding
// family per §13 + §5 header "Same encoding family as BM1362") rather
// than duplicate the brute-force search — additive, no behavior change
// to BM1362.
// ---------------------------------------------------------------------------

/// BM1366 PLL output frequency for given dividers at a reference clock.
/// Thin re-export of [`crate::bm1362::pll_freq_mhz`] — the BM1366 PLL
/// encoding family is identical to BM1362 per PLL bible §5 header
/// ("Same encoding family as BM1362") + §13 cheat-sheet.
pub const fn pll_freq_mhz(
    refclk_mhz: u32,
    refdiv: u32,
    fbdiv: u32,
    postdiv1: u32,
    postdiv2: u32,
    user_div: u32,
) -> u32 {
    crate::bm1362::pll_freq_mhz(refclk_mhz, refdiv, fbdiv, postdiv1, postdiv2, user_div)
}

/// Algorithmically resolve a `(refdiv, fbdiv, postdiv1, postdiv2,
/// user_div)` set yielding `target_mhz` at `ref_mhz`. Re-exports
/// [`crate::bm1362::pll_compute`] — same BM136x-family search per PLL
/// bible §5.1 (identical bmminer format-string trace) + §13.
pub fn pll_compute(target_mhz: u32, ref_mhz: u32) -> Option<crate::bm1362::PllParams> {
    crate::bm1362::pll_compute(target_mhz, ref_mhz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_steps_in_correct_range() {
        assert_eq!(BM1366_TABLE.profiles.len(), 5);
        assert_eq!(BM1366_TABLE.min_step(), -2);
        assert_eq!(BM1366_TABLE.max_step(), 2);
    }

    #[test]
    fn nameplate_default_step_anchors_s19k_pro() {
        // S19k Pro nameplate: 120 TH/s @ 3,420 W â†’ â‰ˆ 28.5 W/TH.
        let default = BM1366_TABLE.default_profile().unwrap();
        assert_eq!(default.wall_watts, Some(3420));
        assert!((default.hashrate_ths.unwrap() - 120.0).abs() < 1e-3);
        let eff = default.watts_per_ths().unwrap();
        assert!(
            (27.0..=30.0).contains(&eff),
            "S19k Pro nameplate efficiency {} W/TH outside [27, 30]",
            eff
        );
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre = BM1366_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1366_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(pre.step, computed.step);
    }

    #[test]
    fn underclocked_steps_beat_default_efficiency() {
        let default = BM1366_TABLE.default_profile().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        for step in [-2, -1] {
            let s = BM1366_TABLE.by_step(step).unwrap();
            let eff = s.watts_per_ths().unwrap();
            assert!(
                eff < eff_def,
                "step {} efficiency ({}) should beat default ({})",
                step,
                eff,
                eff_def
            );
        }
    }

    #[test]
    fn s19k_pro_hardware_constants_match_re_doc() {
        assert_eq!(BM1366_CORES_PER_CHIP, 894);
        assert_eq!(BM1366_CHIPS_PER_CHAIN_S19K_PRO, 76);
        assert_eq!(BM1366_CHAIN_COUNT_S19K_PRO, 3);
        // 76 Ã— 3 = 228 chips total per S19k Pro.
        assert_eq!(
            BM1366_CHIPS_PER_CHAIN_S19K_PRO * BM1366_CHAIN_COUNT_S19K_PRO,
            228
        );
    }

    #[test]
    fn operational_baud_matches_bitmain_path() {
        assert_eq!(BM1366_OPERATIONAL_BAUD, 3_125_000);
        assert_eq!(BM1366_MISCCTRL_BAUD_VALUE, 0x0000_3001);
        assert_eq!(BM1366_MISCCTRL_BAUD_VALUE_1MBAUD, 0x1130_0200);
    }

    #[test]
    fn hashcounting_value_matches_re_doc() {
        // chip-init-sequences.md line 197: 0x0000115A for S19k Pro stock.
        assert_eq!(BM1366_HASHCOUNTING_S19K_PRO_STOCK, 0x0000_115A);
    }

    #[test]
    fn step_voltage_increases_with_frequency() {
        for window in BM1366_PROFILES.windows(2) {
            assert!(
                window[1].voltage_v >= window[0].voltage_v,
                "voltage non-monotonic at step {}",
                window[1].step
            );
            assert!(
                window[1].freq_mhz > window[0].freq_mhz,
                "frequency non-monotonic at step {}",
                window[1].step
            );
        }
    }

    #[test]
    fn json_round_trip_preserves_profile_fields() {
        let original = BM1366_TABLE.by_step(0).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    #[test]
    fn nameplate_voltage_sits_at_138v_autotune_target() {
        let default = BM1366_TABLE.default_profile().unwrap();
        assert!((default.voltage_v - 13.8).abs() < 1e-3);
    }

    #[test]
    fn s19k_pro_efficiency_beats_s17_pro() {
        // BM1366 is a generation newer than BM1397 â€” should produce
        // better J/TH at the nameplate.
        let bm1366_eff = BM1366_TABLE
            .default_profile()
            .unwrap()
            .watts_per_ths()
            .unwrap();
        // BM1397 nameplate is ~41.9 J/TH; BM1366 should be much better.
        assert!(
            bm1366_eff < 35.0,
            "BM1366 nameplate efficiency {} W/TH should beat BM1397's ~42",
            bm1366_eff
        );
    }

    // -------------------------------------------------------------------
    // PR-053 depth-parity pins. Each new geometry/PLL/CRC constant gets a
    // unit test (a number with no test is a liability — bm1362.rs idiom).
    // -------------------------------------------------------------------

    #[test]
    fn chip_id_register_value_is_0x1366() {
        // chip-init-sequences.md BM1366 SPEC BLOCK + driver bm1366.rs:41.
        assert_eq!(chip::CHIP_ID, 0x1366);
    }

    #[test]
    fn process_node_is_5nm() {
        // PLL bible §5 header "Gen 4, TSMC 5nm".
        assert_eq!(chip::PROCESS_NM, 5);
    }

    #[test]
    fn crc5_command_poly_and_init_match_protocol_doc() {
        // DCENT_OS_Antminer/ "CRC5 (poly 0x05, init 0x1F) for
        // commands" — BM139x+ unified command family.
        assert_eq!(chip::CRC5_CMD_POLY, 0x05);
        assert_eq!(chip::CRC5_CMD_INIT, 0x1F);
    }

    #[test]
    fn cores_per_chip_is_894() {
        // chip-init-sequences.md BM1366 SPEC BLOCK + PLL bible §13.
        assert_eq!(chip::CORES_PER_CHIP, 894);
        // Must agree with the long-standing module constant.
        assert_eq!(chip::CORES_PER_CHIP, BM1366_CORES_PER_CHIP);
    }

    #[test]
    fn jobid_step_and_addr_mask_match_spec_block() {
        // chip-init-sequences.md: "Address stride: 256 / N (BM1366:
        // 0xF8 mask, JOBID step +8)".
        assert_eq!(chip::JOBID_STEP, 8);
        assert_eq!(chip::CHIP_ADDR_MASK, 0xF8);
    }

    #[test]
    fn hardware_difficulty_is_256() {
        // PLL bible §13 cheat-sheet "256 (BM1362/1366)".
        assert_eq!(chip::HARDWARE_DIFFICULTY, 256);
    }

    #[test]
    fn open_core_not_required_for_bm136x_family() {
        // chip-init-sequences.md BM1366 SPEC BLOCK "Open-core: NOT
        // required" + PLL bible §13. Pinned via a non-bool comparison
        // to satisfy BOTH clippy::assertions_on_constants (no
        // assert!(CONST)) AND clippy::bool_assert_comparison (no
        // assert_eq!(_, false)).
        assert_eq!(u8::from(chip::OPEN_CORE_REQUIRED), 0);
    }

    #[test]
    fn work_layout_register_addresses_match_pll_bible_family_column() {
        // PLL bible §13 cheat-sheet BM1362/1366/1368/1370 column.
        assert_eq!(work_layout::PLL0_REG, 0x08);
        assert_eq!(work_layout::MISC_CTRL_REG, 0x18);
        assert_eq!(work_layout::FAST_UART_REG, 0x28);
        assert_eq!(work_layout::HASHCOUNTING_REG, 0x10);
        // Response framing matches the driver source-of-truth.
        assert_eq!(work_layout::RESPONSE_BYTES, 11);
    }

    #[test]
    fn s19xp_hashcounting_variants_match_re_doc() {
        // chip-init-sequences.md BM1366 step 11.
        assert_eq!(BM1366_HASHCOUNTING_S19XP_STOCK, 0x0000_151C);
        assert_eq!(BM1366_HASHCOUNTING_S19XP_LUXOS, 0x0000_1446);
        // S19k Pro stock value unchanged (no regression to the
        // long-standing constant).
        assert_eq!(BM1366_HASHCOUNTING_S19K_PRO_STOCK, 0x0000_115A);
    }

    #[test]
    fn freq_voltage_table_is_monotonic_whole_board_cadence() {
        // These are whole-board chain-rail rows (matching
        // BM1366_PROFILES.voltage_v units), so the cadence is freq↑ ⟹
        // voltage↑ (NOT the BHB42xxx chip-rail silicon-floor cadence).
        // Top row = highest freq + highest voltage.
        let t = BM1366_S19K_PRO_FREQ_VOLT_TABLE;
        assert!(!t.is_empty());
        for w in t.windows(2) {
            assert!(w[0].0 > w[1].0, "freq must strictly decrease top→bottom");
            assert!(
                w[0].1 > w[1].1,
                "chain-rail voltage decreases with frequency (whole-board cadence)"
            );
        }
        // Top/bottom freqs bracket the BM1366_PROFILES envelope.
        assert_eq!(t[0].0, 685);
        assert_eq!(t[t.len() - 1].0, 540);
        // Each row's voltage matches a BM1366_PROFILES row (mV ⇄ V).
        for &(f, mv) in t {
            let p = BM1366_PROFILES
                .iter()
                .find(|p| p.freq_mhz == f as u32)
                .unwrap_or_else(|| panic!("no BM1366_PROFILES row for {f} MHz"));
            assert_eq!(
                mv as f32,
                (p.voltage_v * 1000.0).round(),
                "table mV must equal BM1366_PROFILES voltage for {f} MHz"
            );
        }
    }

    #[test]
    fn s19xp_table_aliases_s19kpro_table() {
        // SKU difference is chip-count + HASHCOUNTING, not the
        // freq/voltage envelope — the two tables are content-identical.
        assert_eq!(
            BM1366_S19_XP_FREQ_VOLT_TABLE,
            BM1366_S19K_PRO_FREQ_VOLT_TABLE
        );
    }

    #[test]
    fn sku_geometry_matches_re_doc() {
        let s19k = Bm1366HashboardSku::S19kPro;
        let s19xp = Bm1366HashboardSku::S19Xp;
        assert_eq!(s19k.chips_per_chain(), 76);
        assert_eq!(s19xp.chips_per_chain(), 110);
        assert_eq!(s19k.chain_count(), 3);
        assert_eq!(s19xp.chain_count(), 3);
        assert_eq!(s19k.hashcounting_stock(), 0x0000_115A);
        assert_eq!(s19xp.hashcounting_stock(), 0x0000_151C);
        // 76 × 3 = 228 total chips on a stock S19k Pro (consistent with
        // the long-standing constants).
        assert_eq!(
            s19k.chips_per_chain() as u32 * s19k.chain_count() as u32,
            BM1366_CHIPS_PER_CHAIN_S19K_PRO * BM1366_CHAIN_COUNT_S19K_PRO
        );
    }

    #[test]
    fn sku_id_round_trip_and_default() {
        for sku in ALL_BM1366_HASHBOARD_SKUS {
            assert_eq!(Bm1366HashboardSku::from_id(sku.hashboard_id()), Some(*sku));
        }
        assert_eq!(Bm1366HashboardSku::from_id("nope"), None);
        assert_eq!(
            Bm1366HashboardSku::default_for_unrecognized_sku(),
            Bm1366HashboardSku::S19kPro
        );
    }

    #[test]
    fn sku_serde_round_trip() {
        for sku in ALL_BM1366_HASHBOARD_SKUS {
            let j = serde_json::to_string(sku).unwrap();
            let back: Bm1366HashboardSku = serde_json::from_str(&j).unwrap();
            assert_eq!(*sku, back);
        }
    }

    #[test]
    fn nameplate_jth_matches_hashboard_diagnostics() {
        // HASHBOARD_DIAGNOSTICS.md §1 chip table line 58: 21.5 J/TH.
        assert!((BM1366_NAMEPLATE_JTH - 21.5).abs() < 1e-3);
    }

    #[test]
    fn pll_freq_mhz_matches_bm1362_family_formula() {
        // PLL bible §5.1: freq = (25 × FBDIV) / (REFDIV × PD1 × PD2).
        // Canonical BM1362-family unit dividers: 25 MHz, refdiv 1,
        // user_div 1, (pd1,pd2)=(5,2). FBDIV 218 → 545 MHz.
        assert_eq!(pll_freq_mhz(25, 1, 218, 5, 2, 1), 545);
        // Re-export must be byte-identical to the BM1362 helper.
        assert_eq!(
            pll_freq_mhz(25, 1, 200, 5, 2, 1),
            crate::bm1362::pll_freq_mhz(25, 1, 200, 5, 2, 1)
        );
        // Zero-divider guard (no panic).
        assert_eq!(pll_freq_mhz(25, 0, 218, 5, 2, 1), 0);
    }

    #[test]
    fn pll_compute_resolves_canonical_targets_and_rejects_garbage() {
        // Same BM136x-family search as BM1362 (PLL bible §5.1).
        let p = pll_compute(545, 25).expect("545 MHz must resolve");
        assert_eq!(p.compute_freq_mhz(25), 545);
        assert!(pll_compute(0, 25).is_none());
        assert!(pll_compute(545, 0).is_none());
        // Re-export equivalence with the BM1362 implementation.
        assert_eq!(pll_compute(525, 25), crate::bm1362::pll_compute(525, 25));
    }

    /// PR-057 / R11-13: pins the corpus-confirmed BM1366 **multi-carrier**
    /// identity (Zynq + Amlogic + CVitek — NOT Amlogic-exclusive) so a
    /// future "cleanup" cannot silently re-introduce the stale
    /// "Amlogic NoPic only" module-doc narrative this PR corrected.
    ///
    /// Resolution + full citation index (Zynq "xil" S19k Pro/S19 XP
    /// stock+VNish firmware, the 7-vs-13 voltage-domain conflation
    /// correction, the carrier-vs-NoPic distinction):
    /// .
    /// Static-analysis only; additive; asserts existing constants — no
    /// value/behavior/API change. Mirrors the in-file PR-053 pin idiom
    /// and the PR-054/055/056 "pin = the caveat-closure" discipline.
    #[test]
    fn pr057_bm1366_carrier_is_multi_carrier_not_amlogic_only() {
        // The BM1366 runtime chip identity is carrier-invariant: a Zynq
        // S19k Pro and an Amlogic S19k Pro both enumerate as 0x1366
        // (driver bm1366.rs:41 + chip-init-sequences.md SPEC BLOCK).
        // Carrier never changes this — mirrors the braiins_bm1387.rs
        // "one chip ID for the whole family" discipline.
        assert_eq!(
            chip::CHIP_ID,
            0x1366,
            "BM1366 chip ID is carrier-invariant per PR-057 corpus resolution"
        );
        // Cores/baud are die-fixed regardless of carrier — pin them to
        // the long-standing module constants so a carrier-driven edit
        // can't drift die-fixed values.
        assert_eq!(chip::CORES_PER_CHIP, BM1366_CORES_PER_CHIP);
        assert_eq!(BM1366_OPERATIONAL_BAUD, 3_125_000);

        // What actually distinguishes the SKUs is the *hashboard*
        // geometry, NOT the control-board carrier: S19k Pro = 76
        // chips/chain (BHB56902, fixture Asic_Num_Per_Voltage_Domain=7 ×
        // 11 domains), S19 XP = 110 (BHB56802, =10 × 11 domains). Both
        // ship on Zynq AND Amlogic. The corpus "7-vs-13-domain"
        // shorthand was a conflation: BOTH BM1366 SKUs have 11 voltage
        // domains; the 13-domain/7-chip geometry is S21 XP / BM1370P (a
        // different chip) per PR-057 §3. Pin the existing SKU constants
        // (no fabricated value) so a carrier-driven "fix" can't mutate
        // the geometry instead of the doc-comment.
        let s19k = Bm1366HashboardSku::S19kPro;
        let s19xp = Bm1366HashboardSku::S19Xp;
        assert_eq!(s19k.chips_per_chain(), 76);
        assert_eq!(s19xp.chips_per_chain(), 110);
        assert_eq!(s19k.chain_count(), 3);
        assert_eq!(s19xp.chain_count(), 3);
        // 11 voltage domains is fixed across both SKUs (fixture
        // Voltage_Domain=11); the discriminator is chips-per-domain
        // (7 vs 10), reflected here as chips-per-chain (76 vs 110),
        // never the carrier. Whole-SKU totals stay consistent with the
        // long-standing chain constants.
        assert_eq!(
            s19k.chips_per_chain() as u32 * s19k.chain_count() as u32,
            BM1366_CHIPS_PER_CHAIN_S19K_PRO * BM1366_CHAIN_COUNT_S19K_PRO
        );
        assert_ne!(
            s19k.chips_per_chain(),
            s19xp.chips_per_chain(),
            "S19k Pro vs S19 XP differ by hashboard SKU geometry, NOT by carrier"
        );

        // The live-status sentinel stays LiveConfirmed (BitAxe Hex Supra
        // single-chip first share 2026-03-19) — carrier classification
        // does not regress the BM1366 live posture.
        assert!(matches!(
            BM1366_TABLE.live_status,
            crate::ChipStatus::LiveConfirmed
        ));
    }
}
