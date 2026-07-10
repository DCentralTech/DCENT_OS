//!  ramp-A — LuxOS 10-min boot-to-mining ramp curve (HAL-free).
//!
//! Source RE evidence:
//!
//! (1431 lines, live capture from S19j Pro `a lab unit` running LuxOS).
//!
//! The doc reconstructs the **exact LuxOS autotuner ramp behavior**
//! observed live — from cold-boot (CSV begin) to steady-state mining
//! (~10 min later). Captured per-tick power / hashrate / temperature
//! values let us pin the canonical milestones:
//! - T+0 — CSV starts (PIC controller starting).
//! - T+~10 s — three chains enumerate (126 chips each, parallel reset).
//! - T+~45 s — open-core voltage ramp to 14.92 V.
//! - T+~50 s — first hashrate reading (~3 GH/s, open-core noise).
//! - T+~5 min — steady-state warm-up nearly complete.
//! - T+~10 min — autotune converged; voltage trimmed back to 13.8 V.
//!
//! HAL-free pure DTO + classifier. The runtime adapter uses this to
//! render the dashboard "boot progress" indicator AND to flag a
//! "ramp stuck" warning if the live ramp deviates substantially from
//! the documented curve.

use serde::{Deserialize, Serialize};

/// Discrete ramp milestone observed in the live LuxOS trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RampMilestone {
    /// CSV starts; PIC controller starting.
    CsvBegin,
    /// All three chains enumerated (126 chips each).
    ChainsEnumerated,
    /// Cold-environment adjustment applied (per RE doc lines 98-101).
    ColdEnvAdjusted,
    /// Open-core voltage ramp to 14.92 V (boardctrl::pre_init).
    OpenCoreVoltageRamp,
    /// First non-zero hashrate reading (open-core noise shares).
    FirstHashrate,
    /// Frequency ramp begins (post-open-core).
    FreqRampStart,
    /// Voltage trimmed back from 14.92 V to autotune-target (~13.8 V).
    VoltageTrimmed,
    /// Steady-state warm-up complete.
    SteadyStateWarmup,
    /// Autotuner has converged (per-chip freq stable).
    AutotuneConverged,
}

/// Reference point in the LuxOS canonical ramp curve. Captures the
/// expected wall-clock offset (seconds since CSV begin), the expected
/// power draw, and a brief description.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RampPoint {
    pub milestone: RampMilestone,
    /// Expected seconds since CsvBegin.
    pub at_seconds: u32,
    /// Expected wall watts (range center; runtime adapter checks within
    /// ±20 % tolerance).
    pub expected_watts: u32,
    /// Expected hashrate in TH/s. 0 for milestones before first hash.
    pub expected_hashrate_th: f32,
    /// One-line operator-facing description.
    pub description: &'static str,
}

/// Canonical ramp points distilled from O-live-performance.md.
/// Numbers reflect the LuxOS .79 trace (S19j Pro, BM1362, 3 chains × 126
/// chips). Other models scale ±20 % from these anchors.
pub const LUXOS_S19J_PRO_RAMP: &[RampPoint] = &[
    RampPoint {
        milestone: RampMilestone::CsvBegin,
        at_seconds: 0,
        expected_watts: 0,
        expected_hashrate_th: 0.0,
        description: "CSV begins; PIC controller starting on all chains",
    },
    RampPoint {
        milestone: RampMilestone::ChainsEnumerated,
        at_seconds: 15,
        expected_watts: 0,
        expected_hashrate_th: 0.0,
        description: "All three chains enumerate full 126 chips",
    },
    RampPoint {
        milestone: RampMilestone::ColdEnvAdjusted,
        at_seconds: 45,
        expected_watts: 470,
        expected_hashrate_th: 0.0,
        description: "Cold-environment profile adjustment (T<27C ambient)",
    },
    RampPoint {
        milestone: RampMilestone::OpenCoreVoltageRamp,
        at_seconds: 47,
        expected_watts: 590,
        expected_hashrate_th: 0.0,
        description: "Voltage ramped to 14.92 V (open-core overshoot)",
    },
    RampPoint {
        milestone: RampMilestone::FirstHashrate,
        at_seconds: 115,
        expected_watts: 400,
        expected_hashrate_th: 3.18,
        description: "First non-zero hashrate (~3 GH/s open-core noise)",
    },
    RampPoint {
        milestone: RampMilestone::FreqRampStart,
        at_seconds: 130,
        expected_watts: 1000,
        expected_hashrate_th: 30.0,
        description: "Frequency ramp begins (5 MHz/step)",
    },
    RampPoint {
        milestone: RampMilestone::VoltageTrimmed,
        at_seconds: 200,
        expected_watts: 2500,
        expected_hashrate_th: 80.0,
        description: "Voltage trimmed from 14.92 V to autotune target ~13.8 V",
    },
    RampPoint {
        milestone: RampMilestone::SteadyStateWarmup,
        at_seconds: 300,
        expected_watts: 3000,
        expected_hashrate_th: 100.0,
        description: "Steady-state warm-up nearly complete",
    },
    RampPoint {
        milestone: RampMilestone::AutotuneConverged,
        at_seconds: 600,
        expected_watts: 3300,
        expected_hashrate_th: 110.0,
        description: "Autotune converged; per-chip freq stable",
    },
];

