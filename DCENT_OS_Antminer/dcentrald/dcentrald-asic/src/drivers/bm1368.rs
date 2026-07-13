//! BM1368 ASIC driver (Antminer S21, BitAxe Supra).
//!
//! The BM1368 is a 5nm SHA-256 ASIC used in the Antminer S21 and T21.
//! It shares the BM1366 architecture with minor register differences.
//!
//! Key characteristics:
//!   - 5nm process
//!   - ~894 cores per chip (same as BM1366)
//!   - ~1.2V core voltage (higher than older chips)
//!   - 11-byte response with 16-bit version field
//!   - Full header job format (ASIC computes midstate internally)
//!   - ASIC-internal hardware version rolling
//!   - Job ID increment: +24 mod 128
//!   - 3 Mbaud operational baud (verified on live S21)
//!   - 108 chips per chain on S21 (12 domains x 9)
//!   - NoPic model — no PIC microcontroller for voltage control
//!     (TAS5782M audio DACs repurposed as voltage controllers on S21)
//!   - PLL0 at 0x08 for hashing, PLL1 at 0x60 for baud
//!   - FB_DIV range: 144-235
//!   - CTRL_REG: BM139X mode (bit4=1)
//!   - S21 uses Amlogic A113D (serial UART, not FPGA UIO)
//!     but this driver is transport-agnostic via FpgaChain abstraction
//!
//! Register values from ESP-Miner and ASIC Register Bible:
//!   0x00 ChipAddress:  0x13680000 (ID=0x1368, addr=0x00)
//!   0x08 PLL0:         Variable (hash clock)
//!   0x10 HashCounting: 0x000015A4 (S21 stock default)
//!   0x14 TicketMask:   0x00000000 (reset)
//!   0x18 MiscControl:  0x0000C100 (reset, same as BM1366)
//!   0x28 FastUART:     0x11300200 (1 Mbaud config)
//!   0x3C CoreRegCtrl:  Init values differ from BM1366
//!   0x54 AnalogMux:    0x00000003 (temp diode enable)
//!   0x58 IODriver:     0x02111111 (drive strength)
//!   0xA4 VersionRoll:  0x9000FFFF (version mask)
//!   0xA8 InitControl:  0x000700F0 (per-chip init)

use crate::drivers::{ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1368 chip ID.
pub const CHIP_ID: u16 = 0x1368;

/// BM1368 chips per chain on S21 (verified from live probe).
pub const CHIPS_PER_CHAIN_S21: u8 = 108;

/// S21 fixture uses address_interval=2 (108 chips across 12 voltage domains).
pub const FIXTURE_ADDRESS_INTERVAL: u8 = 2;

/// Bitmain fixture uses ticket mask 0x7f for BM1368.
pub const FIXTURE_TICKET_MASK: u32 = 0x0000_007F;

/// BM1366/BM1368 family UART relay register.
pub const UART_RELAY_REG: u8 = 0x2C;

/// BM1366-style UART relay value used for 12-domain BM1368 boards.
pub const UART_RELAY_12_DOMAIN: u32 = 0x007C_0003;

/// BM1368 response size (11 bytes: nonce + midstate_num + job_id + version + status).
pub const RESPONSE_LENGTH: usize = 11;

/// Number of small cores per core group (16 for BM1368, vs 8 for BM1366).
const SMALL_CORE_COUNT: u8 = 16;

/// Job ID increment per work dispatch (BM1368 uses +24 mod 128).
pub const JOB_ID_INCREMENT: u8 = 24;

/// BM1368 PLL reference frequency (25 MHz crystal oscillator).
const FREQ_MULT: f64 = 25.0;

/// Minimum feedback divider value for PLL search.
const FB_DIV_MIN: u16 = 144;

/// Maximum feedback divider value for PLL search.
const FB_DIV_MAX: u16 = 235;

/// BM1368 PLL VCO lock-range bounds — **Bitmain-canonical, from the unstripped
/// S21 (BM1368) jig** `single_board_test.dec/get_pllparam_divider@CF634` (RE
/// 2026-06-02): the jig accepts a PLL config only when `2000 ≤ VCO ≤ 3200` MHz,
/// additionally `VCO ≤ 3125` when `REFDIV == 1` (`VCO = 25 MHz × FBDIV/REFDIV`)
/// — byte-identical to the BM1370 S21 Pro jig constraint.
///
/// The curated [`BM1368_PLL_TABLE`] (REFDIV=2, FBDIV 160-225) is already fully
/// inside this range (VCO 2000-2812; pinned by
/// `bm1368_pll_lookup_table_within_jig_vco_range`). The **brute-force fallback**
/// `bm1368_pll_search` (FBDIV 144-235, REFDIV 1-2) has NO VCO clamp, so for
/// off-table targets it can select a REFDIV=1 / VCO 3600-5875 config OR a
/// REFDIV=2 / VCO 1800-1987 config (low FBDIV) that the jig would reject. Same
/// finding-class + gate as BM1370 ([`super::bm1370`]); see RE-ASK-BM1370-RAMP-VCO.
const PLL_VCO_MIN_MHZ: f64 = 2000.0;
const PLL_VCO_MAX_MHZ: f64 = 3200.0;
const PLL_VCO_MAX_REFDIV1_MHZ: f64 = 3125.0;

/// Env gate (default-OFF): constrain the BM1368 PLL **fallback** search to the
/// Bitmain S21-jig VCO lock range. OFF = byte-identical to ESP-Miner (the curated
/// lookup table is unaffected — it's already in range). ON = the off-table
/// fallback never selects a config the jig would reject. Resolve the true BM1368
/// VCO range on the live `a lab unit` S21 alongside RE-ASK-BM1370-RAMP-VCO.
const JIG_VCO_CLAMP_ENV: &str = "DCENT_BM1368_JIG_VCO_CLAMP";

/// `true` iff `vco` is inside the Bitmain S21 (BM1368) jig's accepted VCO range
/// for `refdiv`.
fn vco_in_jig_range(vco: f64, refdiv: u8) -> bool {
    let cap = if refdiv == 1 {
        PLL_VCO_MAX_REFDIV1_MHZ
    } else {
        PLL_VCO_MAX_MHZ
    };
    (PLL_VCO_MIN_MHZ..=PLL_VCO_MAX_MHZ).contains(&vco) && vco <= cap
}

/// BM1368 register addresses.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 parameter register (hash clock PLL).
    pub const PLL0_PARAM: u8 = 0x08;
    /// Hash counting number register (nonce range / clock divider).
    pub const HASH_COUNTING: u8 = 0x10;
    /// Ticket mask register (hardware difficulty filter).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc control register (baud rate, misc config).
    pub const MISC_CONTROL: u8 = 0x18;
    /// Fast UART configuration register (BM1366+ baud control).
    pub const FAST_UART: u8 = 0x28;
    /// Core register control (indirect core access).
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// Analog mux control (temperature diode).
    pub const ANALOG_MUX: u8 = 0x54;
    /// IO driver strength.
    pub const IO_DRIVER: u8 = 0x58;
    /// Version rolling mask register.
    pub const VERSION_ROLLING: u8 = 0xA4;
    /// Init control register (used during per-chip init).
    pub const REG_A8: u8 = 0xA8;
}

// ---------------------------------------------------------------------------
// ESP-Miner verified init register values (from esp-miner-asic-driver-analysis)
// ---------------------------------------------------------------------------

/// Version mask register value: 0x9000FFFF
/// Encodes: prefix 0x9000 + (version_mask >> 13) where mask = 0x1FFFE000.
/// 0x1FFFE000 >> 13 = 0xFFFF, so register = 0x9000FFFF.
const VERSION_MASK_REG: u32 = 0x9000_FFFF;

/// Reg 0xA8 broadcast init value.
const REG_A8_BCAST_INIT: u32 = 0x0007_0000;

