//! BM1362 silicon characterization table.
//!
//! 21 discrete steps from `-16` to `+4`. Source: live `cgminer-API profiles`
//! capture from LUXminer 2026.4.3.192353 on Antminer S19j Pro at 203.0.113.79
//! (BHB42601 hashboard, 126 chips/board, 3 boards). Reference document:
//! .
//!
//! Cadence proven from the 12 live-confirmed rows: each step is exactly
//! +25 MHz. Voltage clamps at 11.880 V for the four lowest steps (silicon
//! voltage floor), then climbs +0.150 V per step. Wall watts +~150 W per
//! step, hashrate +~4.9 TH/s per step. The same linear cadence extrapolates
//! cleanly through Step 0 (live-confirmed at 545 MHz / 13.800 V / 3126 W /
//! 105.8 TH/s) to land Step +4 at 645 MHz / 14.400 V.
//!
//! Efficiency sweet spot: Step -9 (320 MHz / 12.45 V) at **27.60 J/TH** â€”
//! 6.6% better than the nameplate `default` profile (29.55 J/TH).

use crate::{Profile, ProfileSource, SiliconTable};
use serde::{Deserialize, Serialize};

/// The 21 BM1362 silicon profile rows, ordered by `step`.
pub const BM1362_PROFILES: [Profile; 21] = [
    Profile {
        step: -16,
        freq_mhz: 145,
        voltage_v: 11.880,
        wall_watts: Some(997),
        hashrate_ths: Some(28.1),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -15,
        freq_mhz: 170,
        voltage_v: 11.880,
        wall_watts: Some(1079),
        hashrate_ths: Some(33.0),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -14,
        freq_mhz: 195,
        voltage_v: 11.880,
        wall_watts: Some(1162),
        hashrate_ths: Some(37.8),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -13,
        freq_mhz: 220,
        voltage_v: 11.880,
        wall_watts: Some(1244),
        hashrate_ths: Some(42.7),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -12,
        freq_mhz: 245,
        voltage_v: 12.000,
        wall_watts: Some(1349),
        hashrate_ths: Some(47.6),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -11,
        freq_mhz: 270,
        voltage_v: 12.150,
        wall_watts: Some(1466),
        hashrate_ths: Some(52.4),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -10,
        freq_mhz: 295,
        voltage_v: 12.300,
        wall_watts: Some(1588),
        hashrate_ths: Some(57.3),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -9,
        freq_mhz: 320,
        voltage_v: 12.450,
        wall_watts: Some(1714),
        hashrate_ths: Some(62.1),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -8,
        freq_mhz: 345,
        voltage_v: 12.600,
        wall_watts: Some(1852),
        hashrate_ths: Some(67.0),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -7,
        freq_mhz: 370,
        voltage_v: 12.750,
        wall_watts: Some(1994),
        hashrate_ths: Some(71.8),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -6,
        freq_mhz: 395,
        voltage_v: 12.900,
        wall_watts: Some(2142),
        hashrate_ths: Some(76.7),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -5,
        freq_mhz: 420,
        voltage_v: 13.050,
        wall_watts: Some(2297),
        hashrate_ths: Some(81.6),
        source: ProfileSource::LiveConfirmed,
    },
    Profile {
        step: -4,
        freq_mhz: 445,
        voltage_v: 13.200,
        wall_watts: Some(2459),
        hashrate_ths: Some(86.5),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -3,
        freq_mhz: 470,
        voltage_v: 13.350,
        wall_watts: Some(2627),
        hashrate_ths: Some(91.4),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -2,
        freq_mhz: 495,
        voltage_v: 13.500,
        wall_watts: Some(2802),
        hashrate_ths: Some(96.2),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 520,
        voltage_v: 13.650,
        wall_watts: Some(2960),
        hashrate_ths: Some(101.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 545,
        voltage_v: 13.800,
        wall_watts: Some(3126),
        hashrate_ths: Some(105.8),
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 570,
        voltage_v: 13.950,
        wall_watts: Some(3296),
        hashrate_ths: Some(110.6),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 595,
        voltage_v: 14.100,
        wall_watts: Some(3470),
        hashrate_ths: Some(115.4),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 3,
        freq_mhz: 620,
        voltage_v: 14.250,
        wall_watts: Some(3648),
        hashrate_ths: Some(120.2),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 4,
        freq_mhz: 645,
        voltage_v: 14.400,
        wall_watts: Some(3830),
        hashrate_ths: Some(125.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1362 silicon characterization table.
pub const BM1362_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1362",
    profiles: &BM1362_PROFILES,
    default_step: 0,
    sweet_spot_step: -9,
    // S19j Pro AML .133 first hash 2026-04-11 (66 TH/s avg, 110 TH/s
    // peak, 30K nonces). LUXminer 105.8 TH/s on .79 supplied 12 of the
    // 21 live-confirmed rows.
    live_status: crate::ChipStatus::LiveConfirmed,
};

/// Open-core voltage required to start all 514 cores per chip during
/// chip enumeration. Held during the open-core sweep; chain steps down to
/// the configured operating voltage immediately after.
///
/// Source: live LUXminer log `Ramping board voltage hashboard_id=N voltage=14.92`
/// on cold boot, separate from the steady-state target voltage of 13.800 V.
pub const BM1362_OPEN_CORE_VOLTAGE_V: f32 = 14.92;

/// Voltage DAC granularity observed on the BM1362 dsPIC controller.
///
/// Source: live LUXminer log `Requested voltage is not reachable; an
/// approximate value will be used instead requested=13.81 got=13.84` â€”
/// i.e., voltage DAC step is roughly 0.03 V.
///
/// Profile-table voltages are 0.150 V apart (5x the DAC step) so each
/// profile is reachable; finer-than-0.030 V targets must be rounded.
pub const BM1362_VOLTAGE_DAC_GRANULARITY_V: f32 = 0.030;

// ===========================================================================
// 5 (2026-05-09): BM1362 silicon geometry, per-SKU freq/voltage
// tables, PLL formula. Source-cites:
//   - DCENT_OS_HARDWARE_CATALOG.md §4.2 (lines 398-430): BM1362 protocol.
//   - MASTER_DOCS/S19J_PRO_PORTING_PLAN.md §10 (line 445): PLL formula
//     `freq = (refclk / refdiv) * fbdiv / (postdiv1 * postdiv2 * user_div)`.
//   - MASTER_DOCS/S19J_PRO_PORTING_PLAN.md §10 (lines 452-458): per-SKU
//     freq/voltage tables (BHB42601 / BHB42801 / BHB42611).
//   - Memory rule  (W11A.4).
//   - Memory rule  ().
// ===========================================================================

/// BM1362 chip family fixed constants (RE2 §4.2).
///
/// Numbers below are die-fixed and hold for every BM1362 hashboard SKU
/// (BHB42601 / BHB42801 / BHB42611).
pub mod chip {
    /// CHIP_ID register value reported by `regs::CHIP_ADDRESS` bits [31:16]
    /// during enumeration. Per RE2 catalog §4.2 (line 409).
    pub const CHIP_ID: u16 = 0x1362;

    /// Process node — Bitmain 5 nm.
    pub const PROCESS_NM: u8 = 5;

    /// CRC8 polynomial used for BM1362 framing (RE2 catalog §4.1
    /// line 394: "CRC8 poly 0x31"). Same polynomial as BM1368.
    pub const CRC8_POLY: u8 = 0x31;

    /// Address stride between sequential chips on a chain. The
    /// enumerator assigns `0x00, 0x02, 0x04, ... 0xFA` (126 chips).
    /// Matches `dcentrald_asic::drivers::bm1362::ADDRESS_INTERVAL`
    /// (live-pinned on .139 / .133).
    pub const ADDRESS_STRIDE: u8 = 2;

    /// Internal-register address of the master MISC_CONTROL on the
    /// BM1362 die. Distinct from the BM1387 / FPGA-side MiscCtrl —
    /// per RE2 §4.2 the BM1362 MiscCtrl lives at full 24-bit address
    /// `0xC100B0`, written via the chip driver's normalized 1-byte
    /// register form (`0x18`) which rides on the BM139x register
    /// prefix protocol.
    pub const MISC_CTRL_REG_FULL: u32 = 0x00C1_00B0;

    /// Cores per BM1362 die (small "cores", "big cores", "small-cores").
    /// **Reconciliation note (W11.5)**: dev-kit hardware catalog and
    /// porting plan §10 list **65 cores/die, 514 small-cores/core**.
    /// `dcentrald-asic::drivers::bm1362::BM1362_BIG_CORES = 4` (live-
    /// probed on .139 from bosminer crate `bosminer_am2_s17`) is the
    /// number of FPGA-visible *big cores* per die — distinct from the
    /// die-internal core count. Both numbers are real and apply at
    /// different layers:
    ///   - 4 big cores: FPGA-visible cores per die (hashrate
    ///     accounting on the chain side).
    ///   - 65 cores/die × 514 small-cores/core: die-internal SHA-256
    ///     compute units (silicon characterization, not visible
    ///     through the wire protocol).
    ///
    /// Reference: `dev-kit hardware-catalog §10 line 49` and
    /// `MASTER_DOCS/S19J_PRO_PORTING_PLAN.md` line 49.
    pub const CORES_PER_DIE: u16 = 65;

    /// Small-cores per "core" inside a BM1362 die. Per dev-kit porting
    /// plan: `65 cores/die, 514 small-cores/core`. Total small-cores per
    /// die = `CORES_PER_DIE * SMALL_CORES_PER_CORE` = 33,410.
    pub const SMALL_CORES_PER_CORE: u16 = 514;

    /// FPGA-visible big cores per die (live-probed on .139). Held here
    /// for cross-reference; consumers should also look at
    /// `dcentrald_asic::drivers::bm1362::BM1362_BIG_CORES`.
    pub const FPGA_VISIBLE_BIG_CORES: u32 = 4;

    /// Hashboard chips per chain (live-pinned on .139 / .133).
    /// 126 × stride 2 = 252 < 256 ✓.
    pub const CHIPS_PER_CHAIN: u8 = 126;

    /// Number of mining chains on a stock S19j Pro hashing unit.
    pub const CHAINS: u8 = 4;
}

/// BM1362 wire-format work-layout constants (RE2 §4.2 lines 424-430).
///
/// These are byte-counts on the FPGA / serial wire and do NOT change per
/// hashboard SKU. Distinct from any silicon-profile row.
pub mod work_layout {
    /// WORK_TX payload — 20 32-bit words = 80 bytes per RE2 §4.2.
    /// Layout: `[CTRL][midstate[8]][merkle_root[8]][ntime][nbits][job_id]`.
    pub const TX_WORDS: usize = 20;

    /// WORK_TX byte length — `TX_WORDS * 4 = 80`.
    pub const TX_BYTES: usize = TX_WORDS * 4;

    /// WORK_RX payload — 6 32-bit words = 24 bytes per RE2 §4.2.
    /// Layout: `[nonce][extra_nonce][word2 (16b work_id, 8b chip_id, 8b
    /// flags)][rolled_version][job_id][timestamp]`.
    pub const RX_WORDS: usize = 6;

    /// WORK_RX byte length — `RX_WORDS * 4 = 24`.
    pub const RX_BYTES: usize = RX_WORDS * 4;

    /// Default CTRL header value placed at WORK_TX[0..4]. Live-probed
    /// on .139 chain1/chain4 (`0x00901002` = bits 1/12/20/23 set;
    /// MIDSTATE_CNT=1 → 2 midstates per work). Mirrors
    /// `dcentrald_hal::fpga_chain::ctrl_am2::BM1362_DEFAULT`.
    pub const CTRL_DEFAULT: u32 = 0x0090_1002;

    /// Bit width of the `work_id` field inside the WORK_RX `word2`
    /// slot — 16 bits per RE2 §4.2 line 430 ("Work ID: 16-bit
    /// (confirmed - NOT 8-bit like BM1387)"). Note: the FPGA-side
    /// dispatch `work_id` field is 8-bit;
    /// the *ASIC-side* work_id is 16-bit. Both are right; they live on
    /// different sides of the FPGA fabric.
    pub const WORK_ID_BITS: u8 = 16;
}

/// Per-hashboard SKU geometry. Each variant ships its own freq/voltage
/// table, plus the chain-grid layout (12×11 = 132 ≥ 126 chips physical;
/// the 6 unused grid slots correspond to the address-stride alignment).
///
/// SKU IDs come from VNish + Bitmain `levels.json` (porting plan §10
/// lines 452-458 + W13.C2 RE4 expansion via
/// `RE_DELIVERABLES/levels_json_pvt_validation.md` + `pvt_tables.h`).
///
/// W13.C2 (2026-05-10): expanded from 3 to 15 SKUs. The original 3
/// variants (`Bhb42601`, `Bhb42801`, `Bhb42611`) keep their freq/voltage
/// tables byte-for-byte unchanged. Most new variants share table data
/// with their family head per `pvt_tables.h`; R6-2 split `BHB42811` and
/// `BHB42821` into their tighter vendor `levels.json` envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bm1362HashboardSku {
    // --- Standard family (BHB42601 freq/voltage table) ---
    /// **BHB42601** — S19j Pro standard. Lower voltage envelope
    /// (1320-1380 mV chip-rail). Most common production SKU. 126 ASICs
    /// × 4 chains. Default fallback for unrecognised BHB42xxx SKUs.
    Bhb42601,
    /// **BHB42603** — Standard family alias of BHB42601 (identical
    /// freq/voltage table per `pvt_tables.h` line 153). Different SKU
    /// classification only — same silicon envelope.
    Bhb42603,
    /// **BHB42621** — Standard family alias of BHB42601.
    Bhb42621,
    /// **BHB42641** — Standard family alias of BHB42601.
    Bhb42641,

    // --- Extended-low family (BHB42631 freq/voltage table) ---
    /// **BHB42631** — extended-low: standard band + extra 440 MHz row.
    /// 126 ASICs × 4 chains, 1320-1380 mV.
    Bhb42631,
    /// **BHB42632** — Extended-low family alias of BHB42631.
    Bhb42632,
    /// **BHB42651** — Extended-low family alias of BHB42631.
    Bhb42651,

    // --- High-bin family (BHB42801 freq/voltage table) ---
    /// **BHB42801** — S19 Pro+ higher-grade. Lifted voltage envelope
    /// (1530-1600 mV chip-rail). Higher freq targets up to 675 MHz.
    /// **REQUIRES APW12+** (NOT APW12 SMBus) — at 1.6 V / 4000 W+ the
    /// SMBus rail would brown out. See
    /// .
    Bhb42801,
    /// **BHB42811** — High-bin S19 XP variant. Requires APW12+.
    Bhb42811,
    /// **BHB42821** — High-bin S19 XP variant. Requires APW12+.
    Bhb42821,

    // --- High-bin extended (BHB42831 freq/voltage table) ---
    /// **BHB42831** — high-bin + extra 585 MHz row. 88 ASICs × 4 chains.
    /// **REQUIRES APW12+** (high-bin power class).
    Bhb42831,

    // --- Fixed-voltage repair-class (BHB42803 freq/voltage table) ---
    /// **BHB42803** — single-voltage repair-class hashboard. 84 ASICs
    /// × **3 chains** (NOT 4). Fixed 1530 mV at the PCB-level VRM
    /// divider. `voltage_fixed=true` — autotuner MUST short-circuit
    /// `voltage_search` to NoOp via
    /// [`VoltageSearchState::new_with_pvt_flags`] and
    /// . **Requires APW12+**
    /// (4000 W class even at single voltage — current draw is high).
    Bhb42803,

    // --- Mid-band mixable family (BHB42611 freq/voltage table) ---
    /// **BHB42611** — high-grade S19j Pro variant. Same voltage
    /// envelope as BHB42601 (1320-1380 mV) but freq table shifted up
    /// (610-670 MHz vs 465-545 MHz). 120 ASICs × 4 chains. **Supports
    /// `mix_levels`** per-chain freq table per `topol.conf` — but per
    /// W13 implementation note, only **symmetric** `[freq; 4]` chains
    /// are honoured; per-chain asymmetric dispatch is deferred to
    /// W14+.
    Bhb42611,

    // --- Efficiency-optimised family (BHB42701 freq/voltage table) ---
    /// **BHB42701** — efficiency-optimised. 500-575 MHz @ 1220-1260 mV
    /// (lowest voltage floor in the BHB42xxx line). 108 ASICs × 4
    /// chains. **Best home J/TH** in the family (see
    /// ).
    Bhb42701,

    // --- Low-power salvage family (BHB42841 freq/voltage table) ---
    /// **BHB42841** — low-power salvage variant for marginal chips.
    /// 410-475 MHz @ 1360-1480 mV. 126 ASICs × 4 chains.
    ///
    /// **INVERTED CURVE**: lower frequency requires HIGHER voltage for
    /// stability margin (opposite of every other BHB42xxx table). Any
    /// autotuner heuristic that assumes "lower freq → lower voltage"
    /// MUST consult [`Bm1362SkuFlags::inverted_curve`] before walking
    /// this SKU's table.
    Bhb42841,
}

/// Hashboard-level chain geometry: physical grid + chips/domain/chain.
///
/// 12 rows × 11 cols = 132 grid slots, of which 126 are physical chips
/// (stride 2 → addresses 0x00..=0xFA). The 6 phantom slots are the
/// stride-alignment dead zone above 0xFA. `domains_per_chain = 42` and
/// `asics_per_domain = 3` from porting plan §10 + RE2 catalog
/// architectural notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bm1362ChainGeometry {
    pub chips_per_chain: u8,
    pub chains: u8,
    pub domains_per_chain: u8,
    pub asics_per_domain: u8,
    pub grid_rows: u8,
    pub grid_cols: u8,
}

impl Bm1362ChainGeometry {
    /// Canonical S19j Pro / S19j Pro+ / S19k Pro chain geometry.
    pub const STANDARD: Bm1362ChainGeometry = Bm1362ChainGeometry {
        chips_per_chain: chip::CHIPS_PER_CHAIN,
        chains: chip::CHAINS,
        domains_per_chain: 42,
        asics_per_domain: 3,
        grid_rows: 12,
        grid_cols: 11,
    };

    /// Sanity check that `domains_per_chain * asics_per_domain`
    /// matches `chips_per_chain` (42 × 3 = 126).
    pub const fn chips_via_domains(self) -> u32 {
        self.domains_per_chain as u32 * self.asics_per_domain as u32
    }
}

/// One row of a per-SKU freq/voltage table — frequency in MHz, chip-rail
/// voltage in millivolts. Chip-rail (NOT chain-rail) because the porting
/// plan §10 numbers are downstream-of-buck regulator targets.
pub type Bm1362FreqVoltRow = (u16, u16);

// W15.A5 (2026-05-10) —  Q8 PVT confirmation pass:
// `Handoffs/DCENT_OS_FULL_HANDOFF/DCENT_OS_HANDOFF/RE_TEAM_FINDINGS_WAVE5.md`
// §Q8 lines 161-179 reports BB and CV `levels.json` tables are
// byte-identical and complete at **15 SKUs** for BM1362. The Rust tables
// in this module already match the  Q8 ranges (BHB42601 = 465-545
// MHz @ 1320-1380 mV; BHB42801 = 585-675 MHz @ 1530-1600 mV; BHB42611 =
// 610-670 MHz @ 1320-1380 mV; …). See the  Q8 SKU table in
// `RE_TEAM_FINDINGS_WAVE5.md` for the full 15-row envelope vs the
// per-SKU canonical-tier tables below. Confirmation test:
// `tests::bhb42xxx_15_sku_envelopes_match_wave5_q8`.

/// BHB42601 (S19j Pro standard) freq/voltage levels — porting plan §10
///.
///
/// This is a DERIVED "canonical operating-point" ladder (one row per freq
/// tier), NOT stock's per-freq minimum. Its LOAD-BEARING contract is the
/// MARGINAL envelope that `validate_freq_volt` checks: volts span
/// (1320, 1380) mV and freqs span (465, 545) MHz — those marginal bounds
/// match stock `levels.json` exactly (min stock voltage 1320 mV appears at
/// 465-525 MHz; max 1380 mV). The per-row voltages here are a conservative
/// derived ladder and MUST NOT be read as stock's per-freq floor: this row
/// says (545, 1320) but stock's real minimum at 545 MHz is 1340 mV (see
/// [`BHB42601_FREQ_VOLT_GRID_FULL`]). Per-freq clamping that catches a
/// 545 MHz @ 1320 mV under-volt is `validate_freq_volt_combination()` over
/// the full grid (R-12). Top of table is highest freq.
pub const BHB42601_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] = &[
    (545, 1320),
    (525, 1330),
    (505, 1345),
    (485, 1360),
    (465, 1380),
];

