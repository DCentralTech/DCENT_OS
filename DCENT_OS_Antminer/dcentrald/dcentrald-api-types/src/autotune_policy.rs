//! Host-side autotune policy DTOs and classifiers.
//!
//! This module is deliberately HAL-free. It classifies operator-provided or
//! already-published telemetry samples; it never polls miners, opens device
//! files, or issues tuning commands.

use crate::chip_init::ChipFamily;
use crate::profile_schema::{
    SOURCE_RANK_LIVE_CONFIRMED, SOURCE_RANK_OPERATOR_CONFIRMED, SOURCE_RANK_VENDOR_EXTRACTED,
};
use crate::thermal_model::{vnish_profile_decision, VnishProfileAction};
use serde::{Deserialize, Serialize};

/// Chain-level metric supported by the imbalance classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainImbalanceMetric {
    Hashrate,
    Temperature,
    EstimatedWatts,
}

/// One caller-owned chain metric sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChainMetricSample {
    pub chain_id: u8,
    pub value: Option<f64>,
}

/// Thresholds for classifying cross-chain skew.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChainImbalanceThresholds {
    /// Minimum usable chains before a skew classification can be meaningful.
    pub min_valid_chains: usize,
    /// Warning threshold for hashrate spread: `(max - min) / max * 100`.
    pub hashrate_warn_pct: f64,
    /// Critical threshold for hashrate spread.
    pub hashrate_critical_pct: f64,
    /// Warning threshold for board/chip temperature spread in degrees C.
    pub temperature_warn_c: f64,
    /// Critical threshold for board/chip temperature spread in degrees C.
    pub temperature_critical_c: f64,
    /// Warning threshold for estimated-watts spread: `(max - min) / avg * 100`.
    pub estimated_watts_warn_pct: f64,
    /// Critical threshold for estimated-watts spread.
    pub estimated_watts_critical_pct: f64,
}

impl Default for ChainImbalanceThresholds {
    fn default() -> Self {
        Self {
            min_valid_chains: 2,
            hashrate_warn_pct: 12.0,
            hashrate_critical_pct: 25.0,
            temperature_warn_c: 8.0,
            temperature_critical_c: 15.0,
            estimated_watts_warn_pct: 15.0,
            estimated_watts_critical_pct: 30.0,
        }
    }
}

/// Fail-closed severity returned by host-side policy classifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainImbalanceSeverity {
    /// No usable sample set was provided.
    Unavailable,
    /// Inputs were present but invalid, such as NaN or a nonsensical threshold.
    Invalid,
    /// Cross-chain skew is inside the warning band.
    Balanced,
    /// Skew is high enough to surface to policy/UI but not yet critical.
    Warning,
    /// Skew is high enough that auto profile switching should not scale up.
    Critical,
}

