//!  psu-A — APW PSU family catalog (HAL-free).
//!
//! Source RE evidence:
//!  §1 (lines
//! 35-69).
//!
//! Bitmain's APW power-supply family covers 11 generations from APW3
//! through APW17. The catalog encodes:
//! - Voltage / current / wattage limits per model.
//! - AC input range + efficiency.
//! - Compatible miner families.
//! - Whether the PSU supports voltage feedback (APW121215d/e/f/g do;
//!   a/b/c don't —).
//! - Replacement compatibility (d/e/f can replace a/b/c with a firmware
//!   upgrade, but NOT vice-versa).
//!
//! HAL-free: pure data + lookup helpers. The runtime adapter inside
//! `dcentrald-hal::psu` consumes this catalog to decide which telemetry
//! reads are safe and which model-specific quirks to apply.

use serde::{Deserialize, Serialize};

// `Deserialize` is used by ApwModel (which has no static fields); ApwSpec
// is `Serialize`-only.

/// Discrete APW PSU model identifiers. Order is "earliest generation
/// first" — APW3 → APW17.  covers every model in the public
/// AntMiner-PSU family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApwModel {
    /// APW3 / APW3++ — S9 / S7 / S9i / T9 era. Up to 1600 W @ 220 V.
    Apw3,
    /// APW5 — early S5/S7 deployments.
    Apw5,
    /// APW7 — higher-current S9 variant + L3+/D3/T9+ supported.
    Apw7,
    /// APW8 8-9.2 V variant (DR5).
    Apw8LowV,
    /// APW8 10-11 V variant (S11).
    Apw8MidV,
    /// APW8 16-20 V variant (S15/T15).
    Apw8HighV,
    /// APW9 — S17 / S17 Pro / T17.
    Apw9,
    /// APW9+ — S17+ / S17e / T17+ / T17e.
    Apw9Plus,
    /// APW12 1215a (no voltage feedback).
    Apw12_1215a,
    /// APW12 1215b (no voltage feedback).
    Apw12_1215b,
    /// APW12 1215c (no voltage feedback).
    Apw12_1215c,
    /// APW12 1215d (voltage feedback — earliest revision with telemetry).
    Apw12_1215d,
    /// APW12 1215e (voltage feedback + EMC revision).
    Apw12_1215e,
    /// APW12 1215f (voltage feedback + EMC revision).
    Apw12_1215f,
    /// APW12 1215g (latest revision).
    Apw12_1215g,
    /// APW12 1417 — L7 / K7 / DR7 / HS3 / KA3.
    Apw12_1417,
    /// APW12A — fixed 12 V, no voltage adjustment.
    Apw12A,
    /// APW17 1215 — S21 family.
    Apw17_1215,
}

/// Static specification for one APW model.
///
/// `Deserialize` is intentionally NOT derived: `label` and
/// `compatible_miners` are `&'static str` (model spec lives in `match`
/// arms), and serde can't deserialize into static borrows from runtime
/// input. Clients consuming the JSON should define their own
/// owned-string struct mirror.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ApwSpec {
    /// Lowest output voltage the model supports, in volts.
    pub voltage_min_v: f32,
    /// Highest output voltage the model supports, in volts.
    pub voltage_max_v: f32,
    /// Maximum sustained output current, in amps. `None` for "varies"
    /// entries in the RE doc.
    pub max_current_a: Option<u32>,
    /// Maximum wattage @ 220 V AC.
    pub max_wattage_220v_w: Option<u32>,
    /// Maximum wattage @ 110 V AC (only some models support 110 V).
    pub max_wattage_110v_w: Option<u32>,
    /// AC input range minimum, in volts.
    pub ac_input_min_v: u32,
    /// AC input range maximum, in volts.
    pub ac_input_max_v: u32,
    /// Typical efficiency, in percent (basis-point precision is overkill).
    pub efficiency_pct: u32,
    /// Whether the PSU exposes a voltage-feedback ADC. Per
    /// , only some 1215 revisions do.
    pub has_voltage_feedback: bool,
    /// Operator-facing label for fleet UI / dashboard / docs.
    pub label: &'static str,
    /// Compatible miner families, comma-joined for dashboard display.
    pub compatible_miners: &'static str,
}

