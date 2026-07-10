//! Clean-room userspace replacement for stock Bitmain BB `uart_trans.ko`.
//!
//! The stock AM335x BeagleBone Black firmware uses a proprietary kernel module
//! as a batching/framing layer above `/dev/ttyO{1,2,4,5}`. Reverse-engineering
//! evidence shows it does not own the UART hardware directly; the normal
//! `omap-serial` driver does. This module keeps the same behavioral shape in
//! Rust userspace: bounded per-chain work queues, ioctl-equivalent control
//! methods, CRC-CCITT work frames, and BM1362 nonce parsing.
//!
//! Runtime shape:  originally named `tokio::fs::File` handles. The
//! static integration keeps this ASIC crate synchronous and uses the daemon's
//! existing dedicated serial I/O thread pattern so UART blocking behavior does
//! not starve the tokio scheduler. See
//! .
//!
//! This is intentionally not a kernel-module compatibility shim and does not
//! copy proprietary source.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::drivers::bm1362;
use crate::drivers::MiningWork;
use crate::protocol;
use crate::{AsicError, Result};
use dcentrald_hal::serial::{SerialChain, BAUD_115200};

/// Stock BB platform exposes four ASIC UARTs.
pub const CHAIN_COUNT: usize = 4;

/// `/dev/ttyO3` is disabled in the Bitmain BB device tree; chain 2 is ttyO4.
pub const DEFAULT_CHAIN_TTYS: [&str; CHAIN_COUNT] =
    ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"];

/// The stock module's queued work item is 86 bytes: header + length + payload + CRC16.
pub const UART_TRANS_FRAME_LEN: usize = 86;

/// Direct BM1362 UART writes prepend the normal command preamble.
pub const UART_TRANS_WIRE_FRAME_LEN: usize = 88;

/// Conservative first-pass queue depth until the DWARF queue-count evidence is
/// live-cross-checked on `a lab unit`.
pub const DEFAULT_WORK_QUEUE_COUNT: usize = 16;

/// hrtimer-equivalent first-pass batch interval. The exact stock period still
/// needs live confirmation; 50 ms is safely below the ~130 frames/s UART limit
/// at 115200 while avoiding a CPU-burning busy loop on Cortex-A8.
pub const DEFAULT_SEND_INTERVAL: Duration = Duration::from_millis(50);

/// Bound for per-chain byte-stream buffers before resync discards old junk.
const RX_BUFFER_LIMIT: usize = 256;

/// Bound for parsed nonce backlog held by the service worker.
const NONCE_QUEUE_LIMIT: usize = 1024;

/// One queued 86-byte `uart_trans` work frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartWork {
    frame: [u8; UART_TRANS_FRAME_LEN],
    asic_job_id: u8,
}

impl UartWork {
    /// Build a BB `uart_trans` frame from the canonical BM1362 serial-work builder.
    pub fn from_bm1362_work(work: &MiningWork, asic_job_id: u8) -> Self {
        let wire = bm1362::build_serial_work_frame(work, asic_job_id);
        let mut frame = [0u8; UART_TRANS_FRAME_LEN];
        frame.copy_from_slice(&wire[2..]);
        Self { frame, asic_job_id }
    }

    /// Build a BB `uart_trans` frame from the daemon's existing BM1362 command
    /// frame: header + length + 82-byte payload, without preamble or CRC.
    pub fn from_command_frame(command: &[u8]) -> Result<Self> {
        if command.len() != UART_TRANS_FRAME_LEN - 2 {
            return Err(AsicError::InvalidParameter(format!(
                "uart_trans BM1362 command frame must be {} bytes, got {}",
                UART_TRANS_FRAME_LEN - 2,
                command.len()
            )));
        }
        if command[0] != 0x21 || command[1] != 0x56 {
            return Err(AsicError::InvalidParameter(format!(
                "uart_trans BM1362 command frame has invalid header/len {:02X} {:02X}",
                command[0], command[1]
            )));
        }

        let mut frame = [0u8; UART_TRANS_FRAME_LEN];
        frame[..command.len()].copy_from_slice(command);
        let crc = protocol::crc16(command);
        frame[84] = (crc >> 8) as u8;
        frame[85] = (crc & 0xFF) as u8;
        let work = Self {
            frame,
            asic_job_id: command[2],
        };
        debug_assert!(work.crc_valid());
        Ok(work)
    }

