// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal protobuf wire codec + the handful of Meshtastic messages a DCENT_axe
//! Router needs to speak.
//!
//! This is a **hand-rolled, dependency-free protobuf subset** — not the full
//! `prost`/`nanopb` generated stack (which would blow the entry board's RAM/OTA
//! budget, exactly the constraint the fork plan called out). It implements only:
//!   * the four proto wire types (varint / 64-bit / length-delimited / 32-bit),
//!   * a **forward-compatible** reader that *skips unknown fields* — essential,
//!     because a real Meshtastic `Data`/`User` packet carries fields this build
//!     does not model, and dropping them must never fail the decode, and
//!   * the specific messages we round-trip: [`Data`] (the portnum envelope every
//!     app rides in), [`User`] (NodeInfo — how a node announces its name), and
//!     [`Position`] (lat/lon/alt).
//!
//! Every decoder takes UNTRUSTED over-the-air bytes, so the contract is the same
//! as `mesh::MeshFrame::decode`: return `Ok`/`Err`, **never panic** (pinned by a
//! fuzz test). Field numbers/types are per the upstream `meshtastic/protobufs`
//! (`mesh.proto`) as of the 2.x series.

/// Protobuf wire types (`tag & 0x07`).
pub mod wire_type {
    pub const VARINT: u32 = 0;
    pub const FIXED64: u32 = 1;
    pub const LEN: u32 = 2;
    pub const FIXED32: u32 = 5;
}

/// Meshtastic `PortNum` values (subset). The portnum tags what app a `Data`
/// payload belongs to; a Router relays them all but only *interprets* a few.
pub mod portnum {
    pub const UNKNOWN_APP: u32 = 0;
    pub const TEXT_MESSAGE_APP: u32 = 1;
    pub const REMOTE_HARDWARE_APP: u32 = 2;
    pub const POSITION_APP: u32 = 3;
    pub const NODEINFO_APP: u32 = 4;
    pub const ROUTING_APP: u32 = 5;
    pub const ADMIN_APP: u32 = 6;
    pub const WAYPOINT_APP: u32 = 8;
    pub const TELEMETRY_APP: u32 = 67;
    /// Start of the private/experimental range (never a registered app).
    pub const PRIVATE_APP: u32 = 256;
    /// DCENT-private structured `$DCM` telemetry carried over a Meshtastic mesh
    /// (Phase 3, DCENT↔DCENT). Sits in the `PRIVATE_APP` (≥ 256) range so it can
    /// never collide with a registered Meshtastic app portnum.
    pub const DCENT_DCM_APP: u32 = PRIVATE_APP + 0x2B; // 299
}

/// Meshtastic `HardwareModel` values (subset). A DCENT_axe is not a registered
/// Meshtastic hardware model, so it honestly announces `PRIVATE_HW` (255).
pub mod hw_model {
    pub const UNSET: u32 = 0;
    pub const PRIVATE_HW: u32 = 255;
}

/// A protobuf decode failure over untrusted bytes. Small + `Clone` so tests can
/// assert on the variant; the decoders never panic, they return one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The buffer ended in the middle of a field / varint / fixed value.
    Truncated,
    /// A varint ran past 10 bytes (would overflow `u64`).
    VarintOverflow,
    /// A length-delimited field declared a length past the end of the buffer.
    BadLength,
    /// A wire type this codec does not implement (3/4 = deprecated groups).
    UnknownWireType(u32),
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WireError::Truncated => write!(f, "protobuf: truncated"),
            WireError::VarintOverflow => write!(f, "protobuf: varint overflow"),
            WireError::BadLength => write!(f, "protobuf: length past end"),
            WireError::UnknownWireType(w) => write!(f, "protobuf: unknown wire type {w}"),
        }
    }
}

