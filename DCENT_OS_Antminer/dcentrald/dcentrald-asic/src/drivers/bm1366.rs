//! BM1366 ASIC driver (Antminer S19 XP, S19k Pro, BitAxe Ultra).
//!
//! The BM1366 is a 5nm SHA-256 ASIC with hardware version rolling (AsicBoost).
//! Unlike the BM1387 which requires host-computed midstates, the BM1366 accepts
//! full block header components and computes midstates internally while rolling
//! version bits autonomously.
//!
//! Key characteristics:
//!   - 5nm process
//!   - 894 cores per chip (112 large x ~8 small cores)
//!   - ~0.4V core voltage
//!   - 11-byte nonce response (includes 16-bit version field)
//!   - Full header job format (82 bytes) — chip computes midstates internally
//!   - On-chip hardware version rolling via MID_AUTO_GEN (register 0xA4)
//!   - PLL0 at 0x08 for hashing, PLL1 at 0x60 for baud clock
//!   - FB_DIV range: 144-235, POSTDIV encoding: (POSTDIV1-1) << 4 | (POSTDIV2-1)
//!   - Default UART baud: 115,740 (BT8D=26 in Fast UART Config 0x28)
//!   - Maximum 1 Mbps baud via Fast UART (register 0x28)
//!   - Job ID increment: +8 mod 128
//!   - 110 chips per chain (S19 XP) or 77 (S19k Pro)
//!   - CTRL_REG: BM139X mode (bit4=1)
//!
//! Register values from ESP-Miner and ASIC Register Bible:
//!   0x00 ChipAddress:  0x13660000 (CHIP_ID=0x1366, addr=0x00)
//!   0x08 PLL0:         0xC0600161 (default/reset PLL config)
//!   0x18 MiscControl:  0x0000C100 (different reset from BM1397)
//!   0x28 FastUART:     0x01301A00 (BT8D=0x1A=26 -> 115,740 baud)
//!   0xA4 VersionRoll:  0x9000FFFF (EN=1, MASK=0xFFFF)
//!
//! Init sequence derived from:
//!   - ESP-Miner BM1366 driver (esp-miner-asic-driver-analysis.md Section 7)
//!   - ASIC Register Bible Section 8 (BM1366 register map)
//!   - ASIC Register Bible Section 18 (BM1366 complete init sequence)

