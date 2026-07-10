//! Tuning profile persistence.
//!
//! After characterization, per-chip frequency profiles are saved as JSON.
//! On warm start, the saved profile is loaded and verified — if chips still
//! perform well at their saved frequencies, tuning is skipped (~3s warm start).

use serde::{Deserialize, Serialize};

/// Silicon quality grade based on max stable frequency relative to nominal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChipGrade {
    /// Excellent silicon: max stable > nominal + 50 MHz.
    A,
    /// Good silicon: max stable within nominal +/- 25 MHz.
    B,
    /// Below average: max stable < nominal - 25 MHz.
    C,
    /// Weak/damaged: max stable < min_freq or failed characterization.
    D,
}

impl std::fmt::Display for ChipGrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChipGrade::A => write!(f, "A"),
            ChipGrade::B => write!(f, "B"),
            ChipGrade::C => write!(f, "C"),
            ChipGrade::D => write!(f, "D"),
        }
    }
}

/// Per-chip tuning result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipProfile {
    /// Chip index on the chain (0-62 for BM1387).
    pub chip_index: u8,
    /// Maximum stable frequency discovered by binary search (MHz).
    pub max_stable_mhz: u16,
    /// Operating frequency after safety margin applied (MHz).
    pub operating_mhz: u16,
    /// Silicon quality grade.
    pub grade: ChipGrade,
    /// Latest measured error rate (fraction, not percent).
    pub error_rate: f64,
    /// Total nonces counted during characterization.
    pub nonces_counted: u64,
    /// Optional V/F characterization curve from DVFS optimization.
    /// Contains measurements at multiple voltage points for this chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vf_curve: Option<Vec<crate::dvfs::VfPoint>>,
    /// Maximum stable frequency under thermal load (MHz).
    /// Discovered during thermal refinement soak. The delta between
    /// `max_stable_mhz` (cold) and `thermal_max_stable_mhz` (hot) reveals
    /// chips with high thermal sensitivity or boards with cooling problems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_max_stable_mhz: Option<u16>,
}

impl ChipProfile {
    /// Return the highest measured stable frequency that was validated at or
    /// below the requested voltage.
    ///
    /// When the chain runs at a safety-margin voltage slightly above the lowest
    /// measured stable point, the lower measured point is still the best safe
    /// cap we have for that operating region.
    pub fn measured_max_stable_at_or_below_voltage(&self, target_voltage_mv: u16) -> Option<u16> {
        self.vf_curve.as_ref().and_then(|curve| {
            curve
                .iter()
                .filter(|point| point.voltage_mv <= target_voltage_mv)
                .map(|point| point.max_stable_mhz)
                .max()
        })
    }

    /// Fit a maximum-clock-rate curve from the stored V/F measurements.
    pub fn fit_mcr_curve(&self) -> Option<crate::mcr_fit::McrFit> {
        let samples: Vec<crate::mcr_fit::McrSample> = self
            .vf_curve
            .as_ref()?
            .iter()
            .map(|point| crate::mcr_fit::McrSample {
                voltage_mv: point.voltage_mv,
                max_stable_mhz: point.max_stable_mhz as f64,
            })
            .collect();
        crate::mcr_fit::fit_mcr_curve(&samples)
    }
}

