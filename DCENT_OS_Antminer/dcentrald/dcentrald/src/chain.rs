//! Mining chain controller for dcentrald.
//!
//! Implements the complete mining data path between the work dispatcher and
//! the FPGA hardware. This module handles:
//!
//! - **Work submission**: Packing mining jobs into the FPGA WORK_TX_FIFO format
//!   with correct midstate word ordering (reversed) and extended work ID encoding.
//! - **Nonce collection**: Reading and decoding nonce results from WORK_RX_FIFO,
//!   extracting work_id, solution_index, and chip address metadata.
//! - **CMD send/receive**: Sending ASIC register commands via CMD_TX_FIFO and
//!   reading responses from CMD_RX_FIFO for chip configuration.
//! - **Baud rate control**: Setting the FPGA UART baud divisor for ASIC communication.
//! - **Fan speed control**: AXI Timer PWM configuration for fan speed management.
//!
//! # FPGA Work TX Format (S9 / Braiins s9io v1.0.2)
//!
//! Each work item written to WORK_TX_FIFO at chain_base + 0x3004:
//!
//! ```text
//! 1-midstate job (48 bytes = 12 words):
//!   Word 0:    Extended Work ID (16-bit, lower bits cleared per midstate config)
//!   Word 1:    nbits (network difficulty, little-endian)
//!   Word 2:    ntime (block timestamp, little-endian)
//!   Word 3:    merkle_tail (last 4 bytes of merkle root, little-endian)
//!   Words 4-11: midstate[0] in REVERSED word order: [7],[6],[5],[4],[3],[2],[1],[0]
//!
//! 4-midstate job (144 bytes = 36 words):
//!   Word 0:    Extended Work ID (16-bit, bits [1:0] = 0 for 4-midstate)
//!   Words 1-3: same header (nbits, ntime, merkle_tail)
//!   Words 4-11:  midstate[0] reversed
//!   Words 12-19: midstate[1] reversed
//!   Words 20-27: midstate[2] reversed
//!   Words 28-35: midstate[3] reversed
//! ```
//!
//! # FPGA Work RX Format (Nonce Responses)
//!
//! Each nonce response from WORK_RX_FIFO at chain_base + 0x2000:
//!
//! ```text
//!   Word 0: Raw nonce value (32-bit)
//!   Word 1: Metadata
//!     [7:0]   solution_index (midstate selector for version rolling)
//!     [23:8]  extended_work_id (matches work_id from submission)
//!     [31:24] CRC (for hardware error detection)
//! ```
//!
//! # CMD FIFO Format
//!
//! Commands are written to CMD_TX_FIFO at chain_base + 0x1004.
//! The FPGA handles UART framing (preamble 0x55 0xAA) and CRC.
//! Words are packed LSB-first.
//!
//! Responses are read from CMD_RX_FIFO at chain_base + 0x1000.
//! Each response is 2 x 32-bit words (register value + metadata).

use std::time::{Duration, Instant};

use dcentrald_hal::fan::FanController;
use dcentrald_hal::fpga_chain::{self, FpgaChain};
use dcentrald_hal::HalError;

use tracing::{debug, info, trace, warn};

// ---------------------------------------------------------------------------
// Work submission
// ---------------------------------------------------------------------------

/// Maximum number of midstates per work item (AsicBoost with version rolling).
pub const MAX_MIDSTATES: usize = 4;

/// Number of 32-bit words in the work header (work_id + nbits + ntime + merkle_tail).
pub const WORK_HEADER_WORDS: usize = 4;

/// Number of 32-bit words per midstate (256 bits = 8 words).
pub const MIDSTATE_WORDS: usize = 8;

/// Total words for 1-midstate work: 4 header + 8 midstate = 12 words = 48 bytes.
pub const WORK_1MS_WORDS: usize = WORK_HEADER_WORDS + MIDSTATE_WORDS;

/// Total words for 4-midstate work: 4 header + 32 midstate = 36 words = 144 bytes.
pub const WORK_4MS_WORDS: usize = WORK_HEADER_WORDS + (4 * MIDSTATE_WORDS);

/// Maximum work items buffered in WORK_TX_FIFO.
/// FIFO depth is 2048 words. At 12 words/job = 170 jobs, at 36 words/job = 56 jobs.
pub const MAX_WORK_1MS: usize = 170;
pub const MAX_WORK_4MS: usize = 56;

