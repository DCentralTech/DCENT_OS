use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn legacy_history_power_source() -> String {
    "legacy_unprovenanced".to_string()
}

fn legacy_history_power_source_detail() -> String {
    "legacy_history_without_provenance".to_string()
}

fn legacy_history_power_modeled() -> bool {
    true
}

fn legacy_history_power_note() -> String {
    "Sample predates history power provenance; legacy power_watts is retained for compatibility."
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotContext {
    pub report_id: Uuid,
    pub generated_at: String,
    pub firmware_version: String,
    pub serial: Option<String>,
    pub mac: Option<String>,
    pub model: Option<String>,
    pub chip_type: String,
    pub chip_id: Option<u16>,
    pub control_board: String,
    pub board_type: Option<String>,
    pub chain_states: Vec<SnapshotChain>,
    pub fan_pwm: u8,
    pub fan_rpm: u32,
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub pool_url: String,
    pub pool_status: String,
    pub pool_difficulty: f64,
    pub uptime_s: u64,
    pub history: Vec<SnapshotHistorySample>,
    pub live_chip_health: Vec<SnapshotChipHealth>,
    pub saved_profiles: Vec<SnapshotProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotChain {
    pub chain_id: u8,
    pub chips: u8,
    pub frequency_mhz: u16,
    pub voltage_mv: u16,
    pub temp_c: f32,
    pub hashrate_ghs: f64,
    pub errors: u32,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHistorySample {
    pub timestamp_s: u64,
    pub hashrate_ghs: f64,
    pub power_watts: f64,
    #[serde(default = "legacy_history_power_source")]
    pub power_source: String,
    #[serde(default = "legacy_history_power_source_detail")]
    pub power_source_detail: String,
    #[serde(default)]
    pub live_power_available: bool,
    #[serde(default = "legacy_history_power_modeled")]
    pub power_modeled: bool,
    #[serde(default)]
    pub power_calibrated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_calibration_multiplier: Option<f64>,
    #[serde(default = "legacy_history_power_note")]
    pub power_note: String,
    pub temp_c: f32,
    pub fan_pwm: u8,
    pub fan_rpm: u32,
    pub accepted: u64,
    pub rejected: u64,
    pub pool_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotChipHealth {
    pub chain_id: u8,
    pub chip_index: u8,
    pub health_score: f64,
    pub trend: f64,
    pub estimated_days_to_warning: Option<f64>,
    pub error_rate_pct: f64,
    pub freq_mhz: u16,
    pub backoff_count: u32,
    pub hashrate_ratio: f64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotProfile {
    pub chain_id: u8,
    pub chip_count: u8,
    pub voltage_mv: u16,
    pub estimated_hashrate_ghs: f64,
    pub chips: Vec<SnapshotProfileChip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotProfileChip {
    pub chip_index: u8,
    pub operating_mhz: u16,
    pub grade: char,
    pub error_rate: f64,
    pub nonces_counted: u64,
    pub thermal_max_stable_mhz: Option<u16>,
}

impl SnapshotContext {
    pub fn chain(&self, chain_id: u8) -> Option<&SnapshotChain> {
        self.chain_states
            .iter()
            .find(|chain| chain.chain_id == chain_id)
    }

    pub fn profile(&self, chain_id: u8) -> Option<&SnapshotProfile> {
        self.saved_profiles
            .iter()
            .find(|profile| profile.chain_id == chain_id)
    }

    pub fn expected_chip_count(&self, chain_id: u8) -> u16 {
        self.profile(chain_id)
            .map(|profile| profile.chip_count as u16)
            .or_else(|| self.chain(chain_id).map(|chain| chain.chips as u16))
            .unwrap_or(0)
    }
}