use crate::drivers::{ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1366 chip ID.
pub const CHIP_ID: u16 = 0x1366;

/// BM1366 default chips per chain (S19 XP).
pub const DEFAULT_CHIPS_PER_CHAIN_S19XP: u8 = 110;

/// BM1366 chips per chain (S19k Pro).
pub const DEFAULT_CHIPS_PER_CHAIN_S19K: u8 = 77;

/// BM1366 response size (11 bytes: nonce + midstate_num + job_id + version + flags).
pub const RESPONSE_BYTES: usize = 11;

/// BM1366 work size for FPGA WORK_TX FIFO.
///
/// The BM1366 uses the FPGA in BM139X mode with MIDSTATE_CNT=2 (4 midstate slots).
/// The FPGA WORK_TX format is: 4 header words + 4 x 8 midstate words = 36 words.
///
/// However, the BM1366 does on-chip version rolling — it computes its own midstates
/// from the full block header. The FPGA still requires the 36-word format, so we
/// pack the full header data into the FPGA work format. The FPGA serializes this
/// over UART using the BM139X job packet format (header 0x21, length 0x56 = 86).
///
/// For the BM1366, the FPGA's 4 midstate slots are filled with the same midstate
/// (computed from the block header). The chip's internal version roller generates
/// different versions autonomously.
pub const WORK_WORDS: usize = 36;

/// Number of midstate slots in the FPGA work format (BM139X mode, MIDSTATE_CNT=2).
const NUM_MIDSTATES: usize = 4;

/// Log2 of NUM_MIDSTATES — used to shift work_id for FPGA encoding.
const MIDSTATE_CNT_LOG2: u32 = 2;

/// Number of SHA-256 cores per BM1366 chip.
/// 112 large cores x ~8 small cores = 894 effective cores.
const NUM_CORES_ON_CHIP: u32 = 894;

/// BM1366 register addresses.
pub mod regs {
    /// Chip address register (contains ChipID=0x1366 in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL0 Parameter — hash clock PLL.
    pub const PLL0: u8 = 0x08;
    /// Hash Counting Number — nonce range partition per chip count.
    pub const HASH_COUNTING: u8 = 0x10;
    /// Ticket Mask — hardware difficulty filter.
    pub const TICKET_MASK: u8 = 0x14;
    /// Misc Control — UART config, GPIO.
    pub const MISC_CONTROL: u8 = 0x18;
    /// Fast UART Configuration — baud rate control (BT8D, BCLK_SEL).
    pub const FAST_UART: u8 = 0x28;
    /// UART Relay register.
    pub const UART_RELAY: u8 = 0x2C;
    /// Core Register Control — indirect core register access.
    pub const CORE_REG_CTRL: u8 = 0x3C;
    /// Core Register Value — core readback.
    pub const CORE_REG_VALUE: u8 = 0x40;
    /// External Temperature Sensor Read.
    pub const TEMP_SENSOR: u8 = 0x44;
    /// Nonce Error Counter.
    pub const NONCE_ERROR: u8 = 0x4C;
    /// Analog Mux Control — temperature diode mux.
    pub const ANALOG_MUX: u8 = 0x54;
    /// IO Driver Strength.
    pub const IO_DRIVER: u8 = 0x58;
    /// PLL1 Parameter — baud clock PLL.
    pub const PLL1: u8 = 0x60;
    /// Init control register (Reg_A8).
    pub const REG_A8: u8 = 0xA8;
    /// Version Rolling — version mask + enable (MID_AUTO_GEN).
    pub const VERSION_ROLLING: u8 = 0xA4;
}

/// BM1366 register init values from ESP-Miner (Section 7.2 and 15.1).
mod init_values {
    /// Version rolling register: EN=1, bit28=1, MASK=0xFFFF.
    /// Written as 0x9000FFFF (for version_mask 0x1FFFE000 >> 13 = 0xFFFF).
    pub const VERSION_ROLLING: u32 = 0x9000_FFFF;

    /// Reg_A8 broadcast init value.
    pub const REG_A8_BCAST: u32 = 0x0007_0000;

    /// Reg_A8 per-chip init value.
    pub const REG_A8_PER_CHIP: u32 = 0x0007_01F0;

    /// Misc Control broadcast init value.
    pub const MISC_CTRL_BCAST: u32 = 0xFF0F_C100;

    /// Misc Control per-chip init value.
    pub const MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;

    /// Core Register Control (reg 0x3C): Hash Clock Ctrl (broadcast).
    pub const CORE_REG_HASH_CLOCK: u32 = 0x8000_8540;

    /// Core Register Control (reg 0x3C): Clock Delay Ctrl (broadcast).
    /// RESOLVED 2026-06-10 from the symbolized S21 jig `set_clock_delay_control`:
    /// `0x80008000 | ((pwth_sel & 7) << 3) | (ccdly_sel << 6) | swpf`. This value
    /// = `0x20` = **pwth_sel=4**, ccdly=0, swpf=0 — matches the S19k Pro factory
    /// `Config.ini` (pwth_sel=4). (NB `bm1368.rs` uses pwth_sel=3 / 0x18 from
    /// ESP-Miner — a flagged intra-family inconsistency; bm1368's is the
    /// `a lab unit`-proven value, this one is factory-matching.)
    pub const CORE_REG_CLOCK_DELAY: u32 = 0x8000_8020;

    /// Core Register Control (reg 0x3C): per-chip AsicBoost / version-rolling
    /// enable. RESOLVED 2026-06-10 — was "unknown purpose"; the S21 jig + the
    /// `bm1370.rs::CORE_REG_3` doc agree this `0x800082AA` is the per-chip
    /// CoreRegCtrl write common to all BM1366+ (enables version-rolling cores).
    pub const CORE_REG_UNKNOWN: u32 = 0x8000_82AA;

    /// Analog Mux Control: enable temp diode.
    pub const ANALOG_MUX: u32 = 0x0000_0003;

    /// IO Driver Strength.
    pub const IO_DRIVER: u32 = 0x0211_1111;

    /// UART Relay (chip 0 only).
    pub const UART_RELAY: u32 = 0x007C_0003;

    /// Hash Counting Number — S19 XP stock default.
    pub const HASH_COUNTING_S19XP: u32 = 0x0000_151C;

    /// Hash Counting Number — S19k Pro stock default.
    pub const HASH_COUNTING_S19K: u32 = 0x0000_115A;

    /// Hash Counting Number — S19 XP **LuxOS-tuned** value (RE-DERIVED,
    /// `mining-bible-v1/_canonical/asic-protocol-bible.md` §11 "S19XP Luxos | 0x00001446 | tuned").
    /// NOT a chip-count-derived stock default — it is a per-firmware tuning value LuxOS
    /// programs into reg 0x10 for the 110-chip S19 XP nonce-range. Exposed so an operator can
    /// match LuxOS's tuned nonce partitioning via `DCENT_BM1366_HASH_COUNTING=0x1446`. Never the
    /// compiled default (see `resolve_hash_counting`).
    pub const HASH_COUNTING_S19XP_LUXOS: u32 = 0x0000_1446;

    /// Fast UART config for 1 Mbps baud.
    pub const FAST_UART_1MBPS: u32 = 0x1130_0200;

    /// Ticket mask for difficulty 256.
    pub const TICKET_MASK_256: u32 = 0x0000_00FF;
}

/// PARITY (RE 2026-06-02): resolve the effective BM1366 HashCountingNumber (reg 0x10)
/// from an optional operator override string (the raw `DCENT_BM1366_HASH_COUNTING` value).
///
/// Pure helper so host tests can pin BOTH states without env mutation, mirroring
/// `bm1368::resolve_fast_uart_value`:
///   - `None` / empty / unparseable → the caller's `stock` value (the chip-count-derived
///     `HASH_COUNTING_S19K`/`HASH_COUNTING_S19XP` stock default — never silently changed).
///   - `Some("0x1446")` → the LuxOS-tuned `HASH_COUNTING_S19XP_LUXOS`, or any other
///     operator-validated value.
///
/// Accepts decimal or `0x`/`0X`-prefixed hex. A malformed override falls back to `stock` so a
/// typo can never reprogram the chain's nonce partitioning.
pub fn resolve_hash_counting(stock: u32, override_raw: Option<&str>) -> u32 {
    let Some(raw) = override_raw else {
        return stock;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return stock;
    }
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        trimmed.parse::<u32>()
    };
    parsed.unwrap_or(stock)
}

