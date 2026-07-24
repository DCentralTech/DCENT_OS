//! One-shot admission for the AM2/Zynq BM1362 hybrid hardware route.
//!
//! `AsicProtocolAdmission` proves only that a descriptor and one runtime ASIC
//! identity agree.  It deliberately says nothing about the carrier or the
//! selected work route, so it must not authorize the S19j hybrid engine by
//! itself.  This module binds all startup-only evidence needed by that engine
//! and returns an opaque, non-duplicable capability.

use crate::daemon_lifecycle::PlatformIdentitySnapshot;
use crate::RuntimeDispatchKind;
use dcentrald_common::{
    AsicProtocolIdentity, BoardFamily, ChainTransportKind, SlotPolicy, VoltageControllerClass,
    WorkEngineKind,
};

const BOARD_TARGET: &str = "am2-s19j";
const OBSERVED_AM2_CONTROL_BOARD: &str = "Zynq am2-s17";

/// Capability to construct and enter the AM2/Zynq BM1362 hybrid engine.
///
/// The private seal prevents sibling modules from constructing this value.
/// Deliberately do not implement `Clone` or `Copy`: one successful admission
/// authorizes one engine entry, not an indefinitely reusable protocol fact.
#[must_use = "hybrid route admission must be consumed by the admitted engine"]
#[derive(Debug)]
pub(crate) struct S19jHybridRouteAdmission {
    _seal: Seal,
}

#[derive(Debug)]
enum Seal {
    Admitted,
}

