//! Fleet profile export/import for multi-miner deployment.
//!
//! A `FleetProfile` packages tuning data from one miner into a portable format
//! that can be transferred to identical miners (same chip type and count per chain).
//! This enables "tune once, deploy everywhere" workflows for fleet operators.

use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Portable fleet profile — contains tuning data that can be transferred
/// between identical miners (same chip type and count).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetProfile {
    /// Format version.
    pub version: u32,
    /// Chip type (e.g., "BM1387").
    pub chip_type: String,
    /// Number of chips this profile was tuned for (per chain).
    pub chip_count: u8,
    /// Voltage used during tuning (mV).
    pub voltage_mv: u16,
    /// Source miner hostname (for provenance).
    pub source_hostname: String,
    /// When this profile was exported (Unix epoch seconds).
    pub exported_at: u64,
    /// Per-chain profiles.
    pub chains: Vec<ChainFleetProfile>,
}

/// Per-chain tuning data within a fleet profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainFleetProfile {
    /// Chain ID (e.g., 6, 7, 8 on S9).
    pub chain_id: u8,
    /// Number of chips on this chain.
    pub chip_count: u8,
    /// Per-chip operating frequencies (the key data).
    pub chip_freqs_mhz: Vec<u16>,
    /// Average operating frequency across chips.
    pub avg_freq_mhz: f64,
    /// Minimum operating frequency (weakest chip).
    pub min_freq_mhz: u16,
    /// Maximum operating frequency (strongest chip).
    pub max_freq_mhz: u16,
}

impl FleetProfile {
    /// Export a fleet profile from existing per-chain TuningProfiles.
    ///
    /// The `profiles` map is keyed by chain_id. All profiles must share the same
    /// chip_type; the voltage_mv from the first profile is used.
    pub fn export(profiles: &HashMap<u8, TuningProfile>, hostname: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Extract chip_type, chip_count, voltage from first profile
        let first = profiles.values().next();
        let chip_type = first.map(|p| p.chip_type.clone()).unwrap_or_default();
        let chip_count = first.map(|p| p.chip_count).unwrap_or(0);
        let voltage_mv = first.map(|p| p.voltage_mv).unwrap_or(0);

        // Build per-chain fleet profiles, sorted by chain_id for determinism
        let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
        chain_ids.sort();

        let chains = chain_ids
            .iter()
            .filter_map(|&id| profiles.get(&id))
            .map(|tp| {
                let chip_freqs_mhz: Vec<u16> = tp.chips.iter().map(|c| c.operating_mhz).collect();
                ChainFleetProfile {
                    chain_id: tp.chain_id,
                    chip_count: tp.chip_count,
                    chip_freqs_mhz,
                    avg_freq_mhz: tp.stats.avg_freq_mhz,
                    min_freq_mhz: tp.stats.min_freq_mhz,
                    max_freq_mhz: tp.stats.max_freq_mhz,
                }
            })
            .collect();

        Self {
            version: 1,
            chip_type,
            chip_count,
            voltage_mv,
            source_hostname: hostname.to_string(),
            exported_at: now,
            chains,
        }
    }

