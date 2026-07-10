//! Auto-tuner configuration from TOML [autotuner] section.

use serde::{Deserialize, Serialize};

/// Absolute maximum BOARD-level power draw in watts (before PSU efficiency loss).
///
/// This is the hard safety cap for residential 120V/15A circuits. No configuration
/// value may exceed this. Note this is BOARD power (DC side), not wall power.
/// Actual wall draw = board_watts / psu_efficiency (e.g., 1800W / 0.88 = 2045W wall,
/// which exceeds a 15A circuit — the PSU will current-limit before that).
pub const ABSOLUTE_MAX_WATTS: u32 = 1800;

///  am2/BM1362 frequency-only autotuner: lower bound of the
/// frequency search band (MHz). Maps to BM1362 PLL Step -12.
///
/// The daemon gate clamps `min_freq_mhz` UP to this floor for the
/// am2/BM1362 family so a stray operator TOML can't drive the search
/// below the chain's enumeration-stable floor.
pub const AM2_BM1362_FREQ_BAND_MIN_MHZ: u16 = 245;

///  am2/BM1362 frequency-only autotuner: upper bound of the
/// frequency search band (MHz). BM1362 nameplate / PLL Step 0.
///
/// The daemon gate clamps `max_freq_mhz` DOWN to this ceiling for the
/// am2/BM1362 family. On a home unit we deliberately do NOT explore
/// above nameplate 545 MHz — overclocking a home space-heater unit is
/// out of scope for this wave and would be a thermal/brick risk.
///
/// This is the DEFAULT / standard-SKU ceiling. PERF-004: mid-band SKUs
/// (e.g. BHB42611) have a designed operating FLOOR above 545 MHz, so a
/// hard 545 ceiling means the SKU literally cannot run in its designed
/// band. [`am2_bm1362_max_freq_for_sku`] makes the ceiling SKU-conditional
/// (545 std / 597 mid-band) — but the daemon only widens the ceiling for a
/// SKU when the operator opts in (see [`Bm1362SkuClass`]); already-working
/// SKUs keep this exact 545 compiled default.
pub const AM2_BM1362_FREQ_BAND_MAX_MHZ: u16 = 545;

/// PERF-004: mid-band BM1362 SKU (BHB42611-class) ceiling (MHz).
///
/// Mid-band hashboards (e.g. the Antminer S19j Pro **A**-grade BHB42611)
/// have a designed operating band that *starts* above the standard 545 MHz
/// nameplate, so clamping them to 545 leaves them below their own floor.
/// 597 MHz is the top of the verified BM1362 PLL table
/// (`dcentrald-asic::drivers::bm1362::BM1362_PLL_TABLE`, fbdiv=239); we do
/// NOT go higher because the chip cannot lock above the table window without
/// overclock packs, and a home space-heater unit must stay inside a
/// PLL-lockable, thermally-validated band.
pub const AM2_BM1362_FREQ_BAND_MAX_MID_BAND_MHZ: u16 = 597;

/// PERF-004: high-bin BM1362 SKU ceiling (MHz).
///
/// High-bin SKUs (BHB42801-class) are rated up toward ~675 MHz on vendor
/// firmware with an APW12+ rail, but the verified BM1362 PLL table tops out
/// at 597 MHz — DCENT_OS cannot currently program a lockable 675 MHz point,
/// and pushing a home unit there is out of scope. We therefore CLAMP the
/// high-bin request to the PLL-table maximum (597) rather than advertising a
/// ceiling the silicon can't reach in our table. This is intentionally
/// conservative: capability is recorded, but the effective ceiling stays at
/// the highest frequency we can actually lock. Raising it requires extending
/// `BM1362_PLL_TABLE` and a separate safety review.
pub const AM2_BM1362_FREQ_BAND_MAX_HIGH_BIN_MHZ: u16 = 597;

/// PERF-004: BM1362 hashboard SKU class for the autotuner frequency ceiling.
///
/// `Standard` is the load-bearing default — it maps to the historical 545 MHz
/// ceiling so already-working SKUs (`a lab unit` / `a lab unit` BHB42601) see byte-identical
/// behavior. The daemon only selects a wider class when the operator opts in
/// (and the live-detected hashboard SKU corroborates it). Never auto-promote a
/// home unit to a wider class without an explicit opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bm1362SkuClass {
    /// BHB42601-class (the live-proven `a lab unit`/`a lab unit` baseline). 545 MHz ceiling.
    Standard,
    /// BHB42611-class mid-band. Designed floor is above 545 MHz → 597 ceiling.
    MidBand,
    /// BHB42801-class high-bin. Capability recorded; effective ceiling stays at
    /// the PLL-table maximum (597) until the table is extended.
    HighBin,
}

impl Default for Bm1362SkuClass {
    fn default() -> Self {
        // Load-bearing: the default class is the proven 545-MHz baseline.
        Self::Standard
    }
}

impl Bm1362SkuClass {
    /// Resolve the upper frequency ceiling (MHz) for this SKU class.
    ///
    /// PERF-004. `Standard` returns the historical 545; mid-band/high-bin
    /// return the widened (but still PLL-lockable) ceiling.
    pub fn max_freq_mhz(self) -> u16 {
        am2_bm1362_max_freq_for_sku(self)
    }

    /// Parse an operator/RE SKU label (e.g. EEPROM-classified `"BHB42611"`)
    /// into a class. Unknown / standard SKUs map to the safe `Standard`
    /// default. Case-insensitive; tolerates surrounding whitespace.
    ///
    /// Only the explicitly-known wider-band SKUs widen the ceiling; anything
    /// else stays at the 545 baseline (fail-safe for home units).
    pub fn from_sku_label(label: &str) -> Self {
        match label.trim().to_ascii_uppercase().as_str() {
            // Mid-band A-grade boards designed above the 545 nameplate.
            "BHB42611" | "BHB42603" => Self::MidBand,
            // High-bin boards (vendor-rated toward 675 on APW12+).
            "BHB42801" | "BHB42803" => Self::HighBin,
            // BHB42601 and everything else → proven 545 baseline.
            _ => Self::Standard,
        }
    }
}

/// PERF-004: SKU-conditional am2/BM1362 frequency ceiling (MHz).
///
/// Free function form (so the daemon can call it without constructing the
/// enum twice). Returns the ceiling each SKU class is allowed to explore up to.
pub fn am2_bm1362_max_freq_for_sku(sku: Bm1362SkuClass) -> u16 {
    match sku {
        Bm1362SkuClass::Standard => AM2_BM1362_FREQ_BAND_MAX_MHZ,
        Bm1362SkuClass::MidBand => AM2_BM1362_FREQ_BAND_MAX_MID_BAND_MHZ,
        Bm1362SkuClass::HighBin => AM2_BM1362_FREQ_BAND_MAX_HIGH_BIN_MHZ,
    }
}

/// Env override that opts the am2/BM1362 family into frequency-only
/// autotuning without editing `dcentrald.toml`. Set to `1` / `true` /
/// `yes` / `on` to enable. Mirrors the `[autotuner]
/// am2_frequency_autotune` TOML key; either source enables it, neither
/// (default) keeps the am2/BM1362 autotuner gate fully closed.
pub const AM2_FREQUENCY_AUTOTUNE_ENV: &str = "DCENT_AM2_FREQUENCY_AUTOTUNE";

/// AT-3: env override that opts the am2/BM1362 dsPIC chain into the gated,
/// default-OFF, READ-ONLY quiet-window 0x3A `MEASURE_VOLTAGE` read during
/// mining. Set to `1`/`true`/`yes`/`on` to enable; mirrors the
/// `[autotuner] at3_rail_read` TOML key (either source enables it). **Default
/// OFF is load-bearing** — with the gate closed the am2 serial-dispatch loop is
/// byte-identical to the proven `a lab unit`/`a lab unit` milestone path (no extra dsPIC
/// transaction, no telemetry change). AT-3 additionally requires the autotuner
/// to be opted in on this path (`am2_frequency_autotune`), and is measure-only:
/// it never writes voltage/frequency (the closed loop is AT-4+).
pub const AT3_RAIL_READ_ENV: &str = "DCENT_AM2_AT3_RAIL_READ";

/// AT-3 quiet-window 0x3A read cadence floor (seconds). The dsPIC samples its
/// own ADC on a ~1 Hz background sweep, so there is no value reading faster;
/// the floor keeps the bus footprint minimal (DESIGN 1 §1.4/§1.5).
pub const AT3_RAIL_READ_INTERVAL_FLOOR_S: u64 = 15;
/// AT-3 quiet-window 0x3A read cadence default (seconds) — aligned with the
/// autotuner `measurement_window_s` default intent (DESIGN 1 §1.5).
pub const AT3_RAIL_READ_INTERVAL_DEFAULT_S: u64 = 30;
/// AT-3 quiet-window 0x3A read cadence ceiling (seconds).
pub const AT3_RAIL_READ_INTERVAL_CEIL_S: u64 = 120;

/// PERF-006: env override that opts the am2/BM1362 dsPIC chain-voltage path
/// into voltage optimization. **Default OFF is load-bearing** — the historical
/// behavior hard-gates voltage optimization / DVFS to S9/BM1387+PIC16 only, and
/// the live-proven `a lab unit`/`a lab unit` am2 BM1362 home units never write a tuned
/// voltage. Setting this to `1`/`true`/`yes`/`on` advertises the
/// `voltage_optimization_supported` capability for the `(0x1362,"dspic")`
/// profile so the operator can opt into a voltage search; the search is then
/// clamped to [`AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV`,
/// `AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV`]. Until live-A/B-validated this stays a
/// capability behind the gate, never a compiled default.
pub const AM2_VOLTAGE_AUTOTUNE_ENV: &str = "DCENT_AM2_VOLTAGE_AUTOTUNE";

/// PERF-006: lower voltage clamp (mV) for the am2/BM1362 dsPIC voltage
/// autotune. The chain rail is live-proven at 13.7 V (13700 mV); we never let a
/// voltage search drop below this floor — under-volting a hashing BM1362 chain
/// risks chip drop-out / lost enumeration on a home unit.
pub const AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV: u16 = 13_700;

/// PERF-006: upper voltage clamp (mV) for the am2/BM1362 dsPIC voltage
/// autotune. 14500 mV is the dsPIC hard safety cap baked into the HAL
/// ( / the 14500 mV dsPIC cap) — a tuned
/// voltage may never exceed it. (The BM1362 rail is rated to 15.2 V max but
/// DCENT_OS clamps the *tunable* range well under that.)
pub const AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV: u16 = 14_500;

/// PERF-006: resolve whether the am2/BM1362 dsPIC voltage autotune is enabled.
///
/// Pure function (mirrors [`AutoTunerConfig::am2_frequency_autotune_enabled`]):
/// the daemon reads `std::env::var(AM2_VOLTAGE_AUTOTUNE_ENV)` and passes the
/// value in; tests pass a literal. Default (env unset) is `false`, keeping the
/// capability fully gated.
pub fn am2_voltage_autotune_enabled(env_value: Option<&str>) -> bool {
    env_value.map(env_flag_is_truthy).unwrap_or(false)
}

/// True iff `value` is an affirmative env-flag string. Shared so the
/// daemon and the unit tests agree on env-truthiness.
pub fn env_flag_is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[derive(Debug, Clone, Copy)]
pub struct AutotunerPresetDef {
    pub slug: &'static str,
    pub display_name: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutotunerCapabilityStatus {
    pub profile_key: String,
    pub family_key: String,
    pub voltage_control: String,
    pub quiet_home_presets: bool,
    pub voltage_optimization_supported: bool,
    pub dvfs_runtime_supported: bool,
    pub mixed_family_ready: bool,
    #[serde(default)]
    pub supported_preset_slugs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedAutotunerPolicy {
    pub requested_preset: Option<String>,
    pub effective_preset: Option<String>,
    pub requested_preset_supported: Option<bool>,
    pub requested_preset_reason: Option<String>,
    pub degraded_from_requested: bool,
    pub capabilities: AutotunerCapabilityStatus,
    pub effective_config: AutoTunerConfig,
}

pub const AUTOTUNER_PRESETS: &[AutotunerPresetDef] = &[
    AutotunerPresetDef {
        slug: "quiet_home",
        display_name: "Quiet Home",
    },
    AutotunerPresetDef {
        slug: "balanced_home",
        display_name: "Balanced Home",
    },
    AutotunerPresetDef {
        slug: "efficiency_max",
        display_name: "Efficiency Max",
    },
    AutotunerPresetDef {
        slug: "hashrate_max",
        display_name: "Hashrate Max",
    },
    AutotunerPresetDef {
        slug: "watt_cap",
        display_name: "Watt Cap",
    },
    AutotunerPresetDef {
        slug: "advanced_manual",
        display_name: "Advanced Manual",
    },
];

pub fn autotuner_preset(slug: &str) -> Option<&'static AutotunerPresetDef> {
    AUTOTUNER_PRESETS.iter().find(|preset| preset.slug == slug)
}

pub fn autotuner_preset_display_name(slug: &str) -> Option<&'static str> {
    autotuner_preset(slug).map(|preset| preset.display_name)
}

pub fn is_supported_autotuner_preset(slug: &str) -> bool {
    autotuner_preset(slug).is_some()
}

fn family_key_for_chip(chip_id: u16) -> &'static str {
    match chip_id {
        0x1387 => "bm1387",
        0x1397 => "bm1397",
        0x1398 => "bm1398",
        0x1362 => "bm1362",
        0x1366 => "bm1366",
        0x1368 => "bm1368",
        0x1370 => "bm1370",
        _ => "unknown",
    }
}

fn normalize_voltage_control_kind(voltage_control: &str) -> &'static str {
    match voltage_control.trim().to_ascii_lowercase().as_str() {
        "pic16" => "pic16",
        "dspic" => "dspic",
        "nopic" => "nopic",
        _ => "unknown",
    }
}

fn preset_slugs(presets: &[&str]) -> Vec<String> {
    presets.iter().map(|slug| (*slug).to_string()).collect()
}

