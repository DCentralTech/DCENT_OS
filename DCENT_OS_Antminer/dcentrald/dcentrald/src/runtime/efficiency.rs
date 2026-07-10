//! Share-efficiency telemetry + PSU efficiency lookup tables.
//!
//! W2.1 extraction from `daemon.rs` (2026-05-07). The share-efficiency
//! tracker integrates wall-watt power over time and divides by accepted
//! pool-target / locally-achieved difficulty to surface a `J/share` and
//! `share/kWh` figure on the dashboard. It also accumulates a
//! "shares_per_kwh" counter for the home-mining ROI panel.
//!
//! Pure compute, no hardware access. Lives outside `daemon.rs` so the
//! `--s19j-hybrid` and `--stratum-proxy` modes can reuse the same
//! integration logic without dragging in S9 init code.

/// Lookup the modeled PSU efficiency (output W / wall W) for a given model
/// label. Used by the share-efficiency tracker when the platform doesn't
/// have live PMBus telemetry (APW3, APW7, APW9 — see also the PSU Override
/// Loki bypass).
pub fn psu_efficiency_for_model_name(model: &str) -> Option<f64> {
    let normalized = model.trim().to_ascii_uppercase();

    if normalized.contains("APW121215") || normalized.starts_with("APW12") {
        Some(0.93)
    } else if normalized.starts_with("APW9") {
        Some(0.91)
    } else if normalized.contains("APW7") {
        Some(0.88)
    } else if normalized.contains("APW3") {
        Some(0.85)
    } else {
        None
    }
}

pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct ShareEfficiencyTracker {
    window_started_at_ms: u64,
    last_power_timestamp_ms: u64,
    accumulated_wall_energy_kwh: f64,
    last_wall_watts: f64,
    power_source: String,
    calibrated: bool,
    accepted_share_count: u64,
    accepted_pool_target_difficulty_sum: f64,
    achieved_difficulty_sum: f64,
}

impl ShareEfficiencyTracker {
    pub fn new(initial_power: &dcentrald_autotuner::LivePowerEstimate) -> Self {
        let started_at_ms = if initial_power.timestamp_ms > 0 {
            initial_power.timestamp_ms
        } else {
            now_unix_ms()
        };
        Self {
            window_started_at_ms: started_at_ms,
            last_power_timestamp_ms: started_at_ms,
            accumulated_wall_energy_kwh: 0.0,
            last_wall_watts: initial_power.wall_watts.max(0.0),
            power_source: initial_power.source.clone(),
            calibrated: initial_power.calibrated,
            accepted_share_count: 0,
            accepted_pool_target_difficulty_sum: 0.0,
            achieved_difficulty_sum: 0.0,
        }
    }

    pub fn integrate_until(&mut self, now_ms: u64) {
        if now_ms <= self.last_power_timestamp_ms {
            return;
        }
        let delta_s = (now_ms - self.last_power_timestamp_ms) as f64 / 1000.0;
        self.accumulated_wall_energy_kwh += self.last_wall_watts * delta_s / 3_600_000.0;
        self.last_power_timestamp_ms = now_ms;
    }

    pub fn observe_power(&mut self, power: &dcentrald_autotuner::LivePowerEstimate) {
        let now_ms = if power.timestamp_ms > 0 {
            power.timestamp_ms
        } else {
            now_unix_ms()
        };
        self.integrate_until(now_ms);
        self.last_wall_watts = power.wall_watts.max(0.0);
        self.power_source = power.source.clone();
        self.calibrated = power.calibrated;
    }

    pub fn record_share(
        &mut self,
        pool_target_difficulty: f64,
        achieved_difficulty: Option<f64>,
        now_ms: u64,
    ) {
        self.integrate_until(now_ms);
        self.accepted_share_count += 1;
        self.accepted_pool_target_difficulty_sum += pool_target_difficulty.max(0.0);
        if let Some(difficulty) = achieved_difficulty.filter(|value| value.is_finite()) {
            self.achieved_difficulty_sum += difficulty.max(0.0);
        }
    }

    pub fn snapshot(&self) -> dcentrald_api::ShareEfficiencyWindow {
        let elapsed_ms = self
            .last_power_timestamp_ms
            .saturating_sub(self.window_started_at_ms);
        let energy_kwh = self.accumulated_wall_energy_kwh.max(0.0);
        let accepted_pool_target_difficulty_per_kwh =
            if energy_kwh > 0.0 && self.accepted_pool_target_difficulty_sum > 0.0 {
                Some(self.accepted_pool_target_difficulty_sum / energy_kwh)
            } else {
                None
            };
        let achieved_difficulty_sum = if self.achieved_difficulty_sum > 0.0 {
            Some(self.achieved_difficulty_sum)
        } else {
            None
        };
        dcentrald_api::ShareEfficiencyWindow {
            window_s: elapsed_ms / 1000,
            accepted_share_count: self.accepted_share_count,
            accepted_difficulty_sum: self.accepted_pool_target_difficulty_sum,
            accepted_pool_target_difficulty_sum: self.accepted_pool_target_difficulty_sum,
            achieved_difficulty_sum,
            estimated_wall_energy_kwh: energy_kwh,
            accepted_shares_per_kwh: if energy_kwh > 0.0 && self.accepted_share_count > 0 {
                Some(self.accepted_share_count as f64 / energy_kwh)
            } else {
                None
            },
            accepted_difficulty_per_kwh: accepted_pool_target_difficulty_per_kwh,
            accepted_pool_target_difficulty_per_kwh,
            achieved_difficulty_per_kwh: if energy_kwh > 0.0 && self.achieved_difficulty_sum > 0.0 {
                Some(self.achieved_difficulty_sum / energy_kwh)
            } else {
                None
            },
            difficulty_source: "pool_target_minimum".to_string(),
            power_source: self.power_source.clone(),
            calibrated: self.calibrated,
        }
    }
}
