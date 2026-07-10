//! BM1370 ASIC driver (Antminer S21 Pro, S21+, S21 XP, BitAxe Gamma).
//!
//! The BM1370 is Bitmain's most advanced SHA-256 ASIC (3nm process).
//! It is closely related to BM1368 (S21) but with important differences.
//!
//! Key characteristics:
//!   - 3nm process (most advanced Bitmain chip)
//!   - 1280 cores per chip (80 domains x 16 small cores)
//!   - 65 chips per chain on S21 Pro (13 domains x 5)
//!   - ~1.2V core voltage (NoPic/LDO voltage control, same as BM1368/S21)
//!   - 11-byte nonce response with version field
//!   - Full header job format (82 bytes, chip computes midstate internally)
//!   - ASIC-internal hardware version rolling (register 0xA4)
//!   - Job ID increment: +24 mod 128 (same as BM1368)
//!   - Maximum 1 Mbps baud (via FAST_UART register 0x28)
//!   - PLL0 at 0x08 for hashing, PLL1 at 0x60 for baud, PLL2 at 0x64
//!   - FB_DIV range: 160-239 (vs 144-235 on BM1368)
//!   - PLL postdiv encoding: (postdiv1-1)<<4 | (postdiv2-1) (subtract 1)
//!   - CTRL_REG: BM139X mode (bit4=1)
//!   - Register 0xB9: BM1370-only mystery register (0x00004480)
//!   - Core register 0x3C: 0x80008B00, 0x8000800C (different from BM1368)
//!   - Core register 0x0D (0x80008DEE): BM1370-only, written at end of init
//!   - Analog Mux 0x54: 0x00000002 (vs 0x00000003 on BM1366/BM1368)
//!   - IO Driver 0x58: 0x00011111 (vs 0x02111111 on BM1366/BM1368)
//!   - Hash Counting 0x10: 0x00001EB5 (S21 Pro stock default)
//!   - Misc Control 0x18: 0xF000C100 (S21 Pro, vs 0xFF0FC100 on S21)
//!
//! Init sequence from ESP-Miner bm1370.c (11-step process):
//!   1. Version mask (3x) -> 2. Read chip IDs -> 3. Version mask (1 more)
//!   4. Reg_A8 -> 5. Misc Control -> 6. Chain Inactive -> 7. Address assignment
//!   8. Core Registers (broadcast) -> 9. Ticket Mask -> 10. IO Driver
//!   11. Per-chip config -> 12. BM1370-specific (0xB9, 0x54, 0x3C extra)
//!   13. Frequency ramp -> 14. Hash Counting Number
//!
//! Frequency ramp: 56.25 -> target MHz documented in Mujina
//!
//! References:
//!   - ESP-Miner bm1370.c (BitAxe Gamma driver)
//!   -  Section 10 (BM1370 register map)
//!   - esp-miner-asic-driver-analysis.md Section 9 (BM1370 init sequence)
//!   - Mujina frequency ramp tables

use crate::drivers::{ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1370 chip ID.
pub const CHIP_ID: u16 = 0x1370;

/// BM1370 default chips per chain (S21 Pro).
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 65;

/// BM1370 response size: 11 bytes (nonce + midstate_num + job_id + version + flags).
pub const RESPONSE_BYTES: usize = 11;

/// Number of SHA-256 cores per BM1370 chip (80 domains x 16 small cores).
const CORES_PER_CHIP: u32 = 1280;

/// Job ID increment step (same as BM1368).
const JOB_ID_STEP: u8 = 24;

/// Job ID modulus.
const JOB_ID_MOD: u8 = 128;

/// BM1370 work size: full header format.
/// Header (0x21) + length + job_id + num_midstates + starting_nonce(4) + nbits(4) +
/// ntime(4) + merkle_root(32) + prev_block_hash(32) + version(4) = 82 bytes payload.
/// Via FPGA WORK_TX_FIFO: ceil(82/4) = 21 words.
/// But the FPGA expects the same framing as BM139X mode:
/// 4 header words + 8 midstate words = 12 words per work item in BM139X mode.
///
/// For BM1370 (full header format), the FPGA in BM139X mode sends the work packet
/// differently from BM1387. The FPGA frames the job for the chip; we write:
///   Word 0: work_id
///   Word 1: nbits
///   Word 2: ntime
///   Word 3: merkle_root[28..32] (last 4 bytes, same position as merkle_tail)
///   Words 4-11: merkle_root[0..28] + prev_block_hash[0..4] packed
///   ...etc
///
/// Actually, for BM1366+ in BM139X FPGA mode with MIDSTATE_CNT=0 (1 midstate),
/// the FPGA serializes the work as a full-header job packet. The work frame is
/// 21 words (84 bytes) written to WORK_TX_FIFO.
const WORK_WORDS: usize = 21;

/// Crystal oscillator reference frequency (MHz).
const FREQ_MULT: f64 = 25.0;

/// BM1370 FB_DIV minimum.
const FB_DIV_MIN: u16 = 160;

/// BM1370 FB_DIV maximum.
const FB_DIV_MAX: u16 = 239;

/// BM1370 PLL VCO lock-range bounds — **Bitmain-canonical, from the unstripped
/// S21 Pro jig** `single_board_test.dec/get_pllparam_divider@CB644` (RE 2026-06-02):
/// the jig accepts a PLL config only when `2000 ≤ VCO ≤ 3200` MHz, additionally
/// `VCO ≤ 3125` when `REFDIV == 1`. (`VCO = 25 MHz × FBDIV / REFDIV`.)
///
/// dcentrald's [`compute_pll_params`] (ESP-Miner-derived) does NOT clamp VCO,
/// but its "lowest VCO" tie-break already lands every operating-range frequency
/// on a `REFDIV=2` / VCO 2000–2600 config — i.e. **inside this jig range**
/// (verified 0/13 out-of-range across 400–700 MHz; pinned by
/// `bm1370_pll_stays_within_bitmain_jig_vco_range`). These constants make that
/// cross-confirmation an explicit, regression-guarded contract: if a future edit
/// to the search (FBDIV range, tie-break) starts selecting an out-of-VCO-range
/// config that wouldn't lock on real BM1370 silicon, the test fails. No
/// behaviour change today — the algorithm is unchanged (it's proven on BitAxe
/// BM1370 via ESP-Miner); this is verification + a guard, not a re-tune.
// VCO encoder note (GhidraMCP goldmine 2026-06-10, decoded from
// S21pro/single_board_test `set_pllparameter@0xCADF8`): the stock PLL-parameter
// ENCODER accepts VCO ≥ 1600 MHz (with a bit28 band-select around 2400 MHz),
// but the validated lock FLOOR enforced here stays 2000 MHz. S21pro and S21xp
// may differ — do NOT lower this clamp without a live A/B on the live S21 .135.
const PLL_VCO_MIN_MHZ: f64 = 2000.0;
const PLL_VCO_MAX_MHZ: f64 = 3200.0;
const PLL_VCO_MAX_REFDIV1_MHZ: f64 = 3125.0;

/// Frequency ramp step size (MHz) — from ESP-Miner.
const FREQ_RAMP_STEP: f64 = 6.25;

/// Frequency ramp start (MHz).
const FREQ_RAMP_START: f64 = 56.25;

/// Delay between frequency ramp steps (ms).
const FREQ_RAMP_DELAY_MS: u64 = 100;

// ---------------------------------------------------------------------------
// BM1370 register addresses
// ---------------------------------------------------------------------------

/// BM1370 register addresses.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    /// Read value: 0x13700000 | (addr << 8).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 parameter register (hash clock PLL).
    pub const PLL0: u8 = 0x08;
    /// Hash counting number register (nonce range / chip distribution).
    pub const HASH_COUNTING: u8 = 0x10;
    /// Ticket mask register (hardware difficulty filter).
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
    ///
    /// Canonical >3 Mbaud UART-reclock procedure, first-hand decompiled from the
    /// S21pro jig `set_chain_baud@CB3B0` (GhidraMCP, goldmine intelligence-exploitation
    /// pass 2026-06-10). For a target baud `>= 0x2dc6c1` (3,000,001) the BM1370 must
    /// source its UART clock from PLL1 BEFORE writing FAST_UART (0x28):
    ///   - PLL1 (0x60) := (cache & 0xc088 | 0x111), high byte |= 0x50000000;
    ///     written TWICE via `send_set_config(chain,1,0,0x60,..)` with `usleep(10ms)`.
    ///   - FAST_UART (0x28) divider then = `400_000_000 / (baud<<3)` (PLL1 = 400 MHz),
    ///     OR-ed with the high-speed enable bits `0x84500000`.
    /// Below the threshold the FAST_UART divider is taken from the 25 MHz reference and
    /// PLL1 is left untouched. 3.125 Mbaud (the BM1370/S21 run baud, Saleae-confirmed
    /// on a live S19j Pro) is ABOVE the threshold, so the PLL1 reclock is mandatory for
    /// any BM1370 fast-baud path. NOTE: this is BM1370-specific — BM1362's fast-baud is
    /// a different bosminer-faithful protocol (reg 0x28=0x3011 + reg 0x18 MiscCtrl); do
    /// NOT cross-apply (see `am3_bb_mining.rs` rank-40 note + `SALEAE_PROTOCOL_REPORT.md`).
    /// Recorded as reference; DCENT's live S21/`a lab unit` path already mines at 3.125 Mbaud.
    pub const PLL1: u8 = 0x60;
    /// PLL2 parameter register. Third entry of the stock
    /// `pllparameter_register_array` = [0x08, 0x60, 0x64], decoded from
    /// `S21pro/single_board_test` .data @ 0x001FA848 via GhidraMCP
    /// (HashSource goldmine 2026-06-10) and cross-confirmed by the
    /// `set_pllparameter` inline literal. This byte-confirms PLL0=0x08 and
    /// PLL1=0x60 (already used) and fills in the previously-undocumented
    /// PLL2 address. Not yet wired into a write path (PLL0 alone drives the
    /// hash clock today); recorded for future multi-PLL domain work.
    pub const PLL2: u8 = 0x64;
    /// PLL3 parameter (domain config).
    pub const PLL3: u8 = 0x68;
    /// Version rolling mask register.
    pub const VERSION_ROLLING: u8 = 0xA4;
    /// Init control register (Reg_A8).
    pub const REG_A8: u8 = 0xA8;
    /// BM1370-specific misc settings register (NEW, unique to BM1370).
    pub const MISC_SETTINGS_B9: u8 = 0xB9;
    /// Domain-voltage ADC control register (0xB8 / 184). Drives the on-chip
    /// per-domain voltage ADC (see [`super::domain_voltage`]).
    pub const DOMAIN_VOLT_CTRL: u8 = 0xB8;
    /// Domain-voltage ADC aux registers (0xBA / 0xBB) written during a read.
    pub const DOMAIN_VOLT_BA: u8 = 0xBA;
    pub const DOMAIN_VOLT_BB: u8 = 0xBB;
    /// Domain-voltage ADC result register (0xBD / 189) — read back per sample.
    pub const DOMAIN_VOLT_ADC: u8 = 0xBD;
}

