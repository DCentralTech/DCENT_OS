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
//! transport/work-engine pairs before constructing a mining arm. The declared
//! ASIC protocol is an admission constraint, not measured-silicon evidence:
//! mutation-capable engines must bind it to configured or observed identity.
//! Remaining fields are migration scaffolding and do not select hashboard, PSU,
//! cooling, network, or complete-miner behavior.
//!
//! # Facets (see `docs/architecture/COMPOSITION_MODEL.md`)
//!
//! `BoardDesc` names a packaged target-composition row. ASIC identity,
//! hashboard SKU, PSU, and cooling are still detected or profiled separately
//! and bound at bring-up time; a target's protocol declaration only narrows
//! what a runtime is permitted to attempt.

#![allow(dead_code)] // Scaffold: fields/enums reserved for migration consumers.

use dcent_schema::hardware::{
    ArtifactKind, ArtifactMaturity, HardwareEnablementPolicy, ImplementationMaturity,
    InstallAuthorization, RecoveryMaturity, StorageTopology, UpdateMechanism,
};

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

impl BoardFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zynq => "zynq",
            Self::BeagleBone => "beaglebone",
            Self::Amlogic => "amlogic",
            Self::Cvitek => "cvitek",
            Self::Stm32Mp15 => "stm32mp15",
        }
    }
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

impl ChainTransportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FpgaUio => "fpga_uio",
            Self::ZynqHybrid => "zynq_hybrid",
            Self::Serial => "serial",
            Self::StockFpga => "stock_fpga",
            Self::UartTrans => "uart_trans",
            Self::None => "none",
        }
    }
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

/// ASIC wire-protocol identity admitted by a composed board target.
///
/// This is the protocol family a mining engine is allowed to speak, not proof
/// that silicon was observed at runtime.  Mutation-capable constructors must
/// bind this declared identity to independently configured or discovered ASIC
/// evidence before they can open a chain.  Keeping it separate from transport
/// prevents a shared UART/FPGA carrier from accidentally authorizing a
/// different chip family's register map or work codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsicProtocolIdentity {
    Bm1387,
    Bm1396,
    Bm1397,
    Bm1398,
    Bm1362,
    Bm1366,
    Bm1368,
    Bm1370,
    /// The target marker is insufficient; passive/runtime identity evidence
    /// must select the protocol before any ASIC mutation.
    RuntimeDiscovered,
}

impl AsicProtocolIdentity {
    /// Map the canonical numeric ChipID used by ASIC drivers and configuration
    /// into the protocol identity consumed by runtime admission.
    pub const fn from_chip_id(chip_id: u16) -> Option<Self> {
        match chip_id {
            0x1387 => Some(Self::Bm1387),
            0x1396 => Some(Self::Bm1396),
            0x1397 => Some(Self::Bm1397),
            0x1398 => Some(Self::Bm1398),
            0x1362 => Some(Self::Bm1362),
            0x1366 => Some(Self::Bm1366),
            0x1368 => Some(Self::Bm1368),
            0x1370 => Some(Self::Bm1370),
            _ => None,
        }
    }

    /// Parse the canonical `BMxxxx` label used by configuration and hardware
    /// identity snapshots. Unknown labels remain unknown instead of being
    /// coerced to a nearby protocol family.
    pub fn from_chip_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_uppercase().as_str() {
            "BM1387" => Some(Self::Bm1387),
            "BM1396" => Some(Self::Bm1396),
            "BM1397" => Some(Self::Bm1397),
            "BM1398" => Some(Self::Bm1398),
            "BM1362" => Some(Self::Bm1362),
            "BM1366" => Some(Self::Bm1366),
            "BM1368" => Some(Self::Bm1368),
            "BM1370" => Some(Self::Bm1370),
            _ => None,
        }
    }
}

/// Proof that a board composition and independent runtime identity evidence
/// agree on one exact ASIC protocol.
///
/// The field is private so mutation-capable engines cannot mint the proof from
/// an enum literal.  Proofs are created only by [`BoardDesc::admit_asic_protocol`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsicProtocolAdmission {
    identity: AsicProtocolIdentity,
}