/// Misc control broadcast init value.
/// ESP-Miner: {0xFF, 0x0F, 0xC1, 0x00} = 0xFF0FC100.
const MISC_CTRL_BCAST_INIT: u32 = 0xFF0F_C100;

// === CoreRegCtrl (reg 0x3C) — RESOLVED 2026-06-10 from the fully-symbolized S21
// jig `single_board_test21` (BM1368). These three writes were previously
// labelled `CORE_REG_UNKNOWN` in `bm1366.rs`; the jig names + bit-decodes them.
// VALUES ARE UNCHANGED (DCENT's ESP-Miner-sourced values are accepted-share-proven
// on `a lab unit`); the doc below records the factory bit-layout + a flagged
// factory-vs-ESP-Miner field difference for a future gated A/B (NOT a blind edit).

/// CoreRegCtrl #1 — `set_clock_select_control`:
///   `0x80008B00 | ((pulse_mode & 3) << 1)` (jig FUN_000cf088).
/// DCENT = pulse_mode 0. The S21 factory `Config.ini` uses **pulse_mode=1**
/// (→ 0x80008B02). Non-blocking difference; candidate gated A/B.
const CORE_REG_CTRL_1: u32 = 0x8000_8B00;

/// CoreRegCtrl #2 — `set_clock_delay_control`:
///   `0x80008000 | ((pwth_sel & 7) << 3) | (ccdly_sel << 6) | swpf` (jig FUN_000cf0b8).
/// DCENT 0x18 = **pwth_sel=3**, ccdly=0, swpf=0. The S21/S19k factory `Config.ini`
/// uses **pwth_sel=4** (→ 0x80008020). Non-blocking; candidate gated A/B.
const CORE_REG_CTRL_2: u32 = 0x8000_8018;

/// CoreRegCtrl #3 — per-chip clock-distribution fixed write `0x800082AA`
/// (jig: third CoreRegCtrl 0x3C write; base differs from #1/#2 — a fixed
/// core-init constant, byte-confirmed identical to the jig).
const CORE_REG_CTRL_3: u32 = 0x8000_82AA;

/// Extra ticket mask init write.
/// Bitmain's fixture uses 0x7f for BM1368.
const TICKET_MASK_INIT: u32 = FIXTURE_TICKET_MASK;

/// Analog mux control value (temp diode).
/// BM1368 uses 0x03 (same as BM1366).
const ANALOG_MUX_VAL: u32 = 0x0000_0003;

/// IO driver strength value.
/// BM1368: {0x02, 0x11, 0x11, 0x11} = 0x02111111.
const IO_DRIVER_VAL: u32 = 0x0211_1111;

/// Reg 0xA8 per-chip init value.
const REG_A8_PER_CHIP: u32 = 0x0007_01F0;

/// Misc control per-chip init value.
/// ESP-Miner: {0xF0, 0x00, 0xC1, 0x00} = 0xF000C100.
const MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;

/// Fast UART configuration value (1 Mbaud on BM1366/68/70).
/// ESP-Miner: {0x11, 0x30, 0x02, 0x00} = 0x11300200.
///
/// This is the COMPILED DEFAULT and stays 1 Mbaud — see `FAST_UART_VALUE_3M`
/// + `resolve_fast_uart_value()` for the default-OFF 3 Mbaud capability.
const FAST_UART_CONFIG: u32 = 0x1130_0200;

/// PERF (W8 BM1368 FastUART target): env-override name for the BM1368 FastUART
/// (reg 0x28) value.
///
/// Mirrors the BM1362 `DCENT_AM2_FAST_UART_VALUE` convention
/// (`bm1362::FAST_UART_VALUE_ENV`). Accepts decimal or `0x`/`0X`-prefixed hex.
/// Exposed as the single source of truth for the gate name so the driver and
/// any daemon-side reader agree.
pub const FAST_UART_VALUE_ENV: &str = "DCENT_S21_FAST_UART_VALUE";

/// PERF (W8 BM1368 FastUART target): the 3 Mbaud FastUART byte order
/// (`0x1130_0000`).
///
/// ESP-Miner's `BM1368_set_max_baud()` writes the FastUART register `0x28` as
/// `{0x11, 0x30, 0x02, 0x00}` = `0x1130_0200`, which yields **1 Mbaud** — and
/// that is what DCENT_OS compiles in by default (`FAST_UART_CONFIG`). Stock
/// Bitmain / VNish run the S21 chain UART at **3 Mbaud** (live-confirmed on the
/// `a lab unit` S21: `S21_BRAIINSOS_DEEP_PROBE.md` reports `ttyS2 @ 3000000`). On the
/// BM1366/68/70 family the FastUART register's BT8D divisor lives in
/// byte[2:3]; `0x0200` → 1 Mbaud, and zeroing the divisor (`0x0000`, the
/// `0x1130_0000` byte order observed in the RE-018 cold-bosminer capture and
/// already shipped for BM1362 as `bm1362::FAST_UART_VALUE_RE018`) selects the
/// chip's maximum fast baud (~3 Mbaud).
///
/// **This is NOT the compiled default.** Flipping it would change the fast-UART
/// units of every live S21 *before* an operator live-A/B has confirmed the new
/// byte order produces accepted shares at 3 Mbaud (the FPGA / Amlogic host UART
/// divider must also be re-paced to match — a transport-layer change). It is
/// applied ONLY when the operator sets `DCENT_S21_FAST_UART_VALUE=0x11300000`.
/// Flagged for operator live-A/B.
pub const FAST_UART_VALUE_3M: u32 = 0x1130_0000;

/// PERF (W8 BM1368 FastUART target): resolve the effective BM1368 FastUART
/// value from an optional operator override string (the raw
/// `DCENT_S21_FAST_UART_VALUE` value).
///
/// Pure helper so host tests can pin BOTH states without env mutation:
///   - `None` / unparseable  → the compiled 1 Mbaud default `0x1130_0200`
///     (load-bearing — never silently changes the live fast-UART units).
///   - `Some("0x11300000")`  → the 3 Mbaud byte order `0x1130_0000`.
///
/// Accepts decimal or `0x`/`0X`-prefixed hex, mirroring
/// `bm1362::resolve_fast_uart_value`. A malformed override falls back to the
/// default so a typo can never reprogram the chip's UART.
pub fn resolve_fast_uart_value(override_raw: Option<&str>) -> u32 {
    let Some(raw) = override_raw else {
        return FAST_UART_CONFIG;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return FAST_UART_CONFIG;
    }
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        trimmed.parse::<u32>()
    };
    parsed.unwrap_or(FAST_UART_CONFIG)
}

/// Hash counting register value (S21 stock default).
/// ESP-Miner: {0x00, 0x00, 0x15, 0xA4} = 0x000015A4.
const HASH_COUNTING_VAL: u32 = 0x0000_15A4;

/// Default ASIC difficulty for BM1368 (from S21 live probe).
const DEFAULT_ASIC_DIFFICULTY: u32 = 128;

// ---------------------------------------------------------------------------
// PLL computation
// ---------------------------------------------------------------------------