    /// Save fleet profile to a JSON file.
    pub fn save(&self, path: &str) -> crate::Result<()> {
        let tmp_path = format!("{}.tmp", path);

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "Failed to create fleet profile directory");
            }
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &json)?;
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        std::fs::rename(&tmp_path, path)?;
        tracing::info!(
            path,
            chains = self.chains.len(),
            chip_type = %self.chip_type,
            source = %self.source_hostname,
            "Fleet profile saved"
        );
        Ok(())
    }

    /// Load fleet profile from a JSON file.
    pub fn load(path: &str) -> crate::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let profile: Self = serde_json::from_str(&json)?;
        tracing::info!(
            path,
            chains = profile.chains.len(),
            chip_type = %profile.chip_type,
            source = %profile.source_hostname,
            "Fleet profile loaded"
        );
        Ok(profile)
    }

    /// Check if this fleet profile is compatible with the current hardware.
    ///
    /// Compatible means: same chip type and same chip count per chain.
    /// `chain_chip_counts` is a slice of (chain_id, chip_count) pairs.
    pub fn is_compatible(&self, chip_type: &str, chain_chip_counts: &[(u8, u8)]) -> bool {
        if self.chip_type != chip_type {
            tracing::info!(
                expected = chip_type,
                profile = %self.chip_type,
                "Fleet profile chip type mismatch"
            );
            return false;
        }

        // Every chain in the fleet profile must have a matching chain in the
        // target hardware with the same chip count.
        for chain in &self.chains {
            let matching = chain_chip_counts
                .iter()
                .find(|&&(id, _)| id == chain.chain_id);
            match matching {
                Some(&(_, count)) if count == chain.chip_count => {}
                Some(&(_, count)) => {
                    tracing::info!(
                        chain_id = chain.chain_id,
                        expected = count,
                        profile = chain.chip_count,
                        "Fleet profile chain chip count mismatch"
                    );
                    return false;
                }
                None => {
                    tracing::info!(
                        chain_id = chain.chain_id,
                        "Fleet profile references chain not present in hardware"
                    );
                    return false;
                }
            }
        }

        true
    }

    /// Convert fleet profile back to TuningProfiles for application.
    ///
    /// Reconstructs per-chip profiles from the stored frequencies. Since the
    /// fleet profile doesn't carry full ChipProfile data (grade, error_rate, etc.),
    /// these are synthesized from the frequency data alone.
    pub fn to_tuning_profiles(&self) -> Vec<TuningProfile> {
        self.chains
            .iter()
            .map(|chain| {
                let chips: Vec<ChipProfile> = chain
                    .chip_freqs_mhz
                    .iter()
                    .enumerate()
                    .map(|(i, &freq)| {
                        // Estimate max_stable by reversing the typical 5% safety margin.
                        // IMPORTANT: This estimate may be wrong for different silicon
                        // generations. The tuner MUST run verification after applying
                        // fleet profiles — if a chip can't sustain the fleet frequency,
                        // it will be caught during thermal refinement and backed off.
                        let max_stable = (freq as f64 / 0.95).round() as u16;
                        let grade = estimate_grade(freq, chain.avg_freq_mhz);
                        ChipProfile {
                            chip_index: i as u8,
                            max_stable_mhz: max_stable,
                            operating_mhz: freq,
                            grade,
                            error_rate: 0.0, // Unknown — must verify on this hardware
                            nonces_counted: 0,
                            thermal_max_stable_mhz: None,
                            vf_curve: None,
                        }
                    })
                    .collect();

                let stats = TuningProfile::compute_stats(&chips, 0.0);

                TuningProfile {
                    version: 1,
                    chip_type: self.chip_type.clone(),
                    chain_id: chain.chain_id,
                    chip_count: chain.chip_count,
                    voltage_mv: self.voltage_mv,
                    tuned_at: self.exported_at.to_string(),
                    ambient_temp_c: None,
                    optimal_voltage_mv: None,
                    estimated_power_w: 0.0,
                    estimated_efficiency_jth: 0.0,
                    equilibrium_temp_c: None,
                    thermal_refinement_duration_s: None,
                    calibrated_c_eff: None,
                    chips,
                    stats,
                    // W13.C3: SKU + flag denormalisation. Fleet imports
                    // pre-date per-SKU envelope tracking; default to None.
                    hashboard_sku: None,
                    hashboard_sku_flags: None,
                }
            })
            .collect()
    }
}

/// Estimate a chip grade from its operating frequency relative to chain average.
///
/// Since fleet profiles don't carry the original grade data, we approximate
/// based on how the chip compares to the chain average (similar logic to the
/// nominal-based grading in profile.rs).
fn estimate_grade(freq_mhz: u16, avg_freq_mhz: f64) -> ChipGrade {
    let diff = freq_mhz as f64 - avg_freq_mhz;
    if diff > 50.0 {
        ChipGrade::A
    } else if diff > -25.0 {
        ChipGrade::B
    } else if diff > -75.0 {
        ChipGrade::C
    } else {
        ChipGrade::D
    }
}

// ---------------------------------------------------------------------------
// Chip Binning Database — Fleet-wide per-position frequency statistics
// ---------------------------------------------------------------------------

