//! PowerCut + FanCommand — single chokepoint types for emergency / teardown (home-first).
//!
//! # Why
//!
//! Thermal, hybrid hard-stop, serial panic hooks, and daemon EmergencyShutdown
//! historically encoded “cut power then cap fans” in parallel. That drift caused
//! docs to claim “fans 100%” while code used `PWM_SAFETY_MAX` (30). These pure
//! types are the **policy language** every path should speak; HAL adapters apply
//! them without inventing new numeric magic.
//!
//! # Status
//!
//! Pure policy + validation only. Wiring live `disable()` / `set_speed()` call
//! sites is a strangler follow-up (behavior-preserving). New emergency code
//! should construct [`SafetyAction`] rather than raw PWM 100/127.

/// Home residential fan safety ceiling (matches HAL `PWM_SAFETY_MAX` intent).
pub const HOME_FAN_PWM_SAFETY_MAX: u8 = 30;

/// Absolute hardware PWM scale upper bound used on many Antminer IP blocks.
pub const FAN_PWM_ABSOLUTE_MAX: u8 = 100;

/// Ordered emergency / teardown intent (cut hash before noise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyAction {
    /// Disable hashboard voltage / rails only (fans unchanged). Carries the
    /// PowerCut so its forensic reason survives into `steps()`.
    PowerCutOnly(PowerCut),
    /// Cut power, then command fans to a capped PWM. Carries the PowerCut so
    /// `steps()` emits the ACTUAL cut reason (e.g. PanicTeardown vs
    /// ThermalEmergency) instead of a hardcoded one.
    PowerCutThenFan(PowerCut, FanCommand),
    /// Fan-only (immersion may zero fans without power cut — rare).
    FanOnly(FanCommand),
}

/// Fan command with mandatory safety intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanCommand {
    /// Profile / operator max (from config `fan_max_pwm`).
    pub profile_max_pwm: u8,
    /// Requested PWM before safety clamp.
    pub requested_pwm: u8,
    /// Whether home safety cap applies (default true for residential images).
    pub apply_home_safety_cap: bool,
}

impl FanCommand {
    /// Quiet park for home units after hard-stop / panic teardown.
    pub const fn home_quiet_park(profile_max_pwm: u8) -> Self {
        Self {
            profile_max_pwm,
            requested_pwm: 0,
            apply_home_safety_cap: true,
        }
    }

    /// Emergency fan command: never exceeds profile ∩ home safety (when enabled).
    pub const fn emergency_cap(profile_max_pwm: u8) -> Self {
        Self {
            profile_max_pwm,
            requested_pwm: HOME_FAN_PWM_SAFETY_MAX,
            apply_home_safety_cap: true,
        }
    }

    /// Effective PWM after policy (the only number adapters should write).
    pub fn effective_pwm(self) -> u8 {
        let profile = self.profile_max_pwm.min(FAN_PWM_ABSOLUTE_MAX);
        let mut pwm = self.requested_pwm.min(profile);
        if self.apply_home_safety_cap {
            pwm = pwm.min(HOME_FAN_PWM_SAFETY_MAX);
        }
        pwm
    }
}

/// Power-cut intent (voltage rails / PSU enable / PIC disable — backend-specific).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerCut {
    /// Human-readable reason for logs / evidence.
    pub reason: PowerCutReason,
    /// Prefer voltage disable before any fan raise (home-first).
    pub cut_hash_before_noise: bool,
}

/// Why power was cut (stable for forensics / API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCutReason {
    ThermalEmergency,
    FanFailure,
    PicHeartbeatMiss,
    OperatorSafeOff,
    PanicTeardown,
    InitFailure,
    Watchdog,
    Other,
}

impl PowerCut {
    pub const fn thermal_emergency() -> Self {
        Self {
            reason: PowerCutReason::ThermalEmergency,
            cut_hash_before_noise: true,
        }
    }

    pub const fn panic_teardown() -> Self {
        Self {
            reason: PowerCutReason::PanicTeardown,
            cut_hash_before_noise: true,
        }
    }

    /// Compose the canonical home emergency sequence.
    pub fn with_emergency_fans(self, profile_max_pwm: u8) -> SafetyAction {
        debug_assert!(self.cut_hash_before_noise);
        SafetyAction::PowerCutThenFan(self, FanCommand::emergency_cap(profile_max_pwm))
    }
}

/// Returns true if a raw PWM would violate home safety when cap is required.
pub fn violates_home_fan_cap(pwm: u8, apply_home_safety_cap: bool) -> bool {
    apply_home_safety_cap && pwm > HOME_FAN_PWM_SAFETY_MAX
}

/// Ordered steps for applying a [`SafetyAction`] (cut-hash-before-noise).
///
/// Adapters execute steps in order. Power is always before fan raise on
/// thermal/panic paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyStep {
    /// Disable rails / voltage / PWR_CONTROL.
    CutPower(PowerCut),
    /// Command fans to an effective PWM (after policy clamp).
    CommandFans(FanCommand),
}

