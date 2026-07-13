//! S19j am2 TAP MODE — bosminer owns hardware, dcentrald only dispatches FPGA work.
//!
//! This is the *defensive* alternative to `s19j_hybrid_mining`:
//!
//! | Concern         | hybrid mode           | **tap mode** (this file)        |
//! |-----------------|-----------------------|---------------------------------|
//! | PSU (APW12)     | dcentrald owns + HB   | **bosminer owns — never touch** |
//! | PIC (Pic0x89)   | dcentrald owns + HB   | **bosminer owns — never touch** |
//! | Serial ASIC init| dcentrald enumerates  | **bosminer already did it**     |
//! | FPGA CTRL/BAUD  | dcentrald writes      | **READ-ONLY — never write**     |
//! | UART relay      | dcentrald enables     | **verified enabled, never write** |
//! | WORK_TX dispatch| dcentrald             | **dcentrald (only active op)**  |
//! | WORK_RX poll    | dcentrald             | **dcentrald**                   |
//! | Stratum V1      | dcentrald             | **dcentrald**                   |
//!
//! ### Why this mode exists (Phase 6 Option 3)
//!
//! The am2 bring-up is blocked by two hardware-side regressions that would need
//! full safety review before re-enabling:
//!   - **PIC 0x86 variant** is not yet supported (
//!     bans RESET on S19j — other firmware revisions may need different framing).
//!   - **PSU framing bug** for APW121215a is unresolved — an errant frame can
//!     self-disable the PSU and crash the 15.2 V rail mid-mining.
//!
//! Tap mode sidesteps both. Operator starts bosminer (stock PIC + PSU path,
//! known-good), quiesces it via `bosminer api pause`, then dcentrald taps into
//! the already-configured FPGA. If dcentrald crashes, bosminer's heartbeats keep
//! the hardware alive. If we hit a FIFO stall we just exit — bosminer resumes.
//!
//! ### Preconditions enforced in `run()`
//!
//! 1. **FPGA CTRL must equal `0x0090_1002`** (am2 authoritative value, IP_ENABLE
//!    + MIDSTATE_CNT=1 + EXT_BAUD + clock-enable). Any other value means
//!    bosminer is not hashing — we refuse rather than push work into a dead or
//!    mis-configured chain.
//! 2. **UART relay register** at glitch-monitor +0x30 / +0x34 should read
//!    `0x2`. If not, we log a loud warning but still start — WORK_RX will just
//!    be silent. (We don't bail because some unit variants may relay via
//!    other bits we haven't probed yet.)
//!
//! ### Invariants this module MUST maintain
//!
//! - `fpga.write_ctrl(...)` — **never called** here.
//! - `fpga.set_baud(...)` / `set_work_time(...)` — **never called**.
//! - `fpga.reset_work_fifos()` / `flush_work_tx()` — **never called on startup**
//!   (could eat bosminer's in-flight work). `flush_work_rx()` on `clean_jobs` is
//!   the *only* FIFO mutation, and even that is reviewed below.
//! - No I2C, no PIC, no PSU, no serial — `dcentrald_hal::i2c`, `psu`,
//!   `serial_chain` must not appear in this file.
//!
//! ### Status
//!
//! **SKELETON ONLY (2026-04-20).** This file compiles and enforces the
//! preconditions; the Stratum + WORK_TX loop is a TODO for Phase 6 Agent B.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use dcentrald_hal::fpga_chain::DevmemFpgaChain;
// W13.B1 (2026-05-10): renamed from `uart_relay::{UartRelay, RELAY_ENABLE_VALUE}`.
// This is the Braiins-am2 diagnostic mirror, not a control surface.
use dcentrald_hal::glitch_monitor::{BraiinsGlitchMonitor, BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT};

use crate::config::DcentraldConfig;

// ---------------------------------------------------------------------------
// Mining loop constants (mirrored from s19j_hybrid_mining.rs — tap mode is a
// strict subset of hybrid's FPGA dispatch path; we intentionally duplicate
// these so the two modules stay decoupled and no cross-module refactor can
// accidentally pull in hybrid's PSU/PIC/serial side effects).
// ---------------------------------------------------------------------------

