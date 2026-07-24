//! Host-only simulated Antminer platform.
//!
//! This module exists only with the `sim-hal` Cargo feature. Runtime use is
//! separately protected by two simulation acknowledgements, an explicit
//! model, and a refusal check for real miner device signatures. S23 also
//! requires the ASIC scaffold driver's two existing acknowledgement keys.
//!
//! I2C/PSU, fan, GPIO, ASIC enumeration, register traffic, and nonce behavior
//! are emulated in this module tree. Tier claims remain evidence-gated by the
//! separate golden-vector and full-runtime proof harnesses.

mod chain;
mod fan;
mod gpio;
mod model_state;
mod psu_dspic;

use std::str::FromStr;
use std::time::Duration;

use crate::i2c::{spawn_sim_i2c_service, I2cBus, I2cServiceHandle};
use crate::platform::{
    BoardType, ChainAccess, FanAccess, GpioAccess, Platform, VoltageControllerEndpoint,
    VoltageControllerKind,
};
use crate::{HalError, Result};

pub use chain::{SimBm1397PlusBackend, SimChain, SimNoncePolicy, SimPic16Operation, TraceEvent};
pub use model_state::SimSiliconState;
pub use psu_dspic::{SimControllerKind, SimI2cBackend, SimPic16Fault, SimPic16Snapshot};

pub const SIM_ALLOW_ENV: &str = "DCENT_SIM_HAL";
pub const SIM_ACK_ENV: &str = "DCENT_CONFIRM_SIM_HAL_IS_NOT_REAL_HARDWARE";
pub const SIM_MODEL_ENV: &str = "DCENT_SIM_MODEL";

// These names intentionally mirror dcentrald-asic without introducing a
// HAL -> ASIC dependency cycle.
pub const SCAFFOLD_ALLOW_ENV: &str = "DCENT_ALLOW_SCAFFOLD_ASIC_DRIVERS";
pub const SCAFFOLD_ACK_ENV: &str = "DCENT_CONFIRM_SCAFFOLD_DRIVERS_ARE_SIMULATOR_STUBS";

/// Model identity selected explicitly for an offline simulation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimModel {
    S9,
    S11,
    S15,
    T15,
    S17,
    S17Pro,
    T17,
    S17Plus,
    T17Plus,
    S17e,
    S19,
    S19Pro,
    S19jPro,
    S19Xp,
    S19kPro,
    S21,
    S21Pro,
    S21Xp,
    S23,
}

impl SimModel {
    pub const fn slug(self) -> &'static str {
        match self {
            Self::S9 => "s9",
            Self::S11 => "s11",
            Self::S15 => "s15",
            Self::T15 => "t15",
            Self::S17 => "s17",
            Self::S17Pro => "s17pro",
            Self::T17 => "t17",
            Self::S17Plus => "s17plus",
            Self::T17Plus => "t17plus",
            Self::S17e => "s17e",
            Self::S19 => "s19",
            Self::S19Pro => "s19pro",
            Self::S19jPro => "s19jpro",
            Self::S19Xp => "s19xp",
            Self::S19kPro => "s19kpro",
            Self::S21 => "s21",
            Self::S21Pro => "s21pro",
            Self::S21Xp => "s21xp",
            Self::S23 => "s23",
        }
    }
}

impl FromStr for SimModel {
    type Err = HalError;

    fn from_str(value: &str) -> Result<Self> {
        let normalized = value
            .trim()
            .to_ascii_lowercase()
            .replace('+', "plus")
            .replace(['-', '_'], "");
        match normalized.as_str() {
            "s9" => Ok(Self::S9),
            "s11" => Ok(Self::S11),
            "s15" => Ok(Self::S15),
            "t15" => Ok(Self::T15),
            "s17" => Ok(Self::S17),
            "s17pro" => Ok(Self::S17Pro),
            "t17" => Ok(Self::T17),
            "s17plus" => Ok(Self::S17Plus),
            "t17plus" => Ok(Self::T17Plus),
            "s17e" => Ok(Self::S17e),
            "s19" => Ok(Self::S19),
            "s19pro" => Ok(Self::S19Pro),
            "s19jpro" => Ok(Self::S19jPro),
            "s19xp" => Ok(Self::S19Xp),
            "s19kpro" => Ok(Self::S19kPro),
            "s21" => Ok(Self::S21),
            "s21pro" => Ok(Self::S21Pro),
            "s21xp" => Ok(Self::S21Xp),
            "s23" => Ok(Self::S23),
            _ => Err(HalError::Platform(format!(
                "unsupported {SIM_MODEL_ENV} value '{value}'"
            ))),
        }
    }
}