/// A mining job ready to be submitted to the FPGA.
///
/// This struct contains all the fields needed to build a WORK_TX_FIFO
/// packet. It is chip-agnostic -- the FPGA format is the same for all
/// BM13xx chips on the same FPGA bitstream (s9io v1.0.2).
#[derive(Clone)]
pub struct FpgaWork {
    /// Work ID (16-bit extended work ID for the FPGA).
    /// Lower 1-2 bits must be zero depending on midstate count.
    pub work_id: u16,
    /// Network difficulty target (nBits field from block header).
    pub nbits: u32,
    /// Block timestamp (nTime field from block header).
    pub ntime: u32,
    /// Last 4 bytes of the merkle root (merkle_tail).
    pub merkle_tail: [u8; 4],
    /// SHA-256 midstate(s) of the first 64 bytes of the block header.
    /// 1 midstate for BM1387 (no AsicBoost), 4 for BM1397+ (version rolling).
    pub midstates: Vec<[u8; 32]>,
}

/// Decoded nonce result from the FPGA WORK_RX_FIFO.
#[derive(Debug, Clone)]
pub struct FpgaNonce {
    /// The raw nonce value found by an ASIC chip (32-bit).
    pub nonce: u32,
    /// Extended work ID that this nonce belongs to (matches FpgaWork::work_id).
    pub work_id: u16,
    /// Solution index (midstate selector for multi-midstate work).
    /// For 1-midstate mode, always 0.
    /// For 4-midstate mode, indicates which version variant found the solution.
    pub solution_index: u8,
    /// Hardware CRC from the FPGA (for error detection).
    pub hw_crc: u8,
    /// Raw word 0 from FIFO (for debugging).
    pub raw_w0: u32,
    /// Raw word 1 from FIFO (for debugging).
    pub raw_w1: u32,
}

// NOTE: The old `submit_work()` and `write_midstate_reversed()` functions were
// removed — the correct work submission path is `bm1387::send_work()` via the
// ChipDriver trait, called by WorkDispatcher. See bm1387.rs send_work().
// IMPORTANT: midstate words must NOT have .swap_bytes() — see bm1387.rs comment.

/// Check how many work items can be submitted before the FIFO is full.
///
/// Reads the WORK_TX_STAT register to determine if the FIFO can accept
/// more work. Returns true if not full.
pub fn work_tx_ready(chain: &FpgaChain) -> bool {
    !chain.work_tx_full()
}

/// Check if the WORK_TX_FIFO is completely empty (all work dispatched to ASICs).
pub fn work_tx_empty(chain: &FpgaChain) -> bool {
    let stat = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_STAT);
    stat & fpga_chain::STAT_TX_EMPTY != 0
}

/// Read the last work ID that was sent to the ASICs by the FPGA.
pub fn work_tx_last_id(chain: &FpgaChain) -> u32 {
    chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_LAST)
}

// ---------------------------------------------------------------------------
// Nonce collection
// ---------------------------------------------------------------------------

/// Collect all available nonces from the WORK_RX_FIFO.
///
/// Reads nonce pairs (2 words each) from the FIFO until it is empty.
/// Returns a vector of decoded nonce results.
///
/// This should be called frequently (every 10ms in the work dispatcher)
/// to prevent FIFO overflow. The FIFO holds 1024 words = 512 nonces.
/// At full hashrate on an S9 (~14 TH/s with difficulty 256), nonces
/// arrive at ~200/second, so the FIFO has ~2.5 seconds of buffer.
pub fn collect_nonces(chain: &FpgaChain) -> Vec<FpgaNonce> {
    let mut nonces = Vec::new();

    // Safety limit to prevent infinite loop on hardware fault
    let max_reads = 512;
    let mut reads = 0;

    while chain.work_rx_has_data() && reads < max_reads {
        // Each nonce is 2 x 32-bit words from the FIFO
        let w0 = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_FIFO);
        let w1 = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_FIFO);

        nonces.push(decode_nonce(w0, w1));
        reads += 1;
    }

    if reads >= max_reads {
        warn!(
            chain_id = chain.chain_id,
            "WORK_RX_FIFO read limit reached ({} nonces) -- possible FIFO overflow or stuck",
            max_reads
        );
    }

    nonces
}