    /// Build a BB `uart_trans` frame from the full direct UART wire frame:
    /// `55 AA` preamble + 86-byte stock-module body.
    pub fn from_wire_frame(wire: &[u8]) -> Result<Self> {
        if wire.len() != UART_TRANS_WIRE_FRAME_LEN {
            return Err(AsicError::InvalidParameter(format!(
                "uart_trans BM1362 wire frame must be {} bytes, got {}",
                UART_TRANS_WIRE_FRAME_LEN,
                wire.len()
            )));
        }
        if wire[0] != 0x55 || wire[1] != 0xAA {
            return Err(AsicError::InvalidParameter(
                "uart_trans BM1362 wire frame missing 55 AA preamble".into(),
            ));
        }

        let mut frame = [0u8; UART_TRANS_FRAME_LEN];
        frame.copy_from_slice(&wire[2..]);
        let work = Self {
            frame,
            asic_job_id: frame[2],
        };
        if !work.crc_valid() {
            return Err(AsicError::InvalidParameter(
                "uart_trans BM1362 wire frame CRC mismatch".into(),
            ));
        }
        Ok(work)
    }

    pub fn as_frame_86(&self) -> &[u8; UART_TRANS_FRAME_LEN] {
        &self.frame
    }

    pub fn asic_job_id(&self) -> u8 {
        self.asic_job_id
    }

    /// Expand the stock-module 86-byte body to a direct ASIC UART wire frame.
    pub fn to_wire_frame(&self) -> [u8; UART_TRANS_WIRE_FRAME_LEN] {
        let mut out = [0u8; UART_TRANS_WIRE_FRAME_LEN];
        out[0] = 0x55;
        out[1] = 0xAA;
        out[2..].copy_from_slice(&self.frame);
        out
    }

    /// Validate the CRC-CCITT tag at frame bytes 84..86.
    pub fn crc_valid(&self) -> bool {
        let expected = protocol::crc16(&self.frame[..84]);
        let actual = ((self.frame[84] as u16) << 8) | self.frame[85] as u16;
        actual == expected
    }
}

/// Parsed BM1362 nonce response from the BB UART path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartNonce {
    pub nonce: u32,
    pub asic_job_id: u8,
    pub midstate_idx: u8,
    pub version_bits_raw: u16,
    pub small_core: u8,
    pub flags: u8,
}

impl UartNonce {
    /// Return the 9-byte BM1362 nonce body expected by the existing serial
    /// mining validation path after a lower layer strips `AA 55`.
    pub fn to_bm1362_body(self) -> [u8; 9] {
        let mut body = [0u8; 9];
        body[..4].copy_from_slice(&self.nonce.to_le_bytes());
        body[4] = self.midstate_idx;
        body[5] = ((((self.asic_job_id as u16) << 1) as u8) & 0xF0) | (self.small_core & 0x0F);
        body[6..8].copy_from_slice(&self.version_bits_raw.to_be_bytes());
        body[8] = self.flags;
        body
    }
}

/// Parse a BM1362 serial nonce response.
///
/// Accepts either the full 11-byte wire frame (`AA 55` + 9-byte body) or the
/// 9-byte body after a lower layer has stripped the preamble.
pub fn parse_bm1362_nonce_frame(raw: &[u8]) -> Option<UartNonce> {
    let body = match raw.len() {
        11 if raw[0] == 0xAA && raw[1] == 0x55 => &raw[2..],
        9 => raw,
        _ => return None,
    };

    let flags = body[8];
    if flags & 0x80 == 0 {
        return None;
    }

    let id_byte = body[5];
    Some(UartNonce {
        nonce: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
        asic_job_id: (id_byte & 0xF0) >> 1,
        midstate_idx: body[4],
        version_bits_raw: u16::from_be_bytes([body[6], body[7]]),
        small_core: id_byte & 0x0F,
        flags,
    })
}

