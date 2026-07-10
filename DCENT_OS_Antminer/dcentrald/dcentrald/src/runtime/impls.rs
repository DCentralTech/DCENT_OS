//! `MiningRuntime` trait implementations for each mining-mode type.
//!
//! W2.2 (2026-05-07): each `*Miner::run()` keeps its existing async body; the
//! trait impl simply forwards to it. The interesting bit is the
//! `default_api_servers()` method on the trait: by implementing
//! `MiningRuntime` for every mining mode, future agents adding a new mode
//! cannot accidentally bypass the dashboard bring-up — the trait method
//! is in scope and the type doesn't compile without it.
//!
//! Per-mode lifecycle wiring (cancellation token storage, hardware shutdown
//! order, fan PWM during shutdown) stays in each mode's existing
//! `pub async fn run(&mut self)` and is deliberately NOT moved into the
//! trait, because each mode has different graceful-shutdown ordering
//! requirements (S9 needs voltage-off-before-fan-quiet, am2 hybrid needs
//! the I2C service drained before the chain UART is closed, etc.).

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use super::MiningRuntime;
use crate::daemon::Daemon;
use crate::s19j_hybrid_mining::S19jHybridMiner;
use crate::s19j_tap_mining::S19jTapMiner;
use crate::serial_mining::SerialMiner;
use crate::stock_mining::StockMiner;

impl MiningRuntime for Daemon {
    fn mode_label(&self) -> &'static str {
        "zynq-s9-or-am2-passthrough"
    }

    async fn run(mut self, _shutdown: CancellationToken) -> Result<()> {
        // The Daemon already owns its CancellationToken (passed via `new`)
        // and its `pub async fn run(&mut self)` ignores any external
        // override. We honor that contract.
        Daemon::run(&mut self).await
    }
}

impl MiningRuntime for S19jHybridMiner {
    fn mode_label(&self) -> &'static str {
        "s19j-hybrid"
    }

    async fn run(mut self, _shutdown: CancellationToken) -> Result<()> {
        S19jHybridMiner::run(&mut self).await
    }
}

impl MiningRuntime for S19jTapMiner {
    fn mode_label(&self) -> &'static str {
        "s19j-tap"
    }

    async fn run(mut self, _shutdown: CancellationToken) -> Result<()> {
        S19jTapMiner::run(&mut self).await
    }
}

impl MiningRuntime for SerialMiner {
    fn mode_label(&self) -> &'static str {
        "serial-mining"
    }

    async fn run(mut self, _shutdown: CancellationToken) -> Result<()> {
        SerialMiner::run(&mut self).await
    }
}

impl MiningRuntime for StockMiner {
    fn mode_label(&self) -> &'static str {
        "stock-fpga"
    }

    async fn run(mut self, _shutdown: CancellationToken) -> Result<()> {
        StockMiner::run(&mut self).await
    }
}

/// Newtype wrapper around the free `stratum_proxy::run(...)` function so it
/// can implement `MiningRuntime` like the other modes. This is the
/// `Stratum V1 TCP relay` mode — bosminer keeps full control of hardware,
/// dcentrald only proxies the byte stream upstream.
pub struct StratumProxyRuntime {
    config: crate::config::DcentraldConfig,
    stats: std::sync::Arc<crate::stratum_proxy::ProxiedStats>,
}

impl StratumProxyRuntime {
    pub fn new(
        config: crate::config::DcentraldConfig,
        stats: std::sync::Arc<crate::stratum_proxy::ProxiedStats>,
    ) -> Self {
        Self { config, stats }
    }
}

impl MiningRuntime for StratumProxyRuntime {
    fn mode_label(&self) -> &'static str {
        "stratum-proxy"
    }

    async fn run(self, shutdown: CancellationToken) -> Result<()> {
        crate::stratum_proxy::run(self.config, shutdown, Some(self.stats)).await
    }
}