/// Read a single nonce from the WORK_RX_FIFO.
///
/// Returns `None` if the FIFO is empty.
pub fn read_nonce(chain: &FpgaChain) -> Option<FpgaNonce> {
    if !chain.work_rx_has_data() {
        return None;
    }

    let w0 = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_FIFO);
    let w1 = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_FIFO);

    Some(decode_nonce(w0, w1))
}

/// Check if nonces are available in the WORK_RX_FIFO.
pub fn nonces_available(chain: &FpgaChain) -> bool {
    chain.work_rx_has_data()
}

/// Check if the WORK_RX_FIFO is full (nonces being lost!).
pub fn nonce_fifo_full(chain: &FpgaChain) -> bool {
    let stat = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_STAT);
    stat & fpga_chain::STAT_RX_FULL != 0
}

/// Decode raw FIFO words into a structured nonce result.
///
/// WORK_RX_FIFO format (from Braiins s9io documentation):
///   Word 0: Raw nonce value (32-bit)
///   Word 1: [solution_index:8 | extended_work_id:16 | crc:8]
///     Bits [7:0]   = solution_index (midstate selector)
///     Bits [23:8]  = extended_work_id (matches submitted work_id)
///     Bits [31:24] = CRC (hardware error detection)
fn decode_nonce(w0: u32, w1: u32) -> FpgaNonce {
    FpgaNonce {
        nonce: w0,
        solution_index: (w1 & 0xFF) as u8,
        work_id: ((w1 >> 8) & 0xFFFF) as u16,
        hw_crc: ((w1 >> 24) & 0xFF) as u8,
        raw_w0: w0,
        raw_w1: w1,
    }
}

// ---------------------------------------------------------------------------
// CMD FIFO operations (ASIC register read/write)
// ---------------------------------------------------------------------------

/// A decoded ASIC command response from the CMD_RX_FIFO.
#[derive(Debug, Clone)]
pub struct CmdResponse {
    /// First word of the response (register data or chip info).
    pub word0: u32,
    /// Second word of the response (metadata: chip addr, CRC, etc.).
    pub word1: u32,
}

/// Send a command word to the ASIC chain via CMD_TX_FIFO.
///
/// The FPGA handles UART framing (preamble, CRC) automatically.
/// Words are packed LSB-first as defined in the protocol module.
///
/// For multi-word commands (e.g., full 32-bit register writes),
/// call this function for each word in sequence.
pub fn send_cmd(chain: &FpgaChain, cmd_word: u32) {
    chain.write_cmd(cmd_word);
}

/// Send a multi-word command to the CMD_TX_FIFO.
pub fn send_cmd_words(chain: &FpgaChain, words: &[u32]) {
    for &word in words {
        chain.write_cmd(word);
    }
}

/// Read all available responses from the CMD_RX_FIFO.
///
/// Each response is 2 x 32-bit words. Returns a vector of decoded responses.
/// Used during chip enumeration and register reads.
pub fn collect_cmd_responses(chain: &FpgaChain) -> Vec<CmdResponse> {
    let mut responses = Vec::new();

    // Safety limit (CMD_RX_FIFO is 256 words = 128 responses max)
    let max_reads = 128;
    let mut reads = 0;

    while chain.cmd_rx_has_data() && reads < max_reads {
        let word0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        let word1 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        responses.push(CmdResponse { word0, word1 });
        reads += 1;
    }

    responses
}

/// Read a single command response from the CMD_RX_FIFO.
///
/// Returns `None` if the FIFO is empty.
pub fn read_cmd_response(chain: &FpgaChain) -> Option<CmdResponse> {
    if !chain.cmd_rx_has_data() {
        return None;
    }

    let word0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
    let word1 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);

    Some(CmdResponse { word0, word1 })
}

/// Send a command and wait for a response with timeout.
///
/// Sends a command word to the CMD_TX_FIFO, then polls the CMD_RX_FIFO
/// for a response. Returns the first response received, or a timeout error.
///
/// # Arguments
///
/// * `chain` - The FPGA chain to send the command on.
/// * `cmd_word` - The command word to send.
/// * `timeout` - Maximum time to wait for a response.
pub fn send_cmd_wait(
    chain: &FpgaChain,
    cmd_word: u32,
    timeout: Duration,
) -> Result<CmdResponse, ChainError> {
    // Clear any stale responses
    while chain.cmd_rx_has_data() {
        let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
    }

    // Send the command
    chain.write_cmd(cmd_word);

    // Poll for response
    let start = Instant::now();
    loop {
        if chain.cmd_rx_has_data() {
            let word0 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            let word1 = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
            return Ok(CmdResponse { word0, word1 });
        }

        if start.elapsed() > timeout {
            return Err(ChainError::CmdTimeout {
                chain_id: chain.chain_id,
                cmd: cmd_word,
                timeout_ms: timeout.as_millis() as u32,
            });
        }

        // Brief yield to avoid busy-spinning
        std::thread::yield_now();
    }
}

