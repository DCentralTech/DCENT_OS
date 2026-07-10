//!  diag-A — Hashboard fault triage flowchart (HAL-free).
//!
//! Source RE evidence:
//!
//! (specifically §2.4 domain failure behavior and §3.5 chain break
//! behavior).
//!
//! Operator-facing fault classifier that takes raw observations
//! (chips-detected count, expected count, voltage-domain readings,
//! signal-chain symptoms) and emits a typed verdict + recommended
//! repair step. Complements  `diode_voltage` (per-pin probe
//! reference) by adding higher-level fault-class reasoning.
//!
//! HAL-free: pure decision tree. The runtime adapter inside
//! `asic-tester` / `dcent-toolbox` consumes the verdict to render
//! human-readable repair guidance.

use serde::{Deserialize, Serialize};

/// Categorical hashboard fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashboardFault {
    /// Board enumerates exactly the expected chip count and all
    /// signals are within range. No fault.
    Healthy,
    /// 0 chips detected — chain entry point dead OR signal Cable issue.
    NoChipsDetected,
    /// Some chips < expected — partial chain break at chip
    /// `(detected_count + 1)`.
    PartialChainBreak,
    /// All chips enumerated but no nonces returned (response chain
    /// broken — RO path).
    NoncesNotReturning,
    /// All domains within ±50 mV: healthy domain layout.
    /// (Reported when only voltage diagnostic is run.)
    DomainsHealthy,
    /// One domain ≥ 100 mV below average — partial short (failed chip
    /// or blown cap in that domain).
    DomainShortSuspect,
    /// One domain significantly above average — open circuit /
    /// broken trace / cracked solder joint.
    DomainOpenSuspect,
    /// Gradual voltage decline across domains — upstream resistance.
    DomainCascadeResistance,
    /// LDO failure — domain regulated supply lost.
    LdoFailure,
}

/// Recommended repair action for a fault class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    NoActionRequired,
    /// Inspect the 18-pin signal cable + control-board level shifters.
    InspectControlBoardCable,
    /// Replace the chip immediately after the last-detected chip.
    ReplaceChipAtBreakPoint,
    /// Reflow / replace the response-chain side (RO path).
    ReflowResponseChainPath,
    /// Locate + replace the shorted chip in the affected domain.
    ReplaceShortedChipInDomain,
    /// Locate the broken trace in the affected domain.
    LocateOpenTraceInDomain,
    /// Verify upstream power-distribution traces and current limit IC.
    InspectPowerDistribution,
    /// Replace the LDO regulator for the affected domain.
    ReplaceLdoRegulator,
}

/// Per-fault verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FaultVerdict {
    pub fault: HashboardFault,
    pub repair: RepairAction,
}

impl FaultVerdict {
    pub fn for_fault(fault: HashboardFault) -> Self {
        let repair = match fault {
            HashboardFault::Healthy | HashboardFault::DomainsHealthy => {
                RepairAction::NoActionRequired
            }
            HashboardFault::NoChipsDetected => RepairAction::InspectControlBoardCable,
            HashboardFault::PartialChainBreak => RepairAction::ReplaceChipAtBreakPoint,
            HashboardFault::NoncesNotReturning => RepairAction::ReflowResponseChainPath,
            HashboardFault::DomainShortSuspect => RepairAction::ReplaceShortedChipInDomain,
            HashboardFault::DomainOpenSuspect => RepairAction::LocateOpenTraceInDomain,
            HashboardFault::DomainCascadeResistance => RepairAction::InspectPowerDistribution,
            HashboardFault::LdoFailure => RepairAction::ReplaceLdoRegulator,
        };
        Self { fault, repair }
    }
}

/// Per-tick hashboard observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HashboardObservation {
    /// Chips detected via GetAddress enumeration.
    pub chips_detected: u32,
    /// Chips expected (per chip family / chain count).
    pub chips_expected: u32,
    /// Whether the chain returns nonces during a stress test (or 0
    /// nonces despite valid enumeration → response-chain broken).
    pub nonces_returning: bool,
}

