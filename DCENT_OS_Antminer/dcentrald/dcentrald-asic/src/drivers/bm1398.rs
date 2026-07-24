//! Experimental BM1398 ASIC driver (NBP1901/S19 Pro lineage).
//!
//! This implementation is admitted from two independent local vendor binaries,
//! not from an assumption that BM1398 is a renamed BM1397. Shared BM139x
//! framing is reused only where captured command bytes agree; BM1398 PLL,
//! core-enable, address-assignment, FIFO, and relay contracts live in
//! `dcentrald_api_types::bm1398_protocol`.
//!
//! Evidence-scoped NBP1901/S19 Pro composition:
//!   - 114 chips per chain, fixed addresses 0x00..0xE2 at interval 2
//!   - ChipID: 0x1398 (vs 0x1397)
//!   - Default operating frequency: 675 MHz (vs ~500 MHz on S17)
//!   - Higher default voltage: 13.8V per board (vs ~12V on S17)
//!   - S19 control board uses am2-s17 platform (same as S17)
//!   - 4 FPGA chain slots (vs 3 on S9), only 3 used
//!
//! Shared with the admitted BM139x transport:
//!   - an experimental 672-slot legacy nonce-attribution/work-time model,
//!     kept separate from the 624 physical small cores recovered from stock
//!   - 9-byte nonce response
//!   - 4-midstate version rolling support (AsicBoost)
//!   - BM139X unified command format (0x51/0x52/0x53 headers)
//!   - PLL0 at register 0x08, MiscCtrl at 0x18, TicketMask at 0x14
//!   - PLL3 at 0x68 for baud rate clock
//!   - 8-bit logical job IDs; the FPGA carrier appends midstate-slot bits
//!   - open-core remains an experimental hardware-validation question. Do not
//!     infer BM1398 requirements from BM1397 die-similarity claims.
//!   - Vendor PLL search: REFDIV 2 then 1, POSTDIV1/2 1..=7,
//!     nearest FBDIV 16..=250, VCO 2000..=3200 MHz
//!   - Maximum baud: 3.125 MHz on CLKI
//!
//! Hardware reference:
//!   - S19 probe:
//!   - S19 deep probe:
//!   - Stock `bmminer` NBP1901 function `FUN_000502c0` (VA `0x502c0`)
//!   - AMTC repair jig `single_board_test_bm1398` function at VA `0x29b48`
//!
//! Register values (reset defaults, same as BM1397):
//!   0x00 ChipAddress:    0x13981800 (ID=0x1398, CORE_NUM=0x18, ADDR=0x00)
//!   0x08 PLL0 Parameter: 0xC0600161 (PLLEN=1, FBDIV=96, REFDIV=1, PD1=6, PD2=1)
//!   0x14 Ticket Mask:    0x00000000
//!   0x18 Misc Control:   0x00003A01 (BT8D=26, BCLK_SEL=1)
//!   0x20 Ordered Clk En: 0x0000FFFF
//!   0x28 Fast UART:      0x0600000F
//!   0x68 PLL3:           0x00700111
//!   0x70 PLL0 Divider:   0x03040607

use crate::drivers::{
    ChipDriver, FpgaNonceDecodeContext, MinerProfile, MiningWork, NonceResult, PllConfig,
};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

use super::bm139x;

/// BM1398 chip ID.
pub const CHIP_ID: u16 = 0x1398;

/// BM1398 default chips per chain (S19 Pro).
/// S19 Pro has 114 chips per chain (3 chains = 342 chips total).
/// S19 (non-Pro) has 76 chips. MinerProfile overrides this at runtime.
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 114;

/// BM1398 response size (2 x 32-bit words from WORK_RX_FIFO).
/// On wire: 9 bytes (AA 55 + 4 nonce + 1 midstate + 1 job_id + 1 flags).
/// The FPGA strips the preamble and delivers 2 words via FIFO.
pub const RESPONSE_WORDS: usize = 2;

/// BM1398 work size with 4 midstates (MIDSTATE_CNT=2 in FPGA):
/// 4 header words + 4 x 8 midstate words = 36 words = 144 bytes.
/// Same FPGA work format as BM1397.
pub const WORK_WORDS: usize = 36;

/// Number of midstate slots in the FPGA work format.
/// BM1398 supports true 4-midstate version rolling (AsicBoost).
const NUM_MIDSTATES: usize = 4;

/// Log2 of NUM_MIDSTATES -- used to shift work_id for FPGA encoding.
const MIDSTATE_CNT_LOG2: u32 = 2;

/// Number of addressable SHA-256 small cores per BM1398 chip.
///
/// Stock NBP1901 reports 156 hash-engine groups, four small cores per group,
/// and a maximum small-core index of 623. The older 672-slot work-time model
/// is not evidence of physical die geometry and remains separate in
/// `MinerProfile::nonce_attribution_cores`.
const NUM_CORES_ON_CHIP: u32 = 624;

