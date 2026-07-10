// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — dcentaxe-bap
//
// BitAxe Accessory Protocol (BAP) server. Port of ESP-Miner `main/bap/*`
// (PRs #1178 / #1525 / #1547 / #1602). The mining ESP32 exposes a
// NMEA-style UART service on UART_NUM_2 (GPIO40 RX, GPIO39 TX) that a
// separate ESP32-S3 "Touch" accessory board queries for telemetry and
// issues configuration commands against.
//
// Frame shape:
//     $BAP,<CMD>,<PARAM>,<VALUE>*<XOR>\r\n
// where `<XOR>` is a two-digit hex XOR of every byte of the body
// (from the first comma after `$BAP` through the last character before `*`),
// matching the NMEA-0183 convention used upstream.
//
// The crate is organised so the protocol math is hardware-independent and
// runs on the host under `cargo test`; the ESP-IDF UART transport is gated
// behind the `esp-idf` feature so host tests stay fast.

pub mod handlers;
pub mod protocol;
pub mod subscription;

#[cfg(feature = "esp-idf")]
pub mod uart;

pub use protocol::{BapCommand, BapError, BapFrame, MAX_FRAME_LEN, MAX_PAYLOAD_LEN};
pub use subscription::{SubscribableParam, SubscriptionManager};

/// Trait that abstracts whichever byte transport carries the BAP frames.
/// The production implementation is a `UartDriver` bound to `UART_NUM_2`,
/// but any byte-oriented duplex transport works (loopback, mock, TCP, …).
pub trait BapTransport: Send {
    /// Read up to `buf.len()` bytes into `buf`. Returns the number read;
    /// 0 means no data is currently available (non-blocking semantics).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, BapError>;

    /// Write the full `bytes` payload. Returns `BapError::TxOverflow`
    /// if the transport cannot accept the whole frame.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), BapError>;
}

/// Snapshot that the host firmware gives the BAP server each tick. Mirrors
/// the REST `/api/system/info` shape so the stock `BAP-GT-TOUCH` accessory
/// firmware sees a familiar payload. Only the fields upstream BAP exposes
/// are included — add more as the accessory firmware learns to read them.
#[derive(Debug, Clone, Default)]
pub struct AppSnapshot {
    pub hashrate_ghs: f64,
    pub hashrate_1m_ghs: f64,
    pub temperature_c: f32,
    pub power_w: f32,
    pub voltage_mv: f32,
    pub current_ma: f32,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub frequency_mhz: f32,
    pub asic_voltage_mv: u16,
    pub fan_speed_pct: u8,
    pub auto_fan: bool,
    pub best_difficulty: f64,
    pub block_height: u64,
    pub wifi_connected: bool,
    pub wifi_ssid: String,
    pub wifi_rssi_dbm: i8,
    pub device_model: String,
    pub firmware_version: String,
}

/// Trait wired by the host firmware to honour `SET` commands and the
/// side-effectful `CMD` operations (restart_mining, identify, …).
///
/// # SCAFFOLD STATUS (BAP-3)
/// As of this revision there is **no `impl BapAppState` in the `dcentaxe`
/// binary** and `BapServer` is never spawned — the BAP service compiles into
/// the Touch board images as feature-gated, currently-unreachable code. Wiring
/// the host impl + spawn loop lives in the `dcentaxe` binary crate (out of this
/// crate's scope). Until that lands, treat BAP as experimental; do not advertise
/// live BAP/Touch control.
///
/// # SAFETY CONTRACT (BAP-2) — the impl MUST honour this
/// The BAP UART is an external, unauthenticated, owner-physical accessory link.
/// The protocol layer applies a coarse outer-envelope reject (see
/// [`protocol::bounds`]), but the AUTHORITATIVE per-board clamp is the host's
/// responsibility. Every implementation of [`set_frequency`](Self::set_frequency),
/// [`set_asic_voltage`](Self::set_asic_voltage) and [`set_fan_speed`](Self::set_fan_speed)
/// **MUST route the value through `config.qualify_operating_point()`** (or the
/// equivalent HAL clamp) before applying it to hardware — exactly as the REST /
/// MCP owner-control path does. Wi-Fi credential mutation
/// ([`set_wifi_ssid`](Self::set_wifi_ssid) / [`set_wifi_password`](Self::set_wifi_password))
/// SHOULD be gated behind explicit pairing/owner-control; do NOT expose it as a
/// passwordless write. An impl that skips these obligations re-opens the
/// over-volt / credential-rewrite surface this contract closes.
pub trait BapAppState: Send {
    fn snapshot(&self) -> AppSnapshot;
    fn set_frequency(&self, mhz: f32) -> Result<(), String>;
    fn set_asic_voltage(&self, mv: u16) -> Result<(), String>;
    fn set_fan_speed(&self, pct: u8) -> Result<(), String>;
    fn set_auto_fan(&self, enabled: bool) -> Result<(), String>;
    fn set_wifi_ssid(&self, ssid: &str) -> Result<(), String>;
    fn set_wifi_password(&self, password: &str) -> Result<(), String>;
    fn restart_mining(&self) -> Result<(), String>;
    fn identify(&self) -> Result<(), String>;
}

