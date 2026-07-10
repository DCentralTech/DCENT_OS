//! Tuning telemetry recording and export.
//!
//! Logs every measurement window as time-series data for debugging and
//! external AI optimization via MCP. Per-chip nonces, errors, frequencies,
//! and tuner decisions are recorded. Keeps the last 3 tuning runs.
//! Exportable via the dcentrald API as JSON or CSV.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;

/// Maximum number of tuning runs to keep.
const MAX_RUNS: usize = 3;

/// Maximum number of snapshots per run (prevent unbounded memory on long runs).
const MAX_SNAPSHOTS_PER_RUN: usize = 10_000;

/// A single telemetry sample for one measurement window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySample {
    /// Seconds elapsed since the start of this tuning run.
    pub elapsed_s: f64,
    /// Chain ID.
    pub chain_id: u8,
    /// Per-chip data for this window.
    pub chips: Vec<ChipTelemetry>,
    /// Board temperature at time of sample, if available.
    pub board_temp_c: Option<f32>,
    /// Tuner state at time of sample.
    pub tuner_state: String,
    /// Current ASIC difficulty.
    pub difficulty: u32,
}

/// Per-chip telemetry within a single measurement window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipTelemetry {
    /// Chip index.
    pub chip_index: u8,
    /// Valid nonces produced this window.
    pub nonces: u64,
    /// Hardware errors this window.
    pub errors: u64,
    /// Current frequency (MHz).
    pub freq_mhz: u16,
    /// Tuner decision for this chip this window (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

/// A complete tuning run recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningRun {
    /// Unix timestamp when the run started.
    pub started_at: u64,
    /// Duration of the run in seconds.
    pub duration_s: f64,
    /// Whether the run completed successfully.
    pub completed: bool,
    /// All telemetry samples recorded during the run.
    pub samples: Vec<TelemetrySample>,
}

/// Live-exportable telemetry state for API/WebSocket consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryExportState {
    pub live_runtime: bool,
    pub recording: bool,
    pub runs: Vec<TuningRun>,
    pub last_update_s: u64,
    pub message: String,
}