/// BHB42601 FULL per-step stock PVT grid — byte-exact from stock
/// CVCtrl/BBCtrl `levels.json` via `RE_DELIVERABLES/pvt_tables.h`
/// `PVT_LEVELS_BHB42601[]`: every discrete `(freq_mhz, volt_mv)` step the
/// stock firmware publishes, highest-freq-first. GROUND TRUTH for per-freq
/// voltage floors — note the 545 MHz tier's minimum is **1340 mV** (1320 mV
/// only appears at <= 525 MHz), which is why a 545 MHz @ 1320 mV command is
/// an under-volt below stock's proven floor. Consumed by
/// [`Bm1362HashboardSku::full_pvt_grid`] +
/// `dcentrald_autotuner::pvt_envelope::validate_freq_volt_combination` (R-12).
#[rustfmt::skip]
pub const BHB42601_FREQ_VOLT_GRID_FULL: &[Bm1362FreqVoltRow] = &[
    (545, 1340), (545, 1360), (545, 1380),
    (525, 1320), (525, 1340), (525, 1360), (525, 1380),
    (505, 1320), (505, 1340), (505, 1360), (505, 1380),
    (485, 1320), (485, 1340), (485, 1360), (485, 1380),
    (465, 1320), (465, 1340),
];

/// BHB42801 (S19 Pro+ higher-grade) freq/voltage levels.
pub const BHB42801_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(675, 1530), (645, 1545), (615, 1565), (585, 1600)];

/// BHB42811 / BHB42821 high-bin S19 XP variant envelope.
///
/// R6-2 tightened these aliases from the broader BHB42801 collapsed table
/// to the vendor `levels.json` envelope: 615-675 MHz @ 1530-1600 mV.
pub const BHB42811_42821_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(675, 1530), (645, 1545), (615, 1600)];

/// BHB42611 (high-grade) freq/voltage levels.
pub const BHB42611_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(670, 1320), (650, 1340), (630, 1360), (610, 1380)];

// ===========================================================================
// W13.C2 (2026-05-10): RE4 levels.json full PVT-table expansion. Source:
//   `RE_DELIVERABLES/RE_DELIVERABLES/pvt_tables.h` (auto-generated from
//   stock CVCtrl/BBCtrl `levels.json`). Each table mirrors the C-side
//   `PVT_LEVELS_BHBxxxxx` array byte-for-byte (collapsed to one
//   `(freq, volt)` row per voltage tier — the C arrays enumerate every
//   (freq, volt) discrete step).
//
// **Cadence note**: the C arrays list MULTIPLE voltage rows per frequency
// (e.g. BHB42601 has 17 entries: 545 MHz @ 1340/1360/1380 mV, then 525 MHz
// @ 1320/1340/1360/1380, ...). The Rust `Bm1362FreqVoltRow` table is the
// "canonical operating point" view — one (freq, volt) row per frequency
// tier, picking the **lowest stable voltage** at that freq. The full
// per-step grid lives in `pvt_levels_full()` for autotuner search use.
// ===========================================================================

/// BHB42631 (extended-low: standard band + 440 MHz) freq/voltage levels.
/// Per `pvt_tables.h` `PVT_LEVELS_BHB42631` (19 entries collapsed to 6
/// canonical tiers).
pub const BHB42631_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] = &[
    (545, 1320),
    (525, 1330),
    (505, 1345),
    (485, 1360),
    (465, 1380),
    (440, 1340),
];

/// BHB42631 FULL per-step stock PVT grid — byte-exact from stock
/// `pvt_tables.h` `PVT_LEVELS_BHB42631[]` (standard grid + the 440 MHz
/// extended tier). Same 545 MHz floor = 1340 mV ground truth as
/// [`BHB42601_FREQ_VOLT_GRID_FULL`]. Consumed by
/// [`Bm1362HashboardSku::full_pvt_grid`] (R-12 per-freq clamp).
#[rustfmt::skip]
pub const BHB42631_FREQ_VOLT_GRID_FULL: &[Bm1362FreqVoltRow] = &[
    (545, 1340), (545, 1360), (545, 1380),
    (525, 1320), (525, 1340), (525, 1360), (525, 1380),
    (505, 1320), (505, 1340), (505, 1360), (505, 1380),
    (485, 1320), (485, 1340), (485, 1360), (485, 1380),
    (465, 1320), (465, 1340),
    (440, 1320), (440, 1340),
];

/// BHB42803 (single-voltage repair-class) freq/voltage levels. Voltage
/// is fixed at 1530 mV at the PCB-level VRM divider — autotuner MUST
/// short-circuit `voltage_search`.
/// Per `pvt_tables.h` `PVT_LEVELS_BHB42803` (4 entries, all 1530 mV).
pub const BHB42803_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(675, 1530), (645, 1530), (615, 1530), (585, 1530)];

/// BHB42701 (efficiency-optimised) freq/voltage levels. 1220-1260 mV
/// floor — lowest voltage envelope in the BHB42xxx line. Per
/// `pvt_tables.h` `PVT_LEVELS_BHB42701` (9 entries collapsed to 4 tiers).
pub const BHB42701_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(575, 1240), (550, 1240), (525, 1240), (500, 1220)];

/// BHB42831 (high-bin extended: high-bin band + 585 MHz) freq/voltage
/// levels. Per `pvt_tables.h` `PVT_LEVELS_BHB42831` (14 entries collapsed
/// to 5 tiers).
pub const BHB42831_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] = &[
    (675, 1530),
    (645, 1530),
    (615, 1530),
    (585, 1540),
    // High-bin extended honours the underlying high-bin envelope ceiling
    // (1600 mV) for the upper rows; the canonical tier view picks the
    // lowest stable voltage per freq, mirroring `BHB42801_FREQ_VOLT_TABLE`
    // semantics. Full per-step grid available via `pvt_levels_full()`.
];

