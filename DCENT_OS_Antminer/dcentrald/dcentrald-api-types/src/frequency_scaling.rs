//!  frq-A — Initial-frequency-ramp planner + cores_per_chip
//! lookup (HAL-free).
//!
//! Source RE evidence:
//!
//! ALGO 1 (lines 14-49) + per-family `cores_per_chip` table (line 47).
//!
//! ALGO 1 — Initial frequency ramp (cold boot):
//! - Step 1: Set chain voltage to startup_volt (~9.4 V S9 / 14.5-15.2 V x19/x21).
//! - Step 9: Linearly ramp PLL from startup_freq to default_freq in
//!   `freq_step` MHz increments (default 5 MHz/step) at 200-500 ms per step.
//!
//! Per-family cores_per_chip is the canonical hashrate-divisor for autotune
//! decisions. **HAZARD**: BM1362 has 4 cores per chip (NOT 894); using 894
//! overestimates by ~220×.
//!
//! HAL-free: pure planner. The runtime adapter inside
//! `dcentrald-autotuner` consumes the ramp sequence and threads the
//! per-step delay over real time.

use crate::chip_init::ChipFamily;
use serde::{Deserialize, Serialize};

/// Default startup frequency (universal across the fleet per RE doc §1).
pub const STARTUP_FREQ_MHZ: u32 = 100;

/// Default ramp-step size in MHz (matches VNish's 5 MHz UI quantization).
pub const DEFAULT_FREQ_STEP_MHZ: u32 = 5;

/// Default per-step ramp delay (mid-of-range from RE doc §1 line 45).
pub const DEFAULT_RAMP_STEP_MS: u64 = 350;

/// Default thermal-stabilization wait at default_freq before autotune
/// state machine takes over (RE doc §1 line 46).
pub const DEFAULT_SETTLE_SECONDS: u64 = 60;

/// Quiet/efficiency preset settle window (RE doc §1 line 46).
pub const QUIET_SETTLE_SECONDS: u64 = 120;

/// Cores-per-chip lookup per chip family (RE doc §1 line 47).
///
/// **HAZARD** (RE doc): BM1362 = 4, NOT 894. BM1368 has 80 big × 16
/// small = 1280 small cores. Using the wrong number wrecks the
/// autotuner's hashrate model.
pub fn cores_per_chip(family: ChipFamily) -> u32 {
    match family {
        ChipFamily::Bm1387 => 114,
        ChipFamily::Bm1397 => 672,
        ChipFamily::Bm1398 => 672,
        ChipFamily::Bm1362 => 4,
        ChipFamily::Bm1366 => 894,
        ChipFamily::Bm1368 => 1280,
        ChipFamily::Bm1370 => 1280,
        //  W5-A: scrypt families. BM1485 = 12 cores per
        // `BM1485_CORE_NUM` from cgminer-ltc (`bm1485.rs:83`).
        // BM1489 placeholder pending  live capture.
        ChipFamily::Bm1485 => 12,
        ChipFamily::Bm1489 => 12,
        //  W8-A: NAMED-ONLY placeholder. 0 cores is a
        // deliberate refuse-to-mine sentinel — autotuner / hashrate
        // model treats `cores_per_chip == 0` as "do not dispatch
        // work to this chain". chip parameters genuinely UNKNOWN
        // per W7-A. [GAP — wave-9 live verification needed]
        ChipFamily::Bm1360 => 0,
        ChipFamily::Bm1491 => 0,
    }
}

/// Frequency ramp configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrequencyRampPlan {
    pub startup_freq_mhz: u32,
    pub default_freq_mhz: u32,
    pub step_mhz: u32,
    pub ms_per_step: u64,
    pub settle_seconds: u64,
}

impl FrequencyRampPlan {
    /// Default plan: 100 → default at 5 MHz/step, 350 ms per step,
    /// 60 s settle.
    pub fn standard(default_freq_mhz: u32) -> Self {
        Self {
            startup_freq_mhz: STARTUP_FREQ_MHZ,
            default_freq_mhz,
            step_mhz: DEFAULT_FREQ_STEP_MHZ,
            ms_per_step: DEFAULT_RAMP_STEP_MS,
            settle_seconds: DEFAULT_SETTLE_SECONDS,
        }
    }

    /// Quiet preset: same shape but 120 s settle (RE doc §1 line 46).
    pub fn quiet(default_freq_mhz: u32) -> Self {
        Self {
            startup_freq_mhz: STARTUP_FREQ_MHZ,
            default_freq_mhz,
            step_mhz: DEFAULT_FREQ_STEP_MHZ,
            ms_per_step: DEFAULT_RAMP_STEP_MS,
            settle_seconds: QUIET_SETTLE_SECONDS,
        }
    }
}

