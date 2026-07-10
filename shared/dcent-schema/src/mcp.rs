use serde::{Deserialize, Serialize};

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const MINIMAL_PROFILE_ID: &str = "dcent.cross-firmware.minimal.v1";

/// The cross-firmware MCP *tuning superset* profile id (convergence S5).
///
/// This is a STRICT superset of `MINIMAL_PROFILE_ID`: the first 6 tools of
/// `tuning_profile()` are byte-identical to `minimal_profile()` (the locked
/// kernel), followed by 6 richer control/read tools (`get_network`,
/// `get_history`, `set_frequency`, `set_core_voltage`, `set_fan_speed`,
/// `run_autotune`). The floor id stays the discovery default — surfaces that
/// only advertise the kernel (REST `/mcp`, the Python `:3000` `initialize`)
/// keep binding `MINIMAL_PROFILE_ID`; the superset id is for surfaces that
/// advertise the full tuning vocabulary (today: axe `tools/list`).
pub const TUNING_PROFILE_ID: &str = "dcent.cross-firmware.tuning.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpProfile {
    pub id: String,
    pub protocol_version: String,
    pub transport: String,
    pub tools: Vec<McpToolDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    pub legacy_aliases: Vec<String>,
    pub write: bool,
}

pub fn minimal_profile(transport: impl Into<String>) -> McpProfile {
    McpProfile {
        id: MINIMAL_PROFILE_ID.to_string(),
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        transport: transport.into(),
        tools: vec![
            McpToolDescriptor {
                name: "get_status".to_string(),
                description: "Read the current miner status summary".to_string(),
                legacy_aliases: vec![
                    "get_system_status".to_string(),
                    "live_stats".to_string(),
                    "get_hashrate".to_string(),
                ],
                write: false,
            },
            McpToolDescriptor {
                name: "get_device_info".to_string(),
                description: "Read the current device identity and ASIC metadata".to_string(),
                legacy_aliases: vec!["get_asic_info".to_string(), "get_config".to_string()],
                write: false,
            },
            McpToolDescriptor {
                name: "get_swarm_status".to_string(),
                description: "Read the shared swarm and discovery status".to_string(),
                legacy_aliases: vec!["get_swarm".to_string()],
                write: false,
            },
            McpToolDescriptor {
                name: "identify_device".to_string(),
                description: "Toggle the physical identify signal for the device".to_string(),
                legacy_aliases: Vec::new(),
                write: true,
            },
            McpToolDescriptor {
                name: "restart_mining".to_string(),
                description: "Restart mining without redefining the pool configuration".to_string(),
                legacy_aliases: vec!["service_control".to_string()],
                write: true,
            },
            McpToolDescriptor {
                name: "set_pool".to_string(),
                description: "Update the active mining pool target".to_string(),
                legacy_aliases: vec!["pool_switch".to_string()],
                write: true,
            },
        ],
    }
}