/// BM1366 PLL frequency computation.
///
/// PLL formula: freq = 25 MHz * FB_DIV / (REF_DIV * POSTDIV1 * POSTDIV2)
///
/// Register 0x08 format (BM1366):
///   Byte 0: VDO_SCALE (0x40 if VCO < 2400 MHz, 0x50 if >= 2400 MHz)
///   Byte 1: FB_DIV (feedback divider, range 144-235)
///   Byte 2: REF_DIV (reference divider, typically 1 or 2)
///   Byte 3: POSTDIV encoded as ((POSTDIV1-1) << 4) | (POSTDIV2-1)
///
/// The search algorithm finds optimal PLL parameters by brute-force,
/// matching ESP-Miner's pll_get_parameters() logic.
fn bm1366_pll_calc(target_mhz: u16) -> (u32, u16) {
    const FREQ_MULT: f64 = 25.0;
    const FB_DIV_MIN: u16 = 144;
    const FB_DIV_MAX: u16 = 235;

    let target = target_mhz as f64;
    let mut best_freq = 0.0f64;
    let mut best_fb = FB_DIV_MIN;
    let mut best_ref = 1u8;
    let mut best_pd1 = 1u8;
    let mut best_pd2 = 1u8;
    let mut best_vco = f64::MAX;

    for refdiv in [1u8, 2] {
        for postdiv1 in 1..=7u8 {
            for postdiv2 in 1..=postdiv1 {
                for fb_div in FB_DIV_MIN..=FB_DIV_MAX {
                    let freq = FREQ_MULT * fb_div as f64
                        / (refdiv as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target).abs();
                    let best_diff = (best_freq - target).abs();
                    let vco = FREQ_MULT * fb_div as f64 / refdiv as f64;

                    if diff < best_diff
                        || (diff == best_diff && vco < best_vco)
                        || (diff == best_diff
                            && vco == best_vco
                            && (postdiv1 as u16 * postdiv2 as u16)
                                < (best_pd1 as u16 * best_pd2 as u16))
                    {
                        best_freq = freq;
                        best_fb = fb_div;
                        best_ref = refdiv;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                        best_vco = vco;
                    }
                }
            }
        }
    }

    // VCO scale byte
    let vdo_scale: u8 = if best_vco >= 2400.0 { 0x50 } else { 0x40 };

    // POSTDIV encoding: BM1366 uses (POSTDIV1-1) << 4 | (POSTDIV2-1)
    let postdiv_byte = ((best_pd1 - 1) << 4) | (best_pd2 - 1);

    // Register value: [VDO_SCALE, FB_DIV, REF_DIV, POSTDIV]
    let reg_value = (vdo_scale as u32) << 24
        | (best_fb as u32) << 16
        | (best_ref as u32) << 8
        | (postdiv_byte as u32);

    let actual_mhz = best_freq.round() as u16;
    (reg_value, actual_mhz)
}

/// BM1366 driver implementation.
pub struct Bm1366Driver;

impl Default for Bm1366Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm1366Driver {
    pub fn new() -> Self {
        Self
    }

    /// Calculate WORK_TIME register value for a given frequency.
    ///
    /// For BM1366, with on-chip version rolling, the effective nonce space per
    /// work item is much larger (version bits multiply the nonce space).
    /// Formula: work_time = 0.9 * midstate_count * 2^19 / freq_Hz * FPGA_WORK_CLK
    ///
    /// The FPGA work_time counter runs at 100 MHz (200 MHz fabric clock / 2).
    pub fn calculate_work_time(freq_mhz: u16, midstate_count: u32) -> u32 {
        // Delegate to the shared BM139x helper (this body was byte-identical to
        // it). Matches the bm1397/bm1398 pattern; removes a duplicated copy.
        crate::drivers::bm139x::calculate_work_time(freq_mhz, midstate_count)
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
            .find(|&freq| Bm1366Driver::new().pll_params(freq).reg_value == masked)
    }

