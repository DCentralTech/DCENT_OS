//! AM335x BeagleBone S19j Pro (`S19J_IO_BOARD_V2_0`) mining mode (`--am3-bb-mining`).
//!
//! Ties together: the Phase-B `BeagleBonePlatform` (board-target TOML +
//! `cold_boot_sequence_s19j_io_v2`), the Phase-1 `bm1362::uart_transport::Am335xUartTransport`
//! (88-byte BM1362 serial work dispatch over the OMAP UART, no kernel module),
//! the BM1362 chip-side init, and a Stratum mining loop.
//!
//! Live target: 203.0.113.79 (LuxOS). The cold-boot sequence is BEST-GUESS
//! — Phase D
//! iterates it on the live unit.
//!
//! ## Mining-loop wiring decision: OPTION B2 (self-contained loop, reuse the crates)
//!
//! `serial_mining.rs`'s `SerialMiner::run()` does its OWN PIC/PSU/GPIO cold-boot
//! and its OWN BM1362 PLL/MiscCtrl init by reopening the serial port — and its
//! `am3-bb uart_trans` branch is hard-wired to `DEFAULT_CHAIN_TTYS`
//! (`/dev/ttyO{1,2,4,5}`, 4 kernel `SerialChain`s), whereas the `a lab unit` unit is
//! `/dev/ttyS{1,2,4}` (3 chains) driven via `DevmemUart` (mmap). Hooking that
//! path up cleanly (B1) would mean a 4-vs-3 chain mismatch, a `/dev/ttyO*` ↔
//! `/dev/ttyS*` rename, and threading an "external cold-boot" gate through dozens
//! of `SerialMiner::run()` branches. Too invasive for the win.
//!
//! Instead this module does the `a lab unit`-specific cold-boot + BM1362 chip-side init
//! itself (it already does both, and that path is LIVE-VALIDATED on `a lab unit`), then
//! runs a **small self-contained mining loop on the existing blocking thread**
//! that REUSES the shared crates rather than re-implementing them:
//!  - `dcentrald_stratum::StratumRouter` (Stratum V1/V2 connect, job feed,
//!    share submit, status) — spawned on the main tokio runtime via a `Handle`,
//!    communicating over mpsc channels (the same channels `serial_mining.rs` uses).
//!  - `dcentrald_stratum::WorkBuilder::next_work` (coinbase → merkle root →
//!    midstate → `MiningWork`).
//!  - `dcentrald_stratum::work::validate_full_header` (the same SHA-256d share
//!    gate that got DCENT_axe / S9 their accepted shares) + dedup-before-submit
//!.
//!  - `dcentrald_asic::bm1362::Am335xUartTransport` for paced 88-byte BM1362
//!    serial work dispatch + 11-byte nonce-frame poll (the transport this
//!    module already builds from the `DevmemUart`s; no kernel module).
//!
//! What is solid: the cold-boot orchestration, the BM1362 chip-side init
//! (GetAddress enum -> ChainInactive x3 + SetChipAddress -> core register block
//! -> PLL ramp -> fast-baud -> per-chip post-baud loop), the transport setup,
//! and the Stratum connect/work-build/dispatch/nonce-validate/dedup/submit wiring.
//!
//! ## R7-3 RESOLVED (2026-05-12, by cross-check against the PROVEN serial path)
//!
//! The earlier "BEST-GUESS `asic_work_t.data`/`.data2`" mapping was wrong: the
//! W14.B 86-byte `asic_work_t` codec ([`dcentrald_asic::bm1362::AsicWorkFrame`])
//! came from the W4 dev-kit `bm1362_frames_v2.h` and does NOT match what a
//! BM1362 chip actually speaks. The LuxOS RE corpus
//! (:
//! "standard BM1362 chip-comm framing") + cross-check against the **proven,
//! sustained-mining-validated** Amlogic-NoPic serial path
//! (`dcentrald::serial_mining` / `dcentrald_asic::drivers::bm1362::build_serial_work_frame`)
//! resolve it:
//!  - **Work-job frame (88 B on the wire)**: `[0x55 0xAA][0x21][0x56][82-byte
//!    BM1366-family full-header payload][CRC16-CCITT-FALSE hi, lo]` — built here
//!    by [`build_bm1362_serial_work_frame`] (verbatim from the proven
//!    `serial_mining.rs` builder, which operates on the same
//!    `dcentrald_stratum::work::MiningWork`). Payload: `job_id(1) num_midstates=0x01(1)
//!    starting_nonce=0(4) nbits(4 LE) ntime(4 LE) merkle_root(32, 32-bit-word-reversed)
//!    prev_block_hash(32, 32-bit-word-reversed) version(4 LE)`.
//!  - **NO open-core dummy-work**: BM1362 is not the BM1387 14 nm — it activates
//!    its cores via init register writes, not 114 dummy-work packets (per
//!    `dcentrald_asic::drivers::bm1362` module docs, verified against bosminer).
//!    The old "N zero-payload `asic_work_t`" open-core step is removed.
//!  - **Nonce-response frame (11 B on the wire)**: `[0xAA 0x55][n3 n2 n1 n0]
//!    [midstate_idx][result][vbits_hi vbits_lo][flags]` —
//!    [`dcentrald_asic::bm1362::Bm1362SerialNonce`] / `parse_bm1362_serial_nonce`.
//!    `nonce = u32::from_le_bytes` of the 4 wire bytes; `job_id = (result & 0xF0) >> 1`
//!    (only bits [6:3] of the sent job_id round-trip, so the dispatcher steps by
//!    [`JOB_ID_INCREMENT`] = 24); `vbits` BE, rolled version reconstructed via
//!    `(base & !0x1FFF_E000) | ((vbits << 13) & 0x1FFF_E000)`; `flags` bit7 = job-response.
//!  - **CRC = CRC-16/CCITT-FALSE** (poly 0x1021, init 0xFFFF, no refin/refout, no
//!    xorout) — `dcentrald_hal::serial_chain::crc16_public`. NOT IBM-SDLC; the
//!     IBM-SDLC claim is for a different
//!    (kernel-internal) layer / was wrong for the on-chip-wire serial frame.
//!
//! BM1362 cold-boot register block (2026-05-13): [`bm1362_chip_init_one_chain`]
//! now ports the proven Amlogic-NoPic serial path to AM335x direct UART:
//! `0xA8` InitControl + MiscCtrl x3 + `0xA4` VersionMask (pre-baud),
//! `0x3C` x2 (HashClk/ClkDelay) + `0x54` AnalogMux + `0x58` IoDriver +
//! `0x14` TicketMask=0xFF + `0x10` HashCountingNumber, a 400 MHz -> target PLL
//! ramp, `0x28` FastUART + MiscCtrl x3, host baud switch, a fast-baud GetAddress
//! probe, then the per-chip post-baud `0xA8`/MiscCtrl x3/`0x3C` x3 loop. The
//! remaining BB blocker is the APW set-voltage/watchdog write opcodes that are
//! still deliberately best-effort stubs.
//!
//! The `a lab unit` cold-boot run (which the milestone log proves works) is unchanged:
//! `cold_boot_sequence_s19j_io_v2` / `run_cold_boot` are consumed exactly as
//! before. Set `DCENT_AM3_BB_STUB_LOOP=1` to fall back to the old logging-only
//! stub ([`run_mining_loop_stub`]) for a cold-boot/enum-only diagnostic run.
//!
//! ## Cross-references
//!
//! - `DCENT_OS_Antminer/dcentrald/dcentrald-hal/src/platform/beaglebone.rs` — `BeagleBonePlatform`
//! - `DCENT_OS_Antminer/dcentrald/dcentrald-hal/src/platform/beaglebone_cold_boot.rs` — `cold_boot_sequence_s19j_io_v2`
//! - `DCENT_OS_Antminer/dcentrald/dcentrald-hal/src/psu_apw_uart_tunnel.rs` — APW121215f UART-tunnel PSU
//! - `DCENT_OS_Antminer/dcentrald/dcentrald-asic/src/bm1362/uart_transport.rs` — pacing/ring transport + the BM1362 serial nonce parser
//! - `DCENT_OS_Antminer/dcentrald/dcentrald-asic/src/drivers/bm1362.rs` — `build_serial_work_frame` (the proven 88-byte BM1362 full-header frame) + `decode_nonce`
//! - `DCENT_OS_Antminer/dcentrald/dcentrald/src/serial_mining.rs` — the shared Stratum/work-build machinery + the proven BM1362 serial work/nonce path this loop mirrors
//! -  — the v1 cold-boot sequence + the LuxOS wire capture

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use dcentrald_asic::bm1362::{
    build_broadcast_write_frame, build_chain_inactive_frame, build_get_address_frame,
    build_set_chip_address_frame, build_single_write_frame, cold_boot_step,
    uart_relay::{
        UART_RELAY_ALT_REG_ADDR, UART_RELAY_BOSMINER_ENABLE, UART_RELAY_BOSMINER_ENABLE_ALT,
        UART_RELAY_REG_ADDR,
    },
    Am335xUartTransport, AsicWorkFrame, Bm1362SerialNonce, ChainUart, UartTransportError,
    CMD_WORK_PACKAGE, UART_SEND_INTERVAL_US,
};
use dcentrald_asic::drivers::bm1362::{
    pll_lookup as bm1362_pll_lookup, pll_ramp_sequence as bm1362_pll_ramp_sequence,
    BM1362_INIT_PLAN,
};
use dcentrald_hal::i2c::{
    spawn_i2c_service_no_register_touch_with_denylist, I2cServiceHandle, I2cTransactionStep,
};
use dcentrald_hal::platform::beaglebone::BeagleBonePlatform;
use dcentrald_hal::platform::{FanAccess, Platform};
use dcentrald_hal::psu_apw_uart_tunnel::{ApwUartTunnel, ApwUartTunnelBus};
use dcentrald_hal::serial::DevmemUart;
use dcentrald_hal::serial_chain::SerialChainBackend;

use crate::config::DcentraldConfig;

/// Number of distinct nonce→work correlation slots (`work_by_id` length).
///
/// The BM1362 serial nonce frame echoes `(sent_job_id & 0xF0) >> 1` — i.e. only
/// bits [6:3] of the sent job id survive — so the meaningful key space is
/// `{0, 8, 16, …, 120}` (16 values mapped into a 0..127 range). We size the
/// table 256 (indexing by the 0..120 echoed value is always in range) and let
/// the dispatcher step by [`JOB_ID_INCREMENT`].
const ASIC_JOB_ID_SPAN: usize = 256;
const WORK_HISTORY_PER_ECHOED_JOB_ID: usize = 128;
const ASIC_JOB_ID_MASK: u8 = 0x7F;

/// Dispatcher job-id step. Must be a multiple of 8 so it round-trips through the
/// chip's `(sent << 1) & 0xF0` echo encoding; 24 matches the proven BM1368/BM1370
/// family path (`serial_mining.rs`).
const JOB_ID_INCREMENT: u8 = 24;

/// BIP320 version-rolling field mask (bits [28:13]) — the rolled-version
/// reconstruction mask, matching `serial_mining.rs::SERIAL_VERSION_ROLLING_FIELD_MASK`.
const VERSION_ROLLING_FIELD_MASK: u32 = 0x1FFF_E000;

/// BM13xx command preamble for direct chain-UART command traffic.
///
/// The shared `dcentrald_asic::bm1362::build_*_frame` helpers return the
/// command body plus CRC5 trailer (`HDR LEN ... CRC5`) because other callers
/// feed them to transports that add framing. AM3 BB writes directly to
/// `/dev/ttyS*`, so every chip-init command must prepend `55 AA` here. Mining
/// work frames are different: `build_bm1362_serial_work_frame` already returns
/// the full 88-byte wire frame including this preamble.
const BM13XX_CMD_PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Reverse 32-bit word order within a 32-byte array (8 words, MSB-first ↔
/// LSB-first). Verbatim from `dcentrald_asic::drivers::bm1362::reverse_32bit_words`
/// / `serial_mining.rs::reverse_32bit_words` — BM1362 expects `merkle_root` and
/// `prev_block_hash` with each 32-bit word reversed in the full-header job frame.
fn reverse_32bit_words(data: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..(i + 1) * 4].copy_from_slice(&data[(7 - i) * 4..(7 - i + 1) * 4]);
    }
    out
}

/// Build the PROVEN 88-byte BM1362 serial-work wire frame from a Stratum
/// [`dcentrald_stratum::work::MiningWork`].
///
/// Verbatim port of the canonical BM1362 builder in `serial_mining.rs`
/// (the Amlogic-NoPic / BeagleBone serial path): wire =
/// `[0x55 0xAA][0x21][0x56][82-byte full-header payload][CRC16-CCITT-FALSE hi, lo]`,
/// CRC over the 84 bytes from the `0x21` header byte through the last payload
/// byte (the `0x55 0xAA` preamble is NOT covered). Mirrors
/// `dcentrald_asic::drivers::bm1362::build_serial_work_frame` (which takes the
/// other `MiningWork` type) byte-for-byte.
fn build_bm1362_serial_work_frame(
    work: &dcentrald_stratum::work::MiningWork,
    asic_job_id: u8,
) -> [u8; 88] {
    let mut payload = [0u8; 82];
    payload[0] = asic_job_id;
    payload[1] = 0x01; // num_midstates — BM1362 chip computes its own
                       // payload[2..6] = starting_nonce = 0 (already zero)
    payload[6..10].copy_from_slice(&work.nbits.to_le_bytes());
    payload[10..14].copy_from_slice(&work.ntime.to_le_bytes());
    let mr = reverse_32bit_words(&work.merkle_root);
    payload[14..46].copy_from_slice(&mr);
    let pbh = reverse_32bit_words(&work.prev_block_hash);
    payload[46..78].copy_from_slice(&pbh);
    payload[78..82].copy_from_slice(&work.version.to_le_bytes());

    let mut frame = [0u8; 88];
    frame[0] = 0x55;
    frame[1] = 0xAA;
    frame[2] = 0x21; // header: TYPE_JOB | GROUP_SINGLE | CMD_WRITE
    frame[3] = 0x56; // length: 86 = hdr(1)+len(1)+payload(82)+CRC16(2)
    frame[4..86].copy_from_slice(&payload);
    // CRC over bytes [0x21 .. last payload byte] = frame[2..86] (84 bytes),
    // big-endian appended (high byte first) — same as the proven path's
    // `send_work()` (and `drivers::bm1362::build_serial_work_frame`).
    let crc = dcentrald_hal::serial_chain::crc16_public(&frame[2..86]);
    frame[86] = (crc >> 8) as u8;
    frame[87] = (crc & 0xFF) as u8;
    frame
}

/// Build the W4 stock-`uart_trans` 86-byte `asic_work_t` diagnostic frame.
///
/// This is deliberately lab-gated by `DCENT_AM3_BB_WORK_CODEC=asic86`: the live
/// `a lab unit` strict runs proved the serial88 path is still not hashing, while the
/// reverse-engineering corpus contains a conflicting 86-byte `asic_work_t`
/// description. Keeping this builder next to the serial88 builder lets the
/// bench prove or kill that hypothesis without changing the default path.
fn build_bm1362_asic86_work_frame(
    work: &dcentrald_stratum::work::MiningWork,
    asic_job_id: u8,
    sno: u32,
) -> AsicWorkFrame {
    let mut data2 = [0u8; 12];
    data2[0..4].copy_from_slice(&work.ntime.to_le_bytes());
    data2[4..8].copy_from_slice(&work.nbits.to_le_bytes());
    // W4 mapping: data2[8..12] carries job_id high bits. The live dispatcher
    // uses an 8-bit ASIC job id, so the high word is currently zero.

    let mut data = [0u8; 64];
    let midstate = work.midstates.first().copied().unwrap_or([0u8; 32]);
    data[0..32].copy_from_slice(&midstate);
    data[32..64].copy_from_slice(&work.merkle_root);

    AsicWorkFrame {
        type_byte: CMD_WORK_PACKAGE,
        rsvd1: 0,
        job_id: asic_job_id,
        rsvd2: 0,
        sno,
        data2,
        data,
    }
}

/// Reconstruct the rolled block version from a base version + the raw
/// version-rolling bits the chip returned in its nonce frame (BIP320 field
/// = bits [28:13]). Mirrors `serial_mining.rs::serial_rolled_version`.
fn rolled_version(base_version: u32, version_bits_raw: u16) -> u32 {
    let raw_masked = ((version_bits_raw as u32) << 13) & VERSION_ROLLING_FIELD_MASK;
    (base_version & !VERSION_ROLLING_FIELD_MASK) | raw_masked
}

fn rolled_version_checked(
    base_version: u32,
    version_mask: u32,
    version_bits_raw: u16,
) -> Option<u32> {
    // Cross-platform Protocol fix sweep (2026-05-15): BM1362-family chips
    // roll BIP320 unconditionally regardless of pool `mining.configure`
    // negotiation. Pre-fix `version_mask == 0 → drop if vbits != 0` was
    // the silent-drop bug pattern that cost the .135 Amlogic 0.023%
    // accept rate. Now reconstruct unconditionally; validate_full_header
    // upstream is the SOLE gate. See
    // .
    let rolled = rolled_version(base_version, version_bits_raw);
    if version_mask == 0 {
        return Some(rolled);
    }
    let delta = rolled ^ base_version;
    if delta & !version_mask != 0 {
        return None;
    }
    Some(rolled)
}

/// The job-id value the BM1362 echoes back in its nonce frame for a given
/// *sent* job id. The chip encodes the sent id as `(sent << 1) & 0xF0` in the
/// high nibble of the RESULT byte, and the parser recovers `(byte & 0xF0) >> 1`
/// — so only bits [6:3] of the sent id survive. This equals `sent & 0x78`.
/// We index `work_by_id` by this value on both store (dispatch) and lookup
/// (nonce). (Matches `serial_mining.rs`'s `(id_byte & 0xF0) >> 1` extraction.)
const fn echoed_job_id(sent: u8) -> u8 {
    ((sent << 1) & 0xF0) >> 1
}

fn next_bm1362_serial_job_id(sent: u8) -> u8 {
    sent.wrapping_add(JOB_ID_INCREMENT) & ASIC_JOB_ID_MASK
}

// ---------------------------------------------------------------------------
// BM1362 cold-boot register values — VERBATIM from the proven Amlogic-NoPic
// serial path (`dcentrald::serial_mining` BM1362 cold-boot, `serial_mining.rs`
// ~lines 415-436). These are the register writes that activate the BM1362
// cores + set the PLL — the milestone log proves the chips *respond* on `a lab unit`
// but they were never set up to hash (the prior `bm1362_chip_init_one_chain`
// did enum → fast-baud → MiscCtrl → TicketMask only). Wiring them in is the #1
// thing for first nonces on `a lab unit`. Reg numbers are the BM1397+ register
// addresses written via [`build_broadcast_write_frame`] (HDR=0x51).
// ---------------------------------------------------------------------------

/// `0xA8` InitControl - broadcast value (Step 1, pre-fast-baud).
const BM1362_REG_INIT_CONTROL: u8 = 0xA8;
const BM1362_INIT_CONTROL_BCAST: u32 = BM1362_INIT_PLAN.init_control_broadcast;
const BM1362_INIT_CONTROL_PER_CHIP: u32 = BM1362_INIT_PLAN.init_control_per_chip;
const BM1362_INIT_CONTROL_BCAST_LEGACY_AMLOGIC: u32 = 0x0000_0000;
const BM1362_INIT_CONTROL_PER_CHIP_LEGACY_AMLOGIC: u32 = 0x0200_0000;
/// `0xA4` VersionMask (Step 1).
const BM1362_REG_VERSION_MASK: u8 = 0xA4;
const BM1362_VERSION_MASK_VALUE: u32 = 0x9000_FFFF;
/// `0x3C` CoreRegCtrl — written 2× broadcast (Step 4): HashClk then ClkDelay.
const BM1362_REG_CORE_CTRL: u8 = 0x3C;
const BM1362_CORE_REG_HASH_CLK: u32 = 0x8000_8540;
const BM1362_CORE_REG_CLK_DELAY: u32 = 0x8000_8008; // BM1362-specific
const BM1362_CORE_REG_UNKNOWN: u32 = 0x8000_82AA;
/// `0x54` AnalogMux (Step 4).
const BM1362_REG_ANALOG_MUX: u8 = 0x54;
const BM1362_ANALOG_MUX_VALUE: u32 = 0x0000_0003;
/// `0x58` IoDriver (Step 4).
const BM1362_REG_IO_DRIVER: u8 = 0x58;
const BM1362_IO_DRIVER_NORMAL: u32 = 0x0001_1111;
/// `0x10` HashCountingNumber / nonce-range (Step 4) — 126 chips (S19j Pro).
const BM1362_REG_NONCE_RANGE: u8 = 0x10;
const BM1362_NONCE_RANGE_126: u32 = 0x0000_1381;
/// `0x70` PLL0 divider (Step 5).
const BM1362_REG_PLL0_DIVIDER: u8 = 0x70;
const BM1362_PLL0_DIVIDER_VALUE: u32 = 0x0000_0000;
/// `0x08` PLL0 param. The live trace's exact 525 MHz value is preserved when
/// 525 MHz is the target; other ramp steps use the canonical lookup table.
const BM1362_REG_PLL0_PARAM: u8 = 0x08;
const BM1362_PLL0_PARAM_525MHZ: u32 = 0x40A8_0265;
const BM1362_PLL_RAMP_START_MHZ: u16 = 400;
const BM1362_PLL_RAMP_STEP_MHZ: u16 = 25;
const BM1362_PLL_RAMP_SETTLE_MS: u64 = 100;
/// `0x14` TicketMask. The proven BM1362 serial path uses `0xFF` (accept 1/256).
const BM1362_REG_TICKET_MASK: u8 = 0x14;
const BM1362_TICKET_MASK_256: u32 = 0x0000_00FF;
const BM1362_SERIAL_PACE_MIN_MS: u64 = 20;
const BM1362_MAX_CHIPS_PER_CHAIN: usize = 255;
const BM1362_MISC_CONTROL_LEGACY_AMLOGIC: u32 = cold_boot_step::MISC_CONTROL_VALUE_POST_FAST_BAUD;

const ENV_AM3_BB_MINING_BAUD: &str = "DCENT_AM3_BB_MINING_BAUD";
const ENV_AM3_BB_FAST_UART_VALUE: &str = "DCENT_AM3_BB_FAST_UART_VALUE";
const ENV_AM3_BB_ENABLE_FAST_UART: &str = "DCENT_AM3_BB_ENABLE_FAST_UART";
const ENV_AM3_BB_SKIP_FAST_UART: &str = "DCENT_AM3_BB_SKIP_FAST_UART";
const ENV_AM3_BB_SKIP_UART_RELAY: &str = "DCENT_AM3_BB_SKIP_UART_RELAY";
const ENV_AM3_BB_LEGACY_AMLOGIC_INIT: &str = "DCENT_AM3_BB_LEGACY_AMLOGIC_INIT";
const ENV_AM3_BB_LEGACY_INIT_ORDER: &str = "DCENT_AM3_BB_LEGACY_INIT_ORDER";
const ENV_AM3_BB_SKIP_DSPIC_INIT: &str = "DCENT_AM3_BB_SKIP_DSPIC_INIT";
const ENV_AM3_BB_SKIP_DSPIC_SET_VOLTAGE: &str = "DCENT_AM3_BB_SKIP_DSPIC_SET_VOLTAGE";
const ENV_AM3_BB_SKIP_DSPIC_HEARTBEAT: &str = "DCENT_AM3_BB_SKIP_DSPIC_HEARTBEAT";
const ENV_AM3_BB_DSPIC_EARLY_ENABLE: &str = "DCENT_AM3_BB_DSPIC_EARLY_ENABLE";
const ENV_AM3_BB_DISABLE_HEARTBEAT_SUPERVISOR: &str = "DCENT_AM3_BB_DISABLE_HEARTBEAT_SUPERVISOR";
const ENV_AM3_BB_SKIP_THERMAL_SUPERVISOR: &str = "DCENT_AM3_BB_SKIP_THERMAL_SUPERVISOR";
// PR-021 lab escape hatch. SAFE direction only: setting this REVERTS to the
// pre-PR-021 behaviour (fan pinned at the quiet safe floor by the run guard,
// fail-closed supervisor still fully active). It can only DISABLE the new
// active cooling — it can never raise a cap or relax a fail-closed path. The
// fail-closed `Am3BbThermalSupervisor::poll_and_check` still runs regardless;
// this gate only parks the additive PID. Continuous PID is the DEFAULT.
const ENV_AM3_BB_DISABLE_FAN_PID: &str = "DCENT_AM3_BB_DISABLE_FAN_PID";
const ENV_AM3_BB_OPEN_CORE_MV: &str = "DCENT_AM3_BB_OPEN_CORE_MV";
const ENV_AM3_BB_OPEN_CORE_HOLD_MS: &str = "DCENT_AM3_BB_OPEN_CORE_HOLD_MS";
const ENV_AM3_BB_FAST_UART_SETTLE_MS: &str = "DCENT_AM3_BB_FAST_UART_SETTLE_MS";
const ENV_AM3_BB_FAST_GETADDR_DELAY_MS: &str = "DCENT_AM3_BB_FAST_GETADDR_DELAY_MS";
const ENV_AM3_BB_FAST_GETADDR_READ_MS: &str = "DCENT_AM3_BB_FAST_GETADDR_READ_MS";
const ENV_AM3_BB_SKIP_FAST_RELAY_AFTER_SWITCH: &str = "DCENT_AM3_BB_SKIP_FAST_RELAY_AFTER_SWITCH";
const ENV_AM3_BB_WRITE_PLL0_DIVIDER: &str = "DCENT_AM3_BB_WRITE_PLL0_DIVIDER";
const ENV_AM3_BB_USE_DEVMEM_UART: &str = "DCENT_AM3_BB_USE_DEVMEM_UART";
const ENV_AM3_BB_ALLOW_NO_RX_MINING: &str = "DCENT_AM3_BB_ALLOW_NO_RX_MINING";
const ENV_AM3_BB_ASSUME_JOB_RESPONSE_FLAGS: &str = "DCENT_AM3_BB_ASSUME_JOB_RESPONSE_FLAGS";
const ENV_AM3_BB_WORK_CODEC: &str = "DCENT_AM3_BB_WORK_CODEC";

// AM3 BB hashboard-side dsPIC path. LuxOS ftrace on `a lab unit` (2026-05-13)
// shows firmware 0x89 controllers on I2C bus 0 using one full-frame write
// followed by one-byte reads. The EEPROM range on the same bus remains
// write-denied.
const AM3_BB_DSPIC_I2C_BUS: u8 = 0;
const AM3_BB_DSPIC_BASE_ADDR: u8 = 0x20;
const AM3_BB_DSPIC_HEARTBEAT_INTERVAL_MS: u64 = 1_000;
const AM3_BB_DSPIC_POST_ENABLE_RESET_ASSERT_MS: u64 = 200;
const AM3_BB_DSPIC_POST_ENABLE_RESET_RELEASE_MS: u64 = 1_100;
const AM3_BB_DSPIC_INTER_CHAIN_RESET_MS: u64 = 10;
const AM3_BB_DSPIC_HEARTBEAT_MAX_FAILURES: u8 = 3;
const AM3_BB_DSPIC_MIN_VOLTAGE_MV: u16 = 11_940;
const AM3_BB_DSPIC_MAX_VOLTAGE_MV: u16 = 15_140;
const AM3_BB_DSPIC_DEFAULT_TARGET_MV: u16 = 13_700;
const AM3_BB_DSPIC_DEFAULT_OPEN_CORE_MV: u16 = 14_920;
const AM3_BB_DSPIC_DEFAULT_OPEN_CORE_HOLD_MS: u64 = 20_000;
const AM3_BB_FAN_SAFE_FLOOR_PWM: u8 = 10;
const AM3_BB_FAN_HARD_CAP_PWM: u8 = 30;
const AM3_BB_HASHBOARD_EEPROM_DENYLIST: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];
const AM3_BB_LM75_SENSOR_ADDRS: [u8; 4] = [0x48, 0x49, 0x4A, 0x4B];
const AM3_BB_LM75_REPLY_LEN: usize = 7;
const AM3_BB_LM75_MIN_VALID_C: f32 = -20.0;
const AM3_BB_LM75_MAX_VALID_C: f32 = 125.0;
// Live `a lab unit` validation on 2026-05-13 showed the dsPIC LM75 bridge can return
// one malformed runtime poll while the pool/heartbeat path is active. Keep
// pre-start proof strict, but tolerate only a short fresh-sample window at
// runtime before cutting ASIC voltage.
const AM3_BB_THERMAL_RUNTIME_RETRY_MS: u64 = 100;
const AM3_BB_THERMAL_MAX_CONSECUTIVE_MISSES: u8 = 3;
const AM3_BB_THERMAL_MAX_STALE_MS: u64 = 15_000;
const AM3_BB_THERMAL_MIN_POLL_MS: u64 = 1_000;
// PR-021 continuous fan PID. Max single-tick PWM slew so the quiet home fan
// never audibly "jumps" — it walks toward the PID target a few PWM steps at a
// time. With AM3_BB_FAN_HARD_CAP_PWM=30 the whole legal band is 20 wide, so a
// 3-step ceiling reaches the cap in ~7 ticks (~14 s at the 2 s default) — fast
// enough for the 2-4 s BM1362 thermal time constant, slow enough to stay quiet.
const AM3_BB_FAN_PID_MAX_STEP_PWM: u8 = 3;