/// Send a command and collect all responses with timeout.
///
/// Similar to `send_cmd_wait`, but collects ALL responses (useful for
/// broadcast commands like GetAddress where every chip responds).
///
/// # Arguments
///
/// * `chain` - The FPGA chain.
/// * `cmd_word` - The command word to send.
/// * `wait_time` - Time to wait for all responses to arrive.
pub fn send_cmd_collect(chain: &FpgaChain, cmd_word: u32, wait_time: Duration) -> Vec<CmdResponse> {
    // Clear any stale responses
    while chain.cmd_rx_has_data() {
        let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
        let _ = chain.cmd.read_reg(fpga_chain::REG_CMD_RX_FIFO);
    }

    // Send the command
    chain.write_cmd(cmd_word);

    // Wait for responses to arrive
    std::thread::sleep(wait_time);

    // Collect all responses
    collect_cmd_responses(chain)
}

// ---------------------------------------------------------------------------
// Chain initialization helpers
// ---------------------------------------------------------------------------

/// Reset all FIFOs on a chain (CMD TX/RX, WORK TX/RX).
///
/// This is required before starting a new mining session or after
/// detecting communication errors. Follows the verified sequence from
/// asic_comm_test.c:
///   CMD_CTRL = 0x03 (RST_TX | RST_RX) -> delay -> CMD_CTRL = 0x04 (IRQ_EN)
pub fn reset_chain_fifos(chain: &FpgaChain) {
    chain.reset_fifos();
    debug!(chain_id = chain.chain_id, "All FIFOs reset");
}

/// Enable a chain in the FPGA CTRL_REG.
///
/// # Arguments
///
/// * `chain` - The FPGA chain to enable.
/// * `bm139x` - Set true for BM1397+ chips (changes UART handling in FPGA).
/// * `midstate_count` - Number of midstates per work (1, 2, or 4).
pub fn enable_chain(chain: &FpgaChain, bm139x: bool, midstate_count: u8) {
    let midstate_bits = match midstate_count {
        1 => 0u32,
        2 => 1u32,
        4 => 2u32,
        _ => 0u32,
    };

    let ctrl = fpga_chain::CTRL_ENABLE
        | if bm139x { fpga_chain::CTRL_BM139X } else { 0 }
        | (midstate_bits << fpga_chain::CTRL_MIDSTATE_SHIFT);

    chain.common.write_reg(fpga_chain::REG_CTRL, ctrl);

    debug!(
        chain_id = chain.chain_id,
        ctrl = format_args!("0x{:08X}", ctrl),
        bm139x,
        midstates = midstate_count,
        "Chain enabled"
    );
}

// `disable_chain()` helper removed (2026-04-16 flash-readiness review).
// Writing 0 to CTRL_REG permanently breaks the FPGA UART state machine once
// UART traffic has flowed (proven on live S9 2026-03-12). A post-mining teardown
// does not need to clear ENABLE — the process exit drops the mmap and the FPGA
// stays in its last state, which also honors hash-on-disconnect. For clean
// reset between runs, use `chain.reset_ip_core()` (read-modify-write of ENABLE
// that preserves MIDSTATE_CNT). If any caller genuinely needs to set ENABLE=0
// BEFORE any UART traffic (cold-boot reset_and_enumerate pattern), they must
// call `chain.set_enabled(false)` directly with clear intent.

// ---------------------------------------------------------------------------
// Baud rate control
// ---------------------------------------------------------------------------

