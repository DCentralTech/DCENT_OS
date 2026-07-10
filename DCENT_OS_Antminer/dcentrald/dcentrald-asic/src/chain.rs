//! Hash chain management.
//!
//! The Chain struct wraps an FPGA chain (or other ChainAccess implementation)
//! with higher-level operations: chip enumeration, address assignment, and
//! command sequencing. It maintains the detected chip type and count.

use dcentrald_hal::fpga_chain::FpgaChain;
use dcentrald_hal::serial_chain::SerialChainBackend;

use crate::drivers::{ChipDriver, PicType};
use crate::Result;

#[inline]
fn production_chip_id_known(chip_id: u16) -> bool {
    crate::drivers::ChipRegistry::production()
        .detect(chip_id)
        .is_some()
}

/// Return whether a detected chain satisfies an operator-configured minimum
/// chip-population fraction.
///
/// `floor = 0.0` preserves historical partial-enumeration behavior. Invalid
/// floor values fail closed here even though daemon config validation rejects
/// them before runtime.
pub fn chain_meets_min_fraction(count: u8, expected: u8, floor: f32) -> bool {
    if !floor.is_finite() || !(0.0..=1.0).contains(&floor) {
        return false;
    }
    if floor == 0.0 || expected == 0 {
        return true;
    }
    (count as f32 / expected as f32) >= floor
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergentChipPolicy {
    Enforce,
    LogOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainDriverDecision {
    Drive,
    SkipDivergent,
    LogOnlyDivergent,
}

/// Decide whether a chain should be driven by the already-latched chip driver.
///
/// A zero ID means no reliable physical signal and preserves historical flow.
/// Divergent IDs only become actionable when both sides are known production
/// ASICs, which keeps malformed/noisy IDs on the existing fail-soft path.
pub fn driver_for_chain_with_policy(
    latched_chip_id: u16,
    chain_chip_id: u16,
    policy: DivergentChipPolicy,
) -> ChainDriverDecision {
    if latched_chip_id == 0 || chain_chip_id == 0 || latched_chip_id == chain_chip_id {
        return ChainDriverDecision::Drive;
    }
    if production_chip_id_known(latched_chip_id) && production_chip_id_known(chain_chip_id) {
        return match policy {
            DivergentChipPolicy::Enforce => ChainDriverDecision::SkipDivergent,
            DivergentChipPolicy::LogOnly => ChainDriverDecision::LogOnlyDivergent,
        };
    }
    ChainDriverDecision::Drive
}

pub fn driver_for_chain(latched_chip_id: u16, chain_chip_id: u16) -> ChainDriverDecision {
    driver_for_chain_with_policy(latched_chip_id, chain_chip_id, DivergentChipPolicy::Enforce)
}

/// Represents one hash board chain with its detected chips.
pub struct Chain {
    /// FPGA chain register access.
    pub fpga: FpgaChain,

    /// Serial command channel for hybrid mode (am2-s17).
    /// When present, ASIC commands (GetAddress, register writes) route through
    /// /dev/ttyS* instead of the FPGA CMD FIFO. Work dispatch still uses the
    /// FPGA WORK_TX/RX FIFOs.
    pub serial: Option<SerialChainBackend>,

    /// Chain ID (6, 7, or 8 on S9; 1, 2, or 3 on am2-s17).
    pub chain_id: u8,

    /// Number of chips detected on this chain.
    pub chip_count: u8,

    /// Detected chip ID (e.g., 0x1387 for BM1387).
    pub chip_id: u16,

    /// Current ASIC frequency in MHz.
    pub frequency_mhz: u16,

    /// Current chain voltage in millivolts.
    pub voltage_mv: u16,

    /// Whether this chain is actively mining.
    pub mining: bool,

    /// FPGA MIDSTATE_CNT from CTRL register (set by prior firmware).
    /// 2 = 4 midstate slots (36-word packets), 3 = 8 midstate slots (68-word packets).
    /// Used to adapt work packet format in passthrough mode.
    pub fpga_midstate_cnt: u8,

    /// I2C address of the PIC/dsPIC voltage controller for this chain.
    /// S9: 0x55/0x56/0x57 (PIC16F1704), S19 Pro: 0x20/0x21/0x22 (dsPIC33EP).
    /// None for NoPic architectures (S21 uses TAS5782M DACs via kernel DTB).
    pub pic_address: Option<u8>,

    /// Effective voltage-controller type for this chain.
    /// This may come from a shared chip-family profile or from a model-specific
    /// override when sibling miners use different board-control realities.
    pub pic_type: PicType,
}

impl Chain {
    /// Create a new chain wrapper around an FPGA chain (FPGA-only mode).
    pub fn new(fpga: FpgaChain, chain_id: u8) -> Self {
        Self {
            fpga,
            serial: None,
            chain_id,
            chip_count: 0,
            chip_id: 0,
            frequency_mhz: 0,
            voltage_mv: 0,
            mining: false,
            fpga_midstate_cnt: 2, // Default: 4 slots (S9 compatible)
            pic_address: None,
            pic_type: PicType::Pic16F1704,
        }
    }

    /// Create a new hybrid chain: serial commands + FPGA work dispatch.
    ///
    /// Used on am2-s17 platforms (S17/S19/S19j) where ASIC commands flow through
    /// PL UARTs (/dev/ttyS1-3) and work dispatch uses FPGA WORK_TX/RX FIFOs.
    pub fn new_hybrid(fpga: FpgaChain, serial: SerialChainBackend, chain_id: u8) -> Self {
        Self {
            fpga,
            serial: Some(serial),
            chain_id,
            chip_count: 0,
            chip_id: 0,
            frequency_mhz: 0,
            voltage_mv: 0,
            mining: false,
            fpga_midstate_cnt: 2,
            pic_address: None,
            pic_type: PicType::Pic16F1704,
        }
    }

    /// Enumerate chips on this chain with multi-baud-rate fallback.
    ///
    /// Tries GetAddress at 115200 first (default after power-cycle), then falls
    /// back to 1.5 Mbaud and 3.125 Mbaud (ASICs may retain baud rate from
    /// previous firmware like bosminer).
    ///
    /// In hybrid mode (serial present), enumeration goes through the serial UART
    /// instead of the FPGA CMD FIFO.
    ///
    /// Returns (chip_count, chip_id).
    pub fn enumerate_chips(&mut self) -> Result<(u8, u16)> {
        // Hybrid mode: enumerate via serial UART
        if self.serial.is_some() {
            return self.enumerate_chips_serial();
        }

        use dcentrald_hal::fpga_chain::{BAUD_REG_115200, BAUD_REG_1_5M, BAUD_REG_3M};

        // Try default 115200 baud first (ASICs fresh from power-cycle)
        match self.try_enumerate_at_baud(BAUD_REG_115200, "115200") {
            Ok(result) => return Ok(result),
            Err(_) => {
                tracing::info!(
                    chain_id = self.chain_id,
                    "No response at 115200 baud -- trying 1.5 Mbaud (ASICs may retain baud from previous firmware)"
                );
            }
        }

        // Try 1.5625 Mbaud (common bosminer operational baud rate)
        match self.try_enumerate_at_baud(BAUD_REG_1_5M, "1.5625M") {
            Ok(result) => {
                tracing::warn!(
                    chain_id = self.chain_id,
                    "ASICs responded at 1.5 Mbaud -- previous firmware left them at this rate (voltage cycle did not fully power-cycle ASICs)"
                );
                return Ok(result);
            }
            Err(_) => {
                tracing::info!(
                    chain_id = self.chain_id,
                    "No response at 1.5 Mbaud either -- trying 3.125 Mbaud"
                );
            }
        }

        // Try 3.125 Mbaud (max speed some firmwares use)
        match self.try_enumerate_at_baud(BAUD_REG_3M, "3.125M") {
            Ok(result) => {
                tracing::warn!(
                    chain_id = self.chain_id,
                    "ASICs responded at 3.125 Mbaud -- previous firmware left them at max speed"
                );
                Ok(result)
            }
            Err(e) => {
                // Restore default baud rate before returning error
                self.fpga.set_baud(BAUD_REG_115200);
                tracing::warn!(
                    chain_id = self.chain_id,
                    "No chips responded at any baud rate (115200, 1.5M, 3.125M) -- chain is dead"
                );
                Err(e)
            }
        }
    }

    /// Try chip enumeration at a specific FPGA baud rate.
    ///
    /// Sends GetAddress broadcast, waits for responses, extracts ChipID.
    /// Response word 0 format: 0x00908713 -> ChipID = bytes[1:0] = 0x1387
    fn try_enumerate_at_baud(&mut self, baud_reg: u32, baud_label: &str) -> Result<(u8, u16)> {
        use crate::protocol;

        // Set baud rate and reset FIFOs
        self.fpga.set_baud(baud_reg);
        self.fpga.reset_fifos();

        // Send BOTH GetAddress formats: BM1387 (0x54) AND BM1397+ (0x52).
        // We don't know the chip type yet -- after power cycle, ASICs reset to default.
        // BM1387 responds to 0x54, BM1397+ responds to 0x52. Both ignore the other.
        let stat_before = self
            .fpga
            .cmd
            .read_reg(dcentrald_hal::fpga_chain::REG_CMD_STAT);
        tracing::info!(
            chain_id = self.chain_id,
            baud = baud_label,
            "Sending GetAddress (BM1387 + BM1397+) at {} baud -- CMD_STAT: 0x{:02X}",
            baud_label,
            stat_before,
        );
        self.fpga.write_cmd(protocol::FIFO_CMD_GET_ADDRESS); // BM1387: header 0x54
        self.fpga.write_cmd(protocol::FIFO_CMD_GET_ADDRESS_BM139X); // BM1397+: header 0x52

        let stat_after_write = self
            .fpga
            .cmd
            .read_reg(dcentrald_hal::fpga_chain::REG_CMD_STAT);
        tracing::info!(
            chain_id = self.chain_id,
            baud = baud_label,
            cmd_stat = format_args!("0x{:02X}", stat_after_write),
            "CMD_STAT after write: TX_EMPTY={}, RX_EMPTY={}",
            if stat_after_write & 0x04 != 0 {
                "yes"
            } else {
                "NO"
            },
            if stat_after_write & 0x01 != 0 {
                "yes"
            } else {
                "NO"
            },
        );

        // Wait for responses -- 500ms is sufficient at any baud rate for 63 chips
        std::thread::sleep(std::time::Duration::from_millis(500));

        let stat_after_wait = self
            .fpga
            .cmd
            .read_reg(dcentrald_hal::fpga_chain::REG_CMD_STAT);
        tracing::info!(
            chain_id = self.chain_id,
            baud = baud_label,
            cmd_stat = format_args!("0x{:02X}", stat_after_wait),
            "CMD_STAT after 500ms wait: TX_EMPTY={}, RX_EMPTY={}",
            if stat_after_wait & 0x04 != 0 {
                "yes"
            } else {
                "NO"
            },
            if stat_after_wait & 0x01 != 0 {
                "yes"
            } else {
                "NO"
            },
        );

        // Read all responses (each chip sends 2 words)
        let mut count = 0u8;
        let mut first_chip_id: u16 = 0;

        while self.fpga.cmd_rx_has_data() {
            let word0 = self.fpga.read_cmd_response().unwrap_or(0);
            let _word1 = self.fpga.read_cmd_response().unwrap_or(0);

            if count == 0 {
                first_chip_id = (word0 & 0xFFFF) as u16;
                if first_chip_id & 0xFF00 > first_chip_id & 0x00FF {
                    first_chip_id = ((first_chip_id & 0xFF) << 8) | ((first_chip_id >> 8) & 0xFF);
                }
                tracing::debug!(
                    chain_id = self.chain_id,
                    raw_word0 = format_args!("0x{:08X}", word0),
                    chip_id = format_args!("0x{:04X}", first_chip_id),
                    baud = baud_label,
                    "First chip responded at {} baud -- ChipID extracted",
                    baud_label,
                );
            }
            count += 1;
        }

        if count == 0 {
            return Err(crate::AsicError::NoChipsDetected {
                chain_id: self.chain_id,
            });
        }

        // Validate chip_id is a production ASIC type -- reject noise/garbage responses.
        // Electromagnetic interference on the UART lines can produce valid-looking
        // responses with bogus chip IDs. Mining with a wrong driver = zero nonces.
        if !production_chip_id_known(first_chip_id) {
            tracing::error!(
                chain_id = self.chain_id,
                chip_id = format_args!("0x{:04X}", first_chip_id),
                chip_count = count,
                baud = baud_label,
                "REJECTED: ChipID 0x{:04X} is not a known ASIC type -- likely noise on UART. \
                 Known types: BM1387, BM1397, BM1398, BM1362, BM1366, BM1368, BM1370",
                first_chip_id,
            );
            return Err(crate::AsicError::NoChipsDetected {
                chain_id: self.chain_id,
            });
        }

        self.chip_count = count;
        self.chip_id = first_chip_id;

        tracing::info!(
            chain_id = self.chain_id,
            chip_count = count,
            chip_id = format_args!("0x{:04X}", first_chip_id),
            baud = baud_label,
            "Chain {} enumeration: {} chips at {} baud, ChipID 0x{:04X}",
            self.chain_id,
            count,
            baud_label,
            first_chip_id,
        );

        Ok((count, first_chip_id))
    }

    /// Assign addresses to all chips on the chain.
    ///
    /// Matches bosminer's proven BM1387 enumeration sequence:
    /// 1. Send Chain Inactive broadcast 3 times (fire-and-forget, no response expected)
    /// 2. Assign addresses using chip_count from enumerate_chips()
    ///
    /// In hybrid mode (serial present), address assignment goes through the serial
    /// UART instead of the FPGA CMD FIFO.
    ///
    /// IMPORTANT: BM1387 does NOT respond to Chain Inactive -- bosminer sends it
    /// 3 times with 100ms delay and never reads responses. The chip count comes
    /// from the prior GetAddress enumeration, not from Chain Inactive responses.
    /// See braiins_lib.rs lines 592-607.
    pub fn assign_addresses(&mut self) -> Result<()> {
        // Hybrid mode: assign addresses via serial UART
        if self.serial.is_some() {
            return self.assign_addresses_serial();
        }

        use crate::protocol;

        if self.chip_count == 0 {
            return Err(crate::AsicError::NoChipsDetected {
                chain_id: self.chain_id,
            });
        }

        // Step 1: Send Chain Inactive broadcast 3 times (matching bosminer)
        // This puts all chips into "inactive but listening" mode. Each chip
        // will accept the next SetChipAddress command, get addressed, then
        // pass further commands down the chain.
        // BM1387 does NOT send responses to Chain Inactive (fire-and-forget).
        tracing::debug!(
            chain_id = self.chain_id,
            "Sending Chain Inactive broadcast x3 (BM1387: no response expected)"
        );
        for i in 0..3 {
            self.fpga.reset_fifos();
            self.fpga.write_cmd(protocol::FIFO_CMD_CHAIN_INACTIVE);
            std::thread::sleep(std::time::Duration::from_millis(100));
            tracing::debug!(
                chain_id = self.chain_id,
                iteration = i + 1,
                "Chain Inactive sent ({}/3)",
                i + 1,
            );
        }

        // Drain any unexpected RX data (shouldn't be any for BM1387)
        while self.fpga.cmd_rx_has_data() {
            let _ = self.fpga.read_cmd_response();
        }

        // Step 2: Assign addresses with spacing of 4 (matching bosminer)
        // Bosminer uses: ChipAddress::One(i) -> hw_addr = i * 4
        // This gives each chip a unique address for individual register access.
        let addr_spacing: u16 = 4; // BM1387 standard: 256 / 63 ??? 4
        self.fpga.reset_fifos();

        for i in 0..self.chip_count {
            let addr = (i as u16 * addr_spacing) as u8;
            self.fpga.write_cmd(protocol::fifo_cmd_set_address(addr));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        tracing::info!(
            chain_id = self.chain_id,
            chips = self.chip_count,
            addr_spacing = addr_spacing,
            first_addr = format_args!("0x{:02X}", 0),
            last_addr = format_args!(
                "0x{:02X}",
                ((self.chip_count - 1) as u16 * addr_spacing) as u8
            ),
            "Chain {} addresses assigned: {} chips, spacing {} (0x00 to 0x{:02X})",
            self.chain_id,
            self.chip_count,
            addr_spacing,
            ((self.chip_count - 1) as u16 * addr_spacing) as u8,
        );

        Ok(())
    }

    /// Initialize the chain with the given chip driver.
    ///
    /// Delegates to the chip-specific initialization sequence.
    pub fn init_with_driver(&mut self, driver: &dyn ChipDriver, freq_mhz: u16) -> Result<()> {
        driver.init_chain(&mut self.fpga, self.chip_count, freq_mhz)?;
        Ok(())
    }

    /// Get the CRC error count for this chain.
    pub fn crc_errors(&self) -> u32 {
        self.fpga.read_error_count()
    }

    /// Clear the CRC error counter.
    pub fn clear_errors(&self) {
        self.fpga.clear_error_count();
    }

    // -----------------------------------------------------------------------
    // Hybrid mode: serial command routing
    // -----------------------------------------------------------------------

    /// Write a command word -- routes to serial (hybrid) or FPGA CMD FIFO.
    ///
    /// In hybrid mode, the 32-bit FIFO word is unpacked and sent as a framed
    /// serial command (with preamble and CRC). In FPGA-only mode, it goes
    /// directly to the CMD FIFO.
    pub fn write_cmd(&self, word: u32) {
        if let Some(ref serial) = self.serial {
            if let Err(e) = serial.send_cmd_word(word) {
                tracing::warn!(
                    chain_id = self.chain_id,
                    error = %e,
                    word = format_args!("0x{:08X}", word),
                    "Serial write_cmd failed"
                );
            }
        } else {
            self.fpga.write_cmd(word);
        }
    }

    /// Write two command words (register write) -- routes to serial or FPGA.
    pub fn write_cmd_words(&self, word0: u32, word1: u32) {
        if let Some(ref serial) = self.serial {
            if let Err(e) = serial.send_cmd_words(word0, word1) {
                tracing::warn!(
                    chain_id = self.chain_id,
                    error = %e,
                    "Serial write_cmd_words failed"
                );
            }
        } else {
            self.fpga.write_cmd(word0);
            self.fpga.write_cmd(word1);
        }
    }

    /// Read a command response -- routes to serial or FPGA CMD RX FIFO.
    ///
    /// Returns a single 32-bit word from the next response, or None if no
    /// response is available within the timeout.
    pub fn read_cmd_response(&self) -> Option<u32> {
        if let Some(ref serial) = self.serial {
            match serial.read_cmd_response() {
                Ok(Some(v)) if v.len() >= 4 => Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]])),
                Ok(Some(v)) if v.len() >= 2 => {
                    // Short response -- zero-extend
                    let mut bytes = [0u8; 4];
                    bytes[..v.len()].copy_from_slice(&v);
                    Some(u32::from_le_bytes(bytes))
                }
                _ => None,
            }
        } else {
            self.fpga.read_cmd_response()
        }
    }

    // -----------------------------------------------------------------------
    // Serial enumeration path (hybrid mode)
    // -----------------------------------------------------------------------

    /// Enumerate chips via serial UART (hybrid mode).
    ///
    /// Tries GetAddress at multiple baud rates, same fallback logic as
    /// the FPGA path but using the serial backend.
    fn enumerate_chips_serial(&mut self) -> Result<(u8, u16)> {
        // Guarded by the `self.serial.is_some()` check at the sole caller, but
        // return a clean chain error rather than panic if a future caller ever
        // reaches here without a serial backend. The workspace is
        // `panic = "abort"`, so a raw unwrap here would crash the whole daemon.
        // Mirrors the Mujina #52/#74 fix: hardware-path expect()/unwrap() that
        // "panic on hardware failure" -> proper error handling.
        let serial = self
            .serial
            .as_ref()
            .ok_or_else(|| crate::AsicError::InitFailed {
                chain_id: self.chain_id,
                detail: "serial backend not initialized for serial enumeration".to_string(),
            })?;

        for (baud, label) in [
            (115_200u32, "115200"),
            (1_562_500, "1.5625M"),
            (3_125_000, "3.125M"),
        ] {
            if let Err(e) = serial.set_baud(baud) {
                tracing::warn!(
                    chain_id = self.chain_id,
                    baud = label,
                    error = %e,
                    "Serial set_baud failed"
                );
                continue;
            }
            if let Err(e) = serial.flush_io() {
                tracing::warn!(chain_id = self.chain_id, error = %e, "Serial flush_io failed");
            }

            tracing::info!(
                chain_id = self.chain_id,
                baud = label,
                "Serial: sending GetAddress (BM1387 + BM1397+) at {} baud",
                label,
            );

            // Send BOTH GetAddress formats ? we don't know chip type yet.
            // BM1387 responds to 0x54, BM1397+ responds to 0x52. Each ignores the other.
            if let Err(e) = serial.send_get_address() {
                tracing::warn!(chain_id = self.chain_id, error = %e, "Serial GetAddress (BM1387) send failed");
            }
            if let Err(e) = serial.send_get_address_bm1397plus() {
                tracing::warn!(chain_id = self.chain_id, error = %e, "Serial GetAddress (BM1397+) send failed");
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));

            let responses = match serial.read_all_responses(500) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(chain_id = self.chain_id, error = %e, "Serial read_all_responses failed");
                    continue;
                }
            };

            if !responses.is_empty() {
                let mut count = 0u8;
                let mut first_chip_id: u16 = 0;
                for resp in &responses {
                    if resp.len() >= 2 {
                        if count == 0 {
                            // Extract ChipID from response body (same format as FPGA path).
                            // Response body bytes are after preamble stripping.
                            first_chip_id = ((resp[1] as u16) << 8) | (resp[0] as u16);
                            if first_chip_id & 0xFF00 > first_chip_id & 0x00FF {
                                first_chip_id =
                                    ((first_chip_id & 0xFF) << 8) | ((first_chip_id >> 8) & 0xFF);
                            }
                        }
                        count += 1;
                    }
                }

                if count > 0 {
                    // Validate chip_id against production ASIC types.
                    if !production_chip_id_known(first_chip_id) {
                        tracing::warn!(
                            chain_id = self.chain_id,
                            chip_id = format_args!("0x{:04X}", first_chip_id),
                            chip_count = count,
                            baud = label,
                            "Serial: ChipID 0x{:04X} is not a known ASIC type -- trying next baud",
                            first_chip_id,
                        );
                        continue;
                    }

                    tracing::info!(
                        chain_id = self.chain_id,
                        chip_count = count,
                        chip_id = format_args!("0x{:04X}", first_chip_id),
                        baud = label,
                        "Serial enumeration: {} chips at {} baud, ChipID 0x{:04X}",
                        count,
                        label,
                        first_chip_id,
                    );
                    self.chip_count = count;
                    self.chip_id = first_chip_id;
                    return Ok((count, first_chip_id));
                }
            }
            tracing::warn!(
                chain_id = self.chain_id,
                baud = label,
                "No chips at {} baud via serial",
                label,
            );
        }

        Err(crate::AsicError::NoChipsDetected {
            chain_id: self.chain_id,
        })
    }

    /// Assign addresses via serial UART (hybrid mode).
    fn assign_addresses_serial(&mut self) -> Result<()> {
        if self.chip_count == 0 {
            return Err(crate::AsicError::NoChipsDetected {
                chain_id: self.chain_id,
            });
        }

        // Guarded by `self.serial.is_some()` at the caller; fail closed with a
        // clean error instead of a panic=abort crash if that ever changes
        // (Mujina #52/#74 hardware-path unwrap-safety parity).
        let serial = self
            .serial
            .as_ref()
            .ok_or_else(|| crate::AsicError::InitFailed {
                chain_id: self.chain_id,
                detail: "serial backend not initialized for chain init".to_string(),
            })?;

        // Step 1: Triple Chain Inactive (same pattern as FPGA path)
        tracing::debug!(
            chain_id = self.chain_id,
            "Serial: sending Chain Inactive broadcast x3"
        );
        for i in 0..3 {
            // Send BOTH ChainInactive formats ? BM1387 uses 0x55, BM1397+ uses 0x53.
            if let Err(e) = serial.send_chain_inactive() {
                tracing::warn!(
                    chain_id = self.chain_id,
                    error = %e,
                    iteration = i + 1,
                    "Serial Chain Inactive (BM1387) send failed"
                );
            }
            if let Err(e) = serial.send_chain_inactive_bm1397plus() {
                tracing::warn!(
                    chain_id = self.chain_id,
                    error = %e,
                    iteration = i + 1,
                    "Serial Chain Inactive (BM1397+) send failed"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Flush any stale responses
        let _ = serial.flush_io();

        // Step 2: Assign addresses with spacing based on chip count
        let addr_spacing: u16 = if self.chip_count > 0 {
            256 / self.chip_count as u16
        } else {
            4
        };

        for i in 0..self.chip_count {
            let addr = (i as u16 * addr_spacing) as u8;
            // Send BOTH SetAddress formats ? BM1387 uses 0x41, BM1397+ uses 0x40.
            if let Err(e) = serial.send_set_address(addr) {
                tracing::warn!(
                    chain_id = self.chain_id,
                    chip = i,
                    addr = format_args!("0x{:02X}", addr),
                    error = %e,
                    "Serial SetAddress (BM1387) failed"
                );
            }
            if let Err(e) = serial.send_set_address_bm1397plus(addr) {
                tracing::warn!(
                    chain_id = self.chain_id,
                    chip = i,
                    addr = format_args!("0x{:02X}", addr),
                    error = %e,
                    "Serial SetAddress (BM1397+) failed"
                );
            }
            // Pace to avoid overwhelming the UART
            if i % 16 == 15 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        tracing::info!(
            chain_id = self.chain_id,
            chips = self.chip_count,
            addr_spacing = addr_spacing,
            "Serial addresses assigned: {} chips, spacing {}",
            self.chip_count,
            addr_spacing,
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        chain_meets_min_fraction, driver_for_chain, driver_for_chain_with_policy,
        production_chip_id_known, ChainDriverDecision, DivergentChipPolicy,
    };
    use crate::drivers::{bm1362, bm1366, bm1368, bm1370, bm1373, bm1387, bm1397, bm1398};

    #[test]
    fn chain_enumeration_chip_allowlist_uses_production_registry() {
        for chip_id in [
            bm1387::CHIP_ID,
            bm1397::CHIP_ID,
            bm1398::CHIP_ID,
            bm1362::CHIP_ID,
            bm1366::CHIP_ID,
            bm1368::CHIP_ID,
            bm1370::CHIP_ID,
        ] {
            assert!(
                production_chip_id_known(chip_id),
                "production chip 0x{chip_id:04X} must be accepted"
            );
        }

        assert!(
            !production_chip_id_known(bm1373::CHIP_ID),
            "scaffold chip IDs must stay out of production enumeration"
        );
        assert!(
            !production_chip_id_known(0xFFFF),
            "noise chip IDs must stay fail-closed"
        );
    }

    #[test]
    fn chain_min_fraction_preserves_zero_floor_partial_enum() {
        assert!(
            chain_meets_min_fraction(28, 126, 0.0),
            "floor 0.0 must preserve the proven .25 28/126 partial-enum path"
        );
        assert!(
            !chain_meets_min_fraction(28, 126, 0.5),
            "28/126 must fail a 50% operator floor"
        );
        assert!(
            chain_meets_min_fraction(126, 126, 1.0),
            "full population must pass a 100% operator floor"
        );
        assert!(
            !chain_meets_min_fraction(126, 126, f32::NAN),
            "non-finite floors fail closed"
        );
        assert!(
            !chain_meets_min_fraction(126, 126, 1.1),
            "out-of-range floors fail closed"
        );
    }

    #[test]
    fn driver_for_chain_skips_divergent_production_chip_ids() {
        assert_eq!(
            driver_for_chain(bm1398::CHIP_ID, bm1362::CHIP_ID),
            ChainDriverDecision::SkipDivergent
        );
        assert_eq!(
            driver_for_chain(bm1362::CHIP_ID, bm1398::CHIP_ID),
            ChainDriverDecision::SkipDivergent
        );
        assert_eq!(
            driver_for_chain(bm1362::CHIP_ID, bm1362::CHIP_ID),
            ChainDriverDecision::Drive
        );
        assert_eq!(
            driver_for_chain(0, bm1362::CHIP_ID),
            ChainDriverDecision::Drive,
            "no latched chip ID must preserve existing discovery behavior"
        );
        assert_eq!(
            driver_for_chain(bm1362::CHIP_ID, 0),
            ChainDriverDecision::Drive,
            "zero chain chip ID must preserve model-hint/passthrough behavior"
        );
    }

    #[test]
    fn driver_for_chain_can_be_log_only_for_xil25_policy() {
        assert_eq!(
            driver_for_chain_with_policy(
                bm1398::CHIP_ID,
                bm1362::CHIP_ID,
                DivergentChipPolicy::LogOnly
            ),
            ChainDriverDecision::LogOnlyDivergent
        );
    }
}
