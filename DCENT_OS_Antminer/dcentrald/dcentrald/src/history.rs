//! Historical data ring buffer for dcentrald.
//!
//! Stores 5-minute samples of mining metrics for 24-hour retention.
//! Persisted to /data/dcent/history.json on shutdown, loaded on boot.
//!
//! Design:
//!   - Thread-safe via Arc<Mutex<VecDeque>>
//!   - Fixed capacity ring buffer (evicts oldest on overflow)
//!   - Crash-safe persistence (sibling temp file, fsync, atomic rename, parent fsync)
//!   - Shared by the daemon sampler and REST API surfaces

use dcentrald_api::{atomic_io, MinerState};
use dcentrald_autotuner::{LivePowerEstimate, PowerAuthorityKind};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Maximum samples retained (24h at 5-min intervals = 288).
const MAX_SAMPLES: usize = 288;
/// Sampling interval in seconds (5 minutes).
pub const HISTORY_INTERVAL_S: u64 = 300;

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

/// A single historical data point captured every 5 minutes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySample {
    /// Unix timestamp (seconds since epoch).
    #[serde(rename = "timestamp", alias = "timestamp_s")]
    pub timestamp_s: u64,
    /// Total hashrate in GH/s across all chains.
    pub hashrate_ghs: f64,
    /// Legacy graph scalar for wall power in watts.
    ///
    /// New samples publish `0.0` when live power is unavailable; provenance
    /// fields below distinguish measured, modeled, unavailable, and legacy
    /// persisted values.
    pub power_watts: f64,
    /// Canonical source for the `power_watts` scalar.
    #[serde(default = "legacy_history_power_source")]
    pub power_source: String,
    /// More precise source class for UI/API consumers.
    #[serde(default = "legacy_history_power_source_detail")]
    pub power_source_detail: String,
    /// True when the sample came from a positive live runtime power estimate.
    #[serde(default)]
    pub live_power_available: bool,
    /// True when `power_watts` is a model-derived runtime value, not measured.
    #[serde(default = "legacy_history_power_modeled")]
    pub power_modeled: bool,
    /// True when a persisted wall-meter calibration shaped the modeled value.
    #[serde(default)]
    pub power_calibrated: bool,
    /// Active calibration multiplier, when `power_calibrated=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_calibration_multiplier: Option<f64>,
    /// Operator-facing explanation for the power value.
    #[serde(default = "legacy_history_power_note")]
    pub power_note: String,
    /// Maximum chip temperature across all chains (Celsius).
    pub temp_c: f32,
    /// Current fan PWM value (0-127).
    pub fan_pwm: u8,
    /// Fan speed in RPM (from tachometer).
    pub fan_rpm: u32,
    /// Total accepted shares (cumulative).
    pub accepted: u64,
    /// Total rejected shares (cumulative).
    pub rejected: u64,
    /// Pool connection status ("alive" or "dead").
    pub pool_status: String,
}

/// Thread-safe ring buffer of historical mining samples.
///
/// Cloneable (via Arc) — multiple tasks can hold a handle and push/read
/// samples concurrently. The internal Mutex serializes access.
#[derive(Debug, Clone)]
pub struct HistoryBuffer {
    inner: Arc<Mutex<VecDeque<HistorySample>>>,
}