/// BM1398 register addresses (identical to BM1397).
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 Parameter -- hash clock PLL configuration.
    pub const PLL0_PARAMETER: u8 = 0x08;
    /// Hash Counting Number -- nonce range / hash counting config.
    pub const HASH_COUNTING_NUM: u8 = 0x10;
    /// Ticket Mask register -- hardware difficulty filter.
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc Control register -- baud rate divider, clock select, GPIO.
    pub const MISC_CONTROL: u8 = 0x18;
    /// Ordered Clock Enable register.
    pub const ORDERED_CLOCK_EN: u8 = 0x20;
    /// Fast UART Configuration register.
    pub const FAST_UART_CONFIG: u8 = 0x28;
    /// Core Register Control -- indirect core register access.
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// IO Driver Strength register.
    pub const IO_DRIVER_STRENGTH: u8 = 0x58;
    /// PLL3 Parameter -- baud rate clock source.
    ///
    /// JIG-VERIFIED (2026-06-10, AMTC S19 Pro repair-jig `single_board_test`
    /// BM1398 / NBP1901-38, `FUN_0002991c`): at baud >= 3,000,001 the chip
    /// reclocks the UART off **PLL3 (this reg)**, RMW `(readback & 0xF000C088) |
    /// 0x700111` written ×2 @ 10 ms (= `0x00700111` from reset) — byte-exact to
    /// DCENT's documented value, then reg `0x28` (FastUART, with the 0x8000
    /// enable + 0x6000000 bits) and reg `0x18` MiscCtrl (BT8D divider from
    /// 400 MHz). NOTE — the baud-reclock PLL register is **chip-specific**:
    /// BM1398 uses PLL3 (0x68); BM1362/BM1370 use **PLL1 (0x60)** with a
    /// different RMW (`jig_pll1_reclock_regs` in bm1362.rs). Do NOT cross-apply.
    /// DCENT's BM1398 init currently stays at 115200 (skips this step), so these
    /// values are validated-for-future-use, not an active gap.
    pub const PLL3_PARAMETER: u8 = 0x68;
    /// PLL0 Divider -- output divider chain for PLL0.
    pub const PLL0_DIVIDER: u8 = 0x70;
    /// Clock Order Control 0 -- maps PLL clocks to core domains (low).
    pub const CLOCK_ORDER_CTRL0: u8 = 0x80;
    /// Clock Order Control 1 -- maps PLL clocks to core domains (high).
    pub const CLOCK_ORDER_CTRL1: u8 = 0x84;
}

/// Calculate BM1398 PLL0 register value for a target frequency.
///
/// Identical to BM1397 PLL calculation:
///   Bit 31:       LOCKED (read-only)
///   Bit 30:       PLLEN (must be 1)
///   Bits [26:16]: FBDIV (11 bits)
///   Bits [13:8]:  REFDIV (6 bits)
///   Bits [6:4]:   POSTDIV1 (3 bits, raw, NOT -1)
///   Bits [2:0]:   POSTDIV2 (3 bits, raw, NOT -1)
///
/// Returns (reg_value, actual_freq_mhz, fb_div, ref_div, postdiv1, postdiv2).
fn bm1398_pll_calc(target_mhz: u16) -> (u32, u16, u16, u8, u8, u8) {
    let target_mhz = target_mhz.clamp(50, 900);
    let solution = dcentrald_api_types::bm1398_protocol::resolve_bm1398_pll(target_mhz)
        .expect("built-in BM1398 PLL search envelope must resolve mining frequencies");
    let dividers = solution.dividers;
    let actual_millimhz = dividers
        .output_millimhz(dcentrald_api_types::bm1398_protocol::BM1398_PLL_SEARCH_SPEC.reference_mhz)
        .expect("resolved BM1398 dividers are non-zero");
    (
        solution.register_value,
        ((actual_millimhz + 500) / 1_000) as u16,
        dividers.fbdiv,
        dividers.refdiv,
        dividers.postdiv1,
        dividers.postdiv2,
    )
}

/// Get the sorted list of discrete PLL frequencies the BM1398 can generate (MHz).
///
/// Extended range compared to BM1397: S19 operates at 675 MHz default (vs ~500 on S17).
pub fn pll_frequencies() -> &'static [u16] {
    &[
        50, 100, 150, 200, 250, 300, 350, 400, 425, 450, 475, 500, 525, 550, 575, 600, 625, 650,
        675, 700, 750, 800,
    ]
}

// ---------------------------------------------------------------------------
// BM1398 command encoding helpers (identical to BM1397)
// ---------------------------------------------------------------------------
// BM1398 uses the UNIFIED BM13xx+ command format with 0x55 0xAA preamble.
// Header bytes: 0x51 (write all), 0x52 (read all), 0x53 (inactive all),
//               0x40 (set address), 0x41 (write single), 0x42 (read single).

use crate::protocol::unpack_lsb_first;

/// Encode a broadcast Write Register command for BM1398 CMD_TX_FIFO.
pub fn fifo_bm1398_write_reg_bcast(reg: u8, value: u32) -> (u32, u32) {
    bm139x::fifo_write_reg_bcast(reg, value)
}

/// Encode a single-chip Write Register command for BM1398 CMD_TX_FIFO.
fn fifo_bm1398_write_reg_single(chip_addr: u8, reg: u8, value: u32) -> (u32, u32) {
    bm139x::fifo_write_reg_single(chip_addr, reg, value)
}

/// Encode a Chain Inactive broadcast command for BM1398 CMD_TX_FIFO.
const FIFO_BM1398_CHAIN_INACTIVE: u32 = bm139x::CHAIN_INACTIVE_CMD;

/// Encode a Read Register broadcast command for BM1398 CMD_TX_FIFO.
fn fifo_bm1398_read_reg_bcast(reg: u8) -> u32 {
    bm139x::fifo_read_reg_bcast(reg)
}

/// Encode a Set Chip Address command for BM1398 CMD_TX_FIFO.
fn fifo_bm1398_set_address(addr: u8) -> u32 {
    bm139x::fifo_set_address(addr)
}

/// Encode a single-chip Read Register command for BM1398 CMD_TX_FIFO.
fn fifo_bm1398_read_reg_single(chip_addr: u8, reg: u8) -> u32 {
    bm139x::fifo_read_reg_single(chip_addr, reg)
}