use dcentrald_asic::drivers::MinerProfile;

/// Per-position chip frequency statistics from fleet-wide data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipPositionStats {
    /// Chip position index (0-based).
    pub position: u8,
    /// Mean operating frequency across fleet.
    pub mean_freq_mhz: f64,
    /// Standard deviation of operating frequency.
    pub std_dev_mhz: f64,
    /// Minimum observed frequency.
    pub min_freq_mhz: u16,
    /// Maximum observed frequency.
    pub max_freq_mhz: u16,
    /// Number of samples (miners contributing to this position).
    pub sample_count: u32,
    /// Welford's algorithm: running sum of squared differences from the mean.
    /// Internal bookkeeping — not meaningful on its own.
    #[serde(default)]
    m2: f64,
}

/// Fleet-wide chip binning database.
///
/// Aggregates per-chip-position frequency data across multiple miners.
/// New miners can use fleet averages as starting frequencies instead of
/// running full characterization — just verify and fine-tune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipBinningDatabase {
    /// Chip type (e.g., "BM1387").
    pub chip_type: String,
    /// Chips per chain.
    pub chip_count: u8,
    /// Per-position statistics.
    pub positions: Vec<ChipPositionStats>,
    /// Total number of miners contributing to this database.
    pub miner_count: u32,
    /// Last updated timestamp (Unix epoch seconds).
    pub updated_at: u64,
}

impl ChipBinningDatabase {
    /// Create an empty chip binning database for the given chip type and count.
    pub fn new(chip_type: &str, chip_count: u8) -> Self {
        let positions = (0..chip_count)
            .map(|i| ChipPositionStats {
                position: i,
                mean_freq_mhz: 0.0,
                std_dev_mhz: 0.0,
                min_freq_mhz: u16::MAX,
                max_freq_mhz: 0,
                sample_count: 0,
                m2: 0.0,
            })
            .collect();

        Self {
            chip_type: chip_type.to_string(),
            chip_count,
            positions,
            miner_count: 0,
            updated_at: 0,
        }
    }

