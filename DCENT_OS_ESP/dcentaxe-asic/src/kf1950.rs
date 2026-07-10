// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — KF1950 (WhatsMiner K-series) ASIC driver
//
// ============================================================================
// UNTESTED — RESEARCH DRIVER. DO NOT ENABLE IN PRODUCTION BUILDS.
// ============================================================================
// Gated behind the `asic-kf1950` Cargo feature (default OFF).
//
// Source: kuenrg153/ESP-Miner KF1950 driver fork (first public open-source
// K-series implementation).
//
// Canonical RE doc:
//
//
// Upstream driver receives jobs but **produces ZERO nonces**. Three
// falsifiable hypotheses for the zero-nonce blocker (canonical RE doc §4):
//   H1 — Final baud-up commands are explicitly skipped (driver stays at
//        bootstrap baud forever; chip may need the handover before hashing).
//   H2 — No voltage commands sent (chip default may be below hashing minimum).
//   H3 — Only 1 of 6 midstate slots is populated in the 228-byte job packet.
//
// This driver intentionally **mirrors the broken upstream** rather than
// attempting a fix in software — the goal is a faithful comparator rig for
// hardware bring-up on a BitshokaNini V1.1 board (or fork-equivalent), not a
// speculative patch over an unknown root cause.
//
// Confidence map (per piece — see canonical RE doc):
//   - chip_id / chip_name:                          HIGH (90%) — explicit in fork
//   - cores_per_chip:                               LOW (40%) — borrowed from BM1397
//   - 9-phase init sequence (literal bytes):        MEDIUM (70%)
//   - Job packet build (228 bytes):                 MEDIUM (75%)
//   - Nonce response parse (11 bytes):              MEDIUM-HIGH (80%)
//   - Nonce job-id at byte[9] (NOT byte[4]):        MEDIUM-HIGH (80%)
//   - Multi-chip address-assignment loop:           LOW (40%) — fork only does 1 chip
//   - PLL frequency formula:                        NOT IMPLEMENTED (0%)
//   - Voltage control (TPS546D24A path):            NOT IMPLEMENTED (0%) — handled by power-manager
//   - Final baud-up (H1):                           NOT IMPLEMENTED (0%) — left as TODO
//   - Version rolling / ASICBoost:                  NOT IMPLEMENTED (no spec)

use crate::common::{AsicError, AsicResult, MiningJob, RegisterData};
use crate::crc::crc8_0x31;
use crate::serial::SerialPort;
use crate::AsicDriver;

// ─── Constants ──────────────────────────────────────────────────────────────

/// Bootstrap baud rate. The fork explicitly skips the "final baud commands"
/// that would switch to mining-grade baud. H1 hypothesis for zero-nonce.
pub const KF1950_INIT_BAUD: u32 = 363_636;

/// Job packet length on the wire (bytes).
pub const KF1950_JOB_PACKET_SIZE: usize = 228;

/// Nonce response length on the wire (bytes).
pub const KF1950_NONCE_RESPONSE_SIZE: usize = 11;

/// Default CRC-8 init for command frames.
const CRC_INIT_DEFAULT: u8 = 0xFF;

/// Special CRC-8 init used ONLY for address-assignment frame.
const CRC_INIT_ADDR_ASSIGN: u8 = 0x3A;

/// Cores-per-chip placeholder borrowed from BM1397 (same Samsung 8nm class).
/// CONFIDENCE: LOW (40%) — not verified for KF1950. Currently unused because
/// the `AsicDriver` trait has no `cores_per_chip()` method; kept here for the
/// upcoming `DispatcherConfig::for_kf1950` interval calculation.
#[allow(dead_code)]
pub const KF1950_CORES_PER_CHIP: u32 = 672;

/// Hardcoded PLL N value used by upstream fork. Real frequency control needs
/// the PLL formula RE'd from a stock WhatsMiner capture.
/// CONFIDENCE: NOT IMPLEMENTED (0%) for actual frequency mapping.
const HARDCODED_PLL_N: u8 = 0x80;