impl Default for ChainImbalanceSeverity {
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Result from a single metric imbalance classification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainImbalanceClassification {
    pub metric: ChainImbalanceMetric,
    pub severity: ChainImbalanceSeverity,
    pub valid_chains: usize,
    pub min_chain_id: Option<u8>,
    pub max_chain_id: Option<u8>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub skew: Option<f64>,
    pub threshold_warn: f64,
    pub threshold_critical: f64,
    pub reason: String,
}

impl ChainImbalanceClassification {
    pub fn blocks_profile_step_up(&self) -> bool {
        matches!(
            self.severity,
            ChainImbalanceSeverity::Warning
                | ChainImbalanceSeverity::Critical
                | ChainImbalanceSeverity::Invalid
        )
    }
}

fn thresholds_for_metric(
    metric: ChainImbalanceMetric,
    thresholds: &ChainImbalanceThresholds,
) -> (f64, f64) {
    match metric {
        ChainImbalanceMetric::Hashrate => (
            thresholds.hashrate_warn_pct,
            thresholds.hashrate_critical_pct,
        ),
        ChainImbalanceMetric::Temperature => (
            thresholds.temperature_warn_c,
            thresholds.temperature_critical_c,
        ),
        ChainImbalanceMetric::EstimatedWatts => (
            thresholds.estimated_watts_warn_pct,
            thresholds.estimated_watts_critical_pct,
        ),
    }
}

fn skew_for_metric(metric: ChainImbalanceMetric, min: f64, max: f64, avg: f64) -> Option<f64> {
    match metric {
        ChainImbalanceMetric::Temperature => Some(max - min),
        ChainImbalanceMetric::Hashrate => (max > 0.0).then_some(((max - min) / max) * 100.0),
        ChainImbalanceMetric::EstimatedWatts => (avg > 0.0).then_some(((max - min) / avg) * 100.0),
    }
}

pub fn classify_chain_imbalance(
    metric: ChainImbalanceMetric,
    samples: &[ChainMetricSample],
    thresholds: ChainImbalanceThresholds,
) -> ChainImbalanceClassification {
    let (warn, critical) = thresholds_for_metric(metric, &thresholds);
    if thresholds.min_valid_chains < 2 || warn < 0.0 || critical < warn {
        return ChainImbalanceClassification {
            metric,
            severity: ChainImbalanceSeverity::Invalid,
            valid_chains: 0,
            min_chain_id: None,
            max_chain_id: None,
            min_value: None,
            max_value: None,
            skew: None,
            threshold_warn: warn,
            threshold_critical: critical,
            reason: "invalid_thresholds".to_string(),
        };
    }

    let valid: Vec<(u8, f64)> = samples
        .iter()
        .filter_map(|sample| sample.value.map(|value| (sample.chain_id, value)))
        .collect();

    if valid.len() < thresholds.min_valid_chains {
        return ChainImbalanceClassification {
            metric,
            severity: ChainImbalanceSeverity::Unavailable,
            valid_chains: valid.len(),
            min_chain_id: None,
            max_chain_id: None,
            min_value: None,
            max_value: None,
            skew: None,
            threshold_warn: warn,
            threshold_critical: critical,
            reason: "not_enough_valid_chains".to_string(),
        };
    }

    if valid
        .iter()
        .any(|(_, value)| !value.is_finite() || *value < 0.0)
    {
        return ChainImbalanceClassification {
            metric,
            severity: ChainImbalanceSeverity::Invalid,
            valid_chains: valid.len(),
            min_chain_id: None,
            max_chain_id: None,
            min_value: None,
            max_value: None,
            skew: None,
            threshold_warn: warn,
            threshold_critical: critical,
            reason: "invalid_sample_value".to_string(),
        };
    }

    let (min_chain_id, min_value) = valid
        .iter()
        .copied()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .expect("valid has at least min_valid_chains");
    let (max_chain_id, max_value) = valid
        .iter()
        .copied()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .expect("valid has at least min_valid_chains");
    let avg = valid.iter().map(|(_, value)| *value).sum::<f64>() / valid.len() as f64;

    let Some(skew) = skew_for_metric(metric, min_value, max_value, avg) else {
        return ChainImbalanceClassification {
            metric,
            severity: ChainImbalanceSeverity::Invalid,
            valid_chains: valid.len(),
            min_chain_id: Some(min_chain_id),
            max_chain_id: Some(max_chain_id),
            min_value: Some(min_value),
            max_value: Some(max_value),
            skew: None,
            threshold_warn: warn,
            threshold_critical: critical,
            reason: "skew_denominator_zero".to_string(),
        };
    };

    let (severity, reason) = if skew >= critical {
        (ChainImbalanceSeverity::Critical, "critical_skew")
    } else if skew >= warn {
        (ChainImbalanceSeverity::Warning, "warning_skew")
    } else {
        (ChainImbalanceSeverity::Balanced, "within_threshold")
    };

    ChainImbalanceClassification {
        metric,
        severity,
        valid_chains: valid.len(),
        min_chain_id: Some(min_chain_id),
        max_chain_id: Some(max_chain_id),
        min_value: Some(min_value),
        max_value: Some(max_value),
        skew: Some(skew),
        threshold_warn: warn,
        threshold_critical: critical,
        reason: reason.to_string(),
    }
}

/// Policy wrapper for profile auto-switching.
///
/// `Default` is intentionally off. Deserializing `{}` or omitting the field
/// must not allow a thermal/profile helper to step presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileAutoSwitchPolicy {
    pub enabled: bool,
    pub allow_step_up: bool,
    pub allow_step_down: bool,
    pub block_step_up_on_chain_imbalance: bool,
    pub min_dwell_s: u32,
    pub imbalance_thresholds: ChainImbalanceThresholds,
}

impl Default for ProfileAutoSwitchPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_step_up: true,
            allow_step_down: true,
            block_step_up_on_chain_imbalance: true,
            min_dwell_s: 300,
            imbalance_thresholds: ChainImbalanceThresholds::default(),
        }
    }
}

/// Output from the profile auto-switch policy wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileAutoSwitchDecision {
    pub action: VnishProfileAction,
    pub policy_enabled: bool,
    pub blocked_by_policy: bool,
    pub reason: String,
}