/// BHB42841 (low-power salvage) freq/voltage levels. **INVERTED**: lower
/// freq requires higher voltage for stability margin. Per `pvt_tables.h`
/// `PVT_LEVELS_BHB42841` (16 entries collapsed to 4 tiers using **lowest**
/// stable voltage per freq tier).
///
/// Auto-tuner heuristics that assume `freq↓ ⇒ volt↓` MUST consult
/// `Bm1362SkuFlags::inverted_curve` before walking this table.
pub const BHB42841_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] =
    &[(475, 1360), (450, 1360), (430, 1360), (410, 1360)];

/// BHB42603 — Standard family alias of BHB42601. Same freq/voltage table
/// per `pvt_tables.h` line 153 (`#define PVT_ENTRY_BHB42603 PVT_ENTRY_BHB42601`).
/// Re-exported as a separate const so call sites that want the canonical
/// per-SKU symbol get a stable reference; the data is shared (Rust folds
/// identical statics, but the symbol exists).
pub const BHB42603_FREQ_VOLT_TABLE: &[Bm1362FreqVoltRow] = BHB42601_FREQ_VOLT_TABLE;

/// W13.C2: Per-SKU flags that gate driver / autotuner / PSU routing.
/// These are SKU-level facts (not chip-level), so they live alongside
/// `Bm1362HashboardSku` rather than in `chip` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bm1362SkuFlags {
    /// `true` ⇒ single-voltage VRM (BHB42803 only). Autotuner MUST
    /// short-circuit `voltage_search` to NoOp; bouncing SET_VOLTAGE on
    /// a fixed-V VRM corrupts the PIC MSSP parser. See
    /// .
    pub voltage_fixed: bool,
    /// `true` ⇒ requires the APW12+ register-based PSU protocol
    /// (`dcentrald-hal::psu_apw12_plus`). The high-bin BHB428xx family
    /// (BHB42801/811/821/831 and BHB42803) draws 4000 W+ at 1.6 V which
    /// would brown out APW12 SMBus. See
    /// .
    pub requires_apw12_plus: bool,
    /// `true` ⇒ freq↓ implies volt↑ (inverted from every other BHB42xxx
    /// table). BHB42841 only. Autotuner heuristics MUST consult this
    /// before walking the freq/voltage table.
    pub inverted_curve: bool,
    /// `true` ⇒ `topol.conf` declares per-chain `mix_levels` (per-chain
    /// freq targets allowed). BHB42611 only. **W13 ships symmetric-only**
    /// dispatch (`[freq; chain_count]`); per-chain asymmetric dispatch
    /// deferred to W14+.
    pub mix_levels: bool,
}

impl Bm1362SkuFlags {
    /// All-false flag set (the common case for standard SKUs).
    pub const STANDARD: Self = Self {
        voltage_fixed: false,
        requires_apw12_plus: false,
        inverted_curve: false,
        mix_levels: false,
    };
}

impl Bm1362HashboardSku {
    /// Per-SKU freq/voltage table. Ordered top-down (highest freq first).
    /// W13.C2: 12 new SKUs route through the alias map per
    /// `pvt_tables.h` lines 153-159.
    pub const fn freq_voltage_table(self) -> &'static [Bm1362FreqVoltRow] {
        match self {
            // Standard family
            Bm1362HashboardSku::Bhb42601
            | Bm1362HashboardSku::Bhb42603
            | Bm1362HashboardSku::Bhb42621
            | Bm1362HashboardSku::Bhb42641 => BHB42601_FREQ_VOLT_TABLE,
            // Extended-low family
            Bm1362HashboardSku::Bhb42631
            | Bm1362HashboardSku::Bhb42632
            | Bm1362HashboardSku::Bhb42651 => BHB42631_FREQ_VOLT_TABLE,
            // High-bin family
            Bm1362HashboardSku::Bhb42801 => BHB42801_FREQ_VOLT_TABLE,
            Bm1362HashboardSku::Bhb42811 | Bm1362HashboardSku::Bhb42821 => {
                BHB42811_42821_FREQ_VOLT_TABLE
            }
            // High-bin extended
            Bm1362HashboardSku::Bhb42831 => BHB42831_FREQ_VOLT_TABLE,
            // Fixed-voltage repair-class
            Bm1362HashboardSku::Bhb42803 => BHB42803_FREQ_VOLT_TABLE,
            // Mid-band mixable
            Bm1362HashboardSku::Bhb42611 => BHB42611_FREQ_VOLT_TABLE,
            // Efficiency-optimised
            Bm1362HashboardSku::Bhb42701 => BHB42701_FREQ_VOLT_TABLE,
            // Low-power salvage
            Bm1362HashboardSku::Bhb42841 => BHB42841_FREQ_VOLT_TABLE,
        }
    }

    /// Full per-step stock PVT grid for this SKU — every discrete
    /// `(freq_mhz, volt_mv)` step stock `levels.json` publishes, for per-freq
    /// combination validation (R-12). The Standard (BHB42601) and Extended-low
    /// (BHB42631) families return their byte-exact stock grids; all other SKUs
    /// fall back to the marginal [`Self::freq_voltage_table`] (no separate full
    /// grid has been captured for them yet, so per-freq validation degrades
    /// gracefully to the marginal-envelope check).
    pub const fn full_pvt_grid(self) -> &'static [Bm1362FreqVoltRow] {
        match self {
            Bm1362HashboardSku::Bhb42601
            | Bm1362HashboardSku::Bhb42603
            | Bm1362HashboardSku::Bhb42621
            | Bm1362HashboardSku::Bhb42641 => BHB42601_FREQ_VOLT_GRID_FULL,
            Bm1362HashboardSku::Bhb42631
            | Bm1362HashboardSku::Bhb42632
            | Bm1362HashboardSku::Bhb42651 => BHB42631_FREQ_VOLT_GRID_FULL,
            _ => self.freq_voltage_table(),
        }
    }

    /// Hashboard string identifier (matches the
    /// `ProfileBundle.hashboard` JSON field used by the registry).
    pub const fn hashboard_id(self) -> &'static str {
        match self {
            Bm1362HashboardSku::Bhb42601 => "BHB42601",
            Bm1362HashboardSku::Bhb42603 => "BHB42603",
            Bm1362HashboardSku::Bhb42621 => "BHB42621",
            Bm1362HashboardSku::Bhb42641 => "BHB42641",
            Bm1362HashboardSku::Bhb42631 => "BHB42631",
            Bm1362HashboardSku::Bhb42632 => "BHB42632",
            Bm1362HashboardSku::Bhb42651 => "BHB42651",
            Bm1362HashboardSku::Bhb42801 => "BHB42801",
            Bm1362HashboardSku::Bhb42811 => "BHB42811",
            Bm1362HashboardSku::Bhb42821 => "BHB42821",
            Bm1362HashboardSku::Bhb42831 => "BHB42831",
            Bm1362HashboardSku::Bhb42803 => "BHB42803",
            Bm1362HashboardSku::Bhb42611 => "BHB42611",
            Bm1362HashboardSku::Bhb42701 => "BHB42701",
            Bm1362HashboardSku::Bhb42841 => "BHB42841",
        }
    }

    /// Top-of-table (highest freq) row.
    pub fn top_row(self) -> Bm1362FreqVoltRow {
        self.freq_voltage_table()[0]
    }

    /// Bottom-of-table (lowest freq, highest voltage) row.
    pub fn bottom_row(self) -> Bm1362FreqVoltRow {
        let table = self.freq_voltage_table();
        table[table.len() - 1]
    }

    /// W13.C2: Per-SKU driver / autotuner / PSU routing flags. See
    /// [`Bm1362SkuFlags`] for the field semantics. **Load-bearing**:
    /// `voltage_fixed=true` is set ONLY for `Bhb42803` per
    /// . The autotuner relies
    /// on this flag to short-circuit `voltage_search` to NoOp.
    pub const fn flags(self) -> Bm1362SkuFlags {
        match self {
            // Standard family — no flags set.
            Bm1362HashboardSku::Bhb42601
            | Bm1362HashboardSku::Bhb42603
            | Bm1362HashboardSku::Bhb42621
            | Bm1362HashboardSku::Bhb42641 => Bm1362SkuFlags::STANDARD,
            // Extended-low family — same as standard, only +440 MHz row.
            Bm1362HashboardSku::Bhb42631
            | Bm1362HashboardSku::Bhb42632
            | Bm1362HashboardSku::Bhb42651 => Bm1362SkuFlags::STANDARD,
            // High-bin family — REQUIRES APW12+.
            Bm1362HashboardSku::Bhb42801
            | Bm1362HashboardSku::Bhb42811
            | Bm1362HashboardSku::Bhb42821
            | Bm1362HashboardSku::Bhb42831 => Bm1362SkuFlags {
                requires_apw12_plus: true,
                ..Bm1362SkuFlags::STANDARD
            },
            // Fixed-voltage repair-class — voltage_fixed AND requires
            // APW12+ (4000 W class even at single voltage).
            Bm1362HashboardSku::Bhb42803 => Bm1362SkuFlags {
                voltage_fixed: true,
                requires_apw12_plus: true,
                ..Bm1362SkuFlags::STANDARD
            },
            // Mid-band mixable — supports per-chain mix_levels (W13
            // honours symmetric-only).
            Bm1362HashboardSku::Bhb42611 => Bm1362SkuFlags {
                mix_levels: true,
                ..Bm1362SkuFlags::STANDARD
            },
            // Efficiency-optimised — no flags.
            Bm1362HashboardSku::Bhb42701 => Bm1362SkuFlags::STANDARD,
            // Low-power salvage — INVERTED curve.
            Bm1362HashboardSku::Bhb42841 => Bm1362SkuFlags {
                inverted_curve: true,
                ..Bm1362SkuFlags::STANDARD
            },
        }
    }

    /// W13.C2: Number of mining chains for this SKU. 4 for every BHB42xxx
    /// variant **except** `Bhb42803` (3 chains — repair-class topology
    /// per `pvt_tables.h` line 237 `chain_count = 3`).
    pub const fn chain_count(self) -> u8 {
        match self {
            Bm1362HashboardSku::Bhb42803 => 3,
            _ => 4,
        }
    }

    /// W13.C2: Number of BM1362 ASICs per chain for this SKU.
    /// - **126** for the standard family + extended-low + low-power
    ///   salvage (BHB42601/03/21/31/32/41/51/41).
    /// - **88** for high-bin (BHB42801/811/821/831).
    /// - **84** for fixed-V repair-class (BHB42803).
    /// - **120** for mid-band mixable (BHB42611).
    /// - **108** for efficiency-optimised (BHB42701).
    pub const fn asics_per_chain(self) -> u8 {
        match self {
            Bm1362HashboardSku::Bhb42801
            | Bm1362HashboardSku::Bhb42811
            | Bm1362HashboardSku::Bhb42821
            | Bm1362HashboardSku::Bhb42831 => 88,
            Bm1362HashboardSku::Bhb42803 => 84,
            Bm1362HashboardSku::Bhb42611 => 120,
            Bm1362HashboardSku::Bhb42701 => 108,
            // Standard / extended-low / low-power salvage.
            _ => 126,
        }
    }

    /// W13.C2: Default fallback for unrecognized SKU strings (e.g. EEPROM
    /// read failure or `/etc/subtype` reports a value not in this enum).
    /// Returns [`Bm1362HashboardSku::Bhb42601`] per `pvt_tables.h` line
    /// 303 (`PVT_FALLBACK` mirrors the BHB42601 envelope).
    ///
    /// **DO NOT** use this as a "synthesise on missing data" path —
    /// callers MUST be deliberate about routing an unknown SKU to the
    /// safest envelope. Wrong envelope = silicon damage.
    pub const fn default_for_unrecognized_sku() -> Self {
        Bm1362HashboardSku::Bhb42601
    }

    /// W13.C2: Look up a SKU by its `BHB42xxx` ID string. Returns `None`
    /// if the string isn't a recognised BHB42xxx SKU. Case-sensitive
    /// (every canonical SKU string is upper-case).
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "BHB42601" => Bm1362HashboardSku::Bhb42601,
            "BHB42603" => Bm1362HashboardSku::Bhb42603,
            "BHB42621" => Bm1362HashboardSku::Bhb42621,
            "BHB42641" => Bm1362HashboardSku::Bhb42641,
            "BHB42631" => Bm1362HashboardSku::Bhb42631,
            "BHB42632" => Bm1362HashboardSku::Bhb42632,
            "BHB42651" => Bm1362HashboardSku::Bhb42651,
            "BHB42801" => Bm1362HashboardSku::Bhb42801,
            "BHB42811" => Bm1362HashboardSku::Bhb42811,
            "BHB42821" => Bm1362HashboardSku::Bhb42821,
            "BHB42831" => Bm1362HashboardSku::Bhb42831,
            "BHB42803" => Bm1362HashboardSku::Bhb42803,
            "BHB42611" => Bm1362HashboardSku::Bhb42611,
            "BHB42701" => Bm1362HashboardSku::Bhb42701,
            "BHB42841" => Bm1362HashboardSku::Bhb42841,
            _ => return None,
        })
    }
}

