//! Platform trait and auto-detection.
//!
//! dcentrald supports multiple control board types through a platform trait.
//! Each platform provides its own implementation of chain access, I2C, fan
//! control, and GPIO. The initial implementation targets Zynq only.
//!
//! Platform auto-detection at startup:
//!   1. Check for UIO devices (/dev/uio0) -> Zynq
//!   2. Check for /dev/ttyO1 -> BeagleBone
//!   3. Check for STM32MP15 / ttySTM* -> Braiins BCB100
//!   4. Check for uart_trans kernel module -> CVitek
//!   5. Check for /dev/ttyS1 + Amlogic DTS -> Amlogic

pub mod am2_controller;
pub mod amlogic;
pub mod beaglebone;
pub mod beaglebone_cold_boot;
pub mod config;
pub mod cvitek;
pub(crate) mod cvitek_cold_boot;
pub(crate) mod cvitek_pinmux;
#[cfg(feature = "sim-hal")]
pub mod sim;
pub mod stm32mp15;
pub mod subtype;
pub mod zynq;

use crate::i2c::I2cBus;
use crate::{HalError, Result};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub use am2_controller::{
    bind_am2_controller_endpoint_from_observation, bind_am2_hashboard_presence,
    discover_am2_controller_endpoint, discover_system_am2_controller_plan,
    observe_am2_hashboard_presence, try_discover_system_am2_controller_plan, Am2ControllerContext,
    Am2ControllerPlan, Am2HashboardPresence,
};
pub use config::VoltageControllerKind;
pub use subtype::{
    discover_system_pic16_endpoint, discover_system_voltage_controller_endpoint,
    VoltageControllerEndpoint, VoltageControllerEndpointError,
};

/// Control board type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardType {
    /// Zynq 7010 (S9, S17, S19) - FPGA UART FIFOs via UIO.
    Zynq,
    /// BeagleBone AM335x (S19j) - hardware UART /dev/ttyO1-5, no FPGA.
    BeagleBone,
    /// Amlogic A113D (S19XP, S21) - software UART /dev/ttyS1-3, no FPGA.
    Amlogic,
    /// CVITEK CV1835 (S21/T21 recent) - uart_trans kernel module.
    CVitek,
    /// STM32MP15 / Braiins BCB100 replacement board - direct UART, lab-gated.
    Stm32Mp15,
}

/// Abstract chain access interface.
///
/// For Zynq, this is implemented by FpgaChain (UIO mmap + IRQ).
/// For BeagleBone, it would be a UART serial device.
/// For Amlogic, it would be software UART or /dev/ttyS.
pub trait ChainAccess: Send + Sync {
    /// Send a command to the ASIC chain.
    fn send_command(&self, data: &[u8]) -> Result<()>;

    /// Read a response from the ASIC chain.
    fn read_response(&self, buf: &mut [u8]) -> Result<usize>;

    /// Send mining work data to the chain.
    fn send_work(&self, data: &[u8]) -> Result<()>;

    /// Read a nonce response from the chain.
    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize>;

    /// Set the UART baud rate for this chain.
    fn set_baud(&self, baud: u32) -> Result<()>;

    /// Blocking wait for nonce data (IRQ or poll-based).
    fn wait_for_nonce(&self) -> Result<()>;
}

/// Checked fan-command evidence. This proves only that the platform command
/// path completed and its available PWM readback matched; it does not prove
/// airflow or physical fan rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanCommandReceipt {
    pub(crate) requested_pwm: u8,
    pub(crate) observed_pwm: u8,
}

impl FanCommandReceipt {
    /// Build command evidence after a platform implementation has read its
    /// applied PWM back. External platform crates can implement [`FanAccess`]
    /// without gaining a constructor that accepts a mismatch.
    pub fn from_matching_readback(requested_pwm: u8, observed_pwm: u8) -> Result<Self> {
        if requested_pwm != observed_pwm {
            return Err(HalError::Fan(format!(
                "fan PWM readback mismatch: requested {requested_pwm}, observed {observed_pwm}"
            )));
        }
        Ok(Self {
            requested_pwm,
            observed_pwm,
        })
    }

    pub fn requested_pwm(&self) -> u8 {
        self.requested_pwm
    }

    pub fn observed_pwm(&self) -> u8 {
        self.observed_pwm
    }
}

