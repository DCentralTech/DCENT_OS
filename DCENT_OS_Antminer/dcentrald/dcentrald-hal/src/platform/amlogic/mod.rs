//! Amlogic A113D platform implementation.
//!
//! The Amlogic A113D (aarch64, quad Cortex-A53) is used in newer Antminer models:
//!   - S19 XP, S19k Pro (with BM1366, NoPic on the verified .78 am3-aml unit)
//!   - S21, S21 Pro, S21+, S21 XP (with BM1368/BM1370, NoPic)
//!   - T21 (with BM1368)
//!   - L9 (Scrypt, with BM1491)
//!
//! Key differences from Zynq:
//!   - NO FPGA — ASIC communication via standard Linux serial ports
//!   - /dev/ttyS1, /dev/ttyS2, /dev/ttyS4 for 3 hash chains (ttyS3 unused)
//!   - GPIO via sysfs or /dev/mem for plug detect and board reset
//!   - Fan control via sysfs hwmon PWM
//!   - I2C via standard /dev/i2c-N
//!   - Some models (S21) have NO PIC — voltage is frequency-controlled only
//!
//! GPIO mapping (verified on S21 at .135, 2026-04-11):
//!   - PLUG_DETECT: gpio439=CH0, gpio440=CH1, gpio441=CH2 (active HIGH, pulldown)
//!   - BOARD_RESET: gpio454=CH0, gpio455=CH1, gpio456=CH2 (active LOW = reset)
//!   - PSU_ENABLE:  gpio437 (active HIGH: 1=PSU ON, 0=OFF —  Q10, corrected 2026-05-21)
//!   - LED_RED:     gpio438, LED_GREEN: gpio453 (active HIGH)
//!   - FAN_TACH:    gpio447-450 (falling edge, 4 fans)
//!
//! Identification:
//!   - Has Micro USB port on faceplate
//!   - /dev/ttyS1 exists but /dev/uio0 does NOT
//!   - /sys/module/uart_trans does NOT exist (that's CVitek)
//!
//! # EEPROM write protection (W3.1, 2026-05-07)
//!
//! Hashboard EEPROMs on am3-aml (S21, S19j Pro Amlogic, S19K Pro) sit on
//! `/dev/i2c-0` at addresses 0x50..=0x57 (AT24C-class, one EEPROM per
//! hashboard slot). These store factory identity (model, serial, frequency
//! profile, defective-core map). Writes to this range are PROTECTED at the
//! HAL layer to prevent the .74 hb2-class corruption pattern that bricked
//! a unit on 2026-04-29 (post-PIC-RESET, the bus master scribbled bytes
//! into hb2's EEPROM along with the dsPIC fw=0x86 downgrade).
//!
//! The new BHB56902 hashboards on S19K Pro use a `0x05 0x11` EEPROM header
//! preamble (vs BHB42xxx-class `0x04 0x11` on am2 Zynq); both are protected
//! by the same write-deny because both store identity bytes that cannot be
//! reconstructed once corrupted.
//!
//! Reads at 0x50..=0x57 still work — only writes are blocked. This is
//! parity with the am2 hybrid path's `[0x50..=0x57]` denylist registered
//! by `s19j_hybrid_mining.rs`. S9 (am1-zynq) deliberately registers no
//! denylist because its 0x55-0x57 are PIC voltage controllers, not
//! EEPROMs — applying this list there would brick PIC writes.
//!
//! See: ,
//! ,
//! `dcentrald_api_types::EEPROM_WRITE_DENYLIST`.
//!
//! ## Sub-modules (W15.D, 2026-05-10)
//!
//! - [`vnish_state`] — Detects whether the live unit is running stock
//!   Bitmain `bmminer` or VNish `cgminer` userspace. Same A113D
//!   silicon, different firmware. NOT a 4th `Platform` enum variant
//!.
//! - [`vnish_cold_boot`] — Data-only port of the W4 VNish AML
//!   cold-boot phase machine + GPIO map (15 pins). Env-gated
//!   (`DCENT_AML_VNISH_ACCEPT_INFERRED=1`) and has no orchestrator
//!   wired in W15; the data lands first so a future bench-unit
//!   operator harness can reuse it.

pub mod vnish_cold_boot;
pub mod vnish_state;

use std::collections::BTreeMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::config::{
    amlogic_tty_candidate_order, probe_tty_chain_device, ChainTransport, PlatformConfig,
    VoltageControllerKind,
};
use super::{BoardType, ChainAccess, FanAccess, GpioAccess, Platform};
use crate::i2c::{
    spawn_i2c_service_no_register_touch_with_denylist,
    spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation, I2cBus,
    I2cMutationLabel, I2cServiceHandle, I2cTransactionStep, Lm75TemperatureRegister,
    TerminalSafeOffTransition,
};
use crate::serial::SerialChain;
use crate::{HalError, Result};

/// I²C bus that carries hashboard EEPROMs on am3-aml (S21, S19j Pro Amlogic,
/// S19K Pro). PSU lives on bus 1 at 0x1f and is intentionally NOT on the
/// denylist — bus 1 is owned separately by `AmlogicPowerThermalService`.
pub const AMLOGIC_HASHBOARD_EEPROM_BUS: u8 = 0;

/// AT24C-class hashboard EEPROM addresses on am3-aml `/dev/i2c-0`.
///
/// Same range as am2 (0x50..=0x57) because both platform families wire one
/// 24Cxx-style EEPROM per hashboard slot to the standard 0x50 base address.
/// Reads (board identity, serial, frequency profile) still work — only
/// writes are refused at the bus layer. See module-level doc for rationale.
pub const AMLOGIC_EEPROM_DENYLIST: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

/// Spawn the kernel-fd-only I²C service for `/dev/i2c-0` with the am3-aml
/// hashboard EEPROM write-deny range pre-registered.
///
/// This is the parity-with-am2 helper. The am2 hybrid path calls
/// `spawn_i2c_service_no_register_touch_with_denylist(0, 0x50..=0x57)`
/// directly; this helper exposes the same protection for am3-aml callers
/// (`daemon.rs::Daemon::run` and any future am3-aml mining path) without
/// re-stating the address range at every site.
///
/// Errors are propagated unchanged from the underlying spawn.
pub fn spawn_amlogic_protected_i2c0_service() -> std::io::Result<I2cServiceHandle> {
    let denylist: Vec<u8> = AMLOGIC_EEPROM_DENYLIST.to_vec();
    let handle =
        spawn_i2c_service_no_register_touch_with_denylist(AMLOGIC_HASHBOARD_EEPROM_BUS, denylist)?;
    tracing::info!(
        bus = AMLOGIC_HASHBOARD_EEPROM_BUS,
        denylist = ?AMLOGIC_EEPROM_DENYLIST
            .iter()
            .map(|a| format!("0x{:02X}", a))
            .collect::<Vec<_>>(),
        "am3-aml I2C service spawned with hashboard EEPROM write-deny (parity with am2 hybrid path)"
    );
    Ok(handle)
}

/// GPIO base for hash board plug detect: 439=CH0, 440=CH1, 441=CH2 (active HIGH).
/// Verified on live S21 at .135 (2026-04-11): CH1=1 (board present), CH0=CH2=0.
const GPIO_PLUG_BASE: u32 = 439;

/// GPIO base for hash board reset: 454=CH0, 455=CH1, 456=CH2 (active LOW = reset).
/// Verified on live S21 at .135: CH1_RST=1 (running), CH0=CH2=0.
const GPIO_RESET_BASE: u32 = 454;

/// GPIO base for fan tachometer inputs: 447, 448, 449, 450 (4 fans).
/// Each fan tach line generates a falling edge per pulse. Typical 4-pin
/// brushless DC fans emit 2 pulses per revolution (`PULSES_PER_REV`),
/// so RPM = falling_edges_per_second * 60 / 2 = falling_edges * 30
/// over a 1-second sample window. Verified GPIO map and
/// the AmlogicPlatform module header (S21 / S19j Pro Amlogic / S19K Pro
/// share this layout).
const GPIO_FAN_TACH_BASE: u32 = 447;
const GPIO_FAN_TACH_COUNT: usize = 4;

/// Pulses-per-revolution assumption for am3-aml fans. This matches the
/// industry-standard 4-pin BLDC fan spec and the bosminer/BraiinsOS
/// fan-tach divisor on the same hardware. If a future production fan
/// is sourced with a different ratio, this constant is the tunable.
const PULSES_PER_REV: u32 = 2;

/// Length of the falling-edge sample window. 1 second gives ~30 RPM
/// resolution at the 2 PPR ratio, which is well below the 300 RPM
/// "degraded" threshold and the 0 RPM FanFailure threshold. The
/// thermal controller ticks every 5 seconds, so a 1 s window leaves
/// 4 s slack for the rest of the loop.
const TACH_SAMPLE_MS: u64 = 1_000;

/// Amlogic A113D platform.
pub struct AmlogicPlatform {
    config: PlatformConfig,
}

/// File-backed Amlogic NoPic topology that may construct power, cooling, and
/// thermal owners. Miner configuration alone never grants this capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmlogicNoPicProfile {
    S19k,
    S21,
}

const AML_BOOT_SAFE_RECEIPT: &str = "/run/dcentos/amlogic-boot-safe-state-v1";
const AML_BOOT_SAFE_SCHEMA: &str = "dcentos.amlogic-safe-state/v1";
const AML_BOOT_SAFE_RESOURCE: &str = "amlogic-gpio437-power-gate";
const AML_BOOT_FAN_DUTY_NS: u32 = 30_000;
const AML_BOOT_FAN_PERIOD_NS: u32 = 100_000;

/// Command/readback evidence emitted by the early Amlogic Linux owner and
/// revalidated immediately before the supervisor publishes dcentrald.
///
/// This is deliberately not named an electrical receipt: GPIO sysfs readback
/// proves only controller software state. Hardware promotion still requires
/// scoped pad/rail qualification across reset and brownout.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AmlogicBootSafeHandoff {
    boot_id: String,
    platform: String,
    board_target: String,
    fan0_duty_ns: u32,
    fan0_period_ns: u32,
    fan1_duty_ns: u32,
    fan1_period_ns: u32,
}

fn parse_amlogic_boot_safe_handoff(source: &str) -> Result<AmlogicBootSafeHandoff> {
    let mut fields = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            HalError::Platform(format!(
                "Amlogic boot-safe receipt line {} lacks key=value framing",
                index + 1
            ))
        })?;
        if key.is_empty() || value.is_empty() {
            return Err(HalError::Platform(format!(
                "Amlogic boot-safe receipt line {} has an empty key or value",
                index + 1
            )));
        }
        if fields.insert(key, value).is_some() {
            return Err(HalError::Platform(format!(
                "Amlogic boot-safe receipt repeats field {key:?}"
            )));
        }
    }

    const REQUIRED: &[&str] = &[
        "schema",
        "state",
        "boot_id",
        "platform",
        "board_target",
        "resource",
        "gpio_direction",
        "gpio_active_low",
        "commanded_value",
        "readback_value",
        "fan0_duty_ns",
        "fan0_period_ns",
        "fan0_enabled",
        "fan1_duty_ns",
        "fan1_period_ns",
        "fan1_enabled",
        "evidence_grade",
        "physical_rail_measured",
    ];
    if fields.len() != REQUIRED.len() || REQUIRED.iter().any(|key| !fields.contains_key(key)) {
        return Err(HalError::Platform(
            "Amlogic boot-safe receipt fields do not match schema v1".into(),
        ));
    }
    let require = |key: &str, expected: &str| -> Result<()> {
        if fields.get(key).copied() == Some(expected) {
            Ok(())
        } else {
            Err(HalError::Platform(format!(
                "Amlogic boot-safe receipt {key} is {:?}, expected {expected:?}",
                fields.get(key)
            )))
        }
    };
    require("schema", AML_BOOT_SAFE_SCHEMA)?;
    require("state", "runtime-handoff")?;
    require("resource", AML_BOOT_SAFE_RESOURCE)?;
    require("gpio_direction", "out")?;
    require("gpio_active_low", "0")?;
    require("commanded_value", "0")?;
    require("readback_value", "0")?;
    require("fan0_enabled", "1")?;
    require("fan1_enabled", "1")?;
    require("evidence_grade", "software-readback")?;
    require("physical_rail_measured", "false")?;

    let parse_duty = |key: &str| -> Result<u32> {
        fields[key].parse::<u32>().map_err(|error| {
            HalError::Platform(format!(
                "Amlogic boot-safe receipt {key} is not a duty in ns: {error}"
            ))
        })
    };
    let receipt = AmlogicBootSafeHandoff {
        boot_id: fields["boot_id"].to_owned(),
        platform: fields["platform"].to_owned(),
        board_target: fields["board_target"].to_owned(),
        fan0_duty_ns: parse_duty("fan0_duty_ns")?,
        fan0_period_ns: parse_duty("fan0_period_ns")?,
        fan1_duty_ns: parse_duty("fan1_duty_ns")?,
        fan1_period_ns: parse_duty("fan1_period_ns")?,
    };
    if receipt.fan0_duty_ns != AML_BOOT_FAN_DUTY_NS || receipt.fan1_duty_ns != AML_BOOT_FAN_DUTY_NS
    {
        return Err(HalError::Platform(format!(
            "Amlogic boot-safe receipt fan duties are {}/{}, expected {AML_BOOT_FAN_DUTY_NS}",
            receipt.fan0_duty_ns, receipt.fan1_duty_ns
        )));
    }
    if receipt.fan0_period_ns != AML_BOOT_FAN_PERIOD_NS
        || receipt.fan1_period_ns != AML_BOOT_FAN_PERIOD_NS
    {
        return Err(HalError::Platform(format!(
            "Amlogic boot-safe receipt fan periods are {}/{}, expected {AML_BOOT_FAN_PERIOD_NS}",
            receipt.fan0_period_ns, receipt.fan1_period_ns
        )));
    }
    Ok(receipt)
}

fn amlogic_handoff_identity_matches_profile(
    expected: AmlogicNoPicProfile,
    platform: &str,
    board_target: &str,
) -> bool {
    match expected {
        AmlogicNoPicProfile::S19k => platform == "am3-aml-s19k" && board_target == "am3-s19k",
        AmlogicNoPicProfile::S21 => matches!(
            (platform, board_target),
            ("am3-aml-s21", "am3-s21")
                | ("am3-aml-s21pro", "am3-s21pro")
                | ("am3-aml-s21xp", "am3-s21xp")
                | ("am3-aml-t21", "am3-t21")
        ),
    }
}