/// The cross-firmware MCP *tuning superset* (convergence S5).
///
/// A STRICT superset of [`minimal_profile`]: it is built FROM `minimal_profile`
/// so the locked kernel-6 can never drift between the two profiles, then the id
/// is overwritten with [`TUNING_PROFILE_ID`] and the 6 extension rows are
/// appended in their frozen tail order (reads-then-writes):
/// `get_network`, `get_history`, `set_frequency`, `set_core_voltage`,
/// `set_fan_speed`, `run_autotune`.
///
/// Tool count is 12 (5 READ / 7 CONTROL). The 4 new CONTROL tools
/// (`set_frequency`/`set_core_voltage`/`set_fan_speed`/`run_autotune`) inherit
/// the same write-gate posture as the kernel writes (axe fail-closed-on-open;
/// OS `:3000` open-on-dev / locked-on-release). The 6 extension tools carry NO
/// legacy aliases on any surface.
///
/// `transport` is parameterized exactly like `minimal_profile` (Python overlays
/// emit `"streamable-http"`; axe `initialize` uses `"http-jsonrpc"`).
pub fn tuning_profile(transport: impl Into<String>) -> McpProfile {
    let mut profile = minimal_profile(transport);
    profile.id = TUNING_PROFILE_ID.to_string();
    profile.tools.extend(vec![
        McpToolDescriptor {
            name: "get_network".to_string(),
            description: "Read the current network identity and Wi-Fi link status".to_string(),
            legacy_aliases: Vec::new(),
            write: false,
        },
        McpToolDescriptor {
            name: "get_history".to_string(),
            description: "Read recent rolling performance history samples".to_string(),
            legacy_aliases: Vec::new(),
            write: false,
        },
        McpToolDescriptor {
            name: "set_frequency".to_string(),
            description: "Set the ASIC PLL frequency target (disables autotuner)".to_string(),
            legacy_aliases: Vec::new(),
            write: true,
        },
        McpToolDescriptor {
            name: "set_core_voltage".to_string(),
            description: "Set the ASIC core voltage target (disables autotuner)".to_string(),
            legacy_aliases: Vec::new(),
            write: true,
        },
        McpToolDescriptor {
            name: "set_fan_speed".to_string(),
            description: "Set the fan duty target (clamped; home cap applies)".to_string(),
            legacy_aliases: Vec::new(),
            write: true,
        },
        McpToolDescriptor {
            name: "run_autotune".to_string(),
            description: "Start or stop the autotuner with an optimization target".to_string(),
            legacy_aliases: Vec::new(),
            write: true,
        },
    ]);
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_constant_is_pinned() {
        // MCP protocol version drives client compatibility. A drift
        // here without coordinating with downstream MCP clients
        // (Claude Desktop, custom integrations) silently breaks them.
        assert_eq!(MCP_PROTOCOL_VERSION, "2024-11-05");
    }

    #[test]
    fn minimal_profile_id_constant_is_pinned() {
        // Profile ID is the discovery key — clients filter on it.
        assert_eq!(MINIMAL_PROFILE_ID, "dcent.cross-firmware.minimal.v1");
    }

    #[test]
    fn minimal_profile_has_six_tools() {
        // The cross-firmware minimal profile must ship exactly 6 tools.
        // Adding/removing without coordination silently breaks clients
        // that pre-bind on tool names.
        let profile = minimal_profile("stdio");
        assert_eq!(profile.tools.len(), 6);
    }

    #[test]
    fn minimal_profile_tools_split_three_read_three_write() {
        // Three read-only tools (no `write`), three write tools.
        // This split is contractual — a refactor that flips a tool's
        // `write` flag silently changes the auth surface clients
        // expect.
        let profile = minimal_profile("stdio");
        let reads = profile.tools.iter().filter(|t| !t.write).count();
        let writes = profile.tools.iter().filter(|t| t.write).count();
        assert_eq!(reads, 3, "expected 3 read tools");
        assert_eq!(writes, 3, "expected 3 write tools");
    }

    #[test]
    fn minimal_profile_tool_names_are_locked() {
        // Pin every tool name. Clients bind by name; a rename is a
        // breaking change that must be deliberate.
        let profile = minimal_profile("stdio");
        let names: Vec<&str> = profile.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "get_status",
                "get_device_info",
                "get_swarm_status",
                "identify_device",
                "restart_mining",
                "set_pool",
            ]
        );
    }

    #[test]
    fn minimal_profile_legacy_aliases_are_locked() {
        // Each tool's legacy_aliases list is the cross-firmware compat
        // surface. A drift here silently breaks toolbox/dcentaxe paths
        // that still call the old name.
        let profile = minimal_profile("stdio");

        let get_status = profile
            .tools
            .iter()
            .find(|t| t.name == "get_status")
            .unwrap();
        assert_eq!(
            get_status.legacy_aliases,
            vec!["get_system_status", "live_stats", "get_hashrate"]
        );

        let get_device_info = profile
            .tools
            .iter()
            .find(|t| t.name == "get_device_info")
            .unwrap();
        assert_eq!(
            get_device_info.legacy_aliases,
            vec!["get_asic_info", "get_config"]
        );

        let get_swarm_status = profile
            .tools
            .iter()
            .find(|t| t.name == "get_swarm_status")
            .unwrap();
        assert_eq!(get_swarm_status.legacy_aliases, vec!["get_swarm"]);

        // identify_device has no legacy aliases (was always identify_device).
        let identify = profile
            .tools
            .iter()
            .find(|t| t.name == "identify_device")
            .unwrap();
        assert!(identify.legacy_aliases.is_empty());

        let restart_mining = profile
            .tools
            .iter()
            .find(|t| t.name == "restart_mining")
            .unwrap();
        assert_eq!(restart_mining.legacy_aliases, vec!["service_control"]);

        let set_pool = profile.tools.iter().find(|t| t.name == "set_pool").unwrap();
        assert_eq!(set_pool.legacy_aliases, vec!["pool_switch"]);
    }

    #[test]
    fn minimal_profile_write_flags_are_locked() {
        // Pin which tools are write-tagged. The auth surface rules in
        // dcentos/dcentaxe gate write tools differently from reads;
        // flipping a flag silently changes the security posture.
        let profile = minimal_profile("stdio");
        for (name, expected_write) in [
            ("get_status", false),
            ("get_device_info", false),
            ("get_swarm_status", false),
            ("identify_device", true),
            ("restart_mining", true),
            ("set_pool", true),
        ] {
            let tool = profile.tools.iter().find(|t| t.name == name).unwrap();
            assert_eq!(
                tool.write, expected_write,
                "{name} write flag must be {expected_write}"
            );
        }
    }

    #[test]
    fn minimal_profile_carries_provided_transport() {
        let profile = minimal_profile("stdio");
        assert_eq!(profile.transport, "stdio");

        let websocket = minimal_profile("websocket");
        assert_eq!(websocket.transport, "websocket");

        let http = minimal_profile("http");
        assert_eq!(http.transport, "http");
    }

    #[test]
    fn mcp_profile_serializes_in_camelcase_wire_form() {
        let profile = minimal_profile("stdio");
        let json = serde_json::to_value(&profile).unwrap();

        assert!(json.get("id").is_some());
        assert!(json.get("protocolVersion").is_some());
        assert!(json.get("transport").is_some());
        assert!(json.get("tools").is_some());

        // snake_case must NOT appear on the wire.
        assert!(json.get("protocol_version").is_none());
    }

    #[test]
    fn mcp_tool_descriptor_serializes_in_camelcase_wire_form() {
        let profile = minimal_profile("stdio");
        let tool_json = serde_json::to_value(&profile.tools[0]).unwrap();
        assert!(tool_json.get("name").is_some());
        assert!(tool_json.get("description").is_some());
        assert!(tool_json.get("legacyAliases").is_some());
        assert!(tool_json.get("write").is_some());

        assert!(tool_json.get("legacy_aliases").is_none());
    }

    #[test]
    fn mcp_profile_round_trips_through_json() {
        let original = minimal_profile("stdio");
        let json = serde_json::to_string(&original).unwrap();
        let recovered: McpProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(original, recovered);
    }

    // ---- S5 tuning superset pins (mirror the minimal pins above) ------------

    #[test]
    fn tuning_profile_id_constant_is_pinned() {
        // The superset discovery key. Clients that bind the full tuning
        // vocabulary filter on this id; a drift is a breaking change.
        assert_eq!(TUNING_PROFILE_ID, "dcent.cross-firmware.tuning.v1");
    }

    #[test]
    fn tuning_profile_id_differs_from_minimal() {
        // The superset MUST advertise a distinct id from the floor so a client
        // can tell "kernel only" from "kernel + tuning" apart at discovery.
        assert_ne!(TUNING_PROFILE_ID, MINIMAL_PROFILE_ID);
    }

    #[test]
    fn tuning_profile_reuses_the_minimal_protocol_version() {
        // No second protocol-version const — the superset rides the same MCP
        // protocol version as the floor.
        let profile = tuning_profile("stdio");
        assert_eq!(profile.protocol_version, MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn tuning_profile_has_twelve_tools() {
        // The superset must ship exactly 12 tools (kernel 6 + 6 extensions).
        let profile = tuning_profile("stdio");
        assert_eq!(profile.tools.len(), 12);
    }

    #[test]
    fn tuning_profile_kernel_is_minimal_profile() {
        // BYTE-IDENTITY GUARANTEE: the first 6 superset tools must equal the
        // entire minimal profile, tool-for-tool. This is what makes the kernel
        // un-driftable between the two profiles (the superset is built FROM
        // minimal_profile, not re-authored).
        let tuning = tuning_profile("stdio");
        let minimal = minimal_profile("stdio");
        assert_eq!(&tuning.tools[..6], &minimal.tools[..]);
    }

    #[test]
    fn tuning_profile_split_five_read_seven_write() {
        // The auth split is contractual: 5 read tools, 7 control/write tools.
        // A flipped `write` flag silently changes the auth surface clients
        // (and the OS release-image bearer-token gate) expect.
        let profile = tuning_profile("stdio");
        let reads = profile.tools.iter().filter(|t| !t.write).count();
        let writes = profile.tools.iter().filter(|t| t.write).count();
        assert_eq!(reads, 5, "expected 5 read tools");
        assert_eq!(writes, 7, "expected 7 write tools");
    }

    #[test]
    fn tuning_profile_tool_names_are_locked() {
        // Pin every superset tool name in frozen order: kernel 6 first (in
        // their minimal order), then the 6 extensions reads-then-writes.
        let profile = tuning_profile("stdio");
        let names: Vec<&str> = profile.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "get_status",
                "get_device_info",
                "get_swarm_status",
                "identify_device",
                "restart_mining",
                "set_pool",
                "get_network",
                "get_history",
                "set_frequency",
                "set_core_voltage",
                "set_fan_speed",
                "run_autotune",
            ]
        );
    }

    #[test]
    fn tuning_profile_extension_tools_have_no_aliases() {
        // The 6 extension tools carry ZERO legacy aliases on every surface
        // (axe already emits all 6 canonical names; OS exposes only
        // set_fan_speed, also canonically). Registering an alias here would
        // invent a compat surface that no firmware actually accepts.
        let profile = tuning_profile("stdio");
        for name in [
            "get_network",
            "get_history",
            "set_frequency",
            "set_core_voltage",
            "set_fan_speed",
            "run_autotune",
        ] {
            let tool = profile.tools.iter().find(|t| t.name == name).unwrap();
            assert!(
                tool.legacy_aliases.is_empty(),
                "{name} must have no legacy aliases"
            );
        }
    }

    #[test]
    fn tuning_profile_write_flags_are_locked() {
        // Pin the (name, write) class for all 12 superset tools. The two new
        // READ tools (get_network/get_history) MUST stay reads; the four new
        // CONTROL tools (set_frequency/set_core_voltage/set_fan_speed/
        // run_autotune) MUST stay writes — they actuate hardware and must route
        // through the write-gate on every surface.
        let profile = tuning_profile("stdio");
        for (name, expected_write) in [
            ("get_status", false),
            ("get_device_info", false),
            ("get_swarm_status", false),
            ("identify_device", true),
            ("restart_mining", true),
            ("set_pool", true),
            ("get_network", false),
            ("get_history", false),
            ("set_frequency", true),
            ("set_core_voltage", true),
            ("set_fan_speed", true),
            ("run_autotune", true),
        ] {
            let tool = profile.tools.iter().find(|t| t.name == name).unwrap();
            assert_eq!(
                tool.write, expected_write,
                "{name} write flag must be {expected_write}"
            );
        }
    }

    #[test]
    fn tuning_profile_carries_provided_transport() {
        assert_eq!(tuning_profile("stdio").transport, "stdio");
        assert_eq!(tuning_profile("http-jsonrpc").transport, "http-jsonrpc");
        assert_eq!(
            tuning_profile("streamable-http").transport,
            "streamable-http"
        );
    }
}

