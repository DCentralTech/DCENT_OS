//!  braiins-D — BraiinsOS+ DPS (Dynamic Performance Scaling)
//! Configuration DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §6 (Dynamic Performance Scaling, lines 739-822) — DPS Modes,
//! Algorithm State Machine, DPSConfiguration message, Temperature
//! Thresholds, On-Start Target Percentage (v1.9.0+).
//!
//!  braiins-A pinned the gRPC method catalog
//! (`BraiinsMethod::SetDPS` introduced in v1.4.0).  braiins-B
//! covered the MinerStatus state machine (NORMAL → SCALING_DOWN →
//! SHUTDOWN → RESTART). This module ships the typed CONFIG payload
//! plus the documented scale-up gate conditions and per-family
//! temperature thresholds.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// DPS Mode
// ---------------------------------------------------------------------------

/// Two DPS modes per RE doc §6 lines 741-746.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum DpsMode {
    /// `DPS_MODE_NORMAL = 1` — Standard thermal management.
    Normal = 1,
    /// `DPS_MODE_BOOST = 2` — Aggressive, tolerates higher temps.
    Boost = 2,
}

impl DpsMode {
    /// Proto3 numeric tag value.
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }

    /// Look up by numeric tag.
    pub fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Normal,
            2 => Self::Boost,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// DPS Configuration
// ---------------------------------------------------------------------------

/// `DPSConfiguration` message per RE doc §6 lines 784-796.
///
/// Wire types match the BraiinsOS+ protobuf §9.2.28: Power=u64 watts,
/// TeraHashrate=f64 TH/s, Hours=u32. `optional` proto fields map to
/// `Option<T>`.
///
/// # D2 — proto hashrate-field UNIT guardrail (RE-002, 2026-05-20)
///
/// bosminer's DPS proto reports hashrate fields in **GHS** (gigahash;
/// the wire fields carry a `*_ghs` suffix in the .proto). This DTO carries
/// hashrate fields in **THS** (terahash) — see [`Self::hashrate_step_ths`]
/// and [`Self::min_hashrate_target_ths`]. There is currently **NO** code
/// path in DCENT_OS that deserializes the bosminer DPS proto into this
/// struct (we author the config locally / via our own REST `/api/dps`), so
/// there is NO live conversion bug today. This is a **future guardrail**:
///
/// **If a future gRPC `SetDPS` / proto-ingest seam is ever added, it MUST
/// convert the proto GH/s value to TH/s before populating the `*_ths` fields
/// here — use the type-safe `dcentrald_common::units::ghs_to_ths` helper (or
/// the `Ghs`/`Ths` newtypes), NOT a bare `/ 1000.0`.** Mixing GH/s into a
/// `*_ths` field would inflate the hashrate floor/step by 1000× and silently
/// break DPS scaling; the `Ghs`/`Ths` newtypes make that mistake a compile
/// error. The unit is encoded in the field name (`_ths`) precisely so this
/// conversion is not forgotten. See `CORPUS_RESOLUTIONS.md` §RE-002 D2 and the
/// `dcentrald-common::units` module docs (gap-swarm G62).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpsConfiguration {
    /// Enable / disable DPS.
    pub enabled: Option<bool>,
    /// Watts to reduce per scale-down step.
    pub power_step_watts: u64,
    /// TH/s to reduce per scale-down step (hashrate-target mode).
    ///
    /// UNIT: terahash/s. The bosminer proto reports this in GHS — any future
    /// proto-ingest seam MUST convert via `dcentrald_common::units::ghs_to_ths`
    /// (NOT a bare `/ 1000.0`; see the D2 guardrail on the struct doc-comment).
    pub hashrate_step_ths: f64,
    /// Floor before shutdown (power-target mode).
    pub min_power_target_watts: u64,
    /// Floor before shutdown (hashrate-target mode).
    ///
    /// UNIT: terahash/s. The bosminer proto reports this in GHS — any future
    /// proto-ingest seam MUST convert via `dcentrald_common::units::ghs_to_ths`
    /// (NOT a bare `/ 1000.0`; see the D2 guardrail on the struct doc-comment).
    pub min_hashrate_target_ths: f64,
    /// Allow shutdown when minimum target reached.
    pub shutdown_enabled: Option<bool>,
    /// How long to stay off after shutdown.
    pub shutdown_duration_hours: u32,
    /// Normal or Boost mode.
    pub mode: Option<DpsMode>,
    /// Initial target as percentage of configured target (0-100).
    /// Introduced in v1.9.0 per RE doc §6 "On-Start Target Percentage".
    pub on_start_target_percent: Option<u32>,

    /// D3 — PSU-budget floor (RE-002, 2026-05-20). When present, the
    /// scale-down floor becomes `max(min_power_target_watts,
    /// min_psu_power_budget)` — i.e. a PSU-budget floor that is DISTINCT
    /// from (and never lower than) the target floor. bosminer carries this
    /// separately because the PSU has a minimum stable-output budget below
    /// which it should not be driven even if the tuning-target floor is
    /// lower. `None` (the serde default) preserves prior behavior exactly:
    /// the floor stays `min_power_target_watts`.
    #[serde(default)]
    pub min_psu_power_budget: Option<u32>,

    /// D3 — informational hashboard index (RE-002, 2026-05-20). bosminer's
    /// DPS can scope a step to a single hashboard. Our governor is
    /// **whole-miner** (it scales the global power target, not a per-board
    /// target), so this field is CARRIED for round-trip / future-design
    /// fidelity but is NOT acted on by `DpsGovernor`. Do not wire it into a
    /// step decision without an EE/Thermal design pass on per-board scaling.
    #[serde(default)]
    pub hashboard_idx: Option<u32>,
}

