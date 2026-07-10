//! OperatingMode-based API access filtering middleware.
//!
//! Checks the active `OperatingMode` before serving requests and
//! blocks endpoints that are not available in the current mode.
//!
//! Mode-Conditional API Access:
//!
//!   Home Mode:
//!     - /api/status, /api/pools, /api/config, /api/system/info (allowed)
//!     - /api/home/* (allowed)
//!     - /api/action/sleep, /api/action/wake (allowed)
//!     - /api/diagnostics/* (allowed)
//!     - /api/debug/* (BLOCKED -- returns 403 with mode explanation)
//!
//!   Standard Mode:
//!     - All Home endpoints (allowed)
//!     - /api/stats, /api/profiles, /api/history (allowed)
//!     - /api/diagnostics/* (allowed)
//!     - /api/debug/* (BLOCKED -- returns 403 with mode explanation)
//!
//!   Hacker Mode:
//!     - All standard endpoints (allowed)
//!     - /api/debug/* (allowed)
//!     - Write operations require { "confirm": true } field

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use crate::OperatingMode;

/// Mode access check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeAccessDenied {
    /// Error description.
    pub error: String,
    /// Current operating mode.
    pub current_mode: String,
    /// Mode required for this endpoint.
    pub required_mode: String,
    /// Suggestion for the user.
    pub suggestion: String,
}

/// Endpoint access level requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    /// Available in all modes (status, pools, config, system, home, diagnostics).
    AllModes,
    /// Available in Standard and Hacker modes (stats, profiles, history).
    StandardOrHigher,
    /// Available only in Hacker mode (debug endpoints).
    HackerOnly,
}

/// Check if a given request path is allowed in the current operating mode.
///
/// Returns `Ok(())` if the endpoint is accessible, or an error response
/// with a 403 status code and explanation if blocked.
pub fn check_mode_access(
    path: &str,
    mode: OperatingMode,
) -> std::result::Result<(), Response<Body>> {
    let access_level = classify_endpoint(path);

    let allowed = match access_level {
        AccessLevel::AllModes => true,
        AccessLevel::StandardOrHigher => {
            matches!(mode, OperatingMode::Standard | OperatingMode::Hacker)
        }
        AccessLevel::HackerOnly => matches!(mode, OperatingMode::Hacker),
    };

    if allowed {
        Ok(())
    } else {
        let required = match access_level {
            AccessLevel::AllModes => "any",
            AccessLevel::StandardOrHigher => "standard",
            AccessLevel::HackerOnly => "hacker",
        };

        let denied = ModeAccessDenied {
            error: format!("Endpoint {} is not available in {} mode", path, mode),
            current_mode: mode.to_string(),
            required_mode: required.to_string(),
            suggestion: format!(
                "Switch to {} mode via POST /api/config {{ \"mode\": {{ \"active\": \"{}\" }} }}",
                required, required
            ),
        };

        let body = serde_json::to_string(&denied).unwrap_or_default();
        let response = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();

        Err(response)
    }
}

/// CE-063: autotuner *control* routes are Standard-or-higher. The autotuner
/// reads/planners (status, target, visibility, quota, room-temp-factor,
/// profitability, fleet-profile/export, noise-profile) stay AllModes so the
/// Home dashboard/onboarding/HA thermostat keep working; only the mutating
/// control routes that walk the live power/hashrate target are gated out of
/// Home mode. Exact-match (contains), NOT starts_with, so no read route is
/// over-blocked.
const AUTOTUNER_CONTROL_PATHS: &[&str] = &[
    "/api/autotuner/active",
    "/api/autotuner/increment_power_target",
    "/api/autotuner/decrement_power_target",
    "/api/autotuner/increment_hashrate_target",
    "/api/autotuner/decrement_hashrate_target",
    "/api/autotuner/set_default_hashrate_target",
];

/// Classify an endpoint path into its access level.
fn classify_endpoint(path: &str) -> AccessLevel {
    if path.starts_with("/api/debug/") {
        AccessLevel::HackerOnly
    } else if path.starts_with("/api/stats")
        || path.starts_with("/api/profiles")
        || path.starts_with("/api/history")
        // GROUP C (W8 parity): the persistent audit-log read-back is operator
        // forensics, classified the same as the volatile `/api/history/audit`
        // ring snapshot — Standard-or-higher (kept out of the minimal Home
        // surface).
        || path.starts_with("/api/audit-log")
        // CE-063: autotuner control (power/hashrate target walk, set-mode).
        || AUTOTUNER_CONTROL_PATHS.contains(&path)
        // CE-121: the singular V/F profile upload (plural "/api/profiles" prefix
        // above does not cover the singular "/api/profile/upload" path).
        || path.starts_with("/api/profile/upload")
    {
        AccessLevel::StandardOrHigher
    } else {
        AccessLevel::AllModes
    }
}

/// Check if a hacker mode write operation has the required confirmation.
///
/// In Hacker mode, write operations to debug endpoints require the
/// request body to include `{ "confirm": true }` as a safety gate.
pub fn check_hacker_confirmation(
    body: &serde_json::Value,
) -> std::result::Result<(), Response<Body>> {
    let confirmed = body
        .get("confirm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if confirmed {
        Ok(())
    } else {
        let error = serde_json::json!({
            "error": "Hacker mode write operation requires confirmation",
            "detail": "Include { \"confirm\": true } in the request body to proceed",
            "warning": "This operation may modify hardware registers or ASIC configuration. Incorrect values can damage hardware."
        });

        let body_str = serde_json::to_string(&error).unwrap_or_default();
        let response = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("Content-Type", "application/json")
            .body(Body::from(body_str))
            .unwrap();

        Err(response)
    }
}

