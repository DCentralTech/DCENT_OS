//! Thermal management subsystem for dcentrald.
//!
//! Provides PID-based thermal control, fan speed management, ATM profiles,
//! curtailment (sleep/wake), and space heater mode support.
//!
//! The thermal controller runs as a 5-second interval Tokio task. It reads
//! temperature sensors, applies PID control to adjust fan speed, and
//! throttles frequency if temperatures exceed thresholds.
//!
//! Modules:
//! - `controller` - PID-based thermal control loop
//! - `profiles`   - ATM thermal profiles and power presets
//! - `curtailment` - Sleep/wake for demand response
//! - `heater`     - Space heater mode (power targeting, room temp, BTU)

pub mod battery;
pub mod controller;
pub mod curtailment;
// Per-chip die-temperature calibration (R-13, BM1362 / am2-s19jpro-zynq).
// DEFAULT-OFF, fail-safe-to-raw calibration of the on-die ADC against an
// absolute PCB sensor at a cold baseline. A wrong calibration must never
// under-report (hide an over-temp), so the safety-facing reading is guaranteed
// never below raw. Pure/HAL-free and host-testable. The daemon wires it into
// the am2 hybrid supervisor's XADC die read; see `die_calibration.rs`.
pub mod die_calibration;
pub mod heater;
// Immersion / hydro cooling mode (W8 parity gap: DCENT ❌/⚠️ vs LuxOS/VNish ✅).
// EXPLICIT, default-OFF opt-in that bypasses air-fan RAMP behavior for
// immersion/hydro rigs (no chassis fans) while KEEPING the thermal SAFETY net
// (still monitors die/chip temp, still fails closed on dangerous/stale temp by
// cutting hash — never by blasting nonexistent fans). Refuses to activate on a
// platform that looks air-cooled unless the operator explicitly acknowledges.
pub mod immersion;
// Off-grid battery telemetry consumes `dcentrald_hal::adc::AdcReading`, so it is
// the only module gated behind the `hal` feature (default-on). Pure thermal logic
// (controller/supervisor/profiles/battery/curtailment/heater) stays HAL-free
// and host-testable with --no-default-features. (gap-swarm no-HAL hunt, finding #11)
#[cfg(feature = "hal")]
pub mod offgrid;
pub mod profiles;
// Wave E (RE-005 closure, 2026-05-19): clean-room LuxOS-shape thermal
// supervisor (6-layer FSM) layered on top of the existing controller PID
// loop. Compiled-but-not-instantiated this wave; opt-in at controller
// integration sites via TOML `[thermal.supervisor].enabled` (Wave G/H).
//
// and the internal Wave E planning notes (§E3).
pub mod supervisor;

use thiserror::Error;

/// Thermal subsystem error type.
#[derive(Debug, Error)]
pub enum ThermalError {
    /// HAL-level error (fan, I2C, GPIO). Gated behind the `hal` feature
    /// (default-on) so the crate compiles HAL-free on a non-Unix host; the
    /// `#[from] dcentrald_hal::HalError` conversion is generated only when the
    /// HAL is present. (gap-swarm no-HAL hunt, finding #11)
    #[cfg(feature = "hal")]
    #[error("HAL error: {0}")]
    Hal(#[from] dcentrald_hal::HalError),

    /// Temperature sensor read failure.
    #[error("temperature sensor error on chain {chain_id}: {detail}")]
    SensorFailure { chain_id: u8, detail: String },

    /// Fan failure detected (0 RPM while PWM > 0).
    #[error("fan failure detected: RPM=0 for {seconds}s while PWM={pwm}")]
    FanFailure { seconds: u32, pwm: u8 },

    /// Thermal shutdown triggered.
    #[error("thermal shutdown: chain {chain_id} reached {temp_c}C (limit={limit_c}C)")]
    ThermalShutdown {
        chain_id: u8,
        temp_c: f32,
        limit_c: u8,
    },

    /// Curtailment error.
    #[error("curtailment error: {0}")]
    Curtailment(String),
}

pub type Result<T> = std::result::Result<T, ThermalError>;

/// Thermal state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalState {
    /// temp < target: fans at minimum, frequency at target.
    ColdStart,

    /// target <= temp < hot: PID control active, normal operation.
    NormalMining,

    /// hot <= temp < dangerous: fans at max, frequency throttled.
    HotThrottle,

    /// temp >= dangerous: emergency shutdown, disable hash boards.
    DangerousShutdown,

    /// System in sleep/curtailment mode.
    Sleep,
}
