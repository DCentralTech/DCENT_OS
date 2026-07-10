// SPDX-License-Identifier: GPL-3.0-or-later
// The lightweight "DCENT mesh" broadcast protocol (fork plan §4.4 option A —
// RECOMMENDED for v1). Small addressed frames carried over the SX1262: a
// block-found beacon, a telemetry beacon, fleet identify/pairing, and an
// owner-authed control frame.
//
// ONE GRAMMAR across the product line: the frame reuses the dcentaxe-bap
// NMEA-style `$…,<TOK>,…*XX\r\n` shape (XOR checksum over the body) so the
// dashboard, MCP, BAP and LoRa all speak the same vocabulary. BAP frames are
// `$BAP,…`; mesh frames are `$DCM,…` (DCENT Mesh). The checksum scheme is
// byte-identical to `dcentaxe_bap::protocol::nmea_xor`.
//
// Telemetry/identify field names are harmonized with DCENT_Raven's `MinerState`
// (projects/dcent-raven/firmware/modules/dcent_raven/MinerState.h) so a Raven
// mesh node and a DCENT_axe board agree on what a "block found" / telemetry
// packet means: hashrate, chip_temp, power, shares acc/rej, best_diff,
// block_height, device_model, asic_model. The block-found beacon maps directly
// to Raven's `found_block` rising edge → high-priority mesh broadcast.
//
// Phase-2 Meshtastic-compatible framing (option B in the fork plan) is a
// first-class, feature-gated module at the crate root: see [`crate::meshtastic`].

use crate::LoraError;
use serde::{Deserialize, Serialize};

/// Frame prefix (cf. BAP's `$BAP`).
pub const MESH_PREFIX: &str = "$DCM";

/// Max body length (`DCM,…` without `*XX\r\n`). LoRa PHY tops out ~255 bytes;
/// stay well under so a frame fits one un-fragmented packet (matches BAP's 240).
pub const MAX_MESH_PAYLOAD: usize = 240;

/// Default hop budget (TTL) a freshly-originated frame carries. Each relay
/// decrements it; a frame reaching 0 is not re-transmitted. 3 hops covers a
/// home/site swarm without letting a packet circulate the mesh indefinitely.
pub const DEFAULT_TTL: u8 = 3;

/// Hard ceiling on TTL so a hostile or garbled frame cannot request unbounded
/// relay. Enforced on the wire by [`MeshFrame::encode`] and on decode.
pub const MAX_TTL: u8 = 7;

/// XOR of every byte — the SAME NMEA-0183 scheme dcentaxe-bap uses. Kept local
/// so this crate carries no dependency on the BAP crate, but the algorithm is
/// identical (one grammar, one checksum).
pub fn xor_checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc ^ b)
}

/// A mesh node identity — Meshtastic-style 32-bit id (on DCENT_axe, derived from
/// the low bytes of the Wi-Fi/efuse MAC). Rendered as 8 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn to_hex(self) -> String {
        format!("{:08x}", self.0)
    }
    pub fn from_hex(s: &str) -> Option<Self> {
        u32::from_str_radix(s, 16).ok().map(NodeId)
    }
}

// ---------------------------------------------------------------------------
// Payloads (serde-ready so they can feed the dashboard / MCP `get_mesh_peers`)
// ---------------------------------------------------------------------------

/// Periodic telemetry beacon. Field names mirror DCENT_Raven `MinerState`.
/// Emit INFREQUENTLY (region duty-cycle bound) — never at a fast cadence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub hashrate_ghs: f64,
    pub chip_temp_c: f64,
    pub power_w: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    /// Pre-formatted best difficulty (e.g. "1.05k"), like Raven's `best_diff`.
    pub best_diff: String,
    pub block_height: i64,
}

/// Block-found beacon — the headline "sovereign miner announces a block over
/// mesh with no internet" event. Maps to Raven `found_block` edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockFound {
    pub block_height: i64,
    /// Best difficulty achieved on the winning share.
    pub best_diff: String,
}

/// Fleet identify / pairing beacon — sets the mesh node's display name (cf.
/// Raven mapping `deviceModel`/`asicModel` → node long-name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identify {
    pub device_model: String,
    pub asic_model: String,
}

/// Owner-authed control frame received over the air. Reuses the BAP CMD/PARAM/
/// VALUE vocabulary. SAFETY: a control frame from the air MUST pass the same
/// owner auth as the MCP owner-control path — never accept a passwordless
/// voltage/fan/pool mutate (fork plan §4.4.5 / §6, and the BAP-2 contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshCommand {
    /// e.g. "set" / "cmd" (mirrors BAP SET / CMD verbs).
    pub verb: String,
    /// e.g. "frequency", "fan_speed", "restart_mining".
    pub param: String,
    pub value: String,
    /// Owner-auth tag (shared-secret HMAC / pairing token). `None` ⇒ unauthed ⇒
    /// the frame MUST be refused before it reaches any hardware write.
    pub auth: Option<String>,
}