// ---------------------------------------------------------------------------
// BM1370 on-chip per-domain voltage ADC telemetry (DATA-ONLY).
//
// RE 2026-06-02 from the unstripped Bitmain S21 Pro test jig
//
// single_board_test.dec/{set_register_to_get_domain_voltage,adc_get_domain_vol_dv_out,
// adc_get_domain_voltage}` (per-function C decompilation, symbols intact;
// `get_register_value_with_ext_data(... 189u, ... "BM1370" ...)` confirms the
// chip family). This is the BM1370's BUILT-IN voltage telemetry, read over the
// ASIC register bus — distinct from (and independent of) the APW/PSU telemetry
// that `Apw121215f` fw=0x76 leaves uncharacterized. dcentrald's BM1370 driver
// previously had NO domain-voltage readback; this characterizes it byte-exact.
//
// **DATA-ONLY / NO live-sending method** (mirrors `dspic::decode_voltage_dac_reply`
// + `cvitek_cold_boot` discipline): the register sequence + the ADC decoder are
// pinned as constants/pure-fns so a future live `read_domain_voltage()` method
// (gated, unverified-on-silicon until an S21-Pro bench read) can use them without
// re-deriving the magic values. No code path sends these to a live chip today.
// ---------------------------------------------------------------------------

/// Byte-exact BM1370 domain-voltage ADC sequence + decoder (jig-sourced).
pub mod domain_voltage {
    /// `set_register_to_get_domain_voltage`: two 0xB8 setup writes that arm the
    /// per-domain ADC path. Apply in order with the documented inter-write delays.
    pub const SETUP_B8_1: u32 = 0x2000_2209; // then usleep(20_000)
    pub const SETUP_B8_2: u32 = 0x2020_2209; // then usleep(10_000)
    pub const SETUP_B8_1_DELAY_US: u32 = 20_000;
    pub const SETUP_B8_2_DELAY_US: u32 = 10_000;

    /// `adc_get_domain_vol_dv_out` aux-register writes (0xB9/0xBA/0xBB), fixed.
    pub const ADC_B9: u32 = 0x3F01_4381;
    pub const ADC_BA: u32 = 0x0004_0010;
    pub const ADC_BB: u32 = 0x0334_0E80;

    /// 0xB8 base value before the sample loop: `(adc_input_sel << 13) | this`.
    pub const ADC_B8_PRE: u32 = 0x230C_030D;
    /// 0xB8 base value inside the sample loop (triggers a conversion):
    /// `(adc_input_sel << 13) | this`, then `usleep(60_000)`, then read 0xBD.
    pub const ADC_B8_LOOP: u32 = 0x232C_030D;
    pub const ADC_SAMPLE_DELAY_US: u32 = 60_000;

    /// Loop bounds from the jig: collect up to 9 VALID samples, ≤20 attempts;
    /// the first valid sample is discarded, the rest averaged.
    pub const MAX_VALID_SAMPLES: u32 = 9;
    pub const MAX_ATTEMPTS: u32 = 20;

    /// The 5 ADC input selects = the 5 voltage domains the jig reads
    /// (`adc_get_domain_voltage` indexes `{1,2,3,4,5}`).
    pub const ADC_INPUT_SELECTS: [u8; 5] = [1, 2, 3, 4, 5];

    /// Build the 0xB8 value for a given ADC input select.
    /// `in_loop=false` → the pre-loop arm; `true` → the per-sample trigger.
    pub fn b8_value(adc_input_sel: u8, in_loop: bool) -> u32 {
        let base = if in_loop { ADC_B8_LOOP } else { ADC_B8_PRE };
        ((adc_input_sel as u32) << 13) | base
    }