/// Set the UART baud rate on a chain.
///
/// The FPGA BAUD_REG controls the UART speed for ASIC communication.
/// Formula: `baud = 200_000_000 / (16 * (divisor + 1))`
///
/// # Common baud rates
///
/// | Divisor | Baud Rate   | Use Case                    |
/// |---------|-------------|-----------------------------|
/// | 0x6C    | 114,679     | Initial ASIC enumeration    |
/// | 0x07    | 1,562,500   | Normal mining operation     |
/// | 0x03    | 3,125,000   | High-speed mining (tested)  |
pub fn set_baud_rate(chain: &FpgaChain, baud: u32) {
    let divisor = crate::fpga::divisor_from_baud(baud);
    let actual = crate::fpga::baud_from_divisor(divisor);

    chain.set_baud(divisor);

    debug!(
        chain_id = chain.chain_id,
        requested = baud,
        actual,
        divisor = format_args!("0x{:02X}", divisor),
        "Baud rate set"
    );
}

/// Set the UART baud rate by directly writing the divisor value.
pub fn set_baud_divisor(chain: &FpgaChain, divisor: u32) {
    chain.set_baud(divisor);
}

/// Set the inter-work delay timer (WORK_TIME register).
///
/// This controls the minimum time between consecutive work items sent
/// to the ASIC chain. The reset value is 1 (minimum delay). The verified
/// operational value from bosminer is 0x0004_0507.
pub fn set_work_time(chain: &FpgaChain, work_time: u32) {
    chain.common.write_reg(fpga_chain::REG_WORK_TIME, work_time);
    debug!(
        chain_id = chain.chain_id,
        work_time = format_args!("0x{:08X}", work_time),
        "Work time set"
    );
}

// ---------------------------------------------------------------------------
// CRC error tracking
// ---------------------------------------------------------------------------

/// Read the CRC error counter for a chain.
///
/// The FPGA counts CRC errors on ASIC responses. High error counts
/// indicate signal integrity problems (bad cable, connector, level shifter).
///
/// Chain 6 on this S9 consistently shows higher CRC errors than chains 7/8,
/// indicating marginal signal integrity on the J6 connector.
pub fn read_crc_errors(chain: &FpgaChain) -> u32 {
    chain.read_error_count()
}

/// Clear the CRC error counter.
pub fn clear_crc_errors(chain: &FpgaChain) {
    chain.clear_error_count();
    debug!(chain_id = chain.chain_id, "CRC error counter cleared");
}

// ---------------------------------------------------------------------------
// Fan control
// ---------------------------------------------------------------------------

/// Fan command range: 0 (minimum command) to 100 (maximum command).
///
/// Older Bitmain-style docs describe a 7-bit 0-127 field, but the AM2/XIL
/// fan IP and the shared HAL clamp commands to 0-100. Values above 100 are
/// saturated and must not be used as a separate safety mode.
pub const FAN_PWM_MIN: u8 = 0;
pub const FAN_PWM_MAX: u8 = dcentrald_hal::fan::PWM_MAX;
pub const FAN_PWM_QUIET: u8 = 10;
pub const FAN_PWM_SAFETY: u8 = dcentrald_hal::fan::PWM_SAFETY_MAX;

/// Set fan speed via the AXI Timer PWM controller.
///
/// The S9 has a single PWM controller for all fans. AM2/XIL exposes two PWM
/// command banks plus separate tach channels through the same HAL abstraction.
/// Command range is 0-100.
///
/// # Calibration (measured on live S9)
///
/// | PWM | RPM    | Description          |
/// |-----|--------|----------------------|
/// | 0   | ~900   | Hardware minimum      |
/// | 10  | ~1260  | Quiet boot default   |
/// | 20  | ~2340  | Whisper quiet         |
/// | 50  | ~4500  | Moderate              |
/// | 100 | ~5940  | Maximum (saturated)  |
pub fn set_fan_speed(fan: &FanController, pwm: u8) {
    fan.set_speed(pwm.min(FAN_PWM_MAX));
}

/// Read current fan RPM from the tachometer.
pub fn get_fan_rpm(fan: &FanController) -> u32 {
    fan.get_rpm()
}

/// Read current fan PWM setting.
pub fn get_fan_pwm(fan: &FanController) -> u8 {
    fan.get_speed_pwm()
}

/// Emergency fan override -- hold fans at the home safety cap.
///
/// Called when any safety condition is triggered:
/// - Temperature sensor read failure
/// - Fan tach reads 0 for >5 seconds
/// - Chip temp exceeds 65C
/// - Mining daemon crash with hash boards powered
pub fn fan_safety_override(fan: &FanController) {
    fan.set_speed(FAN_PWM_SAFETY);
    warn!("Fan safety override: fans commanded to PWM 30 (home-safe)");
}

