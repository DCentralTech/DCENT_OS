//!  pic-A — PIC firmware version catalog (HAL-free).
//!
//! Source RE evidence:
//!  (PIC16F1704
//! and dsPIC33EP16GS202 firmware versions across the Antminer fleet).
//!
//! Two PIC architectures coexist:
//! - **PIC16F1704** (8-bit): S9 (v1.9..v5.1), S17+/T17/T17e (single
//!   version), S19 (v89 / BM1398). LVP-disabled on S17+ and later as
//!   anti-tamper.
//! - **dsPIC33EP16GS202** (16-bit DSP): S17/S17e only. Higher MIPS,
//!   hardware HR-PWM, more flash. Reverted to PIC16F1704 for S17+ era.
//!
//! Plus DCENT_OS-relevant fw bytes from live probes:
//! - `0x82` — early dsPIC variant (BARE protocol; S17 era).
//! - `0x86` — corruption state (post-PIC-RESET on .74 — voltage refused
//!   by default).
//! - `0x89` — S19j Pro AM2 dsPIC (RESET banned per
//!   ).
//! - `0x8A` — S19 Pro / S19j Pro+ variant.
//!
//! HAL-free: pure data + lookup. The runtime adapter inside
//! `dcentrald-asic::dspic` consumes this catalog to dispatch
//! variant-aware behavior (BARE vs FRAMED-SUM, RESET allowed vs banned,
//! voltage commands trusted vs refused).

use serde::{Deserialize, Serialize};

/// Public response schema for `GET /api/hardware/pic_info`.
pub const PIC_INFO_SCHEMA: &str = "dcentos.hardware.pic_info.v2";

/// Discrete PIC architecture identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicArchitecture {
    /// PIC16F1704 — 8-bit MCU. ~8 KB flash. Used on S9, S17+, T17,
    /// T17e, S19.
    Pic16F1704,
    /// dsPIC33EP16GS202 — 16-bit DSP. ~16 KB flash. Used on S17/S17e
    /// (reverted to PIC16F1704 for S17+ era). Live runtime fw bytes
    /// 0x82, 0x86, 0x89, 0x8A all sit on this architecture.
    Dspic33Ep16Gs202,
}

/// One known firmware-byte → behavior mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PicFirmwareVariant {
    /// Firmware byte returned by GET_VERSION (cmd 0x17 framed or 0x04
    /// stock).
    pub fw_byte: u8,
    /// Architecture this fw runs on.
    pub architecture: PicArchitecture,
    /// Wire-form preference. Some variants prefer BARE, others FRAMED-SUM.
    pub wire_form: WireForm,
    /// Whether RESET (cmd 0x07) is safe to issue. False =  rule applies.
    pub reset_safe: bool,
    /// Whether voltage commands (SET_VOLTAGE 0x10) are trusted. False =
    /// fw=0x86 corruption-state refusal.
    pub voltage_trusted: bool,
    /// Operator-facing label for dashboard / docs.
    pub label: &'static str,
}

/// Wire-form preference per (chip-family × fw-byte) combo. Mirrors
/// `dcentrald-api-types::dspic_frame::DspicFrame::encode_*` shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireForm {
    /// 3-byte BARE encoding: `[0x55, 0xAA, CMD] + payload`.
    Bare,
    /// FRAMED-SUM encoding: `[0x55, 0xAA, LEN, CMD, payload..., CKSUM]`.
    FramedSum,
}

/// Wire DTO for a known firmware variant.
///
/// The REST endpoint historically exposed `fw_byte` as a hex string and
/// `fw_byte_decimal` as the numeric value. Keep both so existing dashboard and
/// toolbox clients can adopt the richer status fields without a rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PicFirmwareVariantDto {
    pub fw_byte: String,
    pub fw_byte_decimal: u8,
    pub architecture: PicArchitecture,
    pub wire_form: WireForm,
    pub reset_safe: bool,
    pub voltage_trusted: bool,
    pub voltage_refused_by_default: bool,
    pub label: String,
}

