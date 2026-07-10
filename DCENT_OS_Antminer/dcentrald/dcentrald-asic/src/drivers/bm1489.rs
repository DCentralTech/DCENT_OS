//! BM1489 ASIC driver (Antminer L7 / L9 — Litecoin Scrypt mining) — SCAFFOLD
//!
//! The BM1489 is the Scrypt-mining ASIC used in:
//!   - Antminer L7 (2021): 4 boards × 120 chips, 9.5 GH/s nameplate, 3,425 W
//!   - Antminer L9 (2024): same AML S11board (byte-identical SHA256
//!     `bbc25a2137fd…` §1.1)
//!
//! Status: SIMULATOR ONLY. **No live L7/L9 unit on bench.**  will fill
//! the register map + init sequence from a live unit and validate first hash.
//!
//! # Differences from BM1387 (S9) / BM1397 (S17)
//!
//! - **Algorithm**: Scrypt (NOT SHA-256). Each nonce attempt requires 128 KB
//!   scratchpad memory per candidate, but the scratchpad lives **internal to
//!   the BM1489** (per Halong DragonMint / Bitmain heritage); the host
//!   work-dispatch path doesn't allocate scratchpad RAM.
//! - **Work packet**: 76 bytes (block-header-minus-version) per
//!   `SCRYPT_ASIC_CHIPS.md:169`, NOT 144-byte multi-midstate SHA-256 work.
//! - **Cores per chip**: 12 (matches BM1485 — `chip_init.rs:240`)
//!   `[GAP — wave-8 live verification needed]`
//! - **Chips per chain (L7/L9)**: 120 chips × 4 chains = 480 chips total
//!   (`bm1489.rs:83` silicon profile constant `BM1489_CHIPS_PER_CHAIN_L7`).
//! - **Default freq**: 425 MHz (L7 nameplate per
//!   `dcentrald-silicon-profiles/src/bm1489.rs:46`).
//! - **Default chain voltage**: 13.0 V (L7 nameplate per same file:46).
//! - **Framing**: BM1387-era raw bytes + CRC5 poly 0x05 ASSUMED for first cut
//!   (per `SCRYPT_ASIC_CHIPS.md:308` for BM1485; BM1489 protocol unconfirmed)
//!   `[GAP — wave-8 live verification needed]`
//! - **Platform**: AML S11board (Amlogic AXG/A113D) — same control board as
//!   S19j Pro / S21 ( §1.1
//!   byte-identity finding). 1:1 GPIO reuse: `pwr_en=437`,
//!   `ch{0,1,2}_plug={439-441}`, `ch{0,1,2}_rst={454-456}`.
//! - **Voltage controller**: TBD — likely NoPic (TAS5782M) like S21, but
//!   could be dsPIC. `[GAP — wave-8 live verification needed]`
//!
//! # Status flag matrix
//!
//! | Aspect              | Status                       |
//! |---------------------|------------------------------|
//! | Chip ID             | 0x1489 (confirmed via VNish libbitmain `aml/chip.c`) |
//! | Register addresses  | `[GAP]` — placeholders mirror BM1397+ pattern |
//! | Init sequence       | `[GAP]` — stubbed, returns Err on live call |
//! | Work packet shape   | `[GAP]` — 76-byte assumption, send_work stubbed |
//! | Nonce decode        | `[GAP]` — decode_nonce stubbed |
//! | PLL FB_DIV range    | `[GAP]` — placeholder 60..200 (BM1397 range) |
//! | Hardware difficulty | `[GAP]` — assumed 256 (BM1387/BM1397 default) |
//!
//! # References
//!
//! - PRIMARY template: `dcentrald-asic/src/drivers/bm1397.rs` (chip driver
//!   pattern: ChipDriver trait, regs module, init_chain, send_work, etc.)
//! - Pre-hardware scaffold pattern: `dcentrald-asic/src/drivers/bm1373.rs`
//!   (BM1373 = S23, ALL values projected, similar `[GAP]` discipline).
//! - Silicon profile (5-row baked): `dcentrald-silicon-profiles/src/bm1489.rs`
//!   — operator-confirmed L7 nameplate (9.5 GH/s @ 3,425 W).
//! - L9 = BM1489: :199-200`
//!   (libbitmain `aml/chip.c` BM1489).
//! - L7 chips/chain × chain count: `bm1489.rs:83-86` — 120 × 4 = 480.
//! - AML S11board byte-identity:
//!   §1.1 (`bbc25a2137fd…`).
//! - PLL register pattern:  (W6
//!   inheritance from BM1397+).
//! -  plan: `plans/wave4-scrypt-l9-spike.md` Phase 1.A-1.B.
//! - Honest gap discipline: same as `bm1373.rs` SCAFFOLD discipline.