impl Default for TelemetryExportState {
    fn default() -> Self {
        Self {
            live_runtime: false,
            recording: false,
            runs: Vec::new(),
            last_update_s: 0,
            message: "Autotuner runtime telemetry unavailable".to_string(),
        }
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Telemetry recorder that accumulates samples during tuning.
pub struct TelemetryRecorder {
    /// Historical tuning runs (most recent last). Capped at MAX_RUNS.
    runs: VecDeque<TuningRun>,
    /// Current active run (if tuning is in progress).
    current_run: Option<ActiveRun>,
}

struct ActiveRun {
    started_at: u64,
    start_instant: Instant,
    samples: Vec<TelemetrySample>,
}

impl TelemetryRecorder {
    /// Create a new telemetry recorder.
    pub fn new() -> Self {
        Self {
            runs: VecDeque::with_capacity(MAX_RUNS + 1),
            current_run: None,
        }
    }

    /// Begin recording a new tuning run.
    pub fn start_run(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.current_run = Some(ActiveRun {
            started_at: now,
            start_instant: Instant::now(),
            samples: Vec::new(),
        });
    }

    /// Record a telemetry sample.
    pub fn record_sample(&mut self, sample: TelemetrySample) {
        if let Some(ref mut run) = self.current_run {
            if run.samples.len() < MAX_SNAPSHOTS_PER_RUN {
                run.samples.push(sample);
            }
        }
    }

    /// Finish the current tuning run.
    pub fn finish_run(&mut self, completed: bool) {
        if let Some(run) = self.current_run.take() {
            let duration_s = run.start_instant.elapsed().as_secs_f64();
            let tuning_run = TuningRun {
                started_at: run.started_at,
                duration_s,
                completed,
                samples: run.samples,
            };

            self.runs.push_back(tuning_run);

            // Cap at MAX_RUNS
            while self.runs.len() > MAX_RUNS {
                self.runs.pop_front();
            }
        }
    }

    /// Get elapsed seconds since current run started (for sample timestamping).
    pub fn elapsed_s(&self) -> f64 {
        self.current_run
            .as_ref()
            .map(|r| r.start_instant.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Whether a run is currently active.
    pub fn is_recording(&self) -> bool {
        self.current_run.is_some()
    }

    /// Get the last N completed tuning runs for export.
    pub fn completed_runs(&self) -> &VecDeque<TuningRun> {
        &self.runs
    }

    /// Get the most recent completed run.
    pub fn last_run(&self) -> Option<&TuningRun> {
        self.runs.back()
    }

    /// Export all runs as JSON string.
    pub fn export_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(&self.runs.iter().collect::<Vec<_>>())
    }

    /// Export the most recent run as CSV string.
    ///
    /// Format: elapsed_s,chain_id,chip_index,nonces,errors,freq_mhz,board_temp_c,state,difficulty
    pub fn export_csv(&self) -> String {
        export_runs_csv(&self.runs.iter().cloned().collect::<Vec<_>>())
    }

    /// Export the current telemetry state for watch-channel consumers.
    pub fn export_state(&self) -> TelemetryExportState {
        let runs: Vec<TuningRun> = self.runs.iter().cloned().collect();
        let recording = self.current_run.is_some();
        let message = if recording {
            "Autotuner characterization telemetry recording in progress".to_string()
        } else if runs.is_empty() {
            "No completed autotuner characterization telemetry runs captured yet".to_string()
        } else {
            format!(
                "{} completed autotuner characterization run(s) available",
                runs.len()
            )
        };

        TelemetryExportState {
            live_runtime: true,
            recording,
            runs,
            last_update_s: now_unix_s(),
            message,
        }
    }
}

/// Export the most recent run in CSV form from a captured telemetry state.
pub fn export_runs_csv(runs: &[TuningRun]) -> String {
    let mut csv = String::from(
        "elapsed_s,chain_id,chip_index,nonces,errors,freq_mhz,board_temp_c,state,difficulty\n",
    );

    if let Some(run) = runs.last() {
        for sample in &run.samples {
            let temp_str = sample
                .board_temp_c
                .map(|t| format!("{:.1}", t))
                .unwrap_or_default();

            for chip in &sample.chips {
                csv.push_str(&format!(
                    "{:.3},{},{},{},{},{},{},{},{}\n",
                    sample.elapsed_s,
                    sample.chain_id,
                    chip.chip_index,
                    chip.nonces,
                    chip.errors,
                    chip.freq_mhz,
                    temp_str,
                    sample.tuner_state,
                    sample.difficulty,
                ));
            }
        }
    }

    csv
}

impl Default for TelemetryRecorder {
    fn default() -> Self {
        Self::new()
    }
}

// --- Item 16: Real-Time Efficiency Dashboard Feed ---

/// Per-chip efficiency snapshot for the real-time dashboard.
///
/// Published via a broadcast channel every 5 seconds by the background monitor.
/// The dcentrald-api crate subscribes and pushes to WebSocket clients for
/// live 189-chip heat map visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfficiencySnapshot {
    /// Timestamp (Unix epoch seconds).
    pub timestamp: u64,
    /// Per-chain snapshots.
    pub chains: Vec<ChainEfficiencySnapshot>,
    /// Total estimated power (watts).
    pub total_power_w: f64,
    /// Total estimated hashrate (GH/s).
    pub total_hashrate_ghs: f64,
    /// Total efficiency (J/TH).
    pub total_efficiency_jth: f64,
    /// Provenance basis of the watts in this snapshot. The per-chip and total
    /// watts are ALWAYS model-derived (`PowerModel` from voltage/frequency),
    /// never a live wall-meter measurement — even when the snapshot is
    /// published from the live runtime monitor. That is why a `source:
    /// "runtime"` freshness label is NOT a "measured" signal; consumers must
    /// read this to know the watts are modeled. Carries the
    /// [`crate::power_budget::PowerAuthorityKind`] wire label.
    #[serde(default = "default_efficiency_power_basis")]
    pub power_basis: String,
    /// `true` when the watts above are modeled (the normal, and currently
    /// only, case for this snapshot). Consumers must not treat a modeled
    /// snapshot as a measured power reading.
    #[serde(default = "default_efficiency_modeled")]
    pub modeled: bool,
}

/// Serde default for [`EfficiencySnapshot::power_basis`] on older payloads
/// that predate the field — the snapshot has always been model-derived.
fn default_efficiency_power_basis() -> String {
    crate::power_budget::PowerAuthorityKind::Estimated
        .as_str()
        .to_string()
}

/// Serde default for [`EfficiencySnapshot::modeled`] — modeled by construction.
fn default_efficiency_modeled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptedWorkSignal {
    pub window_s: u64,
    pub accepted_share_count: u64,
    /// Compatibility alias for accepted pool target difficulty work.
    /// This is not achieved/lucky share difficulty.
    pub accepted_difficulty_sum: f64,
    #[serde(default)]
    pub accepted_pool_target_difficulty_sum: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_difficulty_sum: Option<f64>,
    pub estimated_wall_energy_kwh: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_shares_per_kwh: Option<f64>,
    /// Compatibility alias for pool target difficulty work per kWh.
    /// This is not achieved/lucky share difficulty per kWh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_difficulty_per_kwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_pool_target_difficulty_per_kwh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_difficulty_per_kwh: Option<f64>,
    #[serde(default)]
    pub difficulty_source: String,
    pub power_source: String,
    pub calibrated: bool,
}

/// Per-chain efficiency data for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainEfficiencySnapshot {
    /// Chain ID.
    pub chain_id: u8,
    /// Voltage (mV).
    pub voltage_mv: u16,
    /// Board temperature (degrees C), if available.
    pub board_temp_c: Option<f32>,
    /// Per-chip data.
    pub chips: Vec<ChipEfficiency>,
}

/// Per-chip efficiency data point for the dashboard heat map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipEfficiency {
    /// Chip index.
    pub chip_index: u8,
    /// Current operating frequency (MHz).
    pub freq_mhz: u16,
    /// Estimated power (watts).
    pub power_w: f64,
    /// Hashrate contribution (GH/s).
    pub hashrate_ghs: f64,
    /// Efficiency (J/TH).
    pub efficiency_jth: f64,
    /// Health score (0.0-1.0).
    pub health_score: f64,
    /// Silicon grade.
    pub grade: String,
    /// Whether this chip is thermally derated.
    pub thermally_derated: bool,
    /// Whether this chip is masked (dead).
    pub masked: bool,
}

