// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — BM1366 ASIC driver
// Faithful port from ESP-Miner bm1366.c
//
// BM1366: Used in BitAxe Ultra / Hex Ultra
// Chip ID: 0x1366
// Chip ID response length: 11 bytes
// Job packet: 82-byte payload (job_id + num_midstates + starting_nonce[4] + nbits[4] + ntime[4] + merkle_root[32] + prev_block_hash[32] + version[4])
// Job ID increment: +8, mod 128
// PLL fb_divider range: 144..=235

use crate::common::*;
use crate::crc::{crc16_false, crc5};
use crate::pll::{self, FREQ_MULT};
use crate::serial::SerialPort;
use crate::AsicDriver;

const CHIP_ID: u16 = 0x1366;
const CHIP_ID_RESPONSE_LENGTH: usize = 11;

/// Register map: register address -> RegisterType
/// Matches the C static const REGISTER_MAP[]
fn register_type_for(addr: u8) -> RegisterType {
    match addr {
        0x4C => RegisterType::ErrorCount,
        0x88 => RegisterType::Domain0Count,
        0x89 => RegisterType::Domain1Count,
        0x8A => RegisterType::Domain2Count,
        0x8B => RegisterType::Domain3Count,
        0x8C => RegisterType::TotalCount,
        _ => RegisterType::Invalid,
    }
}

/// BM1366 driver
pub struct BM1366 {
    serial: SerialPort,
    chip_count: u8,
    current_frequency: f32,
    address_interval: u8,
    /// Bounded per-stream recent-nonce filter (MD-4). Upstream ESP-Miner
    /// filters only the immediately-previous nonce, so a non-consecutive
    /// looped duplicate would be re-validated/re-counted/re-submitted. The ring
    /// (last `RECENT_NONCE_RING_LEN` distinct values) catches both consecutive
    /// and in-window looped duplicates; shared design with all four drivers.
    recent_nonces: RecentNonceRing,
    /// Last version mask sent to the chain via `set_version_mask` (ASIC-3).
    /// Used only for the observe-only malformed-version-bits metric below; the
    /// rolling math is unchanged. `0` means "not yet known" (check is skipped).
    active_version_mask: u32,
    /// Observe-only count of responses whose shifted version field had bits set
    /// outside the active mask (ASIC-3). Telemetry only — the nonce is NOT
    /// dropped and behavior is byte-identical to upstream.
    malformed_version_bits: u32,
    /// Preamble-scan carry buffer (ASIC-4). Holds bytes that did not yet form a
    /// complete 11-byte frame so a misaligned/partial UART read resyncs on the
    /// `[0xAA, 0x55]` preamble instead of flushing the whole window. Mirrors the
    /// bm1397 reassembler; `clear_buffer()` stays the fallback resync.
    rx_carry: Vec<u8>,
}

impl BM1366 {
    pub fn new(serial: SerialPort) -> Self {
        Self {
            serial,
            chip_count: 0,
            current_frequency: 50.0, // initial frequency for ramp-up
            address_interval: 0,
            recent_nonces: RecentNonceRing::new(),
            active_version_mask: 0,
            malformed_version_bits: 0,
            rx_carry: Vec::with_capacity(128),
        }
    }

    /// Observe-only count of nonce responses whose `(version_raw << 13)` carried
    /// bits outside the active version mask (ASIC-3). Telemetry only.
    pub fn malformed_version_bits(&self) -> u32 {
        self.malformed_version_bits
    }

