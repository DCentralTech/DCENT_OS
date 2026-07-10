//! SV2 binary framing layer.
//!
//! Each SV2 message is framed as:
//!   - extension_type: u16 (LE)  — identifies the protocol extension
//!   - msg_type: u8              — message type within the extension
//!   - msg_length: u24 (LE)      — payload length (3 bytes, max 16MB)
//!   - payload: [u8; msg_length] — serialized message content
//!
//! Total header: 6 bytes. This is MUCH more efficient than JSON-RPC V1.

/// SV2 frame header size in bytes
pub const FRAME_HEADER_SIZE: usize = 6;

/// Maximum SV2 payload size carried by the 24-bit frame length field.
///
/// Mining messages are small, but Template Distribution and Job Declaration can
/// legally carry full transaction data in `B0_16M` fields. Keep the protocol
/// layer honest and let higher layers enforce operational memory policy.
pub const MAX_PAYLOAD_SIZE: u32 = (1 << 24) - 1;

/// A parsed SV2 frame header
#[derive(Debug, Clone)]
pub struct Sv2FrameHeader {
    pub extension_type: u16,
    pub msg_type: u8,
    pub payload_len: u32,
}

impl Sv2FrameHeader {
    /// Parse a frame header from a 6-byte buffer
    pub fn from_bytes(buf: &[u8]) -> Result<Self, FrameError> {
        if buf.len() < FRAME_HEADER_SIZE {
            return Err(FrameError::BufferTooSmall);
        }
        let extension_type = u16::from_le_bytes([buf[0], buf[1]]);
        let msg_type = buf[2];
        // 3-byte little-endian length
        let payload_len = u32::from_le_bytes([buf[3], buf[4], buf[5], 0]);

        if payload_len > MAX_PAYLOAD_SIZE {
            return Err(FrameError::PayloadTooLarge(payload_len));
        }

        Ok(Self {
            extension_type,
            msg_type,
            payload_len,
        })
    }

    /// Serialize this header into a 6-byte buffer.
    ///
    /// Panics if `payload_len > MAX_PAYLOAD_SIZE`. Silent truncation here
    /// would mis-encode the wire-format length (the receiver would parse
    /// fewer bytes than were sent and de-sync the entire stream). All
    /// production callers go through message-builder code that produces
    /// well-bounded payloads, so this panic is unreachable on the happy
    /// path. Use `try_to_bytes` if the caller is constructing a header
    /// from untrusted input.
    #[allow(clippy::expect_used)]
    pub fn to_bytes(&self) -> [u8; FRAME_HEADER_SIZE] {
        self.try_to_bytes()
            .expect("Sv2FrameHeader::to_bytes called with payload_len > MAX_PAYLOAD_SIZE")
    }

    /// Serialize this header without panicking on oversize `payload_len`.
    pub fn try_to_bytes(&self) -> Result<[u8; FRAME_HEADER_SIZE], FrameError> {
        if self.payload_len > MAX_PAYLOAD_SIZE {
            return Err(FrameError::PayloadTooLarge(self.payload_len));
        }
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&self.extension_type.to_le_bytes());
        buf[2] = self.msg_type;
        let len_bytes = self.payload_len.to_le_bytes();
        buf[3] = len_bytes[0];
        buf[4] = len_bytes[1];
        buf[5] = len_bytes[2];
        Ok(buf)
    }
}

/// A complete SV2 frame (header + payload)
#[derive(Debug)]
pub struct Sv2Frame {
    pub header: Sv2FrameHeader,
    pub payload: Vec<u8>,
}

