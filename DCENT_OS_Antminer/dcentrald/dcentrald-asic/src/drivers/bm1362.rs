//! BM1362 ASIC driver (S19j Pro, 5 nm chip).
//!
//! ## Chip activation (no open-core required)
//! Unlike BM1387 (S9, 16 nm) which needs 114 dummy-work packets with
//! `gate_block=1`, BM1362 activates cores via per-chip register writes
//! during init. ESP-Miner BM1366/1368/1370 drivers confirm — zero
//! `open_core`/`gate_block` symbols. Verified against bosminer-am2-s17
//! binary strings (Phase 3 investigation) and the .139 live probe:
//!  has
//! no matches for `open_core|gate_block|dummy_work|NUM_CORES_ON_CHIP|send_init_work`
//! anywhere in the am2-s17 tree — those symbols only appear in
//! `braiins_bm1387.rs` (S9). The normal AM2 path uses the traced BM1362
//! register sequence and does not append the BM1370-style `0xB9` tail block.
//!
//! ## CTRL register (FPGA side, am2)
//! `0x43CX0000 + 0x00 = 0x00901002` (bits 1/12/20/23 set; MIDSTATE_CNT=1
//! i.e. 2 midstates). Live-probed on .139 chain1 and chain4 (identical
//! value, ~69 TH/s sustained, 0 HW errors) — see
//! .
//! The authoritative am2 constant lives in
//! `dcentrald_hal::fpga_chain::ctrl_am2::BM1362_DEFAULT` (Phase 4A Agent β).
//! S9 (am1) uses a completely different layout — see the `ctrl_reg_value`
//! impl below for the S9-only convention and do NOT conflate the two.
//!
//! ## MiscCtrl register (ASIC side, UART-written)
//! Register `0x18` of BM1362, written via the FPGA CMD FIFO on universal
//! hash-board builds or over `/dev/ttyS2`/`/dev/ttyS3` at 3.125 Mbaud on
//! native am2. MUST be triple-written with 5 ms spacing — see
//! . The AM2 traced plan uses
//! `0xFF0FC100` before FastUART and the verified production byte sequence
//! `0x00C100B0` after the fast-baud switch.
//! Phase 4A probe
//! confirmed MiscCtrl is an ASIC-internal register, NOT the FPGA MiscCtrl
//! from S9 — its fields are `ext_baud_enable` (bit 16), `rfs` (bit 14),
//! `tfs` (bit 5), `OpenDrain`, `HashDoing`, `UartReceiving`.
//!
//! ---
//!
//! Live probe (2026-04-20, S19j Pro @ 203.0.113.139, BraiinsOS+ 26.04) confirmed:
//!
//!   - Platform: `zynq-bm3-am2` (bosminer crate `bosminer_am2_s17`), FPGA IP split into
//!     4 sibling blocks per chain (common/cmd-rx/work-rx/work-tx, 4 KiB each).
//!   - 126 chips/chain (log: `Discovered 126 chips (expected 126 chips)`), address
//!     stride 0x02 (126 × 2 = 252 < 256).
//!   - 4 big cores/chip (BM1362 small-die, 504 cores/chain total).
//!   - Rated 545 MHz @ 13.7 V shared-rail, up to 15.2 V max.
//!   - UART 115200 → 3,125,000 baud after MiscCtrl triple-write
//!     (log: `Set baud rate @ requested: 3125000, actual: 3125000`).
//!   - dsPIC33 voltage controller at I2C `0x20/0x21/0x22`, FW byte 0x89
//!     (bosminer driver `pic0x89.rs`). SUM checksum framing — see
//!     .
//!   - 11-byte nonce response (preamble stripped by FPGA; nonce + midstate_num
//!     + result + version bits + CRC/flags).
//!   - Full-header job format on wire; the FPGA in am2 still frames the CMD
//!     path. Actual work dispatch on am2 is through `/dev/ttyS2` / `ttyS3` —
//! and
//!     `DCENT_OS_Antminer/dcentrald/dcentrald/src/serial_mining.rs`.
//!
//! Unified BM1397+ command framing (via `bm139x` helpers):
//!   - `HDR_WRITE_ALL`   = 0x51  (broadcast WRITE) — NOT 0x58 (that is BM1387 SETCONFIG)
//!   - `HDR_WRITE_SINGLE`= 0x41  (per-chip WRITE)
//!   - `HDR_READ_ALL`    = 0x52  (broadcast READ = GetAddress — see
//!     )
//!   - Length byte = 0x09 for WRITE (2 FIFO words), 0x05 for READ (1 word).
//!
//! Safety rules (ENFORCED or cross-referenced):
//!   - NEVER single-write MiscCtrl — triple-write 3× with 5 ms spacing
//!. The baud upgrade path
//!     and every other MiscCtrl write go through `misc_ctrl_triple_write`.
//!   - NEVER PIC RESET on S19j Pro. The
//!     driver does not touch PIC reset; voltage path lives in `pic::PicController`
//!     / `DspicController` — caller is responsible for gating on 5 stable
//!     heartbeat ticks.
//!   - FPGA work_id is 8 bits; `send_work`
//!     shifts only by FPGA MIDSTATE_CNT and the upper bits wrap naturally.
//!   - Version rolling mask is read from caller/config.
//!     The init sequence programs the default 0x1FFFE000-compatible mask but
//!     `send_work` does NOT hardcode version bits into the work.

use crate::drivers::{bm139x, ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::protocol;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1362 chip ID.
///
/// **W11.5 (2026-05-09) cross-reference**: per-SKU hashboard geometry
/// (BHB42601 / BHB42801 / BHB42611), freq/voltage tables, the BM1362
/// PLL formula, and the canonical work-layout byte counts now live in
/// `dcentrald-silicon-profiles::bm1362::{chip, work_layout,
/// Bm1362HashboardSku, Bm1362ChainGeometry, pll_freq_mhz}`. Constants
/// in this driver module (CHIP_ID, ADDRESS_INTERVAL, BM1362_BIG_CORES,
/// WORK_WORDS, JOB_ID_INCREMENT, JOB_ID_MASK) remain the live-pinned
/// driver-side values used by `ChipDriver` impls and are tested against
/// hardware on .139 / .133. Both modules agree on the byte-exact
/// numbers (`silicon-profiles::bm1362::chip::CHIP_ID == 0x1362`,
/// `chip::ADDRESS_STRIDE == 2`, `chip::CHIPS_PER_CHAIN == 126`).
pub const CHIP_ID: u16 = 0x1362;

/// BM1362 default chips per chain — verified on S19j Pro .139
/// (bosminer log: `Discovered 126 chips (expected 126 chips)`).
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 126;

/// Number of big cores per BM1362 chip (small-die variant — 4 per chip).
///
/// Source:
/// (Agent 5). Cross-check: 126 × 4 × ~826 MH/s ≈ 104 TH/s @ 545 MHz, matches
/// the rated hashrate from `bosminer_model.json`.
pub const BM1362_BIG_CORES: u32 = 4;

/// Address stride for BM1362 chain enumeration — 256 / 126 = 2.03 → 2.
/// Matches bosminer `SET_ADDRESS (0x00, 0x02, 0x04, ... 0xFC)` in the am2
/// initialization flow (`SUMMARY.md` step 9).
pub const ADDRESS_INTERVAL: u8 = 2;

/// BM1362 response size (11 bytes: nonce + result + version bits).
pub const RESPONSE_BYTES: usize = 11;

/// BM1362 mining baud after the post-enumeration upgrade (FPGA path).
pub const OPERATIONAL_BAUD: u32 = 3_125_000;

/// BM1362 on the Braiins FPGA still uses the standard BM139X FIFO framing.
/// Even though the ASIC performs version rolling internally, the FPGA expects
/// 4 header words followed by duplicate midstate slots, just like BM1366.
const _NUM_MIDSTATES_HINT: usize = 4;

/// BM1362 uses 36 FIFO words with MIDSTATE_CNT=2 (4 active slots).
pub const WORK_WORDS: usize = 36;

/// Braiins FPGA BM139X mode uses MIDSTATE_CNT=2 (4 slots) for BM1362.
const MIDSTATE_CNT_LOG2: u16 = 2;

/// Job ID increment for BM1362 (same as BM1368/BM1370).
/// Job IDs cycle: 0, 24, 48, 72, 96, 120, 16, 40, ...
pub const JOB_ID_INCREMENT: u8 = 24;

/// Job ID mask: bits [7:4] shifted right by 1, per ASIC Register Bible.
pub const JOB_ID_MASK: u8 = 0x7F;

/// BM1362 register addresses.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 parameter — hash clock PLL.
    pub const PLL0: u8 = 0x08;
    /// Hash counting number / nonce range distribution.
    pub const HASH_COUNTING_NUMBER: u8 = 0x10;
    /// Ticket mask register (hardware difficulty filter).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc control register (UART config, clock settings). Triple-write only.
    pub const MISC_CONTROL: u8 = 0x18;
    /// Fast UART configuration (baud rate for BM1366+ family).
    pub const FAST_UART_CONFIG: u8 = 0x28;
    /// UART relay (multi-chip relay config).
    pub const UART_RELAY: u8 = 0x2C;
    /// Core register control (indirect core access).
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// Analog mux control (temperature diode).
    pub const ANALOG_MUX: u8 = 0x54;
    /// IO driver strength.
    pub const IO_DRIVER_STRENGTH: u8 = 0x58;
    /// PLL1 parameter — baud-rate PLL. Reclocked by the chip's own
    /// `set_chain_baud` for any baud >= 3,000,001 (see [`jig_pll1_reclock_regs`]).
    pub const PLL1: u8 = 0x60;
    /// PLL3 parameter — domain config.
    pub const PLL3: u8 = 0x68;
    /// Version rolling mask register.
    pub const VERSION_MASK: u8 = 0xA4;
    /// Init control register (magic init values).
    pub const INIT_CONTROL: u8 = 0xA8;
}

