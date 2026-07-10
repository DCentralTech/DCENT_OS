//! Mining profitability estimation and noise-aware optimization.
//!
//! Two key features no competitor combines:
//!
//! 1. **Profitability Calculator**: Given BTC price, electricity cost, hashrate,
//!    and power consumption, estimate daily/monthly profit. Displayed in the
//!    dashboard for Standard Mining Mode.
//!
//! 2. **Legacy S9 Noise-Aware Optimization**: S9-only calibration maps
//!    a measured noise target to a fan PWM estimate:
//!    noise target → fan PWM → thermal ceiling → max frequency → power output.
//!    AM2/XIL must not use this as acoustic proof; live tach/RPM is required.
//!
//! 3. **Room Temperature Feedback**: When Home Assistant reports room temperature
//!    has reached target, the autotuner throttles the miner automatically.
//!    The miner IS the thermostat.
//!
//! 4. **Halving-Aware Projections** (W8.3): Block reward is computed as a
//!    function of `now: SystemTime` rather than hardcoded. The next halving
//!    (~2028-04-15 estimated) cuts the reward from 3.125 BTC → 1.5625 BTC,
//!    which is roughly a -50% revenue impact at constant difficulty.
//!    The estimator also returns `daily_btc_post_halving`,
//!    `days_to_halving`, and `breakeven_post_halving_btc_price` so the
//!    dashboard / install wizard can render the cliff explicitly instead
//!    of silently presenting stale economics.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Approximate halving epochs (Unix seconds) for the next four halvings.
///
/// Bitcoin halvings occur every 210,000 blocks. Block heights are deterministic
/// (every halving reduces reward by 50%) but the wall-clock time of each
/// halving drifts based on actual block production rate.
///
/// Reference points:
/// - 2024-04-19: 3.125 BTC reward (block 840,000) — actual.
/// - 2028-04-15: 1.5625 BTC reward (block 1,050,000) — estimated.
/// - 2032-04-15: 0.78125 BTC reward (block 1,260,000) — estimated.
/// - 2036-04-15: 0.390625 BTC reward (block 1,470,000) — estimated.
///
/// These dates are recomputed when the network drifts; the estimator is
/// not a forecasting service. They are used as a "show the cliff" UX hint,
/// not a financial projection.
const HALVING_EPOCHS_SEC: &[(i64, f64)] = &[
    // (unix_sec, reward_btc_AFTER_halving)
    (1_713_484_800, 3.125),    // 2024-04-19
    (1_839_000_000, 1.5625),   // ~2028-04-15
    (1_965_000_000, 0.78125),  // ~2032-04-15
    (2_091_000_000, 0.390625), // ~2036-04-15
];

/// Returns the active block reward at a given wall-clock time.
///
/// `now` is treated as Unix-epoch seconds. Pre-2009 inputs return the
/// post-2024 halving reward (3.125) on the assumption that the caller
/// is asking about current/future economics; this avoids panicking on
/// `SystemTime::now()` returning weird values during boot.
pub fn block_reward_at(now: SystemTime) -> f64 {
    let now_sec = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Walk halvings in order; the active reward is the most recent
    // halving whose epoch is <= now.
    let mut reward = 3.125_f64;
    for (epoch, post_halving_reward) in HALVING_EPOCHS_SEC {
        if now_sec >= *epoch {
            reward = *post_halving_reward;
        } else {
            break;
        }
    }
    reward
}

/// Returns (next_halving_unix_sec, post_halving_reward) for the halving
/// strictly after `now`, or `None` if we are past the last known epoch.
pub fn next_halving(now: SystemTime) -> Option<(i64, f64)> {
    let now_sec = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    HALVING_EPOCHS_SEC
        .iter()
        .find(|(epoch, _)| *epoch > now_sec)
        .copied()
}