fn validate_amlogic_boot_safe_handoff(expected: AmlogicNoPicProfile) -> Result<()> {
    let metadata = fs::symlink_metadata(AML_BOOT_SAFE_RECEIPT).map_err(|error| {
        HalError::Platform(format!(
            "Amlogic NoPic admission requires boot-safe handoff {AML_BOOT_SAFE_RECEIPT}: {error}"
        ))
    })?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(HalError::Platform(format!(
            "Amlogic boot-safe handoff must be a root-owned regular 0600 file: {AML_BOOT_SAFE_RECEIPT}"
        )));
    }
    let source = fs::read_to_string(AML_BOOT_SAFE_RECEIPT).map_err(|error| {
        HalError::Platform(format!("Amlogic boot-safe handoff read failed: {error}"))
    })?;
    let receipt = parse_amlogic_boot_safe_handoff(&source)?;

    let read_marker = |path: &str| -> Result<String> {
        fs::read_to_string(path)
            .map(|value| value.trim().to_owned())
            .map_err(|error| {
                HalError::Platform(format!("Amlogic marker {path} read failed: {error}"))
            })
    };
    let boot_id = read_marker("/proc/sys/kernel/random/boot_id")?;
    let platform = read_marker("/etc/dcentos/platform")?;
    let board_target = read_marker("/etc/dcentos/board_target")?;
    let rail_gpio = read_marker("/etc/dcentos/rail_gpio")?;
    if rail_gpio != GPIO_PSU_ENABLE.to_string() {
        return Err(HalError::Platform(format!(
            "Amlogic rail marker {rail_gpio:?} does not bind GPIO{GPIO_PSU_ENABLE}"
        )));
    }
    let identity_matches_profile =
        amlogic_handoff_identity_matches_profile(expected, &platform, &board_target);
    if !identity_matches_profile
        || receipt.boot_id != boot_id
        || receipt.platform != platform
        || receipt.board_target != board_target
    {
        return Err(HalError::Platform(format!(
            "Amlogic boot-safe handoff identity/profile mismatch: receipt={receipt:?} live_platform={platform:?} live_target={board_target:?}"
        )));
    }

    let read_live = |path: &str| -> Result<String> {
        fs::read_to_string(path)
            .map(|value| value.trim().to_owned())
            .map_err(|error| {
                HalError::Platform(format!("Amlogic handoff revalidation {path}: {error}"))
            })
    };
    let direction = read_live("/sys/class/gpio/gpio437/direction")?;
    let value = read_live("/sys/class/gpio/gpio437/value")?;
    let active_low = read_live("/sys/class/gpio/gpio437/active_low")?;
    let fan0 = read_live("/sys/class/pwm/pwmchip0/pwm0/duty_cycle")?;
    let fan0_period = read_live("/sys/class/pwm/pwmchip0/pwm0/period")?;
    let fan0_enabled = read_live("/sys/class/pwm/pwmchip0/pwm0/enable")?;
    let fan1 = read_live("/sys/class/pwm/pwmchip0/pwm1/duty_cycle")?;
    let fan1_period = read_live("/sys/class/pwm/pwmchip0/pwm1/period")?;
    let fan1_enabled = read_live("/sys/class/pwm/pwmchip0/pwm1/enable")?;
    if direction != "out"
        || value != "0"
        || active_low != "0"
        || fan0 != AML_BOOT_FAN_DUTY_NS.to_string()
        || fan0_period != AML_BOOT_FAN_PERIOD_NS.to_string()
        || fan0_enabled != "1"
        || fan1 != AML_BOOT_FAN_DUTY_NS.to_string()
        || fan1_period != AML_BOOT_FAN_PERIOD_NS.to_string()
        || fan1_enabled != "1"
    {
        return Err(HalError::Platform(format!(
            "Amlogic boot-safe live revalidation failed: direction={direction:?} value={value:?} active_low={active_low:?} fan0={fan0:?}/{fan0_period:?}/{fan0_enabled:?} fan1={fan1:?}/{fan1_period:?}/{fan1_enabled:?}"
        )));
    }
    Ok(())
}

impl AmlogicNoPicProfile {
    fn label(self) -> &'static str {
        match self {
            Self::S19k => "S19K/S19 XP Amlogic NoPic",
            Self::S21 => "S21/T21 Amlogic NoPic",
        }
    }
}

/// Opaque admission proof for one physical Amlogic chain slot.
#[derive(Debug)]
pub struct AmlogicNoPicAdmission {
    profile: AmlogicNoPicProfile,
    active_slot: u8,
    populated_slots: [bool; 3],
}

impl AmlogicNoPicAdmission {
    /// Detect the control-board profile independently of mining configuration,
    /// bind it to the configured chain UART, and refuse every mismatch before
    /// GPIO, PWM, or management-I2C mutation.
    pub fn detect(expected: AmlogicNoPicProfile, serial_device: &str) -> Result<Self> {
        let active_slot = amlogic_slot_from_serial_device(serial_device).ok_or_else(|| {
            HalError::Platform(format!(
                "Amlogic NoPic admission does not recognize chain UART {serial_device:?}"
            ))
        })?;
        if !Path::new(serial_device).exists() {
            return Err(HalError::Platform(format!(
                "Amlogic NoPic admission requires the selected chain UART to exist: {serial_device}"
            )));
        }
        let observed = detect_amlogic_nopic_profile()?;
        validate_amlogic_boot_safe_handoff(expected)?;
        let populated_slots = read_plug_topology_checked()?;
        Self::from_profile_evidence(expected, active_slot, observed, populated_slots)
    }

    fn from_profile_evidence(
        expected: AmlogicNoPicProfile,
        active_slot: u8,
        observed: AmlogicNoPicProfile,
        populated_slots: [bool; 3],
    ) -> Result<Self> {
        if observed != expected {
            return Err(HalError::Platform(format!(
                "Amlogic NoPic profile mismatch: miner requires {}, control-board identity resolved to {}",
                expected.label(),
                observed.label()
            )));
        }
        if active_slot > 2 {
            return Err(HalError::Platform(format!(
                "Amlogic NoPic active slot {active_slot} is outside the verified three-slot topology"
            )));
        }
        if !populated_slots[active_slot as usize] {
            return Err(HalError::Platform(format!(
                "Amlogic NoPic UART slot {active_slot} is not asserted by checked plug-detect topology {populated_slots:?}"
            )));
        }
        Ok(Self {
            profile: observed,
            active_slot,
            populated_slots,
        })
    }

    pub fn profile(&self) -> AmlogicNoPicProfile {
        self.profile
    }

    pub fn active_slot(&self) -> u8 {
        self.active_slot
    }

    pub fn populated_slots(&self) -> [bool; 3] {
        self.populated_slots
    }

    /// Establish the sole retained owner of management bus 1.
    pub fn spawn_power_thermal_service(&self) -> Result<AmlogicPowerThermalService> {
        AmlogicPowerThermalService::spawn(self)
    }

    /// Open only the admitted Amlogic cooling controller. The returned shared
    /// owner can be moved to Tokio's blocking pool without reopening sysfs.
    pub fn open_fan_controller(&self) -> Result<Arc<dyn FanAccess>> {
        Ok(Arc::new(AmlogicFan::new()?))
    }
}

fn amlogic_slot_from_serial_device(serial_device: &str) -> Option<u8> {
    match serial_device {
        "/dev/ttyS1" => Some(0),
        "/dev/ttyS2" => Some(1),
        "/dev/ttyS3" | "/dev/ttyS4" => Some(2),
        _ => None,
    }
}

fn read_plug_topology_checked() -> Result<[bool; 3]> {
    let mut populated = [false; 3];
    for slot in 0..3u32 {
        let gpio = GPIO_PLUG_BASE + slot;
        let root = format!("/sys/class/gpio/gpio{gpio}");
        let value_path = format!("{root}/value");
        if !Path::new(&value_path).exists() {
            let export_result = fs::write("/sys/class/gpio/export", gpio.to_string());
            let deadline = Instant::now() + Duration::from_millis(500);
            while !Path::new(&value_path).exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !Path::new(&value_path).exists() {
                return Err(HalError::Platform(format!(
                    "Amlogic plug-detect GPIO{gpio} did not appear after export: {export_result:?}"
                )));
            }
        }
        let direction_path = format!("{root}/direction");
        fs::write(&direction_path, "in").map_err(|error| {
            HalError::Platform(format!(
                "Amlogic plug-detect GPIO{gpio} direction input failed: {error}"
            ))
        })?;
        let direction = fs::read_to_string(&direction_path).map_err(|error| {
            HalError::Platform(format!(
                "Amlogic plug-detect GPIO{gpio} direction readback failed: {error}"
            ))
        })?;
        if direction.trim() != "in" {
            return Err(HalError::Platform(format!(
                "Amlogic plug-detect GPIO{gpio} direction readback was {:?}, expected input",
                direction.trim()
            )));
        }
        let value = fs::read_to_string(&value_path).map_err(|error| {
            HalError::Platform(format!(
                "Amlogic plug-detect GPIO{gpio} read failed: {error}"
            ))
        })?;
        populated[slot as usize] = match value.trim() {
            "0" => false,
            "1" => true,
            other => {
                return Err(HalError::Platform(format!(
                    "Amlogic plug-detect GPIO{gpio} returned invalid value {other:?}"
                )))
            }
        };
    }
    Ok(populated)
}

impl AmlogicPlatform {
    /// Create a new Amlogic platform.
    ///
    /// Resolve enough file-backed identity to emit a precise refusal. The
    /// generic `Platform` lifecycle cannot carry the retained bus-1 owner, so
    /// production Amlogic mining must use the native serial engine.
    pub fn new() -> Result<Self> {
        // Verify we're actually on Amlogic
        if !["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4"]
            .iter()
            .any(|path| std::path::Path::new(path).exists())
        {
            return Err(HalError::Platform(
                "Amlogic: no ttyS1/ttyS2/ttyS4 device found".to_string(),
            ));
        }

        // Detect specific model
        let config = detect_amlogic_model()?;

        Err(HalError::Platform(format!(
            "{} requires the native serial-mining lifecycle with retained Amlogic power/thermal ownership; generic Platform construction is refused",
            config.name
        )))
    }

    /// Create with explicit config for host-only trait tests.
    #[cfg(test)]
    pub fn with_config(config: PlatformConfig) -> Self {
        Self { config }
    }
}

impl Platform for AmlogicPlatform {
    fn board_type(&self) -> BoardType {
        BoardType::Amlogic
    }

    fn chain_count(&self) -> u8 {
        self.config.chains.len() as u8
    }

    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>> {
        let chain_config = self
            .config
            .chains
            .iter()
            .find(|c| c.chain_id == chain_id)
            .ok_or_else(|| HalError::Platform(format!("chain {} not configured", chain_id)))?;

        match &chain_config.transport {
            ChainTransport::Serial { device, baud } => {
                // Chain-2 has historical ttyS3/ttyS4 drift. The priority order
                // is explicit config data with host tests; runtime probing only
                // resolves which declared candidate the live filesystem exposes.
                let resolved_device = if chain_id == 2 {
                    let label = format!("amlogic-chain-{}", chain_id);
                    let candidates = amlogic_tty_candidate_order(chain_config);
                    let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
                    probe_tty_chain_device(&candidate_refs, &label)
                        .unwrap_or_else(|| device.clone())
                } else {
                    device.clone()
                };
                let serial = SerialChain::open(&resolved_device, *baud)?;
                Ok(Box::new(AmlogicChainAccess {
                    serial: Mutex::new(serial),
                }))
            }
            other => Err(HalError::Platform(format!(
                "unexpected transport for Amlogic chain {}: {:?}",
                chain_id, other
            ))),
        }
    }

    fn open_i2c(&self, bus: u8) -> Result<I2cBus> {
        let mut handle = I2cBus::open(bus)?;
        // Defense-in-depth: any caller that obtains a raw `I2cBus` for the
        // hashboard EEPROM bus on am3-aml inherits the [0x50..=0x57]
        // write-deny. Production code should go through
        // `spawn_amlogic_protected_i2c0_service()` (the long-running
        // serialized service); this gate covers transient one-off opens.
        // See module-level doc for the BHB42xxx/BHB56902 EEPROM rationale
        // and .
        if bus == AMLOGIC_HASHBOARD_EEPROM_BUS {
            handle.set_write_denylist(&AMLOGIC_EEPROM_DENYLIST);
        }
        Ok(handle)
    }

    fn open_fan(&self) -> Result<Box<dyn FanAccess>> {
        Err(HalError::Platform(
            "generic Amlogic fan construction is refused; native mining must consume AmlogicNoPicAdmission"
                .to_string(),
        ))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        Ok(Box::new(AmlogicGpio))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        // Production generic construction is refused above. Host-only trait
        // tests preserve exactly the explicit PlatformConfig classification;
        // the native serial lifecycle resolves controller authority itself.
        self.config.voltage_controller
    }
}

/// Serial-based chain access for Amlogic platforms.
///
/// Unlike Zynq FpgaChain which has separate FIFOs for cmd/work/nonce,
/// Amlogic multiplexes everything on a single serial port. The ASIC
/// protocol preamble (0x55 0xAA vs 0xAA 0x55) distinguishes directions.
///
/// BUG FIX (2026-04-11): Was UnsafeCell with manual Send/Sync — replaced
/// with Mutex to prevent data races on concurrent UART access.
struct AmlogicChainAccess {
    serial: Mutex<SerialChain>,
}

impl ChainAccess for AmlogicChainAccess {
    fn send_command(&self, data: &[u8]) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.write_bytes(data)
    }

    fn read_response(&self, buf: &mut [u8]) -> Result<usize> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.read_bytes(buf)
    }

    fn send_work(&self, data: &[u8]) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.write_bytes(data)
    }

    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.read_bytes(buf)
    }

    fn set_baud(&self, baud: u32) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.set_baud(baud)
    }

    fn wait_for_nonce(&self) -> Result<()> {
        // Serial port uses VTIME timeout — read_bytes will block briefly.
        // For production, this should use epoll/select.
        std::thread::yield_now();
        Ok(())
    }
}

