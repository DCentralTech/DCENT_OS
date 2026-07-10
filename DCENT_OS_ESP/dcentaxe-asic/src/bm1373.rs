// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — BM1373 ASIC driver — SCAFFOLD
//
// BM1373 (BM1373AA): Antminer S23 chip. Projected for future BitAxe boards.
// As of 2026-04-14, chip designation from internal intel. NO public data.
// ALL register values below are PROJECTIONS from BM1370 (closest predecessor).
//
// Status: PRE-HARDWARE SCAFFOLD — DO NOT USE FOR PRODUCTION
//
// ASIC-7 (preserve, do NOT relax): this whole module is the INTENTIONAL,
// fail-closed BM1373 scaffold kept for a future bring-up. Its consts, the
// `register_type_for` map, the struct fields, `send_packet`, and the trait
// methods' params are deliberately present-but-unwired so the real init/work
// code can be filled in against verified hardware values without re-deriving the
// shape. They therefore read as dead/unused today. We blanket-allow dead_code /
// unused_imports / unused_variables AT THE MODULE LEVEL (rather than gutting the
// scaffold or making `init()` return Ok) so the new clippy `-D warnings` CI gate
// stays green WITHOUT weakening the fail-closed fences — every trait method still
// returns an Err. Do NOT delete the scaffold and do NOT change any register/PLL/
// freq constant value.
#![allow(dead_code, unused_imports, unused_variables)]
//
// Chip ID: 0x1373 (projected)
// Response length: 11 bytes (same as BM1370)
// Job packet: 82-byte payload (same as BM1366/BM1368/BM1370)
// Job ID increment: TBD (BM1370 uses +16, BM1368 uses +24 via FPGA, +8 via serial)
// PLL fb_divider range: 160..=250 (projected, may be wider than BM1370's 160..=239)
//
// TODO:
//   - [ ] Verify chip ID via GetAddress command on hardware
//   - [ ] Capture init sequence from stock firmware
//   - [ ] Verify register values (PLL, MISC_CTRL, CORE_REG, IO_DRIVER)
//   - [ ] Determine cores per chip
//   - [ ] Verify PLL FB_DIV range
//   - [ ] Check for new BM1373-specific registers
//   - [ ] Determine job_id extraction mask and step
//   - [ ] Calibrate frequency/hashrate relationship

use crate::common::*;
use crate::crc::{crc16_false, crc5};
use crate::pll::{self, FREQ_MULT};
use crate::serial::SerialPort;

const CHIP_ID: u16 = 0x1373;
const CHIP_ID_RESPONSE_LENGTH: usize = 11;

/// Register map for BM1373 (PROJECTED from BM1370 — verify on hardware)
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

/// BM1373 driver — SCAFFOLD (pre-hardware)
///
/// Cloned from BM1370 driver with projected values.
/// ALL values MUST be verified on live hardware before production use.
pub struct BM1373 {
    serial: SerialPort,
    chip_count: u8,
    current_frequency: f32,
    address_interval: u8,
    prev_nonce: u32,
}

impl BM1373 {
    pub fn new(serial: SerialPort) -> Self {
        debug_assert!(
            cfg!(feature = "asic-bm1373-research"),
            "BM1373 is a pre-hardware scaffold; enable asic-bm1373-research only for lab work"
        );
        Self {
            serial,
            chip_count: 0,
            current_frequency: 50.0,
            address_interval: 0,
            prev_nonce: 0,
        }
    }

    // ── Low-level send ──────────────────────────────────────────────────

    fn send_packet(&mut self, header: u8, data: &[u8]) -> Result<(), AsicError> {
        let is_job = (header & TYPE_JOB) != 0;
        let total_length = if is_job {
            data.len() + 6
        } else {
            data.len() + 5
        };

        let mut buf = [0u8; 96]; // max packet size — stack-allocated
        buf[0] = 0x55;
        buf[1] = 0xAA;
        buf[2] = header;
        buf[3] = if is_job {
            (data.len() + 4) as u8
        } else {
            (data.len() + 3) as u8
        };
        buf[4..4 + data.len()].copy_from_slice(data);

        if is_job {
            let crc = crc16_false(&buf[2..4 + data.len()]);
            buf[4 + data.len()] = (crc >> 8) as u8;
            buf[5 + data.len()] = (crc & 0xFF) as u8;
        } else {
            buf[4 + data.len()] = crc5(&buf[2..4 + data.len()]);
        }

        self.serial
            .write(&buf[..total_length])
            .map_err(|e| AsicError::Serial(format!("BM1373 send_packet: {}", e)))?;
        Ok(())
    }

