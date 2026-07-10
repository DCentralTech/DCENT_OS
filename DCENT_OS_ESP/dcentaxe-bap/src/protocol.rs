// SPDX-License-Identifier: GPL-3.0-or-later
// BAP framing / parsing. Pure Rust, host-testable.

use std::fmt;

/// Absolute maximum frame length on the wire (body + `$BAP,` + `*XX\r\n`).
/// Matches ESP-Miner's `BAP_FRAME_MAX` constant.
pub const MAX_FRAME_LEN: usize = 256;
/// Maximum payload length (`CMD,PARAM,VALUE` without the checksum/terminator).
pub const MAX_PAYLOAD_LEN: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BapCommand {
    Req,
    Res,
    Sub,
    Unsub,
    Set,
    Ack,
    Err,
    Cmd,
    Sta,
    Log,
}

impl BapCommand {
    pub fn as_str(&self) -> &'static str {
        match self {
            BapCommand::Req => "REQ",
            BapCommand::Res => "RES",
            BapCommand::Sub => "SUB",
            BapCommand::Unsub => "UNSUB",
            BapCommand::Set => "SET",
            BapCommand::Ack => "ACK",
            BapCommand::Err => "ERR",
            BapCommand::Cmd => "CMD",
            BapCommand::Sta => "STA",
            BapCommand::Log => "LOG",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "REQ" => BapCommand::Req,
            "RES" => BapCommand::Res,
            "SUB" => BapCommand::Sub,
            "UNSUB" => BapCommand::Unsub,
            "SET" => BapCommand::Set,
            "ACK" => BapCommand::Ack,
            "ERR" => BapCommand::Err,
            "CMD" => BapCommand::Cmd,
            "STA" => BapCommand::Sta,
            "LOG" => BapCommand::Log,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BapError {
    TransportRead(String),
    TransportWrite(String),
    TxOverflow,
    Framing,
    BadChecksum,
    UnknownCommand,
    UnknownParam,
    InvalidValue,
    HandlerFailed(String),
}

impl BapError {
    pub fn as_str(&self) -> &'static str {
        match self {
            BapError::TransportRead(_) => "TRANSPORT_READ",
            BapError::TransportWrite(_) => "TRANSPORT_WRITE",
            BapError::TxOverflow => "TX_OVERFLOW",
            BapError::Framing => "FRAMING",
            BapError::BadChecksum => "CHECKSUM",
            BapError::UnknownCommand => "UNKNOWN_CMD",
            BapError::UnknownParam => "UNKNOWN_PARAM",
            BapError::InvalidValue => "INVALID_VALUE",
            BapError::HandlerFailed(_) => "HANDLER_FAILED",
        }
    }
}

impl fmt::Display for BapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// One parsed BAP frame. Values are held as owned strings for ergonomics —
/// the parser allocates, but the host code only calls it a few Hz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BapFrame {
    pub cmd: BapCommand,
    pub param: String,
    pub value: String,
}

impl BapFrame {
    pub fn new(cmd: BapCommand, param: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            cmd,
            param: param.into(),
            value: value.into(),
        }
    }

    pub fn res(param: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(BapCommand::Res, param, value)
    }

    pub fn ack(param: impl Into<String>) -> Self {
        Self::new(BapCommand::Ack, param, "")
    }

    pub fn error(source: &BapFrame, code: &str) -> Self {
        Self::new(BapCommand::Err, source.param.clone(), code.to_string())
    }

    /// Encode this frame to an on-the-wire byte sequence with the NMEA XOR
    /// checksum + `\r\n` terminator.
    pub fn encode(&self) -> Vec<u8> {
        let body = format!("BAP,{},{},{}", self.cmd.as_str(), self.param, self.value);
        let checksum = nmea_xor(body.as_bytes());
        let mut out = Vec::with_capacity(body.len() + 8);
        out.push(b'$');
        out.extend_from_slice(body.as_bytes());
        out.push(b'*');
        out.extend_from_slice(format!("{:02X}", checksum).as_bytes());
        out.push(b'\r');
        out.push(b'\n');
        out
    }
}