/// Source of falling-edge counts for one fan tach line. Abstracted so
/// unit tests can mock the edge stream without real GPIO hardware.
pub(crate) trait FanTachSource: Send + Sync {
    /// Sample falling edges over the given window and return the count. Any
    /// kernel/poll/read failure invalidates the complete sample; partial counts
    /// must never be promoted to cooling evidence.
    /// Must be a non-blocking-style budget: implementations should bound
    /// the call to `window` regardless of whether edges arrive.
    fn sample_falling_edges(&self, window: Duration) -> std::io::Result<u32>;
}

/// Sysfs-backed falling-edge counter for `/sys/class/gpio/gpioN/value`.
///
/// Why sysfs and not the chardev `/dev/gpiochip*` API? Per
///  the am2 Zynq ships sysfs-only;
/// the am3-aml kernel CAN expose chardev but bosminer/BraiinsOS still
/// drives gpio447-450 through sysfs, so sysfs is the proven-on-live-
/// hardware path. Sysfs also keeps the dependency footprint zero.
///
/// How falling-edge sampling works on sysfs:
/// 1. Export the GPIO, set `direction=in`, set `edge=falling`.
/// 2. `open()` `/sys/class/gpio/gpioN/value` once and cache the fd.
/// 3. To sample, do an initial `read()` (clears any latched event),
///    then `poll(POLLPRI|POLLERR)` with the sample window as the
///    timeout. Each `poll()` return that signals `POLLPRI` is one
///    falling edge. After each event re-`lseek(0)` + `read()` to
///    re-arm the kernel-side latch and continue counting until the
///    deadline passes.
///
/// This is the mechanism the kernel documents in
/// `Documentation/gpio/sysfs.txt` and is what BraiinsOS's
/// `monitor-ipsig` GPIO reader uses on the same hardware.
struct SysfsFallingEdgeCounter {
    gpio: u32,
    fd: std::fs::File,
}

impl SysfsFallingEdgeCounter {
    fn export(gpio: u32) -> Result<Self> {
        let dir = format!("/sys/class/gpio/gpio{}", gpio);
        if !Path::new(&dir).exists() {
            // Best-effort export. If kernel returns EBUSY because another
            // process exported the line, the direction/edge writes below
            // will still succeed because sysfs allows multiple writers.
            if let Err(e) = fs::write("/sys/class/gpio/export", gpio.to_string()) {
                tracing::debug!(gpio, error = %e, "GPIO tach export returned error (may already be exported)");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let dir_path = format!("{}/direction", dir);
        if let Err(e) = fs::write(&dir_path, "in") {
            return Err(HalError::Fan(format!(
                "fan tach gpio{} direction=in failed: {}",
                gpio, e
            )));
        }

        let edge_path = format!("{}/edge", dir);
        if let Err(e) = fs::write(&edge_path, "falling") {
            return Err(HalError::Fan(format!(
                "fan tach gpio{} edge=falling failed (kernel may lack edge support): {}",
                gpio, e
            )));
        }

        let value_path = format!("{}/value", dir);
        let fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&value_path)
            .map_err(|e| {
                HalError::Fan(format!("fan tach gpio{} open value failed: {}", gpio, e))
            })?;

        // Drain any latched event so the first poll() represents the
        // first edge inside the sample window.
        drain_value_fd(fd.as_raw_fd()).map_err(|e| {
            HalError::Fan(format!(
                "fan tach gpio{} initial value read failed: {}",
                gpio, e
            ))
        })?;

        Ok(Self { gpio, fd })
    }
}

/// Read the sysfs "value" file from offset 0. POLLPRI is edge-triggered
/// in the sense that sysfs latches one event until the value is read; a
/// fresh read primes the next event.
fn drain_value_fd(fd: i32) -> std::io::Result<()> {
    use std::io;
    let mut buf = [0u8; 8];
    unsafe {
        if libc::lseek(fd, 0, libc::SEEK_SET) < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Classify one sysfs GPIO poll result. Linux kernfs reports a valid sysfs
/// attribute notification as `POLLPRI|POLLERR`, so `POLLPRI` remains
/// authoritative even when `POLLERR` accompanies it. Bare `POLLERR`, hangup,
/// and invalid-descriptor events are evidence failures.
fn tach_poll_has_edge(revents: libc::c_short) -> std::io::Result<bool> {
    use std::io;
    if revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("fan tach poll reported error revents=0x{revents:04X}"),
        ));
    }
    if revents & libc::POLLPRI != 0 {
        return Ok(true);
    }
    if revents & libc::POLLERR != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("fan tach poll reported bare POLLERR revents=0x{revents:04X}"),
        ));
    }
    Ok(false)
}

impl FanTachSource for SysfsFallingEdgeCounter {
    fn sample_falling_edges(&self, window: Duration) -> std::io::Result<u32> {
        let raw_fd = self.fd.as_raw_fd();
        let deadline = Instant::now() + window;
        let mut edges: u32 = 0;

        // Hard cap: even if the kernel storms us with bogus events
        // (e.g. floating tach line), don't loop forever.
        const MAX_EDGES_PER_WINDOW: u32 = 100_000;

        // Make sure we start from a known-clean state.
        drain_value_fd(raw_fd)?;

        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(r) if !r.is_zero() => r,
                _ => break,
            };

            let mut pollfd = libc::pollfd {
                fd: raw_fd,
                events: libc::POLLPRI | libc::POLLERR,
                revents: 0,
            };

            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            if rc == 0 {
                // Timeout — window expired, no further edges.
                break;
            }

            if tach_poll_has_edge(pollfd.revents)? {
                edges = edges.saturating_add(1);
                if edges >= MAX_EDGES_PER_WINDOW {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "fan tach gpio{} exceeded maximum credible edges per window ({MAX_EDGES_PER_WINDOW})",
                            self.gpio
                        ),
                    ));
                }
                // Re-arm the latch so the next falling edge fires POLLPRI again.
                drain_value_fd(raw_fd)?;
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "fan tach gpio{} poll woke without POLLPRI (revents=0x{:04X})",
                        self.gpio, pollfd.revents
                    ),
                ));
            }
        }

        Ok(edges)
    }
}

/// Convert a falling-edge count over `window` into an RPM value at
/// `PULSES_PER_REV` pulses per revolution. Pulled out for unit tests.
fn edges_to_rpm(edges: u32, window: Duration) -> u32 {
    if window.is_zero() || PULSES_PER_REV == 0 {
        return 0;
    }
    let window_secs = window.as_secs_f64();
    if window_secs <= 0.0 {
        return 0;
    }
    let rpm = (edges as f64) * 60.0 / (window_secs * PULSES_PER_REV as f64);
    rpm.round().clamp(0.0, u32::MAX as f64) as u32
}

/// Amlogic fan control via sysfs PWM (pwmchip0/pwm0 and pwm1).
///
/// Verified on live S21 at .135 (2026-04-11):
///   - pwmchip0/pwm0: rear fans (FAN2, FAN4), period=100000ns (10kHz)
///   - pwmchip0/pwm1: front fans (FAN1, FAN3), period=100000ns (10kHz)
///   - duty_cycle range: 0 (off) to 100000 (100%)
///   - pwmchip0 at FF802000 (AO PWM), pwmchip4 at FFD1B000 (EE PWM)
///
/// Tach uses falling-edge counters on gpio447-450. Production construction is
/// atomic at the capability boundary: all four channels must arm before a fan
/// controller is returned. A zero-edge sample remains zero RPM. This matches
/// the held S21 Pro `single_board_test` binary, whose `fan_get_realtime_speed`
/// returns zero for a zero FPGA tach byte and whose `fan_speed_check` treats
/// low/zero RPM as a protection failure instead of inventing a positive value.
struct AmlogicFan {
    /// Rear fans duty_cycle path
    rear_duty_path: String,
    /// Front fans duty_cycle path
    front_duty_path: String,
    /// PWM period in nanoseconds (10kHz = 100000ns)
    period_ns: u32,
    /// Complete falling-edge counter set for fan slots 0..3. The fixed-size
    /// representation prevents a partially observable cooling controller from
    /// escaping production acquisition.
    tach_sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT],
    /// Monotonic health latch. Acquisition starts healthy; any typed sample or
    /// worker-creation error permanently revokes tach availability for this
    /// owner. In unwind builds a worker panic is also caught and revoked. The
    /// production abort profile instead delegates programmer-fault containment
    /// to the daemon crash-safe state and watchdog because destructors do not
    /// run after an abort.
    tach_healthy: AtomicBool,
    /// Sample window passed to each `FanTachSource::sample_falling_edges`.
    /// Owned by the struct so tests can shorten it.
    sample_window: Duration,
}

/// PWM period for Amlogic fans: 100000ns = 10kHz (confirmed on S21 probe)
const AMLOGIC_PWM_PERIOD_NS: u32 = 100_000;

/// Minimum commanded duty while an air-cooled Amlogic miner owns hash power.
/// Current shipped S21/S21 Pro profiles use 10%; lower config values are idle
/// policy, not proof that an energized chassis can safely stop its fans. The
/// pre-energization motion gate remains authoritative if a particular fan does
/// not move at this command.
pub const REQUIRED_AIRFLOW_MIN_PWM: u8 = 10;

/// Conservative minimum credible tach reading for an energized air-cooled
/// Amlogic chassis. This is an admission floor, not a target fan curve: one
/// stray edge in the one-second sample window (~30 RPM at 2 PPR) must not prove
/// cooling motion. Future board descriptions can replace this platform default
/// with PWM-dependent calibrated capability data.
pub const REQUIRED_AIRFLOW_MIN_RPM: u32 = 300;

/// Convert PWM percent (0-100) to nanosecond duty cycle for sysfs PWM.
/// Shared by Amlogic + BeagleBone fan paths so the kernel-write conversion
/// matches between platforms (and matches the userspace S82dcentrald
/// crash-fan override at PWM 30 = 30000ns / 100000ns period).
pub(super) fn amlogic_pwm_percent_to_duty_ns(pwm: u8, period_ns: u32) -> u32 {
    let pwm_clamped = pwm.min(100) as u32;
    (pwm_clamped * period_ns) / 100
}

/// Inverse of [`amlogic_pwm_percent_to_duty_ns`]. Used for read-back so
/// `set_speed(30); get_speed_pwm() == 30` (modulo integer-division rounding).
pub(super) fn amlogic_duty_ns_to_pwm_percent(duty_ns: u32, period_ns: u32) -> u8 {
    if period_ns == 0 {
        return 0;
    }
    (((duty_ns.min(period_ns) * 100) / period_ns) as u8).min(100)
}

impl AmlogicFan {
    fn new() -> Result<Self> {
        // Use sysfs PWM directly (confirmed on S21 at .135)
        let base = "/sys/class/pwm/pwmchip0";
        let rear_path = format!("{}/pwm0/duty_cycle", base);
        let front_path = format!("{}/pwm1/duty_cycle", base);

        for (label, path) in [("rear", &rear_path), ("front", &front_path)] {
            if !Path::new(path).exists() {
                return Err(HalError::Fan(format!(
                    "Amlogic {} fan PWM path not found: {}",
                    label, path
                )));
            }
        }

        // Bring up gpio447-450 as one complete cooling-observation capability.
        // Mining must not be admitted with a controller that can observe only a
        // subset of the fans.
        let mut tach_sources: Vec<Box<dyn FanTachSource>> = Vec::with_capacity(GPIO_FAN_TACH_COUNT);
        for slot in 0..GPIO_FAN_TACH_COUNT {
            let gpio = GPIO_FAN_TACH_BASE + slot as u32;
            let counter = SysfsFallingEdgeCounter::export(gpio).map_err(|e| {
                HalError::Fan(format!(
                    "Amlogic fan tach capability incomplete: slot {} gpio{} failed: {}",
                    slot, gpio, e
                ))
            })?;
            tach_sources.push(Box::new(counter));
        }

        let tach_sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] =
            tach_sources
                .try_into()
                .map_err(|sources: Vec<Box<dyn FanTachSource>>| {
                    HalError::Fan(format!(
                        "Amlogic fan tach capability incomplete: armed {} of {} channels",
                        sources.len(),
                        GPIO_FAN_TACH_COUNT
                    ))
                })?;
        tracing::info!(
            tach_count = GPIO_FAN_TACH_COUNT,
            "Amlogic fan tach: complete GPIO falling-edge counter set armed"
        );

        Ok(Self {
            rear_duty_path: rear_path,
            front_duty_path: front_path,
            period_ns: AMLOGIC_PWM_PERIOD_NS,
            tach_sources,
            tach_healthy: AtomicBool::new(true),
            sample_window: Duration::from_millis(TACH_SAMPLE_MS),
        })
    }

    /// Test-only constructor that injects mock tach sources. Lets unit
    /// tests assert RPM math without poking real GPIO sysfs.
    #[cfg(test)]
    fn for_test(tach_sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT]) -> Self {
        Self {
            rear_duty_path: String::new(),
            front_duty_path: String::new(),
            period_ns: AMLOGIC_PWM_PERIOD_NS,
            tach_sources,
            tach_healthy: AtomicBool::new(true),
            sample_window: Duration::from_millis(10),
        }
    }

    /// Sample one fan slot. Zero edges deliberately remain zero RPM: callers
    /// must never confuse absence of cooling evidence with evidence of motion.
    fn sample_slot_rpm(&self, slot: usize) -> u32 {
        let Some(source) = self.tach_sources.get(slot) else {
            self.tach_healthy.store(false, Ordering::Release);
            return 0;
        };
        match source.sample_falling_edges(self.sample_window) {
            Ok(edges) => edges_to_rpm(edges, self.sample_window),
            Err(error) => {
                self.tach_healthy.store(false, Ordering::Release);
                tracing::error!(slot, %error, "Amlogic fan tach sample invalidated");
                0
            }
        }
    }

    /// Observe every tach input in one shared wall-clock window. Sampling the
    /// four one-second counters sequentially used to stall the serial miner for
    /// roughly four seconds and a later telemetry read repeated the stall.
    fn sample_all_rpm(&self) -> Vec<(u8, u32)> {
        std::thread::scope(|scope| {
            let mut workers = Vec::with_capacity(self.tach_sources.len());
            for slot in 0..self.tach_sources.len() {
                let worker = std::thread::Builder::new()
                    .name(format!("amlogic-fan-tach-{slot}"))
                    .spawn_scoped(scope, move || self.sample_slot_rpm(slot));
                match worker {
                    Ok(worker) => workers.push((slot, Some(worker))),
                    Err(error) => {
                        self.tach_healthy.store(false, Ordering::Release);
                        tracing::error!(
                            slot,
                            %error,
                            "Amlogic fan tach sampler thread could not start; reporting zero RPM"
                        );
                        workers.push((slot, None));
                    }
                }
            }
            workers
                .into_iter()
                .map(|(slot, worker)| {
                    let rpm = worker.map_or(0, |worker| {
                        worker.join().unwrap_or_else(|_| {
                            self.tach_healthy.store(false, Ordering::Release);
                            tracing::error!(
                                slot,
                                "Amlogic fan tach sampler panicked in an unwind build; reporting zero RPM"
                            );
                            0
                        })
                    });
                    (slot as u8, rpm)
                })
                .collect()
        })
    }
}