// ─── Init phase byte sequences (canonical RE doc §2.3) ──────────────────────
// Each constant holds PAYLOAD only; framer prepends `[0xFF, 0xFF]` and
// appends CRC-8 byte at runtime.

const PHASE1_CHIP_ID_READ: &[u8] = &[0x10, 0x00, 0x06];

const PHASE2_INIT_A: &[u8] = &[0x12, 0x34, 0x02, 0x13, 0xF4];
const PHASE2_INIT_B: &[u8] = &[0x07, 0x04, 0x02, 0x15, 0x15];
const PHASE2_INIT_C: &[u8] = &[0x00, 0x04, 0x01, 0xAA];

// CRC init 0x3A
const PHASE3_ADDR_ASSIGN: &[u8] = &[
    0x00, 0x14, 0x02, 0x00, 0x01, 0xE1, 0x00, 0x01, 0x02, 0x04, 0x01, 0x30,
];

const PHASE4_BAUD_DIV: &[u8] = &[0x07, 0x04, 0x02, 0x35, 0x35];

const PHASE5_LOCK: &[u8] = &[0x03, 0x04, 0x01, 0x05];
const PHASE5_RESET_A: &[u8] = &[0x04, 0x24, 0x01, 0x00];
const PHASE5_RESET_B: &[u8] = &[0x04, 0x24, 0x01, 0x01];

const PHASE6_BROADCAST: &[u8] = &[
    0x80, 0x44, 0x0C, 0x00, 0x00, 0x00, 0x20, 0x00, 0x40, 0x00, 0x60, 0x00, 0x00, 0x00, 0x30,
];
const PHASE6_FOLLOWUP: &[u8] = &[0x8D, 0xD4, 0x01, 0xAA];
const PHASE6_MASKS: [u8; 6] = [0x31, 0x21, 0x11, 0x91, 0xA1, 0xB1];

const PHASE7_A: &[u8] = &[0x02, 0x14, 0x01, 0x3A];
const PHASE7_B: &[u8] = &[0x23, 0x04, 0x06, 0x26, 0x1F, 0x30, 0x80, 0x00, 0xAA];
const PHASE7_C: &[u8] = &[0x24, 0x04, 0x07, 0x25, 0xFE, 0x50, 0x00, 0x60, 0x00, 0x00];
const PHASE7_D: &[u8] = &[0x05, 0x14, 0x04];
const PHASE7_RAW_TRIGGER: &[u8] = &[0xFF, 0xFF, 0xE3]; // sent without CRC
const PHASE7_E: &[u8] = &[0x05, 0x04, 0x01, 0x83];
const PHASE7_F: &[u8] = &[0x06, 0x04, 0x01, 0x11];

const PHASE8_LOCK: &[u8] = &[0x03, 0x04, 0x01, 0x04];
const PHASE8_B1: &[u8] = &[0x04, 0x04, 0x04, 0x01, 0x3F, 0xB1, 0xAA];

const PHASE9_LOCK_A: &[u8] = &[0x03, 0x04, 0x01, 0x04];
const PHASE9_LOCK_B: &[u8] = &[0x20, 0x04, 0x01, 0x04];

fn pll_payload(pll_n: u8, domain: u8) -> [u8; 9] {
    [0x03, 0x14, 0x06, 0x00, pll_n, 0x04, 0x05, domain, 0xAA]
}

fn phase6_mask_payload(mask: u8) -> [u8; 7] {
    [0x04, 0x04, 0x04, 0x01, 0x3F, mask, 0xAA]
}

fn phase8_pll_payload(pll_n: u8) -> [u8; 9] {
    [0x03, 0x14, 0x06, 0x00, pll_n, 0x04, 0x02, 0x03, 0xAA]
}

fn phase9_pll_a(pll_n: u8) -> [u8; 9] {
    [0x03, 0x14, 0x06, 0x00, pll_n, 0x04, 0x04, 0x00, 0xAA]
}

fn phase9_pll_b(pll_n: u8) -> [u8; 9] {
    [0x20, 0x14, 0x06, 0x00, pll_n, 0x04, 0x04, 0x0F, 0xAA]
}

