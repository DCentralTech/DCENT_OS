//! Work dispatch pipeline — distributes pool jobs to ASIC chains and collects nonces.
//!
//! The work dispatcher is the core of the mining pipeline. It:
//! 1. Receives `JobTemplate` from the Stratum V1 client
//! 2. Generates `MiningWork` using the `WorkBuilder` (midstates, merkle root)
//! 3. Dispatches work to each active ASIC chain via FPGA WORK_TX_FIFO
//! 4. Polls WORK_RX_FIFO for nonce results
//! 5. Validates nonces against the pool's share target
//! 6. Submits valid shares back to the Stratum client
//! 7. Updates the MinerState watch channel with hashrate and statistics
//!
//! The dispatcher runs as a Tokio task, using `tokio::select!` to concurrently
//! handle new jobs, nonce polling, and cancellation.
//!
//! FPGA I/O is memory-mapped (volatile read/write, nanosecond latency), so it
//! runs safely in async context without blocking the Tokio runtime.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use dcentrald_asic::chain::Chain;
use dcentrald_asic::drivers::bm1362::Bm1362Driver;
use dcentrald_asic::drivers::bm1366::Bm1366Driver;
use dcentrald_asic::drivers::bm1368::Bm1368Driver;
use dcentrald_asic::drivers::bm1370::Bm1370Driver;
use dcentrald_asic::drivers::bm1387::Bm1387Driver;
use dcentrald_asic::drivers::bm1397::Bm1397Driver;
use dcentrald_asic::drivers::bm1398::Bm1398Driver;
use dcentrald_asic::drivers::{
    ChipDriverExecutionPolicy, ChipRegistry, FpgaNonceDecodeContext, MinerProfile,
    MiningWork as AsicWork, PicType,
};
use dcentrald_autotuner::chip_stats::{
    ChipNonceTracker, ChipStatsSnapshot, BOARD_TEMP_STALE_TIMEOUT_S,
};
use dcentrald_autotuner::power_budget::DispatcherChainLimit;
use dcentrald_autotuner::power_budget::PowerModel;
use dcentrald_autotuner::power_budget::RuntimeWattCapState;
use dcentrald_autotuner::power_budget::{efficiency_jth_from, EfficiencyHashrateEma};
use dcentrald_autotuner::{FreqCommand, FrequencyLimitSource, LivePowerEstimate, PowerCalibration};
use dcentrald_hal::led::LedCommand;
use dcentrald_stratum::share_pipeline::WorkBuilder;
use dcentrald_stratum::types::{JobTemplate, ValidShare};

use crate::asic_identity_publication::{ActiveCompositionSession, AsicIdentityPublicationPort};
use crate::runtime::task_guard::RuntimeTaskGuard;
use crate::voltage_mailbox::{VoltageCommandSender, VoltageTrySendError};
use crate::work_ledger::{ChainWorkLedger, LedgerLookup};

const MIN_RUNTIME_FREQ_MHZ: u16 = 200;
const RECENT_WORK_ID_SLOT_GUARD: Duration = Duration::from_secs(5);
const VOLTAGE_REPLY_TASK_STOP_TIMEOUT: Duration = Duration::from_millis(250);

/// Production chip identities allowed to cross the dispatcher's hardware-write
/// boundary. Keeping this closed and typed prevents an unknown or missing ID
/// from inheriting BM1387 work timing, PLL floors, version policy, or packet
/// selection through a wildcard/default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchWriteChip {
    Bm1362,
    Bm1366,
    Bm1368,
    Bm1370,
    Bm1387,
    Bm1397,
    Bm1398,
}

impl DispatchWriteChip {
    fn chip_id(self) -> u16 {
        match self {
            Self::Bm1362 => 0x1362,
            Self::Bm1366 => 0x1366,
            Self::Bm1368 => 0x1368,
            Self::Bm1370 => 0x1370,
            Self::Bm1387 => 0x1387,
            Self::Bm1397 => 0x1397,
            Self::Bm1398 => 0x1398,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchWriteIdentityError {
    Missing,
    Unsupported(u16),
    Mixed {
        dispatcher_chip_id: u16,
        chain_chip_id: u16,
    },
}

impl std::fmt::Display for DispatchWriteIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "ASIC chip identity is missing"),
            Self::Unsupported(chip_id) => {
                write!(f, "ASIC chip identity 0x{chip_id:04X} is not production-dispatchable")
            }
            Self::Mixed {
                dispatcher_chip_id,
                chain_chip_id,
            } => write!(
                f,
                "dispatcher chip 0x{dispatcher_chip_id:04X} disagrees with chain chip 0x{chain_chip_id:04X}"
            ),
        }
    }
}

impl TryFrom<u16> for DispatchWriteChip {
    type Error = DispatchWriteIdentityError;

    fn try_from(chip_id: u16) -> std::result::Result<Self, Self::Error> {
        match chip_id {
            0 => Err(DispatchWriteIdentityError::Missing),
            0x1362 => Ok(Self::Bm1362),
            0x1366 => Ok(Self::Bm1366),
            0x1368 => Ok(Self::Bm1368),
            0x1370 => Ok(Self::Bm1370),
            0x1387 => Ok(Self::Bm1387),
            0x1397 => Ok(Self::Bm1397),
            0x1398 => Ok(Self::Bm1398),
            other => Err(DispatchWriteIdentityError::Unsupported(other)),
        }
    }
}

/// Work tracking entry for matching nonces back to jobs.
#[derive(Clone)]
struct WorkEntry {
    /// Job ID from the pool (for share submission).
    job_id: String,
    /// Extranonce2 used for this work unit (hex string).
    extranonce2: String,
    /// ntime used for this work unit.
    ntime: u32,
    /// Base block version (before any rolling). Used to compute full rolled version
    /// for SV2 share submission.
    version: u32,
    /// Share target from the pool (big-endian, 32 bytes).
    share_target: [u8; 32],
    /// Per-midstate version bits for version rolling (hex string).
    /// Index 0 = original version, 1-3 = rolled versions.
    version_bits_per_midstate: Vec<Option<String>>,
    /// Pool-advertised version-rolling mask for this work item.
    /// Used to fail safe on chip families whose FPGA nonce path cannot
    /// reconstruct the rolled header version during share submission.
    pool_version_mask: u32,
    /// SHA-256 midstates — one per version-rolled variant.
    /// The FPGA cycles through these; nonce response includes solution_id
    /// indicating which midstate was used. Must validate against the CORRECT one.
    midstates: Vec<[u8; 32]>,
    /// Full merkle root for dispatcher-side full-header validation on BM1398.
    merkle_root: [u8; 32],
    /// Previous block hash in header byte order.
    prev_block_hash: [u8; 32],
    /// Last 12 bytes of block header before nonce: merkle4(4) + ntime(4) + nbits(4).
    header_tail: [u8; 12],
}

#[derive(Debug, Clone)]
pub enum VoltageCommandReply {
    Applied(u16),
    Verified(Option<u16>),
    Disabled,
}

#[derive(Debug)]
pub enum VoltageCommand {
    SetVoltage {
        chain_id: Option<u8>,
        chip_id: u16,
        pic_addr: u8,
        target_mv: u16,
        reply_tx: Option<oneshot::Sender<std::result::Result<VoltageCommandReply, String>>>,
    },
    DisableVoltage {
        chain_id: Option<u8>,
        chip_id: u16,
        pic_addr: u8,
        reply_tx: Option<oneshot::Sender<std::result::Result<VoltageCommandReply, String>>>,
    },
    VerifyVoltage {
        chain_id: Option<u8>,
        chip_id: u16,
        pic_addr: u8,
        target_mv: u16,
        reply_tx: Option<oneshot::Sender<std::result::Result<VoltageCommandReply, String>>>,
    },
}

/// Hashrate tracking using exponential moving average.
struct HashrateTracker {
    /// Nonces found in the current window.
    window_nonces: u64,
    /// Start of the current window.
    window_start: Instant,
    /// Window duration.
    window_duration: Duration,
    /// 5-second rolling hashrate (GH/s).
    hashrate_5s: f64,
    /// Long-term hashrate (GH/s).
    hashrate_avg: f64,
    /// EMA smoothing factor.
    ema_alpha: f64,
    /// Total nonces found since start.
    total_nonces: u64,
    /// Per-chain nonce counts (for per-chain hashrate).
    chain_nonces: Vec<u64>,
    /// Per-chain window start.
    chain_window_start: Vec<Instant>,
    /// Per-chain 5s hashrate.
    chain_hashrate: Vec<f64>,
}

enum PendingVoltageResult {
    Apply {
        chain_id: u8,
        requested_mv: u16,
        pic_type: PicType,
        pic_addr: u8,
        timed_out: bool,
        ack_tx: Option<oneshot::Sender<std::result::Result<u16, String>>>,
        result: std::result::Result<u16, String>,
    },
    Verify {
        chain_id: u8,
        target_mv: u16,
        pic_type: PicType,
        pic_addr: u8,
        timed_out: bool,
        ack_tx: Option<oneshot::Sender<std::result::Result<Option<u16>, String>>>,
        result: std::result::Result<Option<u16>, String>,
    },
}

impl HashrateTracker {
    fn new(num_chains: usize) -> Self {
        let now = Instant::now();
        Self {
            window_nonces: 0,
            window_start: now,
            window_duration: Duration::from_secs(5),
            hashrate_5s: 0.0,
            hashrate_avg: 0.0,
            ema_alpha: 0.1,
            total_nonces: 0,
            chain_nonces: vec![0; num_chains],
            chain_window_start: vec![now; num_chains],
            chain_hashrate: vec![0.0; num_chains],
        }
    }

    /// Record a nonce found on a specific chain.
    /// `difficulty` is the hardware difficulty (TicketMask + 1), typically 256 for BM1387.
    fn record_nonce(&mut self, chain_idx: usize, difficulty: u64) {
        self.window_nonces += 1;
        self.total_nonces += 1;

        if chain_idx < self.chain_nonces.len() {
            self.chain_nonces[chain_idx] += 1;
        }
    }