/// XOR of every byte of `bytes`. Matches NMEA-0183 / ESP-Miner's checksum.
pub fn nmea_xor(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc ^ b)
}

/// Protocol-layer safety envelope for mutating `SET` commands (BAP-2).
///
/// The BAP UART is an **owner-physical accessory header**, not a network port,
/// but it is still an EXTERNAL serial link with no authentication at the wire.
/// The authoritative, per-board clamp lives in the host firmware
/// (`config.qualify_operating_point()`), which every `BapAppState` impl MUST
/// route through. These constants are a defence-in-depth OUTER envelope: an
/// absolute hard reject of values no supported BitAxe board could ever accept
/// (e.g. an over-volt that would damage the ASIC, or a fan request above
/// 100%), so a malformed/hostile frame is rejected at the protocol layer
/// before it ever reaches the host handler. They are intentionally WIDER than
/// any single board's qualified range — narrowing happens in
/// `qualify_operating_point`. Do NOT tighten these to a single board's values;
/// that would silently reject legitimate frames for other boards.
pub mod bounds {
    /// Lowest core voltage any board boots at, minus headroom. Below this the
    /// chip won't run; a value this low is a malformed/garbage request.
    pub const MIN_ASIC_VOLTAGE_MV: u16 = 900;
    /// Absolute over-volt hard ceiling. The widest board envelope tops out at
    /// 1500 mV (BM1397 voltage table); anything above 1600 mV is an over-volt
    /// that the protocol layer refuses outright, independent of the host clamp.
    pub const MAX_ASIC_VOLTAGE_MV: u16 = 1600;
    /// Lowest PLL frequency any supported chip is driven at, minus headroom.
    pub const MIN_FREQUENCY_MHZ: f32 = 50.0;
    /// Absolute frequency hard ceiling across every supported board / hex
    /// daisy-chain. Per-board maxima (≤625 MHz today) clamp tighter downstream.
    pub const MAX_FREQUENCY_MHZ: f32 = 800.0;
    /// Fan duty is a percentage; 0..=100 is the only valid range.
    pub const MAX_FAN_PCT: u8 = 100;

    /// Reject an out-of-envelope ASIC voltage (mV). Returns the value if in
    /// range. The host still applies the tighter per-board clamp.
    pub fn check_asic_voltage_mv(mv: u16) -> Option<u16> {
        (MIN_ASIC_VOLTAGE_MV..=MAX_ASIC_VOLTAGE_MV)
            .contains(&mv)
            .then_some(mv)
    }

    /// Reject an out-of-envelope frequency (MHz). NaN / non-finite is rejected.
    pub fn check_frequency_mhz(mhz: f32) -> Option<f32> {
        (mhz.is_finite() && (MIN_FREQUENCY_MHZ..=MAX_FREQUENCY_MHZ).contains(&mhz)).then_some(mhz)
    }

    /// Reject a fan duty above 100%.
    pub fn check_fan_pct(pct: u8) -> Option<u8> {
        (pct <= MAX_FAN_PCT).then_some(pct)
    }
}

/// Outcome of one `next_frame` scan.
///
/// The caller (`poll_frames`) MUST drain `consumed()` bytes from the front of
/// the buffer after every non-`Incomplete` result so a malformed frame can
/// never head-of-line-block the channel (BAP-1). Returning a byte count even
/// on parse failure is what lets the caller resync past a corrupt frame
/// instead of re-finding the same `$BAP,` forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameScan {
    /// A complete, valid frame was parsed; consume `consumed` bytes.
    Frame { frame: BapFrame, consumed: usize },
    /// A complete-but-corrupt (or over-long) frame was found and skipped;
    /// consume `consumed` bytes and try again. No frame is produced.
    Skip { consumed: usize },
    /// No complete frame is present yet; wait for more bytes. The caller
    /// should NOT drain (a partial frame may still be completed by later RX).
    Incomplete,
}

