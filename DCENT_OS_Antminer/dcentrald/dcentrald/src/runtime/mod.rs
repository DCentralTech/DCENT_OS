//! Runtime subsystems shared by every mining mode.
//!
//! W2.1 + W2.2 (Production Build Plan, 2026-05-07): the `Daemon` god-class in
//! `daemon.rs` was historically the only entry point that wired up the API
//! servers (`:8080` REST + `:4028` CGMiner), MQTT/webhook notifications,
//! share-efficiency telemetry, and hardware-info collection. Other mining
//! modes (`--s19j-hybrid`, `--tap-mode`, `--serial-mining`, `--stock-fpga`,
//! `--stratum-proxy`) bypassed `Daemon::run()` entirely and therefore never
//! brought up the dashboard —.
//!
//! This module hosts the post-W2.1 split:
//!
//! - [`api`] — `spawn_proxy_mode_api` and the minimal `AppState` builder used
//!   by every non-`Daemon` mode. Default trait implementation calls into
//!   here so the dashboard binds on every mode.
//! - [`notifications`] — MQTT + webhook + alert plumbing
//!   (`MqttPublisherTask`, `AlertEvent`, `spawn_mqtt_publisher`,
//!   `RuntimeNotificationConfig`).
//! - [`efficiency`] — `ShareEfficiencyTracker`, PSU efficiency tables,
//!   `now_unix_ms`.
//! - [`hardware_info`] — `collect_hardware_info`, `detect_control_board`,
//!   `read_miner_serial`, `read_hb_type`, `probe_psu_info`,
//!   `read_hashboard_eeprom_fingerprints`, EEPROM fingerprint helpers.
//!
//! ## `MiningRuntime` trait
//!
//! Every mining mode (`Daemon`, `S19jHybridMiner`, `S19jTapMiner`,
//! `SerialMiner`, `StockMiner`, and the `stratum_proxy` wrapper) implements
//! [`MiningRuntime`]. The trait provides default implementations for
//! API-server bring-up so a future mining mode can't accidentally
//! re-introduce the hybrid-mode-no-api regression.
//!
//! Direct hardware orchestration (PIC heartbeat, FPGA UIO open, voltage
//! ramp, single-I2C-owner service spawn) is **not** part of the trait —
//! that lives in mode-specific implementations under `bringup/`. The
//! trait is intentionally small: it is the operator-facing contract
//! ("this thing runs, binds an API, can be cancelled") and nothing more.

pub mod api;
pub mod efficiency;
pub mod hardware_info;
pub mod impls;
pub mod job_declaration;
pub mod notifications;
pub(crate) mod safety_watchdog;
#[cfg(test)]
pub(crate) mod source_contract;
pub(crate) mod task_guard;
pub(crate) mod thread_guard;

use std::future::Future;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::config::DcentraldConfig;

/// Common contract every mining-mode implementation MUST honour.
///
/// The trait is intentionally lean. It exists to:
///
/// 1. Force every new mining mode to expose a single `run(shutdown)` entry
///    point that owns its own lifecycle.
/// 2. Provide a default `start_proxy_mode_api` that spins up the dashboard +
///    CGMiner-API for modes that don't construct a full `Daemon`-style
///    `AppState` themselves. This closes
///    .
///
/// Implementations are free to skip the default API helper if they bring up
/// their own (e.g. the `Daemon` S9 path constructs `AppState` from live
/// telemetry channels and calls `dcentrald_api::start_api_servers` directly).
/// Modes that DO NOT have that infrastructure (`--s19j-hybrid`,
/// `--stratum-proxy`, `--serial-mining` idle path, `--tap-mode`,
/// `--stock-fpga`) get the dashboard for free via the default impl.
pub trait MiningRuntime: Send + 'static {
    /// Mode label used in `RuntimeHealthMode` and operator-facing log lines.
    fn mode_label(&self) -> &'static str;

    /// Owns the entire mining lifecycle. Returns when `shutdown` is
    /// cancelled or a fatal error occurs. MUST drain hardware safely on
    /// the way out (voltage off, fans on a sane PWM, watchdog refreshed).
    ///
    /// Each impl is free to use `async fn` directly (AFIT, stabilized in
    /// Rust 1.75). The return type is left implicit so impls can name
    /// their own concrete futures.
    fn run(self, shutdown: CancellationToken) -> impl Future<Output = Result<()>>;

    /// Default implementation that brings up the minimal API surface.
    ///
    /// Modes that don't construct their own full `AppState` should call
    /// this from inside their `run()` BEFORE entering the work-dispatch
    /// loop. The returned `JoinHandle`s must be retained and explicitly joined
    /// or aborted; dropping a Tokio handle detaches its still-running task.
    ///
    /// This is the load-bearing piece of the trait. Per
    /// , every non-`Daemon` mode
    /// previously skipped `start_api_servers`, leaving the dashboard
    /// unreachable while the daemon was alive. The default impl makes that
    /// regression a compile-time invariant — adding a new `MiningRuntime`
    /// gets the API for free.
    fn default_api_servers(
        &self,
        config: &DcentraldConfig,
        runtime_mode: dcentrald_api::RuntimeHealthMode,
        runtime_health_rx: Option<
            tokio::sync::watch::Receiver<dcentrald_api::RuntimeHealthSnapshot>,
        >,
        shutdown: CancellationToken,
    ) -> impl Future<Output = Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)>>
    {
        // P1-4 (Omega): forwards a shutdown token so the shared notification
        // stack `spawn_proxy_mode_api` brings up (MQTT + webhook dispatcher) is
        // cancelled with the rest of the mode.
        api::spawn_proxy_mode_api(config.clone(), runtime_mode, runtime_health_rx, shutdown)
    }
}

/// Thin wrapper that boxes a runtime + cancellation token together so the
/// caller can store `Box<dyn ErasedRuntime>` heterogeneously when needed.
///
/// We don't use this on the main hot path (each mining mode is constructed
/// via its concrete type and called directly), but tests + future fleet
/// orchestration may want a homogeneous handle.
pub trait ErasedRuntime: Send {
    fn mode_label(&self) -> &'static str;
}

impl<T: MiningRuntime> ErasedRuntime for T {
    fn mode_label(&self) -> &'static str {
        MiningRuntime::mode_label(self)
    }
}
