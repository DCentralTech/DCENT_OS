//! Experimental / lab-only flags for dcentrald.
//!
//! Mirror of bosminer's `bosminer-experimental.toml` concept (clean-room —
//! we observed the flag names by RE'ing the bosminer.unpacked binary at
//! offsets noted in ,
//! but we own the parser, the defaults, and the semantics).
//!
//! Loaded from `/etc/dcentrald-experimental.toml` at startup. Missing file
//! → all defaults (which are conservative — strict, no degraded modes).
//!
//! ## Why these flags exist
//!
//! Bosminer ships `bosminer-experimental.toml` empty by default and treats
//! it as the operator's escape hatch when the production safe defaults are
//! too strict for the actual hardware in front of them. Same intent here.
//!
//! ## What's NOT in here
//!
//! - **License-server flags** (none — DCENT_OS has no license server).
//! - **Telemetry-submit flags** (we never auto-submit; operator-driven).
//! - **DPS power-walk parameters** (Phase N — not yet implemented in our
//!   autotuner; will surface here once the GDTUNER state machine ports).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default file path. Override via `DCENTRALD_EXPERIMENTAL_TOML` env var
/// (lab/test only — production should use the file).
pub const DEFAULT_PATH: &str = "/etc/dcentrald-experimental.toml";

/// Operator-tunable lab flags. ALL fields default to their conservative
/// production values (no operator override required for safe-default
/// behavior).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// Maximum number of bad / missing chip responses tolerated during
    /// BM136x chain enumeration. Default 0 = strict (every chip must
    /// reply with the correct chip-id), matches dcentrald's pre-2026-04
    /// behavior.
    ///
    /// Useful for partially-faulty hashboards (e.g. live `a lab unit` chain1
    /// enumerates 8/77 chips with chip-0 reply 0xe42 instead of the
    /// expected 0x1366; setting this to 8 lets the chain proceed and
    /// mine with the working chips).
    ///
    /// Bosminer's equivalent: `rambo_mode_max_bad_responses` (we kept
    /// the exact name for ecosystem familiarity).
    pub rambo_mode_max_bad_responses: u8,

    /// On NoPic miners (S21/T21/S19K Pro NoPic / S19 XP / S19J XP) the
    /// PSU rails are always-on; bosminer refuses to disable individual
    /// hashboards because the rail can't be cut without cutting all
    /// chains together. When `true`, dcentrald will mark a single dead
    /// chain as disabled for tuner purposes while keeping the others
    /// running (the rail stays on; only the work-dispatch is gated).
    ///
    /// Default: `false` (matches bosminer safety stance).
    pub allow_disabling_hashboards_on_nopic_miners: bool,

    /// Minimum fan PWM floor (0-100). When fan tach is unreliable or
    /// the autoconfigure probe falls back, use this as the safe floor.
    /// Ignored when the active thermal profile's `fan_min_pwm` already
    /// exceeds it.
    ///
    /// Bosminer's empirical floor on .78 was 25% PWM; we default to
    /// 25 to match. Industrial profile may raise this, home profile
    /// is hard-capped at 30 by .
    pub min_fan_pwm_floor: u8,

    /// Disable bosminer's "bootstrap voltage threshold" check at boot.
    /// We don't run that check today (Phase N autotuner port territory),
    /// but the flag is reserved here so the toml schema is forward-
    /// compatible when Stage2 of the autotuner state machine ports.
    pub disable_bootstrap_check: bool,

    /// When true, send a one-time dummy telemetry packet at startup
    /// to validate the pool / dashboard pipeline. Default off.
    pub send_dummy_telemetry_on_startup: bool,
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            rambo_mode_max_bad_responses: 0,
            allow_disabling_hashboards_on_nopic_miners: false,
            min_fan_pwm_floor: 25,
            disable_bootstrap_check: false,
            send_dummy_telemetry_on_startup: false,
        }
    }
}

impl ExperimentalConfig {
    /// Load from the canonical path, with env-var override and clean
    /// fall-through-to-defaults if the file is absent.
    pub fn load() -> Self {
        let path = std::env::var("DCENTRALD_EXPERIMENTAL_TOML")
            .unwrap_or_else(|_| DEFAULT_PATH.to_string());
        Self::load_from(Path::new(&path))
    }

    /// Load from a specific path. Test entry point.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<ExperimentalConfig>(&s) {
                Ok(cfg) => {
                    tracing::info!(?path, "Loaded experimental config");
                    cfg
                }
                Err(e) => {
                    tracing::warn!(?path, error = %e, "experimental toml parse failed; using defaults");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    ?path,
                    "no experimental config (expected on production images)"
                );
                Self::default()
            }
            Err(e) => {
                tracing::warn!(?path, error = %e, "experimental config read failed; using defaults");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "dcentrald_experimental_test_{}_{}.toml",
            name,
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("write temp toml");
        path
    }

    #[test]
    fn default_is_strict() {
        let c = ExperimentalConfig::default();
        assert_eq!(c.rambo_mode_max_bad_responses, 0);
        assert!(!c.allow_disabling_hashboards_on_nopic_miners);
        assert_eq!(c.min_fan_pwm_floor, 25);
        assert!(!c.disable_bootstrap_check);
        assert!(!c.send_dummy_telemetry_on_startup);
    }

    #[test]
    fn missing_file_returns_default() {
        let c = ExperimentalConfig::load_from(Path::new("/this/path/does/not/exist/anywhere.toml"));
        assert_eq!(c.rambo_mode_max_bad_responses, 0);
    }

    #[test]
    fn rambo_mode_8_loads_for_partial_chain_use_case() {
        // .78 use case: chain1 sees 8/77 chips. Operator sets
        // rambo_mode_max_bad_responses = 8 to let init proceed.
        let path = write_temp(
            "rambo_8",
            "rambo_mode_max_bad_responses = 8\nmin_fan_pwm_floor = 30\n",
        );
        let c = ExperimentalConfig::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(c.rambo_mode_max_bad_responses, 8);
        assert_eq!(c.min_fan_pwm_floor, 30);
        // Other fields remain at defaults
        assert!(!c.allow_disabling_hashboards_on_nopic_miners);
    }

    #[test]
    fn malformed_toml_returns_default_not_panic() {
        let path = write_temp("malformed", "this is not valid toml at all { { {");
        let c = ExperimentalConfig::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(c.rambo_mode_max_bad_responses, 0);
    }
}