impl FrameScan {
    /// Bytes the caller should drain from the front of the buffer.
    /// `Incomplete` consumes nothing (wait for more data).
    pub fn consumed(&self) -> usize {
        match self {
            FrameScan::Frame { consumed, .. } | FrameScan::Skip { consumed } => *consumed,
            FrameScan::Incomplete => 0,
        }
    }
}

/// Scan `buf` for the next frame. On a complete-and-valid frame returns
/// [`FrameScan::Frame`]; on a complete-but-corrupt or over-long frame returns
/// [`FrameScan::Skip`] (so the caller resyncs past it); when no complete frame
/// is present yet returns [`FrameScan::Incomplete`].
///
/// Resync + length-bound rules (BAP-1 / BAP-4):
/// * Any garbage before the next `$BAP,` start is skipped.
/// * A frame whose start-to-terminator span exceeds [`MAX_FRAME_LEN`] is
///   rejected and skipped past its (truncated) start so an unterminated
///   `$BAP,` cannot absorb a later frame's terminator or force an unbounded
///   scan.
/// * A complete frame that fails to parse (bad checksum / unknown command /
///   over-long payload) is skipped to just after its `$` so the next `$BAP,`
///   can be found on the following call.
pub fn next_frame_scan(buf: &[u8]) -> FrameScan {
    // Find the next `$BAP,` boundary; everything before it is junk to drop.
    let start = match find_start(buf) {
        Some(s) => s,
        None => {
            // No start in view. Drop everything except a possible partial
            // `$BAP,` prefix at the very tail so a start split across two RX
            // chunks is not destroyed. If there is nothing to drop (empty, or
            // the whole buffer is already a partial start prefix), report
            // `Incomplete` so `Skip` always advances by > 0 bytes.
            let drop = drop_to_partial_start(buf);
            return if drop == 0 {
                FrameScan::Incomplete
            } else {
                FrameScan::Skip { consumed: drop }
            };
        }
    };
    let after_start = &buf[start..];
    // Find the matching `\r\n` terminator after start, but only scan within
    // MAX_FRAME_LEN bytes so an unterminated `$BAP,` cannot swallow later
    // frames or trigger an unbounded over-read (BAP-4).
    let scan_window = after_start.len().min(MAX_FRAME_LEN);
    match find_end(&after_start[..scan_window]) {
        Some(rel_end) => {
            let end = rel_end + start;
            let raw = &buf[start..end + 2]; // include the \r\n
            match parse_frame(raw) {
                Ok(frame) => FrameScan::Frame {
                    frame,
                    consumed: end + 2,
                },
                // Corrupt frame: skip past its leading `$` so the next call
                // resyncs on the following `$BAP,`. Never re-presents the same
                // bad frame (BAP-1).
                Err(_) => FrameScan::Skip {
                    consumed: start + 1,
                },
            }
        }
        None => {
            // No terminator within the window.
            if after_start.len() >= MAX_FRAME_LEN {
                // Over-long unterminated frame — skip past this `$` and resync
                // on the next `$BAP,` (BAP-4). Without this an unterminated
                // start absorbs a later frame's `\r\n`.
                FrameScan::Skip {
                    consumed: start + 1,
                }
            } else {
                // Possibly a partial frame still arriving. Drop any junk before
                // the start but keep the in-progress frame intact.
                if start > 0 {
                    FrameScan::Skip { consumed: start }
                } else {
                    FrameScan::Incomplete
                }
            }
        }
    }
}

/// Backwards-compatible wrapper: returns the parsed frame + consumed count for
/// a complete-and-valid frame, or `None` for anything else.
///
/// NOTE: callers that need correct resync must use [`next_frame_scan`] — this
/// helper hides `Skip`, so on its own it can wedge on a corrupt frame. It is
/// retained only for tests / simple single-frame call sites.
pub fn next_frame(buf: &[u8]) -> Option<(BapFrame, usize)> {
    match next_frame_scan(buf) {
        FrameScan::Frame { frame, consumed } => Some((frame, consumed)),
        FrameScan::Skip { .. } | FrameScan::Incomplete => None,
    }
}