/// Userspace `uart_trans` service facade.
pub struct UartTransService {
    tty_chains: Vec<SerialChain>,
    queues: [VecDeque<UartWork>; CHAIN_COUNT],
    rx_buffers: [Vec<u8>; CHAIN_COUNT],
    nonce_queue: VecDeque<(usize, UartNonce)>,
    chain_exist_bits: u32,
    work_queue_count: usize,
    timer_running: bool,
    send_interval: Duration,
}

impl UartTransService {
    /// Open `/dev/ttyO{1,2,4,5}` with stock-like flags at the BM1362 reset baud.
    pub fn open_default() -> Result<Self> {
        Self::open_paths_with_baud(DEFAULT_CHAIN_TTYS, BAUD_115200)
    }

    pub fn open_paths<P: AsRef<Path>, const N: usize>(paths: [P; N]) -> Result<Self> {
        Self::open_paths_with_baud(paths, BAUD_115200)
    }

    pub fn open_paths_with_baud<P: AsRef<Path>, const N: usize>(
        paths: [P; N],
        baud: u32,
    ) -> Result<Self> {
        if N != CHAIN_COUNT {
            return Err(AsicError::InvalidParameter(format!(
                "uart_trans requires {} tty paths, got {}",
                CHAIN_COUNT, N
            )));
        }

        let mut tty_chains = Vec::with_capacity(CHAIN_COUNT);
        for path in paths {
            tty_chains.push(open_tty(path.as_ref(), baud)?);
        }

        Ok(Self::new_with_ttys(tty_chains))
    }

    /// Constructor for unit tests and dry-run queue validation.
    pub fn without_ttys_for_test() -> Self {
        Self::new_with_ttys(Vec::new())
    }

    fn new_with_ttys(tty_chains: Vec<SerialChain>) -> Self {
        Self {
            tty_chains,
            queues: std::array::from_fn(|_| VecDeque::new()),
            rx_buffers: std::array::from_fn(|_| Vec::new()),
            nonce_queue: VecDeque::new(),
            chain_exist_bits: 0,
            work_queue_count: DEFAULT_WORK_QUEUE_COUNT,
            timer_running: false,
            send_interval: DEFAULT_SEND_INTERVAL,
        }
    }

    /// ioctl-equivalent: SET_CHAIN_EXIST_BITS.
    pub fn set_chain_exist_bits(&mut self, bits: u32) {
        self.chain_exist_bits = bits & 0x0F;
    }

    pub fn chain_exist_bits(&self) -> u32 {
        self.chain_exist_bits
    }

    /// ioctl-equivalent: SET_WORK_QUEUE_COUNT.
    pub fn set_work_queue_count(&mut self, count: usize) -> Result<()> {
        if count == 0 {
            return Err(AsicError::InvalidParameter(
                "uart_trans work queue count must be > 0".into(),
            ));
        }
        self.work_queue_count = count;
        for q in &mut self.queues {
            while q.len() > count {
                q.pop_front();
            }
        }
        Ok(())
    }

    pub fn work_queue_count(&self) -> usize {
        self.work_queue_count
    }

    /// ioctl-equivalent: START_SEND_WORK_TIMER.
    pub fn start_send_work_timer(&mut self) {
        self.timer_running = true;
    }

    /// ioctl-equivalent: STOP_SEND_WORK_TIMER.
    pub fn stop_send_work_timer(&mut self) {
        self.timer_running = false;
    }

    pub fn timer_running(&self) -> bool {
        self.timer_running
    }

    pub fn send_interval(&self) -> Duration {
        self.send_interval
    }