    /// Update hashrate calculations. Call this periodically (e.g., every 5 seconds).
    /// `hw_difficulty` is the TicketMask difficulty (e.g., 256 for BM1387).
    fn update(&mut self, hw_difficulty: u64) {
        let elapsed = self.window_start.elapsed();
        if elapsed >= self.window_duration {
            let seconds = elapsed.as_secs_f64();

            // Hashrate = nonces * hw_difficulty * 2^32 / time / 1e9 (GH/s)
            // Each nonce represents hw_difficulty * 2^32 hashes of search space
            let hashes = self.window_nonces as f64 * hw_difficulty as f64 * 4_294_967_296.0;
            self.hashrate_5s = hashes / seconds / 1e9;

            // EMA for smoothed average.
            // FIX (2026-04-11): Use higher alpha during warmup so the EMA converges
            // faster. With alpha=0.1 and 5s windows, it takes ~50s to reach 90% of
            // true hashrate — too slow for dashboard and autotuner. alpha=0.5 during
            // first 1000 nonces converges in ~10s.
            if self.hashrate_avg == 0.0 {
                self.hashrate_avg = self.hashrate_5s;
            } else {
                let alpha = if self.total_nonces < 1000 {
                    0.5
                } else {
                    self.ema_alpha
                };
                self.hashrate_avg = alpha * self.hashrate_5s + (1.0 - alpha) * self.hashrate_avg;
            }

            // Reset window
            self.window_nonces = 0;
            self.window_start = Instant::now();

            // Per-chain hashrate
            for i in 0..self.chain_nonces.len() {
                let chain_elapsed = self.chain_window_start[i].elapsed().as_secs_f64();
                if chain_elapsed > 0.0 {
                    let chain_hashes =
                        self.chain_nonces[i] as f64 * hw_difficulty as f64 * 4_294_967_296.0;
                    self.chain_hashrate[i] = chain_hashes / chain_elapsed / 1e9;
                }
                self.chain_nonces[i] = 0;
                self.chain_window_start[i] = Instant::now();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ChainFrequencyLimits {
    fan_clamp: Option<u16>,
    thermal: Option<u16>,
    autotuner_thermal: Option<u16>,
    sensor_safety: Option<u16>,
    quiet_mode: Option<u16>,
    off_grid: Option<u16>,
    solar_surplus: Option<u16>,
    power_cap: Option<u16>,
    /// LuxOS-shape ATM (thermal-supervisor) profile-step ceiling. Default-off
    /// (only set when the operator-gated thermal supervisor is enabled). A
    /// dedicated slot so it composes with — never clobbers — the autotuner's
    /// own `autotuner_thermal` ceiling and the controller's `thermal` throttle.
    atm_step: Option<u16>,
}

impl ChainFrequencyLimits {
    fn merge_min(self, other: Self) -> Self {
        fn min_opt(a: Option<u16>, b: Option<u16>) -> Option<u16> {
            match (a, b) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            }
        }

        Self {
            fan_clamp: min_opt(self.fan_clamp, other.fan_clamp),
            thermal: min_opt(self.thermal, other.thermal),
            autotuner_thermal: min_opt(self.autotuner_thermal, other.autotuner_thermal),
            sensor_safety: min_opt(self.sensor_safety, other.sensor_safety),
            quiet_mode: min_opt(self.quiet_mode, other.quiet_mode),
            off_grid: min_opt(self.off_grid, other.off_grid),
            solar_surplus: min_opt(self.solar_surplus, other.solar_surplus),
            power_cap: min_opt(self.power_cap, other.power_cap),
            atm_step: min_opt(self.atm_step, other.atm_step),
        }
    }

    fn effective_ceiling(self) -> Option<u16> {
        [
            self.fan_clamp,
            self.thermal,
            self.autotuner_thermal,
            self.sensor_safety,
            self.quiet_mode,
            self.off_grid,
            self.solar_surplus,
            self.power_cap,
            self.atm_step,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn set(&mut self, source: FrequencyLimitSource, value: Option<u16>) -> bool {
        let slot = match source {
            FrequencyLimitSource::FanClamp => &mut self.fan_clamp,
            FrequencyLimitSource::Thermal => &mut self.thermal,
            FrequencyLimitSource::AutotunerThermal => &mut self.autotuner_thermal,
            FrequencyLimitSource::SensorSafety => &mut self.sensor_safety,
            FrequencyLimitSource::QuietMode => &mut self.quiet_mode,
            FrequencyLimitSource::OffGrid => &mut self.off_grid,
            FrequencyLimitSource::SolarSurplus => &mut self.solar_surplus,
            FrequencyLimitSource::PowerCap => &mut self.power_cap,
            FrequencyLimitSource::AtmStep => &mut self.atm_step,
        };
        let changed = *slot != value;
        *slot = value;
        changed
    }

    fn active_source_labels(self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.sensor_safety.is_some() {
            labels.push("sensor_safety");
        }
        if self.thermal.is_some() || self.autotuner_thermal.is_some() || self.atm_step.is_some() {
            labels.push("thermal");
        }
        if self.off_grid.is_some() {
            labels.push("off_grid");
        }
        if self.solar_surplus.is_some() {
            labels.push("solar_surplus");
        }
        if self.power_cap.is_some() {
            labels.push("power_cap");
        }
        if self.quiet_mode.is_some() {
            labels.push("quiet_mode");
        }
        if self.fan_clamp.is_some() {
            labels.push("fan_clamp");
        }
        labels
    }

    fn dominant_source(self) -> Option<&'static str> {
        let mut candidates: Vec<(&'static str, u16, u8)> = Vec::new();
        if let Some(limit) = self.sensor_safety {
            candidates.push(("sensor_safety", limit, 0));
        }
        if let Some(limit) = [self.thermal, self.autotuner_thermal, self.atm_step]
            .into_iter()
            .flatten()
            .min()
        {
            candidates.push(("thermal", limit, 1));
        }
        if let Some(limit) = self.off_grid {
            candidates.push(("off_grid", limit, 2));
        }
        if let Some(limit) = self.power_cap {
            candidates.push(("power_cap", limit, 3));
        }
        if let Some(limit) = self.quiet_mode {
            candidates.push(("quiet_mode", limit, 4));
        }
        if let Some(limit) = self.fan_clamp {
            candidates.push(("fan_clamp", limit, 5));
        }

        candidates
            .into_iter()
            .min_by_key(|(_, ceiling, rank)| (*ceiling, *rank))
            .map(|(label, _, _)| label)
    }
}

/// Work dispatch state machine.
///
/// Owns the ASIC chains and drives the complete mining pipeline:
/// job reception → work generation → FPGA dispatch → nonce collection → share submission.
pub struct WorkDispatcher {
    /// Channel to receive new jobs from Stratum client.
    job_rx: mpsc::Receiver<JobTemplate>,
    /// Channel to send valid shares to Stratum client.
    share_tx: mpsc::Sender<ValidShare>,
    /// MinerState watch channel for updating hashrate/stats.
    state_tx: watch::Sender<dcentrald_api::MinerState>,
    /// Broadcast channel for Hacker Mode mining-sync events.
    mining_sync_tx: broadcast::Sender<String>,
    /// Shutdown token.
    shutdown: CancellationToken,
    /// Worker name for share submission.
    /// Arc<str> to avoid per-share String clone — the worker name is immutable for
    /// the lifetime of the dispatcher.
    worker_name: Arc<str>,
    /// Active mining chains (owned — the dispatcher is the sole user of FPGA).
    chains: Vec<Chain>,
    /// Detected chip ID (e.g., 0x1387 for BM1387).
    chip_id: u16,
    /// Immutable executable-driver authority inherited from the sealed startup
    /// composition. The dispatcher must not reconstruct a broader policy.
    driver_execution_policy: ChipDriverExecutionPolicy,
    /// Hardware difficulty (TicketMask + 1). Default 256 for BM1387.
    hw_difficulty: u64,
    /// Channel to send per-chip stats snapshots to the autotuner.
    autotune_stats_tx: Option<mpsc::Sender<ChipStatsSnapshot>>,
    /// Channel to receive frequency commands from the autotuner.
    freq_cmd_rx: Option<mpsc::Receiver<FreqCommand>>,
    /// Autotuner measurement window interval (seconds).
    autotune_window_s: u64,
    /// Safety-prioritized mailbox for platform-aware runtime voltage commands.
    voltage_cmd_tx: Option<VoltageCommandSender>,
    /// Gap 2: Shared XADC die temperature from thermal loop (f32 bits).
    xadc_temp: Option<Arc<AtomicU32>>,
    /// Per-chain board temperatures (f32 bits) via BM1387 I2C passthrough.
    /// Written by the dispatcher every 5s, read by the thermal loop.
    /// Vec index matches self.chains index. Value 0 = no reading yet.
    board_temps: Vec<Arc<AtomicU32>>,
    /// Seconds since `board_temp_time_base` for the last valid board-temp sample.
    board_temp_seen_at: Vec<Arc<AtomicU32>>,
    /// Shared monotonic reference for board-temp freshness.
    board_temp_time_base: Arc<Instant>,
    /// AXI bus coordination: heartbeat thread sets true during I2C, dispatcher skips FPGA writes.
    i2c_active: Arc<AtomicBool>,
    /// LED command sender for visual events (new block flash, pipeline heartbeat).
    led_tx: Option<mpsc::Sender<LedCommand>>,
    /// Per-chain per-chip frequencies tracking actual operating state.
    /// Updated when autotuner sends FreqCommand::SetChipFreq or SetChainFreq.
    /// Used every 5s to compute live power estimate.
    /// Outer index = chain position in self.chains (0..N), inner = chip index.
    chip_frequencies: Vec<Vec<u16>>,
    /// Desired per-chip frequencies before thermal/night/off-grid/power ceilings.
    /// The dispatcher re-applies these targets when a ceiling is relaxed.
    desired_chip_frequencies: Vec<Vec<u16>>,
    /// Per-chain runtime frequency ceilings owned by the dispatcher.
    frequency_limits: Vec<ChainFrequencyLimits>,
    /// Per-chip runtime frequency ceilings owned by the dispatcher.
    chip_frequency_limits: Vec<Vec<ChainFrequencyLimits>>,
    /// Live power estimate output channel — read by REST API and WebSocket.
    power_tx: watch::Sender<LivePowerEstimate>,
    /// PSU efficiency for power estimation (0.88 for 120V, 0.93 for 240V).
    psu_efficiency: f64,
    /// Optional wall-meter correction shared with the autotuner/API.
    power_calibration: Arc<std::sync::RwLock<PowerCalibration>>,
    /// Circuit capacity in watts for real-time power cap enforcement.
    /// When Some, the dispatcher throttles frequency to keep wall power under this limit.
    /// None = no enforcement (default). Set via [autotuner] circuit_capacity_watts in config.
    circuit_capacity_watts: Option<u32>,
    /// Shared runtime curtailment state. When true, dispatch and nonce polling pause
    /// and the dispatcher publishes a low-power snapshot instead of mining telemetry.
    curtailment_sleeping: Arc<AtomicBool>,
    /// Diagnostic: skip board temperature reads via BM1387 I2C passthrough.
    skip_board_temp: bool,
    ///  W1 — optional shared ring buffer that captures the last
    /// N local share-validation rejects (per-chain, per-chip, with
    /// computed hash + target prefix + generation age) so operators can
    /// inspect the drop distribution via
    /// `GET /api/diagnostics/shares/local_rejects`. None by default —
    /// zero overhead unless the daemon installs a ring via
    /// `set_local_reject_ring()`.
    local_reject_ring:
        Option<Arc<std::sync::Mutex<dcentrald_api_types::share_validation::LocalRejectRing>>>,
    ///  W1 — divisor that tightens the work-table stale-age
    /// eviction threshold from `work_id_space` to
    /// `work_id_space / stale_age_divisor`. Default 4 (= 64 cycles for
    /// BM1387's 8-bit ring). Higher = tighter (more nonces rejected as
    /// stale up-front, fewer wasted hash recomputes against aliased
    /// midstates). Set to 1 to revert to legacy behavior.
    stale_age_divisor: u32,
    /// One-shot, generation-bound authority to publish measured ASIC identity
    /// after dispatcher/all-chain consensus is proven.
    asic_identity_publication: Option<AsicIdentityPublicationPort>,
}

impl WorkDispatcher {
    fn normalize_dispatch_write_identity<I>(
        dispatcher_chip_id: u16,
        mining_chain_chip_ids: I,
    ) -> std::result::Result<DispatchWriteChip, DispatchWriteIdentityError>
    where
        I: IntoIterator<Item = u16>,
    {
        let chip = DispatchWriteChip::try_from(dispatcher_chip_id)?;
        for chain_chip_id in mining_chain_chip_ids {
            let chain_chip = DispatchWriteChip::try_from(chain_chip_id)?;
            if chain_chip != chip {
                return Err(DispatchWriteIdentityError::Mixed {
                    dispatcher_chip_id,
                    chain_chip_id,
                });
            }
        }
        Ok(chip)
    }

    fn normalize_chain_write_identity(
        dispatcher_chip_id: u16,
        chain_chip_id: u16,
    ) -> std::result::Result<DispatchWriteChip, DispatchWriteIdentityError> {
        Self::normalize_dispatch_write_identity(dispatcher_chip_id, [chain_chip_id])
    }

    /// Create a new work dispatcher with chains and chip info.
    ///
    /// `hw_difficulty` must match the TicketMask written during init.
    /// BM1387 default is 256 (TicketMask = 0xFF).
    pub fn new(
        job_rx: mpsc::Receiver<JobTemplate>,
        share_tx: mpsc::Sender<ValidShare>,
        state_tx: watch::Sender<dcentrald_api::MinerState>,
        mining_sync_tx: broadcast::Sender<String>,
        shutdown: CancellationToken,
        worker_name: String,
        chains: Vec<Chain>,
        chip_id: u16,
        driver_execution_policy: ChipDriverExecutionPolicy,
        hw_difficulty: u64,
        autotune_stats_tx: Option<mpsc::Sender<ChipStatsSnapshot>>,
        freq_cmd_rx: Option<mpsc::Receiver<FreqCommand>>,
        autotune_window_s: u64,
        voltage_cmd_tx: Option<VoltageCommandSender>,
        xadc_temp: Arc<AtomicU32>,
        i2c_active: Arc<AtomicBool>,
        board_temps: Vec<Arc<AtomicU32>>,
        board_temp_seen_at: Vec<Arc<AtomicU32>>,
        board_temp_time_base: Arc<Instant>,
        led_tx: Option<mpsc::Sender<LedCommand>>,
        power_tx: watch::Sender<LivePowerEstimate>,
        psu_efficiency: f64,
        power_calibration: Arc<std::sync::RwLock<PowerCalibration>>,
        curtailment_sleeping: Arc<AtomicBool>,
        skip_board_temp: bool,
    ) -> Self {
        // Initialize per-chip frequency tracking from chain config values.
        // Each chain's chips start at the configured frequency_mhz.
        let chip_frequencies: Vec<Vec<u16>> = chains
            .iter()
            .map(|c| vec![c.frequency_mhz; c.chip_count as usize])
            .collect();
        let desired_chip_frequencies = chip_frequencies.clone();
        let frequency_limits = vec![ChainFrequencyLimits::default(); chains.len()];
        let chip_frequency_limits = chains
            .iter()
            .map(|c| vec![ChainFrequencyLimits::default(); c.chip_count as usize])
            .collect();

        let worker_name: Arc<str> = worker_name.into();
        Self {
            job_rx,
            share_tx,
            state_tx,
            mining_sync_tx,
            shutdown,
            worker_name,
            chains,
            chip_id,
            driver_execution_policy,
            hw_difficulty,
            autotune_stats_tx,
            freq_cmd_rx,
            autotune_window_s,
            voltage_cmd_tx,
            xadc_temp: Some(xadc_temp),
            board_temps,
            board_temp_seen_at,
            board_temp_time_base,
            i2c_active,
            led_tx,
            chip_frequencies,
            desired_chip_frequencies,
            frequency_limits,
            chip_frequency_limits,
            power_tx,
            psu_efficiency,
            power_calibration,
            circuit_capacity_watts: None,
            curtailment_sleeping,
            skip_board_temp,
            local_reject_ring: None,
            //  W1 default — legacy behavior preserved unless the
            // daemon explicitly sets a divisor via
            // `set_stale_age_divisor()`. The daemon plumbs the value
            // from `MiningConfig::stale_age_divisor` (default 4 from
            // `default_stale_age_divisor()`).
            stale_age_divisor: 1,
            asic_identity_publication: None,
        }
    }

    pub(crate) fn set_asic_identity_publication_port(
        &mut self,
        port: Option<AsicIdentityPublicationPort>,
    ) {
        self.asic_identity_publication = port;
    }

    ///  W1 — install the stale-age divisor (from `MiningConfig`).
    /// Call once at construction time. `1` means legacy behavior
    /// (threshold = work_id_space). Default applied by the daemon is `4`
    /// per the analysis in .
    pub fn set_stale_age_divisor(&mut self, divisor: u32) {
        self.stale_age_divisor = divisor.max(1);
    }

    ///  W1 — install a shared ring buffer so the dispatcher can
    /// capture local share-validation rejects for operator inspection.
    /// Call once at construction time. Without this, the diagnostic
    /// hot-path is a single `Option::is_some()` check (zero overhead).
    pub fn set_local_reject_ring(
        &mut self,
        ring: Arc<std::sync::Mutex<dcentrald_api_types::share_validation::LocalRejectRing>>,
    ) {
        self.local_reject_ring = Some(ring);
    }

    fn current_power_scale(&self) -> f64 {
        self.power_calibration
            .read()
            .map(|calibration| calibration.effective_multiplier())
            .unwrap_or(1.0)
    }

    /// Set the circuit capacity for real-time power cap enforcement.
    ///
    /// When set, the work dispatcher throttles frequency every 5 seconds to keep
    /// wall power under this limit. Call before `run()`. Typical values:
    /// - `Some(1350)` for 120V/15A with margin
    /// - `Some(1800)` for 120V/20A absolute max
    /// - `None` to disable (default)
    pub fn set_circuit_capacity(&mut self, watts: Option<u32>) {
        self.circuit_capacity_watts = watts;
    }

    fn emit_sync_event(
        &self,
        event: dcentrald_api::websocket::WsMiningSyncEventKind,
        chain_id: Option<u8>,
        count: Option<u32>,
        job_id: Option<String>,
        difficulty: Option<f64>,
        target_difficulty: Option<f64>,
        intensity: Option<f32>,
        error_code: Option<i64>,
        error_msg: Option<String>,
        extra_fields: Vec<(&'static str, serde_json::Value)>,
    ) {
        let _ = self.mining_sync_tx.send(
            dcentrald_api::websocket::build_mining_sync_message_with_fields(
                &dcentrald_api::websocket::WsMiningSyncMessage {
                    msg_type: "mining_sync".to_string(),
                    timestamp_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    event,
                    chain_id,
                    count,
                    job_id,
                    difficulty,
                    target_difficulty,
                    intensity: intensity.map(|value| value.clamp(0.0, 1.0)),
                    error_code,
                    error_msg,
                },
                extra_fields,
            ),
        );
    }

    fn enter_curtailment_sleep(
        &mut self,
        work_ledgers: &mut [ChainWorkLedger<Arc<WorkEntry>>],
        hashrate: &mut HashrateTracker,
    ) {
        work_ledgers.iter_mut().for_each(ChainWorkLedger::clear);
        for chain in &mut self.chains {
            if chain.mining {
                chain.fpga.flush_work_tx();
                chain.fpga.flush_work_rx();
            }
        }
        for temp in &self.board_temps {
            temp.store(0, Ordering::Release);
        }
        for seen_at in &self.board_temp_seen_at {
            seen_at.store(0, Ordering::Release);
        }
        *hashrate = HashrateTracker::new(hashrate.chain_hashrate.len());
    }

    fn exit_curtailment_sleep(&mut self, hashrate: &mut HashrateTracker) {
        for chain in &mut self.chains {
            if chain.mining {
                chain.fpga.flush_work_tx();
                chain.fpga.flush_work_rx();
            }
        }
        *hashrate = HashrateTracker::new(hashrate.chain_hashrate.len());
    }

    fn publish_curtailment_sleep_snapshot(&self) {
        self.state_tx.send_modify(|state| {
            state.hashrate_ghs = 0.0;
            state.hashrate_5s_ghs = 0.0;
            for chain in &mut state.chains {
                chain.hashrate_ghs = 0.0;
            }
        });

        let wall_watts = 25.0;
        let _ = self.power_tx.send(LivePowerEstimate {
            board_watts: wall_watts,
            wall_watts,
            per_chain_watts: vec![0.0; self.chains.iter().filter(|c| c.mining).count()],
            efficiency_jth: 0.0,
            // Idle/curtailed: 0.0 J/TH is the "no efficiency reading" sentinel,
            // not a settled measurement.
            efficiency_jth_low_confidence: true,
            btu_h: dcentrald_autotuner::btu_from_watts(wall_watts),
            calibrated: false,
            calibration_multiplier: None,
            source: "curtailment".to_string(),
            dispatcher_limits: Vec::new(),
            watt_cap: self.runtime_watt_cap_state(wall_watts, &[]),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });
    }

    fn normalize_tracker_chip_index(
        chip_id: u16,
        chip_count: u8,
        address_plan: Option<dcentrald_api_types::asic_command::LinearAddressPlan>,
        raw_chip_index: u8,
    ) -> Option<u8> {
        if chip_count == 0 {
            return None;
        }

        let chip_index = match chip_id {
            0x1387 | 0x1362 => raw_chip_index,
            0x1397 | 0x1398 | 0x1366 | 0x1368 | 0x1370 => {
                address_plan?.dense_index(raw_chip_index)? as u8
            }
            _ => raw_chip_index,
        };

        (chip_index < chip_count).then_some(chip_index)
    }

    fn chain_pic_type(chain: &Chain) -> PicType {
        chain.pic_type
    }

    fn supports_host_version_bits(chip_id: u16) -> bool {
        matches!(chip_id, 0x1387 | 0x1397 | 0x1398)
    }

    fn requires_negotiated_version_mask(chip_id: u16) -> bool {
        // BM1387 runtime setup enables MMEN and uses the four-midstate FPGA
        // layout. The one-midstate encoder remains as a wire-format fixture,
        // but production S9 mining must fail closed if the pool did not
        // negotiate BIP310 version rolling.
        chip_id == 0x1387
    }

    fn accepts_job_version_mask(chip_id: u16, version_mask: u32) -> bool {
        let Ok(chip) = DispatchWriteChip::try_from(chip_id) else {
            return false;
        };
        chip != DispatchWriteChip::Bm1387 || version_mask != 0
    }

    fn uses_8bit_fpga_work_id(chip_id: u16) -> bool {
        matches!(chip_id, 0x1387 | 0x1397 | 0x1398)
    }

    fn supports_fpga_version_rolling_submission(chip_id: u16) -> bool {
        matches!(chip_id, 0x1387 | 0x1397 | 0x1398)
    }

    fn effective_version_mask(chip_id: u16, version_mask: u32) -> u32 {
        if Self::supports_host_version_bits(chip_id) {
            version_mask
        } else {
            0
        }
    }

    fn work_id_space_for(chip_id: u16, max_midstate_shift: u32) -> usize {
        if Self::uses_8bit_fpga_work_id(chip_id) {
            256
        } else {
            1usize << (16 - max_midstate_shift)
        }
    }

    fn decode_fpga_work_id_for_dispatch(
        chip_id: u16,
        hw_work_id: u16,
        fpga_midstate_cnt: u8,
    ) -> Option<(u16, u8)> {
        if chip_id == 0x1398 {
            let mode = match fpga_midstate_cnt {
                2 => dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Four,
                3 => dcentrald_api_types::bm1398_protocol::Bm1398FpgaMidstateMode::Eight,
                _ => return None,
            };
            return dcentrald_api_types::bm1398_protocol::BM1398_FPGA_FIFO_SPEC
                .decode_work_id(mode, hw_work_id);
        }
        let ms_shift = fpga_midstate_cnt.min(3) as u16;
        let ms_mask = (1u16 << ms_shift) - 1;
        let decoded_work_id = hw_work_id >> ms_shift;
        let work_id = if Self::uses_8bit_fpga_work_id(chip_id) {
            decoded_work_id & 0x00FF
        } else {
            decoded_work_id
        };

        Some((work_id, (hw_work_id & ms_mask) as u8))
    }

    fn version_bits_per_midstate(
        chip_id: u16,
        base_version: u32,
        version_mask: u32,
        midstate_count: usize,
    ) -> Vec<Option<String>> {
        if Self::supports_host_version_bits(chip_id) && version_mask != 0 {
            let mut rolled = base_version;
            let mut version_bits = Vec::with_capacity(midstate_count);
            for _ in 0..midstate_count {
                version_bits.push(Some(format!("{:08x}", rolled ^ base_version)));
                rolled = dcentrald_stratum::work::increment_bitmask_pub(rolled, version_mask);
            }
            version_bits
        } else {
            vec![None; midstate_count]
        }
    }

    fn rolled_version_from_bits(base_version: u32, version_bits: Option<&str>) -> u32 {
        match version_bits {
            Some(vb) => base_version ^ u32::from_str_radix(vb, 16).unwrap_or(0),
            None => base_version,
        }
    }

    fn min_supported_freq(chip: DispatchWriteChip) -> u16 {
        MinerProfile::pll_frequencies_for_chip(chip.chip_id())
            .iter()
            .copied()
            .filter(|freq| *freq >= MIN_RUNTIME_FREQ_MHZ)
            .min()
            .unwrap_or(MIN_RUNTIME_FREQ_MHZ)
    }

    fn clamp_requested_freq(
        chip: DispatchWriteChip,
        requested_mhz: u16,
        ceiling_mhz: Option<u16>,
    ) -> u16 {
        let floor = Self::min_supported_freq(chip);
        let requested = requested_mhz.max(floor);
        ceiling_mhz.map_or(requested, |ceiling| requested.min(ceiling.max(floor)))
    }

    fn normalize_frequency_limit_for_chip(
        chip_id: Option<u16>,
        max_freq_mhz: Option<u16>,
    ) -> Option<Option<u16>> {
        match max_freq_mhz {
            None => Some(None),
            Some(freq) => chip_id.and_then(|id| {
                DispatchWriteChip::try_from(id)
                    .ok()
                    .map(|chip| Some(Self::clamp_requested_freq(chip, freq, None)))
            }),
        }
    }

    fn effective_chain_ceiling(&self, chain_idx: usize) -> Option<u16> {
        self.frequency_limits
            .get(chain_idx)
            .copied()
            .and_then(ChainFrequencyLimits::effective_ceiling)
    }

    fn collect_dispatcher_limits(&self) -> Vec<DispatcherChainLimit> {
        self.frequency_limits
            .iter()
            .enumerate()
            .filter_map(|(chain_idx, chain_limits)| {
                let mut merged_limits = *chain_limits;
                if let Some(chip_limits) = self.chip_frequency_limits.get(chain_idx) {
                    for chip_limit in chip_limits {
                        merged_limits = merged_limits.merge_min(*chip_limit);
                    }
                }

                let effective_ceiling_mhz = merged_limits.effective_ceiling();
                let active_sources = merged_limits.active_source_labels();
                if effective_ceiling_mhz.is_none() && active_sources.is_empty() {
                    return None;
                }

                let chain_id = self.chains.get(chain_idx)?.chain_id;
                Some(DispatcherChainLimit {
                    chain_id,
                    effective_ceiling_mhz,
                    dominant_source: merged_limits.dominant_source().map(str::to_string),
                    active_sources: active_sources.into_iter().map(str::to_string).collect(),
                })
            })
            .collect()
    }

    fn runtime_watt_cap_state(
        &self,
        wall_watts: f64,
        dispatcher_limits: &[DispatcherChainLimit],
    ) -> Option<RuntimeWattCapState> {
        let cap_watts = self.circuit_capacity_watts?;
        let cap_f64 = cap_watts as f64;
        Some(RuntimeWattCapState {
            cap_watts,
            headroom_watts: (cap_f64 - wall_watts).max(0.0),
            overage_watts: (wall_watts - cap_f64).max(0.0),
            utilization_pct: if cap_watts > 0 {
                (wall_watts / cap_f64 * 100.0).max(0.0)
            } else {
                0.0
            },
            throttling: dispatcher_limits.iter().any(|limit| {
                limit
                    .active_sources
                    .iter()
                    .any(|source| source == "power_cap")
            }),
        })
    }

    fn effective_chip_ceiling(&self, chain_idx: usize, chip_idx: usize) -> Option<u16> {
        let chain_ceiling = self.effective_chain_ceiling(chain_idx);
        let chip_ceiling = self
            .chip_frequency_limits
            .get(chain_idx)
            .and_then(|limits| limits.get(chip_idx))
            .copied()
            .and_then(ChainFrequencyLimits::effective_ceiling);

        match (chain_ceiling, chip_ceiling) {
            (Some(chain), Some(chip)) => Some(chain.min(chip)),
            (Some(chain), None) => Some(chain),
            (None, Some(chip)) => Some(chip),
            (None, None) => None,
        }
    }

    fn current_chain_min_freq(&self, chain_idx: usize) -> u16 {
        self.chip_frequencies
            .get(chain_idx)
            .and_then(|freqs| freqs.iter().copied().filter(|f| *f > 0).min())
            .or_else(|| self.chains.get(chain_idx).map(|chain| chain.frequency_mhz))
            .unwrap_or(self.nominal_chain_freq(chain_idx))
    }

    fn nominal_chain_freq(&self, chain_idx: usize) -> u16 {
        self.chains
            .get(chain_idx)
            .map(|chain| {
                DispatchWriteChip::try_from(chain.chip_id)
                    .map(|chip| chain.frequency_mhz.max(Self::min_supported_freq(chip)))
                    .unwrap_or(0)
            })
            .unwrap_or(200)
    }

    fn calculate_work_time_for_chip(
        chip: DispatchWriteChip,
        chip_count: u8,
        min_freq_mhz: u16,
        fpga_midstate_cnt: u8,
    ) -> u32 {
        let midstate_cnt = 1u32 << fpga_midstate_cnt;
        match chip {
            DispatchWriteChip::Bm1362 => {
                Bm1362Driver::calculate_work_time_for(chip_count, min_freq_mhz)
            }
            DispatchWriteChip::Bm1366 => {
                Bm1366Driver::calculate_work_time(min_freq_mhz, midstate_cnt)
            }
            DispatchWriteChip::Bm1368 => {
                Bm1368Driver::calculate_work_time(min_freq_mhz, chip_count)
            }
            DispatchWriteChip::Bm1370 => {
                Bm1370Driver::calculate_work_time(min_freq_mhz, chip_count)
            }
            DispatchWriteChip::Bm1387 => {
                Bm1387Driver::calculate_work_time(min_freq_mhz, midstate_cnt)
            }
            DispatchWriteChip::Bm1397 => {
                Bm1397Driver::calculate_work_time(min_freq_mhz, midstate_cnt)
            }
            DispatchWriteChip::Bm1398 => {
                Bm1398Driver::calculate_work_time(min_freq_mhz, midstate_cnt)
            }
        }
    }

    fn recalc_work_time_for_chain(
        chain: &mut Chain,
        min_freq_mhz: u16,
    ) -> std::result::Result<(), DispatchWriteIdentityError> {
        let chip = DispatchWriteChip::try_from(chain.chip_id)?;
        let work_time = Self::calculate_work_time_for_chip(
            chip,
            chain.chip_count,
            min_freq_mhz,
            chain.fpga_midstate_cnt,
        );
        chain
            .fpga
            .common
            .write_reg(dcentrald_hal::fpga_chain::REG_WORK_TIME, work_time);
        info!(
            chain_id = chain.chain_id,
            min_freq_mhz,
            chip_id = format_args!("0x{:04X}", chain.chip_id),
            work_time = format_args!("0x{:08X}", work_time),
            "AUTOTUNE: WORK_TIME updated chain={} chip=0x{:04X} min_freq={} MHz work_time=0x{:08X}",
            chain.chain_id,
            chain.chip_id,
            min_freq_mhz,
            work_time,
        );
        Ok(())
    }

    async fn reapply_chain_frequencies(
        &mut self,
        chain_idx: usize,
        drv: &dyn dcentrald_asic::drivers::ChipDriver,
        reason: &str,
    ) -> std::result::Result<(), String> {
        let Some(chain) = self.chains.get(chain_idx) else {
            return Err(format!(
                "chain index {} not found for {}",
                chain_idx, reason
            ));
        };
        if !chain.mining {
            return Ok(());
        }
        let write_chip = Self::normalize_chain_write_identity(self.chip_id, chain.chip_id)
            .map_err(|error| format!("refusing {}: {}", reason, error))?;
        let chain_id = chain.chain_id;
        let chain_chip_id = chain.chip_id;
        let chain_chip_count = chain.chip_count;
        let current_chain_freq = chain.frequency_mhz;

        let desired = self
            .desired_chip_frequencies
            .get(chain_idx)
            .cloned()
            .unwrap_or_default();
        let current = self
            .chip_frequencies
            .get(chain_idx)
            .cloned()
            .unwrap_or_default();
        if desired.is_empty() || desired.len() != current.len() {
            return Err(format!(
                "chain {} frequency vectors are unavailable for {}",
                chain_id, reason
            ));
        }

        let mut target_freqs = desired;
        for (chip_idx, freq) in target_freqs.iter_mut().enumerate() {
            *freq = Self::clamp_requested_freq(
                write_chip,
                *freq,
                self.effective_chip_ceiling(chain_idx, chip_idx),
            );
        }

        if target_freqs == current {
            return Ok(());
        }

        let all_same = target_freqs
            .first()
            .copied()
            .map(|first| target_freqs.iter().all(|&f| f == first))
            .unwrap_or(false);
        let mut applied_freqs = current.clone();
        let mut partial_failure = false;
        let mut any_applied = false;
        let mut verification_issue: Option<String> = None;

        if all_same {
            let target = target_freqs[0];
            if let Err(e) = drv.set_frequency(&mut self.chains[chain_idx].fpga, 0xFF, target) {
                warn!(
                    chain_id,
                    target,
                    reason,
                    error = %e,
                    "Failed to reapply broadcast frequency after ceiling update",
                );
                return Err(format!(
                    "chain {} broadcast frequency apply failed during {}: {}",
                    chain_id, reason, e
                ));
            }
            let (verified_freq, issue) =
                Self::verify_applied_frequency(drv, &mut self.chains[chain_idx], 0x00, target);
            applied_freqs.fill(verified_freq);
            verification_issue = issue;
            any_applied = true;
        } else {
            let addr_interval = if chain_chip_count > 0 {
                256u16 / chain_chip_count as u16
            } else {
                4
            };
            for (chip_idx, (&old_freq, &new_freq)) in
                current.iter().zip(target_freqs.iter()).enumerate()
            {
                if old_freq == new_freq {
                    continue;
                }
                let chip_addr = (chip_idx as u16 * addr_interval) as u8;
                if let Err(e) =
                    drv.set_frequency(&mut self.chains[chain_idx].fpga, chip_addr, new_freq)
                {
                    warn!(
                        chain_id,
                        chip_index = chip_idx as u8,
                        old_freq,
                        new_freq,
                        reason,
                        error = %e,
                        "Failed to reapply per-chip frequency after ceiling update",
                    );
                    partial_failure = true;
                    break;
                }
                let (verified_freq, issue) = Self::verify_applied_frequency(
                    drv,
                    &mut self.chains[chain_idx],
                    chip_addr,
                    new_freq,
                );
                applied_freqs[chip_idx] = verified_freq;
                if verification_issue.is_none() {
                    verification_issue = issue;
                }
                any_applied = true;
            }
        }

        if !any_applied {
            return Ok(());
        }

        if let Some(freqs) = self.chip_frequencies.get_mut(chain_idx) {
            *freqs = applied_freqs.clone();
        }
        let new_chain_freq = applied_freqs
            .iter()
            .copied()
            .max()
            .unwrap_or(current_chain_freq);
        let min_freq = applied_freqs
            .iter()
            .copied()
            .min()
            .unwrap_or(current_chain_freq);
        let chain = &mut self.chains[chain_idx];
        chain.frequency_mhz = new_chain_freq;
        Self::recalc_work_time_for_chain(chain, min_freq)
            .map_err(|error| format!("refusing {} work-time update: {}", reason, error))?;
        if partial_failure {
            warn!(
                chain_id,
                reason, "Partially reapplied effective chain frequencies after ceiling update",
            );
            Err(format!(
                "chain {} only partially applied frequency updates during {}",
                chain_id, reason
            ))
        } else if let Some(detail) = verification_issue {
            warn!(chain_id, reason, error = %detail, "Frequency reapply verification mismatch");
            Err(detail)
        } else {
            info!(
                chain_id,
                reason, "Reapplied effective chain frequencies after ceiling update",
            );
            Ok(())
        }
    }

    async fn update_frequency_limit(
        &mut self,
        chain_idx: usize,
        source: FrequencyLimitSource,
        max_freq_mhz: Option<u16>,
        drv: &dyn dcentrald_asic::drivers::ChipDriver,
        reason: &str,
    ) -> std::result::Result<(), String> {
        let Some(normalized) = Self::normalize_frequency_limit_for_chip(
            self.chains.get(chain_idx).map(|chain| chain.chip_id),
            max_freq_mhz,
        ) else {
            warn!(
                chain_idx,
                reason,
                "Skipping frequency-limit update for missing chain identity; refusing BM1387 fallback"
            );
            return Ok(());
        };

        let changed = self
            .frequency_limits
            .get_mut(chain_idx)
            .map(|limits| limits.set(source, normalized))
            .unwrap_or(false);
        if changed {
            self.reapply_chain_frequencies(chain_idx, drv, reason).await
        } else {
            Ok(())
        }
    }

    async fn update_chip_frequency_limit(
        &mut self,
        chain_idx: usize,
        chip_idx: usize,
        source: FrequencyLimitSource,
        max_freq_mhz: Option<u16>,
        drv: &dyn dcentrald_asic::drivers::ChipDriver,
        reason: &str,
    ) -> std::result::Result<(), String> {
        let Some(normalized) = Self::normalize_frequency_limit_for_chip(
            self.chains.get(chain_idx).map(|chain| chain.chip_id),
            max_freq_mhz,
        ) else {
            warn!(
                chain_idx,
                chip_idx,
                reason,
                "Skipping chip-frequency-limit update for missing chain identity; refusing BM1387 fallback"
            );
            return Ok(());
        };

        let changed = self
            .chip_frequency_limits
            .get_mut(chain_idx)
            .and_then(|limits| limits.get_mut(chip_idx))
            .map(|limits| limits.set(source, normalized))
            .unwrap_or(false);
        if changed {
            self.reapply_chain_frequencies(chain_idx, drv, reason).await
        } else {
            Ok(())
        }
    }

    fn prepare_i2c_quiet_window(&mut self) -> std::result::Result<(), String> {
        for chain in &mut self.chains {
            if chain.mining {
                chain.fpga.flush_work_tx();
            }
        }
        Ok(())
    }

    fn verify_applied_frequency(
        drv: &dyn dcentrald_asic::drivers::ChipDriver,
        chain: &mut Chain,
        chip_addr: u8,
        requested_mhz: u16,
    ) -> (u16, Option<String>) {
        match drv.verify_frequency(&mut chain.fpga, chip_addr, requested_mhz) {
            Ok(Some(actual_mhz)) if actual_mhz == requested_mhz => (actual_mhz, None),
            Ok(Some(actual_mhz)) => (
                actual_mhz,
                Some(format!(
                    "chain {} freq readback mismatch on chip 0x{:02X}: requested {} MHz, read back {} MHz",
                    chain.chain_id, chip_addr, requested_mhz, actual_mhz
                )),
            ),
            Ok(None) => (requested_mhz, None),
            Err(e) => (
                requested_mhz,
                Some(format!(
                    "chain {} freq readback failed on chip 0x{:02X}: {}",
                    chain.chain_id, chip_addr, e
                )),
            ),
        }
    }

    fn frequency_limit_reason(source: FrequencyLimitSource) -> &'static str {
        match source {
            FrequencyLimitSource::FanClamp => "fan clamp ceiling",
            FrequencyLimitSource::Thermal => "thermal ceiling",
            FrequencyLimitSource::AutotunerThermal => "autotuner thermal ceiling",
            FrequencyLimitSource::SensorSafety => "sensor safety ceiling",
            FrequencyLimitSource::QuietMode => "quiet-mode ceiling",
            FrequencyLimitSource::OffGrid => "off-grid ceiling",
            FrequencyLimitSource::SolarSurplus => "solar-surplus ceiling",
            FrequencyLimitSource::PowerCap => "power-cap ceiling",
            FrequencyLimitSource::AtmStep => "ATM profile-step ceiling",
        }
    }

    const VOLTAGE_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

    /// Run the work dispatch loop.
    ///
    /// This method runs until shutdown is requested. It concurrently:
    /// - Receives new jobs from the Stratum client
    /// - Dispatches work to all active chains via FPGA WORK_TX_FIFO
    /// - Polls WORK_RX_FIFO for nonce results at high frequency
    /// - Validates nonces and submits valid shares
    /// - Updates hashrate statistics
    pub async fn run(mut self) {
        info!(
            chains = self.chains.len(),
            chip_id = format_args!("0x{:04X}", self.chip_id),
            worker = %self.worker_name,
            "Work dispatcher online — the mining engine that converts pool jobs into bitcoin shares"
        );

        // Normalize the global + per-mining-chain identity before ANY policy
        // that can feed a hardware write. A single registry hit is not enough:
        // the dispatcher owns one driver, so a mixed or unknown chain would
        // otherwise inherit that driver's packet/PLL/version behavior.
        let dispatch_chip = match Self::normalize_dispatch_write_identity(
            self.chip_id,
            self.chains
                .iter()
                .filter(|chain| chain.mining)
                .map(|chain| chain.chip_id),
        ) {
            Ok(chip) => Some(chip),
            Err(error) => {
                error!(
                    chip_id = format_args!("0x{:04X}", self.chip_id),
                    error = %error,
                    "ASIC dispatch identity is not authoritative; work and frequency writes remain disabled"
                );
                None
            }
        };

        // Look up the chip driver only after the typed write boundary accepts
        // the identity. Unknown/missing/mixed identities stay monitoring-only.
        let registry = ChipRegistry::with_execution_policy(self.driver_execution_policy);
        let driver = dispatch_chip.and_then(|chip| registry.detect(chip.chip_id()));

        let mut identity_composition_session: Option<ActiveCompositionSession> = None;
        if let Some(publication) = self.asic_identity_publication.take() {
            if dispatch_chip.is_some() && driver.is_some() {
                match publication.publish(self.chip_id) {
                    Ok(session) => identity_composition_session = Some(session),
                    Err(error) => {
                        error!(
                            error = %error,
                            "Dispatcher ASIC consensus did not publish measured hardware identity"
                        );
                    }
                }
            } else {
                warn!(
                    "Dispatcher identity is missing, mixed, or unsupported; measured hardware identity remains unpublished"
                );
            }
        }

        if driver.is_none() {
            if self.chip_id != 0 {
                error!(
                    chip_id = format_args!("0x{:04X}", self.chip_id),
                    "No chip driver found for ChipID 0x{:04X} — cannot generate ASIC work for unknown chip type",
                    self.chip_id,
                );
            }
            info!(
                "No ASIC chips active — work dispatcher will accept pool jobs but won't dispatch to hardware. \
                 Pool connection stays alive for monitoring and API. Plug in hash boards + PSU to start mining."
            );
        }

        let num_chains = self.chains.iter().filter(|c| c.mining).count();

        if let Some(drv) = driver {
            info!(
                driver = drv.chip_name(),
                chains = num_chains,
                "Mining pipeline: continuous work feed (FIFO-driven), polling nonces 1000x/sec, {} chip driver across {} chain(s)",
                drv.chip_name(), num_chains,
            );
        }

        let mut work_builder = WorkBuilder::new();
        let mut current_job: Option<JobTemplate> = None;
        // BM1387/BM1397/BM1398 live nonce paths only preserve an 8-bit effective work ID.
        // If we widen the host-side work table beyond 256 entries for these chips,
        // nonce lookup starts hitting stale-but-populated low-byte slots after the
        // first wrap and local share validation silently compares against the wrong
        // work entry. Newer families keep the widened 16-bit tracking space.
        let max_midstate_shift = self
            .chains
            .iter()
            .filter(|c| c.mining)
            .map(|c| c.fpga_midstate_cnt.min(3) as u32)
            .max()
            .unwrap_or(0);
        let work_id_space = dispatch_chip
            .map(|chip| Self::work_id_space_for(chip.chip_id(), max_midstate_shift))
            // Monitoring-only mode never dispatches. Keep a one-slot inert
            // table without selecting any ASIC family's work-id policy.
            .unwrap_or(1);
        let mut work_ledgers = (0..self.chains.len())
            .map(|chain_idx| {
                ChainWorkLedger::new(work_id_space, chain_idx)
                    .expect("admitted work ID domain must be a nonzero power of two")
            })
            .collect::<Vec<ChainWorkLedger<Arc<WorkEntry>>>>();
        let mut next_dispatch_serial: u64 = 0;
        let mut hashrate = HashrateTracker::new(num_chains);

        // Nonce dedup set — local variable (dispatcher is single-threaded, no Mutex needed).
        // Key: (generation, nonce, midstate_idx). Generation-based eviction avoids
        // wholesale clear() which could allow duplicate submissions.
        let mut seen: std::collections::HashSet<(u64, u32, u8)> = std::collections::HashSet::new();

        // Round-robin index for board temp reads — only read ONE chain per 5s tick
        // to reduce I2C bus time from ~300-600ms to ~100-200ms per tick.
        let mut temp_read_idx: usize = 0;

        // THERMAL-6 (default-OFF, behavior change → operator live-A/B): reading ALL
        // mining chains every tick makes per-chain thermal data fresh every tick
        // instead of once per (N_chains × 5s) round-robin (e.g. every 15s with 3
        // chains), so the thermal loop reacts to a hot board much sooner. BUT each
        // BM1387 I2C passthrough temp read is ~100-200ms, and reading all chains
        // every 5s tick was the EXACT contention that starved PIC heartbeats and
        // caused the 75s zero-nonce stall (see DCENT_OS_Antminer/ "ASIC Init
        // gate_block" / AXI IIC stuck-state history + the 2026-04-11 round-robin
        // fix). So the proven round-robin stays the COMPILED DEFAULT; the all-chains
        // path is reachable only via `DCENT_THERMAL_READ_ALL_CHAINS_PER_TICK=1` for
        // operator A/B on a platform whose I2C bus has headroom. Even in all-chains
        // mode we re-check `i2c_active` before EACH chain read and yield to PIC
        // heartbeats, so a heartbeat can still interrupt the batch.
        let read_all_chains_per_tick = std::env::var("DCENT_THERMAL_READ_ALL_CHAINS_PER_TICK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        // Per-chip nonce tracker for auto-tuning
        let chain_info: Vec<(u8, u8)> = self
            .chains
            .iter()
            .filter(|c| c.mining)
            .map(|c| (c.chain_id, c.chip_count))
            .collect();
        // Map from overall chain index (in self.chains) to mining-only tracker index.
        // The ChipNonceTracker only has entries for mining chains (indexed 0..N),
        // but the nonce poll loop iterates ALL chains via enumerate(). Without this
        // map, non-mining chains at lower indices shift the mapping and nonces get
        // attributed to the wrong chain (or silently dropped).
        let chain_idx_to_tracker: Vec<Option<usize>> = {
            let mut mining_idx = 0usize;
            self.chains
                .iter()
                .map(|c| {
                    if c.mining {
                        let idx = mining_idx;
                        mining_idx += 1;
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut chip_tracker = if self.autotune_stats_tx.is_some() {
            let mut tracker = ChipNonceTracker::new(&chain_info, self.autotune_window_s);
            // Set ASIC hardware difficulty (TicketMask + 1) for autotuner nonce rate
            // calculations. This must be the ASIC's TicketMask difficulty (256 for
            // BM1387), NOT the pool's share difficulty (e.g., 8192).
            tracker.set_hw_difficulty(self.hw_difficulty as u32);
            Some(tracker)
        } else {
            None
        };

        // Counters for share tracking
        let mut shares_found: u64 = 0;
        let mut shares_submitted: u64 = 0;
        let mut discarded_nonces: u64 = 0;
        let mut dedup_discarded: u64 = 0;
        let mut hw_errors: u64 = 0;
        let mut stale_nonces: u64 = 0;
        let mut stale_overwrite_nonces: u64 = 0;
        let mut stale_empty_slot_nonces: u64 = 0;
        let mut local_header_rejects_bm1398: u64 = 0;
        let mut local_share_rejects_legacy: u64 = 0;
        let mut unsupported_version_drops: u64 = 0;
        let mut clean_job_flushes: u64 = 0;
        let mut current_pool_difficulty: f64 = 0.0;

        // Diagnostic counters for enhanced first-hash logging
        let mut total_work_dispatched: u64 = 0;
        let mut total_nonces_received: u64 = 0;
        let mut first_job_logged = false;
        let mut first_nonce_logged = false;
        let mut first_share_logged = false;
        let mut zero_nonce_alarm_fired = false;
        let mut last_nonce_time: Option<Instant> = None;
        let mut nonce_stall_alarm_fired = false;
        let mut bm1398_local_rejects_logged: u8 = 0;
        let mut bm1387_local_rejects_logged: u8 = 0;
        let mut unsupported_version_submit_logged = false;
        let mut unsupported_version_job_logged = false;
        let mut bm1387_missing_version_mask_logged = false;
        let dispatch_start = std::time::Instant::now();
        let (voltage_result_tx, mut voltage_result_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut voltage_reply_tasks = RuntimeTaskGuard::new(self.shutdown.child_token());
        let mut voltage_reply_task_sequence: u64 = 0;
        let mut power_cap_under_ticks = vec![0u8; self.chains.len()];

        // Work dispatch interval: 5ms poll for TX FIFO space.
        // The FPGA consumes work items every ~2.9ms (WORK_TIME at 650 MHz) or
        // ~3.8ms (at 500 MHz). The TX FIFO is deep (2048 entries = ~570 work items),
        // so 5ms polling is fast enough to keep it fed with massive safety margin.
        //
        // CRITICAL (2026-03-23): Previous 1ms interval caused ~6000 AXI write
        // transactions/sec, starving the I2C controller of AXI bandwidth and causing
        // PIC heartbeat failures (2/3 PICs failed every tick). 5ms reduces AXI
        // pressure 5x while still dispatching 200 work items/sec/chain — far more
        // than the ASIC consumes (~260/sec at 650 MHz).
        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(5));

        // Nonce poll interval: 5ms (200x/sec).
        // At full hashrate (~9800 nonces/sec at diff 256), this gives ~49
        // nonces per poll — well within WORK_RX_FIFO depth (256 entries).
        //
        // CRITICAL (2026-03-23): Previous 1ms interval generated 1000 AXI register
        // reads/sec from WORK_RX_STAT, contributing to xiic I2C IRQ handler timing
        // issues. BraiinsOS uses IRQ-driven nonce collection (zero polling AXI reads).
        // 5ms is a practical compromise — responsive enough for hashrate reporting,
        // gentle enough on AXI to let I2C heartbeats through reliably.
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(5));

        // Hashrate update interval (5 seconds)
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));

        // Low-latency mining-sync flush for Hacker Mode instrumentation.
        let mut mining_sync_timer = tokio::time::interval(Duration::from_millis(250));

        // Dedicated autotuner snapshot timer (uses configured measurement window)
        let mut autotune_timer =
            tokio::time::interval(Duration::from_secs(self.autotune_window_s.max(1)));

        // Power cap enforcement timer (5 seconds).
        // When circuit_capacity_watts is set, estimates wall power and throttles
        // the highest-power chain if over the cap. Protects 120V home circuits.
        let mut power_check_timer = tokio::time::interval(Duration::from_secs(5));
        let circuit_cap = self.circuit_capacity_watts;
        if let Some(cap) = circuit_cap {
            info!(
                cap_watts = cap,
                "Power cap enforcement ACTIVE — will throttle if wall power exceeds {}W", cap,
            );
        }
        let total_work_id_capacity = work_ledgers
            .iter()
            .map(ChainWorkLedger::capacity)
            .sum::<usize>();
        info!(
            work_id_space,
            total_work_id_capacity,
            max_midstate_shift,
            "Work ID tracking uses {} entries per chain, {} total (max MIDSTATE_CNT shift={})",
            work_id_space,
            total_work_id_capacity,
            max_midstate_shift,
        );

        let mut dispatcher_sleeping = self.curtailment_sleeping.load(Ordering::Acquire);
        let mut pending_dispatches = 0u32;
        let mut pending_nonces = 0u32;
        let mut dispatch_chain_counts = vec![0u32; self.chains.len()];
        let mut nonce_chain_counts = vec![0u32; self.chains.len()];

        // P1-3 (D-7): EMA smoother for the published J/TH efficiency denominator.
        // Loop-scoped so it persists across the 5 s telemetry ticks without
        // touching the dispatcher struct. Fed the raw 5 s hashrate each tick so
        // the published efficiency stops spiking when nonce bursts make the raw
        // hashrate momentarily dip toward zero.
        let mut efficiency_ema = EfficiencyHashrateEma::new();

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    info!("Work dispatcher stopping");
                    break;
                }

                // Receive new job from Stratum
                Some(job) = self.job_rx.recv() => {
                    let clean = job.clean_jobs;
                    if clean {
                        clean_job_flushes += 1;
                        info!(
                            job_id = %job.job_id,
                            merkle_branches = job.merkle_branches.len(),
                            "NEW BLOCK — pool sent clean job (new Bitcoin block found!). Flushing all pending work and starting fresh."
                        );
                        work_builder.reset_extranonce2();
                        work_ledgers.iter_mut().for_each(ChainWorkLedger::clear);
                        // Flush both WORK_TX and WORK_RX FIFOs on all chains.
                        // clean_jobs means the previous block is dead; leaving stale work
                        // queued in TX keeps ASICs hashing the old block for extra seconds,
                        // and leaving stale nonces in RX can misattribute submissions.
                        for chain in &mut self.chains {
                            if chain.mining {
                                chain.fpga.flush_work_tx();
                                chain.fpga.flush_work_rx();
                            }
                        }
                        // Flash both LEDs on new block from pool — visual "new block!" indicator
                        if let Some(ref led) = self.led_tx {
                            let _ = led.try_send(LedCommand::FlashBoth { duration_ms: 200 });
                        }
                    } else {
                        debug!(
                            job_id = %job.job_id,
                            merkle_branches = job.merkle_branches.len(),
                            "New job from pool (same block, new transactions added to mempool)"
                        );
                    }

                    if job.is_flush_only() {
                        current_job = None;
                        self.emit_sync_event(
                            dcentrald_api::websocket::WsMiningSyncEventKind::CleanJob,
                            None,
                            Some(1),
                            Some("flush_only".to_string()),
                            None,
                            if current_pool_difficulty > 0.0 {
                                Some(current_pool_difficulty)
                            } else {
                                None
                            },
                            Some(1.0),
                            None,
                            None,
                            Vec::new(),
                        );
                        info!("Flush-only pool-switch job cleared stale work; no ASIC work dispatched");
                        continue;
                    }

                    // Wire pool difficulty to the chip tracker so the autotuner
                    // uses actual pool difficulty instead of hardcoded 256.
                    // pool_difficulty comes from the Stratum client's mining.set_difficulty.
                    if job.pool_difficulty > 0.0 {
                        current_pool_difficulty = job.pool_difficulty;
                        if let Some(ref mut tracker) = chip_tracker {
                            tracker.set_pool_difficulty(job.pool_difficulty);
                        }
                    }

                    if !first_job_logged {
                        first_job_logged = true;
                        info!(
                            job_id = %job.job_id,
                            merkle_branches = job.merkle_branches.len(),
                            version_mask = format_args!("0x{:08X}", job.version_mask),
                            pool_difficulty = job.pool_difficulty,
                            "FIRST JOB FROM POOL — The mining pool just gave us work to do! \
                             This is a real Bitcoin block template with transactions from the mempool. \
                             Our ASICs will now start hashing SHA-256d at billions of attempts per second, \
                             looking for a nonce that produces a hash below the target difficulty. \
                             Version rolling mask: 0x{:08X}{}, pool difficulty: {}",
                            job.version_mask,
                            if job.version_mask != 0 { " (ASICBoost ACTIVE)" } else { " (no rolling)" },
                            job.pool_difficulty,
                        );
                    }

                    let bm1387_missing_version_mask = job.version_mask == 0
                        && self.chains.iter().any(|chain| {
                            chain.mining
                                && !Self::accepts_job_version_mask(chain.chip_id, job.version_mask)
                        });
                    if bm1387_missing_version_mask {
                        if !bm1387_missing_version_mask_logged {
                            bm1387_missing_version_mask_logged = true;
                            let refused_chains: Vec<u8> = self
                                .chains
                                .iter()
                                .filter(|chain| {
                                    chain.mining
                                        && !Self::accepts_job_version_mask(
                                            chain.chip_id,
                                            job.version_mask,
                                        )
                                })
                                .map(|chain| chain.chain_id)
                                .collect();
                            warn!(
                                job_id = %job.job_id,
                                refused_chains = ?refused_chains,
                                "Refusing BM1387 non-AsicBoost job: runtime is pinned to four-midstate/MMEN mode and requires a negotiated BIP310 version mask"
                            );
                        }
                        current_job = None;
                        work_ledgers.iter_mut().for_each(ChainWorkLedger::clear);
                        for chain in &mut self.chains {
                            if chain.mining {
                                chain.fpga.flush_work_tx();
                                chain.fpga.flush_work_rx();
                            }
                        }
                        self.emit_sync_event(
                            dcentrald_api::websocket::WsMiningSyncEventKind::CleanJob,
                            None,
                            Some(1),
                            Some(job.job_id.clone()),
                            None,
                            if current_pool_difficulty > 0.0 {
                                Some(current_pool_difficulty)
                            } else {
                                None
                            },
                            Some(1.0),
                            Some(-1387),
                            Some(
                                "BM1387 requires negotiated AsicBoost/version-rolling mask"
                                    .to_string(),
                            ),
                            vec![(
                                "refusal_reason",
                                serde_json::json!("bm1387_requires_version_mask"),
                            )],
                        );
                        continue;
                    }
                    bm1387_missing_version_mask_logged = false;

                    let unsupported_pool_version_rolling = job.version_mask != 0
                        && self.chains.iter().any(|chain| {
                            chain.mining
                                && !Self::supports_fpga_version_rolling_submission(chain.chip_id)
                        });
                    if unsupported_pool_version_rolling {
                        if !unsupported_version_job_logged {
                            unsupported_version_job_logged = true;
                            let unsupported_families: Vec<String> = self
                                .chains
                                .iter()
                                .filter(|chain| {
                                    chain.mining
                                        && !Self::supports_fpga_version_rolling_submission(
                                            chain.chip_id,
                                        )
                                })
                                .map(|chain| format!("0x{:04X}", chain.chip_id))
                                .collect();
                            warn!(
                                version_mask = format_args!("0x{:08X}", job.version_mask),
                                unsupported_families = ?unsupported_families,
                                "Disabling host version rolling for this job: chip family cannot safely submit rolled-version shares on the current FPGA path"
                            );
                        }
                    } else {
                        unsupported_version_job_logged = false;
                    }
                    let sync_job_id = job.job_id.clone();
                    let sync_event = if clean {
                        dcentrald_api::websocket::WsMiningSyncEventKind::CleanJob
                    } else {
                        dcentrald_api::websocket::WsMiningSyncEventKind::JobReceived
                    };
                    current_job = Some(job);
                    self.emit_sync_event(
                        sync_event,
                        None,
                        Some(1),
                        Some(sync_job_id),
                        None,
                        if current_pool_difficulty > 0.0 {
                            Some(current_pool_difficulty)
                        } else {
                            None
                        },
                        Some(if clean { 1.0 } else { 0.45 }),
                        None,
                        None,
                        Vec::new(),
                    );
                }

                // Continuous work dispatch — keep FPGA TX FIFOs fed.
                //
                // The FPGA consumes one work item every WORK_TIME (~3.8ms at 500 MHz).
                // We poll every 1ms and send new work whenever any chain's TX FIFO
                // is not full. This matches BraiinsOS's IRQ-driven pattern but uses
                // polling since we don't have UIO IRQ support yet.
                //
                // Critical insight: at 100ms dispatch (old code), the FPGA was idle
                // 96% of the time — the #1 cause of ~100x low hashrate.
                _ = dispatch_timer.tick() => {
                    let sleeping_now = self.curtailment_sleeping.load(Ordering::Acquire);
                    if sleeping_now {
                        if !dispatcher_sleeping {
                            info!("Curtailment sleep active — pausing work dispatch and flushing FPGA FIFOs");
                            self.enter_curtailment_sleep(&mut work_ledgers, &mut hashrate);
                            dispatcher_sleeping = true;
                        }
                        continue;
                    } else if dispatcher_sleeping {
                        info!("Curtailment wake complete — resuming work dispatch");
                        self.exit_curtailment_sleep(&mut hashrate);
                        dispatcher_sleeping = false;
                    }

                    let drv = match driver {
                        Some(ref d) => d,
                        None => continue, // No driver — skip dispatch (no ASICs)
                    };
                    if let Some(ref job) = current_job {
                        // AXI bus coordination: skip ENTIRE dispatch tick while heartbeat
                        // thread is doing I2C. Must check BEFORE work generation to avoid
                        // wasting extranonce2 values and creating orphaned ledger entries.
                        // The FPGA TX FIFO is 2048 entries deep (~570 work items) — skipping
                        // one 5ms tick costs zero throughput. ASICs keep hashing from FIFO.
                        if self.i2c_active.load(Ordering::Acquire) {
                            continue;
                        }

                        // Check if ANY chain has FIFO room before generating work.
                        // This avoids wasting extranonce2 values when all FIFOs are full.
                        let any_room = self.chains.iter().any(|c| c.mining && !c.fpga.work_tx_full());
                        if !any_room {
                            continue; // All FIFOs full — FPGA is well-fed, check again in 1ms
                        }

                        // FIX (2026-03-26): Generate UNIQUE work PER CHAIN.
                        // Previously generated one work and dispatched to all 3 chains,
                        // causing all chains to search the same nonce space → 66% duplicate
                        // shares rejected by pool. Now each chain gets its own extranonce2.
                        //
                        // FIX (2026-04-11): Eliminated Vec<usize> allocation — iterate directly.
                        // The `any_room` check above already ensures at least one chain needs work.

                        // Generate unique work for the FIRST chain (sets baseline for this tick)
                        let Some(first_chain_idx) = self
                            .chains
                            .iter()
                            .position(|c| c.mining && !c.fpga.work_tx_full())
                        else {
                            continue;
                        };
                        let first_chain = &self.chains[first_chain_idx];
                        let first_dispatch_at = Instant::now();
                        let pending_first_serial = next_dispatch_serial;
                        let Some(pending_first_reservation) = work_ledgers[first_chain_idx].reserve(
                            pending_first_serial,
                            first_dispatch_at,
                            RECENT_WORK_ID_SLOT_GUARD,
                        ) else {
                            debug!(
                                work_id_space,
                                guard_ms = RECENT_WORK_ID_SLOT_GUARD.as_millis(),
                                "All work_id slots are recently dispatched; stalling this dispatch tick"
                            );
                            continue;
                        };
                        next_dispatch_serial = next_dispatch_serial.wrapping_add(1);
                        let pending_first_wid = pending_first_reservation.work_id();
                        let mut pending_first_reservation = Some(pending_first_reservation);
                        let first_chain_ms_cnt = first_chain.fpga_midstate_cnt;
                        let first_chain_chip_id = first_chain.chip_id;
                        work_builder.set_version_mask(Self::effective_version_mask(first_chain_chip_id, job.version_mask));
                        let stratum_work = work_builder.next_work(job);
                        let asic_work = AsicWork {
                            work_id: pending_first_wid,
                            fpga_midstate_cnt: first_chain_ms_cnt,
                            version: stratum_work.version,
                            nbits: stratum_work.nbits,
                            ntime: stratum_work.ntime,
                            merkle_tail: stratum_work.merkle4,
                            midstates: stratum_work.midstates.clone(),
                            merkle_root: stratum_work.merkle_root,
                            prev_block_hash: stratum_work.prev_block_hash,
                        };

                        // Build header_tail: merkle4(4) + ntime(4 LE) + nbits(4 LE)
                        let mut header_tail = [0u8; 12];
                        header_tail[0..4].copy_from_slice(&stratum_work.merkle4);
                        header_tail[4..8].copy_from_slice(&stratum_work.ntime.to_le_bytes());
                        header_tail[8..12].copy_from_slice(&stratum_work.nbits.to_le_bytes());

                        // DIAGNOSTIC: Log complete header data for the first work item.
                        // This enables offline verification of the entire SHA-256d pipeline.
                        if total_work_dispatched == 0 {
                            let hex_encode = |bytes: &[u8]| -> String {
                                bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>()
                            };
                            info!(
                                job_id = %stratum_work.job_id,
                                extranonce2 = %stratum_work.extranonce2,
                                version = format_args!("0x{:08x}", stratum_work.version),
                                ntime = format_args!("0x{:08x}", stratum_work.ntime),
                                nbits = format_args!("0x{:08x}", stratum_work.nbits),
                                midstate = %hex_encode(&stratum_work.midstates[0]),
                                merkle4 = %hex_encode(&stratum_work.merkle4),
                                header_tail = %hex_encode(&header_tail),
                                "WORK_DIAG: First work item — midstate + header_tail sent to ASIC",
                            );

                            // Reconstruct the full 80-byte block header for verification.
                            let mut full_header = [0u8; 80];
                            full_header[0..4].copy_from_slice(&stratum_work.version.to_le_bytes());
                            let mut diag_prev_hash = job.prev_block_hash;
                            dcentrald_stratum::work::reverse_endianness_per_word_pub(&mut diag_prev_hash);
                            full_header[4..36].copy_from_slice(&diag_prev_hash);
                            // Reconstruct coinbase and merkle root
                            let diag_en2_bytes = decode_hex_bytes(&stratum_work.extranonce2);
                            let mut diag_coinbase = Vec::new();
                            diag_coinbase.extend_from_slice(&job.coinbase1);
                            diag_coinbase.extend_from_slice(&job.extranonce1);
                            diag_coinbase.extend_from_slice(&diag_en2_bytes);
                            diag_coinbase.extend_from_slice(&job.coinbase2);
                            let diag_cb_hash = dcentrald_stratum::work::double_sha256(&diag_coinbase);
                            let mut diag_merkle = diag_cb_hash;
                            for branch in &job.merkle_branches {
                                let mut combined = [0u8; 64];
                                combined[0..32].copy_from_slice(&diag_merkle);
                                combined[32..64].copy_from_slice(branch);
                                diag_merkle = dcentrald_stratum::work::double_sha256(&combined);
                            }
                            full_header[36..68].copy_from_slice(&diag_merkle);
                            full_header[68..72].copy_from_slice(&stratum_work.ntime.to_le_bytes());
                            full_header[72..76].copy_from_slice(&stratum_work.nbits.to_le_bytes());

                            info!(
                                coinbase_len = diag_coinbase.len(),
                                coinbase_hash = %hex_encode(&diag_cb_hash),
                                merkle_root = %hex_encode(&diag_merkle),
                                prev_hash_internal = %hex_encode(&diag_prev_hash),
                                full_header_76 = %hex_encode(&full_header[0..76]),
                                "WORK_DIAG: Full block header reconstruction (76 bytes, no nonce). \
                                 Verify: SHA256d(header[0:76]+nonce_le) should match pool's hash.",
                            );
                        }

                        // Build per-midstate version_bits for share submission.
                        // Index i corresponds to midstate[i]'s rolled version.
                        // CRITICAL FIX (2026-03-19, from DCENT_axe lesson):
                        // Pool expects `rolled_version XOR base_version` — the DIFFERENCE,
                        // not the absolute masked bits. ESP-Miner uses this format and
                        // DCENT_axe confirmed it with 31 accepted shares at public-pool.io.
                        let base_version = stratum_work.version;
                        let version_bits_per_ms = Self::version_bits_per_midstate(
                            first_chain_chip_id,
                            base_version,
                            stratum_work.version_mask,
                            stratum_work.midstates.len(),
                        );

                        // FIX (2026-04-12): Build the entry but do not commit it to
                        // the owning chain ledger yet.
                        // Commit only after send_work() succeeds. If send_work fails, the
                        // slot would point to work that never reached hardware.
                        let pending_first_entry = Arc::new(WorkEntry {
                            job_id: stratum_work.job_id.clone(),
                            extranonce2: stratum_work.extranonce2.clone(),
                            ntime: stratum_work.ntime,
                            version: stratum_work.version,
                            share_target: stratum_work.share_target,
                            version_bits_per_midstate: version_bits_per_ms,
                            pool_version_mask: stratum_work.version_mask,
                            midstates: stratum_work.midstates.clone(),
                            merkle_root: stratum_work.merkle_root,
                            prev_block_hash: stratum_work.prev_block_hash,
                            header_tail,
                        });

                        // Dispatch UNIQUE work to each active chain via FPGA WORK_TX_FIFO
                        //
                        // FIX (2026-03-26): Each chain gets its OWN extranonce2 so they search
                        // different nonce spaces. Previously all chains got the same work,
                        // wasting 2/3 of hashrate on duplicate nonce searching (66% reject rate).
                        // First chain uses work generated above. Subsequent chains get fresh work.
                        let mut first_chain_done = false;
                        for (chain_idx, chain) in self.chains.iter_mut().enumerate() {
                            if !chain.mining {
                                continue;
                            }

                            // Per-chain AXI coordination: abort remaining chains if I2C started
                            if self.i2c_active.load(Ordering::Acquire) {
                                break;
                            }

                            if chain.fpga.work_tx_full() {
                                continue;
                            }

                            // Generate unique work for chains after the first
                            if first_chain_done {
                                let dispatch_at = Instant::now();
                                let pending_serial = next_dispatch_serial;
                                let Some(pending_reservation) = work_ledgers[chain_idx].reserve(
                                    pending_serial,
                                    dispatch_at,
                                    RECENT_WORK_ID_SLOT_GUARD,
                                ) else {
                                    debug!(
                                        chain_id = chain.chain_id,
                                        work_id_space,
                                        guard_ms = RECENT_WORK_ID_SLOT_GUARD.as_millis(),
                                        "This chain's work_id slots are recently dispatched; trying sibling chains"
                                    );
                                    continue;
                                };
                                next_dispatch_serial = next_dispatch_serial.wrapping_add(1);
                                let pending_wid = pending_reservation.work_id();
                                work_builder.set_version_mask(Self::effective_version_mask(chain.chip_id, job.version_mask));
                                let sw = work_builder.next_work(job);
                                let aw = AsicWork {
                                    work_id: pending_wid,
                                    fpga_midstate_cnt: chain.fpga_midstate_cnt,
                                    version: sw.version,
                                    nbits: sw.nbits,
                                    ntime: sw.ntime,
                                    merkle_tail: sw.merkle4,
                                    midstates: sw.midstates.clone(),
                                    merkle_root: sw.merkle_root,
                                    prev_block_hash: sw.prev_block_hash,
                                };
                                // FIX (2026-04-11): Always increment after push (same fix as first chain).
                                let vb = Self::version_bits_per_midstate(
                                    chain.chip_id,
                                    sw.version,
                                    sw.version_mask,
                                    sw.midstates.len(),
                                );
                                let mut ht = [0u8; 12];
                                ht[0..4].copy_from_slice(&sw.merkle4);
                                ht[4..8].copy_from_slice(&sw.ntime.to_le_bytes());
                                ht[8..12].copy_from_slice(&sw.nbits.to_le_bytes());
                                // FIX (2026-04-12): Commit the chain-ledger entry
                                // only after send_work succeeds.
                                // succeeds. If send_work fails, the slot would point to work
                                // that never reached hardware — stale nonces could match it.
                                let pending_entry = Arc::new(WorkEntry {
                                    job_id: sw.job_id.clone(),
                                    extranonce2: sw.extranonce2.clone(),
                                    ntime: sw.ntime,
                                    version: sw.version,
                                    share_target: sw.share_target,
                                    version_bits_per_midstate: vb,
                                    pool_version_mask: sw.version_mask,
                                    midstates: sw.midstates.clone(),
                                    merkle_root: sw.merkle_root,
                                    prev_block_hash: sw.prev_block_hash,
                                    header_tail: ht,
                                });
                                match drv.send_work(&mut chain.fpga, &aw) {
                                    Ok(_) => {
                                        work_ledgers[chain_idx]
                                            .commit(pending_reservation, pending_entry)
                                            .expect("reservation must commit to its owning chain ledger");
                                        total_work_dispatched += 1;
                                        pending_dispatches = pending_dispatches.saturating_add(1);
                                        dispatch_chain_counts[chain_idx] =
                                            dispatch_chain_counts[chain_idx].saturating_add(1);
                                    }
                                    Err(e) => {
                                        warn!(chain_id = chain.chain_id, error = %e, "Work dispatch failed");
                                    }
                                }
                                continue;
                            }
                            first_chain_done = true;

                            // Write first chain's work (generated above)
                            match drv.send_work(&mut chain.fpga, &asic_work) {
                                Ok(wid) => {
                                    work_ledgers[chain_idx]
                                        .commit(
                                            pending_first_reservation
                                                .take()
                                                .expect("first reservation may be committed only once"),
                                            pending_first_entry.clone(),
                                        )
                                        .expect("first reservation must commit to its owning chain ledger");
                                    total_work_dispatched += 1;
                                    pending_dispatches = pending_dispatches.saturating_add(1);
                                    dispatch_chain_counts[chain_idx] =
                                        dispatch_chain_counts[chain_idx].saturating_add(1);
                                    if total_work_dispatched <= 3 {
                                        // Log first 3 work dispatches in detail for debugging
                                        info!(
                                            chain_id = chain.chain_id,
                                            work_id = wid,
                                            nbits = format_args!("0x{:08X}", asic_work.nbits),
                                            ntime = format_args!("0x{:08X}", asic_work.ntime),
                                            merkle_tail = format_args!("{:02X}{:02X}{:02X}{:02X}",
                                                asic_work.merkle_tail[0], asic_work.merkle_tail[1],
                                                asic_work.merkle_tail[2], asic_work.merkle_tail[3]),
                                            midstate_head = format_args!("{:02X}{:02X}{:02X}{:02X}",
                                                asic_work.midstates[0][0], asic_work.midstates[0][1],
                                                asic_work.midstates[0][2], asic_work.midstates[0][3]),
                                            total_dispatched = total_work_dispatched,
                                            "WORK #{} DISPATCHED to chain {} — block header data sent to FPGA WORK_TX_FIFO. \
                                             {} ASIC chips are now hashing this work in parallel.",
                                            total_work_dispatched, chain.chain_id, chain.chip_count,
                                        );
                                    } else if total_work_dispatched <= 100
                                        || total_work_dispatched.is_multiple_of(1000)
                                    {
                                        debug!(
                                            chain_id = chain.chain_id,
                                            work_id = wid,
                                            total = total_work_dispatched,
                                            "Work #{} dispatched",
                                            total_work_dispatched,
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        chain_id = chain.chain_id,
                                        error = %e,
                                        total_dispatched = total_work_dispatched,
                                        "Failed to dispatch work — check FPGA WORK_TX_FIFO state"
                                    );
                                }
                            }
                        }
                    }
                }

                _ = mining_sync_timer.tick() => {
                    if pending_dispatches > 0 {
                        let dominant_chain = dispatch_chain_counts
                            .iter()
                            .enumerate()
                            .max_by_key(|(_, count)| **count)
                            .and_then(|(idx, count)| {
                                if *count > 0 {
                                    Some(self.chains[idx].chain_id)
                                } else {
                                    None
                                }
                            });
                        let work_ring_occupancy = work_ledgers
                            .iter()
                            .map(ChainWorkLedger::occupancy)
                            .sum::<usize>()
                            .min(u32::MAX as usize) as u32;
                        let local_validation_drops_total =
                            local_header_rejects_bm1398.saturating_add(local_share_rejects_legacy);
                        self.emit_sync_event(
                            dcentrald_api::websocket::WsMiningSyncEventKind::DispatchBurst,
                            dominant_chain,
                            Some(pending_dispatches),
                            current_job.as_ref().map(|job| job.job_id.clone()),
                            None,
                            if current_pool_difficulty > 0.0 {
                                Some(current_pool_difficulty)
                            } else {
                                None
                            },
                            Some((pending_dispatches.min(24) as f32) / 24.0),
                            None,
                            None,
                            vec![
                                ("dispatch_queue_depth", serde_json::json!(self.job_rx.len())),
                                ("work_ring_occupancy", serde_json::json!(work_ring_occupancy)),
                                ("stale_nonce_drops_total", serde_json::json!(stale_nonces)),
                                (
                                    "unsupported_version_drops_total",
                                    serde_json::json!(unsupported_version_drops),
                                ),
                                (
                                    "local_validation_drops_total",
                                    serde_json::json!(local_validation_drops_total),
                                ),
                            ],
                        );
                        pending_dispatches = 0;
                        dispatch_chain_counts.fill(0);
                    }

                    if pending_nonces > 0 {
                        let dominant_chain = nonce_chain_counts
                            .iter()
                            .enumerate()
                            .max_by_key(|(_, count)| **count)
                            .and_then(|(idx, count)| {
                                if *count > 0 {
                                    Some(self.chains[idx].chain_id)
                                } else {
                                    None
                                }
                            });
                        let work_ring_occupancy = work_ledgers
                            .iter()
                            .map(ChainWorkLedger::occupancy)
                            .sum::<usize>()
                            .min(u32::MAX as usize) as u32;
                        let local_validation_drops_total =
                            local_header_rejects_bm1398.saturating_add(local_share_rejects_legacy);
                        self.emit_sync_event(
                            dcentrald_api::websocket::WsMiningSyncEventKind::NonceBurst,
                            dominant_chain,
                            Some(pending_nonces),
                            current_job.as_ref().map(|job| job.job_id.clone()),
                            None,
                            if current_pool_difficulty > 0.0 {
                                Some(current_pool_difficulty)
                            } else {
                                None
                            },
                            Some((pending_nonces.min(64) as f32) / 64.0),
                            None,
                            None,
                            vec![
                                ("dispatch_queue_depth", serde_json::json!(self.job_rx.len())),
                                ("work_ring_occupancy", serde_json::json!(work_ring_occupancy)),
                                ("stale_nonce_drops_total", serde_json::json!(stale_nonces)),
                                (
                                    "unsupported_version_drops_total",
                                    serde_json::json!(unsupported_version_drops),
                                ),
                                (
                                    "local_validation_drops_total",
                                    serde_json::json!(local_validation_drops_total),
                                ),
                            ],
                        );
                        pending_nonces = 0;
                        nonce_chain_counts.fill(0);
                    }
                }

                // High-frequency nonce polling — collect results from WORK_RX_FIFO
                _ = nonce_poll_timer.tick() => {
                    if self.curtailment_sleeping.load(Ordering::Acquire) {
                        continue;
                    }

                    // v0.9.7: Gate nonce polling during I2C heartbeats.
                    // With devmem I2C, nonce RX AXI reads compete with I2C register access
                    // on the shared GP0 AXI port, causing PICs to NACK (proven by v0.9.5
                    // diagnostic: ISR=0xD2 on 0x55/0x56). The WORK_RX_FIFO is 256 entries
                    // deep — 135ms pause loses zero nonces at ~3300 nonces/sec/chain.
                    if self.i2c_active.load(std::sync::atomic::Ordering::Acquire) {
                        continue; // Skip nonce polling during I2C — FIFO buffers them
                    }

                    let drv = match driver {
                        Some(ref d) => d,
                        None => continue, // No driver — skip nonce polling (no ASICs)
                    };
                    for (chain_idx, chain) in self.chains.iter_mut().enumerate() {
                        if !chain.mining {
                            continue;
                        }
                        if self.i2c_active.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }

                        // Drain all available nonces from this chain's WORK_RX_FIFO
                        let mut nonces_this_poll = 0u32;
                        while chain.fpga.work_rx_has_data() {
                            if self.i2c_active.load(std::sync::atomic::Ordering::Acquire) {
                                break;
                            }
                            let (w0, w1) = match chain.fpga.read_nonce() {
                                Some(pair) => pair,
                                None => break,
                            };

                            // Decode the nonce using chip-specific driver
                            let Some(decode_context) = FpgaNonceDecodeContext::try_new(
                                chain.fpga_midstate_cnt,
                            ) else {
                                hw_errors += 1;
                                error!(
                                    chain_id = chain.chain_id,
                                    fpga_midstate_cnt = chain.fpga_midstate_cnt,
                                    "Refusing nonce decode with an impossible FPGA midstate count"
                                );
                                break;
                            };
                            let mut nonce_result = match drv
                                .decode_nonce_with_context(&[w0, w1], decode_context)
                            {
                                Ok(nr) => nr,
                                Err(e) => {
                                    hw_errors += 1;
                                    // Report HW error to autotuner only when the chip index is
                                    // trustworthy. On BM1387 we can recover it from the FIFO word,
                                    // but the non-BM1387 decode-failure path does not expose a
                                    // reliable dense chip index yet.
                                    if let Some(ref mut tracker) = chip_tracker {
                                        if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                            if chain.chip_id == 0x1387 {
                                                let raw_chip = ((w1 >> 24) & 0x3F) as u8;
                                                if let Some(chip_idx) = Self::normalize_tracker_chip_index(chain.chip_id, chain.chip_count, chain.address_plan(), raw_chip) {
                                                    tracker.record_hw_error(*tracker_idx, chip_idx);
                                                }
                                            }
                                        }
                                    }
                                    debug!(
                                        chain_id = chain.chain_id,
                                        error = %e,
                                        w0 = format_args!("0x{:08X}", w0),
                                        w1 = format_args!("0x{:08X}", w1),
                                        "Failed to decode nonce"
                                    );
                                    continue;
                                }
                            };
                            let tracker_chip_index = Self::normalize_tracker_chip_index(
                                chain.chip_id,
                                chain.chip_count,
                                chain.address_plan(),
                                nonce_result.chip_index,
                            );

                            // Recompute work_id and midstate_idx using actual FPGA MIDSTATE_CNT
                            // and the per-chip dispatcher work-id width. BM1398 am2 echoes the
                            // same low 8-bit work-id ring as S9/S17-era FPGA paths.
                            {
                                let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
                                let Some((decoded_work_id, midstate_idx)) =
                                    Self::decode_fpga_work_id_for_dispatch(
                                        chain.chip_id,
                                        hw_work_id,
                                        chain.fpga_midstate_cnt,
                                    )
                                else {
                                    hw_errors += 1;
                                    debug!(
                                        chain_id = chain.chain_id,
                                        chip_id = format_args!("0x{:04X}", chain.chip_id),
                                        hw_work_id = format_args!("0x{:04X}", hw_work_id),
                                        fpga_midstate_cnt = chain.fpga_midstate_cnt,
                                        "Refusing nonce whose echoed work-id aliases outside the admitted carrier ring"
                                    );
                                    continue;
                                };
                                nonce_result.work_id = decoded_work_id;
                                nonce_result.midstate_idx = midstate_idx;
                            }

                            nonces_this_poll += 1;
                            total_nonces_received += 1;
                            pending_nonces = pending_nonces.saturating_add(1);
                            nonce_chain_counts[chain_idx] =
                                nonce_chain_counts[chain_idx].saturating_add(1);
                            last_nonce_time = Some(Instant::now());
                            // Use mining-only index for hashrate tracker (same as chip_tracker)
                            if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                hashrate.record_nonce(*tracker_idx, self.hw_difficulty);
                            }

                            // Per-chip nonce attribution for auto-tuning is deferred
                            // until AFTER stale/dedup filtering (see below, near shares_found).
                            // Recording here would count stale and duplicate nonces,
                            // polluting the autotuner's per-chip stats.

                            if !first_nonce_logged {
                                first_nonce_logged = true;
                                let elapsed = dispatch_start.elapsed();
                                info!(
                                    chain_id = chain.chain_id,
                                    nonce = format_args!("0x{:08X}", nonce_result.nonce),
                                    work_id = nonce_result.work_id,
                                    solution_id = nonce_result.solution_id,
                                    fifo_w0 = format_args!("0x{:08X}", w0),
                                    fifo_w1 = format_args!("0x{:08X}", w1),
                                    elapsed_ms = elapsed.as_millis(),
                                    work_dispatched = total_work_dispatched,
                                    "FIRST NONCE! An ASIC chip found a hash that meets difficulty 256! \
                                     Time from first work dispatch to first nonce: {}ms. \
                                     The SHA-256 cores on chain {} are ALIVE and HASHING. \
                                     Now checking if this nonce also meets the pool's share difficulty...",
                                    elapsed.as_millis(), chain.chain_id,
                                );
                            }

                            // Look up the work entry by work_id.
                            // With the full extended work ID preserved, a nonce should never
                            // refer to an entry older than one full ID-space cycle.
                            //
                            // MINE-3 bounds guard: on non-8-bit-work-id chips (e.g. BM1362)
                            // the decoded work_id is `hw_work_id >> ms_shift` and is NOT
                            // family-masked. `work_id_space` (= one chain ledger's capacity) is sized
                            // from the configured `max_midstate_shift`, but the decode uses
                            // the *actual* `fpga_midstate_cnt`. If the FPGA reports fewer
                            // midstates than configured, the decoded work_id can exceed
                            // the ledger capacity, so an unchecked raw slot index would panic
                            // — and with `panic=abort` that crashes the daemon on attacker- or
                            // glitch-controlled wire data. Use a checked `.get()` and treat an
                            // out-of-range id as a stale/garbage nonce (same as an empty slot).
                            let work_record = match work_ledgers
                                .get(chain_idx)
                                .map(|ledger| ledger.lookup(nonce_result.work_id))
                            {
                                Some(LedgerLookup::Found(record)) => {
                                    let age = next_dispatch_serial
                                        .saturating_sub(record.dispatch_serial);
                                    //  W1: tighten the stale-age
                                    // eviction threshold from
                                    // `work_id_space` (256 for BM1387's
                                    // 8-bit ring) to
                                    // `work_id_space / stale_age_divisor`
                                    // (= 64 with the default divisor 4).
                                    //  capture on .39 confirmed
                                    // ~95% of nonces with age in [64..256]
                                    // were aliasing against the wrong
                                    // midstate and producing hashes
                                    // uniformly above target. Tightening
                                    // here reclassifies them as stale
                                    // (cheap reject) instead of running
                                    // SHA-256d against the wrong midstate
                                    // (expensive false-negative).
                                    let stale_threshold = (work_id_space as u64)
                                        .saturating_div(self.stale_age_divisor.max(1) as u64)
                                        .max(1);
                                    if age >= stale_threshold {
                                        stale_nonces += 1;
                                        stale_overwrite_nonces += 1;
                                        if let Some(ref mut tracker) = chip_tracker {
                                            if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                                if let Some(chip_idx) = tracker_chip_index {
                                                    tracker.record_error(*tracker_idx, chip_idx);
                                                }
                                            }
                                        }
                                        continue;
                                    }
                                    record
                                }
                                Some(LedgerLookup::Empty)
                                | Some(LedgerLookup::OutOfRange { .. })
                                | None => {
                                    stale_nonces += 1;
                                    stale_empty_slot_nonces += 1;
                                    // Report stale (empty slot) to autotuner.
                                    if let Some(ref mut tracker) = chip_tracker {
                                        if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                            if let Some(chip_idx) = tracker_chip_index {
                                                tracker.record_error(*tracker_idx, chip_idx);
                                            }
                                        }
                                    }
                                    continue;
                                }
                            };
                            let work_dispatch_serial = work_record.dispatch_serial;
                            let work_entry = work_record.payload;

                            // Deduplicate nonces across chains and midstate slots.
                            //
                            // BUG FIX (2026-03-26): The dedup key MUST account for whether
                            // version rolling is active. Two cases:
                            //
                            // 1. Version rolling ACTIVE (distinct midstates per slot):
                            //    Key = (work_id, nonce, midstate_idx). Different midstate
                            //    slots produce different block headers, so the same nonce
                            //    from different slots is a legitimately different share.
                            //
                            // 2. Version rolling INACTIVE (all 4 FPGA slots = same midstate):
                            //    Key = (work_id, nonce, 0). The FPGA sends 4 copies of the
                            //    same midstate, so the same nonce from slots 0-3 is the SAME
                            //    share. Without this fix, 3 chains x 4 slots = up to 12
                            //    identical submissions per nonce ("Duplicate" rejections).
                            //
                            // We detect active rolling by checking if the work entry has
                            // version_bits that differ across midstates (i.e., at least one
                            // midstate has a non-zero version_bits XOR).
                            {
                                // Only include midstate_idx in dedup key when midstates are
                                // actually distinct (version rolling active with >1 midstate).
                                let has_distinct_midstates = work_entry.midstates.len() > 1
                                    && work_entry.version_bits_per_midstate.iter().any(|vb| {
                                        matches!(vb, Some(s) if s != "00000000")
                                    });
                                let dedup_ms_idx = if has_distinct_midstates {
                                    nonce_result.midstate_idx
                                } else {
                                    0 // Collapse all slots to same key
                                };
                                // Combine generation + work_id for a unique job-scoped key.
                                // Generation is monotonic, so even after work_id wraps, the
                                // combination is unique for the lifetime of the daemon.
                                let gen_key = work_dispatch_serial;
                                let key = (gen_key, nonce_result.nonce, dedup_ms_idx);
                                if !seen.insert(key) {
                                    dedup_discarded += 1;
                                    // Report duplicate to autotuner for error rate tracking.
                                    if let Some(ref mut tracker) = chip_tracker {
                                        if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                            if let Some(chip_idx) = tracker_chip_index {
                                                tracker.record_duplicate(*tracker_idx, chip_idx);
                                            }
                                        }
                                    }
                                    continue;
                                }
                                // FIX (2026-04-11): Generation-based eviction instead of wholesale clear().
                                // clear() could allow duplicate submissions for nonces still in the FIFO pipeline.
                                // retain() keeps recent entries (last 2048 generations) and prunes old ones.
                                if seen.len() > 4000 {
                                    let cutoff = next_dispatch_serial.saturating_sub(2048);
                                    seen.retain(|&(gen, _, _)| gen >= cutoff);
                                }
                            }

                            // Per-chip nonce attribution for auto-tuning.
                            // Recorded AFTER stale/dedup filtering so the autotuner only
                            // sees valid, non-stale, non-duplicate nonces. This is critical:
                            // with 8-bit work_id wrapping, ~89% of raw nonces are stale.
                            // Recording before filtering polluted per-chip stats with garbage.
                            if let Some(ref mut tracker) = chip_tracker {
                                if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                    if let Some(chip_idx) = tracker_chip_index {
                                        tracker.record_nonce(*tracker_idx, chip_idx);
                                    }
                                }
                            }

                            shares_found += 1;

                            // ASICBOOST: use the FPGA midstate_idx (from hw_work_id low bits)
                            // for rolled version selection. NOT solution_id (ASIC internal byte).
                            // FIX (2026-04-13, swarm #1): On BM1398, solution_id != midstate_idx.
                            // Wrong midstate → wrong version_bits → pool rejects share.
                            let ms_idx = (nonce_result.midstate_idx as usize)
                                .min(work_entry.midstates.len().saturating_sub(1));
                            // Defensive: an empty midstates vec makes ms_idx == 0 index
                            // out of bounds below (panic=abort → daemon crash). The
                            // WorkBuilder always emits >= 1 midstate and entries commit
                            // only after send_work, so this is expected-unreachable — but
                            // fail-soft (skip the nonce) rather than risk a crash.
                            if work_entry.midstates.is_empty() {
                                discarded_nonces += 1;
                                continue;
                            }
                            let chain_chip_id = chain.chip_id;
                            let share_version_bits = work_entry.version_bits_per_midstate
                                .get(ms_idx)
                                .cloned()
                                .flatten();

                            // Base version XOR version-bits delta = actual rolled header version.
                            let full_version = Self::rolled_version_from_bits(
                                work_entry.version,
                                share_version_bits.as_deref(),
                            );

                            let mut achieved_difficulty = None;

                            if chain_chip_id == 0x1398 {
                                let header =
                                    dispatcher_build_header(&work_entry, full_version, nonce_result.nonce);
                                if !dcentrald_stratum::share_pipeline::validate_full_header(
                                    &header,
                                    &work_entry.share_target,
                                ) {
                                    if chain_chip_id == 0x1398 {
                                        if bm1398_local_rejects_logged < 3 {
                                            bm1398_local_rejects_logged += 1;
                                            let hw_work_id = ((w1 >> 8) & 0xFFFF) as u16;
                                            let header_hash = full_header_hash_be(&header);
                                            info!(
                                                reject_num = bm1398_local_rejects_logged,
                                                job_id = %work_entry.job_id,
                                                work_id = nonce_result.work_id,
                                                hw_work_id = format_args!("0x{:04X}", hw_work_id),
                                                work_generation = work_dispatch_serial,
                                                dispatch_generation = next_dispatch_serial,
                                                midstate_idx = nonce_result.midstate_idx,
                                                nonce = format_args!("0x{:08x}", nonce_result.nonce),
                                                version_bits = ?share_version_bits,
                                                full_version = format_args!("0x{:08X}", full_version),
                                                midstate_prefix = %hex_prefix(&work_entry.midstates[ms_idx], 8),
                                                merkle_root_prefix = %hex_prefix(&work_entry.merkle_root, 8),
                                                header_prefix = %hex_prefix(&header, 16),
                                                hash_prefix = %hex_prefix(&header_hash, 8),
                                                target_prefix = %hex_prefix(&work_entry.share_target, 8),
                                                "BM1398 local full-header validation rejected a dispatcher candidate share; raw FIFO work-id and generation fields included for Wave 0b T2 stale/midstate diagnosis"
                                            );
                                        }
                                        local_header_rejects_bm1398 += 1;
                                    } else {
                                        local_share_rejects_legacy += 1;
                                    }
                                    discarded_nonces += 1;
                                    continue;
                                }
                                achieved_difficulty = achieved_difficulty_from_header(&header);
                            } else if matches!(chain_chip_id, 0x1387 | 0x1397) {
                                // S9/S17-era FPGA paths already preserve the selected
                                // midstate end-to-end. This is the historically proven
                                // accepted-share path for BM1387/BM1397.
                                let midstate = &work_entry.midstates[ms_idx];
                                let mut legacy_valid = dcentrald_stratum::share_pipeline::validate_share(
                                    midstate,
                                    &work_entry.header_tail,
                                    nonce_result.nonce,
                                    &work_entry.share_target,
                                );
                                if !legacy_valid {
                                    if chain_chip_id == 0x1387 && bm1387_local_rejects_logged < 5 {
                                        bm1387_local_rejects_logged += 1;
                                        let raw_header = dispatcher_build_header(
                                            &work_entry,
                                            full_version,
                                            nonce_result.nonce,
                                        );
                                        let raw_header_76_hex = raw_header[..76]
                                            .iter()
                                            .map(|b| format!("{:02x}", b))
                                            .collect::<String>();
                                        let raw_hash = full_header_hash_be(&raw_header);

                                        let swapped_nonce = nonce_result.nonce.swap_bytes();
                                        let swapped_header =
                                            dispatcher_build_header(&work_entry, full_version, swapped_nonce);
                                        let swapped_hash = full_header_hash_be(&swapped_header);

                                        let chip_bits_cleared_nonce =
                                            nonce_result.nonce & !(((1u32 << 6) - 1) << 2);
                                        let chip_bits_cleared_header = dispatcher_build_header(
                                            &work_entry,
                                            full_version,
                                            chip_bits_cleared_nonce,
                                        );
                                        let chip_bits_cleared_hash =
                                            full_header_hash_be(&chip_bits_cleared_header);

                                        let chip_core_bits_cleared_nonce =
                                            nonce_result.nonce & !((((1u32 << 6) - 1) << 2) | (0x7Fu32 << 24));
                                        let chip_core_bits_cleared_header = dispatcher_build_header(
                                            &work_entry,
                                            full_version,
                                            chip_core_bits_cleared_nonce,
                                        );
                                        let chip_core_bits_cleared_hash =
                                            full_header_hash_be(&chip_core_bits_cleared_header);

                                        info!(
                                            reject_num = bm1387_local_rejects_logged,
                                            job_id = %work_entry.job_id,
                                            work_id = nonce_result.work_id,
                                            midstate_idx = nonce_result.midstate_idx,
                                            chip_index = nonce_result.chip_index,
                                            nonce = format_args!("0x{:08x}", nonce_result.nonce),
                                            full_version = format_args!("0x{:08X}", full_version),
                                            target_prefix = %hex_prefix(&work_entry.share_target, 8),
                                            header_76 = %raw_header_76_hex,
                                            raw_hash_prefix = %hex_prefix(&raw_hash, 8),
                                            raw_meets = raw_hash.as_slice() <= work_entry.share_target.as_slice(),
                                            swapped_nonce = format_args!("0x{:08x}", swapped_nonce),
                                            swapped_hash_prefix = %hex_prefix(&swapped_hash, 8),
                                            swapped_meets = swapped_hash.as_slice() <= work_entry.share_target.as_slice(),
                                            chip_bits_cleared_nonce = format_args!("0x{:08x}", chip_bits_cleared_nonce),
                                            chip_bits_cleared_hash_prefix = %hex_prefix(&chip_bits_cleared_hash, 8),
                                            chip_bits_cleared_meets = chip_bits_cleared_hash.as_slice() <= work_entry.share_target.as_slice(),
                                            chip_core_bits_cleared_nonce = format_args!("0x{:08x}", chip_core_bits_cleared_nonce),
                                            chip_core_bits_cleared_hash_prefix = %hex_prefix(&chip_core_bits_cleared_hash, 8),
                                            chip_core_bits_cleared_meets = chip_core_bits_cleared_hash.as_slice() <= work_entry.share_target.as_slice(),
                                            "BM1387 reject diagnostics: compare likely nonce-normalization variants against the pool target"
                                        );
                                    }
                                    //  W1: capture this reject in the shared ring
                                    // for operator inspection. Compute the hash once for
                                    // the diagnostic (~20us/reject); only fires if a ring
                                    // is installed (zero overhead otherwise).
                                    if let Some(ref ring_arc) = self.local_reject_ring {
                                        let header = dispatcher_build_header(
                                            &work_entry,
                                            full_version,
                                            nonce_result.nonce,
                                        );
                                        let header_hash = full_header_hash_be(&header);
                                        let mut hash_be8 = [0u8; 8];
                                        hash_be8.copy_from_slice(&header_hash[0..8]);
                                        let mut tgt_be8 = [0u8; 8];
                                        tgt_be8.copy_from_slice(&work_entry.share_target[0..8]);
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_millis() as u64)
                                            .unwrap_or(0);
                                        let hw_work_id_raw = ((w1 >> 8) & 0xFFFF) as u16;
                                        let gen_age = next_dispatch_serial
                                            .saturating_sub(work_dispatch_serial);
                                        let diag = dcentrald_api_types::share_validation::LocalRejectDiagnostic {
                                            seq: local_share_rejects_legacy + 1,
                                            timestamp_ms: now_ms,
                                            chain_id: chain.chain_id,
                                            chip_index: nonce_result.chip_index,
                                            nonce: nonce_result.nonce,
                                            work_id: nonce_result.work_id,
                                            midstate_idx: nonce_result.midstate_idx,
                                            fpga_work_id_raw: hw_work_id_raw,
                                            generation_age: gen_age,
                                            computed_hash_be_first8: hash_be8,
                                            share_target_be_first8: tgt_be8,
                                            reason: dcentrald_api_types::share_validation::LocalRejectReason::HashAboveTarget,
                                        };
                                        if let Ok(mut ring) = ring_arc.lock() {
                                            ring.push(diag);
                                        }
                                    }
                                    discarded_nonces += 1;
                                    local_share_rejects_legacy += 1;
                                    continue;
                                }
                                let header =
                                    dispatcher_build_header(&work_entry, full_version, nonce_result.nonce);
                                achieved_difficulty = achieved_difficulty_from_header(&header);
                            }

                            // FPGA WORK_RX defensive drop for BM1362-family chips when
                            // the pool has negotiated version-rolling. The FPGA driver
                            // currently doesn't carry `version_bits_raw` back through
                            // `solution_id`, so we cannot reconstruct the rolled header
                            // version locally — and submitting a share with the wrong
                            // version field would spam the pool with malformed work.
                            //
                            // The chain-UART serial-dispatch path
                            // (`DCENT_AM2_SERIAL_WORK_DISPATCH=1` in
                            // `s19j_hybrid_mining.rs`) handles BIP320 correctly via the
                            // 11-byte serial nonce frame's `version_bits_raw` field +
                            // `dcentrald_asic::bm1362::bip320_reconstruct_rolled_version`.
                            //
                            // TODO(phase 7E — FPGA WORK_TX yield-improvement track):
                            // thread `version_bits` through the FPGA RX path so BM1362+
                            // rolled shares can be submitted via the dedicated WORK_TX
                            // hardware (~35× bandwidth lift over chain-UART direct
                            // dispatch at 115200). See
                            //  F2/F4
                            // and the post-`2b6d46f3` cross-platform Protocol fix
                            // sweep for the helper this would consume.
                            let unsupported_fpga_version_rolling =
                                matches!(chain_chip_id, 0x1362 | 0x1366 | 0x1368 | 0x1370)
                                    && work_entry.pool_version_mask != 0;
                            if unsupported_fpga_version_rolling {
                                if !unsupported_version_submit_logged {
                                    unsupported_version_submit_logged = true;
                                    warn!(
                                        chip_id = format_args!("0x{:04X}", chain_chip_id),
                                        version_mask = format_args!("0x{:08X}", work_entry.pool_version_mask),
                                        "Dropping rolled-version shares: FPGA WORK_RX path does not yet \
                                         carry BM1362-family version_bits back from solution_id. The \
                                         chain-UART serial-dispatch path (DCENT_AM2_SERIAL_WORK_DISPATCH=1) \
                                         handles BIP320 correctly; see Phase 7E in "
                                    );
                                }
                                discarded_nonces += 1;
                                unsupported_version_drops += 1;
                                continue;
                            }

                            // TODO(perf): Change ValidShare.worker_name from String to Arc<str>
                            // in dcentrald-stratum crate to avoid this .to_string() allocation
                            // on every share. Shares are rare (~1/min at pool diff 8192),
                            // so this is low priority but a clean optimization.
                            let share = ValidShare {
                                worker_name: self.worker_name.to_string(),
                                job_id: work_entry.job_id.clone(),
                                extranonce2: work_entry.extranonce2.clone(),
                                ntime: ntime_to_hex(work_entry.ntime),
                                nonce: nonce_to_hex(nonce_result.nonce),
                                version_bits: share_version_bits,
                                version: full_version,
                                achieved_difficulty,
                            };

                            // DIAGNOSTIC: Log first 3 shares with full detail
                            // for offline verification of the Stratum submission format.
                            if shares_found <= 3 {
                                let hex_encode = |bytes: &[u8]| -> String {
                                    bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>()
                                };
                                info!(
                                    share_num = shares_found,
                                    job_id = %share.job_id,
                                    extranonce2 = %share.extranonce2,
                                    ntime_hex = %share.ntime,
                                    nonce_hex = %share.nonce,
                                    nonce_raw = format_args!("0x{:08x}", nonce_result.nonce),
                                    version_bits = ?share.version_bits,
                                    header_tail = %hex_encode(&work_entry.header_tail),
                                    work_id = nonce_result.work_id,
                                    midstate_idx = nonce_result.midstate_idx,
                                    chip = nonce_result.chip_index,
                                    "SHARE_SUBMIT_DIAG[{}]: Complete share data for offline verification. \
                                     Pool will reconstruct header from (job_id, extranonce2, ntime, nonce).",
                                    shares_found,
                                );
                            }

                            if !first_share_logged {
                                first_share_logged = true;
                                let elapsed = dispatch_start.elapsed();
                                info!(
                                    job_id = %share.job_id,
                                    extranonce2 = %share.extranonce2,
                                    ntime = %share.ntime,
                                    nonce = %share.nonce,
                                    shares_found,
                                    total_nonces = total_nonces_received,
                                    elapsed_s = elapsed.as_secs(),
                                    "FIRST VALID SHARE! Nonce 0x{} passed full SHA-256d verification \
                                     AND meets the pool's share target. \
                                     Submitting to pool. {} HW nonces seen, {} discarded (below pool diff), \
                                     {} seconds since mining started.",
                                    share.nonce,
                                    total_nonces_received, discarded_nonces, elapsed.as_secs(),
                                );
                            } else {
                                info!(
                                    job_id = %share.job_id,
                                    extranonce2 = %share.extranonce2,
                                    ntime = %share.ntime,
                                    nonce = %share.nonce,
                                    version_bits = ?share.version_bits,
                                    work_id = nonce_result.work_id,
                                    solution_id = nonce_result.solution_id,
                                    shares_found,
                                    discarded = discarded_nonces,
                                    "VALID SHARE #{}: passed SHA-256d check — submitting to pool",
                                    shares_found,
                                );
                            }

                            // Submit the share via channel to Stratum client.
                            // BUG FIX (2026-04-11): Changed try_send → send().await.
                            // try_send silently dropped valid shares when channel was full
                            // under backpressure. send().await blocks briefly instead —
                            // shares are rare events so this never stalls dispatch.
                            // shares_submitted only increments on successful send.
                            match self.share_tx.send(share).await {
                                Ok(()) => {
                                    shares_submitted += 1;
                                    debug!(
                                        chain_id = chain.chain_id,
                                        nonce = format_args!("0x{:08X}", nonce_result.nonce),
                                        chip_index = nonce_result.chip_index,
                                        job_id = %work_entry.job_id,
                                        "Share #{} submitted",
                                        shares_submitted,
                                    );
                                }
                                Err(_) => {
                                    error!("Share channel closed — Stratum client gone");
                                    break;
                                }
                            }

                            // Safety limit: don't spin too long in one poll
                            if nonces_this_poll > 100 {
                                break;
                            }
                        }
                    }
                }