impl HistoryBuffer {
    /// Create a new empty buffer with pre-allocated capacity.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_SAMPLES))),
        }
    }

    /// Push a new sample, evicting the oldest if at capacity.
    pub fn push(&self, sample: HistorySample) {
        let mut buf = self.inner.lock().unwrap();
        if buf.len() >= MAX_SAMPLES {
            buf.pop_front();
        }
        buf.push_back(sample);
    }

    /// Get all samples as a Vec for JSON serialization.
    pub fn samples(&self) -> Vec<HistorySample> {
        self.inner.lock().unwrap().iter().cloned().collect()
    }

    /// Number of samples currently stored.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Returns true if no samples are stored.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }

    /// Save buffer to a JSON file (crash-safe: write sibling temp, fsync, rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let samples = self.samples();
        let json = serde_json::to_string(&samples).map_err(std::io::Error::other)?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Reuse the same fsync-before-rename primitive as config persistence:
        // sibling tempfile, fsync file bytes, atomic rename, fsync parent dir.
        atomic_io::atomic_write(path, json)?;

        tracing::info!(
            samples = samples.len(),
            path = %path.display(),
            "History saved to disk"
        );
        Ok(())
    }

    /// Load buffer from a JSON file. Returns empty buffer if file doesn't exist
    /// or is corrupted (with a warning log).
    pub fn load(path: &Path) -> Self {
        let buf = Self::new();
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(json) => {
                    match serde_json::from_str::<Vec<HistorySample>>(&json) {
                        Ok(samples) => {
                            let mut inner = buf.inner.lock().unwrap();
                            // Take at most MAX_SAMPLES from the end (most recent)
                            for s in samples.into_iter().rev().take(MAX_SAMPLES).rev() {
                                inner.push_back(s);
                            }
                            tracing::info!(
                                samples = inner.len(),
                                path = %path.display(),
                                "History loaded from disk"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %path.display(),
                                "Failed to parse history file — starting fresh"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "Failed to read history file — starting fresh"
                    );
                }
            }
        }
        buf
    }
}

impl Default for HistoryBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the on-device history storage path.
pub fn storage_path() -> PathBuf {
    if Path::new("/data").exists() {
        PathBuf::from("/data/dcent/history.json")
    } else {
        PathBuf::from("/tmp/dcent/history.json")
    }
}

#[derive(Debug, Clone)]
struct HistoryPowerProjection {
    power_watts: f64,
    source: String,
    source_detail: &'static str,
    live_power_available: bool,
    modeled: bool,
    calibrated: bool,
    calibration_multiplier: Option<f64>,
    note: &'static str,
}

fn (power: &LivePowerEstimate) -> HistoryPowerProjection {
    let live_power_available = power.board_watts.is_finite()
        && power.board_watts > 0.0
        && power.wall_watts.is_finite()
        && power.wall_watts > 0.0;
    if !live_power_available {
        return HistoryPowerProjection {
            power_watts: 0.0,
            source: "unavailable".to_string(),
            source_detail: "live_power_unavailable",
            live_power_available: false,
            modeled: false,
            calibrated: false,
            calibration_multiplier: None,
            note: "Live power has not published positive board and wall watts for this history sample.",
        };
    }

    let source = if power.source.trim().is_empty() {
        "live_power_watch".to_string()
    } else {
        power.source.clone()
    };
    let authority = PowerAuthorityKind::from_source(&source, power.calibrated);
    let source_detail = match authority {
        PowerAuthorityKind::Pmbus => "pmbus_measured",
        PowerAuthorityKind::Adc => "adc_measured",
        PowerAuthorityKind::WallCalibratedEstimate => "wall_calibrated_estimate",
        PowerAuthorityKind::Estimated | PowerAuthorityKind::Unknown => "live_runtime_model",
    };
    let measured = authority.is_measured();

    HistoryPowerProjection {
        power_watts: power.wall_watts,
        source,
        source_detail,
        live_power_available: true,
        modeled: !measured,
        calibrated: power.calibrated,
        calibration_multiplier: power.calibration_multiplier,
        note: if measured {
            "Power is sourced from live measured telemetry."
        } else if authority == PowerAuthorityKind::WallCalibratedEstimate {
            "Power is modeled from live runtime state with an operator wall-meter calibration."
        } else {
            "Power is modeled from the live dispatcher estimate; it is not a direct wall-meter measurement."
        },
    }
}

/// Build a history sample from the latest daemon state snapshot.
pub fn sample_from_runtime(
    timestamp_s: u64,
    state: &MinerState,
    power: &LivePowerEstimate,
) -> HistorySample {
    let temp_c = state
        .chains
        .iter()
        .map(|chain| chain.temp_c)
        .reduce(f32::max)
        .unwrap_or(0.0);
    let power_projection = (power);

    HistorySample {
        timestamp_s,
        hashrate_ghs: state.hashrate_ghs,
        power_watts: power_projection.power_watts,
        power_source: power_projection.source,
        power_source_detail: power_projection.source_detail.to_string(),
        live_power_available: power_projection.live_power_available,
        power_modeled: power_projection.modeled,
        power_calibrated: power_projection.calibrated,
        power_calibration_multiplier: power_projection.calibration_multiplier,
        power_note: power_projection.note.to_string(),
        temp_c,
        fan_pwm: state.fans.pwm,
        fan_rpm: state.fans.rpm,
        accepted: state.accepted,
        rejected: state.rejected,
        pool_status: state.pool.status.clone(),
    }
}