// ─── Pure-logic helpers (testable without hardware) ─────────────────────────

/// Build a framed command: `[0xFF, 0xFF, payload..., CRC8(payload)]`.
///
/// CONFIDENCE: HIGH (90%) on framing; MEDIUM-HIGH (80%) on CRC coverage.
pub fn build_frame(payload: &[u8], crc_init: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + payload.len() + 1);
    out.push(0xFF);
    out.push(0xFF);
    out.extend_from_slice(payload);
    out.push(crc8_0x31(payload, crc_init));
    out
}

/// Build a 228-byte KF1950 job packet (canonical RE doc §2.4).
///
/// CONFIDENCE: MEDIUM (75%) on layout; LOW (50%) on the 5 unfilled midstate
/// slots in the 160-byte padding region — H3 hypothesis.
pub fn build_job_packet(
    job_id: u8,
    merkle_tail: [u8; 2],
    midstate0: &[u8; 32],
    ntime: u32,
    nbits_coeff: [u8; 3],
    starting_nonce: u32,
    counter: u8,
) -> [u8; KF1950_JOB_PACKET_SIZE] {
    let mut buf = [0u8; KF1950_JOB_PACKET_SIZE];

    // Frame header
    buf[0] = 0xFF;
    buf[1] = 0xFF;
    // JOB command
    buf[2] = 0x80;
    // Constants
    buf[3] = 0x04;
    buf[4] = 0xDE;
    // job_id, bit 7 forced high
    buf[5] = job_id | 0x80;
    // Merkle tail
    buf[6] = merkle_tail[0];
    buf[7] = merkle_tail[1];
    // Flag/slot/standard
    buf[8] = 0x00;
    buf[9] = 0x00;
    buf[10] = 0x58;
    // Derived 6-byte pattern (DD = (0x58 + 0x20) & 0xFF = 0x78)
    buf[11] = 0x07;
    buf[12] = 0x78;
    buf[13] = 0x07;
    buf[14] = 0x78;
    buf[15] = 0x07;
    buf[16] = 0x78;
    // Padding 0x55
    for b in &mut buf[17..21] {
        *b = 0x55;
    }
    // Midstate slot 0
    buf[21..53].copy_from_slice(midstate0);
    // Padding 0x55 × 160 (five unused 32-byte midstate slots — H3)
    for b in &mut buf[53..213] {
        *b = 0x55;
    }
    // Tail marker
    buf[213] = 0x56;
    // ntime LE
    buf[214..218].copy_from_slice(&ntime.to_le_bytes());
    // nbits coefficient BE
    buf[218] = nbits_coeff[0];
    buf[219] = nbits_coeff[1];
    buf[220] = nbits_coeff[2];
    // starting_nonce LE
    buf[221..225].copy_from_slice(&starting_nonce.to_le_bytes());
    // counter
    buf[225] = counter;
    // End marker
    buf[226] = 0xAA;
    // CRC over [2..=226]
    buf[227] = crc8_0x31(&buf[2..227], CRC_INIT_DEFAULT);

    buf
}

/// Decoded KF1950 nonce response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedNonce {
    pub chip_addr: u8,
    pub variant: u8,
    pub nonce: u32,
    pub extra_byte: u8,
    pub counter: u8,
}

/// Parse an 11-byte nonce response.
///
/// CONFIDENCE: MEDIUM-HIGH (80%). Job-id at byte[9] (counter echo),
/// NOT byte[4]. CRC covers bytes [0..=9] with default init.
pub fn parse_nonce_response(raw: &[u8]) -> Result<ParsedNonce, AsicError> {
    if raw.len() != KF1950_NONCE_RESPONSE_SIZE {
        return Err(AsicError::InvalidResponse(format!(
            "KF1950 nonce: expected {} bytes, got {}",
            KF1950_NONCE_RESPONSE_SIZE,
            raw.len()
        )));
    }
    let expected_crc = crc8_0x31(&raw[0..10], CRC_INIT_DEFAULT);
    if raw[10] != expected_crc {
        return Err(AsicError::CrcError);
    }
    Ok(ParsedNonce {
        chip_addr: raw[1],
        variant: raw[2],
        nonce: u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
        extra_byte: raw[8],
        counter: raw[9],
    })
}

