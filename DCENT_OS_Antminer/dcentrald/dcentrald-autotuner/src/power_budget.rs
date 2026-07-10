//! Power budget model for ASIC miners.
//!
//! Implements a CMOS dynamic power model: P_chip = C_eff * V^2 * f
//! where C_eff is an empirical constant calibrated from known S9 data points.
//!
//! Reference calibration point (S9):
//!   189 chips x 650 MHz x 9.1V = ~1350W total
//!   Static overhead: 3 chains x ~50W + 20W control board = ~170W
//!   Dynamic power: 1350 - 170 = 1180W = ~6.24W per chip
//!   C_eff = P / (V^2 * f) = 6.24 / (9.1^2 * 650) = ~0.000116

use crate::profile::{ChipGrade, ChipProfile};

// BM1387/S9 runtime estimation constants.
//
// S9 has no PSU telemetry, so runtime power must be inferred from live miner state.
// We keep the existing calibrated V^2*f ASIC model, then layer in telemetry we do
// have on S9: board temperature and fan speed.
//
// Sources:
// - : S9 is estimation-only; improve via
//   actual voltage when available and temperature-dependent leakage.
// - : leakage rises ~2% per C.
// - `dcentrald-hal/src/fan.rs`: measured S9 fan curve (~1260 RPM at PWM 10, ~5940 RPM max).
// - `DCENT_OS_Antminer/:
//   observed quiet S9 baseline ~1078W board / ~1225W wall at 500 MHz.
const BM1387_RUNTIME_STATIC_PER_CHAIN_W: f64 = 45.0;
const BM1387_RUNTIME_CONTROL_BOARD_W: f64 = 25.0;
const BM1387_BASE_LEAKAGE_PER_CHIP_W: f64 = 0.25;
const BM1387_LEAKAGE_REF_TEMP_C: f32 = 55.0;
const BM1387_FAN_BASE_W: f64 = 8.0;
const BM1387_FAN_DYNAMIC_MAX_W: f64 = 56.0;
const BM1387_FAN_MAX_RPM: f64 = 5940.0;
const BM1387_FAN_PWM_MAX: u8 = 100;

fn default_power_calibration_multiplier() -> f64 {
    1.0
}

/// Persistent wall-meter correction applied to estimate-only power paths.
///
/// On S9/APW and similar setups we do not have a trustworthy wall-power sensor,
/// so operators can anchor the model to an external wall meter. The multiplier is
/// intentionally simple and global: it scales the same model that drives the
/// dashboard, efficiency calculations, and power-budget decisions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PowerCalibration {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_power_calibration_multiplier")]
    pub multiplier: f64,
    #[serde(default)]
    pub : Option<f64>,
    #[serde(default)]
    pub estimated_wall_watts: Option<f64>,
    #[serde(default)]
    pub estimated_board_watts: Option<f64>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
    /// W9.4 — operator-confirmed via external wattmeter.
    ///
    /// When true, the calibration was supplied by the operator with an external
    /// wattmeter at a known operating point. The recorded
    /// + `confirmed_hashrate_ths` form the source-of-truth J/TH anchor that the
    ///   efficiency mode optimizer prefers over the modeled C_eff estimate.
    #[serde(default)]
    pub operator_confirmed: bool,
    /// Hashrate (TH/s) that was being produced when the operator measured the
    /// wall watts. Used as the divisor for the operator-confirmed J/TH baseline.
    #[serde(default)]
    pub confirmed_hashrate_ths: Option<f64>,
}

impl Default for PowerCalibration {
    fn default() -> Self {
        Self {
            enabled: false,
            multiplier: 1.0,
            : None,
            estimated_wall_watts: None,
            estimated_board_watts: None,
            updated_at_ms: None,
            operator_confirmed: false,
            confirmed_hashrate_ths: None,
        }
    }
}

impl PowerCalibration {
    pub fn effective_multiplier(&self) -> f64 {
        if self.enabled {
            self.multiplier.clamp(0.5, 1.5)
        } else {
            1.0
        }
    }

    pub fn is_active(&self) -> bool {
        (self.effective_multiplier() - 1.0).abs() > 0.001
    }

    /// W9.4 — return the operator-confirmed J/TH baseline if the operator has
    /// supplied wall watts + a hashrate snapshot via `POST /api/perf/calibrate`.
    ///
    /// Returns `None` if the calibration is disabled, not operator-confirmed,
    /// or has no recorded hashrate.
    pub fn operator_confirmed_jth(&self) -> Option<f64> {
        if !self.enabled || !self.operator_confirmed {
            return None;
        }
        let watts = self.?;
        let ths = self.confirmed_hashrate_ths?;
        if !(watts.is_finite() && ths.is_finite()) || ths <= 0.0 || watts <= 0.0 {
            return None;
        }
        Some(watts / ths)
    }
}

/// Live power estimate computed every 5 seconds from actual operating parameters.
///
/// Consumed by REST API, WebSocket, and CGMiner API. Published via
/// `tokio::sync::watch` so multiple readers can access simultaneously
/// without contention.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DispatcherChainLimit {
    /// Chain identifier (e.g. 6/7/8).
    pub chain_id: u8,
    /// Lowest active dispatcher-owned ceiling for the chain.
    pub effective_ceiling_mhz: Option<u16>,
    /// Dominant runtime limiting source for the chain.
    pub dominant_source: Option<String>,
    /// All currently active dispatcher-owned sources for the chain.
    pub active_sources: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RuntimeWattCapState {
    /// Configured circuit or watt-cap ceiling.
    pub cap_watts: u32,
    /// Remaining room under the cap. Zero when already over.
    pub headroom_watts: f64,
    /// Current excess above the cap. Zero when under.
    pub overage_watts: f64,
    /// Current wall-power usage as a percentage of the cap.
    pub utilization_pct: f64,
    /// Whether the dispatcher is actively holding a power-cap ceiling on any chain.
    pub throttling: bool,
}

/// Ordered source class for power readings.
///
/// This is a read-only authority model: it describes which source should be
/// trusted for reporting/control decisions, but does not itself mutate miner
/// frequencies or voltages. Runtime code should prefer measured sources over
/// calibrated estimates, and calibrated estimates over raw model estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAuthorityKind {
    /// PSU PMBus telemetry, e.g. APW12 read_power/read_voltage.
    Pmbus,
    /// Board/current monitor telemetry, e.g. INA226 or platform ADC.
    Adc,
    /// Estimate anchored to a persisted wall-meter/wattometer calibration.
    WallCalibratedEstimate,
    /// Chip/frequency/voltage model without a live measurement anchor.
    Estimated,
    /// Explicitly unknown or unavailable.
    Unknown,
}

impl PowerAuthorityKind {
    pub fn from_source(source: &str, calibrated: bool) -> Self {
        match source.trim().to_ascii_lowercase().as_str() {
            "pmbus" | "psu" | "apw" | "apw12" | "apw121215" => Self::Pmbus,
            "adc" | "ina226" | "ina" => Self::Adc,
            "estimated" if calibrated => Self::WallCalibratedEstimate,
            "wall_calibrated_estimate" | "calibrated_estimate" => Self::WallCalibratedEstimate,
            "estimated" | "runtime_bootstrap" | "curtailment" => Self::Estimated,
            _ => Self::Unknown,
        }
    }

    pub fn priority(self) -> u8 {
        match self {
            Self::Pmbus => 4,
            Self::Adc => 3,
            Self::WallCalibratedEstimate => 2,
            Self::Estimated => 1,
            Self::Unknown => 0,
        }
    }

    pub fn is_measured(self) -> bool {
        matches!(self, Self::Pmbus | Self::Adc)
    }

    /// Stable wire label for this authority class (matches the serde
    /// `snake_case` rename). Used as the `power_basis` provenance marker on
    /// model-derived surfaces (e.g. [`crate::telemetry::EfficiencySnapshot`])
    /// so a consumer can tell modeled watts from measured ones.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pmbus => "pmbus",
            Self::Adc => "adc",
            Self::WallCalibratedEstimate => "wall_calibrated_estimate",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }
}

/// Normalized power sample selected from the best available source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PowerAuthoritySample {
    pub kind: PowerAuthorityKind,
    pub board_watts: f64,
    pub wall_watts: f64,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
    pub source: String,
}

impl PowerAuthoritySample {
    pub fn from_live_estimate(estimate: &LivePowerEstimate) -> Self {
        let kind = PowerAuthorityKind::from_source(&estimate.source, estimate.calibrated);
        let confidence = match kind {
            PowerAuthorityKind::Pmbus => 0.95,
            PowerAuthorityKind::Adc => 0.90,
            PowerAuthorityKind::WallCalibratedEstimate => 0.75,
            PowerAuthorityKind::Estimated => 0.45,
            PowerAuthorityKind::Unknown => 0.20,
        };

        Self {
            kind,
            board_watts: estimate.board_watts,
            wall_watts: estimate.wall_watts,
            confidence,
            age_ms: None,
            source: estimate.source.clone(),
        }
    }