    /// Decode a raw 0xBD ADC reply: bit 31 is the VALID flag; the 15-bit ADC
    /// count is `bits[14:0]`. Returns `None` if the sample isn't valid (the jig
    /// skips invalid samples). The raw→volts scale lives in the jig's higher
    /// `get_chain_asic_domain_avg_voltage` (a follow-up; not needed to pin the
    /// register/decoder contract here).
    pub fn decode_adc_sample(raw: u32) -> Option<u16> {
        if raw >> 31 == 1 {
            Some((raw & 0x7FFF) as u16)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// BM1370 register init values (from ESP-Miner bm1370.c + ASIC Register Bible)
// ---------------------------------------------------------------------------

/// Version rolling mask: 0x9000FFFF -> enable + full 16-bit rolling range.
/// Register 0xA4 value: prefix 0x9000, mask 0xFFFF (version_mask >> 13).
const VERSION_MASK_VALUE: u32 = 0x9000_FFFF;

/// Reg_A8 broadcast init value.
const REG_A8_BCAST: u32 = 0x0007_0000;

/// Reg_A8 per-chip init value.
const REG_A8_PER_CHIP: u32 = 0x0007_01F0;

/// Misc Control broadcast init value (S21 Pro).
const MISC_CTRL_BCAST: u32 = 0xF000_C100;

/// Misc Control per-chip value (S21 Pro).
const MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;

/// Core Register Control: first broadcast write.
/// CoreRegCtrl (0x3C) #1 — `set_clock_select_control` (S21 jig FUN_0xcf088):
/// `0x80008B00 | ((pulse_mode & 3) << 1)`. This value = pulse_mode 0.
const CORE_REG_1: u32 = 0x8000_8B00;

/// CoreRegCtrl (0x3C) #2 — `set_clock_delay_control` (BM1370-specific value 0x0C,
/// vs 0x18 on BM1368 / 0x20 on BM1366). Same builder
/// `0x80008000 | (pwth_sel<<3) | (ccdly<<6) | swpf` resolved from the S21 jig.
const CORE_REG_2: u32 = 0x8000_800C;

/// CoreRegCtrl (0x3C) #3 — per-chip AsicBoost / version-rolling enable, common to
/// all BM1366+. `0x800082AA` (= the value `bm1366.rs` previously called
/// `CORE_REG_UNKNOWN`; jig-confirmed 2026-06-10).
const CORE_REG_3: u32 = 0x8000_82AA;

/// Core Register Control: BM1370-specific additional write at end of init.
const CORE_REG_EXTRA: u32 = 0x8000_8DEE;

/// IO Driver Strength (S21 Pro, different from BM1366/BM1368).
const IO_DRIVER_VALUE: u32 = 0x0001_1111;

/// Mystery register 0xB9 value (BM1370-only, written twice during init).
const MISC_SETTINGS_B9_VALUE: u32 = 0x0000_4480;

/// Analog Mux Control (BM1370: 0x02, vs 0x03 on BM1366/BM1368).
const ANALOG_MUX_VALUE: u32 = 0x0000_0002;

/// Hash Counting Number (S21 Pro stock default).
const HASH_COUNTING_VALUE: u32 = 0x0000_1EB5;

/// Fast UART register value for 1 Mbps baud.
const FAST_UART_1M: u32 = 0x1130_0200;

/// Ticket mask difficulty (default 256).
const DEFAULT_TICKET_DIFFICULTY: u32 = 256;

// ---------------------------------------------------------------------------
// PLL helpers
// ---------------------------------------------------------------------------

/// Compute PLL parameters for a target frequency (MHz).
///
/// BM1370 PLL register encoding (register 0x08):
///   Byte 0: VDO_SCALE (0x40 if VCO < 2400 MHz, 0x50 if >= 2400 MHz)
///   Byte 1: FBDIV (feedback divider, range 160-239)
///   Byte 2: REFDIV (reference divider, typically 1 or 2)
///   Byte 3: POSTDIV = ((POSTDIV1-1) & 0x0F) << 4 | ((POSTDIV2-1) & 0x0F)
///
/// Frequency formula: freq = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)
///
/// Uses brute-force search from ESP-Miner pll_get_parameters():
///   refdiv: 1 or 2
///   postdiv1: 1..7, postdiv2: 1..7, postdiv1 > postdiv2
///   Priority: closest freq, then lowest VCO, then lowest postdiv product
fn compute_pll_params(target_mhz: f64) -> (u8, u8, u8, u8, u8) {
    // Default path: unclamped, byte-identical to ESP-Miner (proven on BitAxe
    // BM1370). The optional Bitmain-jig VCO clamp is gated; see
    // `compute_pll_params_inner` + `JIG_VCO_CLAMP_ENV`.
    let clamp = std::env::var(JIG_VCO_CLAMP_ENV).as_deref() == Ok("1");
    compute_pll_params_inner(target_mhz, clamp)
}

/// Env gate: constrain the BM1370 PLL search to the **Bitmain S21 Pro jig**
/// VCO lock range ([`PLL_VCO_MIN_MHZ`]..=[`PLL_VCO_MAX_MHZ`], ≤
/// [`PLL_VCO_MAX_REFDIV1_MHZ`] at REFDIV=1).
///
/// **Default OFF.** Off = byte-identical to ESP-Miner (proven on BitAxe BM1370,
/// VCO unconstrained). For the **operating range (≈400–700 MHz) the clamp is a
/// no-op** — the "lowest VCO" tie-break already lands those on REFDIV=2 / VCO
/// 2000–2600 (in jig range). It ONLY changes the **low-frequency ramp steps**
/// (≈56–150 MHz): unclamped, the search prefers a REFDIV=1 / VCO 4000–5975
/// config (strictly closer to target) that is OUTSIDE the BM1370's jig-specified
/// VCO lock range — which, if the jig's range is the true silicon limit, would
/// fail to lock mid-ramp. ESP-Miner/BitAxe apparently tolerates the higher VCO;
/// the S21 Pro jig constrains tighter. Set `=1` for an S21-Pro live A/B if the
/// ramp shows PLL-lock issues. Resolving which range is the true BM1370 limit
/// needs that live test — RE-ASK-BM1370-RAMP-VCO.
const JIG_VCO_CLAMP_ENV: &str = "DCENT_BM1370_JIG_VCO_CLAMP";

/// `true` iff `vco` is inside the Bitmain S21 Pro jig's accepted VCO range for
/// the given `refdiv` (jig `get_pllparam_divider`).
fn vco_in_jig_range(vco: f64, refdiv: u8) -> bool {
    let cap = if refdiv == 1 {
        PLL_VCO_MAX_REFDIV1_MHZ
    } else {
        PLL_VCO_MAX_MHZ
    };
    (PLL_VCO_MIN_MHZ..=PLL_VCO_MAX_MHZ).contains(&vco) && vco <= cap
}

fn compute_pll_params_inner(target_mhz: f64, clamp_vco: bool) -> (u8, u8, u8, u8, u8) {
    let mut best_fb: u8 = FB_DIV_MIN as u8;
    let mut best_ref: u8 = 1;
    let mut best_pd1: u8 = 1;
    let mut best_pd2: u8 = 1;
    let mut best_diff: f64 = f64::MAX;
    let mut best_vco: f64 = f64::MAX;

    for refdiv in 1u8..=2 {
        for postdiv1 in 1u8..=7 {
            for postdiv2 in 1u8..=postdiv1 {
                // postdiv1 must be >= postdiv2 (ESP-Miner constraint: postdiv1 > postdiv2,
                // but we allow equal for the case where both are 1)
                if postdiv1 < postdiv2 {
                    continue;
                }
                for fb_div in FB_DIV_MIN..=FB_DIV_MAX {
                    let freq = FREQ_MULT * fb_div as f64
                        / (refdiv as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target_mhz).abs();
                    let vco = FREQ_MULT * fb_div as f64 / refdiv as f64;

                    // Bitmain-jig VCO clamp (opt-in): never select a config the
                    // jig would reject as out-of-VCO-lock-range.
                    if clamp_vco && !vco_in_jig_range(vco, refdiv) {
                        continue;
                    }

                    if diff < best_diff || (diff == best_diff && vco < best_vco) {
                        best_diff = diff;
                        best_vco = vco;
                        best_fb = fb_div as u8;
                        best_ref = refdiv;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                    }
                }
            }
        }
    }

    let vdo_scale: u8 = if best_vco >= 2400.0 { 0x50 } else { 0x40 };
    (vdo_scale, best_fb, best_ref, best_pd1, best_pd2)
}

/// Encode PLL parameters into a 32-bit register value (big-endian on wire).
///
/// Wire format: [VDO_SCALE, FBDIV, REFDIV, POSTDIV_ENCODED]
fn encode_pll_reg(vdo_scale: u8, fb_div: u8, ref_div: u8, pd1: u8, pd2: u8) -> u32 {
    let postdiv_byte = ((pd1.saturating_sub(1)) & 0x0F) << 4 | ((pd2.saturating_sub(1)) & 0x0F);
    u32::from_be_bytes([vdo_scale, fb_div, ref_div, postdiv_byte])
}

/// Compute actual PLL output frequency from parameters.
fn pll_actual_freq(fb_div: u8, ref_div: u8, pd1: u8, pd2: u8) -> f64 {
    FREQ_MULT * fb_div as f64 / (ref_div as f64 * pd1 as f64 * pd2 as f64)
}

// ---------------------------------------------------------------------------
// BM1370 driver
// ---------------------------------------------------------------------------

/// BM1370 driver implementation.
pub struct Bm1370Driver;

impl Default for Bm1370Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1370Driver {
    pub fn new() -> Self {
        Self
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
            .find(|&freq| Bm1370Driver::new().pll_params(freq).reg_value == masked)
    }

    /// Write a register to all chips (broadcast) via CMD_TX_FIFO.
    fn write_reg_broadcast(chain: &mut FpgaChain, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_bcast_full(reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Write a register to a specific chip via CMD_TX_FIFO.
    fn write_reg_single(chain: &mut FpgaChain, chip_addr: u8, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_full(chip_addr, reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Run the BM1370-specific init sequence after enumeration and address assignment.
    ///
    /// This implements the 14-step init sequence documented in ESP-Miner bm1370.c
    /// and the ASIC Register Bible Section 10.
    fn run_init_sequence(chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        use std::time::Duration;

        let addr_interval = if chip_count > 0 {
            256u16 / chip_count as u16
        } else {
            256
        };

        // ── Rank 50 (goldmine ranks-40-50, 2026-06-10) — jig "Stage-1" SRST release,
        // VERIFIED + DEFERRED to DCENT_EE (NOT implemented here). The S21pro jig
        // `set_register_stage_1@510B4` does, BEFORE its pattern test:
        //   1. `set_core_srst(chain, mode=1, 0, is_active=0)` — a READ-MODIFY-WRITE of
        //      reg 0xA8 (soft_reset: clear bits 0..=3) AND reg 0x18 (misc: byte3=0xFF,
        //      byte2 |= 0x0F). NOT the fixed `reg0x18=0xFF000F` the work order claimed —
        //      that literal is a byte-order error; the real result is `0xFF0F_….` over
        //      the cached base value (verified `set_core_srst@CAF00`).
        //   2. `set_chain_ticketmask(chain, 0xFFFFFFFF)` (reg 0x14, per-byte LUT → 0xFFFFFFFF).
        //   3. `uart_flush_rx` + `usleep(50_000)`.
        // This lives on the jig's `pt_before_send_nonce` PATTERN-TEST path, not a mining
        // cold-boot init, and it touches the live S21 `a lab unit`. It is NOT added here because
        // (a) the RMW needs reliable reg 0xA8/0x18 reads (BM1370 reads often time out),
        // (b) jig-pattern-test ≠ mining-init, and (c) no way to soak-validate without the
        // live unit. DCENT_EE owns the decision (default-OFF gate + `a lab unit` soak) when picked
        // up. Source: goldmine `deliverables/RANKS_40_50_DESK_RE.md` (rank 50 / B08).

        // Step 1: Set version mask (3 times) — primes the version rolling hardware.
        for i in 0..3 {
            Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, VERSION_MASK_VALUE);
            std::thread::sleep(Duration::from_millis(10));
            tracing::debug!("Version mask write {}/3", i + 1);
        }

        // Step 2: Read chip IDs is done by the caller (enumerate phase).
        // Step 3: Version mask (one more time, 4th total).
        Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, VERSION_MASK_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!("Version mask write 4/4");

        // Step 4: Reg_A8 (broadcast) — init control.
        Self::write_reg_broadcast(chain, regs::REG_A8, REG_A8_BCAST);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", REG_A8_BCAST),
            "Reg_A8 broadcast",
        );

        // Step 5: Misc Control (broadcast) — S21 Pro value.
        Self::write_reg_broadcast(chain, regs::MISC_CONTROL, MISC_CTRL_BCAST);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", MISC_CTRL_BCAST),
            "Misc Control broadcast",
        );

        // Step 6: Chain Inactive + Step 7: Address assignment — done by caller.

        // Step 8: Core Register Control (broadcast).
        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, CORE_REG_1);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", CORE_REG_1),
            "Core Register Control #1 (broadcast)",
        );

        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, CORE_REG_2);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", CORE_REG_2),
            "Core Register Control #2 (broadcast, BM1370-specific 0x800C)",
        );

        // Step 9: Ticket Mask — set difficulty.
        let mask = DEFAULT_TICKET_DIFFICULTY.saturating_sub(1);
        Self::write_reg_broadcast(chain, regs::TICKET_MASK, mask);
        std::thread::sleep(Duration::from_millis(10));
        tracing::info!(
            difficulty = DEFAULT_TICKET_DIFFICULTY,
            mask = format_args!("0x{:08X}", mask),
            "Ticket Mask set (difficulty {})",
            DEFAULT_TICKET_DIFFICULTY,
        );

        // Step 10: IO Driver Strength (S21 Pro specific).
        Self::write_reg_broadcast(chain, regs::IO_DRIVER, IO_DRIVER_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", IO_DRIVER_VALUE),
            "IO Driver Strength",
        );

        // Step 11: Per-chip configuration.
        tracing::info!(
            chip_count = chip_count,
            "Starting per-chip init for {} chips (addr_interval={})",
            chip_count,
            addr_interval,
        );
        for i in 0..chip_count {
            let addr = (i as u16 * addr_interval) as u8;

            // a) Reg_A8
            Self::write_reg_single(chain, addr, regs::REG_A8, REG_A8_PER_CHIP);
            // b) Misc Control
            Self::write_reg_single(chain, addr, regs::MISC_CONTROL, MISC_CTRL_PER_CHIP);
            // c) Core Register Control #1
            Self::write_reg_single(chain, addr, regs::CORE_REG_CTRL, CORE_REG_1);
            // d) Core Register Control #2 (BM1370: 0x800C)
            Self::write_reg_single(chain, addr, regs::CORE_REG_CTRL, CORE_REG_2);
            // e) Core Register Control #3 (AsicBoost enable)
            Self::write_reg_single(chain, addr, regs::CORE_REG_CTRL, CORE_REG_3);

            // Small delay between chips (ESP-Miner uses no explicit delay for BM1370,
            // unlike BM1368 which uses 500ms — keep it brief).
            std::thread::sleep(Duration::from_millis(5));
        }
        tracing::info!("Per-chip init complete for {} chips", chip_count);

        // Step 12: BM1370-specific registers.

        // Mystery register 0xB9 (written twice, purpose unknown).
        Self::write_reg_broadcast(chain, regs::MISC_SETTINGS_B9, MISC_SETTINGS_B9_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", MISC_SETTINGS_B9_VALUE),
            "Register 0xB9 write #1 (BM1370-specific)",
        );

        // Analog Mux Control (0x02 for BM1370, controls temperature diode).
        Self::write_reg_broadcast(chain, regs::ANALOG_MUX, ANALOG_MUX_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", ANALOG_MUX_VALUE),
            "Analog Mux Control (0x02, BM1370-specific)",
        );

        // Mystery register 0xB9 (second write, duplicate).
        Self::write_reg_broadcast(chain, regs::MISC_SETTINGS_B9, MISC_SETTINGS_B9_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", MISC_SETTINGS_B9_VALUE),
            "Register 0xB9 write #2 (BM1370-specific, duplicate)",
        );

        // Additional core register write (BM1370-only, beyond documented range).
        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, CORE_REG_EXTRA);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!(
            value = format_args!("0x{:08X}", CORE_REG_EXTRA),
            "Core Register Control extra (0x80008DEE, BM1370-only)",
        );