/// Baud above which the BM1362/BM1370 chip reclocks its command UART off PLL1
/// (`0x2dc6c1` = 3,000,001). DCENT's target of 3,125,000 baud is ABOVE this.
pub const PLL1_RECLOCK_BAUD_THRESHOLD: u32 = 0x002d_c6c1;

/// BM1362 factory-jig `set_chain_baud` register transform — **verified
/// first-hand** from the AMTC S19j Pro repair-jig `single_board_test` binary
/// (BHB42601 / BM1362, `FUN_0002cb14`, decoded from ARM 2026-06-10). Given the
/// chip's current register readbacks and the target baud, returns the
/// read-modify-write values to program for a chain-baud change.
///
/// This is the chip's OWN baud procedure, and it is **byte-identical** to the
/// BM1370/S21pro jig `set_chain_baud@CB3B0`. For `target_baud >= 3_000_001`
/// the chip MUST reclock its command UART off **PLL1 (reg `0x60`)** and take the
/// `reg 0x28` divider from a **400 MHz** clock (written ×2 @ 10 ms); below the
/// threshold it clocks the UART from the **25 MHz** reference and PLL1 is left
/// untouched.
///
/// IMPORTANT — blocker T1 (fast-baud zero-nonce): DCENT's am2 fast-baud path
/// writes a FIXED `reg 0x28 = 0x0000_3011` + `reg 0x18` MiscCtrl and NEVER
/// writes reg `0x60`. The factory jig proves the reg-`0x60` PLL1 reclock is
/// mandatory at 3.125 Mbaud (above the threshold), while DCENT's method is
/// measured `0/126` at 3.125 M on `a lab unit`/`a lab unit`/`a lab unit`. (This reverses the
/// 2026-06-10 "PLL1 refuted for BM1362" desk conclusion, which was inferred
/// from the *absence* of a BM1362 jig — the jig now in hand falsifies it.)
/// The live, gated A/B wiring is in `s19j_hybrid_mining.rs` behind
/// `DCENT_AM2_BAUD_JIG_PLL1_RECLOCK` (default-OFF).
///
/// Returns `(reg60_write, reg28_write)`; `reg60_write` is `None` on the low
/// (25 MHz) path where the chip leaves PLL1 alone. The masks below preserve the
/// exact readback bits the jig keeps (decoded byte-for-byte from the ARM).
pub fn jig_pll1_reclock_regs(
    reg60_readback: u32,
    reg28_readback: u32,
    target_baud: u32,
) -> (Option<u32>, u32) {
    let baud = target_baud.max(1);
    if baud < PLL1_RECLOCK_BAUD_THRESHOLD {
        // 25 MHz reference path — PLL1 untouched, only reg 0x28 divider.
        let div = (25_000_000u32 / (baud << 3)).max(1);
        let d = (div - 1) & 0xFF;
        let reg28 = (reg28_readback & 0xBBFE_00FF) | (d << 8);
        (None, reg28)
    } else {
        // PLL1 reclock path: reg 0x60 RMW, then reg 0x28 divider from 400 MHz.
        let reg60 = (reg60_readback & 0xD000_C088) | 0x5060_0111;
        let div = (400_000_000u32 / (baud << 3)).max(1);
        let d = (div - 1) & 0xFF;
        let reg28 = (reg28_readback & 0xFC0E_00FF) | 0x8450_0000 | (d << 8);
        (Some(reg60), reg28)
    }
}

/// BM1362 PLL frequency lookup table.
///
/// PLL register encoding for BM1366/BM1368/BM1370/BM1362:
///   Byte 0: VCO_SCALE (0x40 if VCO < 2400 MHz, 0x50 if >= 2400 MHz)
///   Byte 1: FBDIV (feedback divider)
///   Byte 2: REFDIV (reference divider)
///   Byte 3: POSTDIV encoded as ((POSTDIV1-1) << 4) | (POSTDIV2-1)
///
/// Formula: freq = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)
/// FB_DIV range for BM1362: 160-239
///
/// Coverage: 400–597 MHz. This includes the live rated 545 MHz and leaves
/// headroom up to ~597 MHz for autotuner stretch goals (BraiinsOS+ allows
/// S19j Pro tuning up to 1300 MHz in config but hardware rarely locks
/// above ~620 MHz without overclock packs).
///
/// SUB-400 MHz WARNING (RE 2026-06-18,
/// 2026-06-18-bm1362-jig-pll-algorithm.md`): do NOT extrapolate this table below
/// 400 MHz by holding the VCO high (4000–5975 MHz) and cranking the postdivider.
/// That is exactly what the live-FALSIFIED `DCENT_AM2_RE018_LOW_FREQ_PLL` probe
/// `0x50D2_0164` did (VCO 5250, postdiv 7×5) → zero nonces. The Bitmain factory
/// jig reaches low frequencies via a SEPARATE low-VCO regime (VCO 2000–3200 MHz,
/// VCO_SCALE split at 2400, small postdividers), NOT high-VCO extrapolation. A
/// proven sub-400 table must come from that low-VCO regime + live validation.
///
/// Format: (freq_mhz, pll_reg_value)
pub const BM1362_PLL_TABLE: &[(u16, u32)] = &[
    // POSTDIV1=5, POSTDIV2=2, REFDIV=1:
    //   encoded postdiv = ((5-1)<<4)|(2-1) = 0x41
    //   freq = 25 * FBDIV / (1 * 5 * 2) = 2.5 * FBDIV
    //   FB_DIV range 160-239 covers 400-597 MHz
    //   VCO = 25*FBDIV = 4000-5975 MHz (all >= 2400, use VCO_SCALE=0x50)
    (400, 0x50A0_0141), // fbdiv=160
    (412, 0x50A5_0141), // fbdiv=165
    (425, 0x50AA_0141), // fbdiv=170
    (437, 0x50AF_0141), // fbdiv=175
    (450, 0x50B4_0141), // fbdiv=180
    (462, 0x50B9_0141), // fbdiv=185
    (475, 0x50BE_0141), // fbdiv=190
    (487, 0x50C3_0141), // fbdiv=195
    (500, 0x50C8_0141), // fbdiv=200
    (512, 0x50CD_0141), // fbdiv=205
    (525, 0x50D2_0141), // fbdiv=210
    (531, 0x50D4_0141), // fbdiv=212 — live autotuned on .139 chain 2
    (537, 0x50D7_0141), // fbdiv=215
    (545, 0x50DA_0141), // fbdiv=218 — RATED freq (S19j Pro default)
    (550, 0x50DC_0141), // fbdiv=220
    (556, 0x50DE_0141), // fbdiv=222.4 — live autotuned on .139 chain 3 (rounded)
    (562, 0x50E1_0141), // fbdiv=225
    (575, 0x50E6_0141), // fbdiv=230
    (587, 0x50EB_0141), // fbdiv=235
    (597, 0x50EF_0141), // fbdiv=239 (top of window)
];

/// Look up the PLL register value for a target frequency.
///
/// Returns (pll_reg_value, actual_frequency_mhz).
/// If the exact frequency isn't in the table, the nearest entry is used.
fn bm1362_pll_lookup(target_mhz: u16) -> (u32, u16) {
    let target = target_mhz.clamp(400, 597);

    let mut best = BM1362_PLL_TABLE[0];
    let mut best_diff = (target as i32 - best.0 as i32).unsigned_abs();

    for &entry in &BM1362_PLL_TABLE[1..] {
        let diff = (target as i32 - entry.0 as i32).unsigned_abs();
        if diff < best_diff {
            best = entry;
            best_diff = diff;
        }
    }

    (best.1, best.0)
}

/// Get the sorted list of discrete PLL frequencies the BM1362 can generate (MHz).
pub fn pll_frequencies() -> &'static [u16] {
    &[
        400, 412, 425, 437, 450, 462, 475, 487, 500, 512, 525, 531, 537, 545, 550, 556, 562, 575,
        587, 597,
    ]
}

/// Look up PLL register value + actual freq for a target frequency (public).
///
/// Mirror of the private `bm1362_pll_lookup` so callers in
/// `s19j_hybrid_mining.rs` can reuse the canonical BM1362 PLL table without
/// keeping a duplicate copy. Same nearest-entry semantics.
pub fn pll_lookup(target_mhz: u16) -> (u32, u16) {
    bm1362_pll_lookup(target_mhz)
}

/// Extend [`pll_lookup`] to the sub-400 MHz range (240-400) for EXPLICIT, GATED
/// callers ONLY (the `a lab unit` RE-018 low-freq bump). The shared
/// [`pll_frequencies`] / `BM1362_PLL_TABLE` are deliberately NOT extended, so the
/// ~12 fleet consumers of `pll_frequencies_for_chip` (autotuner / work_dispatcher
/// / thermal / power_budget / REST / fleet / efficiency) keep their proven 400 MHz
/// runtime floor and stay byte-identical — extending the shared table would
/// silently drop every BM1362 unit's floor to 240 (a fleet regression).
///
/// For `target_mhz >= 400` this delegates to the proven table. For 240-399 it
/// COMPUTES the register from the PROVEN divider (REFDIV=1, POSTDIV1=5,
/// POSTDIV2=2 → ÷10, postdiv byte 0x41, VCO_SCALE=0x50): `FBDIV = round(mhz*2/5)`
/// clamped to [96,160] so `VCO = 25*FBDIV` stays in [2400,4000] (≥2400 = valid
/// for VCO_SCALE 0x50). 240 MHz (FBDIV=96, VCO=2400) is the HARD MINIMUM — below
/// it VCO<2400 forces an off-table postdiv (the live-rejected 0x50D2_0164 ÷35).
/// Returns (reg_value, actual_freq_mhz).
pub fn pll_lookup_extended(target_mhz: u16) -> (u32, u16) {
    if target_mhz >= 400 {
        return bm1362_pll_lookup(target_mhz);
    }
    let clamped = target_mhz.clamp(240, 399) as u32;
    // freq = 2.5 * FBDIV  →  FBDIV = round(freq * 2 / 5); (n*2 + 2)/5 rounds to nearest.
    let fbdiv = ((clamped * 2 + 2) / 5).clamp(96, 160);
    let reg = (0x50u32 << 24) | (fbdiv << 16) | (0x01u32 << 8) | 0x41u32;
    let actual = ((25 * fbdiv) / 10) as u16; // = 2.5 * FBDIV
    (reg, actual)
}

