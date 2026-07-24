//! S19j Pro serial mining — full ASIC init + pure UART work dispatch.
//!
//! Bosminer on S19j Pro sends ALL work via /dev/ttyS2 serial UART, NOT FPGA FIFOs.
//! Confirmed via strace: only /dev/ttyS2, /dev/i2c-0, and fan/board UIO are used.
//!
//! Work packet (88 bytes on wire):
//!   [55 AA] preamble
//!   [21]    header (TYPE_JOB | GROUP_SINGLE | CMD_WRITE)
//!   [36]    length byte (0x36 = 54, bosminer's encoding)
//!   [82 bytes] job payload (BM1366-format: job_id, num_midstates, nonce, nbits, ntime,
//!              merkle_root, prev_block_hash, version)
//!   [2 bytes] CRC-16
//!
//! I2C addresses (AM2 S19j Pro, selected from serial slot):
//!   0x20/0x21/0x22 = dsPIC voltage controllers (fw byte detected at runtime)
//!   0x51 = EEPROM (hashboard serial/calibration data)
//!
//! PIC protocol (serialized I2C service writes to 0x21):
//!   Flush: 16x write 0x00 (clear parser state)
//!   GET_VERSION: short [55 AA 17] or framed [55 AA 04 17 00 1B]
//!   ENABLE:      [55 AA 04 15 01 1A] -> voltage on
//!   HEARTBEAT:   [55 AA 04 16 00 1A] -> keep alive
//!   RESET/JUMP bootloader-control opcodes are banned on Pic0x89 paths.
//!
//! Full ASIC init sequence (14 steps):
//!   Opens serial at 115200, enumerates 126 chips, configures registers,
//!   upgrades baud to 3.125M, ramps PLL to target frequency.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use dcentrald_asic::drivers::{
    bm1368::{
        pll_ramp_sequence as bm1368_pll_ramp_sequence,
        FIXTURE_ADDRESS_INTERVAL as BM1368_ADDRESS_INTERVAL,
        FIXTURE_TICKET_MASK as BM1368_FIXTURE_TICKET_MASK,
        UART_RELAY_12_DOMAIN as BM1368_UART_RELAY_12_DOMAIN,
        UART_RELAY_REG as BM1368_UART_RELAY_REG,
    },
    MinerProfile,
};
use dcentrald_asic::dspic::{
    dspic_runtime_protocol_is_proven, DspicEndpointSession, DspicFirmware, DspicService,
    Pic0x89EndpointSession, Pic0x89Service,
};
use dcentrald_asic::uart_trans::{UartTransService, UartWork, DEFAULT_CHAIN_TTYS};
use dcentrald_hal::i2c::{
    spawn_i2c_service_no_register_touch_with_denylist, I2cMutationLabel, I2cServiceHandle,
    I2cTransactionStep,
};
use dcentrald_hal::platform::{FanAccess, FanCommandReceipt, Platform};
use dcentrald_hal::psu::Apw121215a;
use dcentrald_hal::psu_gpio_gate::PsuGpioGate;
use dcentrald_hal::serial_chain::SerialChainBackend;
use dcentrald_thermal::controller::{
    FanTachSafety, FanTachSafetyState, DEFAULT_FAN_BELOW_MINIMUM_FAILURE_TICKS,
};

use crate::config::DcentraldConfig;
use crate::history::{self, HistoryBuffer};
use crate::model;
use crate::runtime::safety_watchdog::{
    SafetyLiveness, SafetyWatchdogOwner, WatchdogCloseoutReceipt, WatchdogDisarmPermit,
    DEFAULT_WATCHDOG_STOP_TIMEOUT, DEFAULT_WATCHDOG_TEARDOWN_GRACE,
};
use crate::runtime::thread_guard::RuntimeThreadGuard;

// P1.2 fix (Audit C F-004): Bible/memory rule cap is ~50 work-frames/sec on
// serial work dispatch. The previous
// BM1362 values (1 ms × 128 burst = 128,000 frames/sec) violated by 2,560×
// and were the most likely root cause of the live `a lab unit` zero-nonce regime —
// FPGA WORK_TX FIFO saturated instantly with stale work the chips couldn't
// consume. New BM1362 values: 20 ms interval × 1 burst = 50 frames/sec, at
// the documented cap.
const BM1368_DISPATCH_INTERVAL_MS: u64 = 25;
const BM1362_DISPATCH_INTERVAL_MS: u64 = 20;
const DEFAULT_DISPATCH_INTERVAL_MS: u64 = 50;
const BM1368_MAX_WORK_ITEMS_PER_SEC: usize = 40;
const DEFAULT_MAX_WORK_ITEMS_PER_SEC: usize = 20;
const DEFAULT_SERIAL_WORK_QUEUE_DEPTH: usize = 16;
const BM1362_SERIAL_WORK_QUEUE_DEPTH: usize = 512;
const DEFAULT_SERIAL_TX_BURST: usize = 3;
const BM1362_SERIAL_TX_BURST: usize = 1;
const WORK_HISTORY_PER_ID: usize = dcentrald_common::DEFAULT_WORK_HISTORY_PER_ID;
const BM1398_WORK_HISTORY_PER_ID: usize = dcentrald_common::BM1398_WORK_HISTORY_PER_ID;
const AMLOGIC_TEMP_STARTUP_GRACE_S: u64 = 30;
const AMLOGIC_TEMP_MISS_LIMIT: u8 = 3;
const AMLOGIC_THERMAL_RESTART_DELAY_S: u64 = 60;
const AMLOGIC_FAN_SPINUP_ATTEMPTS: u32 = 3;
const AMLOGIC_FAN_SPINUP_RETRY_DELAY: Duration = Duration::from_millis(250);
const APW12_139_ASSUMED_FW: u8 = 0x71;
const RUNTIME_THREAD_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const NOPIC_WATCHDOG_BRINGUP_GRACE: Duration = Duration::from_secs(120);
const NOPIC_SAFETY_LIVENESS_INTERVAL: Duration = Duration::from_secs(2);
const HASHBOARD_EEPROM_WRITE_DENYLIST: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

#[derive(Debug)]
struct FanTachSnapshot {
    available: bool,
    expected_channels: usize,
    readings: Vec<(u8, u32)>,
}

impl FanTachSnapshot {
    fn rpms(&self) -> Vec<u32> {
        self.readings.iter().map(|(_, rpm)| *rpm).collect()
    }
}

async fn sample_fan_tach(fan: Arc<dyn FanAccess>) -> Result<FanTachSnapshot> {
    tokio::task::spawn_blocking(move || {
        let readings = fan.get_per_fan_rpm();
        FanTachSnapshot {
            // Read availability after sampling so a sampler error that revokes
            // the owner's evidence is part of this snapshot.
            available: fan.tach_available(),
            expected_channels: fan.fan_count() as usize,
            readings,
        }
    })
    .await
    .context("fan tach sampling worker did not complete")
}

async fn admit_fan_motion_at_pwm(
    fan: Arc<dyn FanAccess>,
    safety: &mut FanTachSafety,
    pwm: u8,
    stage: &'static str,
) -> Result<FanCommandReceipt> {
    let receipt = fan
        .set_speed_checked(pwm)
        .with_context(|| format!("Amlogic {stage} fan command/readback failed"))?;
    for attempt in 1..=AMLOGIC_FAN_SPINUP_ATTEMPTS {
        let snapshot = sample_fan_tach(fan.clone()).await?;
        let rpms = snapshot.rpms();
        let state = safety.observe_required_airflow(
            snapshot.available,
            receipt.observed_pwm(),
            snapshot.expected_channels,
            &rpms,
        );
        match state {
            FanTachSafetyState::Healthy => {
                info!(
                    stage,
                    attempt,
                    pwm = receipt.observed_pwm(),
                    readings = ?snapshot.readings,
                    "Amlogic pre-energize fan-motion admission accepted"
                );
                return Ok(receipt);
            }
            FanTachSafetyState::AirflowNotCommanded
            | FanTachSafetyState::EvidenceUnavailable { .. } => {
                anyhow::bail!("Amlogic {stage} pre-energize tach evidence unavailable: {state:?}");
            }
            FanTachSafetyState::Debouncing { .. } | FanTachSafetyState::Failed { .. }
                if attempt < AMLOGIC_FAN_SPINUP_ATTEMPTS =>
            {
                warn!(stage, attempt, ?state, readings = ?snapshot.readings, "Amlogic fans have not established motion; retrying before power admission");
                tokio::time::sleep(AMLOGIC_FAN_SPINUP_RETRY_DELAY).await;
            }
            _ => {
                anyhow::bail!(
                    "Amlogic {stage} pre-energize fan-motion admission refused after {} attempts: state={state:?}, readings={:?}",
                    AMLOGIC_FAN_SPINUP_ATTEMPTS,
                    snapshot.readings
                );
            }
        }
    }
    anyhow::bail!("Amlogic {stage} pre-energize fan-motion admission did not complete")
}

async fn admit_fan_airflow_envelope(
    fan: Arc<dyn FanAccess>,
    safety: &mut FanTachSafety,
    minimum_pwm: u8,
    maximum_pwm: u8,
) -> Result<FanCommandReceipt> {
    admit_fan_motion_at_pwm(fan.clone(), safety, maximum_pwm, "spin-up").await?;
    if minimum_pwm != maximum_pwm {
        if let Err(minimum_error) =
            admit_fan_motion_at_pwm(fan.clone(), safety, minimum_pwm, "energized minimum").await
        {
            match fan.set_speed_checked(maximum_pwm) {
                Ok(_) => {
                    anyhow::bail!(
                        "Amlogic energized-minimum motion proof failed; restored startup ceiling before refusing power: {minimum_error:#}"
                    );
                }
                Err(restore_error) => {
                    anyhow::bail!(
                        "Amlogic energized-minimum motion proof failed and startup ceiling restoration also failed: motion={minimum_error:#}; restore={restore_error}"
                    );
                }
            }
        }
    }
    // Leave startup at the retained ceiling. The low-point observation above
    // proves the later proportional controller may safely return to its floor.
    fan.set_speed_checked(maximum_pwm)
        .context("Amlogic final pre-energize fan command/readback failed")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoPicFanLoopDisposition {
    Continue,
    SafeOffAndStop,
}

fn nopic_fan_loop_disposition(state: &FanTachSafetyState) -> NoPicFanLoopDisposition {
    match state {
        FanTachSafetyState::Healthy | FanTachSafetyState::Debouncing { .. } => {
            NoPicFanLoopDisposition::Continue
        }
        FanTachSafetyState::AirflowNotCommanded
        | FanTachSafetyState::Failed { .. }
        | FanTachSafetyState::EvidenceUnavailable { .. } => NoPicFanLoopDisposition::SafeOffAndStop,
    }
}

fn observed_dspic_firmware(version: Option<u8>) -> Result<DspicFirmware> {
    let version = version.context(
        "dsPIC firmware was not observed; refusing protocol-dependent heartbeat startup",
    )?;
    let firmware = DspicFirmware::from_version(version);
    if !dspic_runtime_protocol_is_proven(firmware) {
        anyhow::bail!(
            "unsupported observed dsPIC firmware 0x{version:02X}; refusing protocol-dependent heartbeat startup"
        );
    }
    Ok(firmware)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoPicPowerState {
    NeverOwned,
    MayBeEnergized,
    EnabledByDaemon,
}

struct NoPicPsuGuard {
    state: NoPicPowerState,
    terminal_fence: Option<dcentrald_hal::platform::amlogic::AmlogicPowerThermalFence>,
}

struct NoPicSafeOffReceipt {
    power: dcentrald_hal::platform::amlogic::PsuSafeOffReceipt,
    management_fabric: dcentrald_hal::i2c::TerminalSafeOffTransition,
}

impl NoPicSafeOffReceipt {
    fn power(&self) -> &dcentrald_hal::platform::amlogic::PsuSafeOffReceipt {
        &self.power
    }

    fn management_fabric(&self) -> &dcentrald_hal::i2c::TerminalSafeOffTransition {
        &self.management_fabric
    }
}

impl NoPicPsuGuard {
    fn new() -> Self {
        Self {
            state: NoPicPowerState::NeverOwned,
            terminal_fence: None,
        }
    }

    fn prepare_enable(
        &mut self,
        fan_max_pwm: u8,
        terminal_fence: dcentrald_hal::platform::amlogic::AmlogicPowerThermalFence,
    ) {
        self.state = NoPicPowerState::MayBeEnergized;
        self.terminal_fence = Some(terminal_fence);
        arm_nopic_teardown(fan_max_pwm);
    }

    fn mark_enabled(&mut self) {
        debug_assert_eq!(self.state, NoPicPowerState::MayBeEnergized);
        self.state = NoPicPowerState::EnabledByDaemon;
    }

    fn owns_power(&self) -> bool {
        self.state != NoPicPowerState::NeverOwned
    }

    fn safe_off(&mut self) -> Result<NoPicSafeOffReceipt> {
        if !self.owns_power() {
            anyhow::bail!("NoPic software safe-off requested without an owned power lease");
        }
        let fabric_transition = self
            .terminal_fence
            .as_ref()
            .map(|fence| fence.latch_terminal_safe_off());
        let receipt = dcentrald_hal::platform::amlogic::disable_psu_checked()
            .context("checked NoPic GPIO437 safe-off failed")?;
        self.state = NoPicPowerState::NeverOwned;
        let management_fabric = fabric_transition.context(
            "NoPic GPIO437 is checked low, but no management-fabric terminal transition was issued",
        )?;
        Ok(NoPicSafeOffReceipt {
            power: receipt,
            management_fabric,
        })
    }
}

impl Drop for NoPicPsuGuard {
    fn drop(&mut self) {
        if self.owns_power() {
            if let Some(fence) = self.terminal_fence.as_ref() {
                let transition = fence.latch_terminal_safe_off();
                if !transition.no_controller_mutation_stage_in_flight() {
                    error!(
                        generation = transition.generation(),
                        "NoPic drop fenced management I2C with a controller mutation still in flight"
                    );
                }
            }
            // Safety (cut-hash-before-noise + PWM-30 home cap, per
            // ): CUT PSU POWER FIRST so the heat
            // source (the hashboards) is removed, THEN hold fans at the quiet
            // home cap for coast-down. NEVER blast a home-unit's fans to 100%:
            // once the chips are de-energized there is no active thermal load
            // that justifies a jet, and these are home/space-heater units the
            // operator works beside.
            //
            // (Was: 100% fan-blast for 2 s applied BEFORE the PSU cut — a direct
            // inversion of cut-hash-before-noise and a violation of the absolute
            // PWM-30 home cap. Audit wf_4a84d55e ABSENT finding, 2026-05-29.)
            if let Err(e) = dcentrald_hal::platform::amlogic::disable_psu_checked() {
                warn!(error = %e, "Failed to disable NoPic PSU during shutdown");
            }
            // Quiet coast-down at PWM 30 (home cap). The Amlogic PWM period is
            // 100_000 ns (AMLOGIC_PWM_PERIOD_NS), so PWM 30 = 30_000 ns duty —
            // NOT 100_000 (100% jet). Best-effort; fans run off the control-board
            // rail and stay powered after the hashboard PSU is disabled.
            let _ = std::fs::write("/sys/class/pwm/pwmchip0/pwm0/duty_cycle", "30000");
            let _ = std::fs::write("/sys/class/pwm/pwmchip0/pwm1/duty_cycle", "30000");
        }
    }
}

fn checked_nopic_emergency_safe_off(guard: &mut NoPicPsuGuard) -> Result<NoPicSafeOffReceipt> {
    if guard.owns_power() {
        guard.safe_off()
    } else {
        anyhow::bail!("checked NoPic emergency safe-off requested without an owned power lease")
    }
}

/// Process-global flag, armed the moment a NoPic (am3-aml) run energizes the PSU.
/// Release builds use `panic = "abort"`, which BYPASSES `NoPicPsuGuard::Drop`, and
/// NoPic (TAS5782M DAC voltage) has NO PIC heartbeat watchdog — so without this a
/// panic leaves the hashboards energized indefinitely (fire risk). The `main()`
/// crash panic hook reads this to cut PSU power. Mirrors the am2 `AM2_TEARDOWN_PARAMS`
/// pattern (W24-CRASH-1). Stores the home fan cap, already clamped to PWM_SAFETY_MAX.
static NOPIC_TEARDOWN_ARMED: std::sync::OnceLock<u8> = std::sync::OnceLock::new();

/// Arm the NoPic (am3-aml) panic-hook teardown — call at the instant the NoPic PSU
/// is energized. Idempotent (OnceLock::set). `fan_max_pwm` is clamped to the home
/// PWM_SAFETY_MAX so the coast-down can never blast.
pub fn arm_nopic_teardown(fan_max_pwm: u8) {
    let _ = NOPIC_TEARDOWN_ARMED.set(fan_max_pwm.min(dcentrald_hal::fan::PWM_SAFETY_MAX));
}

/// Best-effort cut-hash-before-noise teardown for the `main()` crash panic hook on
/// the am3-aml NoPic path. No-op (and no allocation) unless a NoPic run armed it.
/// Cuts PSU power FIRST (remove the heat source), then quiet-coasts fans at the home
/// cap. Swallows all errors (must never re-panic from inside the panic hook).
pub fn nopic_panic_hook_best_effort_teardown() {
    if let Some(&cap_pwm) = NOPIC_TEARDOWN_ARMED.get() {
        let _ = dcentrald_hal::platform::amlogic::disable_psu();
        // Amlogic PWM period = 100_000 ns, so PWM N => N * 1000 ns duty (PWM 30 = 30000).
        let duty = (cap_pwm.min(dcentrald_hal::fan::PWM_SAFETY_MAX) as u32) * 1_000;
        let duty_s = duty.to_string();
        let _ = std::fs::write("/sys/class/pwm/pwmchip0/pwm0/duty_cycle", &duty_s);
        let _ = std::fs::write("/sys/class/pwm/pwmchip0/pwm1/duty_cycle", &duty_s);
    }
}

struct Am2PsuRuntimeGuard {
    psu: Option<Arc<Mutex<Apw121215a>>>,
    gate: Option<PsuGpioGate>,
    /// dsPIC chip-rail disable leg, armed AFTER `cold_boot_init` energizes the
    /// per-chain rail (BM1398 0x20/0x21/0x22 or the BM1362-direct selected
    /// dsPIC). `None` until `set_dspic` is called → teardown then behaves
    /// byte-for-byte like the historical PSU-watchdog + set_voltage_min +
    /// PWR_CONTROL-deassert teardown.
    ///
    /// Why this exists: dropping PWR_CONTROL alone does NOT cut the per-chain
    /// dsPIC DC-DC rail — that is exactly the load-bearing finding the am2
    /// hybrid path encoded as `Am2HomeHardStopGuard::arm_dspic_teardown`
    /// (without it "every standalone attempt needs a fresh AC-cycle" and the
    /// chain stays energized behind PWR_CONTROL). The serial BM1398 / BM1362
    /// direct paths energize the same dsPIC chip rail but had no equivalent
    /// disable leg, so a bare `?` early-return after `cold_boot_init` (e.g.
    /// `init_asic_chain` / `init_bm1398` failure, stratum handshake error) left
    /// the chain rail ENABLED. Disabling the dsPIC voltage FIRST on teardown is
    /// cut-hash-before-noise; it is the same op the clean-stop path issues, so
    /// a redundant disable on clean exit is benign + idempotent.
    /// `(service, addrs, selected_addr, selected_fw_hint)`.
    dspic: Option<(I2cServiceHandle, Vec<u8>, u8, Option<u8>)>,
}

impl Am2PsuRuntimeGuard {
    fn new() -> Self {
        Self {
            psu: None,
            gate: None,
            dspic: None,
        }
    }

    fn set_psu(&mut self, psu: Arc<Mutex<Apw121215a>>) {
        self.psu = Some(psu);
    }

    fn set_gate(&mut self, gate: PsuGpioGate) {
        self.gate = Some(gate);
    }

    /// Arm the dsPIC chip-rail disable leg. Call once per `cold_boot_init`
    /// success with the I2C service + the dsPIC addresses whose rail was just
    /// energized (BM1398: all of `S19_DSPIC_ADDRS`; BM1362-direct: the single
    /// selected `pic_addr`). `selected_fw_hint` is only a per-addr decode hint
    /// (non-selected addrs auto-detect). Idempotent-safe; extends the addr set.
    fn set_dspic(
        &mut self,
        service: I2cServiceHandle,
        addrs: Vec<u8>,
        selected_addr: u8,
        selected_fw_hint: Option<u8>,
    ) {
        match self.dspic.as_mut() {
            Some((_, existing, _, _)) => {
                for a in addrs {
                    if !existing.contains(&a) {
                        existing.push(a);
                    }
                }
                existing.sort_unstable();
                existing.dedup();
            }
            None => {
                let mut addrs = addrs;
                addrs.sort_unstable();
                addrs.dedup();
                self.dspic = Some((service, addrs, selected_addr, selected_fw_hint));
            }
        }
    }

    fn teardown(&mut self, reason: &'static str) {
        // Cut hash/power FIRST (cut-hash-before-noise): disable voltage on every
        // armed dsPIC BEFORE touching the APW/PWR_CONTROL, so the per-chain
        // DC-DC rail is brought down instead of left energized behind a dropped
        // PWR_CONTROL. No-op when unarmed → teardown stays byte-for-byte
        // identical to the historical PSU-only behaviour on paths that never
        // energized a dsPIC (NoPic / passthrough).
        if let Some((service, addrs, selected_addr, selected_fw_hint)) = self.dspic.take() {
            for addr in addrs {
                let fw_hint = if addr == selected_addr {
                    selected_fw_hint
                } else {
                    None
                };
                let mut pic = Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint);
                // Best-effort heartbeat first (the hybrid path's pattern keeps
                // the parser sane before the disable opcode); errors are
                // logged, never fatal — teardown must never bail.
                if let Err(e) = pic.send_heartbeat() {
                    warn!(
                        reason,
                        addr = format_args!("0x{:02X}", addr),
                        error = %e,
                        "serial am2 dsPIC heartbeat before best-effort disable failed (continuing)"
                    );
                }
                match pic.disable_voltage() {
                    Ok(()) => info!(
                        reason,
                        addr = format_args!("0x{:02X}", addr),
                        "serial am2 dsPIC voltage disabled during teardown (cut-hash-before-noise)"
                    ),
                    Err(e) => warn!(
                        reason,
                        addr = format_args!("0x{:02X}", addr),
                        error = %e,
                        "serial am2 dsPIC voltage disable failed during teardown"
                    ),
                }
            }
        }

        if let Some(psu) = self.psu.take() {
            // Poison-tolerant lock: this is the cut-hash-before-noise teardown
            // path (called from Drop too). If a PSU heartbeat-thread panic
            // poisoned this mutex, `.unwrap()` would panic HERE and skip the
            // watchdog-off + voltage-min — leaving the rail pinned at the mining
            // setpoint behind only the ~30s hardware watchdog (and a panic inside
            // Drop during unwind aborts). Recover the guard instead so the
            // safety teardown actually runs. Matches the proven i2c.rs idiom.
            let mut psu = psu.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = psu.safe_shutdown_to_min() {
                warn!(reason, error = %e, "BM1362 direct path PSU safe-direction shutdown failed");
            }
        }

        if let Some(mut gate) = self.gate.take() {
            let gpio = gate.gpio();
            if let Err(e) = gate.deassert() {
                warn!(reason, gpio, error = %e, "BM1362 direct path PWR_CONTROL restore failed");
            }
        }
    }

    /// Immediate transport-independent fallback for a feeder that cannot be
    /// proven quiescent. Never enters the PSU mutex or emits dsPIC traffic:
    /// either may still be owned by the detached worker. PWR_CONTROL is cut
    /// first and watchdog feeding has already been cancelled by the thread
    /// owner. An already in-flight heartbeat may complete once, but cannot
    /// begin another loop iteration after the post-lock cancellation fence.
    fn hard_stop_out_of_band(&mut self, reason: &'static str) {
        if self.gate.is_none() && self.psu.is_none() && self.dspic.is_none() {
            return;
        }
        if let Some(mut gate) = self.gate.take() {
            let gpio = gate.gpio();
            if let Err(e) = gate.deassert() {
                warn!(
                    reason,
                    gpio,
                    error = %e,
                    "serial AM2 out-of-band PWR_CONTROL hard stop failed"
                );
            } else {
                warn!(
                    reason,
                    gpio, "serial AM2 PWR_CONTROL hard stop asserted without entering PSU mutex"
                );
            }
        } else {
            warn!(
                reason,
                "serial AM2 out-of-band hard stop had no owned PWR_CONTROL gate; relying on cancelled heartbeat watchdogs"
            );
        }

        // Dropping these owners is non-blocking. A detached feeder retains its
        // own Arc until it returns; no destructor here attempts to reclaim it.
        self.psu.take();
        self.dspic.take();
    }
}

impl Drop for Am2PsuRuntimeGuard {
    fn drop(&mut self) {
        // Drop can run on any `?` path while a feeder is wedged. It must never
        // wait on the shared PSU mutex or on I2C. Explicit graceful teardown is
        // performed only after `RuntimeThreadGuard::stop_and_join` proves every
        // feeder quiescent.
        self.hard_stop_out_of_band("drop");
    }
}

/// Select the fast baud rate based on platform.
/// Zynq NS16550A: 3,125,000 (custom BOTHER divisor from 200 MHz clock)
/// Amlogic meson_uart: 3,000,000 (standard B3000000 from 24 MHz crystal)
/// Fast baud rate for BM136x ASIC communication.
/// BM1368 FAST_UART register configures the ASIC's UART baud from 25 MHz crystal.
/// Zynq NS16550A: exact 3,125,000 via BOTHER (200 MHz PL clock / 64).
/// Amlogic meson_uart: CANNOT do 3,125,000 (rounds to 4M!). Must use B3000000.
/// Bosminer logs "3125000" but the kernel rounds it to the nearest standard rate.
/// The ASIC's FAST_UART value 0x00003001 = 3,125,000 from crystal, but the 4%
/// mismatch to host 3M is within UART tolerance for short bursts.
fn fast_baud() -> u32 {
    if std::path::Path::new("/dev/uio0").exists() {
        dcentrald_hal::serial::BAUD_3125000 // Zynq: exact 3,125,000 via BOTHER
    } else {
        dcentrald_hal::serial::BAUD_3000000 // Amlogic: B3000000 (closest to 3.125M)
    }
}

/// Non-destructive post-ENABLE chain UART rail-engagement probe (BM1362 path).
///
/// APW121215a (FW `0x71`) has NO voltage/current/power feedback (`psu.rs:493`
/// `has_voltage_feedback() == false`), and dsPIC fw=0x86 in bare protocol
/// returns only the FW echo byte for any read — including GET_VOLTAGE
/// (0x3B). The ENABLE_VOLTAGE bare ACK only confirms protocol-level
/// acceptance, NOT actual rail engagement. The only software signal that
/// the chain DC-DC has actually engaged 13.7 V is whether the BM1362 ASICs
/// drive any byte onto the chain UART RX line.
///
/// This probe opens the chain UART via `DevmemUart`, sleeps 200 ms for the
/// DC-DC to ramp, drains RX for up to 500 ms, and logs the byte count +
/// first-up-to-16-byte preview.
///
/// - `rx_bytes_pre_init == 0`: chain rail is likely 0 V — dsPIC ENABLE
///   didn't actually engage the DC-DC even though the I²C ACK landed.
/// - `rx_bytes_pre_init > 0`:  chain is electrically alive — BM1362 init
///   may need adjustment but the rail is up.
///
/// Best-effort: never returns an error. Phase 2 init_asic_chain reopens the
/// UART for formal init.
fn post_enable_chain_uart_probe(serial_device: &str, pic_addr: u8) {
    use dcentrald_hal::serial::DevmemUart;

    // Sleep 200 ms after ENABLE so the DC-DC has time to ramp.
    std::thread::sleep(Duration::from_millis(200));

    let uart = match DevmemUart::open_preserve_state(serial_device, 115_200) {
        Ok(u) => u,
        Err(e) => {
            warn!(
                error = %e,
                serial_device,
                "Post-ENABLE chain UART probe: DevmemUart::open failed — \
                 skipping rail-engagement diagnostic (Phase 2 init will retry)"
            );
            return;
        }
    };

    let mut buf = [0u8; 256];
    let total = uart.read_bytes_timeout(&mut buf, 500);

    let preview_len = total.min(16);
    let preview = &buf[..preview_len];

    tracing::info!(
        serial_device,
        pic_addr = format_args!("0x{:02X}", pic_addr),
        rx_bytes_pre_init = total,
        rx_preview = format!("{:02X?}", preview),
        "Post-ENABLE chain UART rail-engagement probe (rx_bytes>0 implies rail is electrically alive)"
    );

    if total == 0 {
        warn!(
            serial_device,
            "Post-ENABLE chain UART probe: 0 bytes in 500 ms — chain rail is \
             likely 0 V (dsPIC ENABLE didn't actually engage DC-DC). Hardware \
             multimeter on the chain rail is the next step."
        );
    } else {
        info!(
            serial_device,
            rx_bytes_pre_init = total,
            "Post-ENABLE chain UART probe: chain is electrically alive — \
             BM1362 init may still need adjustment but the rail is up."
        );
    }

    // `uart` drops here, releasing the mmap. Phase 2 init_asic_chain reopens.
}

/// Legacy count-inference policy retained only as a regression reference.
/// Production uses `resolve_native_serial_identity_and_geometry`.
#[cfg(test)]
fn serial_chip_id(model_hint: Option<&str>, chip_count: u8) -> u16 {
    if let Some(chip_id) = model_hint.and_then(model::model_chip_id) {
        return chip_id;
    }

    match chip_count {
        114 => 0x1398,
        110 | 77 => 0x1366,
        65 => 0x1370,
        108 => 0x1368,
        _ => 0x1362,
    }
}

/// True when `chip_id` belongs to a NoPic (TAS5782M / LDO) voltage-control
/// family — the S21-class chips that have ONLY ever shipped without a PIC /
/// dsPIC voltage controller. BM1368 (S21/T21) and BM1370 (S21 Pro / S21+ /
/// S21 XP) are the two production NoPic SHA-256 dies; BM1373 (S23) is the
/// pre-hardware NoPic continuation. Everything else (BM1387/BM1397/BM1398/
/// BM1362/BM1366) drives voltage through a PIC16 or dsPIC.
///
/// Source: `dcentrald-asic::drivers::PicType` profile table + `model.rs`
/// `pic_type_hint` (S21/T21/S21 Pro/+/XP all `ModelPicTypeHint::NoPic`).
#[cfg(test)]
fn serial_chip_id_is_nopic_family(chip_id: u16) -> bool {
    matches!(chip_id, 0x1368 | 0x1370 | 0x1373)
}

/// Legacy count-inference discriminator retained only to pin historical
/// regression behavior. It is not production authority; native serial mining
/// requires catalog identity through `resolve_native_serial_identity_and_geometry`.
///
/// Carry-forward **F-E3** (Preparedness-Sweep-v2 HIGH SAFETY): BM1370 SKUs
/// (S21 Pro / S21+ / S21 XP) must NOT silently route through the BM1368 (S21)
/// driver — and must NEVER fall through to the BM1362 catch-all (a PIC-family
/// driver that would try dsPIC I2C voltage control on a NoPic chain). A wrong
/// driver on a live chain is worse than a clean stop, so an undisambiguable
/// NoPic S21-family unit is **refused**, not guessed.
///
/// Discriminator evidence (corpus, not assumption): stock firmware reads the
/// CHIP_ID from register `0x00` — BM1370 returns `0x13700000`, BM1368 returns
/// `0x13680000` (ESP-Miner `bm1370.c` / `bm1368.c`,
///
/// §8.1/§9.1; :26`). When
/// the operator pins the model string (`s21pro`/`s21xp`/`s21+`/`s21plus` →
/// family `bm1370`) that register-truth is honoured directly. The hazard this
/// guard closes is the **count-only** inference when no model string is set:
/// a BM1370 chassis enumerating any count other than the canonical 65 would
/// otherwise be silently downgraded to BM1368 (108) or BM1362 (any other
/// count).
///
/// Rules:
/// - Explicit model chip-id is always trusted (operator-pinned ground truth).
/// - A `nopic`-declared unit must resolve to a NoPic family; if the count-only
///   inference produces a PIC family (BM1362/BM1398/BM1366) the resolution is
///   ambiguous → refuse with an actionable error (operator pins
///   `mining.model`).
/// - Proven count anchors are preserved exactly (114→1398, 110/77→1366,
///   65→1370, 108→1368, and the am2/XIL BM1362 default for PIC units).
#[cfg(test)]
fn resolve_serial_chip_id(model_hint: Option<&str>, chip_count: u8, nopic: bool) -> Result<u16> {
    let chip_id = serial_chip_id(model_hint, chip_count);
    let model_pinned = model_hint.and_then(model::model_chip_id).is_some();

    // F-E3 fail-safe: a NoPic chassis (S21 family) that the count-only
    // heuristic would map onto a PIC-family driver is genuinely ambiguous —
    // most dangerously a BM1370 SKU at a non-65 count falling through to the
    // BM1362 (PIC/dsPIC) catch-all. Refuse rather than drive a NoPic chain
    // with a PIC-family driver. Operator resolves by pinning `mining.model`.
    if nopic && !model_pinned && !serial_chip_id_is_nopic_family(chip_id) {
        anyhow::bail!(
            "Refusing to dispatch a NoPic S21-class chain to the PIC-family \
             driver inferred from chip_count={chip_count} (chip_id=0x{chip_id:04X}). \
             A BM1370 (S21 Pro / S21+ / S21 XP) or BM1368 (S21/T21) unit cannot \
             be safely disambiguated by chip count alone here — set \
             `mining.model` (e.g. s21pro / s21 / s21xp) so the BM1370-vs-BM1368 \
             driver split is explicit. A wrong driver on a live chain is worse \
             than a clean stop (carry-forward F-E3)."
        );
    }

    Ok(chip_id)
}

fn hardware_difficulty_for_serial_family(chip_id: u16) -> Result<u64> {
    MinerProfile::for_chip(chip_id)
        .map(|profile| profile.hardware_difficulty as u64)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "native serial chip 0x{chip_id:04X} has no MinerProfile difficulty policy"
            )
        })
}