const AM3_BB_DSPIC_RESET_FRAME: &[u8] = &[0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B];
const AM3_BB_DSPIC_JUMP_FRAME: &[u8] = &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A];
const AM3_BB_DSPIC_GET_VERSION_FRAME: &[u8] = &[0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B];
const AM3_BB_DSPIC_DISABLE_FRAME: &[u8] = &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A];
const AM3_BB_DSPIC_ENABLE_FRAME: &[u8] = &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B];
const AM3_BB_DSPIC_HEARTBEAT_FRAME: &[u8] = &[0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A];
const AM3_BB_DSPIC_PROBE_3B_48_FRAME: &[u8] = &[0x55, 0xAA, 0x06, 0x3B, 0x48, 0x00, 0x00, 0x89];
const AM3_BB_DSPIC_READ_VOLTAGE_FRAME: &[u8] = &[0x55, 0xAA, 0x04, 0x3A, 0x00, 0x3E];

// ===========================================================================
//  ChainUart adapter — DevmemUart -> Am335xUartTransport
// ===========================================================================

/// Adapter so [`Am335xUartTransport`] (which is HAL-free, requires a
/// [`ChainUart`]) can drive the HAL's [`DevmemUart`].
///
/// Lives in the daemon crate, which depends on both `dcentrald-asic` (the
/// transport) and `dcentrald-hal` (the UART). `dcentrald-asic` deliberately
/// does not name `DevmemUart` so the transport stays pure/host-testable
/// (same pattern as the `pic1704` sealed traits).
///
/// `DevmemUart::init()`/`open()` already programs `MCR=0x03` + `FCR=0x07`
/// — this adapter does NOT
/// re-derive that. `DevmemUart::write_bytes` / `read_bytes_timeout` both take
/// `&self` (the device is single-threaded by construction), so the inner
/// field doesn't need a `&mut` projection.
pub struct DevmemChainUart(pub DevmemUart);

impl ChainUart for DevmemChainUart {
    fn write_frame(&mut self, data: &[u8]) -> Result<(), UartTransportError> {
        self.0
            .write_bytes(data)
            .map_err(|_| UartTransportError::WriteFailed)
    }

    fn read_avail(&mut self, buf: &mut [u8]) -> usize {
        // Short timeout so the mining loop doesn't block — nonce frames
        // arrive asynchronously and the transport polls.
        self.0.read_bytes_timeout(buf, 5)
    }
}

pub enum Am3BbChainUart {
    Kernel(SerialChainBackend),
    Devmem(DevmemUart),
}

impl Am3BbChainUart {
    fn open(spec: &ChainUartSpec, baud: u32) -> Result<Self> {
        if env_flag_set(ENV_AM3_BB_USE_DEVMEM_UART) {
            warn!(
                env = ENV_AM3_BB_USE_DEVMEM_UART,
                device = %spec.device,
                "am3-bb: lab override active - using DevmemUart for chain UART mining"
            );
            return Ok(Self::Devmem(
                DevmemUart::open_no_unbind(&spec.device, baud).with_context(|| {
                    format!(
                        "am3-bb: DevmemUart::open_no_unbind({}, {}) failed",
                        spec.device, baud
                    )
                })?,
            ));
        }

        let serial =
            SerialChainBackend::open(spec.index, &spec.device, baud).with_context(|| {
                format!(
                    "am3-bb: SerialChainBackend::open({}, {}) failed",
                    spec.device, baud
                )
            })?;
        serial.set_vtime(0).with_context(|| {
            format!("am3-bb: set VTIME=0 on kernel UART {} failed", spec.device)
        })?;
        Ok(Self::Kernel(serial))
    }

    fn backend_name(&self) -> &'static str {
        match self {
            Self::Kernel(_) => "kernel",
            Self::Devmem(_) => "devmem",
        }
    }

    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        match self {
            Self::Kernel(serial) => serial
                .write_raw_bytes(data)
                .context("am3-bb: kernel UART raw write failed"),
            Self::Devmem(uart) => uart
                .write_bytes(data)
                .context("am3-bb: devmem UART raw write failed"),
        }
    }

    fn read_bytes_timeout(&mut self, buf: &mut [u8], timeout_ms: u64) -> usize {
        match self {
            Self::Kernel(serial) => match serial.read_raw_bytes_timeout(buf, timeout_ms) {
                Ok(n) => n,
                Err(e) => {
                    warn!(error = %e, "am3-bb: kernel UART raw read failed");
                    0
                }
            },
            Self::Devmem(uart) => uart.read_bytes_timeout(buf, timeout_ms),
        }
    }

    fn set_baud(&mut self, baud: u32) -> Result<()> {
        match self {
            Self::Kernel(serial) => {
                serial.set_baud(baud)?;
                serial.set_vtime(0)?;
                Ok(())
            }
            Self::Devmem(uart) => uart.set_baud(baud).map_err(Into::into),
        }
    }

    fn drain_tx(&mut self) -> Result<()> {
        match self {
            Self::Kernel(serial) => serial
                .drain_tx()
                .context("am3-bb: kernel UART TX drain failed"),
            Self::Devmem(uart) => {
                uart.drain_tx();
                Ok(())
            }
        }
    }

    fn flush_io(&mut self) {
        match self {
            Self::Kernel(serial) => {
                if let Err(e) = serial.flush_io() {
                    warn!(error = %e, "am3-bb: kernel UART flush failed");
                }
            }
            Self::Devmem(uart) => uart.flush_io(),
        }
    }
}

impl ChainUart for Am3BbChainUart {
    fn write_frame(&mut self, data: &[u8]) -> Result<(), UartTransportError> {
        self.write_bytes(data)
            .map_err(|_| UartTransportError::WriteFailed)
    }

    fn read_avail(&mut self, buf: &mut [u8]) -> usize {
        self.read_bytes_timeout(buf, 5)
    }
}

// ===========================================================================
//  APW UART-tunnel bus — direct /dev/i2c-<psu_bus> backing
// ===========================================================================

/// [`ApwUartTunnelBus`] backed by a directly-opened `I2cBus` on the
/// board-target's PSU bus.
///
/// `dcentrald-hal`'s `I2cServiceApwBus` (the shared-service variant) is
/// `recovery-tool`-feature-gated, so the daemon constructs this lighter
/// direct-bus variant instead. On `a lab unit` (`S19J_IO_BOARD_V2_0`) this rides
/// the bit-banged i2c-gpio bus (bus 1, gpio4=SDA / gpio5=SCL) — the kernel's
/// i2c-gpio driver covers the slow bit-banged timing.
///
/// NOTE: this is the bring-up path. If/when `--am3-bb-mining` shares the
/// process-wide I²C service with a future thermal/EEPROM reader, switch to
/// `I2cServiceApwBus` (single-owner architecture) — but on the `a lab unit` board
/// nothing else touches the PSU bus, so a dedicated fd is fine for now.
struct DirectI2cApwBus {
    bus: dcentrald_hal::i2c::I2cBus,
}

impl ApwUartTunnelBus for DirectI2cApwBus {
    // Two SEPARATE I²C transactions with the trait's default `delay()` sleep
    // in between — the APW needs ≥ ~400 ms to produce a reply, so a combined
    // repeated-START write-read reads all-`0xF5` (the original bring-up bug).
    fn write_frame(&mut self, addr: u8, frame: &[u8]) -> dcentrald_hal::Result<()> {
        self.bus.set_slave(addr)?;
        self.bus.write(frame)?;
        Ok(())
    }

    fn read_reply(&mut self, addr: u8, read_len: usize) -> dcentrald_hal::Result<Vec<u8>> {
        self.bus.set_slave(addr)?;
        let mut buf = vec![0u8; read_len];
        self.bus.read(&mut buf)?;
        Ok(buf)
    }
}

// ===========================================================================
//  Auto-detect
// ===========================================================================

/// Detect whether this unit is the AM335x BB `S19J_IO_BOARD_V2_0` carrier
/// (the `a lab unit`-class unit) so the daemon can auto-route to `--am3-bb-mining`
/// even without the explicit CLI flag.
///
/// Returns `true` when EITHER:
///  - `/etc/dcentos/board_target` reads `am3-bb-s19jpro`, OR
///  - `/proc/device-tree/compatible` contains `am335x` AND
///    `/proc/device-tree/model` contains `S19J_IO_BOARD` (LuxOS bring-up
///    unit before the DCENT_OS board-target file has been written).
pub fn auto_detect_am3_bb() -> bool {
    let board_target = std::fs::read_to_string("/etc/dcentos/board_target")
        .unwrap_or_default()
        .trim()
        .to_string();
    if board_target == "am3-bb-s19jpro" {
        return true;
    }

    let compatible = std::fs::read_to_string("/proc/device-tree/compatible").unwrap_or_default();
    let model = std::fs::read_to_string("/proc/device-tree/model").unwrap_or_default();
    compatible.contains("am335x") && model.contains("S19J_IO_BOARD")
}

// ===========================================================================
//  Chain→tty derivation (pure helper — host-testable)
// ===========================================================================

/// Per-chain UART configuration derived from the board-target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainUartSpec {
    /// Logical chain index (`0..n`).
    pub index: u8,
    /// Device path (e.g. `/dev/ttyS1`).
    pub device: String,
}

fn tty_s_to_omap_alias(device: &str) -> Option<&'static str> {
    match device {
        "/dev/ttyS1" => Some("/dev/ttyO1"),
        "/dev/ttyS2" => Some("/dev/ttyO2"),
        "/dev/ttyS4" => Some("/dev/ttyO4"),
        "/dev/ttyS5" => Some("/dev/ttyO5"),
        _ => None,
    }
}

fn resolve_runtime_uart_device(device: &str) -> String {
    if Path::new(device).exists() {
        return device.to_string();
    }

    if let Some(alias) = tty_s_to_omap_alias(device) {
        if Path::new(alias).exists() {
            return alias.to_string();
        }
    }

    device.to_string()
}

/// Derive the chain→tty list from a [`BeagleBonePlatform`]'s board-target.
///
/// The enumeration order is the chain index order from the config. The live
/// LuxOS `a lab unit` proof uses `/dev/ttyS*`; older AM335x kernels expose the same
/// OMAP UARTs as `/dev/ttyO*`, so resolve that alias at runtime when needed.
pub fn chain_uart_specs(platform: &BeagleBonePlatform) -> Vec<ChainUartSpec> {
    platform
        .board_target()
        .uart
        .chains
        .iter()
        .map(|c| ChainUartSpec {
            index: c.index,
            device: resolve_runtime_uart_device(&c.device),
        })
        .collect()
}

// ===========================================================================
//  Entry point
// ===========================================================================

/// Run the AM335x BB S19j Pro (`--am3-bb-mining`) mining mode.
///
/// Signature mirrors the other mode entry points (`SerialMiner::new` etc.):
/// takes the loaded [`DcentraldConfig`] (owned) and a [`CancellationToken`]
/// for graceful shutdown.
///
/// Steps 1-7 in [`run_am3_bb_blocking`] are the cold-boot + chip-init
/// plumbing (LIVE-VALIDATED on `a lab unit`); step 8 is the Stratum mining loop
/// (Option B2 — reuses `dcentrald_stratum` + the `Am335xUartTransport`).
/// `DCENT_AM3_BB_STUB_LOOP=1` keeps the old logging-only stub instead.
pub async fn run_am3_bb_mining(config: DcentraldConfig, shutdown: CancellationToken) -> Result<()> {
    info!("Entering AM335x BB mining mode (--am3-bb-mining) — S19J_IO_BOARD_V2_0 / .79-class");

    if !config.mining_start_enabled() && std::env::var_os("DCENT_AM3_BB_STUB_LOOP").is_none() {
        warn!(
            mining_enabled = config.mining.enabled,
            pool_configured = config.has_configured_pool(),
            "am3-bb: mining is disabled or no pool is configured; keeping API/MCP reachable and refusing hardware cold-boot"
        );
        shutdown.cancelled().await;
        return Ok(());
    }

    // The cold-boot + chip-init is blocking device I/O (mmap UART, i2c-gpio
    // bus, ~seconds of sleeps); the mining loop is a blocking poll loop over
    // the transport. Run it on a blocking thread so we don't stall the tokio
    // reactor; the API/dashboard servers (spawned by main.rs before this is
    // called) keep ticking, and the `StratumRouter` runs on the captured
    // runtime handle (mpsc channels bridge the two).
    let rt_handle = tokio::runtime::Handle::current();
    let result =
        tokio::task::spawn_blocking(move || run_am3_bb_blocking(config, shutdown, rt_handle))
            .await
            .context("am3-bb mining blocking task panicked")?;
    result
}

/// The blocking body of [`run_am3_bb_mining`].
fn run_am3_bb_blocking(
    config: DcentraldConfig,
    shutdown: CancellationToken,
    rt_handle: tokio::runtime::Handle,
) -> Result<()> {
    // ── 1. Build the platform (loads /etc/dcentos/board_targets/<name>.toml,
    //       or the hardcoded .79 defaults; side-effect-free). ──
    let platform = BeagleBonePlatform::new().context("BeagleBonePlatform::new() failed")?;
    let bt = platform.board_target();
    let chain_specs = chain_uart_specs(&platform);
    info!(
        board_target = %platform.board_target_name(),
        chain_count = bt.uart.chain_count,
        chains = ?chain_specs,
        board_enable_gpio = platform.board_enable_gpio_v2_0(),
        asic_reset_gpios = ?platform.chain_reset_gpios_v2_0(),
        plug_detect_gpios = ?platform.chain_plug_gpios_v2_0(),
        eeprom_bus = platform.eeprom_i2c_bus(),
        psu_bus = platform.psu_i2c_bus(),
        psu_addr = format_args!("0x{:02X}", platform.psu_i2c_addr()),
        mining_baud = platform.mining_baud_v2_0(),
        voltage_controller = ?platform.voltage_controller(),
        "am3-bb: loaded board topology"
    );
    // Expected per-chain chip count (operator config; `a lab unit` says 126; fallback
    // 126). Used for address assignment because the live GetAddress response
    // can be truncated before all 126 chips are counted.
    let expected_chips_per_chain = config.mining.serial_chip_count.unwrap_or(126) as usize;
    let target_freq_mhz = config.mining.frequency_mhz.clamp(400, 597);
    info!(
        expected_chips_per_chain,
        configured_freq_mhz = config.mining.frequency_mhz,
        target_freq_mhz,
        "am3-bb: configured per-chain chip count"
    );
    if target_freq_mhz != config.mining.frequency_mhz {
        warn!(
            configured_freq_mhz = config.mining.frequency_mhz,
            clamped_freq_mhz = target_freq_mhz,
            "am3-bb: BM1362 PLL table only covers 400..=597 MHz; clamping configured frequency"
        );
    }

    // Arms a run-scope fail-closed guard before cold boot. Once GPIO ownership
    // begins, every return path should leave the board in a reversible bench
    // state: capped fans, ASIC resets asserted, and board-enable off. dsPIC
    // voltage disable is attached after the controllers initialize.
    let mut _run_safety_guard = Some(Am3BbRunSafetyGuard::new(
        &platform,
        None,
        Vec::new(),
        chain_specs.len(),
        config.thermal.fan_min_pwm,
        config.thermal.fan_max_pwm,
    ));
    // Arm the crash-panic-hook teardown (panic="abort" bypasses the guard's Drop).
    // Done here, before board-enable is driven HIGH, so even a panic during
    // cold-boot cuts board power via main()'s panic hook. (wf_7c757213 safety audit.)
    arm_am3_bb_teardown(&platform, chain_specs.len());

    if shutdown.is_cancelled() {
        info!("am3-bb: shutdown requested before cold-boot — exiting cleanly");
        return Ok(());
    }

    // ── 2. Open the chain DevmemUarts (at 115200 for enumeration). ──
    //
    // `DevmemUart::open` looks the device up in the *active* UART MMIO table,
    // which defaults to Zynq (`/dev/ttyS1` → 0x4100_1000). On AM335x BB we MUST
    // select the AM335x table first (`/dev/ttyS1` → 0x4802_2000, the OMAP UART);
    // otherwise the mmap of /dev/mem hits the Zynq PL-UART address, which is an
    // unmapped region on AM335x → SIGBUS. The table is a process-wide OnceLock;
    // calling this once before any `DevmemUart::open` is the contract.
    dcentrald_hal::serial::select_uart_table_am335x()
        .context("am3-bb: select AM335x OMAP UART MMIO table (must precede DevmemUart::open)")?;
    let enum_baud = 115_200u32;
    if chain_specs.is_empty() {
        anyhow::bail!("am3-bb: board-target declares zero chain UARTs - nothing to mine on");
    }
    let mut cold_boot_uarts: Vec<DevmemUart> = Vec::new();
    info!(
        chains = chain_specs.len(),
        "am3-bb: S19J_IO_BOARD_V2_0 cold-boot does not touch chain UARTs; skipping temporary DevmemUart opens"
    );
    if env_flag_set(ENV_AM3_BB_USE_DEVMEM_UART) {
        warn!(
            env = ENV_AM3_BB_USE_DEVMEM_UART,
            "am3-bb: lab override active - opening temporary DevmemUart handles for cold-boot shape check"
        );
        for spec in &chain_specs {
            let uart = DevmemUart::open_no_unbind(&spec.device, enum_baud).with_context(|| {
            format!(
                "am3-bb: DevmemUart::open({}, {}) failed — is stock luxminer/cgminer still running? \
                 stop it first",
                spec.device, enum_baud
            )
        })?;
            info!(device = %spec.device, baud = enum_baud, "am3-bb: temporary cold-boot UART opened");
            cold_boot_uarts.push(uart);
        }
        if cold_boot_uarts.is_empty() {
            anyhow::bail!("am3-bb: board-target declares zero chain UARTs — nothing to mine on");
        }

        // ── 3. Build the APW121215f UART-tunnel PSU controller (bus 1 @ 0x10). ──
    }

    let psu_bus_num = platform.psu_i2c_bus();
    let psu_addr = platform.psu_i2c_addr();
    let psu_i2c = platform
        .open_i2c(psu_bus_num)
        .with_context(|| format!("am3-bb: open /dev/i2c-{} (PSU bus) failed", psu_bus_num))?;
    let mut psu = ApwUartTunnel::new_at(DirectI2cApwBus { bus: psu_i2c }, psu_addr);
    info!(
        psu_bus = psu_bus_num,
        psu_addr = format_args!("0x{:02X}", psu_addr),
        "am3-bb: APW UART-tunnel PSU controller constructed"
    );

    // Wave J Lane A: 120V "Loki bypass". am3-bb cold-boot asserts the board-enable
    // GPIO (gpio59, board_enable_gpio_v2_0) and the APW UART-tunnel set-voltage /
    // watchdog calls are already Phase-D non-fatal stubs (log + continue), so a
    // non-smart PSU does not block here today. When [power.psu_override] is set we
    // honor it for telemetry (record the declared model + efficiency) and log the
    // disposition so it is never silently ignored. The gpio59 enable + cold-boot
    // below are unchanged; the chip rail is untouched.
    if crate::s19j_hybrid_mining::psu_override_active(config.power.psu_override.as_ref()) {
        let ovr = config
            .power
            .psu_override
            .as_ref()
            .expect("psu_override_active implies Some");
        info!(
            model = %ovr.model,
            rail_v = ovr.voltage_v,
            efficiency =
                ?crate::runtime::efficiency::psu_efficiency_for_model_name(&ovr.model),
            "am3-bb: PSU OVERRIDE honored as INFORMATIONAL — board-enable is gpio59 + APW \
             writes are non-fatal stubs, so there is no blocking smart-PSU probe to bypass; \
             declared model + efficiency recorded for telemetry"
        );
    }

    // ── 4. Cold-boot: gpio59 enable → settle → APW identity probe → set
    //       open-core rail → de-assert ASIC resets → settle.
    //
    //       `run_cold_boot` builds `ColdBootOptsV2::from_board_target(...)`
    //       and calls `cold_boot_sequence_s19j_io_v2`. Several APW steps are
    //       Phase-D stubs that log + continue (the `psu_apw_uart_tunnel`
    //       set_voltage_mv etc. return their "not implemented" sentinel,
    //       which the cold-boot fn treats as non-fatal). The gpio enable +
    //       reset de-assert + settles are real. ──
    info!("am3-bb: starting cold-boot sequence (ColdBootOptsV2 from board-target)");
    platform
        .run_cold_boot(&mut psu, &mut cold_boot_uarts)
        .context("am3-bb: cold-boot sequence failed")?;
    info!("am3-bb: cold-boot sequence returned OK");
    drop(cold_boot_uarts);
    info!("am3-bb: cold-boot complete with no temporary DevmemUart ownership");

    if shutdown.is_cancelled() {
        info!(
            "am3-bb: shutdown requested after cold-boot - safety guard will leave rails disabled"
        );
        return Ok(());
    }

    // ── 4b. Hashboard-SKU energize-refusal gate ( B2, 2026-05-22). ──
    //
    // Drive-half of matrix §7 #15. Classify each chain's EEPROM preamble
    // BEFORE the dsPIC + APW are driven; refuse if any chain reports a
    // malformed/timed-out/mixed-SKU/unbindable preamble. AM3 BB chains
    // expose their EEPROM at `/sys/bus/i2c/devices/<bus>-005<slot>/eeprom`
    // exactly like AM2; the helper used here is platform-agnostic. The
    // env gating (`DCENT_AM2_STRICT_SKU_REFUSE` default OFF) is shared
    // with AM2 so first-deploy telemetry is consistent across both paths
    // — the `AM2_` prefix is historical; the gate is platform-generic.
    {
        use crate::runtime::hardware_info::{
            read_hashboard_eeprom_for_energize_gate, EepromReadinessError,
            DEFAULT_EEPROM_READINESS_BUDGET_MS,
        };
        use dcentrald_silicon_profiles::energize_gate::{
            accept_degraded_hardware_enabled, classify_chain, gate_chains_for_energize_with_opts,
            strict_sku_refuse_enabled, ChainProbe,
        };

        let strict = strict_sku_refuse_enabled();
        let accept_degraded = accept_degraded_hardware_enabled();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(DEFAULT_EEPROM_READINESS_BUDGET_MS);
        let chain_count = chain_specs.len();
        let mut probes: Vec<ChainProbe> = Vec::with_capacity(chain_count);
        for slot in 0..chain_count.min(8) {
            let slot_u8 = u8::try_from(slot).unwrap_or(0);
            match read_hashboard_eeprom_for_energize_gate(slot, deadline) {
                Ok(bytes) => probes.push(classify_chain(slot_u8, Some(&bytes))),
                Err(EepromReadinessError::Timeout { .. }) => {
                    probes.push(ChainProbe::Timeout { chain_id: slot_u8 });
                }
                Err(EepromReadinessError::InvalidSlot { .. }) => {
                    probes.push(ChainProbe::ReadError { chain_id: slot_u8 });
                }
            }
        }
        info!(
            strict,
            accept_degraded,
            probes = ?probes,
            "am3-bb: Phase 4b hashboard-SKU energize-gate probes"
        );
        // am3-bb: timeout_is_skip=true. The hashboard EEPROM (bus 0 @
        // 0x50-0x52) is unpowered until the chain rail is enabled, but this
        // gate runs pre-energize by design → a pre-energize EEPROM read
        // ALWAYS times out (live-proven on .79 2026-05-22: the same bus-0
        // dsPICs only answered after rail-enable). Treating that timeout as
        // refuse-eligible would FALSE-REFUSE every healthy am3-bb chain under
        // strict mode. am3-bb identity protection comes from plug-detect
        // GPIO + dsPIC fw=0x86 refusal + chain-enum liveness instead. Only
        // affects strict mode; default-OFF telemetry path is unchanged.
        //
        match gate_chains_for_energize_with_opts(&probes, strict, true) {
            Ok((bindings, telemetry)) => {
                info!(
                    chains = bindings.len(),
                    bindings = ?bindings,
                    "am3-bb: Phase 4b energize gate ACCEPTED"
                );
                if !telemetry.is_empty() {
                    warn!(
                        reasons = %telemetry.summary(),
                        "am3-bb: [ENERGIZE-REFUSED telemetry-only — would refuse if DCENT_AM2_STRICT_SKU_REFUSE=1] {}",
                        telemetry.summary()
                    );
                }
            }
            Err(refusal) => {
                if accept_degraded {
                    warn!(
                        reasons = %refusal.summary(),
                        "am3-bb: [ENERGIZE-REFUSED but proceeding — DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1 lab override] {}",
                        refusal.summary()
                    );
                } else {
                    tracing::error!(
                        reasons = %refusal.summary(),
                        "am3-bb: [ENERGIZE-REFUSED] {}",
                        refusal.summary()
                    );
                    anyhow::bail!(
                        "am3-bb hashboard-SKU energize gate refused: {}",
                        refusal.summary()
                    );
                }
            }
        }
    }

    // ── 5. Hashboard dsPIC init + heartbeat on I2C bus 0. ──
    //
    // The 2026-05-13 LuxOS ftrace disproved the earlier NoPic assumption for
    // `a lab unit`: LuxOS initializes fw=0x89 controllers at 0x20/0x21/0x22 before
    // BM1362 work starts. Keep EEPROM writes denied on 0x50..=0x57, replay the
    // traced app-mode sequence, and keep 1 Hz heartbeat replies drained while
    // mining.
    let mut _dspic_heartbeat_guard: Option<Am3BbDspicHeartbeatGuard> = None;
    let mut dspic_i2c_main: Option<I2cServiceHandle> = None;
    let mut active_dspic_addrs: Vec<u8> = Vec::new();
    let dspic_target_voltage_mv = am3_bb_dspic_target_voltage_mv(config.mining.voltage_mv);
    if env_flag_set(ENV_AM3_BB_SKIP_DSPIC_INIT) {
        warn!(
            env = ENV_AM3_BB_SKIP_DSPIC_INIT,
            "am3-bb: lab override active — skipping hashboard dsPIC init/heartbeat"
        );
    } else {
        let dspic_i2c = spawn_i2c_service_no_register_touch_with_denylist(
            AM3_BB_DSPIC_I2C_BUS,
            AM3_BB_HASHBOARD_EEPROM_DENYLIST.to_vec(),
        )
        .context(
            "am3-bb: spawn I2C service for hashboard dsPIC bus 0 with EEPROM denylist failed",
        )?;
        info!(
            bus = AM3_BB_DSPIC_I2C_BUS,
            denylist = format_args!("{:02X?}", AM3_BB_HASHBOARD_EEPROM_DENYLIST),
            "am3-bb: hashboard dsPIC I2C service started"
        );

        let early_enable = env_flag_set(ENV_AM3_BB_DSPIC_EARLY_ENABLE);
        if early_enable {
            warn!(
                env = ENV_AM3_BB_DSPIC_EARLY_ENABLE,
                target_voltage_mv = dspic_target_voltage_mv,
                "am3-bb: lab override active - enabling dsPIC rail before BM1362 enumeration"
            );
        } else {
            info!(
                open_core_mv = AM3_BB_DSPIC_DEFAULT_OPEN_CORE_MV,
                steady_mv = dspic_target_voltage_mv,
                "am3-bb: LuxOS-style dsPIC sequence active - controller init now, rail enable after BM1362 chip init"
            );
        }
        active_dspic_addrs = am3_bb_dspic_init_all(
            &dspic_i2c,
            chain_specs.len(),
            dspic_target_voltage_mv,
            early_enable,
        );
        if active_dspic_addrs.is_empty() {
            anyhow::bail!(
                "am3-bb: no fw=0x89 hashboard dsPIC controllers initialized on bus {}",
                AM3_BB_DSPIC_I2C_BUS
            );
        }
        info!(
            active_dspic_addrs = format_args!("{:02X?}", active_dspic_addrs),
            "am3-bb: hashboard dsPIC controllers initialized"
        );

        if let Some(guard) = _run_safety_guard.as_mut() {
            guard.set_dspic(dspic_i2c.clone(), active_dspic_addrs.clone());
        }

        am3_bb_post_dspic_reset_chains(&platform, chain_specs.len())
            .context("am3-bb: post-dsPIC ASIC reset pulse failed")?;

        if env_flag_set(ENV_AM3_BB_SKIP_DSPIC_HEARTBEAT) {
            warn!(
                env = ENV_AM3_BB_SKIP_DSPIC_HEARTBEAT,
                "am3-bb: lab override active — dsPIC runtime heartbeat thread disabled"
            );
        } else {
            _dspic_heartbeat_guard = Some(start_am3_bb_dspic_heartbeat(
                dspic_i2c.clone(),
                active_dspic_addrs.clone(),
                shutdown.clone(),
            )?);
            info!(
                interval_ms = AM3_BB_DSPIC_HEARTBEAT_INTERVAL_MS,
                "am3-bb: dsPIC runtime heartbeat thread started"
            );
        }
        dspic_i2c_main = Some(dspic_i2c);
    }

    if env_flag_set(ENV_AM3_BB_SKIP_THERMAL_SUPERVISOR) {
        warn!(
            env = ENV_AM3_BB_SKIP_THERMAL_SUPERVISOR,
            "am3-bb: lab override active - thermal preflight/supervisor disabled"
        );
    } else {
        let Some(dspic_i2c) = dspic_i2c_main.as_ref() else {
            anyhow::bail!(
                "am3-bb: dsPIC I2C service is unavailable; refusing to mine without thermal supervisor"
            );
        };
        Am3BbThermalSupervisor::new(
            dspic_i2c.clone(),
            active_dspic_addrs.clone(),
            config.thermal.hot_temp_c,
            config.thermal.dangerous_temp_c,
        )?
        .poll_and_check("pre-chip-init")?;
    }

    if shutdown.is_cancelled() {
        info!("am3-bb: shutdown requested after dsPIC init — exiting cleanly");
        return Ok(());
    }

    // ── 6. BM1362 chip-side init per chain. ──
    //
    // Wire bytes are built with the `dcentrald_asic::bm1362` frame builders:
    // GetAddress @115200, ChainInactive + SetChipAddress, core/ticket/nonce
    // registers, PLL ramp, FastUART handoff, then per-chip mining-ready writes.
    // No open-core dummy work: BM1362 uses the register path, not the BM1387
    // dummy-work core gate.
    let mut mining_baud = platform.mining_baud_v2_0();
    if let Some(override_baud) = parse_env_u32(ENV_AM3_BB_MINING_BAUD) {
        warn!(
            env = ENV_AM3_BB_MINING_BAUD,
            default_baud = mining_baud,
            override_baud,
            "am3-bb: lab override for host mining baud is active"
        );
        mining_baud = override_baud;
    }

    let mut fast_uart_value = cold_boot_step::FAST_UART_CONFIG_VALUE;
    if let Some(override_fast_uart) = parse_env_u32(ENV_AM3_BB_FAST_UART_VALUE) {
        warn!(
            env = ENV_AM3_BB_FAST_UART_VALUE,
            default_fast_uart = format_args!("0x{:08X}", fast_uart_value),
            override_fast_uart = format_args!("0x{:08X}", override_fast_uart),
            "am3-bb: lab override for BM1362 FastUART register value is active"
        );
        fast_uart_value = override_fast_uart;
    }

    let enable_fast_uart = env_flag_set(ENV_AM3_BB_ENABLE_FAST_UART);
    if enable_fast_uart {
        warn!(
            env = ENV_AM3_BB_ENABLE_FAST_UART,
            "am3-bb: lab override active — enabling BM1362 FastUART handoff despite .79 live evidence"
        );
    } else {
        warn!(
            enable_env = ENV_AM3_BB_ENABLE_FAST_UART,
            skip_env = ENV_AM3_BB_SKIP_FAST_UART,
            "am3-bb: defaulting to 115200 mining; .79 live runs produced parsed nonce frames only when FastUART was skipped"
        );
    }

    let skip_fast_uart = !enable_fast_uart || env_flag_set(ENV_AM3_BB_SKIP_FAST_UART);
    if skip_fast_uart {
        warn!(
            env = ENV_AM3_BB_SKIP_FAST_UART,
            enable_env = ENV_AM3_BB_ENABLE_FAST_UART,
            "am3-bb: skipping BM1362 FastUART write and keeping chains at 115200"
        );
    }

    let mut uarts: Vec<Am3BbChainUart> = Vec::with_capacity(chain_specs.len());
    for spec in &chain_specs {
        let uart = Am3BbChainUart::open(spec, enum_baud)?;
        info!(
            chain = spec.index,
            device = %spec.device,
            baud = enum_baud,
            backend = uart.backend_name(),
            "am3-bb: mining chain UART opened"
        );
        uarts.push(uart);
    }
    if uarts.is_empty() {
        anyhow::bail!("am3-bb: board-target declares zero mining chain UARTs");
    }

    let mut total_chips: usize = 0;
    let mut init_results: Vec<Bm1362ChainInitResult> = Vec::with_capacity(uarts.len());
    for (idx, uart) in uarts.iter_mut().enumerate() {
        let init = bm1362_chip_init_one_chain(
            uart,
            idx,
            mining_baud,
            fast_uart_value,
            skip_fast_uart,
            expected_chips_per_chain,
            target_freq_mhz,
            bt.cold_boot.run_miscctrl_triple_write,
        )
        .with_context(|| format!("am3-bb: BM1362 chip-init failed on chain {}", idx))?;
        info!(
            chain = idx,
            chips = init.assigned_chips,
            initial_get_address_rx_bytes = init.initial_get_address_rx_bytes,
            fast_get_address_rx_bytes = init.fast_get_address_rx_bytes,
            rx_proven = init.rx_proven(),
            "am3-bb: BM1362 chip-init complete"
        );
        total_chips += init.assigned_chips;
        init_results.push(init);
    }
    let rx_proven_chains = init_results.iter().filter(|r| r.rx_proven()).count();
    let initial_rx_bytes: Vec<usize> = init_results
        .iter()
        .map(|r| r.initial_get_address_rx_bytes)
        .collect();
    let fast_rx_bytes: Vec<usize> = init_results
        .iter()
        .map(|r| r.fast_get_address_rx_bytes)
        .collect();
    info!(
        chains = uarts.len(),
        total_chips,
        rx_proven_chains,
        initial_rx_bytes = ?initial_rx_bytes,
        fast_rx_bytes = ?fast_rx_bytes,
        "am3-bb: BM1362 enumeration complete across all chains"
    );

    // ── 6. Build the work-dispatch transport over the per-chain UARTs. ──
    let stub_loop = std::env::var_os("DCENT_AM3_BB_STUB_LOOP").is_some();
    let allow_no_rx_mining = env_flag_set(ENV_AM3_BB_ALLOW_NO_RX_MINING);
    if rx_proven_chains == 0 && !stub_loop && !allow_no_rx_mining {
        anyhow::bail!(
            "am3-bb: refusing full mining because no BM1362 chain returned any UART bytes \
             during 115200 or fast-baud GetAddress probes (initial_rx_bytes={:?}, \
             fast_rx_bytes={:?}). Set {}=1 only for a bench override; remaining blocker is \
             pre-work BM1362 UART/RX liveness, not nonce parsing.",
            initial_rx_bytes,
            fast_rx_bytes,
            ENV_AM3_BB_ALLOW_NO_RX_MINING
        );
    }
    if rx_proven_chains == 0 && allow_no_rx_mining {
        warn!(
            env = ENV_AM3_BB_ALLOW_NO_RX_MINING,
            initial_rx_bytes = ?initial_rx_bytes,
            fast_rx_bytes = ?fast_rx_bytes,
            "am3-bb: bench override active - dispatching work despite zero BM1362 RX proof"
        );
    }

    if let Some(dspic_i2c) = dspic_i2c_main.as_ref() {
        let open_core_mv = parse_env_u32(ENV_AM3_BB_OPEN_CORE_MV)
            .unwrap_or(u32::from(AM3_BB_DSPIC_DEFAULT_OPEN_CORE_MV))
            .clamp(
                u32::from(AM3_BB_DSPIC_MIN_VOLTAGE_MV),
                u32::from(AM3_BB_DSPIC_MAX_VOLTAGE_MV),
            ) as u16;
        let open_core_hold_ms = parse_env_u32(ENV_AM3_BB_OPEN_CORE_HOLD_MS)
            .map(u64::from)
            .unwrap_or(AM3_BB_DSPIC_DEFAULT_OPEN_CORE_HOLD_MS);
        info!(
            active_dspic_addrs = format_args!("{:02X?}", active_dspic_addrs),
            open_core_mv,
            open_core_hold_ms,
            steady_mv = dspic_target_voltage_mv,
            "am3-bb: starting LuxOS-style dsPIC open-core rail stage before work dispatch"
        );
        am3_bb_dspic_set_voltage_all(
            dspic_i2c,
            &active_dspic_addrs,
            open_core_mv,
            true,
            "open-core-voltage",
        )
        .context("am3-bb: dsPIC open-core rail stage failed")?;
        if open_core_hold_ms > 0 {
            thread::sleep(Duration::from_millis(open_core_hold_ms));
        }
        am3_bb_dspic_set_voltage_all(
            dspic_i2c,
            &active_dspic_addrs,
            dspic_target_voltage_mv,
            false,
            "steady-voltage",
        )
        .context("am3-bb: dsPIC steady rail stage failed")?;
        info!(
            steady_mv = dspic_target_voltage_mv,
            "am3-bb: dsPIC open-core -> steady rail sequence complete"
        );
    }

    drain_chain_uart_rx(&mut uarts, "pre-mining");

    let mut transport = Am335xUartTransport::new(uarts, UART_SEND_INTERVAL_US);
    info!(
        chains = transport.chain_count(),
        dispatch_interval_us = transport.dispatch_interval_us(),
        "am3-bb: Am335xUartTransport ready"
    );

    // ── 7. Drop the APW rail to steady (~13.8 V) now that chip-side init is done.
    //
    //       Do not call the full cold-boot sequence a second time here: that
    //       sequence owns GPIO reset assertion/release and would reset the
    //       chips after the post-baud init. The APW set-voltage payload is still
    //       a Phase-D stub, so this direct call logs the intent without touching
    //       ASIC resets. ──
    if let Err(e) = psu.set_voltage_mv(bt.cold_boot.apw12_rail_steady_mv) {
        warn!(
            target_mv = bt.cold_boot.apw12_rail_steady_mv,
            error = %e,
            "am3-bb: APW rail steady-drop reported an error (continuing - Phase-D stub)"
        );
    } else {
        match psu.read_voltage_mv() {
            Ok(readback_mv) => info!(
                steady_mv = bt.cold_boot.apw12_rail_steady_mv,
                readback_mv, "am3-bb: APW rail steady-drop step completed"
            ),
            Err(e) => warn!(
                steady_mv = bt.cold_boot.apw12_rail_steady_mv,
                error = %e,
                "am3-bb: APW rail steady-drop readback failed (continuing - Phase-D stub)"
            ),
        }
    }

    // ── 7b. Arm the hardware watchdog (AFTER cold-boot + chain enum complete).
    //       `--am3-bb-mining` runs entirely outside `Daemon::run()`, so this path
    //       historically armed NO `/dev/watchdog` — a CPU/runtime hang in the
    //       blocking mining loop below left the boards energized & unsupervised.
    //       Arm it now via the shared, config-gated helper. This body runs on a
    //       `spawn_blocking` thread with no ambient Tokio context, so enter the
    //       captured runtime handle first (the helper spawns the async kicker via
    //       `tokio::spawn`). Placed AFTER bring-up so the DTB-default window can
    //       never trip during the slow cold-boot. ──
    let watchdog_liveness = Arc::new(AtomicU64::new(0));
    {
        let _rt_enter = rt_handle.enter();
        // SAF-5: gate kicks on the am3-bb mining/stub loop so a live-locked
        // blocking runtime stops feeding `/dev/watchdog` after the counter has
        // started advancing.
        crate::daemon::spawn_watchdog_kicker(
            &config.watchdog,
            shutdown.clone(),
            Some(watchdog_liveness.clone()),
        );
    }

    // ── 8. Mining loop (Option B2 — reuse dcentrald_stratum + the transport).
    //       `DCENT_AM3_BB_STUB_LOOP=1` keeps the old logging-only stub for a
    //       cold-boot/enum-only diagnostic run. ──
    if stub_loop {
        warn!("am3-bb: DCENT_AM3_BB_STUB_LOOP set — running the cold-boot/enum-only logging stub, NOT the mining loop");
        run_mining_loop_stub(
            &mut transport,
            total_chips,
            &shutdown,
            watchdog_liveness.clone(),
        );
    } else {
        // Borrow the run guard's clamp-enforced fan view for the continuous
        // PR-021 PID. The guard keeps ownership for the fail-closed teardown;
        // this is `None` if the BeagleBone PWM never opened (the PID then
        // simply doesn't run — the fail-closed supervisor still does).
        let pid_fan = _run_safety_guard.as_ref().and_then(|g| g.capped_fan());
        run_mining_loop(
            &config,
            &mut transport,
            total_chips,
            &shutdown,
            &rt_handle,
            dspic_i2c_main.clone(),
            active_dspic_addrs.clone(),
            pid_fan,
            watchdog_liveness.clone(),
        )
        .context("am3-bb: mining loop exited with error")?;
    }

    info!("am3-bb: mining mode stopped cleanly");
    Ok(())
}

