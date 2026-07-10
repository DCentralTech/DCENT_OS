//! Line-delimited JSON codec for Stratum V1.
//!
//! Stratum V1 uses newline-delimited JSON over TCP. Each message is a single
//! line of JSON text terminated by '\n'. This module provides encode/decode
//! functions for converting between JSON text and typed Stratum messages.

use serde_json::Value;

/// Maximum line length for a Stratum message (16 KB).
pub const MAX_LINE_LENGTH: usize = 16 * 1024;

/// Delimiter for Stratum V1 messages.
pub const DELIMITER: u8 = b'\n';

/// Encode a JSON value into a newline-terminated string.
pub fn encode(value: &Value) -> String {
    let mut line = serde_json::to_string(value).unwrap_or_default();
    line.push('\n');
    line
}

/// Decode a line of text into a JSON value.
///
/// Returns None if the line is empty or not valid JSON.
pub fn decode(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// A line-based frame decoder for buffered TCP reads.
///
/// Accumulates bytes until a complete newline-delimited line is found,
/// then returns the complete line.
pub struct LineCodec {
    buffer: Vec<u8>,
}

impl LineCodec {
    /// Create a new line codec.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Feed bytes into the codec buffer.
    ///
    /// Returns an iterator of complete lines found in the buffer.
    pub fn feed(&mut self, data: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(data);

        let mut lines = Vec::new();

        while let Some(pos) = self.buffer.iter().position(|&b| b == DELIMITER) {
            let line_bytes = self.buffer.drain(..=pos).collect::<Vec<_>>();
            if let Ok(line) = String::from_utf8(line_bytes) {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    lines.push(trimmed);
                }
            }
        }

        // Protect against unbounded buffer growth
        if self.buffer.len() > MAX_LINE_LENGTH {
            self.buffer.clear();
        }

        lines
    }

    /// Reset the codec buffer.
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

impl Default for LineCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn max_line_length_stays_16_kib() {
        assert_eq!(MAX_LINE_LENGTH, 16 * 1024);
    }

    #[test]
    fn encode_appends_newline_delimiter() {
        let value = json!({"id": 1, "method": "ping"});
        let line = encode(&value);
        assert!(line.ends_with('\n'), "encoded line must end with \\n");
        // The JSON itself must be parseable round-trip.
        let trimmed = line.trim_end();
        let recovered: Value = serde_json::from_str(trimmed).unwrap();
        assert_eq!(recovered, value);
    }

    #[test]
    fn encode_handles_empty_object() {
        let line = encode(&json!({}));
        assert_eq!(line, "{}\n");
    }

    #[test]
    fn decode_returns_none_for_empty_string() {
        assert!(decode("").is_none());
    }

    #[test]
    fn decode_returns_none_for_whitespace_only() {
        // Whitespace-only payloads (which can arrive after a CRLF or stray
        // keepalive) must NOT panic and must NOT round-trip as Value::Null.
        assert!(decode("   ").is_none());
        assert!(decode("\t\t").is_none());
        assert!(decode("\r\n").is_none());
    }

    #[test]
    fn decode_returns_none_for_invalid_json() {
        assert!(decode("not json").is_none());
        assert!(decode("{unclosed").is_none());
        assert!(decode("[1,2,").is_none());
    }

    #[test]
    fn decode_accepts_valid_json_and_strips_surrounding_whitespace() {
        let value = decode("  {\"id\":1}  ").unwrap();
        assert_eq!(value, json!({"id": 1}));
    }

    #[test]
    fn line_codec_emits_complete_line_on_newline() {
        let mut codec = LineCodec::new();
        let lines = codec.feed(b"hello\n");
        assert_eq!(lines, vec!["hello".to_string()]);
    }

    #[test]
    fn line_codec_emits_multiple_lines_in_one_feed() {
        let mut codec = LineCodec::new();
        let lines = codec.feed(b"first\nsecond\nthird\n");
        assert_eq!(
            lines,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ]
        );
    }

    #[test]
    fn line_codec_buffers_partial_line_until_newline_arrives() {
        let mut codec = LineCodec::new();
        // First half — no line yet.
        let first = codec.feed(b"partial");
        assert!(first.is_empty(), "partial line must not emit");

        // Complete the line.
        let second = codec.feed(b" rest\n");
        assert_eq!(second, vec!["partial rest".to_string()]);
    }

    #[test]
    fn line_codec_filters_empty_lines() {
        // Real pool TCP streams sometimes contain extra newlines (keepalive
        // padding, double-CRLF). Empty lines must NOT surface as empty
        // String entries — that would propagate to `decode("")` which
        // returns None and pollute the dispatcher with no-op events.
        let mut codec = LineCodec::new();
        let lines = codec.feed(b"\n\n\n");
        assert!(lines.is_empty());

        let mixed = codec.feed(b"real\n\n\n");
        assert_eq!(mixed, vec!["real".to_string()]);
    }

    #[test]
    fn line_codec_strips_carriage_return() {
        // Some pool implementations send CRLF instead of LF. The codec
        // splits on \n then trims, so CR ends up at the end of `line`
        // before trim. Pin that the trailing \r is removed.
        let mut codec = LineCodec::new();
        let lines = codec.feed(b"hello\r\nworld\r\n");
        assert_eq!(lines, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn line_codec_drops_invalid_utf8_line_silently() {
        // Real-world pool TCP streams occasionally carry binary noise
        // (e.g. a misbehaving NAT injects a few bytes during reconnect).
        // The codec uses `String::from_utf8` which returns Err on invalid
        // UTF-8 — the bad line is silently dropped. Pin that this does
        // NOT panic and does NOT corrupt subsequent valid lines.
        let mut codec = LineCodec::new();
        let lines = codec.feed(b"\xff\xfe\xfd\xfc\nvalid\n");
        // The \xff line is dropped by from_utf8; "valid" must still emit.
        assert_eq!(lines, vec!["valid".to_string()]);
    }

    #[test]
    fn line_codec_preserves_buffer_at_exactly_max_line_length() {
        // The buffer-clear trigger is `> MAX_LINE_LENGTH`, NOT `>=`. A line
        // exactly MAX_LINE_LENGTH bytes long (no newline yet) must remain
        // buffered so an arriving newline still emits the line.
        let mut codec = LineCodec::new();
        let payload = vec![b'a'; MAX_LINE_LENGTH];
        let no_lines = codec.feed(&payload);
        assert!(no_lines.is_empty());
        // Now send the newline — line emerges intact.
        let lines = codec.feed(b"\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE_LENGTH);
    }

    #[test]
    fn line_codec_clears_buffer_when_oversize_with_no_newline() {
        // Buffer-bomb protection: a pool that streams more than
        // MAX_LINE_LENGTH bytes without a newline must NOT cause unbounded
        // memory growth. The codec clears the buffer once it exceeds the
        // cap. Subsequent messages must still parse normally.
        let mut codec = LineCodec::new();
        let bomb = vec![b'x'; MAX_LINE_LENGTH + 1024];
        let no_lines = codec.feed(&bomb);
        assert!(no_lines.is_empty());

        // After the buffer clear, a fresh complete line must still emit.
        let recovered = codec.feed(b"after-bomb\n");
        assert_eq!(recovered, vec!["after-bomb".to_string()]);
    }

    #[test]
    fn line_codec_reset_discards_buffered_partial_line() {
        let mut codec = LineCodec::new();
        codec.feed(b"half-buffered");
        codec.reset();
        // After reset, the prior half-line must NOT join the next feed.
        let lines = codec.feed(b"-line\nfresh\n");
        assert_eq!(lines, vec!["-line".to_string(), "fresh".to_string()]);
    }

    #[test]
    fn line_codec_reset_after_complete_line_is_idempotent() {
        let mut codec = LineCodec::new();
        codec.feed(b"complete\n");
        codec.reset(); // buffer was already empty after feed; must not panic
        let lines = codec.feed(b"next\n");
        assert_eq!(lines, vec!["next".to_string()]);
    }

    #[test]
    fn line_codec_default_works_like_new() {
        let mut a = LineCodec::default();
        let mut b = LineCodec::new();
        assert_eq!(a.feed(b"x\n"), b.feed(b"x\n"));
    }
}