    pub fn set_send_interval(&mut self, interval: Duration) -> Result<()> {
        if interval.is_zero() {
            return Err(AsicError::InvalidParameter(
                "uart_trans send interval must be > 0".into(),
            ));
        }
        self.send_interval = interval;
        Ok(())
    }

    /// Change baud on every opened chain. Used after BM1362 fast-baud init.
    pub fn set_baud_all(&mut self, baud: u32) -> Result<()> {
        for chain in &mut self.tty_chains {
            chain.set_baud(baud)?;
            chain.set_nonblocking()?;
        }
        Ok(())
    }

    /// Queue a work item for one chain, enforcing the configured bounded depth.
    pub fn enqueue_work(&mut self, chain: usize, work: UartWork) -> Result<()> {
        let count = self.work_queue_count;
        let q = self.queue_mut(chain)?;
        if q.len() >= count {
            q.pop_front();
        }
        q.push_back(work);
        Ok(())
    }

    pub fn queue_len(&self, chain: usize) -> Result<usize> {
        Ok(self.queue(chain)?.len())
    }

    /// Pop one queued frame for a chain as a direct UART wire frame.
    pub fn pop_next_wire_frame(
        &mut self,
        chain: usize,
    ) -> Result<Option<[u8; UART_TRANS_WIRE_FRAME_LEN]>> {
        Ok(self
            .queue_mut(chain)?
            .pop_front()
            .map(|w| w.to_wire_frame()))
    }

    /// Pop one queued stock-module body frame without the direct-serial preamble.
    pub fn pop_next_frame_86(
        &mut self,
        chain: usize,
    ) -> Result<Option<[u8; UART_TRANS_FRAME_LEN]>> {
        Ok(self.queue_mut(chain)?.pop_front().map(|w| *w.as_frame_86()))
    }