impl SafetyAction {
    /// Expand into ordered adapter steps.
    pub fn steps(self) -> Vec<SafetyStep> {
        match self {
            Self::PowerCutOnly(cut) => vec![SafetyStep::CutPower(cut)],
            Self::PowerCutThenFan(cut, fan) => {
                vec![SafetyStep::CutPower(cut), SafetyStep::CommandFans(fan)]
            }
            Self::FanOnly(fan) => vec![SafetyStep::CommandFans(fan)],
        }
    }
}

impl PowerCut {
    /// Canonical home thermal hard-stop: cut power, then emergency fan cap.
    pub fn home_thermal_hard_stop_action(profile_max_pwm: u8) -> SafetyAction {
        Self::thermal_emergency().with_emergency_fans(profile_max_pwm)
    }

    /// Canonical panic park after power cut: quiet idle (not the louder cap).
    pub fn home_panic_park_action(profile_max_pwm: u8) -> SafetyAction {
        SafetyAction::PowerCutThenFan(
            Self::panic_teardown(),
            FanCommand::home_quiet_park(profile_max_pwm),
        )
    }
}

/// Assert cut-hash-before-noise ordering for a step list.
pub fn power_precedes_fan_raise(steps: &[SafetyStep]) -> bool {
    let mut saw_fan = false;
    for step in steps {
        match step {
            SafetyStep::CommandFans(fan) if fan.effective_pwm() > 0 => {
                saw_fan = true;
            }
            SafetyStep::CutPower(_) if saw_fan => return false,
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emergency_fans_never_reach_100_under_home_cap() {
        let cmd = FanCommand::emergency_cap(100);
        assert_eq!(cmd.effective_pwm(), HOME_FAN_PWM_SAFETY_MAX);
        assert!(cmd.effective_pwm() < 100);
        assert!(!violates_home_fan_cap(cmd.effective_pwm(), true));
    }

    #[test]
    fn profile_below_safety_max_is_respected() {
        let cmd = FanCommand::emergency_cap(20);
        assert_eq!(cmd.effective_pwm(), 20);
    }

    #[test]
    fn thermal_sequence_is_cut_then_fan() {
        let action = PowerCut::thermal_emergency().with_emergency_fans(30);
        match action {
            SafetyAction::PowerCutThenFan(cut, fan) => {
                assert_eq!(fan.effective_pwm(), 30);
                // The cut's real reason is carried (not hardcoded in steps()).
                assert_eq!(cut.reason, PowerCutReason::ThermalEmergency);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn panic_park_steps_report_panic_teardown_not_thermal() {
        // D2: steps() must emit the CARRIED PowerCut reason, not a hardcoded one.
        // home_panic_park is a PanicTeardown — steps() previously mislabeled it
        // ThermalEmergency (the stable forensic reason was lost).
        let steps = PowerCut::home_panic_park_action(30).steps();
        match steps[0] {
            SafetyStep::CutPower(cut) => assert_eq!(cut.reason, PowerCutReason::PanicTeardown),
            other => panic!("expected CutPower first, got {other:?}"),
        }
        // Thermal hard-stop still reports ThermalEmergency (carried, not hardcoded).
        let steps = PowerCut::home_thermal_hard_stop_action(30).steps();
        assert!(matches!(
            steps[0],
            SafetyStep::CutPower(PowerCut {
                reason: PowerCutReason::ThermalEmergency,
                ..
            })
        ));
    }

    #[test]
    fn home_quiet_park_is_zero() {
        assert_eq!(FanCommand::home_quiet_park(30).effective_pwm(), 0);
    }

    #[test]
    fn industrial_path_can_opt_out_of_home_cap() {
        let cmd = FanCommand {
            profile_max_pwm: 100,
            requested_pwm: 80,
            apply_home_safety_cap: false,
        };
        assert_eq!(cmd.effective_pwm(), 80);
    }

    #[test]
    fn thermal_hard_stop_steps_cut_power_before_fans() {
        let action = PowerCut::home_thermal_hard_stop_action(30);
        let steps = action.steps();
        assert!(matches!(steps[0], SafetyStep::CutPower(_)));
        assert!(matches!(steps[1], SafetyStep::CommandFans(_)));
        assert!(power_precedes_fan_raise(&steps));
    }

    #[test]
    fn panic_park_steps_are_cut_then_quiet() {
        let steps = PowerCut::home_panic_park_action(30).steps();
        assert_eq!(steps.len(), 2);
        if let SafetyStep::CommandFans(fan) = steps[1] {
            assert_eq!(fan.effective_pwm(), 0);
        } else {
            panic!("expected fan step");
        }
    }
}