impl FanAccess for AmlogicFan {
    fn set_speed(&self, pwm: u8) {
        // Scale 0-100 (BraiinsOS / `dcentrald-thermal::FAN_PWM_MAX` convention)
        // to 0-period_ns sysfs duty_cycle. Profile values like
        // `home_quiet.fan_max_pwm = 30` mean 30% duty cycle.
        //
        // Pre-2026-04-29: this used /127 (legacy from a different platform's
        // PWM range), which silently rendered profile PWM 30 as ~24% duty.
        // Bosminer fan-autoconfigure on .78 falls back to 25% as the spinning
        // floor — at /127 we'd never reach that floor for home-mining
        // configs. Per Phase H.6 expert-agent finding (Thermal+Perf).
        let duty_ns = amlogic_pwm_percent_to_duty_ns(pwm, self.period_ns);
        let duty_str = duty_ns.to_string();
        // Set both front and rear fans to the same speed
        if let Err(e) = fs::write(&self.rear_duty_path, &duty_str) {
            tracing::error!(path = %self.rear_duty_path, error = %e, "Failed to write rear fan PWM");
        }
        if let Err(e) = fs::write(&self.front_duty_path, &duty_str) {
            tracing::error!(path = %self.front_duty_path, error = %e, "Failed to write front fan PWM");
        }
    }

    fn set_speed_checked(&self, pwm: u8) -> Result<super::FanCommandReceipt> {
        let duty_ns = amlogic_pwm_percent_to_duty_ns(pwm, self.period_ns);
        let duty = duty_ns.to_string();
        fs::write(&self.rear_duty_path, &duty)
            .map_err(|error| HalError::Fan(format!("rear fan PWM write: {error}")))?;
        fs::write(&self.front_duty_path, &duty)
            .map_err(|error| HalError::Fan(format!("front fan PWM write: {error}")))?;

        for (label, path) in [
            ("rear", &self.rear_duty_path),
            ("front", &self.front_duty_path),
        ] {
            let observed = fs::read_to_string(path)
                .map_err(|error| HalError::Fan(format!("{label} fan PWM readback: {error}")))?;
            let observed = observed.trim().parse::<u32>().map_err(|error| {
                HalError::Fan(format!("{label} fan PWM readback parse: {error}"))
            })?;
            if observed != duty_ns {
                return Err(HalError::Fan(format!(
                    "{label} fan PWM readback mismatch: requested {duty_ns}ns, observed {observed}ns"
                )));
            }
        }

        Ok(super::FanCommandReceipt {
            requested_pwm: pwm,
            observed_pwm: pwm,
        })
    }

    fn get_rpm(&self) -> u32 {
        // Return the slowest RPM including zero. One stopped fan must dominate
        // the aggregate used by the thermal protection state machine.
        self.sample_all_rpm()
            .into_iter()
            .map(|(_, rpm)| rpm)
            .min()
            .unwrap_or(0)
    }

    fn get_speed_pwm(&self) -> u8 {
        fs::read_to_string(&self.rear_duty_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|duty_ns| amlogic_duty_ns_to_pwm_percent(duty_ns, self.period_ns))
            .unwrap_or(0)
    }

    fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        // S21 has 4 fans (GPIO tach at 447-450). Sample each slot
        // independently so the dashboard / per-fan UI can flag the
        // exact fan that's slowing down.
        self.sample_all_rpm()
    }

    fn fan_count(&self) -> u8 {
        // S21 / S19j Pro Amlogic / S19K Pro: 4 fans (2 front + 2 rear).
        GPIO_FAN_TACH_COUNT as u8
    }

    fn tach_available(&self) -> bool {
        // AmlogicFan can only be constructed after all four channels arm, and
        // any later sampler failure permanently revokes this owner's evidence.
        self.tach_healthy.load(Ordering::Acquire)
    }
}

/// Amlogic GPIO via sysfs.
struct AmlogicGpio;

impl GpioAccess for AmlogicGpio {
    fn read_plug_detect(&self) -> [bool; 3] {
        let mut result = [false; 3];
        for i in 0..3u32 {
            let gpio = GPIO_PLUG_BASE + i; // 439, 440, 441
            let path = format!("/sys/class/gpio/gpio{}/value", gpio);
            result[i as usize] = fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse::<u8>().ok())
                .map(|v| v == 1) // Active HIGH: 1 = board present
                .unwrap_or(false);
        }
        result
    }

    fn set_board_reset(&self, chain: u8, assert_reset: bool) {
        let gpio = GPIO_RESET_BASE + chain as u32; // 454, 455, 456
        let path = format!("/sys/class/gpio/gpio{}/value", gpio);
        // Active LOW: 0 = assert reset, 1 = running
        let value = if assert_reset { "0" } else { "1" };
        let _ = fs::write(&path, value);
    }
}

// ---------------------------------------------------------------------------
// PSU enable for cold boot
// ---------------------------------------------------------------------------

/// GPIO for PSU enable (PWR_EN, active HIGH: 1=ON, 0=OFF).
///
/// POLARITY CORRECTED 2026-05-21 (EE C1 productionization-sweep finding).
/// Previously this HAL wrote 0=ON / 1=OFF (active LOW, "PSU_nEN"). That was
/// the pre- misreading; it disagreed with every other source of truth
/// (gpio_maps.rs `AmlogicGpioMap`, vnish_cold_boot.rs `GPIO_PWR_EN`, the
/// PRODUCTION-READINESS-MATRIX §4.4, and both  Amlogic GPIO tables).
///
/// Ground truth —  Q10 (RESOLVED 2026-05-10), direct firmware extract of
/// VNish v1.2.7 `S11board` init (and stock-Bitmain bmminer matches it):
///   `echo out > gpio437/direction; echo 1 > gpio437/value` and NO `active_low`
///   file is written, so the kernel default (`active_low=0`) means `value=1`
///   drives the SoC pin electrically HIGH = PSU ON. See
///
///   "PWR_EN active level — RESOLVED 2026-05-10 ( Q10)".
///
/// Why the old wrong polarity was never caught by accepted-share evidence:
/// the S21 `a lab unit` 9-share run (2026-04-11) ran dcentrald ON TOP of already-
/// running BraiinsOS, which had ALREADY driven gpio437 HIGH (PSU on). Native
/// Amlogic cold boot from a PSU-OFF state has never been proven (Phase 3 is
/// BLOCKED), and the old readback gate (write 0, read 0) was a self-consistent
/// tautology that could not detect inverted polarity. The corrected level only
/// matters on a true cold boot — which is exactly the production path this
/// unblocks.
///
/// GPIO is necessary but not sufficient for native cold boot. A captured
/// S19K Pro `a lab unit` BraiinsOS NAND environment contains these U-Boot `preboot`
/// commands while U-Boot's current I2C adapter is selected:
///   `i2c mw 1f 3.1 0 2; i2c mw 1f 1.1 fc 2`
///
/// The environment contains no `i2c dev 1`, so it does not establish that
/// U-Boot's adapter is Linux `/dev/i2c-1`. It is not S21 evidence and does not
/// prove device acknowledgement, APW output state, or rail behavior.
///
/// Keep this GPIO gate first because it is the hard board-level enable.
const GPIO_PSU_ENABLE: u32 = 437;

/// Linux adapter selected by DCENT's current Amlogic management-fabric policy.
/// This bus number is not derived from the captured U-Boot environment.
pub const AMLOGIC_MANAGEMENT_I2C_BUS: u8 = 1;
const APW_PMBUS_ADDR: u8 = 0x1f;

/// Current Linux raw-write translation of the captured U-Boot command text:
///   `[0x03, 0x00, 0x00]`
///   `[0x01, 0xfc, 0xfc]`
///
/// U-Boot `i2c mw` describes memory-write operations. Without the exact vendor
/// implementation or a bus trace, the command text does not prove these bytes
/// were grouped into identical wire transactions.
const APW_PMBUS_CLEAR_FAULTS: [u8; 3] = [0x03, 0x00, 0x00];
const APW_PMBUS_OPERATION_ENABLE: [u8; 3] = [0x01, 0xfc, 0xfc];

/// Standard PMBus STATUS_WORD command. Some Bitmain APW firmwares NACK generic
/// telemetry; use this as a best-effort audit read, not as the enable proof.
const PMBUS_STATUS_WORD: u8 = 0x79;

/// GPIO pins that must be exported to fix I2C pinmux conflict (BOS-3528).
/// Exporting these GPIOs changes the Amlogic pinmux away from PWM/other
/// functions that corrupt the I2C bus. Must be done BEFORE any I2C access.
/// Verified from BraiinsOS S37board_setup: exported as "in" direction.
const GPIO_PINMUX_FIX: [u32; 2] = [476, 477];

/// Retained single owner of the Amlogic management fabric. APW power commands
/// and LM75 telemetry must use this service for the complete hardware session;
/// callers never receive its generic I2C handle.
#[derive(Clone)]
pub struct AmlogicPowerThermalService {
    i2c: I2cServiceHandle,
    required_slots: [bool; 3],
}

/// Cloneable, operation-free capability used by teardown guards to fence all
/// future bus-1 mutations before they cut GPIO437.
#[derive(Clone)]
pub struct AmlogicPowerThermalFence {
    i2c: I2cServiceHandle,
}

impl AmlogicPowerThermalFence {
    pub fn latch_terminal_safe_off(&self) -> TerminalSafeOffTransition {
        self.i2c.latch_terminal_safe_off()
    }
}

/// Software evidence that both fixed APW enable writes completed. Optional
/// STATUS_WORD is diagnostic only and does not prove physical rail voltage.
#[derive(Debug, Clone, Copy)]
pub struct ApwEnableReceipt {
    writes_completed_at: Instant,
    status_word: Option<u16>,
}

impl ApwEnableReceipt {
    pub fn writes_completed_at(self) -> Instant {
        self.writes_completed_at
    }

    pub fn status_word(self) -> Option<u16> {
        self.status_word
    }
}

impl AmlogicPowerThermalService {
    /// Reserve `/dev/i2c-1`, perform the checked BOS-3528 pinmux preparation
    /// under that reservation, and start one kernel-fd-only service.
    fn spawn(admission: &AmlogicNoPicAdmission) -> Result<Self> {
        let i2c = spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation(
            AMLOGIC_MANAGEMENT_I2C_BUS,
            Vec::new(),
            prepare_management_i2c_pinmux,
        )
        .map_err(|error| {
            HalError::Platform(format!(
                "failed to reserve Amlogic management I2C fabric: {error}"
            ))
        })?;
        // A synchronous control round-trip proves the worker opened its fd and
        // accepted the fixed timeout before any power or telemetry operation.
        i2c.set_timeout(10)?;
        Ok(Self {
            i2c,
            required_slots: admission.populated_slots(),
        })
    }

    pub fn terminal_fence(&self) -> AmlogicPowerThermalFence {
        AmlogicPowerThermalFence {
            i2c: self.i2c.clone(),
        }
    }

    /// Enable GPIO437 and issue the two fixed DCENT APW compatibility writes
    /// derived from captured S19K Pro `a lab unit` U-Boot command text. Any failure
    /// after the GPIO mutation performs checked GPIO rollback before returning.
    pub fn enable_psu(&self) -> Result<ApwEnableReceipt> {
        if let Err(enable_error) = enable_psu_gpio() {
            return match disable_psu_checked() {
                Ok(_) => Err(enable_error),
                Err(rollback_error) => Err(HalError::Platform(format!(
                    "PSU GPIO enable failed ({enable_error}); checked rollback also failed ({rollback_error})"
                ))),
            };
        }
        if let Err(enable_error) = self.enable_apw() {
            return match disable_psu_checked() {
                Ok(_) => Err(enable_error),
                Err(rollback_error) => Err(HalError::Platform(format!(
                    "PSU PMBus enable failed ({enable_error}); checked rollback also failed ({rollback_error})"
                ))),
            };
        }

        let writes_completed_at = Instant::now();
        std::thread::sleep(Duration::from_secs(2));
        let status_word = match self.read_apw_status_word() {
            Ok(status) => {
                tracing::info!(
                    bus = AMLOGIC_MANAGEMENT_I2C_BUS,
                    addr = format_args!("0x{:02X}", APW_PMBUS_ADDR),
                    status = format_args!("0x{:04X}", status),
                    "Amlogic APW enable writes completed; optional STATUS_WORD available"
                );
                Some(status)
            }
            Err(error) => {
                tracing::warn!(
                    bus = AMLOGIC_MANAGEMENT_I2C_BUS,
                    addr = format_args!("0x{:02X}", APW_PMBUS_ADDR),
                    %error,
                    "Amlogic APW enable writes completed; optional STATUS_WORD unavailable"
                );
                None
            }
        };
        Ok(ApwEnableReceipt {
            writes_completed_at,
            status_word,
        })
    }

    fn enable_apw(&self) -> Result<()> {
        self.i2c.transaction_mutating(
            I2cMutationLabel::Energize,
            APW_PMBUS_ADDR,
            vec![
                I2cTransactionStep::Write(APW_PMBUS_CLEAR_FAULTS.to_vec()),
                I2cTransactionStep::SleepMs(10),
                I2cTransactionStep::Write(APW_PMBUS_OPERATION_ENABLE.to_vec()),
                I2cTransactionStep::SleepMs(200),
            ],
        )?;
        Ok(())
    }