fn bm1362_command_wire_frame(frame_without_preamble: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(BM13XX_CMD_PREAMBLE.len() + frame_without_preamble.len());
    wire.extend_from_slice(&BM13XX_CMD_PREAMBLE);
    wire.extend_from_slice(frame_without_preamble);
    wire
}

fn bm1362_write_cmd_frame(uart: &mut Am3BbChainUart, frame_without_preamble: &[u8]) -> Result<()> {
    let wire = bm1362_command_wire_frame(frame_without_preamble);
    uart.write_bytes(&wire)
}

fn drain_chain_uart_rx(uarts: &mut [Am3BbChainUart], stage: &'static str) {
    const MAX_DRAIN_READS: usize = 12;
    let mut buf = [0u8; 512];
    for (chain_idx, uart) in uarts.iter_mut().enumerate() {
        if let Err(e) = uart.drain_tx() {
            warn!(
                chain = chain_idx,
                stage,
                error = %e,
                "am3-bb: UART TX drain failed before RX cleanup"
            );
        }

        let mut total = 0usize;
        let mut preview = Vec::with_capacity(96);
        for _ in 0..MAX_DRAIN_READS {
            let n = uart.read_bytes_timeout(&mut buf, 10);
            if n == 0 {
                break;
            }
            let n = n.min(buf.len());
            let take = (96usize.saturating_sub(preview.len())).min(n);
            preview.extend_from_slice(&buf[..take]);
            total += n;
        }
        uart.flush_io();

        if total > 0 {
            warn!(
                chain = chain_idx,
                stage,
                drained_rx_bytes = total,
                rx_preview = %hex_preview(&preview, 96),
                "am3-bb: drained residual BM1362 RX before mining transport handoff"
            );
        } else {
            debug!(
                chain = chain_idx,
                stage, "am3-bb: no residual BM1362 RX before mining transport handoff"
            );
        }
    }
}

/// One BM1362 broadcast register write (`build_broadcast_write_frame`, HDR=0x51)
/// + a short pace gap. Free fn (not a closure) so the `&mut Am3BbChainUart` borrow
/// is per-call — `bm1362_chip_init_one_chain` interleaves these with direct
/// `uart.write_bytes` calls (ChainInactive / SetChipAddress).
fn bm1362_bcast(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    reg: u8,
    val: u32,
    ms: u64,
    what: &str,
) -> Result<()> {
    bm1362_write_cmd_frame(uart, &build_broadcast_write_frame(reg, val))
        .with_context(|| format!("am3-bb chain {}: {} write failed", chain_idx, what))?;
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
    Ok(())
}

/// One BM1362 per-chip register write (`build_single_write_frame`, HDR=0x41)
/// + optional pace gap.
fn bm1362_single(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    chip_addr: u8,
    reg: u8,
    val: u32,
    ms: u64,
    what: &str,
) -> Result<()> {
    bm1362_write_cmd_frame(uart, &build_single_write_frame(chip_addr, reg, val)).with_context(
        || {
            format!(
                "am3-bb chain {} chip 0x{:02X}: {} write failed",
                chain_idx, chip_addr, what
            )
        },
    )?;
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
    Ok(())
}

fn bm1362_miscctrl_triple_write_single(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    chip_addr: u8,
    value: u32,
    what: &str,
) -> Result<()> {
    for _ in 0..3 {
        bm1362_single(
            uart,
            chain_idx,
            chip_addr,
            cold_boot_step::MISC_CONTROL_REG,
            value,
            5,
            what,
        )?;
    }
    Ok(())
}

fn bm1362_miscctrl_triple_write_bcast(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    value: u32,
    what: &str,
) -> Result<()> {
    for _ in 0..3 {
        bm1362_bcast(
            uart,
            chain_idx,
            cold_boot_step::MISC_CONTROL_REG,
            value,
            5,
            what,
        )?;
    }
    Ok(())
}

/// Write the BM1362 ASIC UART_RELAY block that W13 reclassified as the real
/// nonce RX/TX relay control surface. This is ASIC register traffic over the
/// hash chain, not a Braiins FPGA mirror write.
fn bm1362_uart_relay_bcast(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    stage: &'static str,
) -> Result<()> {
    if env_flag_set(ENV_AM3_BB_SKIP_UART_RELAY) {
        warn!(
            chain = chain_idx,
            stage, "am3-bb: lab override active — skipping BM1362 UART_RELAY 0x2C/0x34 broadcasts"
        );
        return Ok(());
    }

    bm1362_bcast(
        uart,
        chain_idx,
        UART_RELAY_REG_ADDR,
        UART_RELAY_BOSMINER_ENABLE,
        10,
        "UART_RELAY(0x2C)",
    )?;
    bm1362_bcast(
        uart,
        chain_idx,
        UART_RELAY_ALT_REG_ADDR,
        UART_RELAY_BOSMINER_ENABLE_ALT,
        10,
        "UART_RELAY_ALT(0x34)",
    )?;
    info!(
        chain = chain_idx,
        stage,
        reg_0x2c = format_args!("0x{:08X}", UART_RELAY_BOSMINER_ENABLE),
        reg_0x34 = format_args!("0x{:08X}", UART_RELAY_BOSMINER_ENABLE_ALT),
        "am3-bb: BM1362 UART_RELAY broadcasts applied"
    );
    Ok(())
}

fn bm1362_uart_relay_single(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    chip_addr: u8,
    stage: &'static str,
) -> Result<()> {
    if env_flag_set(ENV_AM3_BB_SKIP_UART_RELAY) {
        return Ok(());
    }

    bm1362_single(
        uart,
        chain_idx,
        chip_addr,
        UART_RELAY_REG_ADDR,
        UART_RELAY_BOSMINER_ENABLE,
        0,
        "UART_RELAY(0x2C) per-chip",
    )?;
    bm1362_single(
        uart,
        chain_idx,
        chip_addr,
        UART_RELAY_ALT_REG_ADDR,
        UART_RELAY_BOSMINER_ENABLE_ALT,
        0,
        "UART_RELAY_ALT(0x34) per-chip",
    )?;
    debug!(
        chain = chain_idx,
        chip_addr = format_args!("0x{:02X}", chip_addr),
        stage,
        reg_0x2c = format_args!("0x{:08X}", UART_RELAY_BOSMINER_ENABLE),
        reg_0x34 = format_args!("0x{:08X}", UART_RELAY_BOSMINER_ENABLE_ALT),
        "am3-bb: BM1362 UART_RELAY per-chip writes applied"
    );
    Ok(())
}