/// Days until the next halving from `now`. Returns `None` if past the
/// last known epoch.
pub fn days_to_halving(now: SystemTime) -> Option<f64> {
    let now_sec = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    next_halving(now).map(|(epoch, _)| {
        let secs = (epoch - now_sec).max(0) as f64;
        secs / 86_400.0
    })
}

/// Profitability estimate for a mining configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfitabilityEstimate {
    /// Total hashrate (TH/s).
    pub hashrate_ths: f64,
    /// Total power consumption (watts).
    pub power_w: f64,
    /// Electricity cost per kWh (USD).
    pub electricity_cost_kwh: f64,
    /// Current BTC price (USD).
    pub btc_price_usd: f64,
    /// Network difficulty.
    pub network_difficulty: f64,
    /// Daily electricity cost (USD).
    pub daily_electricity_usd: f64,
    /// Daily BTC mined (estimated).
    pub daily_btc: f64,
    /// Daily BTC revenue (USD).
    pub daily_revenue_usd: f64,
    /// Daily profit = revenue - electricity (USD). Negative = losing money.
    pub daily_profit_usd: f64,
    /// Monthly profit projection (USD).
    pub monthly_profit_usd: f64,
    /// Efficiency (J/TH).
    pub efficiency_jth: f64,
    /// Cost to produce 1 BTC at current settings (USD).
    pub cost_per_btc_usd: f64,
    /// Block reward currently in effect (BTC).
    pub block_reward_btc: f64,
    /// Daily BTC mined AFTER the next halving (assuming constant difficulty
    /// and hashrate). Roughly `daily_btc * (next_reward / current_reward)`.
    pub daily_btc_post_halving: f64,
    /// Daily revenue (USD) AFTER the next halving (constant BTC price).
    pub daily_revenue_post_halving_usd: f64,
    /// Daily profit (USD) AFTER the next halving (constant BTC price + cost).
    pub daily_profit_post_halving_usd: f64,
    /// Days until the next halving. `None` if past last known epoch.
    pub days_to_halving: Option<f64>,
    /// BTC price at which post-halving daily revenue equals daily electricity
    /// cost. `None` if revenue or hashrate is zero.
    pub breakeven_post_halving_btc_price: Option<f64>,
    /// 4-year cumulative BTC, accounting for the halving cliff (if any) at
    /// `days_to_halving`. Constant difficulty + hashrate assumed.
    pub four_year_amortized_btc: f64,
    /// 4-year cumulative revenue (USD), constant BTC price.
    pub four_year_amortized_revenue_usd: f64,
    /// Provenance of `power_w` (and therefore of every cost/profit figure
    /// derived from it). Profitability ALWAYS runs on modeled watts — never a
    /// direct wall-meter measurement — so this is `"model"` by default, or
    /// `"calibrated_model"` when an operator wall-meter calibration multiplier
    /// scaled the modeled power. A consumer must not read the daily cost as if
    /// it came from a metered reading.
    #[serde(default = "default_profitability_power_basis")]
    pub power_basis: String,
}

/// Serde default for [`ProfitabilityEstimate::power_basis`]. Profitability is a
/// modeled projection, so the default basis is the plain model.
fn default_profitability_power_basis() -> String {
    "model".to_string()
}

/// Calculate mining profitability.
///
/// Uses the standard Bitcoin mining revenue formula:
///   daily_btc = (hashrate_ths * 1e12 * 86400) / (network_difficulty * 2^32)
///
/// This is a simplified model that doesn't account for pool fees or luck
/// variance. Block reward is now halving-aware (W8.3): `SystemTime::now()`
/// drives `block_reward_at(now)` and the result also exposes
/// `daily_btc_post_halving`, `days_to_halving`, and
/// `breakeven_post_halving_btc_price` so the dashboard can render the cliff
/// instead of presenting silently-stale economics.
pub fn estimate_profitability(
    hashrate_ths: f64,
    power_w: f64,
    electricity_cost_kwh: f64,
    btc_price_usd: f64,
    network_difficulty: f64,
) -> ProfitabilityEstimate {
    estimate_profitability_at(
        hashrate_ths,
        power_w,
        electricity_cost_kwh,
        btc_price_usd,
        network_difficulty,
        SystemTime::now(),
    )
}