impl ApwModel {
    /// Look up the static spec for this model.
    pub fn spec(&self) -> ApwSpec {
        match self {
            ApwModel::Apw3 => ApwSpec {
                voltage_min_v: 11.6,
                voltage_max_v: 13.0,
                max_current_a: Some(133),
                max_wattage_220v_w: Some(1600),
                max_wattage_110v_w: Some(1200),
                ac_input_min_v: 100,
                ac_input_max_v: 264,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW3 / APW3++",
                compatible_miners: "S7, S9, S9i, S9j, S9k, SE, T9",
            },
            ApwModel::Apw5 => ApwSpec {
                voltage_min_v: 12.0,
                voltage_max_v: 12.0,
                max_current_a: Some(125),
                max_wattage_220v_w: Some(1500),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW5",
                compatible_miners: "S5, S7 (early)",
            },
            ApwModel::Apw7 => ApwSpec {
                voltage_min_v: 11.6,
                voltage_max_v: 13.0,
                max_current_a: Some(150),
                max_wattage_220v_w: Some(1800),
                max_wattage_110v_w: Some(1000),
                ac_input_min_v: 100,
                ac_input_max_v: 264,
                efficiency_pct: 95,
                has_voltage_feedback: false,
                label: "APW7",
                compatible_miners: "S9, S9i, L3+, D3, T9+, Z9",
            },
            ApwModel::Apw8LowV => ApwSpec {
                voltage_min_v: 8.0,
                voltage_max_v: 9.2,
                max_current_a: None,
                max_wattage_220v_w: None,
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW8 (8-9.2 V)",
                compatible_miners: "DR5",
            },
            ApwModel::Apw8MidV => ApwSpec {
                voltage_min_v: 10.0,
                voltage_max_v: 11.0,
                max_current_a: None,
                max_wattage_220v_w: None,
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW8 (10-11 V)",
                compatible_miners: "S11",
            },
            ApwModel::Apw8HighV => ApwSpec {
                voltage_min_v: 16.32,
                voltage_max_v: 20.04,
                max_current_a: None,
                max_wattage_220v_w: None,
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW8 (16-20 V)",
                compatible_miners: "S15, T15",
            },
            ApwModel::Apw9 => ApwSpec {
                voltage_min_v: 14.5,
                voltage_max_v: 21.0,
                max_current_a: Some(170),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 90,
                has_voltage_feedback: false,
                label: "APW9",
                compatible_miners: "S17, S17 Pro, T17",
            },
            ApwModel::Apw9Plus => ApwSpec {
                voltage_min_v: 14.5,
                voltage_max_v: 21.0,
                max_current_a: Some(200),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 90,
                has_voltage_feedback: false,
                label: "APW9+",
                compatible_miners: "S17+, S17e, T17+, T17e",
            },
            ApwModel::Apw12_1215a | ApwModel::Apw12_1215b | ApwModel::Apw12_1215c => ApwSpec {
                voltage_min_v: 12.0,
                voltage_max_v: 15.0,
                max_current_a: Some(233),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 94,
                has_voltage_feedback: false,
                label: match self {
                    ApwModel::Apw12_1215a => "APW121215a",
                    ApwModel::Apw12_1215b => "APW121215b",
                    ApwModel::Apw12_1215c => "APW121215c",
                    _ => unreachable!(),
                },
                compatible_miners: "S19, S19 Pro, S19j, S19j Pro, T19",
            },
            ApwModel::Apw12_1215d
            | ApwModel::Apw12_1215e
            | ApwModel::Apw12_1215f
            | ApwModel::Apw12_1215g => ApwSpec {
                voltage_min_v: 12.0,
                voltage_max_v: 15.0,
                max_current_a: Some(233),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 94,
                has_voltage_feedback: true,
                label: match self {
                    ApwModel::Apw12_1215d => "APW121215d",
                    ApwModel::Apw12_1215e => "APW121215e",
                    ApwModel::Apw12_1215f => "APW121215f",
                    ApwModel::Apw12_1215g => "APW121215g",
                    _ => unreachable!(),
                },
                compatible_miners: "S19, S19 Pro, S19j, S19j Pro, T19",
            },
            ApwModel::Apw12_1417 => ApwSpec {
                voltage_min_v: 14.0,
                voltage_max_v: 17.0,
                max_current_a: Some(233),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 94,
                has_voltage_feedback: false,
                label: "APW12 (1417)",
                compatible_miners: "L7, K7, DR7, HS3, KA3",
            },
            ApwModel::Apw12A => ApwSpec {
                voltage_min_v: 12.0,
                voltage_max_v: 12.0,
                max_current_a: None,
                max_wattage_220v_w: None,
                max_wattage_110v_w: None,
                ac_input_min_v: 200,
                ac_input_max_v: 240,
                efficiency_pct: 93,
                has_voltage_feedback: false,
                label: "APW12A",
                compatible_miners: "Older models (no voltage adj.)",
            },
            ApwModel::Apw17_1215 => ApwSpec {
                voltage_min_v: 12.0,
                voltage_max_v: 15.0,
                max_current_a: Some(267),
                max_wattage_220v_w: Some(3600),
                max_wattage_110v_w: None,
                ac_input_min_v: 220,
                ac_input_max_v: 277,
                efficiency_pct: 94,
                has_voltage_feedback: true,
                label: "APW17 (1215)",
                compatible_miners: "S21, S21 Pro, S21 XP, S19j XP, KS5",
            },
        }
    }
}