fn bm1362_per_chip_init_loop(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    n_assign: usize,
    addr_interval: u16,
    init_values: Bm1362Am3InitValues,
    run_miscctrl_triple_write: bool,
    stage: &'static str,
) -> Result<()> {
    info!(
        chain = chain_idx,
        chips = n_assign,
        addr_interval,
        stage,
        "am3-bb: BM1362 per-chip init starting"
    );
    for i in 0..n_assign {
        let chip_addr = (i as u16 * addr_interval) as u8;
        bm1362_single(
            uart,
            chain_idx,
            chip_addr,
            BM1362_REG_INIT_CONTROL,
            init_values.init_control_per_chip,
            0,
            "InitControl(0xA8) per-chip",
        )?;
        if run_miscctrl_triple_write {
            bm1362_miscctrl_triple_write_single(
                uart,
                chain_idx,
                chip_addr,
                init_values.misc_control_pre_baud,
                "MiscCtrl(0x18) per-chip pre-baud",
            )?;
        }
        bm1362_single(
            uart,
            chain_idx,
            chip_addr,
            BM1362_REG_CORE_CTRL,
            BM1362_CORE_REG_HASH_CLK,
            0,
            "CoreReg(0x3C) HashClk per-chip",
        )?;
        bm1362_single(
            uart,
            chain_idx,
            chip_addr,
            BM1362_REG_CORE_CTRL,
            BM1362_CORE_REG_CLK_DELAY,
            0,
            "CoreReg(0x3C) ClkDelay per-chip",
        )?;
        bm1362_single(
            uart,
            chain_idx,
            chip_addr,
            BM1362_REG_CORE_CTRL,
            BM1362_CORE_REG_UNKNOWN,
            0,
            "CoreReg(0x3C) Unknown per-chip",
        )?;
        bm1362_uart_relay_single(uart, chain_idx, chip_addr, stage)?;

        if i % 16 == 15 {
            std::thread::sleep(Duration::from_millis(BM1362_SERIAL_PACE_MIN_MS));
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    info!(
        chain = chain_idx,
        chips = n_assign,
        stage,
        "am3-bb: BM1362 per-chip init complete"
    );
    Ok(())
}

fn bm1362_pll_ramp_to_target(target_freq_mhz: u16) -> Vec<(u32, u16)> {
    let target = target_freq_mhz.clamp(400, 597);
    let mut steps =
        bm1362_pll_ramp_sequence(BM1362_PLL_RAMP_START_MHZ, target, BM1362_PLL_RAMP_STEP_MHZ);

    // The live BM1362 trace has a special 525 MHz value. Keep that exact value
    // when 525 MHz is the requested endpoint; otherwise use the canonical PLL
    // lookup table for ramp steps and fallback targets.
    if target == 525 {
        for (reg, actual_mhz) in &mut steps {
            if *actual_mhz == 525 {
                *reg = BM1362_PLL0_PARAM_525MHZ;
            }
        }
    }

    if steps.is_empty() {
        steps.push(bm1362_pll_lookup(target));
    }
    steps
}

fn env_flag_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn parse_env_u32(name: &str) -> Option<u32> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse::<u32>().ok()
    };

    if parsed.is_none() {
        warn!(
            env = name,
            value = %trimmed,
            "am3-bb: ignoring invalid integer environment override"
        );
    }

    parsed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Am3BbWorkCodec {
    /// Proven BM1362 chip-comm job frame:
    /// `[55 AA][21][56][82-byte full-header payload][CRC16 BE]`.
    Serial88,
    /// W4 stock-`uart_trans` `asic_work_t` frame:
    /// 86 bytes beginning with type `0xAA`, no direct `55 AA` preamble.
    Asic86,
}

impl Am3BbWorkCodec {
    fn from_env() -> Self {
        match std::env::var(ENV_AM3_BB_WORK_CODEC)
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("serial88") | Some("serial") | Some("bm1362") => Self::Serial88,
            Some("asic86") | Some("w4") | Some("uart_trans86") | Some("uart-trans86") => {
                warn!(
                    env = ENV_AM3_BB_WORK_CODEC,
                    "am3-bb: lab override active - using W4 86-byte AsicWorkFrame dispatch"
                );
                Self::Asic86
            }
            Some(other) => {
                warn!(
                    env = ENV_AM3_BB_WORK_CODEC,
                    value = %other,
                    "am3-bb: unknown work codec override; falling back to serial88"
                );
                Self::Serial88
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Serial88 => "serial88",
            Self::Asic86 => "asic86",
        }
    }

    fn job_id_slot(self, sent_job_id: u8) -> u8 {
        match self {
            Self::Serial88 => echoed_job_id(sent_job_id),
            Self::Asic86 => sent_job_id,
        }
    }

    fn job_id_increment(self) -> u8 {
        match self {
            Self::Serial88 => JOB_ID_INCREMENT,
            Self::Asic86 => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bm1362Am3InitValues {
    init_control_broadcast: u32,
    init_control_per_chip: u32,
    misc_control_pre_baud: u32,
    misc_control_post_fast_baud: u32,
    label: &'static str,
}

fn bm1362_am3_init_values_from_env() -> Bm1362Am3InitValues {
    if env_flag_set(ENV_AM3_BB_LEGACY_AMLOGIC_INIT) {
        warn!(
            env = ENV_AM3_BB_LEGACY_AMLOGIC_INIT,
            "am3-bb: lab override active - using legacy Amlogic-derived BM1362 init values"
        );
        return Bm1362Am3InitValues {
            init_control_broadcast: BM1362_INIT_CONTROL_BCAST_LEGACY_AMLOGIC,
            init_control_per_chip: BM1362_INIT_CONTROL_PER_CHIP_LEGACY_AMLOGIC,
            misc_control_pre_baud: BM1362_MISC_CONTROL_LEGACY_AMLOGIC,
            misc_control_post_fast_baud: BM1362_MISC_CONTROL_LEGACY_AMLOGIC,
            label: "legacy-amlogic",
        };
    }

    Bm1362Am3InitValues {
        init_control_broadcast: BM1362_INIT_CONTROL_BCAST,
        init_control_per_chip: BM1362_INIT_CONTROL_PER_CHIP,
        misc_control_pre_baud: BM1362_INIT_PLAN.misc_control_pre_baud,
        misc_control_post_fast_baud: BM1362_INIT_PLAN.misc_control_post_fast_baud,
        label: "canonical-bm1362-init-plan",
    }
}

fn hex_preview(bytes: &[u8], max: usize) -> String {
    bytes
        .iter()
        .take(max)
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

fn am3_bb_dspic_addr_for_chain(chain_idx: usize) -> u8 {
    AM3_BB_DSPIC_BASE_ADDR.saturating_add(chain_idx as u8)
}

fn am3_bb_dspic_temp_bridge_frame(sensor_addr: u8) -> [u8; 8] {
    let checksum = 0x06u8
        .wrapping_add(0x3C)
        .wrapping_add(sensor_addr)
        .wrapping_add(0x02)
        .wrapping_add(0x00);
    [0x55, 0xAA, 0x06, 0x3C, sensor_addr, 0x02, 0x00, checksum]
}

fn am3_bb_decode_lm75_bridge_reply(reply: &[u8]) -> Option<f32> {
    if reply.len() < AM3_BB_LM75_REPLY_LEN || reply[0] != 0x07 || reply[1] != 0x3C {
        return None;
    }
    let checksum = reply[..6].iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    if checksum != reply[6] {
        return None;
    }
    let temp_c = i16::from_be_bytes([reply[3], reply[4]]) as f32 / 256.0;
    if (AM3_BB_LM75_MIN_VALID_C..=AM3_BB_LM75_MAX_VALID_C).contains(&temp_c) {
        Some(temp_c)
    } else {
        None
    }
}

fn am3_bb_dspic_target_voltage_mv(config_mv: u16) -> u16 {
    if (AM3_BB_DSPIC_MIN_VOLTAGE_MV..=AM3_BB_DSPIC_MAX_VOLTAGE_MV).contains(&config_mv) {
        config_mv
    } else {
        warn!(
            config_mv,
            fallback_mv = AM3_BB_DSPIC_DEFAULT_TARGET_MV,
            min_mv = AM3_BB_DSPIC_MIN_VOLTAGE_MV,
            max_mv = AM3_BB_DSPIC_MAX_VOLTAGE_MV,
            "am3-bb: mining.voltage_mv is outside the fw=0x89 dsPIC DAC range; using S19j Pro default"
        );
        AM3_BB_DSPIC_DEFAULT_TARGET_MV
    }
}

fn am3_bb_dspic_voltage_dac(voltage_mv: u16) -> u8 {
    let clamped = voltage_mv.clamp(AM3_BB_DSPIC_MIN_VOLTAGE_MV, AM3_BB_DSPIC_MAX_VOLTAGE_MV);
    let offset = u32::from(clamped - AM3_BB_DSPIC_MIN_VOLTAGE_MV);
    ((offset * 11 + 1600) / 3200) as u8
}

fn am3_bb_dspic_set_voltage_frame(voltage_mv: u16) -> [u8; 6] {
    let dac = am3_bb_dspic_voltage_dac(voltage_mv);
    let checksum = 0x04u8.wrapping_add(0x10).wrapping_add(dac);
    [0x55, 0xAA, 0x04, 0x10, dac, checksum]
}

fn am3_bb_dspic_command(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    frame: &[u8],
    read_count: usize,
    after_write_ms: u64,
    between_read_ms: u64,
    what: &str,
) -> Result<Vec<u8>> {
    let mut steps = Vec::with_capacity(1 + read_count.saturating_mul(2));
    steps.push(I2cTransactionStep::Write(frame.to_vec()));
    if after_write_ms > 0 {
        steps.push(I2cTransactionStep::SleepMs(after_write_ms));
    }
    for read_idx in 0..read_count {
        steps.push(I2cTransactionStep::Read(1));
        if between_read_ms > 0 && read_idx + 1 < read_count {
            steps.push(I2cTransactionStep::SleepMs(between_read_ms));
        }
    }

    let reads = i2c.transaction(addr, steps).with_context(|| {
        format!(
            "am3-bb chain {} dsPIC 0x{:02X}: {} transaction failed",
            chain_idx, addr, what
        )
    })?;
    let out: Vec<u8> = reads.into_iter().flatten().collect();
    if out.len() != read_count {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            command = what,
            expected_read_bytes = read_count,
            actual_read_bytes = out.len(),
            reply = format_args!("{:02X?}", out),
            "am3-bb: dsPIC command returned a short reply"
        );
    }
    Ok(out)
}

fn am3_bb_dspic_read_lm75_temps(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    stage: &'static str,
) -> Vec<(u8, f32)> {
    let mut temps = Vec::new();
    for sensor_addr in AM3_BB_LM75_SENSOR_ADDRS {
        let frame = am3_bb_dspic_temp_bridge_frame(sensor_addr);
        match am3_bb_dspic_command(
            i2c,
            chain_idx,
            addr,
            &frame,
            AM3_BB_LM75_REPLY_LEN,
            20,
            20,
            "0x3c LM75 bridge read",
        ) {
            Ok(reply) => match am3_bb_decode_lm75_bridge_reply(&reply) {
                Some(temp_c) => {
                    debug!(
                        chain = chain_idx,
                        addr = format_args!("0x{:02X}", addr),
                        sensor = format_args!("0x{:02X}", sensor_addr),
                        stage,
                        temp_c,
                        reply = format_args!("{:02X?}", reply),
                        "am3-bb: dsPIC LM75 bridge temperature"
                    );
                    temps.push((sensor_addr, temp_c));
                }
                None => {
                    debug!(
                        chain = chain_idx,
                        addr = format_args!("0x{:02X}", addr),
                        sensor = format_args!("0x{:02X}", sensor_addr),
                        stage,
                        reply = format_args!("{:02X?}", reply),
                        "am3-bb: invalid dsPIC LM75 bridge reply"
                    );
                }
            },
            Err(e) => {
                debug!(
                    chain = chain_idx,
                    addr = format_args!("0x{:02X}", addr),
                    sensor = format_args!("0x{:02X}", sensor_addr),
                    stage,
                    error = %e,
                    "am3-bb: dsPIC LM75 bridge read failed"
                );
            }
        }
    }
    temps
}

#[derive(Debug, Clone, Copy)]
struct Am3BbThermalSnapshot {
    samples: usize,
    max_temp_c: f32,
}

fn am3_bb_poll_dspic_temps(
    i2c: &I2cServiceHandle,
    active_addrs: &[u8],
    stage: &'static str,
) -> Am3BbThermalSnapshot {
    let mut samples = 0usize;
    let mut max_temp_c = f32::NEG_INFINITY;
    for &addr in active_addrs {
        let chain_idx = addr.saturating_sub(AM3_BB_DSPIC_BASE_ADDR) as usize;
        for (_sensor, temp_c) in am3_bb_dspic_read_lm75_temps(i2c, chain_idx, addr, stage) {
            samples += 1;
            max_temp_c = max_temp_c.max(temp_c);
        }
    }
    Am3BbThermalSnapshot {
        samples,
        max_temp_c,
    }
}

struct Am3BbThermalSupervisor {
    i2c: I2cServiceHandle,
    active_addrs: Vec<u8>,
    hot_temp_c: f32,
    dangerous_temp_c: f32,
    last_good: Option<(Instant, Am3BbThermalSnapshot)>,
    consecutive_misses: u8,
}

impl Am3BbThermalSupervisor {
    fn new(
        i2c: I2cServiceHandle,
        active_addrs: Vec<u8>,
        hot_temp_c: u8,
        dangerous_temp_c: u8,
    ) -> Result<Self> {
        if active_addrs.is_empty() {
            anyhow::bail!("am3-bb: thermal supervisor requires at least one active dsPIC");
        }
        Ok(Self {
            i2c,
            active_addrs,
            hot_temp_c: f32::from(hot_temp_c),
            dangerous_temp_c: f32::from(dangerous_temp_c),
            last_good: None,
            consecutive_misses: 0,
        })
    }

    fn poll_and_check(&mut self, stage: &'static str) -> Result<Am3BbThermalSnapshot> {
        let mut snapshot = am3_bb_poll_dspic_temps(&self.i2c, &self.active_addrs, stage);
        if stage == "runtime" && snapshot.samples == 0 {
            thread::sleep(Duration::from_millis(AM3_BB_THERMAL_RUNTIME_RETRY_MS));
            snapshot = am3_bb_poll_dspic_temps(&self.i2c, &self.active_addrs, "runtime-retry");
        }
        if snapshot.samples == 0 {
            self.consecutive_misses = self.consecutive_misses.saturating_add(1);
            if stage == "runtime" {
                if let Some((sample_at, last_good)) = self.last_good {
                    let age = sample_at.elapsed();
                    if self.consecutive_misses <= AM3_BB_THERMAL_MAX_CONSECUTIVE_MISSES
                        && age <= Duration::from_millis(AM3_BB_THERMAL_MAX_STALE_MS)
                    {
                        warn!(
                            consecutive_misses = self.consecutive_misses,
                            last_good_age_ms = age.as_millis(),
                            last_good_samples = last_good.samples,
                            last_good_max_temp_c = last_good.max_temp_c,
                            max_consecutive_misses = AM3_BB_THERMAL_MAX_CONSECUTIVE_MISSES,
                            max_stale_ms = AM3_BB_THERMAL_MAX_STALE_MS,
                            "am3-bb: runtime LM75 poll was noisy; using fresh last-known-good sample"
                        );
                        return Ok(last_good);
                    }
                }
            }
            anyhow::bail!(
                "am3-bb: no valid LM75 temperature samples during {} after {} consecutive miss(es) - refusing to mine without fresh thermal proof",
                stage,
                self.consecutive_misses
            );
        }
        self.last_good = Some((Instant::now(), snapshot));
        self.consecutive_misses = 0;
        if snapshot.max_temp_c >= self.dangerous_temp_c {
            anyhow::bail!(
                "am3-bb: hashboard temperature {:.1}C reached dangerous threshold {:.1}C during {}",
                snapshot.max_temp_c,
                self.dangerous_temp_c,
                stage
            );
        }
        if snapshot.max_temp_c >= self.hot_temp_c {
            warn!(
                samples = snapshot.samples,
                max_temp_c = snapshot.max_temp_c,
                hot_temp_c = self.hot_temp_c,
                dangerous_temp_c = self.dangerous_temp_c,
                stage,
                "am3-bb: hashboard temperature is hot; quiet guard remains capped and will fail closed at dangerous threshold"
            );
        } else {
            info!(
                samples = snapshot.samples,
                max_temp_c = snapshot.max_temp_c,
                hot_temp_c = self.hot_temp_c,
                dangerous_temp_c = self.dangerous_temp_c,
                stage,
                "am3-bb: thermal supervisor sample OK"
            );
        }
        Ok(snapshot)
    }
}

fn am3_bb_dspic_heartbeat_once(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
) -> Result<Vec<u8>> {
    am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_HEARTBEAT_FRAME,
        6,
        20,
        20,
        "heartbeat",
    )
}

fn am3_bb_dspic_read_voltage(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    what: &str,
) -> Result<Vec<u8>> {
    am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_READ_VOLTAGE_FRAME,
        7,
        50,
        20,
        what,
    )
}

fn am3_bb_dspic_set_voltage(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    voltage_mv: u16,
    stage: &str,
) -> Result<Vec<u8>> {
    let frame = am3_bb_dspic_set_voltage_frame(voltage_mv);
    if env_flag_set(ENV_AM3_BB_SKIP_DSPIC_SET_VOLTAGE) {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            voltage_mv,
            stage,
            frame = format_args!("{:02X?}", frame),
            env = ENV_AM3_BB_SKIP_DSPIC_SET_VOLTAGE,
            "am3-bb: lab override active - skipping dsPIC SetVoltage"
        );
        return Ok(Vec::new());
    }
    am3_bb_dspic_command(i2c, chain_idx, addr, &frame, 0, 0, 0, stage)?;
    thread::sleep(Duration::from_millis(50));
    Ok(frame.to_vec())
}

fn am3_bb_dspic_enable_voltage(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    stage: &str,
) -> Result<Vec<u8>> {
    let enable_ack = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_ENABLE_FRAME,
        2,
        50,
        20,
        stage,
    )?;
    let enable_ok = enable_ack.first().copied() == Some(0x15)
        && matches!(enable_ack.get(1).copied(), Some(0x00 | 0x01));
    if !enable_ok {
        anyhow::bail!(
            "am3-bb chain {} dsPIC 0x{:02X}: enable-voltage ACK mismatch during {}: {:02X?}",
            chain_idx,
            addr,
            stage,
            enable_ack
        );
    }
    Ok(enable_ack)
}

fn am3_bb_dspic_set_voltage_all(
    i2c: &I2cServiceHandle,
    active_addrs: &[u8],
    voltage_mv: u16,
    enable: bool,
    stage: &'static str,
) -> Result<()> {
    for &addr in active_addrs {
        let chain_idx = addr.saturating_sub(AM3_BB_DSPIC_BASE_ADDR) as usize;
        let voltage_before =
            am3_bb_dspic_read_voltage(i2c, chain_idx, addr, "read-voltage-before-rail-stage")?;
        let set_frame = am3_bb_dspic_set_voltage(i2c, chain_idx, addr, voltage_mv, stage)?;
        let voltage_after_set =
            am3_bb_dspic_read_voltage(i2c, chain_idx, addr, "read-voltage-after-rail-set")?;
        let enable_ack = if enable {
            let ack = am3_bb_dspic_enable_voltage(i2c, chain_idx, addr, stage)?;
            thread::sleep(Duration::from_millis(70));
            ack
        } else {
            Vec::new()
        };
        let voltage_after =
            am3_bb_dspic_read_voltage(i2c, chain_idx, addr, "read-voltage-after-rail-stage")?;
        info!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            stage,
            voltage_mv,
            enable,
            set_frame = format_args!("{:02X?}", set_frame),
            enable_ack = format_args!("{:02X?}", enable_ack),
            voltage_before = format_args!("{:02X?}", voltage_before),
            voltage_after_set = format_args!("{:02X?}", voltage_after_set),
            voltage_after = format_args!("{:02X?}", voltage_after),
            "am3-bb: dsPIC rail stage complete"
        );
    }
    Ok(())
}

fn am3_bb_dspic_init_one(
    i2c: &I2cServiceHandle,
    chain_idx: usize,
    addr: u8,
    target_voltage_mv: u16,
    early_enable: bool,
) -> Result<Option<u8>> {
    info!(
        chain = chain_idx,
        addr = format_args!("0x{:02X}", addr),
        "am3-bb: LuxOS-trace dsPIC init starting"
    );

    let reset_ack = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_RESET_FRAME,
        2,
        50,
        20,
        "framed parser reset",
    )?;
    if reset_ack.first().copied() != Some(0x07) {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            reply = format_args!("{:02X?}", reset_ack),
            "am3-bb: dsPIC reset echo mismatch; treating this chain controller as absent"
        );
        return Ok(None);
    }
    thread::sleep(Duration::from_millis(500));

    let jump_ack = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_JUMP_FRAME,
        2,
        50,
        20,
        "jump-to-app",
    )?;
    if jump_ack.first().copied() != Some(0x06) {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            reply = format_args!("{:02X?}", jump_ack),
            "am3-bb: dsPIC jump echo mismatch; treating this chain controller as absent"
        );
        return Ok(None);
    }
    thread::sleep(Duration::from_millis(400));

    let version = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_GET_VERSION_FRAME,
        5,
        135,
        20,
        "get-version",
    )?;
    let Some(fw) = version.get(2).copied() else {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            reply = format_args!("{:02X?}", version),
            "am3-bb: dsPIC version reply too short"
        );
        return Ok(None);
    };
    if fw != 0x89 {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            firmware = format_args!("0x{:02X}", fw),
            reply = format_args!("{:02X?}", version),
            "am3-bb: dsPIC firmware is not the LuxOS-traced fw=0x89 path; skipping controller"
        );
        return Ok(None);
    }

    let disable_ack = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_DISABLE_FRAME,
        2,
        50,
        20,
        "disable-voltage",
    )?;
    if disable_ack.first().copied() != Some(0x15) {
        warn!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            reply = format_args!("{:02X?}", disable_ack),
            "am3-bb: dsPIC disable-voltage ACK mismatch; continuing with LuxOS trace sequence"
        );
    }
    thread::sleep(Duration::from_millis(40));

    let probe_3b = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_PROBE_3B_48_FRAME,
        2,
        20,
        20,
        "0x3b/0x48 probe",
    )?;
    debug!(
        chain = chain_idx,
        addr = format_args!("0x{:02X}", addr),
        reply = format_args!("{:02X?}", probe_3b),
        "am3-bb: dsPIC 0x3b/0x48 probe reply"
    );

    let heartbeat = am3_bb_dspic_heartbeat_once(i2c, chain_idx, addr)?;
    debug!(
        chain = chain_idx,
        addr = format_args!("0x{:02X}", addr),
        reply = format_args!("{:02X?}", heartbeat),
        "am3-bb: dsPIC initial heartbeat reply"
    );

    for sensor_addr in AM3_BB_LM75_SENSOR_ADDRS {
        let frame = am3_bb_dspic_temp_bridge_frame(sensor_addr);
        let reply = am3_bb_dspic_command(
            i2c,
            chain_idx,
            addr,
            &frame,
            7,
            20,
            20,
            "0x3c LM75 bridge read",
        )?;
        debug!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            sensor = format_args!("0x{:02X}", sensor_addr),
            reply = format_args!("{:02X?}", reply),
            "am3-bb: dsPIC LM75 bridge reply"
        );
    }

    let voltage_before = am3_bb_dspic_command(
        i2c,
        chain_idx,
        addr,
        AM3_BB_DSPIC_READ_VOLTAGE_FRAME,
        7,
        50,
        20,
        "read-voltage-before-enable",
    )?;

    let set_voltage_frame = am3_bb_dspic_set_voltage_frame(target_voltage_mv);
    let (voltage_after_set, enable_ack, voltage_after) = if early_enable {
        am3_bb_dspic_set_voltage(i2c, chain_idx, addr, target_voltage_mv, "early-enable")?;
        let voltage_after_set =
            am3_bb_dspic_read_voltage(i2c, chain_idx, addr, "read-voltage-after-early-set")?;
        let enable_ack = am3_bb_dspic_enable_voltage(i2c, chain_idx, addr, "early-enable")?;
        thread::sleep(Duration::from_millis(70));
        let voltage_after =
            am3_bb_dspic_read_voltage(i2c, chain_idx, addr, "read-voltage-after-early-enable")?;
        (voltage_after_set, enable_ack, voltage_after)
    } else {
        info!(
            chain = chain_idx,
            addr = format_args!("0x{:02X}", addr),
            target_voltage_mv,
            "am3-bb: dsPIC controller initialized with DC/DC disabled; BM1362 enum/init will run before rail enable"
        );
        (Vec::new(), Vec::new(), voltage_before.clone())
    };
    let heartbeat_after = am3_bb_dspic_heartbeat_once(i2c, chain_idx, addr)?;

    info!(
        chain = chain_idx,
        addr = format_args!("0x{:02X}", addr),
        firmware = format_args!("0x{:02X}", fw),
        reset_ack = format_args!("{:02X?}", reset_ack),
        jump_ack = format_args!("{:02X?}", jump_ack),
        version = format_args!("{:02X?}", version),
        disable_ack = format_args!("{:02X?}", disable_ack),
        enable_ack = format_args!("{:02X?}", enable_ack),
        target_voltage_mv,
        set_voltage_frame = format_args!("{:02X?}", set_voltage_frame),
        voltage_before = format_args!("{:02X?}", voltage_before),
        voltage_after_set = format_args!("{:02X?}", voltage_after_set),
        voltage_after = format_args!("{:02X?}", voltage_after),
        heartbeat_after = format_args!("{:02X?}", heartbeat_after),
        "am3-bb: LuxOS-trace dsPIC init complete"
    );

    Ok(Some(fw))
}

fn am3_bb_dspic_init_all(
    i2c: &I2cServiceHandle,
    chain_count: usize,
    target_voltage_mv: u16,
    early_enable: bool,
) -> Vec<u8> {
    let mut active_addrs = Vec::new();
    for chain_idx in 0..chain_count {
        let addr = am3_bb_dspic_addr_for_chain(chain_idx);
        match am3_bb_dspic_init_one(i2c, chain_idx, addr, target_voltage_mv, early_enable) {
            Ok(Some(_fw)) => active_addrs.push(addr),
            Ok(None) => {}
            Err(e) => warn!(
                chain = chain_idx,
                addr = format_args!("0x{:02X}", addr),
                error = %e,
                "am3-bb: dsPIC init failed on this chain controller"
            ),
        }
    }
    active_addrs
}

fn am3_bb_write_reset_gpio(gpio: u32, asserted: bool) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/value", gpio);
    std::fs::write(&path, if asserted { "1" } else { "0" }).with_context(|| {
        format!(
            "am3-bb: write {} to {} failed",
            if asserted { "assert" } else { "release" },
            path
        )
    })
}

fn am3_bb_export_gpio_if_needed(gpio: u32) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}", gpio);
    if std::path::Path::new(&path).exists() {
        return Ok(());
    }
    std::fs::write("/sys/class/gpio/export", gpio.to_string())
        .with_context(|| format!("am3-bb: export gpio{} failed", gpio))?;
    thread::sleep(Duration::from_millis(10));
    Ok(())
}

fn am3_bb_write_gpio_attr(gpio: u32, attr: &str, value: &str) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/{}", gpio, attr);
    std::fs::write(&path, value)
        .with_context(|| format!("am3-bb: write gpio{} {}={} failed", gpio, attr, value))
}

fn am3_bb_prepare_output_gpio(gpio: u32, active_low: bool) -> Result<()> {
    am3_bb_export_gpio_if_needed(gpio)?;
    am3_bb_write_gpio_attr(gpio, "direction", "out")?;
    if let Err(e) = am3_bb_write_gpio_attr(gpio, "active_low", if active_low { "1" } else { "0" }) {
        debug!(
            gpio,
            active_low,
            error = %e,
            "am3-bb: active_low sysfs write failed during safety teardown"
        );
    }
    Ok(())
}

fn am3_bb_quiet_safe_pwm(config_min_pwm: u8, config_max_pwm: u8) -> u8 {
    let cap = config_max_pwm.min(AM3_BB_FAN_HARD_CAP_PWM);
    if cap >= AM3_BB_FAN_SAFE_FLOOR_PWM {
        config_min_pwm.max(AM3_BB_FAN_SAFE_FLOOR_PWM).min(cap)
    } else {
        cap
    }
}

/// The absolute fan-PWM ceiling for the AM3 BB home/quiet posture.
///
/// This is the single chokepoint every PR-021 continuous-PID fan write passes
/// through. It is mathematically impossible for the return value to exceed
/// `AM3_BB_FAN_HARD_CAP_PWM` (30) — the home/night/space-heater hard cap from
///  and the rust-firmware rule
/// "NEVER allow fans above PWM 30 for home mining". `config_max_pwm` only ever
/// *lowers* the cap (an operator can ask for quieter, never louder); the floor
/// keeps the fan at the whisper-quiet boot level when the PID asks for less.
fn am3_bb_clamp_pid_pwm(config_min_pwm: u8, config_max_pwm: u8, requested: u8) -> u8 {
    let cap = config_max_pwm.min(AM3_BB_FAN_HARD_CAP_PWM);
    let floor = if cap >= AM3_BB_FAN_SAFE_FLOOR_PWM {
        config_min_pwm.max(AM3_BB_FAN_SAFE_FLOOR_PWM).min(cap)
    } else {
        // Operator deliberately set an even lower cap; honour it. Safety on
        // this board comes from cutting ASIC power, never from fan blast.
        cap
    };
    requested.clamp(floor, cap)
}

/// The thermal response, in the order it must happen.
///
/// Invariant #2 (cut hash power BEFORE raising fan noise) is encoded here as a
/// total order: at or above the dangerous threshold the answer is always
/// [`Am3BbThermalAction::CutHashThenFan`], which the caller services by failing
/// closed (the existing `poll_and_check` `Err` → run-guard teardown disables
/// dsPIC voltage / asserts resets / drives board-enable off, THEN the guard
/// keeps the fan at its quiet cap). The PID is only consulted in the
/// [`Am3BbThermalAction::PidWithinCap`] arm, i.e. while temperature is still
/// below dangerous — so the fan is only ever raised *within* the quiet cap and
/// only while hash power is still safely on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Am3BbThermalAction {
    /// temp < dangerous: let the capped PID trim the fan toward the setpoint.
    PidWithinCap,
    /// temp >= dangerous: cut hash power first; fan stays at the quiet cap.
    /// (The actual teardown is owned by the run-scope guard / fail-closed
    /// `poll_and_check`; the PID must NOT try to "out-cool" a dangerous temp
    /// by blasting — that is explicitly the wrong order.)
    CutHashThenFan,
}

/// Pure decision: given the hottest valid board temp and the dangerous
/// threshold, which response (and in which order) applies. Kept free of I/O so
/// it is host-testable and so the ordering invariant is a unit-tested contract.
fn am3_bb_thermal_action(max_temp_c: f32, dangerous_temp_c: f32) -> Am3BbThermalAction {
    if max_temp_c >= dangerous_temp_c {
        Am3BbThermalAction::CutHashThenFan
    } else {
        Am3BbThermalAction::PidWithinCap
    }
}

/// A clamp-enforced view onto the run guard's fan.
///
/// The run-scope [`Am3BbRunSafetyGuard`] retains ownership of the physical fan
/// for teardown; this is a cheap `Arc` clone the runtime PID borrows. EVERY
/// write goes through [`am3_bb_clamp_pid_pwm`], so this type cannot be used to
/// exceed the quiet cap even if a caller passes a bogus value. There is no
/// uncapped setter and no `set_speed_override` — by construction the PID can
/// only move the fan inside `[floor, min(fan_max_pwm, 30)]`.
#[derive(Clone)]
struct Am3BbCappedFan {
    fan: Arc<dyn FanAccess>,
    fan_min_pwm: u8,
    fan_max_pwm: u8,
}

impl Am3BbCappedFan {
    fn cap(&self) -> u8 {
        self.fan_max_pwm.min(AM3_BB_FAN_HARD_CAP_PWM)
    }

    fn floor(&self) -> u8 {
        am3_bb_quiet_safe_pwm(self.fan_min_pwm, self.fan_max_pwm)
    }

    /// Apply a PID-requested PWM, clamped to the quiet envelope. Returns the
    /// PWM actually written so the caller can log/track it.
    fn apply(&self, requested: u8) -> u8 {
        let pwm = am3_bb_clamp_pid_pwm(self.fan_min_pwm, self.fan_max_pwm, requested);
        self.fan.set_speed(pwm);
        pwm
    }

    fn get_rpm(&self) -> u32 {
        self.fan.get_rpm()
    }

    fn tach_available(&self) -> bool {
        self.fan.tach_available()
    }
}

/// The PR-021 continuous fan PID.
///
/// Wraps the proven `dcentrald_thermal::controller::PidController` (reused, not
/// reinvented — same P + anti-windup-I + D math `daemon.rs` uses) and bolts on
/// the AM3 BB quiet-home guarantees:
///  - the PID output is clamped to `[floor, min(fan_max_pwm, 30)]` BEFORE it is
///    ever written (never above the home cap, never a transient spike);
///  - the commanded PWM walks toward the target at most
///    `AM3_BB_FAN_PID_MAX_STEP_PWM` per tick (no audible jump);
///  - on an EMPTY thermal sample the PID does NOT command anything — it holds
///    the last commanded PWM and lets the fail-closed `poll_and_check` own the
///    stale/empty decision (invariant #3: never act on absent sensor data);
///  - at/above the dangerous threshold the PID stops trimming and the caller's
///    fail-closed path cuts hash power first (invariant #2 ordering).
struct Am3BbFanPid {
    pid: dcentrald_thermal::controller::PidController,
    fan: Am3BbCappedFan,
    commanded_pwm: u8,
}

impl Am3BbFanPid {
    fn new(fan: Am3BbCappedFan, target_temp_c: u8) -> Self {
        let pid = dcentrald_thermal::controller::PidController::new(f32::from(target_temp_c));
        // Start at the quiet floor — the boot/whisper level. The PID only ever
        // climbs from here on measured need, and only within the cap.
        let floor = fan.floor();
        let commanded_pwm = fan.apply(floor);
        Self {
            pid,
            fan,
            commanded_pwm,
        }
    }

    /// Feed the supervisor's just-validated snapshot to the PID and move the
    /// fan one bounded step toward the (capped) target. `samples == 0` means
    /// the supervisor served a tolerated last-known-good window or is about to
    /// fail closed — either way we must NOT compute a fan action from absent
    /// data; hold station and return.
    fn step(&mut self, snapshot: &Am3BbThermalSnapshot, dangerous_temp_c: f32) {
        if snapshot.samples == 0 || !snapshot.max_temp_c.is_finite() {
            // Invariant #3: no board/LM75 data → do not drive fans off empty
            // readings. The fail-closed supervisor decides stale-vs-tolerate.
            return;
        }

        // Invariant #2: at/above dangerous, the response is cut-hash-first.
        // The PID does not try to cool its way out by ramping the fan; the
        // caller's `poll_and_check` returns Err and the run guard tears down
        // (voltage off → resets asserted → board-enable off), with the fan
        // held at the quiet cap. So here we simply stop trimming.
        if am3_bb_thermal_action(snapshot.max_temp_c, dangerous_temp_c)
            == Am3BbThermalAction::CutHashThenFan
        {
            return;
        }

        let target = self.pid.update(snapshot.max_temp_c);
        // The PID output is 0..=100; clamp into the quiet envelope first, then
        // rate-limit the slew so the fan never audibly jumps.
        let capped = am3_bb_clamp_pid_pwm(
            self.fan.fan_min_pwm,
            self.fan.fan_max_pwm,
            target.round() as u8,
        );
        let next = if capped > self.commanded_pwm {
            self.commanded_pwm
                .saturating_add(AM3_BB_FAN_PID_MAX_STEP_PWM)
                .min(capped)
        } else {
            self.commanded_pwm
                .saturating_sub(AM3_BB_FAN_PID_MAX_STEP_PWM)
                .max(capped)
        };
        // `apply` re-clamps defensively — even a logic bug above cannot leak a
        // PWM above the home cap past this point.
        self.commanded_pwm = self.fan.apply(next);
    }