/// Calculate mining profitability with an injected wall-clock time.
///
/// This is the pure form used by tests and by callers that need to model
/// a specific halving epoch (e.g. wizard "show what happens in 2028").
/// Production code should call `estimate_profitability` which uses
/// `SystemTime::now()`.
pub fn estimate_profitability_at(
    hashrate_ths: f64,
    power_w: f64,
    electricity_cost_kwh: f64,
    btc_price_usd: f64,
    network_difficulty: f64,
    now: SystemTime,
) -> ProfitabilityEstimate {
    // Daily electricity cost
    let daily_kwh = power_w * 24.0 / 1000.0;
    let daily_electricity = daily_kwh * electricity_cost_kwh;

    // Daily BTC mined (simplified formula).
    // blocks_per_day = 86400 / 600 = 144.
    // hashrate_fraction = hashrate_ths * 1e12 / (network_difficulty * 2^32 / 600)
    // daily_btc = hashrate_fraction * blocks_per_day * block_reward
    let block_reward = block_reward_at(now);
    let blocks_per_day = 144.0_f64;
    let network_hashrate_approx = network_difficulty * (2.0_f64.powi(32)) / 600.0;
    let hashrate_fraction = if network_hashrate_approx > 0.0 {
        (hashrate_ths * 1e12) / network_hashrate_approx
    } else {
        0.0
    };
    let daily_btc = hashrate_fraction * blocks_per_day * block_reward;

    // Post-halving projection (constant difficulty + hashrate + price).
    let next_reward = next_halving(now).map(|(_, r)| r);
    let halving_factor = match next_reward {
        Some(r) if block_reward > 0.0 => r / block_reward,
        _ => 1.0,
    };
    let daily_btc_post_halving = daily_btc * halving_factor;
    let daily_revenue_post_halving = daily_btc_post_halving * btc_price_usd;
    let daily_profit_post_halving = daily_revenue_post_halving - daily_electricity;

    // Revenue and profit
    let daily_revenue = daily_btc * btc_price_usd;
    let daily_profit = daily_revenue - daily_electricity;
    let monthly_profit = daily_profit * 30.44; // Average days per month

    // Efficiency
    let efficiency_jth = if hashrate_ths > 0.0 {
        power_w / hashrate_ths
    } else {
        0.0
    };

    // Cost per BTC (current reward).
    let cost_per_btc = if daily_btc > 0.0 {
        daily_electricity / daily_btc
    } else {
        0.0 // No BTC mined — cost is undefined, use 0 to avoid serde_json Infinity error
    };

    let dth = days_to_halving(now);

    // Break-even BTC price post-halving:
    //   daily_revenue_post_halving = daily_btc_post_halving * P
    //   set equal to daily_electricity → P = daily_electricity / daily_btc_post_halving
    let breakeven_post_halving_btc_price = if daily_btc_post_halving > 0.0 {
        Some(daily_electricity / daily_btc_post_halving)
    } else {
        None
    };

    // 4-year amortization with the halving cliff.
    // If a halving occurs within the 4-year window, we earn `daily_btc` for
    // `days_to_halving` days, then `daily_btc_post_halving` for the remainder.
    // If no halving in the window (e.g. caller after 2036), we just integrate
    // daily_btc for 1461 days.
    const FOUR_YEARS_DAYS: f64 = 365.25 * 4.0;
    let (pre_days, post_days) = match dth {
        Some(d) if d < FOUR_YEARS_DAYS => (d, FOUR_YEARS_DAYS - d),
        _ => (FOUR_YEARS_DAYS, 0.0),
    };
    let four_year_amortized_btc = daily_btc * pre_days + daily_btc_post_halving * post_days;
    let four_year_amortized_revenue_usd = four_year_amortized_btc * btc_price_usd;

    ProfitabilityEstimate {
        hashrate_ths,
        power_w,
        electricity_cost_kwh,
        btc_price_usd,
        network_difficulty,
        daily_electricity_usd: daily_electricity,
        daily_btc,
        daily_revenue_usd: daily_revenue,
        daily_profit_usd: daily_profit,
        monthly_profit_usd: monthly_profit,
        efficiency_jth,
        cost_per_btc_usd: cost_per_btc,
        block_reward_btc: block_reward,
        daily_btc_post_halving,
        daily_revenue_post_halving_usd: daily_revenue_post_halving,
        daily_profit_post_halving_usd: daily_profit_post_halving,
        days_to_halving: dth,
        breakeven_post_halving_btc_price,
        four_year_amortized_btc,
        four_year_amortized_revenue_usd,
        // Default provenance: modeled watts. Callers that scaled `power_w` by
        // an operator wall-meter calibration overwrite this with
        // `"calibrated_model"` (see `post_autotuner_profitability`).
        power_basis: default_profitability_power_basis(),
    }
}

