//! S19j Pro hybrid mining — serial ASIC init + FPGA work dispatch via /dev/mem.
//!
//! The S19j Pro (am2) has a hybrid architecture:
//!   - ASIC init commands go via the PL serial UARTs at 115200 -> 3.125M baud.
//!     The `a lab unit` XIL path uses `/dev/ttyS1` for the proven single-chain route
//!     and may opt into `/dev/ttyS3` only for the separate dual-chain path. CMD
//!     FIFO on am2 is DEAD - do not use.
//!   - Work dispatch normally goes through the FPGA WORK_TX FIFO; the
//!     `a lab unit`/`a lab unit` first-share lineage can instead use serial work dispatch via
//!     `DCENT_AM2_SERIAL_WORK_DISPATCH=1`.
//!   - BM1362 UART_RELAY candidate writes on serial regs `0x2C`/`0x34` are
//!     retained as a default-off lab hypothesis only. Bosminer did not write
//!     those registers in the `a lab unit` wire census, so they must not be treated as
//!     the return-path fix. The Braiins-am2-only miner-glitch-monitor window at
//!     `0x43D00000` is diagnostic/status-like; stock CV1835/AM335x/AML/S9
//!     hardware does NOT populate this IP.
//!   - PIC heartbeat via kernel I2C /dev/i2c-0 at the selected hashboard's
//!     dsPIC address (`Pic0x89` driver — RESET is banned per
//!     ).
//!   - **APW121215a PSU @ I2C 0x10 must be brought up FIRST** with a 1 Hz
//!     heartbeat loop — without the heartbeat the PSU self-disables in ~30 s
//!     and the hashboards lose their 15.2 V rail mid-enumeration
//!.
//!
//! am2 FPGA IP layout (per chain, 4 x 4 KiB):
//!   chain1-common   0x43C00000 (uio0)   CTRL/BAUD/WORK_TIME/BUILD_ID
//!   chain1-cmd-rx   0x43C01000 (uio1)   cmd FIFO (DEAD on am2, see note above)
//!   chain1-work-rx  0x43C02000 (uio2)   nonce FIFO
//!   chain1-work-tx  0x43C03000 (uio3)   work FIFO — **depth 20, never queue >20 deep**
//!   chain4 mirrors at 0x43C30000 + stride (uio12-15).
//!   glitch-monitor   0x43D00000 (uio18) Braiins-am2 only — diagnostic mirror of BM1362 reg 0x2C @ +0x30 (chain1) / +0x34 (chain4).
//!
//! Chain id → physical idx mapping (Phase 1 live probe .139):
//!   logical chain1 = phys-addr-2 (only populated boards), chain4 = phys-addr-3,
//!   phys-addr-1 is unpopulated. This module is single-chain by design — the
//!   multi-chain orchestrator owns per-chain independence.
//!
//! BM1362 work format (am2, Phase 4A): 20 x 32-bit words (4 header + 2 × 8
//! midstate words). CTRL=0x00901002, MIDSTATE_CNT=1 → 2 midstate slots.
//! S9-style 36-word / 4-slot layout does NOT apply here.
//!
//! ### Safety rules enforced here (verify on every edit)
//!   - MiscCtrl (reg 0x18) is ALWAYS triple-written with 5 ms spacing via
//!     `misc_ctrl_triple_write`. Never single-write — see
//!     .
//!   - Register writes use BM1397+ headers (0x51 broadcast / 0x41 single) via
//!     `send_write_reg_broadcast_bm1397plus` / `send_write_reg_bm1397plus`.
//!     NEVER the BM1387 SETCONFIG headers (0x58/0x48) —
//!      cost Codex-found regressions.
//!   - Serial dispatch is rate-limited during init (~50/s) to avoid UART flood
//!.
//!   - FPGA work_id is 8 bits; `asic_job_id`
//!     is a `u8` that wraps naturally.
//!   - FPGA work-tx depth is 20 on am2. `fpga.work_tx_full()` reads the
//!     STAT_TX_FULL bit which is hardware-enforced — we never push past it.
//!   - PIC SetVoltage is gated by 5 stable heartbeat ticks (same rule as PSU)
//!     —.
//!   - PSU SetVoltage calls outside of `cold_boot_sequence` are also gated —
//!     enforced by the `Apw121215a` driver itself.
//!   - Graceful shutdown MUST stop the feeder, ramp to minimum, then disarm the PSU watchdog so
//!     bosminer can cleanly restart afterwards.
//!   - Passthrough relay to bosminer needs SIGKILL (not SIGTERM) —
//!     . Owned by the caller, not this module.

use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::am2_chain_plan::{build_am2_chain_plan, Am2ChainContext};
use crate::runtime::thread_guard::RuntimeThreadGuard;
use dcentrald_asic::bm1362::{bip320_reconstruct_rolled_version, parse_bm1362_serial_nonce};
use dcentrald_asic::drivers::bm1362::{
    build_serial_work_frame, decode_pll_reg_to_freq, jig_pll1_reclock_regs, pll_lookup_extended,
    pll_ramp_sequence as bm1362_pll_ramp_sequence, BM1362_INIT_PLAN, PLL_LOCK_BIT,
};
use dcentrald_asic::drivers::MiningWork as AsicMiningWork;
use dcentrald_hal::board_control::BoardControl;
use dcentrald_hal::fpga_chain::{self, DevmemFpgaChain, FpgaChain};
use dcentrald_hal::platform::{
    bind_am2_hashboard_presence, discover_am2_controller_endpoint,
    discover_system_am2_controller_plan, Am2HashboardPresence, VoltageControllerEndpoint,
};
use dcentrald_hal::xadc::Xadc;
// W13.B1 (2026-05-10) merged glitch_monitor + uart_relay imports below.
use dcentrald_hal::i2c::{
    spawn_i2c_service_no_register_touch,
    spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation, I2cMutationLabel,
    I2cServiceHandle, I2cTransactionStep,
};
use dcentrald_hal::psu::Apw121215a;
// PsuGpioGate is now owned by `Apw121215a` and asserted automatically
// inside `cold_boot_sequence_gated` / `cold_boot_sequence_write_only`
// when `set_psu_gate_spec` has been called. The explicit hybrid-side
// assert/deassert call sites have been removed; deassertion happens
// when the `Arc<Mutex<Apw121215a>>` is dropped at function scope-end
// (after the heartbeat thread join). See
//  and `Apw121215a::Drop`.
//
// `PsuBypassGate` is the Phase-0 alternative used *instead of* `Apw121215a`
// when `[power.psu_override].enabled` is set (the "Loki bypass" — a non-smart
// PSU like an APW3 @ ~12.8 V): it owns the same `PWR_CONTROL` line, performs
// no I²C, and records the operator-declared PSU model + rail voltage. The two
// are mutually exclusive on a given unit. See
// .
use dcentrald_hal::psu_bypass_gate::PsuBypassGate;
use dcentrald_hal::serial_chain::SerialChainBackend;
// W13.B1 (2026-05-10): renamed from `uart_relay::{chain_relay_offset, UartRelay,
// RELAY_ENABLE_VALUE}`. The `0x43D000xx` window is a Braiins-am2 diagnostic
// mirror, NOT control. R6-7 keeps the BM1362 ASIC-side 0x2C/0x34 candidate
// broadcasts lab-gated until live captures confirm exact semantics.
use dcentrald_hal::glitch_monitor::{
    chain_glitch_status_offset, BraiinsGlitchMonitor, BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT,
};

use dcentrald_asic::dspic::{
    bosminer_warmup, dspic_fw86_trust_degraded_override_enabled, dspic_voltage_command_allowed,
    dspic_voltage_refusal_detail, pic0x89_firmware_from_observed_fw_byte, Pic0x89EndpointSession,
    Pic0x89Service,
};

use crate::config::DcentraldConfig;

const HW_DIFFICULTY: u64 = 256;

/// 2^32 — the per-nonce hash-space constant. A single share at difficulty `d`
/// represents `d * 2^32` expected hashes (`feedback`/serial_mining.rs parity).
const TWO_POW_32: f64 = 4_294_967_296.0;

/// AT-DASH (2026-06-14): additive, fail-closed pool-share accounting shared
/// between the Stratum status-handler task (writer) and the am2 mining loops
/// (reader). All fields are lock-free atomics so neither the status handler nor
/// the hot mining loop can ever block on the other — a publish/counter error
/// must NEVER slow or stall mining.
///
/// `achieved_difficulty_milli_sum` accumulates the *achieved* (locally proven)
/// difficulty of every accepted share scaled ×1000 (so we can sum into a u64
/// without floats), which lets the dashboard hashrate estimate use real
/// per-share difficulty rather than a flat diff-256 nonce count. When a pool
/// reports no achieved difficulty for a share, the share still counts toward
/// `accepted` but contributes the pool target difficulty as a conservative
/// floor so the estimate never reads zero while shares are landing.
#[derive(Debug, Default)]
struct Am2ShareAccounting {
    /// Total pool-accepted shares.
    accepted: AtomicU64,
    /// Total pool-rejected shares.
    rejected: AtomicU64,
    /// Sum of achieved (locally proven) difficulty across accepted shares,
    /// scaled ×1000 and rounded so it fits a `u64` with milli-precision.
    achieved_difficulty_milli_sum: AtomicU64,
    /// Unix-ms timestamp of the most recent accepted share (0 = none yet).
    last_share_at_ms: AtomicU64,
    /// Most recent pool target difficulty (×1000), for the pool tile.
    pool_difficulty_milli: AtomicU64,
}

impl Am2ShareAccounting {
    fn record_accepted(&self, achieved_difficulty: f64, pool_target_difficulty: f64) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
        // Prefer the locally proven achieved difficulty; fall back to the pool
        // target as a conservative floor so the estimate is never zero while
        // shares are being accepted.
        let credited = if achieved_difficulty.is_finite() && achieved_difficulty > 0.0 {
            achieved_difficulty
        } else if pool_target_difficulty.is_finite() && pool_target_difficulty > 0.0 {
            pool_target_difficulty
        } else {
            HW_DIFFICULTY as f64
        };
        let milli = (credited * 1000.0).round().max(0.0) as u64;
        self.achieved_difficulty_milli_sum
            .fetch_add(milli, Ordering::Relaxed);
        if pool_target_difficulty.is_finite() && pool_target_difficulty > 0.0 {
            self.pool_difficulty_milli.store(
                (pool_target_difficulty * 1000.0).round().max(0.0) as u64,
                Ordering::Relaxed,
            );
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_share_at_ms.store(now_ms, Ordering::Relaxed);
    }

    fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    fn achieved_difficulty_sum(&self) -> f64 {
        self.achieved_difficulty_milli_sum.load(Ordering::Relaxed) as f64 / 1000.0
    }

    fn last_share_at_secs(&self) -> u64 {
        self.last_share_at_ms.load(Ordering::Relaxed) / 1000
    }

    fn pool_difficulty(&self) -> f64 {
        self.pool_difficulty_milli.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Real hashrate estimate in GH/s from the accepted-share *achieved*
    /// difficulty accumulated since `since`. `H/s = Σd · 2^32 / elapsed_s`,
    /// then `/1e9` for GH/s. Returns 0 for a zero/negative window.
    fn hashrate_ghs_since(&self, since: Instant) -> f64 {
        let elapsed = since.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        self.achieved_difficulty_sum() * TWO_POW_32 / elapsed / 1e9
    }

    /// MINE-LIFE-2: REAL recent-window hashrate (GH/s) from the achieved-
    /// difficulty accrued since the previous tick snapshot `prev_achieved_sum`
    /// over `window_s` seconds. This is the HONEST "current"/5 s tile value —
    /// distinct from the cumulative lifetime average `hashrate_ghs_since`, which
    /// keeps showing a healthy number long after a chain has stalled. Returns 0
    /// for a non-positive window (the caller falls back to the cumulative figure
    /// only on the first tick, when there is no prior window yet) and clamps a
    /// negative delta (counter reset) to 0.
    fn hashrate_ghs_window(&self, prev_achieved_sum: f64, window_s: f64) -> f64 {
        if window_s <= 0.0 {
            return 0.0;
        }
        let delta = (self.achieved_difficulty_sum() - prev_achieved_sum).max(0.0);
        delta * TWO_POW_32 / window_s / 1e9
    }
}

/// MINE-LIFE-2 (eco-stable): rolling horizon (seconds) for the "current"/5 s
/// hashrate tile. The per-tick achieved-difficulty delta is ZERO on the many
/// 5 s ticks BETWEEN a sparse eco unit's accepted shares (a healthy ~1 TH/s @
/// ~50 MHz unit lands a share only every ~10-60 s), so a single-tick 5 s window
/// flickered to 0 GH/s while the unit was mining normally — the very distrust
/// the truthfulness work exists to prevent. A wide rolling window almost always
/// contains a recent share, so the tile stays a STABLE non-zero value; it is
/// short enough to still decay to ~0 within ~a minute of a genuine sustained
/// stall. Telemetry only — never a control input.
const AM2_RECENT_HASHRATE_WINDOW_S: f64 = 60.0;

/// MINE-LIFE-2 (eco-stable): pick the rolling recent-window baseline for the
/// "current"/5 s hashrate tile from retained PRIOR hashrate-tick snapshots.
///
/// `history` is `(age_s, achieved_difficulty_sum)` per prior tick, oldest first
/// (front = largest age); `horizon_s` is the rolling window length. Returns
/// `(span_s, baseline_sum)` to hand to `Am2ShareAccounting::hashrate_ghs_window`
/// (which still credits ONLY the achieved-difficulty delta since `baseline_sum`,
/// so this is a REAL measured value over a wider window — NOT the
/// cumulative-since-boot average that was the original bug). The baseline is the
/// NEWEST prior snapshot still at least `horizon_s` old (so the window spans the
/// full horizon once that much history exists), or the OLDEST retained snapshot
/// before that much history accrues. `None` only when `history` is empty (the
/// first tick — no prior window yet) so the caller can show the cumulative
/// figure exactly once.
fn am2_rolling_window_baseline(history: &[(f64, f64)], horizon_s: f64) -> Option<(f64, f64)> {
    let mut baseline: Option<(f64, f64)> = None;
    for &(age_s, sum) in history {
        if age_s >= horizon_s {
            baseline = Some((age_s, sum));
        } else {
            // `history` is oldest-first (ages descending), so once a snapshot is
            // younger than the horizon every later one is too — stop scanning.
            break;
        }
    }
    baseline.or_else(|| history.first().copied())
}

/// S19j Pro hashboard dsPIC I2C addresses (7-bit), indexed by ttyS slot.
const S19_DSPIC_ADDRS: [u8; 3] = [0x20, 0x21, 0x22];

/// am2 S19j Pro chip-rail target. This is intentionally independent from
/// `[power.psu_override].voltage_v`, which is the PSU output rail.
const S19J_HYBRID_CHIP_RAIL_TARGET_MV: u16 = 13_700;

/// am2 `a lab unit` open-core experiment ceiling (mV). Mirrors the AM3-BB
/// `AM3_BB_DSPIC_MAX_VOLTAGE_MV` / `DSPIC_MAX_VOLTAGE_MV` 15140 mV DAC-span
/// ceiling. `s19j_open_core_mv()` clamps the operator-supplied
/// `DCENT_AM2_OPEN_CORE_MV` into `[S19J_HYBRID_CHIP_RAIL_TARGET_MV,
/// S19J_OPEN_CORE_MAX_MV]`. NOTE: the dsPIC service still enforces the
/// `DSPIC_VOLTAGE_HARD_CAP_MV` 14500 mV input clamp unless
/// `DCENT_AM2_ALLOW_LAB_OVERVOLT=1` lifts it — so anything above 14500 is
/// SILENTLY clamped down at the rail boundary without the lab over-volt flag.
const S19J_OPEN_CORE_MAX_MV: u16 = 15_140;

const AM2_VOLTAGE_ENABLE_ALL_ACTIVE_PICS_ENV: &str = "DCENT_AM2_VOLTAGE_ENABLE_ALL_ACTIVE_PICS";

/// One admitted rail target for the whole open-core experiment lifetime.
///
/// Resolving this once before each hashboard rail mutation prevents environment
/// changes or duplicated gate logic from selecting a different demotion policy
/// after the elevated rail has already been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenCoreRailPlan {
    steady_mv: u16,
    energization_mv: u16,
}

impl OpenCoreRailPlan {
    const fn requires_demotion(self) -> bool {
        self.energization_mv > self.steady_mv
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum OpenCoreRailAdmissionError {
    #[error(
        "open-core elevation cannot be combined with all-active-PIC voltage enable; one selected rail owner must remain authoritative"
    )]
    AllActiveVoltageEnable,
    #[error(
        "open-core target {requested_mv} mV must be strictly above steady target {steady_mv} mV"
    )]
    TargetNotElevated { requested_mv: u16, steady_mv: u16 },
}

const fn admit_open_core_rail_plan(
    open_core_active: bool,
    all_active_voltage_enable: bool,
    steady_mv: u16,
    requested_mv: u16,
) -> std::result::Result<OpenCoreRailPlan, OpenCoreRailAdmissionError> {
    if !open_core_active {
        return Ok(OpenCoreRailPlan {
            steady_mv,
            energization_mv: steady_mv,
        });
    }
    if all_active_voltage_enable {
        return Err(OpenCoreRailAdmissionError::AllActiveVoltageEnable);
    }
    if requested_mv <= steady_mv {
        return Err(OpenCoreRailAdmissionError::TargetNotElevated {
            requested_mv,
            steady_mv,
        });
    }
    Ok(OpenCoreRailPlan {
        steady_mv,
        energization_mv: requested_mv,
    })
}

// Distinct clamp conditions intentionally share the `1` result; keeping them
// as separate arms documents each reason the budget floors to one attempt.
#[allow(clippy::if_same_then_else)]
const fn am2_hb_reset_attempt_budget(
    faithful_retry_enabled: bool,
    requested_attempts: u8,
    elevated_rail: bool,
) -> u8 {
    if elevated_rail || !faithful_retry_enabled {
        1
    } else if requested_attempts < 1 {
        1
    } else if requested_attempts > 20 {
        20
    } else {
        requested_attempts
    }
}

pub(crate) fn psu_override_active(psu_override: Option<&crate::config::PsuOverride>) -> bool {
    psu_override.is_some_and(|o| o.enabled)
}

fn am2_zero_psu_bytes_enabled() -> bool {
    am2_env_flag("DCENT_AM2_ZERO_PSU_BYTES")
}

fn am2_diag_stop_after_psu_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DIAG_STOP_AFTER_PSU")
}

fn am2_diag_stop_after_dspic_enable_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DIAG_STOP_AFTER_DSPIC_ENABLE")
}

fn am2_diag_stop_after_bm1362_enum_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM")
}

fn s19j_hybrid_chip_rail_target_mv(_psu_override: Option<&crate::config::PsuOverride>) -> u16 {
    S19J_HYBRID_CHIP_RAIL_TARGET_MV
}

fn parse_gpio_number_spec(spec: Option<&str>) -> Option<u32> {
    let raw = spec.map(str::trim).unwrap_or("");
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("PWR_CONTROL")
        || raw.eq_ignore_ascii_case("label:PWR_CONTROL")
    {
        return Some(dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO);
    }
    let gpio = raw
        .strip_prefix("gpio:")
        .map(str::trim)
        .unwrap_or(raw)
        .trim();
    if gpio.bytes().all(|b| b.is_ascii_digit()) {
        gpio.parse::<u32>().ok()
    } else {
        None
    }
}

pub fn force_pwr_control_low_checked(spec: Option<&str>, reason: &str) -> Result<()> {
    let Some(gpio) = parse_gpio_number_spec(spec) else {
        anyhow::bail!(
            "AM2 home hard-stop could not parse pwr_control_gpio {:?}; PWR_CONTROL not forced low",
            spec
        );
    };
    let active_low = am2_pwr_control_active_low_for_gpio(gpio)?;

    let gpio_dir = format!("/sys/class/gpio/gpio{}", gpio);
    if !std::path::Path::new(&gpio_dir).exists() {
        fs::write("/sys/class/gpio/export", gpio.to_string())
            .with_context(|| format!("failed to export AM2 PWR_CONTROL gpio{}", gpio))?;
        std::thread::sleep(Duration::from_millis(20));
    }

    let direction_path = format!("{}/direction", gpio_dir);
    let value_path = format!("{}/value", gpio_dir);
    if let Err(e) = fs::write(&direction_path, "out") {
        // prod-readiness hunt #8 (log-honesty): include the captured errno (the
        // value-write branch below already does). Without it a direction-set
        // failure that later breaks the cut-power value-write gives the operator
        // no diagnostic, and `e` was an unused binding (rustc warning).
        warn!(
            gpio,
            reason,
            error = %e,
            "AM2 home hard-stop failed to set PWR_CONTROL direction"
        );
    }
    // 2026-06-07 (.25 active-LOW PWR_CONTROL): writing "0" is the CUT-POWER
    // action only on an active-HIGH line. On `a lab unit` gpio907 is ACTIVE-LOW
    // (RE-018 true-cold strace: "0"=rail ON, "1"=rail OFF), so the hard-stop
    // must write "1" to actually de-energize — writing "0" here would ENERGIZE
    // the rail during a safety teardown on the operator's HOME unit. Gate on
    // DCENT_AM2_PWR_CONTROL_ACTIVE_LOW (default-OFF → fleet unchanged).
    let off_value = if active_low { "1" } else { "0" };
    fs::write(&value_path, off_value).with_context(|| {
        format!(
            "AM2 home hard-stop failed to force gpio{} PWR_CONTROL to the OFF level",
            gpio
        )
    })?;
    let readback = fs::read_to_string(&value_path)
        .map(|v| v.trim().to_string())
        .with_context(|| "AM2 home hard-stop could not read back PWR_CONTROL after OFF write")?;
    if readback != off_value {
        anyhow::bail!(
            "AM2 home hard-stop PWR_CONTROL readback mismatch after OFF write: gpio{} active_low={} expected={} readback={}",
            gpio,
            active_low,
            off_value,
            readback
        );
    }
    info!(
        gpio,
        reason,
        active_low,
        off_value,
        readback,
        "AM2 home hard-stop forced PWR_CONTROL to the OFF level (readback confirmed)"
    );
    Ok(())
}

fn am2_pwr_control_active_low_for_gpio(gpio: u32) -> Result<bool> {
    let active_low = am2_env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_LOW");
    let active_high = am2_env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH");
    if gpio == dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO && active_low == active_high {
        anyhow::bail!(
            "AM2 PWR_CONTROL gpio{} polarity unknown or conflicting; set exactly one of DCENT_AM2_PWR_CONTROL_ACTIVE_LOW=1 or DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH=1",
            gpio
        );
    }
    Ok(active_low)
}

fn am2_wave56_override_runtime_preflight(config: &DcentraldConfig) -> Result<()> {
    if !am2_env_flag(AM2_XIL25_FINGERPRINT_OVERRIDE_ENV) {
        return Ok(());
    }
    if !am2_xil_25_fingerprint_matches() {
        anyhow::bail!(
            "Wave56 .25 override requested but runtime package fingerprint did not match zynq-bm3-am2/am2-s19j-or-xil/Loki"
        );
    }
    let active_low = am2_env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_LOW");
    let active_high = am2_env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH");
    if active_low == active_high {
        anyhow::bail!(
            "Wave56 .25 override requires exactly one PWR_CONTROL polarity marker: DCENT_AM2_PWR_CONTROL_ACTIVE_LOW=1 or DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH=1"
        );
    }
    // P2 (2026-06-13): require the NUMERIC gpio form, not the `PWR_CONTROL`
    // label. `parse_gpio_number_spec` maps the label to 907 too, so the old
    // check passed a label-form config that PsuGpioGate would (pre-P2) DT-resolve
    // to a different line. The resolver is now reconciled
    // (psu_gpio_gate::resolve_gpio), but a  proof run must still pin the
    // unambiguous numeric form so assert/polarity/readback/teardown provably
    // agree on gpio907.
    let pwr_spec = config
        .psu
        .pwr_control_gpio
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    let pwr_is_numeric_907 = pwr_spec == "907" || pwr_spec.eq_ignore_ascii_case("gpio:907");
    if !pwr_is_numeric_907
        || parse_gpio_number_spec(config.psu.pwr_control_gpio.as_deref())
            != Some(dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO)
    {
        anyhow::bail!(
            "Wave56 .25 override requires [psu].pwr_control_gpio = \"gpio:907\" (numeric form, not the PWR_CONTROL label)"
        );
    }
    if config.donation.enabled {
        anyhow::bail!(
            "Wave56 .25 proof run requires [donation].enabled = false so accepted shares are unambiguous"
        );
    }
    if !config.pool.worker.to_ascii_lowercase().contains("xil") {
        anyhow::bail!("Wave56 .25 proof run requires [pool].worker to include xil for attribution");
    }
    if config.pool.failover1.is_some()
        || config.pool.failover2.is_some()
        || config
            .pool
            .routing_mode
            .eq_ignore_ascii_case("weighted_split")
        || config.pool.split_bps.is_some()
        || config.pool.smart_failover_enabled
    {
        anyhow::bail!(
            "Wave56 .25 proof run forbids failover/split/smart routing so shares cannot land on another worker"
        );
    }
    // D1-1 (2026-06-13): [power.psu_override].enabled is a shell-only gate in the
    // launcher; without it here a direct daemon launch with the override env but
    // psu_override DISABLED falls through to the default smart-APW gpio-bitbang
    // engagement and silently abandons the ZERO_PSU_BYTES posture on the Loki
    // home unit (the ZERO_PSU_BYTES + no_smbus_peer guards live ONLY inside the
    // psu_override_active branch).
    if !psu_override_active(config.power.psu_override.as_ref()) {
        anyhow::bail!(
            "Wave56 .25 proof run requires [power.psu_override].enabled = true (Loki dumb-PSU / ZERO_PSU_BYTES posture); disabled routes to the smart-APW gpio-bitbang path"
        );
    }
    // D1-6: pin the declared Loki rail identity (APW3 / 12.8 V) and a real pool so
    // the proof run's rail accounting + share attribution are unambiguous
    // (launcher-only before). psu_override is Some+enabled per the D1-1 check.
    if let Some(ov) = config.power.psu_override.as_ref() {
        if !ov.model.eq_ignore_ascii_case("APW3") {
            anyhow::bail!(
                "Wave56 .25 proof run requires [power.psu_override].model = \"APW3\" (declared Loki rail), got {:?}",
                ov.model
            );
        }
        if (ov.voltage_v - 12.8).abs() > 0.05 {
            anyhow::bail!(
                "Wave56 .25 proof run requires [power.psu_override].voltage_v = 12.8 (APW3 Loki rail), got {}",
                ov.voltage_v
            );
        }
    }
    if config.pool.url.trim().is_empty() {
        anyhow::bail!(
            "Wave56 .25 proof run requires a real [pool].url (a standalone proof needs a live pool to attribute accepted shares)"
        );
    }
    // D1-4 (2026-06-13): serial_device is a shell-only gate, but the runtime
    // resolver defaults to /dev/ttyS2. The only chain that has EVER enumerated on
    // `a lab unit` is /dev/ttyS1 (). A direct launch with serial_device
    // unset/wrong selects ttyS2/ttyS3 and is a guaranteed enum=0 that looks like
    // chip-wake failure but is transport-selection error — the exact ambiguity a
    // decisive proof run must eliminate. `.first()` mirrors the runtime's own
    // selection (resolved_serial_devices(..).first()).
    if config
        .mining
        .resolved_serial_devices("/dev/ttyS2")
        .first()
        .map(String::as_str)
        != Some("/dev/ttyS1")
    {
        anyhow::bail!(
            "Wave56 .25 proof run requires [mining].serial_device = /dev/ttyS1 (the only chain that has enumerated on .25)"
        );
    }
    Ok(())
}

pub fn force_pwr_control_low(spec: Option<&str>, reason: &str) {
    if let Err(e) = force_pwr_control_low_checked(spec, reason) {
        warn!(
            spec = ?spec,
            reason,
            error = %e,
            "AM2 home hard-stop failed to force PWR_CONTROL to the OFF level"
        );
    }
}

fn open_am2_fan_controller(
    reason: &str,
) -> Option<(
    dcentrald_hal::fan::FanUioDiscovery,
    dcentrald_hal::fan::FanController,
)> {
    match dcentrald_hal::fan::FanController::open_discovered() {
        Ok((discovery, fan)) => {
            if !matches!(discovery.variant, dcentrald_hal::fan::FanVariant::Am2Uio16) {
                warn!(
                    reason,
                    uio = discovery.uio_number,
                    variant = ?discovery.variant,
                    "AM2 fan helper found non-AM2 fan-control layout"
                );
                return None;
            }
            Some((discovery, fan))
        }
        Err(e) => {
            warn!(
                reason,
                error = %e,
                "AM2 fan helper could not discover/open fan-control UIO"
            );
            None
        }
    }
}

/// Truthful AM2 fan tach for the dashboard: read-only fan-controller open with
/// the `Am2Uio16` variant (all 4 tach channels) WITHOUT the board-control C52
/// write. Uses `new_with_variant` (NOT `open_with_variant`) so it only mmaps +
/// reads — the C52 fan mode is already set at cold boot, so no board-control
/// poke is needed. Returns the max per-fan rpm, or 0 if the fan UIO can't be
/// read. This replaces a hardcoded `rpm: 0` in the standalone status publish
/// (which the dashboard misreads as a STOPPED fan while mining, a false alarm).
/// Telemetry ONLY — it never writes a PWM/board-control register (no fan command).
fn am2_read_fan_rpm_max() -> u32 {
    let Some(disc) = dcentrald_hal::fan::discover_fan_uio() else {
        return 0;
    };
    let Ok(regs) = dcentrald_hal::uio::UioDevice::open(disc.uio_number) else {
        return 0;
    };
    let fan = dcentrald_hal::fan::FanController::new_with_variant(
        regs,
        dcentrald_hal::fan::FanVariant::Am2Uio16,
    );
    fan.get_per_fan_rpm()
        .iter()
        .map(|(_, rpm)| *rpm)
        .max()
        .unwrap_or(0)
}

fn force_am2_fans_to_configured_cap(fan_max_pwm: u8, reason: &str) {
    // Policy chokepoint: FanCommand intersects profile max ∩ home safety (30).
    let cap = dcentrald_common::FanCommand::emergency_cap(fan_max_pwm)
        .effective_pwm()
        .min(dcentrald_hal::fan::PWM_MAX);
    match open_am2_fan_controller(reason) {
        Some((discovery, fan)) => {
            fan.set_speed(cap);
            let (commanded_pwm0, commanded_pwm1) = fan.get_speed_pwm_channels();
            let commanded_pwm = commanded_pwm0.max(commanded_pwm1);
            let max_rpm = fan
                .get_per_fan_rpm()
                .iter()
                .map(|(_, rpm)| *rpm)
                .max()
                .unwrap_or(0);
            info!(
                pwm = cap,
                uio = discovery.uio_number,
                commanded_pwm,
                commanded_pwm0,
                commanded_pwm1,
                max_rpm,
                reason,
                "AM2 home hard-stop commanded fan PWM to configured cap"
            );
        }
        None => warn!(
            pwm = cap,
            reason, "AM2 home hard-stop could not open fan controller"
        ),
    }
}

fn force_am2_home_hard_stop(config: &DcentraldConfig, reason: &str) {
    // Order matters for home use: remove hashboard power first, then leave the
    // fan command at idle. PWM 30 is for uncertain powered/thermal state; once
    // PWR_CONTROL is low, the last writer should not be the louder cap.
    force_pwr_control_low(config.psu.pwr_control_gpio.as_deref(), reason);
    force_am2_fans_to_quiet_idle(
        config.thermal.fan_idle_pwm,
        config.thermal.fan_max_pwm,
        reason,
    );
}

fn force_am2_thermal_hard_stop(config: &DcentraldConfig, reason: &str) {
    // Measured thermal failure is different from a normal park/no-nonce stop:
    // cut hashboard power first, then keep the home safety cooling cap instead
    // of immediately dropping to idle.
    // Policy order is `PowerCut::home_thermal_hard_stop_action` (cut then fans).
    let _policy =
        dcentrald_common::PowerCut::home_thermal_hard_stop_action(config.thermal.fan_max_pwm);
    debug_assert!(dcentrald_common::power_precedes_fan_raise(&_policy.steps()));
    force_pwr_control_low(config.psu.pwr_control_gpio.as_deref(), reason);
    force_am2_fans_to_configured_cap(config.thermal.fan_max_pwm, reason);
}

/// Pure PWM computation for the am2 low-idle command setpoint.
///
/// The result is ONLY ever driven DOWN: it is the minimum of the operator's
/// configured idle duty, the fan ceiling, and the absolute home safety max
/// (`PWM_SAFETY_MAX = 30`). It can never raise the fan above 30 regardless of
/// config. Kept as a free function so the clamp arithmetic is unit-testable
/// without opening a UIO device. See
/// .
fn compute_quiet_idle_pwm(idle_pwm: u8, fan_max_pwm: u8) -> u8 {
    dcentrald_common::FanCommand {
        profile_max_pwm: fan_max_pwm.min(dcentrald_hal::fan::PWM_MAX),
        requested_pwm: idle_pwm,
        apply_home_safety_cap: true,
    }
    .effective_pwm()
}

/// am2 low-idle fan setter for the management-only park paths.
///
/// This is the SAME command mechanism as the hard-stop's
/// `force_am2_fans_to_configured_cap` — the uio16-mmap `FanController`
/// (: devmem is a
/// no-op on the am2 UIO-bound fan IP) — just with a LOWER setpoint for a
/// parked, non-mining unit. It does NOT replace, reorder, or weaken the
/// hard-stop (which keeps its own `fan_max_pwm`/30 setpoint); it is a
/// separate, additional, strictly-lower park setpoint.
///
/// PWM is only ever driven DOWN (see `compute_quiet_idle_pwm`). A
/// `FanController` open failure is tolerated with a `tracing::warn!` — the
/// park must not panic/abort just because the fan device couldn't be opened
/// (the PIC/PSU watchdog and the run-scope hard-stop guard remain the safety
/// nets; this is a low-PWM command on an already-safe-off unit, not proof of
/// acoustic quiet without tach/RPM/operator confirmation).
pub fn force_am2_fans_to_quiet_idle(idle_pwm: u8, fan_max_pwm: u8, reason: &str) {
    let pwm = compute_quiet_idle_pwm(idle_pwm, fan_max_pwm);
    match open_am2_fan_controller(reason) {
        Some((discovery, fan)) => {
            fan.set_speed(pwm);
            let (commanded_pwm0, commanded_pwm1) = fan.get_speed_pwm_channels();
            let commanded_pwm = commanded_pwm0.max(commanded_pwm1);
            let max_rpm = fan
                .get_per_fan_rpm()
                .iter()
                .map(|(_, rpm)| *rpm)
                .max()
                .unwrap_or(0);
            info!(
                pwm,
                uio = discovery.uio_number,
                commanded_pwm,
                commanded_pwm0,
                commanded_pwm1,
                max_rpm,
                idle_pwm,
                fan_max_pwm,
                reason,
                "AM2 low-idle: parked non-mining unit fans commanded to idle PWM (uio16 mmap; RPM is separate proof)"
            );
        }
        None => warn!(
            pwm,
            reason,
            "AM2 low-idle: could not open fan controller — fans left at \
             current state (PIC/PSU watchdog + hard-stop guard remain the \
             safety nets; park continues)"
        ),
    }
}

/// SAFE-TEARDOWN — Phase 3A AC-cycle-elimination. **Default-ON** (CE-010 /
/// RE-003 / PERF-008); opt OUT with `DCENT_AM2_SAFE_TEARDOWN=0`.
///
/// The clean-stop teardown walks chip rail to floor + pulses HBx_RESET + waits
/// 1.5 s for rail decay BEFORE deasserting PWR_CONTROL — leaving the chain in a
/// state the next `dcentrald` launch can re-init in software, eliminating the
/// operator AC-cycle-per-attempt blocker.
///
/// This is a *strictly-safer-on-failure* promotion: the sequence only ADDS a
/// rail walk-down + reset-drain + decay window in front of the existing
/// `disable_voltage`, and never raises fans (the run-scope hard-stop guard
/// still owns PWR_CONTROL deassert + the PWM-30 cap). It does NOT touch the
/// cold-boot/mining drive path, so it does not make the daemon push the chip
/// harder — it only makes a SHUTDOWN cleaner. Per the live-hardware-default
/// principle, strictly-safer-on-failure de-energization MAY default ON.
///
/// Override precedence: an explicit `DCENT_AM2_SAFE_TEARDOWN=0` (off) wins over
/// everything (lets an operator restore the legacy minimal teardown if a
/// regression is ever suspected). Otherwise (unset, `=1`, or any non-falsey
/// value) the safe teardown runs.
///
/// F4 (AC-cycle root cause;
/// the F3 citation in this file was historically wrong — Wave D RE-CORPUS-001
/// closure fixed it to F4) +
/// F2 and Phase 3A of the multi-phase plan.
fn am2_safe_teardown_enabled() -> bool {
    // Default-ON: only an explicit falsey value disables it.
    !am2_env_flag_off("DCENT_AM2_SAFE_TEARDOWN")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pic0x89CleanStopOwnerPolicy {
    Endpoint,
    LegacyCompatibility,
    RefuseMissingExactEndpoint,
}

const fn pic0x89_clean_stop_owner_policy(
    exact_endpoint_required: bool,
    endpoint_session_present: bool,
) -> Pic0x89CleanStopOwnerPolicy {
    match (exact_endpoint_required, endpoint_session_present) {
        (_, true) => Pic0x89CleanStopOwnerPolicy::Endpoint,
        (true, false) => Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint,
        (false, false) => Pic0x89CleanStopOwnerPolicy::LegacyCompatibility,
    }
}

/// Phase 3A — graceful chain teardown so the next `dcentrald` launch can
/// re-init the chain in software, eliminating the operator AC-cycle-per-
/// attempt blocker.
///
/// EE root cause ( F4 —
/// "AC-cycle-per-attempt root cause: electrical state retained across
/// software teardown"; cited as F3 pre-Wave-D, corrected per RE-CORPUS-001):
/// the prior teardown skipped the voltage walk + HBx_RESET drain, so the
/// BM1362 chip-rail decoupling caps held residual charge past PWR_CONTROL
/// deassert. Next-launch enumeration then saw the chips in a partial-bias
/// state with addressing dirty (the empirical "needs another AC cycle"
/// failure mode from the 2026-05-15 .109 session).
///
/// Sequence:
///   1. SetVoltage(11500 mV) — walk rail to floor (chips coast down).
///   2. Sleep 200 ms — let the dsPIC track to the new setpoint.
///   3. HBx_RESET pulse on slots 0..=3 (20 ms LOW each) — drain rail caps.
///   4. Sleep 1500 ms — rail-decay window per EE.
///   5. PIC disable_voltage — formal shutdown.
///
/// Caller is responsible for the final PWR_CONTROL deassert + fan cap —
/// handled by `Am2HomeHardStopGuard::Drop` (run-scope) or by an explicit
/// `force_am2_home_hard_stop` call on the fail-closed path. Hardware-operation
/// errors remain best-effort because the run-scope hard-stop guard is the final
/// safety net. Losing a required exact endpoint session returns an error and
/// never reconstructs raw address/firmware authority.
fn am2_safe_teardown_sequence(
    endpoint_session: Option<&mut Pic0x89EndpointSession>,
    exact_endpoint_required: bool,
    pic_service: Option<&I2cServiceHandle>,
    pic_addr: u8,
    pic_fw: Option<u8>,
    reason: &str,
) -> Result<()> {
    info!(reason, "Phase 3A: am2 safe-teardown sequence starting");

    let owner_policy =
        pic0x89_clean_stop_owner_policy(exact_endpoint_required, endpoint_session.is_some());
    let mut endpoint_session = endpoint_session;
    let mut legacy_controller = match owner_policy {
        Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => match (pic_service, pic_fw) {
            (Some(service), Some(fw)) => Some(Pic0x89Service::new_with_fw(
                service.clone(),
                pic_addr,
                Some(fw),
            )),
            _ => None,
        },
        Pic0x89CleanStopOwnerPolicy::Endpoint
        | Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => None,
    };

    if owner_policy == Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint {
        error!(
            addr = format_args!("0x{:02X}", pic_addr),
            reason,
            "exact AM2 controller identity was established but its endpoint session is missing at clean stop; refusing raw address/firmware reconstruction"
        );
    }

    // Step 1: walk chip rail to floor so the chips coast down rather than
    // dropping voltage in one step (which leaves more charge on the caps).
    let step1_result = match owner_policy {
        Pic0x89CleanStopOwnerPolicy::Endpoint => endpoint_session
            .as_deref_mut()
            .map(|session| session.controller_mut().set_voltage(11500)),
        Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => legacy_controller
            .as_mut()
            .map(|controller| controller.set_voltage(11500)),
        Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => None,
    };
    if let Some(result) = step1_result {
        match result {
            Ok(()) => info!(
                addr = format_args!("0x{:02X}", pic_addr),
                "teardown step 1/5: walked chip rail to 11500 mV floor"
            ),
            Err(e) => warn!(
                error = %e,
                addr = format_args!("0x{:02X}", pic_addr),
                "teardown step 1/5: set_voltage(11500) failed; chips will see a sharp drop instead of a walk"
            ),
        }
        // Step 2: small wait for the dsPIC to track to the new setpoint
        // before we yank reset (otherwise the chip starts the reset while
        // still under full rail, which doesn't drain caps).
        std::thread::sleep(Duration::from_millis(200));
    } else {
        warn!("teardown step 1/5: no PIC service available — skipping voltage walk");
    }

    // Step 3: HBx_RESET pulse all 4 slots — drain rail caps via the chip's
    // internal reset clamps. Slot mapping isn't guaranteed; pulse all 4
    // (unpopulated slots no-op, populated ones drain).
    // CE-003: discover the board-control UIO by name instead of hardcoding 17.
    match BoardControl::open(board_control_uio_number()) {
        Ok(bc) => {
            for slot in 0u8..=3 {
                match bc.pulse_reset(slot) {
                    Ok(()) => info!(slot, "teardown step 3/5: HBx_RESET pulse on slot {}", slot),
                    Err(e) => warn!(
                        error = %e,
                        slot,
                        "teardown step 3/5: HBx_RESET pulse failed on slot {} (slot may be unpopulated)",
                        slot
                    ),
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Err(e) => warn!(
            error = %e,
            "teardown step 3/5: BoardControl::open(uio17) failed — chain caps may not drain"
        ),
    }

    // Step 4: rail-decay window. 1.5 s is the EE-recommended floor; the
    // BM1362 chip-rail decoupling caps are typically <1 s at this current
    // but we pad for safety.
    info!("teardown step 4/5: 1500 ms rail-decay window starting");
    std::thread::sleep(Duration::from_millis(1500));

    // Step 5: formal disable_voltage. By this point chips are coasted +
    // caps drained; this is the dsPIC bookkeeping more than a power cut.
    let step5_result = match owner_policy {
        Pic0x89CleanStopOwnerPolicy::Endpoint => {
            endpoint_session.map(|session| session.controller_mut().disable_voltage())
        }
        Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => legacy_controller
            .as_mut()
            .map(|controller| controller.disable_voltage()),
        Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => None,
    };
    if let Some(result) = step5_result {
        match result {
            Ok(()) => info!(
                addr = format_args!("0x{:02X}", pic_addr),
                "teardown step 5/5: PIC voltage disabled"
            ),
            Err(e) => warn!(
                error = %e,
                addr = format_args!("0x{:02X}", pic_addr),
                "teardown step 5/5: PIC disable_voltage failed; rail is already at floor + caps drained"
            ),
        }
    }

    info!("Phase 3A: am2 safe-teardown sequence complete");
    if owner_policy == Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint {
        anyhow::bail!(
            "exact AM2 controller endpoint session missing during safe teardown; raw fallback refused"
        );
    }
    Ok(())
}

fn am2_uart_fallback_candidates() -> &'static [&'static str] {
    &[
        "/dev/ttyS0",
        "/dev/ttyS1",
        "/dev/ttyS2",
        "/dev/ttyS3",
        "/dev/ttyS4",
        "/dev/ttyS5",
        "/dev/ttyS6",
        "/dev/ttyS7",
    ]
}

fn proc_comm_matches_target(raw_comm: &str, target: &str) -> bool {
    raw_comm.trim_matches('\0').trim() == target
}

fn process_name_running(target: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let pid = file_name.to_string_lossy();
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let comm_path = entry.path().join("comm");
        let Ok(comm) = fs::read_to_string(comm_path) else {
            continue;
        };
        if proc_comm_matches_target(&comm, target) {
            return true;
        }
    }

    false
}

struct Am2HomeHardStopGuard {
    pwr_control_gpio: Option<String>,
    fan_idle_pwm: u8,
    fan_max_pwm: u8,
    /// Optional dsPIC teardown state, armed AFTER the I2C service + chain
    /// topology are known (the guard is constructed at the top of `run()`,
    /// before `i2c0_service` exists). `None` until `arm_dspic_teardown` is
    /// called → Drop then behaves byte-for-byte like the historical
    /// PWR_CONTROL-low + fans-only teardown. Once armed, Drop disables
    /// voltage on EVERY active dsPIC (incl. the effective chain dsPIC 0x22)
    /// FIRST, then drops PWR_CONTROL + fans — cut-hash-before-noise.
    ///
    /// `I2cServiceHandle` is a `Clone` wrapper around the service-thread
    /// `SyncSender`, so holding a clone here keeps the service worker alive
    /// until this guard drops. Drop order matters: this guard is constructed
    /// BEFORE `i2c0_service` in `run()`, so by reverse-declaration drop order
    /// `i2c0_service` drops first — but the worker thread only exits once the
    /// LAST sender (this clone) is gone, so the dsPIC disable in Drop still
    /// reaches a live service.
    dspic_service: Option<I2cServiceHandle>,
    /// Active dsPIC I²C addresses to disable on teardown (e.g. 0x20/0x21/0x22).
    dspic_disable_addrs: Vec<u8>,
    /// The selected/primary dsPIC address (gets the firmware hint below).
    selected_pic_addr: u8,
    /// Observed firmware byte of the selected dsPIC, if known — used only as a
    /// decode hint for the selected address (non-selected addrs auto-detect).
    selected_pic_fw: Option<u8>,
}

impl Am2HomeHardStopGuard {
    fn new(config: &DcentraldConfig) -> Self {
        Self {
            pwr_control_gpio: config.psu.pwr_control_gpio.clone(),
            fan_idle_pwm: config.thermal.fan_idle_pwm,
            fan_max_pwm: config.thermal.fan_max_pwm,
            dspic_service: None,
            dspic_disable_addrs: Vec::new(),
            selected_pic_addr: 0,
            selected_pic_fw: None,
        }
    }

    /// Arm the dsPIC-disable leg of the run-scope teardown.
    ///
    /// Called once from `run()` after the single-owner `/dev/i2c-0` service is
    /// up and the active-chain topology + selected dsPIC are resolved, so a
    /// later bare `?` early-return (which skips the explicit clean-stop
    /// teardown) still disables voltage on every active dsPIC instead of
    /// leaving the chain rail energized — the root cause of the
    /// "every standalone attempt needs a fresh AC-cycle" iteration tax.
    /// Idempotent-safe to call once; clones the service handle so the worker
    /// stays alive for the Drop.
    fn arm_dspic_teardown(
        &mut self,
        service: &I2cServiceHandle,
        active_dspic_addrs: Vec<u8>,
        selected_pic_addr: u8,
        selected_pic_fw: Option<u8>,
    ) {
        self.dspic_service = Some(service.clone());
        self.dspic_disable_addrs = active_dspic_addrs;
        self.selected_pic_addr = selected_pic_addr;
        self.selected_pic_fw = selected_pic_fw;
    }
}

impl Drop for Am2HomeHardStopGuard {
    fn drop(&mut self) {
        // Cut hash/power FIRST (cut-hash-before-noise): if armed, disable
        // voltage on EVERY active dsPIC (incl. the effective chain dsPIC, e.g.
        // 0x22) before touching PWR_CONTROL or fans. On a bare `?` early-return
        // the explicit clean-stop teardown never ran, so without this the chain
        // rail stayed energized and the next standalone launch needed an
        // AC-cycle. When NOT armed (`dspic_service == None` — e.g. an early
        // return before the I2C service was up, or the passthrough path which
        // never constructs this guard) this leg is a no-op and the teardown is
        // byte-for-byte identical to the historical PWR_CONTROL-low + fans-only
        // behaviour. `disable_voltage` is the same operation already issued on
        // the normal clean-shutdown path, so a redundant disable on clean exit
        // is benign + idempotent. Never raises fans (see below).
        if let Some(service) = self.dspic_service.as_ref() {
            if !self.dspic_disable_addrs.is_empty() {
                disable_dspic_addrs_best_effort(
                    service,
                    &self.dspic_disable_addrs,
                    self.selected_pic_addr,
                    self.selected_pic_fw,
                    "run-scope-drop-dspic-disable",
                );
            }
        }
        force_pwr_control_low(self.pwr_control_gpio.as_deref(), "run-scope-drop");
        force_am2_fans_to_quiet_idle(self.fan_idle_pwm, self.fan_max_pwm, "run-scope-drop");
    }
}

/// Parameters the `main()` panic hook needs to perform a best-effort
/// cut-hash-before-noise teardown when the daemon panics under
/// `panic = "abort"` (W24-CRASH-1 / w24-thermal-safety F-1).
///
/// `Am2HomeHardStopGuard::Drop` does NOT run on a `panic = "abort"` build —
/// the process aborts before any `Drop` impl fires — so the run-scope RAII net
/// is bypassed on a crash. The only remaining backstop is then the ~30 s
/// hardware PIC/PSU heartbeat watchdog. To make the cut-hash-on-crash guarantee
/// real again, the am2 hybrid run-scope stashes these teardown parameters into
/// a process-global the moment it arms PWR_CONTROL; the panic hook reads them
/// best-effort and drives `PWR_CONTROL` low + commands fans to quiet idle.
///
/// The fields mirror `Am2HomeHardStopGuard` exactly so the hook performs the
/// same cut-hash-before-noise sequence the Drop would have. The `String` is
/// allocated once at arm time (NOT in the hook), so the hook itself only reads
/// already-resident data — no allocation that could re-panic inside the hook.
#[derive(Debug, Clone)]
pub struct Am2TeardownParams {
    pub pwr_control_gpio: Option<String>,
    pub fan_idle_pwm: u8,
    pub fan_max_pwm: u8,
}

/// Process-global teardown params for the panic hook. `None`/unset means the
/// daemon panicked before any am2 hybrid run-scope energized hardware, so the
/// hook does nothing (there is nothing to cut). Set exactly once, the first
/// time a non-passthrough am2 hybrid run arms its `Am2HomeHardStopGuard`.
static AM2_TEARDOWN_PARAMS: OnceLock<Am2TeardownParams> = OnceLock::new();

/// Arm the process-global panic-hook teardown params for the am2 hybrid path.
///
/// Called from the run-scope at the same moment the `Am2HomeHardStopGuard` is
/// constructed (i.e. when the run is about to energize PWR_CONTROL). Idempotent:
/// `OnceLock::set` only succeeds the first time; subsequent calls are no-ops,
/// which is correct because every am2 hybrid run uses the same home-capped
/// teardown parameters from the same config shape.
pub fn arm_am2_teardown_params(config: &DcentraldConfig) {
    let _ = AM2_TEARDOWN_PARAMS.set(Am2TeardownParams {
        pwr_control_gpio: config.psu.pwr_control_gpio.clone(),
        fan_idle_pwm: config.thermal.fan_idle_pwm,
        fan_max_pwm: config.thermal.fan_max_pwm,
    });
}

/// Read the armed panic-hook teardown params, if any. `None` means no am2
/// hybrid run has energized hardware yet (nothing to cut on a panic).
pub fn am2_teardown_params() -> Option<&'static Am2TeardownParams> {
    AM2_TEARDOWN_PARAMS.get()
}

/// Best-effort cut-hash-before-noise teardown for the `main()` panic hook.
///
/// Safe to call from a `std::panic` hook on a `panic = "abort"` build: it reads
/// the process-global params (no allocation), and if they are armed it (1) cuts
/// hashboard power FIRST by driving `PWR_CONTROL` low, then (2) commands fans to
/// quiet idle — the same order, and the same home-capped (`PWM_SAFETY_MAX = 30`)
/// clamp, as `Am2HomeHardStopGuard::Drop`. Never blasts fans: the fan command
/// goes through `force_am2_fans_to_quiet_idle`, which only ever drives PWM DOWN
/// (min of idle / fan_max / 30). If params are unset, this is a no-op (the
/// daemon panicked before energizing anything). Errors inside the underlying
/// helpers are swallowed (they only `warn!`/`info!`); there are no `unwrap`s,
/// no `catch_unwind`, and no allocation in this function's own body.
pub fn panic_hook_best_effort_teardown() {
    if let Some(params) = am2_teardown_params() {
        // Cut hash power FIRST (cut-hash-before-noise), then ensure fans are at
        // the quiet home idle setpoint — never a blast.
        force_pwr_control_low(params.pwr_control_gpio.as_deref(), "panic-hook");
        force_am2_fans_to_quiet_idle(params.fan_idle_pwm, params.fan_max_pwm, "panic-hook");
    }
}

/// `DCENT_AM2_SKIP_THERMAL_SUPERVISOR` — lab override that disables the am2
/// thermal supervisor entirely. Never set this for an unattended home soak.
const ENV_AM2_SKIP_THERMAL_SUPERVISOR: &str = "DCENT_AM2_SKIP_THERMAL_SUPERVISOR";

/// R-13: env override that OPTS IN to per-chip die-temperature calibration on
/// top of (or instead of) the `[thermal.die_temp_calibration].enabled` config
/// flag. DEFAULT-OFF. Calibration is fail-safe (a bad/missing/not-cold baseline
/// falls back to raw, and the safety reading is never below raw), but it is
/// still gated because a live bench calibration proof is owed before flipping
/// it on by default — see
/// .
const ENV_AM2_DIE_TEMP_CALIBRATION: &str = "DCENT_AM2_DIE_TEMP_CALIBRATION";

/// Runtime thermal poll: bounded last-known-good tolerance before fail-closed.
const AM2_THERMAL_MAX_CONSECUTIVE_MISSES: u8 = 3;
/// Runtime thermal poll: max age of a last-known-good sample before it is stale.
const AM2_THERMAL_MAX_STALE_MS: u64 = 15_000;
/// Runtime thermal poll cadence floor.
const AM2_THERMAL_MIN_POLL_MS: u64 = 2_000;

/// THERM-1(a): consecutive CONFIDENT, TEMPERATURE-CORROBORATED "commanded PWM>0
/// but ALL fans 0 RPM while at/above the hot threshold" observations required
/// before the runtime supervisor fail-closes for a fan fault. A flaky /
/// inconclusive read (fan UIO could not be opened), any airflow / zero command,
/// or a cool (or non-finite) temperature RESETS the counter (see
/// `am2_fan_fault_step`), so a healthy, adequately-cooled, or flaky unit can
/// never accumulate to a cut.
const AM2_FAN_FAULT_STRIKES: u8 = 3;

/// THERM-1(b): graded thermal-throttle step (MHz) shed per runtime poll once
/// `hot_temp_c` is reached, down to the PLL floor. Matches the BM1362 PLL ramp
/// 25 MHz cadence — small steps, not a slam.
const AM2_THERMAL_THROTTLE_STEP_MHZ: u16 = 25;

/// THERM-1(b) gate. The graded freq throttle is a CONTROL action (it writes the
/// chain PLL), so it is DEFAULT-OFF to guarantee the proven `a lab unit`/`a lab unit` paths
/// stay byte-equivalent. Even when unset the supervisor STILL hard-cuts at the
/// dangerous threshold and warns between hot and dangerous; only the active
/// graded frequency reduction is opt-in.
const ENV_AM2_THERMAL_GRADED_THROTTLE: &str = "DCENT_AM2_THERMAL_GRADED_THROTTLE";

/// MINE-LIFE-1: env override for the GENEROUS mid-run nonce-stall fail-closed
/// timeout (seconds). Absent ⇒ a derived generous default (see
/// `am2_mid_run_nonce_stall_timeout`). `0` disables the mid-run guard.
const ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S: &str = "DCENT_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S";

/// MINE-LIFE-1: floor for the derived mid-run stall default so it is always
/// generous enough that a healthy eco unit emitting sparse nonces (e.g. ~1 TH/s
/// at ~50 MHz) can never trip it.
const AM2_MID_RUN_STALL_MIN_DEFAULT_S: u64 = 300;

/// Quiet fail-closed thermal supervisor for the am2 hybrid mining path.
///
/// `--s19j-hybrid` bypasses `Daemon::run()`, so the am2 hybrid mining loop
/// inherited NO thermal supervision — a wedged-hot ASIC on an unattended home
/// unit had nothing watching it. This mirrors the am3-bb `a lab unit` supervisor
/// contract (pre-stratum hard proof + bounded runtime polling + fail-closed)
/// but is am2-correct: board temps come from the fw=0x89 dsPIC LM75A
/// passthrough, and the Zynq XADC die temp is the MANDATORY fallback when
/// board temps are empty/NaN ( die-temp rule).
/// Fail-closed = cut hash power, fans stay at the configured cap — never a
/// fan blast.
struct Am2ThermalSupervisor {
    pic: Option<Pic0x89Service>,
    hot_temp_c: f32,
    dangerous_temp_c: f32,
    last_good: Option<(Instant, f32)>,
    consecutive_misses: u8,
    /// THERM-1(a): consecutive confident, temperature-corroborated fan-fault
    /// observations (see `am2_fan_fault_step`). Runtime-only; any airflow / zero
    /// command / inconclusive read / cool (or non-finite) temperature resets it.
    fan_fault_strikes: u8,
    /// R-13: per-chip die-temperature calibration. DEFAULT-OFF (fail-safe to
    /// raw). When enabled it captures a cold baseline at the pre-stratum poll
    /// and applies the offset to the XADC die read — but only ever RAISES the
    /// reading (never below raw), so it can never suppress an over-temp trip.
    die_calibration: dcentrald_thermal::die_calibration::DieCalibration,
}

impl Am2ThermalSupervisor {
    fn new(
        pic: Option<Pic0x89Service>,
        hot_temp_c: u8,
        dangerous_temp_c: u8,
        die_cal_cfg: dcentrald_thermal::die_calibration::DieCalibrationConfig,
    ) -> Self {
        Self {
            pic,
            hot_temp_c: f32::from(hot_temp_c),
            dangerous_temp_c: f32::from(dangerous_temp_c),
            last_good: None,
            consecutive_misses: 0,
            fan_fault_strikes: 0,
            die_calibration: dcentrald_thermal::die_calibration::DieCalibration::new(die_cal_cfg),
        }
    }

    /// THERM-1(a): runtime fan-fault poll, CORROBORATED by `temp_c` (the max
    /// die/board temperature the supervisor measured THIS poll). `true` ⇒ the
    /// caller MUST fail closed (cut hash; fans are NEVER raised —
    /// cut-hash-before-noise). Reads the fan tach + commanded PWM read-only (no
    /// board-control write) and runs the conservative, temperature-corroborated
    /// 3-strike decision (`am2_fan_fault_step`): only a confident "commanded
    /// PWM>0 but ALL fans 0 RPM" observed WHILE the unit is at/above the hot
    /// threshold adds a strike; airflow / zero command / an inconclusive read /
    /// a cool (or non-finite) temperature resets it. So a healthy unit (e.g. the
    /// `a lab unit` eco path at PWM 10 / ~2880 RPM / ~49 C) — and, crucially, any
    /// adequately-cooled sister am2-zynq unit with an unverified / wrong-mode /
    /// transient-zero tach — can never be false-cut while running cool.
    fn poll_fan_fault(&mut self, temp_c: f32) -> bool {
        let reading = am2_read_fan_rpm_and_pwm();
        let (next, faulted) =
            am2_fan_fault_step(self.fan_fault_strikes, reading, temp_c, self.hot_temp_c);
        self.fan_fault_strikes = next;
        faulted
    }

    /// Read the two raw temperature sources this poll, kept SEPARATE:
    /// `(max valid dsPIC LM75A board temp, raw Zynq XADC SoC-die temp)`. Either
    /// may be `None` if that source produced no finite, in-range reading. The
    /// XADC die read is ALWAYS attempted — the die source is never skipped
    /// ( die-temp fallback rule).
    ///
    /// The board temp is treated as the ABSOLUTE reference (an off-die PCB-class
    /// sensor) for R-13 die calibration; the die temp is the raw reading that
    /// calibration (when enabled) corrects. Keeping them separate lets
    /// [`Self::maybe_capture_die_baseline`] seed the calibration and lets
    /// [`Self::poll_max_temp`] apply it to the die alone.
    fn read_board_and_die(&mut self) -> (Option<f32>, Option<f32>) {
        let mut board_max = f32::NEG_INFINITY;
        if let Some(pic) = self.pic.as_mut() {
            for t in pic.read_all_temperatures() {
                let t = t as f32;
                if t.is_finite() && (-20.0..=125.0).contains(&t) {
                    board_max = board_max.max(t);
                }
            }
        }
        let board = if board_max.is_finite() {
            Some(board_max)
        } else {
            None
        };
        let die = match Xadc::read_temp() {
            Ok(die_c) if die_c.is_finite() && (0.0..125.0).contains(&die_c) => Some(die_c),
            _ => None,
        };
        (board, die)
    }

    /// Highest finite temperature this poll: `max(valid dsPIC LM75A board
    /// temps, R-13-calibrated Zynq XADC die temp)`. `None` if no source
    /// produced a finite, in-range reading.
    ///
    /// R-13: when die-temp calibration is enabled AND a valid cold baseline was
    /// captured, the die reading is corrected by its per-chip offset before
    /// being folded in — but the correction is fail-safe: it can only ever
    /// RAISE the die reading (never below raw), so this max can only ever be
    /// `>=` the pre-calibration max. Calibration therefore never delays or
    /// suppresses an over-temp trip. When calibration is off/uncaptured the die
    /// value passes through raw and this is byte-identical to the prior path.
    fn poll_max_temp(&mut self) -> Option<f32> {
        let (board, die) = self.read_board_and_die();
        let mut max_c = f32::NEG_INFINITY;
        if let Some(b) = board {
            max_c = max_c.max(b);
        }
        if let Some(die_c) = die {
            // apply_one is a raw passthrough unless calibration is enabled AND a
            // valid baseline exists; it is guaranteed to never return below raw.
            max_c = max_c.max(self.die_calibration.apply_one(die_c));
        }
        if max_c.is_finite() {
            Some(max_c)
        } else {
            None
        }
    }

    /// R-13: capture the cold die-calibration baseline, ONCE, from this poll's
    /// separate board (reference) + raw die readings. No-op unless calibration
    /// is enabled and not yet captured. Fail-safe by construction — the pure
    /// [`dcentrald_thermal::die_calibration::DieCalibration::capture_baseline`]
    /// rejects a missing / non-finite / not-cold / implausible sample and
    /// leaves the calibrator in raw-passthrough mode. Logs the outcome.
    ///
    /// Intended to run at the cold pre-stratum stage (before any hash load),
    /// where board ≈ die ≈ ambient, so the captured offset reflects the die
    /// ADC's per-chip bias rather than a thermal gradient.
    fn maybe_capture_die_baseline(&mut self) {
        if !self.die_calibration.enabled() || self.die_calibration.is_calibrated() {
            return;
        }
        let (board, die) = self.read_board_and_die();
        let (Some(reference_pcb_c), Some(raw_die_c)) = (board, die) else {
            info!(
                board_present = board.is_some(),
                die_present = die.is_some(),
                "R-13 die-calibration baseline skipped — need BOTH a board (reference) and XADC die reading; staying on RAW die temp"
            );
            return;
        };
        let outcome = self
            .die_calibration
            .capture_baseline(reference_pcb_c, &[raw_die_c]);
        if outcome.is_captured() {
            info!(
                chips = self.die_calibration.chip_count(),
                reference_pcb_c,
                raw_die_c,
                "R-13 die-calibration cold baseline captured — die temp is now offset-corrected (never below raw)"
            );
        } else {
            warn!(
                outcome = ?outcome,
                reference_pcb_c,
                raw_die_c,
                "R-13 die-calibration baseline NOT captured — staying on RAW die temp (fail-safe)"
            );
        }
    }

    /// Poll once and enforce the fail-closed contract for `stage`. `Ok(temp)`
    /// = safe to proceed; `Err` = the caller MUST fail closed (cut hash power).
    fn poll_and_check(&mut self, stage: &'static str) -> Result<f32> {
        match self.poll_max_temp() {
            Some(max_c) => {
                self.last_good = Some((Instant::now(), max_c));
                self.consecutive_misses = 0;
                if max_c >= self.dangerous_temp_c {
                    anyhow::bail!(
                        "AM2 thermal supervisor: max temp {:.1}C reached dangerous threshold {:.1}C during {}",
                        max_c,
                        self.dangerous_temp_c,
                        stage
                    );
                }
                if max_c >= self.hot_temp_c {
                    warn!(
                        max_temp_c = max_c,
                        hot_temp_c = self.hot_temp_c,
                        dangerous_temp_c = self.dangerous_temp_c,
                        stage,
                        "AM2 thermal supervisor: hot — fail-closed arms at the dangerous threshold"
                    );
                } else {
                    info!(
                        max_temp_c = max_c,
                        stage, "AM2 thermal supervisor sample OK"
                    );
                }
                Ok(max_c)
            }
            None => {
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if stage == "runtime" {
                    if let Some((at, t)) = self.last_good {
                        if self.consecutive_misses <= AM2_THERMAL_MAX_CONSECUTIVE_MISSES
                            && at.elapsed() <= Duration::from_millis(AM2_THERMAL_MAX_STALE_MS)
                        {
                            warn!(
                                consecutive_misses = self.consecutive_misses,
                                last_good_age_ms = at.elapsed().as_millis() as u64,
                                last_good_temp_c = t,
                                "AM2 thermal supervisor: runtime poll empty — using bounded last-known-good"
                            );
                            return Ok(t);
                        }
                    }
                }
                anyhow::bail!(
                    "AM2 thermal supervisor: no valid temperature (dsPIC LM75A + XADC die) during {} after {} consecutive miss(es) — refusing to mine without thermal proof",
                    stage,
                    self.consecutive_misses
                )
            }
        }
    }
}

/// PIC heartbeat interval — 1 s (same cadence bosminer uses for fw 0x89).
/// Shorter than the S9 PIC watchdog so we have margin before voltage cut.
///
/// See [`dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`]
/// for the per-`(Platform, PicFw)` matrix
/// (`Platform::S19jProAm2` × `PicFw::Dspic33epHealthy` = 1 s tick).
/// The matrix is the canonical source — keep this constant in sync.
const PIC_HEARTBEAT_INTERVAL_MS: u64 = 1000;
/// One aggregate deadline for all blocking AM2 heartbeat/feed workers.
const AM2_FEEDER_STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Consecutive selected-dsPIC heartbeat failures after which the am2 hybrid run
/// cancels itself so teardown de-energizes the chain rail.
///
/// Load-bearing safety rule (: "When PIC
/// heartbeats fail, the response MUST be voltage cut", as the am3-bb path already
/// does). Sized to act at ~20 s (20 × `PIC_HEARTBEAT_INTERVAL_MS`) — comfortably
/// above transient dsPIC read noise (the counter resets on ANY successful
/// heartbeat) yet below the PSU's own ~30 s heartbeat-loss self-disable, so the
/// software cut LEADS the hardware backstop without ever tripping on a
/// healthy-but-briefly-flaky PIC (on the proven `a lab unit`/`a lab unit` mining path a live
/// fw=0x89 dsPIC answers reliably, so 20 consecutive silences means it is dead).
const AM2_HYBRID_PIC_HEARTBEAT_MAX_FAILURES: u32 = 20;

/// `a lab unit` APW121215a is known-good as FW `0x71`. Used only for the write-only
/// GPIO bit-bang bootstrap path.
const APW12_139_ASSUMED_FW: u8 = 0x71;

fn am2_slot_from_serial_device(serial_device: &str) -> Option<u8> {
    match serial_device {
        "/dev/ttyS1" => Some(0),
        "/dev/ttyS2" => Some(1),
        "/dev/ttyS3" => Some(2),
        "/dev/ttyS4" => Some(3),
        _ => None,
    }
}

fn am2_pic_addr_from_serial_device(serial_device: &str) -> Option<u8> {
    let slot = am2_slot_from_serial_device(serial_device)?;
    S19_DSPIC_ADDRS.get(slot as usize).copied()
}

fn dspic_chain_bit(addr: u8) -> Option<u8> {
    S19_DSPIC_ADDRS
        .iter()
        .position(|&candidate| candidate == addr)
        .map(|idx| 1u8 << idx)
}

fn dspic_addr_active(active_chains: u8, addr: u8) -> bool {
    dspic_chain_bit(addr)
        .map(|bit| active_chains & bit != 0)
        .unwrap_or(false)
}

fn active_dspic_addrs(active_chains: u8) -> Vec<u8> {
    S19_DSPIC_ADDRS
        .iter()
        .enumerate()
        .filter_map(|(idx, &addr)| {
            if active_chains & (1u8 << idx) != 0 {
                Some(addr)
            } else {
                None
            }
        })
        .collect()
}

///  all-active bus-prime ordering, extracted as a pure fn ( B05)
/// so the ordering is asserted by a REAL runtime unit test instead of a
/// source-string parse. Returns the active dsPIC addresses EXCEPT `selected`,
/// sorted ascending — these are warmed BEFORE the selected PIC. This pins the
/// deterministic order the loop uses (non-selected first, ascending); it makes
/// NO claim that a specific "0x20-first" sequence is required (that dependency
/// is unverified at runtime — see the loop comment + the memory rule).
fn am2_bus_prime_order(active: &[u8], selected: u8) -> Vec<u8> {
    let mut others: Vec<u8> = active.iter().copied().filter(|&a| a != selected).collect();
    others.sort_unstable();
    others
}

fn disable_dspic_addrs_best_effort(
    service: &I2cServiceHandle,
    addrs: &[u8],
    selected_pic_addr: u8,
    selected_fw_hint: Option<u8>,
    reason: &'static str,
) {
    for &addr in addrs {
        let fw_hint = if addr == selected_pic_addr {
            selected_fw_hint
        } else {
            None
        };
        let fw_hint_log = fw_hint
            .map(|fw| format!("0x{:02X}", fw))
            .unwrap_or_else(|| "auto".to_string());
        let mut shutdown_pic = Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint);
        if let Err(hb_err) = shutdown_pic.send_heartbeat() {
            warn!(
                error = %hb_err,
                addr = format_args!("0x{:02X}", addr),
                reason,
                "PIC heartbeat before best-effort disable failed (continuing)"
            );
        }
        match shutdown_pic.disable_voltage() {
            Ok(()) => info!(
                addr = format_args!("0x{:02X}", addr),
                fw_hint = %fw_hint_log,
                reason,
                "PIC voltage disabled during best-effort teardown"
            ),
            Err(disable_err) => warn!(
                error = %disable_err,
                addr = format_args!("0x{:02X}", addr),
                fw_hint = %fw_hint_log,
                reason,
                "PIC voltage disable failed during best-effort teardown"
            ),
        }
    }
}

fn probe_dspic_addrs(i2c: &I2cServiceHandle) -> Result<u8> {
    let mut active_chains = 0u8;

    for (idx, &addr) in S19_DSPIC_ADDRS.iter().enumerate() {
        // : .139 is expected to expose only
        // hb2 at 0x21; hb3/0x22 can ACK scans yet return 0xFF forever.
        let reply = i2c.transaction_mutating(
            I2cMutationLabel::Recovery,
            addr,
            vec![
                I2cTransactionStep::SetTimeout(10),
                I2cTransactionStep::WriteByteByByte(vec![0u8; 8]),
                I2cTransactionStep::SleepMs(10),
                I2cTransactionStep::Write(vec![0x55, 0xAA, 0x17]),
                I2cTransactionStep::SleepMs(10),
                //  and
                // : bare
                // fw0x86 GET_VERSION is one byte; do not bulk-read tails.
                I2cTransactionStep::Read(1),
            ],
        );

        match reply {
            Ok(mut reads) => {
                let byte = reads
                    .pop()
                    .and_then(|buf| buf.first().copied())
                    .unwrap_or(0xFF);
                if byte != 0xFF {
                    active_chains |= 1u8 << idx;
                    info!(
                        addr = format_args!("0x{:02X}", addr),
                        reply = format_args!("0x{:02X}", byte),
                        "AM2 dsPIC chain probe active"
                    );
                } else {
                    info!(
                        addr = format_args!("0x{:02X}", addr),
                        reply = format_args!("0x{:02X}", byte),
                        "AM2 dsPIC chain probe silent"
                    );
                }
            }
            Err(e) => {
                warn!(
                    addr = format_args!("0x{:02X}", addr),
                    error = %e,
                    "AM2 dsPIC chain probe transaction failed"
                );
            }
        }
    }

    info!(
        mask = format_args!("0b{:03b}", active_chains),
        active_addrs = ?active_dspic_addrs(active_chains),
        "AM2 dsPIC active chain mask"
    );

    if active_chains == 0 {
        anyhow::bail!("No active dsPIC chains detected at 0x20/0x21/0x22");
    }

    Ok(active_chains)
}

/// Cheap single-address dsPIC presence probe (Fix #2).
///
/// Reuses the exact 4-step single-byte GET_VERSION shape from
/// [`probe_dspic_addrs`] (which already encodes "0xFF means absent"
/// semantics): a parser flush (8 zero bytes), a bare framed `[0x55,0xAA,0x17]`
/// GET_VERSION write, and a single-byte read with a tight 10 ms timeout.
///
/// Returns `true` only when the address replies with a NON-`0xFF` byte. An
/// EIO (transaction error) or a `0xFF` reply both mean "absent / not present"
/// → `false`. This lets the Phase-0d bus-prime loop skip a physically-absent
/// dsPIC (e.g. `a lab unit` slot-2 0x21) BEFORE issuing the multi-retry warmup, which
/// otherwise produces an EIO storm that desyncs the AXI-IIC controller faster
/// than the rate-limited (1/sec) fd-reopen can recover.
fn am2_dspic_present(i2c: &I2cServiceHandle, addr: u8) -> bool {
    match i2c.transaction_mutating(
        I2cMutationLabel::Recovery,
        addr,
        vec![
            I2cTransactionStep::SetTimeout(10),
            I2cTransactionStep::WriteByteByByte(vec![0u8; 8]),
            I2cTransactionStep::SleepMs(10),
            I2cTransactionStep::Write(vec![0x55, 0xAA, 0x17]),
            I2cTransactionStep::SleepMs(10),
            I2cTransactionStep::Read(1),
        ],
    ) {
        Ok(mut reads) => {
            let byte = reads
                .pop()
                .and_then(|buf| buf.first().copied())
                .unwrap_or(0xFF);
            byte != 0xFF
        }
        Err(_) => false,
    }
}

/// BM1362 job ID increment. Phase 4A: am2 MIDSTATE_CNT=1 (2 midstate slots)
/// so the ExtWorkId stride is (1 << MIDSTATE_CNT_LOG2) = 2.
/// Braiins encodes `ExtWorkId(wid,ms).to_hw() = (wid << LOG2) | ms` and the
/// driver increments `wid` by 1 per job → hardware work_id increments by 2.
const JOB_ID_INCREMENT: u8 = 2;
const JOB_ID_MASK: u8 = 0x7F;
const WORK_HISTORY_PER_ID: usize = dcentrald_common::DEFAULT_WORK_HISTORY_PER_ID;

/// BM1362 FPGA work size (am2, Phase 4A authoritative):
/// 4 header words + 2 midstate slots × 8 words = 20 words.
const WORK_WORDS: usize = 20;

/// Log2 of the midstate count that fits into this WORK_WORDS payload.
/// For am2: 2 slots = LOG2 1. Used to shift the ASIC-side work_id stride.
const MIDSTATE_CNT_LOG2: u32 = 1;

/// Authoritative am2 FPGA CTRL value (Phase 4A live probe .139, 2026-04-20).
/// Read identically on both populated chains (chain1 @0x43C00000 and
/// chain4 @0x43C30000) while bosminer was hashing ~69 TH/s.
///
/// Bit decode:
///   bit  1 (0x0000_0002): IP_ENABLE / MINER_EN
///   bit 12 (0x0000_1000): clock-enable / midstate lsb
///   bit 20 (0x0010_0000): MIDSTATE_CNT=1 (2-midstate mode)
///   bit 23 (0x0080_0000): EXT_BAUD_ENABLE (fast baud active)
const AM2_CTRL_BM1362: u32 = 0x0090_1002;

/// BM1362 nonce response body length (after preamble strip).
const BM1362_RESP_BODY_LEN: usize = 9;

// ---------------------------------------------------------------------------
// I2C0 bring-up helper (unchanged from Phase 4)
// ---------------------------------------------------------------------------

/// Ensure `/dev/i2c-0` exists and is bound to the xiic-i2c kernel driver.
///
/// NEVER unbind/SOFTR the controller here.
/// This only binds — never unbinds — and never touches AXI timing.
fn ensure_i2c0_kernel_bound() -> Result<()> {
    if std::path::Path::new("/dev/i2c-0").exists() {
        return Ok(());
    }
    let _ = std::fs::write("/sys/bus/platform/drivers/xiic-i2c/bind", "41600000.i2c");
    std::thread::sleep(Duration::from_millis(500));
    if !std::path::Path::new("/dev/i2c-0").exists() {
        use std::process::Command;
        let _ = Command::new("mknod")
            .args(["/dev/i2c-0", "c", "89", "0"])
            .status();
    }
    if !std::path::Path::new("/dev/i2c-0").exists() {
        anyhow::bail!("Failed to bring up /dev/i2c-0 after xiic-i2c bind + mknod");
    }
    info!("xiic-i2c kernel driver bound + /dev/i2c-0 ready");
    Ok(())
}

// ---------------------------------------------------------------------------
// BM1362 ASIC init constants
// ---------------------------------------------------------------------------

const VERSION_MASK_DISABLED: u32 = 0x0000_0000;
const VERSION_MASK_DEFAULT: u32 = BM1362_INIT_PLAN.version_mask;
const INIT_CONTROL_BCAST: u32 = BM1362_INIT_PLAN.init_control_broadcast;
const INIT_CONTROL_PER_CHIP: u32 = BM1362_INIT_PLAN.init_control_per_chip;
const MISC_CONTROL_PRE_BAUD: u32 = BM1362_INIT_PLAN.misc_control_pre_baud;
const MISC_CONTROL_POST_FAST_BAUD: u32 = BM1362_INIT_PLAN.misc_control_post_fast_baud;
const CORE_REG_HASH_CLK: u32 = BM1362_INIT_PLAN.core_reg_hash_clk;
const CORE_REG_CLK_DELAY: u32 = BM1362_INIT_PLAN.core_reg_clk_delay;
const CORE_REG_UNKNOWN: u32 = BM1362_INIT_PLAN.core_reg_family;
const TICKET_MASK_256: u32 = 0x0000_00FF;
const IO_DRIVER_NORMAL: u32 = BM1362_INIT_PLAN.io_driver_normal;
const ANALOG_MUX_VALUE: u32 = BM1362_INIT_PLAN.analog_mux_value;
const FAST_UART_VALUE: u32 = BM1362_INIT_PLAN.fast_uart_value;
/// BM1362 UART_RELAY candidate ASIC registers and enable values.
/// R6-7 keeps these writes lab-gated until byte-exact live captures settle
/// whether reg 0x2C/0x34 are required control writes or data-only clues.
///
/// W13.B1 (2026-05-10): the FPGA `0x43D00030`/`0x43D00034` window is
/// reclassified as a diagnostic-only glitch monitor mirror, Braiins-am2
/// only. Candidate ASIC-side controls are preserved here behind
/// `DCENT_BM1362_ENABLE_UART_RELAY_LAB`. See
/// `dcentrald_asic::bm1362::uart_relay` and
/// .
const BM1362_UART_RELAY_REG: u8 = 0x2C;
const BM1362_UART_RELAY_ENABLE: u32 = 0x007C_0003;
/// Alternate candidate layout retained for lab capture comparison.
const BM1362_UART_RELAY_REG_ALT: u8 = 0x34;
const BM1362_UART_RELAY_ENABLE_ALT: u32 = 0x000F_0003;
const NONCE_RANGE_126: u32 = 0x0000_1381;
const BM1362_PLL0_DIVIDER_REG: u8 = 0x70;
const BM1362_TRACE_PLL0_DIVIDER: u32 = 0x0000_0000;
const BM1362_TRACE_PLL_PARAM_525: u32 = 0x40A8_0265;

// ===========================================================================
// RE-018 byte-exact bosminer cold-wake values (decoded 2026-05-31 from
// `/data/re018-cold-strace.log` on `a lab unit` — a strace of bosminer's COLD chain
// bring-up on /dev/ttyS1). These are the EXACT register values + ORDER
// bosminer uses to cold-wake the BM1362 chain on `a lab unit` standalone (no prior
// engagement). Used ONLY by the default-OFF, `a lab unit`-fingerprinted
// `DCENT_AM2_BM1362_RE018_COLD_SEQUENCE` path. See
// `bm1362_re018_cold_sequence` for the full decoded ordering.
//
// Decoded cold sequence (first init cycle, t0 = first chain write):
//   BCAST 0xA8=0x00000000  -> 0x18=0xB000C100 -> 0xA4=0x9000FFFF
//   CHAIN_INACTIVE x3
//   SET_ADDRESS 0x00..0xFE stride-2 (blind, all 128 positions)
//   BCAST 0x3C=0x80008540 -> 0x3C=0x80008008 -> 0x54=0x00000003 -> 0x58=0x00011111
//   GET_ADDRESS (0x52) x1  (single late presence poll — NON-FATAL)
//   BCAST 0x70=0x00000000 -> 0x08=0x40A80265 -> 0x70=0x00000000 -> 0x08=0x40A80265 -> 0x28=0x11300000
//   PER-CHIP (0x41), each enumerated addr, in order:
//     0xA8=0x00000002, 0x18=0xB000C100, 0x3C=0x80008540, 0x3C=0x80008008,
//     0x3C=0x800082AA, 0x0C=(0x80000000 | addr*0x104)   <- the per-chip NONCE-SPACE base
//
// CRUCIAL deltas vs DCENT init_asic_chain (the 0-nonces cause):
//  - reg 0x0C per-chip nonce-space base is NEVER written by DCENT (missing).
//  - bosminer's per-chip 0xA8 = 0x00000002 (DCENT default 0x000701F0).
//  - bosminer's per-chip + pre/post MiscCtrl = 0xB000C100 (DCENT post-fast
//    default 0x00C100B0; pre-baud default 0xFF0FC100).
//  - bosminer's broadcast 0xA8 = 0x00000000 (DCENT default 0x00070000).
//  - GetAddress is a single LATE poll AFTER the broadcast 0x3C/0x54/0x58
//    block, NOT a per-chip enumeration gate; bosminer NEVER bails on it.
const RE018_BCAST_A8: u32 = 0x0000_0000;
const RE018_MISC_CTRL: u32 = 0xB000_C100;
const RE018_VERSION_MASK: u32 = 0x9000_FFFF;
const RE018_CORE_3C_HASH_CLK: u32 = 0x8000_8540;
const RE018_CORE_3C_CLK_DELAY: u32 = 0x8000_8008;
const RE018_CORE_3C_FAMILY: u32 = 0x8000_82AA;
const RE018_ANALOG_MUX_54: u32 = 0x0000_0003;
const RE018_IO_DRIVER_58: u32 = 0x0001_1111;
const RE018_PLL0_DIVIDER_70: u32 = 0x0000_0000;
const RE018_PLL_08: u32 = 0x40A8_0265;
const RE018_FAST_UART_28: u32 = 0x1130_0000;
const RE018_PER_CHIP_A8: u32 = 0x0000_0002;
/// Per-chip nonce-space base register (reg 0x0C). Value = 0x80000000 | (addr * 0x104).
const RE018_NONCE_SPACE_REG: u8 = 0x0C;
const RE018_NONCE_SPACE_BASE: u32 = 0x8000_0000;
const RE018_NONCE_SPACE_STRIDE: u32 = 0x0000_0104;

// --- RE-018 FULL-CORE-INIT + LOW-FREQ overrides (2026-06-14 perf audit) -------
//
// Root cause of the ~400x-low standalone hashrate (cores CLOCKED but IDLE):
// the RE-018 cold strace this sequence replays captured only the FIRST init
// cycle, and that cycle's INIT_CONTROL (reg 0xA8) values were
//   broadcast  RE018_BCAST_A8    = 0x0000_0000
//   per-chip   RE018_PER_CHIP_A8 = 0x0000_0002
// i.e. the high 0x0007 nibble cluster is ZERO. On the `a lab unit` ACCEPTED-SHARE-
// PROVEN BM1362 init (am3_bb / legacy s19j per-chip A8) the same register is
//   broadcast  INIT_CONTROL_BCAST    = 0x0007_0000  (BM1362_INIT_PLAN)
//   per-chip   INIT_CONTROL_PER_CHIP = 0x0007_01F0  (BM1362_INIT_PLAN)
// The 0x0007 high bits are the core/clock ENABLE cluster — without them the
// PLL clocks the chip but the SHA cores stay gated (idle), so the chain draws
// power at the PLL freq yet produces almost no useful hashing. A live
// ticket-mask A/B already proved the useful hashrate is unchanged by the
// hardware-difficulty filter (reg 0x14), so the idle is UPSTREAM of nonce
// reporting = core enable (reg 0xA8 high bits). These two constants are the
// `a lab unit`-proven activation values, swapped in under
// `DCENT_AM2_RE018_FULL_CORE_INIT` (default-OFF + `a lab unit`-fingerprint) so the
// fleet (.109/.79/.129/.135/S9) and the  handoff stay byte-identical.
const RE018_FULL_CORE_BCAST_A8: u32 = INIT_CONTROL_BCAST; // 0x0007_0000 (BM1362_INIT_PLAN)
const RE018_FULL_CORE_PER_CHIP_A8: u32 = INIT_CONTROL_PER_CHIP; // 0x0007_01F0 (BM1362_INIT_PLAN)

// LOW-FREQ PLL (reg 0x08) for full-core standalone within the ~550 W home
// breaker budget.
//
// POWER: un-idling the cores at the RE-018 default 525 MHz would draw ~1.5 kW
// for one board = breaker TRIP. Power scales ~linearly with frequency at fixed
// voltage, so we MUST drop the freq when we enable the cores. Target 150 MHz:
//   est. board watts ≈ 1500 W * (150 / 525) ≈ ~430 W  (one board)
//   est. board hashrate ≈ 50 TH/s * (150 / 525) ≈ ~14 TH/s
// Conservative — lands clearly under the 550 W ceiling with headroom for the
// PSU/fan overhead, and is a ~120x jump from today's ~120 GH/s useful.
//
// ENCODING (bm1362 crate `BM1362_PLL_TABLE` layout — accepted-share-proven on
// .79/.109/.139): reg 0x08 = [VCO_SCALE 0x50][FBDIV][REFDIV 0x01][POSTDIV],
// POSTDIV byte = ((PD1-1)<<4) | (PD2-1), freq = 25 MHz * FBDIV / (REFDIV*PD1*PD2).
// We deliberately KEEP FBDIV=210 / REFDIV=1 (VCO = 25*210 = 5250 MHz) — the
// SAME VCO the chip already locks at 525 MHz on `a lab unit` today — and only raise
// the postdivider, so the VCO never has to re-lock to a new band:
//   FBDIV=210 (0xD2), REFDIV=1, PD1=7, PD2=5  ->  5250 / (7*5) = 150 MHz
//   POSTDIV byte = ((7-1)<<4)|(5-1) = 0x64
//   reg 0x08 = 0x50_D2_01_64
// NOTE: this is the DCENT proven table byte order, NOT the bosminer cold
// capture's `0x40A8_0265` byte layout (which is bosminer's own field packing
// of fbdiv=168/refdiv=2/pd=2x2 -> 525 MHz). Both reach the wire as raw BE
// bytes via `send_write_reg_broadcast_bm1397plus`; we use the table encoding
// because it is the accepted-share-proven one on the sibling BM1362 units.
// 2026-06-14 (option-B, SUPERSEDES the 150 MHz attempt above): the freq-OVERRIDE default is the
// PROVEN 320 MHz efficiency sweet spot in the table byte order (FBDIV=128/REFDIV=1/PD1=5/PD2=2 ->
// ÷10, 0x50800141 — same encoding bm1362::pll_lookup_extended computes). NOT the live-REJECTED
// off-table 150 MHz (0x50D2_0164, ÷35, VCO re-lock -> zero nonces). Configurable via
// DCENT_AM2_RE018_TARGET_MHZ (240-597 proven) or DCENT_AM2_RE018_PLL_HEX (raw), and INDEPENDENT of
// the A8 regression. The ~50 MHz RE018_PLL_08 stays the DEFAULT (eco/heater floor — a feature, lower
// than any competitor's floor); this override is opt-in + POWER-RAISING (wattmeter-gated live test).
const RE018_LOW_FREQ_PLL_08: u32 = 0x5080_0141; // 320 MHz: FBDIV=128, REFDIV=1, PD1=5, PD2=2 (proven ÷10)
const RE018_LOW_FREQ_MHZ: u16 = 320;

/// The REAL applied chip frequency (MHz), decoded from the RE-018 PLL reg-0x08 and
/// published from the cold sequence so the dashboard reports the ACTUAL frequency
/// (~50 MHz eco/heater default, or the gated bump) instead of the config label
/// (which historically showed a misleading 525). 0 = unset → non-RE-018 units
/// fall back to `config.mining.frequency_mhz` (fleet byte-identical).
static AM2_RE018_APPLIED_FREQ_MHZ: std::sync::atomic::AtomicU16 =
    std::sync::atomic::AtomicU16::new(0);

/// Serial dispatch pacing during init — ~50 writes/sec max.
const SERIAL_PACE_MIN_MS: u64 = 20;

/// am2 work-tx FIFO depth.
#[allow(dead_code)]
const AM2_WORK_TX_DEPTH: usize = 20;

/// MiscCtrl register address. Triple-write only via `misc_ctrl_triple_write_serial`.
const REG_MISC_CONTROL: u8 = BM1362_INIT_PLAN.misc_control_register;

enum Am2FpgaChain {
    Devmem(DevmemFpgaChain),
    Uio(FpgaChain),
}

impl Am2FpgaChain {
    fn read_raw(&self, offset: u32) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_raw(offset),
            Self::Uio(fpga) => fpga.read_raw(offset),
        }
    }

    fn read_version(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_version(),
            Self::Uio(fpga) => fpga.read_version(),
        }
    }

    fn read_ctrl(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_ctrl(),
            Self::Uio(fpga) => fpga.read_ctrl(),
        }
    }

    fn write_ctrl(&self, value: u32) {
        match self {
            Self::Devmem(fpga) => fpga.write_ctrl(value),
            Self::Uio(fpga) => fpga.write_ctrl(value),
        }
    }

    fn read_baud(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_baud(),
            Self::Uio(fpga) => fpga.read_baud(),
        }
    }

    fn set_baud(&self, divisor: u32) {
        match self {
            Self::Devmem(fpga) => fpga.set_baud(divisor),
            Self::Uio(fpga) => fpga.set_baud(divisor),
        }
    }

    fn set_work_time(&self, value: u32) {
        match self {
            Self::Devmem(fpga) => fpga.set_work_time(value),
            Self::Uio(fpga) => fpga.set_work_time(value),
        }
    }

    fn read_work_time(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_time(),
            Self::Uio(fpga) => fpga.read_work_time(),
        }
    }

    fn reset_work_fifos(&self) {
        match self {
            Self::Devmem(fpga) => fpga.reset_work_fifos(),
            Self::Uio(fpga) => fpga.reset_fifos(),
        }
    }

    fn read_work_tx_status(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_tx_status(),
            Self::Uio(fpga) => fpga.read_work_tx_status(),
        }
    }

    fn read_work_tx_ctrl(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_tx_ctrl(),
            Self::Uio(fpga) => fpga.read_work_tx_ctrl(),
        }
    }

    fn read_work_tx_threshold(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_tx_threshold(),
            Self::Uio(fpga) => fpga.read_work_tx_threshold(),
        }
    }

    fn read_work_tx_last(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_tx_last(),
            Self::Uio(fpga) => fpga.read_work_tx_last(),
        }
    }

    fn read_work_rx_status(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_work_rx_status(),
            Self::Uio(fpga) => fpga.read_work_rx_status(),
        }
    }

    fn write_work_tx_ctrl(&self, value: u32) {
        match self {
            Self::Devmem(fpga) => fpga.write_work_tx_ctrl(value),
            Self::Uio(fpga) => fpga.write_work_tx_ctrl(value),
        }
    }

    fn write_work_tx_threshold(&self, value: u32) {
        match self {
            Self::Devmem(fpga) => fpga.write_work_tx_threshold(value),
            Self::Uio(fpga) => fpga.write_work_tx_threshold(value),
        }
    }

    fn read_nonce(&self) -> Option<(u32, u32)> {
        match self {
            Self::Devmem(fpga) => fpga.read_nonce(),
            Self::Uio(fpga) => fpga.read_nonce(),
        }
    }

    fn work_rx_has_data(&self) -> bool {
        match self {
            Self::Devmem(fpga) => fpga.work_rx_has_data(),
            Self::Uio(fpga) => fpga.work_rx_has_data(),
        }
    }

    fn work_tx_full(&self) -> bool {
        match self {
            Self::Devmem(fpga) => fpga.work_tx_full(),
            Self::Uio(fpga) => fpga.work_tx_full(),
        }
    }

    fn flush_work_tx(&self) {
        match self {
            Self::Devmem(fpga) => fpga.flush_work_tx(),
            Self::Uio(fpga) => fpga.flush_work_tx(),
        }
    }

    fn flush_work_rx(&self) {
        match self {
            Self::Devmem(fpga) => fpga.flush_work_rx(),
            Self::Uio(fpga) => fpga.flush_work_rx(),
        }
    }

    fn write_work(&self, words: &[u32]) {
        match self {
            Self::Devmem(fpga) => fpga.write_work(words),
            Self::Uio(fpga) => fpga.write_work(words),
        }
    }

    fn read_error_count(&self) -> u32 {
        match self {
            Self::Devmem(fpga) => fpga.read_error_count(),
            Self::Uio(fpga) => fpga.read_error_count(),
        }
    }
}

/// Triple-write MiscCtrl (reg 0x18) on the serial transport with 5 ms spacing.
fn misc_ctrl_triple_write_serial(serial: &SerialChainBackend, value: u32) -> Result<()> {
    for i in 0..3 {
        serial
            .send_write_reg_broadcast_bm1397plus(REG_MISC_CONTROL, value)
            .with_context(|| format!("MiscCtrl triple-write attempt {}/3", i + 1))?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn misc_ctrl_triple_write_chip_serial(
    serial: &SerialChainBackend,
    chip_addr: u8,
    value: u32,
) -> Result<()> {
    for i in 0..3 {
        serial
            .send_write_reg_bm1397plus(chip_addr, REG_MISC_CONTROL, value)
            .with_context(|| {
                format!(
                    "MiscCtrl chip 0x{:02X} triple-write attempt {}/3",
                    chip_addr,
                    i + 1
                )
            })?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// BM1362 per-chip init — the full per-chip `A8 / MiscCtrl×3 / 3C×3` loop.
///
/// Stock bosminer and the accepted-share-proven BM1362 paths
/// (`serial_mining.rs` Amlogic, `am3_bb_mining.rs` on `a lab unit`) all run this
/// per-chip pass. `init_asic_chain` ran it only as Step 7 — *after* the
/// FastUART transition — so the `DCENT_AM2_SKIP_FAST_UART=1` path (which
/// early-returns before Step 7) skipped per-chip core activation entirely.
/// Broadcast `0xA8 = INIT_CONTROL_BCAST` alone does not activate the cores;
/// the per-chip `0xA8 = INIT_CONTROL_PER_CHIP` write is the BM1362 analogue
/// of BM1387 open-core. Extracted so both the fast-baud Step 7 and the
/// 115200 skip-FastUART path run the identical sequence.
fn bm1362_per_chip_fast_init(
    serial: &SerialChainBackend,
    chip_count: u8,
    addr_interval: u16,
) -> Result<()> {
    // G2 / R10-2 ablation knob (default None = full loop, unchanged). Read
    // ONCE before the loop so the hot path takes only cheap local branches;
    // the self-describing log fires ONLY when the env is set (mirrors
    // PR-019's [AM2-ABLATION-PARAMS] contract — default path stays silent).
    let stop_after = am2_ablation_per_chip_stop_after();
    if let Some(mode) = stop_after {
        warn!(
            "[AM2-ABLATION-PERCHIP] stop_after={} — G2/R10-2 per-chip substep \
             bisection; read the post_per_chip_loop_115200 [AM2-ABLATION] \
             unique_chip_ids (requires DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE=1)",
            mode
        );
    }
    for i in 0..chip_count {
        let chip_addr = (i as u16 * addr_interval) as u8;
        serial.send_write_reg_bm1397plus(chip_addr, 0xA8, INIT_CONTROL_PER_CHIP)?;
        if stop_after != Some("a8") {
            misc_ctrl_triple_write_chip_serial(serial, chip_addr, am2_misc_control_post_fast())?;
            if stop_after != Some("miscctrl") {
                serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_HASH_CLK)?;
                serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_CLK_DELAY)?;
                serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_UNKNOWN)?;
            }
        }
        if i % 16 == 15 {
            std::thread::sleep(Duration::from_millis(SERIAL_PACE_MIN_MS));
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// BM1362 serial-wire nonce frame length (`[0xAA 0x55][9 body bytes]`).
const AM2_SERIAL_NONCE_LEN: usize = dcentrald_common::BM1362_SERIAL_NONCE_LEN;

/// `asic_job_id` step for the serial-dispatch path. The BM1362 echoes the sent
/// job id as `(sent << 1) & 0xF0` in the RESULT byte high nibble, so only bits
/// [6:3] of the sent id survive the round trip. Stepping by 8 keeps every
/// in-flight job in a distinct echoed slot (16 slots: 0, 8, .. 120).
const AM2_SERIAL_JOB_ID_STEP: u8 = dcentrald_common::DEFAULT_SERIAL_JOB_ID_STEP;

/// The echoed job-id slot the BM1362 reports for a given sent job id. Mirrors
/// `parse_bm1362_serial_nonce`'s `(result_byte & 0xF0) >> 1` recovery.
fn am2_serial_echoed_job_id(sent: u8) -> u8 {
    ((sent << 1) & 0xF0) >> 1
}

/// Build the proven 88-byte BM1362 serial-work wire frame from a Stratum
/// `MiningWork`. Reuses the accepted-share-proven codec
/// `dcentrald_asic::drivers::bm1362::build_serial_work_frame` (the same builder
/// the am3-bb `a lab unit` path and `serial_mining.rs` use) by adapting the Stratum
/// `MiningWork` into the asic-crate `MiningWork` it expects. The builder only
/// consumes `version` / `nbits` / `ntime` / `merkle_root` / `prev_block_hash`,
/// so the FPGA-only fields are filled with inert defaults.
fn build_am2_serial_work_frame(
    work: &dcentrald_stratum::share_pipeline::MiningWork,
    asic_job_id: u8,
) -> [u8; 88] {
    let asic_work = AsicMiningWork {
        work_id: 0,
        fpga_midstate_cnt: 0,
        version: work.version,
        nbits: work.nbits,
        ntime: work.ntime,
        merkle_tail: work.merkle4,
        midstates: Vec::new(),
        merkle_root: work.merkle_root,
        prev_block_hash: work.prev_block_hash,
    };
    build_serial_work_frame(&asic_work, asic_job_id)
}

const BM1362_PLL_TABLE: &[(u16, u32)] = &[
    (400, 0x50A0_0141),
    (412, 0x50A5_0141),
    (425, 0x50AA_0141),
    (437, 0x50AF_0141),
    (450, 0x50B4_0141),
    (462, 0x50B9_0141),
    (475, 0x50BE_0141),
    (487, 0x50C3_0141),
    (500, 0x50C8_0141),
    (512, 0x50CD_0141),
    (525, 0x50D2_0141),
    (537, 0x50D7_0141),
    (550, 0x50DC_0141),
    (562, 0x50E1_0141),
    (575, 0x50E6_0141),
    (587, 0x50EB_0141),
    (597, 0x50EF_0141),
];

fn bm1362_pll_lookup(target_mhz: u16) -> (u32, u16) {
    let target = target_mhz.clamp(400, 597);
    let mut best = BM1362_PLL_TABLE[0];
    let mut best_diff = (target as i32 - best.0 as i32).unsigned_abs();
    for &entry in &BM1362_PLL_TABLE[1..] {
        let diff = (target as i32 - entry.0 as i32).unsigned_abs();
        if diff < best_diff {
            best = entry;
            best_diff = diff;
        }
    }
    (best.1, best.0)
}

fn log_bm1362_voltage_topology(chain_id: u8, chip_count: u8) {
    match dcentrald_autotuner::voltage_domain::topology_for_chip(0x1362, chip_count, "dspic") {
        Some(topology) => {
            info!(
                chain_id,
                profile = topology.profile_key.as_str(),
                chips_per_chain = topology.chips_per_chain,
                domains_per_chain = topology.domains_per_chain,
                chips_per_domain = topology.chips_per_domain,
                controller = ?topology.controller,
                verified_from_re = topology.verified_from_re,
                "BM1362 voltage-domain topology"
            );
        }
        None => {
            warn!(
                chain_id,
                chip_count, "BM1362 voltage-domain topology unavailable for detected geometry"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PIC (Pic0x89) helpers — GET_VERSION without I2C_RDWR
// ---------------------------------------------------------------------------
//
// Keep the command write, response wait, and byte-wise reads inside one
// serialized I2C service transaction. This avoids I2C_RDWR while also avoiding
// APW heartbeat or thermal traffic interleaving halfway through the dsPIC
// response window. Probe both known 0x17 GET_VERSION encodings.
//
// Xilinx xiic bulk reads can return the first real byte followed by a synthetic
// shift-left tail. On the live XIL S19j Pro, framed GET_VERSION returned
// `[05, 0A, 14, 28, 50]`: `0x05` is the valid framed length byte, and the rest
// is bulk-read artifact. Read one byte per I2C transaction, matching Braiins'
// PIC helper, so the firmware byte is not lost behind kernel-generated tails.

fn am2_env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// Parse `name` as an explicit OFF switch: returns `true` only when the env var
/// is set to a recognised falsey value (`0`/`false`/`no`/`off`). Unset or any
/// other value returns `false`. Used by default-ON gates so they can be opted
/// OUT without flipping the default-OFF semantics of `am2_env_flag`.
fn am2_env_flag_off(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.as_str(),
                "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF"
            )
        })
        .unwrap_or(false)
}

fn bm1362_uart_relay_lab_enabled() -> bool {
    am2_env_flag("DCENT_BM1362_ENABLE_UART_RELAY_LAB")
}

fn am2_skip_fast_uart_enabled() -> bool {
    am2_env_flag("DCENT_AM2_SKIP_FAST_UART")
}

fn am2_fast_uart_switch_host_first_enabled() -> bool {
    am2_env_flag("DCENT_AM2_FASTUART_SWITCH_HOST_FIRST")
}

/// PERF/T1 — opt-in BM1362 factory-jig baud method (default-OFF).
///
/// When set, the fast-baud switch uses the chip's OWN `set_chain_baud`
/// procedure decoded first-hand from the AMTC S19j Pro repair-jig
/// `single_board_test` (BM1362, `FUN_0002cb14`): a register read-modify-write
/// that, at baud >= 3,000,001, reclocks the command UART off PLL1 (reg `0x60`,
/// ×2 @ 10 ms) and takes the reg `0x28` divider from 400 MHz — instead of
/// DCENT's fixed `reg 0x28 = 0x3011` + `reg 0x18` MiscCtrl (which never writes
/// reg `0x60` and measures `0/126` at 3.125 M). A/B candidate for the BM1362
/// fast-baud zero-nonce blocker. Reversible per run; writes only ASIC config
/// registers (no voltage/PSU/PIC/thermal). See
/// `dcentrald_asic::drivers::bm1362::jig_pll1_reclock_regs`.
fn am2_baud_jig_pll1_reclock_enabled() -> bool {
    am2_env_flag("DCENT_AM2_BAUD_JIG_PLL1_RECLOCK")
}

/// Read one BM1362 chip register value via a broadcast READ at the current
/// (115200) baud. Returns `None` if the chain UART RX is silent — the caller
/// MUST treat that as fail-closed and abort before issuing any write, so the
/// chain is never left half-reclocked. Mirrors the existing PLL-lock readback
/// pattern (`send_read_reg_bm1397plus` + `read_all_responses` + `from_be_bytes`).
fn read_bm1362_reg_value(serial: &SerialChainBackend, chip_addr: u8, reg: u8) -> Option<u32> {
    serial.send_read_reg_bm1397plus(chip_addr, reg).ok()?;
    let resps = serial.read_all_responses(50).unwrap_or_default();
    for r in &resps {
        if r.len() >= 4 {
            return Some(u32::from_be_bytes([r[0], r[1], r[2], r[3]]));
        }
    }
    None
}

/// Apply the BM1362 factory-jig `set_chain_baud` (PLL1 reclock RMW) at the
/// current baud, faithful to the AMTC repair-jig binary (`FUN_0002cb14`). Gated,
/// default-OFF. Fail-closed: if a chip register readback is silent, abort BEFORE
/// any write so the chain is never left half-configured. The jig's
/// `set_chain_baud` does NOT touch reg `0x18` MiscCtrl, so the caller skips the
/// MiscCtrl-post-fast write while this gate is on.
fn apply_bm1362_jig_pll1_reclock(serial: &SerialChainBackend, target_baud: u32) -> Result<()> {
    let r60 = read_bm1362_reg_value(serial, 0x00, 0x60).context(
        "jig-reclock: BM1362 reg 0x60 (PLL1) readback silent at 115200 — \
         aborting before any baud-register write (fail-closed; chain untouched)",
    )?;
    let r28 = read_bm1362_reg_value(serial, 0x00, 0x28).context(
        "jig-reclock: BM1362 reg 0x28 (FastUART) readback silent at 115200 — \
         aborting before any baud-register write (fail-closed; chain untouched)",
    )?;
    let (w60, w28) = jig_pll1_reclock_regs(r60, r28, target_baud);
    info!(
        reg60_readback = format_args!("0x{:08X}", r60),
        reg28_readback = format_args!("0x{:08X}", r28),
        reg60_write = w60
            .map(|v| format!("0x{:08X}", v))
            .unwrap_or_else(|| "(none, low-baud path)".to_string()),
        reg28_write = format_args!("0x{:08X}", w28),
        target_baud,
        "DCENT_AM2_BAUD_JIG_PLL1_RECLOCK=1 — applying BM1362 factory-jig set_chain_baud (PLL1 reclock RMW); reg 0x18 MiscCtrl skipped (jig does not write it in set_chain_baud)"
    );
    if let Some(w60) = w60 {
        // reg 0x60 = PLL1, written twice @ 10 ms (jig order).
        serial.send_write_reg_broadcast_bm1397plus(0x60, w60)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x60, w60)?;
        std::thread::sleep(Duration::from_millis(10));
    }
    serial.send_write_reg_broadcast_bm1397plus(0x28, w28)?;
    serial.drain_tx().ok();
    // Jig timing: usleep(10ms) + usleep(50ms) after the reg 0x28 write.
    std::thread::sleep(Duration::from_millis(60));
    Ok(())
}

fn am2_work_tx_bosminer_idle_enabled() -> bool {
    am2_env_flag("DCENT_AM2_WORK_TX_BOSMODE")
}

// ---------------------------------------------------------------------------
// A36 (knowledge-goldmine s14 F16/F23-F25, IC-5): FPGA-bitstream transport note.
//
// The `/dev/ttyS1` (MMIO 0x41001000) + `/dev/ttyS3` (0x41021000) PL UARTs that
// DCENT_OS uses for BM1362 chain I/O are AXI UART 16550 cores that exist ONLY in
// the BraiinsOS FPGA bitstream — the STOCK Bitmain devicetree has NO nodes at
// those addresses (s14 F25). Stock Bitmain instead drives all chain UART through
// `bitmain_axi.ko` -> `/dev/axi_fpga_dev` -> `ioremap(0x40000000, 0x1400)`
// (s14 F16-F18/F23-F24), which DCENT does NOT load. So the ttyS1/ttyS3 path here
// is valid ONLY because `a lab unit`/`a lab unit` boot the BraiinsOS-inherited bitstream; on a
// stock-bitstream board those devices are absent and the 0x40000000 AXI UART is
// the only chain transport. This is why a `a lab unit` standalone bring-up must keep the
// BraiinsOS bitstream and MUST NOT fall back to bitmain_axi.ko. Documentation
// only — no transport behavior change. Source:
//
// ---------------------------------------------------------------------------

///  (2026-05-23): route the BM1362 chain init through the FPGA
/// FIFO IP blocks (`chain1-common` / `chain1-cmd-rx` / `chain1-work-rx`
/// / `chain1-work-tx` UIO devices at `0x43C0Nxxx`) instead of the PL
/// UART at `0x41001000`. Default-OFF — `a lab unit`'s 2026-05-15 first-shares
/// path stays byte-identical when the flag is unset.
///
/// Set `DCENT_AM2_USE_FPGA_CHAIN=1` to opt in. Phase-1 wiring is
/// skeleton-only (init_asic_chain still uses SerialChainBackend); Phase
/// 2 wires the real `FpgaChainBackend::initialize_chain_for_bm1362`
/// sequence (BUILD_ID check, CTRL preserve-or-write 0x00901002, BAUD
/// 0x6C, FIFO reset).
///
/// Live-evidence rationale: `a lab unit`'s BraiinsOS bitstream wires the chain
/// UART through the FPGA FIFO IP blocks (`/dev/uio0..3` for chain 0,
/// `/dev/uio4..7` for chain 1). Bosminer on `a lab unit` opens NO `/dev/ttyS*`
/// device — strong signal that the PL UART path is wrong for this
/// bitstream. See
/// .
fn am2_use_fpga_chain_enabled() -> bool {
    am2_env_flag("DCENT_AM2_USE_FPGA_CHAIN")
}

/// Opt-OUT of running the BM1362 per-chip init loop on the 115200
/// skip-FastUART path. Default behaviour (flag unset) is to RUN the per-chip
/// loop at 115200 — the proven BM1362 paths all do, and skipping it left the
/// cores broadcast-only-activated (suspected `DCENT_AM2_SKIP_FAST_UART`
/// zero-nonce cause). Set this only to restore the pre-fix behaviour for an
/// A/B comparison.
fn am2_skip_115200_per_chip_enabled() -> bool {
    am2_env_flag("DCENT_AM2_SKIP_115200_PER_CHIP")
}

/// `DCENT_AM2_BM1362_RE018_COLD_SEQUENCE=1` (RE-018, 2026-05-31).
///
/// Replace `init_asic_chain`'s 115200 chain bring-up with the BYTE-EXACT
/// bosminer cold-wake sequence decoded from `/data/re018-cold-strace.log`
/// (a strace of bosminer COLD-engaging the `a lab unit` BM1362 chain on /dev/ttyS1).
/// This replicates the exact register VALUES and ORDER bosminer uses — most
/// notably the per-chip reg 0x0C nonce-space base write DCENT was missing, the
/// per-chip 0xA8=0x00000002 / 0x18=0xB000C100 values, and the single LATE
/// GetAddress poll (vs DCENT's per-chip GetAddress bail). See
/// `bm1362_re018_cold_sequence` for the full decode.
///
/// Default-OFF + AM2-Zynq/BM1362 recipe-gated. When the flag is unset, or
/// neither the `a lab unit` fingerprint nor the explicit sibling recipe proof matches,
/// `init_asic_chain` is byte-identical to today's behaviour. `a lab unit` only enters
/// this path after its own class-recipe env plus `a lab unit` proof env are set.
fn am2_bm1362_re018_cold_sequence_enabled() -> bool {
    am2_env_flag("DCENT_AM2_BM1362_RE018_COLD_SEQUENCE") && am2_zynq_bm1362_recipe_gate_matches()
}

/// `DCENT_PREFER_KERNEL_UART` is the single source of truth for "the chain runs
/// on bosminer's exact kernel `of_serial` transport (File backend, selection=
/// kernel-open), NOT DCENT's DevmemUart". The HAL reads this same env in
/// `SerialChainBackend::needs_devmem`. Used here to GUARD the Fix-C `/dev/uio3`
/// (chain1-work-tx) open, which ENODEVs after the work-tx UIO is unbound to free
/// IRQ 165 (see below).
fn am2_prefer_kernel_uart() -> bool {
    std::env::var_os("DCENT_PREFER_KERNEL_UART").is_some()
}

/// .25 RE-018 KERNEL-UART TRANSPORT (2026-06-09, LIVE TEST 13). On `a lab unit` the Zynq
/// PL UART `ttyS1` (16550A @ 0x41001000) shares GIC 62 / Linux IRQ 165 with the
/// `43c03000.chain1-work-tx` UIO. While `uio_pdrv_genirq` holds that IRQ, the
/// kernel `of_serial` 8250 port cannot request it, so a File-backed `/dev/ttyS1`
/// open fails and DCENT falls back to DevmemUart (a second master on the same
/// 16550 = RX-steal risk). Unbinding the work-tx platform device frees IRQ 165 ->
/// `of_serial` claims `ttyS1` -> `selection=kernel-open` (bosminer's literal,
/// single-owner transport; 8250 port-startup asserts MCR OUT2 itself).
///
/// SAFE only because `DCENT_AM2_SERIAL_WORK_DISPATCH=1` BYPASSES the FPGA work-tx
/// FIFO entirely, so chain1-work-tx is UNUSED on this path. On success this sets
/// `DCENT_PREFER_KERNEL_UART=1` for the process so the chain open selects the
/// kernel File backend (atomic with a SUCCESSFUL unbind - no half-state).
///
/// Triple-gated default-OFF: explicit opt-in env
/// `DCENT_AM2_KERNEL_UART_UNBIND_WORKTX=1` AND the `a lab unit` fingerprint AND
/// serial-work-dispatch mode. Idempotent: if the work-tx platform device is
/// already unbound, set `DCENT_PREFER_KERNEL_UART=1` and keep the kernel path.
fn am2_free_chain1_work_tx_irq_for_kernel_uart() {
    if !(am2_env_flag("DCENT_AM2_KERNEL_UART_UNBIND_WORKTX")
        && am2_zynq_bm1362_recipe_gate_matches()
        && am2_env_flag("DCENT_AM2_SERIAL_WORK_DISPATCH"))
    {
        return;
    }
    const UNBIND: &str = "/sys/bus/platform/drivers/uio_pdrv_genirq/unbind";
    const DEV: &str = "43c03000.chain1-work-tx";
    const DRIVER_LINK: &str = "/sys/bus/platform/devices/43c03000.chain1-work-tx/driver";
    if !std::path::Path::new(DRIVER_LINK).exists() {
        std::env::set_var("DCENT_PREFER_KERNEL_UART", "1");
        info!(
            device = DEV,
            "RE-018 kernel-UART: chain1-work-tx already unbound; set DCENT_PREFER_KERNEL_UART=1"
        );
        return;
    }
    match std::fs::write(UNBIND, DEV) {
        Ok(()) => {
            std::env::set_var("DCENT_PREFER_KERNEL_UART", "1");
            info!(
                device = DEV,
                "RE-018 kernel-UART: unbound chain1-work-tx from uio_pdrv_genirq (freed IRQ 165); \
                 set DCENT_PREFER_KERNEL_UART=1 -> chain opens via kernel of_serial (selection=kernel-open)"
            );
        }
        Err(e) => {
            if !std::path::Path::new(DRIVER_LINK).exists() {
                std::env::set_var("DCENT_PREFER_KERNEL_UART", "1");
                info!(
                    device = DEV,
                    error = %e,
                    "RE-018 kernel-UART: unbind returned an error but driver link is gone; set DCENT_PREFER_KERNEL_UART=1"
                );
            } else {
                warn!(
                    device = DEV,
                    error = %e,
                    "RE-018 kernel-UART: chain1-work-tx unbind write failed and driver remains bound; \
                     leaving transport selection unchanged (DevmemUart fallback)"
                );
            }
        }
    }
}

/// `DCENT_AM2_CONTINUE_PAST_ZERO_ENUM=1` (gap-swarm 2026-05-29, H5 diagnostic):
/// downgrade the Phase-4c "0 chips responded to GetAddress" hard-bail to a
/// warning and continue into the PLL/baud-upgrade + the later GetAddress
/// re-probes. Tests the chicken-and-egg hypothesis that a BM1362 at POR
/// (~50 MHz) cannot drive a clean 115200 UART reply until its per-chip PLL
/// block is programmed — the proven `a lab unit` am3-bb path never gates on the
/// GetAddress count, and the `a lab unit` Amlogic BM1368 driver never
/// GetAddress-enumerates at all, so neither working path depends on a
/// pre-clock enum succeeding. DIAGNOSTIC ONLY, default-OFF: when unset the
/// hard-bail is preserved exactly. 0 chips means no hashing (no thermal risk);
/// the run will still surface `tx_full=true` unless a post-clock re-probe
/// finds chips. See docs/PROJECT_LOG.md cont.21 + the gap-swarm `a lab unit` diagnosis.
fn am2_continue_past_zero_enum_enabled() -> bool {
    am2_env_flag("DCENT_AM2_CONTINUE_PAST_ZERO_ENUM")
}

/// `DCENT_AM2_SKIP_HOTSTART_BAUD_RESET=1` (swarm wf_e0647147 H-skip-baud-reset,
/// 2026-05-29) — skip the `reset_asic_baud()` pre-enum step.
///
/// `reset_asic_baud()` is a HOT-restart helper: it opens the chain UART at
/// 3.125M then 1.5625M baud and sends ChainInactive + MiscCtrl, on the
/// assumption the chain may still be sitting at a high baud from a prior
/// bosminer / DCENT_OS run. On a TRUE cold-boot standalone (`a lab unit` after an
/// AC-cycle, chain at the 115200 default) that high-baud traffic is seen by
/// the 115200 chips as line garbage and can leave the chain UART perturbed
/// right before the first enum — a candidate contributor to the standalone
/// `enum=0`. This gate lets the cold-boot standalone launchers skip it.
///
/// Default-OFF → the proven hot-restart / bosminer-handoff path
/// (`run_wave54_25_PROVEN_MINING.sh`, which restarts after bosminer left the
/// chain at fast baud) is byte-for-byte unchanged. Only set it on a verified
/// cold-boot standalone run.
fn am2_skip_hotstart_baud_reset_enabled() -> bool {
    am2_env_flag("DCENT_AM2_SKIP_HOTSTART_BAUD_RESET")
}

/// `DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS=1` (swarm wf_e0647147 H-heartbeat-0x22,
/// 2026-05-29) — heartbeat EVERY active dsPIC after ENABLE, not just the
/// selected one.
///
/// The post-ENABLE 1 Hz heartbeat is spawned for `selected_pic_addr` only
/// (default 0x20). On the `a lab unit` standalone path the EFFECTIVE chain dsPIC is
/// 0x22 (slot 3, the chain UART routed via `DCENT_AM2_CHAIN_UART_OVERRIDE`),
/// so 0x22's voltage controller can hit its ~30 s no-heartbeat watchdog and
/// cut slot-3's rail BEFORE the first BM1362 enum — a candidate root cause of
/// the standalone `enum=0`. When set, the heartbeat thread also keepalives the
/// other active dsPICs (incl. the effective chain dsPIC 0x22) using the same
/// 1 Hz cadence and the same heartbeat command — no new protocol. The thread
/// is still spawned in Phase 3d, immediately after ENABLE (< 10 s), so no rail
/// is left un-heartbeated through the post-ENABLE settle window.
///
/// Default-OFF → the proven fleet (`a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/s9) and the
///  `a lab unit` bosminer-handoff path keep heartbeating ONLY
/// `selected_pic_addr`, byte-for-byte unchanged on the wire. Only the wave55
/// standalone launchers set it.
fn am2_heartbeat_all_active_pics_enabled() -> bool {
    am2_env_flag("DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS")
}

/// `DCENT_AM2_ABLATION_PER_CHIP_STOP_AFTER={a8|miscctrl}` (Sweep-v3 G2 /
/// R10-2/R11-2 ablation). Default-OFF additive instrumentation that
/// TRUNCATES the per-chip `bm1362_per_chip_fast_init` loop after a named
/// substep so the EXISTING safe between-phase `post_per_chip_loop_115200`
/// probe (PR-019, gated by `DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE`)
/// can bisect WHICH per-chip substep collapses the chain's 126→1+62 chip
/// addressing — WITHOUT injecting any new chain traffic *inside* the
/// tight init loop (a GetAddress probe mid-loop would perturb the very
/// addressing it measures; this truncation reads out via the existing
/// post-loop probe instead).
///
/// - `a8`       → per chip do ONLY the `0xA8` write.
/// - `miscctrl` → per chip do `0xA8` + MiscCtrl×3, skip the `0x3C×3`.
/// - unset/other → run the FULL loop. **DEFAULT — byte-for-byte
///   unchanged behaviour, timing, and on-wire traffic. This is the
///   load-bearing no-op contract, identical to PR-019's.**
///
/// Operator ablation (Loki-IN, quiet fan≤30, `/tmp`, AC-cycle-gated):
/// run the proven milestone command WITHOUT
/// `DCENT_AM2_SKIP_115200_PER_CHIP`, WITH
/// `DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE=1`, three passes —
/// `=a8`, `=miscctrl`, unset(full) — and diff the
/// `post_per_chip_loop_115200` `[AM2-ABLATION]` `unique_chip_ids`: the
/// first pass whose value COLLAPSES names the culprit substep, which
/// directly determines the G2 fix.
fn am2_ablation_per_chip_stop_after() -> Option<&'static str> {
    match std::env::var("DCENT_AM2_ABLATION_PER_CHIP_STOP_AFTER")
        .ok()
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("a8") => Some("a8"),
        Some("miscctrl") => Some("miscctrl"),
        _ => None,
    }
}

/// Route work dispatch through the proven BM1362 88-byte serial work frame
/// over the chain UART (`DevmemUart`) instead of the am2 FPGA WORK_TX FIFO.
/// Reuses the exact codec (`build_serial_work_frame` / `parse_bm1362_serial_nonce`)
/// proven on the am3-bb `a lab unit` accepted-share milestone. Bypasses the
/// long-standing am2 FPGA-FIFO zero-nonce question entirely. Default-off.
fn am2_serial_work_dispatch_enabled() -> bool {
    am2_env_flag("DCENT_AM2_SERIAL_WORK_DISPATCH")
}

/// 2026-06-02 (W8 production-parity drive, GROUP B) — opt-in env gate for the
/// `a lab unit` DUAL-CHAIN serial-dispatch capability.
///
/// W8 parity gap: BraiinsOS / VNish / stock all mine BOTH hashboards on `a lab unit`
/// (PL UART 0 + PL UART 2 → `/dev/ttyS1` + `/dev/ttyS3`), but DCENT_OS today
/// dispatches work to chain 1 ONLY (the first planned chain context / the
/// `serial_device`-derived chain). That leaves ~50 % of the unit's hashrate on
/// the table. When the operator sets `DCENT_AM2_DUAL_CHAIN_TTYS3=1`, the
/// serial-work-dispatch route brings up a SECOND `SerialChainBackend` on
/// `/dev/ttyS3` (dsPIC 0x22, slot 3) in addition to the proven primary chain
/// and runs a dual-chain dispatch loop that pushes work to BOTH chains and
/// collects/attributes nonces from BOTH with per-chain `WorkEntry` history,
/// per-chain dedup, and per-chain BIP320 share reconstruction.
///
/// **DEFAULT-OFF — gate-off is byte-identical to the proven single-chain
/// `run_am2_serial_dispatch_loop` path.** When unset, `run()` never opens the
/// second UART, never spawns the second chain, and calls the exact same
/// single-chain loop as before. The EXACT 2nd-chain cold bring-up ORDER/TIMING
/// is RE-ASK-XIL-6 (not byte-confirmed against a live bosminer dual-chain
/// strace yet), so this is a CAPABILITY pending operator live-validation: it
/// reuses the proven `init_asic_chain` bring-up for the second chain, but
/// whether both chains can be cold-brought-up back-to-back from one daemon
/// without a fresh AC-cycle is unproven. Flipping the default ON is a SEPARATE
/// operator step.
fn am2_dual_chain_ttys3_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DUAL_CHAIN_TTYS3")
}

/// Canonical second-chain UART for the `a lab unit` dual-chain capability. `a lab unit` has
/// hashboards on PL UART 0 (`/dev/ttyS1`, slot 0, dsPIC 0x20) and PL UART 2
/// (`/dev/ttyS3`, slot 2, dsPIC 0x22) — the absent middle slot maps to ttyS2.
/// The operator can override via `DCENT_AM2_DUAL_CHAIN_SECOND_UART` (defense in
/// depth — a future topology with a different second slot stays expressible
/// without a code change). Default is the `a lab unit`-proven `/dev/ttyS3`.
const AM2_DUAL_CHAIN_SECOND_UART_DEFAULT: &str = "/dev/ttyS3";

fn am2_dual_chain_second_uart() -> String {
    std::env::var("DCENT_AM2_DUAL_CHAIN_SECOND_UART")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| AM2_DUAL_CHAIN_SECOND_UART_DEFAULT.to_string())
}

/// RANK-5 (2026-06-13, "make-it-work" + live operator signal). The cold test of
/// WAKE-DSPIC-BEFORE-RAIL delivered power (operator confirmed the hashboard WARMS)
/// but enum stayed 0 with zero RX — so the residual is TRANSPORT, not power.
/// bosminer's cold block-B opens BOTH `/dev/ttyS1` AND `/dev/ttyS3` before enum;
/// DCENT-standalone opens only ttyS1. On `a lab unit` the two PL-UART cores
/// (`41001000.uart` + `41021000.uart`) may share a chain-TX-clock / reset gate in
/// the FPGA fabric, so with only ttyS1 open the chips are powered (board warms)
/// but their TX clock stays gated -> 0 RX. When set, `init_asic_chain` opens the
/// companion ttyS3 (unbind its `chain3-work-tx` IRQ, open, assert OUT2) and HOLDS
/// it open through the primary enum. Default-OFF + `a lab unit`-fingerprinted (fleet /
///  handoff / `a lab unit` / `a lab unit` byte-identical unless the explicit sibling
/// class-recipe proof is set). Transport-layer, so valid on a warm dsPIC (no
/// AC-cycle needed to test).
fn am2_open_both_uarts_before_enum_enabled() -> bool {
    am2_env_flag("DCENT_AM2_OPEN_BOTH_UARTS_BEFORE_ENUM") && am2_zynq_bm1362_recipe_gate_matches()
}

/// 2026-05-22 (XIL `a lab unit` recovery, Layer 1) — opt-in env gate for the
/// bosminer-faithful PIC reset+start-app warmup.
///
/// When the operator sets `DCENT_AM2_PIC_RESET_AND_START_APP=1`, Phase 0d
/// emits the `bosminer_warmup::am2_pic_reset_and_start_app_bosminer_faithful`
/// chain (16-byte parser flush + `[55 AA 07]` + 500 ms + `[55 AA 06]` + 100 ms)
/// just before Phase 1's first GET_VERSION.
///
/// **Default off so the first commit stays byte-identical to today's `a lab unit`
/// behaviour.** The `[mining].am2_dspic_warmup_before_get_version` TOML knob
/// is ALSO required (default true) — both must be true for the prelude to
/// actually run. The double-gate is intentional: the config knob is the
/// "is this feature on for this fleet at all?" switch, the env gate is the
/// "I'm explicitly green-lighting this on THIS unit for an A/B run" switch.
/// Promote to env-off-by-default after `a lab unit` validation succeeds; the config
/// knob then becomes the sole gate.
fn am2_pic_reset_and_start_app_enabled() -> bool {
    am2_env_flag("DCENT_AM2_PIC_RESET_AND_START_APP")
}

/// 2026-05-24 () — opt-in env gate for the STANDALONE Loki spoof
/// cold-boot bring-up path on `a lab unit`-class XIL units.
///
/// When `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1`, Phase 0 calls
/// `Apw121215a::cold_boot_sequence_loki_standalone` instead of the
/// bosminer-handoff path's `cold_boot_sequence_write_only`. The standalone
/// path emits the  captured cold-wake byte sequence
/// (init-frame + poll + follow-up-frame + poll, up to 4 cycles) BEFORE
/// the standard 3× Disable + Ramp + Enable body, so the Loki spoof can
/// engage from a true cold AC-cycled state WITHOUT bosminer pre-engaging
/// the chip rail.
///
/// **Fleet safety**: this env is ADDITIVE to the  PROVEN MINING
/// RECIPE (NOT in the 4-forbidden list at `wave55a_recipe_guard.rs`).
/// Default OFF — unset env → original behavior preserved byte-identically.
/// Only fires when ALL of the following hold:
///   1. `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1` is set.
///   2. PSU transport == `gpio_bitbang` (Loki spoof is the only documented
///      gpio_bitbang PSU on the fleet — `a lab unit` BB / `a lab unit` XIL / `a lab unit`
///      S19 Pro all use kernel-i2c or no-PSU paths).
///   3. The `a lab unit`-class hardware fingerprint matches (platform ==
///      `zynq-bm3-am2` AND board_target ends with `xil`).
///   4. `DCENT_AM2_TRUST_RAIL_FALLBACK != 1` — operator is NOT asking
///      for the bosminer-handoff fallback (the two paths are mutually
///      exclusive: handoff requires the chip rail already engaged by
///      bosminer; standalone tries to engage it without bosminer).
///
///
/// for the  →  byte mapping and the test plan.
fn am2_psu_loki_cold_boot_full_enabled() -> bool {
    am2_env_flag("DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL")
}

/// 2026-05-25 () — opt-in umbrella env gate for the Phase 2c RE
/// finding fix on `a lab unit`-class XIL hardware (DCENT_OS-from-NAND
/// standalone cold-boot).
///
/// **RE finding (`PHASE2C-DSPIC-RAIL-FAILURE-RE.md`):** bosminer's
/// healthy cold-boot trace to dsPIC slave 0x20 contains ZERO `0x10`
/// (SetVoltage) and ZERO `0x15` (ENABLE_VOLTAGE) opcodes. The
/// chip-rail engagement actually happens through:
///   1. The Loki spoof's `0x83` SetVoltageStep opcode (per
///      `PHASE2B-APW12-PIC-PROTOCOL.md`) at the *PWM-DAC layer* — this
///      is what `` adds to `cold_boot_sequence_loki_standalone`.
///   2. The 17-transaction LM75A passthrough sequence (dsPIC opcodes
///      `0x3B`/`0x3C` on sensor addresses `0x48..0x4B`) that warms the
///      dsPIC's MSSP I²C handler state machine — this is what
///      `am2_dspic_lm75_passthrough_warmup` emits.
///
/// When `DCENT_AM2_STANDALONE_RE_FIX=1` AND the `a lab unit` fingerprint
/// matches AND the  standalone Loki cold-boot path is also
/// engaged (`DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1`), Phase 0:
///   1. Runs `cold_boot_sequence_loki_standalone` (which now emits
///      Loki SetVoltage(13700) at end — per change 2 above).
///   2. Runs the 17-tx LM75A passthrough warmup.
///   3. Preserves the standard dsPIC `cold_boot_init` call.  live
///      testing falsified the  skip hypothesis: skipping SetVoltage +
///      ENABLE_VOLTAGE regressed standalone enum to 0/126, while the prior
///      live-proven path with `cold_boot_init_with_options` engaged chips.
///   4. Proceeds through the normal Phase 3 rail-engage path, then Phase 4-7
///      chain enum on `/dev/ttyS1`.
///
/// **Fleet safety**: default-OFF — when unset, all paths preserve
/// byte-identical behavior. Even when set, takes no effect unless the
/// `a lab unit` fingerprint matches AND the Loki cold-boot path is also
/// engaged (compound gate).
///
/// Historical context:
/// recommended the  skip as Option A;  reverted that portion
/// after live evidence showed it regressed rail engagement.
fn am2_standalone_re_fix_enabled() -> bool {
    am2_env_flag("DCENT_AM2_STANDALONE_RE_FIX")
}

/// `DCENT_AM2_FPGA_UART_RELAY_COLD=1` (2026-06-11, LIVE-PINNED) — enable the
/// FPGA UART **return** relay on the am2 `gpio@41220000` AXI-GPIO BEFORE the
/// chain enum walk, so the BM1362 daisy-chain RETURN line (chip RO →
/// chip-id/enum/nonce frames) reaches the PL soft-UART RX.
///
/// H1 candidate for `a lab unit` standalone enum=0 (multi-agent RE + live probe
/// 2026-06-11): the RETURN line is gated by a 2-bit AXI-GPIO at `0x41220000`
/// (bit0 `co_relay_en`, bit1 `ro_relay_en`) — the ONLY `/dev/mem` map bosminer
/// makes. bosminer drives it to `DATA=0x3` / `TRI=0x0` (both bits output,
/// driven); DCENT's `a lab unit` standalone path never mapped it, so the cold-boot
/// reset default (`tri=0xffffffff` = input/floating = relay OFF) left the
/// return path open → commands out, **0 RX**. See
/// [`dcentrald_hal::fpga_uart_relay`]. This was the H1 candidate from
/// ; keep the
/// EBR choreography + `0x15` ENABLE (bosminer/luxminer both send ENABLE — the
/// "withhold-0x15" idea was falsified).
///
/// LIVE FALSIFICATION BOUNDARY: the v+2 run drove this GPIO to the bosminer
/// state and confirmed readback (`relay_confirmed=true`), but enum still stayed
/// 0 with board-cold / LM75 unavailable. Keep this enable because it matches
/// bosminer and removes a confound, but do NOT treat it as the complete root
/// cause. The remaining gap is downstream chip-physical rail/clock or another
/// bosminer mmap/register state not captured by strace.
///
/// Address ambiguity: `psu_gpio_i2c.rs` also pins `0x41220000` as the
/// gpio895/896 PSU SMBus bank. Do not infer from the address alone that this is
/// a dedicated chain-return relay; the live discriminator remains the
/// bosminer-engaged vs DCENT-standalone register diff.
///
/// Default-OFF + AM2-Zynq/BM1362 recipe gate + NOT-handoff
/// (`!DCENT_AM2_TRUST_RAIL_FALLBACK`,
/// since the  handoff inherits the relay bosminer already latched). It
/// is intentionally NOT in `WAVE54_FORBIDDEN_ENV_VARS` (it touches no dsPIC) —
/// adding it there would block the cold path it enables. Byte-identical on
/// `a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9/handoff and whenever the env is unset.
fn am2_fpga_uart_relay_cold_enabled() -> bool {
    am2_env_flag("DCENT_AM2_FPGA_UART_RELAY_COLD")
        && am2_zynq_bm1362_recipe_gate_matches()
        && !am2_env_flag("DCENT_AM2_TRUST_RAIL_FALLBACK")
}

/// Gated, fail-closed when enabled, **idempotent** FPGA UART return-relay enable. No-op unless
/// [`am2_fpga_uart_relay_cold_enabled`]. Safe to call multiple times — the
/// underlying AXI-GPIO write at `0x41220000` is an idempotent read-modify-write
/// (DATA bits[1:0]=0b11, TRI bits[1:0]=0b00). It is called at BOTH the
/// pre-rail-evidence-probe point AND the pre-enum point: the relay MUST be up
/// before `post_enable_chain_uart_probe` so that probe's `rx_bytes_pre_init`
/// can be a true rail+transport signal (otherwise the RETURN line is physically
/// open and the probe is a structurally-guaranteed 0-RX false negative —
/// DCENT_FPGA review 2026-06-11), AND before the SET_ADDRESS enum walk.
/// `stage` labels the call site in the log. When the env gate is set, readback
/// failure aborts before enum.
fn am2_try_enable_fpga_uart_relay_cold(stage: &str) -> Result<()> {
    if !am2_fpga_uart_relay_cold_enabled() {
        return Ok(());
    }
    match dcentrald_hal::fpga_uart_relay::enable_am2_uart_relay_cold() {
        Ok(r) => {
            let relay_confirmed = r.relay_confirmed();
            info!(
                stage,
                env = "DCENT_AM2_FPGA_UART_RELAY_COLD=1",
                data_pre = format_args!("0x{:08X}", r.data_pre),
                tri_pre = format_args!("0x{:08X}", r.tri_pre),
                data_post = format_args!("0x{:08X}", r.data_post),
                tri_post = format_args!("0x{:08X}", r.tri_post),
                relay_confirmed,
                "RE-018 relay: FPGA UART return-relay enabled (gpio@41220000 \
             co_relay_en+ro_relay_en) — expect data_post bits[1:0]=0b11, \
             tri_post bits[1:0]=0b00 (matches live bosminer)"
            );
            if !relay_confirmed {
                anyhow::bail!(
                    "RE-018 relay: FPGA UART return-relay readback not confirmed at {stage}"
                );
            }
        }
        Err(e) => {
            anyhow::bail!("RE-018 relay: FPGA UART return-relay enable failed at {stage}: {e}");
        }
    }
    Ok(())
}

/// `DCENT_AM2_BOARD_CONTROL_BIT8=1` (W2 register-diff, 2026-06-14) — gate for the
/// board-control `+0x04` bit 8 set. Default-OFF + `a lab unit`-fingerprinted: the live
/// paired `dump_fpga_regs_25.sh` diff proved bosminer-engaged `+0x04 = 0x134`
/// while DCENT standalone enum=0 reads `0x034` — bit 8 (`0x100`) is the only
/// persistent-fabric register DCENT never matches. Default-OFF + `a lab unit` fingerprint
/// ⇒ byte-identical for `a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9 and the  handoff
/// (which inherits the value bosminer latched). `a lab unit` requires its explicit
/// class-recipe proof envs. Touches no dsPIC, so it is NOT a  forbidden
/// var.
fn am2_board_control_bit8_enabled() -> bool {
    am2_env_flag("DCENT_AM2_BOARD_CONTROL_BIT8") && am2_zynq_bm1362_recipe_gate_matches()
}

/// `DCENT_AM2_RE018_WRITE_TICKET_HASHCOUNT=1` (perf audit, 2026-06-14) — gate to
/// add the TICKET_MASK (reg 0x14) + HASH_COUNTING_NUMBER (reg 0x10) broadcast
/// writes to the RE-018 cold sequence. The RE-018 path replays a bosminer strace
/// that captured only the FIRST init cycle (ending at the per-chip 0x0C write);
/// bosminer writes 0x14/0x10 in a LATER phase the strace missed. Without 0x14 the
/// chip drops EVERY nonce at the hardware difficulty filter (~silent zero
/// production); without 0x10 all 126 chips hash the SAME nonce range. This is the
/// root cause of the ~1000x-low `a lab unit` standalone hashrate (even the
/// "proven" run was 5 nonces/165s). The legacy init path already writes both
/// (Step 4b). POWER-NEUTRAL: same freq/voltage — the cores already hash; this
/// only lets them REPORT nonces at the correct rate + distribute the range.
/// Default-OFF + AM2-Zynq/BM1362 recipe gate ⇒ fleet stays byte-identical unless
/// a sibling unit supplies its explicit class-recipe proof envs; the
/// handoff stays byte-identical.
fn am2_re018_write_ticket_hashcount_enabled() -> bool {
    am2_env_flag("DCENT_AM2_RE018_WRITE_TICKET_HASHCOUNT") && am2_zynq_bm1362_recipe_gate_matches()
}

/// `DCENT_AM2_RE018_FULL_CORE_INIT=1` (perf audit, 2026-06-14) — gate to swap
/// the RE-018 cold sequence's INIT_CONTROL (reg 0xA8) writes from the
/// strace-captured cores-IDLE values
///   broadcast 0x0000_0000 / per-chip 0x0000_0002
/// to the `a lab unit` ACCEPTED-SHARE-PROVEN core-ENABLE values
///   broadcast 0x0007_0000 / per-chip 0x0007_01F0  (BM1362_INIT_PLAN)
/// AND drop the PLL (reg 0x08) to a LOW [`RE018_LOW_FREQ_PLL_08`] (150 MHz) so
/// the now-active cores stay within the ~550 W home breaker budget (un-idling
/// at the 525 MHz default would draw ~1.5 kW = trip). The 0x0007 high bits are
/// the core/clock enable cluster the RE-018 capture's first cycle omitted; a
/// prior live ticket-mask A/B proved the idle is UPSTREAM of nonce reporting
/// (= core enable), so this is the load-bearing fix for the ~400x-low useful
/// hashrate. Default-OFF + AM2-Zynq/BM1362 recipe gate ⇒ fleet stays
/// byte-identical unless a sibling unit supplies its explicit class-recipe proof
/// envs; the  handoff stays byte-identical.
/// POWER-AWARE: enabling cores REQUIRES the low PLL — the two are coupled in
/// this one gate so cores can never be un-idled at the high freq by mistake.
fn am2_re018_full_core_init_enabled() -> bool {
    am2_env_flag("DCENT_AM2_RE018_FULL_CORE_INIT") && am2_zynq_bm1362_recipe_gate_matches()
}

/// BM1362 PLL VCO lock-range envelope that `pll_lookup_extended` spans:
/// `VCO = 25*FBDIV/REFDIV` in [`RE018_PLL_VCO_MIN_MHZ`, `RE018_PLL_VCO_MAX_MHZ`].
/// The sub-400 low-VCO regime clamps `FBDIV` to [96,160] at REFDIV=1 (VCO floor
/// 2400) and the 400-597 table tops out at FBDIV=239 (VCO 5975).
const RE018_PLL_VCO_MIN_MHZ: u32 = 2400;
const RE018_PLL_VCO_MAX_MHZ: u32 = 5975;

/// Validate a raw `DCENT_AM2_RE018_PLL_HEX` reg-0x08 escape-hatch value against
/// the SAME envelope `pll_lookup_extended` enforces: `VCO = 25*FBDIV/REFDIV`
/// inside [`RE018_PLL_VCO_MIN_MHZ`, `RE018_PLL_VCO_MAX_MHZ`] AND a decoded
/// operating frequency inside the proven [240, 597] MHz window.
///
/// This refuses BOTH a grossly out-of-lock VCO (a fat-fingered overclock/under-lock
/// hex) AND the live-FALSIFIED high-VCO-plus-huge-postdiv shape: e.g.
/// `0x50D2_0164` decodes to VCO 5250 (in band) but a ÷35 postdiv -> 150 MHz, off
/// the proven low-VCO regime the BM1362 jig uses for sub-400 — it returned zero
/// nonces live. Both failure modes land OUTSIDE one of the two checks, so the
/// lab escape hatch can never silently drive the chip to an unsafe / zero-nonce
/// PLL. Returns `true` iff the value is in-envelope.
fn am2_re018_pll_hex_within_envelope(reg: u32) -> bool {
    let r = reg & !PLL_LOCK_BIT;
    let fbdiv = (r >> 16) & 0xFF;
    let refdiv = ((r >> 8) & 0xFF).max(1);
    if fbdiv == 0 {
        return false;
    }
    let vco = 25 * fbdiv / refdiv;
    if !(RE018_PLL_VCO_MIN_MHZ..=RE018_PLL_VCO_MAX_MHZ).contains(&vco) {
        return false;
    }
    matches!(decode_pll_reg_to_freq(reg), Some(mhz) if (240..=597).contains(&mhz))
}

/// Resolve the RE-018 frequency-override PLL reg-0x08 (only consulted when the
/// `DCENT_AM2_RE018_LOW_FREQ_PLL` gate is set). Precedence:
///   1. `DCENT_AM2_RE018_TARGET_MHZ` -> `bm1362::pll_lookup_extended` (PROVEN, 240-597; sub-400 uses
///      the proven ÷10 divider, >=400 the table). Lets the operator dial the eco<->hash range.
///   2. `DCENT_AM2_RE018_PLL_HEX` -> raw reg 0x08 (lab/escape hatch; e.g. 0x50800141), VALIDATED
///      against the `pll_lookup_extended` VCO/operating envelope; out-of-envelope is REFUSED and
///      falls through to the proven default so a fat-fingered hex can't drive an unsafe PLL.
///   3. default [`RE018_LOW_FREQ_PLL_08`] = the proven 320 MHz efficiency sweet spot.
/// POWER-RAISING vs the ~50 MHz eco/heater default ([`RE018_PLL_08`]) — wattmeter-gated live test.
fn am2_re018_resolve_pll_override_reg() -> u32 {
    if let Ok(s) = std::env::var("DCENT_AM2_RE018_TARGET_MHZ") {
        if let Ok(mhz) = s.trim().parse::<u16>() {
            return pll_lookup_extended(mhz).0;
        }
    }
    if let Ok(s) = std::env::var("DCENT_AM2_RE018_PLL_HEX") {
        let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
        if let Ok(reg) = u32::from_str_radix(t, 16) {
            // Clamp the raw lab escape-hatch to the SAME VCO/operating envelope
            // `pll_lookup_extended` enforces so a fat-fingered hex can't drive an
            // unsafe / zero-nonce PLL. Out-of-envelope -> REFUSE, fall through to
            // the proven default. (Lab-only knob — the `a lab unit` proven recipe never
            // sets `DCENT_AM2_RE018_PLL_HEX`, so the proven path is byte-unchanged.)
            if am2_re018_pll_hex_within_envelope(reg) {
                return reg;
            }
            warn!(
                raw_reg = format_args!("0x{:08X}", reg),
                fallback = format_args!("0x{:08X}", RE018_LOW_FREQ_PLL_08),
                "DCENT_AM2_RE018_PLL_HEX outside the BM1362 PLL VCO/operating envelope \
                 (VCO 2400-5975 MHz, freq 240-597 MHz) — REFUSING the raw value and using the \
                 proven default to avoid an unsafe / zero-nonce PLL"
            );
            return RE018_LOW_FREQ_PLL_08;
        }
    }
    RE018_LOW_FREQ_PLL_08
}

/// Gated, fail-closed-when-enabled, idempotent board-control `+0x04` bit 8 set.
/// No-op unless [`am2_board_control_bit8_enabled`]. Called pre-enum so the
/// SET_ADDRESS walk runs with the board-control register byte-identical to
/// bosminer-engaged (`0x134`). `stage` labels the call site. Idempotent: the
/// underlying [`BoardControl::set_control_board_mode_bit8`] is a read-modify-write
/// OR of bit 8 that preserves the C52 fan-mode low byte and every other bit, so
/// it is safe to call after the C52 fan-mode write and on retry. When the gate is
/// set, an open or readback failure aborts before enum (fail-closed) so a
/// half-applied board state can never be mistaken for a clean standalone proof.
fn am2_try_set_board_control_bit8(stage: &str) -> Result<()> {
    if !am2_board_control_bit8_enabled() {
        return Ok(());
    }
    match BoardControl::open(board_control_uio_number()) {
        Ok(bc) => match bc.set_control_board_mode_bit8() {
            Ok(s) => {
                info!(
                    stage,
                    env = "DCENT_AM2_BOARD_CONTROL_BIT8=1",
                    before = format_args!("0x{:08X}", s.before),
                    after = format_args!("0x{:08X}", s.after),
                    "W2-diff board-control: +0x04 bit 8 (0x100) set — expect \
                     after=0x...134 (byte-identical to live bosminer-engaged)"
                );
            }
            Err(e) => {
                anyhow::bail!("W2-diff board-control: +0x04 bit 8 set failed at {stage}: {e}");
            }
        },
        Err(e) => {
            anyhow::bail!(
                "W2-diff board-control: BoardControl::open(uio17) failed at {stage}: {e}"
            );
        }
    }
    Ok(())
}

/// `DCENT_AM2_DSPIC_FW_FROM_OBSERVED=1` ( / RE-018, 2026-05-30) — on the
/// `a lab unit` effective-chain dsPIC, construct the `Pic0x89Service` with the OBSERVED
/// firmware-family echo byte (`0x8A`) instead of `None` (which defaults to
/// `Fw89`). On `a lab unit` the effective-chain dsPIC (0x22) answers GET_VERSION with
/// fw=0x89 but its idle/echo byte is `0x8A` — and `dspic_enable_disable_encoding`
/// selects the **7-byte VnishPadded** ENABLE form for `Fw89` (which `a lab unit` echoes
/// `[0x8A,0x8A]`) vs the **6-byte Canonical** form for `Fw8A` — and the 6-byte
/// Canonical form is the one that produced a real `[0x15,0x00]` ACK on the live
/// ttyS3 cold-cycle (2026-05-30). Passing `Some(0x8A)` also makes
/// `classify_enable_ack` expect `0x8A` so a legitimate echo is `FirmwareEcho`
/// (not `FirmwareEchoMismatch`). Default-OFF + `a lab unit`-fingerprint + effective-chain
/// only → byte-identical for `a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9/handoff (they keep
/// `None`⇒`Fw89`). Workflow `wf_ce77f2f8` Change #1 (SAFE_BEHIND_FLAG).
fn am2_dspic_fw_from_observed_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DSPIC_FW_FROM_OBSERVED")
}

/// `DCENT_AM2_LM75_RAIL_PROXY=1` (2026-05-31) — on the `a lab unit` standalone path,
/// read the chain dsPIC's LM75 die-temp via the 0x3B/0x3C 6-byte PASSTHROUGH
/// protocol (the bosminer-proven `a lab unit` shape) instead of the legacy
/// `read_temperature` (0x30 + 4-byte read), which returns NaN on `a lab unit` because
/// 0x30 is not the LM75-passthrough opcode there. With the passthrough decode
/// working, the rail proxy can emit a 3-state autonomous (no-DMM) verdict:
/// `DECODES` (dsPIC passthrough alive) → `ABOVE-AMBIENT` (>28 °C ⇒ chips
/// drawing power ⇒ rail up) → `DELTA` (≥1.5 °C rise after ENABLE ⇒ rail
/// CONFIRMED up). DIAGNOSTIC/observability ONLY — reads LM75 temps and logs a
/// verdict; changes NO mining/voltage/enum control flow and is safe if the
/// dsPIC doesn't answer (verdict = UNAVAILABLE, never panics/aborts).
///
/// **Default off** + `a lab unit`-fingerprinted (`am2_xil_25_fingerprint_matches`):
/// absent env OR any non-`a lab unit` unit ⇒ byte-identical (keeps calling
/// `read_temperature`). So `a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9/handoff are untouched.
///
/// ⚠️ RE CORRECTION (R5, 2026-05-31 — ):
/// 0x3B/0x3C-with-arg-0x48 are LM75A **TEMPERATURE** passthrough reads (°C), NOT
/// a rail-voltage read (Ghidra: bosminer.bin carries `hwmon/src/lm75a.rs`; the
/// 0x3C reply `[3C 01 1A E0 01 3E]` decodes to ~26.9 °C, NOT millivolts). This
/// proxy therefore produces a HEURISTIC die-temperature inference ("chips warm
/// ⇒ likely drawing power ⇒ rail probably up"), NEVER a direct rail-mV reading.
/// It is observability-only and gates NOTHING — see the verdict site, which
/// changes no mining/voltage/enum control flow and is fail-safe (UNAVAILABLE) if
/// the dsPIC doesn't answer. Do NOT promote its verdict into a rail-mV decision
/// or a mining gate: a temperature is not a voltage. The TRUE actual-rail proxy
/// is the dsPIC analog-ADC 0x3A `measure_voltage` (read elsewhere); even that is
/// "stronger proxy, still not physical/DMM proof".
fn am2_lm75_rail_proxy_enabled() -> bool {
    am2_env_flag("DCENT_AM2_LM75_RAIL_PROXY")
}

/// 2026-05-24 () — `a lab unit`-class XIL fingerprint check using the
/// same conventions as `wave55a_recipe_guard::fingerprint_matches_xil_25`.
/// Reads `/etc/dcentos/platform` and `/etc/dcentos/board_target`; returns
/// `true` when they match the canonical `a lab unit` stamps. The  launcher may
/// also set `DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE=1` to bridge the deliberate
/// `/etc/dcentos/board_target = am2-s19j` sysupgrade package identity into
/// `a lab unit` diagnostics; the platform and Loki-compatible PSU fingerprint still
/// have to match.
///
/// Held here (not delegated to `wave55a_recipe_guard`) because Phase 0 is
/// already deep into `Apw121215a` lifetime — adding the cross-module
/// dependency would force a refactor of the guard module's
/// `evaluate_guard` signature. The fingerprint check is intentionally
/// duplicated; the  regression test pins that both copies stay
/// in sync.
const AM2_XIL25_FINGERPRINT_OVERRIDE_ENV: &str = "DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE";
// Source-visible proof envs pinned by `tests/am2_class_gate_boundary.rs`:
// `DCENT_AM2_ZYNQ_BM1362_CLASS_RECIPE` + `DCENT_AM2_XIL109_FINGERPRINT_OVERRIDE`.
const AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV: &str =
    crate::wave55a_recipe_guard::AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV;
const AM2_XIL109_FINGERPRINT_OVERRIDE_ENV: &str =
    crate::wave55a_recipe_guard::XIL_109_FINGERPRINT_OVERRIDE_ENV;

fn am2_xil_25_fingerprint_matches() -> bool {
    let platform = std::fs::read_to_string("/etc/dcentos/platform")
        .or_else(|_| std::fs::read_to_string("/etc/bos_platform"))
        .unwrap_or_default();
    let board_target = std::fs::read_to_string("/etc/dcentos/board_target").unwrap_or_default();
    let psu_hardware_variant = std::fs::read_to_string("/etc/dcentos/psu_hardware_variant").ok();

    if platform.trim() != "zynq-bm3-am2" {
        return false;
    }
    let board_target = board_target.trim();
    let board_target_matches = board_target.ends_with("xil");
    let explicit_xil25_override =
        board_target == "am2-s19j" && am2_env_flag(AM2_XIL25_FINGERPRINT_OVERRIDE_ENV);
    if !board_target_matches && !explicit_xil25_override {
        return false;
    }
    // psu_hardware_variant is optional. If explicitly declared as
    // something OTHER than "loki", do NOT fire  (operator has
    // declared a non-Loki PSU topology — 's bytes only make
    // sense for the Loki spoof).
    match psu_hardware_variant.as_deref().map(str::trim) {
        Some("") | None => true,
        Some(v) if v.eq_ignore_ascii_case("loki") => true,
        Some(_) => false,
    }
}

fn am2_chip_type_hint() -> Option<String> {
    [
        "/etc/dcentos/serial_chip_type",
        "/etc/dcentos/chip_type",
        "/etc/dcentos/asic_chip_type",
    ]
    .into_iter()
    .find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn am2_zynq_bm1362_class_matches() -> bool {
    let platform = std::fs::read_to_string("/etc/dcentos/platform")
        .or_else(|_| std::fs::read_to_string("/etc/bos_platform"))
        .unwrap_or_default();
    let board_target = std::fs::read_to_string("/etc/dcentos/board_target").unwrap_or_default();
    let chip_type_hint = am2_chip_type_hint();

    crate::wave55a_recipe_guard::am2_zynq_bm1362_class_matches(
        &platform,
        &board_target,
        chip_type_hint.as_deref(),
    )
}

fn am2_xil_109_fingerprint_matches() -> bool {
    // AM2_XIL109_FINGERPRINT_OVERRIDE_ENV is an external per-unit proof
    // supplied by a launcher/runbook after durable identity checks.
    let platform = std::fs::read_to_string("/etc/dcentos/platform")
        .or_else(|_| std::fs::read_to_string("/etc/bos_platform"))
        .unwrap_or_default();
    let board_target = std::fs::read_to_string("/etc/dcentos/board_target").unwrap_or_default();
    let chip_type_hint = am2_chip_type_hint();

    crate::wave55a_recipe_guard::fingerprint_matches_xil_109(
        &platform,
        &board_target,
        chip_type_hint.as_deref(),
        |name| std::env::var(name).ok(),
    )
}

fn am2_zynq_bm1362_recipe_gate_matches() -> bool {
    am2_xil_25_fingerprint_matches()
        || (am2_env_flag(AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV)
            && am2_env_flag(AM2_XIL109_FINGERPRINT_OVERRIDE_ENV)
            && am2_xil_109_fingerprint_matches()
            && am2_zynq_bm1362_class_matches())
}

/// 2026-06-07 (COLD-BYTE-DIFF Fix B) — opt-in env gate for emitting the `a lab unit`
/// cold dsPIC strace-derived warmup as ONE atomic typed I2C transaction (vs the
/// proven N separate transactions). When set, the entire flush→RESET→JUMP
/// warmup runs as a single service request, so no other i2c-0 producer
/// (thermal LM75 reads, PIC heartbeats, absent-slave probes) can interleave
/// between the warmup's transactions and desync the cold dsPIC MSSP bootloader
/// parser (the desync that makes it echo `0x82` instead of ACKing the framed
/// RESET/JUMP).
///
/// **Default-OFF + AM2-Zynq/BM1362 recipe-gated**:
/// absent env OR no `a lab unit`/explicit sibling recipe proof ⇒ the byte-identical
/// N-transaction path (the proven fleet / handoff / legacy behaviour). The
/// on-wire bytes + dwells are UNCHANGED either way — only the transaction
/// boundary changes. See
///  (Fix B).
fn am2_dspic_cold_warmup_exclusive_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DSPIC_COLD_WARMUP_EXCLUSIVE") && am2_zynq_bm1362_recipe_gate_matches()
}

/// Dispatch the  strace-derived FRAMED warmup for `addr`, choosing the
/// COLD-BYTE-DIFF Fix B single-atomic-transaction form when
/// `am2_dspic_cold_warmup_exclusive_enabled()` (env + `a lab unit` fingerprint), else
/// the proven N-transaction form. The two emit byte-and-timing-identical wire
/// traffic; only the transaction boundary differs. Centralising the dispatch
/// here keeps every strace-warmup call site in lockstep and lets the gate-OFF
/// default stay byte-identical to the legacy path.
fn am2_run_strace_derived_warmup(i2c: &I2cServiceHandle, addr: u8) -> dcentrald_asic::Result<()> {
    if am2_dspic_cold_warmup_exclusive_enabled() {
        bosminer_warmup::am2_pic_reset_and_start_app_strace_derived_exclusive(i2c, addr)
    } else {
        bosminer_warmup::am2_pic_reset_and_start_app_strace_derived(i2c, addr)
    }
}

/// 2026-05-23 (XIL `a lab unit` ) — opt-in env gate for the
/// bosminer-plus-tuner 0.9.0 STRACE-DERIVED FRAMED warmup variant.
///
/// When `DCENT_AM2_PIC_RESET_STRACE_DERIVED=1`, Phase 0d uses the new
/// `bosminer_warmup::am2_pic_reset_and_start_app_strace_derived` chain
/// (8 single-byte 0x00 sync heartbeats + framed `[55 AA 04 07 00 0B]` + ACK +
/// 500 ms + framed `[55 AA 04 06 00 0A]` + ACK + 500 ms) — the byte sequence
/// captured by `strace -e write,read` on bosminer-plus-tuner 0.9.0 / LEDE
/// `zynq-bm3-am2` while it successfully mined on `a lab unit` (per
/// ).
///
/// **Takes precedence** over `DCENT_AM2_PIC_RESET_AND_START_APP` and
/// `DCENT_AM2_PIC_RESET_NO_JUMP`. When both `DCENT_AM2_PIC_RESET_STRACE_DERIVED`
/// and `DCENT_AM2_PIC_RESET_AND_START_APP` are set, the strace-derived path
/// wins (mutually exclusive — only one warmup chain can run per Phase 0d).
///
/// **Default off** so the very first deploy of this binary on `a lab unit` stays
/// byte-identical to today's behaviour. Operator flips this on `a lab unit` for the
///  A/B run; promote to env-off-by-default if/when the strace-derived
/// chain proves out fw=0x89 detection + real-13.7-V chip rail on `a lab unit`.
fn am2_pic_reset_strace_derived_enabled() -> bool {
    am2_env_flag("DCENT_AM2_PIC_RESET_STRACE_DERIVED")
}

/// 2026-05-23 (XIL `a lab unit` ) — opt-in env gate for the
/// bosminer-plus-tuner 0.9.0 STRACE-DERIVED FRAMED GET_VERSION read shape.
///
/// When `DCENT_AM2_GET_VERSION_FRAMED_4B=1`, `pic_read_fw_version_service`
/// inserts a 4-byte-read variant of the framed `[55 AA 04 17 00 1B]` probe
/// BEFORE the existing 1-byte probes. The 4-byte read matches the actual
/// bosminer-plus-tuner 0.9.0 response shape captured on `a lab unit`:
/// `[0x17, FW, 0x00, CKSUM]` where the firmware byte is at INDEX 1 (not
/// index 2 like older lineages). On older lineages the 4-byte read either
/// returns a known fw byte at index 1 OR is rejected by the parser → we
/// fall through to the existing 1-byte probes.
///
/// Companion to `DCENT_AM2_PIC_RESET_STRACE_DERIVED`. The two gates are
/// INDEPENDENT: warmup-only, read-only, both, or neither — operator picks
/// per A/B run. Default-off so unset env = byte-identical to today.
///
/// Tipping point: if  reset+start-app lands the chip in fw=0x89
/// but `pic_read_fw_version_service` still reads fw=0x82, it's because the
/// read path is the wrong shape — flip THIS gate to land the matching
/// 4-byte read shape and pick up the fw=0x89 byte at index 1.
fn am2_get_version_framed_4b_enabled() -> bool {
    am2_env_flag("DCENT_AM2_GET_VERSION_FRAMED_4B")
}

/// 2026-05-23 (XIL `a lab unit` ) — bosminer-faithful Phase 0d timing.
///
///  (commit `60a2ab3d`) shipped three faithful timing fixes
/// (parser-flush skip, 6-ms inter-byte sleep, 7-byte sync prelude) behind
/// `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1`. Live result on `a lab unit` run #15:
/// dsPIC 0x20 stays alive throughout cold-boot (wedge FIXED) but
/// GET_VERSION now NACKs with EIO instead of all-FF.
///
/// Diagnosis: bosminer's i2c-0 strace shows NO intervening heartbeats
/// between START_APP ACK and GET_VERSION — just a single ~500-ms wall
/// sleep. DCENT_OS inserts a 5×1-s "stable heartbeat" warmup loop in
/// `s19j_hybrid_mining.rs::Phase 0d` between the warmup-prelude end and
/// the GET_VERSION probe. The hypothesis tested by : those 5
/// extra heartbeats wedge the dsPIC into the EIO/NACK state.
///
/// When `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1`,  replaces the
/// 5×1-s loop with a single `Duration::from_millis(500)` sleep —
/// byte-faithful to the strace evidence — and treats warmup as OK
/// (so `cold_boot_init_with_skip(skip_warmup_loop=true)` runs once
/// downstream, exactly like the env-off path that just succeeded with
/// 5×1-s heartbeats).
///
/// Same env gate as  — operator opts into the WHOLE
/// bosminer-faithful Phase 0d shape (timing + protocol). Default-off
/// → `a lab unit` and the rest of the fleet stay byte-identical.
fn am2_dspic_bosminer_faithful_enabled() -> bool {
    am2_env_flag("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL")
}

/// 2026-05-24 (XIL `a lab unit` ) — EEPROM bus-warmup before dsPIC init.
///
///  strace on DCENT_OS daemon vs  bosminer ground-truth
/// strace revealed a HUGE divergence: bosminer reads ~20 bytes from
/// EEPROM at 0x50 (and 0x52) on `/dev/i2c-0` BEFORE addressing the
/// dsPIC at 0x20. DCENT_OS jumps straight to 0x20 (the  strace
/// shows only 2 `I2C_SLAVE_FORCE` ioctls in the entire run, both
/// 0x20). The dsPIC responds to DCENT_OS with CMD echoes (0x07 after
/// RESET, 0x06 after START_APP) instead of 0x01 ACKs — bootloader
/// CMD-echo-mode behaviour, not app-mode.
///
/// Hypothesis: the EEPROM read bus activity wakes the dsPIC's MSSP
/// I2C slave peripheral. Without it the dsPIC stays in CMD-echo mode
/// and START_APP `[55 AA 04 06 00 0A]` doesn't transition the chip
/// to app firmware → GET_VERSION returns CMD echoes or all-FF.
///
/// When `DCENT_AM2_EEPROM_BUS_WARMUP=1`, Phase 0d emits a Read(32)
/// transaction to 0x50 (and 0x52) BEFORE the dsPIC warmup chain.
/// Read-only — the EEPROM denylist (`I2cBus::write_denylist
/// [0x50..=0x57]`) is preserved. Default-OFF — fleet byte-identical
/// when unset.
fn am2_eeprom_bus_warmup_enabled() -> bool {
    am2_env_flag("DCENT_AM2_EEPROM_BUS_WARMUP")
}

/// Outcome of the Layer-2 opportunistic smart-APW12 handshake under
/// `psu_override` (the "Loki bypass" branch).
///
/// On a Loki-board-equipped unit (`a lab unit`, `a lab unit`) the spoof responds and the
/// full bosminer-canonical `3× Disable → Ramp → Enable` + 1 Hz heartbeat
/// completes — matching what BraiinsOS does. On a genuinely Loki-removed
/// unit the bus is silent on the first opcode and we fall through with
/// `psu = None`, matching today's `psu_override` branch behaviour
/// byte-for-byte.
struct SmartApw12HandshakeOutcome {
    psu: Option<Arc<Mutex<Apw121215a>>>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

/// Layer-2 (FIX-A) — opportunistically drive the smart-APW12 handshake on
/// the Loki spoof under `psu_override`.
///
/// Reuses `Apw121215a`'s existing `probe`/`disable`/`set_voltage_init_bypass`/
/// `enable`/`heartbeat` public methods + the 1 Hz heartbeat loop, but with
/// a tightened probe budget (1 attempt × 200 ms instead of full retry budget)
/// so a genuinely-silent bus (real Loki-removed APW3) returns Ok(None) quickly
/// rather than burning the cold-boot budget on a non-existent peer.
///
/// **The PWR_CONTROL GPIO MUST be owned by the caller's `PsuBypassGate`
/// BEFORE calling this helper.** The `Apw121215a` opened here is configured
/// WITHOUT a gate_spec so it does NOT double-assert the same GPIO. The
/// `PsuBypassGate`'s Drop is the PWR_CONTROL release on shutdown.
///
/// §(d) FIX-A
/// and §(e) Step 3.
fn bring_up_apw121215a_smart_lenient(
    i2c0_service: &I2cServiceHandle,
    psu_address: u8,
    psu_target_rail_v: f64,
    psu_heartbeat_interval: Duration,
    shutdown: CancellationToken,
    psu_transport: &str,
) -> SmartApw12HandshakeOutcome {
    use std::time::Instant;

    //  (2026-05-23 — `a lab unit` cold-wake gap closure):
    //
    // The FIX-A author assumed the smart-APW12 spoof was reachable via the
    // kernel `/dev/i2c-0` bus (matching the non-override path's default).
    // That's wrong on `a lab unit` and any `a lab unit`-class AM2 unit whose
    // `[psu].transport = "gpio_bitbang"`: the PSU SMBus is BIT-BANGED
    // on dedicated `PSU_GPIO_SDA` / `PSU_GPIO_SCL` lines, NOT the kernel
    // i2c-0 bus. Probing 0x10 on kernel i2c-0 returns silence (the bus
    // has no peer at that address) → 200 ms probe deadline always expires
    // → FIX-A returns `psu=None` → daemon proceeds in PWR_CONTROL-only mode
    // → the Loki spoof never gets the bosminer-canonical Disable/Ramp/Enable
    // sequence → APW3 rail stays underpowered → dsPIC stays in standby →
    // GET_VERSION returns all-0xFF.
    //
    // Live evidence for this gap: 2026-05-23 BraiinsOS slot cold-wake test
    // on `a lab unit` (
    // wave28-live-runs/braiinsos-cold-wake-success-20260523.log`). Bosminer
    // probed PSU at 0x10 via gpio-bitbang transport (verified by the absence
    // of slave 0x10 ioctls in bosminer's `/dev/i2c-0` strace), got back
    // `Version '0x71' (APW121215a) detected`, then drove dsPICs to fw=0x89
    // on both PWR/1 + PWR/3, then enumerated 252 chips at FastUART 3.125
    // Mbps. The same hardware returned all-FF to DCENT_OS reads 30 minutes
    // earlier because we were probing on the wrong bus.
    //
    //  fix: honour `[psu].transport` here. On `gpio_bitbang` open via
    // `Apw121215a::open_gpio_bitbang_at` + skip the probe (bit-bang transport
    // is write-only by convention — no read-back) + run
    // `cold_boot_sequence_write_only(target, APW12_139_ASSUMED_FW)` + spawn
    // the heartbeat. On any other transport keep the existing service-backed
    // probe + cold-boot-gated path (this is what `a lab unit`-class Loki-attached
    // units that DO speak on kernel i2c-0 still use).
    info!(
        addr = format_args!("0x{:02X}", psu_address),
        target_rail_v = psu_target_rail_v,
        transport = psu_transport,
        "Phase 0 (b): smart-APW12 opportunistic handshake on Loki spoof — \
         transport-aware (Wave-30); gpio_bitbang skips probe + write-only \
         cold-boot, i2c0 path keeps 200 ms lenient probe"
    );

    // --- gpio_bitbang transport () ---
    if psu_transport == "gpio_bitbang" {
        let mut psu = match Apw121215a::open_gpio_bitbang_at(psu_address) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    error = %e,
                    addr = format_args!("0x{:02X}", psu_address),
                    "smart-APW12 lenient (gpio_bitbang): open_gpio_bitbang_at FAILED — \
                     falling through with psu=None (PWR_CONTROL-only mode)"
                );
                return SmartApw12HandshakeOutcome {
                    psu: None,
                    heartbeat: None,
                };
            }
        };
        // DO NOT propagate any gate spec — the PsuBypassGate above already
        // owns PWR_CONTROL. We're an opportunistic add-on, not a replacement
        // for the bypass gate's ownership.
        psu.set_psu_gate_spec(None);

        //  (2026-05-23, same-day  follow-up after partial-success
        // live test on `a lab unit`):
        //
        //  wired the gpio_bitbang branch correctly but  alone
        // wasn't enough to wake the Loki spoof's state machine. Live evidence:
        //
        // wave28-live-runs/wave30-full-run3-20260523.log` — FIX-A logged
        // `smart-APW12 spoof handshake SUCCEEDED on Loki bus` but the dsPIC
        // still returned all-FF on GET_VERSION afterwards.
        //
        // Comparing against bosminer's actual sequence on `a lab unit` (per the
        // morning strace + bosminer.log timing):
        //
        //   Bosminer order:  Detect(READ) → Disable → CalibrationProbe(×4)
        //                    → Disable(×3) → Ramp → Enable → ChainInit
        //   DCENT_OS order:  Disable(×3) → Ramp → Enable → Heartbeat
        //                    → ChainInit  (NO initial Detect READ)
        //
        // Bosminer's first action on the PSU bus is a READ (GetVersion). It
        // logs `PSU: Version '0x71' (APW121215a) detected` AFTER that read
        // succeeds. The Loki spoof appears to need this initial read-handshake
        // to engage its application state — without it, subsequent writes are
        // ACKed at the bit level but not processed into rail-engagement.
        //
        //  fix: add an explicit best-effort `psu.probe()` BEFORE the
        // write-only sequence. The probe issues the GetVersion READ to the
        // spoof. We don't care about the result (success or failure-with-EIO);
        // the act of reading is the wake event. On Loki-attached units the
        // probe succeeds and the spoof engages; on bare-APW3 units (no peer)
        // the probe fails-fast with EIO and we proceed with the write-only
        // sequence which is silently absorbed (existing safe behaviour).
        //  +  (2026-05-23): initial Detect READ with
        // NAK-tolerant retry.
        //
        //  added the initial probe but gave up on first NAK.
        //  forensics showed the Loki spoof's first response to
        // GetFwVersion is 0xF5 = NAK. Bosminer's log shows it tolerates
        // multiple NAKs in a row (calibration table empty × 4 over 30s)
        // before getting a successful response.  adds retry until
        // ACK OR retry budget exhausted (8 × 250 ms = 2 s total). Two
        // exit conditions:
        //   - Probe Ok → spoof acknowledged; proceed to write-only cold-boot
        //   - Budget exhausted → log + proceed anyway (bare-APW3 with no
        //     peer still falls through cleanly; write-only is silently
        //     absorbed in that case)
        info!(
            "smart-APW12 lenient (gpio_bitbang) — Wave-31+34: emitting initial Detect READ \
             with NAK-tolerant retry (8 × 250 ms) — bosminer-pattern wake for Loki spoof"
        );
        const PROBE_RETRY_BUDGET: u32 = 8;
        const PROBE_RETRY_DELAY_MS: u64 = 250;
        let mut probe_succeeded = false;
        for attempt in 1..=PROBE_RETRY_BUDGET {
            match psu.probe() {
                Ok(model) => {
                    info!(
                        attempt,
                        model = model.name(),
                        fw = format_args!("0x{:02X}", psu.fw_byte().unwrap_or(0)),
                        "smart-APW12 lenient (gpio_bitbang) Wave-31+34: Detect READ succeeded \
                         on attempt {} — Loki spoof responded; proceeding to write-only cold-boot",
                        attempt
                    );
                    probe_succeeded = true;
                    break;
                }
                Err(e) => {
                    if let Some(fw) = psu.fw_byte() {
                        if fw != APW12_139_ASSUMED_FW {
                            error!(
                                fw = format_args!("0x{:02X}", fw),
                                error = %e,
                                "smart-APW12 lenient observed a non-fw71 PSU dialect; \
                                 refusing the write-only fallback"
                            );
                            return SmartApw12HandshakeOutcome {
                                psu: None,
                                heartbeat: None,
                            };
                        }
                    }
                    let err_str = e.to_string();
                    let is_nak = err_str.contains("PSU NAK") || err_str.contains("0xF5");
                    if is_nak && attempt < PROBE_RETRY_BUDGET {
                        info!(
                            attempt,
                            budget = PROBE_RETRY_BUDGET,
                            "smart-APW12 lenient (gpio_bitbang) Wave-34: NAK on Detect READ \
                             attempt {} — sleeping {}ms then retrying (bosminer-tolerance \
                             pattern; spoof may need ~4 retries to wake)",
                            attempt,
                            PROBE_RETRY_DELAY_MS
                        );
                        std::thread::sleep(Duration::from_millis(PROBE_RETRY_DELAY_MS));
                        continue;
                    }
                    info!(
                        attempt,
                        error = %e,
                        "smart-APW12 lenient (gpio_bitbang) Wave-31+34: Detect READ returned \
                         error (expected on bare-APW3 / silent bus, OR NAK budget exhausted on \
                         Loki spoof) — proceeding to write-only cold-boot anyway"
                    );
                    break;
                }
            }
        }
        let _ = probe_succeeded; // logged above; write-only cold-boot runs either way

        //  Patch 4 (2026-05-26): the smart-APW12 lenient (gpio_bitbang)
        // branch was UNCONDITIONALLY calling `cold_boot_sequence_write_only`,
        // bypassing the  standalone Loki cold-wake gate that exists
        // at line ~5440 (the i2c0-service transport branch). On `a lab unit` cold-cold
        // with `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1` + xil fingerprint +
        // no TRUST_RAIL_FALLBACK, we MUST run the standalone cold-wake here
        // BEFORE the write-only body — that's the  ground-truth path
        // (per-byte register-pointer init-frame + bare follow-frame + poll).
        // Without this fix, 's full_cycle never fires on the
        // gpio_bitbang transport and the Loki spoof never wakes up.
        let wave55b_loki_cold_boot_active = am2_psu_loki_cold_boot_full_enabled()
            && am2_xil_25_fingerprint_matches()
            && !am2_env_flag("DCENT_AM2_TRUST_RAIL_FALLBACK");
        if wave55b_loki_cold_boot_active {
            warn!(
                target_v = psu_target_rail_v,
                env_gate = "DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1",
                "Wave-55b STANDALONE Loki cold-boot path ENGAGED (gpio_bitbang transport) — \
                 emitting Wave-38 cold-wake bytes before standard 3× Disable + Ramp + Enable. \
                 This is the no-bosminer-handoff bring-up path for cold-cold .25-class XIL."
            );
            if let Err(e) = psu.cold_boot_sequence_loki_standalone(psu_target_rail_v) {
                warn!(
                    error = %e,
                    "Wave-55b standalone Loki cold-boot FAILED on gpio_bitbang transport — \
                     partial-handshake state; spawning heartbeat anyway to keep the spoof \
                     acknowledged (PsuBypassGate still owns PWR_CONTROL)"
                );
            } else {
                info!(
                    target_rail_v = psu_target_rail_v,
                    "Wave-55b standalone Loki cold-boot complete on gpio_bitbang transport — \
                     Wave-38 cold-wake bytes emitted + write-only body (3× Disable → Ramp → Enable) done"
                );
            }
        } else {
            info!(
                target_rail_v = psu_target_rail_v,
                assumed_fw = format_args!("0x{:02X}", APW12_139_ASSUMED_FW),
                "smart-APW12 lenient (gpio_bitbang): running cold_boot_sequence_write_only \
                 (3× Disable → Ramp → Enable) after Wave-31 Detect-READ wake \
                 (legacy path — Wave-55b not gated for this run)"
            );
            if let Err(e) =
                psu.cold_boot_sequence_write_only(psu_target_rail_v, APW12_139_ASSUMED_FW)
            {
                warn!(
                    error = %e,
                    "smart-APW12 lenient (gpio_bitbang): cold_boot_sequence_write_only FAILED — \
                     partial-handshake state; spawning heartbeat anyway to keep the spoof \
                     acknowledged (PsuBypassGate still owns PWR_CONTROL)"
                );
            } else {
                info!(
                    target_rail_v = psu_target_rail_v,
                    "smart-APW12 lenient (gpio_bitbang): write-only cold-boot complete — \
                     bosminer-parity init achieved on Loki spoof (or silently absorbed by \
                     bare-APW3 fleet — write-only is safe either way)"
                );
            }
        }

        let psu_arc = Arc::new(Mutex::new(psu));
        let psu_hb = psu_arc.clone();
        let shutdown_hb = shutdown.clone();
        let heartbeat = std::thread::Builder::new()
            .name("s19j-psu-hb-lenient-bitbang".into())
            .spawn(move || psu_heartbeat_loop(psu_hb, shutdown_hb, psu_heartbeat_interval))
            .ok();
        if heartbeat.is_some() {
            info!(
                interval_ms = psu_heartbeat_interval.as_millis() as u64,
                "smart-APW12 lenient (gpio_bitbang): 1 Hz heartbeat thread spawned \
                 (keeps Loki spoof acknowledged so it doesn't self-disable the rail)"
            );
        } else {
            warn!(
                "smart-APW12 lenient (gpio_bitbang): heartbeat thread spawn FAILED — \
                 spoof watchdog will cut in ~30 s; continuing with psu=Some, heartbeat=None"
            );
        }

        return SmartApw12HandshakeOutcome {
            psu: Some(psu_arc),
            heartbeat,
        };
    }

    // --- service-backed (kernel /dev/i2c-0) transport — pre- path ---
    let mut psu = match Apw121215a::open_service_at(i2c0_service.clone(), 0, psu_address) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                error = %e,
                addr = format_args!("0x{:02X}", psu_address),
                "smart-APW12 lenient: open_service_at failed — \
                 falling through with psu=None (PWR_CONTROL-only mode)"
            );
            return SmartApw12HandshakeOutcome {
                psu: None,
                heartbeat: None,
            };
        }
    };
    // DO NOT propagate the PsuBypassGate spec — the bypass gate already
    // owns PWR_CONTROL. Setting gate_spec here would cause Apw121215a to
    // double-assert the same GPIO (`KernelClaimed` error or worse).
    psu.set_psu_gate_spec(None);

    // Lenient probe — 1 attempt × 200 ms timeout window. If the spoof is
    // there it answers within ~tens of ms (per bosminer log timing on `a lab unit`).
    // If the bus is genuinely silent (real Loki-removed APW3), we drop out
    // immediately rather than waste the 3×100 ms cold_boot probe budget.
    let probe_deadline = Instant::now() + Duration::from_millis(200);
    let _ = psu.flush_buffer();
    let mut probe_ok = false;
    while Instant::now() < probe_deadline {
        match psu.probe() {
            Ok(model) => {
                info!(
                    model = model.name(),
                    fw = format_args!("0x{:02X}", psu.fw_byte().unwrap_or(0)),
                    "smart-APW12 lenient: probe OK (Loki spoof responded — \
                     proceeding to disable/ramp/enable)"
                );
                probe_ok = true;
                break;
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "smart-APW12 lenient: probe attempt failed; checking deadline"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    if !probe_ok {
        info!(
            "smart-APW12 lenient: probe deadline expired (200 ms) — assuming \
             Loki-removed / genuinely-silent bus; falling through with psu=None \
             (PWR_CONTROL-only mode, byte-identical to legacy psu_override branch)"
        );
        return SmartApw12HandshakeOutcome {
            psu: None,
            heartbeat: None,
        };
    }

    // The spoof is responding — run the bosminer-canonical
    // `3× Disable → Ramp → Enable` sequence. Each step is best-effort; a
    // partial-success outcome (e.g. disable OK but ramp NACKs on a flaky
    // spoof firmware) still spawns the heartbeat to keep PWR_CONTROL armed.
    //
    // NOTE: we use the existing `cold_boot_sequence_gated` here BUT the
    // gate spec is None, so the gated path performs no GPIO work — the I²C
    // history is byte-identical to the bosminer-RE'd canonical path
    // (Step 1 flush + Step 2 probe + Step 3 disable×3 + Step 4 ramp +
    // Step 5 enable; see psu.rs:1698-1779).
    if let Err(e) = psu.cold_boot_sequence_gated(psu_target_rail_v) {
        warn!(
            error = %e,
            "smart-APW12 lenient: cold_boot_sequence_gated failed AFTER successful probe — \
             this is a partial-handshake state; spawning heartbeat anyway to keep the \
             spoof acknowledged (PsuBypassGate still owns PWR_CONTROL)"
        );
    } else {
        info!(
            target_rail_v = psu_target_rail_v,
            "smart-APW12 lenient: 3×Disable → Ramp → Enable sequence completed — \
             bosminer-parity init achieved on Loki spoof"
        );
    }

    let psu_arc = Arc::new(Mutex::new(psu));
    let psu_hb = psu_arc.clone();
    let shutdown_hb = shutdown.clone();
    let heartbeat = std::thread::Builder::new()
        .name("s19j-psu-hb-lenient".into())
        .spawn(move || psu_heartbeat_loop(psu_hb, shutdown_hb, psu_heartbeat_interval))
        .ok();
    if heartbeat.is_some() {
        info!(
            interval_ms = psu_heartbeat_interval.as_millis() as u64,
            "smart-APW12 lenient: 1 Hz heartbeat thread spawned (keeps Loki spoof acknowledged)"
        );
    } else {
        warn!(
            "smart-APW12 lenient: heartbeat thread spawn FAILED — spoof watchdog will cut \
             in ~30s; continuing with psu=Some but heartbeat=None"
        );
    }

    SmartApw12HandshakeOutcome {
        psu: Some(psu_arc),
        heartbeat,
    }
}

// ====================================================================
//  am2/BM1362 FREQUENCY-ONLY autotuner — hybrid-path spawn wiring.
//
// W1-C (commit 2d2cf0d8) added the *gate* in `daemon.rs::Daemon::run()`,
// but `--s19j-hybrid` bypasses `Daemon::run()` entirely (the proven
// `a lab unit`/XIL/.109 path is `S19jHybridMiner::run()` →
// `run_am2_serial_dispatch_loop`). W1-C explicitly DEFERRED the
// hybrid-path spawn wiring to "a separate safety-reviewed wave" — this
// is that wave.
//
// BRICK-CRITICAL discipline (`a lab unit` is a live home unit):
//   * DEFAULT-OFF. Without an explicit operator opt-in
//     (`[autotuner] am2_frequency_autotune = true` OR
//     `DCENT_AM2_FREQUENCY_AUTOTUNE=1`) NONE of this code runs — the
//     serial-dispatch loop is byte-identical to the proven milestone
//     path. The opt-in resolver returns `None`, the loop never gains a
//     `freq_cmd_rx` select arm, never accumulates stats, never spawns a
//     tuner task. Zero `a lab unit` behavior change unless opted in.
//   * NO LIVE VOLTAGE on am2 this wave. Triple defense-in-depth:
//       (1) `pin_am2_bm1362_frequency_only()` HARD-pins
//           `voltage_optimization=false` + `dvfs_enabled=false`;
//       (2) the BM1362+dspic `autotuner_capabilities_for_chip` profile
//           already reports `voltage_optimization_supported=false` +
//           `dvfs_runtime_supported=false`, so the autotuner policy
//           degrades any voltage attempt before it reaches a command;
//       (3) the hybrid FreqCommand consumer HARD-REFUSES `SetVoltage`
//           / `VerifyVoltage` (logs + acks an error, never touches the
//           PIC/PSU). The pin owns prevention; the consumer is the
//           last-line fail-closed.
//   * QUIET HOME PROFILE is the DEFAULT objective: `TuneTarget::Efficiency`
//     (walk frequency DOWN toward the J/TH minimum). Performance /
//     hashrate objectives are opt-in only via operator TOML
//     (`[autotuner] target_mode = "hashrate"`), never the default on a
//     home unit. This mirrors `daemon.rs`'s W1.3 mode-aware default.
//   * Frequency search clamped to the home-safe nameplate band
//     `[245, 545]` MHz by `pin_am2_bm1362_frequency_only()`, AND every
//     *applied* PLL is additionally clamped to the proven on-this-path
//     `BM1362_PLL_TABLE` register range `[400, 545]` — see
//     `am2_freq_only_clamp_applied_mhz`. Walking below 400 MHz would
//     require PLL register words this serial path has never proven on
//     hardware; on a home unit we never apply an unproven register.
//
// Other families (S9/BM1387, am3-aml, am3-bb) never reach this code —
// it lives only inside the am2 serial-dispatch loop.
// ====================================================================

/// The am2/BM1362 hybrid-path frequency-only autotuner is HARD-pinned to
/// the proven on-wire `BM1362_PLL_TABLE` register range. The W1-C config
/// band is `[245, 545]` MHz, but the only PLL register words this serial
/// path has ever driven on real silicon are the `BM1362_PLL_TABLE`
/// entries (400..=597 MHz). On a live home unit we NEVER apply a PLL
/// register the path hasn't proven. We also never go above the nameplate
/// `545` MHz ceiling on a home unit (no overclock exploration).
const AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ: u16 = 400;
const AM2_FREQ_ONLY_APPLIED_CEIL_MHZ: u16 = 545;

/// Clamp an autotuner-requested frequency to the proven-applicable band
/// for the am2 serial PLL path. Intersection of:
///   * the W1-C `[245, 545]` home-safe config band (already enforced on
///     the `AutoTunerConfig` by `pin_am2_bm1362_frequency_only()`), and
///   * the proven `BM1362_PLL_TABLE` register range, nameplate-capped:
///     `[400, 545]`.
/// The result is always a frequency for which `bm1362_pll_lookup`
/// returns a live-proven PLL register word.
fn am2_freq_only_clamp_applied_mhz(requested_mhz: u16) -> u16 {
    requested_mhz.clamp(
        AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ,
        AM2_FREQ_ONLY_APPLIED_CEIL_MHZ,
    )
}

/// Resolve the W1-C frequency-only opt-in for the am2 hybrid path.
///
/// Mirrors `daemon.rs`'s W1-C gate exactly: opted-in iff
/// `[autotuner] am2_frequency_autotune = true` OR
/// `DCENT_AM2_FREQUENCY_AUTOTUNE` is env-truthy. Default (neither) =
/// `false` → the serial-dispatch loop stays byte-identical to the
/// proven `a lab unit` milestone path.
///
/// The am2 hybrid path is am2/BM1362/dsPIC by construction (this whole
/// module only runs for `zynq-bm3-am2` + `model="s19jpro"`), so unlike
/// `daemon.rs` we don't need the `chip_id == 0x1362 && pic==DsPic33EP`
/// family guard — reaching `run_am2_serial_dispatch_loop` *is* the
/// family proof. We still read the same TOML key + env var so a single
/// operator opt-in works identically across both entry points.
fn am2_frequency_autotune_opted_in(config: &DcentraldConfig) -> bool {
    config.autotuner.am2_frequency_autotune_enabled(
        std::env::var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV)
            .ok()
            .as_deref(),
    )
}

/// AT-3: resolve the quiet-window 0x3A measured-rail read opt-in for the am2
/// hybrid path.
///
/// Opted-in iff `[autotuner] at3_rail_read = true` OR `DCENT_AM2_AT3_RAIL_READ`
/// is env-truthy (mirrors `am2_frequency_autotune_opted_in`). **This is only
/// half the gate** — the AT-3 `rail_timer` arm additionally requires
/// `am2_frequency_autotune_opted_in` (the daemon ANDs the two, the most
/// conservative reading of the design's "default-OFF behind a NEW env AND the
/// autotuner-enabled flag"). Default (neither set) keeps AT-3 fully closed, so
/// the serial-dispatch loop is byte-identical to the proven `a lab unit`/`a lab unit`
/// milestone path.
fn am2_at3_rail_read_opted_in(config: &DcentraldConfig) -> bool {
    config.autotuner.at3_rail_read_enabled(
        std::env::var(dcentrald_autotuner::config::AT3_RAIL_READ_ENV)
            .ok()
            .as_deref(),
    )
}

/// AT-3: the clamped quiet-window 0x3A read cadence (seconds), 15..=120,
/// default 30 (`[autotuner] at3_rail_read_interval_s`).
fn am2_at3_rail_read_interval_s(config: &DcentraldConfig) -> u64 {
    config.autotuner.at3_rail_read_interval_s_clamped()
}

/// AT-3: map a dsPIC I²C address (0x20..=0x23) to the canonical AM2 hashboard
/// slot index, used as the per-chain telemetry key the API projection looks up
/// (`ChainState.id`). Mirrors `S19_DSPIC_ADDRS` / `am2_chain_plan`'s
/// `AM2_SLOT_DSPIC_ADDRS`. Returns `None` for an unrecognized address (AT-3
/// then publishes nothing — a clean no-op, never a wrong-keyed reading).
fn am2_chain_id_for_pic_addr(pic_addr: u8) -> Option<u8> {
    match pic_addr {
        0x20 => Some(0),
        0x21 => Some(1),
        0x22 => Some(2),
        0x23 => Some(3),
        _ => None,
    }
}

/// AT-3: gated, default-OFF, READ-ONLY quiet-window dsPIC 0x3A measured-rail
/// read. Best-effort; never blocks mining.
///
/// Returns `Some(mv)` ONLY when ALL of the following hold; otherwise `None`
/// (the AT-1 projection then degrades cleanly to commanded-tagged):
///   1. `fw` resolves to the framed byte-wise family (fw=0x89/0x8A) — enforced
///      by `dspic::at3_measure_voltage_firmware_allowed` so AT-3 never reaches
///      `measure_voltage`'s `I2C_RDWR` fallback for bare/unknown firmware;
///   2. `DspicService::measure_voltage` (the parser-safe `[55 AA 04 3A 00 3E]`
///      byte-wise framed read) succeeds;
///   3. the decoded value passes AT-1's plausibility gate (`> 0`, `<= DAC span`).
///
/// READ-ONLY: this issues a read trigger only — no `SET_VOLTAGE` (0x10), no
/// `ENABLE` (0x15), no frequency change. The clone of the single-owner
/// `I2cServiceHandle` queues one serialized transaction exactly as the thermal
/// supervisor's `poll_max_temp` does, so the bus is never double-owned.
fn at3_read_measured_rail(i2c: &I2cServiceHandle, addr: u8, fw: Option<u8>) -> Option<u16> {
    use dcentrald_asic::dspic::{
        at3_measure_voltage_firmware_allowed, DspicFirmware, DspicService, DSPIC_MAX_VOLTAGE_MV,
    };

    let fw_class = DspicFirmware::from_version(fw?);
    // Independent firmware gate — do NOT trust measure_voltage's internal
    // branching to keep AT-3 off the I2C_RDWR fallback (DESIGN 1 §1.5).
    if !at3_measure_voltage_firmware_allowed(fw_class) {
        return None;
    }
    let mut pic = DspicService::new_with_firmware(i2c.clone(), addr, fw_class);
    match pic.measure_voltage() {
        Ok(mv) if dcentrald_common::chain_voltage::plausible_rail_mv(mv, DSPIC_MAX_VOLTAGE_MV) => {
            Some(mv)
        }
        Ok(mv) => {
            debug!(
                addr = format_args!("0x{:02X}", addr),
                measured_mv = mv,
                "AT-3 0x3A read returned an implausible value — skipping (commanded fallback)"
            );
            None
        }
        Err(e) => {
            // Misframe / 0xFFFF / short read / service busy — best-effort, never
            // a retry on the hot path. Next tick tries again.
            debug!(error = %e, addr = format_args!("0x{:02X}", addr), "AT-3 0x3A read skipped");
            None
        }
    }
}

/// Build the FREQUENCY-ONLY `AutoTunerConfig` for the am2 hybrid path.
///
/// Starts from the operator's `[autotuner]` config, then:
///   1. force `enabled = true` (the operator opted in via
///      `am2_frequency_autotune`; the generic `[autotuner] enabled`
///      flag is a separate switch that the hybrid path historically
///      never consulted — opting into the am2 freq-only layer IS the
///      enable signal here);
///   2. apply `pin_am2_bm1362_frequency_only()` — the single
///      load-bearing transform: `voltage_optimization=false` +
///      `dvfs_enabled=false` (HARD) + freq band clamped `[245, 545]`;
///   3. apply the W1.3 mode-aware target default: if the loaded config
///      still has the structural `Efficiency` default, resolve it via
///      `TuneTarget::for_mode(mode)` (Heater/Mining → Efficiency,
///      Hacker → Hashrate). An explicit operator `target_mode` (e.g.
///      `"hashrate"`, `"power"`) is preserved — opting *out* of the
///      quiet default is the operator's call, never silent.
///
/// The QUIET HOME PROFILE (`Efficiency`) is the default: a home unit
/// optimizes the J/TH bill, never the leaderboard, unless the operator
/// explicitly asks for hashrate.
fn build_am2_freq_only_autotuner_config(
    config: &DcentraldConfig,
) -> dcentrald_autotuner::AutoTunerConfig {
    let mut autotune_config = config.autotuner.clone();

    // (1) The am2 freq-only opt-in IS the enable for this path.
    autotune_config.enabled = true;

    // (2) THE load-bearing safety pin. No live voltage on am2; band
    //     clamped to the home-safe nameplate window. Applied BEFORE the
    //     mode default so `validate()` (min<=max) stays sane.
    autotune_config.pin_am2_bm1362_frequency_only();

    // (3) W1.3 mode-aware quiet default. Only override the *structural*
    //     Efficiency default — an explicit operator target_mode is
    //     preserved (opting out of quiet is explicit, never silent).
    if matches!(
        autotune_config.target_mode,
        dcentrald_autotuner::config::TuneTarget::Efficiency
    ) {
        let mode_str = config.mode.active.as_str();
        let mode_default = dcentrald_autotuner::config::TuneTarget::for_mode(mode_str);
        if mode_default != autotune_config.target_mode {
            info!(
                operating_mode = %mode_str,
                old = ?autotune_config.target_mode,
                new = ?mode_default,
                "am2 freq-only autotuner target_mode adjusted by operating-mode default \
                 (Heater/Mining → Efficiency [quiet home], Hacker → Hashrate)"
            );
            autotune_config.target_mode = mode_default;
        }
    }

    autotune_config
}

/// Chain-level nonce/error accumulator for the am2 serial-dispatch path.
///
/// The 11-byte BM1362 serial nonce frame carries `small_core` + `job_id`
/// but **no chip address** (`parse_bm1362_serial_nonce` /
/// `Bm1362SerialNonce` — there is no chip field on the wire). True
/// per-chip TABS attribution is therefore impossible on the serial path
/// the way it is on the FPGA path. The conservative, correct choice for
/// a home unit is **chain-level** frequency-only tuning: one synthetic
/// chain whose single "chip" slot aggregates every enumerated chip's
/// nonces, and a broadcast PLL applied to all chips at once (exactly the
/// init's proven `send_write_reg_broadcast_bm1397plus(0x08, …)`
/// primitive). The autotuner walks the single chain frequency down
/// toward the J/TH minimum under `Efficiency`.
struct Am2SerialChainStats {
    chain_id: u8,
    /// Aggregate valid nonces this window.
    nonces: u64,
    /// Aggregate hardware errors this window (parse-failed job frames).
    errors: u64,
    window_start: Instant,
    epoch: u64,
    /// Last board/die temperature handed in (best-effort; the thermal
    /// supervisor remains the safety authority — this is informational
    /// for the tuner's thermal refinement only).
    last_temp_c: Option<f32>,
    current_difficulty: u32,
}

impl Am2SerialChainStats {
    fn new(chain_id: u8) -> Self {
        Self {
            chain_id,
            nonces: 0,
            errors: 0,
            window_start: Instant::now(),
            epoch: 0,
            last_temp_c: None,
            current_difficulty: 256,
        }
    }

    #[inline]
    fn record_nonce(&mut self) {
        self.nonces = self.nonces.saturating_add(1);
    }

    #[inline]
    fn record_error(&mut self) {
        self.errors = self.errors.saturating_add(1);
    }

    #[inline]
    fn set_temp(&mut self, temp_c: Option<f32>) {
        if temp_c.is_some() {
            self.last_temp_c = temp_c;
        }
    }

    /// Produce a single-synthetic-chain snapshot and reset the window.
    /// `chip_count` is the LIVE enumerated chip count for this AC cycle
    /// (28..110 on XIL) so the autotuner's expected-NPS math
    /// (`expected_nps_for_chip` × chip_count, 894 nonce-attribution
    /// slots per BM1362 chip — W6.8) scales with the real chain.
    fn take_snapshot(&mut self, chip_count: u8) -> dcentrald_autotuner::ChipStatsSnapshot {
        let now = Instant::now();
        let window_duration_s = now.duration_since(self.window_start).as_secs_f64();
        self.epoch = self.epoch.saturating_add(1);
        let snapshot = dcentrald_autotuner::ChipStatsSnapshot {
            chain_id: self.chain_id,
            measurement_epoch: self.epoch,
            // Single synthetic "chip" slot aggregating the whole chain.
            // The autotuner treats this chain as one frequency domain;
            // chip_count is carried via the ChainTuneInfo so the
            // expected-NPS prediction still scales 28..126.
            chip_nonces: vec![self.nonces],
            chip_errors: vec![self.errors],
            window_duration_s,
            timestamp: now,
            board_temp_c: self.last_temp_c,
            chip_hw_errors: Some(vec![self.errors]),
            chip_timeouts: Some(vec![0]),
            chip_duplicates: Some(vec![0]),
            current_difficulty: self.current_difficulty,
            chip_temps_c: self
                .last_temp_c
                .map(|t| vec![t; chip_count.max(1) as usize]),
            psu_power_w: None,
        };
        self.nonces = 0;
        self.errors = 0;
        self.window_start = now;
        snapshot
    }
}

/// Per-chain serial-dispatch state — the pure, host-testable nucleus of the
/// proven single-chain `run_am2_serial_dispatch_loop` per-chain logic, factored
/// out so the DUAL-CHAIN capability (`DCENT_AM2_DUAL_CHAIN_TTYS3=1`) can run the
/// IDENTICAL dispatch + nonce-attribution + dedup + BIP320 reconstruction on
/// each chain independently.
///
/// This struct owns ONLY the per-chain bookkeeping (work history, dedup set,
/// rolling job id, RX carry buffer, counters). It performs NO I/O — the caller
/// owns the `SerialChainBackend` and feeds RX bytes in / takes the work frame
/// out. That keeps every byte of UART transport (and therefore every byte of
/// the gate-OFF single-chain path) unchanged while making the share-decode
/// logic unit-testable on the host with no hardware.
///
/// Correctness contract (mirrors the proven loop verbatim):
///   * dispatch builds the 88-byte BM1362 serial frame via
///     `build_am2_serial_work_frame` and records a `WorkEntry` in the echoed
///     job-id slot (`am2_serial_echoed_job_id`).
///   * attribution resyncs to the `0xAA 0x55` preamble, drains complete 11-byte
///     frames, rejects non-job-response frames (`flags & 0x80 == 0`), dedups on
///     `(job_id, nonce, version_bits_raw)`, reconstructs the rolled version via
///     the canonical `bip320_reconstruct_rolled_version` helper, validates the
///     full header, and produces a `ValidShare` with `version_bits` + rolled
///     `version` + achieved difficulty populated per-chain.
struct Am2SerialChainState {
    /// Diagnostic chain index (0 = primary, 1 = second). Carried into logs +
    /// the per-chain freq-only autotuner snapshot chain_id.
    chain_id: u8,
    work_builder: dcentrald_stratum::share_pipeline::WorkBuilder,
    current_job: Option<dcentrald_stratum::types::JobTemplate>,
    asic_job_id: u8,
    work_history: Vec<VecDeque<WorkEntry>>,
    /// Dedup key includes version_bits_raw: BM1362 rolls BIP320 internally and
    /// can report the SAME nonce at the SAME job under DIFFERENT rolled
    /// versions — genuinely-different 80-byte headers (two valid shares). When
    /// the chip rolls 0 the key collapses to (job_id, nonce).
    seen_shares: BTreeSet<(u8, u32, u16)>,
    /// RX byte carry buffer for frame resync across reads.
    carry: Vec<u8>,
    total_work: u64,
    total_nonces: u64,
    /// AT-DASH: distinct (job_id,nonce,vbits) reports that survived dedup.
    /// `total_nonces - unique_nonces` is this chain's duplicate spam — the
    /// per-chain diagnostic for the ~400x hashrate gap.
    unique_nonces: u64,
    /// AT-DASH: full-length frames that failed BM1362 nonce parse (HW errors).
    crc_errors: u32,
    total_rx_frames: u64,
    shares_submitted: u64,
    /// Window nonce counter, reset by the caller's hashrate tick.
    hr_nonces: u64,
    first_work_at: Option<Instant>,
}

/// A share decoded + attributed from one RX frame, ready for the caller to send
/// on the shared `share_tx`. Returned by `Am2SerialChainState::ingest_frame` so
/// the I/O (the async `share_tx.send`) stays in the caller.
struct Am2AttributedShare {
    share: dcentrald_stratum::types::ValidShare,
    nonce: u32,
}

impl Am2SerialChainState {
    fn new(chain_id: u8) -> Self {
        Self {
            chain_id,
            work_builder: dcentrald_stratum::share_pipeline::WorkBuilder::new(),
            current_job: None,
            asic_job_id: 0,
            work_history: (0..128)
                .map(|_| VecDeque::with_capacity(WORK_HISTORY_PER_ID))
                .collect(),
            seen_shares: BTreeSet::new(),
            carry: Vec::with_capacity(256),
            total_work: 0,
            total_nonces: 0,
            unique_nonces: 0,
            crc_errors: 0,
            total_rx_frames: 0,
            shares_submitted: 0,
            hr_nonces: 0,
            first_work_at: None,
        }
    }

    /// Apply an incoming pool job. Mirrors the proven loop's `job_rx` arm:
    /// clean_jobs flush, flush-only pause, and the mutually-exclusive host-side
    /// vs chip-side version rolling rule (`set_version_mask(0)`).
    fn on_job(&mut self, job: dcentrald_stratum::types::JobTemplate) {
        if job.clean_jobs {
            self.work_history.iter_mut().for_each(VecDeque::clear);
            self.work_builder.reset_extranonce2();
            self.seen_shares.clear();
        }
        if job.is_flush_only() {
            self.current_job = None;
            return;
        }
        // CHIP rolls BIP320 internally on the serial path; host-side rolling is
        // mutually exclusive (a double-rolled header would not match what the
        // chip hashed). KEEP set_version_mask(0).
        self.work_builder.set_version_mask(0);
        self.current_job = Some(job);
    }

    /// Build the next work frame for this chain and record its `WorkEntry`.
    /// Returns `None` when no job is active (dispatch tick is a no-op). The
    /// returned 88-byte frame is what the caller writes to this chain's UART.
    fn next_work_frame(&mut self) -> Option<[u8; 88]> {
        let job = self.current_job.as_ref()?;
        let work = self.work_builder.next_work(job);
        let frame = build_am2_serial_work_frame(&work, self.asic_job_id);
        let slot = am2_serial_echoed_job_id(self.asic_job_id) as usize;
        let history = &mut self.work_history[slot];
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
            version_bits_per_midstate: vec![None],
            version_rolling_enabled: false,
        });
        self.asic_job_id =
            dcentrald_common::next_asic_job_id(self.asic_job_id, AM2_SERIAL_JOB_ID_STEP);
        self.total_work += 1;
        if self.first_work_at.is_none() {
            self.first_work_at = Some(Instant::now());
        }
        Some(frame)
    }

    /// Append freshly-read RX bytes and drain every complete 11-byte BM1362
    /// serial-nonce frame, attributing each to a `WorkEntry` in this chain's
    /// own history. Returns the `ValidShare`s ready to submit (worker name is
    /// applied by the caller, which owns the pool config). HW-error /
    /// unparseable full-length frames are counted via `errors_out` so the
    /// caller can feed the freq-only tuner's error signal. Pure — no I/O.
    fn ingest_rx(
        &mut self,
        rx: &[u8],
        worker_name: &str,
        errors_out: &mut u64,
    ) -> Vec<Am2AttributedShare> {
        if !rx.is_empty() {
            self.carry.extend_from_slice(rx);
        }
        let mut out = Vec::new();
        loop {
            let mut start = None;
            for i in 0..self.carry.len().saturating_sub(1) {
                if self.carry[i] == 0xAA && self.carry[i + 1] == 0x55 {
                    start = Some(i);
                    break;
                }
            }
            let Some(start) = start else {
                // No preamble — keep only the last byte (a possible split 0xAA).
                if self.carry.len() > 1 {
                    let tail = self.carry.split_off(self.carry.len() - 1);
                    self.carry = tail;
                }
                break;
            };
            if start > 0 {
                self.carry.drain(0..start);
            }
            if self.carry.len() < AM2_SERIAL_NONCE_LEN {
                break; // incomplete frame — wait for more bytes
            }
            let frame: Vec<u8> = self.carry.drain(0..AM2_SERIAL_NONCE_LEN).collect();
            self.total_rx_frames += 1;
            let Some(nr) = parse_bm1362_serial_nonce(&frame) else {
                self.crc_errors = self.crc_errors.saturating_add(1);
                *errors_out = errors_out.saturating_add(1);
                continue;
            };
            if nr.flags & 0x80 == 0 {
                continue; // not a job-response frame
            }
            let (_, vbits_delta) = am2_hybrid_reconstruct_rolled_version(0, nr.version_bits_raw);
            self.total_nonces += 1;
            self.hr_nonces += 1;
            let history = &self.work_history[nr.job_id as usize];
            if history.is_empty() {
                continue;
            }
            if !self
                .seen_shares
                .insert((nr.job_id, nr.nonce, nr.version_bits_raw))
            {
                continue; // duplicate (same job + nonce + rolled version)
            }
            // AT-DASH: dedup-survivor — a genuinely distinct nonce for this chain.
            self.unique_nonces = self.unique_nonces.saturating_add(1);
            if dcentrald_common::should_clear_seen_shares(
                self.seen_shares.len(),
                dcentrald_common::DEFAULT_SEEN_SHARES_CAP,
            ) {
                self.seen_shares.clear();
            }
            if let Some((entry, rolled_version, achieved_difficulty)) =
                history.iter().rev().find_map(|candidate| {
                    let (rolled_version, candidate_vbits_delta) =
                        am2_hybrid_reconstruct_rolled_version(
                            candidate.version,
                            nr.version_bits_raw,
                        );
                    debug_assert_eq!(candidate_vbits_delta, vbits_delta);
                    let header = hybrid_build_header(candidate, rolled_version, nr.nonce);
                    if dcentrald_stratum::share_pipeline::validate_full_header(
                        &header,
                        &candidate.share_target,
                    ) {
                        let achieved = am2_hybrid_achieved_difficulty_from_header(&header);
                        Some((candidate.clone(), rolled_version, achieved))
                    } else {
                        None
                    }
                })
            {
                self.shares_submitted += 1;
                let share = dcentrald_stratum::types::ValidShare {
                    worker_name: worker_name.to_string(),
                    job_id: entry.job_id.clone(),
                    extranonce2: entry.extranonce2.clone(),
                    ntime: format!("{:08x}", entry.ntime),
                    nonce: format!("{:08x}", nr.nonce),
                    version_bits: if vbits_delta != 0 {
                        Some(format!("{:08x}", vbits_delta))
                    } else {
                        None
                    },
                    version: rolled_version,
                    achieved_difficulty,
                };
                out.push(Am2AttributedShare {
                    share,
                    nonce: nr.nonce,
                });
            }
        }
        out
    }
}

/// Apply a broadcast PLL frequency to the whole BM1362 chain over the
/// serial UART, using EXACTLY the proven init primitive
/// (`send_write_reg_broadcast_bm1397plus(0x08, pll_reg)` +
/// `BM1362_PLL0_DIVIDER_REG` re-write). The requested frequency is
/// clamped to the proven-applicable `[400, 545]` MHz band first
/// (`am2_freq_only_clamp_applied_mhz`) so the PLL register word always
/// comes from the live-proven `BM1362_PLL_TABLE`.
///
/// This is a chain-wide broadcast (no per-chip addressing) — correct for
/// the serial path where nonce frames carry no chip address. NEVER
/// writes voltage. Returns the actually-applied frequency for the ack.
fn apply_am2_bm1362_chain_pll(serial: &SerialChainBackend, requested_mhz: u16) -> Result<u16> {
    let applied_mhz = am2_freq_only_clamp_applied_mhz(requested_mhz);
    let (pll_reg, table_mhz) = bm1362_pll_lookup(applied_mhz);
    // Same two-write settle the init's ramp/slam tail uses: re-seat the
    // PLL0 divider, then write the target PLL word.
    serial
        .send_write_reg_broadcast_bm1397plus(BM1362_PLL0_DIVIDER_REG, BM1362_TRACE_PLL0_DIVIDER)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x08, pll_reg)?;
    std::thread::sleep(Duration::from_millis(10));
    info!(
        requested_mhz,
        applied_mhz,
        table_mhz,
        pll_reg = format_args!("0x{:08X}", pll_reg),
        "am2 freq-only autotuner: broadcast PLL applied (chain-wide, no voltage write)"
    );
    Ok(table_mhz)
}

/// `DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE=1` (PR-019 / R11-2 ablation).
///
/// Default-OFF additive instrumentation for the AM2 BM1362 per-chip-init
/// 126→28 chip-collapse root cause. When set, `init_asic_chain` issues the
/// existing read-only `verify_bm1362_get_address` enumeration *after each
/// init phase* (pre-init baseline, post-A8, post-MiscCtrl×3, post-3C×3,
/// post-fast-baud-switch, pre-mining) and logs a greppable `[AM2-ABLATION]`
/// line per phase plus a single `[AM2-ABLATION-PARAMS]` 5-parameter summary
/// so successive operator AC-cycle runs can be diffed mechanically.
///
/// LOAD-BEARING: when this flag is unset the proven path's behaviour,
/// timing, and on-wire traffic are byte-for-byte unchanged — every added
/// GetAddress read AND every added log line is gated behind this check. The
/// extra reads are the exact read-only enumeration form the proven path
/// already uses for its own presence gate; they do not perturb addressing.
fn am2_verify_presence_after_each_phase_enabled() -> bool {
    am2_env_flag("DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE")
}

/// `DCENT_AM2_INIT_CHAIN_INACTIVE_FIRST=1` ( / xil-`a lab unit` chain-enum-0 fix).
///
/// Reorders the BM1362 115200 init block so `CHAIN_INACTIVE x3` +
/// `SET_CHIP_ADDRESS x126` run BEFORE the A8/MiscCtrl x3/A4 broadcasts.
/// Default-OFF — when unset, the proven path's existing order is preserved
/// byte-for-byte (`a lab unit` / `a lab unit` / `a lab unit` regression-safe).
///
/// Background: Agent A's 2026-05-22 RE swarm finding (LEAD hypothesis in
/// ):
/// bosminer's canonical chain4 order is `CHAIN_INACTIVE x3` -> `SET_CHIP_ADDRESS x126`
/// -> read enumeration count -> THEN the register-setup broadcast block.
/// DCENT_OS today does the broadcasts first; chips that haven't received an
/// address can't answer `GetAddress` -> 0/126 by construction. The XIL `a lab unit`
/// observed failure mode matches this exactly. The fix here is the cheapest
/// possible code change to test the hypothesis on live hardware: an
/// env-gated swap, fully reversible by unsetting the env var.
fn am2_init_chain_inactive_first_enabled() -> bool {
    am2_env_flag("DCENT_AM2_INIT_CHAIN_INACTIVE_FIRST")
}

/// `DCENT_AM2_PIC_RESET_NO_JUMP=1` (5 / xil-`a lab unit` SetVoltage-path fix).
///
/// When set with `DCENT_AM2_PIC_RESET_AND_START_APP=1`, Phase 0d emits only
/// the bosminer-faithful flush + 0x07 RESET + 500 ms — skipping the
/// `[0x55, 0xAA, 0x06]` JUMP_TO_APP step. The dsPIC stays in fw=0x89
/// bootloader mode for the rest of the daemon's lifecycle.
///
/// Why: the post-JUMP fw=0x82 SetVoltage frame encoding (`[0x10, hi, lo]`
/// of 16-bit mV) empirically programs the chip rail to ~2 V instead of
/// 13.7 V on `a lab unit` XIL.
/// The fw=0x89 bootloader path uses the BOS-proven framed-DAC encoding
/// (DAC=6 for 13.7 V) per `dcentrald-asic::dspic::mod.rs:framed_voltage_dac`.
///
/// Default OFF — when unset, the existing full-chain (flush + RESET +
/// JUMP) behaviour is preserved byte-for-byte. `a lab unit` / `a lab unit` /`a lab unit`
/// regression-safe.
fn am2_pic_reset_no_jump_enabled() -> bool {
    am2_env_flag("DCENT_AM2_PIC_RESET_NO_JUMP")
}

/// Parse the 4 fields PR-019's `[AM2-ABLATION]` line needs out of a
/// `summarize_bm1362_serial_responses` summary string. The summary is the
/// exact `count=.. chipid_like=.. unique_chip_addrs=.. duplicate_chipid=..
/// nonce_like=.. other=.. malformed=.. chip_preview=.. first=..` shape that
/// `verify_bm1362_get_address` already produces; this is a pure
/// string-extraction helper (no chain I/O) so it is host-unit-testable.
///
/// Returns `(total_responding, unique_chip_ids, duplicate_collisions,
/// chip_preview)`. `total_responding` is the `count=` token (total frames
/// the chain drove back), `unique_chip_ids` is `unique_chip_addrs=`,
/// `duplicate_collisions` is `duplicate_chipid=` (chipid_like minus
/// unique — the 126→28 collapse signature), and `chip_preview` is the
/// first ~8 chip-ids the summary already truncated to.
fn parse_ablation_fields(summary: &str) -> (usize, usize, usize, String) {
    fn usize_tok(summary: &str, key: &str) -> usize {
        summary
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix(key))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
    let total = usize_tok(summary, "count=");
    let unique = usize_tok(summary, "unique_chip_addrs=");
    let duplicates = usize_tok(summary, "duplicate_chipid=");
    let preview = summary
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("chip_preview="))
        .unwrap_or("none")
        .to_string();
    (total, unique, duplicates, preview)
}

/// `DCENT_AM2_LOG_RX_FRAMES=N` — when set to a positive integer, the am2
/// serial-dispatch RX loop hex-dumps the first N raw 11-byte frames (after
/// preamble alignment) along with whether `parse_bm1362_serial_nonce`
/// accepted them and what the job-response / version-rolled flag bits were.
/// Lab-only diagnostic for the 2026-05-15 .109 finding that 4655 RX frames
/// arrive in 90 s but none parse as nonces — captures the actual on-wire
/// shape vs the .79 BB serial-nonce frame the parser was tuned for. Default
/// 0 = disabled.
fn am2_log_rx_frames_max() -> u64 {
    let raw = match std::env::var("DCENT_AM2_LOG_RX_FRAMES") {
        Ok(v) => v,
        Err(_) => return 0,
    };
    raw.trim().parse::<u64>().unwrap_or(0)
}

fn am2_env_u32(name: &str, default: u32) -> u32 {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let trimmed = raw.trim();
    let parsed = if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
    } else {
        trimmed.parse::<u32>()
    };
    match parsed {
        Ok(value) => value,
        Err(e) => {
            warn!(
                env = name,
                value = trimmed,
                default,
                error = %e,
                "Invalid AM2 numeric env override; using default"
            );
            default
        }
    }
}

/// Parse a decimal `u64` AM2 env override (no hex form needed for ms values).
/// Returns `default` when absent or unparseable so a malformed override can
/// never change timing silently.
fn am2_env_u64(name: &str, default: u64) -> u64 {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let trimmed = raw.trim();
    match trimmed.parse::<u64>() {
        Ok(value) => value,
        Err(e) => {
            warn!(
                env = name,
                value = trimmed,
                default,
                error = %e,
                "Invalid AM2 numeric env override; using default"
            );
            default
        }
    }
}

/// MINE-LIFE-1: derive the GENEROUS mid-run nonce-stall fail-closed timeout.
///
/// This is SEPARATE from the startup no-nonce guard (`am2_no_nonce_timeout_s`,
/// which trips only when ZERO nonces have ever arrived). The mid-run guard trips
/// only once mining HAS produced nonces and then goes silent for longer than
/// this generous window. Returns `None` (guard disabled) when the startup
/// no-nonce guard is disabled and no explicit override is set, or when the
/// resolved value is `0`.
fn am2_mid_run_nonce_stall_timeout(no_nonce_timeout_s: u64) -> Option<Duration> {
    let default_s = if no_nonce_timeout_s == 0 {
        // Startup guard disabled (deep lab capture) ⇒ mid-run guard off too
        // unless the operator explicitly opts in via the env override.
        0
    } else {
        // GENEROUS: at least 2× the startup no-nonce timeout AND never below the
        // 5-minute floor, so a healthy eco unit producing sparse nonces cannot
        // false-trip it.
        no_nonce_timeout_s
            .saturating_mul(2)
            .max(AM2_MID_RUN_STALL_MIN_DEFAULT_S)
    };
    let secs = am2_env_u64(ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S, default_s);
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}

/// MINE-LIFE-1: pure mid-run stall decision. `true` ⇒ the loop must fail closed.
///
/// Fires ONLY when a timeout is configured AND at least one nonce has been seen
/// (`last_nonce_elapsed` is `Some`) AND the time since that last nonce meets or
/// exceeds the generous timeout. When no nonce has ever arrived
/// (`last_nonce_elapsed` is `None`) this returns `false` — that startup case is
/// owned by the separate no-nonce guard, so the two never overlap.
fn am2_mid_run_nonce_stalled(
    timeout: Option<Duration>,
    last_nonce_elapsed: Option<Duration>,
) -> bool {
    matches!((timeout, last_nonce_elapsed), (Some(t), Some(elapsed)) if elapsed >= t)
}

/// MINE-LIFE-2: is the chain "recently active" (produced a nonce within the
/// generous stall window)? Used to derive an honest live status so a stalled
/// chain stops reporting "mining". Conservative: when the guard is disabled
/// (`timeout` is `None`) or no nonce has arrived yet (`last_nonce_elapsed` is
/// `None`), returns `true` so the status logic is never falsely downgraded —
/// the proven path stays byte-equivalent.
fn am2_nonce_recently_active(
    timeout: Option<Duration>,
    last_nonce_elapsed: Option<Duration>,
) -> bool {
    match (timeout, last_nonce_elapsed) {
        (Some(t), Some(elapsed)) => elapsed < t,
        _ => true,
    }
}

/// THERM-1(a): pure 3-strike fan-fault decision. Returns `(new_strikes, faulted)`.
///
/// A strike is added ONLY for a confident, TEMPERATURE-CORROBORATED observation
/// that the fan was commanded (`commanded_pwm > 0`) yet ALL fans read 0 RPM
/// (`max_rpm == 0`) AND the unit is actually running warm (`temp_c` finite and
/// `>= hot_c`). The temperature gate is the load-bearing false-cut guard: a
/// confirmed-cool unit (`temp_c < hot_c`) is provably cooling adequately
/// REGARDLESS of what the tach reports, so it must never accumulate a strike —
/// the `Am2Uio16` tach is only live-verified on `a lab unit` (which itself read 0 on
/// some channels before the C49→C52 fan-mode fix), so a sister am2-zynq unit
/// (`a lab unit`/`a lab unit`) with an unverified / wrong-mode / transient-zero tach would
/// otherwise be cut while running cool and adequately cooled. Any airflow, any
/// zero command, an inconclusive read (`reading == None`, e.g. the fan UIO could
/// not be opened), a non-finite temperature, OR a cool temperature RESETS the
/// counter, so a healthy, cool, or flaky unit can never accumulate to a
/// fail-closed cut. The genuine-failure protection is preserved: a truly stalled
/// fan → temp climbs → reaches `hot_c` → strikes accumulate → cut, still before
/// the dangerous-threshold hard-cut.
fn am2_fan_fault_step(
    strikes: u8,
    reading: Option<(u32, u8)>,
    temp_c: f32,
    hot_c: f32,
) -> (u8, bool) {
    let next = match reading {
        Some((max_rpm, commanded_pwm))
            if commanded_pwm > 0 && max_rpm == 0 && temp_c.is_finite() && temp_c >= hot_c =>
        {
            strikes.saturating_add(1)
        }
        _ => 0,
    };
    (next, next >= AM2_FAN_FAULT_STRIKES)
}

/// THERM-1(b): pure graded thermal-throttle target (MHz), or `None` for no change.
///
/// Between `hot_c` and the dangerous-temp hard cut the chain sheds hash in small
/// steps instead of mining full-frequency until the cut. Down-only: never raises
/// frequency, and the caller NEVER raises fans (cut-hash-before-noise).
/// Conservative invariants keep the proven eco path untouched:
///   * `None` when `temp_c` is non-finite or below `hot_c` (the ~50 MHz / ~49 C
///     eco unit never reaches hot, so this never fires);
///   * `None` once `current_mhz <= floor_mhz`. This means the `a lab unit` eco ~50 MHz
///     chip (well below the 400 MHz PLL floor) is NEVER PLL-throttled, so it can
///     never be accidentally clocked UP toward the floor — only genuinely
///     high-frequency units (e.g. `a lab unit`/`a lab unit` at 525 MHz) step down.
fn am2_graded_throttle_target_mhz(
    current_mhz: u16,
    temp_c: f32,
    hot_c: f32,
    floor_mhz: u16,
    step_mhz: u16,
) -> Option<u16> {
    if !temp_c.is_finite() || temp_c < hot_c {
        return None;
    }
    if current_mhz <= floor_mhz {
        return None;
    }
    let target = current_mhz.saturating_sub(step_mhz).max(floor_mhz);
    (target < current_mhz).then_some(target)
}

/// THERM-1(b): is the active graded-frequency throttle opted in? Default-OFF.
fn am2_thermal_graded_throttle_enabled() -> bool {
    am2_env_flag(ENV_AM2_THERMAL_GRADED_THROTTLE)
}

/// Current effective am2 chain frequency for telemetry + the graded throttle:
/// the RE-018-applied freq when known (the `a lab unit` eco path sets it), else the
/// configured nominal. Mirrors `build_am2_chain_state`'s frequency reporting.
fn am2_current_effective_freq_mhz(config: &DcentraldConfig) -> u16 {
    let applied = AM2_RE018_APPLIED_FREQ_MHZ.load(Ordering::Relaxed);
    if applied > 0 {
        applied
    } else {
        config.mining.frequency_mhz
    }
}

/// THERM-1(a): read max fan RPM + max commanded PWM via a read-only fan-UIO open
/// (NO board-control write — C52 fan mode is already set at cold boot). Returns
/// `None` when the fan UIO can't be opened (inconclusive — NEVER treated as a
/// fault). Telemetry only; never commands a PWM.
fn am2_read_fan_rpm_and_pwm() -> Option<(u32, u8)> {
    let disc = dcentrald_hal::fan::discover_fan_uio()?;
    let regs = dcentrald_hal::uio::UioDevice::open(disc.uio_number).ok()?;
    let fan = dcentrald_hal::fan::FanController::new_with_variant(
        regs,
        dcentrald_hal::fan::FanVariant::Am2Uio16,
    );
    let max_rpm = fan
        .get_per_fan_rpm()
        .iter()
        .map(|(_, rpm)| *rpm)
        .max()
        .unwrap_or(0);
    let (p0, p1) = fan.get_speed_pwm_channels();
    Some((max_rpm, p0.max(p1)))
}

fn am2_fast_uart_value() -> u32 {
    am2_env_u32("DCENT_AM2_FAST_UART_VALUE", FAST_UART_VALUE)
}

/// Bosminer's MiscCtrl value on the `a lab unit` cold path: `0xB000_C100`. Our plan
/// defaults differ (`MISC_CONTROL_PRE_BAUD=0xFF0F_C100`,
/// `MISC_CONTROL_POST_FAST_BAUD=0x00C1_00B0`). Captured by the RE swarm
/// (`wf_b7891b82-31f`) from the healthy bosminer `a lab unit`/`a lab unit` cold-boot trace.
const MISC_CONTROL_BOSMINER: u32 = 0xB000_C100;

/// Bosminer's cold A8/INIT_CONTROL broadcast value. On a true `a lab unit` cold-boot
/// bosminer's first healthy chain4 A8 write is `0x0000_0000`, NOT our
/// `INIT_CONTROL_BCAST=0x0007_0000` plan default.
const BM1362_COLD_BROADCAST_BOSMINER_A8: u32 = 0x0000_0000;

/// `DCENT_AM2_BM1362_COLD_BROADCAST_BOSMINER=1` (RE swarm `wf_b7891b82-31f`
/// Candidate #2) — replace the cold 115200 broadcast A8 + MiscCtrl-pre values
/// with bosminer's captured cold values (A8=`0x0000_0000`,
/// MiscCtrl=`0xB000_C100`). Default-OFF: when unset, the `a lab unit`-proven
/// `INIT_CONTROL_BCAST` / `MISC_CONTROL_PRE_BAUD` defaults are byte-identical,
/// so the `a lab unit` first-shares baseline and the  handoff are unaffected.
fn am2_bm1362_cold_broadcast_bosminer() -> bool {
    am2_env_flag("DCENT_AM2_BM1362_COLD_BROADCAST_BOSMINER")
}

/// A8/INIT_CONTROL broadcast value for the cold 115200 block. Default is the
/// `a lab unit`-proven `INIT_CONTROL_BCAST`; the bosminer-captured `0x0000_0000` is
/// opt-in behind `DCENT_AM2_BM1362_COLD_BROADCAST_BOSMINER`.
fn am2_init_control_bcast() -> u32 {
    if am2_bm1362_cold_broadcast_bosminer() {
        BM1362_COLD_BROADCAST_BOSMINER_A8
    } else {
        INIT_CONTROL_BCAST
    }
}

/// MiscCtrl value written pre-baud (triple-write). Default
/// `MISC_CONTROL_PRE_BAUD`; bosminer's `0xB000_C100` is opt-in behind
/// `DCENT_AM2_BM1362_COLD_BROADCAST_BOSMINER`.
fn am2_misc_control_pre_baud() -> u32 {
    if am2_bm1362_cold_broadcast_bosminer() {
        MISC_CONTROL_BOSMINER
    } else {
        MISC_CONTROL_PRE_BAUD
    }
}

/// MiscCtrl value written immediately after the FastUART register write
/// (triple-write, broadcast + per-chip). Default `MISC_CONTROL_POST_FAST_BAUD`
/// (`0x00C1_00B0`); bosminer's `0xB000_C100` is opt-in behind
/// `DCENT_AM2_MISC_CTRL_POST_FAST_BOSMINER` (RE swarm `wf_b7891b82-31f`
/// Candidate #1 — pairs with `DCENT_AM2_FAST_UART_VALUE=0x11300000`, the
/// byte-order-corrected FastUART value the wave52 fast-baud enum=0 implicated).
/// Default-OFF: when unset the `a lab unit`-proven post-fast value is byte-identical.
fn am2_misc_control_post_fast() -> u32 {
    if am2_env_flag("DCENT_AM2_MISC_CTRL_POST_FAST_BOSMINER") {
        MISC_CONTROL_BOSMINER
    } else {
        MISC_CONTROL_POST_FAST_BAUD
    }
}

fn am2_fast_uart_baud() -> u32 {
    am2_env_u32(
        "DCENT_AM2_FAST_UART_BAUD",
        dcentrald_hal::serial::BAUD_3125000,
    )
}

/// Default host-side settle delay (ms) after the 115200→3.125M host UART
/// switch, BEFORE the first fast-baud GetAddress probe. The chip UART
/// divider needs time to re-lock after the FastUART register write; 300 ms
/// was live-proven too short on `a lab unit`, ~1000 ms is the canonical value.
/// This is the R11-7 #1 live-tuning lever
///.
const AM2_FASTUART_SETTLE_MS_DEFAULT: u64 = 1000;
/// Clamp floor for the settle delay (ms).
const AM2_FASTUART_SETTLE_MS_MIN: u64 = 100;
/// Clamp ceiling for the settle delay (ms).
const AM2_FASTUART_SETTLE_MS_MAX: u64 = 3000;

/// Resolve the post-fast-baud-switch settle delay (ms).
///
/// `DCENT_AM2_FASTUART_SETTLE_MS` overrides the [`AM2_FASTUART_SETTLE_MS_DEFAULT`]
/// hardcoded value, CLAMPED to `[100, 3000]`. ABSENT env ⇒ byte-identical
/// behaviour (returns the default 1000 ms). This gates ONLY the host-side
/// settle sleep — the 0x28/0x18/MiscCtrl sequence, the fail-closed GetAddress
/// sentinel, and every other timing are untouched. Makes the R11-7 live
/// settle-delay sweep on `a lab unit` turnkey without N rebuilds.
fn am2_fast_uart_settle_ms() -> u64 {
    let raw = am2_env_u64(
        "DCENT_AM2_FASTUART_SETTLE_MS",
        AM2_FASTUART_SETTLE_MS_DEFAULT,
    );
    let clamped = raw.clamp(AM2_FASTUART_SETTLE_MS_MIN, AM2_FASTUART_SETTLE_MS_MAX);
    info!(
        env = "DCENT_AM2_FASTUART_SETTLE_MS",
        requested_ms = raw,
        settle_ms = clamped,
        default_ms = AM2_FASTUART_SETTLE_MS_DEFAULT,
        min_ms = AM2_FASTUART_SETTLE_MS_MIN,
        max_ms = AM2_FASTUART_SETTLE_MS_MAX,
        "BM1362 post-fast-baud-switch host settle delay resolved"
    );
    clamped
}

/// Default post-chain-reset / pre-enumeration settle delay (ms) — the gap
/// between the chain UART coming up (HB_RESET released, serial port open at
/// 115200) and the FIRST CHAIN_INACTIVE / SET_ADDRESS broadcast. Historically
/// hardcoded at 50 ms.
const AM2_POST_RESET_SETTLE_MS_DEFAULT: u64 = 50;
/// Clamp floor for the post-reset settle delay (ms) — the proven 50 ms.
const AM2_POST_RESET_SETTLE_MS_MIN: u64 = 50;
/// Clamp ceiling for the post-reset settle delay (ms) — generous for cold
/// chip-wake; well below any thermal concern (no hashing happens here).
const AM2_POST_RESET_SETTLE_MS_MAX: u64 = 5000;

/// Resolve the post-chain-reset / pre-enumeration settle delay (ms).
///
///  (2026-05-31) — the `a lab unit` standalone cold-wake P1 lever. The
/// 7-agent swarm (+ the  HB_RESET third-mechanism RE) converged on:
/// the enum=0 blocker is chip-wake, NOT the PSU/Loki layer (the rail is up via
/// PWR_CONTROL on the dumb APW3; DCENT does not gate its flow on the APW12
/// spoof). bosminer's cold choreography holds HB_RESET LOW, releases it, then
/// waits **~1 s** before its first (blind) address — DCENT waited only the
/// hardcoded 50 ms. This env-gated knob lets the operator extend that
/// post-release settle to bosminer-faithful ~1 s WITHOUT a rebuild per sweep.
///
/// `DCENT_AM2_POST_RESET_SETTLE_MS` overrides the [`AM2_POST_RESET_SETTLE_MS_DEFAULT`]
/// 50 ms, CLAMPED to `[50, 5000]`. ABSENT env ⇒ byte-identical behaviour (50 ms),
/// so `a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9 are untouched. SAFE: this sleep sits before
/// any chip enumerates, so 0 chips = no hashing = no thermal risk; the home fan
/// cap (PWM 30) stands on every teardown regardless.
fn am2_post_reset_settle_ms() -> u64 {
    let raw = am2_env_u64(
        "DCENT_AM2_POST_RESET_SETTLE_MS",
        AM2_POST_RESET_SETTLE_MS_DEFAULT,
    );
    let clamped = raw.clamp(AM2_POST_RESET_SETTLE_MS_MIN, AM2_POST_RESET_SETTLE_MS_MAX);
    if clamped != AM2_POST_RESET_SETTLE_MS_DEFAULT {
        info!(
            env = "DCENT_AM2_POST_RESET_SETTLE_MS",
            requested_ms = raw,
            settle_ms = clamped,
            default_ms = AM2_POST_RESET_SETTLE_MS_DEFAULT,
            "BM1362 post-chain-reset / pre-enumeration settle delay resolved \
             (bosminer-faithful cold chip-wake; default 50 ms unchanged elsewhere)"
        );
    }
    clamped
}

/// 2026-05-31 — opt-in env gate for the AM2 `a lab unit` open-core voltage-ramp
/// experiment (ported from the proven AM3-BB `a lab unit` open-core rail stage).
///
/// When `DCENT_AM2_OPEN_CORE_VOLTAGE=1` AND the `a lab unit` fingerprint matches AND
/// the lab over-volt cap is lifted, the Phase-3 dsPIC `cold_boot_init` targets
/// an explicitly configured elevated `s19j_open_core_mv()` instead of the steady
/// 13700 mV — so the BM1362 chip string enumerates AT the open-core voltage,
/// matching what LuxOS/`a lab unit` (same BM1362) + bosminer + the AMTC fixture do on
/// every cold boot. After enum > 0, Phase 4-7 ramps the rail back DOWN to
/// `S19J_HYBRID_CHIP_RAIL_TARGET_MV` (13700) for steady operation.
///
/// **Default off.** Absent env ⇒ byte-identical behaviour on every unit
/// (`a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/S9/handoff): the 13700 mV path is unchanged.
/// The gate is also `a lab unit`-fingerprinted (`am2_xil_25_fingerprint_matches`) so
/// even with the env set, no other AM2 unit elevates.
fn am2_open_core_voltage_enabled() -> bool {
    am2_env_flag("DCENT_AM2_OPEN_CORE_VOLTAGE")
}

/// Resolve the AM2 `a lab unit` open-core chip-rail target voltage (mV).
///
/// Reads `DCENT_AM2_OPEN_CORE_MV` (default = the steady
/// `S19J_HYBRID_CHIP_RAIL_TARGET_MV` 13700), clamped to
/// `[S19J_HYBRID_CHIP_RAIL_TARGET_MV, S19J_OPEN_CORE_MAX_MV]` =
/// `[13700, 15140]`. Returns the steady 13700 mV UNLESS the open-core
/// experiment gate is active — `am2_open_core_voltage_enabled()` AND the
/// `a lab unit` fingerprint matches. This keeps the elevated voltage strictly
/// `a lab unit`-only and behind the explicit env flag; everything else stays at
/// 13700 and the call sites remain byte-identical to today. Admission later
/// requires the resolved value to be strictly greater than steady, so merely
/// enabling the experiment without an explicit elevated target fails closed.
///
/// SAFETY: the returned value is later fed to `pic.cold_boot_init_with_options`
/// / `pic.set_voltage`, where `clamp_dspic_voltage_to_hard_cap` SILENTLY clamps
/// anything above `DSPIC_VOLTAGE_HARD_CAP_MV` (14500) DOWN unless
/// `DCENT_AM2_ALLOW_LAB_OVERVOLT=1` is set. The compound gate at the call site
/// requires the lab over-volt flag and emits a LOUD `warn!` if the open-core
/// env is set without it (so a silent clamp is always explained in the log).
fn s19j_open_core_mv() -> u16 {
    if !(am2_open_core_voltage_enabled() && am2_xil_25_fingerprint_matches()) {
        return S19J_HYBRID_CHIP_RAIL_TARGET_MV;
    }
    let raw = am2_env_u64(
        "DCENT_AM2_OPEN_CORE_MV",
        u64::from(S19J_HYBRID_CHIP_RAIL_TARGET_MV),
    );
    let clamped = raw.clamp(
        u64::from(S19J_HYBRID_CHIP_RAIL_TARGET_MV),
        u64::from(S19J_OPEN_CORE_MAX_MV),
    ) as u16;
    if clamped != S19J_HYBRID_CHIP_RAIL_TARGET_MV {
        info!(
            env = "DCENT_AM2_OPEN_CORE_MV",
            requested_mv = raw,
            open_core_mv = clamped,
            steady_mv = S19J_HYBRID_CHIP_RAIL_TARGET_MV,
            max_mv = S19J_OPEN_CORE_MAX_MV,
            "AM2 .25 open-core chip-rail target resolved (default 13700 unchanged elsewhere)"
        );
    }
    clamped
}

/// True when the AM2 `a lab unit` open-core voltage-ramp experiment is fully armed:
/// the env flag is set, the `a lab unit` fingerprint matches, AND the dsPIC lab
/// over-volt cap is lifted so the elevated target survives the
/// `DSPIC_VOLTAGE_HARD_CAP_MV` (14500) input clamp.
///
/// If the open-core env is set + `a lab unit` matches but the over-volt cap is NOT
/// lifted, emits a LOUD `warn!` (so the operator sees WHY the rail will be
/// silently clamped to ~14500) and returns `false` — the steady 13700 mV path
/// runs unchanged. Default-OFF (env unset) ⇒ silent, byte-identical.
fn am2_open_core_gate_active() -> bool {
    if !(am2_open_core_voltage_enabled() && am2_xil_25_fingerprint_matches()) {
        return false;
    }
    if !dcentrald_asic::dspic::dspic_lab_overvolt_override_enabled() {
        warn!(
            env = "DCENT_AM2_OPEN_CORE_VOLTAGE",
            overvolt_env = dcentrald_asic::dspic::DSPIC_ALLOW_LAB_OVERVOLT_ENV,
            hard_cap_mv = dcentrald_asic::dspic::DSPIC_VOLTAGE_HARD_CAP_MV,
            requested_open_core_mv = s19j_open_core_mv(),
            "DCENT_AM2_OPEN_CORE_VOLTAGE is set on .25 but the dsPIC lab over-volt cap is NOT lifted \
             — any open-core target above {} mV will be SILENTLY CLAMPED to {} mV at the rail boundary. \
             Set {}=1 to actually reach the open-core voltage. Open-core ramp NOT armed.",
            dcentrald_asic::dspic::DSPIC_VOLTAGE_HARD_CAP_MV,
            dcentrald_asic::dspic::DSPIC_VOLTAGE_HARD_CAP_MV,
            dcentrald_asic::dspic::DSPIC_ALLOW_LAB_OVERVOLT_ENV,
        );
        return false;
    }
    true
}

fn maybe_write_bm1362_uart_relay(serial: &SerialChainBackend, stage: &'static str) -> Result<()> {
    if !bm1362_uart_relay_lab_enabled() {
        warn!(
            stage,
            "BM1362 UART_RELAY reg 0x2C/0x34 writes skipped by default; set DCENT_BM1362_ENABLE_UART_RELAY_LAB=1 only for R6-7 capture work"
        );
        return Ok(());
    }

    serial.send_write_reg_broadcast_bm1397plus(BM1362_UART_RELAY_REG, BM1362_UART_RELAY_ENABLE)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(
        BM1362_UART_RELAY_REG_ALT,
        BM1362_UART_RELAY_ENABLE_ALT,
    )?;
    std::thread::sleep(Duration::from_millis(10));
    info!(stage, "BM1362 UART_RELAY lab-gated broadcast sent");
    Ok(())
}

/// Non-destructive post-ENABLE chain UART rail-engagement probe.
///
/// APW121215a (FW `0x71`) has NO voltage/current/power feedback, and dsPIC
/// fw=0x86 in bare protocol returns only its FW echo byte for any read —
/// including GET_VOLTAGE (0x3B). The ENABLE_VOLTAGE bare ACK only confirms
/// protocol-level acceptance, NOT actual rail engagement. The only software
/// signal that the chain DC-DC has actually engaged 13.7 V is whether the
/// BM1362 ASICs themselves drive any byte onto the chain UART RX line.
///
/// This probe opens the chain UART via the same `DevmemUart` infrastructure
/// Phase 4-7 uses, sleeps long enough for the DC-DC to ramp, drains RX for
/// up to 500 ms, and logs the byte count + first-up-to-16-byte preview.
///
/// - `rx_bytes_pre_init == 0`: no early UART evidence. This does NOT prove rail
///   failure and must not be attributed to the falsified chip-register relay
///   hypothesis. On `a lab unit`, the low-bit GPIO relay state can match bosminer and
///   still produce zero bytes.
/// - `rx_bytes_pre_init > 0`: early chain UART evidence exists. GetAddress
///   enumeration remains the authoritative success signal.
///
/// This is a fail-closed rail gate. Phase 4-7 will reopen the UART for formal
/// init only after this probe proves the ASIC chain is electrically alive.
fn post_enable_chain_uart_probe(
    chain_id: u8,
    chain_uart_device: &str,
    selected_pic_addr: u8,
) -> Result<usize> {
    use dcentrald_hal::serial::DevmemUart;

    // Sleep 200 ms after ENABLE so the DC-DC has time to ramp and the
    // BM1362s have had time to drive any spontaneous start-up bytes onto
    // the UART RX line (idle/preamble/echo of inherited state).
    std::thread::sleep(Duration::from_millis(200));

    let uart = match DevmemUart::open_preserve_state(chain_uart_device, 115_200) {
        Ok(u) => u,
        Err(e) => {
            warn!(
                error = %e,
                chain_uart_device,
                "Post-ENABLE chain UART probe: DevmemUart::open failed — \
                 failing closed before Phase 4-7"
            );
            return Err(e).context("Post-ENABLE chain UART probe failed to open UART");
        }
    };

    // Drain RX for up to 500 ms, accumulating bytes into a small buffer.
    // Cap the captured count well above any reasonable startup chatter so
    // we never spin forever on a flooded UART.
    let mut buf = [0u8; 256];
    let total = uart.read_bytes_timeout(&mut buf, 500);

    let preview_len = total.min(16);
    let preview = &buf[..preview_len];

    tracing::info!(
        chain_id,
        chain_uart_device,
        selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
        rx_bytes_pre_init = total,
        rx_preview = format!("{:02X?}", preview),
        "Post-ENABLE chain UART rail-evidence probe (rx_bytes>0 is early UART evidence; GetAddress remains authoritative)"
    );

    if total == 0 {
        warn!(
            chain_id,
            chain_uart_device,
            pic_addr = format_args!("0x{:02X}", selected_pic_addr),
            "Post-ENABLE chain UART probe: 0 bytes in 500 ms - proceeding to \
             BM1362 init anyway. This gate is informational, NOT a precondition. \
             The chip-register relay hypothesis (BM1362 0x2C/0x34) is falsified \
             for .25 and remains disabled; authoritative success is the \
             GetAddress enumeration count in init_asic_chain, not 500ms \
             post-ENABLE chatter."
        );
        // Return Ok(0) so cold-boot proceeds to init_asic_chain. The relay-enable
        // serial broadcast there + BM1362 chip enumeration is the real gate.
    } else {
        info!(
            chain_id,
            chain_uart_device,
            rx_bytes_pre_init = total,
            "Post-ENABLE chain UART probe: chain is electrically alive — \
             BM1362 init may still need adjustment but the rail is up."
        );
    }

    // `uart` drops here, releasing the mmap. Phase 4-7 will reopen for init.
    Ok(total)
}

fn is_known_pic_fw(version: u8) -> bool {
    matches!(version, 0x82 | 0x86 | 0x89 | 0x8A | 0xB9 | 0xFE)
}

fn detected_dspic_fw_allows_voltage_commands(detected_fw: u8, trust_degraded_fw: bool) -> bool {
    let detected_firmware = pic0x89_firmware_from_observed_fw_byte(Some(detected_fw));
    dspic_voltage_command_allowed(detected_firmware, trust_degraded_fw)
}

fn observe_am2_endpoint_firmware(
    i2c: &I2cServiceHandle,
    presence: &Am2HashboardPresence,
    endpoint_slot: &mut Option<VoltageControllerEndpoint>,
) -> Result<u8> {
    let endpoint = discover_am2_controller_endpoint(i2c, presence)?;
    let firmware = endpoint
        .observed_firmware()
        .context("AM2 controller endpoint omitted firmware evidence")?;
    *endpoint_slot = Some(endpoint);
    Ok(firmware)
}

#[cfg(test)]
fn is_shift_left_pic_artifact(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf.windows(2).all(|w| w[1] == w[0].wrapping_shl(1))
}

#[cfg(test)]
fn is_repeated_known_fw_pic_artifact(buf: &[u8]) -> bool {
    buf.len() > 1 && is_known_pic_fw(buf[0]) && buf.iter().all(|&b| b == buf[0])
}

#[cfg(test)]
fn classify_pic_reply(buf: &[u8]) -> &'static str {
    if buf.is_empty() {
        "empty"
    } else if buf.iter().all(|&b| b == 0x00) {
        "all-zero"
    } else if buf.iter().all(|&b| b == 0xFF) {
        "all-ff"
    } else if is_shift_left_pic_artifact(buf) {
        "shift-left-bus-noise"
    } else if is_repeated_known_fw_pic_artifact(buf) {
        "repeated-fw-byte-bus-noise"
    } else if parse_hybrid_pic_fw_reply(buf).is_some() {
        "valid-fw"
    } else {
        "unknown"
    }
}

#[cfg(test)]
fn format_pic_probe_samples(samples: &[String]) -> String {
    samples
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
fn parse_hybrid_pic_fw_reply(buf: &[u8]) -> Option<u8> {
    dcentrald_hal::platform::am2_controller::parse_am2_pic_firmware_reply(buf)
}
#[cfg(test)]
fn pic_get_version_transaction_steps(
    frame: &[u8],
    read_len: usize,
    flush_first: bool,
) -> Vec<I2cTransactionStep> {
    dcentrald_hal::platform::am2_controller::am2_get_version_transaction_steps(
        frame,
        read_len,
        flush_first,
    )
}
#[cfg(test)]
fn collect_single_byte_reads(reads: Vec<Vec<u8>>) -> Vec<u8> {
    reads
        .into_iter()
        .filter_map(|read| read.first().copied())
        .collect()
}

/// bosminer-faithful clean-frame retry budget for GET_VERSION. bosminer
/// (`braiins_power.rs::I2cBackend::read`) retries the WHOLE clean frame up to
/// `I2C_NUM_RETRIES = 15` spaced `I2C_RETRY_DELAY = 100 ms` on a hard bus
/// error (EIO) — it never injects a speculative zero-byte parser flush between
/// tries (the flush is what wedges the dsPIC MSSP parser → all-FF).
#[cfg(test)]
const PIC_GET_VERSION_CLEAN_RETRIES: u32 = 15;
#[cfg(test)]
const PIC_GET_VERSION_RETRY_DELAY_MS: u64 = 100;

#[cfg(test)]
fn pic_read_fw_version_service(i2c: &I2cServiceHandle, addr: u8) -> Result<u8> {
    // Service-only three-phase probing: write -> quiet window -> byte-wise read.
    // Do not use I2C_RDWR here; it changes the `a lab unit` dsPIC failure mode.
    //
    // RE-aligned 2026-05-21 (R1/R2/R5,
    // ):
    // behave like bosminer = do LESS to the bus. The previous probe pre-flushed
    // 8/16 zero bytes and bulk-read 5 framed bytes across 2 encodings (6
    // attempts); DCENT's own `a lab unit` evidence (the early-probe note ~line 4285)
    // proves the speculative zero-flush wedges the dsPIC MSSP parser → all-FF,
    // and over-reading past the 1 staged version byte invites the kernel xiic
    // shift-left tail / idle-FF. The fix:
    //   R1: the FIRST attempt is bosminer-faithful — one clean framed write
    //       [55 AA 04 17 00 1B] then ONE 1-byte read, NO speculative flush.
    //   R2: the framed read length is 1 (the dsPIC stages exactly one byte).
    //   R5: re-issue the CLEAN whole frame (no inter-attempt zero-flush) up to
    //       `PIC_GET_VERSION_CLEAN_RETRIES` × @`PIC_GET_VERSION_RETRY_DELAY_MS`
    //       on a hard transaction error (EIO). A valid reply returns
    //       immediately; an FF/garbage-but-no-error reply is retried within the
    //       same clean budget.
    // The short [55 AA 17] form stays as a fallback but is ALSO issued WITHOUT
    // a preceding flush. No RESET/JUMP is ever emitted (feedback_pic_no_reset_s19j).
    // This is fail-closed-safe: worst case is still all-FF → the caller's
    // trust-rail / fw-whitelist gates refuse voltage. Best case: the dsPIC
    // versions cleanly and mining is unblocked.
    const GET_VERSION_FRAMED: [u8; 6] = [0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B];
    const GET_VERSION_SHORT: [u8; 3] = [0x55, 0xAA, 0x17];
    //  (2026-05-23, strace-derived): when bosminer-plus-tuner 0.9.0
    // on `a lab unit` reads GET_VERSION it gets a **4-byte response**
    // `[0x17, FW, 0x00, CKSUM]` (opcode echo + fw byte at INDEX 1 + status +
    // checksum) — NOT the 1-byte response R1/R2 documented above. R2 was
    // accurate for fw=0x86 / older bosminer firmware lineages but the newer
    // bosminer-plus-tuner 0.9.0 stages all 4 bytes. Reading only 1 returns
    // the opcode-echo byte `0x17` which `parse_hybrid_pic_fw_reply` correctly
    // rejects as "not a known fw byte" → caller errors. Strace evidence:
    // `bosminer-strace-init-full.log` lines 13178-13184 (PWR/1: read 4 bytes
    // `[17 89 00 a5]` after `[55 AA 04 17 00 1B]` write).
    //
    // When `DCENT_AM2_GET_VERSION_FRAMED_4B=1` we INSERT a 4-byte-read
    // framed probe BEFORE the existing 1-byte probes — that's the
    // bosminer-plus-tuner 0.9.0 byte-exact read shape. On older lineages
    // the 4-byte read either succeeds with a known fw byte at index 1 OR
    // returns garbage that `parse_strace_derived_pic_fw_reply` rejects → we
    // fall through to the original 1-byte probes. Default-off so unset env
    // = byte-identical to today. Companion to 's framed reset+start-app
    // gate `DCENT_AM2_PIC_RESET_STRACE_DERIVED`.
    let probes_strace_first: [(&str, &[u8], usize); 3] = [
        // 2026-06-07: read 5 (was 4). The true-cold GET_VERSION reply is the
        // 5-byte `[05 17 89 00 A5]` (leading len 0x05, trailing CRC 0xA5 =
        // 0x1C+0x89). Reading only 4 leaves the 0xA5 in the dsPIC TX FIFO to
        // corrupt the next transaction. The parser at ~:3648 keys on
        // `buf[0]==0x05 && buf[1]==0x17 && is_known_pic_fw(buf[2])` so it still
        // extracts 0x89; reading 5 just drains the trailing byte clean.
        ("framed-55aa0417001b-read5-strace", &GET_VERSION_FRAMED, 5),
        ("framed-55aa0417001b-bytewise", &GET_VERSION_FRAMED, 1),
        ("short-55aa17-bytewise", &GET_VERSION_SHORT, 1),
    ];
    let probes_original: [(&str, &[u8], usize); 2] = [
        ("framed-55aa0417001b-bytewise", &GET_VERSION_FRAMED, 1),
        ("short-55aa17-bytewise", &GET_VERSION_SHORT, 1),
    ];
    let use_strace_first = am2_get_version_framed_4b_enabled();
    // (variant, frame, read_len) — read_len = 1 for both (R2). NO flush in
    // either probe's steps.
    let probes: &[(&str, &[u8], usize)] = if use_strace_first {
        &probes_strace_first
    } else {
        &probes_original
    };
    let mut samples = Vec::new();

    for (variant, frame, read_len) in probes.iter().copied() {
        for attempt in 1..=PIC_GET_VERSION_CLEAN_RETRIES {
            // R1/R5: always a clean whole-frame write + 1-byte read; never a
            // speculative zero-flush before it.
            let reply = match i2c.transaction_mutating(
                I2cMutationLabel::QueryPrelude,
                addr,
                pic_get_version_transaction_steps(frame, read_len, false),
            ) {
                Ok(reads) => {
                    let reply = collect_single_byte_reads(reads);
                    if reply.is_empty() {
                        warn!(
                            attempt,
                            variant, "PIC service GET_VERSION transaction returned no read"
                        );
                        std::thread::sleep(Duration::from_millis(PIC_GET_VERSION_RETRY_DELAY_MS));
                        continue;
                    }
                    reply
                }
                Err(e) => {
                    // Hard bus error (EIO): bosminer's whole-frame retry case.
                    samples.push(format!("{}#{}:transaction-error:{}", variant, attempt, e));
                    warn!(attempt, variant, error = %e, "PIC service GET_VERSION transaction failed (hard EIO); clean whole-frame retry");
                    std::thread::sleep(Duration::from_millis(PIC_GET_VERSION_RETRY_DELAY_MS));
                    continue;
                }
            };

            let class = classify_pic_reply(&reply);
            samples.push(format!("{}#{}:{}:{:02X?}", variant, attempt, class, reply));
            if let Some(fw) = parse_hybrid_pic_fw_reply(&reply) {
                info!(
                    attempt,
                    variant,
                    class,
                    fw = format_args!("0x{:02X}", fw),
                    raw = format_args!("{:02X?}", reply),
                    "PIC service GET_VERSION OK"
                );
                return Ok(fw);
            }

            warn!(
                attempt,
                variant,
                class,
                raw = format_args!("{:02X?}", reply),
                "PIC service GET_VERSION did not return a valid firmware reply; clean whole-frame retry"
            );
            std::thread::sleep(Duration::from_millis(PIC_GET_VERSION_RETRY_DELAY_MS));
        }
    }

    Err(anyhow::anyhow!(
        "PIC service GET_VERSION failed at 0x{:02X}: no valid framed/short 0x17 response after {} clean whole-frame retries per variant (no speculative zero-flush; bosminer-faithful); recent samples: {}",
        addr,
        PIC_GET_VERSION_CLEAN_RETRIES,
        format_pic_probe_samples(&samples),
    ))
}

/// True dsPIC `dumy_read`: zero-write, single 1-byte read. Per
///  and the .74 live test on
/// 2026-04-29, fw=0x86 stably echoes its FW byte (0x86) on bare 1-byte
/// reads even when the framed and short GET_VERSION probes both return
/// kernel xiic-i2c bus-noise. Used as the FINAL trust-rail fallback to
/// recover the FW byte when the standard probes fail. Two consecutive
/// reads must agree on a known FW byte (0x82/0x86/0x89/0x8A) to count
/// as stable — this is the same liveness check `dspic-flash proto-probe`
/// uses (see dspic_flash.rs::probe_protocol).
fn pic_dumy_read_fw_byte(i2c: &I2cServiceHandle, addr: u8) -> Result<u8> {
    let mut samples = Vec::new();
    for attempt in 1..=3 {
        let r1 = i2c.transaction_mutating(
            I2cMutationLabel::QueryPrelude,
            addr,
            vec![
                I2cTransactionStep::SetTimeout(10),
                I2cTransactionStep::Read(1),
            ],
        );
        let r2 = i2c.transaction_mutating(
            I2cMutationLabel::QueryPrelude,
            addr,
            vec![I2cTransactionStep::SleepMs(20), I2cTransactionStep::Read(1)],
        );
        if let (Ok(mut a), Ok(mut b)) = (r1, r2) {
            let ba = a.pop().and_then(|v| v.first().copied());
            let bb = b.pop().and_then(|v| v.first().copied());
            samples.push(format!("dumy#{}:[{:02X?},{:02X?}]", attempt, ba, bb));
            if let (Some(x), Some(y)) = (ba, bb) {
                if x == y && matches!(x, 0x82 | 0x86 | 0x89 | 0x8A) {
                    info!(
                        attempt,
                        fw = format_args!("0x{:02X}", x),
                        "PIC service dumy_read stable FW byte"
                    );
                    return Ok(x);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow::anyhow!(
        "dumy_read at 0x{:02X}: no stable known FW byte after 3 attempts; samples: {}",
        addr,
        samples.join(" | ")
    ))
}

async fn stop_am2_runtime_feeders_bounded(
    config: &DcentraldConfig,
    runtime_threads: &mut RuntimeThreadGuard,
    reason: &str,
) -> bool {
    let summary = runtime_threads.stop_and_join(AM2_FEEDER_STOP_TIMEOUT).await;
    if summary.any_timed_out() {
        error!(
            reason,
            timeout_ms = AM2_FEEDER_STOP_TIMEOUT.as_millis(),
            "AM2 feeder shutdown deadline expired; asserting transport-independent hard stop"
        );
        // Never take the PSU mutex here: the timed-out feeder may still own
        // it.  PWR_CONTROL is an independent GPIO path, and leaving both the
        // PIC and PSU feeder loops cancelled preserves the hardware backstop.
        // A transport call already in flight may complete once after this
        // point, but the post-sleep/post-lock cancellation fences prevent a
        // subsequent intentional feed.
        force_am2_home_hard_stop(config, reason);
        false
    } else {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Am2PowerShutdownEvidence {
    feeders_quiesced: bool,
    hard_stop_asserted: bool,
    psu_present: bool,
    psu_shutdown_succeeded: Option<bool>,
}

impl Am2PowerShutdownEvidence {
    fn completed_gracefully(self) -> bool {
        self.feeders_quiesced && self.psu_shutdown_succeeded.unwrap_or(true)
    }

    fn hard_stop_after_timeout(psu_present: bool) -> Self {
        Self {
            feeders_quiesced: false,
            hard_stop_asserted: true,
            psu_present,
            psu_shutdown_succeeded: None,
        }
    }
}

fn finalize_am2_dispatch_shutdown(
    dispatch_result: Result<()>,
    shutdown_evidence: Am2PowerShutdownEvidence,
) -> Result<()> {
    if shutdown_evidence.completed_gracefully() {
        return dispatch_result;
    }

    let detail = format!(
        "AM2 dispatch ended but shutdown did not complete gracefully: {:?}",
        shutdown_evidence
    );
    match dispatch_result {
        Ok(()) => Err(anyhow::anyhow!(detail)),
        Err(dispatch_error) => Err(dispatch_error.context(detail)),
    }
}

#[cfg(test)]
mod am2_power_shutdown_evidence_tests {
    use super::{
        finalize_am2_dispatch_shutdown, pic0x89_clean_stop_owner_policy, Am2PowerShutdownEvidence,
        Pic0x89CleanStopOwnerPolicy,
    };

    #[test]
    fn exact_pic0x89_identity_can_never_select_legacy_clean_stop_fallback() {
        assert_eq!(
            pic0x89_clean_stop_owner_policy(true, true),
            Pic0x89CleanStopOwnerPolicy::Endpoint
        );
        assert_eq!(
            pic0x89_clean_stop_owner_policy(true, false),
            Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint
        );
        assert_eq!(
            pic0x89_clean_stop_owner_policy(false, false),
            Pic0x89CleanStopOwnerPolicy::LegacyCompatibility
        );
    }

    #[test]
    fn graceful_shutdown_evidence_preserves_dispatch_success() {
        let evidence = Am2PowerShutdownEvidence {
            feeders_quiesced: true,
            hard_stop_asserted: false,
            psu_present: true,
            psu_shutdown_succeeded: Some(true),
        };

        assert!(evidence.completed_gracefully());
        assert!(finalize_am2_dispatch_shutdown(Ok(()), evidence).is_ok());
    }

    #[test]
    fn timeout_evidence_cannot_be_reported_as_graceful_dispatch_success() {
        let evidence = Am2PowerShutdownEvidence {
            feeders_quiesced: false,
            hard_stop_asserted: true,
            psu_present: true,
            psu_shutdown_succeeded: None,
        };

        assert!(!evidence.completed_gracefully());
        let error = finalize_am2_dispatch_shutdown(Ok(()), evidence)
            .expect_err("hard-stop fallback must be observable to the caller");
        assert!(error.to_string().contains("did not complete gracefully"));
    }

    #[test]
    fn failed_psu_plan_cannot_be_reported_as_graceful() {
        let evidence = Am2PowerShutdownEvidence {
            feeders_quiesced: true,
            hard_stop_asserted: false,
            psu_present: true,
            psu_shutdown_succeeded: Some(false),
        };

        assert!(!evidence.completed_gracefully());
    }
}

/// Reclaim every feeder before entering the PSU critical section.
///
/// A timeout deliberately skips `safe_shutdown_to_min`: a detached PSU
/// feeder may still own the same mutex. The preceding hard stop removes the
/// load independently and the cancelled feeder leaves the watchdog as a
/// second cutoff path after any already in-flight transfer drains.
async fn shutdown_am2_psu_after_feeders_bounded(
    config: &DcentraldConfig,
    runtime_threads: &mut RuntimeThreadGuard,
    psu_arc: &Option<Arc<Mutex<Apw121215a>>>,
    reason: &str,
) -> Am2PowerShutdownEvidence {
    let psu_present = psu_arc.is_some();
    if !stop_am2_runtime_feeders_bounded(config, runtime_threads, reason).await {
        warn!(
            reason,
            "skipping PSU mutex teardown after feeder timeout; hard stop is asserted and feeder loops are cancelled (an in-flight transfer may still complete once)"
        );
        return Am2PowerShutdownEvidence::hard_stop_after_timeout(psu_present);
    }

    shutdown_am2_psu_after_feeders_quiesced(psu_arc, reason)
}

fn shutdown_am2_psu_after_feeders_quiesced(
    psu_arc: &Option<Arc<Mutex<Apw121215a>>>,
    reason: &str,
) -> Am2PowerShutdownEvidence {
    let psu_present = psu_arc.is_some();
    let mut psu_shutdown_succeeded = None;
    if let Some(psu_mutex) = psu_arc.as_ref() {
        let mut psu_guard = psu_mutex.lock().unwrap_or_else(|e| e.into_inner());
        match psu_guard.safe_shutdown_to_min() {
            Ok(()) => psu_shutdown_succeeded = Some(true),
            Err(e) => {
                psu_shutdown_succeeded = Some(false);
                warn!(
                    reason,
                    error = %e,
                    "PSU safe-direction shutdown failed after feeders quiesced"
                );
            }
        }
    }
    Am2PowerShutdownEvidence {
        feeders_quiesced: true,
        hard_stop_asserted: false,
        psu_present,
        psu_shutdown_succeeded,
    }
}

async fn teardown_am2_power_after_failed_pic_preflight(
    config: &DcentraldConfig,
    runtime_threads: &mut RuntimeThreadGuard,
    psu_arc: &Option<Arc<Mutex<Apw121215a>>>,
) {
    if stop_am2_runtime_feeders_bounded(config, runtime_threads, "PIC preflight failure").await {
        let Some(psu_mutex) = psu_arc.as_ref() else {
            return;
        };
        // FWSTAB-2: recover from a poisoned lock (a panic elsewhere while holding
        // the shared PSU mutex) instead of panicking again — this teardown is the
        // graceful safe-off path (watchdog-disable + set-voltage-min) and must
        // still run a best-effort cut even if the lock was poisoned. (The PSU
        // hardware watchdog + Apw121215a::Drop remain the ultimate backstops.)
        // All PSU-mutex lock sites in this file use the same poison-tolerant form.
        let mut psu_guard = psu_mutex.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = psu_guard.safe_shutdown_to_min() {
            warn!(error = %e, "PSU safe-direction shutdown failed after PIC preflight failure");
        }
    }

    // PWR_CONTROL gate is owned by `Apw121215a`; it auto-deasserts when
    // the surviving `Arc<Mutex<Apw121215a>>` reference is dropped at the
    // end of the caller's scope (after this teardown returns and the
    // outer error path bails). No explicit deassert here.
}

// ---------------------------------------------------------------------------
// PSU heartbeat loop (1 Hz, shared Arc<Mutex<Apw121215a>>)
// ---------------------------------------------------------------------------

/// 1 Hz PSU heartbeat loop. Runs forever until the process exits.
///
/// Missing 3 consecutive heartbeats on APW121215a triggers the PSU self-disable
/// (~30 s typical grace, <=60 s worst-case). This loop logs at progressively
/// louder levels as consecutive-fails grow:
///   - fails < 3 : warn (transient bus glitch, common on xiic)
///   - fails = 3 : error (watchdog grace period entering danger zone)
///   - fails = 25: error (voltage likely already cut)
fn psu_heartbeat_loop(
    psu: Arc<Mutex<Apw121215a>>,
    shutdown: CancellationToken,
    interval: Duration,
) {
    let mut consecutive_fails = 0u32;
    loop {
        if shutdown.is_cancelled() {
            info!("PSU heartbeat thread shutting down");
            return;
        }
        std::thread::sleep(interval);
        if shutdown.is_cancelled() {
            info!("PSU heartbeat thread shutting down after sleep");
            return;
        }
        let mut psu = psu.lock().unwrap_or_else(|e| e.into_inner());
        if shutdown.is_cancelled() {
            info!("PSU heartbeat thread shutting down after acquiring PSU lock");
            return;
        }
        let result = psu.heartbeat();
        match result {
            Ok(()) => {
                if consecutive_fails > 0 {
                    info!(
                        fails = consecutive_fails,
                        "PSU heartbeat recovered after {} fails", consecutive_fails
                    );
                    consecutive_fails = 0;
                }
            }
            Err(e) => {
                consecutive_fails += 1;
                if consecutive_fails < 3 {
                    warn!(fails = consecutive_fails, "PSU heartbeat fail: {}", e);
                } else if consecutive_fails == 3 {
                    error!(
                        fails = consecutive_fails,
                        "PSU heartbeat failing {} consecutive — PSU watchdog will cut in <30s",
                        consecutive_fails,
                    );
                } else if consecutive_fails == 25 {
                    error!(
                        fails = consecutive_fails,
                        "PSU heartbeat dead for 25s — voltage likely already cut. Consider shutdown.",
                    );
                }
            }
        }
    }
}

/// Spawn the 1 Hz post-ENABLE PIC heartbeat thread.
///
/// The selected controller is issued by the retained opaque endpoint session;
/// this function cannot reconstruct its bus, address, or firmware. Additional
/// addresses are a separate experimental owner and remain independently
/// evidence-gated debt.
///
/// `additional_addrs` is the H-heartbeat-0x22 (`DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS`)
/// extension: when EMPTY (the default / gate-OFF case) the thread heartbeats
/// ONLY `addr` (the selected dsPIC) with byte-for-byte the same wire traffic
/// and timing as before this gate existed. When non-empty, the SAME single
/// thread additionally keepalives each extra dsPIC per tick (incl. the
/// effective chain dsPIC 0x22) using the same heartbeat command — so slot-3's
/// rail isn't watchdog-cut before the first enum. One thread, one
/// `JoinHandle`, one `CancellationToken` → the teardown contract is unchanged.
fn spawn_pic_heartbeat_thread(
    i2c: I2cServiceHandle,
    mut pic: Pic0x89Service,
    additional_addrs: Vec<u8>,
    shutdown: CancellationToken,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("s19j-pic-hb".to_string())
        .spawn(move || {
            let addr = pic.address();
            info!(
                "PIC heartbeat running through AM2 I2C service (0x{:02X})",
                addr
            );

            // H-heartbeat-0x22: one extra Pic0x89Service per additional active
            // dsPIC (e.g. the effective chain dsPIC 0x22). fw is unknown for
            // these, so `None` → default fw=0x89 FRAMED semantics, which is the
            // correct shape for the `a lab unit` slot-3 0x22 (fw=0x89). Empty unless
            // `DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS=1`. The heartbeat command is
            // a non-destructive 1 Hz keepalive — same frame as the selected
            // PIC, just to a different address.
            let mut extra_pics: Vec<(u8, Pic0x89Service)> = additional_addrs
                .iter()
                .copied()
                .map(|extra_addr| {
                    info!(
                        "PIC heartbeat also keepaliving active dsPIC 0x{:02X} \
                         (DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS)",
                        extra_addr
                    );
                    (
                        extra_addr,
                        Pic0x89Service::new_with_fw(i2c.clone(), extra_addr, None),
                    )
                })
                .collect();

            let mut fails = 0u32;
            let mut extra_fails: Vec<u32> = vec![0u32; extra_pics.len()];
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                match pic.send_heartbeat() {
                    Ok(()) => {
                        if fails > 0 {
                            info!("PIC heartbeat OK after {} fails", fails);
                        }
                        fails = 0;
                    }
                    Err(e) => {
                        fails += 1;
                        if fails <= 3 || fails.is_multiple_of(10) {
                            warn!(fails, error = %e, "PIC heartbeat fail ({}x)", fails);
                        }
                        // Load-bearing rule: a sustained selected-dsPIC heartbeat
                        // failure MUST cut voltage (not just warn). After
                        // AM2_HYBRID_PIC_HEARTBEAT_MAX_FAILURES consecutive silences
                        // the PIC is dead — cancel the run so the teardown
                        // de-energizes the chain rail, leading the PSU's own ~30 s
                        // self-disable instead of leaving the boards energized under
                        // an unmaintained rail. `fails` resets on any Ok above, so a
                        // transient blip never reaches this.
                        if fails >= AM2_HYBRID_PIC_HEARTBEAT_MAX_FAILURES {
                            error!(
                                fails,
                                error = %e,
                                "PIC heartbeat dead for {} consecutive ticks — cancelling run to cut chain voltage (load-bearing safety rule)",
                                fails
                            );
                            shutdown.cancel();
                            break;
                        }
                    }
                }
                // Keepalive each additional active dsPIC. A failure here is
                // logged but never fatal — the selected-PIC heartbeat above is
                // the primary watchdog feed and its loop semantics are
                // untouched.
                for (idx, (extra_addr, extra_pic)) in extra_pics.iter_mut().enumerate() {
                    if shutdown.is_cancelled() {
                        break;
                    }
                    match extra_pic.send_heartbeat() {
                        Ok(()) => {
                            if extra_fails[idx] > 0 {
                                info!(
                                    "PIC heartbeat OK for 0x{:02X} after {} fails",
                                    *extra_addr, extra_fails[idx]
                                );
                            }
                            extra_fails[idx] = 0;
                        }
                        Err(e) => {
                            extra_fails[idx] += 1;
                            if extra_fails[idx] <= 3 || extra_fails[idx].is_multiple_of(10) {
                                warn!(
                                    fails = extra_fails[idx],
                                    addr = format_args!("0x{:02X}", *extra_addr),
                                    error = %e,
                                    "PIC heartbeat fail for 0x{:02X} ({}x)",
                                    *extra_addr,
                                    extra_fails[idx]
                                );
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(PIC_HEARTBEAT_INTERVAL_MS));
                if shutdown.is_cancelled() {
                    break;
                }
            }
        })
        .context("Failed to spawn PIC heartbeat thread")
}

fn log_am2_dispatch_snapshot(fpga: &Am2FpgaChain, phase: &str) {
    let raw00 = fpga.read_raw(0x00);
    let raw04 = fpga.read_raw(0x04);
    let raw08 = fpga.read_raw(0x08);
    let raw10 = fpga.read_raw(0x10);
    let raw14 = fpga.read_raw(0x14);
    let raw18 = fpga.read_raw(0x18);
    let tx_ctrl = fpga.read_work_tx_ctrl();
    let tx_stat = fpga.read_work_tx_status();
    let tx_thr = fpga.read_work_tx_threshold();
    let tx_last = fpga.read_work_tx_last();
    let rx_stat = fpga.read_work_rx_status();
    info!(
        phase,
        raw00 = format_args!("0x{:08X}", raw00),
        raw04 = format_args!("0x{:08X}", raw04),
        raw08 = format_args!("0x{:08X}", raw08),
        raw10 = format_args!("0x{:08X}", raw10),
        raw14 = format_args!("0x{:08X}", raw14),
        raw18 = format_args!("0x{:08X}", raw18),
        tx_ctrl = format_args!("0x{:08X}", tx_ctrl),
        tx_stat = format_args!("0x{:08X}", tx_stat),
        tx_thr = format_args!("0x{:08X}", tx_thr),
        tx_last = format_args!("0x{:08X}", tx_last),
        rx_stat = format_args!("0x{:08X}", rx_stat),
        tx_full = tx_stat & fpga_chain::STAT_TX_FULL != 0,
        rx_empty = rx_stat & fpga_chain::STAT_RX_EMPTY != 0,
        "am2_dispatch_snapshot"
    );
}

fn apply_am2_work_tx_bosminer_idle_mode(fpga: &Am2FpgaChain, phase: &str) {
    if !am2_work_tx_bosminer_idle_enabled() {
        return;
    }

    fpga.write_work_tx_threshold(0);
    fpga.write_work_tx_ctrl(0);
    info!(
        phase,
        tx_ctrl = format_args!("0x{:08X}", fpga.read_work_tx_ctrl()),
        tx_thr = format_args!("0x{:08X}", fpga.read_work_tx_threshold()),
        tx_stat = format_args!("0x{:08X}", fpga.read_work_tx_status()),
        "DCENT_AM2_WORK_TX_BOSMODE=1 - using bosminer-observed idle WORK_TX control values"
    );
}

/// Discover the `miner-glitch-monitor` UIO device number by scanning
/// `/sys/class/uio/uio*/name`. Returns the lowest matching number, or
/// `None` if the IP is not present (e.g., am1-s9 bitstream).
///
/// Used by the Phase 3c-pre-gate UART relay path. We do this inline here
/// (rather than threading the [`crate::platform::Platform`] handle through
/// `s19j_hybrid_mining`) because the hybrid mining path bypasses
/// [`daemon::Daemon::run()`] and thus never constructs a platform object.
fn find_glitch_monitor_uio_number() -> Option<u8> {
    let dir = std::fs::read_dir("/sys/class/uio").ok()?;
    let mut best: Option<u8> = None;
    for entry in dir.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let num_str = match dir_name.strip_prefix("uio") {
            Some(s) => s,
            None => continue,
        };
        let n: u8 = match num_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let name_path = format!("/sys/class/uio/{}/name", dir_name);
        let name = match std::fs::read_to_string(&name_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name = name.trim();
        if name.contains("glitch-monitor") || name.contains("glitch_monitor") {
            best = Some(best.map_or(n, |cur| cur.min(n)));
        }
    }
    best
}

/// W13.B1 helper: open the Braiins glitch monitor by auto-discovering its
/// UIO number, falling back to the .139 default `uio18` when nothing in
/// `/sys/class/uio` matches. Returns `None` cleanly on stock hardware
/// (no `glitch-monitor` UIO node present at all).
fn open_braiins_glitch_monitor_for_logging() -> Option<BraiinsGlitchMonitor> {
    let uio = find_glitch_monitor_uio_number().unwrap_or(18);
    BraiinsGlitchMonitor::open(uio).ok()
}

/// am2 board-control UIO number, fallback if discovery finds no match.
///
/// `uio17` is the canonical am2/.25/.139 number (the HAL doc-comment on
/// `BoardControl::open` and the `a lab unit` C49→C52 fan-fix runbook both cite
/// `/sys/class/uio/uio17/name == board-control`), but the kernel assigns
/// `uioN` numbers in probe order and they are NOT guaranteed stable across
/// bitstreams/DTBs. Discover by name first, fall back to 17.
const BOARD_CONTROL_UIO_FALLBACK: u8 = 17;

/// Scan `/sys/class/uio` for the node whose `name` is `board-control` and
/// return the lowest matching `uioN` number. Mirrors
/// `find_glitch_monitor_uio_number` (the proven UIO-by-name discovery idiom on
/// this platform). Returns `None` on stock hardware / no match.
fn find_board_control_uio_number() -> Option<u8> {
    let dir = std::fs::read_dir("/sys/class/uio").ok()?;
    let mut best: Option<u8> = None;
    for entry in dir.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let num_str = match dir_name.strip_prefix("uio") {
            Some(s) => s,
            None => continue,
        };
        let n: u8 = match num_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let name_path = format!("/sys/class/uio/{}/name", dir_name);
        let name = match std::fs::read_to_string(&name_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // am2 control-board IP node is named `board-control`; accept the
        // underscore spelling too for DTB variants.
        let name = name.trim();
        if name == "board-control" || name == "board_control" {
            best = Some(best.map_or(n, |cur| cur.min(n)));
        }
    }
    best
}

/// CE-003: discover the am2 board-control UIO number once (name-matched against
/// `/sys/class/uio`), caching the result for the rest of the process so the HB
/// reset / C52-fan / teardown paths stop hardcoding `uio17`. Falls back to
/// `BOARD_CONTROL_UIO_FALLBACK` (17) when nothing in `/sys/class/uio` is named
/// `board-control` (e.g. a host build, or a stock bitstream that does not
/// populate the node — same behaviour as before this change on those targets).
fn board_control_uio_number() -> u8 {
    static CACHED: OnceLock<u8> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let discovered = find_board_control_uio_number();
        let uio = discovered.unwrap_or(BOARD_CONTROL_UIO_FALLBACK);
        if let Some(n) = discovered {
            if n != BOARD_CONTROL_UIO_FALLBACK {
                info!(
                    uio = n,
                    fallback = BOARD_CONTROL_UIO_FALLBACK,
                    "board-control UIO discovered by name (not the uio17 default)"
                );
            } else {
                tracing::debug!(
                    uio = n,
                    "board-control UIO discovered (matches uio17 default)"
                );
            }
        } else {
            tracing::debug!(
                fallback = BOARD_CONTROL_UIO_FALLBACK,
                "no board-control UIO matched by name; using uio17 fallback"
            );
        }
        uio
    })
}

fn log_am2_glitch_window(phase: &str) {
    match open_braiins_glitch_monitor_for_logging() {
        Some(monitor) => {
            for off in [
                0x00u32, 0x04, 0x08, 0x0C, 0x10, 0x14, 0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30,
                0x34, 0x38, 0x3C,
            ] {
                info!(
                    phase,
                    offset = format_args!("0x{:02X}", off),
                    value = format_args!("0x{:08X}", monitor.read_word(off).unwrap_or(0)),
                    "am2_glitch_window"
                );
            }
        }
        None => {
            warn!(
                phase,
                "am2_glitch_window_unavailable (Braiins-am2 glitch monitor not present — stock hw)"
            );
        }
    }
}

fn run_am2_rx_oracle_window(fpga: &Am2FpgaChain, window: Duration) {
    let deadline = Instant::now() + window;
    let mut rx_nonempty_hits = 0u32;
    let mut rx_irq_hits = 0u32;
    let mut nonce_pairs_logged = 0u32;

    while Instant::now() < deadline {
        let rx_stat = fpga.read_work_rx_status();
        if rx_stat & fpga_chain::STAT_IRQ != 0 {
            rx_irq_hits += 1;
        }
        if rx_stat & fpga_chain::STAT_RX_EMPTY == 0 {
            rx_nonempty_hits += 1;
            if nonce_pairs_logged < 4 {
                if let Some((w0, w1)) = fpga.read_nonce() {
                    nonce_pairs_logged += 1;
                    info!(
                        idx = nonce_pairs_logged,
                        w0 = format_args!("0x{:08X}", w0),
                        w1 = format_args!("0x{:08X}", w1),
                        "rx_oracle_nonce"
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    info!(
        window_ms = window.as_millis() as u64,
        rx_nonempty_hits, rx_irq_hits, nonce_pairs_logged, "rx_oracle_window"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Am2DispatchTimelineSample {
    ctrl: u32,
    baud: u32,
    tx_ctrl: u32,
    tx_thr: u32,
    tx_stat: u32,
    tx_last: u32,
    rx_stat: u32,
    glitch_28: Option<u32>,
    glitch_2c: Option<u32>,
    relay_30: Option<u32>,
    relay_34: Option<u32>,
}

fn read_am2_dispatch_timeline_sample(
    fpga: &Am2FpgaChain,
    monitor: Option<&BraiinsGlitchMonitor>,
) -> Am2DispatchTimelineSample {
    Am2DispatchTimelineSample {
        ctrl: fpga.read_raw(0x00),
        baud: fpga.read_baud(),
        tx_ctrl: fpga.read_work_tx_ctrl(),
        tx_thr: fpga.read_work_tx_threshold(),
        tx_stat: fpga.read_work_tx_status(),
        tx_last: fpga.read_work_tx_last(),
        rx_stat: fpga.read_work_rx_status(),
        glitch_28: monitor.and_then(|m| m.read_word(0x28).ok()),
        glitch_2c: monitor.and_then(|m| m.read_word(0x2C).ok()),
        relay_30: monitor.and_then(|m| m.read_word(0x30).ok()),
        relay_34: monitor.and_then(|m| m.read_word(0x34).ok()),
    }
}

fn opt_hex_u32(value: Option<u32>) -> String {
    value
        .map(|v| format!("0x{:08X}", v))
        .unwrap_or_else(|| "n/a".to_string())
}

fn log_am2_dispatch_timeline_sample(
    phase: &str,
    elapsed_us: u64,
    sample_idx: usize,
    sample: &Am2DispatchTimelineSample,
) {
    info!(
        phase,
        elapsed_us,
        sample_idx,
        ctrl = format_args!("0x{:08X}", sample.ctrl),
        baud = format_args!("0x{:08X}", sample.baud),
        tx_ctrl = format_args!("0x{:08X}", sample.tx_ctrl),
        tx_thr = format_args!("0x{:08X}", sample.tx_thr),
        tx_stat = format_args!("0x{:08X}", sample.tx_stat),
        tx_last = format_args!("0x{:08X}", sample.tx_last),
        rx_stat = format_args!("0x{:08X}", sample.rx_stat),
        tx_irq = sample.tx_stat & fpga_chain::STAT_IRQ != 0,
        tx_full = sample.tx_stat & fpga_chain::STAT_TX_FULL != 0,
        rx_empty = sample.rx_stat & fpga_chain::STAT_RX_EMPTY != 0,
        glitch_28 = %opt_hex_u32(sample.glitch_28),
        glitch_2c = %opt_hex_u32(sample.glitch_2c),
        relay_30 = %opt_hex_u32(sample.relay_30),
        relay_34 = %opt_hex_u32(sample.relay_34),
        "am2_dispatch_timeline"
    );
}

fn run_am2_dispatch_timeline(fpga: &Am2FpgaChain, phase: &str) {
    let checkpoints_us = [
        0u64, 100, 250, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 300_000,
    ];
    let relay = open_braiins_glitch_monitor_for_logging();
    let start = Instant::now();
    let mut last_sample = None;
    let mut samples_logged = 0usize;

    for (idx, &target_us) in checkpoints_us.iter().enumerate() {
        if target_us > 0 {
            let target = start + Duration::from_micros(target_us);
            let now = Instant::now();
            if target > now {
                std::thread::sleep(target - now);
            }
        }

        let sample = read_am2_dispatch_timeline_sample(fpga, relay.as_ref());
        if last_sample.as_ref() != Some(&sample) || idx == 0 {
            let elapsed_us = start.elapsed().as_micros() as u64;
            log_am2_dispatch_timeline_sample(phase, elapsed_us, idx, &sample);
            last_sample = Some(sample);
            samples_logged += 1;
        }
    }

    info!(phase, samples_logged, "am2_dispatch_timeline_end");
}

fn summarize_bm1362_serial_responses(responses: &[Vec<u8>]) -> String {
    let mut chipid_like = 0usize;
    let mut nonce_like = 0usize;
    let mut other = 0usize;
    let mut malformed = 0usize;
    let mut unique_chip_addrs = BTreeSet::new();

    for resp in responses {
        if resp.len() != BM1362_RESP_BODY_LEN {
            malformed += 1;
            continue;
        }

        let flags = resp[8];
        if flags & 0x80 != 0 {
            nonce_like += 1;
            continue;
        }

        if resp[5] == 0x00 && resp[0] == 0x13 && resp[1] == 0x62 && resp[2] == 0x03 {
            chipid_like += 1;
            unique_chip_addrs.insert(resp[4]);
        } else {
            other += 1;
        }
    }

    let first = responses
        .first()
        .map(|r| format!("{:02X?}", r))
        .unwrap_or_else(|| "none".to_string());
    let chip_preview = if unique_chip_addrs.is_empty() {
        "none".to_string()
    } else {
        unique_chip_addrs
            .iter()
            .take(8)
            .map(|addr| format!("{:02X}", addr))
            .collect::<Vec<_>>()
            .join(",")
    };

    format!(
        "count={} chipid_like={} unique_chip_addrs={} duplicate_chipid={} nonce_like={} other={} malformed={} chip_preview={} first={}",
        responses.len(),
        chipid_like,
        unique_chip_addrs.len(),
        chipid_like.saturating_sub(unique_chip_addrs.len()),
        nonce_like,
        other,
        malformed,
        chip_preview,
        first,
    )
}

fn read_bm1362_serial_drain_summary(serial: &SerialChainBackend, max_wait_ms: u64) -> String {
    match serial.read_all_responses(max_wait_ms) {
        Ok(responses) => summarize_bm1362_serial_responses(&responses),
        Err(e) => format!("read_error={}", e),
    }
}

fn bm1362_unique_chip_count_from_summary(summary: &str) -> usize {
    summary
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("unique_chip_addrs="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn verify_bm1362_get_address(
    serial: &SerialChainBackend,
    max_wait_ms: u64,
) -> Result<(String, usize)> {
    let _ = serial.read_all_responses(0); // drain any stale bytes first
    serial
        .send_get_address_bm1397plus()
        .context("send_get_address_bm1397plus failed")?;
    serial
        .drain_tx()
        .context("drain_tx after GetAddress failed")?;

    let summary = read_bm1362_serial_drain_summary(serial, max_wait_ms);
    let unique_count = bm1362_unique_chip_count_from_summary(&summary);
    Ok((summary, unique_count))
}

fn probe_bm1362_get_address_summary(serial: &SerialChainBackend, max_wait_ms: u64) -> String {
    match serial.send_get_address_bm1397plus() {
        Ok(()) => read_bm1362_serial_drain_summary(serial, max_wait_ms),
        Err(e) => format!("send_error={}", e),
    }
}

/// PR-019 / R11-2 per-phase ablation probe — read-only, GATED.
///
/// Does NOTHING and issues NO chain traffic unless
/// `DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE` is set. When the gate is on,
/// it reuses the proven read-only `verify_bm1362_get_address` enumeration
/// (the exact same GetAddress/drain the proven path's presence gate uses —
/// no new chain transaction is invented) and emits one greppable
/// `[AM2-ABLATION]` log line for `phase`: total responding frames, unique
/// chip-ids, duplicate-id collisions (the 126→28 collapse signature), and
/// the first ~8 chip-ids. A probe read failure is logged but never bails —
/// instrumentation must never change control flow of the proven path.
fn am2_ablation_probe(serial: &SerialChainBackend, phase: &str) {
    if !am2_verify_presence_after_each_phase_enabled() {
        return;
    }
    // NOTE: we deliberately do NOT touch `set_response_len` here. It needs
    // `&mut` and mutating the backend's response framing from an
    // instrumentation probe could leak state into the proven path. The
    // early-phase probes (pre-init/post-A8/post-MiscCtrl) may therefore see
    // some frames counted as `malformed` if the response length has not yet
    // been set by the proven path's own Step-4c gate — that is acceptable:
    // `parse_ablation_fields` degrades to zeros, and the post-3C / post-
    // fast-baud / pre-mining probes (the ones that matter for the 126->28
    // localization) all run *after* the proven path has set the length.
    match verify_bm1362_get_address(serial, 300) {
        Ok((summary, _unique)) => {
            let (total, unique, duplicates, preview) = parse_ablation_fields(&summary);
            info!(
                target: "am2_ablation",
                phase,
                total_responding = total,
                unique_chip_ids = unique,
                duplicate_id_collisions = duplicates,
                chip_preview = %preview,
                "[AM2-ABLATION] phase={} total_responding={} unique_chip_ids={} duplicate_id_collisions={} chip_preview={}",
                phase,
                total,
                unique,
                duplicates,
                preview
            );
        }
        Err(e) => {
            warn!(
                target: "am2_ablation",
                phase,
                error = %e,
                "[AM2-ABLATION] phase={} probe read failed (non-fatal, instrumentation only)",
                phase
            );
        }
    }
}

/// PR-019 / R11-2 5-parameter A/B run summary line — GATED.
///
/// Emits a single stable `[AM2-ABLATION-PARAMS]` line capturing the 5 init
/// parameters in play for this run so successive operator AC-cycle runs can
/// be diffed mechanically. No-op unless
/// `DCENT_AM2_VERIFY_PRESENCE_AFTER_EACH_PHASE` is set. Reads only existing
/// env-gates / config values already resolved by the proven path — no
/// hardware access, no behaviour change.
fn am2_ablation_log_params(target_freq_mhz: u16, pll_ramp: bool, baud: u32) {
    if !am2_verify_presence_after_each_phase_enabled() {
        return;
    }
    let skip_115200_per_chip = am2_skip_115200_per_chip_enabled();
    let skip_fast_uart = am2_skip_fast_uart_enabled();
    let serial_work_dispatch = am2_serial_work_dispatch_enabled();
    info!(
        target: "am2_ablation",
        skip_115200_per_chip,
        skip_fast_uart,
        serial_work_dispatch,
        freq_mhz = target_freq_mhz,
        pll_ramp,
        baud,
        "[AM2-ABLATION-PARAMS] skip_115200_per_chip={} skip_fast_uart={} serial_work_dispatch={} freq_mhz={} pll_ramp={} baud={}",
        skip_115200_per_chip,
        skip_fast_uart,
        serial_work_dispatch,
        target_freq_mhz,
        pll_ramp,
        baud
    );
}

// ---------------------------------------------------------------------------
// Work entry for nonce→share lookup
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

/// RE-018 byte-exact bosminer cold-wake sequence for the `a lab unit` BM1362 chain.
///
/// Replicates, byte-for-byte and in-order, bosminer's COLD chain bring-up as
/// decoded from `/data/re018-cold-strace.log` (strace of bosminer cold-engaging
/// `a lab unit` on /dev/ttyS1). Gated by `am2_bm1362_re018_cold_sequence_enabled()`
/// (default-OFF + `a lab unit`-fingerprint). The caller (`init_asic_chain`) only
/// reaches this branch when the gate fires, so every other platform / the
/// `a lab unit` first-shares path / the  handoff are byte-identical.
///
/// Returns the SerialChainBackend (still at 115200 — bosminer switches to fast
/// baud via reg 0x28 inside this sequence, but DCENT's work dispatch on `a lab unit`
/// uses the proven 115200 serial path, so the host UART stays at 115200) plus
/// the unique-chip-reply count from the single late GetAddress poll.
///
/// The GetAddress poll here is bosminer-faithful: a SINGLE late presence check
/// AFTER the broadcast 0x3C/0x54/0x58 block, and it is NEVER fatal — bosminer
/// blind-addresses and proves life by nonces, never by a GetAddress gate. A
/// zero reply is logged and the sequence completes anyway (work dispatch +
/// nonce flow is the authoritative success signal).
fn bm1362_re018_cold_sequence(serial: &SerialChainBackend, chip_count: u8) -> Result<usize> {
    let addr_interval = 256u16 / (chip_count as u16).max(1);
    info!(
        chip_count,
        addr_interval,
        "=== RE-018 BM1362 COLD-WAKE SEQUENCE (byte-exact bosminer replay, .25 standalone) ==="
    );

    // PERF FIX (2026-06-14 audit): resolve the A8 (INIT_CONTROL / core-enable)
    // and PLL (reg 0x08 / frequency) values ONCE, coupled under the single
    // `DCENT_AM2_RE018_FULL_CORE_INIT` gate. Default-OFF keeps the byte-exact
    // bosminer-replay values (cores idle, 525 MHz); enabling swaps to the
    // `a lab unit`-PROVEN core-ENABLE A8 values AND the LOW 150 MHz PLL so the
    // un-idled cores stay within the ~550 W home breaker budget. The two are
    // bound to the same gate so cores can never be activated at the high freq.
    let full_core_init = am2_re018_full_core_init_enabled();
    // DE-COUPLED (2026-06-14): the off-table low-freq PLL (RE018_LOW_FREQ_PLL_08 = 0x50D2_0164) was
    // LIVE-REJECTED — it gave ZERO nonces (total_rx_frames=0) where the existing PLL produced a nonce
    // flood. Root cause: it is a VCO RE-LOCK (existing RE018_PLL_08=0x40A80265 is VCO-scale 0x40 = VCO
    // 2100 MHz; the "150 MHz" value is VCO-scale 0x50 = VCO 5250 MHz) and/or an unaccepted off-table
    // postdiv (7x5 vs the proven 5x2). So FULL_CORE_INIT now enables ONLY the A8 core-ENABLE at the
    // EXISTING chip-ACCEPTED PLL (RE018_PLL_08, ~50 MHz — the freq the chip already enumerates + clocks
    // at). A8-cores at ~50 MHz ≈ ~4.8 TH/s at ~143 W (a ~40x bump from 120 GH/s, WELL within the home
    // breaker). The low-freq PLL is now behind a SEPARATE opt-in gate (DCENT_AM2_RE018_LOW_FREQ_PLL) so
    // a PROVEN table PLL value can be re-tried later for more freq — never coupled, never off-table by
    // default. Fleet/handoff byte-identical (both gates default-OFF + .25-fingerprint).
    // option-B (2026-06-14): the freq override is INDEPENDENT of A8 (full_core_init). A8 is a
    // live-confirmed zero-nonce regression, so coupling the freq bump to it left it unusable. This is
    // its own opt-in gate; default-OFF preserves the ~50 MHz eco/heater default. POWER-RAISING (meter-gated).
    let low_freq_pll = am2_env_flag("DCENT_AM2_RE018_LOW_FREQ_PLL");
    let bcast_a8 = if full_core_init {
        RE018_FULL_CORE_BCAST_A8
    } else {
        RE018_BCAST_A8
    };
    let per_chip_a8 = if full_core_init {
        RE018_FULL_CORE_PER_CHIP_A8
    } else {
        RE018_PER_CHIP_A8
    };
    let pll_08 = if low_freq_pll {
        am2_re018_resolve_pll_override_reg()
    } else {
        RE018_PLL_08
    };
    // Publish the REAL applied chip frequency (decoded from pll_08) so the
    // dashboard reports it instead of the config label (~50 MHz eco default here).
    AM2_RE018_APPLIED_FREQ_MHZ.store(
        decode_pll_reg_to_freq(pll_08).unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    if full_core_init {
        warn!(
            bcast_a8 = format_args!("0x{:08X}", bcast_a8),
            per_chip_a8 = format_args!("0x{:08X}", per_chip_a8),
            "RE-018 FULL-CORE-INIT (A8 core-enable) is a LIVE-CONFIRMED zero-nonce REGRESSION on .25 \
             and should stay OFF"
        );
    }
    if low_freq_pll {
        info!(
            env = "DCENT_AM2_RE018_LOW_FREQ_PLL=1",
            pll_08 = format_args!("0x{:08X}", pll_08),
            freq_mhz = decode_pll_reg_to_freq(pll_08).unwrap_or(0),
            "RE-018 FREQ-OVERRIDE: applying a PROVEN PLL (default 320 MHz sweet spot; set \
             DCENT_AM2_RE018_TARGET_MHZ to pick 240-597). POWER-RAISING vs the ~50 MHz eco/heater \
             default — wattmeter-gated. A8 stays OFF (independent gate)."
        );
    }

    // --- Phase A0: bosminer's one-time B9600 -> B115200 chain-UART port wake ---
    // The cold strace's ONLY B9600 (ttyS1 17:30:21.6 -> 17:30:22.8, the very first
    // chain-UART event, before reset release) is the single host->wire step DCENT
    // skips by opening straight at 115200 (serial.rs:1641). On the Zynq PL UART this
    // two-step forces a DLAB divisor-latch reload + line-control settle that re-arms
    // the UART TX clock-out the BM1362 RX clock-recovery locks onto. Without it the TX
    // FIFO accepts bytes but the wire is never triggered -> chips emit ZERO bytes
    // (count=0, malformed=0 -- silence, not garbage). This is the missing cold-wake
    // TRIGGER; DCENT has only ever woken WARM chains bosminer already port-woke
    // (LIVE TEST 7/8 2026-06-09). `a lab unit`-fingerprint + RE-018-gated (this fn only runs
    // there), so the proven  handoff and the fleet are byte-identical. Default
    // ON inside this path; DCENT_AM2_COLD_PORTWAKE=0 disables it for an A/B.
    if am2_env_u64("DCENT_AM2_COLD_PORTWAKE", 1) != 0 {
        // BLK-2 (2026-06-10): default dwell raised 50 -> 1100 ms and clamp widened to
        // <=2000. bosminer holds a real ~1.16 s B9600 dwell before the chain answers
        // (strace 17:30:21.61 -> 17:30:22.77); the old 50 ms under-replicated the cold
        // RX-clock-recovery settle. Env-overridable for A/B sweeps.
        let portwake_settle_ms =
            am2_env_u64("DCENT_AM2_COLD_PORTWAKE_SETTLE_MS", 1100).clamp(10, 2000);
        info!(
            settle_ms = portwake_settle_ms,
            "RE-018 Phase A0: B9600 -> B115200 chain-UART port wake (bosminer cold-strace trigger)"
        );
        serial.set_baud(9_600)?;
        std::thread::sleep(Duration::from_millis(portwake_settle_ms));
        serial.set_baud(115_200)?;
        std::thread::sleep(Duration::from_millis(portwake_settle_ms));
    }

    // --- BLK-1b: assert MCR OUT2 AFTER the port-wake's final set_baud ---
    // On `a lab unit` the PL-UART OUT2 bit gates the FPGA UART TX clock-out. The kernel
    // `of_serial` transport (used since the IRQ-165 pivot) never asserts it, so the
    // TX FIFO accepts bytes but the wire stays silent -> chain enum=0. This pokes
    // MCR=0x0B for `a lab unit`-fingerprint units; no-op on baseline/devmem. Must run AFTER
    // the final set_baud (a termios/baud change can clear MCR). Then snapshot the
    // registers so this run OBSERVES OUT2 instead of inferring it (Team M R-11).
    if let Err(e) = serial.assert_mcr_out2() {
        anyhow::bail!("RE-018 BLK-1b: MCR OUT2 assert/readback failed before enum: {e}");
    }
    let Some((mcr, ier, lsr)) = serial.diagnostic_registers() else {
        anyhow::bail!("RE-018 BLK-1b: missing chain UART register snapshot after OUT2 assert");
    };
    info!(
        mcr = format_args!("0x{:02X}", mcr),
        ier = format_args!("0x{:02X}", ier),
        lsr = format_args!("0x{:02X}", lsr),
        "RE-018 BLK-1b: chain UART register snapshot after port-wake + OUT2 assert \
         (expect mcr=0x0B; OUT2 bit 0x08 gates the .25 FPGA UART TX clock-out)"
    );
    if (mcr & 0x0B) != 0x0B {
        anyhow::bail!(
            "RE-018 BLK-1b: MCR readback 0x{:02X} missing DTR/RTS/OUT2 mask 0x0B before enum",
            mcr
        );
    }

    // --- Phase A: broadcast pre-config (bosminer t+0.0 .. t+0.25) ---
    // A8 = INIT_CONTROL: `bcast_a8` is RE018_BCAST_A8 (0x0) by default, or the
    // `a lab unit`-proven core-ENABLE 0x0007_0000 when DCENT_AM2_RE018_FULL_CORE_INIT=1.
    serial.send_write_reg_broadcast_bm1397plus(0xA8, bcast_a8)?;
    std::thread::sleep(Duration::from_millis(100));
    misc_ctrl_triple_write_serial(serial, RE018_MISC_CTRL)?;
    std::thread::sleep(Duration::from_millis(100));
    serial.send_write_reg_broadcast_bm1397plus(0xA4, RE018_VERSION_MASK)?;
    std::thread::sleep(Duration::from_millis(100));

    // --- Phase B: CHAIN_INACTIVE x3 (bosminer t+0.356) ---
    for _ in 0..3 {
        serial.send_chain_inactive_bm1397plus()?;
        std::thread::sleep(Duration::from_millis(300));
    }

    // --- Phase C: blind SET_ADDRESS for every assigned position (t+1.3) ---
    // bosminer addresses 0x00..0xFE stride-2 (all 128). DCENT uses
    // addr_interval derived from chip_count so a 126-chip config produces the
    // same stride-2 0x00..0xFA assignment. Blind, fire-and-forget.
    // RE-018 FIX 1 (2026-06-09, LIVE TEST 7 follow-up): the true-cold bosminer
    // strace PROVES the chain enumerates from the UNSOLICITED register-report
    // frames the chips emit DURING this SET_ADDRESS walk at 115200
    // (`AA 55 13 62 03 <addr> ...`), NOT from a GetAddress(0x52) reply (the lone
    // 0x52 fires once, very late, in 662k lines). DCENT's Phase E
    // `verify_bm1362_get_address` DRAINS those frames (`read_all_responses(0)`)
    // before its 0x52, so even a fully-woken chain reports `unique_chip_replies=0`.
    // Capture + count them HERE, draining mid-walk (replacing the old 2 ms idle
    // sleep) so the small Zynq PL-UART RX FIFO does not overflow before we read.
    // This fn only runs on the `a lab unit` RE-018 standalone path
    // (`DCENT_AM2_BM1362_RE018_COLD_SEQUENCE`), so the proven handoff + fleet are
    // byte-identical.
    serial.set_response_len(BM1362_RESP_BODY_LEN);
    let mut walk_responses: Vec<Vec<u8>> = Vec::new();
    for i in 0..chip_count as u16 {
        let addr = (i * addr_interval) as u8;
        serial.send_set_address_bm1397plus(addr)?;
        if i % 16 == 15 {
            // BLK-1b mid-walk forensics: on the FIRST drain, snapshot MCR/IER/LSR so
            // a live run shows OUT2 (mcr bit 0x08) and TX-drain (LSR bit 0x40) DURING
            // the walk — the decisive observation Team M R-11 flagged as missing.
            if i == 15 {
                if let Some((mcr, ier, lsr)) = serial.diagnostic_registers() {
                    info!(
                        mcr = format_args!("0x{:02X}", mcr),
                        ier = format_args!("0x{:02X}", ier),
                        lsr = format_args!("0x{:02X}", lsr),
                        "RE-018 BLK-1b: chain UART register snapshot MID-WALK (after 16 SET_ADDRESS)"
                    );
                }
            }
            if let Ok(mut r) = serial.read_all_responses(2) {
                walk_responses.append(&mut r);
            }
        }
    }
    // Final drain for report frames still arriving after the last address.
    if let Ok(mut r) =
        serial.read_all_responses(am2_env_u64("DCENT_AM2_WALK_ENUM_WAIT_MS", 400).clamp(50, 2000))
    {
        walk_responses.append(&mut r);
    }
    let walk_summary = summarize_bm1362_serial_responses(&walk_responses);
    let walk_count = bm1362_unique_chip_count_from_summary(&walk_summary);
    info!(
        baud = 115_200,
        expected = chip_count,
        unique_chip_replies = walk_count,
        summary = %walk_summary,
        "RE-018 FIX 1: SET_ADDRESS-walk report-frame enum (bosminer-faithful, pre-GetAddress count)"
    );

    // --- Phase D: broadcast core/analog/io config (bosminer t+2.2 .. t+2.36) ---
    serial.send_write_reg_broadcast_bm1397plus(0x3C, RE018_CORE_3C_HASH_CLK)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x3C, RE018_CORE_3C_CLK_DELAY)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x54, RE018_ANALOG_MUX_54)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x58, RE018_IO_DRIVER_58)?;
    std::thread::sleep(Duration::from_millis(10));

    // --- Phase D.5: TICKET_MASK (0x14) + HASH_COUNTING_NUMBER (0x10) ---
    // PERF FIX (2026-06-14 audit): the bosminer strace this sequence replays
    // captured only the FIRST init cycle (ending at the per-chip 0x0C write), so
    // it OMITS 0x14/0x10 — which bosminer writes in a later, un-captured phase.
    // Without 0x14 (TICKET_MASK) the chip drops EVERY nonce at the hardware
    // difficulty filter (~silent zero production); without 0x10 (HASH_COUNTING /
    // nonce-range) all 126 chips hash the SAME range. That is the root cause of
    // the ~1000x-low standalone hashrate (the cores run at 525 MHz drawing power
    // but report almost nothing; even  was 5 nonces/165s). The legacy init
    // path writes both (Step 4b). POWER-NEUTRAL — same freq/voltage; this only
    // lets the already-running cores REPORT nonces at the correct rate. Gated
    // default-OFF + `a lab unit`-fingerprint so the fleet +  handoff are
    // byte-identical; fail-closed (a write error aborts the cold sequence).
    if am2_re018_write_ticket_hashcount_enabled() {
        serial.send_write_reg_broadcast_bm1397plus(0x14, TICKET_MASK_256)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x10, NONCE_RANGE_126)?;
        std::thread::sleep(Duration::from_millis(10));
        info!(
            ticket_mask = format_args!("0x{:08X}", TICKET_MASK_256),
            nonce_range = format_args!("0x{:08X}", NONCE_RANGE_126),
            env = "DCENT_AM2_RE018_WRITE_TICKET_HASHCOUNT=1",
            "RE-018 PERF FIX: TICKET_MASK(0x14)+HASH_COUNTING(0x10) written (were \
             missing on the RE-018 path = the ~1000x-low-hashrate root cause; \
             power-neutral — same freq/voltage, cores now report nonces)"
        );
    }

    // --- Phase E: single LATE GetAddress presence poll (bosminer t+2.37) ---
    // NON-FATAL. bosminer does exactly ONE GetAddress here, reads the chips'
    // unsolicited config-reply frames, and proceeds regardless. We mirror that:
    // log the count, never bail.
    serial.set_response_len(BM1362_RESP_BODY_LEN);
    let (presence_summary, unique_count) = match verify_bm1362_get_address(serial, 300) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "RE-018: late GetAddress poll errored (non-fatal) — continuing per bosminer");
            (String::from("getaddr-error"), 0)
        }
    };
    info!(
        baud = 115_200,
        expected = chip_count,
        unique_chip_replies = unique_count,
        summary = %presence_summary,
        "RE-018: late GetAddress presence poll (bosminer-faithful, non-fatal)"
    );

    // --- Phase F: PLL programming (bosminer t+2.8 .. t+2.86) ---
    // 0x70=0 -> 0x08=PLL -> 0x70=0 -> 0x08=PLL (x2) -> 0x28=FastUART.
    // reg 0x08 = PLL0 (frequency). `pll_08` is RE018_PLL_08 (525 MHz, bosminer
    // byte order) by default, or the LOW RE018_LOW_FREQ_PLL_08 (150 MHz, DCENT
    // table byte order) when DCENT_AM2_RE018_FULL_CORE_INIT=1 — the low freq is
    // MANDATORY whenever the cores are enabled (power budget). Written twice
    // (bosminer slams it twice with the 0x70 divider re-write between).
    serial.send_write_reg_broadcast_bm1397plus(BM1362_PLL0_DIVIDER_REG, RE018_PLL0_DIVIDER_70)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x08, pll_08)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(BM1362_PLL0_DIVIDER_REG, RE018_PLL0_DIVIDER_70)?;
    std::thread::sleep(Duration::from_millis(10));
    serial.send_write_reg_broadcast_bm1397plus(0x08, pll_08)?;
    std::thread::sleep(Duration::from_millis(10));
    // reg 0x28 FastUART (0x11300000). BLK-3 (2026-06-10): DEFAULT-OFF. Writing 0x28
    // chip-side switches the BM1362 UART to FastUART baud, but DCENT keeps the HOST
    // UART at 115200 (the proven `a lab unit`/`a lab unit` work-dispatch baud) — so after this write
    // chip and host are DESYNCED: the entire Phase-G per-chip config (incl. the 0x0C
    // nonce-space base) and the final GetAddress are sent at the wrong baud and are
    // structurally 0, AND it poisons same-process retries. It is NOT the enum cause
    // (the walk precedes this), but it is the GUARANTEED next blocker once enum>0, so it
    // is gated off by default; the chip stays at 115200, coherent with the host. Set
    // `DCENT_AM2_RE018_WRITE_FASTUART_0X28=1` only if a future run also switches the host
    // baud to match (3.125M) — until then leave it off. Byte-parity diagnostic value
    // RE018_FAST_UART_28 is preserved.
    // D6-3 (2026-06-13): FAIL-CLOSED. This gate writes the chip-side FastUART baud
    // (reg 0x28) but NO host-baud switch is wired here, so chip and host desync and
    // Phase-G/final-enum run at the wrong baud → structurally 0 RX (and it poisons
    // same-process retries). Rather than proceed into a guaranteed-broken state
    // (the old behaviour), REFUSE. Re-enable only once a matching host
    // set_baud(3.125M) is wired alongside — and note host fast-baud is itself
    // live-falsified on `a lab unit` (0/126 at 3.125M). The byte-parity diagnostic
    // constant RE018_FAST_UART_28 is retained for reference elsewhere.
    if am2_env_u64("DCENT_AM2_RE018_WRITE_FASTUART_0X28", 0) != 0 {
        anyhow::bail!(
            "DCENT_AM2_RE018_WRITE_FASTUART_0X28=1 but no host-baud switch is wired here — chip/host \
             would desync (guaranteed 0 RX). Refusing; leave it unset so the chip stays at 115200 \
             (the proven .109/.79 work-dispatch baud)."
        );
    }
    info!(
        "RE-018 Phase F: chip-side FastUART 0x28 write SKIPPED (default) — chip + host stay \
         coherent at 115200 (proven .109/.79 work-dispatch baud)"
    );

    // Fix-B (2026-06-07): post-PLL cold-lock settle. bosminer slams PLL0 at
    // t+2.8 and does the per-chip loop at t+4.2 — a ~1.4 s gap that lets a
    // pristine cold PLL lock so the chips can clock their UART to RECEIVE the
    // per-chip writes below AND answer the final enum. DCENT previously went
    // straight from the PLL slam into the per-chip loop with only 10 ms, so on a
    // cold chain the PLL may not have locked and both the per-chip config and the
    // final GetAddress landed on un-clocked chips (→ enum 0). `a lab unit`-only RE-018
    // path. Tunable; ~1.2 s ≈ bosminer's PLL→per-chip gap.
    std::thread::sleep(Duration::from_millis(
        am2_env_u64("DCENT_AM2_POST_PLL_SETTLE_MS", 1200).clamp(0, 5000),
    ));

    // --- Phase G: per-chip config loop (bosminer t+4.2) ---
    // Per chip, in bosminer's exact order:
    //   0xA8=0x00000002, MiscCtrl(0x18)=0xB000C100, 0x3C x3, then the
    //   per-chip nonce-space base 0x0C=(0x80000000 | addr*0x104).
    // The reg 0x0C nonce-space base is the register DCENT's init_asic_chain
    // NEVER wrote — without it every chip hashes from the same base and the
    // FPGA nonce-RX FIFO never sees a valid per-chip nonce -> 0 nonces.
    info!("RE-018: per-chip config loop (0xA8/MiscCtrl/0x3Cx3/0x0C nonce-space base)");
    for i in 0..chip_count as u16 {
        let addr = (i * addr_interval) as u8;
        // A8 = INIT_CONTROL per-chip: `per_chip_a8` is RE018_PER_CHIP_A8 (0x2)
        // by default, or the `a lab unit`-proven core-ENABLE 0x0007_01F0 (the BM1362
        // analogue of BM1387 open-core) under DCENT_AM2_RE018_FULL_CORE_INIT=1.
        serial.send_write_reg_bm1397plus(addr, 0xA8, per_chip_a8)?;
        misc_ctrl_triple_write_chip_serial(serial, addr, RE018_MISC_CTRL)?;
        serial.send_write_reg_bm1397plus(addr, 0x3C, RE018_CORE_3C_HASH_CLK)?;
        serial.send_write_reg_bm1397plus(addr, 0x3C, RE018_CORE_3C_CLK_DELAY)?;
        serial.send_write_reg_bm1397plus(addr, 0x3C, RE018_CORE_3C_FAMILY)?;
        let nonce_space = RE018_NONCE_SPACE_BASE
            .wrapping_add((addr as u32).wrapping_mul(RE018_NONCE_SPACE_STRIDE));
        serial.send_write_reg_bm1397plus(addr, RE018_NONCE_SPACE_REG, nonce_space)?;
        if i % 16 == 15 {
            std::thread::sleep(Duration::from_millis(SERIAL_PACE_MIN_MS));
        }
    }
    std::thread::sleep(Duration::from_millis(100));

    // Final presence re-poll (informational only — non-fatal, same as bosminer).
    let (final_summary, final_count) =
        verify_bm1362_get_address(serial, 300).unwrap_or((String::from("final-getaddr-error"), 0));
    info!(
        baud = 115_200,
        expected = chip_count,
        unique_chip_replies = final_count,
        summary = %final_summary,
        "=== RE-018 COLD-WAKE COMPLETE — chain configured byte-exact to bosminer, work dispatch is authoritative ==="
    );
    // RE-018 FIX 1: the SET_ADDRESS-walk report-frame count is the bosminer-faithful
    // enum signal (Phase C above); fold it in so a populated walk count is honored
    // even when the (drained) Phase-E/final GetAddress polls read 0.
    let result_count = final_count.max(unique_count).max(walk_count);
    // Fix-B (2026-06-07): on the standalone cold-wake path, an enum of 0 after
    // the full byte-exact replay + PLL settle must request the bosminer-faithful
    // HB_RESET re-pulse retry — the caller's retry loop only fires on `Err`.
    // Previously this returned `Ok(0)`, which silently starved BOTH the
    // DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL re-pulse loop AND CONTINUE_PAST_ZERO_ENUM,
    // so the headline sysfs-reset retry never actually ran. Gated on the same env
    // the retry loop reads, so the proven handoff path (which never sets it, and
    // never runs this RE-018 branch) is byte-identical.
    if result_count == 0 && am2_env_flag("DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL") {
        anyhow::bail!(
            "RE-018 cold sequence enumerated 0 chips after PLL+settle — requesting \
             HB_RESET re-pulse retry (bosminer re-pulses many times before chips answer)"
        );
    }
    Ok(result_count)
}

// ---------------------------------------------------------------------------
// S19j Hybrid Miner
// ---------------------------------------------------------------------------

pub struct S19jHybridMiner {
    /// One-shot proof that immutable startup identity, the exact AM2/Zynq
    /// BoardDesc, configured BM1362 identity, and this runtime route agreed
    /// before construction. Taken at `run()` entry so the same admission
    /// cannot authorize a second hardware lifecycle.
    route_admission: Option<crate::s19j_hybrid_admission::S19jHybridRouteAdmission>,
    config: DcentraldConfig,
    shutdown: CancellationToken,
    /// Long-lived `miner-glitch-monitor` UIO handle (Braiins-am2 only).
    ///
    /// W13.B1 (2026-05-10) RECLASSIFIED: this is a diagnostic-only mirror
    /// surface. R6-7 keeps BM1362 0x2C/0x34 candidate relay broadcasts
    /// lab-gated until live captures confirm exact semantics. Stock
    /// CV1835/AM335x/AML/S9 hardware does NOT populate this UIO.
    ///
    ///  RE (2026-04-27, still valid): the Xilinx `uio_pdrv_genirq`
    /// kernel driver gates AXI-Lite write permission on the open
    /// `/dev/uioN` fd. Bosminer keeps the fd alive for the full daemon
    /// lifetime; we mirror that for telemetry parity only. Initialised
    /// lazily in `run()` before Phase 0 PSU bring-up; `None` on platforms
    /// without the IP block or if the UIO open fails.
    glitch_monitor: Option<BraiinsGlitchMonitor>,

    /// AT-DASH (2026-06-14): optional live `MinerState` publisher wired by the
    /// `--s19j-hybrid` launcher in `main.rs`. When `Some`, the mining loops push
    /// a fresh `MinerState` on each `am2_serial_status`/hashrate tick so the REST
    /// `/api/status` (and the dashboard) reflect real hashrate, per-dsPIC
    /// `ChainState`, and accepted/rejected shares instead of a default-empty
    /// snapshot. `None` keeps the prior (blank) behaviour — purely additive and
    /// fail-closed: a closed/missing channel only drops the publish, never the
    /// share.
    state_tx: Option<watch::Sender<dcentrald_api::MinerState>>,

    /// CE-011 (2026-07-08): the Phase-0s energize-gate `SkuBinding`s that were
    /// ACCEPTED for this run (one per bound chain). Populated ONLY from the
    /// `Ok((bindings, _))` arm of the energize gate in `run()`; left EMPTY in
    /// the `accept_degraded` lab-override arm (fail-closed — an unverified
    /// hardware set never widens or registers a PVT ceiling). The freq-only
    /// tuner spawn reads this to register a CEILING-ONLY per-SKU PVT clamp via
    /// `AutoTuner::set_chain_sku`. Empty (the default, and the accept_degraded
    /// case) => no registration => byte-identical to today's behavior.
    accepted_sku_bindings: Vec<dcentrald_silicon_profiles::energize_gate::SkuBinding>,
}

impl S19jHybridMiner {
    /// Construct the BM1362-only hybrid engine after composition admission.
    ///
    /// The route-specific proof prevents a shared ASIC protocol or transport
    /// facet from authorizing this engine on another control-board carrier.
    /// No hardware is opened here, but rejecting before construction keeps
    /// every future Phase-0 mutation behind the same invariant.
    pub fn new(
        config: DcentraldConfig,
        shutdown: CancellationToken,
        route_admission: crate::s19j_hybrid_admission::S19jHybridRouteAdmission,
    ) -> Result<Self> {
        Ok(Self {
            route_admission: Some(route_admission),
            config,
            shutdown,
            glitch_monitor: None,
            state_tx: None,
            accepted_sku_bindings: Vec::new(),
        })
    }

    /// AT-DASH: attach a live `MinerState` publisher so the hybrid mining loops
    /// feed `/api/status` + the dashboard. Returns `self` for builder chaining.
    /// Called by the `--s19j-hybrid` launcher after wiring the matching receiver
    /// into the API. Additive; safe to omit (dashboard then shows zeros).
    pub fn with_state_tx(mut self, state_tx: watch::Sender<dcentrald_api::MinerState>) -> Self {
        self.state_tx = Some(state_tx);
        self
    }

    /// AT-DASH: project the live am2 mining telemetry into a `dcentrald_api::MinerState`
    /// and publish it on `self.state_tx` (no-op when no publisher is attached).
    ///
    /// `chains` is the per-dsPIC attribution the caller assembled (one
    /// `ChainState` per driven hashboard, keyed by `am2_chain_id_for_pic_addr`).
    /// `hashrate_ghs` is the cumulative real estimate (achieved-difficulty based)
    /// and `hashrate_5s_ghs` the rolling-window estimate. Fail-closed: a closed
    /// channel is silently ignored — a dashboard publish must NEVER affect the
    /// mining hot path.
    #[allow(clippy::too_many_arguments)]
    fn publish_miner_state(
        &self,
        accounting: &Am2ShareAccounting,
        pool_quality: &dcentrald_stratum::pool_quality::PoolQualitySnapshot,
        hashrate_ghs: f64,
        hashrate_5s_ghs: f64,
        chains: Vec<dcentrald_api::ChainState>,
        uptime_s: u64,
        // MINE-LIFE-2: whether the unit has produced a nonce within the generous
        // recent-activity window (single-chain: this chain; dual-chain: EITHER
        // chain). `true` on a healthy unit ⇒ the status logic is unchanged.
        mining_recently_active: bool,
    ) {
        let Some(tx) = self.state_tx.as_ref() else {
            return;
        };
        let pool_status = if accounting.accepted() > 0 {
            "mining"
        } else {
            "connecting"
        };
        let mut pool = dcentrald_api::PoolState {
            url: self.config.pool.url.clone(),
            worker: self.config.pool.worker.clone(),
            status: pool_status.to_string(),
            difficulty: accounting.pool_difficulty(),
            last_share_at: accounting.last_share_at_secs(),
            protocol: self
                .config
                .pool
                .protocol
                .clone()
                .unwrap_or_else(|| "sv1".to_string()),
            encrypted: false,
            encrypted_source: dcentrald_api::pool_quality_honest_default_source(),
            sv2_session: None,
            sv2_session_source: dcentrald_api::pool_quality_honest_default_source(),
            sv2_custom_job: None,
            donating: false,
            donating_source: dcentrald_api::pool_quality_honest_default_source(),
            donation_active_url: String::new(),
            donation_active_worker: String::new(),
            donation_pool_index: 0,
            share_efficiency: None,
            auto_fallback_active: false,
            auto_fallback_source: dcentrald_api::pool_quality_honest_default_source(),
            auto_retry_sv2_after_s: None,
            auto_fallback_reason: None,
            failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
            failover_source: dcentrald_api::pool_quality_honest_default_source(),
            hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
            hashrate_split_source: dcentrald_api::pool_quality_honest_default_source(),
            latency_ms: 0,
            latency_ms_source: dcentrald_api::pool_quality_honest_default_source(),
            reject_reason_counts: [0; 6],
            reject_reason_counts_source: dcentrald_api::pool_quality_honest_default_source(),
            rolling_acceptance_pct_30min: 100.0,
            rolling_acceptance_count_30min: (0, 0),
            rolling_acceptance_source: dcentrald_api::pool_quality_honest_default_source(),
            worst_chip_hw_err_rate: None,
        };
        pool.apply_quality_snapshot(pool_quality);
        // FWT-2: apply_quality_snapshot has now set pool.status from the REAL
        // StratumState when one was observed (else the accepted()>0 fallback
        // above stands). Two honest refinements the bare state can't express:
        //   - shares are landing => we are definitively mining, even if the last
        //     StateChanged we saw was Connecting/Authorized (a happily-mining
        //     unit must never read "connecting").
        //   - connected/authorized but every share is rejected => "rejecting",
        //     the single most actionable signal for a home miner (the reject
        //     reason is in reject_reason_counts) — never a reassuring "mining".
        // The >= 3 grace avoids a transient false "rejecting" on a healthy
        // unit whose very first submitted share happens to be a stale-job
        // reject before the first accept lands; a genuinely all-rejecting pool
        // crosses the threshold within seconds.
        if accounting.accepted() == 0
            && accounting.rejected() >= 3
            && matches!(pool.status.as_str(), "mining" | "authorized" | "donating")
        {
            pool.status = "rejecting".to_string();
        } else if accounting.accepted() > 0
            && matches!(pool.status.as_str(), "connecting" | "authorized")
        {
            pool.status = "mining".to_string();
        }
        // MINE-LIFE-2: a unit that has gone quiet (no nonce within the generous
        // recent-activity window) must NOT keep reporting a reassuring "mining"
        // off a cumulative accepted()>0 latch. Only DOWNGRADE an otherwise-
        // "mining" status — never upgrade, never touch rejecting/connecting/
        // disconnected states. On a healthy unit `mining_recently_active` is
        // true, so this is a no-op and the proven path stays byte-equivalent.
        if !mining_recently_active && pool.status == "mining" {
            pool.status = "stalled".to_string();
        }
        let state = dcentrald_api::MinerState {
            hashrate_ghs,
            hashrate_5s_ghs,
            accepted: accounting.accepted(),
            rejected: accounting.rejected(),
            chains,
            fans: dcentrald_api::FanState {
                // Display-only: the hybrid path caps fans via the home hard-stop
                // guard; report the configured idle floor, never a max-blast
                // number. This is telemetry, NEVER a fan command.
                pwm: self.config.thermal.fan_min_pwm,
                // Truthful tach: the REAL max fan rpm via a read-only fan-UIO
                // open (no board-control write) instead of a hardcoded 0 (which
                // the dashboard misreads as a stopped fan while mining). 0 only
                // if the read fails — honest, never fabricated.
                rpm: am2_read_fan_rpm_max(),
                per_fan: vec![],
            },
            pool,
            uptime_s,
            firmware_version: env!("CARGO_PKG_VERSION").to_string(),
            mode: dcentrald_api::OperatingMode::from_config_str(&self.config.mode.active),
        };
        // `watch::Sender::send` only errs when every receiver was dropped (API
        // task gone). That is a clean no-op for mining — ignore it.
        let _ = tx.send(state);
    }

    /// AT-DASH: build a single-chain `ChainState` for the one dsPIC this serial
    /// loop drives. The serial nonce frame carries no per-chip address, so all
    /// nonces attribute to this one chain (keyed by `am2_chain_id_for_pic_addr`).
    fn build_am2_chain_state(
        &self,
        pic_addr: u8,
        chip_count: u8,
        chain_hashrate_ghs: f64,
        unique_nonces: u64,
        crc_errors: u32,
        // MINE-LIFE-2: whether this chain has produced a nonce within the
        // generous recent-activity window. A chain that produced nonces earlier
        // but has since gone quiet must report "stalled", not a reassuring
        // "mining" off a cumulative unique-nonce latch. `true` on a healthy unit
        // ⇒ byte-equivalent to the prior status.
        recently_active: bool,
    ) -> dcentrald_api::ChainState {
        // Truthful per-chain temperature: report the REAL XADC SoC die temp
        // (honest proxy, labeled `soc_die_fallback`) instead of a misleading
        // 0.0°C. On `a lab unit` the LM75 hashboard sensor returns NaN (cold-board
        // artifact) so the SoC die temp is the reliable live reading; the
        // dashboard shows the value + its provenance. Falls back to honest
        // "unknown" (0.0 / None) only if the XADC read itself fails — never a
        // fabricated number. This is telemetry only, never a thermal command
        // (the thermal supervisor still owns the safety reading separately).
        let (temp_c, temp_source) = match Xadc::read_temp() {
            Ok(die_c) if die_c.is_finite() && (0.0..125.0).contains(&die_c) => (
                die_c,
                Some(dcentrald_api::ChainTempSource::SOC_DIE_FALLBACK.to_string()),
            ),
            _ => (0.0, None),
        };
        dcentrald_api::ChainState {
            id: am2_chain_id_for_pic_addr(pic_addr).unwrap_or(0),
            chips: chip_count,
            // Report the REAL applied chip frequency (RE-018 decoded PLL: ~50 MHz eco
            // default or the gated bump) when known, else the config label. 0 =
            // non-RE-018 unit -> config.mining.frequency_mhz (fleet byte-identical).
            frequency_mhz: {
                let applied = AM2_RE018_APPLIED_FREQ_MHZ.load(std::sync::atomic::Ordering::Relaxed);
                if applied > 0 {
                    applied
                } else {
                    self.config.mining.frequency_mhz
                }
            },
            voltage_mv: S19J_HYBRID_CHIP_RAIL_TARGET_MV,
            temp_c,
            temp_source,
            hashrate_ghs: chain_hashrate_ghs,
            errors: crc_errors,
            // MINE-LIFE-2: "stalled" when this chain produced nonces earlier but
            // has gone quiet; "mining" only while recently active; "active"
            // before the first nonce. On a healthy unit `recently_active` is
            // true, so this collapses to the prior {mining|active} contract.
            status: if unique_nonces > 0 && !recently_active {
                "stalled"
            } else if unique_nonces > 0 {
                "mining"
            } else {
                "active"
            }
            .to_string(),
        }
    }

    /// Probe each candidate `/dev/ttyS*` device for chip responses.
    ///
    /// On am2 the FPGA→ASIC UART mapping isn't documented; chain1 may be on
    /// any of `/dev/ttyS{1,2,3,4}` (MMIO bases `0x41001000 … 0x41031000`
    /// per `dcentrald-hal/src/serial.rs:685`). We default to `/dev/ttyS2`
    /// but live evidence on `a lab unit` (2026-04-25) showed 0 chip replies there
    /// even with PIC voltage commanded ON, suggesting wrong UART.
    ///
    /// Strategy: for each candidate, open at 115200, broadcast GetAddress,
    /// read for 300 ms, count CHIPID-like replies. Return the device with
    /// the highest reply count, or `None` if all are silent.
    /// Caller is responsible for ensuring chain voltage is on before
    /// calling — chips need power to respond.
    fn probe_uart_for_chips(default_device: &str) -> Option<(String, usize)> {
        // Live ground truth on XIL (2026-05-12): never include `/dev/ttyPS0`
        // in this fallback sweep. It is the Zynq PS console/control UART, not a
        // hash-chain UART, and the file-backed read path can park shutdown in
        // `n_tty_read` while PIC/PSU heartbeat threads keep running. Restrict
        // the lab sweep to PL `/dev/ttyS*` candidates only.
        let candidates = am2_uart_fallback_candidates();
        let mut best: Option<(String, usize)> = None;
        info!(
            default_device,
            "UART probe: scanning {:?} for chip presence", candidates
        );
        for path in candidates.iter() {
            let mut serial = match SerialChainBackend::open(0, path, 115_200) {
                Ok(s) => s,
                Err(e) => {
                    warn!(path, error = %e, "UART probe: open failed");
                    continue;
                }
            };
            serial.set_response_len(BM1362_RESP_BODY_LEN);
            let _ = serial.read_all_responses(0); // drain
            if let Err(e) = serial.send_get_address_bm1397plus() {
                warn!(path, error = %e, "UART probe: send_get_address failed");
                continue;
            }
            let _ = serial.drain_tx();
            let summary = read_bm1362_serial_drain_summary(&serial, 300);
            let unique_count = bm1362_unique_chip_count_from_summary(&summary);
            info!(
                path,
                unique_chip_addrs = unique_count,
                summary = %summary,
                "UART probe: result"
            );
            match best {
                None => best = Some((path.to_string(), unique_count)),
                Some((_, prev)) if unique_count > prev => {
                    best = Some((path.to_string(), unique_count));
                }
                _ => {}
            }
        }
        match &best {
            Some((path, count)) if *count > 0 => {
                info!(
                    selected = path.as_str(),
                    chips = count,
                    "UART fallback scan SUCCESS: chain UART traffic detected on {}",
                    path
                );
                Some((path.clone(), *count))
            }
            _ => {
                warn!(
                    scanned_nodes = candidates.len(),
                    "UART fallback scan complete: all scanned PL UART candidates returned 0 chips. \
                     Chain UART path is broken upstream of the kernel UART driver \
                     (FPGA UART relay not asserted, OR HBx_RESET still asserted, OR \
                     chips electrically dead)."
                );
                None
            }
        }
    }

    /// Reset ASICs to 115200 baud from any previous baud rate.
    ///
    /// Uses BM1397+ headers (0x51 broadcast) via `send_chain_inactive_bm1397plus`
    /// and the triple-write helper for MiscCtrl.
    fn reset_asic_baud(serial_device: &str) {
        info!("Hot-start baud reset");
        if let Ok(serial) =
            SerialChainBackend::open(0, serial_device, dcentrald_hal::serial::BAUD_3125000)
        {
            let _ = serial.send_chain_inactive_bm1397plus();
            let _ = misc_ctrl_triple_write_serial(&serial, am2_misc_control_pre_baud());
            std::thread::sleep(Duration::from_millis(SERIAL_PACE_MIN_MS));
        }
        if let Ok(serial) = SerialChainBackend::open(0, serial_device, 1_562_500) {
            let _ = serial.send_chain_inactive_bm1397plus();
            let _ = misc_ctrl_triple_write_serial(&serial, am2_misc_control_pre_baud());
            std::thread::sleep(Duration::from_millis(SERIAL_PACE_MIN_MS));
        }
        std::thread::sleep(Duration::from_millis(50));
        info!("Baud reset complete");
    }

    /// BM1362 ASIC chain initialization matching the healthy bosminer `a lab unit`
    /// serial order as closely as possible.
    ///
    /// The 2026-04-22 healthy chain4 trace falsified our older synthesized init
    /// order. Stock bosminer does NOT do the full per-chip loop at 115200, and
    /// it does NOT use the B9/tail relatch block we previously tried to replay.
    /// The live order is:
    ///   1. broadcast A8 / 18 / A4 at 115200
    ///   2. chain_inactive x3 + set_address loop
    ///   3. broadcast 3C(hash) / 3C(delay) / 54 / 58 / GetAddress
    ///   4. write 0x70/0x08 twice, then 0x28 fast-uart
    ///   5. switch host UART to 3.125M and wait ~1s
    ///   6. run the full per-chip A8 / 18 / 3C x3 loop at fast baud
    ///
    /// We intentionally keep this path narrow and .139-oriented rather than
    /// extrapolating a broader BM1362 theory before the direct standalone path
    /// is proven on live hardware.
    fn init_asic_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
        pll_ramp: bool,
    ) -> Result<(SerialChainBackend, usize)> {
        info!(
            pll_ramp,
            "=== BM1362 ASIC INIT ({} chips, {} MHz) ===", chip_count, target_freq_mhz
        );

        //  (2026-05-23) dispatch sentinel. When `DCENT_AM2_USE_FPGA_CHAIN=1`
        // is set, attempt to open the FPGA-FIFO chain backend, run the proven
        // BM1362 init register sequence (BUILD_ID check, CTRL preserve-or-write
        // 0x00901002, BAUD 0x6C, FIFO reset), and report. The downstream
        // per-chip init (CHAIN_INACTIVE / SET_CHIP_ADDRESS / MiscCtrl x3 /
        // A8 / 3C / PLL ramp) is NOT yet ported through the trait — that
        // refactor is Phase 2.5 and lands in a follow-up wave. Until then,
        // this sentinel fails closed with a clear error so an operator who
        // opted into the gate sees exactly why the daemon stops.
        //
        // Default-off path (env-gate UNSET) is byte-identical — the entire
        // legacy SerialChainBackend init below is untouched.
        //
        // Standalone validation of the FpgaChainBackend open + init sequence
        // is via the `fpga_chain_backend_probe` example binary, which the
        // operator runs as part of Phase 0 before any daemon-side dispatch.
        if am2_use_fpga_chain_enabled() {
            info!(
                env = "DCENT_AM2_USE_FPGA_CHAIN=1",
                "Wave-26 dispatch: FPGA-chain backend selected; opening chain 0 via UIO"
            );
            match dcentrald_hal::fpga_chain_backend::FpgaChainBackend::open_am2_uio(0) {
                Ok(backend) => {
                    use dcentrald_hal::chain_backend::Bm1397PlusChainBackend;
                    info!(
                        chain_id = Bm1397PlusChainBackend::chain_id(&backend),
                        transport = backend.transport_label(),
                        "FpgaChainBackend opened — running initialize_chain_for_bm1362()"
                    );
                    if let Err(e) = backend.initialize_chain_for_bm1362() {
                        warn!(error = %e, "FpgaChainBackend::initialize_chain_for_bm1362 failed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "FpgaChainBackend::open_am2_uio failed");
                }
            }
            return Err(anyhow::anyhow!(
                "DCENT_AM2_USE_FPGA_CHAIN=1 — FPGA-chain init sequence executed for \
                 Phase-0 probe, but full per-chip init through the trait is Phase 2.5 \
                 future work. Unset the env flag to use the proven SerialChainBackend \
                 path; meanwhile validate the FPGA register state via the \
                 `fpga_chain_backend_probe` example binary on the target."
            ));
        }

        // RANK-5 (2026-06-13): open the companion second PL-UART (ttyS3) and HOLD it
        // open through this enum. Operator live-confirmed the hashboard WARMS (power
        // delivered) yet enum=0/zero-RX -> transport, not power. bosminer opens BOTH
        // ttyS1+ttyS3 (cold block-B); on `a lab unit` the two PL-UART cores may share a
        // chain-TX-clock/reset gate in the FPGA fabric. Mirror the ttyS1 work-tx
        // unbind for chain3-work-tx so ttyS3's kernel UART can claim its IRQ, then
        // open + assert OUT2. Best-effort + non-fatal: if it fails, the primary enum
        // still proceeds single-UART (byte-identical to today). Held in
        // `_companion_uart` until this fn returns (covers the whole enum).
        let _companion_uart: Option<SerialChainBackend> = if am2_open_both_uarts_before_enum_enabled(
        ) {
            let companion_dev = am2_dual_chain_second_uart();
            if companion_dev == serial_device {
                None
            } else {
                const C3_UNBIND: &str = "/sys/bus/platform/drivers/uio_pdrv_genirq/unbind";
                const C3_DEV: &str = "43c23000.chain3-work-tx";
                const C3_LINK: &str = "/sys/bus/platform/devices/43c23000.chain3-work-tx/driver";
                if std::path::Path::new(C3_LINK).exists() {
                    match std::fs::write(C3_UNBIND, C3_DEV) {
                        Ok(()) => info!(
                            device = C3_DEV,
                            "RANK-5: unbound chain3-work-tx (freed ttyS3 IRQ) for companion open"
                        ),
                        Err(e) => warn!(
                            device = C3_DEV,
                            error = %e,
                            "RANK-5: chain3-work-tx unbind failed (companion open may fall back)"
                        ),
                    }
                }
                match SerialChainBackend::open(0, &companion_dev, 115_200) {
                    Ok(mut c) => {
                        let _ = c.flush_io();
                        if let Err(e) = c.assert_mcr_out2() {
                            warn!(
                                companion = %companion_dev,
                                error = %e,
                                "RANK-5: companion OUT2 assert failed (non-fatal)"
                            );
                        }
                        info!(
                            companion = %companion_dev,
                            "RANK-5: companion PL-UART opened + OUT2 asserted and HELD before \
                             primary enum (bosminer opens BOTH ttyS1+ttyS3; .25 fabric may gate \
                             the chain TX clock on both UARTs)"
                        );
                        Some(c)
                    }
                    Err(e) => {
                        warn!(
                            companion = %companion_dev,
                            error = %e,
                            "RANK-5: companion UART open failed (non-fatal; proceeding single-UART)"
                        );
                        None
                    }
                }
            }
        } else {
            None
        };

        if am2_skip_hotstart_baud_reset_enabled() {
            info!(
                "DCENT_AM2_SKIP_HOTSTART_BAUD_RESET=1 — skipping reset_asic_baud() \
                 (cold-boot standalone: chain already at 115200, hot-baud reset would \
                 perturb the chain UART before the first enum)"
            );
        } else {
            Self::reset_asic_baud(serial_device);
        }

        let mut serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;
        let _ = serial.flush_io();
        //  (2026-05-31): post-chain-reset / pre-enumeration settle.
        // Default 50 ms (byte-identical to every prior wave + every other
        // platform). On `a lab unit` standalone the launcher sets
        // DCENT_AM2_POST_RESET_SETTLE_MS=1000 to give cold chips bosminer's
        // ~1 s post-HB_RESET-release settle before the first CHAIN_INACTIVE /
        // GetAddress — the P1 chip-wake lever (see am2_post_reset_settle_ms).
        std::thread::sleep(Duration::from_millis(am2_post_reset_settle_ms()));

        // 2026-06-11 (LIVE-PINNED) — ensure the FPGA UART RETURN relay is enabled
        // before the SET_ADDRESS enum walk (idempotent RMW; it is ALSO enabled
        // earlier, before the rail-evidence probe — see the call in run()). The
        // BM1362 daisy-chain RETURN (RO) line is routed into the PL soft-UART RX
        // through the FPGA fabric, gated by the 2-bit `gpio@41220000` AXI-GPIO
        // (bit0 co_relay_en + bit1 ro_relay_en); without it, TX drains but 0 RX.
        // Live read on a bosminer-engaged `a lab unit` (2026-06-11): DATA=0x3, TRI=0x0.
        // Gated default-OFF + `a lab unit` fingerprint + !handoff; fail-closed when set.
        am2_try_enable_fpga_uart_relay_cold("pre-enum (init_asic_chain)")?;

        // W2 register-diff (2026-06-14): set board-control +0x04 bit 8 (0x100) so
        // the SET_ADDRESS enum walk runs with the board-control register
        // byte-identical to a bosminer-engaged unit (0x134 vs DCENT's 0x034). The
        // live paired dump_fpga_regs_25.sh diff proved this is the only
        // persistent-fabric register DCENT never matches. Gated default-OFF +
        // `a lab unit` fingerprint; fail-closed when set; idempotent RMW that preserves
        // the C52 fan-mode low byte.
        am2_try_set_board_control_bit8("pre-enum (init_asic_chain)")?;

        // D6-1 (2026-06-13): assert MCR OUT2 on EVERY enum path, not only the
        // RE-018 cold sequence. `assert_mcr_out2()` is self-gating (a no-op unless
        // am2_mcr_out2_mode selects EnvOverride/Xil25Fingerprint), so this is
        // byte-identical on every non-.25 platform. The RE-018 branch re-asserts
        // (with a hard readback bail) after its port-wake — idempotent — but the
        // LEGACY enum path below previously NEVER asserted OUT2 on the kernel File
        // backend, so on `a lab unit` the FPGA UART TX clock-out stayed gated and the
        // chain went silent (enum=0). Now an RE-018 A/B (gate OFF) cannot silently
        // drop OUT2 and masquerade a transport failure as chip-physical.
        if let Err(e) = serial.assert_mcr_out2() {
            warn!(
                error = %e,
                "D6-1: assert_mcr_out2() failed in init_asic_chain (the RE-018 path re-asserts with a readback bail; the legacy path may be silent on .25 without OUT2)"
            );
        }

        // RE-018 (2026-05-31): byte-exact bosminer cold-wake replay. Default-OFF
        // + `a lab unit`-fingerprint-gated (am2_bm1362_re018_cold_sequence_enabled()).
        // When set on `a lab unit`, run the full decoded bosminer cold sequence (incl.
        // the per-chip reg 0x0C nonce-space base DCENT was missing + the
        // non-fatal late GetAddress poll) and return — bypassing the legacy
        // GetAddress-bail init entirely. Byte-identical to today on every other
        // platform / when the gate is unset (this branch is unreachable).
        if am2_bm1362_re018_cold_sequence_enabled() {
            info!(
                env = "DCENT_AM2_BM1362_RE018_COLD_SEQUENCE=1",
                serial_device = %serial_device,
                "RE-018 cold-wake path selected (.25 standalone, byte-exact bosminer replay)"
            );
            let count = bm1362_re018_cold_sequence(&serial, chip_count)?;
            return Ok((serial, count));
        }

        // PR-019 / R11-2 ablation (default-OFF). When the gate is unset every
        // call below is an immediate no-op — proven-path behaviour, timing,
        // and on-wire traffic are byte-for-byte unchanged.
        am2_ablation_log_params(target_freq_mhz, pll_ramp, 115_200);
        am2_ablation_probe(&serial, "pre_init_baseline");

        let chain_inactive_first = am2_init_chain_inactive_first_enabled();
        // .max(1): chip_count is operator-config-derived (serial_chip_count);
        // config validate() rejects 0, but guard the divide too (panic=abort).
        let addr_interval = 256u16 / (chip_count as u16).max(1);

        if chain_inactive_first {
            info!(
                chain_inactive_first = true,
                "Wave-25 chain-init order: CHAIN_INACTIVE + SET_CHIP_ADDRESS BEFORE A8/MiscCtrl/A4 broadcasts (Agent A LEAD hypothesis)"
            );

            // Step 1 ( swap): CHAIN_INACTIVE x3 FIRST so chips drop any
            // stale per-chip address state and accept the upcoming address
            // assignment.
            for _ in 0..3 {
                serial.send_chain_inactive_bm1397plus()?;
                std::thread::sleep(Duration::from_millis(300));
            }

            // Step 2 ( swap): assign addresses BEFORE register
            // broadcasts so each chip has a unique address when subsequent
            // GetAddress queries arrive. This is the load-bearing fix per
            // Agent A's RE finding: unaddressed chips can't answer
            // GetAddress -> 0/126 by construction.
            for i in 0..chip_count as u16 {
                let addr = (i * addr_interval) as u8;
                serial.send_set_address_bm1397plus(addr)?;
                if i % 16 == 15 {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            info!(
                "Enumeration: {} chips (interval={}) [Wave-25 chain_inactive_first order]",
                chip_count, addr_interval
            );

            // Step 3 ( swap): A8 / MiscCtrl x3 / A4 AFTER address
            // assignment, matching bosminer's canonical chain4 order
            // (`SUMMARY.md:80`).
            serial.send_write_reg_broadcast_bm1397plus(0xA8, am2_init_control_bcast())?;
            std::thread::sleep(Duration::from_millis(10));
            am2_ablation_probe(&serial, "post_a8");
            misc_ctrl_triple_write_serial(&serial, am2_misc_control_pre_baud())?;
            std::thread::sleep(Duration::from_millis(10));
            am2_ablation_probe(&serial, "post_miscctrl_x3");
            serial.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_DEFAULT)?;
            std::thread::sleep(Duration::from_millis(10));
        } else {
            // Step 1: bosminer's first healthy chain4 writes at 115200.
            serial.send_write_reg_broadcast_bm1397plus(0xA8, am2_init_control_bcast())?;
            std::thread::sleep(Duration::from_millis(10));
            am2_ablation_probe(&serial, "post_a8");
            misc_ctrl_triple_write_serial(&serial, am2_misc_control_pre_baud())?;
            std::thread::sleep(Duration::from_millis(10));
            am2_ablation_probe(&serial, "post_miscctrl_x3");
            serial.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_DEFAULT)?;
            std::thread::sleep(Duration::from_millis(10));

            // Step 2: Chain Inactive x3.
            for _ in 0..3 {
                serial.send_chain_inactive_bm1397plus()?;
                std::thread::sleep(Duration::from_millis(300));
            }

            // Step 3: Assign addresses (BM1397+ framing)
            for i in 0..chip_count as u16 {
                let addr = (i * addr_interval) as u8;
                serial.send_set_address_bm1397plus(addr)?;
                if i % 16 == 15 {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            info!(
                "Enumeration: {} chips (interval={})",
                chip_count, addr_interval
            );
        }

        // Do not hard-verify yet. SetChipAddress is fire-and-forget, and the
        // traced healthy BM1362 order sends the remaining 115200 broadcast
        // block before the GetAddress sentinel. Cold XIL bring-up proved that
        // a pre-block verifier can false-fail with 0 replies even though power,
        // dsPIC enable, and the UART backend are alive. The real 115200 gate
        // lives after the stock-style 3C/54/58/ticket/relay block below.

        // Step 4: Remaining healthy 115200 broadcast block.
        serial.send_write_reg_broadcast_bm1397plus(0x3C, CORE_REG_HASH_CLK)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x3C, CORE_REG_CLK_DELAY)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x54, ANALOG_MUX_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x58, IO_DRIVER_NORMAL)?;
        std::thread::sleep(Duration::from_millis(10));
        am2_ablation_probe(&serial, "post_3c_x3_broadcast_block");

        // Step 4b: TICKET_MASK + HASH_COUNTING_NUMBER + UART relay.
        //
        // TICKET_MASK (reg 0x14) is the hardware difficulty filter. Without
        // it the chip drops every nonce at hardware level — silent zero
        // production regardless of work dispatch.
        // HASH_COUNTING_NUMBER (reg 0x10) tells the chain how to distribute
        // the 32-bit nonce range across N chips. With 126 chips the value
        // is 0x0000_1381 (per `NONCE_RANGE_126` constant). Without this all
        // 126 chips hash the same range — wasted silicon and rare nonces.
        // UART_RELAY candidate writes (0x2C BM1366-style + 0x34
        // BM1397+style) are preserved for R6-7 capture work but skipped
        // by default. The diagnostic-only glitch monitor mirror at
        // 0x43D00030/0x43D00034 (Braiins-am2 only) reflects state; it is
        // NOT a control surface (W13.B1 reclass).
        serial.send_write_reg_broadcast_bm1397plus(0x14, TICKET_MASK_256)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x10, NONCE_RANGE_126)?;
        std::thread::sleep(Duration::from_millis(10));
        maybe_write_bm1362_uart_relay(&serial, "bm1362_step4_115200")?;
        info!(
            ticket_mask = format_args!("0x{:08X}", TICKET_MASK_256),
            nonce_range = format_args!("0x{:08X}", NONCE_RANGE_126),
            relay_lab_enabled = bm1362_uart_relay_lab_enabled(),
            "BM1362: ticket_mask + nonce_range + optional UART relay stage @ 115200"
        );

        // Step 4c: VERIFY enumeration after the full stock-style 115200 setup
        // block. SetChipAddress and the register broadcasts above are fire-
        // and-forget; GetAddress is the first authoritative proof that the
        // chain both heard the setup and can drive RX back to the controller.
        // If count==0 we know there is no point continuing into PLL/baud-
        // upgrade/work-dispatch: the operator would only see
        // `tx_full=true rx_empty=true nonces_5s=0` forever.
        // (DCENT_RE 2026-05-14 XIL cold-boot fix: this gate must not run
        // before the traced 115200 broadcast block.)
        serial.set_response_len(BM1362_RESP_BODY_LEN);
        let (presence_summary, unique_count) = verify_bm1362_get_address(&serial, 300)
            .context("Chain presence GetAddress verification failed at 115200")?;
        info!(
            baud = 115_200,
            expected = chip_count,
            unique_chip_replies = unique_count,
            summary = %presence_summary,
            "Chain presence verification"
        );
        if unique_count == 0 {
            if am2_continue_past_zero_enum_enabled() {
                warn!(
                    serial_device = %serial_device,
                    summary = %presence_summary,
                    "DCENT_AM2_CONTINUE_PAST_ZERO_ENUM=1: 0 chips responded to GetAddress at 115200 — \
                     DOWNGRADING the enum=0 hard-bail to a warning (DIAGNOSTIC). Continuing into \
                     PLL/baud-upgrade so the later post-clock GetAddress re-probes can test whether the \
                     chips needed clocking before they enumerate (H5). No chips detected => no hashing \
                     (no thermal risk); expect tx_full=true unless a later probe finds chips."
                );
            } else {
                anyhow::bail!(
                    "Chain presence verification FAILED — 0 chips responded to GetAddress at 115200 on {} after the full 115200 setup block. \
                     Either the chain is not powered (PIC enable_voltage didn't actually engage 13.7V), \
                     the wrong /dev/ttyS* is wired to this chain, the BM1362 UART relay/setup register mix is still incomplete, \
                     or the chips themselves are dead. Aborting before PLL/baud-upgrade so the silent-init failure mode \
                     (`tx_full=true forever`) does not happen. \
                     (Set DCENT_AM2_CONTINUE_PAST_ZERO_ENUM=1 to downgrade this to a warning for the H5 clock-ordering probe.) summary: {}",
                    serial_device,
                    presence_summary
                );
            }
        }
        if unique_count < chip_count as usize {
            warn!(
                expected = chip_count,
                got = unique_count,
                "Chain presence: only some chips replied — continuing but expect partial yield"
            );
        } else {
            info!(
                count = unique_count,
                "Chain presence verified: all {} chips replied", chip_count
            );
        }

        // Step 5: PLL programming.
        //
        // Two paths, gated by `mining.am2_pll_ramp` (default ON):
        //
        // (A) RAMP (BM1368 .135 cadence) —  hypothesis fix.
        //     BM1362 default state is ~50 MHz on POR. Slamming straight to
        //     525 MHz in two writes (the legacy path) does not give the on-die
        //     PLL/postdiv chain time to acquire lock — most likely root cause
        //     of `a lab unit` chain UART silence at 13.7 V engaged rail.
        //     BM1368 (working on `a lab unit`) ramps `200 → 525 MHz` in 25 MHz steps
        //     × 100 ms settle (`serial_mining.rs::init_bm1368_chain` Step 7).
        //     We replicate that here for BM1362 starting at 400 MHz (BM1362
        //     PLL-table minimum) and ramping up to the target in 25 MHz steps.
        //
        // (B) SLAM (legacy, traced) — fall-back if the ramp introduces a
        //     regression on a known-good unit. Two writes of the traced
        //     `0x40A8_0265` value (or lookup-derived for non-525 targets)
        //     with the PLL0 divider preconfig in between, 10 ms spacing.
        //
        // Both paths emit a best-effort PLL-lock readback after the final
        // write (the BM1362 family puts the lock bit in the MSB of register
        // 0x08); the readback can't be relied on through the FPGA chain UART
        // path on `a lab unit` (that's the very blocker we're trying to clear), but
        // when present it confirms ramp success.
        let target_clamped = target_freq_mhz.clamp(400, 597);
        let (final_pll_reg, _) = bm1362_pll_lookup(target_clamped);
        if pll_ramp {
            // PLL0 divider preconfig — same value bosminer uses, kept at the
            // head of the ramp so the divider chain is sane before we touch
            // the multiplier/postdiv settings via reg 0x08.
            serial.send_write_reg_broadcast_bm1397plus(
                BM1362_PLL0_DIVIDER_REG,
                BM1362_TRACE_PLL0_DIVIDER,
            )?;
            std::thread::sleep(Duration::from_millis(10));

            let ramp_start: u16 = 400;
            let ramp_step: u16 = 25;
            let ramp_settle_ms: u64 = 100;
            let steps = bm1362_pll_ramp_sequence(ramp_start, target_clamped, ramp_step);
            info!(
                start_mhz = ramp_start,
                target_mhz = target_clamped,
                step_mhz = ramp_step,
                settle_ms = ramp_settle_ms,
                step_count = steps.len(),
                "BM1362 PLL ramp engaged (Wave-6 hypothesis fix)"
            );
            for (i, (pll_reg, freq_mhz)) in steps.iter().enumerate() {
                serial.send_write_reg_broadcast_bm1397plus(0x08, *pll_reg)?;
                std::thread::sleep(Duration::from_millis(ramp_settle_ms));
                // Best-effort PLL-lock readback. We send a broadcast READ on
                // reg 0x08 and drain whatever response frames come back; if
                // ANY reply has the lock bit set we log it. If no replies
                // arrive (chain UART RX silent — which is the actual blocker
                // we're testing for) we log warn and proceed. Never bail on
                // an absent readback.
                if let Err(e) = serial.send_read_reg_bm1397plus(0x00, 0x08) {
                    debug!(
                        step = i + 1,
                        of = steps.len(),
                        error = %e,
                        "PLL ramp: read 0x08 send failed (non-fatal)"
                    );
                }
                let resps = serial.read_all_responses(20).unwrap_or_default();
                let mut any_lock = false;
                for r in &resps {
                    if r.len() >= 4 {
                        let raw = u32::from_be_bytes([r[0], r[1], r[2], r[3]]);
                        if raw & PLL_LOCK_BIT != 0 {
                            any_lock = true;
                        }
                    }
                }
                debug!(
                    step = i + 1,
                    of = steps.len(),
                    target_mhz = freq_mhz,
                    pll_reg = format_args!("0x{:08X}", *pll_reg),
                    replies = resps.len(),
                    any_lock,
                    "PLL ramp step"
                );
            }
            // Re-write PLL0 divider after ramp (mirror legacy slam tail) so
            // the divider chain is settled at the post-ramp clock.
            serial.send_write_reg_broadcast_bm1397plus(
                BM1362_PLL0_DIVIDER_REG,
                BM1362_TRACE_PLL0_DIVIDER,
            )?;
            std::thread::sleep(Duration::from_millis(10));
            // Final write at exactly the target PLL value so downstream
            // FastUART/MiscCtrl writes hit a steady-state PLL.
            serial.send_write_reg_broadcast_bm1397plus(0x08, final_pll_reg)?;
            std::thread::sleep(Duration::from_millis(ramp_settle_ms));
            info!(
                final_pll = format_args!("0x{:08X}", final_pll_reg),
                target_mhz = target_clamped,
                "BM1362 PLL ramp complete"
            );
        } else {
            // Legacy SLAM path. Preserved for fallback / regression debugging.
            let traced_pll_param = if target_freq_mhz == 525 {
                BM1362_TRACE_PLL_PARAM_525
            } else {
                let (pll_reg, actual_freq) = bm1362_pll_lookup(target_clamped);
                warn!(
                    requested_mhz = target_freq_mhz,
                    traced_only_mhz = 525,
                    fallback_pll = format_args!("0x{:08X}", pll_reg),
                    fallback_actual_mhz = actual_freq,
                    "BM1362 traced init only has a live PLL param for 525 MHz; falling back to lookup-derived PLL value"
                );
                pll_reg
            };
            serial.send_write_reg_broadcast_bm1397plus(
                BM1362_PLL0_DIVIDER_REG,
                BM1362_TRACE_PLL0_DIVIDER,
            )?;
            std::thread::sleep(Duration::from_millis(10));
            serial.send_write_reg_broadcast_bm1397plus(0x08, traced_pll_param)?;
            std::thread::sleep(Duration::from_millis(10));
            serial.send_write_reg_broadcast_bm1397plus(
                BM1362_PLL0_DIVIDER_REG,
                BM1362_TRACE_PLL0_DIVIDER,
            )?;
            std::thread::sleep(Duration::from_millis(10));
            serial.send_write_reg_broadcast_bm1397plus(0x08, traced_pll_param)?;
            std::thread::sleep(Duration::from_millis(10));
            info!(
                pll = format_args!("0x{:08X}", traced_pll_param),
                "BM1362 PLL slam complete (legacy path, am2_pll_ramp=false)"
            );
        }

        if am2_skip_fast_uart_enabled() {
            warn!(
                unique_chip_replies = unique_count,
                baud = 115_200,
                fpga_baud_divisor = format_args!("0x{:02X}", fpga_chain::BAUD_REG_115200),
                "DCENT_AM2_SKIP_FAST_UART=1 - skipping BM1362 FastUART handoff and keeping command/work path at 115200"
            );
            // The proven BM1362 paths (serial_mining.rs, am3_bb_mining.rs) run
            // the per-chip A8/MiscCtrl/3C loop to activate the cores.
            // init_asic_chain ran it only as fast-baud Step 7, so skip-FastUART
            // previously left the cores broadcast-only-activated — a suspected
            // cause of the skip-FastUART zero-nonce result. Run it here at
            // 115200 unless explicitly opted out for an A/B comparison.
            let mut skip_count = unique_count;
            if am2_skip_115200_per_chip_enabled() {
                warn!("DCENT_AM2_SKIP_115200_PER_CHIP=1 - leaving BM1362 cores broadcast-only-activated (A/B comparison mode)");
            } else {
                info!(
                    "Running BM1362 per-chip init loop at 115200 (skip-FastUART core activation)"
                );
                am2_ablation_probe(&serial, "pre_per_chip_loop_115200");
                bm1362_per_chip_fast_init(&serial, chip_count, addr_interval)?;
                am2_ablation_probe(&serial, "post_per_chip_loop_115200");
                let (skip_summary, skip_unique) = verify_bm1362_get_address(&serial, 300)
                    .context("Post-per-chip GetAddress verification failed at 115200")?;
                info!(
                    baud = 115_200,
                    expected = chip_count,
                    unique_chip_replies = skip_unique,
                    summary = %skip_summary,
                    "BM1362 post-per-chip GetAddress verification at 115200"
                );
                if skip_unique == 0 {
                    anyhow::bail!(
                        "BM1362 post-per-chip verification FAILED at 115200 on {} — chain replied before the per-chip loop but went silent after it. summary: {}",
                        serial_device,
                        skip_summary
                    );
                }
                skip_count = skip_unique;
            }
            am2_ablation_probe(&serial, "pre_mining");
            info!(
                pll = format_args!("0x{:08X}", final_pll_reg),
                unique_chip_replies = skip_count,
                "=== BM1362 INIT COMPLETE - {} unique chip replies at {} MHz (FastUART skipped; chain remains at 115200) ===",
                skip_count,
                target_freq_mhz
            );
            return Ok((serial, skip_count));
        }

        // Step 6: ASIC baud register (FastUART)
        let fast_uart_switch_host_first = am2_fast_uart_switch_host_first_enabled();
        let fast_uart_value = am2_fast_uart_value();
        let fast_uart_baud = am2_fast_uart_baud();
        // R11-7 #1 live-tuning lever: host settle delay AFTER the host UART
        // baud switch and BEFORE the first fast-baud GetAddress probe.
        // Default 1000 ms (byte-identical when DCENT_AM2_FASTUART_SETTLE_MS is
        // unset); env override clamped to [100, 3000]. Resolved + logged once.
        let fast_uart_settle = Duration::from_millis(am2_fast_uart_settle_ms());

        // T1 A/B: optionally use the BM1362 factory-jig `set_chain_baud` (PLL1
        // reclock RMW) instead of the fixed reg 0x28 + reg 0x18 method. The jig
        // path writes reg 0x60 + reg 0x28 itself and does NOT touch reg 0x18, so
        // the MiscCtrl-post-fast write below is skipped when this gate is on.
        let jig_reclock = am2_baud_jig_pll1_reclock_enabled();
        if jig_reclock {
            apply_bm1362_jig_pll1_reclock(&serial, fast_uart_baud)?;
        } else {
            serial.send_write_reg_broadcast_bm1397plus(0x28, fast_uart_value)?;
            serial.drain_tx()?;
            std::thread::sleep(Duration::from_millis(10));
        }

        let (immediate_fast_summary, immediate_fast_unique_count) = if fast_uart_switch_host_first {
            warn!(
                    fast_uart = format_args!("0x{:08X}", fast_uart_value),
                    baud = fast_uart_baud,
                    "DCENT_AM2_FASTUART_SWITCH_HOST_FIRST=1 - switching host UART immediately after FastUART before post-fast MiscCtrl"
                );
            let baud_before = serial.baud();
            serial.set_baud(fast_uart_baud)?;
            info!(
                baud_before,
                baud_after = serial.baud(),
                requested = fast_uart_baud,
                "FastUART (host-switch-first): host UART backend baud transition"
            );
            // 300 ms was too short vs the canonical path's 1000 ms — the chip
            // UART divider needs time to re-lock after the FastUART register
            // write before a fast-baud GetAddress probe reads cleanly. Tunable
            // via DCENT_AM2_FASTUART_SETTLE_MS (default 1000, clamp [100,3000]).
            std::thread::sleep(fast_uart_settle);

            let (pre_misc_summary, pre_misc_unique_count) = verify_bm1362_get_address(&serial, 300)
                .with_context(|| {
                    format!(
                        "Fast-baud GetAddress probe before post-fast MiscCtrl failed at {}",
                        fast_uart_baud
                    )
                })?;
            info!(
                baud = fast_uart_baud,
                expected = chip_count,
                unique_chip_replies = pre_misc_unique_count,
                summary = %pre_misc_summary,
                "BM1362 fast-baud GetAddress before post-fast MiscCtrl"
            );

            if !jig_reclock {
                misc_ctrl_triple_write_serial(&serial, am2_misc_control_post_fast())?;
            }
            std::thread::sleep(Duration::from_millis(100));
            let post_misc = verify_bm1362_get_address(&serial, 300).with_context(|| {
                format!(
                    "Fast-baud GetAddress probe after post-fast MiscCtrl failed at {}",
                    fast_uart_baud
                )
            })?;
            info!(
                pll = format_args!("0x{:08X}", final_pll_reg),
                fast_uart = format_args!("0x{:08X}", fast_uart_value),
                baud = fast_uart_baud,
                misc_ctrl = format_args!("0x{:08X}", am2_misc_control_post_fast()),
                "Baud upgraded using host-switch-first FastUART order"
            );
            post_misc
        } else {
            if !jig_reclock {
                misc_ctrl_triple_write_serial(&serial, am2_misc_control_post_fast())?;
            }
            std::thread::sleep(Duration::from_millis(10));
            let baud_before = serial.baud();
            serial.set_baud(fast_uart_baud)?;
            info!(
                baud_before,
                baud_after = serial.baud(),
                requested = fast_uart_baud,
                "FastUART (canonical): host UART backend baud transition"
            );
            // Host settle before the fail-closed fast-baud GetAddress sentinel.
            // Tunable via DCENT_AM2_FASTUART_SETTLE_MS (default 1000, clamp [100,3000]).
            std::thread::sleep(fast_uart_settle);
            info!(
                pll = format_args!("0x{:08X}", final_pll_reg),
                fast_uart = format_args!("0x{:08X}", fast_uart_value),
                baud = fast_uart_baud,
                misc_ctrl = format_args!("0x{:08X}", am2_misc_control_post_fast()),
                "Baud upgraded using traced BM1362 FastUART + MiscCtrl order"
            );

            // Verify the baud handoff immediately, before the long per-chip
            // fast-baud programming loop can disturb the chain.
            verify_bm1362_get_address(&serial, 300).with_context(|| {
                format!(
                    "Immediate fast-baud GetAddress verification failed at {}",
                    fast_uart_baud
                )
            })?
        };

        info!(
            baud = fast_uart_baud,
            expected = chip_count,
            unique_chip_replies = immediate_fast_unique_count,
            summary = %immediate_fast_summary,
            "BM1362 immediate fast-baud GetAddress verification"
        );
        if immediate_fast_unique_count == 0 {
            anyhow::bail!(
                "BM1362 immediate fast-baud verification FAILED — 0 chips responded to GetAddress at {} baud on {} immediately after the baud switch. \
                 The chain replied at 115200 but the FastUART handoff itself did not produce a readable fast-baud chain. summary: {}",
                fast_uart_baud,
                serial_device,
                immediate_fast_summary
            );
        }
        if immediate_fast_unique_count < chip_count as usize {
            warn!(
                expected = chip_count,
                got = immediate_fast_unique_count,
                "BM1362 immediate fast-baud verification: only some chips replied"
            );
        }
        am2_ablation_probe(&serial, "post_fast_baud_switch");

        // Step 6b: re-arm UART relay AT 3.125M. Some chips latch the relay
        // value off the post-baud-upgrade clock; writing it again here
        // ensures the latest broadcast wins regardless of which baud the
        // chip was at when it accepted the previous write.
        maybe_write_bm1362_uart_relay(&serial, "bm1362_step6_fast_baud")?;

        if am2_env_flag("DCENT_AM2_SKIP_FAST_PER_CHIP_LOOP") {
            warn!(
                unique_chip_replies = immediate_fast_unique_count,
                "DCENT_AM2_SKIP_FAST_PER_CHIP_LOOP=1 — skipping post-fast per-chip A8/18/3C loop; proceeding with immediate fast-baud verified chain"
            );
            am2_ablation_probe(&serial, "pre_mining");
            info!(
                pll = format_args!("0x{:08X}", final_pll_reg),
                unique_chip_replies = immediate_fast_unique_count,
                "=== BM1362 INIT COMPLETE — {} unique chip replies at {} MHz (post-fast per-chip loop skipped) ===",
                immediate_fast_unique_count,
                target_freq_mhz
            );
            return Ok((serial, immediate_fast_unique_count));
        }

        // Step 7: Healthy bosminer runs the full per-chip A8 / 18 / 3C x3 loop
        // only AFTER the fast-baud transition.
        am2_ablation_probe(&serial, "pre_per_chip_loop_fast_baud");
        bm1362_per_chip_fast_init(&serial, chip_count, addr_interval)?;
        am2_ablation_probe(&serial, "post_per_chip_loop_fast_baud");

        // Step 7b: verify the baud upgrade with a real GetAddress/read at
        // 3.125M. This is the bosminer-canonical sentinel: a configured
        // `chip_count` proves nothing after FastUART, so fail closed before
        // work dispatch if no unique chip replies are measured.
        let (fast_baud_summary, fast_baud_unique_count) = verify_bm1362_get_address(&serial, 300)
            .with_context(|| {
            format!(
                "Post-per-chip fast-baud GetAddress verification failed at {}",
                fast_uart_baud
            )
        })?;
        info!(
            baud = fast_uart_baud,
            expected = chip_count,
            unique_chip_replies = fast_baud_unique_count,
            summary = %fast_baud_summary,
            "BM1362 post-per-chip fast-baud GetAddress verification"
        );
        if fast_baud_unique_count == 0 {
            anyhow::bail!(
                "BM1362 post-per-chip fast-baud verification FAILED — 0 chips responded to GetAddress at 3.125M on {}. \
                 The chain replied immediately after FastUART but disappeared after the post-fast per-chip loop; \
                 refusing to dispatch FPGA work into a silent chain. summary: {}",
                serial_device,
                fast_baud_summary
            );
        }
        if fast_baud_unique_count < chip_count as usize {
            warn!(
                expected = chip_count,
                got = fast_baud_unique_count,
                "BM1362 post-per-chip fast-baud verification: only some chips replied — continuing but expect partial yield"
            );
        } else {
            info!(
                count = fast_baud_unique_count,
                "BM1362 post-per-chip fast-baud verification: all {} chips replied", chip_count
            );
        }

        am2_ablation_probe(&serial, "pre_mining");
        info!(
            pll = format_args!("0x{:08X}", final_pll_reg),
            unique_chip_replies = fast_baud_unique_count,
            "=== BM1362 INIT COMPLETE — {} unique chip replies at {} MHz ===",
            fast_baud_unique_count,
            target_freq_mhz
        );
        Ok((serial, fast_baud_unique_count))
    }

    /// Alternative am2 work-dispatch loop — `DCENT_AM2_SERIAL_WORK_DISPATCH=1`.
    ///
    /// Sends the proven BM1362 88-byte serial work frame over the chain UART
    /// (`DevmemUart`, already proven for GetAddress at 115200 on XIL) and parses
    /// 11-byte serial nonce frames — bypassing the am2 FPGA WORK_TX/WORK_RX
    /// FIFO entirely. Reuses the exact codec
    /// (`build_serial_work_frame` / `parse_bm1362_serial_nonce`) proven on the
    /// am3-bb `a lab unit` accepted-share milestone. This sidesteps the long-standing
    /// am2 FPGA-FIFO own-dispatch zero-nonce question. This path reconstructs
    /// BM1362 BIP320 rolled versions from nonce metadata, so the Stratum router
    /// may negotiate BIP310 here; the FPGA loop keeps version rolling disabled.
    /// Runtime-only, env-gated, default-off — `a lab unit` is unaffected.
    #[allow(clippy::too_many_arguments)]
    async fn run_am2_serial_dispatch_loop(
        &self,
        serial: SerialChainBackend,
        mut job_rx: mpsc::Receiver<dcentrald_stratum::types::JobTemplate>,
        share_tx: mpsc::Sender<dcentrald_stratum::types::ValidShare>,
        chip_count: u8,
        i2c: Option<I2cServiceHandle>,
        pic_addr: u8,
        pic_fw: Option<u8>,
        mut thermal_supervisor: Option<Am2ThermalSupervisor>,
        thermal_poll_ms: u64,
        watchdog_liveness: Arc<AtomicU64>,
        // AT-DASH: shared pool-share accounting (written by the status task),
        // read here on each hashrate tick to compute the real GH/s estimate and
        // publish a live `MinerState` for `/api/status` + the dashboard.
        share_accounting: Arc<Am2ShareAccounting>,
        pool_quality: Arc<RwLock<dcentrald_stratum::pool_quality::PoolQualitySnapshot>>,
    ) -> Result<()> {
        info!(
            chip_count,
            "=== AM2 SERIAL-WORK-DISPATCH MINING ACTIVE (BM1362 88-byte serial frames over the chain UART; FPGA WORK_TX FIFO bypassed) ==="
        );

        let mut work_builder = dcentrald_stratum::share_pipeline::WorkBuilder::new();
        let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
        let mut asic_job_id: u8 = 0;
        let mut work_history: Vec<VecDeque<WorkEntry>> = (0..128)
            .map(|_| VecDeque::with_capacity(WORK_HISTORY_PER_ID))
            .collect();
        // Key includes version_bits_raw: BM1362 rolls BIP320 internally and can
        // find the SAME nonce at the SAME job under DIFFERENT rolled versions —
        // those are genuinely-different 80-byte headers (two valid shares), so the
        // version bits must be part of the dedup key or the second is dropped (lost
        // revenue). Mirrors work_dispatcher.rs keying midstate_idx. When the chip
        // rolls 0 (non-rolling configs) all reports carry vbits=0 and the key
        // collapses to the prior (job_id, nonce) behaviour.
        let mut seen_shares: BTreeSet<(u8, u32, u16)> = BTreeSet::new();

        let mut total_work: u64 = 0;
        let mut total_nonces: u64 = 0;
        let mut total_rx_frames: u64 = 0;
        let mut shares_submitted: u64 = 0;
        let mut hr_nonces: u64 = 0;
        // AT-DASH telemetry (diagnostic for the ~400x hashrate gap):
        //   `unique_nonces` — distinct (job_id,nonce,vbits) reports, the
        //     dedup-survivor count. `total_nonces - unique_nonces` is the
        //     duplicate spam the chain is re-presenting.
        //   `crc_errors` — full-length frames that failed BM1362 nonce parse.
        //   `dsPIC nonce attribution` — this single-chain loop drives ONE dsPIC,
        //     so every unique nonce attributes to `pic_addr`'s chain.
        let mut unique_nonces: u64 = 0;
        let mut crc_errors: u32 = 0;
        // AT-DASH: loop-start anchor for the cumulative achieved-difficulty GH/s
        // estimate (the pool-credited hashrate truth).
        let loop_started = Instant::now();
        let mut first_work_at: Option<Instant> = None;
        // MINE-LIFE-1: timestamp of the most recent nonce (None until the first
        // one). The startup no-nonce guard owns the "never produced a nonce"
        // case; this drives the SEPARATE mid-run stall guard + the honest
        // recent-activity status (MINE-LIFE-2).
        let mut last_nonce_at: Option<Instant> = None;
        // MINE-LIFE-2 (eco-stable): rolling history of recent hashrate-tick
        // snapshots `(Instant, achieved_difficulty_sum)` (oldest first), pruned
        // to ~AM2_RECENT_HASHRATE_WINDOW_S. Drives the REAL recent-window
        // ("current"/5 s tile) hashrate over a WIDE window so a sparse-share eco
        // unit shows a STABLE non-zero value instead of flickering to 0 between
        // shares — distinct from the cumulative-since-boot average.
        let mut hr_window: VecDeque<(Instant, f64)> = VecDeque::new();
        let mut carry: Vec<u8> = Vec::with_capacity(256);
        // DCENT_AM2_LOG_RX_FRAMES — lab-only diagnostic, hex-dump first N
        // raw RX frames to identify the on-wire shape vs the .79 BB
        // 11-byte serial-nonce parser. See am2_log_rx_frames_max() above.
        let log_rx_max: u64 = am2_log_rx_frames_max();
        let mut rx_frames_logged: u64 = 0;
        let mut rxbuf = [0u8; 512];

        let no_nonce_timeout = if self.config.mining.am2_no_nonce_timeout_s == 0 {
            None
        } else {
            Some(Duration::from_secs(
                self.config.mining.am2_no_nonce_timeout_s,
            ))
        };
        if let Some(t) = no_nonce_timeout {
            info!(
                timeout_s = t.as_secs(),
                "AM2 serial-dispatch no-nonce fail-closed guard armed"
            );
        } else {
            warn!("AM2 serial-dispatch no-nonce fail-closed guard disabled by config");
        }

        // MINE-LIFE-1: GENEROUS mid-run nonce-stall guard (trips only AFTER
        // nonces have flowed and then stop for the generous window — separate
        // from the startup no-nonce guard above).
        let mid_run_stall_timeout =
            am2_mid_run_nonce_stall_timeout(self.config.mining.am2_no_nonce_timeout_s);
        match mid_run_stall_timeout {
            Some(t) => info!(
                timeout_s = t.as_secs(),
                "AM2 serial-dispatch mid-run nonce-stall fail-closed guard armed"
            ),
            None => info!("AM2 serial-dispatch mid-run nonce-stall guard disabled"),
        }

        // ----  am2/BM1362 FREQUENCY-ONLY autotuner rig ----
        //
        // DEFAULT-OFF. `am2_frequency_autotune_opted_in` is false unless
        // the operator set `[autotuner] am2_frequency_autotune = true`
        // OR `DCENT_AM2_FREQUENCY_AUTOTUNE=1`. When false, EVERYTHING
        // below is skipped: `freq_cmd_rx` stays `None` (its select arm
        // is `future::pending()` — never fires), `chain_stats` stays
        // `None` (no nonce/error accumulation, no snapshot timer), no
        // tuner task is spawned. The serial-dispatch loop is then
        // byte-identical to the proven `a lab unit`/.109 milestone path:
        //  run/env unaffected.
        //
        // When opted in: spawn the `dcentrald-autotuner` characterizer
        // FREQUENCY-ONLY (voltage/DVFS HARD-pinned off by
        // `pin_am2_bm1362_frequency_only()`, band clamped [245,545],
        // QUIET `Efficiency` objective default), feed it a single
        // synthetic chip-count-aware chain snapshot, and route its
        // SetChainFreq/SetChipFreq suggestions through the proven
        // broadcast-PLL primitive. SetVoltage/VerifyVoltage are
        // HARD-REFUSED here as last-line defense-in-depth.
        let freq_autotune_opt_in = am2_frequency_autotune_opted_in(&self.config);
        let mut freq_cmd_rx: Option<mpsc::Receiver<dcentrald_autotuner::FreqCommand>> = None;
        let mut chain_stats: Option<Am2SerialChainStats> = None;
        let mut autotune_stats_sender: Option<
            mpsc::Sender<dcentrald_autotuner::ChipStatsSnapshot>,
        > = None;
        // Fingerprint-gated warm-start guard: the autotuner's own
        // state-resume path is fingerprint/chip-count gated (it will not
        // replay an N-chip profile at a different count — see
        // `state_persistence` tests). We additionally pin the live
        // enumerated chip count this AC cycle so a mid-run chip-count
        // change is a StepUpGate-blocking event (recompute expected NPS
        // and force a fresh measurement epoch rather than trusting a
        // stale N-chip frequency).
        let autotune_chip_count_at_start = chip_count;
        let mut autotune_chip_count_warned = false;
        // Snapshot cadence: feed the tuner a fresh chain snapshot every
        // `measurement_window_s` (default 30 s). Bounded ≥5 s so a
        // pathological config can't busy-spin the snapshot timer.
        let mut snapshot_timer = {
            let win_s = self.config.autotuner.measurement_window_s.max(5);
            tokio::time::interval(Duration::from_secs(win_s))
        };

        if freq_autotune_opt_in {
            let autotune_config = build_am2_freq_only_autotuner_config(&self.config);
            // HARD invariants re-asserted at the spawn site (the pin
            // already enforces them; this is a fail-closed assertion so
            // a future refactor of `build_am2_freq_only_autotuner_config`
            // that regressed voltage-off would be caught loudly here and
            // the tuner simply would NOT be spawned).
            if autotune_config.voltage_optimization || autotune_config.dvfs_enabled {
                error!(
                    voltage_optimization = autotune_config.voltage_optimization,
                    dvfs_enabled = autotune_config.dvfs_enabled,
                    "am2 freq-only autotuner: voltage/DVFS pin REGRESSED — refusing to spawn the tuner (NO live am2 voltage this wave). Falling back to the proven static-frequency path."
                );
            } else {
                let (fc_tx, fc_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(64);
                let (stats_tx, stats_rx) =
                    mpsc::channel::<dcentrald_autotuner::ChipStatsSnapshot>(64);
                freq_cmd_rx = Some(fc_rx);
                chain_stats = Some(Am2SerialChainStats::new(0));
                autotune_stats_sender = Some(stats_tx);

                let nominal_mhz = self.config.mining.frequency_mhz;
                let power_calibration = std::sync::Arc::new(std::sync::RwLock::new(
                    self.config.power.calibration.clone().unwrap_or_default(),
                ));
                // Single synthetic chain carrying the LIVE enumerated
                // chip count so the autotuner's expected-NPS prediction
                // (894 nonce-attribution slots per BM1362 chip, W6.8)
                // scales 28..126 with the actual chain — not a fixed
                // 126 and not the 4-engine count.
                let chain_infos = vec![dcentrald_autotuner::ChainTuneInfo {
                    chain_id: 0,
                    chip_count: autotune_chip_count_at_start,
                    voltage_mv: 0, // freq-only: voltage is NEVER read or written here
                    chip_id: 0x1362,
                    hardware_identity: dcentrald_autotuner::ChainHardwareIdentity {
                        eeprom_serial: None,
                        eeprom_fingerprint: None,
                        dspic_fw_byte: pic_fw,
                    },
                }];
                let tuner_shutdown = self.shutdown.clone();
                info!(
                    chip_count = autotune_chip_count_at_start,
                    target_mode = ?autotune_config.target_mode,
                    freq_band = format_args!(
                        "{}-{} MHz (applied {}-{})",
                        autotune_config.min_freq_mhz,
                        autotune_config.max_freq_mhz,
                        AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ,
                        AM2_FREQ_ONLY_APPLIED_CEIL_MHZ
                    ),
                    voltage_optimization = autotune_config.voltage_optimization,
                    dvfs_enabled = autotune_config.dvfs_enabled,
                    nominal_mhz,
                    "am2/BM1362 FREQUENCY-ONLY autotuner ENABLED (opted in) — \
                     QUIET home objective, NO live voltage, chip-count-aware NPS, \
                     broadcast-PLL via the proven serial init primitive"
                );
                // CE-011: snapshot the Phase-0s energize-gate SKU bindings so the
                // freq-only tuner can register a CEILING-ONLY per-SKU PVT clamp.
                // Empty (unresolved / accept_degraded / mixed-SKU) => no
                // registration => byte-identical to the proven `a lab unit`/`a lab unit` path.
                let bindings_snapshot = self.accepted_sku_bindings.clone();
                tokio::spawn(async move {
                    let mut tuner = dcentrald_autotuner::AutoTuner::new(
                        autotune_config,
                        nominal_mhz,
                        "BM1362".to_string(),
                        "dspic".to_string(),
                        power_calibration,
                    );
                    // CE-011: register the uniform BM1362 SKU (if any) so
                    // `AutoTuner::run` tightens the frequency CEILING to that
                    // SKU's PVT envelope max. `uniform_bm1362_sku_for_bindings`
                    // returns Some ONLY for a non-empty, all-same, PVT-bearing
                    // BM1362 set; otherwise None => no `set_chain_sku` => today's
                    // behavior. Synthetic chain_id 0 (the am2 tuner runs one
                    // synthetic chain). The applied am2 ceiling (545) is <= every
                    // BM1362 envelope max, so this is a deliberate no-op there.
                    if let Some(sku) =
                        dcentrald_autotuner::pvt_envelope::uniform_bm1362_sku_for_bindings(
                            &bindings_snapshot,
                        )
                    {
                        tuner.set_chain_sku(0, sku);
                    }
                    // Readiness gate: wait for the serial path to be
                    // producing nonces before characterizing. A fixed
                    // sleep is not enough — the first snapshots after
                    // chain bring-up can be empty.
                    let mut ready_tick = tokio::time::interval(std::time::Duration::from_secs(5));
                    ready_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    // The tuner's own readiness is driven by the stats
                    // it receives; just give the chain a brief settle
                    // before starting so the first measurement window
                    // isn't dominated by enumeration transients.
                    tokio::select! {
                        _ = tuner_shutdown.cancelled() => {
                            info!("am2 freq-only autotuner stopping before characterization started");
                            return;
                        }
                        _ = ready_tick.tick() => {}
                    }
                    info!("am2 freq-only autotuner: beginning chain-level frequency characterization (Efficiency walk-down)");
                    tuner
                        .run(&chain_infos, stats_rx, fc_tx, tuner_shutdown)
                        .await;
                });
            }
        }

        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(10));
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(5));
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));
        let mut thermal_timer = tokio::time::interval(Duration::from_millis(
            thermal_poll_ms.max(AM2_THERMAL_MIN_POLL_MS),
        ));

        // ---- AT-3: gated, default-OFF, READ-ONLY quiet-window 0x3A rail read ----
        //
        // DEFAULT-OFF. `at3_rail_read_enabled` is false unless the operator set
        // BOTH `[autotuner] at3_rail_read = true` (or `DCENT_AM2_AT3_RAIL_READ=1`)
        // AND opted into the freq autotuner (`freq_autotune_opt_in`). When false
        // the `rail_timer` select! arm below carries the `, if
        // at3_rail_read_enabled` precondition — tokio never constructs/polls it,
        // so the loop is byte-identical to the proven `a lab unit`/`a lab unit` milestone path
        // (exactly the freq-autotuner arm's default-OFF pattern). `rail_timer`
        // itself is constructed unconditionally, mirroring `snapshot_timer`
        // above; an un-ticked interval has no observable effect.
        let at3_rail_read_enabled =
            am2_at3_rail_read_opted_in(&self.config) && freq_autotune_opt_in;
        let at3_interval_s = am2_at3_rail_read_interval_s(&self.config);
        let at3_chain_id = am2_chain_id_for_pic_addr(pic_addr);
        if at3_rail_read_enabled {
            info!(
                interval_s = at3_interval_s,
                pic_addr = format_args!("0x{:02X}", pic_addr),
                chain_id = ?at3_chain_id,
                pic_fw = ?pic_fw,
                "AT-3 quiet-window 0x3A measured-rail read ENABLED (opted in) — \
                 READ-ONLY/measure-only, parser-safe byte-wise framed path \
                 (fw=0x89/0x8A only), feeds AT-1 telemetry tagged `measured`"
            );
        }
        let mut rail_timer = tokio::time::interval(Duration::from_secs(at3_interval_s));
        // Skip the immediate first tick so AT-3 never fires during the first
        // loop turn right after bring-up — let the chain settle, exactly as a
        // ~30 s cadence intends. (tokio intervals fire immediately on the first
        // tick otherwise.)
        rail_timer.reset();

        'mining: loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => { info!("Shutdown requested"); break; }

                // ----  freq-only autotuner: FreqCommand consumer ----
                //
                // The `, if freq_cmd_rx.is_some()` precondition DISABLES
                // this branch entirely when the gate is closed — tokio
                // never even constructs/polls the future, so the select!
                // is STRICTLY byte-identical to the proven path (zero
                // extra wakeups, zero behavior change). The inner
                // `future::pending()` is a belt-and-braces fallback the
                // precondition makes unreachable. When opted in, the
                // autotuner's chain-level frequency suggestions arrive
                // here and are applied via the proven broadcast-PLL
                // primitive. Voltage commands are HARD-REFUSED (last-line
                // defense-in-depth — the pin + the BM1362 capability
                // profile already prevent them).
                maybe_cmd = async {
                    match freq_cmd_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if freq_cmd_rx.is_some() => {
                    if let Some(cmd) = maybe_cmd {
                        use dcentrald_autotuner::FreqCommand as FC;
                        match cmd {
                            FC::SetChainFreq { freq_mhz, ack_tx, .. } => {
                                // Chain-wide broadcast PLL on the serial
                                // path. ack is Result<(), String>.
                                let result = apply_am2_bm1362_chain_pll(&serial, freq_mhz)
                                    .map(|_applied| ())
                                    .map_err(|e| e.to_string());
                                if let Some(tx) = ack_tx {
                                    let _ = tx.send(result);
                                }
                            }
                            FC::SetChipFreq { freq_mhz, ack_tx, .. } => {
                                // No per-chip addressing on the 11-byte
                                // serial nonce wire — a per-chip request
                                // collapses to the same chain-wide
                                // broadcast PLL. ack is Result<u16,
                                // String> (the applied freq).
                                let result = apply_am2_bm1362_chain_pll(&serial, freq_mhz)
                                    .map_err(|e| e.to_string());
                                if let Some(tx) = ack_tx {
                                    let _ = tx.send(result);
                                }
                            }
                            FC::SetVoltage { ack_tx, .. } => {
                                error!(
                                    "am2 freq-only autotuner: SetVoltage REFUSED — \
                                     NO live am2 voltage write this wave (last-line \
                                     defense-in-depth; the freq-only pin should have \
                                     prevented this command being emitted at all)"
                                );
                                if let Some(tx) = ack_tx {
                                    let _ = tx.send(Err(
                                        "am2 freq-only: voltage writes are hard-refused"
                                            .to_string(),
                                    ));
                                }
                            }
                            FC::VerifyVoltage { ack_tx, .. } => {
                                warn!(
                                    "am2 freq-only autotuner: VerifyVoltage ignored — \
                                     voltage is never written on this path"
                                );
                                if let Some(tx) = ack_tx {
                                    let _ = tx.send(Ok(None));
                                }
                            }
                            FC::Barrier { ack_tx } => {
                                let _ = ack_tx.send(());
                            }
                            FC::BeginMeasurement { ack_tx, .. } => {
                                // Reset the chain window and hand back a
                                // fresh epoch so the tuner measures from
                                // a clean baseline after a freq change.
                                let epoch = chain_stats.as_mut().map(|cs| {
                                    cs.epoch = cs.epoch.saturating_add(1);
                                    cs.nonces = 0;
                                    cs.errors = 0;
                                    cs.window_start = Instant::now();
                                    cs.epoch
                                });
                                let _ = ack_tx.send(epoch);
                            }
                            // UpdateWorkTime is an FPGA-FIFO concept; the
                            // serial path has no WORK_TIME register. The
                            // remaining limit/quiet-window commands are
                            // freq-ceiling bookkeeping the chain-level
                            // path doesn't need to apply directly — ack
                            // them so the tuner doesn't stall on a barrier.
                            FC::UpdateWorkTime { .. } => {}
                            FC::SetFrequencyLimit { ack_tx, .. }
                            | FC::SetChipFrequencyLimit { ack_tx, .. } => {
                                if let Some(tx) = ack_tx {
                                    let _ = tx.send(Ok(()));
                                }
                            }
                            FC::PrepareI2cQuietWindow { ack_tx } => {
                                let _ = ack_tx.send(Ok(()));
                            }
                        }
                    }
                    // `maybe_cmd == None` ⇒ the tuner task ended (only
                    // on the shared `self.shutdown` token). We do NOT
                    // reassign `freq_cmd_rx` here (that would conflict
                    // with the borrow the select branch holds — same
                    // proven pattern as `work_dispatcher.rs:2804`).
                    // The shared shutdown token means the next loop
                    // iteration's shutdown arm wins; mining holds the
                    // last-applied frequency until then.
                }

                // ----  freq-only autotuner: chain snapshot feed ----
                //
                // The `, if chain_stats.is_some()` precondition DISABLES
                // this branch entirely when the gate is closed — the
                // snapshot timer is never even polled, so the default-OFF
                // path has ZERO extra wakeups (strictly byte-identical to
                // the proven milestone path). When opted in, fires every
                // `measurement_window_s` carrying the LIVE enumerated
                // chip count so the tuner's expected-NPS math scales
                // 28..126. A mid-run chip-count change vs the
                // start-of-run count is a StepUpGate-blocking event:
                // expected NPS changes, so we DON'T trust a stale N-chip
                // frequency — log it and let the tuner re-measure from
                // the new epoch (the tuner's own resume/state path is
                // additionally fingerprint+chip-count gated).
                _ = snapshot_timer.tick(), if chain_stats.is_some() => {
                    if let Some(cs) = chain_stats.as_mut() {
                        let live_chip_count = chip_count;
                        if live_chip_count != autotune_chip_count_at_start
                            && !autotune_chip_count_warned
                        {
                            autotune_chip_count_warned = true;
                            warn!(
                                start_chip_count = autotune_chip_count_at_start,
                                live_chip_count,
                                "am2 freq-only autotuner: enumerated chip count changed \
                                 mid-run — expected NPS recomputed; the tuner re-measures \
                                 from a fresh epoch (no stale N-chip frequency trusted)"
                            );
                        }
                        let snapshot = cs.take_snapshot(live_chip_count);
                        if let Some(tx) = autotune_stats_sender.as_ref() {
                            if tx.try_send(snapshot).is_err() {
                                // Tuner busy/backpressured — drop this
                                // window (next one carries fresh counts).
                                debug!("am2 freq-only autotuner: stats channel full, dropping one window");
                            }
                        }
                    }
                }

                Some(job) = job_rx.recv() => {
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "NEW BLOCK — flushing serial work history");
                        work_history.iter_mut().for_each(VecDeque::clear);
                        work_builder.reset_extranonce2();
                        seen_shares.clear();
                    }
                    if job.is_flush_only() {
                        info!(job_id = %job.job_id, "Pool switch flush; serial dispatch paused until next notify");
                        current_job = None;
                        continue;
                    }
                    // OBSOLETE rejection removed (commit follow-up to 2b6d46f3,
                    // 2026-05-15 cross-platform Protocol fix sweep): the AM2
                    // serial-dispatch RX path now reconstructs the rolled
                    // version via the shared `bip320_reconstruct_rolled_version`
                    // helper and submits the share with both `version_bits` and
                    // the rolled `version` populated. BM1362 chips roll BIP320
                    // unconditionally regardless of `mining.configure`, so
                    // refusing version-rolling jobs at acceptance would leave
                    // ASICBoost pool revenue on the table. validate_full_header
                    // is the SOLE gate; pools that understand BIP320 accept the
                    // submitted shares (Public Pool .109 milestone confirmed).
                    //
                    // KEEP `work_builder.set_version_mask(0)` — work_builder
                    // does HOST-SIDE rolling when `version_mask != 0` (see
                    // `dcentrald_stratum::work::WorkBuilder::next_work` line
                    // 148-151). On the serial-dispatch path the CHIP rolls
                    // BIP320 internally; if work_builder also rolled,
                    // dispatched work would carry an already-rolled version
                    // and the chip would re-roll on top, producing a
                    // double-rolled header that doesn't match what the chip
                    // actually hashed. Host-side rolling is mutually
                    // exclusive with chip-side rolling on this path.
                    work_builder.set_version_mask(0);
                    current_job = Some(job);
                }

                _ = dispatch_timer.tick() => {
                    if let Some(ref job) = current_job {
                        let work = work_builder.next_work(job);
                        let frame = build_am2_serial_work_frame(&work, asic_job_id);
                        if let Err(e) = serial.write_raw_bytes(&frame) {
                            warn!(error = %e, "AM2 serial-dispatch: chain UART write failed");
                            continue;
                        }
                        let slot = am2_serial_echoed_job_id(asic_job_id) as usize;
                        let history = &mut work_history[slot];
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
                            version_bits_per_midstate: vec![None],
                            version_rolling_enabled: false,
                        });
                        asic_job_id = asic_job_id.wrapping_add(AM2_SERIAL_JOB_ID_STEP);
                        total_work += 1;
                        if first_work_at.is_none() {
                            first_work_at = Some(Instant::now());
                        }
                        if total_work <= 3 {
                            info!(
                                slot,
                                pool_job = %work.job_id,
                                "AM2 serial WORK #{} sent (88-byte BM1362 frame over chain UART)",
                                total_work
                            );
                        }
                    }
                }

                _ = nonce_poll_timer.tick() => {
                    let n = match serial.read_raw_bytes_timeout(&mut rxbuf, 2) {
                        Ok(n) => n,
                        Err(e) => {
                            warn!(error = %e, "AM2 serial-dispatch: chain UART read failed");
                            0
                        }
                    };
                    if n > 0 {
                        carry.extend_from_slice(&rxbuf[..n]);
                    }
                    // Resync to the 0xAA 0x55 nonce-frame preamble and drain
                    // every complete 11-byte frame currently buffered.
                    loop {
                        let mut start = None;
                        for i in 0..carry.len().saturating_sub(1) {
                            if carry[i] == 0xAA && carry[i + 1] == 0x55 {
                                start = Some(i);
                                break;
                            }
                        }
                        let Some(start) = start else {
                            // No preamble in the buffer — keep only the last
                            // byte (a possible split 0xAA) and wait for more.
                            if carry.len() > 1 {
                                let tail = carry.split_off(carry.len() - 1);
                                carry = tail;
                            }
                            break;
                        };
                        if start > 0 {
                            carry.drain(0..start);
                        }
                        if carry.len() < AM2_SERIAL_NONCE_LEN {
                            break; // incomplete frame — wait for more bytes
                        }
                        let frame: Vec<u8> = carry.drain(0..AM2_SERIAL_NONCE_LEN).collect();
                        total_rx_frames += 1;
                        // DCENT_AM2_LOG_RX_FRAMES — lab diagnostic. Hex-dump
                        // the first N raw frames + report parse status BEFORE
                        // the parse-and-continue chain consumes the unparsed
                        // ones via `continue`. Touches no control flow.
                        if rx_frames_logged < log_rx_max {
                            let parse_attempt = parse_bm1362_serial_nonce(&frame);
                            let hex: String = frame
                                .iter()
                                .map(|b| format!("{:02X}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            match &parse_attempt {
                                Some(nr) => {
                                    let job_response = nr.flags & 0x80 != 0;
                                    info!(
                                        rx_index = total_rx_frames,
                                        len = frame.len(),
                                        bytes = %hex,
                                        parsed = true,
                                        job_response,
                                        nonce = format_args!("0x{:08X}", nr.nonce),
                                        job_id = nr.job_id,
                                        small_core = nr.small_core,
                                        midstate_idx = nr.midstate_idx,
                                        version_bits_raw = format_args!(
                                            "0x{:04X}",
                                            nr.version_bits_raw
                                        ),
                                        flags = format_args!("0x{:02X}", nr.flags),
                                        "AM2 RX frame (lab dump)"
                                    );
                                }
                                None => {
                                    info!(
                                        rx_index = total_rx_frames,
                                        len = frame.len(),
                                        bytes = %hex,
                                        parsed = false,
                                        "AM2 RX frame (lab dump) — failed to parse as .79 11-byte BM1362 serial nonce"
                                    );
                                }
                            }
                            rx_frames_logged += 1;
                        }
                        let Some(nr) = parse_bm1362_serial_nonce(&frame) else {
                            // A full-length frame that failed to parse is
                            // a corrupt/HW-error frame — feed it to the
                            // freq-only tuner's error signal so an
                            // over-clocked chain backs DOWN (Efficiency
                            // already walks down; this just accelerates
                            // it under HW stress). No-op when gate closed.
                            crc_errors = crc_errors.saturating_add(1);
                            if let Some(cs) = chain_stats.as_mut() {
                                cs.record_error();
                            }
                            continue;
                        };
                        if nr.flags & 0x80 == 0 {
                            continue; // not a job-response frame
                        }
                        // BIP320 / BIP310 ASICBoost version-rolling reconstruction.
                        // BM1362 rolls the shared BIP320 field regardless of pool
                        // negotiation; keep this path wired to the canonical BM1362
                        // helper so AM2 and BB/Amlogic serial miners cannot drift.
                        let (_, vbits_delta) =
                            am2_hybrid_reconstruct_rolled_version(0, nr.version_bits_raw);
                        total_nonces += 1;
                        hr_nonces += 1;
                        // MINE-LIFE-1: mark liveness — the mid-run stall guard
                        // measures the gap since this instant on each hashrate tick.
                        last_nonce_at = Some(Instant::now());
                        //  freq-only autotuner: aggregate this
                        // chain's valid nonces (chain-level — the serial
                        // nonce frame carries no chip address). No-op
                        // when the gate is closed (`chain_stats` None).
                        if let Some(cs) = chain_stats.as_mut() {
                            cs.record_nonce();
                        }
                        if total_nonces <= 1000 {
                            info!(
                                nonce = format_args!("0x{:08X}", nr.nonce),
                                job_id = nr.job_id,
                                small_core = nr.small_core,
                                vbits_delta = format_args!("0x{:08X}", vbits_delta),
                                "AM2 serial nonce #{}",
                                total_nonces
                            );
                        }
                        let history = &work_history[nr.job_id as usize];
                        if history.is_empty() {
                            continue;
                        }
                        if !seen_shares.insert((nr.job_id, nr.nonce, nr.version_bits_raw)) {
                            continue; // duplicate (same job + nonce + rolled version)
                        }
                        // AT-DASH: dedup-survivor — a genuinely distinct nonce.
                        // `total_nonces - unique_nonces` is the duplicate spam,
                        // the first number needed to crack the ~400x gap.
                        unique_nonces = unique_nonces.saturating_add(1);
                        if dcentrald_common::should_clear_seen_shares(
                            seen_shares.len(),
                            dcentrald_common::DEFAULT_SEEN_SHARES_CAP,
                        ) {
                            seen_shares.clear();
                        }
                        if let Some((entry, rolled_version, achieved_difficulty)) =
                            history.iter().rev().find_map(|candidate| {
                                let (rolled_version, candidate_vbits_delta) =
                                    am2_hybrid_reconstruct_rolled_version(
                                        candidate.version,
                                        nr.version_bits_raw,
                                    );
                                debug_assert_eq!(candidate_vbits_delta, vbits_delta);
                                let header = hybrid_build_header(
                                    candidate,
                                    rolled_version,
                                    nr.nonce,
                                );
                                if dcentrald_stratum::share_pipeline::validate_full_header(
                                    &header,
                                    &candidate.share_target,
                                ) {
                                    let achieved =
                                        am2_hybrid_achieved_difficulty_from_header(&header);
                                    Some((candidate.clone(), rolled_version, achieved))
                                } else {
                                    None
                                }
                            })
                        {
                            shares_submitted += 1;
                            let share = dcentrald_stratum::types::ValidShare {
                                worker_name: self.config.pool.worker.clone(),
                                job_id: entry.job_id.clone(),
                                extranonce2: entry.extranonce2.clone(),
                                ntime: format!("{:08x}", entry.ntime),
                                nonce: format!("{:08x}", nr.nonce),
                                version_bits: if vbits_delta != 0 {
                                    Some(format!("{:08x}", vbits_delta))
                                } else {
                                    None
                                },
                                version: rolled_version,
                                achieved_difficulty,
                            };
                            match share_tx.send(share).await {
                                Ok(()) => info!(
                                    nonce = format_args!("0x{:08X}", nr.nonce),
                                    "AM2 serial SHARE #{}", shares_submitted
                                ),
                                Err(e) => {
                                    error!(error = %e, "Share channel closed");
                                    break 'mining;
                                }
                            }
                        }
                    }
                }

                _ = thermal_timer.tick() => {
                    watchdog_liveness.fetch_add(1, Ordering::Relaxed);
                    if let Some(sup) = thermal_supervisor.as_mut() {
                        match sup.poll_and_check("runtime") {
                            Err(e) => {
                                error!(error = %e, "AM2 serial-dispatch runtime thermal supervisor tripped — forcing home hard-stop");
                                force_am2_thermal_hard_stop(&self.config, "serial-dispatch-runtime-thermal");
                                self.shutdown.cancel();
                                break;
                            }
                            Ok(temp_c) => {
                                // THERM-1(a): fan-fault fail-closed (cut hash;
                                // fans NEVER raised). Inert on a healthy unit —
                                // only a sustained "PWM>0 but 0 RPM" trips it.
                                if sup.poll_fan_fault(temp_c) {
                                    error!(
                                        temp_c,
                                        hot_temp_c = sup.hot_temp_c,
                                        strikes = AM2_FAN_FAULT_STRIKES,
                                        "AM2 serial-dispatch FAN FAULT (commanded PWM>0 but 0 RPM across all fans while at/above the hot threshold for 3 consecutive polls) — cutting hash POWER first, then holding fans at the configured home cap (<=30 PWM, NOT a blast)"
                                    );
                                    force_am2_thermal_hard_stop(
                                        &self.config,
                                        "serial-dispatch-fan-fault",
                                    );
                                    self.shutdown.cancel();
                                    break;
                                }
                                // THERM-1(b): CONSERVATIVE graded freq throttle
                                // between hot and dangerous (opt-in; cut hash
                                // before noise — fans NEVER raised). Floor-guarded
                                // so the eco ~50 MHz path is inert; only fires when
                                // opted in AND temp ≥ hot AND current freq > floor.
                                if am2_thermal_graded_throttle_enabled() {
                                    let current_mhz =
                                        am2_current_effective_freq_mhz(&self.config);
                                    if let Some(target) = am2_graded_throttle_target_mhz(
                                        current_mhz,
                                        temp_c,
                                        sup.hot_temp_c,
                                        AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ,
                                        AM2_THERMAL_THROTTLE_STEP_MHZ,
                                    ) {
                                        match apply_am2_bm1362_chain_pll(&serial, target) {
                                            Ok(applied) => {
                                                AM2_RE018_APPLIED_FREQ_MHZ
                                                    .store(applied, Ordering::Relaxed);
                                                warn!(
                                                    temp_c,
                                                    hot_temp_c = sup.hot_temp_c,
                                                    from_mhz = current_mhz,
                                                    to_mhz = applied,
                                                    "AM2 graded thermal throttle: reduced chain frequency (cut-hash-before-noise; fans NOT raised)"
                                                );
                                            }
                                            Err(e) => warn!(
                                                error = %e,
                                                "AM2 graded thermal throttle: PLL write failed"
                                            ),
                                        }
                                    }
                                }
                                // Informational only — the supervisor
                                // above remains the thermal SAFETY
                                // authority. Feed the freq-only tuner's
                                // thermal-refinement input (no-op when
                                // the gate is closed). The Err arm above
                                // is byte-identical to the proven path.
                                if let Some(cs) = chain_stats.as_mut() {
                                    cs.set_temp(Some(temp_c));
                                }
                            }
                        }
                    }
                }

                // ---- AT-3: gated, default-OFF, READ-ONLY quiet-window 0x3A read ----
                //
                // The `, if at3_rail_read_enabled` precondition DISABLES this
                // branch entirely when the gate is closed — tokio never
                // constructs/polls the future, so the select! is STRICTLY
                // byte-identical to the proven `a lab unit`/`a lab unit` milestone path
                // (zero extra wakeups, zero behavior change), exactly like the
                // freq-autotuner arm above. When opted in (operator set
                // `DCENT_AM2_AT3_RAIL_READ=1`/TOML AND opted into the autotuner),
                // read the dsPIC 0x3A analog-ADC ACTUAL rail via the parser-safe
                // byte-wise framed path (fw=0x89/0x8A ONLY — `at3_read_measured_rail`
                // refuses bare/unknown firmware so it never reaches the I2C_RDWR
                // fallback), cloning the existing single-owner I²C service handle
                // exactly as the thermal supervisor does, and publish the
                // measured mV into the AT-1 telemetry slot tagged `measured`.
                //
                // READ-ONLY / measure-only: no SET_VOLTAGE, no ENABLE, no
                // frequency change — AT-3 cannot influence the rail (the closed
                // loop is AT-4+, out of scope). Best-effort: a miss publishes
                // nothing and ages out of the AT-1 slot by TTL (clean degrade to
                // commanded-tagged); no retry on the hot path. The voltage hard
                // cap / fw=0x86 refusal are untouched because AT-3 writes no
                // voltage at all.
                _ = rail_timer.tick(), if at3_rail_read_enabled => {
                    if let (Some(svc), Some(chain_id)) = (i2c.as_ref(), at3_chain_id) {
                        if let Some(mv) = at3_read_measured_rail(svc, pic_addr, pic_fw) {
                            // fw=0x8A 0x3A ADC scale is not yet live-verified
                            // (RE-ASK-DSPIC-3A-FW8A-SCALE) — tag it advisory.
                            // It still flows to telemetry as `measured`, but the
                            // flag travels with it so no future consumer uses an
                            // fw=0x8A reading for a control decision.
                            let fw8a_scale_unverified = matches!(
                                pic_fw.map(dcentrald_asic::dspic::DspicFirmware::from_version),
                                Some(dcentrald_asic::dspic::DspicFirmware::Fw8A)
                            );
                            dcentrald_common::at3_rail::publish(
                                chain_id,
                                mv,
                                fw8a_scale_unverified,
                            );
                            debug!(
                                chain_id,
                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                measured_mv = mv,
                                fw8a_scale_unverified,
                                "AT-3 measured rail published (advisory; telemetry-only, never used for control)"
                            );
                        }
                    }
                }

                _ = hashrate_timer.tick() => {
                    // AT-DASH: real hashrate estimate from accepted-share ACHIEVED
                    // difficulty over the elapsed window (NOT a flat diff-256 nonce
                    // count). `hashrate_ghs` is cumulative-since-loop-start;
                    // `hashrate_5s_ghs` is the just-elapsed window. Both are the
                    // pool-credited truth — the diagnostic to separate dup-spam
                    // (total_nonces ≫ unique_nonces) from real low hashing.
                    let accepted = share_accounting.accepted();
                    let rejected = share_accounting.rejected();
                    let hashrate_ghs =
                        share_accounting.hashrate_ghs_since(loop_started);
                    // MINE-LIFE-2 (eco-stable): the "current"/5 s tile is a REAL
                    // recent-window estimate — achieved-difficulty accrued over a
                    // WIDE rolling window (~AM2_RECENT_HASHRATE_WINDOW_S of PRIOR
                    // snapshots), NOT the cumulative lifetime average (presenting
                    // cumulative as "current" keeps showing a healthy number after
                    // a stall). FIX 2: the wide window keeps a sparse-share eco
                    // unit's tile a STABLE non-zero value instead of flickering to
                    // 0 on the empty ticks between shares; the first tick (no prior
                    // history) honestly shows the cumulative figure exactly once.
                    let now_tick = Instant::now();
                    let cur_achieved_sum = share_accounting.achieved_difficulty_sum();
                    let hr_history: Vec<(f64, f64)> = hr_window
                        .iter()
                        .map(|&(at, sum)| (now_tick.duration_since(at).as_secs_f64(), sum))
                        .collect();
                    let hashrate_5s_ghs =
                        match am2_rolling_window_baseline(&hr_history, AM2_RECENT_HASHRATE_WINDOW_S) {
                            Some((span_s, baseline_sum)) => {
                                share_accounting.hashrate_ghs_window(baseline_sum, span_s)
                            }
                            None => hashrate_ghs,
                        };
                    // Record this tick + prune to the rolling horizon (drop a
                    // front snapshot only while the next one is still old enough
                    // to span the window, so the baseline stays correct).
                    hr_window.push_back((now_tick, cur_achieved_sum));
                    while hr_window.len() >= 2
                        && now_tick.duration_since(hr_window[1].0).as_secs_f64()
                            >= AM2_RECENT_HASHRATE_WINDOW_S
                    {
                        hr_window.pop_front();
                    }
                    // MINE-LIFE-1/2: recent-activity + mid-run stall decision off
                    // the time since the last nonce.
                    let last_nonce_elapsed = last_nonce_at.map(|t| t.elapsed());
                    let recently_active =
                        am2_nonce_recently_active(mid_run_stall_timeout, last_nonce_elapsed);
                    let mid_run_stalled =
                        am2_mid_run_nonce_stalled(mid_run_stall_timeout, last_nonce_elapsed);
                    let dup_nonces = total_nonces.saturating_sub(unique_nonces);
                    let uptime_s = loop_started.elapsed().as_secs();
                    info!(
                        total_work,
                        total_rx_frames,
                        total_nonces,
                        unique_nonces,
                        dup_nonces,
                        crc_errors,
                        shares_submitted,
                        accepted,
                        rejected,
                        nonces_5s = hr_nonces,
                        hashrate_ghs,
                        // Per-dsPIC attribution: this single-chain loop drives ONE
                        // dsPIC, so all nonces attribute to `pic_addr`'s chain.
                        dspic_addr = format_args!("0x{:02X}", pic_addr),
                        chain_id = ?am2_chain_id_for_pic_addr(pic_addr),
                        "am2_serial_status"
                    );
                    // Publish a live MinerState so /api/status + the dashboard show
                    // real hashrate / per-chain state / shares. Fail-closed no-op
                    // when no publisher is attached or all receivers are gone.
                    let chain_state = self.build_am2_chain_state(
                        pic_addr,
                        chip_count,
                        hashrate_ghs,
                        unique_nonces,
                        crc_errors,
                        recently_active,
                    );
                    let pool_quality_snapshot = match pool_quality.read() {
                        Ok(quality) => quality.clone(),
                        Err(_) => {
                            warn!(
                                "Pool-quality snapshot lock poisoned; publishing honest defaults"
                            );
                            dcentrald_stratum::pool_quality::PoolQualitySnapshot::default()
                        }
                    };
                    self.publish_miner_state(
                        &share_accounting,
                        &pool_quality_snapshot,
                        hashrate_ghs,
                        hashrate_5s_ghs,
                        vec![chain_state],
                        uptime_s,
                        recently_active,
                    );
                    hr_nonces = 0;
                    if let (Some(timeout), Some(first_work)) = (no_nonce_timeout, first_work_at) {
                        if total_nonces == 0 && first_work.elapsed() >= timeout {
                            error!(
                                work_sent = total_work,
                                rx_frames = total_rx_frames,
                                timeout_s = timeout.as_secs(),
                                "AM2 serial-dispatch no-nonce stall — forcing home hard-stop"
                            );
                            force_am2_home_hard_stop(&self.config, "serial-dispatch-no-nonce-stall");
                            self.shutdown.cancel();
                            break;
                        }
                    }
                    // MINE-LIFE-1: GENEROUS mid-run nonce-stall fail-closed.
                    // Trips only AFTER nonces have flowed (last_nonce_at is Some)
                    // and then stop for the generous window — auto-restart is
                    // unsafe on am2, so the recovery is the SAME safe teardown the
                    // no-nonce path uses (force_am2_home_hard_stop + shutdown).
                    if mid_run_stalled {
                        error!(
                            work_sent = total_work,
                            rx_frames = total_rx_frames,
                            total_nonces,
                            last_nonce_age_s =
                                last_nonce_elapsed.map(|e| e.as_secs()).unwrap_or(0),
                            timeout_s =
                                mid_run_stall_timeout.map(|t| t.as_secs()).unwrap_or(0),
                            "AM2 serial-dispatch MID-RUN nonce stall (nonces flowed then stopped) — forcing home hard-stop"
                        );
                        force_am2_home_hard_stop(
                            &self.config,
                            "serial-dispatch-mid-run-stall",
                        );
                        self.shutdown.cancel();
                        break;
                    }
                }
            }
        }

        // The run-scope owner performs dsPIC/PSU teardown only after it has
        // cancelled and joined both heartbeat feeders. Keeping controller I/O
        // out of this inner loop prevents a late heartbeat racing after disable.

        info!(
            total_work,
            total_rx_frames,
            total_nonces,
            shares_submitted,
            "AM2 serial-work-dispatch loop stopped"
        );
        Ok(())
    }

    /// DUAL-CHAIN serial-work-dispatch loop (`DCENT_AM2_DUAL_CHAIN_TTYS3=1`,
    /// GROUP B / W8 parity).
    ///
    /// Drives TWO `SerialChainBackend`s in lockstep — the proven primary chain
    /// plus a second chain on `/dev/ttyS3` (dsPIC 0x22) — so `a lab unit` mines BOTH
    /// hashboards instead of chain 1 only (~2× the hashrate). Each chain owns an
    /// independent `Am2SerialChainState` (per-chain `WorkEntry` history, dedup,
    /// rolling job id, RX carry) so nonces are attributed to the chain that
    /// produced them and BIP320 share reconstruction stays per-chain-correct.
    ///
    /// Every pool job is broadcast to both chains; both chains dispatch on the
    /// same dispatch tick and are polled on the same nonce tick. The thermal
    /// supervisor and the no-nonce fail-closed guard are SHARED (the guard trips
    /// only when NEITHER chain has produced a nonce within the window — a true
    /// whole-unit stall). The freq-only autotuner is intentionally NOT wired
    /// here (it is single-synthetic-chain by design; dual-chain autotune is a
    /// separate follow-up). The run-scope owner disables voltage on BOTH
    /// chains' dsPICs after heartbeat feeders are joined.
    ///
    /// SAFETY: this is reached ONLY behind `am2_dual_chain_ttys3_enabled()` AND
    /// only after the second chain successfully enumerated in `run()`. The
    /// single-chain `run_am2_serial_dispatch_loop` above is untouched, so the
    /// gate-OFF path is byte-identical.
    #[allow(clippy::too_many_arguments)]
    async fn run_am2_dual_chain_serial_dispatch_loop(
        &self,
        serial_a: SerialChainBackend,
        serial_b: SerialChainBackend,
        mut job_rx: mpsc::Receiver<dcentrald_stratum::types::JobTemplate>,
        share_tx: mpsc::Sender<dcentrald_stratum::types::ValidShare>,
        chip_count: u8,
        _i2c: Option<I2cServiceHandle>,
        pic_addr_a: u8,
        pic_addr_b: u8,
        _pic_fw: Option<u8>,
        mut thermal_supervisor: Option<Am2ThermalSupervisor>,
        thermal_poll_ms: u64,
        watchdog_liveness: Arc<AtomicU64>,
        // AT-DASH: shared pool-share accounting (written by the status task),
        // read here on each hashrate tick to compute the real GH/s estimate and
        // publish a live `MinerState`.
        share_accounting: Arc<Am2ShareAccounting>,
        pool_quality: Arc<RwLock<dcentrald_stratum::pool_quality::PoolQualitySnapshot>>,
    ) -> Result<()> {
        info!(
            chip_count,
            pic_addr_a = format_args!("0x{:02X}", pic_addr_a),
            pic_addr_b = format_args!("0x{:02X}", pic_addr_b),
            "=== AM2 DUAL-CHAIN SERIAL-WORK-DISPATCH MINING ACTIVE (BM1362 88-byte serial frames over TWO chain UARTs; FPGA WORK_TX FIFO bypassed) ==="
        );

        let mut chain_a = Am2SerialChainState::new(0);
        let mut chain_b = Am2SerialChainState::new(1);
        let worker = self.config.pool.worker.clone();
        // AT-DASH: loop-start anchor for the cumulative achieved-difficulty GH/s
        // estimate (pool-credited hashrate truth, summed across BOTH chains).
        let loop_started = Instant::now();

        let mut rxbuf_a = [0u8; 512];
        let mut rxbuf_b = [0u8; 512];
        // Aggregate error counter fed to nothing today (no dual-chain tuner) —
        // kept so the pure `ingest_rx` signature is identical to the
        // host-tested contract; a HW-error frame still advances total_rx_frames.
        let mut errors_sink: u64 = 0;

        let no_nonce_timeout = if self.config.mining.am2_no_nonce_timeout_s == 0 {
            None
        } else {
            Some(Duration::from_secs(
                self.config.mining.am2_no_nonce_timeout_s,
            ))
        };
        if let Some(t) = no_nonce_timeout {
            info!(
                timeout_s = t.as_secs(),
                "AM2 dual-chain no-nonce fail-closed guard armed (trips only if NEITHER chain produces a nonce)"
            );
        } else {
            warn!("AM2 dual-chain no-nonce fail-closed guard disabled by config");
        }
        let mut first_work_at: Option<Instant> = None;

        // MINE-LIFE-1: GENEROUS mid-run nonce-stall guard (per-chain liveness +
        // a unit-level fail-closed that trips only when NEITHER chain has
        // produced a nonce within the generous window).
        let mid_run_stall_timeout =
            am2_mid_run_nonce_stall_timeout(self.config.mining.am2_no_nonce_timeout_s);
        match mid_run_stall_timeout {
            Some(t) => info!(
                timeout_s = t.as_secs(),
                "AM2 dual-chain mid-run nonce-stall fail-closed guard armed (trips only if NEITHER chain stays active)"
            ),
            None => info!("AM2 dual-chain mid-run nonce-stall guard disabled"),
        }
        // Per-chain liveness, sampled by nonce-count delta on each hashrate tick.
        let mut chain_a_last_nonce_at: Option<Instant> = None;
        let mut chain_b_last_nonce_at: Option<Instant> = None;
        let mut prev_a_nonces: u64 = 0;
        let mut prev_b_nonces: u64 = 0;
        // MINE-LIFE-2 (eco-stable): rolling history of recent hashrate-tick
        // snapshots `(Instant, achieved_difficulty_sum)` (oldest first), pruned
        // to ~AM2_RECENT_HASHRATE_WINDOW_S — same wide-window "current" hashrate
        // as the single-chain loop so a sparse-share eco unit shows a STABLE
        // non-zero value instead of flickering to 0 between shares.
        let mut hr_window: VecDeque<(Instant, f64)> = VecDeque::new();

        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(10));
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(5));
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));
        let mut thermal_timer = tokio::time::interval(Duration::from_millis(
            thermal_poll_ms.max(AM2_THERMAL_MIN_POLL_MS),
        ));

        'mining: loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => { info!("Shutdown requested"); break; }

                Some(job) = job_rx.recv() => {
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "NEW BLOCK — flushing both chains' serial work history");
                    }
                    if job.is_flush_only() {
                        info!(job_id = %job.job_id, "Pool switch flush; dual-chain dispatch paused until next notify");
                    }
                    // Both chains track the same pool job stream independently
                    // (independent extranonce2 rolling per chain).
                    chain_a.on_job(job.clone());
                    chain_b.on_job(job);
                }

                _ = dispatch_timer.tick() => {
                    if let Some(frame) = chain_a.next_work_frame() {
                        if let Err(e) = serial_a.write_raw_bytes(&frame) {
                            warn!(error = %e, chain = 0, "AM2 dual-chain: chain-A UART write failed");
                        } else if chain_a.total_work <= 3 {
                            info!(chain = 0, "AM2 dual-chain WORK #{} sent on chain A", chain_a.total_work);
                        }
                    }
                    if let Some(frame) = chain_b.next_work_frame() {
                        if let Err(e) = serial_b.write_raw_bytes(&frame) {
                            warn!(error = %e, chain = 1, "AM2 dual-chain: chain-B UART write failed");
                        } else if chain_b.total_work <= 3 {
                            info!(chain = 1, "AM2 dual-chain WORK #{} sent on chain B", chain_b.total_work);
                        }
                    }
                    if first_work_at.is_none()
                        && (chain_a.first_work_at.is_some() || chain_b.first_work_at.is_some())
                    {
                        first_work_at = Some(Instant::now());
                    }
                }

                _ = nonce_poll_timer.tick() => {
                    // Poll chain A.
                    let n_a = match serial_a.read_raw_bytes_timeout(&mut rxbuf_a, 2) {
                        Ok(n) => n,
                        Err(e) => { warn!(error = %e, chain = 0, "AM2 dual-chain: chain-A UART read failed"); 0 }
                    };
                    let chain_a_id = chain_a.chain_id;
                    for attributed in chain_a.ingest_rx(&rxbuf_a[..n_a], &worker, &mut errors_sink) {
                        match share_tx.send(attributed.share).await {
                            Ok(()) => info!(
                                chain = chain_a_id,
                                nonce = format_args!("0x{:08X}", attributed.nonce),
                                "AM2 dual-chain SHARE (chain A) #{}", chain_a.shares_submitted
                            ),
                            Err(e) => { error!(error = %e, "Share channel closed"); break 'mining; }
                        }
                    }
                    // Poll chain B.
                    let n_b = match serial_b.read_raw_bytes_timeout(&mut rxbuf_b, 2) {
                        Ok(n) => n,
                        Err(e) => { warn!(error = %e, chain = 1, "AM2 dual-chain: chain-B UART read failed"); 0 }
                    };
                    let chain_b_id = chain_b.chain_id;
                    for attributed in chain_b.ingest_rx(&rxbuf_b[..n_b], &worker, &mut errors_sink) {
                        match share_tx.send(attributed.share).await {
                            Ok(()) => info!(
                                chain = chain_b_id,
                                nonce = format_args!("0x{:08X}", attributed.nonce),
                                "AM2 dual-chain SHARE (chain B) #{}", chain_b.shares_submitted
                            ),
                            Err(e) => { error!(error = %e, "Share channel closed"); break 'mining; }
                        }
                    }
                }

                _ = thermal_timer.tick() => {
                    watchdog_liveness.fetch_add(1, Ordering::Relaxed);
                    if let Some(sup) = thermal_supervisor.as_mut() {
                        match sup.poll_and_check("runtime") {
                            Err(e) => {
                                error!(error = %e, "AM2 dual-chain runtime thermal supervisor tripped — forcing home hard-stop");
                                force_am2_thermal_hard_stop(&self.config, "dual-chain-runtime-thermal");
                                self.shutdown.cancel();
                                break;
                            }
                            Ok(temp_c) => {
                                // THERM-1(a): fan-fault fail-closed (cut hash;
                                // fans NEVER raised). Inert on a healthy unit.
                                if sup.poll_fan_fault(temp_c) {
                                    error!(
                                        temp_c,
                                        hot_temp_c = sup.hot_temp_c,
                                        strikes = AM2_FAN_FAULT_STRIKES,
                                        "AM2 dual-chain FAN FAULT (commanded PWM>0 but 0 RPM across all fans while at/above the hot threshold for 3 consecutive polls) — cutting hash POWER first, then holding fans at the configured home cap (<=30 PWM, NOT a blast)"
                                    );
                                    force_am2_thermal_hard_stop(&self.config, "dual-chain-fan-fault");
                                    self.shutdown.cancel();
                                    break;
                                }
                                // THERM-1(b): CONSERVATIVE graded freq throttle on
                                // BOTH chains (opt-in; cut hash before noise — fans
                                // NEVER raised). Floor-guarded so the eco path is inert.
                                if am2_thermal_graded_throttle_enabled() {
                                    let current_mhz =
                                        am2_current_effective_freq_mhz(&self.config);
                                    if let Some(target) = am2_graded_throttle_target_mhz(
                                        current_mhz,
                                        temp_c,
                                        sup.hot_temp_c,
                                        AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ,
                                        AM2_THERMAL_THROTTLE_STEP_MHZ,
                                    ) {
                                        let a = apply_am2_bm1362_chain_pll(&serial_a, target);
                                        let b = apply_am2_bm1362_chain_pll(&serial_b, target);
                                        match (a, b) {
                                            (Ok(applied), _) | (_, Ok(applied)) => {
                                                AM2_RE018_APPLIED_FREQ_MHZ
                                                    .store(applied, Ordering::Relaxed);
                                                warn!(
                                                    temp_c,
                                                    hot_temp_c = sup.hot_temp_c,
                                                    from_mhz = current_mhz,
                                                    to_mhz = applied,
                                                    "AM2 dual-chain graded thermal throttle: reduced both chains' frequency (cut-hash-before-noise; fans NOT raised)"
                                                );
                                            }
                                            (Err(ea), Err(eb)) => warn!(
                                                error_a = %ea,
                                                error_b = %eb,
                                                "AM2 dual-chain graded thermal throttle: PLL write failed on both chains"
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                _ = hashrate_timer.tick() => {
                    // AT-DASH: real cumulative hashrate from accepted-share ACHIEVED
                    // difficulty (pool-credited, both chains combined).
                    let accepted = share_accounting.accepted();
                    let rejected = share_accounting.rejected();
                    let hashrate_ghs = share_accounting.hashrate_ghs_since(loop_started);
                    let unique_total = chain_a.unique_nonces + chain_b.unique_nonces;
                    let dup_total = (chain_a.total_nonces + chain_b.total_nonces)
                        .saturating_sub(unique_total);
                    // MINE-LIFE-1/2: per-chain liveness from each chain's nonce
                    // delta this tick; unit-level stall trips ONLY when NEITHER
                    // chain has produced a nonce within the generous window.
                    let now_tick = Instant::now();
                    if chain_a.total_nonces > prev_a_nonces {
                        chain_a_last_nonce_at = Some(now_tick);
                    }
                    if chain_b.total_nonces > prev_b_nonces {
                        chain_b_last_nonce_at = Some(now_tick);
                    }
                    prev_a_nonces = chain_a.total_nonces;
                    prev_b_nonces = chain_b.total_nonces;
                    let chain_a_active = am2_nonce_recently_active(
                        mid_run_stall_timeout,
                        chain_a_last_nonce_at.map(|t| t.elapsed()),
                    );
                    let chain_b_active = am2_nonce_recently_active(
                        mid_run_stall_timeout,
                        chain_b_last_nonce_at.map(|t| t.elapsed()),
                    );
                    // Unit liveness = the more recent of the two chains
                    // (Option<Instant> orders None < Some, later instant > earlier).
                    let unit_last_nonce_at = chain_a_last_nonce_at.max(chain_b_last_nonce_at);
                    let unit_last_nonce_elapsed = unit_last_nonce_at.map(|t| t.elapsed());
                    let mid_run_stalled =
                        am2_mid_run_nonce_stalled(mid_run_stall_timeout, unit_last_nonce_elapsed);
                    // MINE-LIFE-2 (eco-stable): REAL recent-window "current"
                    // hashrate over a WIDE rolling window (stable non-zero on a
                    // sparse-share eco unit; first tick shows the cumulative
                    // figure exactly once). `now_tick` was taken above.
                    let cur_achieved_sum = share_accounting.achieved_difficulty_sum();
                    let hr_history: Vec<(f64, f64)> = hr_window
                        .iter()
                        .map(|&(at, sum)| (now_tick.duration_since(at).as_secs_f64(), sum))
                        .collect();
                    let hashrate_5s_ghs =
                        match am2_rolling_window_baseline(&hr_history, AM2_RECENT_HASHRATE_WINDOW_S) {
                            Some((span_s, baseline_sum)) => {
                                share_accounting.hashrate_ghs_window(baseline_sum, span_s)
                            }
                            None => hashrate_ghs,
                        };
                    hr_window.push_back((now_tick, cur_achieved_sum));
                    while hr_window.len() >= 2
                        && now_tick.duration_since(hr_window[1].0).as_secs_f64()
                            >= AM2_RECENT_HASHRATE_WINDOW_S
                    {
                        hr_window.pop_front();
                    }
                    info!(
                        total_work = chain_a.total_work + chain_b.total_work,
                        chain_a_work = chain_a.total_work,
                        chain_b_work = chain_b.total_work,
                        total_rx_frames = chain_a.total_rx_frames + chain_b.total_rx_frames,
                        total_nonces = chain_a.total_nonces + chain_b.total_nonces,
                        unique_nonces = unique_total,
                        dup_nonces = dup_total,
                        chain_a_nonces = chain_a.total_nonces,
                        chain_b_nonces = chain_b.total_nonces,
                        chain_a_unique = chain_a.unique_nonces,
                        chain_b_unique = chain_b.unique_nonces,
                        crc_errors = chain_a.crc_errors + chain_b.crc_errors,
                        shares_submitted = chain_a.shares_submitted + chain_b.shares_submitted,
                        accepted,
                        rejected,
                        nonces_5s = chain_a.hr_nonces + chain_b.hr_nonces,
                        hashrate_ghs,
                        dspic_addr_a = format_args!("0x{:02X}", pic_addr_a),
                        dspic_addr_b = format_args!("0x{:02X}", pic_addr_b),
                        "am2_dual_chain_status"
                    );
                    // Per-dsPIC ChainState: split the cumulative hashrate by each
                    // chain's unique-nonce share (honest attribution — both chains
                    // share one pool credit stream so we can't read per-chain pool
                    // difficulty, but unique-nonce ratio is the best proxy).
                    let denom = unique_total.max(1) as f64;
                    let chain_state_a = self.build_am2_chain_state(
                        pic_addr_a,
                        chip_count,
                        hashrate_ghs * (chain_a.unique_nonces as f64 / denom),
                        chain_a.unique_nonces,
                        chain_a.crc_errors,
                        chain_a_active,
                    );
                    let chain_state_b = self.build_am2_chain_state(
                        pic_addr_b,
                        chip_count,
                        hashrate_ghs * (chain_b.unique_nonces as f64 / denom),
                        chain_b.unique_nonces,
                        chain_b.crc_errors,
                        chain_b_active,
                    );
                    let pool_quality_snapshot = match pool_quality.read() {
                        Ok(quality) => quality.clone(),
                        Err(_) => {
                            warn!(
                                "Pool-quality snapshot lock poisoned; publishing honest defaults"
                            );
                            dcentrald_stratum::pool_quality::PoolQualitySnapshot::default()
                        }
                    };
                    self.publish_miner_state(
                        &share_accounting,
                        &pool_quality_snapshot,
                        hashrate_ghs,
                        hashrate_5s_ghs,
                        vec![chain_state_a, chain_state_b],
                        loop_started.elapsed().as_secs(),
                        chain_a_active || chain_b_active,
                    );
                    chain_a.hr_nonces = 0;
                    chain_b.hr_nonces = 0;
                    if let (Some(timeout), Some(first_work)) = (no_nonce_timeout, first_work_at) {
                        let unit_nonces = chain_a.total_nonces + chain_b.total_nonces;
                        if unit_nonces == 0 && first_work.elapsed() >= timeout {
                            error!(
                                work_sent = chain_a.total_work + chain_b.total_work,
                                rx_frames = chain_a.total_rx_frames + chain_b.total_rx_frames,
                                timeout_s = timeout.as_secs(),
                                "AM2 dual-chain no-nonce stall (NEITHER chain produced a nonce) — forcing home hard-stop"
                            );
                            force_am2_home_hard_stop(&self.config, "dual-chain-no-nonce-stall");
                            self.shutdown.cancel();
                            break;
                        }
                    }
                    // MINE-LIFE-1: GENEROUS unit-level mid-run stall fail-closed —
                    // trips only when NEITHER chain has produced a nonce within the
                    // generous window after nonces had flowed (auto-restart is
                    // unsafe on am2, so recovery is the SAME safe teardown).
                    if mid_run_stalled {
                        error!(
                            chain_a_nonces = chain_a.total_nonces,
                            chain_b_nonces = chain_b.total_nonces,
                            last_nonce_age_s =
                                unit_last_nonce_elapsed.map(|e| e.as_secs()).unwrap_or(0),
                            timeout_s =
                                mid_run_stall_timeout.map(|t| t.as_secs()).unwrap_or(0),
                            "AM2 dual-chain MID-RUN nonce stall (NEITHER chain stayed active) — forcing home hard-stop"
                        );
                        force_am2_home_hard_stop(&self.config, "dual-chain-mid-run-stall");
                        self.shutdown.cancel();
                        break;
                    }
                }
            }
        }

        // The run-scope owner disables both dsPICs only after heartbeat feeders
        // are proven quiescent, preventing a post-disable keepalive race.

        info!(
            total_work = chain_a.total_work + chain_b.total_work,
            total_rx_frames = chain_a.total_rx_frames + chain_b.total_rx_frames,
            total_nonces = chain_a.total_nonces + chain_b.total_nonces,
            shares_submitted = chain_a.shares_submitted + chain_b.shares_submitted,
            "AM2 dual-chain serial-work-dispatch loop stopped"
        );
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let _route_admission = self
            .route_admission
            .take()
            .ok_or_else(|| anyhow::anyhow!("AM2 hybrid route admission was already consumed"))?;
        info!("=== S19J PRO HYBRID MINING (Serial init + FPGA work, APW121215a PSU) ===");

        if self.shutdown.is_cancelled() {
            anyhow::bail!("AM2 hybrid run was cancelled before hardware admission");
        }

        //  finding: hybrid mode bypasses `daemon.rs::Daemon::run()` so the
        // daemon-level Phase 2 fan init never runs. We re-anchor a cold-boot
        // origin here for the first-byte timeline trace, comparable to bosminer's
        // `12-bosminer-startup-timeline.md` (T+0 = bosminerd handoff).
        let cold_boot_start = Instant::now();
        let t = || cold_boot_start.elapsed().as_millis();
        info!(
            t_ms = t(),
            "[T+0] cold-boot origin armed (s19j hybrid path)"
        );

        am2_wave56_override_runtime_preflight(&self.config)?;

        // Resolve the complete elevated-rail lifecycle before the first UART,
        // I2C, PSU, or GPIO mutation. This gate depends only on immutable
        // configuration, environment/fingerprint evidence, and passthrough
        // policy, so invalid combinations have no reason to touch hardware.
        let passthrough = self.config.mining.passthrough;
        let all_active_voltage_enable = am2_env_flag(AM2_VOLTAGE_ENABLE_ALL_ACTIVE_PICS_ENV);
        let open_core_active = !passthrough && am2_open_core_gate_active();
        let steady_rail_mv =
            s19j_hybrid_chip_rail_target_mv(self.config.power.psu_override.as_ref());
        let requested_open_core_mv = if open_core_active {
            s19j_open_core_mv()
        } else {
            steady_rail_mv
        };
        let open_core_rail_plan = admit_open_core_rail_plan(
            open_core_active,
            all_active_voltage_enable,
            steady_rail_mv,
            requested_open_core_mv,
        )
        .context("AM2 open-core rail plan was not admitted before hardware preparation")?;

        let serial_devices = self.config.mining.resolved_serial_devices("/dev/ttyS2");
        let planned_chain_contexts = build_am2_chain_plan(&serial_devices)?;
        let serial_device = am2_phase1_select_serial_device(&serial_devices, "/dev/ttyS2");
        let selected_pic_addr =
            am2_pic_addr_from_serial_device(&serial_device).ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot derive S19j dsPIC address from serial device '{}'",
                    serial_device
                )
            })?;
        let mut chain_uart_device = std::env::var("DCENT_AM2_CHAIN_UART_OVERRIDE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| serial_device.clone());
        let effective_chain_dspic_addr = am2_pic_addr_from_serial_device(&chain_uart_device);
        if open_core_rail_plan.requires_demotion()
            && effective_chain_dspic_addr != Some(selected_pic_addr)
        {
            anyhow::bail!(
                "AM2 open-core elevation requires one typed rail owner: selected dsPIC 0x{selected_pic_addr:02X} does not own effective chain UART {} ({:?})",
                chain_uart_device,
                effective_chain_dspic_addr
            );
        }
        if self.shutdown.is_cancelled() {
            anyhow::bail!("AM2 hybrid run was cancelled before the first hardware mutation");
        }

        // .25 RE-018 kernel-UART transport: free IRQ 165 (unbind chain1-work-tx
        // from uio_pdrv_genirq) BEFORE the first SerialChainBackend::open so the
        // kernel of_serial can claim /dev/ttyS1 (selection=kernel-open). No-op
        // unless DCENT_AM2_KERNEL_UART_UNBIND_WORKTX=1 + .25 fingerprint +
        // SERIAL_WORK_DISPATCH all hold -> fleet/handoff byte-identical.
        am2_free_chain1_work_tx_irq_for_kernel_uart();

        let am2_controller_plan = if passthrough {
            None
        } else {
            Some(
                discover_system_am2_controller_plan(&serial_devices)
                    .context("AM2 controller endpoint plan discovery failed")?,
            )
        };
        log_am2_planned_chain_contexts(&planned_chain_contexts);
        if serial_devices.len() > 1 {
            warn!(
                serial_devices = ?serial_devices,
                selected_serial_device = %serial_device,
                planned_chain_contexts = planned_chain_contexts.len(),
                selected_chain_id = ?planned_chain_contexts.first().map(|ctx| ctx.chain_id),
                selected_am2_slot = ?planned_chain_contexts.first().map(|ctx| ctx.am2_slot),
                selected_dspic_addr = ?planned_chain_contexts
                    .first()
                    .map(|ctx| format!("0x{:02X}", ctx.dspic_addr)),
                "mining.serial_devices parsed into an immutable AM2 chain plan, but phase-1 S19j hybrid runtime still executes the first planned context only"
            );
        }
        let chip_count = self.config.mining.serial_chip_count.unwrap_or(126);
        let target_freq = self.config.mining.frequency_mhz;
        if !passthrough && process_name_running("bosminer") {
            anyhow::bail!(
                "refusing native S19j hybrid takeover while bosminer is still running; run /tmp/xil_prepare_dcentos_quiet.sh first and verify the fan guard is active before launching dcentrald"
            );
        }
        // `mut` so the dsPIC-disable leg of the teardown can be armed once the
        // single-owner I2C service + chain topology are known (see
        // `arm_dspic_teardown` below). Until armed, Drop is byte-identical to
        // the historical PWR_CONTROL-low + fans-only teardown.
        let mut _home_hard_stop_guard = if passthrough {
            None
        } else {
            // W24-CRASH-1: arm the process-global panic-hook teardown params at
            // the same moment we arm the run-scope RAII guard. The RAII Drop
            // does NOT run under `panic = "abort"`; the panic hook in `main()`
            // reads these params to perform the same best-effort
            // cut-hash-before-noise teardown on a crash.
            arm_am2_teardown_params(&self.config);
            Some(Am2HomeHardStopGuard::new(&self.config))
        };

        // Parse FPGA chain base address.
        let fpga_base_str = self
            .config
            .mining
            .fpga_chain_base
            .clone()
            .unwrap_or_else(|| "0x43C00000".to_string());
        let fpga_base = u64::from_str_radix(fpga_base_str.trim_start_matches("0x"), 16)
            .context("Invalid fpga_chain_base hex address")?;
        let fpga_chain_id = self.config.mining.fpga_chain_id.unwrap_or(1);

        // Map logical chain_id → physical chain index for the diagnostic
        // glitch-monitor mirror lookup (Braiins-am2 only).
        //
        // Per Phase 1 probe .139: chain1 is physical chain 2 (populated), chain4
        // is physical chain 3 (populated), physical chain 1 is unpopulated. The
        // BraiinsGlitchMonitor mirror layout matches bosminer's UartRelayReg
        // string-table layout:
        //   phys_idx 2 → +0x30 (chain1)
        //   phys_idx 3 → +0x34 (chain4)
        let relay_phys_idx: u8 = match fpga_chain_id {
            1 => 2,
            4 => 3,
            other => anyhow::bail!(
                "am2 supports fpga_chain_id 1 or 4 (physical 2 or 3); got {}",
                other
            ),
        };

        // ================================================================
        // BraiinsGlitchMonitor (UIO) bring-up — BEFORE Phase 0.
        //
        // W13.B1 (2026-05-10) RECLASSIFIED: the `0x43D000xx` window is a
        // diagnostic-only glitch-monitor mirror, Braiins-am2 only. Stock
        // CV1835/AM335x/AML/S9 hardware does NOT populate this UIO.
        // BM1362 0x2C/0x34 candidate relay broadcasts are lab-gated by
        // `DCENT_BM1362_ENABLE_UART_RELAY_LAB`.
        //
        //  RE (2026-04-27, still valid): the Xilinx `uio_pdrv_genirq`
        // kernel driver gates AXI-Lite **write** permission on the open
        // /dev/uioN fd. Bosminer keeps `/dev/uio18` open for the full
        // daemon lifetime; we mirror that for telemetry parity.
        //
        // Failure modes:
        //   * No UIO discovered (stock hw / am1-s9 bitstream): glitch_monitor=None,
        //     downstream telemetry falls back to None.
        //   * UIO open fails (permissions / driver missing): logged WARN,
        //     glitch_monitor=None, same fallback.
        // ================================================================
        if !passthrough && self.config.mining.am2_force_braiins_glitch_mirror_write {
            match find_glitch_monitor_uio_number() {
                Some(uio_n) => match BraiinsGlitchMonitor::open(uio_n) {
                    Ok(gm) => {
                        info!(
                            uio = uio_n,
                            "BraiinsGlitchMonitor opened (Braiins-am2 only — diagnostic mirror, NOT control)"
                        );
                        self.glitch_monitor = Some(gm);
                    }
                    Err(e) => {
                        warn!(
                            uio = uio_n,
                            error = %e,
                            "BraiinsGlitchMonitor::open() failed during early init — diagnostic-only telemetry skipped"
                        );
                    }
                },
                None => {
                    info!(
                        "No miner-glitch-monitor UIO discovered — diagnostic-only telemetry skipped (stock hw or non-Braiins-am2 bitstream)"
                    );
                }
            }
        } else {
            info!(
                passthrough,
                am2_force_braiins_glitch_mirror_write =
                    self.config.mining.am2_force_braiins_glitch_mirror_write,
                "BraiinsGlitchMonitor early init skipped (passthrough or feature disabled)"
            );
        }

        // ================================================================
        // Phase 0: PSU bring-up — THREE mutually-exclusive branches:
        //   (a) passthrough            — bosminer owns the PSU; do nothing here.
        //   (b) psu_override.enabled   — "Loki bypass": a non-smart PSU
        //                                (e.g. APW3 @ ~12.8 V). No APW121215a
        //                                to probe / no watchdog / no DAC.
        //                                Assert PWR_CONTROL via `PsuBypassGate`,
        //                                record the declared model + rail
        //                                voltage, NO heartbeat thread, NO 5 s
        //                                stability sleep — then fall through.
        //   (c) default                — APW121215a @ /dev/i2c-0 slave 0x10:
        //                                MUST be first (PSU self-disables in
        //                                ~30 s without a heartbeat). Bring the
        //                                bus up, probe FW, run 3×Disable → Ramp
        //                                → Enable (`set_voltage_init_bypass`,
        //                                heartbeat not started yet), spawn the
        //                                1 Hz heartbeat thread, wait 5 s so
        //                                downstream SetVoltage can pass its
        //                                stability gate. See `10-psu-watchdog.md`.
        //
        // In all three cases `i2c0_service` is still spawned below (Phase 1
        // dsPIC + the EEPROM 0x50-0x57 write-denylist) — the override branch
        // simply never opens an `Apw121215a` on it.
        // ================================================================
        if self.shutdown.is_cancelled() {
            anyhow::bail!("AM2 hybrid run was cancelled before PSU/rail bring-up");
        }
        let psu_arc: Option<Arc<Mutex<Apw121215a>>>;
        let mut runtime_threads = RuntimeThreadGuard::new(self.shutdown.clone());
        // RAII guard for the "Loki bypass" path: owns PWR_CONTROL when
        // `psu_override.enabled` and the APW121215a path is skipped. Lives to
        // `run()` scope-end so its Drop deasserts PWR_CONTROL — the same
        // teardown guarantee `Apw121215a::Drop` gives the default branch via
        // `psu_arc`. Underscore-prefixed: it is a pure guard, never read.
        // `mut` so the WAKE-DSPIC-BEFORE-RAIL path can bind the deferred rail gate
        // later (Phase 2b), after Phase 0(b) left it None.
        let mut _psu_bypass_gate: Option<PsuBypassGate>;
        // DCENT_AM2_WAKE_DSPIC_BEFORE_RAIL (2026-06-13, "make-it-work" workflow rank-1):
        // replicate bosminer's true-cold block-A->block-B order. bosminer boots the dsPIC
        // SMPS app (RESET->JUMP->fw=0x89) on 3.3 V STANDBY with the main 12.8 V rail OFF,
        // THEN energizes the rail, THEN ENABLEs. DCENT historically asserts PWR_CONTROL FIRST
        // (Phase 0(b)) then wakes the dsPIC with the rail already on; with FORCE_REDO the SMPS
        // app re-inits under the wrong input state, ACKs ENABLE [15,01] + reports a nominal
        // 0x3A setpoint but never soft-starts real die current (enum=0, board cold). When this
        // gate is set AND the .25 fingerprint matches, Phase 0(b) does NOT assert the rail; it
        // forces PWR_CONTROL OFF and stashes the gate args, and the rail is asserted just
        // before Phase 2b (after the Phase-0d standby warmup lands fw=0x89). The deferred gate
        // is still bound to `_psu_bypass_gate` so its Drop de-energizes on teardown. Default-OFF
        // + .25-fingerprinted -> fleet /  handoff / .109 / .79 byte-identical.
        let wake_dspic_before_rail =
            am2_env_flag("DCENT_AM2_WAKE_DSPIC_BEFORE_RAIL") && am2_xil_25_fingerprint_matches();
        let mut deferred_rail_assert: Option<(Option<String>, String, f64)> = None;
        // PWR_CONTROL gate state is no longer tracked here. Ownership has
        // moved into `Apw121215a` (W2.A2): the PSU object asserts the gate
        // inside `cold_boot_sequence_gated` / `cold_boot_sequence_write_only`
        // and the gate's Drop fires when the PSU is dropped at scope-end. The
        // bypass branch routes the same `pwr_control_gpio` spec into a
        // `PsuBypassGate` instead.
        let psu_override_cfg = self.config.power.psu_override.clone();
        let psu_override_active = psu_override_active(psu_override_cfg.as_ref());
        let psu_address = self.config.psu.i2c_address;
        let psu_target_rail_v = self.config.psu.voltage_mv as f64 / 1000.0;
        let psu_transport = self.config.psu.transport.as_str();
        let psu_heartbeat_hz = u64::from(self.config.psu.heartbeat_hz.max(1));
        let psu_heartbeat_interval = Duration::from_millis((1000 / psu_heartbeat_hz).max(1));
        let i2c0_service: Option<I2cServiceHandle> = if passthrough {
            None
        } else {
            // am2 hashboard EEPROM is at I²C addresses 0x50-0x57 (AT24C-series
            // standard). dcentrald NEVER writes to these — only bosminer reads
            // them at boot for board identity. Block writes at the bus layer to
            // defend against any future code-path bug or misrouted address.
            // (2026-04-29
            // .74 hb2 EEPROM corruption incident motivated this gate).
            let am2_eeprom_denylist: Vec<u8> = (0x50u8..=0x57u8).collect();
            let service =
                spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation(
                    0,
                    am2_eeprom_denylist.clone(),
                    || {
                        ensure_i2c0_kernel_bound().map_err(|error| {
                            std::io::Error::other(format!(
                                "xiic-i2c bring-up failed after fabric reservation: {error:#}"
                            ))
                        })
                    },
                )
                .context("Failed to reserve, prepare, and spawn AM2 /dev/i2c-0 service")?;
            if let Err(e) = service.set_timeout(10) {
                warn!(error = %e, "AM2 I2C service timeout setup failed; continuing with service default");
            }
            info!(
                denylist = ?am2_eeprom_denylist.iter().map(|a| format!("0x{:02X}", a)).collect::<Vec<_>>(),
                "AM2 I2C service spawned for /dev/i2c-0 (single owner; EEPROM range write-protected)"
            );
            Some(service)
        };
        // SKIP early chain probe — live `a lab unit` evidence (2026-04-26) shows
        // the probe at fresh boot writes the parser-flush + GET_VERSION to
        // each candidate dsPIC, which corrupts the parser state of healthy
        // chains and breaks the subsequent Phase 1 GET_VERSION. Earlier runs
        // without the probe got fw=0x86 reliably from 0x21; runs WITH the
        // probe got [FF] then [00] (parser corrupted). The probe is now
        // disabled and `active_chains` defaults to 0b111 (assume all 3
        // candidate addresses are POSSIBLY active). Phase 1's existing
        // multi-attempt GET_VERSION + parser flush is the authoritative
        // chain-presence test. If a chain is silent (e.g., hb3=0x22 on
        // `a lab unit`), Phase 1 fails for it and the daemon proceeds with the
        // populated chain only..
        let active_chains: u8 = 0b111;

        if passthrough {
            info!("PASSTHROUGH MODE — skipping Phase 0 PSU bring-up (bosminer owns PSU)");
            psu_arc = None;
            _psu_bypass_gate = None;
        } else if psu_override_active {
            // -----------------------------------------------------------------
            // Phase 0 branch (b): PSU OVERRIDE ("Loki bypass") — non-smart PSU.
            //
            // The operator declared a PSU model + output voltage (e.g. an APW3
            // tweaked to ~12.8 V — the "Loki Mod"). There is no APW121215a at
            // 0x10 to probe, nothing to disable-watchdog / set-DAC / arm. We
            // still MUST assert PWR_CONTROL (the APW3 output enable is wired
            // through it on a Loki-modded chassis, exactly like the stock
            // APW12 was), then proceed straight to Phase 1 (the hashboard
            // dsPIC chip-voltage path is UNCHANGED — it regulates ~13.7 V on
            // the chain from whatever the PSU delivers upstream).
            //
            // NOTE: `psu_override.voltage_v` is the PSU output / hashboard-DC-DC
            // *input* rail (~12.8 V) — it is deliberately NOT used as the
            // per-chain chip-rail setpoint. Phase 3's `cold_boot_init(13_700)`
            // is the chip rail and is untouched here.
            // and
            // .
            // -----------------------------------------------------------------
            let ovr = psu_override_cfg
                .as_ref()
                .expect("psu_override_active implies psu_override_cfg is Some");
            let declared_model = ovr.model.clone();
            let declared_rail_v = ovr.voltage_v;
            let psu_gate_spec: Option<String> = self.config.psu.pwr_control_gpio.clone();

            if !(11.0..=14.5).contains(&declared_rail_v) {
                warn!(
                    voltage_v = declared_rail_v,
                    "[power.psu_override].voltage_v looks unusual for an Antminer PSU rail \
                     (expected ~12.0-14.5 V) — this is the PSU OUTPUT rail, not the ~1.3 V \
                     chip voltage; proceeding anyway"
                );
            }
            if self
                .config
                .psu
                .model
                .to_ascii_uppercase()
                .starts_with("APW12")
            {
                warn!(
                    psu_model = %self.config.psu.model,
                    "[psu].model says APW12 but [power.psu_override] is enabled — if this unit \
                     actually has a smart APW12/APW121215a it will self-disable the rail in \
                     ~30 s with no heartbeat; the override path runs NO heartbeat thread"
                );
            }

            if wake_dspic_before_rail {
                // WAKE-DSPIC-BEFORE-RAIL: do NOT assert the main rail here. Force
                // PWR_CONTROL to the OFF level (idempotent; normally already low at
                // cold boot) so the Phase-0d dsPIC standby warmup runs with the
                // 12.8 V input ABSENT (bosminer block A). Stash the gate args; the
                // rail is asserted just before Phase 2b (bosminer block B).
                force_pwr_control_low(
                    psu_gate_spec.as_deref(),
                    "wake-dspic-before-rail:standby-rail-off",
                );
                _psu_bypass_gate = None;
                deferred_rail_assert = Some((
                    psu_gate_spec.clone(),
                    declared_model.clone(),
                    declared_rail_v,
                ));
                info!(
                    model = %declared_model,
                    rail_v = declared_rail_v,
                    t_ms = t(),
                    "[T+{}] Phase 0 (b): WAKE-DSPIC-BEFORE-RAIL — main 12.8 V rail HELD OFF; \
                     dsPIC will wake on 3.3 V standby first (bosminer block-A order); rail \
                     asserted just before Phase 2b/EBR",
                    t()
                );
            } else {
                let gate = PsuBypassGate::assert(
                    psu_gate_spec.as_deref(),
                    declared_model.clone(),
                    declared_rail_v,
                )
                .context("PSU bypass: PWR_CONTROL gate assert failed")?;
                info!(
                    model = %declared_model,
                    rail_v = declared_rail_v,
                    gpio = gate.gpio(),
                    efficiency = ?crate::runtime::efficiency::psu_efficiency_for_model_name(&declared_model),
                    t_ms = t(),
                    "[T+{}] Phase 0 (b): PSU OVERRIDE (Loki bypass) — operator-declared non-smart PSU; \
                     PWR_CONTROL asserted; rail voltage recorded (NOT the chip voltage)",
                    t()
                );

                // CE §10 #4: this `Some(gate)` is the PSU-override (Loki / bare-APW3)
                // branch's PWR_CONTROL owner — a GPIO-only `PsuBypassGate` that holds
                // the rail enable for the whole run (no smart-APW12 PSU object exists
                // on this path). It is dropped at end-of-run scope, NOT reassigned to
                // None mid-run (contrast the smart-APW12 success branch above, which
                // hands ownership to the live `Apw121215a`). Single-owner invariant.
                _psu_bypass_gate = Some(gate);
            }

            //  (2026-05-22) — log operator-declared psu_hardware_variant
            // ( EE-LOKI-001 telemetry). Metadata only, not consumed by
            // any mining decision path; useful for fleet inventory + future
            // telemetry surfaces.
            info!(
                psu_hardware_variant = ovr.psu_hardware_variant.as_deref().unwrap_or("(unset)"),
                "[T+{}] Phase 0 (b): operator-declared psu_hardware_variant = {}",
                t(),
                ovr.psu_hardware_variant.as_deref().unwrap_or("(unset)")
            );

            // W1.5 RE (2026-06-13, jig-decoded; CORRECTED 2026-06-13 after a code
            // re-read): the BM1362 factory jig (single_board_test `FUN_00015500`)
            // waits `usleep(3000000)` = 3.0s AFTER "APW power on ok" — i.e. after the
            // PSU output rail is enabled — BEFORE PIC-EN + the reset pulse + find-ASIC.
            // ACCURATE DCENT baseline: all three Phase-0 branches DO share the Phase-0p
            // post-EEPROM dsPIC-boot grace (~2s, `am2_post_eeprom_dspic_grace_ms`,
            // s19j ~8101) before Phase 1, and the smart-APW branch additionally has a
            // 5s SetVoltage-stability sleep (s19j ~8017, NOT on this Loki path). So the
            // override path was NOT a zero-settle path — it had ~2s pre-PIC-EN, but
            // that is 1s SHORT of the jig's 3s post-APW AND it is not positioned right
            // after the rail enable (it is post-EEPROM, dsPIC-boot-purposed). This adds
            // the jig-POSITIONED ≥3s post-APW rail-stabilization settle ON TOP of
            // Phase-0p — a timing REFINEMENT to reach/exceed the jig's 3s, not a
            // from-zero missing settle. Plausible (not certain) contributor to the
            // class-B cold-rail signature; the DMM at the die is the real arbiter.
            // Gated: byte-identical (0 ms) for every current unit by default (incl.
            // the proven `a lab unit` path); the `a lab unit` launcher opts in
            // (`DCENT_AM2_POST_APW_SETTLE_MS=3000`). Pure delay, rail ON + chips in
            // reset/off = home-safe, NO new MMIO write. Clamp [0, 10000].
            let post_apw_settle_ms = am2_env_u64("DCENT_AM2_POST_APW_SETTLE_MS", 0).min(10_000);
            // Skip the post-APW settle on the WAKE-DSPIC-BEFORE-RAIL path: the rail is
            // still OFF here, so there is nothing to stabilize yet. The deferred rail
            // assert (just before Phase 2b) performs its own post-rail settle.
            if post_apw_settle_ms > 0 && !wake_dspic_before_rail {
                info!(
                    settle_ms = post_apw_settle_ms,
                    t_ms = t(),
                    "[T+{}] Phase 0 (b): post-APW-power-on rail settle (jig-faithful, W1.5 RE) — \
                     {} ms before dsPIC ENABLE + enum",
                    t(),
                    post_apw_settle_ms
                );
                std::thread::sleep(Duration::from_millis(post_apw_settle_ms));
            }

            //  (2026-05-22) — EE-LOKI-001 hard-skip gate:
            //
            // When the operator declares `[power.psu_override].no_smbus_peer = true`
            // they have asserted "there is NO Loki spoof / APW12 peer on
            // i2c-0@0x10 — this chassis is bare-modded-APW3". The lenient
            // probe's 200 ms deadline + 3×100 ms retry adds latency for no
            // benefit AND opens the EE-LOKI-001 phantom-device-on-0x10 SMBus
            // hazard (per EE review §5/T3): a residual Loki carcass, test
            // fixture, or miswire could ACK the probe and receive the
            // bosminer-canonical SetVoltage 15.2V sequence intended for a
            // smart-APW12. The BM1362 chip itself is electrically isolated
            // by the per-board buck, so this is not a chip-damage path —
            // but the unintended-peer SMBus byte exposure is closed by
            // skipping the probe entirely.
            //
            // Behavior on hard-skip: `psu_arc` remains `None` and the helper
            // that can return a heartbeat handle is never called. The daemon
            // proceeds to PWR_CONTROL-only mode through to Phase 1 unchanged.
            //
            //  (2026-05-26) — zero-PSU-byte diagnostic gate:
            //
            // Patch 8's launcher tried to prove the `a lab unit` path on `a lab unit` by
            // unsetting every Loki/APW env var, but the runtime still entered
            // this opportunistic smart-APW12 lenient branch because the legacy
            // TOML had no `no_smbus_peer=true` field. `DCENT_AM2_ZERO_PSU_BYTES=1`
            // is the explicit run-scope assertion: under psu_override, emit
            // NO APW12/Loki/gpio-bitbang bytes, regardless of `[psu].transport`.
            //
            //
            // Finding EE-LOKI-001 + `config.rs:PsuOverride::no_smbus_peer`.
            if ovr.no_smbus_peer == Some(true) {
                info!(
                    t_ms = t(),
                    "[T+{}] Phase 0c: no_smbus_peer=true — hard-skipping smart-APW12 probe \
                     (operator-declared bare APW3; closes EE-LOKI-001 phantom-device hazard)",
                    t()
                );
                psu_arc = None;
            } else if am2_zero_psu_bytes_enabled() {
                info!(
                    t_ms = t(),
                    env_gate = "DCENT_AM2_ZERO_PSU_BYTES=1",
                    transport = psu_transport,
                    "[T+{}] Phase 0c: DCENT_AM2_ZERO_PSU_BYTES=1 — hard-skipping \
                     smart-APW12/Loki gpio-bitbang branch under psu_override \
                     (zero PSU bytes; PWR_CONTROL-only mode)",
                    t()
                );
                psu_arc = None;
            } else {
                // ----- Layer-2 (FIX-A) opportunistic smart-APW12 handshake -----
                //
                // 2026-05-22 (XIL `a lab unit` recovery): the operator's BraiinsOS log on
                // `a lab unit` shows the Loki board IS a smart-APW12 spoof at 0x10
                // (FW '0x71' (APW121215a)) — it answers GetFwVersion, accepts the
                // canonical `3× Disable → Ramp → Enable` sequence, and grants
                // a working rail. The legacy `psu_override` branch SKIPPED this
                // handshake entirely on the assumption "non-smart PSU = no I²C
                // peer", which is wrong on Loki-equipped units. Opportunistically
                // try the bosminer-canonical sequence here; on success spawn the
                // 1 Hz heartbeat so the spoof watchdog doesn't drop the rail. On
                // a genuinely silent bus (Loki-removed real APW3) this falls
                // through cleanly in ~200 ms with `psu = None` — byte-identical
                // to today's `psu_override` branch behaviour from that point on.
                //
                // The PWR_CONTROL gate is owned by the `PsuBypassGate` above; the
                // helper opens its `Apw121215a` WITHOUT a gate_spec to avoid
                // double-asserting the same GPIO.
                //
                // §(d) FIX-A.
                match i2c0_service.as_ref() {
                    Some(i2c0) => {
                        let outcome = bring_up_apw121215a_smart_lenient(
                            i2c0,
                            psu_address,
                            psu_target_rail_v,
                            psu_heartbeat_interval,
                            self.shutdown.clone(),
                            psu_transport,
                        );
                        psu_arc = outcome.psu;
                        if let Some(handle) = outcome.heartbeat {
                            runtime_threads.push("s19j-psu-heartbeat", handle);
                        }
                        if psu_arc.is_some() && runtime_threads.contains("s19j-psu-heartbeat") {
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 0 (b): smart-APW12 spoof handshake SUCCEEDED on Loki bus \
                                 — heartbeat thread spawned; bosminer-parity init achieved",
                                t()
                            );
                        } else if psu_arc.is_some() {
                            // Rail ENABLED but the 1 Hz heartbeat kicker failed to
                            // spawn. Do NOT fall through to chip-rail bring-up + BM1362
                            // enum + work dispatch: without the heartbeat the spoof
                            // watchdog self-disables the rail in ~30 s, so proceeding
                            // would energize the chips and start mining on a PSU that
                            // is about to drop (fail-open — brief mining under an
                            // unmaintained rail). Bail so the run tears down and the
                            // PsuBypassGate de-energizes PWR_CONTROL now. This branch
                            // is only reachable on a rare thread-spawn failure, never
                            // on a healthy start (which lands in the SUCCEEDED branch
                            // above), so it cannot regress the proven mining path.
                            error!(
                                t_ms = t(),
                                "[T+{}] Phase 0 (b): smart-APW12 rail ENABLED but heartbeat thread \
                                 spawn FAILED (psu=Some, heartbeat=None) — refusing to bring up the \
                                 chip rail on a PSU with no heartbeat; tearing down",
                                t()
                            );
                            anyhow::bail!(
                                "am2 hybrid: PSU rail enabled but the heartbeat thread failed to \
                                 spawn — refusing to mine on an unmaintained rail (teardown \
                                 de-energizes PWR_CONTROL)"
                            );
                        } else {
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 0 (b): smart-APW12 opportunistic bring-up returned no PSU \
                                 (Loki-removed or genuinely-silent bus) — proceeding with \
                                 PWR_CONTROL-only (legacy psu_override behaviour)",
                                t()
                            );
                        }
                    }
                    None => {
                        // `i2c0_service` is None only in `passthrough` mode, which
                        // is a different branch above. Defensive log only.
                        warn!(
                            "Phase 0 (b): i2c0_service is None in psu_override branch — \
                             skipping smart-APW12 handshake (this should not happen)"
                        );
                        psu_arc = None;
                    }
                }
            }
        } else {
            info!(t_ms = t(), "[T+{}] PSU probe start", t());
            info!(
                transport = psu_transport,
                "Phase 0: PSU bring-up (APW121215a, transport={}, addr 0x{:02X})",
                psu_transport,
                psu_address,
            );

            // PWR_CONTROL gate is now owned by `Apw121215a` itself: the PSU
            // module asserts it inside `cold_boot_sequence_gated` /
            // `cold_boot_sequence_write_only` (after we propagate the spec
            // via `set_psu_gate_spec` below) and Drop deasserts it when the
            // PSU is dropped. This replaces the fragile manual call site.
            //.
            let psu_gate_spec: Option<String> = self.config.psu.pwr_control_gpio.clone();

            // Phase 0-pre: do not replay legacy RESET/JUMP sequences. Current
            // `a lab unit` evidence proves GPIO chain reset is not the gap, and
            // Pic0x89-family bootloader-control opcodes must stay behind the
            // typed dsPIC safety guard rather than ad-hoc raw I2C writes.
            warn!(
                "Phase 0-pre: skipping legacy dsPIC RESET/JUMP; production path waits for validated GET_VERSION preflight"
            );

            // Phase 7A finding: PSU at 0x10 is silent until hashboard dsPICs + PSU
            // dsPIC finish booting (~97 s of bosminer's "hashboard EEPROM reads"
            // accidentally gates this). Probe 0x50 (hashboard EEPROM) in a bounded
            // retry loop — once EEPROM ACKs, the hashboards are up and PSU should
            // be too. Max 120 s wait (bosminer sometimes takes the full 97 s).
            info!("Phase 0a: Waiting for hashboard dsPICs to boot (probing 0x50 ACK)...");
            let mut psu = match psu_transport {
                "gpio_bitbang" => {
                    let mut psu = Apw121215a::open_gpio_bitbang_at(psu_address)
                        .context("Failed to open APW121215a via gpio bit-bang")?;
                    // Hand the gate spec to the PSU module before cold-boot;
                    // `cold_boot_sequence_write_only` will assert the gate
                    // fail-closed before any I²C write reaches the APW.
                    psu.set_psu_gate_spec(psu_gate_spec.clone());

                    //  (2026-05-24): STANDALONE Loki cold-boot path.
                    // Fires only when ALL of the following hold:
                    //   1. env DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1
                    //   2. `a lab unit`-class XIL hardware fingerprint matches
                    //   3. env DCENT_AM2_TRUST_RAIL_FALLBACK != 1
                    //      (operator NOT asking for bosminer-handoff)
                    // Default-OFF — when ANY condition fails, the call site
                    // falls through to the byte-identical
                    // `cold_boot_sequence_write_only` path ( proven
                    // recipe), preserving fleet behavior for `a lab unit` / `a lab unit` /
                    // `a lab unit` / `a lab unit` / S9 byte-for-byte.
                    //
                    //
                    // PHASE2B-BYTE-LEVEL-GAPS.md` for the  →
                    // byte mapping. Regression-pinned by
                    // `tests/wave55b_loki_cold_boot_sequence.rs`.
                    let wave55b_loki_cold_boot_active = am2_psu_loki_cold_boot_full_enabled()
                        && am2_xil_25_fingerprint_matches()
                        && !am2_env_flag("DCENT_AM2_TRUST_RAIL_FALLBACK");
                    if wave55b_loki_cold_boot_active {
                        warn!(
                            target_v = psu_target_rail_v,
                            env_gate = "DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1",
                            "Wave-55b STANDALONE Loki cold-boot path ENGAGED — \
                             emitting Wave-38 cold-wake bytes before disable+ramp+enable. \
                             This is the no-bosminer-handoff bring-up path."
                        );
                        psu.cold_boot_sequence_loki_standalone(psu_target_rail_v)
                            .context("Wave-55b standalone Loki cold-boot failed")?;
                    } else {
                        psu.cold_boot_sequence_write_only(psu_target_rail_v, APW12_139_ASSUMED_FW)
                            .context("PSU write-only cold_boot_sequence failed")?;
                    }
                    psu
                }
                _ => {
                    let probe_i2c = i2c0_service
                        .as_ref()
                        .context("AM2 I2C service missing for hashboard EEPROM probe")?;
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                    let mut attempts = 0u32;
                    let mut eeprom_ready = false;
                    while std::time::Instant::now() < deadline {
                        attempts += 1;
                        let read_res = probe_i2c.read_hashboard_eeprom_prefix_at(0x50, deadline);
                        info!(
                            attempt = attempts,
                            read_ok = read_res.is_ok(),
                            "EEPROM 0x50 identity-prefix read attempt"
                        );
                        match read_res {
                            Ok(_) => {
                                info!(
                                    attempts,
                                    "Hashboard EEPROM 0x50 identity prefix readable — dsPICs ready"
                                );
                                eeprom_ready = true;
                                break;
                            }
                            Err(dcentrald_hal::HalError::I2cEndpointNotReady { .. }) => {}
                            Err(error) => {
                                return Err(error).context(
                                    "AM2 hashboard EEPROM bootstrap service failed terminally",
                                );
                            }
                        }
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        std::thread::sleep(remaining.min(std::time::Duration::from_secs(3)));
                    }
                    if !eeprom_ready {
                        warn!(
                            attempts,
                            "Hashboard EEPROM identity prefix remained unreadable for 60 s — proceeding with PSU init anyway"
                        );
                    }
                    let mut psu = Apw121215a::open_service_at(probe_i2c.clone(), 0, psu_address)
                        .context("Failed to open APW121215a via AM2 I2C service at 0x10")?;
                    // Hand the gate spec to the PSU module before cold-boot;
                    // `cold_boot_sequence_gated` (called below) will assert
                    // PWR_CONTROL fail-closed before the first APW opcode.
                    psu.set_psu_gate_spec(psu_gate_spec.clone());

                    match psu.probe() {
                        Ok(model) => info!(
                            model = model.name(),
                            fw = format_args!("0x{:02X}", psu.fw_byte().unwrap_or(0)),
                            "PSU probe OK (expect APW121215a / FW 0x71)",
                        ),
                        Err(e) => {
                            return Err(e).context(
                                "PSU identity probe failed or rejected a non-fw71 command dialect",
                            )
                        }
                    }

                    match psu.get_device_type() {
                        Ok(device_type) => info!(
                            response_len = device_type.len(),
                            "PSU device-type bytes: {:02X?}", device_type
                        ),
                        Err(e) => {
                            warn!(error = %e, "PSU device-type read failed — non-fatal")
                        }
                    }

                    info!(
                        t_ms = t(),
                        "[T+{}] APW disable cycle start (3× → Ramp → Enable)",
                        t()
                    );
                    psu.cold_boot_sequence(psu_target_rail_v)
                        .context("PSU cold_boot_sequence failed")?;
                    info!(t_ms = t(), "[T+{}] PSU rail engage requested", t());
                    psu
                }
            };

            let psu = Arc::new(Mutex::new(psu));

            // Spawn the 1 Hz heartbeat BEFORE anything that could take >30 s.
            let psu_hb = psu.clone();
            let shutdown_hb = self.shutdown.clone();
            runtime_threads.push(
                "s19j-psu-heartbeat",
                std::thread::Builder::new()
                    .name("s19j-psu-hb".into())
                    .spawn(move || psu_heartbeat_loop(psu_hb, shutdown_hb, psu_heartbeat_interval))
                    .context("Failed to spawn PSU heartbeat thread")?,
            );
            info!(
                hz = psu_heartbeat_hz,
                t_ms = t(),
                "[T+{}] PSU heartbeat armed, hz={}",
                t(),
                psu_heartbeat_hz
            );

            // Wait ≥5 stable ticks before any runtime SetVoltage path is allowed.
            // (cold_boot_sequence used set_voltage_init_bypass, which skips the gate
            // precisely for this opening window.)
            //
            // CE-002: this is a PURE wall-clock settle window — nothing touches
            // the I²C bus during it (the 1 Hz PSU heartbeat runs on its own
            // `std::thread`, NOT this Tokio worker). Using `tokio::time::sleep`
            // instead of `std::thread::sleep` lets the executor service the
            // already-spawned :8080 API / dashboard / MCP / watchdog tasks
            // during the 5 s instead of starving them. Hardware semantics are
            // byte-identical (same wall-clock delay, same ordering — no bus
            // activity is interleaved here, so there is no async-reorder risk).
            tokio::time::sleep(Duration::from_secs(5)).await;
            psu_arc = Some(psu);
            // CE §10 #4 ( expert review): the `= None` here is an
            // INTENTIONAL ownership handoff, not dead state. The smart-APW12
            // `Apw121215a` (`psu`) has just taken PWR_CONTROL ownership for the
            // rest of the run; reassigning `_psu_bypass_gate = None` drops the
            // earlier GPIO-only `PsuBypassGate` whose `Drop` would otherwise
            // deassert PWR_CONTROL out from under the now-live PSU. Exactly one
            // owner of PWR_CONTROL must exist at a time — do NOT remove this
            // line or move the gate's lifetime; the Drop ordering is the
            // invariant. See `psu_bypass_gate.rs` and the §4.3 finding.
            _psu_bypass_gate = None;
        }

        if am2_diag_stop_after_psu_enabled() {
            warn!(
                t_ms = t(),
                env_gate = "DCENT_AM2_DIAG_STOP_AFTER_PSU=1",
                psu_override = psu_override_active,
                psu_object_opened = psu_arc.is_some(),
                psu_heartbeat_thread = runtime_threads.contains("s19j-psu-heartbeat"),
                "AM2 diagnostic stop after Phase 0 PSU path — stopping before \
                 dsPIC voltage enable, chain UART init, Stratum, or work dispatch"
            );
            force_am2_home_hard_stop(&self.config, "diag-stop-after-psu");
            let feeders_quiesced = stop_am2_runtime_feeders_bounded(
                &self.config,
                &mut runtime_threads,
                "diag-stop-after-psu",
            )
            .await;

            if feeders_quiesced {
                let Some(psu_mutex) = psu_arc.as_ref() else {
                    anyhow::bail!(
                        "DCENT_AM2_DIAG_STOP_AFTER_PSU=1: diagnostic stop after Phase 0 PSU path \
                         (no dsPIC voltage enable, no chain UART init, no Stratum, no work dispatch)"
                    );
                };
                let mut psu_guard = psu_mutex.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(psu_err) = psu_guard.safe_shutdown_to_min() {
                    warn!(
                        error = %psu_err,
                        "PSU safe-direction shutdown failed during Phase-0 diagnostic stop"
                    );
                }
            }
            anyhow::bail!(
                "DCENT_AM2_DIAG_STOP_AFTER_PSU=1: diagnostic stop after Phase 0 PSU path \
                 (no dsPIC voltage enable, no chain UART init, no Stratum, no work dispatch)"
            );
        }

        // ====================================================================
        // Phase 0p (2026-05-22 XIL `a lab unit` recovery, Layer 3) — post-EEPROM
        // dsPIC firmware-boot grace window.
        //
        // EEPROM 0x50 ACK in Phase 0a proves the hashboard 3.3 V manageability
        // rail is up but does NOT prove the dsPIC has completed firmware boot
        // + registered its I²C MSSP slave ISR. Bosminer's implicit minimum on
        // `a lab unit`/`a lab unit` is ~48 s (incidental from its serial service tasks);
        // by datasheet the dsPIC needs only a few hundred ms. We sleep
        // `am2_post_eeprom_dspic_grace_ms` (default 2000 ms = 4× BraiinsOS
        // `RESET_DELAY`) here, in ALL THREE Phase-0 branches (passthrough,
        // psu_override, default), so the dsPIC has a chance to be fully
        // resident before Phase 1's first GET_VERSION races at T+283 ms on
        // psu_override warm-boot. Skipped in `passthrough` mode if bosminer
        // is owning the bus.
        //
        // §(a)+§(d).
        // ====================================================================
        let dspic_boot_grace_ms = self.config.mining.am2_post_eeprom_dspic_grace_ms;
        if !passthrough && dspic_boot_grace_ms > 0 {
            info!(
                t_ms = t(),
                dspic_boot_grace_ms,
                "[T+{}] Phase 0p: post-EEPROM dsPIC firmware-boot grace ({} ms — bosminer's implicit minimum)",
                t(),
                dspic_boot_grace_ms,
            );
            // CE-002: pure dsPIC firmware-boot settle window — no bus activity
            // is interleaved here, so yield the executor (tokio sleep) instead
            // of blocking the Tokio worker for up to ~2 s. Hardware semantics
            // unchanged (same wall-clock grace, same ordering).
            tokio::time::sleep(Duration::from_millis(dspic_boot_grace_ms)).await;
        } else if !passthrough {
            tracing::debug!(
                "Phase 0p: post-EEPROM dsPIC grace disabled by config (am2_post_eeprom_dspic_grace_ms = 0)"
            );
        }

        // ====================================================================
        // Phase 0s ( B2, 2026-05-22) — hashboard-SKU energize-refusal
        // gate (drive-half of matrix §7 #15).
        //
        // We've just proven the hashboard 3.3 V manageability rail is up
        // (Phase 0a EEPROM 0x50 ACK) and waited for the dsPIC to finish
        // firmware boot (Phase 0p grace). The next steps in this run will
        // drive the chain voltage rail (Phase 3 `cold_boot_init` writes
        // SetVoltage + ENABLE on the dsPIC). BEFORE that, classify EVERY
        // populated chain's EEPROM preamble + refuse to proceed if any
        // chain reports:
        //   • malformed preamble (not 0x04 0x11 BHB42xxx, not 0x05 0x11
        //     BHB56902, not all-0x00/0xFF)
        //   • EEPROM readiness timeout (the owned bus-0 service cannot read
        //     the fixed AT24 identity prefix within the readiness budget)
        //   • mixed-SKU pairing across chains (BHB42xxx + BHB56902 on the
        //     same unit — refuse all)
        //   • profile-bind failure (preamble readable but unknown family)
        //
        // First-deploy rollout: `DCENT_AM2_STRICT_SKU_REFUSE` default OFF.
        // Refusal reasons LOG (so the operator can confirm `a lab unit` is clean)
        // but do NOT bail. Operator flips env=1 after `a lab unit` confirms
        // zero false-positive refusal. `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1`
        // is the lab override (matches the toolbox `--accept-degraded-hardware`
        // pattern) that lets strict mode proceed anyway.
        //
        // Skipped under `passthrough` (bosminer owns the chain).
        // ====================================================================
        let mut am2_hashboard_presence: Vec<Am2HashboardPresence> = Vec::new();
        if !passthrough {
            use crate::runtime::hardware_info::{
                read_hashboard_eeprom_prefix_via_service_for_energize_gate,
                OwnedEepromReadinessError, DEFAULT_EEPROM_READINESS_BUDGET_MS,
            };
            use dcentrald_silicon_profiles::energize_gate::{
                accept_degraded_hardware_enabled, classify_chain, gate_chains_for_energize,
                strict_sku_refuse_enabled, ChainProbe,
            };

            let strict = strict_sku_refuse_enabled();
            let accept_degraded = accept_degraded_hardware_enabled();
            let eeprom_service = i2c0_service
                .as_ref()
                .context("AM2 I2C service missing for serialized hashboard identity reads")?;
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(DEFAULT_EEPROM_READINESS_BUDGET_MS);
            let mut probes: Vec<ChainProbe> = Vec::with_capacity(3);
            for slot in 0u8..=2u8 {
                match read_hashboard_eeprom_prefix_via_service_for_energize_gate(
                    eeprom_service,
                    slot as usize,
                    deadline,
                ) {
                    Ok(bytes) => {
                        if let Some((plan, context)) =
                            am2_controller_plan.as_ref().and_then(|plan| {
                                plan.context_for_slot(slot).map(|context| (plan, context))
                            })
                        {
                            match bind_am2_hashboard_presence(plan, context, bytes.clone()) {
                                Ok(presence) => am2_hashboard_presence.push(presence),
                                Err(error) => warn!(
                                    slot,
                                    error = %error,
                                    "AM2 planned controller slot EEPROM could not bind endpoint presence evidence"
                                ),
                            }
                        }
                        probes.push(classify_chain(slot, Some(&bytes)));
                    }
                    Err(OwnedEepromReadinessError::Timeout { .. }) => {
                        probes.push(ChainProbe::Timeout { chain_id: slot });
                    }
                    Err(OwnedEepromReadinessError::InvalidSlot { .. }) => {
                        // Programmatic bug — log + treat as read error
                        // (silently skipped by the gate so we never accidentally
                        // brick a healthy chain over a bookkeeping error).
                        probes.push(ChainProbe::ReadError { chain_id: slot });
                    }
                    Err(error @ OwnedEepromReadinessError::Terminal { .. }) => {
                        tracing::error!(
                            slot,
                            error = %error,
                            "AM2 hashboard identity service failed terminally; refusing energize"
                        );
                        force_am2_home_hard_stop(
                            &self.config,
                            "am2-hashboard-eeprom-service-terminal",
                        );
                        self.shutdown.cancel();
                        anyhow::bail!(
                            "AM2 hashboard identity service failed terminally on slot {slot}: {error}"
                        );
                    }
                }
            }
            info!(
                t_ms = t(),
                strict,
                accept_degraded,
                probes = ?probes,
                "[T+{}] Phase 0s: hashboard-SKU energize-gate probes",
                t()
            );
            match gate_chains_for_energize(&probes, strict) {
                Ok((bindings, telemetry)) => {
                    info!(
                        t_ms = t(),
                        chains = bindings.len(),
                        bindings = ?bindings,
                        "[T+{}] Phase 0s: energize gate ACCEPTED ({} chain(s) bound)",
                        t(),
                        bindings.len()
                    );
                    if !telemetry.is_empty() {
                        // Strict mode is OFF but reasons surfaced — log
                        // LOUDLY so the operator sees the first-deploy
                        // telemetry-only signal.
                        warn!(
                            t_ms = t(),
                            reasons = %telemetry.summary(),
                            "[T+{}] [ENERGIZE-REFUSED telemetry-only — would refuse if DCENT_AM2_STRICT_SKU_REFUSE=1] {}",
                            t(),
                            telemetry.summary()
                        );
                    }
                    // CE-011: persist the accepted bindings so the freq-only
                    // tuner spawn can tighten its frequency CEILING to the live
                    // hashboard's PVT envelope max (ceiling-only, never widens).
                    // Only the ACCEPTED arm records them; the accept_degraded
                    // override arm below leaves this empty (fail-closed).
                    self.accepted_sku_bindings = bindings;
                }
                Err(refusal) => {
                    if accept_degraded {
                        warn!(
                            t_ms = t(),
                            reasons = %refusal.summary(),
                            "[T+{}] [ENERGIZE-REFUSED but proceeding — DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1 lab override] {}",
                            t(),
                            refusal.summary()
                        );
                    } else {
                        tracing::error!(
                            t_ms = t(),
                            reasons = %refusal.summary(),
                            "[T+{}] [ENERGIZE-REFUSED] {}",
                            t(),
                            refusal.summary()
                        );
                        force_am2_home_hard_stop(
                            &self.config,
                            "am2-hashboard-sku-energize-refused",
                        );
                        self.shutdown.cancel();
                        anyhow::bail!(
                            "AM2 hashboard-SKU energize gate refused: {}",
                            refusal.summary()
                        );
                    }
                }
            }
        }

        // ====================================================================
        // Phase 0b (2026-05-22 XIL `a lab unit` recovery, Layer 3) — early fan
        // autoconfig + RPM gate, BEFORE the first dsPIC GET_VERSION.
        //
        // Bosminer ordering: fans-OK at T+22 well before first PIC opcode at
        // T+58. The legacy Phase 2c-pre location (after PIC GET_VERSION +
        // Phase 2b HBx_RESET) is reverse vs bosminer and means the C49→C52
        // `board-control` mode WRITE (inside `FanController::open_with_variant`)
        // happens AFTER PIC probe. Run the same fan-bring-up + RPM gate logic
        // here, BEFORE Phase 1, when `am2_fan_gate_before_pic` is true.
        //
        // When the early gate succeeds, the late Phase 2c-pre block below
        // becomes a no-op (we track via `early_fan_gate_passed`).
        //
        // §(b).
        // ====================================================================
        let mut early_fan_gate_passed = false;
        if !passthrough && self.config.mining.am2_fan_gate_before_pic {
            info!(
                t_ms = t(),
                "[T+{}] Phase 0b: early fan autoconfig + RPM gate (BEFORE PIC GET_VERSION — bosminer-faithful order)",
                t()
            );
            match open_am2_fan_controller("phase-0b-early-fan-gate") {
                Some((discovery, fan)) => {
                    let configured_max = self.config.thermal.fan_max_pwm;
                    let spin_pwm = configured_max
                        .min(dcentrald_hal::fan::PWM_MAX)
                        .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                    fan.set_speed(spin_pwm);
                    info!(
                        pwm = spin_pwm,
                        uio = discovery.uio_number,
                        configured_fan_max_pwm = configured_max,
                        t_ms = t(),
                        "[T+{}] Phase 0b: fans commanded to home cap PWM {} (no cold-boot burst); C49→C52 board-control mode flipped",
                        t(),
                        spin_pwm
                    );
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    let mut gate_cleared = false;
                    loop {
                        let max_rpm = fan
                            .get_per_fan_rpm()
                            .iter()
                            .map(|(_, r)| *r)
                            .max()
                            .unwrap_or(0);
                        if max_rpm >= 1800 {
                            info!(
                                max_rpm,
                                t_ms = t(),
                                "[T+{}] Phase 0b: early fans gate cleared at {} RPM",
                                t(),
                                max_rpm
                            );
                            gate_cleared = true;
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            warn!(
                                max_rpm,
                                t_ms = t(),
                                "[T+{}] Phase 0b: early fans <1800 RPM after 3 s — fail-closed before PIC GET_VERSION",
                                t()
                            );
                            force_am2_home_hard_stop(&self.config, "am2-fan-rpm-gate-failed-early");
                            self.shutdown.cancel();
                            anyhow::bail!(
                                "AM2 fan RPM gate (early Phase 0b) failed before PIC GET_VERSION: max_rpm={} after 3s",
                                max_rpm
                            );
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    if gate_cleared {
                        early_fan_gate_passed = true;
                    }
                }
                None => {
                    warn!(
                        t_ms = t(),
                        "[T+{}] Phase 0b: FanController open failed in early gate — fail-closed before PIC GET_VERSION",
                        t()
                    );
                    force_am2_home_hard_stop(&self.config, "am2-fan-uio-open-failed-early");
                    self.shutdown.cancel();
                    anyhow::bail!(
                        "AM2 fan-control UIO unavailable before PIC GET_VERSION (early Phase 0b)"
                    );
                }
            }
        }

        // ================================================================
        // Phase 1: PIC detect + enable  (Pic0x89, fw 0x89, RESET BANNED)
        // Phase 2: Fan bring-up (owned by fan controller — we just wait "Fans OK")
        // Phase 3: PIC SetOutputState per-chain 13.7 V
        // Phase 4: Chain reset (UART reset baud — no board-control writes per Agent 14)
        // Phase 5: Chain enumeration @ 115200
        // Phase 6: TicketMask → PLL → ADCControl → OpenDrain (inside init_asic_chain)
        // Phase 7: Baud upgrade to 3.125 Mbaud (inside init_asic_chain)
        // ================================================================
        let mut post_init_serial: Option<SerialChainBackend> = None;
        if !passthrough && !dspic_addr_active(active_chains, selected_pic_addr) {
            anyhow::bail!(
                "Configured AM2 dsPIC chain 0x{:02X} for {} is inactive; active mask=0b{:03b} active_addrs={:02X?}",
                selected_pic_addr,
                serial_device,
                active_chains,
                active_dspic_addrs(active_chains)
            );
        }
        let mut heartbeat_pic_fw: Option<u8> = None;
        let selected_pic_endpoint_required = am2_controller_plan.is_some();
        let mut selected_pic_endpoint_session: Option<Pic0x89EndpointSession> = None;
        let mut selected_pic_heartbeat_controller: Option<Pic0x89Service> = None;
        // : resolve the effective chain UART before all-active dsPIC
        // enable so a strict ACK failure on the target chain can block enum.
        // `selected_pic_addr` remains the selected PIC context derived from
        // TOML `serial_device`; this route is only for chain-side UART
        // observation and Phase 4-7 enum/work.
        if chain_uart_device != serial_device {
            info!(
                selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                pic_serial_device = %serial_device,
                chain_uart_device = %chain_uart_device,
                effective_chain_dspic_addr = ?effective_chain_dspic_addr
                    .map(|addr| format!("0x{:02X}", addr)),
                "Wave-55l: chain UART decoupled from selected_pic_addr via DCENT_AM2_CHAIN_UART_OVERRIDE"
            );
        }
        info!(
            selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
            selected_pic_serial_device = %serial_device,
            effective_chain_uart_device = %chain_uart_device,
            effective_chain_dspic_addr = ?effective_chain_dspic_addr
                .map(|addr| format!("0x{:02X}", addr)),
            chain_uart_override = chain_uart_device != serial_device,
            planned_chain_contexts = planned_chain_contexts.len(),
            "AM2 route selection: selected PIC context vs effective chain UART"
        );

        // The plan was admitted before hardware preparation. Log its typed
        // owner now that the selected controller route is known.
        if open_core_rail_plan.requires_demotion() {
            warn!(
                selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                open_core_mv = open_core_rail_plan.energization_mv,
                steady_mv = open_core_rail_plan.steady_mv,
                "AM2 open-core rail plan admitted for one selected controller; any demotion failure will terminal-safe-off every dsPIC and hard-stop PWR_CONTROL"
            );
        }

        // SWARM-FIX #4: arm the run-scope hard-stop guard's dsPIC-disable leg
        // now that the single-owner `/dev/i2c-0` service is up and the active
        // chain topology + selected dsPIC are resolved. Before this point the
        // guard's Drop only dropped PWR_CONTROL + fans, so a bare `?`
        // early-return in Phase 1-7 left the chain rail energized — the root
        // cause of "every standalone attempt needs a fresh AC-cycle". With the
        // teardown armed, ANY return path (incl. `?`) now disables voltage on
        // EVERY active dsPIC (incl. the effective chain dsPIC, e.g. 0x22) FIRST,
        // then drops PWR_CONTROL + fans (cut-hash-before-noise), making
        // iterative standalone A/B testable WITHOUT an AC-cycle between runs.
        //
        // `heartbeat_pic_fw` is not detected until Phase 1, so the selected-fw
        // hint is `None` here (the disable helper auto-detects per-addr; the
        // hint is only a decode optimisation, never required). Passthrough
        // never constructs this guard (`_home_hard_stop_guard == None`), so the
        // arming is a no-op there and bosminer keeps owning the PSU/chain.
        if let Some(guard) = _home_hard_stop_guard.as_mut() {
            if let Some(i2c0) = i2c0_service.as_ref() {
                // CE-012: arm the teardown with EVERY S19 dsPIC address
                // (0x20/0x21/0x22) UNCONDITIONALLY — not just the
                // active-mask subset — so every controller is de-energized on
                // teardown. This is strictly safer: if a chain was mis-probed
                // as inactive (silent fw=0x86 / transient bus NACK during the
                // Phase-0 probe) its rail would otherwise stay energized on a
                // bare `?` early-return and force the operator AC-cycle. The
                // disable is best-effort per-addr (`disable_dspic_addrs_best_effort`
                // tolerates a NACK from a physically-absent slot — e.g. 0x21 on
                // the `a lab unit` 2-board topology — and just logs a warn), so the
                // superset can never make teardown LESS safe. The active mask +
                // effective-chain dsPIC are still logged for diagnostics.
                let disable_addrs = S19_DSPIC_ADDRS.to_vec();
                guard.arm_dspic_teardown(i2c0, disable_addrs, selected_pic_addr, None);
                info!(
                    teardown_disable_addrs = format_args!("{:02X?}", S19_DSPIC_ADDRS),
                    active_dspic_addrs = format_args!(
                        "{:02X?}",
                        active_dspic_addrs(active_chains)
                    ),
                    selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                    effective_chain_dspic_addr = ?effective_chain_dspic_addr
                        .map(|addr| format!("0x{:02X}", addr)),
                    "CE-012: run-scope hard-stop guard armed to de-energize ALL S19 dsPIC controllers on teardown (no AC-cycle needed between standalone runs)"
                );
            }
        }

        if passthrough {
            info!("PASSTHROUGH: skipping Phase 1-7 (bosminer owns PIC+chain init)");
            match SerialChainBackend::open_passthrough(0, &serial_device) {
                Ok(mut s) => {
                    s.set_response_len(BM1362_RESP_BODY_LEN);
                    post_init_serial = Some(s);
                }
                Err(e) => {
                    warn!(error = %e, path = %serial_device, "Could not open passthrough serial probe backend")
                }
            }
        } else {
            info!(
                active_mask = format_args!("0b{:03b}", active_chains),
                "Phase 1: PIC init at I2C 0x{:02X} (active-chain gated)", selected_pic_addr
            );
            let pic_i2c = i2c0_service
                .as_ref()
                .context("AM2 I2C service missing for PIC init")?;
            let controller_presence = am2_hashboard_presence
                .iter()
                .find(|presence| presence.address() == selected_pic_addr)
                .with_context(|| {
                    format!(
                        "AM2 selected controller 0x{selected_pic_addr:02X} lacks exact board-target, canonical slot/UART, and positive EEPROM presence evidence"
                    )
                })?;
            let mut controller_endpoint: Option<VoltageControllerEndpoint> = None;

            // Read FW version via service-serialized write/sleep/read probes.
            // On .139 the observed FW is 0x86 (S19j framed variant); bosminer's
            // own drivers don't support 0x86 (only 0x88/0x89/0xB9/0xFE) which
            // is why bosminer crash-loops on this unit. Agent 21 confirmed
            // 0x86 and 0x89 share the same framed-protocol command bytes, so
            // our `Pic0x89::new_with_fw` accepts either and routes the right
            // firmware enum to `DspicController` internally.
            //
            // Changed 2026-04-24: fail hard if FW cannot be validated after
            // 3 retries. Previously we logged a warn and defaulted to 0x89
            // semantics; this silently proceeded with a PIC in an unknown
            // state and tonight produced garbage reads (0xFC, 0x18) that
            // reached `enable_voltage` — PIC may have ACK'd I²C without
            // actually setting the voltage DAC. Better to abort Phase 1 and
            // let the operator power-cycle than mine with no chain voltage.
            // 2026-04-26 .139 direct i2cget finding: 0x21 is alive and returns
            // fw=0x86 as a one-byte short/bare GET_VERSION response. The old
            // five-byte bulk read produced `[0x86, 0x0C, 0x18, 0x30, 0x60]`
            // because byte 0 was real and the xiic driver/bus synthesized the
            // shift-left tail after the dsPIC stopped driving data. 0x22, by
            // contrast, returns all 0xFF and is genuinely silent.
            //
            // BUT: bosminer mines successfully on this same unit at 69 TH/s
            // (per `bosminer.log` and `04-bosminer-runtime.md`). The chain is
            // physically powered when PSU rail is on. Voltage engagement may
            // be hardware-default rather than software-controlled on this
            // specific unit/board revision.
            //
            // If both framed and one-byte short probes fail, keep trust-rail
            // fallback lab-only. A silent or unvalidated PIC must not be
            // treated as production voltage control.
            // ============================================================
            // Phase 0d (2026-05-22 XIL `a lab unit` recovery, Layer 1 + Layer 3) —
            // bosminer-faithful dsPIC reset+start-app warmup BEFORE the first
            // GET_VERSION.
            //
            // When BOTH the config knob (`am2_dspic_warmup_before_get_version`,
            // default true) AND the env gate (`DCENT_AM2_PIC_RESET_AND_START_APP=1`,
            // default off) are set, we emit the bosminer-canonical chain via the
            // safe-by-construction `bosminer_warmup::am2_pic_reset_and_start_app_bosminer_faithful`
            // wrapper:
            //
            //   1. PARSER FLUSH — [0x55, 0xAA, 0x00] + 16 × 0x00  (19 wire bytes)
            //   2. RESET        — [0x55, 0xAA, 0x07] + 500 ms     ( 3 wire bytes)
            //   3. JUMP_TO_APP  — [0x55, 0xAA, 0x06] + 100 ms     ( 3 wire bytes)
            //
            // Then 5×1Hz idle heartbeats so Phase 3's `cold_boot_init` can skip
            // its internal 5-tick warmup loop (Layer 3 — see
            // `Pic0x89Service::cold_boot_init_with_options(skip_warmup_loop=true)`).
            //
            // The double-gate (config knob + env var) means the very first deploy
            // of this binary on `a lab unit` stays byte-identical to today's behaviour
            // (env var unset). Operator flips the env on `a lab unit` for the A/B
            // validation run; promote to env-off-by-default after success.
            //
            // The wrapper ALWAYS emits the 16-byte parser flush before the RESET
            // opcode — the `a lab unit` 2026-04-24 bare-RESET-without-flush corruption
            // pattern is structurally impossible here. The narrow restatement of
            //  (RESET without flush stays banned;
            // bosminer-canonical chain with flush is permitted) is the load-
            // bearing safety contract; see
            //  §3+§5.
            // ============================================================
            //  (2026-05-23) adds a 3rd warmup variant —
            // `am2_pic_reset_and_start_app_strace_derived` — the byte-exact
            // bosminer-plus-tuner 0.9.0 FRAMED protocol captured from a live
            // strace on `a lab unit`. It takes precedence over the S9-era
            // bare-3-byte variants when its env gate is set (the two warmup
            // chains are mutually exclusive — only one runs per Phase 0d).
            //  (2026-05-24): EEPROM bus-warmup BEFORE the dsPIC
            // warmup chain. Bosminer's `a lab unit` cold-boot strace shows ~20
            // bytes read from EEPROM 0x50 (and 0x52) on /dev/i2c-0
            // BEFORE addressing the dsPIC at 0x20. DCENT_OS skipped this
            // entirely ( strace evidence). Hypothesis: the EEPROM
            // bus activity wakes the dsPIC's MSSP I2C slave peripheral
            // into command-accepting mode. Without it the dsPIC stays in
            // CMD-echo mode and START_APP doesn't transition to app
            // firmware. Read-only (denylist preserved). Non-fatal —
            // missing EEPROM is logged but does not block dsPIC init.
            if am2_eeprom_bus_warmup_enabled() {
                for eeprom_addr in [0x50u8, 0x52u8] {
                    match bosminer_warmup::am2_eeprom_bus_warmup_read(pic_i2c, eeprom_addr) {
                        Ok(bytes) => {
                            let first_8: Vec<String> =
                                bytes.iter().take(8).map(|b| format!("{:02X}", b)).collect();
                            info!(
                                t_ms = t(),
                                eeprom_addr = format_args!("0x{:02X}", eeprom_addr),
                                bytes_read = bytes.len(),
                                first_8 = first_8.join(" "),
                                "[T+{}] Phase 0d (Wave-46): EEPROM bus warmup OK \
                                 (bus activity may have woken dsPIC FSM)",
                                t()
                            );
                        }
                        Err(e) => {
                            warn!(
                                eeprom_addr = format_args!("0x{:02X}", eeprom_addr),
                                error = %e,
                                "Phase 0d (Wave-46): EEPROM bus warmup read failed \
                                 (non-fatal — bus activity may still have woken dsPIC)"
                            );
                        }
                    }
                }
            }

            //  (2026-05-25) — LM75A passthrough warmup moved from
            // BEFORE the BARE warmup to AFTER (see block ending ~line 6147+).
            // Rationale: on cold-cold dsPIC fw=0x82 BOOTLOADER (the
            // tonight's DCENT_OS-from-NAND scenario), the LM75A opcodes
            // 0x3B/0x3C are not in the bootloader-sync opcode table —
            // the chip echoes CMD bytes back instead of forwarding them
            // to the LM75A sensors. The BARE warmup must transition the
            // chip from fw=0x82 BOOTLOADER → fw=0x82 APP MODE FIRST,
            // then the LM75A passthrough can warm the dsPIC MSSP FSM
            // for the subsequent Loki SetVoltage. See
            // PHASE2C-DSPIC-RAIL-FAILURE-RE.md + the
            // DCENT_RE/CE review docs at
            //
            //  Resolution (B) (2026-05-30, wf_7b37bed4 swarm fix): on a
            // TRUE cold-boot STANDALONE run (`DCENT_AM2_STANDALONE_RE_FIX=1`) on
            // `a lab unit`-class XIL hardware, fire the FRAMED warmup INTERNALLY even
            // when neither top-level `DCENT_AM2_PIC_RESET_*` var is set. Those 2
            // vars are gated by the  startup guard (they would require
            // the lab bypass `DCENT_BYPASS_WAVE54_GUARD=1`); this carve-out lets
            // the proven cold-cold FRAMED warmup chain run WITHOUT tripping the
            // guard, so the ZERO_PSU_BYTES standalone diagnostic
            // (`run_wave56_25_CONSOLIDATED_STANDALONE.sh`) can actually reach
            // dsPIC ENABLE + chain enum instead of aborting in bootloader.
            // Default-OFF + `a lab unit`-fingerprint-gated → zero effect on `a lab unit`, the
            //  bosminer-handoff, or any non-`a lab unit` platform. Uses FRAMED
            // (strace-derived) per the /i/j/k LIVE evidence that FRAMED
            // is the proven cold-cold path to fw=0x89 (BARE alone does NOT lift
            // the chip from fw=0x82 BOOTLOADER → fw=0x89 APP MODE). The
            // strace-derived warmup self-skips if the chip already reports a
            // known app-mode FW ( HIGH-2 precondition probe), so this
            // can never re-break an already-engaged chip.
            let standalone_re_fix_warmup =
                am2_standalone_re_fix_enabled() && am2_xil_25_fingerprint_matches();
            let warmup_did_run = if self.config.mining.am2_dspic_warmup_before_get_version
                && (am2_pic_reset_strace_derived_enabled()
                    || am2_pic_reset_and_start_app_enabled()
                    || standalone_re_fix_warmup)
            {
                // standalone-re-fix forces the FRAMED (strace-derived) chain.
                let use_strace = am2_pic_reset_strace_derived_enabled() || standalone_re_fix_warmup;
                let no_jump = !use_strace && am2_pic_reset_no_jump_enabled();
                let variant_label: &'static str =
                    if standalone_re_fix_warmup && !am2_pic_reset_strace_derived_enabled() {
                        "wave56-standalone-re-fix-framed"
                    } else if use_strace {
                        "wave28-strace-derived"
                    } else if no_jump {
                        "s9-era-bare-no-jump"
                    } else {
                        "s9-era-bare-with-jump"
                    };
                info!(
                    t_ms = t(),
                    addr = format_args!("0x{:02X}", selected_pic_addr),
                    variant = variant_label,
                    "[T+{}] Phase 0d: PIC reset+start-app warmup (variant={})",
                    t(),
                    variant_label
                );
                //  (2026-05-26) — when ALL_ACTIVE_PICS=1, warm up the
                // OTHER active dsPIC addresses (i.e. every active dsPIC except
                // `selected_pic_addr`) in ascending order, BEFORE the selected
                // one, so the bus is primed regardless of which PIC
                // `selected_pic_addr` points at. Observed on `a lab unit` overnight:
                // 0x22 reaches fw=0x89 FRAMED after the other-PIC warmups have
                // run. Best-effort, non-fatal: if a prime warmup fails, log +
                // continue to the selected one.
                //
                //  B05 CORRECTION (2026-05-28): do NOT read this as a
                // proven "0x20-first primes 0x22" dependency. With the real
                // /p config (TOML keeps serial_device=/dev/ttyS1 →
                // selected_pic_addr=0x20), this loop primes 0x21/0x22 and warms
                // the selected 0x20 LAST — the opposite of "0x20 first" — yet
                // 0x22 still reaches fw=0x89. The ordering dependency is
                // UNVERIFIED at runtime (most likely accumulated bus state from
                // ANY prior warmup, or per-slot health, not a specific 0x20→0x22
                // sequence).
                // §" B05 correction".
                if all_active_voltage_enable {
                    let other_pics =
                        am2_bus_prime_order(&active_dspic_addrs(active_chains), selected_pic_addr);
                    for prime_addr in other_pics {
                        //  Fix #2 (2026-05-29): cheap presence probe
                        // BEFORE the multi-retry warmup. `active_chains` is
                        // hardcoded to 0b111, so this loop would otherwise warm
                        // up the PHYSICALLY-ABSENT slot-2 dsPIC 0x21 on `a lab unit` →
                        // EIO storm that desyncs the AXI-IIC controller faster
                        // than the rate-limited (1/sec) fd-reopen can recover →
                        // the subsequent 0x20 sanity heartbeat EIOs and init
                        // aborts. Skip absent addresses (EIO or 0xFF) cleanly.
                        if !am2_dspic_present(pic_i2c, prime_addr) {
                            warn!(
                                t_ms = t(),
                                addr = format_args!("0x{:02X}", prime_addr),
                                "Phase 0d (Wave-55l): bus-prime skipping ABSENT dsPIC 0x{:02X} (presence probe EIO/0xFF) — avoids AXI-IIC poison",
                                prime_addr
                            );
                            continue;
                        }
                        info!(
                            t_ms = t(),
                            addr = format_args!("0x{:02X}", prime_addr),
                            "[T+{}] Phase 0d (Wave-55l): bus-prime warmup on 0x{:02X} BEFORE selected 0x{:02X}",
                            t(),
                            prime_addr,
                            selected_pic_addr
                        );
                        let prime_result = if use_strace {
                            // COLD-BYTE-DIFF Fix B: single-atomic-transaction form
                            // when env+`a lab unit`-fingerprint match, else N-transaction
                            // form (byte-identical either way).
                            am2_run_strace_derived_warmup(pic_i2c, prime_addr)
                        } else if no_jump {
                            bosminer_warmup::am2_pic_reset_only_bosminer_faithful(
                                pic_i2c, prime_addr,
                            )
                        } else {
                            bosminer_warmup::am2_pic_reset_and_start_app_bosminer_faithful(
                                pic_i2c, prime_addr,
                            )
                        };
                        match prime_result {
                            Ok(()) => info!(
                                addr = format_args!("0x{:02X}", prime_addr),
                                "Phase 0d (Wave-55l): bus-prime warmup on 0x{:02X} OK", prime_addr
                            ),
                            Err(e) => {
                                warn!(
                                    addr = format_args!("0x{:02X}", prime_addr),
                                    error = %e,
                                    "Phase 0d (Wave-55l): bus-prime warmup on 0x{:02X} FAILED (best-effort, continuing)",
                                    prime_addr
                                );
                                //  Fix #2: let the lazy I2cService
                                // fd-reopen (rate-limited to >1/sec) settle
                                // before the next address / the downstream
                                // 0x20 sanity heartbeat. Preserves the
                                // wave55l_bus_prime_failure_is_non_fatal
                                // "continue" contract — this is only a settle,
                                // not an abort.
                                std::thread::sleep(std::time::Duration::from_millis(300));
                            }
                        }
                    }
                }
                let warmup_call_result = if use_strace {
                    // COLD-BYTE-DIFF Fix B: single-atomic-transaction form when
                    // env+`a lab unit`-fingerprint match, else the proven N-transaction
                    // form (byte-and-timing-identical wire output either way).
                    am2_run_strace_derived_warmup(pic_i2c, selected_pic_addr)
                } else if no_jump {
                    bosminer_warmup::am2_pic_reset_only_bosminer_faithful(
                        pic_i2c,
                        selected_pic_addr,
                    )
                } else {
                    bosminer_warmup::am2_pic_reset_and_start_app_bosminer_faithful(
                        pic_i2c,
                        selected_pic_addr,
                    )
                };
                match warmup_call_result {
                    Ok(()) => {
                        //  (2026-05-23): under bosminer-faithful, skip
                        // the 5×1Hz heartbeats and emit a single ~500-ms sleep
                        // (matches bosminer's i2c-0 strace exactly between the
                        // START_APP ACK and the first GET_VERSION write).
                        //  live evidence on `a lab unit` run #15: 5/5 stable
                        // heartbeats succeed but the subsequent GET_VERSION
                        // NACKs with EIO — the heartbeat loop is the new
                        // suspected wedge.  tests that hypothesis.
                        if am2_dspic_bosminer_faithful_enabled() {
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 0d (Wave-42): warmup prelude emitted OK — bosminer-faithful skip 5×1Hz heartbeats; single 500 ms settle before GET_VERSION",
                                t()
                            );
                            std::thread::sleep(Duration::from_millis(500));
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 0d (Wave-42): 500 ms settle complete — proceeding to GET_VERSION",
                                t()
                            );
                            // Treat as warmup-OK so the downstream
                            // `cold_boot_init_with_skip(skip_warmup_loop=true)`
                            // runs once, exactly like the env-off path that
                            // just succeeded with the 5×1-s heartbeats. This
                            // is a strict subset of the prior behaviour (we
                            // already proved the dsPIC ACKs the warmup chain
                            // — what we're removing is the redundant 5 s of
                            // post-warmup heartbeats that the strace shows
                            // bosminer NEVER emits).
                            true
                        } else {
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 0d: warmup prelude emitted OK — running 5×1Hz idle heartbeats",
                                t()
                            );
                            // 5×1Hz idle heartbeats (no SetVoltage / no ENABLE).
                            // Constructs a transient Pic0x89Service in firmware-
                            // unknown mode; the heartbeat opcode is byte-identical
                            // across all known fw bytes for the framed-protocol
                            // family (cf. `dspic_heartbeat_frame`).
                            let mut warmup_pic = Pic0x89Service::new_with_fw(
                                pic_i2c.clone(),
                                selected_pic_addr,
                                None,
                            );
                            let mut warmup_ok = 0u32;
                            for tick in 1..=5 {
                                std::thread::sleep(Duration::from_secs(1));
                                match warmup_pic.send_heartbeat() {
                                    Ok(()) => {
                                        warmup_ok += 1;
                                        info!(tick, warmup_ok, "Phase 0d: idle heartbeat OK");
                                    }
                                    Err(e) => warn!(
                                        tick,
                                        error = %e,
                                        "Phase 0d: idle heartbeat failed (non-fatal, continuing)"
                                    ),
                                }
                            }
                            info!(
                                t_ms = t(),
                                warmup_ok,
                                "[T+{}] Phase 0d: warmup complete ({}/5 stable heartbeats) — proceeding to GET_VERSION",
                                t(),
                                warmup_ok
                            );
                            warmup_ok >= 5
                        }
                    }
                    Err(e) => {
                        // Warmup prelude failure is non-fatal — we still want
                        // Phase 1 to try GET_VERSION. Worst case: same state
                        // as no-prelude. Best case: prelude unblocked it.
                        warn!(
                            error = %e,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "Phase 0d: warmup prelude FAILED — falling through to legacy GET_VERSION (non-fatal)"
                        );
                        false
                    }
                }
            } else {
                if self.config.mining.am2_dspic_warmup_before_get_version {
                    tracing::debug!(
                        "Phase 0d: warmup config enabled but env gate DCENT_AM2_PIC_RESET_AND_START_APP \
                         is OFF — keeping byte-identical legacy behaviour (set env=1 to enable on this unit)"
                    );
                } else {
                    tracing::debug!(
                        "Phase 0d: warmup disabled by config (am2_dspic_warmup_before_get_version=false)"
                    );
                }
                false
            };

            // ====================================================================
            // Phase 0d-post (2026-05-25, REORDERED ) —
            // bosminer-faithful 17-tx LM75A passthrough warmup. Per
            // PHASE2C-DSPIC-RAIL-FAILURE-RE.md, bosminer's healthy cold-boot
            // trace to dsPIC slave 0x20 is dominated by 17 LM75A passthrough
            // transactions (opcodes 0x3B/0x3C, sensor addresses 0x48..0x4B).
            // Hypothesis: this polling warms the dsPIC's MSSP I²C handler
            // state machine so the subsequent Loki SetVoltage(0x83) actually
            // engages the chip rail — solving the chain-enum 0/126 failure
            // on cold-boot .25-class hardware.
            //
            //  ordering fix (2026-05-25): this block now runs AFTER
            // the Phase 0d BARE warmup, NOT before. On cold-cold dsPIC
            // fw=0x82 BOOTLOADER, opcodes 0x3B/0x3C are not in the
            // bootloader-sync opcode table — the chip echoes CMD bytes back
            // instead of forwarding them to LM75A sensors. The BARE warmup
            // must transition the chip from fw=0x82 BOOTLOADER → fw=0x82
            // APP MODE FIRST ( evidence), then the LM75A passthrough
            // can warm the MSSP FSM as the RE hypothesis describes. The
            //  live run on `a lab unit` worked despite this misordering
            // only because bosminer had already transitioned the chip to
            // fw=0x89 app mode; on tonight's true cold-cold scenario the
            // ordering matters.
            //
            // Gated by `DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH=1` (the
            // primitive's own env helper) OR by the umbrella
            // `DCENT_AM2_STANDALONE_RE_FIX=1` (the  compound flag
            // which also engages the Loki SetVoltage path + dsPIC SetVoltage
            // skip in Phase 3). Default-OFF — env unset → byte-identical to
            // pre- behavior on .79 / .109 / .135 / .129 / S9.
            //
            // Best-effort: failures are non-fatal (the LM75A sensors may not
            // be populated on .25; the function only escalates to Err if
            // more than half the 17 transactions fail, which would indicate
            // the dsPIC itself is wedged).
            // ====================================================================
            // 2026-06-07 (RE-018 true-cold decode): bosminer's cold sequence does
            // NOT emit any LM75 0x3B/0x3C passthrough between the framed JUMP and the
            // GET_VERSION — it goes flush→RESET→JUMP→GET_VER=0x89 directly. The 17-txn
            //  passthrough is a non-faithful step that may desync the 0x20
            // bootloader parser (0x20 ends echoing 0x82). DCENT_AM2_SKIP_LM75_PASSTHROUGH=1
            // lets the standalone launcher run the clean faithful path.
            let wave55f_lm75_warmup_active =
                (bosminer_warmup::am2_dspic_lm75_passthrough_enabled()
                    || (am2_standalone_re_fix_enabled() && am2_xil_25_fingerprint_matches()))
                    && !am2_env_flag("DCENT_AM2_SKIP_LM75_PASSTHROUGH");
            if wave55f_lm75_warmup_active {
                info!(
                    t_ms = t(),
                    selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                    tx_count = bosminer_warmup::LM75_PASSTHROUGH_TX_COUNT,
                    bare_warmup_did_run = warmup_did_run,
                    env_gate =
                        "DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH or DCENT_AM2_STANDALONE_RE_FIX",
                    "[T+{}] Phase 0d-post (Wave-55f): LM75A passthrough warmup ENGAGED — \
                     emitting 17 bosminer-faithful 0x3B/0x3C transactions to dsPIC 0x{:02X} \
                     AFTER BARE warmup (Wave-55g order). See PHASE2C-DSPIC-RAIL-FAILURE-RE.md.",
                    t(),
                    selected_pic_addr
                );
                match bosminer_warmup::am2_dspic_lm75_passthrough_warmup(pic_i2c, selected_pic_addr)
                {
                    Ok(()) => info!(
                        t_ms = t(),
                        selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                        "[T+{}] Phase 0d-post (Wave-55f): LM75A passthrough warmup OK \
                         (dsPIC FSM warmed; subsequent Loki SetVoltage should engage chip rail)",
                        t()
                    ),
                    Err(e) => warn!(
                        selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                        error = %e,
                        "Phase 0d-post (Wave-55f): LM75A passthrough warmup FAILED \
                         (continuing best-effort — chain-enum will reveal if dsPIC was wedged)"
                    ),
                }
            }

            // ====================================================================
            // Ghidra-RE PART B (2026-05-29, DCENT_AM2_DSPIC_SENSOR_ONLY) —
            // also warm the EFFECTIVE-CHAIN dsPIC's I²C FSM via LM75A
            // passthrough. Per
            //
            // (Ghidra static RE of bosminer.bin): bosminer uses the dsPIC ONLY
            // for LM75A sensor passthrough (0x3B/0x3C ×17), which warms the
            // chip's MSSP I²C slave FSM. dcentrald only ran that warmup on the
            // SELECTED pic (0x20), not the effective-CHAIN pic (0x22) that the
            // chain UART actually maps to — so the chain dsPIC's parser stayed
            // cold and echoed its FW byte (0x8A) to everything.
            //
            // When `DCENT_AM2_DSPIC_SENSOR_ONLY=1` AND there is an
            // effective-chain dsPIC addr that DIFFERS from the selected addr,
            // ALSO emit the 17-tx LM75A passthrough on that chain addr so its
            // FSM gets warmed too. Best-effort / non-fatal (log on error,
            // continue) — the chain enumeration downstream is the real proof.
            //
            // Default-OFF: when the env gate is unset this block is inert and
            // the chain-pic warmup is not added (fleet byte-identical).
            // ====================================================================
            if bosminer_warmup::am2_dspic_sensor_only_enabled() {
                if let Some(chain_addr) = effective_chain_dspic_addr {
                    if chain_addr != selected_pic_addr {
                        info!(
                            t_ms = t(),
                            selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                            effective_chain_dspic_addr = format_args!("0x{:02X}", chain_addr),
                            tx_count = bosminer_warmup::LM75_PASSTHROUGH_TX_COUNT,
                            env_gate = "DCENT_AM2_DSPIC_SENSOR_ONLY",
                            "[T+{}] Ghidra-RE PART B: SENSOR_ONLY — also warming the \
                             effective-chain dsPIC 0x{:02X} via LM75A passthrough \
                             (selected pic 0x{:02X} already warmed; bosminer warms the \
                             chain dsPIC's FSM the same way). \
                             ",
                            t(),
                            chain_addr,
                            selected_pic_addr
                        );
                        match bosminer_warmup::am2_dspic_lm75_passthrough_warmup(
                            pic_i2c, chain_addr,
                        ) {
                            Ok(()) => info!(
                                t_ms = t(),
                                effective_chain_dspic_addr = format_args!("0x{:02X}", chain_addr),
                                "[T+{}] Ghidra-RE PART B: chain dsPIC 0x{:02X} LM75A \
                                 passthrough warmup OK (chain FSM warmed)",
                                t(),
                                chain_addr
                            ),
                            Err(e) => warn!(
                                effective_chain_dspic_addr =
                                    format_args!("0x{:02X}", chain_addr),
                                error = %e,
                                "Ghidra-RE PART B: chain dsPIC LM75A passthrough warmup \
                                 FAILED (non-fatal — continuing; chain-enum reveals if \
                                 the chain dsPIC was wedged)"
                            ),
                        }
                    } else {
                        tracing::debug!(
                            t_ms = t(),
                            selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                            "Ghidra-RE PART B: SENSOR_ONLY set but effective-chain dsPIC \
                             addr == selected addr (0x{:02X}) — chain pic already warmed, \
                             no extra warmup needed",
                            selected_pic_addr
                        );
                    }
                } else {
                    tracing::debug!(
                        t_ms = t(),
                        "Ghidra-RE PART B: SENSOR_ONLY set but no effective-chain dsPIC \
                         addr resolved from the chain UART device — skipping chain-pic warmup"
                    );
                }
            }

            let allow_trust_rail = am2_env_flag("DCENT_AM2_TRUST_RAIL_FALLBACK");
            let detected_fw_result = observe_am2_endpoint_firmware(
                pic_i2c,
                controller_presence,
                &mut controller_endpoint,
            );
            let detected_fw_opt = match detected_fw_result {
                Ok(fw) => Some(fw),
                Err(e) if allow_trust_rail => {
                    warn!(
                        error = %e,
                        "DCENT_AM2_TRUST_RAIL_FALLBACK enabled; continuing after dsPIC GET_VERSION failure"
                    );
                    None
                }
                Err(e) => {
                    force_am2_home_hard_stop(&self.config, "pic-get-version-failed");
                    teardown_am2_power_after_failed_pic_preflight(
                        &self.config,
                        &mut runtime_threads,
                        &psu_arc,
                    )
                    .await;
                    anyhow::bail!(
                        "PIC GET_VERSION failed at 0x{:02X}: dsPIC ACKs but returned no valid framed/short response. \
                         Refusing trust-rail fallback by default because production voltage control requires a validated PIC firmware byte. \
                         Set DCENT_AM2_TRUST_RAIL_FALLBACK=1 for a lab-only blind chain probe. error: {}",
                        selected_pic_addr,
                        e
                    );
                }
            };
            let mut detected_fw = match detected_fw_opt {
                Some(fw) => {
                    info!(
                        fw = format_args!("0x{:02X}", fw),
                        t_ms = t(),
                        "[T+{}] dsPIC fw_byte=0x{:02X} (validated against known dsPIC firmware byte set)",
                        t(),
                        fw,
                    );
                    // D8-6 (2026-06-13): cold-precondition gate. On a TRUE cold-boot
                    // standalone run (!allow_trust_rail) the `a lab unit`/.109-class dsPIC
                    // should report fw=0x82 (BARE bootloader) at the FIRST GET_VERSION
                    // (dual-state model). If it instead reports an app-mode fw
                    // (0x89/0x8A) here, the chip was ALREADY engaged by a prior
                    // bosminer boot (WARM) — so any subsequent enum>0 is a FALSE
                    // standalone positive, not proof DCENT_OS woke the chain itself.
                    // Flag the run loudly as INVALID-FOR-STANDALONE-CLAIM. Additive
                    // log only — does not gate the run (the operator may still want
                    // diagnostics); the trust decision is the human's.
                    if !allow_trust_rail
                        && am2_xil_25_fingerprint_matches()
                        && matches!(fw, 0x89 | 0x8A)
                    {
                        warn!(
                            fw = format_args!("0x{:02X}", fw),
                            "D8-6 COLD-PRECONDITION NOT MET: standalone run but dsPIC reports app-mode fw=0x{:02X} at the FIRST GET_VERSION — the chip is bosminer-pre-engaged (WARM), not cold. Any enum>0 from THIS run is INVALID-FOR-STANDALONE-CLAIM. AC-cycle + boot the DCENT_OS slot for a true cold (fw=0x82) standalone proof.",
                            fw
                        );
                    }
                    fw
                }
                None => {
                    warn!(
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        "LAB-ONLY TRUST-THE-RAIL fallback: PIC GET_VERSION failed, so dsPIC SetVoltage is skipped and ENABLE_VOLTAGE is attempted best-effort before chain UART enumeration."
                    );
                    // 2026-04-29: Before defaulting to 0x89 framing, try a
                    // true 1-byte dumy_read. fw=0x86 chips reliably echo
                    // their FW byte on bare reads even when the framed and
                    // short GET_VERSION probes both return kernel xiic
                    // bus-noise. Using the recovered FW byte routes the
                    // ENABLE call through the correct bare/framed path
                    // (firmware.protocol() in dspic.rs:711), so a fw=0x86
                    // chip gets the bare 4-byte ENABLE frame [55 AA 15 01]
                    // instead of a framed 6-byte form it cannot parse.
                    match pic_dumy_read_fw_byte(pic_i2c, selected_pic_addr) {
                        Ok(fw) => {
                            warn!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                fw = format_args!("0x{:02X}", fw),
                                "TRUST-RAIL: dumy_read recovered FW byte; using detected protocol"
                            );
                            fw
                        }
                        Err(e) => {
                            warn!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                error = %e,
                                "TRUST-RAIL: dumy_read also failed; defaulting to 0x89 framing (best-effort)"
                            );
                            // Use 0x89 as nominal so the best-effort ENABLE
                            // frame uses the Pic0x89 framing while
                            // SetVoltage remains skipped.
                            0x89
                        }
                    }
                }
            };

            // ====================================================================
            // Rung 2 — bounded re-verify when cold-engage left dsPIC in fw=0x82
            // (2026-06-07, `a lab unit` standalone). TWO variants, RESET→JUMP precedence:
            //   (a) RESET→JUMP variant (LIVE TEST 2, the stronger lever) — full
            //       flush→RESET→(longer dwell)→JUMP ×N, env
            //       DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX.
            //   (b) JUMP-only variant (the original rung 2) — flush→JUMP ONLY ×N,
            //       NEVER a 2nd RESET, env DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX.
            // ====================================================================
            // LIVE evidence (`a lab unit` standalone, post commit 0a1bfa5a clean-path +
            // 2-read ack drain + commit 8a9113b8 heartbeat keep-alive): the cold
            // dsPIC 0x20 REACHED fw=0x89 once (run 3) — proving the
            // flush→RESET→JUMP sequence works — but the JUMP→0x89 transition is
            // INTERMITTENT. Subsequent cold-engages (incl. a fresh AC-cycle) read
            // fw=0x82. The JUMP-only re-verify (×6) did NOT transition the chip
            // (LIVE TEST 2 run 6). The strongest hypothesis (SESSION.md §"NEXT-
            // SESSION reliability frontier" #1) is that the chip needs MULTIPLE
            // FULL RESET→JUMP cycles — the only 0x89 ever seen was effectively the
            // 3rd RESET→JUMP cycle, and JUMP-only can't prime because it omits the
            // RESET. So the RESET→JUMP variant runs the full flush→RESET→JUMP ×N
            // (with a longer post-RESET dwell), and TAKES PRECEDENCE over the
            // JUMP-only variant when both envs are set.
            //
            // SAFETY (bible reconciliation): the "never abandon a chip in 0x82
            // after a RESET" invariant is about not DOWNGRADING a *working* fw=0x89
            // chip. Both variants here only run when GET_VERSION already read
            // fw=0x82 (the cold bootloader), so re-cycling RESET→JUMP to bring a
            // cold-0x82 chip UP to 0x89 is non-destructive (an AC-cycle resets it
            // the same way). The JUMP-only "never a 2nd RESET" was written for the
            // *jumped-then-fell-back* case, a DIFFERENT failure than our
            // never-transitions case (SESSION.md §"reliability frontier" #1).
            //
            // Default-OFF + `a lab unit`-fingerprinted: each variant only runs when its
            // env > 0 AND the `a lab unit` fingerprint matches AND the cold-engage left
            // the chip in fw=0x82. With both envs unset, neither loop is entered
            // and the fleet/handoff/legacy paths are byte-identical. Both reuse the
            // same single-owner I2cServiceHandle (`pic_i2c`). On exhaustion both
            // fail-closed exactly as today (continue with fw=0x82; downstream
            // cold_boot_init / chain enum decides — chip stays in recoverable 0x82).
            let reset_jump_reverify_max =
                bosminer_warmup::am2_dspic_reset_jump_reverify_max().min(16);
            let jump_reverify_max = bosminer_warmup::am2_dspic_jump_reverify_max().min(16);
            if reset_jump_reverify_max > 0
                && am2_xil_25_fingerprint_matches()
                && detected_fw == 0x82
            {
                // (a) RESET→JUMP variant — precedence over JUMP-only.
                let reset_dwell_ms = bosminer_warmup::am2_dspic_reset_dwell_ms();
                info!(
                    t_ms = t(),
                    addr = format_args!("0x{:02X}", selected_pic_addr),
                    reset_jump_reverify_max,
                    reset_dwell_ms,
                    "[T+{}] Rung 2 (RESET→JUMP): cold-engage left dsPIC in fw=0x82 (bootloader) — \
                     entering bounded FULL flush→RESET→JUMP re-verify (post-RESET dwell {}ms) up to \
                     {} cycle(s) to reach fw=0x89 (LIVE TEST 2: JUMP-only ×6 did not transition; the \
                     only 0x89 was effectively the 3rd full RESET→JUMP cycle)",
                    t(),
                    reset_dwell_ms,
                    reset_jump_reverify_max
                );
                for attempt in 1..=reset_jump_reverify_max {
                    match bosminer_warmup::am2_pic_reset_jump_reverify(
                        pic_i2c,
                        selected_pic_addr,
                        reset_dwell_ms,
                    ) {
                        Ok(()) => info!(
                            t_ms = t(),
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            reset_jump_reverify_max,
                            "[T+{}] Rung 2 (RESET→JUMP) attempt {}/{}: flush→RESET→JUMP re-issued; re-reading GET_VERSION",
                            t(),
                            attempt,
                            reset_jump_reverify_max
                        ),
                        Err(e) => warn!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            error = %e,
                            "Rung 2 (RESET→JUMP) attempt {}/{}: re-verify emit failed (non-fatal); re-reading GET_VERSION anyway",
                            attempt,
                            reset_jump_reverify_max
                        ),
                    }
                    match observe_am2_endpoint_firmware(
                        pic_i2c,
                        controller_presence,
                        &mut controller_endpoint,
                    ) {
                        Ok(fw) => {
                            info!(
                                t_ms = t(),
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                attempt,
                                fw = format_args!("0x{:02X}", fw),
                                "[T+{}] Rung 2 (RESET→JUMP) attempt {}/{}: GET_VERSION fw_byte=0x{:02X}",
                                t(),
                                attempt,
                                reset_jump_reverify_max,
                                fw
                            );
                            detected_fw = fw;
                            if fw == 0x89 {
                                info!(
                                    t_ms = t(),
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    attempt,
                                    "[T+{}] Rung 2 (RESET→JUMP): dsPIC reached fw=0x89 (FRAMED app mode) after {} full RESET→JUMP cycle(s) — proceeding to cold_boot_init",
                                    t(),
                                    attempt
                                );
                                break;
                            }
                            if fw != 0x82 {
                                // Unexpected fw (e.g. 0x86 degraded) — re-cycling
                                // won't help and the downstream corruption gate
                                // handles non-0x82/0x89 firmware; stop re-cycling.
                                warn!(
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    fw = format_args!("0x{:02X}", fw),
                                    "Rung 2 (RESET→JUMP): GET_VERSION read non-0x82/non-0x89 fw — stopping re-verify (downstream fw gate handles it)"
                                );
                                break;
                            }
                        }
                        Err(e) => warn!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            error = %e,
                            "Rung 2 (RESET→JUMP) attempt {}/{}: GET_VERSION re-read failed (non-fatal); keeping fw=0x82 and retrying if cycles remain",
                            attempt,
                            reset_jump_reverify_max
                        ),
                    }
                }
                if detected_fw != 0x89 {
                    warn!(
                        t_ms = t(),
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        reset_jump_reverify_max,
                        detected_fw = format_args!("0x{:02X}", detected_fw),
                        "[T+{}] Rung 2 (RESET→JUMP): re-verify exhausted {} cycle(s) without reaching fw=0x89; continuing with fw=0x{:02X} (fail-closed exactly as today — chip stays in recoverable 0x82 bootloader)",
                        t(),
                        reset_jump_reverify_max,
                        detected_fw
                    );
                }
            } else if jump_reverify_max > 0
                && am2_xil_25_fingerprint_matches()
                && detected_fw == 0x82
            {
                info!(
                    t_ms = t(),
                    addr = format_args!("0x{:02X}", selected_pic_addr),
                    jump_reverify_max,
                    "[T+{}] Rung 2: cold-engage left dsPIC in fw=0x82 (bootloader) — \
                     entering bounded JUMP-only re-verify (flush → framed JUMP, NEVER a \
                     2nd RESET) up to {} attempt(s) to reach fw=0x89",
                    t(),
                    jump_reverify_max
                );
                for attempt in 1..=jump_reverify_max {
                    match bosminer_warmup::am2_pic_jump_only_reverify(pic_i2c, selected_pic_addr) {
                        Ok(()) => info!(
                            t_ms = t(),
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            jump_reverify_max,
                            "[T+{}] Rung 2 attempt {}/{}: flush → framed JUMP re-issued (no RESET); re-reading GET_VERSION",
                            t(),
                            attempt,
                            jump_reverify_max
                        ),
                        Err(e) => warn!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            error = %e,
                            "Rung 2 attempt {}/{}: JUMP-only re-verify emit failed (non-fatal); re-reading GET_VERSION anyway",
                            attempt,
                            jump_reverify_max
                        ),
                    }
                    match observe_am2_endpoint_firmware(
                        pic_i2c,
                        controller_presence,
                        &mut controller_endpoint,
                    ) {
                        Ok(fw) => {
                            info!(
                                t_ms = t(),
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                attempt,
                                fw = format_args!("0x{:02X}", fw),
                                "[T+{}] Rung 2 attempt {}/{}: GET_VERSION fw_byte=0x{:02X}",
                                t(),
                                attempt,
                                jump_reverify_max,
                                fw
                            );
                            detected_fw = fw;
                            if fw == 0x89 {
                                info!(
                                    t_ms = t(),
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    attempt,
                                    "[T+{}] Rung 2: dsPIC reached fw=0x89 (FRAMED app mode) after {} JUMP-only re-verify attempt(s) — proceeding to cold_boot_init",
                                    t(),
                                    attempt
                                );
                                break;
                            }
                            if fw != 0x82 {
                                // Unexpected fw (e.g. 0x86 degraded) — re-JUMPing
                                // won't help and the downstream corruption gate
                                // handles non-0x82/0x89 firmware; stop re-JUMPing.
                                warn!(
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    fw = format_args!("0x{:02X}", fw),
                                    "Rung 2: GET_VERSION read non-0x82/non-0x89 fw — stopping JUMP-only re-verify (downstream fw gate handles it)"
                                );
                                break;
                            }
                        }
                        Err(e) => warn!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            attempt,
                            error = %e,
                            "Rung 2 attempt {}/{}: GET_VERSION re-read failed (non-fatal); keeping fw=0x82 and retrying if attempts remain",
                            attempt,
                            jump_reverify_max
                        ),
                    }
                }
                if detected_fw != 0x89 {
                    warn!(
                        t_ms = t(),
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        jump_reverify_max,
                        detected_fw = format_args!("0x{:02X}", detected_fw),
                        "[T+{}] Rung 2: JUMP-only re-verify exhausted {} attempt(s) without reaching fw=0x89; continuing with fw=0x{:02X} (fail-closed exactly as today — chip stays in recoverable 0x82 bootloader)",
                        t(),
                        jump_reverify_max,
                        detected_fw
                    );
                }
            }

            let trust_rail = detected_fw_opt.is_none();

            // FW86 uses bare commands while FW89 uses framed commands.
            // GET_VERSION probing never emits RESET/JUMP; fw0x86 short-mode
            // reads exactly one byte to avoid xiic bulk-read tail artifacts.

            // CORRUPTION-PREVENTION GATE (2026-04-29): refuse to issue
            // voltage commands to a dsPIC reporting fw=0x86. fw=0x86 is the
            // PROVEN corruption state from a 2026-04-24 PIC RESET on .139:
            //   - bosminer-plus-tuner 0.9.0 rejects with `{ERR:V3} unexpected
            //     power controller version 134` and crash-loops (live-verified
            //     2026-04-29 via slot-flip back to BraiinsOS+).
            //   - DCENT_OS bare-protocol ENABLE returns clean ACK but the
            //     DC-DC rail does NOT engage (chain UART RX = 0, GetAddress
            //     = 0 chips — live-verified 2026-04-29 with the new
            //     dumy_read fallback).
            //   - No production firmware (bosminer/VNish/Bitmain stock)
            //     supports fw=0x86. Recovery requires physical ICSP.
            // Override for lab/research mode: DCENT_AM2_TRUST_DEGRADED_FW=1.
            //.
            if !detected_dspic_fw_allows_voltage_commands(
                detected_fw,
                dspic_fw86_trust_degraded_override_enabled(),
            ) {
                teardown_am2_power_after_failed_pic_preflight(
                    &self.config,
                    &mut runtime_threads,
                    &psu_arc,
                )
                .await;
                anyhow::bail!(
                    "PIC 0x{:02X}: {}",
                    selected_pic_addr,
                    dspic_voltage_refusal_detail("AM2 cold boot voltage enable")
                );
            }

            heartbeat_pic_fw = Some(detected_fw);
            let endpoint = controller_endpoint.take().with_context(|| {
                format!(
                    "AM2 controller 0x{selected_pic_addr:02X} has no chain-plan-issued endpoint; trust-rail/model/address fallback cannot construct the voltage owner"
                )
            })?;
            if endpoint.observed_firmware() != Some(detected_fw) {
                anyhow::bail!(
                    "AM2 controller endpoint firmware {:?} does not match the final observed firmware 0x{detected_fw:02X}",
                    endpoint.observed_firmware()
                );
            }
            selected_pic_endpoint_session = Some(
                Pic0x89EndpointSession::new(pic_i2c.clone(), endpoint)
                    .context("failed to bind chain-plan-issued AM2 Pic0x89 endpoint")?,
            );
            selected_pic_heartbeat_controller = Some(
                selected_pic_endpoint_session
                    .as_ref()
                    .expect("just-installed AM2 Pic0x89 endpoint session")
                    .controller(),
            );
            let pic = selected_pic_endpoint_session
                .as_mut()
                .expect("just-installed AM2 Pic0x89 endpoint session")
                .controller_mut();

            // WAKE-DSPIC-BEFORE-RAIL (deferred rail-on, bosminer block B). The dsPIC
            // has now booted its SMPS app (RESET->JUMP->fw=0x89, Phase 0d + Rung-2
            // re-verify) on 3.3 V standby with the main 12.8 V rail OFF (block A). NOW
            // assert the main rail, THEN fall into Phase 2b/EBR which holds HB_RESET LOW
            // and runs the dsPIC ENABLE -- so the SMPS controller soft-starts into the
            // chips with its input applied AFTER its app initialized input-absent,
            // byte-for-byte matching bosminer's cold order. The gate is bound to
            // `_psu_bypass_gate` so Drop still de-energizes on teardown; fail-closed.
            if let Some((spec, model, rail_v)) = deferred_rail_assert.take() {
                let gate = PsuBypassGate::assert(spec.as_deref(), model.clone(), rail_v)
                    .context("WAKE-DSPIC-BEFORE-RAIL: deferred PWR_CONTROL rail assert failed")?;
                info!(
                    model = %model,
                    rail_v,
                    gpio = gate.gpio(),
                    t_ms = t(),
                    "[T+{}] WAKE-DSPIC-BEFORE-RAIL: main 12.8 V rail ASSERTED now (bosminer \
                     block-B order) — dsPIC already at fw=0x89 from the standby warmup; Phase \
                     2b/EBR ENABLE follows",
                    t()
                );
                _psu_bypass_gate = Some(gate);
                let settle_ms = am2_env_u64("DCENT_AM2_POST_APW_SETTLE_MS", 1000).clamp(200, 3000);
                std::thread::sleep(Duration::from_millis(settle_ms));
            }

            // Phase 2b: HBx_RESET pulse on the populated hashboard slot.
            //
            // Bosminer's am2 cold_boot logs `CHAIN/N: Resetting hash board`
            // here (between PSU enable and PIC enable_voltage), pulsing the
            // hashboard reset GPIO active-LOW for ~10 ms then releasing HIGH.
            // Without this, the BM1362 chips stay in their last reset state —
            // which after multiple init cycles is "wedged, not responding to
            // GetAddress", exactly what we see (0 chip replies on all 4 ttyS).
            //
            // chain1 → phys slot 2, chain4 → phys slot 3 (per the
            // `relay_phys_idx` map at the top of `run`). The devmem-based
            // pulse helper at `dcentrald-hal/src/board_control.rs:306`
            // preserves PWR_CONTROL HIGH (gpio:907) across the pulse via
            // RMW so the PSU stays armed.
            //
            // (DCENT_RE / Codex 2026-04-25 — confirmed via bosminer wire
            // trace `12-bosminer-startup-timeline.md:80,85,94,95`.)
            // Pulse ALL 4 HB resets — the configured `relay_phys_idx` is the
            // FPGA chain index (chain1=2, chain4=3), but the BoardControl
            // slot↔hashboard physical wiring is not guaranteed to match. By
            // pulsing all 4 we cover both populated chains regardless of
            // which slot the operator's hashboards are physically plugged
            // into. All 4 resets are independent GPIOs (gpio-897..900);
            // pulsing an unpopulated slot is a no-op.
            info!(
                t_ms = t(),
                "[T+{}] Phase 2b: Pulsing HB0..3_RESET (~20 ms LOW → HIGH each) before voltage enable",
                t()
            );
            let doing_bosminer_faithful_reset = am2_env_flag("DCENT_AM2_HB_RESET_REPOINT")
                || am2_env_flag("DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL");
            // CE-003: discover the board-control UIO by name instead of hardcoding 17.
            match BoardControl::open(board_control_uio_number()) {
                Ok(bc) => {
                    for slot in 0u8..=3 {
                        match bc.pulse_reset(slot) {
                            Ok(()) => info!(
                                t_ms = t(),
                                slot,
                                duration_ms = 20,
                                chain_id = fpga_chain_id,
                                "[T+{}] HBx_RESET pulse {}ms LOW chain={} (slot {})",
                                t(),
                                20,
                                fpga_chain_id,
                                slot
                            ),
                            Err(e) => warn!(
                                error = %e,
                                slot,
                                "HB{}_RESET pulse FAILED (slot may be unpopulated/unwired)",
                                slot
                            ),
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    std::thread::sleep(Duration::from_millis(500));

                    // ============================================================
                    // Phase 2b-extended: S9-pattern long reset hold (;
                    // devmem rewrite ).
                    //
                    // S9 BraiinsOS holds each chain in UART BREAK reset for ~4 s
                    // before any UART traffic (BM1387 `init_and_split` pattern,
                    // bm1387.rs:650-664). am2 hybrid's `pulse_reset()` only
                    // holds LOW for ~20 ms, which may be too short for BM1362
                    // to flush previous-firmware state cleanly.
                    //
                    // ORIGINAL (broken on `a lab unit`): this used raw
                    // `std::fs::write("/sys/class/gpio/gpio{897+slot}/value", ..)`
                    // to drive the long hold. On `a lab unit` gpio-898/900 are kernel-
                    // claimed and NOT sysfs-exportable → ENOENT → the hold never
                    // fired → chips never reset → chain enum returned 0. The
                    // proven mechanism is the AXI-GPIO mmap at 0x41210000 that
                    // `pulse_reset()`'s 20 ms pulse already succeeds through on
                    // the same run.  parameterized that devmem hold and
                    // exposed `bc.hold_resets_devmem(&slots, hold_ms)` which
                    // holds all the given slots LOW simultaneously and releases
                    // them together, preserving PWR_CONTROL (gpio-901) HIGH.
                    // Order is now:
                    //   1. Standard pulse_reset() above (FPGA-side bit + 20 ms)
                    //   2. Drive LOW + hold am2_reset_hold_ms + release HIGH, all
                    //      via devmem (AXI-GPIO 0x41210000), all 4 slots at once.
                    // The Phase 2b-post readback (below) is best-effort/diagnostic.
                    // ============================================================
                    let reset_hold_ms = self.config.mining.am2_reset_hold_ms;
                    //  (2026-05-29) handoff-safety gate: on the
                    // bosminer-handoff path (DCENT_AM2_TRUST_RAIL_FALLBACK=1) the
                    // BM1362 chips are ALREADY out of reset + enumerated by
                    // bosminer. Now that the long HB_RESET actually fires via
                    // devmem (it previously no-op'd through a failing sysfs write
                    // on `a lab unit` — gpio-898/900 unexportable), a multi-second reset
                    // here would re-reset bosminer's engaged chips and break the
                    // proven handoff ( recipe: "must not disturb the
                    // engaged state"). So skip the long hold when the rail is
                    // trusted-from-bosminer; the short ~20 ms Phase-2b pulse above
                    // is harmless and still runs. Standalone cold-boot (no
                    // TRUST_RAIL_FALLBACK) still gets the full reset hold it needs.
                    let handoff_rail_trusted = am2_env_flag("DCENT_AM2_TRUST_RAIL_FALLBACK");
                    // R5-5D gap-5 (2026-05-31): TRUST_RAIL_FALLBACK must NOT
                    // suppress the long reset hold when we are EXPLICITLY doing the
                    // bosminer-faithful cold reset. The `a lab unit` standalone recipe sets
                    // DCENT_AM2_TRUST_RAIL_FALLBACK=1 (it is NOT the
                    // standalone-vs-handoff discriminator — see the
                    // HB_RESET_BOSMINER_FAITHFUL note below), so the previous
                    // `handoff_rail_trusted`-only skip silently dropped the long
                    // reset hold on the very standalone cold-wake path that needs
                    // it. When the operator has armed the sysfs re-point
                    // (DCENT_AM2_HB_RESET_REPOINT) or the bosminer-faithful retry
                    // (DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL), we are deliberately
                    // re-resetting cold chips — keep the hold. The  handoff
                    // recipe sets NEITHER of those, so it still skips (chips already
                    // engaged by bosminer, must not re-reset).
                    let reset_slots: &[u8] =
                        if doing_bosminer_faithful_reset && am2_xil_25_fingerprint_matches() {
                            &[0, 2]
                        } else {
                            &[0, 1, 2, 3]
                        };
                    if doing_bosminer_faithful_reset && reset_hold_ms < 1800 {
                        force_pwr_control_low(
                            self.config.psu.pwr_control_gpio.as_deref(),
                            "hb-reset-hold-too-short",
                        );
                        anyhow::bail!(
                            "Phase 2b-extended: faithful HB_RESET hold requires am2_reset_hold_ms >= 1800, got {}",
                            reset_hold_ms
                        );
                    }
                    if reset_hold_ms > 20 && handoff_rail_trusted && !doing_bosminer_faithful_reset
                    {
                        info!(
                            t_ms = t(),
                            reset_hold_ms,
                            "[T+{}] Phase 2b-extended: SKIPPED long reset hold on bosminer-handoff path (DCENT_AM2_TRUST_RAIL_FALLBACK=1) — chips already engaged by bosminer, must not re-reset",
                            t()
                        );
                    } else if reset_hold_ms > 20 {
                        // RE-018 ENABLE-BEFORE-RESET-RELEASE (2026-06-09): the in-tree
                        // cold bosminer strace PROVES (both populated chains) that
                        // ENABLE_VOLTAGE (0x15) fires WHILE HB_RESET is held LOW and
                        // reset releases ~0.3-0.55 s AFTER the chip rail is up — so the
                        // BM1362 deassert reset into an ENERGIZED rail and latch a
                        // running on-die clock. DCENT historically released reset with
                        // the rail still cold (~0 V): chips POR with no core voltage,
                        // never clock, and the chain stays permanently silent
                        // (count=0, malformed=0 — silence not garbage; LIVE TEST 7/8/9
                        // 2026-06-09). Re-order to bosminer's only on the `a lab unit` RE-018
                        // standalone path; the proven  handoff + fleet keep the
                        // legacy order (NOT RE-018-gated -> byte-identical).
                        // 2026-06-09 (LIVE TEST 14): EBR is HARMFUL by default. The early
                        // cold_boot_init (SetVoltage+ENABLE) UNDER reset EIO-perturbs i2c-0
                        // and faults a warmup-confirmed fw=0x89 chip back to fw=0x82
                        // (ENABLE [82,82] firmware_echo_mismatch, NOT real_ack). The proven
                        // TEST-7 sequence has NO EBR (rail energized at Phase 3
                        // bosminer-minimal ENABLE). EXPLICIT opt-in only; default-OFF takes
                        // the legacy long-reset-hold branch. Fleet/handoff never enter this
                        // RE-018-gated block -> byte-identical.
                        // ENUMP clean EBR (2026-06-10): bosminer-minimal variant of
                        // enable-before-reset-release. The OLD
                        // DCENT_AM2_ENABLE_BEFORE_RESET_RELEASE ran the HEAVY cold_boot_init
                        // (flush + 0x16 heartbeat + 0x10 SetVoltage) under reset, EIO-perturbing
                        // i2c-0 and faulting the warmup-confirmed fw=0x89 chip back to fw=0x82
                        // (ENABLE [82,82], LIVE TEST 14). This variant arms bosminer-minimal
                        // ENABLE on the pic FIRST (below), so the ONLY dsPIC traffic under reset
                        // is the byte-identical framed 0x15 ENABLE -- energizing the chip rail
                        // BEFORE HB_RESET release (bosminer order: enable during reset-hold,
                        // release after) while preserving fw=0x89 + real_ack. Default-OFF +
                        // RE-018-gated (.25 fingerprint) + bosminer-minimal-gated -> fleet /
                        //  handoff byte-identical.
                        let ebr_minimal = am2_bm1362_re018_cold_sequence_enabled()
                            && am2_env_flag("DCENT_AM2_EBR_MINIMAL_ENABLE")
                            && bosminer_warmup::am2_dspic_bosminer_minimal_enable_enabled()
                            && am2_xil_25_fingerprint_matches();
                        let enable_before_reset_release = ebr_minimal
                            || (am2_bm1362_re018_cold_sequence_enabled()
                                && am2_env_flag("DCENT_AM2_ENABLE_BEFORE_RESET_RELEASE"));
                        if enable_before_reset_release {
                            info!(
                                t_ms = t(),
                                reset_hold_ms,
                                "[T+{}] Phase 2b-ext(EBR): bosminer-faithful order — assert HB_RESET LOW, energize rail UNDER reset, release AFTER rail up",
                                t()
                            );
                            // 1. Assert all reset lines LOW and HOLD (sysfs-repoint split).
                            if let Err(e) = bc.assert_resets_low(reset_slots) {
                                force_pwr_control_low(
                                    self.config.psu.pwr_control_gpio.as_deref(),
                                    "ebr-assert-low-failed",
                                );
                                anyhow::bail!(
                                    "Phase 2b-ext(EBR): HB_RESET assert-LOW failed before \
                                     rail enable: {e}"
                                );
                            }
                            // 2. Settle the POR while held LOW (bosminer ~1.2-1.8 s LOW->ENABLE gap).
                            std::thread::sleep(Duration::from_millis(
                                am2_env_u64("DCENT_AM2_RESET_LOW_PRE_ENABLE_MS", 1200)
                                    .clamp(200, 4000),
                            ));
                            // 3. Energize the chip rail (SetVoltage + ENABLE) WHILE reset is held LOW.
                            //    The dsPIC is on the 3.3 V i2c-0 bus (separate from the chip
                            //    RST_N line) and is already at fw=0x89 from the Phase-0d warmup,
                            //    so its ENABLE works with the chips held in reset.
                            // ENUMP clean EBR: arm bosminer-minimal so the under-reset enable is
                            // GET_VERSION(0x89, from the Phase-0d warmup) -> framed 0x15 ENABLE
                            // ONLY (no flush, no 0x16 heartbeat, no 0x10 SetVoltage in the
                            // window -- those drifted fw=0x89 -> fw=0x82 in the heavy EBR). When
                            // ebr_minimal is false this is a no-op and the legacy heavy EBR call
                            // is byte-identical.
                            if ebr_minimal {
                                pic.set_bosminer_minimal_enable(true);
                            }
                            info!(
                                t_ms = t(),
                                ebr_minimal,
                                "[T+{}] Phase 2b-ext(EBR): energizing chip rail (cold_boot_init: SetVoltage+ENABLE) WITH HB_RESET held LOW",
                                t()
                            );
                            if self.shutdown.is_cancelled() {
                                let _ = bc.release_resets_high(reset_slots);
                                anyhow::bail!(
                                    "AM2 hybrid run was cancelled before early chip-rail enable"
                                );
                            }
                            if let Err(e) = pic.cold_boot_init_with_options(13_700, true) {
                                let _ = bc.release_resets_high(reset_slots);
                                force_pwr_control_low(
                                    self.config.psu.pwr_control_gpio.as_deref(),
                                    "ebr-rail-enable-failed",
                                );
                                anyhow::bail!(
                                    "Phase 2b-ext(EBR): rail enable under HB_RESET failed: {e}"
                                );
                            }
                            if self.shutdown.is_cancelled() {
                                pic_i2c.latch_terminal_safe_off();
                                let _ = pic.disable_voltage();
                                force_am2_home_hard_stop(
                                    &self.config,
                                    "cancelled-after-early-chip-rail-enable",
                                );
                                let _ = stop_am2_runtime_feeders_bounded(
                                    &self.config,
                                    &mut runtime_threads,
                                    "cancelled-after-early-chip-rail-enable",
                                )
                                .await;
                                anyhow::bail!(
                                    "AM2 hybrid run was cancelled after early chip-rail enable; terminal safe-off requested"
                                );
                            }
                            // 4. Short settle for the DC-DC to reach target with chip held in reset.
                            std::thread::sleep(Duration::from_millis(
                                am2_env_u64("DCENT_AM2_ENABLE_TO_RESET_RELEASE_MS", 350)
                                    .clamp(100, 1500),
                            ));
                            // 5. Release reset HIGH -> chips deassert into a LIVE rail.
                            if let Err(e) = bc.release_resets_high(reset_slots) {
                                force_pwr_control_low(
                                    self.config.psu.pwr_control_gpio.as_deref(),
                                    "ebr-release-high-failed",
                                );
                                anyhow::bail!(
                                    "Phase 2b-ext(EBR): HB_RESET release-HIGH failed after \
                                     rail enable: {e}"
                                );
                            }
                            std::thread::sleep(Duration::from_millis(50));
                            info!(
                                t_ms = t(),
                                "[T+{}] Phase 2b-ext(EBR): HB_RESET released HIGH AFTER rail energized (bosminer order); Phase-3 ENABLE will be idempotent",
                                t()
                            );
                        } else {
                            info!(
                                t_ms = t(),
                                reset_hold_ms,
                                hb_reset_repoint = doing_bosminer_faithful_reset,
                                "[T+{}] Phase 2b-extended: S9-pattern long reset hold ({} ms LOW, all slots) — devmem (AXI-GPIO 0x41210000), or plain-kernel-sysfs when DCENT_AM2_HB_RESET_REPOINT=1 (mechanism logged by hold_resets_devmem)",
                                t(),
                                reset_hold_ms
                            );
                            // Hold ALL slots LOW simultaneously for the configured
                            // duration, then release together (legacy/handoff/fleet order).
                            match bc.hold_resets_devmem(reset_slots, reset_hold_ms) {
                                Ok(()) => info!(
                                    t_ms = t(),
                                    reset_hold_ms,
                                    "[T+{}] Phase 2b-extended: HB0..3_RESET held LOW {} ms then released HIGH via devmem (AXI-GPIO 0x41210000)",
                                    t(),
                                    reset_hold_ms
                                ),
                                Err(e) if doing_bosminer_faithful_reset => {
                                    force_pwr_control_low(
                                        self.config.psu.pwr_control_gpio.as_deref(),
                                        "hb-reset-hold-failed",
                                    );
                                    anyhow::bail!(
                                        "Phase 2b-extended: faithful HB_RESET hold failed before enum: {e}"
                                    );
                                }
                                Err(e) => warn!(
                                    t_ms = t(),
                                    error = %e,
                                    "[T+{}] Phase 2b-extended: devmem multi-slot reset hold FAILED (Phase 2b-post readback follows)",
                                    t()
                                ),
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    } else {
                        info!(
                            t_ms = t(),
                            reset_hold_ms,
                            "[T+{}] Phase 2b-extended: SKIPPED (am2_reset_hold_ms={} <= 20, using legacy pulse only)",
                            t(),
                            reset_hold_ms
                        );
                    }
                }
                Err(e) if doing_bosminer_faithful_reset => {
                    force_pwr_control_low(
                        self.config.psu.pwr_control_gpio.as_deref(),
                        "hb-reset-board-control-open-failed",
                    );
                    anyhow::bail!(
                        "BoardControl::open(uio17) failed before faithful HB_RESET hold: {e}"
                    );
                }
                Err(e) => warn!(
                    error = %e,
                    "BoardControl::open(uio17) FAILED — skipping HB resets (chain may remain wedged)"
                ),
            }

            // ============================================================
            // Phase 2b-post: HBx_RESET sysfs readback + force-HIGH fallback.
            //
            // Live ground truth (.139, 2026-04-27): even after `pulse_reset()`
            // returns Ok, `/sys/kernel/debug/gpio` shows gpio-898 / gpio-899
            // still LOW (`out lo`), meaning the chips remain held in reset
            // and chain UART RX stays at 0. Hypothesis: am2 has no
            // `/dev/gpiochip*`,
            // so libgpiod paths in `pulse_reset()` are no-ops and the
            // devmem RMW fallback either races the kernel sysfs cache or
            // ends LOW. Belt-and-suspenders: read each HBx_RESET sysfs
            // value AFTER the pulse and force-HIGH via sysfs if it didn't
            // settle HIGH.
            //
            // GPIO map (matches `AM2_RESET_GPIOS = [897, 898, 899, 900]`):
            //   slot 0 → gpio897 (HB0_RESET)
            //   slot 1 → gpio898 (HB1_RESET)
            //   slot 2 → gpio899 (HB2_RESET)  ← .139 chain1 (relay_phys_idx=2)
            //   slot 3 → gpio900 (HB3_RESET)  ← .139 chain4 (relay_phys_idx=3)
            //
            // DO NOT replace pulse_reset(); this only adds verification +
            // a recovery write if the pulse left reset asserted.
            // ============================================================
            for slot in 0u8..=3 {
                let gpio_num: u32 = 897 + slot as u32;
                let path = format!("/sys/class/gpio/gpio{}/value", gpio_num);

                let post_state = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok());

                info!(
                    t_ms = t(),
                    slot,
                    gpio = gpio_num,
                    post_pulse_value = ?post_state,
                    "[T+{}] HB{}_RESET post-pulse readback (gpio{})",
                    t(),
                    slot,
                    gpio_num
                );

                if post_state == Some(0) {
                    warn!(
                        t_ms = t(),
                        slot,
                        gpio = gpio_num,
                        "[T+{}] HB{}_RESET still LOW post-pulse — forcing HIGH via sysfs",
                        t(),
                        slot
                    );
                    match std::fs::write(&path, "1") {
                        Ok(()) => {
                            let recheck = std::fs::read_to_string(&path)
                                .ok()
                                .and_then(|s| s.trim().parse::<u8>().ok());
                            info!(
                                t_ms = t(),
                                slot,
                                gpio = gpio_num,
                                after_force = ?recheck,
                                "[T+{}] HB{}_RESET forced HIGH via sysfs (recheck={:?})",
                                t(),
                                slot,
                                recheck
                            );
                        }
                        Err(e) => warn!(
                            t_ms = t(),
                            slot,
                            gpio = gpio_num,
                            error = %e,
                            "[T+{}] HB{}_RESET sysfs force-HIGH FAILED",
                            t(),
                            slot
                        ),
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));

            // ============================================================
            // Phase 2c-pre: fans-OK gate ( missing-step fix).
            //
            // Bosminer's wire trace (`12-bosminer-startup-timeline.md`):
            //   T+163.689 CHAIN/2: Resetting hash board
            //   T+164.691 CHAIN/2: Waiting for fans to spin up… (1 s)
            //   T+166.693 CHAIN/2: Fans OK             ← gate clears
            //   T+166.868 PWR/2:    Enable voltage     ← per-chain ENABLE
            //
            // The hybrid mode bypasses `daemon.rs::Daemon::run()` (line ~4540
            // "Phase 2: Fan setup"), so cold-boot through `--s19j-hybrid` leaves
            // the fan-control IP at PWM 0 → ASICs ramp into stopped airflow →
            // thermal foldback → chain UART silent.  identified this as
            // the most likely missing step blocking native mining on `a lab unit`.
            //
            // Fan-control is selected by UIO sysfs name, not by hard-coded
            // `/dev/uio16`. Bosminer's gate threshold is `min_rpm=2000`
            // (per `SUMMARY.md:78,156`).
            //
            // HOME QUIET CEILING: XIL is a home miner, so `fan_max_pwm` is an
            // absolute command ceiling during cold boot as well as steady-state
            // mining. Do not apply a transient 60% spin-up here.
            //
            // Fail-closed: PWM command readback is not acoustic proof on AM2.
            // Do not proceed toward voltage unless tach/RPM proves the fans are
            // physically moving within the home cap.
            // ============================================================
            let mut fan_gate_pwm: Option<u8> = None;
            if early_fan_gate_passed {
                info!(
                    t_ms = t(),
                    "[T+{}] Phase 2c-pre: SKIPPED — early Phase 0b fan gate already cleared (am2_fan_gate_before_pic=true)",
                    t()
                );
                fan_gate_pwm = Some(
                    self.config
                        .thermal
                        .fan_max_pwm
                        .min(dcentrald_hal::fan::PWM_MAX)
                        .min(dcentrald_hal::fan::PWM_SAFETY_MAX),
                );
            } else {
                info!(
                    t_ms = t(),
                    "[T+{}] Phase 2c-pre: fans-OK gate (Wave-3 missing-step fix)",
                    t()
                );
                // NOTE on uio number: live probe `01-system-recon.md:106` confirms
                // `uio16 fan-control 0x42800000` on `a lab unit`. Verify on a fresh unit
                // with: `cat /sys/class/uio/uio*/name | grep fan`.
                match open_am2_fan_controller("phase-2c-fan-gate") {
                    Some((discovery, fan)) => {
                        let configured_max = self.config.thermal.fan_max_pwm;
                        // Home cap command: no cold-boot fan burst above the configured home
                        // ceiling. The XIL profile must not burst fans at night.
                        let spin_pwm = configured_max
                            .min(dcentrald_hal::fan::PWM_MAX)
                            .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                        fan.set_speed(spin_pwm);
                        fan_gate_pwm = Some(spin_pwm);
                        info!(
                            pwm = spin_pwm,
                            uio = discovery.uio_number,
                            configured_fan_max_pwm = configured_max,
                            t_ms = t(),
                            "[T+{}] Phase 2c-pre: fans commanded to home cap PWM {} (no cold-boot burst)",
                            t(),
                            spin_pwm
                        );
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(3);
                        loop {
                            let max_rpm = fan
                                .get_per_fan_rpm()
                                .iter()
                                .map(|(_, r)| *r)
                                .max()
                                .unwrap_or(0);
                            // Bosminer's gate is min_rpm=2000; user task says 1800.
                            // We use 1800 as the minimum acceptable threshold to
                            // tolerate slightly-slower fans, while still confirming
                            // the gate cleared electrically.
                            if max_rpm >= 1800 {
                                info!(
                                    max_rpm,
                                    t_ms = t(),
                                    "[T+{}] fans gate cleared at {} RPM",
                                    t(),
                                    max_rpm
                                );
                                break;
                            }
                            if std::time::Instant::now() >= deadline {
                                warn!(
                                    max_rpm,
                                    t_ms = t(),
                                    "[T+{}] Phase 2c-pre: fans <1800 RPM after 3 s — fail-closed before voltage",
                                    t()
                                );
                                force_am2_home_hard_stop(&self.config, "am2-fan-rpm-gate-failed");
                                self.shutdown.cancel();
                                anyhow::bail!(
                                    "AM2 fan RPM gate failed before voltage enable: max_rpm={} after 3s",
                                    max_rpm
                                );
                            }
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                    None => {
                        warn!(
                            t_ms = t(),
                            "[T+{}] FanController open(fan-control UIO) FAILED — fail-closed before voltage",
                            t()
                        );
                        force_am2_home_hard_stop(&self.config, "am2-fan-uio-open-failed");
                        self.shutdown.cancel();
                        anyhow::bail!("AM2 fan-control UIO unavailable before voltage enable");
                    }
                }
            }

            // Phase 2c gate (legacy): keep the historical 2 s settling sleep AFTER
            // the explicit fans-OK gate above. Bosminer enables per-chain voltage
            // ~175 ms after "Fans OK" — the small extra wait gives the dsPIC
            // heartbeat thread a beat to stabilise before SetVoltage.
            info!(
                t_ms = t(),
                "[T+{}] Phase 2c: 2 s settle window before per-chain ENABLE_VOLTAGE",
                t()
            );
            std::thread::sleep(Duration::from_secs(2));

            // Phase 3: per-chain voltage enable via the proper dsPIC cold-boot
            // sequence (protocol probe → set_voltage → enable → 1 s DC-DC
            // ramp → post-enable heartbeat). The simpler `enable_voltage()`
            // alone was insufficient on `a lab unit` — chain stayed silent at
            // 0 chip replies on all 4 ttyS UARTs. The 7-step `cold_boot_init`
            // matches BraiinsOS am2-s17's PWR/N init sequence (see
            // `dspic.rs:493-602`).
            // active_chains gates this selected address before Phase 1; the
            // LM75 pass inside cold_boot_init is therefore only run for an
            // active dsPIC on single-board .139.
            if trust_rail {
                warn!(
                    t_ms = t(),
                    "[T+{}] Phase 3 (trust-rail): SetVoltage SKIPPED, calling ENABLE directly on dsPIC 0x{:02X}",
                    t(),
                    selected_pic_addr
                );
                if let Err(e) = pic.enable_voltage() {
                    warn!(
                        error = %e,
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        "ENABLE_VOLTAGE failed in trust-rail mode; hard-stopping instead of blind chain init"
                    );
                    force_am2_home_hard_stop(&self.config, "trust-rail-enable-voltage-failed");
                    let feeders_quiesced = stop_am2_runtime_feeders_bounded(
                        &self.config,
                        &mut runtime_threads,
                        "trust-rail-enable-voltage-failed",
                    )
                    .await;

                    if feeders_quiesced {
                        let Some(psu_mutex) = psu_arc.as_ref() else {
                            return Err(e).context("trust-rail ENABLE_VOLTAGE failed");
                        };
                        let mut psu_guard = psu_mutex.lock().unwrap_or_else(|e| e.into_inner());
                        if let Err(psu_err) = psu_guard.safe_shutdown_to_min() {
                            warn!(
                                error = %psu_err,
                                "PSU safe-direction shutdown failed after trust-rail ENABLE failure"
                            );
                        }
                    }

                    return Err(e).context("trust-rail ENABLE_VOLTAGE failed");
                } else {
                    info!(
                        t_ms = t(),
                        "[T+{}] ENABLE (trust-rail) ack received from dsPIC 0x{:02X}",
                        t(),
                        selected_pic_addr
                    );
                }
            } else {
                let chip_rail_target_mv = open_core_rail_plan.energization_mv;

                // 2026-05-31 — AM2 `a lab unit` open-core voltage-ramp experiment
                // (Variant A: enumerate AT open-core, matching bosminer/AMTC,
                // ported from the proven AM3-BB `a lab unit` open-core rail stage).
                // When the compound gate is active (env
                // `DCENT_AM2_OPEN_CORE_VOLTAGE=1` + `a lab unit` fingerprint + lab
                // over-volt cap lifted), drive the chip string to the elevated
                // the admitted elevated target for enumeration +
                // core activation, then ramp back DOWN to 13700 mV after enum
                // succeeds (Phase 4-7 post-enum ramp-down below). Default-OFF +
                // `a lab unit`-only ⇒ byte-identical for every other unit (the gate
                // helpers short-circuit to the steady 13700 target when unset).
                if open_core_rail_plan.requires_demotion() {
                    let open_core_mv = open_core_rail_plan.energization_mv;
                    warn!(
                        t_ms = t(),
                        steady_mv = open_core_rail_plan.steady_mv,
                        open_core_mv,
                        env_gate = "DCENT_AM2_OPEN_CORE_VOLTAGE=1 + .25 fingerprint + DCENT_AM2_ALLOW_LAB_OVERVOLT=1",
                        "[T+{}] AM2 .25 OPEN-CORE experiment ARMED: enumerating the BM1362 chip \
                         string AT {} mV (open-core) instead of {} mV (steady). Will ramp DOWN to \
                         {} mV after enum > 0. Matches LuxOS/.79 + bosminer + AMTC cold-boot.",
                        t(),
                        open_core_mv,
                        open_core_rail_plan.steady_mv,
                        open_core_rail_plan.steady_mv
                    );
                }
                // Layer 3 (2026-05-22 XIL `a lab unit` recovery): if Phase 0d
                // bosminer-warmup + 5×1Hz idle heartbeats already proved 5
                // stable beats on this dsPIC, tell `cold_boot_init` to skip
                // its internal duplicate 5-tick warmup loop (saves ~5 s on
                // a clean cold boot and avoids racing the same gate twice).
                let skip_warmup_loop = warmup_did_run;

                //  (2026-05-25) — RE-finding-driven skip path for
                // the dsPIC SetVoltage + ENABLE_VOLTAGE. Per
                // `PHASE2C-DSPIC-RAIL-FAILURE-RE.md` H1:
                //   bosminer's `a lab unit` cold-boot trace to dsPIC 0x20 contains
                //   ZERO `0x10` (SetVoltage) and ZERO `0x15` (ENABLE_VOLTAGE)
                //   opcodes. Live evidence on `a lab unit` 2026-04-26 already
                //   showed bare `[55 AA 10 DAC]` is ACK'd at the wire level
                //   but does NOT engage the chain DC-DC rail. On cold-boot
                //   `a lab unit` fw=0x82, the same pattern holds.
                //
                // The actual chip-rail engagement happens via the Loki
                // spoof's `0x83` SetVoltage (wired by  Change 2 in
                // `cold_boot_sequence_loki_standalone`). When the umbrella
                // gate `DCENT_AM2_STANDALONE_RE_FIX=1` is set AND the `a lab unit`
                // fingerprint matches AND we're in the  standalone
                // Loki cold-boot path (PSU transport == gpio_bitbang +
                // `DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1`), SKIP the dsPIC
                // SetVoltage + ENABLE_VOLTAGE — those are no-ops at best,
                // and (per H1) may interfere with the rail state already
                // engaged by Loki SetVoltage.
                //
                // The Phase 0d-post LM75A passthrough warmup (Change 3 above)
                // has already warmed the dsPIC FSM by this point. Phase 4
                // chain-enum will reveal whether the chip rail actually
                // engaged — if it returns 0/126 chips, the Loki SetVoltage
                // path didn't engage and the run aborts there. If it returns
                // chips, the rail engaged via Loki and we proceed to mining.
                //  (2026-05-25, LIVE-driven REVERT of ):
                // the  "skip dsPIC SetVoltage on cold-cold standalone"
                // path was based on a partial RE finding (bosminer didn't
                // emit 0x10/0x15 to dsPIC, so chip rail must engage via Loki
                // 0x83). Two LIVE tonights falsified this:
                //
                //   - -LIVE (2026-05-25 ~21:30): cold-cold NAND boot
                //     DCENT_OS, Loki 0x83 sent, chain enum 0/126 — chip rail
                //     NOT engaged.
                //   -  FORCE_DSPIC_SETVOLTAGE override LIVE: dsPIC
                //     0x10/0x15 ACKed but daemon was using BARE protocol
                //     because chip state read fw=0x82 — chain enum still 0/126.
                //
                // But  LIVE (2026-05-24 ~23:41) — BEFORE the
                // skip shipped — successfully engaged chip rail standalone:
                // chain presence 12 unique chips, 44+ SHARES ACCEPTED via the
                // standalone path with `cold_boot_init_with_options` firing
                // normally. The  optimisation was thus the actual
                // REGRESSION.
                //
                //  REVERT: always call cold_boot_init_with_options.
                // Whether the chip rail actually engages at 13.7 V depends on
                // FRAMED vs BARE protocol (determined by GET_VERSION result —
                // see DCENT_AM2_GET_VERSION_FRAMED_4B) and on the Loki spoof
                // sequence having pre-warmed the chip-rail control plane
                // (/c/d/e standalone Loki cold-wake). Skipping the
                // call cannot make rail engagement happen on its own.
                //
                // The legacy `DCENT_AM2_FORCE_DSPIC_SETVOLTAGE` env name is
                // accepted but now unused — kept as a no-op so old launcher
                // scripts don't trip an unknown-env check.
                let _wave55i_legacy_no_op = am2_env_flag("DCENT_AM2_FORCE_DSPIC_SETVOLTAGE");
                // Historical  skip condition was standalone-only:
                // DCENT_AM2_STANDALONE_RE_FIX=1 + .25 fingerprint +
                // DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1 and NOT
                // DCENT_AM2_TRUST_RAIL_FALLBACK.  live evidence
                // disabled it entirely.
                let wave55f_skip_dspic_setvoltage = false; // Wave-55j REVERT

                if wave55f_skip_dspic_setvoltage {
                    warn!(
                        t_ms = t(),
                        target_mv = chip_rail_target_mv,
                        env_gate = "DCENT_AM2_STANDALONE_RE_FIX=1 + .25 fingerprint + DCENT_AM2_PSU_LOKI_COLD_BOOT_FULL=1",
                        "[T+{}] Phase 3 (Wave-55f RE fix): SKIPPING dsPIC SetVoltage + \
                         ENABLE_VOLTAGE on 0x{:02X}. RE finding (PHASE2C-DSPIC-RAIL-FAILURE-RE.md H1): \
                         bosminer issues ZERO 0x10/0x15 to dsPIC on .25 cold-boot; chip rail \
                         engaged via Loki SetVoltage(0x83) in cold_boot_sequence_loki_standalone \
                         (Wave-55f Change 2). Phase 4 chain-enum reveals actual rail state.",
                        t(),
                        selected_pic_addr
                    );
                    info!(
                        t_ms = t(),
                        target_mv = chip_rail_target_mv,
                        "[T+{}] dsPIC SetVoltage SKIPPED (Wave-55f RE-fix path); LM75A passthrough \
                         warmup already ran in Phase 0d-post — proceeding to Phase 4 chain enum",
                        t()
                    );
                } else {
                    // Post-JUMP heartbeat keep-alive (2026-06-07, `a lab unit`
                    // standalone cold-engage). Default-OFF + `a lab unit`-fingerprint:
                    // when `DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE=1` AND
                    // the `a lab unit` hardware fingerprint matches, tell cold_boot_init
                    // to keep the cold-engaged FRAMED (fw=0x89) dsPIC serviced
                    // with framed 0x16 heartbeats through SetVoltage → ENABLE so
                    // it does not drift back to fw=0x82 bootloader before the
                    // ENABLE (LIVE blocker: ENABLE returned ack_cmd=0x82). With
                    // the env unset OR the fingerprint not matching, this is a
                    // no-op and cold_boot_init is byte-identical for the
                    // fleet/handoff/legacy paths. The env is AND-gated with the
                    // `a lab unit` fingerprint HERE (the platform fingerprint is not
                    // visible from the platform-agnostic dcentrald-asic crate).
                    let postjump_keepalive_active =
                        bosminer_warmup::am2_dspic_postjump_heartbeat_keepalive_enabled()
                            && am2_xil_25_fingerprint_matches();
                    pic.set_postjump_heartbeat_keepalive(postjump_keepalive_active);
                    // Re-JUMP-before-ENABLE (2026-06-07, `a lab unit` standalone
                    // cold-engage) — prior dsPIC app-mode drift fix. It is
                    // still part of the standalone recipe when enabled, but
                    // later v+2 evidence proved the remaining enum=0 blocker is
                    // downstream of this. Default-OFF + `a lab unit`-fingerprint: when
                    // `DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE=1` AND the `a lab unit`
                    // hardware fingerprint matches, tell cold_boot_init to
                    // re-read GET_VERSION immediately before SetVoltage and, if
                    // the cold-engaged FRAMED (fw=0x89) dsPIC has drifted back to
                    // fw=0x82 bootloader, re-JUMP it (flush → framed JUMP, NO
                    // RESET) to fw=0x89 so the ENABLE lands in app mode (the
                    // framed 0x16 keep-alive FAILED — heartbeats don't hold app
                    // mode nor transition 0x82→0x89). With the env unset OR the
                    // fingerprint not matching, this is a no-op and cold_boot_init
                    // is byte-identical for the fleet/handoff/legacy paths. The
                    // env is AND-gated with the `a lab unit` fingerprint HERE (the
                    // platform fingerprint is not visible from the
                    // platform-agnostic dcentrald-asic crate).
                    let rejump_before_enable_active =
                        bosminer_warmup::am2_dspic_rejump_before_enable_enabled()
                            && am2_xil_25_fingerprint_matches();
                    pic.set_rejump_before_enable(rejump_before_enable_active);
                    // Skip-SetVoltage-keep-ENABLE (2026-06-07, `a lab unit` standalone
                    // cold-engage) — prior ENABLE-drift fix
                    // (ENABLE-DRIFT-DIFF.md, commit fc4eef92). Not sufficient
                    // for standalone mining by itself. Default-OFF +
                    // `a lab unit`-fingerprint: when
                    // `DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE=1` AND the
                    // `a lab unit` hardware fingerprint matches, tell cold_boot_init to
                    // SKIP the dsPIC SetVoltage (0x10) — which bosminer never
                    // sends to the `a lab unit` dsPIC (the rail is APW-PSU-side) and
                    // which faults the cold-engaged fw=0x89 app back to fw=0x82
                    // bootloader → ENABLE reads [82,82] — and go GET_VERSION(0x89)
                    // → [re-JUMP if drifted] → ENABLE(0x15) directly like
                    // bosminer. The ENABLE wire bytes are unchanged; the rail
                    // energizes via the ENABLE at the dsPIC power-on default
                    // voltage (bosminer-proven safe). With the env unset OR the
                    // fingerprint not matching, this is a no-op and cold_boot_init
                    // is byte-identical for the fleet/handoff/legacy paths (the
                    // 0x10 SetVoltage still fires). The env is AND-gated with the
                    // `a lab unit` fingerprint HERE (the platform fingerprint is not
                    // visible from the platform-agnostic dcentrald-asic crate).
                    let skip_setvoltage_keep_enable_active =
                        bosminer_warmup::am2_dspic_skip_setvoltage_keep_enable_enabled()
                            && am2_xil_25_fingerprint_matches();
                    pic.set_skip_setvoltage_keep_enable(skip_setvoltage_keep_enable_active);
                    // Bosminer-minimal ENABLE (2026-06-07, `a lab unit` standalone
                    // cold-engage) — the CONSOLIDATED fix for the LIVE-confirmed
                    // ENABLE [82,82] drift. Default-OFF + `a lab unit`-fingerprint: when
                    // `DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE=1` AND the `a lab unit`
                    // hardware fingerprint matches, tell cold_boot_init to send the
                    // dsPIC NOTHING between the confirmed-fw=0x89 GET_VERSION (run
                    // in the external warmup) and the ENABLE — no flush, no 0x16
                    // heartbeat, no 0x30 LM75A read, no second GET_VERSION/0x06
                    // JUMP re-verify, no 0x10 SetVoltage — so the ONLY dsPIC
                    // traffic in that window is the byte-identical 0x15 ENABLE,
                    // exactly like bosminer (whose pre-ENABLE commands DCENT
                    // emitted from multiple code paths; LIVE TEST 6 still showed
                    // LM75A reads + a stray GET_VERSION+JUMP after the env-by-env
                    // skips). It SUPERSEDES skip_setvoltage_keep_enable (omits the
                    // 0x10 too) and renders the keep-alive / re-JUMP moot. The rail
                    // energizes via the unchanged ENABLE at the dsPIC power-on
                    // default voltage (bosminer-proven safe). With the env unset OR
                    // the fingerprint not matching, this is a no-op and
                    // cold_boot_init is byte-identical for the fleet/handoff/legacy
                    // paths. The env is AND-gated with the `a lab unit` fingerprint HERE
                    // (the platform fingerprint is not visible from the
                    // platform-agnostic dcentrald-asic crate).
                    let bosminer_minimal_enable_active =
                        bosminer_warmup::am2_dspic_bosminer_minimal_enable_enabled()
                            && am2_xil_25_fingerprint_matches();
                    pic.set_bosminer_minimal_enable(bosminer_minimal_enable_active);
                    info!(
                        t_ms = t(),
                        target_mv = chip_rail_target_mv,
                        skip_warmup_loop,
                        postjump_keepalive_active,
                        rejump_before_enable_active,
                        skip_setvoltage_keep_enable_active,
                        bosminer_minimal_enable_active,
                        "[T+{}] Phase 3: PIC cold_boot_init starting (GET_VERSION → HB gate → SetVoltage target_mv=13700 → ENABLE; RESET/JUMP banned)",
                        t()
                    );
                    if self.shutdown.is_cancelled() {
                        pic_i2c.latch_terminal_safe_off();
                        let _ = pic.disable_voltage();
                        force_am2_home_hard_stop(&self.config, "cancelled-before-chip-rail-enable");
                        let _ = stop_am2_runtime_feeders_bounded(
                            &self.config,
                            &mut runtime_threads,
                            "cancelled-before-chip-rail-enable",
                        )
                        .await;
                        anyhow::bail!("AM2 hybrid run was cancelled before chip-rail enable");
                    }
                    if let Err(e) =
                        pic.cold_boot_init_with_options(chip_rail_target_mv, skip_warmup_loop)
                    {
                        anyhow::bail!(
                            "PIC cold_boot_init FAILED — without 13.7 V on the chain, ASICs are dead. \
                             Aborting init. error: {}",
                            e
                        );
                    }
                    if self.shutdown.is_cancelled() {
                        pic_i2c.latch_terminal_safe_off();
                        let _ = pic.disable_voltage();
                        force_am2_home_hard_stop(&self.config, "cancelled-after-chip-rail-enable");
                        let _ = stop_am2_runtime_feeders_bounded(
                            &self.config,
                            &mut runtime_threads,
                            "cancelled-after-chip-rail-enable",
                        )
                        .await;
                        anyhow::bail!(
                            "AM2 hybrid run was cancelled after chip-rail enable; terminal safe-off requested"
                        );
                    }
                    info!(
                        t_ms = t(),
                        target_mv = chip_rail_target_mv,
                        "[T+{}] dsPIC cold_boot_init returned OK (SetVoltage applied, ENABLE_VOLTAGE accepted at protocol level — firmware-byte ACK/echo, NOT a rail measurement); chain rail at target_mv=13700 is UNVERIFIED until Phase 4 chain enumeration",
                        t()
                    );
                }

                // 9 / xil-`a lab unit` experiment: also warmup + voltage-enable
                // the OTHER active dsPIC addresses (0x21 / 0x22) BEST-EFFORT.
                // BraiinsOS opens both chain UARTs in parallel on `a lab unit` (2-board
                // unit) and voltage-enables both PICs; DCENT_OS's hybrid path is
                // single-chain-by-design. 8 attempted cold_boot_init on
                // 0x22 directly and failed EIO because PIC 0x22 was still in
                // cold-boot state (no warmup chain ran on it). 9 fixes
                // that: emit the same bosminer-faithful warmup chain on the
                // other PICs first, then cold_boot_init.
                //
                // Gated by `DCENT_AM2_VOLTAGE_ENABLE_ALL_ACTIVE_PICS=1`.
                // Default-OFF — preserves `a lab unit`/`a lab unit`/`a lab unit` byte-parity.
                // Best-effort: each PIC's warmup AND init can fail independently
                // without bailing the daemon.
                if all_active_voltage_enable {
                    let other_pics: Vec<u8> = active_dspic_addrs(active_chains)
                        .into_iter()
                        .filter(|&a| a != selected_pic_addr)
                        .collect();
                    if !other_pics.is_empty() {
                        info!(
                            t_ms = t(),
                            other_pics = format_args!("{:02X?}", other_pics),
                            "[T+{}] Wave-25.9: best-effort warmup + voltage-enable on other active dsPIC addresses",
                            t()
                        );
                        // : if the strace-derived variant is the env-selected
                        // warmup for the primary PIC, use it for the other PICs too so
                        // all of `a lab unit`'s active dsPICs end up in the same fw=0x89 state.
                        let use_strace_for_others = am2_pic_reset_strace_derived_enabled();
                        for other_addr in other_pics {
                            // Step 1: bosminer-faithful warmup chain on this PIC
                            // ( strace-derived OR S9-era bare per env gate).
                            let other_warmup_result = if use_strace_for_others {
                                // COLD-BYTE-DIFF Fix B: single-atomic-transaction
                                // form when env+`a lab unit`-fingerprint match, else the
                                // proven N-transaction form (byte-identical).
                                am2_run_strace_derived_warmup(pic_i2c, other_addr)
                            } else {
                                bosminer_warmup::am2_pic_reset_and_start_app_bosminer_faithful(
                                    pic_i2c, other_addr,
                                )
                            };
                            match other_warmup_result {
                                Ok(()) => info!(
                                    addr = format_args!("0x{:02X}", other_addr),
                                    "Wave-25.9: warmup chain emitted OK on 0x{:02X}", other_addr
                                ),
                                Err(e) => {
                                    let target_is_effective_chain =
                                        effective_chain_dspic_addr == Some(other_addr);
                                    warn!(
                                        addr = format_args!("0x{:02X}", other_addr),
                                        target_is_effective_chain,
                                        error = %e,
                                        "Wave-25.9: warmup chain on 0x{:02X} FAILED (best-effort, skipping cold_boot_init for this PIC)",
                                        other_addr
                                    );
                                    if target_is_effective_chain
                                        && am2_env_flag("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK")
                                    {
                                        if let Some(service) = i2c0_service.as_ref() {
                                            let mut disable_addrs =
                                                vec![selected_pic_addr, other_addr];
                                            disable_addrs.sort_unstable();
                                            disable_addrs.dedup();
                                            disable_dspic_addrs_best_effort(
                                                service,
                                                &disable_addrs,
                                                selected_pic_addr,
                                                heartbeat_pic_fw,
                                                "target-dspic-warmup-failed",
                                            );
                                        }
                                        force_am2_home_hard_stop(
                                            &self.config,
                                            "target-dspic-warmup-failed",
                                        );
                                        self.shutdown.cancel();
                                        anyhow::bail!(
                                            "Target chain dsPIC 0x{:02X} warmup failed under DCENT_AM2_REQUIRE_REAL_ENABLE_ACK=1; refusing BM1362 enum on {} without target-rail proof",
                                            other_addr,
                                            chain_uart_device
                                        );
                                    }
                                    continue;
                                }
                            }
                            // Step 2: 5×1Hz idle heartbeats (match Phase 0d).
                            //  (RE-018, 2026-05-30, wf_ce77f2f8 Change #1): force the
                            // OBSERVED fw-family byte (0x8A) for the `a lab unit` effective-chain dsPIC
                            // (0x22) so ENABLE/SetVoltage use the 6-byte Canonical form — the
                            // form that produced a real [0x15,0x00] ACK on the live ttyS3 cold-
                            // cycle — instead of the 7-byte VnishPadded form (which `a lab unit` echoes
                            // [0x8A,0x8A]). GET_VERSION reports 0x89 but the chip's idle/echo
                            // family is 0x8A (the codebase already documents "0x22 fw=0x8A").
                            // Also makes classify_enable_ack expect 0x8A so a legit echo is
                            // FirmwareEcho, not FirmwareEchoMismatch. Default-OFF +
                            // `a lab unit`-fingerprint + effective-chain only → byte-identical for
                            // .109/.79/.129/.135/S9/handoff (they keep None⇒Fw89). This service
                            // is reused for the 10 direct SetVoltage + ENABLE below.
                            let other_fw_override: Option<u8> =
                                if am2_dspic_fw_from_observed_enabled()
                                    && effective_chain_dspic_addr == Some(other_addr)
                                    && am2_xil_25_fingerprint_matches()
                                {
                                    Some(0x8A)
                                } else {
                                    None
                                };
                            let mut other_warmup_pic = Pic0x89Service::new_with_fw(
                                pic_i2c.clone(),
                                other_addr,
                                other_fw_override,
                            );
                            let mut beats_ok = 0u32;
                            for tick in 1..=5 {
                                std::thread::sleep(Duration::from_secs(1));
                                match other_warmup_pic.send_heartbeat() {
                                    Ok(()) => {
                                        beats_ok += 1;
                                        info!(
                                            addr = format_args!("0x{:02X}", other_addr),
                                            tick,
                                            beats_ok,
                                            "Wave-25.9: 0x{:02X} heartbeat OK",
                                            other_addr
                                        );
                                    }
                                    Err(e) => warn!(
                                        addr = format_args!("0x{:02X}", other_addr),
                                        tick, error = %e,
                                        "Wave-25.9: 0x{:02X} heartbeat failed (non-fatal)",
                                        other_addr
                                    ),
                                }
                            }
                            if beats_ok < 5 {
                                let target_is_effective_chain =
                                    effective_chain_dspic_addr == Some(other_addr);
                                warn!(
                                    addr = format_args!("0x{:02X}", other_addr),
                                    beats_ok,
                                    target_is_effective_chain,
                                    "Wave-25.9: 0x{:02X} fewer than 5 stable beats — skipping cold_boot_init",
                                    other_addr
                                );
                                if target_is_effective_chain
                                    && am2_env_flag("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK")
                                {
                                    if let Some(service) = i2c0_service.as_ref() {
                                        let mut disable_addrs = vec![selected_pic_addr, other_addr];
                                        disable_addrs.sort_unstable();
                                        disable_addrs.dedup();
                                        disable_dspic_addrs_best_effort(
                                            service,
                                            &disable_addrs,
                                            selected_pic_addr,
                                            heartbeat_pic_fw,
                                            "target-dspic-heartbeat-failed",
                                        );
                                    }
                                    force_am2_home_hard_stop(
                                        &self.config,
                                        "target-dspic-heartbeat-failed",
                                    );
                                    self.shutdown.cancel();
                                    anyhow::bail!(
                                        "Target chain dsPIC 0x{:02X} had only {} stable heartbeat(s) under DCENT_AM2_REQUIRE_REAL_ENABLE_ACK=1; refusing BM1362 enum on {} without target-rail proof",
                                        other_addr,
                                        beats_ok,
                                        chain_uart_device
                                    );
                                }
                                continue;
                            }
                            // Step 3 (10): direct SetVoltage + ENABLE on this PIC,
                            // bypassing cold_boot_init's internal heartbeat sanity which
                            // was failing EIO in 9 even though our 5 heartbeats
                            // above succeeded on the same address.
                            //
                            // Reuse the warmed Pic0x89Service from Step 2 — its firmware
                            // protocol selection already settled and it shares state with
                            // the successful heartbeats.
                            info!(
                                addr = format_args!("0x{:02X}", other_addr),
                                target_mv = chip_rail_target_mv,
                                "Wave-25.10: direct SetVoltage on dsPIC 0x{:02X} (bypass cold_boot_init wrapper)",
                                other_addr
                            );
                            match other_warmup_pic.set_voltage(chip_rail_target_mv) {
                                Ok(()) => {
                                    info!(
                                        addr = format_args!("0x{:02X}", other_addr),
                                        "Wave-25.10: 0x{:02X} SetVoltage OK", other_addr
                                    );
                                    match other_warmup_pic.enable_voltage() {
                                        Ok(()) => {
                                            info!(
                                                addr = format_args!("0x{:02X}", other_addr),
                                                "Wave-25.10: 0x{:02X} ENABLE_VOLTAGE accepted; rail unverified until Phase 4 chain enumeration",
                                                other_addr
                                            );
                                            //  (RE-018): framed rail-voltage readback right after
                                            // the (now real) ENABLE ACK — READ-ONLY diagnostic, non-fatal.
                                            // Disambiguates the enum=0 residual: ~13700 mV ⇒ rail engaged
                                            // (chip-wake/transport is the residual, software-fixable);
                                            // ~0/err ⇒ rail not energized despite the ACK (DMM on slot-3
                                            // V+ warranted). Now that Change #1 lands the correct fw=0x8A
                                            // framed encoding, the 0x3B/0x3A reads should decode (were
                                            // all-0xFF before). Never gates init.
                                            match other_warmup_pic.read_voltage() {
                                                Ok(mv) => info!(
                                                    addr = format_args!("0x{:02X}", other_addr),
                                                    mv,
                                                    "Wave-57: 0x{:02X} framed GET_VOLTAGE(0x3B) post-ENABLE rail readback mv={} (diagnostic; ~13700=engaged, ~0/low=dead)",
                                                    other_addr, mv
                                                ),
                                                Err(e) => warn!(
                                                    addr = format_args!("0x{:02X}", other_addr),
                                                    error = %e,
                                                    "Wave-57: 0x{:02X} framed GET_VOLTAGE(0x3B) post-ENABLE readback failed (diagnostic only)",
                                                    other_addr
                                                ),
                                            }
                                            match other_warmup_pic.measure_voltage() {
                                                Ok(mv) => info!(
                                                    addr = format_args!("0x{:02X}", other_addr),
                                                    mv,
                                                    "Wave-57: 0x{:02X} MEASURE_VOLTAGE(0x3A) analog-ADC rail readback mv={} (diagnostic; stronger rail proxy than 0x3B, still NOT physical/DMM proof)",
                                                    other_addr, mv
                                                ),
                                                Err(e) => warn!(
                                                    addr = format_args!("0x{:02X}", other_addr),
                                                    error = %e,
                                                    "Wave-57: 0x{:02X} MEASURE_VOLTAGE(0x3A) readback failed (diagnostic only)",
                                                    other_addr
                                                ),
                                            }
                                        }
                                        Err(e) => {
                                            let target_is_effective_chain =
                                                effective_chain_dspic_addr == Some(other_addr);
                                            warn!(
                                                addr = format_args!("0x{:02X}", other_addr),
                                                error = %e,
                                                target_is_effective_chain,
                                                require_real_ack = am2_env_flag("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK"),
                                                "Wave-25.10: 0x{:02X} ENABLE_VOLTAGE FAILED",
                                                other_addr
                                            );
                                            if target_is_effective_chain
                                                && am2_env_flag("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK")
                                            {
                                                if let Some(service) = i2c0_service.as_ref() {
                                                    let mut disable_addrs =
                                                        vec![selected_pic_addr, other_addr];
                                                    disable_addrs.sort_unstable();
                                                    disable_addrs.dedup();
                                                    disable_dspic_addrs_best_effort(
                                                        service,
                                                        &disable_addrs,
                                                        selected_pic_addr,
                                                        heartbeat_pic_fw,
                                                        "target-dspic-enable-failed",
                                                    );
                                                }
                                                force_am2_home_hard_stop(
                                                    &self.config,
                                                    "target-dspic-enable-failed",
                                                );
                                                self.shutdown.cancel();
                                                anyhow::bail!(
                                                    "Target chain dsPIC 0x{:02X} ENABLE_VOLTAGE failed under DCENT_AM2_REQUIRE_REAL_ENABLE_ACK=1; refusing BM1362 enum on {} without real target-rail ACK",
                                                    other_addr,
                                                    chain_uart_device
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let target_is_effective_chain =
                                        effective_chain_dspic_addr == Some(other_addr);
                                    warn!(
                                        addr = format_args!("0x{:02X}", other_addr),
                                        error = %e,
                                        target_is_effective_chain,
                                        "Wave-25.10: 0x{:02X} SetVoltage FAILED (best-effort, continuing)",
                                        other_addr
                                    );
                                    if target_is_effective_chain
                                        && am2_env_flag("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK")
                                    {
                                        if let Some(service) = i2c0_service.as_ref() {
                                            let mut disable_addrs =
                                                vec![selected_pic_addr, other_addr];
                                            disable_addrs.sort_unstable();
                                            disable_addrs.dedup();
                                            disable_dspic_addrs_best_effort(
                                                service,
                                                &disable_addrs,
                                                selected_pic_addr,
                                                heartbeat_pic_fw,
                                                "target-dspic-setvoltage-failed",
                                            );
                                        }
                                        force_am2_home_hard_stop(
                                            &self.config,
                                            "target-dspic-setvoltage-failed",
                                        );
                                        self.shutdown.cancel();
                                        anyhow::bail!(
                                            "Target chain dsPIC 0x{:02X} SetVoltage failed under DCENT_AM2_REQUIRE_REAL_ENABLE_ACK=1; refusing BM1362 enum on {} without target-rail proof",
                                            other_addr,
                                            chain_uart_device
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // Phase 3b: voltage feedback read — diagnostic only.
                //
                // B03 (2026-05-28): read BOTH the selected PIC and the
                // EFFECTIVE chain dsPIC, each addr-tagged. Before this fix the
                // single read used the selected-PIC handle (`pic`, bound to
                // `selected_pic_addr` = 0x20) and the log line carried NO `addr`
                // field, so under a `DCENT_AM2_CHAIN_UART_OVERRIDE` the effective
                // chain rail (e.g. 0x22 on ttyS3) was NEVER measured and the
                // reported `mv` was silently attributed to the wrong dsPIC. The
                // misattributed `mv=2064` reading wrongly implied the slot-3 rail
                // was dead when it was actually slot-1's (0x20) rail being read.
                // `read_voltage()` is diagnostic-only (I2C_RDWR). This does NOT
                // change the proven bosminer-handoff recipe: that path sets no
                // chain-UART override, so `effective_chain_dspic_addr ==
                // selected_pic_addr` and the second read below is skipped.
                // NOTE: a dsPIC ADC readback is still NOT physical rail proof —
                // only a DMM/scope on the hashboard DC-DC V+ pad is (see
                // ).
                std::thread::sleep(Duration::from_millis(150));
                match pic.read_voltage() {
                    Ok(mv) => {
                        info!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            mv,
                            target_mv = chip_rail_target_mv,
                            rail_proof = "ack-readback-not-physical-rail-proof",
                            t_ms = t(),
                            "[T+{}] dsPIC voltage feedback (selected PIC 0x{:02X}, mv={}, target_mv={}) — diagnostic only; readback is NOT physical rail proof",
                            t(),
                            selected_pic_addr,
                            mv,
                            chip_rail_target_mv
                        );
                        // G03 (gap-swarm, 2026-05-28): when the 0x3B read above
                        // succeeded (⇒ dsPIC parser is healthy), ALSO read the
                        // analog-ADC rail via MEASURE_VOLTAGE (0x3A =
                        // dspic33epxx_get_an_voltage2), distinct from 0x3B
                        // GET_VOLTAGE's setpoint/feedback path. 0x3A is the actual
                        // measured rail — the stronger "is the rail truly
                        // energized?" proxy for Procedure A. Read-only, non-fatal.
                        // Nested under the Ok arm (DCENT_EE N1) so we never add a
                        // back-to-back I2C_RDWR to an already-unhealthy parser.
                        match pic.measure_voltage() {
                            Ok(mv3a) => info!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                mv = mv3a,
                                target_mv = chip_rail_target_mv,
                                opcode = "0x3A-measure-analog-adc",
                                rail_proof = "ack-readback-not-physical-rail-proof",
                                t_ms = t(),
                                "[T+{}] dsPIC ANALOG-ADC rail measure (selected PIC 0x{:02X}, mv={}, target_mv={}, opcode=0x3A) — diagnostic only; stronger rail proxy than 0x3B, still NOT physical (DMM) proof",
                                t(),
                                selected_pic_addr,
                                mv3a,
                                chip_rail_target_mv
                            ),
                            Err(e) => warn!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                error = %e,
                                "dsPIC analog-ADC rail measure failed (selected PIC 0x{:02X}, opcode=0x3A) — diagnostic only; firmware may not answer 0x3A",
                                selected_pic_addr
                            ),
                        }
                    }
                    Err(e) => {
                        // BLK-5 / FWRE PATCH 4 (2026-06-10): 0x3B is the wrong opcode on
                        // fw=0x89/0x8A; its failure is a clean "wrong command", not parser
                        // ill-health, so fall through to the authoritative 0x3A measure
                        // instead of bailing. `a lab unit`-gated; read-only/diagnostic.
                        // MED-1 (DCENT_EE): fall through only on the clean fw=0x89/0x8A
                        // wrong-opcode refusal, never on a parser-health error (N1 rule).
                        let clean_3b_refusal = e.to_string().contains("measure_voltage(0x3A)");
                        warn!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            error = %e,
                            clean_3b_refusal,
                            "dsPIC 0x3B read failed (selected PIC 0x{:02X}) — clean fw=0x89 wrong-opcode refusal falls through to 0x3A; parser-health error does NOT (DCENT_EE N1)",
                            selected_pic_addr
                        );
                        if am2_xil_25_fingerprint_matches() && clean_3b_refusal {
                            match pic.measure_voltage() {
                                Ok(mv3a) => info!(
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    mv = mv3a,
                                    target_mv = chip_rail_target_mv,
                                    opcode = "0x3A-measure-analog-adc",
                                    rail_proof = "ack-readback-not-physical-rail-proof",
                                    t_ms = t(),
                                    "[T+{}] dsPIC ANALOG-ADC rail measure via BLK-5 fall-through (selected PIC 0x{:02X}, mv={}, target_mv={}, opcode=0x3A) — RAIL_UP when mv ~ target_mv; diagnostic only",
                                    t(),
                                    selected_pic_addr,
                                    mv3a,
                                    chip_rail_target_mv
                                ),
                                Err(e2) => warn!(
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    error = %e2,
                                    "dsPIC 0x3A measure also failed (selected PIC 0x{:02X}, BLK-5 fall-through) — firmware may not answer 0x3A",
                                    selected_pic_addr
                                ),
                            }
                        }
                    }
                }
                // B03: also measure the EFFECTIVE chain dsPIC when it differs
                // from the selected PIC (chain-UART override active). This is the
                // rail the BM1362 chips on the enumerated chain actually run
                // from; an `mv` far below `target_mv` here is the strongest
                // SOFTWARE signal that the on-hashboard DC-DC did not energize
                // despite an ENABLE ACK. It remains a proxy — corroborate, never
                // substitute, with a physical (DMM/scope) measurement.
                if let (Some(effective_addr), Some(service)) =
                    (effective_chain_dspic_addr, i2c0_service.as_ref())
                {
                    // Fix-A (2026-06-07): on `a lab unit` also enter this rail-verdict
                    // block when the effective chain dsPIC == the selected PIC
                    // (the standalone ttyS1 path: 0x20 == 0x20). Without the
                    // `|| fingerprint`, the whole block — including the LM75
                    // die-temp RAIL_UP/RAIL_COLD verdict + the 0x3A analog-ADC
                    // measure — is skipped on the ttyS1 path, so a standalone run
                    // produced NO rail verdict (the diagnostic the entire cold-wake
                    // decision tree depends on). Read-only/diagnostic; `a lab unit`-gated
                    // so the proven fleet + handoff are byte-identical.
                    if effective_addr != selected_pic_addr || am2_xil_25_fingerprint_matches() {
                        let mut chain_pic = Pic0x89Service::new_with_fw(
                            service.clone(),
                            effective_addr,
                            heartbeat_pic_fw,
                        );
                        std::thread::sleep(Duration::from_millis(150));
                        match chain_pic.read_voltage() {
                            Ok(mv) => {
                                info!(
                                    addr = format_args!("0x{:02X}", effective_addr),
                                    mv,
                                    target_mv = chip_rail_target_mv,
                                    rail_proof = "ack-readback-not-physical-rail-proof",
                                    t_ms = t(),
                                    "[T+{}] dsPIC voltage feedback (EFFECTIVE chain PIC 0x{:02X}, mv={}, target_mv={}) — diagnostic only; mv far below target_mv ⇒ on-hashboard DC-DC likely NOT energized despite ENABLE ACK (needs DMM proof)",
                                    t(),
                                    effective_addr,
                                    mv,
                                    chip_rail_target_mv
                                );
                                // G03: 0x3B succeeded ⇒ parser healthy ⇒ also take
                                // the analog-ADC measure (0x3A) on the EFFECTIVE
                                // chain dsPIC — the strongest SOFTWARE rail proxy
                                // for the chain the BM1362 chips actually run from.
                                // Read-only, non-fatal. Nested per DCENT_EE N1.
                                // Pair with a DMM (Procedure B) for physical proof.
                                match chain_pic.measure_voltage() {
                                    Ok(mv3a) => info!(
                                        addr = format_args!("0x{:02X}", effective_addr),
                                        mv = mv3a,
                                        target_mv = chip_rail_target_mv,
                                        opcode = "0x3A-measure-analog-adc",
                                        rail_proof = "ack-readback-not-physical-rail-proof",
                                        t_ms = t(),
                                        "[T+{}] dsPIC ANALOG-ADC rail measure (EFFECTIVE chain PIC 0x{:02X}, mv={}, target_mv={}, opcode=0x3A) — diagnostic only; mv far below target_mv ⇒ on-hashboard DC-DC likely NOT energized despite ENABLE ACK (corroborate with DMM, Procedure B)",
                                        t(),
                                        effective_addr,
                                        mv3a,
                                        chip_rail_target_mv
                                    ),
                                    Err(e) => warn!(
                                        addr = format_args!("0x{:02X}", effective_addr),
                                        error = %e,
                                        "dsPIC analog-ADC rail measure failed (EFFECTIVE chain PIC 0x{:02X}, opcode=0x3A) — diagnostic only; firmware may not answer 0x3A",
                                        effective_addr
                                    ),
                                }
                            }
                            Err(e) => {
                                // BLK-5 / FWRE PATCH 4 (2026-06-10): read_voltage(0x3B) is
                                // the WRONG opcode on fw=0x89/0x8A — its failure here is a
                                // clean protocol "wrong command" (the error literally says
                                // "use measure_voltage(0x3A)"), NOT a parser-health signal,
                                // so unlike a bare/unhealthy parser it is SAFE to fall
                                // through to the authoritative 0x3A analog-ADC read. This
                                // arm previously bailed to verdict=UNAVAILABLE while the
                                // real rail value (~13.0 V on `a lab unit`) sat undecoded in the
                                // raw dump (Team M BLK-5 / R-11). `a lab unit`-fingerprint-gated;
                                // read-only/diagnostic.
                                // MED-1 (DCENT_EE): only fall through when 0x3B failed with
                                // the clean fw=0x89/0x8A "wrong opcode" refusal (the error
                                // names measure_voltage(0x3A)). A NACK / 0xFF-frame / decode
                                // error means the parser may be UNHEALTHY, and firing a
                                // back-to-back 0x3A I2C_RDWR then would violate the N1 rule.
                                let clean_3b_refusal =
                                    e.to_string().contains("measure_voltage(0x3A)");
                                warn!(
                                    addr = format_args!("0x{:02X}", effective_addr),
                                    error = %e,
                                    clean_3b_refusal,
                                    "dsPIC 0x3B read failed (EFFECTIVE chain PIC 0x{:02X}) — a clean fw=0x89 wrong-opcode refusal falls through to 0x3A; a parser-health error does NOT (DCENT_EE N1)",
                                    effective_addr
                                );
                                if am2_xil_25_fingerprint_matches() && clean_3b_refusal {
                                    match chain_pic.measure_voltage() {
                                        Ok(mv3a) => info!(
                                            addr = format_args!("0x{:02X}", effective_addr),
                                            mv = mv3a,
                                            target_mv = chip_rail_target_mv,
                                            opcode = "0x3A-measure-analog-adc",
                                            rail_proof = "ack-readback-not-physical-rail-proof",
                                            t_ms = t(),
                                            "[T+{}] dsPIC ANALOG-ADC rail measure via BLK-5 fall-through (EFFECTIVE chain PIC 0x{:02X}, mv={}, target_mv={}, opcode=0x3A) — RAIL_UP when mv ~ target_mv; software ADC proxy, diagnostic only",
                                            t(),
                                            effective_addr,
                                            mv3a,
                                            chip_rail_target_mv
                                        ),
                                        Err(e2) => warn!(
                                            addr = format_args!("0x{:02X}", effective_addr),
                                            error = %e2,
                                            "dsPIC 0x3A measure also failed (EFFECTIVE chain PIC 0x{:02X}, BLK-5 fall-through) — firmware may not answer 0x3A",
                                            effective_addr
                                        ),
                                    }
                                }
                            }
                        }
                        //  (2026-05-29) DIE-TEMP RAIL PROXY: read the LM75
                        // board sensors (0x48-0x4B) via the framed (fw=0x89) chain
                        // dsPIC passthrough. If the chip rail is PHYSICALLY up, the
                        // BM1362 chips draw quiescent power and the board warms well
                        // above ambient by this point (~tens of seconds after
                        // ENABLE); if the rail never engaged, the board sits at
                        // ambient (~room temp). This is the only no-DMM
                        // rail-physical signal that does NOT depend on the
                        // (unimplemented-for-framed) 0x3A/0x3B voltage decoder.
                        // Read-only, non-fatal. Pair with a DMM (Procedure B) for
                        // ground truth.
                        //
                        // 2026-05-31 — the legacy `read_temperature` call below
                        // uses CMD_READ_TEMP (0x30) + a 4-byte read, which on
                        // `a lab unit` is NOT the LM75-passthrough opcode → returns NaN
                        // ("rail signal unavailable"). When
                        // `DCENT_AM2_LM75_RAIL_PROXY=1` AND the `a lab unit` fingerprint
                        // matches, read instead via the bosminer-proven 0x3B/0x3C
                        // 6-byte PASSTHROUGH (`read_lm75_passthrough_temp`) and
                        // emit a 3-state autonomous (no-DMM) rail verdict
                        // (DECODES → ABOVE-AMBIENT → DELTA). Default-off +
                        // `a lab unit`-fingerprinted ⇒ every other unit keeps the legacy
                        // `read_temperature` path byte-identically. DIAGNOSTIC
                        // ONLY: reads LM75 temps + logs a verdict; touches NO
                        // mining/voltage/enum control flow; safe if the dsPIC
                        // doesn't answer (verdict = UNAVAILABLE, never panics).
                        let lm75_rail_proxy_active =
                            am2_lm75_rail_proxy_enabled() && am2_xil_25_fingerprint_matches();

                        if lm75_rail_proxy_active {
                            // Max LM75 die temp across the 4 board sensors via the
                            // `a lab unit` 0x3B/0x3C passthrough. Returns (max_temp_c,
                            // sensors_ok). max is NaN if no sensor decoded.
                            let read_lm75_max = |pic: &mut Pic0x89Service| -> (f64, u8) {
                                let mut max_c = f64::NAN;
                                let mut ok = 0u8;
                                for sensor in [0x48u8, 0x49, 0x4A, 0x4B] {
                                    if let Ok(c) = pic.read_lm75_passthrough_temp(sensor) {
                                        // Reject implausible decodes (sensor absent
                                        // / bus noise) — only plausible board temps
                                        // count as "DECODES".
                                        if c.is_finite() && (5.0..=110.0).contains(&c) {
                                            ok += 1;
                                            if !max_c.is_finite() || c > max_c {
                                                max_c = c;
                                            }
                                        }
                                    }
                                }
                                (max_c, ok)
                            };

                            // First read.
                            let (temp1, sensors_ok1) = read_lm75_max(&mut chain_pic);

                            // Optional DELTA read ~25 s later (a quiet home unit
                            // warms slowly — DELTA threshold is +1.5 C, NOT +5 C).
                            // Only attempt the 2nd read if the 1st DECODED, else
                            // the verdict is already UNAVAILABLE.
                            let delta_ms =
                                am2_env_u64("DCENT_AM2_LM75_RAIL_PROXY_DELTA_MS", 25_000)
                                    .clamp(0, 60_000);
                            let (temp2, sensors_ok2) = if temp1.is_finite() && delta_ms > 0 {
                                std::thread::sleep(Duration::from_millis(delta_ms));
                                read_lm75_max(&mut chain_pic)
                            } else {
                                (f64::NAN, 0u8)
                            };

                            // Verdict: pick the warmer of the two reads as the
                            // representative die temp; compute the rise if both
                            // decoded.
                            let lm75 = if temp2.is_finite() && temp1.is_finite() {
                                temp1.max(temp2)
                            } else if temp1.is_finite() {
                                temp1
                            } else {
                                temp2
                            };
                            let delta = if temp1.is_finite() && temp2.is_finite() {
                                temp2 - temp1
                            } else {
                                f64::NAN
                            };

                            // 3-state autonomous (no-DMM) rail verdict.
                            // ⚠️ R5 (2026-05-31): this verdict is a HEURISTIC
                            // INFERENCE from die TEMPERATURE — 0x3B/0x3C(0x48) are
                            // LM75A temperature reads (°C), NOT a rail-voltage read
                            // (RE: `reference_wave58_25_reanalysis_0x3c_is_lm75...`).
                            // "RAIL_UP" here means "chips are warm ⇒ probably drawing
                            // power"; it is NOT a measured rail mV and must NEVER gate
                            // mining/voltage/enum. Observability-only.
                            //  - UNAVAILABLE: nothing decoded → dsPIC passthrough
                            //    not alive.
                            //  - RAIL_UP: chips are warming the board — either the
                            //    DELTA rose >=1.5 C after ENABLE, OR the absolute
                            //    die temp is meaningfully above room temp (>28 C).
                            //  - RAIL_COLD: decoded but sitting at ~ambient and not
                            //    rising → rail likely NOT energized.
                            let delta_up = delta.is_finite() && delta >= 1.5;
                            let above_ambient = lm75.is_finite() && lm75 > 28.0;
                            let verdict = if !lm75.is_finite() {
                                "UNAVAILABLE"
                            } else if delta_up || above_ambient {
                                "RAIL_UP"
                            } else {
                                "RAIL_COLD"
                            };

                            // The single clear verdict line.
                            info!(
                                addr = format_args!("0x{:02X}", effective_addr),
                                lm75_c = lm75,
                                delta_c = delta,
                                sensors_ok_read1 = sensors_ok1,
                                sensors_ok_read2 = sensors_ok2,
                                delta_ms,
                                above_ambient,
                                delta_up,
                                verdict,
                                rail_proof = "lm75-die-temp-proxy-not-physical-dmm",
                                t_ms = t(),
                                "[T+{}] AM2 .25 rail-proxy verdict: lm75={:.2}C delta={:.2}C verdict={} (EFFECTIVE chain PIC 0x{:02X}, 0x3B/0x3C passthrough) — RAIL_UP = chips drawing power (warming board); RAIL_COLD = decoded but ~ambient & not rising; UNAVAILABLE = dsPIC passthrough silent. No-DMM rail-physical proxy (corroborate with DMM, Procedure B).",
                                t(),
                                lm75,
                                delta,
                                verdict,
                                effective_addr
                            );
                        } else {
                            // Legacy path (byte-identical for non-`a lab unit` / env unset):
                            // CMD_READ_TEMP (0x30) + 4-byte read. On `a lab unit` this
                            // returns NaN (0x30 is not the LM75-passthrough opcode);
                            // on other AM2 fw=0x89 units it may decode.
                            let mut board_temp_max: f64 = f64::NAN;
                            let mut sensors_ok = 0u8;
                            for sensor in [0x48u8, 0x49, 0x4A, 0x4B] {
                                if let Ok(c) = chain_pic.read_temperature(sensor) {
                                    if c.is_finite() {
                                        sensors_ok += 1;
                                        if !board_temp_max.is_finite() || c > board_temp_max {
                                            board_temp_max = c;
                                        }
                                    }
                                }
                            }
                            if board_temp_max.is_finite() {
                                info!(
                                    addr = format_args!("0x{:02X}", effective_addr),
                                    board_temp_c = board_temp_max,
                                    sensors_ok,
                                    t_ms = t(),
                                    "[T+{}] DIE-TEMP RAIL PROXY (EFFECTIVE chain PIC 0x{:02X}): max LM75 board temp = {:.2} C across {} sensor(s) — WELL ABOVE ambient (~room temp) ⇒ chip rail is PHYSICALLY delivering power (chips drawing); ~ambient ⇒ rail NOT engaged. No-DMM rail-physical signal (corroborate with DMM, Procedure B).",
                                    t(),
                                    effective_addr,
                                    board_temp_max,
                                    sensors_ok
                                );
                            } else {
                                warn!(
                                    addr = format_args!("0x{:02X}", effective_addr),
                                    t_ms = t(),
                                    "[T+{}] DIE-TEMP RAIL PROXY (EFFECTIVE chain PIC 0x{:02X}): LM75 board temp unreadable via dsPIC passthrough (all 4 sensors NaN/err) — rail-physical signal unavailable; DMM (Procedure B) is the remaining option",
                                    t(),
                                    effective_addr
                                );
                            }
                        }

                        // RAW FRAMED TELEMETRY DUMP (2026-05-29, RE diagnostic):
                        // the production read_voltage/measure_voltage above do a
                        // COMBINED 4-byte I2C_RDWR read, which garbles the FRAMED
                        // (fw=0x89) reply (live: dsPIC 0x22 decoded as 64760 mV).
                        // The framed reply is actually a longer BYTE-WISE frame
                        // `[cmd_echo, status, v_hi, v_lo, …, CKSUM]` (~9 bytes).
                        // Dump the RAW byte-wise reply so we can SEE whether 0x22
                        // returns real data or only an all-echo (FW-byte 0x8A
                        // repeated). Purely additive — does NOT replace the
                        // (failing) read_voltage/measure_voltage calls above.
                        // Read-only, non-fatal, NEVER on the hot mining path.
                        info!(
                            addr = format_args!("0x{:02X}", effective_addr),
                            t_ms = t(),
                            "[T+{}] RAW FRAMED TELEMETRY DUMP (0x{:02X}): dumping byte-wise GET_VOLTAGE (0x3B) + MEASURE (0x3A) replies on the EFFECTIVE chain dsPIC — diagnostic only, read-only",
                            t(),
                            effective_addr
                        );
                        // GET_VOLTAGE (0x3B): try the full framed length (9) first,
                        // then the production combined length (6) as a comparator.
                        let raw_3b_9 = chain_pic.dump_voltage_raw(9);
                        let raw_3b_6 = chain_pic.dump_voltage_raw(6);
                        // MEASURE_VOLTAGE (0x3A) analog-ADC rail read, raw byte-wise.
                        let raw_3a_9 = chain_pic.dump_measure_raw(9);
                        let raw_3a_6 = chain_pic.dump_measure_raw(6);
                        info!(
                            addr = format_args!("0x{:02X}", effective_addr),
                            get_voltage_3b_len9 = format_args!("{:02X?}", raw_3b_9),
                            get_voltage_3b_len6 = format_args!("{:02X?}", raw_3b_6),
                            measure_3a_len9 = format_args!("{:02X?}", raw_3a_9),
                            measure_3a_len6 = format_args!("{:02X?}", raw_3a_6),
                            t_ms = t(),
                            "[T+{}] RAW FRAMED TELEMETRY DUMP (0x{:02X}) summary — empty Vec means the byte-wise read was incomplete (partial bytes are in the per-call warn line above). All-0x8A repeats ⇒ FW-byte echo only (no real telemetry); a leading [3B/3A echo, status, v_hi, v_lo, …] ⇒ real framed data. Diagnostic only.",
                            t(),
                            effective_addr
                        );
                    }
                }
            }

            if am2_diag_stop_after_dspic_enable_enabled() {
                warn!(
                    t_ms = t(),
                    env_gate = "DCENT_AM2_DIAG_STOP_AFTER_DSPIC_ENABLE=1",
                    selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                    active_chains = format_args!("0b{:03b}", active_chains),
                    active_dspic_addrs = format_args!("{:02X?}", active_dspic_addrs(active_chains)),
                    "AM2 diagnostic stop after dsPIC SetVoltage/ENABLE classification — \
                     no chain UART probe, no BM1362 init, no Stratum, no work dispatch"
                );

                if let Some(service) = i2c0_service.as_ref() {
                    let mut disable_addrs = active_dspic_addrs(active_chains);
                    if !disable_addrs.contains(&selected_pic_addr) {
                        disable_addrs.push(selected_pic_addr);
                    }
                    disable_addrs.sort_unstable();
                    disable_addrs.dedup();

                    for addr in disable_addrs {
                        let fw_hint = if addr == selected_pic_addr {
                            heartbeat_pic_fw
                        } else {
                            None
                        };
                        let fw_hint_log = fw_hint
                            .map(|fw| format!("0x{:02X}", fw))
                            .unwrap_or_else(|| "auto".to_string());
                        let mut shutdown_pic =
                            Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint);
                        if let Err(hb_err) = shutdown_pic.send_heartbeat() {
                            warn!(
                                error = %hb_err,
                                addr = format_args!("0x{:02X}", addr),
                                "PIC heartbeat before diagnostic disable failed (continuing)"
                            );
                        }
                        match shutdown_pic.disable_voltage() {
                            Ok(()) => info!(
                                addr = format_args!("0x{:02X}", addr),
                                fw_hint = %fw_hint_log,
                                "PIC voltage disabled for dsPIC-enable diagnostic stop"
                            ),
                            Err(disable_err) => warn!(
                                error = %disable_err,
                                addr = format_args!("0x{:02X}", addr),
                                fw_hint = %fw_hint_log,
                                "PIC voltage disable failed during dsPIC-enable diagnostic stop; \
                                 forcing PWR_CONTROL low next"
                            ),
                        }
                    }
                } else {
                    warn!(
                        "AM2 I2C service missing during dsPIC-enable diagnostic stop; \
                         forcing PWR_CONTROL low next"
                    );
                }

                force_am2_home_hard_stop(&self.config, "diag-stop-after-dspic-enable");
                shutdown_am2_psu_after_feeders_bounded(
                    &self.config,
                    &mut runtime_threads,
                    &psu_arc,
                    "diag-stop-after-dspic-enable",
                )
                .await;

                anyhow::bail!(
                    "DCENT_AM2_DIAG_STOP_AFTER_DSPIC_ENABLE=1: diagnostic stop after \
                     dsPIC SetVoltage/ENABLE classification (no chain UART probe, no \
                     BM1362 init, no Stratum, no work dispatch)"
                );
            }

            // Phase 3d (MOVED EARLIER 2026-04-26 evening): start PIC heartbeat
            // thread IMMEDIATELY after ENABLE so the dsPIC's voltage controller
            // sees continuous heartbeat traffic during the DC-DC ramp window.
            // Live evidence on `a lab unit` showed: SetVoltage OK + ENABLE bare ACK
            // + 1.85 s gap with no heartbeat → 0 bytes from chain UART. The
            // dsPIC fw=0x86 may auto-disengage the rail on heartbeat timeout;
            // bosminer's heartbeat is ~1 Hz from boot, so we match.
            info!(
                t_ms = t(),
                "[T+{}] Phase 3d: Starting PIC heartbeat thread (moved before chain UART probe)",
                t()
            );
            let heartbeat_i2c = i2c0_service
                .as_ref()
                .context("AM2 I2C service missing for PIC heartbeat")?
                .clone();
            // H-heartbeat-0x22 (swarm wf_7b37bed4 verify, 2026-05-29): when
            // `DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS=1`, also keepalive the
            // EFFECTIVE chain dsPIC (0x22 on the `a lab unit` slot-3 path) so slot-3's
            // rail isn't watchdog-cut before the first enum.
            //
            // CORRECTION (adversarial verify): the extras set is now the
            // effective-chain dsPIC ONLY, NOT a blanket `active_dspic_addrs(
            // active_chains)` enumeration. `active_chains` is hardcoded 0b111 at
            // this call site, so the old base yielded `[0x20,0x21,0x22]` →
            // included the PHYSICALLY ABSENT middle slot 0x21 on `a lab unit`. A
            // heartbeat to 0x21 NACKs every tick → the I2C service tears down +
            // reopens the shared `/dev/i2c-0` fd ~1×/s → bus instability during
            // enum. The pure `heartbeat_extra_addrs` helper (host-tested in
            // dcentrald-common) returns at most the effective chain dsPIC and
            // can never name an empty slot. Default-OFF → empty vec → only
            // `selected_pic_addr` is heartbeated, byte-for-byte unchanged for
            // the proven fleet and the  bosminer-handoff path.
            let heartbeat_additional_addrs: Vec<u8> = if am2_heartbeat_all_active_pics_enabled() {
                let extras = dcentrald_common::dspic_heartbeat::heartbeat_extra_addrs(
                    selected_pic_addr,
                    effective_chain_dspic_addr,
                );
                info!(
                    t_ms = t(),
                    selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                    additional_addrs = format_args!("{:02X?}", extras),
                    "[T+{}] Phase 3d: DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS — \
                         heartbeating selected dsPIC + every other active dsPIC \
                         (incl. effective chain dsPIC)",
                    t()
                );
                extras
            } else {
                Vec::new()
            };
            runtime_threads.push(
                "s19j-pic-heartbeat",
                spawn_pic_heartbeat_thread(
                    heartbeat_i2c,
                    selected_pic_heartbeat_controller.take().context(
                        "exact AM2 Pic0x89 endpoint-issued controller missing before heartbeat ownership handoff",
                    )?,
                    heartbeat_additional_addrs,
                    self.shutdown.clone(),
                )?,
            );

            // Phase 3c: voltage settle window before chain UART activity.
            //
            //  S9-pattern timing parity (default 4000 ms): S9 BraiinsOS
            // waits ~4 s after PIC voltage enable before any chain UART
            // traffic, allowing BM1362 PLL + clock distribution to stabilise
            // at 13.7V. Legacy am2 hybrid waited only 1200 ms.
            //
            // Heartbeats continue firing 1× during this window keeping the
            // rail engaged. Configurable via `[mining] am2_post_enable_settle_ms`
            // — set to 1200 to revert to the legacy short settle.
            let settle_ms = self.config.mining.am2_post_enable_settle_ms;
            info!(
                t_ms = t(),
                settle_ms,
                "[T+{}] Phase 3c: post-ENABLE settle window ({} ms, S9 4 s pattern)",
                t(),
                settle_ms
            );
            std::thread::sleep(Duration::from_millis(settle_ms));

            // Phase 3c-pre-gate: diagnostic-only glitch monitor mirror,
            // Braiins-am2 only (W13.B1, 2026-05-10).
            //
            // RECLASSIFIED: 0x43D00030/0x43D00034 are read-only Braiins-am2
            // glitch monitor STATUS MIRRORS of BM1362 ASIC reg 0x2C — NOT
            // a control surface. Stock CV1835/AM335x/AML/S9 do NOT populate
            // this IP. Phase 9A proved the writes are silent NO-OPs even
            // on Braiins-am2. BM1362 0x2C/0x34 candidate relay broadcasts
            // are lab-gated by `DCENT_BM1362_ENABLE_UART_RELAY_LAB`.
            //
            // This block remains as opt-in (default-off) telemetry parity
            // with bosminer's own glitch_monitor.rs path, gated by
            // `am2_force_braiins_glitch_mirror_write` (lab-only).
            if !passthrough && self.config.mining.am2_force_braiins_glitch_mirror_write {
                match open_braiins_glitch_monitor_for_logging() {
                    Some(monitor) => {
                        if let Some(offset) = chain_glitch_status_offset(relay_phys_idx) {
                            let pre_read = monitor.read_word(offset).unwrap_or(0);
                            let target_value: u32 = BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT;
                            match monitor.force_braiins_glitch_status_mirror_write(relay_phys_idx) {
                                Ok(_report) => {
                                    let post_read = monitor.read_word(offset).unwrap_or(0);
                                    info!(
                                        chain_id = fpga_chain_id,
                                        phys_idx = relay_phys_idx,
                                        offset = format_args!("0x{:02X}", offset),
                                        wrote = format_args!("0x{:08X}", target_value),
                                        pre_read = format_args!("0x{:08X}", pre_read),
                                        post_read = format_args!("0x{:08X}", post_read),
                                        "Phase 3c-pre-gate Braiins glitch mirror force-write attempt (diagnostic-only, Braiins-am2)"
                                    );
                                    if post_read != target_value {
                                        warn!(
                                            chain_id = fpga_chain_id,
                                            phys_idx = relay_phys_idx,
                                            offset = format_args!("0x{:02X}", offset),
                                            wrote = format_args!("0x{:08X}", target_value),
                                            post_read = format_args!("0x{:08X}", post_read),
                                            "Phase 3c-pre-gate glitch mirror readback != write (expected Phase 9A NO-OP). BM1362 relay candidate writes are lab-gated."
                                        );
                                    }
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                Err(e) => {
                                    warn!(
                                        chain_id = fpga_chain_id,
                                        phys_idx = relay_phys_idx,
                                        offset = format_args!("0x{:02X}", offset),
                                        error = %e,
                                        "Phase 3c-pre-gate Braiins glitch mirror write FAILED (diagnostic-only)"
                                    );
                                }
                            }
                        } else {
                            warn!(
                                chain_id = fpga_chain_id,
                                phys_idx = relay_phys_idx,
                                "Phase 3c-pre-gate: no glitch-status offset for phys_idx (chain1=2 / chain4=3 only); skipping"
                            );
                        }
                    }
                    None => {
                        warn!("Phase 3c-pre-gate BraiinsGlitchMonitor::open() failed; skipping diagnostic write (stock hw / no Braiins-am2 IP)");
                    }
                }
            } else if passthrough {
                info!("Phase 3c-pre-gate: passthrough mode — diagnostic-only glitch mirror write skipped");
            } else {
                info!(
                    "Phase 3c-pre-gate: am2_force_braiins_glitch_mirror_write disabled — skipping diagnostic-only mirror write"
                );
            }

            // Phase 3c-pre-gate (UIO path): diagnostic-only glitch monitor
            // mirror write attempt — Braiins-am2 only (W13.B1, 2026-05-10).
            //
            // RECLASSIFIED: bosminer's `/dev/uio18` write through the
            // `miner-glitch-monitor` IP targets the SAME read-only mirror
            // surface as the /dev/mem path above. Phase 9A proved both
            // routes are silent NO-OPs at the FPGA fabric. BM1362
            // 0x2C/0x34 candidate relay broadcasts are lab-gated by
            // `DCENT_BM1362_ENABLE_UART_RELAY_LAB`.
            //
            // We retain this UIO write attempt for telemetry parity with
            // bosminer (lab-only) — gated by `am2_force_braiins_glitch_mirror_write`.
            if !passthrough && self.config.mining.am2_force_braiins_glitch_mirror_write {
                // Use the long-lived BraiinsGlitchMonitor instance opened
                // during early init (Braiins-am2 only — `None` on stock hw).
                match self.glitch_monitor.as_ref() {
                    Some(gm) => match gm.force_braiins_glitch_status_mirror_write(relay_phys_idx) {
                        Ok(attempt) => {
                            info!(
                                chain_id = fpga_chain_id,
                                phys_idx = relay_phys_idx,
                                uio = gm.uio_number(),
                                post_read = format_args!("0x{:08X}", attempt.post_read),
                                "Phase 3c-pre-gate: Braiins glitch mirror UIO write attempt (cached fd, diagnostic-only)"
                            );
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(e) => warn!(
                            chain_id = fpga_chain_id,
                            phys_idx = relay_phys_idx,
                            uio = gm.uio_number(),
                            error = %e,
                            "Phase 3c-pre-gate: Braiins glitch mirror UIO write FAILED (diagnostic-only)"
                        ),
                    },
                    None => {
                        // No BraiinsGlitchMonitor available (early init
                        // failed, or IP block not present on this stock
                        // hardware). This is the expected case for stock
                        // CV1835/AM335x/AML/S9; BM1362 0x2C/0x34
                        // candidate relay broadcasts are lab-gated.
                        info!(
                            chain_id = fpga_chain_id,
                            phys_idx = relay_phys_idx,
                            "Phase 3c-pre-gate: no cached BraiinsGlitchMonitor — UIO mirror write skipped (stock hw or early-init log)"
                        );
                    }
                }
            }

            // Phase 3b1-relay: send BM1362 UART relay enable BEFORE the chain
            // UART RX gate ( kernel-module RE finding). The MMIO at
            // 0x43D00030/34 is a read-only status mirror — the actual
            // relay-enable mechanism is a serial broadcast write to BM1362
            // chip register 0x2C (and 0x34 alt). Chips do NOT emit on chain
            // UART until they receive this broadcast. The gate at Phase 3b2
            // expects chip chatter that requires this broadcast — moving the
            // broadcast BEFORE the gate breaks the chicken-and-egg problem.
            //
            // init_asic_chain (Phase 4-7) ALSO sends this broadcast (at
            // line ~1547); double-write is harmless (chip has same value
            // already). We send here as well so the chain UART RX gate has a
            // chance to see chip output before init_asic_chain runs.
            match dcentrald_hal::serial_chain::SerialChainBackend::open(
                fpga_chain_id,
                &chain_uart_device,
                115_200,
            ) {
                Ok(serial_pre) => {
                    let relay_result =
                        maybe_write_bm1362_uart_relay(&serial_pre, "bm1362_phase3b1_pre_gate");
                    std::thread::sleep(Duration::from_millis(200));
                    info!(
                        chain_id = fpga_chain_id,
                        serial_device = chain_uart_device.as_str(),
                        relay_lab_enabled = bm1362_uart_relay_lab_enabled(),
                        relay_result = ?relay_result.as_ref().map(|_| ()),
                        "Phase 3b1-relay: optional BM1362 UART relay lab stage complete"
                    );
                    relay_result?;
                    drop(serial_pre);
                }
                Err(e) => warn!(
                    chain_id = fpga_chain_id,
                    serial_device = chain_uart_device.as_str(),
                    error = %e,
                    "Phase 3b1-relay: SerialChainBackend::open failed; skipping pre-gate relay enable"
                ),
            }

            // Phase 3b2b: post-ENABLE chain UART rail-engagement probe.
            // APW121215a has no voltage/current feedback (`psu.rs:493`
            // `has_voltage_feedback() == false`) and dsPIC fw=0x86 bare
            // GET_VOLTAGE only echoes the FW byte, so we cannot infer rail
            // engagement from the PSU or PIC. The only software signal is
            // whether the BM1362 ASICs themselves drive any byte onto the
            // chain UART RX line after the DC-DC ramp. See
            //  and
            // .
            //
            // : this gate is now informational (warn instead of bail!).
            // Authoritative success is the GetAddress enumeration count in
            // init_asic_chain.
            //
            // 2026-06-11 (LIVE-PINNED, DCENT_FPGA review): enable the FPGA UART
            // RETURN relay (gpio@41220000) HERE, before the rail-evidence probe.
            // This clears the v+2 low-bit GPIO confound before reading RX. Live
            // v+2 proved the GPIO can match bosminer and still produce enum=0,
            // so a zero-byte probe is not a complete root-cause discriminator.
            // Non-zero bytes are useful early evidence; GetAddress remains
            // authoritative. Gated default-OFF + `a lab unit` fp + !handoff; idempotent;
            // fail-closed when set.
            // P3 (2026-06-13): the relay RMW drives gpio895/896 — which are ALSO the
            // PSU SMBus SDA/SCL bit-bang lines (psu_gpio_i2c.rs). The .25 fingerprint
            // already requires a Loki-or-unset psu_hardware_variant, but a smart-PSU
            // .25-class board with the variant UNSET could still reach here. Require
            // the dumb-PSU bypass to own the bus before driving those bits as outputs.
            // Defense-in-depth; benign on the real .25 (Loki = dumb APW3 = override on).
            // Fires before init_asic_chain's relay call too (this run() site is earlier),
            // so it covers both relay call sites. Uses the run-scope psu_override_active.
            if am2_fpga_uart_relay_cold_enabled() && !psu_override_active {
                anyhow::bail!(
                    "DCENT_AM2_FPGA_UART_RELAY_COLD set but [power.psu_override] is not active — the relay RMW drives gpio895/896 (PSU SMBus SDA/SCL); refusing unless the dumb-PSU bypass owns the bus"
                );
            }
            am2_try_enable_fpga_uart_relay_cold("pre-rail-probe (post-ENABLE)")?;
            let rx_bytes_pre_init = match post_enable_chain_uart_probe(
                fpga_chain_id,
                &chain_uart_device,
                selected_pic_addr,
            ) {
                Ok(total) => total,
                Err(e) => {
                    warn!(
                        error = %e,
                        selected_pic_serial_device = %serial_device,
                        effective_chain_uart_device = %chain_uart_device,
                        allow_fallback_scan = am2_env_flag("DCENT_AM2_UART_FALLBACK_SCAN"),
                        "Configured chain UART RX gate failed"
                    );
                    if am2_env_flag("DCENT_AM2_UART_FALLBACK_SCAN")
                        && !open_core_rail_plan.requires_demotion()
                    {
                        warn!(
                            selected_pic_serial_device = %serial_device,
                            effective_chain_uart_device = %chain_uart_device,
                            "DCENT_AM2_UART_FALLBACK_SCAN=1; scanning all AM2 UART candidates before power teardown"
                        );
                        if let Some((fallback_device, chip_like_count)) =
                            Self::probe_uart_for_chips(&chain_uart_device)
                        {
                            warn!(
                                selected_pic_serial_device = %serial_device,
                                previous_chain_uart_device = %chain_uart_device,
                                selected_serial = %fallback_device,
                                chip_like_count,
                                "AM2 UART fallback found chip responses after configured UART gate failure"
                            );
                            chain_uart_device = fallback_device;
                            chip_like_count
                        } else {
                            warn!(
                                error = %e,
                                "Post-ENABLE chain UART RX gate failed after opt-in fallback scan; tearing down PIC/PSU watchdog state before returning"
                            );
                            let _ = pic.send_heartbeat();
                            match pic.disable_voltage() {
                                Ok(()) => info!(
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    "PIC voltage disabled after chain UART RX gate failure"
                                ),
                                Err(disable_err) => warn!(
                                    error = %disable_err,
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    "PIC voltage disable failed after chain UART RX gate failure"
                                ),
                            }
                            force_am2_home_hard_stop(
                                &self.config,
                                "chain-uart-rx-gate-failed-after-fallback",
                            );
                            shutdown_am2_psu_after_feeders_bounded(
                                &self.config,
                                &mut runtime_threads,
                                &psu_arc,
                                "chain-uart-rx-gate-failed-after-fallback",
                            )
                            .await;

                            // PWR_CONTROL gate auto-deasserts when the
                            // surviving `psu_arc` reference is dropped on
                            // return — owned by `Apw121215a::Drop`.

                            return Err(e).context("Post-ENABLE chain UART RX gate failed");
                        }
                    } else {
                        warn!(
                            error = %e,
                            elevated_rail_owner_locked = open_core_rail_plan.requires_demotion(),
                            fallback_scan_requested = am2_env_flag("DCENT_AM2_UART_FALLBACK_SCAN"),
                            "Post-ENABLE chain UART RX gate failed; fallback UART scan is disabled unless explicitly requested and is always refused while an elevated rail is owned by the selected PIC; tearing down PIC/PSU watchdog state before returning"
                        );
                        let _ = pic.send_heartbeat();
                        match pic.disable_voltage() {
                            Ok(()) => info!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                "PIC voltage disabled after chain UART RX gate failure"
                            ),
                            Err(disable_err) => warn!(
                                error = %disable_err,
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                "PIC voltage disable failed after chain UART RX gate failure"
                            ),
                        }
                        force_am2_home_hard_stop(&self.config, "chain-uart-rx-gate-failed");
                        shutdown_am2_psu_after_feeders_bounded(
                            &self.config,
                            &mut runtime_threads,
                            &psu_arc,
                            "chain-uart-rx-gate-failed",
                        )
                        .await;

                        // PWR_CONTROL gate auto-deasserts when the surviving
                        // `psu_arc` reference is dropped on return — owned
                        // by `Apw121215a::Drop`.

                        return Err(e).context("Post-ENABLE chain UART RX gate failed");
                    }
                }
            };
            // Phase 3.5: post-ENABLE UART evidence is diagnostic only. A
            // non-zero count is useful early evidence; 0 bytes does not prove
            // rail failure and must not be routed back to the falsified
            // chip-register relay hypothesis. GetAddress enumeration remains
            // the authoritative software success signal.
            if rx_bytes_pre_init > 0 {
                info!(
                    rx_bytes_pre_init,
                    t_ms = t(),
                    chain_uart_device = %chain_uart_device,
                    "[T+{}] chain UART RX poll {} bytes accumulated_total={} (early UART evidence; GetAddress remains authoritative)",
                    t(),
                    rx_bytes_pre_init,
                    rx_bytes_pre_init
                );
            } else {
                warn!(
                    rx_bytes_pre_init,
                    t_ms = t(),
                    chain_uart_device = %chain_uart_device,
                    "[T+{}] chain UART RX poll observed 0 bytes; rail remains unproven until GetAddress enumeration",
                    t()
                );
            }

            // Phase 4/5/6/7: chain reset + enum + init + baud selection.
            //
            //  (2026-05-26) historical diagnostic: decouple Phase 4-7
            // chain UART from `selected_pic_addr`'s `serial_device`. When
            // `DCENT_AM2_CHAIN_UART_OVERRIDE` is set, use that path for chain
            // enum/work dispatch while `selected_pic_addr` stays derived from
            // `serial_device`. Current  standalone diagnostics keep this
            // override unset and use the proven `/dev/ttyS1` route; `/dev/ttyS3`
            // is now treated as a separate dual-chain validation path.
            // Fix-C (2026-06-07): pre-program the FPGA chain-common block
            // (CTRL=0x00901002 + BAUD=0x6C) BEFORE the cold enum. In
            // SERIAL_WORK_DISPATCH mode the FPGA WORK_TX FIFO is bypassed, so DCENT
            // only writes the chain-common CTRL at Phase 8 (POST-enum) — which on a
            // cold standalone boot never runs (enum fails first). Under bosminer
            // (the warm handoff) this register was already 0x00901002 when DCENT
            // took over. Per the OUT2 evidence (serial.rs: OUT2 gates "the FPGA UART
            // block's TX clock-out"), the FPGA UART block's IP_ENABLE + clk-enable
            // bits can gate the chain UART TX clock, so set this before cold enum
            // to clear that confound. Later live evidence showed cold CTRL/BAUD
            // already matched bosminer in the failing standalone run, so this is
            // not the complete blocker. Default-OFF + `a lab unit`-fingerprint → the
            // proven fleet + handoff are byte-identical (they never set this gate).
            // Set once before the retry loop; the register persists in FPGA
            // hardware (HB_RESET re-pulses the chips, not the FPGA chain-common
            // block).
            // 2026-06-09: skip Fix-C entirely on the kernel-UART transport. Fix-C
            // opens the FPGA chain-common (uio0..uio3), but uio3 (chain1-work-tx)
            // is UNBOUND on the kernel-UART path (IRQ 165 freed for of_serial) ->
            // /dev/uio3 ENODEV (LIVE TEST 13). In SERIAL_WORK_DISPATCH the work-tx
            // FIFO is bypassed anyway.
            if am2_env_flag("DCENT_AM2_FPGA_CTRL_PREINIT")
                && am2_xil_25_fingerprint_matches()
                && !am2_prefer_kernel_uart()
            {
                let fpga_pre = if fpga_chain_id == 1 && std::path::Path::new("/dev/uio0").exists() {
                    FpgaChain::open_am2(fpga_chain_id, 0).map(Am2FpgaChain::Uio)
                } else {
                    DevmemFpgaChain::open_am2(fpga_chain_id, fpga_base).map(Am2FpgaChain::Devmem)
                };
                match fpga_pre {
                    Ok(fpga_pre) => {
                        let ctrl_before = fpga_pre.read_ctrl();
                        // write_ctrl refuses any IP_ENABLE-clear value (incl. 0), so
                        // this can never brick the UART state machine.
                        fpga_pre.write_ctrl(fpga_chain::ctrl_am2::BM1362_DEFAULT);
                        fpga_pre.set_baud(fpga_chain::BAUD_REG_115200);
                        let ctrl_after = fpga_pre.read_ctrl();
                        info!(
                            ctrl_before = format_args!("0x{:08X}", ctrl_before),
                            ctrl_after = format_args!("0x{:08X}", ctrl_after),
                            baud = format_args!("0x{:02X}", fpga_chain::BAUD_REG_115200),
                            chain = fpga_chain_id,
                            t_ms = t(),
                            "[T+{}] Fix-C: FPGA chain-common CTRL+BAUD pre-programmed before cold enum (confound clear; serial-dispatch never writes it pre-enum)",
                            t()
                        );
                    }
                    Err(e) => warn!(
                        error = %e,
                        "Fix-C: FPGA chain-common pre-init open failed (diagnostic only; proceeding to enum)"
                    ),
                }
            }

            info!(
                serial_device = %chain_uart_device,
                t_ms = t(),
                "[T+{}] BM1362 chain reset chain={} (Phase 4-7: reset / enumerate / init / baud selection)",
                t(),
                fpga_chain_id
            );
            let pll_ramp = self.config.mining.am2_pll_ramp;
            // #15b (HB_RESET dig, 2026-05-29): DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL=1
            // wraps the BM1362 init in a bosminer-faithful reset->enum retry. On
            // init failure (the standalone enum=0 case bails as Err), re-pulse
            // HB_RESET (LOW->HIGH all slots via devmem or the sysfs repoint path
            // - the chip RESET line; the dsPIC DC-DC rail stays up, so this
            // re-wakes wedged chips without dropping voltage) and retry, up to
            // the bounded env-tunable attempt count, BEFORE the existing
            // teardown+bail. bosminer (bosminer-strace-init-full.log:25408/25619)
            // does exactly this repeating reset->GetAddress loop; DCENT_OS fired
            // only a single 20 ms pulse with no retry. STANDALONE-ONLY + default-OFF:
            // gated off on the bosminer-handoff path (TRUST_RAIL_FALLBACK), where
            // re-resetting would disturb bosminer's engaged chips; when the env is
            // unset the loop runs EXACTLY once and is byte-identical to before.
            // Gated on the explicit opt-in env ALONE. CORRECTION (live-verified
            // 2026-05-30 against run_wave48_25.sh): the `a lab unit` STANDALONE recipe
            // ALSO sets DCENT_AM2_TRUST_RAIL_FALLBACK=1 — it is NOT the
            // standalone-vs-handoff discriminator (BOTH recipes set it; the real
            // discriminator is DCENT_AM2_PIC_RESET_AND_START_APP, which the handoff
            // path forbids). An earlier `&& !TRUST_RAIL_FALLBACK` guard therefore
            // wrongly DISABLED the retry on the very standalone path it exists for.
            // The retry only ever fires on an init-Err, AND the proven
            // handoff recipe never sets DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL, so the
            // explicit opt-in env is the correct + sufficient control (no handoff
            // chip-disturbance risk).
            let hb_faithful = am2_env_flag("DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL");
            // Fix-B (2026-06-07): bosminer re-pulses HB_RESET many times (~dozens,
            // ~12 s cadence) until the cold chain answers; 3 attempts was too few.
            // Bump the bosminer-faithful retry budget (env-tunable). Each retry
            // re-pulses the sysfs HB_RESET (REPOINT path) + re-runs the full RE-018
            // replay + PLL settle. Non-faithful path stays single-shot.
            let requested_hb_attempts =
                am2_env_u64("DCENT_AM2_HB_RESET_MAX_ATTEMPTS", 6).clamp(1, 20) as u8;
            let hb_max_attempts = am2_hb_reset_attempt_budget(
                hb_faithful,
                requested_hb_attempts,
                open_core_rail_plan.requires_demotion(),
            );
            if open_core_rail_plan.requires_demotion() && hb_faithful {
                warn!(
                    requested_attempts = requested_hb_attempts,
                    admitted_attempts = hb_max_attempts,
                    "AM2 open-core rail plan restricts ASIC init to one attempt because no evidence-backed elevated-rail dwell/retry budget exists"
                );
            }
            let mut hb_attempt: u8 = 0;
            let (mut s, init_unique_count) = loop {
                hb_attempt += 1;
                let init_result = if self.shutdown.is_cancelled() {
                    Err(anyhow::anyhow!(
                        "shutdown requested before BM1362 init attempt {hb_attempt}"
                    ))
                } else {
                    Self::init_asic_chain(&chain_uart_device, chip_count, target_freq, pll_ramp)
                };
                match init_result {
                    Ok((serial, unique_count)) => break (serial, unique_count),
                    Err(e)
                        if hb_faithful
                            && !self.shutdown.is_cancelled()
                            && hb_attempt < hb_max_attempts =>
                    {
                        warn!(
                            attempt = hb_attempt,
                            max_attempts = hb_max_attempts,
                            error = %e,
                            "DCENT_AM2_HB_RESET_BOSMINER_FAITHFUL: BM1362 init failed — re-pulsing HB_RESET (bosminer-faithful reset->enum retry) then retrying"
                        );
                        // Re-pulse HB_RESET (rail stays up — HB_RESET is the chip reset
                        // line, not the DC-DC). Bounded hold; best-effort.
                        // R5 (2026-05-31): `hold_resets_devmem` is the single reset
                        // dispatcher — when DCENT_AM2_HB_RESET_REPOINT=1 it internally
                        // re-points to the bosminer-faithful plain-kernel-sysfs
                        // LOW->hold->HIGH pulse (the cold-capture-proven mechanism)
                        // instead of the devmem RMW. So this faithful re-pulse picks up
                        // the sysfs drive automatically when the repoint gate is set;
                        // unset = the existing devmem RMW path, byte-identical.
                        // CE-003: discover the board-control UIO by name instead of hardcoding 17.
                        match BoardControl::open(board_control_uio_number()) {
                            Ok(bc) => {
                                let hold = self.config.mining.am2_reset_hold_ms.clamp(1800, 10_000);
                                let reset_slots: &[u8] = if am2_xil_25_fingerprint_matches() {
                                    &[0, 2]
                                } else {
                                    &[0, 1, 2, 3]
                                };
                                if let Err(re) = bc.hold_resets_devmem(reset_slots, hold) {
                                    force_pwr_control_low(
                                        self.config.psu.pwr_control_gpio.as_deref(),
                                        "hb-reset-repulse-failed",
                                    );
                                    return Err(anyhow::anyhow!(
                                        "HB_RESET faithful re-pulse failed before retry: {re}"
                                    ));
                                }
                            }
                            Err(oe) => {
                                force_pwr_control_low(
                                    self.config.psu.pwr_control_gpio.as_deref(),
                                    "hb-reset-board-control-open-failed",
                                );
                                return Err(anyhow::anyhow!(
                                    "HB_RESET faithful re-pulse: BoardControl::open failed: {oe}"
                                ));
                            }
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            "Phase 4-7 failed after power-up; tearing down PIC/PSU watchdog state before returning"
                        );
                        if am2_diag_stop_after_bm1362_enum_enabled() {
                            warn!(
                                env_gate = "DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM=1",
                                selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                                selected_pic_serial_device = %serial_device,
                                effective_chain_uart_device = %chain_uart_device,
                                effective_chain_dspic_addr = ?effective_chain_dspic_addr
                                    .map(|addr| format!("0x{:02X}", addr)),
                                active_chains = format_args!("0b{:03b}", active_chains),
                                active_dspic_addrs = format_args!("{:02X?}", active_dspic_addrs(active_chains)),
                                "AM2 diagnostic stop requested before mining, but BM1362 init failed before successful GetAddress enumeration - no Stratum, no work dispatch, no shares"
                            );
                        }
                        if am2_env_flag("DCENT_AM2_UART_FALLBACK_SCAN") {
                            match Self::probe_uart_for_chips(&chain_uart_device) {
                                Some((path, count)) => info!(
                                    fallback_path = %path,
                                    unique_chip_replies = count,
                                    "UART fallback scan after BM1362 init failure found chip traffic"
                                ),
                                None => warn!(
                                "UART fallback scan after BM1362 init failure found no chip traffic"
                            ),
                            }
                        }
                        if let Some(service) = i2c0_service.as_ref() {
                            let mut disable_addrs = if all_active_voltage_enable
                                || am2_diag_stop_after_bm1362_enum_enabled()
                            {
                                active_dspic_addrs(active_chains)
                            } else {
                                vec![selected_pic_addr]
                            };
                            if !disable_addrs.contains(&selected_pic_addr) {
                                disable_addrs.push(selected_pic_addr);
                            }
                            disable_addrs.sort_unstable();
                            disable_addrs.dedup();
                            disable_dspic_addrs_best_effort(
                                service,
                                &disable_addrs,
                                selected_pic_addr,
                                heartbeat_pic_fw,
                                "bm1362-init-failed",
                            );
                        }
                        force_am2_home_hard_stop(&self.config, "bm1362-init-failed");
                        shutdown_am2_psu_after_feeders_bounded(
                            &self.config,
                            &mut runtime_threads,
                            &psu_arc,
                            "bm1362-init-failed",
                        )
                        .await;

                        // PWR_CONTROL gate auto-deasserts when the surviving
                        // `psu_arc` reference is dropped on return — owned by
                        // `Apw121215a::Drop`.

                        return Err(e).context("Phase 4-7 ASIC init failed");
                    }
                }
            };
            s.set_response_len(BM1362_RESP_BODY_LEN);
            // Phase 5/7 closed: chain enumeration + baud selection complete.
            let asic_baud = s.baud();
            info!(
                t_ms = t(),
                serial_device = %chain_uart_device,
                expected_chip_count = chip_count,
                unique_chip_replies = init_unique_count,
                asic_baud,
                "[T+{}] BM1362 GetAddress @{}baud chain={} unique_chip_replies={} expected={} (Phase 4-7 done)",
                t(),
                asic_baud,
                fpga_chain_id,
                init_unique_count,
                chip_count
            );
            if am2_diag_stop_after_bm1362_enum_enabled() {
                warn!(
                    t_ms = t(),
                    env_gate = "DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM=1",
                    selected_pic_addr = format_args!("0x{:02X}", selected_pic_addr),
                    selected_pic_serial_device = %serial_device,
                    effective_chain_uart_device = %chain_uart_device,
                    active_chains = format_args!("0b{:03b}", active_chains),
                    active_dspic_addrs = format_args!("{:02X?}", active_dspic_addrs(active_chains)),
                    unique_chip_replies = init_unique_count,
                    expected_chip_count = chip_count,
                    asic_baud,
                    "AM2 diagnostic stop after BM1362 GetAddress enumeration - no Stratum, no work dispatch, no shares"
                );

                if let Some(service) = i2c0_service.as_ref() {
                    let mut disable_addrs = active_dspic_addrs(active_chains);
                    if !disable_addrs.contains(&selected_pic_addr) {
                        disable_addrs.push(selected_pic_addr);
                    }
                    disable_addrs.sort_unstable();
                    disable_addrs.dedup();

                    for addr in disable_addrs {
                        let fw_hint = if addr == selected_pic_addr {
                            heartbeat_pic_fw
                        } else {
                            None
                        };
                        let fw_hint_log = fw_hint
                            .map(|fw| format!("0x{:02X}", fw))
                            .unwrap_or_else(|| "auto".to_string());
                        let mut shutdown_pic =
                            Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint);
                        if let Err(hb_err) = shutdown_pic.send_heartbeat() {
                            warn!(
                                error = %hb_err,
                                addr = format_args!("0x{:02X}", addr),
                                "PIC heartbeat before BM1362-enum diagnostic disable failed (continuing)"
                            );
                        }
                        match shutdown_pic.disable_voltage() {
                            Ok(()) => info!(
                                addr = format_args!("0x{:02X}", addr),
                                fw_hint = %fw_hint_log,
                                "PIC voltage disabled for BM1362-enum diagnostic stop"
                            ),
                            Err(disable_err) => warn!(
                                error = %disable_err,
                                addr = format_args!("0x{:02X}", addr),
                                fw_hint = %fw_hint_log,
                                "PIC voltage disable failed during BM1362-enum diagnostic stop; forcing PWR_CONTROL low next"
                            ),
                        }
                    }
                } else {
                    warn!(
                        "AM2 I2C service missing during BM1362-enum diagnostic stop; forcing PWR_CONTROL low next"
                    );
                }

                force_am2_home_hard_stop(&self.config, "diag-stop-after-bm1362-enum");
                shutdown_am2_psu_after_feeders_bounded(
                    &self.config,
                    &mut runtime_threads,
                    &psu_arc,
                    "diag-stop-after-bm1362-enum",
                )
                .await;

                anyhow::bail!(
                    "DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM=1: diagnostic stop after \
                     BM1362 GetAddress enumeration (no Stratum, no work dispatch, no shares)"
                );
            }

            // 2026-05-31 — AM2 `a lab unit` open-core ramp-DOWN (Variant A second
            // half). The chip string enumerated AT the elevated open-core
            // voltage (set at the Phase-3 `cold_boot_init` call site above).
            // Now that enum SUCCEEDED (enum > 0) and BEFORE work dispatch, ramp
            // the chip rail back DOWN to the steady operating voltage — exactly
            // what the AM3-BB `a lab unit` reference does (`am3_bb_mining.rs`
            // open-core-mv -> hold -> steady-mv) on the same BM1362.
            //
            // Guard: only fire when the open-core gate is active AND enum > 0.
            // If enum == 0 we never reach here: Phase 4-7 takes the full
            // safe-off path. Elevated mode admits only one init attempt because
            // no evidence-backed dwell/retry budget exists. Default-OFF /
            // non-`a lab unit` ⇒ this block is a no-op.
            if open_core_rail_plan.requires_demotion() {
                if init_unique_count == 0 {
                    pic_i2c.latch_terminal_safe_off();
                    if let Err(disable_err) = pic.disable_voltage() {
                        warn!(
                            error = %disable_err,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "Elevated open-core init returned zero chips; selected dsPIC safe-off failed before PWR_CONTROL hard-stop"
                        );
                    }
                    force_am2_home_hard_stop(&self.config, "open-core-elevated-zero-enumeration");
                    self.shutdown.cancel();
                    let _ = stop_am2_runtime_feeders_bounded(
                        &self.config,
                        &mut runtime_threads,
                        "open-core-elevated-zero-enumeration",
                    )
                    .await;
                    anyhow::bail!(
                        "elevated AM2 open-core initialization returned zero chips; terminal safe-off requested and work dispatch refused"
                    );
                }
                let steady_mv = open_core_rail_plan.steady_mv;
                if self.shutdown.is_cancelled() {
                    pic_i2c.latch_terminal_safe_off();
                    if let Err(disable_err) = pic.disable_voltage() {
                        warn!(
                            error = %disable_err,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "AM2 open-core demotion was cancelled; selected dsPIC safe-off also failed before PWR_CONTROL hard-stop"
                        );
                    }
                    force_am2_home_hard_stop(&self.config, "open-core-demotion-cancelled");
                    let _ = stop_am2_runtime_feeders_bounded(
                        &self.config,
                        &mut runtime_threads,
                        "open-core-demotion-cancelled",
                    )
                    .await;
                    anyhow::bail!(
                        "AM2 open-core demotion cancelled before the elevated rail could be returned to steady; terminal safe-off asserted"
                    );
                }
                info!(
                    t_ms = t(),
                    open_core_mv = open_core_rail_plan.energization_mv,
                    steady_mv,
                    unique_chip_replies = init_unique_count,
                    "[T+{}] AM2 .25 open-core ramp-DOWN: enum succeeded ({} chips) — ramping chip \
                     rail from open-core back to steady {} mV before work dispatch (matches AM3-BB .79)",
                    t(),
                    init_unique_count,
                    steady_mv
                );
                // Sanctioned cold-boot init transition: enumeration has just
                // proven the elevated open-core rail usable, and work remains
                // fenced until this one-shot demotion reaches steady voltage.
                if let Err(e) = pic.set_voltage(steady_mv) {
                    error!(
                        t_ms = t(),
                        steady_mv,
                        open_core_mv = open_core_rail_plan.energization_mv,
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        error = %e,
                        "[T+{}] AM2 .25 open-core ramp-DOWN failed; refusing work dispatch and entering terminal safe-off",
                        t()
                    );

                    // Fence every queued/non-SafeOff I2C mutation before the
                    // compensation command. The HAL request has a finite
                    // admission/start/execution deadline; there is no retry
                    // whose late completion could re-energize the rail.
                    pic_i2c.latch_terminal_safe_off();
                    if let Err(disable_err) = pic.disable_voltage() {
                        warn!(
                            error = %disable_err,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "Selected dsPIC voltage-disable failed after demotion failure; PWR_CONTROL hard-stop remains authoritative"
                        );
                    }
                    force_am2_home_hard_stop(&self.config, "open-core-demotion-failed");
                    self.shutdown.cancel();
                    let _ = stop_am2_runtime_feeders_bounded(
                        &self.config,
                        &mut runtime_threads,
                        "open-core-demotion-failed",
                    )
                    .await;
                    return Err(e).context(
                        "AM2 open-core rail demotion failed; terminal safe-off asserted and work dispatch refused",
                    );
                }
                info!(
                    t_ms = t(),
                    steady_mv,
                    addr = format_args!("0x{:02X}", selected_pic_addr),
                    "[T+{}] AM2 .25 open-core ramp-DOWN complete: chip rail set to steady {} mV",
                    t(),
                    steady_mv
                );
                if self.shutdown.is_cancelled() {
                    pic_i2c.latch_terminal_safe_off();
                    if let Err(disable_err) = pic.disable_voltage() {
                        warn!(
                            error = %disable_err,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "Shutdown arrived during open-core demotion; selected dsPIC safe-off failed before PWR_CONTROL hard-stop"
                        );
                    }
                    force_am2_home_hard_stop(
                        &self.config,
                        "open-core-demotion-post-command-cancelled",
                    );
                    let _ = stop_am2_runtime_feeders_bounded(
                        &self.config,
                        &mut runtime_threads,
                        "open-core-demotion-post-command-cancelled",
                    )
                    .await;
                    anyhow::bail!(
                        "shutdown arrived while returning the open-core rail to steady; terminal safe-off asserted before work dispatch"
                    );
                }
            }

            if let Some(gate_pwm) = fan_gate_pwm {
                let configured_max = self.config.thermal.fan_max_pwm;
                match open_am2_fan_controller("phase-2c-steady-cap") {
                    Some((discovery, fan)) => {
                        let steady_pwm = configured_max
                            .min(dcentrald_hal::fan::PWM_MAX)
                            .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                        fan.set_speed(steady_pwm);
                        let max_rpm = fan
                            .get_per_fan_rpm()
                            .iter()
                            .map(|(_, rpm)| *rpm)
                            .max()
                            .unwrap_or(0);
                        info!(
                            t_ms = t(),
                            uio = discovery.uio_number,
                            fan_gate_pwm = gate_pwm,
                            target_max_pwm = steady_pwm,
                            max_rpm,
                            "[T+{}] Phase 2c-pre fan gate complete; fan PWM command remains capped at configured cap {} and PID may resume",
                            t(),
                            steady_pwm
                        );
                    }
                    None => warn!(
                        t_ms = t(),
                        fan_gate_pwm = gate_pwm,
                        target_max_pwm = configured_max,
                        "[T+{}] Phase 2c-pre fan gate complete; could not re-open fan controller to re-assert configured cap",
                        t()
                    ),
                }
            }
            post_init_serial = Some(s);
        }

        if passthrough {
            if let Some(serial) = post_init_serial.as_ref() {
                let startup_drain = read_bm1362_serial_drain_summary(serial, 50);
                info!(summary = %startup_drain, "startup_passthrough_serial_drain");

                let startup_probe = probe_bm1362_get_address_summary(serial, 250);
                info!(summary = %startup_probe, "startup_passthrough_serial_probe");
            }
        }

        // ================================================================
        // Phase 8: FPGA chain open_am2 + CTRL + BAUD
        // Phase 9: diagnostic-only Braiins glitch monitor mirror, Braiins-am2 only
        // ================================================================
        let fpga = match fpga_chain_id {
            // W2 enum-fix follow-on (2026-06-14): on the `a lab unit` kernel-UART transport
            // the chain1-work-tx UIO (uio3) is UNBOUND to free IRQ 165 for kernel
            // of_serial, so `FpgaChain::open_am2` (which maps uio0..uio3) ENODEVs ->
            // "Failed to open am2 chain1 via UIO" -> teardown AFTER a successful
            // 126/126 enum. In SERIAL_WORK_DISPATCH the FPGA work-tx FIFO is bypassed
            // (work goes over ttyS1), so the FPGA object is only needed for the
            // chain-common CTRL/BAUD, which the `/dev/mem` path provides without the
            // unbound UIO. Guard with `!am2_prefer_kernel_uart()` (the SAME guard the
            // Fix-C pre-init UIO open already uses) so the kernel-UART path falls to
            // the devmem branch below. Fleet/.109 (DevmemUart, UIO stays bound) never
            // set DCENT_PREFER_KERNEL_UART -> byte-identical UIO path, no regression.
            1 if std::path::Path::new("/dev/uio0").exists() && !am2_prefer_kernel_uart() => {
                info!(
                    "Phase 8: Opening FPGA chain {} via UIO base {} (am2 layout)",
                    fpga_chain_id, 0
                );
                Am2FpgaChain::Uio(
                    FpgaChain::open_am2(fpga_chain_id, 0)
                        .context("Failed to open am2 chain1 via UIO")?,
                )
            }
            4 if std::path::Path::new("/dev/uio12").exists() && !am2_prefer_kernel_uart() => {
                info!(
                    "Phase 8: Opening FPGA chain {} via UIO base {} (am2 layout)",
                    fpga_chain_id, 12
                );
                Am2FpgaChain::Uio(
                    FpgaChain::open_am2(fpga_chain_id, 12)
                        .context("Failed to open am2 chain4 via UIO")?,
                )
            }
            _ => {
                info!(
                    "Phase 8: Opening FPGA chain {} at 0x{:08X} via /dev/mem (am2 layout)",
                    fpga_chain_id, fpga_base
                );
                Am2FpgaChain::Devmem(
                    DevmemFpgaChain::open_am2(fpga_chain_id, fpga_base)
                        .context("Failed to open FPGA chain via /dev/mem (open_am2)")?,
                )
            }
        };

        let version = fpga.read_version();
        let ctrl = fpga.read_ctrl();
        let baud = fpga.read_baud();
        let work_time = fpga.read_work_time();
        info!(
            version = format_args!("0x{:08X}", version),
            ctrl = format_args!("0x{:08X}", ctrl),
            baud = format_args!("0x{:02X}", baud),
            work_time = format_args!("0x{:08X}", work_time),
            "FPGA chain {} state (am2)",
            fpga_chain_id,
        );

        let skip_fast_uart = am2_skip_fast_uart_enabled();
        let fpga_baud_divisor = if skip_fast_uart {
            fpga_chain::BAUD_REG_115200
        } else {
            fpga_chain::BAUD_REG_3M
        };
        let fpga_baud_label = if skip_fast_uart { "115200" } else { "3M" };

        if !passthrough {
            // In skip-FastUART mode the chain runs at 115200, so the FPGA
            // work-rx UART deserializer must NOT be in the post-baud-switch
            // EXT_BAUD posture (CTRL bit 23) or it mis-samples the returning
            // nonce frames. Clear bit 23 while skip-FastUART keeps the chain
            // slow; the full-FastUART path keeps the authoritative value.
            let ctrl_value = if skip_fast_uart {
                AM2_CTRL_BM1362 & !fpga_chain::ctrl_am2::EXT_BAUD_OR_CLKEN_23
            } else {
                AM2_CTRL_BM1362
            };
            info!(
                "Configuring FPGA: CTRL=0x{:08X} (am2 authoritative, skip_fast_uart={}), BAUD={}",
                ctrl_value, skip_fast_uart, fpga_baud_label
            );
            fpga.write_ctrl(ctrl_value);
            let ctrl_readback = fpga.read_ctrl();
            if ctrl_readback != ctrl_value {
                warn!(
                    wrote = format_args!("0x{:08X}", ctrl_value),
                    read = format_args!("0x{:08X}", ctrl_readback),
                    "CTRL readback mismatch"
                );
            } else {
                info!(
                    ctrl = format_args!("0x{:08X}", ctrl_readback),
                    "CTRL readback OK"
                );
            }
            fpga.set_baud(fpga_baud_divisor);
            info!(
                divisor = format_args!("0x{:02X}", fpga_baud_divisor),
                baud = fpga_baud_label,
                "FPGA BAUD divisor programmed for active ASIC baud"
            );
            // WORK_TIME is inline in the work-tx FIFO payload on am2; write is a
            // no-op inside the HAL but left here for audit / S9 symmetry.
            let wt = calculate_work_time_bm1362(target_freq);
            fpga.set_work_time(wt);
            info!(
                "FPGA WORK_TIME = 0x{:08X} (for {} MHz) — informational only on am2",
                wt, target_freq
            );
        } else {
            if ctrl == 0 {
                warn!(
                    "FPGA CTRL=0 (chain disabled) — writing am2 authoritative 0x{:08X}",
                    AM2_CTRL_BM1362
                );
                fpga.write_ctrl(AM2_CTRL_BM1362);
                let ctrl_readback = fpga.read_ctrl();
                if ctrl_readback != AM2_CTRL_BM1362 {
                    warn!(
                        wrote = format_args!("0x{:08X}", AM2_CTRL_BM1362),
                        read = format_args!("0x{:08X}", ctrl_readback),
                        "CTRL readback mismatch after passthrough enable"
                    );
                }
                if baud == 0 {
                    fpga.set_baud(fpga_baud_divisor);
                }
            } else {
                info!(
                    ctrl = format_args!("0x{:08X}", ctrl),
                    "Passthrough: preserving inherited CTRL as-is"
                );
            }
            info!(
                "FPGA state: CTRL=0x{:08X}, BAUD=0x{:02X}, WORK_TIME=0x{:08X}",
                fpga.read_ctrl(),
                fpga.read_baud(),
                fpga.read_work_time(),
            );
        }

        // Reset work FIFOs unless skip_fifo_reset is armed in passthrough mode.
        let skip_reset = self.config.mining.skip_fifo_reset && passthrough;
        if skip_reset {
            warn!(
                chain_id = fpga_chain_id,
                "DIAGNOSTIC: skip_fifo_reset armed — preserving WORK_TX/WORK_RX state (passthrough only)"
            );
        } else {
            fpga.reset_work_fifos();
            info!("FPGA work FIFOs reset");
            apply_am2_work_tx_bosminer_idle_mode(&fpga, "post_fifo_reset");
        }

        // Phase 9: diagnostic-only Braiins glitch monitor mirror, Braiins-am2 only.
        //
        // W13.B1 (2026-05-10) RECLASSIFIED: 0x43D00030/0x43D00034 are
        // read-only Braiins-am2 status mirrors of BM1362 ASIC reg 0x2C —
        // NOT a control surface. Phase 9A proved 7 devmem writes are
        // silent NO-OPs at the FPGA fabric. BM1362 0x2C/0x34 candidate
        // relay broadcasts are lab-gated by
        // `DCENT_BM1362_ENABLE_UART_RELAY_LAB`.
        //
        // This block remains as opt-in (default-off) telemetry parity
        // with bosminer's own glitch.rs. Gated by
        // `am2_force_braiins_glitch_mirror_write` (lab-only).
        match open_braiins_glitch_monitor_for_logging() {
            Some(monitor) => {
                log_am2_glitch_window("glitch_mirror_pre");

                if let Some(offset) = chain_glitch_status_offset(relay_phys_idx) {
                    let pre_read = monitor.read_word(offset).unwrap_or(0);

                    if !passthrough && self.config.mining.am2_force_braiins_glitch_mirror_write {
                        let target_value: u32 = BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT;
                        match monitor.force_braiins_glitch_status_mirror_write(relay_phys_idx) {
                            Ok(_report) => {
                                let post_read = monitor.read_word(offset).unwrap_or(0);
                                info!(
                                    chain_id = fpga_chain_id,
                                    phys_idx = relay_phys_idx,
                                    offset = format_args!("0x{:02X}", offset),
                                    wrote = format_args!("0x{:08X}", target_value),
                                    pre_read = format_args!("0x{:08X}", pre_read),
                                    post_read = format_args!("0x{:08X}", post_read),
                                    "Phase 9 Braiins glitch mirror force-write attempt (diagnostic-only)"
                                );
                                if post_read != target_value {
                                    warn!(
                                        chain_id = fpga_chain_id,
                                        phys_idx = relay_phys_idx,
                                        offset = format_args!("0x{:02X}", offset),
                                        wrote = format_args!("0x{:08X}", target_value),
                                        post_read = format_args!("0x{:08X}", post_read),
                                        "Phase 9 glitch mirror readback != write (expected — Phase 9A NO-OP)"
                                    );
                                }
                            }
                            Err(e) => {
                                warn!(
                                    chain_id = fpga_chain_id,
                                    phys_idx = relay_phys_idx,
                                    offset = format_args!("0x{:02X}", offset),
                                    error = %e,
                                    "Phase 9 Braiins glitch mirror write FAILED (diagnostic-only)"
                                );
                            }
                        }
                    } else {
                        let reason = if passthrough {
                            "passthrough mode"
                        } else {
                            "am2_force_braiins_glitch_mirror_write disabled"
                        };
                        info!(
                            chain_id = fpga_chain_id,
                            phys_idx = relay_phys_idx,
                            offset = format_args!("0x{:02X}", offset),
                            observed = format_args!("0x{:08X}", pre_read),
                            reason,
                            "Braiins glitch mirror read; no diagnostic write attempted"
                        );
                    }
                } else {
                    warn!(
                        phys_idx = relay_phys_idx,
                        "No known Braiins glitch-status offset for chain"
                    );
                }

                log_am2_glitch_window("glitch_mirror_post");
            }
            None => {
                info!("Braiins glitch monitor unavailable (stock hw / no Braiins-am2 IP); diagnostic-only telemetry skipped");
            }
        }

        // ================================================================
        // PIC heartbeat is started at Phase 3d, immediately after voltage
        // enable/settle and before ASIC or FPGA work can block init.
        // ================================================================
        if passthrough {
            info!(
                "Phase 9b: passthrough mode skips DCENT_OS PIC heartbeat; inherited firmware owns voltage watchdog"
            );
        } else if runtime_threads.contains("s19j-pic-heartbeat") {
            info!("Phase 9b: PIC heartbeat already running from Phase 3d");
        } else {
            anyhow::bail!("PIC heartbeat was not started before ASIC init");
        }

        // ---- Arm the hardware watchdog (AFTER chain bring-up completes) ----
        // `--s19j-hybrid` bypasses `Daemon::run()`, so historically this path armed
        // NO `/dev/watchdog` — a CPU/runtime hang here left the boards energized &
        // unsupervised. Arm it now (chains enumerated, FPGA open, PIC heartbeat
        // live) using the shared, config-gated helper — NOT earlier, so the
        // DTB-10s window can never trip during the slow cold-boot. Gated on
        // `watchdog.enabled`: the `a lab unit`/XIL bring-up configs set it `false`, so
        // this is INERT on `a lab unit` and the proven recipe stays byte-unchanged.
        // SAF-5: gate kicks on this path's runtime thermal/housekeeping loop so a
        // live-locked hybrid miner stops feeding `/dev/watchdog` after the
        // counter has started advancing.
        let watchdog_liveness = Arc::new(AtomicU64::new(0));
        crate::daemon::spawn_watchdog_kicker(
            &self.config.watchdog,
            self.shutdown.clone(),
            Some(watchdog_liveness.clone()),
        );

        // ================================================================
        // Phase 10: Pool connect + mining loop
        // ================================================================
        info!("Phase 10a: Connecting to pool");
        let (job_tx, mut job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
        let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
        let (status_tx, mut status_rx) =
            mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);
        let serial_work_dispatch = am2_serial_work_dispatch_enabled();

        let stratum_config = crate::config::build_stratum_config(
            &self.config,
            crate::config::stratum_donation_config(&self.config.donation),
            serial_work_dispatch && self.config.mining.version_rolling,
            false,
        );
        let stratum_router = dcentrald_stratum::StratumRouter::new(stratum_config);
        tokio::spawn(async move {
            stratum_router.run(job_tx, share_rx, status_tx).await;
        });

        // AT-DASH (2026-06-14): shared pool-share accounting. The status-logger
        // task below is the SOLE writer (accepted/rejected counts + achieved
        // difficulty sum); the mining loop reads it on each hashrate tick to
        // compute the real GH/s estimate and publish a live MinerState. Atomic +
        // fail-closed: a publish/counter error can never block or slow mining.
        let share_accounting = Arc::new(Am2ShareAccounting::default());
        let pool_quality = Arc::new(RwLock::new(
            dcentrald_stratum::pool_quality::PoolQualitySnapshot::default(),
        ));

        // Status logger
        let ss = self.shutdown.clone();
        let status_accounting = share_accounting.clone();
        let status_pool_quality = pool_quality.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = ss.cancelled() => break,
                    Some(st) = status_rx.recv() => {
                        match status_pool_quality.write() {
                            Ok(mut quality) => {
                                dcentrald_stratum::pool_quality::apply_stratum_status(
                                    &mut quality,
                                    &st,
                                );
                            }
                            Err(_) => {
                                warn!("Pool-quality snapshot lock poisoned; leaving last truthful snapshot in place");
                            }
                        }
                        match st {
                        dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, pool_target_difficulty, achieved_difficulty, .. } => {
                            status_accounting.record_accepted(
                                achieved_difficulty.unwrap_or(0.0),
                                pool_target_difficulty,
                            );
                            info!(job_id = %job_id, pool_target_difficulty, achieved_difficulty, "SHARE ACCEPTED");
                        }
                        dcentrald_stratum::types::StratumStatus::ShareRejected { job_id, error_msg, .. } => {
                            status_accounting.record_rejected();
                            warn!(job_id = %job_id, error = %error_msg, "SHARE REJECTED");
                        }
                        dcentrald_stratum::types::StratumStatus::DifficultyChanged(d) =>
                            info!("Pool difficulty: {}", d),
                        dcentrald_stratum::types::StratumStatus::StateChanged(state) =>
                            info!("Pool: {:?}", state),
                        _ => {}
                        }
                    }
                }
            }
        });

        // ---- AM2 thermal supervision: pre-stratum hard proof ----
        //
        // `--s19j-hybrid` bypasses `Daemon::run()`, so the am2 hybrid mining
        // loop previously had no thermal supervision at all. Build the
        // supervisor and require a valid temperature reading before
        // dispatching any work; fail closed (cut hash power, fans stay capped)
        // if no source produces a reading.
        let mut thermal_supervisor: Option<Am2ThermalSupervisor> = if am2_env_flag(
            ENV_AM2_SKIP_THERMAL_SUPERVISOR,
        ) {
            warn!(
                    env = ENV_AM2_SKIP_THERMAL_SUPERVISOR,
                    "AM2 thermal supervisor disabled by lab override — never set this for an unattended home soak"
                );
            None
        } else {
            // R-13: die-temp calibration is DEFAULT-OFF. It activates only when
            // the `[thermal.die_temp_calibration].enabled` config flag OR the
            // `DCENT_AM2_DIE_TEMP_CALIBRATION` env override is set. Even then it
            // is fail-safe (bad/missing/not-cold baseline → raw; safety reading
            // never below raw), so this can never delay or suppress a trip.
            let mut die_cal_cfg = self.config.thermal.die_temp_calibration.clone();
            if am2_env_flag(ENV_AM2_DIE_TEMP_CALIBRATION) {
                die_cal_cfg.enabled = true;
                info!(
                    env = ENV_AM2_DIE_TEMP_CALIBRATION,
                    "R-13 die-temp calibration opted in via env override (fail-safe: never reports below raw)"
                );
            }
            let thermal_pic = match pic0x89_clean_stop_owner_policy(
                selected_pic_endpoint_required,
                selected_pic_endpoint_session.is_some(),
            ) {
                Pic0x89CleanStopOwnerPolicy::Endpoint => Some(
                    selected_pic_endpoint_session
                        .as_ref()
                        .expect("endpoint policy requires retained thermal session")
                        .controller(),
                ),
                Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => {
                    error!(
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        "exact AM2 endpoint session missing before thermal ownership handoff; refusing raw address/firmware reconstruction"
                    );
                    force_am2_home_hard_stop(
                        &self.config,
                        "thermal-supervisor-missing-exact-pic-endpoint",
                    );
                    anyhow::bail!(
                        "exact AM2 endpoint session missing before thermal ownership handoff"
                    );
                }
                Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => {
                    match (i2c0_service.as_ref(), heartbeat_pic_fw) {
                        (Some(service), Some(fw)) => Some(Pic0x89Service::new_with_fw(
                            service.clone(),
                            selected_pic_addr,
                            Some(fw),
                        )),
                        _ => None,
                    }
                }
            };
            let mut sup = Am2ThermalSupervisor::new(
                thermal_pic,
                self.config.thermal.hot_temp_c,
                self.config.thermal.dangerous_temp_c,
                die_cal_cfg,
            );
            // Capture the cold die-calibration baseline (no-op unless enabled)
            // at this pre-stratum stage, BEFORE the hard thermal proof, while
            // the unit is still cold and board ≈ die ≈ ambient.
            sup.maybe_capture_die_baseline();
            if let Err(e) = sup.poll_and_check("pre-stratum") {
                error!(error = %e, "AM2 pre-stratum thermal proof FAILED — forcing home hard-stop");
                force_am2_thermal_hard_stop(&self.config, "pre-stratum-thermal-proof-failed");
                return Err(e);
            }
            Some(sup)
        };
        let thermal_poll_ms = ((self.config.thermal.pid_interval_s.max(1.0) * 1000.0) as u64)
            .max(AM2_THERMAL_MIN_POLL_MS);

        // ---- Route: serial-work-dispatch alternative (DCENT_AM2_SERIAL_WORK_DISPATCH=1) ----
        if serial_work_dispatch {
            let serial = post_init_serial.take().ok_or_else(|| {
                anyhow::anyhow!(
                    "DCENT_AM2_SERIAL_WORK_DISPATCH=1 but the post-init serial backend is unavailable"
                )
            })?;

            // ---- GROUP B / W8: DUAL-CHAIN capability (DCENT_AM2_DUAL_CHAIN_TTYS3=1) ----
            //
            // DEFAULT-OFF. When set, bring up a SECOND chain on the configured
            // second UART (`/dev/ttyS3` on `a lab unit`, dsPIC 0x22) using the SAME
            // proven `init_asic_chain` bring-up as the primary chain, then run
            // the dual-chain dispatch loop so BOTH hashboards mine (~2× hashrate
            // vs chain-1-only). The exact 2nd-chain cold bring-up order/timing is
            // RE-ASK-XIL-6 (unconfirmed vs a live bosminer dual-chain strace), so
            // this is a CAPABILITY flagged for operator live-validation. If the
            // second chain fails to enumerate, FALL BACK to the proven
            // single-chain path (never regress the chain-1 milestone).
            if am2_dual_chain_ttys3_enabled() {
                let second_uart = am2_dual_chain_second_uart();
                let pll_ramp = self.config.mining.am2_pll_ramp;
                if second_uart == chain_uart_device {
                    warn!(
                        second_uart = %second_uart,
                        primary_chain_uart = %chain_uart_device,
                        "DCENT_AM2_DUAL_CHAIN_TTYS3=1 but the configured second UART equals the primary chain UART — refusing to bring up a duplicate chain; running single-chain"
                    );
                } else {
                    let pic_addr_b = am2_pic_addr_from_serial_device(&second_uart);
                    info!(
                        second_uart = %second_uart,
                        primary_chain_uart = %chain_uart_device,
                        pic_addr_b = ?pic_addr_b.map(|a| format!("0x{:02X}", a)),
                        "DCENT_AM2_DUAL_CHAIN_TTYS3=1 — attempting second BM1362 chain bring-up (RE-ASK-XIL-6: order/timing pending operator live-validation)"
                    );
                    match Self::init_asic_chain(&second_uart, chip_count, target_freq, pll_ramp) {
                        Ok((serial_b, unique_b)) => {
                            info!(
                                second_uart = %second_uart,
                                unique_chip_replies = unique_b,
                                "DUAL-CHAIN: second chain enumerated — routing work to BOTH chains"
                            );
                            // pic_addr_b is best-effort for teardown; on the `a lab unit`
                            // topology slot-2/ttyS3 → 0x22 (already enabled by the
                            // all-active-PICs Phase 1-3 path). Fall back to the
                            // primary's dsPIC addr if the path is unrecognized so
                            // teardown still disables a real controller.
                            let pic_addr_b = pic_addr_b.unwrap_or(selected_pic_addr);
                            let dispatch_result = self
                                .run_am2_dual_chain_serial_dispatch_loop(
                                    serial,
                                    serial_b,
                                    job_rx,
                                    share_tx,
                                    chip_count,
                                    i2c0_service.clone(),
                                    selected_pic_addr,
                                    pic_addr_b,
                                    heartbeat_pic_fw,
                                    thermal_supervisor,
                                    thermal_poll_ms,
                                    watchdog_liveness.clone(),
                                    share_accounting.clone(),
                                    pool_quality.clone(),
                                )
                                .await;
                            let feeders_quiesced = stop_am2_runtime_feeders_bounded(
                                &self.config,
                                &mut runtime_threads,
                                "dual-chain-dispatch-stop",
                            )
                            .await;
                            let shutdown_evidence = if feeders_quiesced {
                                let mut teardown_addrs = vec![selected_pic_addr, pic_addr_b];
                                teardown_addrs.sort_unstable();
                                teardown_addrs.dedup();
                                if let Some(service) = i2c0_service.as_ref() {
                                    disable_dspic_addrs_best_effort(
                                        service,
                                        &teardown_addrs,
                                        selected_pic_addr,
                                        heartbeat_pic_fw,
                                        "dual-chain-clean-stop",
                                    );
                                }
                                shutdown_am2_psu_after_feeders_quiesced(
                                    &psu_arc,
                                    "dual-chain-dispatch-stop",
                                )
                            } else {
                                Am2PowerShutdownEvidence::hard_stop_after_timeout(psu_arc.is_some())
                            };
                            return finalize_am2_dispatch_shutdown(
                                dispatch_result,
                                shutdown_evidence,
                            );
                        }
                        Err(e) => {
                            warn!(
                                second_uart = %second_uart,
                                error = %e,
                                "DUAL-CHAIN: second chain bring-up FAILED — falling back to the proven single-chain path (no regression to the chain-1 milestone)"
                            );
                        }
                    }
                }
            } else if am2_xil_25_fingerprint_matches() {
                // D6-2 (2026-06-13): on the `a lab unit` 2-board topology (hashboards on
                // PL UART 0 + 2 = ttyS1 + ttyS3), with DCENT_AM2_DUAL_CHAIN_TTYS3
                // unset DCENT drives ONLY the primary chain. Record it loudly so an
                // enum=0 / low-hashrate result is NOT misread as a whole-unit
                // failure — the second board on /dev/ttyS3 is intentionally idle.
                // Fires once per run() (this is dispatch setup, before the loop).
                warn!(
                    primary_chain_uart = %chain_uart_device,
                    idle_second_uart = AM2_DUAL_CHAIN_SECOND_UART_DEFAULT,
                    "DUAL-CHAIN GATE OFF on a .25-fingerprint unit: only the primary chain is driven; /dev/ttyS3 (slot-2 board) is IDLE. An enum=0 here is a SINGLE-chain result, NOT whole-unit. Set DCENT_AM2_DUAL_CHAIN_TTYS3=1 for a 2-board run."
                );
            }

            info!(
                "DCENT_AM2_SERIAL_WORK_DISPATCH=1 — routing work via the proven BM1362 88-byte serial frame over the chain UART (FPGA WORK_TX FIFO bypassed)"
            );
            let mut dispatch_result = self
                .run_am2_serial_dispatch_loop(
                    serial,
                    job_rx,
                    share_tx,
                    chip_count,
                    i2c0_service.clone(),
                    selected_pic_addr,
                    heartbeat_pic_fw,
                    thermal_supervisor,
                    thermal_poll_ms,
                    watchdog_liveness.clone(),
                    share_accounting.clone(),
                    pool_quality.clone(),
                )
                .await;
            let feeders_quiesced = stop_am2_runtime_feeders_bounded(
                &self.config,
                &mut runtime_threads,
                "single-chain-dispatch-stop",
            )
            .await;
            let shutdown_evidence = if feeders_quiesced {
                // Phase 3A (CE-010): preserve the rail walk/reset/decay plan,
                // but only after PIC and PSU feeders are both joined.
                if am2_safe_teardown_enabled() {
                    if let Err(error) = am2_safe_teardown_sequence(
                        selected_pic_endpoint_session.as_mut(),
                        selected_pic_endpoint_required,
                        i2c0_service.as_ref(),
                        selected_pic_addr,
                        heartbeat_pic_fw,
                        "serial-dispatch-clean-stop",
                    ) {
                        error!(
                            error = %error,
                            "AM2 safe teardown lost exact endpoint authority; forcing hard stop"
                        );
                        force_am2_home_hard_stop(
                            &self.config,
                            "safe-teardown-missing-exact-pic-endpoint",
                        );
                        dispatch_result = Err(error);
                    }
                } else {
                    match pic0x89_clean_stop_owner_policy(
                        selected_pic_endpoint_required,
                        selected_pic_endpoint_session.is_some(),
                    ) {
                        Pic0x89CleanStopOwnerPolicy::Endpoint => {
                            let pic = selected_pic_endpoint_session
                                .as_mut()
                                .expect("endpoint policy requires retained session")
                                .controller_mut();
                            if let Err(e) = pic.disable_voltage() {
                                warn!(
                                    error = %e,
                                    addr = format_args!("0x{:02X}", selected_pic_addr),
                                    "AM2 serial-dispatch PIC voltage disable failed after feeders quiesced"
                                );
                            }
                        }
                        Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => {
                            error!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                "exact AM2 endpoint session missing at serial-dispatch clean stop; refusing raw address/firmware reconstruction"
                            );
                            force_am2_home_hard_stop(
                                &self.config,
                                "serial-dispatch-missing-exact-pic-endpoint",
                            );
                            dispatch_result = Err(anyhow::anyhow!(
                                "exact AM2 endpoint session missing at serial-dispatch clean stop"
                            ));
                        }
                        Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => {
                            if let (Some(service), Some(fw)) =
                                (i2c0_service.as_ref(), heartbeat_pic_fw)
                            {
                                let mut pic = Pic0x89Service::new_with_fw(
                                    service.clone(),
                                    selected_pic_addr,
                                    Some(fw),
                                );
                                if let Err(e) = pic.disable_voltage() {
                                    warn!(
                                        error = %e,
                                        addr = format_args!("0x{:02X}", selected_pic_addr),
                                        "AM2 legacy-compatible serial-dispatch PIC voltage disable failed after feeders quiesced"
                                    );
                                }
                            }
                        }
                    }
                }
                shutdown_am2_psu_after_feeders_quiesced(&psu_arc, "single-chain-dispatch-stop")
            } else {
                Am2PowerShutdownEvidence::hard_stop_after_timeout(psu_arc.is_some())
            };
            return finalize_am2_dispatch_shutdown(dispatch_result, shutdown_evidence);
        }

        // ---- Phase 10b: Mining loop (FPGA work dispatch + nonce collection) ----
        info!(
            "=== MINING ACTIVE — {} BM1362 chips, FPGA chain {} at 0x{:08X} (phys idx {}) ===",
            chip_count, fpga_chain_id, fpga_base, relay_phys_idx,
        );

        log_bm1362_voltage_topology(fpga_chain_id, chip_count);

        let mut work_builder = dcentrald_stratum::share_pipeline::WorkBuilder::new();
        let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
        let mut asic_job_id: u8 = 0;
        let mut work_history: Vec<VecDeque<WorkEntry>> = (0..128)
            .map(|_| VecDeque::with_capacity(WORK_HISTORY_PER_ID))
            .collect();

        let mut total_work: u64 = 0;
        let mut total_nonces: u64 = 0;
        let mut shares_submitted: u64 = 0;
        // Dedup the FPGA RX FIFO: the same nonce surfaces multiple times (both
        // midstate slots carry identical work, and the FIFO can re-present a
        // nonce), so without this every duplicate is re-submitted → pool reject
        // code 22. Mirrors the serial-dispatch loop + work_dispatcher.rs. Keyed
        // (work_id, nonce): the two identical-midstate slots reconstruct the same
        // header, so collapsing them on (work_id, nonce) is correct here
        // (am2 configs are version_rolling=false; add solution_id if ever enabled).
        let mut seen_shares: std::collections::BTreeSet<(u8, u32)> =
            std::collections::BTreeSet::new();
        let mut unsupported_share_submit_logged = false;
        let mut unsupported_job_logged = false;
        let start_time = Instant::now();
        let mut last_hr_time = Instant::now();
        let mut hr_nonces: u64 = 0;
        let mut first_dispatch_diag_done = false;
        let mut first_gate_bypass_done = false;
        let mut chain4_hidden_state_diag_done = false;
        let no_nonce_timeout = if self.config.mining.am2_no_nonce_timeout_s == 0 {
            None
        } else {
            Some(Duration::from_secs(
                self.config.mining.am2_no_nonce_timeout_s,
            ))
        };
        let mut first_work_at: Option<Instant> = None;
        let mut no_nonce_stall_reason: Option<String> = None;
        let chain4_timeline_enabled = self.config.mining.am2_first_work_timeline
            && passthrough
            && self.config.mining.skip_fifo_reset
            && fpga_chain_id == 4;
        if let Some(timeout) = no_nonce_timeout {
            info!(
                timeout_s = timeout.as_secs(),
                "AM2 no-nonce fail-closed guard armed"
            );
        } else {
            warn!("AM2 no-nonce fail-closed guard disabled by config");
        }
        if self.config.mining.am2_first_work_timeline && !chain4_timeline_enabled {
            warn!(
                fpga_chain_id,
                passthrough,
                skip_fifo_reset = self.config.mining.skip_fifo_reset,
                "AM2 first-work timeline requested but inactive; requires passthrough + skip_fifo_reset + chain4"
            );
        }

        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(10));
        let mut nonce_poll_timer = tokio::time::interval(Duration::from_millis(5));
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));
        let mut thermal_timer = tokio::time::interval(Duration::from_millis(
            thermal_poll_ms.max(AM2_THERMAL_MIN_POLL_MS),
        ));

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => { info!("Shutdown requested"); break; }

                _ = thermal_timer.tick() => {
                    watchdog_liveness.fetch_add(1, Ordering::Relaxed);
                    if let Some(sup) = thermal_supervisor.as_mut() {
                        if let Err(e) = sup.poll_and_check("runtime") {
                            error!(error = %e, "AM2 runtime thermal supervisor tripped — forcing home hard-stop");
                            force_am2_thermal_hard_stop(&self.config, "runtime-thermal-fail-closed");
                            self.shutdown.cancel();
                            break;
                        }
                    }
                }

                Some(job) = job_rx.recv() => {
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "NEW BLOCK — flushing FPGA work queues");
                        if !first_dispatch_diag_done {
                            log_am2_dispatch_snapshot(&fpga, "clean_jobs_pre_flush");
                            log_am2_glitch_window("clean_jobs_pre_flush");
                        }
                        work_history.iter_mut().for_each(VecDeque::clear);
                        seen_shares.clear(); // new block: prior nonces are now stale
                        work_builder.reset_extranonce2();
                        fpga.flush_work_tx();
                        apply_am2_work_tx_bosminer_idle_mode(&fpga, "clean_jobs_post_work_tx_flush");
                        fpga.flush_work_rx();
                        if !first_dispatch_diag_done {
                            log_am2_dispatch_snapshot(&fpga, "clean_jobs_post_flush");
                            log_am2_glitch_window("clean_jobs_post_flush");
                        }
                    }
                    if job.is_flush_only() {
                        info!(
                            job_id = %job.job_id,
                            "Pool switch flush complete; hybrid dispatch paused until the next pool notify"
                        );
                        current_job = None;
                        unsupported_job_logged = false;
                        continue;
                    }
                    if job.version_mask != 0 {
                        if !unsupported_job_logged {
                            unsupported_job_logged = true;
                            warn!(
                                version_mask = format_args!("0x{:08X}", job.version_mask),
                                "Hybrid BM1362 path cannot safely submit rolled-version shares on the FPGA nonce path — refusing version-rolling jobs"
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
                    if let Some(ref job) = current_job {
                        if !chain4_hidden_state_diag_done
                            && passthrough
                            && self.config.mining.skip_fifo_reset
                            && fpga_chain_id == 4
                            && total_work == 0
                        {
                            chain4_hidden_state_diag_done = true;
                            if let Some(serial) = post_init_serial.as_ref() {
                                info!("chain4_hidden_state_diag_begin");
                                let summary = probe_bm1362_get_address_summary(serial, 250);
                                info!(summary = %summary, "chain4_hidden_state_serial_probe");
                                info!("chain4_hidden_state_diag_end");
                            } else {
                                warn!("chain4_hidden_state_diag_serial_unavailable");
                            }
                        }

                        let tx_stat_now = fpga.read_work_tx_status();
                        let allow_first_job_despite_full = !first_gate_bypass_done
                            && passthrough
                            && self.config.mining.skip_fifo_reset
                            && total_work == 0
                            && ((tx_stat_now & fpga_chain::STAT_IRQ != 0)
                                || (fpga_chain_id == 4 && chain4_hidden_state_diag_done));

                        if fpga.work_tx_full() && !allow_first_job_despite_full {
                            if !first_dispatch_diag_done {
                                log_am2_dispatch_snapshot(&fpga, "first_dispatch_blocked_pre");
                                log_am2_glitch_window("first_dispatch_blocked_pre");

                                if passthrough && self.config.mining.skip_fifo_reset {
                                    fpga.write_work_tx_ctrl(0x0000_0000);
                                    std::thread::sleep(Duration::from_millis(1));
                                    log_am2_dispatch_snapshot(&fpga, "first_dispatch_txctrl_zero");

                                    fpga.write_work_tx_ctrl(fpga_chain::CMD_CTRL_IRQ_EN);
                                    std::thread::sleep(Duration::from_millis(1));
                                    log_am2_dispatch_snapshot(&fpga, "first_dispatch_txctrl_irqen");
                                }

                                if let Some(serial) = post_init_serial.as_ref() {
                                    let blocked_drain = read_bm1362_serial_drain_summary(serial, 50);
                                    info!(summary = %blocked_drain, "first_dispatch_blocked_serial_drain");
                                    let serial_probe = probe_bm1362_get_address_summary(serial, 250);
                                    info!(summary = %serial_probe, "first_dispatch_blocked_serial_probe");
                                } else {
                                    info!("first_dispatch_blocked_serial_probe: unavailable");
                                }

                                post_init_serial = None;
                                first_dispatch_diag_done = true;
                            }
                            continue;
                        }

                        if allow_first_job_despite_full {
                            first_gate_bypass_done = true;
                            warn!(
                                tx_stat = format_args!("0x{:08X}", tx_stat_now),
                                "DIAGNOSTIC: allowing one first work write despite TX_FULL because am2 hidden-state gate suggests a valid discriminator write"
                            );
                            if !first_dispatch_diag_done {
                                log_am2_dispatch_snapshot(&fpga, "first_dispatch_gate_bypass");
                            }
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

                        let version_bits_per_ms: Vec<Option<String>> = vec![None; work.midstates.len()];

                        let history = &mut work_history[asic_job_id as usize];
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

                        if !first_dispatch_diag_done {
                            log_am2_dispatch_snapshot(&fpga, "first_dispatch_pre");
                            log_am2_glitch_window("first_dispatch_pre");
                        }

                        fpga.write_work(&words);

                        if chain4_timeline_enabled && total_work > 0 && total_work < 3 {
                            let phase = format!("chain4_dispatch_timeline_job{}", total_work + 1);
                            run_am2_dispatch_timeline(&fpga, &phase);
                        }

                        if !first_dispatch_diag_done {
                            log_am2_dispatch_snapshot(&fpga, "first_dispatch_post");
                            log_am2_glitch_window("first_dispatch_post");

                            if chain4_timeline_enabled {
                                run_am2_dispatch_timeline(&fpga, "chain4_dispatch_timeline_job1");
                            } else {
                                std::thread::sleep(Duration::from_millis(300));

                                log_am2_dispatch_snapshot(&fpga, "first_dispatch_300ms");
                                log_am2_glitch_window("first_dispatch_300ms");
                            }

                            run_am2_rx_oracle_window(&fpga, Duration::from_secs(2));
                            log_am2_dispatch_snapshot(&fpga, "first_dispatch_rx_oracle_end");

                            if let Some(serial) = post_init_serial.as_ref() {
                                let dispatch_drain = read_bm1362_serial_drain_summary(serial, 50);
                                info!(summary = %dispatch_drain, "first_dispatch_serial_drain");
                                let serial_probe = probe_bm1362_get_address_summary(serial, 250);
                                info!(summary = %serial_probe, "first_dispatch_serial_probe");
                            } else {
                                info!("first_dispatch_serial_probe: unavailable");
                            }

                            post_init_serial = None;
                            first_dispatch_diag_done = true;
                        }

                        asic_job_id = (asic_job_id.wrapping_add(JOB_ID_INCREMENT)) & JOB_ID_MASK;
                        total_work += 1;
                        if first_work_at.is_none() {
                            first_work_at = Some(Instant::now());
                        }

                        if total_work <= 3 {
                            info!(
                                work_id = asic_job_id.wrapping_sub(JOB_ID_INCREMENT) & JOB_ID_MASK,
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

                        if total_nonces <= 1000 {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                work_id,
                                solution_id,
                                w1 = format_args!("0x{:08X}", w1),
                                "Nonce #{}", total_nonces,
                            );
                        }

                        let history = &work_history[work_id as usize];
                        if history.is_empty() {
                            warn!(work_id, "Stale nonce (no work history)");
                            continue;
                        }
                        if !seen_shares.insert((work_id, nonce)) {
                            continue; // duplicate (FPGA RX FIFO re-surfaced this nonce)
                        }
                        if dcentrald_common::should_clear_seen_shares(
                            seen_shares.len(),
                            dcentrald_common::DEFAULT_SEEN_SHARES_CAP,
                        ) {
                            seen_shares.clear();
                        }

                        let latest_entry = history
                            .back()
                            .expect("history checked non-empty")
                            .clone();

                        let ms_idx = (solution_id as usize)
                            .min(latest_entry.version_bits_per_midstate.len().saturating_sub(1));
                        let share_version_bits = latest_entry.version_bits_per_midstate
                            .get(ms_idx)
                            .cloned()
                            .flatten();
                        let _rolled_version_guess = match &share_version_bits {
                            Some(vb) => latest_entry.version ^ u32::from_str_radix(vb, 16).unwrap_or(0),
                            None => latest_entry.version,
                        };

                        let missing_version_reconstruction = latest_entry.version_rolling_enabled
                            && latest_entry.version_bits_per_midstate.iter().all(|vb| vb.is_none());
                        if missing_version_reconstruction {
                            if !unsupported_share_submit_logged {
                                unsupported_share_submit_logged = true;
                                warn!(
                                    "S19j hybrid path cannot yet reconstruct BM1362 rolled version bits from FPGA nonce metadata — dropping share submissions to avoid malformed pool submits"
                                );
                            }
                            continue;
                        }

                        if let Some((entry, rolled_version, share_version_bits)) = history.iter().rev().find_map(|candidate| {
                            let ms_idx = (solution_id as usize)
                                .min(candidate.version_bits_per_midstate.len().saturating_sub(1));
                            let share_version_bits = candidate
                                .version_bits_per_midstate
                                .get(ms_idx)
                                .cloned()
                                .flatten();
                            let rolled_version = match &share_version_bits {
                                Some(vb) => candidate.version ^ u32::from_str_radix(vb, 16).unwrap_or(0),
                                None => candidate.version,
                            };
                            let header = hybrid_build_header(candidate, rolled_version, nonce);
                            if dcentrald_stratum::share_pipeline::validate_full_header(&header, &candidate.share_target) {
                                Some((candidate.clone(), rolled_version, share_version_bits))
                            } else {
                                None
                            }
                        }) {
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
                                    info!(nonce = format_args!("0x{:08X}", nonce), "SHARE #{}", shares_submitted);
                                }
                                Err(e) => {
                                    error!(error = %e, "Share channel closed");
                                    break;
                                }
                            }
                        }

                        if nonces_this_poll > 100 { break; }
                    }
                }

                _ = hashrate_timer.tick() => {
                    // Per-5s FPGA status snapshot — always emitted.
                    let ctrl_now = fpga.read_ctrl();
                    let errs = fpga.read_error_count();
                    let tx_full = fpga.work_tx_full();
                    let rx_has = fpga.work_rx_has_data();
                    let tx_last = fpga.read_work_tx_last();
                    info!(
                        chain = fpga_chain_id,
                        ctrl = format_args!("0x{:08X}", ctrl_now),
                        err_cnt = errs,
                        tx_last = format_args!("0x{:08X}", tx_last),
                        tx_full,
                        rx_empty = !rx_has,
                        nonces_5s = hr_nonces,
                        "fpga_status"
                    );

                    if let (Some(timeout), Some(first_work)) = (no_nonce_timeout, first_work_at) {
                        if total_nonces == 0 && first_work.elapsed() >= timeout {
                            let tx_stat = fpga.read_work_tx_status();
                            let rx_stat = fpga.read_work_rx_status();
                            let reason = format!(
                                "AM2 no-nonce stall after {}s: work_sent={} shares={} tx_stat=0x{:08X} tx_last=0x{:08X} rx_stat=0x{:08X}",
                                first_work.elapsed().as_secs(),
                                total_work,
                                shares_submitted,
                                tx_stat,
                                tx_last,
                                rx_stat
                            );
                            error!(
                                work_sent = total_work,
                                shares = shares_submitted,
                                timeout_s = timeout.as_secs(),
                                tx_stat = format_args!("0x{:08X}", tx_stat),
                                tx_last = format_args!("0x{:08X}", tx_last),
                                rx_stat = format_args!("0x{:08X}", rx_stat),
                                "AM2 no-nonce stall detected - forcing home hard-stop"
                            );
                            force_am2_home_hard_stop(&self.config, "no-nonce-stall");
                            no_nonce_stall_reason = Some(reason);
                            self.shutdown.cancel();
                            break;
                        }
                    }

                    // Per-5s PSU status snapshot (Phase 5B instrumentation).
                    if let Some(ref psu_mutex) = psu_arc {
                        let psu_guard = psu_mutex.lock().unwrap_or_else(|e| e.into_inner());
                        let ticks = psu_guard.heartbeat_ticks();
                        let gate_ok = psu_guard.is_voltage_set_allowed();
                        let model = psu_guard.model().name();
                        let fw = psu_guard.fw_byte().unwrap_or(0);
                        info!(
                            chain = fpga_chain_id,
                            model,
                            fw = format_args!("0x{:02X}", fw),
                            hb_ticks = ticks,
                            voltage_gate_ok = gate_ok,
                            "psu_status"
                        );
                    }

                    let elapsed = last_hr_time.elapsed().as_secs_f64();
                    if elapsed > 0.0 && hr_nonces > 0 {
                        let ths = hr_nonces as f64 * HW_DIFFICULTY as f64 * 4_294_967_296.0 / elapsed / 1e12;
                        info!("{:.2} TH/s — {} nonces, {} shares, {} CRC errs, {}s uptime",
                            ths, total_nonces, shares_submitted, errs, start_time.elapsed().as_secs());
                    }
                    hr_nonces = 0;
                    last_hr_time = Instant::now();
                }
            }
        }

        // ================================================================
        // Graceful shutdown — stop all heartbeat ownership, command the bulk
        // rail toward minimum, then disarm its watchdog. If the minimum-ramp
        // fails, the armed watchdog becomes the independent backstop after any
        // already in-flight heartbeat transfer drains.
        // ================================================================
        info!("=== SHUTDOWN: graceful PSU teardown ===");
        let feeders_quiesced =
            stop_am2_runtime_feeders_bounded(&self.config, &mut runtime_threads, "normal-shutdown")
                .await;
        let mut endpoint_authority_error: Option<anyhow::Error> = None;
        let shutdown_evidence = if feeders_quiesced {
            // No heartbeat can race after this disable: both feeder handles
            // have been observed finished and joined before controller traffic.
            match pic0x89_clean_stop_owner_policy(
                selected_pic_endpoint_required,
                selected_pic_endpoint_session.is_some(),
            ) {
                Pic0x89CleanStopOwnerPolicy::Endpoint => {
                    let pic = selected_pic_endpoint_session
                        .as_mut()
                        .expect("endpoint policy requires retained session")
                        .controller_mut();
                    match pic.disable_voltage() {
                        Ok(()) => info!(
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "PIC voltage disabled after heartbeat feeders quiesced"
                        ),
                        Err(e) => warn!(
                            error = %e,
                            addr = format_args!("0x{:02X}", selected_pic_addr),
                            "PIC voltage disable failed during shutdown"
                        ),
                    }
                }
                Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint => {
                    error!(
                        addr = format_args!("0x{:02X}", selected_pic_addr),
                        "exact AM2 endpoint session missing at normal shutdown; refusing raw address/firmware reconstruction"
                    );
                    force_am2_home_hard_stop(
                        &self.config,
                        "normal-shutdown-missing-exact-pic-endpoint",
                    );
                    endpoint_authority_error = Some(anyhow::anyhow!(
                        "exact AM2 endpoint session missing at normal shutdown"
                    ));
                }
                Pic0x89CleanStopOwnerPolicy::LegacyCompatibility => {
                    if let (Some(service), Some(fw)) = (i2c0_service.as_ref(), heartbeat_pic_fw) {
                        let mut pic = Pic0x89Service::new_with_fw(
                            service.clone(),
                            selected_pic_addr,
                            Some(fw),
                        );
                        match pic.disable_voltage() {
                            Ok(()) => info!(
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                "legacy-compatible PIC voltage disabled after heartbeat feeders quiesced"
                            ),
                            Err(e) => warn!(
                                error = %e,
                                addr = format_args!("0x{:02X}", selected_pic_addr),
                                "legacy-compatible PIC voltage disable failed during shutdown"
                            ),
                        }
                    }
                }
            }
            shutdown_am2_psu_after_feeders_quiesced(&psu_arc, "normal-shutdown")
        } else {
            // The bounded stop already asserted PWR_CONTROL independently.
            // Do not touch PIC/I2C or the PSU mutex while a detached worker may
            // still own either transport.
            Am2PowerShutdownEvidence::hard_stop_after_timeout(psu_arc.is_some())
        };
        // PWR_CONTROL gate auto-deasserts via `Apw121215a::Drop` when the
        // last `Arc<Mutex<Apw121215a>>` reference (held in `psu_arc`)
        // is dropped at the end of this function scope — after the
        // heartbeat thread has joined and released its clone above.
        // No explicit deassert here.

        if !shutdown_evidence.completed_gracefully() {
            error!(
                ?shutdown_evidence,
                "AM2 shutdown used a hard-stop fallback or failed its PSU safe-direction plan"
            );
            anyhow::bail!(
                "AM2 shutdown did not complete gracefully: {:?}",
                shutdown_evidence
            );
        }
        info!(?shutdown_evidence, "=== SHUTDOWN COMPLETE ===");
        if let Some(error) = endpoint_authority_error {
            return Err(error);
        }
        if let Some(reason) = no_nonce_stall_reason {
            anyhow::bail!("{}", reason);
        }
        Ok(())
    }
}

fn hybrid_build_header(entry: &WorkEntry, rolled_version: u32, nonce: u32) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&rolled_version.to_le_bytes());
    header[4..36].copy_from_slice(&entry.prev_block_hash);
    header[36..68].copy_from_slice(&entry.merkle_root);
    header[68..72].copy_from_slice(&entry.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&entry.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

fn am2_hybrid_reconstruct_rolled_version(base_version: u32, version_bits_raw: u16) -> (u32, u32) {
    bip320_reconstruct_rolled_version(base_version, version_bits_raw)
}

fn am2_phase1_select_serial_device(serial_devices: &[String], default_device: &str) -> String {
    serial_devices
        .first()
        .cloned()
        .unwrap_or_else(|| default_device.to_string())
}

fn log_am2_planned_chain_contexts(plan: &[Am2ChainContext]) {
    for ctx in plan {
        info!(
            planned_chain_id = ctx.chain_id,
            planned_serial_device = %ctx.serial_device,
            planned_am2_slot = ctx.am2_slot,
            planned_dspic_addr = format_args!("0x{:02X}", ctx.dspic_addr),
            selected_for_phase1_execution = ctx.chain_id == 0,
            "AM2 immutable planned chain context"
        );
    }
}

/// Locally-proven achieved difficulty for an AM2 serial-dispatch share —
/// computed by SHA256d-ing the rebuilt header (with rolled version + nonce
/// applied) and converting the big-endian hash to a difficulty value.
///
/// Mirrors `am3_bb_achieved_difficulty_from_header` in `am3_bb_mining.rs`.
/// Populates `ValidShare.achieved_difficulty` so  9F achieved-vs-
/// pool-target reporting works correctly on AM2 serial-dispatch shares
/// (previously hard-coded to `None` — see
///  F9).
fn am2_hybrid_achieved_difficulty_from_header(header: &[u8; 80]) -> Option<f64> {
    let hash = dcentrald_stratum::work::double_sha256(header);
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }
    let difficulty = dcentrald_stratum::v1::difficulty::hash_to_difficulty(&hash_be);
    if difficulty.is_finite() && difficulty > 0.0 {
        Some(difficulty)
    } else {
        None
    }
}

/// Calculate WORK_TIME for BM1362 at a given frequency.
fn calculate_work_time_bm1362(freq_mhz: u16) -> u32 {
    const FPGA_WORK_CLK: f64 = 100_000_000.0;
    let freq_hz = freq_mhz as f64 * 1_000_000.0;
    let nonce_range = 1_048_576.0; // 2^20
    let work_time = (0.9 * nonce_range / freq_hz * FPGA_WORK_CLK) as u32;
    work_time.max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        admit_open_core_rail_plan,
        // RE-018 cold-wake byte-exact decode constants + cold-sequence gate.
        // Pre-existing module-scope items that `re018_decoded_register_values_are_byte_exact`
        // asserts on but were never imported into this test module, so the binary
        // test target failed to compile. The breakage went undetected until the
        // binary `cargo test --no-run` compile gate was wired into CI (QA-004) and
        // the cross-toolchain gate ran with `--tests`. Importing them is logic-neutral.
        am2_at3_rail_read_interval_s,
        am2_at3_rail_read_opted_in,
        am2_bm1362_re018_cold_sequence_enabled,
        am2_bus_prime_order,
        am2_chain_id_for_pic_addr,
        am2_dual_chain_second_uart,
        am2_dual_chain_ttys3_enabled,
        am2_env_flag_off,
        am2_fan_fault_step,
        am2_fast_uart_settle_ms,
        am2_freq_only_clamp_applied_mhz,
        am2_frequency_autotune_opted_in,
        am2_graded_throttle_target_mhz,
        am2_hb_reset_attempt_budget,
        am2_hybrid_reconstruct_rolled_version,
        am2_mid_run_nonce_stall_timeout,
        am2_mid_run_nonce_stalled,
        am2_nonce_recently_active,
        am2_phase1_select_serial_device,
        am2_re018_pll_hex_within_envelope,
        am2_rolling_window_baseline,
        am2_safe_teardown_enabled,
        am2_serial_echoed_job_id,
        am2_teardown_params,
        am2_uart_fallback_candidates,
        arm_am2_teardown_params,
        board_control_uio_number,
        build_am2_freq_only_autotuner_config,
        compute_quiet_idle_pwm,
        detected_dspic_fw_allows_voltage_commands,
        hybrid_build_header,
        parse_ablation_fields,
        parse_gpio_number_spec,
        parse_hybrid_pic_fw_reply,
        pic_get_version_transaction_steps,
        proc_comm_matches_target,
        psu_override_active,
        s19j_hybrid_chip_rail_target_mv,
        Am2SerialChainState,
        Am2SerialChainStats,
        Am2ShareAccounting,
        OpenCoreRailAdmissionError,
        S19jHybridMiner,
        WorkEntry,
        AM2_DUAL_CHAIN_SECOND_UART_DEFAULT,
        AM2_FAN_FAULT_STRIKES,
        AM2_FASTUART_SETTLE_MS_DEFAULT,
        AM2_FASTUART_SETTLE_MS_MAX,
        AM2_FASTUART_SETTLE_MS_MIN,
        AM2_FREQ_ONLY_APPLIED_CEIL_MHZ,
        AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ,
        AM2_MID_RUN_STALL_MIN_DEFAULT_S,
        AM2_RECENT_HASHRATE_WINDOW_S,
        AM2_SERIAL_JOB_ID_STEP,
        AM2_THERMAL_THROTTLE_STEP_MHZ,
        BOARD_CONTROL_UIO_FALLBACK,
        ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S,
        INIT_CONTROL_BCAST,
        INIT_CONTROL_PER_CHIP,
        NONCE_RANGE_126,
        PIC_GET_VERSION_CLEAN_RETRIES,
        PIC_GET_VERSION_RETRY_DELAY_MS,
        RE018_ANALOG_MUX_54,
        RE018_BCAST_A8,
        RE018_CORE_3C_CLK_DELAY,
        RE018_CORE_3C_FAMILY,
        RE018_CORE_3C_HASH_CLK,
        RE018_FAST_UART_28,
        RE018_FULL_CORE_BCAST_A8,
        RE018_FULL_CORE_PER_CHIP_A8,
        RE018_IO_DRIVER_58,
        RE018_LOW_FREQ_MHZ,
        RE018_LOW_FREQ_PLL_08,
        RE018_MISC_CTRL,
        RE018_NONCE_SPACE_BASE,
        RE018_NONCE_SPACE_REG,
        RE018_NONCE_SPACE_STRIDE,
        RE018_PER_CHIP_A8,
        RE018_PLL0_DIVIDER_70,
        RE018_PLL_08,
        RE018_VERSION_MASK,
        S19J_HYBRID_CHIP_RAIL_TARGET_MV,
        S19_DSPIC_ADDRS,
        TICKET_MASK_256,
    };
    use crate::config::DcentraldConfig;
    use crate::config::PsuOverride;
    use dcentrald_hal::i2c::I2cTransactionStep;
    use proptest::prelude::*;

    const S19J_SOURCE: &str = include_str!("s19j_hybrid_mining.rs");

    #[test]
    fn open_core_rail_plan_is_atomic_and_fail_closed() {
        let inactive = admit_open_core_rail_plan(false, true, 13_700, 15_000)
            .expect("inactive experiment ignores unused elevated inputs");
        assert_eq!(inactive.steady_mv, 13_700);
        assert_eq!(inactive.energization_mv, 13_700);
        assert!(!inactive.requires_demotion());

        assert_eq!(
            admit_open_core_rail_plan(true, true, 13_700, 15_000),
            Err(OpenCoreRailAdmissionError::AllActiveVoltageEnable)
        );
        for requested_mv in [13_699, 13_700] {
            assert_eq!(
                admit_open_core_rail_plan(true, false, 13_700, requested_mv),
                Err(OpenCoreRailAdmissionError::TargetNotElevated {
                    requested_mv,
                    steady_mv: 13_700,
                })
            );
        }

        let elevated = admit_open_core_rail_plan(true, false, 13_700, 15_000)
            .expect("one selected elevated rail is admitted");
        assert_eq!(elevated.energization_mv, 15_000);
        assert!(elevated.requires_demotion());
    }

    #[test]
    fn elevated_rail_has_one_asic_init_attempt_without_a_dwell_budget() {
        assert_eq!(am2_hb_reset_attempt_budget(true, 20, true), 1);
        assert_eq!(am2_hb_reset_attempt_budget(false, 20, false), 1);
        assert_eq!(am2_hb_reset_attempt_budget(true, 0, false), 1);
        assert_eq!(am2_hb_reset_attempt_budget(true, 6, false), 6);
        assert_eq!(am2_hb_reset_attempt_budget(true, u8::MAX, false), 20);
    }

    #[test]
    fn am2_endpoint_migration_reuses_existing_eeprom_and_version_observations() {
        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        let run = run
            .split("fn log_am2_planned_chain_contexts")
            .next()
            .expect("bounded S19j run body");
        assert_eq!(
            run.matches("read_hashboard_eeprom_prefix_via_service_for_energize_gate(")
                .count(),
            1,
            "the typed service reader remains the only hybrid EEPROM transaction source"
        );
        assert_eq!(
            run.matches("read_hashboard_eeprom_prefix_at(").count(),
            1,
            "the bootstrap readiness probe must use the same typed service operation"
        );
        assert!(!run.contains("read_bytes(0x50"));
        assert!(run.contains("HalError::I2cEndpointNotReady"));
        assert!(run.contains("AM2 hashboard EEPROM bootstrap service failed terminally"));
        assert!(!run.contains("read_hashboard_eeprom_for_energize_gate("));
        assert!(run.contains("OwnedEepromReadinessError::Terminal"));
        assert!(run.contains("am2-hashboard-eeprom-service-terminal"));
        assert_eq!(run.matches("bind_am2_hashboard_presence(").count(), 1);
        assert!(!run.contains("observe_am2_hashboard_presence("));
        assert_eq!(run.matches("observe_am2_endpoint_firmware(").count(), 3);
        assert!(!run.contains("pic_read_fw_version_service("));
    }

    #[test]
    fn selected_pic0x89_owner_has_no_raw_model_address_fallback() {
        let start = S19J_SOURCE
            .find("heartbeat_pic_fw = Some(detected_fw)")
            .expect("selected Pic0x89 owner boundary");
        let owner = &S19J_SOURCE[start..];
        let owner = owner
            .split("// WAKE-DSPIC-BEFORE-RAIL")
            .next()
            .expect("bounded selected owner");
        assert!(owner.contains("controller_endpoint.take()"));
        assert!(owner.contains("Pic0x89EndpointSession::new"));
        assert!(!owner.contains("Pic0x89Service::new_with_fw"));
        assert!(owner.contains("trust-rail/model/address fallback cannot construct"));
    }

    #[test]
    fn selected_pic0x89_endpoint_session_is_retained_through_clean_stop() {
        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        assert!(run.contains("let mut selected_pic_endpoint_session"));
        assert!(run.contains("Option<Pic0x89EndpointSession>"));
        assert!(run.contains("selected_pic_endpoint_session = Some("));
        assert!(run.contains(".controller_mut()"));

        let clean_stop_start = run
            .rfind(".run_am2_serial_dispatch_loop(")
            .expect("single-chain dispatch clean-stop boundary");
        let clean_stop = &run[clean_stop_start..];
        let clean_stop = clean_stop
            .split("// ---- Phase 10b: Mining loop")
            .next()
            .expect("bounded single-chain clean stop");
        assert!(clean_stop.contains("selected_pic_endpoint_session.as_mut()"));
        assert!(clean_stop.contains("selected_pic_endpoint_required"));
        assert!(clean_stop.contains("RefuseMissingExactEndpoint"));
        assert!(clean_stop.contains("refusing raw address/firmware reconstruction"));

        let exact_branch = clean_stop
            .split("Pic0x89CleanStopOwnerPolicy::Endpoint =>")
            .nth(1)
            .expect("exact endpoint clean-stop branch");
        let exact_branch = exact_branch
            .split("Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint")
            .next()
            .expect("bounded exact endpoint clean-stop branch");
        assert!(exact_branch.contains(".controller_mut()"));
        assert!(!exact_branch.contains("Pic0x89Service::new_with_fw"));
    }

    #[test]
    fn normal_shutdown_exact_pic0x89_owner_cannot_reconstruct_raw_authority() {
        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        let shutdown = run
            .split("// Graceful shutdown")
            .nth(1)
            .expect("normal shutdown boundary");
        let shutdown = shutdown
            .split("if let Some(reason) = no_nonce_stall_reason")
            .next()
            .expect("bounded normal shutdown");
        assert!(shutdown.contains("selected_pic_endpoint_required"));
        assert!(shutdown.contains("selected_pic_endpoint_session.is_some()"));
        assert!(shutdown.contains("RefuseMissingExactEndpoint"));
        assert!(shutdown.contains("normal-shutdown-missing-exact-pic-endpoint"));
        assert!(shutdown.contains("endpoint_authority_error"));

        let exact_branch = shutdown
            .split("Pic0x89CleanStopOwnerPolicy::Endpoint =>")
            .nth(1)
            .expect("normal-shutdown exact endpoint branch");
        let exact_branch = exact_branch
            .split("Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint")
            .next()
            .expect("bounded normal-shutdown exact endpoint branch");
        assert!(exact_branch.contains(".controller_mut()"));
        assert!(!exact_branch.contains("Pic0x89Service::new_with_fw"));

        let legacy_branch = shutdown
            .split("Pic0x89CleanStopOwnerPolicy::LegacyCompatibility =>")
            .nth(1)
            .expect("normal-shutdown non-target compatibility branch");
        assert!(legacy_branch.contains("Pic0x89Service::new_with_fw"));
    }

    #[test]
    fn post_enable_uart_gate_failure_reuses_retained_pic0x89_controller() {
        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        let failure_owner = run
            .split("let rx_bytes_pre_init = match post_enable_chain_uart_probe(")
            .nth(1)
            .expect("post-ENABLE UART gate owner");
        let failure_owner = failure_owner
            .split("// Phase 3.5: post-ENABLE UART evidence")
            .next()
            .expect("bounded post-ENABLE UART gate owner");

        assert_eq!(
            failure_owner.matches("pic.disable_voltage()").count(),
            2,
            "both failure branches must reuse the retained endpoint controller"
        );
        assert_eq!(failure_owner.matches("pic.send_heartbeat()").count(), 2);
        assert!(!failure_owner.contains("Pic0x89Service::new_with_fw"));
        assert!(!failure_owner.contains("DspicService::new"));
    }

    #[test]
    fn selected_pic0x89_heartbeat_owner_is_issued_by_retained_endpoint_session() {
        let heartbeat = S19J_SOURCE
            .split("fn spawn_pic_heartbeat_thread(")
            .nth(1)
            .expect("heartbeat owner");
        let heartbeat = heartbeat
            .split("fn log_am2_dispatch_snapshot")
            .next()
            .expect("bounded heartbeat owner");
        assert!(heartbeat.contains("mut pic: Pic0x89Service"));

        let selected_owner = heartbeat
            .split("let mut extra_pics")
            .next()
            .expect("selected heartbeat owner before additional-address debt");
        assert!(heartbeat.contains("match pic.send_heartbeat()"));
        assert!(!selected_owner.contains("Pic0x89Service::new_with_fw"));
        assert!(!selected_owner.contains("DspicService::new"));

        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        let issuance = run
            .split("selected_pic_heartbeat_controller = Some(")
            .nth(1)
            .expect("endpoint-issued heartbeat controller");
        let issuance = issuance
            .split("let pic = selected_pic_endpoint_session")
            .next()
            .expect("bounded heartbeat controller issuance");
        assert!(issuance.contains("selected_pic_endpoint_session"));
        assert!(issuance.contains(".controller()"));
        let handoff = run
            .split("spawn_pic_heartbeat_thread(")
            .nth(1)
            .expect("heartbeat ownership handoff");
        let handoff = handoff
            .split("// Phase 3c: voltage settle window")
            .next()
            .expect("bounded heartbeat ownership handoff");
        assert!(handoff.contains("selected_pic_heartbeat_controller.take()"));
        assert!(handoff
            .contains("endpoint-issued controller missing before heartbeat ownership handoff"));
        assert!(!handoff.contains("selected_pic_addr,"));
        assert!(!handoff.contains("heartbeat_pic_fw,"));
    }

    #[test]
    fn selected_pic0x89_thermal_owner_is_issued_by_retained_endpoint_session() {
        let supervisor = S19J_SOURCE
            .split("struct Am2ThermalSupervisor")
            .nth(1)
            .expect("thermal supervisor");
        let supervisor = supervisor
            .split("fn am2_slot_from_serial_device")
            .next()
            .expect("bounded thermal supervisor");
        assert!(supervisor.contains("pic: Option<Pic0x89Service>"));
        assert!(supervisor.contains("self.pic.as_mut()"));
        assert!(!supervisor.contains("Pic0x89Service::new_with_fw"));
        assert!(!supervisor.contains("DspicService::new"));

        let run = S19J_SOURCE
            .split("pub async fn run(&mut self)")
            .nth(1)
            .expect("S19j run body");
        let handoff = run
            .split("let thermal_pic = match pic0x89_clean_stop_owner_policy(")
            .nth(1)
            .expect("thermal ownership handoff");
        let handoff = handoff
            .split("let mut sup = Am2ThermalSupervisor::new(")
            .next()
            .expect("bounded thermal ownership handoff");
        assert!(handoff.contains("selected_pic_endpoint_session"));
        assert!(handoff.contains(".controller()"));
        assert!(handoff.contains("RefuseMissingExactEndpoint"));
        assert!(handoff.contains("thermal-supervisor-missing-exact-pic-endpoint"));

        let exact_branch = handoff
            .split("Pic0x89CleanStopOwnerPolicy::Endpoint =>")
            .nth(1)
            .expect("exact thermal endpoint branch");
        let exact_branch = exact_branch
            .split("Pic0x89CleanStopOwnerPolicy::RefuseMissingExactEndpoint")
            .next()
            .expect("bounded exact thermal endpoint branch");
        assert!(exact_branch.contains(".controller()"));
        assert!(!exact_branch.contains("Pic0x89Service::new_with_fw"));

        let legacy_branch = handoff
            .split("Pic0x89CleanStopOwnerPolicy::LegacyCompatibility =>")
            .nth(1)
            .expect("thermal non-target compatibility branch");
        assert!(legacy_branch.contains("Pic0x89Service::new_with_fw"));
    }

    /// Minimal valid `DcentraldConfig` for the freq-only autotuner
    /// gate/quiet-default tests. Every section has a serde default; a
    /// bare `[pool]` is the smallest deserializable form.
    fn min_config() -> DcentraldConfig {
        toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#,
        )
        .expect("minimal config must deserialize")
    }

    // FIX 3 (2026-06-28): the `DCENT_AM2_RE018_PLL_HEX` raw lab escape-hatch is now
    // validated against the SAME VCO/operating envelope `pll_lookup_extended`
    // enforces (VCO = 25*FBDIV/REFDIV in [2400,5975] MHz AND decoded freq in
    // [240,597] MHz). `am2_re018_pll_hex_within_envelope` is the pure predicate.
    #[test]
    fn re018_pll_hex_envelope_accepts_proven_and_refuses_unsafe() {
        // Proven in-envelope values pass:
        //  - the resolver's own default (320 MHz, VCO 3200, FBDIV=128)
        assert!(am2_re018_pll_hex_within_envelope(RE018_LOW_FREQ_PLL_08));
        //  - the documented escape-hatch example 0x50800141 (same value)
        assert!(am2_re018_pll_hex_within_envelope(0x5080_0141));
        //  - the bottom of pll_lookup_extended's range: 240 MHz (FBDIV=96, VCO 2400)
        assert!(am2_re018_pll_hex_within_envelope(
            dcentrald_asic::drivers::bm1362::pll_lookup_extended(240).0
        ));
        //  - the top of the table: 597 MHz (FBDIV=239, VCO 5975)
        assert!(am2_re018_pll_hex_within_envelope(0x50EF_0141));
        //  - a set PLL lock bit is masked off and must not change the verdict
        assert!(am2_re018_pll_hex_within_envelope(
            0x5080_0141 | dcentrald_asic::drivers::bm1362::PLL_LOCK_BIT
        ));

        // The canonical live-FALSIFIED value 0x50D2_0164 (VCO 5250 IN band, but a
        // ÷35 postdiv -> 150 MHz, off the proven low-VCO regime) is REFUSED on the
        // operating-frequency check — this is the exact fat-fingered shape the
        // clamp must catch.
        assert!(!am2_re018_pll_hex_within_envelope(0x50D2_0164));
        // The ~50 MHz low-VCO eco default (VCO 2100 < 2400, 50 MHz) is below the
        // pll_lookup_extended envelope and is refused via the escape hatch (the
        // standalone path applies it directly, not through this resolver).
        assert!(!am2_re018_pll_hex_within_envelope(RE018_PLL_08));
        // Grossly-high VCO (FBDIV=255 -> VCO 6375 > 5975) is refused.
        assert!(!am2_re018_pll_hex_within_envelope(0x50FF_0141));
        // Degenerate FBDIV=0 is refused (no divide, no lock).
        assert!(!am2_re018_pll_hex_within_envelope(0x5000_0141));
    }

    #[test]
    fn at3_chain_id_for_pic_addr_maps_canonical_dspic_addrs_to_slots() {
        // AT-3 keys the per-chain telemetry slot by the canonical AM2 slot
        // index (mirrors S19_DSPIC_ADDRS / am2_chain_plan).
        assert_eq!(am2_chain_id_for_pic_addr(0x20), Some(0));
        assert_eq!(am2_chain_id_for_pic_addr(0x21), Some(1));
        assert_eq!(am2_chain_id_for_pic_addr(0x22), Some(2));
        assert_eq!(am2_chain_id_for_pic_addr(0x23), Some(3));
        // Unrecognized address → None (AT-3 publishes nothing, never a
        // wrong-keyed reading).
        assert_eq!(am2_chain_id_for_pic_addr(0x55), None);
        assert_eq!(am2_chain_id_for_pic_addr(0x00), None);
    }

    #[test]
    fn at3_rail_read_gate_defaults_off_and_opts_in_via_config() {
        // Default-OFF is load-bearing — with the gate closed the AT-3 select!
        // arm is never polled and the loop is byte-identical to the proven path.
        // (Guard the default-false assertion on the opt-in env being unset so a
        // CI runner that happens to export it can't make this flaky.)
        if std::env::var(dcentrald_autotuner::config::AT3_RAIL_READ_ENV).is_err() {
            assert!(
                !am2_at3_rail_read_opted_in(&min_config()),
                "AT-3 must default OFF"
            );
        }
        // The TOML key opts in regardless of env (returns before reading it).
        let mut cfg = min_config();
        cfg.autotuner.at3_rail_read = true;
        assert!(am2_at3_rail_read_opted_in(&cfg));
    }

    #[test]
    fn at3_rail_read_interval_defaults_30_and_clamps() {
        // Default cadence is 30 s.
        assert_eq!(am2_at3_rail_read_interval_s(&min_config()), 30);
        // Below the 15 s floor clamps up; above the 120 s ceiling clamps down.
        let mut fast = min_config();
        fast.autotuner.at3_rail_read_interval_s = 1;
        assert_eq!(am2_at3_rail_read_interval_s(&fast), 15);
        let mut slow = min_config();
        slow.autotuner.at3_rail_read_interval_s = 10_000;
        assert_eq!(am2_at3_rail_read_interval_s(&slow), 120);
    }

    #[test]
    fn am2_bus_prime_order_primes_non_selected_ascending() {
        //  B05: assert the ACTUAL runtime order, not a source string.
        // With selected=0x20 (the real /p config, serial_device=ttyS1),
        // the non-selected PICs 0x21/0x22 are primed FIRST and 0x20 is warmed
        // LAST — there is NO "0x20 first" guarantee.
        assert_eq!(
            am2_bus_prime_order(&[0x20, 0x21, 0x22], 0x20),
            vec![0x21, 0x22]
        );
        assert_eq!(am2_bus_prime_order(&[0x20, 0x22], 0x20), vec![0x22]);
        // selected=0x22 (serial_device=ttyS3 case): primes 0x20/0x21 first.
        assert_eq!(
            am2_bus_prime_order(&[0x20, 0x21, 0x22], 0x22),
            vec![0x20, 0x21]
        );
        // ascending regardless of input order.
        assert_eq!(
            am2_bus_prime_order(&[0x22, 0x20, 0x21], 0x20),
            vec![0x21, 0x22]
        );
        // selected not present → all returned ascending.
        assert_eq!(am2_bus_prime_order(&[0x22, 0x20], 0x23), vec![0x20, 0x22]);
        // single PIC == selected → empty.
        assert_eq!(am2_bus_prime_order(&[0x20], 0x20), Vec::<u8>::new());
    }

    #[test]
    fn am2_hybrid_bip320_reconstruction_matches_shared_bm1362_helper() {
        let base_version = 0x21FF_E000;
        let version_bits_raw = 0x0014;
        let nonce = 0xAABB_CCDD;

        let (rolled_version, vbits_delta) =
            am2_hybrid_reconstruct_rolled_version(base_version, version_bits_raw);
        assert_eq!(
            (rolled_version, vbits_delta),
            dcentrald_asic::bm1362::bip320_reconstruct_rolled_version(
                base_version,
                version_bits_raw,
            ),
            "AM2 hybrid serial-dispatch must stay wired to the shared BM1362 BIP320 helper"
        );
        assert_ne!(
            vbits_delta, 0,
            "non-zero version_bits_raw must be reconstructed, not dropped before validation"
        );

        let entry = WorkEntry {
            job_id: "job".to_string(),
            extranonce2: "00000000".to_string(),
            ntime: 0x6655_4433,
            nbits: 0x1d00_ffff,
            version: base_version,
            share_target: [0xff; 32],
            prev_block_hash: [0x11; 32],
            merkle_root: [0x22; 32],
            version_bits_per_midstate: Vec::new(),
            version_rolling_enabled: false,
        };
        let header = hybrid_build_header(&entry, rolled_version, nonce);
        assert_eq!(
            u32::from_le_bytes(header[0..4].try_into().expect("version bytes")),
            rolled_version,
            "rebuilt AM2 header must carry the reconstructed rolled version"
        );
        assert_eq!(
            u32::from_le_bytes(header[76..80].try_into().expect("nonce bytes")),
            nonce,
            "rebuilt AM2 header must carry the chip nonce"
        );
    }

    #[test]
    fn am2_phase1_multi_serial_devices_selects_first_chain_only() {
        let devices = vec!["/dev/ttyS1".to_string(), "/dev/ttyS3".to_string()];

        assert_eq!(
            am2_phase1_select_serial_device(&devices, "/dev/ttyS2"),
            "/dev/ttyS1",
            "phase-1 S19j hybrid runtime must preserve single-chain behavior while accepting the future dual-chain list"
        );
        assert_eq!(
            am2_phase1_select_serial_device(&[], "/dev/ttyS2"),
            "/dev/ttyS2",
            "empty caller input falls back to the legacy default device"
        );
    }

    #[test]
    fn am2_phase1_dual_plan_is_logged_but_execution_selector_stays_first_only() {
        let devices = vec!["/dev/ttyS1".to_string(), "/dev/ttyS3".to_string()];
        let plan = crate::am2_chain_plan::build_am2_chain_plan(&devices)
            .expect("dual AM2 chain plan should build");

        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].serial_device, "/dev/ttyS1");
        assert_eq!(plan[0].am2_slot, 0);
        assert_eq!(plan[0].dspic_addr, 0x20);
        assert_eq!(plan[0].chain_id, 0);
        assert_eq!(plan[1].serial_device, "/dev/ttyS3");
        assert_eq!(plan[1].am2_slot, 2);
        assert_eq!(plan[1].dspic_addr, 0x22);
        assert_eq!(plan[1].chain_id, 1);

        assert_eq!(
            am2_phase1_select_serial_device(&devices, "/dev/ttyS2"),
            plan[0].serial_device,
            "Phase 1 may log both planned contexts, but runtime execution remains first-context only"
        );
    }

    // ====================================================================
    // GROUP B / W8 — DUAL-CHAIN `a lab unit` ttyS3 dispatch capability
    // ====================================================================

    /// Build an 11-byte BM1362 serial nonce RX frame for the dual-chain
    /// attribution tests. Mirrors `parse_bm1362_serial_nonce`'s wire layout:
    /// `[0xAA 0x55][n0 n1 n2 n3 LE][midstate_idx][result][vbits_hi vbits_lo][flags]`.
    /// `sent_job_id` is the job id the host dispatched; the chip echoes it as
    /// `(sent << 1) & 0xF0` in the result byte high nibble (so the parsed
    /// `job_id` round-trips to `am2_serial_echoed_job_id(sent_job_id)`).
    fn make_dual_chain_nonce_frame(sent_job_id: u8, nonce: u32, vbits_raw: u16) -> Vec<u8> {
        let n = nonce.to_le_bytes();
        let v = vbits_raw.to_be_bytes();
        vec![
            0xAA,
            0x55,
            n[0],
            n[1],
            n[2],
            n[3],
            0x00,                      // midstate_idx
            (sent_job_id << 1) & 0xF0, // result byte → echoed job_id slot
            v[0],
            v[1],
            0x80, // flags: bit7 set ⇒ job response
        ]
    }

    /// Seed a chain's work-history slot with a permissive `WorkEntry` (all-FF
    /// share target ⇒ any nonce validates) so `ingest_rx` deterministically
    /// produces a share without needing a real pool job / mined nonce.
    fn seed_permissive_work_entry(
        chain: &mut Am2SerialChainState,
        sent_job_id: u8,
        job_label: &str,
    ) {
        let slot = am2_serial_echoed_job_id(sent_job_id) as usize;
        chain.work_history[slot].push_back(WorkEntry {
            job_id: job_label.to_string(),
            extranonce2: "00000000".to_string(),
            ntime: 0x6655_4433,
            nbits: 0x1d00_ffff,
            version: 0x2000_0000,
            share_target: [0xff; 32], // permissive: any header validates
            prev_block_hash: [0x11; 32],
            merkle_root: [0x22; 32],
            version_bits_per_midstate: vec![None],
            version_rolling_enabled: false,
        });
    }

    /// LOAD-BEARING: the dual-chain capability is DEFAULT-OFF. With the env
    /// unset, the gate returns false → `run()` never opens the second UART and
    /// runs the proven single-chain `run_am2_serial_dispatch_loop` (the chain-1
    /// `a lab unit`/.109 milestone path) byte-for-byte. This pins that contract.
    #[test]
    fn am2_dual_chain_gate_is_default_off() {
        const ENV: &str = "DCENT_AM2_DUAL_CHAIN_TTYS3";
        std::env::remove_var(ENV);
        assert!(
            !am2_dual_chain_ttys3_enabled(),
            "dual-chain MUST be OFF when the env is unset — gate-off is the proven single-chain path"
        );
        // Explicit truthy turns it on; falsey/garbage keep it off.
        for on in ["1", "true", "yes", "on", "ON"] {
            std::env::set_var(ENV, on);
            assert!(
                am2_dual_chain_ttys3_enabled(),
                "'{on}' must enable dual-chain"
            );
        }
        for off in ["0", "false", "no", "off", "", "maybe"] {
            std::env::set_var(ENV, off);
            assert!(
                !am2_dual_chain_ttys3_enabled(),
                "'{off}' must NOT enable dual-chain"
            );
        }
        std::env::remove_var(ENV);
    }

    /// The second chain defaults to the `a lab unit`-proven `/dev/ttyS3` (slot 2,
    /// dsPIC 0x22) and is operator-overridable for a different topology.
    #[test]
    fn am2_dual_chain_second_uart_default_and_override() {
        const ENV: &str = "DCENT_AM2_DUAL_CHAIN_SECOND_UART";
        std::env::remove_var(ENV);
        assert_eq!(
            am2_dual_chain_second_uart(),
            AM2_DUAL_CHAIN_SECOND_UART_DEFAULT,
            "default second chain UART must be /dev/ttyS3 (.25 slot-2 / dsPIC 0x22)"
        );
        assert_eq!(AM2_DUAL_CHAIN_SECOND_UART_DEFAULT, "/dev/ttyS3");

        std::env::set_var(ENV, "/dev/ttyS4");
        assert_eq!(
            am2_dual_chain_second_uart(),
            "/dev/ttyS4",
            "operator override must select the configured second UART"
        );
        // Empty override falls back to the default (defense in depth).
        std::env::set_var(ENV, "");
        assert_eq!(
            am2_dual_chain_second_uart(),
            AM2_DUAL_CHAIN_SECOND_UART_DEFAULT
        );
        std::env::remove_var(ENV);
    }

    /// A single chain's `ingest_rx` attributes a job-response nonce to the
    /// matching `WorkEntry` and produces exactly one share, dedups a replay of
    /// the same (job, nonce, vbits), and ignores non-job-response frames.
    #[test]
    fn am2_serial_chain_state_attributes_and_dedups() {
        let mut chain = Am2SerialChainState::new(0);
        let sent_job_id = AM2_SERIAL_JOB_ID_STEP; // 8 → echoed slot 8
        seed_permissive_work_entry(&mut chain, sent_job_id, "jobA");

        let frame = make_dual_chain_nonce_frame(sent_job_id, 0xDEAD_BEEF, 0x0000);
        let mut errors = 0u64;
        let shares = chain.ingest_rx(&frame, "worker.1", &mut errors);
        assert_eq!(shares.len(), 1, "one valid nonce must produce one share");
        assert_eq!(shares[0].nonce, 0xDEAD_BEEF);
        assert_eq!(shares[0].share.worker_name, "worker.1");
        assert_eq!(shares[0].share.job_id, "jobA");
        assert_eq!(shares[0].share.nonce, "deadbeef");
        assert_eq!(chain.total_nonces, 1);
        assert_eq!(chain.shares_submitted, 1);
        assert_eq!(errors, 0);

        // Replay of the identical frame is deduped (same job/nonce/vbits).
        let replay = chain.ingest_rx(&frame, "worker.1", &mut errors);
        assert!(
            replay.is_empty(),
            "duplicate (job,nonce,vbits) must be dropped"
        );
        assert_eq!(chain.shares_submitted, 1, "no second share for a replay");

        // A non-job-response frame (flags bit7 clear) is ignored as a share but
        // still counts as an RX frame.
        let mut not_job = make_dual_chain_nonce_frame(sent_job_id, 0x1234_5678, 0x0000);
        not_job[10] = 0x00; // clear job-response flag
        let none = chain.ingest_rx(&not_job, "worker.1", &mut errors);
        assert!(none.is_empty());
        assert_eq!(chain.shares_submitted, 1);
    }

    /// The dual-chain core: two independent `Am2SerialChainState`s attribute
    /// nonces to THEIR OWN chain only. A nonce produced on chain A's wire is
    /// never credited to chain B (and vice-versa), and each chain keeps its own
    /// counters — the property that makes per-chain attribution correct.
    #[test]
    fn am2_dual_chain_attributes_nonces_to_the_producing_chain_only() {
        let mut chain_a = Am2SerialChainState::new(0);
        let mut chain_b = Am2SerialChainState::new(1);

        // Same echoed job-id slot on both chains, but each chain seeds a
        // DIFFERENT pool job into that slot — so a nonce can only validate
        // against its own chain's history and is labelled with that chain's job.
        let sent = AM2_SERIAL_JOB_ID_STEP * 2; // 16 → echoed slot 16
        seed_permissive_work_entry(&mut chain_a, sent, "jobA");
        seed_permissive_work_entry(&mut chain_b, sent, "jobB");

        let mut errors = 0u64;
        // Chain A produces nonce 0xAAAA0001; only chain A ingests it.
        let frame_a = make_dual_chain_nonce_frame(sent, 0xAAAA_0001, 0x0000);
        let shares_a = chain_a.ingest_rx(&frame_a, "w", &mut errors);
        // Chain B produces nonce 0xBBBB0002; only chain B ingests it.
        let frame_b = make_dual_chain_nonce_frame(sent, 0xBBBB_0002, 0x0000);
        let shares_b = chain_b.ingest_rx(&frame_b, "w", &mut errors);

        assert_eq!(shares_a.len(), 1);
        assert_eq!(
            shares_a[0].share.job_id, "jobA",
            "chain-A nonce must carry chain-A's job"
        );
        assert_eq!(shares_a[0].nonce, 0xAAAA_0001);

        assert_eq!(shares_b.len(), 1);
        assert_eq!(
            shares_b[0].share.job_id, "jobB",
            "chain-B nonce must carry chain-B's job"
        );
        assert_eq!(shares_b[0].nonce, 0xBBBB_0002);

        // Per-chain counters are independent.
        assert_eq!(chain_a.total_nonces, 1);
        assert_eq!(chain_b.total_nonces, 1);
        assert_eq!(chain_a.shares_submitted, 1);
        assert_eq!(chain_b.shares_submitted, 1);

        // Crucially: chain A never saw chain B's nonce (different chain UARTs)
        // — feeding A's frame to B yields NO share because B's slot holds jobB
        // but A's nonce is genuinely B's only if B mined it; B did not ingest
        // A's bytes, so A's nonce is unknown to B. Prove the isolation directly:
        // a fresh chain that never ingested a frame has zero shares.
        let fresh = Am2SerialChainState::new(2);
        assert_eq!(fresh.total_nonces, 0);
        assert_eq!(fresh.shares_submitted, 0);
    }

    /// Per-chain BIP320 reconstruction stays correct: a non-zero
    /// `version_bits_raw` on a chain's nonce is reconstructed (never dropped)
    /// and surfaced in the attributed share's `version_bits` + rolled
    /// `version`, independently on each chain.
    #[test]
    fn am2_dual_chain_bip320_reconstruction_is_per_chain_correct() {
        let mut chain = Am2SerialChainState::new(0);
        let sent = AM2_SERIAL_JOB_ID_STEP * 3; // 24 → echoed slot 24
        seed_permissive_work_entry(&mut chain, sent, "jobV");

        let vbits_raw: u16 = 0x0014;
        let (_rolled, vbits_delta) = am2_hybrid_reconstruct_rolled_version(0, vbits_raw);
        assert_ne!(
            vbits_delta, 0,
            "non-zero rolled bits must reconstruct, not drop"
        );

        let frame = make_dual_chain_nonce_frame(sent, 0x0000_0042, vbits_raw);
        let mut errors = 0u64;
        let shares = chain.ingest_rx(&frame, "w", &mut errors);
        assert_eq!(shares.len(), 1);
        assert_eq!(
            shares[0].share.version_bits,
            Some(format!("{:08x}", vbits_delta)),
            "share must carry the reconstructed BIP320 version_bits delta"
        );
        // The rolled version applied to the chain's base (0x20000000 seeded
        // above) must match the canonical helper.
        let (expected_rolled, _) = am2_hybrid_reconstruct_rolled_version(0x2000_0000, vbits_raw);
        assert_eq!(shares[0].share.version, expected_rolled);
    }

    /// W24-CRASH-1 (w24-thermal-safety F-1): the process-global panic-hook
    /// teardown params set→read round-trip.
    ///
    /// `AM2_TEARDOWN_PARAMS` is a process-wide `OnceLock` (set-once), so this
    /// is the ONLY test allowed to arm it — a second arming test in the same
    /// process would see the first test's value (set is idempotent). We arm it
    /// from a config carrying non-default values (`pwr_control_gpio` set, a
    /// non-default `fan_idle_pwm`/`fan_max_pwm`) and confirm the hook would read
    /// back exactly those values, in the same shape `Am2HomeHardStopGuard`
    /// uses. This pins the wiring the panic hook depends on without invoking a
    /// real panic (the hook itself only touches sysfs/UIO, which is not
    /// host-testable; the Drop-vs-hook equivalence is the cut-hash-before-noise
    /// contract verified here at the params layer).
    #[test]
    fn am2_teardown_params_global_set_then_read_round_trip() {
        // Unset before arming proves the no-op-on-unset branch of the hook.
        // (OnceLock can't be reset, so we only get to observe this when this
        // test runs first — assert the round-trip regardless of ordering.)
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[psu]
pwr_control_gpio = "PWR_CONTROL"

[thermal]
fan_idle_pwm = 12
fan_max_pwm = 28
"#,
        )
        .expect("teardown-params test config must deserialize");

        // Sanity-check the config carries the non-default values we will arm.
        assert_eq!(config.psu.pwr_control_gpio.as_deref(), Some("PWR_CONTROL"));
        assert_eq!(config.thermal.fan_idle_pwm, 12);
        assert_eq!(config.thermal.fan_max_pwm, 28);

        arm_am2_teardown_params(&config);

        let params = am2_teardown_params().expect("params must be armed after arm_*");
        assert_eq!(
            params.pwr_control_gpio.as_deref(),
            Some("PWR_CONTROL"),
            "panic hook must read back the same PWR_CONTROL gpio spec"
        );
        assert_eq!(
            params.fan_idle_pwm, 12,
            "panic hook must read back the armed fan_idle_pwm"
        );
        assert_eq!(
            params.fan_max_pwm, 28,
            "panic hook must read back the armed fan_max_pwm"
        );

        // The hook drives fans through compute_quiet_idle_pwm, which only ever
        // drives PWM DOWN and is hard-capped at PWM_SAFETY_MAX (30). Confirm the
        // armed params can never command above the home cap.
        let commanded = compute_quiet_idle_pwm(params.fan_idle_pwm, params.fan_max_pwm);
        assert!(
            commanded <= 30,
            "panic-hook fan command must never exceed the home PWM cap (30); got {commanded}"
        );
        assert_eq!(commanded, 12, "min(12, 28, 30) == 12");
    }

    /// R11-7 FastUART settle-delay env knob (`DCENT_AM2_FASTUART_SETTLE_MS`).
    /// Single test (no shared guard needed — this is the ONLY test that
    /// touches this env var) walking the three contracts in sequence:
    ///   1. absent env ⇒ the hardcoded default (byte-identical behaviour),
    ///   2. valid override is parsed + applied,
    ///   3. out-of-range overrides clamp to [MIN, MAX].
    #[test]
    fn am2_fastuart_settle_ms_default_override_and_clamp() {
        const ENV: &str = "DCENT_AM2_FASTUART_SETTLE_MS";

        // Pin the canonical hardcoded default so a future edit to the live
        // sleep value cannot silently diverge from the env default.
        assert_eq!(
            AM2_FASTUART_SETTLE_MS_DEFAULT, 1000,
            "default must stay the canonical ~1000 ms host settle"
        );
        assert_eq!(AM2_FASTUART_SETTLE_MS_MIN, 100);
        assert_eq!(AM2_FASTUART_SETTLE_MS_MAX, 3000);

        // 1. Absent env ⇒ default (byte-identical to the prior hardcoded sleep).
        std::env::remove_var(ENV);
        assert_eq!(
            am2_fast_uart_settle_ms(),
            AM2_FASTUART_SETTLE_MS_DEFAULT,
            "absent env MUST yield the hardcoded default (no behavior change)"
        );

        // 2. Valid in-range override is parsed + applied verbatim.
        std::env::set_var(ENV, "750");
        assert_eq!(am2_fast_uart_settle_ms(), 750);
        std::env::set_var(ENV, "100");
        assert_eq!(am2_fast_uart_settle_ms(), AM2_FASTUART_SETTLE_MS_MIN);
        std::env::set_var(ENV, "3000");
        assert_eq!(am2_fast_uart_settle_ms(), AM2_FASTUART_SETTLE_MS_MAX);

        // 3. Out-of-range clamps to the bounds.
        std::env::set_var(ENV, "0");
        assert_eq!(
            am2_fast_uart_settle_ms(),
            AM2_FASTUART_SETTLE_MS_MIN,
            "below-floor MUST clamp UP to MIN"
        );
        std::env::set_var(ENV, "50");
        assert_eq!(am2_fast_uart_settle_ms(), AM2_FASTUART_SETTLE_MS_MIN);
        std::env::set_var(ENV, "999999");
        assert_eq!(
            am2_fast_uart_settle_ms(),
            AM2_FASTUART_SETTLE_MS_MAX,
            "above-ceiling MUST clamp DOWN to MAX"
        );

        // Unparseable ⇒ falls back to default (never silently changes timing).
        std::env::set_var(ENV, "not-a-number");
        assert_eq!(am2_fast_uart_settle_ms(), AM2_FASTUART_SETTLE_MS_DEFAULT);

        std::env::remove_var(ENV);
    }

    /// CE-010: `am2_env_flag_off` is the explicit-OFF parser used by default-ON
    /// gates. It must return `true` ONLY for recognised falsey values, and
    /// `false` for unset / truthy / unrecognised values. Uses a unique
    /// throwaway env name so it can never race the shared gate names.
    #[test]
    fn am2_env_flag_off_only_true_for_falsey_values() {
        const ENV: &str = "DCENT_AM2_TEST_FLAG_OFF_UNIQUE";
        std::env::remove_var(ENV);
        assert!(!am2_env_flag_off(ENV), "absent env is NOT an explicit OFF");

        for off in ["0", "false", "FALSE", "no", "NO", "off", "OFF"] {
            std::env::set_var(ENV, off);
            assert!(am2_env_flag_off(ENV), "'{off}' must parse as explicit OFF");
        }
        for on in ["1", "true", "yes", "on", "", "maybe", "2"] {
            std::env::set_var(ENV, on);
            assert!(
                !am2_env_flag_off(ENV),
                "'{on}' must NOT parse as explicit OFF"
            );
        }
        std::env::remove_var(ENV);
    }

    /// CE-010 / RE-003 / PERF-008: SAFE-TEARDOWN is now default-ON (strictly
    /// safer de-energization on failure), opt-OUT only via an explicit falsey
    /// `DCENT_AM2_SAFE_TEARDOWN`. Pins BOTH states. This is the only test that
    /// touches `DCENT_AM2_SAFE_TEARDOWN`, so it owns the name within the suite.
    #[test]
    fn am2_safe_teardown_default_on_opt_out() {
        const ENV: &str = "DCENT_AM2_SAFE_TEARDOWN";

        // Unset ⇒ ON (the promoted default — no operator AC-cycle needed).
        std::env::remove_var(ENV);
        assert!(
            am2_safe_teardown_enabled(),
            "SAFE-TEARDOWN must default ON when the env is unset"
        );

        // Explicit truthy keeps it ON.
        for on in ["1", "true", "on", "yes"] {
            std::env::set_var(ENV, on);
            assert!(
                am2_safe_teardown_enabled(),
                "'{on}' must keep SAFE-TEARDOWN ON"
            );
        }

        // Explicit falsey opts OUT (restores the legacy minimal teardown).
        for off in ["0", "false", "off", "no"] {
            std::env::set_var(ENV, off);
            assert!(
                !am2_safe_teardown_enabled(),
                "'{off}' must opt OUT of SAFE-TEARDOWN"
            );
        }

        std::env::remove_var(ENV);
    }

    /// CE-003: the board-control UIO discovery helper must fall back to the
    /// canonical `uio17` on a host (no `/sys/class/uio/.../name == board-control`
    /// node present), so the HB-reset / teardown paths behave exactly as they
    /// did with the hardcoded `BoardControl::open(17)`.
    #[test]
    fn board_control_uio_falls_back_to_17_on_host() {
        assert_eq!(BOARD_CONTROL_UIO_FALLBACK, 17);
        // On the build/test host there is no am2 board-control UIO node, so
        // discovery returns the fallback. (The OnceLock caches it; calling
        // twice must be stable.)
        let first = board_control_uio_number();
        assert_eq!(
            first, BOARD_CONTROL_UIO_FALLBACK,
            "host build with no board-control UIO node must use the uio17 fallback"
        );
        assert_eq!(
            board_control_uio_number(),
            first,
            "cached UIO number must be stable across calls"
        );
    }

    /// CE-012: teardown de-energizes EVERY S19 dsPIC controller (0x20/0x21/0x22)
    /// — the unconditional superset, not just the active-mask subset. Pin the
    /// canonical address set so a future edit cannot silently shrink it.
    #[test]
    fn s19_dspic_addrs_cover_all_three_controllers() {
        assert_eq!(
            S19_DSPIC_ADDRS,
            [0x20, 0x21, 0x22],
            "all three S19 dsPIC controller addresses must be present for unconditional teardown de-energization"
        );
        // Ascending + deduped by construction (the CE-012 arming relies on this
        // to drop the prior sort/dedup as a proven no-op).
        let mut sorted = S19_DSPIC_ADDRS;
        sorted.sort_unstable();
        assert_eq!(
            S19_DSPIC_ADDRS, sorted,
            "address const must already be ascending"
        );
    }

    #[test]
    fn psu_override_active_truth_table() {
        assert!(!psu_override_active(None));

        let disabled = PsuOverride {
            enabled: false,
            model: "APW3".to_string(),
            voltage_v: 12.8,
            no_smbus_peer: None,
            psu_hardware_variant: None,
        };
        assert!(!psu_override_active(Some(&disabled)));

        let enabled = PsuOverride {
            enabled: true,
            model: "APW3".to_string(),
            voltage_v: 12.8,
            no_smbus_peer: None,
            psu_hardware_variant: None,
        };
        assert!(psu_override_active(Some(&enabled)));
    }

    #[test]
    fn chip_rail_target_ignores_psu_override_voltage() {
        let loki_override = PsuOverride {
            enabled: true,
            model: "APW3".to_string(),
            voltage_v: 12.8,
            no_smbus_peer: None,
            psu_hardware_variant: None,
        };

        assert_eq!(S19J_HYBRID_CHIP_RAIL_TARGET_MV, 13_700);
        assert_eq!(
            s19j_hybrid_chip_rail_target_mv(Some(&loki_override)),
            13_700,
            "psu_override.voltage_v is the PSU rail and must never become the dsPIC chip-rail target"
        );
    }

    #[test]
    fn am2_uart_fallback_candidates_exclude_ps_console_uart() {
        let candidates = am2_uart_fallback_candidates();
        assert!(candidates.contains(&"/dev/ttyS1"));
        assert!(candidates.contains(&"/dev/ttyS4"));
        assert!(
            !candidates.contains(&"/dev/ttyPS0"),
            "ttyPS0 is the Zynq PS console/control UART and must never be probed by the chain fallback sweep"
        );
    }

    #[test]
    fn gpio_number_spec_parser_accepts_numeric_and_pwr_control_label_specs() {
        assert_eq!(parse_gpio_number_spec(Some("gpio:907")), Some(907));
        assert_eq!(parse_gpio_number_spec(Some("907")), Some(907));
        assert_eq!(
            parse_gpio_number_spec(Some(" label:PWR_CONTROL ")),
            Some(dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO)
        );
        assert_eq!(
            parse_gpio_number_spec(Some("PWR_CONTROL")),
            Some(dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO)
        );
        assert_eq!(
            parse_gpio_number_spec(None),
            Some(dcentrald_hal::board_control::AM2_PSU_ENABLE_GPIO)
        );
        assert_eq!(parse_gpio_number_spec(Some("label:OTHER")), None);
    }

    #[test]
    fn proc_comm_matcher_requires_exact_bosminer_name() {
        assert!(proc_comm_matches_target("bosminer\n", "bosminer"));
        assert!(proc_comm_matches_target("bosminer\0", "bosminer"));
        assert!(!proc_comm_matches_target("bosminer-helper\n", "bosminer"));
        assert!(!proc_comm_matches_target("dcentrald\n", "bosminer"));
    }

    #[test]
    fn xil_pic_get_version_framed_reply_parser_accepts_fw89() {
        assert_eq!(
            parse_hybrid_pic_fw_reply(&[0x05, 0x17, 0x89, 0x00, 0xA5]),
            Some(0x89)
        );
    }

    #[test]
    fn xil_pic_get_version_transaction_uses_bytewise_write_and_single_byte_read() {
        // R1/R2 (2026-05-21 RE): the framed GET_VERSION attempt does a clean
        // whole-frame write, NO speculative zero-flush, then ONE 1-byte read.
        let frame = [0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B];
        let steps = pic_get_version_transaction_steps(&frame, 1, false);

        // SetTimeout + WriteByteByByte(frame) + SleepMs(100) + Read(1) = 4 steps.
        assert_eq!(
            steps.len(),
            4,
            "no zero-flush + read_len=1 → exactly 4 steps"
        );
        assert!(matches!(steps[0], I2cTransactionStep::SetTimeout(10)));
        match &steps[1] {
            I2cTransactionStep::WriteByteByByte(bytes) => assert_eq!(bytes.as_slice(), &frame),
            other => panic!("unexpected GET_VERSION write step: {:?}", other),
        }
        assert!(matches!(steps[2], I2cTransactionStep::SleepMs(100)));
        assert!(matches!(steps[3], I2cTransactionStep::Read(1)));
        // No 16-zero-byte parser-flush write may precede the framed write.
        assert!(
            !steps.iter().any(|step| matches!(
                step,
                I2cTransactionStep::WriteByteByByte(bytes) if bytes.as_slice() == [0u8; 16]
            )),
            "framed GET_VERSION must NOT prepend a speculative zero-flush"
        );
    }

    #[test]
    fn pic_get_version_retry_budget_is_bosminer_faithful() {
        // R5: clean whole-frame retry up to 15× @100 ms (bosminer cadence),
        // NOT a 3-attempt-then-flush loop.
        assert_eq!(PIC_GET_VERSION_CLEAN_RETRIES, 15);
        assert_eq!(PIC_GET_VERSION_RETRY_DELAY_MS, 100);
    }

    #[test]
    fn pic_get_version_helper_can_still_prepend_flush_when_asked() {
        // The helper retains the optional flush param (used elsewhere/tests),
        // but the production version-read path never asks for it (flush=false).
        let frame = [0x55, 0xAA, 0x17];
        let steps = pic_get_version_transaction_steps(&frame, 1, true);

        assert_eq!(steps.len(), 6);
        assert!(matches!(steps[0], I2cTransactionStep::SetTimeout(10)));
        match &steps[1] {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(bytes.as_slice(), &[0u8; 16])
            }
            other => panic!("unexpected parser flush step: {:?}", other),
        }
        assert!(matches!(steps[2], I2cTransactionStep::SleepMs(10)));
        match &steps[3] {
            I2cTransactionStep::WriteByteByByte(bytes) => assert_eq!(bytes.as_slice(), &frame),
            other => panic!("unexpected GET_VERSION write step: {:?}", other),
        }
        assert!(matches!(steps[4], I2cTransactionStep::SleepMs(100)));
        assert!(matches!(steps[5], I2cTransactionStep::Read(1)));
    }

    // ------------------------------------------------------------------
    // PR-019 / R11-2 ablation summary parser (pure, host-safe)
    // ------------------------------------------------------------------

    #[test]
    fn parse_ablation_fields_extracts_canonical_summary_shape() {
        // Exact shape produced by `summarize_bm1362_serial_responses`.
        let summary = "count=126 chipid_like=126 unique_chip_addrs=126 duplicate_chipid=0 \
                       nonce_like=0 other=0 malformed=0 chip_preview=00,02,04,06,08,0A,0C,0E \
                       first=[13, 62, 03, 00, 00, 00, 00, 00, 00]";
        let (total, unique, dups, preview) = parse_ablation_fields(summary);
        assert_eq!(total, 126);
        assert_eq!(unique, 126);
        assert_eq!(dups, 0);
        assert_eq!(preview, "00,02,04,06,08,0A,0C,0E");
    }

    #[test]
    fn parse_ablation_fields_captures_126_to_28_collapse_signature() {
        // The R11-2 failure mode: many frames, few unique ids, big collision
        // count — exactly what PR-019 instrumentation must surface per phase.
        let summary = "count=90 chipid_like=90 unique_chip_addrs=28 duplicate_chipid=62 \
                       nonce_like=0 other=0 malformed=0 chip_preview=00,02,04,06,08,0A,0C,0E \
                       first=[13, 62, 03, 00, 00, 00, 00, 00, 00]";
        let (total, unique, dups, preview) = parse_ablation_fields(summary);
        assert_eq!(total, 90);
        assert_eq!(unique, 28);
        assert_eq!(
            dups, 62,
            "duplicate_chipid is the 126->28 chain-collapse signature R11-2 needs to localize"
        );
        assert_eq!(preview, "00,02,04,06,08,0A,0C,0E");
    }

    // =======================================================================
    //  — bosminer-faithful Phase 0d skip-heartbeats env-gate pin
    // =======================================================================

    #[test]
    fn wave42_env_gate_name_is_dcent_am2_dspic_bosminer_faithful() {
        // The  gate intentionally re-uses the SAME env name as the
        //  dsPIC bosminer-faithful gate. Operator opts into the
        // whole bosminer-faithful Phase 0d shape (timing + protocol) with
        // ONE flag. If a future refactor splits these into separate gates,
        // it must update this test + the run_wave42_25.sh launcher
        // together — the operator-facing contract is "one env, the entire
        // bosminer-faithful behaviour".
        std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL");
        assert!(
            !super::am2_dspic_bosminer_faithful_enabled(),
            "default-off: unset env must leave Wave-42 disabled (byte-identical \
             to today's 5×1Hz heartbeat path for .109/.79/.139)"
        );
        std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL", "1");
        assert!(
            super::am2_dspic_bosminer_faithful_enabled(),
            "operator opt-in via '1' must enable Wave-42 (skip 5×1Hz heartbeats; \
             emit single 500-ms settle instead — matches bosminer i2c-0 strace)"
        );
        std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL");
    }

    // =======================================================================
    //  — strace-derived 4-byte GET_VERSION response parser pins
    // =======================================================================

    #[test]
    fn wave28b_parse_recognizes_strace_4byte_response_with_fw_at_index_1() {
        // Real strace evidence from .25: bosminer-plus-tuner 0.9.0 reads
        // `[0x17, 0x89, 0x00, 0xA5]` after `[55 AA 04 17 00 1B]` write.
        // fw=0x89 must be parsed from INDEX 1, not index 2.
        let strace_reply = [0x17u8, 0x89, 0x00, 0xA5];
        assert_eq!(parse_hybrid_pic_fw_reply(&strace_reply), Some(0x89));
    }

    #[test]
    fn wave28b_parse_handles_all_known_fw_bytes_in_4byte_shape() {
        // All four known fw bytes must parse correctly from index 1
        // (0x82 bare BE-mV, 0x86 corrupted state, 0x89 framed-DAC,
        // 0x8A future-RE'd variant).
        for fw in [0x82u8, 0x86, 0x89, 0x8A] {
            let reply = [0x17u8, fw, 0x00, 0x00]; // checksum byte irrelevant
            assert_eq!(
                parse_hybrid_pic_fw_reply(&reply),
                Some(fw),
                "fw 0x{:02X} must parse from index 1 in 4-byte response shape",
                fw
            );
        }
    }

    #[test]
    fn daemon_side_fw86_refuses_voltage_without_lab_override() {
        assert!(
            !detected_dspic_fw_allows_voltage_commands(0x86, false),
            "daemon cold-boot path must refuse voltage commands to dsPIC fw=0x86"
        );
        assert!(
            detected_dspic_fw_allows_voltage_commands(0x86, true),
            "the documented lab override is the only fw=0x86 voltage-command bypass"
        );
        assert!(detected_dspic_fw_allows_voltage_commands(0x89, false));
        assert!(detected_dspic_fw_allows_voltage_commands(0x8A, false));
    }

    #[test]
    fn wave28b_parse_does_not_false_positive_on_older_3byte_shape() {
        // Older bare-firmware response `[0x17, 0x00, FW]` puts fw at
        // INDEX 2 with status 0x00 at index 1. The new 4-byte branch
        // must NOT consume this — it requires buf[1] to be a known fw
        // byte (not 0x00). With buf[1] == 0x00, `is_known_pic_fw(0x00)`
        // is false → new branch skipped → fall through to existing
        // 3-byte branch that returns Some(fw) from buf[2].
        let older_reply = [0x17u8, 0x00, 0x89, 0xFF, 0xFF];
        assert_eq!(parse_hybrid_pic_fw_reply(&older_reply), Some(0x89));
        // Belt-and-suspenders: the same 3-byte prefix alone.
        let older_prefix = [0x17u8, 0x00, 0x89];
        assert_eq!(parse_hybrid_pic_fw_reply(&older_prefix), Some(0x89));
    }

    #[test]
    fn wave28b_parse_does_not_false_positive_on_vnish_5byte_shape() {
        // VNish-RE'd `[0x05, 0x17, FW, ?, checksum]` (LEN=5, opcode echo,
        // fw at index 2). The new 4-byte branch requires buf[0] == 0x17;
        // VNish has buf[0] == 0x05 → new branch skipped → fall through to
        // existing VNish branch that returns Some(fw) from buf[2].
        let vnish_reply = [0x05u8, 0x17, 0x89, 0x00, 0xA5];
        assert_eq!(parse_hybrid_pic_fw_reply(&vnish_reply), Some(0x89));
    }

    #[test]
    fn wave28b_parse_rejects_strace_shape_with_garbage_status_byte() {
        // The strace evidence shows status byte is ALWAYS 0x00. If the
        // dsPIC ever returns a different status byte (corrupted reply,
        // bus noise) we must reject the 4-byte branch and fall through
        // to the older patterns (which will likely also reject and
        // return None). This guards against random buf[1]==knownfw
        // patterns that aren't actually the strace shape.
        let bad_status_reply = [0x17u8, 0x89, 0xFF, 0xA5];
        // Falls through to older patterns: `[0x17, 0xFF, ...]`:
        //   - 3-byte branch needs buf[1]==0x00 → fails
        //   - 3-byte fallback needs is_known_pic_fw(buf[2]==0xFF) → fails
        // Then `is_known_pic_fw(buf[0]==0x17)` → fails. Returns None.
        assert_eq!(parse_hybrid_pic_fw_reply(&bad_status_reply), None);
    }

    #[test]
    fn wave28b_parse_rejects_strace_shape_with_garbage_fw_byte() {
        // buf[1] must be a known fw byte; otherwise reject.
        let bad_fw_reply = [0x17u8, 0x77, 0x00, 0xFF];
        // Falls through to older patterns; `is_known_pic_fw(0x00)` is
        // false at index 2 → None.
        assert_eq!(parse_hybrid_pic_fw_reply(&bad_fw_reply), None);
    }

    #[test]
    fn wave28b_parse_handles_1_byte_read_without_false_4byte_match() {
        // When the env gate is OFF (default), the read is still 1 byte
        // → buf = [0x17]. The new 4-byte branch requires buf.len() >= 4
        // → skipped. The 3-byte branches also skip. Then
        // `is_known_pic_fw(0x17)` → false. Returns None — exactly the
        // pre- behaviour. Default-off byte-identical guarantee.
        let one_byte_opcode_echo = [0x17u8];
        assert_eq!(parse_hybrid_pic_fw_reply(&one_byte_opcode_echo), None);
    }

    #[test]
    fn parse_ablation_fields_tolerates_missing_or_error_summaries() {
        // read_error / send_error strings and empty input must not panic and
        // must degrade to zeros + "none" — instrumentation never bails.
        let (t, u, d, p) = parse_ablation_fields("read_error=timeout");
        assert_eq!((t, u, d), (0, 0, 0));
        assert_eq!(p, "none");

        let (t, u, d, p) = parse_ablation_fields("");
        assert_eq!((t, u, d), (0, 0, 0));
        assert_eq!(p, "none");

        // No chip replies path: chip_preview=none is emitted by the summarizer.
        let none_summary = "count=0 chipid_like=0 unique_chip_addrs=0 duplicate_chipid=0 \
                            nonce_like=0 other=0 malformed=0 chip_preview=none first=none";
        let (t, u, d, p) = parse_ablation_fields(none_summary);
        assert_eq!((t, u, d), (0, 0, 0));
        assert_eq!(p, "none");
    }

    // ================================================================
    //  am2/BM1362 FREQUENCY-ONLY autotuner — host-safe unit tests.
    //
    // Pure-logic coverage of the load-bearing invariants. (No HAL /
    // hardware — these run wherever the rest of the dcentrald suite
    // runs.)
    // ================================================================

    #[test]
    fn freq_only_default_off_is_byte_identical_gate() {
        // DEFAULT-OFF is THE load-bearing invariant: with neither the
        // TOML key nor the env var set, the gate is closed → the
        // serial-dispatch loop never builds the rig (no freq_cmd_rx, no
        // chain_stats, no tuner spawn) → byte-identical to the proven
        // `a lab unit`/.109 path. We assert the resolver returns false on a
        // clean minimal config (env not set in the test process).
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        let cfg = min_config();
        assert!(
            !cfg.autotuner.am2_frequency_autotune,
            "am2_frequency_autotune MUST default to false"
        );
        assert!(
            !am2_frequency_autotune_opted_in(&cfg),
            "gate MUST be closed by default — zero `a lab unit` behavior change unless opted in"
        );
    }

    #[test]
    fn freq_only_opt_in_via_config_key() {
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        let mut cfg = min_config();
        cfg.autotuner.am2_frequency_autotune = true;
        assert!(
            am2_frequency_autotune_opted_in(&cfg),
            "[autotuner] am2_frequency_autotune = true must open the gate"
        );
    }

    #[test]
    fn applied_pll_band_is_proven_table_intersection_400_545() {
        // The W1-C config band is [245,545], but the only PLL register
        // words this serial path has ever driven on real silicon are
        // BM1362_PLL_TABLE (400..=597). On a live home unit we NEVER
        // apply a PLL register the path hasn't proven, and NEVER above
        // nameplate 545. So every applied frequency is clamped to
        // [400,545].
        assert_eq!(AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ, 400);
        assert_eq!(AM2_FREQ_ONLY_APPLIED_CEIL_MHZ, 545);
        // Below proven floor → clamps UP to 400 (never an unproven
        // sub-400 PLL word).
        assert_eq!(am2_freq_only_clamp_applied_mhz(245), 400);
        assert_eq!(am2_freq_only_clamp_applied_mhz(320), 400);
        assert_eq!(am2_freq_only_clamp_applied_mhz(399), 400);
        // In-band passes through.
        assert_eq!(am2_freq_only_clamp_applied_mhz(400), 400);
        assert_eq!(am2_freq_only_clamp_applied_mhz(500), 500);
        assert_eq!(am2_freq_only_clamp_applied_mhz(545), 545);
        // Above nameplate → clamps DOWN to 545 (no home overclock).
        assert_eq!(am2_freq_only_clamp_applied_mhz(546), 545);
        assert_eq!(am2_freq_only_clamp_applied_mhz(597), 545);
        assert_eq!(am2_freq_only_clamp_applied_mhz(900), 545);
    }

    #[test]
    fn built_config_hard_pins_voltage_and_dvfs_off_and_clamps_band() {
        // The single load-bearing transform: even if an operator tried
        // to enable voltage_optimization / dvfs / a wild band in TOML,
        // the freq-only builder force-disables voltage+DVFS and clamps
        // the *config* band to the home-safe [245,545] window. NO live
        // am2 voltage this wave.
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        let mut cfg = min_config();
        cfg.autotuner.am2_frequency_autotune = true;
        cfg.autotuner.voltage_optimization = true;
        cfg.autotuner.dvfs_enabled = true;
        cfg.autotuner.min_freq_mhz = 100;
        cfg.autotuner.max_freq_mhz = 900;

        let built = build_am2_freq_only_autotuner_config(&cfg);
        assert!(
            !built.voltage_optimization,
            "voltage_optimization MUST be hard-pinned false (NO live am2 voltage this wave)"
        );
        assert!(
            !built.dvfs_enabled,
            "dvfs_enabled MUST be hard-pinned false (DVFS implies voltage)"
        );
        assert_eq!(
            built.min_freq_mhz, 245,
            "config band floor clamps UP to 245"
        );
        assert_eq!(
            built.max_freq_mhz, 545,
            "config band ceiling clamps DOWN to 545 — no above-nameplate exploration"
        );
        assert!(built.enabled, "opt-in IS the enable for this path");
        assert!(
            built.validate().is_ok(),
            "pinned config must still validate"
        );
    }

    #[test]
    fn quiet_home_efficiency_is_the_default_objective_for_dot25() {
        // `a lab unit` is a home unit ("aim for a home friendly profile").
        // The QUIET profile (`Efficiency` — walk frequency DOWN toward
        // the J/TH minimum) is the DEFAULT objective when autotune is
        // opted in. Standard/home/heater all resolve to Efficiency;
        // Performance/Hashrate is never the default on a home unit.
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        for mode in ["standard", "mining", "home", "heater"] {
            let mut cfg = min_config();
            cfg.autotuner.am2_frequency_autotune = true;
            cfg.mode.active = mode.to_string();
            let built = build_am2_freq_only_autotuner_config(&cfg);
            assert_eq!(
                built.target_mode,
                dcentrald_autotuner::config::TuneTarget::Efficiency,
                "mode {:?} on a home unit MUST default to the QUIET Efficiency objective (J/TH walk-down), never Performance",
                mode
            );
        }
    }

    #[test]
    fn explicit_operator_hashrate_target_is_preserved_not_silently_quieted() {
        // Opting OUT of the quiet default is the operator's explicit
        // call and must never be silently overridden — mirrors the
        // daemon W1.3 behavior. An explicit non-Efficiency target_mode
        // survives the builder.
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        let mut cfg = min_config();
        cfg.autotuner.am2_frequency_autotune = true;
        cfg.autotuner.target_mode = dcentrald_autotuner::config::TuneTarget::Hashrate;
        cfg.mode.active = "standard".to_string();
        let built = build_am2_freq_only_autotuner_config(&cfg);
        assert_eq!(
            built.target_mode,
            dcentrald_autotuner::config::TuneTarget::Hashrate,
            "an explicit operator target_mode must be preserved (opting out of quiet is explicit, never silent)"
        );
        // ...but voltage is STILL hard-off even under Hashrate.
        assert!(!built.voltage_optimization);
        assert!(!built.dvfs_enabled);
    }

    #[test]
    fn hacker_mode_opts_back_into_hashrate_but_voltage_stays_off() {
        std::env::remove_var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV);
        let mut cfg = min_config();
        cfg.autotuner.am2_frequency_autotune = true;
        cfg.mode.active = "hacker".to_string();
        let built = build_am2_freq_only_autotuner_config(&cfg);
        assert_eq!(
            built.target_mode,
            dcentrald_autotuner::config::TuneTarget::Hashrate,
            "hacker mode opts back into Hashrate (raw-register users asked for the leaderboard)"
        );
        assert!(
            !built.voltage_optimization && !built.dvfs_enabled,
            "voltage/DVFS HARD-off regardless of objective — no live am2 voltage this wave"
        );
    }

    #[test]
    fn chain_stats_snapshot_is_chip_count_aware_and_resets_window() {
        // The autotuner's expected-NPS math is per-chip × chip_count
        // (894 nonce-attribution slots per BM1362 chip, W6.8). The
        // serial path's chain-level snapshot must carry the LIVE
        // enumerated chip count (28..110 on XIL) so the prediction
        // scales with the real chain — chip_temps_c length proves the
        // count is propagated; aggregate nonce/error counts feed the
        // single synthetic chain; the window resets after each
        // snapshot.
        let mut cs = Am2SerialChainStats::new(0);
        for _ in 0..50 {
            cs.record_nonce();
        }
        for _ in 0..3 {
            cs.record_error();
        }
        cs.set_temp(Some(52.0));

        let snap_28 = cs.take_snapshot(28);
        assert_eq!(snap_28.chain_id, 0);
        assert_eq!(snap_28.chip_nonces, vec![50]);
        assert_eq!(snap_28.chip_errors, vec![3]);
        assert_eq!(snap_28.measurement_epoch, 1);
        assert_eq!(
            snap_28.chip_temps_c.as_ref().map(|v| v.len()),
            Some(28),
            "chip-count-aware: temp vec length must equal the LIVE enumerated chip count"
        );

        // Window reset: a fresh snapshot at a DIFFERENT chip count
        // (mid-run change, e.g. AC-cycle yield) carries the new count
        // and zeroed aggregates.
        let snap_110 = cs.take_snapshot(110);
        assert_eq!(snap_110.chip_nonces, vec![0], "window reset after snapshot");
        assert_eq!(snap_110.chip_errors, vec![0]);
        assert_eq!(
            snap_110.measurement_epoch, 2,
            "epoch increments per snapshot"
        );
        assert_eq!(
            snap_110.chip_temps_c.as_ref().map(|v| v.len()),
            Some(110),
            "chip-count change is reflected in the next snapshot (recompute expected NPS)"
        );

        // Expected-NPS scales linearly with the enumerated chip count
        // (894 slots/chip, the W6.8 BM1362 geometry) — proves the
        // chain estimate is chip-count-aware, not fixed-126.
        let per_chip = dcentrald_autotuner::chip_geometry::expected_nps_for_chip(0x1362, 500, 256);
        assert!(per_chip > 0.0);
        let nps_28 = per_chip * 28.0;
        let nps_110 = per_chip * 110.0;
        assert!(
            (nps_110 / nps_28 - 110.0 / 28.0).abs() < 1e-9,
            "expected NPS must scale linearly with the live enumerated chip count"
        );
    }

    // -----------------------------------------------------------------------
    // R1 — am2 low-idle PWM command clamp (compute_quiet_idle_pwm)
    //
    // -----------------------------------------------------------------------

    /// The low-idle command setpoint defaults to 10 when the ceiling and
    /// safety max are above it (the `a lab unit` home path: idle 10, cap 30). Physical
    /// quiet is not implied; RPM/operator feedback is the proof.
    #[test]
    fn quiet_idle_pwm_default_home_path() {
        assert_eq!(
            compute_quiet_idle_pwm(10, 30),
            10,
            "idle 10 with cap 30 must resolve to 10 (the .25 home low-PWM path)"
        );
    }

    /// PWM is only ever driven DOWN: if the configured idle exceeds the fan
    /// ceiling, it clamps to the ceiling — never up.
    #[test]
    fn quiet_idle_pwm_clamps_down_to_fan_max() {
        assert_eq!(
            compute_quiet_idle_pwm(25, 20),
            20,
            "idle 25 with cap 20 must clamp DOWN to 20"
        );
        assert_eq!(compute_quiet_idle_pwm(100, 15), 15);
    }

    /// `PWM_SAFETY_MAX` (30) is an enforced upper bound on EVERY path — even
    /// if both the idle and the ceiling somehow exceeded it.
    #[test]
    fn quiet_idle_pwm_never_exceeds_safety_max() {
        // Even a pathological idle=255, fan_max=255 cannot exceed 30.
        assert_eq!(
            compute_quiet_idle_pwm(255, 255),
            dcentrald_hal::fan::PWM_SAFETY_MAX,
            "the absolute home safety cap (30) must hold on every path"
        );
        assert_eq!(compute_quiet_idle_pwm(50, 80), 30);
        // And it never exceeds the IP ceiling (100) either.
        assert!(compute_quiet_idle_pwm(255, 255) <= dcentrald_hal::fan::PWM_MAX);
    }

    /// A 0 setpoint is honored (off) — the clamp only bounds the TOP, never
    /// raises a low value.
    #[test]
    fn quiet_idle_pwm_zero_is_preserved() {
        assert_eq!(compute_quiet_idle_pwm(0, 30), 0);
        assert_eq!(compute_quiet_idle_pwm(0, 0), 0);
    }

    // -----------------------------------------------------------------------
    // RE-018 byte-exact bosminer cold-wake decode (2026-05-31).
    //
    // Pins the register VALUES decoded from `/data/re018-cold-strace.log`. If
    // any of these drift, the `a lab unit` standalone cold-wake replay is no longer
    // byte-exact to bosminer and the test fails loudly.
    // -----------------------------------------------------------------------
    #[test]
    fn re018_decoded_register_values_are_byte_exact() {
        // Broadcast pre-config.
        assert_eq!(RE018_BCAST_A8, 0x0000_0000, "broadcast 0xA8");
        assert_eq!(
            RE018_MISC_CTRL, 0xB000_C100,
            "MiscCtrl 0x18 (pre + per-chip + post)"
        );
        assert_eq!(RE018_VERSION_MASK, 0x9000_FFFF, "version mask 0xA4");
        // Broadcast core/analog/io block.
        assert_eq!(RE018_CORE_3C_HASH_CLK, 0x8000_8540, "core 0x3C hash-clk");
        assert_eq!(RE018_CORE_3C_CLK_DELAY, 0x8000_8008, "core 0x3C clk-delay");
        assert_eq!(
            RE018_CORE_3C_FAMILY, 0x8000_82AA,
            "core 0x3C family (per-chip only)"
        );
        assert_eq!(RE018_ANALOG_MUX_54, 0x0000_0003, "analog-mux 0x54");
        assert_eq!(RE018_IO_DRIVER_58, 0x0001_1111, "io-driver 0x58");
        // PLL block.
        assert_eq!(RE018_PLL0_DIVIDER_70, 0x0000_0000, "PLL0 divider 0x70");
        assert_eq!(RE018_PLL_08, 0x40A8_0265, "PLL 0x08 (525 MHz)");
        assert_eq!(
            RE018_FAST_UART_28, 0x1130_0000,
            "FastUART 0x28 (byte-order-correct)"
        );
        // Per-chip.
        assert_eq!(RE018_PER_CHIP_A8, 0x0000_0002, "per-chip 0xA8");
        assert_eq!(RE018_NONCE_SPACE_REG, 0x0C, "per-chip nonce-space register");
        assert_eq!(RE018_NONCE_SPACE_BASE, 0x8000_0000, "nonce-space base");
        assert_eq!(
            RE018_NONCE_SPACE_STRIDE, 0x0000_0104,
            "nonce-space stride per addr"
        );
    }

    // -----------------------------------------------------------------------
    // Regression pins for the 2026-06-14 `a lab unit` hashrate fixes (perf audit).
    // If any drift, the gentle-bump (A8 core-enable) or the ticket-mask cleanup
    // silently changes — these were live-diagnosed and must not regress.
    // -----------------------------------------------------------------------
    #[test]
    fn re018_hashrate_fix_constants_are_byte_exact() {
        // TICKET_MASK (0x14) + HASH_COUNTING (0x10): the bosminer-strace-omitted
        // diff-256 hardware filter + 126-chip nonce range. Live-proven (flood
        // collapse 270x: 709/s -> 2.6/s) — a cleanup, NOT the hashrate root cause.
        assert_eq!(TICKET_MASK_256, 0x0000_00FF, "ticket mask 0x14 (diff-256)");
        assert_eq!(
            NONCE_RANGE_126, 0x0000_1381,
            "hash-counting 0x10 (126-chip range)"
        );
        // A8 core-ENABLE (.79 accepted-share-proven). The RE018 strace's 0x0/0x2
        // left the SHA cores IDLE; the 0x0007 high bits are the core/clock enable.
        assert_eq!(
            RE018_FULL_CORE_BCAST_A8, INIT_CONTROL_BCAST,
            "full-core broadcast A8 must equal the .79-proven INIT_CONTROL_BCAST"
        );
        assert_eq!(
            RE018_FULL_CORE_BCAST_A8, 0x0007_0000,
            "full-core broadcast A8"
        );
        assert_eq!(
            RE018_FULL_CORE_PER_CHIP_A8, INIT_CONTROL_PER_CHIP,
            "full-core per-chip A8 must equal the .79-proven INIT_CONTROL_PER_CHIP"
        );
        assert_eq!(
            RE018_FULL_CORE_PER_CHIP_A8, 0x0007_01F0,
            "full-core per-chip A8"
        );
        // option-B (2026-06-14): the freq-override DEFAULT is now the PROVEN 320 MHz sweet spot
        // (table ÷10 byte order, 0x50800141), NOT the live-rejected off-table 150 MHz 0x50D2_0164.
        // It must (a) be that proven value, (b) match bm1362::pll_lookup_extended(320), and (c) stay
        // DISTINCT from the ~50 MHz eco default RE018_PLL_08 (de-couple sentinel — never the off-table value).
        assert_eq!(
            RE018_LOW_FREQ_PLL_08, 0x5080_0141,
            "freq-override default = proven 320 MHz"
        );
        assert_eq!(RE018_LOW_FREQ_MHZ, 320, "freq-override default target MHz");
        assert_eq!(
            RE018_LOW_FREQ_PLL_08,
            dcentrald_asic::drivers::bm1362::pll_lookup_extended(320).0,
            "override default must equal the proven bm1362 320 MHz encoding"
        );
        assert_ne!(
            RE018_LOW_FREQ_PLL_08, RE018_PLL_08,
            "freq-override PLL must differ from the ~50 MHz eco default (de-couple sentinel)"
        );
        assert_ne!(
            RE018_LOW_FREQ_PLL_08, 0x50D2_0164,
            "must never be the live-rejected off-table 150 MHz value"
        );
    }

    /// The per-chip nonce-space base (reg 0x0C) value bosminer writes per chip,
    /// from the trace: chip addr 0x00 -> 0x80000000, 0x02 -> 0x80000208,
    /// 0x04 -> 0x80000410. value = 0x80000000 | (addr * 0x104).
    #[test]
    fn re018_nonce_space_base_matches_traced_values() {
        let f = |addr: u8| -> u32 {
            RE018_NONCE_SPACE_BASE
                .wrapping_add((addr as u32).wrapping_mul(RE018_NONCE_SPACE_STRIDE))
        };
        assert_eq!(f(0x00), 0x8000_0000, "chip 0x00");
        assert_eq!(f(0x02), 0x8000_0208, "chip 0x02 (trace value)");
        assert_eq!(f(0x04), 0x8000_0410, "chip 0x04 (trace value)");
        assert_eq!(
            f(0xFA),
            0x8000_0000u32.wrapping_add(0xFAu32 * 0x104),
            "chip 0xFA (last @126/stride-2)"
        );
    }

    /// The RE-018 cold-wake gate must be DOUBLE-gated: the env flag AND the
    /// `a lab unit` fingerprint. With the env unset the path is unreachable regardless
    /// of platform (byte-identical to today everywhere else). We can only
    /// assert the env-half here (fingerprint reads /etc files absent in CI),
    /// but the unset case is the load-bearing default-OFF guarantee.
    #[test]
    fn re018_gate_is_off_by_default() {
        // Env unset in the test environment -> gate must be false.
        std::env::remove_var("DCENT_AM2_BM1362_RE018_COLD_SEQUENCE");
        assert!(
            !am2_bm1362_re018_cold_sequence_enabled(),
            "RE-018 cold-wake path must be OFF by default (env unset)"
        );
    }

    // ----------------------------------------------------------------------
    // MINE-LIFE-1 / MINE-LIFE-2 / THERM-1 (desk-only ship-path hardening)
    // ----------------------------------------------------------------------

    fn dur(secs: u64) -> std::time::Duration {
        std::time::Duration::from_secs(secs)
    }

    /// MINE-LIFE-1: the GENEROUS mid-run stall timeout derivation + env override.
    /// This is the ONLY test that touches `ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S`,
    /// so it owns that name within the suite.
    #[test]
    fn am2_mid_run_stall_timeout_default_override_and_disable() {
        std::env::remove_var(ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S);

        // Pin the conservative floor so a future edit can't make it un-generous.
        assert_eq!(AM2_MID_RUN_STALL_MIN_DEFAULT_S, 300);

        // Default derived from the 90 s no-nonce default ⇒ max(2×90, 300) = 300 s,
        // i.e. ALWAYS ≥ the startup no-nonce timeout and ≥ the 5-minute floor.
        let derived = am2_mid_run_nonce_stall_timeout(90).expect("armed for a non-zero base");
        assert_eq!(derived, dur(300));
        assert!(
            derived >= dur(90),
            "mid-run timeout must never be shorter than the startup no-nonce timeout"
        );

        // A large base scales (2×) above the floor.
        assert_eq!(am2_mid_run_nonce_stall_timeout(200), Some(dur(400)));

        // Base 0 (startup guard disabled for deep lab capture) ⇒ guard off.
        assert_eq!(am2_mid_run_nonce_stall_timeout(0), None);

        // Explicit env override wins (still generous in practice; the operator owns it).
        std::env::set_var(ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S, "600");
        assert_eq!(am2_mid_run_nonce_stall_timeout(90), Some(dur(600)));
        // Override of 0 disables even when the base would arm.
        std::env::set_var(ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S, "0");
        assert_eq!(am2_mid_run_nonce_stall_timeout(90), None);

        std::env::remove_var(ENV_AM2_MID_RUN_NONCE_STALL_TIMEOUT_S);
    }

    /// MINE-LIFE-1: the stall detector fires ONLY after the generous timeout, and
    /// NEVER on a healthy nonce cadence or before the first nonce.
    #[test]
    fn am2_mid_run_stall_fires_only_after_generous_timeout() {
        let timeout = Some(dur(300));

        // No nonce ever ⇒ NOT a mid-run stall (the startup no-nonce guard owns it).
        assert!(!am2_mid_run_nonce_stalled(timeout, None));

        // Healthy cadence: last nonce a few seconds ago ⇒ never trips.
        assert!(!am2_mid_run_nonce_stalled(timeout, Some(dur(5))));
        // Even a sparse-but-alive eco unit well under the window stays healthy.
        assert!(!am2_mid_run_nonce_stalled(timeout, Some(dur(299))));

        // At/after the generous timeout ⇒ fail closed.
        assert!(am2_mid_run_nonce_stalled(timeout, Some(dur(300))));
        assert!(am2_mid_run_nonce_stalled(timeout, Some(dur(900))));

        // Guard disabled ⇒ never trips regardless of elapsed.
        assert!(!am2_mid_run_nonce_stalled(None, Some(dur(99999))));
    }

    /// MINE-LIFE-2: recent-activity predicate is the inverse of "stalled" and is
    /// conservative (true) when the guard is off or no nonce has arrived yet.
    #[test]
    fn am2_nonce_recently_active_is_conservative() {
        let timeout = Some(dur(300));
        assert!(am2_nonce_recently_active(timeout, Some(dur(5))));
        assert!(!am2_nonce_recently_active(timeout, Some(dur(300))));
        // No nonce yet, or guard disabled ⇒ never falsely "stalled" (returns true).
        assert!(am2_nonce_recently_active(timeout, None));
        assert!(am2_nonce_recently_active(None, Some(dur(99999))));
    }

    /// MINE-LIFE-2: the recent-window "current" hashrate differs from the
    /// cumulative latch — it drops to 0 once activity stops, instead of keeping a
    /// healthy lifetime average.
    #[test]
    fn am2_recent_window_hashrate_drops_when_activity_stops() {
        let acct = Am2ShareAccounting::default();
        acct.record_accepted(256.0, 256.0); // achieved-difficulty sum = 256

        // Window that captured the share (prev sum 0 → delta 256) reads > 0.
        let active = acct.hashrate_ghs_window(0.0, 5.0);
        assert!(
            active > 0.0,
            "an active window must read a positive hashrate"
        );

        // Next window with NO new shares (prev sum == current sum) reads 0 — the
        // honest "current" value — even though the cumulative achieved sum (256)
        // would still yield a positive lifetime average.
        let stalled = acct.hashrate_ghs_window(256.0, 5.0);
        assert_eq!(stalled, 0.0, "a window with no new shares must read 0 GH/s");
        assert!(
            stalled < active,
            "recent-window current must fall below the active reading when activity stops"
        );

        // Non-positive window is a safe 0 (no divide-by-zero / negative).
        assert_eq!(acct.hashrate_ghs_window(0.0, 0.0), 0.0);
        // A negative delta (counter reset) clamps to 0, never negative.
        assert_eq!(acct.hashrate_ghs_window(1_000.0, 5.0), 0.0);
    }

    /// FIX 2 / MINE-LIFE-2 (eco-stable): the rolling-window "current" hashrate
    /// stays a STABLE non-zero value across a realistic sparse-share eco cadence
    /// (a healthy ~1 TH/s @ ~50 MHz unit lands a share only every ~30 s) instead
    /// of flickering to 0 on the empty 5 s ticks BETWEEN shares — and still
    /// decays to ~0 only after a genuine SUSTAINED stall (no new share for the
    /// whole horizon). The baseline selection + the existing
    /// `hashrate_ghs_window` delta math together remain a REAL measured value,
    /// never the cumulative-since-boot average.
    #[test]
    fn am2_rolling_window_hashrate_stable_on_sparse_eco_cadence() {
        let horizon = AM2_RECENT_HASHRATE_WINDOW_S; // 60 s
        assert_eq!(horizon, 60.0);

        // Current achieved-difficulty sum after two accepted shares within the
        // last minute (the eco cadence), nothing since.
        let acct = Am2ShareAccounting::default();
        acct.record_accepted(256.0, 256.0);
        acct.record_accepted(256.0, 256.0); // achieved sum = 512

        // Prior-tick history (oldest first), 5 s ticks over the last minute. The
        // achieved sum was 0 until the first share (~age 55 s), 256 until the
        // second (~age 25 s), and 512 since.
        let history: Vec<(f64, f64)> = (1..=12)
            .rev()
            .map(|k| {
                let age = (k * 5) as f64; // 60,55,...,5
                let sum = if age >= 55.0 {
                    0.0
                } else if age >= 25.0 {
                    256.0
                } else {
                    512.0
                };
                (age, sum)
            })
            .collect();

        // Baseline spans the full 60 s horizon (newest snapshot >= 60 s old).
        let (span, base) = am2_rolling_window_baseline(&history, horizon).unwrap();
        assert_eq!((span, base), (60.0, 0.0));
        // End-to-end: a sparse-but-ALIVE cadence reads a STABLE non-zero value
        // (the old single-tick 5 s window would have read 0 on this tick).
        let hr = acct.hashrate_ghs_window(base, span);
        assert!(
            hr > 0.0,
            "sparse-but-alive eco cadence must read non-zero, got {hr}"
        );

        // One empty 5 s tick later (ages +5, NO new share) must NOT flicker to 0 —
        // the wide window still contains the recent shares.
        let next: Vec<(f64, f64)> = history.iter().map(|&(a, s)| (a + 5.0, s)).collect();
        let (span_n, base_n) = am2_rolling_window_baseline(&next, horizon).unwrap();
        assert!(
            acct.hashrate_ghs_window(base_n, span_n) > 0.0,
            "one empty tick must not flicker the current tile to 0"
        );

        // Genuine SUSTAINED stall: no new share for longer than the whole horizon
        // ⇒ every in-window baseline already sits at the current sum ⇒ delta 0.
        let stalled: Vec<(f64, f64)> = (1..=12).rev().map(|k| ((k * 5) as f64, 512.0)).collect();
        let (span_s, base_s) = am2_rolling_window_baseline(&stalled, horizon).unwrap();
        assert_eq!(
            acct.hashrate_ghs_window(base_s, span_s),
            0.0,
            "a sustained stall must decay the current tile to 0"
        );

        // First tick (no prior history) ⇒ None, so the caller shows the
        // cumulative figure exactly once.
        assert_eq!(am2_rolling_window_baseline(&[], horizon), None);

        // Before a full horizon of history accrues, the baseline is the OLDEST
        // retained snapshot (a real, non-zero span — not a flicker-0 zero-span):
        // e.g. with only 15 s of history.
        let young: Vec<(f64, f64)> = vec![(15.0, 0.0), (10.0, 0.0), (5.0, 256.0)];
        assert_eq!(
            am2_rolling_window_baseline(&young, horizon),
            Some((15.0, 0.0)),
            "before the horizon fills, use the oldest snapshot, not a 0-span"
        );
    }

    /// THERM-1(a) / FIX 1: the 3-strike fan-fault decision trips only on a
    /// sustained confident "commanded PWM>0 but all fans 0 RPM" observed WHILE
    /// the unit is at/above the hot threshold, and resets on any airflow, zero
    /// command, inconclusive read, or a cool / non-finite temperature.
    #[test]
    fn am2_fan_fault_step_requires_sustained_confident_zero_rpm() {
        assert_eq!(AM2_FAN_FAULT_STRIKES, 3);
        let hot = 65.0_f32; // hot threshold
        let cool = 49.3_f32; // the .25 eco steady-state die temp (proven path)
        let warm = 70.0_f32; // above hot (a genuinely overheating unit)

        // Healthy: airflow present ⇒ never strikes (the .25 eco PWM10/~2880 RPM
        // case), even at a warm temperature that would otherwise be eligible.
        let (s, f) = am2_fan_fault_step(0, Some((2880, 10)), warm, hot);
        assert_eq!((s, f), (0, false));

        // FIX 1 (false-cut guard): commanded but 0 RPM while COOL is NOT a fault.
        // A confirmed-cool unit is cooling adequately regardless of the tach, so
        // it never strikes — this is the healthy sister am2-zynq / unverified-tach
        // case the fix protects.
        let (s, f) = am2_fan_fault_step(0, Some((0, 10)), cool, hot);
        assert_eq!((s, f), (0, false));
        // ...and a cool read RESETS an in-progress streak (never accumulates).
        let (s, f) = am2_fan_fault_step(2, Some((0, 10)), cool, hot);
        assert_eq!((s, f), (0, false));

        // Commanded but 0 RPM WHILE HOT accumulates strikes; trips on the 3rd.
        let (s, f) = am2_fan_fault_step(0, Some((0, 10)), warm, hot);
        assert_eq!((s, f), (1, false));
        let (s, f) = am2_fan_fault_step(s, Some((0, 10)), warm, hot);
        assert_eq!((s, f), (2, false));
        let (s, f) = am2_fan_fault_step(s, Some((0, 10)), warm, hot);
        assert_eq!(
            (s, f),
            (3, true),
            "fault must trip on the 3rd consecutive hot strike"
        );
        // Exactly AT the hot threshold also corroborates (>= hot).
        let (s, f) = am2_fan_fault_step(0, Some((0, 10)), hot, hot);
        assert_eq!((s, f), (1, false));

        // Zero command (fans not driven) is NOT a fault — resets even while hot.
        let (s, f) = am2_fan_fault_step(2, Some((0, 0)), warm, hot);
        assert_eq!((s, f), (0, false));

        // Inconclusive read (fan UIO unreadable) is NOT a fault — resets, never cuts.
        let (s, f) = am2_fan_fault_step(2, None, warm, hot);
        assert_eq!((s, f), (0, false));

        // Non-finite temperature is inconclusive corroboration — resets, never cuts.
        let (s, f) = am2_fan_fault_step(2, Some((0, 10)), f32::NAN, hot);
        assert_eq!((s, f), (0, false));

        // Recovered airflow mid-streak resets so it can't accumulate across blips.
        let (s, f) = am2_fan_fault_step(2, Some((1500, 10)), warm, hot);
        assert_eq!((s, f), (0, false));
    }

    /// THERM-1(b): the graded throttle reduces frequency in small steps at/above
    /// hot, never raises it, and is inert on the eco path / below the PLL floor.
    #[test]
    fn am2_graded_throttle_steps_down_only_above_hot() {
        let floor = AM2_FREQ_ONLY_APPLIED_FLOOR_MHZ; // 400
        let step = AM2_THERMAL_THROTTLE_STEP_MHZ; // 25
        let hot = 65.0_f32;

        // Below hot ⇒ no change (the proven path stays full-frequency).
        assert_eq!(
            am2_graded_throttle_target_mhz(525, 60.0, hot, floor, step),
            None
        );
        assert_eq!(
            am2_graded_throttle_target_mhz(525, 64.9, hot, floor, step),
            None
        );

        // At/above hot on a high-frequency unit ⇒ one small step DOWN, never up.
        let t = am2_graded_throttle_target_mhz(525, 65.0, hot, floor, step).unwrap();
        assert_eq!(t, 500);
        assert!(t < 525, "throttle must only ever reduce frequency");
        // Hotter still keeps stepping down toward the floor.
        assert_eq!(
            am2_graded_throttle_target_mhz(425, 75.0, hot, floor, step),
            Some(400)
        );
        // A step that would undershoot clamps to the floor (never below).
        assert_eq!(
            am2_graded_throttle_target_mhz(410, 75.0, hot, floor, step),
            Some(400)
        );

        // At/below the PLL floor ⇒ no PLL throttle. This is the .25 eco ~50 MHz
        // guard: a sub-floor chip is NEVER clocked UP toward the floor.
        assert_eq!(
            am2_graded_throttle_target_mhz(50, 90.0, hot, floor, step),
            None
        );
        assert_eq!(
            am2_graded_throttle_target_mhz(400, 90.0, hot, floor, step),
            None
        );

        // Non-finite temp ⇒ no change (never throttle on a bad reading).
        assert_eq!(
            am2_graded_throttle_target_mhz(525, f32::NAN, hot, floor, step),
            None
        );
    }

    fn min_hybrid_config() -> DcentraldConfig {
        toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#,
        )
        .expect("minimal hybrid test config must deserialize")
    }

    fn admitted_hybrid_route() -> crate::s19j_hybrid_admission::S19jHybridRouteAdmission {
        let identity = crate::daemon_lifecycle::PlatformIdentitySnapshot {
            declared_board_target: Some("am2-s19j".to_string()),
            board_desc: dcentrald_common::BoardDesc::lookup("am2-s19j"),
            declared_platform_marker: Some("zynq-bm3-am2".to_string()),
            declared_subtype: None,
            declared_psu_hardware_variant: None,
            observed_control_board: "Zynq am2-s17".to_string(),
        };
        crate::s19j_hybrid_admission::admit_s19j_hybrid_route(
            &identity,
            crate::RuntimeDispatchKind::S19jHybrid,
            Some(dcentrald_common::AsicProtocolIdentity::Bm1362),
        )
        .expect("canonical test composition must admit the hybrid route")
    }

    #[tokio::test]
    async fn hybrid_route_admission_is_consumed_at_first_run_entry() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        let mut miner =
            S19jHybridMiner::new(min_hybrid_config(), shutdown, admitted_hybrid_route()).unwrap();

        let first = miner.run().await.unwrap_err().to_string();
        assert!(first.contains("cancelled before hardware admission"));
        let second = miner.run().await.unwrap_err().to_string();
        assert!(second.contains("route admission was already consumed"));
    }

    /// MINE-LIFE-2: the per-chain `ChainState.status` reverts to "stalled" when a
    /// chain produced nonces earlier but has gone quiet, and reads "mining" only
    /// while recently active (byte-equivalent to the prior contract on a healthy
    /// unit).
    #[test]
    fn am2_chain_state_status_reverts_to_stalled_when_inactive() {
        let miner = S19jHybridMiner::new(
            min_hybrid_config(),
            tokio_util::sync::CancellationToken::new(),
            admitted_hybrid_route(),
        )
        .unwrap();

        // Produced nonces + recently active ⇒ "mining" (unchanged healthy path).
        let active = miner.build_am2_chain_state(0x20, 126, 1000.0, 5, 0, true);
        assert_eq!(active.status, "mining");

        // Produced nonces but no recent activity ⇒ "stalled", NOT a reassuring
        // "mining" off the cumulative unique-nonce latch.
        let stalled = miner.build_am2_chain_state(0x20, 126, 1000.0, 5, 0, false);
        assert_eq!(stalled.status, "stalled");

        // Never produced a nonce ⇒ "active" (pre-first-nonce), regardless of flag.
        let pre = miner.build_am2_chain_state(0x20, 126, 0.0, 0, 0, false);
        assert_eq!(pre.status, "active");
    }

    /// MINE-LIFE-2: the published pool `status` downgrades from "mining" to
    /// "stalled" when the unit has gone quiet, and stays "mining" while active —
    /// so the dashboard stops latching "mining" at a healthy hashrate on a stall.
    #[test]
    fn am2_publish_status_downgrades_to_stalled_on_inactivity() {
        let (tx, rx) = tokio::sync::watch::channel(dcentrald_api::MinerState::empty(
            dcentrald_api::OperatingMode::Standard,
        ));
        let miner = S19jHybridMiner::new(
            min_hybrid_config(),
            tokio_util::sync::CancellationToken::new(),
            admitted_hybrid_route(),
        )
        .unwrap()
        .with_state_tx(tx);

        let acct = Am2ShareAccounting::default();
        acct.record_accepted(256.0, 256.0); // accepted()>0 ⇒ base status "mining"
        let quality = dcentrald_stratum::pool_quality::PoolQualitySnapshot::default();

        // Recently active ⇒ "mining" (healthy path unchanged).
        miner.publish_miner_state(&acct, &quality, 1000.0, 1000.0, vec![], 10, true);
        assert_eq!(rx.borrow().pool.status, "mining");

        // Gone quiet ⇒ downgraded to "stalled" (no more reassuring "mining").
        miner.publish_miner_state(&acct, &quality, 1000.0, 0.0, vec![], 10, false);
        assert_eq!(rx.borrow().pool.status, "stalled");
    }
}