/// Author-once / emit-twice / VALIDATE generator (token-contract §0,
/// `UIVIS-RENDER-1`) for the MCP domain.
///
/// `minimal_profile()` above is the AUTHORED spec. This module is the "emit"
/// half: it serializes the registry to the exact `{tools:[{name, legacyAliases,
/// write}]}` JSON the Python `:3000` overlay re-states by hand, and pins that
/// emitted SHAPE so a registry change is a deliberate, reviewed edit.
///
/// The companion "validate" half lives in `tests/python_overlay_drift.rs`,
/// which drives its assertions FROM `minimal_profile()` against both overlay
/// files — so this generator and that drift check together close the loop:
/// change the spec here → the emission shape pin (this module) and the Python
/// overlays (that test) BOTH go red until re-aligned.
///
/// No new build dependency: `serde_json` is already a dev-dependency
/// (`Cargo.toml`), and nothing is written to disk (a checked-in generated JSON
/// would be a commit-the-artifact hazard analogous to the forbidden
/// `dist/index.html`).
#[cfg(test)]
mod python_overlay_emission_contract {
    use super::*;
    use serde_json::Value;

    /// The transport the Python `:3000` overlay emits (`minimal_profile()` dict
    /// hard-codes `"transport": "streamable-http"`).
    const PY_TRANSPORT: &str = "streamable-http";