/// Safety envelope defining mode-dependent limits.
///
/// Enforced by the mode middleware and safety systems to constrain
/// what API operations are allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyEnvelope {
    /// Emergency shutdown threshold temperature.
    pub dangerous_temp_c: u8,
    /// Maximum allowed ASIC frequency.
    pub max_frequency_mhz: u16,
    /// Allow frequency above model default.
    pub allow_overclock: bool,
    /// Allow direct register read/write via API.
    pub allow_raw_registers: bool,
    /// Fan behavior mode.
    pub fan_mode: FanMode,
    /// Minimum fan PWM floor.
    pub min_fan_pwm: u8,
    /// Hard power cap in watts.
    pub max_power_watts: u32,
}

/// Fan behavior mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FanMode {
    /// PID targets lower RPM, prioritizes quiet operation (Home mode).
    NoiseOptimized,
    /// PID uses full PWM range for maximum cooling (Standard/Hacker).
    FullRange,
}

impl SafetyEnvelope {
    /// Get the safety envelope for Home mode.
    ///
    /// Conservative limits for residential 120V deployment.
    /// 1200W max keeps the miner within a 15A circuit's safe range
    /// (1200W + ~200W PSU overhead = 1400W < 1440W = 120V × 12A continuous).
    pub fn home() -> Self {
        Self {
            dangerous_temp_c: 70,
            max_frequency_mhz: 650, // Model default
            allow_overclock: false,
            allow_raw_registers: false,
            fan_mode: FanMode::NoiseOptimized,
            min_fan_pwm: 10, // low home command; physical RPM is platform-dependent
            max_power_watts: 1200,
        }
    }

    /// Get the safety envelope for Standard mode.
    pub fn standard() -> Self {
        Self {
            dangerous_temp_c: 75,
            max_frequency_mhz: 700, // Model max
            allow_overclock: false,
            allow_raw_registers: false,
            fan_mode: FanMode::FullRange,
            min_fan_pwm: 0, // ~900 RPM
            max_power_watts: 1400,
        }
    }

    /// Get the safety envelope for Hacker mode.
    ///
    /// Relaxed limits for power users. Still capped at 1800W absolute max
    /// for residential safety (120V × 15A = 1800W circuit max).
    pub fn hacker() -> Self {
        Self {
            dangerous_temp_c: 85, // User-overridable via config
            max_frequency_mhz: 900,
            allow_overclock: true,
            allow_raw_registers: true,
            fan_mode: FanMode::FullRange,
            min_fan_pwm: 0,
            max_power_watts: 1800,
        }
    }

    /// Get the safety envelope for a given operating mode.
    pub fn for_mode(mode: OperatingMode) -> Self {
        match mode {
            OperatingMode::Home => Self::home(),
            OperatingMode::Standard => Self::standard(),
            OperatingMode::Hacker => Self::hacker(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_mode_blocks_autotuner_control_routes() {
        for path in AUTOTUNER_CONTROL_PATHS {
            let r = check_mode_access(path, OperatingMode::Home);
            assert!(r.is_err(), "{path} should be blocked in Home mode");
            assert_eq!(
                r.unwrap_err().status(),
                StatusCode::FORBIDDEN,
                "{path} must return 403"
            );
        }
    }

    #[test]
    fn standard_and_hacker_allow_autotuner_control_routes() {
        for path in AUTOTUNER_CONTROL_PATHS {
            assert!(check_mode_access(path, OperatingMode::Standard).is_ok());
            assert!(check_mode_access(path, OperatingMode::Hacker).is_ok());
        }
    }

    #[test]
    fn home_mode_keeps_autotuner_read_and_planner_routes() {
        for path in [
            "/api/autotuner/status",
            "/api/autotuner/target",
            "/api/autotuner/visibility",
            "/api/autotuner/quota",
            "/api/autotuner/room-temp-factor",
        ] {
            assert!(
                check_mode_access(path, OperatingMode::Home).is_ok(),
                "{path} must stay usable in Home mode (dashboard/onboarding/HA)"
            );
        }
    }

    #[test]
    fn home_blocks_singular_vf_profile_upload_but_not_download() {
        // CE-121: singular upload path is Standard-or-higher.
        assert!(check_mode_access("/api/profile/upload", OperatingMode::Home).is_err());
        assert!(check_mode_access("/api/profile/upload", OperatingMode::Standard).is_ok());
        // Download stays open (read-only).
        assert!(check_mode_access("/api/profile/download", OperatingMode::Home).is_ok());
    }

    #[test]
    fn debug_routes_stay_hacker_only() {
        assert!(check_mode_access("/api/debug/registers", OperatingMode::Home).is_err());
        assert!(check_mode_access("/api/debug/registers", OperatingMode::Standard).is_err());
        assert!(check_mode_access("/api/debug/registers", OperatingMode::Hacker).is_ok());
    }
}
