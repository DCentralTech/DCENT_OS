//! AM335x BeagleBone clean-room ASIC work-dispatch transport for BM1362.
//!
//! Mirrors the INTERNAL behavior of stock Bitmain `uart_trans.ko` (per
//!  + )
//! WITHOUT porting the kernel module (
//! +  decision #2: UIO + userspace, no kernel modules).
//!
//! ## What goes on the wire
//!
//! The 86-byte `asic_work_t` ([`super::wire_uart_trans::AsicWorkFrame`],
//! serialized via [`super::wire_uart_trans::wire_frame_bytes`]). The
//! "168-byte `pack_asic_work`" in the W4 handoff was the kernel-internal
//! buffer (86 B wire + 82 B metadata: chain index, dispatch timestamp,
//! retry count, ring-slot index) — NOT the wire format. The codec lives
//! in [`super::wire_uart_trans`]; this module is the live I/O wrapper.
//!
//! ## HAL-free design
//!
//! `dcentrald-asic` does depend on `dcentrald-hal`, but this transport
//! deliberately does not reach into it. The UART I/O is expressed via
//! the [`ChainUart`] trait; the daemon crate provides the
//! `impl ChainUart for dcentrald_hal::serial::DevmemUart` adapter (the
//! `DevmemUart::init()` already writes `MCR=0x03` + `FCR=0x07` per
//!  — do NOT re-implement
//! that here). This keeps the transport pure (no `std::thread`, no
//! `clock_nanosleep`, no device files) so the byte / pacing / ring
//! behavior is fully host-testable with an in-memory mock UART — the
//! same separation-of-concerns approach as the `pic1704` sealed traits.
//!
//! ## Pacing
//!
//! `uart_trans.ko` rate-limits work dispatch with an `hrtimer` that pops
//! at most one frame per chain per tick, `uart_send_interval` ≈ 4600 µs
//! by default. We mirror that with [`UART_SEND_INTERVAL_US`]; the caller
//! supplies "now" (monotonic µs) and we return `Ok(false)` (= not
//! dispatched, retry later) until the interval has elapsed. There is a
//! hard floor of [`MIN_DISPATCH_INTERVAL_US`] (≈ 50 frames/s/chain) —
//! flooding the chain UART produces zero nonces after the first batch
//!. The actual timer lives in the
//! daemon's mining loop, not here.
//!
//! ## CRC note (UNRESOLVED DISCREPANCY — do NOT change speculatively)
//!
//! Frames use `crate::protocol::crc16`, which is **CRC-CCITT-FALSE**
//! (poly `0x1021`, init `0xFFFF`, no refin/refout, no xorout — pinned by
//! the `wire_uart_trans` test `crc_itu_table_known_vector_123456789` ⇒
//! `0x29B1`). The  memory rule
//! instead claims the on-wire CRC is **CRC-16/IBM-SDLC** (poly `0x1021`,
//! init `0xFFFF`, refin/refout = true, xorout `0xFFFF`). These are
//! different algorithms. The codec was built from the W4 handoff
//! `bm1362_frames_v2.h` which uses CCITT-FALSE, so that is what we keep.
//! **Live bring-up MUST capture a real bmminer wire trace on the bench
//! AM335x BB unit and resolve which CRC the BM1362 actually validates.**
//! If it turns out to be IBM-SDLC, the fix belongs in the codec
//! (`wire_uart_trans::AsicWorkFrame` + `parse_nonce_frame`), not here.
//!
//! ## Cross-references
//!
//! -  — kernel struct layout
//!   (86 B wire / 168 B kernel buffer / 16-slot ring / hrtimer cadence /
//!   the CRC discrepancy noted above).
//! -  — clean-room port shape (per-chain
//!   tty fd, hrtimer-equiv TX thread, per-chain async RX, MPSC tx ring).
//! -  — `DevmemUart::init`
//!   already does `MCR=0x03` + `FCR=0x07`; reuse it, don't re-derive.
//! -  — OMAP UART bases (ttyO1/2/4/5).
//! -  — ≤ ~50 frames/s/chain, never flood.
//! -  — `job_id` is the low 8 bits.
//! -  /  decision #2 — no
//!   kernel-module port.

use crate::bm1362::wire_uart_trans::{
    parse_nonce_frame, wire_frame_bytes, AsicWorkFrame, NonceResponse, ASIC_WORK_SIZE, CMD_MAGIC,
    NONCE_FRAME_MIN_LEN,
};

// ---------------------------------------------------------------------------
// BM1362 serial-wire nonce response (the PROVEN format — see note below)
// ---------------------------------------------------------------------------
//
// **R7-3 (2026-05-12): the live BM1362-on-AM335x-BB work + nonce wire format
// is the *standard BM1362 chip-comm frame*, NOT the W14.B 86-byte `asic_work_t`
// codec ([`AsicWorkFrame`] / [`parse_nonce_frame`]).** The W14.B codec came
// from the W4 dev-kit `bm1362_frames_v2.h` and does not match what the chip
// actually speaks: the LuxOS RE corpus (`analysis/C-asic-fpga-protocol.md`:
// "standard BM1362 chip-comm framing") + cross-check against the *proven*
// Amlogic-NoPic serial path (`dcentrald::serial_mining` / `dcentrald_asic::
// drivers::bm1362::build_serial_work_frame`, sustained-mining-validated) say:
//
//   Work-job frame (88 bytes on the wire):
//     [0x55 0xAA] [0x21] [0x56] [82-byte BM1366-family full-header payload]
//     [CRC16-CCITT-FALSE hi, lo]   (CRC over the 84 bytes from 0x21 .. last
//                                   payload byte; big-endian appended)
//     — `dcentrald_asic::drivers::bm1362::build_serial_work_frame`. BM1362
//     needs NO open-core dummy-work (it's not the BM1387 14 nm; per
//     `drivers::bm1362` module docs — verified against bosminer).
//
//   Nonce-response frame (11 bytes on the wire):
//     [0xAA 0x55] [n3 n2 n1 n0] [midstate_idx] [result] [vbits_hi vbits_lo]
//     [flags]
//       — `nonce`   = u32::from_le_bytes([n0..n3] in wire order) per the
//         ESP-Miner convention the proven path mimics
//         (`u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])`).
//       — `result`  : job_id = (result & 0xF0) >> 1, small_core = result & 0x0F
//       — `vbits`   : big-endian; reconstruct rolled version via
//         `((vbits as u32) << 13) & 0x1FFF_E000`
//       — `flags`   : bit7 = 1 ⇒ job response (else skip); bits[4:0] = CRC5
//
// The W14.B [`AsicWorkFrame`]/[`parse_nonce_frame`] codec is kept for
// stock-firmware byte-parity diagnostics (see `wire_uart_trans.rs` "CODEC
// ONLY") but is NOT what the live AM335x-BB mining path uses.

/// `0xAA 0x55` response preamble that prefixes every BM1362 serial-wire frame
/// (the inverse of the `0x55 0xAA` command preamble).
pub const SERIAL_RESP_PREAMBLE: [u8; 2] = [0xAA, 0x55];

/// On-wire length of a BM1362 serial nonce frame (`0xAA 0x55` + 9 body bytes).
pub const BM1362_SERIAL_NONCE_FRAME_LEN: usize = 11;

/// BIP320 ASICBoost version-rolling field mask: 16 bits at block-header
/// version positions 13..28 — the bits BM1362-family chips are physically
/// allowed to roll. Per BIP320 §"Version Rolling".
pub const BIP320_VERSION_ROLLING_MASK: u32 = 0x1FFF_E000;

/// Reconstruct the rolled block-header version from a base version + the raw
/// 16-bit rolled-bits delta the BM1362-family chip returned in its 11-byte
/// serial nonce frame ([`Bm1362SerialNonce::version_bits_raw`]).
///
/// **BM1362 / BM1366 / BM1368 / BM1370 chips physically roll the BIP320
/// 16-bit field (mask `0x1FFFE000`) regardless of whether the pool
/// negotiated version-rolling via `mining.configure`.** The chip's
/// rolled-bits delta is shifted left 13 + masked, then OR'd into the base
/// version with the BIP320 field cleared.
///
/// Returns `(rolled_version, vbits_delta_masked)`:
///
/// * `rolled_version` — the full 32-bit header version field with rolled
///   bits applied. Use this when calling `validate_full_header`, building
///   the block header that you SHA256d locally, and populating
///   [`dcentrald_stratum::types::ValidShare::version`] for SV2.
///
/// * `vbits_delta_masked` — the BIP320-shifted-and-masked delta. This is
///   the canonical hex value to emit as the Stratum V1 `mining.submit`
///   6th parameter (and to populate
///   [`dcentrald_stratum::types::ValidShare::version_bits`]) when the
///   delta is non-zero.
///
/// `validate_full_header(header_with_rolled_version, share_target)` is
/// the SOLE gate. Do **not** add pre-validate filters on
/// `version_bits_raw` (the rejection guard at
/// `s19j_hybrid_mining.rs::run_am2_serial_dispatch_loop` pre-`2b6d46f3`
/// dropped 95% of valid hashing work on AM2 XIL `a lab unit`).
///
/// See memory rules:
///
///
///
pub fn bip320_reconstruct_rolled_version(base_version: u32, version_bits_raw: u16) -> (u32, u32) {
    let vbits_delta: u32 = ((version_bits_raw as u32) << 13) & BIP320_VERSION_ROLLING_MASK;
    let rolled_version = (base_version & !BIP320_VERSION_ROLLING_MASK) | vbits_delta;
    (rolled_version, vbits_delta)
}

