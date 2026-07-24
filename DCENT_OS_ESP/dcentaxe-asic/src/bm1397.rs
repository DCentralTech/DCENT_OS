// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — BM1397 ASIC driver
// Faithful port from ESP-Miner bm1397.c
//
// BM1397: Used in original BitAxe
// Chip ID: 0x1397
// Chip ID response length: 9 bytes (NOT 11 like BM1366/BM1368/BM1370!)
// Job packet: variable length with up to 4 midstates (14 + 4 + N*32 bytes)
//   - 1 midstate:  14 + 4 + 32 = 50 bytes payload
//   - 4 midstates: 14 + 4 + 128 = 146 bytes payload (job_packet = 146)
// Job ID increment: +4, mod 128
// PLL fb_divider range: 60..=200
// No version rolling via version mask (set_version_mask is a no-op in C)
// Uses midstate-based work (not full block header like BM1366/68/70)

use crate::common::*;
use crate::crc::{crc16_false, crc5};
use crate::pll;
use crate::serial::SerialPort;
use crate::AsicDriver;

const CHIP_ID: u16 = 0x1397;
const CHIP_ID_RESPONSE_LENGTH: usize = 9;

// Register addresses
const CLOCK_ORDER_CONTROL_0: u8 = 0x80;
const CLOCK_ORDER_CONTROL_1: u8 = 0x84;
const ORDERED_CLOCK_ENABLE: u8 = 0x20;
const CORE_REGISTER_CONTROL: u8 = 0x3C;
const PLL3_PARAMETER: u8 = 0x68;
const FAST_UART_CONFIGURATION: u8 = 0x28;
const MISC_CONTROL: u8 = 0x18;

const SLEEP_TIME_MS: u64 = 20;

// ── Gated open-core (cross-pollination ASIC-2 / XPRE-1) ─────────────────────
//
// Ported faithfully from DCENT_OS `dcentrald-asic/src/drivers/bm1398.rs`
// (commit 97b3e852, `open_core_enable_value` + `send_open_core_work`), which
// itself ports the Bitmain S17 factory jig's `single_BM1397_open_core` per-core
// `enable_core_clock` sweep: `CoreRegCtrl (0x3C) = (core << 16) | 0x84AA`,
// 3 cores/slot × 84 slots = 252 writes. ESP-Miner-heritage drivers (this one
// included) assume cores boot active and never run open-core; the factory runs
// the explicit sweep right before mining. This lands the jig-verified
// core-activation half so it can be A/B tested on a BM1397/Max bench unit.
//
// **Default-OFF** via `DCENTAXE_BM_OPEN_CORE=1` (see `bm_open_core_enabled`),
// read only at the end of `init()`, so default firmware is byte-identical and
// no field-proven board changes behavior unless an operator opts in. The jig
// also issues a dummy "open" work + `OpenCoreGap` per slot; that WORK_TX
// trigger is intentionally NOT wired here (matching DCENT_OS) — its format +
// necessity must be confirmed on a live BM1397/S17 first. The enable sweep is
// the jig-verified differentiator DCENT_axe currently lacks.

/// BM1397 per-core `enable_core_clock` register value, jig-verified from the
/// S17 factory jig (`single_BM1397_open_core`): `CoreRegCtrl (0x3C) =
/// (core << 16) | 0x84AA`. Anchored to DCENT_OS `bm1398.rs::open_core_enable_value`.
const OPEN_CORE_ENABLE_BASE: u32 = 0x84AA;
/// 84 slots (0x54) × 3 cores/slot = 252 cores swept. From the S17 jig.
const OPEN_CORE_SLOTS: u32 = 0x54;

/// Per-core open-core enable value: `(core << 16) | 0x84AA`. Free fn so host
/// tests can call it directly (mirrors DCENT_OS `open_core_enable_value`).
fn open_core_enable_value(core: u32) -> u32 {
    (core << 16) | OPEN_CORE_ENABLE_BASE
}

/// The 6-byte `CoreRegCtrl` write payload for a given core, byte-identical to
/// the init4 frame layout `[0x00, 0x3C, value_BE]`. Free fn so host tests pin
/// the exact wire bytes.
fn open_core_payload(core: u32) -> [u8; 6] {
    let v = open_core_enable_value(core);
    [
        0x00,
        CORE_REGISTER_CONTROL,
        (v >> 24) as u8,
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
    ]
}