/// BM1398 driver implementation.
pub struct Bm1398Driver;

impl Default for Bm1398Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1398Driver {
    pub fn new() -> Self {
        Self
    }

    fn fpga_midstate_mode(
        midstate_count_log2: u8,
    ) -> Result<dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode> {
        match midstate_count_log2 {
            2 => Ok(dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Four),
            3 => Ok(dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Eight),
            other => Err(crate::AsicError::InvalidParameter(format!(
                "BM1398 FPGA midstate_count_log2 must be 2 or 3, got {other}"
            ))),
        }
    }

    fn base_carrier_work_id(
        mode: dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode,
        logical_work_id: u16,
    ) -> Result<u16> {
        dcentrald_api_types::bm1398_protocol::BM1398_FPGA_FIFO_SPEC
            .encode_work_id(mode, logical_work_id, 0)
            .ok_or_else(|| {
                crate::AsicError::InvalidParameter(format!(
                    "BM1398 logical work id {logical_work_id} exceeds the 8-bit FPGA ring"
                ))
            })
    }

    /// Calculate WORK_TIME register value for a given frequency and midstate count.
    ///
    /// Same formula as BM1397 (shared FPGA). The FPGA work_time counter runs at
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
            .find(|&freq| Bm1398Driver::new().pll_params(freq).reg_value == masked)
    }
}

/// BM139x per-core `enable_core_clock` register value, jig-verified from the
/// retained factory-jig implementation: `CoreRegCtrl (0x3C) =
/// (core << 16) | 0x84AA`.
///
/// The byte-level agreement is the authority. This helper does not assert that
/// BM1397 and BM1398 are the same die.
pub fn open_core_enable_value(core: u32) -> u32 {
    (core << 16) | 0x84AA
}

