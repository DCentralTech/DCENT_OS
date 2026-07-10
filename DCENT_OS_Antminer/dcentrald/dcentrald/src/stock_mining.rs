//! Stock Bitmain FPGA mining path.
//!
//! This module provides a complete mining pipeline for S9 miners running
//! stock Bitmain firmware (kernel 3.14.0-xilinx with bitmain_axi.ko +
//! fpga_mem_driver.ko). It replaces the BraiinsOS-specific UIO/FIFO
//! approach with direct mmap access to the stock FPGA register block.
//!
//! Architecture differences from the BraiinsOS path (daemon.rs):
//!
//!   - **FPGA registers**: Single flat 352-byte block at 0x43C00000 via
//!     /dev/axi_fpga_dev, vs per-chain 4KB UIO blocks.
//!   - **PIC I2C**: Via FPGA IIC_COMMAND register (0x030), vs kernel
//!     /dev/i2c-0 or AXI IIC devmem.
//!   - **Work dispatch**: DHASH accelerator + DMA double-buffer, vs
//!     per-chain WORK_TX_FIFO.
//!   - **Nonce collection**: Shared RETURN_NONCE FIFO (0x010), vs
//!     per-chain WORK_RX_FIFO.
//!   - **Board detect**: FPGA HASH_ON_PLUG register (0x008), vs sysfs GPIO.
//!   - **Fan control**: FPGA FAN_CONTROL register (0x084), vs UIO fan IP.
//!   - **ASIC commands**: BC_WRITE_COMMAND register (0x0C0), vs per-chain
//!     CMD TX/RX FIFOs.
//!
//! The ASIC init sequence (chain_inactive, set_address, set_freq, open_core)
//! is the same — only the register access method changes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use dcentrald_hal::stock_fpga::*;
use dcentrald_hal::stock_fpga_iic::StockFpgaI2c;
use dcentrald_hal::stock_fpga_work::{StockFpgaDma, StockFpgaWorkEngine};

use crate::config::DcentraldConfig;

// ---------------------------------------------------------------------------
// Stock FPGA chain numbering
// ---------------------------------------------------------------------------

/// Stock Bitmain chain IDs (maps to HASH_ON_PLUG bit positions).
/// These are the physical chain numbers used in the FPGA's IIC_COMMAND register.
///
/// BUG FIX (2026-04-11): Was hardcoded to [6, 7, 8]. Some S9 units use
/// chains [5, 6, 7] instead. Now check all possible chain positions (5-8)
/// and detect which ones have boards via HASH_ON_PLUG register.
const STOCK_CHAIN_IDS: [u8; 4] = [5, 6, 7, 8];

/// Number of BM1387 chips per S9 hash board.
const CHIPS_PER_CHAIN: u8 = 63;

/// Default PIC DAC value for ~9.10V operating voltage.
/// pic_val = round(1608.42 - 170.42 * 9.1) = 57
const DEFAULT_VOLTAGE_DAC: u8 = 57;

/// Safe init voltage DAC (~9.4V). Used during chip enumeration.
const INIT_VOLTAGE_DAC: u8 = 6;

/// PIC heartbeat interval (ms). Well within the ~1 minute stock PIC timeout.
const HEARTBEAT_INTERVAL_MS: u64 = 5000;

/// Hardware difficulty for BM1387 with TicketMask 0xFF = diff 256.
const HW_DIFFICULTY: u64 = 256;

// ---------------------------------------------------------------------------
// StockMiner — top-level stock FPGA mining orchestrator
// ---------------------------------------------------------------------------

/// Process-global crash-panic teardown state for the stock-fpga (S9 BM1387)
/// path. Holds the energized chain list so the `main()` panic hook can re-open
/// the FPGA and cut their voltage. Mirrors the am2 `AM2_TEARDOWN_PARAMS` /
/// am3-aml `NOPIC_TEARDOWN_ARMED` / am3-bb `AM3BB_TEARDOWN_ARMED` pattern —
/// the stock-fpga path was the one energizing path with NO panic-hook peer
/// (prod-readiness hunt needs_more_thought #1).
static STOCK_FPGA_TEARDOWN_ARMED: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// Arm the stock-fpga crash-panic teardown. Call once, right after the per-chain
/// voltage is enabled (boards energized). Idempotent (`OnceLock::set`).
fn arm_stock_fpga_teardown(chains: &[u8]) {
    let _ = STOCK_FPGA_TEARDOWN_ARMED.set(chains.to_vec());
}