// ---------------------------------------------------------------------------
// Mining chain controller (high-level)
// ---------------------------------------------------------------------------

/// High-level mining chain controller that wraps an FPGA chain with
/// mining-specific state and operations.
///
/// This is the struct used by the work dispatcher to interact with
/// a single hash board during mining.
pub struct MiningChain {
    /// The underlying FPGA chain register access.
    pub fpga: FpgaChain,
    /// Chain ID (6, 7, or 8 on S9).
    pub chain_id: u8,
    /// Number of ASIC chips detected on this chain.
    pub chip_count: u8,
    /// Detected chip ID (e.g., 0x1387 for BM1387).
    pub chip_id: u16,
    /// Current ASIC frequency in MHz.
    pub frequency_mhz: u16,
    /// Current chain voltage in millivolts.
    pub voltage_mv: u16,
    /// Whether this chain is actively mining.
    pub mining: bool,
    /// Work ID counter for this chain (wraps at 16-bit boundary).
    work_id_counter: u16,
    /// Number of midstates per work item on this chain.
    midstate_count: u8,
    /// Total nonces received on this chain.
    pub total_nonces: u64,
    /// Total work items submitted on this chain.
    pub total_work_submitted: u64,
}

impl MiningChain {
    /// Create a new mining chain from an FPGA chain controller.
    pub fn new(fpga: FpgaChain, chain_id: u8) -> Self {
        Self {
            fpga,
            chain_id,
            chip_count: 0,
            chip_id: 0,
            frequency_mhz: 0,
            voltage_mv: 0,
            mining: false,
            work_id_counter: 0,
            midstate_count: 1,
            total_nonces: 0,
            total_work_submitted: 0,
        }
    }

    /// Set the midstate count for this chain (1, 2, or 4).
    pub fn set_midstate_count(&mut self, count: u8) {
        self.midstate_count = count.clamp(1, 4);
    }

    /// Allocate the next work ID, respecting midstate alignment.
    ///
    /// For 1-midstate: any 16-bit value.
    /// For 2-midstate: must be even (bit 0 = 0).
    /// For 4-midstate: must be divisible by 4 (bits 1:0 = 0).
    pub fn next_work_id(&mut self) -> u16 {
        let id = self.work_id_counter;
        let increment = match self.midstate_count {
            1 => 1u16,
            2 => 2,
            4 => 4,
            _ => 1,
        };
        self.work_id_counter = self.work_id_counter.wrapping_add(increment);
        id
    }

    /// Collect all available nonces from this chain.
    pub fn collect_nonces(&mut self) -> Vec<FpgaNonce> {
        let nonces = collect_nonces(&self.fpga);
        self.total_nonces += nonces.len() as u64;
        nonces
    }

    /// Read a single nonce from this chain.
    pub fn read_nonce(&mut self) -> Option<FpgaNonce> {
        let nonce = read_nonce(&self.fpga);
        if nonce.is_some() {
            self.total_nonces += 1;
        }
        nonce
    }

    /// Check if nonces are available.
    pub fn has_nonces(&self) -> bool {
        nonces_available(&self.fpga)
    }

    /// Check if the work FIFO is ready for more work.
    pub fn work_ready(&self) -> bool {
        work_tx_ready(&self.fpga)
    }

    /// Get CRC error count.
    pub fn crc_errors(&self) -> u32 {
        read_crc_errors(&self.fpga)
    }

    /// Clear CRC error counter.
    pub fn clear_errors(&self) {
        clear_crc_errors(&self.fpga);
    }

    /// Initialize the chain for mining.
    ///
    /// Sets baud rate, resets FIFOs, enables the chain controller.
    /// Must be called after chip enumeration and before submitting work.
    pub fn init_for_mining(&mut self, bm139x: bool) {
        reset_chain_fifos(&self.fpga);
        enable_chain(&self.fpga, bm139x, self.midstate_count);
        set_work_time(&self.fpga, fpga_chain::WORK_TIME_DEFAULT);
        clear_crc_errors(&self.fpga);
        self.mining = true;
        self.work_id_counter = 0;
        self.total_nonces = 0;
        self.total_work_submitted = 0;

        info!(
            chain_id = self.chain_id,
            chip_count = self.chip_count,
            chip_id = format_args!("0x{:04X}", self.chip_id),
            midstates = self.midstate_count,
            "Mining chain initialized"
        );
    }