impl std::error::Error for WireError {}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// A tiny append-only protobuf writer. Fields must be written in ascending
/// field-number order to match canonical encoders (not required by the wire
/// format, but keeps our output byte-stable and test-pinnable).
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Append a base-128 varint (LEB128, LSB-first, MSB = continuation).
    pub fn write_varint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                self.buf.push(byte | 0x80);
            } else {
                self.buf.push(byte);
                break;
            }
        }
    }

    fn write_tag(&mut self, field: u32, wire: u32) {
        self.write_varint(((field as u64) << 3) | wire as u64);
    }

    /// `uint32`/`enum` field (varint). Caller decides whether to skip a zero
    /// value (proto3 omits scalar defaults).
    pub fn write_uint32(&mut self, field: u32, v: u32) {
        self.write_tag(field, wire_type::VARINT);
        self.write_varint(v as u64);
    }

    /// `int32` field. Negative values are sign-extended to 64 bits before the
    /// varint (proto3 semantics) so they round-trip through `i32`.
    pub fn write_int32(&mut self, field: u32, v: i32) {
        self.write_tag(field, wire_type::VARINT);
        self.write_varint(v as i64 as u64);
    }

    /// `bool` field (varint 0/1).
    pub fn write_bool(&mut self, field: u32, v: bool) {
        self.write_tag(field, wire_type::VARINT);
        self.write_varint(v as u64);
    }

    /// Length-delimited `bytes` field.
    pub fn write_bytes(&mut self, field: u32, data: &[u8]) {
        self.write_tag(field, wire_type::LEN);
        self.write_varint(data.len() as u64);
        self.buf.extend_from_slice(data);
    }

    /// Length-delimited `string` field.
    pub fn write_string(&mut self, field: u32, s: &str) {
        self.write_bytes(field, s.as_bytes());
    }

    /// `fixed32`/`sfixed32` field (4 bytes, little-endian).
    pub fn write_fixed32(&mut self, field: u32, v: u32) {
        self.write_tag(field, wire_type::FIXED32);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Reader (forward-compatible: unknown fields are skipped, not fatal)
// ---------------------------------------------------------------------------

/// One decoded protobuf value, borrowed from the source buffer.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl Value<'_> {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Varint(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().map(|v| v as u32)
    }
    pub fn as_i32(&self) -> Option<i32> {
        self.as_u64().map(|v| v as i32)
    }
    pub fn as_bool(&self) -> Option<bool> {
        self.as_u64().map(|v| v != 0)
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    /// UTF-8 string from a `Bytes` value; lossy so hostile bytes never fail the
    /// whole decode (a mangled name is better than a dropped NodeInfo).
    pub fn as_string_lossy(&self) -> Option<String> {
        self.as_bytes()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
    /// `sfixed32`/`fixed32` reinterpreted as signed.
    pub fn as_sfixed32(&self) -> Option<i32> {
        match self {
            Value::Fixed32(v) => Some(*v as i32),
            _ => None,
        }
    }
}

/// A field-at-a-time protobuf reader. `next()` yields `(field_number, Value)`
/// and transparently skips wire types the caller does not consume.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_varint(&mut self) -> Result<u64, WireError> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *self.buf.get(self.pos).ok_or(WireError::Truncated)?;
            self.pos += 1;
            if shift >= 64 {
                return Err(WireError::VarintOverflow);
            }
            // The 10th byte may only contribute the top bit; guard the shift.
            result |= ((byte & 0x7f) as u64)
                .checked_shl(shift)
                .ok_or(WireError::VarintOverflow)?;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError::BadLength)?;
        let slice = self.buf.get(self.pos..end).ok_or(WireError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// Decode the next `(field, value)` pair, or `None` at clean end-of-buffer.
    pub fn next_field(&mut self) -> Option<Result<(u32, Value<'a>), WireError>> {
        if self.pos >= self.buf.len() {
            return None;
        }
        Some(self.next_field_inner())
    }

    fn next_field_inner(&mut self) -> Result<(u32, Value<'a>), WireError> {
        let tag = self.read_varint()?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u32;
        let value = match wire {
            wire_type::VARINT => Value::Varint(self.read_varint()?),
            wire_type::FIXED64 => {
                let b = self.take(8)?;
                Value::Fixed64(u64::from_le_bytes(b.try_into().unwrap()))
            }
            wire_type::LEN => {
                let len = self.read_varint()? as usize;
                Value::Bytes(self.take(len)?)
            }
            wire_type::FIXED32 => {
                let b = self.take(4)?;
                Value::Fixed32(u32::from_le_bytes(b.try_into().unwrap()))
            }
            other => return Err(WireError::UnknownWireType(other)),
        };
        Ok((field, value))
    }
}

