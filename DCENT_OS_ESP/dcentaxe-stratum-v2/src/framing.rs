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

/// Maximum allowed payload size — derived from the single authoritative budget
/// (`crate::MAX_MESSAGE_SIZE` = 16384, the ESP32 RAM budget). This is also
/// AEAD-safe (<= 65519 single-block ChaChaPoly plaintext) and equals
/// `noise::MAX_ENCRYPTED_SIZE - 16`. SV2 standard-channel frames are tiny (jobs
/// ~80B, shares 24B), so 16KB is comfortably above any legitimate frame; chunking
/// for payloads >65535 is intentionally out of scope for a standard-channel client.
/// The decoder rejects `payload_len > MAX_PAYLOAD_SIZE` at this authoritative budget.
pub const MAX_PAYLOAD_SIZE: u32 = crate::MAX_MESSAGE_SIZE as u32;

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

    /// Serialize this header into a 6-byte buffer
    pub fn to_bytes(&self) -> [u8; FRAME_HEADER_SIZE] {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&self.extension_type.to_le_bytes());
        buf[2] = self.msg_type;
        let len_bytes = self.payload_len.to_le_bytes();
        buf[3] = len_bytes[0];
        buf[4] = len_bytes[1];
        buf[5] = len_bytes[2];
        buf
    }
}

/// A complete SV2 frame (header + payload)
#[derive(Debug)]
pub struct Sv2Frame {
    pub header: Sv2FrameHeader,
    pub payload: Vec<u8>,
}

impl Sv2Frame {
    /// Create a new frame with the given message type and payload
    pub fn new(extension_type: u16, msg_type: u8, payload: Vec<u8>) -> Self {
        Self {
            header: Sv2FrameHeader {
                extension_type,
                msg_type,
                payload_len: payload.len() as u32,
            },
            payload,
        }
    }

    /// Serialize the complete frame (header + payload) into bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FRAME_HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&self.header.to_bytes());
        buf.extend_from_slice(&self.payload);
        buf
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_payload_too_large() {
        let mut buf = [0u8; 6];
        buf[0..2].copy_from_slice(&0u16.to_le_bytes());
        buf[2] = 0x00;
        // Set length to MAX_PAYLOAD_SIZE + 1
        let big = (MAX_PAYLOAD_SIZE + 1).to_le_bytes();
        buf[3] = big[0];
        buf[4] = big[1];
        buf[5] = big[2];
        assert!(matches!(
            Sv2FrameHeader::from_bytes(&buf),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    /// SV2-7: a header whose payload_len == MAX_PAYLOAD_SIZE parses OK (boundary
    /// accepted), and payload_len == MAX_PAYLOAD_SIZE + 1 is rejected.
    #[test]
    fn test_payload_at_budget_accepted() {
        let mut at = [0u8; 6];
        let len = MAX_PAYLOAD_SIZE.to_le_bytes();
        at[3] = len[0];
        at[4] = len[1];
        at[5] = len[2];
        let parsed = Sv2FrameHeader::from_bytes(&at).expect("payload at budget must parse");
        assert_eq!(parsed.payload_len, MAX_PAYLOAD_SIZE);

        let mut over = [0u8; 6];
        let over_len = (MAX_PAYLOAD_SIZE + 1).to_le_bytes();
        over[3] = over_len[0];
        over[4] = over_len[1];
        over[5] = over_len[2];
        assert!(matches!(
            Sv2FrameHeader::from_bytes(&over),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    /// SV2-7: lock the three ceilings together so a future drift fails CI:
    /// framing budget == lib MAX_MESSAGE_SIZE, and it stays AEAD-safe (<= 65519
    /// single-block plaintext, here checked against the conservative 65535 wire cap).
    #[test]
    fn test_framing_budget_matches_lib() {
        assert_eq!(MAX_PAYLOAD_SIZE as usize, crate::MAX_MESSAGE_SIZE);
        assert!(MAX_PAYLOAD_SIZE as usize + 16 <= 65535);
    }
}
