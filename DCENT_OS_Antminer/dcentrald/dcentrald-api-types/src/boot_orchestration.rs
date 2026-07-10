//!  orch-A — system-orchestration FSM DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! (514 lines).
//!
//! This module pins:
//! 1. The 12 DCENT_OS subsystems and their bring-up dependencies.
//! 2. The 9 cold-boot orchestration phases the runtime walks through.
//! 3. The teardown ordering (reverse-topological — failures unwind safely).
//!
//! The runtime adapter wires actual subsystem health/start/stop hooks; this
//! crate just pins the contract so the orchestration FSM, dashboard
//! "boot progress" widget, and CI gates all agree on what depends on what.
//!
//! Hard rules pinned by tests:
//! - The dependency graph has NO cycles.
//! - `PsuReady` REQUIRES `PicReady` (PIC is on the I²C bus the PSU shares).
//! - `MiningCapable` REQUIRES `StratumConnected` AND `AsicEnumerated` AND
//!   `ThermalArmed`.
//! - Teardown order is the reverse of bring-up order (LIFO).

use serde::{Deserialize, Serialize};

/// 12 subsystems that bring up the DCENT_OS mining stack. Per
/// port-bos-lux PROGRESS.md numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemId {
    /// Sub-01 — early-init, /dev mounts, watchdog disable.
    Init,
    /// Sub-02 — HAL platform layer (I²C, GPIO, UART, FPGA UIO).
    Hal,
    /// Sub-03 — ASIC drivers (BM13xx/BM14xx).
    Asic,
    /// Sub-04 — PIC voltage controller / dsPIC microcontroller layer.
    Pic,
    /// Sub-04 — PSU control (APW family, PMBus).
    Psu,
    /// Sub-05 — Thermal control + fans.
    Thermal,
    /// Sub-06 — Stratum V1/V2 client.
    Stratum,
    /// Sub-07 — Autotuner (frequency/voltage profile management).
    Autotuner,
    /// Sub-08 — Mining REST API (port 8080) + CGMiner-compat (port 4028).
    MiningApi,
    /// Sub-09 — Dashboard UI (server.py on port 80).
    Dashboard,
    /// Sub-10 — Fleet installer / OTA / sysupgrade machinery.
    FleetInstaller,
    /// Sub-11 — Telemetry + audit log + metrics CSV.
    Telemetry,
    /// Sub-12 — Recovery + security tools (PIC recovery, x21_aes).
    Recovery,
}

/// Coarse cold-boot orchestration phase. Each phase requires its
/// predecessor. The runtime emits one `OrchestrationPhase` event per
/// transition for the dashboard "boot progress" widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationPhase {
    /// T+0: PID 1 launched, before any HAL touch.
    PreBoot,
    /// HAL platform layer initialized; I²C bus, GPIO, UART available.
    HalReady,
    /// PIC heartbeat established (5+ stable ticks).
    PicReady,
    /// PSU enabled and reporting healthy on the same I²C bus.
    PsuReady,
    /// All ASIC chains enumerated (chip count matches expected per chain).
    AsicEnumerated,
    /// Thermal controller armed; fans spinning under PWM control.
    ThermalArmed,
    /// Stratum subscribed + authorized; first job received.
    StratumConnected,
    /// Mining loop active; work flowing TX → ASICs and nonces RX.
    MiningCapable,
    /// Autotuner started; per-chip frequency optimization running.
    AutotunerArmed,
}

impl OrchestrationPhase {
    /// Index of this phase in the canonical order (0 = PreBoot).
    pub fn index(&self) -> u8 {
        match self {
            Self::PreBoot => 0,
            Self::HalReady => 1,
            Self::PicReady => 2,
            Self::PsuReady => 3,
            Self::AsicEnumerated => 4,
            Self::ThermalArmed => 5,
            Self::StratumConnected => 6,
            Self::MiningCapable => 7,
            Self::AutotunerArmed => 8,
        }
    }

    /// True iff this phase strictly precedes `other`.
    pub fn precedes(&self, other: OrchestrationPhase) -> bool {
        self.index() < other.index()
    }

    /// Predecessor phase, or None for `PreBoot`.
    pub fn predecessor(&self) -> Option<OrchestrationPhase> {
        match self {
            Self::PreBoot => None,
            Self::HalReady => Some(Self::PreBoot),
            Self::PicReady => Some(Self::HalReady),
            Self::PsuReady => Some(Self::PicReady),
            Self::AsicEnumerated => Some(Self::PsuReady),
            Self::ThermalArmed => Some(Self::AsicEnumerated),
            Self::StratumConnected => Some(Self::ThermalArmed),
            Self::MiningCapable => Some(Self::StratumConnected),
            Self::AutotunerArmed => Some(Self::MiningCapable),
        }
    }