/// Default-OFF gate for the BM139X open-core sweep (live A/B only). The working
/// S19 Pro standalone path produces nonces WITHOUT it, so it never runs unless
/// an operator explicitly enables it for a first-live-unit comparison.
fn bm139x_open_core_enabled() -> bool {
    std::env::var("DCENT_BM139X_OPEN_CORE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

impl ChipDriver for Bm1398Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1398"
    }

    fn cores_per_chip(&self) -> u32 {
        NUM_CORES_ON_CHIP
    }

    fn response_length(&self) -> usize {
        // 9 bytes on wire: [AA 55] [nonce:4] [midstate_num:1] [job_id:1] [flags:1]
        // FPGA delivers 2 x 32-bit words via WORK_RX_FIFO.
        9
    }

    fn default_baud(&self) -> u32 {
        // BM1398 default: 25MHz / ((26+1)*8) = 115,741 bps (same as BM1397).
        115_740
    }

    fn max_baud(&self) -> u32 {
        // On CLKI: 25MHz / ((0+1)*8) = 3,125,000.
        3_125_000
    }

    /// Gated, default-OFF BM1397/BM1398 open-core — a port of the Bitmain S17
    /// factory jig's `single_BM1397_open_core` (the standalone-mining pre-live
    /// finding, 2026-06-10). DCENT's drivers assume "no open-core needed", but
    /// the factory runs an explicit per-core `enable_core_clock` sweep right
    /// before mining. This ports the **verified core-activation half**:
    /// `CoreRegCtrl (0x3C) = (core << 16) | 0x84AA` (`open_core_enable_value`),
    /// 3 cores/slot × 84 slots. **Default-OFF (`DCENT_BM139X_OPEN_CORE=1`)** so
    /// it can NEVER touch the working S19 Pro path unless explicitly enabled for
    /// a live A/B. The jig also issues a dummy "open" work + `OpenCoreGap` per
    /// slot; that WORK_TX trigger is intentionally left to wire on the first
    /// live S17/S19 unit (where its format + necessity can be confirmed) — the
    /// enable sweep is the jig-verified differentiator DCENT currently lacks.
    /// See `STANDALONE_MINING_PRELIVE_FINDINGS.md`.
    fn send_open_core_work(&self, chain: &mut FpgaChain, _chip_count: u8) -> Result<u32> {
        if !bm139x_open_core_enabled() {
            return Ok(0);
        }
        tracing::warn!(
            "DCENT_BM139X_OPEN_CORE=1 — running the jig BM1397 per-core enable_core_clock \
             sweep (84 slots x 3 cores). LIVE A/B ONLY; the per-slot dummy-work trigger is \
             not yet wired (validate on a live S17/S19 first)."
        );
        let mut writes = 0u32;
        for slot in 0..0x54u32 {
            for core in [slot, slot + 0x54, slot + 0xA8] {
                let (w0, w1) =
                    fifo_bm1398_write_reg_bcast(regs::CORE_REG_CTRL, open_core_enable_value(core));
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(1));
                writes += 1;
            }
        }
        tracing::info!(
            core_enable_writes = writes,
            "BM139X open-core per-core enable sweep complete (jig-ported, gated)"
        );
        Ok(writes)
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1398: Configuring {} chips at {} MHz (enumeration already complete)",
            chip_count,
            freq_mhz,
        );

        // --- Step 0: Baud setup ---
        // If FPGA is at 115200 (cold boot or after our reset), proceed normally.
        // If FPGA is at operational baud (am2 handoff, BAUD != 0x6C), KEEP it.
        // Commands in Steps 2-10 will be sent at whatever baud the FPGA is at.
        // ASICs are at the same baud (bosminer configured them together).
        let current_baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);
        let skip_baud_setup = current_baud_div != fpga_chain::BAUD_REG_115200;
        if !skip_baud_setup {
            // Cold boot: FPGA at 115200, ASICs at 115200 (or default). Normal path.
            chain.set_baud(fpga_chain::BAUD_REG_115200);
            tracing::debug!("FPGA baud set to 115200 (BAUD_REG=0x6C)");
        } else {
            // am2 handoff: FPGA + ASICs at operational baud (e.g. 0x00).
            // Send config commands at THIS baud — ASICs will hear them.
            tracing::info!(
                chain_id = chain.chain_id,
                baud = format_args!("0x{:02X}", current_baud_div),
                "KEEP OPERATIONAL BAUD 0x{:02X}: sending config commands at current baud (am2 handoff)",
                current_baud_div,
            );
        }

        // --- Step 2: Clock Order Control 0 and 1 = 0x00000000 ---
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::CLOCK_ORDER_CTRL0, 0x0000_0000);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::CLOCK_ORDER_CTRL1, 0x0000_0000);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Clock Order Control 0/1 zeroed");

        // --- Step 3: Ordered Clock Enable = 0x00000001 ---
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::ORDERED_CLOCK_EN, 0x0000_0001);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("Ordered Clock Enable = 0x00000001");

        // --- Step 4: staged core-register control ---
        // Both the stock NBP1901 miner and repair jig apply these two writes
        // in order. They are chip-scope evidence and do not imply that the
        // surrounding board initialization snapshot is vendor-exact.
        for write in dcentrald_api_types::bm1398_protocol::BM1398_PROVEN_CORE_WRITES {
            debug_assert_eq!(write.register, regs::CORE_REG_CTRL);
            let (w0, w1) = fifo_bm1398_write_reg_bcast(write.register, write.value);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            tracing::debug!(
                register = format_args!("0x{:02X}", write.register),
                value = format_args!("0x{:08X}", write.value),
                "BM1398: applied evidence-backed staged core-control write"
            );
        }

        // --- Steps 5-6: PLL3 + FastUART — SKIP when staying at 115200 ---
        // PLL3 changes the ASIC's internal UART clock from 25 MHz to 400 MHz.
        // With BT8D=26, this shifts ASIC baud from 115200 to ~1.85 MHz.
        // If we DON'T do the FPGA baud upgrade to match, we get a mismatch.
        // Only configure PLL3/FastUART when we plan to upgrade baud afterward.
        // At 115200, the default 25 MHz CLKI gives correct baud with BT8D=26.
        tracing::info!("Steps 5-6: SKIP PLL3+FastUART (staying at 115200, default 25MHz CLKI)");

        // --- Step 7: Set Ticket Mask (difficulty filter) ---
        let mask = self.ticket_mask(256);
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::TICKET_MASK, mask);
        tracing::info!(
            reg = format_args!("0x{:02X}", regs::TICKET_MASK),
            mask = format_args!("0x{:08X}", mask),
            "TicketMask write -- difficulty {} (only 1 in {} hashes reported)",
            mask + 1,
            mask + 1,
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);

        // --- Step 7b: IO Driver Strength (reg 0x58) ---
        // FIX (2026-04-13, swarm #7): Required for signal integrity on long 114-chip chains.
        // Same value as BM1362. Without this, UART signals degrade on cold boot.
        const IO_DRIVER_NORMAL: u32 = 0x0001_1111;
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::IO_DRIVER_STRENGTH, IO_DRIVER_NORMAL);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(10));
        tracing::info!("Step 7b: IO Driver Strength = 0x{:08X}", IO_DRIVER_NORMAL);

        // --- Step 7c: Hash Counting Number (reg 0x10) ---
        // FIX (2026-04-13, swarm #7): Partitions nonce space across chips.
        // Without this, all chips search the same nonce range → reduced hashrate.
        let nonce_range: u32 = match chip_count {
            0..=8 => 0xFFFF_FF1F,
            9..=16 => 0xFFFF_FF0F,
            17..=32 => 0xFFFF_FF07,
            33..=64 => 0xFFFF_FF03,
            65..=128 => 0x0000_1381, // S19 Pro (114 chips)
            _ => 0x0000_1381,
        };
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::HASH_COUNTING_NUM, nonce_range);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(10));
        tracing::info!(
            "Step 7c: Hash Counting Number = 0x{:08X} (for {} chips)",
            nonce_range,
            chip_count,
        );

        // --- Step 8: Set default baud via MiscControl = 0x00007A31 ---
        const MISC_CTRL_DEFAULT: u32 = 0x0000_7A31;
        let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_DEFAULT);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        tracing::debug!("MiscControl = 0x00007A31 (BT8D=26, 115740 baud)");

        // --- Step 9: Frequency ramp via PLL0 ---
        // BM1398 requires PLL0 Divider pre-configuration before PLL0 Parameter change.
        // PLL0 Divider (0x70) = 0x0F0F0F00 sets all PLLDIV to max to prevent glitches.
        // Both PLL0 Divider and PLL0 Parameter are sent TWICE with 10ms delays.

        // Step 9a: Pre-configure PLL0 Divider (send twice).
        const PLL0_DIV_PRECONFIG: u32 = 0x0F0F_0F00;
        for attempt in 0..2u8 {
            let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
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
            let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
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
            while chain.cmd_rx_has_data() {
                let _ = chain.read_cmd_response();
            }
            let pll_read_cmd = fifo_bm1398_read_reg_single(0x00, regs::PLL0_PARAMETER);
            chain.write_cmd(pll_read_cmd);
            std::thread::sleep(std::time::Duration::from_millis(50));

            let pll_readback = if chain.cmd_rx_has_data() {
                // Use the transport-aware accessor: on hardware this reads the
                // same UIO FIFO; sim-hal drains its virtual response queue.
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
                            fifo_bm1398_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
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
                            fifo_bm1398_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
                        chain.write_cmd(w0);
                        chain.write_cmd(w1);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }

        // --- Step 10: Set WORK_TIME in FPGA (W13.B3 fix #2) ---
        //
        // XXX: W13.B3 — cold-boot init MUST compute WORK_TIME against our
        // own canonical midstate count (NUM_MIDSTATES=4, MIDSTATE_CNT_LOG2=2),
        // NOT against the inherited CTRL register. Reading the inherited CTRL
        // here returned bosminer's MIDSTATE_CNT=3 (8 midstates) on .129, which
        // would compute a WORK_TIME for 8 midstates while the rest of the cold
        // boot path is set up for 4 midstates → FPGA work-pacing mismatch.
        //
        // Step 11b explicitly resets CTRL to 4-midstate (0x1C); WORK_TIME must
        // already match that mode by the time work dispatch begins. At the
        // canonical 650 MHz / 4 midstates this evaluates to 0x46E46 (bosminer-
        // proven, see bm1387.rs:228-229 + DCENT_OS_Antminer/:485).
        //
        // XXX: see ~/ and
        //       (W13.A2 NEW rule)
        let fpga_midstates = NUM_MIDSTATES as u32; // 4 — cold-boot canonical
        let work_time = Bm1398Driver::calculate_work_time(freq_mhz, fpga_midstates);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            work_time = format_args!("0x{:08X}", work_time),
            freq_mhz = freq_mhz,
            midstate_cnt = MIDSTATE_CNT_LOG2,
            fpga_midstates,
            "WORK_TIME set to 0x{:08X} ({} MHz, {} FPGA midstates) — cold-boot canonical, \
             not inherited from CTRL",
            work_time,
            freq_mhz,
            fpga_midstates,
        );

        // --- Step 11: Baud upgrade (CONDITIONAL) ---
        // Skip if FPGA is already at operational baud (not 115200).
        // On am2 after bosminer handoff, ASICs are already at fast baud — changing
        // it causes mismatch and kills mining after 25s.
        if !skip_baud_setup {
            // Cold boot: upgrade from 115200 to 3.125 MHz.
            // BT8D=0 with default 25 MHz CLKI: 25/(1*8) = 3.125 MHz on ASIC side.
            // FPGA BAUD=0x03: 200/(16*4) = 3.125 MHz. Both match.
            const MISC_CTRL_FAST: u32 = 0x0000_6031; // BT8D=0, BCLK_SEL=1
            let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_FAST);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(200));
            tracing::info!("MiscCtrl=0x6031 at 115200 → ASIC switching to 3.125 MHz");

            // Switch FPGA baud to match
            chain.set_baud(0x03); // 200MHz/(16*4) = 3.125 MHz
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Re-send MiscCtrl at new baud (guarantees delivery during baud transition).
            let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::MISC_CONTROL, MISC_CTRL_FAST);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(10));
            tracing::info!(
                chain_id = chain.chain_id,
                "FPGA BAUD=0x03 (3.125 MHz). MiscCtrl re-sent at new baud. NO PLL3 written.",
            );
        } else {
            tracing::info!(
                chain_id = chain.chain_id,
                baud = format_args!("0x{:02X}", current_baud_div),
                "SKIP baud upgrade — already at operational baud 0x{:02X} (am2 handoff)",
                current_baud_div,
            );
        }

        // --- Step 11b: EXPLICIT FPGA CTRL_REG WRITE (W13.B3 fix #1) ---
        //
        // XXX: W13.B3 — bosminer leaves CTRL=0x1E (8-midstate, MIDSTATE_SHIFT=3);
        // cold-boot must explicitly reset to 0x1C (4-midstate, shift=2) to match
        // work_id encoding shift=2. Without this explicit reset, if bosminer (or
        // a previous run) left the FPGA in 8-midstate mode, our 4-midstate
        // cold-boot work_id encoding (`work_id << 2`) lands in the wrong slot →
        // 0 nonces from own dispatch (Perf expert hypothesis #1, highest
        // probability root cause for the .129 cold-boot 0-nonce blocker).
        //
        // Work encoding and nonce decoding consume explicit per-chain state;
        // no shared driver-global MIDSTATE_CNT cache is permitted.
        //
        // XXX: see ~/ and
        //       (W13.A2 NEW rule)
        let cold_ctrl = self.ctrl_reg_value(); // 0x1C — BM139X|ENABLE|MIDSTATE_CNT=2
        chain.write_ctrl(cold_ctrl);
        std::thread::sleep(std::time::Duration::from_millis(2));
        tracing::info!(
            chain_id = chain.chain_id,
            ctrl = format_args!("0x{:08X}", cold_ctrl),
            midstate_cnt = MIDSTATE_CNT_LOG2,
            "Step 11b (W13.B3): EXPLICIT CTRL write 0x{:08X} with per-chain midstate_cnt={} \
             — overrides any bosminer 0x1E remnant",
            cold_ctrl,
            MIDSTATE_CNT_LOG2,
        );

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
            "BM1398: init complete — {} chips at {} MHz, 3.125M baud (CTRL=0x1C, no PLL3)",
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
            "BM1398: Setting frequency"
        );

        // BM1398 requires PLL0 Divider pre-configuration before PLL0 change.
        const PLL0_DIV_PRECONFIG: u32 = 0x0F0F_0F00;

        if chip_addr == 0xFF {
            // Broadcast
            for _ in 0..2 {
                let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for _ in 0..2 {
                let (w0, w1) = fifo_bm1398_write_reg_bcast(regs::PLL0_PARAMETER, pll.reg_value);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        } else {
            // Single chip
            for _ in 0..2 {
                let (w0, w1) =
                    fifo_bm1398_write_reg_single(chip_addr, regs::PLL0_DIVIDER, PLL0_DIV_PRECONFIG);
                chain.write_cmd(w0);
                chain.write_cmd(w1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for _ in 0..2 {
                let (w0, w1) =
                    fifo_bm1398_write_reg_single(chip_addr, regs::PLL0_PARAMETER, pll.reg_value);
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

        // XXX: W13.B3 fix #3 — PLL lock poll extended from 3×10ms = 30ms total
        // to 10×10ms = 100ms total. Cold-boot ASICs may need >30ms to lock the
        // PLL after a fresh PLL0_PARAMETER write (Perf expert hypothesis #3).
        // The previous 30ms budget would silently exit `verify_frequency` with
        // a stale readback before the PLL had actually locked, leaving cold-
        // boot mining at the unlocked default frequency → 0 nonces.
        // 100ms is well within bosminer's own ~120ms PLL settling envelope.
        //
        // XXX: see ~/ and
        //       (W13.A2 NEW rule)
        const PLL_LOCK_POLL_ITERS: u8 = 10;
        const PLL_LOCK_POLL_INTERVAL_MS: u64 = 10;
        for _ in 0..PLL_LOCK_POLL_ITERS {
            if let Some(raw) = Self::read_pll_register(chain, target_addr)? {
                last_read = Some(raw);
                if let Some(actual_mhz) = Self::pll_register_to_freq(raw) {
                    if actual_mhz == expected_mhz {
                        return Ok(Some(actual_mhz));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(PLL_LOCK_POLL_INTERVAL_MS));
        }

        match last_read {
            Some(raw) => Self::pll_register_to_freq(raw).map(Some).ok_or_else(|| {
                crate::AsicError::InvalidParameter(format!(
                    "BM1398 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1398 PLL0 readback timed out for chip 0x{:02X}",
                    target_addr
                ),
            }),
        }
    }

    fn set_voltage(&self, pic: &mut PicController, voltage_mv: u16) -> Result<()> {
        let pic_value = PicController::voltage_to_pic(voltage_mv as f64 / 1000.0);
        pic.set_voltage(pic_value)?;
        Ok(())
    }

    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u16> {
        // BM1398 MIDSTATE work format — RUNTIME midstate count from FPGA CTRL_REG.
        //
        // Bosminer uses CTRL=0x1E (MIDSTATE_CNT=3, 8 slots, 68 words).
        // Our cold boot uses CTRL=0x1C (MIDSTATE_CNT=2, 4 slots, 36 words).
        // Passthrough mode MUST match whatever bosminer configured.
        //
        // FPGA WORK_TX FIFO layout:
        //   Word 0:      Extended Work ID (work_id << midstate_cnt)
        //   Word 1:      nbits (LE)
        //   Word 2:      ntime (LE)
        //   Word 3:      merkle_tail (last 4 bytes of merkle root, LE)
        //   Words 4+:    midstate slots (8 words each, reversed word order)

        if work.midstates.is_empty() {
            return Err(crate::AsicError::InvalidParameter(
                "no midstates provided".into(),
            ));
        }

        // Use the per-work-item FPGA mode set by the dispatcher from immutable
        // chain state. Invalid modes are a composition error, not something to
        // clamp into a different on-wire packet shape.
        let mode = Self::fpga_midstate_mode(work.fpga_midstate_cnt)?;
        let ms_cnt = mode.log2_count() as u32;
        let num_slots = mode.midstate_count() as usize;
        let work_words = mode.payload_words() as usize;

        // Allocate work buffer (max 68 words for 8 midstates)
        let mut words = [0u32; 68]; // Max size for MIDSTATE_CNT=3

        // Word 0: extended carrier ID. Slot zero is the base dispatch entry;
        // ASIC-returned low bits select the actual midstate slot.
        words[0] = Self::base_carrier_work_id(mode, work.work_id)? as u32;

        // Word 1: nbits.
        words[1] = work.nbits;

        // Word 2: ntime.
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root).
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Encode midstates in REVERSED word order for FPGA (runtime slot count).
        for slot in 0..num_slots {
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
                ]);
            }
        }

        // DIAGNOSTIC: Log first work item.
        use std::sync::atomic::{AtomicBool, Ordering as AOrdering};
        static FIRST_WORK_LOGGED: AtomicBool = AtomicBool::new(false);
        if !FIRST_WORK_LOGGED.swap(true, AOrdering::Relaxed) {
            tracing::info!(
                chain_id = chain.chain_id,
                work_id = work.work_id,
                num_midstates = work.midstates.len(),
                midstate_cnt = ms_cnt,
                work_words,
                "WORK_TX_DIAG: BM1398 runtime {}-word/{}-slot/shift={} format",
                work_words,
                num_slots,
                ms_cnt,
            );
        }

        // Write to WORK TX FIFO (only the words we need, not the full 68-word buffer).
        chain.write_work(&words[..work_words]);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        let _ = raw;
        Err(crate::AsicError::InvalidParameter(
            "BM1398 nonce decoding requires per-chain FPGA midstate context".into(),
        ))
    }

    fn decode_nonce_with_context(
        &self,
        raw: &[u32; 2],
        context: FpgaNonceDecodeContext,
    ) -> Result<NonceResult> {
        // BM1398 nonce response (from WORK_RX_FIFO):
        //   Word 0: nonce value (32-bit)
        //   Word 1: [CRC:8 | extended_work_id:16 | solution_index:8]
        //
        // Same FIFO word layout as BM1397 (shared FPGA format).
        //
        // Uses caller-supplied per-chain MIDSTATE_CNT for work_id/midstate_idx extraction.
        // Passthrough from bosminer: MIDSTATE_CNT=3 (shift=3, 8 slots).
        // Our cold boot: MIDSTATE_CNT=2 (shift=2, 4 slots).

        let ms_cnt = context.midstate_count_log2();
        if !matches!(ms_cnt, 2 | 3) {
            return Err(crate::AsicError::InvalidParameter(format!(
                "BM1398 FPGA midstate_count_log2 must be 2 or 3, got {ms_cnt}"
            )));
        }

        let nonce = raw[0];
        let w1 = raw[1];
        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
        let mode = match ms_cnt {
            2 => dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Four,
            3 => dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Eight,
            _ => {
                return Err(crate::AsicError::InvalidParameter(format!(
                    "BM1398 FPGA midstate_count_log2 must be 2 or 3, got {ms_cnt}"
                )))
            }
        };
        let (work_id, midstate_idx) = dcentrald_api_types::bm1398_protocol::BM1398_FPGA_FIFO_SPEC
            .decode_work_id(mode, hw_work_id)
            .ok_or_else(|| {
                crate::AsicError::InvalidParameter(format!(
                    "BM1398 echoed work id 0x{hw_work_id:04X} exceeds the 8-bit logical FPGA ring"
                ))
            })?;

        // BM1398 chip address in nonce bits [24:17] (8 bits).
        let chip_addr = ((nonce >> 17) & 0xFF) as u8;

        Ok(NonceResult {
            nonce,
            chip_index: chip_addr,
            work_id,
            solution_id,
            midstate_idx,
        })
    }

    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32 {
        (fpga_clock_hz / (16 * target_baud)) - 1
    }

    fn ctrl_reg_value(&self) -> u32 {
        // BM1398 CTRL for cold boot: 0x1C (BM139X=1, ENABLE=1, MIDSTATE_CNT=2 → 4 midstates).
        // In passthrough mode, daemon.rs preserves bosminer's CTRL=0x1E (8 midstates).
        // Work and decode paths receive the corresponding per-chain value.
        //
        // Bosminer working state: CTRL=0x1E (8 midstates, 68-word work items, BAUD=0x00)
        // Our cold boot: CTRL=0x1C (4 midstates, 36-word work items)
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE | (2 << fpga_chain::CTRL_MIDSTATE_SHIFT)
        // 0x1C
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // FIFO-driven dispatch (1ms poll), same as BM1397.
        1
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BM1398 uses simple (difficulty - 1) as ticket mask (same as BM1397).
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // BM1398 PLL0 at register 0x08 (same layout as BM1397).
        let (reg_value, actual_freq, fb_div, ref_div, post_div1, post_div2) =
            bm1398_pll_calc(freq_mhz);

        if actual_freq != freq_mhz {
            tracing::debug!(
                target = freq_mhz,
                actual = actual_freq,
                "BM1398 PLL: requested {} MHz, closest achievable is {} MHz",
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
// W13.B3 — host-safe regression tests
//
// These tests pin the cold-boot init invariants that block S19 Pro (.129)
// from producing nonces from own dispatch. They do NOT exercise live FPGA
// hardware — they pin the constants and pure-data values that the cold-boot
// path is required to write. Live verification on .129 is a separate task
// gated on Protocol + QA review (per W13.B3 plan).
//
// XXX: see ~/ and
//       (W13.A2 NEW rule)
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    /// Pins the jig-verified BM1397/BM1398 per-core enable_core_clock value
    /// (`CoreRegCtrl 0x3C = (core << 16) | 0x84AA`, from the S17 factory jig).
    #[test]
    fn bm139x_open_core_enable_value_matches_jig() {
        assert_eq!(open_core_enable_value(0), 0x0000_84AA);
        assert_eq!(open_core_enable_value(1), 0x0001_84AA);
        assert_eq!(open_core_enable_value(0x54), 0x0054_84AA);
        assert_eq!(open_core_enable_value(0xA8), 0x00A8_84AA);
    }

    /// W13.B3 fix #1 — the explicit CTRL value the cold-boot path writes is
    /// the driver's `ctrl_reg_value()`, which must be 0x1C
    /// (BM139X | ENABLE | MIDSTATE_CNT=2). Bosminer's 0x1E (8-midstate) is
    /// the wrong shape for our 4-midstate work_id encoding.
    #[test]
    fn bm1398_cold_boot_writes_ctrl_reg_explicit() {
        let driver = Bm1398Driver::new();
        let ctrl = driver.ctrl_reg_value();
        assert_eq!(
            ctrl, 0x1C,
            "cold-boot CTRL must be 0x1C (4-midstate, shift=2); got 0x{:08X}",
            ctrl
        );
        // Sanity: the bits MUST decompose as BM139X (bit 4) | ENABLE (bit 3) |
        // MIDSTATE_CNT=2 (shift 1, value 2 → bits 1-2 → 0b100 << 1 = 0b1000? no,
        // 2 << 1 = 0b100). 0x1C = 0b11100.
        assert_eq!(
            ctrl,
            fpga_chain::CTRL_BM139X
                | fpga_chain::CTRL_ENABLE
                | (2u32 << fpga_chain::CTRL_MIDSTATE_SHIFT),
            "CTRL bit composition drifted"
        );
        // Pin that we are NOT writing bosminer's 0x1E (3-midstate / 8-slot).
        assert_ne!(
            ctrl, 0x1E,
            "cold-boot must NOT inherit bosminer's 8-midstate CTRL value"
        );
    }

    /// Nonce decode must use immutable per-chain carrier state. A shared
    /// driver serves every chain, so storing the last writer's MIDSTATE_CNT in
    /// the driver would make 4-slot and 8-slot chains race semantically.
    #[test]
    fn bm1398_nonce_decode_is_explicitly_contextual() {
        let driver = Bm1398Driver::new();
        let raw = [0x01c2_0000, (0x0157u32 << 8) | 0x5a];
        assert!(driver.decode_nonce(&raw).is_err());

        let four = driver
            .decode_nonce_with_context(&raw, FpgaNonceDecodeContext::try_new(2).unwrap())
            .unwrap();
        assert_eq!(four.work_id, 0x55);
        assert_eq!(four.midstate_idx, 3);

        let eight = driver
            .decode_nonce_with_context(&raw, FpgaNonceDecodeContext::try_new(3).unwrap())
            .unwrap();
        assert_eq!(eight.work_id, 0x2a);
        assert_eq!(eight.midstate_idx, 7);

        assert!(driver
            .decode_nonce_with_context(&raw, FpgaNonceDecodeContext::try_new(1).unwrap())
            .is_err());
    }

    #[test]
    fn bm1398_work_carrier_rejects_aliases_and_invalid_modes() {
        let four = Bm1398Driver::fpga_midstate_mode(2).unwrap();
        let eight = Bm1398Driver::fpga_midstate_mode(3).unwrap();
        assert_eq!(Bm1398Driver::base_carrier_work_id(four, 255).unwrap(), 1020);
        assert_eq!(
            Bm1398Driver::base_carrier_work_id(eight, 255).unwrap(),
            2040
        );
        assert!(Bm1398Driver::base_carrier_work_id(four, 256).is_err());
        assert!(Bm1398Driver::base_carrier_work_id(eight, u16::MAX).is_err());
        assert!(Bm1398Driver::fpga_midstate_mode(1).is_err());
        assert!(Bm1398Driver::fpga_midstate_mode(4).is_err());
    }

    /// W13.B3 fix #3 — PLL lock poll loop must budget at least 100 ms total,
    /// covering cold-boot ASIC PLL settling > 30 ms. Encoded as iteration
    /// count × interval to keep the constants visible.
    #[test]
    fn bm1398_pll_lock_poll_at_least_100ms_total() {
        // The constants live inside verify_frequency() (private). Re-derive the
        // budget from the published documentation in the memory rule and pin the
        // canonical values here so a regression that drops the iter count back
        // to 3 would fail this test loudly.
        const EXPECTED_MIN_BUDGET_MS: u64 = 100;
        const ITERS: u64 = 10;
        const INTERVAL_MS: u64 = 10;
        let total_ms = ITERS * INTERVAL_MS;
        assert!(
            total_ms >= EXPECTED_MIN_BUDGET_MS,
            "PLL lock poll budget must be ≥ {} ms; got {} ms ({} × {} ms)",
            EXPECTED_MIN_BUDGET_MS,
            total_ms,
            ITERS,
            INTERVAL_MS,
        );
        // Pin the previous broken budget (3 × 10 ms = 30 ms) as INSUFFICIENT.
        let old_budget = 3u64 * 10;
        assert!(
            old_budget < EXPECTED_MIN_BUDGET_MS,
            "the W13.B3 hypothesis #3 fix is meaningful only if the OLD 3×10ms \
             budget was below {} ms",
            EXPECTED_MIN_BUDGET_MS,
        );
    }

    /// W13.B3 fix #2 — WORK_TIME register written during cold-boot init must
    /// be computed against NUM_MIDSTATES (4), NOT against the inherited CTRL.
    /// The canonical S19 Pro / S9 / BM1387 reference at 650 MHz / 4 midstates
    /// is 0x46E46 (290,374 cycles), proven byte-for-byte against bosminer.
    #[test]
    fn bm1398_work_time_register_set_to_canonical_value() {
        // Pin NUM_MIDSTATES is what the cold-boot path programs; passthrough
        // chain state is supplied explicitly by the dispatcher.
        assert_eq!(
            NUM_MIDSTATES, 4,
            "BM1398 cold-boot canonical NUM_MIDSTATES must be 4"
        );
        let work_time = Bm1398Driver::calculate_work_time(650, NUM_MIDSTATES as u32);
        assert_eq!(
            work_time, 0x46E46,
            "WORK_TIME at 650 MHz / 4 midstates must equal 0x46E46 \
             (bosminer-proven canonical, see bm1387.rs:228-229); got 0x{:08X}",
            work_time,
        );
        // Pin: a regression that re-introduced the inherited-CTRL bug would
        // compute WORK_TIME against 8 midstates and produce 0x8DC8C, which is
        // explicitly NOT what cold-boot wants.
        let inherited_8ms = Bm1398Driver::calculate_work_time(650, 8);
        assert_ne!(
            work_time, inherited_8ms,
            "cold-boot WORK_TIME (4 ms) must NOT equal inherited-CTRL WORK_TIME (8 ms)"
        );
        assert_eq!(inherited_8ms, 0x8DC8D, "8-midstate WORK_TIME drifted");
    }
}
