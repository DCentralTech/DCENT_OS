//! Last-known-good autotuner state persistence.
//!
//! Profiles remain the authoritative per-chain tuning artifacts. This module
//! adds a small hardware-fingerprinted resume state so a reboot can prove that
//! the saved operating point belongs to the currently attached hashboards before
//! treating it as last-known-good.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::profile::TuningProfile;
use crate::ChainHardwareIdentity;

pub const AUTOTUNER_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutotunerHardwareFingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub chains: Vec<ChainHardwareFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainHardwareFingerprint {
    pub chain_id: u8,
    pub chip_id: u16,
    pub chip_count: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eeprom_serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eeprom_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dspic_fw_byte: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastKnownGoodChipState {
    pub chip_index: u8,
    pub operating_mhz: u16,
    pub max_stable_mhz: u16,
    pub voltage_mv: u16,
    pub grade: crate::profile::ChipGrade,
    pub error_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcr_curve: Option<crate::mcr_fit::QuadraticFunc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcr_rms_error_mhz: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastKnownGoodChainState {
    pub chain_id: u8,
    pub chip_count: u8,
    pub voltage_mv: u16,
    pub avg_freq_mhz: f64,
    pub estimated_power_w: f64,
    pub estimated_efficiency_jth: f64,
    pub chips: Vec<LastKnownGoodChipState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutotunerResumeState {
    pub version: u32,
    pub saved_at_unix_s: u64,
    pub fingerprint: AutotunerHardwareFingerprint,
    pub chains: Vec<LastKnownGoodChainState>,
}

impl AutotunerHardwareFingerprint {
    pub fn from_profiles(profiles: &HashMap<u8, TuningProfile>, platform: Option<String>) -> Self {
        Self::from_profiles_with_identities(profiles, platform, &HashMap::new())
    }

    pub fn from_profiles_with_identities(
        profiles: &HashMap<u8, TuningProfile>,
        platform: Option<String>,
        identities: &HashMap<u8, ChainHardwareIdentity>,
    ) -> Self {
        let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
        chain_ids.sort_unstable();

        let chains = chain_ids
            .into_iter()
            .filter_map(|chain_id| {
                let profile = profiles.get(&chain_id)?;
                let identity = identities.get(&chain_id);
                Some(ChainHardwareFingerprint {
                    chain_id,
                    chip_id: crate::chip_id_from_type(&profile.chip_type).unwrap_or_default(),
                    chip_count: profile.chip_count,
                    eeprom_serial: identity.and_then(|identity| identity.eeprom_serial.clone()),
                    eeprom_fingerprint: identity
                        .and_then(|identity| identity.eeprom_fingerprint.clone()),
                    dspic_fw_byte: identity.and_then(|identity| identity.dspic_fw_byte),
                })
            })
            .collect();

        Self { platform, chains }
    }

    pub fn with_chain_identity(
        mut self,
        chain_id: u8,
        eeprom_serial: Option<String>,
        dspic_fw_byte: Option<u8>,
    ) -> Self {
        if let Some(chain) = self
            .chains
            .iter_mut()
            .find(|chain| chain.chain_id == chain_id)
        {
            chain.eeprom_serial = eeprom_serial;
            chain.dspic_fw_byte = dspic_fw_byte;
        }
        self
    }

    pub fn with_chain_eeprom_fingerprint(
        mut self,
        chain_id: u8,
        eeprom_fingerprint: Option<String>,
    ) -> Self {
        if let Some(chain) = self
            .chains
            .iter_mut()
            .find(|chain| chain.chain_id == chain_id)
        {
            chain.eeprom_fingerprint = eeprom_fingerprint;
        }
        self
    }
}

impl AutotunerResumeState {
    pub fn from_profiles(
        profiles: &HashMap<u8, TuningProfile>,
        fingerprint: AutotunerHardwareFingerprint,
    ) -> Self {
        let mut chain_ids: Vec<u8> = profiles.keys().copied().collect();
        chain_ids.sort_unstable();

        let chains = chain_ids
            .into_iter()
            .filter_map(|chain_id| {
                let profile = profiles.get(&chain_id)?;
                let voltage_mv = profile.optimal_voltage_mv.unwrap_or(profile.voltage_mv);
                let chips = profile
                    .chips
                    .iter()
                    .map(|chip| {
                        let mcr_fit = chip.fit_mcr_curve();
                        LastKnownGoodChipState {
                            chip_index: chip.chip_index,
                            operating_mhz: chip.operating_mhz,
                            max_stable_mhz: chip.max_stable_mhz,
                            voltage_mv,
                            grade: chip.grade,
                            error_rate: chip.error_rate,
                            mcr_curve: mcr_fit.as_ref().map(|fit| fit.curve),
                            mcr_rms_error_mhz: mcr_fit.as_ref().map(|fit| fit.rms_error_mhz),
                        }
                    })
                    .collect();

                Some(LastKnownGoodChainState {
                    chain_id,
                    chip_count: profile.chip_count,
                    voltage_mv,
                    avg_freq_mhz: profile.stats.avg_freq_mhz,
                    estimated_power_w: profile.estimated_power_w,
                    estimated_efficiency_jth: profile.estimated_efficiency_jth,
                    chips,
                })
            })
            .collect();

        Self {
            version: AUTOTUNER_STATE_VERSION,
            saved_at_unix_s: now_unix_s(),
            fingerprint,
            chains,
        }
    }

    pub fn hardware_matches(&self, fingerprint: &AutotunerHardwareFingerprint) -> bool {
        self.version == AUTOTUNER_STATE_VERSION && &self.fingerprint == fingerprint
    }

    pub fn load(path: impl AsRef<Path>) -> crate::Result<Self> {
        let toml = std::fs::read_to_string(path.as_ref())?;
        toml::from_str(&toml)
            .map_err(|err| crate::AutoTunerError::Config(format!("state parse error: {err}")))
    }

    pub fn load_if_hardware_matches(
        path: impl AsRef<Path>,
        fingerprint: &AutotunerHardwareFingerprint,
    ) -> crate::Result<Option<Self>> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }

        let state = Self::load(path)?;
        if state.hardware_matches(fingerprint) {
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> crate::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = path.with_extension("toml.tmp");
        let toml = toml::to_string_pretty(self).map_err(|err| {
            crate::AutoTunerError::Config(format!("state serialize error: {err}"))
        })?;
        std::fs::write(&tmp_path, toml)?;
        if let Ok(file) = std::fs::File::open(&tmp_path) {
            let _ = file.sync_all();
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

fn now_unix_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvfs::VfPoint;
    use crate::profile::{ChipGrade, ChipProfile, ProfileStats};

    fn make_profile(chain_id: u8, chip_count: u8) -> TuningProfile {
        let chips: Vec<ChipProfile> = (0..chip_count)
            .map(|chip_index| ChipProfile {
                chip_index,
                max_stable_mhz: 700,
                operating_mhz: 650,
                grade: ChipGrade::A,
                error_rate: 0.001,
                nonces_counted: 100,
                vf_curve: Some(vec![
                    VfPoint {
                        voltage_mv: 8800,
                        max_stable_mhz: 600,
                        estimated_power_w: 0.0,
                        estimated_hashrate_ghs: 0.0,
                        efficiency_jth: 0.0,
                    },
                    VfPoint {
                        voltage_mv: 9000,
                        max_stable_mhz: 650,
                        estimated_power_w: 0.0,
                        estimated_hashrate_ghs: 0.0,
                        efficiency_jth: 0.0,
                    },
                    VfPoint {
                        voltage_mv: 9200,
                        max_stable_mhz: 700,
                        estimated_power_w: 0.0,
                        estimated_hashrate_ghs: 0.0,
                        efficiency_jth: 0.0,
                    },
                ]),
                thermal_max_stable_mhz: None,
            })
            .collect();

        TuningProfile {
            version: TuningProfile::CURRENT_VERSION,
            chip_type: "BM1387".to_string(),
            chain_id,
            chip_count,
            voltage_mv: 9100,
            tuned_at: "0".to_string(),
            ambient_temp_c: Some(28.0),
            optimal_voltage_mv: Some(9000),
            estimated_power_w: 450.0,
            estimated_efficiency_jth: 32.0,
            equilibrium_temp_c: None,
            thermal_refinement_duration_s: None,
            calibrated_c_eff: None,
            chips,
            stats: ProfileStats {
                avg_freq_mhz: 650.0,
                min_freq_mhz: 650,
                max_freq_mhz: 650,
                grade_a: chip_count as u16,
                grade_b: 0,
                grade_c: 0,
                grade_d: 0,
                tuning_duration_s: 10.0,
                estimated_hashrate_ghs: 0.0,
                estimated_power_w: 450.0,
                estimated_efficiency_jth: 32.0,
            },
            // W13.C3: SKU + flag denormalisation. Test fixture default.
            hashboard_sku: None,
            hashboard_sku_flags: None,
        }
    }

    #[test]
    fn state_round_trip_preserves_fingerprint_and_mcr_fit() {
        let mut profiles = HashMap::new();
        profiles.insert(6, make_profile(6, 2));
        let fingerprint =
            AutotunerHardwareFingerprint::from_profiles(&profiles, Some("s9".to_string()))
                .with_chain_identity(6, Some("EEPROM123".to_string()), Some(0x03));
        let state = AutotunerResumeState::from_profiles(&profiles, fingerprint.clone());

        let dir = std::env::temp_dir().join("dcent_autotuner_state_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");

        state.save_atomic(&path).expect("save state");
        let loaded = AutotunerResumeState::load_if_hardware_matches(&path, &fingerprint)
            .expect("load state")
            .expect("matching fingerprint");

        assert_eq!(loaded.fingerprint, fingerprint);
        assert_eq!(loaded.chains[0].chips.len(), 2);
        assert!(loaded.chains[0].chips[0].mcr_curve.is_some());
        assert!(loaded.chains[0].chips[0].mcr_rms_error_mhz.unwrap() < 0.01);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_invalidates_on_hardware_change() {
        let mut profiles = HashMap::new();
        profiles.insert(6, make_profile(6, 2));
        let fingerprint =
            AutotunerHardwareFingerprint::from_profiles(&profiles, Some("s9".to_string()));
        let state = AutotunerResumeState::from_profiles(&profiles, fingerprint.clone());

        let dir = std::env::temp_dir().join("dcent_autotuner_state_invalidate");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");
        state.save_atomic(&path).expect("save state");

        profiles.insert(6, make_profile(6, 3));
        let changed =
            AutotunerHardwareFingerprint::from_profiles(&profiles, Some("s9".to_string()));
        let loaded =
            AutotunerResumeState::load_if_hardware_matches(&path, &changed).expect("load state");
        assert!(loaded.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_includes_optional_chain_identity() {
        let mut profiles = HashMap::new();
        profiles.insert(6, make_profile(6, 2));

        let mut identities = HashMap::new();
        identities.insert(
            6,
            ChainHardwareIdentity {
                eeprom_serial: Some("BHB42-123".to_string()),
                eeprom_fingerprint: Some("i2c1-0x50:sha256:abcd".to_string()),
                dspic_fw_byte: Some(0x89),
            },
        );

        let fingerprint = AutotunerHardwareFingerprint::from_profiles_with_identities(
            &profiles,
            Some("s19j-am2".to_string()),
            &identities,
        );

        assert_eq!(
            fingerprint.chains[0].eeprom_serial.as_deref(),
            Some("BHB42-123")
        );
        assert_eq!(
            fingerprint.chains[0].eeprom_fingerprint.as_deref(),
            Some("i2c1-0x50:sha256:abcd")
        );
        assert_eq!(fingerprint.chains[0].dspic_fw_byte, Some(0x89));
    }
}