    /// The canonical `(name, legacy_aliases, write)` emission rows, exactly as
    /// the Python overlay's `minimal_profile()` dict must re-state them.
    /// Authored here once; the drift test asserts the overlays mirror it.
    fn expected_rows() -> Vec<(&'static str, Vec<&'static str>, bool)> {
        vec![
            (
                "get_status",
                vec!["get_system_status", "live_stats", "get_hashrate"],
                false,
            ),
            (
                "get_device_info",
                vec!["get_asic_info", "get_config"],
                false,
            ),
            ("get_swarm_status", vec!["get_swarm"], false),
            ("identify_device", vec![], true),
            ("restart_mining", vec!["service_control"], true),
            ("set_pool", vec!["pool_switch"], true),
        ]
    }

    /// The registry serializes to the exact wire shape the Python overlay
    /// mirrors: `transport == "streamable-http"`, and each tool emits
    /// `name` / `legacyAliases` (camelCase) / `write` with the canonical values.
    #[test]
    fn emitted_profile_shape_is_stable_for_python_overlay() {
        let json = serde_json::to_value(minimal_profile(PY_TRANSPORT)).unwrap();

        assert_eq!(
            json.get("transport").and_then(Value::as_str),
            Some(PY_TRANSPORT),
            "Python overlay emits streamable-http; the generator must match"
        );

        let tools = json
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array");
        let expected = expected_rows();
        assert_eq!(
            tools.len(),
            expected.len(),
            "emitted tool count drifted from the Python-overlay expectation"
        );

        for (tool, (name, aliases, write)) in tools.iter().zip(expected.iter()) {
            assert_eq!(
                tool.get("name").and_then(Value::as_str),
                Some(*name),
                "emitted tool name drift"
            );
            // camelCase key on the wire (NOT snake_case) — the Python dict uses
            // `legacyAliases`, so the generator must too.
            assert!(
                tool.get("legacy_aliases").is_none(),
                "snake_case legacy_aliases must NOT appear on the wire"
            );
            let emitted_aliases: Vec<&str> = tool
                .get("legacyAliases")
                .and_then(Value::as_array)
                .expect("legacyAliases array")
                .iter()
                .map(|v| v.as_str().expect("alias is a string"))
                .collect();
            assert_eq!(
                &emitted_aliases, aliases,
                "emitted legacyAliases drift for `{name}`"
            );
            assert_eq!(
                tool.get("write").and_then(Value::as_bool),
                Some(*write),
                "emitted write flag drift for `{name}`"
            );
        }
    }

