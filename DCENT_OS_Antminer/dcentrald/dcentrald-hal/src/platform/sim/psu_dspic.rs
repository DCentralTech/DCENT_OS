//! Simulated I2C devices used by Antminer board-management paths.
//!
//! Protocol bytes are sourced from the in-tree production implementations:
//! `dcentrald-asic::dspic` (0x17/0x10/0x15/0x16),
//! `dcentrald-asic::pic1704::protocol` (registers 0x00/0x08/0x09),
//! `dcentrald-asic::pic` (PIC16F1704 app-mode commands), and
//! `dcentrald-hal::psu` (APW framed protocol). The simulator intentionally
//! models acknowledgements and state transitions, not electrical timing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::i2c::I2cSimBackend as I2cSimBackendTrait;
use crate::{HalError, Result};

use super::{SimBoardProfile, SimModel, SimPic16Operation, TraceEvent};

const DSPIC_GET_VERSION: u8 = 0x17;
const DSPIC_SET_VOLTAGE: u8 = 0x10;
const DSPIC_ENABLE_VOLTAGE: u8 = 0x15;
const DSPIC_HEARTBEAT: u8 = 0x16;
const DSPIC_FW_APPLICATION: u8 = 0x89;

const PIC16_GET_VERSION_STOCK: u8 = 0x04;
const PIC16_GET_VERSION_BRAIINS: u8 = 0x17;
const PIC16_SET_VOLTAGE: u8 = 0x10;
const PIC16_ENABLE_VOLTAGE: u8 = 0x15;
const PIC16_HEARTBEAT: u8 = 0x16;
const PIC16_GET_VOLTAGE: u8 = 0x18;
const PIC16_JUMP_FROM_LOADER: u8 = 0x06;
const PIC16_BOOTLOADER_MODE: u8 = 0xCC;
const PIC16_APP_MODE: u8 = 0x60;
const PIC16_RECENT_HEARTBEAT_CAPACITY: usize = 16;

const PIC1704_REG_VERSION: u8 = 0x00;
const PIC1704_REG_VOLTAGE_L: u8 = 0x02;
const PIC1704_REG_STATUS: u8 = 0x08;
const PIC1704_REG_CONTROL: u8 = 0x09;
const PIC1704_CTRL_OFF: u8 = 0x00;
const PIC1704_CTRL_ON: u8 = 0x01;
const PIC1704_CTRL_HEARTBEAT: u8 = 0x02;
const PIC1704_FW_APPLICATION: u8 = 0x89;

const APW_ADDR: u8 = 0x10;
const APW_GET_VERSION: u8 = 0x01;
const APW_MEASURE_VOLTAGE: u8 = 0x04;
const APW_READ_STATE: u8 = 0x05;
const APW_WATCHDOG: u8 = 0x81;
const APW_SET_VOLTAGE: u8 = 0x83;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimControllerKind {
    Pic16,
    Dspic,
    Pic1704,
    NoPic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimPic16Fault {
    TransportError,
    ShortRead,
}

#[derive(Debug, Clone, Copy)]
struct ScheduledPic16Fault {
    successful_matches_before_fault: u64,
    effect: SimPic16Fault,
}

impl SimControllerKind {
    pub const fn for_model(model: SimModel) -> Self {
        use SimModel::*;
        match model {
            S9 | S11 | S15 | T15 | T17 | S17Plus | T17Plus => Self::Pic16,
            S17 | S17Pro | S17e | S19 | S19Pro | S19jPro => Self::Dspic,
            S19Xp | S19kPro | S21 | S21Pro | S21Xp | S23 => Self::NoPic,
        }
    }
}

#[derive(Debug, Default)]
struct DeviceState {
    accumulator: Vec<u8>,
    pending: VecDeque<u8>,
}

#[derive(Debug, Clone)]
struct SimPic16DeviceState {
    raw_state: u8,
    voltage_pic: Option<u8>,
    voltage_enabled: bool,
    heartbeat_count: u64,
    heartbeat_times: VecDeque<Duration>,
    watchdog: Option<SimControllerWatchdog>,
    generation: u64,
    chain_live: bool,
    live_chain_lease: Arc<AtomicBool>,
    faults: HashMap<SimPic16Operation, VecDeque<ScheduledPic16Fault>>,
}

impl SimPic16DeviceState {
    fn application_default() -> Self {
        Self {
            raw_state: PIC16_APP_MODE,
            voltage_pic: None,
            voltage_enabled: false,
            heartbeat_count: 0,
            heartbeat_times: VecDeque::with_capacity(PIC16_RECENT_HEARTBEAT_CAPACITY),
            watchdog: None,
            generation: 0,
            chain_live: false,
            live_chain_lease: Arc::new(AtomicBool::new(false)),
            faults: HashMap::new(),
        }
    }

    fn invalidate_live_chain_lease(&mut self) {
        self.chain_live = false;
        self.live_chain_lease.store(false, Ordering::SeqCst);
    }

    fn establish_live_chain_lease(&mut self) -> Weak<AtomicBool> {
        self.live_chain_lease.store(false, Ordering::SeqCst);
        self.live_chain_lease = Arc::new(AtomicBool::new(true));
        self.chain_live = true;
        Arc::downgrade(&self.live_chain_lease)
    }
}

impl Default for SimPic16DeviceState {
    fn default() -> Self {
        Self::application_default()
    }
}

/// Endpoint-scoped PIC16 state exposed by the host-only simulator.
///
/// Aggregate compatibility getters cannot prove multi-controller isolation;
/// new admission and watchdog tests must use this snapshot instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimPic16Snapshot {
    raw_state: u8,
    voltage_pic: Option<u8>,
    voltage_mv: Option<u16>,
    voltage_enabled: bool,
    chain_live: bool,
    heartbeat_count: u64,
    heartbeat_times: Vec<Duration>,
    watchdog_expired: bool,
    generation: u64,
}

impl SimPic16Snapshot {
    pub fn raw_state(&self) -> u8 {
        self.raw_state
    }

    pub fn voltage_pic(&self) -> Option<u8> {
        self.voltage_pic
    }

    pub fn voltage_mv(&self) -> Option<u16> {
        self.voltage_mv
    }

    pub fn voltage_enabled(&self) -> bool {
        self.voltage_enabled
    }

    pub fn chain_live(&self) -> bool {
        self.chain_live
    }

    pub fn heartbeat_count(&self) -> u64 {
        self.heartbeat_count
    }

    pub fn heartbeat_times(&self) -> &[Duration] {
        &self.heartbeat_times
    }

    pub fn watchdog_expired(&self) -> bool {
        self.watchdog_expired
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug)]
struct SimI2cState {
    service_identity: usize,
    controller: SimControllerKind,
    controller_addresses: Vec<u8>,
    devices: HashMap<(u8, u8), DeviceState>,
    pic16_devices: HashMap<(u8, u8), SimPic16DeviceState>,
    trace: Vec<TraceEvent>,
    voltage_mv: u16,
    voltage_enabled: bool,
    heartbeat_count: u64,
    timeout_jiffies: Option<u32>,
    virtual_now: Duration,
    controller_watchdog: Option<SimControllerWatchdog>,
    pic16_watchdog_timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy)]
struct SimControllerWatchdog {
    timeout: Duration,
    last_heartbeat: Duration,
    expired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimI2cStallState {
    Idle,
    Armed,
    Stalled,
    Released,
}

/// Logical PIC16F1704 app-mode commands accepted by the production runtime.
///
/// SET_VOLTAGE and ENABLE_VOLTAGE have the same framing on stock and BraiinsOS
/// firmware. They are write-only commands: a successful I2C write is the only
/// acknowledgement and must not leave a synthetic response byte queued for a
/// later read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pic16Command {
    JumpFromLoader,
    GetVersionStock,
    GetVersionBraiins,
    GetVoltage,
    SetVoltage(u8),
    SetVoltageEnabled(bool),
    Heartbeat,
}

#[derive(Debug)]
struct SimI2cStallGate {
    state: Mutex<SimI2cStallState>,
    changed: Condvar,
}

impl SimI2cStallGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(SimI2cStallState::Idle),
            changed: Condvar::new(),
        }
    }
}

/// Cloneable virtual I2C fabric. Every `I2cBus` opened by a SimPlatform shares
/// this state, matching the physical fact that the buses address the same PSU
/// and hashboard controllers.
#[derive(Clone)]
pub struct SimI2cBackend {
    state: Arc<Mutex<SimI2cState>>,
    stall: Arc<SimI2cStallGate>,
}

impl SimI2cBackend {
    pub fn for_profile(profile: SimBoardProfile) -> Self {
        let controller = SimControllerKind::for_model(profile.model);
        let controller_addresses = match profile.model {
            SimModel::S9 => vec![0x55, 0x56, 0x57],
            SimModel::T17 | SimModel::S17Plus | SimModel::T17Plus => {
                vec![0x50, 0x51, 0x52]
            }
            // These profiles retain their protocol-family classification, but
            // no endpoint topology is admitted until hardware evidence pins it.
            SimModel::S11 | SimModel::S15 | SimModel::T15 => Vec::new(),
            _ => match controller {
                SimControllerKind::Dspic | SimControllerKind::Pic1704 => {
                    vec![0x20, 0x21, 0x22]
                }
                SimControllerKind::Pic16 | SimControllerKind::NoPic => Vec::new(),
            },
        };
        Self::with_controller_addresses(controller, controller_addresses)
    }