/// Hardware difficulty floor on the BM1362 FPGA nonce path.
const HW_DIFFICULTY: u64 = 256;

/// Work-history ring depth per FPGA work_id slot.
const WORK_HISTORY_PER_ID: usize = 32;

/// am2 MIDSTATE_CNT=1 → 2 midstate slots → ExtWorkId stride 2.
const JOB_ID_INCREMENT: u8 = 2;

/// FPGA work_id is 8 bits. We wrap at 0xFF
/// naturally via `u8::wrapping_add`, but keep the legacy 0x7F mask used by
/// hybrid mode to keep nonce→slot indexing identical between the two paths.
const JOB_ID_MASK: u8 = 0x7F;

/// Per-chain work_history slots. Matches hybrid exactly (128, one per
/// wrapping-masked 7-bit job id).
const WORK_HISTORY_SLOTS: usize = 128;

/// BM1362 work payload on am2: 4 header words + 2 midstate slots × 8 words.
const WORK_WORDS: usize = 20;

/// Log2 of midstate slot count on am2 (1 → 2 slots).
const MIDSTATE_CNT_LOG2: u32 = 1;

// ---------------------------------------------------------------------------
// Work entry for nonce → share lookup.
//
// Copied from `s19j_hybrid_mining::WorkEntry`. We keep this as a private copy
// inside the tap module rather than promoting it to a shared location — the
// smaller edit, and it keeps hybrid/tap coupling to exactly zero.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct WorkEntry {
    job_id: String,
    extranonce2: String,
    ntime: u32,
    nbits: u32,
    version: u32,
    share_target: [u8; 32],
    prev_block_hash: [u8; 32],
    merkle_root: [u8; 32],
    version_bits_per_midstate: Vec<Option<String>>,
    version_rolling_enabled: bool,
}