/// Serialize samples into the JSON shape exposed by the REST API.
pub fn serialize_for_api(samples: &[HistorySample]) -> Vec<serde_json::Value> {
    samples
        .iter()
        .filter_map(|sample| serde_json::to_value(sample).ok())
        .collect()
}

/// Result of a PIC voltage readback verification.
#[derive(Debug, Clone)]
pub struct VoltageReadback {
    /// PIC I2C address (0x55, 0x56, or 0x57 on S9).
    pub pic_addr: u8,
    /// Target voltage requested by the autotuner (millivolts).
    pub target_mv: u32,
    /// Actual voltage read back from the PIC DAC (millivolts).
    pub actual_mv: u32,
    /// Signed delta: actual - target (millivolts).
    pub delta_mv: i32,
}

/// Check voltage readback for all PICs and warn if mismatch exceeds threshold.
///
/// Called by the daemon after the autotuner sets a new voltage. The autotuner
/// itself cannot read PIC voltage (it has no I2C access — that lives in the
/// HAL crate). Instead, the autotuner sends a `FreqCommand::VerifyVoltage`
/// signal, and the daemon calls this function with the HAL's I2C read results.
///
/// # Arguments
/// * `pic_addrs` — Slice of PIC I2C addresses to verify (one per chain).
/// * `target_voltage_mv` — The voltage the autotuner requested (millivolts).
/// * `i2c_read_fn` — Closure that reads the actual voltage from a PIC address.
///   Returns `Some(mv)` on success, `None` if the PIC is unreachable.
/// * `threshold_mv` — Maximum acceptable delta before logging a warning.
pub fn check_voltage_readback(
    pic_addrs: &[u8],
    target_voltage_mv: u32,
    i2c_read_fn: impl Fn(u8) -> Option<u32>,
    threshold_mv: u32,
) -> Vec<VoltageReadback> {
    let mut results = Vec::new();
    for &addr in pic_addrs {
        if let Some(actual) = i2c_read_fn(addr) {
            let delta = actual as i32 - target_voltage_mv as i32;
            if delta.unsigned_abs() > threshold_mv {
                tracing::warn!(
                    pic_addr = format!("0x{:02X}", addr),
                    target_mv = target_voltage_mv,
                    actual_mv = actual,
                    delta_mv = delta,
                    "Voltage readback mismatch exceeds {}mV threshold",
                    threshold_mv,
                );
            } else {
                tracing::debug!(
                    pic_addr = format!("0x{:02X}", addr),
                    actual_mv = actual,
                    delta_mv = delta,
                    "Voltage readback OK"
                );
            }
            results.push(VoltageReadback {
                pic_addr: addr,
                target_mv: target_voltage_mv,
                actual_mv: actual,
                delta_mv: delta,
            });
        } else {
            tracing::warn!(
                pic_addr = format!("0x{:02X}", addr),
                "Voltage readback failed — PIC unreachable"
            );
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_sample(timestamp_s: u64) -> HistorySample {
        HistorySample {
            timestamp_s,
            hashrate_ghs: 14000.0,
            power_watts: 1350.0,
            power_source: "pmbus".to_string(),
            power_source_detail: "pmbus_measured".to_string(),
            live_power_available: true,
            power_modeled: false,
            power_calibrated: false,
            power_calibration_multiplier: None,
            power_note: "Power is sourced from live measured telemetry.".to_string(),
            temp_c: 55.0,
            fan_pwm: 30,
            fan_rpm: 3200,
            accepted: 0,
            rejected: 0,
            pool_status: "alive".to_string(),
        }
    }

    fn sample_state() -> MinerState {
        let mut state = MinerState::empty(dcentrald_api::OperatingMode::Standard);
        state.hashrate_ghs = 14_000.0;
        state.fans.pwm = 30;
        state.fans.rpm = 3200;
        state.accepted = 42;
        state.rejected = 1;
        state.pool.status = "alive".to_string();
        state
    }

    fn power_estimate(
        board_watts: f64,
        wall_watts: f64,
        source: &str,
        calibrated: bool,
    ) -> LivePowerEstimate {
        LivePowerEstimate {
            board_watts,
            wall_watts,
            source: source.to_string(),
            calibrated,
            calibration_multiplier: calibrated.then_some(1.08),
            ..Default::default()
        }
    }

    #[test]
    fn test_new_buffer_is_empty() {
        let buf = HistoryBuffer::new();
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(buf.samples().is_empty());
    }

    #[test]
    fn test_push_and_retrieve() {
        let buf = HistoryBuffer::new();
        let mut sample = history_sample(1000);
        sample.accepted = 42;
        sample.rejected = 1;
        buf.push(sample.clone());
        assert_eq!(buf.len(), 1);
        assert!(!buf.is_empty());

        let samples = buf.samples();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].timestamp_s, 1000);
        assert_eq!(samples[0].accepted, 42);
    }

    #[test]
    fn test_ring_buffer_eviction() {
        let buf = HistoryBuffer::new();
        // Push MAX_SAMPLES + 10 items
        for i in 0..(MAX_SAMPLES + 10) {
            buf.push(history_sample(i as u64));
        }
        assert_eq!(buf.len(), MAX_SAMPLES);

        // Oldest sample should be timestamp 10 (first 10 evicted)
        let samples = buf.samples();
        assert_eq!(samples[0].timestamp_s, 10);
        assert_eq!(
            samples[MAX_SAMPLES - 1].timestamp_s,
            (MAX_SAMPLES + 9) as u64
        );
    }

    #[test]
    fn test_clone_is_shared() {
        let buf1 = HistoryBuffer::new();
        let buf2 = buf1.clone();
        buf1.push(history_sample(999));
        // buf2 should see the same data (shared Arc)
        assert_eq!(buf2.len(), 1);
        assert_eq!(buf2.samples()[0].timestamp_s, 999);
    }

    #[test]
    fn test_voltage_readback_ok() {
        let results = check_voltage_readback(
            &[0x55, 0x56],
            9100,
            |addr| match addr {
                0x55 => Some(9095),
                0x56 => Some(9105),
                _ => None,
            },
            50, // 50 mV threshold
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].delta_mv, -5);
        assert_eq!(results[1].delta_mv, 5);
    }

    #[test]
    fn test_voltage_readback_mismatch() {
        let results = check_voltage_readback(
            &[0x55],
            9100,
            |_| Some(8900), // 200 mV off
            50,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].delta_mv, -200);
        assert!(results[0].delta_mv.unsigned_abs() > 50);
    }

    #[test]
    fn test_voltage_readback_unreachable() {
        let results = check_voltage_readback(
            &[0x55],
            9100,
            |_| None, // PIC unreachable
            50,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("dcent_history_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_history.json");

        // Save
        let buf = HistoryBuffer::new();
        for i in 0..5 {
            let mut sample = history_sample(1000 + i);
            sample.accepted = i;
            buf.push(sample);
        }
        buf.save(&path).unwrap();
        let staging_leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("test_history.json.tmp")
            })
            .map(|entry| entry.path())
            .collect();
        assert!(
            staging_leftovers.is_empty(),
            "history save left staging tempfiles behind: {staging_leftovers:?}"
        );

        // Load
        let loaded = HistoryBuffer::load(&path);
        assert_eq!(loaded.len(), 5);
        let samples = loaded.samples();
        assert_eq!(samples[0].timestamp_s, 1000);
        assert_eq!(samples[4].accepted, 4);

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn history_save_uses_fsyncing_atomic_write_helper() {
        let implementation = include_str!("history.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("implementation prefix must exist");
        assert!(
            implementation.contains("atomic_io::atomic_write(path, json)?;"),
            "HistoryBuffer::save must use the fsyncing atomic write helper"
        );
        assert!(
            !implementation.contains("std::fs::write(&tmp")
                && !implementation.contains("std::fs::rename(&tmp"),
            "HistoryBuffer::save must not regress to non-fsynced write+rename"
        );
    }

    #[test]
    fn sample_from_runtime_suppresses_unavailable_power() {
        let sample = sample_from_runtime(
            1234,
            &sample_state(),
            &power_estimate(900.0, 0.0, "estimated", false),
        );

        assert_eq!(sample.power_watts, 0.0);
        assert_eq!(sample.power_source, "unavailable");
        assert_eq!(sample.power_source_detail, "live_power_unavailable");
        assert!(!sample.live_power_available);
        assert!(!sample.power_modeled);
        assert!(!sample.power_calibrated);
        assert!(sample.power_calibration_multiplier.is_none());
        assert!(sample.power_note.contains("has not published positive"));
    }

    #[test]
    fn sample_from_runtime_marks_measured_pmbus_power() {
        let sample = sample_from_runtime(
            1234,
            &sample_state(),
            &power_estimate(1180.0, 1320.0, "pmbus", false),
        );

        assert_eq!(sample.power_watts, 1320.0);
        assert_eq!(sample.power_source, "pmbus");
        assert_eq!(sample.power_source_detail, "pmbus_measured");
        assert!(sample.live_power_available);
        assert!(!sample.power_modeled);
        assert!(!sample.power_calibrated);
        assert!(sample.power_calibration_multiplier.is_none());
    }

    #[test]
    fn sample_from_runtime_marks_calibrated_modeled_power() {
        let sample = sample_from_runtime(
            1234,
            &sample_state(),
            &power_estimate(1180.0, 1320.0, "estimated", true),
        );

        assert_eq!(sample.power_watts, 1320.0);
        assert_eq!(sample.power_source, "estimated");
        assert_eq!(sample.power_source_detail, "wall_calibrated_estimate");
        assert!(sample.live_power_available);
        assert!(sample.power_modeled);
        assert!(sample.power_calibrated);
        assert_eq!(sample.power_calibration_multiplier, Some(1.08));
    }

    #[test]
    fn serialize_for_api_exposes_power_provenance() {
        let sample = sample_from_runtime(
            1234,
            &sample_state(),
            &power_estimate(1180.0, 1320.0, "adc", false),
        );
        let serialized = serialize_for_api(&[sample]);

        assert_eq!(serialized.len(), 1);
        assert_eq!(serialized[0]["power_watts"], serde_json::json!(1320.0));
        assert_eq!(serialized[0]["power_source"], serde_json::json!("adc"));
        assert_eq!(
            serialized[0]["power_source_detail"],
            serde_json::json!("adc_measured")
        );
        assert_eq!(
            serialized[0]["live_power_available"],
            serde_json::json!(true)
        );
        assert_eq!(serialized[0]["power_modeled"], serde_json::json!(false));
    }

    #[test]
    fn legacy_history_json_loads_with_unprovenanced_power_marker() {
        let json = r#"[{
            "timestamp": 1000,
            "hashrate_ghs": 14000.0,
            "power_watts": 1350.0,
            "temp_c": 55.0,
            "fan_pwm": 30,
            "fan_rpm": 3200,
            "accepted": 42,
            "rejected": 1,
            "pool_status": "alive"
        }]"#;
        let samples: Vec<HistorySample> = serde_json::from_str(json).unwrap();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].power_watts, 1350.0);
        assert_eq!(samples[0].power_source, "legacy_unprovenanced");
        assert_eq!(
            samples[0].power_source_detail,
            "legacy_history_without_provenance"
        );
        assert!(!samples[0].live_power_available);
        assert!(samples[0].power_modeled);
        assert!(samples[0]
            .power_note
            .contains("predates history power provenance"));
    }

    #[test]
    fn test_load_nonexistent_file() {
        let buf = HistoryBuffer::load(Path::new("/tmp/nonexistent_dcent_history_xyz.json"));
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_load_corrupted_file() {
        let dir = std::env::temp_dir().join("dcent_history_corrupt_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("corrupt.json");
        std::fs::write(&path, "this is not json{{{").unwrap();

        let buf = HistoryBuffer::load(&path);
        assert_eq!(buf.len(), 0); // Should gracefully return empty

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