fn validate_serial_model_voltage_identity(
    model_name: &str,
    chip_id: u16,
    pic_hint: Option<model::ModelPicTypeHint>,
) -> Result<()> {
    let definitively_nopic_family = matches!(chip_id, 0x1368 | 0x1370 | 0x1373);
    let controller_only_family =
        matches!(chip_id, 0x1362 | 0x1387 | 0x1391 | 0x1396 | 0x1397 | 0x1398);
    match pic_hint {
        Some(model::ModelPicTypeHint::NoPic) if controller_only_family => anyhow::bail!(
            "native serial model `{model_name}` declares NoPic but chip 0x{chip_id:04X} is a controller-only family"
        ),
        Some(model::ModelPicTypeHint::Pic16 | model::ModelPicTypeHint::DsPic)
            if definitively_nopic_family =>
        {
            anyhow::bail!(
                "native serial model `{model_name}` declares a PIC/dsPIC but chip 0x{chip_id:04X} is a definitive NoPic family"
            )
        }
        None if definitively_nopic_family => anyhow::bail!(
            "native serial model `{model_name}` has definitive NoPic chip 0x{chip_id:04X} but no voltage-architecture declaration"
        ),
        _ => Ok(()),
    }
}

fn resolve_native_serial_identity_and_geometry(
    model_hint: Option<&str>,
    configured_chip_count: Option<u8>,
) -> Result<(u16, u8)> {
    let model_name = model_hint
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context(
            "native serial mining requires an explicit recognized mining.model; chip count is geometry, not ASIC identity",
        )?;
    let chip_id = model::model_chip_id(model_name).ok_or_else(|| {
        anyhow::anyhow!(
            "native serial mining model `{model_name}` is unknown or has no authoritative chip identity"
        )
    })?;
    if !matches!(chip_id, 0x1362 | 0x1366 | 0x1368 | 0x1370 | 0x1398) {
        anyhow::bail!(
            "native serial mining model `{model_name}` resolves to unsupported chip 0x{chip_id:04X}; no serial dispatcher exists for this family"
        );
    }
    validate_serial_model_voltage_identity(
        model_name,
        chip_id,
        model::model_pic_type_hint(model_name),
    )?;
    let chip_count = configured_chip_count
        .or_else(|| model::model_chip_count_hint(model_name))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "native serial mining model `{model_name}` has no authoritative per-chain geometry; set mining.serial_chip_count explicitly"
            )
        })?;
    if chip_count == 0 {
        anyhow::bail!("native serial mining chip count must be at least 1");
    }
    Ok((chip_id, chip_count))
}

fn subtype_requires_bhb56_endpoint_capability(subtype: Option<&str>) -> bool {
    subtype
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase().starts_with("AMLCTRL_BHB56"))
        .unwrap_or(false)
}

/// Check if we're on a NoPic miner (no PIC voltage controller — voltage is
/// either kernel-managed via TAS5782M DTB or pre-set by hardware).
///
/// ## Declarative signals (always consulted, in order)
/// 1. `model.pic_type_hint == NoPic` — per the static catalog
///    ([`crate::model::model_pic_type_hint`]). Covers S21/T21/S21 Pro/+/XP,
///    S19K Pro NoPic, S19 XP, S19J XP — every model whose BraiinsOS+
///    catalog only ships NoPic variants.
/// 2. Fallback chip-id whitelist for chassis where the model string isn't
///    set: `0x1368` (BM1368, S21 family) and `0x1370` (BM1370, S21 Pro
///    family) — these chips have only ever shipped NoPic. Note that
///    `0x1366` (BM1366) is deliberately NOT in this list: the corpus only
///    proves S19k Pro NoPic, not all BM1366 boards, so a hardcoded chip-id
///    whitelist would mis-classify PIC-bearing BM1366 variants. EEPROM
///    authority (below) is the correct mechanism for that case.
///
/// ## EEPROM authority (gated, default-OFF —  Phase 2B)
/// When `DCENT_AM2_EEPROM_PIC_DETECT=1` the declarative result above is an
/// INPUT to the EEPROM-authoritative resolver
/// ([`crate::runtime::hardware_info::resolve_is_nopic_from_eeprom`], built
/// on the single pure decision [`crate::runtime::hardware_info::resolve_pic_type`]):
/// a chain whose EEPROM preamble classifies to a clear NoPic SKU
/// (BHB56902 / `0x05 0x11`) forces `true` regardless of carrier/chip-id; a
/// clear PIC/dsPIC preamble forces `false`; any weak/absent signal
/// (malformed/timeout/unpopulated/read-error/ambiguous) falls back to the
/// declarative result. The authority NEVER moves the answer toward "PIC"
/// on a weak signal — it fails toward the existing behavior. This is the
/// real runtime detection the old doc-comment falsely promised.
///
/// **No-regression guarantee:** with the gate OFF (the default) the EEPROM
/// is never read here and the result is the declarative value, byte-identical
/// to today. EEPROM reads (0x50-0x57) are READ-allowed by the HAL denylist;
/// this path never issues a write, and never SET_VOLTAGE on a NoPic board.
fn is_nopic(config: &DcentraldConfig) -> bool {
    let declarative_nopic = if let Some(model) = config.mining.model.as_deref() {
        matches!(
            model::model_pic_type_hint(model),
            Some(model::ModelPicTypeHint::NoPic)
        ) || matches!(config.mining.model_chip_id(), Some(0x1368 | 0x1370))
    } else {
        matches!(config.mining.model_chip_id(), Some(0x1368 | 0x1370))
    };

    // Chain-slot count for the gated EEPROM probe: the profile's chain
    // count when the chip-id is known, else the universal 3-chain default.
    let chain_slots = config
        .mining
        .model_chip_id()
        .and_then(MinerProfile::for_chip)
        .map(|p| p.chain_count as usize)
        .unwrap_or(3);

    crate::runtime::hardware_info::resolve_is_nopic_from_eeprom(declarative_nopic, chain_slots)
}

/// Legacy S19j Pro PIC I2C address fallback (7-bit).
///
/// Native BM1362 mode resolves the active PIC from the serial slot instead of
/// using this globally, because am2 boards expose one voltage controller per
/// hashboard at 0x20/0x21/0x22.
const S19J_PIC_ADDR_7BIT: u8 = 0x21;

/// S19 Pro dsPIC I2C addresses (7-bit).
const S19_DSPIC_ADDRS: [u8; 3] = [0x20, 0x21, 0x22];

/// PIC heartbeat interval — 1 s.
///
/// See [`dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`]
/// for the canonical per-`(Platform, PicFw)` matrix. Serial-mining is
/// am2-s17 / am3-aml depending on detected SoC — both rows pin 1 s
/// (am3-aml is no-op via `cfg.nopic`).
const PIC_HEARTBEAT_INTERVAL_MS: u64 = 1000;
const BM13XX_CMD_RESP_BODY_LEN: usize = 9;

/// BM1362: job_id increments by 24, 9-byte response body.
const BM1362_JOB_ID_INC: u8 = 24;
/// BM1366: job_id increments by 8, 9-byte response body.
const BM1366_JOB_ID_INC: u8 = 8;
/// BM1398: job_id increments by 4 (lower 2 bits = midstate index), 7-byte response body.
const BM1398_JOB_ID_INC: u8 = 4;
const JOB_ID_MASK: u8 = 0x7F;

const BM1362_RESP_BODY_LEN: usize = 9;
const BM1398_RESP_BODY_LEN: usize = 7;
const SERIAL_VERSION_ROLLING_FIELD_MASK: u32 = 0x1FFF_E000;

// ---------------------------------------------------------------------------
// CRC-5 for PIC I2C commands (same as ASIC protocol CRC5)
// ---------------------------------------------------------------------------

/// Build a 6-byte PIC command: [55 AA 04 cmd arg checksum]
///
/// S19j Pro PIC (v0x86 stock Bitmain) uses a simple byte-sum checksum,
/// NOT the CRC5 used by ASIC commands. Confirmed by matching bosminer strace:
///   [55 AA 04 17 00 1B] -> checksum = 0x04 + 0x17 + 0x00 = 0x1B
///   [55 AA 04 15 01 1A] -> checksum = 0x04 + 0x15 + 0x01 = 0x1A
///   [55 AA 04 16 00 1A] -> checksum = 0x04 + 0x16 + 0x00 = 0x1A
fn pic_cmd(cmd: u8, arg: u8) -> [u8; 6] {
    let checksum = 0x04u8.wrapping_add(cmd).wrapping_add(arg);
    [0x55, 0xAA, 0x04, cmd, arg, checksum]
}

/// Build a 7-byte PIC ENABLE/DISABLE_VOLTAGE command in the VNish-RE'd form:
///   `[55 AA 05 15 ARG 0x00 SUM]`
///
/// Source: VNish/bosminer cgminer disasm (RE corpus 2026-04-25, 22 firmwares
/// cross-validated). For fw=0x86 (S19j stock Bitmain) and fw=0x89 (S19j Pro am2)
/// the ENABLE/DISABLE frames have a 2-byte payload `[ARG, 0x00]`, NOT the
/// 1-byte `[ARG]` form previously used.
///
/// Verified frames:
///   ENABLE  : [55 AA 05 15 01 00 1B]  SUM = (0x05+0x15+0x01+0x00)&0xFF = 0x1B
///   DISABLE : [55 AA 05 15 00 00 1A]  SUM = (0x05+0x15+0x00+0x00)&0xFF = 0x1A
///
/// Mirrors `dcentrald_asic::dspic::dspic_enable_voltage_frame` /
/// `dspic_disable_voltage_frame` with `EnableFrameEncoding::VnishPadded`.
#[allow(dead_code)]
fn pic_enable_cmd_vnish(arg: u8) -> [u8; 7] {
    let checksum = 0x05u8
        .wrapping_add(0x15)
        .wrapping_add(arg)
        .wrapping_add(0x00);
    [0x55, 0xAA, 0x05, 0x15, arg, 0x00, checksum]
}

// ---------------------------------------------------------------------------
// BM1362 ASIC init constants (from bm1362.rs + Mujina PROTOCOL.md)
// ---------------------------------------------------------------------------

const VERSION_MASK_VALUE: u32 = 0x9000_FFFF;
const INIT_CONTROL_BCAST: u32 = 0x0000_0000;
const MISC_CONTROL_INIT: u32 = 0x00C1_00B0;
const INIT_CONTROL_PER_CHIP: u32 = 0x0200_0000;
const CORE_REG_HASH_CLK: u32 = 0x8000_8540;
const CORE_REG_CLK_DELAY: u32 = 0x8000_8008; // BM1362-specific
const CORE_REG_UNKNOWN: u32 = 0x8000_82AA;
const IO_DRIVER_NORMAL: u32 = 0x0001_1111;
const ANALOG_MUX_VALUE: u32 = 0x0000_0003;
const FAST_UART_VALUE: u32 = 0x0000_3011;
// R6-7 keeps BM1362 UART_RELAY writes lab-gated until exact 0x2C/0x34
// control semantics are live-captured.
const BM1362_UART_RELAY_REG: u8 = 0x2C;
const BM1362_UART_RELAY_ENABLE: u32 = 0x007C_0003;
const BM1362_UART_RELAY_REG_ALT: u8 = 0x34;
const BM1362_UART_RELAY_ENABLE_ALT: u32 = 0x000F_0003;
const TICKET_MASK_256: u32 = 0x0000_00FF;
const NONCE_RANGE_126: u32 = 0x0000_1381; // BM1362: 126 chips (S19j Pro)
const NONCE_RANGE_108: u32 = 0x0000_15A4; // BM1368: 108 chips (S21 stock default)
const BM1362_PLL0_DIVIDER_REG: u8 = 0x70;
const BM1362_TRACE_PLL0_DIVIDER: u32 = 0x0000_0000;
const BM1362_TRACE_PLL_PARAM_525: u32 = 0x40A8_0265;

/// Serial dispatch pacing during init — mirrors the traced/order-sensitive am2 path.
const SERIAL_PACE_MIN_MS: u64 = 20;

// ---------------------------------------------------------------------------
// BM1368 ASIC init constants (from bm1368.rs + ESP-Miner, verified on S21)
// Register values differ from BM1362 — using BM1362 values = 0 nonces.
// ---------------------------------------------------------------------------
const BM1368_REG_A8_BCAST: u32 = 0x0007_0000;
const BM1368_MISC_CTRL_BCAST: u32 = 0xFF0F_C100;
const BM1368_CORE_REG_1: u32 = 0x8000_8B00;
const BM1368_CORE_REG_2: u32 = 0x8000_8018;
const BM1368_CORE_REG_3: u32 = 0x8000_82AA;
const BM1368_TICKET_MASK: u32 = BM1368_FIXTURE_TICKET_MASK;
const BM1368_IO_DRIVER: u32 = 0x0211_1111;
const BM1368_REG_A8_PER_CHIP: u32 = 0x0007_01F0;
const BM1368_MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;
const BM1368_FAST_UART: u32 = 0x0000_3001; // 3.125M (BM1366+ value). Host B3000000 within tolerance.
/// BM1368 PLL FB_DIV range (144-235, different from BM1362's 160-239).
const BM1368_FB_DIV_MIN: u16 = 144;
const BM1368_FB_DIV_MAX: u16 = 235;

// ---------------------------------------------------------------------------
// BM1366 ASIC init constants (from bm1366.rs + ESP-Miner)
// ---------------------------------------------------------------------------
const BM1366_VERSION_MASK_VALUE: u32 = 0x9000_FFFF;
const BM1366_REG_A8_BCAST: u32 = 0x0007_0000;
const BM1366_REG_A8_PER_CHIP: u32 = 0x0007_01F0;
const BM1366_MISC_CTRL_BCAST: u32 = 0xFF0F_C100;
const BM1366_MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;
const BM1366_CORE_REG_HASH_CLOCK: u32 = 0x8000_8540;
const BM1366_CORE_REG_CLOCK_DELAY: u32 = 0x8000_8020;
const BM1366_CORE_REG_UNKNOWN: u32 = 0x8000_82AA;
const BM1366_ANALOG_MUX: u32 = 0x0000_0003;
const BM1366_IO_DRIVER: u32 = 0x0211_1111;
const BM1366_UART_RELAY: u32 = 0x007C_0003;
const BM1366_HASH_COUNTING_S19XP: u32 = 0x0000_151C;
const BM1366_HASH_COUNTING_S19K: u32 = 0x0000_115A;
const BM1366_TICKET_MASK: u32 = 0x0000_00FF;

// ---------------------------------------------------------------------------
// BM1370 ASIC init constants (from bm1370.rs + ESP-Miner)
// ---------------------------------------------------------------------------
const BM1370_VERSION_MASK_VALUE: u32 = 0x9000_FFFF;
const BM1370_REG_A8_BCAST: u32 = 0x0007_0000;
const BM1370_REG_A8_PER_CHIP: u32 = 0x0007_01F0;
const BM1370_MISC_CTRL_BCAST: u32 = 0xF000_C100;
const BM1370_MISC_CTRL_PER_CHIP: u32 = 0xF000_C100;
const BM1370_CORE_REG_1: u32 = 0x8000_8B00;
const BM1370_CORE_REG_2: u32 = 0x8000_800C;
const BM1370_CORE_REG_3: u32 = 0x8000_82AA;
const BM1370_CORE_REG_EXTRA: u32 = 0x8000_8DEE;
const BM1370_MISC_SETTINGS_B9: u32 = 0x0000_4480;
const BM1370_ANALOG_MUX: u32 = 0x0000_0002;
const BM1370_IO_DRIVER: u32 = 0x0001_1111;
const BM1370_HASH_COUNTING: u32 = 0x0000_1EB5;
const BM1370_TICKET_MASK: u32 = 0x0000_00FF;

/// BM1362 PLL table — (freq_mhz, pll_reg_value).
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

// ---------------------------------------------------------------------------
// BM1398 ASIC init constants (from bm1398.rs)
// ---------------------------------------------------------------------------

const BM1398_MISC_CTRL_INIT: u32 = 0x0000_7A31; // BT8D=26 → 115200 baud
const BM1398_MISC_CTRL_FAST: u32 = 0x0000_6031; // BT8D=0 → 3.125 MHz baud
const BM1398_TICKET_MASK: u32 = 0x0000_00FF; // Difficulty 256
const BM1398_ORDERED_CLK_EN: u32 = 0x0000_0001;
const BM1398_CLK_ORDER_CTRL: u32 = 0x0000_0000;

fn bm1398_pll_lookup(target_mhz: u16) -> (u32, u16) {
    let target_mhz = target_mhz.clamp(400, 700);
    let solution = dcentrald_api_types::bm1398_protocol::resolve_bm1398_pll(target_mhz)
        .expect("built-in BM1398 PLL search envelope must resolve mining frequencies");
    let actual_millimhz = solution
        .dividers
        .output_millimhz(dcentrald_api_types::bm1398_protocol::BM1398_PLL_SEARCH_SPEC.reference_mhz)
        .expect("resolved BM1398 dividers are non-zero");
    (
        solution.register_value,
        ((actual_millimhz + 500) / 1_000) as u16,
    )
}

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

/// BM1368 PLL brute-force search (from bm1368.rs).
/// freq = 25 MHz * fb_div / (ref_div * postdiv1 * postdiv2)
/// Returns (pll_reg_value, actual_freq_mhz).
fn bm1368_pll_search(target_mhz: u16) -> (u32, u16) {
    let target = target_mhz as f64;
    let mut best_fb: u8 = 144;
    let mut best_ref: u8 = 1;
    let mut best_pd1: u8 = 1;
    let mut best_pd2: u8 = 1;
    let mut best_freq: f64 = 0.0;
    let mut best_diff: f64 = f64::MAX;

    for ref_div in [1u8, 2] {
        for postdiv1 in 1u8..=7 {
            for postdiv2 in 1u8..=postdiv1 {
                for fb_div in BM1368_FB_DIV_MIN..=BM1368_FB_DIV_MAX {
                    let freq =
                        25.0 * fb_div as f64 / (ref_div as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target).abs();
                    if diff < best_diff {
                        best_fb = fb_div as u8;
                        best_ref = ref_div;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                        best_freq = freq;
                        best_diff = diff;
                    }
                }
            }
        }
    }

    // Encode PLL register: vco_scale | fb_div | ref_div | postdiv
    let vco = 25.0 * best_fb as f64 / best_ref as f64;
    let vco_scale: u8 = if vco >= 2400.0 { 0x50 } else { 0x40 };
    let postdiv = ((best_pd1 - 1) << 4) | (best_pd2 - 1);
    let pll_reg =
        (vco_scale as u32) << 24 | (best_fb as u32) << 16 | (best_ref as u32) << 8 | postdiv as u32;

    (pll_reg, best_freq.round() as u16)
}

fn bm1366_pll_search(target_mhz: u16) -> (u32, u16) {
    const BM1366_FB_DIV_MIN: u16 = 144;
    const BM1366_FB_DIV_MAX: u16 = 235;

    let target = target_mhz as f64;
    let mut best_fb = BM1366_FB_DIV_MIN;
    let mut best_ref = 1u8;
    let mut best_pd1 = 1u8;
    let mut best_pd2 = 1u8;
    let mut best_freq = 0.0f64;
    let mut best_diff = f64::MAX;
    let mut best_vco = f64::MAX;

    for ref_div in [1u8, 2] {
        for postdiv1 in 1u8..=7 {
            for postdiv2 in 1u8..=postdiv1 {
                for fb_div in BM1366_FB_DIV_MIN..=BM1366_FB_DIV_MAX {
                    let freq =
                        25.0 * fb_div as f64 / (ref_div as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target).abs();
                    let vco = 25.0 * fb_div as f64 / ref_div as f64;
                    if diff < best_diff || (diff == best_diff && vco < best_vco) {
                        best_fb = fb_div;
                        best_ref = ref_div;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                        best_freq = freq;
                        best_diff = diff;
                        best_vco = vco;
                    }
                }
            }
        }
    }

    let vco_scale: u8 = if best_vco >= 2400.0 { 0x50 } else { 0x40 };
    let postdiv = ((best_pd1 - 1) << 4) | (best_pd2 - 1);
    let pll_reg =
        (vco_scale as u32) << 24 | (best_fb as u32) << 16 | (best_ref as u32) << 8 | postdiv as u32;

    (pll_reg, best_freq.round() as u16)
}

fn bm1370_pll_search(target_mhz: u16) -> (u32, u16) {
    const BM1370_FB_DIV_MIN: u16 = 160;
    const BM1370_FB_DIV_MAX: u16 = 239;

    let target = target_mhz as f64;
    let mut best_fb = BM1370_FB_DIV_MIN;
    let mut best_ref = 1u8;
    let mut best_pd1 = 1u8;
    let mut best_pd2 = 1u8;
    let mut best_freq = 0.0f64;
    let mut best_diff = f64::MAX;
    let mut best_vco = f64::MAX;

    for ref_div in 1u8..=2 {
        for postdiv1 in 1u8..=7 {
            for postdiv2 in 1u8..=postdiv1 {
                for fb_div in BM1370_FB_DIV_MIN..=BM1370_FB_DIV_MAX {
                    let freq =
                        25.0 * fb_div as f64 / (ref_div as f64 * postdiv1 as f64 * postdiv2 as f64);
                    let diff = (freq - target).abs();
                    let vco = 25.0 * fb_div as f64 / ref_div as f64;
                    if diff < best_diff || (diff == best_diff && vco < best_vco) {
                        best_fb = fb_div;
                        best_ref = ref_div;
                        best_pd1 = postdiv1;
                        best_pd2 = postdiv2;
                        best_freq = freq;
                        best_diff = diff;
                        best_vco = vco;
                    }
                }
            }
        }
    }

    let vco_scale: u8 = if best_vco >= 2400.0 { 0x50 } else { 0x40 };
    let postdiv = ((best_pd1 - 1) << 4) | (best_pd2 - 1);
    let pll_reg =
        (vco_scale as u32) << 24 | (best_fb as u32) << 16 | (best_ref as u32) << 8 | postdiv as u32;

    (pll_reg, best_freq.round() as u16)
}

pub struct SerialMiner {
    config: DcentraldConfig,
    shutdown: CancellationToken,
}

impl SerialMiner {
    pub fn new(config: DcentraldConfig, shutdown: CancellationToken) -> Self {
        Self { config, shutdown }
    }

    fn drain_serial_passthrough_backlog(serial: &SerialChainBackend, window_ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(window_ms);
        let mut drained_frames = 0usize;

        while Instant::now() < deadline {
            match serial.read_all_responses(50) {
                Ok(batch) if !batch.is_empty() => drained_frames += batch.len(),
                Ok(_) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    warn!(error = %e, "Passthrough backlog drain read failed");
                    break;
                }
            }
        }