    /// Send one queued work frame per existing chain. Intended to be called by
    /// the future timer task at [`DEFAULT_SEND_INTERVAL`].
    pub fn send_due_work_once(&mut self) -> Result<usize> {
        if !self.timer_running {
            return Ok(0);
        }

        let mut sent = 0usize;
        for chain in 0..CHAIN_COUNT {
            if self.chain_exist_bits & (1 << chain) == 0 {
                continue;
            }
            let Some(frame) = self.pop_next_wire_frame(chain)? else {
                continue;
            };
            let Some(tty) = self.tty_chains.get_mut(chain) else {
                return Err(AsicError::InvalidParameter(format!(
                    "uart_trans chain {} has no tty handle",
                    chain
                )));
            };
            tty.write_bytes(&frame)?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Poll all opened UARTs once and parse BM1362 nonce frames from byte streams.
    pub fn poll_nonces_once(&mut self) -> Result<Vec<(usize, UartNonce)>> {
        let mut out = Vec::new();
        let mut scratch = [0u8; 128];

        for chain in 0..CHAIN_COUNT {
            loop {
                let n = {
                    let Some(tty) = self.tty_chains.get_mut(chain) else {
                        break;
                    };
                    tty.read_bytes(&mut scratch)?
                };
                if n == 0 {
                    break;
                }
                self.rx_buffers[chain].extend_from_slice(&scratch[..n]);
                Self::drain_nonce_frames(chain, &mut self.rx_buffers[chain], &mut out);
            }
        }

        Ok(out)
    }

    /// Poll UART RX and append parsed nonce frames to the service backlog.
    pub fn poll_nonces_into_queue_once(&mut self) -> Result<usize> {
        let nonces = self.poll_nonces_once()?;
        let count = nonces.len();
        for nonce in nonces {
            if self.nonce_queue.len() >= NONCE_QUEUE_LIMIT {
                self.nonce_queue.pop_front();
            }
            self.nonce_queue.push_back(nonce);
        }
        Ok(count)
    }

    pub fn pop_next_nonce(&mut self) -> Option<(usize, UartNonce)> {
        self.nonce_queue.pop_front()
    }

    pub fn drain_nonces(&mut self) -> Vec<(usize, UartNonce)> {
        self.nonce_queue.drain(..).collect()
    }

    pub fn nonce_queue_len(&self) -> usize {
        self.nonce_queue.len()
    }

    /// Start the hrtimer-equivalent TX loop on a background OS thread.
    ///
    /// The returned handle owns the shared service. Callers enqueue work through
    /// `with_service`, and `stop` joins the worker.
    pub fn spawn_tx_worker(self) -> UartTransWorker {
        UartTransWorker::new(self)
    }

    fn drain_nonce_frames(chain: usize, buf: &mut Vec<u8>, out: &mut Vec<(usize, UartNonce)>) {
        while buf.len() >= bm1362::RESPONSE_BYTES {
            let Some(pos) = buf.windows(2).position(|w| w == [0xAA, 0x55]) else {
                let keep = buf.len().min(1);
                let discard = buf.len().saturating_sub(keep);
                buf.drain(..discard);
                break;
            };
            if pos > 0 {
                buf.drain(..pos);
            }
            if buf.len() < bm1362::RESPONSE_BYTES {
                break;
            }

            let frame: Vec<u8> = buf[..bm1362::RESPONSE_BYTES].to_vec();
            if let Some(nonce) = parse_bm1362_nonce_frame(&frame) {
                out.push((chain, nonce));
                buf.drain(..bm1362::RESPONSE_BYTES);
            } else {
                buf.drain(..1);
            }
        }

        if buf.len() > RX_BUFFER_LIMIT {
            let drain = buf.len() - RX_BUFFER_LIMIT;
            buf.drain(..drain);
        }
    }

    fn queue(&self, chain: usize) -> Result<&VecDeque<UartWork>> {
        self.queues.get(chain).ok_or_else(|| {
            AsicError::InvalidParameter(format!("invalid uart_trans chain {}", chain))
        })
    }

    fn queue_mut(&mut self, chain: usize) -> Result<&mut VecDeque<UartWork>> {
        self.queues.get_mut(chain).ok_or_else(|| {
            AsicError::InvalidParameter(format!("invalid uart_trans chain {}", chain))
        })
    }
}

/// Background TX worker for the userspace `uart_trans` timer.
pub struct UartTransWorker {
    service: Arc<Mutex<UartTransService>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<()>>>,
}

impl UartTransWorker {
    fn new(service: UartTransService) -> Self {
        Self {
            service: Arc::new(Mutex::new(service)),
            stop: Arc::new(AtomicBool::new(false)),
            join: None,
        }
    }

    pub fn start(mut self) -> Self {
        let service = Arc::clone(&self.service);
        let stop = Arc::clone(&self.stop);

        self.join = Some(thread::spawn(move || {
            let mut started = false;
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }

                let interval = {
                    let mut guard = service.lock().map_err(|_| {
                        AsicError::InvalidParameter("uart_trans worker mutex poisoned".into())
                    })?;
                    if !started {
                        guard.start_send_work_timer();
                        started = true;
                    }
                    guard.send_due_work_once()?;
                    guard.poll_nonces_into_queue_once()?;
                    guard.send_interval()
                };

                thread::sleep(interval);
            }
            Ok(())
        }));

        self
    }

