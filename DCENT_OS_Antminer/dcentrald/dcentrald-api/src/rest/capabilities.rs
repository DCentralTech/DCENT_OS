use super::*;

pub(super) fn runtime_capability_guard_error(
    descriptor: &DeviceCapabilityDescriptor,
    required: RuntimeCapability,
    route: &str,
) -> Option<CapabilityError> {
    if descriptor.runtime_caps.contains(&required) {
        return None;
    }

    let message = format!(
        "{route} requires runtime capability `{}`; current support tier {:?} with identity confidence {:?} does not grant it.",
        required.as_str(),
        descriptor.support,
        descriptor.identity.confidence
    );

    if descriptor.identity.confidence == IdentityConfidence::Unknown {
        return Some(CapabilityError::unknown_hardware(message));
    }

    if descriptor.support == CapabilitySupportTier::Unsupported {
        Some(CapabilityError::unsupported(required, message))
    } else {
        Some(CapabilityError::conflict(required, message))
    }
}

/// CE-052: fail-closed runtime-capability gate for the control BRIDGES (gRPC /
/// MQTT / CGMiner) that share the REST write core but historically skipped the
/// Beta-tier + Exact/High-identity capability check their REST twins enforce.
///
/// Builds the current antminer capability descriptor from live `AppState` and
/// runs it through the SAME [`runtime_capability_guard_error`] the REST handlers
/// use. On denial the flat `Err(message)` is surfaced through each bridge's
/// existing honest channel (gRPC reject / MQTT warn-drop / CGMiner STATUS error)
/// — the guard runs BEFORE any filesystem write / HAL open / restart flag /
/// channel dispatch, so a denied identity never reaches a side effect.
pub(crate) fn bridge_runtime_capability_guard(
    state: &AppState,
    required: RuntimeCapability,
    surface: &str,
) -> std::result::Result<(), String> {
    let descriptor = current_antminer_capability_descriptor(state);
    match runtime_capability_guard_error(&descriptor, required, surface) {
        Some(error) => Err(error.message),
        None => Ok(()),
    }
}

/// CE-052: narrow `AsicOptions` bridge guard. `pub` (re-exported from
/// `crate::rest`) so the daemon crate can gate its gRPC write delegate without a
/// `dcent_schema` capability dependency.
pub fn bridge_guard_asic_options(
    state: &AppState,
    surface: &str,
) -> std::result::Result<(), String> {
    bridge_runtime_capability_guard(state, RuntimeCapability::AsicOptions, surface)
}

/// CE-052: narrow `Identify` bridge guard. `pub` (re-exported from `crate::rest`)
/// so the daemon crate can gate `locate_device` without a `dcent_schema`
/// capability dependency.
pub fn bridge_guard_identify(
    state: &AppState,
    surface: &str,
) -> std::result::Result<(), String> {
    bridge_runtime_capability_guard(state, RuntimeCapability::Identify, surface)
}
