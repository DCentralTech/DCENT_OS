use serde::{Deserialize, Serialize};

pub const SWARM_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SwarmRole {
    Standalone,
    Worker,
    Queen,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HomeControlMode {
    Manual,
    Thermal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum SwarmSource {
    SelfReported,
    Reported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmCapabilities {
    pub can_coordinate: bool,
    pub room_temp_input: bool,
    pub target_temp_control: bool,
    pub target_watts_control: bool,
    pub identify: bool,
    pub mcp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmHomeStatus {
    pub control_mode: HomeControlMode,
    pub observed_room_temp_c: Option<f32>,
    pub target_room_temp_c: Option<f32>,
    pub target_watts: Option<f32>,
    pub heat_watts: f64,
    pub heat_btu_h: f64,
    pub heating_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DcentSwarmInfo {
    pub schema: u8,
    pub node_id: String,
    pub family: String,
    pub role: SwarmRole,
    pub cluster_id: Option<String>,
    pub queen_id: Option<String>,
    pub capabilities: SwarmCapabilities,
    pub home: SwarmHomeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmNode {
    pub id: String,
    pub hostname: String,
    pub display_name: String,
    pub ip: String,
    pub board_model: String,
    pub board_version: String,
    pub board_target: String,
    pub asic_model: String,
    pub firmware_version: String,
    pub mining_enabled: bool,
    pub pool_connected: bool,
    pub hashrate_ghs: f64,
    pub last_seen_unix_ms: u64,
    pub source: SwarmSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmDiscoveryInfo {
    pub mdns_enabled: bool,
    pub mdns_hostname: Option<String>,
    pub discovery_hint: String,
    pub api_url: Option<String>,
    pub mcp_url: Option<String>,
    pub mcp_transport: Option<String>,
    pub mcp_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmDesiredState {
    pub mining_enabled: Option<bool>,
    pub control_mode: Option<HomeControlMode>,
    pub target_room_temp_c: Option<f32>,
    pub target_watts: Option<f32>,
    pub issued_by: Option<String>,
    pub issued_at_epoch_s: Option<u64>,
    pub expires_at_epoch_s: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmCoordinationStatus {
    pub schema: u8,
    pub enrollment_supported: bool,
    pub report_endpoint: Option<String>,
    pub desired_state_endpoint: Option<String>,
    pub heartbeat_ttl_sec: Option<u64>,
    pub desired_state: Option<SwarmDesiredState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmStatus {
    pub schema: u8,
    pub node_id: String,
    pub role: SwarmRole,
    pub cluster_id: Option<String>,
    pub queen_id: Option<String>,
    pub hashrate_ghs: f64,
    pub power_watts: f64,
    pub heat_watts: f64,
    pub heat_btu_h: f64,
    pub control_mode: HomeControlMode,
    pub observed_room_temp_c: Option<f32>,
    pub target_room_temp_c: Option<f32>,
    pub target_watts: Option<f32>,
    pub heating_active: bool,
    pub updated_at: u64,
    pub local: Option<SwarmNode>,
    pub peers: Vec<SwarmNode>,
    pub peer_count: usize,
    pub discovery: Option<SwarmDiscoveryInfo>,
    pub coordination: SwarmCoordinationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmRoomTempRequest {
    #[serde(alias = "temp_c", alias = "tempC")]
    pub temp_c: f32,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, alias = "ttl_sec", alias = "ttlSec")]
    pub ttl_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwarmPeerReport {
    pub id: Option<String>,
    pub hostname: String,
    pub ip: String,
    #[serde(alias = "board_model")]
    pub board_model: String,
    #[serde(alias = "board_version")]
    pub board_version: Option<String>,
    #[serde(alias = "board_target")]
    pub board_target: Option<String>,
    #[serde(alias = "asic_model")]
    pub asic_model: String,
    #[serde(alias = "firmware_version")]
    pub firmware_version: String,
    #[serde(alias = "mining_enabled")]
    pub mining_enabled: bool,
    #[serde(alias = "pool_connected")]
    pub pool_connected: bool,
    #[serde(alias = "hashrate_ghs")]
    pub hashrate_ghs: f64,
}

impl Default for SwarmCoordinationStatus {
    fn default() -> Self {
        Self {
            schema: SWARM_SCHEMA_VERSION,
            enrollment_supported: false,
            report_endpoint: None,
            desired_state_endpoint: None,
            heartbeat_ttl_sec: Some(300),
            desired_state: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_pinned() {
        assert_eq!(SWARM_SCHEMA_VERSION, 1);
    }

    #[test]
    fn swarm_role_serializes_in_lowercase_wire_form() {
        // Wire form is critical for cross-firmware compat. Pin each
        // variant so a refactor that flips PascalCase/lowercase silently
        // breaks every consumer.
        assert_eq!(
            serde_json::to_string(&SwarmRole::Standalone).unwrap(),
            "\"standalone\""
        );
        assert_eq!(
            serde_json::to_string(&SwarmRole::Worker).unwrap(),
            "\"worker\""
        );
        assert_eq!(
            serde_json::to_string(&SwarmRole::Queen).unwrap(),
            "\"queen\""
        );
    }

    #[test]
    fn swarm_role_round_trips_in_lowercase() {
        for (variant, wire) in [
            (SwarmRole::Standalone, "\"standalone\""),
            (SwarmRole::Worker, "\"worker\""),
            (SwarmRole::Queen, "\"queen\""),
        ] {
            let parsed: SwarmRole = serde_json::from_str(wire).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn swarm_role_rejects_pascal_case_input() {
        // serde will error on case mismatch — pin so a refactor that
        // accidentally adds case-insensitive deserialization (deserialize_with
        // tag) would change this contract.
        assert!(serde_json::from_str::<SwarmRole>("\"Standalone\"").is_err());
        assert!(serde_json::from_str::<SwarmRole>("\"WORKER\"").is_err());
        assert!(serde_json::from_str::<SwarmRole>("\"QUEEN\"").is_err());
    }

    #[test]
    fn home_control_mode_serializes_in_lowercase() {
        assert_eq!(
            serde_json::to_string(&HomeControlMode::Manual).unwrap(),
            "\"manual\""
        );
        assert_eq!(
            serde_json::to_string(&HomeControlMode::Thermal).unwrap(),
            "\"thermal\""
        );
    }

    #[test]
    fn swarm_source_serializes_in_pascal_case() {
        // SwarmSource is intentionally PascalCase (DIFFERENT from
        // SwarmRole / HomeControlMode which are lowercase). Pin both
        // variants so a refactor that "harmonizes" the casing breaks
        // here instead of silently breaking downstream consumers.
        assert_eq!(
            serde_json::to_string(&SwarmSource::SelfReported).unwrap(),
            "\"SelfReported\""
        );
        assert_eq!(
            serde_json::to_string(&SwarmSource::Reported).unwrap(),
            "\"Reported\""
        );
    }

    #[test]
    fn swarm_source_rejects_lowercase_input() {
        // The intentional mismatch with SwarmRole catches refactors
        // that accidentally use lowercase here.
        assert!(serde_json::from_str::<SwarmSource>("\"selfReported\"").is_err());
        assert!(serde_json::from_str::<SwarmSource>("\"reported\"").is_err());
    }

    #[test]
    fn swarm_capabilities_serializes_in_camelcase() {
        let caps = SwarmCapabilities {
            can_coordinate: true,
            room_temp_input: false,
            target_temp_control: true,
            target_watts_control: false,
            identify: true,
            mcp: false,
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert!(json.get("canCoordinate").is_some());
        assert!(json.get("roomTempInput").is_some());
        assert!(json.get("targetTempControl").is_some());
        assert!(json.get("targetWattsControl").is_some());
        assert!(json.get("identify").is_some());
        assert!(json.get("mcp").is_some());

        // snake_case must NOT appear.
        assert!(json.get("can_coordinate").is_none());
        assert!(json.get("room_temp_input").is_none());
    }

    #[test]
    fn swarm_coordination_status_default_carries_schema_and_ttl() {
        // Default must produce a structure with schema=1 and
        // heartbeat_ttl_sec=Some(300). Pin so a refactor that flips
        // either silently breaks the heartbeat protocol.
        let default = SwarmCoordinationStatus::default();
        assert_eq!(default.schema, SWARM_SCHEMA_VERSION);
        assert_eq!(default.schema, 1);
        assert!(!default.enrollment_supported);
        assert!(default.report_endpoint.is_none());
        assert!(default.desired_state_endpoint.is_none());
        assert_eq!(default.heartbeat_ttl_sec, Some(300));
        assert!(default.desired_state.is_none());
    }

    #[test]
    fn swarm_room_temp_request_accepts_both_camel_and_snake_aliases() {
        // Operators can ship temp_c (Python toolbox) or tempC (JS) —
        // both must deserialize cleanly.
        let snake = r#"{"temp_c": 21.5}"#;
        let parsed_snake: SwarmRoomTempRequest = serde_json::from_str(snake).unwrap();
        assert!((parsed_snake.temp_c - 21.5).abs() < f32::EPSILON);

        let camel = r#"{"tempC": 22.5}"#;
        let parsed_camel: SwarmRoomTempRequest = serde_json::from_str(camel).unwrap();
        assert!((parsed_camel.temp_c - 22.5).abs() < f32::EPSILON);
    }

    #[test]
    fn swarm_room_temp_request_accepts_ttl_aliases() {
        let snake = r#"{"temp_c": 20.0, "ttl_sec": 600}"#;
        let parsed: SwarmRoomTempRequest = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.ttl_sec, Some(600));

        let camel = r#"{"temp_c": 20.0, "ttlSec": 900}"#;
        let parsed: SwarmRoomTempRequest = serde_json::from_str(camel).unwrap();
        assert_eq!(parsed.ttl_sec, Some(900));
    }

    #[test]
    fn swarm_peer_report_aliases_snake_case_field_names() {
        // Older toolbox versions emit snake_case; the alias must continue
        // to accept both forms.
        let snake = r#"{
            "id": null,
            "hostname": "miner-39",
            "ip": "203.0.113.39",
            "board_model": "S9",
            "board_version": "v3.1",
            "board_target": "am1-s9",
            "asic_model": "BM1387",
            "firmware_version": "0.5.0",
            "mining_enabled": true,
            "pool_connected": true,
            "hashrate_ghs": 13500.0
        }"#;
        let parsed: SwarmPeerReport = serde_json::from_str(snake).unwrap();
        assert_eq!(parsed.hostname, "miner-39");
        assert_eq!(parsed.board_model, "S9");
        assert_eq!(parsed.board_target.as_deref(), Some("am1-s9"));
        assert_eq!(parsed.asic_model, "BM1387");
        assert!(parsed.mining_enabled);
        assert!(parsed.pool_connected);
        assert!((parsed.hashrate_ghs - 13500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dcent_swarm_info_round_trips_through_json() {
        let info = DcentSwarmInfo {
            schema: SWARM_SCHEMA_VERSION,
            node_id: "node-1".to_string(),
            family: "dcentos".to_string(),
            role: SwarmRole::Worker,
            cluster_id: Some("cluster-a".to_string()),
            queen_id: None,
            capabilities: SwarmCapabilities {
                can_coordinate: false,
                room_temp_input: true,
                target_temp_control: true,
                target_watts_control: false,
                identify: true,
                mcp: false,
            },
            home: SwarmHomeStatus {
                control_mode: HomeControlMode::Thermal,
                observed_room_temp_c: Some(21.5),
                target_room_temp_c: Some(22.0),
                target_watts: None,
                heat_watts: 1500.0,
                heat_btu_h: 5118.0,
                heating_active: true,
            },
        };
        let json = serde_json::to_string(&info).unwrap();
        let recovered: DcentSwarmInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, recovered);
    }

    #[test]
    fn dcent_swarm_info_serializes_camelcase_node_id_and_cluster_id() {
        let info = DcentSwarmInfo {
            schema: 1,
            node_id: "n1".to_string(),
            family: "dcentaxe".to_string(),
            role: SwarmRole::Standalone,
            cluster_id: Some("c1".to_string()),
            queen_id: Some("q1".to_string()),
            capabilities: SwarmCapabilities {
                can_coordinate: false,
                room_temp_input: false,
                target_temp_control: false,
                target_watts_control: false,
                identify: false,
                mcp: false,
            },
            home: SwarmHomeStatus {
                control_mode: HomeControlMode::Manual,
                observed_room_temp_c: None,
                target_room_temp_c: None,
                target_watts: None,
                heat_watts: 0.0,
                heat_btu_h: 0.0,
                heating_active: false,
            },
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("nodeId").is_some());
        assert!(json.get("clusterId").is_some());
        assert!(json.get("queenId").is_some());
        // snake_case must NOT appear on the wire.
        assert!(json.get("node_id").is_none());
        assert!(json.get("cluster_id").is_none());
    }

    #[test]
    fn swarm_node_carries_all_identification_fields_in_round_trip() {
        let node = SwarmNode {
            id: "n1".to_string(),
            hostname: "host-1".to_string(),
            display_name: "Display 1".to_string(),
            ip: "203.0.113.39".to_string(),
            board_model: "S9".to_string(),
            board_version: "v3.1".to_string(),
            board_target: "am1-s9".to_string(),
            asic_model: "BM1387".to_string(),
            firmware_version: "0.5.0".to_string(),
            mining_enabled: true,
            pool_connected: true,
            hashrate_ghs: 13500.0,
            last_seen_unix_ms: 1_700_000_000_000,
            source: SwarmSource::SelfReported,
        };
        let json = serde_json::to_string(&node).unwrap();
        let recovered: SwarmNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, recovered);
        assert_eq!(recovered.source, SwarmSource::SelfReported);
    }
}