    /// The exact JSON the Python overlay re-states by hand round-trips back into
    /// the strongly-typed registry, byte-for-byte equal to what the generator
    /// emits. This is the machine-checkable "the hand-mirror IS the spec" pin.
    #[test]
    fn python_facing_emission_round_trips_into_the_registry() {
        let original = minimal_profile(PY_TRANSPORT);
        let emitted = serde_json::to_string(&original).unwrap();
        let recovered: McpProfile = serde_json::from_str(&emitted).unwrap();
        assert_eq!(
            original, recovered,
            "the streamable-http emission must round-trip into McpProfile"
        );
    }

    /// The full 12-row tuning-superset emission rows, in frozen order. The OS
    /// `:3000` overlay mirrors only a SUBSET of these (kernel 6 + set_fan_speed);
    /// the per-surface exposure policy lives in `tests/python_overlay_drift.rs`,
    /// NOT here. This pins the wire SHAPE of the full vocabulary the schema emits.
    fn expected_tuning_rows() -> Vec<(&'static str, Vec<&'static str>, bool)> {
        let mut rows = expected_rows();
        rows.extend(vec![
            ("get_network", vec![], false),
            ("get_history", vec![], false),
            ("set_frequency", vec![], true),
            ("set_core_voltage", vec![], true),
            ("set_fan_speed", vec![], true),
            ("run_autotune", vec![], true),
        ]);
        rows
    }

