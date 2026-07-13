//! Simulated I2C devices used by Antminer board-management paths.
//!
//! Protocol bytes are sourced from the in-tree production implementations:
//! `dcentrald-asic::dspic` (0x17/0x10/0x15/0x16),
//! `dcentrald-asic::pic1704::protocol` (registers 0x00/0x08/0x09),
//! `dcentrald-asic::pic` (PIC16F1704 app-mode commands), and
//! `dcentrald-hal::psu` (APW framed protocol). The simulator intentionally
//! models acknowledgements and state transitions, not electrical timing.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::i2c::I2cSimBackend as I2cSimBackendTrait;
use crate::{HalError, Result};

use super::{SimBoardProfile, SimModel, TraceEvent};

const DSPIC_GET_VERSION: u8 = 0x17;
const DSPIC_SET_VOLTAGE: u8 = 0x10;
const DSPIC_ENABLE_VOLTAGE: u8 = 0x15;
const DSPIC_HEARTBEAT: u8 = 0x16;
const DSPIC_FW_APPLICATION: u8 = 0x89;

const PIC16_GET_VERSION_STOCK: u8 = 0x04;
const PIC16_SET_VOLTAGE: u8 = 0x10;
const PIC16_ENABLE_VOLTAGE: u8 = 0x15;
const PIC16_HEARTBEAT: u8 = 0x16;
const PIC16_APP_MODE: u8 = 0x60;
const PIC16_FW_VERSION: u8 = 0x5A;

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