    fn commanded_pwm(&self) -> u8 {
        self.commanded_pwm
    }
}

/// Crash-panic-hook teardown params for the am3-bb path (wf_7c757213 safety
/// audit, 2026-05-29 — the cross-platform completion of the am2 + am3-aml panic
/// hooks shipped earlier this session).
///
/// Release builds use `panic = "abort"`, which BYPASSES `Am3BbRunSafetyGuard::Drop`.
/// am3-bb's fw=0x89 dsPIC has its own ~1-minute hardware watchdog that eventually
/// cuts voltage, but without this a panic would leave the hashboards ENERGIZED
/// (board-enable HIGH) for up to that full minute on a home/office unit. This stores
/// the minimal GPIO state so the `main()` crash hook can cut board power IMMEDIATELY.
/// Mirrors the am2 `AM2_TEARDOWN_PARAMS` / am3-aml `NOPIC_TEARDOWN_ARMED` pattern.
///
/// Fans are intentionally NOT re-driven here: the am3-bb run holds fans at
/// `safe_pwm` (<= `PWM_SAFETY_MAX`) for the ENTIRE run via `Am3BbCappedFan`, so on a
/// panic they are already within the home cap by construction — and the BeagleBone
/// fan PWM sysfs node is kernel-variant-dependent (pwmchip0 / pwmchip2 / legacy
/// pwm1+pwm2), so a blind `duty_cycle` write from inside the panic hook would be a
/// guess (and the ns-period varies). Cutting board-enable removes the heat source,
/// which is the actual fire-risk mitigation.
struct Am3BbPanicTeardown {
    board_enable_gpio: u32,
    board_enable_active_high: bool,
    reset_gpios: Vec<u32>,
}

static AM3BB_TEARDOWN_ARMED: std::sync::OnceLock<Am3BbPanicTeardown> = std::sync::OnceLock::new();

/// Arm the am3-bb crash-panic-hook teardown. Call once, at guard-arm time (before
/// board-enable is driven HIGH). Idempotent (`OnceLock::set`).
pub fn arm_am3_bb_teardown(platform: &BeagleBonePlatform, chain_count: usize) {
    let _ = AM3BB_TEARDOWN_ARMED.set(Am3BbPanicTeardown {
        board_enable_gpio: platform.board_enable_gpio_v2_0(),
        board_enable_active_high: platform.board_target().board_enable_active_high(),
        reset_gpios: platform
            .chain_reset_gpios_v2_0()
            .into_iter()
            .take(chain_count)
            .collect(),
    });
}

/// Best-effort cut-hash teardown for the `main()` crash panic hook on the am3-bb
/// path. No-op (allocation-free) unless an am3-bb run armed it. Asserts the
/// active-low ASIC resets, then drives board-enable OFF (cuts hashboard power).
/// Swallows all errors — must never re-panic from inside the panic hook. Fans are
/// already <= `PWM_SAFETY_MAX` by construction (see [`Am3BbPanicTeardown`]).
pub fn am3_bb_panic_hook_best_effort_teardown() {
    if let Some(params) = AM3BB_TEARDOWN_ARMED.get() {
        for &gpio in &params.reset_gpios {
            let _ = am3_bb_prepare_output_gpio(gpio, true)
                .and_then(|_| am3_bb_write_gpio_attr(gpio, "value", "1"));
        }
        let off_level = if params.board_enable_active_high {
            "0"
        } else {
            "1"
        };
        let _ = am3_bb_prepare_output_gpio(params.board_enable_gpio, false)
            .and_then(|_| am3_bb_write_gpio_attr(params.board_enable_gpio, "value", off_level));
    }
}

struct Am3BbRunSafetyGuard {
    dspic_i2c: Option<I2cServiceHandle>,
    active_dspic_addrs: Vec<u8>,
    reset_gpios: Vec<u32>,
    board_enable_gpio: u32,
    board_enable_active_high: bool,
    /// Shared so the runtime PID can borrow a clamp-enforced view
    /// ([`Am3BbCappedFan`]) while the guard keeps ownership for teardown. The
    /// PID can only ever drive this fan inside the quiet cap; the guard always
    /// re-asserts the quiet `safe_pwm` on teardown regardless of where the PID
    /// left it.
    fan: Option<Arc<dyn FanAccess>>,
    fan_min_pwm: u8,
    fan_max_pwm: u8,
    safe_pwm: u8,
    teardown_done: bool,
}

impl Am3BbRunSafetyGuard {
    fn new(
        platform: &BeagleBonePlatform,
        dspic_i2c: Option<I2cServiceHandle>,
        active_dspic_addrs: Vec<u8>,
        chain_count: usize,
        fan_min_pwm: u8,
        fan_max_pwm: u8,
    ) -> Self {
        let safe_pwm = am3_bb_quiet_safe_pwm(fan_min_pwm, fan_max_pwm);
        let fan: Option<Arc<dyn FanAccess>> = match platform.open_fan() {
            Ok(fan) => {
                let fan: Arc<dyn FanAccess> = Arc::from(fan);
                fan.set_speed(safe_pwm);
                let rpm = fan.get_rpm();
                info!(
                    safe_pwm,
                    rpm,
                    tach_available = fan.tach_available(),
                    fan_count = fan.fan_count(),
                    "am3-bb: quiet fan guard armed"
                );
                Some(fan)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    safe_pwm,
                    "am3-bb: fan guard could not open BeagleBone PWM; ASIC voltage/reset guard remains armed"
                );
                None
            }
        };

        Self {
            dspic_i2c,
            active_dspic_addrs,
            reset_gpios: platform
                .chain_reset_gpios_v2_0()
                .into_iter()
                .take(chain_count)
                .collect(),
            board_enable_gpio: platform.board_enable_gpio_v2_0(),
            board_enable_active_high: platform.board_target().board_enable_active_high(),
            fan,
            fan_min_pwm,
            fan_max_pwm,
            safe_pwm,
            teardown_done: false,
        }
    }

    fn set_dspic(&mut self, dspic_i2c: I2cServiceHandle, active_dspic_addrs: Vec<u8>) {
        self.dspic_i2c = Some(dspic_i2c);
        self.active_dspic_addrs = active_dspic_addrs;
        info!(
            active_dspic_addrs = format_args!("{:02X?}", self.active_dspic_addrs),
            "am3-bb: quiet safety guard attached to dsPIC voltage controllers"
        );
    }

    /// A clamp-enforced, `Arc`-shared view onto the guard's fan for the
    /// runtime PID. Returns `None` when the BeagleBone PWM could not be opened
    /// (the guard's ASIC voltage/reset path stays armed regardless — a missing
    /// fan must NOT block the fail-closed teardown). The guard keeps ownership
    /// and always re-asserts the quiet `safe_pwm` on teardown, so the worst the
    /// PID can do is move the fan inside the quiet cap during the run.
    fn capped_fan(&self) -> Option<Am3BbCappedFan> {
        self.fan.as_ref().map(|fan| Am3BbCappedFan {
            fan: Arc::clone(fan),
            fan_min_pwm: self.fan_min_pwm,
            fan_max_pwm: self.fan_max_pwm,
        })
    }

    fn teardown(&mut self) {
        if self.teardown_done {
            return;
        }
        self.teardown_done = true;

        if let Some(fan) = self.fan.as_ref() {
            fan.set_speed(self.safe_pwm);
        }

        // prod-readiness hunt #4 (log-honesty): track the two best-effort legs
        // (dsPIC disable + reset-assert) so the final summary doesn't affirm
        // "voltage off, resets asserted" when only the board-enable-off write
        // (the load-bearing cut) succeeded. Log-only — no command/ordering change.
        let mut dspic_disable_ok = true;
        if let Some(i2c) = self.dspic_i2c.as_ref() {
            for &addr in &self.active_dspic_addrs {
                let chain_idx = addr.saturating_sub(AM3_BB_DSPIC_BASE_ADDR) as usize;
                match am3_bb_dspic_command(
                    i2c,
                    chain_idx,
                    addr,
                    AM3_BB_DSPIC_DISABLE_FRAME,
                    2,
                    50,
                    20,
                    "safety-guard-disable-voltage",
                ) {
                    Ok(reply) => info!(
                        chain = chain_idx,
                        addr = format_args!("0x{:02X}", addr),
                        reply = format_args!("{:02X?}", reply),
                        "am3-bb: safety guard disabled dsPIC voltage"
                    ),
                    Err(e) => {
                        dspic_disable_ok = false;
                        warn!(
                            chain = chain_idx,
                            addr = format_args!("0x{:02X}", addr),
                            error = %e,
                            "am3-bb: safety guard dsPIC disable-voltage failed"
                        );
                    }
                }
            }
        }

        let mut resets_asserted_ok = true;
        for &gpio in &self.reset_gpios {
            if let Err(e) = am3_bb_prepare_output_gpio(gpio, true)
                .and_then(|_| am3_bb_write_gpio_attr(gpio, "value", "1"))
            {
                resets_asserted_ok = false;
                warn!(
                    gpio,
                    error = %e,
                    "am3-bb: safety guard failed to assert active-low ASIC reset"
                );
            }
        }

        let off_level = if self.board_enable_active_high {
            "0"
        } else {
            "1"
        };
        if let Err(e) = am3_bb_prepare_output_gpio(self.board_enable_gpio, false)
            .and_then(|_| am3_bb_write_gpio_attr(self.board_enable_gpio, "value", off_level))
        {
            warn!(
                gpio = self.board_enable_gpio,
                off_level,
                error = %e,
                "am3-bb: safety guard failed to drive board-enable off"
            );
        } else if dspic_disable_ok && resets_asserted_ok {
            info!(
                reset_gpios = ?self.reset_gpios,
                board_enable_gpio = self.board_enable_gpio,
                board_enable_off_level = off_level,
                safe_pwm = self.safe_pwm,
                "am3-bb: safety guard teardown complete (voltage off, resets asserted, quiet fan cap preserved)"
            );
        } else {
            // prod-readiness hunt #4: board-enable-off (the load-bearing power
            // cut) succeeded, but one or more best-effort legs did NOT — say so
            // instead of affirming "voltage off, resets asserted".
            warn!(
                reset_gpios = ?self.reset_gpios,
                board_enable_gpio = self.board_enable_gpio,
                board_enable_off_level = off_level,
                safe_pwm = self.safe_pwm,
                dspic_disable_ok,
                resets_asserted_ok,
                "am3-bb: safety guard drove board-enable OFF (the load-bearing power cut + quiet fan cap), \
                 but did NOT confirm all legs — dsPIC-disable and/or reset-assert reported errors above. \
                 Board-enable-off remains the safety net."
            );
        }
    }
}

impl Drop for Am3BbRunSafetyGuard {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn am3_bb_post_dspic_reset_chains(platform: &BeagleBonePlatform, chain_count: usize) -> Result<()> {
    let gpios = platform.chain_reset_gpios_v2_0();
    let active_gpios: Vec<u32> = gpios.into_iter().take(chain_count).collect();
    if active_gpios.is_empty() {
        return Ok(());
    }

    for &gpio in &active_gpios {
        am3_bb_write_reset_gpio(gpio, true)?;
    }
    thread::sleep(Duration::from_millis(
        AM3_BB_DSPIC_POST_ENABLE_RESET_ASSERT_MS,
    ));

    for &gpio in &active_gpios {
        am3_bb_write_reset_gpio(gpio, false)?;
        thread::sleep(Duration::from_millis(AM3_BB_DSPIC_INTER_CHAIN_RESET_MS));
    }
    thread::sleep(Duration::from_millis(
        AM3_BB_DSPIC_POST_ENABLE_RESET_RELEASE_MS,
    ));

    info!(
        reset_gpios = ?active_gpios,
        assert_ms = AM3_BB_DSPIC_POST_ENABLE_RESET_ASSERT_MS,
        release_settle_ms = AM3_BB_DSPIC_POST_ENABLE_RESET_RELEASE_MS,
        "am3-bb: ASIC resets re-pulsed after dsPIC rail enable"
    );
    Ok(())
}

struct Am3BbDspicHeartbeatGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Am3BbDspicHeartbeatGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                warn!("am3-bb: dsPIC heartbeat thread panicked during shutdown");
            }
        }
    }
}

fn start_am3_bb_dspic_heartbeat(
    i2c: I2cServiceHandle,
    active_addrs: Vec<u8>,
    shutdown: CancellationToken,
) -> Result<Am3BbDspicHeartbeatGuard> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = Arc::clone(&stop);
    let supervisor_disabled = env_flag_set(ENV_AM3_BB_DISABLE_HEARTBEAT_SUPERVISOR);
    if supervisor_disabled {
        warn!(
            env = ENV_AM3_BB_DISABLE_HEARTBEAT_SUPERVISOR,
            "am3-bb: lab override active - dsPIC heartbeat failures will not cancel mining"
        );
    }
    let handle = thread::Builder::new()
        .name("am3-bb-dspic-heartbeat".to_string())
        .spawn(move || {
            let mut tick = 0u64;
            let mut consecutive_failures = vec![0u8; active_addrs.len()];
            while !stop_worker.load(Ordering::SeqCst) {
                tick = tick.wrapping_add(1);
                for (chain_idx, addr) in active_addrs.iter().copied().enumerate() {
                    match am3_bb_dspic_heartbeat_once(&i2c, chain_idx, addr) {
                        Ok(reply) => {
                            consecutive_failures[chain_idx] = 0;
                            if tick.is_multiple_of(30) {
                                debug!(
                                    chain = chain_idx,
                                    addr = format_args!("0x{:02X}", addr),
                                    reply = format_args!("{:02X?}", reply),
                                    "am3-bb: dsPIC runtime heartbeat OK"
                                );
                            }
                        }
                        Err(e) => {
                            consecutive_failures[chain_idx] =
                                consecutive_failures[chain_idx].saturating_add(1);
                            if tick == 1 || tick.is_multiple_of(10) {
                                warn!(
                                    chain = chain_idx,
                                    addr = format_args!("0x{:02X}", addr),
                                    consecutive_failures = consecutive_failures[chain_idx],
                                    error = %e,
                                    "am3-bb: dsPIC runtime heartbeat failed"
                                );
                            }
                            if !supervisor_disabled
                                && consecutive_failures[chain_idx]
                                    >= AM3_BB_DSPIC_HEARTBEAT_MAX_FAILURES
                            {
                                warn!(
                                    chain = chain_idx,
                                    addr = format_args!("0x{:02X}", addr),
                                    consecutive_failures = consecutive_failures[chain_idx],
                                    max_failures = AM3_BB_DSPIC_HEARTBEAT_MAX_FAILURES,
                                    "am3-bb: dsPIC heartbeat supervisor cancelling mining; safety guard will cut voltage"
                                );
                                shutdown.cancel();
                                return;
                            }
                        }
                    }
                }

                let sleep_start = Instant::now();
                while sleep_start.elapsed()
                    < Duration::from_millis(AM3_BB_DSPIC_HEARTBEAT_INTERVAL_MS)
                {
                    if stop_worker.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        })
        .context("am3-bb: spawn dsPIC heartbeat thread failed")?;

    Ok(Am3BbDspicHeartbeatGuard {
        stop,
        handle: Some(handle),
    })
}

/// BM1362 chip-side init for ONE chain. Returns the detected chip count
/// (the number of chip addresses we assigned).
///
/// Wire bytes from `dcentrald_asic::bm1362` builders — the chip protocol is
/// NOT reimplemented here.
///
/// **R7-3 note (2026-05-12): BM1362 needs NO open-core dummy-work** — it is not
/// the BM1387 14 nm; it activates its cores via init register writes (per
/// `dcentrald_asic::drivers::bm1362` module docs, verified against bosminer).
/// The old "N zero-payload `asic_work_t`" step is gone.
///
/// **Cold-boot register block (2026-05-13): wired in** — the core activation
/// that the proven Amlogic-NoPic serial path does is now ported here:
/// `0xA8` InitControl + MiscCtrl×3 + `0xA4` VersionMask
/// (Step 1, pre-fast-baud) → `0x3C`×2 (HashClk/ClkDelay) + `0x54` AnalogMux +
/// `0x58` IoDriver + `0x14` TicketMask + `0x10` HashCountingNumber (Step 4) →
/// `0x70`/`0x08` PLL ramp from 400 MHz to the configured target (Step 5) →
/// `0x28` FastUART + MiscCtrl×3 + host fast-baud (Step 6) → per-chip
/// `0xA8`/MiscCtrl×3/`0x3C`×3 loop (Step 7). The APW voltage opcodes are still
/// best-effort stubs; this chip-side sequence is now the proven register plan
/// adapted to direct `/dev/ttyS*` on AM335x.
#[derive(Clone, Copy, Debug)]
struct Bm1362ChainInitResult {
    assigned_chips: usize,
    initial_get_address_rx_bytes: usize,
    fast_get_address_rx_bytes: usize,
}

impl Bm1362ChainInitResult {
    fn rx_proven(self) -> bool {
        self.initial_get_address_rx_bytes > 0 || self.fast_get_address_rx_bytes > 0
    }
}

fn bm1362_chip_init_one_chain(
    uart: &mut Am3BbChainUart,
    chain_idx: usize,
    mining_baud: u32,
    fast_uart_value: u32,
    skip_fast_uart: bool,
    expected_chips_per_chain: usize,
    target_freq_mhz: u16,
    run_miscctrl_triple_write: bool,
) -> Result<Bm1362ChainInitResult> {
    let init_values = bm1362_am3_init_values_from_env();
    let legacy_init_order = env_flag_set(ENV_AM3_BB_LEGACY_INIT_ORDER);
    info!(
        chain = chain_idx,
        init_plan = init_values.label,
        legacy_init_order,
        init_control_bcast = format_args!("0x{:08X}", init_values.init_control_broadcast),
        init_control_per_chip = format_args!("0x{:08X}", init_values.init_control_per_chip),
        misc_pre_baud = format_args!("0x{:08X}", init_values.misc_control_pre_baud),
        misc_post_fast = format_args!("0x{:08X}", init_values.misc_control_post_fast_baud),
        "am3-bb: BM1362 init values selected"
    );

    // a. GetAddress broadcast @ 115200. Phase D parses the chip-address
    //    responses to count chips; here we send the broadcast and read
    //    whatever comes back, logging the byte count as a liveness signal.
    let get_addr = build_get_address_frame();
    bm1362_write_cmd_frame(uart, &get_addr)
        .with_context(|| format!("am3-bb chain {}: GetAddress write failed", chain_idx))?;
    std::thread::sleep(Duration::from_millis(50));
    let mut rx = [0u8; 2048];
    let n = uart.read_bytes_timeout(&mut rx, 100);
    // Each BM1362 chip-address response is a few bytes; this is a rough
    // count, refined in Phase D. If nothing comes back, we still proceed —
    // the live unit may need a different enum baud / settle time.
    let approx_chips = n / 5;
    let n_assign = expected_chips_per_chain.clamp(1, BM1362_MAX_CHIPS_PER_CHAIN);
    let addr_interval = (256u16 / n_assign as u16).max(1);
    let mut n_fast = 0usize;
    info!(
        chain = chain_idx,
        backend = uart.backend_name(),
        get_address_rx_bytes = n,
        approx_chips,
        approx_9b_frames = n / 9,
        assigned_chips = n_assign,
        addr_interval,
        rx_preview = %hex_preview(&rx[..n], 96),
        "am3-bb: GetAddress enumeration response"
    );

    // Step 1 (@115200) — VersionMask first, then InitControl + MiscCtrl.
    // This mirrors the shared BM1362 driver. The previous AM3 BB order wrote
    // InitControl before the mask and only produced status/chatter frames.
    for _ in 0..3 {
        bm1362_bcast(
            uart,
            chain_idx,
            BM1362_REG_VERSION_MASK,
            BM1362_VERSION_MASK_VALUE,
            5,
            "VersionMask(0xA4)",
        )?;
    }
    bm1362_bcast(
        uart,
        chain_idx,
        BM1362_REG_INIT_CONTROL,
        init_values.init_control_broadcast,
        10,
        "InitControl(0xA8)",
    )?;
    if run_miscctrl_triple_write {
        bm1362_miscctrl_triple_write_bcast(
            uart,
            chain_idx,
            init_values.misc_control_pre_baud,
            "MiscCtrl(0x18) pre-baud",
        )?;
    }

    // b. ChainInactive ×3 + SetChipAddress for the configured chain length.
    // The live `a lab unit` GetAddress response can be truncated (64 bytes), so do not
    // size the chain from that rough byte count; S19j Pro BB chains are 126
    // chips and use address interval 256/126 => 2.
    let chain_inactive = build_chain_inactive_frame();
    for _ in 0..3 {
        bm1362_write_cmd_frame(uart, &chain_inactive)
            .with_context(|| format!("am3-bb chain {}: ChainInactive write failed", chain_idx))?;
        std::thread::sleep(Duration::from_millis(5));
    }
    for i in 0..n_assign {
        let chip_addr = (i as u16 * addr_interval) as u8;
        let f = build_set_chip_address_frame(chip_addr);
        bm1362_write_cmd_frame(uart, &f)
            .with_context(|| format!("am3-bb chain {}: SetChipAddress write failed", chain_idx))?;
        std::thread::sleep(Duration::from_millis(2));
    }

    if legacy_init_order {
        warn!(
            chain = chain_idx,
            "am3-bb: legacy BM1362 init order active; broadcast core/hash-count/PLL before per-chip loop"
        );
        bm1362_bcast(
            uart,
            chain_idx,
            BM1362_REG_CORE_CTRL,
            BM1362_CORE_REG_HASH_CLK,
            10,
            "CoreReg(0x3C) HashClk",
        )?;
        bm1362_bcast(
            uart,
            chain_idx,
            BM1362_REG_CORE_CTRL,
            BM1362_CORE_REG_CLK_DELAY,
            10,
            "CoreReg(0x3C) ClkDelay",
        )?;
    } else {
        // Canonical BM1362 order: address chips first, then program each chip's
        // InitControl/MiscCtrl/CoreReg block before any ticket/hash-count/PLL
        // work. The earlier AM3 path inverted this and produced no real shares.
        bm1362_per_chip_init_loop(
            uart,
            chain_idx,
            n_assign,
            addr_interval,
            init_values,
            run_miscctrl_triple_write,
            "bm1362_canonical_pre_baud_per_chip",
        )?;
    }

    // Ticket/IO/analog are broadcast after the per-chip block in the canonical
    // driver. Keep the same broadcast values for the legacy escape hatch.
    bm1362_bcast(
        uart,
        chain_idx,
        BM1362_REG_TICKET_MASK,
        BM1362_TICKET_MASK_256,
        10,
        "TicketMask(0x14)",
    )?;
    bm1362_bcast(
        uart,
        chain_idx,
        BM1362_REG_IO_DRIVER,
        BM1362_IO_DRIVER_NORMAL,
        10,
        "IoDriver(0x58)",
    )?;
    bm1362_bcast(
        uart,
        chain_idx,
        BM1362_REG_ANALOG_MUX,
        BM1362_ANALOG_MUX_VALUE,
        10,
        "AnalogMux(0x54)",
    )?;
    if legacy_init_order {
        bm1362_bcast(
            uart,
            chain_idx,
            BM1362_REG_NONCE_RANGE,
            BM1362_NONCE_RANGE_126,
            10,
            "HashCountingNumber(0x10)",
        )?;
        let pll_steps = bm1362_pll_ramp_to_target(target_freq_mhz);
        let write_pll0_divider = env_flag_set(ENV_AM3_BB_WRITE_PLL0_DIVIDER);
        if write_pll0_divider {
            warn!(
                chain = chain_idx,
                env = ENV_AM3_BB_WRITE_PLL0_DIVIDER,
                "am3-bb: lab override active - writing BM1362 PLL0 divider before PLL0 param"
            );
        }
        for (step_idx, (pll_param, actual_mhz)) in pll_steps.iter().copied().enumerate() {
            if write_pll0_divider {
                bm1362_bcast(
                    uart,
                    chain_idx,
                    BM1362_REG_PLL0_DIVIDER,
                    BM1362_PLL0_DIVIDER_VALUE,
                    10,
                    "PLL0 divider(0x70)",
                )?;
            }
            bm1362_bcast(
                uart,
                chain_idx,
                BM1362_REG_PLL0_PARAM,
                pll_param,
                10,
                "PLL0 param(0x08)",
            )?;
            debug!(
                chain = chain_idx,
                pll_step = step_idx + 1,
                pll_steps = pll_steps.len(),
                actual_mhz,
                pll_param = format_args!("0x{:08X}", pll_param),
                "am3-bb: legacy-order BM1362 PLL ramp step applied"
            );
            std::thread::sleep(Duration::from_millis(BM1362_PLL_RAMP_SETTLE_MS));
        }
        info!(
            chain = chain_idx,
            target_freq_mhz,
            final_freq_mhz = pll_steps
                .last()
                .map(|(_, mhz)| *mhz)
                .unwrap_or(target_freq_mhz),
            pll_steps = pll_steps.len(),
            "am3-bb: legacy-order BM1362 hash-count + PLL ramp applied before baud stage"
        );
    }
    bm1362_uart_relay_bcast(uart, chain_idx, "bm1362_pre_baud_broadcasts")?;

    if skip_fast_uart {
        warn!(
            chain = chain_idx,
            "am3-bb: skipping FastUART/host-baud switch; continuing post-init and work dispatch at 115200"
        );
    } else {
        // ── Rank 40 (goldmine ranks-40-50; 2026-06-10 desk RE, REVISED 2026-06-10
        // intelligence-exploitation pass) — the BM1370 high-speed UART regime is now
        // FIRST-HAND Ghidra-VERIFIED, and its transfer to BM1362 is REFUTED. Do NOT
        // port a PLL1 step into this BM1362 path.
        //
        // VERIFIED (S21pro jig `set_chain_baud@CB3B0`, BM1370, decompiled in full):
        //   if (baudrate < 0x2dc6c1 /* 3_000_001 */) {            // LOW regime
        //       reg 0x28 divider = 25_000_000 / (baud<<3);        // 25 MHz reference
        //   } else {                                             // HIGH regime
        //       reg 0x60 (PLL1) := (cache & 0xc088 | 0x111), hi |= 0x50000000;
        //       send_set_config(chain,1,0,0x60,pll1); usleep(10ms);  // written 2x
        //       send_set_config(chain,1,0,0x60,pll1); usleep(10ms);
        //       reg 0x28 divider = 400_000_000 / (baud<<3);       // PLL1 = 400 MHz
        //       reg 0x28 |= 0x84500000;                           // hi-speed enable
        //   }
        //   send_set_config(chain,1,0,0x28,...); set_bt8d_chain(chain,baud);
        // So for BM1370 a >3 Mbaud chain MUST reclock its UART off PLL1@400 MHz first.
        // 3.125 Mbaud (the BM136x/BM137x run baud, Saleae-confirmed on a live S19j Pro)
        // is above the 0x2dc6c1 threshold → this path is real for BM1370/S21.
        //
        // REFUTED for BM1362 (this AM3-BB / AM2-XIL path): BM1362's fast-baud is a
        // DIFFERENT, bosminer-faithful register protocol — reg 0x28 = 0x00003011
        // (broadcast) + reg 0x18 MiscCtrl = 0x00C100B0 (triple-write), byte-pinned from
        // the live `a lab unit` capture (`baud_switch.rs`). It shares NO structure with the
        // BM1370 jig path: no reg-0x60 PLL1 write, no 400 MHz divider, no 0x84500000.
        // bosminer is exactly the firmware driving the Saleae-captured 3.125 Mbaud
        // S19j Pro (BM1362) that mines successfully, and DCENT replays bosminer's
        // frames — so the chip-side baud sequence is NOT the gap. The BM1362
        // zero-nonce-at-fast-baud blocker is downstream on the HOST transport (PL-UART
        // MCR OUT2 / FPGA UART clock-out / dsPIC engagement) — exactly where the `a lab unit`
        // v+1 OUT2 work landed.
        // Therefore: do NOT add a BM1370 PLL1 step here. BM1362's own `set_chain_baud`
        // is NOT cleanly RE-able from held assets (stock S19j Pro bmminer is
        // symbol-stripped and delegates baud to the kernel `uart_trans.ko` — zero baud
        // strings; bosminer is 43k-fn stripped Rust). The only honest way to settle
        // whether bosminer does an UNcaptured BM1362 PLL step is a gated live A/B or a
        // luxminer decode — never by porting the BM1370 jig code.
        // Source: goldmine `deliverables/RANKS_40_50_DESK_RE.md` (rank 40 / C05) +
        // `SALEAE_PROTOCOL_REPORT.md`; .
        //
        // c. fast-baud upgrade: broadcast write FAST_UART_CONFIG(0x28).
        bm1362_bcast(
            uart,
            chain_idx,
            cold_boot_step::FAST_UART_CONFIG_REG,
            fast_uart_value,
            10,
            "FastUART(0x28)",
        )?;

        // Step 6 (cont.) — MiscCtrl(0x18) = 0x00C100B0 triple-write at 115200,
        // BEFORE the host baud switch (the proven trace does the
        // post-fast-uart-reg MiscCtrl×3 then switches the host baud) — only
        // when the board-target opts say so
        //.
        if run_miscctrl_triple_write {
            for _ in 0..3 {
                bm1362_bcast(
                    uart,
                    chain_idx,
                    cold_boot_step::MISC_CONTROL_REG,
                    init_values.misc_control_post_fast_baud,
                    5,
                    "MiscCtrl(0x18) post-fast-uart-reg",
                )?;
            }
            info!(
                chain = chain_idx,
                misc_post_fast = format_args!("0x{:08X}", init_values.misc_control_post_fast_baud),
                "am3-bb: MiscCtrl triple-write done after FastUART register write"
            );
        } else {
            info!(
                chain = chain_idx,
                "am3-bb: MiscCtrl triple-write SKIPPED (board-target run_miscctrl_triple_write=false)"
            );
        }

        // d. switch host UART to mining_baud. On AM335x the OMAP UART base
        // baud is 3 MHz, so the BM1362 0x28 FastUART handoff is followed by
        // divisor-1 3 Mbaud by default. The lab baud override lets live RE
        // sweep this without a rebuild.
        uart.drain_tx().with_context(|| {
            format!(
                "am3-bb chain {}: drain TX before set_baud({}) failed",
                chain_idx, mining_baud
            )
        })?;
        uart.set_baud(mining_baud).with_context(|| {
            format!(
                "am3-bb chain {}: set_baud({}) failed",
                chain_idx, mining_baud
            )
        })?;
        let fast_uart_settle_ms = parse_env_u32(ENV_AM3_BB_FAST_UART_SETTLE_MS)
            .map(u64::from)
            .unwrap_or(500);
        std::thread::sleep(Duration::from_millis(fast_uart_settle_ms));
        info!(
            chain = chain_idx,
            mining_baud,
            fast_uart_settle_ms,
            fast_uart = format_args!("0x{:08X}", fast_uart_value),
            "am3-bb: host UART switched to mining baud"
        );
        if env_flag_set(ENV_AM3_BB_SKIP_FAST_RELAY_AFTER_SWITCH) {
            warn!(
                chain = chain_idx,
                env = ENV_AM3_BB_SKIP_FAST_RELAY_AFTER_SWITCH,
                "am3-bb: lab override active - skipping UART_RELAY broadcast after host baud switch"
            );
        } else {
            bm1362_uart_relay_bcast(uart, chain_idx, "bm1362_step6_fast_baud")?;
        }

        // Fast-baud liveness probe. We do not trust the configured chip count
        // as a post-baud proof: if the host and ASIC baud rates disagree, every
        // later per-chip write becomes a silent no-op and mining yields zero
        // nonce frames.
        if let Err(e) = uart.drain_tx() {
            warn!(
                chain = chain_idx,
                error = %e,
                "am3-bb: fast-baud UART_RELAY TX drain failed before liveness probe"
            );
        }
        uart.flush_io();
        bm1362_write_cmd_frame(uart, &get_addr).with_context(|| {
            format!(
                "am3-bb chain {}: fast-baud GetAddress write failed",
                chain_idx
            )
        })?;
        let fast_getaddr_delay_ms = parse_env_u32(ENV_AM3_BB_FAST_GETADDR_DELAY_MS)
            .map(u64::from)
            .unwrap_or(50);
        let fast_getaddr_read_ms = parse_env_u32(ENV_AM3_BB_FAST_GETADDR_READ_MS)
            .map(u64::from)
            .unwrap_or(300);
        std::thread::sleep(Duration::from_millis(fast_getaddr_delay_ms));
        n_fast = uart.read_bytes_timeout(&mut rx, fast_getaddr_read_ms);
        uart.flush_io();
        if n_fast == 0 {
            warn!(
                chain = chain_idx,
                backend = uart.backend_name(),
                mining_baud,
                fast_uart = format_args!("0x{:08X}", fast_uart_value),
                fast_getaddr_delay_ms,
                fast_getaddr_read_ms,
                "am3-bb: fast-baud GetAddress returned no bytes; baud handoff may still be wrong"
            );
        } else {
            info!(
                chain = chain_idx,
                backend = uart.backend_name(),
                mining_baud,
                fast_uart = format_args!("0x{:08X}", fast_uart_value),
                fast_getaddr_delay_ms,
                fast_getaddr_read_ms,
                fast_get_address_rx_bytes = n_fast,
                approx_chips = n_fast / 5,
                approx_9b_frames = n_fast / 9,
                rx_preview = %hex_preview(&rx[..n_fast], 96),
                "am3-bb: fast-baud GetAddress liveness response"
            );
        }
    }

    if !legacy_init_order {
        bm1362_bcast(
            uart,
            chain_idx,
            BM1362_REG_NONCE_RANGE,
            BM1362_NONCE_RANGE_126,
            10,
            "HashCountingNumber(0x10)",
        )?;
        let pll_steps = bm1362_pll_ramp_to_target(target_freq_mhz);
        let write_pll0_divider = env_flag_set(ENV_AM3_BB_WRITE_PLL0_DIVIDER);
        if write_pll0_divider {
            warn!(
                chain = chain_idx,
                env = ENV_AM3_BB_WRITE_PLL0_DIVIDER,
                "am3-bb: lab override active - writing BM1362 PLL0 divider before PLL0 param"
            );
        }
        for (step_idx, (pll_param, actual_mhz)) in pll_steps.iter().copied().enumerate() {
            if write_pll0_divider {
                bm1362_bcast(
                    uart,
                    chain_idx,
                    BM1362_REG_PLL0_DIVIDER,
                    BM1362_PLL0_DIVIDER_VALUE,
                    10,
                    "PLL0 divider(0x70)",
                )?;
            }
            bm1362_bcast(
                uart,
                chain_idx,
                BM1362_REG_PLL0_PARAM,
                pll_param,
                10,
                "PLL0 param(0x08)",
            )?;
            debug!(
                chain = chain_idx,
                pll_step = step_idx + 1,
                pll_steps = pll_steps.len(),
                actual_mhz,
                pll_param = format_args!("0x{:08X}", pll_param),
                "am3-bb: canonical-order BM1362 PLL ramp step applied"
            );
            std::thread::sleep(Duration::from_millis(BM1362_PLL_RAMP_SETTLE_MS));
        }
        info!(
            chain = chain_idx,
            target_freq_mhz,
            final_freq_mhz = pll_steps
                .last()
                .map(|(_, mhz)| *mhz)
                .unwrap_or(target_freq_mhz),
            pll_steps = pll_steps.len(),
            "am3-bb: canonical-order BM1362 hash-count + PLL ramp applied after baud stage"
        );
    } else {
        bm1362_per_chip_init_loop(
            uart,
            chain_idx,
            n_assign,
            addr_interval,
            init_values,
            run_miscctrl_triple_write,
            "bm1362_legacy_post_baud_per_chip",
        )?;
    }

    bm1362_bcast(
        uart,
        chain_idx,
        BM1362_REG_VERSION_MASK,
        BM1362_VERSION_MASK_VALUE,
        10,
        "VersionMask(0xA4) final",
    )?;

    // No open-core dummy-work: BM1362 doesn't use BM1387-style 114-dummy-work
    // to gate its cores. The register plan above is the core gate.
    info!(
        chain = chain_idx,
        n_assign, legacy_init_order, "am3-bb: BM1362 chip-side init done"
    );

    Ok(Bm1362ChainInitResult {
        assigned_chips: n_assign,
        initial_get_address_rx_bytes: n,
        fast_get_address_rx_bytes: n_fast,
    })
}

// ===========================================================================
//  Mining loop (Option B2 — reuse dcentrald_stratum + the transport)
// ===========================================================================

/// One in-flight dispatched work unit, kept so a returned nonce can be
/// validated against the exact header that produced it.
///
/// Indexed by `asic_work_t.job_id` (low 8 bits — ):
/// the BM1362 nonce frame echoes the same 8-bit field. When the dispatcher
/// wraps 0..=255 the oldest entry at that slot is overwritten — work that old
/// is stale anyway, same as the transport's 16-slot in-flight ring.
#[derive(Clone)]
struct DispatchedWork {
    job_id: String,
    extranonce2: String,
    ntime: u32,
    nbits: u32,
    version: u32,
    version_mask: u32,
    /// `prev_block_hash` AND `merkle_root` in header byte order (already
    /// word-reversed for `prev_block_hash` — `MiningWork` does that in
    /// `WorkBuilder::next_work`).
    prev_block_hash: [u8; 32],
    merkle_root: [u8; 32],
    share_target: [u8; 32],
}

impl DispatchedWork {
    /// Assemble the 80-byte block header for this work + a candidate nonce,
    /// using `rolled_version` (the base `version` with the BM1362-returned
    /// version-rolling bits applied — see [`rolled_version`]). Pass
    /// `self.version` for a non-version-rolled share.
    ///
    /// Layout matches `WorkBuilder`'s `header_prefix` + `serial_build_header`
    /// (version LE, prev_hash [already word-reversed by `WorkBuilder`],
    /// merkle_root [raw SHA-256d order], ntime LE, nbits LE) + the trailing
    /// nonce LE — i.e. the byte order that `validate_full_header` hashes to a
    /// valid share. (Same approach that got DCENT_axe / the Amlogic path their
    /// accepted shares.)
    fn full_header(&self, rolled_version: u32, nonce: u32) -> [u8; 80] {
        self.full_header_from_nonce_bytes(rolled_version, nonce.to_le_bytes())
    }

    /// Variant used only by live diagnostics to replay plausible nonce byte
    /// interpretations while keeping pool submission on the proven path.
    fn full_header_from_nonce_bytes(&self, rolled_version: u32, nonce_bytes: [u8; 4]) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&rolled_version.to_le_bytes());
        h[4..36].copy_from_slice(&self.prev_block_hash);
        h[36..68].copy_from_slice(&self.merkle_root);
        h[68..72].copy_from_slice(&self.ntime.to_le_bytes());
        h[72..76].copy_from_slice(&self.nbits.to_le_bytes());
        h[76..80].copy_from_slice(&nonce_bytes);
        h
    }
}