/// Build the explicit ramp sequence — a list of frequency targets in
/// MHz, starting from `startup_freq_mhz` and ending at `default_freq_mhz`.
///
/// - Always includes both endpoints.
/// - Intermediate steps are `step_mhz` apart.
/// - Handles ascending (startup < default) and descending (startup >
///   default) cases.
/// - `step_mhz == 0` collapses to `[startup, default]` (no intermediate).
/// - `startup == default` returns `[startup]` (single-element).
pub fn build_ramp(plan: &FrequencyRampPlan) -> Vec<u32> {
    let start = plan.startup_freq_mhz;
    let end = plan.default_freq_mhz;
    if start == end {
        return vec![start];
    }
    if plan.step_mhz == 0 {
        return vec![start, end];
    }
    let mut steps = Vec::new();
    if start < end {
        let mut cur = start;
        while cur < end {
            steps.push(cur);
            cur = cur.saturating_add(plan.step_mhz);
            if cur > end {
                break;
            }
        }
        steps.push(end);
    } else {
        let mut cur = start;
        while cur > end {
            steps.push(cur);
            cur = cur.saturating_sub(plan.step_mhz);
            if cur < end {
                break;
            }
        }
        steps.push(end);
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cores_per_chip_table_matches_re_doc_verbatim() {
        // RE doc §1 line 47 verbatim:
        //   BM1387=114, BM1397=672, BM1398=672,
        //   BM1362=4, BM1366=894, BM1368=894 (80×16=1280 small),
        //   BM1370=1280
        assert_eq!(cores_per_chip(ChipFamily::Bm1387), 114);
        assert_eq!(cores_per_chip(ChipFamily::Bm1397), 672);
        assert_eq!(cores_per_chip(ChipFamily::Bm1398), 672);
        assert_eq!(cores_per_chip(ChipFamily::Bm1362), 4);
        assert_eq!(cores_per_chip(ChipFamily::Bm1366), 894);
        assert_eq!(cores_per_chip(ChipFamily::Bm1368), 1280);
        assert_eq!(cores_per_chip(ChipFamily::Bm1370), 1280);
    }

    #[test]
    fn bm1362_cores_must_be_4_not_894() {
        // RE doc HAZARD callout. Using 894 overestimates hashrate by ~220×.
        assert_eq!(cores_per_chip(ChipFamily::Bm1362), 4);
    }

    #[test]
    fn standard_plan_uses_canonical_constants() {
        let p = FrequencyRampPlan::standard(650);
        assert_eq!(p.startup_freq_mhz, 100);
        assert_eq!(p.default_freq_mhz, 650);
        assert_eq!(p.step_mhz, 5);
        assert_eq!(p.ms_per_step, 350);
        assert_eq!(p.settle_seconds, 60);
    }

    #[test]
    fn quiet_plan_lengthens_settle() {
        let p = FrequencyRampPlan::quiet(545);
        assert_eq!(p.settle_seconds, 120);
        assert_eq!(p.step_mhz, 5);
    }

    #[test]
    fn build_ramp_ascending_endpoints_match() {
        let p = FrequencyRampPlan::standard(120);
        let ramp = build_ramp(&p);
        assert_eq!(*ramp.first().unwrap(), 100);
        assert_eq!(*ramp.last().unwrap(), 120);
        // 100, 105, 110, 115, 120 — 5 steps.
        assert_eq!(ramp, vec![100, 105, 110, 115, 120]);
    }

    #[test]
    fn build_ramp_does_not_overshoot_default() {
        // 100 → 113 with step 5 should NOT have a step at 115 — must
        // clamp at the documented endpoint.
        let p = FrequencyRampPlan {
            startup_freq_mhz: 100,
            default_freq_mhz: 113,
            step_mhz: 5,
            ms_per_step: 350,
            settle_seconds: 60,
        };
        let ramp = build_ramp(&p);
        assert_eq!(*ramp.last().unwrap(), 113);
        assert!(!ramp.iter().any(|f| *f > 113));
    }

    #[test]
    fn build_ramp_descending_endpoints_match() {
        // Useful for thermal throttle scenarios.
        let p = FrequencyRampPlan {
            startup_freq_mhz: 650,
            default_freq_mhz: 600,
            step_mhz: 10,
            ms_per_step: 350,
            settle_seconds: 60,
        };
        let ramp = build_ramp(&p);
        assert_eq!(*ramp.first().unwrap(), 650);
        assert_eq!(*ramp.last().unwrap(), 600);
        assert_eq!(ramp, vec![650, 640, 630, 620, 610, 600]);
    }

    #[test]
    fn build_ramp_zero_step_collapses_to_endpoints() {
        let p = FrequencyRampPlan {
            startup_freq_mhz: 100,
            default_freq_mhz: 650,
            step_mhz: 0,
            ms_per_step: 350,
            settle_seconds: 60,
        };
        let ramp = build_ramp(&p);
        assert_eq!(ramp, vec![100, 650]);
    }

    #[test]
    fn build_ramp_same_endpoints_returns_single_value() {
        let p = FrequencyRampPlan::standard(100);
        let ramp = build_ramp(&p);
        assert_eq!(ramp, vec![100]);
    }

    #[test]
    fn build_ramp_step_count_matches_5mhz_cadence() {
        // 100 → 650 at 5 MHz/step = 110 unique frequencies (incl both ends).
        let p = FrequencyRampPlan::standard(650);
        let ramp = build_ramp(&p);
        assert_eq!(ramp.len(), 111);
        for window in ramp.windows(2) {
            assert_eq!(window[1] - window[0], 5);
        }
    }

    #[test]
    fn frequency_ramp_plan_round_trips_through_serde() {
        let p = FrequencyRampPlan::standard(545);
        let json = serde_json::to_string(&p).unwrap();
        let back: FrequencyRampPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