        // Step 13: Frequency ramp from 56.25 MHz to target frequency.
        // ESP-Miner ramps in 6.25 MHz steps with 100ms delays.
        tracing::info!(
            target_mhz = freq_mhz,
            "Starting frequency ramp: {:.2} MHz -> {} MHz ({:.2} MHz steps, {}ms delay)",
            FREQ_RAMP_START,
            freq_mhz,
            FREQ_RAMP_STEP,
            FREQ_RAMP_DELAY_MS,
        );

        let mut current_freq = FREQ_RAMP_START;
        let target = freq_mhz as f64;

        while current_freq < target {
            current_freq += FREQ_RAMP_STEP;
            if current_freq > target {
                current_freq = target;
            }

            let (vdo, fb, rd, pd1, pd2) = compute_pll_params(current_freq);
            let pll_reg = encode_pll_reg(vdo, fb, rd, pd1, pd2);
            Self::write_reg_broadcast(chain, regs::PLL0, pll_reg);
            std::thread::sleep(Duration::from_millis(FREQ_RAMP_DELAY_MS));
        }

        let actual = pll_actual_freq(
            compute_pll_params(target).1,
            compute_pll_params(target).2,
            compute_pll_params(target).3,
            compute_pll_params(target).4,
        );
        tracing::info!(
            target_mhz = freq_mhz,
            actual_mhz = format_args!("{:.2}", actual),
            "Frequency ramp complete — target {} MHz, actual {:.2} MHz",
            freq_mhz,
            actual,
        );

