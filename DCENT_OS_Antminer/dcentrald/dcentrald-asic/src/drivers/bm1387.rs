//! BM1387 ASIC driver (Antminer S9).
//!
//! The BM1387 is the first driver implementation. We have complete register
//! dumps from live hardware and the initialization sequence is well-understood.
//!
//! Key characteristics:
//!   - 16nm process
//!   - ~0.4V core voltage
//!   - 9-byte response (2 x 32-bit words)
//!   - 1-midstate job format (12 words = 48 bytes per work item)
//!   - No version rolling (single midstate)
//!   - Maximum 6 Mbps baud (via FPGA)
//!   - 63 chips per chain on S9
//!
//! Register values from live dump (chip 0):
//!   0x00 ChipAddress:  0x13879000 (ID=0x1387, addr=0x00)
//!   0x0C PLL:          0x80400222 (default/reset PLL config)
//!   0x1C MiscControl:  0x40201A00 (baud_div=26, inv_clock=1, mmen=0)

use crate::drivers::{ChipDriver, MinerProfile, MiningWork, NonceResult, PllConfig};
use crate::pic::PicController;
use crate::Result;
use dcentrald_hal::fpga_chain::{self, FpgaChain};

/// BM1387 chip ID.
pub const CHIP_ID: u16 = 0x1387;

/// BM1387 default chips per chain (S9).
pub const DEFAULT_CHIPS_PER_CHAIN: u8 = 63;

/// BM1387 response size (2 x 32-bit words = 8 bytes payload).
pub const RESPONSE_WORDS: usize = 2;

/// BM1387 work size with MIDSTATE_CNT=2 (4 midstates):
/// 4 header words + 4 x 8 midstate words = 36 words = 144 bytes.
/// The s9io FPGA requires MIDSTATE_CNT=2 (CTRL_REG=0x0C), so we send
/// 4 copies of the same midstate. The FPGA cycles through them, and
/// nonce responses include solution_id indicating which was used.
pub const WORK_WORDS: usize = 36;

/// Number of midstate slots in the FPGA work format.
const NUM_MIDSTATES: usize = 4;

/// Log2 of NUM_MIDSTATES — used to shift work_id for FPGA encoding.
/// BraiinsOS pattern: work_id = (counter << MIDSTATE_CNT_LOG2) | midstate_idx
const MIDSTATE_CNT_LOG2: u32 = 2;

const MISC_CTRL_MMEN: u32 = 1 << 7;
const MISC_CTRL_GATE_BLOCK: u32 = 1 << 15;
const MISC_CTRL_NOT_SET_BAUD: u32 = 1 << 30;

/// MiscCtrl write that gates cores, enables multi-midstate/AsicBoost mode,
/// and switches BM1387 ASICs to 1.5625M baud.
pub const BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST: u32 = 0x0020_8180;

/// MiscCtrl confirmation write at 1.5625M baud. Keeps AsicBoost and gate_block
/// set while avoiding a second baud change.
pub const BM1387_MISC_CTRL_CONFIRM_GATE_ASICBOOST: u32 = 0x4020_8180;

/// BM1387 register addresses.
pub mod regs {
    /// Chip address register (contains ChipID in bits 31:16).
    pub const CHIP_ADDRESS: u8 = 0x00;
    /// PLL configuration register.
    pub const PLL: u8 = 0x0C;
    /// Misc control register (baud rate divider, clock settings).
    ///
    /// BM1387 = 0x1C. CROSS-CHIP NOTE: MISC_CONTROL migrated to **0x18** on
    /// BM1391 and every subsequent chip (BM1397/BM1370 all use 0x18). The catch
    /// is that BM1391 (Antminer S11) keeps the BM1387-byte-identical FIL command
    /// set — it routes through the BM1387-compat path with no 0x1391 chip-ID on
    /// the wire — yet places MISC_CONTROL at the BM1397 address. A future BM1391
    /// driver that inherits this BM1387 path MUST override MISC_CONTROL to 0x18;
    /// do NOT let it regress to 0x1C.
    /// Source: knowledge-base goldmine `findings/s9-registers-pll.md` R13/IC-2
    /// (`BM1391_set_baud@13280.c` in S17/single-board-test:
    /// `BM1391_set_config(chain, 0, 0x18, gBM1391_MISC_CONTROL_reg, 1)`);
    /// cross-chip register map in the same file confirms 0x1C → 0x18 at BM1391.
    pub const MISC_CONTROL: u8 = 0x1C;
    /// Ticket mask register (hardware difficulty filter).
    /// BM1387 = 0x18. NOTE: BM1397+ uses 0x14 — different register map!
    pub const TICKET_MASK: u8 = 0x18;
    /// I2C control register — passthrough to on-board temp sensor via chip 0.
    ///
    /// Format (32-bit, big-endian on wire):
    ///   Byte 3 [31:24]: flags — bit 31=BUSY, bit 24=DO_COMMAND
    ///   Byte 2 [23:16]: I2C slave address (8-bit: even=read, odd=write in BM1387 convention)
    ///   Byte 1 [15:8]:  I2C register number on the slave
    ///   Byte 0 [7:0]:   Data (read result or write payload)
    ///
    /// Source: BraiinsOS braiins_bm1387.rs I2cControlReg (REG_NUM = 0x20).
    pub const I2C_CONTROL: u8 = 0x20;
}

/// Known I2C addresses for on-board temperature sensors (8-bit format).
///
/// S9 hash boards have one temp sensor per board, located at one of these
/// addresses. BraiinsOS probes all three in order and uses the first that
/// responds.
///
/// Source: BraiinsOS braiins_sensor.rs SENSOR_I2C_ADDRESS.
const SENSOR_I2C_ADDRESSES: [u8; 3] = [0x98, 0x9A, 0x9C];

/// Sensor identification registers (SMBus standard).
const REG_MANUFACTURER_ID: u8 = 0xFE;
const REG_DEVICE_ID: u8 = 0xFF;

/// Temperature register — local sensor (die temp of the sensor chip itself).
/// Present on TMP451, ADT7461, NCT218, TMP42x.
const REG_LOCAL_TEMP: u8 = 0x00;

/// Temperature register — remote sensor (measures the BM1387 die via external diode).
/// Present on TMP451, ADT7461, NCT218.
const REG_REMOTE_TEMP: u8 = 0x01;

/// BM1387 PLL frequency lookup table.
///
/// Maps target frequency (MHz) to register 0x0C value.
///
/// PLL register layout (BM1387, same as BM1385/BM1485 family):
///   bits 23:16 = FBDIV (feedback divider)
///   bits 11:8  = REFDIV (reference divider)
///   bits 7:4   = POSTDIV1 (post divider 1)
///   bits 3:0   = POSTDIV2 (post divider 2)
///
/// Formula: freq = 25 MHz * FBDIV / REFDIV / POSTDIV1 / POSTDIV2
///
/// Values verified against:
///   - BraiinsOS braiins_bm1387.rs PllReg tests (500M=0x500221, 650M=0x680221)
///   - freq_pll_1485[] table (same PLL layout, confirmed 0x420221=412M, 0x500221=500M)
///
/// CRITICAL BUG FIX (2026-03-17): Previous table used bmminer-mix values with WRONG
/// frequency labels. 0x00420221 was labeled "500 MHz" but actually produces 412 MHz
/// (fbdiv=0x42=66, 25*66/2/2/1 = 412.5 MHz). This caused ~18% hashrate loss.
///
/// Format: (freq_mhz, pll_reg_value)
pub const BM1387_PLL_TABLE: &[(u16, u32)] = &[
    // Low frequencies: postdiv1=4, postdiv2=1 (÷4 total post-division)
    // freq = 25 * fbdiv / 2 / 4 / 1 = 3.125 * fbdiv
    (100, 0x0020_0241), // fbdiv=32:  25*32/2/4/1 = 100
    (125, 0x0028_0241), // fbdiv=40:  25*40/2/4/1 = 125
    (150, 0x0030_0241), // fbdiv=48:  25*48/2/4/1 = 150
    (175, 0x0038_0241), // fbdiv=56:  25*56/2/4/1 = 175
    (200, 0x0040_0241), // fbdiv=64:  25*64/2/4/1 = 200
    (225, 0x0048_0241), // fbdiv=72:  25*72/2/4/1 = 225
    (250, 0x0050_0241), // fbdiv=80:  25*80/2/4/1 = 250
    (275, 0x0058_0241), // fbdiv=88:  25*88/2/4/1 = 275
    (300, 0x0060_0241), // fbdiv=96:  25*96/2/4/1 = 300
    (325, 0x0068_0241), // fbdiv=104: 25*104/2/4/1 = 325
    (350, 0x0070_0241), // fbdiv=112: 25*112/2/4/1 = 350
    (375, 0x0078_0241), // fbdiv=120: 25*120/2/4/1 = 375
    // Mid frequencies: postdiv1=2, postdiv2=1 (÷2 total post-division)
    // freq = 25 * fbdiv / 2 / 2 / 1 = 6.25 * fbdiv
    (400, 0x0040_0221), // fbdiv=64:  25*64/2/2/1 = 400
    (425, 0x0044_0221), // fbdiv=68:  25*68/2/2/1 = 425
    (450, 0x0048_0221), // fbdiv=72:  25*72/2/2/1 = 450
    (462, 0x004A_0221), // fbdiv=74:  25*74/2/2/1 = 462.5
    (475, 0x004C_0221), // fbdiv=76:  25*76/2/2/1 = 475
    (500, 0x0050_0221), // fbdiv=80:  25*80/2/2/1 = 500  *** WAS 0x0042_0221 (=412M!) ***
    (525, 0x0054_0221), // fbdiv=84:  25*84/2/2/1 = 525
    (550, 0x0058_0221), // fbdiv=88:  25*88/2/2/1 = 550
    (575, 0x005C_0221), // fbdiv=92:  25*92/2/2/1 = 575
    (600, 0x0060_0221), // fbdiv=96:  25*96/2/2/1 = 600
    (625, 0x0064_0221), // fbdiv=100: 25*100/2/2/1 = 625
    (650, 0x0068_0221), // fbdiv=104: 25*104/2/2/1 = 650  (matches BraiinsOS test)
    // High frequencies: still postdiv1=2 up to 800 MHz (fbdiv max ~128 per BraiinsOS)
    // freq = 25 * fbdiv / 2 / 2 / 1 = 6.25 * fbdiv
    (700, 0x0070_0221), // fbdiv=112: 25*112/2/2/1 = 700
    (725, 0x0074_0221), // fbdiv=116: 25*116/2/2/1 = 725
    (750, 0x0078_0221), // fbdiv=120: 25*120/2/2/1 = 750
    (775, 0x007C_0221), // fbdiv=124: 25*124/2/2/1 = 775
    (800, 0x0080_0221), // fbdiv=128: 25*128/2/2/1 = 800
    // Very high frequencies: postdiv1=1, postdiv2=1 (÷1 total post-division)
    // freq = 25 * fbdiv / 2 / 1 / 1 = 12.5 * fbdiv
    // WARNING: fbdiv > 128 is outside BraiinsOS tested range
    (825, 0x0042_0211), // fbdiv=66:  25*66/2/1/1  = 825
    (850, 0x0044_0211), // fbdiv=68:  25*68/2/1/1  = 850
    (875, 0x0046_0211), // fbdiv=70:  25*70/2/1/1  = 875
    (900, 0x0048_0211), // fbdiv=72:  25*72/2/1/1  = 900
];