impl AsicProtocolAdmission {
    pub const fn identity(self) -> AsicProtocolIdentity {
        self.identity
    }
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

/// Legacy combined slot/update policy.
///
/// New authorization or update decisions MUST use [`BoardDesc::enablement`].
/// This enum remains temporarily for runtime-dispatch compatibility while
/// those call sites migrate; notably, `LabGated` is not a storage topology.
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

const ZYNQ_PUBLIC_UPDATE_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::RedundantSlots,
    update_mechanism: UpdateMechanism::ZynqUbiFwSetenv,
    update_maturity: ImplementationMaturity::Experimental,
    install_authorization: InstallAuthorization::PublicBeta,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::SysupgradeBundle,
    artifact_maturity: ArtifactMaturity::Experimental,
};

const ZYNQ_LAB_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::RedundantSlots,
    update_mechanism: UpdateMechanism::ZynqUbiFwSetenv,
    update_maturity: ImplementationMaturity::Experimental,
    install_authorization: InstallAuthorization::LabOnly,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::SysupgradeBundle,
    artifact_maturity: ArtifactMaturity::Experimental,
};

const ZYNQ_RUNTIME_ONLY_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::RedundantSlots,
    update_mechanism: UpdateMechanism::ZynqUbiFwSetenv,
    update_maturity: ImplementationMaturity::NotImplemented,
    install_authorization: InstallAuthorization::Denied,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::None,
    artifact_maturity: ArtifactMaturity::NotImplemented,
};

const AMLOGIC_LAB_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::SingleSlot,
    update_mechanism: UpdateMechanism::HostRootfsWindow,
    update_maturity: ImplementationMaturity::Experimental,
    install_authorization: InstallAuthorization::LabOnly,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::SysupgradeBundle,
    artifact_maturity: ArtifactMaturity::Experimental,
};

const AMLOGIC_RUNTIME_ONLY_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::SingleSlot,
    update_mechanism: UpdateMechanism::HostRootfsWindow,
    update_maturity: ImplementationMaturity::NotImplemented,
    install_authorization: InstallAuthorization::Denied,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::None,
    artifact_maturity: ArtifactMaturity::NotImplemented,
};

const BEAGLEBONE_SD_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::ExternalMediaOnly,
    update_mechanism: UpdateMechanism::SdImage,
    update_maturity: ImplementationMaturity::NotImplemented,
    install_authorization: InstallAuthorization::LabOnly,
    recovery_maturity: RecoveryMaturity::EvidenceOnly,
    artifact_kind: ArtifactKind::SdCardPayload,
    artifact_maturity: ArtifactMaturity::Experimental,
};

