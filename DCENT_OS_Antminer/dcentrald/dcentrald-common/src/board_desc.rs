//! BoardDesc — declarative composition identity for a control-board target (ADR-0011).
//!
//! # Purpose
//!
//! New hardware support should be **data + thin backends**, not a new
//! `*_mining.rs` product. This module defines the host-safe, HAL-free shape of
//! that data so packaging, toolbox, TD-003 gates, and the daemon can eventually
//! share one registry.
//!
//! # Status (2026-07-11)
//!
//! Runtime adoption is deliberately narrow. The standard daemon binds an exact
//! registry row to its immutable platform-identity snapshot and uses typed
//! [`BoardFamily`] to reject a declared/observed control-board contradiction
//! before serialized-I2C construction. Main runtime dispatch also admits exact
//! transport/work-engine pairs before constructing a mining arm. Remaining
//! fields are migration scaffolding: none identify measured ASIC silicon or
//! select hashboard, PSU, cooling, storage, network, or complete-miner behavior.
//!
//! # Facets (see `docs/architecture/COMPOSITION_MODEL.md`)
//!
//! `BoardDesc` names the **control-board row** of the composition tables. ASIC
//! die, hashboard SKU, PSU, and cooling may be detected or profiled separately
//! and bound at bring-up time.

#![allow(dead_code)] // Scaffold: fields/enums reserved for migration consumers.

/// High-level SoC / carrier family (mirrors HAL `BoardType` names without
/// depending on `dcentrald-hal`, so Windows host tests stay clean).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoardFamily {
    /// Zynq-7000 am1/am2 class (S9, S17, S19, S19j Pro XIL).
    Zynq,
    /// TI AM335x BeagleBone class.
    BeagleBone,
    /// Amlogic A113D class.
    Amlogic,
    /// CVITEK CV183x class.
    Cvitek,
    /// STM32MP15 / BCB100 lab class.
    Stm32Mp15,
}

/// How the daemon talks to the ASIC chain (transport facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainTransportKind {
    /// Braiins-layout FPGA FIFO via UIO (`FpgaChain`).
    FpgaUio,
    /// AM2 hybrid: PL UART and/or FPGA work (recipe-selected).
    ZynqHybrid,
    /// Linux serial (`/dev/ttyS*` / `ttyO*`) NS16550-class.
    Serial,
    /// Bitmain stock `/dev/axi_fpga_dev` mmap path.
    StockFpga,
    /// CVITEK `uart_trans` kernel helper.
    UartTrans,
    /// Management-only / no chain open.
    None,
}

/// Where mining work is pushed (work-engine facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkEngineKind {
    FpgaWorkFifo,
    SerialWork,
    StockDma,
    /// API/dashboard only; hash boards not energized by this target default.
    ManagementOnly,
}

/// Voltage-controller class (power facet; protocol details live in asic crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoltageControllerClass {
    Pic16F1704,
    DsPic33Ep,
    Pic1704,
    /// Exact hardware identity proves no hashboard voltage MCU exists.
    NoPic,
    /// The control-board target alone cannot select a controller protocol.
    /// Runtime subtype/topology discovery must refine this before mutation.
    RuntimeDiscovered,
}

/// A/B or single-slot install policy (storage facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotPolicy {
    /// Dual-copy U-Boot env + inactive rootfs (classic Zynq DCENT sysupgrade).
    ZynqAbFwSetenv,
    /// Single eMMC/NAND slot — no A/B fallback (e.g. some CV paths).
    SingleSlot,
    /// SD-first / no trusted env map (BB empty fw_env).
    SdOnly,
    /// Lab / undocumented — refuse product install without override.
    LabGated,
}

/// Declarative control-board target description.
///
/// `board_target` should match `/etc/dcentos/board_target` and toolbox package
/// identity strings (e.g. `am2-s19j`, `am1-s9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardDesc {
    /// Canonical target id (`am2-s19j`, `am1-s9`, …).
    pub board_target: &'static str,
    /// Coarse SoC family.
    pub family: BoardFamily,
    /// Default chain transport for this target.
    pub chain_transport: ChainTransportKind,
    /// Default work engine.
    pub work_engine: WorkEngineKind,
    /// Informational controller expectation, never mutation authority.
    /// Use `RuntimeDiscovered` when the control-board target is insufficient.
    pub voltage_controller: VoltageControllerClass,
    /// Install / recovery slot policy.
    pub slot_policy: SlotPolicy,
    /// Whether public-beta product install is intended for this target.
    pub public_beta_install: bool,
    /// Whether mining is allowed to auto-start on a fresh image (usually false).
    pub mining_default_enabled: bool,
}