    /// Returns ALL phases in canonical bring-up order.
    pub fn canonical_order() -> [OrchestrationPhase; 9] {
        [
            Self::PreBoot,
            Self::HalReady,
            Self::PicReady,
            Self::PsuReady,
            Self::AsicEnumerated,
            Self::ThermalArmed,
            Self::StratumConnected,
            Self::MiningCapable,
            Self::AutotunerArmed,
        ]
    }
}

/// Dependency edge in the subsystem graph: `subsystem` depends on `requires`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsystemDependency {
    pub subsystem: SubsystemId,
    pub requires: SubsystemId,
}

/// Canonical subsystem bring-up dependency graph. Read as
/// "`subsystem` cannot start until `requires` is up".
pub const SUBSYSTEM_DEPENDENCIES: &[SubsystemDependency] = &[
    SubsystemDependency {
        subsystem: SubsystemId::Hal,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Pic,
        requires: SubsystemId::Hal,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Psu,
        requires: SubsystemId::Pic,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Asic,
        requires: SubsystemId::Psu,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Asic,
        requires: SubsystemId::Hal,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Thermal,
        requires: SubsystemId::Hal,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Thermal,
        requires: SubsystemId::Asic,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Stratum,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Autotuner,
        requires: SubsystemId::Asic,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Autotuner,
        requires: SubsystemId::Thermal,
    },
    SubsystemDependency {
        subsystem: SubsystemId::MiningApi,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Dashboard,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::FleetInstaller,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Telemetry,
        requires: SubsystemId::MiningApi,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Recovery,
        requires: SubsystemId::Init,
    },
    SubsystemDependency {
        subsystem: SubsystemId::Recovery,
        requires: SubsystemId::Pic,
    },
];

/// Canonical bring-up order (Kahn's topological sort applied to
/// `SUBSYSTEM_DEPENDENCIES`). Stable ordering pinned by tests.
pub const SUBSYSTEM_BRINGUP_ORDER: &[SubsystemId] = &[
    SubsystemId::Init,
    SubsystemId::Hal,
    SubsystemId::Pic,
    SubsystemId::Psu,
    SubsystemId::Asic,
    SubsystemId::Thermal,
    SubsystemId::Stratum,
    SubsystemId::Autotuner,
    SubsystemId::MiningApi,
    SubsystemId::Dashboard,
    SubsystemId::FleetInstaller,
    SubsystemId::Telemetry,
    SubsystemId::Recovery,
];

/// Look up the immediate dependencies of a subsystem.
pub fn dependencies_of(subsystem: SubsystemId) -> Vec<SubsystemId> {
    SUBSYSTEM_DEPENDENCIES
        .iter()
        .filter(|edge| edge.subsystem == subsystem)
        .map(|edge| edge.requires)
        .collect()
}

/// Teardown order is the REVERSE of bring-up order (LIFO). On a fault,
/// later subsystems must stop before earlier ones, so the runtime never
/// pulls the rug out from under a still-running upper layer.
pub fn teardown_order() -> Vec<SubsystemId> {
    SUBSYSTEM_BRINGUP_ORDER.iter().rev().copied().collect()
}