/// Abstract fan access interface.
pub trait FanAccess: Send + Sync {
    /// Set fan speed (PWM value, platform-specific range).
    fn set_speed(&self, pwm: u8);

    /// Set fan speed through a platform-specific fallible command path and
    /// require its available readback. The default is deliberately unavailable:
    /// calling an infallible compatibility setter and then observing an old,
    /// already-equal value cannot prove that the new write completed.
    fn set_speed_checked(&self, _pwm: u8) -> Result<FanCommandReceipt> {
        Err(HalError::Fan(
            "checked fan commands are not implemented for this platform".to_string(),
        ))
    }

    /// Get current fan RPM.
    fn get_rpm(&self) -> u32;

    /// Get current PWM value.
    fn get_speed_pwm(&self) -> u8;

    /// Get per-fan RPM readings. Returns (fan_id, rpm) pairs.
    /// Default: single-fan fallback from get_rpm().
    fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        vec![(0, self.get_rpm())]
    }

    /// Number of physical fan channels.
    fn fan_count(&self) -> u8 {
        1
    }

    /// Whether hardware fan tachometer readings are available.
    /// When false, RPM accessors provide no safety evidence: they may return
    /// zero for compatibility, but the thermal controller must not interpret
    /// that zero as a measured stopped fan. It must rely on temperature
    /// thresholds until fresh tach evidence becomes available.
    fn tach_available(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HardwareMutationGatePhase {
    Pending,
    Open,
    Closed,
}

#[derive(Debug)]
struct HardwareMutationGateState {
    phase: HardwareMutationGatePhase,
    in_flight: usize,
}

#[derive(Debug)]
struct HardwareMutationGateInner {
    state: Mutex<HardwareMutationGateState>,
    drained: Condvar,
}

/// Shared admission barrier for hardware mutations that can race teardown.
///
/// Every external control-plane mutator holds a lease for the full blocking
/// hardware call. Teardown closes admission and waits for all leases to drain
/// before it observes software safe-off. This makes a checked LOW readback a
/// stable ordering fact instead of a value that an in-flight API handler can
/// immediately invalidate.
#[derive(Clone, Debug)]
pub struct HardwareMutationGate {
    inner: Arc<HardwareMutationGateInner>,
}

impl HardwareMutationGate {
    pub fn new_open() -> Self {
        Self::new(HardwareMutationGatePhase::Open)
    }

    /// Construct a terminally closed gate for management surfaces that must
    /// never admit hardware mutations in this process lifetime.
    ///
    /// Closed admission is irreversible. Because no lease can have been
    /// admitted before construction, [`Self::close_and_drain`] succeeds
    /// immediately and returns the same opaque barrier evidence as a drained
    /// open gate.
    pub fn new_closed() -> Self {
        Self::new(HardwareMutationGatePhase::Closed)
    }

    fn new(phase: HardwareMutationGatePhase) -> Self {
        Self {
            inner: Arc::new(HardwareMutationGateInner {
                state: Mutex::new(HardwareMutationGateState {
                    phase,
                    in_flight: 0,
                }),
                drained: Condvar::new(),
            }),
        }
    }

    pub fn try_acquire(&self) -> Result<HardwareMutationLease> {
        let mut state =
            self.inner.state.lock().map_err(|_| {
                HalError::Platform("hardware mutation gate mutex poisoned".to_string())
            })?;
        match state.phase {
            HardwareMutationGatePhase::Open => {}
            HardwareMutationGatePhase::Pending => {
                return Err(HalError::Platform(
                    "hardware mutation admission is pending mining readiness".to_string(),
                ));
            }
            HardwareMutationGatePhase::Closed => {
                return Err(HalError::Platform(
                    "hardware mutation admission is closed for teardown".to_string(),
                ));
            }
        }
        state.in_flight = state.in_flight.saturating_add(1);
        drop(state);
        Ok(HardwareMutationLease {
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn close_and_drain(&self, timeout: Duration) -> Result<HardwareMutationBarrierReceipt> {
        let started_at = Instant::now();
        let mut state =
            self.inner.state.lock().map_err(|_| {
                HalError::Platform("hardware mutation gate mutex poisoned".to_string())
            })?;
        state.phase = HardwareMutationGatePhase::Closed;

        while state.in_flight != 0 {
            let remaining = timeout.saturating_sub(started_at.elapsed());
            if remaining.is_zero() {
                return Err(HalError::Platform(format!(
                    "timed out draining {} in-flight hardware mutation(s)",
                    state.in_flight
                )));
            }
            let (next_state, wait) =
                self.inner
                    .drained
                    .wait_timeout(state, remaining)
                    .map_err(|_| {
                        HalError::Platform("hardware mutation gate mutex poisoned".to_string())
                    })?;
            state = next_state;
            if wait.timed_out() && state.in_flight != 0 {
                return Err(HalError::Platform(format!(
                    "timed out draining {} in-flight hardware mutation(s)",
                    state.in_flight
                )));
            }
        }

        Ok(HardwareMutationBarrierReceipt {
            closed_and_drained_at: Instant::now(),
        })
    }
}

/// Sole capability that can transition a shared hardware-mutation gate from
/// pending to open. API state receives only [`HardwareMutationGate`], so it can
/// acquire leases after admission or close admission, but cannot authorize its
/// own hardware access during engine bring-up.
#[derive(Debug)]
pub struct HardwareMutationGateOwner {
    gate: HardwareMutationGate,
}

impl HardwareMutationGateOwner {
    pub fn new_pending() -> Self {
        let gate = HardwareMutationGate::new(HardwareMutationGatePhase::Pending);
        Self { gate }
    }

    pub fn gate(&self) -> HardwareMutationGate {
        self.gate.clone()
    }

    pub fn open(&self) -> Result<HardwareMutationAdmissionReceipt> {
        let mut state =
            self.gate.inner.state.lock().map_err(|_| {
                HalError::Platform("hardware mutation gate mutex poisoned".to_string())
            })?;
        match state.phase {
            HardwareMutationGatePhase::Pending => {
                state.phase = HardwareMutationGatePhase::Open;
                Ok(HardwareMutationAdmissionReceipt {
                    opened_at: Instant::now(),
                })
            }
            HardwareMutationGatePhase::Open => Err(HalError::Platform(
                "hardware mutation admission was already opened".to_string(),
            )),
            HardwareMutationGatePhase::Closed => Err(HalError::Platform(
                "hardware mutation admission is terminally closed".to_string(),
            )),
        }
    }

    pub fn close_and_drain(&self, timeout: Duration) -> Result<HardwareMutationBarrierReceipt> {
        self.gate.close_and_drain(timeout)
    }
}

impl Drop for HardwareMutationGateOwner {
    fn drop(&mut self) {
        // Drop is fail-closed and non-blocking. A failed zero-time drain still
        // leaves admission terminally closed; it simply cannot mint evidence.
        let _ = self.gate.close_and_drain(Duration::ZERO);
    }
}

/// Opaque evidence that the hardware owner transitioned pending admission to
/// open exactly once.
#[derive(Debug)]
pub struct HardwareMutationAdmissionReceipt {
    opened_at: Instant,
}

impl HardwareMutationAdmissionReceipt {
    pub fn opened_at(&self) -> Instant {
        self.opened_at
    }
}

/// RAII proof that one admitted hardware mutation is still in flight.
#[derive(Debug)]
pub struct HardwareMutationLease {
    inner: Arc<HardwareMutationGateInner>,
}

impl Drop for HardwareMutationLease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            self.inner.drained.notify_all();
        }
    }
}

/// Opaque evidence that mutation admission is closed and every admitted
/// control-plane mutation has completed.
#[derive(Debug)]
pub struct HardwareMutationBarrierReceipt {
    closed_and_drained_at: Instant,
}

impl HardwareMutationBarrierReceipt {
    pub fn closed_and_drained_at(&self) -> Instant {
        self.closed_and_drained_at
    }
}

#[cfg(test)]
mod hardware_mutation_gate_tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    struct InfallibleCompatibilityFan;

    impl FanAccess for InfallibleCompatibilityFan {
        fn set_speed(&self, _pwm: u8) {}

        fn get_rpm(&self) -> u32 {
            0
        }

        fn get_speed_pwm(&self) -> u8 {
            30
        }
    }

    #[test]
    fn default_checked_fan_path_never_mints_receipt_from_stale_equal_readback() {
        let fan = InfallibleCompatibilityFan;
        assert!(fan.set_speed_checked(30).is_err());
    }

    #[test]
    fn close_rejects_new_mutations_and_waits_for_admitted_lease() {
        let gate = HardwareMutationGate::new_open();
        let lease = gate.try_acquire().unwrap();
        let closer = gate.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            started_tx.send(()).unwrap();
            closer.close_and_drain(Duration::from_secs(1))
        });
        started_rx.recv().unwrap();

        while gate.try_acquire().is_ok() {
            thread::yield_now();
        }
        assert!(!handle.is_finished());
        drop(lease);
        assert!(handle.join().unwrap().is_ok());
        assert!(gate.try_acquire().is_err());
    }

    #[test]
    fn drain_timeout_keeps_admission_closed() {
        let gate = HardwareMutationGate::new_open();
        let _lease = gate.try_acquire().unwrap();
        assert!(gate.close_and_drain(Duration::from_millis(1)).is_err());
        assert!(gate.try_acquire().is_err());
    }

    #[test]
    fn initially_closed_gate_rejects_leases_and_is_already_drained() {
        let gate = HardwareMutationGate::new_closed();

        assert!(gate.try_acquire().is_err());
        let receipt = gate.close_and_drain(Duration::ZERO).unwrap();
        assert!(receipt.closed_and_drained_at() <= Instant::now());
        assert!(gate.try_acquire().is_err());
    }

    #[test]
    fn pending_gate_can_only_be_opened_once_by_its_owner() {
        let owner = HardwareMutationGateOwner::new_pending();
        let gate = owner.gate();
        assert!(gate
            .try_acquire()
            .unwrap_err()
            .to_string()
            .contains("pending mining readiness"));

        let receipt = owner.open().unwrap();
        assert!(receipt.opened_at() <= Instant::now());
        let lease = gate.try_acquire().unwrap();
        assert!(owner.open().is_err());
        drop(lease);

        owner.close_and_drain(Duration::ZERO).unwrap();
        assert!(gate.try_acquire().is_err());
        assert!(owner.open().is_err());
    }

    #[test]
    fn dropping_pending_gate_owner_terminally_closes_api_admission() {
        let gate = {
            let owner = HardwareMutationGateOwner::new_pending();
            let gate = owner.gate();
            owner.open().unwrap();
            assert!(gate.try_acquire().is_ok());
            gate
        };

        assert!(gate.try_acquire().is_err());
    }
}