/// Legacy S9-only noise-to-frequency mapping for Space Heater Mode.
///
/// S9 noise levels (measured at 1m distance):
///   PWM 100 (max): ~76 dB (data center level)
///   PWM  80:       ~65 dB (vacuum cleaner)
///   PWM  40:       ~50 dB (quiet office)
///   PWM  20:       ~42 dB (library)
///   PWM  10:       ~38 dB (S9-specific low setting)
///
/// The mapping is approximate and varies by enclosure, fan model, and muffling.
/// Users should calibrate with live tach/RPM and a meter for their specific setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseProfile {
    /// Maximum acceptable noise level (dB at 1m).
    pub max_noise_db: f32,
    /// S9-derived fan PWM estimate for the noise target.
    pub target_fan_pwm: u8,
    /// Resulting thermal ceiling factor (0.65 - 1.0).
    pub thermal_factor: f64,
    /// Maximum sustainable power at this noise level (watts, estimated).
    pub max_sustainable_watts: f64,
}

/// Convert a noise target (dB) to a fan PWM setting.
///
/// Based on S9 fan noise measurements. Linear interpolation between
/// calibration points. Returns PWM 10-100.
pub fn noise_to_fan_pwm(target_noise_db: f32) -> u8 {
    // Calibration points: (dB, PWM)
    // Derived from S9 fan measurements at 1m distance
    let calibration: [(f32, f32); 5] = [
        (38.0, 10.0),  // S9-specific low setting
        (42.0, 20.0),  // library
        (50.0, 40.0),  // quiet office
        (65.0, 80.0),  // vacuum cleaner
        (76.0, 100.0), // data center
    ];

    if target_noise_db <= calibration[0].0 {
        return calibration[0].1 as u8;
    }
    if target_noise_db >= calibration[calibration.len() - 1].0 {
        return calibration[calibration.len() - 1].1 as u8;
    }

    // Linear interpolation
    for i in 0..calibration.len() - 1 {
        let (db_lo, pwm_lo) = calibration[i];
        let (db_hi, pwm_hi) = calibration[i + 1];
        if target_noise_db >= db_lo && target_noise_db <= db_hi {
            let t = (target_noise_db - db_lo) / (db_hi - db_lo);
            let pwm = pwm_lo + t * (pwm_hi - pwm_lo);
            return (pwm as u8).clamp(10, 100);
        }
    }

    64 // fallback
}