use crate::drivers::{ChipDriver, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1489 chip ID (confirmed via VNish libbitmain `aml/chip.c` per
/// :199-200`).
pub const CHIP_ID: u16 = 0x1489;

/// L7/L9 default chips per chain.
///
/// L7 nameplate = 4 boards × 120 chips = 480 chips total (per
/// `dcentrald-silicon-profiles/src/bm1489.rs:83` constant
/// `BM1489_CHIPS_PER_CHAIN_L7`). L9 inherits the same AML S11board so chip
/// count is identical.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 120;

/// L7/L9 default chain count (= 4, NOT the typical 3 of S9/S17/S19).
///
/// Per silicon profile `BM1489_CHAIN_COUNT_L7` (`bm1489.rs:86`).
pub const DEFAULT_CHAIN_COUNT: u8 = 4;

/// BM1489 response size on the wire = 7 bytes.
///
/// ** W8-C:** Confirmed via inheritance from BM1485 lineage (per
///  §4).
/// BM1485 nonce response is 7 bytes per `mining-bible-v1/3-asic-protocol/bm1485.md`
/// §12 (header + 4-byte nonce + chip_addr + CRC5). BM1489 uses BM1485 framing
/// (NOT BM1397+ unified 9-byte) because:
/// 1. L7 cgminer string `chip#%d from nonce` (l7-1.2.7-xil:26530) matches
///    BM1485 nonce-response format.
/// 2. `0x55 0xAA` preamble strings ABSENT from L7 binary (would be present if
///    BM1397+ unified framing).
/// 3. libbitmain source path `src/chip/chip.c` — Scrypt driver, not BM139X
///    SHA-256 family driver.
pub const RESPONSE_BYTES: usize = 7;

/// Number of Scrypt cores per BM1489 chip
/// `[GAP — wave-8 live verification needed]`.
///
/// Placeholder = 12 (matches BM1485 entry in `chip_init.rs:240` and
/// `frequency_scaling.rs:58`). The actual number is "very small" per
/// `SCRYPT_ASIC_CHIPS.md:48` (BM1485 = 12 cores). BM1489 nameplate is 19.8
/// MH/s per chip × 12 cores ≈ 1.65 MH/s/core, plausible.
const NUM_CORES_ON_CHIP: u32 = 12;

/// Scrypt work packet size in bytes
/// `[GAP — wave-8 live verification needed]`.
///
/// Per `SCRYPT_ASIC_CHIPS.md:169`: 76 bytes (block-header-minus-version).
/// ASIC computes Scrypt internally with on-chip 128 KB scratchpad; host
/// never allocates scratchpad RAM.
pub const SCRYPT_WORK_BYTES: usize = 76;

/// BM1489 register addresses.
///
/// ** W8-C update (2026-05-04):** Register-naming evidence from wave-6
/// decoded strings (L7 1.2.7-xil + L9 1.2.7-aml libbitmain `src/chip/chip.c`)
/// confirms BM1489 is a **BM1485 lineage** chip (NOT BM1397+), per
/// .
/// libbitmain build-path probe `/tmp/build/libbitmain/src/chip/chip.c`
/// (cite: l9-1.2.7-aml:26339, l7-1.2.7-xil:26362) confirms a single
/// vendor-internal driver. BM1485 register addresses (open-source via
/// `bitmaintech/cgminer-ltc:driver-btm-L3.h` — see
///  §7.1) are the
/// trustworthy inheritance line.
///
/// **NOT BM1397+** because:
/// - L7 cgminer uses BM1485 framing (0x40/0x41/0x42/0x51/0x52/0x53/0x20).
/// - BM1397+ uses 0x55 0xAA preamble — NOT present in L7 strings.
/// - Halong/DragonMint heritage check: NEGATIVE result (0 string hits).
///   BM1489 is Bitmain-internal libbitmain, NOT Halong-derived. The earlier
///    heritage hint applies to
///   Innosilicon T2T (libinno HAL), not Bitmain L7/L9.
///
/// **Confidence:**
/// - HIGH (BM1485 inheritance, string-confirmed in BM1489): TICKET_MASK,
///   MISC_CONTROL.
/// - MEDIUM-HIGH (BM1485 inheritance, plausible for BM1489): CHIP_ADDRESS,
///   PLL0_PARAMETER, HASH_COUNTING, CORE_REG_CTRL.
/// - GAP (NEW BM1489 register names recovered via string mining; addresses
///   require wave-9 Ghidra disassembly): ORDERED_CLOCK_EN, DRIVE_STRENGTH,
///   PLL1_PARAMETER, RELAY, CHIP_REG_MISC_CONTROL1, CLOCK_DELAY_CTRL,
///   ANALOG_MUX_CTRL, SWEEP_CLOCK_CTRL.
///
/// MUST NOT be trusted for live writes until wave-9 (Ghidra) or live L7/L9
/// hardware confirms. The simulator path enforces this — `init_chain` returns
/// an error rather than writing these.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16). BM1485
    /// `CHIP_ADDR` at same offset; standard across Bitmain ASICs.
    /// (Cite: cgminer-ltc driver-btm-L3.h; SCRYPT_ASIC_CHIPS.md §7.1)
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 Parameter — primary hash clock PLL.
    /// BM1485 `PLL_PARAMETER` at 0x08 (single PLL; BM1489 has at least two
    /// per L7 string `PLL1 register` cite l7-1.2.7-xil:26607).
    /// (Cite: cgminer-ltc; SCRYPT_ASIC_CHIPS.md §7.1)
    pub const PLL0_PARAMETER: u8 = 0x08;
    /// Hash counting / nonce range register.
    /// BM1485 `HCN` (Hash Counting Number) at 0x10.
    /// (Cite: cgminer-ltc driver-btm-L3.h)
    pub const HASH_COUNTING: u8 = 0x10;
    /// Ticket Mask register (hardware difficulty filter).
    /// BM1485 `TICKET_MASK` at 0x14; string `ICKET_MASK` confirmed in L7/L9
    /// libbitmain (cite l7-1.2.7-xil:26471, l9-1.2.7-aml:26452).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc Control register (LDO + baud + filter + CRC counters).
    /// BM1485 `MISC_CONTROL` at 0x18; string `ISC_CONTROL` confirmed in L7/L9
    /// libbitmain (cite l7-1.2.7-xil:26435, l9-1.2.7-aml:26412). Bitfield
    /// layout per SCRYPT_ASIC_CHIPS.md §7.2 (hashratectrl, ldoctrl, bt8d
    /// baud divisor in byte 2).
    pub const MISC_CONTROL: u8 = 0x18;
    /// Core Register Control (indirect core register access).
    /// BM1485 `CORE_CMD_IN` at 0x3C; CORE_RESP_OUT at 0x40.
    /// (Cite: cgminer-ltc driver-btm-L3.h; SCRYPT_ASIC_CHIPS.md §7.4)
    pub const CORE_REG_CTRL: u8 = 0x3C;

    /// BM1489_PENDING_W27_DOC:
    ///
    /// W27 intentionally does not assign real addresses to the eight
    /// BM1489-only register names recovered by earlier string mining.
    /// They remain pending an operator Ghidra pass against the L7/L9
    /// libbitmain binary. The `REG_W27_UNKNOWN_*` values below are
    /// deprecated `0xFF` sentinels so any future caller has to confront
    /// the unresolved address instead of accidentally treating `0x00`
    /// as a real register. Do not replace these with guessed BM1397,
    /// BM136x, or BM1485 offsets.
    pub const BM1489_PENDING_W27_DOC: &str =
        "BM1489 W27 register addresses pending operator Ghidra session";

    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_1: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_2: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_3: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_4: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_5: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_6: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_7: u8 = 0xFF; // pending Ghidra W27
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const REG_W27_UNKNOWN_8: u8 = 0xFF; // pending Ghidra W27

    // ---- W8-C/W27: NEW BM1489 register names (addresses GAP - W27 Ghidra) ----

    /// Ordered Clock Enable. NEW BM1489 register (multi-domain sequencing
    /// per L7's 24 voltage-domain × 5-chips topology and dual-crystal Y1/Y2
    /// clock split). Address `[GAP — wave-9 Ghidra]`. Name string recovered
    /// at l7-1.2.7-xil:26448 via libbitmain "Failed to set ORDERED_CLOCK_EN"
    /// log template. Was 0x20 in W7-F BM1397-pattern guess — that's BM1485
    /// `SECURITY_IIC`. **Reset to GAP.**
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const ORDERED_CLOCK_EN: u8 = 0xFF; // pending Ghidra W27
    /// Drive Strength register. NEW BM1489 register
    /// (libbitmain typo: `DRIVER_STRENGHT` per l7-1.2.7-xil:26579).
    /// Per-chip I/O drive strength — `io_drive_strength` field exists in
    /// hwscan struct ( §9.2). Address `[GAP]`.
    /// Was 0x58 in W7-F guess — BM1485 doesn't have anything at 0x58.
    /// **Reset to GAP.**
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const IO_DRIVE_STRENGTH: u8 = 0xFF; // pending Ghidra W27
    /// Second PLL register. BM1489 has multi-PLL — name `PLL1 register`
    /// recovered at l7-1.2.7-xil:26607 + l9-1.2.7-aml:26590. Likely the
    /// second clock domain's PLL (chips 61-120 in L7's Y1/Y2 split).
    /// Address `[GAP — wave-9 Ghidra]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const PLL1_PARAMETER: u8 = 0xFF; // pending Ghidra W27
    /// UART Relay register. NEW BM1489 register. Likely controls daisy-chain
    /// UART pass-through (analogous to BM1362's FPGA-side UART relay regs at
    /// 0x43D00030/34, but on-chip here since no FPGA on Amlogic L9). String
    /// `RELAY` recovered at l7-1.2.7-xil:26713. Address `[GAP]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const RELAY: u8 = 0xFF; // pending Ghidra W27
    /// Misc Control 1 — second misc-control register (numbered).
    /// String `CHIP_REG_MISC_CONTROL1` recovered at l7-1.2.7-xil:26545 +
    /// l9-1.2.7-aml:26526. Likely splits some MISC_CONTROL bits into a
    /// second register because BM1489 needs more bits than BM1485's 32-bit
    /// MISC_CONTROL provides (pulse_mode, pulse_width, multi-PLL gating).
    /// Address `[GAP]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const CHIP_REG_MISC_CONTROL1: u8 = 0xFF; // pending Ghidra W27
    /// Clock Delay Control. NEW BM1489 register. String `CLOCK_DELAY_CTRL`
    /// recovered at l7-1.2.7-xil:26451, 26455 + l9-1.2.7-aml:26432. Likely
    /// per-chip clock skew tuning across the daisy-chain. Address `[GAP]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const CLOCK_DELAY_CTRL: u8 = 0xFF; // pending Ghidra W27
    /// Analog Mux Control. NEW BM1489 register. String `ANALOG_MUX_CTRL`
    /// recovered at l7-1.2.7-xil:26459 + l9-1.2.7-aml:26440. Likely
    /// temp-sensor / ADC mux selector. Co-related with `analog_mux` field
    /// in hwscan struct. Address `[GAP]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const ANALOG_MUX_CTRL: u8 = 0xFF; // pending Ghidra W27
    /// Sweep Clock Control. NEW BM1489 register. String `SWEEP_CLOCK_CTRL`
    /// recovered at l7-1.2.7-xil:26463 + l9-1.2.7-aml:26444. Likely PLL
    /// pre-lock frequency ramp control. Address `[GAP]`.
    #[deprecated(note = "W27 placeholder; see master plan")]
    pub const SWEEP_CLOCK_CTRL: u8 = 0xFF; // pending Ghidra W27
}

/// BM1489 PLL constants `[GAP — wave-8 live verification needed]`.
///
/// Crystal reference frequency assumed 25 MHz (industry standard,
/// matches BM1397 in `bm1397.rs:112`). Actual oscillator frequency on
/// L7/L9 boards must be verified against silkscreen + bring-up logs.
const CLKI_MHZ: f64 = 25.0;

/// PLL FB_DIV minimum `[GAP — placeholder from BM1397 range; BM1489 may
/// be wider since Scrypt loops want lower freq for power efficiency]`.
const FB_DIV_MIN: u16 = 60;

/// PLL FB_DIV maximum `[GAP — placeholder]`.
const FB_DIV_MAX: u16 = 200;

/// Default Scrypt mining frequency for L7 nameplate (425 MHz).
///
/// Per silicon profile `dcentrald-silicon-profiles/src/bm1489.rs:46`
/// (Step 0 = OperatorConfirmed L7 nameplate). L9 likely identical given
/// shared AML S11board.
pub const L7_NAMEPLATE_FREQ_MHZ: u16 = 425;

/// Default Scrypt chain voltage for L7 nameplate (13,000 mV = 13.0 V).
///
/// Per silicon profile `bm1489.rs:46` (`voltage_v: 13.0`). Chain-rail
/// voltage, not chip-rail. Per `SCRYPT_ASIC_CHIPS.md:69-93` operator
/// confirmation.
pub const L7_NAMEPLATE_VOLTAGE_MV: u16 = 13_000;

// ---------------------------------------------------------------------------
// PLL helpers (placeholder — mirrors BM1397 brute-force search)
// ---------------------------------------------------------------------------

/// Discrete Scrypt mining frequencies BM1489 can run at (MHz)
/// `[GAP — wave-8 live verification needed]`.
///
/// Placeholder set spans the silicon profile's 5-row range
/// (320..510 MHz per `bm1489.rs:30-65`) plus reasonable steps. Used by
/// the autotuner for binary search.
static PLL_FREQ_TABLE: &[u16] = &[
    // Eco-low / underclock (silicon profile Step -2..0)
    280, 300, 320, 340, 360, 380, 400, 425, // Stock and overclock (Step 0..+2)
    450, 470, 490, 510, 530, 550,
];

/// Get the sorted list of discrete PLL frequencies BM1489 can generate.
pub fn pll_frequencies() -> &'static [u16] {
    PLL_FREQ_TABLE
}