/// The main service loop. Consumes a transport + app-state and services
/// one frame-per-call. Callers spawn this in a thread with their own
/// cadence (ESP-Miner polls roughly every 100 ms).
pub struct BapServer<T: BapTransport, S: BapAppState> {
    transport: T,
    app: S,
    subscriptions: SubscriptionManager,
    rx_buf: Vec<u8>,
}

impl<T: BapTransport, S: BapAppState> BapServer<T, S> {
    pub fn new(transport: T, app: S) -> Self {
        Self {
            transport,
            app,
            subscriptions: SubscriptionManager::new(),
            rx_buf: Vec::with_capacity(1024),
        }
    }

    /// Drain any pending RX bytes, parse complete frames, and dispatch each
    /// to the handler. Returns the number of frames dispatched this call.
    pub fn poll_frames(&mut self) -> Result<usize, BapError> {
        let mut scratch = [0u8; 256];
        loop {
            let n = self.transport.read(&mut scratch)?;
            if n == 0 {
                break;
            }
            if self.rx_buf.len() + n > 4096 {
                // Runaway buffer — drop and resync on the next `$BAP`.
                self.rx_buf.clear();
            }
            self.rx_buf.extend_from_slice(&scratch[..n]);
        }

        let mut dispatched = 0usize;
        // BAP-1: drive the scan with `next_frame_scan` so a complete-but-corrupt
        // or over-long frame is DRAINED (Skip) rather than re-presented forever.
        // `Incomplete` consumes 0 bytes and breaks the loop to await more RX.
        loop {
            match protocol::next_frame_scan(&self.rx_buf) {
                protocol::FrameScan::Frame { frame, consumed } => {
                    self.rx_buf.drain(..consumed);
                    match handlers::dispatch(&self.app, &mut self.subscriptions, &frame) {
                        Ok(Some(reply)) => {
                            let bytes = reply.encode();
                            self.transport.write_all(&bytes)?;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::warn!("BAP: handler error: {:?}", e);
                            let err_frame = BapFrame::error(&frame, e.as_str());
                            let _ = self.transport.write_all(&err_frame.encode());
                        }
                    }
                    dispatched += 1;
                }
                protocol::FrameScan::Skip { consumed } => {
                    // Corrupt / over-long / junk prefix — drain past it and
                    // keep scanning. `consumed` is always > 0 when a start or
                    // junk exists, so this cannot spin.
                    if consumed == 0 {
                        break;
                    }
                    self.rx_buf.drain(..consumed);
                }
                protocol::FrameScan::Incomplete => break,
            }
        }
        Ok(dispatched)
    }

    /// Emit any subscription updates whose cadence has elapsed. The caller
    /// should drive this from the same loop that calls `poll_frames()`.
    pub fn tick_subscriptions(&mut self, now_ms: u64) -> Result<(), BapError> {
        let snap = self.app.snapshot();
        for frame in self.subscriptions.emit_due(now_ms, &snap) {
            self.transport.write_all(&frame.encode())?;
        }
        Ok(())
    }

    /// Invoke after each call where the accessory has been heard from — keeps
    /// subscriptions alive past the 5-minute idle window.
    pub fn refresh_keepalive(&mut self, now_ms: u64) {
        self.subscriptions.refresh_keepalive(now_ms);
    }
}

#[cfg(test)]
mod server_tests {
    use super::*;
    use protocol::BapCommand;
    use std::cell::RefCell;

    /// In-memory transport: RX is a queue of byte chunks the "accessory" sent;
    /// TX captures everything the server wrote back so the test can assert on
    /// the replies. Exercises the real `BapServer::poll_frames()` loop on the
    /// host (the BAP-1 load-bearing fix lives there, not just in the parser).
    struct MockTransport {
        rx: RefCell<Vec<u8>>,
        tx: RefCell<Vec<u8>>,
    }

