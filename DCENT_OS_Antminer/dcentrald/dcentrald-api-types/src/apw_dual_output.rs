//!  psu-C — APW12 dual-output architecture DTOs (HAL-free).
//!
//! Source RE evidence:
//!  §1.4
//! (APW12 Dual-Output Architecture, lines 71-90).
//!
//! APW12 PSUs ship two independent DC outputs from a single AC input:
//!
//! - **OUT1** (J3/J4): Main adjustable hash-board rail, 12-15 V, up to
//!   233 A (3,600 W max). Cut by the PSU watchdog when the host stops
//!   pinging via I²C cmd 0x81.
//! - **OUT2** (J6): Fixed 12 V control-board rail, 15 A max. ALWAYS
//!   ON — survives PSU watchdog timeout. The control plane (kernel,
//!   dcentrald, dashboard, SSH) keeps running even when the hash boards
//!   go dark.
//!
//! This split is the load-bearing reason a missed PIC heartbeat takes
//! the chains offline WITHOUT bricking the unit. Pinning these
//! semantics in tests defends against a refactor that would treat
//! "PSU" as a single rail.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// One of the two APW12 DC outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApwOutput {
    /// J3/J4 — main 12-15 V hash-board rail.
    Out1HashBoard,
    /// J6 — fixed 12 V control-board + fan rail.
    Out2ControlBoard,
}

impl ApwOutput {
    /// Operator-facing connector label.
    pub fn connector(&self) -> &'static str {
        match self {
            Self::Out1HashBoard => "J3/J4",
            Self::Out2ControlBoard => "J6",
        }
    }

    /// Which physical role this output supplies.
    pub fn role(&self) -> &'static str {
        match self {
            Self::Out1HashBoard => "hash_boards",
            Self::Out2ControlBoard => "control_board_and_fans",
        }
    }

    /// True iff the PSU watchdog (cmd 0x81) cuts this rail when the
    /// host fails to ping. OUT2 is INDEPENDENT of the watchdog — the
    /// control plane survives.
    pub fn watchdog_can_cut(&self) -> bool {
        matches!(self, Self::Out1HashBoard)
    }

    /// Voltage range in volts (min, max).
    pub fn voltage_range(&self) -> (f32, f32) {
        match self {
            Self::Out1HashBoard => (12.0, 15.0),
            Self::Out2ControlBoard => (12.0, 12.0),
        }
    }

    /// Maximum current draw in amperes.
    pub fn max_current_a(&self) -> f32 {
        match self {
            Self::Out1HashBoard => 233.0,
            Self::Out2ControlBoard => 15.0,
        }
    }

    /// Maximum wattage — `voltage * current` at the high end.
    pub fn max_power_w(&self) -> f32 {
        let (_, v_max) = self.voltage_range();
        v_max * self.max_current_a()
    }
}

/// Both outputs in canonical order (OUT1 first, OUT2 second).
pub const ALL_OUTPUTS: [ApwOutput; 2] = [ApwOutput::Out1HashBoard, ApwOutput::Out2ControlBoard];

// ---------------------------------------------------------------------------
// Architecture-level constants
// ---------------------------------------------------------------------------

/// Number of independent AC inputs to APW12.
pub const APW12_AC_INPUT_COUNT: u32 = 2;

/// AC input voltage range (V).
pub const APW12_AC_VOLTAGE_MIN: u32 = 200;
pub const APW12_AC_VOLTAGE_MAX: u32 = 240;

