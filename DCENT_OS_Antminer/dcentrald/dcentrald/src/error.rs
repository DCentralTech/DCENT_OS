//! Centralized error types for the dcentrald daemon.
//!
//! Uses thiserror for ergonomic error definitions. Each subsystem has its
//! own error type in its crate; this module defines the top-level daemon
//! errors that aggregate subsystem failures.

use thiserror::Error;

/// Top-level daemon errors.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// Configuration file error (missing, invalid, or unreadable).
    #[error("configuration error: {0}")]
    Config(String),

    /// Hardware abstraction layer failure.
    #[error("HAL error: {0}")]
    Hal(#[from] dcentrald_hal::HalError),

    /// ASIC driver subsystem failure.
    #[error("ASIC error: {0}")]
    Asic(#[from] dcentrald_asic::AsicError),

    /// Stratum protocol failure.
    #[error("Stratum error: {0}")]
    Stratum(#[from] dcentrald_stratum::StratumError),

    /// Thermal management failure.
    #[error("thermal error: {0}")]
    Thermal(#[from] dcentrald_thermal::ThermalError),

    /// Safety system triggered (thermal shutdown, fan failure, etc.).
    #[error("safety shutdown: {reason}")]
    SafetyShutdown { reason: String },

    /// Watchdog failure.
    #[error("watchdog error: {0}")]
    Watchdog(String),

    /// Generic I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