impl HashboardObservation {
    /// Classify a chain-enumeration observation.
    pub fn classify_chain(&self) -> HashboardFault {
        if self.chips_detected == 0 {
            return HashboardFault::NoChipsDetected;
        }
        if self.chips_detected < self.chips_expected {
            return HashboardFault::PartialChainBreak;
        }
        if !self.nonces_returning {
            return HashboardFault::NoncesNotReturning;
        }
        HashboardFault::Healthy
    }

    /// Pinpoint the chain-break chip index. Returns the 1-based index
    /// of the first chip that's NOT detected (i.e. the chip immediately
    /// downstream of the last detected one). Returns `None` if the chain
    /// is fully detected (no break) or fully dead (entry point bad).
    pub fn break_point_chip_idx(&self) -> Option<u32> {
        if self.chips_detected == 0 {
            return None;
        }
        if self.chips_detected >= self.chips_expected {
            return None;
        }
        // 1-based index of the first missing chip.
        Some(self.chips_detected + 1)
    }
}

/// Classify a per-domain voltage observation against the canonical
/// ±50 mV / ±100 mV thresholds from RE doc §2.4 lines 202-206.
///
/// `domain_voltages_mv` is the slice of per-domain readings (one entry
/// per voltage domain on the board).
pub fn classify_domain_voltages(domain_voltages_mv: &[u32]) -> HashboardFault {
    if domain_voltages_mv.is_empty() {
        return HashboardFault::Healthy;
    }
    let avg: f64 =
        domain_voltages_mv.iter().map(|&v| v as f64).sum::<f64>() / domain_voltages_mv.len() as f64;
    let mut max_below: f64 = 0.0;
    let mut max_above: f64 = 0.0;
    for &v in domain_voltages_mv {
        let dev = v as f64 - avg;
        if dev < -max_below {
            max_below = -dev;
        }
        if dev > max_above {
            max_above = dev;
        }
    }
    if max_below >= 100.0 {
        return HashboardFault::DomainShortSuspect;
    }
    if max_above >= 200.0 {
        return HashboardFault::DomainOpenSuspect;
    }
    // Cascade: the domain values are monotonically decreasing AND the
    // first-to-last delta is > 50 mV. Indicates upstream resistance.
    let mut monotonic_dec = true;
    for window in domain_voltages_mv.windows(2) {
        if window[1] >= window[0] {
            monotonic_dec = false;
            break;
        }
    }
    if monotonic_dec
        && domain_voltages_mv.first().unwrap() - domain_voltages_mv.last().unwrap() > 50
    {
        return HashboardFault::DomainCascadeResistance;
    }
    if max_below <= 50.0 && max_above <= 50.0 {
        HashboardFault::DomainsHealthy
    } else {
        // Within 50-100 mV of average: not strictly healthy but not yet
        // into Short / Open territory. Flag as cascade for now (operator
        // should cross-check with diode probe).
        HashboardFault::DomainCascadeResistance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(detected: u32, expected: u32, nonces: bool) -> HashboardObservation {
        HashboardObservation {
            chips_detected: detected,
            chips_expected: expected,
            nonces_returning: nonces,
        }
    }

    #[test]
    fn full_chain_with_nonces_is_healthy() {
        let v = obs(108, 108, true).classify_chain();
        assert_eq!(v, HashboardFault::Healthy);
    }

    #[test]
    fn zero_chips_detected_signals_no_chips() {
        let v = obs(0, 108, false).classify_chain();
        assert_eq!(v, HashboardFault::NoChipsDetected);
        assert_eq!(
            FaultVerdict::for_fault(v).repair,
            RepairAction::InspectControlBoardCable
        );
    }

    #[test]
    fn partial_chain_pinpoints_break_at_next_chip() {
        // RE doc §3.5 line 283-285: detected 29 of 108 → break at chip 30.
        let o = obs(29, 108, false);
        let v = o.classify_chain();
        assert_eq!(v, HashboardFault::PartialChainBreak);
        assert_eq!(o.break_point_chip_idx(), Some(30));
        assert_eq!(
            FaultVerdict::for_fault(v).repair,
            RepairAction::ReplaceChipAtBreakPoint
        );
    }

    #[test]
    fn full_enum_no_nonces_signals_response_chain() {
        let v = obs(108, 108, false).classify_chain();
        assert_eq!(v, HashboardFault::NoncesNotReturning);
        assert_eq!(
            FaultVerdict::for_fault(v).repair,
            RepairAction::ReflowResponseChainPath
        );
    }

    #[test]
    fn break_point_none_for_full_chain() {
        let o = obs(108, 108, true);
        assert_eq!(o.break_point_chip_idx(), None);
    }

    #[test]
    fn break_point_none_for_dead_chain() {
        let o = obs(0, 108, false);
        assert_eq!(o.break_point_chip_idx(), None);
    }

    #[test]
    fn balanced_domains_within_50mv_are_healthy() {
        // RE doc §2.4: ±50 mV of average → healthy.
        let voltages = vec![13_700u32, 13_690, 13_710, 13_705];
        assert_eq!(
            classify_domain_voltages(&voltages),
            HashboardFault::DomainsHealthy
        );
    }

    #[test]
    fn domain_100mv_below_avg_signals_short() {
        let voltages = vec![13_700u32, 13_690, 13_550, 13_710];
        // 13_550 is 113 mV below the average.
        let v = classify_domain_voltages(&voltages);
        assert_eq!(v, HashboardFault::DomainShortSuspect);
    }

    #[test]
    fn domain_far_above_avg_signals_open() {
        let voltages = vec![13_700u32, 13_690, 13_710, 14_000];
        // 14_000 is 225 mV above average → open.
        let v = classify_domain_voltages(&voltages);
        assert_eq!(v, HashboardFault::DomainOpenSuspect);
    }

    #[test]
    fn monotonic_decline_signals_cascade_resistance() {
        let voltages = vec![13_750u32, 13_720, 13_690, 13_660];
        // First-to-last delta = 90 mV; monotonic decreasing.
        // Average ≈ 13,705. 13_750 - 13,705 = 45 (above), 13_705 - 13_660 = 45 (below).
        // Neither crosses 100/200 thresholds, so cascade detector kicks in.
        let v = classify_domain_voltages(&voltages);
        assert_eq!(v, HashboardFault::DomainCascadeResistance);
    }

    #[test]
    fn empty_domain_list_is_healthy() {
        assert_eq!(classify_domain_voltages(&[]), HashboardFault::Healthy);
    }

    #[test]
    fn fault_verdict_round_trips_through_serde() {
        let v = FaultVerdict::for_fault(HashboardFault::PartialChainBreak);
        let json = serde_json::to_string(&v).unwrap();
        let back: FaultVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
        assert!(json.contains("\"fault\":\"partial_chain_break\""));
        assert!(json.contains("\"repair\":\"replace_chip_at_break_point\""));
    }

    #[test]
    fn every_fault_has_a_repair_action() {
        for fault in [
            HashboardFault::Healthy,
            HashboardFault::NoChipsDetected,
            HashboardFault::PartialChainBreak,
            HashboardFault::NoncesNotReturning,
            HashboardFault::DomainsHealthy,
            HashboardFault::DomainShortSuspect,
            HashboardFault::DomainOpenSuspect,
            HashboardFault::DomainCascadeResistance,
            HashboardFault::LdoFailure,
        ] {
            let v = FaultVerdict::for_fault(fault);
            // No fault should map to NoActionRequired except the
            // healthy-equivalents.
            if matches!(
                fault,
                HashboardFault::Healthy | HashboardFault::DomainsHealthy
            ) {
                assert_eq!(v.repair, RepairAction::NoActionRequired);
            } else {
                assert_ne!(v.repair, RepairAction::NoActionRequired);
            }
        }
    }
}