/// Best-effort cut-hash teardown for the `main()` crash panic hook on the
/// stock-fpga (S9) path. No-op (allocation-free early return) unless a
/// stock-fpga run armed it. Re-opens the FPGA (the running handle may be held by
/// the panicking thread — the Ok-path graceful shutdown re-opens the same way at
/// `StockFpga::open()` below) and drives `enable_voltage(chain, false)` per
/// energized chain to cut the rail. Swallows ALL errors — must NEVER re-panic
/// from inside the panic hook.
///
/// Why this matters: the release profile is `panic = "abort"`, so a panic runs
/// NO `Drop`, and `StockMiner` has no Drop guard. Without this the only S9
/// backstop after a panic mid-bringup is the ~60 s PIC heartbeat watchdog,
/// leaving the boards energized in the meantime. Fans are left to the FPGA
/// cooldown register the Ok path sets; the actual fire-risk mitigation is
/// cutting the chip rail, which this does immediately.
pub fn stock_fpga_panic_hook_best_effort_teardown() {
    let Some(chains) = STOCK_FPGA_TEARDOWN_ARMED.get() else {
        return;
    };
    if chains.is_empty() {
        return;
    }
    if let Ok(fpga) = StockFpga::open() {
        let i2c = StockFpgaI2c::new(&fpga);
        for &chain in chains {
            let _ = i2c.enable_voltage(chain, false);
        }
    }
}

/// Stock Bitmain FPGA mining orchestrator.
///
/// Owns the stock FPGA register interface, DMA buffers, work engine,
/// and PIC I2C interface. Manages the full mining lifecycle from board
/// detection through work dispatch and nonce collection.
pub struct StockMiner {
    config: DcentraldConfig,
    shutdown: CancellationToken,
}

impl StockMiner {
    pub fn new(config: DcentraldConfig, shutdown: CancellationToken) -> Self {
        Self { config, shutdown }
    }