    /// Append received bytes to the preamble-scan carry buffer (ASIC-4),
    /// bounding it so a stuck stream cannot grow it without limit. Mirrors
    /// bm1397's `append_rx_carry`.
    fn append_rx_carry(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.rx_carry.len() == 128 {
                self.rx_carry.remove(0);
            }
            self.rx_carry.push(byte);
        }
    }

    /// Drain complete 11-byte frames from the carry buffer, resyncing on the
    /// `[0xAA, 0x55]` preamble (ASIC-4). Mirrors bm1397's `parse_rx_carry` but
    /// for the 11-byte BM1366/68/70 response. Keeps the trailing partial bytes.
    fn parse_rx_carry(&mut self) -> Vec<AsicResult> {
        let mut results = Vec::new();

        loop {
            let Some(preamble_pos) = self
                .rx_carry
                .windows(2)
                .position(|window| window == [0xAA, 0x55])
            else {
                // No preamble in view: keep only the last byte (it may be the
                // 0xAA of a preamble split across reads) and drop the rest.
                if self.rx_carry.len() > 1 {
                    let keep = self.rx_carry.pop();
                    self.rx_carry.clear();
                    if let Some(byte) = keep {
                        self.rx_carry.push(byte);
                    }
                }
                break;
            };

            if preamble_pos > 0 {
                self.rx_carry.drain(..preamble_pos);
            }
            if self.rx_carry.len() < 11 {
                break;
            }

            let mut frame = [0u8; 11];
            frame.copy_from_slice(&self.rx_carry[..11]);
            match self.process_work(&frame) {
                Ok(mut parsed) => {
                    results.append(&mut parsed);
                    self.rx_carry.drain(..11);
                }
                Err(e) => {
                    log::debug!("BM1366: dropping invalid response byte: {}", e);
                    self.rx_carry.drain(..1);
                }
            }
        }

        results
    }

    // ── Low-level send ──────────────────────────────────────────────────

    /// Build and send a packet to the BM1366 chain.
    /// Exact port of _send_BM1366() from bm1366.c.
    fn send_packet(&mut self, header: u8, data: &[u8]) -> Result<(), AsicError> {
        let is_job = (header & TYPE_JOB) != 0;
        let total_length = if is_job {
            data.len() + 6
        } else {
            data.len() + 5
        };

        let mut buf = [0u8; 96]; // max packet size — stack-allocated

        // Preamble
        buf[0] = 0x55;
        buf[1] = 0xAA;

        // Header
        buf[2] = header;

        // Length field
        buf[3] = if is_job {
            (data.len() + 4) as u8
        } else {
            (data.len() + 3) as u8
        };

        // Data
        buf[4..4 + data.len()].copy_from_slice(data);

        // CRC
        if is_job {
            let crc = crc16_false(&buf[2..4 + data.len()]);
            buf[4 + data.len()] = (crc >> 8) as u8;
            buf[5 + data.len()] = (crc & 0xFF) as u8;
        } else {
            buf[4 + data.len()] = crc5(&buf[2..4 + data.len()]);
        }

        self.serial.write(&buf[..total_length])?;
        Ok(())
    }

    /// Send raw bytes directly (port of _send_simple)
    fn send_simple(&mut self, data: &[u8]) -> Result<(), AsicError> {
        self.serial.write(data)?;
        Ok(())
    }

    /// Send chain inactive command
    fn send_chain_inactive(&mut self) -> Result<(), AsicError> {
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_INACTIVE, &[0x00, 0x00])
    }

    /// Set chip address
    fn set_chip_address(&mut self, addr: u8) -> Result<(), AsicError> {
        log::info!("Set chip address: 0x{:02x}", addr);
        self.send_packet(TYPE_CMD | GROUP_SINGLE | CMD_SETADDRESS, &[addr, 0x00])
    }

    // ── Frequency ───────────────────────────────────────────────────────

    /// Set hash frequency via PLL register.
    /// Exact port of BM1366_send_hash_frequency() from bm1366.c.
    fn send_hash_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        let params = pll::find_best_pll(target_freq, 144, 235);

        // Reject a target below the chip's representable PLL minimum. When no
        // (refdiv,postdiv1,postdiv2) yields an in-range fb_divider, find_best_pll
        // leaves fb_divider=0/refdiv=0 (same all-zero sentinel as ESP-Miner
        // pll.c). Sending that is a garbage PLL write, and `params.postdiv1 - 1`
        // below would underflow-panic under debug overflow checks. Guard at the
        // Rust boundary instead of changing the search math.
        if params.fb_divider == 0 {
            return Err(AsicError::InitFailed(format!(
                "target {target_freq} MHz below BM1366 PLL minimum (no in-range fb_divider)"
            )));
        }

        let vdo_scale: u8 = if params.fb_divider as f32 * FREQ_MULT / params.refdiv as f32 >= 2400.0
        {
            0x50
        } else {
            0x40
        };
        let postdiv = (((params.postdiv1 - 1) & 0x0f) << 4) | ((params.postdiv2 - 1) & 0x0f);

        let freqbuf: [u8; 6] = [
            0x00,
            0x08,
            vdo_scale,
            params.fb_divider,
            params.refdiv,
            postdiv,
        ];

        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &freqbuf)?;

        log::info!(
            "Setting Frequency to {} MHz ({})",
            target_freq,
            params.actual_freq
        );
        // Track the actual PLL-quantised frequency, not the requested target.
        // Pass-5 audit: upstream ESP-Miner uses actual_frequency for dashboard
        // and J/TH calc — using target_freq drifts when target doesn't land
        // on a valid PLL solution (e.g. 527 MHz → nearest is 525).
        self.current_frequency = params.actual_freq;
        Ok(())
    }

    // ── Count chips ─────────────────────────────────────────────────────

    /// Count ASIC chips on the chain by reading CHIP_ID responses.
    /// Port of count_asic_chips() from asic_common.c specialized for BM1366.
    fn count_chips(&mut self, expected_count: u8) -> Result<u8, AsicError> {
        let mut buffer = [0u8; 11];
        let mut chip_counter: u8 = 0;

        loop {
            let received = self.serial.read(&mut buffer, 1000)?;
            if received == 0 {
                break;
            }

            if received != CHIP_ID_RESPONSE_LENGTH {
                log::error!(
                    "Invalid CHIP_ID response length: expected {}, got {}",
                    CHIP_ID_RESPONSE_LENGTH,
                    received
                );
                break;
            }

            // Check preamble (big-endian: 0xAA55)
            let preamble = ((buffer[0] as u16) << 8) | buffer[1] as u16;
            if preamble != PREAMBLE_BE {
                log::warn!(
                    "Preamble mismatch: expected 0x{:04x}, got 0x{:04x}",
                    PREAMBLE_BE,
                    preamble
                );
                continue;
            }

            // Check chip ID
            let received_chip_id = ((buffer[2] as u16) << 8) | buffer[3] as u16;
            if received_chip_id != CHIP_ID {
                log::warn!(
                    "CHIP_ID mismatch: expected 0x{:04x}, got 0x{:04x}",
                    CHIP_ID,
                    received_chip_id
                );
                continue;
            }

            // CRC5 check (bytes 2..end should CRC to 0)
            if crc5(&buffer[2..received]) != 0 {
                log::warn!("Checksum failed on CHIP_ID response");
                continue;
            }

            log::info!(
                "Chip {} detected: CORE_NUM: 0x{:02x} ADDR: 0x{:02x}",
                chip_counter,
                buffer[4],
                buffer[5]
            );

            // Saturating so a looping / echoing chain or a noise stream that
            // delivers >= 256 CRC5-valid CHIP_ID frames cannot overflow this u8
            // (debug panic) or wrap to 0 / a small value (release: false
            // NoAsicsFound or a mis-sized address_interval). 256+ chips is
            // physically impossible; the expected-count mismatch below still warns.
            chip_counter = chip_counter.saturating_add(1);
        }

        if chip_counter != expected_count {
            log::warn!(
                "{} chip(s) detected on the chain, expected {}",
                chip_counter,
                expected_count
            );
        }

        Ok(chip_counter)
    }

    // ASIC-7 cleanup: the standalone `receive_work_response` helper (a port of
    // receive_work() from asic_common.c) was genuinely dead — no caller anywhere,
    // not even under cfg, and no parity counterpart in the other drivers, which
    // read responses via the `read_responses`/`process_work` trait methods that
    // call `self.serial.read()` directly. Removed to keep the clippy `-D warnings`
    // CI gate green without an allow; the real receive path is unchanged.
}

