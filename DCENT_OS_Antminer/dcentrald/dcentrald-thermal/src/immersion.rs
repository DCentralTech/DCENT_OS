//! Immersion / hydro cooling mode (W8 parity gap: DCENT ❌/⚠️ vs LuxOS/VNish ✅).
//!
//! Immersion (single-phase / two-phase dielectric fluid) and hydro (water-block)
//! rigs do NOT have chassis fans on the air path — cooling is handled by an
//! EXTERNAL loop (pump + dry-cooler / radiator / reservoir). On such a unit the
//! normal air-fan PID ramp is meaningless: there is nothing for `dcentrald` to
//! ramp, and on a board with the fan headers unpopulated a "ramp fans" command
//! is a no-op at best. LuxOS (`immersionswitch`) and VNish (`cooling_mode =
//! "immersion"` / `"hydro"`) both ship an explicit immersion mode; DCENT_OS did
//! not. This module adds it as an EXPLICIT, default-OFF opt-in.
//!
//! # SAFETY-CRITICAL contract (do not regress)
//!
//! Immersion mode ONLY removes the fan-RAMP behavior. It does NOT remove the
//! thermal SAFETY net:
//!
//! - **Die / board / chip temperature is still monitored every tick.**
//! - **Stale / lost temperature still fails closed** — a sensor that goes dark
//!   is treated as dangerous and cuts hash (it NEVER assumes "the fluid is
//!   handling it"). On an immersion rig there are no fans to blast as a
//!   sensor-failure response anyway, so the fail-closed response is HASH-CUT,
//!   never fan-blast.
//! - **Over-temp still cuts hash** (`ThermalAction::EmergencyShutdown`) at the
//!   `dangerous_temp_c` threshold. Immersion only suppresses the *fan ramp* —
//!   it never weakens the hash-cut floor.
//! - **It is DEFAULT-OFF and EXPLICIT opt-in.** On an air-cooled unit, silently
//!   bypassing fan management would be catastrophic (boards cook with no
//!   airflow), so this mode must never auto-enable. When enabled it logs a
//!   prominent warning that fan management is bypassed, and — unless the
//!   operator explicitly acknowledges an air-cooled override — it refuses to
//!   activate on a platform that looks air-cooled.
//!
//! Mirrors the `ThermalSupervisorConfig` opt-in pattern (`enabled: bool`,
//! `#[serde(default)]`, `Default` = off) so the daemon-side `ThermalConfig`
//! can embed `[thermal.immersion]` the same way it embeds
//! `[thermal.supervisor]`. The daemon-side wiring (reading this into
//! `ThermalConfig`, calling `ThermalController::enable_immersion`, and skipping
//! the fan write when `immersion_active()`) is a separate-crate change tracked
//! in the handoff — this crate owns the policy + the controller behavior.

use serde::{Deserialize, Serialize};

/// Immersion / hydro cooling mode configuration.
///
/// **Default-OFF.** When `enabled == false` the controller behaves
/// byte-identically to the pre-immersion path — there is zero behavioral delta
/// on every existing air-cooled unit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImmersionConfig {
    /// **Default false.** Explicit operator opt-in. With this flag false the
    /// controller's fan-RAMP behavior is unchanged and `immersion_active()`
    /// returns false.
    #[serde(default)]
    pub enabled: bool,

    /// **Air-cooled safety override.** Default false. Immersion mode bypasses
    /// fan management, which is catastrophic on an air-cooled unit (boards
    /// cook with no airflow). When the running platform looks air-cooled (the
    /// daemon passes `platform_looks_air_cooled = true`), the controller
    /// REFUSES to activate immersion unless this flag is explicitly set. This
    /// is the "I really do have an external cooling loop on this air-cooled
    /// chassis" acknowledgement — it must be a deliberate operator action, not
    /// a default.
    #[serde(default)]
    pub acknowledge_air_cooled_override: bool,
}