/// Rebuild the 80-byte block header for share validation.
fn tap_build_header(entry: &WorkEntry, rolled_version: u32, nonce: u32) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&rolled_version.to_le_bytes());
    header[4..36].copy_from_slice(&entry.prev_block_hash);
    header[36..68].copy_from_slice(&entry.merkle_root);
    header[68..72].copy_from_slice(&entry.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&entry.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Authoritative am2 FPGA CTRL value when bosminer is mining BM1362 at full tilt
/// (IP_ENABLE | MIDSTATE_CNT=1 | clock-enable | EXT_BAUD).
///
/// Same value as `s19j_hybrid_mining::AM2_CTRL_BM1362` — kept duplicated here
/// so the two modules stay decoupled (tap mode is a strict subset; we don't
/// want an accidental cross-module refactor to import hybrid-mode side effects).
const AM2_EXPECTED_CTRL: u32 = 0x0090_1002;

/// Default FPGA chain base when none is in config. Logical chain 1 on am2.
const DEFAULT_FPGA_CHAIN_BASE: &str = "0x43C00000";

/// Default FPGA chain id when none is in config.
const DEFAULT_FPGA_CHAIN_ID: u8 = 1;

// ---------------------------------------------------------------------------
// S19jTapMiner
// ---------------------------------------------------------------------------

/// Tap-mode miner. Owns only the FPGA devmem mapping and the Stratum client.
///
/// Construction does NOT touch hardware — the precondition checks run in
/// `run()`. This matches the hybrid-miner pattern and lets the caller defer
/// any risky MMIO until after the process has fully initialized its logging
/// and signal handlers.
pub struct S19jTapMiner {
    config: DcentraldConfig,
    shutdown: CancellationToken,
}

impl S19jTapMiner {
    /// Construct the tap miner. Infallible — the failure modes are all in
    /// `run()` where we can emit rich tracing spans.
    pub fn new(config: DcentraldConfig, shutdown: CancellationToken) -> Result<Self> {
        Ok(Self { config, shutdown })
    }

    /// Main tap-mode entry point.
    ///
    /// ```text
    /// 1. Open FPGA chain via devmem (am2 layout)
    /// 2. Verify CTRL == AM2_EXPECTED_CTRL (bail if bosminer isn't hashing)
    /// 3. Verify UART relay is enabled (warn-only)
    /// 4. [TODO: Phase 6 Agent B] Connect Stratum + run dispatch/poll loop
    /// 5. On shutdown, do NOT disable the chain (bosminer resumes after us)
    /// ```
    pub async fn run(&mut self) -> Result<()> {
        info!("=== S19j am2 TAP MODE — bosminer owns hardware state ===");

        // ────────────────────────────────────────────────────────────────────
        //  B2 (2026-05-22) — hashboard-SKU energize-refusal gate is
        // INTENTIONALLY SKIPPED in tap mode.
        //
        // Tap mode is the read-only contract: bosminer owns CTRL/PIC/PSU and
        // is already hashing when this path starts. There is NO energize
        // event downstream of this point — Step 5 explicitly does NOT
        // disable the chain on shutdown, and no `set_voltage`/
        // `enable_voltage`/`cold_boot_init` write fires from this path. The
        // matrix §7 #15 drive-half gate would have nothing to gate.
        //
        // If a future tap-mode variant adds any voltage write, the gate
        // (`dcentrald_silicon_profiles::energize_gate::gate_chains_for_energize`)
        // MUST be inserted BEFORE that write, mirroring the s19j_hybrid /
        // am3_bb / serial_mining call sites.
        // ────────────────────────────────────────────────────────────────────
        info!(
            "tap-mode: hashboard-SKU energize gate skipped — bosminer owns voltage; no dcentrald-side energize event"
        );

        // ----- Step 1: open FPGA chain (READ-ONLY contract from here on) -----
        let fpga_base_str = self
            .config
            .mining
            .fpga_chain_base
            .clone()
            .unwrap_or_else(|| DEFAULT_FPGA_CHAIN_BASE.to_string());
        let fpga_base = u64::from_str_radix(fpga_base_str.trim_start_matches("0x"), 16)
            .with_context(|| format!("Invalid fpga_chain_base hex: {}", fpga_base_str))?;
        let fpga_chain_id = self
            .config
            .mining
            .fpga_chain_id
            .unwrap_or(DEFAULT_FPGA_CHAIN_ID);

        info!(
            chain_id = fpga_chain_id,
            base = format_args!("0x{:08X}", fpga_base),
            "Opening FPGA chain via /dev/mem (am2 layout, read-mostly)"
        );
        let fpga = DevmemFpgaChain::open_am2(fpga_chain_id, fpga_base)
            .context("Failed to open FPGA chain via /dev/mem (open_am2)")?;

        // ----- Step 2: verify bosminer-initialized CTRL -----
        let ctrl = fpga.read_ctrl();
        let baud = fpga.read_baud();
        let build = fpga.read_build();
        info!(
            chain_id = fpga_chain_id,
            ctrl = format_args!("0x{:08X}", ctrl),
            baud = format_args!("0x{:02X}", baud),
            build = format_args!("0x{:08X}", build),
            "FPGA chain state sampled"
        );

        if ctrl != AM2_EXPECTED_CTRL {
            bail!(
                "Tap mode requires bosminer to have the chain initialized and mining. \
                 CTRL=0x{:08X}, expected 0x{:08X}.\n\
                 \n\
                 To use tap mode:\n\
                   1. Start bosminer and confirm the chain is hashing.\n\
                   2. Quiesce bosminer's work dispatch (e.g. `bosminer api pause`).\n\
                   3. Re-run dcentrald --tap-mode.\n\
                 \n\
                 Tap mode WILL NOT write CTRL, BAUD, WORK_TIME, PIC, or PSU. \
                 If you need dcentrald to own hardware, use --s19j-hybrid instead.",
                ctrl,
                AM2_EXPECTED_CTRL,
            );
        }
        info!(
            ctrl = format_args!("0x{:08X}", ctrl),
            "FPGA chain confirmed bosminer-initialized"
        );

        // ----- Step 3: verify UART relay is enabled (warn-only) -----
        let relay_phys_idx: u8 = match fpga_chain_id {
            1 => 2,
            4 => 3,
            other => bail!(
                "am2 supports fpga_chain_id 1 or 4 (physical 2 or 3); got {}",
                other
            ),
        };
        let relay_offset = if relay_phys_idx == 2 {
            0x30_u32
        } else {
            0x34_u32
        };
        // W13.B1 (2026-05-10): use the diagnostic-only Braiins glitch
        // monitor mirror (Braiins-am2 only) to verify bosminer-set state.
        // dcentrald tap mode never writes the BM1362 relay candidates; this
        // read is pure parity check.
        let braiins_glitch_uio: u8 = std::env::var("DCENT_BRAIINS_GLITCH_UIO")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(18);
        match BraiinsGlitchMonitor::open(braiins_glitch_uio) {
            Ok(monitor) => {
                let val = monitor.read_word(relay_offset).unwrap_or(0);
                if val == BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT {
                    info!(
                        chain_id = fpga_chain_id,
                        phys_idx = relay_phys_idx,
                        offset = format_args!("0x{:02X}", relay_offset),
                        value = format_args!("0x{:08X}", val),
                        "Braiins glitch mirror reflects bosminer-set UART_RELAY (parity check OK)"
                    );
                } else {
                    warn!(
                        chain_id = fpga_chain_id,
                        phys_idx = relay_phys_idx,
                        offset = format_args!("0x{:02X}", relay_offset),
                        value = format_args!("0x{:08X}", val),
                        expected = format_args!("0x{:08X}", BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT),
                        "Braiins glitch mirror not in expected state — WORK_RX may stay empty. \
                         Not bailing; mirror is diagnostic-only."
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to open Braiins glitch monitor (Braiins-am2 only) — skipping mirror parity check. \
                     If WORK_RX stays silent, start bosminer first, confirm it's hashing, then retry."
                );
            }
        }

        info!(
            chain_id = fpga_chain_id,
            "Tap mode preconditions passed — ready for Stratum + WORK_TX loop"
        );

        // ================================================================
        // Phase 8A: Stratum + WORK_TX / WORK_RX loop
        //
        // Mirrors `s19j_hybrid_mining.rs` Phase 10a/10b (lines 902–1226),
        // minus every PSU/PIC/serial side effect. We strictly enforce the
        // tap-mode invariants documented at the top of this file:
        //   * never write CTRL/BAUD/WORK_TIME
        //   * never flush WORK_TX (bosminer's in-flight work must survive)
        //   * flush WORK_RX only on `clean_jobs`
        //   * re-read CTRL on every dispatch tick — if it drifts from
        //     AM2_EXPECTED_CTRL we exit cleanly (no hardware teardown)
        // ================================================================
        info!("Phase 8A-a: Connecting to pool (tap-mode Stratum client)");
        let (job_tx, mut job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
        let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
        let (status_tx, mut status_rx) =
            mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);

        // Tap mode refuses version-rolling jobs (we can't reconstruct BM1362
        // rolled version bits from FPGA nonce metadata without the same full
        // machinery hybrid mode uses — and even hybrid drops those submits
        // today). Tell the router NOT to negotiate BIP 310.
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

        // Status logger — fire-and-forget; exits when the shutdown token
        // flips or the channel is closed by the router.
        let ss = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = ss.cancelled() => break,
                    Some(st) = status_rx.recv() => match st {
                        dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, pool_target_difficulty, achieved_difficulty, .. } =>
                            info!(job_id = %job_id, pool_target_difficulty, achieved_difficulty, "SHARE ACCEPTED"),
                        dcentrald_stratum::types::StratumStatus::ShareRejected { job_id, error_msg, .. } =>
                            warn!(job_id = %job_id, error = %error_msg, "SHARE REJECTED"),
                        dcentrald_stratum::types::StratumStatus::DifficultyChanged(d) =>
                            info!("Pool difficulty: {}", d),
                        dcentrald_stratum::types::StratumStatus::StateChanged(state) =>
                            info!("Pool: {:?}", state),
                        _ => {}
                    }
                }
            }
        });

        // ---- Phase 8A-b: Mining loop (FPGA work dispatch + nonce polling) ----
        info!(
            chain_id = fpga_chain_id,
            base = format_args!("0x{:08X}", fpga_base),
            phys_idx = relay_phys_idx,
            "=== TAP MINING ACTIVE — bosminer owns PSU/PIC/serial, dcentrald owns WORK_TX/WORK_RX only ==="
        );

        let mut work_builder = dcentrald_stratum::share_pipeline::WorkBuilder::new();
        let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
        // FPGA work_id is 8 bits. `u8` wraps
        // naturally at 0xFF via `wrapping_add`. We still AND with JOB_ID_MASK
        // so the slot index stays bounded by WORK_HISTORY_SLOTS.
        let mut asic_job_id: u8 = 0;
        let mut work_history: Vec<VecDeque<WorkEntry>> = (0..WORK_HISTORY_SLOTS)
            .map(|_| VecDeque::with_capacity(WORK_HISTORY_PER_ID))
            .collect();

        let mut total_work: u64 = 0;
        let mut total_nonces: u64 = 0;
        let mut shares_submitted: u64 = 0;
        let mut unsupported_share_submit_logged = false;
        let mut unsupported_job_logged = false;
        let start_time = Instant::now();
        let mut last_hr_time = Instant::now();
        let mut hr_nonces: u64 = 0;

        // Pacing: 10 ms dispatch tick (~100 works/sec cap), 5 ms nonce poll.
        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(10));
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(5));
        let mut status_timer = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    info!("Shutdown requested — tap mode exiting (hardware stays with bosminer)");
                    break;
                }

                Some(job) = job_rx.recv() => {
                    info!(
                        job_id = %job.job_id,
                        clean = job.clean_jobs,
                        version_mask = format_args!("0x{:08X}", job.version_mask),
                        "New job from pool"
                    );
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "NEW BLOCK — clearing work history + flushing WORK_RX (TX untouched, bosminer's in-flight work survives)");
                        current_job = None;
                        work_history.iter_mut().for_each(VecDeque::clear);
                        work_builder.reset_extranonce2();
                        // INVARIANT: never flush_work_tx in tap mode — it
                        // would eat bosminer's in-flight work. Only RX.
                        fpga.flush_work_rx();
                    }
                    if job.version_mask != 0 {
                        if !unsupported_job_logged {
                            unsupported_job_logged = true;
                            warn!(
                                version_mask = format_args!("0x{:08X}", job.version_mask),
                                "Tap mode cannot safely submit rolled-version shares — refusing version-rolling jobs"
                            );
                        }
                        current_job = None;
                        continue;
                    }
                    unsupported_job_logged = false;
                    work_builder.set_version_mask(0);
                    current_job = Some(job);
                }

                _ = dispatch_timer.tick() => {
                    // Gotcha #4: re-read CTRL every dispatch tick. If bosminer
                    // un-pauses (or anything else reconfigures the chain), the
                    // only race-free response is to stop pushing work and exit.
                    let ctrl_now = fpga.read_ctrl();
                    if ctrl_now != AM2_EXPECTED_CTRL {
                        warn!(
                            ctrl = format_args!("0x{:08X}", ctrl_now),
                            expected = format_args!("0x{:08X}", AM2_EXPECTED_CTRL),
                            "CTRL drifted — bosminer may have resumed. Exiting tap mode to avoid collision."
                        );
                        break;
                    }

                    if let Some(ref job) = current_job {
                        if fpga.work_tx_full() {
                            continue;
                        }

                        let work = work_builder.next_work(job);
                        let mut words = [0u32; WORK_WORDS];

                        // Word 0: Extended work_id.
                        words[0] = (asic_job_id as u32) << MIDSTATE_CNT_LOG2;
                        // Word 1: nbits
                        words[1] = work.nbits;
                        // Word 2: ntime
                        words[2] = work.ntime;
                        // Word 3: merkle_tail
                        words[3] = u32::from_le_bytes(work.merkle4);

                        // Words 4-19: 2 midstate slots (8 words each).
                        for slot in 0..(1usize << MIDSTATE_CNT_LOG2) {
                            let midstate = &work.midstates[0];
                            let base = 4 + slot * 8;
                            for i in 0..8 {
                                let word_idx = 7 - i;
                                words[base + i] = u32::from_be_bytes([
                                    midstate[word_idx * 4],
                                    midstate[word_idx * 4 + 1],
                                    midstate[word_idx * 4 + 2],
                                    midstate[word_idx * 4 + 3],
                                ]);
                            }
                        }

                        let version_bits_per_ms: Vec<Option<String>> =
                            vec![None; work.midstates.len()];

                        let slot_idx = (asic_job_id & JOB_ID_MASK) as usize;
                        let history = &mut work_history[slot_idx];
                        if history.len() >= WORK_HISTORY_PER_ID {
                            history.pop_front();
                        }
                        history.push_back(WorkEntry {
                            job_id: work.job_id.clone(),
                            extranonce2: work.extranonce2.clone(),
                            ntime: work.ntime,
                            nbits: work.nbits,
                            version: work.version,
                            share_target: work.share_target,
                            prev_block_hash: work.prev_block_hash,
                            merkle_root: work.merkle_root,
                            version_bits_per_midstate: version_bits_per_ms,
                            version_rolling_enabled: work.version_mask != 0,
                        });

                        fpga.write_work(&words);

                        // 8-bit wrap — FPGA only sees 8 bits of work_id; mask
                        // to JOB_ID_MASK (0x7F) so steady-state range matches
                        // hybrid and slot index stays bounded by WORK_HISTORY_SLOTS.
                        asic_job_id = asic_job_id.wrapping_add(JOB_ID_INCREMENT) & JOB_ID_MASK;
                        total_work += 1;

                        if total_work <= 3 {
                            info!(
                                work_id = asic_job_id.wrapping_sub(JOB_ID_INCREMENT),
                                pool_job = %work.job_id,
                                words = WORK_WORDS,
                                "WORK #{} sent ({} words to FPGA WORK_TX)",
                                total_work, WORK_WORDS,
                            );
                        }
                    }
                }

                _ = nonce_poll_timer.tick() => {
                    let mut nonces_this_poll = 0;
                    while let Some((w0, w1)) = fpga.read_nonce() {
                        nonces_this_poll += 1;
                        total_nonces += 1;
                        hr_nonces += 1;

                        let nonce = w0;
                        let ext_work_id = ((w1 >> 8) & 0xFFFF) as u16;
                        let work_id = ((ext_work_id >> MIDSTATE_CNT_LOG2) & 0x7F) as u8;
                        let solution_id = (w1 & 0xFF) as u8;

                        if total_nonces.is_multiple_of(100) {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                work_id,
                                solution_id,
                                total_nonces,
                                "Nonce counter tick"
                            );
                        } else if total_nonces <= 10 {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                work_id,
                                solution_id,
                                w1 = format_args!("0x{:08X}", w1),
                                "Nonce #{}", total_nonces,
                            );
                        }

                        let history = &work_history[(work_id & JOB_ID_MASK) as usize];
                        if history.is_empty() {
                            if total_nonces <= 50 {
                                warn!(work_id, "Stale nonce (no work history — likely bosminer's)");
                            }
                            continue;
                        }

                        let latest_entry = history
                            .back()
                            .expect("history checked non-empty")
                            .clone();

                        let missing_version_reconstruction = latest_entry.version_rolling_enabled
                            && latest_entry
                                .version_bits_per_midstate
                                .iter()
                                .all(|vb| vb.is_none());
                        if missing_version_reconstruction {
                            if !unsupported_share_submit_logged {
                                unsupported_share_submit_logged = true;
                                warn!(
                                    "Tap mode cannot reconstruct BM1362 rolled version bits — dropping share"
                                );
                            }
                            continue;
                        }

                        if let Some((entry, rolled_version, share_version_bits)) =
                            history.iter().rev().find_map(|candidate| {
                                let ms_idx = (solution_id as usize).min(
                                    candidate.version_bits_per_midstate.len().saturating_sub(1),
                                );
                                let share_version_bits = candidate
                                    .version_bits_per_midstate
                                    .get(ms_idx)
                                    .cloned()
                                    .flatten();
                                let rolled_version = match &share_version_bits {
                                    Some(vb) => {
                                        candidate.version
                                            ^ u32::from_str_radix(vb, 16).unwrap_or(0)
                                    }
                                    None => candidate.version,
                                };
                                let header = tap_build_header(candidate, rolled_version, nonce);
                                if dcentrald_stratum::share_pipeline::validate_full_header(
                                    &header,
                                    &candidate.share_target,
                                ) {
                                    Some((candidate.clone(), rolled_version, share_version_bits))
                                } else {
                                    None
                                }
                            })
                        {
                            shares_submitted += 1;
                            let vdelta = rolled_version ^ entry.version;
                            let share = dcentrald_stratum::types::ValidShare {
                                worker_name: self.config.pool.worker.clone(),
                                job_id: entry.job_id.clone(),
                                extranonce2: entry.extranonce2.clone(),
                                ntime: format!("{:08x}", entry.ntime),
                                nonce: format!("{:08x}", nonce),
                                version_bits: share_version_bits.or_else(|| {
                                    if vdelta != 0 {
                                        Some(format!("{:08x}", vdelta))
                                    } else {
                                        None
                                    }
                                }),
                                version: rolled_version,
                                achieved_difficulty: None,
                            };
                            match share_tx.send(share).await {
                                Ok(()) => {
                                    info!(
                                        nonce = format_args!("0x{:08X}", nonce),
                                        shares_submitted,
                                        "SHARE SUBMIT #{}",
                                        shares_submitted
                                    );
                                }
                                Err(e) => {
                                    error!(error = %e, "Share channel closed — Stratum task died");
                                    break;
                                }
                            }
                        }

                        if nonces_this_poll > 100 {
                            break;
                        }
                    }
                }

                _ = status_timer.tick() => {
                    // 5-second tap-mode status snapshot — CTRL/BAUD/BUILD plus
                    // FIFO occupancy and cumulative nonce count. Matches the
                    // hybrid `fpga_status` event but tagged `tap_status` so
                    // log consumers can distinguish the two paths.
                    let ctrl_now = fpga.read_ctrl();
                    let baud_now = fpga.read_baud();
                    let build_now = fpga.read_build();
                    let errs = fpga.read_error_count();
                    let tx_full = fpga.work_tx_full();
                    let rx_has = fpga.work_rx_has_data();
                    info!(
                        chain = fpga_chain_id,
                        ctrl = format_args!("0x{:08X}", ctrl_now),
                        baud = format_args!("0x{:02X}", baud_now),
                        build = format_args!("0x{:08X}", build_now),
                        tx_full,
                        rx_empty = !rx_has,
                        err_cnt = errs,
                        nonces_5s = hr_nonces,
                        total_nonces,
                        total_work,
                        shares_submitted,
                        "tap_status"
                    );

                    let elapsed = last_hr_time.elapsed().as_secs_f64();
                    if elapsed > 0.0 && hr_nonces > 0 {
                        let ths = hr_nonces as f64 * HW_DIFFICULTY as f64
                            * 4_294_967_296.0
                            / elapsed
                            / 1e12;
                        info!(
                            "{:.2} TH/s (tap) — {} nonces, {} shares, {} CRC errs, {}s uptime",
                            ths,
                            total_nonces,
                            shares_submitted,
                            errs,
                            start_time.elapsed().as_secs()
                        );
                    }
                    hr_nonces = 0;
                    last_hr_time = Instant::now();
                }
            }
        }

        // Tap mode: no hardware teardown. bosminer still owns PSU/PIC/serial
        // and its heartbeats will keep the chain alive after we exit. Just log
        // and return — do NOT call write_ctrl, flush_work_tx, set_baud, or
        // anything else that would perturb the FPGA state bosminer expects.
        info!(
            chain_id = fpga_chain_id,
            total_work,
            total_nonces,
            shares_submitted,
            uptime_s = start_time.elapsed().as_secs(),
            "=== TAP MODE SHUTDOWN (no hardware teardown — bosminer owns state) ==="
        );
        Ok(())
    }
}