/// Default-OFF gate for the BM open-core sweep (live A/B only). Mirrors
/// DCENT_OS `bm139x_open_core_enabled` (`DCENT_BM139X_OPEN_CORE`). `std::env`
/// works on esp-rs std; this crate already uses `std::thread::sleep`.
fn bm_open_core_enabled() -> bool {
    std::env::var("DCENTAXE_BM_OPEN_CORE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Register map for BM1397 (different from BM1366/68/70)
fn register_type_for(addr: u8) -> RegisterType {
    match addr {
        0x04 => RegisterType::Hashrate,
        0x4C => RegisterType::ErrorCount,
        _ => RegisterType::Invalid,
    }
}

/// BM1397 driver
pub struct BM1397 {
    serial: SerialPort,
    chip_count: u8,
    current_frequency: f32,
    address_interval: u8,
    job_id: u32,
    /// Bounded per-stream recent-nonce filter (driver-level dedup).
    ///
    /// ASIC-1 fix: the previous `prev_nonce` + `first_nonce`/`nonce_found`
    /// model promoted upstream's *call-local* `first_nonce`/`nonce_found`
    /// (`bm1397.c:319-320`, reset every call so the `else if` branch is dead)
    /// into *persistent* struct fields, which permanently blacklisted the
    /// session's very first nonce. The ring catches the same consecutive AND
    /// looped duplicates without ever permanently blacklisting a value — an old
    /// nonce ages out after `RECENT_NONCE_RING_LEN` newer distinct nonces.
    /// Shared design with bm1366/68/70 (MD-4).
    recent_nonces: RecentNonceRing,
    rx_carry: Vec<u8>,
}

impl BM1397 {
    pub fn new(serial: SerialPort) -> Self {
        Self {
            serial,
            chip_count: 0,
            current_frequency: 50.0,
            address_interval: 0,
            job_id: 0,
            recent_nonces: RecentNonceRing::new(),
            rx_carry: Vec::with_capacity(128),
        }
    }

    fn append_rx_carry(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.rx_carry.len() == 128 {
                self.rx_carry.remove(0);
            }
            self.rx_carry.push(byte);
        }
    }

    fn parse_rx_carry(&mut self) -> Vec<AsicResult> {
        let mut results = Vec::new();

        loop {
            let Some(preamble_pos) = self
                .rx_carry
                .windows(2)
                .position(|window| window == [0xAA, 0x55])
            else {
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
            if self.rx_carry.len() < 9 {
                break;
            }

            let mut frame = [0u8; 9];
            frame.copy_from_slice(&self.rx_carry[..9]);
            match self.process_work(&frame) {
                Ok(mut parsed) => {
                    results.append(&mut parsed);
                    self.rx_carry.drain(..9);
                }
                Err(e) => {
                    log::debug!("BM1397: dropping invalid response byte: {}", e);
                    self.rx_carry.drain(..1);
                }
            }
        }

        results
    }

    // ── Low-level send ──────────────────────────────────────────────────

    fn send_packet(&mut self, header: u8, data: &[u8]) -> Result<(), AsicError> {
        let is_job = (header & TYPE_JOB) != 0;
        let total_length = if is_job {
            data.len() + 6
        } else {
            data.len() + 5
        };

        let mut buf = [0u8; 160]; // max packet size — stack-allocated (BM1397 jobs up to 152 bytes)
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

        self.serial.write(&buf[..total_length])?;
        Ok(())
    }

    fn send_read_address(&mut self) -> Result<(), AsicError> {
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_READ, &[0x00, 0x00])
    }

    fn send_chain_inactive(&mut self) -> Result<(), AsicError> {
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_INACTIVE, &[0x00, 0x00])
    }

    fn set_chip_address(&mut self, addr: u8) -> Result<(), AsicError> {
        self.send_packet(TYPE_CMD | GROUP_SINGLE | CMD_SETADDRESS, &[addr, 0x00])
    }

    // ── Frequency ───────────────────────────────────────────────────────

    /// Port of BM1397_send_hash_frequency().
    /// Note: BM1397 uses a different PLL write sequence than BM1366/68/70:
    /// it writes prefreq (pll0_divider) twice, then freqbuf (pll0_parameter) twice.
    fn send_hash_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        let params = pll::find_best_pll(target_freq, 60, 200);

        // Reject a target below the chip's representable PLL minimum. When no
        // (refdiv,postdiv1,postdiv2) yields an in-range fb_divider, find_best_pll
        // leaves fb_divider=0/refdiv=0 (same all-zero sentinel as ESP-Miner
        // pll.c). BM1397's postdiv encoding uses `& 0x7` so it does not underflow
        // like BM1366/68/70, but sending fb_divider=0/refdiv=0 is still a garbage
        // PLL write — guard at the Rust boundary instead of changing the search.
        if params.fb_divider == 0 {
            return Err(AsicError::InitFailed(format!(
                "target {target_freq} MHz below BM1397 PLL minimum (no in-range fb_divider)"
            )));
        }

        let vdo_scale: u8 = 0x40;
        // BM1397 uses different postdiv encoding: (postdiv1 & 0x7) << 4 + (postdiv2 & 0x7)
        let postdiv = ((params.postdiv1 & 0x7) << 4) + (params.postdiv2 & 0x7);
        let freqbuf: [u8; 6] = [
            0x00,
            0x08,
            vdo_scale,
            params.fb_divider,
            params.refdiv,
            postdiv,
        ];
        let prefreq1: [u8; 6] = [0x00, 0x70, 0x0F, 0x0F, 0x0F, 0x00]; // pll0_divider

        // Send prefreq twice with 10ms delays
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &prefreq1)?;
        }

        // Send freqbuf twice with 10ms delays
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &freqbuf)?;
        }

        std::thread::sleep(std::time::Duration::from_millis(10));

        log::info!(
            "Setting Frequency to {} MHz ({})",
            target_freq,
            params.actual_freq
        );
        self.current_frequency = params.actual_freq;
        Ok(())
    }

    // ── Count chips ─────────────────────────────────────────────────────

    fn count_chips(&mut self, expected_count: u8) -> Result<u8, AsicError> {
        // BM1397 has 9-byte chip ID response (not 11)
        let mut buffer = [0u8; 9];
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

            let preamble = ((buffer[0] as u16) << 8) | buffer[1] as u16;
            if preamble != PREAMBLE_BE {
                log::warn!(
                    "Preamble mismatch: expected 0x{:04x}, got 0x{:04x}",
                    PREAMBLE_BE,
                    preamble
                );
                continue;
            }

            let received_chip_id = ((buffer[2] as u16) << 8) | buffer[3] as u16;
            if received_chip_id != CHIP_ID {
                log::warn!(
                    "CHIP_ID mismatch: expected 0x{:04x}, got 0x{:04x}",
                    CHIP_ID,
                    received_chip_id
                );
                continue;
            }

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

            // Saturating: a looping/echoing chain or noise delivering >= 256
            // CRC5-valid CHIP_ID frames must not overflow this u8 (debug panic) or
            // wrap to 0 / small (release: false NoAsicsFound / mis-sized interval).
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::with_valid_crc5;
    use crate::AsicDriver;

    #[test]
    fn process_work_extracts_bm1397_job_and_midstate_ids() {
        let mut driver = BM1397::new(SerialPort::new());
        let frame = with_valid_crc5(
            [
                0xAA, 0x55, // response preamble
                0x12, 0x34, 0x56, 0x78, // nonce bytes
                0x00, // midstate byte from ASIC
                0xCF, // job id 0xCC plus midstate index 3
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
                assert_eq!(*job_id, 0xCC);
                assert_eq!(*nonce, 0x7856_3412);
                assert_eq!(*rolled_version, 0x03);
            }
            other => panic!("expected nonce result, got {other:?}"),
        }
    }

    #[test]
    fn read_responses_reassembles_split_bm1397_frame() {
        let frame = with_valid_crc5(
            [
                0xAA, 0x55, // response preamble
                0x22, 0x34, 0x56, 0x78, // nonce bytes
                0x00, // midstate byte from ASIC
                0xC1, // job id 0xC0 plus midstate index 1
                0x00, // CRC/job flag filled by helper
            ],
            true,
        );

        let mut serial = SerialPort::new();
        serial.init().unwrap();
        serial.push_rx(&frame[..4]);
        let mut driver = BM1397::new(serial);

        assert!(driver.read_responses(0).unwrap().is_empty());
        driver.serial.push_rx(&frame[4..]);

        let results = driver.read_responses(0).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AsicResult::Nonce {
                job_id,
                nonce,
                rolled_version,
                ..
            } => {
                assert_eq!(*job_id, 0xC0);
                assert_eq!(*nonce, 0x7856_3422);
                assert_eq!(*rolled_version, 1);
            }
            other => panic!("expected nonce result, got {other:?}"),
        }
    }

    #[test]
    fn read_responses_keeps_tail_after_garbage_and_split_frames() {
        let frame = with_valid_crc5([0xAA, 0x55, 0x23, 0x34, 0x56, 0x78, 0x00, 0xC2, 0x00], true);

        let mut serial = SerialPort::new();
        serial.init().unwrap();
        serial.push_rx(&[0x13, 0x37, frame[0], frame[1], frame[2]]);
        let mut driver = BM1397::new(serial);

        assert!(driver.read_responses(0).unwrap().is_empty());
        driver.serial.push_rx(&frame[3..]);

        let results = driver.read_responses(0).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            AsicResult::Nonce {
                job_id,
                nonce,
                rolled_version,
                ..
            } => {
                assert_eq!(*job_id, 0xC0);
                assert_eq!(*nonce, 0x7856_3423);
                assert_eq!(*rolled_version, 2);
            }
            other => panic!("expected nonce result, got {other:?}"),
        }
    }

    // ── Gated open-core (ASIC-2 / XPRE-1) — jig-verified pins ────────────────

    /// Pins the jig-verified BM1397 per-core enable_core_clock value
    /// (`CoreRegCtrl 0x3C = (core << 16) | 0x84AA`, S17 factory jig). Mirrors
    /// DCENT_OS `bm1398.rs::bm139x_open_core_enable_value_matches_jig`.
    #[test]
    fn open_core_enable_value_matches_jig() {
        assert_eq!(open_core_enable_value(0), 0x0000_84AA);
        assert_eq!(open_core_enable_value(1), 0x0001_84AA);
        assert_eq!(open_core_enable_value(0x54), 0x0054_84AA);
        assert_eq!(open_core_enable_value(0xA8), 0x00A8_84AA);
    }

    /// Pins the exact 6-byte CoreRegCtrl wire payload — must be byte-identical
    /// to the init4 frame layout `[0x00, 0x3C, value_BE]`.
    #[test]
    fn open_core_payload_is_core_reg_ctrl_be() {
        assert_eq!(open_core_payload(0), [0x00, 0x3C, 0x00, 0x00, 0x84, 0xAA]);
        assert_eq!(open_core_payload(1), [0x00, 0x3C, 0x00, 0x01, 0x84, 0xAA]);
        assert_eq!(
            open_core_payload(0xA8),
            [0x00, 0x3C, 0x00, 0xA8, 0x84, 0xAA]
        );
    }

    /// The slot/core iteration covers all 252 cores exactly once (no dupes,
    /// no gaps): 84 slots × {slot, slot+84, slot+168}.
    #[test]
    fn open_core_sweep_covers_252_cores_no_dupes() {
        use std::collections::BTreeSet;
        let mut set = BTreeSet::new();
        for slot in 0..OPEN_CORE_SLOTS {
            for core in [slot, slot + OPEN_CORE_SLOTS, slot + 2 * OPEN_CORE_SLOTS] {
                assert!(set.insert(core), "duplicate core {core} in sweep");
            }
        }
        assert_eq!(set.len(), 252, "expected 252 unique cores");
        assert_eq!(set.iter().next().copied(), Some(0), "min core must be 0");
        assert_eq!(
            set.iter().next_back().copied(),
            Some(251),
            "max core must be 251"
        );
    }

    /// The gate is default-OFF: with `DCENTAXE_BM_OPEN_CORE` unset (CI default),
    /// the open-core sweep never runs, so default firmware is byte-identical.
    #[test]
    fn open_core_gate_default_off() {
        assert!(
            !bm_open_core_enabled(),
            "DCENTAXE_BM_OPEN_CORE must be unset in CI — open-core is default-OFF"
        );
    }

    // ── Nonce dedup ring (ASIC-1 / MD-4) ────────────────────────────────────

    /// Build a BM1397 job-response frame carrying `nonce` (LE on wire) and
    /// `id`, with a CRC5-valid + job-flag-set final byte.
    fn bm1397_nonce_frame(nonce: u32, id: u8) -> [u8; 9] {
        let n = nonce.to_le_bytes();
        with_valid_crc5([0xAA, 0x55, n[0], n[1], n[2], n[3], 0x00, id, 0x00], true)
    }

    /// ASIC-1: the session's first nonce must NOT be permanently blacklisted.
    /// The old persistent `first_nonce`/`nonce_found` model filtered it forever;
    /// the bounded ring ages it out and accepts a genuine rediscovery.
    #[test]
    fn first_nonce_is_not_blacklisted_forever() {
        let mut driver = BM1397::new(SerialPort::new());
        let first = 0x1111_1111u32;

        // First sighting passes.
        assert_eq!(
            driver
                .process_work(&bm1397_nonce_frame(first, 0xC1))
                .unwrap()
                .len(),
            1
        );
        // Immediate repeat is filtered.
        assert!(driver
            .process_work(&bm1397_nonce_frame(first, 0xC1))
            .unwrap()
            .is_empty());

        // Flush the ring with RECENT_NONCE_RING_LEN distinct other nonces.
        for k in 0..RECENT_NONCE_RING_LEN as u32 {
            let other = 0x2000_0000 + k;
            assert_eq!(
                driver
                    .process_work(&bm1397_nonce_frame(other, 0xC1))
                    .unwrap()
                    .len(),
                1
            );
        }

        // `first` has aged out — a genuine rediscovery is accepted again, NOT
        // silently dropped forever (the regression this fix corrects).
        assert_eq!(
            driver
                .process_work(&bm1397_nonce_frame(first, 0xC1))
                .unwrap()
                .len(),
            1,
            "aged-out first nonce must be accepted, not blacklisted forever"
        );
    }

    /// ASIC-1 / MD-4: a non-consecutive looped duplicate within the window is
    /// dropped (the single prev_nonce scalar would have missed it).
    #[test]
    fn bm1397_loop_duplicate_within_window_is_dropped() {
        let mut driver = BM1397::new(SerialPort::new());
        let a = 0xAAAA_0001u32;
        let b = 0xBBBB_0002u32;
        let c = 0xCCCC_0003u32;
        assert_eq!(
            driver
                .process_work(&bm1397_nonce_frame(a, 0xC1))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            driver
                .process_work(&bm1397_nonce_frame(b, 0xC1))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            driver
                .process_work(&bm1397_nonce_frame(c, 0xC1))
                .unwrap()
                .len(),
            1
        );
        // `a` was not the immediately-previous nonce but is still in-window.
        assert!(
            driver
                .process_work(&bm1397_nonce_frame(a, 0xC1))
                .unwrap()
                .is_empty(),
            "in-window looped duplicate must be filtered"
        );
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
    /// unramped PLL write. BM1397 emits the prefreq/freqbuf double-write per step,
    /// so a multi-step ramp yields many [0x55,0xAA] preambles.
    #[test]
    fn set_frequency_ramps_in_multiple_steps() {
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        let mut driver = BM1397::new(serial);
        // 100 -> 180 MHz, both inside the BM1397 60..=200 fb_divider range.
        driver.current_frequency = 100.0;
        driver.serial.clear_tx();
        driver.set_frequency(180.0).expect("ramp ok");
        let packets = freq_packet_count(&driver.serial);
        assert!(
            packets > 1,
            "100->180 must ramp in multiple steps, got {packets}"
        );
    }

    /// FIX 2: a target below the chip's representable PLL minimum must error
    /// (fb_divider==0 sentinel) instead of writing fb_divider=0/refdiv=0.
    #[test]
    fn send_hash_frequency_rejects_below_min_target() {
        let mut serial = SerialPort::new();
        serial.init().unwrap();
        let mut driver = BM1397::new(serial);
        assert!(
            driver.send_hash_frequency(10.0).is_err(),
            "below-min target must error, not write fb_divider=0"
        );
    }
}