/// Complete tuning profile for one hash board chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningProfile {
    /// Profile format version (for future compatibility).
    pub version: u32,
    /// Chip type (e.g., "BM1387").
    pub chip_type: String,
    /// Chain ID (6, 7, or 8 on S9).
    pub chain_id: u8,
    /// Number of chips on this chain.
    pub chip_count: u8,
    /// Voltage at time of tuning (millivolts).
    pub voltage_mv: u16,
    /// Timestamp when tuning was performed (Unix epoch seconds).
    pub tuned_at: String,
    /// Ambient/die temperature at time of tuning (celsius), if available.
    #[serde(default)]
    pub ambient_temp_c: Option<f32>,
    /// Optimal voltage discovered by voltage search (mV), with safety margin applied.
    /// None if voltage optimization was not performed.
    #[serde(default)]
    pub optimal_voltage_mv: Option<u16>,
    /// Estimated power consumption at tuned settings (watts).
    /// Computed from the power model after tuning is finalized.
    #[serde(default)]
    pub estimated_power_w: f64,
    /// Estimated efficiency at tuned settings (J/TH).
    /// Lower is better. Computed from power model and hashrate estimate.
    #[serde(default)]
    pub estimated_efficiency_jth: f64,
    /// Temperature at thermal equilibrium (degrees C).
    /// Recorded when the thermal refinement phase detects equilibrium.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equilibrium_temp_c: Option<f32>,
    /// Duration of the thermal refinement soak (seconds).
    /// How long the refinement phase ran before declaring equilibrium or hitting max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_refinement_duration_s: Option<f64>,
    /// Calibrated power coefficient from PSU telemetry (item 7).
    /// Stored in profile so it persists across restarts. None = not yet calibrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibrated_c_eff: Option<f64>,
    /// Per-chip profiles.
    pub chips: Vec<ChipProfile>,
    /// Aggregate statistics.
    pub stats: ProfileStats,
    /// W13.C3 (2026-05-10): hashboard SKU id string (`BHB42601`,
    /// `BHB42801`, …) for the chain's installed BM1362 hashboard.
    /// Used by the autotuner to look up the per-SKU PVT envelope via
    /// `dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku::from_id`
    /// and clamp `(freq, volt)` dispatches.
    ///
    /// Optional + serde-default so old profile files without the field
    /// keep loading. When `None`, the autotuner falls back to the
    /// W13.C2 default (`Bhb42601`) for unrecognised SKUs.
    ///
    /// See `~/.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashboard_sku: Option<String>,
    /// W13.C3 (2026-05-10): denormalised per-SKU flags snapshot from
    /// `Bm1362HashboardSku::flags()` at the time the profile was tuned.
    /// Lets the dashboard render "voltage_fixed" / "requires_apw12_plus"
    /// indicators without re-resolving the SKU enum.
    ///
    /// Optional + serde-default for backwards compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashboard_sku_flags: Option<TuningProfileSkuFlags>,
}

/// W13.C3 (2026-05-10): denormalised per-SKU flag snapshot stored in
/// [`TuningProfile::hashboard_sku_flags`]. Mirrors
/// `dcentrald_silicon_profiles::bm1362::Bm1362SkuFlags` field-for-field
/// so the profile schema is self-contained — old profiles still parse
/// after future flag additions on the silicon-profiles side because
/// every field is `serde(default)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuningProfileSkuFlags {
    #[serde(default)]
    pub voltage_fixed: bool,
    #[serde(default)]
    pub requires_apw12_plus: bool,
    #[serde(default)]
    pub inverted_curve: bool,
    #[serde(default)]
    pub mix_levels: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMcrFitSummary {
    pub available: bool,
    pub chips_with_fit: usize,
    pub chip_count: usize,
    pub avg_rms_error_mhz: Option<f64>,
    pub min_voltage_mv: Option<u16>,
    pub max_voltage_mv: Option<u16>,
    pub avg_predicted_mhz_at_profile_voltage: Option<f64>,
    pub reason: String,
}

/// Aggregate statistics for the tuning profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileStats {
    /// Average operating frequency across all chips (MHz).
    pub avg_freq_mhz: f64,
    /// Minimum operating frequency (weakest chip).
    pub min_freq_mhz: u16,
    /// Maximum operating frequency (strongest chip).
    pub max_freq_mhz: u16,
    /// Count of each grade.
    pub grade_a: u16,
    pub grade_b: u16,
    pub grade_c: u16,
    pub grade_d: u16,
    /// Total characterization time (seconds).
    pub tuning_duration_s: f64,
    /// Estimated total hashrate (GH/s) based on operating frequencies.
    #[serde(default)]
    pub estimated_hashrate_ghs: f64,
    /// Estimated total power consumption (watts) from power model.
    #[serde(default)]
    pub estimated_power_w: f64,
    /// Estimated efficiency (J/TH). Lower is better.
    #[serde(default)]
    pub estimated_efficiency_jth: f64,
}