// ── AsicDriver trait implementation ─────────────────────────────────────────

impl super::AsicDriver for BM1366 {
    /// Initialize the BM1366 chain.
    /// Exact port of BM1366_init() from bm1366.c.
    /// Returns the number of chips detected.
    fn init(
        &mut self,
        frequency: f32,
        asic_count: u8,
        initial_difficulty: f64,
    ) -> Result<u8, AsicError> {
        // Set version mask 3 times (matches C code)
        for _ in 0..3 {
            self.set_version_mask(STRATUM_DEFAULT_VERSION_MASK)?;
        }

        // Read register 00 on all chips (init3)
        // {0x55, 0xAA, 0x52, 0x05, 0x00, 0x00, 0x0A}
        self.send_simple(&[0x55, 0xAA, 0x52, 0x05, 0x00, 0x00, 0x0A])?;

        let chip_counter = self.count_chips(asic_count)?;
        if chip_counter == 0 {
            return Err(AsicError::NoAsicsFound);
        }

        // init4: Reg_A8 write
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x00, 0x00, 0x03}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x00, 0x00, 0x03,
        ])?;

        // init5: Misc Control
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0xFF, 0x0F, 0xC1, 0x00, 0x00}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0xFF, 0x0F, 0xC1, 0x00, 0x00,
        ])?;

        // Chain inactive
        self.send_chain_inactive()?;

        // Split chip address space evenly and assign addresses
        self.address_interval = (256 / chip_counter as u16) as u8;
        for i in 0..chip_counter {
            self.set_chip_address(i.wrapping_mul(self.address_interval))?;
        }

        // init135: Core Register Control
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x85, 0x40, 0x0C}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x85, 0x40, 0x0C,
        ])?;

        // init136: Core Register Control
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x80, 0x20, 0x19}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x80, 0x20, 0x19,
        ])?;

        // Set difficulty mask (from caller — last-known pool diff or safe default).
        log::info!(
            "BM1366: init difficulty={:.3} (overridden on first mining.set_difficulty)",
            initial_difficulty
        );
        let difficulty_mask = get_difficulty_mask(initial_difficulty);
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &difficulty_mask)?;

        // init138: Analog Mux Control
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x03, 0x1D}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x03, 0x1D,
        ])?;

        // init139: IO Driver Strength
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0x58, 0x02, 0x11, 0x11, 0x11, 0x06}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x58, 0x02, 0x11, 0x11, 0x11, 0x06,
        ])?;

        // init171: per-chip write to register 0x2C
        // {0x55, 0xAA, 0x41, 0x09, 0x00, 0x2C, 0x00, 0x7C, 0x00, 0x03, 0x03}
        self.send_simple(&[
            0x55, 0xAA, 0x41, 0x09, 0x00, 0x2C, 0x00, 0x7C, 0x00, 0x03, 0x03,
        ])?;

        // Per-chip register initialization
        for i in 0..chip_counter {
            let addr = i.wrapping_mul(self.address_interval);

            // set_a8_register: {addr, 0xA8, 0x00, 0x07, 0x01, 0xF0}
            self.send_packet(
                TYPE_CMD | GROUP_SINGLE | CMD_WRITE,
                &[addr, 0xA8, 0x00, 0x07, 0x01, 0xF0],
            )?;

            // set_18_register: {addr, 0x18, 0xF0, 0x00, 0xC1, 0x00}
            self.send_packet(
                TYPE_CMD | GROUP_SINGLE | CMD_WRITE,
                &[addr, 0x18, 0xF0, 0x00, 0xC1, 0x00],
            )?;

            // set_3c_register_first: {addr, 0x3C, 0x80, 0x00, 0x85, 0x40}
            self.send_packet(
                TYPE_CMD | GROUP_SINGLE | CMD_WRITE,
                &[addr, 0x3C, 0x80, 0x00, 0x85, 0x40],
            )?;

            // set_3c_register_second: {addr, 0x3C, 0x80, 0x00, 0x80, 0x20}
            self.send_packet(
                TYPE_CMD | GROUP_SINGLE | CMD_WRITE,
                &[addr, 0x3C, 0x80, 0x00, 0x80, 0x20],
            )?;

            // set_3c_register_third: {addr, 0x3C, 0x80, 0x00, 0x82, 0xAA}
            self.send_packet(
                TYPE_CMD | GROUP_SINGLE | CMD_WRITE,
                &[addr, 0x3C, 0x80, 0x00, 0x82, 0xAA],
            )?;
        }

        // Frequency ramp-up
        pll::do_frequency_transition(&mut self.current_frequency, frequency, |freq| {
            // We need to send the hash frequency command; build it inline
            let params = pll::find_best_pll(freq, 144, 235);
            if params.fb_divider == 0 {
                log::warn!("PLL: no in-range solution for {freq} MHz step; skipping write");
                return;
            }
            let vdo_scale: u8 =
                if params.fb_divider as f32 * FREQ_MULT / params.refdiv as f32 >= 2400.0 {
                    0x50
                } else {
                    0x40
                };
            let postdiv = (((params.postdiv1 - 1) & 0x0f) << 4) | ((params.postdiv2 - 1) & 0x0f);
            let freqbuf: [u8; 6] = [
                0x00,
                0x08,
                vdo_scale,
                params.fb_divider,
                params.refdiv,
                postdiv,
            ];

            // Build the raw packet for frequency setting — stack-allocated
            let data = &freqbuf;
            let header = TYPE_CMD | GROUP_ALL | CMD_WRITE;
            let total_len = data.len() + 5;
            let mut buf = [0u8; 16]; // 6-byte data + 5 overhead = 11
            buf[0] = 0x55;
            buf[1] = 0xAA;
            buf[2] = header;
            buf[3] = (data.len() + 3) as u8;
            buf[4..4 + data.len()].copy_from_slice(data);
            buf[4 + data.len()] = crc5(&buf[2..4 + data.len()]);
            // Note: ignoring write error in ramp-up closure (matches C behavior)
            let _ = self.serial.write(&buf[..total_len]);
        });

        // set_10_hash_counting: S19XP-Stock Default
        // {0x00, 0x10, 0x00, 0x00, 0x15, 0x1C}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, 0x10, 0x00, 0x00, 0x15, 0x1C],
        )?;

        // init795: final version mask
        // {0x55, 0xAA, 0x51, 0x09, 0x00, 0xA4, 0x90, 0x00, 0xFF, 0xFF, 0x1C}
        self.send_simple(&[
            0x55, 0xAA, 0x51, 0x09, 0x00, 0xA4, 0x90, 0x00, 0xFF, 0xFF, 0x1C,
        ])?;

        self.chip_count = chip_counter;
        Ok(chip_counter)
    }

    /// Send a mining job to the BM1366 chain.
    /// Port of BM1366_send_work() from bm1366.c.
    fn send_work(&mut self, job: &MiningJob) -> Result<(), AsicError> {
        // Use the dispatcher's job_id — must match what the dispatcher stores for nonce mapping
        let job_id = job.job_id;

        // Build the 82-byte job payload (BM1366_job struct) — stack-allocated
        let mut payload = [0u8; 82];
        let mut pos = 0;

        payload[pos] = job_id;
        pos += 1;
        payload[pos] = 0x01;
        pos += 1; // num_midstates = 1

        payload[pos..pos + 4].copy_from_slice(&job.starting_nonce.to_le_bytes());
        pos += 4;
        payload[pos..pos + 4].copy_from_slice(&job.nbits.to_le_bytes());
        pos += 4;
        payload[pos..pos + 4].copy_from_slice(&job.ntime.to_le_bytes());
        pos += 4;
        payload[pos..pos + 32].copy_from_slice(&job.merkle_root);
        pos += 32;
        payload[pos..pos + 32].copy_from_slice(&job.prev_block_hash);
        pos += 32;
        payload[pos..pos + 4].copy_from_slice(&job.version.to_le_bytes());
        pos += 4;

        self.send_packet(TYPE_JOB | GROUP_SINGLE | CMD_WRITE, &payload[..pos])?;

        log::debug!("Send Job: {:02X}", job_id);

        Ok(())
    }

    /// Process a work response from the BM1366.
    /// Port of BM1366_process_work() from bm1366.c.
    fn process_work(&mut self, rx_buf: &[u8]) -> Result<Vec<AsicResult>, AsicError> {
        // Expected response: 11 bytes (bm1366_asic_result_t)
        if rx_buf.len() < 11 {
            return Err(AsicError::InvalidResponse(format!(
                "BM1366 response too short: {} bytes",
                rx_buf.len()
            )));
        }

        // Validate preamble
        let preamble = ((rx_buf[0] as u16) << 8) | rx_buf[1] as u16;
        if preamble != PREAMBLE_BE {
            return Err(AsicError::PreambleMismatch);
        }

        // CRC5 check
        if crc5(&rx_buf[2..11]) != 0 {
            return Err(AsicError::CrcError);
        }

        // Check if this is a job response or register response
        // Bit 7 of byte 10 is the is_job_response flag
        let is_job_response = (rx_buf[10] & 0x80) != 0;

        if !is_job_response {
            // Register response
            // bytes 2-5: value (big-endian)
            let value = u32::from_be_bytes([rx_buf[2], rx_buf[3], rx_buf[4], rx_buf[5]]);
            let asic_address = rx_buf[6];
            let register_address = rx_buf[7];

            let reg_type = register_type_for(register_address);
            if reg_type == RegisterType::Invalid {
                log::warn!("Unknown register read: {:02x}", register_address);
                return Ok(Vec::new());
            }

            let asic_nr = if self.address_interval > 0 {
                asic_address / self.address_interval
            } else {
                0
            };

            return Ok(vec![AsicResult::Register {
                register_type: reg_type,
                asic_nr,
                value,
                timestamp_us: crate::common::now_us(),
            }]);
        }

        // Job response
        // bytes 2-5: nonce (stored as LE to match ESP-Miner memcpy on LE host)
        let nonce = u32::from_le_bytes([rx_buf[2], rx_buf[3], rx_buf[4], rx_buf[5]]);
        // Driver-level dedup (MD-4): bounded recent-nonce ring catches looped
        // duplicates the single `prev_nonce` scalar missed, without permanently
        // blacklisting a value. Per-stream tier; dispatcher does cross-stream.
        if self.recent_nonces.is_duplicate(nonce) {
            return Ok(vec![]);
        }
        let _midstate_num = rx_buf[6];
        let id = rx_buf[7];
        let version_raw = u16::from_be_bytes([rx_buf[8], rx_buf[9]]);

        let job_id = id & 0xf8;
        // BE interpretation for bit-field extraction (matches ntohl in C)
        let nonce_h = u32::from_be_bytes([rx_buf[2], rx_buf[3], rx_buf[4], rx_buf[5]]);
        let asic_nr = if self.address_interval > 0 {
            ((nonce_h >> 17) & 0xff) as u8 / self.address_interval
        } else {
            0
        };
        let core_id = ((nonce_h >> 25) & 0x7f) as u8;
        let small_core_id = id & 0x07;
        let version_bits = (version_raw as u32) << 13;

        // Observe-only plausibility metric (ASIC-3): when the active version
        // mask is known, count responses whose shifted version field has bits
        // set OUTSIDE the mask. The dispatcher clips these to the mask anyway,
        // so this is pure telemetry — the nonce is NOT dropped and the rolling
        // math is unchanged (faithful to ESP-Miner which shifts blindly).
        if self.active_version_mask != 0 && (version_bits & !self.active_version_mask) != 0 {
            self.malformed_version_bits = self.malformed_version_bits.saturating_add(1);
            log::debug!(
                "BM1366: version bits 0x{:08X} outside mask 0x{:08X} (count={})",
                version_bits,
                self.active_version_mask,
                self.malformed_version_bits
            );
        }

        log::debug!(
            "Job ID: {:02X}, Asic nr: {}, Core: {}/{}, Ver: {:08X}",
            job_id,
            asic_nr,
            core_id,
            small_core_id,
            version_bits
        );

        Ok(vec![AsicResult::Nonce {
            job_id,
            nonce,
            rolled_version: version_bits, // caller must OR with original version
            asic_nr,
            timestamp_us: crate::common::now_us(),
        }])
    }

    /// Set hash frequency with ramp-up transition.
    fn set_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        // ESP-Miner parity: ASIC_set_frequency wraps EVERY chip's
        // send_hash_frequency in do_frequency_transition, so the PLL moves in
        // 6.25 MHz steps with a 100 ms settle between steps. An unramped large
        // jump (dashboard/MCP 200->500, or a thermal throttle 525->100) can drop
        // PLL lock / brown out the rail / spike HW errors. send_hash_frequency is
        // the exact per-step function (byte-identical writes); ramp both
        // directions like upstream. A local cursor drives the step loop while the
        // closure still updates self.current_frequency to the PLL-quantised value.
        // Capture the LAST per-step write error so a failed PLL write propagates
        // to the caller (main.rs frequency-change handler) instead of being
        // silently swallowed. Per-step writes stay byte-identical: each step
        // still calls send_hash_frequency exactly once. The ramp always runs to
        // completion; only the returned Result changes.
        let mut last_err = Ok(());
        let mut current = self.current_frequency;
        pll::do_frequency_transition(&mut current, target_freq, |freq| {
            if let Err(e) = self.send_hash_frequency(freq) {
                last_err = Err(e);
            }
        });
        last_err
    }

    /// Set version mask for version rolling.
    /// Port of BM1366_set_version_mask() from bm1366.c.
    fn set_version_mask(&mut self, mask: u32) -> Result<(), AsicError> {
        // Record for the observe-only malformed-version-bits metric (ASIC-3).
        // Does not affect the rolling math (still byte-identical to upstream).
        self.active_version_mask = mask;
        let versions_to_roll = mask >> 13;
        let version_byte0 = (versions_to_roll >> 8) as u8;
        let version_byte1 = (versions_to_roll & 0xFF) as u8;
        let version_cmd: [u8; 6] = [0x00, 0xA4, 0x90, 0x00, version_byte0, version_byte1];
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &version_cmd)
    }

    /// Read all known registers from all chips.
    /// Port of BM1366_read_registers() from bm1366.c.
    fn read_registers(&mut self) -> Result<Vec<RegisterData>, AsicError> {
        let register_addresses: &[u8] = &[0x4C, 0x88, 0x89, 0x8A, 0x8B, 0x8C];
        let mut results = Vec::new();

        for &reg in register_addresses {
            self.send_packet(TYPE_CMD | GROUP_ALL | CMD_READ, &[0x00, reg])?;
            std::thread::sleep(std::time::Duration::from_millis(1));

            // Try to read the response
            let mut buf = [0u8; 11];
            if let Ok(n) = self.serial.read(&mut buf, 100) {
                if n == 11 && crc5(&buf[2..11]) == 0 {
                    let value = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]);
                    let asic_address = buf[6];
                    let reg_type = register_type_for(buf[7]);
                    if reg_type != RegisterType::Invalid {
                        results.push(RegisterData {
                            register_type: reg_type,
                            asic_nr: if self.address_interval > 0 {
                                asic_address / self.address_interval
                            } else {
                                0
                            },
                            value,
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    fn chip_count(&self) -> u8 {
        self.chip_count
    }

    fn current_frequency(&self) -> f32 {
        self.current_frequency
    }

    fn read_responses(&mut self, timeout_ms: u16) -> Result<Vec<AsicResult>, AsicError> {
        // ASIC-4: read a window and reassemble on the [0xAA,0x55] preamble via
        // the carry buffer, so a misaligned/partial stream resyncs and keeps the
        // tail instead of flushing every frame. The intentional clear_buffer()
        // resync is kept as the fallback when the carry buffer fills with garbage
        // that never yields a valid frame (bounded recovery).
        let mut buf = [0u8; 64];
        match self.serial.read(&mut buf, timeout_ms) {
            Ok(n) if n > 0 => {
                self.append_rx_carry(&buf[..n]);
                let results = self.parse_rx_carry();
                // Fallback resync: if the carry buffer is saturated with bytes
                // that never form a valid frame, flush both it and the UART so
                // the next read starts clean (preserves the documented
                // clear_buffer() resync behavior).
                if results.is_empty() && self.rx_carry.len() >= 128 {
                    log::debug!("BM1366: carry buffer saturated with no frame, flushing UART");
                    self.rx_carry.clear();
                    let _ = self.serial.clear_buffer();
                }
                Ok(results)
            }
            Ok(_) => Ok(Vec::new()), // timeout, no data
            Err(e) => Err(e),
        }
    }

    fn set_difficulty(&mut self, difficulty: f64) -> Result<(), AsicError> {
        let difficulty_mask = get_difficulty_mask(difficulty);
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &difficulty_mask)?;
        log::info!("BM1366: TicketMask updated to difficulty {}", difficulty);
        Ok(())
    }

    fn set_max_baud(&mut self) -> Result<u32, AsicError> {
        log::info!("BM1366: Setting max baud to 1000000");
        let fast_uart: [u8; 6] = [0x00, 0x28, 0x11, 0x30, 0x02, 0x00];
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &fast_uart)?;
        self.serial.set_baud(1_000_000)?;
        self.serial.clear_buffer()?;
        Ok(1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::with_valid_crc5;
    use crate::AsicDriver;

    #[test]
    fn process_work_extracts_bm1366_job_id_mask() {
        let mut driver = BM1366::new(SerialPort::new());
        let frame = with_valid_crc5(
            [
                0xAA, 0x55, // response preamble
                0x12, 0x34, 0x56, 0x78, // nonce bytes
                0x00, // midstate
                0xAB, // job id plus small core id
                0x00, 0x02, // version bits before << 13
                0x00, // CRC/job flag filled by helper
            ],
            true,
        );

        let results = driver.process_work(&frame).expect("valid response");
        assert_eq!(results.len(), 1);
        match &results[0] {
            AsicResult::Nonce {
                job_id,
                nonce,
                rolled_version,
                ..
            } => {
                assert_eq!(*job_id, 0xA8);
                assert_eq!(*nonce, 0x7856_3412);
                assert_eq!(*rolled_version, 0x4000);
            }
            other => panic!("expected nonce result, got {other:?}"),
        }
    }

    /// Build an 11-byte BM1366 job-response frame carrying `nonce` (LE on wire)
    /// with `version_raw` and a CRC5-valid + job-flag-set final byte.
    fn bm1366_nonce_frame(nonce: u32, id: u8, version_raw: u16) -> [u8; 11] {
        let n = nonce.to_le_bytes();
        let v = version_raw.to_be_bytes();
        with_valid_crc5(
            [
                0xAA, 0x55, n[0], n[1], n[2], n[3], 0x00, id, v[0], v[1], 0x00,
            ],
            true,
        )
    }

    /// MD-4: a non-consecutive looped duplicate within the window is dropped.
    /// The old single `prev_nonce` scalar would have missed it.
    #[test]
    fn loop_duplicate_within_window_is_dropped() {
        let mut driver = BM1366::new(SerialPort::new());
        let a = 0xAAAA_0001u32;
        let b = 0xBBBB_0002u32;
        assert_eq!(
            driver
                .process_work(&bm1366_nonce_frame(a, 0xA0, 0))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            driver
                .process_work(&bm1366_nonce_frame(b, 0xA0, 0))
                .unwrap()
                .len(),
            1
        );
        assert!(
            driver
                .process_work(&bm1366_nonce_frame(a, 0xA0, 0))
                .unwrap()
                .is_empty(),
            "in-window looped duplicate must be filtered"
        );
    }

    /// ASIC-3: version bits OUTSIDE the active mask bump the observe-only
    /// counter but the nonce is STILL returned (telemetry, not a drop).
    #[test]
    fn malformed_version_bits_is_observe_only() {
        let mut driver = BM1366::new(SerialPort::new());
        // Active mask = default 0x1FFFE000 (bits 13..28). version_raw << 13.
        driver
            .set_version_mask(crate::common::STRATUM_DEFAULT_VERSION_MASK)
            .ok();
        assert_eq!(driver.malformed_version_bits(), 0);

        // version_raw=0x0001 -> bits = 1<<13 = 0x2000, inside the mask: no count.
        let r = driver
            .process_work(&bm1366_nonce_frame(0x10, 0xA0, 0x0001))
            .unwrap();
        assert_eq!(r.len(), 1, "in-mask version must still return a nonce");
        assert_eq!(
            driver.malformed_version_bits(),
            0,
            "in-mask bits must not count"
        );

        // version_raw=0xFFFF -> 0xFFFF<<13 = 0x1FFFE000 has bit 28 set; the top
        // bit (0x10000000) is inside the mask but higher bits spill out. Choose a
        // raw value whose shift lands a bit above bit 28: 0xFFFF<<13=0x1FFFE000.
        // 0xFFFF spans exactly the mask, so use a value that overflows: not
        // possible with <<13 of a u16 beyond bit 28. Instead temporarily narrow
        // the active mask so a normally-valid value falls outside it.
        driver.active_version_mask = 0x0000_2000; // only bit 13 allowed
        let r2 = driver
            .process_work(&bm1366_nonce_frame(0x11, 0xA0, 0x0002))
            .unwrap();
        assert_eq!(
            r2.len(),
            1,
            "out-of-mask version must STILL return the nonce"
        );
        assert_eq!(
            driver.malformed_version_bits(),
            1,
            "out-of-mask version bits must bump the observe-only counter"
        );
    }

    /// ASIC-4: a split 11-byte frame delivered across two reads is reassembled
    /// on the [0xAA,0x55] preamble (port of the bm1397 reassembler).
    #[test]
    fn read_responses_reassembles_split_frame() {
        let frame = bm1366_nonce_frame(0x7856_3412, 0xA0, 0x0002);
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        serial.push_rx(&frame[..5]);
        let mut driver = BM1366::new(serial);

        // First read sees only a partial frame — nothing yet, but no flush.
        assert!(driver.read_responses(0).unwrap().is_empty());
        driver.serial.push_rx(&frame[5..]);

        let results = driver.read_responses(0).unwrap();
        assert_eq!(
            results.len(),
            1,
            "split frame must reassemble, not be flushed"
        );
        match &results[0] {
            AsicResult::Nonce { nonce, .. } => assert_eq!(*nonce, 0x7856_3412),
            other => panic!("expected nonce, got {other:?}"),
        }
    }

    /// ASIC-4: leading garbage before a valid frame is discarded by the
    /// preamble scan and the real frame still parses.
    #[test]
    fn read_responses_resyncs_after_leading_garbage() {
        let frame = bm1366_nonce_frame(0x1234_5678, 0xA0, 0x0001);
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        serial.push_rx(&[0x13, 0x37, 0x00]); // 3 garbage bytes (no preamble)
        serial.push_rx(&frame);
        let mut driver = BM1366::new(serial);

        let results = driver.read_responses(0).unwrap();
        assert_eq!(
            results.len(),
            1,
            "must resync past garbage to the real frame"
        );
        match &results[0] {
            AsicResult::Nonce { nonce, .. } => assert_eq!(*nonce, 0x1234_5678),
            other => panic!("expected nonce, got {other:?}"),
        }
    }

    fn freq_packet_count(serial: &SerialPort) -> usize {
        serial
            .tx_bytes()
            .windows(2)
            .filter(|w| w[0] == 0x55 && w[1] == 0xAA)
            .count()
    }

    /// FIX 1: `set_frequency` must route through do_frequency_transition so a
    /// large jump ramps in 6.25 MHz steps (ESP-Miner parity), not a single
    /// unramped PLL write that can drop lock / brown the rail.
    #[test]
    fn set_frequency_ramps_in_multiple_steps() {
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        let mut driver = BM1366::new(serial);
        driver.current_frequency = 200.0;
        driver.serial.clear_tx();
        driver.set_frequency(500.0).expect("ramp ok");
        let packets = freq_packet_count(&driver.serial);
        assert!(
            packets > 1,
            "200->500 must ramp in multiple steps, got {packets}"
        );
    }

    /// FIX 2: a target below the chip's representable PLL minimum must error
    /// (fb_divider==0 sentinel) instead of writing fb_divider=0 / panicking on
    /// the `postdiv1 - 1` underflow under debug overflow checks.
    #[test]
    fn send_hash_frequency_rejects_below_min_target() {
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        let mut driver = BM1366::new(serial);
        assert!(
            driver.send_hash_frequency(10.0).is_err(),
            "below-min target must error, not write fb_divider=0"
        );
    }
}