    /// Stop mining on this chain.
    ///
    /// Uses `reset_ip_core()` (read-modify-write of ENABLE that preserves
    /// MIDSTATE_CNT), NOT a raw `set_enabled(false)` which would zero CTRL_REG
    /// and permanently break the FPGA UART state machine (see fpga_chain.rs).
    /// Safe to call multiple times; safe to call on shutdown.
    pub fn stop_mining(&mut self) {
        self.fpga.reset_ip_core();
        self.mining = false;

        info!(
            chain_id = self.chain_id,
            total_nonces = self.total_nonces,
            total_work = self.total_work_submitted,
            crc_errors = self.crc_errors(),
            "Mining chain stopped"
        );
    }

    /// Send an ASIC command and wait for response.
    pub fn send_cmd_wait(
        &self,
        cmd_word: u32,
        timeout: Duration,
    ) -> Result<CmdResponse, ChainError> {
        send_cmd_wait(&self.fpga, cmd_word, timeout)
    }

    /// Send an ASIC command and collect all responses.
    pub fn send_cmd_collect(&self, cmd_word: u32, wait_time: Duration) -> Vec<CmdResponse> {
        send_cmd_collect(&self.fpga, cmd_word, wait_time)
    }

    /// Set baud rate on this chain.
    pub fn set_baud(&self, baud: u32) {
        set_baud_rate(&self.fpga, baud);
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors specific to the chain controller.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    /// The WORK_TX_FIFO is full -- cannot submit more work.
    #[error("chain {chain_id}: WORK_TX_FIFO full, cannot submit work")]
    WorkTxFull { chain_id: u8 },

    /// Invalid work parameters.
    #[error("chain {chain_id}: invalid work: {detail}")]
    InvalidWork { chain_id: u8, detail: String },

    /// Command response timeout.
    #[error("chain {chain_id}: CMD timeout for 0x{cmd:08X} after {timeout_ms}ms")]
    CmdTimeout {
        chain_id: u8,
        cmd: u32,
        timeout_ms: u32,
    },

    /// No chips detected.
    #[error("chain {chain_id}: no chips detected")]
    NoChips { chain_id: u8 },

    /// HAL error.
    #[error("HAL error: {0}")]
    Hal(#[from] HalError),
}

// ---------------------------------------------------------------------------
// Work ID management
// ---------------------------------------------------------------------------

/// Work ID allocator that tracks mappings between work IDs and pool jobs.
///
/// The FPGA uses a 16-bit work ID to match nonces back to submitted work.
/// This allocator manages the work ID space and maintains a ring buffer
/// of active work entries for nonce-to-job matching.
pub struct WorkIdTracker {
    /// Ring buffer of work entries indexed by work_id.
    /// Size is 2^16 = 65536 entries for full 16-bit work ID space.
    /// Using a smaller ring (e.g., 256 entries) and masking the work_id
    /// is more practical.
    entries: Vec<Option<WorkIdEntry>>,
    /// Mask for indexing into the ring buffer.
    mask: u16,
}

/// A work entry in the work ID tracker.
#[derive(Clone, Debug)]
pub struct WorkIdEntry {
    /// Pool job ID (for share submission).
    pub job_id: String,
    /// Extranonce2 used for this work.
    pub extranonce2: String,
    /// ntime used for this work.
    pub ntime: u32,
    /// Version bits (for version rolling).
    pub version_bits: u32,
    /// Share target (for difficulty checking).
    pub share_target: [u8; 32],
    /// Timestamp when this work was submitted.
    pub submitted_at: Instant,
}

impl WorkIdTracker {
    /// Create a new work ID tracker with a ring buffer of the given size.
    ///
    /// `ring_size` must be a power of 2. Recommended: 256 or 1024.
    pub fn new(ring_size: usize) -> Self {
        assert!(
            ring_size.is_power_of_two(),
            "ring_size must be a power of 2"
        );
        let mask = (ring_size - 1) as u16;
        Self {
            entries: vec![None; ring_size],
            mask,
        }
    }

    /// Store a work entry at the given work_id.
    pub fn insert(&mut self, work_id: u16, entry: WorkIdEntry) {
        let idx = (work_id & self.mask) as usize;
        self.entries[idx] = Some(entry);
    }

    /// Look up a work entry by work_id.
    pub fn get(&self, work_id: u16) -> Option<&WorkIdEntry> {
        let idx = (work_id & self.mask) as usize;
        self.entries[idx].as_ref()
    }

    /// Clear all entries (on new block / clean job).
    pub fn clear(&mut self) {
        self.entries.iter_mut().for_each(|e| *e = None);
    }

    /// Remove a stale entry.
    pub fn remove(&mut self, work_id: u16) {
        let idx = (work_id & self.mask) as usize;
        self.entries[idx] = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_id_tracker() {
        let mut tracker = WorkIdTracker::new(256);

        let entry = WorkIdEntry {
            job_id: "abc123".to_string(),
            extranonce2: "00000001".to_string(),
            ntime: 0x12345678,
            version_bits: 0,
            share_target: [0xFF; 32],
            submitted_at: Instant::now(),
        };

        tracker.insert(42, entry.clone());
        assert!(tracker.get(42).is_some());
        assert_eq!(tracker.get(42).unwrap().job_id, "abc123");

        // Work ID 42+256 should collide (ring buffer)
        tracker.insert(
            42 + 256,
            WorkIdEntry {
                job_id: "new_job".to_string(),
                ..entry
            },
        );
        assert_eq!(tracker.get(42).unwrap().job_id, "new_job");

        tracker.clear();
        assert!(tracker.get(42).is_none());
    }

    #[test]
    fn emergency_fan_override_commands_the_home_safe_cap_not_a_blast() {
        // fan_safety_override() — the path on temp-sensor failure, tach=0 >5s,
        // overtemp, or a daemon crash with boards powered — must command the
        // home-safe cap, NEVER a blast. It writes FAN_PWM_SAFETY, which MUST be
        // the 30-PWM safety ceiling and MUST stay below the hardware max. If this
        // alias were ever pointed at FAN_PWM_MAX (100), every emergency would
        // blast the fans — the exact fan-never-blast regression the rust-firmware
        // rule forbids ("cut hash before you raise noise").
        assert_eq!(
            FAN_PWM_SAFETY, 30,
            "emergency fan override must command PWM 30"
        );
        assert_eq!(FAN_PWM_SAFETY, dcentrald_hal::fan::PWM_SAFETY_MAX);
        assert!(
            FAN_PWM_SAFETY < FAN_PWM_MAX,
            "safety cap ({FAN_PWM_SAFETY}) must be strictly below the hardware max ({FAN_PWM_MAX})"
        );
    }

    #[test]
    fn test_decode_nonce() {
        // Simulated nonce response from FPGA:
        // Word 0: nonce = 0xDEADBEEF
        // Word 1: solution_idx=0x02, work_id=0x0034, crc=0xAB
        let w0 = 0xDEADBEEF;
        let w1 = 0xAB003402; // [crc:8 | work_id_hi:8 | work_id_lo:8 | solution:8]

        let nonce = decode_nonce(w0, w1);

        assert_eq!(nonce.nonce, 0xDEADBEEF);
        assert_eq!(nonce.solution_index, 0x02);
        assert_eq!(nonce.work_id, 0x0034);
        assert_eq!(nonce.hw_crc, 0xAB);
    }

    #[test]
    fn test_midstate_count_alignment() {
        // 1-midstate: any work_id is fine
        let mut chain_state = MiningChainState {
            work_id_counter: 0,
            midstate_count: 1,
        };
        assert_eq!(chain_state.next_work_id(), 0);
        assert_eq!(chain_state.next_work_id(), 1);
        assert_eq!(chain_state.next_work_id(), 2);

        // 4-midstate: work_id must be divisible by 4
        chain_state.work_id_counter = 0;
        chain_state.midstate_count = 4;
        assert_eq!(chain_state.next_work_id(), 0);
        assert_eq!(chain_state.next_work_id(), 4);
        assert_eq!(chain_state.next_work_id(), 8);
    }

    /// Helper struct for testing work ID allocation without full MiningChain.
    struct MiningChainState {
        work_id_counter: u16,
        midstate_count: u8,
    }

    impl MiningChainState {
        fn next_work_id(&mut self) -> u16 {
            let id = self.work_id_counter;
            let increment = match self.midstate_count {
                1 => 1u16,
                2 => 2,
                4 => 4,
                _ => 1,
            };
            self.work_id_counter = self.work_id_counter.wrapping_add(increment);
            id
        }
    }
}