        let _ = serial.flush_io();
        info!(
            window_ms,
            drained_frames, "Passthrough serial backlog drained before own work dispatch"
        );
    }

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
        let slot = Self::am2_slot_from_serial_device(serial_device)?;
        S19_DSPIC_ADDRS.get(slot as usize).copied()
    }

    fn bm1362_pic_addr_for_serial_runtime(
        serial_device: &str,
        is_bm1362: bool,
        nopic: bool,
        passthrough: bool,
    ) -> Result<Option<u8>> {
        if !is_bm1362 || nopic {
            return Ok(None);
        }
        if passthrough {
            info!(
                serial_device = %serial_device,
                "BM1362 passthrough selected; skipping AM2 PIC address resolution"
            );
            return Ok(None);
        }

        let addr = Self::am2_pic_addr_from_serial_device(serial_device).ok_or_else(|| {
            anyhow::anyhow!(
                "BM1362 serial device {} does not map to a known am2 PIC address",
                serial_device,
            )
        })?;
        Ok(Some(addr))
    }

    fn dspic_service_for_serial_route(
        i2c: &I2cServiceHandle,
        sessions: &[DspicEndpointSession],
        endpoint_capability_required: bool,
        address: u8,
        firmware: Option<DspicFirmware>,
    ) -> Result<DspicService> {
        if let Some(session) = sessions.iter().find(|session| session.address() == address) {
            return Ok(match firmware {
                Some(firmware) => session.service_with_firmware(firmware),
                None => session.service(),
            });
        }
        if endpoint_capability_required {
            anyhow::bail!(
                "discovery-bound dsPIC endpoint capability for I2C 0x{address:02X} is unavailable; refusing caller-asserted protocol/address fallback"
            );
        }
        Ok(match firmware {
            Some(firmware) => DspicService::new_with_firmware(i2c.clone(), address, firmware),
            None => DspicService::new(i2c.clone(), address),
        })
    }

    fn am3_bb_uart_trans_chain_from_serial_device(serial_device: &str) -> Option<usize> {
        DEFAULT_CHAIN_TTYS
            .iter()
            .position(|path| *path == serial_device)
    }

    fn am3_bb_uart_trans_chains_from_serial_device(serial_device: &str) -> Option<Vec<usize>> {
        let mut chains = Vec::new();
        for raw in serial_device.split(',') {
            let path = raw.trim();
            if path.is_empty() {
                return None;
            }
            let chain = Self::am3_bb_uart_trans_chain_from_serial_device(path)?;
            if !chains.contains(&chain) {
                chains.push(chain);
            }
        }
        if chains.is_empty() {
            None
        } else {
            Some(chains)
        }
    }

    fn am3_bb_uart_trans_chain_bits(chains: &[usize]) -> u32 {
        chains.iter().fold(0u32, |bits, chain| {
            if *chain < DEFAULT_CHAIN_TTYS.len() {
                bits | (1u32 << chain)
            } else {
                bits
            }
        })
    }

    fn spawn_am3_bb_uart_trans_io_thread(
        serial_device: String,
        selected_chains: Vec<usize>,
        work_queue_io: Arc<Mutex<VecDeque<Vec<u8>>>>,
        nonce_tx: mpsc::Sender<Vec<u8>>,
        reader_shutdown: CancellationToken,
        work_queue_depth: usize,
        tx_burst_per_loop: usize,
    ) -> Result<std::thread::JoinHandle<()>> {
        let mut service = UartTransService::open_paths_with_baud(DEFAULT_CHAIN_TTYS, fast_baud())
            .context("am3-bb uart_trans failed to open ttyO chains")?;
        let selected_chain_bits = Self::am3_bb_uart_trans_chain_bits(&selected_chains);
        service.set_chain_exist_bits(selected_chain_bits);
        service
            .set_work_queue_count(work_queue_depth)
            .context("am3-bb uart_trans rejected queue depth")?;
        service
            .set_send_interval(Duration::from_millis(BM1362_DISPATCH_INTERVAL_MS))
            .context("am3-bb uart_trans rejected send interval")?;
        service.start_send_work_timer();

        std::thread::Builder::new()
            .name("am3-bb-uart-trans-io".to_string())
            .spawn(move || {
                info!(
                    serial_device,
                    ?selected_chains,
                    selected_chain_bits,
                    work_queue_depth,
                    tx_burst_per_loop,
                    "am3-bb uart_trans I/O thread starting"
                );

                let mut total_tx: u64 = 0;
                let mut total_nonces: u64 = 0;
                let mut last_diag = Instant::now();

                loop {
                    if reader_shutdown.is_cancelled() {
                        break;
                    }

                    for _ in 0..tx_burst_per_loop {
                        let frame = work_queue_io
                            .lock()
                            .unwrap_or_else(|e| {
                                tracing::warn!("work_queue mutex poisoned, recovering");
                                e.into_inner()
                            })
                            .pop_front();
                        let Some(frame) = frame else {
                            break;
                        };

                        let work = match UartWork::from_command_frame(&frame) {
                            Ok(work) => work,
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    frame_len = frame.len(),
                                    "am3-bb uart_trans dropped malformed BM1362 work frame"
                                );
                                continue;
                            }
                        };

                        for chain in &selected_chains {
                            if let Err(e) = service.enqueue_work(*chain, work.clone()) {
                                warn!(error = %e, chain, "am3-bb uart_trans queue failed");
                                break;
                            }
                        }
                    }

                    match service.send_due_work_once() {
                        Ok(sent) => total_tx = total_tx.saturating_add(sent as u64),
                        Err(e) => {
                            warn!(error = %e, "am3-bb uart_trans work send failed");
                            break;
                        }
                    }

                    match service.poll_nonces_once() {
                        Ok(nonces) => {
                            for (chain, nonce) in nonces {
                                if !selected_chains.contains(&chain) {
                                    debug!(
                                        chain,
                                        ?selected_chains,
                                        "am3-bb uart_trans ignored nonce from unselected chain"
                                    );
                                    continue;
                                }
                                total_nonces = total_nonces.saturating_add(1);
                                if nonce_tx
                                    .blocking_send(nonce.to_bm1362_body().to_vec())
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "am3-bb uart_trans nonce poll failed");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }

                    if last_diag.elapsed() > Duration::from_secs(10) {
                        info!(
                            total_nonces,
                            total_tx,
                            "am3-bb uart_trans I/O: {} RX nonces, {} TX work sent",
                            total_nonces,
                            total_tx,
                        );
                        last_diag = Instant::now();
                    }

                    std::thread::sleep(service.send_interval());
                }

                info!("am3-bb uart_trans I/O thread exited");
            })
            .context("Failed to spawn am3-bb uart_trans I/O thread")
    }

    fn pulse_am2_hashboard_reset(serial_device: &str) {
        let Some(slot) = Self::am2_slot_from_serial_device(serial_device) else {
            return;
        };

        match dcentrald_hal::platform::zynq::ZynqPlatform::new() {
            Ok(platform) => match platform.open_board_control() {
                Ok(Some(board_control)) => {
                    if let Err(e) = board_control.pulse_reset(slot) {
                        warn!(slot, path = serial_device, error = %e, "am2 board-control reset pulse failed");
                    }
                }
                Ok(None) => {
                    warn!(
                        slot,
                        path = serial_device,
                        "am2 board-control IP not available for reset pulse"
                    );
                }
                Err(e) => {
                    warn!(slot, path = serial_device, error = %e, "Failed to open am2 board-control for reset pulse");
                }
            },
            Err(e) => {
                warn!(slot, path = serial_device, error = %e, "Failed to detect Zynq platform for am2 reset pulse");
            }
        }
    }

    fn bm1362_stop_after_pre_pll_probe_enabled() -> bool {
        std::env::var_os("DCENT_BM1362_STOP_AFTER_PRE_PLL_PROBE").is_some()
    }

    fn bm1362_skip_post_power_reset() -> bool {
        std::env::var_os("DCENT_BM1362_SKIP_POST_POWER_RESET").is_some()
    }

    fn bm1362_early_115200_probe_enabled() -> bool {
        std::env::var_os("DCENT_BM1362_EARLY_115200_PROBE").is_some()
    }

    fn bm1362_allow_stale_enable_reply_enabled() -> bool {
        std::env::var_os("DCENT_BM1362_ALLOW_STALE_ENABLE_REPLY").is_some()
    }

    fn bm1362_uart_relay_lab_enabled() -> bool {
        std::env::var_os("DCENT_BM1362_ENABLE_UART_RELAY_LAB").is_some()
    }

    fn am3_bb_uart_trans_lab_enabled() -> bool {
        std::env::var_os("DCENT_AM3_BB_ENABLE_UART_TRANS_LAB").is_some()
    }

    fn maybe_write_bm1362_uart_relay(
        serial: &SerialChainBackend,
        stage: &'static str,
    ) -> Result<()> {
        if !Self::bm1362_uart_relay_lab_enabled() {
            warn!(
                stage,
                "BM1362 UART_RELAY reg 0x2C/0x34 writes skipped by default; set DCENT_BM1362_ENABLE_UART_RELAY_LAB=1 only for R6-7 capture work"
            );
            return Ok(());
        }

        serial
            .send_write_reg_broadcast_bm1397plus(BM1362_UART_RELAY_REG, BM1362_UART_RELAY_ENABLE)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(
            BM1362_UART_RELAY_REG_ALT,
            BM1362_UART_RELAY_ENABLE_ALT,
        )?;
        std::thread::sleep(Duration::from_millis(10));
        info!(stage, "BM1362 UART_RELAY lab-gated broadcast sent");
        Ok(())
    }

    fn is_known_pic_fw(version: u8) -> bool {
        matches!(version, 0x82 | 0x86 | 0x89 | 0x8A | 0xB9 | 0xFE)
    }

    fn is_shift_left_pic_artifact(buf: &[u8]) -> bool {
        buf.len() >= 2 && buf.windows(2).all(|w| w[1] == w[0].wrapping_shl(1))
    }

    fn classify_pic_reply(buf: &[u8]) -> &'static str {
        if buf.is_empty() {
            "empty"
        } else if buf.iter().all(|&b| b == 0x00) {
            "all-zero"
        } else if buf.iter().all(|&b| b == 0xFF) {
            "all-ff"
        } else if Self::is_shift_left_pic_artifact(buf) {
            "shift-left-bus-noise"
        } else if Self::parse_pic_fw_reply(buf, buf.len()).is_some() {
            "valid-fw"
        } else {
            "unknown"
        }
    }

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

    fn parse_pic_fw_reply(buf: &[u8], read_len: usize) -> Option<u8> {
        let buf = &buf[..read_len.min(buf.len())];
        if buf.is_empty()
            || buf.iter().all(|&b| b == 0x00)
            || buf.iter().all(|&b| b == 0xFF)
            || Self::is_shift_left_pic_artifact(buf)
        {
            return None;
        }

        if buf.len() >= 3 && buf[0] == 0x05 && buf[1] == 0x17 && Self::is_known_pic_fw(buf[2]) {
            return Some(buf[2]);
        }
        if buf.len() >= 3 && buf[0] == 0x17 && Self::is_known_pic_fw(buf[2]) {
            return Some(buf[2]);
        }
        if Self::is_known_pic_fw(buf[0]) {
            return Some(buf[0]);
        }
        None
    }

    fn format_probe_samples(responses: &[Vec<u8>]) -> String {
        responses
            .iter()
            .take(4)
            .map(|resp| {
                resp.iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

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
                            consecutive_fails
                        );
                    } else if consecutive_fails == 25 {
                        error!(fails = consecutive_fails, "PSU heartbeat dead for 25s — voltage likely already cut. Consider shutdown.");
                    }
                }
            }
        }
    }

    fn pic_read_fw_version_service(i2c: &I2cServiceHandle, addr: u8) -> Result<(u8, Vec<u8>)> {
        // Service-only three-phase probing: flush -> write -> quiet window -> read.
        // Do not use I2C_RDWR here; it can turn a parser wedge into persistent bus noise.
        //
        // DELIBERATE DIVERGENCE from the am2-Zynq hybrid path's pic_read_fw_version_service
        // in s19j_hybrid_mining.rs (which dropped the 16-zero flush + 5-byte read in favour
        // of a bosminer-faithful no-flush, 1-byte clean read + retry, 2026-05-21). This
        // BM1362 *direct-serial* path is the am3-bb `a lab unit` / Amlogic accepted-share-proven
        // route, where the 16-zero flush is REQUIRED to clear a healthy fw=0x89 dsPIC MSSP
        // parser (see init_pic doc-comment below: "v0x89 needs 16, not 8"). The all-FF wedge
        // the hybrid fix targets is a `a lab unit`/`a lab unit` am2 phenomenon, NOT observed here. Do NOT
        // "unify" these two readers until the `a lab unit` clean-read A/B proves the flush is the
        // actual am2 FF-generator — unifying now would regress the proven `a lab unit` path.
        const GET_VERSION_FRAMED: [u8; 6] = [0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B];
        const GET_VERSION_SHORT: [u8; 3] = [0x55, 0xAA, 0x17];
        let probes: [(&str, &[u8], usize); 2] = [
            ("framed-55aa0417001b", &GET_VERSION_FRAMED, 5),
            ("short-55aa17", &GET_VERSION_SHORT, 1),
        ];
        let mut samples = Vec::new();

        for attempt in 1..=3 {
            for (variant, frame, read_len) in probes.iter().copied() {
                let buf = match i2c.transaction_mutating(
                    I2cMutationLabel::Recovery,
                    addr,
                    vec![
                        I2cTransactionStep::SetTimeout(10),
                        I2cTransactionStep::WriteByteByByte(vec![0u8; 16]),
                        I2cTransactionStep::SleepMs(10),
                        I2cTransactionStep::Write(frame.to_vec()),
                        I2cTransactionStep::SleepMs(100),
                        I2cTransactionStep::Read(read_len),
                    ],
                ) {
                    Ok(mut reads) => match reads.pop() {
                        Some(buf) => buf,
                        None => {
                            warn!(
                                addr = format_args!("0x{:02X}", addr),
                                attempt,
                                variant,
                                "BM1362 direct PIC GET_VERSION transaction returned no read"
                            );
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                    },
                    Err(e) => {
                        samples.push(format!("{}#{}:transaction-error:{}", variant, attempt, e));
                        warn!(
                            addr = format_args!("0x{:02X}", addr),
                            attempt,
                            variant,
                            error = %e,
                            "BM1362 direct PIC GET_VERSION transaction failed"
                        );
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                };
                let class = Self::classify_pic_reply(&buf);
                samples.push(format!("{}#{}:{}:{:02X?}", variant, attempt, class, buf));
                info!(
                    addr = format_args!("0x{:02X}", addr),
                    attempt,
                    variant,
                    class,
                    read_len = buf.len(),
                    raw = format_args!("{:02X?}", buf),
                    "BM1362 direct PIC GET_VERSION service reply",
                );

                if let Some(fw) = Self::parse_pic_fw_reply(&buf, buf.len()) {
                    return Ok((fw, buf));
                }

                warn!(
                    addr = format_args!("0x{:02X}", addr),
                    attempt,
                    variant,
                    class,
                    raw = format_args!("{:02X?}", buf),
                    "BM1362 direct PIC service GET_VERSION did not return a valid firmware reply",
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        Err(anyhow::anyhow!(
            "BM1362 direct PIC service GET_VERSION failed at 0x{:02X}: no valid framed/short 0x17 response after 3 attempts; recent samples: {}",
            addr,
            Self::format_pic_probe_samples(&samples),
        ))
    }

    fn log_bm1362_probe_stage(
        serial: &mut SerialChainBackend,
        stage: &str,
        wait_ms: u64,
    ) -> Result<()> {
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(wait_ms));
        let responses = serial.read_all_responses(500)?;
        info!(
            stage,
            responses = responses.len(),
            samples = %Self::format_probe_samples(&responses),
            "BM1362 probe ladder"
        );
        Ok(())
    }

    /// Reset ASICs to 115200 baud from any previous baud rate (hot-start recovery).
    ///
    /// After power cycle, ASICs default to 115200 so this is a no-op.
    /// After killing previous firmware, ASICs may be at 1.5625M or 3.125M.
    /// We send chain_inactive at each possible baud to wake them up,
    /// then send MiscControl with baud reset to force 115200.
    fn reset_asic_baud(serial_device: &str) {
        let fb = fast_baud();
        info!(
            "Hot-start baud reset: trying common baud rates (fast={})",
            fb
        );

        // Try fast baud first (3.125M on Zynq, 3.0M on Amlogic)
        if let Ok(serial) = SerialChainBackend::open(0, serial_device, fb) {
            info!("Sending chain_inactive at {}M", fb / 1_000_000);
            // Send BOTH BM1387 and BM1397+ commands (universal — works for any chip family)
            let _ = serial.send_chain_inactive();
            let _ = serial.send_chain_inactive_bm1397plus();
            let _ = serial.send_write_reg_broadcast(0x18, 0x00C1_00B0);
            let _ = serial.send_write_reg_broadcast_bm1397plus(0x18, 0x00C1_00B0);
            std::thread::sleep(Duration::from_millis(20));
            drop(serial);
        }

        // Try 1.5625M (Zynq only — Amlogic can't produce this rate)
        if fb != 3_000_000 {
            if let Ok(serial) = SerialChainBackend::open(0, serial_device, 1_562_500) {
                info!("Sending chain_inactive at 1.5625M");
                let _ = serial.send_chain_inactive();
                let _ = serial.send_chain_inactive_bm1397plus();
                let _ = serial.send_write_reg_broadcast(0x18, 0x00C1_00B0);
                let _ = serial.send_write_reg_broadcast_bm1397plus(0x18, 0x00C1_00B0);
                std::thread::sleep(Duration::from_millis(20));
                drop(serial);
            }
        }

        std::thread::sleep(Duration::from_millis(50));
        info!("Baud reset complete — ASICs should be at 115200");
    }

    fn bm1362_misc_ctrl_triple_write_serial(serial: &SerialChainBackend, value: u32) -> Result<()> {
        for i in 0..3 {
            serial
                .send_write_reg_broadcast_bm1397plus(0x18, value)
                .with_context(|| format!("BM1362 MiscCtrl triple-write attempt {}/3", i + 1))?;
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn bm1362_misc_ctrl_triple_write_chip_serial(
        serial: &SerialChainBackend,
        chip_addr: u8,
        value: u32,
    ) -> Result<()> {
        for i in 0..3 {
            serial
                .send_write_reg_bm1397plus(chip_addr, 0x18, value)
                .with_context(|| {
                    format!(
                        "BM1362 MiscCtrl chip 0x{:02X} triple-write attempt {}/3",
                        chip_addr,
                        i + 1
                    )
                })?;
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn bm1368_addr_interval(chip_count: u8) -> u8 {
        if chip_count == 108 {
            BM1368_ADDRESS_INTERVAL
        } else {
            (256u16 / chip_count.max(1) as u16) as u8
        }
    }

    fn bm1368_chain_inactive(serial: &SerialChainBackend) -> Result<()> {
        for i in 0..3 {
            serial.send_chain_inactive_bm1397plus()?;
            std::thread::sleep(Duration::from_millis(10));
            debug!("BM1368 chain inactive {}/3", i + 1);
        }
        Ok(())
    }

    fn bm1368_write_fixture_registers(serial: &SerialChainBackend) -> Result<()> {
        serial.send_write_reg_broadcast_bm1397plus(0x54, ANALOG_MUX_VALUE)?;
        serial.send_write_reg_broadcast_bm1397plus(0xA8, BM1368_REG_A8_BCAST)?;
        serial.send_write_reg_broadcast_bm1397plus(0x18, BM1368_MISC_CTRL_BCAST)?;
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_1)?;
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_2)?;
        serial.send_write_reg_broadcast_bm1397plus(0x14, BM1368_TICKET_MASK)?;
        serial.send_write_reg_broadcast_bm1397plus(0x58, BM1368_IO_DRIVER)?;
        serial.send_write_reg_bm1397plus(
            0x00,
            BM1368_UART_RELAY_REG,
            BM1368_UART_RELAY_12_DOMAIN,
        )?;
        Ok(())
    }

    fn bm1368_core_reset(serial: &SerialChainBackend, chip_count: u8) -> Result<()> {
        let addr_interval = Self::bm1368_addr_interval(chip_count);
        for i in 0..chip_count {
            let chip_addr = i.saturating_mul(addr_interval);
            serial.send_write_reg_bm1397plus(chip_addr, 0xA8, BM1368_REG_A8_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x18, BM1368_MISC_CTRL_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_1)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_2)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_3)?;
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Full BM1362 ASIC chain initialization over serial UART.
    fn init_asic_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
    ) -> Result<SerialChainBackend> {
        info!(
            "=== BM1362 ASIC INIT ({} chips, {} MHz target) ===",
            chip_count, target_freq_mhz
        );

        // Hot-start baud reset: bring ASICs back to 115200 if they're at a fast baud.
        // When the direct diagnostic owns APW/PIC power and pulses the hashboard
        // reset after rail stabilization, this extra baud-reset can perturb the
        // clean 115200 baseline we are trying to measure.
        if !Self::bm1362_skip_post_power_reset() {
            info!("BM1362 post-power reset ordering enabled; skipping hot-start baud reset");
        } else {
            warn!("BM1362 post-power reset explicitly skipped; using hot-start baud reset");
            Self::reset_asic_baud(serial_device);
        }

        // Open serial at 115200 for init commands
        info!("Opening {} at 115200 baud for init", serial_device);
        let mut serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;

        // Flush any stale data
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));

        // am2 Braiins glitch monitor mirror diagnostics (Braiins-am2 bitstream only).
        // W13.B1 (2026-05-10): the `0x43D00000` window is reclassified as a
        // diagnostic-only Braiins-am2 status mirror. R6-7 keeps the BM1362
        // 0x2C/0x34 candidate relay broadcasts lab-gated.
        if let Some(slot) = Self::am2_slot_from_serial_device(serial_device) {
            let phys_idx = slot + 1;
            if dcentrald_hal::glitch_monitor::chain_glitch_status_offset(phys_idx).is_some() {
                let braiins_glitch_uio: u8 = std::env::var("DCENT_BRAIINS_GLITCH_UIO")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(18);
                match dcentrald_hal::glitch_monitor::BraiinsGlitchMonitor::open(braiins_glitch_uio)
                {
                    Ok(monitor) => match monitor.read_chain_uart_relay_mirror(phys_idx) {
                        Ok(value) => info!(
                            phys_idx,
                            serial_device,
                            value = format_args!("0x{:08X}", value),
                            "am2 Braiins glitch mirror observed for direct BM1362 path (diagnostic only)"
                        ),
                        Err(e) => warn!(
                            phys_idx,
                            serial_device,
                            error = %e,
                            "am2 Braiins glitch mirror read failed — continuing (non-fatal)"
                        ),
                    },
                    Err(e) => warn!(
                        error = %e,
                        "am2 Braiins glitch mirror open failed (Braiins-am2 only) — continuing (non-fatal)"
                    ),
                }
            } else {
                warn!(
                    slot,
                    serial_device,
                    "am2 Braiins glitch mirror: slot {} has no known mirror offset (populated slots: 1=chain1, 2=chain4)",
                    slot
                );
            }
        }

        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_open_115200", 150)?;
        }

        // === Phase A: Healthy traced 115200-baud init ===

        // Step 1: First healthy chain writes use BM1397+/BM1362 headers.
        serial.send_write_reg_broadcast_bm1397plus(0xA8, INIT_CONTROL_BCAST)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::bm1362_misc_ctrl_triple_write_serial(&serial, MISC_CONTROL_INIT)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));
        info!("Step 1: Healthy traced 115200 broadcast preamble applied");
        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_115200_preamble", 150)?;
        }

        // Step 2: Healthy stock init issues CHAIN_INACTIVE three times before
        // re-addressing the chain.
        info!("Step 2: Chain Inactive x3");
        for i in 0..3 {
            serial.send_chain_inactive_bm1397plus()?;
            std::thread::sleep(Duration::from_millis(300));
            if Self::bm1362_early_115200_probe_enabled() {
                let stage = format!("after_chain_inactive_{}", i + 1);
                Self::log_bm1362_probe_stage(&mut serial, &stage, 150)?;
            }
        }

        // Step 3: Assign addresses with BM1397+/BM1362 framing.
        info!("Step 3: Assigning addresses to {} chips", chip_count);
        let addr_interval = 256u16 / (chip_count as u16).max(1);
        for i in 0..chip_count as u16 {
            let addr = (i * addr_interval) as u8;
            serial.send_set_address_bm1397plus(addr)?;
            if Self::bm1362_early_115200_probe_enabled()
                && (i == 0 || i % 16 == 15 || i + 1 == chip_count as u16)
            {
                std::thread::sleep(Duration::from_millis(20));
                let stage = format!("after_setaddr_{:03}", i + 1);
                Self::log_bm1362_probe_stage(&mut serial, &stage, 150)?;
            }
            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
        info!(
            "Enumeration complete: {} chips addressed (interval={})",
            chip_count, addr_interval
        );

        // Step 4: Remaining traced 115200-baud broadcast block before the fast-baud switch.
        serial.send_write_reg_broadcast_bm1397plus(0x3C, CORE_REG_HASH_CLK)?;
        std::thread::sleep(Duration::from_millis(10));
        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_reg_3c_hash_clk", 150)?;
        }
        serial.send_write_reg_broadcast_bm1397plus(0x3C, CORE_REG_CLK_DELAY)?;
        std::thread::sleep(Duration::from_millis(10));
        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_reg_3c_clk_delay", 150)?;
        }
        serial.send_write_reg_broadcast_bm1397plus(0x54, ANALOG_MUX_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));
        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_reg_54_analog_mux", 150)?;
        }
        serial.send_write_reg_broadcast_bm1397plus(0x58, IO_DRIVER_NORMAL)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x14, TICKET_MASK_256)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x10, NONCE_RANGE_126)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::maybe_write_bm1362_uart_relay(&serial, "bm1362_step4_115200")?;
        info!("Step 4: Healthy traced 115200 broadcast block complete");
        if Self::bm1362_early_115200_probe_enabled() {
            Self::log_bm1362_probe_stage(&mut serial, "after_reg_58_stock_0x52_probe", 150)?;
        }

        // Step 5: Healthy chain4 trace shows the PLL divider / param pair twice
        // before the FastUART register write.
        Self::log_bm1362_probe_stage(&mut serial, "pre_pll_preamble", 150)?;
        if Self::bm1362_stop_after_pre_pll_probe_enabled() {
            return Err(anyhow::anyhow!(
                "DCENT_BM1362_STOP_AFTER_PRE_PLL_PROBE requested; stopping before PLL and fast-baud writes",
            ));
        }
        let (traced_pll_param, final_freq_mhz) = if target_freq_mhz == 525 {
            (BM1362_TRACE_PLL_PARAM_525, 525)
        } else {
            let (pll_reg, actual_freq) = bm1362_pll_lookup(target_freq_mhz.clamp(400, 597));
            warn!(
                requested_mhz = target_freq_mhz,
                traced_only_mhz = 525,
                fallback_pll = format_args!("0x{:08X}", pll_reg),
                fallback_actual_mhz = actual_freq,
                "BM1362 traced init only has a live PLL param for 525 MHz; falling back to lookup-derived PLL value"
            );
            (pll_reg, actual_freq)
        };
        serial.send_write_reg_broadcast_bm1397plus(
            BM1362_PLL0_DIVIDER_REG,
            BM1362_TRACE_PLL0_DIVIDER,
        )?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x08, traced_pll_param)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::log_bm1362_probe_stage(&mut serial, "post_pll_pair_1", 150)?;
        serial.send_write_reg_broadcast_bm1397plus(
            BM1362_PLL0_DIVIDER_REG,
            BM1362_TRACE_PLL0_DIVIDER,
        )?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x08, traced_pll_param)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::log_bm1362_probe_stage(&mut serial, "post_pll_pair_2", 150)?;
        info!(
            pll = format_args!("0x{:08X}", traced_pll_param),
            "Step 5: traced PLL preamble applied"
        );

        // Step 6: ASIC and host fast-baud switch.
        serial.send_write_reg_broadcast_bm1397plus(0x28, FAST_UART_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::bm1362_misc_ctrl_triple_write_serial(&serial, MISC_CONTROL_INIT)?;
        std::thread::sleep(Duration::from_millis(10));
        Self::log_bm1362_probe_stage(&mut serial, "post_fast_uart_reg_pre_host_switch", 150)?;
        serial.set_baud(fast_baud())?;
        std::thread::sleep(Duration::from_millis(1000));
        info!(
            pll = format_args!("0x{:08X}", traced_pll_param),
            fast_uart = format_args!("0x{:08X}", FAST_UART_VALUE),
            misc_ctrl = format_args!("0x{:08X}", MISC_CONTROL_INIT),
            "Step 6: Host baud upgraded to 3.125 Mbaud"
        );

        Self::maybe_write_bm1362_uart_relay(&serial, "bm1362_step6_fast_baud")?;

        // === DIAGNOSTIC: Verify small-command RX still works immediately after
        // the fast-baud transition. If this probe dies, the remaining blocker is
        // likely baud/transport or mining-ready state, not the full work frame.
        // BM13xx register-read responses are 11 bytes total on wire, i.e. 9
        // bytes after the 0xAA 0x55 preamble. `read_all_responses()` expects
        // the body length, not the total frame length.
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_baud_responses = serial.read_all_responses(500)?;
        info!(
            "POST-BAUD PROBE: {} responses (after step 6 fast-baud switch)",
            post_baud_responses.len()
        );

        // === Phase B: High-baud mining-ready configuration ===

        // Step 7: Healthy stock init runs the full per-chip A8 / 18 / 3C x3 loop
        // only after the fast-baud transition.
        info!(
            "Step 7: Per-chip init loop after fast-baud switch ({} chips, 5 regs each)",
            chip_count
        );
        for i in 0..chip_count {
            let chip_addr = (i as u16 * addr_interval) as u8;

            serial.send_write_reg_bm1397plus(chip_addr, 0xA8, INIT_CONTROL_PER_CHIP)?;
            Self::bm1362_misc_ctrl_triple_write_chip_serial(&serial, chip_addr, MISC_CONTROL_INIT)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_HASH_CLK)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_CLK_DELAY)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, CORE_REG_UNKNOWN)?;

            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(SERIAL_PACE_MIN_MS));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        info!("Step 7: Per-chip init complete");

        // Healthy stock chain4 tracing has a clear post-baud per-chip loop, but
        // did not show an additional trailing broadcast block for BM1362 here.
        // On `a lab unit`, the command path stays alive immediately after the baud
        // switch and then goes dead again later in our synthetic tail. Keep the
        // direct path as close to stock as possible and defer any extra mining-
        // ready broadcasts until they are proven necessary by live parity.
        info!("Step 8: Skipping synthetic post-baud nonce-range/version-mask tail on BM1362 direct path");

        // === DIAGNOSTIC: Verify small-command RX still works after the full
        // high-baud mining-ready register block.
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_final_responses = serial.read_all_responses(500)?;
        info!(
            "POST-FINAL PROBE: {} responses (after post-baud BM1362 direct config)",
            post_final_responses.len()
        );

        info!(
            "=== BM1362 INIT COMPLETE — {} chips at {} MHz ===",
            chip_count, final_freq_mhz
        );

        // Flush any stale register responses from init commands before nonce collection
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        let _ = serial.flush_io();
        info!("Serial RX flushed after init");

        Ok(serial)
    }

    /// BM1368 ASIC init via serial (S21, T21).
    /// Uses BM1368-specific register values from ESP-Miner + bm1368.rs.
    /// Key differences from BM1362: different MISC_CTRL, CORE_REG, IO_DRIVER,
    /// FAST_UART (1 Mbaud not 3.125M), and per-chip register values.
    fn init_bm1368_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
    ) -> Result<SerialChainBackend> {
        info!(
            "=== BM1368 ASIC INIT ({} chips, {} MHz target) ===",
            chip_count, target_freq_mhz
        );

        Self::reset_asic_baud(serial_device);

        // Open serial at 115200 for init commands
        info!("Opening {} at 115200 baud for init", serial_device);
        let mut serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));

        // === Phase A: Enumeration (at 115200 baud) ===

        // === ALL COMMANDS USE BM1397+ HEADERS (0x51/0x41/0x53/0x40) ===
        // BM1387 uses 0x58/0x48/0x55/0x41 which are CMD_SETCONFIG — incompatible!

        // Step 1: Version mask x4 (BM1368 needs 4, not 3 like BM1362)
        for i in 0..4 {
            serial.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_VALUE)?;
            std::thread::sleep(Duration::from_millis(5));
            info!("Step 1: Version mask write {}/4", i + 1);
        }

        // Step 2: Chain Inactive x3 (fixture order)
        info!("Step 2: Chain Inactive x3");
        Self::bm1368_chain_inactive(&serial)?;

        // Step 3: Assign addresses (BM1397+ header 0x40)
        info!("Step 3: Assigning addresses to {} chips", chip_count);
        let addr_interval = Self::bm1368_addr_interval(chip_count) as u16;
        for i in 0..chip_count as u16 {
            let addr = (i * addr_interval) as u8;
            serial.send_set_address_bm1397plus(addr)?;
            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
        info!(
            "Enumeration: {} chips addressed (interval={})",
            chip_count, addr_interval
        );

        // === DIAGNOSTIC: Probe chips after enumeration, before any register writes ===
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let pre_responses = serial.read_all_responses(500)?;
        info!(
            "PRE-INIT PROBE: {} chip responses (before register writes)",
            pre_responses.len()
        );

        // === Phase B: Register Configuration (BM1368-specific, BM1397+ headers) ===

        // Step 4a-4h: Fixture broadcast register block
        Self::bm1368_write_fixture_registers(&serial)?;
        std::thread::sleep(Duration::from_millis(10));
        info!("Step 4: Fixture broadcast registers applied");
        info!("Step 4a: AnalogMux = 0x{:08X}", ANALOG_MUX_VALUE);
        info!("Step 4b: REG_A8 = 0x{:08X}", BM1368_REG_A8_BCAST);

        // Step 4b: Misc Control broadcast
        serial.send_write_reg_broadcast_bm1397plus(0x18, BM1368_MISC_CTRL_BCAST)?;
        std::thread::sleep(Duration::from_millis(10));
        info!("Step 4b: MiscCtrl = 0x{:08X}", BM1368_MISC_CTRL_BCAST);

        // Step 4c: Core register control — first write
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_1)?;
        info!("Step 4c: CoreReg[1] = 0x{:08X}", BM1368_CORE_REG_1);

        // Step 4d: Core register control — second write
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_2)?;
        info!("Step 4d: CoreReg[2] = 0x{:08X}", BM1368_CORE_REG_2);

        // Step 4e: Ticket mask init (BM1368 extra)
        serial.send_write_reg_broadcast_bm1397plus(0x14, BM1368_TICKET_MASK)?;
        info!("Step 4e: TicketMask init = 0x{:08X}", BM1368_TICKET_MASK);

        // Step 4f: Analog mux (temp diode)
        serial.send_write_reg_broadcast_bm1397plus(0x54, ANALOG_MUX_VALUE)?;
        info!("Step 4f: AnalogMux = 0x{:08X}", ANALOG_MUX_VALUE);

        // Step 4g: IO driver strength
        serial.send_write_reg_broadcast_bm1397plus(0x58, BM1368_IO_DRIVER)?;
        info!("Step 4g: IODriver = 0x{:08X}", BM1368_IO_DRIVER);

        std::thread::sleep(Duration::from_millis(10));

        // === DIAGNOSTIC: Probe after broadcast registers ===
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_bcast = serial.read_all_responses(500)?;
        info!(
            "POST-BROADCAST PROBE: {} responses (after 4a-4g)",
            post_bcast.len()
        );

        // Step 5: Per-chip register init (5 writes per chip, BM1397+ single-chip header 0x41)
        info!("Step 5: Per-chip init loop ({} chips)", chip_count);
        for i in 0..chip_count {
            let chip_addr = (i as u16 * addr_interval) as u8;
            serial.send_write_reg_bm1397plus(chip_addr, 0xA8, BM1368_REG_A8_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x18, BM1368_MISC_CTRL_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_1)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_2)?;
            serial.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_3)?;
            // 20ms per chip (ESP-Miner uses 500ms; 20ms is a compromise for Linux serial)
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(50));
        info!("Step 5: Per-chip init complete");

        // Step 6: Difficulty mask
        serial.send_write_reg_broadcast_bm1397plus(0x14, BM1368_TICKET_MASK)?;
        info!("Step 6: TicketMask = 0x{:08X}", BM1368_TICKET_MASK);

        // === DIAGNOSTIC: Probe after per-chip config ===
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_perchip = serial.read_all_responses(500)?;
        info!(
            "POST-PERCHIP PROBE: {} responses (after step 5-6)",
            post_perchip.len()
        );

        // Step 7: PLL frequency ramp (at 115200 baud — match ESP-Miner order)
        let target_freq = target_freq_mhz.clamp(50, 800);
        info!("Step 7: PLL ramp to {} MHz (at 115200)", target_freq);
        let mut current_freq: u16 = 200;
        while current_freq < target_freq {
            let (pll_reg, actual_freq) = bm1368_pll_search(current_freq);
            serial.send_write_reg_broadcast_bm1397plus(0x08, pll_reg)?;
            std::thread::sleep(Duration::from_millis(100));
            debug!("PLL ramp: {} MHz (0x{:08X})", actual_freq, pll_reg);
            current_freq = current_freq.saturating_add(25);
        }
        let (final_pll, final_freq) = bm1368_pll_search(target_freq);
        serial.send_write_reg_broadcast_bm1397plus(0x08, final_pll)?;
        std::thread::sleep(Duration::from_millis(100));
        info!(
            "Step 7: PLL final = {} MHz (0x{:08X})",
            final_freq, final_pll
        );

        // Step 8: Hash counting / nonce range (at 115200 — match ESP-Miner order)
        serial.send_write_reg_broadcast_bm1397plus(0x10, NONCE_RANGE_108)?;
        std::thread::sleep(Duration::from_millis(10));
        info!(
            "Step 8: HashCounting = 0x{:08X} (108 chips)",
            NONCE_RANGE_108
        );

        // Step 9: Final version mask (at 115200)
        serial.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));
        info!("Step 9: Final version mask = 0x{:08X}", VERSION_MASK_VALUE);

        // === Phase C: SKIP BAUD UPGRADE — mine at 115200 ===
        // FAST_UART baud switch fails on Amlogic (can't produce 3,125,000).
        // 115200 is fast enough: 131 frames/sec max, we need only ~20/sec.
        // TODO: figure out Amlogic meson_uart PLL config for exact 3,125,000
        info!("Step 10: SKIPPING baud upgrade — mining at 115200 (cold boot compatible)");

        // === POST-INIT: Verify chips still respond at 115200 ===
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);
        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_responses = serial.read_all_responses(500)?;
        info!(
            "Post-init probe: {} chip responses at 115200",
            post_responses.len()
        );

        info!(
            "=== BM1368 INIT COMPLETE — {} chips at {} MHz, 115200 baud (cold boot) ===",
            chip_count, final_freq
        );

        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        let _ = serial.flush_io();
        info!("Serial RX flushed after init");

        Ok(serial)
    }

    fn init_bm1366_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
    ) -> Result<SerialChainBackend> {
        // Phase H.13: load operator-tunable rambo_mode_max_bad_responses
        // from /etc/dcentrald-experimental.toml (default 0 = strict).
        // Bosminer's "Rambo mode" — proceed with chain enumeration even
        // when some chips fail to respond. Useful for partially-faulty
        // hashboards (e.g. .78 chain1 sees 8/77 chips: with rambo=8 the
        // chain still mines on the 8 working chips; without it dcentrald
        // refuses to start).
        let experimental = crate::experimental::ExperimentalConfig::load();
        let rambo_max = experimental.rambo_mode_max_bad_responses;

        info!(
            "=== BM1366 ASIC INIT (experimental, {} chips, {} MHz target, rambo_max={}) ===",
            chip_count, target_freq_mhz, rambo_max
        );

        Self::reset_asic_baud(serial_device);

        let mut serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);

        for _ in 0..3 {
            serial.send_write_reg_broadcast_bm1397plus(0xA4, BM1366_VERSION_MASK_VALUE)?;
            std::thread::sleep(Duration::from_millis(5));
        }

        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let pre_responses = serial.read_all_responses(500)?;
        // Rambo gate: tolerate up to `rambo_max` missing chips.
        // chip_count.saturating_sub(rambo_max) = minimum acceptable response count.
        let min_required = (chip_count as u16).saturating_sub(rambo_max as u16) as usize;
        if pre_responses.len() < min_required.max(1) {
            anyhow::bail!(
                "Only {} of {} BM1366 ASICs responded to GetAddress on {} (rambo_max={} → min_required={}); chain looks dead",
                pre_responses.len(),
                chip_count,
                serial_device,
                rambo_max,
                min_required.max(1),
            );
        }
        if pre_responses.len() < chip_count as usize {
            tracing::warn!(
                got = pre_responses.len(),
                expected = chip_count,
                rambo_max,
                "BM1366 partial chain enumeration — proceeding under rambo_mode tolerance",
            );
        }

        serial.send_write_reg_broadcast_bm1397plus(0xA8, BM1366_REG_A8_BCAST)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_broadcast_bm1397plus(0x18, BM1366_MISC_CTRL_BCAST)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_chain_inactive_bm1397plus()?;
        std::thread::sleep(Duration::from_millis(10));

        let addr_interval = (256u16 / chip_count.max(1) as u16) as u8;
        for i in 0..chip_count {
            serial.send_set_address_bm1397plus((i as u16 * addr_interval as u16) as u8)?;
            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(Duration::from_millis(10));

        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1366_CORE_REG_HASH_CLOCK)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1366_CORE_REG_CLOCK_DELAY)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_broadcast_bm1397plus(0x14, BM1366_TICKET_MASK)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_broadcast_bm1397plus(0x54, BM1366_ANALOG_MUX)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_broadcast_bm1397plus(0x58, BM1366_IO_DRIVER)?;
        std::thread::sleep(Duration::from_millis(5));
        serial.send_write_reg_bm1397plus(0x00, 0x2C, BM1366_UART_RELAY)?;
        std::thread::sleep(Duration::from_millis(5));

        for i in 0..chip_count {
            let addr = (i as u16 * addr_interval as u16) as u8;
            serial.send_write_reg_bm1397plus(addr, 0xA8, BM1366_REG_A8_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(addr, 0x18, BM1366_MISC_CTRL_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1366_CORE_REG_HASH_CLOCK)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1366_CORE_REG_CLOCK_DELAY)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1366_CORE_REG_UNKNOWN)?;
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));

        let (pll_reg, actual_freq) = bm1366_pll_search(target_freq_mhz);
        serial.send_write_reg_broadcast_bm1397plus(0x08, pll_reg)?;
        std::thread::sleep(Duration::from_millis(100));

        let hash_counting = if chip_count >= 100 {
            BM1366_HASH_COUNTING_S19XP
        } else {
            BM1366_HASH_COUNTING_S19K
        };
        serial.send_write_reg_broadcast_bm1397plus(0x10, hash_counting)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0xA4, BM1366_VERSION_MASK_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));

        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_responses = serial.read_all_responses(500)?;
        if post_responses.is_empty() {
            anyhow::bail!(
                "BM1366 init completed but no ASICs responded to post-init GetAddress on {}",
                serial_device
            );
        }

        info!(
            "=== BM1366 INIT COMPLETE — {} chips at {} MHz, 115200 baud ===",
            chip_count, actual_freq
        );
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        let _ = serial.flush_io();
        Ok(serial)
    }

    fn init_bm1370_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
    ) -> Result<SerialChainBackend> {
        info!(
            "=== BM1370 ASIC INIT (experimental, {} chips, {} MHz target) ===",
            chip_count, target_freq_mhz
        );

        Self::reset_asic_baud(serial_device);

        let mut serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        serial.set_response_len(BM13XX_CMD_RESP_BODY_LEN);

        for _ in 0..4 {
            serial.send_write_reg_broadcast_bm1397plus(0xA4, BM1370_VERSION_MASK_VALUE)?;
            std::thread::sleep(Duration::from_millis(5));
        }

        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let pre_responses = serial.read_all_responses(500)?;
        if pre_responses.is_empty() {
            anyhow::bail!(
                "No BM1370 ASICs responded to GetAddress on {} before init",
                serial_device
            );
        }

        serial.send_write_reg_broadcast_bm1397plus(0xA8, BM1370_REG_A8_BCAST)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x18, BM1370_MISC_CTRL_BCAST)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_chain_inactive_bm1397plus()?;
        std::thread::sleep(Duration::from_millis(10));

        let addr_interval = (256u16 / chip_count.max(1) as u16) as u8;
        for i in 0..chip_count {
            serial.send_set_address_bm1397plus((i as u16 * addr_interval as u16) as u8)?;
            if i % 16 == 15 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(Duration::from_millis(10));

        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1370_CORE_REG_1)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1370_CORE_REG_2)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x14, BM1370_TICKET_MASK)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x58, BM1370_IO_DRIVER)?;
        std::thread::sleep(Duration::from_millis(10));

        for i in 0..chip_count {
            let addr = (i as u16 * addr_interval as u16) as u8;
            serial.send_write_reg_bm1397plus(addr, 0xA8, BM1370_REG_A8_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(addr, 0x18, BM1370_MISC_CTRL_PER_CHIP)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1370_CORE_REG_1)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1370_CORE_REG_2)?;
            serial.send_write_reg_bm1397plus(addr, 0x3C, BM1370_CORE_REG_3)?;
            std::thread::sleep(Duration::from_millis(5));
        }

        serial.send_write_reg_broadcast_bm1397plus(0xB9, BM1370_MISC_SETTINGS_B9)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x54, BM1370_ANALOG_MUX)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0xB9, BM1370_MISC_SETTINGS_B9)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0x3C, BM1370_CORE_REG_EXTRA)?;
        std::thread::sleep(Duration::from_millis(10));

        let (pll_reg, actual_freq) = bm1370_pll_search(target_freq_mhz);
        serial.send_write_reg_broadcast_bm1397plus(0x08, pll_reg)?;
        std::thread::sleep(Duration::from_millis(100));
        serial.send_write_reg_broadcast_bm1397plus(0x10, BM1370_HASH_COUNTING)?;
        std::thread::sleep(Duration::from_millis(10));
        serial.send_write_reg_broadcast_bm1397plus(0xA4, BM1370_VERSION_MASK_VALUE)?;
        std::thread::sleep(Duration::from_millis(10));

        let _ = serial.send_get_address_bm1397plus();
        std::thread::sleep(Duration::from_millis(200));
        let post_responses = serial.read_all_responses(500)?;
        if post_responses.is_empty() {
            anyhow::bail!(
                "BM1370 init completed but no ASICs responded to post-init GetAddress on {}",
                serial_device
            );
        }

        info!(
            "=== BM1370 INIT COMPLETE — {} chips at {} MHz, 115200 baud ===",
            chip_count, actual_freq
        );
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        let _ = serial.flush_io();
        Ok(serial)
    }

    /// BM1398 ASIC init via serial (S19 Pro).
    /// Simplified init: enumerate at 115200, configure registers, upgrade to 3.125M.
    /// No PLL3/FastUART — uses default 25 MHz CLKI for baud clock.
    fn init_bm1398_chain(
        serial_device: &str,
        chip_count: u8,
        target_freq_mhz: u16,
    ) -> Result<SerialChainBackend> {
        info!(
            "=== BM1398 ASIC INIT ({} chips, {} MHz target) ===",
            chip_count, target_freq_mhz
        );

        // Open serial at 115200 for init commands
        let serial = SerialChainBackend::open(0, serial_device, 115_200)
            .context("Failed to open serial port at 115200")?;
        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));

        // Step 1: Chain Inactive (BM1397+ format: header 0x53)
        info!("Step 1: Chain Inactive (BM1397+)");
        serial.send_chain_inactive_bm1397plus()?;
        std::thread::sleep(Duration::from_millis(10));

        // Step 2: Assign addresses
        info!("Step 2: Assigning addresses to {} chips", chip_count);
        let addr_interval = 256u16 / (chip_count as u16).max(1);
        for i in 0..chip_count as u16 {
            let addr = (i * addr_interval) as u8;
            serial.send_set_address_bm1397plus(addr)?;
        }
        std::thread::sleep(Duration::from_millis(10));
        info!(
            "Addresses assigned: {} chips, spacing {}",
            chip_count, addr_interval
        );

        // Step 2b: GetAddress scan — verify ASICs respond
        info!("Step 2b: GetAddress scan — verifying ASICs respond");
        let _ = serial.flush_io();
        serial.send_get_address_bm1397plus()?;
        std::thread::sleep(Duration::from_millis(500));
        let responses = serial.read_all_responses(500)?;
        if responses.is_empty() {
            anyhow::bail!("NO chips responded to GetAddress on {} — hash board may not be connected or powered", serial_device);
        } else {
            info!(
                "GetAddress: {} response(s) — ASICs are alive!",
                responses.len()
            );
        }

        // Step 3: Clock Order Control 0/1 = 0
        serial.send_write_reg_broadcast(0x80, BM1398_CLK_ORDER_CTRL)?;
        serial.send_write_reg_broadcast(0x84, BM1398_CLK_ORDER_CTRL)?;
        std::thread::sleep(Duration::from_millis(5));

        // Step 4: Ordered Clock Enable = 1
        serial.send_write_reg_broadcast(0x20, BM1398_ORDERED_CLK_EN)?;
        std::thread::sleep(Duration::from_millis(5));

        // Step 5: staged core-register control recovered independently from
        // the stock NBP1901 miner and repair jig.
        for write in dcentrald_api_types::bm1398_protocol::BM1398_PROVEN_CORE_WRITES {
            serial.send_write_reg_broadcast(write.register, write.value)?;
            std::thread::sleep(Duration::from_millis(5));
        }

        // Step 6: TicketMask (difficulty 256)
        serial.send_write_reg_broadcast(0x14, BM1398_TICKET_MASK)?;
        info!("TicketMask = 0x{:08X} (difficulty 256)", BM1398_TICKET_MASK);
        std::thread::sleep(Duration::from_millis(5));

        // Step 7: MiscCtrl at 115200 (BT8D=26)
        serial.send_write_reg_broadcast(0x18, BM1398_MISC_CTRL_INIT)?;
        std::thread::sleep(Duration::from_millis(10));

        // Step 8: PLL0 (frequency)
        let (pll_reg, actual_freq) = bm1398_pll_lookup(target_freq_mhz);
        for _ in 0..2 {
            serial.send_write_reg_broadcast(0x70, 0x0F0F_0F00)?; // PLL0 Divider preconfig
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..2 {
            serial.send_write_reg_broadcast(0x08, pll_reg)?; // PLL0 Parameter
            std::thread::sleep(Duration::from_millis(10));
        }
        info!("PLL0 = 0x{:08X} ({} MHz)", pll_reg, actual_freq);
        std::thread::sleep(Duration::from_millis(20)); // PLL lock time

        // Step 9: Baud upgrade to 3.125 MHz
        // MiscCtrl BT8D=0 → ASIC baud = 25MHz/(1*8) = 3.125 MHz (default CLKI, no PLL3)
        serial.send_write_reg_broadcast(0x18, BM1398_MISC_CTRL_FAST)?;
        std::thread::sleep(Duration::from_millis(200));
        info!("MiscCtrl = 0x6031 (BT8D=0 → 3.125 MHz). Upgrading serial baud...");

        // Switch serial port to 3.125 Mbaud
        serial.set_baud(fast_baud())?;
        std::thread::sleep(Duration::from_millis(100));
        info!("Serial baud = 3.125 Mbaud");

        // Re-send MiscCtrl at new baud (CE expert: S9 Step 5b pattern)
        serial.send_write_reg_broadcast(0x18, BM1398_MISC_CTRL_FAST)?;
        std::thread::sleep(Duration::from_millis(10));
        info!("MiscCtrl re-sent at 3.125M baud");

        info!(
            "=== BM1398 INIT COMPLETE — {} chips at {} MHz, 3.125M baud ===",
            chip_count, actual_freq
        );

        let _ = serial.flush_io();
        std::thread::sleep(Duration::from_millis(50));
        let _ = serial.flush_io();

        Ok(serial)
    }

    pub async fn run(&mut self) -> Result<()> {
        let serial_device = self
            .config
            .mining
            .serial_device
            .clone()
            .unwrap_or_else(|| "/dev/ttyS2".to_string());
        let model_hint = self.config.mining.model.as_deref();
        let nopic = is_nopic(&self.config);
        let (resolved_chip_id, chip_count) = resolve_native_serial_identity_and_geometry(
            model_hint,
            self.config.mining.serial_chip_count,
        )?;
        let target_freq = self.config.mining.frequency_mhz;
        let passthrough = self.config.mining.passthrough;

        // `resolve_native_serial_identity_and_geometry` is the production
        // identity boundary. Per-family dispatch is derived only from its
        // catalog chip ID; chip count is geometry and never selects a driver.
        let is_bm1398 = resolved_chip_id == 0x1398;
        let is_bm1368 = resolved_chip_id == 0x1368;
        let is_bm1366 = resolved_chip_id == 0x1366;
        let is_bm1370 = resolved_chip_id == 0x1370;
        let is_bm1362 = resolved_chip_id == 0x1362;
        let native_nopic_power_owner = !passthrough && (is_bm1368 || is_bm1370);
        if is_bm1366 && nopic && !passthrough {
            anyhow::bail!(
                "native BM1366 NoPic mining is refused until S19K/S19 XP power adoption, retained bus-1 ownership, and watchdog closeout share one admitted lifecycle"
            );
        }
        let nopic_watchdog_liveness = SafetyLiveness::default();
        // Declared before the PSU guard so ordinary unwinding cuts GPIO437
        // before dropping the watchdog command owner. The retained bus-1
        // owner is declared first so the PSU guard drops (and cuts power)
        // before the final management-fabric handle can disappear.
        let mut amlogic_admission: Option<dcentrald_hal::platform::amlogic::AmlogicNoPicAdmission> =
            None;
        let mut amlogic_power_thermal: Option<
            dcentrald_hal::platform::amlogic::AmlogicPowerThermalService,
        > = None;
        let mut amlogic_fan: Option<Arc<dyn FanAccess>> = None;
        let mut nopic_fan_safety = FanTachSafety::with_minimum_credible_rpm(
            DEFAULT_FAN_BELOW_MINIMUM_FAILURE_TICKS,
            dcentrald_hal::platform::amlogic::REQUIRED_AIRFLOW_MIN_RPM,
        );
        let mut nopic_watchdog: Option<SafetyWatchdogOwner> = None;
        let mut nopic_psu_guard = NoPicPsuGuard::new();
        let mut nopic_energized_at: Option<Instant> = None;
        let mut am2_power = Am2PsuRuntimeGuard::new();
        // Hardware-worker cancellation is independent from the process token.
        // The main loop observes process shutdown, admits watchdog Teardown,
        // and only then asks this owner to stop its actors.
        let mut runtime_threads = RuntimeThreadGuard::new(CancellationToken::new());
        let mut bm1362_detected_pic_fw: Option<u8> = None;
        let hw_difficulty = hardware_difficulty_for_serial_family(resolved_chip_id)?;
        let system_subtype = dcentrald_hal::platform::subtype::read_subtype();
        let bhb56_endpoint_capability_required = is_bm1366 && !nopic && !passthrough;
        if bhb56_endpoint_capability_required
            && !subtype_requires_bhb56_endpoint_capability(system_subtype.as_deref())
        {
            anyhow::bail!(
                "BM1366 PIC voltage route requires exact AMLCtrl_BHB56-family system identity; refusing model/config-only dsPIC authorization (observed subtype: {})",
                system_subtype.as_deref().unwrap_or("<missing>")
            );
        }

        // Capability issuance MUST precede the serialized service: discovery
        // performs non-payload probes with a short-lived raw fd, while the
        // service becomes the sole bus owner immediately after it is spawned.
        // Exact BHB56 identity makes this route mandatory; any missing bus,
        // address, or ACK aborts before protocol bytes can be selected.
        let pending_bhb56_endpoints = if bhb56_endpoint_capability_required {
            let mut endpoints = Vec::with_capacity(S19_DSPIC_ADDRS.len());
            for address in S19_DSPIC_ADDRS {
                endpoints.push(
                    dcentrald_hal::platform::discover_system_voltage_controller_endpoint(
                        0, address,
                    )
                    .with_context(|| {
                        format!(
                            "BHB56 endpoint discovery failed on I2C bus 0 address 0x{address:02X}"
                        )
                    })?,
                );
            }
            endpoints
        } else {
            Vec::new()
        };
        // The direct BM1362 path is shared by several control boards. Only an
        // exact DCENT_OS `am2-s19j` image may make AM2 endpoint authority
        // mandatory here; every non-target retains the existing AM3-BB or raw
        // compatibility path. Planning reads system identity before the
        // serialized service owns the bus and emits no controller traffic.
        let pending_am2_bm1362_plan = if is_bm1362 && !nopic && !passthrough {
            dcentrald_hal::platform::try_discover_system_am2_controller_plan(std::slice::from_ref(
                &serial_device,
            ))?
        } else {
            None
        };
        let bm1362_pic_addr = Self::bm1362_pic_addr_for_serial_runtime(
            &serial_device,
            is_bm1362,
            nopic,
            passthrough,
        )?;
        if let Some(addr) = bm1362_pic_addr {
            info!(
                serial_device = %serial_device,
                pic_addr = format_args!("0x{:02X}", addr),
                "BM1362 direct PIC address selected from serial slot",
            );
        }

        if is_bm1366 {
            warn!("BM1366 serial mining path is experimental — bring-up and live validation still pending");
        }

        if is_bm1370 {
            warn!("BM1370 serial mining path is experimental — job-id behavior and live validation still pending");
        }

        let job_id_increment: u8 = if is_bm1398 {
            BM1398_JOB_ID_INC
        } else if is_bm1366 {
            BM1366_JOB_ID_INC
        } else {
            BM1362_JOB_ID_INC
        };
        let resp_body_len: usize = if is_bm1398 {
            BM1398_RESP_BODY_LEN
        } else {
            BM1362_RESP_BODY_LEN
        };

        if is_bm1398 {
            info!(
                "=== S19 PRO SERIAL MINING (BM1398, {} chips) ===",
                chip_count
            );
        } else if is_bm1366 {
            info!(
                "=== BM1366 SERIAL MINING (experimental, {} chips) ===",
                chip_count
            );
        } else if is_bm1370 {
            info!(
                "=== BM1370 SERIAL MINING (experimental, {} chips, NoPic) ===",
                chip_count
            );
        } else if is_bm1368 {
            info!(
                "=== S21 SERIAL MINING (BM1368, {} chips, NoPic) ===",
                chip_count
            );
        } else {
            info!(
                "=== S19J PRO SERIAL MINING (BM1362, {} chips) ===",
                chip_count
            );
        }

        let mut published_voltage_mv = self.config.mining.voltage_mv;

        // ────────────────────────────────────────────────────────────────────
        // Hashboard-SKU energize-refusal gate ( B2, 2026-05-22).
        //
        // Drive-half of matrix §7 #15. Probes per-chain EEPROM preambles
        // BEFORE any dsPIC/NoPic voltage write. Refuses on malformed
        // header / timeout / mixed-SKU / profile-bind failure. Skipped on
        // `passthrough` (bosminer or other-OS owns voltage). Env-gated
        // strictness (`DCENT_AM2_STRICT_SKU_REFUSE`, default OFF);
        // `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1` is the lab override.
        //
        // The serial path is multi-chip-family: BM1398 (am2 S19 Pro,
        // PIC-class), BM1362 (am2 S19j Pro, PIC-class), BM1366 (am3 S19k
        // Pro, NoPic-class), BM1368 (am3 S21, NoPic-class), BM1370 (am3
        // S21 Pro/XP, NoPic-class). All routes through the gate the same
        // way — the gate only inspects EEPROM preambles, not chip ID, so
        // it's family-agnostic. BHB-S9 / BHB-S11 / BHB-S17 hashboards
        // have `eeprom_preamble = None` in the catalog and so their
        // (typically all-zero / vendor-proprietary) headers will surface
        // as Unpopulated/ReadError/MalformedPreamble depending on what's
        // in the EEPROM. For those platforms the gate is informational
        // until the catalog is filled in — strict-mode operators should
        // confirm classification first.
        // ────────────────────────────────────────────────────────────────────
        let mut retained_eeprom_bytes: [Option<Vec<u8>>; 3] = [None, None, None];
        if !passthrough {
            use crate::runtime::hardware_info::{
                read_hashboard_eeprom_for_energize_gate, EepromReadinessError,
                DEFAULT_EEPROM_READINESS_BUDGET_MS,
            };
            use dcentrald_silicon_profiles::energize_gate::{
                accept_degraded_hardware_enabled, classify_chain, gate_chains_for_energize,
                strict_sku_refuse_enabled, ChainProbe,
            };
            let strict = strict_sku_refuse_enabled();
            let accept_degraded = accept_degraded_hardware_enabled();
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(DEFAULT_EEPROM_READINESS_BUDGET_MS);
            let mut probes: Vec<ChainProbe> = Vec::with_capacity(3);
            for slot in 0u8..=2u8 {
                match read_hashboard_eeprom_for_energize_gate(slot as usize, deadline) {
                    Ok(bytes) => {
                        retained_eeprom_bytes[slot as usize] = Some(bytes.clone());
                        probes.push(classify_chain(slot, Some(&bytes)));
                    }
                    Err(EepromReadinessError::Timeout { .. }) => {
                        probes.push(ChainProbe::Timeout { chain_id: slot });
                    }
                    Err(EepromReadinessError::InvalidSlot { .. }) => {
                        probes.push(ChainProbe::ReadError { chain_id: slot });
                    }
                }
            }
            info!(
                strict,
                accept_degraded,
                probes = ?probes,
                "serial-mining: hashboard-SKU energize-gate probes"
            );
            match gate_chains_for_energize(&probes, strict) {
                Ok((bindings, telemetry)) => {
                    info!(
                        chains = bindings.len(),
                        bindings = ?bindings,
                        "serial-mining: energize gate ACCEPTED"
                    );
                    if !telemetry.is_empty() {
                        warn!(
                            reasons = %telemetry.summary(),
                            "serial-mining: [ENERGIZE-REFUSED telemetry-only — would refuse if DCENT_AM2_STRICT_SKU_REFUSE=1] {}",
                            telemetry.summary()
                        );
                    }
                }
                Err(refusal) => {
                    if accept_degraded {
                        warn!(
                            reasons = %refusal.summary(),
                            "serial-mining: [ENERGIZE-REFUSED but proceeding — DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1 lab override] {}",
                            refusal.summary()
                        );
                    } else {
                        tracing::error!(
                            reasons = %refusal.summary(),
                            "serial-mining: [ENERGIZE-REFUSED] {}",
                            refusal.summary()
                        );
                        anyhow::bail!(
                            "serial-mining hashboard-SKU energize gate refused: {}",
                            refusal.summary()
                        );
                    }
                }
            }
        }

        // Bootstrap sysfs EEPROM reads above must finish before the sole
        // runtime service reserves /dev/i2c-0. A kernel AT24 read is an I2C
        // transfer even though it appears as a read-only sysfs file; allowing
        // it after this boundary would create an invisible second bus owner.
        let bm1362_i2c_service = if (is_bm1398 || is_bm1366 || is_bm1362) && !nopic && !passthrough
        {
            Some(
                spawn_i2c_service_no_register_touch_with_denylist(
                    0,
                    HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec(),
                )
                .context("Failed to spawn AM2 serial /dev/i2c-0 service")?,
            )
        } else {
            None
        };
        if native_nopic_power_owner {
            amlogic_admission = Some(
                dcentrald_hal::platform::amlogic::AmlogicNoPicAdmission::detect(
                    dcentrald_hal::platform::amlogic::AmlogicNoPicProfile::S21,
                    &serial_device,
                )
                .context("Amlogic control-board identity did not admit native NoPic ownership")?,
            );
            let admission = amlogic_admission
                .as_ref()
                .context("Amlogic NoPic admission disappeared before owner construction")?;
            amlogic_power_thermal = Some(
                admission
                    .spawn_power_thermal_service()
                    .context("Failed to establish retained Amlogic /dev/i2c-1 ownership")?,
            );
        }

        // Cooling ownership and a checked startup command are prerequisites
        // for native NoPic power. This narrow constructor performs no platform
        // re-detection or raw I2C probe after the serialized services exist.
        let (effective_fan_min_pwm, effective_fan_max_pwm): (u8, u8) = if native_nopic_power_owner {
            let accept_degraded_tach = std::env::var("DCENT_AM3_AML_ACCEPT_DEGRADED_TACH")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let mut profile = dcentrald_thermal::profiles::ThermalProfile {
                fan_max_pwm: self.config.thermal.fan_max_pwm,
                fan_min_pwm: self.config.thermal.fan_min_pwm,
                ..Default::default()
            };
            let _ = dcentrald_thermal::profiles::enforce_amlogic_tach_safety_policy(
                &mut profile,
                true,
                accept_degraded_tach,
            );
            dcentrald_thermal::profiles::enforce_required_airflow_pwm(
                &mut profile,
                dcentrald_hal::platform::amlogic::REQUIRED_AIRFLOW_MIN_PWM,
            )
            .context("Amlogic air-cooled fan profile cannot satisfy required airflow")?;
            (profile.fan_min_pwm, profile.fan_max_pwm)
        } else {
            (
                self.config.thermal.fan_min_pwm,
                self.config.thermal.fan_max_pwm,
            )
        };
        amlogic_fan = if native_nopic_power_owner {
            let fan = amlogic_admission
                .as_ref()
                .context("Amlogic cooling construction lacks NoPic admission")?
                .open_fan_controller()
                .context("Failed to open Amlogic fan control before power admission")?;
            if effective_fan_max_pwm < self.config.thermal.fan_max_pwm {
                warn!(
                    requested_cap = self.config.thermal.fan_max_pwm,
                    applied_cap = effective_fan_max_pwm,
                    "am3-aml fan cap exceeds degraded-tach safety policy; applying retained startup ceiling"
                );
            }
            let receipt = admit_fan_airflow_envelope(
                fan.clone(),
                &mut nopic_fan_safety,
                effective_fan_min_pwm,
                effective_fan_max_pwm,
            )
            .await?;
            debug!(
                requested_pwm = receipt.requested_pwm(),
                observed_pwm = receipt.observed_pwm(),
                minimum_pwm = effective_fan_min_pwm,
                "Amlogic cooling owner admitted before NoPic power after min/max motion proof"
            );
            Some(fan)
        } else {
            None
        };
        let bhb56_dspic_sessions = if pending_bhb56_endpoints.is_empty() {
            Vec::new()
        } else {
            let i2c_service = bm1362_i2c_service
                .as_ref()
                .context("BHB56 endpoints were issued but serialized I2C service did not start")?;
            pending_bhb56_endpoints
                .into_iter()
                .map(|endpoint| DspicEndpointSession::new(i2c_service.clone(), endpoint))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to bind BHB56 endpoints to serialized I2C service")?
        };

        let serial = if passthrough {
            info!("PASSTHROUGH MODE — skipping PIC/ASIC init");
            info!(
                "Opening {} in preserve-state passthrough mode",
                serial_device
            );
            let mut s = SerialChainBackend::open_passthrough(0, &serial_device)
                .context("Failed to open passthrough serial backend")?;
            s.set_response_len(resp_body_len);
            Self::drain_serial_passthrough_backlog(&s, 5000);
            s
        } else if is_bm1398 {
            // ---- BM1398 (S19 Pro): dsPIC voltage init + ASIC init ----
            let mut target_voltage_mv = self.config.mining.voltage_mv;
            let profile_voltage_mv = MinerProfile::for_chip(0x1398)
                .map(|profile| profile.default_voltage_mv)
                .unwrap_or(13800);

            // S19 Pro dsPIC boards operate in a 12-15V range. Reuse the BM1398
            // profile default when a generic config still carries an S9-style voltage.
            if target_voltage_mv == 0 || !(11940..=15140).contains(&target_voltage_mv) {
                warn!(
                    config_mv = target_voltage_mv,
                    profile_mv = profile_voltage_mv,
                    "Serial BM1398 cold boot received out-of-range dsPIC voltage — using profile default",
                );
                target_voltage_mv = profile_voltage_mv;
            }
            published_voltage_mv = target_voltage_mv;

            info!("Phase 1: dsPIC voltage init at {:?}", S19_DSPIC_ADDRS);
            {
                let i2c_service = bm1362_i2c_service
                    .as_ref()
                    .context("AM2 serial I2C service missing for BM1398 dsPIC init")?;
                for &addr in &S19_DSPIC_ADDRS {
                    let mut dspic = DspicService::new(i2c_service.clone(), addr);
                    match dspic.cold_boot_init(target_voltage_mv) {
                        Ok(()) => {
                            info!(
                                "dsPIC 0x{:02X}: voltage {:.2}V, DC-DC enabled",
                                addr,
                                target_voltage_mv as f64 / 1000.0
                            );
                            // Arm the run-scope dsPIC disable leg the instant this
                            // rail is energized, so a bare `?` early-return in the
                            // serial ASIC init / stratum handshake (or clean
                            // shutdown) cuts this chain rail FIRST instead of
                            // leaving it ENABLED with no software backstop.
                            am2_power.set_dspic(i2c_service.clone(), vec![addr], addr, None);
                        }
                        Err(e) => warn!("dsPIC 0x{:02X} init failed: {}", addr, e),
                    }
                }
            }
            info!("Waiting 21s for ASIC boot (DC-DC ramp + power-on-reset)...");
            std::thread::sleep(Duration::from_millis(21000));

            // ---- Phase 2: BM1398 ASIC init via serial (with port fallback) ----
            // Try the configured port first, then fall back to other serial ports
            // if GetAddress scan returns no responses (hash board on different port).
            let ports_to_try: Vec<String> = {
                let mut ports = vec![serial_device.clone()];
                for fallback in ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3", "/dev/ttyS4"] {
                    let fb = fallback.to_string();
                    if !ports.contains(&fb) {
                        ports.push(fb);
                    }
                }
                ports
            };

            let mut last_err: Option<anyhow::Error> = None;
            let mut found_serial: Option<SerialChainBackend> = None;
            for port in &ports_to_try {
                info!("Phase 2: BM1398 ASIC init on {}", port);
                match Self::init_bm1398_chain(port, chip_count, target_freq) {
                    Ok(s) => {
                        info!("BM1398 init SUCCESS on {}", port);
                        found_serial = Some(s);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, port = %port, "BM1398 init/GetAddress failed on port — trying next");
                        last_err = Some(e);
                    }
                }
            }
            let mut s = match found_serial {
                Some(serial) => serial,
                None => {
                    return Err(last_err.unwrap_or_else(|| {
                        anyhow::anyhow!("BM1398 init failed on all serial ports")
                    }))
                }
            };
            s.set_response_len(resp_body_len);
            s
        } else if is_bm1366 {
            // ---- BM1366 (S19 XP / S19K Pro): dsPIC voltage init + ASIC init ----
            //
            // Two variants supported:
            //   1. BHB56 dsPIC variant — exact AMLCtrl_BHB56-family identity,
            //      bus-0 family anchor, and per-address ACKs issue the endpoint
            //      sessions used below. Model/config hints alone are refused.
            //   2. NoPic variant — am3-aml (S19K Pro NoPic, S19 XP, S19J XP).
            //      Voltage is set by kernel-managed TAS5782M DACs at i2c-0
            //      0x49/0x4A/0x4B per DTB. NO dsPIC anywhere on the bus.
            //      Skipping dsPIC init prevents the daemon from erroring out
            //      with "AM2 serial I2C service missing" since
            //      `bm1362_i2c_service` is intentionally None for NoPic units
            //      (see the gate at line ~2065).
            let mut target_voltage_mv = self.config.mining.voltage_mv;
            let profile_voltage_mv = MinerProfile::for_chip(0x1366)
                .map(|profile| profile.default_voltage_mv)
                .unwrap_or(13800);

            if target_voltage_mv == 0 || !(11940..=15140).contains(&target_voltage_mv) {
                warn!(
                    config_mv = target_voltage_mv,
                    profile_mv = profile_voltage_mv,
                    "Serial BM1366 cold boot received out-of-range voltage — using profile default",
                );
                target_voltage_mv = profile_voltage_mv;
            }
            published_voltage_mv = target_voltage_mv;

            if nopic {
                info!(
                    target_mv = target_voltage_mv,
                    "BM1366 NoPic: skipping dsPIC init (TAS5782M kernel-managed at i2c-0 0x49/0x4A/0x4B). \
                     Voltage published is informational only — actual rail set by DTB."
                );
            } else {
                info!("Phase 1: dsPIC voltage init at {:?}", S19_DSPIC_ADDRS);
                let i2c_service = bm1362_i2c_service
                    .as_ref()
                    .context("AM2 serial I2C service missing for BM1366 dsPIC init")?;
                for &addr in &S19_DSPIC_ADDRS {
                    let mut dspic = Self::dspic_service_for_serial_route(
                        i2c_service,
                        &bhb56_dspic_sessions,
                        bhb56_endpoint_capability_required,
                        addr,
                        None,
                    )?;
                    match dspic.cold_boot_init(target_voltage_mv) {
                        Ok(()) => {
                            info!(
                                "dsPIC 0x{:02X}: voltage {:.2}V, DC-DC enabled",
                                addr,
                                target_voltage_mv as f64 / 1000.0
                            );
                            // Arm the run-scope dsPIC disable leg the instant this
                            // rail is energized, so a bare `?` early-return in the
                            // serial ASIC init / stratum handshake (or clean
                            // shutdown) cuts this chain rail FIRST instead of
                            // leaving it ENABLED with no software backstop.
                            am2_power.set_dspic(i2c_service.clone(), vec![addr], addr, None);
                        }
                        Err(e) => warn!("dsPIC 0x{:02X} init failed: {}", addr, e),
                    }
                }
            }
            info!("Waiting 21s for BM1366 ASIC boot (DC-DC ramp + power-on-reset)...");
            std::thread::sleep(Duration::from_millis(21000));

            let ports_to_try: Vec<String> = {
                let mut ports = vec![serial_device.clone()];
                for fallback in ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3", "/dev/ttyS4"] {
                    let fb = fallback.to_string();
                    if !ports.contains(&fb) {
                        ports.push(fb);
                    }
                }
                ports
            };

            let mut last_err: Option<anyhow::Error> = None;
            let mut found_serial: Option<SerialChainBackend> = None;
            for port in &ports_to_try {
                info!("Phase 2: BM1366 ASIC init on {}", port);
                match Self::init_bm1366_chain(port, chip_count, target_freq) {
                    Ok(s) => {
                        info!("BM1366 init SUCCESS on {}", port);
                        found_serial = Some(s);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, port = %port, "BM1366 init/GetAddress failed on port — trying next");
                        last_err = Some(e);
                    }
                }
            }

            let mut s = match found_serial {
                Some(serial) => serial,
                None => {
                    return Err(last_err.unwrap_or_else(|| {
                        anyhow::anyhow!("BM1366 init failed on all serial ports")
                    }))
                }
            };
            s.set_response_len(resp_body_len);
            s
        } else if is_bm1368 || is_bm1370 {
            // ---- BM1368 (S21/T21): NoPic + BM1368-specific init ----
            // Voltage architecture (verified 2026-04-12 via ftrace + fixture RE + live probe):
            //   - TAS5782M DACs (bus 0, addr 0x49/0x4A/0x4B) are kernel-managed from DTB
            //   - APW PSU enabled via GPIO 437 plus APW I2C/PMBus preboot sequence at 0x1f
            //   - Bosminer never writes to TAS5782M — voltage DACs stay kernel-managed
            //   - No PMBus device at 0x58 on either I2C bus (confirmed 2026-04-12);
            //     the native cold-boot APW path is bus 1 addr 0x1f
            info!("Phase 1: PSU enable (NoPic model, TAS5782M kernel-managed)");

            // Wave J Lane A: NoPic PSU enable is GPIO-437-only (the voltage is
            // kernel-managed TAS5782M; there is NO smart-PSU probe to skip), so a
            // 120V non-smart PSU already works here. When [power.psu_override] is
            // set we honor it for telemetry: record the declared model + its
            // efficiency, and log the disposition so the override is never silently
            // ignored (fail-loud honesty). The GPIO 437 enable below is correct for
            // any PSU and is unchanged.
            if crate::s19j_hybrid_mining::psu_override_active(
                self.config.power.psu_override.as_ref(),
            ) {
                let ovr = self
                    .config
                    .power
                    .psu_override
                    .as_ref()
                    .expect("psu_override_active implies Some");
                info!(
                    model = %ovr.model,
                    rail_v = ovr.voltage_v,
                    efficiency =
                        ?crate::runtime::efficiency::psu_efficiency_for_model_name(&ovr.model),
                    "NoPic (am3-aml): PSU OVERRIDE honored as INFORMATIONAL — PSU enable is \
                     GPIO-437-only + kernel-managed TAS5782M voltage, so there is no smart-PSU \
                     probe to bypass; declared model + efficiency recorded for telemetry"
                );
            }

            if !native_nopic_power_owner {
                anyhow::bail!(
                    "internal NoPic ownership mismatch: energizing path lacks an explicit power lease"
                );
            }
            if self.shutdown.is_cancelled() {
                anyhow::bail!("shutdown was already requested before NoPic watchdog admission");
            }
            let (watchdog_owner, watchdog_admission) =
                SafetyWatchdogOwner::start_before_energizing(
                    &self.config.watchdog,
                    NOPIC_WATCHDOG_BRINGUP_GRACE,
                    NOPIC_SAFETY_LIVENESS_INTERVAL,
                    nopic_watchdog_liveness.clone(),
                )
                .await?;
            let arm_receipt = watchdog_admission.require_armed("native serial NoPic")?;
            info!(
                requested_timeout_s = arm_receipt.requested_timeout_s,
                effective_timeout_s = arm_receipt.effective_timeout_s,
                kick_interval_s = arm_receipt.kick_interval_s,
                bringup_grace_s = NOPIC_WATCHDOG_BRINGUP_GRACE.as_secs(),
                "NoPic watchdog arm admission observed before GPIO437 mutation"
            );
            nopic_watchdog = Some(watchdog_owner);
            if self.shutdown.is_cancelled() {
                anyhow::bail!("shutdown raced NoPic watchdog admission; refusing PSU enable");
            }

            // Enable PSU via GPIO 437 (PWR_EN, active HIGH: 1=ON, 0=OFF —
            // Q10, polarity CORRECTED 2026-05-21; see amlogic/mod.rs enable_psu_gpio
            // + the psu_enable_is_active_high_437 test). Do NOT re-read this as the
            // old "PSU_nEN active LOW" — that inverted reading was the original
            // polarity bug. (prod-readiness hunt-2 #H2.) Arm the shutdown guard
            // immediately so a failed init does not leave boards powered.
            let power_thermal = amlogic_power_thermal
                .as_ref()
                .context("NoPic power route lacks retained Amlogic bus-1 ownership")?;
            nopic_psu_guard.prepare_enable(effective_fan_max_pwm, power_thermal.terminal_fence());
            let power_enable_owner = power_thermal.clone();
            let enable_receipt =
                tokio::task::spawn_blocking(move || power_enable_owner.enable_psu())
                    .await
                    .context("NoPic PSU enable worker did not complete")?
                    .context("Failed to enable NoPic PSU")?;
            debug!(
                writes_completed_at = ?enable_receipt.writes_completed_at(),
                status_word = ?enable_receipt.status_word(),
                "NoPic APW enable receipt retained"
            );
            nopic_psu_guard.mark_enabled();
            let energized_at = Instant::now();
            nopic_energized_at = Some(energized_at);

            let startup_thermal_owner = power_thermal.clone();
            let mut startup_board_temps = tokio::task::spawn_blocking(move || {
                startup_thermal_owner
                    .read_board_temperatures(Instant::now() + Duration::from_millis(750))
            })
            .await
            .context("NoPic startup thermal worker did not complete")?;
            let startup_deadline = energized_at + Duration::from_secs(AMLOGIC_TEMP_STARTUP_GRACE_S);
            loop {
                if let Some(hottest_temp) = startup_board_temps.hottest_celsius() {
                    if hottest_temp >= self.config.thermal.dangerous_temp_c as f32 {
                        let _safe_off = nopic_psu_guard
                            .safe_off()
                            .context("dangerous startup temperature safe-off failed")?;
                        anyhow::bail!(
                            "Amlogic startup observed dangerous temperature {hottest_temp:.1} C before ASIC initialization"
                        );
                    }
                }
                let startup_coverage = startup_board_temps.required_coverage();
                if startup_coverage.is_complete() {
                    break;
                }
                warn!(
                    required_slots = ?startup_coverage.required_slots(),
                    missing_slots = ?startup_coverage.missing_slots(),
                    deadline_ms = startup_deadline.saturating_duration_since(Instant::now()).as_millis(),
                    "Required board-temperature coverage is incomplete after PSU enable; ASIC initialization remains blocked"
                );
                if self.shutdown.is_cancelled() || Instant::now() >= startup_deadline {
                    let _safe_off = nopic_psu_guard
                        .safe_off()
                        .context("startup thermal-coverage safe-off failed")?;
                    anyhow::bail!(
                        "required Amlogic board-temperature coverage did not become complete before the powered-startup deadline"
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                let retry_owner = power_thermal.clone();
                startup_board_temps = tokio::task::spawn_blocking(move || {
                    retry_owner.read_board_temperatures(Instant::now() + Duration::from_millis(750))
                })
                .await
                .context("NoPic startup thermal retry worker did not complete")?;
            }
            if let Some(hottest_temp) = startup_board_temps.hottest_celsius() {
                info!(
                    sensors = startup_board_temps.readings().len(),
                    unavailable = startup_board_temps.unavailable().len(),
                    hottest_c = format_args!("{:.1}", hottest_temp),
                    "Board temperature sensors responded after PSU enable"
                );
            }

            // Phase 1b: NO GPIO board reset on S21 NoPic!
            // GPIO 454-456 reset kills TAS5782M DAC voltage → ASICs lose power.
            // Instead: skip reset, send GetAddress to verify chips are alive.
            info!("Phase 1b: Skipping GPIO board reset (NoPic — reset kills voltage)");

            // Diagnostic: probe chips at multiple bauds to find if they're alive.
            // Try the common live-state fast baud first, then back down.
            let mut found_chips = false;
            let mut passthrough_serial: Option<SerialChainBackend> = None;
            for probe_baud in [3_000_000u32, 1_000_000, 115_200] {
                info!("Phase 1c: Probing chips at {} baud...", probe_baud);
                if let Ok(mut probe) = SerialChainBackend::open(0, &serial_device, probe_baud) {
                    probe.set_response_len(BM13XX_CMD_RESP_BODY_LEN); // 11 bytes total on wire
                    let _ = probe.send_get_address_bm1397plus();
                    std::thread::sleep(Duration::from_millis(200));
                    match probe.read_all_responses(500) {
                        Ok(responses) if !responses.is_empty() => {
                            info!(
                                "CHIPS ALIVE at {} baud: {} responses!",
                                probe_baud,
                                responses.len()
                            );
                            if probe_baud >= 1_000_000 {
                                // Chips already at fast baud — use passthrough
                                info!(
                                    "Using passthrough at {} baud (existing live ASIC/UART state)",
                                    probe_baud
                                );
                                found_chips = true;
                                let mut s = probe;
                                s.set_response_len(resp_body_len);
                                passthrough_serial = Some(s);
                            } else {
                                found_chips = true;
                                drop(probe);
                            }
                            break;
                        }
                        Ok(_) => {
                            info!("No responses at {} baud", probe_baud);
                            drop(probe);
                        }
                        Err(e) => {
                            warn!("Probe error at {} baud: {}", probe_baud, e);
                            drop(probe);
                        }
                    }
                }
            }

            if !found_chips {
                if startup_board_temps.readings().is_empty() {
                    anyhow::bail!(
                        "NoPic PSU enable produced no board-temperature or ASIC-response proof — refusing to continue blind"
                    );
                }

                warn!(
                    "No ASICs responded yet, but board temperatures are alive after PSU enable — continuing with cold NoPic init"
                );
            }

            if is_bm1370 {
                info!("Phase 2: Full BM1370 ASIC init ({} chips)", chip_count);
                let mut s = Self::init_bm1370_chain(&serial_device, chip_count, target_freq)?;
                s.set_response_len(resp_body_len);
                s
            } else if let Some(mut s) = passthrough_serial {
                // Chips alive at fast baud — do full register reinit at this baud
                info!("=== BM1368 WARM INIT (chips alive at 3M, full register config) ===");

                // Chain inactive to reset ASIC work scheduler (critical for accepting our work)
                let _ = s.send_chain_inactive_bm1397plus();
                std::thread::sleep(Duration::from_millis(50));

                // Re-address chips
                let addr_interval = 256u16 / (chip_count as u16).max(1);
                for i in 0..chip_count as u16 {
                    let _ = s.send_set_address_bm1397plus((i * addr_interval) as u8);
                }
                std::thread::sleep(Duration::from_millis(10));
                info!("Warm init: {} chips re-addressed at 3M", chip_count);

                // Full broadcast register config (same as cold boot init)
                let _ = s.send_write_reg_broadcast_bm1397plus(0xA8, BM1368_REG_A8_BCAST);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x18, BM1368_MISC_CTRL_BCAST);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_1);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x3C, BM1368_CORE_REG_2);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x14, BM1368_TICKET_MASK);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x54, ANALOG_MUX_VALUE);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x58, BM1368_IO_DRIVER);
                std::thread::sleep(Duration::from_millis(10));

                // Per-chip register config
                for i in 0..chip_count {
                    let chip_addr = (i as u16 * addr_interval) as u8;
                    let _ = s.send_write_reg_bm1397plus(chip_addr, 0xA8, BM1368_REG_A8_PER_CHIP);
                    let _ = s.send_write_reg_bm1397plus(chip_addr, 0x18, BM1368_MISC_CTRL_PER_CHIP);
                    let _ = s.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_1);
                    let _ = s.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_2);
                    let _ = s.send_write_reg_bm1397plus(chip_addr, 0x3C, BM1368_CORE_REG_3);
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(50));
                info!("Warm init: per-chip config complete");

                // Runtime registers
                let _ = s.send_write_reg_broadcast_bm1397plus(0x14, BM1368_TICKET_MASK);
                let _ = s.send_write_reg_broadcast_bm1397plus(0x10, NONCE_RANGE_108);
                let _ = s.send_write_reg_broadcast_bm1397plus(0xA4, VERSION_MASK_VALUE);

                // Flush stale RX
                let _ = s.flush_io();
                std::thread::sleep(Duration::from_millis(50));
                let _ = s.flush_io();
                info!(
                    "=== BM1368 WARM INIT COMPLETE — {} chips at 3 Mbaud ===",
                    chip_count
                );
                s
            } else {
                info!("Phase 2: Full BM1368 ASIC init ({} chips)", chip_count);
                let mut s = Self::init_bm1368_chain(&serial_device, chip_count, target_freq)?;
                s.set_response_len(resp_body_len);
                s
            }
        } else {
            // ---- BM1362 (S19j Pro): PIC init + BM1362-specific init ----
            if nopic {
                info!("Phase 1: SKIPPED — NoPic model, voltage via TAS5782M DAC");
            } else {
                // W24-CRASH-1 (panic-hook coverage for the BM1362 direct serial
                // path): this branch is about to assert PWR_CONTROL + bring up the
                // APW rail + set the chain rail to 13.7 V via the dsPIC. On a
                // `panic = "abort"` build NONE of `Am2PsuRuntimeGuard::Drop` /
                // `PsuGpioGate::Drop` run, and — unlike the NoPic branch (which
                // arms `arm_nopic_teardown`) — this path previously armed NO
                // panic-hook teardown at all, so a panic during enum / init /
                // stratum handshake left PWR_CONTROL asserted with NO software
                // backstop (APW fw=0x71 + dsPIC fw=0x86 have no telemetry; the
                // only hardware backstop is the dsPIC heartbeat watchdog). Arm
                // the SAME process-global the am2 hybrid run-scope uses so the
                // already-installed `main()` panic hook drives this unit's
                // `pwr_control_gpio` low FIRST, then caps fans at PWM_SAFETY_MAX
                // (30). Idempotent (OnceLock); stores config only — no hardware
                // I/O on the happy path; only fires on a panic. (Audit
                // panic-hook-coverage gap, 2026-05-29.)
                crate::s19j_hybrid_mining::arm_am2_teardown_params(&self.config);

                // Quiet-state `a lab unit` direct tests were still leaning on whatever
                // shared PSU state BraiinsOS left behind. Own the APW rail in the
                // pure serial BM1362 path too so direct tests no longer depend on
                // stock fee-session runtime state.
                let psu_transport = self.config.psu.transport.as_str();
                let psu_address = self.config.psu.i2c_address;
                let psu_target_rail_v = self.config.psu.voltage_mv as f64 / 1000.0;
                let psu_heartbeat_hz = u64::from(self.config.psu.heartbeat_hz.max(1));
                let psu_heartbeat_interval =
                    Duration::from_millis((1000 / psu_heartbeat_hz).max(1));

                info!(
                    transport = psu_transport,
                    addr = format_args!("0x{:02X}", psu_address),
                    "Phase -1: APW bring-up for BM1362 direct path"
                );
                // Wave J Lane A: 120V "Loki bypass" — when the operator declares a
                // non-smart PSU via [power.psu_override], there is no APW121215a at
                // 0x10 to probe / cold-boot / heartbeat (it would BLOCK on a dumb
                // PSU). Assert PWR_CONTROL via PsuGpioGate (the APW output enable is
                // wired through it) and proceed — the BM1362 chip-rail voltage path
                // below is UNCHANGED (psu_override.voltage_v is the PSU OUTPUT rail,
                // never the chip setpoint). Mirrors the proven s19j_hybrid Phase-0
                // branch (b)..
                if crate::s19j_hybrid_mining::psu_override_active(
                    self.config.power.psu_override.as_ref(),
                ) {
                    let ovr = self
                        .config
                        .power
                        .psu_override
                        .as_ref()
                        .expect("psu_override_active implies Some");
                    let gate = PsuGpioGate::assert(self.config.psu.pwr_control_gpio.as_deref())
                        .context("PSU bypass: PWR_CONTROL assert failed (BM1362 direct)")?;
                    info!(
                        model = %ovr.model,
                        rail_v = ovr.voltage_v,
                        gpio = gate.gpio(),
                        efficiency =
                            ?crate::runtime::efficiency::psu_efficiency_for_model_name(&ovr.model),
                        "BM1362 direct: PSU OVERRIDE (Loki bypass) — skipping Apw121215a \
                         probe/cold-boot/heartbeat; PWR_CONTROL asserted; rail voltage recorded \
                         (NOT the chip voltage)"
                    );
                    am2_power.set_gate(gate);
                } else if psu_transport == "gpio_bitbang" {
                    let gate = PsuGpioGate::assert(self.config.psu.pwr_control_gpio.as_deref())
                        .context("Failed to assert PWR_CONTROL for BM1362 direct path")?;
                    info!(
                        gpio = gate.gpio(),
                        "BM1362 direct path: PWR_CONTROL asserted"
                    );
                    am2_power.set_gate(gate);

                    let mut psu = Apw121215a::open_gpio_bitbang_at(psu_address).context(
                        "Failed to open APW121215a via gpio bit-bang for BM1362 direct path",
                    )?;
                    psu.cold_boot_sequence_write_only(psu_target_rail_v, APW12_139_ASSUMED_FW)
                        .context("BM1362 direct path PSU write-only cold_boot_sequence failed")?;
                    let psu = Arc::new(Mutex::new(psu));
                    let psu_hb = psu.clone();
                    let shutdown_hb = runtime_threads.cancellation_token();
                    let handle = std::thread::Builder::new()
                        .name("s19j-serial-psu-hb".into())
                        .spawn(move || {
                            Self::psu_heartbeat_loop(psu_hb, shutdown_hb, psu_heartbeat_interval)
                        })
                        .context("Failed to spawn BM1362 direct PSU heartbeat thread")?;
                    runtime_threads.push("s19j-serial-psu-hb", handle);
                    info!(
                        hz = psu_heartbeat_hz,
                        "BM1362 direct PSU heartbeat thread spawned"
                    );
                    std::thread::sleep(Duration::from_secs(5));
                    am2_power.set_psu(psu);
                } else if let Some(i2c_service) = bm1362_i2c_service.as_ref() {
                    let gate = PsuGpioGate::assert(self.config.psu.pwr_control_gpio.as_deref())
                        .context("Failed to assert PWR_CONTROL for BM1362 direct service path")?;
                    info!(
                        gpio = gate.gpio(),
                        "BM1362 direct service path: PWR_CONTROL asserted"
                    );
                    am2_power.set_gate(gate);

                    let mut psu = Apw121215a::open_service_at(i2c_service.clone(), 0, psu_address)
                        .context("Failed to open APW121215a through BM1362 direct I2C service")?;
                    psu.cold_boot_sequence_write_only(psu_target_rail_v, APW12_139_ASSUMED_FW)
                        .context("BM1362 direct path APW service cold_boot_sequence failed")?;
                    let psu = Arc::new(Mutex::new(psu));
                    let psu_hb = psu.clone();
                    let shutdown_hb = runtime_threads.cancellation_token();
                    let handle = std::thread::Builder::new()
                        .name("s19j-serial-psu-hb".into())
                        .spawn(move || {
                            Self::psu_heartbeat_loop(psu_hb, shutdown_hb, psu_heartbeat_interval)
                        })
                        .context("Failed to spawn BM1362 direct PSU service heartbeat thread")?;
                    runtime_threads.push("s19j-serial-psu-hb", handle);
                    info!(
                        hz = psu_heartbeat_hz,
                        "BM1362 direct PSU service heartbeat thread spawned"
                    );
                    std::thread::sleep(Duration::from_secs(5));
                    am2_power.set_psu(psu);
                } else {
                    warn!(transport = psu_transport, "BM1362 direct path APW service unavailable; continuing without explicit PSU bootstrap");
                }

                let pic_addr =
                    bm1362_pic_addr.context("BM1362 direct PIC address was not resolved")?;
                info!("Phase 1: PIC init at I2C 0x{:02X} (Pic0x89 flow)", pic_addr);
                let (detected_fw, detected_fw_reply) =
                    if let Some(i2c_service) = bm1362_i2c_service.as_ref() {
                        Self::pic_read_fw_version_service(i2c_service, pic_addr)
                            .context("BM1362 direct PIC service GET_VERSION preflight failed")?
                    } else {
                        anyhow::bail!(
                            "BM1362 direct PIC service missing; refusing second /dev/i2c-0 owner"
                        )
                    };
                info!(
                    fw = format_args!("0x{:02X}", detected_fw),
                    "BM1362 direct PIC FW version"
                );
                bm1362_detected_pic_fw = Some(detected_fw);

                // Bosminer's am2 cold-boot order resets the hashboard after the
                // APW rail is owned but before per-chain dsPIC voltage enable.
                // A post-enable reset can drop ASICs immediately after the
                // DC-DC ramp, so keep the reset-before-enable ordering aligned
                // with the hybrid path.
                if Self::bm1362_skip_post_power_reset() {
                    warn!(
                        "DCENT_BM1362_SKIP_POST_POWER_RESET set; no post-voltage reset will be sent. \
                         Keeping bosminer-aligned pre-voltage reset."
                    );
                }
                info!("Phase 1b: Pulsing am2 hashboard reset before PIC voltage enable");
                Self::pulse_am2_hashboard_reset(&serial_device);
                info!("Phase 1c: Waiting 2s after HB reset for fan/autoconfig gate ('Fans OK' window)");
                std::thread::sleep(Duration::from_secs(2));
                if let Some(i2c_service) = bm1362_i2c_service.as_ref() {
                    let mut pic = match dcentrald_hal::platform::beaglebone::try_bind_system_am3_bb_dspic_endpoint(
                        &serial_device,
                        pic_addr,
                        &retained_eeprom_bytes,
                        &detected_fw_reply,
                    )
                    .context("AM3-BB direct PIC endpoint binding failed")?
                    {
                        Some(endpoint) => {
                            info!(
                                serial_device = %serial_device,
                                pic_addr = format_args!("0x{:02X}", endpoint.address()),
                                "BM1362 direct PIC owner bound to exact AM3-BB endpoint capability"
                            );
                            Pic0x89EndpointSession::new(i2c_service.clone(), endpoint)
                                .context("failed to bind AM3-BB Pic0x89 endpoint to I2C service")?
                                .into_controller()
                        }
                        None => {
                            if let Some(plan) = pending_am2_bm1362_plan.as_ref() {
                                let context = plan.context_for_address(pic_addr).ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "exact am2-s19j plan has no context for observed PIC address 0x{pic_addr:02X}; refusing raw-address fallback"
                                    )
                                })?;
                                let eeprom_bytes = retained_eeprom_bytes
                                    .get(usize::from(context.slot()))
                                    .and_then(|bytes| bytes.clone())
                                    .with_context(|| {
                                        format!(
                                            "exact am2-s19j slot {} lacks its retained pre-energize EEPROM observation",
                                            context.slot()
                                        )
                                    })?;
                                let presence =
                                    dcentrald_hal::platform::bind_am2_hashboard_presence(
                                        plan,
                                        context,
                                        eeprom_bytes,
                                    )?;
                                let endpoint = dcentrald_hal::platform::bind_am2_controller_endpoint_from_observation(
                                    &presence,
                                    &detected_fw_reply,
                                )?;
                                info!(
                                    serial_device = %serial_device,
                                    pic_addr = format_args!("0x{:02X}", endpoint.address()),
                                    firmware = ?endpoint.observed_firmware(),
                                    "BM1362 direct PIC owner bound to exact AM2 endpoint capability from retained observations"
                                );
                                Pic0x89EndpointSession::new(i2c_service.clone(), endpoint)
                                    .context("failed to bind AM2 Pic0x89 endpoint to I2C service")?
                                    .into_controller()
                            } else {
                                Pic0x89Service::new_with_fw(
                                    i2c_service.clone(),
                                    pic_addr,
                                    Some(detected_fw),
                                )
                            }
                        }
                    };
                    // DspicService handles fw0x86 bare ENABLE as a one-byte
                    // firmware ACK and uses frame-shape-aware RESET/JUMP
                    // guards.
                    pic.cold_boot_init(13_700)
                        .context("BM1362 direct PIC service cold_boot_init failed")?;
                    // Arm the run-scope dsPIC chip-rail disable leg the instant the
                    // rail is energized, so ANY later return path (bare `?` in
                    // `init_asic_chain` / stratum handshake, or clean shutdown)
                    // disables voltage on this dsPIC FIRST instead of leaving the
                    // chain rail engaged behind a dropped PWR_CONTROL.
                    am2_power.set_dspic(
                        i2c_service.clone(),
                        vec![pic_addr],
                        pic_addr,
                        Some(detected_fw),
                    );
                } else {
                    anyhow::bail!(
                        "BM1362 direct PIC service missing; refusing second /dev/i2c-0 owner"
                    )
                }
                info!("BM1362 direct PIC cold_boot_init returned OK (SetVoltage 13.7V applied + ENABLE_VOLTAGE accepted at protocol level — ACK/echo only); rail engagement UNVERIFIED until the post-enable chain-UART probe / chain enumeration below");

                // Post-ENABLE chain UART rail-engagement probe. APW121215a
                // has no voltage feedback (`psu.rs:493`) and dsPIC fw=0x86
                // bare GET_VOLTAGE only echoes the FW byte, so chain UART
                // byte-count is the only software signal of actual rail
                // engagement. and
                // .
                post_enable_chain_uart_probe(&serial_device, pic_addr);

                std::thread::sleep(Duration::from_millis(1200));
            }

            info!("Phase 2: Full BM1362 ASIC init ({} chips)", chip_count);
            let mut s = Self::init_asic_chain(&serial_device, chip_count, target_freq)?;
            s.set_response_len(resp_body_len);
            s
        };

        // ---- Phase 3: PIC heartbeat thread (kernel I2C) ----
        // S21/T21 NoPic: no PIC → no heartbeat needed (voltage stays from kernel DAC)
        if passthrough {
            info!("Phase 3: SKIPPED - passthrough mode does not own PIC voltage state");
        } else if nopic {
            info!("Phase 3: SKIPPED — NoPic model, no PIC heartbeat needed");
        } else {
            info!("Phase 3: Starting PIC heartbeat thread");
            let hb_shutdown = runtime_threads.cancellation_token();
            let hb_uses_dspic = is_bm1398 || is_bm1366 || is_bm1362;
            let hb_is_bm1362 = is_bm1362;
            let heartbeat_pic_addr = bm1362_pic_addr.unwrap_or(S19J_PIC_ADDR_7BIT);
            let heartbeat_bm1362_firmware = if hb_is_bm1362 {
                Some(observed_dspic_firmware(bm1362_detected_pic_fw)?)
            } else {
                None
            };
            let heartbeat_i2c_service = bm1362_i2c_service.clone();
            let heartbeat_bhb56_sessions = bhb56_dspic_sessions;
            let heartbeat_requires_endpoint = bhb56_endpoint_capability_required;
            let handle = std::thread::Builder::new()
                .name("s19j-pic-hb".to_string())
                .spawn(move || {
                    if hb_uses_dspic {
                        if let Some(i2c_service) = heartbeat_i2c_service {
                            info!(
                                bm1362 = hb_is_bm1362,
                                "dsPIC heartbeat running through AM2 serial I2C service"
                            );
                            let mut fails = 0u32;
                            loop {
                                if hb_shutdown.is_cancelled() {
                                    break;
                                }
                                let mut ok = true;
                                let addrs: &[u8] = if hb_is_bm1362 {
                                    std::slice::from_ref(&heartbeat_pic_addr)
                                } else {
                                    &S19_DSPIC_ADDRS
                                };
                                for &addr in addrs {
                                    let firmware = if hb_is_bm1362 {
                                        // Resolved once at the lifecycle boundary before
                                        // spawning this thread. `Some` is proven by the
                                        // `hb_is_bm1362` branch above.
                                        let Some(firmware) = heartbeat_bm1362_firmware else {
                                            error!(
                                                "BM1362 heartbeat firmware invariant was lost; stopping heartbeat thread"
                                            );
                                            return;
                                        };
                                        firmware
                                    } else {
                                        match addr {
                                            0x20 => DspicFirmware::Fw82,
                                            0x21 => DspicFirmware::Fw86,
                                            0x22 => DspicFirmware::Fw8A,
                                            _ => DspicFirmware::Unknown,
                                        }
                                    };
                                    let mut dspic = match Self::dspic_service_for_serial_route(
                                        &i2c_service,
                                        &heartbeat_bhb56_sessions,
                                        heartbeat_requires_endpoint,
                                        addr,
                                        Some(firmware),
                                    ) {
                                        Ok(dspic) => dspic,
                                        Err(error) => {
                                            ok = false;
                                            error!(
                                                addr = format_args!("0x{:02X}", addr),
                                                %error,
                                                "dsPIC heartbeat refused without bound endpoint capability"
                                            );
                                            continue;
                                        }
                                    };
                                    if let Err(e) = dspic.send_heartbeat() {
                                        ok = false;
                                        warn!(
                                            addr = format_args!("0x{:02X}", addr),
                                            error = %e,
                                            "dsPIC service heartbeat failed"
                                        );
                                    }
                                }
                                if ok {
                                    if fails > 0 {
                                        info!("dsPIC service heartbeat OK after {} failed cycles", fails);
                                    }
                                    fails = 0;
                                } else {
                                    fails += 1;
                                }
                                std::thread::sleep(Duration::from_millis(PIC_HEARTBEAT_INTERVAL_MS));
                            }
                            return;
                        }
                    }

                    error!(
                        bm1362 = hb_is_bm1362,
                        legacy_pic_route = !hb_uses_dspic,
                        "serialized I2C heartbeat authority is unavailable; refusing an unbrokered /dev/i2c-0 owner"
                    );
                })
                .context("Failed to spawn heartbeat thread")?;
            runtime_threads.push("s19j-pic-hb", handle);
        } // end NoPic check

        // ---- Arm the hardware watchdog (AFTER chain bring-up completes) ----
        // `--serial-mining` (PIC and NoPic) bypasses `Daemon::run()`, so this path
        // historically armed NO `/dev/watchdog` — a CPU/runtime hang here left the
        // boards energized & unsupervised. Arm it now (chain init + heartbeat
        // complete, before pool connect) via the shared, config-gated helper —
        // NOT earlier, so the DTB-10s window can never trip during cold-boot.
        // SAF-5: gate kicks on this path's mining/event-loop heartbeat so a
        // live-locked serial miner stops feeding `/dev/watchdog` after the
        // counter has started advancing. The Amlogic thermal arm still owns
        // actual temperature fail-closed behavior.
        let watchdog_liveness = Arc::new(AtomicU64::new(0));
        if nopic_watchdog.is_none() {
            crate::daemon::spawn_watchdog_kicker(
                &self.config.watchdog,
                self.shutdown.clone(),
                Some(watchdog_liveness.clone()),
            );
        }

        // ---- Phase 4: Pool connection ----
        info!("Phase 4: Connecting to pool");
        let (job_tx, mut job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
        let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
        let (status_tx, mut status_rx) =
            mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);
        let (mining_sync_tx, _) = tokio::sync::broadcast::channel(256);
        let (jd_status_tx, jd_status_rx) = tokio::sync::watch::channel(
            crate::daemon::initial_job_declaration_status(&self.config.job_declaration),
        );
        crate::daemon::spawn_job_declaration_supervisor(
            self.config.job_declaration.clone(),
            jd_status_tx,
            self.shutdown.clone(),
        );

        let serial_version_rolling = self.config.mining.version_rolling;
        if is_bm1398 && self.config.mining.version_rolling {
            info!("BM1398 serial path reconstructs rolled versions from nonce midstate index");
        }

        let stratum_config = crate::config::build_stratum_config(
            &self.config,
            crate::config::stratum_donation_config(&self.config.donation),
            serial_version_rolling,
            false,
        );
        let stratum_router = dcentrald_stratum::StratumRouter::new(stratum_config)
            .with_job_declaration_status_rx(jd_status_rx.clone());
        let recent_share_history = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(async move {
            stratum_router.run(job_tx, share_rx, status_tx).await;
        });

        let (state_tx, state_rx) = tokio::sync::watch::channel(dcentrald_api::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: vec![dcentrald_api::ChainState {
                id: 0,
                chips: chip_count,
                frequency_mhz: target_freq,
                voltage_mv: published_voltage_mv,
                temp_c: 0.0,
                temp_source: None,
                hashrate_ghs: 0.0,
                errors: 0,
                // FWT-4: this is the pre-mining initial snapshot (hashrate 0) —
                // report the honest "active" (chain present, not yet producing),
                // not a fabricated "mining".
                status: "active".to_string(),
            }],
            fans: dcentrald_api::FanState {
                // Placeholder snapshot (rpm/temp/hashrate all 0 here): report the
                // quiet idle floor, not a max-blast number. The old NoPic value
                // (127) was both alarming and off-scale for the Amlogic 0-100 fan
                // range; this is display-only telemetry, never a fan command.
                pwm: 10,
                rpm: 0,
                per_fan: vec![],
            },
            pool: dcentrald_api::PoolState {
                url: self.config.pool.url.clone(),
                worker: self.config.pool.worker.clone(),
                status: "connecting".to_string(),
                difficulty: 0.0,
                last_share_at: 0,
                protocol: "sv1".to_string(),
                encrypted: false,
                encrypted_source: dcentrald_api::pool_quality_honest_default_source(),
                sv2_session: None,
                sv2_session_source: dcentrald_api::pool_quality_honest_default_source(),
                donating: false,
                donating_source: dcentrald_api::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: dcentrald_api::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                sv2_custom_job: None,
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
            },
            uptime_s: 0,
            firmware_version: "0.4.0".to_string(),
            mode: dcentrald_api::OperatingMode::Standard,
        });

        // Status logger
        let ss = self.shutdown.clone();
        let recent_share_history_status = recent_share_history.clone();
        let mining_sync_status_tx = mining_sync_tx.clone();
        let status_state_tx = state_tx.clone();
        tokio::spawn(async move {
            let mut current_pool_difficulty = 1.0f64;
            let mut pool_quality = dcentrald_stratum::pool_quality::PoolQualitySnapshot::default();
            loop {
                tokio::select! {
                    _ = ss.cancelled() => break,
                    Some(st) = status_rx.recv() => {
                        dcentrald_stratum::pool_quality::apply_stratum_status(
                            &mut pool_quality,
                            &st,
                        );
                        let quality_snapshot = pool_quality.clone();
                        status_state_tx.send_modify(|state| {
                            state.pool.apply_quality_snapshot(&quality_snapshot);
                        });
                        match st {
                        dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, pool_target_difficulty, achieved_difficulty, meta } => {
                            let timestamp_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let target_difficulty = if pool_target_difficulty > 0.0 {
                                pool_target_difficulty
                            } else {
                                current_pool_difficulty
                            }
                            .max(1.0);
                            let achieved_difficulty = achieved_difficulty
                                .filter(|value| value.is_finite() && *value > 0.0);
                            let lucky_share = achieved_difficulty
                                .map(|difficulty| difficulty >= target_difficulty * 10.0)
                                .unwrap_or(false);
                            let _ = mining_sync_status_tx.send(
                                dcentrald_api::websocket::build_mining_sync_message(
                                    &dcentrald_api::websocket::WsMiningSyncMessage {
                                        msg_type: "mining_sync".to_string(),
                                        timestamp_ms,
                                        event: if lucky_share {
                                            dcentrald_api::websocket::WsMiningSyncEventKind::LuckyShare
                                        } else {
                                            dcentrald_api::websocket::WsMiningSyncEventKind::ShareAccepted
                                        },
                                        chain_id: None,
                                        count: Some(1),
                                        job_id: Some(job_id.clone()),
                                        difficulty: achieved_difficulty,
                                        target_difficulty: Some(target_difficulty),
                                        intensity: Some(0.75),
                                        error_code: None,
                                        error_msg: None,
                                    },
                                ),
                            );
                            dcentrald_api::push_recent_share_event(
                                &recent_share_history_status,
                                dcentrald_api::RecentShareEvent {
                                    timestamp_ms,
                                    result: "accepted".to_string(),
                                    job_id: job_id.clone(),
                                    difficulty: achieved_difficulty,
                                    target_difficulty: Some(target_difficulty),
                                    error_code: None,
                                    error_msg: None,
                                    worker_name: meta.as_ref().map(|meta| meta.share.worker_name.clone()),
                                    nonce: meta.as_ref().map(|meta| meta.share.nonce.clone()),
                                    ntime: meta.as_ref().map(|meta| meta.share.ntime.clone()),
                                    extranonce2: meta.as_ref().map(|meta| meta.share.extranonce2.clone()),
                                    version_bits: meta.as_ref().and_then(|meta| meta.share.version_bits.clone()),
                                    version: meta.as_ref().map(|meta| meta.share.version),
                                    protocol_meta_present: meta.is_some(),
                                },
                            );
                            status_state_tx.send_modify(|state| {
                                state.accepted += 1;
                                state.pool.difficulty = target_difficulty;
                                state.pool.last_share_at = timestamp_ms / 1000;
                            });
                            info!(job_id = %job_id, pool_target_difficulty = target_difficulty, achieved_difficulty, "SHARE ACCEPTED");
                        }
                        dcentrald_stratum::types::StratumStatus::ShareRejected { job_id, error_code, error_msg, meta } => {
                            let timestamp_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let _ = mining_sync_status_tx.send(
                                dcentrald_api::websocket::build_mining_sync_message(
                                    &dcentrald_api::websocket::WsMiningSyncMessage {
                                        msg_type: "mining_sync".to_string(),
                                        timestamp_ms,
                                        event: dcentrald_api::websocket::WsMiningSyncEventKind::ShareRejected,
                                        chain_id: None,
                                        count: Some(1),
                                        job_id: Some(job_id.clone()),
                                        difficulty: None,
                                        target_difficulty: Some(current_pool_difficulty.max(1.0)),
                                        intensity: Some(0.75),
                                        error_code: Some(error_code),
                                        error_msg: Some(error_msg.clone()),
                                    },
                                ),
                            );
                            dcentrald_api::push_recent_share_event(
                                &recent_share_history_status,
                                dcentrald_api::RecentShareEvent {
                                    timestamp_ms,
                                    result: "rejected".to_string(),
                                    job_id: job_id.clone(),
                                    difficulty: None,
                                    target_difficulty: Some(current_pool_difficulty.max(1.0)),
                                    error_code: Some(error_code),
                                    error_msg: Some(error_msg.clone()),
                                    worker_name: meta.as_ref().map(|meta| meta.share.worker_name.clone()),
                                    nonce: meta.as_ref().map(|meta| meta.share.nonce.clone()),
                                    ntime: meta.as_ref().map(|meta| meta.share.ntime.clone()),
                                    extranonce2: meta.as_ref().map(|meta| meta.share.extranonce2.clone()),
                                    version_bits: meta.as_ref().and_then(|meta| meta.share.version_bits.clone()),
                                    version: meta.as_ref().map(|meta| meta.share.version),
                                    protocol_meta_present: meta.is_some(),
                                },
                            );
                            status_state_tx.send_modify(|state| {
                                state.rejected += 1;
                                state.pool.difficulty = current_pool_difficulty.max(1.0);
                            });
                            warn!(job_id = %job_id, error = %error_msg, "SHARE REJECTED");
                        }
                        dcentrald_stratum::types::StratumStatus::DifficultyChanged(d) => {
                            current_pool_difficulty = d;
                            status_state_tx.send_modify(|state| {
                                state.pool.difficulty = d;
                            });
                            info!("Pool difficulty: {}", d);
                        }
                        dcentrald_stratum::types::StratumStatus::StateChanged(state) => {
                            let status_str = match state {
                                dcentrald_stratum::types::StratumState::Disconnected => "Disconnected",
                                dcentrald_stratum::types::StratumState::Connecting => "Connecting",
                                dcentrald_stratum::types::StratumState::Authorized => "Authorized",
                                dcentrald_stratum::types::StratumState::Mining => "Alive",
                                dcentrald_stratum::types::StratumState::Donating => "Donating",
                                dcentrald_stratum::types::StratumState::AuthFailed => "AuthFailed",
                            };
                            status_state_tx.send_modify(|miner_state| {
                                miner_state.pool.status = status_str.to_string();
                            });
                            let pool_authorized = matches!(status_str, "Authorized" | "Alive" | "Donating");
                            let authorize_state = match status_str {
                                "Alive" => "mining",
                                other => other,
                            }
                            .to_ascii_lowercase();
                            let timestamp_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let _ = mining_sync_status_tx.send(
                                dcentrald_api::websocket::build_mining_sync_message_with_fields(
                                    &dcentrald_api::websocket::WsMiningSyncMessage {
                                        msg_type: "mining_sync".to_string(),
                                        timestamp_ms,
                                        event: dcentrald_api::websocket::WsMiningSyncEventKind::AuthorizeState,
                                        chain_id: None,
                                        count: Some(1),
                                        job_id: None,
                                        difficulty: None,
                                        target_difficulty: None,
                                        intensity: None,
                                        error_code: None,
                                        error_msg: None,
                                    },
                                    vec![
                                        ("pool_authorized", serde_json::json!(pool_authorized)),
                                        ("authorize_state", serde_json::json!(authorize_state)),
                                    ],
                                ),
                            );
                            info!("Pool: {:?}", state)
                        }
                        dcentrald_stratum::types::StratumStatus::Sv2CustomJobDeclared { channel_id, request_id, template_id } => {
                            let updated_at_s = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            status_state_tx.send_modify(|state| {
                                state.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                    status: "declared".to_string(),
                                    channel_id: Some(channel_id),
                                    request_id: Some(request_id),
                                    template_id: Some(template_id),
                                    job_id: None,
                                    last_error: None,
                                    updated_at_s,
                                });
                            });
                        }
                        dcentrald_stratum::types::StratumStatus::Sv2CustomJobAccepted { channel_id, request_id, template_id, job_id } => {
                            let updated_at_s = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            status_state_tx.send_modify(|state| {
                                state.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                    status: "accepted".to_string(),
                                    channel_id: Some(channel_id),
                                    request_id: Some(request_id),
                                    template_id: Some(template_id),
                                    job_id: Some(job_id),
                                    last_error: None,
                                    updated_at_s,
                                });
                            });
                        }
                        dcentrald_stratum::types::StratumStatus::Sv2CustomJobRejected { channel_id, request_id, template_id, reason } => {
                            let updated_at_s = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            status_state_tx.send_modify(|state| {
                                state.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                    status: "rejected".to_string(),
                                    channel_id: Some(channel_id),
                                    request_id: Some(request_id),
                                    template_id,
                                    job_id: None,
                                    last_error: Some(reason.clone()),
                                    updated_at_s,
                                });
                            });
                        }
                        _ => {}
                        }
                    }
                }
            }
        });

        // ---- Phase 4.5: Serial I/O thread ----
        // Blocking serial reads (VTIME=100ms) cannot run in async context —
        // they block the tokio executor and starve job_rx/dispatch_timer.
        // Solution: dedicated thread owns serial port, handles both reads and writes.
        let (nonce_tx, mut nonce_rx) = mpsc::channel::<Vec<u8>>(256);
        let work_queue_depth = if is_bm1362 {
            BM1362_SERIAL_WORK_QUEUE_DEPTH
        } else {
            DEFAULT_SERIAL_WORK_QUEUE_DEPTH
        };
        let tx_burst_per_loop = if is_bm1362 {
            BM1362_SERIAL_TX_BURST
        } else {
            DEFAULT_SERIAL_TX_BURST
        };
        let tx_before_rx = is_bm1362;
        let work_queue: Arc<Mutex<VecDeque<Vec<u8>>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(work_queue_depth)));
        let work_queue_io = Arc::clone(&work_queue);

        // Keep VTIME=1 (100ms) — proven to work. Bounded queue provides pipeline depth.

        let reader_shutdown = runtime_threads.cancellation_token();
        let parsed_uart_trans_chains = if is_bm1362 {
            Self::am3_bb_uart_trans_chains_from_serial_device(&serial_device)
        } else {
            None
        };
        if parsed_uart_trans_chains.is_some() && !Self::am3_bb_uart_trans_lab_enabled() {
            warn!(
                serial_device = %serial_device,
                "BM1362 /dev/ttyO* uart_trans routing skipped by default; set DCENT_AM3_BB_ENABLE_UART_TRANS_LAB=1 only for R6-9 capture work"
            );
        }
        let am3_bb_uart_trans_chains = if Self::am3_bb_uart_trans_lab_enabled() {
            parsed_uart_trans_chains
        } else {
            None
        };
        let thread_name = if am3_bb_uart_trans_chains.is_some() {
            "am3-bb-uart-trans-io"
        } else {
            "s19j-serial-io"
        };
        let handle = if let Some(selected_chains) = am3_bb_uart_trans_chains {
            info!(
                serial_device = %serial_device,
                ?selected_chains,
                "Routing BM1362 /dev/ttyO* work through userspace uart_trans"
            );
            drop(serial);
            Self::spawn_am3_bb_uart_trans_io_thread(
                serial_device.clone(),
                selected_chains,
                work_queue_io,
                nonce_tx,
                reader_shutdown,
                work_queue_depth,
                tx_burst_per_loop,
            )?
        } else {
            std::thread::Builder::new()
                .name("s19j-serial-io".to_string())
                .spawn(move || {
                info!(
                    work_queue_depth,
                    tx_burst_per_loop,
                    tx_before_rx,
                    "Serial I/O thread started (VTIME=1, bounded queue, family-specific TX scheduler)"
                );
                let mut total_frames: u64 = 0;
                let mut total_tx: u64 = 0;
                let mut last_diag = Instant::now();

                // VTIME=1 (100ms) provides natural pacing. For BM1362, we send a
                // much larger burst before the blocking read so a dead RX path does
                // not cap us at ~30 TX/s and starve the ASIC scheduler.
                let mut drain_tx = |limit: usize, total_tx: &mut u64| -> bool {
                    for _ in 0..limit {
                        let frame = work_queue_io
                            .lock()
                            .unwrap_or_else(|e| {
                                tracing::warn!("work_queue mutex poisoned, recovering");
                                e.into_inner()
                            })
                            .pop_front();
                        let Some(frame) = frame else { break; };
                        if let Err(e) = serial.send_work(&frame) {
                            warn!(error = %e, "Serial work send failed");
                            return false;
                        }
                        *total_tx += 1;
                    }
                    true
                };

                loop {
                    if reader_shutdown.is_cancelled() {
                        break;
                    }

                    if tx_before_rx && !drain_tx(tx_burst_per_loop, &mut total_tx) {
                        break;
                    }

                    // === RX: read one nonce frame (VTIME=1, blocks up to 100ms) ===
                    match serial.read_nonce_response() {
                        Ok(Some(data)) => {
                            total_frames += 1;
                            if nonce_tx.blocking_send(data).is_err() {
                                break;
                            }
                            // If we got data, try draining more without blocking
                            for _ in 0..31 {
                                match serial.read_nonce_response() {
                                    Ok(Some(data)) => {
                                        total_frames += 1;
                                        if nonce_tx.blocking_send(data).is_err() {
                                            break;
                                        }
                                    }
                                    _ => break,
                                }
                            }
                        }
                        Ok(None) => {} // timeout — 100ms elapsed, do TX
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }

                    if !tx_before_rx && !drain_tx(tx_burst_per_loop, &mut total_tx) {
                        break;
                    }

                    // Diagnostic: log frame count every 10 seconds
                    if last_diag.elapsed() > Duration::from_secs(10) {
                        info!(
                            total_frames,
                            total_tx,
                            "Serial I/O: {} RX nonces, {} TX work sent",
                            total_frames,
                            total_tx,
                        );
                        last_diag = Instant::now();
                    }
                }
                info!("Serial I/O thread exited");
            })
                .context("Failed to spawn serial I/O thread")?
        };
        runtime_threads.push(thread_name, handle);

        // ---- Phase 4b: Start API servers (dashboard, REST, CGMiner, WebSocket) ----
        let (mode_tx, mode_rx) =
            tokio::sync::watch::channel(dcentrald_api::OperatingMode::Standard);
        let (stats_tx, _) = tokio::sync::broadcast::channel(64);
        let (diag_tx, _) = tokio::sync::broadcast::channel(16);
        let (auto_tx, _) = tokio::sync::broadcast::channel(16);
        let (power_tx, power_rx) =
            tokio::sync::watch::channel(dcentrald_autotuner::LivePowerEstimate::default());
        let (auto_status_tx, auto_status_rx) =
            tokio::sync::watch::channel(dcentrald_autotuner::AutotunerRuntimeStatus::default());
        let (auto_eff_tx, auto_eff_rx) =
            tokio::sync::watch::channel(None::<dcentrald_autotuner::EfficiencySnapshot>);
        let (auto_health_tx, auto_health_rx) =
            tokio::sync::watch::channel(None::<dcentrald_autotuner::LiveChipHealthState>);
        let (auto_telem_tx, auto_telem_rx) =
            tokio::sync::watch::channel(dcentrald_autotuner::TelemetryExportState::default());

        let api_config = dcentrald_api::ApiConfig {
            cgminer_port: self.config.api.cgminer_port,
            http_port: self.config.api.http_port,
            http_bind: self.config.api.http_bind.clone(),
            websocket_enabled: self.config.api.websocket,
            websocket_tickets: self.config.api.websocket_tickets,
            cgminer_bind_lan: self.config.api.cgminer_bind_lan,
            cgminer_lan_writes: self.config.api.cgminer_lan_writes,
            metrics_require_auth: self.config.api.metrics_require_auth,
            // W13.D1: dev-mode boot-timeline gate. See ApiConfig docs.
            expose_boot_timeline: self.config.api.expose_boot_timeline,
        };
        let power_calibration = std::sync::Arc::new(std::sync::RwLock::new(
            self.config.power.calibration.clone().unwrap_or_default(),
        ));
        let psu_lock = std::sync::Arc::new(std::sync::Mutex::new(()));
        let hardware_mutation_gate = dcentrald_hal::platform::HardwareMutationGate::new_open();

        let history_path = history::storage_path();
        let history_buffer = HistoryBuffer::load(&history_path);
        let history_data = Arc::new(Mutex::new(history::serialize_for_api(
            &history_buffer.samples(),
        )));
        let solar_history = Arc::new(Mutex::new(Vec::new()));
        let history_state_rx = state_rx.clone();
        let history_power_rx = power_rx.clone();
        let mining_pipeline_snapshot_rx = if self.config.mining.pipeline_snapshot.enabled {
            Some(
                dcentrald_api::mining_pipeline_snapshot::spawn_mining_pipeline_snapshot_publisher(
                    &mining_sync_tx,
                    self.config.mining.pipeline_snapshot.stale_after_ms,
                ),
            )
        } else {
            None
        };

        let app_state = std::sync::Arc::new(dcentrald_api::AppState {
            state_rx: state_rx.clone(),
            mode_rx: mode_rx.clone(),
            stats_tx: stats_tx.clone(),
            mining_sync_tx: mining_sync_tx.clone(),
            mining_pipeline_snapshot_rx,
            mining_pipeline_snapshot_stale_after_ms: self
                .config
                .mining
                .pipeline_snapshot
                .stale_after_ms
                .max(1),
            diagnostic_progress_tx: diag_tx.clone(),
            diagnostic_service: Arc::new(tokio::sync::Mutex::new(
                dcentrald_diagnostics::DiagnosticService::new(diag_tx),
            )),
            autotuner_tx: auto_tx,
            config: api_config,
            network_block: self.config.network_block.clone(),
            jd_status_rx,
            profile_path: "/tmp/profiles".to_string(),
            led_tx: None,
            led_status_rx: None,
            curtailment: std::sync::Arc::new(tokio::sync::Mutex::new(
                dcentrald_thermal::curtailment::CurtailmentController::new(),
            )),
            power_rx: power_rx.clone(),
            power_calibration,
            psu_lock,
            hardware_mutation_gate: hardware_mutation_gate.clone(),
            autotuner_status_rx: auto_status_rx,
            autotuner_efficiency_rx: auto_eff_rx,
            autotuner_chip_health_rx: auto_health_rx,
            autotuner_telemetry_rx: auto_telem_rx,
            autotuner_command_tx: None,
            history_data: history_data.clone(),
            recent_share_history: recent_share_history.clone(),
            local_reject_ring: std::sync::Arc::new(std::sync::Mutex::new(
                dcentrald_api_types::share_validation::LocalRejectRing::with_default_capacity(),
            )),
            boot_progress: std::sync::Arc::new(dcentrald_api::BootProgressSnapshot::new()),
            audit_ring: std::sync::Arc::new(std::sync::Mutex::new(
                dcentrald_api_types::audit_log::AuditRing::with_default_capacity(),
            )),
            room_temp_c10: std::sync::atomic::AtomicU32::new(0),
            hardware_info: std::sync::Arc::new(std::sync::Mutex::new(
                dcentrald_api::HardwareInfo::default(),
            )),
            // W13.D1 boot phase tracker — default Generic(Booting), live
            // wiring deferred to W14+.
            boot_phase_tracker: std::sync::Arc::new(
                dcentrald_api::boot_phase_tracker::BootPhaseTracker::new(),
            ),
            offgrid_rx: None,
            pid_state_rx: None,
            pid_command_tx: None,
            solar_rx: None,
            solar_history,
            // P3-2: read-only status handlers read this in-memory mirror of
            // dcentrald.toml instead of re-parsing the file every request.
            config_cache: std::sync::Arc::new(dcentrald_api::ConfigTableCache::new()),
        });

        match dcentrald_api::start_api_servers(app_state).await {
            Ok(_) => info!(
                http_port = self.config.api.http_port,
                cgminer_port = self.config.api.cgminer_port,
                "API servers online — dashboard + CGMiner + WebSocket"
            ),
            Err(e) => warn!(error = %e, "Failed to start API servers — mining without monitoring"),
        }

        let _metrics_csv_handle = crate::metrics_export::spawn_metrics_csv_task(
            self.shutdown.clone(),
            state_rx.clone(),
            power_rx.clone(),
        );

        let history_shutdown = self.shutdown.clone();
        let history_buffer_task = history_buffer.clone();
        let history_data_task = history_data.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(history::HISTORY_INTERVAL_S));
            loop {
                tokio::select! {
                    _ = history_shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        let timestamp_s = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let state = history_state_rx.borrow().clone();
                        let power = history_power_rx.borrow().clone();
                        let sample = history::sample_from_runtime(timestamp_s, &state, &power);
                        history_buffer_task.push(sample);

                        if let Ok(mut guard) = history_data_task.lock() {
                            *guard = history::serialize_for_api(&history_buffer_task.samples());
                        }
                    }
                }
            }
        });

        // ---- Phase 5: Mining loop ----
        info!(
            "=== MINING ACTIVE — {} {} chips on {} at {} MHz ===",
            chip_count,
            if is_bm1398 {
                "BM1398"
            } else if is_bm1366 {
                "BM1366"
            } else if is_bm1370 {
                "BM1370"
            } else if is_bm1368 {
                "BM1368"
            } else {
                "BM1362"
            },
            serial_device,
            target_freq
        );

        let mut work_builder = dcentrald_stratum::share_pipeline::WorkBuilder::new();
        let mut current_job: Option<dcentrald_stratum::types::JobTemplate> = None;
        let mut asic_job_id: u8 = 0;
        let history_per_id = if is_bm1398 {
            BM1398_WORK_HISTORY_PER_ID
        } else {
            WORK_HISTORY_PER_ID
        };
        let mut work_history: Vec<VecDeque<WorkEntry>> = (0..128)
            .map(|_| VecDeque::with_capacity(history_per_id))
            .collect();
        let mut dispatch_generation: u64 = 0;
        let mut seen_shares: std::collections::HashSet<(u64, u32, u8)> =
            std::collections::HashSet::new();

        let mut total_work: u64 = 0;
        let mut total_nonces: u64 = 0;
        let mut shares_submitted: u64 = 0;
        let start_time = Instant::now();
        let mut last_hr_time = Instant::now();
        let mut hr_nonces: u64 = 0;

        let dispatch_ms = if is_bm1362 {
            BM1362_DISPATCH_INTERVAL_MS
        } else if is_bm1366 {
            (2000u64 / chip_count.max(1) as u64).max(10)
        } else if is_bm1368 || is_bm1370 {
            BM1368_DISPATCH_INTERVAL_MS
        } else {
            50
        };
        let mut dispatch_timer = tokio::time::interval(Duration::from_millis(dispatch_ms));
        let mut hashrate_timer = tokio::time::interval(Duration::from_secs(5));
        let mut mining_sync_timer = tokio::time::interval(Duration::from_millis(250));
        let mut pending_dispatches = 0u32;
        let mut pending_nonces = 0u32;

        // Thermal management for NoPic/Amlogic platforms
        let mut thermal_timer = tokio::time::interval(Duration::from_secs(2));
        // The pre-energize cooling owner computed this retained ceiling once;
        // every steady-state command below uses the same value.
        let mut latest_temp_c: f32 = 0.0;
        let mut latest_fan_pwm: u8 = if amlogic_fan.is_some() {
            // THERMAL-2: seed with the degraded-tach-clamped ceiling, matching the
            // startup `fan.set_speed(effective_fan_max_pwm)` above.
            effective_fan_max_pwm
        } else {
            10
        };
        let mut latest_fan_rpm: u32 = 0;
        let mut latest_per_fan: Vec<(u8, u32)> = Vec::new();
        let thermal_started_at = nopic_energized_at.unwrap_or_else(Instant::now);
        let mut consecutive_missing_temp_ticks: u8 = 0;
        let mut early_safe_off_receipt: Option<NoPicSafeOffReceipt> = None;
        let mut terminal_safety_error: Option<anyhow::Error> = None;

        if let Some(watchdog) = nopic_watchdog.as_mut() {
            // The serial actor, fan owner, and thermal branch now exist. The
            // worker snapshots liveness at this transition; the next completed
            // thermal tick must advance it, so zero is never an unlimited grace.
            watchdog.enter_mining().await?;
        }

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => { info!("Shutdown"); break; }

                // Thermal control loop (every 2 seconds)
                _ = thermal_timer.tick(), if amlogic_fan.is_some() => {
                    let Some(thermal_owner) = amlogic_power_thermal.clone() else {
                        terminal_safety_error = Some(anyhow::anyhow!(
                            "Amlogic cooling owner exists without retained power/thermal ownership"
                        ));
                        break;
                    };
                    let Some(fan_sampler) = amlogic_fan.clone() else {
                        terminal_safety_error = Some(anyhow::anyhow!(
                            "Amlogic thermal tick lost its retained cooling owner"
                        ));
                        break;
                    };
                    // Both operations are synchronous kernel/sysfs work. Run them
                    // concurrently on the blocking pool so the Tokio worker can
                    // continue processing shutdown and network tasks.
                    let temperature_worker = tokio::task::spawn_blocking(move || {
                        thermal_owner.read_board_temperatures(
                            Instant::now() + Duration::from_millis(750),
                        )
                    });
                    let fan_worker = sample_fan_tach(fan_sampler);
                    let (temperature_result, fan_result) = tokio::join!(temperature_worker, fan_worker);
                    let temperature_snapshot = match temperature_result {
                        Ok(snapshot) => snapshot,
                        Err(join_error) => {
                            error!(%join_error, "Amlogic thermal polling worker failed; cutting hash power");
                            match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                Err(safe_off_error) => error!(%safe_off_error, "Amlogic thermal-worker safe-off did not complete"),
                            }
                            terminal_safety_error = Some(anyhow::anyhow!(
                                "Amlogic thermal polling worker failed: {join_error}"
                            ));
                            break;
                        }
                    };
                    let fan_snapshot = match fan_result {
                        Ok(snapshot) => snapshot,
                        Err(sample_error) => {
                            error!(%sample_error, "Amlogic fan polling worker failed; cutting hash power");
                            match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                Err(safe_off_error) => error!(%safe_off_error, "Amlogic fan-worker safe-off did not complete"),
                            }
                            terminal_safety_error = Some(anyhow::anyhow!(
                                "Amlogic fan polling worker failed: {sample_error}"
                            ));
                            break;
                        }
                    };
                    let FanTachSnapshot {
                        available: fan_tach_available,
                        expected_channels: expected_fan_channels,
                        readings,
                    } = fan_snapshot;
                    latest_per_fan = readings;
                    latest_fan_rpm = latest_per_fan
                        .iter()
                        .map(|(_, rpm)| *rpm)
                        .min()
                        .unwrap_or(0);
                    let fan_rpms = latest_per_fan
                        .iter()
                        .map(|(_, rpm)| *rpm)
                        .collect::<Vec<_>>();
                    let fan_safety_state = nopic_fan_safety.observe_required_airflow(
                        fan_tach_available,
                        latest_fan_pwm,
                        expected_fan_channels,
                        &fan_rpms,
                    );
                    if let FanTachSafetyState::Debouncing {
                        consecutive_below_minimum,
                        failure_ticks,
                        minimum_credible_rpm,
                    } = fan_safety_state
                    {
                        warn!(
                            consecutive_below_minimum,
                            failure_ticks,
                            minimum_credible_rpm,
                            readings = ?latest_per_fan,
                            "Amlogic below-threshold RPM observation is inside the bounded fan-failure debounce"
                        );
                    }
                    if nopic_fan_loop_disposition(&fan_safety_state)
                        == NoPicFanLoopDisposition::SafeOffAndStop
                    {
                        error!(
                            ?fan_safety_state,
                            readings = ?latest_per_fan,
                            "Amlogic fan safety admission revoked; cutting hash power"
                        );
                        match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                            Ok(receipt) => early_safe_off_receipt = Some(receipt),
                            Err(safe_off_error) => error!(%safe_off_error, "Amlogic fan-safety checked safe-off did not complete"),
                        }
                        let _ = crate::restart::schedule_daemon_restart(
                            "amlogic_fan_safety_restart",
                            Duration::from_secs(AMLOGIC_THERMAL_RESTART_DELAY_S),
                        );
                        terminal_safety_error = Some(anyhow::anyhow!(
                            "Amlogic fan safety admission revoked: {fan_safety_state:?}"
                        ));
                        break;
                    }
                    let required_coverage = temperature_snapshot.required_coverage();
                    let dangerous_observation = temperature_snapshot
                        .hottest_celsius()
                        .is_some_and(|temp| temp >= self.config.thermal.dangerous_temp_c as f32);
                    if let Some(ref fan) = amlogic_fan {
                        if !required_coverage.is_complete() && !dangerous_observation {
                            latest_temp_c = 0.0;
                            // stale-temp / no thermal proof: cap fans at the PWM-30 home cap.
                            // NoPic (am3-aml) has no die-temp fallback, and disable_psu fires
                            // after the startup grace — so blasting fans here is never justified.
                            // ("stale-temp ... ALL must cap at
                            // PWM 30"). swarm wf_e0647147 GAP #2 (emergency/stale arm).
                            // THERMAL-2: use the degraded-tach-clamped ceiling (then the
                            // PWM-30 stale-temp safety cap). Both only lower the value.
                            let stale_pwm = effective_fan_max_pwm
                                .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                            match fan.set_speed_checked(stale_pwm) {
                                Ok(receipt) => latest_fan_pwm = receipt.observed_pwm(),
                                Err(fan_error) => {
                                    error!(%fan_error, "Amlogic stale-temperature fan command/readback failed; cutting hash power");
                                    match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                        Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                        Err(safe_off_error) => error!(%safe_off_error, "Amlogic fan failure emergency safe-off did not complete"),
                                    }
                                    terminal_safety_error = Some(anyhow::anyhow!(
                                        "Amlogic stale-temperature fan command/readback failed: {fan_error}"
                                    ));
                                    break;
                                }
                            }
                            consecutive_missing_temp_ticks = consecutive_missing_temp_ticks.saturating_add(1);

                            if thermal_started_at.elapsed() >= Duration::from_secs(AMLOGIC_TEMP_STARTUP_GRACE_S)
                                && consecutive_missing_temp_ticks >= AMLOGIC_TEMP_MISS_LIMIT
                            {
                                error!(
                                    required_slots = ?required_coverage.required_slots(),
                                    missing_slots = ?required_coverage.missing_slots(),
                                    missing_ticks = consecutive_missing_temp_ticks,
                                    grace_s = AMLOGIC_TEMP_STARTUP_GRACE_S,
                                    "Required Amlogic board-temperature coverage remained incomplete after startup grace — shutting down NoPic mining for safety"
                                );
                                match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                    Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                    Err(safe_off_error) => error!(%safe_off_error, "Amlogic missing-temperature checked safe-off did not complete"),
                                }
                                let _ = crate::restart::schedule_daemon_restart(
                                    "amlogic_missing_temps_restart",
                                    Duration::from_secs(AMLOGIC_THERMAL_RESTART_DELAY_S),
                                );
                                terminal_safety_error = Some(anyhow::anyhow!(
                                    "Required Amlogic board-temperature coverage remained incomplete after the startup grace"
                                ));
                                break;
                            }
                        } else {
                            consecutive_missing_temp_ticks = 0;
                            let max_temp = temperature_snapshot
                                .hottest_celsius()
                                .unwrap_or(0.0);
                            latest_temp_c = max_temp;
                            if max_temp >= self.config.thermal.dangerous_temp_c as f32 {
                                error!(temp = max_temp, "DANGEROUS TEMP — emergency PSU disable!");
                                // EmergencyShutdown: cap fans at the PWM-30 home cap (cut-hash-
                                // before-noise — disable_psu fires next, removing the heat source,
                                // so a fan jet is never justified).
                                // ("EmergencyShutdown ... ALL must cap at PWM 30"). swarm GAP #2.
                                // THERMAL-2: degraded-tach-clamped ceiling, then PWM-30
                                // emergency cap (cut-hash-before-noise — disable_psu next).
                                let emergency_pwm = effective_fan_max_pwm
                                    .min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                                match fan.set_speed_checked(emergency_pwm) {
                                    Ok(receipt) => latest_fan_pwm = receipt.observed_pwm(),
                                    Err(fan_error) => {
                                        error!(%fan_error, "Amlogic emergency fan command/readback failed; cutting hash power");
                                        match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                            Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                            Err(safe_off_error) => error!(%safe_off_error, "Amlogic emergency fan failure safe-off did not complete"),
                                        }
                                        terminal_safety_error = Some(anyhow::anyhow!(
                                            "Amlogic emergency fan command/readback failed: {fan_error}"
                                        ));
                                        break;
                                    }
                                }
                                match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                    Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                    Err(safe_off_error) => error!(%safe_off_error, "Amlogic dangerous-temperature checked safe-off did not complete"),
                                }
                                let _ = crate::restart::schedule_daemon_restart(
                                    "amlogic_thermal_restart",
                                    Duration::from_secs(AMLOGIC_THERMAL_RESTART_DELAY_S),
                                );
                                terminal_safety_error = Some(anyhow::anyhow!(
                                    "Amlogic dangerous temperature {max_temp:.1} C triggered emergency shutdown"
                                ));
                                break; // exit mining loop — NoPicPsuGuard::Drop handles cleanup
                            } else if max_temp >= self.config.thermal.hot_temp_c as f32 {
                                warn!(temp = max_temp, "HOT — fans to profile max");
                                // THERMAL-2: degraded-tach-clamped ceiling.
                                // F-02: also clamp to PWM_SAFETY_MAX for symmetry with
                                // the DANGEROUS/emergency arm above. effective_fan_max_pwm
                                // is already ≤ the home cap on a correctly-configured unit
                                // (home cap ≤ 30), so this is strictly-safer defense
                                // against a misconfig where the profile ceiling exceeds the
                                // hard fan cap — a HOT event must never blast past it.
                                let hot_pwm =
                                    effective_fan_max_pwm.min(dcentrald_hal::fan::PWM_SAFETY_MAX);
                                match fan.set_speed_checked(hot_pwm) {
                                    Ok(receipt) => latest_fan_pwm = receipt.observed_pwm(),
                                    Err(fan_error) => {
                                        error!(%fan_error, "Amlogic hot-state fan command/readback failed; cutting hash power");
                                        match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                            Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                            Err(safe_off_error) => error!(%safe_off_error, "Amlogic hot-state fan failure safe-off did not complete"),
                                        }
                                        terminal_safety_error = Some(anyhow::anyhow!(
                                            "Amlogic hot-state fan command/readback failed: {fan_error}"
                                        ));
                                        break;
                                    }
                                }
                            } else {
                                // Proportional fan control: scale between min and the
                                // THERMAL-2 degraded-tach-clamped max PWM.
                                let target = self.config.thermal.target_temp_c as f32;
                                // Guard the denominator: a target_temp_c of 30 makes it 0,
                                // and 0/0 (max_temp==30) yields NaN → `NaN as u8 == 0`, which
                                // would command fan PWM 0 in the cool branch instead of
                                // min_pwm. `.max(1.0)` keeps the ratio finite and fail-safe.
                                let ratio =
                                    ((max_temp - 30.0) / (target - 30.0).max(1.0)).clamp(0.0, 1.0);
                                let min_pwm = effective_fan_min_pwm as f32;
                                let max_pwm = effective_fan_max_pwm as f32;
                                let pwm = (min_pwm + ratio * (max_pwm - min_pwm)) as u8;
                                match fan.set_speed_checked(pwm) {
                                    Ok(receipt) => latest_fan_pwm = receipt.observed_pwm(),
                                    Err(fan_error) => {
                                        error!(%fan_error, "Amlogic thermal fan command/readback failed; cutting hash power");
                                        match checked_nopic_emergency_safe_off(&mut nopic_psu_guard) {
                                            Ok(receipt) => early_safe_off_receipt = Some(receipt),
                                            Err(safe_off_error) => error!(%safe_off_error, "Amlogic thermal fan failure safe-off did not complete"),
                                        }
                                        terminal_safety_error = Some(anyhow::anyhow!(
                                            "Amlogic thermal fan command/readback failed: {fan_error}"
                                        ));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    nopic_watchdog_liveness.mark_progress();
                }

                Some(job) = job_rx.recv() => {
                    let sync_job_id = job.job_id.clone();
                    if job.clean_jobs {
                        info!(job_id = %job.job_id, "NEW BLOCK");
                        work_history.iter_mut().for_each(VecDeque::clear);
                        seen_shares.clear();
                        work_builder.reset_extranonce2();
                        work_queue.lock().unwrap_or_else(|e| { tracing::warn!("work_queue mutex poisoned"); e.into_inner() }).clear(); // flush stale work
                    }
                    work_builder.set_version_mask(job.version_mask);
                    let _ = mining_sync_tx.send(
                        dcentrald_api::websocket::build_mining_sync_message(
                            &dcentrald_api::websocket::WsMiningSyncMessage {
                                msg_type: "mining_sync".to_string(),
                                timestamp_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                event: if job.clean_jobs {
                                    dcentrald_api::websocket::WsMiningSyncEventKind::CleanJob
                                } else {
                                    dcentrald_api::websocket::WsMiningSyncEventKind::JobReceived
                                },
                                chain_id: None,
                                count: Some(1),
                                job_id: Some(sync_job_id),
                                difficulty: None,
                                target_difficulty: None,
                                intensity: Some(if job.clean_jobs { 1.0 } else { 0.45 }),
                                error_code: None,
                                error_msg: None,
                            },
                        ),
                    );
                    if job.is_flush_only() {
                        info!(
                            job_id = %job.job_id,
                            "Pool switch flush complete; serial dispatch paused until the next pool notify"
                        );
                        current_job = None;
                        continue;
                    }
                    current_job = Some(job);
                }

                _ = dispatch_timer.tick() => {
                    if let Some(ref job) = current_job {
                        let work = work_builder.next_work(job);

                        // BM1362/BM1366 Full Header serial work format (ESP-Miner struct):
                        //   [0]     = job_id (full byte, 0-127)
                        //   [1]     = num_midstates (0x01)
                        //   [2..5]  = starting_nonce (0x00000000)
                        //   [6..9]  = nbits (LE)
                        //   [10..13] = ntime (LE)
                        //   [14..45] = merkle_root (32 bytes, word-reversed)
                        //   [46..77] = prev_block_hash (32 bytes, word-reversed)
                        //   [78..81] = version (LE)
                        // Total payload: 82 bytes
                        // Length byte: 0x56 = 86 = cmd(1) + len(1) + payload(82) + CRC16(2)
                        let work_frame = if is_bm1398 {
                            // BM1398 midstate work format (4 midstates)
                            // Payload: job_id(1) + num_ms(1) + nonce(4) + nbits(4) + ntime(4) + merkle4(4) + 4*midstate(128) = 146
                            let mut payload = vec![0u8; 146];
                            payload[0] = asic_job_id;
                            payload[1] = 0x04; // num_midstates = 4
                            // payload[2..5] = starting_nonce = 0 (already zero)
                            payload[6..10].copy_from_slice(&work.nbits.to_le_bytes());
                            payload[10..14].copy_from_slice(&work.ntime.to_le_bytes());
                            payload[14..18].copy_from_slice(&work.merkle4);
                            // Midstates: 32 bytes each, reversed 32-bit word order
                            for (slot, ms) in work.midstates.iter().enumerate().take(4) {
                                let base = 18 + slot * 32;
                                for i in 0..8 {
                                    let word_idx = 7 - i;
                                    payload[base + i*4..base + i*4 + 4].copy_from_slice(&[
                                        ms[word_idx*4], ms[word_idx*4+1], ms[word_idx*4+2], ms[word_idx*4+3]
                                    ]);
                                }
                            }
                            // Duplicate midstate 0 if fewer than 4 provided
                            if work.midstates.len() < 4 {
                                for slot in work.midstates.len()..4 {
                                    let dst = 18 + slot * 32;
                                    let src_data: Vec<u8> = payload[18..50].to_vec();
                                    payload[dst..dst+32].copy_from_slice(&src_data);
                                }
                            }
                            let mut frame = Vec::with_capacity(148);
                            frame.push(0x21); // header: TYPE_JOB | CMD_WRITE
                            frame.push(0x96); // length: 150 = 2(hdr+len) + 146(payload) + 2(CRC16)
                            frame.extend_from_slice(&payload);
                            frame
                        } else {
                            // BM1362 full-header work format (existing)
                            let mut payload = [0u8; 82];
                            payload[0] = asic_job_id;
                            payload[1] = 0x01; // num_midstates
                            payload[6..10].copy_from_slice(&work.nbits.to_le_bytes());
                            payload[10..14].copy_from_slice(&work.ntime.to_le_bytes());
                            let mr = reverse_32bit_words(&work.merkle_root);
                            payload[14..46].copy_from_slice(&mr);
                            let pbh = reverse_32bit_words(&work.prev_block_hash);
                            payload[46..78].copy_from_slice(&pbh);
                            payload[78..82].copy_from_slice(&work.version.to_le_bytes());
                            let mut frame = Vec::with_capacity(84);
                            frame.push(0x21);
                            frame.push(0x56); // length: 86
                            frame.extend_from_slice(&payload);
                            frame
                        };

                        let history = &mut work_history[asic_job_id as usize];
                        if history.len() >= history_per_id {
                            history.pop_front();
                        }
                        history.push_back(WorkEntry {
                            generation: dispatch_generation,
                            job_id: work.job_id.clone(),
                            extranonce2: work.extranonce2.clone(),
                            ntime: work.ntime,
                            nbits: work.nbits,
                            version: work.version,
                            version_mask: work.version_mask,
                            share_target: work.share_target,
                            prev_block_hash: work.prev_block_hash,
                            merkle_root: work.merkle_root,
                        });
                        dispatch_generation = dispatch_generation.saturating_add(1);

                        asic_job_id = serial_next_asic_job_id(asic_job_id, job_id_increment);
                        total_work += 1;
                        pending_dispatches = pending_dispatches.saturating_add(1);

                        if total_work <= 1 {
                            // Log FULL frame including preamble + CRC (88 bytes on wire)
                            let full_hex: String = {
                                // Reconstruct what send_work() produces
                                let crc = dcentrald_hal::serial_chain::crc16_public(&work_frame);
                                let mut full = vec![0x55u8, 0xAA];
                                full.extend_from_slice(&work_frame);
                                full.push((crc >> 8) as u8);
                                full.push((crc & 0xFF) as u8);
                                full.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
                            };
                            info!("FULL FRAME ON WIRE ({} bytes): {}", work_frame.len() + 4, full_hex);
                        }
                        if total_work <= 3 {
                            let hex: String = work_frame.iter().take(20)
                                .map(|b| format!("{:02X}", b))
                                .collect::<Vec<_>>().join(" ");
                            info!(
                                job_id = asic_job_id.wrapping_sub(job_id_increment) & JOB_ID_MASK,
                                pool_job = %work.job_id,
                                hex = %hex,
                                "WORK #{} sent: {}", total_work, hex,
                            );
                        }

                        {
                            let mut q = work_queue.lock().unwrap_or_else(|e| { tracing::warn!("work_queue mutex poisoned"); e.into_inner() });
                            if q.len() >= work_queue_depth { q.pop_front(); } // drop oldest if full
                            q.push_back(work_frame);
                        }
                    }
                }

                _ = mining_sync_timer.tick() => {
                    watchdog_liveness.fetch_add(1, Ordering::Relaxed);
                    if pending_dispatches > 0 {
                        let _ = mining_sync_tx.send(
                            dcentrald_api::websocket::build_mining_sync_message(
                                &dcentrald_api::websocket::WsMiningSyncMessage {
                                    msg_type: "mining_sync".to_string(),
                                    timestamp_ms: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis() as u64,
                                    event: dcentrald_api::websocket::WsMiningSyncEventKind::DispatchBurst,
                                    chain_id: None,
                                    count: Some(pending_dispatches),
                                    job_id: current_job.as_ref().map(|job| job.job_id.clone()),
                                    difficulty: None,
                                    target_difficulty: None,
                                    intensity: Some((pending_dispatches.min(24) as f32) / 24.0),
                                    error_code: None,
                                    error_msg: None,
                                },
                            ),
                        );
                        pending_dispatches = 0;
                    }

                    if pending_nonces > 0 {
                        let _ = mining_sync_tx.send(
                            dcentrald_api::websocket::build_mining_sync_message(
                                &dcentrald_api::websocket::WsMiningSyncMessage {
                                    msg_type: "mining_sync".to_string(),
                                    timestamp_ms: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis() as u64,
                                    event: dcentrald_api::websocket::WsMiningSyncEventKind::NonceBurst,
                                    chain_id: None,
                                    count: Some(pending_nonces),
                                    job_id: current_job.as_ref().map(|job| job.job_id.clone()),
                                    difficulty: None,
                                    target_difficulty: None,
                                    intensity: Some((pending_nonces.min(64) as f32) / 64.0),
                                    error_code: None,
                                    error_msg: None,
                                },
                            ),
                        );
                        pending_nonces = 0;
                    }
                }

                Some(resp) = nonce_rx.recv() => {
                    if resp.len() < resp_body_len { continue; }

                    total_nonces += 1;
                    hr_nonces += 1;
                    pending_nonces = pending_nonces.saturating_add(1);

                    // BM1362 serial response (9 body bytes after 0xAA 0x55 preamble strip):
                    //   [0..3] = nonce (4 raw bytes from ASIC, big-endian on wire)
                    //   [4]    = midstate_num (always 0 for BM1362)
                    //   [5]    = RESULT: job_id = (byte & 0xF0) >> 1, small_core = byte & 0x0F
                    //   [6..7] = version bits (VH VL, big-endian, shifted << 13)
                    //   [8]    = FLAGS (bit7=1 = job response, bits 4:0 = CRC5)
                    //
                    // NONCE BYTE ORDER (critical for share validation + pool submission):
                    // The ASIC sends nonce bytes in big-endian order on the wire.
                    // ESP-Miner reads them into a packed struct where the u32 field
                    // gets the LE interpretation of those bytes (memcpy on LE ESP32).
                    // For share submission, ESP-Miner formats this u32 as "%08lx".
                    // The pool parses that hex value, does to_le_bytes(), and gets
                    // back the original wire bytes for header hashing.
                    //
                    // We mimic ESP-Miner: interpret the wire bytes as LE u32.
                    // from_le_bytes([resp[0], resp[1], resp[2], resp[3]]) makes
                    // resp[0] = LSB, resp[3] = MSB — same as C packed struct on LE.
                    let nonce = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
                    let id_byte = resp[5];
                    // BM1362 serial response ID byte: job_id encoded as (job_id << 1),
                    // small_core in lower 4 bits (BM1362 has 16 small cores like BM1370).
                    //
                    // BM1362 uses +24 job_id increment (BM1368/BM1370 family), so the
                    // extraction must match ESP-Miner BM1368/BM1370:
                    //   job_id = (id_byte & 0xF0) >> 1
                    //
                    // Example: sent job_id=24 (0x18), ASIC encodes 0x18<<1=0x30,
                    //   response byte 0x36 = 0x30 | small_core=6
                    //   (0x36 & 0xF0) >> 1 = 0x30 >> 1 = 0x18 = 24  CORRECT
                    //
                    // BM1368/BM1370/BM1362: job_id = (id_byte & 0xF0) >> 1, small_core = id_byte & 0x0F
                    let (resp_job_id, midstate_idx, version_bits_raw, flags) = if is_bm1398 {
                        // BM1398: 7-byte body [nonce(4), midstate(1), job_id(1), crc5(1)]
                        // No resp[7] or resp[8] — only 7 bytes after preamble strip
                        let jid = id_byte & 0xFC; // upper 6 bits = job_id
                        (jid, resp[4], 0u16, resp[6])
                    } else if is_bm1366 {
                        (
                            id_byte & 0xF8,
                            resp[4],
                            u16::from_be_bytes([resp[6], resp[7]]),
                            resp[8],
                        )
                    } else {
                        (
                            (id_byte & 0xF0) >> 1,
                            resp[4],
                            u16::from_be_bytes([resp[6], resp[7]]),
                            resp[8],
                        )
                    };

                    if flags & 0x80 == 0 { continue; }

                    if total_nonces <= 10 {
                        if is_bm1398 {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                job_id = resp_job_id,
                                midstate_idx,
                                raw = format_args!("{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                                    resp[0], resp[1], resp[2], resp[3], resp[4], resp[5], resp[6]),
                                "Nonce #{} (BM1398)", total_nonces,
                            );
                        } else {
                            info!(
                                nonce = format_args!("0x{:08X}", nonce),
                                job_id = resp_job_id,
                                midstate_idx,
                                vbits = format_args!("0x{:04X}", version_bits_raw),
                                raw = format_args!("{:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                                    resp[0], resp[1], resp[2], resp[3], resp[4], resp[5], resp[6], resp[7], resp[8]),
                                "Nonce #{}", total_nonces,
                            );
                        }
                    }

                    // resp_job_id derives from a raw hardware RX byte (resp[5]); the
                    // BM1398 (& 0xFC) / BM1366 (& 0xF8) extractions are NOT masked to
                    // work_history's 128-entry range, so a garbage/bit-flipped frame
                    // with bit7 set yields >=128 — a bounds-check panic that, under
                    // panic=abort, takes down the daemon on a single noisy RX byte.
                    // Drop the frame instead of aborting (no-brick; matches the
                    // masked hybrid/FPGA paths).
                    let Some(history) = work_history.get(resp_job_id as usize) else {
                        debug!(resp_job_id, "RX job_id out of work_history range — dropping frame");
                        continue;
                    };
                    if history.is_empty() {
                        debug!(resp_job_id, "Stale (no work history entry)");
                        continue;
                    }

                    let latest_entry = match history.back() {
                        Some(e) => e.clone(),
                        None => { warn!("History empty after non-empty check — skipping nonce"); continue; }
                    };
                    let latest_rolled_version = match serial_rolled_version(
                        &latest_entry,
                        version_bits_raw,
                        is_bm1398,
                        midstate_idx,
                    ) {
                        Some(version) => version,
                        None => {
                            debug!(
                                midstate_idx,
                                resp_job_id,
                                version_bits_raw = format_args!("0x{:04X}", version_bits_raw),
                                "Serial nonce referenced unsupported version metadata"
                            );
                            continue;
                        }
                    };

                    // Full 80-byte header validation.
                    //
                    // NONCE BYTE ORDER FIX (root cause of share validation failure):
                    // The nonce u32 is now parsed via from_le_bytes() (matching ESP-Miner's
                    // packed struct on LE hardware). to_le_bytes() reconstructs the original
                    // wire bytes. format!("{:08x}", nonce) produces the correct pool
                    // submission hex string. Both validation and submission are consistent.
                    //
                    // prev_block_hash: internal header format (reverse_endianness_per_word
                    // already applied by WorkBuilder). merkle_root: raw SHA-256d output.
                    let latest_meets_target = dcentrald_stratum::share_pipeline::validate_full_header(
                        &serial_build_header(&latest_entry, latest_rolled_version, nonce),
                        &latest_entry.share_target,
                    );

                    if total_nonces <= 5 || latest_meets_target {
                        info!(
                            nonce = format_args!("0x{:08X}", nonce),
                            rolled_ver = format_args!("0x{:08X}", latest_rolled_version),
                            vbits = format_args!("0x{:04X}", version_bits_raw),
                            midstate_idx,
                            meets = latest_meets_target,
                            "VALIDATION: meets={}",
                            latest_meets_target,
                        );
                    }

                    if let Some((entry, rolled_version, header)) = history.iter().rev().find_map(|candidate| {
                        let rolled_version = serial_rolled_version(
                            candidate,
                            version_bits_raw,
                            is_bm1398,
                            midstate_idx,
                        )?;
                        let header = serial_build_header(candidate, rolled_version, nonce);
                        if dcentrald_stratum::share_pipeline::validate_full_header(&header, &candidate.share_target) {
                            Some((candidate.clone(), rolled_version, header))
                        } else {
                            None
                        }
                    }) {
                        let distinct_midstates = if is_bm1398 {
                            entry.version_mask != 0
                        } else {
                            version_bits_raw != 0
                        };
                        let dedup_midstate_idx = if distinct_midstates { midstate_idx } else { 0 };
                        let dedup_key = (entry.generation, nonce, dedup_midstate_idx);
                        if !seen_shares.insert(dedup_key) {
                            debug!(
                                generation = entry.generation,
                                nonce = format_args!("0x{:08X}", nonce),
                                midstate_idx = dedup_midstate_idx,
                                "Duplicate serial share candidate ignored"
                            );
                            continue;
                        }
                        if seen_shares.len() > 4096 {
                            let cutoff = dispatch_generation.saturating_sub(2048);
                            seen_shares.retain(|&(generation, _, _)| generation >= cutoff);
                        }

                        let vdelta = rolled_version ^ entry.version;
                        let achieved_difficulty = serial_achieved_difficulty_from_header(&header);
                        let share = dcentrald_stratum::types::ValidShare {
                            worker_name: self.config.pool.worker.clone(),
                            job_id: entry.job_id.clone(),
                            extranonce2: entry.extranonce2.clone(),
                            ntime: format!("{:08x}", entry.ntime),
                            nonce: format!("{:08x}", nonce),
                            version_bits: if vdelta != 0 { Some(format!("{:08x}", vdelta)) } else { None },
                            version: rolled_version,
                            achieved_difficulty,
                        };
                        // BUG FIX (2026-04-11): try_send → send().await to prevent
                        // silently dropping valid shares under backpressure.
                        match share_tx.send(share).await {
                            Ok(()) => {
                                shares_submitted += 1;
                                info!(nonce = format_args!("0x{:08X}", nonce), "SHARE #{}", shares_submitted);
                            }
                            Err(e) => {
                                error!(error = %e, "Share channel closed");
                                break;
                            }
                        }
                    }
                }

                _ = hashrate_timer.tick() => {
                    let elapsed = last_hr_time.elapsed().as_secs_f64();
                    if elapsed > 0.0 && hr_nonces > 0 {
                        let ths = hr_nonces as f64 * hw_difficulty as f64 * 4_294_967_296.0 / elapsed / 1e12;
                        info!("{:.2} TH/s — {} nonces, {} shares, {}s uptime",
                            ths, total_nonces, shares_submitted, start_time.elapsed().as_secs());
                        hr_nonces = 0;
                        last_hr_time = Instant::now();
                    } else {
                        info!(total_work, total_nonces, shares_submitted,
                            uptime = start_time.elapsed().as_secs(),
                            "Mining loop alive — {} work, {} nonces, {}s",
                            total_work, total_nonces, start_time.elapsed().as_secs());
                    }

                    // Publish loop-owned telemetry via send_modify. Each field has
                    // one writer: this loop owns hashrate/chains/fans/uptime and the
                    // status/reducer task owns accepted/rejected plus all live
                    // pool-quality fields. Leaving pool.* untouched here avoids
                    // clobbering failover, donation, SV2, latency, and reject
                    // evidence between status events.
                    let current_ths = if elapsed > 0.0 { total_nonces as f64 * hw_difficulty as f64 * 4_294_967_296.0 / start_time.elapsed().as_secs_f64() / 1e12 } else { 0.0 };
                    let per_fan = latest_per_fan
                        .iter()
                        .copied()
                        .map(|(id, rpm)| dcentrald_api::PerFanReading {
                            id,
                            rpm,
                            // Amlogic/Braiins fan PWM is already a 0-100 duty
                            // value on the serial path; do not rescale it as a
                            // legacy 0-127 S9 FPGA register.
                            pwm_percent: latest_fan_pwm.min(100),
                        })
                        .collect::<Vec<_>>();
                    state_tx.send_modify(|s| {
                        s.hashrate_ghs = current_ths * 1000.0; // TH/s → GH/s
                        s.hashrate_5s_ghs = current_ths * 1000.0;
                        s.chains = vec![dcentrald_api::ChainState {
                            id: 0,
                            chips: chip_count,
                            frequency_mhz: target_freq,
                            voltage_mv: published_voltage_mv,
                            temp_c: latest_temp_c,
                            // Amlogic (am3-aml / NoPic) reads real board sensors via
                            // the retained typed thermal service; label as a board sensor when we have
                            // a reading, else leave the source unknown so the UI shows
                            // "no telemetry" rather than a fabricated 0 °C. There is no
                            // XADC die-temp fallback on this platform.
                            temp_source: if latest_temp_c > 0.0 {
                                Some(dcentrald_api::ChainTempSource::BOARD_SENSOR.to_string())
                            } else {
                                None
                            },
                            hashrate_ghs: current_ths * 1000.0,
                            errors: 0,
                            // FWT-4: derive the per-chain status from the REAL
                            // measured hashrate instead of a hardcoded "mining".
                            // A chain whose rolling hashrate has collapsed to 0
                            // (stalled/dead) must read "active" (enumerated, not
                            // producing), never a falsely-alive "mining". Mirrors
                            // the hybrid path (unique_nonces>0 ? mining : active).
                            status: if current_ths > 0.0 { "mining" } else { "active" }
                                .to_string(),
                        }];
                        s.fans = dcentrald_api::FanState {
                            pwm: latest_fan_pwm,
                            rpm: latest_fan_rpm,
                            per_fan,
                        };
                        s.uptime_s = start_time.elapsed().as_secs();
                        s.firmware_version = "0.4.0".to_string();
                        s.mode = dcentrald_api::OperatingMode::Standard;
                    });
                }
            }
        }

        info!("=== SHUTDOWN ===");
        let watchdog_teardown_result = if let Some(watchdog) = nopic_watchdog.as_mut() {
            watchdog
                .begin_teardown(DEFAULT_WATCHDOG_TEARDOWN_GRACE)
                .await
        } else {
            Ok(())
        };
        // Close every control-plane hardware mutation before actor cancellation
        // and wait for admitted calls to finish. A timeout is retained as
        // negative evidence: teardown still cuts power, but watchdog Disarm is
        // then unreachable.
        let mutation_gate_for_drain = hardware_mutation_gate.clone();
        let mutation_barrier_result = match tokio::task::spawn_blocking(move || {
            mutation_gate_for_drain.close_and_drain(RUNTIME_THREAD_STOP_TIMEOUT)
        })
        .await
        {
            Ok(result) => result.map_err(anyhow::Error::from),
            Err(join_error) => Err(anyhow::anyhow!(
                "hardware mutation barrier worker failed: {join_error}"
            )),
        };
        // Stop future hardware mutations before cancelling the serial actor.
        work_queue
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("work queue mutex poisoned during shutdown; recovering for terminal clear");
                poisoned.into_inner()
            })
            .clear();
        let thread_stop = runtime_threads
            .stop_and_join(RUNTIME_THREAD_STOP_TIMEOUT)
            .await;
        if thread_stop.any_timed_out() {
            warn!(
                timeout_ms = RUNTIME_THREAD_STOP_TIMEOUT.as_millis(),
                "one or more serial runtime threads were detached at the shutdown deadline; using out-of-band hard stop"
            );
            am2_power.hard_stop_out_of_band("runtime-thread-timeout");
        } else {
            am2_power.teardown("shutdown");
        }

        if let Some(watchdog_owner) = nopic_watchdog.take() {
            // Cut hash power before acoustic coast-down. A fan readback failure
            // is degraded shutdown evidence, but must not force a reboot that
            // could re-energize a GPIO already proven low.
            let power_receipt = match early_safe_off_receipt.take() {
                Some(receipt) => receipt,
                None => nopic_psu_guard.safe_off()?,
            };
            if let Some(fan) = amlogic_fan.as_ref() {
                match fan.set_speed_checked(dcentrald_hal::fan::PWM_SAFETY_MAX) {
                    Ok(receipt) => info!(
                        requested_pwm = receipt.requested_pwm(),
                        observed_pwm = receipt.observed_pwm(),
                        "NoPic quiet fan coast-down completed on both PWM channels"
                    ),
                    Err(error) => warn!(
                        %error,
                        "NoPic power is checked low, but quiet fan coast-down readback failed"
                    ),
                }
            }
            watchdog_teardown_result?;
            let mutation_barrier = mutation_barrier_result?;
            let mutation_barriers: [&dyn crate::runtime::safety_watchdog::MutationBarrierEvidence;
                2] = [&mutation_barrier, power_receipt.management_fabric()];
            let permit = WatchdogDisarmPermit::from_evidence_set(
                &mutation_barriers,
                &thread_stop,
                power_receipt.power(),
            )?;
            let closeout = watchdog_owner
                .disarm_and_join(permit, DEFAULT_WATCHDOG_STOP_TIMEOUT)
                .await?;
            match closeout {
                WatchdogCloseoutReceipt::MagicCloseWriteCompletedAndWorkerExitObserved => info!(
                    gpio = power_receipt.power().gpio(),
                    "NoPic watchdog close and worker exit observed after actor quiescence and checked GPIO-low safe-off"
                ),
            }
        }
        if let Err(e) = history_buffer.save(&history_path) {
            warn!(error = %e, path = %history_path.display(), "Failed to persist history to disk");
        }
        if let Some(error) = terminal_safety_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