/// Abstract GPIO access interface.
pub trait GpioAccess: Send + Sync {
    /// Read hash board plug detect state.
    fn read_plug_detect(&self) -> [bool; 3];

    /// Assert or release hash board reset.
    fn set_board_reset(&self, chain: u8, assert_reset: bool);
}

/// Platform trait for multi-board support.
///
/// Each supported control board type implements this trait to provide
/// platform-specific hardware access.
pub trait Platform: Send + Sync {
    /// Get the board type.
    fn board_type(&self) -> BoardType;

    /// Get the number of hash board chains this platform supports.
    fn chain_count(&self) -> u8;

    /// Open a chain access interface for the given chain ID.
    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>>;

    /// Open an I2C bus.
    fn open_i2c(&self, bus: u8) -> Result<I2cBus>;

    /// Open the fan controller.
    fn open_fan(&self) -> Result<Box<dyn FanAccess>>;

    /// Open the GPIO controller.
    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>>;

    /// Informational voltage-controller kind in use on this platform.
    ///
    /// The default is deliberately `NoPic`: implementations without exact
    /// hashboard identity must not silently select dsPIC wire bytes. This enum
    /// is compatibility/telemetry data, not service-construction authority.
    ///
    /// W2A.2 (2026-05-09): introduced as part of the PIC1704 wire-up.
    fn voltage_controller(&self) -> VoltageControllerKind {
        VoltageControllerKind::NoPic
    }
}