impl BoardDesc {
    /// Well-known beta-tier S9 Xilinx target (am1).
    pub const fn am1_s9() -> Self {
        Self {
            board_target: "am1-s9",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::FpgaUio,
            work_engine: WorkEngineKind::FpgaWorkFifo,
            voltage_controller: VoltageControllerClass::Pic16F1704,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            public_beta_install: true,
            mining_default_enabled: false,
        }
    }

    /// Well-known beta-tier S19j Pro Xilinx target (am2).
    pub const fn am2_s19jpro() -> Self {
        Self {
            board_target: "am2-s19j",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            public_beta_install: true,
            mining_default_enabled: false,
        }
    }

    /// AM3 BeagleBone S19j Pro — runtime mining proven; not public-beta install.
    pub const fn am3_bb_s19jpro() -> Self {
        Self {
            board_target: "am3-bb-s19jpro",
            family: BoardFamily::BeagleBone,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::SdOnly,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S21 — mining evidence exists; public install lab-gated (ADR-0002).
    pub const fn am3_s21() -> Self {
        Self {
            board_target: "am3-s21",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S21 Pro — runtime identity is subtype/topology discovered.
    pub const fn am3_s21pro() -> Self {
        Self {
            board_target: "am3-s21pro",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S21 XP — runtime identity is subtype/topology discovered.
    pub const fn am3_s21xp() -> Self {
        Self {
            board_target: "am3-s21xp",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic T21 — runtime identity is subtype/topology discovered.
    pub const fn am3_t21() -> Self {
        Self {
            board_target: "am3-t21",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19k Pro — bring-up / evidence-gap install.
    pub const fn am3_s19kpro() -> Self {
        Self {
            board_target: "am3-s19kpro",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19 XP (BM1366 class) — bring-up; not public-beta install.
    pub const fn am3_s19xp() -> Self {
        Self {
            board_target: "am3-s19xp",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19j Pro — code paths present; not public-beta install.
    pub const fn am3_s19jpro_aml() -> Self {
        Self {
            board_target: "am3-s19jpro-aml",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// CVITEK CV1835 S19j Pro class — single-slot, lab-gated.
    pub const fn cv1835_s19jpro() -> Self {
        Self {
            board_target: "cv1835-s19jpro",
            family: BoardFamily::Cvitek,
            chain_transport: ChainTransportKind::UartTrans,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::SingleSlot,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Zynq S19 Pro AM2 — experimental / identity-gated (TD-003/TD-016 class).
    pub const fn am2_s19pro() -> Self {
        Self {
            board_target: "am2-s19pro",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::SerialWork,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// TD-003 scaffolding: S17 family — management-only until promotion.
    pub const fn am2_s17() -> Self {
        Self {
            board_target: "am2-s17p",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// TD-003 scaffolding: T19 — management-only until promotion.
    pub const fn am2_t19() -> Self {
        Self {
            board_target: "am2-t19",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Whether product install (signed public beta package) is appropriate.
    ///
    /// Distinct from TD-003 mining-enable gates: a target may allow lab mining
    /// evidence without being public-beta install ready.
    pub fn is_public_beta_install_target(board_target: &str) -> bool {
        Self::lookup(board_target)
            .map(|d| d.public_beta_install)
            .unwrap_or(false)
    }

    /// Lookup a static desc by `board_target` string (trim-sensitive exact match).
    ///
    /// Returns `None` for unknown targets — callers must fail closed or use
    /// management-only defaults (ADR-0002 scaffolding rules).
    pub fn lookup(board_target: &str) -> Option<&'static BoardDesc> {
        Self::all_registered()
            .iter()
            .find(|d| d.board_target == board_target)
    }

    /// All registered descriptors (for matrix generators / tests).
    ///
    /// Grow this static list when adding targets — not a new `*_mining.rs`.
    pub fn all_registered() -> &'static [BoardDesc] {
        static REGISTRY: &[BoardDesc] = &[
            BoardDesc::am1_s9(),
            BoardDesc::am2_s19jpro(),
            BoardDesc::am2_s19pro(),
            BoardDesc::am2_s17(),
            BoardDesc::am2_t19(),
            BoardDesc::am3_bb_s19jpro(),
            BoardDesc::am3_s21(),
            BoardDesc::am3_s21pro(),
            BoardDesc::am3_s21xp(),
            BoardDesc::am3_t21(),
            BoardDesc::am3_s19kpro(),
            BoardDesc::am3_s19xp(),
            BoardDesc::am3_s19jpro_aml(),
            BoardDesc::cv1835_s19jpro(),
        ];
        REGISTRY
    }

    /// Whether this target is allowed to auto-route product install without lab overrides.
    pub fn product_install_allowed(&self) -> bool {
        self.public_beta_install
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn collect_overlay_board_targets(dir: &Path, targets: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read board overlay tree") {
            let entry = entry.expect("read board overlay entry");
            let file_type = entry.file_type().expect("read overlay entry type");
            if file_type.is_dir() {
                collect_overlay_board_targets(&entry.path(), targets);
            } else if file_type.is_file() && entry.file_name() == "board_target" {
                let target = std::fs::read_to_string(entry.path())
                    .expect("read board_target marker")
                    .trim()
                    .to_string();
                assert!(!target.is_empty(), "board_target marker must not be empty");
                targets.push(target);
            }
        }
    }

    #[test]
    fn beta_targets_are_registered() {
        let s9 = BoardDesc::lookup("am1-s9").expect("am1-s9");
        assert!(s9.public_beta_install);
        assert!(!s9.mining_default_enabled);
        assert_eq!(s9.family, BoardFamily::Zynq);
        assert_eq!(s9.chain_transport, ChainTransportKind::FpgaUio);

        let j = BoardDesc::lookup("am2-s19j").expect("am2-s19j");
        assert!(j.public_beta_install);
        assert_eq!(j.chain_transport, ChainTransportKind::ZynqHybrid);
        assert_eq!(j.work_engine, WorkEngineKind::SerialWork);
        assert_eq!(j.voltage_controller, VoltageControllerClass::DsPic33Ep);
    }

    #[test]
    fn unknown_target_is_none() {
        assert!(BoardDesc::lookup("am2-not-a-real-sku").is_none());
        assert!(BoardDesc::lookup("").is_none());
    }

    #[test]
    fn every_shipped_overlay_board_target_has_an_exact_descriptor() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("dcentrald-common lives under DCENT_OS_Antminer/dcentrald");
        let mut targets = Vec::new();
        collect_overlay_board_targets(
            &project_root.join("br2_external_dcentos/board"),
            &mut targets,
        );
        targets.sort();
        targets.dedup();
        assert!(!targets.is_empty(), "expected shipped board_target markers");
        for target in targets {
            assert!(
                BoardDesc::lookup(&target).is_some(),
                "shipped overlay board_target {target:?} is absent from BoardDesc"
            );
        }
    }

    #[test]
    fn beta_targets_prefer_ab_fw_setenv() {
        assert_eq!(BoardDesc::am1_s9().slot_policy, SlotPolicy::ZynqAbFwSetenv);
        assert_eq!(
            BoardDesc::am2_s19jpro().slot_policy,
            SlotPolicy::ZynqAbFwSetenv
        );
    }

    #[test]
    fn only_beta_flags_match_public_beta_gate_story() {
        let beta: Vec<_> = BoardDesc::all_registered()
            .iter()
            .filter(|d| d.public_beta_install)
            .map(|d| d.board_target)
            .collect();
        assert_eq!(beta, vec!["am1-s9", "am2-s19j"]);
    }

    #[test]
    fn bb_is_sd_only_not_public_beta() {
        let bb = BoardDesc::lookup("am3-bb-s19jpro").expect("bb");
        assert!(!bb.public_beta_install);
        assert_eq!(bb.slot_policy, SlotPolicy::SdOnly);
        assert_eq!(bb.family, BoardFamily::BeagleBone);
    }

    #[test]
    fn amlogic_targets_are_runtime_discovered_serial_lab_gated() {
        for id in [
            "am3-s21",
            "am3-s21pro",
            "am3-s21xp",
            "am3-t21",
            "am3-s19kpro",
            "am3-s19xp",
            "am3-s19jpro-aml",
        ] {
            let d = BoardDesc::lookup(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(d.family, BoardFamily::Amlogic, "{id}");
            assert_eq!(d.chain_transport, ChainTransportKind::Serial, "{id}");
            assert_eq!(
                d.voltage_controller,
                VoltageControllerClass::RuntimeDiscovered,
                "{id}"
            );
            assert_eq!(d.slot_policy, SlotPolicy::LabGated, "{id}");
            assert!(!d.product_install_allowed(), "{id}");
        }
    }

    #[test]
    fn cvitek_is_single_slot_and_requires_runtime_controller_discovery() {
        let d = BoardDesc::lookup("cv1835-s19jpro").expect("cv");
        assert_eq!(d.family, BoardFamily::Cvitek);
        assert_eq!(d.slot_policy, SlotPolicy::SingleSlot);
        assert_eq!(
            d.voltage_controller,
            VoltageControllerClass::RuntimeDiscovered
        );
        assert_eq!(d.chain_transport, ChainTransportKind::UartTrans);
    }

    #[test]
    fn registry_has_no_duplicate_targets() {
        let ids: Vec<_> = BoardDesc::all_registered()
            .iter()
            .map(|d| d.board_target)
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            ids.len(),
            sorted.len(),
            "duplicate board_target in registry"
        );
    }
}