    pub fn with_service<T>(&self, f: impl FnOnce(&mut UartTransService) -> Result<T>) -> Result<T> {
        let mut guard = self
            .service
            .lock()
            .map_err(|_| AsicError::InvalidParameter("uart_trans worker mutex poisoned".into()))?;
        f(&mut guard)
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| {
                AsicError::InvalidParameter("uart_trans worker thread panicked".into())
            })??;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_tty(path: &Path, baud: u32) -> Result<SerialChain> {
    let path = path.to_str().ok_or_else(|| {
        AsicError::InvalidParameter(format!("uart_trans path is not UTF-8: {}", path.display()))
    })?;
    let mut chain = SerialChain::open(path, baud)?;
    chain.set_nonblocking()?;
    Ok(chain)
}

#[cfg(not(unix))]
fn open_tty(path: &Path, _baud: u32) -> Result<SerialChain> {
    Err(AsicError::InvalidParameter(format!(
        "uart_trans tty open is only supported on Unix targets: {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_work() -> MiningWork {
        MiningWork {
            work_id: 0x42,
            fpga_midstate_cnt: 2,
            version: 0x2000_0004,
            nbits: 0x1703_5f0f,
            ntime: 0x6611_2233,
            merkle_tail: [0x10, 0x11, 0x12, 0x13],
            midstates: vec![[0xAB; 32]],
            merkle_root: [0x55; 32],
            prev_block_hash: [0x77; 32],
        }
    }

    #[test]
    fn default_chain_ttys_lock_bb_uart_map() {
        assert_eq!(CHAIN_COUNT, 4);
        assert_eq!(
            DEFAULT_CHAIN_TTYS,
            ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"]
        );
        assert!(
            !DEFAULT_CHAIN_TTYS.contains(&"/dev/ttyO3"),
            "ttyO3 is disabled in the Bitmain BB device tree"
        );
    }

    #[test]
    fn open_paths_requires_four_ttys_before_touching_paths() {
        let err =
            match UartTransService::open_paths_with_baud(["/definitely/not/opened"], BAUD_115200) {
                Ok(_) => panic!("wrong path count must fail before any tty open"),
                Err(err) => err,
            };

        assert!(err.to_string().contains("requires 4 tty paths"));
    }

    #[test]
    fn work_frame_is_86_bytes_with_valid_crc() {
        let work = fixture_work();
        let frame = UartWork::from_bm1362_work(&work, 0x18);

        assert_eq!(frame.as_frame_86().len(), UART_TRANS_FRAME_LEN);
        assert_eq!(frame.as_frame_86()[0], 0x21);
        assert_eq!(frame.as_frame_86()[1], 0x56);
        assert!(frame.crc_valid());

        let wire = frame.to_wire_frame();
        assert_eq!(&wire[..2], &[0x55, 0xAA]);
        assert_eq!(&wire[2..], frame.as_frame_86());
    }

    #[test]
    fn work_frame_from_command_frame_appends_crc() {
        let work = fixture_work();
        let wire = bm1362::build_serial_work_frame(&work, 0x18);
        let frame = UartWork::from_command_frame(&wire[2..86]).unwrap();

        assert_eq!(frame.asic_job_id(), 0x18);
        assert_eq!(frame.as_frame_86(), &wire[2..88]);
        assert!(frame.crc_valid());
    }

    #[test]
    fn work_frame_from_wire_frame_validates_crc() {
        let work = fixture_work();
        let mut wire = bm1362::build_serial_work_frame(&work, 0x30);
        let frame = UartWork::from_wire_frame(&wire).unwrap();
        assert_eq!(frame.asic_job_id(), 0x30);
        assert_eq!(frame.to_wire_frame(), wire);

        wire[87] ^= 0x01;
        assert!(UartWork::from_wire_frame(&wire).is_err());
    }

    #[test]
    fn queue_depth_is_bounded_and_drops_oldest() {
        let mut svc = UartTransService::without_ttys_for_test();
        svc.set_work_queue_count(2).unwrap();

        for id in [0x18, 0x30, 0x48] {
            svc.enqueue_work(0, UartWork::from_bm1362_work(&fixture_work(), id))
                .unwrap();
        }

        assert_eq!(svc.queue_len(0).unwrap(), 2);
        let first = svc.queues[0].front().unwrap();
        assert_eq!(first.asic_job_id(), 0x30);
    }

    #[test]
    fn can_pop_internal_86_byte_frame_for_uart_trans_compat() {
        let mut svc = UartTransService::without_ttys_for_test();
        let work = UartWork::from_bm1362_work(&fixture_work(), 0x18);
        let expected = *work.as_frame_86();
        svc.enqueue_work(0, work).unwrap();

        let popped = svc.pop_next_frame_86(0).unwrap().unwrap();
        assert_eq!(popped, expected);
        assert_eq!(svc.queue_len(0).unwrap(), 0);
    }

    #[test]
    fn chain_exist_bits_are_masked_to_four_chains() {
        let mut svc = UartTransService::without_ttys_for_test();
        svc.set_chain_exist_bits(0xFFFF);
        assert_eq!(svc.chain_exist_bits(), 0x0F);
    }

    #[test]
    fn parse_bm1362_nonce_accepts_wire_and_body_forms() {
        let body = [0x44, 0x33, 0x22, 0x11, 0x00, 0x36, 0x12, 0x34, 0x80];
        let parsed = parse_bm1362_nonce_frame(&body).unwrap();
        assert_eq!(parsed.nonce, 0x1122_3344);
        assert_eq!(parsed.asic_job_id, 0x18);
        assert_eq!(parsed.small_core, 0x06);
        assert_eq!(parsed.version_bits_raw, 0x1234);

        let mut wire = [0u8; 11];
        wire[0] = 0xAA;
        wire[1] = 0x55;
        wire[2..].copy_from_slice(&body);
        assert_eq!(parse_bm1362_nonce_frame(&wire), Some(parsed));
        assert_eq!(parsed.to_bm1362_body(), body);
    }

    #[test]
    fn parse_bm1362_nonce_rejects_non_job_response() {
        let body = [0x44, 0x33, 0x22, 0x11, 0x00, 0x36, 0x12, 0x34, 0x00];
        assert_eq!(parse_bm1362_nonce_frame(&body), None);
    }

    #[test]
    fn nonce_stream_parser_resyncs_after_noise() {
        let mut buf = vec![0x00, 0x13, 0xAA];
        buf.extend_from_slice(&[
            0xAA, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x36, 0x12, 0x34, 0x80,
        ]);
        let mut out = Vec::new();

        UartTransService::drain_nonce_frames(2, &mut buf, &mut out);

        assert!(buf.is_empty());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2);
        assert_eq!(out[0].1.nonce, 0x1122_3344);
    }

    #[test]
    fn parsed_nonces_can_be_queued_and_drained() {
        let mut svc = UartTransService::without_ttys_for_test();
        assert_eq!(svc.nonce_queue_len(), 0);

        svc.nonce_queue.push_back((
            1,
            UartNonce {
                nonce: 0x1122_3344,
                asic_job_id: 0x18,
                midstate_idx: 0,
                version_bits_raw: 0x1234,
                small_core: 6,
                flags: 0x80,
            },
        ));

        assert_eq!(svc.nonce_queue_len(), 1);
        assert_eq!(svc.pop_next_nonce().unwrap().0, 1);
        assert_eq!(svc.nonce_queue_len(), 0);
    }

    #[test]
    fn tx_worker_starts_and_stops_without_present_chains() {
        let worker = UartTransService::without_ttys_for_test()
            .spawn_tx_worker()
            .start();
        std::thread::sleep(Duration::from_millis(5));
        worker.stop().unwrap();
    }

    #[test]
    fn tx_worker_does_not_restart_timer_after_manual_stop() {
        let mut svc = UartTransService::without_ttys_for_test();
        svc.set_send_interval(Duration::from_millis(1)).unwrap();

        let worker = svc.spawn_tx_worker().start();
        std::thread::sleep(Duration::from_millis(5));
        worker
            .with_service(|svc| {
                assert!(svc.timer_running());
                svc.stop_send_work_timer();
                Ok(())
            })
            .unwrap();

        std::thread::sleep(Duration::from_millis(5));
        worker
            .with_service(|svc| {
                assert!(!svc.timer_running());
                Ok(())
            })
            .unwrap();
        worker.stop().unwrap();
    }
}