                // Autotuner snapshot tick — dedicated timer for measurement windows
                _ = autotune_timer.tick() => {
                    if let (Some(ref mut tracker), Some(ref tx)) = (&mut chip_tracker, &self.autotune_stats_tx) {
                        if let Some(ref xadc_temp) = self.xadc_temp {
                            let die_temp_c = f32::from_bits(xadc_temp.load(Ordering::Acquire));
                            if die_temp_c > 0.0 && die_temp_c < 125.0 {
                                let now_s = self.board_temp_time_base.elapsed().as_secs() as u32;
                                for (chain_idx, chain) in self.chains.iter().enumerate().filter(|(_, c)| c.mining) {
                                    let board_temp_stale = if chain_idx < self.board_temp_seen_at.len() {
                                        let seen_at_s = self.board_temp_seen_at[chain_idx].load(Ordering::Acquire);
                                        seen_at_s == 0
                                            || now_s.saturating_sub(seen_at_s)
                                                > BOARD_TEMP_STALE_TIMEOUT_S as u32
                                    } else {
                                        true
                                    };

                                    if board_temp_stale {
                                        if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(chain_idx) {
                                            tracker.set_board_temp(*tracker_idx, die_temp_c);
                                            debug!(
                                                chain_id = chain.chain_id,
                                                die_temp_c = format_args!("{:.1}", die_temp_c),
                                                "Autotuner snapshot using XADC die-temp fallback"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        let snapshots = tracker.try_snapshot();

                        for snapshot in snapshots {
                            if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(snapshot) {
                                warn!("Autotuner stats channel full — snapshot dropped (tuner may be behind)");
                            }
                        }
                    }
                }

                Some(voltage_result) = voltage_result_rx.recv() => {
                    match voltage_result {
                        PendingVoltageResult::Apply {
                            chain_id,
                            requested_mv,
                            pic_type,
                            pic_addr,
                            timed_out,
                            ack_tx,
                            result,
                        } => {
                            if let Some(ack_tx) = ack_tx {
                                let _ = ack_tx.send(result.clone());
                            }

                            match result {
                                Ok(applied_mv) => {
                                    if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                        self.chains[idx].voltage_mv = applied_mv;
                                    }
                                    info!(
                                        chain_id,
                                        voltage_mv = applied_mv,
                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                        ?pic_type,
                                        "Autotuner: voltage change applied to runtime controller",
                                    );
                                }
                                Err(detail) => {
                                    if timed_out {
                                        if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                            let conservative_mv = self.chains[idx].voltage_mv.max(requested_mv);
                                            self.chains[idx].voltage_mv = conservative_mv;
                                        }
                                        warn!(
                                            chain_id,
                                            requested_mv,
                                            ?pic_type,
                                            error = %detail,
                                            "Autotuner: voltage change timed out — preserving a conservative software voltage estimate until reconciled",
                                        );
                                    } else {
                                        warn!(
                                            chain_id,
                                            requested_mv,
                                            ?pic_type,
                                            error = %detail,
                                            "Autotuner: voltage change failed — keeping previous software voltage state",
                                        );
                                    }
                                }
                            }
                        }
                        PendingVoltageResult::Verify {
                            chain_id,
                            target_mv,
                            pic_type,
                            pic_addr,
                            timed_out,
                            ack_tx,
                            result,
                        } => {
                            if let Some(ack_tx) = ack_tx {
                                let _ = ack_tx.send(result.clone());
                            }

                            match result {
                                Ok(Some(actual_mv)) => {
                                    if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                        self.chains[idx].voltage_mv = actual_mv;
                                    }
                                    let delta_mv = actual_mv as i32 - target_mv as i32;
                                    info!(
                                        chain_id,
                                        target_mv,
                                        actual_mv,
                                        delta_mv,
                                        ?pic_type,
                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                        "Autotuner: voltage verification completed",
                                    );
                                }
                                Ok(None) => {
                                    warn!(
                                        chain_id,
                                        target_mv,
                                        ?pic_type,
                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                        "Autotuner: voltage verification unsupported for this controller type",
                                    );
                                }
                                Err(detail) => {
                                    if timed_out {
                                        warn!(
                                            chain_id,
                                            target_mv,
                                            ?pic_type,
                                            error = %detail,
                                            "Autotuner: voltage verification timed out",
                                        );
                                    } else {
                                        warn!(
                                            chain_id,
                                            target_mv,
                                            ?pic_type,
                                            error = %detail,
                                            "Autotuner: voltage verification failed",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // Frequency command from autotuner — apply per-chip freq changes
                cmd = async {
                    match self.freq_cmd_rx {
                        Some(ref mut rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(cmd) = cmd {
                        let completed_reply_tasks = voltage_reply_tasks.reap_finished().await;
                        if completed_reply_tasks.any_panicked() {
                            warn!("A voltage reply task panicked; command acknowledgement may have been lost");
                        }
                        // Gate FPGA-touching freq commands on i2c_active to prevent
                        // AXI bus contention with PIC heartbeat thread. Same pattern
                        // as work dispatch (line 1372) and nonce polling (line 1661).
                        let needs_fpga = matches!(&cmd,
                            FreqCommand::SetChipFreq { .. }
                            | FreqCommand::SetChainFreq { .. }
                            | FreqCommand::SetFrequencyLimit { .. }
                            | FreqCommand::SetChipFrequencyLimit { .. }
                            | FreqCommand::UpdateWorkTime { .. }
                        );
                        if needs_fpga && self.i2c_active.load(Ordering::Acquire) {
                            match cmd {
                                FreqCommand::SetChipFreq {
                                    ack_tx: Some(tx), ..
                                } => {
                                    let _ = tx.send(Err("i2c_active: FPGA bus busy, retry".into()));
                                }
                                FreqCommand::SetChainFreq { ack_tx, .. }
                                | FreqCommand::SetFrequencyLimit { ack_tx, .. } => {
                                    if let Some(tx) = ack_tx {
                                        let _ = tx.send(Err("i2c_active: FPGA bus busy, retry".into()));
                                    }
                                }
                                FreqCommand::SetChipFrequencyLimit {
                                    ack_tx: Some(tx), ..
                                } => {
                                    let _ = tx.send(Err("i2c_active: FPGA bus busy, retry".into()));
                                }
                                FreqCommand::UpdateWorkTime { .. } => {}
                                _ => {}
                            }
                            continue;
                        }
                        let drv = match driver {
                            Some(ref d) => d,
                            None => continue,
                        };
                        match cmd {
                            FreqCommand::SetChipFreq { chain_id, chip_index, freq_mhz, ack_tx } => {
                                let mut apply_result: std::result::Result<u16, String> = Err(format!(
                                    "chain {} not found for SetChipFreq",
                                    chain_id
                                ));
                                // Compute chip hardware address from chip index using per-platform stride.
                                // BM1387: 63 chips → stride 4, BM1397: 48 → stride 5, BM1362: 126 → stride 2, etc.
                                // Formula: addr_interval = 256 / chip_count, hw_addr = chip_index * addr_interval
                                if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                    let ci = chip_index as usize;
                                    let chain_chip_id = self.chains[idx].chip_id;
                                    let chain_chip_count = self.chains[idx].chip_count;
                                    let chain_write_chip = match
                                        Self::normalize_chain_write_identity(
                                            self.chip_id,
                                            chain_chip_id,
                                        ) {
                                        Ok(chip) => chip,
                                        Err(identity_error) => {
                                            apply_result = Err(format!(
                                                "refusing SetChipFreq for chain {}: {}",
                                                chain_id, identity_error
                                            ));
                                            if let Some(ack_tx) = ack_tx {
                                                let _ = ack_tx.send(apply_result);
                                            }
                                            continue;
                                        }
                                    };
                                    if ci >= chain_chip_count as usize {
                                        warn!(chain_id, chip_index, freq_mhz, "Autotuner: chip index out of range for SetChipFreq");
                                        apply_result = Err(format!(
                                            "chip {} out of range for chain {}",
                                            chip_index, chain_id
                                        ));
                                        if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(apply_result);
                                        }
                                        continue;
                                    }

                                    let desired_freq =
                                        Self::clamp_requested_freq(chain_write_chip, freq_mhz, None);
                                    if let Some(freqs) = self.desired_chip_frequencies.get_mut(idx) {
                                        if ci < freqs.len() {
                                            freqs[ci] = desired_freq;
                                        }
                                    }

                                    let applied_freq = Self::clamp_requested_freq(
                                        chain_write_chip,
                                        desired_freq,
                                        self.effective_chip_ceiling(idx, ci),
                                    );
                                    let current_freq = self
                                        .chip_frequencies
                                        .get(idx)
                                        .and_then(|freqs| freqs.get(ci))
                                        .copied()
                                        .unwrap_or(0);

                                    if applied_freq == current_freq {
                                        apply_result = Ok(applied_freq);
                                        if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(apply_result);
                                        }
                                        continue;
                                    }

                                    let addr_interval = if chain_chip_count > 0 { 256u16 / chain_chip_count as u16 } else { 4 };
                                    let chip_addr = (chip_index as u16 * addr_interval) as u8;
                                    let chain = &mut self.chains[idx];
                                    if let Err(e) = drv.set_frequency(&mut chain.fpga, chip_addr, applied_freq) {
                                        warn!(
                                            chain_id,
                                            chip_index,
                                            requested_mhz = freq_mhz,
                                            applied_mhz = applied_freq,
                                            error = %e,
                                            "Autotuner: failed to set chip frequency"
                                        );
                                        apply_result = Err(format!(
                                            "failed to set chain {} chip {} to {} MHz: {}",
                                            chain_id, chip_index, applied_freq, e
                                        ));
                                    } else {
                                        let (verified_freq, verification_issue) = Self::verify_applied_frequency(
                                            *drv,
                                            chain,
                                            chip_addr,
                                            applied_freq,
                                        );
                                        if idx < self.chip_frequencies.len() && ci < self.chip_frequencies[idx].len() {
                                            self.chip_frequencies[idx][ci] = verified_freq;
                                        }
                                        let chain_freq = self
                                            .chip_frequencies
                                            .get(idx)
                                            .and_then(|freqs| freqs.iter().copied().max())
                                            .unwrap_or(verified_freq);
                                        self.chains[idx].frequency_mhz = chain_freq;
                                        apply_result = if let Some(detail) = verification_issue {
                                            Err(detail)
                                        } else {
                                            Ok(verified_freq)
                                        };
                                    }
                                }
                                if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(apply_result);
                                }
                            }
                            FreqCommand::SetChainFreq { chain_id, freq_mhz, ack_tx } => {
                                let target_indices: Vec<usize> = if chain_id == 0xFF {
                                    self.chains
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, chain)| chain.mining)
                                        .map(|(idx, _)| idx)
                                        .collect()
                                } else {
                                    self.chains
                                        .iter()
                                        .enumerate()
                                        .find(|(_, chain)| chain.chain_id == chain_id)
                                        .map(|(idx, _)| vec![idx])
                                        .unwrap_or_default()
                                };

                                let mut apply_result = Ok(());

                                for idx in target_indices {
                                    let chain_chip_id = self.chains[idx].chip_id;
                                    let chain_write_chip = match Self::normalize_chain_write_identity(
                                        self.chip_id,
                                        chain_chip_id,
                                    ) {
                                        Ok(chip) => chip,
                                        Err(identity_error) => {
                                            apply_result = Err(format!(
                                                "refusing SetChainFreq for chain {}: {}",
                                                self.chains[idx].chain_id,
                                                identity_error
                                            ));
                                            break;
                                        }
                                    };
                                    let desired_freq =
                                        Self::clamp_requested_freq(chain_write_chip, freq_mhz, None);
                                    if let Some(freqs) = self.desired_chip_frequencies.get_mut(idx) {
                                        freqs.fill(desired_freq);
                                    }
                                    if let Err(detail) = self
                                        .reapply_chain_frequencies(idx, *drv, "chain frequency update")
                                        .await
                                    {
                                        apply_result = Err(detail);
                                        break;
                                    }
                                }

                                if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(apply_result);
                                }
                            }
                            FreqCommand::SetFrequencyLimit { chain_id, max_freq_mhz, source, ack_tx } => {
                                let target_indices: Vec<usize> = if chain_id == 0xFF {
                                    self.chains
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, chain)| chain.mining)
                                        .map(|(idx, _)| idx)
                                        .collect()
                                } else {
                                    self.chains
                                        .iter()
                                        .enumerate()
                                        .find(|(_, chain)| chain.chain_id == chain_id)
                                        .map(|(idx, _)| vec![idx])
                                        .unwrap_or_default()
                                };

                                let mut apply_result = Ok(());

                                for idx in target_indices {
                                    let reason = Self::frequency_limit_reason(source);
                                    if let Err(detail) = self
                                        .update_frequency_limit(idx, source, max_freq_mhz, *drv, reason)
                                        .await
                                    {
                                        apply_result = Err(detail);
                                        break;
                                    }
                                }

                                if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(apply_result);
                                }
                            }
                            FreqCommand::SetChipFrequencyLimit {
                                chain_id,
                                chip_index,
                                max_freq_mhz,
                                source,
                                ack_tx,
                            } => {
                                let mut apply_result: std::result::Result<(), String> = Err(format!(
                                    "chain {} not found for SetChipFrequencyLimit",
                                    chain_id
                                ));
                                if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                    let ci = chip_index as usize;
                                    let chip_count = self.chains[idx].chip_count as usize;
                                    if ci >= chip_count {
                                        warn!(chain_id, chip_index, ?source, "Autotuner: chip index out of range for SetChipFrequencyLimit");
                                        apply_result = Err(format!(
                                            "chip {} out of range for chain {} limit update",
                                            chip_index, chain_id
                                        ));
                                        if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(apply_result);
                                        }
                                        continue;
                                    }

                                    let reason = Self::frequency_limit_reason(source);
                                    apply_result = self
                                        .update_chip_frequency_limit(idx, ci, source, max_freq_mhz, *drv, reason)
                                        .await;
                                }

                                if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(apply_result);
                                }
                            }
                            FreqCommand::UpdateWorkTime { chain_id, min_freq_mhz } => {
                                if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                    let actual_min = self.current_chain_min_freq(idx);
                                    let effective_min = actual_min.min(min_freq_mhz);
                                    let chain = &mut self.chains[idx];
                                    if let Err(identity_error) = Self::normalize_chain_write_identity(
                                        self.chip_id,
                                        chain.chip_id,
                                    ) {
                                        warn!(
                                            chain_id,
                                            error = %identity_error,
                                            "Refusing WORK_TIME update for non-authoritative chip identity"
                                        );
                                        continue;
                                    }
                                    if let Err(identity_error) =
                                        Self::recalc_work_time_for_chain(chain, effective_min)
                                    {
                                        warn!(
                                            chain_id,
                                            error = %identity_error,
                                            "Refusing WORK_TIME update for unsupported chip identity"
                                        );
                                    }
                                }
                            }
                            FreqCommand::SetVoltage { chain_id, voltage_mv, ack_tx } => {
                                if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                    let chain_chip_id = self.chains[idx].chip_id;
                                    let pic_addr = self.chains[idx].pic_address;
                                    let pic_type = Self::chain_pic_type(&self.chains[idx]);
                                    if let Some(pic_addr) = pic_addr {
                                        if let Some(ref tx) = self.voltage_cmd_tx {
                                            if pic_type == PicType::NoPic {
                                                if let Some(ack_tx) = ack_tx {
                                                    let _ = ack_tx.send(Err("NoPic architecture has no runtime voltage controller".to_string()));
                                                }
                                            } else {
                                                let (reply_tx, reply_rx) = oneshot::channel();
                                                let cmd = VoltageCommand::SetVoltage {
                                                    chain_id: Some(chain_id),
                                                    chip_id: chain_chip_id,
                                                    pic_addr,
                                                    target_mv: voltage_mv,
                                                    reply_tx: Some(reply_tx),
                                                };

                                                if let Err(e) = tx.try_send(cmd) {
                                                    match &e {
                                                        VoltageTrySendError::Full(_) => warn!(chain_id, "voltage mailbox full, rejecting SetVoltage from autotuner"),
                                                        VoltageTrySendError::Disconnected => error!(chain_id, "voltage worker thread dead — daemon shutdown imminent (autotuner SetVoltage)"),
                                                        VoltageTrySendError::TerminalLatched => warn!(chain_id, "terminal safe-off latched, rejecting autotuner SetVoltage"),
                                                        VoltageTrySendError::Superseded { generation } => warn!(chain_id, generation = %generation, "endpoint disable pending, superseding autotuner SetVoltage"),
                                                    }
                                                    if let Some(ack_tx) = ack_tx {
                                                        let _ = ack_tx.send(Err(format!("failed to send voltage command to runtime thread: {}", e)));
                                                    }
                                                } else {
                                                    let voltage_result_tx = voltage_result_tx.clone();
                                                    let timeout = Self::VOLTAGE_COMMAND_TIMEOUT;
                                                    let task_shutdown = voltage_reply_tasks.cancellation_token();
                                                    voltage_reply_task_sequence = voltage_reply_task_sequence.wrapping_add(1);
                                                    let task_name = format!("voltage-apply-reply-{chain_id}-{voltage_reply_task_sequence}");
                                                    if !voltage_reply_tasks.spawn(task_name, async move {
                                                        let reply = tokio::select! {
                                                            _ = task_shutdown.cancelled() => return,
                                                            reply = tokio::time::timeout(timeout, reply_rx) => reply,
                                                        };
                                                        let (timed_out, result) = match reply {
                                                            Ok(Ok(Ok(VoltageCommandReply::Applied(applied_mv)))) => (false, Ok(applied_mv)),
                                                            Ok(Ok(Ok(other))) => (false, Err(format!("unexpected reply to SetVoltage command: {:?}", other))),
                                                            Ok(Ok(Err(detail))) => (false, Err(detail)),
                                                            Ok(Err(_)) => (false, Err("runtime voltage reply channel dropped before acknowledgement".to_string())),
                                                            Err(_) => (true, Err(format!(
                                                                "runtime voltage apply timed out after {}s",
                                                                timeout.as_secs(),
                                                            ))),
                                                        };
                                                        let _ = voltage_result_tx.send(PendingVoltageResult::Apply {
                                                            chain_id,
                                                            requested_mv: voltage_mv,
                                                            pic_type,
                                                            pic_addr,
                                                            timed_out,
                                                            ack_tx,
                                                            result,
                                                        });
                                                    }) {
                                                        error!(chain_id, "Could not register owned voltage-apply reply task");
                                                    }
                                                }
                                            }
                                        } else if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(Err("runtime voltage thread is unavailable".to_string()));
                                        }
                                    } else if let Some(ack_tx) = ack_tx {
                                        let _ = ack_tx.send(Err("chain has no voltage controller address".to_string()));
                                    }
                                } else if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(Err(format!(
                                        "chain {} not found for SetVoltage",
                                        chain_id
                                    )));
                                }
                            }
                            FreqCommand::VerifyVoltage { chain_id, target_mv, ack_tx } => {
                                if let Some(idx) = self.chains.iter().position(|c| c.chain_id == chain_id) {
                                    let chain_chip_id = self.chains[idx].chip_id;
                                    let pic_addr = self.chains[idx].pic_address;
                                    let pic_type = Self::chain_pic_type(&self.chains[idx]);
                                    if let Some(pic_addr) = pic_addr {
                                        if let Some(ref tx) = self.voltage_cmd_tx {
                                            let (reply_tx, reply_rx) = oneshot::channel();
                                            let cmd = VoltageCommand::VerifyVoltage {
                                                chain_id: Some(chain_id),
                                                chip_id: chain_chip_id,
                                                pic_addr,
                                                target_mv,
                                                reply_tx: Some(reply_tx),
                                            };

                                            if let Err(e) = tx.try_send(cmd) {
                                                match &e {
                                                    VoltageTrySendError::Full(_) => warn!(chain_id, "voltage mailbox full, rejecting VerifyVoltage from autotuner"),
                                                    VoltageTrySendError::Disconnected => error!(chain_id, "voltage worker thread dead — daemon shutdown imminent (autotuner VerifyVoltage)"),
                                                    VoltageTrySendError::TerminalLatched => warn!(chain_id, "terminal safe-off latched, rejecting autotuner VerifyVoltage"),
                                                    VoltageTrySendError::Superseded { generation } => warn!(chain_id, generation = %generation, "endpoint disable pending, superseding autotuner VerifyVoltage"),
                                                }
                                                if let Some(ack_tx) = ack_tx {
                                                    let _ = ack_tx.send(Err(format!("failed to send voltage verification command to runtime thread: {}", e)));
                                                }
                                            } else {
                                                let voltage_result_tx = voltage_result_tx.clone();
                                                let timeout = Self::VOLTAGE_COMMAND_TIMEOUT;
                                                let task_shutdown = voltage_reply_tasks.cancellation_token();
                                                voltage_reply_task_sequence = voltage_reply_task_sequence.wrapping_add(1);
                                                let task_name = format!("voltage-verify-reply-{chain_id}-{voltage_reply_task_sequence}");
                                                if !voltage_reply_tasks.spawn(task_name, async move {
                                                    let reply = tokio::select! {
                                                        _ = task_shutdown.cancelled() => return,
                                                        reply = tokio::time::timeout(timeout, reply_rx) => reply,
                                                    };
                                                    let (timed_out, result) = match reply {
                                                        Ok(Ok(Ok(VoltageCommandReply::Verified(actual_mv)))) => (false, Ok(actual_mv)),
                                                        Ok(Ok(Ok(other))) => (false, Err(format!("unexpected reply to VerifyVoltage command: {:?}", other))),
                                                        Ok(Ok(Err(detail))) => (false, Err(detail)),
                                                        Ok(Err(_)) => (false, Err("runtime voltage verification reply channel dropped".to_string())),
                                                        Err(_) => (true, Err(format!(
                                                            "runtime voltage verification timed out after {}s",
                                                            timeout.as_secs(),
                                                        ))),
                                                    };
                                                    let _ = voltage_result_tx.send(PendingVoltageResult::Verify {
                                                        chain_id,
                                                        target_mv,
                                                        pic_type,
                                                        pic_addr,
                                                        timed_out,
                                                        ack_tx,
                                                        result,
                                                    });
                                                }) {
                                                    error!(chain_id, "Could not register owned voltage-verify reply task");
                                                }
                                            }
                                        } else if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(Err("runtime voltage thread is unavailable".to_string()));
                                        }
                                    } else if let Some(ack_tx) = ack_tx {
                                        let _ = ack_tx.send(Err("chain has no voltage controller address".to_string()));
                                    }
                                } else if let Some(ack_tx) = ack_tx {
                                    let _ = ack_tx.send(Err(format!(
                                        "chain {} not found for VerifyVoltage",
                                        chain_id
                                    )));
                                }
                            }
                            FreqCommand::Barrier { ack_tx } => {
                                // Synchronization barrier: confirms all prior commands in the
                                // mpsc channel have been processed by the dispatcher. The
                                // autotuner awaits this before starting measurement windows.
                                let _ = ack_tx.send(());
                            }
                            FreqCommand::BeginMeasurement { chain_id, ack_tx } => {
                                let epoch = chip_tracker
                                    .as_mut()
                                    .and_then(|tracker| tracker.begin_measurement(chain_id));
                                if epoch.is_none() {
                                    warn!(
                                        chain_id,
                                        "Autotuner: failed to start fresh measurement window for chain {}",
                                        chain_id,
                                    );
                                }
                                let _ = ack_tx.send(epoch);
                            }
                            FreqCommand::PrepareI2cQuietWindow { ack_tx } => {
                                let _ = ack_tx.send(self.prepare_i2c_quiet_window());
                            }
                        }
                    }
                }

                // Power cap enforcement — throttle frequency if wall power exceeds circuit limit.
                //
                // Every 5 seconds, compute live wall power from per-chip frequencies and
                // voltages. If over the configured circuit capacity, reduce frequency on
                // the chain contributing the most power. Maximum 50 MHz reduction per cycle
                // to avoid thermal shock and oscillation. Frequency is NEVER restored by
                // this arm — only the autotuner raises frequencies.
                //
                // This protects 120V home miners from tripping breakers. A user running
                // 3 hash boards at high frequency on a 15A circuit (1,800W max, ~1,350W
                // safe with other loads) needs firmware-level enforcement.
                _ = power_check_timer.tick() => {
                    if self.curtailment_sleeping.load(Ordering::Acquire) {
                        continue;
                    }

                    if let Some(cap) = circuit_cap {
                        const POWER_CAP_CLEAR_MARGIN: f64 = 0.90;
                        const POWER_CAP_CLEAR_TICKS: u8 = 3;
                        // Compute live wall power using the same model as hashrate tick.
                        let power_model = PowerModel::new_for_chip(self.chip_id)
                            .with_power_scale(self.current_power_scale());
                        let live_chains: Vec<(f64, Vec<u16>)> = self.chains
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.mining)
                            .map(|(i, c)| {
                                let voltage_v = if c.voltage_mv > 0 {
                                    c.voltage_mv as f64 / 1000.0
                                } else {
                                    9.1 // Safe fallback
                                };
                                let freqs = if i < self.chip_frequencies.len() {
                                    self.chip_frequencies[i].clone()
                                } else {
                                    vec![c.frequency_mhz; c.chip_count as usize]
                                };
                                (voltage_v, freqs)
                            })
                            .collect();

                        let (live_chain_temps, fan_pwm, fan_rpm, pool_accepted, pool_rejected) = {
                            let state_snapshot = self.state_tx.borrow();
                            let live_chain_temps: Vec<f32> = self
                                .chains
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| c.mining)
                                .map(|(i, _)| {
                                    state_snapshot.chains.get(i).map(|c| c.temp_c).unwrap_or(0.0)
                                })
                                .collect();
                            (
                                live_chain_temps,
                                state_snapshot.fans.pwm,
                                state_snapshot.fans.rpm,
                                state_snapshot.accepted,
                                state_snapshot.rejected,
                            )
                        };

                        // Use the same telemetry-aware runtime estimate we publish elsewhere so
                        // the circuit cap tracks calibrated wall power, fan load, and thermal leakage.
                        let hr_ths = hashrate.hashrate_5s / 1000.0;
                        let live_power = power_model.estimate_live_with_telemetry(
                            &live_chains,
                            &live_chain_temps,
                            fan_pwm,
                            fan_rpm,
                            hr_ths,
                            self.psu_efficiency,
                        );
                        let wall_watts = live_power.wall_watts;

                        if wall_watts > cap as f64 {
                            power_cap_under_ticks.fill(0);
                            warn!(
                                estimated_w = format_args!("{:.0}", wall_watts),
                                cap_w = cap,
                                over_pct = format_args!("{:.1}", (wall_watts / cap as f64 - 1.0) * 100.0),
                                "POWER CAP: estimated wall power {:.0}W exceeds circuit limit {}W — throttling",
                                wall_watts, cap,
                            );

                            // Find the chain with the highest per-chain power and reduce
                            // its frequency. The per-chain power is available from live_power.
                            if let Some(drv) = driver {
                                // Identify the highest-power mining chain.
                                let mut worst_chain_idx: Option<usize> = None;
                                let mut worst_power: f64 = 0.0;
                                let mut pc_idx = 0usize;
                                for (i, chain) in self.chains.iter().enumerate() {
                                    if !chain.mining { continue; }
                                    let chain_power = live_power.per_chain_watts
                                        .get(pc_idx)
                                        .copied()
                                        .unwrap_or(0.0);
                                    if chain_power > worst_power {
                                        worst_power = chain_power;
                                        worst_chain_idx = Some(i);
                                    }
                                    pc_idx += 1;
                                }

                                if let Some(idx) = worst_chain_idx {
                                    let (chain_id, chain_chip_id, fallback_freq_mhz) = {
                                        let chain = &self.chains[idx];
                                        (chain.chain_id, chain.chip_id, chain.frequency_mhz)
                                    };

                                    // Compute target frequency reduction.
                                    // Scale: target_freq = current_avg * (cap / wall_watts)
                                    // Clamp reduction to 25-50 MHz per cycle for stability.
                                    let current_avg: f64 = if idx < self.chip_frequencies.len()
                                        && !self.chip_frequencies[idx].is_empty()
                                    {
                                        let sum: u64 = self.chip_frequencies[idx].iter().map(|f| *f as u64).sum();
                                        sum as f64 / self.chip_frequencies[idx].len() as f64
                                    } else {
                                        fallback_freq_mhz as f64
                                    };

                                    let scale = cap as f64 / wall_watts;
                                    let ideal_freq = (current_avg * scale) as u16;
                                    let reduction = current_avg as u16 - ideal_freq;
                                    // Clamp: at least 25 MHz reduction (meaningful step),
                                    // at most 50 MHz per cycle (avoid thermal shock).
                                    let clamped_reduction = reduction.clamp(25, 50);
                                    let new_freq = (current_avg as u16).saturating_sub(clamped_reduction);

                                    // Enforce minimum frequency floor (don't kill mining entirely)
                                    let min_freq = match Self::normalize_chain_write_identity(
                                        self.chip_id,
                                        chain_chip_id,
                                    ) {
                                        Ok(chip) => Self::min_supported_freq(chip),
                                        Err(identity_error) => {
                                            error!(
                                                chain_id,
                                                error = %identity_error,
                                                "Refusing power-cap frequency write for non-authoritative chip identity"
                                            );
                                            continue;
                                        }
                                    };
                                    let new_freq = new_freq.max(min_freq);

                                    if new_freq < current_avg as u16 {
                                        info!(
                                            chain_id,
                                            from_mhz = current_avg as u16,
                                            to_mhz = new_freq,
                                            reduction_mhz = clamped_reduction,
                                            chain_power_w = format_args!("{:.0}", worst_power),
                                            wall_w = format_args!("{:.0}", wall_watts),
                                            cap_w = cap,
                                            "POWER THROTTLE: reducing chain {} frequency {}→{} MHz ({} MHz step) to bring {:.0}W under {}W cap",
                                            chain_id, current_avg as u16, new_freq, clamped_reduction,
                                            wall_watts, cap,
                                        );

                                        if let Err(detail) = self
                                            .update_frequency_limit(
                                                idx,
                                                FrequencyLimitSource::PowerCap,
                                                Some(new_freq),
                                                drv,
                                                "power-cap ceiling",
                                            )
                                            .await
                                        {
                                            warn!(chain_id, error = %detail, "Power cap: failed to apply dispatcher ceiling");
                                        }
                                    }
                                }
                            }
                        } else if wall_watts <= cap as f64 * POWER_CAP_CLEAR_MARGIN {
                            if let Some(drv) = driver {
                                for idx in self
                                    .chains
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, chain)| chain.mining)
                                    .map(|(idx, _)| idx)
                                    .collect::<Vec<_>>()
                                {
                                    let has_power_cap = self
                                        .frequency_limits
                                        .get(idx)
                                        .and_then(|limits| limits.power_cap)
                                        .is_some();
                                    if !has_power_cap {
                                        power_cap_under_ticks[idx] = 0;
                                        continue;
                                    }

                                    power_cap_under_ticks[idx] =
                                        power_cap_under_ticks[idx].saturating_add(1);
                                    if power_cap_under_ticks[idx] >= POWER_CAP_CLEAR_TICKS {
                                        if let Err(detail) = self
                                            .update_frequency_limit(
                                                idx,
                                                FrequencyLimitSource::PowerCap,
                                                None,
                                                drv,
                                                "power-cap recovery",
                                            )
                                            .await
                                        {
                                            warn!(chain_id = self.chains[idx].chain_id, error = %detail, "Power cap: failed to clear dispatcher ceiling");
                                        }
                                        power_cap_under_ticks[idx] = 0;
                                    }
                                }
                            }
                        } else {
                            power_cap_under_ticks.fill(0);
                        }
                    }
                }

                // Hashrate update tick — compute rates and update MinerState
                _ = hashrate_timer.tick() => {
                    if self.curtailment_sleeping.load(Ordering::Acquire) {
                        if !dispatcher_sleeping {
                            info!("Curtailment sleep active — publishing low-power dispatcher snapshot");
                            self.enter_curtailment_sleep(&mut work_ledgers, &mut hashrate);
                            dispatcher_sleeping = true;
                        }
                        self.publish_curtailment_sleep_snapshot();
                        continue;
                    } else if dispatcher_sleeping {
                        info!("Curtailment wake detected — resetting hashrate tracker for clean resume");
                        self.exit_curtailment_sleep(&mut hashrate);
                        dispatcher_sleeping = false;
                    }

                    hashrate.update(self.hw_difficulty);

                    // Nonce stall detector: nonces were flowing, then stopped
                    if let Some(last_time) = last_nonce_time {
                        let stall_secs = last_time.elapsed().as_secs();
                        if stall_secs >= 30 && !nonce_stall_alarm_fired {
                            nonce_stall_alarm_fired = true;
                            error!(
                                total_nonces = total_nonces_received,
                                stall_secs,
                                "NONCE STALL: {} nonces received previously, none for {}s. \
                                 Dumping per-chain FPGA state.",
                                total_nonces_received, stall_secs,
                            );
                            for chain in self.chains.iter() {
                                if chain.mining {
                                    let ctrl = chain.fpga.common.read_reg(
                                        dcentrald_hal::fpga_chain::REG_CTRL,
                                    );
                                    let wt = chain.fpga.common.read_reg(
                                        dcentrald_hal::fpga_chain::REG_WORK_TIME,
                                    );
                                    let baud = chain.fpga.common.read_reg(
                                        dcentrald_hal::fpga_chain::REG_BAUD,
                                    );
                                    let errs = chain.fpga.read_error_count();
                                    let rx_data = chain.fpga.work_rx_has_data();
                                    error!(
                                        chain_id = chain.chain_id,
                                        "  DIAG ch{}: CTRL=0x{:08X} WORK_TIME=0x{:08X} \
                                         BAUD=0x{:08X} ERR_CNT={} RX_DATA={}",
                                        chain.chain_id, ctrl, wt, baud, errs, rx_data,
                                    );
                                }
                            }
                        }
                        if stall_secs < 5 && nonce_stall_alarm_fired {
                            nonce_stall_alarm_fired = false;
                            info!(
                                "Nonce stall detector cleared; local nonce flow observed, share proof pending"
                            );
                        }
                    }

                    // Read per-chain board temperatures via BM1387 I2C passthrough.
                    //
                    // This is the ONLY safe place to do FPGA CMD FIFO operations:
                    // the hashrate tick is every 5 seconds, and we check i2c_active
                    // to avoid colliding with PIC heartbeats.
                    //
                    // Each read takes ~100-200ms (I2C probe + read + restore).
                    // With 3 chains, total is ~300-600ms every 5 seconds.
                    // FIX (2026-04-11): Round-robin board temp reads — only read ONE chain per tick.
                    // Each I2C temp read takes ~100-200ms. Reading all 3 chains every 5s tick
                    // consumed 300-600ms of I2C bus time, starving PIC heartbeats. Now we read
                    // one chain per tick (round-robin), so each chain gets read every 15s instead
                    // of every 5s — acceptable for thermal control, much less I2C contention.
                    if let Some(driver) = driver {
                        if !self.skip_board_temp && !self.i2c_active.load(Ordering::Acquire) {
                            let mining_chains: Vec<usize> = self.chains.iter().enumerate()
                                .filter(|(_, c)| c.mining).map(|(i, _)| i).collect();
                            if !mining_chains.is_empty() {
                                let now_s = self.board_temp_time_base.elapsed().as_secs() as u32;
                                // THERMAL-6: in the default round-robin mode read exactly
                                // ONE chain this tick (proven low-contention path); in the
                                // env-gated all-chains mode read every mining chain so each
                                // board's temp is fresh every tick. `temp_read_idx` still
                                // advances in round-robin mode for fair coverage.
                                let targets: Vec<usize> = if read_all_chains_per_tick {
                                    mining_chains.clone()
                                } else {
                                    let target = mining_chains[temp_read_idx % mining_chains.len()];
                                    temp_read_idx = temp_read_idx.wrapping_add(1);
                                    vec![target]
                                };
                                for target in targets {
                                    // Re-check before EACH chain read so a PIC heartbeat that
                                    // grabbed the bus mid-batch interrupts the all-chains loop
                                    // (heartbeat health > thermal freshness).
                                    if self.i2c_active.load(Ordering::Acquire) {
                                        break;
                                    }
                                    let chain = &mut self.chains[target];
                                    if let Some(temp) = driver.read_board_temp(&mut chain.fpga) {
                                        if target < self.board_temps.len() {
                                            self.board_temps[target].store(
                                                temp.to_bits(),
                                                Ordering::Release,
                                            );
                                        }
                                        if target < self.board_temp_seen_at.len() {
                                            self.board_temp_seen_at[target].store(
                                                now_s.max(1),
                                                Ordering::Release,
                                            );
                                        }
                                        // Also update the chain's temp in MinerState
                                        self.state_tx.send_modify(|state| {
                                            if target < state.chains.len() {
                                                state.chains[target].temp_c = temp;
                                            }
                                        });
                                        // Wire board temp to chip tracker for autotuner.
                                        // Use mining-only tracker index (not raw chain index).
                                        if let Some(ref mut tracker) = chip_tracker {
                                            if let Some(Some(tracker_idx)) = chain_idx_to_tracker.get(target) {
                                                tracker.set_board_temp(*tracker_idx, temp);
                                            }
                                        }
                                    } else {
                                        debug!(chain_id = chain.chain_id, "Board temperature read failed — preserving last good sample until stale timeout");
                                    }
                                }
                            }
                        }
                    }

                    let current_temp_now_s = self.board_temp_time_base.elapsed().as_secs() as u32;

                    // Update MinerState with current hashrate and per-chain stats
                    self.state_tx.send_modify(|state| {
                        state.hashrate_ghs = hashrate.hashrate_avg;
                        state.hashrate_5s_ghs = hashrate.hashrate_5s;

                        // Update per-chain hashrate
                        for (i, chain_state) in state.chains.iter_mut().enumerate() {
                            if i < hashrate.chain_hashrate.len() {
                                chain_state.hashrate_ghs = hashrate.chain_hashrate[i];
                            }
                            if i < self.chains.len() && !self.i2c_active.load(Ordering::Acquire) {
                                chain_state.errors = self.chains[i].crc_errors();
                            }
                            if i < self.board_temp_seen_at.len() {
                                let seen_at_s = self.board_temp_seen_at[i].load(Ordering::Acquire);
                                let temp_is_stale = seen_at_s == 0
                                    || current_temp_now_s.saturating_sub(seen_at_s)
                                        > BOARD_TEMP_STALE_TIMEOUT_S as u32;
                                if temp_is_stale {
                                    chain_state.temp_c = 0.0;
                                }
                            }
                        }
                    });

                    if hashrate.hashrate_5s > 0.0 {
                        let ths_5s = hashrate.hashrate_5s / 1000.0;
                        let ths_avg = hashrate.hashrate_avg / 1000.0;
                        let elapsed_s = dispatch_start.elapsed().as_secs_f64();
                        let work_rate = if elapsed_s > 0.0 { total_work_dispatched as f64 / elapsed_s } else { 0.0 };

                        // Compute LIVE power from actual per-chip frequencies and voltages.
                        // Unlike the old static estimate, this reflects autotuner freq changes,
                        // thermal throttling, and voltage drift in real time.
                        let power_model = PowerModel::new_for_chip(self.chip_id)
                            .with_power_scale(self.current_power_scale());
                        let live_chains: Vec<(f64, Vec<u16>)> = self.chains
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.mining)
                            .map(|(i, c)| {
                                let voltage_v = if c.voltage_mv > 0 {
                                    c.voltage_mv as f64 / 1000.0
                                } else {
                                    9.1 // Safe fallback before PIC voltage readback
                                };
                                let freqs = if i < self.chip_frequencies.len() {
                                    self.chip_frequencies[i].clone()
                                } else {
                                    vec![c.frequency_mhz; c.chip_count as usize]
                                };
                                (voltage_v, freqs)
                            })
                            .collect();

                        let (live_chain_temps, fan_pwm, fan_rpm, pool_accepted, pool_rejected) = {
                            let state_snapshot = self.state_tx.borrow();
                            let live_chain_temps: Vec<f32> = self
                                .chains
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| c.mining)
                                .map(|(i, _)| {
                                    state_snapshot.chains.get(i).map(|c| c.temp_c).unwrap_or(0.0)
                                })
                                .collect();
                            (
                                live_chain_temps,
                                state_snapshot.fans.pwm,
                                state_snapshot.fans.rpm,
                                state_snapshot.accepted,
                                state_snapshot.rejected,
                            )
                        };

                        let mut live_power = power_model.estimate_live_with_telemetry(
                            &live_chains,
                            &live_chain_temps,
                            fan_pwm,
                            fan_rpm,
                            ths_5s,
                            self.psu_efficiency,
                        );
                        live_power.dispatcher_limits = self.collect_dispatcher_limits();
                        live_power.watt_cap = self.runtime_watt_cap_state(
                            live_power.wall_watts,
                            &live_power.dispatcher_limits,
                        );

                        // P1-3 (D-7): recompute the published J/TH against an
                        // EMA-smoothed hashrate denominator. `ths_5s` swings
                        // ~0..full-rate as nonce bursts arrive; dividing the
                        // near-constant wall power by it spiked efficiency to
                        // impossible values. Smooth the denominator and flag
                        // low confidence until the EMA has warmed up.
                        let (smoothed_ths, eff_confident) = efficiency_ema.update(ths_5s);
                        live_power.efficiency_jth =
                            efficiency_jth_from(live_power.wall_watts, smoothed_ths);
                        live_power.efficiency_jth_low_confidence = !eff_confident;

                        let p_total = live_power.board_watts;
                        let wall_watts = live_power.wall_watts;
                        let j_per_th = live_power.efficiency_jth;

                        // Publish live power estimate for REST API and WebSocket consumers
                        let _ = self.power_tx.send(live_power);

                        // Power model input data for accuracy debugging
                        let chains_mining = self.chains.iter().filter(|c| c.mining).count();
                        let chain_voltages: Vec<String> = self.chains.iter()
                            .filter(|c| c.mining)
                            .map(|c| format!("{}mV", c.voltage_mv))
                            .collect();
                        let chain_freq_avgs: Vec<String> = self.chip_frequencies.iter()
                            .enumerate()
                            .filter(|(i, _)| *i < self.chains.len() && self.chains[*i].mining)
                            .map(|(_, freqs)| {
                                if freqs.is_empty() { "0".to_string() }
                                else { format!("{:.0}MHz", freqs.iter().map(|&f| f as f64).sum::<f64>() / freqs.len() as f64) }
                            })
                            .collect();

                        // FPGA CRC error count aggregated across all chains. Distinct from
                        // `hw_errors` (dispatcher decode-path failures). Surfaces what the
                        // CGMiner-compat API reports as "Hardware Errors" so operators see
                        // chip-level CRC noise in real time without polling :4028.
                        let chain_errors_total: u64 = self.chains.iter().map(|c| c.crc_errors() as u64).sum();

                        info!(
                            hashrate_5s = format_args!("{:.2} GH/s ({:.3} TH/s)", hashrate.hashrate_5s, ths_5s),
                            hashrate_avg = format_args!("{:.2} GH/s ({:.3} TH/s)", hashrate.hashrate_avg, ths_avg),
                            total_nonces = hashrate.total_nonces,
                            work_dispatched = total_work_dispatched,
                            work_rate_per_sec = format_args!("{:.0}", work_rate),
                            shares_submitted,
                            pool_accepted,
                            pool_rejected,
                            dedup = dedup_discarded,
                            hw_errors,
                            chain_errors = chain_errors_total,
                            stale = stale_nonces,
                            stale_overwrite = stale_overwrite_nonces,
                            stale_empty = stale_empty_slot_nonces,
                            local_bm1398 = local_header_rejects_bm1398,
                            local_legacy = local_share_rejects_legacy,
                            version_drops = unsupported_version_drops,
                            clean_job_flushes,
                            pool_diff = format_args!("{:.0}", current_pool_difficulty),
                            chains_mining,
                            freq_avgs = format_args!("{}", chain_freq_avgs.join(",")),
                            power_w = format_args!("{:.0}", p_total),
                            wall_w = format_args!("{:.0}", wall_watts),
                            efficiency = format_args!("{:.1} J/TH", j_per_th),
                            btu_h = format_args!("{:.0}", dcentrald_autotuner::btu_from_watts(wall_watts)),
                            "Hashrate: {:.2} TH/s | {:.0} work/sec | sub:{} acc:{} rej:{} | drop dedup:{} stale:{} (ovr:{} empty:{}) local:{} version:{} clean:{} | Power: {:.0}W (wall: {:.0}W) | {:.1} J/TH | {} chains @ [{}]",
                            ths_5s, work_rate, shares_submitted, pool_accepted, pool_rejected,
                            dedup_discarded, stale_nonces, stale_overwrite_nonces, stale_empty_slot_nonces,
                            local_header_rejects_bm1398 + local_share_rejects_legacy,
                            unsupported_version_drops, clean_job_flushes,
                            p_total, wall_watts, j_per_th, chains_mining, chain_freq_avgs.join(","),
                        );

                        // Per-chain hashrate breakdown
                        for (i, chain) in self.chains.iter().enumerate() {
                            if i < hashrate.chain_hashrate.len() && chain.mining {
                                let chain_ths = hashrate.chain_hashrate[i] / 1000.0;
                                debug!(
                                    chain_id = chain.chain_id,
                                    hashrate_ghs = format_args!("{:.2}", hashrate.chain_hashrate[i]),
                                    hashrate_ths = format_args!("{:.3}", chain_ths),
                                    crc_errors = chain.crc_errors(),
                                    "  Chain {}: {:.2} GH/s, {} CRC errors",
                                    chain.chain_id, hashrate.chain_hashrate[i], chain.crc_errors(),
                                );
                            }
                        }
                    } else if current_job.is_some() && total_work_dispatched > 0 && total_nonces_received == 0 {
                        let elapsed = dispatch_start.elapsed();
                        // Zero-nonce watchdog (swarm review 2026-03-26): alert after 60s of zero nonces
                        if !zero_nonce_alarm_fired && elapsed.as_secs() >= 60 {
                            error!(
                                work_dispatched = total_work_dispatched,
                                elapsed_s = elapsed.as_secs(),
                                "ZERO NONCE ALARM: {} work items dispatched over {}s with 0 nonces back. \
                                 Something is fundamentally wrong. Check: (1) PIC voltage actually enabled? \
                                 (2) gate_block cleared? (3) FPGA UART not in BREAK state? (4) ASICs powered?",
                                total_work_dispatched, elapsed.as_secs(),
                            );
                            zero_nonce_alarm_fired = true;
                        }
                        // Check FPGA state for troubleshooting
                        let mut fifo_states = Vec::new();
                        for chain in self.chains.iter() {
                            if chain.mining {
                                let rx_has_data = chain.fpga.work_rx_has_data();
                                let tx_full = chain.fpga.work_tx_full();
                                fifo_states.push(format!(
                                    "ch{}: RX_DATA={}, TX_FULL={}",
                                    chain.chain_id,
                                    if rx_has_data { "YES" } else { "no" },
                                    if tx_full { "YES" } else { "no" },
                                ));
                            }
                        }
                        warn!(
                            work_dispatched = total_work_dispatched,
                            nonces_received = total_nonces_received,
                            elapsed_s = elapsed.as_secs(),
                            "NO NONCES after {}s — {} work items dispatched, 0 nonces back. \
                             FIFO state: [{}]. If this persists >30s, check: \
                             (1) register writes reaching ASICs? (2) baud mismatch? (3) voltage on? (4) hash board connected?",
                            elapsed.as_secs(), total_work_dispatched, fifo_states.join(", "),
                        );
                    }
                }
            }
        }

        // Log final stats — this is the mining session summary
        let voltage_reply_stop = voltage_reply_tasks
            .stop_and_join(VOLTAGE_REPLY_TASK_STOP_TIMEOUT)
            .await;
        if voltage_reply_stop.any_timed_out() {
            error!(
                timeout_ms = VOLTAGE_REPLY_TASK_STOP_TIMEOUT.as_millis(),
                "Voltage reply task did not terminate after dispatcher cancellation and abort"
            );
        } else if voltage_reply_stop.any_panicked() {
            warn!("Voltage reply task panicked before dispatcher shutdown");
        }

        info!("=== MINING SESSION SUMMARY ===");
        info!(
            total_nonces = hashrate.total_nonces,
            shares_submitted,
            discarded = discarded_nonces,
            dedup_discarded,
            hw_errors,
            stale_nonces,
            stale_overwrite_nonces,
            stale_empty_slot_nonces,
            local_header_rejects_bm1398,
            local_share_rejects_legacy,
            unsupported_version_drops,
            clean_job_flushes,
            "Work dispatcher stopped — {} nonces found, {} shares submitted, {} discarded (bm1398-local:{} legacy-local:{} version:{}), {} duplicates suppressed, {} stale (overwrite:{} empty:{}), {} clean-job flushes, {} HW errors",
            hashrate.total_nonces, shares_submitted, discarded_nonces,
            local_header_rejects_bm1398, local_share_rejects_legacy, unsupported_version_drops,
            dedup_discarded, stale_nonces, stale_overwrite_nonces, stale_empty_slot_nonces,
            clean_job_flushes, hw_errors,
        );
        if hw_errors > 0 {
            let hw_pct = hw_errors as f64 / (hashrate.total_nonces.max(1)) as f64 * 100.0;
            if hw_pct > 1.0 {
                warn!(
                    hw_error_pct = format_args!("{:.1}%", hw_pct),
                    "Hardware error rate {:.1}% is above 1% — this may indicate: overclocking too high, voltage too low, bad ASIC chip, or signal integrity issues on the UART chain",
                    hw_pct,
                );
            }
        }
        if let Some(session) = identity_composition_session {
            if let Err(error) = session.revoke() {
                warn!(
                    error = %error,
                    "Dispatcher composition session could not revoke measured hardware identity during shutdown"
                );
            }
        }
    }
}