// ─── Driver struct ──────────────────────────────────────────────────────────

/// KF1950 (WhatsMiner K-series) driver.
///
/// Wraps a `SerialPort` configured at `KF1950_INIT_BAUD`. Single-chip mode
/// only — multi-chip enumeration loop is unimplemented (LOW confidence).
pub struct Kf1950 {
    serial: SerialPort,
    chip_count: u8,
    current_frequency: f32,
    job_id_counter: u8,
}

impl Kf1950 {
    pub fn new(serial: SerialPort) -> Self {
        Self {
            serial,
            chip_count: 0,
            current_frequency: 0.0,
            job_id_counter: 0,
        }
    }

    /// Send a framed command (default CRC init).
    fn write_cmd(&mut self, payload: &[u8]) -> Result<(), AsicError> {
        let frame = build_frame(payload, CRC_INIT_DEFAULT);
        self.serial.write(&frame)?;
        Ok(())
    }

    /// Send a framed command with the address-assignment CRC init.
    fn write_cmd_addr_init(&mut self, payload: &[u8]) -> Result<(), AsicError> {
        let frame = build_frame(payload, CRC_INIT_ADDR_ASSIGN);
        self.serial.write(&frame)?;
        Ok(())
    }
}

impl AsicDriver for Kf1950 {
    /// Run the 9-phase init sequence at 363,636 baud.
    ///
    /// Returns the chip count claimed by the caller (no real readback yet —
    /// the chip-ID enumeration in Phase 1 is sent but the response is not
    /// parsed until UART read is wired up downstream).
    ///
    /// CONFIDENCE: MEDIUM (70%) on init bytes; LOW (40%) on multi-chip path.
    fn init(
        &mut self,
        frequency: f32,
        chain_count: u8,
        _initial_difficulty: f64,
    ) -> Result<u8, AsicError> {
        log::warn!(
            "KF1950 init — UNTESTED, expect zero nonces. \
             frequency={} MHz IGNORED (PLL formula not RE'd, fork hardcodes pll_n=0x80). \
 §4.",
            frequency
        );

        if chain_count == 0 {
            return Err(AsicError::NoAsicsFound);
        }
        if chain_count > 1 {
            return Err(AsicError::InitFailed(format!(
                "KF1950: multi-chip enumeration not implemented (chain_count={}). \
                 Upstream fork only handles 1 chip.",
                chain_count
            )));
        }

        // Ensure UART is at bootstrap baud. The fork explicitly skips the
        // baud-up; we mirror that.
        if self.serial.baud_rate() != KF1950_INIT_BAUD {
            self.serial.set_baud(KF1950_INIT_BAUD)?;
        }

        // Phase 1: chip-ID readback
        self.write_cmd(PHASE1_CHIP_ID_READ)?;
        // TODO: read 11-byte responses (one per chip), check CHIP_ID == 0x1950.

        // Phase 2: initial config
        self.write_cmd(PHASE2_INIT_A)?;
        self.write_cmd(PHASE2_INIT_B)?;
        self.write_cmd(PHASE2_INIT_C)?;

        // Phase 3: address assignment (CRC init 0x3A)
        self.write_cmd_addr_init(PHASE3_ADDR_ASSIGN)?;

        // Phase 4: baud divider
        self.write_cmd(PHASE4_BAUD_DIV)?;

        // Phase 5: PLL config (4 domains × 2 iterations = 8 outer cycles,
        // 5 frames each = 40 frames total).
        for _iteration in 0..2 {
            for domain in 0..4u8 {
                self.write_cmd(PHASE5_LOCK)?;
                self.write_cmd(&pll_payload(HARDCODED_PLL_N, domain))?;
                self.write_cmd(PHASE5_RESET_A)?;
                self.write_cmd(PHASE5_RESET_B)?;
            }
        }

        // Phase 6: core enables
        self.write_cmd(PHASE6_BROADCAST)?;
        self.write_cmd(PHASE6_FOLLOWUP)?;
        for &mask in PHASE6_MASKS.iter() {
            self.write_cmd(&phase6_mask_payload(mask))?;
        }
        self.write_cmd(PHASE5_RESET_A)?;
        self.write_cmd(PHASE5_RESET_B)?;

        // Phase 7: post-config registers
        self.write_cmd(PHASE7_A)?;
        self.write_cmd(PHASE7_B)?;
        self.write_cmd(PHASE7_C)?;
        self.write_cmd(PHASE7_D)?;
        self.serial.write(PHASE7_RAW_TRIGGER)?; // raw, no CRC
        self.write_cmd(PHASE7_E)?;
        self.write_cmd(PHASE7_F)?;

        // Phase 8: PLL phase 2 + B1 second pass
        self.write_cmd(PHASE8_LOCK)?;
        self.write_cmd(&phase8_pll_payload(HARDCODED_PLL_N))?;
        self.write_cmd(PHASE8_B1)?;

        // Phase 9: final PLL writes
        self.write_cmd(PHASE9_LOCK_A)?;
        self.write_cmd(&phase9_pll_a(HARDCODED_PLL_N))?;
        self.write_cmd(PHASE9_LOCK_B)?;
        self.write_cmd(&phase9_pll_b(HARDCODED_PLL_N))?;

        // H1 hypothesis: final baud-up commands intentionally NOT sent.
        log::warn!(
            "KF1950 init complete at {} bps. Final baud-up SKIPPED \
             (matches upstream fork). H1 hypothesis: this is why nonces \
             never return.",
            KF1950_INIT_BAUD
        );

        self.chip_count = chain_count;
        self.current_frequency = frequency;
        Ok(self.chip_count)
    }