fn reverse_32bit_words(data: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..(i + 1) * 4].copy_from_slice(&data[(7 - i) * 4..(7 - i + 1) * 4]);
    }
    out
}

#[derive(Clone)]
struct WorkEntry {
    generation: u64,
    job_id: String,
    extranonce2: String,
    ntime: u32,
    nbits: u32,
    version: u32,
    version_mask: u32,
    share_target: [u8; 32],
    prev_block_hash: [u8; 32],
    merkle_root: [u8; 32],
}

fn serial_rolled_version(
    entry: &WorkEntry,
    version_bits_raw: u16,
    is_bm1398: bool,
    midstate_idx: u8,
) -> Option<u32> {
    if is_bm1398 {
        if midstate_idx >= 4 {
            return None;
        }
        if entry.version_mask == 0 {
            return Some(entry.version);
        }

        let mut rolled_version = entry.version;
        for _ in 0..midstate_idx {
            rolled_version =
                dcentrald_stratum::work::increment_bitmask_pub(rolled_version, entry.version_mask);
        }
        return Some(rolled_version);
    }

    // BIP320 reconstruction is unconditional for BM1362-family chips —
    // they roll the BIP320 16-bit field regardless of whether the pool
    // negotiated `mining.configure`. The .135 Amlogic S21 pre-fix run
    // (2026-04-11, 0.023% accept rate) was THIS branch silently dropping
    // ~99.9% of valid hashing work whenever the pool didn't negotiate the
    // mask. The 2026-05-15 .109 XIL milestone confirmed the chip-side
    // behavior. See:
    //
    //
    //   -  F1.
    let (rolled_version, vbits_delta) =
        dcentrald_asic::bm1362::bip320_reconstruct_rolled_version(entry.version, version_bits_raw);

    if entry.version_mask == 0 {
        // Pool didn't negotiate version-rolling. The chip rolled anyway —
        // submit the rolled-version share. validate_full_header upstream is
        // the SOLE gate; pools that understand BIP320 will accept (Public
        // Pool does), pools that don't will reject post-submit.
        return Some(rolled_version);
    }

    if vbits_delta & !entry.version_mask != 0 {
        // Chip rolled bits OUTSIDE the pool's negotiated mask — the share
        // would be rejected post-submit. Drop it locally to avoid spamming
        // the pool with unsanctioned rolls.
        return None;
    }

    Some(rolled_version)
}