    fn read_apw_status_word(&self) -> Result<u16> {
        let bytes = self.i2c.write_read_mutating(
            I2cMutationLabel::QueryPrelude,
            APW_PMBUS_ADDR,
            &[PMBUS_STATUS_WORD],
            2,
        )?;
        let bytes: [u8; 2] = bytes.try_into().map_err(|bytes: Vec<u8>| HalError::I2c {
            bus: AMLOGIC_MANAGEMENT_I2C_BUS,
            addr: APW_PMBUS_ADDR,
            detail: format!(
                "APW STATUS_WORD returned {} byte(s); exactly 2 required",
                bytes.len()
            ),
        })?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Capture all statically mapped board sensors through the same retained
    /// fabric owner. Unpowered/unpopulated endpoints remain explicit evidence
    /// in the returned snapshot rather than causing an adapter reset.
    pub fn read_board_temperatures(&self, deadline: Instant) -> AmlogicTemperatureSnapshot {
        let mut readings = Vec::with_capacity(AMLOGIC_TEMPERATURE_ENDPOINTS.len());
        let mut unavailable = Vec::new();
        for endpoint in AMLOGIC_TEMPERATURE_ENDPOINTS {
            match self
                .i2c
                .read_lm75_temperature_register_at(endpoint.address, deadline)
            {
                // LM75-family wire values top out below 128 C. Preserve every
                // high observation as hot evidence; 125 C must never be hidden
                // as "unavailable" while a cooler endpoint survives.
                Ok(register) if register.celsius() >= -40.0 => {
                    readings.push(AmlogicTemperatureReading { endpoint, register });
                }
                Ok(register) => unavailable.push(AmlogicTemperatureUnavailable {
                    endpoint,
                    reason: AmlogicTemperatureUnavailableReason::BelowAdmittedRange {
                        raw_register: register.raw_be(),
                    },
                    detail: format!(
                        "temperature {:.4} C is below the admitted board-sensor range",
                        register.celsius()
                    ),
                }),
                Err(error) => unavailable.push(AmlogicTemperatureUnavailable {
                    endpoint,
                    reason: AmlogicTemperatureUnavailableReason::from_hal_error(&error),
                    detail: error.to_string(),
                }),
            }
        }
        AmlogicTemperatureSnapshot {
            required_slots: self.required_slots,
            readings,
            unavailable,
        }
    }
}

/// Enable the APW PSU output for cold boot.
///
/// On S21, the PSU bring-up has two layers (polarity corrected 2026-05-21):
///   - LOW  (0) = PSU disabled (safe init / shutdown)
///   - HIGH (1) = PSU enabled (12V output active)
///   - APW at I2C bus 1 / address 0x1f receives the stock U-Boot preboot
///     enable sequence before ASIC probing.
///
/// Stock/VNish userspace drives gpio437 HIGH (`echo 1`) to enable hashboard
/// power. Native DCENT_OS must also replay the APW I2C sequence because there
/// is no bosminer/BraiinsOS rootfs in the final boot.
fn enable_psu_gpio() -> Result<()> {
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);

    // Ensure GPIO is exported
    let export_path = "/sys/class/gpio/export";
    if !std::path::Path::new(&gpio_path).exists() {
        let _ = fs::write(export_path, format!("{}", GPIO_PSU_ENABLE));
        std::thread::sleep(Duration::from_millis(100));
    }

    // Pin raw active-high mode explicitly. A stale active_low=1 would invert
    // logical sysfs readback and could turn a requested safety LOW into HIGH.
    ensure_psu_active_low_disabled_checked()?;
    let dir_path = format!("/sys/class/gpio/gpio{}/direction", GPIO_PSU_ENABLE);
    fs::write(&dir_path, "low")
        .map_err(|e| HalError::Platform(format!("PSU GPIO direction: {}", e)))?;
    fs::write(&gpio_path, "1")
        .map_err(|e| HalError::Platform(format!("PSU GPIO enable: {}", e)))?;

    std::thread::sleep(Duration::from_millis(50));
    if !read_psu_enabled_checked()? {
        return Err(HalError::Platform(format!(
            "PSU GPIO {} readback stayed LOW after enable",
            GPIO_PSU_ENABLE
        )));
    }

    tracing::info!(
        "PSU GPIO {} driven HIGH and read back HIGH (PSU enabled)",
        GPIO_PSU_ENABLE
    );

    Ok(())
}

fn read_psu_active_low_disabled_checked() -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/active_low", GPIO_PSU_ENABLE);
    let value = fs::read_to_string(&path)
        .map_err(|error| HalError::Platform(format!("PSU GPIO active_low readback: {error}")))?;
    if value.trim() == "0" {
        Ok(())
    } else {
        Err(HalError::Platform(format!(
            "PSU GPIO {} active_low readback was {:?}, expected raw active-high mode 0",
            GPIO_PSU_ENABLE,
            value.trim()
        )))
    }
}

fn ensure_psu_active_low_disabled_checked() -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/active_low", GPIO_PSU_ENABLE);
    fs::write(&path, "0")
        .map_err(|error| HalError::Platform(format!("PSU GPIO active_low=0: {error}")))?;
    read_psu_active_low_disabled_checked()
}

/// Checked software safe-off evidence for GPIO437. This proves that the LOW
/// sysfs write completed and a subsequent fallible sysfs read returned LOW; it
/// does not claim an independently measured physical rail voltage.
#[derive(Debug)]
pub struct PsuSafeOffReceipt {
    gpio: u32,
    completed_at: Instant,
}

impl PsuSafeOffReceipt {
    pub fn gpio(&self) -> u32 {
        self.gpio
    }

    pub fn completed_at(&self) -> Instant {
        self.completed_at
    }
}

/// Disable the APW PSU output and require a checked LOW readback.
pub fn disable_psu_checked() -> Result<PsuSafeOffReceipt> {
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);
    ensure_psu_active_low_disabled_checked()?;
    // Drive LOW to disable PSU (active HIGH,  Q10 — corrected 2026-05-21).
    fs::write(&gpio_path, "0")
        .map_err(|e| HalError::Platform(format!("PSU GPIO disable: {}", e)))?;

    std::thread::sleep(Duration::from_millis(50));
    if read_psu_enabled_checked()? {
        return Err(HalError::Platform(format!(
            "PSU GPIO {} readback stayed HIGH after disable",
            GPIO_PSU_ENABLE
        )));
    }

    tracing::info!(
        "PSU GPIO {} driven LOW and read back LOW (PSU disabled)",
        GPIO_PSU_ENABLE
    );
    Ok(PsuSafeOffReceipt {
        gpio: GPIO_PSU_ENABLE,
        completed_at: Instant::now(),
    })
}

/// Compatibility wrapper for callers that do not yet consume typed evidence.
pub fn disable_psu() -> Result<()> {
    disable_psu_checked().map(|_| ())
}

/// Read GPIO437 without converting I/O or parse failures into a false `off`.
pub fn read_psu_enabled_checked() -> Result<bool> {
    read_psu_active_low_disabled_checked()?;
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);
    let value = fs::read_to_string(&gpio_path)
        .map_err(|error| HalError::Platform(format!("PSU GPIO readback: {error}")))?;
    parse_psu_gpio_enabled(&value)
}

fn parse_psu_gpio_enabled(value: &str) -> Result<bool> {
    match value.trim() {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(HalError::Platform(format!(
            "PSU GPIO readback was neither 0 nor 1: {other:?}"
        ))),
    }
}

/// Lossy telemetry compatibility helper. Safety decisions must use
/// [`read_psu_enabled_checked`] or [`disable_psu_checked`].
pub fn is_psu_enabled() -> bool {
    read_psu_enabled_checked().unwrap_or(false)
}

/// Checked BOS-3528 pinmux preparation. This runs only inside the bus-1 fabric
/// reservation, before the worker opens `/dev/i2c-1`.
fn prepare_management_i2c_pinmux() -> std::io::Result<()> {
    let export_path = "/sys/class/gpio/export";
    for gpio in GPIO_PINMUX_FIX {
        let gpio_dir = format!("/sys/class/gpio/gpio{}", gpio);
        let gpio_path = Path::new(&gpio_dir);
        if !gpio_path.exists() {
            if let Err(error) = fs::write(export_path, gpio.to_string()) {
                if error.raw_os_error() != Some(libc::EBUSY) {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("failed to export pinmux GPIO {gpio}: {error}"),
                    ));
                }
            }
        }

        let ready_deadline = Instant::now() + Duration::from_millis(500);
        while !gpio_path.exists() && Instant::now() < ready_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !gpio_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("pinmux GPIO {gpio} did not appear after export"),
            ));
        }

        let direction_path = format!("{gpio_dir}/direction");
        fs::write(&direction_path, "in").map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to set pinmux GPIO {gpio} input: {error}"),
            )
        })?;
        let observed = fs::read_to_string(&direction_path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("failed to read pinmux GPIO {gpio} direction: {error}"),
            )
        })?;
        if observed.trim() != "in" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "pinmux GPIO {gpio} direction readback was {:?}, expected input",
                    observed.trim()
                ),
            ));
        }
        tracing::debug!(gpio, "Amlogic management-I2C pinmux GPIO verified as input");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Temperature sensor reading (LM75-compatible)
// ---------------------------------------------------------------------------

/// LM75-compatible sensor addresses on Amlogic-platform hash boards.
///
/// Verified live (.78, S19K Pro NoPic, 2026-04-29) via `bosminer.log`:
///   hb1: address 72 (inlet), 76 (outlet)   → 0x48 / 0x4C
///   hb2: address 73 (inlet), 77 (outlet)   → 0x49 / 0x4D
///   hb3: address 74 (inlet), 78 (outlet)   → 0x4A / 0x4E
///
/// Per-chain layout: inlets at `0x48 + chain_id`, outlets at
/// `0x4C + chain_id`. Same convention applies to S21 / S21 Pro / S19j Pro
/// Amlogic / S19K Pro NoPic / S19 XP — all am3-aml hashboard families.
///
/// **Bug fix history**: Pre-2026-04-29 this const declared `(0x73, ...)`
/// which was the decimal-as-hex confusion (decimal 73 = hex 0x49). The
/// raw I²C ioctl at line ~488 would NACK on hex 0x73 (no device present),
/// so temperature reads silently failed. Per Phase H.5 expert-agent
/// finding (Thermal+Perf).
///
/// The table below is keyed by physical slot, not a runtime chain count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmlogicTemperaturePosition {
    Inlet,
    Outlet,
}

/// Board-slot-bound sensor endpoint. This is physical wiring data and must
/// never be inferred from ASIC count or runtime chain enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmlogicTemperatureEndpoint {
    slot: u8,
    address: u8,
    position: AmlogicTemperaturePosition,
}

impl AmlogicTemperatureEndpoint {
    pub fn slot(self) -> u8 {
        self.slot
    }

    pub fn address(self) -> u8 {
        self.address
    }

    pub fn position(self) -> AmlogicTemperaturePosition {
        self.position
    }
}

/// Verified live on the .78 S19K Pro: inlet 0x48..=0x4a and outlet
/// 0x4c..=0x4e, paired by physical board slot.
const AMLOGIC_TEMPERATURE_ENDPOINTS: [AmlogicTemperatureEndpoint; 6] = [
    AmlogicTemperatureEndpoint {
        slot: 0,
        address: 0x48,
        position: AmlogicTemperaturePosition::Inlet,
    },
    AmlogicTemperatureEndpoint {
        slot: 0,
        address: 0x4c,
        position: AmlogicTemperaturePosition::Outlet,
    },
    AmlogicTemperatureEndpoint {
        slot: 1,
        address: 0x49,
        position: AmlogicTemperaturePosition::Inlet,
    },
    AmlogicTemperatureEndpoint {
        slot: 1,
        address: 0x4d,
        position: AmlogicTemperaturePosition::Outlet,
    },
    AmlogicTemperatureEndpoint {
        slot: 2,
        address: 0x4a,
        position: AmlogicTemperaturePosition::Inlet,
    },
    AmlogicTemperatureEndpoint {
        slot: 2,
        address: 0x4e,
        position: AmlogicTemperaturePosition::Outlet,
    },
];

#[derive(Debug, Clone, Copy)]
pub struct AmlogicTemperatureReading {
    endpoint: AmlogicTemperatureEndpoint,
    register: Lm75TemperatureRegister,
}

impl AmlogicTemperatureReading {
    pub fn endpoint(self) -> AmlogicTemperatureEndpoint {
        self.endpoint
    }

    pub fn raw_register(self) -> i16 {
        self.register.raw_be()
    }

    pub fn celsius(self) -> f32 {
        self.register.celsius()
    }
}

#[derive(Debug, Clone)]
pub struct AmlogicTemperatureUnavailable {
    endpoint: AmlogicTemperatureEndpoint,
    reason: AmlogicTemperatureUnavailableReason,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmlogicTemperatureUnavailableReason {
    BelowAdmittedRange { raw_register: i16 },
    EndpointNotReady,
    EndpointRefused,
    TransportFault,
}

impl AmlogicTemperatureUnavailableReason {
    fn from_hal_error(error: &HalError) -> Self {
        match error {
            HalError::I2cEndpointNotReady { .. } => Self::EndpointNotReady,
            HalError::I2cEndpointRefused { .. }
            | HalError::I2cAdmissionBusy { .. }
            | HalError::I2cSafetySuperseded { .. } => Self::EndpointRefused,
            _ => Self::TransportFault,
        }
    }
}

impl AmlogicTemperatureUnavailable {
    pub fn endpoint(&self) -> AmlogicTemperatureEndpoint {
        self.endpoint
    }

    pub fn reason(&self) -> AmlogicTemperatureUnavailableReason {
        self.reason
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Debug, Clone)]
pub struct AmlogicTemperatureSnapshot {
    required_slots: [bool; 3],
    readings: Vec<AmlogicTemperatureReading>,
    unavailable: Vec<AmlogicTemperatureUnavailable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmlogicTemperatureCoverage {
    required_slots: [bool; 3],
    inlet_available: [bool; 3],
    outlet_available: [bool; 3],
}

impl AmlogicTemperatureCoverage {
    pub fn required_slots(self) -> [bool; 3] {
        self.required_slots
    }