impl MeshCommand {
    /// Air-gap control is always owner-gated.
    pub fn requires_auth(&self) -> bool {
        true
    }

    /// Verify the carried owner tag against the configured 256-bit owner `key`,
    /// in constant time. The tag is an HMAC-SHA256 over a message binding the
    /// source node, the frame sequence number, and this verb/param/value, so it
    /// cannot be replayed onto another node/seq or tampered field-by-field.
    ///
    /// Returns [`LoraError::Unauthorized`] when no key is configured, the tag is
    /// absent/malformed, or the MAC does not verify. This is the AUTHENTICITY
    /// gate only — the caller must also pass the frame through a stateful
    /// [`ReplayGuard`](crate::auth::ReplayGuard) (or use
    /// [`MeshAuthenticator`](crate::auth::MeshAuthenticator), which does both)
    /// before routing the command through the host operating-point clamp.
    pub fn authorize(&self, key: Option<&[u8]>, src: NodeId, seq: u8) -> Result<(), LoraError> {
        crate::auth::verify_command_mac(self, key, src, seq)
    }
}

/// The frame body kinds. Token strings are the wire tags after `$DCM,`.
#[derive(Debug, Clone, PartialEq)]
pub enum MeshKind {
    Telemetry(Telemetry),
    BlockFound(BlockFound),
    Identify(Identify),
    Command(MeshCommand),
    /// Delivery acknowledgement (the param echoes what is being acked).
    Ack(String),
}

impl MeshKind {
    pub fn token(&self) -> &'static str {
        match self {
            MeshKind::Telemetry(_) => "TLM",
            MeshKind::BlockFound(_) => "BLK",
            MeshKind::Identify(_) => "IDN",
            MeshKind::Command(_) => "CMD",
            MeshKind::Ack(_) => "ACK",
        }
    }
}

/// A complete addressed mesh frame: source node + sequence/hop metadata + typed
/// body.
///
/// `seq` is a per-source counter set by the ORIGINATOR; `(src, seq)` is the
/// dedup key a relay uses to suppress an already-forwarded frame and the nonce an
/// owner command is authenticated against. `ttl` is the remaining hop budget,
/// decremented on each relay ([`next_hop`](Self::next_hop)); a frame at ttl 0/1
/// is never re-transmitted.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshFrame {
    pub src: NodeId,
    /// Per-source sequence number (wraps at 256). Dedup + anti-replay key.
    pub seq: u8,
    /// Remaining hop budget; clamped to [`MAX_TTL`] on the wire.
    pub ttl: u8,
    pub kind: MeshKind,
}

impl MeshFrame {
    /// A frame from `src` with sequence 0 and the default hop budget —
    /// convenience for tests and single-shot local beacons. Use
    /// [`originate`](Self::originate) to carry a real per-node sequence number.
    pub fn new(src: NodeId, kind: MeshKind) -> Self {
        Self {
            src,
            seq: 0,
            ttl: DEFAULT_TTL,
            kind,
        }
    }

    /// A freshly-originated frame carrying an explicit sequence number and the
    /// default hop budget.
    pub fn originate(src: NodeId, seq: u8, kind: MeshKind) -> Self {
        Self {
            src,
            seq,
            ttl: DEFAULT_TTL,
            kind,
        }
    }

    /// The relayed copy of this frame with one hop consumed, or `None` when the
    /// hop budget is exhausted (`ttl <= 1`) and it must not be re-transmitted.
    /// `src`/`seq`/body are preserved verbatim — a relay forwards, it never
    /// re-originates (so dedup and owner-auth still key on the original node).
    pub fn next_hop(&self) -> Option<MeshFrame> {
        if self.ttl <= 1 {
            return None;
        }
        Some(MeshFrame {
            src: self.src,
            seq: self.seq,
            ttl: self.ttl - 1,
            kind: self.kind.clone(),
        })
    }

    /// Encode to the wire: `$DCM,<TOK>,<src-hex8>,<seq-hex2>,<ttl-hex>,<fields…>*XX\r\n`.
    /// `ttl` is clamped to [`MAX_TTL`] on the wire. Returns [`LoraError::Protocol`]
    /// if the body would exceed [`MAX_MESH_PAYLOAD`].
    pub fn encode(&self) -> Result<Vec<u8>, LoraError> {
        let fields = self.encode_fields();
        let body = format!(
            "DCM,{},{},{:02x},{:x},{}",
            self.kind.token(),
            self.src.to_hex(),
            self.seq,
            self.ttl.min(MAX_TTL),
            fields
        );
        if body.len() > MAX_MESH_PAYLOAD {
            return Err(LoraError::Protocol(format!(
                "mesh body {} > {} bytes",
                body.len(),
                MAX_MESH_PAYLOAD
            )));
        }
        let cks = xor_checksum(body.as_bytes());
        Ok(format!("${body}*{cks:02X}\r\n").into_bytes())
    }