// ---------------------------------------------------------------------------
// Data — the portnum envelope (mesh.proto `Data`)
// ---------------------------------------------------------------------------

/// A decoded/decodable Meshtastic `Data` payload (the plaintext under the
/// channel encryption). We model the fields a Router acts on; everything else is
/// skipped on decode and omitted on encode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    /// `portnum` (field 1) — which app this payload belongs to.
    pub portnum: u32,
    /// `payload` (field 2) — the app-specific bytes.
    pub payload: Vec<u8>,
    /// `dest` (field 3), `source` (field 4) — used by DMs/routing (0 = unset).
    pub dest: u32,
    pub source: u32,
    /// `request_id` (field 5) / `reply_id` (field 6) — ack/reply correlation.
    pub request_id: u32,
    pub reply_id: u32,
    /// `want_response` (field 8).
    pub want_response: bool,
}

impl Data {
    pub fn new(portnum: u32, payload: Vec<u8>) -> Self {
        Self {
            portnum,
            payload,
            ..Default::default()
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        // portnum is written even when 0 (UNKNOWN_APP) so an all-default Data is
        // still a well-formed message; payload always present (may be empty).
        w.write_uint32(1, self.portnum);
        w.write_bytes(2, &self.payload);
        if self.dest != 0 {
            w.write_uint32(3, self.dest);
        }
        if self.source != 0 {
            w.write_uint32(4, self.source);
        }
        if self.request_id != 0 {
            w.write_uint32(5, self.request_id);
        }
        if self.reply_id != 0 {
            w.write_uint32(6, self.reply_id);
        }
        if self.want_response {
            w.write_bool(8, true);
        }
        w.into_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Data, WireError> {
        let mut d = Data::default();
        let mut r = Reader::new(buf);
        while let Some(field) = r.next_field() {
            let (num, val) = field?;
            match num {
                1 => d.portnum = val.as_u32().unwrap_or(0),
                2 => d.payload = val.as_bytes().unwrap_or(&[]).to_vec(),
                3 => d.dest = val.as_u32().unwrap_or(0),
                4 => d.source = val.as_u32().unwrap_or(0),
                5 => d.request_id = val.as_u32().unwrap_or(0),
                6 => d.reply_id = val.as_u32().unwrap_or(0),
                8 => d.want_response = val.as_bool().unwrap_or(false),
                _ => {} // forward-compat: skip unknown fields
            }
        }
        Ok(d)
    }
}

// ---------------------------------------------------------------------------
// User — NodeInfo (mesh.proto `User`)
// ---------------------------------------------------------------------------

/// A node's self-announced identity, carried on [`portnum::NODEINFO_APP`]. This
/// is how a DCENT_axe appears by name in every Meshtastic client's node list —
/// and how we learn peer names off the air.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct User {
    /// `id` (field 1) — canonical node id, e.g. `"!deadbeef"`.
    pub id: String,
    /// `long_name` (field 2) — display name.
    pub long_name: String,
    /// `short_name` (field 3) — up to 4 chars, shown on small screens.
    pub short_name: String,
    /// `hw_model` (field 5).
    pub hw_model: u32,
    /// `is_licensed` (field 6).
    pub is_licensed: bool,
}

impl User {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.id.is_empty() {
            w.write_string(1, &self.id);
        }
        if !self.long_name.is_empty() {
            w.write_string(2, &self.long_name);
        }
        if !self.short_name.is_empty() {
            w.write_string(3, &self.short_name);
        }
        if self.hw_model != 0 {
            w.write_uint32(5, self.hw_model);
        }
        if self.is_licensed {
            w.write_bool(6, true);
        }
        w.into_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<User, WireError> {
        let mut u = User::default();
        let mut r = Reader::new(buf);
        while let Some(field) = r.next_field() {
            let (num, val) = field?;
            match num {
                1 => u.id = val.as_string_lossy().unwrap_or_default(),
                2 => u.long_name = val.as_string_lossy().unwrap_or_default(),
                3 => u.short_name = val.as_string_lossy().unwrap_or_default(),
                5 => u.hw_model = val.as_u32().unwrap_or(0),
                6 => u.is_licensed = val.as_bool().unwrap_or(false),
                _ => {}
            }
        }
        Ok(u)
    }
}

// ---------------------------------------------------------------------------
// Position — lat/lon/alt (mesh.proto `Position`)
// ---------------------------------------------------------------------------

/// A node position, carried on [`portnum::POSITION_APP`]. `latitude_i`/
/// `longitude_i` are degrees × 1e7 (`sfixed32`); `altitude` is metres MSL.
/// proto3 scalar fields cannot distinguish "0" from "absent", so a decoded
/// all-zero position is indistinguishable from an unset one (documented, matches
/// upstream behaviour). A DCENT_axe is stationary and does not emit Position by
/// default; decode is the useful direction (show where mesh nodes are).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Position {
    pub latitude_i: i32,
    pub longitude_i: i32,
    pub altitude: i32,
}