/// Walk the dependency graph and return true iff there is a cycle. Pinned
/// to FALSE by tests.
pub fn has_cycle() -> bool {
    use std::collections::{HashMap, HashSet};
    let mut adj: HashMap<SubsystemId, Vec<SubsystemId>> = HashMap::new();
    for edge in SUBSYSTEM_DEPENDENCIES {
        adj.entry(edge.subsystem).or_default().push(edge.requires);
    }
    let mut visiting: HashSet<SubsystemId> = HashSet::new();
    let mut visited: HashSet<SubsystemId> = HashSet::new();

    fn dfs(
        node: SubsystemId,
        adj: &std::collections::HashMap<SubsystemId, Vec<SubsystemId>>,
        visiting: &mut std::collections::HashSet<SubsystemId>,
        visited: &mut std::collections::HashSet<SubsystemId>,
    ) -> bool {
        if visiting.contains(&node) {
            return true;
        }
        if visited.contains(&node) {
            return false;
        }
        visiting.insert(node);
        if let Some(deps) = adj.get(&node) {
            for dep in deps {
                if dfs(*dep, adj, visiting, visited) {
                    return true;
                }
            }
        }
        visiting.remove(&node);
        visited.insert(node);
        false
    }

    for &node in SUBSYSTEM_BRINGUP_ORDER {
        if dfs(node, &adj, &mut visiting, &mut visited) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_phase_count_is_nine() {
        assert_eq!(OrchestrationPhase::canonical_order().len(), 9);
    }

    #[test]
    fn phase_index_is_strictly_increasing() {
        let order = OrchestrationPhase::canonical_order();
        for window in order.windows(2) {
            assert!(window[0].index() < window[1].index());
        }
    }

    #[test]
    fn precedes_is_strict_partial_order() {
        assert!(OrchestrationPhase::PreBoot.precedes(OrchestrationPhase::HalReady));
        assert!(OrchestrationPhase::PsuReady.precedes(OrchestrationPhase::AsicEnumerated));
        // Strict: a phase does NOT precede itself.
        assert!(!OrchestrationPhase::PsuReady.precedes(OrchestrationPhase::PsuReady));
        // Strict: AutotunerArmed does NOT precede PreBoot.
        assert!(!OrchestrationPhase::AutotunerArmed.precedes(OrchestrationPhase::PreBoot));
    }

    #[test]
    fn pre_boot_has_no_predecessor() {
        assert_eq!(OrchestrationPhase::PreBoot.predecessor(), None);
    }

    #[test]
    fn psu_ready_predecessor_is_pic_ready() {
        // Pinned by the I²C-bus-sharing rule: PIC must be alive on the
        // shared bus before PSU PMBus traffic begins.
        assert_eq!(
            OrchestrationPhase::PsuReady.predecessor(),
            Some(OrchestrationPhase::PicReady)
        );
    }

    #[test]
    fn mining_capable_predecessor_is_stratum_connected() {
        // MiningCapable requires StratumConnected (and transitively
        // AsicEnumerated, ThermalArmed via the chain).
        assert_eq!(
            OrchestrationPhase::MiningCapable.predecessor(),
            Some(OrchestrationPhase::StratumConnected)
        );
    }

    #[test]
    fn dependencies_of_psu_includes_pic() {
        let deps = dependencies_of(SubsystemId::Psu);
        assert!(deps.contains(&SubsystemId::Pic));
    }

    #[test]
    fn dependencies_of_asic_includes_psu_and_hal() {
        let deps = dependencies_of(SubsystemId::Asic);
        assert!(deps.contains(&SubsystemId::Psu));
        assert!(deps.contains(&SubsystemId::Hal));
    }

    #[test]
    fn dependencies_of_init_is_empty() {
        // Init is the root — nothing precedes it.
        let deps = dependencies_of(SubsystemId::Init);
        assert!(deps.is_empty());
    }

    #[test]
    fn dependencies_of_thermal_includes_asic() {
        // Thermal needs Asic up so it can read chip temps via I²C
        // passthrough.
        let deps = dependencies_of(SubsystemId::Thermal);
        assert!(deps.contains(&SubsystemId::Asic));
        assert!(deps.contains(&SubsystemId::Hal));
    }

    #[test]
    fn graph_has_no_cycles() {
        // HARD rule: cycles in subsystem deps would deadlock cold boot.
        assert!(!has_cycle(), "subsystem dependency graph must be acyclic");
    }

    #[test]
    fn bringup_order_respects_all_dependencies() {
        // Every dependency edge: requires position < subsystem position.
        let pos = |s: SubsystemId| {
            SUBSYSTEM_BRINGUP_ORDER
                .iter()
                .position(|x| *x == s)
                .expect("subsystem missing from bringup order")
        };
        for edge in SUBSYSTEM_DEPENDENCIES {
            assert!(
                pos(edge.requires) < pos(edge.subsystem),
                "edge {:?} -> {:?} violates topological order",
                edge.requires,
                edge.subsystem
            );
        }
    }

    #[test]
    fn teardown_order_is_reverse_of_bringup() {
        let teardown = teardown_order();
        assert_eq!(teardown.len(), SUBSYSTEM_BRINGUP_ORDER.len());
        for (i, &id) in teardown.iter().enumerate() {
            assert_eq!(
                id,
                SUBSYSTEM_BRINGUP_ORDER[SUBSYSTEM_BRINGUP_ORDER.len() - 1 - i]
            );
        }
        // Spot-check: teardown starts with Recovery, ends with Init.
        assert_eq!(teardown.first(), Some(&SubsystemId::Recovery));
        assert_eq!(teardown.last(), Some(&SubsystemId::Init));
    }

    #[test]
    fn subsystem_id_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&SubsystemId::Asic).unwrap(),
            "\"asic\""
        );
        assert_eq!(
            serde_json::to_string(&SubsystemId::MiningApi).unwrap(),
            "\"mining_api\""
        );
        assert_eq!(
            serde_json::to_string(&SubsystemId::FleetInstaller).unwrap(),
            "\"fleet_installer\""
        );
    }

    #[test]
    fn orchestration_phase_round_trips_through_serde() {
        for phase in OrchestrationPhase::canonical_order() {
            let json = serde_json::to_string(&phase).unwrap();
            let back: OrchestrationPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(phase, back);
        }
    }

    #[test]
    fn subsystem_dependency_edge_serializes_explicit_field_names() {
        let edge = SubsystemDependency {
            subsystem: SubsystemId::Psu,
            requires: SubsystemId::Pic,
        };
        let json = serde_json::to_string(&edge).unwrap();
        assert!(json.contains("\"subsystem\":\"psu\""));
        assert!(json.contains("\"requires\":\"pic\""));
    }
}