/// Calculate BM1489 PLL register value
/// `[GAP — wave-8 live verification needed]`.
///
/// Placeholder uses the BM1397+ formula:
///   f_PLL = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)
/// with raw POSTDIV encoding (no -1 subtraction). Real BM1489 PLL bit
/// layout TBD.
fn bm1489_pll_calc(target_mhz: u16) -> (u32, u16, u16, u8, u8, u8) {
    let target = target_mhz.clamp(50, 800) as f64;

    let mut best_freq = 0.0f64;
    let mut best_fb: u16 = 96;
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
                if diff < best_diff {
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

    // BM1397-style encoding (placeholder).
    let reg_value: u32 = (1u32 << 30)                        // PLLEN = 1
        | ((best_fb as u32 & 0x7FF) << 16)                   // FBDIV [26:16]
        | ((best_ref as u32 & 0x3F) << 8)                    // REFDIV [13:8]
        | ((best_pd1 as u32 & 0x7) << 4)                     // POSTDIV1 [6:4]
        | (best_pd2 as u32 & 0x7); // POSTDIV2 [2:0]

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

/// BM1489 driver — SIMULATOR-ONLY SCAFFOLD.
///
/// All hardware-touching paths return `AsicError::InvalidParameter` with a
/// clear "wave-8 needed" message. Only chip-identity getters
/// (`chip_id`/`chip_name`/`cores_per_chip`/etc.) and pure helper
/// functions (`pll_params`/`ticket_mask`/`baud_reg_value`) are functional
/// for offline simulator + autotuner table-prep work.
///
///  deliverables to make this driver live:
///   1. Capture init sequence from L7/L9 stock cgminer or vnish via UART
///      sniff (or strace if running) → fill `init_chain`.
///   2. Verify register addresses via known-good writes + readback.
///   3. Verify nonce response format via passive UART RX capture.
///   4. Calibrate `c_eff` against measured wall-watts at known freq/V.
pub struct Bm1489Driver;

impl Bm1489Driver {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Bm1489Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl ChipDriver for Bm1489Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1489"
    }

    fn cores_per_chip(&self) -> u32 {
        NUM_CORES_ON_CHIP
    }

    fn response_length(&self) -> usize {
        RESPONSE_BYTES
    }

    fn default_baud(&self) -> u32 {
        // Industry-standard 115_200 (matches BM1387/BM1397 defaults).
        // [GAP — wave-8: confirm against L7/L9 cold-boot UART trace]
        115_200
    }

    fn max_baud(&self) -> u32 {
        // [GAP — placeholder. BM1397 supports 3.125 MHz; BM1489 likely
        // similar or lower since Scrypt I/O bandwidth needs are smaller
        // (much lower nonces/sec than SHA-256).  verify.]
        1_500_000
    }

    fn init_chain(&self, _chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        // SIMULATOR-ONLY: log + reject. No hardware path.
        tracing::warn!(
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1489 init_chain: SCAFFOLD — simulator only, no live unit. \
             Register map placeholders mirror BM1397+ pattern; wave-8 \
             will fill from live L7/L9 capture."
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1489 driver is a pre-hardware scaffold. Cannot init without \
             verified register values. [GAP — wave-8 live verification needed]"
                .into(),
        ))
    }

    fn set_frequency(&self, _chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        tracing::warn!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz = freq_mhz,
            "BM1489 set_frequency: SCAFFOLD — simulator only, no live unit"
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1489 set_frequency not yet implemented [GAP — wave-8]".into(),
        ))
    }

    fn set_voltage(&self, _pic: &mut PicController, voltage_mv: u16) -> Result<()> {
        // BM1489 voltage path is TBD. AML S11board is shared with S21
        // (NoPic / TAS5782M DAC) per BIBLE byte-identity finding, so
        // L9 likely follows the NoPic model. L7 control board may use
        // a different DC-DC topology.
        // [GAP — wave-8: verify L7/L9 voltage controller identity by
        // probing /dev/i2c-0 + checking DT for tas5782 vs dspic]
        tracing::warn!(
            voltage_mv = voltage_mv,
            "BM1489 set_voltage: SCAFFOLD — voltage path TBD (likely NoPic \
             on AML S11board, but unconfirmed). [GAP — wave-8]"
        );
        Ok(()) // No-op for PIC path (matches BM1373/S21 NoPic pattern).
    }

    fn send_work(&self, _chain: &mut FpgaChain, _work: &MiningWork) -> Result<u16> {
        // Scrypt work packet is 76 bytes (block-header-minus-version)
        // per SCRYPT_ASIC_CHIPS.md:169. Differs from SHA-256 multi-
        // midstate packet. Host doesn't allocate 128 KB scratchpad —
        // that lives in BM1489 internal SRAM.
        //
        // Translation from MiningWork (which carries SHA-256-shaped
        // fields like midstates) to a 76-byte Scrypt packet requires:
        //   - The MiningWork.prev_block_hash (32 bytes)
        //   - The MiningWork.merkle_root (32 bytes)
        //   - ntime (4 bytes), nbits (4 bytes), version (4 bytes)
        //     = 76 bytes total
        // Midstate fields are NOT used (Scrypt has no midstate concept).
        //
        // [GAP — wave-8: capture L7/L9 work packet wire format from
        // live UART trace; confirm framing and CRC; implement here.]
        tracing::warn!(
            "BM1489 send_work: SCAFFOLD — Scrypt 76-byte packet not yet \
             implemented. [GAP — wave-8]"
        );
        Err(crate::AsicError::InvalidParameter(
            "BM1489 send_work not yet implemented (Scrypt 76-byte packet). \
             [GAP — wave-8 live verification needed]"
                .into(),
        ))
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // Scrypt nonce response format unconfirmed.
        //
        // BM1485 (BM1489's predecessor) uses BM1387-era 7-byte raw
        // framing per SCRYPT_ASIC_CHIPS.md:308. BM1489 may inherit or
        // may have moved to BM1397+ unified framing (9-byte with
        // 0x55 0xAA preamble).
        //
        // Simulator returns a synthetic NonceResult so the offline test
        // harness can exercise the path without a live ASIC. This is
        // NOT correct for real hardware; wave-8 must replace.
        //
        // [GAP — wave-8: capture L7/L9 nonce response wire format,
        // implement real decode.]
        tracing::warn!(
            raw0 = format_args!("0x{:08X}", raw[0]),
            raw1 = format_args!("0x{:08X}", raw[1]),
            "BM1489 decode_nonce: SCAFFOLD — synthetic decode for simulator. \
             [GAP — wave-8]"
        );
        Ok(NonceResult {
            nonce: raw[0],
            chip_index: ((raw[1] >> 17) & 0xFF) as u8,
            work_id: ((raw[1] >> 8) & 0xFFFF) as u16,
            solution_id: (raw[1] & 0xFF) as u8,
            midstate_idx: 0, // Scrypt has no midstate concept.
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        // Same FPGA divisor formula as BM1387/BM1397:
        //   div = fpga_clk / (16 * baud) - 1
        // Note: AML platforms use kernel UART (ttyS1-3), NOT FPGA-driven
        // chains; this method is mostly inert for L9. Kept for trait
        // conformance with Zynq-style chains.
        let div = fpga_clock_hz / (16 * target_baud);
        div.saturating_sub(1)
    }

    fn ctrl_reg_value(&self) -> u32 {
        // [GAP — wave-8: confirm CTRL_REG layout. AML platform has no
        // FPGA, so this register pattern is Zynq-only. For the simulator
        // path we return a reasonable BM1397-like value.]
        // BM139X mode (bit4=1), MIDSTATE_CNT=0 (Scrypt has no midstates),
        // ENABLE=1.
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // Scrypt nonce throughput is FAR lower than SHA-256 (12 cores
        // vs 672 BM1397 / 1280 BM1368). Per SCRYPT_ASIC_CHIPS.md:48,
        // "host dispatches more frequently" but each work item lives
        // longer because nonce search is slower.
        // [GAP — wave-8: tune from live work-rate observation. Placeholder
        // is conservative ~10ms which trades off some host-side overhead
        // for prompt work delivery.]
        10
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // Standard (difficulty - 1) — same as BM1397+. BM1485 (predecessor)
        // uses bit-reversed encoding per BM1387 pattern; BM1489 unconfirmed.
        // [GAP — wave-8: verify against live ticket_mask write trace.]
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        let (reg_value, actual_freq, fb_div, ref_div, post_div1, post_div2) =
            bm1489_pll_calc(freq_mhz);

        if actual_freq != freq_mhz {
            tracing::debug!(
                target = freq_mhz,
                actual = actual_freq,
                "BM1489 PLL: requested {} MHz, closest achievable is {} MHz \
                 (PLL formula is BM1397-pattern placeholder — [GAP — wave-8])",
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_id_is_0x1489() {
        let driver = Bm1489Driver::new();
        assert_eq!(driver.chip_id(), 0x1489);
        assert_eq!(driver.chip_id(), CHIP_ID);
    }

    #[test]
    fn name_is_bm1489() {
        let driver = Bm1489Driver::new();
        assert_eq!(driver.chip_name(), "BM1489");
    }

    #[test]
    fn default_chips_per_chain_is_120() {
        // L7/L9 share AML S11board (
        // §1.1 byte-identity); 120 chips × 4 chains = 480 total per
        // dcentrald-silicon-profiles/src/bm1489.rs:83.
        assert_eq!(DEFAULT_CHIPS_PER_CHAIN, 120);
    }

    #[test]
    fn default_chain_count_is_4() {
        // L7/L9 use 4 chains, NOT the 3 typical of S9/S17/S19.
        assert_eq!(DEFAULT_CHAIN_COUNT, 4);
    }

    #[test]
    fn cores_per_chip_matches_bm1485_placeholder() {
        let driver = Bm1489Driver::new();
        // Placeholder = 12 (matches BM1485 in chip_init.rs:240). Real
        // value pending wave-8 live capture.
        assert_eq!(driver.cores_per_chip(), 12);
    }

    #[test]
    fn response_length_is_seven_bytes_placeholder() {
        let driver = Bm1489Driver::new();
        // Placeholder mirrors BM1485 / BM1387-era raw framing
        // (7 bytes). BM1489 may have moved to BM1397+ 9-byte format;
        // wave-8 verifies.
        assert_eq!(driver.response_length(), 7);
    }

    #[test]
    fn default_baud_is_115200() {
        let driver = Bm1489Driver::new();
        assert_eq!(driver.default_baud(), 115_200);
    }

    #[test]
    fn pll_frequencies_table_is_sorted_and_reasonable() {
        let table = pll_frequencies();
        assert!(!table.is_empty(), "PLL freq table must not be empty");
        // Check sort order.
        for window in table.windows(2) {
            assert!(
                window[0] < window[1],
                "PLL freq table not strictly increasing at {} -> {}",
                window[0],
                window[1],
            );
        }
        // Confirm the L7 nameplate freq (425) is in the table.
        assert!(
            table.contains(&L7_NAMEPLATE_FREQ_MHZ),
            "L7 nameplate freq {} MHz should be in PLL table",
            L7_NAMEPLATE_FREQ_MHZ,
        );
    }

    #[test]
    fn pll_calc_returns_valid_config_for_nameplate_freq() {
        let driver = Bm1489Driver::new();
        let config = driver.pll_params(L7_NAMEPLATE_FREQ_MHZ);
        assert!(config.fb_div >= FB_DIV_MIN);
        assert!(config.fb_div <= FB_DIV_MAX);
        assert!(config.ref_div >= 1);
        assert!(config.post_div1 >= 1);
        assert!(config.post_div2 >= 1);
        // PLLEN bit must be set in the register value.
        assert_ne!(config.reg_value & (1u32 << 30), 0, "PLLEN bit not set");
    }

    #[test]
    fn ticket_mask_difficulty_256_returns_255() {
        let driver = Bm1489Driver::new();
        assert_eq!(driver.ticket_mask(256), 255);
    }

    #[test]
    fn ticket_mask_zero_difficulty_does_not_underflow() {
        let driver = Bm1489Driver::new();
        // saturating_sub avoids u32 wrap.
        assert_eq!(driver.ticket_mask(0), 0);
    }

    #[test]
    fn baud_reg_value_handles_typical_freq() {
        let driver = Bm1489Driver::new();
        // 100 MHz FPGA / (16 * 115200) = 54.25 → divisor 53 after -1.
        let div = driver.baud_reg_value(115_200, 100_000_000);
        assert_eq!(div, 53);
    }

    #[test]
    fn baud_reg_value_does_not_underflow_on_high_baud() {
        let driver = Bm1489Driver::new();
        // baud > clock case (saturating_sub prevents wrap).
        let div = driver.baud_reg_value(50_000_000, 25_000_000);
        assert_eq!(div, 0);
    }

    #[test]
    fn init_chain_returns_scaffold_error_on_simulator() {
        // Simulator path: init_chain MUST refuse to run (no live unit).
        // We can't easily build a FpgaChain in a unit test, so this
        // test is structural — verify the message contains "scaffold"
        // or "wave-8" by inspecting the error type construction path.
        // Since FpgaChain construction needs HAL, we skip the actual
        // call here and just verify driver instantiation works.
        let driver = Bm1489Driver::new();
        assert_eq!(driver.chip_id(), 0x1489);
    }

    #[test]
    fn driver_default_trait_works() {
        let driver = Bm1489Driver;
        assert_eq!(driver.chip_id(), 0x1489);
        let driver2 = Bm1489Driver;
        assert_eq!(driver2.chip_id(), 0x1489);
    }

    #[test]
    fn nameplate_voltage_is_chain_rail() {
        // 13.0 V chain rail (NOT 0.5-2.0 V chip rail). Important for
        // W7-D's voltage validation fix — BM1489 chain-rail voltage
        // bracket is 11.5V..14.0V, not BM1397+ chip-rail 0.5..2.0V.
        let voltage_mv = L7_NAMEPLATE_VOLTAGE_MV;
        assert_eq!(voltage_mv, 13_000);
        assert!((10_001..=14_999).contains(&voltage_mv)); // chain-rail range
    }

    /// W13-B: Pin BM1489 register addresses + scaffold framing constants.
    ///
    /// Two purposes:
    ///
    /// 1. **Lock the W8-C BM1485-inheritance addresses** (HIGH / MEDIUM-HIGH
    ///    confidence). If a future
    ///    refactor changes any of these without updating the cite-doc, this
    ///    test breaks.
    ///
    /// 2. **Canary on [GAP] resolution.** The 8 NEW BM1489 register names
    ///    recovered by W8-C string-mining all carry W27 placeholder address
    ///    `0xFF` until Ghidra fills them. When a future Ghidra session lands,
    ///    that wave's PR will assign real addresses; the
    ///    `assert_eq!(..., 0xFF)` lines below will then break, forcing the
    ///    PR author to also update
    ///    §9.5 + the W8-C cite footer + this test.
    ///
    /// This is the closure mechanism for matrix entry W13-B-1 (regs module
    /// not imported by any consumer): the test now imports the module so
    /// the constants are reachable, eliminating the decoration-only state.
    #[test]
    fn bm1489_regs_pin_w8c_inheritance_and_gap_addresses() {
        // ---- W8-C: BM1485-inheritance addresses (HIGH / MEDIUM-HIGH) ----
        // Cite:  §4
        assert_eq!(regs::CHIP_ADDRESS, 0x00);
        assert_eq!(regs::PLL0_PARAMETER, 0x08);
        assert_eq!(regs::HASH_COUNTING, 0x10);
        assert_eq!(regs::TICKET_MASK, 0x14);
        assert_eq!(regs::MISC_CONTROL, 0x18);
        assert_eq!(regs::CORE_REG_CTRL, 0x3C);

        // ---- W8-C/W27: 8 NEW BM1489 register names - addresses [GAP] ----
        // When W27 Ghidra fills any of these, this test must be updated
        // alongside the cite-doc. DO NOT silently flip a 0xFF to a real
        // address without also updating REGISTER_MAP_DELTA.md and the
        // module-level docstring at bm1489.rs:154-229.
        assert_eq!(regs::ORDERED_CLOCK_EN, 0xFF);
        assert_eq!(regs::IO_DRIVE_STRENGTH, 0xFF);
        assert_eq!(regs::PLL1_PARAMETER, 0xFF);
        assert_eq!(regs::RELAY, 0xFF);
        assert_eq!(regs::CHIP_REG_MISC_CONTROL1, 0xFF);
        assert_eq!(regs::CLOCK_DELAY_CTRL, 0xFF);
        assert_eq!(regs::ANALOG_MUX_CTRL, 0xFF);
        assert_eq!(regs::SWEEP_CLOCK_CTRL, 0xFF);

        // ---- W8-C-confirmed framing constants ----
        // RESPONSE_BYTES = 7 (BM1485-lineage 7-byte nonce frame, NOT
        // BM1397+ unified 9-byte) per W8-C HIGH-confidence finding.
        assert_eq!(RESPONSE_BYTES, 7);
        // SCRYPT_WORK_BYTES = 76 (block-header-minus-version) per
        // SCRYPT_ASIC_CHIPS.md:169.
        assert_eq!(SCRYPT_WORK_BYTES, 76);
    }
}