/// Convert a nonce to its hex string representation (for share submission).
///
/// Stratum V1 uses VALUE hex: the u32 nonce value formatted as 8 hex chars.
/// Example: nonce 0x12345678 → "12345678"
///
/// This matches ESP-Miner's `sprintf("%08lx", nonce)` and cgminer's
/// `sprintf("%08x", swab32(nonce))` (where swab32 un-swaps the wire format).
///
/// The pool parses this as a value and places nonce.to_le_bytes() into the
/// block header at offset 76.
fn nonce_to_hex(nonce: u32) -> String {
    // Stratum V1: nonce as VALUE hex. Python verification proved:
    // ckpool(flip32+flip80) == our_standard_path for all header bytes.
    // The nonce format doesn't matter for matching — what matters is
    // that the ASIC finds VALID nonces (which it currently doesn't).
    format!("{:08x}", nonce)
}

/// Convert an ntime value to its hex string representation (for share submission).
///
/// Stratum V1 uses VALUE hex: the u32 ntime value formatted as 8 hex chars.
/// This is the same format the pool sends in mining.notify.
/// Example: ntime 0x504E86ED → "504e86ed"
fn ntime_to_hex(ntime: u32) -> String {
    // Stratum V1: ntime is VALUE hex (same format pool sends in mining.notify).
    // LE bytes causes "Ntime out of range" rejection from ckpool.
    format!("{:08x}", ntime)
}