/// Minimal model geometry used by the transport simulator.
///
/// Counts are copied from `dcentrald-asic::drivers::MINER_PROFILES` or its
/// model-specific comments. `None` is deliberate wherever held evidence or
/// current code disagrees; the simulator must not turn an estimate into proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimBoardProfile {
    pub model: SimModel,
    pub carrier: BoardType,
    pub chip_id: u16,
    pub chips_per_chain: Option<u8>,
    pub chain_count: u8,
    pub response_length: usize,
    pub default_baud: u32,
    pub fan_count: u8,
    /// Register-0 value returned before address assignment. This is a silicon
    /// identity word, not an invented per-chip address.
    pub enumeration_identity: u32,
    /// Three response trailer bytes where capture-backed; zero otherwise.
    pub enumeration_suffix: [u8; 3],
}

impl SimBoardProfile {
    pub const fn for_model(model: SimModel) -> Self {
        use SimModel::*;

        let (carrier, chip_id, chips_per_chain, response_length, default_baud) = match model {
            S9 => (BoardType::Zynq, 0x1387, Some(63), 9, 115_200),
            S11 | S15 | T15 => (BoardType::Zynq, 0x1391, None, 9, 115_200),
            S17 | S17Pro => (BoardType::Zynq, 0x1397, Some(48), 9, 115_740),
            T17 => (BoardType::Zynq, 0x1397, Some(30), 9, 115_740),
            S17Plus => (BoardType::Zynq, 0x1396, Some(65), 9, 115_740),
            T17Plus => (BoardType::Zynq, 0x1396, Some(44), 9, 115_740),
            S17e => (BoardType::Zynq, 0x1397, None, 9, 115_740),
            // Plain-S19 geometry remains intentionally unknown: held sources
            // disagree on the 76-vs-114 chip hashboard hint.
            S19 => (BoardType::Zynq, 0x1398, None, 9, 115_740),
            S19Pro => (BoardType::Zynq, 0x1398, Some(114), 9, 115_740),
            S19jPro => (BoardType::Zynq, 0x1362, Some(126), 11, 115_200),
            S19Xp => (BoardType::Amlogic, 0x1366, Some(110), 11, 115_200),
            S19kPro => (BoardType::Amlogic, 0x1366, Some(77), 11, 115_200),
            S21 => (BoardType::Amlogic, 0x1368, Some(108), 11, 115_200),
            S21Pro => (BoardType::Amlogic, 0x1370, Some(65), 11, 115_200),
            S21Xp => (BoardType::Amlogic, 0x1370, None, 11, 115_200),
            // S23 enumerates as 0x1372. Geometry remains unknown by operator
            // decision; never copy the projected 90-chip scaffold as truth.
            S23 => (BoardType::Amlogic, 0x1372, None, 11, 115_200),
        };

        Self {
            model,
            carrier,
            chip_id,
            chips_per_chain,
            chain_count: match model {
                S23 => 4,
                _ => 3,
            },
            response_length,
            default_baud,
            fan_count: 4,
            enumeration_identity: match chip_id {
                0x1397 => 0x1397_1800,
                0x1398 => 0x1398_1800,
                _ => (chip_id as u32) << 16,
            },
            // S17_BasicTest_Cap1.sal: AA 55 13 97 18 00 00 00 06,
            // repeated once per unaddressed chip after GetAddress.
            enumeration_suffix: if chip_id == 0x1397 {
                [0x00, 0x00, 0x06]
            } else {
                [0x00; 3]
            },
        }
    }
}

/// Known device signatures that make simulation unsafe on the current host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HardwareSignatures {
    pub uio0: bool,
    pub tty_o1: bool,
    pub uart_trans: bool,
    pub tty_s1: bool,
}

impl HardwareSignatures {
    fn detect() -> Self {
        Self {
            uio0: std::path::Path::new("/dev/uio0").exists(),
            tty_o1: std::path::Path::new("/dev/ttyO1").exists(),
            uart_trans: std::path::Path::new("/sys/module/uart_trans").exists(),
            tty_s1: std::path::Path::new("/dev/ttyS1").exists(),
        }
    }