pub fn autotuner_capabilities_for_chip(
    chip_id: u16,
    voltage_control: &str,
) -> AutotunerCapabilityStatus {
    let family_key = family_key_for_chip(chip_id);
    let voltage_control = normalize_voltage_control_kind(voltage_control);

    let (profile_key, quiet_home_presets, voltage_optimization_supported, supported_preset_slugs) =
        match (chip_id, voltage_control) {
            (0x1387, "pic16") => (
                "bm1387-home-pic16",
                true,
                true,
                preset_slugs(&[
                    "quiet_home",
                    "balanced_home",
                    "efficiency_max",
                    "hashrate_max",
                    "watt_cap",
                    "advanced_manual",
                ]),
            ),
            (0x1397, "pic16") => (
                "bm1397-x17-pic16",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1397, "dspic") => (
                "bm1397-x17-dspic",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1398, "pic16") | (0x1398, "dspic") => (
                "bm1398-voltage-controlled",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1362, "pic16") | (0x1362, "dspic") => (
                "bm1362-voltage-controlled",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1366, "pic16") | (0x1366, "dspic") => (
                "bm1366-voltage-controlled",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1366, "nopic") => (
                "bm1366-nopic",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1368, "nopic") => (
                "bm1368-nopic",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            (0x1370, "nopic") => (
                "bm1370-nopic",
                false,
                false,
                preset_slugs(&["hashrate_max", "watt_cap", "advanced_manual"]),
            ),
            _ => ("unknown-or-planned", false, false, Vec::new()),
        };

    AutotunerCapabilityStatus {
        profile_key: profile_key.to_string(),
        family_key: family_key.to_string(),
        voltage_control: voltage_control.to_string(),
        quiet_home_presets,
        voltage_optimization_supported,
        dvfs_runtime_supported: false,
        mixed_family_ready: false,
        supported_preset_slugs,
    }
}

/// PERF-006 + PERF-011: env-gated capability overlay for the dsPIC
/// voltage-controlled families (BM1362 / BM1397 / BM1398).
///
/// Returns the same capability status as [`autotuner_capabilities_for_chip`],
/// EXCEPT: when one of the dsPIC voltage-controlled profiles is requested AND
/// the `DCENT_AM2_VOLTAGE_AUTOTUNE` gate (`env_value`) is truthy, this:
///   - flips `voltage_optimization_supported` to `true` (the operator opts into
///     a clamped dsPIC voltage search), and
///   - advertises the `quiet_home` + `efficiency_max` presets (PERF-011) so the
///     resolver routes the operator's request to them instead of degrading to
///     watt_cap.
///
/// With the gate unset (default), the result is byte-identical to the pure
/// function — the live-proven `a lab unit`/`a lab unit` and S17/S19 Pro behavior is
/// unchanged.
///
/// The daemon reads `std::env::var(AM2_VOLTAGE_AUTOTUNE_ENV)` and passes it in;
/// tests pass a literal. The voltage *range* a BM1362 dsPIC search may explore
/// is still clamped downstream to [`AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV`,
/// `AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV`] (the daemon applies the clamp to the
/// resolved config; capability advertisement here does not itself write
/// voltage). PERF-011 leaves the per-family minimum-voltage floors to the
/// existing `minimum_voltage_for_capabilities` table.
pub fn autotuner_capabilities_for_chip_with_voltage_autotune(
    chip_id: u16,
    voltage_control: &str,
    env_value: Option<&str>,
) -> AutotunerCapabilityStatus {
    let mut caps = autotuner_capabilities_for_chip(chip_id, voltage_control);
    let normalized = normalize_voltage_control_kind(voltage_control);
    let is_dspic_voltage_family =
        normalized == "dspic" && matches!(chip_id, 0x1362 | 0x1397 | 0x1398);
    if is_dspic_voltage_family && am2_voltage_autotune_enabled(env_value) {
        caps.voltage_optimization_supported = true;
        caps.quiet_home_presets = true;
        // Surface the (now-supported) home/efficiency presets so the resolver
        // can route the operator's request to them instead of degrading to
        // watt_cap. Append-only; preserves existing slugs/ordering.
        for slug in ["quiet_home", "balanced_home", "efficiency_max"] {
            if !caps.supported_preset_slugs.iter().any(|s| s == slug) {
                caps.supported_preset_slugs.push(slug.to_string());
            }
        }
    }
    caps
}

/// PERF-006: clamp a tunable dsPIC chain voltage (mV) into the am2/BM1362
/// voltage-autotune safe window [13700, 14500]. Used by the daemon when the
/// voltage-autotune gate is on so a search can never drive the chain rail
/// below the proven floor or above the dsPIC hard cap.
pub fn clamp_am2_dspic_autotune_voltage_mv(voltage_mv: u16) -> u16 {
    voltage_mv.clamp(
        AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV,
        AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV,
    )
}

pub fn autotuner_capabilities_for_mixed_families() -> AutotunerCapabilityStatus {
    AutotunerCapabilityStatus {
        profile_key: "mixed-family-conservative".to_string(),
        family_key: "mixed".to_string(),
        voltage_control: "mixed".to_string(),
        quiet_home_presets: false,
        voltage_optimization_supported: false,
        dvfs_runtime_supported: false,
        mixed_family_ready: false,
        supported_preset_slugs: vec!["advanced_manual".to_string()],
    }
}

pub fn is_supported_autotuner_preset_for_capabilities(
    slug: &str,
    capabilities: &AutotunerCapabilityStatus,
) -> bool {
    capabilities
        .supported_preset_slugs
        .iter()
        .any(|item| item == slug)
}

/// Tuning target mode.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuneTarget {
    /// Maximize total hashrate.
    Hashrate,
    /// Target a specific power budget.
    Power,
    /// Maximize hashrate per watt (J/TH). **Default for home miners** —
    /// Heater + Mining (Standard) modes default to this so the autotuner
    /// optimizes the J/TH bill, not the leaderboard. Hacker mode opts back
    /// into `Hashrate` via [`TuneTarget::for_mode`].
    Efficiency,
    /// Target a specific hashrate at minimum power.
    /// User says "give me X TH/s at minimum power."
    #[serde(rename = "hashrate_target")]
    HashrateTarget,
    /// W9.4 — minimize J/TH against an operator-confirmed wattmeter reading.
    ///
    /// Like [`TuneTarget::Efficiency`] but treats `PowerCalibration`
    /// `operator_confirmed=true` as the source of truth for J/TH instead of
    /// the modeled C_eff baseline. Falls back to the modeled estimate when
    /// no wattmeter calibration is on file. The autotuner uses the real
    /// measured baseline as the cost function so tuning steps that drop the
    /// model's J/TH but raise the wall meter's J/TH are correctly rejected.
    #[serde(rename = "efficiency_jth")]
    EfficiencyJTH,
}

/// Default to **Efficiency** so any code path that constructs a
/// `TuneTarget::default()` without an explicit `OperatingMode` lands in the
/// safer J/TH-minimizing branch instead of TH/s-greedy. Home miners pay for
/// electricity per kWh; the autotuner default must reflect that.
///
/// Per-mode dispatch (Heater/Mining/Hacker) goes through
/// [`TuneTarget::for_mode`] — which is what the daemon uses at construction
/// time. This `Default` impl is purely the fallback for callers that have no
/// `OperatingMode` context.
impl Default for TuneTarget {
    fn default() -> Self {
        Self::Efficiency
    }
}

impl TuneTarget {
    /// Mode-aware default selector.
    ///
    /// - `OperatingMode::Home`     → `Efficiency` (J/TH wins over TH/s).
    /// - `OperatingMode::Standard` → `Efficiency` (mining mode is still a
    ///   home miner paying for electricity; competitor firmwares
    ///   default to hashrate, we don't).
    /// - `OperatingMode::Hacker`   → `Hashrate` (raw register access users
    ///   asked for the leaderboard, give them the leaderboard).
    ///
    /// `mode_name` is the lowercase serde tag from
    /// `dcentrald_api_types::OperatingMode` (`"home"` / `"standard"` /
    /// `"hacker"`). Unknown / mistyped values fall through to
    /// `TuneTarget::default()` (Efficiency) — fail-safe for home miners,
    /// matching the donation-default-2% operator-locked posture.
    ///
    /// Kept as `&str` instead of taking a typed `OperatingMode` argument so
    /// `dcentrald-autotuner` does not pull in the whole api-types
    /// dependency graph just for an enum dispatch. The mode strings are
    /// already canonical across the daemon, REST, and dashboard surfaces.
    pub fn for_mode(mode_name: &str) -> Self {
        match mode_name {
            "home" | "heater" => Self::Efficiency,
            "standard" | "mining" => Self::Efficiency,
            "hacker" => Self::Hashrate,
            _ => Self::default(),
        }
    }
}

/// Operator-facing tuner mode.
///
/// `TuneTarget` remains the low-level runtime objective used by the existing
/// state machine. `TunerMode` is the external policy shape we can expose through
/// config and API without overloading integer fields such as `target_watts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum TunerMode {
    /// Maximize hashrate within safety limits.
    Performance,
    /// Walk toward a board-level power target.
    PowerTarget { watts: u32 },
    /// Walk toward a hashrate target.
    HashrateTarget { ths: f64 },
    /// Explicit fixed operating point for expert/manual operation.
    Manual { freq_mhz: u16, voltage_mv: u32 },
    /// Minimize J/TH.
    Efficiency,
    /// Target heat output for space-heater use.
    Heater { btu_h: u32 },
}

impl Default for TunerMode {
    fn default() -> Self {
        Self::Performance
    }
}