        // Step 14: Hash Counting Number (S21 Pro stock default).
        Self::write_reg_broadcast(chain, regs::HASH_COUNTING, HASH_COUNTING_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::info!(
            value = format_args!("0x{:08X}", HASH_COUNTING_VALUE),
            "Hash Counting Number set (S21 Pro stock default)",
        );

        // Final: Set version mask one more time to ensure it's active.
        Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, VERSION_MASK_VALUE);
        std::thread::sleep(Duration::from_millis(10));
        tracing::debug!("Final version mask set");

        Ok(())
    }

    /// Calculate WORK_TIME register value for a given frequency.
    ///
    /// BM1370 uses BM139X FPGA mode with MIDSTATE_CNT=0 (1 midstate, chip computes internally).
    /// Formula: work_time = 0.9 * 1 * 2^32 / (freq_Hz * nchips) * FPGA_WORK_CLK
    /// For a single-midstate chip, the nonce range is the full 2^32 divided among chips.
    /// With ASIC-internal version rolling, the effective nonce space is much larger,
    /// so work_time is mainly a job dispatch pacing mechanism.
    pub fn calculate_work_time(_freq_mhz: u16, chip_count: u8) -> u32 {
        // Use a simpler approximation: 500ms / chip_count as base interval,
        // matching ESP-Miner's BM1370 job interval of 500ms / asic_count.
        const FPGA_WORK_CLK: u64 = 100_000_000;
        let interval_ms = 500u64 / (chip_count as u64).max(1);
        let work_time = (interval_ms * FPGA_WORK_CLK / 1000) as u32;
        work_time.max(1)
    }
}

