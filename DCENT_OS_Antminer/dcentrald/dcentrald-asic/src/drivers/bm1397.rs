//! BM1397 ASIC driver (Antminer S17/T17).
//!
//! The BM1397 is the second-generation driver. Key differences from BM1387:
//!   - 7nm process (vs 16nm)
//!   - 672 cores per chip (vs 114)
//!   - 48 chips per chain on S17 (vs 63 on S9)
//!   - 9-byte nonce response (vs 7-byte on BM1387)
//!   - 4-midstate version rolling support (BM1387 duplicates single midstate)
//!   - Different command framing: uses BM1397+ unified headers (0x51/0x52/0x53)
//!     NOT BM1387's legacy headers (0x58/0x54/0x55)
//!   - PLL0 at register 0x08 (vs 0x0C on BM1387)
//!   - PLL for baud: PLL3 at 0x68 (vs CLKI-derived on BM1387)
//!   - MiscControl at register 0x18 (vs 0x1C on BM1387)
//!   - TicketMask at register 0x14 (vs 0x18 on BM1387)
//!   - open-core: ⚠️ PRE-LIVE FLAG (2026-06-10). DCENT assumes "cores always
//!     active, no open-core" (sourced from ESP-Miner's single-chip BitAxe
//!     context). BUT the Bitmain S17 factory jig DOES run an explicit
//!     `single_BM1397_open_core` right before mining: per-core
//!     `BM1397_enable_core_clock` (×84) + dummy TW work + `OpenCoreGap`.
//!     DCENT does broadcast CoreRegCtrl (0x3C) clock config but SKIPS the
//!     per-core open-core sweep. DCENT_OS has not validated that path on an
//!     S17, so it remains the #1 standalone-execution risk for this driver
//!     (zero/reduced nonces if BM1397 actually needs open-core). A/B the gated
//!     open-core on the first live S17. (Sibling BM1398/S19 Pro DID produce
//!     146K nonces without it — so it is not a hard zero-nonce there, but may be
//!     leaving hashrate on the table via partial core activation.) See
//!     .
//!   - BM139X compatibility mode in FPGA CTRL_REG (bit 4 = 1)
//!   - FB_DIV range: 60-200 (vs 32-128)
//!   - Default baud: 115740 (25MHz / (27*8)) due to integer divider
//!   - Maximum baud: 3.125 MHz on CLKI, higher with PLL3
//!   - Job ID increments by 4 (mod 128), not by 1
//!
//! Register values (reset defaults from ASIC Register Bible):
//!   0x00 ChipAddress:    0x13971800 (ID=0x1397, CORE_NUM=0x18, ADDR=0x00)
//!   0x08 PLL0 Parameter: 0xC0600161 (PLLEN=1, FBDIV=96, REFDIV=1, PD1=6, PD2=1)
//!   0x14 Ticket Mask:    0x00000000
//!   0x18 Misc Control:   0x00003A01 (BT8D=26, BCLK_SEL=1)
//!   0x20 Ordered Clk En: 0x0000FFFF
//!   0x28 Fast UART:      0x0600000F
//!   0x68 PLL3:           0x00700111
//!   0x70 PLL0 Divider:   0x03040607

use crate::drivers::{ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

use super::bm139x;

/// BM1397 chip ID.
pub const CHIP_ID: u16 = 0x1397;

/// BM1397 default chips per chain (S17).
/// S17 has 48 chips per chain (3 chains = 144 chips total).
/// Contrast: BM1387 has 63 chips per chain on S9.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 48;

/// BM1397 response size (2 x 32-bit words from WORK_RX_FIFO).
/// On wire: 9 bytes (AA 55 + 4 nonce + 1 midstate + 1 job_id + 1 flags).
/// The FPGA strips the preamble and delivers 2 words via FIFO.
pub const RESPONSE_WORDS: usize = 2;

/// BM1397 work size with 4 midstates (MIDSTATE_CNT=2 in FPGA):
/// 4 header words + 4 x 8 midstate words = 36 words = 144 bytes.
/// Same FPGA work format as BM1387, but BM1397 uses REAL 4-midstate
/// version rolling (each midstate has a different rolled version).
pub const WORK_WORDS: usize = 36;

/// Number of midstate slots in the FPGA work format.
/// BM1397 supports true 4-midstate version rolling (AsicBoost).
/// Each midstate is computed with a different rolled block version.
const NUM_MIDSTATES: usize = 4;

/// Log2 of NUM_MIDSTATES -- used to shift work_id for FPGA encoding.
/// Pattern: work_id = (counter << MIDSTATE_CNT_LOG2) | midstate_idx
const MIDSTATE_CNT_LOG2: u32 = 2;

/// Number of SHA-256 cores per BM1397 chip.
/// ASIC Register Bible: CORE_NUM=0x18 (24), multiply by 28 = 672 actual cores.
const NUM_CORES_ON_CHIP: u32 = 672;

/// BM1397 register addresses.
///
/// IMPORTANT: These are DIFFERENT from BM1387 register addresses!
/// BM1387: PLL=0x0C, MiscCtrl=0x1C, TicketMask=0x18
/// BM1397: PLL0=0x08, MiscCtrl=0x18, TicketMask=0x14
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 Parameter -- hash clock PLL configuration.
    /// BM1397: register 0x08 (BM1387 uses 0x0C).
    pub const PLL0_PARAMETER: u8 = 0x08;
    /// Ticket Mask register -- hardware difficulty filter.
    /// BM1397: register 0x14 (BM1387 uses 0x18).
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc Control register -- baud rate divider, clock select, GPIO.
    /// BM1397: register 0x18 (BM1387 uses 0x1C).
    pub const MISC_CONTROL: u8 = 0x18;
    /// Ordered Clock Enable register.
    pub const ORDERED_CLOCK_EN: u8 = 0x20;
    /// Fast UART Configuration register.
    pub const FAST_UART_CONFIG: u8 = 0x28;
    /// Core Register Control -- indirect core register access.
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// PLL3 Parameter -- baud rate clock source.
    /// BM1397 uses PLL3 for high-speed baud (BM1387 uses CLKI).
    pub const PLL3_PARAMETER: u8 = 0x68;
    /// PLL0 Divider -- output divider chain for PLL0.
    /// Must be pre-configured before changing PLL0 frequency.
    pub const PLL0_DIVIDER: u8 = 0x70;
    /// Clock Order Control 0 -- maps PLL clocks to core domains (low).
    pub const CLOCK_ORDER_CTRL0: u8 = 0x80;
    /// Clock Order Control 1 -- maps PLL clocks to core domains (high).
    pub const CLOCK_ORDER_CTRL1: u8 = 0x84;
    /// Hash Counting Number -- nonce range / hash counting config.
    pub const HASH_COUNTING_NUM: u8 = 0x10;
    /// IO Driver Strength register.
    pub const IO_DRIVER_STRENGTH: u8 = 0x58;
}