impl From<&PicFirmwareVariant> for PicFirmwareVariantDto {
    fn from(value: &PicFirmwareVariant) -> Self {
        Self {
            fw_byte: format_fw_byte(value.fw_byte),
            fw_byte_decimal: value.fw_byte,
            architecture: value.architecture,
            wire_form: value.wire_form,
            reset_safe: value.reset_safe,
            voltage_trusted: value.voltage_trusted,
            voltage_refused_by_default: requires_voltage_refusal(value.fw_byte),
            label: value.label.to_string(),
        }
    }
}

/// Overall `/api/hardware/pic_info` status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicFirmwareInfoStatus {
    /// Static catalog only. This is the current REST behavior when AppState has
    /// no PIC service snapshot handle.
    CatalogOnly,
    /// A service-owned snapshot supplied live per-slot firmware observations.
    LiveSnapshot,
    /// Some slot observations were present, but at least one slot was missing
    /// or errored.
    PartialSnapshot,
}

/// Status for the future live per-slot PIC firmware snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicFirmwareLiveSlotStatus {
    /// REST has no PicService/Pic0x89Service snapshot handle in AppState.
    NotWired,
    /// A snapshot handle exists, but it has not published any slot data.
    Unavailable,
    /// A slot has a parsed firmware byte from a service-owned snapshot.
    Live,
    /// A slot exists, but the service reported an error for it.
    Error,
}

/// One future live per-slot firmware observation.
///
/// This type is a plumbing seam: the REST handler can serialize it when some
/// daemon-owned PicService publisher exists, but the REST handler itself must
/// never issue I2C reads to fill it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PicFirmwareLiveSlot {
    pub slot: Option<String>,
    pub chain_id: Option<u8>,
    pub i2c_bus: Option<u8>,
    pub i2c_addr: Option<u8>,
    pub fw_byte: Option<String>,
    pub fw_byte_decimal: Option<u8>,
    pub status: PicFirmwareLiveSlotStatus,
    pub voltage_refused_by_default: bool,
    pub variants: Vec<PicFirmwareVariantDto>,
    pub observed_at_ms: Option<u64>,
    pub source: String,
    pub error: Option<String>,
}

impl Default for PicFirmwareLiveSlot {
    fn default() -> Self {
        Self {
            slot: None,
            chain_id: None,
            i2c_bus: None,
            i2c_addr: None,
            fw_byte: None,
            fw_byte_decimal: None,
            status: PicFirmwareLiveSlotStatus::Unavailable,
            voltage_refused_by_default: false,
            variants: Vec::new(),
            observed_at_ms: None,
            source: "unavailable".to_string(),
            error: None,
        }
    }
}

impl PicFirmwareLiveSlot {
    pub fn observed(
        slot: impl Into<String>,
        chain_id: Option<u8>,
        i2c_bus: Option<u8>,
        i2c_addr: Option<u8>,
        fw_byte: u8,
        observed_at_ms: Option<u64>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            slot: Some(slot.into()),
            chain_id,
            i2c_bus,
            i2c_addr,
            fw_byte: Some(format_fw_byte(fw_byte)),
            fw_byte_decimal: Some(fw_byte),
            status: PicFirmwareLiveSlotStatus::Live,
            voltage_refused_by_default: requires_voltage_refusal(fw_byte),
            variants: all_variants_with_fw_byte(fw_byte)
                .into_iter()
                .map(PicFirmwareVariantDto::from)
                .collect(),
            observed_at_ms,
            source: source.into(),
            error: None,
        }
    }

    pub fn error(
        slot: impl Into<String>,
        chain_id: Option<u8>,
        i2c_bus: Option<u8>,
        i2c_addr: Option<u8>,
        error: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            slot: Some(slot.into()),
            chain_id,
            i2c_bus,
            i2c_addr,
            status: PicFirmwareLiveSlotStatus::Error,
            source: source.into(),
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// Future live per-slot section for the PIC info response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PicFirmwareLivePerSlot {
    pub status: PicFirmwareLiveSlotStatus,
    pub service_handle_present: bool,
    pub observations: Vec<PicFirmwareLiveSlot>,
    pub limitations: Vec<String>,
}

