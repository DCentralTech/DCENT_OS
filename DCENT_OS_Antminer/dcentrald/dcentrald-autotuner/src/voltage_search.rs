//! Per-domain voltage optimization search.
//!
//! After frequency characterization discovers each chip's max stable frequency,
//! the voltage search finds the minimum stable voltage for the chain's voltage
//! domain. This reduces power consumption and heat without affecting hashrate.
//!
//! Algorithm (per chain / voltage domain):
//!   1. Start at the current operating voltage (e.g., 9100 mV)
//!   2. Step DOWN in 20 mV coarse steps
//!   3. At each voltage, wait one measurement window and check all chips' error rates
//!   4. If all chips stable: record as last_stable, continue stepping down
//!   5. If any chip unstable: switch to fine phase (10 mV steps) from last_stable
//!   6. Fine phase: step down in 10 mV increments until instability
//!   7. Result = min_stable_voltage + safety_margin
//!
//! On S9, each chain has one voltage domain (1 PIC per hash board), so voltage
//! optimization is per-chain. Future miners with multiple voltage domains per
//! board would run one search per domain.

/// Voltage search phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageSearchPhase {
    /// Coarse search: stepping down in large increments (default 20 mV).
    Coarse,
    /// Fine search: stepping down in small increments (default 10 mV) from
    /// the last known stable voltage found during coarse phase.
    Fine,
    /// Search complete — result is available.
    Done,
    /// W13.C1 (2026-05-10): SKU declared `voltage_fixed=true` (BHB42803).
    /// Voltage is locked at the PCB-level VRM divider value; the autotuner
    /// MUST NOT issue SET_VOLTAGE commands. State is immediately terminal —
    /// `result()` returns the initial voltage unchanged. See
    /// .
    NoOp,
}

/// Per-chain voltage search state machine.
///
/// Drives a descending voltage search to find the minimum stable voltage
/// for all chips on a chain. The autotuner advances this state machine
/// after each measurement window.
#[derive(Debug, Clone)]
pub struct VoltageSearchState {
    /// Chain ID (6, 7, or 8 on S9).
    chain_id: u8,
    /// Voltage at the start of the search (mV). Never go above this.
    initial_voltage_mv: u16,
    /// Current voltage being tested (mV).
    current_voltage_mv: u16,
    /// Minimum allowed voltage (mV). Safety floor from config.
    min_voltage_mv: u16,
    /// Safety margin to add above minimum stable voltage (mV).
    safety_margin_mv: u16,
    /// Current step size (mV). 20 for coarse, 10 for fine.
    step_mv: u16,
    /// Last voltage where all chips were stable (mV).
    last_stable_voltage_mv: u16,
    /// The discovered minimum stable voltage (mV), set when search completes.
    min_stable_voltage_mv: Option<u16>,
    /// Current search phase.
    phase: VoltageSearchPhase,
    /// Whether the caller should request a PIC voltage readback after setting
    /// the final optimized voltage. Set to true when the search transitions
    /// to Done. The daemon checks this flag and sends a VerifyVoltage command.
    readback_requested: bool,
}

impl VoltageSearchState {
    /// Coarse step size in millivolts.
    const COARSE_STEP_MV: u16 = 20;
    /// Fine step size in millivolts.
    const FINE_STEP_MV: u16 = 10;

    /// Create a new voltage search starting at `initial_voltage_mv`.
    ///
    /// - `chain_id`: which chain this search is for
    /// - `initial_voltage_mv`: current operating voltage (search starts here, never goes above)
    /// - `min_voltage_mv`: absolute minimum voltage (safety floor from config)
    /// - `safety_margin_mv`: margin added above discovered minimum stable voltage
    pub fn new(
        chain_id: u8,
        initial_voltage_mv: u16,
        min_voltage_mv: u16,
        safety_margin_mv: u16,
    ) -> Self {
        Self {
            chain_id,
            initial_voltage_mv,
            current_voltage_mv: initial_voltage_mv,
            min_voltage_mv,
            safety_margin_mv,
            step_mv: Self::COARSE_STEP_MV,
            last_stable_voltage_mv: initial_voltage_mv,
            min_stable_voltage_mv: None,
            phase: VoltageSearchPhase::Coarse,
            readback_requested: false,
        }
    }