impl Sv2Frame {
    /// Create a new frame with the given message type and payload.
    ///
    /// Panics if `payload.len() > MAX_PAYLOAD_SIZE` (16 MiB - 1). The wire
    /// format reserves only 24 bits for the length field, so silently
    /// allowing a larger payload would mis-encode the length and de-sync
    /// the stream at the receiver. All production callers go through
    /// message-builder code that produces well-bounded payloads (mining
    /// protocol messages are << 1 KB, JD/TDP messages are bounded by
    /// `MAX_PAYLOAD_SIZE` minus their per-message constants), so this
    /// panic is unreachable on the happy path. Use `try_new` for callers
    /// that may pass untrusted payload sizes.
    #[allow(clippy::expect_used)]
    pub fn new(extension_type: u16, msg_type: u8, payload: Vec<u8>) -> Self {
        Self::try_new(extension_type, msg_type, payload)
            .expect("Sv2Frame::new called with payload > MAX_PAYLOAD_SIZE")
    }

    /// Create a new frame, returning `PayloadTooLarge` if the payload
    /// exceeds the wire-format 24-bit length field.
    pub fn try_new(
        extension_type: u16,
        msg_type: u8,
        payload: Vec<u8>,
    ) -> Result<Self, FrameError> {
        let payload_len_u64 = payload.len() as u64;
        if payload_len_u64 > MAX_PAYLOAD_SIZE as u64 {
            return Err(FrameError::PayloadTooLarge(
                // Saturate the reported value so the error message is
                // bounded — usize on 64-bit platforms could be > u32::MAX.
                payload_len_u64.min(u32::MAX as u64) as u32,
            ));
        }
        Ok(Self {
            header: Sv2FrameHeader {
                extension_type,
                msg_type,
                payload_len: payload.len() as u32,
            },
            payload,
        })
    }

    /// Serialize the complete frame (header + payload) into bytes.
    ///
    /// Panics if `header.payload_len > MAX_PAYLOAD_SIZE` (only reachable if
    /// the caller constructed `Sv2Frame { header, payload }` directly with
    /// a hand-rolled header field; `Sv2Frame::new` and `try_new` both
    /// validate the bound). Use `try_to_bytes` for graceful handling.
    #[allow(clippy::expect_used)]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_bytes()
            .expect("Sv2Frame::to_bytes called with header.payload_len > MAX_PAYLOAD_SIZE")
    }

    /// Serialize the complete frame, returning `PayloadTooLarge` if the
    /// header carries an oversize `payload_len`.
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, FrameError> {
        let header_bytes = self.header.try_to_bytes()?;
        let mut buf = Vec::with_capacity(FRAME_HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&header_bytes);
        buf.extend_from_slice(&self.payload);
        Ok(buf)
    }

    /// Parse a complete frame from a byte buffer.
    /// Returns the frame and the number of bytes consumed.
    pub fn from_bytes(buf: &[u8]) -> Result<(Self, usize), FrameError> {
        let header = Sv2FrameHeader::from_bytes(buf)?;
        let total_len = FRAME_HEADER_SIZE + header.payload_len as usize;
        if buf.len() < total_len {
            return Err(FrameError::Incomplete {
                have: buf.len(),
                need: total_len,
            });
        }
        let payload = buf[FRAME_HEADER_SIZE..total_len].to_vec();
        Ok((Self { header, payload }, total_len))
    }
}

/// Frame parsing errors
#[derive(Debug)]
pub enum FrameError {
    /// Buffer doesn't contain enough bytes for a header
    BufferTooSmall,
    /// Payload exceeds maximum allowed size
    PayloadTooLarge(u32),
    /// Buffer contains a partial frame
    Incomplete { have: usize, need: usize },
    /// Invalid message type
    InvalidMessageType(u8),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferTooSmall => write!(f, "buffer too small for SV2 header"),
            Self::PayloadTooLarge(n) => write!(f, "payload too large: {} bytes", n),
            Self::Incomplete { have, need } => {
                write!(f, "incomplete frame: have {} bytes, need {}", have, need)
            }
            Self::InvalidMessageType(t) => write!(f, "invalid message type: 0x{:02x}", t),
        }
    }
}

impl std::error::Error for FrameError {}