impl ChipDriver for Bm1370Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1370"
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
        1_000_000
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1370: Initializing chain — {} chips, target {} MHz",
            chip_count,
            freq_mhz,
        );

        // Set FPGA baud to 115200 for configuration commands.
        chain.set_baud(fpga_chain::BAUD_REG_115200);
        tracing::debug!("FPGA baud set to 115200 (BAUD_REG=0x6C)");

        // Run the full BM1370 init sequence.
        Self::run_init_sequence(chain, chip_count, freq_mhz)?;

        // Set WORK_TIME for the FPGA.
        let work_time = Self::calculate_work_time(freq_mhz, chip_count);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            work_time = format_args!("0x{:08X}", work_time),
            "WORK_TIME set for {} chips at {} MHz",
            chip_count,
            freq_mhz,
        );

        // Upgrade baud rate to 1 Mbps via FAST_UART register.
        // Step 1: Write FAST_UART register on all chips.
        Self::write_reg_broadcast(chain, regs::FAST_UART, FAST_UART_1M);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!(
            value = format_args!("0x{:08X}", FAST_UART_1M),
            "FAST_UART set to 1 Mbps on all chips",
        );

        // Step 2: Switch FPGA baud to match.
        // FPGA baud for 1 Mbps: FPGA_CLK / (16 * baud) - 1 = 200M / (16 * 1M) - 1 = 11.5 -> 11
        let fpga_baud_1m = (fpga_chain::FPGA_CLK_HZ / (16 * 1_000_000)) - 1;
        chain.set_baud(fpga_baud_1m);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!(
            baud_reg = format_args!("0x{:02X}", fpga_baud_1m),
            "FPGA baud set to 1 Mbps (BAUD_REG=0x{:02X})",
            fpga_baud_1m,
        );

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1370: Chain init complete — {} chips at {} MHz, 1 Mbps baud",
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
            "BM1370: Setting PLL frequency",
        );

        if chip_addr == 0xFF {
            // Broadcast to all chips — ramp from current to target.
            // For simplicity, write the target directly (caller should ramp if needed).
            Self::write_reg_broadcast(chain, regs::PLL0, pll.reg_value);
        } else {
            Self::write_reg_single(chain, chip_addr, regs::PLL0, pll.reg_value);
        }

        // Wait for PLL to lock.
        std::thread::sleep(std::time::Duration::from_millis(10));
        tracing::debug!("PLL lock wait complete (10ms)");

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
                    "BM1370 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1370 PLL0 readback timed out for chip 0x{:02X}",
                    target_addr
                ),
            }),
        }
    }

    fn set_voltage(&self, _pic: &mut PicController, _voltage_mv: u16) -> Result<()> {
        // S21 Pro uses NoPic/LDO voltage control — NO PIC microcontroller.
        // Voltage is controlled via TAS5782M audio DACs repurposed as voltage
        // controllers, accessed over I2C. The PicController is not used.
        //
        // TODO: Implement TAS5782M I2C DAC voltage control.
        tracing::warn!(
            "BM1370: set_voltage() not yet implemented — S21 Pro uses TAS5782M DAC, not PIC",
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1370/S21 Pro uses TAS5782M DAC for voltage control, not PIC".into(),
        ))
    }

    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u16> {
        // BM1370 uses the full header job format (82 bytes).
        // The chip computes midstate internally and does version rolling autonomously.
        //
        // FPGA WORK_TX_FIFO format for BM139X mode with full header:
        //   Word 0:      work_id
        //   Word 1:      nbits (32-bit LE)
        //   Word 2:      ntime (32-bit LE)
        //   Word 3:      merkle_root bytes [28..32] (last 4 bytes, LE)
        //   Words 4-10:  merkle_root bytes [0..28] (7 words, LE)
        //   Words 11-18: prev_block_hash (8 words, 32 bytes, LE)
        //   Word 19:     version (32-bit LE)
        //   Word 20:     (padding / reserved, zero)
        //
        // The FPGA frames this into the BM1366/BM1368/BM1370 job packet format
        // on the wire: [0x55 0xAA 0x21 0x56 job_id 0x01 start_nonce[4] nbits[4]
        //               ntime[4] merkle_root[32] prev_block_hash[32] version[4] CRC16[2]]

        let mut words = [0u32; WORK_WORDS];

        // Word 0: work_id. BM1370 in BM139X mode with MIDSTATE_CNT=0 (1 midstate)
        // means no shift needed — work_id is used directly.
        words[0] = work.work_id as u32;

        // Word 1: nbits
        words[1] = work.nbits;

        // Word 2: ntime
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root) — same position as BM1387.
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Words 4-10: Full merkle root bytes [0..28] (7 words, reversed word order).
        // BM1370 full-header format: the chip computes midstates internally,
        // so we send the actual merkle root (NOT the midstate).
        let merkle = &work.merkle_root;
        for i in 0..7 {
            let word_idx = 6 - i; // Word 7 is already carried by merkle_tail above.
            words[4 + i] = u32::from_be_bytes([
                merkle[word_idx * 4],
                merkle[word_idx * 4 + 1],
                merkle[word_idx * 4 + 2],
                merkle[word_idx * 4 + 3],
            ]);
        }

        // Words 11-18: prev_block_hash (32 bytes, reversed word order).
        let prev_hash = &work.prev_block_hash;
        for i in 0..8 {
            let word_idx = 7 - i;
            words[11 + i] = u32::from_be_bytes([
                prev_hash[word_idx * 4],
                prev_hash[word_idx * 4 + 1],
                prev_hash[word_idx * 4 + 2],
                prev_hash[word_idx * 4 + 3],
            ]);
        }

        // Word 19: version (block version with version rolling base).
        words[19] = work.version;

        // Word 20: starting_nonce/reserved (zero).
        words[20] = 0;

        // DIAGNOSTIC: Log first work item.
        use std::sync::atomic::{AtomicBool, Ordering as AOrdering};
        static FIRST_WORK_LOGGED: AtomicBool = AtomicBool::new(false);
        if !FIRST_WORK_LOGGED.swap(true, AOrdering::Relaxed) {
            tracing::info!(
                chain_id = chain.chain_id,
                work_id = work.work_id,
                nbits = format_args!("0x{:08X}", work.nbits),
                ntime = format_args!("0x{:08X}", work.ntime),
                "BM1370: First work item — {} FIFO words",
                WORK_WORDS,
            );
            for (i, word) in words.iter().take(WORK_WORDS.min(12)).enumerate() {
                tracing::debug!("WORK_TX[{}] = 0x{:08X}", i, word,);
            }
        }

        // Write to WORK TX FIFO.
        chain.write_work(&words);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1370 nonce response (11 bytes, from WORK_RX_FIFO):
        //
        // After FPGA processing, we get 2 x 32-bit words:
        //   Word 0: nonce value (32-bit)
        //   Word 1: packed metadata
        //
        // BM1370 response wire format (11 bytes):
        //   Bytes 0-1: Preamble (0xAA 0x55) — stripped by FPGA
        //   Bytes 2-5: Nonce (32-bit, big-endian on wire)
        //   Byte 6:    midstate_num
        //   Byte 7:    job_id + small_core_id
        //              job_id = (byte7 & 0xF0) >> 1 (upper 4 bits, shifted right 1)
        //              small_core_id = byte7 & 0x0F (lower 4 bits, 16 small cores)
        //   Bytes 8-9: version bits (16-bit, big-endian)
        //              rolled_version_bits = ntohs(bytes[8:9]) << 13
        //   Byte 10:   flags: bit 7 = is_job (1=nonce, 0=register read)
        //              bits 4:0 = CRC-5
        //
        // Nonce field encoding (same as BM1368):
        //   bits[31:25] = core_id (7 bits, 80 core domains)
        //   bits[24:17] = asic_address (8 bits)
        //   bits[16:0]  = nonce value (17 bits)
        //
        // FPGA packs into word1 similarly to BM1387 but with different field positions.
        let nonce = raw[0];
        let w1 = raw[1];
        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;

        // In FPGA BM139X mode with MIDSTATE_CNT=0, word 0 carries the raw work_id
        // directly and the FIFO returns that same 8-bit ID in hw_work_id[7:0].
        // Re-applying the ASIC wire-format nibble shuffle here corrupts the lookup
        // key and makes every nonce miss the global work table.
        let work_id = hw_work_id;

        // Chip index from nonce bits [24:17] (8-bit address / interval).
        // The caller needs to divide by address_interval to get chip index.
        let chip_addr = ((nonce >> 17) & 0xFF) as u8;

        Ok(NonceResult {
            nonce,
            chip_index: chip_addr,
            work_id,
            solution_id,
            midstate_idx: 0, // BM1370 uses ASIC-internal version rolling, no midstate index
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        (fpga_clock_hz / (16 * target_baud)) - 1
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM1370 uses BM139X mode (bit4=1), ENABLE (bit3=1), MIDSTATE_CNT=0 (1 midstate).
        // The chip handles version rolling internally, so only 1 midstate slot is needed.
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE
    }

    fn job_interval_ms(&self, chip_count: u8, _freq_mhz: u16) -> u32 {
        // ESP-Miner BM1370: interval = 500ms / asic_count.
        // Minimum 1ms to prevent busy-spinning.
        let interval = 500u32 / (chip_count as u32).max(1);
        interval.max(1)
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BM1370 uses simple (difficulty - 1) encoding (same as BM1366/BM1368).
        // The bit-reversal used by BM1387 is NOT needed for BM1366+ chips.
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        let target = freq_mhz as f64;
        let (vdo, fb, rd, pd1, pd2) = compute_pll_params(target);
        let reg_value = encode_pll_reg(vdo, fb, rd, pd1, pd2);
        PllConfig {
            fb_div: fb as u16,
            ref_div: rd,
            post_div1: pd1,
            post_div2: pd2,
            reg_value,
        }
    }
}