    /// W13.C1 (2026-05-10): SKU-flag-aware constructor. If `voltage_fixed`
    /// is `true` (currently only BHB42803 — see
    /// `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku::Bhb42803`),
    /// the state machine is born in [`VoltageSearchPhase::NoOp`]: every
    /// `advance(...)` call is a no-op, [`current_voltage()`] returns the
    /// initial voltage unchanged, and [`result()`] returns the initial
    /// voltage. The caller MUST check [`is_no_op()`] (or the equivalent
    /// `phase()`) and skip any `SET_VOLTAGE` dispatch when true — bouncing
    /// SET_VOLTAGE on a fixed-V VRM corrupts the PIC MSSP parser
    /// permanently.
    ///
    /// When `voltage_fixed` is `false`, this is identical to [`Self::new`].
    pub fn new_with_pvt_flags(
        chain_id: u8,
        initial_voltage_mv: u16,
        min_voltage_mv: u16,
        safety_margin_mv: u16,
        voltage_fixed: bool,
    ) -> Self {
        if voltage_fixed {
            // Born terminal — voltage is already at the PCB-level VRM
            // value, so `last_stable` and `min_stable` BOTH equal initial.
            // `result()` clamps to initial → caller sees a stable answer
            // identical to "no search ran". `readback_requested` stays
            // false because no SET_VOLTAGE was issued; nothing to verify.
            return Self {
                chain_id,
                initial_voltage_mv,
                current_voltage_mv: initial_voltage_mv,
                min_voltage_mv,
                safety_margin_mv: 0,
                step_mv: 0,
                last_stable_voltage_mv: initial_voltage_mv,
                min_stable_voltage_mv: Some(initial_voltage_mv),
                phase: VoltageSearchPhase::NoOp,
                readback_requested: false,
            };
        }
        Self::new(
            chain_id,
            initial_voltage_mv,
            min_voltage_mv,
            safety_margin_mv,
        )
    }

    /// W13.C1: `true` if the search is in the [`VoltageSearchPhase::NoOp`]
    /// terminal state (SKU `voltage_fixed=true`). When this returns `true`
    /// the autotuner MUST NOT dispatch any `SET_VOLTAGE` command for this
    /// chain..
    pub fn is_no_op(&self) -> bool {
        self.phase == VoltageSearchPhase::NoOp
    }

    /// Get the chain ID for this search.
    pub fn chain_id(&self) -> u8 {
        self.chain_id
    }

    /// Get the current voltage being tested (mV).
    pub fn current_voltage(&self) -> u16 {
        self.current_voltage_mv
    }

    /// Get the current search phase.
    pub fn phase(&self) -> VoltageSearchPhase {
        self.phase
    }

    /// Check if the search is complete (either ran to [`VoltageSearchPhase::Done`]
    /// or was born in [`VoltageSearchPhase::NoOp`] for a `voltage_fixed` SKU).
    pub fn is_done(&self) -> bool {
        matches!(
            self.phase,
            VoltageSearchPhase::Done | VoltageSearchPhase::NoOp
        )
    }

    /// Whether a PIC voltage readback should be requested after the final
    /// voltage is applied. Returns true once when the search completes,
    /// then resets to false after the first call.
    pub fn readback_requested(&mut self) -> bool {
        if self.readback_requested {
            self.readback_requested = false;
            true
        } else {
            false
        }
    }

    /// Transition to Done state and request readback verification.
    fn finish(&mut self) {
        self.phase = VoltageSearchPhase::Done;
        self.readback_requested = true;
    }

