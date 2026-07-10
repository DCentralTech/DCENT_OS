// SPDX-License-Identifier: GPL-3.0-or-later
// MCP tool DEFINITIONS for the LoRa/mesh subsystem (fork plan §4.6).
//
// These are plain, serde-ready descriptors + I/O structs, ready to be registered
// into the `dcentaxe` binary's single `/mcp` JSON-RPC handler. Registering MCP
// tools costs ZERO extra URI handlers (the whole MCP server is one endpoint), so
// LoRa telemetry/control is exposed here rather than as new dashboard pages
// (root + project : prefer MCP tools / inline over new routes).
//
// ⚠️ NOT REGISTERED. This scaffold only DEFINES the tools — it does not wire
// them into the binary's MCP registry. A non-functional "LoRa" tool that the
// daemon advertised but could not service would be a lying control surface.
// Integration (registration + handler impls reading the live `Sx1262` + mesh
// peer table) is the documented follow-up (README.md).
//
// OWNER-AUTH CONTRACT (2026-06-12 hardening, mirrored from dcentaxe-bap BAP-2):
// every MUTATING tool MUST require `authorize_mcp_control()` in the binary. The
// access class below ([`McpAccess::OwnerControl`]) is the machine-checkable
// marker the registry uses to enforce that — `lora_send_beacon` is owner-control
// and MUST NOT be reachable via passwordless REST/MCP read semantics.

use serde::{Deserialize, Serialize};

/// Access class for an MCP tool. The binary's registry uses this to decide
/// whether `authorize_mcp_control()` is required before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAccess {
    /// Monitoring / read-only. No owner auth required.
    Read,
    /// Mutating / owner-control. Requires `authorize_mcp_control()`.
    OwnerControl,
}

impl McpAccess {
    pub fn requires_auth(self) -> bool {
        matches!(self, McpAccess::OwnerControl)
    }
}

/// A tool descriptor ready to register (name + human description + access class).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoraMcpTool {
    pub name: &'static str,
    pub description: &'static str,
    pub access: McpAccess,
}

impl LoraMcpTool {
    pub fn requires_auth(&self) -> bool {
        self.access.requires_auth()
    }
}

/// `lora_status` (read) — radio state, region, last beacon, RSSI/SNR of last RX,
/// mesh peer count.
pub const LORA_STATUS: LoraMcpTool = LoraMcpTool {
    name: "lora_status",
    description:
        "Read LoRa radio state, region, last beacon, last-RX RSSI/SNR, and mesh peer count.",
    access: McpAccess::Read,
};

/// `lora_send_beacon` (owner-control) — broadcast a block-found / identify /
/// custom beacon. MUTATING → owner auth required.
pub const LORA_SEND_BEACON: LoraMcpTool = LoraMcpTool {
    name: "lora_send_beacon",
    description:
        "Broadcast a block-found / identify / custom beacon over the mesh (owner-control).",
    access: McpAccess::OwnerControl,
};

/// `get_mesh_peers` (read) — discovered LoRa peers (fleet without Wi-Fi).
pub const GET_MESH_PEERS: LoraMcpTool = LoraMcpTool {
    name: "get_mesh_peers",
    description: "List LoRa mesh peers discovered over the air (fleet telemetry without Wi-Fi).",
    access: McpAccess::Read,
};

/// All LoRa MCP tools, ready for the binary to fold into its `/mcp` tool list.
pub fn tools() -> [LoraMcpTool; 3] {
    [LORA_STATUS, LORA_SEND_BEACON, GET_MESH_PEERS]
}

// ---------------------------------------------------------------------------
// Tool I/O structs (serde — the JSON-RPC request/response shapes)
// ---------------------------------------------------------------------------

/// `lora_status` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoraStatusResponse {
    /// "eu868" | "na915".
    pub region: String,
    /// Radio lifecycle: "uninitialized" | "standby" | "rx" | "tx" (honest
    /// proof-ladder state, never an optimistic default).
    pub radio_state: String,
    pub last_beacon_unix_ms: Option<u64>,
    pub last_rx_rssi_dbm: Option<i16>,
    pub last_rx_snr_db: Option<f32>,
    pub mesh_peer_count: u32,
}

/// Beacon kind for `lora_send_beacon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeaconKind {
    BlockFound,
    Identify,
    Telemetry,
    Custom,
}

/// `lora_send_beacon` request (owner-control).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendBeaconRequest {
    pub kind: BeaconKind,
    /// Optional free-text payload for `Custom`; ignored otherwise.
    #[serde(default)]
    pub message: Option<String>,
}

/// `lora_send_beacon` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendBeaconResponse {
    /// True once the frame was handed to the radio for TX (NOT delivery proof —
    /// LoRa broadcast is unacknowledged; honest proof-ladder semantics).
    pub queued: bool,
    pub frame_bytes: u16,
    /// Honest refusal reason when `queued == false` — e.g. `"duty_budget"` when the
    /// region airtime governor clamped the transmit, `"radio_unavailable"` before
    /// the radio cold-boot is proven. `None`/omitted on success. `#[serde(default)]`
    /// so a legacy caller that predates the field still round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One discovered mesh peer (harmonized with DCENT_Raven node identity fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshPeerInfo {
    /// 8-hex node id (see [`crate::mesh::NodeId`]).
    pub node_id: String,
    pub device_model: String,
    pub asic_model: String,
    pub last_seen_unix_ms: u64,
    pub rssi_dbm: i16,
    pub snr_db: f32,
}

/// `get_mesh_peers` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshPeersResponse {
    pub peers: Vec<MeshPeerInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_tools_with_expected_names() {
        let t = tools();
        let names: Vec<&str> = t.iter().map(|x| x.name).collect();
        assert_eq!(names, ["lora_status", "lora_send_beacon", "get_mesh_peers"]);
    }

    #[test]
    fn only_send_beacon_requires_owner_auth() {
        // The single mutating tool is owner-control; the two reads are not.
        assert!(LORA_SEND_BEACON.requires_auth());
        assert_eq!(LORA_SEND_BEACON.access, McpAccess::OwnerControl);
        assert!(!LORA_STATUS.requires_auth());
        assert!(!GET_MESH_PEERS.requires_auth());

        // Exactly one mutating tool — guards against accidentally shipping a
        // passwordless mutate (the BAP-2 / 2026-06-12 contract).
        let mutating = tools().iter().filter(|t| t.requires_auth()).count();
        assert_eq!(mutating, 1);
    }

    #[test]
    fn tool_io_structs_serialize() {
        // Prove the descriptors + I/O are JSON-ready for the `/mcp` registry.
        let req = SendBeaconRequest {
            kind: BeaconKind::BlockFound,
            message: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("block_found"));

        let access = serde_json::to_string(&McpAccess::OwnerControl).unwrap();
        assert_eq!(access, "\"owner_control\"");
    }
}