/// Get the sorted list of discrete PLL frequencies the BM1370 can produce (MHz).
///
/// Computed from the PLL parameter space: fb_div 160-239, refdiv 1-2,
/// postdiv1 1-7, postdiv2 1-postdiv1. For practical use, we list commonly
/// used frequencies at 25 MHz steps within the typical operating range.
pub fn pll_frequencies() -> &'static [u16] {
    &[
        200, 225, 250, 275, 300, 325, 350, 375, 400, 425, 450, 475, 500, 525, 550, 575, 600,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chip_id() {
        let driver = Bm1370Driver::new();
        assert_eq!(driver.chip_id(), 0x1370);
    }

    #[test]
    fn domain_voltage_adc_matches_bitmain_s21pro_jig() {
        use domain_voltage as dv;
        // Byte-exact vs HashSource S21pro single_board_test.dec
        // (set_register_to_get_domain_voltage / adc_get_domain_vol_dv_out).
        assert_eq!(dv::SETUP_B8_1, 0x2000_2209); // decimal 536879625
        assert_eq!(dv::SETUP_B8_2, 0x2020_2209); // decimal 538976777
        assert_eq!(dv::ADC_B9, 0x3F01_4381); // decimal 1057047425
        assert_eq!(dv::ADC_BA, 0x0004_0010); // decimal 262160
        assert_eq!(dv::ADC_BB, 0x0334_0E80); // decimal 53743232
        assert_eq!(dv::ADC_B8_PRE, 0x230C_030D);
        assert_eq!(dv::ADC_B8_LOOP, 0x232C_030D);
        assert_eq!(dv::ADC_SAMPLE_DELAY_US, 60_000);
        assert_eq!(dv::MAX_VALID_SAMPLES, 9);
        assert_eq!(dv::ADC_INPUT_SELECTS, [1, 2, 3, 4, 5]);

        // b8_value composition: (sel << 13) | base.
        assert_eq!(dv::b8_value(1, false), (1u32 << 13) | 0x230C_030D);
        assert_eq!(dv::b8_value(5, true), (5u32 << 13) | 0x232C_030D);

        // ADC sample decode: bit31 = VALID, value = bits[14:0].
        assert_eq!(dv::decode_adc_sample(0x8000_1234), Some(0x1234)); // valid
        assert_eq!(dv::decode_adc_sample(0x8000_7FFF), Some(0x7FFF)); // valid, max
        assert_eq!(dv::decode_adc_sample(0x8000_8001), Some(0x0001)); // bit15 masked off
        assert_eq!(dv::decode_adc_sample(0x0000_1234), None); // bit31 clear → invalid

        // Result register is 0xBD (189) — the jig's get_register_value_with_ext_data arg.
        assert_eq!(regs::DOMAIN_VOLT_ADC, 0xBD);
        assert_eq!(regs::DOMAIN_VOLT_CTRL, 0xB8);
    }

    #[test]
    fn bm1370_pll_vs_bitmain_jig_vco_range_and_clamp() {
        // Cross-check dcentrald's ESP-Miner PLL search against the Bitmain S21 Pro
        // jig get_pllparam_divider VCO constraint (2000..=3200, <=3125 @ REFDIV=1).
        let vco_of = |fb: u8, rd: u8| FREQ_MULT * fb as f64 / rd as f64;

        // (1) THE FINDING (load-bearing): the UNCLAMPED ESP-Miner search selects an
        //     out-of-jig-VCO config (REFDIV=1, VCO 4000-5975) for a SIGNIFICANT
        //     fraction of the OPERATING range — it's strictly closer to target than
        //     any REFDIV=2 config. Pin the exact count so the finding can't silently
        //     change: 66 of the 301 integer targets in 400-700 MHz (~22%).
        let mut unclamped_out = 0;
        for t in 400..=700u32 {
            let (_v, fb, rd, _p1, _p2) = compute_pll_params_inner(t as f64, false);
            if !vco_in_jig_range(vco_of(fb, rd), rd) {
                unclamped_out += 1;
            }
        }
        assert_eq!(
            unclamped_out, 66,
            "expected 66/301 unclamped 400-700 targets out of jig VCO range (the finding)"
        );
        // 447 MHz is a concrete operating-range example (VCO 4025 > jig range).
        let (_v, fb, rd, _p1, _p2) = compute_pll_params_inner(447.0, false);
        assert!(
            !vco_in_jig_range(vco_of(fb, rd), rd),
            "unclamped 447 MHz must be out of jig range"
        );

        // (2) The gated FIX: CLAMPED, EVERY frequency 100-900 MHz (the whole ramp
        //     band from 56.25 up + the operating range) stays inside the jig VCO
        //     lock range — i.e. only configs that lock on real BM1370 per the jig.
        for t in 100..=900u32 {
            let (_v, fb, rd, _p1, _p2) = compute_pll_params_inner(t as f64, true);
            assert!(
                vco_in_jig_range(vco_of(fb, rd), rd),
                "clamped: target {t} MHz VCO {} out of jig range (fb={fb} rd={rd})",
                vco_of(fb, rd)
            );
        }

        // (3) The clamp keeps frequency accuracy across the operating band: the
        //     best in-jig-range config is at most ~2 MHz off target (≤1.5 measured),
        //     negligible for mining — so the fix doesn't sacrifice tunability.
        let mut worst = 0.0_f64;
        for t in 400..=700u32 {
            let (_v, fb, rd, p1, p2) = compute_pll_params_inner(t as f64, true);
            worst = worst.max((pll_actual_freq(fb, rd, p1, p2) - t as f64).abs());
        }
        assert!(
            worst <= 2.0,
            "clamped worst freq error {worst} MHz across 400-700 (>2)"
        );

        // (4) Default entry point is UNCLAMPED unless the env gate is set.
        assert_eq!(JIG_VCO_CLAMP_ENV, "DCENT_BM1370_JIG_VCO_CLAMP");
    }

    #[test]
    fn bm1370_pll_register_addresses_match_s21pro_jig() {
        // S21 Pro single_board_test: pllparameter_register_array @ .data
        // 0x001fa848 = {0x08, 0x60, 0x64}; set_pllparameter indexes it
        // directly for which_pll 0..2.
        assert_eq!([regs::PLL0, regs::PLL1, regs::PLL2], [0x08, 0x60, 0x64]);
        assert_eq!(regs::PLL1, 0x60);
        assert_eq!(regs::PLL2, 0x64);
        assert_ne!(regs::PLL2, regs::PLL3);
    }

    #[test]
    fn bm1370_matches_esp_miner_clean_room_reference() {
        // RE 2026-06-02 clean-room cross-check (no live hardware / no Ghidra): pin DCENT's BM1370
        // init values to ESP-Miner's open-source BM1370 driver
        // ("from S21Pro dump"). This moves
        // BM1370 (S21 Pro / S21 XP) from "silicon-unverified" to cross-validated against the
        // independent open reference, and confirms the B9/54/B9/3C tail is BM1370-specific
        // (partially answers RE-ASK-CHIP-BM1370). ESP-Miner writes, register-for-register:
        //   0xA8 -> 00 07 00 00 | 0x18 -> F0 00 C1 00 | 0x58 -> 00 01 11 11 | 0xB9 -> 00 00 44 80
        //   0x54 -> 00 00 00 02 | 0x10 -> 00 00 1E B5 | 0xA4 -> 90 00 .. .. | 0x3C -> 80 00 8B 00 / 80 00 80 0C
        assert_eq!(CHIP_ID, 0x1370); // ESP-Miner BM1370_CHIP_ID 0x1370
        assert_eq!(REG_A8_BCAST, 0x0007_0000); // {0xA8,0x00,0x07,0x00,0x00}
        assert_eq!(REG_A8_PER_CHIP, 0x0007_01F0); // per-chip {0xA8,0x00,0x07,0x01,0xF0}
        assert_eq!(MISC_CTRL_BCAST, 0xF000_C100); // S21Pro {0x18,0xF0,0x00,0xC1,0x00} (NOT S21's 0xFF0FC100)
        assert_eq!(IO_DRIVER_VALUE, 0x0001_1111); // S21Pro {0x58,0x00,0x01,0x11,0x11} (NOT 0x02111111)
        assert_eq!(MISC_SETTINGS_B9_VALUE, 0x0000_4480); // {0xB9,0x00,0x00,0x44,0x80}
        assert_eq!(ANALOG_MUX_VALUE, 0x0000_0002); // {0x54,0x00,0x00,0x00,0x02} (NOT 0x03)
        assert_eq!(HASH_COUNTING_VALUE, 0x0000_1EB5); // {0x10,0x00,0x00,0x1E,0xB5} S21 Pro stock
        assert_eq!(VERSION_MASK_VALUE & 0xFFFF_0000, 0x9000_0000); // {0xA4,0x90,0x00,..}
        assert_eq!(regs::CORE_REG_CTRL, 0x3C); // {0x3C,0x80,0x00,0x8B,0x00} / {..,0x80,0x0C}
        assert_eq!(regs::MISC_SETTINGS_B9, 0xB9);
    }

    #[test]
    fn test_chip_name() {
        let driver = Bm1370Driver::new();
        assert_eq!(driver.chip_name(), "BM1370");
    }

    #[test]
    fn test_cores_per_chip() {
        let driver = Bm1370Driver::new();
        assert_eq!(driver.cores_per_chip(), 1280);
    }

    #[test]
    fn test_response_length() {
        let driver = Bm1370Driver::new();
        assert_eq!(driver.response_length(), 11);
    }

    #[test]
    fn test_ctrl_reg_bm139x_mode() {
        let driver = Bm1370Driver::new();
        let ctrl = driver.ctrl_reg_value();
        // BM139X mode bit (bit 4) should be set.
        assert!(ctrl & fpga_chain::CTRL_BM139X != 0);
        // ENABLE bit (bit 3) should be set.
        assert!(ctrl & fpga_chain::CTRL_ENABLE != 0);
        // MIDSTATE_CNT should be 0 (bits 2:1 = 00).
        assert_eq!((ctrl >> 1) & 0x03, 0);
    }

    #[test]
    fn test_pll_params_500mhz() {
        let driver = Bm1370Driver::new();
        let pll = driver.pll_params(500);
        // 500 MHz = 25 * fbdiv / (refdiv * pd1 * pd2)
        let actual = 25.0 * pll.fb_div as f64
            / (pll.ref_div as f64 * pll.post_div1 as f64 * pll.post_div2 as f64);
        assert!(
            (actual - 500.0).abs() < 10.0,
            "PLL at 500 MHz: actual = {:.2}",
            actual
        );
        // fb_div should be within BM1370 range.
        assert!(pll.fb_div >= FB_DIV_MIN);
        assert!(pll.fb_div <= FB_DIV_MAX);
    }

    #[test]
    fn test_pll_params_400mhz() {
        let driver = Bm1370Driver::new();
        let pll = driver.pll_params(400);
        let actual = 25.0 * pll.fb_div as f64
            / (pll.ref_div as f64 * pll.post_div1 as f64 * pll.post_div2 as f64);
        assert!(
            (actual - 400.0).abs() < 10.0,
            "PLL at 400 MHz: actual = {:.2}",
            actual
        );
    }

    #[test]
    fn test_pll_vdo_scale() {
        // VCO >= 2400 MHz should use 0x50.
        let (vdo, fb, rd, _, _) = compute_pll_params(500.0);
        let vco = 25.0 * fb as f64 / rd as f64;
        if vco >= 2400.0 {
            assert_eq!(vdo, 0x50);
        } else {
            assert_eq!(vdo, 0x40);
        }
    }

    #[test]
    fn test_pll_postdiv_encoding() {
        // Verify the -1 subtract for postdiv encoding.
        let reg = encode_pll_reg(0x40, 200, 1, 2, 1);
        let bytes = reg.to_be_bytes();
        // bytes[3] should be ((2-1) << 4) | (1-1) = 0x10
        assert_eq!(bytes[3], 0x10);
    }

    #[test]
    fn test_ticket_mask() {
        let driver = Bm1370Driver::new();
        assert_eq!(driver.ticket_mask(256), 255);
        assert_eq!(driver.ticket_mask(512), 511);
        assert_eq!(driver.ticket_mask(1), 0);
    }

    #[test]
    fn test_job_interval() {
        let driver = Bm1370Driver::new();
        // 65 chips: 500 / 65 = 7
        assert_eq!(driver.job_interval_ms(65, 500), 7);
        // 1 chip: 500 / 1 = 500
        assert_eq!(driver.job_interval_ms(1, 500), 500);
    }

    #[test]
    fn test_baud_reg() {
        let driver = Bm1370Driver::new();
        // 115200: 200M / (16 * 115200) - 1 = 108.5 -> 107 (0x6B)
        // Close to fpga_chain::BAUD_REG_115200 (0x6C) — integer rounding.
        let baud_reg = driver.baud_reg_value(115_200, fpga_chain::FPGA_CLK_HZ);
        assert!((0x6B..=0x6C).contains(&baud_reg));
        // 1 MHz: 200M / (16 * 1M) - 1 = 11.5 -> 11
        assert_eq!(
            driver.baud_reg_value(1_000_000, fpga_chain::FPGA_CLK_HZ),
            11
        );
    }

    #[test]
    fn pll_register_to_freq_round_trips_known_frequencies() {
        // verify_frequency() reads back PLL0 and decodes via pll_register_to_freq.
        // Pin that the decode is the exact inverse of pll_params().reg_value for
        // every common frequency, and that the PLL lock bit (MSB) is masked off
        // before the lookup. Read-only PLL-lock-verification correctness check.
        let drv = Bm1370Driver::new();
        for &f in MinerProfile::pll_frequencies_for_chip(CHIP_ID) {
            let reg = drv.pll_params(f).reg_value;
            assert_eq!(
                Bm1370Driver::pll_register_to_freq(reg),
                Some(f),
                "bare PLL0 readback 0x{:08X} must decode to {} MHz",
                reg,
                f
            );
            assert_eq!(
                Bm1370Driver::pll_register_to_freq(reg | 0x8000_0000),
                Some(f),
                "locked PLL0 readback for {} MHz must mask bit31 before lookup",
                f
            );
        }
        // Unknown register → None (verify_frequency surfaces an error, not OK).
        assert_eq!(Bm1370Driver::pll_register_to_freq(0x0000_0000), None);
    }

    #[test]
    fn test_decode_nonce_preserves_raw_work_id() {
        let driver = Bm1370Driver::new();
        let raw = [0x0123_4567u32, (0x5Au32 << 8) | 0x11];
        let decoded = driver.decode_nonce(&raw).unwrap();
        assert_eq!(decoded.work_id, 0x5A);
        assert_eq!(decoded.solution_id, 0x11);
    }
}