    pub const fn any(self) -> bool {
        self.uio0 || self.tty_o1 || self.uart_trans || self.tty_s1
    }
}

/// Pure two-key gate. Model selection and hardware-signature refusal are
/// validated separately by [`validate_sim_request`].
pub fn should_use_sim_platform(allow: Option<&str>, ack: Option<&str>) -> bool {
    allow == Some("1") && ack == Some("1")
}

/// Pure fail-closed request validator used by environment loading and tests.
pub fn validate_sim_request(
    allow: Option<&str>,
    ack: Option<&str>,
    model: Option<&str>,
    hardware: HardwareSignatures,
    scaffold_allow: Option<&str>,
    scaffold_ack: Option<&str>,
) -> Result<SimModel> {
    if !should_use_sim_platform(allow, ack) {
        return Err(HalError::Platform(format!(
            "sim HAL requires {SIM_ALLOW_ENV}=1 and {SIM_ACK_ENV}=1"
        )));
    }
    let selected = model
        .ok_or_else(|| HalError::Platform(format!("sim HAL requires explicit {SIM_MODEL_ENV}")))?
        .parse::<SimModel>()?;
    if hardware.any() {
        return Err(HalError::Platform(
            "sim HAL refused: a real-miner hardware signature is present".to_string(),
        ));
    }
    if selected == SimModel::S23 && (scaffold_allow != Some("1") || scaffold_ack != Some("1")) {
        return Err(HalError::Platform(format!(
            "S23 simulation additionally requires {SCAFFOLD_ALLOW_ENV}=1 and {SCAFFOLD_ACK_ENV}=1"
        )));
    }
    Ok(selected)
}