/// When no `$BAP,` start byte sequence is present, return how many bytes are
/// safe to drop while preserving a possible partial `$BAP,` straddling the
/// buffer tail (so a start split across two reads survives).
fn drop_to_partial_start(buf: &[u8]) -> usize {
    const START: &[u8] = b"$BAP,";
    // Keep up to START.len()-1 trailing bytes only if they are a prefix of START.
    for keep in (1..START.len()).rev() {
        if buf.len() >= keep && buf[buf.len() - keep..] == START[..keep] {
            return buf.len() - keep;
        }
    }
    buf.len()
}

fn find_start(buf: &[u8]) -> Option<usize> {
    for (i, window) in buf.windows(5).enumerate() {
        if window == b"$BAP," {
            return Some(i);
        }
    }
    None
}

fn find_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            return Some(i);
        }
    }
    None
}

fn parse_frame(raw: &[u8]) -> Result<BapFrame, BapError> {
    // Enforce the declared wire bounds (BAP-4). A frame longer than
    // MAX_FRAME_LEN is rejected rather than parsed/allocated.
    if raw.len() < 10 || raw.len() > MAX_FRAME_LEN {
        return Err(BapError::Framing);
    }
    if !raw.starts_with(b"$BAP,") || !raw.ends_with(b"\r\n") {
        return Err(BapError::Framing);
    }
    let without_terminator = &raw[..raw.len() - 2];
    // Split on `*` to isolate body vs checksum.
    let star_pos = without_terminator
        .iter()
        .rposition(|&b| b == b'*')
        .ok_or(BapError::Framing)?;
    if star_pos + 3 != without_terminator.len() {
        return Err(BapError::Framing);
    }
    let body = &without_terminator[1..star_pos]; // strip leading `$`
                                                 // Enforce the declared payload bound (BAP-4): the body (`BAP,CMD,PARAM,VALUE`)
                                                 // must not exceed MAX_PAYLOAD_LEN. Rejected as a framing error so the caller
                                                 // resyncs past it.
    if body.len() > MAX_PAYLOAD_LEN {
        return Err(BapError::Framing);
    }
    let expected_hex = &without_terminator[star_pos + 1..];
    let expected = u8::from_str_radix(
        std::str::from_utf8(expected_hex).map_err(|_| BapError::Framing)?,
        16,
    )
    .map_err(|_| BapError::Framing)?;
    let computed = nmea_xor(body);
    if computed != expected {
        return Err(BapError::BadChecksum);
    }

    // body = "BAP,CMD,PARAM,VALUE"
    let body_str = std::str::from_utf8(body).map_err(|_| BapError::Framing)?;
    let mut it = body_str.splitn(4, ',');
    let tag = it.next().ok_or(BapError::Framing)?;
    if tag != "BAP" {
        return Err(BapError::Framing);
    }
    let cmd_str = it.next().ok_or(BapError::Framing)?;
    let param = it.next().unwrap_or("").to_string();
    let value = it.next().unwrap_or("").to_string();
    let cmd = BapCommand::parse(cmd_str).ok_or(BapError::UnknownCommand)?;
    Ok(BapFrame { cmd, param, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_expect(cmd: BapCommand, param: &str, value: &str) -> Vec<u8> {
        BapFrame::new(cmd, param, value).encode()
    }

    #[test]
    fn round_trip_simple_req() {
        let bytes = encode_expect(BapCommand::Req, "hashrate", "");
        let (frame, consumed) = next_frame(&bytes).expect("parse");
        assert_eq!(consumed, bytes.len());
        assert_eq!(frame.cmd, BapCommand::Req);
        assert_eq!(frame.param, "hashrate");
        assert_eq!(frame.value, "");
    }

    #[test]
    fn checksum_known_vector() {
        // $BAP,REQ,hashrate,*XX — compute XOR by hand:
        let body = b"BAP,REQ,hashrate,";
        let xor = nmea_xor(body);
        assert!(xor > 0);
        let expected = BapFrame::new(BapCommand::Req, "hashrate", "");
        let bytes = expected.encode();
        // Verify that the pattern ends in "*XX\r\n"
        assert_eq!(&bytes[bytes.len() - 2..], b"\r\n");
        assert_eq!(bytes[bytes.len() - 5], b'*');
    }

    // ---- BAP-2: protocol-layer safety envelope ----

    #[test]
    fn bounds_reject_overvolt_and_accept_normal() {
        use bounds::*;
        // A legitimate core voltage passes (host still clamps per board).
        assert_eq!(check_asic_voltage_mv(1200), Some(1200));
        assert_eq!(
            check_asic_voltage_mv(MAX_ASIC_VOLTAGE_MV),
            Some(MAX_ASIC_VOLTAGE_MV)
        );
        assert_eq!(
            check_asic_voltage_mv(MIN_ASIC_VOLTAGE_MV),
            Some(MIN_ASIC_VOLTAGE_MV)
        );
        // An over-volt and an absurdly low value are rejected outright.
        assert_eq!(check_asic_voltage_mv(MAX_ASIC_VOLTAGE_MV + 1), None);
        assert_eq!(check_asic_voltage_mv(2000), None);
        assert_eq!(check_asic_voltage_mv(0), None);
        assert_eq!(check_asic_voltage_mv(500), None);
    }

    #[test]
    fn bounds_reject_bad_frequency() {
        use bounds::*;
        assert_eq!(check_frequency_mhz(500.0), Some(500.0));
        assert_eq!(
            check_frequency_mhz(MAX_FREQUENCY_MHZ),
            Some(MAX_FREQUENCY_MHZ)
        );
        assert_eq!(check_frequency_mhz(MAX_FREQUENCY_MHZ + 1.0), None);
        assert_eq!(check_frequency_mhz(10.0), None);
        assert_eq!(check_frequency_mhz(f32::NAN), None);
        assert_eq!(check_frequency_mhz(f32::INFINITY), None);
        assert_eq!(check_frequency_mhz(-100.0), None);
    }

    #[test]
    fn bounds_reject_fan_over_100() {
        use bounds::*;
        assert_eq!(check_fan_pct(0), Some(0));
        assert_eq!(check_fan_pct(100), Some(100));
        assert_eq!(check_fan_pct(101), None);
        assert_eq!(check_fan_pct(255), None);
    }

    #[test]
    fn bad_checksum_rejected() {
        let mut bytes = BapFrame::new(BapCommand::Req, "hashrate", "").encode();
        // Tamper with the checksum hex.
        let len = bytes.len();
        bytes[len - 3] = b'0';
        bytes[len - 4] = b'0';
        assert!(next_frame(&bytes).is_none());
    }

    #[test]
    fn junk_prefix_then_valid_frame() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"garbage noise ");
        stream.extend_from_slice(&BapFrame::new(BapCommand::Set, "fan_speed", "50").encode());
        let (frame, _) = next_frame(&stream).expect("parse past junk");
        assert_eq!(frame.cmd, BapCommand::Set);
        assert_eq!(frame.param, "fan_speed");
        assert_eq!(frame.value, "50");
    }

    #[test]
    fn req_then_set_back_to_back() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&BapFrame::new(BapCommand::Req, "hashrate", "").encode());
        stream.extend_from_slice(&BapFrame::new(BapCommand::Sub, "temperature", "").encode());

        let (f1, c1) = next_frame(&stream).unwrap();
        assert_eq!(f1.cmd, BapCommand::Req);
        let rest = &stream[c1..];
        let (f2, _) = next_frame(rest).unwrap();
        assert_eq!(f2.cmd, BapCommand::Sub);
    }

    // ---- BAP-1: malformed frame must not head-of-line-block the channel ----

    #[test]
    fn bad_checksum_frame_skips_and_next_frame_dispatches() {
        // [bad-checksum frame][good frame]. The scan must Skip the bad frame
        // (consuming bytes) so the good frame can still be parsed.
        let mut bad = BapFrame::new(BapCommand::Set, "fan_speed", "50").encode();
        let len = bad.len();
        bad[len - 3] = b'0';
        bad[len - 4] = b'0';
        let good = BapFrame::new(BapCommand::Req, "hashrate", "").encode();

        let mut stream = Vec::new();
        stream.extend_from_slice(&bad);
        stream.extend_from_slice(&good);

        // First scan: the bad frame is skipped (NOT wedged).
        let scan1 = next_frame_scan(&stream);
        let consumed1 = match scan1 {
            FrameScan::Skip { consumed } => consumed,
            other => panic!("expected Skip on bad checksum, got {:?}", other),
        };
        assert!(
            consumed1 > 0,
            "Skip must consume > 0 bytes to make progress"
        );
        stream.drain(..consumed1);

        // Keep draining Skips until the good frame surfaces — proves no wedge.
        let mut guard = 0;
        let frame = loop {
            guard += 1;
            assert!(guard < 1000, "resync loop did not terminate");
            match next_frame_scan(&stream) {
                FrameScan::Frame { frame, consumed } => {
                    stream.drain(..consumed);
                    break frame;
                }
                FrameScan::Skip { consumed } => {
                    assert!(consumed > 0);
                    stream.drain(..consumed);
                }
                FrameScan::Incomplete => panic!("good frame was lost"),
            }
        };
        assert_eq!(frame.cmd, BapCommand::Req);
        assert_eq!(frame.param, "hashrate");
    }

    #[test]
    fn skip_consumed_makes_progress_on_repeated_bad_frame() {
        // A lone bad frame must report Skip with consumed>0 so the caller can
        // drain it; a second scan after draining must not re-find the same bad
        // frame (the BAP-1 wedge).
        let mut bad = BapFrame::new(BapCommand::Set, "fan_speed", "50").encode();
        let len = bad.len();
        bad[len - 3] = b'0';
        bad[len - 4] = b'0';
        let mut buf = bad.clone();
        match next_frame_scan(&buf) {
            FrameScan::Skip { consumed } => {
                assert!(consumed > 0);
                buf.drain(..consumed);
            }
            other => panic!("expected Skip, got {:?}", other),
        }
        // After draining the skip, eventually the buffer no longer yields the
        // same bad frame as a Frame (no wedge).
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 1000);
            match next_frame_scan(&buf) {
                FrameScan::Frame { .. } => panic!("corrupt frame must never parse as Frame"),
                FrameScan::Skip { consumed } => {
                    assert!(consumed > 0, "Skip must advance");
                    buf.drain(..consumed);
                }
                FrameScan::Incomplete => break,
            }
        }
    }

    // ---- BAP-4: length bounds (oversize frame rejected, no over-read) ----

    #[test]
    fn oversize_terminated_frame_is_rejected() {
        // A complete frame whose body pushes total length over MAX_FRAME_LEN
        // must be rejected (Skip), not parsed.
        let big_value = "A".repeat(MAX_FRAME_LEN); // guarantees > MAX_FRAME_LEN total
        let frame = BapFrame::new(BapCommand::Set, "ssid", big_value);
        let bytes = frame.encode();
        assert!(bytes.len() > MAX_FRAME_LEN);
        match next_frame_scan(&bytes) {
            FrameScan::Skip { consumed } => assert!(consumed > 0),
            other => panic!("oversize frame must be Skipped, got {:?}", other),
        }
        // And the back-compat wrapper must refuse it.
        assert!(next_frame(&bytes).is_none());
    }

    #[test]
    fn unterminated_overlong_start_does_not_absorb_later_frame() {
        // An unterminated `$BAP,` longer than MAX_FRAME_LEN must be skipped so
        // it cannot swallow a later valid frame's terminator.
        let mut stream = Vec::new();
        stream.extend_from_slice(b"$BAP,");
        stream.extend_from_slice(&vec![b'X'; MAX_FRAME_LEN + 16]); // no \r\n
        let good = BapFrame::new(BapCommand::Req, "temperature", "").encode();
        stream.extend_from_slice(&good);

        // Drive the scan to resync; the later good frame must eventually parse.
        let mut guard = 0;
        let frame = loop {
            guard += 1;
            assert!(guard < 5000, "did not resync past overlong start");
            match next_frame_scan(&stream) {
                FrameScan::Frame { frame, consumed } => {
                    stream.drain(..consumed);
                    break frame;
                }
                FrameScan::Skip { consumed } => {
                    assert!(consumed > 0);
                    stream.drain(..consumed);
                }
                FrameScan::Incomplete => panic!("good frame lost behind overlong start"),
            }
        };
        assert_eq!(frame.cmd, BapCommand::Req);
        assert_eq!(frame.param, "temperature");
    }

    #[test]
    fn partial_start_at_tail_is_preserved() {
        // A start sequence split across two reads must not be destroyed.
        let partial = b"noise $BA"; // tail "$BA" is a prefix of "$BAP,"
        match next_frame_scan(partial) {
            FrameScan::Skip { consumed } => {
                // Must keep "$BA" (3 bytes) at the tail.
                assert_eq!(consumed, partial.len() - 3);
            }
            FrameScan::Incomplete => { /* also acceptable: nothing dropped */ }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn incomplete_partial_frame_waits_for_more() {
        // A `$BAP,` start with a short body and no terminator yet must be
        // Incomplete (consume 0) so later RX can complete it.
        let partial = b"$BAP,REQ,hashr";
        assert_eq!(next_frame_scan(partial), FrameScan::Incomplete);
        assert_eq!(next_frame_scan(partial).consumed(), 0);
    }

    #[test]
    fn all_commands_round_trip() {
        for cmd in [
            BapCommand::Req,
            BapCommand::Res,
            BapCommand::Sub,
            BapCommand::Unsub,
            BapCommand::Set,
            BapCommand::Ack,
            BapCommand::Err,
            BapCommand::Cmd,
            BapCommand::Sta,
            BapCommand::Log,
        ] {
            let bytes = BapFrame::new(cmd, "x", "y").encode();
            let (frame, _) = next_frame(&bytes).unwrap();
            assert_eq!(frame.cmd, cmd);
        }
    }

    // ---- BAP-1 / BAP-4: arbitrary/hostile UART bytes must never panic or wedge ----

    #[test]
    fn fuzz_scan_never_panics_or_wedges_on_arbitrary_bytes() {
        // The BAP UART is an external, unauthenticated serial link; the parser
        // must survive arbitrary/garbage bytes without panicking (no OOB slice,
        // no overflow) AND the drain loop must always terminate — every `Skip`
        // advances by `0 < consumed <= buf.len()` and `Incomplete` stops the
        // loop. This converts the by-inspection BAP-1 (no-wedge) + BAP-4
        // (length-bound) guarantees into an executable regression pin against
        // fuzz-shaped input across all parser branches.
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            // xorshift32 — deterministic, no external rng dependency.
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        // Alphabet biased toward bytes that exercise the framing grammar
        // ($ B A P , * CR LF + hex) plus uniform noise on the odd draws.
        let alphabet = b"$BAP,*\r\n0123456789ABCDEFxyz ";
        for _ in 0..4000 {
            let len = (next() % (MAX_FRAME_LEN as u32 * 2)) as usize;
            let mut buf: Vec<u8> = Vec::with_capacity(len);
            for _ in 0..len {
                let r = next();
                if r & 1 == 0 {
                    buf.push(alphabet[(r >> 1) as usize % alphabet.len()]);
                } else {
                    buf.push((r >> 3) as u8);
                }
            }
            let mut guard = 0;
            loop {
                guard += 1;
                assert!(guard < 100_000, "scan/drain loop failed to terminate");
                match next_frame_scan(&buf) {
                    FrameScan::Frame { consumed, .. } => {
                        assert!(
                            consumed > 0 && consumed <= buf.len(),
                            "Frame consumed {consumed} out of bounds for len {}",
                            buf.len()
                        );
                        buf.drain(..consumed);
                    }
                    FrameScan::Skip { consumed } => {
                        assert!(
                            consumed > 0 && consumed <= buf.len(),
                            "Skip must advance within bounds (consumed {consumed}, len {})",
                            buf.len()
                        );
                        buf.drain(..consumed);
                    }
                    FrameScan::Incomplete => break,
                }
            }
        }
    }
}