    fn encode_fields(&self) -> String {
        match &self.kind {
            MeshKind::Telemetry(t) => format!(
                "{:.2},{:.1},{:.2},{},{},{},{}",
                t.hashrate_ghs,
                t.chip_temp_c,
                t.power_w,
                t.shares_accepted,
                t.shares_rejected,
                escape(&t.best_diff),
                t.block_height,
            ),
            MeshKind::BlockFound(b) => format!("{},{}", b.block_height, escape(&b.best_diff)),
            MeshKind::Identify(i) => {
                format!("{},{}", escape(&i.device_model), escape(&i.asic_model))
            }
            MeshKind::Command(c) => format!(
                "{},{},{},{}",
                escape(&c.verb),
                escape(&c.param),
                escape(&c.value),
                c.auth.as_deref().map(escape).unwrap_or_default(),
            ),
            MeshKind::Ack(p) => escape(p),
        }
    }

    /// Decode one frame from the wire. Verifies the prefix, terminator, and XOR
    /// checksum, then parses the typed body.
    pub fn decode(raw: &[u8]) -> Result<MeshFrame, LoraError> {
        let s = std::str::from_utf8(raw).map_err(|_| LoraError::Protocol("non-utf8".into()))?;
        let s = s.strip_suffix("\r\n").unwrap_or(s);
        let s = s
            .strip_prefix('$')
            .ok_or_else(|| LoraError::Protocol("missing $".into()))?;
        let (body, cks_hex) = s
            .rsplit_once('*')
            .ok_or_else(|| LoraError::Protocol("missing *checksum".into()))?;
        let expected = u8::from_str_radix(cks_hex, 16)
            .map_err(|_| LoraError::Protocol("bad checksum hex".into()))?;
        if xor_checksum(body.as_bytes()) != expected {
            return Err(LoraError::Protocol("checksum mismatch".into()));
        }
        // body = "DCM,<TOK>,<src>,<seq>,<ttl>,<fields…>"
        let mut it = body.splitn(6, ',');
        if it.next() != Some("DCM") {
            return Err(LoraError::Protocol("bad prefix tag".into()));
        }
        let tok = it
            .next()
            .ok_or_else(|| LoraError::Protocol("no token".into()))?;
        let src_hex = it
            .next()
            .ok_or_else(|| LoraError::Protocol("no src".into()))?;
        let seq_hex = it
            .next()
            .ok_or_else(|| LoraError::Protocol("no seq".into()))?;
        let ttl_hex = it
            .next()
            .ok_or_else(|| LoraError::Protocol("no ttl".into()))?;
        let src = NodeId::from_hex(src_hex).ok_or_else(|| LoraError::Protocol("bad src".into()))?;
        let seq =
            u8::from_str_radix(seq_hex, 16).map_err(|_| LoraError::Protocol("bad seq".into()))?;
        let ttl = u8::from_str_radix(ttl_hex, 16)
            .map_err(|_| LoraError::Protocol("bad ttl".into()))?
            .min(MAX_TTL);
        let fields = it.next().unwrap_or("");
        let kind = decode_kind(tok, fields)?;
        Ok(MeshFrame {
            src,
            seq,
            ttl,
            kind,
        })
    }
}

fn decode_kind(tok: &str, fields: &str) -> Result<MeshKind, LoraError> {
    let parts: Vec<&str> = fields.split(',').collect();
    let bad = || LoraError::Protocol(format!("malformed {tok} fields"));
    match tok {
        "TLM" => {
            if parts.len() != 7 {
                return Err(bad());
            }
            Ok(MeshKind::Telemetry(Telemetry {
                hashrate_ghs: parts[0].parse().map_err(|_| bad())?,
                chip_temp_c: parts[1].parse().map_err(|_| bad())?,
                power_w: parts[2].parse().map_err(|_| bad())?,
                shares_accepted: parts[3].parse().map_err(|_| bad())?,
                shares_rejected: parts[4].parse().map_err(|_| bad())?,
                best_diff: unescape(parts[5]),
                block_height: parts[6].parse().map_err(|_| bad())?,
            }))
        }
        "BLK" => {
            if parts.len() != 2 {
                return Err(bad());
            }
            Ok(MeshKind::BlockFound(BlockFound {
                block_height: parts[0].parse().map_err(|_| bad())?,
                best_diff: unescape(parts[1]),
            }))
        }
        "IDN" => {
            if parts.len() != 2 {
                return Err(bad());
            }
            Ok(MeshKind::Identify(Identify {
                device_model: unescape(parts[0]),
                asic_model: unescape(parts[1]),
            }))
        }
        "CMD" => {
            if parts.len() != 4 {
                return Err(bad());
            }
            let auth = if parts[3].is_empty() {
                None
            } else {
                Some(unescape(parts[3]))
            };
            Ok(MeshKind::Command(MeshCommand {
                verb: unescape(parts[0]),
                param: unescape(parts[1]),
                value: unescape(parts[2]),
                auth,
            }))
        }
        "ACK" => Ok(MeshKind::Ack(unescape(fields))),
        other => Err(LoraError::Protocol(format!("unknown mesh token {other}"))),
    }
}