    // TODO: Implement remaining methods when hardware is available.
    // The following methods from BM1370 will need to be ported and verified:
    //   - send_read_register()
    //   - send_write_register()
    //   - send_chain_inactive()
    //   - send_set_address()
    //   - init_sequence() — the 14-step init
    //   - frequency_ramp()
    //   - parse_nonce_response()
}

fn scaffold_disabled(op: &str) -> AsicError {
    AsicError::InitFailed(format!(
        "BM1373 {op} is a pre-hardware scaffold and is disabled unless the \
         asic-bm1373-research feature is explicitly enabled"
    ))
}

// ── Command type constants (same as BM1366/BM1368/BM1370) ────────────────

const TYPE_JOB: u8 = 0x21;
const TYPE_CMD: u8 = 0x51;

// ── AsicDriver trait implementation ──────────────────────────────────────

impl crate::AsicDriver for BM1373 {
    fn init(
        &mut self,
        frequency: f32,
        chain_count: u8,
        initial_difficulty: f64,
    ) -> Result<u8, AsicError> {
        let _ = initial_difficulty; // scaffold — consumed once init is implemented
                                    // TODO: Implement full init sequence when hardware available.
                                    // Expected to follow BM1370 pattern:
                                    //   1. Version mask writes (3-4x)
                                    //   2. Chip ID enumeration (GetAddress broadcast)
                                    //   3. Chain Inactive + address assignment
                                    //   4. Register configuration (Reg_A8, MiscCtrl, CoreReg, TicketMask, etc.)
                                    //   5. BM1373-specific registers (verify 0xB9 and any new ones)
                                    //   6. Frequency ramp (56.25 MHz → target in 6.25 MHz steps)
                                    //   7. Hash Counting Number
        log::warn!("BM1373 init: SCAFFOLD — not yet implemented (pre-hardware)");
        Err(AsicError::InitFailed(
            "BM1373 driver is a pre-hardware scaffold. Cannot init without verified register values."
                .into(),
        ))
    }

    fn send_work(&mut self, job: &MiningJob) -> Result<(), AsicError> {
        // Expected: full header 82-byte format, same as BM1370.
        log::warn!("BM1373 send_work: SCAFFOLD — not yet implemented");
        let _ = job;
        Err(scaffold_disabled("send_work"))
    }

    fn process_work(&mut self, rx_buf: &[u8]) -> Result<Vec<AsicResult>, AsicError> {
        // Expected: 11-byte responses, same parsing as BM1370.
        // Job ID extraction mask: TBD (BM1370 uses (id & 0xf0) >> 1)
        log::warn!("BM1373 process_work: SCAFFOLD — not yet implemented");
        let _ = rx_buf;
        Err(scaffold_disabled("process_work"))
    }

    fn set_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        // Expected: PLL register writes at 0x08, ramped in 6.25 MHz steps.
        // FB_DIV range: 160-250 (projected, wider than BM1370's 160-239).
        log::warn!("BM1373 set_frequency: SCAFFOLD — not yet implemented");
        Err(AsicError::InitFailed(
            "BM1373 set_frequency not yet implemented".into(),
        ))
    }

    fn set_version_mask(&mut self, mask: u32) -> Result<(), AsicError> {
        // Expected: register 0xA4 write, same as BM1370.
        log::warn!("BM1373 set_version_mask: SCAFFOLD — not yet implemented");
        let _ = mask;
        Err(scaffold_disabled("set_version_mask"))
    }

    fn read_registers(&mut self) -> Result<Vec<RegisterData>, AsicError> {
        log::warn!("BM1373 read_registers: SCAFFOLD — not yet implemented");
        Err(scaffold_disabled("read_registers"))
    }

    fn chip_count(&self) -> u8 {
        self.chip_count
    }

    fn current_frequency(&self) -> f32 {
        self.current_frequency
    }

    fn read_responses(&mut self, _timeout_ms: u16) -> Result<Vec<AsicResult>, AsicError> {
        log::warn!("BM1373 read_responses: SCAFFOLD — not yet implemented");
        Err(scaffold_disabled("read_responses"))
    }

    fn set_difficulty(&mut self, difficulty: f64) -> Result<(), AsicError> {
        log::warn!("BM1373 set_difficulty: SCAFFOLD — not yet implemented");
        let _ = difficulty;
        Err(scaffold_disabled("set_difficulty"))
    }

    fn set_max_baud(&mut self) -> Result<u32, AsicError> {
        log::warn!("BM1373 set_max_baud: SCAFFOLD — not yet implemented");
        Err(scaffold_disabled("set_max_baud"))
    }
}