/// Bitmain-verified BM1368 PLL lookup table.
///
/// Source: S21 fixture test jig (`single_board_test` ch0_0.log, 2023-09-14).
/// 68 entries from 56.25 MHz to 475.00 MHz in 6.25 MHz steps.
/// All entries: refdiv=2, usr_divider=1, zero PLL error (exact lock).
///
/// Format: (freq_mhz_x100, fbdiv, postdiv1, postdiv2)
/// freq_mhz_x100 avoids floating point: 5625 = 56.25 MHz, 40000 = 400.00 MHz
const BM1368_PLL_TABLE: &[(u16, u8, u8, u8)] = &[
    (5625, 162, 6, 6),
    (6250, 175, 7, 5),
    (6875, 165, 6, 5),
    (7500, 168, 7, 4),
    (8125, 182, 7, 4),
    (8750, 168, 6, 4),
    (9375, 180, 6, 4),
    (10000, 168, 7, 3),
    (10625, 170, 5, 4),
    (11250, 162, 6, 3),
    (11875, 171, 6, 3),
    (12500, 180, 6, 3),
    (13125, 189, 6, 3),
    (13750, 165, 5, 3),
    (14375, 161, 7, 2),
    (15000, 168, 7, 2),
    (15625, 175, 7, 2),
    (16250, 182, 7, 2),
    (16875, 162, 6, 2),
    (17500, 168, 6, 2),
    (18125, 174, 6, 2),
    (18750, 180, 6, 2),
    (19375, 186, 6, 2),
    (20000, 160, 5, 2),
    (20625, 165, 5, 2),
    (21250, 170, 5, 2),
    (21875, 175, 5, 2),
    (22500, 180, 5, 2),
    (23125, 185, 5, 2),
    (23750, 190, 5, 2),
    (24375, 195, 5, 2),
    (25000, 160, 4, 2),
    (25625, 164, 4, 2),
    (26250, 168, 4, 2),
    (26875, 172, 4, 2),
    (27500, 176, 4, 2),
    (28125, 180, 4, 2),
    (28750, 161, 7, 1),
    (29375, 188, 4, 2),
    (30000, 168, 7, 1),
    (30625, 196, 4, 2),
    (31250, 175, 7, 1),
    (31875, 204, 4, 2),
    (32500, 182, 7, 1),
    (33125, 212, 4, 2),
    (33750, 162, 6, 1),
    (34375, 165, 6, 1),
    (35000, 168, 6, 1),
    (35625, 171, 6, 1),
    (36250, 174, 6, 1),
    (36875, 177, 6, 1),
    (37500, 180, 6, 1),
    (38125, 183, 6, 1),
    (38750, 186, 6, 1),
    (39375, 189, 6, 1),
    (40000, 160, 5, 1),
    (40625, 195, 6, 1),
    (41250, 165, 5, 1),
    (41875, 201, 6, 1),
    (42500, 170, 5, 1),
    (43125, 207, 6, 1),
    (43750, 175, 5, 1),
    (44375, 213, 6, 1),
    (45000, 180, 5, 1),
    (45625, 219, 6, 1),
    (46250, 185, 5, 1),
    (46875, 225, 6, 1),
    (47500, 190, 5, 1),
];

/// Look up PLL parameters from Bitmain's verified table.
/// Returns (fbdiv, refdiv, postdiv1, postdiv2, actual_freq) or None if not in table.
fn bm1368_pll_lookup(target_mhz: f64) -> Option<(u8, u8, u8, u8, f64)> {
    // Convert to x100 integer for lookup (e.g., 400.0 → 40000)
    let target_x100 = (target_mhz * 100.0).round() as u16;

    // Find closest entry (within 3.125 MHz = half a step)
    for &(freq_x100, fbdiv, pd1, pd2) in BM1368_PLL_TABLE {
        if freq_x100 == target_x100 {
            let actual = FREQ_MULT * fbdiv as f64 / (2.0 * pd1 as f64 * pd2 as f64);
            return Some((fbdiv, 2, pd1, pd2, actual));
        }
    }

    // Try nearest 6.25 MHz step
    let snapped = ((target_mhz / 6.25).round() * 6.25 * 100.0).round() as u16;
    for &(freq_x100, fbdiv, pd1, pd2) in BM1368_PLL_TABLE {
        if freq_x100 == snapped {
            let actual = FREQ_MULT * fbdiv as f64 / (2.0 * pd1 as f64 * pd2 as f64);
            return Some((fbdiv, 2, pd1, pd2, actual));
        }
    }

    None
}

/// Top of Bitmain's verified BM1368 PLL lookup table (x100 MHz) = 475.00 MHz.
const BM1368_PLL_TABLE_MAX_X100: u32 = 47500;

/// PERF-005: capability ceiling (x100 MHz) for the BM1368 PLL ramp = 600.00 MHz.
///
/// The verified fixture table tops out at 475 MHz; the autotuner's
/// `common_frequencies()` already exposes targets up to 600 MHz. Previously the
/// ramp HARD-clamped the target to 475 MHz, so any request 475 < f ≤ 600 MHz
/// silently capped at 475 (the chip never reached its commanded frequency).
/// This ceiling lets the ramp continue above the table via the existing
/// brute-force PLL search. **This is a CAPABILITY ceiling, not a default**: the
/// effective operating frequency is still whatever the config/preset requests
/// (S21 default stays in the table window), so a default tune produces
/// byte-identical ramp output to before. Raising the *requested* frequency is a
/// separate, operator-driven config change.
const BM1368_PLL_RAMP_MAX_X100: u32 = 60000;

/// Build the fixture-style PLL ramp from the BM1368 default 50 MHz state.
///
/// The programmable table starts at 56.25 MHz, so the first explicit write is
/// 56.25 MHz and then increments in 6.25 MHz steps up to the snapped target.
///
/// PERF-005: for targets above the verified table max (475 MHz) and up to the
/// capability ceiling (600 MHz), the ramp first walks the whole verified table,
/// then appends brute-force-searched 6.25 MHz steps for the 475→target segment
/// so the chip is ramped (not slammed) all the way to the commanded frequency.
pub fn pll_ramp_sequence(target_mhz: u16) -> Vec<(u32, u32)> {
    let target_x100 = ((target_mhz as u32 * 100 + 312) / 625) * 625;
    let clamped_target = target_x100.clamp(5625, BM1368_PLL_RAMP_MAX_X100);
    let mut steps = Vec::new();

    // Phase 1: verified-table steps up to min(target, table max).
    let table_target = clamped_target.min(BM1368_PLL_TABLE_MAX_X100);
    for &(freq_x100, fbdiv, pd1, pd2) in BM1368_PLL_TABLE {
        let freq_x100 = freq_x100 as u32;
        if freq_x100 > table_target {
            break;
        }
        let reg = bm1368_pll_encode(fbdiv, 2, pd1, pd2);
        steps.push((reg, freq_x100));
    }

    // Phase 2 (PERF-005): for targets above the table, continue ramping in
    // 6.25 MHz steps using the brute-force PLL search. Keeps the staged
    // ramp behavior above the fixture window instead of a single slam.
    if clamped_target > BM1368_PLL_TABLE_MAX_X100 {
        let mut next = BM1368_PLL_TABLE_MAX_X100 + 625;
        while next <= clamped_target {
            let mhz = next as f64 / 100.0;
            let (fbdiv, refdiv, pd1, pd2, _) = bm1368_pll_search(mhz);
            steps.push((bm1368_pll_encode(fbdiv, refdiv, pd1, pd2), next));
            next += 625;
        }
        // Ensure the final step lands exactly on the (clamped) target if the
        // 6.25 MHz cadence didn't divide evenly into it.
        if steps.last().map(|&(_, f)| f) != Some(clamped_target) {
            let mhz = clamped_target as f64 / 100.0;
            let (fbdiv, refdiv, pd1, pd2, _) = bm1368_pll_search(mhz);
            steps.push((bm1368_pll_encode(fbdiv, refdiv, pd1, pd2), clamped_target));
        }
    }

    if steps.is_empty() {
        let (fbdiv, refdiv, pd1, pd2, _) = bm1368_pll_search(target_mhz as f64);
        steps.push((
            bm1368_pll_encode(fbdiv, refdiv, pd1, pd2),
            target_mhz as u32 * 100,
        ));
    }

    steps
}

