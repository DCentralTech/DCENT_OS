use serde::{Deserialize, Serialize};

/// Version of the cross-firmware hardware-enablement policy contract.
///
/// This contract separates physical topology from implementation maturity and
/// operator authorization.  A single-slot device is not thereby installable,
/// and an experimental artifact is not thereby a persistent update.
/// Version 2 adds explicit absent-artifact wire values (`none` and
/// `not_implemented`). Version 3 separates public first-install eligibility
/// from the broader install authorization used by an already-running target's
/// persistent-update API; strict older consumers must reject rather than
/// silently reinterpret either change.
pub const HARDWARE_ENABLEMENT_SCHEMA_VERSION: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageTopology {
    #[serde(rename = "redundant_slots")]
    RedundantSlots,
    #[serde(rename = "single_slot")]
    SingleSlot,
    #[serde(rename = "external_media_only")]
    ExternalMediaOnly,
    #[serde(rename = "unknown")]
    Unknown,
}

impl StorageTopology {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RedundantSlots => "redundant_slots",
            Self::SingleSlot => "single_slot",
            Self::ExternalMediaOnly => "external_media_only",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpdateMechanism {
    #[serde(rename = "zynq_ubi_fw_setenv")]
    ZynqUbiFwSetenv,
    #[serde(rename = "host_rootfs_window")]
    HostRootfsWindow,
    /// Passive boot evidence exists, but no persistent selector writer does.
    #[serde(rename = "emmc_content_selector_evidence_only")]
    EmmcContentSelectorEvidenceOnly,
    #[serde(rename = "sd_image")]
    SdImage,
    #[serde(rename = "none")]
    None,
}

impl UpdateMechanism {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ZynqUbiFwSetenv => "zynq_ubi_fw_setenv",
            Self::HostRootfsWindow => "host_rootfs_window",
            Self::EmmcContentSelectorEvidenceOnly => "emmc_content_selector_evidence_only",
            Self::SdImage => "sd_image",
            Self::None => "none",
        }
    }

    /// Whether this mechanism implements the persistent sysupgrade contract.
    ///
    /// Evidence-only selectors and external-media image creation are useful
    /// capabilities, but neither is authority to mutate an installed rootfs.
    pub const fn supports_sysupgrade(self) -> bool {
        matches!(self, Self::ZynqUbiFwSetenv | Self::HostRootfsWindow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImplementationMaturity {
    #[serde(rename = "not_implemented")]
    NotImplemented,
    #[serde(rename = "experimental")]
    Experimental,
    #[serde(rename = "production")]
    Production,
}

impl ImplementationMaturity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotImplemented => "not_implemented",
            Self::Experimental => "experimental",
            Self::Production => "production",
        }
    }

    pub const fn is_implemented(self) -> bool {
        !matches!(self, Self::NotImplemented)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstallAuthorization {
    #[serde(rename = "denied")]
    Denied,
    #[serde(rename = "lab_only")]
    LabOnly,
    #[serde(rename = "public_beta")]
    PublicBeta,
    #[serde(rename = "production")]
    Production,
}

impl InstallAuthorization {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::LabOnly => "lab_only",
            Self::PublicBeta => "public_beta",
            Self::Production => "production",
        }
    }

    pub const fn allows_any_install(self) -> bool {
        !matches!(self, Self::Denied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecoveryMaturity {
    #[serde(rename = "not_implemented")]
    NotImplemented,
    #[serde(rename = "evidence_only")]
    EvidenceOnly,
    #[serde(rename = "verified")]
    Verified,
}

impl RecoveryMaturity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotImplemented => "not_implemented",
            Self::EvidenceOnly => "evidence_only",
            Self::Verified => "verified",
        }
    }

    pub const fn allows_restore(self) -> bool {
        matches!(self, Self::Verified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// No artifact producer or supported artifact lane exists for this target.
    #[serde(rename = "none")]
    None,
    #[serde(rename = "sysupgrade")]
    SysupgradeBundle,
    #[serde(rename = "offline_analysis")]
    OfflineAnalysisBundle,
    #[serde(rename = "sdcard_payload")]
    SdCardPayload,
    #[serde(rename = "runtime_bundle")]
    RuntimeBundle,
    #[serde(rename = "recovery_image")]
    RecoveryImage,
    #[serde(rename = "rootfs_reference")]
    RootfsReference,
}

impl ArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SysupgradeBundle => "sysupgrade",
            Self::OfflineAnalysisBundle => "offline_analysis",
            Self::SdCardPayload => "sdcard_payload",
            Self::RuntimeBundle => "runtime_bundle",
            Self::RecoveryImage => "recovery_image",
            Self::RootfsReference => "rootfs_reference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "sysupgrade" => Some(Self::SysupgradeBundle),
            "offline_analysis" => Some(Self::OfflineAnalysisBundle),
            "sdcard_payload" => Some(Self::SdCardPayload),
            "runtime_bundle" => Some(Self::RuntimeBundle),
            "recovery_image" => Some(Self::RecoveryImage),
            "rootfs_reference" => Some(Self::RootfsReference),
            _ => None,
        }
    }

    pub const fn is_persistent_update(self) -> bool {
        matches!(self, Self::SysupgradeBundle)
    }

    pub const fn is_implemented(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactMaturity {
    /// Artifact production is not implemented for this target.
    #[serde(rename = "not_implemented")]
    NotImplemented,
    #[serde(rename = "experimental")]
    Experimental,
    #[serde(rename = "production")]
    Production,
}

impl ArtifactMaturity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotImplemented => "not_implemented",
            Self::Experimental => "experimental",
            Self::Production => "production",
        }
    }

    pub const fn is_implemented(self) -> bool {
        !matches!(self, Self::NotImplemented)
    }
}