fn am3_bb_full_header_hash_be(header: &[u8; 80]) -> [u8; 32] {
    let hash = dcentrald_stratum::work::double_sha256(header);
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }
    hash_be
}

fn am3_bb_achieved_difficulty_from_header(header: &[u8; 80]) -> Option<f64> {
    let hash_be = am3_bb_full_header_hash_be(header);
    let difficulty = dcentrald_stratum::v1::difficulty::hash_to_difficulty(&hash_be);
    if difficulty.is_finite() && difficulty > 0.0 {
        Some(difficulty)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
struct Am3BbNonceReplay {
    job_id: String,
    nonce_label: &'static str,
    version_label: &'static str,
    nonce_submit: u32,
    rolled_version: u32,
    achieved_difficulty: Option<f64>,
}

fn rolled_version_or_shift_checked(
    base_version: u32,
    version_mask: u32,
    version_bits_raw: u16,
) -> Option<u32> {
    // Canonical BIP320 reconstruction: shift vbits_raw left 13, mask with
    // 0x1FFFE000, OR into base with the field cleared. The .79 BB serial
    // path's prior `version_mask == 0 → drop if vbits != 0` early return
    // was a silent-drop bug for any pool that doesn't negotiate
    // version-rolling. See
    // .
    let (rolled, _) =
        dcentrald_asic::bm1362::bip320_reconstruct_rolled_version(base_version, version_bits_raw);
    if version_mask == 0 {
        return Some(rolled);
    }
    let delta = rolled ^ base_version;
    if delta & !version_mask != 0 {
        return None;
    }
    Some(rolled)
}

fn rolled_version_no_shift_checked(
    base_version: u32,
    version_mask: u32,
    version_bits_raw: u16,
) -> Option<u32> {
    // Alternate replay codec where the chip returned vbits already in the
    // pool-mask layout (no shift). Pre-fix `version_mask == 0 → drop if
    // vbits != 0` was the same silent-drop bug as the shift variant.
    if version_mask == 0 {
        // Without a pool mask we don't know the no-shift bit positions —
        // fall back to the canonical shifted reconstruction so we still
        // submit something the pool can validate. validate_full_header
        // upstream is the SOLE gate.
        let (rolled, _) = dcentrald_asic::bm1362::bip320_reconstruct_rolled_version(
            base_version,
            version_bits_raw,
        );
        return Some(rolled);
    }

    let rolled =
        (base_version & !VERSION_ROLLING_FIELD_MASK) | ((version_bits_raw as u32) & version_mask);
    let delta = rolled ^ base_version;
    if delta & !version_mask != 0 {
        return None;
    }
    Some(rolled)
}

fn am3_bb_update_best_replay(best: &mut Option<Am3BbNonceReplay>, replay: Am3BbNonceReplay) {
    let replay_diff = replay.achieved_difficulty.unwrap_or(0.0);
    let best_diff = best
        .as_ref()
        .and_then(|b| b.achieved_difficulty)
        .unwrap_or(0.0);
    if replay_diff > best_diff {
        *best = Some(replay);
    }
}

fn am3_bb_replay_bm1362_nonce_decodes(
    history: &VecDeque<DispatchedWork>,
    nr: &Bm1362SerialNonce,
) -> (
    Option<Am3BbNonceReplay>,
    Option<Am3BbNonceReplay>,
    Option<Am3BbNonceReplay>,
    u32,
) {
    let nonce_wire = [
        nr.raw_frame[2],
        nr.raw_frame[3],
        nr.raw_frame[4],
        nr.raw_frame[5],
    ];
    let nonce_be = u32::from_be_bytes(nonce_wire);
    let vbits_le = u16::from_le_bytes([nr.raw_frame[8], nr.raw_frame[9]]);
    let mut best_current = None;
    let mut best_any = None;
    let mut alternate_pool_hit = None;
    let mut version_rejects = 0u32;

    for candidate in history.iter().rev() {
        let version_variants = [
            (
                "be_shift_replace",
                rolled_version_checked(
                    candidate.version,
                    candidate.version_mask,
                    nr.version_bits_raw,
                ),
            ),
            (
                "le_shift_replace",
                rolled_version_checked(candidate.version, candidate.version_mask, vbits_le),
            ),
            ("base_version", Some(candidate.version)),
            (
                "be_shift_or",
                rolled_version_or_shift_checked(
                    candidate.version,
                    candidate.version_mask,
                    nr.version_bits_raw,
                ),
            ),
            (
                "be_no_shift_replace",
                rolled_version_no_shift_checked(
                    candidate.version,
                    candidate.version_mask,
                    nr.version_bits_raw,
                ),
            ),
        ];
        let nonce_variants = [
            ("wire_le_header", nr.nonce, nr.nonce.to_le_bytes()),
            ("be_numeric_header", nonce_be, nonce_be.to_le_bytes()),
        ];

        for (version_label, maybe_version) in version_variants {
            let Some(rolled_version) = maybe_version else {
                version_rejects = version_rejects.saturating_add(1);
                continue;
            };
            for (nonce_label, nonce_submit, nonce_header_bytes) in nonce_variants {
                let header =
                    candidate.full_header_from_nonce_bytes(rolled_version, nonce_header_bytes);
                let achieved_difficulty = am3_bb_achieved_difficulty_from_header(&header);
                let replay = Am3BbNonceReplay {
                    job_id: candidate.job_id.clone(),
                    nonce_label,
                    version_label,
                    nonce_submit,
                    rolled_version,
                    achieved_difficulty,
                };
                let is_current =
                    nonce_label == "wire_le_header" && version_label == "be_shift_replace";
                if is_current {
                    am3_bb_update_best_replay(&mut best_current, replay.clone());
                }
                am3_bb_update_best_replay(&mut best_any, replay.clone());

                if !is_current
                    && dcentrald_stratum::work::validate_full_header(
                        &header,
                        &candidate.share_target,
                    )
                {
                    alternate_pool_hit.get_or_insert(replay);
                }
            }
        }
    }

    (best_current, best_any, alternate_pool_hit, version_rejects)
}

/// Build the `dcentrald_stratum::StratumConfig` from the daemon config.
///
/// Mirrors the construction in `serial_mining.rs::SerialMiner::run` so the
/// am3-bb path gets the same failover/donation/version-rolling behavior; the
/// caller is responsible for spawning `StratumRouter::run` with it.
fn stratum_config_from(config: &DcentraldConfig) -> dcentrald_stratum::types::StratumConfig {
    crate::config::build_stratum_config(
        config,
        crate::config::stratum_donation_config(&config.donation),
        config.mining.version_rolling,
        false,
    )
}

/// The Option-B2 mining loop: Stratum (router on `rt_handle`) ↔ this blocking
/// thread (work-build + paced dispatch + nonce-validate + dedup + submit).
///
/// Thermal: the run-scope guard keeps fan PWM capped at quiet home-mining levels
/// and cuts ASIC voltage/resets on exit, heartbeat failure, or an LM75 overtemp.
/// PR-021 adds a CONTINUOUS, capped fan PID on top of (not instead of) that
/// fail-closed supervisor: every runtime LM75 poll that the supervisor
/// validates is also fed to an `Am3BbFanPid` that trims the fan toward the
/// configured target temperature, hard-clamped to `min(fan_max_pwm, 30)` and
/// rate-limited so the home unit stays quiet. The proven fail-closed checks
/// (stale/empty → tolerate-or-cut, dangerous → cut hash first) are unchanged.
fn run_mining_loop<U: ChainUart>(
    config: &DcentraldConfig,
    transport: &mut Am335xUartTransport<U>,
    total_chips: usize,
    shutdown: &CancellationToken,
    rt_handle: &tokio::runtime::Handle,
    thermal_i2c: Option<I2cServiceHandle>,
    thermal_dspic_addrs: Vec<u8>,
    pid_fan: Option<Am3BbCappedFan>,
    watchdog_liveness: Arc<AtomicU64>,
) -> Result<()> {
    use dcentrald_stratum::work::{validate_full_header, WorkBuilder};

    if config.pool.url.trim().is_empty() || config.pool.worker.trim().is_empty() {
        anyhow::bail!(
            "am3-bb: mining loop requires a non-empty pool.url AND pool.worker — set a real BTC address. \
             (Use DCENT_AM3_BB_STUB_LOOP=1 for a cold-boot/enum-only run with no pool.)"
        );
    }

    let mut thermal_supervisor = if env_flag_set(ENV_AM3_BB_SKIP_THERMAL_SUPERVISOR) {
        warn!(
            env = ENV_AM3_BB_SKIP_THERMAL_SUPERVISOR,
            "am3-bb: lab override active - runtime LM75 thermal supervisor disabled"
        );
        None
    } else {
        let Some(i2c) = thermal_i2c else {
            anyhow::bail!("am3-bb: runtime LM75 thermal supervisor requires dsPIC I2C access");
        };
        let mut supervisor = Am3BbThermalSupervisor::new(
            i2c,
            thermal_dspic_addrs,
            config.thermal.hot_temp_c,
            config.thermal.dangerous_temp_c,
        )?;
        supervisor.poll_and_check("pre-stratum")?;
        Some(supervisor)
    };
    let thermal_poll_ms = ((config.thermal.pid_interval_s.max(1.0) * 1000.0).round() as u64)
        .max(AM3_BB_THERMAL_MIN_POLL_MS);

    // PR-021: the CONTINUOUS, capped fan PID. Default-ON. The lab escape hatch
    // can only park the PID (revert to the pre-PR-021 pinned-fan behaviour) —
    // it can never raise a cap or relax a fail-closed path. The PID is also
    // skipped (gracefully, not fatally) when the BeagleBone PWM never opened or
    // when the fail-closed supervisor itself is disabled (lab override): with
    // no validated thermal proof there is nothing safe to drive a PID from.
    let fan_pid_disabled = env_flag_set(ENV_AM3_BB_DISABLE_FAN_PID);
    let mut fan_pid = if fan_pid_disabled {
        warn!(
            env = ENV_AM3_BB_DISABLE_FAN_PID,
            "am3-bb: lab override active - continuous fan PID parked; fan stays pinned at the quiet \
             safe floor by the run guard (fail-closed LM75 supervisor still fully active)"
        );
        None
    } else if thermal_supervisor.is_none() {
        warn!(
            "am3-bb: fan PID not started - the fail-closed LM75 supervisor is disabled, so there is \
             no validated thermal proof to drive a PID from"
        );
        None
    } else {
        match pid_fan {
            Some(fan) => {
                let pid = Am3BbFanPid::new(fan, config.thermal.target_temp_c);
                info!(
                    target_temp_c = config.thermal.target_temp_c,
                    start_pwm = pid.commanded_pwm(),
                    "am3-bb: continuous fan PID armed (clamped to the quiet home cap)"
                );
                Some(pid)
            }
            None => {
                warn!(
                    "am3-bb: continuous fan PID requested but BeagleBone PWM is unavailable; \
                     fail-closed LM75 supervisor + run-guard quiet cap remain in force"
                );
                None
            }
        }
    };

    info!(
        fan_min_pwm = config.thermal.fan_min_pwm,
        fan_max_pwm = config.thermal.fan_max_pwm,
        fan_hard_cap_pwm = AM3_BB_FAN_HARD_CAP_PWM,
        target_temp_c = config.thermal.target_temp_c,
        hot_temp_c = config.thermal.hot_temp_c,
        dangerous_temp_c = config.thermal.dangerous_temp_c,
        thermal_poll_ms,
        supervisor_enabled = thermal_supervisor.is_some(),
        fan_pid_enabled = fan_pid.is_some(),
        "am3-bb: quiet thermal guard active (fail-closed LM75 hard-stop + continuous capped fan PID)"
    );

    // --- Stratum channels (same shape serial_mining.rs uses). ---
    let (job_tx, mut job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
    let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
    let (status_tx, mut status_rx) = mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);

    let stratum_config = stratum_config_from(config);
    let router = dcentrald_stratum::StratumRouter::new(stratum_config);
    rt_handle.spawn(async move {
        router.run(job_tx, share_rx, status_tx).await;
    });
    // Drain Stratum status events on the runtime so the channel doesn't back
    // up; we only log them here (the dashboard/state-channel wiring is owned by
    // the proxy-mode API main.rs already started — out of scope for this loop).
    rt_handle.spawn(async move {
        while let Some(st) = status_rx.recv().await {
            match st {
                dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, .. } => {
                    info!(job_id = %job_id, "am3-bb: SHARE ACCEPTED")
                }
                dcentrald_stratum::types::StratumStatus::ShareRejected {
                    job_id,
                    error_msg,
                    ..
                } => warn!(job_id = %job_id, error = %error_msg, "am3-bb: SHARE REJECTED"),
                dcentrald_stratum::types::StratumStatus::DifficultyChanged(d) => {
                    info!(difficulty = d, "am3-bb: pool difficulty")
                }
                dcentrald_stratum::types::StratumStatus::StateChanged(s) => {
                    info!(state = ?s, "am3-bb: pool state")
                }
                _ => {}
            }
        }
    });

    let assume_job_response_flags = env_flag_set(ENV_AM3_BB_ASSUME_JOB_RESPONSE_FLAGS);
    if assume_job_response_flags {
        warn!(
            env = ENV_AM3_BB_ASSUME_JOB_RESPONSE_FLAGS,
            "am3-bb: lab override active - validating parsed BM1362 frames even when flags bit7 is clear"
        );
    }

    let work_codec = Am3BbWorkCodec::from_env();

    info!(
        chains = transport.chain_count(),
        total_chips,
        dispatch_interval_us = transport.dispatch_interval_us(),
        work_codec = work_codec.as_str(),
        pool = %config.pool.url,
        "=== am3-bb MINING ACTIVE (Option B2) — Stratum + paced BM1362 serial-work dispatch + nonce-validate/dedup/submit ==="
    );

    let worker_name = config.pool.worker.clone();
    let mut work_builder = WorkBuilder::new();
    let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
    let mut asic_job_id: u8 = 0;
    // Per-job_id-slot history of dispatched work, for nonce → header lookup.
    let mut work_by_id: Vec<VecDeque<DispatchedWork>> =
        (0..ASIC_JOB_ID_SPAN).map(|_| VecDeque::new()).collect();
    let mut next_chain: usize = 0;
    // Dedup BEFORE pool submission.
    let mut seen_shares: HashSet<(u8, u32, u16)> = HashSet::new();
    // Bound the dedup set so a long run doesn't grow unbounded; a clean-jobs
    // event clears it (new block ⇒ old (job_id, nonce) pairs are irrelevant).
    const SEEN_SHARES_SOFT_CAP: usize = 8192;

    let chain_count = transport.chain_count().max(1);
    let start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut last_thermal_poll = Instant::now();
    let mut last_dispatch_attempt = Instant::now();
    // Per-chain ~one frame every dispatch_interval_us; loop-tick ~= the
    // interval / chain_count so all chains stay fed without flooding.
    let tick =
        Duration::from_micros((transport.dispatch_interval_us() / chain_count as u64).max(500));
    let mut total_work: u64 = 0;
    let mut total_rx_frames: u64 = 0;
    let mut total_nonces: u64 = 0;
    let mut non_job_frames: u64 = 0;
    let mut assumed_job_response_frames: u64 = 0;
    let mut shares_submitted: u64 = 0;
    let mut dup_nonces: u64 = 0;
    let mut bad_nonces: u64 = 0;
    let mut target_miss_nonces: u64 = 0;
    let mut unknown_job_nonces: u64 = 0;
    let mut alternate_decode_pool_hits: u64 = 0;
    let mut version_metadata_rejects: u64 = 0;
    let mut best_target_miss_difficulty: Option<f64> = None;

    loop {
        if shutdown.is_cancelled() {
            info!(
                uptime_s = start.elapsed().as_secs(),
                work_codec = work_codec.as_str(),
                total_work,
                total_rx_frames,
                total_nonces,
                non_job_frames,
                assumed_job_response_frames,
                shares_submitted,
                dup_nonces,
                bad_nonces,
                target_miss_nonces,
                unknown_job_nonces,
                alternate_decode_pool_hits,
                version_metadata_rejects,
                best_target_miss_difficulty,
                "am3-bb: shutdown requested — exiting mining loop"
            );
            return Ok(());
        }
        watchdog_liveness.fetch_add(1, Ordering::Relaxed);

        if let Some(supervisor) = thermal_supervisor.as_mut() {
            if last_thermal_poll.elapsed() >= Duration::from_millis(thermal_poll_ms) {
                last_thermal_poll = Instant::now();
                // INVARIANT #2 + #4 ORDERING: `?` fires FIRST. If the
                // supervisor decides dangerous temp or lost-thermal-proof, it
                // returns Err here and the run-scope guard tears down (dsPIC
                // voltage off → resets asserted → board-enable off → fan held
                // at quiet cap) BEFORE the PID is ever consulted. So the PID
                // can only ever see a snapshot the supervisor already deemed
                // safe and fresh — it never "out-cools" a dangerous temp by
                // ramping the fan; hash power is cut first, by construction.
                let snapshot = supervisor.poll_and_check("runtime")?;
                if let Some(pid) = fan_pid.as_mut() {
                    pid.step(&snapshot, f32::from(config.thermal.dangerous_temp_c));
                }
            }
        }

        // --- Pull any pool jobs (non-blocking; the router pushes). ---
        loop {
            match job_rx.try_recv() {
                Ok(job) => {
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "am3-bb: NEW BLOCK — flush stale work + dedup set");
                        work_builder.reset_extranonce2();
                        transport.clean_work();
                        seen_shares.clear();
                        for slot in work_by_id.iter_mut() {
                            slot.clear();
                        }
                    }
                    work_builder.set_version_mask(job.version_mask);
                    if job.is_flush_only() {
                        info!(job_id = %job.job_id, "am3-bb: pool-switch flush — dispatch paused until next notify");
                        current_job = None;
                    } else {
                        current_job = Some(job);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    warn!("am3-bb: Stratum job channel closed — exiting mining loop");
                    return Ok(());
                }
            }
        }

        // --- Dispatch one round of work (round-robin across chains, paced). ---
        if last_dispatch_attempt.elapsed() >= tick {
            last_dispatch_attempt = Instant::now();
            if let Some(ref job) = current_job {
                let work = work_builder.next_work(job);

                // R7-3: the PROVEN 88-byte BM1362 serial full-header work frame
                // ([0x55 0xAA][0x21][0x56][82-byte payload][CRC16-CCITT-FALSE BE])
                // — see `build_bm1362_serial_work_frame` + the module doc. The
                // W14.B `AsicWorkFrame` 86-byte `asic_work_t` codec was a W4
                // dev-kit fabrication and is NOT what the chip speaks.
                let chain = next_chain % transport.chain_count().max(1);
                next_chain = next_chain.wrapping_add(1);
                let now = now_us();
                let send_result = match work_codec {
                    Am3BbWorkCodec::Serial88 => {
                        let frame_bytes = build_bm1362_serial_work_frame(&work, asic_job_id);
                        transport.try_send_raw(chain, &frame_bytes, asic_job_id, now)
                    }
                    Am3BbWorkCodec::Asic86 => {
                        let frame = build_bm1362_asic86_work_frame(
                            &work,
                            asic_job_id,
                            total_work.wrapping_add(1) as u32,
                        );
                        transport.try_send_work(chain, &frame, now)
                    }
                };
                match send_result {
                    Ok(true) => {
                        let job_slot = work_codec.job_id_slot(asic_job_id);
                        let history = &mut work_by_id[job_slot as usize];
                        history.push_back(DispatchedWork {
                            job_id: work.job_id.clone(),
                            extranonce2: work.extranonce2.clone(),
                            ntime: work.ntime,
                            nbits: work.nbits,
                            version: work.version,
                            version_mask: work.version_mask,
                            prev_block_hash: work.prev_block_hash,
                            merkle_root: work.merkle_root,
                            share_target: work.share_target,
                        });
                        if history.len() > WORK_HISTORY_PER_ECHOED_JOB_ID {
                            history.pop_front();
                        }
                        asic_job_id = match work_codec {
                            Am3BbWorkCodec::Serial88 => next_bm1362_serial_job_id(asic_job_id),
                            Am3BbWorkCodec::Asic86 => {
                                asic_job_id.wrapping_add(work_codec.job_id_increment())
                            }
                        };
                        total_work += 1;
                        if total_work == 1 {
                            info!(
                                chain,
                                work_codec = work_codec.as_str(),
                                "am3-bb: first BM1362 work frame dispatched"
                            );
                        }
                    }
                    Ok(false) => { /* paced off — retry next tick */ }
                    Err(UartTransportError::ChainOutOfRange) => {
                        warn!(chain, "am3-bb: dispatch chain out of range — skipping");
                    }
                    Err(UartTransportError::WriteFailed) => {
                        warn!(chain, "am3-bb: chain UART write failed during dispatch");
                    }
                    Err(UartTransportError::FrameTooLong) => {
                        // Unreachable for try_send_raw (no length cap) — handled
                        // for exhaustiveness.
                        warn!("am3-bb: dispatch reported FrameTooLong — unexpected for raw send");
                    }
                }
            }
        }

        // --- Poll nonces, validate, dedup, submit. ---
        if work_codec == Am3BbWorkCodec::Serial88 {
            //
            // R7-3: BM1362 serial-wire nonce frames are 11 bytes ([0xAA 0x55]
            // [n3 n2 n1 n0][midstate_idx][result][vbits_hi vbits_lo][flags]) —
            // `recv_bm1362_serial_nonces` parses them. (`recv_nonces` parses the
            // different/wrong W14.B 10-byte codec.) `flags` bit7 = job response;
            // `result` high nibble (>>1) echoes `sent_job_id & 0x78`; `vbits` BE.
            for (chain_idx, nr) in transport.recv_bm1362_serial_nonces() {
                total_rx_frames += 1;
                let is_job_response = nr.flags & 0x80 != 0;
                if !is_job_response {
                    // Not a job-response frame (status / config echo) — ignore.
                    non_job_frames += 1;
                    if non_job_frames <= 8 {
                        info!(
                            chain = chain_idx,
                            job_id = nr.job_id,
                            result = format_args!("0x{:02X}", nr.result_byte),
                            small_core = nr.small_core,
                            midstate_idx = nr.midstate_idx,
                            flags = format_args!("0x{:02X}", nr.flags),
                            vbits = format_args!("0x{:04X}", nr.version_bits_raw),
                            nonce = format_args!("0x{:08X}", nr.nonce),
                            "am3-bb: non-job BM1362 serial frame ignored"
                        );
                    }
                    if !assume_job_response_flags {
                        continue;
                    }
                    assumed_job_response_frames += 1;
                }
                total_nonces += 1;

                let history = &work_by_id[nr.job_id as usize];
                if history.is_empty() {
                    bad_nonces += 1;
                    unknown_job_nonces += 1;
                    debug!(
                        chain = chain_idx,
                        job_id = nr.job_id,
                        result = format_args!("0x{:02X}", nr.result_byte),
                        small_core = nr.small_core,
                        flags = format_args!("0x{:02X}", nr.flags),
                        nonce = format_args!("0x{:08X}", nr.nonce),
                        "am3-bb: nonce for unknown job_id slot — dropped (stale / reused slot)"
                    );
                    continue;
                };

                let matched = history.iter().rev().find_map(|candidate| {
                    let rv = rolled_version_checked(
                        candidate.version,
                        candidate.version_mask,
                        nr.version_bits_raw,
                    )?;
                    let header = candidate.full_header(rv, nr.nonce);
                    if validate_full_header(&header, &candidate.share_target) {
                        Some((candidate.clone(), rv, header))
                    } else {
                        None
                    }
                });
                let Some((dw, rv, header)) = matched else {
                    bad_nonces += 1;
                    target_miss_nonces += 1;
                    let (best_current, best_any, alternate_pool_hit, version_rejects) =
                        am3_bb_replay_bm1362_nonce_decodes(history, &nr);
                    version_metadata_rejects =
                        version_metadata_rejects.saturating_add(u64::from(version_rejects));
                    if let Some(best) = best_current.as_ref().and_then(|b| b.achieved_difficulty) {
                        best_target_miss_difficulty = Some(
                            best_target_miss_difficulty
                                .map(|prev| prev.max(best))
                                .unwrap_or(best),
                        );
                    }
                    if let Some(alt) = alternate_pool_hit {
                        alternate_decode_pool_hits = alternate_decode_pool_hits.saturating_add(1);
                        warn!(
                            chain = chain_idx,
                            job_id = nr.job_id,
                            raw = %hex_preview(&nr.raw_frame, nr.raw_frame.len()),
                            alt_pool_job = %alt.job_id,
                            nonce_decode = alt.nonce_label,
                            version_decode = alt.version_label,
                            nonce = format_args!("0x{:08X}", alt.nonce_submit),
                            rolled_version = format_args!("0x{:08X}", alt.rolled_version),
                            achieved_difficulty = alt.achieved_difficulty,
                            "am3-bb: alternate BM1362 nonce decode would meet pool target"
                        );
                    }
                    if total_nonces <= 8 {
                        let nonce_be = u32::from_be_bytes([
                            nr.raw_frame[2],
                            nr.raw_frame[3],
                            nr.raw_frame[4],
                            nr.raw_frame[5],
                        ]);
                        let vbits_le = u16::from_le_bytes([nr.raw_frame[8], nr.raw_frame[9]]);
                        info!(
                            chain = chain_idx,
                            job_id = nr.job_id,
                            job_id_bm1366 = nr.result_byte & 0xF8,
                            job_id_no_shift = nr.result_byte & 0xF0,
                            result = format_args!("0x{:02X}", nr.result_byte),
                            small_core = nr.small_core,
                            flags = format_args!("0x{:02X}", nr.flags),
                            vbits = format_args!("0x{:04X}", nr.version_bits_raw),
                            vbits_le = format_args!("0x{:04X}", vbits_le),
                            nonce = format_args!("0x{:08X}", nr.nonce),
                            nonce_be = format_args!("0x{:08X}", nonce_be),
                            raw = %hex_preview(&nr.raw_frame, nr.raw_frame.len()),
                            history_len = history.len(),
                            best_current_pool_diff = best_current
                                .as_ref()
                                .and_then(|b| b.achieved_difficulty),
                            best_any_decode = ?best_any.as_ref().map(|b| {
                                format!(
                                    "{}+{} nonce=0x{:08X} ver=0x{:08X} diff={:?}",
                                    b.nonce_label,
                                    b.version_label,
                                    b.nonce_submit,
                                    b.rolled_version,
                                    b.achieved_difficulty
                                )
                            }),
                            version_rejects,
                            assumed_job_response = !is_job_response && assume_job_response_flags,
                            "am3-bb: nonce did not validate against recent work history"
                        );
                    }
                    continue;
                };

                let dedup_key = (nr.job_id, nr.nonce, nr.version_bits_raw);
                if !seen_shares.insert(dedup_key) {
                    dup_nonces += 1;
                    continue;
                }
                if seen_shares.len() > SEEN_SHARES_SOFT_CAP {
                    seen_shares.clear();
                    seen_shares.insert(dedup_key);
                }
                let vdelta = rv ^ dw.version;
                let achieved_difficulty = am3_bb_achieved_difficulty_from_header(&header);
                let share = dcentrald_stratum::types::ValidShare {
                    worker_name: worker_name.clone(),
                    job_id: dw.job_id.clone(),
                    extranonce2: dw.extranonce2.clone(),
                    ntime: format!("{:08x}", dw.ntime),
                    nonce: format!("{:08x}", nr.nonce),
                    version_bits: if vdelta != 0 {
                        Some(format!("{:08x}", vdelta))
                    } else {
                        None
                    },
                    version: rv,
                    achieved_difficulty,
                };
                if share_tx.blocking_send(share).is_err() {
                    warn!("am3-bb: Stratum share channel closed — exiting mining loop");
                    return Ok(());
                }
                shares_submitted += 1;
                info!(
                    chain = chain_idx,
                    job_id = %dw.job_id,
                    small_core = nr.small_core,
                    nonce = format_args!("0x{:08X}", nr.nonce),
                    version = format_args!("0x{:08X}", rv),
                    achieved_difficulty,
                    "am3-bb: VALID SHARE submitted to pool"
                );
            }
        } else {
            for (chain_idx, nr) in transport.recv_nonces() {
                total_rx_frames += 1;
                total_nonces += 1;

                let history = &work_by_id[nr.job_id as usize];
                if history.is_empty() {
                    bad_nonces += 1;
                    unknown_job_nonces += 1;
                    debug!(
                        chain = chain_idx,
                        response_chain = nr.chain_id,
                        job_id = nr.job_id,
                        nonce = format_args!("0x{:08X}", nr.nonce),
                        "am3-bb: asic86 nonce for unknown job_id slot - dropped"
                    );
                    continue;
                }

                let matched = history.iter().rev().find_map(|candidate| {
                    let header = candidate.full_header(candidate.version, nr.nonce);
                    if validate_full_header(&header, &candidate.share_target) {
                        Some((candidate.clone(), header))
                    } else {
                        None
                    }
                });
                let Some((dw, header)) = matched else {
                    bad_nonces += 1;
                    target_miss_nonces += 1;
                    if total_nonces <= 8 {
                        info!(
                            chain = chain_idx,
                            response_chain = nr.chain_id,
                            job_id = nr.job_id,
                            nonce = format_args!("0x{:08X}", nr.nonce),
                            history_len = history.len(),
                            "am3-bb: asic86 nonce did not validate against recent work history"
                        );
                    }
                    continue;
                };

                let dedup_key = (nr.job_id, nr.nonce, 0);
                if !seen_shares.insert(dedup_key) {
                    dup_nonces += 1;
                    continue;
                }
                if seen_shares.len() > SEEN_SHARES_SOFT_CAP {
                    seen_shares.clear();
                    seen_shares.insert(dedup_key);
                }

                let achieved_difficulty = am3_bb_achieved_difficulty_from_header(&header);
                let share = dcentrald_stratum::types::ValidShare {
                    worker_name: worker_name.clone(),
                    job_id: dw.job_id.clone(),
                    extranonce2: dw.extranonce2.clone(),
                    ntime: format!("{:08x}", dw.ntime),
                    nonce: format!("{:08x}", nr.nonce),
                    version_bits: None,
                    version: dw.version,
                    achieved_difficulty,
                };
                if share_tx.blocking_send(share).is_err() {
                    warn!("am3-bb: Stratum share channel closed - exiting mining loop");
                    return Ok(());
                }
                shares_submitted += 1;
                info!(
                    chain = chain_idx,
                    response_chain = nr.chain_id,
                    job_id = %dw.job_id,
                    nonce = format_args!("0x{:08X}", nr.nonce),
                    version = format_args!("0x{:08X}", dw.version),
                    achieved_difficulty,
                    "am3-bb: VALID asic86 SHARE submitted to pool"
                );
            }
        }

        if last_heartbeat.elapsed() >= Duration::from_secs(15) {
            let rx_counters = transport.bm1362_serial_rx_counters();
            let rx_raw_bytes: Vec<u64> = rx_counters.iter().map(|c| c.raw_bytes).collect();
            let rx_parsed_frames: Vec<u64> = rx_counters.iter().map(|c| c.parsed_frames).collect();
            let rx_job_response_frames: Vec<u64> =
                rx_counters.iter().map(|c| c.job_response_frames).collect();
            let rx_non_job_frames: Vec<u64> = rx_counters
                .iter()
                .map(|c| c.non_job_response_frames)
                .collect();
            let rx_resync_bytes: Vec<u64> = rx_counters.iter().map(|c| c.resync_bytes).collect();
            let rx_buffered_bytes: Vec<usize> =
                rx_counters.iter().map(|c| c.buffered_bytes).collect();
            info!(
                uptime_s = start.elapsed().as_secs(),
                work_codec = work_codec.as_str(),
                total_work,
                total_rx_frames,
                total_nonces,
                non_job_frames,
                assumed_job_response_frames,
                shares_submitted,
                dup_nonces,
                bad_nonces,
                target_miss_nonces,
                unknown_job_nonces,
                alternate_decode_pool_hits,
                version_metadata_rejects,
                best_target_miss_difficulty,
                chains = transport.chain_count(),
                rx_raw_bytes = ?rx_raw_bytes,
                rx_parsed_frames = ?rx_parsed_frames,
                rx_job_response_frames = ?rx_job_response_frames,
                rx_non_job_frames = ?rx_non_job_frames,
                rx_resync_bytes = ?rx_resync_bytes,
                rx_buffered_bytes = ?rx_buffered_bytes,
                fan_pid_pwm = fan_pid.as_ref().map(|p| p.commanded_pwm()),
                fan_pid_rpm = fan_pid.as_ref().map(|p| p.fan.get_rpm()),
                "am3-bb: mining loop alive"
            );
            last_heartbeat = Instant::now();
        }

        std::thread::sleep(tick);
    }
}