/// PLL parameters for the BM1366/BM1368/BM1370 family.
///
/// First checks Bitmain's verified lookup table (from S21 fixture test jig).
/// Falls back to ESP-Miner brute-force search for frequencies not in the table.
///
///   freq = FREQ_MULT * fb_div / (ref_div * postdiv1 * postdiv2)
///
/// Constraints:
///   - ref_div: 1 or 2
///   - postdiv1: 1..=7
///   - postdiv2: 1..=7
///   - postdiv1 > postdiv2
///   - fb_div: FB_DIV_MIN..=FB_DIV_MAX (144-235 for BM1368)
///
/// Selects: closest frequency, then lowest VCO, then lowest postdiv product.
fn bm1368_pll_search(target_mhz: f64) -> (u8, u8, u8, u8, f64) {
    // Try Bitmain's verified lookup table first
    if let Some(params) = bm1368_pll_lookup(target_mhz) {
        return params;
    }
    // Fall back to brute-force search (Bitmain-jig VCO clamp opt-in — same
    // gate/finding as BM1370; OFF = byte-identical ESP-Miner behaviour).
    let clamp_vco = std::env::var(JIG_VCO_CLAMP_ENV).as_deref() == Ok("1");
    bm1368_pll_fallback(target_mhz, clamp_vco)
}

/// Brute-force PLL search fallback (off-table targets), with the optional
/// Bitmain-jig VCO clamp. Separated from [`bm1368_pll_search`] so the clamp is
/// deterministically testable without env races.
fn bm1368_pll_fallback(target_mhz: f64, clamp_vco: bool) -> (u8, u8, u8, u8, f64) {
    let mut best_fb: u8 = 144;
    let mut best_ref: u8 = 1;
    let mut best_pd1: u8 = 1;
    let mut best_pd2: u8 = 1;
    let mut best_freq: f64 = 0.0;
    let mut best_diff: f64 = f64::MAX;
    let mut best_vco: f64 = f64::MAX;
    let mut best_pdprod: u8 = u8::MAX;

    for ref_div in [1u8, 2] {
        for postdiv1 in 1u8..=7 {
            for postdiv2 in 1u8..=7 {
                if postdiv1 <= postdiv2 && postdiv1 != postdiv2 {
                    continue;
                }
                // postdiv1 must be > postdiv2 (ESP-Miner constraint),
                // OR they can be equal (both 1).
                if postdiv1 < postdiv2 {
                    continue;
                }
                for fb_div in FB_DIV_MIN..=FB_DIV_MAX {
                    let freq = FREQ_MULT * fb_div as f64
                        / (ref_div as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target_mhz).abs();
                    let vco = FREQ_MULT * fb_div as f64 / ref_div as f64;
                    let pdprod = postdiv1 * postdiv2;

                    // Skip configs the S21 jig would reject as out-of-VCO-range
                    // (gated; OFF = byte-identical ESP-Miner behaviour).
                    if clamp_vco && !vco_in_jig_range(vco, ref_div) {
                        continue;
                    }

                    let better = diff < best_diff
                        || (diff == best_diff && vco < best_vco)
                        || (diff == best_diff && vco == best_vco && pdprod < best_pdprod);

                    if better {
                        best_fb = fb_div as u8;
                        best_ref = ref_div;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                        best_freq = freq;
                        best_diff = diff;
                        best_vco = vco;
                        best_pdprod = pdprod;
                    }
                }
            }
        }
    }

    (best_fb, best_ref, best_pd1, best_pd2, best_freq)
}

/// Encode PLL parameters into the 32-bit register value for BM1368.
///
/// Register 0x08 byte layout:
///   Byte 0: VDO_SCALE (0x40 if VCO < 2400 MHz, 0x50 if >= 2400 MHz)
///   Byte 1: FBDIV
///   Byte 2: REFDIV
///   Byte 3: ((POSTDIV1-1) << 4) | (POSTDIV2-1)
fn bm1368_pll_encode(fb_div: u8, ref_div: u8, postdiv1: u8, postdiv2: u8) -> u32 {
    let vco = FREQ_MULT * fb_div as f64 / ref_div as f64;
    let vdo_scale: u8 = if vco >= 2400.0 { 0x50 } else { 0x40 };
    let postdiv_byte = ((postdiv1.saturating_sub(1)) << 4) | postdiv2.saturating_sub(1);

    ((vdo_scale as u32) << 24)
        | ((fb_div as u32) << 16)
        | ((ref_div as u32) << 8)
        | (postdiv_byte as u32)
}

/// BM1368 driver implementation.
pub struct Bm1368Driver;

impl Default for Bm1368Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1368Driver {
    pub fn new() -> Self {
        Self
    }

    /// Calculate a WORK_TIME register value for BM1368.
    ///
    /// BM1368 uses on-chip version rolling, so the effective nonce space
    /// per work item is larger. The FPGA work_time counter runs at 100 MHz.
    /// For S21 at 550 MHz: interval ~500ms / 108 chips = ~4.6ms per chip.
    ///
    /// Formula: work_time = nonce_range / freq_Hz * FPGA_WORK_CLK
    /// With version rolling: nonce_range = 2^32 (full 4 GH range per chip).
    /// But work is dispatched across all chips, so we divide by chip_count.
    pub fn calculate_work_time(_freq_mhz: u16, chip_count: u8) -> u32 {
        const FPGA_WORK_CLK: f64 = 100_000_000.0;
        // 500ms interval divided by chip count, converted to FPGA clock ticks
        let interval_s = 0.5 / chip_count as f64;
        let work_time = (interval_s * FPGA_WORK_CLK) as u32;
        work_time.max(1)
    }

    fn read_pll_register(chain: &mut FpgaChain, chip_addr: u8) -> Result<Option<u32>> {
        crate::drivers::bm139x::read_pll_register(chain, chip_addr, regs::PLL0_PARAM)
    }

    fn pll_register_to_freq(raw_reg: u32) -> Option<u16> {
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        let masked = raw_reg & !PLL_LOCK_BIT;
        MinerProfile::pll_frequencies_for_chip(CHIP_ID)
            .iter()
            .copied()
            .find(|&freq| Bm1368Driver::new().pll_params(freq).reg_value == masked)
    }