impl DpsConfiguration {
    /// True iff `min_power_target_watts > 0` AND
    /// `power_step_watts > 0` AND
    /// `power_step_watts <= min_power_target_watts` (sane scale-down
    /// granularity check).
    pub fn power_step_is_well_ordered(&self) -> bool {
        self.power_step_watts > 0
            && self.min_power_target_watts > 0
            && self.power_step_watts <= self.min_power_target_watts
    }

    /// True iff `on_start_target_percent` is present AND in [0, 100].
    pub fn on_start_percent_is_valid(&self) -> bool {
        match self.on_start_target_percent {
            Some(pct) => pct <= 100,
            None => true, // not set is also valid
        }
    }

    /// True iff DPS is enabled (helper around the Option<bool>).
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Scale-Up Gate Conditions
// ---------------------------------------------------------------------------

/// Conditions that must ALL be true before DPS scales BACK UP per RE
/// doc §6 "Scale-Up Conditions (ALL must be true)" lines 760-764.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DpsScaleUpConditions {
    /// Hashboard temperature must be at least this much below `hot_temp`.
    pub board_temp_below_hot_by_c: f32,
    /// Device must have been below `hot_temp` for at least this many minutes.
    pub sustained_below_hot_minutes: u32,
    /// Fan speed must be below this percentage.
    pub max_fan_speed_pct: u8,
    /// Tuner status must have been STABLE for at least this many minutes.
    pub tuner_stable_minutes: u32,
}

impl Default for DpsScaleUpConditions {
    fn default() -> Self {
        // RE doc §6 documented anchors:
        // 1. "at least 5C below hot_temp"
        // 2. "below hot_temp for at least 30 minutes"
        // 3. "Fan speed below 80%"
        // 4. "Tuner status is STABLE for at least 30 minutes"
        Self {
            board_temp_below_hot_by_c: 5.0,
            sustained_below_hot_minutes: 30,
            max_fan_speed_pct: 80,
            tuner_stable_minutes: 30,
        }
    }
}

impl DpsScaleUpConditions {
    /// Evaluate the 4-AND scale-up gate against a runtime sample.
    pub fn met(
        &self,
        board_temp_c: f32,
        hot_temp_c: f32,
        sustained_below_minutes: u32,
        fan_speed_pct: u8,
        tuner_stable_minutes: u32,
    ) -> bool {
        let temp_ok = board_temp_c <= hot_temp_c - self.board_temp_below_hot_by_c;
        let sustained_ok = sustained_below_minutes >= self.sustained_below_hot_minutes;
        let fan_ok = fan_speed_pct < self.max_fan_speed_pct;
        let tuner_ok = tuner_stable_minutes >= self.tuner_stable_minutes;
        temp_ok && sustained_ok && fan_ok && tuner_ok
    }
}