#[derive(Debug)]
struct SimI2cState {
    controller: SimControllerKind,
    controller_addresses: Vec<u8>,
    devices: HashMap<(u8, u8), DeviceState>,
    trace: Vec<TraceEvent>,
    voltage_mv: u16,
    voltage_enabled: bool,
    heartbeat_count: u64,
    timeout_jiffies: Option<u32>,
    virtual_now: Duration,
    controller_watchdog: Option<SimControllerWatchdog>,
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
    GetVersionStock,
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
        Self::with_controller(SimControllerKind::for_model(profile.model))
    }

    pub fn with_controller(controller: SimControllerKind) -> Self {
        let controller_addresses = match controller {
            SimControllerKind::Pic16 => vec![0x50, 0x51, 0x52, 0x55, 0x56, 0x57],
            SimControllerKind::Dspic => vec![0x20, 0x21, 0x22],
            SimControllerKind::Pic1704 => vec![0x20, 0x21, 0x22],
            SimControllerKind::NoPic => Vec::new(),
        };
        Self {
            state: Arc::new(Mutex::new(SimI2cState {
                controller,
                controller_addresses,
                devices: HashMap::new(),
                trace: Vec::new(),
                voltage_mv: 13_700,
                voltage_enabled: false,
                heartbeat_count: 0,
                timeout_jiffies: None,
                virtual_now: Duration::ZERO,
                controller_watchdog: None,
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
        Ok(self.lock()?.voltage_enabled)
    }

    pub fn voltage_mv(&self) -> Result<u16> {
        Ok(self.lock()?.voltage_mv)
    }

    pub fn heartbeat_count(&self) -> Result<u64> {
        Ok(self.lock()?.heartbeat_count)
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
        Self::evaluate_controller_watchdog(&mut state);
        Ok(())
    }

    pub fn controller_watchdog_expired(&self) -> Result<bool> {
        Ok(self
            .lock()?
            .controller_watchdog
            .map(|watchdog| watchdog.expired)
            .unwrap_or(false))
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
            [0x55, 0xAA, PIC16_GET_VERSION_STOCK] => Some(Pic16Command::GetVersionStock),
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

    fn pic16_reply(command: Pic16Command) -> Vec<u8> {
        match command {
            Pic16Command::GetVersionStock => vec![PIC16_FW_VERSION],
            Pic16Command::SetVoltage(_)
            | Pic16Command::SetVoltageEnabled(_)
            | Pic16Command::Heartbeat => Vec::new(),
        }
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

    fn update_controller_state(state: &mut SimI2cState, frame: &[u8]) {
        let Some(cmd) = Self::command_from_frame(frame) else {
            return;
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
            SimControllerKind::Pic16 => match Self::pic16_command_from_frame(frame) {
                Some(Pic16Command::SetVoltage(value)) => {
                    // Same formula documented by the production PIC path.
                    let volts = (1608.42 - f64::from(value)) / 170.42;
                    state.voltage_mv = (volts * 1000.0).round() as u16;
                }
                Some(Pic16Command::SetVoltageEnabled(enabled)) => {
                    state.voltage_enabled = enabled;
                    if enabled {
                        Self::note_controller_enabled(state);
                    }
                }
                Some(Pic16Command::Heartbeat) => Self::note_controller_heartbeat(state),
                Some(Pic16Command::GetVersionStock) | None => {}
            },
            SimControllerKind::Pic1704 | SimControllerKind::NoPic => {}
        }
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

    fn process_complete_frame(state: &mut SimI2cState, bus: u8, addr: u8, frame: &[u8]) {
        if addr == APW_ADDR {
            let Some(cmd) = frame.get(3).copied() else {
                return;
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
            return;
        }

        if state.controller_addresses.contains(&addr) {
            if state.controller == SimControllerKind::Pic1704 {
                Self::pic1704_write(state, frame);
                return;
            }
            Self::update_controller_state(state, frame);
            if let Some(cmd) = Self::command_from_frame(frame) {
                let response = match state.controller {
                    SimControllerKind::Dspic => Self::dspic_reply(cmd, usize::MAX),
                    SimControllerKind::Pic16 => Self::pic16_command_from_frame(frame)
                        .map(Self::pic16_reply)
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
    }
}

impl I2cSimBackendTrait for SimI2cBackend {
    fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
        self.stall_next_transfer_if_armed()?;
        let mut state = self.lock()?;
        state.trace.push(TraceEvent::I2cWrite {
            bus,
            addr,
            bytes: data.to_vec(),
        });

        let is_known = addr == APW_ADDR || state.controller_addresses.contains(&addr);
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

        let frame = {
            let device = state.devices.entry((bus, addr)).or_default();
            device.accumulator.extend_from_slice(data);
            if addr == APW_ADDR {
                device.pending.extend(data.iter().copied());
            }
            device.accumulator.clone()
        };
        if Self::frame_complete(&frame, state.controller, addr) {
            Self::process_complete_frame(&mut state, bus, addr, &frame);
            state
                .devices
                .entry((bus, addr))
                .or_default()
                .accumulator
                .clear();
        }
        Ok(data.len())
    }

    fn read(&self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<usize> {
        self.stall_next_transfer_if_armed()?;
        let mut state = self.lock()?;
        let is_known = addr == APW_ADDR || state.controller_addresses.contains(&addr);
        if !is_known {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: "simulated device is not present".to_string(),
            });
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
            bytes.push(match state.controller {
                SimControllerKind::Dspic => DSPIC_FW_APPLICATION,
                SimControllerKind::Pic16 => PIC16_APP_MODE,
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
        if addr != APW_ADDR && !state.controller_addresses.contains(&addr) {
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
            Self::update_controller_state(&mut state, write_data);
            let cmd = Self::command_from_frame(write_data).unwrap_or_default();
            match state.controller {
                SimControllerKind::Dspic => Self::dspic_reply(cmd, read_buf.len()),
                SimControllerKind::Pic16 => Self::pic16_command_from_frame(write_data)
                    .map(Self::pic16_reply)
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
        bus.set_slave(0x50).expect("select PIC16");

        for byte in [0x55, 0xAA, PIC16_SET_VOLTAGE] {
            bus.write(&[byte]).expect("partial SET_VOLTAGE byte");
        }
        assert_eq!(backend.voltage_mv().expect("voltage before value"), 13_700);

        let pic_value = 100;
        bus.write(&[pic_value]).expect("SET_VOLTAGE value");
        let expected_mv = (((1608.42 - f64::from(pic_value)) / 170.42) * 1000.0).round() as u16;
        assert_eq!(
            backend.voltage_mv().expect("voltage after value"),
            expected_mv
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
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x50).expect("select PIC16");

        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_SET_VOLTAGE, 100])
            .expect("SET_VOLTAGE frame");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x01])
            .expect("ENABLE_VOLTAGE frame");
        bus.write_byte_by_byte(&[0x55, 0xAA, PIC16_HEARTBEAT])
            .expect("HEARTBEAT frame");

        let mut raw = [0_u8; 1];
        bus.read(&mut raw).expect("raw app-mode read");
        assert_eq!(raw, [PIC16_APP_MODE]);
        assert!(backend.voltage_enabled().expect("enable state"));
        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 1);

        let mut version = [0_u8; 1];
        bus.write_read(&[0x55, 0xAA, PIC16_GET_VERSION_STOCK], &mut version)
            .expect("stock GET_VERSION");
        assert_eq!(version, [PIC16_FW_VERSION]);
    }

    #[test]
    fn pic16_deprecated_and_malformed_frames_cannot_mutate_runtime_state() {
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        bus.set_slave(0x50).expect("select PIC16");

        // Deprecated bmminer-era IDs are not PIC16F1704 app-mode commands.
        bus.write(&[0x55, 0xAA, 0x03, 100])
            .expect("deprecated SET_VOLTAGE frame");
        bus.write(&[0x55, 0xAA, 0x02])
            .expect("deprecated ENABLE frame");
        bus.write(&[0x55, 0xAA, 0x11])
            .expect("deprecated HEARTBEAT frame");
        bus.write(&[0x55, 0xAA, PIC16_ENABLE_VOLTAGE, 0x02])
            .expect("malformed ENABLE frame");

        assert_eq!(backend.voltage_mv().expect("voltage state"), 13_700);
        assert!(!backend.voltage_enabled().expect("enable state"));
        assert_eq!(backend.heartbeat_count().expect("heartbeat state"), 0);
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

        bus.bus_recovery();
        bus.write(&[0x55]).expect("write command byte");
        let mut echo = [0_u8; 1];
        assert_eq!(bus.read(&mut echo).expect("echo"), 1);
        assert_eq!(echo[0], 0x55);
    }
}