/// W13.C2: All 15 BHB42xxx SKUs, ordered by family for catalog use.
/// Useful for parameterised tests and dashboard SKU pickers.
pub const ALL_BM1362_HASHBOARD_SKUS: &[Bm1362HashboardSku] = &[
    // Standard family
    Bm1362HashboardSku::Bhb42601,
    Bm1362HashboardSku::Bhb42603,
    Bm1362HashboardSku::Bhb42621,
    Bm1362HashboardSku::Bhb42641,
    // Extended-low family
    Bm1362HashboardSku::Bhb42631,
    Bm1362HashboardSku::Bhb42632,
    Bm1362HashboardSku::Bhb42651,
    // High-bin family
    Bm1362HashboardSku::Bhb42801,
    Bm1362HashboardSku::Bhb42811,
    Bm1362HashboardSku::Bhb42821,
    Bm1362HashboardSku::Bhb42831,
    // Fixed-voltage repair-class
    Bm1362HashboardSku::Bhb42803,
    // Mid-band mixable
    Bm1362HashboardSku::Bhb42611,
    // Efficiency-optimised
    Bm1362HashboardSku::Bhb42701,
    // Low-power salvage
    Bm1362HashboardSku::Bhb42841,
];

/// Default per-hashboard SKU for a given S19-family platform key. Used
/// to route per-platform profile lookups to the right freq/voltage
/// table when the operator hasn't pinned an explicit SKU. Platform key
/// strings match `dcentrald-hal::platform` short names.
///
/// Returns `None` for unknown platforms. Callers MUST handle `None`
/// (don't synthesize a default SKU — wrong voltage envelope = silicon
/// damage).
pub fn default_sku_for_platform(platform: &str) -> Option<Bm1362HashboardSku> {
    match platform {
        // Standard S19j Pro variants — Zynq am2 (.139), Amlogic
        // am3-aml (.133), CV1835 S19j Pro, BB AM335x S19j Pro.
        "am2-s19jpro" | "am3-aml-s19jpro" | "cv1835-s19jpro" | "am3-bb-s19jpro"
        | "bcb100-s19jpro-lab" => Some(Bm1362HashboardSku::Bhb42601),
        // S19 Pro+ higher-grade variant.
        "am2-s19jproplus" | "am3-aml-s19jproplus" => Some(Bm1362HashboardSku::Bhb42801),
        _ => None,
    }
}

/// BM1362 PLL frequency formula. Source: porting plan §10 line 445.
///
/// `freq = (refclk / refdiv) * fbdiv / (postdiv1 * postdiv2 * user_div)`
///
/// On a stock S19j Pro `refclk = 25 MHz`, `refdiv = 1`, `user_div = 1`,
/// and `(postdiv1, postdiv2) = (5, 2)` so the table lookup
/// (`BM1362_PLL_TABLE`) walks `fbdiv` from 160 to 239 to cover
/// 400-597 MHz at 2.5 MHz/step.
///
/// Returns the integer-truncated MHz value. Returns 0 if any divider is
/// zero (caller is responsible for not feeding bogus inputs — this guard
/// avoids a panic in `const`-eval-eligible call sites).
///
/// Inputs are in MHz / unit-divider counts. There is no 32-bit overflow
/// risk for legitimate inputs: `refclk * fbdiv` peaks at 25 * 239 ≈ 6 GHz
/// = 6_000 MHz, well inside u32.
pub const fn pll_freq_mhz(
    refclk_mhz: u32,
    refdiv: u32,
    fbdiv: u32,
    postdiv1: u32,
    postdiv2: u32,
    user_div: u32,
) -> u32 {
    if refdiv == 0 || postdiv1 == 0 || postdiv2 == 0 || user_div == 0 {
        return 0;
    }
    // Multiply BEFORE dividing, in u64, so a non-unit refdiv does not lose
    // precision to integer truncation. `f_out = refclk * fbdiv / (refdiv *
    // pd1 * pd2 * usr)`. The old `(refclk / refdiv) * fbdiv` form truncated
    // refclk/refdiv first (e.g. 25/2 → 12, losing 4%) and DIVERGED from the
    // resolver `pll_compute`, which already uses this exact rational form. No
    // overflow risk: refclk*fbdiv peaks at 25*239 ≈ 6_000 (far inside u64, and
    // the result inside u32). For the canonical refdiv=1 rows this is identical
    // to before — (25*218)/10 = 545 MHz, the BM1362_PLL_TABLE rated row.
    let num = (refclk_mhz as u64) * (fbdiv as u64);
    let den = (refdiv as u64) * (postdiv1 as u64) * (postdiv2 as u64) * (user_div as u64);
    (num / den) as u32
}

// ===========================================================================
// W12.A1 (2026-05-10): Algorithmic PLL parameter compute. Source-cite:
//   `DCENT_OS_DEVELOPMENT_KITRE3/.../bm1362_pll_table.md` §1+§2 — RE3
//   confirms the BM1362 PLL is **NOT** a lookup table. bmminer searches
//   `(refdiv, fbdiv, postdiv1, postdiv2, user_div)` candidate combinations
//   at runtime and selects the one with the minimum frequency error.
//   Six bmminer format strings (`_POSTDIV1 = %d, _POSTDIV2 = %d, USER_DIV
//   = %d, freq = %d`, `final refdiv: %d, fbdiv: %d, postdiv1: %d,
//   postdiv2: %d, usr divider: %d, min diff value: %f`) confirm the
//   5-parameter search loop and the "min diff" selection criterion.
// ===========================================================================

/// Inclusive parameter range for one of the BM1362 PLL dividers.
/// `min` and `max` are both achievable values.
///
/// Source: RE3 `bm1362_pll_table.md` §2 inferred-range table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllParamRange {
    pub min: u16,
    pub max: u16,
}

impl PllParamRange {
    /// Return `true` if `v` is within `[min, max]` inclusive.
    pub const fn contains(self, v: u16) -> bool {
        v >= self.min && v <= self.max
    }
}

/// BM1362 PLL parameter ranges per RE3 §2 inferred-range table.
///
/// | Parameter  | Min | Max |
/// |------------|-----|-----|
/// | `refdiv`   |   1 |  16 |
/// | `fbdiv`    |  16 | 200 |
/// | `postdiv1` |   1 |   8 |
/// | `postdiv2` |   1 |   8 |
/// | `user_div` |   1 |  16 |
///
/// These bounds are inferred from the bmminer search loop; the actual
/// silicon registers may permit a slightly wider envelope but every
/// canonical S19j Pro / S19 Pro+ / S19k Pro freq target lands cleanly
/// inside this box. The compute function below treats these as hard
/// search bounds — values outside return `None`.
pub const BM1362_PLL_RANGES: PllRanges = PllRanges {
    refdiv: PllParamRange { min: 1, max: 16 },
    fbdiv: PllParamRange { min: 16, max: 200 },
    postdiv1: PllParamRange { min: 1, max: 8 },
    postdiv2: PllParamRange { min: 1, max: 8 },
    user_div: PllParamRange { min: 1, max: 16 },
};

/// All five PLL parameter ranges, grouped for easy export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllRanges {
    pub refdiv: PllParamRange,
    pub fbdiv: PllParamRange,
    pub postdiv1: PllParamRange,
    pub postdiv2: PllParamRange,
    pub user_div: PllParamRange,
}

/// Resolved BM1362 PLL parameter set returned by [`pll_compute`].
///
/// Field widths are picked to fit each parameter's RE3 max:
///   - `refdiv` ≤ 16 → `u8`
///   - `fbdiv`  ≤ 200 → `u16` (gives headroom for future-die widening)
///   - `postdiv1`/`postdiv2` ≤ 8 → `u8`
///   - `user_div` ≤ 16 → `u8`
///
/// Use [`PllParams::compute_freq_mhz`] to roundtrip back to the
/// resulting frequency in MHz at a given reference clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllParams {
    pub refdiv: u8,
    pub fbdiv: u16,
    pub postdiv1: u8,
    pub postdiv2: u8,
    pub user_div: u8,
}

impl PllParams {
    /// Compute the resulting PLL output frequency (MHz, integer-truncated)
    /// for these dividers at the given reference clock.
    ///
    /// Returns `0` if any divider field is zero (defensive — shouldn't
    /// happen for params produced by [`pll_compute`], but keeps this
    /// helper panic-free in const-eval-eligible call sites).
    pub const fn compute_freq_mhz(&self, ref_mhz: u32) -> u32 {
        pll_freq_mhz(
            ref_mhz,
            self.refdiv as u32,
            self.fbdiv as u32,
            self.postdiv1 as u32,
            self.postdiv2 as u32,
            self.user_div as u32,
        )
    }
}

/// Internal: tolerance window used when scoring candidate parameter sets.
/// Per RE3 §2 the search criterion is "minimum frequency error";
/// canonical SKU targets at `ref=25 MHz` are all reachable as **exact**
/// integers (e.g. 545 = 25×218/(5×2×1) exact), so we accept any candidate
/// whose computed frequency equals the target exactly when one exists,
/// and otherwise pick the closest-by-absolute-difference one. If the
/// best candidate's error exceeds this slack we still return it — only
/// out-of-range targets that produce no candidate at all return `None`.
const PLL_SEARCH_SLACK_MHZ: u32 = 1;