/// A streaming frame decoder that accumulates bytes and emits complete frames.
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
        }
    }

    /// Feed bytes into the decoder
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Try to extract a complete frame from the buffer.
    /// Returns None if not enough data yet.
    pub fn next_frame(&mut self) -> Result<Option<Sv2Frame>, FrameError> {
        if self.buffer.len() < FRAME_HEADER_SIZE {
            return Ok(None);
        }

        match Sv2Frame::from_bytes(&self.buffer) {
            Ok((frame, consumed)) => {
                self.buffer.drain(..consumed);
                Ok(Some(frame))
            }
            Err(FrameError::Incomplete { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Reset the decoder, discarding any buffered data
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sv2_frame_decoder_never_panics_and_always_makes_progress_on_garbage() {
        // Fuzz the untrusted SV2 network frame decoder (priority 1). A pool / JD
        // proxy — or a hostile peer / MITM before the noise handshake — feeds
        // arbitrary bytes; a panic is a remote DoS and a stuck decoder is a hang.
        // Deterministic LCG. Two targets: (1) the pure Sv2Frame::from_bytes on
        // random buffers; (2) the streaming FrameDecoder fed random chunks, drained
        // to completion. Asserts: no panic; from_bytes returns Ok/Err; and the drain
        // loop ALWAYS terminates — every Ok(Some) consumes >= the 6-byte header, so
        // the decoder can never spin without making progress.
        let mut lcg: u64 = 0x51ED_270B_9EAF_1D3C;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        for _ in 0..4000u32 {
            // (1) pure header/frame parse on a random buffer.
            let blen = (next() % 40) as usize;
            let mut buf = Vec::with_capacity(blen);
            for _ in 0..blen {
                buf.push((next() & 0xFF) as u8);
            }
            let _ = Sv2FrameHeader::from_bytes(&buf); // must not panic
            let _ = Sv2Frame::from_bytes(&buf); // must not panic

            // (2) streaming decoder fed in random chunks.
            let mut dec = FrameDecoder::new();
            let feeds = 1 + (next() % 4);
            for _ in 0..feeds {
                let clen = (next() % 32) as usize;
                let mut chunk = Vec::with_capacity(clen);
                for _ in 0..clen {
                    chunk.push((next() & 0xFF) as u8);
                }
                dec.feed(&chunk);
            }
            let mut guard = 0;
            loop {
                guard += 1;
                assert!(
                    guard < 100_000,
                    "next_frame did not make progress (possible infinite loop)"
                );
                match dec.next_frame() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }

    fn replay_sv2_frame_decoder_fuzz_bytes(data: &[u8]) {
        let mut decoder = FrameDecoder::new();
        let chunk_len = data.first().map(|b| (*b as usize % 64) + 1).unwrap_or(1);

        for chunk in data.chunks(chunk_len) {
            decoder.feed(chunk);
            for _ in 0..8 {
                match decoder.next_frame() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }

        for _ in 0..8 {
            match decoder.next_frame() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
    }

    #[test]
    fn sv2_frame_decoder_fuzz_corpus_replays_under_cargo_test() {
        const CORPUS: &[(&str, &[u8])] = &[
            (
                "empty-frame.bin",
                include_bytes!("../../../fuzz/corpus/sv2_frame_decoder/empty-frame.bin"),
            ),
            (
                "partial-header.bin",
                include_bytes!("../../../fuzz/corpus/sv2_frame_decoder/partial-header.bin"),
            ),
        ];

        for (name, bytes) in CORPUS {
            replay_sv2_frame_decoder_fuzz_bytes(bytes);
            assert!(!name.is_empty(), "corpus entry must carry a name");
        }
    }

    #[test]
    fn test_header_roundtrip() {
        let header = Sv2FrameHeader {
            extension_type: 0x0000,
            msg_type: 0x1e,
            payload_len: 256,
        };
        let bytes = header.to_bytes();
        let parsed = Sv2FrameHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.extension_type, 0x0000);
        assert_eq!(parsed.msg_type, 0x1e);
        assert_eq!(parsed.payload_len, 256);
    }

    #[test]
    fn test_frame_roundtrip() {
        let payload = vec![0x01, 0x02, 0x03, 0x04];
        let frame = Sv2Frame::new(0x0000, 0x10, payload.clone());
        let bytes = frame.to_bytes();

        let (parsed, consumed) = Sv2Frame::from_bytes(&bytes).unwrap();
        assert_eq!(consumed, 10); // 6 header + 4 payload
        assert_eq!(parsed.header.msg_type, 0x10);
        assert_eq!(parsed.payload, payload);
    }

    #[test]
    fn test_frame_decoder_streaming() {
        let mut decoder = FrameDecoder::new();

        let frame = Sv2Frame::new(0x0000, 0x00, vec![0xAA, 0xBB]);
        let bytes = frame.to_bytes();

        // Feed partial data
        decoder.feed(&bytes[..4]);
        assert!(decoder.next_frame().unwrap().is_none());

        // Feed remaining
        decoder.feed(&bytes[4..]);
        let decoded = decoder.next_frame().unwrap().unwrap();
        assert_eq!(decoded.header.msg_type, 0x00);
        assert_eq!(decoded.payload, vec![0xAA, 0xBB]);
    }

    #[test]
    fn test_max_payload_header_round_trip() {
        let mut buf = [0u8; 6];
        buf[0..2].copy_from_slice(&0u16.to_le_bytes());
        buf[2] = 0x00;
        let max = MAX_PAYLOAD_SIZE.to_le_bytes();
        buf[3] = max[0];
        buf[4] = max[1];
        buf[5] = max[2];

        let header = Sv2FrameHeader::from_bytes(&buf).unwrap();
        assert_eq!(header.payload_len, MAX_PAYLOAD_SIZE);
    }

    // -----------------------------------------------------------------------
    // Error-path and edge-case contracts.
    //
    // The frame layer is the lowest wire boundary in the SV2 stack — every
    // mining message and every Noise transport block flows through it. Pin
    // the error branches and edge cases so a future refactor that flips
    // BufferTooSmall to a truncating success or makes Incomplete drain bytes
    // it shouldn't lights up the test suite.
    // -----------------------------------------------------------------------

    #[test]
    fn from_bytes_buffer_too_small_at_zero_bytes() {
        let result = Sv2FrameHeader::from_bytes(&[]);
        assert!(matches!(result, Err(FrameError::BufferTooSmall)));
    }

    #[test]
    fn from_bytes_buffer_too_small_at_one_byte() {
        let result = Sv2FrameHeader::from_bytes(&[0xff]);
        assert!(matches!(result, Err(FrameError::BufferTooSmall)));
    }

    #[test]
    fn from_bytes_buffer_too_small_at_five_bytes() {
        // One short of FRAME_HEADER_SIZE (6).
        let result = Sv2FrameHeader::from_bytes(&[0u8; 5]);
        assert!(matches!(result, Err(FrameError::BufferTooSmall)));
    }

    #[test]
    fn frame_from_bytes_returns_incomplete_for_truncated_payload() {
        // Header claims payload_len=10 but only 4 payload bytes are present.
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes()); // extension_type
        buf.push(0x10); // msg_type
        buf.extend_from_slice(&[10u8, 0, 0]); // payload_len = 10 (24-bit LE)
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // only 4 of 10 payload bytes

        let result = Sv2Frame::from_bytes(&buf);
        match result {
            Err(FrameError::Incomplete { have, need }) => {
                assert_eq!(have, 6 + 4);
                assert_eq!(need, 6 + 10);
            }
            other => panic!("expected Incomplete, got {:?}", other),
        }
    }

    #[test]
    fn frame_from_bytes_zero_length_payload_round_trip() {
        // Zero-payload frames are legal (e.g. SetupConnectionSuccess can be
        // payload-empty in some pool implementations).
        let frame = Sv2Frame::new(0x0000, 0x01, Vec::new());
        let bytes = frame.to_bytes();
        assert_eq!(bytes.len(), FRAME_HEADER_SIZE);
        let (parsed, consumed) = Sv2Frame::from_bytes(&bytes).unwrap();
        assert_eq!(consumed, FRAME_HEADER_SIZE);
        assert_eq!(parsed.header.msg_type, 0x01);
        assert_eq!(parsed.header.payload_len, 0);
        assert!(parsed.payload.is_empty());
    }

    #[test]
    fn decoder_emits_multiple_frames_from_one_feed() {
        let mut decoder = FrameDecoder::new();

        let frame_a = Sv2Frame::new(0x0000, 0x10, vec![0x01, 0x02]);
        let frame_b = Sv2Frame::new(0x0000, 0x11, vec![0x03]);
        let frame_c = Sv2Frame::new(0x0000, 0x12, vec![0x04, 0x05, 0x06]);

        let mut combined = Vec::new();
        combined.extend(frame_a.to_bytes());
        combined.extend(frame_b.to_bytes());
        combined.extend(frame_c.to_bytes());

        decoder.feed(&combined);

        let first = decoder.next_frame().unwrap().unwrap();
        assert_eq!(first.header.msg_type, 0x10);
        assert_eq!(first.payload, vec![0x01, 0x02]);

        let second = decoder.next_frame().unwrap().unwrap();
        assert_eq!(second.header.msg_type, 0x11);
        assert_eq!(second.payload, vec![0x03]);

        let third = decoder.next_frame().unwrap().unwrap();
        assert_eq!(third.header.msg_type, 0x12);
        assert_eq!(third.payload, vec![0x04, 0x05, 0x06]);

        // No fourth frame.
        assert!(decoder.next_frame().unwrap().is_none());
    }

    #[test]
    fn decoder_returns_none_when_buffer_below_header_size() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0u8; 3]);
        assert!(decoder.next_frame().unwrap().is_none());
        // No bytes should be drained — the next feed completes the header.
        decoder.feed(&[0u8; 3]); // total 6 bytes, header complete, payload_len=0
        let frame = decoder.next_frame().unwrap().unwrap();
        assert_eq!(frame.header.payload_len, 0);
    }

    #[test]
    fn decoder_reset_discards_buffered_bytes() {
        let mut decoder = FrameDecoder::new();
        // Feed a complete frame, then reset — the frame must NOT come out.
        let frame = Sv2Frame::new(0x0000, 0x10, vec![0xAA]);
        decoder.feed(&frame.to_bytes());
        decoder.reset();
        assert!(decoder.next_frame().unwrap().is_none());

        // Feed a fresh frame after reset to prove the decoder still works.
        let new_frame = Sv2Frame::new(0x0000, 0x11, vec![0xBB]);
        decoder.feed(&new_frame.to_bytes());
        let decoded = decoder.next_frame().unwrap().unwrap();
        assert_eq!(decoded.header.msg_type, 0x11);
        assert_eq!(decoded.payload, vec![0xBB]);
    }

    #[test]
    fn frame_error_display_messages_are_actionable() {
        // Operators read these when debugging Noise/transport issues. Pin
        // the message strings so a refactor that strips the byte counts
        // gets caught.
        assert!(FrameError::BufferTooSmall
            .to_string()
            .contains("buffer too small"));
        assert!(FrameError::PayloadTooLarge(99_999)
            .to_string()
            .contains("99999"));

        let incomplete = FrameError::Incomplete { have: 4, need: 16 };
        let s = incomplete.to_string();
        assert!(
            s.contains("4"),
            "Incomplete message must include `have` count: {s}"
        );
        assert!(
            s.contains("16"),
            "Incomplete message must include `need` count: {s}"
        );

        assert!(FrameError::InvalidMessageType(0x42)
            .to_string()
            .contains("0x42"));
    }

    #[test]
    fn header_payload_len_max_24_bit_round_trip() {
        // The 24-bit length field caps at 2^24 - 1 = 16777215 bytes.
        // `u32::from_le_bytes([0xFF, 0xFF, 0xFF, 0])` yields exactly that
        // value, so the parse path can never produce PayloadTooLarge from
        // a real on-wire 6-byte header. Pin this so a refactor that adds
        // a 4th byte to the length field doesn't silently break the
        // wire format.
        let header = Sv2FrameHeader {
            extension_type: 0x0000,
            msg_type: 0x10,
            payload_len: MAX_PAYLOAD_SIZE,
        };
        let bytes = header.to_bytes();
        let parsed = Sv2FrameHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.payload_len, MAX_PAYLOAD_SIZE);
    }

    #[test]
    fn header_try_to_bytes_returns_payload_too_large_at_24_bit_boundary() {
        // : the silent-truncation bug at the > 16MB boundary is now
        // a real PayloadTooLarge error. `try_to_bytes` returns Err so the
        // caller can decide what to do; `to_bytes` panics (loud failure
        // for protocol violations).
        let header = Sv2FrameHeader {
            extension_type: 0x0000,
            msg_type: 0x10,
            payload_len: 0x0100_0000, // exactly 1 byte past MAX_PAYLOAD_SIZE
        };
        let result = header.try_to_bytes();
        assert!(
            matches!(result, Err(FrameError::PayloadTooLarge(0x0100_0000))),
            "try_to_bytes must surface PayloadTooLarge, got {:?}",
            result
        );
    }

    #[test]
    #[should_panic(
        expected = "Sv2FrameHeader::to_bytes called with payload_len > MAX_PAYLOAD_SIZE"
    )]
    fn header_to_bytes_panics_above_24_bit_boundary() {
        let header = Sv2FrameHeader {
            extension_type: 0x0000,
            msg_type: 0x10,
            payload_len: 0x0100_0000,
        };
        let _ = header.to_bytes();
    }

    #[test]
    fn frame_try_new_accepts_max_payload_size() {
        // The exact MAX_PAYLOAD_SIZE must be acceptable — it's the protocol's
        // legal upper bound, not a fence-post error.
        let payload = vec![0u8; MAX_PAYLOAD_SIZE as usize];
        let frame = Sv2Frame::try_new(0x0000, 0x10, payload).unwrap();
        assert_eq!(frame.header.payload_len, MAX_PAYLOAD_SIZE);
    }

    #[test]
    fn frame_try_new_rejects_one_byte_past_max() {
        // Defensive: one byte past the cap must surface PayloadTooLarge so
        // a future caller working with untrusted payload sizes can fail
        // gracefully instead of corrupting the stream.
        let payload = vec![0u8; MAX_PAYLOAD_SIZE as usize + 1];
        let result = Sv2Frame::try_new(0x0000, 0x10, payload);
        assert!(
            matches!(
                result,
                Err(FrameError::PayloadTooLarge(n)) if n == MAX_PAYLOAD_SIZE + 1
            ),
            "try_new must reject one-byte-past-max, got {:?}",
            result.map(|_| ())
        );
    }

    #[test]
    #[should_panic(expected = "Sv2Frame::new called with payload > MAX_PAYLOAD_SIZE")]
    fn frame_new_panics_above_max_payload() {
        // `new` is the loud-failure constructor: protocol violations should
        // panic so dev/test catches them early. Production callers pass
        // bounded payloads from message builders so this is unreachable on
        // the happy path.
        let payload = vec![0u8; MAX_PAYLOAD_SIZE as usize + 1];
        let _ = Sv2Frame::new(0x0000, 0x10, payload);
    }

    #[test]
    fn frame_try_new_smoke_test_for_typical_mining_frame() {
        // Sanity: ordinary mining frames (small payloads) take the success
        // path on try_new and produce identical output to new.
        let frame_via_new = Sv2Frame::new(0x0000, 0x1a, vec![0xDE, 0xAD]);
        let frame_via_try = Sv2Frame::try_new(0x0000, 0x1a, vec![0xDE, 0xAD]).unwrap();
        assert_eq!(frame_via_new.to_bytes(), frame_via_try.to_bytes());
    }
}