/// Full public response for `GET /api/hardware/pic_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PicFirmwareInfoResponse {
    pub schema: String,
    pub status: PicFirmwareInfoStatus,
    pub read_only: bool,
    pub rest_handler_hardware_reads: bool,
    pub rest_handler_hardware_writes: bool,
    pub control_actions: bool,
    pub live_service_handle_present: bool,
    pub count: usize,
    pub variants: Vec<PicFirmwareVariantDto>,
    pub live_per_slot: PicFirmwareLivePerSlot,
    pub limitations: Vec<String>,
}

impl PicFirmwareInfoResponse {
    /// Current host-safe REST shape: catalog plus explicit no-service status.
    pub fn catalog_only_without_live_service() -> Self {
        Self::from_live_slots(false, Vec::new())
    }

    /// Build a response from service-owned live observations.
    ///
    /// This is intentionally pure. Callers may pass snapshots collected by the
    /// daemon/PIC service, but this function does not perform I/O.
    pub fn from_live_slots(
        service_handle_present: bool,
        observations: Vec<PicFirmwareLiveSlot>,
    ) -> Self {
        let live_status = if !service_handle_present {
            PicFirmwareLiveSlotStatus::NotWired
        } else if observations.is_empty() {
            PicFirmwareLiveSlotStatus::Unavailable
        } else if observations
            .iter()
            .all(|slot| slot.status == PicFirmwareLiveSlotStatus::Live)
        {
            PicFirmwareLiveSlotStatus::Live
        } else {
            PicFirmwareLiveSlotStatus::Error
        };

        let status = match live_status {
            PicFirmwareLiveSlotStatus::Live => PicFirmwareInfoStatus::LiveSnapshot,
            PicFirmwareLiveSlotStatus::Error => PicFirmwareInfoStatus::PartialSnapshot,
            PicFirmwareLiveSlotStatus::NotWired | PicFirmwareLiveSlotStatus::Unavailable => {
                PicFirmwareInfoStatus::CatalogOnly
            }
        };

        let variants = KNOWN_VARIANTS
            .iter()
            .map(PicFirmwareVariantDto::from)
            .collect();
        let live_limitations = match live_status {
            PicFirmwareLiveSlotStatus::NotWired => vec![
                "No PicService/Pic0x89Service snapshot handle is present in API state.".to_string(),
                "REST did not issue I2C reads or writes to collect firmware bytes.".to_string(),
            ],
            PicFirmwareLiveSlotStatus::Unavailable => vec![
                "A live PIC snapshot handle exists, but no per-slot firmware sample has been published yet."
                    .to_string(),
                "REST did not issue I2C reads or writes to fill the gap.".to_string(),
            ],
            PicFirmwareLiveSlotStatus::Live => vec![
                "Firmware bytes are service-owned snapshots; REST remains read-only.".to_string(),
            ],
            PicFirmwareLiveSlotStatus::Error => vec![
                "At least one service-owned PIC firmware observation reported an error.".to_string(),
                "Failed slots must not be treated as live voltage trust evidence.".to_string(),
            ],
        };

        Self {
            schema: PIC_INFO_SCHEMA.to_string(),
            status,
            read_only: true,
            rest_handler_hardware_reads: false,
            rest_handler_hardware_writes: false,
            control_actions: false,
            live_service_handle_present: service_handle_present,
            count: KNOWN_VARIANTS.len(),
            variants,
            live_per_slot: PicFirmwareLivePerSlot {
                status: live_status,
                service_handle_present,
                observations,
                limitations: live_limitations,
            },
            limitations: vec![
                "PIC firmware semantics are cataloged from RE and live probe evidence.".to_string(),
                "fw=0x86 remains voltage-refused by default unless a lab override is explicitly enabled outside this REST response."
                    .to_string(),
                "RESET/JUMP safety is reported as metadata only; this endpoint executes no PIC commands."
                    .to_string(),
            ],
        }
    }
}