    /// Advance the search state machine based on measurement results.
    ///
    /// `all_chips_stable`: true if ALL chips on this chain had error rates
    /// below the threshold during the measurement window at `current_voltage_mv`.
    pub fn advance(&mut self, all_chips_stable: bool) {
        match self.phase {
            // W13.C1: NoOp = born terminal for voltage_fixed SKUs. Every
            // advance() call is a no-op; current_voltage stays at initial.
            VoltageSearchPhase::NoOp => (),
            VoltageSearchPhase::Done => (),

            VoltageSearchPhase::Coarse => {
                if all_chips_stable {
                    // Current voltage is stable — record it and try lower
                    self.last_stable_voltage_mv = self.current_voltage_mv;

                    let next = self.current_voltage_mv.saturating_sub(self.step_mv);
                    if next < self.min_voltage_mv {
                        // Hit the safety floor — done. Min stable is current.
                        self.min_stable_voltage_mv = Some(self.current_voltage_mv);
                        self.finish();
                    } else {
                        self.current_voltage_mv = next;
                    }
                } else {
                    // Unstable at current voltage.
                    if self.current_voltage_mv == self.initial_voltage_mv {
                        // Already unstable at the starting voltage — cannot reduce.
                        // The initial voltage is the minimum stable voltage.
                        self.min_stable_voltage_mv = Some(self.initial_voltage_mv);
                        self.finish();
                    } else {
                        // Switch to fine search starting from last_stable - fine_step.
                        // We know last_stable was stable, so start one fine step below it.
                        self.phase = VoltageSearchPhase::Fine;
                        self.step_mv = Self::FINE_STEP_MV;
                        let next = self.last_stable_voltage_mv.saturating_sub(self.step_mv);
                        if next < self.min_voltage_mv {
                            // Can't go lower than floor — last_stable is the answer.
                            self.min_stable_voltage_mv = Some(self.last_stable_voltage_mv);
                            self.finish();
                        } else {
                            self.current_voltage_mv = next;
                        }
                    }
                }
            }

            VoltageSearchPhase::Fine => {
                if all_chips_stable {
                    // Stable at this fine voltage — record and try lower
                    self.last_stable_voltage_mv = self.current_voltage_mv;

                    let next = self.current_voltage_mv.saturating_sub(self.step_mv);
                    if next < self.min_voltage_mv {
                        // Hit the floor — done
                        self.min_stable_voltage_mv = Some(self.current_voltage_mv);
                        self.finish();
                    } else {
                        self.current_voltage_mv = next;
                    }
                } else {
                    // Unstable — last_stable is the minimum stable voltage
                    self.min_stable_voltage_mv = Some(self.last_stable_voltage_mv);
                    self.finish();
                }
            }
        }
    }

    /// Get the final optimized voltage (minimum stable + safety margin).
    ///
    /// Returns the initial voltage if the search hasn't completed yet.
    /// The result is clamped to never exceed the initial voltage.
    pub fn result(&self) -> u16 {
        match self.min_stable_voltage_mv {
            Some(min_stable) => {
                let with_margin = min_stable.saturating_add(self.safety_margin_mv);
                // Never exceed initial voltage
                with_margin.min(self.initial_voltage_mv)
            }
            None => self.initial_voltage_mv,
        }
    }