    /// Run the stock FPGA mining pipeline.
    ///
    /// This is the stock equivalent of Daemon::run(). It handles:
    /// 1. FPGA register block open + version verify
    /// 2. Hash board detection via HASH_ON_PLUG
    /// 3. PIC init via FPGA I2C (voltage, enable, heartbeat)
    /// 4. ASIC chain init (chain_inactive, set_address, set_freq, open_core)
    /// 5. Stratum pool connection (reuses existing stratum client)
    /// 6. Work dispatch via DHASH accelerator + DMA
    /// 7. Nonce collection via shared RETURN_NONCE FIFO
    /// 8. Share validation and submission
    pub async fn run(&mut self) -> Result<()> {
        info!("=== STOCK FPGA MINING PATH ===");
        info!("Using stock Bitmain FPGA register interface (/dev/axi_fpga_dev)");
        info!("This path does NOT require BraiinsOS boot components or UIO devices");

        // ---- Phase 1: Open stock FPGA register block ----
        info!("--- Phase 1: Opening stock FPGA registers ---");
        let fpga = StockFpga::open()
            .context("Failed to open stock FPGA — is /dev/axi_fpga_dev present? This requires stock Bitmain kernel modules.")?;

        let version = fpga.read_version();
        let board_type = ((version >> 8) & 0xFF) as u16;
        let fpga_version = version & 0xFF;

        info!(
            version = format_args!("0x{:08X}", version),
            board_type = format_args!("0x{:02X}", board_type),
            fpga_ver = fpga_version,
            "Stock FPGA version: 0x{:08X} (board=0x{:02X}, ver={})",
            version,
            board_type,
            fpga_version,
        );

        if board_type != BOARD_TYPE_C5 {
            warn!(
                "FPGA board type 0x{:02X} is not C5 (Zynq S9) — proceeding anyway but results may be unexpected",
                board_type,
            );
        }

        let chip_id = fpga.read_chip_id();
        info!(
            chip_id = format_args!("0x{:016X}", chip_id),
            "FPGA chip ID: 0x{:016X}", chip_id,
        );

        // ---- Phase 2: Detect hash boards ----
        info!("--- Phase 2: Hash board detection via HASH_ON_PLUG register ---");
        let plug = fpga.read_hash_on_plug();
        info!(
            hash_on_plug = format_args!("0x{:02X}", plug),
            "HASH_ON_PLUG = 0x{:02X} (0xE0 = all 3 boards present)", plug,
        );

        let mut detected_chains: Vec<u8> = Vec::new(); // chain IDs
        for &chain_id in &STOCK_CHAIN_IDS {
            if fpga.is_board_present(chain_id) {
                info!(
                    chain_id,
                    connector = format_args!("J{}", chain_id + 1),
                    "Hash board DETECTED on chain {} (J{})",
                    chain_id,
                    chain_id + 1,
                );
                detected_chains.push(chain_id);
            } else {
                info!(
                    chain_id,
                    connector = format_args!("J{}", chain_id + 1),
                    "No hash board on chain {} (J{}) — slot empty",
                    chain_id,
                    chain_id + 1,
                );
            }
        }

        if detected_chains.is_empty() {
            bail!("No hash boards detected — cannot mine without hardware");
        }

        info!(
            boards = detected_chains.len(),
            "Found {} hash board(s) — initializing PICs and ASICs",
            detected_chains.len(),
        );

        // ---- Phase 3: PIC initialization via FPGA I2C ----
        info!("--- Phase 3: PIC voltage controller init (FPGA I2C register) ---");
        let i2c = StockFpgaI2c::new(&fpga);

        let mut initialized_chains: Vec<u8> = Vec::new();

        for &chain_id in &detected_chains {
            info!(
                chain_id,
                "Initializing PIC on chain {} via FPGA IIC_COMMAND register", chain_id,
            );

            // Detect PIC state — check if in bootloader (0xCC) or app mode (0x60)
            match i2c.raw_read(chain_id) {
                Ok(0xCC) => {
                    info!(
                        chain_id,
                        "PIC on chain {} is in BOOTLOADER — sending JUMP to app mode", chain_id
                    );
                    if let Err(e) = i2c.jump_to_app(chain_id) {
                        warn!(chain_id, error = %e, "PIC JUMP failed on chain {} — trying init anyway", chain_id);
                    }
                }
                Ok(byte) => {
                    info!(
                        chain_id,
                        byte = format_args!("0x{:02X}", byte),
                        "PIC on chain {} responds 0x{:02X} (expected 0x60=app or 0xCC=bootloader)",
                        chain_id,
                        byte
                    );
                }
                Err(e) => {
                    warn!(chain_id, error = %e, "PIC raw read failed on chain {} — attempting init anyway", chain_id);
                }
            }

            // Try to read PIC version (verifies I2C communication)
            match i2c.get_pic_version(chain_id) {
                Ok(ver) => {
                    info!(
                        chain_id,
                        version = format_args!("0x{:02X}", ver),
                        "PIC version: 0x{:02X} on chain {} (0x56/0x5A/0x5E=stock, 0x03=BraiinsOS)",
                        ver,
                        chain_id,
                    );
                }
                Err(e) => {
                    warn!(
                        chain_id,
                        error = %e,
                        "PIC version read failed on chain {} — PIC may need reflash",
                        chain_id,
                    );
                }
            }

            // Set safe init voltage (9.4V)
            if let Err(e) = i2c.set_voltage(chain_id, INIT_VOLTAGE_DAC) {
                warn!(
                    chain_id,
                    error = %e,
                    "Failed to set init voltage on chain {} — may not mine",
                    chain_id,
                );
                continue;
            }

            // Enable voltage output
            if let Err(e) = i2c.enable_voltage(chain_id, true) {
                warn!(
                    chain_id,
                    error = %e,
                    "Failed to enable voltage on chain {}",
                    chain_id,
                );
                continue;
            }

            // Send initial heartbeat
            if let Err(e) = i2c.send_heartbeat(chain_id) {
                warn!(
                    chain_id,
                    error = %e,
                    "PIC heartbeat failed on chain {} — PIC watchdog may fire",
                    chain_id,
                );
            }

            info!(
                chain_id,
                "PIC on chain {} initialized — voltage enabled at ~9.4V (DAC={})",
                chain_id,
                INIT_VOLTAGE_DAC,
            );
            initialized_chains.push(chain_id);
        }

        if initialized_chains.is_empty() {
            bail!("No PICs initialized — cannot power hash boards");
        }

        // prod-readiness hunt #16: the boards are now energized. Arm the
        // crash-panic teardown so a panic from here on cuts the rail immediately
        // (panic=abort bypasses Drop; StockMiner has no Drop guard; otherwise the
        // only backstop is the ~60 s PIC watchdog). Mirrors the am2/am3 hooks.
        arm_stock_fpga_teardown(&initialized_chains);

        // `--stock-fpga` bypasses `Daemon::run()`, so it must arm the shared
        // hardware watchdog kicker itself once voltage is enabled and the PIC
        // heartbeat path has been proven. SAF-5: gate kicks on the stock mining
        // loop's status heartbeat so a live-locked loop stops feeding the SoC WDT.
        let watchdog_liveness = Arc::new(AtomicU64::new(0));
        crate::daemon::spawn_watchdog_kicker(
            &self.config.watchdog,
            self.shutdown.clone(),
            Some(watchdog_liveness.clone()),
        );

        // Wait for voltage to stabilize and ASICs to boot
        info!("Waiting 2s for DC-DC voltage ramp and ASIC boot...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // ---- Phase 4: FPGA setup for mining ----
        info!("--- Phase 4: FPGA register configuration ---");

        let passthrough = self.config.mining.passthrough;
        if passthrough {
            // Passthrough mode: DO NOT reset hash boards or modify FPGA state.
            // bmminer already configured ASICs (PLL, baud, TicketMask, open-core).
            // Resetting boards would kill the ASIC state and require full reinit.
            info!("PASSTHROUGH mode: preserving bmminer's FPGA + ASIC configuration");
            info!("Skipping hash board reset, QN_WRITE_DATA, and timeout — using bmminer's values");
        } else {
            // Full init mode: reset boards and configure from scratch
            fpga.reset_all_hashboards();
            info!("Hash boards reset via FPGA RESET_HASHBOARD register");
            tokio::time::sleep(Duration::from_secs(4)).await;

            fpga.set_qn_write_data(0x0080_800F);
            info!("QN_WRITE_DATA set to 0x0080800F (all chains enabled)");

            fpga.set_timeout(0x8000_9C40);
            info!("ASIC response timeout set (0x80009C40)");
        }

        if passthrough {
            // Read and preserve bmminer's ticket mask
            let existing_mask = fpga.read_reg(REG_TICKET_MASK);
            info!(
                ticket_mask = format_args!("0x{:02X}", existing_mask),
                "PASSTHROUGH: preserving bmminer's ticket mask 0x{:02X}", existing_mask,
            );
        } else {
            fpga.set_ticket_mask(0xFF);
            info!("Ticket mask set to 0xFF (hardware difficulty 256)");
        }

        // Flush nonce FIFO (safe in both modes — just clears stale nonces)
        fpga.write_reg(
            REG_NONCE_FIFO_INTERRUPT,
            dcentrald_hal::stock_fpga::NONCE_FIFO_FLUSH,
        );
        std::thread::sleep(Duration::from_millis(1));
        fpga.write_reg(
            REG_NONCE_FIFO_INTERRUPT,
            dcentrald_hal::stock_fpga::NONCE_IRQ_ENABLE | 0x01,
        );
        info!("Nonce FIFO flushed and IRQ enabled");

        // ---- Phase 4b: ASIC chain init via BC_WRITE_COMMAND ----
        //
        // On the stock FPGA, ASIC commands are sent via the BC_WRITE_COMMAND register
        // (0x0C0) which broadcasts to ALL chains simultaneously. This is different from
        // BraiinsOS which has per-chain CMD TX/RX FIFOs.
        //
        // The init sequence is the same:
        //   1. chain_inactive (set all chips to address 0)
        //   2. set_chip_address (assign sequential addresses)
        //   3. set_frequency (PLL configuration)
        //   4. open_core (114 dummy work items)
        //
        // ASIC init via BC_WRITE_COMMAND is NOT yet implemented.
        // Stock FPGA only works in passthrough mode: kill bmminer/bosminer first,
        // then start dcentrald. ASICs retain their state (baud, freq, addresses)
        // until power is cycled.
        //
        // BUG FIX (2026-04-11): Hard-fail on cold boot instead of silently proceeding
        // with uninitialized ASICs (which produces 0 nonces and wastes time debugging).
        info!("--- Phase 4b: ASIC chain init ---");
        warn!("Stock FPGA ASIC init via BC_WRITE_COMMAND not yet implemented. \
               Passthrough mode only — bmminer/bosminer must have initialized ASICs before dcentrald.");
        // Read HASH_COUNTING_NUMBER to check if ASICs are alive.
        // REG_RETURN_NONCE (0x010) is destructive and can consume a real nonce.
        let counting = fpga.read_reg(REG_HASH_COUNTING_NUMBER);
        if counting == 0 {
            error!(
                "HASH_COUNTING_NUMBER = 0 — no ASICs detected. \
                    Stock FPGA cold-boot init is not yet implemented. \
                    Start bmminer or bosminer first to initialize ASICs, \
                    then kill it and restart dcentrald."
            );
            return Err(anyhow::anyhow!(
                "Stock FPGA: no ASICs detected (cold boot not supported). \
                                         Pre-initialize with bmminer/bosminer first."
            ));
        }
        info!(
            "HASH_COUNTING_NUMBER = {} — ASICs appear initialized (passthrough mode)",
            counting
        );

        // Set operating voltage (9.1V)
        info!(
            "Setting operating voltage to ~9.1V (DAC={})",
            DEFAULT_VOLTAGE_DAC
        );
        for &chain in &initialized_chains {
            if let Err(e) = i2c.set_voltage(chain, DEFAULT_VOLTAGE_DAC) {
                warn!(
                    chain,
                    error = %e,
                    "Failed to set operating voltage on chain {}",
                    chain,
                );
            }
        }

        // ---- Phase 5: Start PIC heartbeat thread ----
        info!("--- Phase 5: Starting PIC heartbeat thread ---");
        let hb_chains = initialized_chains.clone();
        let hb_shutdown = self.shutdown.clone();

        // The PIC heartbeat runs on a dedicated OS thread (not tokio) to guarantee
        // timing even when the async runtime is busy with work dispatch.
        // Stock PIC watchdog is ~1 minute. We send heartbeats every 5 seconds.
        std::thread::Builder::new()
            .name("stock-pic-heartbeat".to_string())
            .spawn(move || {
                // Re-open FPGA in heartbeat thread (StockFpga is not Sync across threads
                // for mutable access — each thread needs its own mmap handle).
                let hb_fpga = match StockFpga::open() {
                    Ok(f) => f,
                    Err(e) => {
                        error!(error = %e, "Heartbeat thread: failed to open stock FPGA");
                        return;
                    }
                };
                let hb_i2c = StockFpgaI2c::new(&hb_fpga);

                info!(
                    chains = hb_chains.len(),
                    interval_ms = HEARTBEAT_INTERVAL_MS,
                    "PIC heartbeat thread running — {} chain(s), every {}ms (stock timeout ~60s)",
                    hb_chains.len(),
                    HEARTBEAT_INTERVAL_MS,
                );

                loop {
                    if hb_shutdown.is_cancelled() {
                        info!("PIC heartbeat stopping");
                        break;
                    }

                    for &chain in &hb_chains {
                        if let Err(e) = hb_i2c.send_heartbeat(chain) {
                            warn!(
                                chain,
                                error = %e,
                                "PIC heartbeat failed on chain {}",
                                chain,
                            );
                        }
                    }

                    std::thread::sleep(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
                }
            })
            .context("Failed to spawn PIC heartbeat thread")?;

        // ---- Phase 6: Open DMA buffers ----
        info!("--- Phase 6: Opening DMA buffer interface ---");
        let dma = StockFpgaDma::open()
            .context("Failed to open DMA buffer — is /dev/fpga_mem present?")?;

        // ---- Phase 7: Initialize work engine ----
        info!("--- Phase 7: Initializing DHASH accelerator + work engine ---");
        let mut work_engine = StockFpgaWorkEngine::new(&fpga, &dma);

        // Total chip count across all detected boards
        let total_chips = detected_chains.len() as u32 * CHIPS_PER_CHAIN as u32;

        if passthrough {
            // Passthrough: preserve bmminer's DHASH state (0x8100, not 0x8160)
            work_engine.init_passthrough();
        } else {
            work_engine.init(total_chips);
        }
        info!(
            total_chips,
            boards = detected_chains.len(),
            passthrough,
            "Work engine initialized — {} chips across {} board(s)",
            total_chips,
            detected_chains.len(),
        );

        // ---- Phase 8: Connect to pool and start mining ----
        info!("--- Phase 8: Connecting to mining pool ---");

        let (job_tx, mut job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
        let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
        let (status_tx, mut status_rx) =
            mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);

        let stratum_config = crate::config::build_stratum_config(
            &self.config,
            crate::config::disabled_stratum_donation_config(),
            false,
            false,
        );

        let stratum_router = dcentrald_stratum::StratumRouter::new(stratum_config);

        tokio::spawn(async move {
            stratum_router.run(job_tx, share_rx, status_tx).await;
        });

        // Stratum status logger
        let status_shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = status_shutdown.cancelled() => break,
                    Some(status) = status_rx.recv() => {
                        match status {
                            dcentrald_stratum::types::StratumStatus::StateChanged(state) => {
                                let s = match state {
                                    dcentrald_stratum::types::StratumState::Disconnected => "Disconnected",
                                    dcentrald_stratum::types::StratumState::Connecting => "Connecting",
                                    dcentrald_stratum::types::StratumState::Authorized => "Authorized",
                                    dcentrald_stratum::types::StratumState::Mining => "Mining",
                                    dcentrald_stratum::types::StratumState::Donating => "Donating",
                                    dcentrald_stratum::types::StratumState::AuthFailed => "AuthFailed",
                                };
                                info!(state = s, "Pool: {}", s);
                            }
                            dcentrald_stratum::types::StratumStatus::DifficultyChanged(d) => {
                                info!(difficulty = d, "Pool difficulty changed to {}", d);
                            }
                            dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, pool_target_difficulty, achieved_difficulty, .. } => {
                                info!(job_id = %job_id, pool_target_difficulty, achieved_difficulty, "SHARE ACCEPTED");
                            }
                            dcentrald_stratum::types::StratumStatus::ShareRejected { job_id, error_msg, .. } => {
                                warn!(job_id = %job_id, error = %error_msg, "SHARE REJECTED: {}", error_msg);
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        // ---- Phase 9: Mining loop ----
        info!("=== STOCK FPGA MINING ACTIVE ===");
        info!(
            // W1.4: the worker is the operator's wallet/payout address on Stratum
            // V1 — mask it; and strip any inline credential from the pool URL.
            pool = %dcentrald_stratum::pool_api::sanitize_pool_url(&self.config.pool.url),
            worker = %dcentrald_common::wallet_mask::mask_wallet(&self.config.pool.worker),
            boards = detected_chains.len(),
            total_chips,
            "Mining on stock Bitmain FPGA — {} board(s), {} chips",
            detected_chains.len(), total_chips,
        );

        let mut work_builder = dcentrald_stratum::WorkBuilder::new();
        let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
        let mut work_id_counter: u8 = 0;

        // Work tracking table for nonce → share matching
        let mut work_table: Vec<Option<StockWorkEntry>> = vec![None; 256];

        // Stats
        let mut total_work_dispatched: u64 = 0;
        let mut total_nonces: u64 = 0;
        let mut shares_submitted: u64 = 0;
        let mut shares_found: u64 = 0;
        let mut hw_errors: u64 = 0;
        let start_time = Instant::now();
        let mut last_hashrate_time = Instant::now();
        let mut hashrate_nonces: u64 = 0;

        // Timers
        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(1));
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(1));
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    info!("Stock mining stopping — shutdown requested");
                    break;
                }

                // Receive new job from Stratum
                Some(job) = job_rx.recv() => {
                    if job.clean_jobs {
                        info!(
                            job_id = %job.job_id,
                            "NEW BLOCK — flushing work and nonces",
                        );
                        work_table.iter_mut().for_each(|e| *e = None);
                        work_engine.signal_new_block();
                        work_builder.reset_extranonce2();
                    }
                    current_job = Some(job);
                }

                // Dispatch work to FPGA via DMA
                _ = dispatch_timer.tick() => {
                    if let Some(ref job) = current_job {
                        // NOTE: BUFFER_SPACE register reads 0 during normal mining
                        // (bmminer also shows 0). It does NOT gate work dispatch.
                        // The FPGA picks up work via JOB_DATA_READY regardless.

                        // Generate new work
                        let stratum_work = work_builder.next_work(job);

                        // Convert prev_block_hash from pool byte order to 8 x u32 words.
                        // The FPGA uses this to construct block headers internally.
                        // Pool sends prev_hash with each 4-byte word byte-swapped.
                        let mut prev_hash_words = [0u32; 8];
                        for (i, word) in prev_hash_words.iter_mut().enumerate() {
                            *word = u32::from_be_bytes([
                                job.prev_block_hash[i * 4],
                                job.prev_block_hash[i * 4 + 1],
                                job.prev_block_hash[i * 4 + 2],
                                job.prev_block_hash[i * 4 + 3],
                            ]);
                        }

                        // Build job data for DMA buffer.
                        //
                        // For VIL mode, the DMA buffer contains the coinbase template +
                        // merkle branches. The FPGA's DHASH accelerator uses these with
                        // the nonce2 counter to generate work internally.
                        //
                        // Layout: [coinbase1 | extranonce1 | extranonce2 | coinbase2 | merkle0 | merkle1 | ...]
                        let en2_len = job.extranonce2_size;
                        let en2_bytes = decode_hex_bytes(&stratum_work.extranonce2);

                        // Nonce2 offset = position of extranonce2 in the coinbase
                        let nonce2_offset = job.coinbase1.len() + job.extranonce1.len();

                        // Build coinbase portion
                        let mut job_data = Vec::new();
                        job_data.extend_from_slice(&job.coinbase1);
                        job_data.extend_from_slice(&job.extranonce1);
                        job_data.extend_from_slice(&en2_bytes);
                        job_data.extend_from_slice(&job.coinbase2);

                        // Coinbase length is everything before the merkle branches
                        let coinbase_len = job_data.len();

                        // Append merkle branches after coinbase
                        for branch in &job.merkle_branches {
                            job_data.extend_from_slice(branch);
                        }

                        // Set lengths for DHASH accelerator
                        work_engine.set_lengths(
                            coinbase_len as u16,
                            en2_len as u8,
                            nonce2_offset as u8,
                        );
                        work_engine.set_merkle_count(job.merkle_branches.len() as u32);

                        // Build header tail for share validation
                        let mut header_tail = [0u8; 12];
                        header_tail[0..4].copy_from_slice(&stratum_work.merkle4);
                        header_tail[4..8].copy_from_slice(&stratum_work.ntime.to_le_bytes());
                        header_tail[8..12].copy_from_slice(&stratum_work.nbits.to_le_bytes());

                        // Track work for nonce matching
                        work_table[work_id_counter as usize] = Some(StockWorkEntry {
                            job_id: stratum_work.job_id.clone(),
                            extranonce2: stratum_work.extranonce2.clone(),
                            ntime: stratum_work.ntime,
                            version: stratum_work.version,
                            share_target: stratum_work.share_target,
                            midstate: stratum_work.midstates[0],
                            header_tail,
                        });

                        // Dispatch via DMA + DHASH accelerator.
                        // In VIL mode, FPGA computes midstate internally from:
                        //   prev_hash (registers) + coinbase (DMA) + merkle (DMA)
                        let _fpga_job_id = work_engine.dispatch_work(
                            &job_data,
                            &prev_hash_words,
                            stratum_work.version,
                            stratum_work.ntime,
                            stratum_work.nbits,
                        );

                        work_id_counter = work_id_counter.wrapping_add(1);
                        total_work_dispatched += 1;

                        if total_work_dispatched <= 3 {
                            info!(
                                work_id = work_id_counter.wrapping_sub(1),
                                job_id = %stratum_work.job_id,
                                version = format_args!("0x{:08X}", stratum_work.version),
                                ntime = format_args!("0x{:08X}", stratum_work.ntime),
                                nbits = format_args!("0x{:08X}", stratum_work.nbits),
                                "WORK #{} dispatched to stock FPGA via DMA",
                                total_work_dispatched,
                            );
                        }
                    }
                }

                // Poll for nonces from the shared RETURN_NONCE FIFO
                _ = nonce_poll_timer.tick() => {
                    while let Some((nonce, ext)) = work_engine.read_nonce() {
                        total_nonces += 1;
                        hashrate_nonces += 1;

                        // Decode extended nonce data.
                        //
                        // Stock FPGA RETURN_NONCE_EXT format (from bmminer debug):
                        //   Bits [31:24] = chain_id (or CRC)
                        //   Bits [23:8]  = extended_work_id
                        //   Bits [7:0]   = solution_index
                        //
                        // The job_id field in the ext word maps back to the FPGA's
                        // internal job counter (set by REG_JOB_ID). We use our work_table
                        // indexed by the low byte of the job_id for simplicity.
                        let ext_work_id = ((ext >> 8) & 0xFFFF) as u16;
                        let solution_idx = (ext & 0xFF) as u8;
                        // Map back to our work_id counter (modulo 256)
                        let work_id = (ext_work_id & 0xFF) as u8;

                        if total_nonces <= 3 {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                ext = format_args!("0x{:08X}", ext),
                                work_id,
                                solution_idx,
                                "Nonce #{} from stock FPGA — ASIC chips are hashing!",
                                total_nonces,
                            );
                        }

                        // Look up work entry
                        let entry = match &work_table[work_id as usize] {
                            Some(e) => e.clone(),
                            None => {
                                debug!(work_id, "Nonce for unknown work_id — stale");
                                continue;
                            }
                        };

                        shares_found += 1;

                        // BUG FIX (2026-04-11): Enable share validation. Was bypassed and
                        // submitting ALL nonces, spamming pools with invalid shares.
                        // In VIL mode the FPGA computes its own midstate from DMA coinbase.
                        // If CPU midstate doesn't match, shares are correctly rejected here
                        // rather than wasting pool bandwidth.
                        let meets_target = dcentrald_stratum::work::validate_share(
                            &entry.midstate,
                            &entry.header_tail,
                            nonce,
                            &entry.share_target,
                        );

                        if !meets_target {
                            continue;
                        }

                        shares_submitted += 1;
                        let share = dcentrald_stratum::types::ValidShare {
                            worker_name: self.config.pool.worker.clone(),
                            job_id: entry.job_id.clone(),
                            extranonce2: entry.extranonce2.clone(),
                            ntime: format!("{:08x}", entry.ntime),
                            nonce: format!("{:08x}", nonce),
                            version_bits: None,
                            version: entry.version,
                            achieved_difficulty: None,
                        };

                        match share_tx.send(share).await {
                            Ok(()) => {
                                info!(
                                    nonce = format_args!("0x{:08X}", nonce),
                                    job_id = %entry.job_id,
                                    total_submitted = shares_submitted,
                                    "SHARE SUBMITTED to pool (#{}) — nonce 0x{:08X}",
                                    shares_submitted, nonce,
                                );
                            }
                            Err(e) => {
                                error!(error = %e, "Share channel closed");
                                break;
                            }
                        }
                    }
                }

                // Periodic hashrate calculation
                _ = hashrate_timer.tick() => {
                    watchdog_liveness.fetch_add(1, Ordering::Relaxed);
                    let elapsed = last_hashrate_time.elapsed().as_secs_f64();
                    if elapsed > 0.0 && hashrate_nonces > 0 {
                        let hashes = hashrate_nonces as f64 * HW_DIFFICULTY as f64 * 4_294_967_296.0;
                        let hashrate_ghs = hashes / elapsed / 1e9;
                        let hashrate_ths = hashrate_ghs / 1000.0;

                        info!(
                            hashrate_ths = format_args!("{:.2}", hashrate_ths),
                            hashrate_ghs = format_args!("{:.0}", hashrate_ghs),
                            nonces_5s = hashrate_nonces,
                            total_nonces,
                            total_work = total_work_dispatched,
                            shares_submitted,
                            shares_found,
                            uptime_s = start_time.elapsed().as_secs(),
                            crc_errors = fpga.read_crc_errors(),
                            "Hashrate: {:.2} TH/s ({:.0} GH/s) — {} nonces, {} shares submitted",
                            hashrate_ths, hashrate_ghs, total_nonces, shares_submitted,
                        );

                        hashrate_nonces = 0;
                        last_hashrate_time = Instant::now();
                    }
                }
            }
        }

        // ---- Shutdown ----
        info!("=== STOCK FPGA MINING SHUTDOWN ===");

        // Stop DHASH accelerator
        work_engine.stop();

        // Disable PIC voltages
        // Re-open FPGA for shutdown I2C (heartbeat thread may still hold old handle)
        if let Ok(shutdown_fpga) = StockFpga::open() {
            let shutdown_i2c = StockFpgaI2c::new(&shutdown_fpga);
            for &chain in &initialized_chains {
                if let Err(e) = shutdown_i2c.enable_voltage(chain, false) {
                    warn!(
                        chain,
                        error = %e,
                        "Failed to disable voltage on chain {} — PIC watchdog will cut power in ~60s",
                        chain,
                    );
                } else {
                    info!(chain, "Voltage disabled on chain {}", chain);
                }
            }
        } else {
            // prod-readiness hunt #5 (log-honesty): the FPGA re-open failed, so the
            // per-chain software voltage-disable above was entirely SKIPPED. Without
            // this branch the silent skip was invisible while "shutdown complete"
            // still printed below. Surface it: the PIC watchdog (~60s) is the only
            // thing cutting voltage now.
            warn!(
                "Stock FPGA shutdown: could not re-open the FPGA to disable chain \
                 voltages — software did NOT cut the rail; the PIC watchdog (~60s) \
                 is the only safety net. Do NOT warm-restart until power is off."
            );
        }

        // Post-mining cooldown fan. The chips were just mining and are still hot,
        // but the rail is already cut above, so cap at the configured home cap
        // instead of the old hardcoded ~50% blast — the PWM-30 home cap is
        // load-bearing (; cut-hash-before-noise) and
        // every sibling teardown (daemon.rs Step 7, NoPicPsuGuard, Am3BbRunSafetyGuard)
        // already honors it. fan_max_pwm is 0-100; REG_FAN_CONTROL duty (bits [23:16])
        // is 0-255, so scale. PWM_SAFETY_MAX (30) is never exceeded.
        let cooldown_pct = self
            .config
            .thermal
            .fan_max_pwm
            .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
        let duty_raw = (cooldown_pct as u32) * 255 / 100;
        fpga.write_reg(REG_FAN_CONTROL, (duty_raw << 16) | 0x14);
        info!(
            cooldown_pct,
            "Fan set to PWM {}% (home cap) via FPGA FAN_CONTROL for post-mining cooldown",
            cooldown_pct
        );

        info!("Stock FPGA mining shutdown complete");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Work tracking for stock FPGA path
// ---------------------------------------------------------------------------

/// Work entry for matching nonces back to pool jobs (stock FPGA path).
#[derive(Clone)]
struct StockWorkEntry {
    job_id: String,
    extranonce2: String,
    ntime: u32,
    version: u32,
    share_target: [u8; 32],
    midstate: [u8; 32],
    header_tail: [u8; 12],
}

// ---------------------------------------------------------------------------
// Hex decoding helper
// ---------------------------------------------------------------------------

/// Decode a hex string into bytes.
fn decode_hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| {
            if i + 2 <= hex.len() {
                u8::from_str_radix(&hex[i..i + 2], 16).ok()
            } else {
                None
            }
        })
        .collect()
}