/// Look up the PLL register value for a target frequency.
///
/// Returns (pll_reg_value, actual_frequency_mhz).
/// If the exact frequency isn't in the table, the nearest entry is used.
fn bm1387_pll_lookup(target_mhz: u16) -> (u32, u16) {
    // Clamp to table range
    let target = target_mhz.clamp(100, 900);

    // Find the closest entry
    let mut best = BM1387_PLL_TABLE[0];
    let mut best_diff = (target as i32 - best.0 as i32).unsigned_abs();

    for &entry in &BM1387_PLL_TABLE[1..] {
        let diff = (target as i32 - entry.0 as i32).unsigned_abs();
        if diff < best_diff {
            best = entry;
            best_diff = diff;
        }
    }

    (best.1, best.0)
}

/// BM1387 driver implementation.
pub struct Bm1387Driver;

impl Default for Bm1387Driver {
    fn default() -> Self {
        Self::new()
    }
}

/// Number of SHA-256 cores per BM1387 chip.
/// Used by open-core init to send exactly one work item per core.
const NUM_CORES_ON_CHIP: u32 = 114;

/// Get the sorted list of discrete PLL frequencies the BM1387 can generate (MHz).
///
/// Use this instead of duplicating the frequency table. The autotuner uses
/// this for binary search bounds and step-down calculations.
pub fn pll_frequencies() -> &'static [u16] {
    // Extract just the MHz values from the PLL table.
    // This is a compile-time static slice matching BM1387_PLL_TABLE entries.
    &[
        100, 125, 150, 175, 200, 225, 250, 275, 300, 325, 350, 375, 400, 425, 450, 462, 475, 500,
        525, 550, 575, 600, 625, 650, 700, 725, 750, 775, 800, 825, 850, 875, 900,
    ]
}

impl Bm1387Driver {
    pub fn new() -> Self {
        Self
    }

    /// Calculate WORK_TIME register value for a given frequency and midstate count.
    ///
    /// Formula from BraiinsOS: work_time = 0.9 * midstate_count * 2^19 / freq_Hz * FPGA_WORK_CLK
    /// The FPGA work_time counter runs at 100 MHz (200 MHz fabric clock / 2).
    /// Proof: BraiinsOS at 650 MHz produces WORK_TIME=0x46E46. With 100 MHz clock
    /// and our formula: 0.9 * 4 * 2^19 / 650M * 100M = 290,374 = 0x46E46. Exact match.
    /// The previous 50 MHz assumption produced half the correct value.
    pub fn calculate_work_time(freq_mhz: u16, midstate_count: u32) -> u32 {
        const FPGA_WORK_CLK: f64 = 100_000_000.0;
        let freq_hz = freq_mhz as f64 * 1_000_000.0;
        let nonce_range = midstate_count as f64 * 524_288.0; // 2^19 (matches BraiinsOS braiins_bm1387.rs)
        let work_time = (0.9 * nonce_range / freq_hz * FPGA_WORK_CLK) as u32;
        work_time.max(1) // minimum 1 to avoid zero
    }

    fn read_pll_register(chain: &mut FpgaChain, chip_addr: u8) -> Result<Option<u32>> {
        use crate::protocol::fifo_cmd_read_register;

        while chain.cmd_rx_has_data() {
            let _ = chain.read_cmd_response();
        }

        chain.write_cmd(fifo_cmd_read_register(chip_addr, regs::PLL));
        std::thread::sleep(std::time::Duration::from_millis(20));

        if !chain.cmd_rx_has_data() {
            return Ok(None);
        }

        let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        let bytes = crate::protocol::unpack_lsb_first(r0);
        Ok(Some(u32::from_be_bytes(bytes)))
    }

    fn pll_register_to_freq(raw_reg: u32) -> Option<u16> {
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        let masked = raw_reg & !PLL_LOCK_BIT;
        MinerProfile::pll_frequencies_for_chip(CHIP_ID)
            .iter()
            .copied()
            .find(|&freq| Bm1387Driver::new().pll_params(freq).reg_value == masked)
    }
}

/// I2C passthrough temperature reading via BM1387 register 0x20.
///
/// The BM1387 chip has an on-chip I2C controller accessible via register 0x20.
/// On S9 hash boards, chip 0's I2C bus is connected to a temperature sensor
/// (TMP451, ADT7461, or NCT218 depending on board revision).
///
/// Protocol (from BraiinsOS braiins_bm1387_i2c.rs):
///   1. Configure MiscCtrl: set TF=SCL0, RF=SDA0, i2c_bus=Bottom
///   2. Write I2C_CONTROL with do_command=1 + slave addr + register
///   3. Poll I2C_CONTROL until busy=0
///   4. Read I2C_CONTROL — data byte contains the sensor's response
///
/// IMPORTANT: This uses the CMD FIFO (same path as register reads/writes).
/// It MUST NOT be called while the heartbeat thread is doing I2C on the
/// AXI bus, or while work dispatch is writing to WORK_TX_FIFO at high speed.
/// Call from the WorkDispatcher during the 5-second hashrate update tick.
impl Bm1387Driver {
    /// Enable I2C passthrough on chip 0 by writing MiscCtrl with TF=SCL0, RF=SDA0.
    ///
    /// This is a read-modify-write cycle: we first read MiscCtrl, then modify
    /// only the I2C-related bits (tfs, rfs, i2c_bus) while preserving baud_div,
    /// mmen, inv_clock, etc.
    ///
    /// BraiinsOS equivalent: `MiscCtrlReg::set_i2c(Some(I2cBusSelect::Bottom))`
    /// which sets:
    ///   - not_set_baud = true (bit 30)
    ///   - gate_block = false (bit 15)
    ///   - tfs = SCL0 (bits 6:5 = 0x03)
    ///   - rfs = SDA0 (bit 14 = 1)
    ///   - i2c_bus = Bottom (bit 16 = 0)
    fn enable_i2c_on_chip0(chain: &mut FpgaChain) -> bool {
        use crate::protocol::*;

        // Read current MiscCtrl from chip 0
        let read_cmd = fifo_cmd_read_register(0x00, regs::MISC_CONTROL);
        // Drain stale CMD RX
        while chain.cmd_rx_has_data() {
            let _ = chain.read_cmd_response();
        }
        chain.write_cmd(read_cmd);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let current_misc = if chain.cmd_rx_has_data() {
            let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            // Drain second response word (BM1387 always returns 2 words)
            if chain.cmd_rx_has_data() {
                let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            }
            let bytes = unpack_lsb_first(r0);
            u32::from_be_bytes(bytes)
        } else {
            tracing::warn!(
                chain_id = chain.chain_id,
                "I2C enable: MiscCtrl readback timeout from chip 0 — using default"
            );
            // Default mining MiscCtrl value: not_set_baud=0, inv_clock=1, gate_block=0,
            // baud_div=1, mmen=1
            0x0020_0180
        };

        // Modify for I2C: set TF=SCL0 (bits 6:5 = 11b), RF=SDA0 (bit 14 = 1),
        // i2c_bus=Bottom (bit 16 = 0), not_set_baud=1 (bit 30), gate_block=0 (bit 15).
        //
        // From BraiinsOS MiscCtrlReg::set_i2c():
        //   self.not_set_baud = true;  // bit 30
        //   self.gate_block = false;   // bit 15
        //   self.tfs = TfSelector::SCL0; // bits 6:5 = 0b11
        //   self.rfs = RfSelector::SDA0; // bit 14
        //   self.i2c_bus = i2c_bus;    // bit 16
        let mut new_misc = current_misc;
        new_misc |= 1 << 30; // not_set_baud = 1 (don't change baud)
        new_misc &= !(1 << 15); // gate_block = 0
        new_misc |= 0x03 << 5; // tfs = SCL0 (bits 6:5 = 11)
        new_misc |= 1 << 14; // rfs = SDA0
        new_misc &= !(1 << 16); // i2c_bus = Bottom (0)

        let (w0, w1) = fifo_cmd_write_reg_full(0x00, regs::MISC_CONTROL, new_misc);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(5));