/// Independent enablement facets for one packaged board target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareEnablementPolicy {
    pub storage_topology: StorageTopology,
    pub update_mechanism: UpdateMechanism,
    pub update_maturity: ImplementationMaturity,
    pub install_authorization: InstallAuthorization,
    pub recovery_maturity: RecoveryMaturity,
    pub artifact_kind: ArtifactKind,
    pub artifact_maturity: ArtifactMaturity,
}

impl HardwareEnablementPolicy {
    /// Whether kind and maturity agree on the existence of an artifact lane.
    pub const fn artifact_contract_is_consistent(self) -> bool {
        self.artifact_kind.is_implemented() == self.artifact_maturity.is_implemented()
    }

    /// Whether a persistent-update API is representable for this target.
    pub const fn allows_persistent_update(self) -> bool {
        self.update_maturity.is_implemented()
            && self.install_authorization.allows_any_install()
            && self.artifact_kind.is_persistent_update()
            && self.update_mechanism.supports_sysupgrade()
    }

    /// Whether a destructive restore route is representable for this target.
    pub const fn allows_restore(self) -> bool {
        self.recovery_maturity.allows_restore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_pinned() {
        assert_eq!(HARDWARE_ENABLEMENT_SCHEMA_VERSION, 3);
    }

    #[test]
    fn absent_artifact_cannot_become_an_update_from_single_slot_topology() {
        let policy = HardwareEnablementPolicy {
            storage_topology: StorageTopology::SingleSlot,
            update_mechanism: UpdateMechanism::EmmcContentSelectorEvidenceOnly,
            update_maturity: ImplementationMaturity::NotImplemented,
            install_authorization: InstallAuthorization::Denied,
            recovery_maturity: RecoveryMaturity::NotImplemented,
            artifact_kind: ArtifactKind::None,
            artifact_maturity: ArtifactMaturity::NotImplemented,
        };

        assert!(!policy.allows_persistent_update());
        assert!(!policy.allows_restore());
        assert!(policy.artifact_contract_is_consistent());
        assert_eq!(policy.storage_topology, StorageTopology::SingleSlot);
    }

    #[test]
    fn persistent_update_requires_an_actual_sysupgrade_writer() {
        let baseline = HardwareEnablementPolicy {
            storage_topology: StorageTopology::SingleSlot,
            update_mechanism: UpdateMechanism::HostRootfsWindow,
            update_maturity: ImplementationMaturity::Experimental,
            install_authorization: InstallAuthorization::LabOnly,
            recovery_maturity: RecoveryMaturity::NotImplemented,
            artifact_kind: ArtifactKind::SysupgradeBundle,
            artifact_maturity: ArtifactMaturity::Experimental,
        };

        assert!(baseline.allows_persistent_update());
        for update_mechanism in [
            UpdateMechanism::None,
            UpdateMechanism::EmmcContentSelectorEvidenceOnly,
            UpdateMechanism::SdImage,
        ] {
            assert!(
                !HardwareEnablementPolicy {
                    update_mechanism,
                    ..baseline
                }
                .allows_persistent_update(),
                "{update_mechanism:?} must not authorize sysupgrade"
            );
        }
    }

    #[test]
    fn artifact_kind_wire_values_match_current_manifests() {
        for (wire, expected) in [
            ("none", ArtifactKind::None),
            ("sysupgrade", ArtifactKind::SysupgradeBundle),
            ("offline_analysis", ArtifactKind::OfflineAnalysisBundle),
            ("sdcard_payload", ArtifactKind::SdCardPayload),
        ] {
            assert_eq!(ArtifactKind::parse(wire), Some(expected));
            assert_eq!(expected.as_str(), wire);
            assert_eq!(
                serde_json::to_string(&expected).unwrap(),
                format!("\"{wire}\"")
            );
        }
        assert_eq!(ArtifactKind::parse("firmware-ish"), None);
    }

    #[test]
    fn artifact_kind_and_maturity_cannot_disagree() {
        let policy = HardwareEnablementPolicy {
            storage_topology: StorageTopology::Unknown,
            update_mechanism: UpdateMechanism::None,
            update_maturity: ImplementationMaturity::NotImplemented,
            install_authorization: InstallAuthorization::Denied,
            recovery_maturity: RecoveryMaturity::NotImplemented,
            artifact_kind: ArtifactKind::None,
            artifact_maturity: ArtifactMaturity::Experimental,
        };
        assert!(!policy.artifact_contract_is_consistent());
    }
}