    pub fn with_controller(controller: SimControllerKind) -> Self {
        let controller_addresses = match controller {
            SimControllerKind::Pic16 => vec![0x55, 0x56, 0x57],
            SimControllerKind::Dspic => vec![0x20, 0x21, 0x22],
            SimControllerKind::Pic1704 => vec![0x20, 0x21, 0x22],
            SimControllerKind::NoPic => Vec::new(),
        };
        Self::with_controller_addresses(controller, controller_addresses)
    }

    fn with_controller_addresses(
        controller: SimControllerKind,
        controller_addresses: Vec<u8>,
    ) -> Self {
        static NEXT_SERVICE_IDENTITY: AtomicUsize = AtomicUsize::new(1);
        let service_identity = NEXT_SERVICE_IDENTITY.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            service_identity, 0,
            "simulated I2C service identity space exhausted"
        );
        let pic16_devices = if controller == SimControllerKind::Pic16 {
            controller_addresses
                .iter()
                .copied()
                .map(|address| ((0, address), SimPic16DeviceState::default()))
                .collect()
        } else {
            HashMap::new()
        };
        Self {
            state: Arc::new(Mutex::new(SimI2cState {
                service_identity,
                controller,
                controller_addresses,
                devices: HashMap::new(),
                pic16_devices,
                trace: Vec::new(),
                voltage_mv: 13_700,
                voltage_enabled: false,
                heartbeat_count: 0,
                timeout_jiffies: None,
                virtual_now: Duration::ZERO,
                controller_watchdog: None,
                pic16_watchdog_timeout: None,
            })),
            stall: Arc::new(SimI2cStallGate::new()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, SimI2cState>> {
        self.state
            .lock()
            .map_err(|_| HalError::Other("sim I2C state lock poisoned".to_string()))
    }

    pub fn drain_trace(&self) -> Result<Vec<TraceEvent>> {
        Ok(std::mem::take(&mut self.lock()?.trace))
    }

    pub fn voltage_enabled(&self) -> Result<bool> {
        let state = self.lock()?;
        Ok(if state.controller == SimControllerKind::Pic16 {
            state
                .pic16_devices
                .values()
                .any(|device| device.voltage_enabled)
        } else {
            state.voltage_enabled
        })
    }

    pub fn voltage_mv(&self) -> Result<u16> {
        let state = self.lock()?;
        if state.controller != SimControllerKind::Pic16 {
            return Ok(state.voltage_mv);
        }
        if state.pic16_devices.is_empty() {
            return Err(HalError::Other(
                "simulated PIC16 topology has no evidence-backed voltage setpoint".into(),
            ));
        }
        let mut common = None;
        for device in state.pic16_devices.values() {
            let Some(voltage) = device.voltage_pic.map(Self::pic16_voltage_mv) else {
                return Err(HalError::Other(
                    "simulated PIC16 endpoint voltage is unprogrammed; use pic16_snapshot for endpoint-local evidence"
                        .into(),
                ));
            };
            if common.is_some_and(|expected| expected != voltage) {
                return Err(HalError::Other(
                    "simulated PIC16 endpoint voltages diverge; use pic16_snapshot for endpoint-local evidence"
                        .into(),
                ));
            }
            common = Some(voltage);
        }
        common.ok_or_else(|| {
            HalError::Other("simulated PIC16 voltage aggregation produced no value".into())
        })
    }

    pub fn heartbeat_count(&self) -> Result<u64> {
        let state = self.lock()?;
        Ok(if state.controller == SimControllerKind::Pic16 {
            state
                .pic16_devices
                .values()
                .map(|device| device.heartbeat_count)
                .fold(0_u64, u64::saturating_add)
        } else {
            state.heartbeat_count
        })
    }

    pub fn pic16_snapshot(&self, bus: u8, address: u8) -> Result<SimPic16Snapshot> {
        let mut state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        let device = state.pic16_devices.entry((bus, address)).or_default();
        Ok(SimPic16Snapshot {
            raw_state: device.raw_state,
            voltage_pic: device.voltage_pic,
            voltage_mv: device.voltage_pic.map(Self::pic16_voltage_mv),
            voltage_enabled: device.voltage_enabled,
            chain_live: device.chain_live,
            heartbeat_count: device.heartbeat_count,
            heartbeat_times: device.heartbeat_times.iter().copied().collect(),
            watchdog_expired: device
                .watchdog
                .map(|watchdog| watchdog.expired)
                .unwrap_or(false),
            generation: device.generation,
        })
    }

    /// Configure one simulated PIC16's raw mode byte before an admission test.
    /// Changing mode represents a controller lifecycle transition: voltage and
    /// heartbeat credit are cleared and the endpoint generation advances.
    pub fn configure_pic16_raw_state(&self, bus: u8, address: u8, raw_state: u8) -> Result<()> {
        let mut state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        let virtual_now = state.virtual_now;
        let watchdog_timeout = state.pic16_watchdog_timeout;
        {
            let device = state.pic16_devices.entry((bus, address)).or_default();
            device.raw_state = raw_state;
            device.voltage_pic = None;
            device.voltage_enabled = false;
            device.heartbeat_count = 0;
            device.heartbeat_times.clear();
            device.generation = device.generation.saturating_add(1);
            device.invalidate_live_chain_lease();
            device.watchdog = watchdog_timeout.map(|timeout| SimControllerWatchdog {
                timeout,
                last_heartbeat: virtual_now,
                expired: false,
            });
        }
        Self::clear_pic16_transport_state(&mut state, bus, address);
        Self::refresh_pic16_compatibility_state(&mut state);
        Ok(())
    }

    /// Establish explicit simulator-only evidence that the ASIC chain behind
    /// one powered PIC16 endpoint is live. PIC application mode and an enabled
    /// rail are necessary but deliberately insufficient.
    pub(crate) fn establish_pic16_live_chain(
        &self,
        bus: u8,
        address: u8,
    ) -> Result<Weak<AtomicBool>> {
        let mut state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        let device = state.pic16_devices.entry((bus, address)).or_default();
        if !Self::pic16_application_state(device.raw_state) || !device.voltage_enabled {
            return Err(HalError::Other(format!(
                "simulated PIC16 endpoint 0x{address:02X} on bus {bus} cannot prove a live chain while its controller rail is off or outside application mode"
            )));
        }
        Ok(device.establish_live_chain_lease())
    }

    pub(crate) fn invalidate_pic16_live_chain(&self, bus: u8, address: u8) -> Result<()> {
        let mut state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        state
            .pic16_devices
            .entry((bus, address))
            .or_default()
            .invalidate_live_chain_lease();
        Ok(())
    }

    pub(crate) fn pic16_live_chain_lease(&self, bus: u8, address: u8) -> Result<Weak<AtomicBool>> {
        let state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        let device = state
            .pic16_devices
            .get(&(bus, address))
            .expect("validated PIC16 topology has device state");
        if !device.chain_live || !device.live_chain_lease.load(Ordering::SeqCst) {
            return Err(HalError::Other(format!(
                "simulated PIC16 endpoint 0x{address:02X} on bus {bus} lacks explicit live-chain evidence"
            )));
        }
        Ok(Arc::downgrade(&device.live_chain_lease))
    }

    pub fn schedule_pic16_fault(
        &self,
        bus: u8,
        address: u8,
        operation: SimPic16Operation,
        successful_matches_before_fault: u64,
        effect: SimPic16Fault,
    ) -> Result<()> {
        if effect == SimPic16Fault::ShortRead && operation != SimPic16Operation::RawRead {
            return Err(HalError::Other(
                "simulated PIC16 ShortRead is valid only for RawRead".into(),
            ));
        }
        let mut state = self.lock()?;
        Self::validate_pic16_endpoint(&state, bus, address)?;
        state
            .pic16_devices
            .entry((bus, address))
            .or_default()
            .faults
            .entry(operation)
            .or_default()
            .push_back(ScheduledPic16Fault {
                successful_matches_before_fault,
                effect,
            });
        Ok(())
    }

    fn validate_pic16_endpoint(state: &SimI2cState, bus: u8, address: u8) -> Result<()> {
        if state.controller != SimControllerKind::Pic16 {
            return Err(HalError::Other(
                "simulated platform does not use PIC16 controllers".into(),
            ));
        }
        if bus != 0 || !state.controller_addresses.contains(&address) {
            return Err(HalError::I2c {
                bus,
                addr: address,
                detail: "simulated PIC16 endpoint is not present".into(),
            });
        }
        Ok(())
    }

    fn take_pic16_fault(
        device: &mut SimPic16DeviceState,
        operation: SimPic16Operation,
    ) -> Option<SimPic16Fault> {
        let (effect, queue_empty) = {
            let queue = device.faults.get_mut(&operation)?;
            let scheduled = queue.front_mut()?;
            if scheduled.successful_matches_before_fault > 0 {
                scheduled.successful_matches_before_fault -= 1;
                return None;
            }
            let effect = queue.pop_front().map(|fault| fault.effect);
            (effect, queue.is_empty())
        };
        if queue_empty {
            device.faults.remove(&operation);
        }
        effect
    }

    fn injected_pic16_fault(
        bus: u8,
        address: u8,
        operation: SimPic16Operation,
        effect: SimPic16Fault,
    ) -> HalError {
        HalError::I2c {
            bus,
            addr: address,
            detail: format!("injected PIC16 {operation:?} {effect:?} fault"),
        }
    }

    fn clear_pic16_transport_state(state: &mut SimI2cState, bus: u8, address: u8) {
        let transport = state.devices.entry((bus, address)).or_default();
        transport.accumulator.clear();
        transport.pending.clear();
    }

    fn refresh_pic16_compatibility_state(state: &mut SimI2cState) {
        state.voltage_enabled = state
            .pic16_devices
            .values()
            .any(|device| device.voltage_enabled);
        state.heartbeat_count = state
            .pic16_devices
            .values()
            .map(|device| device.heartbeat_count)
            .fold(0_u64, u64::saturating_add);
    }

    /// Stall the next simulated transfer until `release_transfer_stall()`.
    /// The fault gate is independent from device state, so watchdog time and
    /// voltage remain observable while the transfer is blocked indefinitely.
    pub fn arm_next_transfer_stall(&self) -> Result<()> {
        let mut state = self
            .stall
            .state
            .lock()
            .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
        if matches!(*state, SimI2cStallState::Armed | SimI2cStallState::Stalled) {
            return Err(HalError::Other(
                "sim I2C transfer stall is already armed or active".into(),
            ));
        }
        *state = SimI2cStallState::Armed;
        self.stall.changed.notify_all();
        Ok(())
    }

    pub fn wait_for_transfer_stall(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .stall
            .state
            .lock()
            .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
        loop {
            if *state == SimI2cStallState::Stalled {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let (next, wait) = self
                .stall
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
            state = next;
            if wait.timed_out() && *state != SimI2cStallState::Stalled {
                return Ok(false);
            }
        }
    }

    pub fn release_transfer_stall(&self) -> Result<()> {
        let mut state = self
            .stall
            .state
            .lock()
            .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
        *state = SimI2cStallState::Released;
        self.stall.changed.notify_all();
        Ok(())
    }

    fn stall_next_transfer_if_armed(&self) -> Result<()> {
        let mut state = self
            .stall
            .state
            .lock()
            .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
        match *state {
            SimI2cStallState::Idle => return Ok(()),
            SimI2cStallState::Released => {
                *state = SimI2cStallState::Idle;
                return Ok(());
            }
            SimI2cStallState::Armed => {
                *state = SimI2cStallState::Stalled;
                self.stall.changed.notify_all();
            }
            SimI2cStallState::Stalled => {}
        }
        while *state == SimI2cStallState::Stalled {
            state = self
                .stall
                .changed
                .wait(state)
                .map_err(|_| HalError::Other("sim I2C stall gate poisoned".into()))?;
        }
        *state = SimI2cStallState::Idle;
        Ok(())
    }

    pub fn configure_controller_watchdog(&self, timeout: Duration) -> Result<()> {
        if timeout.is_zero() {
            return Err(HalError::Other(
                "sim controller watchdog timeout must be non-zero".into(),
            ));
        }
        let mut state = self.lock()?;
        if state.controller == SimControllerKind::Pic16 {
            let virtual_now = state.virtual_now;
            let addresses = state.controller_addresses.clone();
            state.pic16_watchdog_timeout = Some(timeout);
            for address in addresses {
                state
                    .pic16_devices
                    .entry((0, address))
                    .or_default()
                    .watchdog = Some(SimControllerWatchdog {
                    timeout,
                    last_heartbeat: virtual_now,
                    expired: false,
                });
            }
            return Ok(());
        }
        state.controller_watchdog = Some(SimControllerWatchdog {
            timeout,
            last_heartbeat: state.virtual_now,
            expired: false,
        });
        Ok(())
    }

    pub fn advance_virtual_time(&self, delta: Duration) -> Result<()> {
        let mut state = self.lock()?;
        state.virtual_now = state.virtual_now.saturating_add(delta);
        if state.controller == SimControllerKind::Pic16 {
            Self::evaluate_pic16_watchdogs(&mut state);
        } else {
            Self::evaluate_controller_watchdog(&mut state);
        }
        Ok(())
    }

    pub fn controller_watchdog_expired(&self) -> Result<bool> {
        let state = self.lock()?;
        Ok(if state.controller == SimControllerKind::Pic16 {
            state.pic16_devices.values().any(|device| {
                device
                    .watchdog
                    .map(|watchdog| watchdog.expired)
                    .unwrap_or(false)
            })
        } else {
            state
                .controller_watchdog
                .map(|watchdog| watchdog.expired)
                .unwrap_or(false)
        })
    }

    fn command_from_frame(frame: &[u8]) -> Option<u8> {
        if frame.len() < 3 || frame[0..2] != [0x55, 0xAA] {
            return None;
        }
        if frame.len() >= 4 && matches!(frame[2], 0x04 | 0x05) {
            Some(frame[3])
        } else {
            Some(frame[2])
        }
    }

    fn pic16_command_from_frame(frame: &[u8]) -> Option<Pic16Command> {
        match frame {
            [0x55, 0xAA, PIC16_JUMP_FROM_LOADER] => Some(Pic16Command::JumpFromLoader),
            [0x55, 0xAA, PIC16_GET_VERSION_STOCK] => Some(Pic16Command::GetVersionStock),
            [0x55, 0xAA, PIC16_GET_VERSION_BRAIINS] => Some(Pic16Command::GetVersionBraiins),
            [0x55, 0xAA, PIC16_GET_VOLTAGE] => Some(Pic16Command::GetVoltage),
            [0x55, 0xAA, PIC16_SET_VOLTAGE, value] => Some(Pic16Command::SetVoltage(*value)),
            [0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x00] => {
                Some(Pic16Command::SetVoltageEnabled(false))
            }
            [0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01] => Some(Pic16Command::SetVoltageEnabled(true)),
            [0x55, 0xAA, PIC16_HEARTBEAT] => Some(Pic16Command::Heartbeat),
            _ => None,
        }
    }

    fn frame_complete(frame: &[u8], controller: SimControllerKind, addr: u8) -> bool {
        if addr == APW_ADDR {
            return frame.len() >= 3
                && frame[0..2] == [0x55, 0xAA]
                && frame.len() >= 3 + usize::from(frame[2]);
        }
        if !matches!(
            controller,
            SimControllerKind::Dspic | SimControllerKind::Pic16
        ) {
            return true;
        }
        let Some(cmd) = Self::command_from_frame(frame) else {
            return false;
        };
        if controller == SimControllerKind::Pic16 {
            let expected = match cmd {
                PIC16_SET_VOLTAGE | PIC16_ENABLE_VOLTAGE => 4,
                _ => 3,
            };
            return frame.len() >= expected;
        }
        if frame.len() >= 4 && matches!(frame[2], 0x04 | 0x05) {
            // dsPIC LEN counts CMD + payload + checksum after the two-byte
            // preamble. Canonical LEN=4 frames are 6 bytes total and VNish
            // LEN=5 frames are 7 bytes total. Using `3 + LEN` left every
            // framed production command permanently incomplete in the
            // accumulator and could falsely report successful safe-off.
            return frame.len() >= 2 + usize::from(frame[2]);
        }
        let expected = match (controller, cmd) {
            (SimControllerKind::Dspic, DSPIC_GET_VERSION | DSPIC_HEARTBEAT) => 3,
            (SimControllerKind::Dspic, DSPIC_SET_VOLTAGE) => 5,
            (SimControllerKind::Dspic, DSPIC_ENABLE_VOLTAGE) => 4,
            _ => frame.len(),
        };
        frame.len() >= expected
    }

    fn resynchronize_framed_accumulator(frame: &mut Vec<u8>) {
        loop {
            match frame.as_slice() {
                [] | [0x55] => return,
                [0x55, 0xAA, ..] => return,
                [0x55, ..] => {
                    frame.remove(0);
                }
                _ => {
                    frame.remove(0);
                }
            }
        }
    }

    fn dspic_reply(cmd: u8, read_len: usize) -> Vec<u8> {
        let canonical = match cmd {
            DSPIC_GET_VERSION => vec![DSPIC_GET_VERSION, 0x01, DSPIC_FW_APPLICATION, 0x00, 0x00],
            DSPIC_SET_VOLTAGE => vec![DSPIC_SET_VOLTAGE, 0x01, 0x00],
            DSPIC_ENABLE_VOLTAGE => vec![DSPIC_ENABLE_VOLTAGE, 0x01],
            DSPIC_HEARTBEAT => vec![DSPIC_HEARTBEAT, 0x01, 0x00, 0x00, 0x00, 0x00],
            _ => vec![0x01],
        };
        if read_len == 1 && cmd == DSPIC_GET_VERSION {
            vec![DSPIC_FW_APPLICATION]
        } else {
            canonical
        }
    }

    fn pic16_reply(command: Pic16Command, raw_state: u8, voltage_pic: Option<u8>) -> Vec<u8> {
        match command {
            Pic16Command::GetVersionStock | Pic16Command::GetVersionBraiins => vec![raw_state],
            Pic16Command::GetVoltage => voltage_pic.into_iter().collect(),
            Pic16Command::JumpFromLoader
            | Pic16Command::SetVoltage(_)
            | Pic16Command::SetVoltageEnabled(_)
            | Pic16Command::Heartbeat => Vec::new(),
        }
    }

    fn pic16_reply_for_endpoint(
        state: &SimI2cState,
        bus: u8,
        address: u8,
        command: Pic16Command,
    ) -> Vec<u8> {
        let device = state.pic16_devices.get(&(bus, address));
        Self::pic16_reply(
            command,
            device.map(|device| device.raw_state).unwrap_or_default(),
            device.and_then(|device| device.voltage_pic),
        )
    }

    fn pic16_application_state(raw_state: u8) -> bool {
        matches!(raw_state, 0x03 | 0x56 | 0x5A | 0x5E | PIC16_APP_MODE)
    }

    fn pic16_voltage_mv(pic_value: u8) -> u16 {
        let volts = (1608.42 - f64::from(pic_value)) / 170.42;
        (volts * 1000.0).round() as u16
    }

    fn fill_read(out: &mut [u8], bytes: &[u8]) -> usize {
        out.fill(0);
        let copied = out.len().min(bytes.len());
        out[..copied].copy_from_slice(&bytes[..copied]);
        copied
    }

    fn note_controller_enabled(state: &mut SimI2cState) {
        if let Some(ref mut watchdog) = state.controller_watchdog {
            watchdog.last_heartbeat = state.virtual_now;
            watchdog.expired = false;
        }
    }

    fn note_controller_heartbeat(state: &mut SimI2cState) {
        state.heartbeat_count = state.heartbeat_count.saturating_add(1);
        if state.voltage_enabled {
            if let Some(ref mut watchdog) = state.controller_watchdog {
                watchdog.last_heartbeat = state.virtual_now;
            }
        }
    }

    fn evaluate_controller_watchdog(state: &mut SimI2cState) {
        let Some(ref mut watchdog) = state.controller_watchdog else {
            return;
        };
        if !state.voltage_enabled
            || watchdog.expired
            || state.virtual_now.saturating_sub(watchdog.last_heartbeat) < watchdog.timeout
        {
            return;
        }
        state.voltage_enabled = false;
        watchdog.expired = true;
        let at_ms = u64::try_from(state.virtual_now.as_millis()).unwrap_or(u64::MAX);
        state
            .trace
            .push(TraceEvent::ControllerWatchdogExpired { at_ms });
    }

    fn evaluate_pic16_watchdogs(state: &mut SimI2cState) {
        let virtual_now = state.virtual_now;
        let at_ms = u64::try_from(virtual_now.as_millis()).unwrap_or(u64::MAX);
        let mut expirations = Vec::new();
        for (&(bus, address), device) in &mut state.pic16_devices {
            let Some(ref mut watchdog) = device.watchdog else {
                continue;
            };
            if !Self::pic16_application_state(device.raw_state)
                || watchdog.expired
                || virtual_now.saturating_sub(watchdog.last_heartbeat) < watchdog.timeout
            {
                continue;
            }
            device.voltage_enabled = false;
            device.raw_state = PIC16_BOOTLOADER_MODE;
            device.voltage_pic = None;
            device.heartbeat_count = 0;
            device.heartbeat_times.clear();
            device.generation = device.generation.saturating_add(1);
            watchdog.expired = true;
            device.invalidate_live_chain_lease();
            expirations.push((bus, address, device.generation));
        }
        expirations.sort_unstable_by_key(|(bus, address, _)| (*bus, *address));
        for &(bus, address, _) in &expirations {
            Self::clear_pic16_transport_state(state, bus, address);
        }
        Self::refresh_pic16_compatibility_state(state);
        for (bus, address, generation) in expirations {
            state
                .trace
                .push(TraceEvent::Pic16ControllerWatchdogExpired {
                    bus,
                    addr: address,
                    at_ms,
                    generation,
                });
        }
    }

    fn update_pic16_endpoint_state(
        state: &mut SimI2cState,
        bus: u8,
        address: u8,
        frame: &[u8],
    ) -> Result<()> {
        let Some(command) = Self::pic16_command_from_frame(frame) else {
            return Ok(());
        };
        let virtual_now = state.virtual_now;
        let watchdog_timeout = state.pic16_watchdog_timeout;
        let mut voltage_mv = None;
        let mut lifecycle_transition = false;
        let accepted_operation;
        {
            let device = state.pic16_devices.entry((bus, address)).or_default();
            let application_state = Self::pic16_application_state(device.raw_state);
            let valid_state = match command {
                Pic16Command::JumpFromLoader => device.raw_state == PIC16_BOOTLOADER_MODE,
                Pic16Command::GetVersionStock => {
                    matches!(device.raw_state, PIC16_BOOTLOADER_MODE | 0x56 | 0x5A | 0x5E)
                }
                Pic16Command::GetVersionBraiins => device.raw_state == 0x03,
                Pic16Command::GetVoltage => application_state && device.voltage_pic.is_some(),
                Pic16Command::SetVoltage(_)
                | Pic16Command::SetVoltageEnabled(_)
                | Pic16Command::Heartbeat => application_state,
            };
            if !valid_state {
                return Err(HalError::I2c {
                    bus,
                    addr: address,
                    detail: format!(
                        "simulated PIC16 {command:?} is invalid in raw state 0x{:02X}",
                        device.raw_state
                    ),
                });
            }
            let operation = match command {
                Pic16Command::JumpFromLoader => Some(SimPic16Operation::JumpFromLoader),
                Pic16Command::Heartbeat => Some(SimPic16Operation::Heartbeat),
                Pic16Command::SetVoltage(_) => Some(SimPic16Operation::SetVoltage),
                Pic16Command::GetVoltage => Some(SimPic16Operation::ReadVoltage),
                Pic16Command::SetVoltageEnabled(true) => Some(SimPic16Operation::EnableVoltage),
                Pic16Command::SetVoltageEnabled(false) => Some(SimPic16Operation::DisableVoltage),
                Pic16Command::GetVersionStock | Pic16Command::GetVersionBraiins => None,
            };
            if let Some(operation) = operation {
                if let Some(effect) = Self::take_pic16_fault(device, operation) {
                    return Err(Self::injected_pic16_fault(bus, address, operation, effect));
                }
            }
            accepted_operation = operation;
            match command {
                Pic16Command::JumpFromLoader => {
                    device.raw_state = PIC16_APP_MODE;
                    device.voltage_pic = None;
                    device.voltage_enabled = false;
                    device.heartbeat_count = 0;
                    device.heartbeat_times.clear();
                    device.generation = device.generation.saturating_add(1);
                    device.invalidate_live_chain_lease();
                    device.watchdog = watchdog_timeout.map(|timeout| SimControllerWatchdog {
                        timeout,
                        last_heartbeat: virtual_now,
                        expired: false,
                    });
                    lifecycle_transition = true;
                }
                Pic16Command::SetVoltage(value) => {
                    device.invalidate_live_chain_lease();
                    let millivolts = Self::pic16_voltage_mv(value);
                    device.voltage_pic = Some(value);
                    voltage_mv = Some(millivolts);
                }
                Pic16Command::SetVoltageEnabled(enabled) => {
                    device.invalidate_live_chain_lease();
                    device.voltage_enabled = enabled;
                    if enabled {
                        if let Some(ref mut watchdog) = device.watchdog {
                            watchdog.last_heartbeat = virtual_now;
                            watchdog.expired = false;
                        }
                    }
                }
                Pic16Command::Heartbeat => {
                    device.heartbeat_count = device.heartbeat_count.saturating_add(1);
                    if device.heartbeat_times.len() == PIC16_RECENT_HEARTBEAT_CAPACITY {
                        device.heartbeat_times.pop_front();
                    }
                    device.heartbeat_times.push_back(virtual_now);
                    if let Some(ref mut watchdog) = device.watchdog {
                        watchdog.last_heartbeat = virtual_now;
                        watchdog.expired = false;
                    }
                }
                Pic16Command::GetVersionStock
                | Pic16Command::GetVersionBraiins
                | Pic16Command::GetVoltage => {}
            }
        }
        if lifecycle_transition {
            Self::clear_pic16_transport_state(state, bus, address);
        }
        if let Some(millivolts) = voltage_mv {
            // Compatibility view only. Endpoint snapshots are authoritative
            // when multiple PIC16 controllers diverge.
            state.voltage_mv = millivolts;
        }
        if let Some(operation) = accepted_operation {
            let at_ms = u64::try_from(state.virtual_now.as_millis()).unwrap_or(u64::MAX);
            state.trace.push(TraceEvent::Pic16OperationAccepted {
                bus,
                addr: address,
                operation,
                at_ms,
            });
        }
        Self::refresh_pic16_compatibility_state(state);
        Ok(())
    }

    fn update_controller_state(
        state: &mut SimI2cState,
        bus: u8,
        address: u8,
        frame: &[u8],
    ) -> Result<()> {
        let Some(cmd) = Self::command_from_frame(frame) else {
            return Ok(());
        };
        match state.controller {
            SimControllerKind::Dspic => match cmd {
                DSPIC_SET_VOLTAGE => {
                    if frame.len() >= 5 && !matches!(frame[2], 0x04 | 0x05) {
                        state.voltage_mv = u16::from_be_bytes([frame[3], frame[4]]);
                    }
                }
                DSPIC_ENABLE_VOLTAGE => {
                    state.voltage_enabled = frame.iter().rev().any(|byte| *byte == 0x01);
                    if state.voltage_enabled {
                        Self::note_controller_enabled(state);
                    }
                }
                DSPIC_HEARTBEAT => Self::note_controller_heartbeat(state),
                _ => {}
            },
            SimControllerKind::Pic16 => {
                return Self::update_pic16_endpoint_state(state, bus, address, frame)
            }
            SimControllerKind::Pic1704 | SimControllerKind::NoPic => {}
        }
        Ok(())
    }

    fn pic1704_write(state: &mut SimI2cState, data: &[u8]) {
        if data.len() < 2 {
            return;
        }
        if data[0] == PIC1704_REG_CONTROL {
            match data[1] {
                PIC1704_CTRL_OFF => state.voltage_enabled = false,
                PIC1704_CTRL_ON => {
                    state.voltage_enabled = true;
                    Self::note_controller_enabled(state);
                }
                PIC1704_CTRL_HEARTBEAT => Self::note_controller_heartbeat(state),
                _ => {}
            }
        }
    }

    fn pic1704_read(state: &SimI2cState, reg: u8, len: usize) -> Vec<u8> {
        let mut out = match reg {
            PIC1704_REG_VERSION => vec![PIC1704_FW_APPLICATION],
            PIC1704_REG_VOLTAGE_L => state.voltage_mv.to_le_bytes().to_vec(),
            PIC1704_REG_STATUS => vec![0x02 | u8::from(state.voltage_enabled)],
            _ => vec![0; len],
        };
        out.resize(len, 0);
        out
    }

    fn apw_response(cmd: u8, state: &SimI2cState) -> Vec<u8> {
        match cmd {
            APW_GET_VERSION => [0x01, 0x00]
                .into_iter()
                .chain(b"APW121215a".iter().copied())
                .collect(),
            APW_MEASURE_VOLTAGE => {
                let raw =
                    ((f32::from(state.voltage_mv) / 1000.0) * 63.017 - 0.8615).max(0.0) as u16;
                vec![cmd, 0x00, (raw >> 8) as u8, raw as u8]
            }
            APW_READ_STATE => vec![cmd, 0x00, 0x00, u8::from(state.voltage_enabled)],
            _ => vec![cmd, 0x01],
        }
    }

    fn process_complete_frame(
        state: &mut SimI2cState,
        bus: u8,
        addr: u8,
        frame: &[u8],
    ) -> Result<()> {
        if addr == APW_ADDR {
            let Some(cmd) = frame.get(3).copied() else {
                return Ok(());
            };
            if cmd == APW_SET_VOLTAGE {
                if frame.len() >= 5 {
                    let dac = frame[4];
                    let volts = 15.1084 - 0.013046 * f64::from(dac);
                    state.voltage_mv = (volts * 1000.0).round() as u16;
                }
            } else if cmd == APW_WATCHDOG {
                state.heartbeat_count += 1;
            } else {
                let response = Self::apw_response(cmd, state);
                let device = state.devices.entry((bus, addr)).or_default();
                device.pending.clear();
                device.pending.extend(response);
            }
            return Ok(());
        }

        if state.controller_addresses.contains(&addr) {
            if state.controller == SimControllerKind::Pic1704 {
                Self::pic1704_write(state, frame);
                return Ok(());
            }
            if state.controller == SimControllerKind::Pic16 {
                // A new complete command supersedes any unread reply from an
                // older command; real controller reply state is not a FIFO.
                state
                    .devices
                    .entry((bus, addr))
                    .or_default()
                    .pending
                    .clear();
            }
            Self::update_controller_state(state, bus, addr, frame)?;
            if let Some(cmd) = Self::command_from_frame(frame) {
                let response = match state.controller {
                    SimControllerKind::Dspic => Self::dspic_reply(cmd, usize::MAX),
                    SimControllerKind::Pic16 => Self::pic16_command_from_frame(frame)
                        .map(|command| Self::pic16_reply_for_endpoint(state, bus, addr, command))
                        .unwrap_or_default(),
                    SimControllerKind::Pic1704 | SimControllerKind::NoPic => Vec::new(),
                };
                state
                    .devices
                    .entry((bus, addr))
                    .or_default()
                    .pending
                    .extend(response);
            }
        }
        Ok(())
    }
}

impl I2cSimBackendTrait for SimI2cBackend {
    fn service_identity(&self) -> Option<usize> {
        self.lock().ok().map(|state| state.service_identity)
    }

    fn service_time(&self, _bus: u8) -> Option<Duration> {
        self.lock().ok().map(|state| state.virtual_now)
    }

    fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
        self.stall_next_transfer_if_armed()?;
        let mut state = self.lock()?;
        state.trace.push(TraceEvent::I2cWrite {
            bus,
            addr,
            bytes: data.to_vec(),
        });

        let is_known = addr == APW_ADDR
            || (state.controller_addresses.contains(&addr)
                && (state.controller != SimControllerKind::Pic16 || bus == 0));
        if !is_known {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: "simulated device is not present".to_string(),
            });
        }

        if state.controller == SimControllerKind::Pic1704
            && state.controller_addresses.contains(&addr)
            && data.len() >= 2
        {
            Self::pic1704_write(&mut state, data);
            return Ok(data.len());
        }

        let controller = state.controller;
        let frame = {
            let device = state.devices.entry((bus, addr)).or_default();
            device.accumulator.extend_from_slice(data);
            if matches!(
                controller,
                SimControllerKind::Dspic | SimControllerKind::Pic16
            ) {
                Self::resynchronize_framed_accumulator(&mut device.accumulator);
            }
            if addr == APW_ADDR {
                device.pending.extend(data.iter().copied());
            }
            device.accumulator.clone()
        };
        if Self::frame_complete(&frame, state.controller, addr) {
            let process_result = Self::process_complete_frame(&mut state, bus, addr, &frame);
            // A failed complete command must not survive into the worker's
            // mandatory parser flush and become falsely credited on replay.
            state
                .devices
                .entry((bus, addr))
                .or_default()
                .accumulator
                .clear();
            process_result?;
        }
        Ok(data.len())
    }