impl Position {
    /// Degrees from the fixed-point `latitude_i` (× 1e-7).
    pub fn latitude_deg(&self) -> f64 {
        self.latitude_i as f64 * 1e-7
    }
    /// Degrees from the fixed-point `longitude_i` (× 1e-7).
    pub fn longitude_deg(&self) -> f64 {
        self.longitude_i as f64 * 1e-7
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if self.latitude_i != 0 {
            w.write_fixed32(1, self.latitude_i as u32);
        }
        if self.longitude_i != 0 {
            w.write_fixed32(2, self.longitude_i as u32);
        }
        if self.altitude != 0 {
            w.write_int32(3, self.altitude);
        }
        w.into_vec()
    }

    pub fn decode(buf: &[u8]) -> Result<Position, WireError> {
        let mut p = Position::default();
        let mut r = Reader::new(buf);
        while let Some(field) = r.next_field() {
            let (num, val) = field?;
            match num {
                1 => p.latitude_i = val.as_sfixed32().unwrap_or(0),
                2 => p.longitude_i = val.as_sfixed32().unwrap_or(0),
                3 => p.altitude = val.as_i32().unwrap_or(0),
                _ => {}
            }
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- varint / wire primitives ----

    fn varint_bytes(v: u64) -> Vec<u8> {
        let mut w = Writer::new();
        w.write_varint(v);
        w.into_vec()
    }

    #[test]
    fn varint_round_trips_edge_values() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let bytes = varint_bytes(v);
            let mut r = Reader::new(&bytes);
            assert_eq!(r.read_varint().unwrap(), v, "varint {v}");
        }
    }

    #[test]
    fn varint_known_encodings() {
        assert_eq!(varint_bytes(0), vec![0x00]);
        assert_eq!(varint_bytes(1), vec![0x01]);
        assert_eq!(varint_bytes(127), vec![0x7f]);
        assert_eq!(varint_bytes(128), vec![0x80, 0x01]);
        assert_eq!(varint_bytes(300), vec![0xac, 0x02]);
    }