    /// Helper: broadcast write a register to all chips via CMD_TX_FIFO.
    fn write_reg_broadcast(chain: &mut FpgaChain, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_bcast_full(reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }

    /// Helper: write a register to a specific chip via CMD_TX_FIFO.
    fn write_reg_single(chain: &mut FpgaChain, chip_addr: u8, reg: u8, value: u32) {
        let (w0, w1) = crate::protocol::fifo_cmd_write_reg_full(chip_addr, reg, value);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
    }
}

impl ChipDriver for Bm1366Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1366"
    }

    fn cores_per_chip(&self) -> u32 {
        NUM_CORES_ON_CHIP
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
            "BM1366: Configuring {} chips at {} MHz (enumeration and addressing already complete)",
            chip_count,
            freq_mhz,
        );

        // =====================================================================
        // BM1366 Init Sequence (from ESP-Miner Section 7.2 and Register Bible Section 18)
        //
        // Enumeration and address assignment is done BEFORE init_chain() is called.
        // This function handles: register configuration, PLL setup, baud upgrade.
        //
        // ESP-Miner init order:
        //   1. Version mask (3x)           — register 0xA4
        //   2. Reg_A8 broadcast            — register 0xA8
        //   3. Misc Control broadcast      — register 0x18
        //   4. Core Register Control (2x)  — register 0x3C
        //   5. Ticket Mask                 — register 0x14
        //   6. Analog Mux                  — register 0x54
        //   7. IO Driver Strength          — register 0x58
        //   8. UART Relay (chip 0)         — register 0x2C
        //   9. Per-chip config (5 regs each)
        //  10. Frequency ramp              — register 0x08
        //  11. Hash Counting Number        — register 0x10
        //  12. Final version mask          — register 0xA4
        //  13. Fast UART (baud upgrade)    — register 0x28
        // =====================================================================

        // Step 0: Reset ASIC baud to default if hot start (same pattern as BM1387).
        let current_baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);
        if current_baud_div != fpga_chain::BAUD_REG_115200 {
            // Send MiscCtrl at current baud to reset ASICs to 115200.
            Self::write_reg_broadcast(chain, regs::MISC_CONTROL, init_values::MISC_CTRL_BCAST);
            std::thread::sleep(std::time::Duration::from_millis(10));
            tracing::info!(
                chain_id = chain.chain_id,
                current_baud_div = format_args!("0x{:02X}", current_baud_div),
                "Hot start baud reset: sent MiscCtrl at current baud — ASICs switching to default",
            );
        }

        // Step 1: Set FPGA baud to 115200 for configuration commands.
        chain.set_baud(fpga_chain::BAUD_REG_115200);
        tracing::debug!("FPGA baud set to 115200 (BAUD_REG=0x6C)");

        // Step 2: Set version mask (3 times, per ESP-Miner).
        // Register 0xA4 = 0x9000FFFF: EN=1, bit28=1, MASK=0xFFFF.
        for _ in 0..3 {
            Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, init_values::VERSION_ROLLING);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        tracing::info!("Version rolling enabled (0xA4 = 0x9000FFFF, sent 3x)");

        // Step 3: Reg_A8 broadcast = 0x00070000.
        Self::write_reg_broadcast(chain, regs::REG_A8, init_values::REG_A8_BCAST);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Step 4: Misc Control broadcast = 0xFF0FC100.
        Self::write_reg_broadcast(chain, regs::MISC_CONTROL, init_values::MISC_CTRL_BCAST);
        std::thread::sleep(std::time::Duration::from_millis(5));
        tracing::info!("MiscCtrl broadcast = 0xFF0FC100");

        // Step 5: Core Register Control — Hash Clock Ctrl.
        Self::write_reg_broadcast(chain, regs::CORE_REG_CTRL, init_values::CORE_REG_HASH_CLOCK);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Step 6: Core Register Control — Clock Delay Ctrl.
        Self::write_reg_broadcast(
            chain,
            regs::CORE_REG_CTRL,
            init_values::CORE_REG_CLOCK_DELAY,
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        tracing::info!("Core registers configured (0x3C: 0x80008540, 0x80008020)");

        // Step 7: Ticket Mask (difficulty 256).
        let mask = self.ticket_mask(256);
        Self::write_reg_broadcast(chain, regs::TICKET_MASK, mask);
        std::thread::sleep(std::time::Duration::from_millis(5));
        tracing::info!(
            mask = format_args!("0x{:08X}", mask),
            "TicketMask set for difficulty 256",
        );

        // Step 8: Analog Mux Control = 0x00000003 (enable temp diode).
        Self::write_reg_broadcast(chain, regs::ANALOG_MUX, init_values::ANALOG_MUX);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Step 9: IO Driver Strength = 0x02111111.
        Self::write_reg_broadcast(chain, regs::IO_DRIVER, init_values::IO_DRIVER);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Step 10: UART Relay = 0x007C0003 (chip 0 only).
        Self::write_reg_single(chain, 0x00, regs::UART_RELAY, init_values::UART_RELAY);
        std::thread::sleep(std::time::Duration::from_millis(5));

        tracing::info!("Broadcast register config complete, starting per-chip init");

        // Step 11: Per-chip register configuration.
        // Each chip gets: Reg_A8, MiscCtrl, CoreReg x3.
        let addr_interval = if chip_count > 0 {
            256u16 / chip_count as u16
        } else {
            256
        };
        for i in 0..chip_count {
            let addr = (i as u16 * addr_interval) as u8;

            // Reg_A8 per-chip = 0x000701F0
            Self::write_reg_single(chain, addr, regs::REG_A8, init_values::REG_A8_PER_CHIP);

            // MiscCtrl per-chip = 0xF000C100
            Self::write_reg_single(
                chain,
                addr,
                regs::MISC_CONTROL,
                init_values::MISC_CTRL_PER_CHIP,
            );

            // Core Register: Hash Clock Ctrl
            Self::write_reg_single(
                chain,
                addr,
                regs::CORE_REG_CTRL,
                init_values::CORE_REG_HASH_CLOCK,
            );

            // Core Register: Clock Delay Ctrl
            Self::write_reg_single(
                chain,
                addr,
                regs::CORE_REG_CTRL,
                init_values::CORE_REG_CLOCK_DELAY,
            );

            // Core Register: per-chip AsicBoost/version-rolling enable (0x800082AA)
            // (resolved 2026-06-10 from the S21 jig — see init_values::CORE_REG_UNKNOWN doc)
            Self::write_reg_single(
                chain,
                addr,
                regs::CORE_REG_CTRL,
                init_values::CORE_REG_UNKNOWN,
            );

            // Small delay between chips (ESP-Miner has no explicit delay for BM1366,
            // unlike BM1368 which has 500ms per chip).
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        tracing::info!(
            chip_count = chip_count,
            "Per-chip register configuration complete (5 registers x {} chips)",
            chip_count,
        );

        // Step 12: Frequency ramp from 50 MHz to target.
        // ESP-Miner ramps in 6.25 MHz steps with 100ms delay between steps.
        let mut current_freq: f64 = 50.0;
        let target_freq = freq_mhz as f64;
        let step = 6.25;

        tracing::info!(
            target_mhz = freq_mhz,
            "Starting frequency ramp: 50 MHz -> {} MHz (6.25 MHz steps)",
            freq_mhz,
        );

        while current_freq < target_freq - 0.1 {
            current_freq = (current_freq + step).min(target_freq);
            let (pll_reg, _actual) = bm1366_pll_calc(current_freq as u16);
            Self::write_reg_broadcast(chain, regs::PLL0, pll_reg);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Final PLL write at target frequency (ensure we hit it exactly).
        let pll = self.pll_params(freq_mhz);
        Self::write_reg_broadcast(chain, regs::PLL0, pll.reg_value);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!(
            freq_mhz = freq_mhz,
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            "Frequency ramp complete — all chips at {} MHz",
            freq_mhz,
        );

        // Step 13: Set WORK_TIME in FPGA.
        let work_time = Self::calculate_work_time(freq_mhz, NUM_MIDSTATES as u32);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            work_time = format_args!("0x{:08X}", work_time),
            "WORK_TIME set for {} MHz, {} midstates",
            freq_mhz,
            NUM_MIDSTATES,
        );

        // Step 14: Hash Counting Number controls nonce range partitioning across the chain.
        // The stock defaults differ between the 110-chip S19 XP and the 77-chip S19k Pro.
        let stock_hash_counting = if chip_count <= DEFAULT_CHIPS_PER_CHAIN_S19K {
            init_values::HASH_COUNTING_S19K
        } else {
            init_values::HASH_COUNTING_S19XP
        };
        // PARITY (RE 2026-06-02): allow an operator override (e.g. the LuxOS-tuned
        // `HASH_COUNTING_S19XP_LUXOS = 0x0000_1446`) via `DCENT_BM1366_HASH_COUNTING`. When the
        // env is unset/empty/malformed, `resolve_hash_counting` returns the stock count-derived
        // value byte-identical — the live nonce partitioning is never silently changed.
        let hash_counting = resolve_hash_counting(
            stock_hash_counting,
            std::env::var("DCENT_BM1366_HASH_COUNTING").ok().as_deref(),
        );
        Self::write_reg_broadcast(chain, regs::HASH_COUNTING, hash_counting);
        std::thread::sleep(std::time::Duration::from_millis(5));
        tracing::info!(
            chip_count,
            hash_counting = format_args!("0x{:08X}", hash_counting),
            "Hash Counting Number set for {}-chip BM1366 chain",
            chip_count,
        );

        // Step 15: Final version mask write.
        Self::write_reg_broadcast(chain, regs::VERSION_ROLLING, init_values::VERSION_ROLLING);
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Step 16: Baud upgrade to 1 Mbps via Fast UART (register 0x28).
        // ESP-Miner: Reg 0x28 = 0x11300200 for 1 Mbps.
        Self::write_reg_broadcast(chain, regs::FAST_UART, init_values::FAST_UART_1MBPS);
        std::thread::sleep(std::time::Duration::from_millis(50));
        tracing::info!("Fast UART set to 0x11300200 — ASICs switching to 1 Mbps");

        // Step 17: Switch FPGA baud to match (1 Mbps).
        // FPGA baud = 200 MHz / (16 * (div + 1)) -> div = 200M / (16M) - 1 = 11.5 -> 11
        // 200M / (16 * 12) = 1,041,666 bps (closest to 1 Mbps).
        const BAUD_REG_1MBPS: u32 = 0x0B; // 200M / (16 * 12) = 1,041,667 bps
        chain.set_baud(BAUD_REG_1MBPS);
        std::thread::sleep(std::time::Duration::from_millis(100));
        tracing::info!("FPGA baud set to ~1 Mbps (BAUD_REG=0x0B) — matches ASIC fast UART");

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1366: Chain configuration complete — {} chips at {} MHz, version rolling enabled",
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
            "BM1366: Setting frequency"
        );

        if chip_addr == 0xFF {
            // Broadcast to all chips
            Self::write_reg_broadcast(chain, regs::PLL0, pll.reg_value);
        } else {
            // Single chip
            Self::write_reg_single(chain, chip_addr, regs::PLL0, pll.reg_value);
        }

        // Wait for PLL to lock (~10ms typical)
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
                    "BM1366 PLL0 readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1366 PLL0 readback timed out for chip 0x{:02X}",
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
        // BM1366 work via FPGA WORK_TX FIFO.
        //
        // The BM1366 natively accepts full block header jobs (82 bytes) with on-chip
        // version rolling. However, the Braiins s9io FPGA expects work in its standard
        // format: 4 header words + N x 8 midstate words, where N comes from the
        // runtime FPGA MIDSTATE_CNT. Passthrough can preserve bosminer's 8-slot mode.
        //
        // The FPGA serializes this over UART as a BM139X job packet. The BM1366
        // receives the job and computes midstates internally.
        //
        // We pack the work data into the FPGA's expected format:
        //   Word 0:       Extended Work ID (shifted left by MIDSTATE_CNT_LOG2)
        //   Word 1:       nbits (32-bit LE)
        //   Word 2:       ntime (32-bit LE)
        //   Word 3:       merkle_tail (last 4 bytes of merkle root, LE)
        //   Words 4-11:   midstate 0 (reversed word order)
        //   Words 12-19:  midstate 1 (duplicate)
        //   Words 20-27:  midstate 2 (duplicate)
        //   Words 28-35:  midstate 3 (duplicate)
        //
        // Since the BM1366 does on-chip version rolling, all 4 midstate slots
        // contain the same midstate. The chip rolls versions autonomously and
        // reports the version bits used in the nonce response.

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

        // Word 1: nbits
        words[1] = work.nbits;

        // Word 2: ntime
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root)
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Encode midstate in REVERSED word order for FPGA.
        // Same encoding as BM1387 — DO NOT add .swap_bytes().
        // See BM1387 driver comments for full proof of byte ordering.
        let midstate = &work.midstates[0];
        let mut ms_words = [0u32; 8];
        for (i, ms_word) in ms_words.iter_mut().enumerate() {
            let word_idx = 7 - i;
            *ms_word = u32::from_be_bytes([
                midstate[word_idx * 4],
                midstate[word_idx * 4 + 1],
                midstate[word_idx * 4 + 2],
                midstate[word_idx * 4 + 3],
            ]); // NO .swap_bytes() — proven correct by first accepted shares (2026-03-17)
        }

        // Copy the same midstate into every active FPGA slot.
        for slot in 0..num_slots {
            let base = 4 + slot * 8;
            words[base..base + 8].copy_from_slice(&ms_words);
        }

        // Write to WORK TX FIFO
        chain.write_work(&words[..work_words]);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1366 nonce response (from WORK_RX_FIFO, 2 x 32-bit words):
        //
        // The FPGA reads the 11-byte ASIC response and packs it into 2 words:
        //   Word 0: nonce value (32-bit)
        //   Word 1: [CRC:8 | extended_work_id:16 | solution_index:8]
        //
        // With MIDSTATE_CNT=2, the extended_work_id contains:
        //   - Low 2 bits: midstate index (FPGA slot, always 0 for BM1366)
        //   - Remaining bits: original work_id (shifted left by 2 in send_work)
        //
        // BM1366 nonce encoding (from ESP-Miner):
        //   bits[31:25] = core_id (7 bits, 112 large cores)
        //   bits[24:17] = chip address (8 bits)
        //   bits[16:0]  = actual nonce value (17 bits)
        //
        // Job ID extraction: job_id = byte7 & 0xF8 (upper 5 bits)
        // Small core ID:     small_core_id = byte7 & 0x07 (lower 3 bits, 8 small cores)
        //
        // Version bits from response bytes 8-9:
        //   version_bits = ntohs(VH:VL) << 13
        //   These are stored in the solution_id field by the FPGA for BM139X chips.
        let nonce = raw[0];
        let w1 = raw[1];

        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
        let work_id = hw_work_id >> MIDSTATE_CNT_LOG2;
        let midstate_idx = (hw_work_id & ((1 << MIDSTATE_CNT_LOG2) - 1)) as u8;

        // Chip index from nonce bits [24:17] / address_interval.
        // For now, extract raw chip address; the caller normalizes by chip count.
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
        // BM1366: BM139X mode (bit4=1), ENABLE (bit3=1), MIDSTATE_CNT=2 (bits2:1=10)
        // Same as BM1397 — the FPGA treats all BM139X-era chips the same way.
        fpga_chain::CTRL_BM139X | fpga_chain::CTRL_ENABLE | (2 << fpga_chain::CTRL_MIDSTATE_SHIFT)
    }

    fn job_interval_ms(&self, chip_count: u8, _freq_mhz: u16) -> u32 {
        // BM1366: 2000ms / chip_count (from ESP-Miner).
        // The on-chip version rolling means each chip searches a much larger
        // space per job, so jobs last longer than on BM1387.
        if chip_count > 0 {
            (2000 / chip_count as u32).max(1)
        } else {
            100
        }
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BM1366 uses simple (difficulty - 1) as ticket mask, same as BM1397/BM1368/BM1370.
        // ESP-Miner BM1366 driver writes the mask value directly without bit reversal.
        // Only BM1387 uses .reverse_bits().swap_bytes() encoding.
        // For difficulty 256: mask = 255 = 0x000000FF.
        difficulty.saturating_sub(1)
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        let (reg_value, _actual_freq) = bm1366_pll_calc(freq_mhz);

        // Extract components from register value for informational purposes
        let fb_div = ((reg_value >> 16) & 0xFF) as u16;
        let ref_div = ((reg_value >> 8) & 0xFF) as u8;
        let postdiv_byte = (reg_value & 0xFF) as u8;
        let post_div1 = ((postdiv_byte >> 4) & 0x0F) + 1;
        let post_div2 = (postdiv_byte & 0x0F) + 1;

        PllConfig {
            fb_div,
            ref_div,
            post_div1,
            post_div2,
            reg_value,
        }
    }
}