/// Look up an APW12 1215 revision by the firmware byte returned via
/// `0x17 GET_VERSION`. Mirrors
/// (fw 0x71 → APW121215a) and
/// (fw 0x76 → APW121215f).
pub fn apw_from_fw_byte(fw: u8) -> Option<ApwModel> {
    match fw {
        0x71 => Some(ApwModel::Apw12_1215a),
        0x72 => Some(ApwModel::Apw12_1215b),
        0x73 => Some(ApwModel::Apw12_1215c),
        0x74 => Some(ApwModel::Apw12_1215d),
        0x75 => Some(ApwModel::Apw12_1215e),
        0x76 => Some(ApwModel::Apw12_1215f),
        0x77 => Some(ApwModel::Apw12_1215g),
        _ => None,
    }
}

/// Whether `replacement` can swap into a slot currently holding `original`.
///
/// Per the RE doc note (lines 66-69):
/// - APW121215d/e/f/g can replace any 1215a/b/c (one-way; firmware
///   upgrade required).
/// - APW121215a/b/c CANNOT replace any 1215d/e/f/g — the newer firmware
///   relies on the voltage-feedback ADC the older revisions don't have.
/// - Same-tier swaps (a↔b↔c, or d↔e↔f↔g) are always allowed.
/// - Cross-family swaps (e.g. APW9 → APW12) are NOT allowed.
pub fn replacement_compatible(original: ApwModel, replacement: ApwModel) -> bool {
    if original == replacement {
        return true;
    }
    let abc = [
        ApwModel::Apw12_1215a,
        ApwModel::Apw12_1215b,
        ApwModel::Apw12_1215c,
    ];
    let defg = [
        ApwModel::Apw12_1215d,
        ApwModel::Apw12_1215e,
        ApwModel::Apw12_1215f,
        ApwModel::Apw12_1215g,
    ];
    let original_in_abc = abc.contains(&original);
    let original_in_defg = defg.contains(&original);
    let replacement_in_abc = abc.contains(&replacement);
    let replacement_in_defg = defg.contains(&replacement);
    if original_in_abc && replacement_in_abc {
        return true;
    }
    if original_in_defg && replacement_in_defg {
        return true;
    }
    if original_in_abc && replacement_in_defg {
        // d/e/f/g can replace a/b/c (one-way).
        return true;
    }
    // Anything else (different family, or a/b/c into d/e/f/g slot) → no.
    false
}