fn dispatcher_build_header(entry: &WorkEntry, rolled_version: u32, nonce: u32) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&rolled_version.to_le_bytes());
    header[4..36].copy_from_slice(&entry.prev_block_hash);
    header[36..68].copy_from_slice(&entry.merkle_root);
    header[68..72].copy_from_slice(&entry.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&entry.header_tail[8..12]);
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

fn full_header_hash_be(header: &[u8; 80]) -> [u8; 32] {
    let hash = dcentrald_stratum::work::double_sha256(header);
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }
    hash_be
}

fn achieved_difficulty_from_header(header: &[u8; 80]) -> Option<f64> {
    let hash_be = full_header_hash_be(header);
    let difficulty = dcentrald_stratum::v1::difficulty::hash_to_difficulty(&hash_be);
    if difficulty.is_finite() && difficulty > 0.0 {
        Some(difficulty)
    } else {
        None
    }
}

fn hex_prefix(bytes: &[u8], prefix_bytes: usize) -> String {
    bytes
        .iter()
        .take(prefix_bytes)
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

/// Decode a hex string to bytes (no external crate dependency).
fn decode_hex_bytes(hex_str: &str) -> Vec<u8> {
    let hex_str = hex_str.trim();
    let mut bytes = Vec::with_capacity(hex_str.len() / 2);
    let mut chars = hex_str.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let byte = (hex_char_to_nibble(hi) << 4) | hex_char_to_nibble(lo);
        bytes.push(byte);
    }
    bytes
}