impl ImmersionConfig {
    /// Decide whether immersion mode may activate given the requested config
    /// and what the daemon believes about the platform's cooling topology.
    ///
    /// Pure function (no logging / no side effects) so it is unit-testable on
    /// any host. The caller (`ThermalController::enable_immersion`) maps the
    /// decision to a `tracing` warning and the activation/refusal.
    ///
    /// - `enabled == false` → [`ImmersionDecision::Disabled`] (the common
    ///   air-cooled path; nothing changes).
    /// - `enabled == true` on a platform that does NOT look air-cooled →
    ///   [`ImmersionDecision::Activated`] (a hydro / immersion rig).
    /// - `enabled == true` on a platform that DOES look air-cooled, WITHOUT the
    ///   acknowledgement → [`ImmersionDecision::RefusedAirCooled`] (fail-closed:
    ///   keep fan management, do NOT bypass it).
    /// - `enabled == true` on an air-cooled-looking platform WITH the explicit
    ///   acknowledgement → [`ImmersionDecision::ActivatedAirCooledOverride`]
    ///   (operator took responsibility for an external loop on an air chassis).
    pub fn decide(&self, platform_looks_air_cooled: bool) -> ImmersionDecision {
        if !self.enabled {
            return ImmersionDecision::Disabled;
        }
        if !platform_looks_air_cooled {
            return ImmersionDecision::Activated;
        }
        if self.acknowledge_air_cooled_override {
            ImmersionDecision::ActivatedAirCooledOverride
        } else {
            ImmersionDecision::RefusedAirCooled
        }
    }
}

/// Outcome of [`ImmersionConfig::decide`]. The controller maps each variant to
/// a `tracing` line; tests assert the variant directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImmersionDecision {
    /// Immersion mode is off (default). Fan management is unchanged.
    Disabled,
    /// Immersion mode is on for a platform that does not look air-cooled
    /// (a genuine immersion / hydro rig). Fan ramp is bypassed; the thermal
    /// safety net (hash-cut on over-temp / stale temp) stays intact.
    Activated,
    /// Immersion mode is on for an air-cooled-looking platform WITH the
    /// explicit operator acknowledgement. Same behavior as `Activated` but the
    /// controller logs a louder warning.
    ActivatedAirCooledOverride,
    /// Immersion mode was requested on an air-cooled-looking platform WITHOUT
    /// the acknowledgement. The controller FAILS CLOSED — it keeps fan
    /// management and does NOT bypass it. This is the safe refusal.
    RefusedAirCooled,
}

impl ImmersionDecision {
    /// True iff the controller should actually bypass fan-RAMP behavior.
    /// `RefusedAirCooled` and `Disabled` both keep normal fan management.
    pub fn fans_bypassed(self) -> bool {
        matches!(
            self,
            ImmersionDecision::Activated | ImmersionDecision::ActivatedAirCooledOverride
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        let cfg = ImmersionConfig::default();
        assert!(!cfg.enabled);
        assert!(!cfg.acknowledge_air_cooled_override);
        // Default config never bypasses fans, regardless of platform.
        assert_eq!(cfg.decide(false), ImmersionDecision::Disabled);
        assert_eq!(cfg.decide(true), ImmersionDecision::Disabled);
        assert!(!cfg.decide(false).fans_bypassed());
        assert!(!cfg.decide(true).fans_bypassed());
    }

    #[test]
    fn enabled_on_non_air_cooled_activates() {
        let cfg = ImmersionConfig {
            enabled: true,
            acknowledge_air_cooled_override: false,
        };
        // Hydro / immersion rig (not air-cooled) → activates without an override.
        assert_eq!(cfg.decide(false), ImmersionDecision::Activated);
        assert!(cfg.decide(false).fans_bypassed());
    }

    #[test]
    fn enabled_on_air_cooled_without_ack_refuses_fail_closed() {
        let cfg = ImmersionConfig {
            enabled: true,
            acknowledge_air_cooled_override: false,
        };
        // Air-cooled-looking platform, no acknowledgement → REFUSE (keep fans).
        assert_eq!(cfg.decide(true), ImmersionDecision::RefusedAirCooled);
        assert!(
            !cfg.decide(true).fans_bypassed(),
            "an air-cooled unit without acknowledgement must KEEP fan management"
        );
    }

    #[test]
    fn enabled_on_air_cooled_with_ack_activates_with_override() {
        let cfg = ImmersionConfig {
            enabled: true,
            acknowledge_air_cooled_override: true,
        };
        assert_eq!(
            cfg.decide(true),
            ImmersionDecision::ActivatedAirCooledOverride
        );
        assert!(cfg.decide(true).fans_bypassed());
        // The acknowledgement does NOT change the non-air-cooled path.
        assert_eq!(cfg.decide(false), ImmersionDecision::Activated);
    }

    #[test]
    fn acknowledgement_alone_does_nothing_when_disabled() {
        // A stray acknowledgement with enabled=false must never bypass fans.
        let cfg = ImmersionConfig {
            enabled: false,
            acknowledge_air_cooled_override: true,
        };
        assert_eq!(cfg.decide(false), ImmersionDecision::Disabled);
        assert_eq!(cfg.decide(true), ImmersionDecision::Disabled);
        assert!(!cfg.decide(true).fans_bypassed());
    }
}