/// Bind immutable startup identity to the exact hardware-owning route.
///
/// This is intentionally narrower than generic `BoardDesc` dispatch
/// admission.  A different carrier can carry the same BM1362 hashboard and
/// still require completely different GPIO, I2C, UART, reset, and work paths.
/// The platform marker is not required here: explicit `--s19j-hybrid` has
/// historically remained usable when that optional package marker is absent.
pub(crate) fn admit_s19j_hybrid_route(
    identity: &PlatformIdentitySnapshot,
    dispatch: RuntimeDispatchKind,
    configured_asic_protocol: Option<AsicProtocolIdentity>,
) -> Result<S19jHybridRouteAdmission, String> {
    if dispatch != RuntimeDispatchKind::S19jHybrid {
        return Err(format!(
            "AM2 BM1362 hybrid admission requires the s19j-hybrid runtime route, got {}",
            dispatch.label()
        ));
    }

    if identity.board_target() != BOARD_TARGET {
        return Err(format!(
            "AM2 BM1362 hybrid admission requires declared board target {BOARD_TARGET}, got {:?}",
            identity.board_target()
        ));
    }

    let board_desc = identity.board_desc.ok_or_else(|| {
        format!("AM2 BM1362 hybrid admission requires the registered {BOARD_TARGET} BoardDesc")
    })?;
    if board_desc.board_target != identity.board_target() {
        return Err(format!(
            "immutable board target {:?} contradicts bound BoardDesc {}",
            identity.board_target(),
            board_desc.board_target
        ));
    }

    let exact_facets = board_desc.board_target == BOARD_TARGET
        && board_desc.family == BoardFamily::Zynq
        && board_desc.chain_transport == ChainTransportKind::ZynqHybrid
        && board_desc.work_engine == WorkEngineKind::SerialWork
        && board_desc.asic_protocol == AsicProtocolIdentity::Bm1362
        && board_desc.voltage_controller == VoltageControllerClass::DsPic33Ep
        && board_desc.slot_policy == SlotPolicy::ZynqAbFwSetenv;
    if !exact_facets {
        return Err(format!(
            "BoardDesc {} is not the exact AM2/Zynq BM1362 hybrid composition",
            board_desc.board_target
        ));
    }

    // The current passive detector reports the AM2 UIO topology as
    // `Zynq am2-s17`; the product suffix is historical and not ASIC evidence.
    // Bind only the measured carrier class and leave hashboard/ASIC proof to
    // their independent evidence boundaries.
    if identity.observed_control_board != OBSERVED_AM2_CONTROL_BOARD {
        return Err(format!(
            "declared AM2 hybrid composition contradicts observed control board {:?}",
            identity.observed_control_board
        ));
    }

    if configured_asic_protocol != Some(AsicProtocolIdentity::Bm1362) {
        return Err(format!(
            "AM2 BM1362 hybrid admission requires exact configured ASIC identity Bm1362, got {configured_asic_protocol:?}"
        ));
    }

    Ok(S19jHybridRouteAdmission {
        _seal: Seal::Admitted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = include_str!("s19j_hybrid_admission.rs");

    fn identity(board_target: &str, observed_control_board: &str) -> PlatformIdentitySnapshot {
        PlatformIdentitySnapshot {
            declared_board_target: Some(board_target.to_string()),
            board_desc: dcentrald_common::BoardDesc::lookup(board_target),
            declared_platform_marker: Some("zynq-bm3-am2".to_string()),
            declared_subtype: None,
            declared_psu_hardware_variant: None,
            observed_control_board: observed_control_board.to_string(),
        }
    }

    #[test]
    fn exact_am2_zynq_bm1362_hybrid_composition_is_admitted() {
        let _admission = admit_s19j_hybrid_route(
            &identity(BOARD_TARGET, "Zynq am2-s17"),
            RuntimeDispatchKind::S19jHybrid,
            Some(AsicProtocolIdentity::Bm1362),
        )
        .expect("the canonical AM2/Zynq BM1362 hybrid composition must admit");
    }

    #[test]
    fn same_bm1362_on_other_carriers_cannot_launder_protocol_admission() {
        for (board_target, observed_control_board) in [
            ("am3-bb-s19jpro", "BeagleBone S9"),
            ("am3-s19jpro-aml", "AML Amlogic"),
            ("cv1835-s19jpro", "CVITEK CV1835"),
        ] {
            let desc = dcentrald_common::BoardDesc::lookup(board_target)
                .expect("same-BM1362 comparison target must be registered");
            assert_eq!(desc.asic_protocol, AsicProtocolIdentity::Bm1362);
            assert!(
                admit_s19j_hybrid_route(
                    &identity(board_target, observed_control_board),
                    RuntimeDispatchKind::S19jHybrid,
                    Some(AsicProtocolIdentity::Bm1362),
                )
                .is_err(),
                "same ASIC protocol on {board_target} must not authorize the AM2 hybrid route"
            );
        }
    }

    #[test]
    fn stale_am2_declaration_on_wrong_observed_carrier_fails_closed() {
        for observed_control_board in ["Zynq am1-s9", "BeagleBone S9", "AML Amlogic", "Unknown"] {
            assert!(
                admit_s19j_hybrid_route(
                    &identity(BOARD_TARGET, observed_control_board),
                    RuntimeDispatchKind::S19jHybrid,
                    Some(AsicProtocolIdentity::Bm1362),
                )
                .is_err(),
                "observed carrier {observed_control_board} must contradict AM2 admission"
            );
        }
    }

    #[test]
    fn wrong_route_or_asic_identity_fails_closed() {
        let am2 = identity(BOARD_TARGET, "Zynq am2-s17");
        assert!(admit_s19j_hybrid_route(
            &am2,
            RuntimeDispatchKind::Serial,
            Some(AsicProtocolIdentity::Bm1362),
        )
        .is_err());
        assert!(admit_s19j_hybrid_route(
            &am2,
            RuntimeDispatchKind::S19jHybrid,
            Some(AsicProtocolIdentity::Bm1398),
        )
        .is_err());
        assert!(admit_s19j_hybrid_route(&am2, RuntimeDispatchKind::S19jHybrid, None,).is_err());
    }

    #[test]
    fn route_capability_has_no_public_mint_clone_or_copy_surface() {
        let production = SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("production source prefix");

        assert!(production.contains("pub(crate) struct S19jHybridRouteAdmission"));
        assert!(production.contains("_seal: Seal"));
        assert!(!production.contains("pub struct S19jHybridRouteAdmission"));
        assert!(!production.contains("pub fn admit_s19j_hybrid_route"));
        assert!(!production.contains("impl Clone for S19jHybridRouteAdmission"));
        assert!(!production.contains("impl Copy for S19jHybridRouteAdmission"));

        let derive = production
            .split("pub(crate) struct S19jHybridRouteAdmission")
            .next()
            .and_then(|prefix| prefix.lines().rev().find(|line| line.contains("derive")))
            .expect("route capability derive");
        assert!(!derive.contains("Clone"));
        assert!(!derive.contains("Copy"));
    }
}