/// Live-known firmware variant catalog.
pub const KNOWN_VARIANTS: &[PicFirmwareVariant] = &[
    PicFirmwareVariant {
        fw_byte: 0x03,
        architecture: PicArchitecture::Pic16F1704,
        wire_form: WireForm::FramedSum,
        reset_safe: true,
        voltage_trusted: true,
        label: "BraiinsOS reflashed S9 PIC (v0x03)",
    },
    PicFirmwareVariant {
        fw_byte: 0x56,
        architecture: PicArchitecture::Pic16F1704,
        wire_form: WireForm::Bare,
        reset_safe: false, // stock S9 PIC bootloader doesn't handle JUMP
        voltage_trusted: true,
        label: "Stock Bitmain S9 PIC v5.6",
    },
    PicFirmwareVariant {
        fw_byte: 0x5A,
        architecture: PicArchitecture::Pic16F1704,
        wire_form: WireForm::Bare,
        reset_safe: false,
        voltage_trusted: true,
        label: "Stock Bitmain S9 PIC v5.10",
    },
    PicFirmwareVariant {
        fw_byte: 0x5E,
        architecture: PicArchitecture::Pic16F1704,
        wire_form: WireForm::Bare,
        reset_safe: false,
        voltage_trusted: true,
        label: "Stock Bitmain S9 PIC v5.14 (latest stock)",
    },
    PicFirmwareVariant {
        fw_byte: 0x82,
        architecture: PicArchitecture::Dspic33Ep16Gs202,
        wire_form: WireForm::Bare,
        reset_safe: true,
        voltage_trusted: true,
        label: "Early dsPIC (S17 era)",
    },
    PicFirmwareVariant {
        fw_byte: 0x86,
        architecture: PicArchitecture::Dspic33Ep16Gs202,
        wire_form: WireForm::FramedSum,
        // : NEVER reset fw=0x86; the
        // post-RESET state is the corruption mode.
        reset_safe: false,
        // : voltage refused by
        // default; override only via DCENT_AM2_TRUST_DEGRADED_FW=1.
        voltage_trusted: false,
        label: "dsPIC corruption state (post-RESET .74)",
    },
    PicFirmwareVariant {
        fw_byte: 0x88,
        architecture: PicArchitecture::Dspic33Ep16Gs202,
        wire_form: WireForm::FramedSum,
        reset_safe: false,
        voltage_trusted: true,
        label: "dsPIC AM2 variant",
    },
    PicFirmwareVariant {
        fw_byte: 0x89,
        architecture: PicArchitecture::Dspic33Ep16Gs202,
        wire_form: WireForm::FramedSum,
        // S19j Pro AM2 — RESET banned.
        reset_safe: false,
        voltage_trusted: true,
        label: "S19j Pro AM2 dsPIC v0x89",
    },
    PicFirmwareVariant {
        fw_byte: 0x8A,
        architecture: PicArchitecture::Dspic33Ep16Gs202,
        wire_form: WireForm::FramedSum,
        reset_safe: false,
        voltage_trusted: true,
        label: "S19 Pro / S19j Pro+ dsPIC v0x8A",
    },
    PicFirmwareVariant {
        fw_byte: 0x89, // PIC16F1704 used 0x89 too on S19 / BM1398
        architecture: PicArchitecture::Pic16F1704,
        wire_form: WireForm::FramedSum,
        reset_safe: false,
        voltage_trusted: true,
        label: "S19 BM1398 PIC16F1704 v89 (Zynq am2)",
    },
];

/// Look up a firmware variant by fw byte. If multiple variants share
/// the same byte (PIC16F1704 v89 vs dsPIC v0x89 — yes, this happens),
/// returns the first match — caller should disambiguate by chip family.
pub fn variant_by_fw_byte(fw: u8) -> Option<&'static PicFirmwareVariant> {
    KNOWN_VARIANTS.iter().find(|v| v.fw_byte == fw)
}

pub fn format_fw_byte(fw: u8) -> String {
    format!("0x{:02x}", fw)
}