/// Field escaping so a `,` / `*` / `\` inside a string field cannot corrupt the
/// frame grammar. Backslash-escape the four reserved bytes.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\c"),
            '*' => out.push_str("\\a"),
            '\r' | '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('c') => out.push(','),
                Some('a') => out.push('*'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Mesh peer table (backs the future `get_mesh_peers` MCP tool)
// ---------------------------------------------------------------------------

/// Default capacity of a [`PeerTable`]. Bounded so the table costs a fixed,
/// predictable amount of RAM on the cheap entry board (no unbounded growth from
/// air traffic). 32 nodes comfortably covers a home/fleet swarm.
pub const MAX_PEERS: usize = 32;

/// One tracked mesh peer, refreshed on every frame observed from that node.
///
/// `last_seen` is a CALLER-supplied monotonic tick (e.g. unix-ms or uptime-ms);
/// the table never reads a wall clock itself, keeping it pure/host-testable and
/// `no_std`-friendly.
#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    pub node_id: NodeId,
    /// Caller-supplied tick at the most recent observation.
    pub last_seen: u64,
    /// RSSI (dBm) of the last received frame from this node.
    pub rssi_dbm: i16,
    /// SNR (dB) of the last received frame from this node.
    pub snr_db: i8,
    /// Remaining hop budget (TTL) of the LAST frame observed from this node.
    pub last_ttl: u8,
    /// Highest TTL ever observed from this node — the least-relayed (shortest-
    /// path) sighting, from which [`hop_distance`](Peer::hop_distance) is derived.
    pub best_ttl: u8,
    /// Wire token of the last frame seen ("TLM"/"BLK"/"IDN"/"CMD"/"ACK").
    pub last_kind: &'static str,
    /// Self-reported model strings — only an [`Identify`] frame carries these,
    /// so they stay `None` until the peer announces and are NOT cleared by a
    /// later non-Identify frame.
    pub device_model: Option<String>,
    pub asic_model: Option<String>,
}

impl Peer {
    /// Estimated number of relay hops to this node on its shortest observed path.
    /// `0` == a direct (1-hop) neighbour we heard un-relayed. Derived as
    /// `DEFAULT_TTL - best_ttl`, assuming standard origination at [`DEFAULT_TTL`]
    /// (a frame minted at a higher TTL only ever under-estimates, never crashes).
    pub fn hop_distance(&self) -> u8 {
        DEFAULT_TTL.saturating_sub(self.best_ttl)
    }

    /// `true` when this peer is a direct neighbour (heard without any relay).
    pub fn is_direct(&self) -> bool {
        self.hop_distance() == 0
    }
}

/// A compact snapshot of mesh topology + link health, for the dashboard/MCP
/// "mesh health" surface — the manageable, defensible-network view. All fields
/// are derived from the [`PeerTable`]; no clock or radio is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshHealth {
    /// Total peers currently tracked.
    pub total_peers: usize,
    /// Peers reachable directly (0-hop neighbours).
    pub direct_neighbors: usize,
    /// Peers reached only via ≥ 1 relay.
    pub multi_hop: usize,
    /// Farthest known node's hop distance (0 when the mesh is empty).
    pub max_hop: u8,
    /// Strongest direct-neighbour RSSI (dBm), if any direct neighbour is known.
    pub best_direct_rssi_dbm: Option<i16>,
    /// Weakest tracked-link SNR (dB), if any peer is known — a coverage-edge hint.
    pub worst_snr_db: Option<i8>,
}

/// A fixed-capacity table of mesh peers discovered over the air, keyed by
/// [`NodeId`]. Backs the future `get_mesh_peers` MCP tool ([`crate::mcp`]).
///
/// When full, inserting a new node evicts the least-recently-seen peer (LRU), so
/// an active swarm stays represented and stale strangers age out. The table is
/// clock-free: the caller passes the `now` tick into [`observe`](Self::observe)
/// and [`expire`](Self::expire).
#[derive(Debug, Clone)]
pub struct PeerTable {
    peers: Vec<Peer>,
    capacity: usize,
}

impl Default for PeerTable {
    fn default() -> Self {
        Self::with_capacity(MAX_PEERS)
    }
}

impl PeerTable {
    /// A table holding up to [`MAX_PEERS`] peers.
    pub fn new() -> Self {
        Self::default()
    }