/// Compute a full noise profile for Space Heater Mode.
///
/// Given a noise target, computes an S9-derived fan PWM, thermal ceiling factor,
/// and maximum sustainable power. AM2/XIL callers must not use this as acoustic
/// proof; live tach/RPM/acoustic calibration is required there.
pub fn compute_noise_profile(target_noise_db: f32) -> NoiseProfile {
    let pwm = noise_to_fan_pwm(target_noise_db);
    let thermal_factor = super::tuner::fan_thermal_factor(pwm);

    // Estimate max sustainable power at this fan speed.
    // At PWM 100 (max cooling): ~1350W (full S9)
    // At PWM 10 (S9 low):       ~500W (limited cooling)
    // Linear approximation between 10 and 100 PWM.
    let power_fraction = (pwm as f64 - 10.0).max(0.0) / (100.0 - 10.0);
    let max_watts = 500.0 + power_fraction * 850.0;

    NoiseProfile {
        max_noise_db: target_noise_db,
        target_fan_pwm: pwm,
        thermal_factor,
        max_sustainable_watts: max_watts,
    }
}

/// Room temperature feedback for Space Heater Mode.
///
/// When the room temperature reaches the target, the miner should throttle
/// down. When it drops below target - hysteresis, ramp back up.
///
/// Returns the power scaling factor (0.0 - 1.0). Multiply by target_watts.
pub fn room_temp_power_factor(room_temp_c: f32, target_temp_c: f32, hysteresis_c: f32) -> f64 {
    if room_temp_c >= target_temp_c {
        // Room is at or above target — reduce to minimum (keep circulating air)
        0.1
    } else if room_temp_c > target_temp_c - hysteresis_c {
        // Within hysteresis band — proportional reduction
        let band = hysteresis_c;
        let above_lower = room_temp_c - (target_temp_c - hysteresis_c);
        let factor = 1.0 - (above_lower / band) as f64 * 0.9;
        factor.clamp(0.1, 1.0)
    } else {
        // Room is cold — full power
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profitability_basic() {
        // S9 at 14 TH/s, 1350W, $0.10/kWh, $100k BTC, difficulty 100T
        let est = estimate_profitability(
            14.0,
            1350.0,
            0.10,
            100_000.0,
            100_000_000_000_000.0, // 100T difficulty
        );

        assert!(est.daily_electricity_usd > 0.0);
        assert!(est.daily_btc > 0.0);
        assert!(est.efficiency_jth > 0.0);
        assert!(est.cost_per_btc_usd > 0.0);
        // At $0.10/kWh, S9 should cost about $3.24/day in electricity
        assert!(
            (est.daily_electricity_usd - 3.24).abs() < 0.1,
            "Expected ~$3.24/day electricity, got ${:.2}",
            est.daily_electricity_usd,
        );
    }

    #[test]
    fn test_profitability_zero_hashrate() {
        let est = estimate_profitability(0.0, 0.0, 0.10, 100_000.0, 1e14);
        assert_eq!(est.daily_btc, 0.0);
        assert_eq!(est.daily_revenue_usd, 0.0);
    }

    #[test]
    fn test_profitability_serialization() {
        let est = estimate_profitability(14.0, 1350.0, 0.10, 100_000.0, 1e14);
        let json = serde_json::to_string(&est).expect("serialize failed");
        let deser: ProfitabilityEstimate = serde_json::from_str(&json).expect("deserialize failed");
        assert!((deser.hashrate_ths - 14.0).abs() < 0.01);
    }

    #[test]
    fn test_noise_to_fan_pwm() {
        // Exact calibration points
        assert_eq!(noise_to_fan_pwm(38.0), 10);
        assert_eq!(noise_to_fan_pwm(76.0), 100);

        // Below minimum
        assert_eq!(noise_to_fan_pwm(30.0), 10);

        // Above maximum
        assert_eq!(noise_to_fan_pwm(90.0), 100);

        // Interpolation: 44 dB should be between 20 and 40 PWM
        let pwm_44 = noise_to_fan_pwm(44.0);
        assert!(
            pwm_44 > 20 && pwm_44 < 40,
            "44 dB → PWM {} should be 20-40",
            pwm_44
        );

        // Monotonic: higher noise target → higher PWM allowed
        let pwm_40 = noise_to_fan_pwm(40.0);
        let pwm_50 = noise_to_fan_pwm(50.0);
        let pwm_60 = noise_to_fan_pwm(60.0);
        assert!(pwm_40 < pwm_50, "40dB ({}) < 50dB ({})", pwm_40, pwm_50);
        assert!(pwm_50 < pwm_60, "50dB ({}) < 60dB ({})", pwm_50, pwm_60);
    }

    #[test]
    fn test_noise_profile() {
        let profile = compute_noise_profile(42.0); // Library quiet
        assert_eq!(profile.target_fan_pwm, 20);
        assert!(
            profile.thermal_factor < 0.75,
            "Quiet mode should have low thermal factor"
        );
        assert!(
            profile.max_sustainable_watts < 600.0,
            "Quiet mode should limit power to ~500W, got {:.0}",
            profile.max_sustainable_watts,
        );
    }

    // ---------- W8.3 halving-aware tests ----------

    fn t(secs: i64) -> SystemTime {
        // All test timestamps are post-1970; clamp to keep the helper
        // non-panicking even if a future test passes a negative offset.
        let s = secs.max(0) as u64;
        UNIX_EPOCH + std::time::Duration::from_secs(s)
    }

    #[test]
    fn test_block_reward_post_2024_halving() {
        // 2024-04-19 + 1s, before 2028
        let now = t(1_713_484_801);
        assert!((block_reward_at(now) - 3.125).abs() < 1e-9);
    }

    #[test]
    fn test_block_reward_post_2028_halving() {
        // 2028-04-15 + 1s
        let now = t(1_839_000_001);
        assert!((block_reward_at(now) - 1.5625).abs() < 1e-9);
    }

    #[test]
    fn test_block_reward_post_2032_halving() {
        let now = t(1_965_000_001);
        assert!((block_reward_at(now) - 0.78125).abs() < 1e-9);
    }

    #[test]
    fn test_block_reward_pre_2024_falls_back() {
        // We treat pre-2024 inputs as "use post-2024 reward" because the
        // estimator is forward-looking; old timestamps should not crash.
        let now = t(1_600_000_000); // 2020
        assert!((block_reward_at(now) - 3.125).abs() < 1e-9);
    }

    #[test]
    fn test_next_halving_before_2028() {
        let now = t(1_800_000_000); // 2027
        let (epoch, reward) = next_halving(now).expect("expected next halving");
        assert_eq!(epoch, 1_839_000_000);
        assert!((reward - 1.5625).abs() < 1e-9);
    }

    #[test]
    fn test_next_halving_past_last_known_returns_none() {
        // Far future: 2100
        let now = t(4_102_444_800);
        assert!(next_halving(now).is_none());
        assert!(days_to_halving(now).is_none());
    }

    #[test]
    fn test_days_to_halving_monotonic() {
        let now1 = t(1_800_000_000);
        let now2 = t(1_800_086_400); // +1 day
        let d1 = days_to_halving(now1).expect("d1");
        let d2 = days_to_halving(now2).expect("d2");
        assert!(d1 > d2);
        assert!((d1 - d2 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_estimate_profitability_at_pre_2028_carries_full_reward() {
        // 2026-05 — pre-2028 halving
        let now = t(1_777_000_000);
        let est = estimate_profitability_at(14.0, 1350.0, 0.10, 100_000.0, 1e14, now);
        assert!((est.block_reward_btc - 3.125).abs() < 1e-9);
        assert!(est.days_to_halving.expect("dth") > 0.0);
        // Post-halving daily btc should be ~half of current daily btc.
        assert!(est.daily_btc > 0.0);
        let ratio = est.daily_btc_post_halving / est.daily_btc;
        assert!(
            (ratio - 0.5).abs() < 1e-6,
            "post-halving ratio {} should be 0.5",
            ratio
        );
    }

    #[test]
    fn test_estimate_profitability_at_post_2028_uses_smaller_reward() {
        // 2029 — post-2028 halving, pre-2032
        let now = t(1_870_000_000);
        let est = estimate_profitability_at(14.0, 1350.0, 0.10, 100_000.0, 1e14, now);
        assert!((est.block_reward_btc - 1.5625).abs() < 1e-9);
        // Post-halving (next is 2032 → 0.78125): ratio 0.78125 / 1.5625 = 0.5
        let ratio = est.daily_btc_post_halving / est.daily_btc;
        assert!(
            (ratio - 0.5).abs() < 1e-6,
            "post-halving ratio {} should be 0.5",
            ratio
        );
    }

    #[test]
    fn test_breakeven_post_halving_btc_price_doubles_from_current_breakeven() {
        // At constant difficulty + hashrate, halving the reward doubles
        // the break-even price required to cover the same electricity cost.
        let now = t(1_777_000_000);
        let est = estimate_profitability_at(14.0, 1350.0, 0.10, 100_000.0, 1e14, now);
        let pre_breakeven = est.daily_electricity_usd / est.daily_btc;
        let post = est.breakeven_post_halving_btc_price.expect("post be");
        assert!(
            (post - pre_breakeven * 2.0).abs() / pre_breakeven < 1e-6,
            "post halving be {} should be ~2x pre be {}",
            post,
            pre_breakeven
        );
    }

    #[test]
    fn test_four_year_amortization_includes_cliff() {
        // Pick a `now` where halving is exactly 1 year out, so the 4-year
        // window splits 1 year pre + 3 years post.
        // 2028-04-15 - 365.25d ≈ 1_807_440_000.
        let now = t(1_839_000_000 - (365.25 * 86400.0) as i64);
        let est = estimate_profitability_at(14.0, 1350.0, 0.10, 100_000.0, 1e14, now);
        // Naive (no halving): daily_btc * 1461.
        let naive = est.daily_btc * 365.25 * 4.0;
        // With halving: 1y full + 3y half ⇒ 1y + 1.5y = 2.5 effective years
        // vs naive 4. So amortized should be 2.5/4 of naive ≈ 0.625x.
        let ratio = est.four_year_amortized_btc / naive;
        assert!(
            (ratio - 0.625).abs() < 0.01,
            "4y amortized ratio {} should be ~0.625",
            ratio
        );
    }

    #[test]
    fn test_estimate_profitability_now_basic_smoke() {
        // Uses SystemTime::now() — must not panic and must produce sane shape.
        let est = estimate_profitability(14.0, 1350.0, 0.10, 100_000.0, 1e14);
        assert!(est.daily_btc > 0.0);
        assert!(est.daily_btc_post_halving > 0.0);
        assert!(est.block_reward_btc > 0.0 && est.block_reward_btc <= 3.125);
        // Either we are pre-last-halving (Some) or far future (None) — both ok.
        match est.days_to_halving {
            Some(d) => assert!(d > 0.0),
            None => {}
        }
    }

    #[test]
    fn test_room_temp_factor() {
        // Room cold: full power
        assert!((room_temp_power_factor(18.0, 22.0, 2.0) - 1.0).abs() < 0.01);

        // Room at target: minimum power
        let at_target = room_temp_power_factor(22.0, 22.0, 2.0);
        assert!(
            (at_target - 0.1).abs() < 0.01,
            "At target should be 0.1, got {}",
            at_target
        );

        // Room above target: minimum power
        let above = room_temp_power_factor(25.0, 22.0, 2.0);
        assert!((above - 0.1).abs() < 0.01);

        // Room in hysteresis band (21C with target 22, hysteresis 2): partial
        let in_band = room_temp_power_factor(21.0, 22.0, 2.0);
        assert!(
            in_band > 0.1 && in_band < 1.0,
            "In band should be partial, got {}",
            in_band
        );

        // Just below hysteresis: full power
        let below = room_temp_power_factor(19.9, 22.0, 2.0);
        assert!(
            (below - 1.0).abs() < 0.01,
            "Below band should be 1.0, got {}",
            below
        );
    }
}