    /// The superset registry serializes to the exact wire shape any surface that
    /// advertises the full tuning vocabulary mirrors: `transport ==
    /// "streamable-http"`, 12 rows, each tool emitting `name` / `legacyAliases`
    /// (camelCase) / `write` with the canonical values. Mirrors
    /// `emitted_profile_shape_is_stable_for_python_overlay` for the superset.
    #[test]
    fn emitted_tuning_profile_shape_is_stable() {
        let json = serde_json::to_value(tuning_profile(PY_TRANSPORT)).unwrap();

        assert_eq!(
            json.get("transport").and_then(Value::as_str),
            Some(PY_TRANSPORT),
            "tuning superset emits the provided transport"
        );

        let tools = json
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array");
        let expected = expected_tuning_rows();
        assert_eq!(
            tools.len(),
            expected.len(),
            "emitted superset tool count drifted (expected 12)"
        );

        for (tool, (name, aliases, write)) in tools.iter().zip(expected.iter()) {
            assert_eq!(
                tool.get("name").and_then(Value::as_str),
                Some(*name),
                "emitted superset tool name drift"
            );
            // camelCase key on the wire (NOT snake_case).
            assert!(
                tool.get("legacy_aliases").is_none(),
                "snake_case legacy_aliases must NOT appear on the wire"
            );
            let emitted_aliases: Vec<&str> = tool
                .get("legacyAliases")
                .and_then(Value::as_array)
                .expect("legacyAliases array")
                .iter()
                .map(|v| v.as_str().expect("alias is a string"))
                .collect();
            assert_eq!(
                &emitted_aliases, aliases,
                "emitted superset legacyAliases drift for `{name}`"
            );
            assert_eq!(
                tool.get("write").and_then(Value::as_bool),
                Some(*write),
                "emitted superset write flag drift for `{name}`"
            );
        }
    }
}