    /// Submit a mining job.
    ///
    /// Uses `job.midstates[0]` if present (BM1397-style). If no midstate is
    /// supplied, returns InitFailed — the dispatcher must compute a midstate
    /// from `prev_block_hash + version + first 28 bytes of merkle_root` and
    /// place it in `midstates[0]` before dispatch.
    ///
    /// CONFIDENCE: MEDIUM (75%) on packet layout; MEDIUM-LOW on the
    /// dispatcher contract.
    fn send_work(&mut self, job: &MiningJob) -> Result<(), AsicError> {
        let midstate0: &[u8; 32] = job.midstates.first().ok_or_else(|| {
            AsicError::InitFailed(
                "KF1950 send_work: job.midstates is empty. KF1950 expects a \
                 pre-computed midstate in slot 0; dispatcher must populate."
                    .to_string(),
            )
        })?;

        // Merkle tail: first two bytes of merkle4 (BM1397-style remainder).
        let merkle_tail = [job.merkle4[0], job.merkle4[1]];

        let nbits_coeff: [u8; 3] = [
            ((job.nbits >> 16) & 0xFF) as u8,
            ((job.nbits >> 8) & 0xFF) as u8,
            (job.nbits & 0xFF) as u8,
        ];

        let counter = self.job_id_counter;
        self.job_id_counter = self.job_id_counter.wrapping_add(1);

        let packet = build_job_packet(
            job.job_id,
            merkle_tail,
            midstate0,
            job.ntime,
            nbits_coeff,
            job.starting_nonce,
            counter,
        );

        self.serial.write(&packet)?;
        Ok(())
    }