/// Look up the canonical reference point for a milestone.
pub fn (milestone: RampMilestone) -> Option<&'static RampPoint> {
    LUXOS_S19J_PRO_RAMP
        .iter()
        .find(|p| p.milestone == milestone)
}

/// Verdict for a measured ramp observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RampVerdict {
    /// Ramp is on schedule (within ±20 % of canonical curve).
    OnTrack,
    /// Ramp is meaningfully ahead of schedule (faster than expected).
    Ahead,
    /// Ramp is more than 50 % beyond the expected wall-clock — flag
    /// as "stuck" for operator review.
    Stuck,
}

/// Classify a measured (milestone, observed_seconds) tuple against the
/// canonical curve.
pub fn classify_ramp_progress(milestone: RampMilestone, observed_seconds: u32) -> RampVerdict {
    let p = match (milestone) {
        Some(p) => p,
        None => return RampVerdict::OnTrack,
    };
    if p.at_seconds == 0 {
        // CsvBegin is the anchor; observed_seconds 0 is fine, anything
        // else is somehow before-zero (impossible) or close to zero.
        return RampVerdict::OnTrack;
    }
    let expected = p.at_seconds;
    let lo = expected.saturating_sub(expected / 5); // 80 %
    let hi_stuck = expected + expected / 2; // 150 %
    if observed_seconds < lo {
        RampVerdict::Ahead
    } else if observed_seconds <= hi_stuck {
        RampVerdict::OnTrack
    } else {
        RampVerdict::Stuck
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_curve_has_nine_milestones() {
        assert_eq!(LUXOS_S19J_PRO_RAMP.len(), 9);
    }

    #[test]
    fn milestones_in_chronological_order() {
        for window in LUXOS_S19J_PRO_RAMP.windows(2) {
            assert!(
                window[1].at_seconds >= window[0].at_seconds,
                "milestone {:?} starts before {:?}",
                window[1].milestone,
                window[0].milestone
            );
        }
    }

    #[test]
    fn csv_begin_anchors_at_t_zero() {
        let p = (RampMilestone::CsvBegin).unwrap();
        assert_eq!(p.at_seconds, 0);
        assert_eq!(p.expected_watts, 0);
        assert_eq!(p.expected_hashrate_th, 0.0);
    }

    #[test]
    fn first_hashrate_anchored_at_re_doc_value() {
        // RE doc line 157: T+~115s "FIRST HASHRATE: 3,179,960 H/s = 3.18 GH/s".
        let p = (RampMilestone::FirstHashrate).unwrap();
        assert_eq!(p.expected_hashrate_th, 3.18);
        assert_eq!(p.at_seconds, 115);
    }

    #[test]
    fn open_core_voltage_at_canonical_14_92_v() {
        // RE doc line 103: "Ramping board voltage 0/1/2 → voltage=14.92".
        // This is the open-core overshoot canonical value.
        let p = (RampMilestone::OpenCoreVoltageRamp).unwrap();
        // Description must reference 14.92 V verbatim.
        assert!(p.description.contains("14.92"));
    }

    #[test]
    fn voltage_trimmed_milestone_includes_138_value() {
        // RE doc: voltage trimmed back from 14.92 to autotune target ~13.8 V.
        let p = (RampMilestone::VoltageTrimmed).unwrap();
        assert!(p.description.contains("13.8"));
    }

    #[test]
    fn classify_ramp_within_tolerance_returns_on_track() {
        // FirstHashrate at 115 s; observed 100 s is within 80 % - 120 %.
        let v = classify_ramp_progress(RampMilestone::FirstHashrate, 100);
        assert_eq!(v, RampVerdict::OnTrack);
        // Observed 130 s also within 120 %.
        let v = classify_ramp_progress(RampMilestone::FirstHashrate, 130);
        assert_eq!(v, RampVerdict::OnTrack);
    }

    #[test]
    fn classify_ramp_significantly_late_returns_stuck() {
        // FirstHashrate at 115 s; observed 250 s is way beyond 150 %.
        let v = classify_ramp_progress(RampMilestone::FirstHashrate, 250);
        assert_eq!(v, RampVerdict::Stuck);
    }

    #[test]
    fn classify_ramp_significantly_early_returns_ahead() {
        // FirstHashrate at 115 s; observed 30 s is way ahead.
        let v = classify_ramp_progress(RampMilestone::FirstHashrate, 30);
        assert_eq!(v, RampVerdict::Ahead);
    }

    #[test]
    fn ramp_milestone_round_trips_through_serde() {
        for m in [
            RampMilestone::CsvBegin,
            RampMilestone::ChainsEnumerated,
            RampMilestone::ColdEnvAdjusted,
            RampMilestone::OpenCoreVoltageRamp,
            RampMilestone::FirstHashrate,
            RampMilestone::FreqRampStart,
            RampMilestone::VoltageTrimmed,
            RampMilestone::SteadyStateWarmup,
            RampMilestone::AutotuneConverged,
        ] {
            let json = serde_json::to_string(&m).unwrap();
            let back: RampMilestone = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn ramp_point_serializes_to_documented_shape() {
        let p = (RampMilestone::FirstHashrate).unwrap();
        let json = serde_json::to_string(p).unwrap();
        assert!(json.contains("\"milestone\":\"first_hashrate\""));
        assert!(json.contains("\"at_seconds\":115"));
    }
}