/// Get the sorted list of common PLL frequencies the BM1366 can generate (MHz).
///
/// BM1366 uses a dynamic PLL search (not a fixed table), but the autotuner
/// needs a discrete frequency list for binary search. These are the commonly
/// used frequencies within the BM1366's operating range.
pub fn pll_frequencies() -> &'static [u16] {
    &[
        350, 375, 400, 425, 450, 475, 500, 525, 550, 575, 600, 625, 650, 675, 700, 725, 750,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn decode_nonce_never_panics_on_arbitrary_fifo_words(raw in any::<[u32; 2]>()) {
            let drv = Bm1366Driver::new();
            let decoded = drv.decode_nonce(&raw);
            prop_assert!(decoded.is_ok());
        }
    }

    #[test]
    fn chip_identity() {
        let drv = Bm1366Driver::new();
        assert_eq!(drv.chip_id(), 0x1366);
        assert_eq!(drv.chip_name(), "BM1366");
        assert_eq!(drv.response_length(), 11);
        assert_eq!(drv.max_baud(), 1_000_000);
    }

    #[test]
    fn ticket_mask_basic() {
        let drv = Bm1366Driver::new();
        assert_eq!(drv.ticket_mask(256), 0xFF);
        assert_eq!(drv.ticket_mask(128), 0x7F);
        assert_eq!(drv.ticket_mask(1), 0);
    }

    #[test]
    fn hash_counting_values_and_override_resolver() {
        // PARITY (RE 2026-06-02): pin all three documented BM1366 HashCounting (reg 0x10)
        // values + the operator-override resolver semantics.
        assert_eq!(init_values::HASH_COUNTING_S19K, 0x0000_115A); // S19k Pro stock (≤77 chips)
        assert_eq!(init_values::HASH_COUNTING_S19XP, 0x0000_151C); // S19 XP stock (110 chips)
        assert_eq!(init_values::HASH_COUNTING_S19XP_LUXOS, 0x0000_1446); // S19 XP LuxOS-tuned

        let stock = init_values::HASH_COUNTING_S19XP;
        // Unset / empty / whitespace / malformed → stock value byte-identical (never reprogram).
        assert_eq!(resolve_hash_counting(stock, None), stock);
        assert_eq!(resolve_hash_counting(stock, Some("")), stock);
        assert_eq!(resolve_hash_counting(stock, Some("   ")), stock);
        assert_eq!(resolve_hash_counting(stock, Some("0xZZZZ")), stock);
        assert_eq!(resolve_hash_counting(stock, Some("not-a-number")), stock);
        // Explicit override (hex, 0X, decimal) → parsed value, e.g. the LuxOS-tuned 0x1446.
        assert_eq!(
            resolve_hash_counting(stock, Some("0x1446")),
            init_values::HASH_COUNTING_S19XP_LUXOS
        );
        assert_eq!(resolve_hash_counting(stock, Some("0X1446")), 0x1446);
        assert_eq!(resolve_hash_counting(stock, Some("5190")), 0x1446); // 0x1446 == 5190 dec
        assert_eq!(resolve_hash_counting(stock, Some("  0x1446  ")), 0x1446);
    }

    #[test]
    fn bm1366_matches_clean_room_re_reference() {
        // RE 2026-06-02 cross-check (no live hardware / no Ghidra): confirm DCENT's BM1366 init
        // values agree with the independent open-source clean-room RE register map
        //. Hardens S19k Pro / S19 XP (BM1366)
        // coverage by pinning the driver to the open reference.
        assert_eq!(CHIP_ID, 0x1366); // clean ref: CHIP_ID = 0x1366
        assert_eq!(regs::MISC_CONTROL, 0x18); // clean ref: Misc Control addr 0x18 (reset 0x0000_C100)
        assert_eq!(regs::VERSION_ROLLING, 0xA4); // clean ref: Version Rolling addr 0xA4 (reset 0x0000_FFFF)
        assert_eq!(regs::REG_A8, 0xA8); // clean ref: Reg_A8 addr 0xA8 (reset 0x0007_0000)
                                        // Reg_A8 broadcast init equals the clean-ref reset value exactly.
        assert_eq!(init_values::REG_A8_BCAST, 0x0007_0000);
        // Version-rolling VERSION_MASK low-16 = 0xFFFF (clean-ref reset 0x0000_FFFF; full word 0x9000_FFFF).
        assert_eq!(init_values::VERSION_ROLLING & 0x0000_FFFF, 0x0000_FFFF);
    }

    #[test]
    fn ctrl_reg_bm139x_midstate2() {
        let drv = Bm1366Driver::new();
        let ctrl = drv.ctrl_reg_value();
        assert!(ctrl & fpga_chain::CTRL_BM139X != 0);
        assert!(ctrl & fpga_chain::CTRL_ENABLE != 0);
        // MIDSTATE_CNT=2 lives in the S9 midstate shift field.
        assert_eq!((ctrl >> fpga_chain::CTRL_MIDSTATE_SHIFT) & 0x3, 2);
    }

    #[test]
    fn pll_register_to_freq_round_trips_known_frequencies() {
        // verify_frequency() reads back PLL0 and decodes via pll_register_to_freq.
        // Pin that the decode is the exact inverse of pll_params().reg_value for
        // every common frequency, and that the PLL lock bit (MSB) is masked off
        // before the lookup. Read-only PLL-lock-verification correctness check.
        let drv = Bm1366Driver::new();
        for &f in MinerProfile::pll_frequencies_for_chip(CHIP_ID) {
            let reg = drv.pll_params(f).reg_value;
            assert_eq!(
                Bm1366Driver::pll_register_to_freq(reg),
                Some(f),
                "bare PLL0 readback 0x{:08X} must decode to {} MHz",
                reg,
                f
            );
            assert_eq!(
                Bm1366Driver::pll_register_to_freq(reg | 0x8000_0000),
                Some(f),
                "locked PLL0 readback for {} MHz must mask bit31 before lookup",
                f
            );
        }
        // Unknown register → None (verify_frequency surfaces an error, not OK).
        assert_eq!(Bm1366Driver::pll_register_to_freq(0x0000_0000), None);
    }
}
