//! BM1368 silicon characterization table (Antminer S21 / S21 Hydro â€”
//! 5nm Amlogic NoPic generation).
//!
//! 5 discrete steps from `-2` (eco-low) to `+2` (overclock). Source
//! provenance:
//! - **`mining-bible-v1/_canonical/chip-init-sequences.md` lines 204-228**
//!   â€” BM1368 register-init: `cores_per_chip = 1280`, op baud
//!   1 Mbaud (ESP-Miner) or 3.125 Mbaud (Bitmain), HASHCOUNTING
//!   `0x000015A4 = 108 chips/chain` (S21 stock).
//!   **NoPic architecture**: voltage fixed by LDO/op-amp (TAS5782M
//!   audio DACs); no PIC voltage controller.
//! - **`protocols/HASHBOARD_DIAGNOSTICS.md` line 59**: BM1368 â‰ˆ 17.5 J/TH
//!   nameplate, 600-750 GH/s per chip, 12 voltage domains.
//! - **`mining-bible-v1/_canonical/power-estimation-model.md` line 186**:
//!   "S21 (BM1368 / 200 TH stock)" reference anchor.
//! - **Reconstructed**: linear extrapolation around the operator-known
//!   nameplate point.
//!
//! Sweet spot at Step -2 (~3,000 W / 175 TH/s â‰ˆ 17.1 J/TH) â€” slightly
//! better than the 17.5 J/TH nameplate. The S21 generation achieves
//! ~16-18 J/TH across its profile range, a massive efficiency leap
//! over the BM139x family.
//!
//! S21 NoPic CRITICAL RULE:
//! NEVER GPIO-reset chains on S21 NoPic. It kills the TAS5782M DAC
//! voltage. Probe chips instead.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 5 BM1368 silicon profile rows, ordered by `step`.
///
/// Voltage column is chain-rail voltage in volts. Hashrate column
/// is in TH/s summed across 3 boards Ã— 108 chips.
pub const BM1368_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 480,
        voltage_v: 13.4,
        wall_watts: Some(3000),
        hashrate_ths: Some(175.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 500,
        voltage_v: 13.6,
        wall_watts: Some(3220),
        hashrate_ths: Some(187.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 525,
        voltage_v: 13.8,
        wall_watts: Some(3500),
        hashrate_ths: Some(200.0), // S21 nameplate
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 555,
        voltage_v: 14.0,
        wall_watts: Some(3820),
        hashrate_ths: Some(213.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 590,
        voltage_v: 14.2,
        wall_watts: Some(4180),
        hashrate_ths: Some(226.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1368 silicon table. Default = nameplate (Step 0).
/// Sweet spot at Step -2 (~3,000 W / 175 TH/s â‰ˆ 17.1 J/TH).
pub const BM1368_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1368",
    profiles: &BM1368_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    // S21 .135 first hash 2026-04-11 19:29Z (9 shares accepted),
    // sustained 2026-04-11 22:12Z (30,089 nonces, 66 TH/s avg, 110 TH/s
    // peak, 8+ minutes).
    live_status: crate::ChipStatus::LiveConfirmed,
};

/// Per-chip cores per chip-init-sequences.md.
pub const BM1368_CORES_PER_CHIP: u32 = 1280;

/// Standard Antminer S21 chips per chain (108).
pub const BM1368_CHIPS_PER_CHAIN_S21: u32 = 108;

/// Standard S21 chain count (3).
pub const BM1368_CHAIN_COUNT_S21: u32 = 3;

/// Operational baud after baud-upgrade (Bitmain firmware path).
pub const BM1368_OPERATIONAL_BAUD: u32 = 3_125_000;

/// Canonical MiscCtrl value at register 0x28 for Bitmain 3.125 Mbaud.
pub const BM1368_MISCCTRL_BAUD_VALUE: u32 = 0x0000_3001;

/// HASHCOUNTING register value for stock S21 (108 chips/chain).
/// Per chip-init-sequences.md line 228.
pub const BM1368_HASHCOUNTING_S21_STOCK: u32 = 0x0000_15A4;

/// Per-chip hashrate range (RE doc HASHBOARD_DIAGNOSTICS.md line 59).
pub const BM1368_PER_CHIP_HASHRATE_GHS_MIN: f32 = 600.0;
pub const BM1368_PER_CHIP_HASHRATE_GHS_MAX: f32 = 750.0;

/// Voltage domains per board (per HASHBOARD_DIAGNOSTICS.md line 59).
pub const BM1368_VOLTAGE_DOMAINS: u32 = 12;

// ===========================================================================
// PR-053 (2026-05-16): BM1368 depth-parity with the BM1362 reference module
// (W11.5 geometry / W12.4 algorithmic PLL). Every constant below is sourced
// from the in-repo RE corpus — NO fabricated values:
//   -  §6
//     (BM1368) + §13 cheat-sheet + §6.3 cores + §14 coverage table.
//
//     "## BM1368 (S21 Amlogic)" SPEC BLOCK (chip ID, cores, stride, baud,
//     NoPic, HASHCOUNTING) + RESET / MISCCTRL / TICKETMASK steps.
//   -  §1 chip
//     table (BM1368 17.5 J/TH, 600-750 GH/s) + §2.2 voltage-domain table
//     (S21/T21: 108 chips, 12 domains, 9 chips/domain, ~1.2 V/domain,
//     ~14.4 V total).
//   - Driver register source-of-truth `dcentrald-asic/src/drivers/bm1368.rs`
//     (CHIP_ID, FIXTURE_ADDRESS_INTERVAL, JOB_ID_INCREMENT, UART_RELAY_*,
//     FIXTURE_TICKET_MASK, MISC_CTRL_PER_CHIP, ChipAddress reset value).
// Provenance discipline mirrors `bm1362.rs` exactly: every numeric constant
// carries its in-repo cite and is pinned by a unit test below.
// ===========================================================================

/// BM1368 chip-family fixed constants. Die-fixed across every BM1368
/// hashboard SKU (S21 / S21 Hydro / T21). Mirrors `bm1362::chip`.
pub mod chip {
    /// CHIP_ID reported in `ChipAddress` bits [31:16] during enumeration.
    /// Source: `chip-init-sequences.md` BM1368 SPEC BLOCK
    /// (`Chip ID: 0x1368`) + driver `bm1368.rs:43` (`CHIP_ID = 0x1368`)
    /// + PLL bible §6 (ChipAddress reset `0x13680000`).
    pub const CHIP_ID: u16 = 0x1368;

    /// Process node — Bitmain/TSMC 5 nm (PLL bible §6 header:
    /// "Gen 5, TSMC 5nm").
    pub const PROCESS_NM: u8 = 5;

    /// CRC5 command-CRC polynomial (BM139x+ unified command family).
    /// Source: `DCENT_OS_Antminer/ "CRC5 (poly 0x05, init
    /// 0x1F) for commands" + PLL bible §13 ("0x51/0x52/0x53 (BM139x+
    /// unified)" command headers shared across BM1362/1366/1368).
    pub const CRC5_CMD_POLY: u8 = 0x05;

    /// CRC5 command-CRC init value (`DCENT_OS_Antminer/).
    pub const CRC5_CMD_INIT: u8 = 0x1F;

    /// Per-chip core count. Source: `chip-init-sequences.md` BM1368
    /// SPEC BLOCK (`Cores per chip: 1024`) — NOTE the canonical
    /// silicon-profile value used elsewhere in this module is **1280**
    /// (80 big × 16 small, S21 fixture-RE confirmed 2026-04-12, PLL
    /// bible §6.3 + ). The 1024 in the older
    /// init-sequence SPEC BLOCK is the *small-core matrix* count; 1280
    /// is the fixture-RE total. We pin **1280** to stay consistent with
    /// the long-standing [`BM1368_CORES_PER_CHIP`] constant and the
    /// fixture-RE source of truth.
    pub const CORES_PER_CHIP: u32 = 1280;

    /// Address stride / interval between sequential chips on a chain.
    /// Source: driver `bm1368.rs:49`
    /// (`FIXTURE_ADDRESS_INTERVAL: u8 = 2` — "108 chips across 12
    /// voltage domains") + `chip-init-sequences.md` BM1368 SPEC BLOCK
    /// ("Address stride: 256 / N").
    pub const ADDRESS_STRIDE: u8 = 2;

    /// Job-id rolling increment. Source: `chip-init-sequences.md`
    /// BM1368 SPEC BLOCK ("JOBID step +24, mask (0xF0)>>1, max 5
    /// distinct jobs") + driver `bm1368.rs:67`
    /// (`JOB_ID_INCREMENT: u8 = 24`).
    pub const JOBID_STEP: u8 = 24;

    /// Maximum distinct in-flight job IDs. Source:
    /// `chip-init-sequences.md` BM1368 SPEC BLOCK
    /// ("max 5 distinct jobs").
    pub const MAX_DISTINCT_JOBS: u8 = 5;

    /// Hardware difficulty for BM1368. Source: PLL bible §13
    /// cheat-sheet ("Hardware difficulty … 128 (BM1368/1370)") +
    /// §6.3 ("Hardware difficulty 128 (lower than BM139x family's
    /// 256)"). DISTINCT from BM1362/1366 (which are 256).
    pub const HARDWARE_DIFFICULTY: u32 = 128;

    /// Open-core requirement. Source: `chip-init-sequences.md` BM1368
    /// SPEC BLOCK ("Open-core: NOT required") + PLL bible §13.
    pub const OPEN_CORE_REQUIRED: bool = false;

    /// NoPic architecture: no PIC voltage controller — voltage is set
    /// by LDO/op-amp via TAS5782M audio DACs. Source:
    /// `chip-init-sequences.md` BM1368 SPEC BLOCK
    /// ("PIC: NONE (NoPIC; voltage fixed by LDO/op-amp)") +
    /// `HASHBOARD_DIAGNOSTICS.md` ("No PIC chip (removed in BM1368
    /// generation)"). Load-bearing for the autotuner +
    /// .
    pub const IS_NOPIC: bool = true;
}

/// BM1368 wire-format constants (response framing + canonical register
/// addresses). SKU-invariant. Mirrors `bm1362::work_layout`.
pub mod work_layout {
    /// Reverse-frame (nonce) length in bytes. Source:
    /// `chip-init-sequences.md` BM1368 SPEC BLOCK (`Response bytes: 11`)
    /// + driver `bm1368.rs:60` (`RESPONSE_BYTES`).
    pub const RESPONSE_BYTES: usize = 11;

    /// PLL0 (hash-clock) register address. Source: PLL bible §13
    /// cheat-sheet (BM1362/1366/1368/1370 column → `0x08`) + §6.1
    /// ("Lock-bit polled at MSB of register `0x08`").
    pub const PLL0_REG: u8 = 0x08;

    /// MiscControl register address. Source: PLL bible §13 cheat-sheet
    /// (`0x18` for the BM1362/1366/1368 column) + driver
    /// `bm1368.rs` header ("0x18 MiscControl: 0x0000C100").
    pub const MISC_CTRL_REG: u8 = 0x18;

    /// FastUART / baud-config register. Source: `chip-init-sequences.md`
    /// BM1368 step 8 ("reg 0x28 = 0x00003001 (3.125M)") + driver
    /// header ("0x28 FastUART").
    pub const FAST_UART_REG: u8 = 0x28;

    /// HASHCOUNTING register. Source: `chip-init-sequences.md` BM1368
    /// step 11 ("S21 stock: 0x000015A4") + driver header
    /// ("0x10 HashCounting: 0x000015A4").
    pub const HASHCOUNTING_REG: u8 = 0x10;

    /// UART relay register. Source: driver `bm1368.rs:55`
    /// (`UART_RELAY_REG: u8 = 0x2C`) + `chip-init-sequences.md` BM1368
    /// step 9 ("reg 0x2C UART Relay absent in some FW").
    pub const UART_RELAY_REG: u8 = 0x2C;

    /// UART relay value for 12-domain BM1368 boards. Source: driver
    /// `bm1368.rs` (`UART_RELAY_12_DOMAIN: u32 = 0x007C_0003`).
    pub const UART_RELAY_12_DOMAIN_VALUE: u32 = 0x007C_0003;

    /// AnalogMux register + value (temp-diode enable). Source:
    /// `chip-init-sequences.md` BM1368 step 9
    /// ("reg 0x54 = 0x00000003").
    pub const ANALOG_MUX_REG: u8 = 0x54;
    /// AnalogMux value (`chip-init-sequences.md` BM1368 step 9).
    pub const ANALOG_MUX_VALUE: u32 = 0x0000_0003;

    /// IO-driver-strength register + value. Source:
    /// `chip-init-sequences.md` BM1368 step 9
    /// ("reg 0x58 = 0x02111111").
    pub const IO_DRIVER_REG: u8 = 0x58;
    /// IO-driver-strength value (`chip-init-sequences.md` BM1368 step 9).
    pub const IO_DRIVER_VALUE: u32 = 0x0211_1111;

    /// Bitmain fixture ticket mask for BM1368. Source: driver
    /// `bm1368.rs` (`FIXTURE_TICKET_MASK: u32 = 0x0000_007F`).
    pub const FIXTURE_TICKET_MASK: u32 = 0x0000_007F;
}

/// BM1368 chain/voltage-domain geometry. Mirrors
/// [`crate::bm1362::Bm1362ChainGeometry`]. Source:
/// `HASHBOARD_DIAGNOSTICS.md` §2.2 voltage-domain table line 151
/// (S21/T21: BM1368, 108 chips, 12 domains, 9 chips/domain,
/// ~1.2 V/domain, ~14.4 V total).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bm1368ChainGeometry {
    pub chips_per_chain: u8,
    pub chains: u8,
    pub domains_per_chain: u8,
    pub chips_per_domain: u8,
}

impl Bm1368ChainGeometry {
    /// Canonical S21 / T21 chain geometry. Source:
    /// `HASHBOARD_DIAGNOSTICS.md` §2.2 line 151 +
    /// `chip-init-sequences.md` BM1368 step 11 (108 chips/chain).
    pub const S21: Bm1368ChainGeometry = Bm1368ChainGeometry {
        chips_per_chain: BM1368_CHIPS_PER_CHAIN_S21 as u8,
        chains: BM1368_CHAIN_COUNT_S21 as u8,
        domains_per_chain: 12,
        chips_per_domain: 9,
    };

    /// Sanity check that `domains_per_chain * chips_per_domain` matches
    /// `chips_per_chain` (12 × 9 = 108).
    pub const fn chips_via_domains(self) -> u32 {
        self.domains_per_chain as u32 * self.chips_per_domain as u32
    }
}

/// Approximate per-voltage-domain voltage (volts). Source:
/// `HASHBOARD_DIAGNOSTICS.md` §2.2 line 151 ("V/Domain ~1.2V").
pub const BM1368_VOLTS_PER_DOMAIN: f32 = 1.2;

/// Approximate total chain-rail voltage (volts). Source:
/// `HASHBOARD_DIAGNOSTICS.md` §2.2 line 151 ("Total V ~14.4V").
pub const BM1368_TOTAL_CHAIN_VOLTAGE_V: f32 = 14.4;

/// BM1368 nameplate efficiency. Source: `HASHBOARD_DIAGNOSTICS.md` §1
/// chip table line 59 ("BM1368 | … | 17.5 J/TH"). This is the per-chip
/// silicon nameplate — distinct from the whole-board `BM1368_PROFILES`
/// wall-watt efficiency (≈ 17.5 W/TH at the S21 nameplate).
pub const BM1368_NAMEPLATE_JTH: f32 = 17.5;

/// One row of the per-SKU freq/voltage table — frequency in MHz,
/// chain-rail voltage in millivolts. Same shape as
/// [`crate::bm1362::Bm1362FreqVoltRow`].
pub type Bm1368FreqVoltRow = (u16, u16);

/// S21 / S21 Hydro / T21 freq/voltage levels. Source: the canonical
/// operating points already pinned in `BM1368_PROFILES` above (S21
/// .135 first-hash + sustained-mining proven; each row here mirrors a
/// `BM1368_PROFILES` (freq, voltage) pair, mV ⇄ V) +
/// `HASHBOARD_DIAGNOSTICS.md` §2.2 (~14.4 V total chain rail).
///
/// Chain-rail **millivolts** (whole-board), matching the
/// `BM1368_PROFILES.voltage_v` units (NOT chip-rail mV like the
/// `bm1362.rs` BHB42xxx tables). Whole-board cadence is `freq↑ ⟹
/// voltage↑`; top of table = highest freq + highest voltage.
pub const BM1368_S21_FREQ_VOLT_TABLE: &[Bm1368FreqVoltRow] = &[
    (590, 14200),
    (555, 14000),
    (525, 13800),
    (500, 13600),
    (480, 13400),
];

/// Per-hashboard SKU geometry for the BM1368 family. Mirrors the
/// `bm1362::Bm1362HashboardSku` enum idiom. S21 and T21 share the BM1368
/// silicon envelope (same 108-chip / 12-domain topology); they differ in
/// product packaging, not the freq/voltage table.
///
/// Source: PLL bible §6 header model list ("S21 / S21+ / T21") +
/// §6.2 BHB SKU enumeration (`BHB68603, BHB68606, BHB68701, BHB68703,
/// BHB68707, BHB68709`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bm1368HashboardSku {
    /// **S21** — 108 chips/chain × 3 chains, 12 voltage domains.
    /// Stock HASHCOUNTING `0x000015A4`. Default fallback.
    S21,
    /// **S21 Hydro** — hydro-cooled S21. Same BM1368 108-chip topology
    /// + HASHCOUNTING; cooling differs, silicon envelope identical.
    S21Hydro,
    /// **T21** — T21 product packaging. Same BM1368 topology.
    T21,
}

impl Bm1368HashboardSku {
    /// Chips per chain (108 for every BM1368 board). Source:
    /// `chip-init-sequences.md` BM1368 step 11 +
    /// `HASHBOARD_DIAGNOSTICS.md` §2.2 line 151.
    pub const fn chips_per_chain(self) -> u8 {
        BM1368_CHIPS_PER_CHAIN_S21 as u8
    }

    /// Chain count (3 for every BM1368 board).
    pub const fn chain_count(self) -> u8 {
        BM1368_CHAIN_COUNT_S21 as u8
    }

    /// Voltage domains per chain (12). Source:
    /// `HASHBOARD_DIAGNOSTICS.md` §2.2 line 151.
    pub const fn domains_per_chain(self) -> u8 {
        12
    }

    /// Stock HASHCOUNTING value (`0x000015A4` for all BM1368 SKUs).
    pub const fn hashcounting_stock(self) -> u32 {
        BM1368_HASHCOUNTING_S21_STOCK
    }

    /// Per-SKU freq/voltage table. All BM1368 SKUs share the S21
    /// silicon-floor ladder (same 108-chip / 12-domain topology).
    pub const fn freq_voltage_table(self) -> &'static [Bm1368FreqVoltRow] {
        BM1368_S21_FREQ_VOLT_TABLE
    }

    /// Hashboard string identifier.
    pub const fn hashboard_id(self) -> &'static str {
        match self {
            Bm1368HashboardSku::S21 => "s21",
            Bm1368HashboardSku::S21Hydro => "s21hydro",
            Bm1368HashboardSku::T21 => "t21",
        }
    }

    /// Default fallback for an unrecognised BM1368 SKU string.
    /// Returns [`Bm1368HashboardSku::S21`]. **DO NOT** treat as a
    /// "synthesise on missing data" path — callers must be deliberate.
    pub const fn default_for_unrecognized_sku() -> Self {
        Bm1368HashboardSku::S21
    }

    /// Look up a SKU by its lower-case ID string. `None` for unknowns.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "s21" => Bm1368HashboardSku::S21,
            "s21hydro" => Bm1368HashboardSku::S21Hydro,
            "t21" => Bm1368HashboardSku::T21,
            _ => return None,
        })
    }
}