    pub fn inlet_available(self, slot: u8) -> bool {
        self.inlet_available
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    pub fn outlet_available(self, slot: u8) -> bool {
        self.outlet_available
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    pub fn is_complete(self) -> bool {
        (0..3).all(|slot| {
            !self.required_slots[slot]
                || (self.inlet_available[slot] && self.outlet_available[slot])
        })
    }

    pub fn missing_slots(self) -> Vec<u8> {
        (0..3u8)
            .filter(|slot| {
                self.required_slots[*slot as usize]
                    && !(self.inlet_available(*slot) && self.outlet_available(*slot))
            })
            .collect()
    }
}

impl AmlogicTemperatureSnapshot {
    pub fn readings(&self) -> &[AmlogicTemperatureReading] {
        &self.readings
    }

    pub fn unavailable(&self) -> &[AmlogicTemperatureUnavailable] {
        &self.unavailable
    }

    /// The PSU gate is chassis-wide, so every slot asserted by checked
    /// plug-detect topology requires both its inlet and outlet sensor.
    pub fn required_coverage(&self) -> AmlogicTemperatureCoverage {
        let mut inlet_available = [false; 3];
        let mut outlet_available = [false; 3];
        for reading in &self.readings {
            let slot = reading.endpoint.slot as usize;
            if slot >= self.required_slots.len() || !self.required_slots[slot] {
                continue;
            }
            match reading.endpoint.position {
                AmlogicTemperaturePosition::Inlet => inlet_available[slot] = true,
                AmlogicTemperaturePosition::Outlet => outlet_available[slot] = true,
            }
        }
        AmlogicTemperatureCoverage {
            required_slots: self.required_slots,
            inlet_available,
            outlet_available,
        }
    }

    pub fn hottest_celsius(&self) -> Option<f32> {
        self.readings
            .iter()
            .map(|reading| reading.celsius())
            .reduce(f32::max)
    }
}

// Board temperatures are available only through
// `AmlogicPowerThermalService::read_board_temperatures`.
fn normalize_model_token(model: &str) -> String {
    model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect()
}

fn config_for_dcentos_platform(marker: &str) -> Result<Option<PlatformConfig>> {
    let normalized = marker.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "am3-aml-s19k" | "am3-aml-s19kpro" | "am3-aml-s19xp" => {
            Ok(Some(PlatformConfig::s19k_amlogic()))
        }
        "am3-aml" | "am3-aml-s21" | "am3-aml-s21pro" | "am3-aml-s21xp"
        | "am3-aml-t21" => Ok(Some(PlatformConfig::s21_amlogic())),
        // These targets use a per-hashboard controller and cannot inherit a
        // NoPic profile merely because the control board is also A113D.
        "am3-aml-s19jpro" | "am3-aml-s19jproplus" => Err(HalError::Platform(format!(
            "Amlogic S19j Pro platform {normalized:?} requires its dedicated controller profile; refusing NoPic fallback"
        ))),
        _ => Ok(None),
    }
}

fn nopic_profile_for_model_value(model: &str) -> Option<AmlogicNoPicProfile> {
    match normalize_model_token(model).as_str() {
        "antminers19kpro"
        | "antminers19kpronopic"
        | "s19kpro"
        | "s19kpronopic"
        | "antminers19xp"
        | "s19xp" => Some(AmlogicNoPicProfile::S19k),
        "antminers21" | "antminers21pro" | "antminers21xp" | "antminers21+" | "antminert21"
        | "s21" | "s21pro" | "s21xp" | "s21+" | "t21" => Some(AmlogicNoPicProfile::S21),
        _ => None,
    }
}

fn nopic_profile_for_bosminer_toml(source: &str) -> Result<Option<AmlogicNoPicProfile>> {
    let document: toml::Value = toml::from_str(source).map_err(|error| {
        HalError::Platform(format!(
            "failed to parse /etc/bosminer.toml for Amlogic admission: {error}"
        ))
    })?;
    let model = document
        .get("format")
        .and_then(|format| format.get("model"))
        .and_then(toml::Value::as_str);
    Ok(model.and_then(nopic_profile_for_model_value))
}

fn config_for_bosminer_model(source: &str) -> Result<Option<PlatformConfig>> {
    Ok(match nopic_profile_for_bosminer_toml(source)? {
        Some(AmlogicNoPicProfile::S19k) => Some(PlatformConfig::s19k_amlogic()),
        Some(AmlogicNoPicProfile::S21) => Some(PlatformConfig::s21_amlogic()),
        None => None,
    })
}

fn nopic_profile_for_dcentos_marker(marker: &str) -> Option<AmlogicNoPicProfile> {
    match marker.trim().to_ascii_lowercase().as_str() {
        "am3-aml-s19k" | "am3-aml-s19kpro" | "am3-aml-s19xp" => Some(AmlogicNoPicProfile::S19k),
        "am3-aml-s21" | "am3-aml-s21pro" | "am3-aml-s21xp" | "am3-aml-t21" => {
            Some(AmlogicNoPicProfile::S21)
        }
        _ => None,
    }
}

/// Detect only identity evidence strong enough to authorize Amlogic NoPic
/// mutation. Unlike generic platform reporting, this never guesses from the
/// cross-SKU `am3-aml` marker or substring-scans an arbitrary config file.
fn detect_amlogic_nopic_profile() -> Result<AmlogicNoPicProfile> {
    if let Ok(marker) = fs::read_to_string("/etc/dcentos-platform") {
        return nopic_profile_for_dcentos_marker(&marker).ok_or_else(|| {
            HalError::Platform(format!(
                "/etc/dcentos-platform value {:?} is not SKU-qualified for Amlogic NoPic mutation",
                marker.trim()
            ))
        });
    }
    if let Ok(source) = fs::read_to_string("/etc/bosminer.toml") {
        return nopic_profile_for_bosminer_toml(&source)?.ok_or_else(|| {
            HalError::Platform(
                "/etc/bosminer.toml [format].model is missing or not an admitted Amlogic NoPic SKU"
                    .to_string(),
            )
        });
    }
    let model = fs::read_to_string("/proc/device-tree/model")
        .unwrap_or_default()
        .trim_end_matches('\0')
        .to_string();
    nopic_profile_for_model_value(&model).ok_or_else(|| {
        HalError::Platform(format!(
            "device-tree model {model:?} is not SKU-qualified for Amlogic NoPic mutation"
        ))
    })
}

/// Detect which Amlogic model we're running on.
///
/// Signal precedence (highest priority first):
/// 1. `/etc/dcentos-platform` — set explicitly by our buildroot board overlay
///    (Phase H.12). Authoritative when DCENT_OS is the running rootfs.
/// 2. `/etc/bos_platform` + `/etc/bosminer.toml` model — set by BraiinsOS+;
///    used when DCENT_OS daemon runs side-by-side with bosminer (passthrough
///    or runtime-only mode), or for Phase K dry-run install detection.
/// 3. `/proc/device-tree/model` from DTB — last resort. On `a lab unit` this is
///    just literal "Amlogic" (no specific model), so DTB alone cannot
///    disambiguate S19K Pro vs S21 vs S19j Pro Amlogic. Live-verified.
fn detect_amlogic_model() -> Result<PlatformConfig> {
    // 1. /etc/dcentos-platform takes highest precedence.
    if let Ok(plat) = fs::read_to_string("/etc/dcentos-platform") {
        let plat_norm = plat.trim().to_ascii_lowercase();
        tracing::debug!(platform = %plat_norm, "Read /etc/dcentos-platform");
        match config_for_dcentos_platform(&plat_norm)? {
            Some(config) => return Ok(config),
            None => tracing::debug!(
                platform = %plat_norm,
                "/etc/dcentos-platform did not match a known token; falling back"
            ),
        }
    }

    // 2. BraiinsOS+ secondary signal: /etc/bosminer.toml `model` field.
    //    This wins over DTB when bosminer is the running rootfs because
    //    it carries the exact factory model name (e.g. "Antminer S19K Pro NoPic").
    if let Ok(toml) = fs::read_to_string("/etc/bosminer.toml") {
        if let Some(config) = config_for_bosminer_model(&toml)? {
            tracing::info!(profile = %config.name, "Detected Amlogic profile from /etc/bosminer.toml model field");
            return Ok(config);
        }
    }

    // 3. Fallback: device-tree model. Often too generic on Amlogic AXG
    //    (.78 returns just "Amlogic"), so this is the last-resort path.
    let model = fs::read_to_string("/proc/device-tree/model")
        .unwrap_or_default()
        .trim_end_matches('\0')
        .to_string();
    let normalized = normalize_model_token(&model);

    tracing::debug!(model = %model, normalized = %normalized, "Device tree model");

    if normalized.is_empty() {
        Err(HalError::Platform(
            "Missing Amlogic model string; refusing to guess an unsafe platform profile"
                .to_string(),
        ))
    } else if normalized.contains("s19jpro") {
        Err(HalError::Platform(
            format!(
                "Amlogic S19j Pro profile is not implemented yet; refusing to guess a serial platform profile for '{}'",
                model
            ),
        ))
    } else if normalized.contains("s19j") {
        Err(HalError::Platform(
            format!(
                "Amlogic S19j profile is not implemented yet; refusing to use the S19k profile for '{}'",
                model
            ),
        ))
    } else if normalized.contains("s19k") || normalized.contains("s19xp") {
        Ok(PlatformConfig::s19k_amlogic())
    } else if normalized.contains("s21pro")
        || normalized.contains("s21xp")
        || normalized.contains("s21plus")
        || normalized.contains("s21")
        || normalized.contains("t21")
    {
        Ok(PlatformConfig::s21_amlogic())
    } else if normalized.contains("s19") {
        Err(HalError::Platform(
            format!(
                "Unsupported/ambiguous Amlogic S19 model '{}'; refusing to guess an unsafe platform profile",
                model
            ),
        ))
    } else {
        Err(HalError::Platform(format!(
            "Unknown Amlogic model '{}'; refusing to guess an unsafe platform profile",
            model
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot_safe_handoff_fixture() -> String {
        [
            "schema=dcentos.amlogic-safe-state/v1",
            "state=runtime-handoff",
            "boot_id=11111111-2222-3333-4444-555555555555",
            "platform=am3-aml-s21",
            "board_target=am3-s21",
            "resource=amlogic-gpio437-power-gate",
            "gpio_direction=out",
            "gpio_active_low=0",
            "commanded_value=0",
            "readback_value=0",
            "fan0_duty_ns=30000",
            "fan0_period_ns=100000",
            "fan0_enabled=1",
            "fan1_duty_ns=30000",
            "fan1_period_ns=100000",
            "fan1_enabled=1",
            "evidence_grade=software-readback",
            "physical_rail_measured=false",
        ]
        .join("\n")
    }

    #[test]
    fn boot_safe_handoff_parser_is_exact_and_honest_about_evidence() {
        let receipt = parse_amlogic_boot_safe_handoff(&boot_safe_handoff_fixture()).unwrap();
        assert_eq!(receipt.platform, "am3-aml-s21");
        assert_eq!(receipt.board_target, "am3-s21");
        assert_eq!(receipt.fan0_duty_ns, AML_BOOT_FAN_DUTY_NS);

        for mutation in [
            ("state=runtime-handoff", "state=boot-safe"),
            ("readback_value=0", "readback_value=1"),
            ("gpio_active_low=0", "gpio_active_low=1"),
            (
                "evidence_grade=software-readback",
                "evidence_grade=measured-rail-off",
            ),
            (
                "physical_rail_measured=false",
                "physical_rail_measured=true",
            ),
            ("fan1_duty_ns=30000", "fan1_duty_ns=100000"),
            ("fan0_period_ns=100000", "fan0_period_ns=30000"),
            ("fan1_enabled=1", "fan1_enabled=0"),
        ] {
            let invalid = boot_safe_handoff_fixture().replace(mutation.0, mutation.1);
            assert!(
                parse_amlogic_boot_safe_handoff(&invalid).is_err(),
                "mutation {:?} must be refused",
                mutation
            );
        }

        let duplicate = format!("{}\nstate=runtime-handoff", boot_safe_handoff_fixture());
        assert!(parse_amlogic_boot_safe_handoff(&duplicate).is_err());
        let unknown = format!("{}\nunexpected=true", boot_safe_handoff_fixture());
        assert!(parse_amlogic_boot_safe_handoff(&unknown).is_err());
    }

    #[test]
    fn boot_safe_handoff_identity_is_exact_sku_not_prefix_based() {
        assert!(amlogic_handoff_identity_matches_profile(
            AmlogicNoPicProfile::S19k,
            "am3-aml-s19k",
            "am3-s19k"
        ));
        for (platform, target) in [
            ("am3-aml-s21", "am3-s21"),
            ("am3-aml-s21pro", "am3-s21pro"),
            ("am3-aml-s21xp", "am3-s21xp"),
            ("am3-aml-t21", "am3-t21"),
        ] {
            assert!(amlogic_handoff_identity_matches_profile(
                AmlogicNoPicProfile::S21,
                platform,
                target
            ));
        }
        for (platform, target) in [
            ("am3-aml", "am3-s21"),
            ("am3-aml-s21", "am3-s21xp"),
            ("am3-aml-s19jpro", "am3-s19jpro-aml"),
            ("am3-aml-s21-future", "am3-s21-future"),
        ] {
            assert!(!amlogic_handoff_identity_matches_profile(
                AmlogicNoPicProfile::S21,
                platform,
                target
            ));
        }
    }

    #[test]
    fn checked_psu_gpio_parser_never_converts_unknown_data_to_off() {
        assert_eq!(parse_psu_gpio_enabled("0\n").unwrap(), false);
        assert_eq!(parse_psu_gpio_enabled("1\n").unwrap(), true);
        for invalid in ["", "2", "off", "read error"] {
            assert!(
                parse_psu_gpio_enabled(invalid).is_err(),
                "value={invalid:?}"
            );
        }
    }

    #[test]
    fn shipped_amlogic_platform_tokens_are_explicitly_classified_or_refused() {
        for marker in [
            "am3-aml-s19k",
            "am3-aml-s19kpro",
            "am3-aml-s19xp",
            "am3-aml-s21",
            "am3-aml-s21pro",
            "am3-aml-s21xp",
            "am3-aml-t21",
        ] {
            assert!(
                config_for_dcentos_platform(marker).unwrap().is_some(),
                "shipped marker {marker} must not fall through to generic DT detection"
            );
        }
        for marker in ["am3-aml-s19jpro", "am3-aml-s19jproplus"] {
            let error = config_for_dcentos_platform(marker)
                .expect_err("S19j Pro must not inherit a NoPic transport profile");
            assert!(error.to_string().contains("dedicated controller profile"));
        }
        assert!(config_for_dcentos_platform("future-amlogic")
            .unwrap()
            .is_none());
    }

    #[test]
    fn bosminer_model_identity_covers_admitted_s19k_and_s21_families() {
        for model in ["Antminer S19K Pro NoPic", "Antminer S19 XP"] {
            let config = config_for_bosminer_model(&format!("[format]\nmodel = {model:?}"))
                .unwrap()
                .expect("S19K/S19 XP bosminer identity");
            assert_eq!(config.name, PlatformConfig::s19k_amlogic().name);
        }
        for model in ["Antminer S21", "Antminer S21 Pro", "Antminer T21"] {
            let config = config_for_bosminer_model(&format!("[format]\nmodel = {model:?}"))
                .unwrap()
                .expect("S21/T21 bosminer identity");
            assert_eq!(config.name, PlatformConfig::s21_amlogic().name);
        }
        assert!(config_for_bosminer_model("[format]\nnotes = 'S21 spare'")
            .unwrap()
            .is_none());
        assert!(
            config_for_bosminer_model("[format]\nmodel = 'future miner'")
                .unwrap()
                .is_none()
        );
        assert!(config_for_bosminer_model("[format\nmodel = 'Antminer S21'").is_err());
        assert_eq!(nopic_profile_for_dcentos_marker("am3-aml"), None);
    }

    #[test]
    fn nopic_admission_binds_detected_profile_and_physical_uart_slot() {
        assert_eq!(amlogic_slot_from_serial_device("/dev/ttyS1"), Some(0));
        assert_eq!(amlogic_slot_from_serial_device("/dev/ttyS2"), Some(1));
        assert_eq!(amlogic_slot_from_serial_device("/dev/ttyS3"), Some(2));
        assert_eq!(amlogic_slot_from_serial_device("/dev/ttyS4"), Some(2));
        assert_eq!(amlogic_slot_from_serial_device("/dev/ttyO1"), None);

        let admission = AmlogicNoPicAdmission::from_profile_evidence(
            AmlogicNoPicProfile::S21,
            1,
            AmlogicNoPicProfile::S21,
            [true, true, false],
        )
        .unwrap();
        assert_eq!(admission.profile(), AmlogicNoPicProfile::S21);
        assert_eq!(admission.active_slot(), 1);
        assert_eq!(admission.populated_slots(), [true, true, false]);

        assert!(AmlogicNoPicAdmission::from_profile_evidence(
            AmlogicNoPicProfile::S21,
            1,
            AmlogicNoPicProfile::S19k,
            [false, true, false],
        )
        .is_err());
        assert!(AmlogicNoPicAdmission::from_profile_evidence(
            AmlogicNoPicProfile::S21,
            3,
            AmlogicNoPicProfile::S21,
            [false, true, false],
        )
        .is_err());
        assert!(AmlogicNoPicAdmission::from_profile_evidence(
            AmlogicNoPicProfile::S21,
            1,
            AmlogicNoPicProfile::S21,
            [true, false, false],
        )
        .is_err());
    }

    #[test]
    fn checked_fan_command_surfaces_partial_two_channel_write() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "dcentos-amlogic-fan-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let rear = root.join("rear_duty");
        std::fs::write(&rear, "0").unwrap();
        let front = root.join("missing-parent").join("front_duty");
        let fan = AmlogicFan {
            rear_duty_path: rear.to_string_lossy().into_owned(),
            front_duty_path: front.to_string_lossy().into_owned(),
            period_ns: AMLOGIC_PWM_PERIOD_NS,
            tach_sources: std::array::from_fn(|_| {
                Box::new(FixedEdgeMock(0)) as Box<dyn FanTachSource>
            }),
            tach_healthy: AtomicBool::new(true),
            sample_window: Duration::from_millis(1),
        };

        let result = fan.set_speed_checked(30);
        assert!(result.is_err(), "front-channel failure must reject receipt");
        assert_eq!(std::fs::read_to_string(&rear).unwrap(), "30000");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apw_pmbus_compatibility_translation_is_stable() {
        // This pins DCENT's current Linux translation; it does not prove U-Boot
        // wire framing, U-Boot/Linux adapter identity, or S21 applicability.
        assert_eq!(AMLOGIC_MANAGEMENT_I2C_BUS, 1);
        assert_eq!(APW_PMBUS_ADDR, 0x1f);
        assert_eq!(APW_PMBUS_CLEAR_FAULTS, [0x03, 0x00, 0x00]);
        assert_eq!(APW_PMBUS_OPERATION_ENABLE, [0x01, 0xfc, 0xfc]);
    }

    #[test]
    fn amlogic_management_source_has_one_service_owned_bus1_path() {
        let source = include_str!("mod.rs");
        assert!(source.contains(
            "spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation"
        ));
        assert!(source.contains("prepare_management_i2c_pinmux"));
        assert!(source.contains("read_lm75_temperature_register_at"));
        assert!(source.contains("AmlogicNoPicAdmission"));
        assert!(!source
            .lines()
            .any(|line| line.starts_with("pub fn open_fan_controller()")));
        assert!(!source
            .lines()
            .any(|line| line.starts_with("pub fn spawn() -> Result<Self>")));
        let raw_apw_open = ["I2cBus::", "open(APW_PMBUS"].concat();
        assert!(!source.contains(&raw_apw_open));
        let raw_temperature_helper = ["pub fn read_board_", "temps("].concat();
        let raw_apw_helper = ["pub fn enable_psu_", "pmbus("].concat();
        assert!(!source.contains(&raw_temperature_helper));
        assert!(!source.contains(&raw_apw_helper));
    }

    #[cfg(feature = "sim-hal")]
    struct ApwSequenceBackend {
        identity: usize,
        writes: std::sync::Mutex<Vec<(u8, u8, Vec<u8>)>>,
    }

    #[cfg(feature = "sim-hal")]
    impl ApwSequenceBackend {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
            Self {
                identity: usize::MAX
                    - 10_000
                    - NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                writes: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[cfg(feature = "sim-hal")]
    impl crate::i2c::I2cSimBackend for ApwSequenceBackend {
        fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            self.writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((bus, addr, data.to_vec()));
            Ok(data.len())
        }

        fn read(&self, bus: u8, addr: u8, _buf: &mut [u8]) -> Result<usize> {
            Err(HalError::I2c {
                bus,
                addr,
                detail: "APW test backend refuses standalone reads".into(),
            })
        }

        fn write_read(
            &self,
            bus: u8,
            addr: u8,
            _write_data: &[u8],
            _read_buf: &mut [u8],
        ) -> Result<()> {
            Err(HalError::I2cEndpointNotReady {
                bus,
                addr,
                detail: "simulated APW STATUS_WORD NACK".into(),
            })
        }

        fn service_identity(&self) -> Option<usize> {
            Some(self.identity)
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn apw_enable_writes_complete_even_when_optional_status_is_unavailable() {
        let backend = std::sync::Arc::new(ApwSequenceBackend::new());
        let service = AmlogicPowerThermalService {
            i2c: crate::i2c::spawn_sim_i2c_service(
                AMLOGIC_MANAGEMENT_I2C_BUS,
                backend.clone(),
                Vec::new(),
            )
            .unwrap(),
            required_slots: [false, true, false],
        };

        service.enable_apw().unwrap();
        assert!(service.read_apw_status_word().is_err());
        assert_eq!(
            *backend
                .writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                (
                    AMLOGIC_MANAGEMENT_I2C_BUS,
                    APW_PMBUS_ADDR,
                    APW_PMBUS_CLEAR_FAULTS.to_vec(),
                ),
                (
                    AMLOGIC_MANAGEMENT_I2C_BUS,
                    APW_PMBUS_ADDR,
                    APW_PMBUS_OPERATION_ENABLE.to_vec(),
                ),
            ]
        );
    }

    #[test]
    fn fan_pwm_percent_to_duty_uses_0_to_100_scale() {
        assert_eq!(amlogic_pwm_percent_to_duty_ns(0, AMLOGIC_PWM_PERIOD_NS), 0);
        assert_eq!(
            amlogic_pwm_percent_to_duty_ns(30, AMLOGIC_PWM_PERIOD_NS),
            30_000
        );
        assert_eq!(
            amlogic_pwm_percent_to_duty_ns(100, AMLOGIC_PWM_PERIOD_NS),
            AMLOGIC_PWM_PERIOD_NS
        );
        assert_eq!(
            amlogic_pwm_percent_to_duty_ns(127, AMLOGIC_PWM_PERIOD_NS),
            AMLOGIC_PWM_PERIOD_NS,
            "legacy 0-127 inputs must clamp to 100%"
        );
    }

    #[test]
    fn fan_duty_to_pwm_getter_uses_0_to_100_scale() {
        assert_eq!(amlogic_duty_ns_to_pwm_percent(0, AMLOGIC_PWM_PERIOD_NS), 0);
        assert_eq!(
            amlogic_duty_ns_to_pwm_percent(30_000, AMLOGIC_PWM_PERIOD_NS),
            30
        );
        assert_eq!(
            amlogic_duty_ns_to_pwm_percent(100_000, AMLOGIC_PWM_PERIOD_NS),
            100
        );
        assert_eq!(
            amlogic_duty_ns_to_pwm_percent(127_000, AMLOGIC_PWM_PERIOD_NS),
            100,
            "out-of-range sysfs duty is clamped to 100%"
        );
    }

    #[test]
    fn temperature_topology_matches_live_78_physical_slots() {
        // Live-verified per .78 bosminer.log:
        //   hb1.72 (Inlet=0x48), hb1.76 (Outlet=0x4C)
        //   hb2.73 (Inlet=0x49), hb2.77 (Outlet=0x4D)
        //   hb3.74 (Inlet=0x4A), hb3.78 (Outlet=0x4E)
        assert_eq!(AMLOGIC_TEMPERATURE_ENDPOINTS.len(), 6);
        for slot in 0..=2 {
            let pair: Vec<_> = AMLOGIC_TEMPERATURE_ENDPOINTS
                .iter()
                .copied()
                .filter(|endpoint| endpoint.slot() == slot)
                .collect();
            assert_eq!(pair.len(), 2);
            assert_eq!(pair[0].address(), 0x48 + slot);
            assert_eq!(pair[0].position(), AmlogicTemperaturePosition::Inlet);
            assert_eq!(pair[1].address(), 0x4c + slot);
            assert_eq!(pair[1].position(), AmlogicTemperaturePosition::Outlet);
        }
    }

    #[test]
    fn required_slot_coverage_cannot_be_masked_by_other_slots_or_extreme_heat() {
        let reading =
            |endpoint: AmlogicTemperatureEndpoint, celsius: i16| AmlogicTemperatureReading {
                endpoint,
                register: Lm75TemperatureRegister::from_raw_be(celsius * 256),
            };
        let slot0_inlet = AMLOGIC_TEMPERATURE_ENDPOINTS[0];
        let slot0_outlet = AMLOGIC_TEMPERATURE_ENDPOINTS[1];
        let slot1_inlet = AMLOGIC_TEMPERATURE_ENDPOINTS[2];
        let slot1_outlet = AMLOGIC_TEMPERATURE_ENDPOINTS[3];

        let incomplete = AmlogicTemperatureSnapshot {
            required_slots: [true, true, false],
            readings: vec![reading(slot0_inlet, 40), reading(slot1_inlet, 45)],
            unavailable: Vec::new(),
        };
        let coverage = incomplete.required_coverage();
        assert!(coverage.inlet_available(0));
        assert!(coverage.inlet_available(1));
        assert!(!coverage.outlet_available(0));
        assert!(!coverage.outlet_available(1));
        assert_eq!(coverage.missing_slots(), vec![0, 1]);
        assert!(!coverage.is_complete());

        let complete = AmlogicTemperatureSnapshot {
            required_slots: [true, true, false],
            readings: vec![
                reading(slot0_inlet, 40),
                reading(slot0_outlet, 50),
                reading(slot1_inlet, 45),
                reading(slot1_outlet, 55),
            ],
            unavailable: Vec::new(),
        };
        assert!(complete.required_coverage().is_complete());

        let extreme = AmlogicTemperatureSnapshot {
            required_slots: [false, true, false],
            readings: vec![reading(slot1_inlet, 45), reading(slot1_outlet, 125)],
            unavailable: Vec::new(),
        };
        assert!(extreme.required_coverage().is_complete());
        assert_eq!(extreme.hottest_celsius(), Some(125.0));
    }

    // ---------------------------------------------------------------------
    // W3.1 — am3-aml hashboard EEPROM write-deny parity with am2
    // ---------------------------------------------------------------------

    #[test]
    fn amlogic_eeprom_denylist_covers_full_at24c_range() {
        // Contract: every AT24C-class hashboard EEPROM slot from 0x50..=0x57
        // is on the denylist. Same range as am2 (proven against the .74 hb2
        // corruption pattern) and now extended to am3-aml because BHB56902
        // hashboards on S19K Pro carry EEPROMs at the same standard 0x50
        // base. Live-verified via i2cdetect on .78 (2026-04-29).
        assert_eq!(AMLOGIC_EEPROM_DENYLIST.len(), 8);
        for addr in 0x50u8..=0x57u8 {
            assert!(
                AMLOGIC_EEPROM_DENYLIST.contains(&addr),
                "AMLOGIC_EEPROM_DENYLIST must cover 0x{:02X} (AT24C slot)",
                addr
            );
        }
    }

    // -----------------------------------------------------------------
    // W3.2 (2026-05-07) — GPIO falling-edge fan tach replaces synthesized RPM.
    // -----------------------------------------------------------------

    /// Mock tach source that always returns a fixed pulse count. Lets
    /// the test assert the edges→RPM math without real GPIO.
    struct FixedEdgeMock(u32);
    impl FanTachSource for FixedEdgeMock {
        fn sample_falling_edges(&self, _window: Duration) -> std::io::Result<u32> {
            Ok(self.0)
        }
    }

    struct FailingEdgeMock;
    impl FanTachSource for FailingEdgeMock {
        fn sample_falling_edges(&self, _window: Duration) -> std::io::Result<u32> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "injected tach failure",
            ))
        }
    }

    #[cfg(panic = "unwind")]
    struct PanickingEdgeMock;
    #[cfg(panic = "unwind")]
    impl FanTachSource for PanickingEdgeMock {
        fn sample_falling_edges(&self, _window: Duration) -> std::io::Result<u32> {
            panic!("injected tach sampler panic")
        }
    }

    struct ConcurrentEdgeMock {
        active: Arc<std::sync::atomic::AtomicUsize>,
        maximum: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FanTachSource for ConcurrentEdgeMock {
        fn sample_falling_edges(&self, _window: Duration) -> std::io::Result<u32> {
            use std::sync::atomic::Ordering;

            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self
                .maximum
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |maximum| {
                    Some(maximum.max(active))
                });
            std::thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(1)
        }
    }

    #[test]
    fn amlogic_hashboard_eeprom_bus_is_zero() {
        //: hashboard
        // EEPROMs are on /dev/i2c-0. PSU is on /dev/i2c-1 at 0x1f and must
        // NOT carry the denylist (PSU enable writes would be blocked).
        assert_eq!(AMLOGIC_HASHBOARD_EEPROM_BUS, 0);
    }

    #[test]
    fn eeprom_denylist_blocks_am3_aml_write() {
        use crate::i2c::I2cBus;
        // Open a devmem-stub bus (no real /dev/i2c-N) so the denylist can
        // be exercised without hardware. Apply the same denylist that the
        // platform wires up at startup.
        let mut bus = I2cBus::open_devmem();
        bus.set_write_denylist(&AMLOGIC_EEPROM_DENYLIST);

        // BHB42xxx-class slots (am2 legacy) and BHB56902-class slots
        // (am3-aml S19K Pro NEW) live in the same 0x50..=0x57 address
        // range — both must refuse writes via the public `write()` API.
        for addr in 0x50u8..=0x57u8 {
            bus.set_slave(addr)
                .expect("set_slave on devmem stub is infallible");
            let res = bus.write(&[0xDE, 0xAD]);
            assert!(
                res.is_err(),
                "am3-aml hashboard EEPROM at 0x{:02X} must REFUSE writes",
                addr
            );
        }
        // Counter must reflect 8 blocked writes.
        assert_eq!(
            bus.blocked_write_count(),
            8,
            "blocked_write_count must increment per refusal across 0x50..=0x57"
        );
    }

    #[test]
    fn amlogic_denylist_allows_psu_and_temp_sensors() {
        // The denylist must be EEPROM-only. PSU PMBus (0x1f), LM75 inlet
        // sensors (0x48..=0x4A), LM75 outlet sensors (0x4C..=0x4E), dsPIC
        // hybrid addresses (0x20..=0x22), and APW PSU (0x10) must remain
        // writable. If a future change extends the denylist to any of
        // these, PSU enable, temperature reads (which need a register-
        // pointer write), and dsPIC voltage commands all break.
        use crate::i2c::I2cBus;
        let mut bus = I2cBus::open_devmem();
        bus.set_write_denylist(&AMLOGIC_EEPROM_DENYLIST);

        for &(addr, label) in &[
            (0x10u8, "APW PSU"),
            (0x1fu8, "PSU PMBus"),
            (0x20u8, "dsPIC hb1"),
            (0x21u8, "dsPIC hb2"),
            (0x22u8, "dsPIC hb3"),
            (0x48u8, "LM75 inlet hb1"),
            (0x49u8, "LM75 inlet hb2"),
            (0x4Au8, "LM75 inlet hb3"),
            (0x4Cu8, "LM75 outlet hb1"),
            (0x4Du8, "LM75 outlet hb2"),
            (0x4Eu8, "LM75 outlet hb3"),
        ] {
            bus.set_slave(addr).expect("set_slave on devmem stub");
            // devmem stub `write` returns Ok for non-denied addresses
            // (the actual MMIO call is a no-op when not on real hardware).
            let res = bus.write(&[0x00]);
            // We only care that the denylist did NOT trip — devmem may
            // still error for other reasons but the error string must
            // not mention "write denylist".
            if let Err(e) = res {
                let msg = format!("{:?}", e);
                assert!(
                    !msg.contains("write denylist"),
                    "{} (0x{:02X}) was incorrectly denied: {}",
                    label,
                    addr,
                    msg
                );
            }
        }
    }

    #[test]
    fn s9_platform_must_not_inherit_amlogic_denylist() {
        // S9 (am1-zynq) registers NO denylist on startup because its
        // 0x55-0x57 are PIC voltage controllers, NOT EEPROMs. Applying
        // AMLOGIC_EEPROM_DENYLIST on S9 would brick PIC writes. This
        // test pins the contract: a fresh I2cBus has an empty denylist
        // and PIC-range writes are NOT denied by default.
        use crate::i2c::I2cBus;
        let mut s9_bus = I2cBus::open_devmem();
        // No denylist registered — simulating S9 platform startup.
        for addr in 0x55u8..=0x57u8 {
            s9_bus.set_slave(addr).expect("set_slave on devmem stub");
            // The denylist gate must not refuse this write — any error
            // from the devmem stub is unrelated to our protection.
            if let Err(e) = s9_bus.write(&[0x00]) {
                let msg = format!("{:?}", e);
                assert!(
                    !msg.contains("write denylist"),
                    "S9 PIC at 0x{:02X} was wrongly denied: {}",
                    addr,
                    msg
                );
            }
        }
        assert_eq!(
            s9_bus.blocked_write_count(),
            0,
            "S9 platform must register zero blocked writes (no denylist active)"
        );
    }

    #[test]
    fn edges_to_rpm_uses_2_pulses_per_revolution_at_1s_window() {
        // 60 falling edges in 1 s, 2 PPR ⇒ 60 * 60 / (1 * 2) = 1800 RPM.
        assert_eq!(edges_to_rpm(60, Duration::from_secs(1)), 1800);
        // 0 edges ⇒ 0 RPM. No positive fallback may hide a stopped fan.
        assert_eq!(edges_to_rpm(0, Duration::from_secs(1)), 0);
        // Very high count (industrial fan at 6000 RPM ≈ 200 edges/s).
        assert_eq!(edges_to_rpm(200, Duration::from_secs(1)), 6000);
    }

    #[test]
    fn edges_to_rpm_handles_short_window() {
        // 30 edges in 0.5 s ⇒ 30 * 60 / (0.5 * 2) = 1800 RPM.
        assert_eq!(edges_to_rpm(30, Duration::from_millis(500)), 1800);
    }

    #[test]
    fn edges_to_rpm_zero_window_does_not_panic() {
        assert_eq!(edges_to_rpm(0, Duration::from_secs(0)), 0);
        assert_eq!(edges_to_rpm(100, Duration::from_secs(0)), 0);
    }

    #[test]
    fn tach_poll_classifier_accepts_kernfs_pri_err_but_rejects_bare_errors() {
        assert_eq!(tach_poll_has_edge(libc::POLLPRI).unwrap(), true);
        assert_eq!(
            tach_poll_has_edge(libc::POLLPRI | libc::POLLERR).unwrap(),
            true,
            "kernfs valid sysfs notifications carry POLLPRI|POLLERR"
        );
        assert_eq!(tach_poll_has_edge(0).unwrap(), false);
        for revents in [libc::POLLERR, libc::POLLHUP, libc::POLLNVAL] {
            assert!(
                tach_poll_has_edge(revents).is_err(),
                "revents=0x{revents:04X} must invalidate the tach sample"
            );
        }
    }

    #[test]
    fn amlogic_fan_reports_each_measured_tach_channel() {
        // With sample_window = 10 ms and 2 PPR, N edges produces N * 3000 RPM.
        let sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] = [
            Box::new(FixedEdgeMock(20)),
            Box::new(FixedEdgeMock(10)),
            Box::new(FixedEdgeMock(5)),
            Box::new(FixedEdgeMock(2)),
        ];
        let fan = AmlogicFan::for_test(sources);

        assert!(fan.tach_available());

        // Sample window is 10 ms in the test constructor.
        let window = Duration::from_millis(10);
        let per_fan = fan.get_per_fan_rpm();
        assert_eq!(per_fan.len(), 4, "S21/S19j Pro Amlogic has 4 fan slots");
        assert_eq!(per_fan[0], (0, edges_to_rpm(20, window)));
        assert_eq!(per_fan[1], (1, edges_to_rpm(10, window)));
        assert_eq!(per_fan[2], (2, edges_to_rpm(5, window)));
        assert_eq!(per_fan[3], (3, edges_to_rpm(2, window)));
    }

    #[test]
    fn amlogic_zero_tach_dominates_aggregate_rpm() {
        let sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] = [
            Box::new(FixedEdgeMock(20)),
            Box::new(FixedEdgeMock(10)),
            Box::new(FixedEdgeMock(0)),
            Box::new(FixedEdgeMock(5)),
        ];
        let fan = AmlogicFan::for_test(sources);

        assert_eq!(fan.get_per_fan_rpm()[2], (2, 0));
        assert_eq!(fan.get_rpm(), 0, "a stopped fan must not be filtered out");
    }

    #[test]
    fn amlogic_tach_sample_error_revokes_runtime_availability() {
        let sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] = [
            Box::new(FixedEdgeMock(20)),
            Box::new(FailingEdgeMock),
            Box::new(FixedEdgeMock(10)),
            Box::new(FixedEdgeMock(5)),
        ];
        let fan = AmlogicFan::for_test(sources);

        assert!(fan.tach_available());
        let readings = fan.get_per_fan_rpm();
        assert_eq!(readings[1], (1, 0));
        assert!(
            !fan.tach_available(),
            "an invalid sample must permanently revoke this owner's evidence"
        );
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn amlogic_tach_sampler_panic_revokes_runtime_availability() {
        let sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] = [
            Box::new(FixedEdgeMock(20)),
            Box::new(PanickingEdgeMock),
            Box::new(FixedEdgeMock(10)),
            Box::new(FixedEdgeMock(5)),
        ];
        let fan = AmlogicFan::for_test(sources);

        assert!(fan.tach_available());
        let readings = fan.get_per_fan_rpm();
        assert_eq!(readings[1], (1, 0));
        assert!(
            !fan.tach_available(),
            "a panicked sampler must permanently revoke this owner's evidence"
        );
    }

    #[test]
    fn fan_tach_slots_share_one_parallel_observation_window() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sources: [Box<dyn FanTachSource>; GPIO_FAN_TACH_COUNT] = std::array::from_fn(|_| {
            Box::new(ConcurrentEdgeMock {
                active: active.clone(),
                maximum: maximum.clone(),
            }) as Box<dyn FanTachSource>
        });
        let fan = AmlogicFan::for_test(sources);

        assert_eq!(fan.get_per_fan_rpm().len(), 4);
        assert!(
            maximum.load(std::sync::atomic::Ordering::SeqCst) > 1,
            "tach sources must overlap instead of consuming four sequential windows"
        );
    }