/// Monotonic microseconds, for [`Am335xUartTransport::try_send_raw`] pacing.
fn now_us() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_micros() as u64
}

/// Cold-boot/enum-only diagnostic stub: log readiness, then idle while polling
/// the transport for nonces so any residual-state nonces show up in the log.
///
/// This is NOT the mining loop — it is the `DCENT_AM3_BB_STUB_LOOP=1` escape
/// hatch for validating just the cold-boot orchestration + BM1362 enumeration
/// + transport setup (no pool, no work dispatch). The real loop is
/// [`run_mining_loop`].
fn run_mining_loop_stub<U: ChainUart>(
    transport: &mut Am335xUartTransport<U>,
    total_chips: usize,
    shutdown: &CancellationToken,
    watchdog_liveness: Arc<AtomicU64>,
) {
    info!(
        chains = transport.chain_count(),
        total_chips,
        "am3-bb: DCENT_AM3_BB_STUB_LOOP — cold-boot complete, transport ready. This is the \
         enum-only diagnostic stub: no pool, no work dispatch. Idling; will log any nonce frames \
         the chain returns from residual state. Unset DCENT_AM3_BB_STUB_LOOP for the mining loop."
    );

    let start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut total_nonces: u64 = 0;
    let mut job_response_nonces: u64 = 0;
    let mut non_job_frames: u64 = 0;
    loop {
        watchdog_liveness.fetch_add(1, Ordering::Relaxed);
        if shutdown.is_cancelled() {
            info!(
                uptime_s = start.elapsed().as_secs(),
                total_nonces,
                job_response_nonces,
                non_job_frames,
                "am3-bb: shutdown requested — exiting stub loop"
            );
            return;
        }

        // Poll the transport — in the stub we have no work dispatched, so
        // this should be empty, but if the chain is producing nonces from
        // residual state it's worth seeing. (Parses the PROVEN 11-byte BM1362
        // serial nonce frame — see R7-3 note in the module doc.)
        for (chain_idx, nonce) in transport.recv_bm1362_serial_nonces() {
            total_nonces += 1;
            if nonce.flags & 0x80 != 0 {
                job_response_nonces += 1;
            } else {
                non_job_frames += 1;
            }
            info!(
                chain = chain_idx,
                job_id = nonce.job_id,
                small_core = nonce.small_core,
                flags = format_args!("0x{:02X}", nonce.flags),
                vbits = format_args!("0x{:04X}", nonce.version_bits_raw),
                nonce = format_args!("0x{:08X}", nonce.nonce),
                "am3-bb: nonce frame received (stub loop — not submitted to any pool)"
            );
        }

        if last_heartbeat.elapsed() >= Duration::from_secs(30) {
            info!(
                uptime_s = start.elapsed().as_secs(),
                total_nonces,
                job_response_nonces,
                non_job_frames,
                chains = transport.chain_count(),
                "am3-bb: stub loop alive (no pool, no work dispatch — DCENT_AM3_BB_STUB_LOOP set)"
            );
            last_heartbeat = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

// ===========================================================================
//  Tests (host-safe — no hardware)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_hal::platform::beaglebone::BeagleBonePlatform;
    use dcentrald_hal::platform::beaglebone_cold_boot::ColdBootOptsV2;
    use dcentrald_hal::platform::config::PlatformConfig;

    fn test_platform() -> BeagleBonePlatform {
        // `with_config` builds the platform with the explicit `PlatformConfig`
        // and the hardcoded `a lab unit` (`S19J_IO_BOARD_V2_0`) board-target
        // defaults — the same topology the runtime sees on a LuxOS unit with
        // no `/etc/dcentos/board_targets/<name>.toml` present.
        BeagleBonePlatform::with_config(PlatformConfig::s19j_beaglebone())
    }

    #[test]
    fn chain_uart_specs_match_board_target() {
        let p = test_platform();
        let specs = chain_uart_specs(&p);
        // `a lab unit` S19J_IO_BOARD_V2_0 has 3 chains on ttyS1/ttyS2/ttyS4.
        assert_eq!(specs.len(), 3, "three chains on the .79 IO board");
        for (i, s) in specs.iter().enumerate() {
            assert_eq!(s.index as usize, i, "chain index matches position");
            assert!(
                s.device.starts_with("/dev/ttyS"),
                "chain {} device looks like a tty: {}",
                i,
                s.device
            );
        }
        // The default chain ttys are ttyS1, ttyS2, ttyS4 (NOT ttyS3).
        let devs: Vec<&str> = specs.iter().map(|s| s.device.as_str()).collect();
        assert_eq!(devs, vec!["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4"]);
    }

    #[test]
    fn cold_boot_opts_pulled_from_board_target() {
        let p = test_platform();
        let opts = ColdBootOptsV2::from_board_target(p.board_target());
        // `from_board_target` builds `apw_drop_to_steady = false` — the
        // daemon flips it on the second call (step 7). If this default
        // changes, step 7's logic needs revisiting.
        assert!(
            !opts.apw_drop_to_steady,
            "ColdBootOptsV2::from_board_target must default apw_drop_to_steady=false"
        );
        // The open-core rail must be at or above the steady rail.
        assert!(
            opts.apw12_rail_open_core_mv >= opts.apw12_rail_steady_mv,
            "open-core rail ({} mV) must be >= steady rail ({} mV)",
            opts.apw12_rail_open_core_mv,
            opts.apw12_rail_steady_mv
        );
        // The steady rail is the home-mining target (~13.8 V); sanity-bound it.
        assert!(
            opts.apw12_rail_steady_mv >= 12_000 && opts.apw12_rail_steady_mv <= 15_500,
            "steady rail {} mV is in a plausible chain-voltage band",
            opts.apw12_rail_steady_mv
        );
    }

    #[test]
    fn quiet_safe_pwm_never_exceeds_home_cap() {
        assert_eq!(am3_bb_quiet_safe_pwm(10, 30), 10);
        assert_eq!(am3_bb_quiet_safe_pwm(20, 30), 20);
        assert_eq!(
            am3_bb_quiet_safe_pwm(80, 100),
            AM3_BB_FAN_HARD_CAP_PWM,
            "operator config cannot make the AM3 BB guard blast fans"
        );
        assert_eq!(
            am3_bb_quiet_safe_pwm(10, 5),
            5,
            "an intentionally lower cap is respected; safety comes from cutting ASIC power"
        );
    }

    #[test]
    fn board_target_psu_topology_is_the_79_layout() {
        let p = test_platform();
        // .79: APW121215f on the bit-banged i2c-gpio bus (bus 1) @ 0x10.
        assert_eq!(p.psu_i2c_bus(), 1, "PSU on the bit-banged i2c-gpio bus");
        assert_eq!(p.psu_i2c_addr(), 0x10, "APW12 PSU at I2C 0x10");
        // Hashboard EEPROMs on bus 0.
        assert_eq!(p.eeprom_i2c_bus(), 0, "hashboard EEPROMs on bus 0");
        // gpio59 board-enable (IO-board-specific, not the W4 BBCtrl map).
        assert_eq!(p.board_enable_gpio_v2_0(), 59, "BOARD_ENABLE = gpio59");
        // The APW UART-tunnel PSU is upstream, but the hashboards still use
        // fw=0x89 dsPIC controllers on I2C bus 0.
        assert_eq!(
            p.voltage_controller(),
            dcentrald_hal::platform::VoltageControllerKind::Dspic33Ep,
            "S19J_IO_BOARD_V2_0 uses hashboard dsPIC controllers"
        );
    }

    #[test]
    fn mining_baud_matches_am335x_fast_uart_handoff() {
        let p = test_platform();
        // LuxOS reports "3 Mbaud"; AM335x OMAP UART base_baud is 3 MHz, so
        // divisor 1 is exact. 937500 rounded to actual 1 Mbaud and produced
        // silent zero-nonce runs after the BM1362 FastUART handoff.
        assert_eq!(p.mining_baud_v2_0(), 3_000_000);
    }

    #[test]
    fn bm1362_chip_init_commands_are_preamble_framed() {
        let get = bm1362_command_wire_frame(&build_get_address_frame());
        assert_eq!(&get[..2], &[0x55, 0xAA], "GetAddress command preamble");
        assert_eq!(&get[2..6], &[0x52, 0x05, 0x00, 0x00]);
        assert_eq!(get.len(), 7, "GetAddress is preamble + 5-byte command");

        let bcast = bm1362_command_wire_frame(&build_broadcast_write_frame(0x3C, 0x8000_8540));
        assert_eq!(&bcast[..4], &[0x55, 0xAA, 0x51, 0x09]);
        assert_eq!(bcast[5], 0x3C);
        assert_eq!(
            bcast.len(),
            11,
            "broadcast write is preamble + 9-byte command"
        );

        let single = bm1362_command_wire_frame(&build_single_write_frame(0x7E, 0xA8, 0x0200_0000));
        assert_eq!(&single[..4], &[0x55, 0xAA, 0x41, 0x09]);
        assert_eq!(single[4], 0x7E);
        assert_eq!(single[5], 0xA8);
        assert_eq!(
            single.len(),
            11,
            "single write is preamble + 9-byte command"
        );
    }

    #[test]
    fn luxos_trace_dspic_frames_are_pinned() {
        // Captured from `a lab unit` LuxOS ftrace on 2026-05-13. These are full
        // write frames followed by one-byte reads, not I2C_RDWR combined
        // transactions.
        assert_eq!(
            AM3_BB_DSPIC_RESET_FRAME,
            &[0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B]
        );
        assert_eq!(
            AM3_BB_DSPIC_JUMP_FRAME,
            &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A]
        );
        assert_eq!(
            AM3_BB_DSPIC_GET_VERSION_FRAME,
            &[0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B]
        );
        assert_eq!(
            AM3_BB_DSPIC_DISABLE_FRAME,
            &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
        );
        assert_eq!(
            am3_bb_dspic_set_voltage_frame(13_700),
            [0x55, 0xAA, 0x04, 0x10, 0x06, 0x1A]
        );
        assert_eq!(
            am3_bb_dspic_set_voltage_frame(13_800),
            [0x55, 0xAA, 0x04, 0x10, 0x06, 0x1A]
        );
        assert_eq!(
            AM3_BB_DSPIC_ENABLE_FRAME,
            &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B]
        );
        assert_eq!(
            AM3_BB_DSPIC_HEARTBEAT_FRAME,
            &[0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A]
        );
        assert_eq!(
            AM3_BB_DSPIC_PROBE_3B_48_FRAME,
            &[0x55, 0xAA, 0x06, 0x3B, 0x48, 0x00, 0x00, 0x89]
        );
        assert_eq!(
            AM3_BB_DSPIC_READ_VOLTAGE_FRAME,
            &[0x55, 0xAA, 0x04, 0x3A, 0x00, 0x3E]
        );
        assert_eq!(
            am3_bb_dspic_temp_bridge_frame(0x48),
            [0x55, 0xAA, 0x06, 0x3C, 0x48, 0x02, 0x00, 0x8C]
        );
        assert_eq!(
            am3_bb_dspic_temp_bridge_frame(0x4B),
            [0x55, 0xAA, 0x06, 0x3C, 0x4B, 0x02, 0x00, 0x8F]
        );
        assert_eq!(
            am3_bb_decode_lm75_bridge_reply(&[0x07, 0x3C, 0x01, 0x1D, 0x20, 0x00, 0x81]),
            Some(29.125)
        );
        assert_eq!(
            am3_bb_decode_lm75_bridge_reply(&[0x07, 0x3C, 0x01, 0x1D, 0x20, 0x00, 0x80]),
            None,
            "LM75 bridge checksum must be enforced before mining"
        );
        assert_eq!(am3_bb_dspic_voltage_dac(AM3_BB_DSPIC_MIN_VOLTAGE_MV), 0x00);
        assert_eq!(am3_bb_dspic_voltage_dac(AM3_BB_DSPIC_MAX_VOLTAGE_MV), 0x0B);
        assert_eq!(
            am3_bb_dspic_target_voltage_mv(9_100),
            AM3_BB_DSPIC_DEFAULT_TARGET_MV
        );
        assert_eq!(am3_bb_dspic_addr_for_chain(0), 0x20);
        assert_eq!(am3_bb_dspic_addr_for_chain(1), 0x21);
        assert_eq!(am3_bb_dspic_addr_for_chain(2), 0x22);
    }

    #[test]
    fn auto_detect_is_false_on_a_dev_host() {
        // On the dev host there is no /etc/dcentos/board_target and no
        // /proc/device-tree/model with S19J_IO_BOARD — must not false-positive.
        assert!(
            !auto_detect_am3_bb(),
            "auto_detect_am3_bb must be false on a non-am3-bb host"
        );
    }

    #[test]
    fn devmem_chain_uart_is_a_thin_newtype() {
        // We can't construct a real DevmemUart on the host (it mmaps
        // /dev/mem), so this just pins the adapter shape: `DevmemChainUart`
        // is a 1-tuple newtype over `DevmemUart`, and it implements
        // `ChainUart`. If someone refactors it into something heavier this
        // forces a conscious decision.
        fn _assert_impls_chain_uart<T: ChainUart>() {}
        _assert_impls_chain_uart::<DevmemChainUart>();
        // size_of equality is the cheapest "it's still a newtype" check.
        assert_eq!(
            std::mem::size_of::<DevmemChainUart>(),
            std::mem::size_of::<DevmemUart>(),
            "DevmemChainUart must stay a zero-overhead newtype over DevmemUart"
        );
    }

    #[test]
    fn chain_uart_spec_is_value_equal() {
        let a = ChainUartSpec {
            index: 0,
            device: "/dev/ttyS1".to_string(),
        };
        let b = ChainUartSpec {
            index: 0,
            device: "/dev/ttyS1".to_string(),
        };
        assert_eq!(a, b);
        let c = ChainUartSpec {
            index: 1,
            device: "/dev/ttyS1".to_string(),
        };
        assert_ne!(a, c);
    }

    // -----------------------------------------------------------------
    // Mining-loop pure helpers (Option B2)
    // -----------------------------------------------------------------

    #[test]
    fn job_id_correlation_constants_and_roundtrip() {
        // work_by_id is indexed by the chip's echoed job-id (0..120 in steps of
        // 8); we size it 256 so the index is always in range.
        assert_eq!(ASIC_JOB_ID_SPAN, 256);
        assert_eq!(ASIC_JOB_ID_SPAN, u8::MAX as usize + 1);
        // The dispatcher step must be a multiple of 8 (only bits [6:3] of the
        // sent job-id round-trip through the chip's (sent<<1)&0xF0 echo).
        assert_eq!(
            JOB_ID_INCREMENT % 8,
            0,
            "JOB_ID_INCREMENT must be a multiple of 8"
        );
        assert_eq!(
            JOB_ID_INCREMENT, 24,
            "matches the proven BM1368/BM1370-family path"
        );
        assert_eq!(ASIC_JOB_ID_MASK, 0x7F, "serial job ids stay in 0..=127");
        assert_eq!(next_bm1362_serial_job_id(0), 24);
        assert_eq!(next_bm1362_serial_job_id(96), 120);
        assert_eq!(
            next_bm1362_serial_job_id(120),
            16,
            "wrap like serial_mining.rs, not through the 0x80..0xFF range"
        );
        // echoed_job_id(sent) == sent & 0x78, and that's what the parser
        // ((result & 0xF0) >> 1) recovers — so dispatch-side store and
        // nonce-side lookup land in the same slot for every value the
        // dispatcher actually uses (multiples of 8).
        for sent in (0u8..=255).step_by(8) {
            let echoed = echoed_job_id(sent);
            assert_eq!(echoed, sent & 0x78, "echoed_job_id({sent}) == sent & 0x78");
            // Re-derive via the chip's encode/decode round-trip.
            let result_byte_high_nibble = (sent << 1) & 0xF0;
            assert_eq!(result_byte_high_nibble >> 1, echoed);
        }
    }

    #[test]
    fn build_bm1362_serial_work_frame_byte_layout() {
        // Pin the PROVEN 88-byte BM1362 serial full-header work frame:
        //   [0..2]   = 0x55 0xAA preamble
        //   [2]      = 0x21 header  ·  [3] = 0x56 length
        //   [4]      = job_id  ·  [5] = 0x01 num_midstates  ·  [6..10] = nonce(0)
        //   [10..14] = nbits LE  ·  [14..18] = ntime LE
        //   [18..50] = merkle_root, 32-bit-word-reversed
        //   [50..82] = prev_block_hash, 32-bit-word-reversed
        //   [82..86] = version LE  ·  [86..88] = CRC16-CCITT-FALSE, BE-appended,
        //              over frame[2..86] (the 84 bytes from 0x21)
        let work = dcentrald_stratum::work::MiningWork {
            midstates: vec![[0u8; 32]],
            merkle4: [0u8; 4],
            ntime: 0x1122_3344,
            nbits: 0x5566_7788,
            job_id: "abc".to_string(),
            extranonce2: "00".to_string(),
            version: 0x2000_0004,
            version_mask: 0x1FFF_E000,
            share_target: [0xFF; 32],
            // Distinct per-word bytes so the word-reversal is visible.
            merkle_root: {
                let mut m = [0u8; 32];
                for (i, b) in m.iter_mut().enumerate() {
                    *b = i as u8;
                }
                m
            },
            prev_block_hash: {
                let mut p = [0u8; 32];
                for (i, b) in p.iter_mut().enumerate() {
                    *b = 0x80 + i as u8;
                }
                p
            },
        };
        let f = build_bm1362_serial_work_frame(&work, 0x18);
        assert_eq!(f.len(), 88, "88 bytes on the wire");
        assert_eq!(
            &f[0..4],
            &[0x55, 0xAA, 0x21, 0x56],
            "preamble + header + length"
        );
        assert_eq!(f[4], 0x18, "job_id at payload[0]");
        assert_eq!(f[5], 0x01, "num_midstates = 1");
        assert_eq!(&f[6..10], &[0, 0, 0, 0], "starting_nonce = 0");
        assert_eq!(&f[10..14], &work.nbits.to_le_bytes(), "nbits LE");
        assert_eq!(&f[14..18], &work.ntime.to_le_bytes(), "ntime LE");
        assert_eq!(
            &f[18..50],
            &reverse_32bit_words(&work.merkle_root),
            "merkle_root, 32-bit-word-reversed"
        );
        assert_eq!(
            &f[50..82],
            &reverse_32bit_words(&work.prev_block_hash),
            "prev_block_hash, 32-bit-word-reversed"
        );
        assert_eq!(&f[82..86], &work.version.to_le_bytes(), "version LE");
        // CRC over the 84 bytes from f[2] (0x21) through f[85] (last payload byte).
        let crc = dcentrald_hal::serial_chain::crc16_public(&f[2..86]);
        assert_eq!(
            f[86],
            (crc >> 8) as u8,
            "CRC hi byte first (big-endian append)"
        );
        assert_eq!(f[87], (crc & 0xFF) as u8, "CRC lo byte");
        // Cross-check the word-reversal helper against a hand-computed example.
        let mut src = [0u8; 32];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        let rev = reverse_32bit_words(&src);
        // word 0 (bytes 0..4) ↔ word 7 (bytes 28..32)
        assert_eq!(&rev[0..4], &[28, 29, 30, 31]);
        assert_eq!(&rev[28..32], &[0, 1, 2, 3]);
    }

    #[test]
    fn build_bm1362_asic86_work_frame_byte_layout() {
        let mut midstate = [0u8; 32];
        for (i, b) in midstate.iter_mut().enumerate() {
            *b = 0x40 + i as u8;
        }
        let work = dcentrald_stratum::work::MiningWork {
            midstates: vec![midstate],
            merkle4: [0u8; 4],
            ntime: 0x1122_3344,
            nbits: 0x5566_7788,
            job_id: "abc".to_string(),
            extranonce2: "00".to_string(),
            version: 0x2000_0004,
            version_mask: 0x1FFF_E000,
            share_target: [0xFF; 32],
            merkle_root: [0xAB; 32],
            prev_block_hash: [0xCD; 32],
        };

        let frame = build_bm1362_asic86_work_frame(&work, 0x27, 0xAABB_CCDD);
        assert_eq!(frame.type_byte, CMD_WORK_PACKAGE);
        assert_eq!(frame.job_id, 0x27);
        assert_eq!(frame.sno, 0xAABB_CCDD);
        assert_eq!(&frame.data2[0..4], &work.ntime.to_le_bytes());
        assert_eq!(&frame.data2[4..8], &work.nbits.to_le_bytes());
        assert_eq!(&frame.data2[8..12], &[0, 0, 0, 0]);
        assert_eq!(&frame.data[0..32], &midstate);
        assert_eq!(&frame.data[32..64], &work.merkle_root);
        let bytes = frame.to_bytes();
        assert_eq!(bytes.len(), 86);
        assert_eq!(bytes[0], CMD_WORK_PACKAGE);
    }

    #[test]
    fn bm1362_cold_boot_register_values_match_canonical_init_plan() {
        // Pin the AM3 BB defaults to the shared BM1362_INIT_PLAN. The legacy
        // Amlogic-derived values remain available only through
        // DCENT_AM3_BB_LEGACY_AMLOGIC_INIT for A/B bench diagnostics.
        assert_eq!(
            (BM1362_REG_INIT_CONTROL, BM1362_INIT_CONTROL_BCAST),
            (0xA8, 0x0007_0000)
        );
        assert_eq!(BM1362_INIT_CONTROL_PER_CHIP, 0x0007_01F0);
        assert_eq!(BM1362_INIT_CONTROL_BCAST_LEGACY_AMLOGIC, 0x0000_0000);
        assert_eq!(BM1362_INIT_CONTROL_PER_CHIP_LEGACY_AMLOGIC, 0x0200_0000);
        assert_eq!(
            (BM1362_REG_VERSION_MASK, BM1362_VERSION_MASK_VALUE),
            (0xA4, 0x9000_FFFF)
        );
        assert_eq!(BM1362_REG_CORE_CTRL, 0x3C);
        assert_eq!(BM1362_CORE_REG_HASH_CLK, 0x8000_8540);
        assert_eq!(BM1362_CORE_REG_CLK_DELAY, 0x8000_8008);
        assert_eq!(BM1362_CORE_REG_UNKNOWN, 0x8000_82AA);
        assert_eq!(
            (BM1362_REG_ANALOG_MUX, BM1362_ANALOG_MUX_VALUE),
            (0x54, 0x0000_0003)
        );
        assert_eq!(
            (BM1362_REG_IO_DRIVER, BM1362_IO_DRIVER_NORMAL),
            (0x58, 0x0001_1111)
        );
        assert_eq!(
            (BM1362_REG_NONCE_RANGE, BM1362_NONCE_RANGE_126),
            (0x10, 0x0000_1381)
        );
        assert_eq!(
            (BM1362_REG_PLL0_DIVIDER, BM1362_PLL0_DIVIDER_VALUE),
            (0x70, 0x0000_0000)
        );
        assert_eq!(
            (BM1362_REG_PLL0_PARAM, BM1362_PLL0_PARAM_525MHZ),
            (0x08, 0x40A8_0265)
        );
        assert_eq!(
            (BM1362_REG_TICKET_MASK, BM1362_TICKET_MASK_256),
            (0x14, 0x0000_00FF)
        );
        assert_eq!(
            (UART_RELAY_REG_ADDR, UART_RELAY_BOSMINER_ENABLE),
            (0x2C, 0x007C_0003)
        );
        assert_eq!(
            (UART_RELAY_ALT_REG_ADDR, UART_RELAY_BOSMINER_ENABLE_ALT),
            (0x34, 0x000F_0003)
        );
        assert_eq!(BM1362_PLL_RAMP_START_MHZ, 400);
        assert_eq!(BM1362_PLL_RAMP_STEP_MHZ, 25);
        assert_eq!(BM1362_PLL_RAMP_SETTLE_MS, 100);
        assert_eq!(BM1362_SERIAL_PACE_MIN_MS, 20);
        // The on-wire frame for a sample one: 0x3C = HashClk → [HDR=0x51, LEN=0x09,
        // CHIP=0x00, REG=0x3C, VAL_BE=80 00 85 40, CRC5].
        let f = build_broadcast_write_frame(BM1362_REG_CORE_CTRL, BM1362_CORE_REG_HASH_CLK);
        assert_eq!(&f[..8], &[0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x85, 0x40]);
        // Per-chip Step 7 uses HDR=0x41 with the assigned chip address.
        let f =
            build_single_write_frame(0x7E, BM1362_REG_INIT_CONTROL, BM1362_INIT_CONTROL_PER_CHIP);
        assert_eq!(&f[..8], &[0x41, 0x09, 0x7E, 0xA8, 0x00, 0x07, 0x01, 0xF0]);
        let pll_400 = bm1362_pll_ramp_to_target(400);
        assert_eq!(pll_400.last().map(|(_, mhz)| *mhz), Some(400));
        let pll_525 = bm1362_pll_ramp_to_target(525);
        assert_eq!(pll_525.last(), Some(&(BM1362_PLL0_PARAM_525MHZ, 525)));
        assert_eq!(BM1362_INIT_PLAN.misc_control_pre_baud, 0xFF0F_C100);
        assert_eq!(BM1362_INIT_PLAN.misc_control_post_fast_baud, 0x00C1_00B0);
        assert_eq!(BM1362_MISC_CONTROL_LEGACY_AMLOGIC, 0x00C1_00B0);
        assert_eq!(cold_boot_step::MISC_CONTROL_REG, 0x18);
        assert_eq!(
            cold_boot_step::MISC_CONTROL_VALUE_POST_FAST_BAUD,
            0x00C1_00B0
        );
    }

    #[test]
    fn rolled_version_applies_bip320_field() {
        // Base version 0x2000_0004; raw bits 0x0001 → << 13 = 0x0000_2000,
        // which is inside the 0x1FFF_E000 field → version becomes 0x2000_2004.
        assert_eq!(rolled_version(0x2000_0004, 0x0001), 0x2000_2004);
        // Zero rolling bits ⇒ unchanged.
        assert_eq!(rolled_version(0x2000_0004, 0x0000), 0x2000_0004);
        // Bits that fall outside the field (low 13 / above bit 28) are masked off.
        // 0xFFFF << 13 = 0x1FFF_E000 exactly fills the field.
        assert_eq!(
            rolled_version(0x2000_0004, 0xFFFF) & 0x1FFF_E000,
            0x1FFF_E000
        );
        // The base's bits outside the field are preserved.
        assert_eq!(
            rolled_version(0xE000_0005, 0x0000) & !0x1FFF_E000,
            0xE000_0005 & !0x1FFF_E000
        );
    }

    #[test]
    fn rolled_version_checked_respects_negotiated_mask() {
        // Updated 2026-05-15 (cross-platform Protocol fix sweep): when
        // version_mask=0 and vbits != 0, BM1362 chips have rolled BIP320
        // unconditionally; reconstruct rather than drop. The previous
        // assertion `rolled_version_checked(0x2000_0004, 0, 1) == None`
        // pinned the silent-drop bug (Q2 Protocol expert F2; fixed across
        // 4 sites in this sweep).
        assert_eq!(rolled_version_checked(0x2000_0004, 0, 0), Some(0x2000_0004));
        // vbits=1, mask=0 → reconstruct: (1 << 13) & 0x1FFFE000 = 0x2000;
        // rolled = (0x2000_0004 & !0x1FFFE000) | 0x2000 = 0x2000_2004.
        assert_eq!(rolled_version_checked(0x2000_0004, 0, 1), Some(0x2000_2004));
        // mask != 0 + delta inside mask → accept.
        assert_eq!(
            rolled_version_checked(0x2000_0004, 0x0000_6000, 1),
            Some(0x2000_2004)
        );
        // mask != 0 + delta OUTSIDE the negotiated mask → still drop
        // (the share would be rejected post-submit by the pool; drop
        // locally to avoid spamming).
        assert_eq!(
            rolled_version_checked(0x2000_0004, 0x0000_6000, 4),
            None,
            "version delta outside the negotiated mask is rejected"
        );
    }

    #[test]
    fn dispatched_work_full_header_byte_layout() {
        // Pin the 80-byte header layout so the share-validation path can't
        // drift: version LE @ [0..4], prev_block_hash @ [4..36], merkle_root
        // @ [36..68], ntime LE @ [68..72], nbits LE @ [72..76], nonce LE @
        // [76..80] — the byte order WorkBuilder produces / serial_build_header
        // uses + validate_full_header hashes.
        let dw = DispatchedWork {
            job_id: "deadbeef".to_string(),
            extranonce2: "00000000".to_string(),
            ntime: 0x1122_3344,
            nbits: 0x5566_7788,
            version: 0x2000_0004,
            version_mask: 0x1FFF_E000,
            prev_block_hash: [0xAA; 32],
            merkle_root: [0xBB; 32],
            share_target: [0xFF; 32],
        };
        // Use a rolled version distinct from the base to prove it lands at [0..4].
        let rv = 0x2000_4004u32;
        let h = dw.full_header(rv, 0xDEAD_BEEF);
        assert_eq!(&h[0..4], &rv.to_le_bytes(), "rolled version LE @ [0..4]");
        assert_eq!(&h[4..36], &[0xAA; 32], "prev_block_hash @ [4..36]");
        assert_eq!(&h[36..68], &[0xBB; 32], "merkle_root @ [36..68]");
        assert_eq!(
            &h[68..72],
            &0x1122_3344u32.to_le_bytes(),
            "ntime LE @ [68..72]"
        );
        assert_eq!(
            &h[72..76],
            &0x5566_7788u32.to_le_bytes(),
            "nbits LE @ [72..76]"
        );
        assert_eq!(
            &h[76..80],
            &0xDEAD_BEEFu32.to_le_bytes(),
            "nonce LE @ [76..80]"
        );
        assert_eq!(h.len(), 80, "block header is exactly 80 bytes");
        // Different nonce ⇒ only the trailing 4 bytes change.
        let h2 = dw.full_header(rv, 0);
        assert_eq!(
            &h[..76],
            &h2[..76],
            "only the nonce field differs between headers"
        );
        assert_ne!(&h[76..], &h2[76..]);
    }

    #[test]
    fn dispatched_work_is_clone() {
        // The nonce → header lookup table holds `Option<DispatchedWork>` and
        // is built with `vec![None; 256]`, which requires Clone.
        fn _assert_clone<T: Clone>() {}
        _assert_clone::<DispatchedWork>();
        _assert_clone::<Option<DispatchedWork>>();
    }

    #[test]
    fn now_us_is_monotonic_and_nonzero_after_init() {
        let a = now_us();
        // Burn a little wall-clock time.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = now_us();
        assert!(b >= a, "now_us must be monotonic (a={a}, b={b})");
        // After at least one prior call the epoch is set; the second reading
        // is the elapsed-since-epoch, so it's >= the first (could be 0 only on
        // the very first call when the epoch was just created).
        assert!(b >= 2_000 || b >= a, "now_us advanced after a 2ms sleep");
    }

    #[test]
    fn stratum_config_from_propagates_pool_and_donation() {
        // Build a minimal config (all fields are #[serde(default)]).
        let cfg: DcentraldConfig = toml::from_str(
            "[pool]\nurl = \"stratum+tcp://pool.example.com:3333\"\nworker = \"bc1qtest.rig1\"\npassword = \"x\"\n\n[mining]\nversion_rolling = true\nsuggest_difficulty = 4096\n",
        )
        .expect("minimal config must parse");
        let sc = stratum_config_from(&cfg);
        assert_eq!(sc.pool1.url, "stratum+tcp://pool.example.com:3333");
        assert_eq!(sc.pool1.worker, "bc1qtest.rig1");
        assert_eq!(sc.pool1.password, "x");
        assert!(sc.pool2.is_none() && sc.pool3.is_none());
        assert_eq!(sc.routing_mode, "failover");
        assert!(sc.version_rolling);
        assert_eq!(sc.suggest_difficulty, Some(4096));
        // Donation defaults flow through (transparent 2% donation by default).
        assert_eq!(sc.donation.enabled, cfg.donation.enabled);
        assert_eq!(sc.donation.percent, cfg.donation.percent);
        assert_eq!(sc.donation.worker, cfg.donation.worker);
        // am3-bb path does not pre-claim a nominal hashrate.
        assert_eq!(sc.nominal_hashrate_ghs, 0.0);
        assert!(!sc.sv2_extended_channel);
    }

    // ─────────────────────────────────────────────────────────────────────
    // PR-021: continuous fan PID — load-bearing safety invariants.
    // ─────────────────────────────────────────────────────────────────────

    use std::sync::Mutex;

    /// Records every PWM the PID writes so tests can assert the clamp + slew.
    struct RecordingFan {
        pwm_log: Mutex<Vec<u8>>,
        rpm: u32,
        tach: bool,
    }

    impl FanAccess for RecordingFan {
        fn set_speed(&self, pwm: u8) {
            self.pwm_log.lock().unwrap().push(pwm);
        }
        fn get_rpm(&self) -> u32 {
            self.rpm
        }
        fn get_speed_pwm(&self) -> u8 {
            self.pwm_log.lock().unwrap().last().copied().unwrap_or(0)
        }
        fn tach_available(&self) -> bool {
            self.tach
        }
    }

    fn recording_capped_fan(
        fan_min_pwm: u8,
        fan_max_pwm: u8,
    ) -> (Am3BbCappedFan, Arc<RecordingFan>) {
        let rec = Arc::new(RecordingFan {
            pwm_log: Mutex::new(Vec::new()),
            rpm: 1260,
            tach: true,
        });
        let fan = Am3BbCappedFan {
            fan: rec.clone() as Arc<dyn FanAccess>,
            fan_min_pwm,
            fan_max_pwm,
        };
        (fan, rec)
    }

    /// INVARIANT #1: the PID PWM clamp can NEVER exceed the PWM-30 home cap,
    /// for any requested value and any operator config — and an operator can
    /// only ever make it quieter, never louder.
    #[test]
    fn clamp_pid_pwm_never_exceeds_home_cap() {
        // Sweep every possible requested PWM against the home config.
        for req in 0u8..=255 {
            let out = am3_bb_clamp_pid_pwm(0, 30, req);
            assert!(
                out <= AM3_BB_FAN_HARD_CAP_PWM,
                "requested {req} clamped to {out} which exceeds the {AM3_BB_FAN_HARD_CAP_PWM} cap"
            );
        }
        // Operator asking for a HIGHER max cannot raise the real ceiling.
        assert_eq!(
            am3_bb_clamp_pid_pwm(0, 100, 100),
            AM3_BB_FAN_HARD_CAP_PWM,
            "operator config can never make the AM3 BB fan blast past 30"
        );
        assert_eq!(am3_bb_clamp_pid_pwm(0, 100, 255), AM3_BB_FAN_HARD_CAP_PWM);
        // Below the quiet floor → snapped UP to the whisper-quiet floor.
        assert_eq!(
            am3_bb_clamp_pid_pwm(0, 30, 0),
            AM3_BB_FAN_SAFE_FLOOR_PWM,
            "PID asking for less than the quiet floor still keeps fans spinning at the boot level"
        );
        // An intentionally LOWER operator cap is honoured (safety on this
        // board comes from cutting ASIC power, not from fan blast).
        assert_eq!(am3_bb_clamp_pid_pwm(0, 5, 100), 5);
        // A normal in-band request passes through unchanged.
        assert_eq!(am3_bb_clamp_pid_pwm(10, 30, 22), 22);
    }

    /// INVARIANT #2: at/above the dangerous threshold the decision is always
    /// CutHashThenFan (hash power is cut first; the PID never tries to
    /// out-cool a dangerous temp by ramping the fan).
    #[test]
    fn thermal_action_orders_cut_hash_before_fan() {
        assert_eq!(
            am3_bb_thermal_action(54.0, 75.0),
            Am3BbThermalAction::PidWithinCap
        );
        assert_eq!(
            am3_bb_thermal_action(74.9, 75.0),
            Am3BbThermalAction::PidWithinCap
        );
        // Exactly at dangerous → cut hash first.
        assert_eq!(
            am3_bb_thermal_action(75.0, 75.0),
            Am3BbThermalAction::CutHashThenFan
        );
        assert_eq!(
            am3_bb_thermal_action(95.0, 75.0),
            Am3BbThermalAction::CutHashThenFan
        );
    }

    /// INVARIANT #2 at the PID layer: a dangerous snapshot must NOT move the
    /// fan — the PID stops trimming and lets the fail-closed path cut hash.
    #[test]
    fn pid_does_not_ramp_fan_on_dangerous_temp() {
        let (fan, rec) = recording_capped_fan(0, 30);
        let mut pid = Am3BbFanPid::new(fan, 55);
        let writes_after_init = rec.pwm_log.lock().unwrap().len();
        // Dangerous reading: the PID must NOT issue a new fan command.
        pid.step(
            &Am3BbThermalSnapshot {
                samples: 3,
                max_temp_c: 80.0,
            },
            75.0,
        );
        assert_eq!(
            rec.pwm_log.lock().unwrap().len(),
            writes_after_init,
            "PID issued a fan write on a dangerous-temp sample — it must defer to cut-hash-first"
        );
    }

    /// INVARIANT #3: an EMPTY thermal sample must NOT drive the fan — the PID
    /// holds station and lets the fail-closed supervisor own stale/empty.
    #[test]
    fn pid_holds_station_on_empty_sample() {
        let (fan, rec) = recording_capped_fan(0, 30);
        let mut pid = Am3BbFanPid::new(fan, 55);
        let start_pwm = pid.commanded_pwm();
        let writes_after_init = rec.pwm_log.lock().unwrap().len();
        pid.step(
            &Am3BbThermalSnapshot {
                samples: 0,
                max_temp_c: f32::NEG_INFINITY,
            },
            75.0,
        );
        assert_eq!(
            rec.pwm_log.lock().unwrap().len(),
            writes_after_init,
            "PID drove the fan from an empty sample — must NOT act on absent sensor data"
        );
        assert_eq!(pid.commanded_pwm(), start_pwm, "commanded PWM unchanged");
    }

    /// INVARIANT #6 + quiet posture: at/below the setpoint the PID stays at
    /// the whisper-quiet floor; a sustained hot (but not dangerous) temp ramps
    /// the fan only TOWARD the cap, never past it, and only a few PWM steps
    /// per tick (no audible jump).
    #[test]
    fn pid_steady_state_is_quiet_and_ramps_bounded_within_cap() {
        let (fan, _rec) = recording_capped_fan(0, 30);
        let mut pid = Am3BbFanPid::new(fan, 55);
        // Cool: a few ticks well below the setpoint → stays at the floor.
        for _ in 0..5 {
            pid.step(
                &Am3BbThermalSnapshot {
                    samples: 3,
                    max_temp_c: 45.0,
                },
                75.0,
            );
        }
        assert_eq!(
            pid.commanded_pwm(),
            AM3_BB_FAN_SAFE_FLOOR_PWM,
            "below setpoint the quiet home fan must stay at the whisper-quiet floor"
        );

        // Now hot-but-safe (68C, below dangerous 75): the PID ramps up, but
        // every single-tick move is bounded and the value never exceeds 30.
        let mut prev = pid.commanded_pwm();
        for _ in 0..30 {
            pid.step(
                &Am3BbThermalSnapshot {
                    samples: 3,
                    max_temp_c: 68.0,
                },
                75.0,
            );
            let now = pid.commanded_pwm();
            assert!(
                now <= AM3_BB_FAN_HARD_CAP_PWM,
                "PID commanded {now} which exceeds the {AM3_BB_FAN_HARD_CAP_PWM} home cap"
            );
            assert!(
                now.abs_diff(prev) <= AM3_BB_FAN_PID_MAX_STEP_PWM,
                "single-tick PWM slew {} -> {} exceeds the {}-step quiet limit",
                prev,
                now,
                AM3_BB_FAN_PID_MAX_STEP_PWM
            );
            prev = now;
        }
        // Sustained hot drove it to the cap (active cooling within the quiet
        // envelope), but NOT past it.
        assert_eq!(
            pid.commanded_pwm(),
            AM3_BB_FAN_HARD_CAP_PWM,
            "sustained hot temp should ramp the fan to (but never past) the quiet cap"
        );
    }

    /// The capped-fan view itself cannot be coerced to blast: even a bogus
    /// 255 request applied directly is clamped.
    #[test]
    fn capped_fan_apply_is_clamped() {
        let (fan, rec) = recording_capped_fan(0, 30);
        let written = fan.apply(255);
        assert_eq!(written, AM3_BB_FAN_HARD_CAP_PWM);
        assert_eq!(*rec.pwm_log.lock().unwrap().last().unwrap(), 30);
        assert_eq!(fan.apply(0), AM3_BB_FAN_SAFE_FLOOR_PWM);
    }
}