    pub fn prefer(self, candidate: Self) -> Self {
        if candidate.kind.priority() > self.kind.priority() {
            candidate
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LivePowerEstimate {
    /// Board-level power in watts (sum of chip dynamic power + static overhead).
    pub board_watts: f64,
    /// Wall power in watts (board_watts / PSU efficiency).
    pub wall_watts: f64,
    /// Per-chain power in watts (one entry per active chain).
    pub per_chain_watts: Vec<f64>,
    /// Energy efficiency in joules per terahash (wall_watts / hashrate_TH).
    ///
    /// P1-3 (D-7): when published from the dispatcher this is computed against an
    /// EMA-smoothed hashrate denominator (see [`EfficiencyHashrateEma`]) so it no
    /// longer spikes when the raw 5 s hashrate momentarily dips toward zero.
    pub efficiency_jth: f64,
    /// P1-3 (D-7): `true` while the J/TH reading is still low-confidence — either
    /// computed from a single instantaneous sample (the per-call estimate) or
    /// before the dispatcher's EMA smoother has warmed up. Consumers should show
    /// it as "stabilizing" rather than a settled efficiency number.
    #[serde(default)]
    pub efficiency_jth_low_confidence: bool,
    /// Heat output in BTU/h (wall_watts * 3.412).
    pub btu_h: f64,
    /// Whether a persisted wall-meter calibration is currently applied.
    #[serde(default)]
    pub calibrated: bool,
    /// Active calibration multiplier when `calibrated=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_multiplier: Option<f64>,
    /// Data source: "estimated" (from C_eff model) or "pmbus" (from PSU telemetry).
    pub source: String,
    /// Dispatcher-owned active runtime ceilings per chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatcher_limits: Vec<DispatcherChainLimit>,
    /// Current circuit-cap / watt-cap state when runtime enforcement is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watt_cap: Option<RuntimeWattCapState>,
    /// Unix timestamp in milliseconds when this estimate was computed.
    pub timestamp_ms: u64,
}

/// Convert wall watts to BTU/h. 1 watt = 3.412 BTU/h.
///
/// CRITICAL: This must appear in ALL modes (Home, Standard, Hacker) —
/// it's the primary metric for space heater use case.
pub fn btu_from_watts(wall_watts: f64) -> f64 {
    wall_watts * 3.412
}

/// Compute J/TH from wall watts and a hashrate (TH/s).
///
/// Returns `0.0` when the hashrate is not a usable positive number — the
/// established "no efficiency reading yet" sentinel used across the power
/// surfaces (so an idle / cold-boot estimate publishes `0.0` rather than a
/// divide-by-zero infinity).
pub fn efficiency_jth_from(wall_watts: f64, hashrate_ths: f64) -> f64 {
    if hashrate_ths > 0.0 && hashrate_ths.is_finite() {
        wall_watts / hashrate_ths
    } else {
        0.0
    }
}

/// Exponential-moving-average smoother for the J/TH efficiency *denominator*.
///
/// `efficiency_jth = wall_watts / hashrate_ths`. Feeding it the raw 5-second
/// hashrate makes it unstable: the 5 s value legitimately swings between ~0 and
/// the full chip rate as nonce bursts arrive (the `.100` audit observed
/// 0..24 TH/s), and dividing a near-constant wall power by a denominator that
/// momentarily approaches zero produces physically impossible efficiency spikes.
/// We smooth the hashrate with an EMA and report low confidence until the
/// smoother has warmed up, so the dashboard can show "efficiency stabilizing"
/// instead of a garbage number.
///
/// The dispatcher owns one instance across the 5 s telemetry ticks and feeds it
/// the per-tick hashrate. Pure + host-testable (no clock, no HAL).
#[derive(Debug, Clone)]
pub struct EfficiencyHashrateEma {
    alpha: f64,
    smoothed_ths: Option<f64>,
    samples: u32,
}

impl EfficiencyHashrateEma {
    /// Smoothing factor. 0.30 ≈ a ~6-sample (≈30 s at the 5 s tick cadence) time
    /// constant — fast enough to track a real autotuner frequency step within a
    /// minute, slow enough to reject single-tick nonce-gap dropouts.
    pub const DEFAULT_ALPHA: f64 = 0.30;
    /// Valid samples required before the smoothed value is treated as confident.
    pub const MIN_CONFIDENT_SAMPLES: u32 = 6;

    pub fn new() -> Self {
        Self {
            alpha: Self::DEFAULT_ALPHA,
            smoothed_ths: None,
            samples: 0,
        }
    }

    /// Construct with an explicit smoothing factor (clamped to `[0, 1]`).
    pub fn with_alpha(alpha: f64) -> Self {
        Self {
            alpha: alpha.clamp(0.0, 1.0),
            ..Self::new()
        }
    }

    /// Fold one per-tick hashrate (TH/s) into the EMA.
    ///
    /// Returns `(smoothed_ths, confident)`. Non-finite or negative inputs are
    /// ignored (the previous smoothed value carries forward and the sample is not
    /// counted). `confident` is `false` until `MIN_CONFIDENT_SAMPLES` valid
    /// samples have been folded in.
    pub fn update(&mut self, hashrate_ths: f64) -> (f64, bool) {
        if hashrate_ths.is_finite() && hashrate_ths >= 0.0 {
            self.smoothed_ths = Some(match self.smoothed_ths {
                Some(prev) => self.alpha * hashrate_ths + (1.0 - self.alpha) * prev,
                None => hashrate_ths,
            });
            self.samples = self.samples.saturating_add(1);
        }
        (self.smoothed_ths.unwrap_or(0.0), self.confident())
    }

    /// The current smoothed hashrate, or `None` before the first valid sample.
    pub fn smoothed_ths(&self) -> Option<f64> {
        self.smoothed_ths
    }

    /// Whether enough valid samples have accumulated to trust the smoothed value.
    pub fn confident(&self) -> bool {
        self.samples >= Self::MIN_CONFIDENT_SAMPLES
    }
}

impl Default for EfficiencyHashrateEma {
    fn default() -> Self {
        Self::new()
    }
}

/// Power model for ASIC miners using CMOS dynamic power formula.
///
/// Total power = sum(C_eff * V^2 * f_i) + P_static
///
/// The model is deliberately simple — we calibrate C_eff from a known
/// data point rather than modeling from first principles.
#[derive(Debug, Clone)]
pub struct PowerModel {
    /// Empirical power coefficient per chip type.
    /// P_chip = c_eff * V^2 * f_mhz where V is in volts and f in MHz.
    c_eff: f64,
    /// Static power per chain in watts (board overhead, fans, etc.).
    static_per_chain_w: f64,
    /// Control board overhead in watts.
    control_board_w: f64,
    /// ASIC chip ID for PLL frequency table lookup (e.g., 0x1387 for BM1387).
    chip_id: u16,
    /// Global correction factor derived from an external wall meter.
    power_scale: f64,
}

impl PowerModel {
    /// Create a power model calibrated for BM1387 (Antminer S9).
    ///
    /// Calibration from known S9 operating point:
    ///   189 chips, 650 MHz, 9.1V → ~1350W total
    ///   Static: 3 chains × 50W + 20W = 170W
    ///   Dynamic: 1350 - 170 = 1180W
    ///   Per-chip at 650 MHz, 9.1V: 1180/189 ≈ 6.24W
    ///   C_eff = 6.24 / (9.1^2 × 650) ≈ 0.000116
    pub fn new_bm1387() -> Self {
        // Calibrate C_eff from known S9 data point:
        // P_dynamic_total = 1350 - 170 = 1180W for 189 chips
        // P_per_chip = 1180 / 189 = 6.243W
        // C_eff = P / (V^2 * f) = 6.243 / (9.1^2 * 650)
        let v = 9.1_f64;
        let f = 650.0_f64;
        let p_total = 1350.0_f64;
        let p_static = 3.0 * 50.0 + 20.0; // 170W
        let p_dynamic = p_total - p_static;
        let p_per_chip = p_dynamic / 189.0;
        let c_eff = p_per_chip / (v * v * f);

        Self {
            c_eff,
            static_per_chain_w: 50.0,
            control_board_w: 20.0,
            chip_id: 0x1387,
            power_scale: 1.0,
        }
    }

    /// Create a power model from a MinerProfile's pre-calculated C_eff.
    ///
    /// Uses the per-chip-type C_eff values from the centralized MINER_PROFILES
    /// table in dcentrald-asic. This avoids duplicating calibration constants.
    ///
    /// Supported chip IDs:
    ///   0x1387 (BM1387/S9), 0x1397 (BM1397/S17), 0x1398 (BM1398/S19),
    ///   0x1362 (BM1362/S19j Pro), 0x1366 (BM1366/S19 XP),
    ///   0x1368 (BM1368/S21), 0x1370 (BM1370/S21 Pro).
    ///
    /// Falls back to BM1387 model for unknown chip IDs.
    pub fn new_for_chip(chip_id: u16) -> Self {
        if let Some(profile) = dcentrald_asic::drivers::MinerProfile::for_chip(chip_id) {
            Self {
                c_eff: profile.c_eff,
                static_per_chain_w: profile.static_per_chain_w,
                control_board_w: profile.control_board_w,
                chip_id,
                power_scale: 1.0,
            }
        } else {
            // Unknown chip — fall back to BM1387 model
            Self::new_bm1387()
        }
    }

    /// Look up a harvested per-model J/TH efficiency anchor for this power
    /// model's chip family from the cited
    /// [`dcentrald_silicon_profiles::operating_points`] catalog.
    ///
    /// This is the additive bridge from the 2026-06-14 per-model power
    /// harvest into the autotuner. It does NOT change the `c_eff`-based
    /// estimate (`chip_power_w` / `total_power_w` are untouched) — it gives
    /// callers a *measured/vendor-extracted* efficiency reference they can
    /// use to sanity-check or anchor the modeled estimate.
    ///
    /// Cooling-aware: pass the unit's actual cooling so an air-cooled unit is
    /// never anchored on a hydro/immersion curve (and vice-versa). Returns
    /// `None` when no harvested model for this `chip_id`+`cooling` has
    /// computable power data (e.g. air T19 is a documented full GAP).
    ///
    /// The returned tuple is `(j_per_th, is_measured, source)` from the most
    /// efficient harvested operating point of the matching model:
    ///   - `j_per_th` — best-efficiency J/TH from the harvest
    ///   - `is_measured` — `true` only if that point is `Measured` (real
    ///     hardware), `false` if `Inferred` (vendor/reconstructed)
    ///   - `source` — the citation string for that point
    pub fn harvested_efficiency_anchor(
        &self,
        cooling: dcentrald_silicon_profiles::operating_points::Cooling,
    ) -> Option<(f32, bool, &'static str)> {
        let model = dcentrald_silicon_profiles::operating_points::measured_anchor_for(
            self.chip_id,
            cooling,
        )?;
        let point = model.best_efficiency_point()?;
        Some((
            point.computed_j_per_th()?,
            point.confidence.is_measured(),
            point.source,
        ))
    }

    /// Create a power model with custom parameters.
    pub fn new(c_eff: f64, static_per_chain_w: f64, control_board_w: f64) -> Self {
        Self {
            c_eff,
            static_per_chain_w,
            control_board_w,
            chip_id: 0x1387,
            power_scale: 1.0,
        }
    }

    /// Apply a persisted wall-meter correction multiplier.
    pub fn with_power_scale(mut self, power_scale: f64) -> Self {
        self.power_scale = if power_scale.is_finite() {
            power_scale.clamp(0.5, 1.5)
        } else {
            1.0
        };
        self
    }

    /// Override the model's dynamic power coefficient with a saved runtime calibration.
    pub fn with_c_eff(mut self, c_eff: f64) -> Self {
        if c_eff.is_finite() && c_eff > 0.0 {
            self.c_eff = c_eff;
        }
        self
    }

    /// Get the empirical power coefficient.
    pub fn c_eff(&self) -> f64 {
        self.c_eff
    }

    /// Get the static power per chain in watts (board overhead, fans, etc.).
    pub fn static_per_chain_w(&self) -> f64 {
        self.static_per_chain_w * self.power_scale
    }

    /// Get the control board overhead in watts.
    pub fn control_board_w(&self) -> f64 {
        self.control_board_w * self.power_scale
    }

    /// Get the chip family this power model is calibrated for.
    pub fn chip_id(&self) -> u16 {
        self.chip_id
    }

    pub fn power_scale(&self) -> f64 {
        self.power_scale
    }

    fn uncalibrated_budget_derate(&self) -> f64 {
        match self.chip_id {
            // S9 has the most mature local validation and live telemetry model.
            0x1387 => 0.85,
            // 17/19-series constants are stock-spec anchored but not yet
            // wall-meter closed-loop validated across model variants.
            0x1397 | 0x1398 | 0x1362 | 0x1366 => 0.80,
            // NoPic S21/T21-class targets are high power and currently rely on
            // fixed-voltage estimates until measured PSU/wall telemetry is wired.
            0x1368 | 0x1370 => 0.75,
            _ => 0.75,
        }
    }

    fn bm1387_leakage_factor(board_temp_c: f32) -> f64 {
        let effective_temp_c = if board_temp_c > 0.0 {
            board_temp_c
        } else {
            BM1387_LEAKAGE_REF_TEMP_C
        };

        (1.0 + 0.02 * (effective_temp_c - BM1387_LEAKAGE_REF_TEMP_C) as f64).clamp(0.75, 2.5)
    }

    fn bm1387_fan_watts(fan_pwm: u8, fan_rpm: u32) -> f64 {
        let speed_ratio = if fan_rpm > 0 {
            (fan_rpm as f64 / BM1387_FAN_MAX_RPM).clamp(0.0, 1.0)
        } else if fan_pwm > 0 {
            let fan_pwm = fan_pwm.min(BM1387_FAN_PWM_MAX);
            (fan_pwm as f64 / 100.0).clamp(0.0, 1.0)
        } else {
            // S9 fans never truly stop while mining; if telemetry is missing, assume a
            // quiet-but-spinning baseline instead of zeroing fan power entirely.
            0.0
        };

        BM1387_FAN_BASE_W + BM1387_FAN_DYNAMIC_MAX_W * speed_ratio.powi(3)
    }

    /// Estimate dynamic power for a single chip at given voltage and frequency.
    ///
    /// P_chip = C_eff × V² × f
    ///
    /// `voltage_v`: voltage in volts (e.g., 9.1)
    /// `freq_mhz`: frequency in MHz (e.g., 650)
    pub fn chip_power_w(&self, voltage_v: f64, freq_mhz: u16) -> f64 {
        self.c_eff * voltage_v * voltage_v * freq_mhz as f64 * self.power_scale
    }

    /// Estimate total power for a set of chains with per-chip frequencies.
    ///
    /// `chains`: slice of (voltage_mv, per_chip_frequencies) tuples.
    /// Returns total estimated power in watts including static overhead.
    pub fn total_power_w(&self, chains: &[(u16, &[u16])]) -> f64 {
        let mut total = self.control_board_w();

        for &(voltage_mv, freqs) in chains {
            let voltage_v = voltage_mv as f64 / 1000.0;
            let chain_dynamic: f64 = freqs.iter().map(|&f| self.chip_power_w(voltage_v, f)).sum();
            total += chain_dynamic + self.static_per_chain_w();
        }

        total
    }

    /// Estimate one chain's board power while assigning only this chain's
    /// proportional share of the control-board overhead.
    ///
    /// `total_power_w()` intentionally adds the control board once for a whole
    /// miner. Per-profile chain estimates need a split share instead; otherwise
    /// summing three saved chain profiles overstates board power by two full
    /// control-board loads.
    pub fn chain_power_w(&self, voltage_mv: u16, freqs: &[u16], active_chains: u8) -> f64 {
        let voltage_v = voltage_mv as f64 / 1000.0;
        let chain_dynamic: f64 = freqs.iter().map(|&f| self.chip_power_w(voltage_v, f)).sum();
        let control_share = if active_chains > 0 {
            self.control_board_w() / active_chains as f64
        } else {
            0.0
        };

        chain_dynamic + self.static_per_chain_w() + control_share
    }

    /// Estimate live power from actual operating parameters.
    ///
    /// Called every 5 seconds by the work dispatcher with real-time voltage
    /// and per-chip frequency data. Unlike the static `total_power_w()` which
    /// uses config values, this reflects the ACTUAL state after autotuner
    /// frequency changes, thermal throttling, and voltage drift.
    ///
    /// `chains`: per-chain tuple of (voltage_v, per_chip_freq_mhz_vec).
    /// `hashrate_ths`: current hashrate in TH/s for efficiency calculation.
    /// `psu_efficiency`: PSU efficiency factor (0.88 for 120V, 0.93 for 240V).
    pub fn estimate_live(
        &self,
        chains: &[(f64, Vec<u16>)],
        hashrate_ths: f64,
        psu_efficiency: f64,
    ) -> LivePowerEstimate {
        let mut board_watts = self.control_board_w();
        let mut per_chain_watts = Vec::with_capacity(chains.len());

        for (voltage_v, chip_freqs) in chains {
            let chain_dynamic: f64 = chip_freqs
                .iter()
                .map(|&f| self.chip_power_w(*voltage_v, f))
                .sum();
            let chain_total = chain_dynamic + self.static_per_chain_w();
            per_chain_watts.push(chain_total);
            board_watts += chain_total;
        }

        let eff = if psu_efficiency > 0.0 {
            psu_efficiency
        } else {
            0.93
        };
        let wall_watts = board_watts / eff;
        let btu_h = btu_from_watts(wall_watts);
        // P1-3 (D-7): per-call (instantaneous) efficiency from the supplied
        // hashrate. The dispatcher recomputes this against an EMA-smoothed
        // denominator before publishing; it is flagged low-confidence here
        // because a single 5 s sample is not yet a settled reading.
        let efficiency_jth = efficiency_jth_from(wall_watts, hashrate_ths);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        LivePowerEstimate {
            board_watts,
            wall_watts,
            per_chain_watts,
            efficiency_jth,
            efficiency_jth_low_confidence: true,
            btu_h,
            calibrated: (self.power_scale - 1.0).abs() > 0.001,
            calibration_multiplier: ((self.power_scale - 1.0).abs() > 0.001)
                .then_some(self.power_scale),
            source: "estimated".to_string(),
            dispatcher_limits: Vec::new(),
            watt_cap: None,
            timestamp_ms,
        }
    }

    /// Estimate live runtime power using extra telemetry when available.
    ///
    /// On S9/BM1387 there is no direct PSU telemetry, so we improve the estimate with:
    /// - board-temperature leakage adjustment
    /// - live fan PWM/RPM power
    ///
    /// Other chip families currently fall back to the standard live estimate until they
    /// grow the same runtime-specific calibration data.
    pub fn estimate_live_with_telemetry(
        &self,
        chains: &[(f64, Vec<u16>)],
        chain_temps_c: &[f32],
        fan_pwm: u8,
        fan_rpm: u32,
        hashrate_ths: f64,
        psu_efficiency: f64,
    ) -> LivePowerEstimate {
        if self.chip_id != 0x1387 {
            return self.estimate_live(chains, hashrate_ths, psu_efficiency);
        }

        let fan_watts = Self::bm1387_fan_watts(fan_pwm, fan_rpm) * self.power_scale;
        let mut board_watts = BM1387_RUNTIME_CONTROL_BOARD_W * self.power_scale + fan_watts;
        let mut per_chain_watts = Vec::with_capacity(chains.len());

        for (idx, (voltage_v, chip_freqs)) in chains.iter().enumerate() {
            let voltage_ratio = if *voltage_v > 0.0 {
                *voltage_v / 9.1
            } else {
                1.0
            };
            let leakage_factor = Self::bm1387_leakage_factor(
                chain_temps_c
                    .get(idx)
                    .copied()
                    .unwrap_or(BM1387_LEAKAGE_REF_TEMP_C),
            );

            let chain_asic_watts: f64 = chip_freqs
                .iter()
                .map(|&freq_mhz| {
                    let nominal_chip_total = self.chip_power_w(*voltage_v, freq_mhz);
                    let nominal_leakage =
                        BM1387_BASE_LEAKAGE_PER_CHIP_W * voltage_ratio * self.power_scale;
                    let dynamic_watts = (nominal_chip_total - nominal_leakage).max(0.0);
                    let adjusted_leakage = nominal_leakage * leakage_factor;
                    dynamic_watts + adjusted_leakage
                })
                .sum();

            let chain_total =
                chain_asic_watts + BM1387_RUNTIME_STATIC_PER_CHAIN_W * self.power_scale;
            per_chain_watts.push(chain_total);
            board_watts += chain_total;
        }

        let eff = if psu_efficiency > 0.0 {
            psu_efficiency
        } else {
            0.93
        };
        let wall_watts = board_watts / eff;
        let btu_h = btu_from_watts(wall_watts);
        // P1-3 (D-7): per-call (instantaneous) efficiency from the supplied
        // hashrate. The dispatcher recomputes this against an EMA-smoothed
        // denominator before publishing; it is flagged low-confidence here
        // because a single 5 s sample is not yet a settled reading.
        let efficiency_jth = efficiency_jth_from(wall_watts, hashrate_ths);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        LivePowerEstimate {
            board_watts,
            wall_watts,
            per_chain_watts,
            efficiency_jth,
            efficiency_jth_low_confidence: true,
            btu_h,
            calibrated: (self.power_scale - 1.0).abs() > 0.001,
            calibration_multiplier: ((self.power_scale - 1.0).abs() > 0.001)
                .then_some(self.power_scale),
            source: "estimated".to_string(),
            dispatcher_limits: Vec::new(),
            watt_cap: None,
            timestamp_ms,
        }
    }

    /// Calibrate the power model from an actual power measurement.
    ///
    /// Adjusts C_eff so the model matches the measured total power, accounting for
    /// the actual chip frequencies, voltage, and number of chains.
    ///
    /// This corrects for per-miner hardware variation (±10%) in the theoretical model.
    pub fn calibrate(
        &mut self,
        measured_w: f64,
        chip_freqs: &[(u16, u16)], // (chip_freq_mhz, count_at_this_freq)
        voltage_mv: u16,
        num_chains: u8,
    ) {
        let voltage_v = voltage_mv as f64 / 1000.0;
        let v_squared = voltage_v * voltage_v;

        // Subtract static overhead
        let static_overhead =
            self.static_per_chain_w() * num_chains as f64 + self.control_board_w();
        let measured_dynamic = measured_w - static_overhead;

        if measured_dynamic <= 0.0 {
            tracing::warn!(
                measured_w,
                static_overhead,
                "Measured power ({:.0}W) doesn't cover static overhead ({:.0}W) — skipping calibration",
                measured_w, static_overhead,
            );
            return;
        }

        // Compute total frequency sum across all chips
        let total_freq_sum: f64 = chip_freqs
            .iter()
            .map(|&(freq, count)| freq as f64 * count as f64)
            .sum();

        if total_freq_sum <= 0.0 {
            return;
        }

        // Solve: measured_dynamic = c_eff_new * V^2 * total_freq_sum
        let old_c_eff = self.c_eff;
        let new_c_eff = measured_dynamic / (v_squared * total_freq_sum * self.power_scale);
        let adjustment_pct = ((new_c_eff / old_c_eff) - 1.0) * 100.0;

        self.c_eff = new_c_eff;

        tracing::info!(
            old_c_eff = format_args!("{:.6e}", old_c_eff),
            new_c_eff = format_args!("{:.6e}", new_c_eff),
            adjustment_pct = format_args!("{:+.1}%", adjustment_pct),
            measured_w = format_args!("{:.0}", measured_w),
            "Power model calibrated: C_eff adjusted {:+.1}% from reference",
            adjustment_pct,
        );
    }

    /// Calibrate C_eff from a whole-miner power reading with per-chain voltage.
    ///
    /// This is the preferred path for PMBus calibration because voltage may
    /// differ by chain after runtime undervolting. Solving against a single
    /// averaged voltage biases C_eff whenever the rails are not equal.
    pub fn calibrate_chains(&mut self, measured_w: f64, chains: &[(u16, &[u16])]) {
        if chains.is_empty() {
            return;
        }

        let static_overhead =
            self.static_per_chain_w() * chains.len() as f64 + self.control_board_w();
        let measured_dynamic = measured_w - static_overhead;

        if measured_dynamic <= 0.0 {
            tracing::warn!(
                measured_w,
                static_overhead,
                "Measured power ({:.0}W) doesn't cover static overhead ({:.0}W) — skipping chain-aware calibration",
                measured_w,
                static_overhead,
            );
            return;
        }

        let denominator: f64 = chains
            .iter()
            .map(|(voltage_mv, freqs)| {
                let voltage_v = *voltage_mv as f64 / 1000.0;
                let freq_sum: f64 = freqs.iter().map(|&freq| freq as f64).sum();
                self.power_scale * voltage_v * voltage_v * freq_sum
            })
            .sum();

        if denominator <= 0.0 {
            return;
        }

        let old_c_eff = self.c_eff;
        let new_c_eff = measured_dynamic / denominator;
        let adjustment_pct = ((new_c_eff / old_c_eff) - 1.0) * 100.0;

        self.c_eff = new_c_eff;

        tracing::info!(
            old_c_eff = format_args!("{:.6e}", old_c_eff),
            new_c_eff = format_args!("{:.6e}", new_c_eff),
            adjustment_pct = format_args!("{:+.1}%", adjustment_pct),
            measured_w = format_args!("{:.0}", measured_w),
            "Power model calibrated from per-chain voltage state: C_eff adjusted {:+.1}% from reference",
            adjustment_pct,
        );
    }

    /// Given a power budget, compute per-chip frequencies using proportional allocation.
    ///
    /// Grade A chips get more budget, Grade D chips get minimum.
    /// The algorithm:
    ///   1. Subtract static overhead from budget → available dynamic watts
    ///   2. Compute weight per chip based on grade: A=1.2, B=1.0, C=0.8, D=0.5
    ///   3. Per-chip budget = available × (weight_i / sum_weights)
    ///   4. Per-chip frequency = budget_i / (C_eff × V²) — solving P = C × V² × f
    ///   5. Clamp each frequency to [min_freq_mhz, chip.max_stable_mhz]
    ///   6. If clamping freed up budget, redistribute to unclamped chips (iterate 2-3x)
    ///
    /// Returns a Vec of target frequencies (one per chip, same order as chip_profiles).
    ///
    /// If `calibrated_c_eff` is `None` (no PSU calibration yet), the effective
    /// budget is derated by 15% to compensate for ±10% model error. This
    /// prevents overshoot on uncalibrated miners.
    pub fn allocate_budget(
        &self,
        budget_w: f64,
        voltage_v: f64,
        chip_profiles: &[ChipProfile],
        min_freq_mhz: u16,
        num_chains: u8,
    ) -> Vec<u16> {
        if chip_profiles.is_empty() {
            return Vec::new();
        }

        // Step 1: Subtract static overhead from budget
        let static_overhead =
            self.static_per_chain_w() * num_chains as f64 + self.control_board_w();
        let available_dynamic_w = (budget_w - static_overhead).max(0.0);

        if available_dynamic_w <= 0.0 {
            // Budget doesn't even cover static overhead — run all at minimum
            return vec![min_freq_mhz; chip_profiles.len()];
        }

        let mut result = vec![0u16; chip_profiles.len()];
        let mut clamped = vec![false; chip_profiles.len()];

        // Water-fill allocation. Once a weak chip clamps, keep its actual
        // power fixed and reallocate the remaining original dynamic budget
        // across the still-unclamped chips.
        let mut remaining_budget = available_dynamic_w;

        for _iteration in 0..chip_profiles.len().min(8) {
            // Step 2: Compute weights for unclamped chips
            let weights: Vec<f64> = chip_profiles
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if clamped[i] {
                        0.0
                    } else {
                        grade_weight(p.grade)
                    }
                })
                .collect();

            let sum_weights: f64 = weights.iter().sum();
            if sum_weights <= 0.0 {
                break;
            }

            let mut any_newly_clamped = false;

            for (i, profile) in chip_profiles.iter().enumerate() {
                if clamped[i] {
                    continue;
                }

                // Step 3: Per-chip budget proportional to weight
                let chip_budget = remaining_budget * (weights[i] / sum_weights);

                // Step 4: Solve P = C_eff × V² × f for f
                // f = P / (C_eff × V²)
                let raw_freq =
                    chip_budget / (self.c_eff * self.power_scale * voltage_v * voltage_v);
                let raw_freq_mhz = raw_freq as u16;

                // Step 5: Clamp to [min_freq_mhz, max_stable_mhz]
                let max_freq = if profile.max_stable_mhz > 0 {
                    profile.max_stable_mhz
                } else {
                    min_freq_mhz
                };

                // Guard against an INVERTED clamp range. `min_freq_mhz` (the
                // autotuner config floor, ~400 on am2) and `max_freq` (the
                // chip's runtime/persisted `max_stable_mhz`) come from
                // independent sources, and a degraded or weakly-binned chip can
                // have `max_stable_mhz` below the floor (it is shrunk at runtime
                // via `max_stable_mhz.min(measured_freq)` and can be loaded from
                // persisted/imported state). `u16::clamp(lo, hi)` PANICS when
                // lo > hi, and the workspace is built `panic = "abort"` → that
                // would abort dcentrald and crash-loop on restart. This is the
                // exact class of the prior solar.rs crash (687bd2ed). When the
                // chip's stable ceiling is below the floor the ceiling MUST win
                // — never overclock a weak chip past its measured stable point
                // to honor a floor. Result stays <= max_freq either way.
                let lo = min_freq_mhz.min(max_freq);
                let clamped_freq = raw_freq_mhz.clamp(lo, max_freq);
                result[i] = clamped_freq;

                if raw_freq_mhz < min_freq_mhz || raw_freq_mhz > max_freq {
                    clamped[i] = true;
                    any_newly_clamped = true;
                }
            }

            if !any_newly_clamped {
                break;
            }

            let clamped_power: f64 = result
                .iter()
                .enumerate()
                .filter(|(idx, _)| clamped[*idx])
                .map(|(_, &freq)| self.chip_power_w(voltage_v, freq))
                .sum();
            remaining_budget = (available_dynamic_w - clamped_power).max(0.0);
        }

        // Snap each frequency to the nearest PLL entry that doesn't exceed it
        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(self.chip_id);
        for (i, freq) in result.iter_mut().enumerate() {
            *freq = pll
                .iter()
                .rev()
                .find(|&&f| f <= *freq)
                .copied()
                .unwrap_or(min_freq_mhz);
            // Ensure we don't go below the minimum — but the floor must never
            // exceed the chip's stable ceiling. A weak/degraded chip whose
            // max_stable_mhz is below the config floor must NOT be overclocked
            // past its measured stable point to honor the floor. Without the
            // `.min(ceiling)` cap this floor re-application would re-introduce
            // the exact overclock the inverted-range clamp above prevents.
            let ceiling = if chip_profiles[i].max_stable_mhz > 0 {
                chip_profiles[i].max_stable_mhz
            } else {
                min_freq_mhz
            };
            let floor = min_freq_mhz.min(ceiling);
            if *freq < floor {
                *freq = floor;
            }
        }

        // Post-snap power overshoot check: PLL quantization can push total power
        // above the budget. If so, iteratively reduce the lowest-grade chip's
        // frequency by one PLL step until we're back within budget.
        let v_squared = voltage_v * voltage_v;
        let actual_dynamic: f64 = result
            .iter()
            .map(|&f| self.c_eff * self.power_scale * v_squared * f as f64)
            .sum();
        if actual_dynamic > available_dynamic_w * 1.001 {
            // Sort chip indices by grade (worst first) for preferential reduction
            let mut grade_order: Vec<usize> = (0..chip_profiles.len()).collect();
            grade_order.sort_by(|&a, &b| {
                let wa = grade_weight(chip_profiles[a].grade);
                let wb = grade_weight(chip_profiles[b].grade);
                wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
            });

            let mut remaining_excess = actual_dynamic - available_dynamic_w;
            for &idx in &grade_order {
                if remaining_excess <= 0.0 {
                    break;
                }
                // Step down one PLL entry
                if let Some(&lower) = pll.iter().rev().find(|&&f| f < result[idx]) {
                    let saved = self.c_eff
                        * self.power_scale
                        * v_squared
                        * (result[idx] as f64 - lower as f64);
                    result[idx] = lower;
                    remaining_excess -= saved;
                }
            }
        }

        result
    }

    /// Like `allocate_budget()` but applies uncalibrated derating when no
    /// PSU calibration data is available.
    ///
    /// When neither a persisted runtime `C_eff` calibration nor an active
    /// wall-meter multiplier is present, the effective budget is multiplied by
    /// a family-specific safety factor. This prevents PSU overload on first
    /// boot before the saved calibration state has been anchored to real
    /// hardware.
    pub fn allocate_budget_safe(
        &self,
        budget_w: f64,
        voltage_v: f64,
        chip_profiles: &[ChipProfile],
        min_freq_mhz: u16,
        num_chains: u8,
        calibrated_c_eff: Option<f64>,
    ) -> Vec<u16> {
        let has_saved_calibration =
            calibrated_c_eff.is_some() || (self.power_scale - 1.0).abs() > 0.001;
        let effective_budget = if !has_saved_calibration {
            let derate = self.uncalibrated_budget_derate();
            let derated = budget_w * derate;
            tracing::debug!(
                raw_budget = format_args!("{:.0}", budget_w),
                derated_budget = format_args!("{:.0}", derated),
                "Uncalibrated power model: derating budget by family safety factor ({:.0}W -> {:.0}W)",
                budget_w,
                derated,
            );
            derated
        } else {
            budget_w
        };

        self.allocate_budget(
            effective_budget,
            voltage_v,
            chip_profiles,
            min_freq_mhz,
            num_chains,
        )
    }

    /// Redistribute freed power from a backed-off chip to strong neighbors.
    ///
    /// When a weak chip is backed off, the difference between its old and new
    /// power consumption becomes available for redistribution. This budget is
    /// allocated to the remaining chips proportionally by grade weight,
    /// boosting strong chips while respecting their `max_stable_mhz` ceiling.
    ///
    /// Returns a Vec of (chip_index, new_freq_mhz) for chips that should be boosted.
    #[allow(clippy::too_many_arguments)]
    pub fn redistribute_freed_power(
        &self,
        backed_off_chip: u8,
        old_freq_mhz: u16,
        new_freq_mhz: u16,
        voltage_v: f64,
        chip_profiles: &[ChipProfile],
        current_freqs: &[(u8, u16)], // (chip_index, current_freq)
        min_freq_mhz: u16,
    ) -> Vec<(u8, u16)> {
        // Compute freed power
        let old_power = self.chip_power_w(voltage_v, old_freq_mhz);
        let new_power = self.chip_power_w(voltage_v, new_freq_mhz);
        let freed_w = old_power - new_power;

        if freed_w <= 0.01 {
            return Vec::new();
        }

        let pll = dcentrald_asic::drivers::MinerProfile::pll_frequencies_for_chip(self.chip_id);
        let v_squared = voltage_v * voltage_v;

        // Build list of eligible chips (not the backed-off one, not at max stable)
        let mut eligible: Vec<(u8, f64, u16, u16)> = Vec::new(); // (idx, weight, current_freq, max_stable)
        for profile in chip_profiles {
            if profile.chip_index == backed_off_chip {
                continue;
            }

            let current = current_freqs
                .iter()
                .find(|&&(idx, _)| idx == profile.chip_index)
                .map(|&(_, f)| f)
                .unwrap_or(profile.operating_mhz);

            // Skip chips already at or near max stable.
            // Reserve 5% thermal headroom — chips boosted to 100% of max_stable
            // have zero margin for temperature increases. The dead chip cascade
            // scenario: boost neighbor to max → temp rises → neighbor errors → cascade.
            let headroom_max = (profile.max_stable_mhz as f64 * 0.95) as u16;
            if current >= headroom_max {
                continue;
            }

            let weight = grade_weight(profile.grade);
            eligible.push((profile.chip_index, weight, current, headroom_max));
        }

        if eligible.is_empty() {
            return Vec::new();
        }

        let total_weight: f64 = eligible.iter().map(|e| e.1).sum();
        let mut result = Vec::new();
        let mut remaining_budget = freed_w;

        for &(chip_idx, weight, current_freq, max_stable) in &eligible {
            if remaining_budget <= 0.01 {
                break;
            }

            // Share of freed budget proportional to grade weight
            let chip_budget = freed_w * (weight / total_weight);
            // Additional power this chip can absorb
            let additional_freq = chip_budget / (self.c_eff * self.power_scale * v_squared);
            let target_freq = current_freq as f64 + additional_freq;
            let target_mhz = (target_freq as u16).min(max_stable);

            // Snap to PLL
            let new_freq = pll
                .iter()
                .rev()
                .find(|&&f| f <= target_mhz)
                .copied()
                .unwrap_or(min_freq_mhz);

            if new_freq > current_freq {
                let actual_added_power = self.chip_power_w(voltage_v, new_freq)
                    - self.chip_power_w(voltage_v, current_freq);
                remaining_budget -= actual_added_power;
                result.push((chip_idx, new_freq));
            }
        }

        result
    }

    /// Compute the power budget needed to achieve a target hashrate.
    ///
    /// Inverse of `allocate_budget()`: given a desired hashrate in TH/s,
    /// compute the required average frequency and the corresponding power budget.
    ///
    /// BM1387: hashrate_per_chip_ghs = freq_mhz × 0.114
    /// target_ths = sum(freq_i × 0.114) / 1000
    ///
    /// Returns the synthetic watts budget that can be passed to `allocate_budget()`.
    pub fn budget_for_hashrate(
        &self,
        target_ths: f64,
        voltage_v: f64,
        chip_count: usize,
        num_chains: u8,
    ) -> f64 {
        if chip_count == 0 || target_ths <= 0.0 {
            return 0.0;
        }

        let ghs_per_mhz = crate::chip_geometry::ghs_per_mhz_for_chip(self.chip_id());

        // target_ths * 1000 = total GH/s
        // total GH/s / ghs_per_mhz / chip_count = avg_freq_mhz needed
        let avg_freq_mhz = (target_ths * 1000.0) / (ghs_per_mhz * chip_count as f64);

        // P_total = chip_count * C_eff * V^2 * avg_freq + static_overhead
        let dynamic_per_chip = self.c_eff * self.power_scale * voltage_v * voltage_v * avg_freq_mhz;
        let total_dynamic = dynamic_per_chip * chip_count as f64;
        let static_overhead =
            self.static_per_chain_w() * num_chains as f64 + self.control_board_w();

        total_dynamic + static_overhead
    }
}

/// Get the allocation weight for a chip grade.
///
/// Higher grades get proportionally more power budget.
pub(crate) fn grade_weight(grade: ChipGrade) -> f64 {
    match grade {
        ChipGrade::A => 1.2,
        ChipGrade::B => 1.0,
        ChipGrade::C => 0.8,
        ChipGrade::D => 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_silicon_profiles::operating_points::Cooling;

    #[test]
    fn harvested_efficiency_anchor_returns_measured_for_s9() {
        // BM1387/S9 has a measured live anchor; the air-cooled anchor must be
        // measured and land at the BM1387 sweet-spot J/TH (~91 W/TH at the
        // 450 MHz step).
        let m = PowerModel::new_for_chip(0x1387);
        let (j, measured, src) = m
            .harvested_efficiency_anchor(Cooling::Air)
            .expect("S9 has a harvested air anchor");
        assert!(measured, "S9 anchor should be Measured");
        assert!(!src.is_empty());
        assert!(
            (85.0..=100.0).contains(&j),
            "S9 best-eff J/TH {} outside expected band",
            j
        );
    }

    #[test]
    fn harvested_efficiency_anchor_is_cooling_aware() {
        // BM1370/S21 Pro air has no live unit (inferred only); the air anchor
        // must never come back as a hydro/immersion curve. And a hydro
        // request for the same chip family must return a (much lower) hydro
        // J/TH, proving the two are not conflated.
        let m = PowerModel::new_for_chip(0x1370);
        let air = m.harvested_efficiency_anchor(Cooling::Air);
        let hydro = m.harvested_efficiency_anchor(Cooling::Hydro);
        if let (Some((j_air, _, _)), Some((j_hydro, _, _))) = (air, hydro) {
            // Hydro/immersion BM1370 (S21+ Hydro ULP 11.6 J/TH) is far more
            // efficient than the air S21 Pro reconstructed curve (~14 J/TH).
            assert!(
                j_hydro < j_air,
                "hydro J/TH {} should beat air J/TH {} for BM1370",
                j_hydro,
                j_air
            );
        }
    }

    #[test]
    fn harvested_efficiency_anchor_none_for_unknown_chip() {
        let m = PowerModel::new_for_chip(0x1387);
        // A chip family with no harvested model at all returns None.
        let weird = PowerModel {
            chip_id: 0xDEAD,
            ..m
        };
        assert!(weird.harvested_efficiency_anchor(Cooling::Air).is_none());
    }

    #[test]
    fn efficiency_jth_from_guards_zero_and_nan_denominator() {
        // P1-3 (D-7): a usable positive hashrate divides normally...
        assert!((efficiency_jth_from(3000.0, 100.0) - 30.0).abs() < f64::EPSILON);
        // ...and a zero / negative / non-finite hashrate returns the 0.0 sentinel
        // instead of an infinity.
        assert_eq!(efficiency_jth_from(3000.0, 0.0), 0.0);
        assert_eq!(efficiency_jth_from(3000.0, -5.0), 0.0);
        assert_eq!(efficiency_jth_from(3000.0, f64::NAN), 0.0);
    }

    #[test]
    fn efficiency_ema_smooths_a_swinging_hashrate() {
        // P1-3 (D-7): a hashrate swinging 0..24 TH/s around a ~12 TH/s mean must
        // not make the smoothed denominator (and thus J/TH) swing wildly. The EMA
        // output stays bounded well inside the raw swing.
        let mut ema = EfficiencyHashrateEma::new();
        let raw = [12.0, 24.0, 0.0, 18.0, 6.0, 24.0, 0.0, 12.0, 20.0, 4.0];
        let mut last = 0.0;
        for &h in &raw {
            let (smoothed, _) = ema.update(h);
            last = smoothed;
        }
        // The smoothed value lands near the mean, never pinned to a 0 or 24 spike.
        assert!(
            last > 5.0 && last < 20.0,
            "smoothed hashrate {} escaped the expected band",
            last
        );
        // A constant power over the smoothed denominator yields a sane J/TH,
        // whereas dividing by a raw 0.0 tick would have been infinite.
        let jth = efficiency_jth_from(360.0, last);
        assert!(jth.is_finite() && jth > 0.0);
    }

    #[test]
    fn efficiency_ema_reports_low_confidence_until_warm() {
        let mut ema = EfficiencyHashrateEma::new();
        // Below MIN_CONFIDENT_SAMPLES the reading is low-confidence.
        for _ in 0..(EfficiencyHashrateEma::MIN_CONFIDENT_SAMPLES - 1) {
            let (_, confident) = ema.update(12.0);
            assert!(!confident);
        }
        // The MIN_CONFIDENT_SAMPLES-th valid sample flips it confident.
        let (_, confident) = ema.update(12.0);
        assert!(confident);
        assert!(ema.confident());
    }

    #[test]
    fn efficiency_ema_ignores_non_finite_and_negative_samples() {
        let mut ema = EfficiencyHashrateEma::new();
        ema.update(10.0);
        let (smoothed_before, _) = ema.update(10.0);
        // Garbage inputs do not move the smoothed value or count as samples.
        let (after_nan, _) = ema.update(f64::NAN);
        let (after_neg, _) = ema.update(-3.0);
        assert_eq!(smoothed_before, after_nan);
        assert_eq!(smoothed_before, after_neg);
    }

    #[test]
    fn live_estimate_flags_efficiency_low_confidence() {
        // The per-call estimate is inherently single-sample → low-confidence.
        let model = PowerModel::new_bm1387();
        let est = model.estimate_live(&[(9.1, vec![650u16; 63])], 13.5, 0.93);
        assert!(est.efficiency_jth_low_confidence);
        assert!(est.efficiency_jth > 0.0);
    }

    #[test]
    fn test_bm1387_calibration() {
        let model = PowerModel::new_bm1387();

        // Verify calibration: 189 chips at 650 MHz, 9.1V should give ~1180W dynamic
        let per_chip = model.chip_power_w(9.1, 650);
        let total_dynamic = per_chip * 189.0;

        // Should be close to 1180W (calibration target)
        assert!(
            (total_dynamic - 1180.0).abs() < 1.0,
            "Expected ~1180W dynamic, got {:.1}W",
            total_dynamic
        );
    }

    #[test]
    fn test_chip_power_scales_with_freq() {
        let model = PowerModel::new_bm1387();

        let p_low = model.chip_power_w(9.1, 400);
        let p_high = model.chip_power_w(9.1, 800);

        // Power should scale linearly with frequency
        assert!(
            (p_high / p_low - 2.0).abs() < 0.01,
            "Power should double when frequency doubles: {:.3}W vs {:.3}W",
            p_low,
            p_high
        );
    }

    #[test]
    fn test_chip_power_scales_with_voltage_squared() {
        let model = PowerModel::new_bm1387();

        let p_low = model.chip_power_w(8.0, 650);
        let p_high = model.chip_power_w(9.0, 650);

        // Power should scale with V^2
        let expected_ratio = (9.0 * 9.0) / (8.0 * 8.0);
        let actual_ratio = p_high / p_low;

        assert!(
            (actual_ratio - expected_ratio).abs() < 0.01,
            "Power should scale with V^2: expected ratio {:.4}, got {:.4}",
            expected_ratio,
            actual_ratio
        );
    }

    #[test]
    fn test_total_power_includes_static() {
        let model = PowerModel::new_bm1387();

        // Single chain, 3 chips at 650 MHz, 9100mV
        let freqs = vec![650u16; 3];
        let chains: Vec<(u16, &[u16])> = vec![(9100, &freqs)];
        let total = model.total_power_w(&chains);

        let expected_dynamic: f64 = (0..3).map(|_| model.chip_power_w(9.1, 650)).sum();
        let expected = expected_dynamic + 50.0 + 20.0; // static_per_chain + control_board

        assert!(
            (total - expected).abs() < 0.1,
            "Expected {:.1}W, got {:.1}W",
            expected,
            total
        );
    }

    #[test]
    fn test_total_power_s9_reference() {
        let model = PowerModel::new_bm1387();

        // Full S9: 3 chains × 63 chips = 189 chips at 650 MHz, 9100mV
        let freqs = vec![650u16; 63];
        let chains: Vec<(u16, &[u16])> = vec![(9100, &freqs), (9100, &freqs), (9100, &freqs)];
        let total = model.total_power_w(&chains);

        // Should be close to 1350W (the calibration reference point)
        assert!(
            (total - 1350.0).abs() < 5.0,
            "Expected ~1350W for full S9, got {:.1}W",
            total
        );
    }

    #[test]
    fn test_chain_power_splits_control_board_once() {
        let model = PowerModel::new_bm1387();
        let freqs = vec![650u16; 63];
        let chains: Vec<(u16, &[u16])> = vec![(9100, &freqs), (9100, &freqs), (9100, &freqs)];
        let total = model.total_power_w(&chains);

        let split_total: f64 = (0..3).map(|_| model.chain_power_w(9100, &freqs, 3)).sum();

        assert!(
            (split_total - total).abs() < 0.1,
            "Split chain estimates ({:.1}W) should sum to total ({:.1}W)",
            split_total,
            total
        );
    }

    #[test]
    fn test_calibrate_chains_handles_different_voltages() {
        let reference = PowerModel::new_bm1387();
        let freqs_a = vec![600u16; 63];
        let freqs_b = vec![650u16; 63];
        let freqs_c = vec![700u16; 63];
        let chains: Vec<(u16, &[u16])> = vec![(8900, &freqs_a), (9100, &freqs_b), (9300, &freqs_c)];
        let measured_w = reference.total_power_w(&chains);

        let mut calibrated = PowerModel::new_bm1387().with_c_eff(reference.c_eff() * 0.8);
        calibrated.calibrate_chains(measured_w, &chains);

        assert!(
            (calibrated.c_eff() / reference.c_eff() - 1.0).abs() < 0.001,
            "Chain-aware calibration should recover reference C_eff"
        );
    }

    #[test]
    fn test_power_scale_scales_total_power() {
        let model = PowerModel::new_bm1387();
        let scaled = PowerModel::new_bm1387().with_power_scale(1.1);
        let freqs = vec![650u16; 63];
        let chains: Vec<(u16, &[u16])> = vec![(9100, &freqs), (9100, &freqs), (9100, &freqs)];

        let base_total = model.total_power_w(&chains);
        let scaled_total = scaled.total_power_w(&chains);

        assert!(
            (scaled_total / base_total - 1.1).abs() < 0.01,
            "Scaled power {:.1}W should be ~10% above base {:.1}W",
            scaled_total,
            base_total
        );
    }

    #[test]
    fn test_bm1387_runtime_estimate_matches_quiet_baseline() {
        let model = PowerModel::new_bm1387();
        let chains = vec![(9.1, vec![500u16; 63]); 3];
        let temps = vec![55.0_f32, 55.0_f32, 55.0_f32];

        let live = model.estimate_live_with_telemetry(&chains, &temps, 10, 1260, 10.8, 0.88);

        assert!(
            (live.board_watts - 1078.0).abs() < 15.0,
            "Expected quiet S9 board power near 1078W, got {:.1}W",
            live.board_watts
        );
        assert!(
            (live.wall_watts - 1225.0).abs() < 20.0,
            "Expected quiet S9 wall power near 1225W, got {:.1}W",
            live.wall_watts
        );
    }

    #[test]
    fn test_bm1387_runtime_estimate_increases_with_temperature() {
        let model = PowerModel::new_bm1387();
        let chains = vec![(9.1, vec![500u16; 63]); 3];

        let cool = model.estimate_live_with_telemetry(
            &chains,
            &[45.0_f32, 45.0_f32, 45.0_f32],
            10,
            1260,
            10.8,
            0.88,
        );
        let hot = model.estimate_live_with_telemetry(
            &chains,
            &[65.0_f32, 65.0_f32, 65.0_f32],
            10,
            1260,
            10.8,
            0.88,
        );

        assert!(
            hot.board_watts > cool.board_watts,
            "Hot estimate {:.1}W should exceed cool estimate {:.1}W",
            hot.board_watts,
            cool.board_watts
        );
    }

    #[test]
    fn test_bm1387_runtime_estimate_increases_with_fan_speed() {
        let model = PowerModel::new_bm1387();
        let chains = vec![(9.1, vec![500u16; 63]); 3];
        let temps = vec![55.0_f32, 55.0_f32, 55.0_f32];

        let quiet = model.estimate_live_with_telemetry(&chains, &temps, 10, 1260, 10.8, 0.88);
        let loud = model.estimate_live_with_telemetry(&chains, &temps, 80, 3780, 10.8, 0.88);

        assert!(
            loud.board_watts > quiet.board_watts,
            "High-fan estimate {:.1}W should exceed quiet estimate {:.1}W",
            loud.board_watts,
            quiet.board_watts
        );
    }

    #[test]
    fn test_allocate_budget_respects_limit() {
        let model = PowerModel::new_bm1387();

        // Create 63 chips all grade B with max_stable 650
        let profiles: Vec<ChipProfile> = (0..63)
            .map(|i| ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: 650,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();

        // Budget of 800W for a single chain
        let result = model.allocate_budget(800.0, 9.1, &profiles, 200, 1);
        assert_eq!(result.len(), 63);

        // Verify total power stays under budget
        let freqs_ref: Vec<u16> = result.clone();
        let chains: Vec<(u16, &[u16])> = vec![(9100, &freqs_ref)];
        let total = model.total_power_w(&chains);

        assert!(
            total <= 800.0 + 1.0, // small tolerance for PLL snapping
            "Total power {:.1}W should be <= 800W budget",
            total
        );
    }

    #[test]
    fn allocate_budget_never_panics_when_a_weak_chip_ceiling_is_below_the_floor() {
        // Regression pin for the u16::clamp(lo, hi) guard (the solar.rs crash class,
        // 687bd2ed): a degraded / weakly-binned chip can have max_stable_mhz BELOW
        // the requested min_freq floor. The guard `lo = min_freq_mhz.min(max_freq)`
        // keeps clamp from ever seeing lo > hi (which panics; panic=abort would
        // crash-loop dcentrald on restart). Removing the `.min(max_freq)` would panic
        // right here. The ceiling must win — a weak chip is never overclocked up to
        // the floor.
        let model = PowerModel::new_bm1387();
        let profiles: Vec<ChipProfile> = (0..8)
            .map(|i| ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: 100, // badly degraded, far below the 600 MHz floor
                operating_mhz: 100,
                grade: ChipGrade::B,
                error_rate: 0.5,
                nonces_counted: 10,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();

        // Floor (600) > every chip's ceiling (100): must NOT panic.
        let result = model.allocate_budget(800.0, 9.1, &profiles, 600, 1);
        assert_eq!(result.len(), 8);
        for f in result {
            assert!(
                f <= 600,
                "a weak chip (ceiling 100 MHz) must never be raised to the 600 MHz floor, got {f}"
            );
        }
    }

    #[test]
    fn test_allocate_budget_grade_weighting() {
        let model = PowerModel::new_bm1387();

        // 4 chips with different grades, generous budget so no clamping to min
        let profiles = vec![
            ChipProfile {
                chip_index: 0,
                max_stable_mhz: 900,
                operating_mhz: 800,
                grade: ChipGrade::A,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 1,
                max_stable_mhz: 900,
                operating_mhz: 700,
                grade: ChipGrade::B,
                error_rate: 0.002,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 2,
                max_stable_mhz: 900,
                operating_mhz: 600,
                grade: ChipGrade::C,
                error_rate: 0.003,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 3,
                max_stable_mhz: 900,
                operating_mhz: 500,
                grade: ChipGrade::D,
                error_rate: 0.01,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
        ];

        // Give enough budget that all chips can run, but not at max
        let result = model.allocate_budget(300.0, 9.1, &profiles, 200, 1);

        // Grade A should get highest frequency, Grade D lowest
        assert!(
            result[0] >= result[1],
            "Grade A ({}) should get >= Grade B ({})",
            result[0],
            result[1]
        );
        assert!(
            result[1] >= result[2],
            "Grade B ({}) should get >= Grade C ({})",
            result[1],
            result[2]
        );
        assert!(
            result[2] >= result[3],
            "Grade C ({}) should get >= Grade D ({})",
            result[2],
            result[3]
        );
    }

    #[test]
    fn test_allocate_budget_zero_budget() {
        let model = PowerModel::new_bm1387();

        let profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 650,
            operating_mhz: 650,
            grade: ChipGrade::B,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }];

        // Zero budget — should return minimum frequencies
        let result = model.allocate_budget(0.0, 9.1, &profiles, 200, 1);
        assert_eq!(result[0], 200);
    }

    #[test]
    fn test_allocate_budget_empty_profiles() {
        let model = PowerModel::new_bm1387();
        let result = model.allocate_budget(1000.0, 9.1, &[], 200, 1);
        assert!(result.is_empty());
    }

    #[test]
    fn test_allocate_budget_safe_derates_only_without_calibration() {
        let model = PowerModel::new_bm1387();
        let profiles: Vec<ChipProfile> = (0..63)
            .map(|i| ChipProfile {
                chip_index: i,
                max_stable_mhz: 650,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();

        let uncalibrated = model.allocate_budget_safe(500.0, 9.1, &profiles, 200, 1, None);
        let calibrated =
            model.allocate_budget_safe(500.0, 9.1, &profiles, 200, 1, Some(model.c_eff()));

        let uncalibrated_avg =
            uncalibrated.iter().map(|&freq| freq as f64).sum::<f64>() / uncalibrated.len() as f64;
        let calibrated_avg =
            calibrated.iter().map(|&freq| freq as f64).sum::<f64>() / calibrated.len() as f64;

        assert!(
            calibrated_avg > uncalibrated_avg,
            "Calibrated budget should allocate more frequency than uncalibrated safe mode"
        );
    }

    #[test]
    fn test_uncalibrated_budget_derate_is_more_conservative_for_nopic() {
        let s9_model = PowerModel::new_for_chip(0x1387);
        let s21_model = PowerModel::new_for_chip(0x1368);

        assert_eq!(s9_model.uncalibrated_budget_derate(), 0.85);
        assert_eq!(s21_model.uncalibrated_budget_derate(), 0.75);
        assert!(s21_model.uncalibrated_budget_derate() < s9_model.uncalibrated_budget_derate());
    }

    #[test]
    fn test_allocate_budget_safe_respects_saved_wall_meter_multiplier() {
        let uncalibrated_model = PowerModel::new_bm1387();
        let calibrated_model = PowerModel::new_bm1387().with_power_scale(1.10);
        let profiles: Vec<ChipProfile> = (0..63)
            .map(|i| ChipProfile {
                chip_index: i,
                max_stable_mhz: 650,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();

        let uncalibrated =
            uncalibrated_model.allocate_budget_safe(500.0, 9.1, &profiles, 200, 1, None);
        let wall_meter_calibrated =
            calibrated_model.allocate_budget_safe(500.0, 9.1, &profiles, 200, 1, None);

        let uncalibrated_avg =
            uncalibrated.iter().map(|&freq| freq as f64).sum::<f64>() / uncalibrated.len() as f64;
        let wall_meter_avg = wall_meter_calibrated
            .iter()
            .map(|&freq| freq as f64)
            .sum::<f64>()
            / wall_meter_calibrated.len() as f64;

        assert!(
            wall_meter_avg > uncalibrated_avg,
            "Saved wall-meter calibration should skip the extra uncalibrated derate"
        );
    }

    #[test]
    fn test_allocate_budget_clamps_to_max_stable() {
        let model = PowerModel::new_bm1387();

        // Chip with low max_stable — should be clamped even with generous budget
        let profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 400,
            operating_mhz: 400,
            grade: ChipGrade::C,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }];

        let result = model.allocate_budget(5000.0, 9.1, &profiles, 200, 1);
        assert!(
            result[0] <= 400,
            "Frequency {} should be clamped to max_stable 400",
            result[0]
        );
    }

    #[test]
    fn allocate_budget_inverted_freq_range_does_not_panic() {
        // Regression: `min_freq_mhz` (the config floor) and `max_stable_mhz`
        // (the per-chip runtime/persisted ceiling) come from INDEPENDENT
        // sources. A degraded or weakly-binned chip can have max_stable_mhz
        // BELOW the floor (it is shrunk at runtime via `.min(measured_freq)`
        // and can be loaded from persisted/imported state, or the operator can
        // raise the floor above a weak chip's ceiling). Before the fix,
        // `u16::clamp(min_freq_mhz, max_stable_mhz)` with min > max PANICS →
        // panic=abort → dcentrald aborts and crash-loops on restart (the exact
        // class of the prior solar.rs crash 687bd2ed). The chip's ceiling must
        // win; the result must never exceed max_stable.
        let model = PowerModel::new_bm1387();
        let profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 300, // chip can only stably reach 300 MHz...
            operating_mhz: 300,
            grade: ChipGrade::C,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }];
        // ...but the operator floor is 400 MHz (> the chip ceiling) → inverted.
        let result = model.allocate_budget(5000.0, 9.1, &profiles, 400, 1);
        assert!(
            result[0] <= 300,
            "inverted-range clamp must never exceed the chip's max_stable (300), got {}",
            result[0]
        );
        assert!(
            result[0] > 0,
            "frequency must be a sane positive value, got {}",
            result[0]
        );
    }

    #[test]
    fn test_allocate_budget_snaps_to_pll() {
        let model = PowerModel::new_bm1387();
        let pll = dcentrald_asic::drivers::bm1387::pll_frequencies();

        let profiles = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 700,
            operating_mhz: 650,
            grade: ChipGrade::B,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }];

        let result = model.allocate_budget(500.0, 9.1, &profiles, 200, 1);

        // Result should be in the PLL table
        assert!(
            pll.contains(&result[0]),
            "Frequency {} should be in PLL table",
            result[0]
        );
    }

    #[test]
    fn test_budget_for_hashrate() {
        let model = PowerModel::new_bm1387();

        // S9 reference: 189 chips at 650 MHz ≈ 14 TH/s at ~1350W
        let budget = model.budget_for_hashrate(14.0, 9.1, 189, 3);
        // Should be approximately 1350W (our calibration point)
        assert!(
            (budget - 1350.0).abs() < 50.0,
            "Expected ~1350W for 14 TH/s, got {:.0}W",
            budget
        );

        // Half hashrate should be roughly half power (dynamic component)
        let budget_7ths = model.budget_for_hashrate(7.0, 9.1, 189, 3);
        assert!(
            budget_7ths < budget,
            "7 TH/s budget ({:.0}W) should be less than 14 TH/s ({:.0}W)",
            budget_7ths,
            budget
        );
    }

    #[test]
    fn test_budget_for_hashrate_edge_cases() {
        let model = PowerModel::new_bm1387();

        assert_eq!(model.budget_for_hashrate(0.0, 9.1, 189, 3), 0.0);
        assert_eq!(model.budget_for_hashrate(14.0, 9.1, 0, 3), 0.0);
    }

    #[test]
    fn test_budget_for_hashrate_is_chip_aware() {
        let bm1387 = PowerModel::new_for_chip(0x1387);
        let bm1398 = PowerModel::new_for_chip(0x1398);

        let s9_budget = bm1387.budget_for_hashrate(14.0, 9.1, 189, 3);
        let s19_budget = bm1398.budget_for_hashrate(14.0, 13.8, 228, 3);

        assert_ne!(s9_budget, s19_budget);
        assert!(s19_budget > 0.0);
    }

    #[test]
    fn test_redistribute_freed_power() {
        let model = PowerModel::new_bm1387();

        let profiles = vec![
            ChipProfile {
                chip_index: 0,
                max_stable_mhz: 700,
                operating_mhz: 650,
                grade: ChipGrade::A,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 1,
                max_stable_mhz: 600,
                operating_mhz: 600,
                grade: ChipGrade::C,
                error_rate: 0.01,
                nonces_counted: 80,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 2,
                max_stable_mhz: 700,
                operating_mhz: 625,
                grade: ChipGrade::B,
                error_rate: 0.002,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
        ];

        let current_freqs: Vec<(u8, u16)> = vec![(0, 650), (1, 600), (2, 625)];

        // Back off chip 1 from 600 to 500
        let boosts = model.redistribute_freed_power(
            1,   // backed off chip
            600, // old freq
            500, // new freq
            9.1, // voltage
            &profiles,
            &current_freqs,
            200, // min freq
        );

        // Chip 0 (grade A) and chip 2 (grade B) should potentially get boosts
        // Both have headroom (max_stable 700)
        for &(idx, freq) in &boosts {
            assert_ne!(idx, 1, "Backed-off chip should not be boosted");
            let current = current_freqs.iter().find(|&&(i, _)| i == idx).unwrap().1;
            assert!(
                freq > current,
                "Chip {} should be boosted: {} > {}",
                idx,
                freq,
                current
            );
        }
    }

    #[test]
    fn test_redistribute_no_headroom() {
        let model = PowerModel::new_bm1387();

        // All chips already at max stable — no redistribution possible
        let profiles = vec![
            ChipProfile {
                chip_index: 0,
                max_stable_mhz: 650,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
            ChipProfile {
                chip_index: 1,
                max_stable_mhz: 600,
                operating_mhz: 600,
                grade: ChipGrade::C,
                error_rate: 0.01,
                nonces_counted: 80,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
        ];

        let current_freqs: Vec<(u8, u16)> = vec![(0, 650), (1, 600)];
        let boosts =
            model.redistribute_freed_power(1, 600, 500, 9.1, &profiles, &current_freqs, 200);
        assert!(
            boosts.is_empty(),
            "No chips have headroom — should return empty"
        );
    }
}