    /// Write a register to all chips via broadcast CMD.
    ///
    /// Uses the 2-word FIFO encoding (fifo_cmd_write_reg_bcast_full).
    fn write_reg_broadcast(chain: &mut FpgaChain, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_bcast_full(reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Write a register to a single chip via CMD.
    ///
    /// Uses the 2-word FIFO encoding (fifo_cmd_write_reg_full).
    fn write_reg_single(chain: &mut FpgaChain, chip_addr: u8, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_full(chip_addr, reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }
}

impl ChipDriver for Bm1368Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1368"
    }

    fn cores_per_chip(&self) -> u32 {
        // 1280 cores per BM1368: 80 big cores x 16 small cores.
        // Confirmed by Bitmain S21 fixture test RE (2026-04-12):
        //   small_core_in_big_core = 16, 108 ASICs x 1280 cores x 8 patterns = 1,102,464 nonces
        // Previous value 894 was incorrectly carried from BM1366 before fixture analysis.
        1280
    }

    fn response_length(&self) -> usize {
        RESPONSE_LENGTH
    }

    fn default_baud(&self) -> u32 {
        115_200
    }

    fn max_baud(&self) -> u32 {
        // 3 Mbaud tested on live S21 (S21 deep probe confirms ttyS2 at 3000000).
        // ESP-Miner uses 1 Mbaud via FAST_UART register.
        3_000_000
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1368: Configuring {} chips at {} MHz",
            chip_count,
            freq_mhz,
        );

        // =====================================================================
        // BM1368 init sequence (from ESP-Miner bm1368.c BM1368_init())
        //
        // The BM1368 uses the BM1366-style init with slight register differences.
        // This sequence is transport-agnostic — works over FPGA UIO (S9 control
        // board with S21 hash boards) or serial UART (native S21).
        // =====================================================================

        // Step 1: Set FPGA baud to 115200 for configuration commands.
        chain.set_baud(fpga_chain::BAUD_REG_115200);
        tracing::debug!(chain_id = chain.chain_id, "FPGA baud set to 115200");

        // Step 2: Version mask (sent 4 times — BM1368 requires 4x, vs 3x for BM1366).
        // This configures the hardware version rolling mask before chip enumeration.
        for i in 0..4 {
            Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, VERSION_MASK_REG);
            if i == 0 {
                tracing::debug!(
                    chain_id = chain.chain_id,
                    value = format_args!("0x{:08X}", VERSION_MASK_REG),
                    "Version mask set (x4)",
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Step 3: Bulk init registers (broadcast to all chips).
        // These configure the chip's core infrastructure before per-chip setup.

        // 3a) Reg 0xA8 (init control)
        Self::write_reg_broadcast(chain, regs::REG_A8, REG_A8_BCAST_INIT);
        tracing::debug!(
            chain_id = chain.chain_id,
            "Reg 0xA8 = 0x{:08X}",
            REG_A8_BCAST_INIT,
        );

        // 3b) Misc control
        Self::write_reg_broadcast(chain, regs::MISC_CONTROL, MISC_CTRL_BCAST_INIT);
        tracing::debug!(
            chain_id = chain.chain_id,
            "MiscCtrl = 0x{:08X}",
            MISC_CTRL_BCAST_INIT,
        );

        // 3c) Core register control — first write
        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, CORE_REG_CTRL_1);
        tracing::debug!(
            chain_id = chain.chain_id,
            "CoreRegCtrl[1] = 0x{:08X}",
            CORE_REG_CTRL_1,
        );

        // 3d) Core register control — second write
        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, CORE_REG_CTRL_2);
        tracing::debug!(
            chain_id = chain.chain_id,
            "CoreRegCtrl[2] = 0x{:08X}",
            CORE_REG_CTRL_2,
        );

        // 3e) Ticket mask init write (BM1368 extra — not present in BM1366 init)
        Self::write_reg_broadcast(chain, regs::TICKET_MASK, TICKET_MASK_INIT);
        tracing::debug!(
            chain_id = chain.chain_id,
            "TicketMask init = 0x{:08X}",
            TICKET_MASK_INIT,
        );

        // 3f) Analog mux control (temp diode)
        Self::write_reg_broadcast(chain, regs::ANALOG_MUX, ANALOG_MUX_VAL);
        tracing::debug!(
            chain_id = chain.chain_id,
            "AnalogMux = 0x{:08X}",
            ANALOG_MUX_VAL,
        );

        // 3g) IO driver strength
        Self::write_reg_broadcast(chain, regs::IO_DRIVER, IO_DRIVER_VAL);
        tracing::debug!(
            chain_id = chain.chain_id,
            "IODriver = 0x{:08X}",
            IO_DRIVER_VAL,
        );

        // S21 fixture enables the UART relay before the core reset/ramp stages.
        Self::write_reg_single(chain, 0x00, UART_RELAY_REG, UART_RELAY_12_DOMAIN);
        tracing::debug!(
            chain_id = chain.chain_id,
            "UARTRelay = 0x{:08X} on reg 0x{:02X}",
            UART_RELAY_12_DOMAIN,
            UART_RELAY_REG,
        );

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Step 4: Per-chip configuration.
        // Each chip gets individual register writes with its assigned address.
        // BM1368 requires 500ms delay between per-chip configurations (ESP-Miner).
        let addr_interval = if chip_count == CHIPS_PER_CHAIN_S21 {
            FIXTURE_ADDRESS_INTERVAL as u16
        } else if chip_count > 0 {
            256 / chip_count as u16
        } else {
            256
        };

        for i in 0..chip_count {
            let chip_addr = (i as u16 * addr_interval) as u8;

            // 4a) Reg 0xA8 per-chip
            Self::write_reg_single(chain, chip_addr, regs::REG_A8, REG_A8_PER_CHIP);

            // 4b) Misc control per-chip
            Self::write_reg_single(chain, chip_addr, regs::MISC_CONTROL, MISC_CTRL_PER_CHIP);

            // 4c) Core register control — first (same as broadcast)
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_CTRL_1);

            // 4d) Core register control — second (same as broadcast)
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_CTRL_2);

