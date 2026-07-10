//!  bypass-A — PSU + hashboard bypass policy (HAL-free).
//!
//! Source RE evidence:
//!
//! (757 lines).
//!
//! Stock Bitmain firmware refuses to start the mining daemon if either:
//! 1. The PSU model-string whitelist check fails (e.g. an APW3++
//!    physically connected where firmware expects APW12).
//! 2. Fewer than 3 hashboards report a PLUGO signal (common when
//!    running 1-board home-mining configurations).
//!
//! Pivotal Pleb's Loki board is a hardware spoofer that works around
//! both checks. **DCENT_OS solves both in firmware** — that's the
//! design advantage of writing the firmware from scratch.
//!
//! This module captures:
//! - Per-firmware Loki-required matrix (stock Bitmain / BraiinsOS /
//!   VNish / LuxOS / DCENT_OS).
//! - Per-platform PLUGO behavior (Xilinx / Amlogic / BB).
//! - Operator-pickable bypass policies (Off / PsuOnly / FullBypass).
//!
//! HAL-free. The runtime adapter consumes this to decide whether to
//! enforce PSU validation, accept partial hashboard population, etc.

use serde::{Deserialize, Serialize};

/// Discrete miner firmware identifiers for the Loki-required matrix
/// (RE doc §2 lines 68-75).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinerFirmware {
    /// Bitmain stock firmware.
    BitmainStock,
    /// BraiinsOS / BraiinsOS+.
    BraiinsOs,
    /// VNish.
    Vnish,
    /// LuxOS.
    LuxOs,
    /// DCENT_OS — designed without these checks from day 1.
    DcentOs,
}

/// Operator-pickable PSU bypass policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BypassMode {
    /// Strict: enforce the firmware's documented PSU + hashboard checks.
    /// Useful when the operator wants to mirror stock behavior.
    Off,
    /// Skip PSU model-string whitelist; still require all hashboards
    /// detected (PLUGO present on every populated slot).
    PsuOnly,
    /// Skip PSU model-string AND accept partial hashboard population
    /// (1 or 2 boards instead of 3). The 120 V single-board scenario.
    FullBypass,
}

/// Capabilities of one (firmware, platform) combination per RE doc §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct LokiNeed {
    /// Whether this firmware needs Loki-PSS for PSU model spoofing.
    pub needs_psu_spoof: bool,
    /// Whether this firmware needs Loki-HBS for hashboard detection.
    pub needs_hashboard_spoof: bool,
    /// Operator-facing recommendation.
    pub label: &'static str,
}

/// Per-firmware Loki-required matrix (RE doc §2 lines 68-75).
pub fn loki_need(firmware: MinerFirmware) -> LokiNeed {
    match firmware {
        MinerFirmware::BitmainStock => LokiNeed {
            needs_psu_spoof: true,
            needs_hashboard_spoof: false,
            label: "Bitmain stock — Loki-PSS required",
        },
        MinerFirmware::BraiinsOs => LokiNeed {
            needs_psu_spoof: true,
            needs_hashboard_spoof: true,
            label: "BraiinsOS — Loki-Duo (PSS + HBS) required",
        },
        MinerFirmware::Vnish => LokiNeed {
            needs_psu_spoof: true,
            needs_hashboard_spoof: true,
            label: "VNish — Loki-Duo (PSS + HBS) required",
        },
        MinerFirmware::LuxOs => LokiNeed {
            needs_psu_spoof: false,
            needs_hashboard_spoof: false,
            label: "LuxOS — has built-in PSU bypass mode",
        },
        MinerFirmware::DcentOs => LokiNeed {
            needs_psu_spoof: false,
            needs_hashboard_spoof: false,
            label: "DCENT_OS — solves both in firmware (no Loki needed)",
        },
    }
}

/// Whether the firmware needs ANY Loki hardware to run.
pub fn needs_loki(firmware: MinerFirmware) -> bool {
    let n = loki_need(firmware);
    n.needs_psu_spoof || n.needs_hashboard_spoof
}

/// Whether the bypass mode permits partial hashboard population
/// (1 or 2 boards instead of all 3).
pub fn allows_partial_hashboard_population(mode: BypassMode) -> bool {
    matches!(mode, BypassMode::FullBypass)
}

/// Whether the bypass mode permits skipping the PSU model whitelist.
pub fn allows_psu_model_skip(mode: BypassMode) -> bool {
    matches!(mode, BypassMode::PsuOnly | BypassMode::FullBypass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dcent_os_never_needs_loki() {
        // The whole point of DCENT_OS — solve both problems in firmware.
        assert!(!needs_loki(MinerFirmware::DcentOs));
        let n = loki_need(MinerFirmware::DcentOs);
        assert!(!n.needs_psu_spoof);
        assert!(!n.needs_hashboard_spoof);
    }

    #[test]
    fn lux_os_never_needs_loki() {
        // RE doc §1: LuxOS already has built-in PSU bypass mode.
        assert!(!needs_loki(MinerFirmware::LuxOs));
    }

    #[test]
    fn bitmain_stock_needs_psu_only() {
        // RE doc §2 row 1: Loki-Lite (PSS only) works with stock.
        let n = loki_need(MinerFirmware::BitmainStock);
        assert!(n.needs_psu_spoof);
        assert!(!n.needs_hashboard_spoof);
    }

    #[test]
    fn braiins_os_and_vnish_need_loki_duo() {
        // RE doc §2 row 2: BraiinsOS and VNish both need PSS + HBS.
        for fw in [MinerFirmware::BraiinsOs, MinerFirmware::Vnish] {
            let n = loki_need(fw);
            assert!(n.needs_psu_spoof, "{:?} should need PSU spoof", fw);
            assert!(n.needs_hashboard_spoof, "{:?} should need HBS", fw);
            assert!(needs_loki(fw));
        }
    }

    #[test]
    fn bypass_off_strict_about_both_checks() {
        assert!(!allows_psu_model_skip(BypassMode::Off));
        assert!(!allows_partial_hashboard_population(BypassMode::Off));
    }

    #[test]
    fn bypass_psu_only_allows_psu_skip_only() {
        assert!(allows_psu_model_skip(BypassMode::PsuOnly));
        assert!(!allows_partial_hashboard_population(BypassMode::PsuOnly));
    }

    #[test]
    fn bypass_full_allows_both() {
        assert!(allows_psu_model_skip(BypassMode::FullBypass));
        assert!(allows_partial_hashboard_population(BypassMode::FullBypass));
    }

    #[test]
    fn miner_firmware_round_trips_through_serde() {
        for fw in [
            MinerFirmware::BitmainStock,
            MinerFirmware::BraiinsOs,
            MinerFirmware::Vnish,
            MinerFirmware::LuxOs,
            MinerFirmware::DcentOs,
        ] {
            let json = serde_json::to_string(&fw).unwrap();
            let back: MinerFirmware = serde_json::from_str(&json).unwrap();
            assert_eq!(fw, back);
        }
    }

    #[test]
    fn bypass_mode_round_trips_through_serde() {
        for m in [BypassMode::Off, BypassMode::PsuOnly, BypassMode::FullBypass] {
            let json = serde_json::to_string(&m).unwrap();
            let back: BypassMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn loki_need_serializes_to_documented_shape() {
        let n = loki_need(MinerFirmware::BraiinsOs);
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains("\"needs_psu_spoof\":true"));
        assert!(json.contains("\"needs_hashboard_spoof\":true"));
        assert!(json.contains("Loki-Duo"));
    }
}