    /// A table with an explicit capacity (clamped to ≥ 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            peers: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Maximum number of peers retained before LRU eviction kicks in.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of peers currently tracked.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// `true` when no peers are tracked.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Look up a peer by id.
    pub fn get(&self, id: NodeId) -> Option<&Peer> {
        self.peers.iter().find(|p| p.node_id == id)
    }

    /// Record one observed frame: update the matching peer in place (RSSI/SNR/
    /// last-seen/last-kind), or insert a new peer — evicting the least-recently-
    /// seen one first when the table is at capacity. An [`Identify`] frame also
    /// refreshes the peer's model strings; other frame kinds never clear them.
    /// Returns the observed [`NodeId`].
    pub fn observe(&mut self, frame: &MeshFrame, rssi_dbm: i16, snr_db: i8, now: u64) -> NodeId {
        let id = frame.src;
        let ttl = frame.ttl;
        let kind = frame.kind.token();
        let (dev, asic) = match &frame.kind {
            MeshKind::Identify(i) => (Some(i.device_model.clone()), Some(i.asic_model.clone())),
            _ => (None, None),
        };

        if let Some(p) = self.peers.iter_mut().find(|p| p.node_id == id) {
            p.last_seen = now;
            p.rssi_dbm = rssi_dbm;
            p.snr_db = snr_db;
            p.last_ttl = ttl;
            if ttl > p.best_ttl {
                p.best_ttl = ttl;
            }
            p.last_kind = kind;
            if dev.is_some() {
                p.device_model = dev;
            }
            if asic.is_some() {
                p.asic_model = asic;
            }
            return id;
        }

        // New node. Evict the least-recently-seen peer if we are at capacity.
        if self.peers.len() >= self.capacity {
            if let Some((idx, _)) = self
                .peers
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.last_seen)
            {
                self.peers.remove(idx);
            }
        }
        self.peers.push(Peer {
            node_id: id,
            last_seen: now,
            rssi_dbm,
            snr_db,
            last_ttl: ttl,
            best_ttl: ttl,
            last_kind: kind,
            device_model: dev,
            asic_model: asic,
        });
        id
    }

    /// A compact [`MeshHealth`] snapshot of topology + link quality for the
    /// dashboard/MCP mesh-health surface. O(n) over the tracked peers.
    pub fn health(&self) -> MeshHealth {
        let mut h = MeshHealth {
            total_peers: self.peers.len(),
            direct_neighbors: 0,
            multi_hop: 0,
            max_hop: 0,
            best_direct_rssi_dbm: None,
            worst_snr_db: None,
        };
        for p in &self.peers {
            let hop = p.hop_distance();
            if hop == 0 {
                h.direct_neighbors += 1;
                h.best_direct_rssi_dbm = Some(match h.best_direct_rssi_dbm {
                    Some(cur) => cur.max(p.rssi_dbm),
                    None => p.rssi_dbm,
                });
            } else {
                h.multi_hop += 1;
            }
            h.max_hop = h.max_hop.max(hop);
            h.worst_snr_db = Some(match h.worst_snr_db {
                Some(cur) => cur.min(p.snr_db),
                None => p.snr_db,
            });
        }
        h
    }

    /// Iterate peers in internal (insertion) order.
    pub fn peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter()
    }

    /// Peers ordered most-recently-seen first (freshest at index 0) — the order
    /// the `get_mesh_peers` MCP tool should present to a caller.
    pub fn peers_by_recency(&self) -> Vec<&Peer> {
        let mut v: Vec<&Peer> = self.peers.iter().collect();
        v.sort_by_key(|p| core::cmp::Reverse(p.last_seen));
        v
    }

    /// Drop peers not seen within `ttl` ticks of `now` (a peer is stale when
    /// `now - last_seen > ttl`). Clock skew where `last_seen > now` is treated as
    /// age 0 (kept). Returns how many peers were expired.
    pub fn expire(&mut self, now: u64, ttl: u64) -> usize {
        let before = self.peers.len();
        self.peers
            .retain(|p| now.saturating_sub(p.last_seen) <= ttl);
        before - self.peers.len()
    }
}