/// True when any simulation key exists in the process environment.
/// Partial requests are routed to validation and fail closed.
pub fn sim_environment_is_mentioned() -> bool {
    [SIM_ALLOW_ENV, SIM_ACK_ENV, SIM_MODEL_ENV]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

/// In-memory platform. Cloned chain handles share one state machine per chain.
pub struct SimPlatform {
    profile: SimBoardProfile,
    chains: Vec<SimChain>,
    fan: fan::SimFan,
    gpio: gpio::SimGpio,
    i2c: SimI2cBackend,
    silicon: SimSiliconState,
}

impl SimPlatform {
    pub fn new(model: SimModel) -> Self {
        let profile = SimBoardProfile::for_model(model);
        let chains = (0..profile.chain_count)
            .map(|chain_id| SimChain::new(chain_id, profile))
            .collect();
        Self {
            profile,
            chains,
            fan: fan::SimFan::new(profile.fan_count),
            gpio: gpio::SimGpio::new(profile.chain_count),
            i2c: SimI2cBackend::for_profile(profile),
            silicon: SimSiliconState::for_profile(profile),
        }
    }

    pub fn from_env() -> Result<Self> {
        let allow = std::env::var(SIM_ALLOW_ENV).ok();
        let ack = std::env::var(SIM_ACK_ENV).ok();
        let model = std::env::var(SIM_MODEL_ENV).ok();
        let scaffold_allow = std::env::var(SCAFFOLD_ALLOW_ENV).ok();
        let scaffold_ack = std::env::var(SCAFFOLD_ACK_ENV).ok();
        let selected = validate_sim_request(
            allow.as_deref(),
            ack.as_deref(),
            model.as_deref(),
            HardwareSignatures::detect(),
            scaffold_allow.as_deref(),
            scaffold_ack.as_deref(),
        )?;
        Ok(Self::new(selected))
    }

    pub const fn profile(&self) -> SimBoardProfile {
        self.profile
    }

    pub const fn silicon(&self) -> &SimSiliconState {
        &self.silicon
    }

    pub fn open_bm1397plus_backend(&self, chain_id: u8) -> Result<SimBm1397PlusBackend> {
        if matches!(self.profile.chip_id, 0x1387 | 0x1391) {
            return Err(HalError::Platform(format!(
                "{} uses the legacy chain path, not Bm1397PlusChainBackend",
                self.profile.model.slug()
            )));
        }
        self.chains
            .get(chain_id as usize)
            .cloned()
            .map(SimBm1397PlusBackend::new)
            .ok_or_else(|| HalError::Platform(format!("sim chain {chain_id} not found")))
    }

    pub fn drain_i2c_trace(&self) -> Result<Vec<TraceEvent>> {
        self.i2c.drain_trace()
    }

    pub fn arm_next_i2c_transfer_stall(&self) -> Result<()> {
        self.i2c.arm_next_transfer_stall()
    }

    pub fn wait_for_i2c_transfer_stall(&self, timeout: Duration) -> Result<bool> {
        self.i2c.wait_for_transfer_stall(timeout)
    }

    pub fn release_i2c_transfer_stall(&self) -> Result<()> {
        self.i2c.release_transfer_stall()
    }

    pub fn configure_controller_watchdog(&self, timeout: Duration) -> Result<()> {
        self.i2c.configure_controller_watchdog(timeout)
    }

    pub fn advance_i2c_time(&self, delta: Duration) -> Result<()> {
        self.i2c.advance_virtual_time(delta)
    }

    pub fn i2c_voltage_enabled(&self) -> Result<bool> {
        self.i2c.voltage_enabled()
    }

    pub fn controller_watchdog_expired(&self) -> Result<bool> {
        self.i2c.controller_watchdog_expired()
    }

    pub fn pic16_snapshot(&self, bus: u8, address: u8) -> Result<SimPic16Snapshot> {
        self.i2c.pic16_snapshot(bus, address)
    }

    /// Explicitly establish simulator-only ASIC-chain liveness for one powered
    /// PIC16 domain. Tests must call this after their simulated enumeration;
    /// PIC application mode and voltage state do not imply a live chain.
    pub fn establish_pic16_live_chain(&self, bus: u8, address: u8) -> Result<()> {
        self.i2c.establish_pic16_live_chain(bus, address)?;
        Ok(())
    }

    /// Revoke simulator ASIC-chain liveness without changing PIC mode,
    /// setpoint, or rail state. This models chain-only failure independently
    /// from controller and power-domain state.
    pub fn invalidate_pic16_live_chain(&self, bus: u8, address: u8) -> Result<()> {
        self.i2c.invalidate_pic16_live_chain(bus, address)
    }

    /// Mint simulated running-endpoint evidence from an explicit live-chain
    /// lease and the exact I2C service allocation. Real platforms must bind the
    /// same capability to authoritative ASIC enumeration/liveness evidence.
    pub fn prove_running_pic16_endpoint(
        &self,
        service: &crate::i2c::I2cServiceHandle,
        bus: u8,
        address: u8,
    ) -> Result<crate::i2c::Pic16RunningEndpoint> {
        let live_chain_lease = self.i2c.pic16_live_chain_lease(bus, address)?;
        crate::i2c::Pic16RunningEndpoint::from_verified_handoff(
            self.pic16_endpoint(bus, address)?,
            service,
            live_chain_lease,
        )
    }

    pub fn configure_pic16_raw_state(&self, bus: u8, address: u8, raw_state: u8) -> Result<()> {
        self.i2c.configure_pic16_raw_state(bus, address, raw_state)
    }

    pub fn schedule_pic16_fault(
        &self,
        bus: u8,
        address: u8,
        operation: SimPic16Operation,
        successful_matches_before_fault: u64,
        effect: SimPic16Fault,
    ) -> Result<()> {
        self.i2c.schedule_pic16_fault(
            bus,
            address,
            operation,
            successful_matches_before_fault,
            effect,
        )
    }

    /// Open the same serialized I2C service surface used by daemon-side PIC
    /// and PSU controllers, backed by the shared simulator fabric.
    pub fn open_i2c_service(&self, bus: u8) -> Result<I2cServiceHandle> {
        let denylist = if matches!(
            self.profile.model,
            SimModel::S9
                | SimModel::S11
                | SimModel::S15
                | SimModel::T15
                | SimModel::T17
                | SimModel::S17Plus
                | SimModel::T17Plus
        ) {
            Vec::new()
        } else {
            (0x50..=0x57).collect()
        };
        Ok(spawn_sim_i2c_service(
            bus,
            std::sync::Arc::new(self.i2c.clone()),
            denylist,
        )?)
    }

    /// Issue an opaque PIC16 endpoint for host-only simulator integration
    /// tests. This constructor is unavailable without `sim-hal` and cannot
    /// mint production hardware authority.
    pub fn pic16_endpoint(&self, bus: u8, address: u8) -> Result<VoltageControllerEndpoint> {
        // The production typed service currently admits only the standard S9
        // topology. X17 endpoint state is modeled below the authority layer,
        // but must not be advertised as usable until typed SafeOff and worker
        // validators consume a profile-bound topology capability.
        if self.profile.model != SimModel::S9 || bus != 0 || !(0x55..=0x57).contains(&address) {
            return Err(HalError::Platform(format!(
                "{} simulator has no service-authorized PIC16 endpoint at bus {bus} address 0x{address:02X}",
                self.profile.model.slug()
            )));
        }
        Ok(VoltageControllerEndpoint::from_simulated_pic16(
            bus, address,
        ))
    }
}

impl Platform for SimPlatform {
    fn board_type(&self) -> BoardType {
        self.profile.carrier
    }

    fn chain_count(&self) -> u8 {
        self.profile.chain_count
    }

    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>> {
        self.chains
            .get(chain_id as usize)
            .cloned()
            .map(|chain| Box::new(chain) as Box<dyn ChainAccess>)
            .ok_or_else(|| HalError::Platform(format!("sim chain {chain_id} not found")))
    }

    fn open_i2c(&self, bus: u8) -> Result<I2cBus> {
        let mut handle = I2cBus::try_open_sim(bus, std::sync::Arc::new(self.i2c.clone()))?;
        if !matches!(
            self.profile.model,
            SimModel::S9
                | SimModel::S11
                | SimModel::S15
                | SimModel::T15
                | SimModel::T17
                | SimModel::S17Plus
                | SimModel::T17Plus
        ) {
            handle.set_write_denylist(&(0x50..=0x57).collect::<Vec<_>>());
        }
        Ok(handle)
    }

    fn open_fan(&self) -> Result<Box<dyn FanAccess>> {
        Ok(Box::new(self.fan.clone()))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        Ok(Box::new(self.gpio.clone()))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        match SimControllerKind::for_model(self.profile.model) {
            SimControllerKind::Pic16 => VoltageControllerKind::Pic16f1704,
            SimControllerKind::Dspic => VoltageControllerKind::Dspic33Ep,
            SimControllerKind::Pic1704 => VoltageControllerKind::Pic1704,
            SimControllerKind::NoPic => VoltageControllerKind::NoPic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_names_are_independent() {
        assert_ne!(SIM_ALLOW_ENV, SIM_ACK_ENV);
        assert_ne!(SCAFFOLD_ALLOW_ENV, SCAFFOLD_ACK_ENV);
    }

    #[test]
    fn gate_defaults_fail_closed() {
        assert!(!should_use_sim_platform(None, None));
        assert!(!should_use_sim_platform(Some("1"), None));
        assert!(should_use_sim_platform(Some("1"), Some("1")));
        assert!(
            validate_sim_request(None, None, None, HardwareSignatures::default(), None, None)
                .is_err()
        );
    }

    #[test]
    fn partial_gate_never_enables_simulation() {
        assert!(validate_sim_request(
            Some("1"),
            None,
            Some("s19pro"),
            HardwareSignatures::default(),
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn explicit_model_is_mandatory() {
        assert!(validate_sim_request(
            Some("1"),
            Some("1"),
            None,
            HardwareSignatures::default(),
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn any_real_hardware_signature_is_refused() {
        for hardware in [
            HardwareSignatures {
                uio0: true,
                ..HardwareSignatures::default()
            },
            HardwareSignatures {
                tty_o1: true,
                ..HardwareSignatures::default()
            },
            HardwareSignatures {
                uart_trans: true,
                ..HardwareSignatures::default()
            },
            HardwareSignatures {
                tty_s1: true,
                ..HardwareSignatures::default()
            },
        ] {
            let err =
                validate_sim_request(Some("1"), Some("1"), Some("s19pro"), hardware, None, None)
                    .expect_err("real hardware must be rejected");
            assert!(err.to_string().contains("real-miner hardware signature"));
        }
    }

    #[test]
    fn s23_requires_four_key_ceremony() {
        assert!(validate_sim_request(
            Some("1"),
            Some("1"),
            Some("s23"),
            HardwareSignatures::default(),
            None,
            None,
        )
        .is_err());
        assert_eq!(
            validate_sim_request(
                Some("1"),
                Some("1"),
                Some("s23"),
                HardwareSignatures::default(),
                Some("1"),
                Some("1"),
            )
            .expect("all four keys permit the scaffold simulator"),
            SimModel::S23
        );
    }

    #[test]
    fn plain_s19_and_s23_do_not_invent_chip_counts() {
        assert_eq!(
            SimBoardProfile::for_model(SimModel::S19).chips_per_chain,
            None
        );
        assert_eq!(
            SimBoardProfile::for_model(SimModel::S23).chips_per_chain,
            None
        );
    }

    #[test]
    fn plus_model_spelling_does_not_collapse_to_base_model() {
        assert_eq!(
            "s17+".parse::<SimModel>().expect("S17+ alias"),
            SimModel::S17Plus
        );
        assert_eq!(
            "t17+".parse::<SimModel>().expect("T17+ alias"),
            SimModel::T17Plus
        );
    }

    #[test]
    fn legacy_models_cannot_open_modern_backend_face() {
        let s9 = SimPlatform::new(SimModel::S9);
        assert!(s9.open_bm1397plus_backend(0).is_err());
        assert!(SimPlatform::new(SimModel::S19Pro)
            .open_bm1397plus_backend(0)
            .is_ok());
    }

    #[test]
    fn simulated_platform_opens_shared_i2c_without_device_nodes() {
        let platform = SimPlatform::new(SimModel::S19Pro);
        let mut bus = platform.open_i2c(0).expect("simulated I2C bus");
        bus.set_slave(0x20).expect("select dsPIC");
        let mut version = [0_u8; 5];
        bus.write_read(&[0x55, 0xAA, 0x17], &mut version)
            .expect("simulated GET_VERSION");
        assert_eq!(version[2], 0x89);

        bus.set_slave(0x50).expect("select protected EEPROM");
        assert!(bus.write(&[0]).is_err());
    }

    #[test]
    fn simulated_i2c_service_uses_production_handle_contract() {
        let platform = SimPlatform::new(SimModel::S19Pro);
        let service = platform.open_i2c_service(0).expect("sim I2C service");
        let version = service
            .write_read(0x20, &[0x55, 0xAA, 0x17], 5)
            .expect("service GET_VERSION");
        assert_eq!(version[2], 0x89);
        service
            .write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x01])
            .expect("service ENABLE");
        assert!(platform.i2c.voltage_enabled().expect("shared enable state"));
    }

    #[test]
    fn simulated_service_rejects_pic16_heartbeat_outside_application_mode() {
        let platform = SimPlatform::new(SimModel::S9);
        platform
            .configure_pic16_raw_state(0, 0x55, 0xCC)
            .expect("configure bootloader endpoint");
        let endpoint = platform
            .pic16_endpoint(0, 0x55)
            .expect("issue simulated endpoint");
        let service = platform.open_i2c_service(0).expect("sim I2C service");

        assert!(service.pic16_heartbeat(&endpoint).is_err());
        assert_eq!(
            platform
                .pic16_snapshot(0, 0x55)
                .expect("endpoint snapshot")
                .heartbeat_count(),
            0
        );
    }

    #[test]
    fn pic16_profiles_pin_typed_endpoints_and_ordered_watchdog_traces() {
        let s9 = SimPlatform::new(SimModel::S9);
        assert!(s9.pic16_endpoint(0, 0x55).is_ok());
        assert!(s9.pic16_endpoint(0, 0x50).is_err());

        for model in [SimModel::T17, SimModel::S17Plus, SimModel::T17Plus] {
            let platform = SimPlatform::new(model);
            assert!(platform.pic16_snapshot(0, 0x50).is_ok());
            assert!(platform.pic16_snapshot(0, 0x55).is_err());
            assert!(platform.pic16_endpoint(0, 0x50).is_err());
            assert!(platform.pic16_endpoint(0, 0x55).is_err());
        }
        for model in [SimModel::S11, SimModel::S15, SimModel::T15] {
            let platform = SimPlatform::new(model);
            assert!(platform.pic16_endpoint(0, 0x50).is_err());
            assert!(platform.pic16_endpoint(0, 0x55).is_err());
        }

        s9.configure_controller_watchdog(Duration::from_millis(1))
            .expect("configure S9 endpoint watchdogs");
        s9.advance_i2c_time(Duration::from_millis(1))
            .expect("expire S9 endpoint watchdogs");
        let addresses: Vec<u8> = s9
            .drain_i2c_trace()
            .expect("ordered expiry trace")
            .into_iter()
            .filter_map(|event| match event {
                TraceEvent::Pic16ControllerWatchdogExpired { addr, .. } => Some(addr),
                _ => None,
            })
            .collect();
        assert_eq!(addresses, vec![0x55, 0x56, 0x57]);
    }
}