/// Decode a BM1362 PLL reg-0x08 value to its REAL frequency in MHz by the divider
/// formula `freq = 25*FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)`, ignoring the lock
/// bit and the VCO_SCALE band byte. Generic field decode that works for the table
/// rows, the computed sub-400 rows, AND the RE-018 default `0x40A80265` (→ 50 MHz,
/// NOT the 525 config label). Used to report the actual applied frequency.
pub fn decode_pll_reg_to_freq(raw_reg: u32) -> Option<u16> {
    let reg = raw_reg & !PLL_LOCK_BIT;
    let fbdiv = (reg >> 16) & 0xFF;
    let refdiv = ((reg >> 8) & 0xFF).max(1);
    let postdiv = reg & 0xFF;
    let pd1 = ((postdiv >> 4) & 0x0F) + 1;
    let pd2 = (postdiv & 0x0F) + 1;
    let denom = refdiv * pd1 * pd2;
    if denom == 0 || fbdiv == 0 {
        return None;
    }
    Some(((25 * fbdiv) / denom) as u16)
}

/// PLL lock bit — MSB of register `0x08` once the PLL has stabilized at the
/// programmed VCO/postdiv. Same convention as BM1366/1368/1370/1397/1398.
pub const PLL_LOCK_BIT: u32 = 0x8000_0000;

/// Build a staged PLL ramp from `start_mhz` up to `target_mhz` in `step_mhz`
/// increments. Returns a `Vec` of `(pll_reg_value, freq_mhz)` tuples ordered
/// from low → high. The caller is responsible for the inter-step settle delay
/// and the lock-check.
///
/// Why this exists (cross-reference  BM1368-vs-BM1362 comparison agent):
///
/// BM1368 — proven on `a lab unit` — ramps PLL `200 → 525 MHz` in `25 MHz` steps with
/// `100 ms` settle (`serial_mining.rs::init_bm1368_chain` Step 7). BM1362 on
/// `a lab unit` previously SLAMMED PLL straight from default (~50 MHz) to the
/// `0x40A8_0265` traced 525 MHz value in two writes with `10 ms` spacing
/// (`s19j_hybrid_mining.rs::init_asic_chain` Step 5). 's hypothesis is
/// that the slam never lets the on-die PLL acquire lock, leaving the chain
/// UART path silent even at 13.7 V engaged rail. This helper replicates the
/// BM1368 staging pattern for BM1362.
///
/// The first emitted step is at `start_mhz` clamped into `[400, 597]` (the
/// table window) and is encoded from the lookup table. Each subsequent step
/// adds `step_mhz` (or whatever the table snaps to) until we reach or pass
/// `target_mhz`. The final entry is always exactly `target_mhz` (clamped) so
/// the caller doesn't have to special-case the tail.
///
/// `step_mhz` is best-effort — the actual step size is whatever the nearest
/// PLL-table entry is. Caller should pass `25` to match the BM1368 cadence.
pub fn pll_ramp_sequence(start_mhz: u16, target_mhz: u16, step_mhz: u16) -> Vec<(u32, u16)> {
    let target = target_mhz.clamp(400, 597);
    let start = start_mhz.clamp(400, target);
    let step = step_mhz.max(1);

    let mut steps: Vec<(u32, u16)> = Vec::new();
    let mut current = start;
    loop {
        let (reg, actual) = bm1362_pll_lookup(current);
        // De-dupe: never emit the same (reg, actual) twice (table snapping
        // can cause a step of size < 12 MHz to land on the same entry).
        if steps
            .last()
            .is_none_or(|&(_, last_freq)| last_freq != actual)
        {
            steps.push((reg, actual));
        }
        if current >= target {
            break;
        }
        let next = current.saturating_add(step);
        current = next.min(target);
    }

    // Belt-and-suspenders: ensure the very last step is exactly `target`
    // (could differ from the saturating_add path if `target` is mid-step).
    if let Some(&(_, last_freq)) = steps.last() {
        if last_freq != bm1362_pll_lookup(target).1 {
            steps.push(bm1362_pll_lookup(target));
        }
    }

    steps
}

// ---------------------------------------------------------------------------
// BM1362 / am2 init register constants
// ---------------------------------------------------------------------------

/// Immutable BM1362 AM2 init plan distilled from the traced healthy S19j Pro
/// sequence. Exposed so transport-specific paths can share the same register
/// constants without local copies drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bm1362InitPlan {
    pub version_mask: u32,
    pub init_control_register: u8,
    pub init_control_broadcast: u32,
    pub init_control_per_chip: u32,
    pub misc_control_register: u8,
    pub misc_control_pre_baud: u32,
    pub misc_control_post_fast_baud: u32,
    pub misc_control_triple_writes: u8,
    pub fast_uart_register: u8,
    pub fast_uart_value: u32,
    pub core_reg_ctrl_register: u8,
    pub core_reg_hash_clk: u32,
    pub core_reg_clk_delay: u32,
    pub core_reg_family: u32,
    pub analog_mux_register: u8,
    pub analog_mux_value: u32,
    pub io_driver_strength_register: u8,
    pub io_driver_normal: u32,
    pub append_bm1370_tail: bool,
}

/// Canonical BM1362 AM2 init plan. This is data-only so host tests can pin
/// the plan without touching hardware.
pub const BM1362_INIT_PLAN: Bm1362InitPlan = Bm1362InitPlan {
    version_mask: 0x9000_FFFF,
    init_control_register: regs::INIT_CONTROL,
    init_control_broadcast: 0x0007_0000,
    init_control_per_chip: 0x0007_01F0,
    misc_control_register: regs::MISC_CONTROL,
    misc_control_pre_baud: 0xFF0F_C100,
    misc_control_post_fast_baud: 0x00C1_00B0,
    misc_control_triple_writes: 3,
    fast_uart_register: regs::FAST_UART_CONFIG,
    fast_uart_value: 0x0000_3011,
    core_reg_ctrl_register: regs::CORE_REG_CTRL,
    core_reg_hash_clk: 0x8000_8540,
    core_reg_clk_delay: 0x8000_8008,
    core_reg_family: 0x8000_82AA,
    analog_mux_register: regs::ANALOG_MUX,
    analog_mux_value: 0x0000_0003,
    io_driver_strength_register: regs::IO_DRIVER_STRENGTH,
    io_driver_normal: 0x0001_1111,
    append_bm1370_tail: false,
};

/// Version mask register value.
///
/// Encodes the standard 0x1FFFE000 version-rolling mask:
///   prefix 0x9000 | (0x1FFFE000 >> 13 = 0xFFFF) = 0x9000FFFF.
///
/// NOTE:, drivers must NEVER hardcode
/// version-rolling *in the work path*. This register value configures the ASIC
/// for the default Bitmain-spec mask; `send_work` still uses the `MiningWork`
/// version field supplied by the Stratum layer (which reflects the pool /
/// config-negotiated mask). Changing the pool-negotiated mask would require
/// re-writing this register with a different prefix — a follow-up concern.
const VERSION_MASK_VALUE: u32 = BM1362_INIT_PLAN.version_mask;

/// Init Control (0xA8) broadcast value for the pre-baud BM1362 AM2 stage.
const INIT_CONTROL_BCAST: u32 = BM1362_INIT_PLAN.init_control_broadcast;

/// Misc Control (0x18) values for BM1362. Pre-baud init uses 0xFF0FC100;
/// after FastUART 0x28 is written, the fast-baud stage uses 0x00C100B0.
const MISC_CONTROL_PRE_BAUD: u32 = BM1362_INIT_PLAN.misc_control_pre_baud;
const MISC_CONTROL_POST_FAST_BAUD: u32 = BM1362_INIT_PLAN.misc_control_post_fast_baud;

/// Init Control (0xA8) per-chip value from the traced AM2 plan.
const INIT_CONTROL_PER_CHIP: u32 = BM1362_INIT_PLAN.init_control_per_chip;

/// Core Register Ctrl — Hash Clock Control (enable hash counting).
const CORE_REG_HASH_CLK: u32 = BM1362_INIT_PLAN.core_reg_hash_clk;

/// Core Register Ctrl — Clock Delay Control.
/// BM1362-specific (vs 0x80008020 on BM1366, 0x8000800C on BM1370).
const CORE_REG_CLK_DELAY: u32 = BM1362_INIT_PLAN.core_reg_clk_delay;

/// Core Register Ctrl — third write (common to BM1366+ family).
const CORE_REG_FAMILY: u32 = BM1362_INIT_PLAN.core_reg_family;

/// Analog Mux — temperature diode enable (0x03 matches BM1366).
const ANALOG_MUX_VALUE: u32 = BM1362_INIT_PLAN.analog_mux_value;

/// Fast UART config (reg 0x28) — BM1362-specific baud encoding (vs 0x00003001
/// on BM1366). Programmed alongside MiscCtrl during the baud upgrade stage.
const FAST_UART_VALUE: u32 = BM1362_INIT_PLAN.fast_uart_value;

/// PERF-003: env-override name for the BM1362 FastUART (reg 0x28) value.
///
/// The daemon's `s19j_hybrid_mining.rs::am2_fast_uart_value()` reads this and
/// applies it (its `am2_env_u32` parser accepts the `0x`-prefixed hex form).
/// Exposed here as the single source of truth for the gate name so the driver
/// and the daemon agree.
pub const FAST_UART_VALUE_ENV: &str = "DCENT_AM2_FAST_UART_VALUE";