    /// Get the voltage savings achieved (mV reduction from initial).
    pub fn savings_mv(&self) -> u16 {
        self.initial_voltage_mv.saturating_sub(self.result())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state() {
        let state = VoltageSearchState::new(6, 9100, 8400, 20);
        assert_eq!(state.chain_id(), 6);
        assert_eq!(state.current_voltage(), 9100);
        assert_eq!(state.phase(), VoltageSearchPhase::Coarse);
        assert!(!state.is_done());
    }

    #[test]
    fn test_coarse_then_fine_converges() {
        // Simulate: stable down to 8900 mV, unstable at 8880 mV
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);

        // Coarse steps: 9100 -> 9080 -> 9060 -> ... -> 8900 (stable)
        // Then 8880 (unstable) -> switch to fine
        let stable_floor = 8900u16;

        let mut steps = 0;
        while !state.is_done() && steps < 100 {
            let stable = state.current_voltage() >= stable_floor;
            state.advance(stable);
            steps += 1;
        }

        assert!(state.is_done());
        // Result should be min_stable + margin = 8900 + 20 = 8920
        assert_eq!(state.result(), 8920);
        assert_eq!(state.savings_mv(), 9100 - 8920);
    }

    #[test]
    fn test_all_stable_down_to_floor() {
        // Everything is stable all the way to the minimum voltage
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);

        let mut steps = 0;
        while !state.is_done() && steps < 200 {
            state.advance(true); // always stable
            steps += 1;
        }