/// Auto-tuner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoTunerConfig {
    /// Whether auto-tuning is enabled.
    #[serde(default = "default_false")]
    pub enabled: bool,

    /// Requested operator-facing preset.
    ///
    /// This is the product-level policy label (e.g. `quiet_home`,
    /// `efficiency_max`, `hashrate_max`) that the UI/API should surface.
    /// The autotuner still uses the lower-level fields such as `target_mode`
    /// and power/fan constraints to implement the behavior.
    #[serde(default)]
    pub preset: Option<String>,

    /// Tuning target mode.
    #[serde(default)]
    pub target_mode: TuneTarget,

    /// Operator-facing tuner mode. When absent, the legacy `target_mode` fields
    /// remain authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuner_mode: Option<TunerMode>,

    /// Measurement window for each binary search step (seconds).
    /// Shorter = faster tuning but noisier measurements.
    /// 6s gives ~24 nonces per chip at 256 diff / 650 MHz — enough for statistical
    /// significance. 3s windows caused false "0 nonces" readings due to variance.
    #[serde(default = "default_measurement_window")]
    pub measurement_window_s: u64,

    /// Verification window after binary search completes (seconds).
    /// Longer window confirms stability before committing the profile.
    #[serde(default = "default_verification_window")]
    pub verification_window_s: u64,

    /// Hardware error rate threshold (percent).
    /// If a chip exceeds this error rate, its frequency is too high.
    #[serde(default = "default_error_threshold")]
    pub error_threshold_pct: f64,

    /// Safety margin applied to max stable frequency (percent).
    /// operating_freq = max_stable * (1 - safety_margin/100).
    #[serde(default = "default_safety_margin")]
    pub safety_margin_pct: f64,

    /// Minimum allowed frequency (MHz). Below this, chip is considered dead.
    #[serde(default = "default_min_freq")]
    pub min_freq_mhz: u16,

    /// Maximum allowed frequency (MHz). Safety clamp.
    #[serde(default = "default_max_freq")]
    pub max_freq_mhz: u16,

    /// Background monitoring interval (seconds).
    /// After tuning completes, check chip health at this interval.
    #[serde(default = "default_background_interval")]
    pub background_interval_s: u64,

    /// Directory path for profile persistence.
    #[serde(default = "default_profile_path")]
    pub profile_path: String,

    /// Maximum consecutive error windows before backing off a chip.
    #[serde(default = "default_max_consecutive_errors")]
    pub max_consecutive_errors: u32,

    /// Frequency step-down when backing off a chip (MHz).
    #[serde(default = "default_backoff_step")]
    pub backoff_step_mhz: u16,

    /// Enable temperature-compensated frequency derating.
    ///
    /// When enabled, the background monitor reduces chip frequencies if
    /// board temperature rises above the derating threshold. Frequencies
    /// are restored when temperature drops back below threshold.
    #[serde(default = "default_true")]
    pub thermal_compensation: bool,

    /// Enable aging detection and re-characterization alerts.
    ///
    /// When enabled, the background monitor tracks long-term error rate
    /// trends via EMA and logs warnings when chips show sustained degradation.
    #[serde(default = "default_true")]
    pub aging_detection: bool,

    /// Temperature derating coefficient (fraction per degree C above threshold).
    ///
    /// Default: 0.003 (0.3% frequency reduction per degree C above derating threshold).
    /// Higher values = more aggressive derating. Range: 0.001-0.01.
    #[serde(default = "default_derating_coeff")]
    pub thermal_derating_per_c: f32,

    /// Whether voltage optimization is enabled (separate from freq tuning).
    /// When enabled, after frequency characterization the autotuner will
    /// search for the minimum stable voltage, reducing power consumption.
    #[serde(default = "default_false")]
    pub voltage_optimization: bool,

    /// Minimum allowed voltage (mV). Safety floor — never go below this.
    /// S9 safe range: 8400-9400 mV.
    #[serde(default = "default_min_voltage")]
    pub min_voltage_mv: u16,

    /// Voltage safety margin above minimum stable (mV).
    /// Final operating voltage = min_stable_voltage + voltage_margin_mv.
    #[serde(default = "default_voltage_margin")]
    pub voltage_margin_mv: u16,

    /// Power budget target in watts (0 = no limit, use max hashrate).
    ///
    /// When `target_mode` is `Power`, the auto-tuner will allocate per-chip
    /// frequencies to stay within this power budget, giving grade A chips more
    /// headroom than grade D chips.
    ///
    /// Clamped to ABSOLUTE_MAX_WATTS (1800) for residential safety.
    #[serde(default)]
    pub target_watts: u32,

    /// Operator/API power step for DPS target changes.
    #[serde(default = "default_power_step_w")]
    pub power_step_w: u32,

    /// Thermal hysteresis band in degrees C.
    ///
    /// Derating activates at the derating threshold (60C), but frequencies are
    /// only restored when temperature drops below `threshold - hysteresis`.
    /// Prevents frequency oscillation when temperature hovers near the threshold.
    /// Default: 3.0. Range: [1.0, 10.0].
    #[serde(default = "default_thermal_hysteresis")]
    pub thermal_hysteresis_c: f32,

    /// Minimum hashrate ratio (actual/expected) before triggering frequency backoff.
    ///
    /// A chip with stuck cores may run at high frequency but produce fewer nonces
    /// than expected. This ratio catches such invisible failures.
    /// Default: 0.7 (70% of expected). Range: [0.3, 1.0].
    #[serde(default = "default_min_hashrate_ratio")]
    pub min_hashrate_ratio: f64,

    /// Whether profile rollback is enabled.
    ///
    /// When enabled, profiles are backed up before voltage optimization or
    /// re-characterization. If the new settings produce worse error rates,
    /// the backup is automatically restored.
    #[serde(default = "default_true")]
    pub enable_rollback: bool,

    /// Rollback error multiplier threshold.
    ///
    /// After optimization, if the error rate exceeds `pre_optimization_rate * multiplier`,
    /// the autotuner reverts to the backup profile.
    /// Default: 2.0. Range: [1.5, 10.0].
    #[serde(default = "default_rollback_error_multiplier")]
    pub rollback_error_multiplier: f64,

    /// Enable DVFS (Dynamic Voltage-Frequency Scaling) joint optimization.
    ///
    /// When enabled, the autotuner characterizes chips at multiple voltage points
    /// to find the Pareto-optimal V/F operating point per chip.
    /// This takes longer (~75s vs ~15s) but discovers true optimal efficiency.
    #[serde(default)]
    pub dvfs_enabled: bool,

    /// DVFS voltage step size in millivolts.
    ///
    /// The voltage decrement between each DVFS characterization point.
    /// Default: 100 mV. Range: [50, 200].
    #[serde(default = "default_dvfs_step")]
    pub dvfs_step_mv: u16,

    /// Number of voltage points to characterize in DVFS mode.
    ///
    /// Total DVFS time ≈ dvfs_voltage_points × 15s binary search.
    /// Default: 5. Range: [2, 8].
    #[serde(default = "default_dvfs_voltage_points")]
    pub dvfs_voltage_points: u8,

    /// Total power limit across all chains (watts). 0 = no limit.
    ///
    /// When set, after all chains are tuned the autotuner will verify total
    /// estimated power doesn't exceed this limit. If it does, least-efficient
    /// chips are backed off first.
    ///
    /// Clamped to ABSOLUTE_MAX_WATTS (1800) for residential safety.
    /// Default: 1400.
    #[serde(default = "default_total_power_limit")]
    pub total_power_limit_w: u32,

    /// Enable weak chip compensation.
    ///
    /// When a chip is backed off by the background monitor, its freed power budget
    /// is redistributed to strong neighboring chips (grade-weighted: A chips get
    /// more boost than C chips). Maintains board-level hashrate despite weak chips.
    /// Cap at each chip's `max_stable_mhz`.
    #[serde(default = "default_true")]
    pub weak_chip_compensation: bool,

    /// Target hashrate in TH/s for HashrateTarget mode.
    ///
    /// When `target_mode` is `HashrateTarget`, the auto-tuner computes the required
    /// average frequency from this target hashrate, then allocates per-chip frequencies
    /// to achieve it at minimum power.
    #[serde(default)]
    pub target_hashrate_ths: f64,

    /// Operator/API hashrate step for DPS target changes.
    #[serde(default = "default_hashrate_step_ths")]
    pub hashrate_step_ths: f64,

    /// Allow DPS to take larger target steps when the operator explicitly wants
    /// faster convergence.
    #[serde(default)]
    pub dps_high_performance_mode: bool,

    /// Enable immersion cooling mode.
    ///
    /// When enabled, all thermal thresholds are raised by `immersion_temp_offset_c`
    /// to account for immersion cooling's superior heat dissipation.
    #[serde(default)]
    pub immersion_mode: bool,

    /// Temperature offset for immersion mode (degrees C).
    ///
    /// Added to all thermal thresholds when `immersion_mode` is true.
    /// Default: 20.0. Range: [5.0, 40.0].
    #[serde(default = "default_immersion_offset")]
    pub immersion_temp_offset_c: f32,

    /// Explicit confirmation that the miner is truly immersion-cooled.
    ///
    /// Required when `immersion_mode` is true and fans are at PWM 0 with no
    /// RPM data. Without this confirmation, the immersion temperature offset
    /// is not applied (safety: prevents misconfiguration on air-cooled miners).
    #[serde(default)]
    pub immersion_confirmed: bool,

    /// Consecutive clean windows before a backed-off chip is boosted back up.
    ///
    /// If a backed-off chip runs error-free for this many consecutive windows,
    /// step it back up one PLL entry toward its profile frequency.
    /// Default: 30 (30 windows × 60s = 30 min). Range: [10, 120].
    #[serde(default = "default_boost_back_threshold")]
    pub boost_back_threshold: u32,

    /// Maximum boost-back attempts per chip.
    ///
    /// After this many failed boost attempts, the chip stays at its current
    /// frequency permanently until re-characterization.
    /// Default: 3. Range: [1, 10].
    #[serde(default = "default_max_boost_attempts")]
    pub max_boost_attempts: u32,

    /// Fan speed awareness: current fan PWM (0-127) for thermal ceiling estimation.
    ///
    /// At low fan speeds (Home mode quiet, PWM 10-20), the board's thermal
    /// ceiling is much lower than at full speed. The autotuner reduces its max frequency
    /// expectations proportionally: `thermal_ceiling = base_ceiling * fan_factor`.
    ///
    /// Fan factor: PWM 127 = 1.0, PWM 64 = 0.85, PWM 10 = 0.65.
    /// 0 = disabled (don't adjust for fan speed). Updated by thermal controller.
    #[serde(default)]
    pub current_fan_pwm: u8,

    /// Maximum power draw in watts for circuit protection (120V safety).
    ///
    /// When set, the work dispatcher will throttle frequency in real time to stay
    /// under this limit. Unlike `total_power_limit_w` (which is checked at tune time),
    /// this is enforced continuously every 5 seconds during mining.
    ///
    /// Typical values:
    /// - `Some(1350)` — 120V/15A with 25% margin (recommended for home mining)
    /// - `Some(1800)` — 120V/20A (absolute max, no margin)
    /// - `None` — no real-time power cap enforcement (default)
    ///
    /// When the estimated wall power exceeds this cap, the dispatcher reduces
    /// frequency on the highest-power chain by up to 50 MHz per 5-second cycle.
    /// Frequency is never automatically restored — the autotuner handles recovery.
    #[serde(default)]
    pub circuit_capacity_watts: Option<u32>,

    /// Enable automatic post-tune rollback.
    ///
    /// If the average error rate across all chips exceeds 2x the pre-tune rate
    /// within the first 5 background monitoring windows after tuning completes,
    /// automatically revert to the backup profile. Prevents bad tunes from
    /// running for hours before a human notices.
    #[serde(default = "default_true")]
    pub auto_rollback_post_tune: bool,

    /// Enable thermal refinement soak phase after characterization.
    ///
    /// When enabled, chips run at TABS-discovered frequencies while the board
    /// heats toward thermal equilibrium. Chips that become unstable as temperature
    /// rises are stepped down. This produces thermally-validated frequencies that
    /// are honest under sustained load.
    #[serde(default = "default_true")]
    pub thermal_refinement_enabled: bool,

    /// Maximum thermal refinement duration (seconds).
    /// Refinement will exit after this time even if equilibrium is not reached.
    /// Default: 600 (10 min). Range: [60, 1800].
    #[serde(default = "default_thermal_refinement_max")]
    pub thermal_refinement_max_s: u64,

    /// Minimum soak time before early exit is allowed (seconds).
    /// Even if temperature stabilizes quickly, refinement runs at least this long.
    /// Default: 120 (2 min). Range: [30, 600].
    #[serde(default = "default_thermal_refinement_min")]
    pub thermal_refinement_min_s: u64,

    /// Measurement window per thermal refinement round (seconds).
    /// Longer than TABS binary search windows for better noise rejection.
    /// Default: 15. Range: [5, 60].
    #[serde(default = "default_thermal_refinement_window")]
    pub thermal_refinement_window_s: u64,

    /// Temperature slope threshold for declaring thermal equilibrium (C/min).
    /// When the rate of temperature change drops below this, the board is
    /// considered thermally stable.
    /// Default: 0.2. Range: [0.05, 1.0].
    #[serde(default = "default_thermal_stability")]
    pub thermal_stability_c_per_min: f32,

    /// Shortened thermal soak duration for warm starts (seconds).
    /// When loading a saved profile, run a brief thermal check to verify
    /// stability at current ambient temperature. 0 = skip warm start check.
    /// Default: 120 (2 min). Range: [0, 600].
    #[serde(default = "default_warm_start_thermal_check")]
    pub warm_start_thermal_check_s: u64,

    /// Warning threshold for thermal degradation (percent).
    /// If thermal refinement reduces average frequency by more than this
    /// percentage, log a warning about board cooling issues.
    /// Default: 15.0. Range: [5.0, 50.0].
    #[serde(default = "default_thermal_degradation_warn")]
    pub thermal_degradation_warn_pct: f32,

    /// am2/BM1362 FREQUENCY-ONLY autotuning opt-in (default **false**).
    ///
    ///  of the am2/BM1362 autotuner enablement. Historically the
    /// daemon hard-gated voltage-optimization / DVFS to S9/BM1387+PIC16
    /// only, and the am2 BM1362 path (S19j Pro Zynq, dsPIC per-chain
    /// voltage) had ZERO live autotuning. This flag lets an operator
    /// opt the am2/BM1362 family into **frequency-only** TABS
    /// characterization. It NEVER enables a live voltage write on am2:
    /// the daemon gate force-pins `voltage_optimization=false` and
    /// `dvfs_enabled=false` for am2/BM1362 regardless of any other
    /// config, and clamps the frequency search band to the nameplate
    /// `[245, 545]` MHz window (Step -12 … Step 0; never above 545 on a
    /// home unit).
    ///
    /// **Default `false` is load-bearing.** While `false`, the am2
    /// BM1362 autotuner gate is fully closed — behavior on a live home
    /// unit (`a lab unit` / XIL) is byte-identical to today (no autotuner
    /// spawn on that family). The operator must explicitly opt in via
    /// this TOML key OR the `DCENT_AM2_FREQUENCY_AUTOTUNE=1` env
    /// override before any frequency search engages on am2/BM1362.
    ///
    /// Other families (S9/BM1387, am3-aml, am3-bb) are unaffected by
    /// this flag — their existing gate logic is preserved unchanged.
    #[serde(default = "default_false")]
    pub am2_frequency_autotune: bool,

    /// AT-3: opt the am2/BM1362 dsPIC chain into the gated, default-OFF,
    /// READ-ONLY quiet-window 0x3A `MEASURE_VOLTAGE` read during mining.
    ///
    /// **Default `false` is load-bearing.** While `false`, the AT-3
    /// `rail_timer` arm is never polled — the am2 serial-dispatch loop is
    /// byte-identical to the proven `a lab unit`/`a lab unit` milestone path (no extra
    /// dsPIC transaction, no telemetry change). AT-3 also requires
    /// [`Self::am2_frequency_autotune`] (the daemon ANDs the two), is
    /// firmware-gated to fw=0x89/0x8A (the parser-safe byte-wise framed read),
    /// and is measure-only — it never writes voltage/frequency. Either this
    /// TOML key OR `DCENT_AM2_AT3_RAIL_READ=1` enables it.
    #[serde(default = "default_false")]
    pub at3_rail_read: bool,

    /// AT-3: quiet-window 0x3A read cadence in seconds. Clamped to
    /// `[AT3_RAIL_READ_INTERVAL_FLOOR_S, AT3_RAIL_READ_INTERVAL_CEIL_S]`
    /// (15..=120) by [`Self::at3_rail_read_interval_s_clamped`]; default 30 s.
    /// There is no value reading faster than the dsPIC's own ~1 Hz ADC sweep.
    #[serde(default = "default_at3_rail_read_interval")]
    pub at3_rail_read_interval_s: u64,
}