    /// Ingest a fleet profile, updating running statistics for each chip position.
    ///
    /// Uses Welford's online algorithm for numerically stable incremental
    /// mean and variance computation. Each chain in the fleet profile contributes
    /// one sample per chip position (fleet profiles from multi-chain miners
    /// contribute one sample per chain, all mapped to the same position indices).
    pub fn ingest_fleet_profile(&mut self, fleet: &FleetProfile) {
        if fleet.chip_type != self.chip_type {
            tracing::warn!(
                expected = %self.chip_type,
                got = %fleet.chip_type,
                "Chip type mismatch in fleet ingestion, skipping"
            );
            return;
        }

        let mut any_ingested = false;

        for chain in &fleet.chains {
            for (i, &freq) in chain.chip_freqs_mhz.iter().enumerate() {
                if i >= self.positions.len() {
                    // More chips on this chain than our database tracks — skip extras
                    continue;
                }

                let stats = &mut self.positions[i];

                // Welford's online algorithm:
                // n += 1
                // delta = x - old_mean
                // new_mean = old_mean + delta / n
                // delta2 = x - new_mean
                // M2 += delta * delta2
                // variance = M2 / n  (population variance)
                stats.sample_count += 1;
                let n = stats.sample_count as f64;
                let x = freq as f64;

                let delta = x - stats.mean_freq_mhz;
                stats.mean_freq_mhz += delta / n;
                let delta2 = x - stats.mean_freq_mhz;
                stats.m2 += delta * delta2;

                // Update std_dev from M2 (population std dev)
                stats.std_dev_mhz = if stats.sample_count > 1 {
                    (stats.m2 / n).sqrt()
                } else {
                    0.0
                };

                // Update min/max
                if freq < stats.min_freq_mhz {
                    stats.min_freq_mhz = freq;
                }
                if freq > stats.max_freq_mhz {
                    stats.max_freq_mhz = freq;
                }

                any_ingested = true;
            }
        }

        if any_ingested {
            self.miner_count += 1;
            self.updated_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
    }

    /// Return fleet-average frequencies per position, snapped to valid PLL entries.
    ///
    /// For positions with no data, returns the lowest PLL frequency as a safe default.
    /// Each returned frequency is the nearest PLL entry at or below the mean.
    ///
    /// `chip_id`: ASIC chip ID for PLL table lookup. Pass 0x1387 for BM1387 (default).
    pub fn suggested_frequencies(&self, chip_id: u16) -> Vec<u16> {
        let pll = MinerProfile::pll_frequencies_for_chip(chip_id);
        let min_pll = pll.first().copied().unwrap_or(100);

        self.positions
            .iter()
            .map(|stats| {
                if stats.sample_count == 0 {
                    return min_pll;
                }

                let target = stats.mean_freq_mhz.round() as u16;

                // Find nearest PLL entry <= target
                pll.iter()
                    .rev()
                    .find(|&&f| f <= target)
                    .copied()
                    .unwrap_or(min_pll)
            })
            .collect()
    }

    /// Save chip binning database to a JSON file.
    pub fn save(&self, path: &str) -> crate::Result<()> {
        let tmp_path = format!("{}.tmp", path);

        if let Some(parent) = std::path::Path::new(path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "Failed to create binning database directory");
            }
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &json)?;
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        std::fs::rename(&tmp_path, path)?;
        tracing::info!(
            path,
            chip_type = %self.chip_type,
            miner_count = self.miner_count,
            positions = self.positions.len(),
            "Chip binning database saved"
        );
        Ok(())
    }

    /// Load chip binning database from a JSON file.
    pub fn load(path: &str) -> crate::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let db: Self = serde_json::from_str(&json)?;
        tracing::info!(
            path,
            chip_type = %db.chip_type,
            miner_count = db.miner_count,
            positions = db.positions.len(),
            "Chip binning database loaded"
        );
        Ok(db)
    }

    /// Return a confidence score (0.0..1.0) for a given chip position.
    ///
    /// Confidence is based on two factors:
    /// - **Sample count**: More samples = higher confidence (saturates at 20 miners).
    /// - **Low variance**: Tight clustering of frequencies = higher confidence.
    ///
    /// A position with 20+ samples and std_dev < 10 MHz scores ~1.0.
    /// A position with 1 sample scores ~0.25.
    /// A position with no data scores 0.0.
    pub fn confidence_for_position(&self, pos: u8) -> f64 {
        let stats = match self.positions.get(pos as usize) {
            Some(s) => s,
            None => return 0.0,
        };

        if stats.sample_count == 0 {
            return 0.0;
        }

        // Sample-count factor: log-ish ramp, saturates at 20 samples -> 1.0
        let sample_factor = (stats.sample_count as f64 / 20.0).min(1.0);

        // Variance factor: lower std_dev = higher confidence.
        // std_dev of 0 -> 1.0, std_dev of 50+ -> ~0.33
        let variance_factor = 1.0 / (1.0 + stats.std_dev_mhz / 25.0);

        // Weighted combination: samples matter more than variance
        let confidence = 0.6 * sample_factor + 0.4 * variance_factor;
        confidence.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ChipGrade, ChipProfile, TuningProfile};
    use dcentrald_asic::drivers::bm1387;

    fn make_test_profiles() -> HashMap<u8, TuningProfile> {
        let mut profiles = HashMap::new();

        for &chain_id in &[6u8, 7, 8] {
            let chips: Vec<ChipProfile> = (0..3)
                .map(|i| {
                    let freq = 600 + i * 25;
                    ChipProfile {
                        chip_index: i as u8,
                        max_stable_mhz: freq + 30,
                        operating_mhz: freq,
                        grade: if i == 0 {
                            ChipGrade::A
                        } else if i == 1 {
                            ChipGrade::B
                        } else {
                            ChipGrade::C
                        },
                        error_rate: 0.001,
                        nonces_counted: 100,
                        thermal_max_stable_mhz: None,
                        vf_curve: None,
                    }
                })
                .collect();
            let stats = TuningProfile::compute_stats(&chips, 15.0);

            profiles.insert(
                chain_id,
                TuningProfile {
                    version: 1,
                    chip_type: "BM1387".to_string(),
                    chain_id,
                    chip_count: 3,
                    voltage_mv: 9100,
                    tuned_at: "1710000000".to_string(),
                    ambient_temp_c: None,
                    optimal_voltage_mv: None,
                    estimated_power_w: 0.0,
                    estimated_efficiency_jth: 0.0,
                    equilibrium_temp_c: None,
                    thermal_refinement_duration_s: None,
                    calibrated_c_eff: None,
                    chips,
                    stats,
                    // W13.C3: SKU + flag denormalisation. Fleet imports
                    // pre-date per-SKU envelope tracking; default to None.
                    hashboard_sku: None,
                    hashboard_sku_flags: None,
                },
            );
        }

        profiles
    }

    #[test]
    fn test_export_from_profiles() {
        let profiles = make_test_profiles();
        let fleet = FleetProfile::export(&profiles, "miner-01.local");

        assert_eq!(fleet.version, 1);
        assert_eq!(fleet.chip_type, "BM1387");
        assert_eq!(fleet.chip_count, 3);
        assert_eq!(fleet.voltage_mv, 9100);
        assert_eq!(fleet.source_hostname, "miner-01.local");
        assert_eq!(fleet.chains.len(), 3);

        // Chains should be sorted by chain_id
        assert_eq!(fleet.chains[0].chain_id, 6);
        assert_eq!(fleet.chains[1].chain_id, 7);
        assert_eq!(fleet.chains[2].chain_id, 8);

        // Check per-chip frequencies
        assert_eq!(fleet.chains[0].chip_freqs_mhz, vec![600, 625, 650]);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let profiles = make_test_profiles();
        let fleet = FleetProfile::export(&profiles, "test-miner");

        let dir = std::env::temp_dir().join("dcent_test_fleet");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fleet-profile.json");
        let path_str = path.to_str().unwrap();

        // Save
        fleet.save(path_str).expect("save failed");

        // Load
        let loaded = FleetProfile::load(path_str).expect("load failed");
        assert_eq!(loaded.chip_type, "BM1387");
        assert_eq!(loaded.chip_count, 3);
        assert_eq!(loaded.source_hostname, "test-miner");
        assert_eq!(loaded.chains.len(), 3);
        assert_eq!(loaded.chains[0].chip_freqs_mhz, vec![600, 625, 650]);

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_compatibility_check() {
        let profiles = make_test_profiles();
        let fleet = FleetProfile::export(&profiles, "test-miner");

        // Compatible: same chip type and counts
        assert!(fleet.is_compatible("BM1387", &[(6, 3), (7, 3), (8, 3)]));

        // Incompatible: wrong chip type
        assert!(!fleet.is_compatible("BM1397", &[(6, 3), (7, 3), (8, 3)]));

        // Incompatible: wrong chip count on one chain
        assert!(!fleet.is_compatible("BM1387", &[(6, 3), (7, 63), (8, 3)]));

        // Incompatible: missing a chain
        assert!(!fleet.is_compatible("BM1387", &[(6, 3), (7, 3)]));
    }

    #[test]
    fn test_to_tuning_profiles() {
        let profiles = make_test_profiles();
        let fleet = FleetProfile::export(&profiles, "test-miner");

        let restored = fleet.to_tuning_profiles();
        assert_eq!(restored.len(), 3);

        for tp in &restored {
            assert_eq!(tp.chip_type, "BM1387");
            assert_eq!(tp.chip_count, 3);
            assert_eq!(tp.voltage_mv, 9100);
            assert_eq!(tp.chips.len(), 3);
            // Frequencies should match the original operating frequencies
            assert_eq!(tp.chips[0].operating_mhz, 600);
            assert_eq!(tp.chips[1].operating_mhz, 625);
            assert_eq!(tp.chips[2].operating_mhz, 650);
        }
    }

    #[test]
    fn test_export_empty_profiles() {
        let profiles: HashMap<u8, TuningProfile> = HashMap::new();
        let fleet = FleetProfile::export(&profiles, "empty");

        assert_eq!(fleet.chains.len(), 0);
        assert_eq!(fleet.chip_type, "");
        assert_eq!(fleet.chip_count, 0);
    }

    #[test]
    fn test_estimate_grade() {
        assert_eq!(estimate_grade(700, 625.0), ChipGrade::A); // +75 > +50
        assert_eq!(estimate_grade(630, 625.0), ChipGrade::B); // +5, within -25..+50
        assert_eq!(estimate_grade(580, 625.0), ChipGrade::C); // -45, within -75..-25
        assert_eq!(estimate_grade(500, 625.0), ChipGrade::D); // -125, < -75
    }

    // -----------------------------------------------------------------------
    // ChipBinningDatabase tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_binning_db_empty_creation() {
        let db = ChipBinningDatabase::new("BM1387", 63);

        assert_eq!(db.chip_type, "BM1387");
        assert_eq!(db.chip_count, 63);
        assert_eq!(db.positions.len(), 63);
        assert_eq!(db.miner_count, 0);
        assert_eq!(db.updated_at, 0);

        // All positions should have zero samples
        for (i, pos) in db.positions.iter().enumerate() {
            assert_eq!(pos.position, i as u8);
            assert_eq!(pos.sample_count, 0);
            assert_eq!(pos.mean_freq_mhz, 0.0);
            assert_eq!(pos.std_dev_mhz, 0.0);
            assert_eq!(pos.min_freq_mhz, u16::MAX);
            assert_eq!(pos.max_freq_mhz, 0);
        }
    }

    #[test]
    fn test_binning_db_ingest_one_fleet_profile() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);
        let profiles = make_test_profiles();
        let fleet = FleetProfile::export(&profiles, "miner-01");

        db.ingest_fleet_profile(&fleet);

        assert_eq!(db.miner_count, 1);
        assert!(db.updated_at > 0);

        // Fleet has 3 chains, each with 3 chips [600, 625, 650].
        // Position 0 got three samples of 600: mean=600, std_dev=0
        assert_eq!(db.positions[0].sample_count, 3);
        assert!((db.positions[0].mean_freq_mhz - 600.0).abs() < 0.01);
        assert!(db.positions[0].std_dev_mhz < 0.01);
        assert_eq!(db.positions[0].min_freq_mhz, 600);
        assert_eq!(db.positions[0].max_freq_mhz, 600);

        // Position 1 got three samples of 625
        assert_eq!(db.positions[1].sample_count, 3);
        assert!((db.positions[1].mean_freq_mhz - 625.0).abs() < 0.01);
        assert_eq!(db.positions[1].min_freq_mhz, 625);
        assert_eq!(db.positions[1].max_freq_mhz, 625);

        // Position 2 got three samples of 650
        assert_eq!(db.positions[2].sample_count, 3);
        assert!((db.positions[2].mean_freq_mhz - 650.0).abs() < 0.01);
        assert_eq!(db.positions[2].min_freq_mhz, 650);
        assert_eq!(db.positions[2].max_freq_mhz, 650);
    }

    /// Helper: build a fleet profile with custom per-chip frequencies.
    fn make_fleet_with_freqs(freqs: &[u16], hostname: &str) -> FleetProfile {
        let chains = vec![ChainFleetProfile {
            chain_id: 6,
            chip_count: freqs.len() as u8,
            chip_freqs_mhz: freqs.to_vec(),
            avg_freq_mhz: freqs.iter().map(|&f| f as f64).sum::<f64>() / freqs.len() as f64,
            min_freq_mhz: *freqs.iter().min().unwrap_or(&0),
            max_freq_mhz: *freqs.iter().max().unwrap_or(&0),
        }];

        FleetProfile {
            version: 1,
            chip_type: "BM1387".to_string(),
            chip_count: freqs.len() as u8,
            voltage_mv: 9100,
            source_hostname: hostname.to_string(),
            exported_at: 1710000000,
            chains,
        }
    }

    #[test]
    fn test_binning_db_ingest_multiple_profiles_statistics_converge() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);

        // Miner 1: chips at [600, 650, 700]
        let fleet1 = make_fleet_with_freqs(&[600, 650, 700], "miner-01");
        // Miner 2: chips at [620, 660, 680]
        let fleet2 = make_fleet_with_freqs(&[620, 660, 680], "miner-02");
        // Miner 3: chips at [610, 640, 690]
        let fleet3 = make_fleet_with_freqs(&[610, 640, 690], "miner-03");

        db.ingest_fleet_profile(&fleet1);
        db.ingest_fleet_profile(&fleet2);
        db.ingest_fleet_profile(&fleet3);

        assert_eq!(db.miner_count, 3);

        // Position 0: samples [600, 620, 610] -> mean ~610.0
        assert_eq!(db.positions[0].sample_count, 3);
        assert!((db.positions[0].mean_freq_mhz - 610.0).abs() < 0.01);
        assert_eq!(db.positions[0].min_freq_mhz, 600);
        assert_eq!(db.positions[0].max_freq_mhz, 620);
        // Population std_dev of [600, 620, 610] = sqrt(((10^2 + 10^2 + 0^2)/3)) = sqrt(200/3) ~= 8.165
        assert!((db.positions[0].std_dev_mhz - 8.165).abs() < 0.1);

        // Position 1: samples [650, 660, 640] -> mean = 650.0
        assert_eq!(db.positions[1].sample_count, 3);
        assert!((db.positions[1].mean_freq_mhz - 650.0).abs() < 0.01);
        assert_eq!(db.positions[1].min_freq_mhz, 640);
        assert_eq!(db.positions[1].max_freq_mhz, 660);

        // Position 2: samples [700, 680, 690] -> mean ~690.0
        assert_eq!(db.positions[2].sample_count, 3);
        assert!((db.positions[2].mean_freq_mhz - 690.0).abs() < 0.01);
        assert_eq!(db.positions[2].min_freq_mhz, 680);
        assert_eq!(db.positions[2].max_freq_mhz, 700);

        // Add many more identical profiles — variance should decrease
        for i in 0..50 {
            let fleet = make_fleet_with_freqs(&[610, 650, 690], &format!("miner-{}", i + 4));
            db.ingest_fleet_profile(&fleet);
        }

        // After 53 samples with mostly identical values, std_dev should be very low
        assert!(db.positions[0].std_dev_mhz < 3.0);
        assert!(db.positions[1].std_dev_mhz < 3.0);
        assert!(db.positions[2].std_dev_mhz < 3.0);
    }

    #[test]
    fn test_binning_db_suggested_frequencies_are_valid_pll() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);

        // Ingest with frequencies that are NOT exact PLL entries
        // PLL table: 100, 125, 150, ..., 600, 625, 650, ...
        // Mean of [610, 637, 688] should snap DOWN to nearest PLL
        let fleet1 = make_fleet_with_freqs(&[610, 637, 688], "miner-01");
        db.ingest_fleet_profile(&fleet1);

        let suggested = db.suggested_frequencies(0x1387);
        assert_eq!(suggested.len(), 3);

        let pll = bm1387::pll_frequencies();

        // Each suggested frequency must be a valid PLL entry
        for &freq in &suggested {
            assert!(
                pll.contains(&freq),
                "Suggested frequency {} is not a valid PLL entry",
                freq
            );
        }

        // 610 -> snaps to 600 (nearest PLL <= 610)
        assert_eq!(suggested[0], 600);
        // 637 -> snaps to 625 (nearest PLL <= 637)
        assert_eq!(suggested[1], 625);
        // 688 -> snaps to 650 (nearest PLL <= 688; BM1387 table has no 675 MHz entry)
        assert_eq!(suggested[2], 650);
    }

    #[test]
    fn test_binning_db_suggested_frequencies_empty_positions() {
        let db = ChipBinningDatabase::new("BM1387", 3);
        let suggested = db.suggested_frequencies(0x1387);

        // Empty positions should return the minimum PLL frequency
        let min_pll = bm1387::pll_frequencies().first().copied().unwrap();
        for &freq in &suggested {
            assert_eq!(freq, min_pll);
        }
    }

    #[test]
    fn test_binning_db_save_load_roundtrip() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);
        let fleet = make_fleet_with_freqs(&[600, 650, 700], "miner-01");
        db.ingest_fleet_profile(&fleet);

        let dir = std::env::temp_dir().join("dcent_test_binning_db");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("binning-db.json");
        let path_str = path.to_str().unwrap();

        // Save
        db.save(path_str).expect("save failed");

        // Load
        let loaded = ChipBinningDatabase::load(path_str).expect("load failed");
        assert_eq!(loaded.chip_type, "BM1387");
        assert_eq!(loaded.chip_count, 3);
        assert_eq!(loaded.miner_count, 1);
        assert_eq!(loaded.positions.len(), 3);

        // Verify statistics survived the roundtrip
        assert!((loaded.positions[0].mean_freq_mhz - 600.0).abs() < 0.01);
        assert!((loaded.positions[1].mean_freq_mhz - 650.0).abs() < 0.01);
        assert!((loaded.positions[2].mean_freq_mhz - 700.0).abs() < 0.01);
        assert_eq!(loaded.positions[0].min_freq_mhz, 600);
        assert_eq!(loaded.positions[2].max_freq_mhz, 700);

        // Verify m2 is preserved (Welford state) so further ingestion works
        let fleet2 = make_fleet_with_freqs(&[620, 660, 680], "miner-02");
        let mut loaded = loaded;
        loaded.ingest_fleet_profile(&fleet2);
        assert_eq!(loaded.positions[0].sample_count, 2);
        assert!((loaded.positions[0].mean_freq_mhz - 610.0).abs() < 0.01);

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binning_db_confidence_scoring() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);

        // No data: confidence = 0
        assert_eq!(db.confidence_for_position(0), 0.0);

        // Out-of-bounds position: confidence = 0
        assert_eq!(db.confidence_for_position(99), 0.0);

        // Ingest one profile — low sample count
        let fleet = make_fleet_with_freqs(&[600, 650, 700], "miner-01");
        db.ingest_fleet_profile(&fleet);

        let conf_1 = db.confidence_for_position(0);
        assert!(conf_1 > 0.0, "Confidence should be > 0 with 1 sample");
        assert!(conf_1 < 0.8, "Confidence should be moderate with 1 sample");

        // Ingest many identical profiles — confidence should increase
        for i in 0..25 {
            let fleet = make_fleet_with_freqs(&[600, 650, 700], &format!("miner-{}", i + 2));
            db.ingest_fleet_profile(&fleet);
        }

        let conf_many = db.confidence_for_position(0);
        assert!(
            conf_many > conf_1,
            "Confidence should increase with more samples"
        );
        // With 26 identical samples, std_dev = 0, sample_count > 20 => near 1.0
        assert!(
            conf_many > 0.9,
            "Confidence should be high with many consistent samples, got {}",
            conf_many
        );
    }

    #[test]
    fn test_binning_db_chip_type_mismatch_rejected() {
        let mut db = ChipBinningDatabase::new("BM1387", 3);

        // Create a fleet profile with a different chip type
        let mut fleet = make_fleet_with_freqs(&[600, 650, 700], "wrong-chip");
        fleet.chip_type = "BM1397".to_string();

        db.ingest_fleet_profile(&fleet);

        // Should be rejected — miner_count stays 0
        assert_eq!(db.miner_count, 0);
        assert_eq!(db.positions[0].sample_count, 0);
    }

    #[test]
    fn test_binning_db_welford_accuracy() {
        // Verify Welford's algorithm produces correct mean and std_dev
        // for a known dataset: [500, 600, 700, 800, 900]
        // Mean = 700, population variance = 20000, std_dev = 141.42...
        let mut db = ChipBinningDatabase::new("BM1387", 1);

        for (i, freq) in [500u16, 600, 700, 800, 900].iter().enumerate() {
            let fleet = make_fleet_with_freqs(&[*freq], &format!("miner-{}", i));
            db.ingest_fleet_profile(&fleet);
        }

        let stats = &db.positions[0];
        assert_eq!(stats.sample_count, 5);
        assert!((stats.mean_freq_mhz - 700.0).abs() < 0.01);
        // Population std dev = sqrt(20000) = 141.421...
        assert!(
            (stats.std_dev_mhz - 141.421).abs() < 0.1,
            "Expected std_dev ~141.42, got {}",
            stats.std_dev_mhz
        );
        assert_eq!(stats.min_freq_mhz, 500);
        assert_eq!(stats.max_freq_mhz, 900);
    }
}