    // ─── W2A.2: PIC1704 wire-up regression guards ───

    #[test]
    fn s21_subtype_returns_nopic() {
        // S21 NoPic-class subtypes (e.g. an Amlogic carrier with no
        // BHB42/56 hashboard) classify to NoPic. The sustained-mining
        // s21 unit boots without /etc/subtype, so this guards
        // future BraiinsOS+ images that DO ship one.
        use crate::platform::subtype::classify_voltage_controller;
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB68900")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_S21Pro")),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn bhb56_subtype_returns_dspic() {
        // S19k Pro at .78 (`AMLCtrl_BHB56902`) and any future
        // BHB56xxx-class hashboard must stay on the existing dsPIC33EP
        // path. This is the no-regression guard for the .78 platform.
        use crate::platform::subtype::classify_voltage_controller;
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56902")),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56xxx")),
            VoltageControllerKind::Dspic33Ep,
        );
    }

    #[test]
    fn amlogic_with_config_voltage_controller_is_passthrough() {
        // Any `with_config` call carries voltage_controller from the
        // PlatformConfig. Default builders preserve the existing safe
        // path; only an explicit override re-routes to PIC1704. This
        // is the no-regression guard for s19jpro (sustained-mining
        // unit running existing dsPIC path): merely instantiating
        // AmlogicPlatform from a default config never silently upgrades
        // it to PIC1704.
        let cfg = PlatformConfig::s19k_amlogic();
        assert_eq!(cfg.voltage_controller, VoltageControllerKind::NoPic);
        let p = AmlogicPlatform::with_config(cfg);
        assert_eq!(p.voltage_controller(), VoltageControllerKind::NoPic);

        let cfg = PlatformConfig::s21_amlogic();
        assert_eq!(cfg.voltage_controller, VoltageControllerKind::NoPic);
        let p = AmlogicPlatform::with_config(cfg);
        assert_eq!(p.voltage_controller(), VoltageControllerKind::NoPic);
    }

    /// PSU enable polarity regression guard — GPIO 437 PWR_EN is active HIGH
    /// (1=ON, 0=OFF), per  Q10 (VNish v1.2.7 `S11board` + stock bmminer).
    ///
    /// `enable_psu_gpio()`, `disable_psu()`, and `is_psu_enabled()` write/read
    /// real sysfs paths, so they cannot run on the host. This source-level guard
    /// instead pins the corrected polarity contract: the enable path drives the
    /// pin to `"1"`, the disable path to `"0"`, and the enabled-readback treats
    /// `"1"` as enabled. It exists so a future edit cannot silently re-invert the
    /// HAL back to the pre- active-LOW misreading (EE C1, 2026-05-21).
    /// Mirrors the active-HIGH constant in `gpio_maps.rs::AmlogicGpioMap` and
    /// `vnish_cold_boot.rs::GPIO_PWR_EN`.
    #[test]
    fn psu_enable_is_active_high_437() {
        let src = include_str!("mod.rs");

        // Enable path drives the value file HIGH.
        assert!(
            src.contains("fs::write(&gpio_path, \"1\")"),
            "enable_psu_gpio must write \"1\" to enable (active HIGH, Wave 5 Q10)"
        );
        assert!(
            src.contains("ensure_psu_active_low_disabled_checked()?;"),
            "all GPIO437 mutations must first pin and check raw active-high mode"
        );
        assert!(
            src.contains("fs::write(&dir_path, \"low\")"),
            "enable_psu_gpio must establish a glitch-free LOW before driving HIGH"
        );
        // Disable path drives the value file LOW.
        assert!(
            src.contains("fs::write(&gpio_path, \"0\")"),
            "disable_psu must write \"0\" to disable (active HIGH, Wave 5 Q10)"
        );
        // Readback treats HIGH as enabled.
        assert!(
            src.contains("\"1\" => Ok(true)"),
            "the checked GPIO parser must treat \"1\" (HIGH) as enabled (active HIGH, Wave 5 Q10)"
        );
        // The old active-LOW readback must be gone.
        assert!(
            !src.contains("\"0\" => Ok(true)"),
            "active-LOW readback must not be re-introduced (EE C1 regression guard)"
        );
    }
}