impl Default for AutoTunerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: None,
            target_mode: TuneTarget::default(),
            tuner_mode: None,
            measurement_window_s: default_measurement_window(),
            verification_window_s: default_verification_window(),
            error_threshold_pct: default_error_threshold(),
            safety_margin_pct: default_safety_margin(),
            min_freq_mhz: default_min_freq(),
            max_freq_mhz: default_max_freq(),
            background_interval_s: default_background_interval(),
            profile_path: default_profile_path(),
            max_consecutive_errors: default_max_consecutive_errors(),
            backoff_step_mhz: default_backoff_step(),
            thermal_compensation: true,
            aging_detection: true,
            thermal_derating_per_c: default_derating_coeff(),
            voltage_optimization: false,
            min_voltage_mv: default_min_voltage(),
            voltage_margin_mv: default_voltage_margin(),
            target_watts: 0,
            power_step_w: default_power_step_w(),
            thermal_hysteresis_c: default_thermal_hysteresis(),
            min_hashrate_ratio: default_min_hashrate_ratio(),
            enable_rollback: true,
            rollback_error_multiplier: default_rollback_error_multiplier(),
            dvfs_enabled: false,
            dvfs_step_mv: default_dvfs_step(),
            dvfs_voltage_points: default_dvfs_voltage_points(),
            total_power_limit_w: default_total_power_limit(),
            weak_chip_compensation: true,
            target_hashrate_ths: 0.0,
            hashrate_step_ths: default_hashrate_step_ths(),
            dps_high_performance_mode: false,
            immersion_mode: false,
            immersion_temp_offset_c: default_immersion_offset(),
            immersion_confirmed: false,
            boost_back_threshold: default_boost_back_threshold(),
            max_boost_attempts: default_max_boost_attempts(),
            current_fan_pwm: 0,
            circuit_capacity_watts: None,
            auto_rollback_post_tune: true,
            thermal_refinement_enabled: true,
            thermal_refinement_max_s: default_thermal_refinement_max(),
            thermal_refinement_min_s: default_thermal_refinement_min(),
            thermal_refinement_window_s: default_thermal_refinement_window(),
            thermal_stability_c_per_min: default_thermal_stability(),
            warm_start_thermal_check_s: default_warm_start_thermal_check(),
            thermal_degradation_warn_pct: default_thermal_degradation_warn(),
            //  am2/BM1362 frequency-only opt-in. Default OFF —
            // zero behavior change on the proven live am2 path until
            // the operator explicitly opts in.
            am2_frequency_autotune: false,
            // AT-3 quiet-window 0x3A measured-rail read. Default OFF —
            // byte-identical to the proven am2 path until the operator
            // explicitly opts in (AND opts into the freq autotuner).
            at3_rail_read: false,
            at3_rail_read_interval_s: default_at3_rail_read_interval(),
        }
    }
}

impl AutoTunerConfig {
    /// Resolve the effective am2/BM1362 frequency-only opt-in.
    ///
    /// Returns `true` iff EITHER the `am2_frequency_autotune` TOML key
    /// is set OR the `DCENT_AM2_FREQUENCY_AUTOTUNE` env var is truthy.
    /// `env_value` is the caller-read env string (`None` = env unset)
    /// so this stays a pure function — the daemon reads
    /// `std::env::var(AM2_FREQUENCY_AUTOTUNE_ENV)` and passes it in,
    /// the unit tests pass a literal. Default (neither source set) is
    /// `false`, keeping the am2/BM1362 autotuner gate fully closed.
    pub fn am2_frequency_autotune_enabled(&self, env_value: Option<&str>) -> bool {
        if self.am2_frequency_autotune {
            return true;
        }
        env_value.map(env_flag_is_truthy).unwrap_or(false)
    }

    /// AT-3: resolve the effective quiet-window 0x3A read opt-in.
    ///
    /// Returns `true` iff EITHER the `at3_rail_read` TOML key is set OR the
    /// `DCENT_AM2_AT3_RAIL_READ` env var is truthy. Pure (the daemon reads
    /// `std::env::var(AT3_RAIL_READ_ENV)` and passes it in). Default (neither
    /// source set) is `false`, keeping the AT-3 gate fully closed. Note: the
    /// daemon ALSO requires the autotuner to be opted in
    /// ([`Self::am2_frequency_autotune_enabled`]) before AT-3 runs.
    pub fn at3_rail_read_enabled(&self, env_value: Option<&str>) -> bool {
        if self.at3_rail_read {
            return true;
        }
        env_value.map(env_flag_is_truthy).unwrap_or(false)
    }

    /// AT-3: the configured quiet-window 0x3A read cadence, clamped to the
    /// safe `[AT3_RAIL_READ_INTERVAL_FLOOR_S, AT3_RAIL_READ_INTERVAL_CEIL_S]`
    /// (15..=120 s) window so a pathological config can neither busy-poll the
    /// dsPIC nor stall the cadence indefinitely.
    pub fn at3_rail_read_interval_s_clamped(&self) -> u64 {
        self.at3_rail_read_interval_s.clamp(
            AT3_RAIL_READ_INTERVAL_FLOOR_S,
            AT3_RAIL_READ_INTERVAL_CEIL_S,
        )
    }

    ///  am2/BM1362 frequency-only safety pin.
    ///
    /// Applied by the daemon gate ONLY for the am2/BM1362 family AFTER
    /// the opt-in check passes. This is the single load-bearing
    /// transform that guarantees this wave can never write voltage on
    /// am2 and never explores outside the home-safe nameplate band:
    ///
    /// 1. `voltage_optimization = false` — HARD. No live voltage write
    ///    on am2 this wave. Voltage co-opt is a separately
    ///    safety-reviewed later wave.
    /// 2. `dvfs_enabled = false` — HARD. DVFS implies voltage points.
    /// 3. `min_freq_mhz` clamped UP to
    ///    [`AM2_BM1362_FREQ_BAND_MIN_MHZ`] (245).
    /// 4. `max_freq_mhz` clamped DOWN to
    ///    [`AM2_BM1362_FREQ_BAND_MAX_MHZ`] (545).
    ///
    /// Idempotent. Touches nothing else (slew/StepUpGate/PVT-envelope
    /// clamps live downstream in `dcentrald-autotuner::tuner` /
    /// `pvt_envelope` and are NOT weakened here).
    ///
    /// This is the load-bearing standard-SKU form — it pins the ceiling to
    /// the proven 545 MHz nameplate. For mid-band / high-bin SKUs the daemon
    /// uses [`pin_am2_bm1362_frequency_only_for_sku`] with the operator-opted-in
    /// SKU class.
    pub fn pin_am2_bm1362_frequency_only(&mut self) {
        self.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::Standard);
    }

    /// PERF-004: SKU-conditional form of [`pin_am2_bm1362_frequency_only`].
    ///
    /// Identical safety contract (voltage/DVFS hard-off, min clamp UP to 245,
    /// inverted-band recovery) but the upper ceiling is SKU-class-dependent:
    /// 545 (standard) / 597 (mid-band / high-bin). The daemon must only pass a
    /// wider class when the operator has explicitly opted in AND the live SKU
    /// classification corroborates it — never auto-widen a home unit. Passing
    /// [`Bm1362SkuClass::Standard`] is byte-identical to the historical
    /// behavior.
    ///
    /// Idempotent.
    pub fn pin_am2_bm1362_frequency_only_for_sku(&mut self, sku: Bm1362SkuClass) {
        let max_ceiling = am2_bm1362_max_freq_for_sku(sku);
        self.voltage_optimization = false;
        self.dvfs_enabled = false;
        if self.min_freq_mhz < AM2_BM1362_FREQ_BAND_MIN_MHZ {
            self.min_freq_mhz = AM2_BM1362_FREQ_BAND_MIN_MHZ;
        }
        if self.max_freq_mhz > max_ceiling {
            self.max_freq_mhz = max_ceiling;
        }
        // Defensive: if an operator inverted the band inside the
        // clamped window, collapse to the SKU's nameplate band so
        // `validate()` (min<=max) and the tuner clamp stay sane.
        if self.min_freq_mhz > self.max_freq_mhz {
            self.min_freq_mhz = AM2_BM1362_FREQ_BAND_MIN_MHZ;
            self.max_freq_mhz = max_ceiling;
        }
    }

    /// Validate configuration values.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.min_freq_mhz > self.max_freq_mhz {
            return Err(format!(
                "autotuner.min_freq_mhz ({}) must be <= max_freq_mhz ({})",
                self.min_freq_mhz, self.max_freq_mhz
            ));
        }
        if self.measurement_window_s < 1 {
            return Err("autotuner.measurement_window_s must be >= 1".to_string());
        }
        // FWAT-1: background_monitor() feeds this straight into
        // tokio::time::interval(Duration::from_secs(..)), which PANICS on a zero
        // period. Validation is the gate (the daemon makes validate() fatal), so
        // a hand-edited `background_interval_s = 0` must be rejected here, matching
        // the other defended interval fields.
        if self.background_interval_s < 1 {
            return Err("autotuner.background_interval_s must be >= 1".to_string());
        }
        // TUNE-4: range-validate `at3_rail_read_interval_s` at config-load time, the
        // same fail-loud posture the sibling interval fields above use
        // (`measurement_window_s`/`background_interval_s` both reject 0). The field
        // is consumed via `at3_rail_read_interval_s_clamped()` which saturates into
        // the safe `[AT3_RAIL_READ_INTERVAL_FLOOR_S, AT3_RAIL_READ_INTERVAL_CEIL_S]`
        // (15..=120 s) window as defense-in-depth — but a bare `0` (meaningless
        // cadence) or an absurd value above the ceiling is almost always a typo and
        // should be surfaced loudly at load rather than silently clamped, so the
        // operator sees their intent was out of range. A mildly-too-fast value
        // (1..15) is left for the clamp to floor (not a footgun); only 0 and
        // values past the documented ceiling are rejected here.
        if self.at3_rail_read_interval_s == 0 {
            return Err(format!(
                "autotuner.at3_rail_read_interval_s ({}) must be >= {} s — a 0 \
                 cadence is meaningless (the dsPIC samples its own ADC on a ~1 Hz \
                 sweep). Set it within [{}, {}] s; the default is {} s.",
                self.at3_rail_read_interval_s,
                AT3_RAIL_READ_INTERVAL_FLOOR_S,
                AT3_RAIL_READ_INTERVAL_FLOOR_S,
                AT3_RAIL_READ_INTERVAL_CEIL_S,
                AT3_RAIL_READ_INTERVAL_DEFAULT_S
            ));
        }
        if self.at3_rail_read_interval_s > AT3_RAIL_READ_INTERVAL_CEIL_S {
            return Err(format!(
                "autotuner.at3_rail_read_interval_s ({}) exceeds the {} s ceiling — \
                 values above this are almost always a typo. Set it within [{}, {}] s; \
                 the default is {} s.",
                self.at3_rail_read_interval_s,
                AT3_RAIL_READ_INTERVAL_CEIL_S,
                AT3_RAIL_READ_INTERVAL_FLOOR_S,
                AT3_RAIL_READ_INTERVAL_CEIL_S,
                AT3_RAIL_READ_INTERVAL_DEFAULT_S
            ));
        }
        if self.error_threshold_pct <= 0.0 || self.error_threshold_pct >= 100.0 {
            return Err(format!(
                "autotuner.error_threshold_pct ({}) must be in (0.0, 100.0)",
                self.error_threshold_pct
            ));
        }
        if self.safety_margin_pct <= 0.0 || self.safety_margin_pct >= 50.0 {
            return Err(format!(
                "autotuner.safety_margin_pct ({}) must be in (0.0, 50.0)",
                self.safety_margin_pct
            ));
        }
        if self.backoff_step_mhz < 1 {
            return Err("autotuner.backoff_step_mhz must be >= 1".to_string());
        }
        // max_consecutive_errors is the >= threshold a just-incremented (>=1)
        // error/deficit counter is compared against. 0 makes `1 >= 0` always
        // true, so a single transient window ratchets every chip toward
        // min_freq and boost-back can never settle upward (stuck-low). Reject 0.
        if self.max_consecutive_errors < 1 {
            return Err("autotuner.max_consecutive_errors must be >= 1".to_string());
        }
        if self.max_consecutive_errors > 50 {
            return Err(format!(
                "autotuner.max_consecutive_errors ({}) must be <= 50",
                self.max_consecutive_errors
            ));
        }
        if self.power_step_w == 0 || self.power_step_w > ABSOLUTE_MAX_WATTS {
            return Err(format!(
                "autotuner.power_step_w ({}) must be in [1, {}]",
                self.power_step_w, ABSOLUTE_MAX_WATTS
            ));
        }
        if !self.hashrate_step_ths.is_finite() || self.hashrate_step_ths <= 0.0 {
            return Err(format!(
                "autotuner.hashrate_step_ths ({}) must be a positive finite value",
                self.hashrate_step_ths
            ));
        }
        if let Some(mode) = &self.tuner_mode {
            mode.validate()?;
        }
        if self.voltage_optimization {
            // Generic config validation must not hardcode the S9/PIC16 DAC range.
            // Exact controller-specific ranges are enforced later at runtime once
            // the active platform is known (PIC16, dsPIC, or NoPic).
            if self.min_voltage_mv < 5000 {
                return Err(format!(
                    "autotuner.min_voltage_mv ({}) must be >= 5000 for supported miner families",
                    self.min_voltage_mv
                ));
            }
            if self.min_voltage_mv > 20000 {
                return Err(format!(
                    "autotuner.min_voltage_mv ({}) must be <= 20000 for supported miner families",
                    self.min_voltage_mv
                ));
            }
            if self.voltage_margin_mv < 10 {
                return Err("autotuner.voltage_margin_mv must be >= 10 mV for safety".to_string());
            }
            if self.voltage_margin_mv > 200 {
                return Err(format!(
                    "autotuner.voltage_margin_mv ({}) must be <= 200",
                    self.voltage_margin_mv
                ));
            }
        }
        if self.thermal_derating_per_c < 0.001 || self.thermal_derating_per_c > 0.01 {
            return Err(format!(
                "autotuner.thermal_derating_per_c ({}) must be in [0.001, 0.01]",
                self.thermal_derating_per_c
            ));
        }
        let preset_supplies_power_target = matches!(
            self.preset.as_deref(),
            Some("quiet_home") | Some("balanced_home") | Some("watt_cap")
        );
        if self.target_mode == TuneTarget::Power
            && self.target_watts == 0
            && !preset_supplies_power_target
        {
            return Err(
                "autotuner.target_watts must be > 0 when target_mode is 'power'".to_string(),
            );
        }
        if self.target_mode == TuneTarget::HashrateTarget && self.target_hashrate_ths <= 0.0 {
            return Err(
                "autotuner.target_hashrate_ths must be > 0 when target_mode is 'hashrate_target'"
                    .to_string(),
            );
        }
        if self.immersion_mode
            && (self.immersion_temp_offset_c < 5.0 || self.immersion_temp_offset_c > 40.0)
        {
            return Err(format!(
                "autotuner.immersion_temp_offset_c ({}) must be in [5.0, 40.0]",
                self.immersion_temp_offset_c
            ));
        }
        // Hard power caps for residential safety (120V/15A).
        if self.total_power_limit_w > 0 && self.total_power_limit_w < 200 {
            return Err(format!(
                "autotuner.total_power_limit_w ({}) is unreasonably low (min 200W when set)",
                self.total_power_limit_w
            ));
        }
        if self.total_power_limit_w > ABSOLUTE_MAX_WATTS {
            return Err(format!(
                "autotuner.total_power_limit_w ({}) exceeds absolute safety max ({}W)",
                self.total_power_limit_w, ABSOLUTE_MAX_WATTS
            ));
        }
        if self.target_watts > ABSOLUTE_MAX_WATTS {
            return Err(format!(
                "autotuner.target_watts ({}) exceeds absolute safety max ({}W)",
                self.target_watts, ABSOLUTE_MAX_WATTS
            ));
        }
        if let Some(cap) = self.circuit_capacity_watts {
            if cap > ABSOLUTE_MAX_WATTS {
                return Err(format!(
                    "autotuner.circuit_capacity_watts ({}) exceeds absolute safety max ({}W)",
                    cap, ABSOLUTE_MAX_WATTS
                ));
            }
            if cap < 100 {
                return Err(format!(
                    "autotuner.circuit_capacity_watts ({}) is unreasonably low (min 100W)",
                    cap
                ));
            }
        }
        if self.boost_back_threshold < 10 || self.boost_back_threshold > 120 {
            return Err(format!(
                "autotuner.boost_back_threshold ({}) must be in [10, 120]",
                self.boost_back_threshold
            ));
        }
        if self.max_boost_attempts < 1 || self.max_boost_attempts > 10 {
            return Err(format!(
                "autotuner.max_boost_attempts ({}) must be in [1, 10]",
                self.max_boost_attempts
            ));
        }
        if self.thermal_hysteresis_c < 1.0 || self.thermal_hysteresis_c > 10.0 {
            return Err(format!(
                "autotuner.thermal_hysteresis_c ({}) must be in [1.0, 10.0]",
                self.thermal_hysteresis_c
            ));
        }
        if self.min_hashrate_ratio < 0.3 || self.min_hashrate_ratio > 1.0 {
            return Err(format!(
                "autotuner.min_hashrate_ratio ({}) must be in [0.3, 1.0]",
                self.min_hashrate_ratio
            ));
        }
        if self.rollback_error_multiplier < 1.5 || self.rollback_error_multiplier > 10.0 {
            return Err(format!(
                "autotuner.rollback_error_multiplier ({}) must be in [1.5, 10.0]",
                self.rollback_error_multiplier
            ));
        }
        if self.thermal_refinement_enabled {
            if self.thermal_refinement_max_s < 60 || self.thermal_refinement_max_s > 1800 {
                return Err(format!(
                    "autotuner.thermal_refinement_max_s ({}) must be in [60, 1800]",
                    self.thermal_refinement_max_s
                ));
            }
            if self.thermal_refinement_min_s < 30 || self.thermal_refinement_min_s > 600 {
                return Err(format!(
                    "autotuner.thermal_refinement_min_s ({}) must be in [30, 600]",
                    self.thermal_refinement_min_s
                ));
            }
            if self.thermal_refinement_min_s > self.thermal_refinement_max_s {
                return Err(format!(
                    "autotuner.thermal_refinement_min_s ({}) must be <= thermal_refinement_max_s ({})",
                    self.thermal_refinement_min_s, self.thermal_refinement_max_s
                ));
            }
            if self.thermal_refinement_window_s < 5 || self.thermal_refinement_window_s > 60 {
                return Err(format!(
                    "autotuner.thermal_refinement_window_s ({}) must be in [5, 60]",
                    self.thermal_refinement_window_s
                ));
            }
            if self.thermal_stability_c_per_min < 0.05 || self.thermal_stability_c_per_min > 1.0 {
                return Err(format!(
                    "autotuner.thermal_stability_c_per_min ({}) must be in [0.05, 1.0]",
                    self.thermal_stability_c_per_min
                ));
            }
            if self.warm_start_thermal_check_s > 600 {
                return Err(format!(
                    "autotuner.warm_start_thermal_check_s ({}) must be in [0, 600]",
                    self.warm_start_thermal_check_s
                ));
            }
            if self.thermal_degradation_warn_pct < 5.0 || self.thermal_degradation_warn_pct > 50.0 {
                return Err(format!(
                    "autotuner.thermal_degradation_warn_pct ({}) must be in [5.0, 50.0]",
                    self.thermal_degradation_warn_pct
                ));
            }
        }
        if self.dvfs_enabled {
            if self.dvfs_step_mv < 50 || self.dvfs_step_mv > 200 {
                return Err(format!(
                    "autotuner.dvfs_step_mv ({}) must be in [50, 200]",
                    self.dvfs_step_mv
                ));
            }
            if self.dvfs_voltage_points < 2 || self.dvfs_voltage_points > 8 {
                return Err(format!(
                    "autotuner.dvfs_voltage_points ({}) must be in [2, 8]",
                    self.dvfs_voltage_points
                ));
            }
        }
        Ok(())
    }
}