/// BM1397 PLL calculation constants.
/// Formula: f_PLL = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)
const CLKI_MHZ: f64 = 25.0;
const FB_DIV_MIN: u16 = 60;
const FB_DIV_MAX: u16 = 200;

/// Calculate BM1397 PLL0 register value for a target frequency.
///
/// BM1397 PLL register (0x08) bit layout:
///   Bit 31:       LOCKED (read-only, PLL lock indicator)
///   Bit 30:       PLLEN (PLL enable, MUST be 1)
///   Bits [26:16]: FBDIV (feedback divider, 11 bits)
///   Bits [13:8]:  REFDIV (reference divider, 6 bits)
///   Bits [6:4]:   POSTDIV1 (3 bits, must be >= POSTDIV2)
///   Bits [2:0]:   POSTDIV2 (3 bits)
///
/// BM1397 difference from BM1366+: postdiv encoding uses RAW values
/// (not values-1). VDO_SCALE is always 0x40 (not dynamic like BM1366+).
///
/// Returns (reg_value, actual_freq_mhz, fb_div, ref_div, postdiv1, postdiv2).
fn bm1397_pll_calc(target_mhz: u16) -> (u32, u16, u16, u8, u8, u8) {
    let target = target_mhz.clamp(50, 800) as f64;

    let mut best_freq = 0.0f64;
    let mut best_fb: u16 = 96;
    let mut best_ref: u8 = 1;
    let mut best_pd1: u8 = 1;
    let mut best_pd2: u8 = 1;
    let mut best_diff = f64::MAX;

    // Brute-force search for optimal PLL settings (matches ESP-Miner pll_get_parameters).
    // Priority: 1) closest freq, 2) lowest VCO, 3) lowest postdiv product.
    for refdiv in [1u8, 2] {
        for postdiv1 in 1..=7u8 {
            for postdiv2 in 1..=7u8 {
                // Constraint: postdiv1 must be >= postdiv2.
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

    // BM1397 PLL register encoding (NO -1 on postdiv, unlike BM1366+):
    //   Bit 30: PLLEN = 1
    //   Bits [26:16]: FBDIV
    //   Bits [13:8]: REFDIV
    //   Bits [6:4]: POSTDIV1 (raw, NOT postdiv1-1)
    //   Bits [2:0]: POSTDIV2 (raw, NOT postdiv2-1)
    //
    // ESP-Miner BM1397 exception: "postdiv = ((postdiv1 & 0x7) << 4) + (postdiv2 & 0x7)"
    // This confirms BM1397 uses raw postdiv values (no subtraction).
    //
    // BM1397 always uses VDO_SCALE = 0x40 (bit 30 = PLLEN in our encoding).
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

/// Get the sorted list of discrete PLL frequencies the BM1397 can generate (MHz).
///
/// Common S17 operating frequencies. Used by the autotuner for step calculations.
pub fn pll_frequencies() -> &'static [u16] {
    &[
        50, 100, 150, 200, 250, 300, 350, 400, 425, 450, 475, 500, 525, 550, 575, 600, 625, 650,
        700, 750, 800,
    ]
}

// ---------------------------------------------------------------------------
// BM1397 command encoding helpers
// ---------------------------------------------------------------------------
// BM1397 uses the UNIFIED BM13xx+ command format with 0x55 0xAA preamble.
// Header bytes: 0x51 (write all), 0x52 (read all), 0x53 (inactive all),
//               0x40 (set address), 0x41 (write single), 0x42 (read single).
//
// CRITICAL DIFFERENCE from BM1387:
//   BM1387 uses 0x58 (SETCONFIG broadcast), 0x54 (READ broadcast), 0x55 (INACTIVE broadcast).
//   BM1397 uses 0x51 (WRITE broadcast),     0x52 (READ broadcast), 0x53 (INACTIVE broadcast).
//
// The FPGA handles preamble insertion, so we only pack the header + data.
// Wire format: [header, length, chip_addr, reg_addr, value_BE[0..4], CRC5]
// FIFO format: 2 x 32-bit words, LSB-first packed.

use crate::protocol::unpack_lsb_first;

/// Encode a broadcast Write Register command for BM1397 CMD_TX_FIFO.
///
/// Wire format (after FPGA adds preamble 0x55 0xAA):
///   [0x51, 0x09, 0x00, reg, value_BE[0], value_BE[1], value_BE[2], value_BE[3], CRC5]
///
/// BM1397 uses header 0x51 (CMD|BCAST|WRITE=0x01), NOT BM1387's 0x58 (CMD|BCAST|SETCONFIG=0x08).
/// chip_addr = 0x00 for broadcast. Length = 0x09 (9 bytes after preamble).
fn fifo_bm1397_write_reg_bcast(reg: u8, value: u32) -> (u32, u32) {
    bm139x::fifo_write_reg_bcast(reg, value)
}

/// Encode a single-chip Write Register command for BM1397 CMD_TX_FIFO.
///
/// Wire format: [0x41, 0x09, chip_addr, reg, value_BE[0..4], CRC5]
/// BM1397 uses header 0x41 (CMD|SINGLE|WRITE=0x01).
fn fifo_bm1397_write_reg_single(chip_addr: u8, reg: u8, value: u32) -> (u32, u32) {
    bm139x::fifo_write_reg_single(chip_addr, reg, value)
}

/// Encode a Chain Inactive broadcast command for BM1397 CMD_TX_FIFO.
///
/// Wire format: [0x53, 0x05, 0x00, 0x00, CRC5]
/// BM1397 uses CMD_INACTIVE=0x03 (header 0x53), NOT BM1387's CMD_INACTIVE=0x05 (header 0x55).
const FIFO_BM1397_CHAIN_INACTIVE: u32 = bm139x::CHAIN_INACTIVE_CMD;

/// Encode a Read Register broadcast command for BM1397 CMD_TX_FIFO.
///
/// Wire format: [0x52, 0x05, 0x00, reg, CRC5]
/// BM1397 uses CMD_READ=0x02 (header 0x52), NOT BM1387's CMD_READ=0x04 (header 0x54).
fn fifo_bm1397_read_reg_bcast(reg: u8) -> u32 {
    bm139x::fifo_read_reg_bcast(reg)
}

/// Encode a Set Chip Address command for BM1397 CMD_TX_FIFO.
///
/// Wire format: [0x40, 0x05, addr, 0x00, CRC5]
/// BM1397 uses CMD_SETADDRESS=0x00 (header 0x40), NOT BM1387's CMD_SETADDR=0x01 (header 0x41).
fn fifo_bm1397_set_address(addr: u8) -> u32 {
    bm139x::fifo_set_address(addr)
}

/// Encode a single-chip Read Register command for BM1397 CMD_TX_FIFO.
///
/// Wire format: [0x42, 0x05, chip_addr, reg, CRC5]
fn fifo_bm1397_read_reg_single(chip_addr: u8, reg: u8) -> u32 {
    bm139x::fifo_read_reg_single(chip_addr, reg)
}

/// BM1397 driver implementation.
pub struct Bm1397Driver;

impl Default for Bm1397Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1397Driver {
    pub fn new() -> Self {
        Self
    }

    /// Calculate WORK_TIME register value for a given frequency and midstate count.
    ///
    /// Same formula as BM1387 (shared FPGA). The FPGA work_time counter runs at
    /// 100 MHz. Work interval = 0.9 * midstate_count * 2^19 / freq_Hz * 100MHz.
    pub fn calculate_work_time(freq_mhz: u16, midstate_count: u32) -> u32 {
        bm139x::calculate_work_time(freq_mhz, midstate_count)
    }

    fn read_pll_register(chain: &mut FpgaChain, chip_addr: u8) -> Result<Option<u32>> {
        bm139x::read_pll_register(chain, chip_addr, regs::PLL0_PARAMETER)
    }

    fn pll_register_to_freq(raw_reg: u32) -> Option<u16> {
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        let masked = raw_reg & !PLL_LOCK_BIT;
        MinerProfile::pll_frequencies_for_chip(CHIP_ID)
            .iter()
            .copied()
            .find(|&freq| Bm1397Driver::new().pll_params(freq).reg_value == masked)
    }
}

impl ChipDriver for Bm1397Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1397"
    }

    fn cores_per_chip(&self) -> u32 {
        // ASIC Register Bible: CORE_NUM=0x18 (24 domains), 28 small cores each = 672.
        672
    }

    fn response_length(&self) -> usize {
        // 9 bytes on wire: [AA 55] [nonce:4] [midstate_num:1] [job_id:1] [flags:1]
        // FPGA delivers 2 x 32-bit words via WORK_RX_FIFO.
        9
    }

    fn default_baud(&self) -> u32 {
        // BM1397 default: 25MHz / ((26+1)*8) = 115,741 bps.
        // Slightly different from BM1387's 115200 due to integer division.
        115_740
    }

    fn max_baud(&self) -> u32 {
        // On CLKI: 25MHz / ((0+1)*8) = 3,125,000.
        // Higher rates possible via PLL3.
        3_125_000
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1397: Configuring {} chips at {} MHz (enumeration already complete)",
            chip_count,
            freq_mhz,
        );

        // --- Step 0: Hot-start baud reset (same logic as BM1387) ---
        // If the FPGA is not at 115200 baud, ASICs may be at a fast baud from
        // prior firmware. Send MiscCtrl at the current baud to reset ASICs to
        // default baud, then switch FPGA to match.
        let current_baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);
        if current_baud_div != fpga_chain::BAUD_REG_115200 {
            // BM1397 MiscControl: BT8D=26 (0x1A), BCLK_SEL=1 -> 115740 baud.
            // Value 0x00007A31: BT8D_4_0 = 11010 (26), TFS=00, BCLK_SEL=1.
            const MISC_CTRL_DEFAULT_BAUD: u32 = 0x0000_7A31;
            let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_DEFAULT_BAUD);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(10));
            tracing::info!(
                chain_id = chain.chain_id,
                current_baud_div = format_args!("0x{:02X}", current_baud_div),
                "Hot start baud reset: sent MiscCtrl(BT8D=26) at current baud -- \
                 ASICs switching back to 115740 baud",
            );
        }

        // --- Step 1: Set FPGA baud to 115200 for configuration commands ---
        chain.set_baud(fpga_chain::BAUD_REG_115200);
        tracing::debug!("FPGA baud set to 115200 (BAUD_REG=0x6C)");

        // --- Step 2: Clock Order Control 0 and 1 = 0x00000000 ---
        // From ESP-Miner bm1397.c init sequence (Steps 5-6).
        // Zeroes all clock domain assignments before PLL configuration.
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::CLOCK_ORDER_CTRL0, 0x0000_0000);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::CLOCK_ORDER_CTRL1, 0x0000_0000);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Clock Order Control 0/1 zeroed");

        // --- Step 3: Ordered Clock Enable = 0x00000001 ---
        // From ESP-Miner Step 7. Enable only clock domain 0 initially.
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::ORDERED_CLOCK_EN, 0x0000_0001);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Ordered Clock Enable = 0x00000001");

        // --- Step 4: Core Register Control = 0x80008074 ---
        // From ESP-Miner Step 8. Enables AsicBoost and sets tuning parameters.
        // Bit 31 = RD#_WR = 1 (write), core_reg_id and value configure core hashing.
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::CORE_REG_CTRL, 0x8000_8074);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Core Register Control = 0x80008074 (AsicBoost enable)");

        // --- Step 5: PLL3 Parameter = 0xC0700111 ---
        // Configure PLL3 for baud rate clock source.
        // PLLEN=1, FBDIV=112 (0x70), REFDIV=1, POSTDIV1=1, POSTDIV2=1.
        // f_PLL3 = 25 * 112 / 1 = 2800 MHz.
        // JIG-VERIFIED 2026-06-10 (S17 factory jig `set_baud_ext`): writes reg
        // 0x68 = 0xC0700111 + reg 0x28 = 0x0600000F byte-exact — confirms this
        // ESP-Miner-sourced value is factory-correct (the "0xC0660011 / FBDIV=0x66"
        // config-grep lead was WRONG, refuted first-hand). BM139x family = PLL3/0x68.
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::PLL3_PARAMETER, 0xC070_0111);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("PLL3 = 0xC0700111 (2800 MHz for baud clock)");

        // --- Step 6: Fast UART Configuration = 0x0600000F ---
        // PLL3_DIV4 = 6 -> f_base = 2800 / (6+1) = 400 MHz.
        // Flags = 0x0F.
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::FAST_UART_CONFIG, 0x0600_000F);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Fast UART Config = 0x0600000F (PLL3_DIV4=6)");

        // --- Step 7: Set Ticket Mask (difficulty filter) ---
        // ESP-Miner Step 9. Set before frequency ramp.
        let difficulty = 256;
        let mask = self.ticket_mask(difficulty);
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::TICKET_MASK, mask);
        tracing::info!(
            reg = format_args!("0x{:02X}", regs::TICKET_MASK),
            mask = format_args!("0x{:08X}", mask),
            "TicketMask write -- difficulty {} (only 1 in {} hashes reported)",
            difficulty,
            difficulty,
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);

        // --- Step 8: Set default baud via MiscControl = 0x00007A31 ---
        // BT8D=26, BCLK_SEL=1 -> 115740 baud (default).
        // From ESP-Miner Step 12.
        const MISC_CTRL_DEFAULT: u32 = 0x0000_7A31;
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_DEFAULT);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("MiscControl = 0x00007A31 (BT8D=26, 115740 baud)");

        // --- Step 9: Frequency ramp via PLL0 ---
        // BM1397 requires PLL0 Divider pre-configuration before PLL0 Parameter change.
        // PLL0 Divider (0x70) = 0x0F0F0F00 sets all PLLDIV to max to prevent glitches.
        // Both PLL0 Divider and PLL0 Parameter are sent TWICE with 10ms delays.
        //
        // The S17 factory jig ramps in 25 MHz steps from 50 MHz. This
        // Experimental driver still sets the target directly with the proven
        // pre-frequency glitch protection; the complete bounded ramp belongs
        // to the exact BHB07601 composition plan.

        // Step 9a: Pre-configure PLL0 Divider (send twice).
        const PLL0_DIV_PRECONFIG: u32 = 0x0F0F_0F00;
        for attempt in 0..2u8 {
            let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(10));
            if attempt == 0 {
                tracing::debug!("PLL0 Divider pre-config = 0x0F0F0F00 (glitch protection, 1/2)");
            }
        }

        // Step 9b: Set PLL0 Parameter (send twice).
        let pll = self.pll_params(freq_mhz);
        for attempt in 0..2u8 {
            let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(10));
            if attempt == 0 {
                tracing::info!(
                    pll_reg = format_args!("0x{:08X}", pll.reg_value),
                    freq_mhz = freq_mhz,
                    fb_div = pll.fb_div,
                    ref_div = pll.ref_div,
                    post_div1 = pll.post_div1,
                    post_div2 = pll.post_div2,
                    "PLL0 write (1/2) -- all chips switching to {} MHz",
                    freq_mhz,
                );
            }
        }

        // Wait for PLL to lock (~10ms typical, 20ms to be safe).
        std::thread::sleep(std::time::Duration::from_millis(20));

        // PLL readback verification: read PLL0 from chip 0 to confirm lock.
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        for pll_retry in 0..3u8 {
            // Drain stale CMD RX responses.
            while chain.cmd_rx_has_data() {
                let _ = chain.read_cmd_response();
            }
            let pll_read_cmd = fifo_bm1397_read_reg_single(0x00, regs::PLL0_PARAMETER);
            chain.write_cmd(pll_read_cmd);
            std::thread::sleep(std::time::Duration::from_millis(50));

            let pll_readback = if chain.cmd_rx_has_data() {
                let r0 = chain.read_cmd_response().unwrap_or_default();
                let _r1 = chain.read_cmd_response();
                let bytes = unpack_lsb_first(r0);
                Some(u32::from_be_bytes(bytes))
            } else {
                None
            };

            match pll_readback {
                Some(val) if (val & !PLL_LOCK_BIT) == pll.reg_value => {
                    let locked = val & PLL_LOCK_BIT != 0;
                    tracing::info!(
                        chain_id = chain.chain_id,
                        readback = format_args!("0x{:08X}", val),
                        pll_locked = locked,
                        "PLL0 readback VERIFIED -- chip 0 at {} MHz, PLL_LOCKED={} (attempt {})",
                        freq_mhz,
                        locked,
                        pll_retry + 1,
                    );
                    break;
                }
                Some(val) => {
                    tracing::warn!(
                        chain_id = chain.chain_id,
                        expected = format_args!("0x{:08X}", pll.reg_value),
                        got = format_args!("0x{:08X}", val),
                        "PLL0 readback MISMATCH (attempt {}/3)",
                        pll_retry + 1,
                    );
                    if pll_retry < 2 {
                        let (w0, w1) =
                            fifo_bm1397_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
                        chain.write_cmd(w0);
                        chain.write_cmd(w1);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
                None => {
                    tracing::warn!(
                        chain_id = chain.chain_id,
                        "PLL0 readback TIMEOUT -- chip 0 did not respond (attempt {}/3)",
                        pll_retry + 1,
                    );
                    if pll_retry < 2 {
                        let (w0, w1) =
                            fifo_bm1397_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
                        chain.write_cmd(w0);
                        chain.write_cmd(w1);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }

        // --- Step 10: Set WORK_TIME in FPGA ---
        let work_time = Bm1397Driver::calculate_work_time(freq_mhz, NUM_MIDSTATES as u32);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            work_time = format_args!("0x{:08X}", work_time),
            freq_mhz = freq_mhz,
            "WORK_TIME set to 0x{:08X} ({} MHz, {} midstates)",
            work_time,
            freq_mhz,
            NUM_MIDSTATES,
        );

        // --- Step 11: Baud upgrade to 3.125 MHz ---
        // MiscControl with BT8D=0, BCLK_SEL=1 -> 25MHz / ((0+1)*8) = 3,125,000 baud.
        // Value 0x00006031: BT8D_4_0=0, TFS=01 (UART_RX), bit4=1, BCLK_SEL=1.
        // From ESP-Miner/ASIC Register Bible.
        const MISC_CTRL_FAST_BAUD: u32 = 0x0000_6031;
        let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_FAST_BAUD);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!("MiscControl = 0x00006031 (BT8D=0, 3.125 MHz baud)");

        // Switch FPGA baud to match. 200MHz / (16 * 3.125M) = 4 -> divisor = 3.
        // But the s9io FPGA uses: baud_div = 200M / (16 * baud) - 1.
        // 200M / (16 * 3125000) = 4.0 - 1 = 3.
        const BAUD_REG_3_125M: u32 = 0x03;
        chain.set_baud(BAUD_REG_3_125M);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!("FPGA baud set to 3,125,000 (BAUD_REG=0x03)");

        // --- Step 12: MiscControl readback verification ---
        while chain.cmd_rx_has_data() {
            let _ = chain.read_cmd_response();
        }
        let readback_cmd = fifo_bm1397_read_reg_single(0x00, regs::MISC_CONTROL);
        chain.write_cmd(readback_cmd);
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some((_w0, _w1)) = chain.read_nonce() {
            // Wrong FIFO -- try CMD RX below.
        }
        let readback = if chain.cmd_rx_has_data() {
            let r0 = chain.read_cmd_response().unwrap_or_default();
            let _r1 = chain.read_cmd_response();
            let bytes = unpack_lsb_first(r0);
            Some(u32::from_be_bytes(bytes))
        } else {
            None
        };

        match readback {
            Some(val) => {
                tracing::info!(
                    chain_id = chain.chain_id,
                    readback = format_args!("0x{:08X}", val),
                    "READBACK: MiscCtrl from chip 0 = 0x{:08X}. {}",
                    val,
                    if val == MISC_CTRL_FAST_BAUD {
                        "MATCH -- ASIC received writes!"
                    } else if val == 0x00003A01 {
                        "DEFAULT -- ASIC did NOT receive writes"
                    } else {
                        "UNEXPECTED -- check baud mismatch"
                    },
                );
            }
            None => {
                tracing::warn!(
                    chain_id = chain.chain_id,
                    "READBACK: MiscCtrl TIMEOUT -- chip 0 did not respond at 3.125M baud",
                );
            }
        }

        // Reset WORK_TX and WORK_RX FIFOs for clean mining start.
        chain.work_tx.write_reg(fpga_chain::REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(2));
        chain
            .work_tx
            .write_reg(fpga_chain::REG_WORK_TX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);
        chain.work_rx.write_reg(fpga_chain::REG_WORK_RX_CTRL, 0x01); // RST_RX
        std::thread::sleep(std::time::Duration::from_millis(1));
        chain
            .work_rx
            .write_reg(fpga_chain::REG_WORK_RX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1397: Chain configuration complete -- {} chips at {} MHz, 3.125M baud",
            chip_count,
            freq_mhz,
        );

        Ok(())
    }

    fn set_frequency(&self, chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        let pll = self.pll_params(freq_mhz);

        tracing::info!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz,
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            "BM1397: Setting frequency"
        );

        // BM1397 requires PLL0 Divider pre-configuration before PLL0 change.
        // Send PLL0 Divider (0x70) = 0x0F0F0F00 twice, then PLL0 Parameter twice.
        const PLL0_DIV_PRECONFIG: u32 = 0x0F0F_0F00;

        if chip_addr == 0xFF {
            // Broadcast
            for _ in 0..2 {
                let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for _ in 0..2 {
                let (w0, w1) = fifo_bm1397_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        } else {
            // Single chip
            for _ in 0..2 {
                let (w0, w1) =
                    fifo_bm1397_write_reg_single(chip_addr, regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for _ in 0..2 {
                let (w0, w1) =
                    fifo_bm1397_write_reg_single(chip_addr, regs::PLL0_PARAMETER, pll.reg_value);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        // Wait for PLL to lock (~20ms).
        std::thread::sleep(std::time::Duration::from_millis(20));
        tracing::debug!(
            "PLL0 lock wait complete (20ms) -- SHA-256 cores now at {} MHz",
            freq_mhz
        );

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
                    "BM1397 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1397 PLL0 readback timed out for chip 0x{:02X}",
                    target_addr
                ),
            }),
        }
    }

    fn set_voltage(&self, pic: &mut PicController, voltage_mv: u16) -> Result<()> {
        // NOTE: This voltage formula is for PIC16F1704 (S9) only.
        // dsPIC33EP (S17/S19) uses DspicController::set_voltage(mv) with direct millivolt values.
        // NoPic (S21) uses kernel DTB-managed TAS5782M DACs.
        // The daemon routes voltage commands based on MinerProfile.pic_type, so this
        // function is only called for Pic16F1704 boards.
        let pic_value = PicController::voltage_to_pic(voltage_mv as f64 / 1000.0);
        pic.set_voltage(pic_value)?;
        Ok(())
    }

    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u16> {
        // BM1397 work format with runtime MIDSTATE_CNT via WORK_TX_FIFO:
        //   Word 0:      Extended Work ID, shifted left by the active MIDSTATE_CNT.
        //                FPGA uses the low bits for the midstate index.
        //   Word 1:      nbits (32-bit LE)
        //   Word 2:      ntime (32-bit LE)
        //   Word 3:      merkle_tail (last 4 bytes of merkle root, LE)
        //   Words 4+:    midstate slots (8 words each, reversed word order)
        //
        // BM1397 DIFFERENCE from BM1387:
        // BM1397 supports TRUE multi-midstate version rolling. Each midstate slot
        // contains a different midstate computed with a different rolled block
        // version. The ASIC cycles through them and reports which midstate
        // produced the nonce via the midstate_num field in the response.
        //
        // If only 1 midstate is provided (no version rolling), we duplicate it
        // into all 4 slots (same behavior as BM1387).

        if work.midstates.is_empty() {
            return Err(crate::AsicError::InvalidParameter(
                "no midstates provided".into(),
            ));
        }

        let ms_cnt = (work.fpga_midstate_cnt as u32).clamp(2, 3);
        let num_slots = 1usize << ms_cnt;
        let work_words = 4 + num_slots * 8;
        let mut words = [0u32; 68];

        // Word 0: Extended Work ID, shifted by the FPGA's active MIDSTATE_CNT.
        words[0] = (work.work_id as u32) << ms_cnt;

        // Word 1: nbits.
        words[1] = work.nbits;

        // Word 2: ntime.
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root).
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Encode midstates in REVERSED word order for FPGA.
        // Same encoding as BM1387 -- see bm1387.rs send_work() for detailed proof
        // of why we use u32::from_be_bytes() WITHOUT .swap_bytes().
        for slot in 0..num_slots {
            // Select which midstate to use for this slot.
            // If we have enough rolled midstates for every active slot, use them.
            // Otherwise duplicate midstate 0 into the remaining slots.
            let ms_idx = if slot < work.midstates.len() { slot } else { 0 };
            let midstate = &work.midstates[ms_idx];

            let base = 4 + slot * 8;
            for i in 0..8 {
                let word_idx = 7 - i; // Reversed word order.
                words[base + i] = u32::from_be_bytes([
                    midstate[word_idx * 4],
                    midstate[word_idx * 4 + 1],
                    midstate[word_idx * 4 + 2],
                    midstate[word_idx * 4 + 3],
                ]); // NO .swap_bytes() -- proven correct for accepted shares.
            }
        }

        // DIAGNOSTIC: Log first work item for byte-level comparison.
        use std::sync::atomic::{AtomicBool, Ordering as AOrdering};
        static FIRST_WORK_LOGGED: AtomicBool = AtomicBool::new(false);
        if !FIRST_WORK_LOGGED.swap(true, AOrdering::Relaxed) {
            tracing::info!(
                chain_id = chain.chain_id,
                work_id = work.work_id,
                num_midstates = work.midstates.len(),
                midstate_cnt = ms_cnt,
                "WORK_TX_DIAG: First BM1397 work item -- {} FIFO words, {} midstate(s), shift={}",
                work_words,
                work.midstates.len(),
                ms_cnt,
            );
            tracing::info!("WORK_TX[0] work_id_shifted = 0x{:08X}", words[0]);
            tracing::info!("WORK_TX[1] nbits  = 0x{:08X}", words[1]);
            tracing::info!("WORK_TX[2] ntime  = 0x{:08X}", words[2]);
            tracing::info!("WORK_TX[3] merkle4 = 0x{:08X}", words[3]);
            for i in 0..8 {
                tracing::info!(
                    "WORK_TX[{}] midstate0[{}] = 0x{:08X}",
                    4 + i,
                    i,
                    words[4 + i]
                );
            }
        }

        // Write to WORK TX FIFO.
        chain.write_work(&words[..work_words]);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1397 nonce response (from WORK_RX_FIFO):
        //   Word 0: nonce value (32-bit)
        //   Word 1: [CRC:8 | extended_work_id:16 | solution_index:8]
        //
        // Same FIFO word layout as BM1387 (shared FPGA delivers same format).
        //
        // BM1397 NONCE DECODING DIFFERENCE from BM1387:
        //   BM1387: chip_index = (nonce >> 2) & 0x3F  (nonce bits [7:2], 6 bits)
        //   BM1397: chip_addr  = (nonce >> 17) & 0xFF  (nonce bits [24:17], 8 bits)
        //           chip_index = chip_addr / address_interval
        //
        // The address_interval = 256 / chip_count. For 48 chips: interval = 5.
        // We return chip_addr raw; the caller divides by address_interval.
        //
        // RESOLVED 2026-06-10 (goldmine ranks-40-50, live Ghidra bridge):
        // `gChain_Asic_Interval` is a config-driven address STRIDE set in the S17 jig's
        // `read_config@183E4` per hashboard type (`.bss` global @ 0x00230f84). For the
        // BM1397 board (BHB07601, AsicType 0x1397 = 5015, 672 cores) it is hardcoded to 5
        // for a 48-ASIC chain; other boards: BHB91601 (BM1391) = 3, BHB91603 = 2. The jig
        // nonce decode `BHB07601_check_nonce@20DC4` computes
        // `which_asic = (uint8)(buf[1] >> 14) / gChain_Asic_Interval` — confirming DCENT's
        // existing `nonce_word2 >> 14` field. The stride is simply `256 / chips_per_chain`
        // (48 -> 5, matching the hardcode). Per-model BM1397 chip counts (operator-confirmed
        // 2026-06-10): T17 = 30/chain -> interval 8, T17+ = 45 -> 5, S17 jig BHB07601 = 48
        // -> 5, S17+ = 65 -> 3. So DCENT's `address_interval = 256 / chip_count` formula is
        // CORRECT and chip-count-general — the on-binary constant is just the 48-chip
        // instance, NOT a hidden permutation. No behavior change; gap closed.
        // Source: goldmine `deliverables/RANKS_40_50_DESK_RE.md` (rank 49 / C02).
        //
        // BM1397 job_id extraction: byte7 & 0xFC (upper 6 bits = job_id).
        // Small core ID: byte7 & 0x03 (lower 2 bits).
        // Midstate num: byte6 (which of 4 midstates produced the nonce).

        let nonce = raw[0];
        let w1 = raw[1];
        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
        let work_id = hw_work_id >> MIDSTATE_CNT_LOG2;
        // Midstate slot index: low 2 bits of hw_work_id.
        let midstate_idx = (hw_work_id & ((1 << MIDSTATE_CNT_LOG2) - 1)) as u8;

        // BM1397 chip address in nonce bits [24:17] (8 bits).
        // Contrast: BM1387 uses nonce bits [7:2] (6 bits).
        let chip_addr = ((nonce >> 17) & 0xFF) as u8;

        Ok(NonceResult {
            nonce,
            chip_index: chip_addr, // Raw chip address; caller divides by interval.
            work_id,
            solution_id,
            midstate_idx,
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        // Same FPGA formula as BM1387: divisor = fpga_clk / (16 * baud) - 1.
        (fpga_clock_hz / (16 * target_baud)) - 1
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM139X mode: bit4=1 (BM139X compatibility), bit3=1 (ENABLE),
        // bits2:1=10 (MIDSTATE_CNT=2 -> 4 midstates).
        //
        // CRITICAL DIFFERENCE from BM1387:
        //   BM1387: CTRL = 0x0C (ENABLE + MIDSTATE_CNT=2, bit4=0)
        //   BM1397: CTRL = 0x1C (ENABLE + MIDSTATE_CNT=2, bit4=1 BM139X mode)
        //
        // The BM139X bit tells the FPGA to use the BM1397+ response parser
        // (9-byte responses vs 7-byte for BM1387).
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE | (2 << fpga_chain::CTRL_MIDSTATE_SHIFT)
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // FIFO-driven dispatch (1ms poll), same as BM1387.
        // The FPGA consumes work every WORK_TIME ticks.
        1
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // The S17 factory jig `BM1397_set_TM@26DA4` applies its canonical
        // 8-bit bit-reversal table independently to every byte of
        // `difficulty - 1` before writing register 0x14. The u32 operation
        // below is exactly that byte-wise transform: reverse all bits, then
        // restore byte order. ESP-Miner's BM1397 implementation agrees.
        difficulty.saturating_sub(1).reverse_bits().swap_bytes()
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // BM1397 PLL0 at register 0x08.
        // Formula: f = 25 MHz * FBDIV / (REFDIV * POSTDIV1 * POSTDIV2)
        //
        // CRITICAL DIFFERENCE from BM1387:
        //   BM1387: PLL at 0x0C, lookup table, different bit layout.
        //   BM1397: PLL0 at 0x08, computed PLL with brute-force search,
        //           POSTDIV encoding uses RAW values (no -1 subtraction).
        //
        // CRITICAL DIFFERENCE from BM1366+:
        //   BM1366+: postdiv = ((pd1-1)<<4) | (pd2-1), VDO_SCALE dynamic.
        //   BM1397:  postdiv = (pd1<<4) | pd2 (raw), VDO_SCALE always 0x40.

        let (reg_value, actual_freq, fb_div, ref_div, post_div1, post_div2) =
            bm1397_pll_calc(freq_mhz);

        if actual_freq != freq_mhz {
            tracing::debug!(
                target = freq_mhz,
                actual = actual_freq,
                "BM1397 PLL: requested {} MHz, closest achievable is {} MHz",
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

#[cfg(test)]
mod ranks_40_50_desk_re_tests {
    //! Goldmine ranks-40-50 (rank 49 / C02), 2026-06-10 — verified desk-RE of the
    //! S17 `single-board-test` `.data` via the live Ghidra bridge. Closes the two
    //! `.data GAP` markers that lived in `ticket_mask` and `decode_nonce`.
    use super::*;

    /// The 256 bytes read verbatim out of `bit_swap_table @ 0x00030b3c` in
    /// .
    /// `BM1397_set_TM@26DA4` indexes this LUT for every TICKET_MASK byte.
    /// This array is the extraction's provenance anchor; the test below proves it
    /// is the canonical 8-bit bit-reversal LUT, so any transcription error here
    /// fails the build rather than silently corrupting the record.
    #[rustfmt::skip]
    const S17_JIG_BIT_SWAP_TABLE: [u8; 256] = [
        0,128,64,192,32,160,96,224,16,144,80,208,48,176,112,240,
        8,136,72,200,40,168,104,232,24,152,88,216,56,184,120,248,
        4,132,68,196,36,164,100,228,20,148,84,212,52,180,116,244,
        12,140,76,204,44,172,108,236,28,156,92,220,60,188,124,252,
        2,130,66,194,34,162,98,226,18,146,82,210,50,178,114,242,
        10,138,74,202,42,170,106,234,26,154,90,218,58,186,122,250,
        6,134,70,198,38,166,102,230,22,150,86,214,54,182,118,246,
        14,142,78,206,46,174,110,238,30,158,94,222,62,190,126,254,
        1,129,65,193,33,161,97,225,17,145,81,209,49,177,113,241,
        9,137,73,201,41,169,105,233,25,153,89,217,57,185,121,249,
        5,133,69,197,37,165,101,229,21,149,85,213,53,181,117,245,
        13,141,77,205,45,173,109,237,29,157,93,221,61,189,125,253,
        3,131,67,195,35,163,99,227,19,147,83,211,51,179,115,243,
        11,139,75,203,43,171,107,235,27,155,91,219,59,187,123,251,
        7,135,71,199,39,167,103,231,23,151,87,215,55,183,119,247,
        15,143,79,207,47,175,111,239,31,159,95,223,63,191,127,255,
    ];

    /// Rank 49 — the extracted `bit_swap_table` IS exactly `u8::reverse_bits` for
    /// all 256 entries. Self-checking: a mistyped literal above fails this test.
    #[test]
    fn bm1397_bit_swap_table_is_bit_reversal() {
        for i in 0u16..256 {
            let b = i as u8;
            assert_eq!(
                S17_JIG_BIT_SWAP_TABLE[i as usize],
                b.reverse_bits(),
                "bit_swap_table[{i}] must equal reverse_bits({b})"
            );
        }
        // Spot anchors actually read off `.data` during extraction.
        assert_eq!(S17_JIG_BIT_SWAP_TABLE[0], 0x00);
        assert_eq!(S17_JIG_BIT_SWAP_TABLE[1], 0x80);
        assert_eq!(S17_JIG_BIT_SWAP_TABLE[0x3F], 0xFC); // diff-64 mask byte
        assert_eq!(S17_JIG_BIT_SWAP_TABLE[0xFF], 0xFF);
    }

    /// Rank 49 — the default init TICKET_MASK (diff 256) is invariant under
    /// raw-vs-bit-reversed encoding, while a non-byte-boundary difficulty pins
    /// the vendor's byte-wise reversal.
    #[test]
    fn bm1397_ticket_mask_diff256_encoding_invariant() {
        let drv = Bm1397Driver;
        let encoded = drv.ticket_mask(256);
        assert_eq!(encoded, 0x0000_00FF, "vendor encoding for diff 256");
        // Bit-reversed form of the same value is identical at 256.
        assert_eq!(
            256u32.saturating_sub(1).reverse_bits().swap_bytes(),
            0x0000_00FF
        );
        // They diverge only off byte boundaries (documented, not exercised live):
        assert_eq!(64u32.saturating_sub(1), 0x3F);
        assert_eq!(64u32.saturating_sub(1).reverse_bits().swap_bytes(), 0xFC);
        assert_eq!(drv.ticket_mask(64), 0xFC);
    }

    /// Rank 49 — `gChain_Asic_Interval` is `256 / chips_per_chain`. The S17 jig
    /// hardcodes 5 for the 48-ASIC BHB07601 (BM1397) board (`read_config@183E4`).
    /// Per-model BM1397 chip counts (operator-confirmed 2026-06-10) all satisfy it.
    #[test]
    fn bm1397_address_interval_matches_jig_and_models() {
        let interval = |chips: u32| 256 / chips;
        assert_eq!(interval(48), 5); // S17 jig BHB07601 hardcode @ read_config@183E4
        assert_eq!(interval(30), 8); // Antminer T17
        assert_eq!(interval(45), 5); // Antminer T17+
        assert_eq!(interval(65), 3); // Antminer S17+
                                     // All addresses (chip_idx * interval) stay within the 8-bit nonce field.
        for &chips in &[30u32, 45, 48, 65] {
            let max_addr = (chips - 1) * interval(chips);
            assert!(max_addr <= 0xFF, "{chips} chips overflow 8-bit address");
        }
    }
}