/// Look up all variants with a given fw byte. Returns up to 2 entries
/// when an architecture overlap exists (e.g. fw 0x89 lives on both
/// PIC16F1704 and dsPIC).
pub fn all_variants_with_fw_byte(fw: u8) -> Vec<&'static PicFirmwareVariant> {
    KNOWN_VARIANTS.iter().filter(|v| v.fw_byte == fw).collect()
}

/// Whether a given fw byte triggers the voltage-refused rule per
/// .
pub fn requires_voltage_refusal(fw: u8) -> bool {
    fw == 0x86
}

/// Whether RESET (cmd 0x07) is safe to issue against a given fw byte.
/// Conservative: returns false for any fw byte not in the catalog.
pub fn reset_is_safe(fw: u8) -> bool {
    variant_by_fw_byte(fw)
        .map(|v| v.reset_safe)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw_0x86_corruption_refuses_voltage() {
        //: fw=0x86 = corruption
        // state, voltage commands refused by default.
        assert!(requires_voltage_refusal(0x86));
        let v = variant_by_fw_byte(0x86).unwrap();
        assert!(!v.voltage_trusted);
        assert!(!v.reset_safe);
    }

    #[test]
    fn fw_0x89_dspic_reset_banned() {
        //.
        let variants = all_variants_with_fw_byte(0x89);
        assert!(!variants.is_empty());
        for v in variants {
            assert!(!v.reset_safe, "{:?} should NOT allow reset", v.label);
        }
    }

    #[test]
    fn fw_0x82_supports_bare_wire_form() {
        // Early dsPIC uses BARE encoding per dspic-protocol-bible.md.
        let v = variant_by_fw_byte(0x82).unwrap();
        assert_eq!(v.wire_form, WireForm::Bare);
        assert_eq!(v.architecture, PicArchitecture::Dspic33Ep16Gs202);
    }

    #[test]
    fn braiinsos_v0x03_is_first_class_pic16f1704() {
        let v = variant_by_fw_byte(0x03).unwrap();
        assert_eq!(v.architecture, PicArchitecture::Pic16F1704);
        assert!(v.reset_safe);
        assert!(v.voltage_trusted);
        assert_eq!(v.wire_form, WireForm::FramedSum);
    }

    #[test]
    fn stock_s9_pic_versions_use_bare() {
        for fw in [0x56, 0x5A, 0x5E] {
            let v = variant_by_fw_byte(fw).unwrap();
            assert_eq!(
                v.architecture,
                PicArchitecture::Pic16F1704,
                "fw {:#x} should be PIC16F1704",
                fw
            );
            assert_eq!(
                v.wire_form,
                WireForm::Bare,
                "fw {:#x} should use BARE wire form",
                fw
            );
        }
    }

    #[test]
    fn fw_0x89_overlaps_two_architectures() {
        // RE doc §4: PIC16F1704 v89 used on S19 BM1398, AND dsPIC fw
        // 0x89 used on S19j Pro AM2. Different chips, same byte.
        let variants = all_variants_with_fw_byte(0x89);
        assert!(
            variants.len() >= 2,
            "fw 0x89 should resolve to at least 2 architectures"
        );
        let archs: Vec<PicArchitecture> = variants.iter().map(|v| v.architecture).collect();
        assert!(archs.contains(&PicArchitecture::Pic16F1704));
        assert!(archs.contains(&PicArchitecture::Dspic33Ep16Gs202));
    }

    #[test]
    fn unknown_fw_byte_returns_none() {
        assert!(variant_by_fw_byte(0xFF).is_none());
        assert!(variant_by_fw_byte(0x00).is_none());
        assert!(variant_by_fw_byte(0x42).is_none());
    }

    #[test]
    fn reset_is_safe_conservative_for_unknown_fw() {
        // Any fw byte not in the catalog → reset NOT safe.
        assert!(!reset_is_safe(0x00));
        assert!(!reset_is_safe(0xFF));
    }

    #[test]
    fn voltage_refusal_only_triggers_for_0x86() {
        // The fw whitelist rule is narrowly scoped — only 0x86.
        for fw in [0x03u8, 0x82, 0x88, 0x89, 0x8A] {
            assert!(
                !requires_voltage_refusal(fw),
                "fw {:#x} should NOT trigger refusal",
                fw
            );
        }
    }

    #[test]
    fn pic_architecture_round_trips_through_serde() {
        for a in [
            PicArchitecture::Pic16F1704,
            PicArchitecture::Dspic33Ep16Gs202,
        ] {
            let json = serde_json::to_string(&a).unwrap();
            let back: PicArchitecture = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn variant_serializes_to_documented_shape() {
        let v = variant_by_fw_byte(0x86).unwrap();
        let json = serde_json::to_string(v).unwrap();
        assert!(json.contains("\"fw_byte\":134"));
        assert!(json.contains("\"voltage_trusted\":false"));
        assert!(json.contains("\"reset_safe\":false"));
    }

    #[test]
    fn pic_info_catalog_only_response_reports_no_live_service_handle() {
        let response = PicFirmwareInfoResponse::catalog_only_without_live_service();
        assert_eq!(response.schema, PIC_INFO_SCHEMA);
        assert_eq!(response.status, PicFirmwareInfoStatus::CatalogOnly);
        assert!(response.read_only);
        assert!(!response.rest_handler_hardware_reads);
        assert!(!response.rest_handler_hardware_writes);
        assert!(!response.control_actions);
        assert!(!response.live_service_handle_present);
        assert_eq!(
            response.live_per_slot.status,
            PicFirmwareLiveSlotStatus::NotWired
        );
        assert!(response.live_per_slot.observations.is_empty());
    }

    #[test]
    fn pic_info_response_preserves_key_firmware_safety_semantics() {
        let response = PicFirmwareInfoResponse::catalog_only_without_live_service();
        let variant = |fw: u8| {
            response
                .variants
                .iter()
                .find(|variant| variant.fw_byte_decimal == fw)
                .expect("firmware variant should be present")
        };

        let fw03 = variant(0x03);
        assert_eq!(fw03.fw_byte, "0x03");
        assert!(fw03.reset_safe);
        assert!(fw03.voltage_trusted);
        assert!(!fw03.voltage_refused_by_default);

        let fw56 = variant(0x56);
        assert_eq!(fw56.wire_form, WireForm::Bare);
        assert!(!fw56.reset_safe);
        assert!(fw56.voltage_trusted);

        let fw86 = variant(0x86);
        assert!(!fw86.reset_safe);
        assert!(!fw86.voltage_trusted);
        assert!(fw86.voltage_refused_by_default);

        let fw89_variants: Vec<&PicFirmwareVariantDto> = response
            .variants
            .iter()
            .filter(|variant| variant.fw_byte_decimal == 0x89)
            .collect();
        assert!(fw89_variants.len() >= 2);
        assert!(fw89_variants.iter().all(|variant| !variant.reset_safe));
        assert!(fw89_variants
            .iter()
            .all(|variant| variant.voltage_trusted && !variant.voltage_refused_by_default));
    }

    #[test]
    fn pic_info_live_slot_seam_marks_fw86_voltage_refusal() {
        let slot = PicFirmwareLiveSlot::observed(
            "hb2",
            Some(2),
            Some(0),
            Some(0x21),
            0x86,
            Some(123_456),
            "test_snapshot",
        );
        let response = PicFirmwareInfoResponse::from_live_slots(true, vec![slot.clone()]);

        assert_eq!(response.status, PicFirmwareInfoStatus::LiveSnapshot);
        assert_eq!(
            response.live_per_slot.status,
            PicFirmwareLiveSlotStatus::Live
        );
        assert!(response.live_service_handle_present);
        assert_eq!(slot.fw_byte.as_deref(), Some("0x86"));
        assert_eq!(slot.fw_byte_decimal, Some(0x86));
        assert!(slot.voltage_refused_by_default);
        assert!(slot
            .variants
            .iter()
            .any(|variant| { variant.fw_byte_decimal == 0x86 && !variant.voltage_trusted }));
    }
}