/// PERF-003: the RE-018 byte-order FastUART value (`0x1130_0000`).
///
/// RE-018's cold bosminer capture shows the BM1362 FastUART register written
/// in the byte order `0x1130_0000` (vs the `a lab unit`-proven compiled default
/// `0x0000_3011`). **This is NOT the compiled default** — flipping it would
/// change the fast-UART units of every live AM2 unit *before* an operator
/// live-A/B has confirmed the new byte order produces accepted shares. It is
/// applied ONLY when the operator sets `DCENT_AM2_FAST_UART_VALUE=0x11300000`.
/// Flagged for operator live-A/B.
pub const FAST_UART_VALUE_RE018: u32 = 0x1130_0000;

/// PERF-003: resolve the effective BM1362 FastUART value from an optional
/// operator override string (the raw `DCENT_AM2_FAST_UART_VALUE` value).
///
/// Pure helper so host tests can pin BOTH states without env mutation:
///   - `None` / unparseable  → the compiled `a lab unit`-proven default `0x0000_3011`
///     (load-bearing — never silently changes the live fast-UART units).
///   - `Some("0x11300000")`  → the RE-018 byte order `0x1130_0000`.
///
/// Accepts decimal or `0x`/`0X`-prefixed hex, mirroring the daemon's
/// `am2_env_u32` parser. A malformed override falls back to the default so a
/// typo can never reprogram the chip's UART.
pub fn resolve_fast_uart_value(override_raw: Option<&str>) -> u32 {
    let Some(raw) = override_raw else {
        return FAST_UART_VALUE;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return FAST_UART_VALUE;
    }
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        trimmed.parse::<u32>()
    };
    parsed.unwrap_or(FAST_UART_VALUE)
}

/// IO Driver Strength (reg 0x58) — normal chips. Domain-end chips (every 3rd
/// chip on S19j Pro, 42 domains × 3 chips) may want 0x0001F111 for signal
/// integrity; we broadcast the normal value and rely on bosminer-style
/// per-domain pre-drive only if we see runtime enumeration loss.
const IO_DRIVER_NORMAL: u32 = BM1362_INIT_PLAN.io_driver_normal;

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// BM1362 driver implementation.
pub struct Bm1362Driver;

impl Default for Bm1362Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1362Driver {
    pub fn new() -> Self {
        Self
    }

    /// Calculate WORK_TIME register value for BM1362.
    ///
    /// This is the 1-arg convenience form kept for back-compat with
    /// `work_dispatcher.rs:743` (`0x1362 => Bm1362Driver::calculate_work_time(min_freq_mhz)`).
    /// New call sites should prefer `calculate_work_time_for(chip_count, freq_mhz)`.
    pub fn calculate_work_time(freq_mhz: u16) -> u32 {
        Self::calculate_work_time_for(DEFAULT_CHIPS_PER_CHAIN, freq_mhz)
    }

    /// Calculate WORK_TIME register value for BM1362, parameterized by chip count.
    ///
    /// BM1362 has on-chip version rolling (16-bit version field × 126 chips ×
    /// 4 cores ≈ 33 MH per work item in 2^20 nonce-space terms). For FPGA-
    /// mediated dispatch we use a ~500 ms cadence divided across chips, same
    /// strategy as BM1368; serial dispatch on am2 (see `serial_mining.rs`)
    /// overrides this entirely with per-chip pacing.
    pub fn calculate_work_time_for(chip_count: u8, freq_mhz: u16) -> u32 {
        const FPGA_WORK_CLK: f64 = 100_000_000.0;
        let chips = chip_count.max(1) as f64;
        let freq_hz = freq_mhz.max(100) as f64 * 1_000_000.0;
        // PERF-007: the per-dispatch nonce budget (~2^20 nonces, with a
        // conservative 0.9 de-rate to avoid FIFO starvation at high MHz) is a
        // PER-CHAIN cadence that must be SHARED across all chips on the chain.
        // The previous formula divided `freq_hz * chips / chips`, where the
        // `chips` term cancelled out — so work-time never scaled with chip
        // count (a 28-chip and a 126-chip chain produced identical values).
        // Correct model (matching BM1368's `0.5 / chip_count` strategy): the
        // chain-level interval is `nonce_budget / freq_hz`, then divided across
        // `chips` so each chip gets a fair slice of the dispatch window. More
        // chips ⇒ shorter per-chip work-time.
        let chain_interval_s = 0.9 * 1_048_576.0 / freq_hz;
        let interval_s = chain_interval_s / chips;
        let work_time = (interval_s * FPGA_WORK_CLK) as u32;
        work_time.max(1)
    }

    fn read_pll_register(chain: &mut FpgaChain, chip_addr: u8) -> Result<Option<u32>> {
        crate::drivers::bm139x::read_pll_register(chain, chip_addr, regs::PLL0)
    }

    fn pll_register_to_freq(raw_reg: u32) -> Option<u16> {
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        let masked = raw_reg & !PLL_LOCK_BIT;
        MinerProfile::pll_frequencies_for_chip(CHIP_ID)
            .iter()
            .copied()
            .find(|&freq| Bm1362Driver::new().pll_params(freq).reg_value == masked)
    }

