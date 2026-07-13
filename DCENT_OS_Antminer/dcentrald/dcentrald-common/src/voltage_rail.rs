//! VoltageRail — power-facet interface sketch (ADR-0010).
//!
//! # Why this exists
//!
//! `ChipDriver::set_voltage(&mut PicController, …)` is the wrong long-term
//! spine: BM1362 is a no-op, BM1370 is TODO TAS5782M, PIC1704 is a third
//! protocol family, and production AM2 uses dsPIC with fw-identity-dependent
//! framing. Voltage must be a **composed facet**, not a chip-driver method.
//!
//! # Status
//!
//! Pure error/type surface only. Live controllers remain in `dcentrald-asic`
//! (`pic`, `dspic`, `pic1704`) and PSU modules in `dcentrald-hal`. New code
//! should route voltage through a type that *will* implement these operations,
//! even if the impl is still a thin wrapper today.
//!
//! Do not add a fourth parallel voltage protocol without implementing (or
//! planning) this trait.

/// Class of refuse conditions that must fail closed on production paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageRefuseReason {
    /// Proven post-RESET corruption (e.g. dsPIC fw=0x86).
    DegradedFirmware,
    /// Controller not in app mode / bootloader only.
    WrongMode,
    /// Platform binding does not match probe (e.g. Pic1704 seal fail).
    BindingMismatch,
    /// Lab override required but not set.
    LabOverrideRequired,
}

/// Host-safe voltage operation error (no HAL types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoltageRailError {
    /// Communication / I/O failure (filled by adapters with detail).
    Io { detail: String },
    /// Explicit refuse (must not retry as success).
    Refused {
        reason: VoltageRefuseReason,
        detail: String,
    },
    /// Feature not implemented for this binding (must not return Ok(())).
    Unsupported { detail: String },
    /// Invalid set-point.
    InvalidParameter { detail: String },
}

impl std::fmt::Display for VoltageRailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { detail } => write!(f, "voltage I/O: {detail}"),
            Self::Refused { reason, detail } => {
                write!(f, "voltage refused ({reason:?}): {detail}")
            }
            Self::Unsupported { detail } => write!(f, "voltage unsupported: {detail}"),
            Self::InvalidParameter { detail } => write!(f, "voltage parameter: {detail}"),
        }
    }
}

impl std::error::Error for VoltageRailError {}

/// Operations every voltage backend must eventually expose.
///
/// Implemented by adapters over PIC16 / dsPIC / PIC1704 / NoPic+PSU — not by
/// `ChipDriver`. Heartbeat is part of the rail (fail-closed on miss → cut).
pub trait VoltageRail {
    /// Commanded voltage in millivolts (protocol-specific encoding inside impl).
    fn set_mv(&mut self, mv: u16) -> Result<(), VoltageRailError>;

    /// Enable hashboard rail / voltage output.
    fn enable(&mut self) -> Result<(), VoltageRailError>;

    /// Disable rail (emergency and teardown must prefer this path).
    fn disable(&mut self) -> Result<(), VoltageRailError>;

    /// Heartbeat / kick so hardware watchdogs do not cut unexpectedly.
    fn heartbeat(&mut self) -> Result<(), VoltageRailError>;

    /// Optional measured rail mV (None if measure unsupported).
    fn measure_mv(&mut self) -> Result<Option<u16>, VoltageRailError> {
        Ok(None)
    }

    /// Firmware / identity byte when known (e.g. dsPIC GET_VERSION).
    fn firmware_identity(&self) -> Option<u8> {
        None
    }
}

/// Map a “voltage path not implemented for this chip” situation.
///
/// **Must not** be converted to `Ok(())` at call sites (historical BM1362
/// `set_voltage` no-op returned Ok — that pattern is forbidden).
pub fn unsupported_voltage_path(detail: impl Into<String>) -> VoltageRailError {
    VoltageRailError::Unsupported {
        detail: detail.into(),
    }
}

/// Map degraded-firmware refuse (e.g. dsPIC fw=0x86).
pub fn refuse_degraded_firmware(detail: impl Into<String>) -> VoltageRailError {
    VoltageRailError::Refused {
        reason: VoltageRefuseReason::DegradedFirmware,
        detail: detail.into(),
    }
}

/// Map wrong app/bootloader mode refuse.
pub fn refuse_wrong_mode(detail: impl Into<String>) -> VoltageRailError {
    VoltageRailError::Refused {
        reason: VoltageRefuseReason::WrongMode,
        detail: detail.into(),
    }
}

/// True when the error is a hard refuse (operator/lab override required).
pub fn is_hard_refuse(err: &VoltageRailError) -> bool {
    matches!(err, VoltageRailError::Refused { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RefuseAll;

    impl VoltageRail for RefuseAll {
        fn set_mv(&mut self, _mv: u16) -> Result<(), VoltageRailError> {
            Err(VoltageRailError::Refused {
                reason: VoltageRefuseReason::DegradedFirmware,
                detail: "test".into(),
            })
        }
        fn enable(&mut self) -> Result<(), VoltageRailError> {
            self.set_mv(0)
        }
        fn disable(&mut self) -> Result<(), VoltageRailError> {
            Ok(())
        }
        fn heartbeat(&mut self) -> Result<(), VoltageRailError> {
            Ok(())
        }
    }

    #[test]
    fn refuse_is_not_ok() {
        let mut r = RefuseAll;
        assert!(r.set_mv(13700).is_err());
        assert!(r.enable().is_err());
        assert!(r.disable().is_ok());
    }

    #[test]
    fn unsupported_must_not_be_confused_with_success() {
        let err = VoltageRailError::Unsupported {
            detail: "TAS5782M not wired".into(),
        };
        assert!(matches!(err, VoltageRailError::Unsupported { .. }));
        // BM1362 historically returned Ok(()) while doing nothing — forbidden.
        assert_ne!(format!("{err}"), "");
    }

    #[test]
    fn helper_constructors_match_discipline() {
        let e = unsupported_voltage_path("BM1362 needs DspicController");
        assert!(!is_hard_refuse(&e));
        assert!(is_hard_refuse(&refuse_degraded_firmware("fw=0x86")));
        assert!(is_hard_refuse(&refuse_wrong_mode("bootloader")));
    }
}