/// Builder for EfficiencySnapshot from current tuner state.
///
/// Called by the background monitor every 5 seconds to produce a snapshot
/// for the dashboard WebSocket feed.
pub fn build_efficiency_snapshot(
    profiles: &std::collections::HashMap<u8, crate::profile::TuningProfile>,
    power_scale: f64,
) -> EfficiencySnapshot {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut chains = Vec::new();
    let mut total_power = 0.0_f64;
    let mut total_hashrate = 0.0_f64;
    let mut control_board_added = false;

    for profile in profiles.values() {
        let chip_id = crate::chip_id_from_type(&profile.chip_type).unwrap_or(0x1387);
        let power_model =
            crate::power_budget::PowerModel::new_for_chip(chip_id).with_power_scale(power_scale);
        let voltage_mv = profile.optimal_voltage_mv.unwrap_or(profile.voltage_mv);
        let voltage_v = voltage_mv as f64 / 1000.0;

        let mut chip_data = Vec::new();

        for chip in &profile.chips {
            let power = power_model.chip_power_w(voltage_v, chip.operating_mhz);
            let hashrate =
                crate::chip_geometry::chip_hashrate_ghs_for_chip(chip_id, chip.operating_mhz);
            let hashrate_ths = hashrate / 1000.0;
            let efficiency = if hashrate_ths > 0.0 {
                power / hashrate_ths
            } else {
                0.0
            };

            total_power += power;
            total_hashrate += hashrate;

            chip_data.push(ChipEfficiency {
                chip_index: chip.chip_index,
                freq_mhz: chip.operating_mhz,
                power_w: power,
                hashrate_ghs: hashrate,
                efficiency_jth: efficiency,
                health_score: 1.0, // Updated by health tracker externally
                grade: format!("{}", chip.grade),
                thermally_derated: false, // Updated by monitor externally
                masked: chip.operating_mhz == 0,
            });
        }

        total_power += power_model.static_per_chain_w();
        if !control_board_added {
            total_power += power_model.control_board_w();
            control_board_added = true;
        }

        chains.push(ChainEfficiencySnapshot {
            chain_id: profile.chain_id,
            voltage_mv,
            board_temp_c: profile.equilibrium_temp_c,
            chips: chip_data,
        });
    }

    let total_hashrate_ths = total_hashrate / 1000.0;
    let total_efficiency = if total_hashrate_ths > 0.0 {
        total_power / total_hashrate_ths
    } else {
        0.0
    };

    // The per-chip watts above come from `PowerModel::chip_power_w` (a
    // voltage/frequency model), so this snapshot's power is always modeled.
    // Stamp the provenance from the shared authority model rather than leaving
    // a `source: "runtime"` freshness label to imply it is measured.
    let power_basis = crate::power_budget::PowerAuthorityKind::Estimated;

    EfficiencySnapshot {
        timestamp: now,
        chains,
        total_power_w: total_power,
        total_hashrate_ghs: total_hashrate,
        total_efficiency_jth: total_efficiency,
        power_basis: power_basis.as_str().to_string(),
        modeled: !power_basis.is_measured(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorder_lifecycle() {
        let mut recorder = TelemetryRecorder::new();

        recorder.start_run();
        assert!(recorder.is_recording());

        recorder.record_sample(TelemetrySample {
            elapsed_s: 0.0,
            chain_id: 6,
            chips: vec![ChipTelemetry {
                chip_index: 0,
                nonces: 50,
                errors: 1,
                freq_mhz: 650,
                decision: None,
            }],
            board_temp_c: Some(55.0),
            tuner_state: "Characterizing".to_string(),
            difficulty: 256,
        });

        recorder.finish_run(true);
        assert!(!recorder.is_recording());

        let runs = recorder.completed_runs();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].completed);
        assert_eq!(runs[0].samples.len(), 1);
        assert_eq!(runs[0].samples[0].chips[0].nonces, 50);
    }

    #[test]
    fn test_recorder_caps_at_max_runs() {
        let mut recorder = TelemetryRecorder::new();

        for i in 0..5 {
            recorder.start_run();
            recorder.record_sample(TelemetrySample {
                elapsed_s: i as f64,
                chain_id: 6,
                chips: vec![],
                board_temp_c: None,
                tuner_state: "Test".to_string(),
                difficulty: 256,
            });
            recorder.finish_run(true);
        }

        assert_eq!(recorder.completed_runs().len(), MAX_RUNS);
    }

    #[test]
    fn test_csv_export() {
        let mut recorder = TelemetryRecorder::new();
        recorder.start_run();
        recorder.record_sample(TelemetrySample {
            elapsed_s: 1.5,
            chain_id: 6,
            chips: vec![
                ChipTelemetry {
                    chip_index: 0,
                    nonces: 100,
                    errors: 0,
                    freq_mhz: 650,
                    decision: None,
                },
                ChipTelemetry {
                    chip_index: 1,
                    nonces: 95,
                    errors: 2,
                    freq_mhz: 625,
                    decision: Some("backoff".to_string()),
                },
            ],
            board_temp_c: Some(55.0),
            tuner_state: "Tuned".to_string(),
            difficulty: 256,
        });
        recorder.finish_run(true);

        let csv = recorder.export_csv();
        assert!(csv.contains("elapsed_s,chain_id,chip_index"));
        assert!(csv.contains("1.500,6,0,100,0,650,55.0,Tuned,256"));
        assert!(csv.contains("1.500,6,1,95,2,625,55.0,Tuned,256"));
    }

    #[test]
    fn test_export_state_tracks_recording_and_completed_runs() {
        let mut recorder = TelemetryRecorder::new();

        let initial = recorder.export_state();
        assert!(initial.live_runtime);
        assert!(!initial.recording);
        assert!(initial.runs.is_empty());
        assert!(initial
            .message
            .contains("No completed autotuner characterization"));

        recorder.start_run();
        recorder.record_sample(TelemetrySample {
            elapsed_s: 0.5,
            chain_id: 6,
            chips: vec![ChipTelemetry {
                chip_index: 0,
                nonces: 10,
                errors: 0,
                freq_mhz: 650,
                decision: None,
            }],
            board_temp_c: Some(54.0),
            tuner_state: "Characterizing".to_string(),
            difficulty: 256,
        });
        let active = recorder.export_state();
        assert!(active.recording);
        assert!(active.message.contains("recording in progress"));

        recorder.finish_run(true);
        let completed = recorder.export_state();
        assert!(!completed.recording);
        assert_eq!(completed.runs.len(), 1);
        assert!(completed
            .message
            .contains("1 completed autotuner characterization run"));
    }

    #[test]
    fn test_export_runs_csv_uses_latest_run() {
        let runs = vec![
            TuningRun {
                started_at: 1,
                duration_s: 1.0,
                completed: true,
                samples: vec![TelemetrySample {
                    elapsed_s: 0.2,
                    chain_id: 6,
                    chips: vec![ChipTelemetry {
                        chip_index: 0,
                        nonces: 1,
                        errors: 0,
                        freq_mhz: 600,
                        decision: None,
                    }],
                    board_temp_c: Some(50.0),
                    tuner_state: "Characterizing".to_string(),
                    difficulty: 256,
                }],
            },
            TuningRun {
                started_at: 2,
                duration_s: 2.0,
                completed: true,
                samples: vec![TelemetrySample {
                    elapsed_s: 1.0,
                    chain_id: 7,
                    chips: vec![ChipTelemetry {
                        chip_index: 3,
                        nonces: 77,
                        errors: 4,
                        freq_mhz: 625,
                        decision: Some("backoff".to_string()),
                    }],
                    board_temp_c: None,
                    tuner_state: "Verifying".to_string(),
                    difficulty: 512,
                }],
            },
        ];

        let csv = export_runs_csv(&runs);
        assert!(csv.contains("1.000,7,3,77,4,625,,Verifying,512"));
        assert!(!csv.contains("0.200,6,0,1,0,600,50.0,Characterizing,256"));
    }

    #[test]
    fn test_efficiency_snapshot_builder() {
        use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
        use std::collections::HashMap;

        let chips: Vec<ChipProfile> = (0..3)
            .map(|i| ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: 700,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();

        let stats = TuningProfile::compute_stats(&chips, 15.0);

        let mut profiles = HashMap::new();
        profiles.insert(
            6u8,
            TuningProfile {
                version: 2,
                chip_type: "BM1387".to_string(),
                chain_id: 6,
                chip_count: 3,
                voltage_mv: 9100,
                tuned_at: "0".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: Some(9000),
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: Some(55.0),
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips,
                stats,
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        let snapshot = build_efficiency_snapshot(&profiles, 1.0);

        assert_eq!(snapshot.chains.len(), 1);
        assert_eq!(snapshot.chains[0].chain_id, 6);
        assert_eq!(snapshot.chains[0].chips.len(), 3);
        assert!(
            snapshot.total_power_w > 0.0,
            "Total power should be positive"
        );
        assert!(
            snapshot.total_hashrate_ghs > 0.0,
            "Total hashrate should be positive"
        );
        assert!(
            snapshot.total_efficiency_jth > 0.0,
            "Efficiency should be positive"
        );

        // Each chip at 650 MHz ≈ 650 * 0.114 ≈ 74.1 GH/s
        let expected_hashrate = 3.0 * 650.0 * 0.114;
        assert!(
            (snapshot.total_hashrate_ghs - expected_hashrate).abs() < 1.0,
            "Expected ~{:.1} GH/s, got {:.1}",
            expected_hashrate,
            snapshot.total_hashrate_ghs,
        );
    }

    #[test]
    fn test_efficiency_snapshot_uses_profile_chip_family() {
        use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
        use std::collections::HashMap;

        let chips = vec![ChipProfile {
            chip_index: 0,
            max_stable_mhz: 525,
            operating_mhz: 500,
            grade: ChipGrade::B,
            error_rate: 0.001,
            nonces_counted: 100,
            vf_curve: None,
            thermal_max_stable_mhz: None,
        }];
        let stats = TuningProfile::compute_stats(&chips, 15.0);

        let mut profiles = HashMap::new();
        profiles.insert(
            7u8,
            TuningProfile {
                version: 2,
                chip_type: "BM1398".to_string(),
                chain_id: 7,
                chip_count: 1,
                voltage_mv: 13800,
                tuned_at: "0".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: Some(13800),
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: Some(58.0),
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips,
                stats,
                // W13.C3: SKU + flag denormalisation. Test fixture default.
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        let snapshot = build_efficiency_snapshot(&profiles, 1.0);
        let expected_hashrate = crate::chip_geometry::chip_hashrate_ghs_for_chip(0x1398, 500);

        assert!(
            (snapshot.total_hashrate_ghs - expected_hashrate).abs() < 0.01,
            "expected BM1398 hashrate model, got {:.3} GH/s",
            snapshot.total_hashrate_ghs,
        );
    }

    #[test]
    fn test_efficiency_snapshot_serialization() {
        let snapshot = EfficiencySnapshot {
            timestamp: 1710000000,
            chains: vec![ChainEfficiencySnapshot {
                chain_id: 6,
                voltage_mv: 9100,
                board_temp_c: Some(55.0),
                chips: vec![ChipEfficiency {
                    chip_index: 0,
                    freq_mhz: 650,
                    power_w: 6.24,
                    hashrate_ghs: 74.1,
                    efficiency_jth: 84.0,
                    health_score: 0.95,
                    grade: "B".to_string(),
                    thermally_derated: false,
                    masked: false,
                }],
            }],
            total_power_w: 1350.0,
            total_hashrate_ghs: 14000.0,
            total_efficiency_jth: 96.0,
            power_basis: "estimated".to_string(),
            modeled: true,
        };

        let json = serde_json::to_string(&snapshot).expect("serialize failed");
        // POWER-PROVENANCE PIN: the snapshot must carry `power_basis`/`modeled`
        // on the wire so a "runtime"-sourced snapshot is never mistaken for a
        // measured wattage.
        let value: serde_json::Value = serde_json::from_str(&json).expect("value");
        assert_eq!(value["power_basis"], "estimated");
        assert_eq!(value["modeled"], true);

        let deserialized: EfficiencySnapshot =
            serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.chains[0].chips[0].freq_mhz, 650);
        assert!((deserialized.total_power_w - 1350.0).abs() < 0.1);
        assert_eq!(deserialized.power_basis, "estimated");
        assert!(deserialized.modeled);
    }

    #[test]
    fn test_efficiency_snapshot_builder_marks_modeled_power_basis() {
        use crate::power_budget::PowerAuthorityKind;
        use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
        use std::collections::HashMap;

        let chips: Vec<ChipProfile> = (0..2)
            .map(|i| ChipProfile {
                chip_index: i as u8,
                max_stable_mhz: 700,
                operating_mhz: 650,
                grade: ChipGrade::B,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            })
            .collect();
        let stats = TuningProfile::compute_stats(&chips, 15.0);

        let mut profiles = HashMap::new();
        profiles.insert(
            6u8,
            TuningProfile {
                version: 2,
                chip_type: "BM1387".to_string(),
                chain_id: 6,
                chip_count: 2,
                voltage_mv: 9100,
                tuned_at: "0".to_string(),
                ambient_temp_c: None,
                optimal_voltage_mv: Some(9000),
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
                equilibrium_temp_c: Some(55.0),
                thermal_refinement_duration_s: None,
                calibrated_c_eff: None,
                chips,
                stats,
                hashboard_sku: None,
                hashboard_sku_flags: None,
            },
        );

        let snapshot = build_efficiency_snapshot(&profiles, 1.0);

        // The builder's watts are model-derived; the provenance must say so and
        // must not classify as a measured authority.
        assert!(snapshot.modeled, "per-chip watts are modeled, not measured");
        assert_eq!(snapshot.power_basis, "estimated");
        assert!(
            !PowerAuthorityKind::from_source(&snapshot.power_basis, false).is_measured(),
            "modeled snapshot must not resolve to a measured authority"
        );
    }
}