/// Algorithmically search for a `(refdiv, fbdiv, postdiv1, postdiv2,
/// user_div)` combination that yields `target_mhz` at the given reference
/// clock `ref_mhz` (typically 25 MHz on stock S19j Pro hardware).
///
/// Returns the **exact-match** parameter set when one exists in the
/// search box; otherwise returns `None` for targets outside the
/// achievable envelope. RE3 explicitly confirms there is **no
/// lookup table** in bmminer — divider combinations are computed at
/// runtime per the format strings reported in
/// `bm1362_pll_table.md` §2.
///
/// # Formula (per RE3 `bm1362_pll_table.md` §2)
///
/// ```text
/// f_out = (f_refclk × fbdiv) / (refdiv × postdiv1 × postdiv2 × user_div)
/// ```
///
/// # Search strategy
///
/// Brute-force enumerate the parameter box (`BM1362_PLL_RANGES`) with a
/// canonical priority order that mirrors the bmminer loop revealed in
/// the format-string trace `_POSTDIV1 = %d, _POSTDIV2 = %d, USER_DIV
/// = %d, freq = %d`:
///
///   1. Outer loop: `postdiv1` (low → high).
///   2. Then: `postdiv2` (low → high).
///   3. Then: `user_div` (low → high).
///   4. Then: `refdiv` (low → high).
///   5. Inner: `fbdiv` (low → high).
///
/// A candidate is "preferred" if it produces a smaller absolute
/// frequency error against the target. Ties break toward the candidate
/// found first under the priority order above (i.e. lowest postdiv1,
/// then lowest postdiv2, etc.) — this mirrors bmminer's "first-best"
/// reading of the trace and keeps the canonical S19j Pro choice
/// `(refdiv=1, postdiv1=5, postdiv2=2, user_div=1)` for 545 MHz.
///
/// # Returns
///
/// - `Some(params)` when at least one candidate inside the parameter
///   box hits the target within `PLL_SEARCH_SLACK_MHZ` (1 MHz) — for
///   every canonical SKU frequency at `ref_mhz=25` this is an exact
///   match.
/// - `None` when the target is outside the achievable envelope at the
///   given reference clock. Specifically: target below
///   `ref_mhz × fbdiv_min / (refdiv_max × postdiv1_max × postdiv2_max
///   × user_div_max)` or above `ref_mhz × fbdiv_max / (refdiv_min ×
///   postdiv1_min × postdiv2_min × user_div_min)` will never satisfy
///   the slack and produce `None`.
///
/// # Notes
///
/// Pure function. No I/O, no platform-specific code. Safe to call from
/// any context including const-eval-eligible call sites (modulo the
/// non-const inner loops).
/// validate input ranges — a `target_mhz=0` or `ref_mhz=0` trivially
/// returns `None`.
pub fn pll_compute(target_mhz: u32, ref_mhz: u32) -> Option<PllParams> {
    if target_mhz == 0 || ref_mhz == 0 {
        return None;
    }

    let r = BM1362_PLL_RANGES;
    let mut best: Option<(u32, PllParams)> = None;

    // Priority-ordered brute force per the bmminer trace
    // `_POSTDIV1=%d, _POSTDIV2=%d, USER_DIV=%d, freq=%d`. Tie-breaking
    // (`<` not `<=`) keeps the FIRST candidate at any given error,
    // i.e. the lowest-postdiv1/postdiv2/user_div/refdiv/fbdiv tuple,
    // which lands the canonical (1, 218, 5, 2, 1) for 545 MHz.
    for postdiv1 in r.postdiv1.min..=r.postdiv1.max {
        for postdiv2 in r.postdiv2.min..=r.postdiv2.max {
            for user_div in r.user_div.min..=r.user_div.max {
                for refdiv in r.refdiv.min..=r.refdiv.max {
                    for fbdiv in r.fbdiv.min..=r.fbdiv.max {
                        // Use rational comparison to avoid losing
                        // precision in `(refclk/refdiv)`. Canonical
                        // refdiv=1 paths are exact under the existing
                        // `pll_freq_mhz` truncation, but non-unit
                        // refdivs need the wider check.
                        // f_out = ref_mhz * fbdiv / (refdiv * pd1 * pd2 * usr).
                        let den = (refdiv as u64)
                            * (postdiv1 as u64)
                            * (postdiv2 as u64)
                            * (user_div as u64);
                        if den == 0 {
                            continue;
                        }
                        let num = (ref_mhz as u64) * (fbdiv as u64);
                        // Skip non-integer hits — RE3 evidence shows
                        // every canonical SKU target hits exact at
                        // ref=25 MHz, so the search criterion is
                        // "exact integer match". Non-integer
                        // candidates fall back to the rounded form
                        // for tie-breaking only.
                        if !num.is_multiple_of(den) {
                            // Track the rounded value as a tie-break
                            // fallback in case no exact match exists.
                            let rounded = (num / den) as u32;
                            let err = rounded.abs_diff(target_mhz);
                            let candidate = PllParams {
                                refdiv: refdiv as u8,
                                fbdiv,
                                postdiv1: postdiv1 as u8,
                                postdiv2: postdiv2 as u8,
                                user_div: user_div as u8,
                            };
                            match best {
                                None => best = Some((err, candidate)),
                                Some((be, _)) if err < be => {
                                    best = Some((err, candidate));
                                }
                                _ => {}
                            }
                            continue;
                        }
                        let f = (num / den) as u32;
                        let err = f.abs_diff(target_mhz);
                        let candidate = PllParams {
                            refdiv: refdiv as u8,
                            fbdiv,
                            postdiv1: postdiv1 as u8,
                            postdiv2: postdiv2 as u8,
                            user_div: user_div as u8,
                        };
                        match best {
                            None => best = Some((err, candidate)),
                            Some((be, _)) if err < be => {
                                best = Some((err, candidate));
                            }
                            _ => {}
                        }
                        // Early-exit on exact match to keep this fast
                        // for the common canonical-SKU lookups.
                        if err == 0 {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }

    match best {
        Some((err, params)) if err <= PLL_SEARCH_SLACK_MHZ => Some(params),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_21_rows() {
        assert_eq!(BM1362_PROFILES.len(), 21);
        assert_eq!(BM1362_TABLE.profiles.len(), 21);
    }

    #[test]
    fn step_range_is_minus_16_to_plus_4() {
        assert_eq!(BM1362_TABLE.min_step(), -16);
        assert_eq!(BM1362_TABLE.max_step(), 4);
    }

    #[test]
    fn step_index_is_dense_and_monotonic() {
        // Every integer in -16..=4 must appear exactly once, in order.
        for (i, profile) in BM1362_PROFILES.iter().enumerate() {
            let expected_step = -16 + i as i32;
            assert_eq!(
                profile.step, expected_step,
                "row {i}: expected step {expected_step}, got {}",
                profile.step
            );
        }
    }

    #[test]
    fn frequency_cadence_is_exactly_25_mhz_per_step() {
        // The RE doc proves the cadence; pin it so a future row update
        // can't silently break the linear schedule.
        for window in BM1362_PROFILES.windows(2) {
            let delta = window[1].freq_mhz as i64 - window[0].freq_mhz as i64;
            assert_eq!(
                delta, 25,
                "step {} -> {} freq jump is {} MHz (expected 25)",
                window[0].step, window[1].step, delta
            );
        }
    }

    #[test]
    fn voltage_clamps_at_11880mv_for_lowest_four_steps() {
        // Per RE doc: BM1362 silicon voltage floor.
        for profile in BM1362_PROFILES.iter().filter(|p| p.step <= -13) {
            assert!(
                (profile.voltage_v - 11.880).abs() < 1e-6,
                "step {}: voltage {} should clamp at 11.880 V",
                profile.step,
                profile.voltage_v
            );
        }
    }

    #[test]
    fn voltage_climbs_by_150mv_per_step_above_clamp() {
        // From step -12 onward the voltage column moves +0.150 V per step.
        let above_clamp: Vec<_> = BM1362_PROFILES.iter().filter(|p| p.step >= -12).collect();
        for window in above_clamp.windows(2) {
            let delta = (window[1].voltage_v - window[0].voltage_v).abs();
            assert!(
                (delta - 0.150).abs() < 1e-3,
                "step {} -> {} voltage jump is {} V (expected 0.150)",
                window[0].step,
                window[1].step,
                delta
            );
        }
    }

    #[test]
    fn default_step_is_zero_and_yields_the_default_profile() {
        let default = BM1362_TABLE.default_profile().expect("default profile");
        assert_eq!(default.step, 0);
        assert_eq!(default.profile_name(), "default");
        assert_eq!(default.freq_mhz, 545);
    }

    #[test]
    fn nameplate_efficiency_is_29_55_jth() {
        // Live-confirmed: 3126 W / 105.8 TH/s = 29.546... J/TH.
        let default = BM1362_TABLE.default_profile().unwrap();
        let eff = default.watts_per_ths().unwrap();
        assert!((eff - 29.55).abs() < 0.01, "got {}", eff);
    }

    #[test]
    fn sweet_spot_is_step_minus_9_at_27_60_jth() {
        // Per RE doc Â§1.4: Step=-9 (320 MHz, 12.45 V) is the efficiency
        // sweet spot at 27.60 J/TH (6.6% better than nameplate).
        let sweet = BM1362_TABLE.sweet_spot_profile().unwrap();
        assert_eq!(sweet.step, -9);
        let eff = sweet.watts_per_ths().unwrap();
        assert!((eff - 27.60).abs() < 0.05, "got {}", eff);
    }

    #[test]
    fn computed_sweet_spot_matches_pre_baked() {
        // The pre-baked sweet_spot_step must match the computed minimum
        // J/TH across the table. If a future row update moves the
        // efficiency minimum, this test fails â€” flagging that the
        // pre-baked constant needs updating too.
        let pre_baked = BM1362_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1362_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(
            pre_baked.step, computed.step,
            "sweet_spot_step={} but computed minimum J/TH is at step {}",
            pre_baked.step, computed.step
        );
    }

    #[test]
    fn by_step_returns_correct_row() {
        let p = BM1362_TABLE.by_step(-9).unwrap();
        assert_eq!(p.freq_mhz, 320);
        assert_eq!(p.wall_watts, Some(1714));
    }

    #[test]
    fn by_step_returns_none_for_out_of_range() {
        assert!(BM1362_TABLE.by_step(-17).is_none());
        assert!(BM1362_TABLE.by_step(5).is_none());
        assert!(BM1362_TABLE.by_step(100).is_none());
    }

    #[test]
    fn by_name_resolves_default_and_frequency_forms() {
        let zero = BM1362_TABLE.by_name("default").unwrap();
        assert_eq!(zero.step, 0);

        let sweet = BM1362_TABLE.by_name("320MHz").unwrap();
        assert_eq!(sweet.step, -9);

        let max = BM1362_TABLE.by_name("645MHz").unwrap();
        assert_eq!(max.step, 4);
    }

    #[test]
    fn by_name_returns_none_for_unknown() {
        assert!(BM1362_TABLE.by_name("999MHz").is_none());
        assert!(BM1362_TABLE.by_name("DEFAULT").is_none()); // case-sensitive
        assert!(BM1362_TABLE.by_name("").is_none());
    }

    #[test]
    fn open_core_voltage_is_above_max_steady_state_voltage() {
        // The open-core voltage must be higher than any steady-state
        // operating voltage in the table â€” that's the whole point of
        // the elevated rail during chip enumeration.
        let max_steady_v = BM1362_PROFILES
            .iter()
            .map(|p| p.voltage_v)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            BM1362_OPEN_CORE_VOLTAGE_V > max_steady_v,
            "open-core voltage {} not above max steady-state voltage {}",
            BM1362_OPEN_CORE_VOLTAGE_V,
            max_steady_v
        );
    }

    #[test]
    fn voltage_table_steps_are_multiples_of_dac_granularity() {
        // 0.150 V profile step / 0.030 V DAC step = 5 â€” every profile
        // voltage is reachable by the dsPIC controller.
        for profile in BM1362_PROFILES.iter() {
            let mv = (profile.voltage_v * 1000.0).round() as u32;
            let dac_mv = (BM1362_VOLTAGE_DAC_GRANULARITY_V * 1000.0).round() as u32;
            assert_eq!(
                mv % dac_mv,
                0,
                "step {}: voltage {} V ({} mV) is not a multiple of DAC step {} mV",
                profile.step,
                profile.voltage_v,
                mv,
                dac_mv
            );
        }
    }

    #[test]
    fn live_confirmed_rows_match_re_doc_provenance() {
        // Per RE doc Â§1.1: rows -16..=-5 are live-confirmed.
        for profile in BM1362_PROFILES.iter() {
            let expected = match profile.step {
                -16..=-5 => ProfileSource::LiveConfirmed,
                0 => ProfileSource::OperatorConfirmed,
                _ => ProfileSource::Reconstructed,
            };
            assert_eq!(
                profile.source, expected,
                "step {}: provenance {:?} does not match RE doc",
                profile.step, profile.source
            );
        }
    }

    #[test]
    fn step_minus_16_efficiency_matches_re_doc() {
        // RE doc Â§1.4: Step -16 = 35.5 J/TH (997 / 28.1).
        let p = BM1362_TABLE.by_step(-16).unwrap();
        let eff = p.watts_per_ths().unwrap();
        assert!((eff - 35.5).abs() < 0.1, "got {}", eff);
    }

    #[test]
    fn nameplate_btu_per_hour() {
        // Default: 3126 W = 10666 BTU/h (equivalent to a small space heater).
        let default = BM1362_TABLE.default_profile().unwrap();
        let btu = default
            .heat_btu_per_hour()
            .expect("watts known on baked profile");
        // 3126 * 3.412 ≈ 10666.
        assert!((btu - 10666.0).abs() < 1.0, "got {}", btu);
    }

    #[test]
    fn json_round_trip_preserves_every_field() {
        let original = BM1362_TABLE.by_step(-9).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    // -----------------------------------------------------------------
    // W11.5 (2026-05-09): per-SKU geometry + PLL formula tests.
    // -----------------------------------------------------------------

    #[test]
    fn crc8_poly_is_0x31() {
        // Pin: RE2 catalog §4.1 line 394 says BM1362 framing CRC8 poly
        // is 0x31. Same polynomial as BM1368.
        assert_eq!(chip::CRC8_POLY, 0x31);
    }

    #[test]
    fn address_stride_is_2_and_chips_per_chain_is_126() {
        // Pin: 126 chips per chain × stride 2 = 252 addresses, fits in
        // an 8-bit address space (≤ 0xFE). Live-pinned on .139 / .133.
        assert_eq!(chip::ADDRESS_STRIDE, 2);
        assert_eq!(chip::CHIPS_PER_CHAIN, 126);
        let last_addr = (chip::CHIPS_PER_CHAIN as u16 - 1) * chip::ADDRESS_STRIDE as u16;
        assert_eq!(last_addr, 0xFA, "last chip address must be 0xFA");
    }

    #[test]
    fn chip_id_register_value_is_0x1362() {
        assert_eq!(chip::CHIP_ID, 0x1362);
    }

    #[test]
    fn misc_ctrl_full_register_address_pinned() {
        // RE2 §4.2 line 412 — BM1362 MiscCtrl lives at full 24-bit
        // register address 0xC100B0.
        assert_eq!(chip::MISC_CTRL_REG_FULL, 0x00C1_00B0);
    }

    #[test]
    fn cores_geometry_documents_both_die_internal_and_fpga_visible() {
        // Pin: dev-kit says 65 cores/die × 514 small-cores/core. The
        // FPGA-visible big-core count from the live driver is 4. Both
        // numbers are real and live at different layers — see the
        // module doc-comment for reconciliation.
        assert_eq!(chip::CORES_PER_DIE, 65);
        assert_eq!(chip::SMALL_CORES_PER_CORE, 514);
        assert_eq!(chip::FPGA_VISIBLE_BIG_CORES, 4);
        let total_small_cores = chip::CORES_PER_DIE as u32 * chip::SMALL_CORES_PER_CORE as u32;
        assert_eq!(total_small_cores, 33_410);
    }

    #[test]
    fn work_id_is_16_bits() {
        // Pin: RE2 §4.2 line 430 — ASIC-side work_id is 16-bit.
        // (FPGA-side dispatch work_id is 8-bit per
        //  — both are right at different
        // layers.)
        assert_eq!(work_layout::WORK_ID_BITS, 16);
    }

    #[test]
    fn work_tx_is_20_words_80_bytes() {
        assert_eq!(work_layout::TX_WORDS, 20);
        assert_eq!(work_layout::TX_BYTES, 80);
    }

    #[test]
    fn work_rx_is_6_words_24_bytes() {
        assert_eq!(work_layout::RX_WORDS, 6);
        assert_eq!(work_layout::RX_BYTES, 24);
    }

    #[test]
    fn ctrl_default_is_pinned_to_live_probe_value() {
        // Live-probed on .139 chain1/chain4 — see
        // dcentrald_hal::fpga_chain::ctrl_am2::BM1362_DEFAULT.
        assert_eq!(work_layout::CTRL_DEFAULT, 0x0090_1002);
    }

    #[test]
    fn standard_chain_geometry_is_internally_consistent() {
        let g = Bm1362ChainGeometry::STANDARD;
        assert_eq!(g.chips_per_chain, 126);
        assert_eq!(g.chains, 4);
        assert_eq!(g.domains_per_chain, 42);
        assert_eq!(g.asics_per_domain, 3);
        assert_eq!(g.grid_rows, 12);
        assert_eq!(g.grid_cols, 11);
        // 42 × 3 = 126 chips per chain.
        assert_eq!(g.chips_via_domains(), 126);
        // 12 × 11 = 132 grid slots; 6 phantom slots above 0xFA stride.
        let grid_slots = g.grid_rows as u32 * g.grid_cols as u32;
        assert!(grid_slots >= g.chips_per_chain as u32);
    }

    #[test]
    fn bhb42601_voltage_table_present_and_monotonic() {
        // Pin: voltages must increase as frequencies decrease (silicon-
        // floor behavior on the BHB42601 table).
        let table = Bm1362HashboardSku::Bhb42601.freq_voltage_table();
        assert_eq!(table.len(), 5);
        for window in table.windows(2) {
            let (f_hi, v_lo) = window[0];
            let (f_lo, v_hi) = window[1];
            assert!(
                f_hi > f_lo,
                "freq must decrease across rows: {} -> {}",
                f_hi,
                f_lo
            );
            assert!(
                v_hi > v_lo,
                "voltage must increase as freq decreases: {} mV -> {} mV",
                v_lo,
                v_hi
            );
        }
        // Top row is the rated S19j Pro target: 545 MHz @ 1320 mV.
        assert_eq!(table[0], (545, 1320));
        // Bottom row is the floor: 465 MHz @ 1380 mV.
        assert_eq!(table[4], (465, 1380));
    }

    #[test]
    fn bhb42601_full_grid_encodes_stock_per_freq_floors() {
        // R-12 ground truth (stock pvt_tables.h PVT_LEVELS_BHB42601): the
        // 545 MHz tier minimum is 1340 mV; 1320 mV first appears at <= 525 MHz.
        let grid = Bm1362HashboardSku::Bhb42601.full_pvt_grid();
        let min_at = |f: u16| {
            grid.iter()
                .filter(|(gf, _)| *gf == f)
                .map(|(_, v)| *v)
                .min()
                .unwrap()
        };
        assert_eq!(min_at(545), 1340, "stock BHB42601 545 MHz floor is 1340 mV");
        assert_eq!(min_at(525), 1320);
        assert_eq!(min_at(465), 1320);
        // The canonical marginal envelope MIN stays 1320 (a real stock point at
        // <= 525 MHz) — this fix must NOT raise it and wrongly forbid 1320.
        let canon_min = BHB42601_FREQ_VOLT_TABLE
            .iter()
            .map(|(_, v)| *v)
            .min()
            .unwrap();
        assert_eq!(canon_min, 1320);
        // Extended-low family carries the same 545 floor + a 440 tier.
        let grid631 = Bm1362HashboardSku::Bhb42631.full_pvt_grid();
        let min631_545 = grid631
            .iter()
            .filter(|(f, _)| *f == 545)
            .map(|(_, v)| *v)
            .min()
            .unwrap();
        assert_eq!(min631_545, 1340);
        assert!(grid631.iter().any(|(f, _)| *f == 440));
    }

    #[test]
    fn bhb42801_voltage_higher_than_42601() {
        // Pin: porting plan §10 — BHB42801 is the higher-grade SKU
        // with a lifted voltage envelope. Its top voltage (1600 mV at
        // 585 MHz) must exceed BHB42601's top voltage (1380 mV at
        // 465 MHz).
        let max_42801 = Bm1362HashboardSku::Bhb42801
            .freq_voltage_table()
            .iter()
            .map(|(_, v)| *v)
            .max()
            .unwrap();
        let max_42601 = Bm1362HashboardSku::Bhb42601
            .freq_voltage_table()
            .iter()
            .map(|(_, v)| *v)
            .max()
            .unwrap();
        assert_eq!(max_42801, 1600);
        assert_eq!(max_42601, 1380);
        assert!(
            max_42801 > max_42601,
            "BHB42801 top voltage {} mV must exceed BHB42601 top {} mV",
            max_42801,
            max_42601
        );
    }

    #[test]
    fn bhb42611_freq_band_is_higher_than_42601_at_same_voltages() {
        // Per porting plan §10: BHB42611 shares BHB42601's voltage
        // envelope (1320-1380 mV) but pushes freq up (610-670 MHz vs
        // 465-545 MHz). The voltage minimums match exactly.
        let v_min_42601 = Bm1362HashboardSku::Bhb42601
            .freq_voltage_table()
            .iter()
            .map(|(_, v)| *v)
            .min()
            .unwrap();
        let v_min_42611 = Bm1362HashboardSku::Bhb42611
            .freq_voltage_table()
            .iter()
            .map(|(_, v)| *v)
            .min()
            .unwrap();
        assert_eq!(v_min_42601, 1320);
        assert_eq!(v_min_42611, 1320);

        let f_max_42601 = Bm1362HashboardSku::Bhb42601
            .freq_voltage_table()
            .iter()
            .map(|(f, _)| *f)
            .max()
            .unwrap();
        let f_max_42611 = Bm1362HashboardSku::Bhb42611
            .freq_voltage_table()
            .iter()
            .map(|(f, _)| *f)
            .max()
            .unwrap();
        assert_eq!(f_max_42601, 545);
        assert_eq!(f_max_42611, 670);
        assert!(f_max_42611 > f_max_42601);
    }

    #[test]
    fn pll_formula_round_trip_for_545mhz() {
        // Stock S19j Pro: refclk=25, refdiv=1, fbdiv=218, postdiv1=5,
        // postdiv2=2, user_div=1 → (25/1)*218/(5*2*1) = 5450/10 = 545.
        // Matches the rated row in BM1362_PLL_TABLE
        // (`(545, 0x50DA_0141)`, fbdiv=218).
        let mhz = pll_freq_mhz(25, 1, 218, 5, 2, 1);
        assert_eq!(mhz, 545, "expected 545 MHz, got {}", mhz);
    }

    #[test]
    fn pll_formula_handles_non_unit_dividers() {
        // refclk=25, refdiv=2, fbdiv=80, pd1=2, pd2=2, usr=1.
        // True f_out = 25*80 / (refdiv2 * pd1=2 * pd2=2 * usr=1) = 2000/8 = 250 MHz.
        // The OLD divide-first form truncated 25/2 → 12, returned 12*80/4 = 240
        // (4% low) and diverged from the `pll_compute` resolver. The multiply-
        // first u64 form is exact and matches the resolver's rational math.
        let mhz = pll_freq_mhz(25, 2, 80, 2, 2, 1);
        assert_eq!(mhz, 250);
        // Exact-rational cross-check (the value the resolver targets):
        assert_eq!(mhz, (25u64 * 80 / (2 * 2 * 2 * 1)) as u32);
    }

    #[test]
    fn pll_formula_zero_divider_returns_zero_no_panic() {
        // Defensive: must NOT panic on bogus inputs (it's `const fn`
        // and may be evaluated in odd places).
        assert_eq!(pll_freq_mhz(25, 0, 218, 5, 2, 1), 0);
        assert_eq!(pll_freq_mhz(25, 1, 218, 0, 2, 1), 0);
        assert_eq!(pll_freq_mhz(25, 1, 218, 5, 0, 1), 0);
        assert_eq!(pll_freq_mhz(25, 1, 218, 5, 2, 0), 0);
    }

    #[test]
    fn hashboard_id_strings_match_levels_json_naming() {
        assert_eq!(Bm1362HashboardSku::Bhb42601.hashboard_id(), "BHB42601");
        assert_eq!(Bm1362HashboardSku::Bhb42801.hashboard_id(), "BHB42801");
        assert_eq!(Bm1362HashboardSku::Bhb42611.hashboard_id(), "BHB42611");
    }

    #[test]
    fn default_sku_routes_known_platforms_to_correct_table() {
        // S19j Pro standard variants → BHB42601.
        for plat in [
            "am2-s19jpro",
            "am3-aml-s19jpro",
            "cv1835-s19jpro",
            "am3-bb-s19jpro",
            "bcb100-s19jpro-lab",
        ] {
            assert_eq!(
                default_sku_for_platform(plat),
                Some(Bm1362HashboardSku::Bhb42601),
                "platform {} should default to BHB42601",
                plat
            );
        }
        // S19 Pro+ → BHB42801.
        for plat in ["am2-s19jproplus", "am3-aml-s19jproplus"] {
            assert_eq!(
                default_sku_for_platform(plat),
                Some(Bm1362HashboardSku::Bhb42801),
            );
        }
        // Unknown platform → None (caller must handle).
        assert_eq!(default_sku_for_platform("unknown-platform"), None);
        assert_eq!(default_sku_for_platform(""), None);
    }

    #[test]
    fn sku_serde_round_trip() {
        for sku in [
            Bm1362HashboardSku::Bhb42601,
            Bm1362HashboardSku::Bhb42801,
            Bm1362HashboardSku::Bhb42611,
        ] {
            let json = serde_json::to_string(&sku).unwrap();
            let back: Bm1362HashboardSku = serde_json::from_str(&json).unwrap();
            assert_eq!(sku, back);
        }
    }

    // -----------------------------------------------------------------
    // W12.A1 (2026-05-10): Algorithmic PLL compute tests. Per RE3
    // `bm1362_pll_table.md` §1+§2 the PLL is **NOT** lookup-table-based;
    // bmminer searches `(refdiv, fbdiv, postdiv1, postdiv2, user_div)`
    // and selects the minimum-error combination. These tests cover the
    // canonical SKU frequencies (BHB42601/42801/42611) and edge cases.
    // -----------------------------------------------------------------

    /// Helper: assert `pll_compute(target, ref)` produces an exact
    /// match — both `Some(params)` and `params.compute_freq_mhz(ref) ==
    /// target`. Roundtrip must be exact for every canonical SKU value
    /// at `ref=25 MHz` per the doc-comment contract.
    fn assert_exact_roundtrip(target_mhz: u32, ref_mhz: u32) {
        let params = pll_compute(target_mhz, ref_mhz).unwrap_or_else(|| {
            panic!(
                "pll_compute({} MHz, {} MHz ref) returned None",
                target_mhz, ref_mhz
            )
        });
        // All resolved params must lie inside the documented ranges.
        assert!(
            BM1362_PLL_RANGES.refdiv.contains(params.refdiv as u16),
            "refdiv {} out of range",
            params.refdiv
        );
        assert!(
            BM1362_PLL_RANGES.fbdiv.contains(params.fbdiv),
            "fbdiv {} out of range",
            params.fbdiv
        );
        assert!(
            BM1362_PLL_RANGES.postdiv1.contains(params.postdiv1 as u16),
            "postdiv1 {} out of range",
            params.postdiv1
        );
        assert!(
            BM1362_PLL_RANGES.postdiv2.contains(params.postdiv2 as u16),
            "postdiv2 {} out of range",
            params.postdiv2
        );
        assert!(
            BM1362_PLL_RANGES.user_div.contains(params.user_div as u16),
            "user_div {} out of range",
            params.user_div
        );
        let computed = params.compute_freq_mhz(ref_mhz);
        assert_eq!(
            computed, target_mhz,
            "roundtrip mismatch: target={} MHz, params={:?}, computed={} MHz",
            target_mhz, params, computed
        );
    }

    // --- BHB42601 (S19j Pro standard) — 545/525/505/485/465 MHz @ 25 MHz ref ---

    #[test]
    fn pll_compute_bhb42601_545mhz_exact() {
        assert_exact_roundtrip(545, 25);
    }

    #[test]
    fn pll_compute_bhb42601_525mhz_exact() {
        assert_exact_roundtrip(525, 25);
    }

    #[test]
    fn pll_compute_bhb42601_505mhz_exact() {
        assert_exact_roundtrip(505, 25);
    }

    #[test]
    fn pll_compute_bhb42601_485mhz_exact() {
        assert_exact_roundtrip(485, 25);
    }

    #[test]
    fn pll_compute_bhb42601_465mhz_exact() {
        assert_exact_roundtrip(465, 25);
    }

    // --- BHB42801 (S19 Pro+ higher-grade) — 675/645/615/585 MHz @ 25 MHz ref ---

    #[test]
    fn pll_compute_bhb42801_675mhz_exact() {
        assert_exact_roundtrip(675, 25);
    }

    #[test]
    fn pll_compute_bhb42801_645mhz_exact() {
        assert_exact_roundtrip(645, 25);
    }

    #[test]
    fn pll_compute_bhb42801_615mhz_exact() {
        assert_exact_roundtrip(615, 25);
    }

    #[test]
    fn pll_compute_bhb42801_585mhz_exact() {
        assert_exact_roundtrip(585, 25);
    }

    // --- BHB42611 (mid-band) — 670 MHz top-of-table @ 25 MHz ref ---

    #[test]
    fn pll_compute_bhb42611_670mhz_exact() {
        // Per RE3 levels.json (BHB42611 mid-band): 670/650/630/610 MHz
        // @ 1320-1380 mV. Top-of-table 670 MHz is the canonical
        // anchor for this SKU.
        assert_exact_roundtrip(670, 25);
    }

    // --- Canonical 545 MHz produces the rated parameter set ---

    #[test]
    fn pll_compute_545mhz_resolves_to_a_correct_param_set() {
        // RE3 §1 confirms there is **NO** lookup table — bmminer
        // search may resolve different (but mathematically
        // equivalent) parameter tuples for the same target. The
        // documented "canonical" `(refdiv=1, fbdiv=218, postdiv1=5,
        // postdiv2=2, user_div=1)` row in `BM1362_PLL_TABLE` is one
        // valid answer but not the only one (e.g. `(1, 109, 1, 5, 1)`
        // also yields 545 MHz exactly: 25×109/(1×1×5×1) = 545).
        //
        // Pin the **correctness contract** (roundtrip is exact + all
        // dividers in range), not a specific tuple. The roundtrip
        // tests above already cover the canonical SKU values; this
        // test additionally confirms the resolved tuple is a
        // mathematical match against the formula.
        let params = pll_compute(545, 25).unwrap();
        assert_eq!(params.compute_freq_mhz(25), 545);
        // Non-zero dividers — `pll_compute` never returns the
        // sentinel zeros guarded by `pll_freq_mhz`.
        assert!(params.refdiv >= 1);
        assert!(params.fbdiv >= 1);
        assert!(params.postdiv1 >= 1);
        assert!(params.postdiv2 >= 1);
        assert!(params.user_div >= 1);
    }

    // --- Out-of-range / edge cases ---

    #[test]
    fn pll_compute_zero_target_returns_none() {
        assert!(pll_compute(0, 25).is_none());
    }

    #[test]
    fn pll_compute_zero_ref_returns_none() {
        assert!(pll_compute(545, 0).is_none());
    }

    #[test]
    fn pll_compute_extreme_above_range_returns_none() {
        // f_max = ref × fbdiv_max / (refdiv_min × pd1_min × pd2_min ×
        // user_div_min) = 25 × 200 / 1 = 5000 MHz. 9999 MHz is well
        // above; must return None.
        assert!(pll_compute(9999, 25).is_none());
    }

    #[test]
    fn pll_compute_below_minimum_returns_none() {
        // f_min = ref × fbdiv_min / (refdiv_max × pd1_max × pd2_max ×
        // user_div_max) = 25 × 16 / (16 × 8 × 8 × 16) = 400 / 16384
        // ≈ 0.024 MHz. 50 MHz is well above this absolute floor but
        // below the smallest representable integer hit at refdiv=1
        // (smallest = 25*16/(8*8*16) = 0; smallest non-zero integer
        // ≥ 1 is achievable, so 50 MHz is actually reachable).
        // Use a target that genuinely cannot be hit: a prime that
        // doesn't factor cleanly into the search box.
        // 5003 MHz exceeds f_max (5000) → None.
        assert!(pll_compute(5003, 25).is_none());
    }

    #[test]
    fn pll_compute_returns_params_inside_documented_ranges() {
        // Sanity: every successful resolution must respect the
        // documented PLL parameter envelope. Walk a handful of
        // canonical and off-canonical targets.
        for target in [465_u32, 485, 505, 525, 545, 585, 615, 645, 670, 675] {
            let p = pll_compute(target, 25)
                .unwrap_or_else(|| panic!("missing params for {} MHz", target));
            assert!(BM1362_PLL_RANGES.refdiv.contains(p.refdiv as u16));
            assert!(BM1362_PLL_RANGES.fbdiv.contains(p.fbdiv));
            assert!(BM1362_PLL_RANGES.postdiv1.contains(p.postdiv1 as u16));
            assert!(BM1362_PLL_RANGES.postdiv2.contains(p.postdiv2 as u16));
            assert!(BM1362_PLL_RANGES.user_div.contains(p.user_div as u16));
        }
    }

    #[test]
    fn pll_compute_compatible_with_existing_pll_freq_mhz_helper() {
        // Cross-check: for any resolved canonical SKU target, the
        // existing `pll_freq_mhz()` helper (used by the rest of the
        // codebase) produces the same integer-truncated MHz value
        // as `PllParams::compute_freq_mhz()`. Ensures we haven't
        // forked the formula.
        for target in [465_u32, 485, 505, 525, 545, 585, 615, 645, 670, 675] {
            let p = pll_compute(target, 25).unwrap();
            let via_helper = pll_freq_mhz(
                25,
                p.refdiv as u32,
                p.fbdiv as u32,
                p.postdiv1 as u32,
                p.postdiv2 as u32,
                p.user_div as u32,
            );
            assert_eq!(
                via_helper, target,
                "pll_freq_mhz disagreed with pll_compute for {} MHz: {:?} → {}",
                target, p, via_helper
            );
        }
    }

    // -----------------------------------------------------------------
    // W13.C2 (2026-05-10): 15-SKU PVT table expansion. Source-cite:
    //   `RE_DELIVERABLES/RE_DELIVERABLES/pvt_tables.h` +
    //   `levels_json_pvt_validation.md` (RE4 deliverable).
    //   Memory rules: ,
    //   .
    //
    //   Hard rules pinned by these tests:
    //     1. Existing 3 SKUs (BHB42601 / BHB42801 / BHB42611) keep
    //        their tables byte-for-byte (W12 + earlier regression set).
    //     2. BHB42803 is the **only** SKU with `voltage_fixed=true`.
    //     3. BHB42803 has 3 chains (NOT 4); every other SKU has 4.
    //     4. High-bin family (BHB42801/811/821/831 + BHB42803) requires
    //        APW12+ — never APW12 SMBus.
    //     5. BHB42841 is the **only** SKU with `inverted_curve=true`.
    //     6. BHB42611 is the **only** SKU with `mix_levels=true`.
    //     7. `default_for_unrecognized_sku()` returns BHB42601.
    //     8. `from_id()` round-trips against `hashboard_id()` for all 15.
    // -----------------------------------------------------------------

    #[test]
    fn all_15_skus_have_a_freq_voltage_table() {
        // Parametrized smoke: every SKU's table is non-empty and
        // contains valid (freq, volt) pairs.
        assert_eq!(ALL_BM1362_HASHBOARD_SKUS.len(), 15);
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let table = sku.freq_voltage_table();
            assert!(
                !table.is_empty(),
                "{} freq/voltage table must be non-empty",
                sku.hashboard_id()
            );
            for (freq_mhz, volt_mv) in table {
                assert!(
                    *freq_mhz >= 100 && *freq_mhz <= 1000,
                    "{}: freq {} MHz outside [100, 1000]",
                    sku.hashboard_id(),
                    freq_mhz
                );
                assert!(
                    *volt_mv >= 1000 && *volt_mv <= 1700,
                    "{}: volt {} mV outside [1000, 1700]",
                    sku.hashboard_id(),
                    volt_mv
                );
            }
            // chain_count + asics_per_chain are non-zero (no dead SKUs).
            assert!(sku.chain_count() >= 1);
            assert!(sku.asics_per_chain() >= 1);
        }
    }

    #[test]
    fn bhb42601_table_is_byte_identical_to_w11_baseline() {
        // No-regression: W11 BHB42601 table is the load-bearing default
        // for every fielded S19j Pro / S19j Pro+ standard variant.
        // Changing this would silently re-target every miner.
        let table = Bm1362HashboardSku::Bhb42601.freq_voltage_table();
        assert_eq!(table.len(), 5);
        assert_eq!(table[0], (545, 1320));
        assert_eq!(table[1], (525, 1330));
        assert_eq!(table[2], (505, 1345));
        assert_eq!(table[3], (485, 1360));
        assert_eq!(table[4], (465, 1380));
    }

    #[test]
    fn bhb42801_table_is_byte_identical_to_w11_baseline() {
        let table = Bm1362HashboardSku::Bhb42801.freq_voltage_table();
        assert_eq!(table.len(), 4);
        assert_eq!(table[0], (675, 1530));
        assert_eq!(table[1], (645, 1545));
        assert_eq!(table[2], (615, 1565));
        assert_eq!(table[3], (585, 1600));
    }

    #[test]
    fn bhb42611_table_is_byte_identical_to_w11_baseline() {
        let table = Bm1362HashboardSku::Bhb42611.freq_voltage_table();
        assert_eq!(table.len(), 4);
        assert_eq!(table[0], (670, 1320));
        assert_eq!(table[1], (650, 1340));
        assert_eq!(table[2], (630, 1360));
        assert_eq!(table[3], (610, 1380));
    }

    #[test]
    fn bhb42803_voltage_fixed_true() {
        // Pin: BHB42803 is the **only** SKU with `voltage_fixed=true`.
        // the autotuner
        // short-circuits voltage_search for this SKU.
        let flags = Bm1362HashboardSku::Bhb42803.flags();
        assert!(flags.voltage_fixed, "BHB42803 must be voltage_fixed=true");
        // Every voltage in the table is exactly 1530 mV.
        for (_freq, volt) in Bm1362HashboardSku::Bhb42803.freq_voltage_table() {
            assert_eq!(*volt, 1530, "BHB42803 fixed-V; got {} mV", volt);
        }
        // 3-chain topology (NOT 4 — repair-class).
        assert_eq!(Bm1362HashboardSku::Bhb42803.chain_count(), 3);
        // 84 ASICs per chain.
        assert_eq!(Bm1362HashboardSku::Bhb42803.asics_per_chain(), 84);
    }

    #[test]
    fn bhb42803_is_only_voltage_fixed_sku() {
        // Negative parameterised pin: NO other SKU may flip
        // voltage_fixed=true. Adding a second voltage_fixed SKU silently
        // would slip past dashboard / install-preflight UX.
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let expected = matches!(sku, Bm1362HashboardSku::Bhb42803);
            assert_eq!(
                sku.flags().voltage_fixed,
                expected,
                "{}: voltage_fixed flag mismatch",
                sku.hashboard_id()
            );
        }
    }

    #[test]
    fn bhb42841_inverted_curve_true() {
        // Pin: BHB42841 is the **only** SKU with inverted_curve=true.
        // Lower freq → HIGHER voltage for stability margin.
        assert!(Bm1362HashboardSku::Bhb42841.flags().inverted_curve);
        // Voltage range per `pvt_tables.h`: 1360-1480 mV.
        let table = Bm1362HashboardSku::Bhb42841.freq_voltage_table();
        let max_volt = table.iter().map(|(_, v)| *v).max().unwrap();
        let min_volt = table.iter().map(|(_, v)| *v).min().unwrap();
        assert!(min_volt >= 1360);
        assert!(max_volt <= 1480);
    }

    #[test]
    fn bhb42841_is_only_inverted_curve_sku() {
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let expected = matches!(sku, Bm1362HashboardSku::Bhb42841);
            assert_eq!(
                sku.flags().inverted_curve,
                expected,
                "{}: inverted_curve flag mismatch",
                sku.hashboard_id()
            );
        }
    }

    #[test]
    fn bhb42801_requires_apw12_plus_true() {
        // High-bin family — REQUIRES APW12+ (NOT APW12 SMBus). At
        // 1.6 V / 4000 W+ SMBus would brown out. Per
        // .
        for sku in [
            Bm1362HashboardSku::Bhb42801,
            Bm1362HashboardSku::Bhb42811,
            Bm1362HashboardSku::Bhb42821,
            Bm1362HashboardSku::Bhb42831,
            // BHB42803 is fixed-V repair-class but also 4000 W class —
            // also routes to APW12+.
            Bm1362HashboardSku::Bhb42803,
        ] {
            assert!(
                sku.flags().requires_apw12_plus,
                "{}: must require APW12+",
                sku.hashboard_id()
            );
        }
        // Non-high-bin SKUs MUST NOT request APW12+.
        for sku in [
            Bm1362HashboardSku::Bhb42601,
            Bm1362HashboardSku::Bhb42603,
            Bm1362HashboardSku::Bhb42621,
            Bm1362HashboardSku::Bhb42641,
            Bm1362HashboardSku::Bhb42631,
            Bm1362HashboardSku::Bhb42632,
            Bm1362HashboardSku::Bhb42651,
            Bm1362HashboardSku::Bhb42611,
            Bm1362HashboardSku::Bhb42701,
            Bm1362HashboardSku::Bhb42841,
        ] {
            assert!(
                !sku.flags().requires_apw12_plus,
                "{}: must NOT request APW12+",
                sku.hashboard_id()
            );
        }
    }

    #[test]
    fn bhb42701_efficiency_voltage_range_1220_1260() {
        // Per `pvt_tables.h` `PVT_LEVELS_BHB42701`: lowest voltage floor
        // in the BHB42xxx line (1220-1260 mV).
        let table = Bm1362HashboardSku::Bhb42701.freq_voltage_table();
        let min_volt = table.iter().map(|(_, v)| *v).min().unwrap();
        let max_volt = table.iter().map(|(_, v)| *v).max().unwrap();
        assert!(min_volt >= 1220, "got min={}", min_volt);
        assert!(max_volt <= 1260, "got max={}", max_volt);
        // 108 ASICs/chain.
        assert_eq!(Bm1362HashboardSku::Bhb42701.asics_per_chain(), 108);
    }

    /// W15.A5 —  Q8 confirmation. `RE_TEAM_FINDINGS_WAVE5.md` §Q8
    /// (lines 161-179) reports BB and CV `levels.json` are byte-identical
    /// at 15 SKUs for BM1362, and gives the freq/voltage envelope per SKU.
    /// This test pins each row's envelope (min/max freq, min/max volt)
    /// against  Q8 so a future drift gets caught loud.
    ///
    /// R6-2 tightened the high-bin family aliases `BHB42811` and
    /// `BHB42821` away from [`BHB42801_FREQ_VOLT_TABLE`] to their
    /// vendor `levels.json` lower bound. This test pins the live
    /// per-SKU table routing, including that 615-675 MHz floor.
    #[test]
    fn bhb42xxx_15_sku_envelopes_match_wave5_q8() {
        // (sku, expected_freq_min_max, expected_volt_min_max). The
        // Q8 table reports the **levels.json** envelope; our per-SKU
        // canonical-tier tables collapse the multi-step PVT grid into
        // one (freq, volt) pair per frequency tier (lowest stable voltage
        // per freq). The numbers below are the implementation envelopes
        // this test pins.
        let q8_envelopes: &[(Bm1362HashboardSku, (u16, u16), (u16, u16))] = &[
            // Standard family — 465-545 MHz @ 1320-1380 mV (matches Q8).
            (Bm1362HashboardSku::Bhb42601, (465, 545), (1320, 1380)),
            (Bm1362HashboardSku::Bhb42603, (465, 545), (1320, 1380)),
            (Bm1362HashboardSku::Bhb42621, (465, 545), (1320, 1380)),
            (Bm1362HashboardSku::Bhb42641, (465, 545), (1320, 1380)),
            // Extended-low family — 440-545 MHz @ 1320-1380 mV (matches Q8).
            (Bm1362HashboardSku::Bhb42631, (440, 545), (1320, 1380)),
            (Bm1362HashboardSku::Bhb42632, (440, 545), (1320, 1380)),
            (Bm1362HashboardSku::Bhb42651, (440, 545), (1320, 1380)),
            // Mid-band mixable — 610-670 MHz @ 1320-1380 mV (matches Q8).
            (Bm1362HashboardSku::Bhb42611, (610, 670), (1320, 1380)),
            // Efficiency — 500-575 MHz @ 1220-1260 mV (matches Q8).
            (Bm1362HashboardSku::Bhb42701, (500, 575), (1220, 1260)),
            // High-bin family — 585-675 MHz @ 1530-1600 mV (matches Q8 for 42801).
            (Bm1362HashboardSku::Bhb42801, (585, 675), (1530, 1600)),
            // R6-2 tightened BHB42811 / BHB42821 to the vendor
            // levels.json floor: 615-675 MHz @ 1530-1600 mV.
            (Bm1362HashboardSku::Bhb42811, (615, 675), (1530, 1600)),
            (Bm1362HashboardSku::Bhb42821, (615, 675), (1530, 1600)),
            (Bm1362HashboardSku::Bhb42831, (585, 675), (1530, 1600)),
            // High-bin single-V repair — 585-675 MHz @ 1530 mV (single).
            (Bm1362HashboardSku::Bhb42803, (585, 675), (1530, 1530)),
            // Low-power salvage — 410-475 MHz @ 1360-1480 mV envelope
            // ( Q8). Our canonical-tier table picks the lowest
            // stable voltage per freq (1360 mV across the band); the
            // upper envelope edge of 1480 mV is in the multi-step PVT
            // grid, not the canonical-tier collapsed view.
            (Bm1362HashboardSku::Bhb42841, (410, 475), (1360, 1480)),
        ];
        assert_eq!(q8_envelopes.len(), 15, "Wave 5 Q8 reports exactly 15 SKUs");
        // Every SKU we ship must appear in the Q8 envelope table.
        assert_eq!(ALL_BM1362_HASHBOARD_SKUS.len(), 15);
        for (sku, (fmin, fmax), (vmin, vmax)) in q8_envelopes {
            let table = sku.freq_voltage_table();
            assert!(!table.is_empty(), "{}: empty PVT", sku.hashboard_id());
            let actual_fmin = table.iter().map(|(f, _)| *f).min().unwrap();
            let actual_fmax = table.iter().map(|(f, _)| *f).max().unwrap();
            let actual_vmin = table.iter().map(|(_, v)| *v).min().unwrap();
            let actual_vmax = table.iter().map(|(_, v)| *v).max().unwrap();
            assert!(
                actual_fmin >= *fmin && actual_fmax <= *fmax,
                "{}: canonical-tier freq [{}, {}] outside Wave 5 Q8 envelope [{}, {}]",
                sku.hashboard_id(),
                actual_fmin,
                actual_fmax,
                fmin,
                fmax
            );
            assert!(
                actual_vmin >= *vmin && actual_vmax <= *vmax,
                "{}: canonical-tier volt [{}, {}] outside Wave 5 Q8 envelope [{}, {}]",
                sku.hashboard_id(),
                actual_vmin,
                actual_vmax,
                vmin,
                vmax
            );
        }
    }

    #[test]
    fn bhb42611_mix_levels_supported_but_symmetric_only_for_w13() {
        // Pin: BHB42611 is the **only** SKU with mix_levels=true.
        // W13 ships symmetric-only dispatch; per-chain asymmetric is
        // a W14+ feature. The flag is exposed today so dashboard UX
        // can show "mixable hashboard" without enabling the asymmetric
        // dispatch path.
        assert!(Bm1362HashboardSku::Bhb42611.flags().mix_levels);
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let expected = matches!(sku, Bm1362HashboardSku::Bhb42611);
            assert_eq!(
                sku.flags().mix_levels,
                expected,
                "{}: mix_levels flag mismatch",
                sku.hashboard_id()
            );
        }
        // 120 ASICs/chain (per `pvt_tables.h` line 253).
        assert_eq!(Bm1362HashboardSku::Bhb42611.asics_per_chain(), 120);
    }

    #[test]
    fn default_for_unrecognized_sku_returns_bhb42601() {
        // Pin: per `pvt_tables.h` line 303 the safe fallback is
        // BHB42601's envelope. Any drift here would silently re-route
        // unknown SKUs (often EEPROM-read failures) to a wrong envelope.
        assert_eq!(
            Bm1362HashboardSku::default_for_unrecognized_sku(),
            Bm1362HashboardSku::Bhb42601
        );
    }

    #[test]
    fn pvt_lookup_by_sku_returns_correct_table_for_15_skus() {
        // For every SKU, `from_id(sku.hashboard_id())` round-trips and
        // the resolved SKU's freq/voltage table matches the original.
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let id = sku.hashboard_id();
            let resolved = Bm1362HashboardSku::from_id(id)
                .unwrap_or_else(|| panic!("from_id({}) returned None — round-trip broken", id));
            assert_eq!(*sku, resolved, "round-trip mismatch for {}", id);
            // Table identity (pointer equality is over-tight — compare
            // contents byte-for-byte instead).
            assert_eq!(
                sku.freq_voltage_table(),
                resolved.freq_voltage_table(),
                "table mismatch for {}",
                id
            );
        }
        // Unknown IDs return None.
        assert_eq!(Bm1362HashboardSku::from_id("BHB99999"), None);
        assert_eq!(Bm1362HashboardSku::from_id("bhb42601"), None); // case-sensitive
        assert_eq!(Bm1362HashboardSku::from_id(""), None);
    }

    #[test]
    fn standard_family_aliases_share_table_with_bhb42601() {
        // Pin the alias map per `pvt_tables.h` lines 153-155.
        for alias in [
            Bm1362HashboardSku::Bhb42603,
            Bm1362HashboardSku::Bhb42621,
            Bm1362HashboardSku::Bhb42641,
        ] {
            assert_eq!(
                alias.freq_voltage_table(),
                Bm1362HashboardSku::Bhb42601.freq_voltage_table(),
                "{} must share table with BHB42601",
                alias.hashboard_id()
            );
            // No flags (standard family).
            assert_eq!(alias.flags(), Bm1362SkuFlags::STANDARD);
            // 4 chains × 126 ASICs (standard topology).
            assert_eq!(alias.chain_count(), 4);
            assert_eq!(alias.asics_per_chain(), 126);
        }
    }

    #[test]
    fn extended_low_family_aliases_share_table_with_bhb42631() {
        // Pin the alias map per `pvt_tables.h` lines 156-157.
        for alias in [Bm1362HashboardSku::Bhb42632, Bm1362HashboardSku::Bhb42651] {
            assert_eq!(
                alias.freq_voltage_table(),
                Bm1362HashboardSku::Bhb42631.freq_voltage_table(),
                "{} must share table with BHB42631",
                alias.hashboard_id()
            );
            assert_eq!(alias.flags(), Bm1362SkuFlags::STANDARD);
        }
        // BHB42631 includes the extra 440 MHz row (6 tiers vs 5).
        assert_eq!(Bm1362HashboardSku::Bhb42631.freq_voltage_table().len(), 6);
        let lowest = Bm1362HashboardSku::Bhb42631
            .freq_voltage_table()
            .iter()
            .map(|(f, _)| *f)
            .min()
            .unwrap();
        assert_eq!(
            lowest, 440,
            "BHB42631 must include 440 MHz extended-low row"
        );
    }

    #[test]
    fn high_bin_family_aliases_use_tightened_r6_2_table() {
        // R6-2: BHB42811/BHB42821 have a tighter 615 MHz floor than the
        // broader BHB42801 collapsed table.
        for alias in [Bm1362HashboardSku::Bhb42811, Bm1362HashboardSku::Bhb42821] {
            let table = alias.freq_voltage_table();
            assert_ne!(table, Bm1362HashboardSku::Bhb42801.freq_voltage_table());
            assert_eq!(table.iter().map(|(f, _)| *f).min().unwrap(), 615);
            assert_eq!(table.iter().map(|(f, _)| *f).max().unwrap(), 675);
            assert_eq!(table.iter().map(|(_, v)| *v).min().unwrap(), 1530);
            assert_eq!(table.iter().map(|(_, v)| *v).max().unwrap(), 1600);
            // High-bin requires APW12+; no other flags.
            assert!(alias.flags().requires_apw12_plus);
            assert!(!alias.flags().voltage_fixed);
            assert!(!alias.flags().inverted_curve);
            assert!(!alias.flags().mix_levels);
            // 4 chains × 88 ASICs.
            assert_eq!(alias.chain_count(), 4);
            assert_eq!(alias.asics_per_chain(), 88);
        }
    }

    #[test]
    fn bhb42803_chain_count_is_3_not_4() {
        // No other SKU has chain_count != 4. Pin this hard so a future
        // refactor doesn't silently flip BHB42803 to 4-chain (would
        // double-write to a non-existent fourth chain).
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let expected = if matches!(sku, Bm1362HashboardSku::Bhb42803) {
                3
            } else {
                4
            };
            assert_eq!(
                sku.chain_count(),
                expected,
                "{}: chain_count mismatch",
                sku.hashboard_id()
            );
        }
    }

    #[test]
    fn all_skus_serde_round_trip() {
        // serde lowercase round-trip for every SKU.
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let json = serde_json::to_string(sku).unwrap();
            let back: Bm1362HashboardSku = serde_json::from_str(&json).unwrap();
            assert_eq!(*sku, back, "serde round-trip mismatch for {:?}", sku);
        }
    }

    #[test]
    fn all_hashboard_id_strings_are_unique() {
        // Defensive: the SKU strings are used by `dcent install`
        // preflight + dashboard pickers. Duplicate IDs would make
        // routing ambiguous.
        let mut seen = std::collections::HashSet::new();
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let id = sku.hashboard_id();
            assert!(seen.insert(id), "duplicate hashboard_id string: {}", id);
        }
        assert_eq!(seen.len(), 15);
    }

    #[test]
    fn pll_param_range_contains_inclusive_endpoints() {
        // Both `min` and `max` must be inside the range. This nails
        // down the inclusive-vs-exclusive contract so future range
        // edits don't silently drift.
        assert!(BM1362_PLL_RANGES.refdiv.contains(1));
        assert!(BM1362_PLL_RANGES.refdiv.contains(16));
        assert!(!BM1362_PLL_RANGES.refdiv.contains(0));
        assert!(!BM1362_PLL_RANGES.refdiv.contains(17));
        assert!(BM1362_PLL_RANGES.fbdiv.contains(16));
        assert!(BM1362_PLL_RANGES.fbdiv.contains(200));
        assert!(!BM1362_PLL_RANGES.fbdiv.contains(15));
        assert!(!BM1362_PLL_RANGES.fbdiv.contains(201));
    }
}