    /// Parse one or more 11-byte nonce frames out of `rx_buf`.
    ///
    /// CONFIDENCE: MEDIUM-HIGH (80%) when the buffer contains aligned
    /// 11-byte frames. Frame resync after partial reads is NOT implemented —
    /// caller must align reads.
    fn process_work(&mut self, rx_buf: &[u8]) -> Result<Vec<AsicResult>, AsicError> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + KF1950_NONCE_RESPONSE_SIZE <= rx_buf.len() {
            let chunk = &rx_buf[i..i + KF1950_NONCE_RESPONSE_SIZE];
            match parse_nonce_response(chunk) {
                Ok(parsed) => {
                    out.push(AsicResult::Nonce {
                        // job_id from byte[9] (counter echo), NOT byte[4]
                        job_id: parsed.counter,
                        nonce: parsed.nonce,
                        rolled_version: 0, // version rolling not supported
                        asic_nr: parsed.chip_addr,
                        timestamp_us: crate::common::now_us(),
                    });
                }
                Err(_) => {
                    // Skip one byte and try to resync. This is naive but
                    // matches the BM1370 driver's resync philosophy.
                }
            }
            i += KF1950_NONCE_RESPONSE_SIZE;
        }
        Ok(out)
    }

    /// CONFIDENCE: NOT IMPLEMENTED (0%) — PLL formula unknown.
    fn set_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        Err(AsicError::InitFailed(format!(
            "KF1950 set_frequency({} MHz): PLL formula not yet RE'd. \
             Fork hardcodes pll_n=0x80 regardless of target.",
            target_freq
        )))
    }

    /// Version rolling is not supported by the upstream fork.
    fn set_version_mask(&mut self, _mask: u32) -> Result<(), AsicError> {
        // No-op — upstream sets `rolled_version = 0` always.
        Ok(())
    }

    /// CONFIDENCE: NOT IMPLEMENTED — register read protocol partially known
    /// (Phase 1 chip-ID and `[0x11, 0x00, 0x01]` status read) but no full
    /// register map.
    fn read_registers(&mut self) -> Result<Vec<RegisterData>, AsicError> {
        Ok(Vec::new())
    }

    fn chip_count(&self) -> u8 {
        self.chip_count
    }

    fn current_frequency(&self) -> f32 {
        self.current_frequency
    }

    /// Read any pending UART bytes and parse them.
    fn read_responses(&mut self, timeout_ms: u16) -> Result<Vec<AsicResult>, AsicError> {
        // Read one nonce-frame's worth at a time.
        let mut buf = [0u8; 64];
        let n = self.serial.read(&mut buf, timeout_ms)?;
        if n == 0 {
            return Ok(Vec::new());
        }
        self.process_work(&buf[..n])
    }

    /// CONFIDENCE: NOT IMPLEMENTED — KF1950 difficulty / ticket mask register
    /// address is not in the canonical RE doc.
    fn set_difficulty(&mut self, _difficulty: f64) -> Result<(), AsicError> {
        Ok(())
    }

    /// CONFIDENCE: NOT IMPLEMENTED — this is the H1 hypothesis. The fork
    /// explicitly skips final baud-up; restoring it is the leading suspect
    /// for fixing zero-nonce.
    fn set_max_baud(&mut self) -> Result<u32, AsicError> {
        Err(AsicError::InitFailed(
            "KF1950 set_max_baud: H1 hypothesis. Upstream fork explicitly \
             skips final baud-up commands. Implementing this requires \
             discovering which command bytes the fork omits, then issuing \
             them and reconfiguring the host UART. See canonical RE doc §4."
                .to_string(),
        ))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_header_payload_crc() {
        let frame = build_frame(&[0x10, 0x00, 0x06], CRC_INIT_DEFAULT);
        assert_eq!(frame.len(), 6);
        assert_eq!(frame[0], 0xFF);
        assert_eq!(frame[1], 0xFF);
        assert_eq!(&frame[2..5], &[0x10, 0x00, 0x06]);
        assert_eq!(frame[5], crc8_0x31(&[0x10, 0x00, 0x06], CRC_INIT_DEFAULT));
    }

    #[test]
    fn job_packet_is_228_bytes() {
        let buf = build_job_packet(
            1,
            [0xAA, 0xBB],
            &[0u8; 32],
            0x6402_FFFF,
            [0x17, 0x02, 0x13],
            0,
            0x42,
        );
        assert_eq!(buf.len(), KF1950_JOB_PACKET_SIZE);
    }

    #[test]
    fn job_packet_fixed_offsets() {
        let buf = build_job_packet(0x01, [0xAA, 0xBB], &[0u8; 32], 0, [0; 3], 0, 0);
        assert_eq!(buf[0], 0xFF);
        assert_eq!(buf[1], 0xFF);
        assert_eq!(buf[2], 0x80);
        assert_eq!(buf[3], 0x04);
        assert_eq!(buf[4], 0xDE);
        assert_eq!(buf[10], 0x58);
        assert_eq!(&buf[11..17], &[0x07, 0x78, 0x07, 0x78, 0x07, 0x78]);
        assert_eq!(buf[213], 0x56);
        assert_eq!(buf[226], 0xAA);
    }

    #[test]
    fn job_packet_job_id_bit7_forced_high() {
        let buf = build_job_packet(0x00, [0; 2], &[0; 32], 0, [0; 3], 0, 0);
        assert_eq!(buf[5] & 0x80, 0x80);
    }

    #[test]
    fn job_packet_endianness() {
        let buf = build_job_packet(
            1,
            [0; 2],
            &[0u8; 32],
            0x1234_5678,
            [0x17, 0x02, 0x13],
            0xDEAD_BEEF,
            0,
        );
        assert_eq!(&buf[214..218], &[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(&buf[218..221], &[0x17, 0x02, 0x13]);
        assert_eq!(&buf[221..225], &[0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn job_packet_crc_self_consistent() {
        let buf = build_job_packet(1, [0xAA, 0xBB], &[0u8; 32], 0x1234, [0; 3], 0, 0);
        assert_eq!(buf[227], crc8_0x31(&buf[2..227], CRC_INIT_DEFAULT));
    }

    fn build_nonce_for_test(n: &ParsedNonce) -> [u8; KF1950_NONCE_RESPONSE_SIZE] {
        let mut raw = [0u8; KF1950_NONCE_RESPONSE_SIZE];
        raw[0] = 0x00;
        raw[1] = n.chip_addr;
        raw[2] = n.variant;
        raw[3] = 0x06;
        raw[4..8].copy_from_slice(&n.nonce.to_le_bytes());
        raw[8] = n.extra_byte;
        raw[9] = n.counter;
        raw[10] = crc8_0x31(&raw[0..10], CRC_INIT_DEFAULT);
        raw
    }

    #[test]
    fn parse_nonce_round_trip() {
        let n = ParsedNonce {
            chip_addr: 0x42,
            variant: 0x04,
            nonce: 0xDEAD_BEEF,
            extra_byte: 0x12,
            counter: 0x88,
        };
        let raw = build_nonce_for_test(&n);
        let parsed = parse_nonce_response(&raw).unwrap();
        assert_eq!(parsed, n);
    }

    #[test]
    fn parse_nonce_rejects_bad_crc() {
        let n = ParsedNonce {
            chip_addr: 1,
            variant: 0,
            nonce: 0,
            extra_byte: 0,
            counter: 0,
        };
        let mut raw = build_nonce_for_test(&n);
        raw[10] ^= 0xFF;
        assert!(matches!(
            parse_nonce_response(&raw),
            Err(AsicError::CrcError)
        ));
    }

    #[test]
    fn parse_nonce_rejects_short_buf() {
        assert!(parse_nonce_response(&[0u8; 10]).is_err());
    }

    #[test]
    fn process_work_extracts_aligned_frames() {
        let n = ParsedNonce {
            chip_addr: 0x05,
            variant: 0x04,
            nonce: 0x1234_5678,
            extra_byte: 0,
            counter: 0x99,
        };
        let raw = build_nonce_for_test(&n);
        // Two back-to-back frames.
        let mut buf = Vec::new();
        buf.extend_from_slice(&raw);
        buf.extend_from_slice(&raw);

        // We need a Kf1950 instance — but the SerialPort stub is fine here.
        let port = SerialPort::new();
        let mut drv = Kf1950::new(port);
        let results = drv.process_work(&buf).unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            match r {
                AsicResult::Nonce {
                    job_id,
                    nonce,
                    asic_nr,
                    ..
                } => {
                    // job_id from byte[9] (counter), not byte[4]
                    assert_eq!(*job_id, 0x99);
                    assert_eq!(*nonce, 0x1234_5678);
                    assert_eq!(*asic_nr, 0x05);
                }
                _ => panic!("expected nonce"),
            }
        }
    }
}