/// Internal PFC bus voltage (DC) across C1/C2.
pub const APW12_PFC_BUS_VOLTAGE_MIN: u32 = 410;
pub const APW12_PFC_BUS_VOLTAGE_MAX: u32 = 420;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out1_is_the_hash_board_rail() {
        // RE doc line 84: "OUT1 (J3/J4): Main adjustable output,
        // 12-15V, up to 233A (3600W max)".
        let o1 = ApwOutput::Out1HashBoard;
        assert_eq!(o1.connector(), "J3/J4");
        assert_eq!(o1.role(), "hash_boards");
        assert_eq!(o1.voltage_range(), (12.0, 15.0));
        assert_eq!(o1.max_current_a(), 233.0);
        // 15.0 * 233.0 = 3495 W (≈ 3600 W nameplate).
        let p = o1.max_power_w();
        assert!(
            (3450.0..=3600.0).contains(&p),
            "OUT1 max power {} W should be near 3600 W nameplate",
            p
        );
    }

    #[test]
    fn out2_is_the_control_board_rail() {
        // RE doc line 85: "OUT2 (J6): Fixed 12V output, 15A max, for
        // control board and cooling fans".
        let o2 = ApwOutput::Out2ControlBoard;
        assert_eq!(o2.connector(), "J6");
        assert_eq!(o2.role(), "control_board_and_fans");
        assert_eq!(o2.voltage_range(), (12.0, 12.0));
        assert_eq!(o2.max_current_a(), 15.0);
        // 12.0 * 15.0 = 180 W.
        assert!((o2.max_power_w() - 180.0).abs() < 1e-3);
    }

    #[test]
    fn watchdog_cuts_only_out1() {
        // CRITICAL: PSU watchdog (cmd 0x81 timeout) cuts OUT1 only.
        // OUT2 stays — control plane survives. Pin so a refactor
        // doesn't classify OUT2 as watchdog-managed (which would
        // imply a dashboard-killing latent bug).
        assert!(ApwOutput::Out1HashBoard.watchdog_can_cut());
        assert!(!ApwOutput::Out2ControlBoard.watchdog_can_cut());
    }

    #[test]
    fn out2_voltage_range_is_fixed_12v() {
        // RE doc: OUT2 is fixed 12 V — min == max. A refactor that
        // made OUT2 adjustable would silently break the control-plane
        // assumption.
        let (v_min, v_max) = ApwOutput::Out2ControlBoard.voltage_range();
        assert_eq!(v_min, 12.0);
        assert_eq!(v_max, 12.0);
    }

    #[test]
    fn out1_voltage_range_is_adjustable_12_to_15v() {
        let (v_min, v_max) = ApwOutput::Out1HashBoard.voltage_range();
        assert_eq!(v_min, 12.0);
        assert_eq!(v_max, 15.0);
        assert!(v_max > v_min);
    }

    #[test]
    fn all_outputs_listed_in_canonical_order() {
        // OUT1 first, OUT2 second.
        assert_eq!(ALL_OUTPUTS.len(), 2);
        assert_eq!(ALL_OUTPUTS[0], ApwOutput::Out1HashBoard);
        assert_eq!(ALL_OUTPUTS[1], ApwOutput::Out2ControlBoard);
    }

    #[test]
    fn ac_input_count_is_two_independent_phases() {
        // RE doc line 87: "Two independent AC inputs: Each feeds its
        // own EMI filter and PFC stage".
        assert_eq!(APW12_AC_INPUT_COUNT, 2);
    }

    #[test]
    fn ac_input_voltage_range_is_200_to_240() {
        assert_eq!(APW12_AC_VOLTAGE_MIN, 200);
        assert_eq!(APW12_AC_VOLTAGE_MAX, 240);
    }

    #[test]
    fn pfc_bus_voltage_in_documented_range() {
        // RE doc line 88: "PFC outputs: DC 410-420V across large
        // capacitors (C1/C2)".
        assert_eq!(APW12_PFC_BUS_VOLTAGE_MIN, 410);
        assert_eq!(APW12_PFC_BUS_VOLTAGE_MAX, 420);
        assert!(APW12_PFC_BUS_VOLTAGE_MIN < APW12_PFC_BUS_VOLTAGE_MAX);
    }

    #[test]
    fn out1_carries_more_current_than_out2() {
        // Hash-board rail must always carry more current than
        // control-board rail. Pin so a swapped refactor jumps out.
        assert!(
            ApwOutput::Out1HashBoard.max_current_a() > ApwOutput::Out2ControlBoard.max_current_a()
        );
    }

    #[test]
    fn output_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ApwOutput::Out1HashBoard).unwrap(),
            "\"out1_hash_board\""
        );
        assert_eq!(
            serde_json::to_string(&ApwOutput::Out2ControlBoard).unwrap(),
            "\"out2_control_board\""
        );
    }

    #[test]
    fn output_round_trips_through_serde() {
        for o in [ApwOutput::Out1HashBoard, ApwOutput::Out2ControlBoard] {
            let json = serde_json::to_string(&o).unwrap();
            let back: ApwOutput = serde_json::from_str(&json).unwrap();
            assert_eq!(o, back);
        }
    }
}