// ---------------------------------------------------------------------------
// Sustained-below-hot counter (DPS integration helper)
// ---------------------------------------------------------------------------

/// Stateful counter for "consecutive minutes the board/chip temperature has
/// been below the `hot` threshold". This is the input the DPS scale-up gate
/// reads as `DpsTick::sustained_below_hot_minutes` (see the DPS governor in
/// `dcentrald_autotuner::dps_governor`), and the value
/// [`DpsScaleUpConditions::sustained_below_hot_minutes`] is compared against.
///
/// It lives here in the no-HAL api-types crate (not in the daemon) so it is
/// pure and host-testable: it holds NO hardware handle, does NO I/O, and reads
/// NO clock — the caller supplies the elapsed wall-seconds and the below-hot
/// boolean each observation. On any sample at/above `hot` the accrued time
/// resets to zero (anti-flap), exactly mirroring the gate's "sustained ≥ N
/// minutes below hot" semantics, in the same spirit as
/// `dcentrald_autotuner::tuner_stability::TunerStabilityClock`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SustainedBelowHotCounter {
    /// Accrued whole seconds the temp has been continuously below `hot`.
    accrued_secs: u64,
}

impl SustainedBelowHotCounter {
    /// New counter; not yet below-hot.
    pub fn new() -> Self {
        Self { accrued_secs: 0 }
    }

    /// Advance by `elapsed_secs` of wall-time. When `below_hot` is true the
    /// elapsed time accrues; when false the counter resets to zero. Returns the
    /// accrued whole MINUTES below hot after this step.
    pub fn observe(&mut self, below_hot: bool, elapsed_secs: u64) -> u32 {
        if below_hot {
            self.accrued_secs = self.accrued_secs.saturating_add(elapsed_secs);
        } else {
            self.accrued_secs = 0;
        }
        self.minutes()
    }

    /// Accrued whole minutes below hot. Pure read (no state change).
    pub fn minutes(&self) -> u32 {
        (self.accrued_secs / 60) as u32
    }
}

// ---------------------------------------------------------------------------
// Per-family temperature threshold table
// ---------------------------------------------------------------------------

/// Documented `(target, hot, dangerous)` thermal thresholds from RE
/// doc §6 lines 800-806.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpsThermalProfile {
    /// S9 family: 89 / 100 / 110 °C.
    S9,
    /// S17 family: 72 / 85 / 92 °C (more sensitive boards).
    S17Family,
    /// S19 family: 75 / 85 / 95 °C.
    S19Family,
    /// S21 family — same as S19 family per documented practice.
    S21Family,
}

impl DpsThermalProfile {
    /// Returns `(target_c, hot_c, dangerous_c)`.
    pub fn thresholds(&self) -> (f32, f32, f32) {
        match self {
            Self::S9 => (89.0, 100.0, 110.0),
            Self::S17Family => (72.0, 85.0, 92.0),
            Self::S19Family => (75.0, 85.0, 95.0),
            Self::S21Family => (75.0, 85.0, 95.0),
        }
    }

    pub fn target_c(&self) -> f32 {
        self.thresholds().0
    }
    pub fn hot_c(&self) -> f32 {
        self.thresholds().1
    }
    pub fn dangerous_c(&self) -> f32 {
        self.thresholds().2
    }
}

/// First BraiinsOS+ minor version that exposed the SetDPS RPC.
pub const DPS_INTRODUCED_VERSION: &str = "1.4.0";