// Phase-2 Meshtastic-compatible interop (fork plan §4.4 option B) is now a
// first-class module at the crate root — `crate::meshtastic` — no longer a stub
// nested here. It reuses this module's [`Identify`]/[`Telemetry`] via the bridge
// in `crate::meshtastic::{user_from_identify, ...}`.

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: NodeId = NodeId(0xdeadbeef);

    fn round_trip(frame: &MeshFrame) -> MeshFrame {
        let bytes = frame.encode().expect("encode");
        // Frame must terminate and start correctly.
        assert!(bytes.starts_with(b"$DCM,"));
        assert!(bytes.ends_with(b"\r\n"));
        MeshFrame::decode(&bytes).expect("decode")
    }

    #[test]
    fn telemetry_round_trip() {
        let frame = MeshFrame::new(
            NODE,
            MeshKind::Telemetry(Telemetry {
                hashrate_ghs: 123.45,
                chip_temp_c: 56.7,
                power_w: 14.20,
                shares_accepted: 42,
                shares_rejected: 1,
                best_diff: "1.05k".into(),
                block_height: 901_234,
            }),
        );
        let back = round_trip(&frame);
        assert_eq!(back, frame);
    }

    #[test]
    fn block_found_round_trip() {
        let frame = MeshFrame::new(
            NODE,
            MeshKind::BlockFound(BlockFound {
                block_height: 901_234,
                best_diff: "21.3M".into(),
            }),
        );
        assert_eq!(round_trip(&frame), frame);
    }

    #[test]
    fn identify_round_trip() {
        let frame = MeshFrame::new(
            NODE,
            MeshKind::Identify(Identify {
                device_model: "DCENT_axe Hex BM1397".into(),
                asic_model: "BM1397".into(),
            }),
        );
        assert_eq!(round_trip(&frame), frame);
    }

    #[test]
    fn command_round_trip_and_reserved_chars_survive() {
        // A value containing a comma + star must survive the grammar via escaping.
        let frame = MeshFrame::new(
            NODE,
            MeshKind::Command(MeshCommand {
                verb: "set".into(),
                param: "pool".into(),
                value: "stratum+tcp://a,b:3333".into(),
                auth: Some("owner-tok-123".into()),
            }),
        );
        let back = round_trip(&frame);
        assert_eq!(back, frame);
    }

    #[test]
    fn command_owner_auth_gate() {
        use crate::auth::{command_mac, tag_to_hex};
        let key = [0x11u8; 32];
        let seq = 7u8;
        // A correctly-signed command verifies only against the right key+src+seq.
        let authed = MeshCommand {
            verb: "set".into(),
            param: "frequency".into(),
            value: "525".into(),
            auth: Some(tag_to_hex(&command_mac(
                &key,
                NODE,
                seq,
                "set",
                "frequency",
                "525",
            ))),
        };
        assert!(authed.requires_auth());
        assert!(authed.authorize(Some(&key), NODE, seq).is_ok());
        // Wrong key / wrong src / wrong seq all fail.
        assert_eq!(
            authed.authorize(Some(&[0x22u8; 32]), NODE, seq),
            Err(LoraError::Unauthorized)
        );
        assert_eq!(
            authed.authorize(Some(&key), NodeId(0x1234_5678), seq),
            Err(LoraError::Unauthorized)
        );
        assert_eq!(
            authed.authorize(Some(&key), NODE, seq.wrapping_add(1)),
            Err(LoraError::Unauthorized)
        );

        // An UNauthed command is always refused (the air-gap-bypass guard).
        let unauthed = MeshCommand {
            verb: "set".into(),
            param: "asic_voltage".into(),
            value: "1300".into(),
            auth: None,
        };
        assert_eq!(
            unauthed.authorize(Some(&key), NODE, seq),
            Err(LoraError::Unauthorized)
        );
        // …and no configured owner key never authorizes anything.
        assert_eq!(
            authed.authorize(None, NODE, seq),
            Err(LoraError::Unauthorized)
        );
    }

    #[test]
    fn seq_and_ttl_round_trip() {
        let frame = MeshFrame::originate(NODE, 0x2a, MeshKind::Ack("x".into()));
        assert_eq!(frame.seq, 0x2a);
        assert_eq!(frame.ttl, DEFAULT_TTL);
        let back = round_trip(&frame);
        assert_eq!((back.seq, back.ttl), (0x2a, DEFAULT_TTL));
        assert_eq!(back, frame);
    }

    #[test]
    fn ack_wire_format_is_pinned() {
        // Pins the field ORDER so an accidental reorder of the grammar is loud.
        let frame = MeshFrame::originate(NodeId(0xdead_beef), 0x05, MeshKind::Ack("ok".into()));
        let bytes = frame.encode().unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            s.starts_with("$DCM,ACK,deadbeef,05,3,ok*"),
            "grammar drift: got {s:?}"
        );
        assert!(s.ends_with("\r\n"));
        assert_eq!(MeshFrame::decode(&bytes).unwrap(), frame);
    }

    #[test]
    fn next_hop_decrements_ttl_and_preserves_src_seq() {
        let f = MeshFrame {
            src: NODE,
            seq: 9,
            ttl: 3,
            kind: MeshKind::Ack("y".into()),
        };
        let h = f.next_hop().expect("ttl 3 relays");
        assert_eq!((h.src, h.seq, h.ttl), (NODE, 9, 2));
        let h2 = h.next_hop().expect("ttl 2 relays");
        assert_eq!(h2.ttl, 1);
        assert!(h2.next_hop().is_none(), "ttl 1 is the last hop");
        // A ttl-0 frame decodes fine but is never relayed.
        let dead = MeshFrame {
            src: NODE,
            seq: 9,
            ttl: 0,
            kind: MeshKind::Ack("z".into()),
        };
        assert!(dead.next_hop().is_none());
    }

    #[test]
    fn ttl_clamped_to_max_on_wire() {
        let f = MeshFrame {
            src: NODE,
            seq: 1,
            ttl: 200,
            kind: MeshKind::Ack("q".into()),
        };
        let back = MeshFrame::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(back.ttl, MAX_TTL);
    }

    #[test]
    fn corrupt_checksum_rejected() {
        let frame = MeshFrame::new(NODE, MeshKind::Ack("hello".into()));
        let mut bytes = frame.encode().unwrap();
        // Flip a body byte so the checksum no longer matches.
        let pos = bytes.iter().position(|&b| b == b'h').unwrap();
        bytes[pos] = b'H';
        assert!(matches!(
            MeshFrame::decode(&bytes),
            Err(LoraError::Protocol(_))
        ));
    }

    #[test]
    fn oversize_body_rejected() {
        let frame = MeshFrame::new(
            NODE,
            MeshKind::Identify(Identify {
                device_model: "X".repeat(MAX_MESH_PAYLOAD),
                asic_model: "Y".into(),
            }),
        );
        assert!(matches!(frame.encode(), Err(LoraError::Protocol(_))));
    }

    // ---- PeerTable ----

    fn ack(id: u32) -> MeshFrame {
        MeshFrame::new(NodeId(id), MeshKind::Ack("x".into()))
    }

    fn ack_ttl(id: u32, ttl: u8) -> MeshFrame {
        MeshFrame {
            src: NodeId(id),
            seq: 0,
            ttl,
            kind: MeshKind::Ack("x".into()),
        }
    }

    #[test]
    fn peer_hop_distance_from_observed_ttl() {
        let mut t = PeerTable::new();
        // Direct neighbour: heard at the full DEFAULT_TTL → 0 hops.
        t.observe(&ack_ttl(0xA, DEFAULT_TTL), -40, 8, 10);
        assert_eq!(t.get(NodeId(0xA)).unwrap().hop_distance(), 0);
        assert!(t.get(NodeId(0xA)).unwrap().is_direct());
        // Two relays away: heard at DEFAULT_TTL - 2.
        t.observe(&ack_ttl(0xB, DEFAULT_TTL - 2), -90, 1, 10);
        assert_eq!(t.get(NodeId(0xB)).unwrap().hop_distance(), 2);
        assert!(!t.get(NodeId(0xB)).unwrap().is_direct());
        // best_ttl keeps the SHORTEST-path sighting: a later, more-relayed copy
        // (lower ttl) must NOT increase the hop estimate.
        t.observe(&ack_ttl(0xA, DEFAULT_TTL - 1), -70, 3, 20);
        assert_eq!(
            t.get(NodeId(0xA)).unwrap().hop_distance(),
            0,
            "a farther sighting never worsens a known-direct peer"
        );
        assert_eq!(t.get(NodeId(0xA)).unwrap().last_ttl, DEFAULT_TTL - 1);
    }

    #[test]
    fn peer_table_health_summarizes_topology() {
        let mut t = PeerTable::new();
        assert_eq!(t.health().total_peers, 0);
        assert_eq!(t.health().max_hop, 0);

        t.observe(&ack_ttl(0xA, DEFAULT_TTL), -40, 8, 10); // direct, strong
        t.observe(&ack_ttl(0xB, DEFAULT_TTL), -55, 5, 10); // direct, weaker RSSI
        t.observe(&ack_ttl(0xC, DEFAULT_TTL - 2), -95, -3, 10); // 2 hops, worst SNR

        let h = t.health();
        assert_eq!(h.total_peers, 3);
        assert_eq!(h.direct_neighbors, 2);
        assert_eq!(h.multi_hop, 1);
        assert_eq!(h.max_hop, 2);
        assert_eq!(h.best_direct_rssi_dbm, Some(-40), "strongest direct link");
        assert_eq!(h.worst_snr_db, Some(-3), "coverage-edge SNR");
    }

    #[test]
    fn peer_table_inserts_then_updates_in_place() {
        let mut t = PeerTable::new();
        let n = NodeId(0x1111_1111);
        t.observe(&ack(0x1111_1111), -50, 7, 100);
        assert_eq!(t.len(), 1);
        let p = t.get(n).unwrap();
        assert_eq!(
            (p.rssi_dbm, p.snr_db, p.last_seen, p.last_kind),
            (-50, 7, 100, "ACK")
        );
        assert_eq!(p.device_model, None);

        // Same node again → in-place update, not a second row.
        t.observe(&ack(0x1111_1111), -60, 5, 200);
        assert_eq!(t.len(), 1);
        let p = t.get(n).unwrap();
        assert_eq!((p.rssi_dbm, p.snr_db, p.last_seen), (-60, 5, 200));
    }

    #[test]
    fn peer_table_identify_sets_models_and_telemetry_keeps_them() {
        let mut t = PeerTable::new();
        let n = NodeId(0xAABB_CCDD);
        let idn = MeshFrame::new(
            n,
            MeshKind::Identify(Identify {
                device_model: "DCENT_axe Hex BM1397".into(),
                asic_model: "BM1397".into(),
            }),
        );
        t.observe(&idn, -40, 9, 10);
        let p = t.get(n).unwrap();
        assert_eq!(p.device_model.as_deref(), Some("DCENT_axe Hex BM1397"));
        assert_eq!(p.asic_model.as_deref(), Some("BM1397"));
        assert_eq!(p.last_kind, "IDN");

        // A later telemetry frame updates link quality but MUST NOT erase the
        // learned model strings.
        let tlm = MeshFrame::new(
            n,
            MeshKind::Telemetry(Telemetry {
                hashrate_ghs: 500.0,
                chip_temp_c: 55.0,
                power_w: 15.0,
                shares_accepted: 1,
                shares_rejected: 0,
                best_diff: "1k".into(),
                block_height: 1,
            }),
        );
        t.observe(&tlm, -45, 6, 20);
        let p = t.get(n).unwrap();
        assert_eq!(p.device_model.as_deref(), Some("DCENT_axe Hex BM1397"));
        assert_eq!(p.last_kind, "TLM");
        assert_eq!(p.last_seen, 20);
    }

    #[test]
    fn peer_table_evicts_least_recently_seen_when_full() {
        let mut t = PeerTable::with_capacity(2);
        t.observe(&ack(0xA), -10, 1, 100); // A @100
        t.observe(&ack(0xB), -10, 1, 200); // B @200
        t.observe(&ack(0xA), -10, 1, 300); // refresh A → B is now the LRU
        t.observe(&ack(0xC), -10, 1, 400); // full → evict LRU (B @200)
        assert_eq!(t.len(), 2);
        assert!(
            t.get(NodeId(0xB)).is_none(),
            "least-recently-seen peer evicted"
        );
        assert!(t.get(NodeId(0xA)).is_some());
        assert!(t.get(NodeId(0xC)).is_some());
    }

    #[test]
    fn peer_table_recency_ordering_is_freshest_first() {
        let mut t = PeerTable::new();
        t.observe(&ack(1), -10, 1, 100);
        t.observe(&ack(2), -10, 1, 300);
        t.observe(&ack(3), -10, 1, 200);
        let order: Vec<u32> = t.peers_by_recency().iter().map(|p| p.node_id.0).collect();
        assert_eq!(order, vec![2, 3, 1]);
        // peers() stays in insertion order.
        let ins: Vec<u32> = t.peers().map(|p| p.node_id.0).collect();
        assert_eq!(ins, vec![1, 2, 3]);
    }

    #[test]
    fn peer_table_expire_drops_only_stale() {
        let mut t = PeerTable::new();
        t.observe(&ack(1), -10, 1, 100); // age @2000 == 1900
        t.observe(&ack(2), -10, 1, 1000); // age @2000 == 1000
        let removed = t.expire(2000, 1500);
        assert_eq!(removed, 1);
        assert!(t.get(NodeId(1)).is_none(), "stale peer expired");
        assert!(t.get(NodeId(2)).is_some(), "fresh peer kept");
    }

    // ---- Untrusted over-the-air bytes must never panic the decoder ----

    #[test]
    fn fuzz_decode_never_panics_on_arbitrary_bytes() {
        // `MeshFrame::decode` parses UNTRUSTED radio bytes. Arbitrary/hostile
        // input must return Ok/Err, never panic — no OOB index, no integer
        // overflow, no unwrap on attacker-controlled data, and a trailing-
        // backslash escape must not run `unescape` off the end. This pins the
        // no-panic guarantee across every `decode_kind` branch plus the
        // escape/unescape and checksum-hex paths.
        let mut state: u32 = 0x9E37_79B9;
        let mut next = || {
            // xorshift32 — deterministic, no external rng dependency.
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        // Alphabet biased toward the mesh grammar ($ D C M , * CR LF, the
        // escape bytes \ c a, the tokens, hex + punctuation) plus uniform noise.
        let alphabet = b"$DCM,*\r\n\\caTLMBLKIDNCMDACK0123456789abcdef.-+ ";
        for _ in 0..6000 {
            let len = (next() % 300) as usize;
            let mut buf: Vec<u8> = Vec::with_capacity(len);
            for _ in 0..len {
                let r = next();
                if r % 3 != 0 {
                    buf.push(alphabet[(r >> 2) as usize % alphabet.len()]);
                } else {
                    buf.push((r >> 5) as u8);
                }
            }
            // Any Result is acceptable; the only contract is "must not panic".
            let _ = MeshFrame::decode(&buf);
        }
    }
}