impl TunerMode {
    pub fn validate(&self) -> std::result::Result<(), String> {
        match self {
            Self::Performance | Self::Efficiency => Ok(()),
            Self::PowerTarget { watts } => {
                if *watts == 0 || *watts > ABSOLUTE_MAX_WATTS {
                    Err(format!(
                        "autotuner.tuner_mode power target ({}) must be in [1, {}]",
                        watts, ABSOLUTE_MAX_WATTS
                    ))
                } else {
                    Ok(())
                }
            }
            Self::HashrateTarget { ths } => {
                if !ths.is_finite() || *ths <= 0.0 {
                    Err(format!(
                        "autotuner.tuner_mode hashrate target ({}) must be a positive finite value",
                        ths
                    ))
                } else {
                    Ok(())
                }
            }
            Self::Manual {
                freq_mhz,
                voltage_mv,
            } => {
                if *freq_mhz == 0 {
                    return Err("autotuner.tuner_mode manual freq_mhz must be > 0".to_string());
                }
                if *voltage_mv < 5_000 || *voltage_mv > 20_000 {
                    return Err(format!(
                        "autotuner.tuner_mode manual voltage_mv ({}) must be in [5000, 20000]",
                        voltage_mv
                    ));
                }
                Ok(())
            }
            Self::Heater { btu_h } => {
                if *btu_h == 0 {
                    return Err("autotuner.tuner_mode heater btu_h must be > 0".to_string());
                }
                let watts = crate::dps::watts_for_btu_h(*btu_h);
                if watts > ABSOLUTE_MAX_WATTS {
                    Err(format!(
                        "autotuner.tuner_mode heater target ({} BTU/h ~= {}W) exceeds {}W",
                        btu_h, watts, ABSOLUTE_MAX_WATTS
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn legacy_target_mode(&self) -> TuneTarget {
        match self {
            Self::Performance | Self::Manual { .. } => TuneTarget::Hashrate,
            Self::PowerTarget { .. } | Self::Heater { .. } => TuneTarget::Power,
            Self::HashrateTarget { .. } => TuneTarget::HashrateTarget,
            Self::Efficiency => TuneTarget::Efficiency,
        }
    }

    pub fn target_watts(&self) -> Option<u32> {
        match self {
            Self::PowerTarget { watts } => Some(*watts),
            Self::Heater { btu_h } => Some(crate::dps::watts_for_btu_h(*btu_h)),
            _ => None,
        }
    }

    pub fn target_hashrate_ths(&self) -> Option<f64> {
        match self {
            Self::HashrateTarget { ths } => Some(*ths),
            _ => None,
        }
    }

    pub fn from_config(config: &AutoTunerConfig) -> Self {
        if let Some(mode) = &config.tuner_mode {
            return mode.clone();
        }

        match config.target_mode {
            TuneTarget::Hashrate => Self::Performance,
            TuneTarget::Power => Self::PowerTarget {
                watts: config.target_watts,
            },
            // EfficiencyJTH maps onto the same operator-facing TunerMode::Efficiency.
            // It only changes the runtime cost-function source (operator wattmeter
            // vs modeled C_eff), not the user-presented mode label.
            TuneTarget::Efficiency | TuneTarget::EfficiencyJTH => Self::Efficiency,
            TuneTarget::HashrateTarget => Self::HashrateTarget {
                ths: config.target_hashrate_ths,
            },
        }
    }

    pub fn apply_to_config(&self, config: &mut AutoTunerConfig) {
        config.tuner_mode = Some(self.clone());
        config.target_mode = self.legacy_target_mode();
        config.target_watts = self.target_watts().unwrap_or(0);
        config.target_hashrate_ths = self.target_hashrate_ths().unwrap_or(0.0);
    }
}

fn default_watt_cap_for_capabilities(capabilities: &AutotunerCapabilityStatus) -> u32 {
    match capabilities.profile_key.as_str() {
        "bm1387-home-pic16" => 1000,
        "bm1368-nopic" | "bm1370-nopic" => 1500,
        "bm1366-nopic" => 1400,
        "mixed-family-conservative" | "unknown-or-planned" => 0,
        _ => 1400,
    }
}

fn minimum_voltage_for_capabilities(capabilities: &AutotunerCapabilityStatus) -> Option<u16> {
    match capabilities.profile_key.as_str() {
        "bm1387-home-pic16" => Some(8400),
        "bm1397-x17-pic16"
        | "bm1397-x17-dspic"
        | "bm1398-voltage-controlled"
        | "bm1362-voltage-controlled"
        | "bm1366-voltage-controlled" => Some(11940),
        _ => None,
    }
}

fn clamp_preset_power_target(config: &AutoTunerConfig, desired_watts: u32) -> u32 {
    let mut target = desired_watts.min(ABSOLUTE_MAX_WATTS);
    if let Some(circuit_cap) = config.circuit_capacity_watts {
        target = target.min(circuit_cap);
    }
    if config.total_power_limit_w > 0 {
        target = target.min(config.total_power_limit_w);
    }
    target
}

fn apply_preset_to_config(
    base: &AutoTunerConfig,
    effective_preset: &str,
    capabilities: &AutotunerCapabilityStatus,
) -> AutoTunerConfig {
    let mut config = base.clone();
    let user_voltage_opt = base.voltage_optimization;

    match effective_preset {
        "quiet_home" => {
            config.target_mode = TuneTarget::Power;
            config.target_watts = clamp_preset_power_target(&config, 500);
            config.max_freq_mhz = config.max_freq_mhz.min(600);
            config.safety_margin_pct = config.safety_margin_pct.max(8.0);
            config.measurement_window_s = config.measurement_window_s.max(8);
            config.verification_window_s = config.verification_window_s.max(20);
            config.thermal_hysteresis_c = config.thermal_hysteresis_c.max(4.0);
            config.thermal_derating_per_c = config.thermal_derating_per_c.max(0.004);
            config.thermal_refinement_enabled = true;
            config.thermal_refinement_min_s = config.thermal_refinement_min_s.max(120);
            config.thermal_refinement_max_s = config.thermal_refinement_max_s.max(600);
            config.thermal_refinement_window_s = config.thermal_refinement_window_s.max(20);
            config.warm_start_thermal_check_s = config.warm_start_thermal_check_s.max(180);
            config.total_power_limit_w = clamp_preset_power_target(&config, 900);
            config.weak_chip_compensation = true;
            config.backoff_step_mhz = config.backoff_step_mhz.max(25);
            config.boost_back_threshold = config.boost_back_threshold.max(45);
            config.max_boost_attempts = config.max_boost_attempts.min(2);
            config.auto_rollback_post_tune = true;
            config.voltage_optimization =
                user_voltage_opt && capabilities.voltage_optimization_supported;
            if let Some(min_voltage) = minimum_voltage_for_capabilities(capabilities) {
                config.min_voltage_mv = config.min_voltage_mv.max(min_voltage);
            }
            config.voltage_margin_mv = config.voltage_margin_mv.max(30);
        }
        "balanced_home" => {
            config.target_mode = TuneTarget::Power;
            config.target_watts = clamp_preset_power_target(&config, 800);
            config.max_freq_mhz = config.max_freq_mhz.min(650);
            config.safety_margin_pct = config.safety_margin_pct.max(6.0);
            config.measurement_window_s = config.measurement_window_s.max(6);
            config.verification_window_s = config.verification_window_s.max(18);
            config.thermal_hysteresis_c = config.thermal_hysteresis_c.max(3.5);
            config.thermal_derating_per_c = config.thermal_derating_per_c.max(0.0035);
            config.thermal_refinement_enabled = true;
            config.thermal_refinement_min_s = config.thermal_refinement_min_s.max(120);
            config.thermal_refinement_max_s = config.thermal_refinement_max_s.max(600);
            config.thermal_refinement_window_s = config.thermal_refinement_window_s.max(15);
            config.warm_start_thermal_check_s = config.warm_start_thermal_check_s.max(150);
            config.total_power_limit_w = clamp_preset_power_target(&config, 1100);
            config.weak_chip_compensation = true;
            config.backoff_step_mhz = config.backoff_step_mhz.max(25);
            config.boost_back_threshold = config.boost_back_threshold.max(35);
            config.max_boost_attempts = config.max_boost_attempts.min(3);
            config.auto_rollback_post_tune = true;
            config.voltage_optimization =
                user_voltage_opt && capabilities.voltage_optimization_supported;
            if let Some(min_voltage) = minimum_voltage_for_capabilities(capabilities) {
                config.min_voltage_mv = config.min_voltage_mv.max(min_voltage);
            }
            config.voltage_margin_mv = config.voltage_margin_mv.max(25);
        }
        "efficiency_max" => {
            config.target_mode = TuneTarget::Efficiency;
            config.safety_margin_pct = config.safety_margin_pct.max(6.0);
            config.measurement_window_s = config.measurement_window_s.max(8);
            config.verification_window_s = config.verification_window_s.max(20);
            config.thermal_hysteresis_c = config.thermal_hysteresis_c.max(3.5);
            config.thermal_derating_per_c = config.thermal_derating_per_c.max(0.0035);
            config.thermal_refinement_enabled = true;
            config.thermal_refinement_min_s = config.thermal_refinement_min_s.max(120);
            config.thermal_refinement_max_s = config.thermal_refinement_max_s.max(600);
            config.thermal_refinement_window_s = config.thermal_refinement_window_s.max(15);
            config.warm_start_thermal_check_s = config.warm_start_thermal_check_s.max(180);
            config.total_power_limit_w =
                clamp_preset_power_target(&config, default_watt_cap_for_capabilities(capabilities));
            config.weak_chip_compensation = true;
            config.min_hashrate_ratio = config.min_hashrate_ratio.max(0.75);
            config.backoff_step_mhz = config.backoff_step_mhz.max(25);
            config.auto_rollback_post_tune = true;
            config.voltage_optimization =
                user_voltage_opt && capabilities.voltage_optimization_supported;
            if let Some(min_voltage) = minimum_voltage_for_capabilities(capabilities) {
                config.min_voltage_mv = config.min_voltage_mv.max(min_voltage);
            }
            config.voltage_margin_mv = config.voltage_margin_mv.max(20);
        }
        "hashrate_max" => {
            config.target_mode = TuneTarget::Hashrate;
            config.voltage_optimization = false;
            config.safety_margin_pct = config.safety_margin_pct.min(3.0);
            config.measurement_window_s = config.measurement_window_s.clamp(3, 4);
            config.verification_window_s = config.verification_window_s.clamp(10, 12);
            config.error_threshold_pct = config.error_threshold_pct.max(2.5);
            config.thermal_refinement_enabled = true;
            config.thermal_refinement_min_s = config.thermal_refinement_min_s.max(60);
            config.thermal_refinement_max_s = config.thermal_refinement_max_s.max(300);
            config.thermal_refinement_window_s = config.thermal_refinement_window_s.max(10);
            config.min_hashrate_ratio = config.min_hashrate_ratio.max(0.65);
            config.backoff_step_mhz = config.backoff_step_mhz.max(25);
            config.boost_back_threshold = config.boost_back_threshold.min(20);
            config.max_boost_attempts = config.max_boost_attempts.max(4);
            config.auto_rollback_post_tune = true;
        }
        "watt_cap" => {
            config.target_mode = TuneTarget::Power;
            let desired = if config.target_watts > 0 {
                config.target_watts
            } else {
                default_watt_cap_for_capabilities(capabilities)
            };
            config.target_watts = clamp_preset_power_target(&config, desired);
            config.measurement_window_s = config.measurement_window_s.max(8);
            config.verification_window_s = config.verification_window_s.max(20);
            config.thermal_hysteresis_c = config.thermal_hysteresis_c.max(3.5);
            config.thermal_derating_per_c = config.thermal_derating_per_c.max(0.0035);
            config.thermal_refinement_enabled = true;
            config.thermal_refinement_min_s = config.thermal_refinement_min_s.max(120);
            config.thermal_refinement_max_s = config.thermal_refinement_max_s.max(600);
            config.thermal_refinement_window_s = config.thermal_refinement_window_s.max(15);
            config.warm_start_thermal_check_s = config.warm_start_thermal_check_s.max(180);
            config.total_power_limit_w = clamp_preset_power_target(&config, desired.max(400));
            config.weak_chip_compensation = true;
            config.min_hashrate_ratio = config.min_hashrate_ratio.max(0.75);
            config.backoff_step_mhz = config.backoff_step_mhz.max(25);
            config.auto_rollback_post_tune = true;
            config.voltage_optimization =
                user_voltage_opt && capabilities.voltage_optimization_supported;
            if let Some(min_voltage) = minimum_voltage_for_capabilities(capabilities) {
                config.min_voltage_mv = config.min_voltage_mv.max(min_voltage);
            }
            config.voltage_margin_mv = config.voltage_margin_mv.max(20);
        }
        "advanced_manual" => {}
        _ => {}
    }

    config
}

fn fallback_preset_for_capabilities(
    requested_preset: &str,
    capabilities: &AutotunerCapabilityStatus,
) -> Option<&'static str> {
    if capabilities.family_key == "mixed" {
        return is_supported_autotuner_preset_for_capabilities("advanced_manual", capabilities)
            .then_some("advanced_manual");
    }

    match requested_preset {
        "quiet_home" | "balanced_home" => {
            if is_supported_autotuner_preset_for_capabilities("efficiency_max", capabilities)
                && capabilities.voltage_optimization_supported
            {
                Some("efficiency_max")
            } else if is_supported_autotuner_preset_for_capabilities("watt_cap", capabilities) {
                Some("watt_cap")
            } else if is_supported_autotuner_preset_for_capabilities(
                "advanced_manual",
                capabilities,
            ) {
                Some("advanced_manual")
            } else {
                None
            }
        }
        "efficiency_max" => {
            if is_supported_autotuner_preset_for_capabilities("watt_cap", capabilities) {
                Some("watt_cap")
            } else if is_supported_autotuner_preset_for_capabilities(
                "advanced_manual",
                capabilities,
            ) {
                Some("advanced_manual")
            } else {
                None
            }
        }
        _ => is_supported_autotuner_preset_for_capabilities("advanced_manual", capabilities)
            .then_some("advanced_manual"),
    }
}

fn unsupported_preset_reason(
    requested_preset: &str,
    capabilities: &AutotunerCapabilityStatus,
) -> String {
    if capabilities.family_key == "mixed" {
        "mixed_family_runtime_not_ready".to_string()
    } else if matches!(requested_preset, "quiet_home" | "balanced_home")
        && !capabilities.quiet_home_presets
    {
        "home_presets_not_supported_for_this_family".to_string()
    } else if requested_preset == "efficiency_max" && !capabilities.voltage_optimization_supported {
        "efficiency_preset_requires_voltage_control".to_string()
    } else {
        format!(
            "preset_not_supported_for_capability_profile:{}",
            capabilities.profile_key
        )
    }
}

pub fn resolve_autotuner_policy(
    base: &AutoTunerConfig,
    capabilities: &AutotunerCapabilityStatus,
) -> ResolvedAutotunerPolicy {
    let requested_preset = base.preset.clone().and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });

    let mut policy = ResolvedAutotunerPolicy {
        requested_preset: requested_preset.clone(),
        effective_preset: None,
        requested_preset_supported: requested_preset
            .as_deref()
            .map(|slug| is_supported_autotuner_preset_for_capabilities(slug, capabilities)),
        requested_preset_reason: None,
        degraded_from_requested: false,
        capabilities: capabilities.clone(),
        effective_config: base.clone(),
    };

    let Some(requested_slug) = requested_preset.as_deref() else {
        return policy;
    };

    if !is_supported_autotuner_preset(requested_slug) {
        policy.requested_preset_reason = Some("unknown_preset".to_string());
        return policy;
    }

    let effective_slug =
        if is_supported_autotuner_preset_for_capabilities(requested_slug, capabilities) {
            Some(requested_slug)
        } else {
            fallback_preset_for_capabilities(requested_slug, capabilities)
        };

    if let Some(effective_slug) = effective_slug {
        policy.effective_preset = Some(effective_slug.to_string());
        policy.degraded_from_requested = effective_slug != requested_slug;
        if policy.degraded_from_requested {
            policy.requested_preset_reason =
                Some(unsupported_preset_reason(requested_slug, capabilities));
        }
        policy.effective_config = apply_preset_to_config(base, effective_slug, capabilities);
    } else {
        policy.degraded_from_requested = true;
        policy.requested_preset_reason =
            Some(unsupported_preset_reason(requested_slug, capabilities));
    }

    policy
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_measurement_window() -> u64 {
    6
}

fn default_at3_rail_read_interval() -> u64 {
    AT3_RAIL_READ_INTERVAL_DEFAULT_S
}

fn default_verification_window() -> u64 {
    15
}

fn default_error_threshold() -> f64 {
    2.0
}

fn default_safety_margin() -> f64 {
    5.0
}

fn default_min_freq() -> u16 {
    200
}

fn default_max_freq() -> u16 {
    750
}

fn default_background_interval() -> u64 {
    60
}

fn default_profile_path() -> String {
    "/data/dcent".to_string()
}

fn default_max_consecutive_errors() -> u32 {
    3
}

fn default_backoff_step() -> u16 {
    25
}

fn default_power_step_w() -> u32 {
    300
}

fn default_hashrate_step_ths() -> f64 {
    11.0
}

fn default_derating_coeff() -> f32 {
    0.003
}

fn default_min_voltage() -> u16 {
    8400
}

fn default_voltage_margin() -> u16 {
    20
}

fn default_thermal_hysteresis() -> f32 {
    3.0
}

fn default_min_hashrate_ratio() -> f64 {
    0.7
}

fn default_rollback_error_multiplier() -> f64 {
    2.0
}

fn default_thermal_refinement_max() -> u64 {
    600
}

fn default_thermal_refinement_min() -> u64 {
    120
}

fn default_thermal_refinement_window() -> u64 {
    15
}

fn default_thermal_stability() -> f32 {
    0.2
}

fn default_warm_start_thermal_check() -> u64 {
    120
}

fn default_thermal_degradation_warn() -> f32 {
    15.0
}

fn default_total_power_limit() -> u32 {
    1400
}

fn default_dvfs_step() -> u16 {
    100
}

fn default_dvfs_voltage_points() -> u8 {
    5
}

fn default_immersion_offset() -> f32 {
    20.0
}

fn default_boost_back_threshold() -> u32 {
    30
}

fn default_max_boost_attempts() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_validates() {
        let config = AutoTunerConfig::default();
        assert!(config.validate().is_ok());
        assert!(!config.enabled);
        assert!(!config.voltage_optimization);
    }

    /// W1.3 — `TuneTarget::default()` must be `Efficiency`, not `Hashrate`.
    /// Home miners pay for electricity; the autotuner default has to
    /// reflect that. Hacker mode opts back into `Hashrate` via `for_mode`.
    #[test]
    fn test_tune_target_default_is_efficiency() {
        assert_eq!(TuneTarget::default(), TuneTarget::Efficiency);
    }

    /// W1.3 — Mode-aware dispatch contract.
    /// Home + Standard (Mining) modes both default to `Efficiency`.
    /// Hacker mode is the only one that opts into `Hashrate`.
    /// Unknown / mistyped mode strings fall through to the safe default
    /// (Efficiency) — fail-safe for home miners.
    #[test]
    fn test_tune_target_for_mode_dispatch() {
        // Home mode → efficiency (J/TH wins over TH/s).
        assert_eq!(
            TuneTarget::for_mode("home"),
            TuneTarget::Efficiency,
            "Home (Heater) mode must default to Efficiency"
        );
        // "heater" alias for the dashboard's local mode name.
        assert_eq!(
            TuneTarget::for_mode("heater"),
            TuneTarget::Efficiency,
            "Heater alias must default to Efficiency"
        );

        // Standard (Mining) mode → efficiency. Mining mode is still
        // a home miner — don't default them to leaderboard mode.
        assert_eq!(
            TuneTarget::for_mode("standard"),
            TuneTarget::Efficiency,
            "Standard (Mining) mode must default to Efficiency"
        );
        assert_eq!(
            TuneTarget::for_mode("mining"),
            TuneTarget::Efficiency,
            "Mining alias must default to Efficiency"
        );

        // Hacker mode → hashrate. Raw register access users opted in.
        assert_eq!(
            TuneTarget::for_mode("hacker"),
            TuneTarget::Hashrate,
            "Hacker mode must default to Hashrate"
        );

        // Fail-safe fallback: unknown mode strings land on the safe
        // home-miner default (Efficiency), NOT Hashrate.
        assert_eq!(
            TuneTarget::for_mode("garbage_value"),
            TuneTarget::Efficiency,
            "Unknown mode strings must fall through to Efficiency, not Hashrate"
        );
        assert_eq!(
            TuneTarget::for_mode(""),
            TuneTarget::Efficiency,
            "Empty mode strings must fall through to Efficiency"
        );
    }

    #[test]
    fn test_voltage_range_ignored_when_voltage_optimization_disabled() {
        let config = AutoTunerConfig {
            voltage_optimization: false,
            min_voltage_mv: 12000,
            voltage_margin_mv: 500,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_voltage_range_allows_ds_pic_family_values() {
        let config = AutoTunerConfig {
            voltage_optimization: true,
            min_voltage_mv: 12000,
            voltage_margin_mv: 50,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_voltage_range_rejects_extreme_values() {
        let low = AutoTunerConfig {
            voltage_optimization: true,
            min_voltage_mv: 4000,
            voltage_margin_mv: 50,
            ..Default::default()
        };
        assert!(low.validate().is_err());

        let high = AutoTunerConfig {
            voltage_optimization: true,
            min_voltage_mv: 21000,
            voltage_margin_mv: 50,
            ..Default::default()
        };
        assert!(high.validate().is_err());
    }

    #[test]
    fn test_bm1387_capabilities_include_home_presets() {
        let caps = autotuner_capabilities_for_chip(0x1387, "pic16");
        assert!(caps.quiet_home_presets);
        assert!(caps.voltage_optimization_supported);
        assert!(is_supported_autotuner_preset_for_capabilities(
            "quiet_home",
            &caps
        ));
        assert!(is_supported_autotuner_preset_for_capabilities(
            "balanced_home",
            &caps
        ));
    }

    #[test]
    fn test_bm1368_capabilities_gate_home_presets() {
        let caps = autotuner_capabilities_for_chip(0x1368, "nopic");
        assert!(!caps.quiet_home_presets);
        assert!(!caps.voltage_optimization_supported);
        assert!(!is_supported_autotuner_preset_for_capabilities(
            "quiet_home",
            &caps
        ));
        assert!(!is_supported_autotuner_preset_for_capabilities(
            "efficiency_max",
            &caps
        ));
        assert!(is_supported_autotuner_preset_for_capabilities(
            "watt_cap", &caps
        ));
    }

    #[test]
    fn test_non_bm1387_voltage_profiles_do_not_advertise_voltage_optimization() {
        for (chip_id, voltage_control) in [
            (0x1397, "pic16"),
            (0x1397, "dspic"),
            (0x1398, "dspic"),
            (0x1362, "dspic"),
            (0x1366, "dspic"),
        ] {
            let caps = autotuner_capabilities_for_chip(chip_id, voltage_control);
            assert!(!caps.voltage_optimization_supported);
            assert!(!is_supported_autotuner_preset_for_capabilities(
                "efficiency_max",
                &caps
            ));
            assert!(is_supported_autotuner_preset_for_capabilities(
                "watt_cap", &caps
            ));
        }
    }

    #[test]
    fn test_unknown_family_has_no_supported_presets() {
        let caps = autotuner_capabilities_for_chip(0x1391, "unknown");
        assert!(caps.supported_preset_slugs.is_empty());
        assert!(!is_supported_autotuner_preset_for_capabilities(
            "advanced_manual",
            &caps
        ));
    }

    #[test]
    fn test_mixed_family_capabilities_are_conservative() {
        let caps = autotuner_capabilities_for_mixed_families();
        assert_eq!(caps.family_key, "mixed");
        assert_eq!(caps.profile_key, "mixed-family-conservative");
        assert!(!caps.mixed_family_ready);
        assert!(is_supported_autotuner_preset_for_capabilities(
            "advanced_manual",
            &caps
        ));
        assert!(!is_supported_autotuner_preset_for_capabilities(
            "efficiency_max",
            &caps
        ));
    }

    #[test]
    fn test_resolve_bm1387_quiet_home_policy() {
        let base = AutoTunerConfig {
            preset: Some("quiet_home".to_string()),
            ..Default::default()
        };
        let caps = autotuner_capabilities_for_chip(0x1387, "pic16");
        let policy = resolve_autotuner_policy(&base, &caps);

        assert_eq!(policy.effective_preset.as_deref(), Some("quiet_home"));
        assert!(!policy.degraded_from_requested);
        assert_eq!(policy.effective_config.target_mode, TuneTarget::Power);
        assert_eq!(policy.effective_config.target_watts, 500);
        assert!(!policy.effective_config.voltage_optimization);
    }

    #[test]
    fn test_resolve_no_pic_efficiency_degrades_to_watt_cap() {
        let base = AutoTunerConfig {
            preset: Some("efficiency_max".to_string()),
            ..Default::default()
        };
        let caps = autotuner_capabilities_for_chip(0x1368, "nopic");
        let policy = resolve_autotuner_policy(&base, &caps);

        assert_eq!(policy.effective_preset.as_deref(), Some("watt_cap"));
        assert!(policy.degraded_from_requested);
        assert_eq!(
            policy.requested_preset_reason.as_deref(),
            Some("efficiency_preset_requires_voltage_control")
        );
        assert_eq!(policy.effective_config.target_mode, TuneTarget::Power);
        assert!(!policy.effective_config.voltage_optimization);
    }

    #[test]
    fn test_resolve_mixed_family_falls_back_to_advanced_manual() {
        let base = AutoTunerConfig {
            preset: Some("quiet_home".to_string()),
            ..Default::default()
        };
        let caps = autotuner_capabilities_for_mixed_families();
        let policy = resolve_autotuner_policy(&base, &caps);

        assert_eq!(policy.effective_preset.as_deref(), Some("advanced_manual"));
        assert!(policy.degraded_from_requested);
        assert_eq!(
            policy.requested_preset_reason.as_deref(),
            Some("mixed_family_runtime_not_ready")
        );
    }

    #[test]
    fn test_validate_min_gt_max_freq() {
        let config = AutoTunerConfig {
            min_freq_mhz: 800,
            max_freq_mhz: 200,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_zero_measurement_window() {
        let config = AutoTunerConfig {
            measurement_window_s: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_zero_background_interval() {
        // FWAT-1: background_interval_s = 0 would panic tokio::time::interval at
        // runtime — validate() must reject it (the daemon makes validate() fatal).
        let config = AutoTunerConfig {
            background_interval_s: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_at3_rail_read_interval_bounds() {
        // TUNE-4: validate() range-validates at3_rail_read_interval_s the same
        // fail-loud way as the sibling interval fields. A 0 cadence and a value
        // past the documented ceiling are rejected; sane in-band values pass.

        // 0 is rejected (meaningless cadence).
        let zero = AutoTunerConfig {
            at3_rail_read_interval_s: 0,
            ..Default::default()
        };
        assert!(
            zero.validate().is_err(),
            "at3_rail_read_interval_s = 0 must be rejected"
        );

        // Absurd value above the ceiling is rejected (almost always a typo).
        let absurd = AutoTunerConfig {
            at3_rail_read_interval_s: AT3_RAIL_READ_INTERVAL_CEIL_S + 1,
            ..Default::default()
        };
        assert!(
            absurd.validate().is_err(),
            "at3_rail_read_interval_s above the {} s ceiling must be rejected",
            AT3_RAIL_READ_INTERVAL_CEIL_S
        );

        // A sane in-band value passes (the default 30 s, and the band edges).
        let ok = AutoTunerConfig {
            at3_rail_read_interval_s: AT3_RAIL_READ_INTERVAL_DEFAULT_S,
            ..Default::default()
        };
        assert!(
            ok.validate().is_ok(),
            "a sane in-band at3_rail_read_interval_s must be accepted"
        );
        let floor = AutoTunerConfig {
            at3_rail_read_interval_s: AT3_RAIL_READ_INTERVAL_FLOOR_S,
            ..Default::default()
        };
        assert!(floor.validate().is_ok());
        let ceil = AutoTunerConfig {
            at3_rail_read_interval_s: AT3_RAIL_READ_INTERVAL_CEIL_S,
            ..Default::default()
        };
        assert!(ceil.validate().is_ok());
    }

    #[test]
    fn test_validate_error_threshold_bounds() {
        let config = AutoTunerConfig {
            error_threshold_pct: 0.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = AutoTunerConfig {
            error_threshold_pct: 100.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_safety_margin_bounds() {
        let config = AutoTunerConfig {
            safety_margin_pct: 0.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = AutoTunerConfig {
            safety_margin_pct: 50.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_zero_backoff_step() {
        let config = AutoTunerConfig {
            backoff_step_mhz: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_default_profile_path() {
        let config = AutoTunerConfig::default();
        assert_eq!(config.profile_path, "/data/dcent");
    }

    #[test]
    fn test_default_max_freq() {
        let config = AutoTunerConfig::default();
        assert_eq!(config.max_freq_mhz, 750);
    }

    #[test]
    fn test_tuner_mode_serializes_power_target() {
        let mode = TunerMode::PowerTarget { watts: 1200 };
        let json = serde_json::to_value(&mode).unwrap();
        assert_eq!(json["mode"], "power_target");
        assert_eq!(json["watts"], 1200);
    }

    #[test]
    fn test_tuner_mode_applies_legacy_power_fields() {
        let mode = TunerMode::Heater { btu_h: 5118 };
        let mut config = AutoTunerConfig::default();
        mode.apply_to_config(&mut config);

        assert_eq!(config.target_mode, TuneTarget::Power);
        assert_eq!(config.target_watts, 1500);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_tuner_mode_hashrate_target_validation() {
        let mut config = AutoTunerConfig {
            tuner_mode: Some(TunerMode::HashrateTarget { ths: 0.0 }),
            ..Default::default()
        };
        assert!(config.validate().is_err());

        config.tuner_mode = Some(TunerMode::HashrateTarget { ths: 120.0 });
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_dps_step_defaults_validate() {
        let config = AutoTunerConfig::default();
        assert_eq!(config.power_step_w, 300);
        assert_eq!(config.hashrate_step_ths, 11.0);
        assert!(config.validate().is_ok());
    }

    // ---------------------------------------------------------------
    //  am2/BM1362 frequency-only gate tests
    // ---------------------------------------------------------------

    #[test]
    fn am2_frequency_autotune_defaults_off() {
        // Default-OFF is load-bearing: zero behavior change on the
        // proven live `a lab unit` / XIL am2 path until the operator opts in.
        let cfg = AutoTunerConfig::default();
        assert!(
            !cfg.am2_frequency_autotune,
            "am2_frequency_autotune MUST default to false"
        );
        // Neither config nor env → gate stays closed.
        assert!(!cfg.am2_frequency_autotune_enabled(None));
        assert!(!cfg.am2_frequency_autotune_enabled(Some("0")));
        assert!(!cfg.am2_frequency_autotune_enabled(Some("")));
        assert!(!cfg.am2_frequency_autotune_enabled(Some("false")));
    }

    #[test]
    fn am2_frequency_autotune_opt_in_via_config_or_env() {
        // Config key opts in.
        let mut cfg = AutoTunerConfig::default();
        cfg.am2_frequency_autotune = true;
        assert!(cfg.am2_frequency_autotune_enabled(None));

        // Env opts in even when the config key is false.
        let cfg2 = AutoTunerConfig::default();
        assert!(!cfg2.am2_frequency_autotune);
        for truthy in ["1", "true", "TRUE", "yes", "On", " on "] {
            assert!(
                cfg2.am2_frequency_autotune_enabled(Some(truthy)),
                "env value {:?} should opt in",
                truthy
            );
        }
        for falsy in ["0", "false", "no", "off", "", "garbage"] {
            assert!(
                !cfg2.am2_frequency_autotune_enabled(Some(falsy)),
                "env value {:?} must NOT opt in",
                falsy
            );
        }
    }

    #[test]
    fn at3_rail_read_defaults_off() {
        // Default-OFF is load-bearing: with the gate closed the am2
        // serial-dispatch loop is byte-identical to the proven path.
        let cfg = AutoTunerConfig::default();
        assert!(!cfg.at3_rail_read, "at3_rail_read MUST default to false");
        assert!(!cfg.at3_rail_read_enabled(None));
        assert!(!cfg.at3_rail_read_enabled(Some("0")));
        assert!(!cfg.at3_rail_read_enabled(Some("")));
        assert!(!cfg.at3_rail_read_enabled(Some("false")));
    }

    #[test]
    fn at3_rail_read_opt_in_via_config_or_env() {
        // Config key opts in.
        let mut cfg = AutoTunerConfig::default();
        cfg.at3_rail_read = true;
        assert!(cfg.at3_rail_read_enabled(None));

        // Env opts in even when the config key is false.
        let cfg2 = AutoTunerConfig::default();
        assert!(!cfg2.at3_rail_read);
        for truthy in ["1", "true", "TRUE", "yes", "On", " on "] {
            assert!(
                cfg2.at3_rail_read_enabled(Some(truthy)),
                "env value {:?} should opt in",
                truthy
            );
        }
        for falsy in ["0", "false", "no", "off", "", "garbage"] {
            assert!(
                !cfg2.at3_rail_read_enabled(Some(falsy)),
                "env value {:?} must NOT opt in",
                falsy
            );
        }
    }

    #[test]
    fn at3_rail_read_interval_defaults_30_and_clamps_to_15_120() {
        let cfg = AutoTunerConfig::default();
        assert_eq!(cfg.at3_rail_read_interval_s, 30);
        assert_eq!(cfg.at3_rail_read_interval_s_clamped(), 30);

        // Below floor clamps UP to 15.
        let mut fast = AutoTunerConfig::default();
        fast.at3_rail_read_interval_s = 0;
        assert_eq!(fast.at3_rail_read_interval_s_clamped(), 15);
        fast.at3_rail_read_interval_s = 7;
        assert_eq!(fast.at3_rail_read_interval_s_clamped(), 15);

        // Above ceiling clamps DOWN to 120.
        let mut slow = AutoTunerConfig::default();
        slow.at3_rail_read_interval_s = 9_999;
        assert_eq!(slow.at3_rail_read_interval_s_clamped(), 120);

        // In-band passes through.
        let mut mid = AutoTunerConfig::default();
        mid.at3_rail_read_interval_s = 45;
        assert_eq!(mid.at3_rail_read_interval_s_clamped(), 45);
    }

    #[test]
    fn pin_am2_bm1362_hard_disables_voltage_and_dvfs() {
        // Even if an operator tried to enable voltage_optimization /
        // dvfs in TOML, the pin force-disables them. No live voltage
        // write on am2 this wave — load-bearing invariant.
        let mut cfg = AutoTunerConfig::default();
        cfg.voltage_optimization = true;
        cfg.dvfs_enabled = true;
        cfg.pin_am2_bm1362_frequency_only();
        assert!(
            !cfg.voltage_optimization,
            "voltage_optimization MUST be hard-pinned false for am2/BM1362"
        );
        assert!(
            !cfg.dvfs_enabled,
            "dvfs_enabled MUST be hard-pinned false for am2/BM1362"
        );
    }

    #[test]
    fn pin_am2_bm1362_clamps_freq_band_to_245_545() {
        // Operator asks for a wild 100-900 MHz band; the pin clamps it
        // to the home-safe nameplate window. Never explore above 545.
        let mut cfg = AutoTunerConfig::default();
        cfg.min_freq_mhz = 100;
        cfg.max_freq_mhz = 900;
        cfg.pin_am2_bm1362_frequency_only();
        assert_eq!(
            cfg.min_freq_mhz, AM2_BM1362_FREQ_BAND_MIN_MHZ,
            "min freq must clamp UP to 245"
        );
        assert_eq!(
            cfg.max_freq_mhz, AM2_BM1362_FREQ_BAND_MAX_MHZ,
            "max freq must clamp DOWN to 545 — no above-nameplate exploration"
        );
        assert_eq!(AM2_BM1362_FREQ_BAND_MIN_MHZ, 245);
        assert_eq!(AM2_BM1362_FREQ_BAND_MAX_MHZ, 545);
        assert!(cfg.validate().is_ok(), "pinned config must still validate");
    }

    #[test]
    fn pin_am2_bm1362_keeps_a_tighter_operator_band() {
        // An operator band already inside [245,545] is preserved — the
        // pin is a clamp, not a forced overwrite.
        let mut cfg = AutoTunerConfig::default();
        cfg.min_freq_mhz = 400;
        cfg.max_freq_mhz = 525;
        cfg.pin_am2_bm1362_frequency_only();
        assert_eq!(cfg.min_freq_mhz, 400);
        assert_eq!(cfg.max_freq_mhz, 525);
    }

    #[test]
    fn pin_am2_bm1362_recovers_inverted_band() {
        // Degenerate inverted band collapses to the nameplate band so
        // validate()'s min<=max invariant holds and the downstream
        // tuner clamp stays sane.
        let mut cfg = AutoTunerConfig::default();
        cfg.min_freq_mhz = 600; // > 545 → clamps to 545
        cfg.max_freq_mhz = 300; // < 545 stays, but now min(545) > max(300)
        cfg.pin_am2_bm1362_frequency_only();
        assert!(cfg.min_freq_mhz <= cfg.max_freq_mhz);
        assert_eq!(cfg.min_freq_mhz, AM2_BM1362_FREQ_BAND_MIN_MHZ);
        assert_eq!(cfg.max_freq_mhz, AM2_BM1362_FREQ_BAND_MAX_MHZ);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn pin_am2_bm1362_is_idempotent() {
        let mut a = AutoTunerConfig::default();
        a.min_freq_mhz = 100;
        a.max_freq_mhz = 900;
        a.voltage_optimization = true;
        a.dvfs_enabled = true;
        a.pin_am2_bm1362_frequency_only();
        let mut b = a.clone();
        b.pin_am2_bm1362_frequency_only();
        assert_eq!(a.min_freq_mhz, b.min_freq_mhz);
        assert_eq!(a.max_freq_mhz, b.max_freq_mhz);
        assert_eq!(a.voltage_optimization, b.voltage_optimization);
        assert_eq!(a.dvfs_enabled, b.dvfs_enabled);
    }

    #[test]
    fn bm1362_sku_class_falls_back_to_the_safe_545_baseline_for_unknown_labels() {
        // Hardware-detection fail-safe: an unknown / misread SKU label must resolve
        // to the proven 545-MHz Standard baseline, never a wider ceiling — an
        // unrecognized board must never be overclocked past the safe baseline.
        // Only explicitly-known wider-band SKUs widen the ceiling.
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42611"),
            Bm1362SkuClass::MidBand
        );
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42801"),
            Bm1362SkuClass::HighBin
        );
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42601"),
            Bm1362SkuClass::Standard
        );

        // Unknown / garbage / empty labels ALL fail safe to Standard (545).
        for label in [
            "", "  ", "BHB99999", "S19", "unknown", "0x1362", "BHB", "garbage",
        ] {
            assert_eq!(
                Bm1362SkuClass::from_sku_label(label),
                Bm1362SkuClass::Standard,
                "unknown label {label:?} must fail safe to the 545 baseline"
            );
        }

        // Case + whitespace tolerance (per the doc contract).
        assert_eq!(
            Bm1362SkuClass::from_sku_label("  bhb42611  "),
            Bm1362SkuClass::MidBand
        );
        // The type default is the safe baseline.
        assert_eq!(Bm1362SkuClass::default(), Bm1362SkuClass::Standard);

        // The unknown-fallback (Standard) ceiling must be the LOWEST of all SKU
        // classes — an unknown board can never get a higher ceiling than any known
        // class.
        let ceilings = [
            Bm1362SkuClass::Standard.max_freq_mhz(),
            Bm1362SkuClass::MidBand.max_freq_mhz(),
            Bm1362SkuClass::HighBin.max_freq_mhz(),
        ];
        assert_eq!(
            Bm1362SkuClass::Standard.max_freq_mhz(),
            *ceilings.iter().min().unwrap(),
            "the unknown-fallback (Standard) ceiling must be the lowest SKU ceiling"
        );
        assert_eq!(Bm1362SkuClass::Standard.max_freq_mhz(), 545);
    }

    #[test]
    fn env_flag_truthiness_contract() {
        assert!(env_flag_is_truthy("1"));
        assert!(env_flag_is_truthy("true"));
        assert!(env_flag_is_truthy("YES"));
        assert!(env_flag_is_truthy(" on "));
        assert!(!env_flag_is_truthy("0"));
        assert!(!env_flag_is_truthy("false"));
        assert!(!env_flag_is_truthy(""));
        assert!(!env_flag_is_truthy("2"));
    }

    #[test]
    fn am2_pin_preserves_other_family_config_when_not_applied() {
        // Sanity: the pin is opt-in scoped by the DAEMON to am2/BM1362
        // only. A default config that never has the pin applied keeps
        // its wide defaults — proves the pin is not implicitly run.
        let cfg = AutoTunerConfig::default();
        assert_eq!(cfg.min_freq_mhz, default_min_freq());
        assert_eq!(cfg.max_freq_mhz, default_max_freq());
        assert!(!cfg.voltage_optimization);
        assert!(!cfg.dvfs_enabled);
    }

    // ---------------------------------------------------------------
    // PERF-004 — SKU-conditional BM1362 frequency ceiling
    // ---------------------------------------------------------------

    #[test]
    fn perf004_sku_ceiling_constants() {
        // The standard ceiling stays at the proven 545 nameplate; mid-band
        // and high-bin widen to the PLL-table maximum (597). High-bin does
        // NOT advertise a ceiling the silicon can't lock in our table.
        assert_eq!(AM2_BM1362_FREQ_BAND_MAX_MHZ, 545);
        assert_eq!(AM2_BM1362_FREQ_BAND_MAX_MID_BAND_MHZ, 597);
        assert_eq!(AM2_BM1362_FREQ_BAND_MAX_HIGH_BIN_MHZ, 597);
        assert_eq!(am2_bm1362_max_freq_for_sku(Bm1362SkuClass::Standard), 545);
        assert_eq!(am2_bm1362_max_freq_for_sku(Bm1362SkuClass::MidBand), 597);
        assert_eq!(am2_bm1362_max_freq_for_sku(Bm1362SkuClass::HighBin), 597);
        assert_eq!(Bm1362SkuClass::default(), Bm1362SkuClass::Standard);
    }

    #[test]
    fn perf004_default_pin_is_unchanged_545_ceiling() {
        // The historical `pin_am2_bm1362_frequency_only()` MUST still clamp to
        // 545 — already-working SKUs see byte-identical behavior. This is the
        // load-bearing live-default guard.
        let mut cfg = AutoTunerConfig::default();
        cfg.min_freq_mhz = 100;
        cfg.max_freq_mhz = 900;
        cfg.pin_am2_bm1362_frequency_only();
        assert_eq!(
            cfg.max_freq_mhz, 545,
            "default pin must keep the 545 ceiling"
        );

        // Standard SKU explicit form is identical.
        let mut std_cfg = AutoTunerConfig::default();
        std_cfg.max_freq_mhz = 900;
        std_cfg.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::Standard);
        assert_eq!(std_cfg.max_freq_mhz, 545);
    }

    #[test]
    fn perf004_mid_band_sku_widens_ceiling_to_597() {
        // BHB42611 mid-band: a 900 MHz operator band clamps to 597, NOT 545 —
        // so the SKU can run inside its designed (>545) band. Voltage/DVFS
        // still hard-off, min still clamps up to 245.
        let mut cfg = AutoTunerConfig::default();
        cfg.min_freq_mhz = 100;
        cfg.max_freq_mhz = 900;
        cfg.voltage_optimization = true;
        cfg.dvfs_enabled = true;
        cfg.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::MidBand);
        assert_eq!(cfg.max_freq_mhz, 597, "mid-band must widen to 597");
        assert_eq!(cfg.min_freq_mhz, AM2_BM1362_FREQ_BAND_MIN_MHZ);
        assert!(!cfg.voltage_optimization, "voltage still hard-off");
        assert!(!cfg.dvfs_enabled, "dvfs still hard-off");
        assert!(cfg.validate().is_ok());

        // A tighter operator band inside the wider window is preserved.
        let mut tight = AutoTunerConfig::default();
        tight.min_freq_mhz = 560;
        tight.max_freq_mhz = 580;
        tight.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::MidBand);
        assert_eq!(tight.min_freq_mhz, 560);
        assert_eq!(tight.max_freq_mhz, 580);
    }

    #[test]
    fn perf004_high_bin_sku_clamps_to_pll_table_max() {
        // High-bin is vendor-rated higher, but DCENT clamps to the
        // PLL-lockable maximum (597) — capability recorded, ceiling honest.
        let mut cfg = AutoTunerConfig::default();
        cfg.max_freq_mhz = 900;
        cfg.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::HighBin);
        assert_eq!(cfg.max_freq_mhz, 597);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn perf004_sku_label_parsing() {
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42601"),
            Bm1362SkuClass::Standard
        );
        assert_eq!(
            Bm1362SkuClass::from_sku_label("bhb42601"),
            Bm1362SkuClass::Standard
        );
        assert_eq!(
            Bm1362SkuClass::from_sku_label(" BHB42611 "),
            Bm1362SkuClass::MidBand
        );
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42801"),
            Bm1362SkuClass::HighBin
        );
        // Unknown labels fail safe to the 545 baseline.
        assert_eq!(
            Bm1362SkuClass::from_sku_label("WHATEVER"),
            Bm1362SkuClass::Standard
        );
        assert_eq!(Bm1362SkuClass::from_sku_label(""), Bm1362SkuClass::Standard);
    }

    // ---------------------------------------------------------------
    // PERF-006 — am2/BM1362 dsPIC voltage-autotune capability (gated)
    // ---------------------------------------------------------------

    #[test]
    fn perf006_voltage_autotune_defaults_off() {
        // Default-OFF is load-bearing — the pure capability function and the
        // overlay with the gate unset are byte-identical for BM1362 dsPIC.
        assert!(!am2_voltage_autotune_enabled(None));
        assert!(!am2_voltage_autotune_enabled(Some("0")));
        assert!(!am2_voltage_autotune_enabled(Some("false")));

        let pure = autotuner_capabilities_for_chip(0x1362, "dspic");
        assert!(!pure.voltage_optimization_supported);

        let gated_off =
            autotuner_capabilities_for_chip_with_voltage_autotune(0x1362, "dspic", None);
        assert!(!gated_off.voltage_optimization_supported);
        assert_eq!(
            gated_off.supported_preset_slugs, pure.supported_preset_slugs,
            "gate-off overlay must be byte-identical to the pure capability"
        );
    }

    #[test]
    fn perf006_voltage_autotune_opt_in_flips_capability() {
        for truthy in ["1", "true", "YES", " on "] {
            let caps = autotuner_capabilities_for_chip_with_voltage_autotune(
                0x1362,
                "dspic",
                Some(truthy),
            );
            assert!(
                caps.voltage_optimization_supported,
                "env {:?} must enable BM1362 dsPIC voltage optimization",
                truthy
            );
            assert!(is_supported_autotuner_preset_for_capabilities(
                "efficiency_max",
                &caps
            ));
        }
        // pic16 BM1362 (not dspic) is NOT widened by this gate.
        let pic16 =
            autotuner_capabilities_for_chip_with_voltage_autotune(0x1362, "pic16", Some("1"));
        assert!(!pic16.voltage_optimization_supported);
    }

    #[test]
    fn perf006_voltage_clamp_window() {
        // Clamp pins to the proven floor and the dsPIC hard cap.
        assert_eq!(AM2_DSPIC_VOLTAGE_AUTOTUNE_MIN_MV, 13_700);
        assert_eq!(AM2_DSPIC_VOLTAGE_AUTOTUNE_MAX_MV, 14_500);
        assert_eq!(clamp_am2_dspic_autotune_voltage_mv(13_000), 13_700);
        assert_eq!(clamp_am2_dspic_autotune_voltage_mv(13_800), 13_800);
        assert_eq!(clamp_am2_dspic_autotune_voltage_mv(15_000), 14_500);
        // Never exceeds the dsPIC 14500 hard cap.
        assert!(clamp_am2_dspic_autotune_voltage_mv(u16::MAX) <= 14_500);
    }

    // ---------------------------------------------------------------
    // PERF-011 — quiet_home / efficiency_max + voltage capability for
    // BM1397 / BM1398 dsPIC (all gated default-OFF)
    // ---------------------------------------------------------------

    #[test]
    fn perf011_bm1397_bm1398_dspic_presets_gated_off_by_default() {
        // With the gate unset, BM1397/BM1398 dsPIC keep the conservative
        // [hashrate_max, watt_cap, advanced_manual] set and no home presets —
        // matching `test_non_bm1387_voltage_profiles...`.
        for chip in [0x1397u16, 0x1398] {
            let off = autotuner_capabilities_for_chip_with_voltage_autotune(chip, "dspic", None);
            assert!(!off.voltage_optimization_supported);
            assert!(!off.quiet_home_presets);
            assert!(!is_supported_autotuner_preset_for_capabilities(
                "quiet_home",
                &off
            ));
            assert!(!is_supported_autotuner_preset_for_capabilities(
                "efficiency_max",
                &off
            ));
        }
    }

    #[test]
    fn perf011_bm1397_bm1398_dspic_presets_opt_in() {
        for chip in [0x1397u16, 0x1398] {
            let on =
                autotuner_capabilities_for_chip_with_voltage_autotune(chip, "dspic", Some("1"));
            assert!(on.voltage_optimization_supported);
            assert!(on.quiet_home_presets);
            assert!(is_supported_autotuner_preset_for_capabilities(
                "quiet_home",
                &on
            ));
            assert!(is_supported_autotuner_preset_for_capabilities(
                "efficiency_max",
                &on
            ));
        }
        // BM1398 pic16 is NOT widened by the gate (only dspic).
        let pic16 =
            autotuner_capabilities_for_chip_with_voltage_autotune(0x1398, "pic16", Some("1"));
        assert!(!pic16.voltage_optimization_supported);
    }
}