impl BoardType {
    /// Fail-closed hint for callers without a live `Platform` instance.
    /// Control-board family alone does not identify the attached hashboard or
    /// its controller protocol, so every unproven static default is `NoPic`.
    pub fn voltage_controller_default(&self) -> VoltageControllerKind {
        match self {
            // Even a CV1835 carrier can host different hashboard families.
            BoardType::CVitek => VoltageControllerKind::NoPic,
            // No carrier family alone grants a controller protocol.
            BoardType::Zynq => VoltageControllerKind::NoPic,
            BoardType::BeagleBone => VoltageControllerKind::NoPic,
            BoardType::Amlogic => VoltageControllerKind::NoPic,
            BoardType::Stm32Mp15 => VoltageControllerKind::NoPic,
        }
    }
}

/// Auto-detect the current platform.
///
/// Checks hardware signatures to determine which control board we're running on.
/// For Zynq boards, further distinguishes S9 (am1-s9) vs S19 (am2-s17) via UIO
/// device naming patterns — see `zynq::detect_zynq_variant()`.
///
/// Detection order matters when multiple signatures coexist (e.g. stock Bitmain
/// BB has both `/dev/ttyO1` AND `/sys/module/uart_trans` loaded — BB must win
/// because uart_trans is just a wrapper layered on top of the same omap-serial
/// ttyOX devices). The AM33XX CPU string is the BB tiebreaker over CVitek
/// (different SoC entirely).
pub fn detect_platform() -> Result<Box<dyn Platform>> {
    // Simulation is considered before hardware auto-detection only when at
    // least one simulation variable is explicitly present. A partial or
    // malformed request fails closed instead of falling through to a real
    // platform. `SimPlatform::from_env` also refuses every known miner
    // hardware signature, even in a binary accidentally built with sim-hal.
    #[cfg(feature = "sim-hal")]
    if sim::sim_environment_is_mentioned() {
        return Ok(Box::new(sim::SimPlatform::from_env()?));
    }

    // 1. Zynq — UIO devices (covers both S9 and S19/am2-s17)
    if std::path::Path::new("/dev/uio0").exists() {
        return Ok(Box::new(zynq::ZynqPlatform::new()?));
    }

    // 2. BeagleBone — TI AM335x SoC + a chain-0 UART node.
    //    Stock Bitmain BB also loads uart_trans.ko (which proxies the same
    //    ttyOX devices), so check BB BEFORE the uart_trans-based CVitek path.
    //    The `/proc/cpuinfo` "AM33XX" hardware string is the authoritative
    //    SoC tiebreaker — it is present ONLY on AM335x (a real Amlogic A113D
    //    is aarch64 and never reports AM33XX), so an AM335x match cannot
    //    false-positive onto the Amlogic branch below.
    //
    //    Chain-0 UART naming differs by kernel: stock Bitmain BB exposes
    //    `/dev/ttyO1` (legacy omap-serial naming), while LuxOS / DCENT_OS on
    //    the `a lab unit`-class S19J_IO_BOARD_V2_0 unit exposes `/dev/ttyS1`
    //    (mainline omap-serial). `BeagleBonePlatform::new()` already accepts
    //    EITHER node; this detection gate must accept both too, otherwise a
    //    `a lab unit`-class LuxOS/DCENT_OS BB (ttyS1, no ttyO1) skips the BB branch
    //    and falls through to the Amlogic `/dev/ttyS1` branch — constructing
    //    the wrong (aarch64 Amlogic) HAL on an armv7 AM335x board.
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let is_am335x =
        cpuinfo.contains("AM33XX") || cpuinfo.contains("AM335x") || cpuinfo.contains("am33xx");
    if is_am335x
        && (std::path::Path::new("/dev/ttyO1").exists()
            || std::path::Path::new("/dev/ttyS1").exists())
    {
        return Ok(Box::new(beaglebone::BeagleBonePlatform::new()?));
    }

    // 3. Braiins BCB100 / STM32MP15. The constructor is lab-gated until
    // the GPIO, fan, PSU, and PIC maps are live-verified.
    if stm32mp15::looks_like_bcb100_host() {
        return Ok(Box::new(stm32mp15::Bcb100Platform::new()?));
    }

    // 4. CVitek uart_trans kernel module (CV1835 SoC, NOT BeagleBone).
    //
    // The reverse-engineered HAL remains available to host tests, but it is
    // not a runtime admission surface. The constructor is itself a typed,
    // non-mutating refusal; detection repeats that refusal before construction.
    if std::path::Path::new("/sys/module/uart_trans").exists() {
        return Err(HalError::Platform(
            "CV1835 runtime NOT IMPLEMENTED: automatic CVitek HAL construction and pinmux mutation are disabled"
                .to_string(),
        ));
    }

    // 4. Amlogic UART (must come after CVitek — both may have /dev/ttyS).
    if ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3"]
        .iter()
        .any(|path| std::path::Path::new(path).exists())
    {
        return Ok(Box::new(amlogic::AmlogicPlatform::new()?));
    }

    Err(HalError::Platform(
        "unable to detect platform: no known hardware signatures found".to_string(),
    ))
}