impl TuningProfile {
    pub fn mcr_fit_summary(&self) -> ProfileMcrFitSummary {
        let profile_voltage_mv = self.optimal_voltage_mv.unwrap_or(self.voltage_mv);
        let fits: Vec<crate::mcr_fit::McrFit> = self
            .chips
            .iter()
            .filter_map(ChipProfile::fit_mcr_curve)
            .collect();

        if fits.is_empty() {
            return ProfileMcrFitSummary {
                available: false,
                chips_with_fit: 0,
                chip_count: self.chips.len(),
                avg_rms_error_mhz: None,
                min_voltage_mv: None,
                max_voltage_mv: None,
                avg_predicted_mhz_at_profile_voltage: None,
                reason: "Saved profile has no V/F characterization points.".to_string(),
            };
        }

        let chips_with_fit = fits.len();
        let avg_rms_error_mhz =
            fits.iter().map(|fit| fit.rms_error_mhz).sum::<f64>() / chips_with_fit as f64;
        let min_voltage_mv = fits.iter().map(|fit| fit.min_voltage_mv).min();
        let max_voltage_mv = fits.iter().map(|fit| fit.max_voltage_mv).max();
        let avg_predicted_mhz_at_profile_voltage = fits
            .iter()
            .map(|fit| fit.curve.predict_voltage_mv(profile_voltage_mv))
            .sum::<f64>()
            / chips_with_fit as f64;

        ProfileMcrFitSummary {
            available: true,
            chips_with_fit,
            chip_count: self.chips.len(),
            avg_rms_error_mhz: Some(avg_rms_error_mhz),
            min_voltage_mv,
            max_voltage_mv,
            avg_predicted_mhz_at_profile_voltage: Some(avg_predicted_mhz_at_profile_voltage),
            reason: "One or more chips have enough V/F points for MCR(V) fitting.".to_string(),
        }
    }