// ── AsicDriver trait implementation ─────────────────────────────────────────

impl super::AsicDriver for BM1397 {
    /// Initialize the BM1397 chain.
    /// Exact port of BM1397_init() from bm1397.c.
    fn init(
        &mut self,
        frequency: f32,
        asic_count: u8,
        initial_difficulty: f64,
    ) -> Result<u8, AsicError> {
        // Send the init command (read address on all chips)
        self.send_read_address()?;

        let chip_counter = self.count_chips(asic_count)?;
        if chip_counter == 0 {
            return Err(AsicError::NoAsicsFound);
        }

        // Sleep 20ms then chain inactive
        std::thread::sleep(std::time::Duration::from_millis(SLEEP_TIME_MS));
        self.send_chain_inactive()?;

        // Split chip address space evenly
        self.address_interval = (256 / chip_counter as u16) as u8;
        for i in 0..chip_counter {
            self.set_chip_address(i.wrapping_mul(self.address_interval))?;
        }

        // init1: clock_order_control0 = {0x00, 0x80, 0x00, 0x00, 0x00, 0x00}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, CLOCK_ORDER_CONTROL_0, 0x00, 0x00, 0x00, 0x00],
        )?;

        // init2: clock_order_control1 = {0x00, 0x84, 0x00, 0x00, 0x00, 0x00}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, CLOCK_ORDER_CONTROL_1, 0x00, 0x00, 0x00, 0x00],
        )?;

        // init3: ordered_clock_enable = {0x00, 0x20, 0x00, 0x00, 0x00, 0x01}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, ORDERED_CLOCK_ENABLE, 0x00, 0x00, 0x00, 0x01],
        )?;

        // init4: core_register_control = {0x00, 0x3C, 0x80, 0x00, 0x80, 0x74}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, CORE_REGISTER_CONTROL, 0x80, 0x00, 0x80, 0x74],
        )?;

        // Set difficulty mask (from caller — last-known pool diff or safe default).
        log::info!(
            "BM1397: init difficulty={:.3} (overridden on first mining.set_difficulty)",
            initial_difficulty
        );
        let difficulty_mask = get_difficulty_mask(initial_difficulty);
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &difficulty_mask)?;

        // init5: pll3_parameter = {0x00, 0x68, 0xC0, 0x70, 0x01, 0x11}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, PLL3_PARAMETER, 0xC0, 0x70, 0x01, 0x11],
        )?;

        // init6: fast_uart_configuration = {0x00, 0x28, 0x06, 0x00, 0x00, 0x0F}
        self.send_packet(
            TYPE_CMD | GROUP_ALL | CMD_WRITE,
            &[0x00, FAST_UART_CONFIGURATION, 0x06, 0x00, 0x00, 0x0F],
        )?;

        // Set default baud
        self.set_default_baud()?;

        // Frequency ramp-up
        pll::do_frequency_transition(&mut self.current_frequency, frequency, |freq| {
            let params = pll::find_best_pll(freq, 60, 200);
            if params.fb_divider == 0 {
                log::warn!("PLL: no in-range solution for {freq} MHz step; skipping write");
                return;
            }
            let vdo_scale: u8 = 0x40;
            let postdiv = ((params.postdiv1 & 0x7) << 4) + (params.postdiv2 & 0x7);
            let freqbuf: [u8; 6] = [
                0x00,
                0x08,
                vdo_scale,
                params.fb_divider,
                params.refdiv,
                postdiv,
            ];
            let prefreq1: [u8; 6] = [0x00, 0x70, 0x0F, 0x0F, 0x0F, 0x00];

            // Build and send prefreq twice — stack-allocated
            for _ in 0..2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                let header = TYPE_CMD | GROUP_ALL | CMD_WRITE;
                let data = &prefreq1;
                let total_len = data.len() + 5;
                let mut buf = [0u8; 16]; // 6-byte data + 5 overhead = 11
                buf[0] = 0x55;
                buf[1] = 0xAA;
                buf[2] = header;
                buf[3] = (data.len() + 3) as u8;
                buf[4..4 + data.len()].copy_from_slice(data);
                buf[4 + data.len()] = crc5(&buf[2..4 + data.len()]);
                let _ = self.serial.write(&buf[..total_len]);
            }

            // Build and send freqbuf twice — stack-allocated
            for _ in 0..2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                let header = TYPE_CMD | GROUP_ALL | CMD_WRITE;
                let data = &freqbuf;
                let total_len = data.len() + 5;
                let mut buf = [0u8; 16]; // 6-byte data + 5 overhead = 11
                buf[0] = 0x55;
                buf[1] = 0xAA;
                buf[2] = header;
                buf[3] = (data.len() + 3) as u8;
                buf[4..4 + data.len()].copy_from_slice(data);
                buf[4 + data.len()] = crc5(&buf[2..4 + data.len()]);
                let _ = self.serial.write(&buf[..total_len]);
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
        });

        // Gated, default-OFF open-core sweep (cross-pollination ASIC-2/XPRE-1).
        // Runs ONLY when DCENTAXE_BM_OPEN_CORE=1 — same place the S17 jig runs
        // open_core (before mining) and where DCENT_OS calls it (after init_chain).
        // Default firmware is byte-identical (returns Ok(0) when the flag is unset).
        let _ = self.send_open_core_work()?;

        self.chip_count = chip_counter;
        Ok(chip_counter)
    }

    /// Send a mining job to the BM1397 chain.
    /// Port of BM1397_send_work() from bm1397.c.
    ///
    /// BM1397 uses midstate-based jobs (not full block headers).
    /// The job_packet struct in C is:
    ///   job_id(1) + num_midstates(1) + starting_nonce(4) + nbits(4) + ntime(4) +
    ///   merkle4(4) + midstate(32) + [midstate1(32) + midstate2(32) + midstate3(32)]
    /// Total: 14 + 4 + num_midstates * 32 bytes
    fn send_work(&mut self, job: &MiningJob) -> Result<(), AsicError> {
        // Use the dispatcher's job_id — must match what the dispatcher stores for nonce mapping
        let job_id = job.job_id;

        let num_midstates = job.midstates.len().min(4) as u8;
        if num_midstates == 0 {
            return Err(AsicError::InvalidResponse(
                "BM1397 requires at least 1 midstate".to_string(),
            ));
        }

        // Build job payload — stack-allocated (max: 14 + 4 + 4*32 = 146 bytes)
        let mut payload = [0u8; 148];
        let mut pos = 0;

        payload[pos] = job_id;
        pos += 1;
        payload[pos] = num_midstates;
        pos += 1;
        payload[pos..pos + 4].copy_from_slice(&job.starting_nonce.to_le_bytes());
        pos += 4;
        payload[pos..pos + 4].copy_from_slice(&job.nbits.to_le_bytes());
        pos += 4;
        payload[pos..pos + 4].copy_from_slice(&job.ntime.to_le_bytes());
        pos += 4;
        payload[pos..pos + 4].copy_from_slice(&job.merkle4);
        pos += 4;

        // Always send 4 midstates (zero-padded if fewer available) to match
        // ESP-Miner which always sends the full 146-byte job_packet struct.
        for i in 0..4 {
            if i < job.midstates.len() {
                payload[pos..pos + 32].copy_from_slice(&job.midstates[i]);
            }
            // else: already zero-initialized
            pos += 32;
        }

        self.send_packet(TYPE_JOB | GROUP_SINGLE | CMD_WRITE, &payload[..pos])?;

        // Log first 20 jobs with full detail for debugging
        if self.job_id < 20 {
            log::info!("BM1397 Job {:02X}: ms={} nonce={:08x} nbits={:08x} ntime={:08x} merkle4={:02X?} midstate[0..4]={:02X?}",
                job_id, num_midstates, job.starting_nonce, job.nbits, job.ntime,
                &job.merkle4, &job.midstates[0][..4]);
        }
        self.job_id = self.job_id.wrapping_add(1);
        Ok(())
    }

    /// Process a work response from the BM1397.
    /// Port of BM1397_process_work() from bm1397.c.
    ///
    /// BM1397 response is 9 bytes (not 11 like BM1366/68/70):
    ///   preamble(2) + nonce(4) + midstate_num(1) + id(1) + crc5(1)
    fn process_work(&mut self, rx_buf: &[u8]) -> Result<Vec<AsicResult>, AsicError> {
        // BM1397 response: 9 bytes
        if rx_buf.len() < 9 {
            return Err(AsicError::InvalidResponse(format!(
                "BM1397 response too short: {} bytes",
                rx_buf.len()
            )));
        }

        let preamble = ((rx_buf[0] as u16) << 8) | rx_buf[1] as u16;
        if preamble != PREAMBLE_BE {
            return Err(AsicError::PreambleMismatch);
        }

        if crc5(&rx_buf[2..9]) != 0 {
            return Err(AsicError::CrcError);
        }

        let is_job_response = (rx_buf[8] & 0x80) != 0;

        if !is_job_response {
            // Register response (bytes 2-7)
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
        let _midstate_num = rx_buf[6];
        let id = rx_buf[7];

        let rx_job_id = id & 0xfc;
        let rx_midstate_index = id & 0x03;

        // Driver-level duplicate-nonce filtering (ASIC-1 / MD-4).
        //
        // BM1397 hardware re-emits the same nonce stream in a loop, so a nonce
        // can repeat both consecutively and non-consecutively (interleaved with
        // other nonces). Without a guard we double-submit shares to the pool and
        // they get rejected as "duplicate share". A bounded recent-nonce ring
        // (last RECENT_NONCE_RING_LEN distinct values) catches both cases.
        //
        // Unlike upstream's single `prev_nonce` (or the earlier persistent
        // `first_nonce` model that blacklisted the session's first nonce
        // forever), an aged-out nonce is accepted again — a genuinely
        // rediscovered valid nonce in a later job is not lost. This is the
        // per-stream tier; cross-stream dedup is handled at the dispatcher.
        if self.recent_nonces.is_duplicate(nonce) {
            return Ok(Vec::new());
        }

        // BE interpretation for bit-field extraction (matches ntohl in C)
        let nonce_h = u32::from_be_bytes([rx_buf[2], rx_buf[3], rx_buf[4], rx_buf[5]]);
        let asic_nr = if self.address_interval > 0 {
            ((nonce_h >> 17) & 0xff) as u8 / self.address_interval
        } else {
            0
        };
        let core_id = ((nonce_h >> 25) & 0x7f) as u8;
        let small_core_id = id & 0x0f;

        log::debug!(
            "Job ID: {:02X}, Asic nr: {}, Core: {}/{}, midstate_idx: {}",
            rx_job_id,
            asic_nr,
            core_id,
            small_core_id,
            rx_midstate_index
        );

        // Note: For BM1397, rolled_version must be computed by the caller
        // using increment_bitmask() for the midstate index.
        // We return the midstate_index in the nonce result so the caller can do this.
        // The rolled_version field here stores midstate_index as a hint.
        Ok(vec![AsicResult::Nonce {
            job_id: rx_job_id,
            nonce,
            rolled_version: rx_midstate_index as u32, // caller must compute actual version
            asic_nr,
            timestamp_us: crate::common::now_us(),
        }])
    }

    fn set_frequency(&mut self, target_freq: f32) -> Result<(), AsicError> {
        // ESP-Miner parity: ASIC_set_frequency wraps EVERY chip's
        // send_hash_frequency in do_frequency_transition, so the PLL moves in
        // 6.25 MHz steps with a 100 ms settle between steps. An unramped large
        // jump (dashboard/MCP, or a thermal throttle) can drop PLL lock / brown
        // out the rail / spike HW errors. send_hash_frequency is the exact
        // per-step function (byte-identical writes, incl. the prefreq/freqbuf
        // double-write + 10 ms gaps); ramp both directions like upstream. A local
        // cursor drives the step loop while the closure still updates
        // self.current_frequency to the PLL-quantised value.
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

    /// BM1397 does NOT support version mask setting (placeholder/no-op in C code).
    fn set_version_mask(&mut self, _mask: u32) -> Result<(), AsicError> {
        // BM1397_set_version_mask is a no-op in the C code
        Ok(())
    }

    fn read_registers(&mut self) -> Result<Vec<RegisterData>, AsicError> {
        // BM1397 only has registers 0x04 (hashrate) and 0x4C (error count)
        let register_addresses: &[u8] = &[0x04, 0x4C];
        let mut results = Vec::new();

        for &reg in register_addresses {
            self.send_packet(TYPE_CMD | GROUP_ALL | CMD_READ, &[0x00, reg])?;
            std::thread::sleep(std::time::Duration::from_millis(1));

            // BM1397 responses are 9 bytes
            let mut buf = [0u8; 9];
            if let Ok(n) = self.serial.read(&mut buf, 100) {
                if n == 9 && crc5(&buf[2..9]) == 0 {
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
        // Read up to 64 bytes to check what's in the UART buffer
        let mut buf = [0u8; 64];
        match self.serial.read(&mut buf, timeout_ms) {
            Ok(n) if n > 0 => {
                log::debug!("UART RX: {} bytes: {:02X?}", n, &buf[..n.min(18)]);
                self.append_rx_carry(&buf[..n]);
                Ok(self.parse_rx_carry())
            }
            Ok(_) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn set_difficulty(&mut self, difficulty: f64) -> Result<(), AsicError> {
        let difficulty_mask = get_difficulty_mask(difficulty);
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &difficulty_mask)?;
        log::info!("BM1397: TicketMask updated to difficulty {}", difficulty);
        Ok(())
    }

    fn set_max_baud(&mut self) -> Result<u32, AsicError> {
        // XPRE-2 (verify-only, no change): BM1397 fast baud (3.125M) is
        // MISC_CONTROL(reg 0x18)/CLKI-derived — reg value 0x0000_6031, byte-exact
        // to DCENT_OS `MISC_CTRL_FAST_BAUD`. This is the jig-correct BM1397
        // mechanism and is NOT a PLL1 (reg 0x60) reclock. The DCENT_OS BM1370/
        // BM1362 jig finding (set_chain_baud must RMW-reclock PLL1 at baud
        // >= 3,000,001) does NOT apply to any DCENT_axe path: only BM1397 reaches
        // >= 3M, and it does so via MiscControl/CLKI, not PLL1. BM1366/68/70 cap
        // at exactly 1,000,000 (below the threshold). Do NOT add a PLL1 reclock.
        log::info!("BM1397: Setting max baud to 3125000");
        let baudrate: [u8; 6] = [0x00, MISC_CONTROL, 0x00, 0x00, 0b01100000, 0b00110001];
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &baudrate)?;
        // Change host UART to match
        self.serial.set_baud(3_125_000)?;
        self.serial.clear_buffer()?;
        Ok(3_125_000)
    }
}

impl BM1397 {
    /// Set default baud rate (115749).
    /// Port of BM1397_set_default_baud().
    /// Baud formula = 25M/((denominator+1)*8), denominator=26 -> 115749
    ///
    /// ASIC-8: this writes only the ASIC-side MISC_CONTROL baud divider (ASIC TX
    /// → 115749). The HOST UART deliberately stays at 115200 (`UART_FREQ`),
    /// faithful to upstream `bm1397.c` (no host reconfig here). The ~0.43%
    /// asymmetry is well within UART tolerance (<2.5%) and is harmless because
    /// the init window between here and `set_max_baud()` is read-free:
    /// `count_chips()` reads BEFORE this call (both ends still at 115200), and
    /// `set_max_baud()` reconciles BOTH ends to 3.125M immediately after `init()`.
    /// Do NOT insert a response read between `init()` and `set_max_baud()` — it
    /// would parse 115749-baud frames at 115200 and could show elevated CRC
    /// failures. No reads are valid until `set_max_baud()` reconciles both ends.
    pub fn set_default_baud(&mut self) -> Result<i32, AsicError> {
        let baudrate: [u8; 6] = [0x00, MISC_CONTROL, 0x00, 0x00, 0b01111010, 0b00110001];
        self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &baudrate)?;
        Ok(115749)
    }

    /// Gated, default-OFF BM1397 open-core — a faithful port of the Bitmain S17
    /// factory jig's `single_BM1397_open_core` (cross-pollination ASIC-2/XPRE-1),
    /// anchored to DCENT_OS `bm1398.rs::send_open_core_work` (commit 97b3e852).
    ///
    /// Runs the jig-verified per-core `enable_core_clock` sweep
    /// (`CoreRegCtrl (0x3C) = (core << 16) | 0x84AA`, 3 cores/slot × 84 slots =
    /// 252 writes) using the existing `send_packet` path, so each frame is
    /// byte-identical to the init4 `CoreRegCtrl` write `[0x00, 0x3C, value_BE]`.
    ///
    /// **Default-OFF** (`DCENTAXE_BM_OPEN_CORE=1`); returns `Ok(0)` and does
    /// NOTHING when the flag is unset. The jig's per-slot dummy-work + OpenCoreGap
    /// trigger is intentionally NOT wired here (matching DCENT_OS) — its format +
    /// necessity must be confirmed on a live BM1397/S17 first. Returns the number
    /// of `CoreRegCtrl` writes issued.
    fn send_open_core_work(&mut self) -> Result<u32, AsicError> {
        if !bm_open_core_enabled() {
            return Ok(0);
        }
        log::warn!(
            "DCENTAXE_BM_OPEN_CORE=1 — running the jig BM1397 per-core enable_core_clock \
             sweep (84 slots x 3 cores = 252 writes). LIVE A/B ONLY; the per-slot \
             dummy-work trigger is not yet wired (validate on a live BM1397/Max first)."
        );
        let mut writes = 0u32;
        for slot in 0..OPEN_CORE_SLOTS {
            for core in [slot, slot + OPEN_CORE_SLOTS, slot + 2 * OPEN_CORE_SLOTS] {
                self.send_packet(TYPE_CMD | GROUP_ALL | CMD_WRITE, &open_core_payload(core))?;
                std::thread::sleep(std::time::Duration::from_millis(1));
                writes += 1;
            }
        }
        log::info!(
            "BM1397 open-core per-core enable sweep complete (jig-ported, gated): {} writes",
            writes
        );
        Ok(writes)
    }
}