    fn read(&self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<usize> {
        self.stall_next_transfer_if_armed()?;
        let mut state = self.lock()?;
        let is_known = addr == APW_ADDR
            || (state.controller_addresses.contains(&addr)
                && (state.controller != SimControllerKind::Pic16 || bus == 0));
        if !is_known {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: "simulated device is not present".to_string(),
            });
        }
        if buf.is_empty() {
            state.trace.push(TraceEvent::I2cRead {
                bus,
                addr,
                bytes: Vec::new(),
            });
            return Ok(0);
        }

        let mut bytes = Vec::with_capacity(buf.len());
        if let Some(device) = state.devices.get_mut(&(bus, addr)) {
            while bytes.len() < buf.len() {
                let Some(byte) = device.pending.pop_front() else {
                    break;
                };
                bytes.push(byte);
            }
        }
        if bytes.is_empty() {
            let is_pic16_endpoint = state.controller == SimControllerKind::Pic16
                && bus == 0
                && state.controller_addresses.contains(&addr);
            if is_pic16_endpoint {
                let fault = Self::take_pic16_fault(
                    state.pic16_devices.entry((bus, addr)).or_default(),
                    SimPic16Operation::RawRead,
                );
                if let Some(effect) = fault {
                    if effect == SimPic16Fault::ShortRead {
                        buf.fill(0);
                        state.trace.push(TraceEvent::I2cRead {
                            bus,
                            addr,
                            bytes: Vec::new(),
                        });
                        return Ok(0);
                    }
                    return Err(Self::injected_pic16_fault(
                        bus,
                        addr,
                        SimPic16Operation::RawRead,
                        effect,
                    ));
                }
                let at_ms = u64::try_from(state.virtual_now.as_millis()).unwrap_or(u64::MAX);
                state.trace.push(TraceEvent::Pic16OperationAccepted {
                    bus,
                    addr,
                    operation: SimPic16Operation::RawRead,
                    at_ms,
                });
            }
            bytes.push(match state.controller {
                SimControllerKind::Dspic => DSPIC_FW_APPLICATION,
                SimControllerKind::Pic16 if is_pic16_endpoint => {
                    state
                        .pic16_devices
                        .entry((bus, addr))
                        .or_default()
                        .raw_state
                }
                SimControllerKind::Pic16 => 0,
                SimControllerKind::Pic1704 => PIC1704_FW_APPLICATION,
                SimControllerKind::NoPic => 0,
            });
        }
        let copied = Self::fill_read(buf, &bytes);
        state.trace.push(TraceEvent::I2cRead {
            bus,
            addr,
            bytes: buf[..copied].to_vec(),
        });
        Ok(copied)
    }

    fn write_read(&self, bus: u8, addr: u8, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        self.stall_next_transfer_if_armed()?;
        let mut state = self.lock()?;
        if addr != APW_ADDR
            && (!state.controller_addresses.contains(&addr)
                || (state.controller == SimControllerKind::Pic16 && bus != 0))
        {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: "simulated device is not present".to_string(),
            });
        }

        let reply = if addr == APW_ADDR {
            let cmd = write_data.get(3).copied().unwrap_or_default();
            Self::apw_response(cmd, &state)
        } else if state.controller == SimControllerKind::Pic1704 {
            Self::pic1704_read(
                &state,
                write_data.first().copied().unwrap_or_default(),
                read_buf.len(),
            )
        } else {
            if state.controller == SimControllerKind::Pic16 {
                state
                    .devices
                    .entry((bus, addr))
                    .or_default()
                    .pending
                    .clear();
            }
            Self::update_controller_state(&mut state, bus, addr, write_data)?;
            let cmd = Self::command_from_frame(write_data).unwrap_or_default();
            match state.controller {
                SimControllerKind::Dspic => Self::dspic_reply(cmd, read_buf.len()),
                SimControllerKind::Pic16 => Self::pic16_command_from_frame(write_data)
                    .map(|command| Self::pic16_reply_for_endpoint(&state, bus, addr, command))
                    .unwrap_or_default(),
                SimControllerKind::Pic1704 | SimControllerKind::NoPic => Vec::new(),
            }
        };
        Self::fill_read(read_buf, &reply);
        state.trace.push(TraceEvent::I2cWriteRead {
            bus,
            addr,
            write: write_data.to_vec(),
            read: read_buf.to_vec(),
        });
        Ok(())
    }

    fn set_timeout(&self, bus: u8, timeout_jiffies: u32) -> Result<()> {
        let mut state = self.lock()?;
        state.timeout_jiffies = Some(timeout_jiffies);
        state.trace.push(TraceEvent::I2cTimeoutChanged {
            bus,
            timeout_jiffies,
        });
        Ok(())
    }

    fn bus_recovery(&self, bus: u8) {
        if let Ok(mut state) = self.state.lock() {
            for device in state.devices.values_mut() {
                device.accumulator.clear();
                device.pending.clear();
            }
            state.trace.push(TraceEvent::I2cRecovery { bus });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::I2cBus;

    #[test]
    fn dspic_get_version_set_voltage_enable_and_heartbeat() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x20).expect("select dsPIC");

        let mut version = [0_u8; 5];
        bus.write_read(&[0x55, 0xAA, 0x17], &mut version)
            .expect("GET_VERSION");
        assert_eq!(version[2], DSPIC_FW_APPLICATION);

        bus.write(&[0x55, 0xAA, 0x10, 0x35, 0x84])
            .expect("SET_VOLTAGE 13700mV");
        bus.write(&[0x55, 0xAA, 0x15, 0x01])
            .expect("ENABLE_VOLTAGE");
        bus.write(&[0x55, 0xAA, 0x16]).expect("heartbeat");
        assert_eq!(backend.voltage_mv().expect("voltage state"), 13_700);
        assert!(backend.voltage_enabled().expect("enable state"));
        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 1);
    }

    #[test]
    fn pic16_runtime_frames_complete_only_after_their_required_arguments() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        for byte in [0x55, 0xAA, PIC16_SET_VOLTAGE] {
            bus.write(&[byte]).expect("partial SET_VOLTAGE byte");
        }
        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("voltage before value")
                .voltage_mv(),
            None
        );

        let pic_value = 100;
        bus.write(&[pic_value]).expect("SET_VOLTAGE value");
        let expected_mv = (((1608.42 - f64::from(pic_value)) / 170.42) * 1000.0).round() as u16;
        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("voltage after value")
                .voltage_mv(),
            Some(expected_mv)
        );

        for byte in [0x55, 0xAA, PIC16_ENABLE_VOLTAGE] {
            bus.write(&[byte]).expect("partial ENABLE_VOLTAGE byte");
        }
        assert!(!backend.voltage_enabled().expect("enable before argument"));
        bus.write(&[0x01]).expect("ENABLE_VOLTAGE argument");
        assert!(backend.voltage_enabled().expect("enabled state"));

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x00])
            .expect("DISABLE_VOLTAGE frame");
        assert!(!backend.voltage_enabled().expect("disabled state"));
    }

    #[test]
    fn pic16_write_only_commands_do_not_queue_synthetic_acknowledgements() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .configure_pic16_raw_state(0, 0x55, 0x5A)
            .expect("configure stock PIC version");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .expect("SET_VOLTAGE frame");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .expect("ENABLE_VOLTAGE frame");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("HEARTBEAT frame");

        let mut raw = [0_u8; 1];
        bus.read(&mut raw).expect("raw app-mode read");
        assert_eq!(raw, [0x5A]);
        assert!(backend.voltage_enabled().expect("enable state"));
        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 1);

        let mut version = [0_u8; 1];
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .expect("stock GET_VERSION");
        assert_eq!(version, [0x5A]);
    }

    #[test]
    fn pic16_version_queries_only_report_evidence_backed_firmware_states() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        let mut version = [0_u8; 1];

        backend
            .configure_pic16_raw_state(0, 0x55, 0x56)
            .expect("configure stock version");
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .expect("stock version query");
        assert_eq!(version, [0x56]);
        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VERSION_BRAIINS], &mut version)
            .is_err());

        backend
            .configure_pic16_raw_state(0, 0x55, 0x03)
            .expect("configure Braiins version");
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VERSION_BRAIINS], &mut version)
            .expect("Braiins version query");
        assert_eq!(version, [0x03]);
        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .is_err());

        backend
            .configure_pic16_raw_state(0, 0x55, PIC16_APP_MODE)
            .expect("configure unknown application mode");
        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .is_err());
        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VERSION_BRAIINS], &mut version)
            .is_err());
    }

    #[test]
    fn pic16_voltage_readback_is_unknown_until_programmed_and_endpoint_local() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        let mut voltage_pic = [0_u8; 1];

        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VOLTAGE], &mut voltage_pic)
            .is_err());
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .expect("program voltage");
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VOLTAGE], &mut voltage_pic)
            .expect("read voltage register");
        assert_eq!(voltage_pic, [100]);

        let first = backend.pic16_snapshot(0, 0x55).expect("first snapshot");
        let second = backend.pic16_snapshot(0, 0x56).expect("second snapshot");
        assert_eq!(first.voltage_pic(), Some(100));
        assert_eq!(
            first.voltage_mv(),
            Some(SimI2cBackend::pic16_voltage_mv(100))
        );
        assert_eq!(second.voltage_pic(), None);
        assert_eq!(second.voltage_mv(), None);
    }

    #[test]
    fn pic16_parser_flush_resynchronizes_before_the_next_valid_frame() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        bus.write_byte_by_byte(&[
            0x55, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ])
        .expect("parser flush");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("heartbeat after parser flush");

        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 1);
    }

    #[test]
    fn pic16_deprecated_and_malformed_frames_cannot_mutate_runtime_state() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        // Deprecated bmminer-era IDs are not PIC16F1704 app-mode commands.
        bus.write(&[0x55, 0xAA, 0x03, 100])
            .expect("deprecated SET_VOLTAGE frame");
        bus.write(&[0x55, 0xAA, 0x02])
            .expect("deprecated ENABLE frame");
        bus.write(&[0x55, 0xAA, 0x11])
            .expect("deprecated HEARTBEAT frame");
        bus.write(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x02])
            .expect("malformed ENABLE frame");

        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("voltage state")
                .voltage_mv(),
            None
        );
        assert!(!backend.voltage_enabled().expect("enable state"));
        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 0);
    }

    #[test]
    fn pic16_runtime_state_is_isolated_by_bus_and_address() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select first PIC16");

        let pic_value = 100;
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, pic_value])
            .expect("set first voltage");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .expect("enable first rail");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("heartbeat first controller");

        let first = backend.pic16_snapshot(0, 0x55).expect("first snapshot");
        let second = backend.pic16_snapshot(0, 0x56).expect("second snapshot");
        let expected_mv = (((1608.42 - f64::from(pic_value)) / 170.42) * 1000.0).round() as u16;
        assert_eq!(first.voltage_mv(), Some(expected_mv));
        assert!(first.voltage_enabled());
        assert_eq!(first.heartbeat_count(), 1);
        assert_eq!(second.voltage_mv(), None);
        assert!(!second.voltage_enabled());
        assert_eq!(second.heartbeat_count(), 0);
        assert!(backend.voltage_enabled().expect("aggregate rail state"));
        assert!(backend.voltage_mv().is_err(), "divergence must be explicit");
    }

    #[test]
    fn pic16_aggregate_voltage_is_independent_of_snapshot_order() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        let pic_value = 100;
        let expected_mv = SimI2cBackend::pic16_voltage_mv(pic_value);

        for address in [0x55, 0x56, 0x57] {
            bus.set_slave(address).expect("select PIC16 endpoint");
            bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, pic_value])
                .expect("program common voltage");
        }
        assert_eq!(backend.voltage_mv().expect("common voltage"), expected_mv);
        for address in [0x57, 0x55, 0x56] {
            backend
                .pic16_snapshot(0, address)
                .expect("order-independent snapshot");
            assert_eq!(backend.voltage_mv().expect("stable aggregate"), expected_mv);
        }

        bus.set_slave(0x56).expect("select divergent endpoint");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 101])
            .expect("program divergent voltage");
        assert!(backend.voltage_mv().is_err());
    }

    #[test]
    fn pic16_jump_requires_exact_bootloader_state_and_is_endpoint_local() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .configure_pic16_raw_state(0, 0x55, PIC16_BOOTLOADER_MODE)
            .expect("configure bootloader endpoint");
        backend
            .configure_pic16_raw_state(0, 0x56, 0x42)
            .expect("configure unknown endpoint state");
        let before_generation = backend
            .pic16_snapshot(0, 0x55)
            .expect("snapshot before jump")
            .generation();
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));

        bus.set_slave(0x55).expect("select bootloader endpoint");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_JUMP_FROM_LOADER])
            .expect("jump bootloader endpoint");
        bus.set_slave(0x56).expect("select unknown endpoint");
        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_JUMP_FROM_LOADER])
            .is_err());

        let jumped = backend.pic16_snapshot(0, 0x55).expect("jumped snapshot");
        let unchanged = backend.pic16_snapshot(0, 0x56).expect("unchanged snapshot");
        assert_eq!(jumped.raw_state(), PIC16_APP_MODE);
        assert_eq!(jumped.generation(), before_generation + 1);
        assert_eq!(unchanged.raw_state(), 0x42);
    }

    #[test]
    fn pic16_application_commands_fail_outside_application_mode() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .configure_pic16_raw_state(0, 0x55, PIC16_BOOTLOADER_MODE)
            .expect("configure bootloader endpoint");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select bootloader endpoint");

        for frame in [
            &[0x55, 0xAA, PIC16_HEARTBEAT][..],
            &[0x55, 0xAA, PIC16_SET_VOLTAGE, 100][..],
            &[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01][..],
        ] {
            assert!(
                bus.write_byte_by_byte(frame).is_err(),
                "bootloader command {frame:02X?} must not provide success evidence"
            );
        }
        let mut version = [0_u8; 1];
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .expect("bootloader stock version observation");
        assert_eq!(version, [PIC16_BOOTLOADER_MODE]);

        let snapshot = backend.pic16_snapshot(0, 0x55).expect("snapshot");
        assert_eq!(snapshot.raw_state(), PIC16_BOOTLOADER_MODE);
        assert_eq!(snapshot.voltage_mv(), None);
        assert!(!snapshot.voltage_enabled());
        assert_eq!(snapshot.heartbeat_count(), 0);
    }

    #[test]
    fn pic16_semantic_faults_are_endpoint_local_and_count_matching_operations() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x56,
                SimPic16Operation::Heartbeat,
                4,
                SimPic16Fault::TransportError,
            )
            .expect("schedule fifth heartbeat failure");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));

        bus.set_slave(0x55).expect("select healthy endpoint");
        for _ in 0..5 {
            bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
                .expect("healthy heartbeat");
        }
        bus.set_slave(0x56).expect("select faulted endpoint");
        for _ in 0..4 {
            bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
                .expect("pre-fault heartbeat");
        }
        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .is_err());

        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("healthy snapshot")
                .heartbeat_count(),
            5
        );
        assert_eq!(
            backend
                .pic16_snapshot(0, 0x56)
                .expect("faulted snapshot")
                .heartbeat_count(),
            4
        );
    }

    #[test]
    fn faulted_pic16_frame_is_not_replayed_by_parser_flush() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::Heartbeat,
                0,
                SimPic16Fault::TransportError,
            )
            .expect("schedule heartbeat failure");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .is_err());
        bus.write_byte_by_byte(&[0_u8; 16])
            .expect("worker-style parser flush");
        assert!(!backend
            .drain_trace()
            .expect("fault trace")
            .into_iter()
            .any(|event| matches!(
                event,
                TraceEvent::Pic16OperationAccepted {
                    operation: SimPic16Operation::Heartbeat,
                    ..
                }
            )));
        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("post-flush snapshot")
                .heartbeat_count(),
            0
        );

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("next heartbeat");
        assert_eq!(
            backend
                .pic16_snapshot(0, 0x55)
                .expect("recovered snapshot")
                .heartbeat_count(),
            1
        );
        assert_eq!(
            backend
                .drain_trace()
                .expect("accepted trace")
                .into_iter()
                .filter(|event| matches!(
                    event,
                    TraceEvent::Pic16OperationAccepted {
                        operation: SimPic16Operation::Heartbeat,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn pic16_activation_faults_do_not_mutate_endpoint_state() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::SetVoltage,
                0,
                SimPic16Fault::TransportError,
            )
            .expect("schedule voltage fault");
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::EnableVoltage,
                0,
                SimPic16Fault::TransportError,
            )
            .expect("schedule enable fault");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .is_err());
        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .is_err());

        let snapshot = backend.pic16_snapshot(0, 0x55).expect("snapshot");
        assert_eq!(snapshot.voltage_mv(), None);
        assert!(!snapshot.voltage_enabled());
    }

    #[test]
    fn pic16_enable_and_disable_faults_are_independent() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::DisableVoltage,
                0,
                SimPic16Fault::TransportError,
            )
            .expect("schedule disable fault");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .expect("enable must not consume disable fault");
        assert!(bus
            .write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x00])
            .is_err());
        assert!(backend
            .pic16_snapshot(0, 0x55)
            .expect("unresolved disable snapshot")
            .voltage_enabled());
    }

    #[test]
    fn pic16_lifecycle_transitions_clear_transport_and_setpoint_evidence() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        backend
            .configure_pic16_raw_state(0, 0x55, 0x56)
            .expect("configure stock endpoint");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .expect("program voltage");
        bus.write(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK])
            .expect("queue old-generation version");

        backend
            .configure_pic16_raw_state(0, 0x55, PIC16_BOOTLOADER_MODE)
            .expect("force lifecycle transition");
        let transitioned = backend
            .pic16_snapshot(0, 0x55)
            .expect("transitioned snapshot");
        assert_eq!(transitioned.voltage_pic(), None);
        let mut raw = [0_u8; 1];
        assert_eq!(bus.read(&mut raw).expect("current raw state"), 1);
        assert_eq!(raw, [PIC16_BOOTLOADER_MODE]);

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_JUMP_FROM_LOADER])
            .expect("jump to a new application generation");
        let jumped = backend.pic16_snapshot(0, 0x55).expect("jumped snapshot");
        assert_eq!(jumped.voltage_pic(), None);
        let mut voltage = [0_u8; 1];
        assert!(bus
            .write_read(&[0x55, 0xAA, PIC16_GET_VOLTAGE], &mut voltage)
            .is_err());
    }

    #[test]
    fn pic16_combined_command_supersedes_an_unread_queued_reply() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .configure_pic16_raw_state(0, 0x55, 0x56)
            .expect("configure stock endpoint");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .expect("program voltage");
        bus.write(&[0x55, 0xAA, PIC16_GET_VOLTAGE])
            .expect("queue voltage reply");
        let mut ignored = [0_u8; 1];
        bus.write_read(&[0x55, 0xAA, PIC16_HEARTBEAT], &mut ignored)
            .expect("combined command supersedes queued reply");

        let mut raw = [0_u8; 1];
        bus.read(&mut raw)
            .expect("raw state after combined command");
        assert_eq!(raw, [0x56]);
    }

    #[test]
    fn pic16_recent_heartbeat_history_is_bounded() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        for _ in 0..100 {
            bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
                .expect("heartbeat");
        }
        let snapshot = backend.pic16_snapshot(0, 0x55).expect("snapshot");
        assert_eq!(snapshot.heartbeat_count(), 100);
        assert_eq!(
            snapshot.heartbeat_times().len(),
            PIC16_RECENT_HEARTBEAT_CAPACITY
        );
    }

    #[test]
    fn pic16_short_raw_read_provides_no_application_evidence() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::RawRead,
                0,
                SimPic16Fault::ShortRead,
            )
            .expect("schedule short read");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        let mut raw = [0xFF_u8; 1];

        assert_eq!(bus.read(&mut raw).expect("short raw read"), 0);
        assert_eq!(raw, [0]);
        assert_eq!(bus.read(&mut raw).expect("next raw read"), 1);
        assert_eq!(raw, [PIC16_APP_MODE]);
    }

    #[test]
    fn zero_length_pic16_read_does_not_consume_fault_or_emit_acceptance() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .schedule_pic16_fault(
                0,
                0x55,
                SimPic16Operation::RawRead,
                0,
                SimPic16Fault::ShortRead,
            )
            .expect("schedule raw short read");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");

        assert_eq!(bus.read(&mut []).expect("zero-length read"), 0);
        assert!(!backend
            .drain_trace()
            .expect("zero-length trace")
            .into_iter()
            .any(|event| matches!(
                event,
                TraceEvent::Pic16OperationAccepted {
                    operation: SimPic16Operation::RawRead,
                    ..
                }
            )));
        let mut raw = [0xFF_u8; 1];
        assert_eq!(bus.read(&mut raw).expect("scheduled short read"), 0);
        assert_eq!(raw, [0]);
    }

    #[test]
    fn pic16_watchdogs_expire_only_endpoints_missing_their_deadline() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        backend
            .configure_controller_watchdog(Duration::from_millis(50))
            .expect("configure endpoint watchdogs");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        for address in [0x55, 0x56] {
            bus.set_slave(address).expect("select energized endpoint");
            bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
                .expect("energize endpoint");
        }
        let maintained_generation = backend
            .pic16_snapshot(0, 0x55)
            .expect("maintained generation")
            .generation();
        let expiring_generation = backend
            .pic16_snapshot(0, 0x56)
            .expect("expiring generation")
            .generation();
        backend
            .advance_virtual_time(Duration::from_millis(25))
            .expect("advance to heartbeat");
        bus.set_slave(0x55).expect("select maintained endpoint");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("maintaining heartbeat");

        backend
            .advance_virtual_time(Duration::from_millis(25))
            .expect("advance to first deadline");
        let maintained = backend
            .pic16_snapshot(0, 0x55)
            .expect("maintained snapshot");
        let expired = backend.pic16_snapshot(0, 0x56).expect("expired snapshot");
        assert_eq!(maintained.raw_state(), PIC16_APP_MODE);
        assert!(!maintained.watchdog_expired());
        assert!(maintained.voltage_enabled());
        assert_eq!(maintained.generation(), maintained_generation);
        assert_eq!(maintained.heartbeat_count(), 1);
        assert_eq!(expired.raw_state(), PIC16_BOOTLOADER_MODE);
        assert!(expired.watchdog_expired());
        assert!(!expired.voltage_enabled());
        assert_eq!(expired.generation(), expiring_generation + 1);
        assert_eq!(expired.voltage_pic(), None);
        assert_eq!(expired.heartbeat_count(), 0);
        let first_expiry_trace = backend
            .drain_trace()
            .expect("watchdog trace")
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    TraceEvent::Pic16ControllerWatchdogExpired {
                        bus: 0,
                        addr: 0x56,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(first_expiry_trace, 1);

        backend
            .advance_virtual_time(Duration::from_millis(25))
            .expect("advance to maintained deadline");
        let maintained = backend
            .pic16_snapshot(0, 0x55)
            .expect("eventually expired snapshot");
        assert!(maintained.watchdog_expired());
        assert!(!maintained.voltage_enabled());
        assert_eq!(maintained.generation(), maintained_generation + 1);
        let later_trace = backend.drain_trace().expect("later watchdog trace");
        assert_eq!(
            later_trace
                .iter()
                .filter(|event| matches!(
                    event,
                    TraceEvent::Pic16ControllerWatchdogExpired {
                        bus: 0,
                        addr: 0x55,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!later_trace.iter().any(|event| matches!(
            event,
            TraceEvent::Pic16ControllerWatchdogExpired {
                bus: 0,
                addr: 0x56,
                ..
            }
        )));
    }

    #[test]
    fn apw_reads_cannot_create_a_spurious_pic16_watchdog_endpoint() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(APW_ADDR).expect("select APW endpoint");
        let mut raw = [0_u8; 1];
        bus.read(&mut raw).expect("empty APW read fallback");
        assert_eq!(raw, [0], "APW fallback must not look like PIC app mode");
        backend
            .configure_controller_watchdog(Duration::from_millis(1))
            .expect("configure PIC16 watchdogs");
        backend
            .advance_virtual_time(Duration::from_millis(1))
            .expect("expire PIC16 endpoints");

        assert!(!backend
            .drain_trace()
            .expect("watchdog trace")
            .into_iter()
            .any(|event| matches!(
                event,
                TraceEvent::Pic16ControllerWatchdogExpired { addr: APW_ADDR, .. }
            )));
    }

    #[test]
    fn pic16_lifecycle_transition_refreshes_the_apw_compatibility_mirror() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x55).expect("select PIC16");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .expect("enable PIC16 rail");
        assert!(backend.voltage_enabled().expect("enabled aggregate"));

        backend
            .configure_pic16_raw_state(0, 0x55, PIC16_BOOTLOADER_MODE)
            .expect("transition endpoint");
        assert!(!backend.voltage_enabled().expect("disabled aggregate"));
        bus.set_slave(APW_ADDR).expect("select APW");
        let mut state_reply = [0_u8; 4];
        bus.write_read(&[0x55, 0xAA, 0x02, APW_READ_STATE, 0x00], &mut state_reply)
            .expect("APW state compatibility query");
        assert_eq!(state_reply[3], 0);
    }

    #[test]
    fn permanent_transfer_stall_does_not_hold_simulator_state_lock() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x20).expect("select dsPIC");
        backend.arm_next_transfer_stall().expect("arm stall");

        let writer = std::thread::spawn(move || bus.write(&[0x55, 0xAA, DSPIC_HEARTBEAT]));
        assert!(backend
            .wait_for_transfer_stall(Duration::from_secs(1))
            .expect("wait for stall"));

        let observed_at = Instant::now();
        assert!(!backend
            .voltage_enabled()
            .expect("voltage remains observable"));
        backend.drain_trace().expect("trace remains observable");
        backend
            .advance_virtual_time(Duration::from_secs(1))
            .expect("virtual time remains mutable");
        assert!(observed_at.elapsed() < Duration::from_millis(100));

        backend.release_transfer_stall().expect("release stall");
        writer
            .join()
            .expect("writer thread")
            .expect("stalled write");
    }

    #[test]
    fn virtual_controller_watchdog_cuts_voltage_exactly_at_deadline() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        backend
            .configure_controller_watchdog(Duration::from_millis(50))
            .expect("configure watchdog");
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x20).expect("select dsPIC");
        bus.write(&[0x55, 0xAA, DSPIC_ENABLE_VOLTAGE, 0x01])
            .expect("enable voltage");
        bus.write(&[0x55, 0xAA, DSPIC_HEARTBEAT])
            .expect("heartbeat");

        backend
            .advance_virtual_time(Duration::from_millis(49))
            .expect("advance before boundary");
        assert!(backend.voltage_enabled().expect("voltage before boundary"));
        assert!(!backend
            .controller_watchdog_expired()
            .expect("watchdog before boundary"));

        backend
            .advance_virtual_time(Duration::from_millis(1))
            .expect("advance to boundary");
        assert!(!backend.voltage_enabled().expect("voltage at boundary"));
        assert!(backend
            .controller_watchdog_expired()
            .expect("watchdog at boundary"));
        backend
            .advance_virtual_time(Duration::from_secs(1))
            .expect("advance after expiry");

        bus.write(&[0x55, 0xAA, DSPIC_HEARTBEAT])
            .expect("late heartbeat");
        assert!(!backend
            .voltage_enabled()
            .expect("late heartbeat cannot re-enable"));

        bus.write(&[0x55, 0xAA, DSPIC_ENABLE_VOLTAGE, 0x01])
            .expect("explicit re-enable");
        assert!(backend.voltage_enabled().expect("explicit re-enable state"));
        assert!(!backend
            .controller_watchdog_expired()
            .expect("watchdog rearmed"));

        let expiry_count = backend
            .drain_trace()
            .expect("trace")
            .into_iter()
            .filter(|event| matches!(event, TraceEvent::ControllerWatchdogExpired { .. }))
            .count();
        assert_eq!(expiry_count, 1);
    }

    #[test]
    fn framed_dspic_enable_and_disable_update_the_rail_at_exact_lengths() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x20).expect("select dsPIC");

        bus.write(&[0x55, 0xAA, 0x04, DSPIC_ENABLE_VOLTAGE, 0x01, 0x1A])
            .expect("canonical framed enable");
        assert!(backend.voltage_enabled().expect("canonical enable state"));

        bus.write(&[0x55, 0xAA, 0x05, DSPIC_ENABLE_VOLTAGE, 0x00, 0x00, 0x1A])
            .expect("VNish framed disable");
        assert!(!backend.voltage_enabled().expect("VNish disable state"));
    }

    #[test]
    fn pic1704_register_protocol_tracks_enable_and_voltage() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic1704);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x20).expect("select PIC1704");
        let mut version = [0_u8; 1];
        bus.write_read(&[PIC1704_REG_VERSION], &mut version)
            .expect("version register");
        assert_eq!(version[0], PIC1704_FW_APPLICATION);
        bus.write(&[PIC1704_REG_CONTROL, PIC1704_CTRL_ON])
            .expect("enable DC-DC");
        assert!(backend.voltage_enabled().expect("enable state"));
        let mut voltage = [0_u8; 2];
        bus.write_read(&[PIC1704_REG_VOLTAGE_L], &mut voltage)
            .expect("voltage register");
        assert_eq!(u16::from_le_bytes(voltage), 13_700);
    }

    #[test]
    fn simulator_still_enforces_i2c_write_denylist() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend));
        bus.set_write_denylist(&[0x50]);
        bus.set_slave(0x50)
            .expect("select protected EEPROM address");
        assert!(bus.write(&[0x00]).is_err());
        assert_eq!(bus.blocked_write_count(), 1);
    }

    #[test]
    fn apw_byte_echo_and_query_response_are_distinct() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::NoPic);
        let mut bus = I2cBus::open_sim(1, Arc::new(backend));
        bus.set_slave(APW_ADDR).expect("select APW");

        for byte in [0x55, 0xAA, 0x02, APW_GET_VERSION, 0x03] {
            bus.write(&[byte]).expect("query byte");
        }
        let mut response = [0_u8; 16];
        let count = bus.read(&mut response).expect("version response");
        assert!(count >= 4);
        assert_eq!(response[0], APW_GET_VERSION);
        assert_eq!(&response[2..5], b"APW");

        bus.bus_recovery().expect("simulated recovery");
        bus.write(&[0x55]).expect("write command byte");
        let mut echo = [0_u8; 1];
        assert_eq!(bus.read(&mut echo).expect("echo"), 1);
        assert_eq!(echo[0], 0x55);
    }
}