        tracing::debug!(
            chain_id = chain.chain_id,
            old_misc = format_args!("0x{:08X}", current_misc),
            new_misc = format_args!("0x{:08X}", new_misc),
            "I2C passthrough enabled on chip 0"
        );
        true
    }

    /// Restore MiscCtrl on chip 0 back to mining mode (disable I2C passthrough).
    ///
    /// After temp reading, TF/RF must go back to their mining-mode functions.
    /// If left in I2C mode, chip 0's hash output (RO pin) is repurposed as SDA,
    /// blocking nonce output for the ENTIRE chain (daisy-chain through chip 0).
    ///
    /// Writes MiscCtrl 3 times with delays to maximize reliability.
    /// CMD register readback does NOT work on BM1387 via FPGA CMD FIFO
    /// (chip 0 never responds to register reads — tested 2026-04-19,
    /// 100% timeout on all 3 chains, all attempts). The nonce stall
    /// detector in work_dispatcher.rs is the real safety net.
    fn disable_i2c_on_chip0(chain: &mut FpgaChain) {
        use crate::protocol::*;

        const MISC_CTRL_MINING_I2C_OFF: u32 = 0x4020_0180;

        for _ in 0..3u8 {
            let (w0, w1) =
                fifo_cmd_write_reg_full(0x00, regs::MISC_CONTROL, MISC_CTRL_MINING_I2C_OFF);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Drain any stale CMD RX data after the writes
        while chain.cmd_rx_has_data() {
            let _ = chain.read_cmd_response();
        }
        tracing::debug!(
            chain_id = chain.chain_id,
            "I2C passthrough disabled — 3x MiscCtrl write (0x4020_0180)"
        );
    }

    /// Read one byte from a sensor register via BM1387 I2C passthrough on chip 0.
    ///
    /// Protocol:
    ///   1. Write I2C_CONTROL register on chip 0: do_command=1, addr=slave_addr (even=read),
    ///      reg=sensor_reg, data=0
    ///   2. Wait for busy flag to clear by reading I2C_CONTROL back
    ///   3. Return the data byte from the response
    ///
    /// Returns None if the I2C controller times out or the response address
    /// doesn't match (indicating no sensor at that address).
    fn i2c_read_byte(chain: &mut FpgaChain, slave_addr: u8, sensor_reg: u8) -> Option<u8> {
        use crate::protocol::*;

        // Build I2C read command: do_command=1 (bit 24), addr=slave_addr (even=read),
        // reg=sensor_reg, data=0.
        // I2cControlReg format: [flags:8 | addr:8 | reg:8 | data:8]
        let i2c_cmd: u32 = (0x01u32 << 24) // do_command = 1
            | ((slave_addr as u32) << 16)     // slave address (even = read)
            | ((sensor_reg as u32) << 8); // register to read; data = 0 for read

        // Write I2C_CONTROL register on chip 0
        let (w0, w1) = fifo_cmd_write_reg_full(0x00, regs::I2C_CONTROL, i2c_cmd);
        chain.write_cmd(w0);
        chain.write_cmd(w1);

        // Wait for the I2C transaction to complete by polling the register.
        // BraiinsOS uses MAX_I2C_BUSY_WAIT_TRIES=50 with 1ms delay = 50ms max.
        for attempt in 0..50u8 {
            std::thread::sleep(std::time::Duration::from_millis(1));

            // Read I2C_CONTROL back from chip 0
            let read_cmd = fifo_cmd_read_register(0x00, regs::I2C_CONTROL);
            // Drain stale CMD RX
            while chain.cmd_rx_has_data() {
                let _ = chain.read_cmd_response();
            }
            chain.write_cmd(read_cmd);
            std::thread::sleep(std::time::Duration::from_millis(2));

            if !chain.cmd_rx_has_data() {
                if attempt >= 10 {
                    tracing::trace!(
                        chain_id = chain.chain_id,
                        attempt,
                        "I2C read: no CMD RX response (attempt {})",
                        attempt,
                    );
                }
                continue;
            }

            let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            // Drain second word
            if chain.cmd_rx_has_data() {
                let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            }

            let bytes = unpack_lsb_first(r0);
            let reg_val = u32::from_be_bytes(bytes);

            // Check busy flag (bit 31)
            if reg_val & 0x8000_0000 != 0 {
                continue; // Still busy
            }

            // Verify the response address and register match our request.
            // BraiinsOS checks: cmd_reply.addr == cmd_request.addr && cmd_reply.reg == reg
            let resp_addr = ((reg_val >> 16) & 0xFF) as u8;
            let resp_reg = ((reg_val >> 8) & 0xFF) as u8;
            let resp_data = (reg_val & 0xFF) as u8;

            if resp_addr == slave_addr && resp_reg == sensor_reg {
                return Some(resp_data);
            }

            // Address/register mismatch — BraiinsOS retries up to MAX_I2C_FAIL_TRIES=3
            // with 50ms delay. For our simplified impl, just continue polling.
            tracing::trace!(
                chain_id = chain.chain_id,
                expected_addr = format_args!("0x{:02X}", slave_addr),
                got_addr = format_args!("0x{:02X}", resp_addr),
                expected_reg = format_args!("0x{:02X}", sensor_reg),
                got_reg = format_args!("0x{:02X}", resp_reg),
                "I2C read: address/register mismatch"
            );
        }

        None // Timed out
    }

    /// Probe for a temperature sensor on this chain's hash board.
    ///
    /// Tries all known I2C addresses (0x98, 0x9A, 0x9C) and reads the
    /// manufacturer ID and device ID to identify the sensor type.
    ///
    /// Returns (slave_addr, manufacturer_id, device_id) if a sensor is found.
    ///
    /// Source: BraiinsOS braiins_sensor.rs probe_i2c_device().
    /// Sensor types:
    ///   - manufacturer_id=0x55: TI — TMP451 (default) or TMP42x (dev_id 0x21-0x23)
    ///   - manufacturer_id=0x41: Analog Devices — ADT7461
    ///   - manufacturer_id=0x1A: ON Semi — NCT218
    fn probe_sensor(chain: &mut FpgaChain) -> Option<(u8, u8, u8)> {
        for &addr in &SENSOR_I2C_ADDRESSES {
            // Read manufacturer ID (register 0xFE)
            let man_id = match Self::i2c_read_byte(chain, addr, REG_MANUFACTURER_ID) {
                Some(v) => v,
                None => continue,
            };

            // Read device ID (register 0xFF)
            let dev_id = match Self::i2c_read_byte(chain, addr, REG_DEVICE_ID) {
                Some(v) => v,
                None => continue,
            };

            // Check if this is a known sensor
            match man_id {
                0x55 | 0x41 | 0x1A => {
                    tracing::info!(
                        chain_id = chain.chain_id,
                        addr = format_args!("0x{:02X}", addr),
                        manufacturer_id = format_args!("0x{:02X}", man_id),
                        device_id = format_args!("0x{:02X}", dev_id),
                        sensor_type = match man_id {
                            0x55 => match dev_id {
                                0x21..=0x23 => "TMP42x",
                                _ => "TMP451",
                            },
                            0x41 => "ADT7461",
                            0x1A => "NCT218",
                            _ => "Unknown",
                        },
                        "Temperature sensor detected on hash board"
                    );
                    return Some((addr, man_id, dev_id));
                }
                _ => {
                    tracing::debug!(
                        chain_id = chain.chain_id,
                        addr = format_args!("0x{:02X}", addr),
                        manufacturer_id = format_args!("0x{:02X}", man_id),
                        "Unknown device at I2C address — skipping"
                    );
                }
            }
        }
        None
    }

    /// Read hash board temperature via BM1387 I2C passthrough (register 0x20).
    ///
    /// Reads from chip 0's I2C passthrough to the on-board temp sensor.
    /// Returns temperature in degrees Celsius, or None if sensor not detected.
    ///
    /// The sensor's "remote" temperature (REG_REMOTE_TEMP = 0x01) measures the
    /// BM1387 die temperature via an external diode connected to the ASIC's
    /// TEMP_P/TEMP_N pins. This is the actual chip temperature.
    ///
    /// The sensor's "local" temperature (REG_LOCAL_TEMP = 0x00) measures the
    /// sensor IC's own die temperature, which approximates PCB temperature.
    ///
    /// We read the remote temperature as it's more relevant for thermal control.
    /// If remote read fails (open circuit / no diode), fall back to local temp.
    ///
    /// IMPORTANT: This temporarily reconfigures chip 0's MiscCtrl to enable I2C.
    /// While I2C is active, chip 0 cannot report nonces (RF pin is SDA, not RO).
    /// The function restores mining mode before returning. Total disruption is
    /// ~100-200ms per read, acceptable at 5-second intervals.
    pub fn read_board_temp(chain: &mut FpgaChain) -> Option<f32> {
        // Step 1: Enable I2C passthrough on chip 0
        if !Self::enable_i2c_on_chip0(chain) {
            return None;
        }

        // Step 2: Probe for sensor (first call) or read directly (subsequent calls)
        // For simplicity, we probe every time. The overhead is ~10ms per address
        // which is negligible at 5-second intervals. A future optimization could
        // cache the sensor address per chain.
        let sensor = Self::probe_sensor(chain);

        let result = if let Some((addr, _man_id, _dev_id)) = sensor {
            // Step 3: Read remote temperature (BM1387 die temp via external diode)
            let remote = Self::i2c_read_byte(chain, addr, REG_REMOTE_TEMP);

            match remote {
                Some(temp) if temp < 127 && temp > 0 => {
                    // Valid remote temp (0-126C range, 127 = open circuit / error)
                    tracing::debug!(
                        chain_id = chain.chain_id,
                        remote_temp_c = temp,
                        "Board temp (remote/ASIC diode): {}C",
                        temp,
                    );
                    Some(temp as f32)
                }
                _ => {
                    // Remote temp failed or out of range — try local temp as fallback
                    let local = Self::i2c_read_byte(chain, addr, REG_LOCAL_TEMP);
                    match local {
                        Some(temp) if temp < 127 => {
                            tracing::debug!(
                                chain_id = chain.chain_id,
                                local_temp_c = temp,
                                "Board temp (local/PCB sensor): {}C (remote diode unavailable)",
                                temp,
                            );
                            Some(temp as f32)
                        }
                        _ => {
                            tracing::warn!(
                                chain_id = chain.chain_id,
                                "Sensor found but temp read failed (remote={:?}, local={:?})",
                                remote,
                                local,
                            );
                            None
                        }
                    }
                }
            }
        } else {
            tracing::debug!(
                chain_id = chain.chain_id,
                "No temperature sensor found on hash board"
            );
            None
        };

        // Step 4: ALWAYS restore mining mode, even if temp read failed
        // Triple-write MiscCtrl (no readback — BM1387 CMD reads always timeout)
        Self::disable_i2c_on_chip0(chain);

        result
    }
}