/// Every model in the catalog. Useful for fleet-wide rendering + tests.
pub const ALL_MODELS: &[ApwModel] = &[
    ApwModel::Apw3,
    ApwModel::Apw5,
    ApwModel::Apw7,
    ApwModel::Apw8LowV,
    ApwModel::Apw8MidV,
    ApwModel::Apw8HighV,
    ApwModel::Apw9,
    ApwModel::Apw9Plus,
    ApwModel::Apw12_1215a,
    ApwModel::Apw12_1215b,
    ApwModel::Apw12_1215c,
    ApwModel::Apw12_1215d,
    ApwModel::Apw12_1215e,
    ApwModel::Apw12_1215f,
    ApwModel::Apw12_1215g,
    ApwModel::Apw12_1417,
    ApwModel::Apw12A,
    ApwModel::Apw17_1215,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_returns_a_valid_spec() {
        for m in ALL_MODELS {
            let s = m.spec();
            assert!(s.voltage_max_v >= s.voltage_min_v);
            assert!(s.ac_input_max_v >= s.ac_input_min_v);
            assert!(!s.label.is_empty());
            assert!(!s.compatible_miners.is_empty());
        }
    }

    #[test]
    fn all_models_count_matches_re_doc() {
        // RE doc §1 (12 family rows) + §1 sub-matrix (7 1215 revisions
        // — but a/b/c/d/e/f/g overlap the family row, so total unique
        // = 18). We list 18 in ALL_MODELS.
        assert_eq!(ALL_MODELS.len(), 18);
    }

    #[test]
    fn apw121215abc_lacks_voltage_feedback() {
        for m in [
            ApwModel::Apw12_1215a,
            ApwModel::Apw12_1215b,
            ApwModel::Apw12_1215c,
        ] {
            assert!(
                !m.spec().has_voltage_feedback,
                "{:?} should NOT have voltage feedback per RE doc",
                m
            );
        }
    }

    #[test]
    fn apw121215defg_has_voltage_feedback() {
        for m in [
            ApwModel::Apw12_1215d,
            ApwModel::Apw12_1215e,
            ApwModel::Apw12_1215f,
            ApwModel::Apw12_1215g,
        ] {
            assert!(
                m.spec().has_voltage_feedback,
                "{:?} should have voltage feedback per RE doc",
                m
            );
        }
    }

    #[test]
    fn apw3_dual_voltage_max_wattage_pinned() {
        let s = ApwModel::Apw3.spec();
        assert_eq!(s.max_wattage_220v_w, Some(1600));
        assert_eq!(s.max_wattage_110v_w, Some(1200));
    }

    #[test]
    fn apw17_supports_s21_fleet() {
        let s = ApwModel::Apw17_1215.spec();
        assert!(s.compatible_miners.contains("S21"));
        assert_eq!(s.max_current_a, Some(267));
        assert!(s.has_voltage_feedback);
    }

    #[test]
    fn fw_byte_lookup_known_firmwares() {
        assert_eq!(apw_from_fw_byte(0x71), Some(ApwModel::Apw12_1215a));
        assert_eq!(apw_from_fw_byte(0x76), Some(ApwModel::Apw12_1215f));
        assert_eq!(apw_from_fw_byte(0x77), Some(ApwModel::Apw12_1215g));
        // Unknown fw byte → None.
        assert!(apw_from_fw_byte(0x00).is_none());
        assert!(apw_from_fw_byte(0xFF).is_none());
    }

    #[test]
    fn replacement_d_can_replace_a() {
        // d/e/f/g can replace a/b/c (one-way).
        assert!(replacement_compatible(
            ApwModel::Apw12_1215a,
            ApwModel::Apw12_1215d
        ));
        assert!(replacement_compatible(
            ApwModel::Apw12_1215c,
            ApwModel::Apw12_1215f
        ));
    }

    #[test]
    fn replacement_a_cannot_replace_d() {
        // a/b/c CANNOT replace d/e/f/g.
        assert!(!replacement_compatible(
            ApwModel::Apw12_1215d,
            ApwModel::Apw12_1215a
        ));
        assert!(!replacement_compatible(
            ApwModel::Apw12_1215f,
            ApwModel::Apw12_1215c
        ));
    }

    #[test]
    fn replacement_same_tier_swaps_allowed() {
        // a ↔ b within the abc tier.
        assert!(replacement_compatible(
            ApwModel::Apw12_1215a,
            ApwModel::Apw12_1215b
        ));
        assert!(replacement_compatible(
            ApwModel::Apw12_1215b,
            ApwModel::Apw12_1215a
        ));
        // d ↔ g within the defg tier.
        assert!(replacement_compatible(
            ApwModel::Apw12_1215d,
            ApwModel::Apw12_1215g
        ));
        assert!(replacement_compatible(
            ApwModel::Apw12_1215g,
            ApwModel::Apw12_1215d
        ));
    }

    #[test]
    fn replacement_cross_family_rejected() {
        // APW9 → APW12 (different generation): not allowed.
        assert!(!replacement_compatible(
            ApwModel::Apw9,
            ApwModel::Apw12_1215f
        ));
        // APW3 → APW17: not allowed.
        assert!(!replacement_compatible(
            ApwModel::Apw3,
            ApwModel::Apw17_1215
        ));
    }

    #[test]
    fn replacement_self_is_always_compatible() {
        for m in ALL_MODELS {
            assert!(replacement_compatible(*m, *m));
        }
    }

    #[test]
    fn apw_model_round_trips_through_serde() {
        for m in ALL_MODELS {
            let json = serde_json::to_string(m).unwrap();
            let back: ApwModel = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn apw_spec_serializes_to_documented_json_shape() {
        // Serialize-only (struct holds &'static str). Verify the wire
        // shape includes every documented key.
        let s = ApwModel::Apw17_1215.spec();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"label\":\"APW17 (1215)\""));
        assert!(json.contains("\"has_voltage_feedback\":true"));
        assert!(json.contains("\"max_current_a\":267"));
        assert!(json.contains("\"voltage_min_v\":12"));
    }
}