fn serial_build_header(entry: &WorkEntry, rolled_version: u32, nonce: u32) -> [u8; 80] {
    let mut header = [0u8; 80];
    header[0..4].copy_from_slice(&rolled_version.to_le_bytes());
    header[4..36].copy_from_slice(&entry.prev_block_hash);
    header[36..68].copy_from_slice(&entry.merkle_root);
    header[68..72].copy_from_slice(&entry.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&entry.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

fn serial_full_header_hash_be(header: &[u8; 80]) -> [u8; 32] {
    let hash = dcentrald_stratum::work::double_sha256(header);
    let mut hash_be = [0u8; 32];
    for i in 0..32 {
        hash_be[i] = hash[31 - i];
    }
    hash_be
}

fn serial_achieved_difficulty_from_header(header: &[u8; 80]) -> Option<f64> {
    let hash_be = serial_full_header_hash_be(header);
    let difficulty = dcentrald_stratum::v1::difficulty::hash_to_difficulty(&hash_be);
    if difficulty.is_finite() && difficulty > 0.0 {
        Some(difficulty)
    } else {
        None
    }
}

fn serial_next_asic_job_id(asic_job_id: u8, job_id_increment: u8) -> u8 {
    asic_job_id.wrapping_add(job_id_increment) & JOB_ID_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MotionFanMock {
        pwm: std::sync::atomic::AtomicU8,
        minimum_motion_pwm: std::sync::atomic::AtomicU8,
        fail_on_command: std::sync::atomic::AtomicUsize,
        commands: Mutex<Vec<u8>>,
    }

    impl FanAccess for MotionFanMock {
        fn set_speed(&self, pwm: u8) {
            self.pwm.store(pwm, Ordering::Release);
            self.commands.lock().unwrap().push(pwm);
        }

        fn set_speed_checked(&self, pwm: u8) -> dcentrald_hal::Result<FanCommandReceipt> {
            self.set_speed(pwm);
            let command_count = self.commands.lock().unwrap().len();
            if self.fail_on_command.load(Ordering::Acquire) == command_count {
                return Err(dcentrald_hal::HalError::Fan(format!(
                    "injected checked fan-command failure at command {command_count}"
                )));
            }
            FanCommandReceipt::from_matching_readback(pwm, self.get_speed_pwm())
        }

        fn get_rpm(&self) -> u32 {
            self.get_per_fan_rpm()
                .into_iter()
                .map(|(_, rpm)| rpm)
                .min()
                .unwrap_or(0)
        }

        fn get_speed_pwm(&self) -> u8 {
            self.pwm.load(Ordering::Acquire)
        }

        fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
            let pwm = self.get_speed_pwm();
            let minimum_motion_pwm = self.minimum_motion_pwm.load(Ordering::Acquire).max(1);
            let rpm = if pwm == 0 {
                0
            } else if pwm >= minimum_motion_pwm {
                1200
            } else {
                // Model one stray edge per second. A positive reading this low
                // must not satisfy the Amlogic credible-motion admission floor.
                30
            };
            (0..4).map(|id| (id, rpm)).collect()
        }

        fn fan_count(&self) -> u8 {
            4
        }
    }

    #[tokio::test]
    async fn preenergize_airflow_envelope_proves_max_then_min_then_restores_max() {
        let concrete = Arc::new(MotionFanMock::default());
        let fan: Arc<dyn FanAccess> = concrete.clone();
        let mut safety = FanTachSafety::with_minimum_credible_rpm(
            DEFAULT_FAN_BELOW_MINIMUM_FAILURE_TICKS,
            dcentrald_hal::platform::amlogic::REQUIRED_AIRFLOW_MIN_RPM,
        );

        let receipt = admit_fan_airflow_envelope(fan, &mut safety, 10, 30)
            .await
            .unwrap();

        assert_eq!(receipt.observed_pwm(), 30);
        assert_eq!(*concrete.commands.lock().unwrap(), [30, 10, 30]);
    }

    #[tokio::test]
    async fn preenergize_airflow_envelope_refuses_low_point_and_restores_maximum() {
        let concrete = Arc::new(MotionFanMock::default());
        concrete.minimum_motion_pwm.store(20, Ordering::Release);
        let fan: Arc<dyn FanAccess> = concrete.clone();
        let mut safety = FanTachSafety::with_minimum_credible_rpm(
            DEFAULT_FAN_BELOW_MINIMUM_FAILURE_TICKS,
            dcentrald_hal::platform::amlogic::REQUIRED_AIRFLOW_MIN_RPM,
        );

        let error = admit_fan_airflow_envelope(fan, &mut safety, 10, 30)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("restored startup ceiling"));
        assert_eq!(concrete.pwm.load(Ordering::Acquire), 30);
        assert_eq!(*concrete.commands.lock().unwrap(), [30, 10, 30]);
    }

    #[tokio::test]
    async fn preenergize_airflow_envelope_reports_low_point_and_restore_failures_together() {
        let concrete = Arc::new(MotionFanMock::default());
        concrete.minimum_motion_pwm.store(20, Ordering::Release);
        concrete.fail_on_command.store(3, Ordering::Release);
        let fan: Arc<dyn FanAccess> = concrete.clone();
        let mut safety = FanTachSafety::with_minimum_credible_rpm(
            DEFAULT_FAN_BELOW_MINIMUM_FAILURE_TICKS,
            dcentrald_hal::platform::amlogic::REQUIRED_AIRFLOW_MIN_RPM,
        );

        let error = admit_fan_airflow_envelope(fan, &mut safety, 10, 30)
            .await
            .unwrap_err();
        let message = error.to_string();

        assert!(message.contains("motion proof failed"));
        assert!(message.contains("restoration also failed"));
        assert!(message.contains("injected checked fan-command failure"));
        assert_eq!(*concrete.commands.lock().unwrap(), [30, 10, 30]);
    }

    #[test]
    fn nopic_fan_loop_disposition_is_terminal_for_every_revoked_state() {
        let continuing = [
            FanTachSafetyState::Healthy,
            FanTachSafetyState::Debouncing {
                consecutive_below_minimum: 1,
                failure_ticks: 3,
                minimum_credible_rpm: 300,
            },
        ];
        for state in continuing {
            assert_eq!(
                nopic_fan_loop_disposition(&state),
                NoPicFanLoopDisposition::Continue
            );
        }

        let terminal = [
            FanTachSafetyState::AirflowNotCommanded,
            FanTachSafetyState::Failed {
                consecutive_below_minimum: 3,
                failure_ticks: 3,
                minimum_credible_rpm: 300,
            },
            FanTachSafetyState::EvidenceUnavailable {
                expected_channels: 4,
                observed_channels: 3,
            },
        ];
        for state in terminal {
            assert_eq!(
                nopic_fan_loop_disposition(&state),
                NoPicFanLoopDisposition::SafeOffAndStop
            );
        }
    }

    #[test]
    fn bm1362_heartbeat_requires_supported_observed_dspic_firmware() {
        assert!(observed_dspic_firmware(None).is_err());
        assert!(observed_dspic_firmware(Some(0x00)).is_err());
        assert!(observed_dspic_firmware(Some(0xff)).is_err());
        assert!(observed_dspic_firmware(Some(0x88)).is_err());
        assert!(matches!(
            observed_dspic_firmware(Some(0x89)).unwrap(),
            DspicFirmware::Fw89
        ));
    }

    #[test]
    fn serial_runtime_has_no_unbrokered_kernel_i2c_fd_or_ioctl_path() {
        let source = include_str!("serial_mining.rs");
        let raw_open = ["std::fs::", "OpenOptions"].concat();
        let raw_ioctl = ["libc::", "ioctl"].concat();
        let raw_bus_type = ["I2c", "Bus::"].concat();
        let platform_raw_open = [".open_i2c", "("].concat();
        let direct_device_open = [".open(\"/dev/", "i2c-"].concat();
        assert!(!source.contains(&raw_open));
        assert!(!source.contains(&raw_ioctl));
        assert!(!source.contains(&raw_bus_type));
        assert!(!source.contains(&platform_raw_open));
        assert!(!source.contains(&direct_device_open));
        assert!(source.contains("refusing an unbrokered /dev/i2c-0 owner"));
    }

    fn sample_entry(version: u32, version_mask: u32) -> WorkEntry {
        WorkEntry {
            generation: 1,
            job_id: "job".to_string(),
            extranonce2: "00000000".to_string(),
            ntime: 0x65a0_b1c2,
            nbits: 0x1703_4219,
            version,
            version_mask,
            share_target: [0xff; 32],
            prev_block_hash: [0x11; 32],
            merkle_root: [0x22; 32],
        }
    }

    #[test]
    fn serial_rolled_version_reconstructs_when_pool_did_not_negotiate_mask() {
        // Updated 2026-05-15 (cross-platform Protocol fix sweep):
        // BM1362-family chips roll BIP320 unconditionally regardless of
        // pool `mining.configure` negotiation. The previous test asserted
        // a "drop on version_bits_raw != 0 when version_mask == 0" early
        // return, which was the silent-drop bug responsible for the .135
        // Amlogic 0.023% accept rate (per
        //  F1).
        // Now we reconstruct the rolled version unconditionally;
        // validate_full_header is the SOLE gate.
        let entry = sample_entry(0x2000_0000, 0);

        // vbits=0 → base_version (no rolling, identity).
        assert_eq!(
            serial_rolled_version(&entry, 0, false, 0),
            Some(0x2000_0000)
        );
        // vbits=1, mask=0 → reconstruct rolled version: (1 << 13) & 0x1FFFE000 = 0x2000;
        // rolled = (0x2000_0000 & !0x1FFFE000) | 0x2000 = 0x2000_2000.
        assert_eq!(
            serial_rolled_version(&entry, 1, false, 0),
            Some(0x2000_2000)
        );
        // vbits with the BIP320 field maximally set (vbits=0xFFFF) →
        // delta = 0x1FFFE000 (full mask); rolled = 0x2000_0000 | 0x1FFFE000.
        assert_eq!(
            serial_rolled_version(&entry, 0xFFFF, false, 0),
            Some(0x2000_0000 | 0x1FFF_E000)
        );
    }

    #[test]
    fn serial_rolled_version_accepts_only_negotiated_mask_bits() {
        let entry = sample_entry(0x2000_0000, 0x0000_6000);

        assert_eq!(
            serial_rolled_version(&entry, 1, false, 0),
            Some(0x2000_2000)
        );
        assert_eq!(serial_rolled_version(&entry, 4, false, 0), None);
    }

    #[test]
    fn bm1398_rejects_out_of_range_midstate_even_without_rolling() {
        let entry = sample_entry(0x2000_0000, 0);

        assert_eq!(serial_rolled_version(&entry, 0, true, 3), Some(0x2000_0000));
        assert_eq!(serial_rolled_version(&entry, 0, true, 4), None);
    }

    #[test]
    fn serial_share_fixture_keeps_target_and_achieved_difficulty_separate() {
        let entry = sample_entry(0x2000_0000, 0);
        let nonce = 0x2a00_0000;
        let header = serial_build_header(&entry, entry.version, nonce);

        assert!(dcentrald_stratum::share_pipeline::validate_full_header(
            &header,
            &entry.share_target
        ));

        let achieved = serial_achieved_difficulty_from_header(&header)
            .expect("fixture header should produce a finite achieved difficulty");
        let pool_target_difficulty = 8_192.0;
        let share = dcentrald_stratum::types::ValidShare {
            worker_name: "worker.1".to_string(),
            job_id: entry.job_id.clone(),
            extranonce2: entry.extranonce2.clone(),
            ntime: format!("{:08x}", entry.ntime),
            nonce: format!("{:08x}", nonce),
            version_bits: None,
            version: entry.version,
            achieved_difficulty: Some(achieved),
        };

        assert_eq!(share.achieved_difficulty, Some(achieved));
        assert_ne!(share.achieved_difficulty, Some(pool_target_difficulty));
    }

    #[test]
    fn bm1398_fixture_validates_full_header_with_rolled_midstate() {
        let entry = sample_entry(0x2000_0000, 0x0000_6000);
        let rolled_version =
            serial_rolled_version(&entry, 0, true, 3).expect("BM1398 midstate 3 is valid");
        let header = serial_build_header(&entry, rolled_version, 0x1b2c_3d4e);

        assert_eq!(rolled_version, 0x2000_6000);
        assert!(dcentrald_stratum::share_pipeline::validate_full_header(
            &header,
            &entry.share_target
        ));
        assert!(serial_achieved_difficulty_from_header(&header).is_some());
    }

    #[test]
    fn bm1398_work_id_wraps_on_seven_bit_job_ring() {
        assert_eq!(serial_next_asic_job_id(0, BM1398_JOB_ID_INC), 4);
        assert_eq!(serial_next_asic_job_id(124, BM1398_JOB_ID_INC), 0);
        assert_eq!(serial_next_asic_job_id(120, BM1398_JOB_ID_INC), 124);
    }

    #[test]
    fn bm1362_pic_addr_resolution_allows_passthrough_tty_stm_lab() {
        assert_eq!(
            SerialMiner::bm1362_pic_addr_for_serial_runtime("/dev/ttySTM0", true, false, true)
                .expect("passthrough should not require AM2 PIC mapping"),
            None
        );
    }

    #[test]
    fn bm1362_pic_addr_resolution_still_rejects_unknown_native_port() {
        assert!(
            SerialMiner::bm1362_pic_addr_for_serial_runtime("/dev/ttySTM0", true, false, false)
                .is_err(),
            "native BM1362 cold boot must not infer PIC address for non-AM2 tty"
        );
        assert_eq!(
            SerialMiner::bm1362_pic_addr_for_serial_runtime("/dev/ttyS2", true, false, false)
                .expect("AM2 ttyS2 maps to dsPIC 0x21"),
            Some(0x21)
        );
    }

    #[test]
    fn am3_bb_uart_trans_chain_parser_accepts_single_ttyo_path() {
        assert_eq!(
            SerialMiner::am3_bb_uart_trans_chains_from_serial_device("/dev/ttyO4"),
            Some(vec![2])
        );
        assert_eq!(SerialMiner::am3_bb_uart_trans_chain_bits(&[2]), 0b0100);
    }

    #[test]
    fn am3_bb_uart_trans_chain_parser_accepts_deduped_ttyo_list() {
        assert_eq!(
            SerialMiner::am3_bb_uart_trans_chains_from_serial_device(
                "/dev/ttyO1, /dev/ttyO2,/dev/ttyO4,/dev/ttyO2"
            ),
            Some(vec![0, 1, 2])
        );
        assert_eq!(
            SerialMiner::am3_bb_uart_trans_chain_bits(&[0, 1, 2]),
            0b0111
        );
    }

    #[test]
    fn am3_bb_uart_trans_chain_parser_rejects_unknown_or_empty_paths() {
        assert_eq!(
            SerialMiner::am3_bb_uart_trans_chains_from_serial_device("/dev/ttyS2"),
            None
        );
        assert_eq!(
            SerialMiner::am3_bb_uart_trans_chains_from_serial_device("/dev/ttyO1,"),
            None
        );
    }

    // -----------------------------------------------------------------------
    // F-E3 — BM1370 vs BM1368 driver-dispatch safety
    //
    // Discriminator: stock firmware reads CHIP_ID from register 0x00 —
    // BM1370 returns 0x13700000, BM1368 returns 0x13680000 (ESP-Miner
    // bm1370.c / bm1368.c). When the model string is pinned that ground
    // truth is honoured; the fail-safe closes the count-only inference path
    // so a BM1370 SKU can never silently land on the BM1368 path or the
    // BM1362 PIC-family catch-all.
    // -----------------------------------------------------------------------

    #[test]
    fn s21pro_family_models_resolve_to_bm1370_not_bm1368() {
        // Every BM1370 SKU model string must resolve to 0x1370 (NoPic), never
        // the BM1368 (0x1368) S21 path. nopic=true (S21 family is always NoPic).
        for model in ["s21pro", "s21xp", "s21+", "s21plus"] {
            let chip_id = resolve_serial_chip_id(Some(model), 65, true)
                .unwrap_or_else(|e| panic!("model {model} must resolve, got: {e}"));
            assert_eq!(
                chip_id, 0x1370,
                "{model} must dispatch to BM1370, not BM1368/BM1362"
            );
            assert!(serial_chip_id_is_nopic_family(chip_id));
        }
        // The proven S21/T21 path stays BM1368.
        for model in ["s21", "t21"] {
            assert_eq!(
                resolve_serial_chip_id(Some(model), 108, true).unwrap(),
                0x1368
            );
        }
    }

    #[test]
    fn pinned_bm1370_model_wins_over_misleading_chip_count() {
        // A BM1370 chassis that mis-enumerates to 108 chips (the S21/BM1368
        // count) must STILL resolve to BM1370 when the model is pinned — the
        // operator-pinned register truth beats the count heuristic.
        assert_eq!(
            resolve_serial_chip_id(Some("s21pro"), 108, true).unwrap(),
            0x1370
        );
        // ...and a count of 65 with no model still infers BM1370 (anchor).
        assert_eq!(resolve_serial_chip_id(None, 65, true).unwrap(), 0x1370);
    }

    #[test]
    fn ambiguous_nopic_count_refuses_instead_of_guessing_a_pic_driver() {
        // The core F-E3 hazard: a NoPic S21-class chain with no model string
        // enumerating a non-anchor count would, under the old logic, fall
        // through to the BM1362 (PIC/dsPIC) catch-all — a wrong-family driver
        // on a NoPic chain. The fail-safe must REFUSE, not guess.
        for count in [60u8, 120, 130, 195, 0] {
            let result = resolve_serial_chip_id(None, count, true);
            assert!(
                result.is_err(),
                "NoPic chain at chip_count={count} with no model must refuse, \
                 got Ok({:?})",
                result.ok()
            );
        }
    }

    #[test]
    fn pic_family_default_path_is_unchanged_for_non_nopic_units() {
        // BM1362 am2/XIL units (PIC family, nopic=false) keep the proven
        // catch-all default at non-anchor counts (e.g. 28/126) — no regression.
        assert_eq!(resolve_serial_chip_id(None, 126, false).unwrap(), 0x1362);
        assert_eq!(resolve_serial_chip_id(None, 28, false).unwrap(), 0x1362);
        // Proven count anchors for the other PIC families are preserved.
        assert_eq!(resolve_serial_chip_id(None, 114, false).unwrap(), 0x1398);
        assert_eq!(resolve_serial_chip_id(None, 110, false).unwrap(), 0x1366);
        assert_eq!(resolve_serial_chip_id(None, 77, false).unwrap(), 0x1366);
    }

    #[test]
    fn native_serial_identity_never_comes_from_default_or_explicit_geometry() {
        assert!(resolve_native_serial_identity_and_geometry(None, None).is_err());
        assert!(resolve_native_serial_identity_and_geometry(None, Some(126)).is_err());
        assert!(
            resolve_native_serial_identity_and_geometry(Some("future-miner"), Some(126)).is_err()
        );
        assert!(
            resolve_native_serial_identity_and_geometry(Some("s9"), Some(63)).is_err(),
            "a recognized model outside the native serial dispatcher must fail closed"
        );
    }

    #[test]
    fn native_serial_geometry_requires_catalog_evidence_or_explicit_override() {
        assert_eq!(
            resolve_native_serial_identity_and_geometry(Some("s19jpro"), None).unwrap(),
            (0x1362, 126)
        );
        assert!(resolve_native_serial_identity_and_geometry(Some("t19"), None).is_err());
        assert_eq!(
            resolve_native_serial_identity_and_geometry(Some("t19"), Some(76)).unwrap(),
            (0x1398, 76)
        );
        assert!(resolve_native_serial_identity_and_geometry(Some("t19"), Some(0)).is_err());
    }

    #[test]
    fn native_serial_voltage_identity_rejects_impossible_model_chip_pairs() {
        assert!(validate_serial_model_voltage_identity(
            "bad-pic-s21",
            0x1368,
            Some(model::ModelPicTypeHint::DsPic),
        )
        .is_err());
        assert!(validate_serial_model_voltage_identity(
            "bad-nopic-s19j",
            0x1362,
            Some(model::ModelPicTypeHint::NoPic),
        )
        .is_err());
        assert!(
            validate_serial_model_voltage_identity("missing-s21-declaration", 0x1370, None,)
                .is_err()
        );
        assert!(validate_serial_model_voltage_identity(
            "s19kpro",
            0x1366,
            Some(model::ModelPicTypeHint::NoPic),
        )
        .is_ok());
    }

    #[test]
    fn native_serial_difficulty_requires_a_registered_profile() {
        assert!(hardware_difficulty_for_serial_family(0xFFFF).is_err());
        let expected = MinerProfile::for_chip(0x1362)
            .expect("BM1362 profile")
            .hardware_difficulty as u64;
        assert_eq!(
            hardware_difficulty_for_serial_family(0x1362).unwrap(),
            expected
        );
    }

    #[test]
    fn only_exact_bhb56_family_identity_requires_endpoint_capability() {
        for subtype in ["AMLCtrl_BHB56902", "amlctrl_bhb56999"] {
            assert!(subtype_requires_bhb56_endpoint_capability(Some(subtype)));
        }
        for subtype in [
            None,
            Some(""),
            Some("AMLCtrl_BHB42XXX"),
            Some("AMLCtrl_BHB68xxx"),
            Some("FutureCtrl_BHB56"),
        ] {
            assert!(!subtype_requires_bhb56_endpoint_capability(subtype));
        }
    }

    #[test]
    fn nopic_watchdog_and_safeoff_order_is_fail_closed() {
        let source = include_str!("serial_mining.rs");
        let management_owner = source
            .find("let mut amlogic_power_thermal")
            .expect("retained Amlogic management owner declaration");
        let cooling_owner = source
            .find("let mut amlogic_fan")
            .expect("retained Amlogic cooling owner declaration");
        let watchdog_owner = source
            .find("let mut nopic_watchdog")
            .expect("retained NoPic watchdog owner declaration");
        let psu_guard = source
            .find("let mut nopic_psu_guard")
            .expect("NoPic PSU guard declaration");
        let service_spawn = source
            .find(".spawn_power_thermal_service()")
            .expect("retained bus-1 service spawn");
        let admission = source
            .find("AmlogicNoPicAdmission::detect(")
            .expect("typed Amlogic NoPic admission");
        let bm1366_refusal = source
            .find("native BM1366 NoPic mining is refused")
            .expect("BM1366 NoPic fail-closed boundary");
        let fan_open = source
            .find(".open_fan_controller()")
            .expect("pre-energize cooling admission");
        let fan_motion = source
            .find("let receipt = admit_fan_airflow_envelope(")
            .expect("pre-energize fan-motion evidence gate");
        let power_start = source
            .find("if !native_nopic_power_owner")
            .expect("NoPic power-ownership boundary");
        let power_end = source[power_start..]
            .find("let mut startup_board_temps")
            .map(|offset| power_start + offset)
            .expect("NoPic post-enable temperature boundary");
        let power = &source[power_start..power_end];
        let watchdog_arm = power
            .find("SafetyWatchdogOwner::start_before_energizing")
            .expect("pre-energize watchdog admission");
        let may_be_energized = power
            .find("nopic_psu_guard.prepare_enable")
            .expect("pre-mutation NoPic guard arm");
        let psu_enable = power
            .find("tokio::task::spawn_blocking(move || power_enable_owner.enable_psu())")
            .expect("retained-service GPIO437/APW enable call");
        let enabled_receipt = power
            .find("nopic_psu_guard.mark_enabled()")
            .expect("NoPic enable ownership receipt");
        let startup_coverage_gate = source[power_end..]
            .find("if startup_coverage.is_complete()")
            .map(|offset| power_end + offset)
            .expect("complete startup thermal-coverage gate");
        let first_asic_probe = source[power_end..]
            .find("Phase 1c: Probing chips")
            .map(|offset| power_end + offset)
            .expect("first NoPic ASIC probe");
        assert!(watchdog_arm < may_be_energized);
        assert!(may_be_energized < psu_enable);
        assert!(psu_enable < enabled_receipt);
        assert!(enabled_receipt + power_start < startup_coverage_gate);
        assert!(startup_coverage_gate < first_asic_probe);
        assert!(management_owner < cooling_owner);
        assert!(cooling_owner < watchdog_owner);
        assert!(watchdog_owner < psu_guard);
        assert!(service_spawn < fan_open);
        assert!(fan_open < fan_motion);
        assert!(fan_motion < power_start);
        assert!(bm1366_refusal < admission);

        assert!(source.contains("RuntimeThreadGuard::new(CancellationToken::new())"));
        assert!(source.contains("let reader_shutdown = runtime_threads.cancellation_token();"));
        assert!(source.contains("AmlogicNoPicAdmission::detect("));
        assert!(source.contains(".spawn_power_thermal_service()"));
        assert!(source.contains(".read_board_temperatures("));
        let raw_temperature_helper = ["amlogic::read_board_", "temps("].concat();
        let generic_platform_open = ["amlogic::AmlogicPlatform::", "new()"].concat();
        assert!(!source.contains(&raw_temperature_helper));
        assert!(!source.contains(&generic_platform_open));
        assert!(source.contains("fence.latch_terminal_safe_off();"));

        let runtime_fan_safety = source
            .find("let fan_safety_state = nopic_fan_safety.observe_required_airflow")
            .expect("runtime fan-safety observation");
        let runtime_safe_off = source[runtime_fan_safety..]
            .find("checked_nopic_emergency_safe_off(&mut nopic_psu_guard)")
            .map(|offset| runtime_fan_safety + offset)
            .expect("runtime fan-safety checked safe-off");
        let watchdog_progress = source[runtime_fan_safety..]
            .find("nopic_watchdog_liveness.mark_progress()")
            .map(|offset| runtime_fan_safety + offset)
            .expect("NoPic watchdog liveness marker");
        assert!(runtime_fan_safety < runtime_safe_off);
        assert!(runtime_safe_off < watchdog_progress);

        let shutdown = source
            .split("info!(\"=== SHUTDOWN ===\");")
            .nth(1)
            .expect("serial shutdown section");
        let teardown = shutdown
            .find(".begin_teardown(DEFAULT_WATCHDOG_TEARDOWN_GRACE)")
            .expect("watchdog Teardown admission");
        let mutation_barrier = shutdown
            .find(".close_and_drain(RUNTIME_THREAD_STOP_TIMEOUT)")
            .expect("control-plane hardware mutation barrier");
        let actor_join = shutdown
            .find(".stop_and_join(RUNTIME_THREAD_STOP_TIMEOUT)")
            .expect("serial actor join");
        let power_off = shutdown
            .find("nopic_psu_guard.safe_off()")
            .expect("checked NoPic safe-off");
        let quiet_fan = shutdown
            .find("fan.set_speed_checked")
            .expect("checked quiet fan command");
        let permit = shutdown
            .find("WatchdogDisarmPermit::from_evidence_set")
            .expect("typed watchdog disarm permit");
        assert!(shutdown.contains("power_receipt.management_fabric()"));
        let disarm = shutdown
            .find(".disarm_and_join")
            .expect("watchdog close and join");
        let persistence = shutdown
            .find("history_buffer.save")
            .expect("noncritical history persistence");
        assert!(teardown < mutation_barrier);
        assert!(mutation_barrier < actor_join);
        assert!(actor_join < power_off);
        assert!(power_off < quiet_fan);
        assert!(quiet_fan < permit);
        assert!(permit < disarm);
        assert!(disarm < persistence);
    }

    #[test]
    fn bhb56_endpoint_issuance_precedes_service_and_init_has_no_raw_fallback() {
        let source = include_str!("serial_mining.rs");
        let issue = source
            .find("let pending_bhb56_endpoints")
            .expect("BHB56 endpoint issuance");
        let spawn = source
            .find("let bm1362_i2c_service")
            .expect("serialized I2C service spawn");
        let bind = source
            .find("let bhb56_dspic_sessions")
            .expect("endpoint-to-service session binding");
        assert!(issue < spawn && spawn < bind);
        assert!(source.contains("refusing model/config-only dsPIC authorization"));

        let branch_start = source
            .find("// ---- BM1366 (S19 XP / S19K Pro): dsPIC voltage init + ASIC init ----")
            .expect("BM1366 voltage-init branch");
        let branch_end = source[branch_start..]
            .find("} else if is_bm1368 || is_bm1370 {")
            .map(|offset| branch_start + offset)
            .expect("end of BM1366 voltage-init branch");
        let branch = &source[branch_start..branch_end];
        assert!(branch.contains("Self::dspic_service_for_serial_route("));
        assert!(!branch.contains("DspicService::new("));
        assert!(!branch.contains("DspicService::new_with_firmware("));

        let helper_start = source
            .find("fn dspic_service_for_serial_route(")
            .expect("capability-aware service helper");
        let helper_end = source[helper_start..]
            .find("fn am3_bb_uart_trans_chain_from_serial_device(")
            .map(|offset| helper_start + offset)
            .expect("end of capability-aware service helper");
        let helper = &source[helper_start..helper_end];
        assert!(helper.contains("if endpoint_capability_required"));
        assert!(helper.contains("refusing caller-asserted protocol/address fallback"));
    }

    #[test]
    fn am3_bb_bm1362_init_reuses_existing_observations_for_endpoint_authority() {
        let source = include_str!("serial_mining.rs");
        assert!(source
            .contains("let mut retained_eeprom_bytes: [Option<Vec<u8>>; 3] = [None, None, None];"));
        assert!(source.contains("retained_eeprom_bytes[slot as usize] = Some(bytes.clone());"));
        assert!(source.contains("let (detected_fw, detected_fw_reply)"));
        let get_version_call = [
            "Self::pic_read_fw_version_service",
            "(i2c_service, pic_addr)",
        ]
        .concat();
        assert_eq!(
            source.matches(&get_version_call).count(),
            1,
            "endpoint binding must reuse the one existing GET_VERSION observation"
        );
        assert!(source.contains("try_bind_system_am3_bb_dspic_endpoint("));
        assert!(source.contains("Pic0x89EndpointSession::new(i2c_service.clone(), endpoint)"));

        let branch_start = source
            .find("let (detected_fw, detected_fw_reply)")
            .expect("BM1362 existing firmware observation");
        let branch_end = source[branch_start..]
            .find("post_enable_chain_uart_probe(&serial_device, pic_addr);")
            .map(|offset| branch_start + offset)
            .expect("BM1362 post-enable proof boundary");
        let branch = &source[branch_start..branch_end];
        assert!(branch.contains("try_bind_system_am3_bb_dspic_endpoint("));
        assert!(branch.contains("Pic0x89EndpointSession::new"));
    }

    #[test]
    fn exact_am2_bm1362_init_reuses_the_same_eeprom_and_version_observations() {
        let source = include_str!("serial_mining.rs");
        let plan = source
            .find("let pending_am2_bm1362_plan")
            .expect("AM2 direct-serial plan discovery");
        let service = source
            .find("let bm1362_i2c_service")
            .expect("serialized I2C service spawn");
        assert!(
            plan < service,
            "identity planning must emit no service traffic"
        );

        let get_version_call = [
            "Self::pic_read_fw_version_service",
            "(i2c_service, pic_addr)",
        ]
        .concat();
        assert_eq!(
            source.matches(&get_version_call).count(),
            1,
            "AM2 endpoint binding must not add a second GET_VERSION transaction"
        );

        let branch_start = source
            .find("let (detected_fw, detected_fw_reply)")
            .expect("existing BM1362 firmware observation");
        let branch_end = source[branch_start..]
            .find("post_enable_chain_uart_probe(&serial_device, pic_addr);")
            .map(|offset| branch_start + offset)
            .expect("BM1362 post-enable proof boundary");
        let branch = &source[branch_start..branch_end];
        assert!(branch.contains("pending_am2_bm1362_plan.as_ref()"));
        assert!(branch.contains("retained_eeprom_bytes"));
        assert!(branch.contains("bind_am2_hashboard_presence("));
        assert!(branch.contains("bind_am2_controller_endpoint_from_observation("));
        assert!(branch.contains("&detected_fw_reply"));
        assert!(branch.contains("Pic0x89EndpointSession::new"));
        assert!(branch.contains("refusing raw-address fallback"));

        let exact_start = branch
            .find("if let Some(plan) = pending_am2_bm1362_plan.as_ref()")
            .expect("exact AM2 capability branch");
        let legacy_start = branch[exact_start..]
            .find("Pic0x89Service::new_with_fw(")
            .map(|offset| exact_start + offset)
            .expect("non-target compatibility seam");
        assert!(branch[exact_start..legacy_start].contains("Pic0x89EndpointSession::new"));
        assert!(!branch[exact_start..legacy_start].contains("Pic0x89Service::new_with_fw"));
    }

    #[test]
    fn nopic_family_classifier_matches_profile_table() {
        // BM1368 / BM1370 / BM1373 are the NoPic families; the PIC families
        // are not. Mirrors dcentrald-asic PicType + model.rs pic_type_hint.
        assert!(serial_chip_id_is_nopic_family(0x1368));
        assert!(serial_chip_id_is_nopic_family(0x1370));
        assert!(serial_chip_id_is_nopic_family(0x1373));
        assert!(!serial_chip_id_is_nopic_family(0x1362));
        assert!(!serial_chip_id_is_nopic_family(0x1366));
        assert!(!serial_chip_id_is_nopic_family(0x1398));
        assert!(!serial_chip_id_is_nopic_family(0x1387));
    }

    #[test]
    fn bm1370_and_bm1368_chip_ids_are_distinct_in_discriminator() {
        // Regression pin: the two NoPic S21-class dies must never collapse to
        // the same chip-id in the serial discriminator (the register-0x00
        // CHIP_ID truth 0x13700000 vs 0x13680000).
        let bm1370 = resolve_serial_chip_id(Some("s21pro"), 65, true).unwrap();
        let bm1368 = resolve_serial_chip_id(Some("s21"), 108, true).unwrap();
        assert_ne!(bm1370, bm1368);
        assert_eq!(bm1370, 0x1370);
        assert_eq!(bm1368, 0x1368);
    }

    /// VNish-RE'd 7-byte ENABLE/DISABLE_VOLTAGE form for fw=0x86/0x89 PICs.
    /// Source: .
    /// Mirrors the byte-exact assertion in `dcentrald-asic/src/dspic.rs`.
    #[test]
    fn pic_enable_cmd_vnish_byte_exact() {
        // ENABLE: [55 AA 05 15 01 00 1B] — SUM = (0x05+0x15+0x01+0x00) = 0x1B
        assert_eq!(
            pic_enable_cmd_vnish(0x01),
            [0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B]
        );
        // DISABLE: [55 AA 05 15 00 00 1A] — SUM = (0x05+0x15+0x00+0x00) = 0x1A
        assert_eq!(
            pic_enable_cmd_vnish(0x00),
            [0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
        );
    }
}