const CV1835_EVIDENCE_ONLY_ENABLEMENT: HardwareEnablementPolicy = HardwareEnablementPolicy {
    storage_topology: StorageTopology::SingleSlot,
    update_mechanism: UpdateMechanism::EmmcContentSelectorEvidenceOnly,
    update_maturity: ImplementationMaturity::NotImplemented,
    install_authorization: InstallAuthorization::Denied,
    recovery_maturity: RecoveryMaturity::NotImplemented,
    artifact_kind: ArtifactKind::None,
    artifact_maturity: ArtifactMaturity::NotImplemented,
};

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
    /// ASIC protocol expected by this complete target composition.  This is an
    /// admission constraint only; runtime evidence still has to agree.
    pub asic_protocol: AsicProtocolIdentity,
    /// Informational controller expectation, never mutation authority.
    /// Use `RuntimeDiscovered` when the control-board target is insufficient.
    pub voltage_controller: VoltageControllerClass,
    /// Install / recovery slot policy.
    pub slot_policy: SlotPolicy,
    /// Independent storage, artifact, maturity, authorization, and recovery
    /// facets. This is the authoritative install/update contract.
    pub enablement: HardwareEnablementPolicy,
    /// Whether a public-beta package may be used for first install from a
    /// non-DCENT_OS source. This is intentionally narrower than
    /// `enablement.install_authorization`, which also governs self-update on an
    /// already-running DCENT_OS target.
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
            asic_protocol: AsicProtocolIdentity::Bm1387,
            voltage_controller: VoltageControllerClass::Pic16F1704,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_PUBLIC_UPDATE_ENABLEMENT,
            public_beta_install: true,
            mining_default_enabled: false,
        }
    }

    /// S19j Pro Xilinx target (am2): public-beta self-update, but no
    /// vendor-source first-install capsule.
    pub const fn am2_s19jpro() -> Self {
        Self {
            board_target: "am2-s19j",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::SerialWork,
            asic_protocol: AsicProtocolIdentity::Bm1362,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_PUBLIC_UPDATE_ENABLEMENT,
            public_beta_install: false,
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
            asic_protocol: AsicProtocolIdentity::Bm1362,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::SdOnly,
            enablement: BEAGLEBONE_SD_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Generic AM3 BeagleBone image — management only without exact carrier proof.
    pub const fn am3_bb() -> Self {
        Self {
            board_target: "am3-bb",
            family: BoardFamily::BeagleBone,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1362,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::SdOnly,
            enablement: BEAGLEBONE_SD_ENABLEMENT,
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
            asic_protocol: AsicProtocolIdentity::Bm1368,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
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
            asic_protocol: AsicProtocolIdentity::Bm1370,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
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
            asic_protocol: AsicProtocolIdentity::Bm1370,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic T21 — complete target uses BM1368; controller remains runtime-discovered.
    pub const fn am3_t21() -> Self {
        Self {
            board_target: "am3-t21",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            asic_protocol: AsicProtocolIdentity::Bm1368,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19k Pro — runtime board target is the stock-compatible `am3-s19k`.
    pub const fn am3_s19kpro() -> Self {
        Self {
            board_target: "am3-s19k",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            asic_protocol: AsicProtocolIdentity::Bm1366,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19 XP (BM1366 class) — metadata/runtime only; no artifact lane.
    pub const fn am3_s19xp() -> Self {
        Self {
            board_target: "am3-s19xp",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::SerialWork,
            asic_protocol: AsicProtocolIdentity::Bm1366,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_RUNTIME_ONLY_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// Amlogic S19j Pro — dedicated controller profile is not implemented.
    pub const fn am3_s19jpro_aml() -> Self {
        Self {
            board_target: "am3-s19jpro-aml",
            family: BoardFamily::Amlogic,
            chain_transport: ChainTransportKind::Serial,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1362,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: AMLOGIC_LAB_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// CVITEK CV1835 S19j Pro identity — evidence-only, with no artifact or runtime lane.
    pub const fn cv1835_s19jpro() -> Self {
        Self {
            board_target: "cv1835-s19jpro",
            family: BoardFamily::Cvitek,
            chain_transport: ChainTransportKind::UartTrans,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1362,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::SingleSlot,
            enablement: CV1835_EVIDENCE_ONLY_ENABLEMENT,
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
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1398,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_LAB_ENABLEMENT,
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
            asic_protocol: AsicProtocolIdentity::Bm1397,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_LAB_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// S17+ exact composition; management-only until PIC16/BM1396 bench admission.
    pub const fn am2_s17plus() -> Self {
        Self {
            board_target: "am2-s17plus",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1396,
            voltage_controller: VoltageControllerClass::Pic16F1704,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_RUNTIME_ONLY_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// T17 exact composition; management-only until PIC16/BM1397 bench admission.
    pub const fn am2_t17() -> Self {
        Self {
            board_target: "am2-t17",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1397,
            voltage_controller: VoltageControllerClass::Pic16F1704,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_RUNTIME_ONLY_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// T17+ exact composition; management-only until PIC16/BM1396 bench admission.
    pub const fn am2_t17plus() -> Self {
        Self {
            board_target: "am2-t17plus",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1396,
            voltage_controller: VoltageControllerClass::Pic16F1704,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_RUNTIME_ONLY_ENABLEMENT,
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    /// TD-003 scaffolding: T19 — management-only, with no artifact producer.
    pub const fn am2_t19() -> Self {
        Self {
            board_target: "am2-t19",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::ZynqHybrid,
            work_engine: WorkEngineKind::ManagementOnly,
            asic_protocol: AsicProtocolIdentity::Bm1398,
            voltage_controller: VoltageControllerClass::DsPic33Ep,
            slot_policy: SlotPolicy::ZynqAbFwSetenv,
            enablement: ZYNQ_RUNTIME_ONLY_ENABLEMENT,
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

    /// Bind the protocol declared by this target to independent runtime ASIC
    /// evidence and return a non-forgeable admission proof.
    pub fn admit_asic_protocol(
        &self,
        configured_or_observed: Option<AsicProtocolIdentity>,
        required: AsicProtocolIdentity,
    ) -> Result<AsicProtocolAdmission, String> {
        if self.asic_protocol != required {
            return Err(format!(
                "BoardDesc {} declares ASIC protocol {:?}, incompatible with engine requiring {:?}",
                self.board_target, self.asic_protocol, required
            ));
        }
        if configured_or_observed != Some(required) {
            return Err(format!(
                "BoardDesc {} requires exact {:?} runtime ASIC evidence, got {:?}",
                self.board_target, required, configured_or_observed
            ));
        }
        Ok(AsicProtocolAdmission { identity: required })
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
            BoardDesc::am2_s17plus(),
            BoardDesc::am2_t17(),
            BoardDesc::am2_t17plus(),
            BoardDesc::am2_t19(),
            BoardDesc::am3_bb(),
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

    /// Whether this target is allowed to auto-route first install from a
    /// non-DCENT_OS source without lab overrides.
    pub fn product_install_allowed(&self) -> bool {
        self.public_beta_install
            && matches!(
                self.enablement.install_authorization,
                InstallAuthorization::PublicBeta | InstallAuthorization::Production
            )
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
            } else if file_type.is_file() && entry.file_name() == "post-build.sh" {
                let post_build =
                    std::fs::read_to_string(entry.path()).expect("read post-build identity stamps");
                for line in post_build.lines().map(str::trim) {
                    if !line.starts_with("echo \"")
                        || !line.contains("${TARGET_DIR}/etc/dcentos/board_target")
                    {
                        continue;
                    }
                    let target = line
                        .strip_prefix("echo \"")
                        .and_then(|rest| rest.split('"').next())
                        .expect("parse post-build board_target stamp");
                    assert!(
                        !target.is_empty(),
                        "post-build board target must not be empty"
                    );
                    targets.push(target.to_string());
                }
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
        assert!(!j.public_beta_install);
        assert!(!j.product_install_allowed());
        assert_eq!(
            j.enablement.install_authorization,
            InstallAuthorization::PublicBeta
        );
        assert!(j.enablement.allows_persistent_update());
        assert_eq!(j.chain_transport, ChainTransportKind::ZynqHybrid);
        assert_eq!(j.work_engine, WorkEngineKind::SerialWork);
        assert_eq!(j.asic_protocol, AsicProtocolIdentity::Bm1362);
        assert_eq!(j.voltage_controller, VoltageControllerClass::DsPic33Ep);
    }

    #[test]
    fn unknown_target_is_none() {
        assert!(BoardDesc::lookup("am2-not-a-real-sku").is_none());
        assert!(BoardDesc::lookup("").is_none());
    }

    #[test]
    fn every_shipped_board_target_stamp_has_an_exact_descriptor() {
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
    fn s19k_overlay_post_build_and_descriptor_share_runtime_identity() {
        let descriptor = BoardDesc::lookup("am3-s19k").expect("registered S19k runtime target");
        assert_eq!(descriptor, &BoardDesc::am3_s19kpro());
        assert!(
            BoardDesc::lookup("am3-s19kpro").is_none(),
            "build-lane name must not masquerade as the runtime board target"
        );

        let post_build =
            include_str!("../../../br2_external_dcentos/board/amlogic/am3-s19kpro/post-build.sh");
        let stamps: Vec<_> = post_build
            .lines()
            .filter(|line| line.contains("${TARGET_DIR}/etc/dcentos/board_target"))
            .collect();
        assert_eq!(
            stamps.len(),
            1,
            "S19k post-build must stamp one unambiguous runtime board target"
        );
        assert!(
            stamps[0].contains(r#"echo "am3-s19k""#),
            "S19k post-build target drifted from BoardDesc: {}",
            stamps[0]
        );
    }

    #[test]
    fn generic_bb_post_build_has_a_management_only_descriptor() {
        let descriptor = BoardDesc::lookup("am3-bb").expect("registered generic BB target");
        assert_eq!(descriptor.family, BoardFamily::BeagleBone);
        assert_eq!(descriptor.work_engine, WorkEngineKind::ManagementOnly);
        assert_eq!(descriptor.asic_protocol, AsicProtocolIdentity::Bm1362);

        let post_build =
            include_str!("../../../br2_external_dcentos/board/beaglebone/am3-bb/post-build.sh");
        let stamps: Vec<_> = post_build
            .lines()
            .filter(|line| line.contains("${TARGET_DIR}/etc/dcentos/board_target"))
            .collect();
        assert_eq!(stamps.len(), 1, "generic BB must stamp one board target");
        assert!(
            stamps[0].contains(r#"echo "am3-bb""#),
            "generic BB post-build target drifted from BoardDesc: {}",
            stamps[0]
        );
    }

    #[test]
    fn shipped_serial_configs_match_descriptor_asic_identity() {
        let shipped_configs = [
            (
                "am2-s19j",
                include_str!("../../configs/dcentrald_s19jpro_am2_baked_default.toml"),
            ),
            (
                "am2-s19pro",
                include_str!("../../configs/dcentrald_s19pro_am2_baked_default.toml"),
            ),
            (
                "am2-s17p",
                include_str!("../../configs/dcentrald_s17pro_am2_baked_default.toml"),
            ),
            (
                "am3-bb",
                include_str!("../../../br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-bb-s19jpro",
                include_str!("../../../br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-s21",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-s21/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-s21pro",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-s21pro/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-s21xp",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-s21xp/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-t21",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-t21/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-s19k",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-s19kpro/rootfs-overlay/etc/dcentrald.toml"),
            ),
            (
                "am3-s19jpro-aml",
                include_str!("../../../br2_external_dcentos/board/amlogic/am3-s19jpro-aml/rootfs-overlay/etc/dcentrald.toml"),
            ),
        ];

        for (board_target, config) in shipped_configs {
            let descriptor =
                BoardDesc::lookup(board_target).expect("shipped config target is registered");
            let configured_protocols: Vec<_> = config
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    if line.starts_with('#') {
                        return None;
                    }
                    let (key, value) = line.split_once('=')?;
                    if !matches!(key.trim(), "chip" | "serial_chip_type") {
                        return None;
                    }
                    let label = value.trim().trim_matches('"');
                    Some(
                        AsicProtocolIdentity::from_chip_label(label).unwrap_or_else(|| {
                            panic!("{board_target} shipped config has unknown ASIC label {label}")
                        }),
                    )
                })
                .collect();
            assert!(
                !configured_protocols.is_empty(),
                "{board_target} shipped config has no exact ASIC identity"
            );
            assert!(
                configured_protocols
                    .iter()
                    .all(|configured| *configured == descriptor.asic_protocol),
                "{board_target} shipped config ASIC identity {:?} disagrees with descriptor {:?}",
                configured_protocols,
                descriptor.asic_protocol
            );
        }

        let acceptance_skus = include_str!("../../../scripts/hw-acceptance/skus.conf");
        assert!(
            acceptance_skus
                .lines()
                .any(|line| line.starts_with("T21|am3-t21|aarch64|BM1368|0x1368|")),
            "T21 acceptance inventory must share the descriptor ASIC identity"
        );
    }

    #[test]
    fn acceptance_inventory_matches_registered_asic_protocols() {
        let acceptance_skus = include_str!("../../../scripts/hw-acceptance/skus.conf");
        let mut validated_rows = 0;
        for line in acceptance_skus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<_> = line.split('|').collect();
            assert_eq!(fields.len(), 11, "malformed acceptance row: {line}");
            let runtime_target = match fields[1] {
                "am2-s19jpro-zynq" | "am2-s19jpro" => "am2-s19j",
                "am2-s19" => "am2-s19pro",
                target => target,
            };
            let descriptor = match BoardDesc::lookup(runtime_target) {
                Some(descriptor) => descriptor,
                None if fields[8] == "NOT-IMPLEMENTED" => continue,
                None => panic!(
                    "{} is {} but has no registered BoardDesc for {}",
                    fields[0], fields[8], runtime_target
                ),
            };

            let configured_protocol = AsicProtocolIdentity::from_chip_label(fields[3])
                .unwrap_or_else(|| panic!("{} has unknown ASIC label {}", fields[0], fields[3]));
            let chip_id_text = fields[4]
                .strip_prefix("0x")
                .unwrap_or_else(|| panic!("{} has no exact ChipID", fields[0]));
            let chip_id = u16::from_str_radix(chip_id_text, 16)
                .unwrap_or_else(|_| panic!("{} has malformed ChipID {}", fields[0], fields[4]));
            assert_eq!(
                AsicProtocolIdentity::from_chip_id(chip_id),
                Some(configured_protocol),
                "{} acceptance ASIC label/ChipID mismatch",
                fields[0]
            );
            assert_eq!(
                descriptor.asic_protocol, configured_protocol,
                "{} acceptance ASIC identity disagrees with BoardDesc {}",
                fields[0], runtime_target
            );
            validated_rows += 1;
        }
        assert_eq!(
            validated_rows, 16,
            "registered acceptance coverage changed; classify new aliases explicitly"
        );
    }

    #[test]
    fn every_claimed_artifact_has_a_primary_build_driver_lane() {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("dcentrald-common lives under DCENT_OS_Antminer/dcentrald");
        let build_driver =
            include_str!("../../../scripts/build_in_docker.sh").replace("\r\n", "\n");
        let package_validation_marker = build_driver
            .find("echo \"Package-only validation:\"")
            .expect("package-only validation marker");
        let package_validation_case_start = build_driver[..package_validation_marker]
            .rfind("case ")
            .expect("package-only validation case");
        let package_validation_case =
            &build_driver[package_validation_case_start..package_validation_marker];
        let non_s9_inventory = include_str!("../../../scripts/rebuild_all_non_s9.sh");
        for producer in crate::artifact_producer::PRIMARY_ARTIFACT_PRODUCERS {
            let runtime_target = producer.board_target;
            let build_target = producer.build_target;
            let defconfig = producer.defconfig;
            let overlay = producer.overlay;
            let package_validated = producer.package_validated;
            let tarball = producer.artifact_filename;
            let arm = format!("\n    {build_target})");
            let arm_start = build_driver.find(&arm).unwrap_or_else(|| {
                panic!(
                    "{runtime_target} claims an artifact but build target {build_target} is absent"
                )
            });
            let arm_body = &build_driver[arm_start + arm.len()..];
            let arm_end = arm_body
                .find("\n        ;;")
                .unwrap_or_else(|| panic!("unterminated build target arm {build_target}"));
            let arm_body = &arm_body[..arm_end];
            assert!(
                arm_body.contains(&format!(r#"BR_DEFCONFIG="{defconfig}""#)),
                "{build_target} does not select {defconfig}"
            );
            assert!(
                arm_body.contains(&format!(r#"BOARD_PKG_NAME="{runtime_target}""#)),
                "{build_target} does not package canonical runtime target {runtime_target}"
            );
            assert!(
                project_root
                    .join("br2_external_dcentos/configs")
                    .join(defconfig)
                    .is_file(),
                "{build_target} references missing defconfig {defconfig}"
            );
            let version_overlay =
                format!("/build/dcentos/br2_external_dcentos/{overlay}/rootfs-overlay");
            assert!(
                build_driver.matches(&version_overlay).count() >= 2,
                "{build_target} must synchronize and then verify {overlay}/rootfs-overlay"
            );
            if package_validated {
                assert!(
                    package_validation_case.contains(&format!("|{build_target}|"))
                        || package_validation_case.contains(&format!("|{build_target})"))
                        || package_validation_case.contains(&format!("    {build_target}|"))
                        || package_validation_case.contains(&format!("    {build_target})")),
                    "{build_target} artifact bypasses package-only validation"
                );
            }
            if build_target != "s9" {
                assert!(
                    non_s9_inventory.contains(&format!("\n    {build_target}\n")),
                    "{build_target} is absent from the fail-closed non-S9 inventory"
                );
                assert!(
                    non_s9_inventory.contains(&format!(r#"[{build_target}]="{tarball}""#)),
                    "{build_target} inventory mapping does not name {tarball}"
                );
            }
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
        assert_eq!(beta, vec!["am1-s9"]);
    }

    #[test]
    fn bb_is_sd_only_not_public_beta() {
        for id in ["am3-bb", "am3-bb-s19jpro"] {
            let bb = BoardDesc::lookup(id).unwrap_or_else(|| panic!("missing {id}"));
            assert!(!bb.public_beta_install);
            assert_eq!(bb.slot_policy, SlotPolicy::SdOnly);
            assert_eq!(bb.family, BoardFamily::BeagleBone);
        }
    }

    #[test]
    fn amlogic_targets_are_runtime_discovered_serial_lab_gated() {
        for id in [
            "am3-s21",
            "am3-s21pro",
            "am3-s21xp",
            "am3-t21",
            "am3-s19k",
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
    fn unimplemented_runtime_targets_are_management_only() {
        for id in [
            "am2-s17plus",
            "am2-t17",
            "am2-t17plus",
            "am3-bb",
            "am3-s19jpro-aml",
            "cv1835-s19jpro",
        ] {
            let descriptor = BoardDesc::lookup(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(
                descriptor.work_engine,
                WorkEngineKind::ManagementOnly,
                "{id} must be rejected before a hardware-owning runtime is constructed"
            );
        }
    }

    #[test]
    fn cvitek_is_single_slot_and_requires_runtime_controller_discovery() {
        let d = BoardDesc::lookup("cv1835-s19jpro").expect("cv");
        assert_eq!(d.family, BoardFamily::Cvitek);
        assert_eq!(d.slot_policy, SlotPolicy::SingleSlot);
        assert_eq!(d.enablement.storage_topology, StorageTopology::SingleSlot);
        assert_eq!(
            d.enablement.update_mechanism,
            UpdateMechanism::EmmcContentSelectorEvidenceOnly
        );
        assert_eq!(
            d.enablement.update_maturity,
            ImplementationMaturity::NotImplemented
        );
        assert_eq!(
            d.enablement.install_authorization,
            InstallAuthorization::Denied
        );
        assert_eq!(d.enablement.artifact_kind, ArtifactKind::None);
        assert_eq!(
            d.enablement.artifact_maturity,
            ArtifactMaturity::NotImplemented
        );
        assert_eq!(
            d.enablement.recovery_maturity,
            RecoveryMaturity::NotImplemented
        );
        assert!(!d.enablement.allows_persistent_update());
        assert!(!d.enablement.allows_restore());
        assert_eq!(
            d.voltage_controller,
            VoltageControllerClass::RuntimeDiscovered
        );
        assert_eq!(d.chain_transport, ChainTransportKind::UartTrans);
        assert_eq!(d.work_engine, WorkEngineKind::ManagementOnly);
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

    #[test]
    fn registry_artifact_contracts_are_consistent() {
        for board in BoardDesc::all_registered() {
            assert!(
                board.enablement.artifact_contract_is_consistent(),
                "{} has contradictory artifact kind/maturity",
                board.board_target
            );
        }
    }

    #[test]
    fn protocol_admission_requires_declared_and_runtime_identity_to_match() {
        let s19j = BoardDesc::am2_s19jpro();
        let proof = s19j
            .admit_asic_protocol(
                Some(AsicProtocolIdentity::Bm1362),
                AsicProtocolIdentity::Bm1362,
            )
            .expect("exact BM1362 composition should admit");
        assert_eq!(proof.identity(), AsicProtocolIdentity::Bm1362);
        assert!(s19j
            .admit_asic_protocol(None, AsicProtocolIdentity::Bm1362)
            .is_err());

        let s19pro = BoardDesc::am2_s19pro();
        assert_eq!(s19pro.asic_protocol, AsicProtocolIdentity::Bm1398);
        assert_eq!(s19pro.work_engine, WorkEngineKind::ManagementOnly);
        assert!(s19pro
            .admit_asic_protocol(
                Some(AsicProtocolIdentity::Bm1398),
                AsicProtocolIdentity::Bm1362,
            )
            .is_err());
    }

    #[test]
    fn protocol_identity_parsers_refuse_unknown_or_ambiguous_labels() {
        assert_eq!(
            AsicProtocolIdentity::from_chip_id(0x1398),
            Some(AsicProtocolIdentity::Bm1398)
        );
        assert_eq!(
            AsicProtocolIdentity::from_chip_id(0x1396),
            Some(AsicProtocolIdentity::Bm1396)
        );
        assert_eq!(
            AsicProtocolIdentity::from_chip_label("BM1396"),
            Some(AsicProtocolIdentity::Bm1396)
        );
        assert_eq!(
            AsicProtocolIdentity::from_chip_label(" bm1362 "),
            Some(AsicProtocolIdentity::Bm1362)
        );
        assert_eq!(AsicProtocolIdentity::from_chip_id(0x1390), None);
        assert_eq!(AsicProtocolIdentity::from_chip_label("BM13XX"), None);
    }
}