    #[test]
    fn varint_overflow_is_error_not_panic() {
        // 11 continuation bytes → never terminates within u64.
        let bytes = [0xffu8; 11];
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            r.read_varint(),
            Err(WireError::VarintOverflow) | Err(WireError::Truncated)
        ));
    }

    #[test]
    fn tag_field_and_wire_split() {
        // field 2, wire type LEN → tag = (2<<3)|2 = 0x12.
        let mut w = Writer::new();
        w.write_bytes(2, &[0xaa, 0xbb]);
        let bytes = w.into_vec();
        assert_eq!(bytes[0], 0x12);
        let mut r = Reader::new(&bytes);
        let (field, val) = r.next_field().unwrap().unwrap();
        assert_eq!(field, 2);
        assert_eq!(val.as_bytes(), Some(&[0xaa, 0xbb][..]));
    }

    // ---- Data ----

    #[test]
    fn data_round_trip_minimal() {
        let d = Data::new(portnum::TEXT_MESSAGE_APP, b"gm nostr".to_vec());
        let back = Data::decode(&d.encode()).unwrap();
        assert_eq!(back, d);
        assert_eq!(back.portnum, 1);
        assert_eq!(back.payload, b"gm nostr");
    }

    #[test]
    fn data_round_trip_all_fields() {
        let d = Data {
            portnum: portnum::TELEMETRY_APP,
            payload: vec![1, 2, 3, 4],
            dest: 0xdead_beef,
            source: 0x0000_00a1,
            request_id: 42,
            reply_id: 7,
            want_response: true,
        };
        assert_eq!(Data::decode(&d.encode()).unwrap(), d);
    }

    #[test]
    fn data_known_wire_bytes() {
        // portnum=1 (field 1 varint), payload="Hi" (field 2 len=2).
        // 0x08 0x01  0x12 0x02 'H' 'i'
        let d = Data::new(1, b"Hi".to_vec());
        assert_eq!(d.encode(), vec![0x08, 0x01, 0x12, 0x02, b'H', b'i']);
    }

    #[test]
    fn data_decode_skips_unknown_fields() {
        // A real Meshtastic Data carries fields we don't model (e.g. field 7
        // fixed32 emoji, field 9 bitfield). Hand-build one and prove decode keeps
        // the known fields and ignores the rest.
        let mut w = Writer::new();
        w.write_uint32(1, portnum::NODEINFO_APP); // portnum
        w.write_fixed32(7, 0x1234_5678); // unknown emoji field (fixed32)
        w.write_bytes(2, b"payload"); // out-of-order known field
        w.write_uint32(99, 0xffff); // unknown high field (varint)
        let d = Data::decode(&w.into_vec()).unwrap();
        assert_eq!(d.portnum, portnum::NODEINFO_APP);
        assert_eq!(d.payload, b"payload");
    }

    // ---- User (NodeInfo) ----

    #[test]
    fn user_round_trip() {
        let u = User {
            id: "!deadbeef".into(),
            long_name: "DCENT_axe Hex".into(),
            short_name: "DCAX".into(),
            hw_model: hw_model::PRIVATE_HW,
            is_licensed: false,
        };
        assert_eq!(User::decode(&u.encode()).unwrap(), u);
    }

    #[test]
    fn user_empty_strings_omitted_but_decode_defaults() {
        let u = User {
            id: "!01020304".into(),
            ..Default::default()
        };
        let bytes = u.encode();
        // Only field 1 present → first byte is tag (1<<3)|2 = 0x0a.
        assert_eq!(bytes[0], 0x0a);
        let back = User::decode(&bytes).unwrap();
        assert_eq!(back.id, "!01020304");
        assert_eq!(back.long_name, "");
        assert_eq!(back.hw_model, 0);
    }

    // ---- Position ----

    #[test]
    fn position_round_trip_and_degrees() {
        // Montreal-ish: 45.5019°N, -73.5674°E.
        let p = Position {
            latitude_i: 455_019_000,
            longitude_i: -735_674_000,
            altitude: 42,
        };
        let back = Position::decode(&p.encode()).unwrap();
        assert_eq!(back, p);
        assert!((back.latitude_deg() - 45.5019).abs() < 1e-4);
        assert!((back.longitude_deg() - -73.5674).abs() < 1e-4);
    }

    #[test]
    fn position_negative_coords_survive_sfixed32() {
        let p = Position {
            latitude_i: -1,
            longitude_i: i32::MIN,
            altitude: -100,
        };
        assert_eq!(Position::decode(&p.encode()).unwrap(), p);
    }

    // ---- untrusted-bytes no-panic guarantee ----

    #[test]
    fn decoders_never_panic_on_arbitrary_bytes() {
        let mut state: u32 = 0x1234_5678;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _ in 0..8000 {
            let len = (next() % 260) as usize;
            let buf: Vec<u8> = (0..len).map(|_| (next() >> 3) as u8).collect();
            // Any Result is fine; the only contract is "must not panic".
            let _ = Data::decode(&buf);
            let _ = User::decode(&buf);
            let _ = Position::decode(&buf);
            // The raw reader must also be panic-free.
            let mut r = Reader::new(&buf);
            while let Some(f) = r.next_field() {
                if f.is_err() {
                    break;
                }
            }
        }
    }
}