impl ChipDriver for Bm1387Driver {
    fn chip_id(&self) -> u16 {
        CHIP_ID
    }

    fn chip_name(&self) -> &'static str {
        "BM1387"
    }

    fn cores_per_chip(&self) -> u32 {
        // BM1387 core count not precisely documented; use hashrate-derived estimate
        114
    }

    fn response_length(&self) -> usize {
        9 // 9 bytes = 2 x 32-bit words + flags
    }

    fn default_baud(&self) -> u32 {
        115_200
    }

    fn max_baud(&self) -> u32 {
        6_000_000
    }

    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8, freq_mhz: u16) -> Result<()> {
        use crate::protocol::*;

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1387: Configuring {} chips at {} MHz (enumeration and addressing already complete)",
            chip_count,
            freq_mhz,
        );

        // NOTE: FPGA IP core reset (UART BREAK) is DISABLED.
        //
        // BraiinsOS does disable_ip_core() + enable_ip_core() BEFORE any UART
        // traffic. But on DCENTos hot restart, UART traffic has already flowed
        // (Phase 4c enumeration). The reset_ip_core() sends a BREAK that resets
        // ASICs to 115200, but it ALSO breaks the FPGA UART state machine —
        // proven by register readback showing TIMEOUT after reset.
        //
        // BraiinsOS's init_and_split() resets the IP core as the FIRST operation
        // (before opening FIFOs or sending ANY commands). Our init_chain runs
        // AFTER enumeration, so the FPGA has already had UART traffic.
        //
        // The reset must be moved to Phase 4a (before Phase 4c enumeration)
        // to match BraiinsOS ordering. For now, skip it here.

        // Step 0: Reset ASIC baud to 115200 if hot start.
        //
        // On hot start, ASICs are at whatever baud rate the previous firmware left
        // them (typically 1.5M from bosminer). The FPGA baud matches after Phase 4c
        // enumeration. If we just set FPGA to 115200 (Step 1), the ASICs can't hear
        // any of our register writes because they're still at 1.5M.
        //
        // Fix: send a MiscCtrl write at the CURRENT FPGA baud to reset ASICs to
        // 115200, then switch FPGA to 115200 to match. On cold boot, the FPGA is
        // already at 115200, so this step is skipped.
        let current_baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);
        if current_baud_div != fpga_chain::BAUD_REG_115200 {
            // Send MiscCtrl with not_set_baud=0, baud_div=26 (115200) at current baud.
            // This tells ASICs to switch their UART back to 115200.
            // gate_block=0 is fine here — we set gate_block=1 in Step 4 (MiscCtrl).
            const MISC_CTRL_RESET_BAUD: u32 = 0x0020_1A80;
            let (w0, w1) = fifo_cmd_write_reg_bcast_full(regs::MISC_CONTROL, MISC_CTRL_RESET_BAUD);
            chain.write_cmd(w0);
            chain.write_cmd(w1);
            std::thread::sleep(std::time::Duration::from_millis(10));
            tracing::info!(
                chain_id = chain.chain_id,
                current_baud_div = format_args!("0x{:02X}", current_baud_div),
                "Hot start baud reset: sent MiscCtrl(not_set_baud=0, baud_div=26) at current baud — \
                 ASICs switching from fast baud back to 115200",
            );
        }

        // Step 1: Set FPGA baud to 115200 for configuration commands.
        chain.set_baud(fpga_chain::BAUD_REG_115200);
        tracing::debug!("FPGA baud set to 115200 (BAUD_REG=0x6C)");

        // Step 2: Set PLL frequency FIRST (at 115200 baud — matches BraiinsOS).
        // BraiinsOS writes PLL before baud upgrade so the clock change happens
        // over reliable 115200 baud communication.
        let pll = self.pll_params(freq_mhz);
        let (w0, w1) = fifo_cmd_write_reg_bcast_full(regs::PLL, pll.reg_value);
        let w0_bytes = unpack_lsb_first(w0);
        let w1_bytes = unpack_lsb_first(w1);
        tracing::info!(
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            freq_mhz = freq_mhz,
            fifo_w0 = format_args!("0x{:08X}", w0),
            fifo_w1 = format_args!("0x{:08X}", w1),
            wire = format_args!(
                "[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}]",
                w0_bytes[0],
                w0_bytes[1],
                w0_bytes[2],
                w0_bytes[3],
                w1_bytes[0],
                w1_bytes[1],
                w1_bytes[2],
                w1_bytes[3]
            ),
            "PLL write (broadcast at 115200) — all chips switching to {} MHz",
            freq_mhz,
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        // Wait for PLL to lock (~10ms typical)
        std::thread::sleep(std::time::Duration::from_millis(10));

        // PLL readback verification: read PLL register from chip 0 to confirm it took effect.
        // BM1387 PLL register bit 31 = PLL_LOCKED (read-only status bit, set when PLL locks).
        // We must MASK bit 31 when comparing, otherwise readback always "mismatches" and
        // we spuriously re-write PLL 3 times, disrupting the init sequence.
        const PLL_LOCK_BIT: u32 = 0x8000_0000;
        for pll_retry in 0..3u8 {
            while chain.cmd_rx_has_data() {
                let _ = chain.read_cmd_response();
            }
            let pll_read_cmd = pack_lsb_first(&[0x44, 0x05, 0x00, regs::PLL]);
            chain.write_cmd(pll_read_cmd);
            std::thread::sleep(std::time::Duration::from_millis(50));

            let pll_readback = if chain.cmd_rx_has_data() {
                let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
                let _r1 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
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
                        "PLL readback VERIFIED — chip 0 at {} MHz, PLL_LOCKED={} (attempt {})",
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
                        masked = format_args!("0x{:08X}", val & !PLL_LOCK_BIT),
                        "PLL readback MISMATCH — expected 0x{:08X}, got 0x{:08X} (masked=0x{:08X}, attempt {}/3)",
                        pll.reg_value, val, val & !PLL_LOCK_BIT, pll_retry + 1,
                    );
                    if pll_retry < 2 {
                        let (rw0, rw1) = fifo_cmd_write_reg_bcast_full(regs::PLL, pll.reg_value);
                        chain.write_cmd(rw0);
                        chain.write_cmd(rw1);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
                None => {
                    tracing::warn!(
                        chain_id = chain.chain_id,
                        "PLL readback TIMEOUT — chip 0 did not respond (attempt {}/3)",
                        pll_retry + 1,
                    );
                    if pll_retry < 2 {
                        let (rw0, rw1) = fifo_cmd_write_reg_bcast_full(regs::PLL, pll.reg_value);
                        chain.write_cmd(rw0);
                        chain.write_cmd(rw1);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }

        // Step 3: Set WORK_TIME in FPGA (calculated for actual frequency).
        let work_time = Bm1387Driver::calculate_work_time(freq_mhz, NUM_MIDSTATES as u32);
        chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
        tracing::info!(
            work_time = format_args!("0x{:08X}", work_time),
            freq_mhz = freq_mhz,
            "WORK_TIME set to 0x{:08X} (calculated for {} MHz, {} midstates)",
            work_time,
            freq_mhz,
            NUM_MIDSTATES,
        );

        // Step 4: MiscCtrl — single write: gate_block=1 + baud upgrade to 1.5M.
        //
        // BraiinsOS does this as ONE register write:
        //   configure_hash_chain(1.5M, not_set_baud=false, gate_block=true)
        //   -> MiscCtrlReg::new(false, true, 1, true, true) = 0x00208180
        //
        // The previous TWO-STEP approach (0x40209A80 then 0x00208180) was
        // unreliable — MiscCtrl readback showed gate_block=0 after Step 5.
        // The two writes arrive back-to-back with only 10ms gap, and the second
        // write may collide with ASIC processing the first.
        //
        // BraiinsOS has proven this works in a single write for years.
        //
        // Value 0x00208180:
        //   bit 30: not_set_baud=0 (DO change baud)
        //   bit 21: inv_clock=1
        //   bit 15: gate_block=1 (BLOCK all cores until open-core)
        //   bits 12:8: baud_div=1 -> 25MHz / (8*2*1) = 1,562,500
        //   bit 7: mmen=1 (multi-midstate / AsicBoost)
        let (w0, w1) = fifo_cmd_write_reg_bcast_full(
            regs::MISC_CONTROL,
            BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST,
        );
        tracing::info!(
            value = format_args!("0x{:08X}", BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST),
            "MiscCtrl: gate_block=1 + baud upgrade to 1.5M (single write, BraiinsOS-proven)",
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Step 5: Switch FPGA baud to match ASICs.
        chain.set_baud(fpga_chain::BAUD_REG_1_5M);
        tracing::info!("FPGA baud set to 1,562,500 (BAUD_REG=0x07) — matches ASIC baud");

        // Settling delay after baud upgrade.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Step 5b: Re-send MiscCtrl at 1.5M to GUARANTEE gate_block=1 delivery.
        //
        // CRITICAL FIX (2026-03-18, 9-agent team review):
        // The Step 4 MiscCtrl write at 115200 has a baud transition race condition.
        // The ASIC starts switching to 1.5M mid-command (not_set_baud=0 triggers
        // immediate baud change), potentially corrupting the gate_block bit (bit 15).
        // Evidence: readback showed gate_block=0 despite writing gate_block=1.
        //
        // Fix: Re-send MiscCtrl at 1.5M (now both FPGA and ASIC are at 1.5M)
        // with not_set_baud=1 (don't change baud again, just set gate_block).
        // Value 0x40208180: not_set_baud=1, inv_clock=1, gate_block=1, baud_div=1, mmen=1.
        let (w0, w1) = fifo_cmd_write_reg_bcast_full(
            regs::MISC_CONTROL,
            BM1387_MISC_CTRL_CONFIRM_GATE_ASICBOOST,
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(50));
        tracing::info!(
            value = format_args!("0x{:08X}", BM1387_MISC_CTRL_CONFIRM_GATE_ASICBOOST),
            "Step 5b: MiscCtrl re-sent at 1.5M (not_set_baud=1, gate_block=1) — guarantees gate_block delivery",
        );

        // Step 6: Set TicketMask LAST (after baud upgrade, at 1.5M baud)
        // BraiinsOS writes TicketMask after baud upgrade — this ensures the
        // register write uses the fast operational baud rate.
        let mask = self.ticket_mask(256);
        let (w0, w1) = fifo_cmd_write_reg_bcast_full(regs::TICKET_MASK, mask);
        let w0_bytes = unpack_lsb_first(w0);
        let w1_bytes = unpack_lsb_first(w1);
        tracing::info!(
            reg = format_args!("0x{:02X}", regs::TICKET_MASK),
            mask = format_args!("0x{:08X}", mask),
            fifo_w0 = format_args!("0x{:08X}", w0),
            fifo_w1 = format_args!("0x{:08X}", w1),
            wire = format_args!(
                "[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}]",
                w0_bytes[0],
                w0_bytes[1],
                w0_bytes[2],
                w0_bytes[3],
                w1_bytes[0],
                w1_bytes[1],
                w1_bytes[2],
                w1_bytes[3]
            ),
            "TicketMask write (at 1.5M baud) — difficulty {} (only 1 in {} hashes reported back)",
            mask + 1,
            mask + 1,
        );
        chain.write_cmd(w0);
        chain.write_cmd(w1);

        // === REGISTER READBACK VERIFICATION ===
        // Read MiscCtrl from chip 0 to verify our writes actually reached the ASICs.
        // If this returns the value we wrote (0x00208180), UART communication works.
        // If it returns default (0x40201A00) or times out, our writes didn't reach.
        // CMD read: [0x44, 0x05, chip_addr, reg] — single chip read
        let readback_cmd = pack_lsb_first(&[0x44, 0x05, 0x00, regs::MISC_CONTROL]);
        // Clear stale CMD RX
        while chain.cmd_rx_has_data() {
            let _ = chain.read_cmd_response();
        }
        chain.write_cmd(readback_cmd);
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Drain WORK_RX FIFO — stale nonces from open-core or init may have landed here.
        // CMD register responses go to CMD_RX_FIFO, not WORK_RX, but we drain both
        // to ensure clean state for the readback check.
        while chain.read_nonce().is_some() {}
        let readback = if chain.cmd_rx_has_data() {
            let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            let r1 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            // Response word 0 contains the register value (LSB-first packed)
            // Unpack to bytes, then interpret as BE register value
            let bytes = unpack_lsb_first(r0);
            let reg_val = u32::from_be_bytes(bytes);
            Some((reg_val, r0, r1))
        } else {
            None
        };

        match readback {
            Some((val, raw0, raw1)) => {
                let gb = (val >> 15) & 1;
                let bd = (val >> 8) & 0x1F;
                tracing::warn!(
                    chain_id = chain.chain_id,
                    readback = format_args!("0x{:08X}", val),
                    raw_w0 = format_args!("0x{:08X}", raw0),
                    raw_w1 = format_args!("0x{:08X}", raw1),
                    "READBACK: MiscCtrl from chip 0 = 0x{:08X} (gate_block={}, baud_div={}). \
                     Expected 0x00208180 (gate_block=1, baud_div=1). {}",
                    val,
                    gb,
                    bd,
                    if val == 0x00208180 {
                        "MATCH — ASIC received our writes!"
                    } else if val == 0x40201A00 {
                        "DEFAULT — ASIC did NOT receive writes (still at reset state)"
                    } else {
                        "UNEXPECTED — check baud mismatch or timing"
                    },
                );
            }
            None => {
                tracing::error!(
                    chain_id = chain.chain_id,
                    "READBACK: MiscCtrl TIMEOUT — chip 0 did not respond to CMD read! \
                     ASIC UART is not working at the current baud (1.5M). \
                     Our register writes (PLL, MiscCtrl, TicketMask) likely never reached the chips.",
                );
            }
        }

        tracing::info!(
            chain_id = chain.chain_id,
            chip_count = chip_count,
            freq_mhz = freq_mhz,
            "BM1387: Chain configuration complete — {} chips at {} MHz, gate_block=1 (cores blocked until open-core init)",
            chip_count, freq_mhz,
        );

        Ok(())
    }

    fn send_open_core_work(&self, chain: &mut FpgaChain, _chip_count: u8) -> Result<u32> {
        use crate::protocol::*;

        // BM1387 requires 114 dummy work items to activate SHA-256 cores.
        // gate_block=1 in MiscCtrl puts all cores in blocked state. Each
        // work item with bit[0]=1 activates one core. After 114 items,
        // all cores are active and ready to hash real mining work.
        //
        // Work format: 36 words (4 header + 4x8 midstate words)
        // nbits = 0x1d00ffff (difficulty-1, any hash is valid)
        // All-zero midstate (actual hash content doesn't matter for activation)

        // CRITICAL: Reset WORK_TX FIFO before open-core to clear any partial
        // work frame left by bosminer's SIGKILL. The FPGA's UART serializer may
        // be stuck mid-frame, causing all subsequent work items to be misaligned.
        chain.work_tx.write_reg(fpga_chain::REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(2));
        chain
            .work_tx
            .write_reg(fpga_chain::REG_WORK_TX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);
        std::thread::sleep(std::time::Duration::from_millis(1));
        tracing::info!(
            chain_id = chain.chain_id,
            "WORK_TX FIFO reset before open-core"
        );

        // === DIAGNOSTIC: FPGA state before open-core ===
        let ctrl_reg = chain.common.read_reg(fpga_chain::REG_CTRL);
        let work_time = chain.common.read_reg(fpga_chain::REG_WORK_TIME);
        let baud_reg = chain.common.read_reg(fpga_chain::REG_BAUD);
        let err_cnt_before = chain.common.read_reg(fpga_chain::REG_ERR_COUNTER);
        let wtx_stat = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_STAT);
        let wrx_stat = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_STAT);
        let wtx_last = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_LAST);
        tracing::warn!(
            chain_id = chain.chain_id,
            ctrl_reg = format_args!("0x{:08X}", ctrl_reg),
            work_time = format_args!("0x{:08X}", work_time),
            baud_reg = format_args!("0x{:08X}", baud_reg),
            err_cnt = err_cnt_before,
            wtx_stat = format_args!("0x{:02X}", wtx_stat),
            wrx_stat = format_args!("0x{:02X}", wrx_stat),
            wtx_last = format_args!("0x{:08X}", wtx_last),
            "DIAG: FPGA state BEFORE open-core — CTRL=0x{:08X} (ENABLE={}, MIDSTATE_CNT={}), \
             WORK_TIME=0x{:08X}, BAUD=0x{:08X}, ERRORS={}, \
             WTX_STAT=0x{:02X} (TX_EMPTY={}, TX_FULL={}), \
             WRX_STAT=0x{:02X} (RX_EMPTY={}), WTX_LAST=0x{:08X}",
            ctrl_reg,
            ctrl_reg & 0x08 != 0,
            (ctrl_reg >> 1) & 0x03,
            work_time,
            baud_reg,
            err_cnt_before,
            wtx_stat,
            wtx_stat & 0x04 != 0,
            wtx_stat & 0x08 != 0,
            wrx_stat,
            wrx_stat & 0x01 != 0,
            wtx_last,
        );

        tracing::info!(
            chain_id = chain.chain_id,
            cores = NUM_CORES_ON_CHIP,
            "OPEN-CORE: Sending {} init work items to activate SHA-256 cores \
             (gate_block=1 → cores enabled one-by-one)",
            NUM_CORES_ON_CHIP,
        );

        // PACING RECONCILIATION (RE 2026-06-02, mining-bible-v1 chip-init-sequences.md §10):
        // the AMTC test-jig open-core is "114 dummy works PER CHIP, 10 ms between dummies,
        // 50 ms OpenCoreGap BETWEEN CHIPS" (sequential). DCENT uses the proven BROADCAST model
        // instead — 114 broadcast dummies, each activating core[i] on EVERY chip simultaneously
        // (sustained-mining-proven on live S9 since 2026-04-19). The bible's 10 ms inter-dummy
        // spacing IS applied below (the per-item sleep); the 50 ms per-chip OpenCoreGap does not
        // apply because there is no per-chip loop in the broadcast model. Also note open-core nbits
        // is 0xFFFFFFFF here (BraiinsOS null_work.rs), NOT the bible §10 value 0x1d00ffff — the
        // bible value is errata (it has zero bytes that block activation); firmware is correct.
        for i in 0..NUM_CORES_ON_CHIP {
            let mut words = [0u32; WORK_WORDS];

            // Word 0: work_id shifted for MIDSTATE_CNT=2
            words[0] = i << MIDSTATE_CNT_LOG2;
            // Word 1: nbits = 0xFFFFFFFF for core activation.
            // BraiinsOS null_work.rs: `let bits = if enable_core { 0xffff_ffff } else { 0 };`
            // The BM1387 checks this specific pattern to activate cores with gate_block=1.
            // 0x1d00ffff does NOT work — it has zero bytes that prevent core activation.
            words[1] = 0xFFFF_FFFF;
            // Words 2-3: ntime=0, merkle_tail=0 (already zero)
            // Words 4-35: all-zero midstates (already zero)

            // Expert review fix: Wait for TX FIFO space before writing.
            // Prevents silent work item loss if FPGA can't drain fast enough.
            for _ in 0..100 {
                if !chain.work_tx_full() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
            chain.write_work(&words);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Wait for last work items to be processed
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Drain WORK_RX_FIFO — count init nonces
        let mut nonce_count: u32 = 0;
        while chain.read_nonce().is_some() {
            nonce_count += 1;
        }

        // If zero init nonces, log WARNING and retry open-core once.
        // Zero nonces during open-core means cores may not have activated.
        if nonce_count == 0 {
            tracing::warn!(
                chain_id = chain.chain_id,
                "OPEN-CORE: 0 init nonces — cores may not have activated. Retrying open-core once.",
            );
            // Reset WORK_TX FIFO again
            chain.work_tx.write_reg(fpga_chain::REG_WORK_TX_CTRL, 0x02);
            std::thread::sleep(std::time::Duration::from_millis(2));
            chain
                .work_tx
                .write_reg(fpga_chain::REG_WORK_TX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);
            std::thread::sleep(std::time::Duration::from_millis(1));

            for i in 0..NUM_CORES_ON_CHIP {
                let mut words = [0u32; WORK_WORDS];
                words[0] = i << MIDSTATE_CNT_LOG2;
                words[1] = 0xFFFF_FFFF;
                for _ in 0..100 {
                    if !chain.work_tx_full() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_micros(100));
                }
                chain.write_work(&words);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Drain retry nonces
            while chain.read_nonce().is_some() {
                nonce_count += 1;
            }
            tracing::info!(
                chain_id = chain.chain_id,
                nonces_after_retry = nonce_count,
                "OPEN-CORE retry complete — {} total init nonces after retry",
                nonce_count,
            );
        }

        // Re-arm WORK_RX FIFO after open-core: reset and re-enable IRQ.
        // This ensures no stale open-core nonces remain in the FIFO and the
        // RX path is clean for real mining work. Without this, leftover init
        // nonces or a partially-filled FIFO entry could cause misalignment.
        chain.work_rx.write_reg(fpga_chain::REG_WORK_RX_CTRL, 0x01); // RST_RX
        std::thread::sleep(std::time::Duration::from_millis(1));
        chain
            .work_rx
            .write_reg(fpga_chain::REG_WORK_RX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);

        // === DIAGNOSTIC: FPGA state after open-core ===
        let err_cnt_after = chain.common.read_reg(fpga_chain::REG_ERR_COUNTER);
        let wtx_stat_after = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_STAT);
        let wrx_stat_after = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_STAT);
        let wtx_last_after = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_LAST);
        let ctrl_after = chain.common.read_reg(fpga_chain::REG_CTRL);
        tracing::warn!(
            chain_id = chain.chain_id,
            nonces_received = nonce_count,
            ctrl_reg = format_args!("0x{:08X}", ctrl_after),
            err_before = err_cnt_before,
            err_after = err_cnt_after,
            wtx_stat = format_args!("0x{:02X}", wtx_stat_after),
            wrx_stat = format_args!("0x{:02X}", wrx_stat_after),
            wtx_last = format_args!("0x{:08X}", wtx_last_after),
            "DIAG: FPGA state AFTER open-core — {} nonces, CTRL=0x{:08X}, \
             CRC_ERRORS: {}→{} (delta={}), \
             WTX_STAT=0x{:02X} (TX_EMPTY={}), WRX_STAT=0x{:02X} (RX_EMPTY={}), \
             WTX_LAST=0x{:08X}. If WTX_LAST is still 0x0 after 114 work items, \
             FPGA is NOT consuming work from TX FIFO!",
            nonce_count,
            ctrl_after,
            err_cnt_before,
            err_cnt_after,
            err_cnt_after.wrapping_sub(err_cnt_before),
            wtx_stat_after,
            wtx_stat_after & 0x04 != 0,
            wrx_stat_after,
            wrx_stat_after & 0x01 != 0,
            wtx_last_after,
        );

        tracing::info!(
            chain_id = chain.chain_id,
            nonces_received = nonce_count,
            "OPEN-CORE complete: {} init nonces received and discarded — \
             all {} SHA-256 cores now active",
            nonce_count,
            NUM_CORES_ON_CHIP,
        );

        // CRITICAL: Clear gate_block after open-core to allow normal mining work.
        //
        // With gate_block=1, the BM1387 "ignores any incoming work" unless the
        // activation bit (bit[0] of nbits) is set. Open-core work has this bit
        // set (nbits=0xFFFFFFFF) to activate cores. But real mining work does NOT
        // have this bit set, so with gate_block=1 still active, ALL mining work
        // is silently ignored — zero nonces.
        //
        // BraiinsOS clears gate_block as a side effect of I2C sensor init
        // (set_i2c() sets gate_block=false). Verified: BraiinsOS live ASIC
        // MiscCtrl = 0x00200180 (gate_block=0), ours was 0x00208180 (gate_block=1).
        //
        // Value 0x00200180:
        //   bit 30: not_set_baud=0
        //   bit 21: inv_clock=1        → 0x200000
        //   bit 15: gate_block=0       → 0x000000  (CLEARED!)
        //   bits 12:8: baud_div=1      → 0x000100
        //   bit 7: mmen=1              → 0x000080
        //   Total: 0x200000 + 0x100 + 0x80 = 0x200180
        const MISC_CTRL_MINING: u32 = 0x0020_0180;
        let (w0, w1) =
            crate::protocol::fifo_cmd_write_reg_bcast_full(regs::MISC_CONTROL, MISC_CTRL_MINING);
        chain.write_cmd(w0);
        chain.write_cmd(w1);
        std::thread::sleep(std::time::Duration::from_millis(50));
        tracing::info!(
            chain_id = chain.chain_id,
            value = format_args!("0x{:08X}", MISC_CTRL_MINING),
            "MiscCtrl: gate_block CLEARED — cores now accept ALL incoming work (mining mode)",
        );

        // Readback MiscCtrl to VERIFY gate_block was cleared.
        // If gate_block is still 1, ALL mining work is silently ignored → 0 nonces.
        // This catches the baud mismatch race condition that caused 0-nonce boots.
        {
            let readback_cmd = pack_lsb_first(&[0x44, 0x05, 0x00, regs::MISC_CONTROL]);
            while chain.cmd_rx_has_data() {
                let _ = chain.read_cmd_response();
            }
            chain.write_cmd(readback_cmd);
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Drain WORK_RX in case nonce landed there
            let _ = chain.read_nonce();
            if chain.cmd_rx_has_data() {
                let r0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
                // Drain r1 (CRC/status word) to prevent FIFO contamination of next CMD read.
                // BM1387 register response is 2 FIFO words. Matches pattern at lines 470-471.
                let _r1 = if chain.cmd_rx_has_data() {
                    chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO)
                } else {
                    0
                };
                // FPGA packs wire bytes LSB-first. BM1387 sends register values MSB-first (BE).
                // unpack_lsb_first recovers wire order, from_be_bytes interprets the BE value.
                let bytes = unpack_lsb_first(r0);
                let reg_val = u32::from_be_bytes(bytes);
                let gb = (reg_val >> 15) & 1;
                if gb == 0 {
                    tracing::info!(
                        chain_id = chain.chain_id,
                        readback = format_args!("0x{:08X}", reg_val),
                        "VERIFIED: MiscCtrl gate_block=0 — ASICs ready for mining work",
                    );
                } else {
                    tracing::error!(
                        chain_id = chain.chain_id,
                        readback = format_args!("0x{:08X}", reg_val),
                        "CRITICAL: gate_block STILL SET after clear! Retrying MiscCtrl write...",
                    );
                    // Retry the gate_block clear
                    let (w0r, w1r) = crate::protocol::fifo_cmd_write_reg_bcast_full(
                        regs::MISC_CONTROL,
                        MISC_CTRL_MINING,
                    );
                    chain.write_cmd(w0r);
                    chain.write_cmd(w1r);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            } else {
                tracing::warn!(
                    chain_id = chain.chain_id,
                    "MiscCtrl readback timeout after gate_block clear — cannot verify (proceeding anyway)",
                );
            }
        }

        // Reset WORK_TX FIFO after open-core to flush any residual work frames.
        // Without this, leftover open-core work items could interleave with
        // real mining work in the FPGA's UART serializer, corrupting dispatches.
        chain.work_tx.write_reg(fpga_chain::REG_WORK_TX_CTRL, 0x02); // RST_TX
        std::thread::sleep(std::time::Duration::from_millis(2));
        chain
            .work_tx
            .write_reg(fpga_chain::REG_WORK_TX_CTRL, fpga_chain::CMD_CTRL_IRQ_EN);
        tracing::info!(
            chain_id = chain.chain_id,
            "WORK_TX FIFO reset after open-core — clean slate for mining"
        );

        Ok(nonce_count)
    }

    fn set_frequency(&self, chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()> {
        let pll = self.pll_params(freq_mhz);

        tracing::info!(
            chip_addr = format_args!("0x{:02X}", chip_addr),
            freq_mhz,
            pll_reg = format_args!("0x{:08X}", pll.reg_value),
            "BM1387: Setting frequency"
        );

        // Write PLL register (0x0C) — requires 2 FIFO words for full 32-bit value
        if chip_addr == 0xFF {
            // Broadcast to all chips
            let (w0, w1) = crate::protocol::fifo_cmd_write_reg_bcast_full(regs::PLL, pll.reg_value);
            let w0_bytes = crate::protocol::unpack_lsb_first(w0);
            let w1_bytes = crate::protocol::unpack_lsb_first(w1);
            tracing::info!(
                pll_reg = format_args!("0x{:08X}", pll.reg_value),
                fifo_w0 = format_args!("0x{:08X}", w0),
                fifo_w1 = format_args!("0x{:08X}", w1),
                wire = format_args!(
                    "[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}]",
                    w0_bytes[0],
                    w0_bytes[1],
                    w0_bytes[2],
                    w0_bytes[3],
                    w1_bytes[0],
                    w1_bytes[1],
                    w1_bytes[2],
                    w1_bytes[3]
                ),
                "PLL write (broadcast) — telling all chips to run their SHA-256 cores at {} MHz",
                freq_mhz,
            );
            chain.write_cmd(w0);
            chain.write_cmd(w1);
        } else {
            // Single chip
            let (w0, w1) =
                crate::protocol::fifo_cmd_write_reg_full(chip_addr, regs::PLL, pll.reg_value);
            let w0_bytes = crate::protocol::unpack_lsb_first(w0);
            let w1_bytes = crate::protocol::unpack_lsb_first(w1);
            tracing::info!(
                chip_addr = format_args!("0x{:02X}", chip_addr),
                pll_reg = format_args!("0x{:08X}", pll.reg_value),
                wire = format_args!(
                    "[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}]",
                    w0_bytes[0],
                    w0_bytes[1],
                    w0_bytes[2],
                    w0_bytes[3],
                    w1_bytes[0],
                    w1_bytes[1],
                    w1_bytes[2],
                    w1_bytes[3]
                ),
                "PLL write (chip 0x{:02X}) — {} MHz",
                chip_addr,
                freq_mhz,
            );
            chain.write_cmd(w0);
            chain.write_cmd(w1);
        }

        // Wait for PLL to lock (~10ms typical)
        std::thread::sleep(std::time::Duration::from_millis(10));
        tracing::debug!(
            "PLL lock wait complete (10ms) — SHA-256 cores now clocking at {} MHz",
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
                    "BM1387 PLL readback 0x{:08X} did not map to a known frequency",
                    raw
                ))
            }),
            None => Err(crate::AsicError::FifoTimeout {
                chain_id: chain.chain_id,
                detail: format!(
                    "BM1387 PLL readback timed out for chip 0x{:02X}",
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
        // BM1387 work format with MIDSTATE_CNT=2 (4 midstates, via WORK_TX_FIFO):
        //   Word 0:      Extended Work ID, shifted left by MIDSTATE_CNT_LOG2.
        //                FPGA uses low 2 bits for midstate index.
        //                BraiinsOS pattern: hw_id = (work_id << 2) | midstate_idx
        //   Word 1:      nbits (32-bit LE)
        //   Word 2:      ntime (32-bit LE)
        //   Word 3:      merkle_tail (last 4 bytes of merkle root, LE)
        //   Words 4-11:  midstate 0 (reversed word order, native u32 from BE source)
        //   Words 12-19: midstate 1 (same format)
        //   Words 20-27: midstate 2 (same format)
        //   Words 28-35: midstate 3 (same format)
        //   Total: 36 words = 144 bytes
        //
        // Since we only have 1 midstate, we duplicate it 4 times. The FPGA
        // cycles through all 4 slots, but since they're identical, every
        // nonce found is valid for our single midstate.

        if work.midstates.is_empty() {
            return Err(crate::AsicError::InvalidParameter(
                "no midstates provided".into(),
            ));
        }

        let mut words = [0u32; WORK_WORDS];

        // Word 0: Extended Work ID, shifted left by 2 for MIDSTATE_CNT=2.
        // The FPGA stores this and returns it in nonce responses.
        // decode_nonce() shifts right by 2 to recover the original work_id.
        words[0] = (work.work_id as u32) << MIDSTATE_CNT_LOG2;

        // Word 1: nbits
        words[1] = work.nbits;

        // Word 2: ntime
        words[2] = work.ntime;

        // Word 3: merkle_tail (last 4 bytes of merkle root)
        words[3] = u32::from_le_bytes(work.merkle_tail);

        // Encode midstate in REVERSED word order for FPGA.
        //
        // Our midstate is stored as big-endian bytes (from compute_midstate):
        //   bytes [0..4] = H0.to_be_bytes(), bytes [4..8] = H1.to_be_bytes(), etc.
        //
        // BraiinsOS does: `midstate_word.to_be()` where `midstate_word` comes from
        // `mid.state.words::<u32>()`. The `.words::<u32>()` does a RAW memory cast
        // of BE-stored bytes on a LE platform, so each u32 is already byte-swapped
        // relative to the true hash word value. Then `.to_be()` swaps again, and
        // the two swaps cancel out. The net effect: on LE ARM, MMIO stores the
        // midstate bytes in the OPPOSITE order from the BE source bytes.
        //
        // Our equivalent: `u32::from_be_bytes(...)` correctly recovers the native
        // H value. Writing this native u32 via MMIO on LE ARM stores it as LE bytes,
        // which is exactly the same LE byte order that BraiinsOS produces.
        //
        // DO NOT ADD .swap_bytes() here — that was proven wrong (all shares rejected
        // "Above target"). Without .swap_bytes(), first accepted shares achieved
        // 2026-03-17. The CE agent analysis was incorrect because it missed the
        // implicit byte swap from BraiinsOS's raw memory cast of BE bytes on LE ARM.
        // ASICBOOST (v0.8.2): Write DISTINCT midstates per FPGA slot.
        // With version rolling, WorkBuilder generates 4 midstates from different
        // rolled versions. Each FPGA slot gets a unique midstate → 4x nonce search
        // space → ~25% effective hashrate boost. Falls back to midstate[0] for all
        // slots when version rolling is disabled (only 1 midstate in Vec).
        //
        // CRITICAL: This change MUST be paired with validation + submission changes
        // in work_dispatcher.rs (use midstates[midstate_idx], not midstates[0]).
        for slot in 0..NUM_MIDSTATES {
            let ms_idx = if slot < work.midstates.len() { slot } else { 0 };
            let midstate = &work.midstates[ms_idx];
            let base = 4 + slot * 8;
            for i in 0..8 {
                let word_idx = 7 - i;
                words[base + i] = u32::from_be_bytes([
                    midstate[word_idx * 4],
                    midstate[word_idx * 4 + 1],
                    midstate[word_idx * 4 + 2],
                    midstate[word_idx * 4 + 3],
                ]); // NO .swap_bytes() — see 2026-03-17 proof
            }
        }

        // DIAGNOSTIC: Log first work item's FIFO words for byte-level comparison
        // with BraiinsOS. This is the exact data the FPGA receives.
        use std::sync::atomic::{AtomicBool, Ordering as AOrdering};
        static FIRST_WORK_LOGGED: AtomicBool = AtomicBool::new(false);
        if !FIRST_WORK_LOGGED.swap(true, AOrdering::Relaxed) {
            tracing::info!(
                chain_id = chain.chain_id,
                work_id = work.work_id,
                "WORK_TX_DIAG: First work item — 36 FIFO words (hex). Compare with BraiinsOS byte-for-byte.",
            );
            tracing::info!("WORK_TX[0] work_id_shifted = 0x{:08X}", words[0],);
            tracing::info!(
                "WORK_TX[1] nbits  = 0x{:08X} (LE bytes on AXI: {:02X} {:02X} {:02X} {:02X})",
                words[1],
                (words[1] & 0xFF) as u8,
                ((words[1] >> 8) & 0xFF) as u8,
                ((words[1] >> 16) & 0xFF) as u8,
                ((words[1] >> 24) & 0xFF) as u8,
            );
            tracing::info!(
                "WORK_TX[2] ntime  = 0x{:08X} (LE bytes on AXI: {:02X} {:02X} {:02X} {:02X})",
                words[2],
                (words[2] & 0xFF) as u8,
                ((words[2] >> 8) & 0xFF) as u8,
                ((words[2] >> 16) & 0xFF) as u8,
                ((words[2] >> 24) & 0xFF) as u8,
            );
            tracing::info!(
                "WORK_TX[3] merkle4 = 0x{:08X} (LE bytes on AXI: {:02X} {:02X} {:02X} {:02X})",
                words[3],
                (words[3] & 0xFF) as u8,
                ((words[3] >> 8) & 0xFF) as u8,
                ((words[3] >> 16) & 0xFF) as u8,
                ((words[3] >> 24) & 0xFF) as u8,
            );
            // Log midstate words 4-11 (first midstate slot)
            for i in 0..8 {
                tracing::info!(
                    "WORK_TX[{}] midstate[{}] = 0x{:08X}",
                    4 + i,
                    i,
                    words[4 + i],
                );
            }
        }

        // Write to WORK TX FIFO
        chain.write_work(&words);

        Ok(work.work_id)
    }

    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult> {
        // BM1387 nonce response (from WORK_RX_FIFO):
        //   Word 0: nonce value (32-bit)
        //   Word 1: [CRC:8 | extended_work_id:16 | solution_index:8]
        //     Bits [7:0]   = solution_index (which midstate found the nonce)
        //     Bits [23:8]  = extended_work_id (maps back to submitted work)
        //     Bits [31:24] = CRC (hardware CRC, can ignore)
        //
        // With MIDSTATE_CNT=2, the extended_work_id contains:
        //   - Low 2 bits: midstate index (which of 4 midstates found the nonce)
        //   - Remaining bits: original work_id (shifted left by 2 during send_work)
        // We shift right by MIDSTATE_CNT_LOG2 to recover the original work_id.
        //
        // BM1387: chip address is encoded in nonce bits [7:2] (6 bits, 0-63).
        // Each chip XORs its address into these nonce bits before sending the
        // response. This allows per-chip nonce attribution without metadata.
        // Source: BraiinsOS braiins_bm1387.rs:152, ASIC Register Bible line 1194.
        let nonce = raw[0];
        let w1 = raw[1];
        let solution_id = (w1 & 0xFF) as u8;
        let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
        let work_id = hw_work_id >> MIDSTATE_CNT_LOG2;
        // Midstate slot index: low 2 bits of hw_work_id (set by FPGA when cycling
        // through 4 midstate slots). This tells us WHICH midstate the ASIC used.
        let midstate_idx = (hw_work_id & ((1 << MIDSTATE_CNT_LOG2) - 1)) as u8;
        let chip_index = ((nonce >> 2) & 0x3F) as u8;

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
        // BM1387 mode: bit4=0 (NOT BM139X), bit3=1 (ENABLE), bits2:1=10 (4 midstates)
        // The s9io FPGA requires MIDSTATE_CNT=2 — MIDSTATE_CNT=0 breaks enumeration.
        // send_work() writes 36-word items with 4 copies of the same midstate.
        // work_id is shifted left by 2 in send_work, right by 2 in decode_nonce.
        fpga_chain::CTRL_ENABLE | (2 << fpga_chain::CTRL_MIDSTATE_SHIFT) // 0x0C: ENABLE + MIDSTATE_CNT=2 (4 midstates)
    }

    fn job_interval_ms(&self, _chip_count: u8, _freq_mhz: u16) -> u32 {
        // Work dispatch is now FIFO-driven (1ms poll), not timer-based.
        // The FPGA consumes work every WORK_TIME (~3.8ms at 500 MHz),
        // so we must feed it at least that fast. This value is only used
        // as a fallback; the actual dispatch loop checks TX FIFO space.
        1
    }

    fn ticket_mask(&self, difficulty: u32) -> u32 {
        // BraiinsOS encoding: (difficulty - 1).reverse_bits().swap_bytes()
        // For difficulty 256: 255 -> 0xFF000000 (reverse) -> 0x000000FF (swap) = 0xFF
        // Same result as simple `255` for power-of-2 difficulties, but correct for
        // arbitrary values (the ASIC compares bit-reversed nonce prefix against this mask).
        difficulty.saturating_sub(1).reverse_bits().swap_bytes()
    }

    fn pll_params(&self, freq_mhz: u16) -> PllConfig {
        // BM1387 PLL register: 0x0C
        // Reference clock: 25 MHz on-board crystal
        //
        // Known-good PLL register values extracted from bmminer-mix (Bitmain's
        // own S9 driver). These produce the correct frequencies on real hardware.
        //
        // NOTE: The internal bit-field layout of these register values does NOT
        // match BraiinsOS's PllReg definition (which uses a different divider
        // strategy). The fb_div/ref_div/post_div fields below are approximate —
        // only reg_value is written to hardware and is authoritative.
        //
        // Lookup table covers 100-900 MHz in ~25-50 MHz steps.
        // For intermediate frequencies, the nearest entry is used.

        let (reg_value, actual_freq) = bm1387_pll_lookup(freq_mhz);

        PllConfig {
            fb_div: actual_freq, // Use actual freq as label (divider decode is unreliable)
            ref_div: 0,
            post_div1: 0,
            post_div2: 0,
            reg_value,
        }
    }

    fn read_board_temp(&self, chain: &mut FpgaChain) -> Option<f32> {
        Bm1387Driver::read_board_temp(chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misc_ctrl_values_keep_asicboost_and_gate_block_enabled() {
        for value in [
            BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST,
            BM1387_MISC_CTRL_CONFIRM_GATE_ASICBOOST,
        ] {
            assert_ne!(
                value & MISC_CTRL_MMEN,
                0,
                "mmen/AsicBoost bit must stay set"
            );
            assert_ne!(
                value & MISC_CTRL_GATE_BLOCK,
                0,
                "gate_block must stay set before open-core"
            );
        }
        assert_eq!(
            BM1387_MISC_CTRL_GATE_AND_BAUD_ASICBOOST & MISC_CTRL_NOT_SET_BAUD,
            0,
            "first MiscCtrl write must perform the baud transition"
        );
        assert_ne!(
            BM1387_MISC_CTRL_CONFIRM_GATE_ASICBOOST & MISC_CTRL_NOT_SET_BAUD,
            0,
            "confirm MiscCtrl write must not trigger another baud transition"
        );
    }

    #[test]
    fn ctrl_mode_pins_four_midstate_asicboost_layout() {
        let ctrl = Bm1387Driver::new().ctrl_reg_value();
        assert_ne!(ctrl & fpga_chain::CTRL_ENABLE, 0);
        assert_eq!(
            (ctrl >> fpga_chain::CTRL_MIDSTATE_SHIFT) & 0x03,
            MIDSTATE_CNT_LOG2,
            "BM1387 S9 path must stay in 4-midstate FPGA mode"
        );
        assert_eq!(NUM_MIDSTATES, 4);
        assert_eq!(WORK_WORDS, 36);
    }
}