            // 4e) Core register control — third (clock distribution)
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_CTRL_3);

            // BM1368 requires 500ms delay between per-chip init (ESP-Miner).
            // For large chains (108 chips) this totals ~54 seconds.
            // Use a shorter delay for FPGA-mediated chains where timing is faster.
            if chip_count > 1 {
                // Scale delay: 500ms for serial UART, shorter for FPGA.
                // On FPGA chains, commands are buffered and very fast.
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        tracing::info!(
            chain_id = chain.chain_id,
            "Per-chip init complete for {} chips",
            chip_count,
        );

        // Step 5: Set difficulty mask (broadcast).
        let mask = self.ticket_mask(DEFAULT_ASIC_DIFFICULTY);
        Self::write_reg_broadcast(chain, regs::TICKET_MASK, mask);
        tracing::info!(
            chain_id = chain.chain_id,
            mask = format_args!("0x{:08X}", mask),
            "TicketMask set to difficulty {} (mask=0x{:08X})",
            DEFAULT_ASIC_DIFFICULTY,
            mask,
        );

        // Step 6: Frequency ramp in 6.25 MHz steps, matching the fixture flow.
        for (pll_reg, freq_x100) in pll_ramp_sequence(freq_mhz) {
            Bm1368Driver::write_reg_broadcast(chain, regs::PLL0_PARAM, pll_reg);
            std::thread::sleep(std::time::Duration::from_millis(10));
            tracing::debug!(
                chain_id = chain.chain_id,
                pll_reg = format_args!("0x{:08X}", pll_reg),
                "PLL ramp step {}.{:02} MHz",
                freq_x100 / 100,
                freq_x100 % 100,
            );
        }

        // Step 7: Fast UART configuration (baud upgrade).
        // Set the ASIC's internal UART to fast mode via register 0x28.
        //
        // The compiled default is 1 Mbaud (`FAST_UART_CONFIG` = 0x11300200,
        // ESP-Miner `BM1368_set_max_baud`). Stock/VNish run the S21 chain at
        // 3 Mbaud; the default-OFF `DCENT_S21_FAST_UART_VALUE=0x11300000`
        // override selects that — env unset ⇒ byte-identical 1 Mbaud, so live
        // S21 behavior is unchanged until an operator opts in for live-A/B.
        // NOTE: lifting the ASIC to 3 Mbaud also requires the host/FPGA UART
        // divider (Step 8 below) to be re-paced to match — flagged followup.
        let fast_uart_value =
            resolve_fast_uart_value(std::env::var(FAST_UART_VALUE_ENV).ok().as_deref());
        Self::write_reg_broadcast(chain, regs::FAST_UART, fast_uart_value);
        tracing::info!(
            chain_id = chain.chain_id,
            fast_uart = format_args!("0x{:08X}", fast_uart_value),
            "FastUART = 0x{:08X} ({})",
            fast_uart_value,
            if fast_uart_value == FAST_UART_CONFIG {
                "1 Mbaud default"
            } else {
                "operator override (3 Mbaud capability; FPGA/host baud unchanged — flagged followup)"
            },
        );

        // Step 8: Upgrade FPGA baud to match ASIC.
        // BM1368 supports up to 3 Mbaud, but ESP-Miner uses 1 Mbaud via FAST_UART.
        // Use 1.5M for FPGA chains (closest standard FPGA baud divisor).
        chain.set_baud(fpga_chain::BAUD_REG_1_5M);
        tracing::info!(
            chain_id = chain.chain_id,
            "FPGA baud upgraded to 1.5M (BAUD_REG=0x07)",
        );
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Step 9: Hash counting register (S21 stock default).
        Self::write_reg_broadcast(chain, regs::HASH_COUNTING, HASH_COUNTING_VAL);
        tracing::debug!(
            chain_id = chain.chain_id,
            "HashCounting = 0x{:08X}",
            HASH_COUNTING_VAL,
        );

        // Step 10: Final version mask set.
        Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, VERSION_MASK_REG);
        tracing::debug!(
            chain_id = chain.chain_id,
            "Final version mask = 0x{:08X}",
            VERSION_MASK_REG,
        );

        // Step 11: Set WORK_TIME in FPGA.
        let work_time = Bm1368Driver::calculate_work_time(freq_mhz, chip_count);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            chain_id = chain.chain_id,
            work_time = format_args!("0x{:08X}", work_time),
            "WORK_TIME set for {} chips at {} MHz",
            chip_count,
            freq_mhz,
        );

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1368: Chain configuration complete — {} chips at {} MHz",
            chip_count,
            freq_mhz,
        );

        Ok(())
    }

    fn set_frequency(&self, chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        let pll = self.pll_params(freq_mhz);

        tracing::info!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz = freq_mhz,
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            fb_div = pll.fb_div,
            ref_div = pll.ref_div,
            post_div1 = pll.post_div1,
            post_div2 = pll.post_div2,
            "BM1368: Setting PLL frequency",
        );

        if chip_addr == 0xFF {
            // Broadcast to all chips
            Bm1368Driver::write_reg_broadcast(chain, regs::PLL0_PARAM, pll.reg_value);
        } else {
            // Single chip
            Bm1368Driver::write_reg_single(chain, chip_addr, regs::PLL0_PARAM, pll.reg_value);
        }

        // PLL lock settling time (~10ms typical)
        std::thread::sleep(std::time::Duration::from_millis(10));

        Ok(())
    }

    fn verify_frequency(
        &self,
        chain: &mut FpgaChain,
        chip_addr: u8,
        expected_mhz: u16,
    ) -> Result<Option<u16>> {
        let target_addr = if chip_addr == 0xFF { 0x00 } else { chip_addr };
        let mut last_read = None;

        for _ in 0..3 {
            if let Some(raw) = Self::read_pll_register(chain, target_addr)? {
                last_read = Some(raw);
                if let Some(actual_mhz) = Self::pll_register_to_freq(raw) {
                    if actual_mhz == expected_mhz {
                        return Ok(Some(actual_mhz));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        match last_read {
            Some(raw) => Self::pll_register_to_freq(raw).map(Some).ok_or_else(|| {
                crate::AsicError::InvalidParameter(format!(
                    "BM1368 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1368 PLL0 readback timed out for chip 0x{:02X}",
                    target_addr
                ),
            }),
        }
    }

    fn set_voltage(&self, _pic: &mut PicController, _voltage_mv: u16) -> Result<()> {
        // S21 does NOT use PIC for voltage control (TAS5782M / NoPic path).
        // ADR-0010: refuse silent Ok(()) — callers must use the real voltage rail.
        tracing::warn!(
            "BM1368: set_voltage() called — S21 uses TAS5782M DAC, not PIC. \
             On S9 control board, use PicController directly.",
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1368/S21 voltage is TAS5782M/NoPic (not PicController ChipDriver path)".into(),
        ))
    }

    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u16> {
        // BM1368 uses full block header job format (82 bytes on wire).
        // Unlike BM1387 (midstate-based), BM1368 computes the midstate internally.
        // The ASIC also performs hardware version rolling.
        //
        // However, the FPGA WORK_TX FIFO expects the BraiinsOS work format:
        //   Word 0:      Work ID
        //   Word 1:      nbits (32-bit LE)
        //   Word 2:      ntime (32-bit LE)
        //   Word 3:      merkle_tail (last 4 bytes of merkle root, LE)
        //   Words 4-11:  midstate (reversed word order, native u32 values)
        //
        // For FPGA-mediated operation (S9 control board with BM1368 hash boards),
        // the FPGA handles the UART framing and work dispatch. We send the same
        // work format as BM1387 but with CTRL_REG in BM139X mode.
        //
        // For native S21 (serial UART), the full header format would be used
        // directly. This is handled by the transport layer, not the driver.

        if work.midstates.is_empty() {
            return Err(crate::AsicError::InvalidParameter(
                "no midstates provided".into(),
            ));
        }

        // FPGA work format: 12 words (4 header + 8 midstate) for BM139X mode.
        // BM139X mode uses MIDSTATE_CNT=0 (1 midstate per work item).
        const WORK_WORDS: usize = 12;

        let mut words = [0u32; WORK_WORDS];

        // Word 0: Work ID (no midstate shift — BM139X uses MIDSTATE_CNT=0).
        words[0] = work.work_id as u32;

        // Word 1: nbits
        words[1] = work.nbits;

        // Word 2: ntime
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root)
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Encode midstate in REVERSED word order for FPGA.
        // Same encoding as BM1387 — the FPGA handles the midstate regardless
        // of whether the ASIC computes it internally or not.
        //
        // DO NOT ADD .swap_bytes() — proven wrong on BM1387 (2026-03-17),
        // same byte ordering applies to BM139X FPGA mode.
        let midstate = &work.midstates[0];
        for i in 0..8 {
            let word_idx = 7 - i;
            words[4 + i] = u32::from_be_bytes([
                midstate[word_idx * 4],
                midstate[word_idx * 4 + 1],
                midstate[word_idx * 4 + 2],
                midstate[word_idx * 4 + 3],
            ]);
        }

        // Wait for TX FIFO space.
        for _ in 0..100 {
            if !chain.work_tx_full() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }

        chain.write_work(&words);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1368 nonce response (from WORK_RX_FIFO, 2 x 32-bit words):
        //
        // The FPGA packs the 11-byte ASIC response into 2 FIFO words.
        //   Word 0: nonce value (32-bit)
        //   Word 1: packed metadata
        //     Bits [7:0]   = solution_index / midstate_num (byte 6 of response)
        //     Bits [23:8]  = work_id (maps to hw_work_id from FPGA)
        //     Bits [31:24] = CRC / flags
        //
        // BM1368 job_id extraction from response byte 7:
        //   job_id = (byte7 & 0xF0) >> 1
        //   small_core_id = byte7 & 0x0F (16 small cores)
        //
        // BM1368 nonce bit field:
        //   bits[31:25] = core_id (7 bits)
        //   bits[24:17] = asic_address (8 bits)
        //   bits[16:0]  = nonce value (17 bits)
        //
        // Version bits from response bytes 8-9:
        //   version_bits = ntohs(bytes[8:9]) << 13
        //
        // Note: When running through the BraiinsOS FPGA, the raw FIFO format
        // may differ from direct serial UART. The FPGA strips the preamble
        // and packs the payload. The exact bit mapping depends on the FPGA
        // IP core version and BM139X mode configuration.

        let nonce = raw[0];
        let w1 = raw[1];

        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
        let work_id = hw_work_id;

        // For BM139X mode, the FPGA does not encode midstate index
        // (MIDSTATE_CNT=0 means 1 midstate, no index needed).
        let midstate_idx = 0;

        // Chip index from nonce bits [24:17] (address / interval).
        // The ASIC encodes its chip address in the nonce field.
        let chip_index = ((nonce >> 17) & 0xFF) as u8;

        Ok(NonceResult {
            nonce,
            chip_index,
            work_id,
            solution_id,
            midstate_idx,
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        (fpga_clock_hz / (16 * target_baud)) - 1
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM1368 requires BM139X mode (bit 4) + ENABLE (bit 3).
        // MIDSTATE_CNT=0 (bits 2:1 = 00) for single midstate.
        // BM1368 computes midstate internally and uses hardware version rolling,
        // so we only send 1 midstate (12 words per work item).
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE
    }

    fn job_interval_ms(&self, chip_count: u8, _freq_mhz: u16) -> u32 {
        // BM1368 job interval: 500ms / chip_count (from ESP-Miner).
        // For 108 chips: 500 / 108 = ~4.6ms per dispatch.
        // Minimum 1ms to avoid starving the FPGA.
        let interval = 500u32 / chip_count.max(1) as u32;
        interval.max(1)
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // Dynamic ticket mask: difficulty - 1 (matching BM1366/BM1370/BM1397/BM1398).
        // Fixture uses hardcoded 0x7F (diff 128) for testing, but production needs
        // dynamic difficulty matching the pool's suggested difficulty.
        difficulty.max(1).saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // BM1368 PLL: register 0x08
        // Formula: freq = 25 MHz * fb_div / (ref_div * postdiv1 * postdiv2)
        //
        // Uses brute-force search matching ESP-Miner pll_get_parameters().
        // FB_DIV range: 144-235 for BM1368.
        let (fb_div, ref_div, postdiv1, postdiv2, _actual_freq) =
            bm1368_pll_search(freq_mhz as f64);

        let reg_value = bm1368_pll_encode(fb_div, ref_div, postdiv1, postdiv2);

        PllConfig {
            fb_div: fb_div as u16,
            ref_div,
            post_div1: postdiv1,
            post_div2: postdiv2,
            reg_value,
        }
    }
}

/// Get the list of common target frequencies for BM1368 (MHz).
///
/// These cover the typical operating range for S21 miners (400-600 MHz).
/// The autotuner uses this for frequency stepping.
pub fn common_frequencies() -> &'static [u16] {
    &[400, 425, 450, 475, 500, 525, 550, 575, 600]
}

/// Alias for autotuner compatibility — same as common_frequencies().
pub fn pll_frequencies() -> &'static [u16] {
    common_frequencies()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1368_pll_vs_bitmain_s21_jig_vco_range_and_clamp() {
        let vco_of = |fb: u8, rd: u8| FREQ_MULT * fb as f64 / rd as f64;

        // (1) The curated lookup table (the primary/standard-frequency path) is
        //     ALREADY fully inside the Bitmain S21-jig VCO range (REFDIV=2,
        //     FBDIV 160-225 -> VCO 2000-2812). Confirms the standard freqs are
        //     all jig-compatible — no change needed on the hot path.
        for &(_fx100, fbdiv, _pd1, _pd2) in BM1368_PLL_TABLE {
            let vco = vco_of(fbdiv, 2); // table hardcodes REFDIV=2
            assert!(
                vco_in_jig_range(vco, 2),
                "lookup entry fbdiv={fbdiv} -> VCO {vco} out of jig range"
            );
        }

        // (2) The FINDING: the UNCLAMPED brute-force fallback (hit for off-table
        //     targets) selects a config the jig would reject for 364/801 targets
        //     across 100-900 MHz — mostly low frequencies picking REFDIV=2 /
        //     low-FBDIV configs with VCO BELOW the jig's 2000 MHz floor. 100 MHz
        //     is a concrete example (fallback -> VCO 1800 < 2000). (The curated
        //     table — part 1 — covers the operating range in-jig-range, so this
        //     is lower-impact than BM1370's, but the same finding-class.)
        let (fb, rd, _p1, _p2, _f) = bm1368_pll_fallback(100.0, false);
        assert!(
            !vco_in_jig_range(vco_of(fb, rd), rd),
            "unclamped 100 MHz fallback VCO {} should be out of jig range (the finding)",
            vco_of(fb, rd)
        );

        // (3) The gated FIX: CLAMPED, the off-table fallback stays in the jig VCO
        //     lock range across the whole band.
        for t in 100..=900u32 {
            let (fb, rd, _p1, _p2, _f) = bm1368_pll_fallback(t as f64, true);
            assert!(
                vco_in_jig_range(vco_of(fb, rd), rd),
                "clamped fallback: target {t} MHz VCO {} out of jig range (fb={fb} rd={rd})",
                vco_of(fb, rd)
            );
        }

        assert_eq!(JIG_VCO_CLAMP_ENV, "DCENT_BM1368_JIG_VCO_CLAMP");
    }

    #[test]
    fn bm1368_matches_esp_miner_clean_room_reference() {
        // RE 2026-06-02 clean-room cross-check (no live hardware / no Ghidra): pin DCENT's BM1368
        // (S21) init values to ESP-Miner's open-source BM1368 driver
        //. Confirms the S21 chip driver
        // (already live-accepted-share-proven on .135) also matches the independent open reference,
        // and that the S21 primer (0xFF0FC100) is distinct from the BM1370/S21Pro primer (0xF000C100).
        // ESP-Miner writes: 0xA8->00 07 00 00 | 0x18->FF 0F C1 00 | 0x54->00 00 00 03 |
        //   0x58->02 11 11 11 | 0x10->00 00 15 A4 | 0xA4->90 00 .. .. | per-chip 0x18->F0 00 C1 00.
        assert_eq!(CHIP_ID, 0x1368); // ESP-Miner BM1368_CHIP_ID 0x1368, response len 11
        assert_eq!(REG_A8_BCAST_INIT, 0x0007_0000);
        assert_eq!(REG_A8_PER_CHIP, 0x0007_01F0);
        assert_eq!(MISC_CTRL_BCAST_INIT, 0xFF0F_C100); // S21 primer (distinct from BM1370 0xF000C100)
        assert_eq!(MISC_CTRL_PER_CHIP, 0xF000_C100);
        assert_eq!(ANALOG_MUX_VAL, 0x0000_0003); // S21 (BM1370 uses 0x02)
        assert_eq!(IO_DRIVER_VAL, 0x0211_1111); // S21 (BM1370 uses 0x00011111)
        assert_eq!(HASH_COUNTING_VAL, 0x0000_15A4); // S21 stock
        assert_eq!(VERSION_MASK_REG & 0xFFFF_0000, 0x9000_0000);
    }

    #[test]
    fn test_pll_search_500mhz() {
        let (fb, rd, pd1, pd2, freq) = bm1368_pll_search(500.0);
        // 25 * fb / (rd * pd1 * pd2) should be close to 500
        let actual = 25.0 * fb as f64 / (rd as f64 * pd1 as f64 * pd2 as f64);
        assert!(
            (actual - 500.0).abs() < 1.0,
            "Expected ~500 MHz, got {}",
            actual
        );
        assert!(fb >= FB_DIV_MIN as u8 && fb <= FB_DIV_MAX as u8);
        assert!(freq > 499.0 && freq < 501.0);
    }

    #[test]
    fn test_pll_search_550mhz() {
        let (fb, rd, pd1, pd2, _freq) = bm1368_pll_search(550.0);
        let actual = 25.0 * fb as f64 / (rd as f64 * pd1 as f64 * pd2 as f64);
        assert!(
            (actual - 550.0).abs() < 7.0,
            "Expected ~550 MHz, got {}",
            actual
        );
    }

    #[test]
    fn test_pll_encode() {
        // fb=160, ref=1, pd1=2, pd2=1 -> freq = 25*160/1/2/1 = 2000 MHz
        // VCO = 25*160/1 = 4000 >= 2400 -> vdo_scale = 0x50
        // postdiv_byte = ((2-1) << 4) | (1-1) = 0x10
        let val = bm1368_pll_encode(160, 1, 2, 1);
        assert_eq!(val, 0x50A0_0110, "Encoding mismatch: 0x{:08X}", val);
    }

    #[test]
    fn test_pll_encode_low_vco() {
        // fb=144, ref=2, pd1=1, pd2=1 -> freq = 25*144/2/1/1 = 1800 MHz
        // VCO = 25*144/2 = 1800 < 2400 -> vdo_scale = 0x40
        // postdiv_byte = ((1-1) << 4) | (1-1) = 0x00
        let val = bm1368_pll_encode(144, 2, 1, 1);
        assert_eq!(val, 0x4090_0200, "Encoding mismatch: 0x{:08X}", val);
    }

    #[test]
    fn test_chip_id() {
        let driver = Bm1368Driver::new();
        assert_eq!(driver.chip_id(), 0x1368);
        assert_eq!(driver.chip_name(), "BM1368");
        assert_eq!(driver.cores_per_chip(), 1280);
        assert_eq!(driver.response_length(), 11);
    }

    #[test]
    fn test_ticket_mask() {
        let driver = Bm1368Driver::new();
        assert_eq!(driver.ticket_mask(128), 127);
        assert_eq!(driver.ticket_mask(256), 255);
        assert_eq!(driver.ticket_mask(1), 0);
    }

    #[test]
    fn test_job_interval() {
        let driver = Bm1368Driver::new();
        // 108 chips: 500/108 = 4
        assert_eq!(driver.job_interval_ms(108, 550), 4);
        // 1 chip: 500/1 = 500
        assert_eq!(driver.job_interval_ms(1, 500), 500);
    }

    #[test]
    fn test_ctrl_reg() {
        let driver = Bm1368Driver::new();
        let ctrl = driver.ctrl_reg_value();
        // BM139X mode (bit 4) + ENABLE (bit 3) = 0x18
        assert_eq!(ctrl, 0x18, "CTRL_REG should be 0x18, got 0x{:02X}", ctrl);
    }

    #[test]
    fn pll_register_to_freq_round_trips_known_frequencies() {
        // verify_frequency() reads back PLL0 and decodes via pll_register_to_freq.
        // Pin that the decode is the exact inverse of pll_params().reg_value for
        // every common frequency, and that the PLL lock bit (MSB) is masked off
        // before the lookup. Read-only PLL-lock-verification correctness check.
        let drv = Bm1368Driver::new();
        for &f in MinerProfile::pll_frequencies_for_chip(CHIP_ID) {
            let reg = drv.pll_params(f).reg_value;
            assert_eq!(
                Bm1368Driver::pll_register_to_freq(reg),
                Some(f),
                "bare PLL0 readback 0x{:08X} must decode to {} MHz",
                reg,
                f
            );
            assert_eq!(
                Bm1368Driver::pll_register_to_freq(reg | 0x8000_0000),
                Some(f),
                "locked PLL0 readback for {} MHz must mask bit31 before lookup",
                f
            );
        }
        // Unknown register → None (verify_frequency surfaces an error, not OK).
        assert_eq!(Bm1368Driver::pll_register_to_freq(0x0000_0000), None);
    }

    #[test]
    fn test_pll_fb_div_range() {
        // Verify all common frequencies produce valid fb_div values
        for &freq in common_frequencies() {
            let pll = Bm1368Driver::new().pll_params(freq);
            assert!(
                pll.fb_div >= FB_DIV_MIN && pll.fb_div <= FB_DIV_MAX,
                "freq {} MHz: fb_div {} outside range {}-{}",
                freq,
                pll.fb_div,
                FB_DIV_MIN,
                FB_DIV_MAX,
            );
            assert!(
                pll.post_div1 >= pll.post_div2,
                "postdiv1 must be >= postdiv2"
            );
        }
    }

    // -----------------------------------------------------------------
    // PERF-005 — PLL ramp ceiling raised to ~600 MHz (capability only)
    // -----------------------------------------------------------------

    #[test]
    fn perf005_ramp_unchanged_at_or_below_table_max() {
        // Conservative-default guard: a 475 MHz (table-max) ramp produces only
        // verified-table steps, byte-identical to the pre-PERF-005 behavior.
        let steps = pll_ramp_sequence(475);
        assert!(!steps.is_empty());
        // Every step must be a verified-table entry (≤ 475.00 MHz).
        for &(_reg, freq_x100) in &steps {
            assert!(
                freq_x100 <= BM1368_PLL_TABLE_MAX_X100,
                "step {} x100 must stay within the verified table at/below 475",
                freq_x100
            );
        }
        assert_eq!(
            steps.last().map(|&(_, f)| f),
            Some(47500),
            "475 MHz ramp must end exactly at the table max"
        );
    }

    #[test]
    fn perf005_ramp_extends_above_table_to_600() {
        // A 600 MHz request now ramps all the way up instead of capping at 475.
        let steps = pll_ramp_sequence(600);
        assert!(!steps.is_empty());

        // Monotonic non-decreasing frequencies (it's a ramp).
        for w in steps.windows(2) {
            assert!(
                w[1].1 >= w[0].1,
                "ramp must be monotonic: {} then {}",
                w[0].1,
                w[1].1
            );
        }

        // It must reach exactly the 600.00 MHz target (60000 x100).
        assert_eq!(
            steps.last().map(|&(_, f)| f),
            Some(60000),
            "600 MHz ramp must terminate exactly at 60000 x100"
        );

        // And it must include at least one above-table step (proves the
        // brute-force extension fired, not a single slam from 475 to 600).
        let above_table = steps
            .iter()
            .filter(|&&(_, f)| f > BM1368_PLL_TABLE_MAX_X100)
            .count();
        assert!(
            above_table >= 2,
            "expected staged steps above the 475 MHz table, got {}",
            above_table
        );
    }

    #[test]
    fn perf005_ramp_clamps_request_above_capability_ceiling() {
        // Requests above the 600 MHz capability ceiling clamp to 600 — we never
        // try to program a frequency above the reviewed capability window.
        let steps = pll_ramp_sequence(900);
        assert_eq!(
            steps.last().map(|&(_, f)| f),
            Some(BM1368_PLL_RAMP_MAX_X100),
            "over-ceiling request must clamp to the 600 MHz capability ceiling"
        );
    }

    // -----------------------------------------------------------------
    // W8 BM1368 FastUART 3M target — default-OFF env-gated capability
    // -----------------------------------------------------------------

    #[test]
    fn fast_uart_default_is_compiled_1m() {
        // Load-bearing: the COMPILED default MUST remain 0x1130_0200 (1 Mbaud,
        // ESP-Miner BM1368_set_max_baud). No env / blank / whitespace → default.
        // Flipping this would change the fast-UART units of every live S21
        // before an operator live-A/B confirms 3 Mbaud produces accepted shares.
        assert_eq!(FAST_UART_CONFIG, 0x1130_0200);
        assert_eq!(resolve_fast_uart_value(None), 0x1130_0200);
        assert_eq!(resolve_fast_uart_value(Some("")), 0x1130_0200);
        assert_eq!(resolve_fast_uart_value(Some("   ")), 0x1130_0200);
    }

    #[test]
    fn fast_uart_override_applies_3m_byte_order() {
        // The 3 Mbaud byte order applies cleanly via the env override, in both
        // lower- and upper-case hex prefix forms. Plain decimal also works.
        assert_eq!(FAST_UART_VALUE_3M, 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some("0x11300000")), 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some("0X11300000")), 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some(" 0x11300000 ")), 0x1130_0000);
        assert_eq!(
            resolve_fast_uart_value(Some(&FAST_UART_VALUE_3M.to_string())),
            0x1130_0000
        );
        // A malformed override falls back to the proven 1 Mbaud default — a typo
        // can never silently reprogram the chip UART.
        assert_eq!(resolve_fast_uart_value(Some("0xZZZZ")), FAST_UART_CONFIG);
        assert_eq!(
            resolve_fast_uart_value(Some("not-a-number")),
            FAST_UART_CONFIG
        );
        // Gate name is the agreed single source of truth (mirrors the BM1362
        // DCENT_AM2_FAST_UART_VALUE convention).
        assert_eq!(FAST_UART_VALUE_ENV, "DCENT_S21_FAST_UART_VALUE");
    }

    #[test]
    fn fast_uart_3m_differs_from_default_only_in_bt8d_divisor() {
        // The 1 Mbaud default and the 3 Mbaud override share the high two bytes
        // (BCLK_SEL/config = 0x1130) and differ ONLY in the BT8D divisor field
        // (byte[2:3]): 0x0200 → 1 Mbaud, 0x0000 → max fast baud. This pins the
        // RE understanding so a future edit can't silently corrupt the config
        // bytes while changing the divisor.
        assert_eq!(FAST_UART_CONFIG & 0xFFFF_0000, 0x1130_0000);
        assert_eq!(FAST_UART_VALUE_3M & 0xFFFF_0000, 0x1130_0000);
        assert_eq!(FAST_UART_CONFIG & 0x0000_FFFF, 0x0000_0200);
        assert_eq!(FAST_UART_VALUE_3M & 0x0000_FFFF, 0x0000_0000);
    }
}