pub fn profile_auto_switch_decision(
    policy: &ProfileAutoSwitchPolicy,
    temp_c: f32,
    fan_pwm_percent: u8,
    sustained_above_seconds: u32,
    sustained_below_seconds: u32,
    chain_imbalances: &[ChainImbalanceClassification],
) -> ProfileAutoSwitchDecision {
    if !policy.enabled {
        return ProfileAutoSwitchDecision {
            action: VnishProfileAction::Hold,
            policy_enabled: false,
            blocked_by_policy: true,
            reason: "profile_auto_switch_disabled".to_string(),
        };
    }

    let action = vnish_profile_decision(
        temp_c,
        fan_pwm_percent,
        sustained_above_seconds,
        sustained_below_seconds,
    );

    if action == VnishProfileAction::StepUp
        && policy.block_step_up_on_chain_imbalance
        && chain_imbalances
            .iter()
            .any(ChainImbalanceClassification::blocks_profile_step_up)
    {
        return ProfileAutoSwitchDecision {
            action: VnishProfileAction::Hold,
            policy_enabled: true,
            blocked_by_policy: true,
            reason: "step_up_blocked_by_chain_imbalance".to_string(),
        };
    }

    if action == VnishProfileAction::StepUp && !policy.allow_step_up {
        return ProfileAutoSwitchDecision {
            action: VnishProfileAction::Hold,
            policy_enabled: true,
            blocked_by_policy: true,
            reason: "step_up_disabled_by_policy".to_string(),
        };
    }

    if action == VnishProfileAction::StepDown && !policy.allow_step_down {
        return ProfileAutoSwitchDecision {
            action: VnishProfileAction::Hold,
            policy_enabled: true,
            blocked_by_policy: true,
            reason: "step_down_disabled_by_policy".to_string(),
        };
    }

    ProfileAutoSwitchDecision {
        action,
        policy_enabled: true,
        blocked_by_policy: false,
        reason: "vnish_profile_decision_applied".to_string(),
    }
}

/// Source evidence for enabling full TABS on newer Bitmain families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabsTargetProofSource {
    None,
    VendorExtracted,
    OperatorConfirmed,
    LiveConfirmed,
}

impl TabsTargetProofSource {
    pub fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::VendorExtracted => SOURCE_RANK_VENDOR_EXTRACTED,
            Self::OperatorConfirmed => SOURCE_RANK_OPERATOR_CONFIRMED,
            Self::LiveConfirmed => SOURCE_RANK_LIVE_CONFIRMED,
        }
    }
}

impl Default for TabsTargetProofSource {
    fn default() -> Self {
        Self::None
    }
}

/// Target evidence required before full TABS can be advertised for newer ASICs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabsTargetProof {
    pub source: TabsTargetProofSource,
    pub has_hashrate_target: bool,
    pub has_estimated_watts_target: bool,
    pub has_frequency_voltage_targets: bool,
}

impl TabsTargetProof {
    pub fn is_sufficient_for_full_tabs(self) -> bool {
        self.source.rank() >= SOURCE_RANK_VENDOR_EXTRACTED
            && self.has_hashrate_target
            && self.has_estimated_watts_target
            && self.has_frequency_voltage_targets
    }
}

/// Host-side full-TABS gate state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabsEnablement {
    /// Full TABS is proven usable for this family/profile target.
    FullTabs,
    /// Family is known, but full TABS must remain off until target proof exists.
    TargetProofRequired,
    /// Family is outside the full-TABS support surface.
    UnsupportedFamily,
}