    /// Save the profile as JSON to disk.
    ///
    /// Uses atomic write (write to .tmp, then rename) to prevent corruption
    /// from power loss during write.
    pub fn save(&self, dir: &str) -> crate::Result<()> {
        let path = format!("{}/autotune-chain{}.json", dir, self.chain_id);
        let tmp_path = format!("{}.tmp", path);

        // Ensure directory exists
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, dir, "Failed to create profile directory");
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &json)?;
        // fsync: commit data to NAND before rename to prevent power-loss corruption
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        std::fs::rename(&tmp_path, &path)?;
        tracing::info!(
            chain_id = self.chain_id,
            path = %path,
            chips = self.chip_count,
            avg_freq = format_args!("{:.0}", self.stats.avg_freq_mhz),
            "Tuning profile saved — {} chips, avg {:.0} MHz ({} A / {} B / {} C / {} D)",
            self.chip_count,
            self.stats.avg_freq_mhz,
            self.stats.grade_a,
            self.stats.grade_b,
            self.stats.grade_c,
            self.stats.grade_d,
        );
        Ok(())
    }

    /// Current profile format version.
    pub const CURRENT_VERSION: u32 = 2;

    /// Load a profile from disk with version-aware deserialization.
    ///
    /// Deserializes as `serde_json::Value` first, reads the `version` field,
    /// then migrates old formats to current. One-way migrations:
    /// - V1→V2: adds `current_difficulty` default (256), `calibrated_c_eff` (None)
    ///
    /// Returns None if file doesn't exist or is invalid.
    pub fn load(dir: &str, chain_id: u8) -> Option<Self> {
        let path = format!("{}/autotune-chain{}.json", dir, chain_id);
        let json = std::fs::read_to_string(&path).ok()?;

        // First, try parsing as a generic JSON value to check version
        let mut value: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    chain_id,
                    error = %e,
                    path = %path,
                    "Failed to parse saved profile JSON — will re-tune from scratch"
                );
                return None;
            }
        };

        // Read version (default to 1 if missing)
        let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

        // Migrate V1 → V2: add new fields with defaults
        if version < 2 {
            tracing::info!(
                chain_id,
                old_version = version,
                new_version = Self::CURRENT_VERSION,
                "Migrating profile from V{} to V{}",
                version,
                Self::CURRENT_VERSION,
            );

            // Update version
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "version".to_string(),
                    serde_json::Value::from(Self::CURRENT_VERSION),
                );
                // V2 adds calibrated_c_eff (stored in profile for PSU telemetry calibration)
                if !obj.contains_key("calibrated_c_eff") {
                    obj.insert("calibrated_c_eff".to_string(), serde_json::Value::Null);
                }
            }
        }

        // Now deserialize the (possibly migrated) value
        match serde_json::from_value::<Self>(value) {
            Ok(profile) => {
                // Cross-check: chips vec length must match chip_count
                if profile.chips.len() != profile.chip_count as usize {
                    tracing::warn!(
                        chain_id,
                        expected = profile.chip_count,
                        actual = profile.chips.len(),
                        "Profile chip count mismatch — will re-tune from scratch"
                    );
                    return None;
                }
                tracing::info!(
                    chain_id,
                    path = %path,
                    version = profile.version,
                    "Loaded saved tuning profile (V{})",
                    profile.version,
                );
                Some(profile)
            }
            Err(e) => {
                tracing::warn!(
                    chain_id,
                    error = %e,
                    path = %path,
                    "Failed to deserialize profile after migration — will re-tune from scratch"
                );
                None
            }
        }
    }

    /// Validate that a saved profile is compatible with current hardware.
    ///
    /// Checks chip count matches and profile isn't too stale.
    pub fn is_compatible(&self, chip_type: &str, chip_count: u8) -> bool {
        if self.chip_type != chip_type {
            tracing::info!(
                expected = chip_type,
                saved = %self.chip_type,
                "Profile chip type mismatch"
            );
            return false;
        }
        if self.chip_count != chip_count {
            tracing::info!(
                expected = chip_count,
                saved = self.chip_count,
                "Profile chip count mismatch"
            );
            return false;
        }
        if self.version > Self::CURRENT_VERSION {
            tracing::info!(
                version = self.version,
                current = Self::CURRENT_VERSION,
                "Profile version {} is newer than supported version {} — re-tuning",
                self.version,
                Self::CURRENT_VERSION,
            );
            return false;
        }
        true
    }

    /// Validate a saved profile against the current chain identity.
    pub fn is_compatible_with_chain(&self, chip_id: u16, chain_id: u8, chip_count: u8) -> bool {
        let Some(saved_chip_id) = crate::chip_id_from_type(&self.chip_type) else {
            tracing::info!(saved = %self.chip_type, "Profile chip type could not be parsed into chip id");
            return false;
        };

        if saved_chip_id != chip_id {
            tracing::info!(
                expected = format_args!("0x{:04X}", chip_id),
                saved = format_args!("0x{:04X}", saved_chip_id),
                "Profile chip id mismatch"
            );
            return false;
        }

        if self.chain_id != chain_id {
            tracing::info!(
                expected = chain_id,
                saved = self.chain_id,
                "Profile chain id mismatch"
            );
            return false;
        }

        self.is_compatible(&self.chip_type, chip_count)
    }

    /// Save a backup copy of this profile before destructive operations.
    ///
    /// Saves as `autotune-chain{N}.backup.json` alongside the main profile.
    /// Used before voltage optimization or re-characterization so we can
    /// automatically revert if the new settings are worse.
    pub fn save_backup(&self, dir: &str) -> crate::Result<()> {
        let path = format!("{}/autotune-chain{}.backup.json", dir, self.chain_id);
        let tmp_path = format!("{}.tmp", path);

        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, dir, "Failed to create backup directory");
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &json)?;
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        std::fs::rename(&tmp_path, &path)?;
        tracing::info!(
            chain_id = self.chain_id,
            path = %path,
            "Profile backup saved before optimization"
        );
        Ok(())
    }

    /// Load a backup profile from disk. Returns None if no backup exists.
    pub fn load_backup(dir: &str, chain_id: u8) -> Option<Self> {
        let path = format!("{}/autotune-chain{}.backup.json", dir, chain_id);
        let json = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<Self>(&json) {
            Ok(profile) => {
                if profile.chips.len() != profile.chip_count as usize {
                    tracing::warn!(chain_id, "Backup profile chip count mismatch");
                    return None;
                }
                tracing::info!(chain_id, "Loaded backup profile");
                Some(profile)
            }
            Err(e) => {
                tracing::warn!(chain_id, error = %e, "Failed to parse backup profile");
                None
            }
        }
    }

    /// Compute aggregate statistics from chip profiles.
    pub fn compute_stats(chips: &[ChipProfile], duration_s: f64) -> ProfileStats {
        if chips.is_empty() {
            return ProfileStats {
                avg_freq_mhz: 0.0,
                min_freq_mhz: 0,
                max_freq_mhz: 0,
                grade_a: 0,
                grade_b: 0,
                grade_c: 0,
                grade_d: 0,
                tuning_duration_s: duration_s,
                estimated_hashrate_ghs: 0.0,
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
            };
        }

        let sum: u64 = chips.iter().map(|c| c.operating_mhz as u64).sum();
        let avg = sum as f64 / chips.len() as f64;
        let min = chips.iter().map(|c| c.operating_mhz).min().unwrap_or(0);
        let max = chips.iter().map(|c| c.operating_mhz).max().unwrap_or(0);

        let mut ga = 0u16;
        let mut gb = 0u16;
        let mut gc = 0u16;
        let mut gd = 0u16;
        for c in chips {
            match c.grade {
                ChipGrade::A => ga += 1,
                ChipGrade::B => gb += 1,
                ChipGrade::C => gc += 1,
                ChipGrade::D => gd += 1,
            }
        }

        ProfileStats {
            avg_freq_mhz: avg,
            min_freq_mhz: min,
            max_freq_mhz: max,
            grade_a: ga,
            grade_b: gb,
            grade_c: gc,
            grade_d: gd,
            tuning_duration_s: duration_s,
            // Power/hashrate estimates are populated later by the tuner
            // after the power model is applied.
            estimated_hashrate_ghs: 0.0,
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
        }
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    fn make_profile() -> TuningProfile {
        TuningProfile {
            version: TuningProfile::CURRENT_VERSION,
            chip_type: "BM1387".to_string(),
            chain_id: 6,
            chip_count: 3,
            voltage_mv: 9100,
            tuned_at: "0".to_string(),
            ambient_temp_c: None,
            optimal_voltage_mv: None,
            estimated_power_w: 0.0,
            estimated_efficiency_jth: 0.0,
            equilibrium_temp_c: None,
            thermal_refinement_duration_s: None,
            calibrated_c_eff: None,
            chips: vec![
                ChipProfile {
                    chip_index: 0,
                    max_stable_mhz: 650,
                    operating_mhz: 625,
                    grade: ChipGrade::B,
                    error_rate: 0.0,
                    nonces_counted: 100,
                    vf_curve: None,
                    thermal_max_stable_mhz: None,
                },
                ChipProfile {
                    chip_index: 1,
                    max_stable_mhz: 650,
                    operating_mhz: 625,
                    grade: ChipGrade::B,
                    error_rate: 0.0,
                    nonces_counted: 100,
                    vf_curve: None,
                    thermal_max_stable_mhz: None,
                },
                ChipProfile {
                    chip_index: 2,
                    max_stable_mhz: 650,
                    operating_mhz: 625,
                    grade: ChipGrade::B,
                    error_rate: 0.0,
                    nonces_counted: 100,
                    vf_curve: None,
                    thermal_max_stable_mhz: None,
                },
            ],
            stats: ProfileStats {
                avg_freq_mhz: 625.0,
                min_freq_mhz: 625,
                max_freq_mhz: 625,
                grade_a: 0,
                grade_b: 3,
                grade_c: 0,
                grade_d: 0,
                tuning_duration_s: 10.0,
                estimated_hashrate_ghs: 0.0,
                estimated_power_w: 0.0,
                estimated_efficiency_jth: 0.0,
            },
            // W13.C3: SKU + flag denormalisation. Test fixture pre-dates
            // per-SKU envelope tracking; default to None.
            hashboard_sku: None,
            hashboard_sku_flags: None,
        }
    }

    #[test]
    fn test_profile_compatible_with_chain_identity() {
        let profile = make_profile();
        assert!(profile.is_compatible_with_chain(0x1387, 6, 3));
        assert!(!profile.is_compatible_with_chain(0x1397, 6, 3));
        assert!(!profile.is_compatible_with_chain(0x1387, 7, 3));
        assert!(!profile.is_compatible_with_chain(0x1387, 6, 4));
    }

    #[test]
    fn test_measured_max_stable_at_or_below_voltage_uses_lower_curve_point() {
        let mut profile = make_profile();
        profile.chips[0].vf_curve = Some(vec![
            crate::dvfs::VfPoint {
                voltage_mv: 9100,
                max_stable_mhz: 650,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 8900,
                max_stable_mhz: 625,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
        ]);

        assert_eq!(
            profile.chips[0].measured_max_stable_at_or_below_voltage(8920),
            Some(625)
        );
        assert_eq!(
            profile.chips[0].measured_max_stable_at_or_below_voltage(8800),
            None
        );
    }

    #[test]
    fn test_chip_profile_fits_mcr_from_vf_curve() {
        let mut profile = make_profile();
        profile.chips[0].vf_curve = Some(vec![
            crate::dvfs::VfPoint {
                voltage_mv: 8800,
                max_stable_mhz: 600,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 9000,
                max_stable_mhz: 650,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 9200,
                max_stable_mhz: 700,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
        ]);

        let fit = profile.chips[0].fit_mcr_curve().expect("mcr fit");
        assert_eq!(fit.sample_count, 3);
        assert!((fit.curve.predict_voltage_mv(9000) - 650.0).abs() < 0.01);
    }

    #[test]
    fn test_profile_mcr_summary_reports_aggregate_fit() {
        let mut profile = make_profile();
        profile.optimal_voltage_mv = Some(9000);
        profile.chips[0].vf_curve = Some(vec![
            crate::dvfs::VfPoint {
                voltage_mv: 8800,
                max_stable_mhz: 600,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 9000,
                max_stable_mhz: 650,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
            crate::dvfs::VfPoint {
                voltage_mv: 9200,
                max_stable_mhz: 700,
                estimated_power_w: 0.0,
                estimated_hashrate_ghs: 0.0,
                efficiency_jth: 0.0,
            },
        ]);

        let summary = profile.mcr_fit_summary();
        assert!(summary.available);
        assert_eq!(summary.chips_with_fit, 1);
        assert_eq!(summary.chip_count, 3);
        assert_eq!(summary.min_voltage_mv, Some(8800));
        assert_eq!(summary.max_voltage_mv, Some(9200));
        assert!((summary.avg_predicted_mhz_at_profile_voltage.unwrap() - 650.0).abs() < 0.01);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_profile() -> TuningProfile {
        let chips = vec![
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
                operating_mhz: 575,
                grade: ChipGrade::B,
                error_rate: 0.003,
                nonces_counted: 95,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
        ];
        let stats = TuningProfile::compute_stats(&chips, 15.0);
        TuningProfile {
            version: 1,
            chip_type: "BM1387".to_string(),
            chain_id: 6,
            chip_count: 2,
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
            // W13.C3: SKU + flag denormalisation. Test fixture default.
            hashboard_sku: None,
            hashboard_sku_flags: None,
        }
    }

    #[test]
    fn test_compute_stats() {
        let chips = vec![
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
                max_stable_mhz: 500,
                operating_mhz: 475,
                grade: ChipGrade::D,
                error_rate: 0.05,
                nonces_counted: 50,
                vf_curve: None,
                thermal_max_stable_mhz: None,
            },
        ];
        let stats = TuningProfile::compute_stats(&chips, 10.0);
        assert_eq!(stats.min_freq_mhz, 475);
        assert_eq!(stats.max_freq_mhz, 650);
        assert!((stats.avg_freq_mhz - 562.5).abs() < 0.1);
        assert_eq!(stats.grade_a, 1);
        assert_eq!(stats.grade_d, 1);
        assert_eq!(stats.tuning_duration_s, 10.0);
    }

    #[test]
    fn test_is_compatible() {
        let profile = make_test_profile();
        assert!(profile.is_compatible("BM1387", 2));
        assert!(!profile.is_compatible("BM1397", 2));
        assert!(!profile.is_compatible("BM1387", 63));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let profile = make_test_profile();
        let dir = std::env::temp_dir().join("dcent_test_profile");
        let dir_str = dir.to_str().unwrap();

        // Clean up from previous runs
        let _ = std::fs::remove_dir_all(&dir);

        // Save
        profile.save(dir_str).expect("save failed");

        // Load
        let loaded = TuningProfile::load(dir_str, 6).expect("load returned None");
        assert_eq!(loaded.chip_type, "BM1387");
        assert_eq!(loaded.chain_id, 6);
        assert_eq!(loaded.chip_count, 2);
        assert_eq!(loaded.chips.len(), 2);
        assert_eq!(loaded.chips[0].operating_mhz, 650);
        assert_eq!(loaded.chips[1].grade, ChipGrade::B);
        assert!(loaded.ambient_temp_c.is_none());

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_nonexistent() {
        let result = TuningProfile::load("/tmp/dcent_nonexistent_dir_12345", 99);
        assert!(result.is_none());
    }

    #[test]
    fn test_ambient_temp_option() {
        let mut profile = make_test_profile();
        profile.ambient_temp_c = Some(25.5);

        let json = serde_json::to_string(&profile).unwrap();
        let loaded: TuningProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.ambient_temp_c, Some(25.5));

        // Test with None
        profile.ambient_temp_c = None;
        let json = serde_json::to_string(&profile).unwrap();
        let loaded: TuningProfile = serde_json::from_str(&json).unwrap();
        assert!(loaded.ambient_temp_c.is_none());
    }

    #[test]
    fn test_v1_to_v2_migration() {
        // Simulate a V1 profile JSON (no calibrated_c_eff field)
        let v1_json = r#"{
            "version": 1,
            "chip_type": "BM1387",
            "chain_id": 6,
            "chip_count": 1,
            "voltage_mv": 9100,
            "tuned_at": "1710000000",
            "estimated_power_w": 0.0,
            "estimated_efficiency_jth": 0.0,
            "chips": [{
                "chip_index": 0,
                "max_stable_mhz": 700,
                "operating_mhz": 650,
                "grade": "A",
                "error_rate": 0.001,
                "nonces_counted": 100
            }],
            "stats": {
                "avg_freq_mhz": 650.0,
                "min_freq_mhz": 650,
                "max_freq_mhz": 650,
                "grade_a": 1,
                "grade_b": 0,
                "grade_c": 0,
                "grade_d": 0,
                "tuning_duration_s": 15.0
            }
        }"#;

        // Write V1 profile to temp dir
        let dir = std::env::temp_dir().join("dcent_test_v1_migration");
        let dir_str = dir.to_str().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = format!("{}/autotune-chain6.json", dir_str);
        std::fs::write(&path, v1_json).unwrap();

        // Load — should migrate from V1 to V2
        let loaded = TuningProfile::load(dir_str, 6).expect("V1→V2 migration failed");
        assert_eq!(loaded.version, TuningProfile::CURRENT_VERSION);
        assert_eq!(loaded.chip_count, 1);
        assert_eq!(loaded.chips[0].operating_mhz, 650);
        assert!(loaded.calibrated_c_eff.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_compatible_accepts_v2() {
        let mut profile = make_test_profile();
        profile.version = 2;
        assert!(profile.is_compatible("BM1387", 2));
    }

    #[test]
    fn test_is_compatible_rejects_future_version() {
        let mut profile = make_test_profile();
        profile.version = 999;
        assert!(!profile.is_compatible("BM1387", 2));
    }
}