    /// Encode a BM1397+ broadcast WRITE (header 0x51, length 0x09).
    ///
    /// Routed through `bm139x` helpers so the wire format matches bosminer's
    /// `bm1362` driver. NEVER use `protocol::fifo_cmd_write_reg_bcast_full` for
    /// this chip — that helper emits header 0x58 (BM1387 SETCONFIG) which
    /// corrupts parser state on the BM1397+ family
    ///.
    #[inline]
    fn write_reg_broadcast(chain: &mut FpgaChain, reg: u8, value: u32) {
        let (w0, w1) = bm139x::fifo_write_reg_bcast(reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Encode a BM1397+ single-chip WRITE (header 0x41, length 0x09).
    #[inline]
    fn write_reg_single(chain: &mut FpgaChain, chip_addr: u8, reg: u8, value: u32) {
        let (w0, w1) = bm139x::fifo_write_reg_single(chip_addr, reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Triple-write MiscCtrl with 5 ms spacing.
    ///
    /// Private helper — the ONLY legal way to touch register 0x18 from this
    /// driver.: fire-and-forget
    /// MiscCtrl writes on BM1387 cost the DCENT_OS S9 port 75 s of zero-nonce
    /// stall. Readback verification of BM1362 register 0x18 is not possible
    /// via the FPGA CMD path on am2 (CMD FIFO does not relay back-channel
    /// answers —), so triple-write is
    /// the only reliable approach.
    fn misc_ctrl_triple_write(chain: &mut FpgaChain, value: u32) {
        for i in 0..3 {
            Self::write_reg_broadcast(chain, regs::MISC_CONTROL, value);
            std::thread::sleep(std::time::Duration::from_millis(5));
            tracing::trace!(
                chain_id = chain.chain_id,
                attempt = i + 1,
                "MiscCtrl triple-write {}/3 = 0x{:08X}",
                i + 1,
                value,
            );
        }
    }

    fn misc_ctrl_triple_write_single(chain: &mut FpgaChain, chip_addr: u8, value: u32) {
        for i in 0..3 {
            Self::write_reg_single(chain, chip_addr, regs::MISC_CONTROL, value);
            std::thread::sleep(std::time::Duration::from_millis(5));
            tracing::trace!(
                chain_id = chain.chain_id,
                chip_addr = format_args!("0x{:02X}", chip_addr),
                attempt = i + 1,
                "MiscCtrl per-chip triple-write {}/3 = 0x{:08X}",
                i + 1,
                value,
            );
        }
    }
}

impl ChipDriver for Bm1362Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1362"
    }

    fn cores_per_chip(&self) -> u32 {
        // BM1362 has 4 big cores per chip (small-die BM1397+ variant). Each
        // big core SHA-256 engine pushes ~826 MH/s @ 545 MHz, matching the
        // rated 104 TH/s / 378 chips = 275 GH/chip / 4 cores on S19j Pro.
        //
        // This differs from BM1366/BM1368/BM1370 (894–1280 "small cores")
        // because BraiinsOS reports cores as "big SHA256 engines", not the
        // nonce-space partitioning. Hashrate math stays consistent with the
        // MinerProfile `ghs_per_mhz = 0.529`.
        //
        // Source: live probe SUMMARY.md + 05-chip-identity.md (Agent 5).
        BM1362_BIG_CORES
    }

    fn response_length(&self) -> usize {
        RESPONSE_BYTES
    }

    fn default_baud(&self) -> u32 {
        115_200
    }

    fn max_baud(&self) -> u32 {
        // Live S19j Pro .139 confirmed 3.125 Mbaud stable after the MiscCtrl
        // triple-write upgrade (log: `Set baud rate @ requested: 3125000`).
        OPERATIONAL_BAUD
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        // am2 IMPLEMENTATION NOTE: On the native S19j Pro platform, ASIC
        // command traffic is carried on `/dev/ttyS2` / `/dev/ttyS3` UARTs
        //, not the FPGA CMD FIFO.
        // This `init_chain` path is kept for the UIO/FIFO cross-platform
        // compat build (e.g., S9 control board running a BM1362 hash board
        // via the universal-hash-board harness, or the future Zynq-only
        // bring-up fixture). `serial_mining.rs` owns the native am2 init
        // sequence verbatim. Both paths emit the SAME register values; only
        // the transport differs.

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1362: Init {} chips @ {} MHz (am2 register set via FPGA CMD transport)",
            chip_count,
            freq_mhz,
        );

        // === Step 0: Reset FPGA baud to 115 200 for enumeration & init.
        let current_baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);
        if current_baud_div != fpga_chain::BAUD_REG_115200 {
            tracing::info!(
                chain_id = chain.chain_id,
                current_baud_div = format_args!("0x{:02X}", current_baud_div),
                "Hot start detected: resetting FPGA baud to 115200",
            );
        }
        chain.set_baud(fpga_chain::BAUD_REG_115200);

        // === Step 1: Enable version rolling (3×). Register value is the
        // default 0x1FFFE000 pool mask — callers that negotiate a different
        // mask via Stratum must rewrite reg 0xA4 post-init.
        for _ in 0..3 {
            Self::write_reg_broadcast(chain, regs::VERSION_MASK, VERSION_MASK_VALUE);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        tracing::info!(
            chain_id = chain.chain_id,
            "Step 1: Version mask = 0x{:08X} (3×)",
            VERSION_MASK_VALUE,
        );

        // === Step 2: Chip discovery handled by daemon enumeration. See the
        // am2 `serial_mining.rs` path — CHAIN_INACTIVE broadcast, then
        // SET_ADDRESS 0x00/0x02/.../0xFC, then read_register(0) broadcast
        // to count 126 responses.

        // === Step 3: Init Control broadcast from the canonical BM1362 plan.
        Self::write_reg_broadcast(chain, regs::INIT_CONTROL, INIT_CONTROL_BCAST);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // === Step 4: Pre-baud-upgrade MiscCtrl (triple-write).
        Self::misc_ctrl_triple_write(chain, MISC_CONTROL_PRE_BAUD);
        tracing::info!(
            chain_id = chain.chain_id,
            "Step 4: MiscCtrl (pre-baud) triple-write = 0x{:08X}",
            MISC_CONTROL_PRE_BAUD,
        );

        // === Steps 5–6: Chain-inactive + address assignment handled by
        // daemon enumeration (see `serial_mining.rs` step 9).

        // === Step 7: Per-chip init registers.
        // Address stride 2 means chip_addr = i * ADDRESS_INTERVAL.
        let addr_interval = if chip_count == DEFAULT_CHIPS_PER_CHAIN {
            ADDRESS_INTERVAL as u16
        } else if chip_count > 0 {
            256u16 / chip_count as u16
        } else {
            256
        };

        tracing::info!(
            chain_id = chain.chain_id,
            "Step 7: Per-chip init ({} chips, stride={})",
            chip_count,
            addr_interval,
        );

        for i in 0..chip_count {
            let chip_addr = (i as u16 * addr_interval) as u8;
            Self::write_reg_single(chain, chip_addr, regs::INIT_CONTROL, INIT_CONTROL_PER_CHIP);
            Self::misc_ctrl_triple_write_single(chain, chip_addr, MISC_CONTROL_PRE_BAUD);
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_HASH_CLK);
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_CLK_DELAY);
            Self::write_reg_single(chain, chip_addr, regs::CORE_REG_CTRL, CORE_REG_FAMILY);

            if i % 16 == 15 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        // === Step 8: Ticket Mask (difficulty).
        let mask = self.ticket_mask(256);
        Self::write_reg_broadcast(chain, regs::TICKET_MASK, mask);
        std::thread::sleep(std::time::Duration::from_millis(10));
        tracing::info!(
            chain_id = chain.chain_id,
            "Step 8: TicketMask = 0x{:08X} (diff {})",
            mask,
            mask.wrapping_add(1),
        );

        // === Step 9: IO driver strength (reg 0x58).
        Self::write_reg_broadcast(chain, regs::IO_DRIVER_STRENGTH, IO_DRIVER_NORMAL);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // === Step 10: Analog Mux (temp diode).
        // Normal BM1362/AM2 init does not append the BM1370-style B9 tail.
        Self::write_reg_broadcast(chain, regs::ANALOG_MUX, ANALOG_MUX_VALUE);
        std::thread::sleep(std::time::Duration::from_millis(10));

        tracing::info!(
            chain_id = chain.chain_id,
            "Step 10: AnalogMux = 0x{:08X}; B9 tail omitted",
            ANALOG_MUX_VALUE,
        );

        // === Step 11: Baud upgrade — 115200 → 3.125 Mbaud.
        //
        // ORDER MATTERS: FastUART register first, THEN MiscCtrl triple-write
        // (verified 0x00C100B0 bytes), THEN switch the FPGA divider.
        // Gated on the caller having confirmed 100 % enumeration success.
        // On am2 serial dispatch the sequence is identical but writes go over
        // ttyS2/ttyS3 at 115200 before the switch.
        //
        // Keep the production MiscCtrl bytes unchanged here.
        Self::write_reg_broadcast(chain, regs::FAST_UART_CONFIG, FAST_UART_VALUE);
        std::thread::sleep(std::time::Duration::from_millis(10));

        Self::misc_ctrl_triple_write(chain, MISC_CONTROL_POST_FAST_BAUD);

        chain.set_baud(fpga_chain::BAUD_REG_3M);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!(
            chain_id = chain.chain_id,
            "Step 11: Baud upgraded — FastUART=0x{:08X}, MiscCtrl(fast)=0x{:08X} \
             FPGA BAUD_REG=0x{:02X}",
            FAST_UART_VALUE,
            MISC_CONTROL_POST_FAST_BAUD,
            fpga_chain::BAUD_REG_3M,
        );

        // === Step 12: Hash Counting Number (nonce-range partition).
        let nonce_range = match chip_count {
            0..=8 => 0xFFFF_FF1Fu32,
            9..=16 => 0xFFFF_FF0F,
            17..=32 => 0xFFFF_FF07,
            33..=64 => 0xFFFF_FF03,
            65..=128 => 0x0000_1381, // S19j Pro default (126 chips)
            _ => 0x0000_1381,
        };
        Self::write_reg_broadcast(chain, regs::HASH_COUNTING_NUMBER, nonce_range);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // === Step 13: Frequency ramp via reg 0x08 (PLL0).
        let start_freq: u16 = 400;
        let target_freq = freq_mhz.clamp(400, 597);
        let mut current = start_freq;
        while current < target_freq {
            let (pll_reg, actual) = bm1362_pll_lookup(current);
            Self::write_reg_broadcast(chain, regs::PLL0, pll_reg);
            std::thread::sleep(std::time::Duration::from_millis(100));
            tracing::debug!("PLL ramp {} MHz (reg=0x{:08X})", actual, pll_reg);
            current = current.saturating_add(12);
        }
        let pll = self.pll_params(target_freq);
        Self::write_reg_broadcast(chain, regs::PLL0, pll.reg_value);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!(
            chain_id = chain.chain_id,
            "Step 13: PLL final = 0x{:08X} ({} MHz)",
            pll.reg_value,
            target_freq,
        );

        // === Step 14: Belt-and-suspenders final version-mask write (matches
        // bosminer am2 "Modifying MiscCtrl" … re-arm pattern on every chain
        // init; also mirrors the `Ramping voltage 15.200 V -> 15.200 V (slow)`
        // PSU re-arm the caller performs separately).
        Self::write_reg_broadcast(chain, regs::VERSION_MASK, VERSION_MASK_VALUE);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // === FPGA WORK_TIME.
        //
        // TODO: Phase 2 Agent G will verify the exact FPGA WORK_TIME register
        // offset inside the am2 chain*-work-tx UIO region (SUMMARY.md lists
        // `likely chain*-work-tx +0x14`). Until then we use the existing
        // `fpga_chain::REG_WORK_TIME` which lands on the common/cmd region —
        // correct for S9-class boards, needs verification on am2.
        // See SUMMARY.md open question #2.
        let work_time = Self::calculate_work_time_for(chip_count, target_freq);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            chain_id = chain.chain_id,
            "WORK_TIME = 0x{:08X} ({} chips @ {} MHz)",
            work_time,
            chip_count,
            target_freq,
        );

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = target_freq,
            "BM1362: Chain init complete — {} chips @ {} MHz, version rolling enabled",
            chip_count,
            target_freq,
        );

        Ok(())
    }

    fn set_frequency(&self, chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        let pll = self.pll_params(freq_mhz);

        tracing::info!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz,
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            "BM1362: Setting frequency",
        );

        if chip_addr == 0xFF {
            Self::write_reg_broadcast(chain, regs::PLL0, pll.reg_value);
        } else {
            Self::write_reg_single(chain, chip_addr, regs::PLL0, pll.reg_value);
        }

        // Wait for PLL to lock (`locked` bit — SUMMARY.md step 12).
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
                    "BM1362 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1362 PLL0 readback timed out for chip 0x{:02X}",
                    target_addr
                ),
            }),
        }
    }

    fn set_voltage(&self, _pic: &mut PicController, _voltage_mv: u16) -> Result<()> {
        // S19j Pro voltage control lives in `DspicController::set_voltage(mv)`
        // (dsPIC33 @ 0x20/0x21/0x22, FW byte 0x89, SUM-framed protocol — see
        // ). The daemon routes voltage
        // commands based on `MinerProfile.pic_type`; this function is only
        // reached if the caller mistakenly treats BM1362 as a PIC16F1704 part.
        //
        // ADR-0010 / VoltageRail: MUST NOT return Ok(()) (silent success) —
        // that pattern made callers believe voltage was applied. Fail closed
        // with InvalidParameter so the routing bug is loud.
        tracing::warn!(
            "BM1362::set_voltage called — S19j Pro uses DspicController, not \
             PicController. Route voltage through MinerProfile.pic_type."
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1362 voltage is DspicController (not PicController); \
             route via MinerProfile.pic_type / VoltageRail"
                .into(),
        ))
    }

    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u16> {
        // FPGA-mediated work dispatch (universal hash-board / Zynq cross-build
        // paths). On native am2, serial_mining.rs owns dispatch.
        //
        // BM1362 uses BM1397+ FPGA framing: 4 header words + duplicate midstate
        // slots driven by the FPGA's active MIDSTATE_CNT. MIDSTATE_CNT=2 → 4
        // slots → 36 words per work packet.
        if work.midstates.is_empty() {
            return Err(crate::AsicError::InvalidParameter(
                "no midstates provided".into(),
            ));
        }

        let ms_cnt = (work.fpga_midstate_cnt as u32).clamp(2, 3);
        let num_slots = 1usize << ms_cnt;
        let work_words = 4 + num_slots * 8;
        let mut words = [0u32; 68];

        // Word 0: work_id << MIDSTATE_CNT. FPGA work_id is 8 bits — we rely on
        // the natural `u16` truncation to `u8` happening at the FPGA boundary;
        //  applies here: callers must not pre-
        // shift beyond 8 effective bits. See decode_nonce for the reconstruction.
        words[0] = (work.work_id as u32) << ms_cnt;

        words[1] = work.nbits;
        words[2] = work.ntime;
        words[3] = u32::from_le_bytes(work.merkle_tail);

        let midstate = &work.midstates[0];
        let mut ms_words = [0u32; 8];
        for (i, ms_word) in ms_words.iter_mut().enumerate() {
            let word_idx = 7 - i;
            *ms_word = u32::from_be_bytes([
                midstate[word_idx * 4],
                midstate[word_idx * 4 + 1],
                midstate[word_idx * 4 + 2],
                midstate[word_idx * 4 + 3],
            ]);
        }

        for slot in 0..num_slots {
            let base = 4 + slot * 8;
            words[base..base + 8].copy_from_slice(&ms_words);
        }

        chain.write_work(&words[..work_words]);
        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1362 nonce response (11 bytes on wire, packed into 2 FIFO words):
        //   Bytes [0:1]  0xAA 0x55 preamble (stripped by FPGA)
        //   Bytes [2:5]  Nonce (N3 N2 N1 N0, big-endian)
        //   Byte  [6]    MIDSTATE_NUM
        //   Byte  [7]    RESULT: job_id = (byte7 & 0xF0) >> 1
        //                        small_core = byte7 & 0x0F
        //   Bytes [8:9]  Version bits (VH VL, BE, << 13 when reconstructing)
        //   Byte  [10]   FLAGS (bit7=1 job response, bits 4:0 CRC5)
        //
        // FPGA packing (BM139X mode, MIDSTATE_CNT=2):
        //   Word 0: nonce value (32-bit)
        //   Word 1: [CRC:8 | extended_work_id:16 | solution_index:8]
        let nonce = raw[0];
        let w1 = raw[1];

        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;

        // Chip address encoded in nonce bits [24:17] (BM1397+ style).
        let chip_addr = ((nonce >> 17) & 0xFF) as u8;
        let addr_interval = if DEFAULT_CHIPS_PER_CHAIN > 0 {
            (256u16 / DEFAULT_CHIPS_PER_CHAIN as u16) as u8
        } else {
            1
        };
        let chip_index = if addr_interval > 0 {
            chip_addr / addr_interval
        } else {
            0
        };

        let midstate_mask = (1u16 << MIDSTATE_CNT_LOG2) - 1;
        let midstate_idx = (hw_work_id & midstate_mask) as u8;
        let work_id = hw_work_id >> MIDSTATE_CNT_LOG2;

        Ok(NonceResult {
            nonce,
            chip_index,
            work_id,
            solution_id,
            midstate_idx,
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        (fpga_clock_hz / (16 * target_baud.max(1))) - 1
    }

    fn ctrl_reg_value(&self) -> u32 {
        // S9-ONLY (am1 bitstream) CTRL value for BM1362 on the universal
        // hash-board cross-build path (S9 control board + BM1362 hash
        // board). BM139X mode + CTRL_ENABLE + MIDSTATE=2 slots via the S9
        // `CTRL_MIDSTATE_SHIFT` layout.
        //
        // This value is NOT valid on am2 (S19/S19j Pro native). am2 uses a
        // COMPLETELY DIFFERENT CTRL layout — see
        // `dcentrald_hal::fpga_chain::ctrl_am2::BM1362_DEFAULT` (`0x00901002`,
        // Phase 4A Agent β) and the module-level doc block at the top of
        // this file. Call sites that run on am2 must NOT use this function;
        // they should write `ctrl_am2::BM1362_DEFAULT` directly.
        //
        // Current call site: `daemon.rs:5299` — S9 cold-chain disable path
        // (pre-reset IP-core CTRL value used to clear ENABLE while preserving
        // MIDSTATE_CNT for re-arming on the S9 am1 bitstream). Verified S9
        // context only.
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE | (2 << fpga_chain::CTRL_MIDSTATE_SHIFT)
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // FPGA WORK_TIME-driven dispatch — poll at 1 ms, FPGA pulls work.
        1
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BM1397+ family: ticket mask is (difficulty - 1) with no bit-reverse.
        // Default 256 → 0xFF.
        difficulty.max(1).saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // Decode table entry for reporting. All BM1362 PLL table entries use
        // REFDIV=1, POSTDIV1=5, POSTDIV2=2.
        let (reg_value, _actual) = bm1362_pll_lookup(freq_mhz);
        let fbdiv = ((reg_value >> 16) & 0xFF) as u16;
        let refdiv = ((reg_value >> 8) & 0xFF) as u8;
        let postdiv_encoded = (reg_value & 0xFF) as u8;
        let post_div1 = ((postdiv_encoded >> 4) & 0x0F) + 1;
        let post_div2 = (postdiv_encoded & 0x0F) + 1;

        PllConfig {
            fb_div: fbdiv,
            ref_div: refdiv.max(1),
            post_div1,
            post_div2,
            reg_value,
        }
    }
}

// ---------------------------------------------------------------------------
// Serial work frame builder (BB platform + Amlogic + serial-mining path)
// ---------------------------------------------------------------------------
//
// On non-FPGA platforms (BeagleBone AM335x, Amlogic A113D, CVitek), BM1362
// receives work over the same UART that carries commands. The wire format is
// the BM1366-family "full header" job packet:
//
//   [0x55 0xAA]               2 B  preamble
//   [0x21]                    1 B  header (TYPE_JOB | GROUP_SINGLE | CMD_WRITE)
//   [0x56]                    1 B  length byte (= 86 = hdr+len+payload+CRC16)
//   [82-byte payload]              full block-header form (see layout below)
//   [CRC16 hi, CRC16 lo]      2 B  CRC-CCITT (poly 0x1021, init 0xFFFF)
//                                  computed over bytes [hdr..end-of-payload]
//                                  i.e. the 84 bytes from 0x21 through the
//                                  last payload byte. Preamble is NOT included.
//
//   Total wire length: 88 bytes.
//
// Payload layout (BM1362/BM1366 ESP-Miner full-header form):
//
//   [0]       job_id            (1 B, full byte)
//   [1]       num_midstates     (1 B, 0x01)
//   [2..6]    starting_nonce    (4 B, zeros)
//   [6..10]   nbits             (4 B LE)
//   [10..14]  ntime             (4 B LE)
//   [14..46]  merkle_root       (32 B, 32-bit-word reversed from internal byte order)
//   [46..78]  prev_block_hash   (32 B, 32-bit-word reversed)
//   [78..82]  version           (4 B LE)
//
// Reference: existing inline builder in `dcentrald/src/serial_mining.rs:3582-3597`
// (the canonical BM1362 BB/AML frame), and .
// This function is the testable, reusable extraction of that pattern.

/// Build the 82-byte BM1362 serial-work payload.
///
/// `asic_job_id` is the wire-level job ID (0..127) that gets embedded as
/// payload[0]. Callers are responsible for ring-buffer accounting; this
/// function performs no validation beyond the byte-layout itself.
pub fn build_serial_work_payload(work: &MiningWork, asic_job_id: u8) -> [u8; 82] {
    let mut payload = [0u8; 82];
    payload[0] = asic_job_id;
    payload[1] = 0x01; // num_midstates — BM1362 chip computes its own
                       // payload[2..6] = starting_nonce = 0 (already zero)
    payload[6..10].copy_from_slice(&work.nbits.to_le_bytes());
    payload[10..14].copy_from_slice(&work.ntime.to_le_bytes());
    let mr = reverse_32bit_words(&work.merkle_root);
    payload[14..46].copy_from_slice(&mr);
    let pbh = reverse_32bit_words(&work.prev_block_hash);
    payload[46..78].copy_from_slice(&pbh);
    payload[78..82].copy_from_slice(&work.version.to_le_bytes());
    payload
}

/// Build the full 88-byte BM1362 serial-work wire frame including preamble
/// and CRC16. Suitable for `transport.write_all(&frame)`.
pub fn build_serial_work_frame(work: &MiningWork, asic_job_id: u8) -> [u8; 88] {
    let payload = build_serial_work_payload(work, asic_job_id);
    let mut frame = [0u8; 88];
    frame[0] = 0x55;
    frame[1] = 0xAA;
    frame[2] = 0x21; // header
    frame[3] = 0x56; // length: 86 = hdr(1)+len(1)+payload(82)+CRC16(2)
    frame[4..86].copy_from_slice(&payload);
    // CRC over bytes [hdr..=last_payload_byte] = frame[2..86] (84 bytes).
    // Big-endian append: high byte first.
    let crc = protocol::crc16(&frame[2..86]);
    frame[86] = (crc >> 8) as u8;
    frame[87] = (crc & 0xFF) as u8;
    frame
}

/// Reverse 32-bit word order within a 32-byte array (8 words, MSB-first → LSB-first).
///
/// BM1362 expects merkle_root and prev_block_hash with each 32-bit word reversed
/// relative to internal Bitcoin byte order (mirrors the Bitcoin-Core
/// pre-image SHA convention used by ASIC fixed-function hardware).
fn reverse_32bit_words(data: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..8 {
        let src = (7 - i) * 4;
        let dst = i * 4;
        out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
    }
    out
}

impl Bm1362Driver {
    /// Write a work frame down a serial transport (BB / Amlogic / serial-mining path).
    ///
    /// This is the non-FPGA counterpart to [`ChipDriver::send_work`]. Returns
    /// the `asic_job_id` echoed back in payload[0] (also returned to make the
    /// signature symmetric with the FPGA path's `Result<u16>`).
    ///
    /// `transport` is anything that implements `std::io::Write` — typically a
    /// `SerialChain` from `dcentrald-hal`, but unit tests use `Vec<u8>`.
    pub fn send_work_serial<W: std::io::Write>(
        &self,
        transport: &mut W,
        work: &MiningWork,
        asic_job_id: u8,
    ) -> Result<u8> {
        if work.midstates.is_empty()
            && (work.merkle_root == [0u8; 32] || work.prev_block_hash == [0u8; 32])
        {
            return Err(crate::AsicError::InvalidParameter(
                "serial work requires either a midstate or full merkle_root + prev_block_hash"
                    .into(),
            ));
        }
        let frame = build_serial_work_frame(work, asic_job_id);
        transport
            .write_all(&frame)
            .map_err(|e| crate::AsicError::Hal(dcentrald_hal::HalError::Io(e)))?;
        Ok(asic_job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn decode_nonce_never_panics_on_arbitrary_fifo_words(raw in any::<[u32; 2]>()) {
            let drv = Bm1362Driver::new();
            let decoded = drv.decode_nonce(&raw);
            prop_assert!(decoded.is_ok());
        }
    }

    #[test]
    fn chip_identity() {
        let drv = Bm1362Driver::new();
        assert_eq!(drv.chip_id(), 0x1362);
        assert_eq!(drv.chip_name(), "BM1362");
        assert_eq!(drv.cores_per_chip(), 4);
        assert_eq!(drv.response_length(), 11);
        assert_eq!(drv.max_baud(), 3_125_000);
        assert_eq!(DEFAULT_CHIPS_PER_CHAIN, 126);
        assert_eq!(ADDRESS_INTERVAL, 2);
    }

    #[test]
    fn pll_register_to_freq_round_trips_known_frequencies() {
        // verify_frequency() reads back PLL0 and decodes it via
        // pll_register_to_freq. This pins that the decode is the exact inverse
        // of pll_params().reg_value for every table frequency, AND that the
        // PLL lock bit (MSB) is masked off before the lookup (a locked-PLL
        // readback has bit31 set). Read-only correctness check for the
        // LuxOS/BOS-style PLL-lock verification.
        let drv = Bm1362Driver::new();
        for &f in MinerProfile::pll_frequencies_for_chip(CHIP_ID) {
            let reg = drv.pll_params(f).reg_value;
            // Bare readback (no lock bit) decodes to the commanded frequency.
            assert_eq!(
                Bm1362Driver::pll_register_to_freq(reg),
                Some(f),
                "bare PLL0 readback 0x{:08X} must decode to {} MHz",
                reg,
                f
            );
            // Locked readback (MSB set) must mask the lock bit and still decode.
            assert_eq!(
                Bm1362Driver::pll_register_to_freq(reg | 0x8000_0000),
                Some(f),
                "locked PLL0 readback for {} MHz must mask bit31 before lookup",
                f
            );
        }
        // A register that maps to no known frequency returns None (so
        // verify_frequency surfaces an InvalidParameter rather than a false OK).
        assert_eq!(Bm1362Driver::pll_register_to_freq(0x0000_0000), None);
    }

    #[test]
    fn pll_table_covers_rated_freq() {
        // The rated 545 MHz and the live autotuned points 531 / 556 MHz must
        // all appear in the table (or resolve exactly via lookup).
        for &f in &[400u16, 500, 531, 545, 556, 597] {
            let (_reg, actual) = bm1362_pll_lookup(f);
            assert_eq!(actual, f, "expected exact PLL entry for {} MHz", f);
        }
    }

    #[test]
    fn sub400_extended_pll_is_optionb_fleet_safe() {
        // (1) FLEET-SAFETY PIN: the SHARED table / pll_frequencies MUST stay
        // 400-597 (no sub-400 rows) so the ~12 fleet consumers of
        // pll_frequencies_for_chip keep their proven 400 MHz runtime floor.
        assert_eq!(
            pll_frequencies()[0],
            400,
            "shared pll_frequencies floor must stay 400 (fleet)"
        );
        assert!(
            pll_frequencies().iter().all(|&f| f >= 400),
            "no sub-400 in shared pll_frequencies"
        );
        assert!(
            BM1362_PLL_TABLE.iter().all(|&(f, _)| f >= 400),
            "no sub-400 in BM1362_PLL_TABLE"
        );
        // (2) pll_lookup_extended computes the PROVEN ÷10 sub-400 encodings.
        for &(mhz, hex) in &[
            (240u16, 0x5060_0141u32),
            (270, 0x506C_0141),
            (300, 0x5078_0141),
            (320, 0x5080_0141),
            (350, 0x508C_0141),
            (370, 0x5094_0141),
        ] {
            let (reg, actual) = pll_lookup_extended(mhz);
            assert_eq!(reg, hex, "{mhz} MHz must encode to {hex:#010x}");
            assert_eq!(actual, mhz, "{mhz} MHz actual freq");
            assert_eq!(reg & 0xFF, 0x41, "proven postdiv 0x41 (÷10) for {mhz} MHz");
            assert_eq!((reg >> 24) & 0xFF, 0x50, "VCO_SCALE 0x50 for {mhz} MHz");
            assert!(25 * ((reg >> 16) & 0xFF) >= 2400, "VCO>=2400 for {mhz} MHz");
        }
        // >=400 delegates to the proven table (byte-identical).
        assert_eq!(pll_lookup_extended(525), bm1362_pll_lookup(525));
        // (3) decode_pll_reg_to_freq round-trips, incl. the RE-018 default = 50 MHz (NOT 525).
        assert_eq!(decode_pll_reg_to_freq(0x5080_0141), Some(320));
        assert_eq!(decode_pll_reg_to_freq(0x506C_0141), Some(270));
        assert_eq!(
            decode_pll_reg_to_freq(0x50D2_0141),
            Some(525),
            "proven 525 row decodes back"
        );
        assert_eq!(
            decode_pll_reg_to_freq(0x40A8_0265),
            Some(50),
            "RE-018 default decodes to 50 MHz, not the 525 config label"
        );
    }

    #[test]
    fn ticket_mask_basic() {
        let drv = Bm1362Driver::new();
        assert_eq!(drv.ticket_mask(256), 0xFF);
        assert_eq!(drv.ticket_mask(128), 0x7F);
        assert_eq!(drv.ticket_mask(1), 0);
        assert_eq!(drv.ticket_mask(0), 0);
    }

    #[test]
    fn work_time_never_zero() {
        // Even at a low frequency, WORK_TIME must not collapse to 0.
        assert!(Bm1362Driver::calculate_work_time(100) >= 1);
        assert!(Bm1362Driver::calculate_work_time_for(126, 545) >= 1);
    }

    #[test]
    fn ctrl_reg_s9_layout() {
        // ctrl_reg_value() is S9-ONLY (am1 bitstream). BM139X mode bit must
        // be set and ENABLE must be set; MIDSTATE=2 lives in the S9-specific
        // shift field. am2 (native S19j Pro) uses `ctrl_am2::BM1362_DEFAULT`
        // (0x00901002) instead — see module doc.
        let drv = Bm1362Driver::new();
        let ctrl = drv.ctrl_reg_value();
        assert!(ctrl & fpga_chain::CTRL_BM139X != 0);
        assert!(ctrl & fpga_chain::CTRL_ENABLE != 0);
    }

    #[test]
    fn bm1362_fast_baud_production_bytes() {
        assert_eq!(FAST_UART_VALUE, 0x0000_3011);
        assert_eq!(MISC_CONTROL_POST_FAST_BAUD, 0x00C1_00B0);
    }

    // === BM1362 factory-jig set_chain_baud (FUN_0002cb14) — pins the exact
    //     decoded RMW transform so the verified RE can never silently drift.
    //     Vectors hand-computed from the ARM disassembly (see the doc-comment
    //     on `jig_pll1_reclock_regs`).

    #[test]
    fn jig_reclock_threshold_is_3_000_001() {
        assert_eq!(PLL1_RECLOCK_BAUD_THRESHOLD, 0x002d_c6c1);
        assert_eq!(PLL1_RECLOCK_BAUD_THRESHOLD, 3_000_001);
        // 3_000_000 stays on the 25 MHz path (PLL1 untouched);
        // 3_000_001 crosses to the PLL1 reclock path.
        assert!(jig_pll1_reclock_regs(0, 0, 3_000_000).0.is_none());
        assert!(jig_pll1_reclock_regs(0, 0, 3_000_001).0.is_some());
    }

    #[test]
    fn jig_reclock_high_path_3_125_000_constants() {
        // DCENT's target baud. Above threshold → PLL1 reclock.
        // div = 400_000_000 / (3_125_000 << 3) = 16 → (div-1)=0x0F.
        let (r60, r28) = jig_pll1_reclock_regs(0, 0, 3_125_000);
        assert_eq!(r60, Some(0x5060_0111));
        assert_eq!(r28, 0x8450_0F00);
    }

    #[test]
    fn jig_reclock_high_path_12_000_000_jig_native() {
        // The jig's own configured baud (Config.ini Baudrate=12_000_000).
        // div = 400_000_000 / (12_000_000 << 3) = 4 → (div-1)=3.
        let (r60, r28) = jig_pll1_reclock_regs(0, 0, 12_000_000);
        assert_eq!(r60, Some(0x5060_0111));
        assert_eq!(r28, 0x8450_0300);
    }

    #[test]
    fn jig_reclock_preserves_readback_masked_bits() {
        // Masks keep exactly the readback bits the jig keeps:
        //   reg60 = (rb & 0xD000_C088) | 0x5060_0111
        //   reg28 = (rb & 0xFC0E_00FF) | 0x8450_0000 | ((div-1)&0xFF)<<8
        let (r60, r28) = jig_pll1_reclock_regs(0xFFFF_FFFF, 0xFFFF_FFFF, 3_125_000);
        assert_eq!(r60, Some(0xD060_C199));
        assert_eq!(r28, 0xFC5E_0FFF);
    }

    #[test]
    fn jig_reclock_low_path_115200_leaves_pll1_untouched() {
        // 115200 is far below threshold → 25 MHz reference, no reg 0x60.
        // div = 25_000_000 / (115200 << 3) = 27 → (div-1)=26=0x1A.
        let (r60, r28) = jig_pll1_reclock_regs(0, 0, 115_200);
        assert_eq!(r60, None);
        assert_eq!(r28, 0x0000_1A00);
    }

    #[test]
    fn bm1362_am2_init_plan_pins_traced_registers() {
        let plan = BM1362_INIT_PLAN;
        assert_eq!(plan.init_control_broadcast, 0x0007_0000);
        assert_eq!(plan.init_control_per_chip, 0x0007_01F0);
        assert_eq!(plan.misc_control_pre_baud, 0xFF0F_C100);
        assert_eq!(plan.misc_control_post_fast_baud, 0x00C1_00B0);
        assert_eq!(plan.fast_uart_register, 0x28);
        assert_eq!(plan.misc_control_register, 0x18);
        assert_eq!(plan.misc_control_triple_writes, 3);
        assert!(
            !plan.append_bm1370_tail,
            "normal BM1362 AM2 init must not append the BM1370 B9 tail"
        );
    }

    fn fixture_work() -> MiningWork {
        // Stable, deterministic fixture so the CRC test pins a known-good value.
        // No sentinel meaning — the bytes were chosen to exercise every quadrant
        // of the layout (job_id, nbits, ntime, merkle, prev, version).
        MiningWork {
            work_id: 0x0042,
            fpga_midstate_cnt: 2,
            version: 0x20000004,
            nbits: 0x17021369,
            ntime: 0x6634A1BC,
            merkle_tail: [0x11, 0x22, 0x33, 0x44],
            midstates: vec![[0u8; 32]],
            merkle_root: {
                let mut m = [0u8; 32];
                for (i, byte) in m.iter_mut().enumerate() {
                    *byte = i as u8 + 0x40;
                }
                m
            },
            prev_block_hash: {
                let mut p = [0u8; 32];
                for (i, byte) in p.iter_mut().enumerate() {
                    *byte = (0x80 + i) as u8;
                }
                p
            },
        }
    }

    #[test]
    fn serial_work_payload_layout() {
        // Verify the 82-byte payload matches the documented BM1362 ESP-Miner
        // full-header form byte-for-byte.
        let work = fixture_work();
        let payload = build_serial_work_payload(&work, 0x07);
        assert_eq!(payload.len(), 82);
        assert_eq!(payload[0], 0x07, "payload[0] = job_id");
        assert_eq!(payload[1], 0x01, "payload[1] = num_midstates");
        assert_eq!(&payload[2..6], &[0u8; 4], "payload[2..6] = starting_nonce");
        // nbits LE: 0x17021369 → 69 13 02 17
        assert_eq!(&payload[6..10], &[0x69, 0x13, 0x02, 0x17]);
        // ntime LE: 0x6634A1BC → BC A1 34 66
        assert_eq!(&payload[10..14], &[0xBC, 0xA1, 0x34, 0x66]);
        // merkle_root[0..4] in source = [0x40,0x41,0x42,0x43] but it's the
        // last 32-bit word in the reversed output, so payload[42..46] holds it.
        assert_eq!(&payload[42..46], &[0x40, 0x41, 0x42, 0x43]);
        // First word of source merkle_root = [0x5C,0x5D,0x5E,0x5F] (i=7) ends
        // up at payload[14..18] after reversal.
        assert_eq!(&payload[14..18], &[0x5C, 0x5D, 0x5E, 0x5F]);
        // prev_block_hash same pattern: source last word goes to payload[46..50]
        assert_eq!(&payload[46..50], &[0x9C, 0x9D, 0x9E, 0x9F]);
        // version LE: 0x20000004 → 04 00 00 20
        assert_eq!(&payload[78..82], &[0x04, 0x00, 0x00, 0x20]);
    }

    #[test]
    fn serial_work_frame_is_88_bytes_with_preamble_and_crc() {
        let work = fixture_work();
        let frame = build_serial_work_frame(&work, 0x07);
        assert_eq!(frame.len(), 88, "BM1362 serial wire frame is 88 bytes");
        assert_eq!(&frame[0..2], &[0x55, 0xAA], "preamble");
        assert_eq!(
            frame[2], 0x21,
            "header byte (TYPE_JOB | GROUP_SINGLE | CMD_WRITE)"
        );
        assert_eq!(frame[3], 0x56, "length byte = 86");
        // CRC is over [0x21 0x56 .. last_payload_byte] = frame[2..86] (84 B).
        let expected_crc = protocol::crc16(&frame[2..86]);
        let frame_crc = ((frame[86] as u16) << 8) | (frame[87] as u16);
        assert_eq!(frame_crc, expected_crc, "CRC16 BE-encoded at frame[86..88]");
    }

    #[test]
    fn serial_work_payload_round_trips_for_back_to_back_jobs() {
        // Different asic_job_id values must affect ONLY payload[0] and the CRC,
        // not any other byte. This protects against accidental contamination
        // from job-ID into other layout slots.
        let work = fixture_work();
        let p1 = build_serial_work_payload(&work, 0x05);
        let p2 = build_serial_work_payload(&work, 0x7F);
        assert_eq!(p1[0], 0x05);
        assert_eq!(p2[0], 0x7F);
        assert_eq!(&p1[1..], &p2[1..], "all other bytes must match");
    }

    #[test]
    fn send_work_serial_writes_full_frame_to_transport() {
        // Verifies the driver method writes the same 88-byte wire frame as
        // build_serial_work_frame returns directly.
        let drv = Bm1362Driver::new();
        let work = fixture_work();
        let mut buf: Vec<u8> = Vec::new();
        let returned_id = drv.send_work_serial(&mut buf, &work, 0x07).unwrap();
        assert_eq!(returned_id, 0x07);
        let expected = build_serial_work_frame(&work, 0x07);
        assert_eq!(buf.len(), 88);
        assert_eq!(&buf[..], &expected[..]);
    }

    #[test]
    fn send_work_serial_rejects_empty_inputs() {
        // No midstate + zero merkle_root + zero prev_block_hash = rejected.
        let drv = Bm1362Driver::new();
        let work = MiningWork {
            work_id: 0,
            fpga_midstate_cnt: 2,
            version: 0,
            nbits: 0,
            ntime: 0,
            merkle_tail: [0; 4],
            midstates: vec![],
            merkle_root: [0; 32],
            prev_block_hash: [0; 32],
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(drv.send_work_serial(&mut buf, &work, 0).is_err());
    }

    // -----------------------------------------------------------------
    // PERF-003 — FastUART env override (default stays 0x3011)
    // -----------------------------------------------------------------

    #[test]
    fn perf003_fast_uart_default_is_compiled_3011() {
        // Load-bearing: the compiled default MUST remain 0x0000_3011 (the
        // `a lab unit`-proven byte order). No env → default.
        assert_eq!(FAST_UART_VALUE, 0x0000_3011);
        assert_eq!(resolve_fast_uart_value(None), 0x0000_3011);
        assert_eq!(resolve_fast_uart_value(Some("")), 0x0000_3011);
        assert_eq!(resolve_fast_uart_value(Some("   ")), 0x0000_3011);
    }

    #[test]
    fn perf003_fast_uart_override_applies_re018_byte_order() {
        // The RE-018 byte order applies cleanly via the env override, in both
        // lower- and upper-case hex prefix forms. Plain decimal also works.
        assert_eq!(FAST_UART_VALUE_RE018, 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some("0x11300000")), 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some("0X11300000")), 0x1130_0000);
        assert_eq!(resolve_fast_uart_value(Some(" 0x11300000 ")), 0x1130_0000);
        assert_eq!(
            resolve_fast_uart_value(Some(&FAST_UART_VALUE_RE018.to_string())),
            0x1130_0000
        );
        // A malformed override falls back to the proven default — a typo can
        // never silently reprogram the chip UART.
        assert_eq!(resolve_fast_uart_value(Some("0xZZZZ")), FAST_UART_VALUE);
        assert_eq!(
            resolve_fast_uart_value(Some("not-a-number")),
            FAST_UART_VALUE
        );
        // Gate name is the agreed single source of truth.
        assert_eq!(FAST_UART_VALUE_ENV, "DCENT_AM2_FAST_UART_VALUE");
    }

    // -----------------------------------------------------------------
    // PERF-007 — work-time must scale with chip count
    // -----------------------------------------------------------------

    #[test]
    fn perf007_work_time_scales_with_chip_count() {
        // The previous formula had `freq_hz * chips / chips` (chips cancel),
        // so 28-chip and 126-chip chains produced identical work-time. After
        // the fix, more chips ⇒ shorter per-chip work-time.
        let wt_28 = Bm1362Driver::calculate_work_time_for(28, 545);
        let wt_126 = Bm1362Driver::calculate_work_time_for(126, 545);
        assert_ne!(
            wt_28, wt_126,
            "work-time must differ between 28 and 126 chips (PERF-007)"
        );
        assert!(
            wt_28 > wt_126,
            "fewer chips ⇒ longer per-chip window: {} vs {}",
            wt_28,
            wt_126
        );
        // Roughly inverse-proportional to chip count (126/28 = 4.5×).
        let ratio = wt_28 as f64 / wt_126 as f64;
        assert!(
            (ratio - 4.5).abs() < 0.2,
            "expected ~4.5× ratio for 126/28 chips, got {:.2}",
            ratio
        );
        // Never zero (the back-compat 1-arg form delegates here).
        assert!(Bm1362Driver::calculate_work_time(100) >= 1);
        assert!(wt_126 >= 1);
    }
}