/// First BraiinsOS+ minor version that introduced
/// `on_start_target_percent`.
pub const ON_START_TARGET_PERCENT_INTRODUCED_VERSION: &str = "1.9.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dps_mode_proto_values_pinned() {
        // RE doc §6 lines 743-744: NORMAL=1, BOOST=2.
        assert_eq!(DpsMode::Normal.as_u8(), 1);
        assert_eq!(DpsMode::Boost.as_u8(), 2);
    }

    #[test]
    fn dps_mode_round_trips_through_proto_byte() {
        for m in [DpsMode::Normal, DpsMode::Boost] {
            let b = m.as_u8();
            assert_eq!(DpsMode::from_u8(b), Some(m));
        }
        assert!(DpsMode::from_u8(0).is_none());
        assert!(DpsMode::from_u8(3).is_none());
        assert!(DpsMode::from_u8(255).is_none());
    }

    #[test]
    fn dps_mode_serializes_in_screaming_snake_case() {
        // Proto3 wire form for enum names.
        assert_eq!(
            serde_json::to_string(&DpsMode::Normal).unwrap(),
            "\"NORMAL\""
        );
        assert_eq!(serde_json::to_string(&DpsMode::Boost).unwrap(), "\"BOOST\"");
    }

    #[test]
    fn configuration_round_trips_through_serde() {
        let original = DpsConfiguration {
            enabled: Some(true),
            power_step_watts: 100,
            hashrate_step_ths: 2.5,
            min_power_target_watts: 1000,
            min_hashrate_target_ths: 50.0,
            shutdown_enabled: Some(true),
            shutdown_duration_hours: 4,
            mode: Some(DpsMode::Normal),
            on_start_target_percent: Some(70),
            min_psu_power_budget: Some(800),
            hashboard_idx: Some(1),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: DpsConfiguration = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn power_step_well_ordered_predicate() {
        // Sane: 100 W step, 1000 W floor → step ≤ floor.
        let cfg = DpsConfiguration {
            enabled: Some(true),
            power_step_watts: 100,
            hashrate_step_ths: 1.0,
            min_power_target_watts: 1000,
            min_hashrate_target_ths: 10.0,
            shutdown_enabled: Some(true),
            shutdown_duration_hours: 4,
            mode: None,
            on_start_target_percent: None,
            min_psu_power_budget: None,
            hashboard_idx: None,
        };
        assert!(cfg.power_step_is_well_ordered());

        // Insane: 2000 W step, 1000 W floor → step > floor (not ordered).
        let bad = DpsConfiguration {
            power_step_watts: 2000,
            ..cfg.clone()
        };
        assert!(!bad.power_step_is_well_ordered());

        // Zero step is invalid.
        let zero = DpsConfiguration {
            power_step_watts: 0,
            ..cfg
        };
        assert!(!zero.power_step_is_well_ordered());
    }

    #[test]
    fn on_start_percent_in_zero_to_hundred() {
        let mut cfg = DpsConfiguration {
            enabled: None,
            power_step_watts: 1,
            hashrate_step_ths: 1.0,
            min_power_target_watts: 1,
            min_hashrate_target_ths: 1.0,
            shutdown_enabled: None,
            shutdown_duration_hours: 1,
            mode: None,
            on_start_target_percent: Some(70),
            min_psu_power_budget: None,
            hashboard_idx: None,
        };
        assert!(cfg.on_start_percent_is_valid());

        cfg.on_start_target_percent = Some(0);
        assert!(cfg.on_start_percent_is_valid());

        cfg.on_start_target_percent = Some(100);
        assert!(cfg.on_start_percent_is_valid());

        cfg.on_start_target_percent = Some(101);
        assert!(!cfg.on_start_percent_is_valid());

        cfg.on_start_target_percent = None;
        assert!(cfg.on_start_percent_is_valid()); // not set is valid
    }

    #[test]
    fn scale_up_default_anchors_match_re_doc() {
        // RE doc §6 lines 760-764.
        let conds = DpsScaleUpConditions::default();
        assert_eq!(conds.board_temp_below_hot_by_c, 5.0);
        assert_eq!(conds.sustained_below_hot_minutes, 30);
        assert_eq!(conds.max_fan_speed_pct, 80);
        assert_eq!(conds.tuner_stable_minutes, 30);
    }

    #[test]
    fn scale_up_gate_requires_all_four_conditions() {
        let conds = DpsScaleUpConditions::default();
        // hot_temp=85 (S19 family). Cold board, long sustain, low fans,
        // tuner stable → ALL met → scale up.
        assert!(conds.met(70.0, 85.0, 60, 60, 60));

        // Drop just temp: board hot → gate closed.
        assert!(!conds.met(83.0, 85.0, 60, 60, 60));
        // Drop just sustained: insufficient time below hot.
        assert!(!conds.met(70.0, 85.0, 5, 60, 60));
        // Drop just fan: fan still over 80%.
        assert!(!conds.met(70.0, 85.0, 60, 90, 60));
        // Drop just tuner: tuner not stable long enough.
        assert!(!conds.met(70.0, 85.0, 60, 60, 5));
    }

    #[test]
    fn scale_up_temp_margin_must_be_at_least_5c() {
        // Per RE doc: "at least 5C below hot_temp". Edge cases.
        let conds = DpsScaleUpConditions::default();
        // hot=85, board=80 → exactly 5°C below → met.
        assert!(conds.met(80.0, 85.0, 60, 60, 60));
        // hot=85, board=80.1 → only 4.9°C below → NOT met.
        assert!(!conds.met(80.1, 85.0, 60, 60, 60));
    }

    #[test]
    fn s9_thermal_thresholds_match_re_doc() {
        // RE doc §6 lines 802: S9 89/100/110 °C.
        let p = DpsThermalProfile::S9;
        assert_eq!(p.thresholds(), (89.0, 100.0, 110.0));
        assert_eq!(p.target_c(), 89.0);
        assert_eq!(p.hot_c(), 100.0);
        assert_eq!(p.dangerous_c(), 110.0);
    }

    #[test]
    fn s17_thermal_thresholds_match_re_doc() {
        // RE doc §6 lines 803: S17 72/85/92 °C.
        let p = DpsThermalProfile::S17Family;
        assert_eq!(p.thresholds(), (72.0, 85.0, 92.0));
    }

    #[test]
    fn s19_thermal_thresholds_match_re_doc() {
        // RE doc §6 lines 804: S19/S21 75/85/95 °C.
        let p = DpsThermalProfile::S19Family;
        assert_eq!(p.thresholds(), (75.0, 85.0, 95.0));
    }

    #[test]
    fn s21_thresholds_alias_s19() {
        // S21 family uses the same documented thresholds as S19 family.
        let s19 = DpsThermalProfile::S19Family;
        let s21 = DpsThermalProfile::S21Family;
        assert_eq!(s19.thresholds(), s21.thresholds());
    }

    #[test]
    fn target_below_hot_below_dangerous_for_every_profile() {
        for p in [
            DpsThermalProfile::S9,
            DpsThermalProfile::S17Family,
            DpsThermalProfile::S19Family,
            DpsThermalProfile::S21Family,
        ] {
            let (target, hot, dangerous) = p.thresholds();
            assert!(
                target < hot && hot < dangerous,
                "{:?} thresholds not strictly increasing: {} {} {}",
                p,
                target,
                hot,
                dangerous
            );
        }
    }

    #[test]
    fn version_constants_pinned() {
        // RE doc §6: SetDPS introduced v1.4.0; on_start_target_percent v1.9.0.
        assert_eq!(DPS_INTRODUCED_VERSION, "1.4.0");
        assert_eq!(ON_START_TARGET_PERCENT_INTRODUCED_VERSION, "1.9.0");
    }

    #[test]
    fn is_enabled_predicate_handles_unset_optional() {
        // optional bool with None → false (DPS off until explicitly on).
        let cfg = DpsConfiguration {
            enabled: None,
            power_step_watts: 100,
            hashrate_step_ths: 1.0,
            min_power_target_watts: 1000,
            min_hashrate_target_ths: 10.0,
            shutdown_enabled: None,
            shutdown_duration_hours: 4,
            mode: None,
            on_start_target_percent: None,
            min_psu_power_budget: None,
            hashboard_idx: None,
        };
        assert!(!cfg.is_enabled());

        let on = DpsConfiguration {
            enabled: Some(true),
            ..cfg.clone()
        };
        assert!(on.is_enabled());

        let off = DpsConfiguration {
            enabled: Some(false),
            ..cfg
        };
        assert!(!off.is_enabled());
    }

    #[test]
    fn thermal_profile_round_trips_through_serde() {
        for p in [
            DpsThermalProfile::S9,
            DpsThermalProfile::S17Family,
            DpsThermalProfile::S19Family,
            DpsThermalProfile::S21Family,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: DpsThermalProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn thermal_profile_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&DpsThermalProfile::S9).unwrap(),
            "\"s9\""
        );
        assert_eq!(
            serde_json::to_string(&DpsThermalProfile::S17Family).unwrap(),
            "\"s17_family\""
        );
        assert_eq!(
            serde_json::to_string(&DpsThermalProfile::S19Family).unwrap(),
            "\"s19_family\""
        );
    }

    #[test]
    fn power_step_uses_u64_not_u32() {
        // RE doc §9.2.28: Power.watt is u64. Pin via field type — a
        // refactor that downgraded to u32 would silently cap at 4 GW
        // (probably fine in practice, but the wire type is u64).
        let cfg = DpsConfiguration {
            enabled: Some(true),
            power_step_watts: u64::MAX,
            hashrate_step_ths: 0.0,
            min_power_target_watts: u64::MAX,
            min_hashrate_target_ths: 0.0,
            shutdown_enabled: None,
            shutdown_duration_hours: 0,
            mode: None,
            on_start_target_percent: None,
            min_psu_power_budget: None,
            hashboard_idx: None,
        };
        let json = serde_json::to_value(&cfg).unwrap();
        // Serialized as a number — sanity check.
        assert!(json["power_step_watts"].is_u64() || json["power_step_watts"].is_number());
    }

    // -----------------------------------------------------------------
    // D3 — min_psu_power_budget + hashboard_idx (RE-002, 2026-05-20)
    // -----------------------------------------------------------------

    #[test]
    fn d3_fields_default_to_none_and_are_additive() {
        // A config JSON authored BEFORE the D3 fields existed (no
        // min_psu_power_budget / hashboard_idx keys) must still deserialize
        // — the #[serde(default)] makes them None, preserving prior shape.
        let legacy_json = r#"{
            "enabled": true,
            "power_step_watts": 300,
            "hashrate_step_ths": 11.0,
            "min_power_target_watts": 943,
            "min_hashrate_target_ths": 70.7417,
            "shutdown_enabled": false,
            "shutdown_duration_hours": 3,
            "mode": null,
            "on_start_target_percent": 100
        }"#;
        let cfg: DpsConfiguration = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(cfg.min_psu_power_budget, None);
        assert_eq!(cfg.hashboard_idx, None);
    }

    #[test]
    fn d3_fields_round_trip_when_present() {
        let original = DpsConfiguration {
            enabled: Some(true),
            power_step_watts: 300,
            hashrate_step_ths: 11.0,
            min_power_target_watts: 943,
            min_hashrate_target_ths: 70.7417,
            shutdown_enabled: Some(false),
            shutdown_duration_hours: 3,
            mode: None,
            on_start_target_percent: Some(100),
            min_psu_power_budget: Some(1100),
            hashboard_idx: Some(2),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: DpsConfiguration = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
        assert_eq!(back.min_psu_power_budget, Some(1100));
        assert_eq!(back.hashboard_idx, Some(2));
    }

    // -----------------------------------------------------------------
    // SustainedBelowHotCounter (DPS scale-up gate input)
    // -----------------------------------------------------------------

    #[test]
    fn sustained_below_hot_accrues_then_resets() {
        let mut c = SustainedBelowHotCounter::new();
        assert_eq!(c.minutes(), 0);
        // Below hot for 30 * 60 s → 30 minutes (satisfies the default gate).
        assert_eq!(c.observe(true, 30 * 60), 30);
        // One more minute below hot → 31.
        assert_eq!(c.observe(true, 60), 31);
        // A sample AT/ABOVE hot resets the accrued time to zero (anti-flap).
        assert_eq!(c.observe(false, 60), 0);
        assert_eq!(c.minutes(), 0);
        // Re-accrues from zero, not from the old high-water mark.
        assert_eq!(c.observe(true, 5 * 60), 5);
    }

    #[test]
    fn sustained_below_hot_partial_minute_floors() {
        let mut c = SustainedBelowHotCounter::new();
        // 59 s below hot → 0 whole minutes (gate not yet satisfiable).
        assert_eq!(c.observe(true, 59), 0);
        // +1 s → exactly the 1-minute boundary crossed.
        assert_eq!(c.observe(true, 1), 1);
    }

    #[test]
    fn sustained_below_hot_feeds_scale_up_gate_threshold() {
        // Wire the counter to the documented default gate: 30 min sustained.
        let conds = DpsScaleUpConditions::default();
        let mut c = SustainedBelowHotCounter::new();
        // 29 min below hot — gate's sustained condition NOT yet met.
        let m29 = c.observe(true, 29 * 60);
        assert!(!conds.met(70.0, 85.0, m29, 60, 60));
        // 30 min — sustained condition satisfied (all four now true).
        let m30 = c.observe(true, 60);
        assert!(conds.met(70.0, 85.0, m30, 60, 60));
    }
}