/// Upper bound for a per-chain serial RX carry buffer after parsing.
const BM1362_SERIAL_RX_MAX_BUFFER: usize = BM1362_SERIAL_NONCE_FRAME_LEN * 8;

/// Per-chain cumulative BM1362 serial RX counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bm1362SerialRxCounters {
    /// Raw UART bytes read by the serial nonce poller.
    pub raw_bytes: u64,
    /// Complete 11-byte BM1362 serial frames parsed.
    pub parsed_frames: u64,
    /// Parsed frames whose flags byte marks a job response (`flags & 0x80 != 0`).
    pub job_response_frames: u64,
    /// Parsed frames without the job-response flag. During bring-up these are
    /// usually residual chip-init/address replies, not share candidates.
    pub non_job_response_frames: u64,
    /// Bytes discarded while resynchronizing to the `0xAA 0x55` preamble.
    pub resync_bytes: u64,
    /// Bytes dropped by the defensive carry-buffer cap.
    pub dropped_bytes: u64,
    /// Bytes currently retained as an incomplete frame tail.
    pub buffered_bytes: usize,
}

/// Parsed BM1362 serial-wire nonce response (see the module note above for the
/// byte layout). This is the PROVEN format (matches the Amlogic-NoPic serial
/// path) — distinct from the W14.B [`NonceResponse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bm1362SerialNonce {
    /// Full 11-byte wire frame, including the `0xAA 0x55` preamble. Kept for
    /// AM3-BB bring-up diagnostics so decode/endian hypotheses can be replayed
    /// without another live UART capture.
    pub raw_frame: [u8; BM1362_SERIAL_NONCE_FRAME_LEN],
    /// Found nonce, interpreted as `u32::from_le_bytes` of the 4 wire bytes
    /// (the ESP-Miner / proven-path convention).
    pub nonce: u32,
    /// Job id echoed by the chip: `(result_byte & 0xF0) >> 1`.
    pub job_id: u8,
    /// Raw RESULT byte. High nibble carries echoed job id bits; low nibble is
    /// the small-core index.
    pub result_byte: u8,
    /// Small-core index: `result_byte & 0x0F` (BM1362 has 16 small cores).
    pub small_core: u8,
    /// Midstate slot index (always 0 for the 1-midstate full-header form).
    pub midstate_idx: u8,
    /// Raw version-rolling bits, big-endian. Reconstruct the rolled version:
    /// `(base_version & !0x1FFF_E000) | (((version_bits_raw as u32) << 13) & 0x1FFF_E000)`.
    pub version_bits_raw: u16,
    /// FLAGS byte: bit7 = job-response, bits[4:0] = CRC5. The caller should
    /// skip frames where `flags & 0x80 == 0`.
    pub flags: u8,
}

/// Parse an 11-byte BM1362 serial-wire nonce frame.
///
/// Validates the `0xAA 0x55` preamble and the length only — the trailing byte
/// mixes CRC5 with flags and the proven path does not validate it (the chip's
/// `TicketMask` already gates which nonces come back; the caller filters on
/// [`Bm1362SerialNonce::flags`] bit7). Returns `None` on a bad preamble or a
/// short slice.
pub fn parse_bm1362_serial_nonce(raw: &[u8]) -> Option<Bm1362SerialNonce> {
    if raw.len() < BM1362_SERIAL_NONCE_FRAME_LEN {
        return None;
    }
    if raw[0] != SERIAL_RESP_PREAMBLE[0] || raw[1] != SERIAL_RESP_PREAMBLE[1] {
        return None;
    }
    let b = &raw[2..11]; // 9 body bytes
    let id_byte = b[5];
    let mut raw_frame = [0u8; BM1362_SERIAL_NONCE_FRAME_LEN];
    raw_frame.copy_from_slice(&raw[..BM1362_SERIAL_NONCE_FRAME_LEN]);
    Some(Bm1362SerialNonce {
        raw_frame,
        nonce: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        job_id: (id_byte & 0xF0) >> 1,
        result_byte: id_byte,
        small_core: id_byte & 0x0F,
        midstate_idx: b[4],
        version_bits_raw: u16::from_be_bytes([b[6], b[7]]),
        flags: b[8],
    })
}

// ---------------------------------------------------------------------------
// ChainUart — the per-chain UART I/O primitive the transport drives
// ---------------------------------------------------------------------------

/// Per-chain UART I/O primitive the transport drives.
///
/// The daemon crate provides the
/// `impl ChainUart for dcentrald_hal::serial::DevmemUart` adapter. This
/// crate (`dcentrald-asic`) does not name `DevmemUart` directly so the
/// transport stays pure/host-testable (same pattern as the `pic1704`
/// sealed traits).
pub trait ChainUart {
    /// Write a complete frame to the chain UART (blocking, full-buffer).
    /// Implementations MUST write the entire slice or return
    /// [`UartTransportError::WriteFailed`].
    fn write_frame(&mut self, data: &[u8]) -> Result<(), UartTransportError>;