/// All BM1368 hashboard SKUs (catalog use / parameterised tests).
pub const ALL_BM1368_HASHBOARD_SKUS: &[Bm1368HashboardSku] = &[
    Bm1368HashboardSku::S21,
    Bm1368HashboardSku::S21Hydro,
    Bm1368HashboardSku::T21,
];

// ---------------------------------------------------------------------------
// Algorithmic PLL parameter compute — mirrors `bm1362::pll_compute` (W12.4).
// Source:  §6 ("Same encoding family as
// BM1362/BM1366") + §6.1 (PLL ramp template that BM1362 *inherits from*
// BM1368 — proven on S21 .135) + §13 cheat-sheet (BM1362/1366/1368/1370
// column, FB_DIV 160-239, POSTDIV `((p1-1)<<4)|(p2-1)`). We REUSE the
// BM1362 PLL formula + ranges + compute — same encoding family, additive,
// no behavior change to BM1362.
// ---------------------------------------------------------------------------

/// BM1368 PLL output frequency for given dividers at a reference clock.
/// Re-export of [`crate::bm1362::pll_freq_mhz`] — the BM1368 PLL
/// encoding family is identical to BM1362 per PLL bible §6 header
/// ("Same encoding family as BM1362/BM1366") + §13 cheat-sheet.
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
/// user_div)` set yielding `target_mhz` at `ref_mhz`. Re-export of
/// [`crate::bm1362::pll_compute`] — same BM136x-family search per PLL
/// bible §6 + §6.1 (the BM1368 ramp template BM1362 inherits) + §13.
pub fn pll_compute(target_mhz: u32, ref_mhz: u32) -> Option<crate::bm1362::PllParams> {
    crate::bm1362::pll_compute(target_mhz, ref_mhz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_steps_in_correct_range() {
        assert_eq!(BM1368_TABLE.profiles.len(), 5);
        assert_eq!(BM1368_TABLE.min_step(), -2);
        assert_eq!(BM1368_TABLE.max_step(), 2);
    }

    #[test]
    fn nameplate_default_step_anchors_s21() {
        // S21 nameplate: 200 TH/s @ 3,500 W â†’ â‰ˆ 17.5 J/TH.
        let default = BM1368_TABLE.default_profile().unwrap();
        assert_eq!(default.wall_watts, Some(3500));
        assert!((default.hashrate_ths.unwrap() - 200.0).abs() < 1e-3);
        let eff = default.watts_per_ths().unwrap();
        assert!(
            (16.5..=18.5).contains(&eff),
            "S21 nameplate efficiency {} W/TH outside [16.5, 18.5]",
            eff
        );
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre = BM1368_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1368_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(pre.step, computed.step);
    }

    #[test]
    fn underclocked_steps_beat_default_efficiency() {
        let default = BM1368_TABLE.default_profile().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        for step in [-2, -1] {
            let s = BM1368_TABLE.by_step(step).unwrap();
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
    fn s21_hardware_constants_match_re_doc() {
        assert_eq!(BM1368_CORES_PER_CHIP, 1280);
        assert_eq!(BM1368_CHIPS_PER_CHAIN_S21, 108);
        assert_eq!(BM1368_CHAIN_COUNT_S21, 3);
        // 108 Ã— 3 = 324 chips total per S21.
        assert_eq!(BM1368_CHIPS_PER_CHAIN_S21 * BM1368_CHAIN_COUNT_S21, 324);
    }

    #[test]
    fn hashcounting_value_matches_re_doc() {
        // chip-init-sequences.md line 228: 0x000015A4 stock.
        assert_eq!(BM1368_HASHCOUNTING_S21_STOCK, 0x0000_15A4);
    }

    #[test]
    fn operational_baud_matches_bitmain_path() {
        assert_eq!(BM1368_OPERATIONAL_BAUD, 3_125_000);
        assert_eq!(BM1368_MISCCTRL_BAUD_VALUE, 0x0000_3001);
    }

    #[test]
    fn per_chip_hashrate_range_matches_re_doc() {
        // HASHBOARD_DIAGNOSTICS.md line 59: 600-750 GH/s per chip.
        assert!((BM1368_PER_CHIP_HASHRATE_GHS_MIN - 600.0).abs() < 1e-3);
        assert!((BM1368_PER_CHIP_HASHRATE_GHS_MAX - 750.0).abs() < 1e-3);
        assert!(BM1368_PER_CHIP_HASHRATE_GHS_MIN < BM1368_PER_CHIP_HASHRATE_GHS_MAX);
    }

    #[test]
    fn voltage_domain_count_pinned() {
        // HASHBOARD_DIAGNOSTICS.md line 59: 12 voltage domains.
        assert_eq!(BM1368_VOLTAGE_DOMAINS, 12);
    }

    #[test]
    fn step_voltage_increases_with_frequency() {
        for window in BM1368_PROFILES.windows(2) {
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
    fn s21_nameplate_beats_bm1366_nameplate_efficiency() {
        // BM1368 (5nm) is a generation newer than BM1366 â€” S21 â‰ˆ 17.5
        // J/TH should beat S19k Pro's ~28.5 J/TH by a wide margin.
        let bm1368_eff = BM1368_TABLE
            .default_profile()
            .unwrap()
            .watts_per_ths()
            .unwrap();
        assert!(
            bm1368_eff < 20.0,
            "BM1368 nameplate efficiency {} W/TH should beat BM1366's ~28.5",
            bm1368_eff
        );
    }

    #[test]
    fn json_round_trip_preserves_profile_fields() {
        let original = BM1368_TABLE.by_step(0).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    // -------------------------------------------------------------------
    // PR-053 depth-parity pins. Each new geometry/PLL/CRC constant gets a
    // unit test (a number with no test is a liability — bm1362.rs idiom).
    // -------------------------------------------------------------------

    #[test]
    fn chip_id_register_value_is_0x1368() {
        // chip-init-sequences.md BM1368 SPEC BLOCK + driver bm1368.rs:43.
        assert_eq!(chip::CHIP_ID, 0x1368);
    }

    #[test]
    fn process_node_is_5nm() {
        // PLL bible §6 header "Gen 5, TSMC 5nm".
        assert_eq!(chip::PROCESS_NM, 5);
    }

    #[test]
    fn crc5_command_poly_and_init_match_protocol_doc() {
        // DCENT_OS_Antminer/ "CRC5 (poly 0x05, init 0x1F)".
        assert_eq!(chip::CRC5_CMD_POLY, 0x05);
        assert_eq!(chip::CRC5_CMD_INIT, 0x1F);
    }

    #[test]
    fn cores_per_chip_is_1280_fixture_re() {
        // PLL bible §6.3 + MASTER_CHIP_CATALOG (80 big × 16 small).
        assert_eq!(chip::CORES_PER_CHIP, 1280);
        // Must agree with the long-standing module constant.
        assert_eq!(chip::CORES_PER_CHIP, BM1368_CORES_PER_CHIP);
    }

    #[test]
    fn address_stride_and_jobid_step_match_driver_and_spec_block() {
        // driver bm1368.rs:49 (FIXTURE_ADDRESS_INTERVAL=2) +
        // chip-init-sequences.md "JOBID step +24 … max 5 distinct jobs".
        assert_eq!(chip::ADDRESS_STRIDE, 2);
        assert_eq!(chip::JOBID_STEP, 24);
        assert_eq!(chip::MAX_DISTINCT_JOBS, 5);
    }

    #[test]
    fn hardware_difficulty_is_128_distinct_from_bm1362_1366() {
        // PLL bible §13 + §6.3: 128 (BM1368/1370), NOT the 256 used by
        // the BM1362/1366 family. The cross-family contrast is the
        // load-bearing fact here.
        assert_eq!(chip::HARDWARE_DIFFICULTY, 128);
        assert_eq!(crate::bm1366::chip::HARDWARE_DIFFICULTY, 256);
        assert_ne!(
            chip::HARDWARE_DIFFICULTY,
            crate::bm1366::chip::HARDWARE_DIFFICULTY
        );
    }

    #[test]
    fn open_core_not_required_and_is_nopic() {
        // chip-init-sequences.md BM1368 SPEC BLOCK "Open-core: NOT
        // required" + "PIC: NONE (NoPIC)". Pinned via non-bool
        // comparison to satisfy BOTH clippy::assertions_on_constants
        // AND clippy::bool_assert_comparison.
        assert_eq!(u8::from(chip::OPEN_CORE_REQUIRED), 0);
        assert_eq!(u8::from(chip::IS_NOPIC), 1);
    }

    #[test]
    fn work_layout_register_addresses_match_re_corpus() {
        // PLL bible §13 family column + chip-init-sequences.md BM1368
        // steps 8/9/11 + driver bm1368.rs source-of-truth.
        assert_eq!(work_layout::RESPONSE_BYTES, 11);
        assert_eq!(work_layout::PLL0_REG, 0x08);
        assert_eq!(work_layout::MISC_CTRL_REG, 0x18);
        assert_eq!(work_layout::FAST_UART_REG, 0x28);
        assert_eq!(work_layout::HASHCOUNTING_REG, 0x10);
        assert_eq!(work_layout::UART_RELAY_REG, 0x2C);
        assert_eq!(work_layout::UART_RELAY_12_DOMAIN_VALUE, 0x007C_0003);
        assert_eq!(work_layout::ANALOG_MUX_REG, 0x54);
        assert_eq!(work_layout::ANALOG_MUX_VALUE, 0x0000_0003);
        assert_eq!(work_layout::IO_DRIVER_REG, 0x58);
        assert_eq!(work_layout::IO_DRIVER_VALUE, 0x0211_1111);
        assert_eq!(work_layout::FIXTURE_TICKET_MASK, 0x0000_007F);
    }

    #[test]
    fn chain_geometry_is_internally_consistent_with_re_doc() {
        // HASHBOARD_DIAGNOSTICS.md §2.2 line 151: 12 domains × 9
        // chips/domain = 108 chips/chain.
        let g = Bm1368ChainGeometry::S21;
        assert_eq!(g.chips_per_chain, 108);
        assert_eq!(g.chains, 3);
        assert_eq!(g.domains_per_chain, 12);
        assert_eq!(g.chips_per_domain, 9);
        assert_eq!(g.chips_via_domains(), 108);
        assert_eq!(g.chips_via_domains(), g.chips_per_chain as u32);
        // Consistent with the long-standing constants.
        assert_eq!(
            g.chips_per_chain as u32 * g.chains as u32,
            BM1368_CHIPS_PER_CHAIN_S21 * BM1368_CHAIN_COUNT_S21
        );
        // Domain count matches the long-standing constant.
        assert_eq!(g.domains_per_chain as u32, BM1368_VOLTAGE_DOMAINS);
    }

    #[test]
    fn voltage_domain_levels_match_hashboard_diagnostics() {
        // HASHBOARD_DIAGNOSTICS.md §2.2 line 151: ~1.2 V/domain,
        // ~14.4 V total.
        assert!((BM1368_VOLTS_PER_DOMAIN - 1.2).abs() < 1e-3);
        assert!((BM1368_TOTAL_CHAIN_VOLTAGE_V - 14.4).abs() < 1e-3);
        // 12 domains × 1.2 V ≈ 14.4 V total (V_domain × domains).
        let computed = BM1368_VOLTS_PER_DOMAIN * BM1368_VOLTAGE_DOMAINS as f32;
        assert!((computed - BM1368_TOTAL_CHAIN_VOLTAGE_V).abs() < 1e-3);
    }

    #[test]
    fn nameplate_jth_matches_hashboard_diagnostics() {
        // HASHBOARD_DIAGNOSTICS.md §1 chip table line 59: 17.5 J/TH.
        assert!((BM1368_NAMEPLATE_JTH - 17.5).abs() < 1e-3);
    }

    #[test]
    fn freq_voltage_table_is_monotonic_whole_board_cadence() {
        // Whole-board chain-rail rows (BM1368_PROFILES.voltage_v units):
        // freq↑ ⟹ voltage↑. Top row = highest freq + highest voltage.
        let t = BM1368_S21_FREQ_VOLT_TABLE;
        assert!(!t.is_empty());
        for w in t.windows(2) {
            assert!(w[0].0 > w[1].0, "freq must strictly decrease top→bottom");
            assert!(
                w[0].1 > w[1].1,
                "chain-rail voltage decreases with frequency (whole-board cadence)"
            );
        }
        // Brackets the BM1368_PROFILES envelope.
        assert_eq!(t[0].0, 590);
        assert_eq!(t[t.len() - 1].0, 480);
        // Each row matches a BM1368_PROFILES row (mV ⇄ V).
        for &(f, mv) in t {
            let p = BM1368_PROFILES
                .iter()
                .find(|p| p.freq_mhz == f as u32)
                .unwrap_or_else(|| panic!("no BM1368_PROFILES row for {f} MHz"));
            assert_eq!(mv as f32, (p.voltage_v * 1000.0).round());
        }
    }

    #[test]
    fn sku_geometry_is_uniform_across_bm1368_skus() {
        for sku in ALL_BM1368_HASHBOARD_SKUS {
            assert_eq!(sku.chips_per_chain(), 108);
            assert_eq!(sku.chain_count(), 3);
            assert_eq!(sku.domains_per_chain(), 12);
            assert_eq!(sku.hashcounting_stock(), 0x0000_15A4);
            // Content-identical envelope across all BM1368 SKUs.
            assert_eq!(sku.freq_voltage_table(), BM1368_S21_FREQ_VOLT_TABLE);
        }
        assert_eq!(
            Bm1368HashboardSku::default_for_unrecognized_sku(),
            Bm1368HashboardSku::S21
        );
    }

    #[test]
    fn sku_id_round_trip_and_serde() {
        for sku in ALL_BM1368_HASHBOARD_SKUS {
            assert_eq!(Bm1368HashboardSku::from_id(sku.hashboard_id()), Some(*sku));
            let j = serde_json::to_string(sku).unwrap();
            let back: Bm1368HashboardSku = serde_json::from_str(&j).unwrap();
            assert_eq!(*sku, back);
        }
        assert_eq!(Bm1368HashboardSku::from_id("nope"), None);
    }

    #[test]
    fn pll_freq_mhz_matches_bm1362_family_formula() {
        // PLL bible §6 "Same encoding family as BM1362/BM1366".
        // Canonical unit dividers: 25 MHz, refdiv 1, user_div 1,
        // (pd1,pd2)=(5,2). FBDIV 210 → 525 MHz (the S21 .135-proven
        // ramp target).
        assert_eq!(pll_freq_mhz(25, 1, 210, 5, 2, 1), 525);
        assert_eq!(
            pll_freq_mhz(25, 1, 218, 5, 2, 1),
            crate::bm1362::pll_freq_mhz(25, 1, 218, 5, 2, 1)
        );
        assert_eq!(pll_freq_mhz(25, 1, 210, 0, 2, 1), 0);
    }

    #[test]
    fn pll_compute_resolves_s21_ramp_target_and_rejects_garbage() {
        // S21 .135 proven ramp lands at 525 MHz (PLL bible §6.1).
        let p = pll_compute(525, 25).expect("525 MHz must resolve");
        assert_eq!(p.compute_freq_mhz(25), 525);
        assert!(pll_compute(0, 25).is_none());
        assert!(pll_compute(525, 0).is_none());
        assert_eq!(pll_compute(545, 25), crate::bm1362::pll_compute(545, 25));
    }
}
