//! am2 PSU bypass gate — the "Loki bypass" path for non-smart PSUs.
//!
//! When an am2 (Zynq) S19j Pro runs a *non-smart* PSU — e.g. an APW3 tweaked
//! to ~12.8 V via the "Loki Mod" — there is no APW121215a at I2C `0x10` to
//! probe, no watchdog to disable, no DAC to program. But the hardware power
//! gate is unchanged: the `PWR_CONTROL` line must still be asserted before the
//! hashboard rail comes up (the APW3 output enable is still wired through it on
//! a Loki-modded chassis, exactly like the stock APW12 was).
//!
//! `PsuBypassGate` is what the am2 hybrid cold-boot uses *instead of*
//! `Apw121215a` when `[power.psu_override].enabled` is set. It owns the same
//! `PWR_CONTROL` GPIO that `Apw121215a` would have owned (so there is exactly
//! one owner of that line — they are mutually exclusive on a given unit) and
//! records the operator-declared PSU model + input/rail voltage so the rest of
//! the daemon can report efficiency/power context. It performs **no** I2C.
//!
//! The declared rail voltage is the PSU output / hashboard-DC-DC *input*
//! (~12.8 V for the APW3 Loki Mod, ~15.2 V for a stock APW12). It is **not**
//! the per-chain chip-rail voltage that the hashboard dsPIC regulates to
//! (~13.7 V setpoint on am2) — that stays a separate `[mining].voltage_mv`
//! concern and is never derived from this value.

use crate::psu_gpio_gate::PsuGpioGate;
use crate::Result;

/// Scoped PSU-bypass guard: asserts `PWR_CONTROL` and records the
/// operator-declared PSU model + rail voltage.
///
/// On `Drop` the inner [`PsuGpioGate`] restores `PWR_CONTROL` to its
/// pre-asserted state — the same teardown guarantee `Apw121215a` provides via
/// its own `Drop`.
pub struct PsuBypassGate {
    gate: PsuGpioGate,
    declared_model: String,
    declared_rail_v: f64,
}

impl PsuBypassGate {
    /// Assert the `PWR_CONTROL` gate for a Loki-bypass (non-smart PSU) unit.
    ///
    /// `gate_spec` is the same `[psu].pwr_control_gpio` value the `Apw121215a`
    /// path uses — `None` -> `label:PWR_CONTROL` (fall back to gpio901),
    /// `label:NAME`, `gpio:N`, or bare `N`. Fails closed if the line cannot be
    /// driven via sysfs (e.g. kernel-claimed / EBUSY), exactly like
    /// `Apw121215a::cold_boot_sequence_gated`.
    ///
    /// `model` is the operator-declared PSU model string ("APW3", "APW7", …);
    /// `rail_v` is the declared PSU output / hashboard-DC-DC-input voltage.
    pub fn assert(gate_spec: Option<&str>, model: String, rail_v: f64) -> Result<Self> {
        let gate = PsuGpioGate::assert(gate_spec)?;
        tracing::info!(
            gpio = gate.gpio(),
            model = %model,
            rail_v,
            "PSU bypass gate asserted (non-smart PSU; no APW I2C)"
        );
        Ok(Self {
            gate,
            declared_model: model,
            declared_rail_v: rail_v,
        })
    }

    /// The Linux global GPIO number backing `PWR_CONTROL`.
    pub fn gpio(&self) -> u32 {
        self.gate.gpio()
    }

    /// Whether `PWR_CONTROL` is currently asserted.
    pub fn is_asserted(&self) -> bool {
        self.gate.is_asserted()
    }

    /// Operator-declared PSU model string ("APW3", "APW7", …).
    pub fn declared_model(&self) -> &str {
        &self.declared_model
    }

    /// Operator-declared PSU output / hashboard-DC-DC-input voltage (volts).
    ///
    /// This is the *rail* voltage, NOT the per-chain chip-rail setpoint.
    pub fn declared_rail_v(&self) -> f64 {
        self.declared_rail_v
    }

    /// Restore `PWR_CONTROL` to its pre-asserted state (idempotent).
    pub fn deassert(&mut self) -> Result<()> {
        self.gate.deassert()
    }
}

impl Drop for PsuBypassGate {
    fn drop(&mut self) {
        // The inner PsuGpioGate's own Drop does the sysfs restore + logging;
        // this exists only to make the bypass teardown visible in traces.
        tracing::info!(
            gpio = self.gate.gpio(),
            model = %self.declared_model,
            "PSU bypass gate releasing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_report_declared_metadata() {
        // PsuGpioGate::assert hits sysfs, so use the test-only constructor to
        // build a guard without touching /sys, mirroring how psu_gpio_gate.rs
        // keeps its sysfs path out of unit tests.
        let bypass = PsuBypassGate {
            gate: PsuGpioGate::for_test(907),
            declared_model: "APW3".to_string(),
            declared_rail_v: 12.8,
        };
        assert_eq!(bypass.gpio(), 907);
        assert!(bypass.is_asserted());
        assert_eq!(bypass.declared_model(), "APW3");
        assert!((bypass.declared_rail_v() - 12.8).abs() < f64::EPSILON);
        // Drop here: PsuBypassGate::drop logs, then PsuGpioGate::drop attempts
        // the sysfs restore (errors without /sys, swallowed by its Drop) — must
        // not panic.
    }
}