    /// Read up to `buf.len()` bytes from the chain UART RX FIFO; returns
    /// the count read (`0` = nothing available). Non-blocking or
    /// short-timeout — nonce frames arrive asynchronously and the
    /// transport polls.
    fn read_avail(&mut self, buf: &mut [u8]) -> usize;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum in-flight `work_id` slots per chain.
///
/// Mirrors `uart_trans.ko`'s `send_work_id[]` ring buffer — 16 slots,
/// used for ack / retry / duplicate-detect tracking
///.
pub const SEND_WORK_RING_SLOTS: usize = 16;

/// Default inter-frame dispatch interval, microseconds.
///
/// Mirrors the kernel `hrtimer` `uart_send_interval` default ≈ 4600 µs
///; also keeps us at ≤ ~217
/// frames/s/chain — well under the ~50 frames/s/chain cap that matters
/// once the daemon's loop adds its
/// own batching.
pub const UART_SEND_INTERVAL_US: u64 = 4_600;

/// Hard floor on the dispatch interval, microseconds (≈ 50 frames/s/chain).
///
/// `new()` clamps the configured interval *up* to this. Flooding the
/// chain UART faster than this produces zero nonces after the first
/// batch — there is no legitimate
/// reason to go faster, so this is a floor, not a default.
pub const MIN_DISPATCH_INTERVAL_US: u64 = 20_000;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the AM335x BB work-dispatch transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartTransportError {
    /// The underlying UART write returned an error or a partial write.
    WriteFailed,
    /// A frame exceeded the expected on-wire size ([`ASIC_WORK_SIZE`]).
    FrameTooLong,
    /// Chain index out of range for this transport's configured chains.
    ChainOutOfRange,
}

// ---------------------------------------------------------------------------
// Per-chain state
// ---------------------------------------------------------------------------

/// One chain's TX ring + pacing state.
struct ChainState {
    /// 16-slot ring of in-flight `work_id` (low 8 bits). `None` = empty
    /// slot. When the ring wraps, the oldest entry is overwritten
    /// (drop-oldest, like the kernel module — work that old is stale
    /// anyway).
    in_flight: [Option<u8>; SEND_WORK_RING_SLOTS],
    /// Next ring slot to write (round-robin, wrapping).
    next_slot: usize,
    /// Monotonic-µs timestamp of the last dispatched frame. The caller
    /// supplies "now" so the transport stays pure/testable; `0` until
    /// the first dispatch (any non-zero `now_us` on the first call
    /// passes the pacing check).
    last_dispatch_us: u64,
    /// `true` once at least one frame has been dispatched on this chain
    /// (so a literal `now_us == 0` first call still works).
    has_dispatched: bool,
}

impl ChainState {
    fn new() -> Self {
        Self {
            in_flight: [None; SEND_WORK_RING_SLOTS],
            next_slot: 0,
            last_dispatch_us: 0,
            has_dispatched: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Am335xUartTransport
// ---------------------------------------------------------------------------

/// AM335x BB work-dispatch transport over one or more chain UARTs.
///
/// Generic over the [`ChainUart`] impl so it is host-testable with an
/// in-memory mock UART. The transport is pure: it never sleeps, never
/// spawns threads, never touches device files — the caller supplies
/// "now" (monotonic µs) and decides when to retry a paced-off send. The
/// daemon's mining loop owns the actual timer.
pub struct Am335xUartTransport<U: ChainUart> {
    /// One `DevmemUart`-equivalent per chain.
    chains: Vec<U>,
    /// Per-chain TX ring + pacing state, parallel to `chains`.
    state: Vec<ChainState>,
    /// Configured dispatch interval, µs (clamped ≥ [`MIN_DISPATCH_INTERVAL_US`]).
    dispatch_interval_us: u64,
    /// RX scratch buffer, reused across [`Am335xUartTransport::recv_nonces`] calls.
    rx_scratch: Vec<u8>,
    /// Per-chain carry buffers for BM1362 serial frames split across reads.
    rx_bm1362_serial: Vec<Vec<u8>>,
    rx_bm1362_serial_raw_bytes: Vec<u64>,
    rx_bm1362_serial_parsed_frames: Vec<u64>,
    rx_bm1362_serial_job_response_frames: Vec<u64>,
    rx_bm1362_serial_non_job_response_frames: Vec<u64>,
    rx_bm1362_serial_resync_bytes: Vec<u64>,
    rx_bm1362_serial_dropped_bytes: Vec<u64>,
}

impl<U: ChainUart> Am335xUartTransport<U> {
    /// Build a transport over the given per-chain UARTs.
    ///
    /// `dispatch_interval_us` is clamped **up** to
    /// [`MIN_DISPATCH_INTERVAL_US`] (you can ask for slower, never
    /// faster). Use [`UART_SEND_INTERVAL_US`] for the stock-equivalent
    /// cadence.
    pub fn new(chains: Vec<U>, dispatch_interval_us: u64) -> Self {
        let n = chains.len();
        let state = (0..n).map(|_| ChainState::new()).collect();
        Self {
            chains,
            state,
            dispatch_interval_us: dispatch_interval_us.max(MIN_DISPATCH_INTERVAL_US),
            // 256 B scratch comfortably holds several 10-byte nonce
            // frames per poll; grows on demand if a chain bursts more.
            rx_scratch: vec![0u8; 256],
            rx_bm1362_serial: (0..n).map(|_| Vec::with_capacity(32)).collect(),
            rx_bm1362_serial_raw_bytes: vec![0; n],
            rx_bm1362_serial_parsed_frames: vec![0; n],
            rx_bm1362_serial_job_response_frames: vec![0; n],
            rx_bm1362_serial_non_job_response_frames: vec![0; n],
            rx_bm1362_serial_resync_bytes: vec![0; n],
            rx_bm1362_serial_dropped_bytes: vec![0; n],
        }
    }

    /// Number of chains.
    pub fn chain_count(&self) -> usize {
        self.chains.len()
    }

    /// The effective (post-clamp) dispatch interval, µs.
    pub fn dispatch_interval_us(&self) -> u64 {
        self.dispatch_interval_us
    }

    /// Snapshot of a chain's in-flight `work_id` ring (round-robin
    /// order, `None` = empty slot). Mainly for tests / diagnostics.
    /// Returns `None` if `chain_idx` is out of range.
    pub fn in_flight_ring(&self, chain_idx: usize) -> Option<[Option<u8>; SEND_WORK_RING_SLOTS]> {
        self.state.get(chain_idx).map(|s| s.in_flight)
    }

    /// Snapshot cumulative raw RX counters for the BM1362 serial nonce poller.
    pub fn bm1362_serial_rx_counters(&self) -> Vec<Bm1362SerialRxCounters> {
        (0..self.chains.len())
            .map(|idx| Bm1362SerialRxCounters {
                raw_bytes: self.rx_bm1362_serial_raw_bytes[idx],
                parsed_frames: self.rx_bm1362_serial_parsed_frames[idx],
                job_response_frames: self.rx_bm1362_serial_job_response_frames[idx],
                non_job_response_frames: self.rx_bm1362_serial_non_job_response_frames[idx],
                resync_bytes: self.rx_bm1362_serial_resync_bytes[idx],
                dropped_bytes: self.rx_bm1362_serial_dropped_bytes[idx],
                buffered_bytes: self.rx_bm1362_serial[idx].len(),
            })
            .collect()
    }

    /// Try to dispatch `frame` to chain `chain_idx` at time `now_us`.
    ///
    /// - `Ok(true)`  — dispatched (written to the chain UART, `work_id`
    ///   recorded in the 16-slot ring, pacing clock updated).
    /// - `Ok(false)` — not dispatched yet: the pacing interval has not
    ///   elapsed since the last frame on this chain. The caller should
    ///   retry later (the hrtimer-equivalent backpressure).
    /// - `Err(ChainOutOfRange)` — `chain_idx >= chain_count()`.
    /// - `Err(WriteFailed)` — the underlying UART write failed; the ring
    ///   and pacing clock are left untouched so the caller can retry.
    ///
    /// When the 16-slot ring wraps, the oldest in-flight entry is
    /// overwritten (drop-oldest, matching the kernel module's
    /// `send_work_id[]` behavior).
    pub fn try_send_work(
        &mut self,
        chain_idx: usize,
        frame: &AsicWorkFrame,
        now_us: u64,
    ) -> Result<bool, UartTransportError> {
        if chain_idx >= self.chains.len() {
            return Err(UartTransportError::ChainOutOfRange);
        }

        // Pacing check (uses `has_dispatched` so a literal now_us == 0
        // first call still works).
        {
            let st = &self.state[chain_idx];
            if st.has_dispatched {
                let elapsed = now_us.saturating_sub(st.last_dispatch_us);
                if elapsed < self.dispatch_interval_us {
                    return Ok(false);
                }
            }
        }

        let bytes: [u8; ASIC_WORK_SIZE] = wire_frame_bytes(frame);
        // Defensive: the on-wire frame is fixed-size. If a future codec
        // change ever returned something longer, refuse rather than
        // silently truncate at the UART layer.
        if bytes.len() > ASIC_WORK_SIZE {
            return Err(UartTransportError::FrameTooLong);
        }

        self.chains[chain_idx].write_frame(&bytes)?;

        // Record in the ring + advance pacing only after a successful write.
        let st = &mut self.state[chain_idx];
        st.in_flight[st.next_slot] = Some(frame.job_id);
        st.next_slot = (st.next_slot + 1) % SEND_WORK_RING_SLOTS;
        st.last_dispatch_us = now_us;
        st.has_dispatched = true;
        Ok(true)
    }

    /// Try to dispatch a **pre-built raw frame** to chain `chain_idx` at
    /// `now_us`, recording `job_id` in the 16-slot in-flight ring.
    ///
    /// Same pacing / ring / error semantics as [`Self::try_send_work`], but
    /// takes ready bytes rather than an [`AsicWorkFrame`] — for the live
    /// AM335x-BB path, which sends the *proven* 88-byte BM1362 full-header
    /// frame (`[0x55 0xAA 0x21 0x56][82-byte payload][CRC16 BE]` from
    /// `dcentrald_asic::drivers::bm1362::build_serial_work_frame`), NOT the
    /// W14.B 86-byte `asic_work_t`. See the "R7-3" module note above.
    ///
    /// No length cap (the caller owns the frame format); a `WriteFailed` from
    /// the underlying UART leaves the ring + pacing clock untouched so the
    /// caller can retry.
    pub fn try_send_raw(
        &mut self,
        chain_idx: usize,
        frame: &[u8],
        job_id: u8,
        now_us: u64,
    ) -> Result<bool, UartTransportError> {
        if chain_idx >= self.chains.len() {
            return Err(UartTransportError::ChainOutOfRange);
        }
        {
            let st = &self.state[chain_idx];
            if st.has_dispatched {
                let elapsed = now_us.saturating_sub(st.last_dispatch_us);
                if elapsed < self.dispatch_interval_us {
                    return Ok(false);
                }
            }
        }
        self.chains[chain_idx].write_frame(frame)?;
        let st = &mut self.state[chain_idx];
        st.in_flight[st.next_slot] = Some(job_id);
        st.next_slot = (st.next_slot + 1) % SEND_WORK_RING_SLOTS;
        st.last_dispatch_us = now_us;
        st.has_dispatched = true;
        Ok(true)
    }

    /// Poll every chain's RX for **BM1362 serial-wire nonce frames**
    /// (`[0xAA 0x55][...]`, 11 bytes — the PROVEN format; see the "R7-3"
    /// module note above).
    ///
    /// Scans for the `0xAA 0x55` preamble, parses each 11-byte window via
    /// [`parse_bm1362_serial_nonce`], and returns the parsed frames. The
    /// caller is responsible for (a) filtering on the FLAGS bit7
    /// (job-response) and (b) deduplicating against previously-submitted
    /// nonces before pool submission (per the S9 first-hash fix). A bad
    /// preamble byte advances the scanner by one (resync); a parsed 11-byte
    /// frame advances by 11. Incomplete tails are carried across polls because
    /// AM335x UART reads can split an 11-byte BM1362 response across multiple
    /// zero-timeout `read_avail` calls. This is the BM1362-serial counterpart to
    /// [`Self::recv_nonces`] (which parses the different/wrong W14.B 10-byte
    /// `0x55`-led codec).
    pub fn recv_bm1362_serial_nonces(&mut self) -> Vec<(usize, Bm1362SerialNonce)> {
        let mut out = Vec::new();
        for chain_idx in 0..self.chains.len() {
            loop {
                let n = self.chains[chain_idx].read_avail(&mut self.rx_scratch);
                if n == 0 {
                    break;
                }
                let n = n.min(self.rx_scratch.len());
                self.rx_bm1362_serial[chain_idx].extend_from_slice(&self.rx_scratch[..n]);
                self.rx_bm1362_serial_raw_bytes[chain_idx] += n as u64;
            }

            let mut parsed_frames = 0u64;
            let mut job_response_frames = 0u64;
            let mut non_job_response_frames = 0u64;
            let mut resync_bytes = 0u64;
            let mut dropped_bytes = 0u64;
            {
                let buf = &mut self.rx_bm1362_serial[chain_idx];
                let mut i = 0usize;
                let mut consumed = 0usize;
                while i + 1 < buf.len() {
                    if buf[i] != SERIAL_RESP_PREAMBLE[0] || buf[i + 1] != SERIAL_RESP_PREAMBLE[1] {
                        i += 1;
                        consumed = i;
                        resync_bytes += 1;
                        continue;
                    }
                    if i + BM1362_SERIAL_NONCE_FRAME_LEN > buf.len() {
                        break;
                    }
                    // Preamble matched + a full 11-byte frame is present: it is a
                    // frame, so advance by 11 even if the caller later rejects it.
                    if let Some(nr) =
                        parse_bm1362_serial_nonce(&buf[i..i + BM1362_SERIAL_NONCE_FRAME_LEN])
                    {
                        if nr.flags & 0x80 != 0 {
                            job_response_frames += 1;
                        } else {
                            non_job_response_frames += 1;
                        }
                        out.push((chain_idx, nr));
                        parsed_frames += 1;
                    }
                    i += BM1362_SERIAL_NONCE_FRAME_LEN;
                    consumed = i;
                }

                if consumed > 0 {
                    buf.drain(..consumed);
                }
                if buf.len() > BM1362_SERIAL_RX_MAX_BUFFER {
                    let keep_from = buf.len().saturating_sub(BM1362_SERIAL_NONCE_FRAME_LEN - 1);
                    dropped_bytes += keep_from as u64;
                    buf.drain(..keep_from);
                }
            }
            self.rx_bm1362_serial_parsed_frames[chain_idx] += parsed_frames;
            self.rx_bm1362_serial_job_response_frames[chain_idx] += job_response_frames;
            self.rx_bm1362_serial_non_job_response_frames[chain_idx] += non_job_response_frames;
            self.rx_bm1362_serial_resync_bytes[chain_idx] += resync_bytes;
            self.rx_bm1362_serial_dropped_bytes[chain_idx] += dropped_bytes;
        }
        out
    }

    /// Poll every chain's RX for nonce frames.
    ///
    /// Reads whatever is available on each chain's UART, scans for the
    /// `0x55` magic byte, attempts [`parse_nonce_frame`] on each 10-byte
    /// window, and returns the ones that parse (magic OK + CRC OK).
    /// Invalid / partial frames are skipped — the scanner resyncs on the
    /// next `0x55` (the kernel module tolerates lossy async framing the
    /// same way). The caller is responsible for deduplicating nonces
    /// against previously-submitted ones before pool submission (per the
    /// S9 first-hash fix — ).
    pub fn recv_nonces(&mut self) -> Vec<(usize, NonceResponse)> {
        let mut out = Vec::new();
        for chain_idx in 0..self.chains.len() {
            // Drain in chunks; loop until the chain reports nothing more.
            loop {
                let n = self.chains[chain_idx].read_avail(&mut self.rx_scratch);
                if n == 0 {
                    break;
                }
                let n = n.min(self.rx_scratch.len());
                let buf = &self.rx_scratch[..n];

                // Scan for 0x55 magic; on each candidate, try to parse a
                // 10-byte nonce frame. Resync past garbage.
                let mut i = 0usize;
                while i < buf.len() {
                    if buf[i] != CMD_MAGIC {
                        i += 1;
                        continue;
                    }
                    if i + NONCE_FRAME_MIN_LEN > buf.len() {
                        // Magic near the tail with not enough bytes for a
                        // full frame; stop (a real driver would carry the
                        // partial into the next read — this scratch-based
                        // poller just drops it, which matches the kernel
                        // module's "lossy async RX tolerated" behavior).
                        break;
                    }
                    match parse_nonce_frame(&buf[i..i + NONCE_FRAME_MIN_LEN]) {
                        Ok(nr) => {
                            out.push((chain_idx, nr));
                            i += NONCE_FRAME_MIN_LEN;
                        }
                        Err(_) => {
                            // Bad CRC / malformed — this 0x55 wasn't a
                            // real frame start. Skip one byte and resync.
                            i += 1;
                        }
                    }
                }

                // If the read returned fewer bytes than the scratch
                // capacity, the FIFO is drained for this poll.
                if n < self.rx_scratch.len() {
                    break;
                }
            }
        }
        out
    }

    /// Flush all chains' TX ring (mirrors the `uart_trans.ko` flush path
    /// and the S9 "flush WORK_RX_FIFO on clean_jobs" rule).
    ///
    /// Clears in-flight `work_id` tracking on every chain and resets the
    /// round-robin slot pointer. Does **not** touch the UART hardware
    /// FIFO — the caller's [`ChainUart`] impl owns that — and does not
    /// reset pacing (a clean-jobs event doesn't license a faster burst).
    pub fn clean_work(&mut self) {
        for st in &mut self.state {
            st.in_flight = [None; SEND_WORK_RING_SLOTS];
            st.next_slot = 0;
        }
    }
}

// ===========================================================================
// Tests (host-safe, no hardware)
// ===========================================================================

#[cfg(test)]
mod bip320_tests {
    use super::*;

    #[test]
    fn vbits_zero_returns_base_version_unchanged() {
        let (rolled, delta) = bip320_reconstruct_rolled_version(0x2000_0000, 0);
        assert_eq!(rolled, 0x2000_0000);
        assert_eq!(delta, 0);
    }

    #[test]
    fn vbits_xil_milestone_sample_rx_index_10() {
        // From the 2026-05-15 .109 RX-frame instrumented run, rx_index=10:
        // bytes "AA 55 77 84 1F A1 00 2C 00 14 8A" → version_bits_raw=0x0014.
        // Base version 0x2000_0000 (per Public Pool's mining.notify default).
        // delta = (0x14 << 13) & 0x1FFFE000 = 0x0002_8000.
        // rolled = (base & !mask) | delta = 0x2000_0000 | 0x0002_8000.
        let (rolled, delta) = bip320_reconstruct_rolled_version(0x2000_0000, 0x0014);
        assert_eq!(delta, 0x0002_8000);
        assert_eq!(rolled, 0x2002_8000);
    }

    #[test]
    fn helper_clears_existing_bip320_field_then_or_in_delta() {
        // Base version with stale bits inside the BIP320 field — helper
        // must clear those bits before OR'ing in the new delta. Otherwise
        // we'd double-count and produce a wrong-version share.
        let (rolled, delta) = bip320_reconstruct_rolled_version(0x21FF_E000, 0x0014);
        assert_eq!(delta, 0x0002_8000);
        assert_eq!(rolled, 0x2002_8000);
    }

    #[test]
    fn helper_max_u16_fills_bip320_field_no_overflow() {
        // version_bits_raw = 0xFFFF (16 bits all set) → delta should be
        // exactly the BIP320 field (mask itself). Confirms the shift
        // doesn't overflow into bit 29+.
        let (rolled, delta) = bip320_reconstruct_rolled_version(0x2000_0000, 0xFFFF);
        assert_eq!(delta, BIP320_VERSION_ROLLING_MASK);
        assert_eq!(delta, 0x1FFF_E000);
        assert_eq!(rolled, 0x2000_0000 | 0x1FFF_E000);
    }

    #[test]
    fn helper_outside_field_bits_in_base_preserved() {
        // Base bits OUTSIDE the BIP320 field (e.g. high bits 29..31, low
        // bits 0..12) must be preserved unchanged.
        let base = 0xE000_1FFF; // bits 29-31 + bits 0-12 set
        let (rolled, delta) = bip320_reconstruct_rolled_version(base, 0x0014);
        assert_eq!(delta, 0x0002_8000);
        // Outside-field bits unchanged + delta OR'd in.
        assert_eq!(rolled, 0xE002_9FFF);
    }

    #[test]
    fn mask_constant_is_bip320_canonical() {
        // BIP320 canonical mask is 0x1FFFE000 — pinned by spec, must not
        // drift. If you're tempted to change this constant, read BIP320
        // first.
        assert_eq!(BIP320_VERSION_ROLLING_MASK, 0x1FFF_E000);
        // Mask covers exactly 16 contiguous bits at positions 13..28.
        assert_eq!(BIP320_VERSION_ROLLING_MASK.count_ones(), 16);
        assert_eq!(BIP320_VERSION_ROLLING_MASK.trailing_zeros(), 13);
        assert_eq!(BIP320_VERSION_ROLLING_MASK.leading_zeros(), 3);
    }

    #[test]
    fn bip320_reconstruct_is_exhaustively_correct_for_every_u16() {
        // Exhaustive property pin (strengthens the 8-sample sweep below). For
        // EVERY 16-bit version_bits_raw, across representative base versions —
        // including one whose BIP320 field is already fully set, to prove the
        // clear-then-OR — the reconstruction must:
        //   1. reconstruct the BIP320 field to exactly (vbits << 13),
        //   2. keep the delta strictly inside the mask (no spill into 0..12 / 29..31),
        //   3. preserve every non-BIP320 bit of the base version verbatim,
        //   4. set the rolled version's BIP320 field to exactly that delta,
        //   5. yield a NON-zero delta for every non-zero vbits — the load-bearing
        //      anti-regression: the banned `version_bits_raw != 0 -> drop` guard
        //      would make the delta irrelevant and discard ~95% of valid work
        //.
        const MASK: u32 = BIP320_VERSION_ROLLING_MASK;
        for base in [
            0x0000_0000u32,
            0x2000_0000,
            0x21FF_E000,
            0xFFFF_FFFF,
            0x1234_5678,
        ] {
            for raw in 0u16..=u16::MAX {
                let (rolled, delta) = bip320_reconstruct_rolled_version(base, raw);
                let expected_delta = (raw as u32) << 13;
                assert_eq!(delta, expected_delta, "base=0x{base:08X} raw=0x{raw:04X}");
                assert_eq!(
                    delta & !MASK,
                    0,
                    "delta spilled outside the mask: raw=0x{raw:04X}"
                );
                assert_eq!(
                    rolled & !MASK,
                    base & !MASK,
                    "non-BIP320 bits not preserved: base=0x{base:08X} raw=0x{raw:04X}"
                );
                assert_eq!(
                    rolled & MASK,
                    delta,
                    "BIP320 field != delta: base=0x{base:08X} raw=0x{raw:04X}"
                );
                assert_eq!(
                    delta != 0,
                    raw != 0,
                    "delta/vbits zero-correspondence broke: raw=0x{raw:04X}"
                );
            }
        }
    }

    /// Anti-rejection-guard regression pin.
    ///
    /// This test exists specifically to FAIL if anyone ever
    /// re-introduces the form
    ///
    /// ```ignore
    /// if nr.version_bits_raw != 0 { continue; }
    /// ```
    ///
    /// in a BM1362-family share-submit path. That guard discarded ~95%
    /// of valid hashing work on AM2 XIL `a lab unit` (the
    /// 4655-RX-frames-0-nonces failure of 2026-05-15 morning) and is
    /// permanently banned by
    ///
    /// + .
    ///
    /// The contract being pinned: a non-zero `version_bits_raw` is NOT a
    /// reason to drop a nonce — it is a rolled-version that MUST be
    /// reconstructed (the chip rolls BIP320 unconditionally, regardless
    /// of `mining.configure`). For a sweep of non-zero raw values the
    /// helper must:
    ///   * produce a `rolled_version` that DIFFERS from the base
    ///     (proving the share is reconstructed, not silently passed
    ///     through identity — and certainly not dropped),
    ///   * produce a non-zero masked `delta`,
    ///   * keep `delta` strictly inside the BIP320 field,
    ///   * leave the base's outside-field bits untouched,
    ///   * be a pure function of the inputs (no hidden "drop on
    ///     non-zero" branch could satisfy all of the above).
    #[test]
    fn nonzero_version_bits_raw_is_reconstructed_never_dropped() {
        // A non-trivial base with bits set OUTSIDE the BIP320 field so we
        // can also assert outside bits survive.
        let base: u32 = 0xA000_0FFF;

        // Sweep representative non-zero raw values, including the real
        // XIL `a lab unit` milestone sample (0x0014), single-bit, low, high,
        // and full-field.
        for &raw in &[
            0x0001u16, 0x0014, 0x00FF, 0x0100, 0x1234, 0x7FFF, 0x8000, 0xFFFF,
        ] {
            let (rolled, delta) = bip320_reconstruct_rolled_version(base, raw);

            // The banned `if version_bits_raw != 0 { continue; }` guard
            // would mean these nonces never reach validate_full_header —
            // i.e. the share is discarded. The contract is the exact
            // opposite: every non-zero raw value yields a real
            // reconstructed version. Assert it is reconstructed (delta
            // non-zero) and that the rolled version actually changed
            // versus base (it is NOT a no-op / silently-passed value).
            assert_ne!(
                delta, 0,
                "raw=0x{raw:04X}: non-zero version_bits_raw must \
                 reconstruct a non-zero BIP320 delta — re-introducing \
                 the version_bits_raw!=0 rejection guard would drop \
                 this valid share (regression of \
                 )"
            );
            assert_ne!(
                rolled, base,
                "raw=0x{raw:04X}: rolled version must differ from base \
                 — the share is being reconstructed, not dropped or \
                 passed through unchanged"
            );

            // Delta is strictly the BIP320 field — no spill into bits
            // 0..12 or 29..31.
            assert_eq!(
                delta & !BIP320_VERSION_ROLLING_MASK,
                0,
                "raw=0x{raw:04X}: delta escaped the BIP320 field"
            );

            // Outside-field bits of the base are preserved verbatim.
            assert_eq!(
                rolled & !BIP320_VERSION_ROLLING_MASK,
                base & !BIP320_VERSION_ROLLING_MASK,
                "raw=0x{raw:04X}: outside-field base bits were mutated"
            );

            // Exact canonical formula:
            // rolled == (base & !mask) | ((raw << 13) & mask).
            let expected = (base & !BIP320_VERSION_ROLLING_MASK)
                | (((raw as u32) << 13) & BIP320_VERSION_ROLLING_MASK);
            assert_eq!(
                rolled, expected,
                "raw=0x{raw:04X}: reconstruction drifted from the \
                 canonical (base & !mask) | ((vbits<<13) & mask) formula"
            );
        }

        // Determinism: the helper is a pure function. A guard that
        // dropped on non-zero raw could not also be a stable pure map,
        // but pin idempotence explicitly so the contract is total.
        assert_eq!(
            bip320_reconstruct_rolled_version(base, 0x0014),
            bip320_reconstruct_rolled_version(base, 0x0014),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm1362::wire_uart_trans::CMD_WORK_PACKAGE;
    use crate::protocol::crc16;
    use std::collections::VecDeque;

    // -----------------------------------------------------------------
    // In-memory mock ChainUart
    // -----------------------------------------------------------------

    /// Mock chain UART: captures everything written; serves pre-loaded
    /// bytes on read. `fail_writes` makes every `write_frame` fail.
    #[derive(Default)]
    struct MockUart {
        tx: VecDeque<u8>,
        rx: VecDeque<u8>,
        fail_writes: bool,
        /// Max bytes returned per `read_avail` call (0 = unlimited).
        rx_chunk: usize,
    }

    impl MockUart {
        fn new() -> Self {
            Self::default()
        }
        fn with_rx(bytes: &[u8]) -> Self {
            let mut m = Self::default();
            m.rx.extend(bytes.iter().copied());
            m
        }
        fn tx_bytes(&self) -> Vec<u8> {
            self.tx.iter().copied().collect()
        }
    }

    impl ChainUart for MockUart {
        fn write_frame(&mut self, data: &[u8]) -> Result<(), UartTransportError> {
            if self.fail_writes {
                return Err(UartTransportError::WriteFailed);
            }
            self.tx.extend(data.iter().copied());
            Ok(())
        }
        fn read_avail(&mut self, buf: &mut [u8]) -> usize {
            let limit = if self.rx_chunk == 0 {
                buf.len()
            } else {
                self.rx_chunk.min(buf.len())
            };
            let n = limit.min(self.rx.len());
            for slot in buf.iter_mut().take(n) {
                *slot = self.rx.pop_front().unwrap();
            }
            n
        }
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn make_frame(seed: u32) -> AsicWorkFrame {
        let mut data2 = [0u8; 12];
        for (i, b) in data2.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u32) as u8;
        }
        let mut data = [0u8; 64];
        for (i, b) in data.iter_mut().enumerate() {
            *b = seed.wrapping_mul(31).wrapping_add(i as u32) as u8;
        }
        AsicWorkFrame {
            type_byte: CMD_WORK_PACKAGE,
            rsvd1: 0,
            job_id: (seed & 0xFF) as u8,
            rsvd2: 0,
            sno: seed,
            data2,
            data,
        }
    }

    /// Build a valid 10-byte 0x55 nonce frame with valid CRC.
    fn make_nonce_frame(chain_id: u8, job_id: u8, nonce: u32) -> [u8; 10] {
        let mut f = [0u8; 10];
        f[0] = CMD_MAGIC;
        f[1] = 0x0A;
        f[2] = chain_id;
        f[3] = job_id;
        f[4..8].copy_from_slice(&nonce.to_le_bytes());
        let crc = crc16(&f[..8]);
        f[8..10].copy_from_slice(&crc.to_le_bytes());
        f
    }

    fn transport_with_chains(n: usize, interval_us: u64) -> Am335xUartTransport<MockUart> {
        Am335xUartTransport::new((0..n).map(|_| MockUart::new()).collect(), interval_us)
    }

    // -----------------------------------------------------------------
    // Const pins
    // -----------------------------------------------------------------

    #[test]
    fn send_work_ring_slots_is_16() {
        assert_eq!(SEND_WORK_RING_SLOTS, 16);
    }

    #[test]
    fn uart_send_interval_us_is_4600() {
        assert_eq!(UART_SEND_INTERVAL_US, 4_600);
    }

    #[test]
    fn min_dispatch_interval_us_is_20000() {
        assert_eq!(MIN_DISPATCH_INTERVAL_US, 20_000);
    }

    #[test]
    fn min_floor_is_above_stock_cadence() {
        // The hard floor is *slower* than the stock hrtimer cadence —
        // i.e. the daemon's loop must not out-pace the floor. (If this
        // ever inverts, `new()`'s clamp would silently slow the stock
        // cadence down, which is a behavior change worth catching.)
        assert!(MIN_DISPATCH_INTERVAL_US > UART_SEND_INTERVAL_US);
    }

    // -----------------------------------------------------------------
    // Wire-frame fidelity
    // -----------------------------------------------------------------

    #[test]
    fn transport_writes_86_byte_wire_frame() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let frame = make_frame(0xDEAD_BEEF);
        assert_eq!(t.try_send_work(0, &frame, 1_000), Ok(true));
        let tx = t.chains[0].tx_bytes();
        assert_eq!(tx.len(), 86, "exactly one 86-byte asic_work_t on the wire");
        assert_eq!(
            tx.as_slice(),
            &wire_frame_bytes(&frame)[..],
            "byte-for-byte equal to wire_frame_bytes(frame)"
        );
    }

    #[test]
    fn wire_frame_round_trip_through_transport() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let frame = make_frame(0xCAFE_0042);
        assert_eq!(t.try_send_work(0, &frame, 5_000), Ok(true));
        let tx = t.chains[0].tx_bytes();
        let mut buf = [0u8; ASIC_WORK_SIZE];
        buf.copy_from_slice(&tx);
        let decoded = AsicWorkFrame::from_bytes(&buf).expect("transport output must round-trip");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn second_frame_appends_after_pacing_elapses() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let f0 = make_frame(1);
        let f1 = make_frame(2);
        assert_eq!(t.try_send_work(0, &f0, 0), Ok(true));
        assert_eq!(t.try_send_work(0, &f1, MIN_DISPATCH_INTERVAL_US), Ok(true));
        let tx = t.chains[0].tx_bytes();
        assert_eq!(tx.len(), 172, "two 86-byte frames back to back");
        assert_eq!(&tx[..86], &wire_frame_bytes(&f0)[..]);
        assert_eq!(&tx[86..], &wire_frame_bytes(&f1)[..]);
    }

    // -----------------------------------------------------------------
    // Pacing
    // -----------------------------------------------------------------

    #[test]
    fn try_send_work_respects_pacing_interval() {
        let interval = MIN_DISPATCH_INTERVAL_US;
        let mut t = transport_with_chains(1, interval);
        let f = make_frame(7);
        // First send always goes (even at now=0).
        assert_eq!(t.try_send_work(0, &f, 0), Ok(true));
        // Just shy of the interval → paced off.
        assert_eq!(t.try_send_work(0, &f, interval - 1), Ok(false));
        // Exactly at the interval → goes.
        assert_eq!(t.try_send_work(0, &f, interval), Ok(true));
        // Only two frames actually hit the wire.
        assert_eq!(t.chains[0].tx_bytes().len(), 172);
    }

    #[test]
    fn pacing_is_independent_per_chain() {
        let interval = MIN_DISPATCH_INTERVAL_US;
        let mut t = transport_with_chains(3, interval);
        let f = make_frame(0x33);
        // All three chains can dispatch at the same instant.
        assert_eq!(t.try_send_work(0, &f, 1000), Ok(true));
        assert_eq!(t.try_send_work(1, &f, 1000), Ok(true));
        assert_eq!(t.try_send_work(2, &f, 1000), Ok(true));
        // ...and each is independently paced off at the same instant.
        assert_eq!(t.try_send_work(0, &f, 1000), Ok(false));
        assert_eq!(t.try_send_work(1, &f, 1000), Ok(false));
        assert_eq!(t.try_send_work(2, &f, 1000), Ok(false));
    }

    #[test]
    fn dispatch_interval_clamped_to_minimum() {
        let t = transport_with_chains(1, 100);
        assert_eq!(
            t.dispatch_interval_us(),
            MIN_DISPATCH_INTERVAL_US,
            "asking for 100 µs must clamp up to the hard floor"
        );
    }

    #[test]
    fn dispatch_interval_not_clamped_when_slower() {
        let want = 50_000u64;
        let t = transport_with_chains(1, want);
        assert_eq!(
            t.dispatch_interval_us(),
            want,
            "slower-than-floor is honored"
        );
    }

    #[test]
    fn paced_off_send_does_not_record_in_ring() {
        let interval = MIN_DISPATCH_INTERVAL_US;
        let mut t = transport_with_chains(1, interval);
        let f0 = make_frame(0xAA);
        let f1 = make_frame(0xBB);
        assert_eq!(t.try_send_work(0, &f0, 0), Ok(true));
        assert_eq!(t.try_send_work(0, &f1, 1), Ok(false)); // paced off
        let ring = t.in_flight_ring(0).unwrap();
        assert_eq!(ring[0], Some(0xAA));
        assert_eq!(ring[1], None, "the paced-off frame must NOT be in the ring");
    }

    // -----------------------------------------------------------------
    // 16-slot ring
    // -----------------------------------------------------------------

    #[test]
    fn ring_buffer_has_16_slots() {
        let t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let ring = t.in_flight_ring(0).unwrap();
        assert_eq!(ring.len(), 16);
        assert!(ring.iter().all(|s| s.is_none()), "starts empty");
    }

    #[test]
    fn ring_buffer_tracks_dispatched_job_ids() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let mut now = 0u64;
        for jid in 0u8..8 {
            let f = make_frame(jid as u32); // job_id == jid
            assert_eq!(t.try_send_work(0, &f, now), Ok(true));
            now += MIN_DISPATCH_INTERVAL_US;
        }
        let ring = t.in_flight_ring(0).unwrap();
        for jid in 0u8..8 {
            assert_eq!(ring[jid as usize], Some(jid));
        }
        for slot in 8..16 {
            assert_eq!(ring[slot], None);
        }
    }

    #[test]
    fn ring_buffer_wraps_drop_oldest() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        let mut now = 0u64;
        // Dispatch 20 frames with job_id = 0..20 (low 8 bits).
        for jid in 0u32..20 {
            let f = make_frame(jid);
            assert_eq!(t.try_send_work(0, &f, now), Ok(true));
            now += MIN_DISPATCH_INTERVAL_US;
        }
        let ring = t.in_flight_ring(0).unwrap();
        // 20 frames into a 16-slot ring: slots hold job_ids 16..20 (the
        // most recent 4 wrote over slots 0..4) and 4..16 (unchanged).
        // i.e. slot k holds: k in 0..4 → 16+k ; k in 4..16 → k.
        for k in 0usize..16 {
            let expect = if k < 4 { (16 + k) as u8 } else { k as u8 };
            assert_eq!(ring[k], Some(expect), "slot {} after wrap", k);
        }
        // The 16 most-recent job_ids (4..=19) are all present somewhere.
        let present: std::collections::HashSet<u8> = ring.iter().filter_map(|s| *s).collect();
        for jid in 4u8..20 {
            assert!(
                present.contains(&jid),
                "recent job_id {} must still be tracked",
                jid
            );
        }
        // The oldest (0..=3) have been dropped.
        for jid in 0u8..4 {
            assert!(
                !present.contains(&jid),
                "stale job_id {} must be dropped",
                jid
            );
        }
    }