        assert!(state.is_done());
        // Should stop at min_voltage_mv (8400). Result = 8400 + 20 = 8420.
        assert_eq!(state.result(), 8420);
    }

    #[test]
    fn test_unstable_at_initial_voltage() {
        // Already unstable at the starting voltage — cannot reduce
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);
        state.advance(false); // unstable at 9100

        assert!(state.is_done());
        // Result should be the initial voltage itself (can't go higher)
        assert_eq!(state.result(), 9100);
    }

    #[test]
    fn test_fine_phase_finds_exact_minimum() {
        // Stable at 9100, 9080, 9060 (coarse)
        // Unstable at 9040 -> switch to fine from 9060
        // Fine: 9050 (stable), 9040 (unstable)
        // Min stable = 9050, result = 9050 + 20 = 9070
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);

        // 9100: stable
        assert_eq!(state.current_voltage(), 9100);
        state.advance(true);

        // 9080: stable
        assert_eq!(state.current_voltage(), 9080);
        state.advance(true);

        // 9060: stable
        assert_eq!(state.current_voltage(), 9060);
        state.advance(true);

        // 9040: unstable -> fine phase starts from 9060 - 10 = 9050
        assert_eq!(state.current_voltage(), 9040);
        assert_eq!(state.phase(), VoltageSearchPhase::Coarse);
        state.advance(false);

        assert_eq!(state.phase(), VoltageSearchPhase::Fine);
        assert_eq!(state.current_voltage(), 9050);

        // 9050: stable
        state.advance(true);
        assert_eq!(state.current_voltage(), 9040);

        // 9040: unstable -> done, min_stable = 9050
        state.advance(false);
        assert!(state.is_done());
        assert_eq!(state.result(), 9070); // 9050 + 20 margin
    }

    #[test]
    fn test_result_never_exceeds_initial() {
        // If min_stable + margin > initial, clamp to initial
        let mut state = VoltageSearchState::new(6, 9100, 8400, 200);

        // First step unstable at 9100 — result is capped at 9100
        state.advance(false);
        assert!(state.is_done());
        // min_stable=9100, margin=200 -> 9300, but clamped to 9100
        assert_eq!(state.result(), 9100);
    }

    #[test]
    fn test_result_before_completion() {
        let state = VoltageSearchState::new(6, 9100, 8400, 20);
        // Before search completes, result() returns initial voltage
        assert_eq!(state.result(), 9100);
    }

    #[test]
    fn test_advance_after_done_is_noop() {
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);
        state.advance(false); // instant done
        assert!(state.is_done());
        let result_before = state.result();

        state.advance(true); // should be no-op
        assert!(state.is_done());
        assert_eq!(state.result(), result_before);
    }

    #[test]
    fn test_fine_phase_hits_floor() {
        // Coarse: stable at 8420, unstable at 8400 (floor)
        // Fine should start from 8420 - 10 = 8410
        let mut state = VoltageSearchState::new(6, 8440, 8400, 20);

        // 8440: stable
        assert_eq!(state.current_voltage(), 8440);
        state.advance(true);

        // 8420: stable
        assert_eq!(state.current_voltage(), 8420);
        state.advance(true);

        // 8400: unstable -> fine phase from 8420 - 10 = 8410
        assert_eq!(state.current_voltage(), 8400);
        state.advance(false);

        assert_eq!(state.phase(), VoltageSearchPhase::Fine);
        assert_eq!(state.current_voltage(), 8410);

        // 8410: stable
        state.advance(true);

        // 8400: at floor -> done. min_stable = 8400 (if stable) or stays at 8410
        // Actually next would be 8400 which is at floor
        assert_eq!(state.current_voltage(), 8400);
        state.advance(true);

        // Hit floor in fine — done. min_stable = 8400
        assert!(state.is_done());
        assert_eq!(state.result(), 8420); // 8400 + 20 margin
    }

    #[test]
    fn test_coarse_hits_floor_directly() {
        // Start close to floor: 8440, step 20 -> 8420 -> 8400 (floor)
        let mut state = VoltageSearchState::new(6, 8440, 8400, 20);

        // 8440: stable
        state.advance(true);
        // 8420: stable
        assert_eq!(state.current_voltage(), 8420);
        state.advance(true);
        // 8400: at min, stable -> next would be 8380 which is < min_voltage
        assert_eq!(state.current_voltage(), 8400);
        state.advance(true);

        // Should be done since next step would go below floor
        assert!(state.is_done());
        assert_eq!(state.result(), 8420); // 8400 + 20 margin
    }

    #[test]
    fn test_savings_calculation() {
        let mut state = VoltageSearchState::new(6, 9100, 8400, 20);
        assert_eq!(state.savings_mv(), 0); // No savings yet

        // Run to completion with stable_floor = 8800
        while !state.is_done() {
            let stable = state.current_voltage() >= 8800;
            state.advance(stable);
        }

        let result = state.result();
        assert!(result < 9100);
        assert_eq!(state.savings_mv(), 9100 - result);
    }

    // -----------------------------------------------------------------
    // W13.C1 (2026-05-10): voltage_fixed=true SKU short-circuit. See
    //  and
    //  for the load-bearing rule:
    // BHB42803 is single-voltage at 1530 mV (PSU PCB-level VRM divider).
    // SET_VOLTAGE bouncing on a fixed-V VRM corrupts the PIC MSSP parser.
    // This MUST land before the 15-SKU PVT table activation in C2.
    // -----------------------------------------------------------------

    #[test]
    fn bhb42803_voltage_fixed_short_circuits_voltage_search() {
        // BHB42803 envelope: 84 chips × 3 chains, fixed 1530 mV.
        let mut state =
            VoltageSearchState::new_with_pvt_flags(0, 1530, 1320, 20, /*voltage_fixed=*/ true);

        // Born terminal — `is_done()` is true on construction.
        assert!(state.is_done(), "voltage_fixed=true must be born terminal");
        assert!(
            state.is_no_op(),
            "phase must be NoOp for voltage_fixed=true"
        );
        assert_eq!(state.phase(), VoltageSearchPhase::NoOp);

        // current_voltage stays at initial — no SET_VOLTAGE issued.
        assert_eq!(state.current_voltage(), 1530);

        // result() returns initial unchanged (no margin added).
        assert_eq!(state.result(), 1530);
        assert_eq!(state.savings_mv(), 0);

        // No readback requested — nothing was written, nothing to verify.
        assert!(
            !state.readback_requested(),
            "no readback for fixed-V SKU; nothing was set"
        );

        // advance() must be a no-op — call it 20 times with mixed
        // stability inputs, state must not change.
        for stable in [true, false, true, false, true]
            .into_iter()
            .cycle()
            .take(20)
        {
            state.advance(stable);
            assert!(state.is_no_op(), "phase must stay NoOp across advance()");
            assert_eq!(state.current_voltage(), 1530);
            assert_eq!(state.result(), 1530);
        }
    }

    #[test]
    fn voltage_fixed_false_falls_through_to_normal_search() {
        // Defensive: pass voltage_fixed=false; behavior must be identical
        // to `VoltageSearchState::new()`. This pins that the new
        // constructor is a *strict* superset and doesn't accidentally
        // alter normal-SKU semantics.
        let baseline = VoltageSearchState::new(6, 9100, 8400, 20);
        let via_pvt = VoltageSearchState::new_with_pvt_flags(
            6, 9100, 8400, 20, /*voltage_fixed=*/ false,
        );

        assert_eq!(baseline.chain_id(), via_pvt.chain_id());
        assert_eq!(baseline.current_voltage(), via_pvt.current_voltage());
        assert_eq!(baseline.phase(), via_pvt.phase());
        assert_eq!(baseline.is_done(), via_pvt.is_done());
        assert!(!via_pvt.is_no_op());
        assert_eq!(baseline.result(), via_pvt.result());
    }

    #[test]
    fn voltage_fixed_phase_is_distinct_from_done() {
        // Pin: NoOp and Done are distinct phases. is_done() returns true
        // for both, but is_no_op() returns true ONLY for NoOp. This lets
        // the dispatcher distinguish "search completed normally, may
        // need readback" from "search was skipped entirely, no I/O at all".
        let mut normal = VoltageSearchState::new(6, 9100, 8400, 20);
        normal.advance(false); // immediate Done at initial

        let fixed = VoltageSearchState::new_with_pvt_flags(0, 1530, 1320, 20, true);

        assert!(normal.is_done());
        assert!(fixed.is_done());
        assert_ne!(normal.phase(), fixed.phase());
        assert!(!normal.is_no_op());
        assert!(fixed.is_no_op());
    }

    #[test]
    fn voltage_fixed_initial_below_min_does_not_panic() {
        // Defensive: even with degenerate inputs (initial < min) the
        // voltage_fixed path must not panic and must not enter coarse/fine.
        let state = VoltageSearchState::new_with_pvt_flags(0, 1300, 1320, 20, true);
        assert!(state.is_no_op());
        assert_eq!(state.current_voltage(), 1300);
        assert_eq!(state.result(), 1300);
    }

    #[test]
    fn bm1398_no_voltage_search_when_voltage_fixed() {
        // Defensive parity test: voltage_fixed MUST short-circuit
        // regardless of which chip family the chain happens to mount.
        // BM1398 (S19/S19 Pro) doesn't ship a fixed-V hashboard today,
        // but the autotuner contract is voltage-fixed-aware at the SKU
        // layer, NOT at the chip layer. If a future BM1398 hashboard
        // SKU declares voltage_fixed=true, the same rule MUST apply.
        let bm1398_chain_voltage_mv = 13_800; // S19 Pro chain rail
        let state = VoltageSearchState::new_with_pvt_flags(
            7,
            bm1398_chain_voltage_mv,
            12_500,
            20,
            /*voltage_fixed=*/ true,
        );
        assert!(state.is_no_op());
        assert_eq!(state.current_voltage(), bm1398_chain_voltage_mv);
        assert_eq!(state.result(), bm1398_chain_voltage_mv);
    }

    #[test]
    fn test_zero_margin() {
        let mut state = VoltageSearchState::new(6, 9100, 8400, 0);

        // Stable down to 9000, unstable at 8980
        while !state.is_done() {
            let stable = state.current_voltage() >= 9000;
            state.advance(stable);
        }

        // With zero margin, result should be exactly the minimum stable voltage
        // Fine phase finds ~9000 as the floor
        let result = state.result();
        assert!(result >= 9000);
        assert!(result <= 9010); // Could be 9000 exactly or 9000+fine_step depending on exact convergence
    }
}