fn hex_char_to_nibble(c: char) -> u8 {
    match c {
        '0'..='9' => c as u8 - b'0',
        'a'..='f' => c as u8 - b'a' + 10,
        'A'..='F' => c as u8 - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_work_entry() -> WorkEntry {
        let mut share_target = [0u8; 32];
        share_target[0] = 0xFF;
        let mut header_tail = [0u8; 12];
        header_tail[0..4].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        header_tail[4..8].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        header_tail[8..12].copy_from_slice(&0x5566_7788u32.to_le_bytes());

        WorkEntry {
            job_id: "job-1".to_string(),
            extranonce2: "abcd1234".to_string(),
            ntime: 0x1122_3344,
            version: 0x2000_0000,
            share_target,
            version_bits_per_midstate: vec![None],
            pool_version_mask: 0,
            midstates: vec![[0x42; 32]],
            merkle_root: [0x22; 32],
            prev_block_hash: [0x11; 32],
            header_tail,
        }
    }

    #[test]
    fn normalize_tracker_chip_index_handles_dense_and_strided_families() {
        let bm1397_plan =
            dcentrald_api_types::asic_command::LinearAddressPlan::from_truncated_byte_space(48)
                .unwrap();
        let bm1398_plan = dcentrald_api_types::bm1398_protocol::S19_PRO_NBP1901_ADDRESS_PLAN;
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1387, 63, None, 12),
            Some(12)
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1362, 126, None, 42),
            Some(42)
        );

        // BM1397 uses strided hardware addresses (256 / 48 = 5).
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1397, 48, Some(bm1397_plan), 0),
            Some(0)
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1397, 48, Some(bm1397_plan), 5),
            Some(1)
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1397, 48, Some(bm1397_plan), 6),
            None,
            "unassigned strided addresses must not alias a neighboring chip"
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1397, 48, Some(bm1397_plan), 235),
            Some(47)
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1397, 48, Some(bm1397_plan), 240),
            None
        );

        // S19 Pro uses interval 2; raw address 225 is not assigned and used
        // to truncate to dense chip 112.
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1398, 114, Some(bm1398_plan), 224),
            Some(112)
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1398, 114, Some(bm1398_plan), 225),
            None
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1398, 113, Some(bm1398_plan), 226),
            None,
            "observed population cannot reinterpret the immutable composition plan"
        );
        assert_eq!(
            WorkDispatcher::normalize_tracker_chip_index(0x1398, 114, None, 224),
            None,
            "strided families fail closed without retained address authority"
        );
    }

    #[test]
    fn clamp_requested_freq_respects_chip_specific_floor_and_ceiling() {
        // BM1362 minimum supported PLL entry is 400 MHz, not the S9's 200 MHz.
        assert_eq!(
            WorkDispatcher::clamp_requested_freq(DispatchWriteChip::Bm1362, 200, None),
            400
        );
        assert_eq!(
            WorkDispatcher::clamp_requested_freq(DispatchWriteChip::Bm1362, 375, Some(450)),
            400
        );

        // S9/BM1387 still respects a 200 MHz floor and ceiling overlays.
        assert_eq!(
            WorkDispatcher::clamp_requested_freq(DispatchWriteChip::Bm1387, 150, None),
            200
        );
        assert_eq!(
            WorkDispatcher::clamp_requested_freq(DispatchWriteChip::Bm1387, 650, Some(600)),
            600
        );
    }

    #[test]
    fn frequency_limit_normalization_refuses_missing_chip_identity() {
        assert_eq!(
            WorkDispatcher::normalize_frequency_limit_for_chip(Some(0x1387), Some(150)),
            Some(Some(200)),
            "valid S9/BM1387 chains keep their 200 MHz floor"
        );
        assert_eq!(
            WorkDispatcher::normalize_frequency_limit_for_chip(Some(0x1362), Some(150)),
            Some(Some(400)),
            "valid BM1362 chains keep their 400 MHz floor"
        );
        assert_eq!(
            WorkDispatcher::normalize_frequency_limit_for_chip(None, Some(150)),
            None,
            "missing chain identity must not silently assume BM1387/S9"
        );
        assert_eq!(
            WorkDispatcher::normalize_frequency_limit_for_chip(Some(0xFFFF), Some(150)),
            None,
            "unknown chain identity must not inherit the BM1387 PLL table"
        );
        assert_eq!(
            WorkDispatcher::normalize_frequency_limit_for_chip(None, None),
            Some(None),
            "clearing an absent limit remains a no-op, not a chip-family guess"
        );
    }

    #[test]
    fn dispatch_write_identity_accepts_every_production_chip_route() {
        for chip_id in [0x1362, 0x1366, 0x1368, 0x1370, 0x1387, 0x1397, 0x1398] {
            let normalized =
                WorkDispatcher::normalize_dispatch_write_identity(chip_id, [chip_id, chip_id])
                    .expect("production chip identity should cross the write boundary");
            assert_eq!(normalized.chip_id(), chip_id);
        }
    }

    #[test]
    fn dispatch_write_identity_rejects_missing_unknown_and_mixed_chains() {
        assert_eq!(
            WorkDispatcher::normalize_dispatch_write_identity(0, [0]),
            Err(DispatchWriteIdentityError::Missing)
        );
        assert_eq!(
            WorkDispatcher::normalize_dispatch_write_identity(0xFFFF, [0xFFFF]),
            Err(DispatchWriteIdentityError::Unsupported(0xFFFF))
        );
        for scaffold_id in [0x1372, 0x1373] {
            assert_eq!(
                WorkDispatcher::normalize_dispatch_write_identity(scaffold_id, [scaffold_id]),
                Err(DispatchWriteIdentityError::Unsupported(scaffold_id)),
                "S23 scaffold identities must remain outside production work dispatch"
            );
        }
        assert_eq!(
            WorkDispatcher::normalize_dispatch_write_identity(0x1387, [0x1387, 0x1397]),
            Err(DispatchWriteIdentityError::Mixed {
                dispatcher_chip_id: 0x1387,
                chain_chip_id: 0x1397,
            })
        );
        assert_eq!(
            WorkDispatcher::normalize_dispatch_write_identity(0x1387, [0]),
            Err(DispatchWriteIdentityError::Missing),
            "a missing mining-chain identity disables the whole write policy"
        );
    }

    #[test]
    fn typed_work_time_keeps_known_routes_without_a_bm1387_fallback() {
        let freq = 500;
        let chip_count = 63;
        let midstate_shift = 2;
        let midstate_count = 1 << midstate_shift;

        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1387,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1387Driver::calculate_work_time(freq, midstate_count)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1362,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1362Driver::calculate_work_time_for(chip_count, freq)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1366,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1366Driver::calculate_work_time(freq, midstate_count)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1368,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1368Driver::calculate_work_time(freq, chip_count)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1370,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1370Driver::calculate_work_time(freq, chip_count)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1397,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1397Driver::calculate_work_time(freq, midstate_count)
        );
        assert_eq!(
            WorkDispatcher::calculate_work_time_for_chip(
                DispatchWriteChip::Bm1398,
                chip_count,
                freq,
                midstate_shift,
            ),
            Bm1398Driver::calculate_work_time(freq, midstate_count)
        );
        assert_eq!(
            DispatchWriteChip::try_from(0x1396),
            Err(DispatchWriteIdentityError::Unsupported(0x1396)),
            "BM1396 has no production dispatcher driver and must not borrow BM1387 timing"
        );
    }

    #[test]
    fn chain_frequency_limits_use_most_restrictive_active_source() {
        let mut limits = ChainFrequencyLimits::default();
        assert_eq!(limits.effective_ceiling(), None);

        assert!(limits.set(FrequencyLimitSource::Thermal, Some(650)));
        assert_eq!(limits.effective_ceiling(), Some(650));

        assert!(limits.set(FrequencyLimitSource::QuietMode, Some(600)));
        assert_eq!(limits.effective_ceiling(), Some(600));

        assert!(limits.set(FrequencyLimitSource::PowerCap, Some(575)));
        assert_eq!(limits.effective_ceiling(), Some(575));

        assert!(limits.set(FrequencyLimitSource::PowerCap, None));
        assert_eq!(limits.effective_ceiling(), Some(600));

        assert!(limits.set(FrequencyLimitSource::AutotunerThermal, Some(590)));
        assert_eq!(limits.effective_ceiling(), Some(590));

        assert!(limits.set(FrequencyLimitSource::QuietMode, None));
        assert_eq!(limits.effective_ceiling(), Some(590));

        assert!(limits.set(FrequencyLimitSource::AutotunerThermal, None));
        assert_eq!(limits.effective_ceiling(), Some(650));
    }

    #[test]
    fn bm1398_uses_8bit_fpga_work_id_ring() {
        assert!(WorkDispatcher::uses_8bit_fpga_work_id(0x1398));
        assert_eq!(WorkDispatcher::work_id_space_for(0x1398, 2), 256);

        let hw_work_id = (0x00ABu16 << 2) | 0x03;
        let (work_id, midstate_idx) =
            WorkDispatcher::decode_fpga_work_id_for_dispatch(0x1398, hw_work_id, 2).unwrap();

        assert_eq!(work_id, 0x00AB);
        assert_eq!(midstate_idx, 3);
    }

    #[test]
    fn non_legacy_fpga_work_id_keeps_widened_ring() {
        assert!(!WorkDispatcher::uses_8bit_fpga_work_id(0x1362));
        assert_eq!(WorkDispatcher::work_id_space_for(0x1362, 2), 1 << 14);

        let hw_work_id = (0x12ABu16 << 2) | 0x02;
        let (work_id, midstate_idx) =
            WorkDispatcher::decode_fpga_work_id_for_dispatch(0x1362, hw_work_id, 2).unwrap();

        assert_eq!(work_id, 0x12AB);
        assert_eq!(midstate_idx, 2);
    }

    /// W17 regression: dispatch 257 jobs with monotonically-increasing work_id
    /// using the same wrapping arithmetic the dispatcher uses, and confirm
    /// the ring wraps at 256 for the BM1387/BM1397/BM1398 family.
    ///
    /// Without the 8-bit ring invariant, a u16 counter would overflow at 65536
    /// instead of 256, and the FPGA's 8-bit work_id field would alias every
    /// nonce back to a wrong slot — pool rejects ~89% of nonces as stale.
    ///.
    #[test]
    fn bm1398_work_id_counter_wraps_at_256_for_257_jobs() {
        for chip_id in [0x1387u16, 0x1397, 0x1398] {
            let work_id_space = WorkDispatcher::work_id_space_for(chip_id, 2);
            assert_eq!(
                work_id_space, 256,
                "{:#06x} must use 8-bit FPGA work_id ring",
                chip_id
            );
            let work_id_mask = (work_id_space - 1) as u16;
            assert_eq!(work_id_mask, 0x00FF);

            // Drive the counter the same way the dispatcher does
            // (wrapping_add(1) & work_id_mask).
            let mut counter: u16 = 0;
            let mut seen_zero_after_first = false;
            for i in 0..257u32 {
                if i == 0 {
                    assert_eq!(counter, 0, "first work_id must be 0");
                }
                let next = counter.wrapping_add(1) & work_id_mask;
                if i == 255 {
                    // After 256 increments we should be back at 0.
                    assert_eq!(next, 0, "ring must wrap to 0 after 256 jobs");
                    seen_zero_after_first = true;
                }
                counter = next;
            }
            assert!(
                seen_zero_after_first,
                "{:#06x}: ring did not wrap as expected",
                chip_id
            );
            // After 257 steps from 0, counter is at 1.
            assert_eq!(counter, 1);
        }
    }

    /// BM1398's carrier echoes a 16-bit extended field but admits only an
    /// 8-bit logical ring. High logical bits are corruption, not permission
    /// to mask-alias an old work-table slot.
    #[test]
    fn bm1398_decode_refuses_high_logical_work_id_bits() {
        let hw_work_id = (0x3F12u16 << 2) | 0x01;
        assert_eq!(
            WorkDispatcher::decode_fpga_work_id_for_dispatch(0x1398, hw_work_id, 2),
            None
        );

        // Same hw_work_id on a BM1362 (16-bit ring) keeps the upper bits.
        let (decoded_legacy, ms_idx_legacy) =
            WorkDispatcher::decode_fpga_work_id_for_dispatch(0x1362, hw_work_id, 2).unwrap();
        assert_eq!(decoded_legacy, 0x3F12);
        assert_eq!(ms_idx_legacy, 1);
    }

    /// MINE-3 regression: a non-8-bit chip whose live FPGA reports FEWER
    /// midstates than the configured `max_midstate_shift` decodes a work_id
    /// that can EXCEED `work_id_space` (= `work_table.len()`). The nonce-RX
    /// lookup must use a checked `.get()` so this never indexes out of bounds
    /// and panics (which, under `panic=abort`, would crash the whole daemon on
    /// attacker- or glitch-controlled wire data).
    ///
    /// Before the fix, the lookup was `work_table[work_id as usize]` — this test
    /// would index past the end and panic. After the fix it returns `None` and
    /// the nonce is treated as stale/garbage.
    #[test]
    fn mine3_oversized_decoded_work_id_does_not_index_out_of_bounds() {
        // BM1362 is a non-8-bit-work-id chip. Size the table the way the
        // dispatcher does, from the CONFIGURED max midstate shift.
        let max_midstate_shift = 2u32;
        let work_id_space = WorkDispatcher::work_id_space_for(0x1362, max_midstate_shift);
        assert_eq!(work_id_space, 1 << 14); // 16384

        // But the LIVE FPGA reports 0 midstates here (fpga_midstate_cnt = 0),
        // so the decode shift is 0 and the full 16-bit hw work_id survives.
        let hw_work_id = 0xFFFFu16;
        let (decoded_work_id, _midstate_idx) =
            WorkDispatcher::decode_fpga_work_id_for_dispatch(0x1362, hw_work_id, 0).unwrap();
        assert_eq!(decoded_work_id, 0xFFFF); // 65535 — far past 16384

        // The decoded id is out of range for the table.
        assert!(
            (decoded_work_id as usize) >= work_id_space,
            "test precondition: decoded work_id must exceed work_id_space"
        );

        // The dispatcher's checked lookup pattern must NOT panic and must
        // resolve to the stale/garbage path (None), exactly like an empty slot.
        let work_table: Vec<Option<u32>> = vec![None; work_id_space];
        let slot = work_table.get(decoded_work_id as usize);
        assert!(
            slot.is_none(),
            "out-of-range work_id must miss the table (no panic, no UB)"
        );
        // A raw `work_table[decoded_work_id as usize]` here would have panicked.
    }

    /// W17 regression: ASICBoost's three atomic changes cooperate end-to-end:
    /// 1. version_bits_per_midstate produces non-empty entries when host
    ///    version rolling is supported AND mask is non-zero.
    /// 2. rolled_version_from_bits round-trips: rolling N bits then XOR-ing
    ///    against base_version reproduces the rolled version stored in slot N.
    /// 3. With mask=0 (no rolling), all entries are None — and submit will
    ///    omit the BIP310 version_bits field per messages.rs.
    /// + .
    #[test]
    fn asicboost_three_changes_round_trip_for_bm1398() {
        let base_version = 0x2000_0000u32;
        let mask = 0x1fff_e000u32;

        // Change 1: per-midstate version_bits populated when mask != 0
        let bits = WorkDispatcher::version_bits_per_midstate(0x1398, base_version, mask, 4);
        assert_eq!(bits.len(), 4);
        assert!(bits.iter().all(|b| b.is_some()));
        // First midstate is base (no roll yet) => version_bits == "00000000"
        assert_eq!(bits[0].as_deref(), Some("00000000"));
        // Subsequent slots must be unique
        let unique: std::collections::HashSet<_> = bits.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            4,
            "each midstate must use a unique rolled version"
        );

        // Change 2 (round-trip): each rolled version reconstructed from bits
        // matches the corresponding slot
        for b in &bits {
            let rolled = WorkDispatcher::rolled_version_from_bits(base_version, b.as_deref());
            // Bits outside the mask must be preserved
            assert_eq!(rolled & !mask, base_version & !mask);
            // The XOR delta must equal the version_bits string
            assert_eq!(
                format!("{:08x}", rolled ^ base_version),
                b.as_deref().unwrap()
            );
        }

        // Change 3: mask=0 (no rolling) -> all None -> submit drops version_bits
        let no_roll = WorkDispatcher::version_bits_per_midstate(0x1398, base_version, 0, 4);
        assert_eq!(no_roll.len(), 4);
        assert!(no_roll.iter().all(|b| b.is_none()));

        // Chips that don't support host version_rolling get no bits even
        // when the pool advertises a mask (e.g. BM1362+ rolls inside the chip
        // and the host should not emit BIP310 version_bits).
        let bm1362 = WorkDispatcher::version_bits_per_midstate(0x1362, base_version, mask, 4);
        assert!(bm1362.iter().all(|b| b.is_none()));
    }

    #[test]
    fn version_bits_reconstruct_rolled_versions_by_midstate() {
        let base_version = 0x2000_0000;
        let version_mask = 0x1fff_e000;
        let version_bits =
            WorkDispatcher::version_bits_per_midstate(0x1398, base_version, version_mask, 4);

        assert_eq!(version_bits[0].as_deref(), Some("00000000"));
        for bits in &version_bits {
            let rolled = WorkDispatcher::rolled_version_from_bits(base_version, bits.as_deref());
            assert_eq!(rolled & !version_mask, base_version & !version_mask);
            assert_eq!(
                rolled ^ base_version,
                u32::from_str_radix(bits.as_ref().unwrap(), 16).unwrap()
            );
        }
        assert!(version_bits.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn unsupported_fpga_rx_chips_disable_host_version_mask() {
        let version_mask = 0x1fff_e000;

        for chip_id in [0x1362u16, 0x1366, 0x1368, 0x1370] {
            assert_eq!(
                WorkDispatcher::effective_version_mask(chip_id, version_mask),
                0
            );
            assert!(!WorkDispatcher::supports_fpga_version_rolling_submission(
                chip_id
            ));
            assert!(WorkDispatcher::version_bits_per_midstate(
                chip_id,
                0x2000_0000,
                version_mask,
                4,
            )
            .iter()
            .all(|bits| bits.is_none()));
        }

        assert_eq!(
            WorkDispatcher::effective_version_mask(0x1398, version_mask),
            version_mask
        );
        assert!(WorkDispatcher::supports_fpga_version_rolling_submission(
            0x1398
        ));
    }

    #[test]
    fn bm1387_runtime_refuses_jobs_without_version_mask() {
        assert!(WorkDispatcher::requires_negotiated_version_mask(0x1387));
        assert!(!WorkDispatcher::accepts_job_version_mask(0x1387, 0));
        assert!(WorkDispatcher::accepts_job_version_mask(
            0x1387,
            0x1fff_e000
        ));

        for chip_id in [0x1397u16, 0x1398, 0x1362, 0x1366, 0x1368, 0x1370] {
            assert!(
                WorkDispatcher::accepts_job_version_mask(chip_id, 0),
                "non-BM1387 chip 0x{chip_id:04X} must not inherit the S9-only refusal"
            );
        }
        assert!(
            !WorkDispatcher::accepts_job_version_mask(0xFFFF, 0x1fff_e000),
            "an unknown chip must not accept a job under a guessed version policy"
        );
        assert!(
            !WorkDispatcher::accepts_job_version_mask(0, 0x1fff_e000),
            "a missing chip identity must not accept a job under a guessed version policy"
        );
    }

    #[test]
    fn dispatcher_build_header_places_fields_correctly() {
        let entry = sample_work_entry();
        let rolled_version = 0x3A5A_0000;
        let nonce = 0xCAFEBABE;

        let header = dispatcher_build_header(&entry, rolled_version, nonce);

        assert_eq!(&header[0..4], &rolled_version.to_le_bytes());
        assert_eq!(&header[4..36], &entry.prev_block_hash);
        assert_eq!(&header[36..68], &entry.merkle_root);
        assert_eq!(&header[68..72], &entry.ntime.to_le_bytes());
        assert_eq!(&header[72..76], &0x5566_7788u32.to_le_bytes());
        assert_eq!(&header[76..80], &nonce.to_le_bytes());
    }

    #[test]
    fn dispatcher_build_header_ignores_merkle4_prefix_in_header_tail() {
        let entry = sample_work_entry();
        let header = dispatcher_build_header(&entry, entry.version, 0x0102_0304);

        assert_eq!(&header[36..68], &entry.merkle_root);
        assert_ne!(&header[36..40], &entry.header_tail[0..4]);
    }

    // -----------------------------------------------------------------------
    //  W1 — stale-age divisor logic
    //
    // The actual stale-eviction site lives inside `WorkDispatcher::run()`
    // and reaches into FPGA / async channels, so we can't drive it from a
    // unit test without massive scaffolding. Instead, pin the pure
    // arithmetic that the eviction site uses, so a future refactor can't
    // silently break the threshold computation.
    // -----------------------------------------------------------------------

    fn stale_threshold(work_id_space: usize, divisor: u32) -> u64 {
        (work_id_space as u64)
            .saturating_div(divisor.max(1) as u64)
            .max(1)
    }

    #[test]
    fn stale_age_divisor_default_4_yields_64_cycle_threshold_for_bm1387() {
        // BM1387's 8-bit ring +  default divisor 4 = 64-cycle
        // freshness window. This is the load-bearing arithmetic from
        // the W1 fix; pin it so a future refactor can't drift.
        let space = WorkDispatcher::work_id_space_for(0x1387, 2);
        assert_eq!(space, 256);
        assert_eq!(stale_threshold(space, 4), 64);
    }

    #[test]
    fn stale_age_divisor_one_preserves_legacy_behavior() {
        // Operator override: divisor = 1 -> threshold = work_id_space.
        // Used for rollback if the tighter threshold regresses on a
        // unit we haven't profiled.
        let space_8bit = WorkDispatcher::work_id_space_for(0x1387, 2);
        assert_eq!(stale_threshold(space_8bit, 1), space_8bit as u64);
        let space_16bit = WorkDispatcher::work_id_space_for(0x1362, 2);
        assert_eq!(stale_threshold(space_16bit, 1), space_16bit as u64);
    }

    #[test]
    fn stale_age_divisor_zero_clamps_to_one() {
        // A misconfigured divisor of 0 would otherwise divide-by-zero;
        // the runtime clamps to 1 (= legacy behavior). Pin the clamp.
        let space = WorkDispatcher::work_id_space_for(0x1387, 2);
        assert_eq!(stale_threshold(space, 0), space as u64);
    }

    #[test]
    fn stale_age_divisor_4_safe_for_16bit_ring_chips() {
        // BM1362/BM1366/BM1368/BM1370 use a 16-bit ring
        // (work_id_space = 1 << 14 = 16384 with the canonical
        // 4-midstate / 2-bit shift configuration). Divisor 4 gives a
        // 4096-cycle threshold there — still 16x more than typical
        // pipeline depth, so the default is safe across chip families.
        for chip_id in [0x1362u16, 0x1368, 0x1370] {
            let space = WorkDispatcher::work_id_space_for(chip_id, 2);
            assert!(space >= 4096, "expected 16-bit ring for {:#06x}", chip_id);
            let thresh = stale_threshold(space, 4);
            assert_eq!(thresh, space as u64 / 4);
            assert!(thresh >= 1024, "16-bit ring threshold too small");
        }
    }

    // -----------------------------------------------------------------------
    // PR-052 — S19 Pro BM1398 share-acceptance regression pins.
    //
    // These tests close the final Contract-5 gap (ValidShare.version ↔
    // version_bits consistency for BM1398 non-zero-delta midstate slots)
    // identified in the PR-052 coverage audit.  The W17 tests above cover
    // the per-midstate version_bits generation and rolled_version_from_bits
    // round-trip, but do not assert the two fields form a *consistent pair*
    // when assembled into a ValidShare.
    //
    // Per memory rules:
    //
    //
    //
    // -----------------------------------------------------------------------

    /// PR-052 Contract-5 pin: for BM1398 with host-side BIP320 rolling (FPGA
    /// path), every non-zero-delta midstate slot produces a (version_bits,
    /// version) pair where:
    ///
    ///   `version == rolled_version_from_bits(base_version, version_bits)`
    ///
    /// This is the integrity constraint the submit path relies on — if the two
    /// fields drift apart the pool sees a version_bits XOR delta that does not
    /// match the header version, producing a reject.
    ///
    /// Additionally, the test confirms that:
    ///   * slot 0 has `version_bits == "00000000"` and `version == base_version`
    ///     (no rolling for the first midstate)
    ///   * slots 1..=3 each have a non-zero `version_bits` string (BM1398 uses
    ///     4 midstates, so 3 of them are rolled)
    ///   * `validate_full_header` is the ONLY gate referenced on the BM1398 FPGA
    ///     path — no `version_bits_raw != 0` rejection filter is applied here
    ///     (BM1398 is the FPGA chip; the chip-internal serialnonce
    ///     `version_bits_raw` field lives only in the BM1362-family nonce frame;
    ///     on BM1398 the rolled version comes from the host's own per-midstate
    ///     table, so the banned anti-pattern does not apply to this path either)
    #[test]
    fn pr052_bm1398_valid_share_version_and_version_bits_are_consistent_pair() {
        let base_version: u32 = 0x2000_0000;
        let version_mask: u32 = 0x1FFF_E000; // BIP320 canonical mask
        let midstate_count = 4; // BM1398 / NUM_MIDSTATES = 4

        // Generate the same per-midstate version_bits the dispatcher uses.
        let bits = WorkDispatcher::version_bits_per_midstate(
            0x1398,
            base_version,
            version_mask,
            midstate_count,
        );

        // All 4 slots must be populated (BM1398 supports host version rolling).
        assert_eq!(
            bits.len(),
            midstate_count,
            "BM1398 must generate one version_bits entry per midstate"
        );
        assert!(
            bits.iter().all(|b| b.is_some()),
            "every BM1398 midstate slot must have a version_bits string"
        );

        // Slot 0: no roll applied — version_bits == "00000000", version == base.
        assert_eq!(
            bits[0].as_deref(),
            Some("00000000"),
            "BM1398 slot 0: first midstate must carry zero-delta version_bits"
        );
        let slot0_version =
            WorkDispatcher::rolled_version_from_bits(base_version, bits[0].as_deref());
        assert_eq!(
            slot0_version, base_version,
            "BM1398 slot 0: rolled version must equal base_version when delta is zero"
        );

        // Slots 1..3: non-zero delta, and (version_bits, version) form a
        // consistent pair via the same rolled_version_from_bits call the submit
        // path uses when it assembles ValidShare.
        for (idx, vb) in bits.iter().enumerate().skip(1) {
            let vb_str = vb.as_deref().unwrap();

            // Non-zero delta for all non-zero slots.
            assert_ne!(
                vb_str, "00000000",
                "BM1398 slot {idx}: non-zero midstate must carry non-zero version_bits delta"
            );

            // Reconstruct the rolled version from the version_bits string —
            // this is the exact call the submit path makes when building the
            // ValidShare.version field.
            let rolled_version =
                WorkDispatcher::rolled_version_from_bits(base_version, Some(vb_str));

            // The two fields are consistent: version == base ^ delta.
            let delta = u32::from_str_radix(vb_str, 16).expect("version_bits must be valid hex");
            assert_eq!(
                rolled_version,
                base_version ^ delta,
                "BM1398 slot {idx}: ValidShare.version must equal base ^ version_bits_delta"
            );

            // version_bits delta is strictly inside the BIP320 field (no spill).
            assert_eq!(
                delta & !version_mask,
                0,
                "BM1398 slot {idx}: version_bits delta must not escape the BIP320 mask"
            );

            // Outside-field bits of base_version are preserved in rolled_version.
            assert_eq!(
                rolled_version & !version_mask,
                base_version & !version_mask,
                "BM1398 slot {idx}: rolled_version must preserve outside-field base bits"
            );

            // Idempotent: reconstructing from the same string gives the same result.
            assert_eq!(
                WorkDispatcher::rolled_version_from_bits(base_version, Some(vb_str)),
                rolled_version,
                "BM1398 slot {idx}: rolled_version_from_bits must be a pure function"
            );
        }

        // All 4 (version_bits, version) pairs are distinct — no two midstates
        // share the same rolled version (regression guard against off-by-one in
        // the BIP320 increment loop).
        let versions: Vec<u32> = bits
            .iter()
            .map(|vb| WorkDispatcher::rolled_version_from_bits(base_version, vb.as_deref()))
            .collect();
        let unique_versions: std::collections::HashSet<u32> = versions.iter().cloned().collect();
        assert_eq!(
            unique_versions.len(),
            midstate_count,
            "BM1398: all 4 midstate rolled versions must be distinct"
        );
    }

    /// PR-052 Contract-2/3 companion pin for the BM1398 FPGA path:
    /// `validate_full_header` is the sole gate on nonce acceptance.
    ///
    /// BM1398 uses host-side BIP320 rolling (FPGA WORK_TX); the per-chip
    /// nonce frame's `version_bits_raw` field (from the BM1362-family serial
    /// wire format) does NOT exist in the BM1398 FPGA WORK_RX path.  This
    /// test pins that the BM1398 chip family supports FPGA version rolling
    /// (`supports_fpga_version_rolling_submission`) while the banned serial-path
    /// anti-pattern (`if nr.version_bits_raw != 0 { continue; }`) is
    /// architecturally absent from this chip's code path.
    ///
    /// The positive side: the only gate is the full-header SHA-256 check via
    /// `validate_full_header` (pinned separately in `dcentrald-stratum`).
    /// The negative side: a non-zero version_bits string on BM1398 means the
    /// host rolled the version itself — it is never a reason to drop the nonce.
    #[test]
    fn pr052_bm1398_fpga_path_supports_host_version_rolling_no_chip_serial_filter() {
        // BM1398 is a chip that supports FPGA-side host version rolling.
        assert!(
            WorkDispatcher::supports_fpga_version_rolling_submission(0x1398),
            "BM1398 must support host-side FPGA version rolling"
        );

        // The effective mask for BM1398 equals the pool mask (no zeroing-out).
        let pool_mask: u32 = 0x1FFF_E000;
        assert_eq!(
            WorkDispatcher::effective_version_mask(0x1398, pool_mask),
            pool_mask,
            "BM1398 effective_version_mask must pass the pool mask through unchanged"
        );

        // A non-zero-delta version_bits string for BM1398 is a valid rolled
        // version produced by the host — it maps to a concrete rolled_version.
        // Specifically: delta "00200000" → rolled = base ^ delta.
        let base_version: u32 = 0x2000_0000;
        let vb_str = "00200000"; // representative non-zero delta inside BIP320 mask
        let rolled = WorkDispatcher::rolled_version_from_bits(base_version, Some(vb_str));
        assert_ne!(
            rolled, base_version,
            "BM1398: non-zero version_bits must produce a rolled_version distinct from base"
        );
        assert_eq!(
            rolled,
            base_version ^ 0x0020_0000,
            "BM1398: rolled_version must equal base XOR delta"
        );

        // BM1362-family chips (which DO use chip-internal rolling and whose
        // `version_bits_raw` nonce-frame field is the one the banned anti-pattern
        // wrongly filtered) must return FALSE from supports_fpga_version_rolling,
        // confirming the two code paths are separate.
        for chip_id in [0x1362u16, 0x1366, 0x1368, 0x1370] {
            assert!(
                !WorkDispatcher::supports_fpga_version_rolling_submission(chip_id),
                "chip 0x{chip_id:04X} (BM1362-family) must NOT use host FPGA version rolling"
            );
            // Their effective mask is forced to zero (host does NOT roll for them).
            assert_eq!(
                WorkDispatcher::effective_version_mask(chip_id, pool_mask),
                0,
                "chip 0x{chip_id:04X}: effective_version_mask must be 0 (chip rolls internally)"
            );
        }
    }
}