    // -----------------------------------------------------------------
    // recv_nonces
    // -----------------------------------------------------------------

    #[test]
    fn recv_nonces_parses_valid_frame() {
        let frame = make_nonce_frame(0x07, 0x2A, 0x1234_5678);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&frame)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_nonces();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 0);
        assert_eq!(
            got[0].1,
            NonceResponse {
                chain_id: 0x07,
                job_id: 0x2A,
                nonce: 0x1234_5678
            }
        );
    }

    #[test]
    fn recv_nonces_empty_when_no_rx() {
        let mut t = transport_with_chains(2, MIN_DISPATCH_INTERVAL_US);
        assert!(t.recv_nonces().is_empty());
    }

    #[test]
    fn recv_nonces_resyncs_past_garbage() {
        let valid = make_nonce_frame(0x01, 0x02, 0xABCD_EF01);
        let mut bytes = vec![0xFFu8, 0x00, 0x13, 0x37];
        bytes.extend_from_slice(&valid);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&bytes)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_nonces();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.nonce, 0xABCD_EF01);
    }

    #[test]
    fn recv_nonces_handles_leading_0x55_garbage() {
        // A stray 0x55 that is NOT a frame start (followed by junk),
        // then a real frame. parse fails on the junk window → resync by
        // one byte → eventually finds the real frame.
        let valid = make_nonce_frame(0xCC, 0x09, 0xDEAD_0000);
        let mut bytes = vec![0x55u8, 0x55, 0x55, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE];
        bytes.extend_from_slice(&valid);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&bytes)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_nonces();
        assert!(got
            .iter()
            .any(|(_, nr)| nr.nonce == 0xDEAD_0000 && nr.chain_id == 0xCC));
    }

    #[test]
    fn recv_nonces_skips_bad_crc() {
        let mut frame = make_nonce_frame(0x01, 0x02, 0x0304_0506);
        frame[8] ^= 0xFF; // corrupt CRC
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&frame)], MIN_DISPATCH_INTERVAL_US);
        assert!(t.recv_nonces().is_empty(), "bad-CRC frame must be skipped");
    }

    #[test]
    fn recv_nonces_skips_wrong_magic_only_frame() {
        // 10 bytes, no 0x55 anywhere → nothing parsed.
        let buf = [0xAAu8; 10];
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&buf)], MIN_DISPATCH_INTERVAL_US);
        assert!(t.recv_nonces().is_empty());
    }

    #[test]
    fn recv_nonces_two_frames_back_to_back() {
        let f1 = make_nonce_frame(0x00, 0x10, 0x1111_1111);
        let f2 = make_nonce_frame(0x00, 0x11, 0x2222_2222);
        let mut bytes = f1.to_vec();
        bytes.extend_from_slice(&f2);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&bytes)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_nonces();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1.nonce, 0x1111_1111);
        assert_eq!(got[1].1.nonce, 0x2222_2222);
    }

    #[test]
    fn recv_nonces_multichain() {
        let f0 = make_nonce_frame(0x00, 0x01, 0x0A0A_0A0A);
        let f2 = make_nonce_frame(0x02, 0x03, 0x0C0C_0C0C);
        let chains = vec![
            MockUart::with_rx(&f0),
            MockUart::new(),
            MockUart::with_rx(&f2),
            MockUart::new(),
        ];
        let mut t = Am335xUartTransport::new(chains, MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_nonces();
        assert_eq!(got.len(), 2);
        // chain 0 frame first, then chain 2 frame.
        assert_eq!(got[0].0, 0);
        assert_eq!(got[0].1.nonce, 0x0A0A_0A0A);
        assert_eq!(got[1].0, 2);
        assert_eq!(got[1].1.nonce, 0x0C0C_0C0C);
    }

    #[test]
    fn recv_nonces_drains_across_chunked_reads() {
        // RX served in 4-byte chunks; a full 10-byte frame straddles
        // chunk boundaries. The transport must keep reading until the
        // chain reports nothing more. (This particular MockUart cannot
        // carry a partial across reads, so we feed it as one buffer but
        // throttle the chunk size — proving the drain loop iterates.)
        let frame = make_nonce_frame(0x05, 0x06, 0x5050_5050);
        // Pad so total length > scratch size would still terminate; here
        // just exercise the chunked-read loop with chunk=4.
        let mut m = MockUart::with_rx(&frame);
        m.rx_chunk = 4;
        let mut t = Am335xUartTransport::new(vec![m], MIN_DISPATCH_INTERVAL_US);
        // First read returns 4 bytes [55 0A 05 06] — parse fails on a
        // too-short tail, loop continues; next reads bring the rest, but
        // since the magic-near-tail handler dropped it on the first pass
        // there's nothing to recover. This documents the scratch-poller
        // limitation: a real DevmemUart read returns as much as the FIFO
        // holds in one go, so straddling is rare. Assert the loop at
        // least terminates and yields *something or nothing* without
        // panicking.
        let _ = t.recv_nonces();
    }

    // -----------------------------------------------------------------
    // clean_work
    // -----------------------------------------------------------------

    #[test]
    fn clean_work_clears_in_flight() {
        let mut t = transport_with_chains(2, MIN_DISPATCH_INTERVAL_US);
        let mut now = 0u64;
        for jid in 0u32..3 {
            assert_eq!(t.try_send_work(0, &make_frame(jid), now), Ok(true));
            assert_eq!(t.try_send_work(1, &make_frame(jid + 100), now), Ok(true));
            now += MIN_DISPATCH_INTERVAL_US;
        }
        assert!(t.in_flight_ring(0).unwrap().iter().any(|s| s.is_some()));
        assert!(t.in_flight_ring(1).unwrap().iter().any(|s| s.is_some()));
        t.clean_work();
        assert!(t.in_flight_ring(0).unwrap().iter().all(|s| s.is_none()));
        assert!(t.in_flight_ring(1).unwrap().iter().all(|s| s.is_none()));
        // Ring slot pointer also reset → next dispatch lands in slot 0.
        now += MIN_DISPATCH_INTERVAL_US;
        assert_eq!(t.try_send_work(0, &make_frame(0x5A), now), Ok(true));
        assert_eq!(t.in_flight_ring(0).unwrap()[0], Some(0x5A));
    }

    #[test]
    fn clean_work_does_not_reset_pacing() {
        let interval = MIN_DISPATCH_INTERVAL_US;
        let mut t = transport_with_chains(1, interval);
        assert_eq!(t.try_send_work(0, &make_frame(1), 0), Ok(true));
        t.clean_work();
        // Still paced off — clean-jobs doesn't license a faster burst.
        assert_eq!(t.try_send_work(0, &make_frame(2), interval - 1), Ok(false));
        assert_eq!(t.try_send_work(0, &make_frame(2), interval), Ok(true));
    }

    // -----------------------------------------------------------------
    // Error paths
    // -----------------------------------------------------------------

    #[test]
    fn chain_out_of_range_rejected() {
        let mut t = transport_with_chains(4, MIN_DISPATCH_INTERVAL_US);
        assert_eq!(
            t.try_send_work(4, &make_frame(0), 1000),
            Err(UartTransportError::ChainOutOfRange)
        );
        assert_eq!(
            t.try_send_work(99, &make_frame(0), 1000),
            Err(UartTransportError::ChainOutOfRange)
        );
        // in_flight_ring also bounds-checks.
        assert!(t.in_flight_ring(4).is_none());
    }

    #[test]
    fn write_failure_propagates_and_leaves_state_clean() {
        let mut chains = vec![MockUart::new()];
        chains[0].fail_writes = true;
        let mut t = Am335xUartTransport::new(chains, MIN_DISPATCH_INTERVAL_US);
        let f = make_frame(0x99);
        assert_eq!(
            t.try_send_work(0, &f, 1000),
            Err(UartTransportError::WriteFailed)
        );
        // Ring + pacing untouched → caller can retry.
        assert!(t.in_flight_ring(0).unwrap().iter().all(|s| s.is_none()));
        // Now allow writes; the retry at the *same* now_us succeeds
        // because the failed attempt didn't advance the pacing clock.
        t.chains[0].fail_writes = false;
        assert_eq!(t.try_send_work(0, &f, 1000), Ok(true));
        assert_eq!(t.in_flight_ring(0).unwrap()[0], Some(0x99));
    }

    #[test]
    fn chain_count_reflects_constructor() {
        assert_eq!(transport_with_chains(0, 1).chain_count(), 0);
        assert_eq!(transport_with_chains(1, 1).chain_count(), 1);
        assert_eq!(transport_with_chains(3, 1).chain_count(), 3);
    }

    #[test]
    fn zero_chain_transport_recv_is_empty() {
        let mut t = transport_with_chains(0, MIN_DISPATCH_INTERVAL_US);
        assert!(t.recv_nonces().is_empty());
        // And sending to chain 0 is out-of-range.
        assert_eq!(
            t.try_send_work(0, &make_frame(0), 1),
            Err(UartTransportError::ChainOutOfRange)
        );
    }

    // -----------------------------------------------------------------
    // R7-3: try_send_raw + BM1362 serial-wire nonce parsing (the PROVEN
    // format — distinct from the W14.B AsicWorkFrame/parse_nonce_frame codec)
    // -----------------------------------------------------------------

    /// Build a valid 11-byte BM1362 serial-wire nonce frame.
    fn make_bm1362_serial_nonce(
        nonce_le: u32,
        midstate_idx: u8,
        job_id: u8,
        small_core: u8,
        vbits_raw: u16,
        job_response: bool,
    ) -> [u8; BM1362_SERIAL_NONCE_FRAME_LEN] {
        let mut f = [0u8; BM1362_SERIAL_NONCE_FRAME_LEN];
        f[0] = SERIAL_RESP_PREAMBLE[0]; // 0xAA
        f[1] = SERIAL_RESP_PREAMBLE[1]; // 0x55
        f[2..6].copy_from_slice(&nonce_le.to_le_bytes());
        f[6] = midstate_idx;
        // result byte: job_id encoded << 1 in the high nibble, small_core low nibble
        f[7] = ((job_id << 1) & 0xF0) | (small_core & 0x0F);
        f[8..10].copy_from_slice(&vbits_raw.to_be_bytes());
        f[10] = if job_response { 0x80 } else { 0x00 };
        f
    }

    #[test]
    fn serial_nonce_consts() {
        assert_eq!(SERIAL_RESP_PREAMBLE, [0xAA, 0x55]);
        assert_eq!(BM1362_SERIAL_NONCE_FRAME_LEN, 11);
    }

    #[test]
    fn parse_bm1362_serial_nonce_byte_exact() {
        // job_id 24 (0x18) → result high nibble = (0x18 << 1) & 0xF0 = 0x30.
        let raw = make_bm1362_serial_nonce(0x1234_5678, 0, 24, 6, 0x0A00, true);
        let nr = parse_bm1362_serial_nonce(&raw).expect("valid job-response frame");
        assert_eq!(nr.nonce, 0x1234_5678);
        assert_eq!(nr.job_id, 24, "(0x36 & 0xF0) >> 1 == 0x18 == 24");
        assert_eq!(nr.result_byte, 0x36);
        assert_eq!(nr.small_core, 6);
        assert_eq!(nr.midstate_idx, 0);
        assert_eq!(nr.version_bits_raw, 0x0A00);
        assert_eq!(nr.flags & 0x80, 0x80);
        // Verify the nonce bytes are LE-interpreted (ESP-Miner convention).
        assert_eq!(&raw[2..6], &[0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn parse_bm1362_serial_nonce_rejects_bad_preamble_and_short() {
        let mut raw = make_bm1362_serial_nonce(1, 0, 0, 0, 0, true);
        raw[0] = 0x55; // not 0xAA
        assert!(parse_bm1362_serial_nonce(&raw).is_none());
        assert!(
            parse_bm1362_serial_nonce(&[0xAA, 0x55, 0x00]).is_none(),
            "too short"
        );
        // A non-job-response frame still parses (flags exposed; caller filters).
        let raw = make_bm1362_serial_nonce(7, 0, 0, 0, 0, false);
        let nr = parse_bm1362_serial_nonce(&raw).expect("parses; caller filters on flags");
        assert_eq!(nr.flags & 0x80, 0);
    }

    #[test]
    fn try_send_raw_writes_frame_and_records_job_id() {
        let mut t = transport_with_chains(1, MIN_DISPATCH_INTERVAL_US);
        // A stand-in "88-byte BM1362 full-header frame" (content irrelevant to the transport).
        let frame: Vec<u8> = (0u8..88).collect();
        assert_eq!(t.try_send_raw(0, &frame, 0x42, 1_000), Ok(true));
        assert_eq!(
            t.chains[0].tx_bytes(),
            frame,
            "raw frame written byte-for-byte"
        );
        assert_eq!(t.in_flight_ring(0).unwrap()[0], Some(0x42));
        // Pacing applies to try_send_raw too.
        assert_eq!(t.try_send_raw(0, &frame, 0x43, 1_000), Ok(false));
        assert_eq!(
            t.try_send_raw(99, &frame, 0, 1_000),
            Err(UartTransportError::ChainOutOfRange)
        );
    }

    #[test]
    fn per_chain_collections_match_chain_count_so_the_dispatch_bounds_check_is_sound() {
        // Index-panic sweep pin. The dispatch fns bounds-check
        // `chain_idx >= self.chains.len()` but then index BOTH self.chains[idx] AND
        // self.state[idx] (+ the per-chain counter vecs). That single check is only
        // sound if every per-chain collection has the SAME length as chains. Pin it:
        // for N chains there are N per-chain state rows; chain_idx == N (one past the
        // end) is rejected as ChainOutOfRange (so state[N] is never touched) while
        // N-1 is in bounds and does not panic. A future edit that sized `state`
        // differently from `chains` would fail HERE instead of panicking at runtime
        // (panic=abort -> a crash with no teardown on the mining hot path).
        let frame = [0u8; BM1362_SERIAL_NONCE_FRAME_LEN];
        for n in [1usize, 2, 3, 4] {
            let chains: Vec<MockUart> = (0..n).map(|_| MockUart::new()).collect();
            let mut t = Am335xUartTransport::new(chains, MIN_DISPATCH_INTERVAL_US);
            assert_eq!(
                t.bm1362_serial_rx_counters().len(),
                n,
                "per-chain state must be sized to the chain count"
            );
            assert_eq!(
                t.try_send_raw(n, &frame, 0, 1_000),
                Err(UartTransportError::ChainOutOfRange),
                "chain_idx == len must be rejected (state[len] must never be indexed)"
            );
            assert_ne!(
                t.try_send_raw(n - 1, &frame, 0, 1_000),
                Err(UartTransportError::ChainOutOfRange),
                "the last valid chain_idx must be in bounds"
            );
        }
    }

    #[test]
    fn recv_bm1362_serial_nonces_never_panics_and_caps_the_carry_buffer_on_garbage() {
        // Fuzz the streaming nonce parser (priority 1: production risk). A faulty
        // or hostile chain spewing arbitrary bytes — no preamble, half-frames,
        // endless 0xAA/0x55 bait, kilobytes of noise, split across tiny reads —
        // must NEVER panic and must NEVER let the per-chain carry buffer grow past
        // the defensive cap; otherwise a stuck chain could OOM the daemon. Uses a
        // deterministic LCG (reproducible; the harness forbids RNG) so any failure
        // replays from the fixed seed.
        let mut lcg: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        for case in 0..400u32 {
            let len = (next() % 5000) as usize;
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                bytes.push(match next() % 5 {
                    0 => 0xAA, // preamble byte 1 — bait the resync scanner
                    1 => 0x55, // preamble byte 2
                    2 => 0x00,
                    _ => (next() & 0xFF) as u8,
                });
            }
            let mut mock = MockUart::with_rx(&bytes);
            if case % 3 == 0 {
                // Exercise the carry-across-reads path (a frame straddling reads).
                mock.rx_chunk = 1 + (next() % 7) as usize;
            }
            let mut t = Am335xUartTransport::new(vec![mock], MIN_DISPATCH_INTERVAL_US);
            let _ = t.recv_bm1362_serial_nonces(); // must not panic on any input
            for c in t.bm1362_serial_rx_counters() {
                assert!(
                    c.buffered_bytes <= BM1362_SERIAL_RX_MAX_BUFFER,
                    "case {case}: carry buffer {} exceeded the cap {BM1362_SERIAL_RX_MAX_BUFFER}",
                    c.buffered_bytes
                );
            }
        }
    }

    #[test]
    fn recv_bm1362_serial_nonces_parses_and_resyncs() {
        let f1 = make_bm1362_serial_nonce(0xAABB_CCDD, 0, 8, 1, 0x0100, true);
        let f2 = make_bm1362_serial_nonce(0x1111_2222, 0, 16, 2, 0x0000, true);
        let mut bytes = vec![0x00u8, 0xFF, 0x13]; // garbage to resync past
        bytes.extend_from_slice(&f1);
        bytes.extend_from_slice(&f2);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&bytes)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_bm1362_serial_nonces();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 0);
        assert_eq!(got[0].1.nonce, 0xAABB_CCDD);
        assert_eq!(got[0].1.job_id, 8);
        assert_eq!(got[1].1.nonce, 0x1111_2222);
        assert_eq!(got[1].1.job_id, 16);
        // Empty RX → empty.
        let mut t2 = transport_with_chains(2, MIN_DISPATCH_INTERVAL_US);
        assert!(t2.recv_bm1362_serial_nonces().is_empty());
    }

    #[test]
    fn recv_bm1362_serial_nonces_preserves_fragment_across_polls() {
        let f = make_bm1362_serial_nonce(0x1020_3040, 0, 24, 4, 0x0200, true);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&f[..5])], MIN_DISPATCH_INTERVAL_US);

        assert!(
            t.recv_bm1362_serial_nonces().is_empty(),
            "partial serial frame is retained, not emitted"
        );
        let counters = t.bm1362_serial_rx_counters();
        assert_eq!(counters[0].raw_bytes, 5);
        assert_eq!(counters[0].parsed_frames, 0);
        assert_eq!(counters[0].buffered_bytes, 5);
        t.chains[0].rx.extend(f[5..].iter().copied());

        let got = t.recv_bm1362_serial_nonces();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 0);
        assert_eq!(got[0].1.nonce, 0x1020_3040);
        assert_eq!(got[0].1.job_id, 24);
        let counters = t.bm1362_serial_rx_counters();
        assert_eq!(counters[0].raw_bytes, BM1362_SERIAL_NONCE_FRAME_LEN as u64);
        assert_eq!(counters[0].parsed_frames, 1);
        assert_eq!(counters[0].job_response_frames, 1);
        assert_eq!(counters[0].non_job_response_frames, 0);
        assert_eq!(counters[0].buffered_bytes, 0);
    }

    #[test]
    fn recv_bm1362_serial_nonces_classifies_non_job_frames() {
        let job = make_bm1362_serial_nonce(0x1111_2222, 0, 8, 1, 0, true);
        let non_job = make_bm1362_serial_nonce(0x3333_4444, 0, 16, 2, 0, false);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&job);
        bytes.extend_from_slice(&non_job);
        let mut t =
            Am335xUartTransport::new(vec![MockUart::with_rx(&bytes)], MIN_DISPATCH_INTERVAL_US);

        let got = t.recv_bm1362_serial_nonces();
        assert_eq!(got.len(), 2);
        assert_ne!(got[0].1.flags & 0x80, 0);
        assert_eq!(got[1].1.flags & 0x80, 0);
        let counters = t.bm1362_serial_rx_counters();
        assert_eq!(counters[0].parsed_frames, 2);
        assert_eq!(counters[0].job_response_frames, 1);
        assert_eq!(counters[0].non_job_response_frames, 1);
    }

    #[test]
    fn recv_bm1362_serial_nonces_drains_chunked_reads_in_one_poll() {
        let f = make_bm1362_serial_nonce(0x5566_7788, 0, 32, 9, 0x0400, true);
        let mut uart = MockUart::with_rx(&f);
        uart.rx_chunk = 4;
        let mut t = Am335xUartTransport::new(vec![uart], MIN_DISPATCH_INTERVAL_US);

        let got = t.recv_bm1362_serial_nonces();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.nonce, 0x5566_7788);
        assert_eq!(got[0].1.job_id, 32);
        let counters = t.bm1362_serial_rx_counters();
        assert_eq!(counters[0].raw_bytes, BM1362_SERIAL_NONCE_FRAME_LEN as u64);
        assert_eq!(counters[0].parsed_frames, 1);
        assert_eq!(counters[0].buffered_bytes, 0);
    }

    #[test]
    fn recv_bm1362_serial_nonces_does_not_rescan_inside_a_frame() {
        // A frame whose body happens to contain the 0xAA 0x55 byte pair must
        // not be re-parsed from the middle — the scanner advances by the full
        // 11 bytes after a matched preamble.
        let f = make_bm1362_serial_nonce(0x55AA_55AA, 0xAA, 0x2A, 5, 0x55AA, true);
        let mut t = Am335xUartTransport::new(vec![MockUart::with_rx(&f)], MIN_DISPATCH_INTERVAL_US);
        let got = t.recv_bm1362_serial_nonces();
        assert_eq!(
            got.len(),
            1,
            "exactly one frame, not re-parsed from an inner 0xAA55"
        );
        assert_eq!(got[0].1.nonce, 0x55AA_55AA);
    }
}