pub fn full_tabs_enablement_for_family(
    family: ChipFamily,
    proof: TabsTargetProof,
) -> TabsEnablement {
    match family {
        ChipFamily::Bm1387 => TabsEnablement::FullTabs,
        ChipFamily::Bm1398
        | ChipFamily::Bm1362
        | ChipFamily::Bm1366
        | ChipFamily::Bm1368
        | ChipFamily::Bm1370 => {
            if proof.is_sufficient_for_full_tabs() {
                TabsEnablement::FullTabs
            } else {
                TabsEnablement::TargetProofRequired
            }
        }
        _ => TabsEnablement::UnsupportedFamily,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(chain_id: u8, value: f64) -> ChainMetricSample {
        ChainMetricSample {
            chain_id,
            value: Some(value),
        }
    }

    #[test]
    fn hashrate_imbalance_classifies_low_chain_skew() {
        let result = classify_chain_imbalance(
            ChainImbalanceMetric::Hashrate,
            &[sample(6, 33.0), sample(7, 31.0), sample(8, 24.0)],
            ChainImbalanceThresholds::default(),
        );

        assert_eq!(result.severity, ChainImbalanceSeverity::Critical);
        assert_eq!(result.min_chain_id, Some(8));
        assert_eq!(result.max_chain_id, Some(6));
        assert!(result.skew.unwrap() >= 25.0);
    }

    #[test]
    fn temperature_imbalance_uses_absolute_celsius_delta() {
        let result = classify_chain_imbalance(
            ChainImbalanceMetric::Temperature,
            &[sample(6, 58.0), sample(7, 64.0), sample(8, 68.5)],
            ChainImbalanceThresholds::default(),
        );

        assert_eq!(result.severity, ChainImbalanceSeverity::Warning);
        assert_eq!(result.skew, Some(10.5));
        assert_eq!(result.max_chain_id, Some(8));
    }

    #[test]
    fn estimated_watts_imbalance_uses_average_relative_skew() {
        let result = classify_chain_imbalance(
            ChainImbalanceMetric::EstimatedWatts,
            &[sample(6, 480.0), sample(7, 520.0), sample(8, 700.0)],
            ChainImbalanceThresholds::default(),
        );

        assert_eq!(result.severity, ChainImbalanceSeverity::Critical);
        assert_eq!(result.max_chain_id, Some(8));
        assert!(result.skew.unwrap() > 30.0);
    }

    #[test]
    fn imbalance_classifier_fails_closed_on_missing_or_invalid_samples() {
        let missing = classify_chain_imbalance(
            ChainImbalanceMetric::Hashrate,
            &[ChainMetricSample {
                chain_id: 6,
                value: None,
            }],
            ChainImbalanceThresholds::default(),
        );
        assert_eq!(missing.severity, ChainImbalanceSeverity::Unavailable);

        let invalid = classify_chain_imbalance(
            ChainImbalanceMetric::Temperature,
            &[sample(6, 60.0), sample(7, f64::NAN)],
            ChainImbalanceThresholds::default(),
        );
        assert_eq!(invalid.severity, ChainImbalanceSeverity::Invalid);
    }

    #[test]
    fn profile_auto_switch_default_is_off_and_holds() {
        let policy = ProfileAutoSwitchPolicy::default();
        assert!(!policy.enabled);

        let decision = profile_auto_switch_decision(&policy, 86.0, 50, 60, 0, &[]);
        assert_eq!(decision.action, VnishProfileAction::Hold);
        assert!(decision.blocked_by_policy);
        assert_eq!(decision.reason, "profile_auto_switch_disabled");
    }

    #[test]
    fn profile_auto_switch_empty_json_deserializes_disabled() {
        let policy: ProfileAutoSwitchPolicy = serde_json::from_str("{}").unwrap();
        assert!(!policy.enabled);
        assert!(policy.allow_step_down);
        assert!(policy.allow_step_up);
    }

    #[test]
    fn profile_auto_switch_applies_vnish_action_when_enabled() {
        let policy = ProfileAutoSwitchPolicy {
            enabled: true,
            ..Default::default()
        };

        let decision = profile_auto_switch_decision(&policy, 86.0, 50, 60, 0, &[]);
        assert_eq!(decision.action, VnishProfileAction::StepDown);
        assert!(!decision.blocked_by_policy);
    }

    #[test]
    fn profile_auto_switch_blocks_step_up_when_chain_is_imbalanced() {
        let policy = ProfileAutoSwitchPolicy {
            enabled: true,
            ..Default::default()
        };
        let imbalance = classify_chain_imbalance(
            ChainImbalanceMetric::Hashrate,
            &[sample(6, 33.0), sample(7, 30.0), sample(8, 20.0)],
            ChainImbalanceThresholds::default(),
        );

        let decision = profile_auto_switch_decision(&policy, 55.0, 40, 0, 60, &[imbalance]);
        assert_eq!(decision.action, VnishProfileAction::Hold);
        assert_eq!(decision.reason, "step_up_blocked_by_chain_imbalance");
    }

    #[test]
    fn modern_sha256_families_require_target_proof_for_full_tabs() {
        for family in [
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            assert_eq!(
                full_tabs_enablement_for_family(family, TabsTargetProof::default()),
                TabsEnablement::TargetProofRequired,
                "{:?} must not advertise full TABS without target proof",
                family
            );
        }
    }

    #[test]
    fn modern_sha256_full_tabs_gate_opens_only_with_complete_target_proof() {
        let incomplete = TabsTargetProof {
            source: TabsTargetProofSource::VendorExtracted,
            has_hashrate_target: true,
            has_estimated_watts_target: true,
            has_frequency_voltage_targets: false,
        };
        assert_eq!(
            full_tabs_enablement_for_family(ChipFamily::Bm1368, incomplete),
            TabsEnablement::TargetProofRequired
        );

        let complete = TabsTargetProof {
            source: TabsTargetProofSource::VendorExtracted,
            has_hashrate_target: true,
            has_estimated_watts_target: true,
            has_frequency_voltage_targets: true,
        };
        assert_eq!(
            full_tabs_enablement_for_family(ChipFamily::Bm1368, complete),
            TabsEnablement::FullTabs
        );
    }

    #[test]
    fn unsupported_or_placeholder_families_do_not_enter_full_tabs_gate() {
        for family in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1485,
            ChipFamily::Bm1489,
            ChipFamily::Bm1360,
            ChipFamily::Bm1491,
        ] {
            assert_eq!(
                full_tabs_enablement_for_family(family, TabsTargetProof::default()),
                TabsEnablement::UnsupportedFamily
            );
        }
    }
}