    impl MockTransport {
        fn new(rx: Vec<u8>) -> Self {
            Self {
                rx: RefCell::new(rx),
                tx: RefCell::new(Vec::new()),
            }
        }
    }

    impl BapTransport for MockTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, BapError> {
            let mut rx = self.rx.borrow_mut();
            if rx.is_empty() {
                return Ok(0);
            }
            let n = buf.len().min(rx.len());
            buf[..n].copy_from_slice(&rx[..n]);
            rx.drain(..n);
            Ok(n)
        }
        fn write_all(&mut self, bytes: &[u8]) -> Result<(), BapError> {
            self.tx.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// Minimal app: REQ,hashrate renders a fixed value so we can recognise the
    /// reply on the TX side.
    struct StubApp;
    unsafe impl Send for StubApp {}
    impl BapAppState for StubApp {
        fn snapshot(&self) -> AppSnapshot {
            AppSnapshot {
                hashrate_ghs: 123.0,
                ..Default::default()
            }
        }
        fn set_frequency(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_asic_voltage(&self, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn set_fan_speed(&self, _: u8) -> Result<(), String> {
            Ok(())
        }
        fn set_auto_fan(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi_ssid(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi_password(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn restart_mining(&self) -> Result<(), String> {
            Ok(())
        }
        fn identify(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// BAP-1 end-to-end: a corrupt frame at the head of the RX buffer must NOT
    /// head-of-line-block a following good frame in the REAL server loop. Before
    /// the fix, `poll_frames` used `next_frame` (which returns None on a corrupt
    /// frame without draining) and wedged forever. The fix migrated the loop to
    /// `next_frame_scan` so the bad frame is Skip-drained.
    #[test]
    fn poll_frames_skips_corrupt_then_dispatches_good() {
        // [bad-checksum SET][good REQ,hashrate]
        let mut bad = BapFrame::new(BapCommand::Set, "fan_speed", "50").encode();
        let len = bad.len();
        bad[len - 3] = b'0';
        bad[len - 4] = b'0';
        let good = BapFrame::new(BapCommand::Req, "hashrate", "").encode();

        let mut stream = Vec::new();
        stream.extend_from_slice(&bad);
        stream.extend_from_slice(&good);

        let transport = MockTransport::new(stream);
        let mut server = BapServer::new(transport, StubApp);

        let dispatched = server.poll_frames().expect("poll");
        // The good frame must have been dispatched even though a corrupt frame
        // sat in front of it (no wedge).
        assert!(
            dispatched >= 1,
            "good frame must dispatch past the corrupt one, got {dispatched}"
        );
        // The TX side must carry the RES,hashrate reply (proving the good frame
        // reached the handler and produced a reply).
        let tx = server.transport.tx.borrow().clone();
        let tx_str = String::from_utf8_lossy(&tx);
        assert!(
            tx_str.contains("BAP,RES,hashrate,"),
            "expected RES,hashrate reply on TX, got {tx_str:?}"
        );
        // The rx_buf must be fully drained (no leftover wedged bytes).
        assert_eq!(server.rx_buf.len(), 0, "rx_buf must be drained, not wedged");
    }

    /// A lone corrupt frame must be fully drained by `poll_frames` (Skip path),
    /// leaving an empty rx_buf — proving the buffer cannot accumulate a wedged
    /// bad frame across polls.
    #[test]
    fn poll_frames_drains_lone_corrupt_frame() {
        let mut bad = BapFrame::new(BapCommand::Set, "fan_speed", "50").encode();
        let len = bad.len();
        bad[len - 3] = b'0';
        bad[len - 4] = b'0';

        let transport = MockTransport::new(bad);
        let mut server = BapServer::new(transport, StubApp);
        let dispatched = server.poll_frames().expect("poll");
        assert_eq!(dispatched, 0, "a corrupt frame dispatches nothing");
        assert_eq!(
            server.rx_buf.len(),
            0,
            "corrupt frame must be drained, not left to wedge"
        );
    }

    /// A partial frame (start present, no terminator yet) must be PRESERVED in
    /// rx_buf across a poll so a later RX chunk can complete it — `Incomplete`
    /// must not drain it.
    #[test]
    fn poll_frames_preserves_incomplete_frame() {
        let partial = b"$BAP,REQ,hashr".to_vec();
        let transport = MockTransport::new(partial.clone());
        let mut server = BapServer::new(transport, StubApp);
        let dispatched = server.poll_frames().expect("poll");
        assert_eq!(dispatched, 0);
        assert_eq!(
            server.rx_buf.len(),
            partial.len(),
            "incomplete frame must be retained for the next RX chunk"
        );
    }
}
