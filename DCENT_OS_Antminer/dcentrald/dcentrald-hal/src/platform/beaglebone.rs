//! BeagleBone AM335x platform (Antminer S19j Pro BB carrier board).
//!
//! Live-verified from `a lab unit` (Stock Bitmain Antminer S19j Pro Dec 2022,
//! kernel 3.8.13+ #36 SMP, AM335x Cortex-A8 ARMv7, 2026-04-29):
//!
//! - **No FPGA**. ASIC chains driven via mainline `omap-serial` Linux UARTs.
//! - 3-board S19j Pro variant: chains 0/1/2 → `/dev/ttyO1`, `/dev/ttyO2`,
//!   `/dev/ttyO4` (ttyO3 is disabled in the BB device tree).
//! - 4-board SKUs additionally use `/dev/ttyO5` for chain 3.
//! - dsPIC33EP voltage controllers at I²C `0x20/0x21/0x22` on `/dev/i2c-0`
//!   (same chip family as am2 — BHB42601 hashboards have BM1362 chips and
//!   identical PIC topology across both Zynq and BB carrier boards).
//! - 4 fans on PWM sysfs. Stock exposes legacy `/sys/class/pwm/pwm1` and
//!   `pwm2`; LuxOS/DCENT_OS kernels expose `pwmchip0` plus `pwmchip2`.
//! - Pinout (from `/etc/init.d/S70cgminer` on .79, captured verbatim):
//!     - PLUG_DETECT: GPIO 51/48/47/44 (input, active HIGH)
//!     - BOARD_RESET: GPIO 5/4/27/22 (output, default HIGH = running)
//!     - PSU_ENABLE:  GPIO 65 (output HIGH after init)
//!     - LEDs:        GPIO 23 (green), GPIO 45 (red)
//!     - FAN_TACH:    GPIO 7/20/110/112 (real falling-edge counter, W3.3)
//!
//! ## uart_trans.ko bypass
//!
//! Stock Bitmain firmware loads a proprietary `uart_trans.ko` kernel module
//! that wraps `vfs_write` to `/dev/ttyOX` with hardware-timed batched ASIC
//! framing. **DCENT_OS does NOT use it.** Per the RE report
//!, the mainline `omap-serial` driver
//! already provides `/dev/ttyO{0..5}` independently. When stock
//! `bmminer`/`cgminer` is stopped, those `filp_open` references release and
//! the tty devices become available for normal user-space opens. We talk to
//! them directly via [`SerialChain`] + termios + software CRC.
//!
//! ## References
//!
//!
//! - Memory: ,
//!   ,

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use dcentrald_api_types::thermal_model::FanMode;

use super::config::{ChainTransport, PlatformConfig, VoltageControllerKind};
use super::subtype::{
    classify_from_board_target, classify_with_probe, read_subtype, VoltageControllerEndpoint,
};
use super::{BoardType, ChainAccess, FanAccess, FanCommandReceipt, GpioAccess, Platform};
use crate::i2c::I2cBus;
use crate::serial::SerialChain;
use crate::{HalError, Result};

// ─── Pinout — DEV-KIT CANONICAL (cross-checked W2B 2026-05-09) ───
//
// Source-of-truth file (verbatim, captured into the dev-kit tree):
//   `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/DCENT_OS_DEVELOPMENT_KIT/
//    ROOTFS_AM335x/BBCtrl_rootfs/etc/init.d/S70cgminer`
//
// Cross-references:
//   - Live-probed `a lab unit` (stock Bitmain S19j Pro, 2026-04-29). Same values.
//   - `DCENT_OS_GAP_ANALYSIS_FINAL.md` lists CV1835 XGPIOC pins (different
//     hardware family — CVCtrl uses Cvitek SoC GPIOs 425-435). Do NOT
//     conflate the two; this file is AM335x BB only.
//   - `S19J_PRO_PORTING_PLAN.md §12` (HARDWARE ABSTRACTION LAYER).
//
// Earlier RE notes referenced `S37bitmainer_setup` which sometimes shows
// a different ordering. The canonical value-of-record is the BBCtrl
// S70cgminer init script as captured above; that's what bmminer/cgminer
// actually relies on at runtime, and it's what we mirror byte-for-byte.

/// Plug detect GPIOs per chain (0..3). Active HIGH (1 = board present).
///
/// AM335x SoC mapping: PLUG0=GPIO1_19→51, PLUG1=GPIO1_16→48,
/// PLUG2=GPIO1_15→47, PLUG3=GPIO1_12→44. Direction: input.
const GPIO_PLUG_DETECT: [u32; 4] = [51, 48, 47, 44];

/// Board reset GPIOs per chain (0..3). Active LOW (0 = reset asserted, 1 = running).
///
/// AM335x SoC mapping: RST0=GPIO0_5→5, RST1=GPIO0_4→4, RST2=GPIO0_27→27,
/// RST3=GPIO0_22→22. Direction: output, default HIGH (running). NOT the
/// CV1835 `XGPIOC[11/13/15]` (gpio427/429/431) numbers from the gap-analysis
/// doc — those are a different SoC family.
const GPIO_BOARD_RESET: [u32; 4] = [5, 4, 27, 22];

/// PSU enable GPIO. Output HIGH after PSU init. AM335x GPIO2_1 → 65.
const GPIO_PSU_ENABLE: u32 = 65;

/// Recovery button GPIO (W14.A6 — R4-CONFIRMED). AM335x GPIO13_30 → 446.
/// Wired to the front-panel reset/recovery button on the BB carrier;
/// stock bmminer reads this in the maintenance / factory-reset path.
/// Direction: input (active-low when pressed). Source: W4 RE
/// `am335x_board_init.c` GPIO map + Bitmain `s19j_init.h`.
pub const GPIO_RECOVERY_BTN: u32 = 446;

/// LED GPIOs. AM335x mapping per S70cgminer:
/// - GREEN: GPIO0_23 → 23 (NOT CV1835 GPIO435 from gap-analysis).
/// - RED: GPIO1_13 → 45  (NOT CV1835 GPIO434 from gap-analysis).
const GPIO_LED_GREEN: u32 = 23;
const GPIO_LED_RED: u32 = 45;

/// Fan tachometer GPIOs (4 fans). Falling-edge counted by the
/// [`BeagleBoneFanTach`] sampler thread (W3.3).
///
/// S70cgminer naming (with chip line):
/// - FAN_REAR_SPEEP0 (GPIO0_7) → 7
/// - FAN_REAR_SPEEP1 (GPIO0_20) → 20
/// - FAN_FRONT_SPEED1 (GPIO3_14) → 110
/// - FAN_FRONT_SPEED0 (GPIO3_16) → 112
const GPIO_FAN_TACH: [u32; 4] = [7, 20, 110, 112];

/// EEPROM I²C address range on AM335x BB hashboards.
///
/// Mirrors the am2/am3-aml denylist:
/// 0x50..=0x57 are AT24C-class EEPROMs on every BHB42XXX-family hashboard,
/// whether the carrier is Zynq, BB, AML, or CV1835. The .74 hb2 corruption
/// incident (2026-04-29) demonstrated that a single rogue write to one of
/// these addresses can permanently brick the unit. Reads still pass through;
/// only writes are refused at the bus layer.
const BB_HASHBOARD_EEPROM_DENYLIST: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

/// Only active I2C bus on stock Bitmain BB.
const BB_I2C_BUS: u8 = 0;

/// Chain UARTs exposed by the AM335x omap-serial driver.
const BB_CHAIN_UARTS: [&str; 4] = ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"];

/// PWM period for BB fans: 100000 ns = 10 kHz (per S70cgminer init).
const BB_PWM_PERIOD_NS: u32 = 100_000;

// ─── S19J_IO_BOARD_V2_0 pinout — the `a lab unit` unit (Phase B, 2026-05-12) ───
//
// The W4 consts above (`GPIO_PSU_ENABLE=65`, `GPIO_BOARD_RESET=[5,4,27,22]`,
// `GPIO_PLUG_DETECT=[51,48,47,44]`) were reconstructed from a *different* IO
// board (the W4 dev-kit BBCtrl). The first physical `am3-bb` on the fleet
// (`203.0.113.79`, LuxOS, BeagleBone Black v2.1 on `S19J_IO_BOARD_V2_0`)
// has a different GPIO map, captured DTS-labeled from `/sys/kernel/debug/gpio`:
//
//   gpio59  "enable"  → BOARD/PSU ENABLE (NOT gpio65)
//   gpio49  "rst0"    → ASIC RST chain 0
//   gpio60  "rst1"    → ASIC RST chain 1
//   gpio27  "rst2"    → ASIC RST chain 2
//   gpio22  "rst3"    → ASIC RST chain 3 (4th chain — unpopulated on this 3-chain unit)
//   gpio51/48/47/46   → PLUG/board-detect candidates (4 — for 4 chain slots)
//   gpio7/20/110/112  → FAN tachometers ×4 (same as the W4 map)
//   gpio23/gpio45     → LED candidates (red/green — same as the W4 map)
//   gpio4="sda"/gpio5="scl" → bit-banged i2c-gpio bus = /dev/i2c-1 (the APW12 PSU bus)
//
// Source:  ("GPIO map"
// + "I2C topology") + `cold-boot-sequence.md` §0/§1. Defaults below are the
// hardcoded fallback used when `/etc/dcentos/board_targets/am3-bb-s19jpro.toml`
// is absent (e.g. on a LuxOS unit without `/etc/dcentos/`).

/// `a lab unit` board/PSU enable GPIO. DTS label `enable`. Gates hashboard power.
/// luxminer configures sysfs `active_low=false` (direct level). The default
/// cold-boot drives it HIGH = ON; a true-cold `a lab unit` trace still needs to catch
/// the OFF→ON edge directly, so the board-target TOML can flip
/// `[gpio].board_enable_active` if future evidence contradicts this.
pub const GPIO_BOARD_ENABLE_V2_0: u32 = 59;

/// `a lab unit` per-chain ASIC reset GPIOs (rst0/rst1/rst2/rst3). DTS labels
/// `rst0`/`rst1`/`rst2`/`rst3`. Active LOW per the luxminer `set_active_low`
/// strings (write `0` = release / running, `1` = assert reset). On a 3-chain
/// unit, rst3 (gpio22) stays asserted (chain 3 unpopulated).
pub const GPIO_ASIC_RST_V2_0: [u32; 4] = [49, 60, 27, 22];

/// `a lab unit` PLUG/board-detect GPIO candidates (one per chain slot). Order is a
/// best-guess — Phase D ground-truths which is plug0/1/2/3 (a populated
/// chain reads one level, an empty slot the other).
pub const GPIO_PLUG_DETECT_V2_0: [u32; 4] = [51, 48, 47, 46];

/// `a lab unit` fan tachometer GPIOs (4 fans). Same as the W4 map (`GPIO_FAN_TACH`).
pub const GPIO_FAN_TACH_V2_0: [u32; 4] = [7, 20, 110, 112];

/// `a lab unit` LED GPIO candidates (red/green). Same as the W4 map.
pub const GPIO_LED_V2_0: (u32, u32) = (GPIO_LED_GREEN, GPIO_LED_RED);

/// `a lab unit` hashboard-EEPROM I²C bus = bus 0 (`4819c000.i2c`, OMAP HW, 100 kHz).
/// EEPROMs at 0x50/0x51/0x52. The 0x50-0x57 HAL write-deny rule applies here.
pub const I2C_BUS_EEPROM_V2_0: u8 = 0;

/// `a lab unit` PSU I²C bus = bus 1 (bit-banged i2c-gpio on gpio4=SDA/gpio5=SCL).
/// The APW121215f PSU lives at 0x10. Cold-boot must tolerate slower /
/// finickier bit-banged timing than HW I²C.
pub const I2C_BUS_PSU_V2_0: u8 = 1;

/// `a lab unit` PSU I²C slave address.
pub const I2C_ADDR_PSU_V2_0: u8 = 0x10;

/// `a lab unit` chain UART devices (3-chain unit; the 4th-chain UART would be
/// `/dev/ttyS5` @ 0x481a_a000). chain0→ttyS1, chain1→ttyS2, chain2→ttyS4.
pub const CHAIN_UARTS_V2_0: [&str; 3] = ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4"];

/// Default board-target name for the `a lab unit`-class unit. Read from
/// `/etc/dcentos/board_target` at runtime; this is the value the TOML
/// loader looks for under `/etc/dcentos/board_targets/<name>.toml`.
pub const DEFAULT_BOARD_TARGET_V2_0: &str = "am3-bb-s19jpro";

/// Exact device-tree model captured from the live LuxOS `a lab unit` unit.
pub const S19J_IO_BOARD_V2_0_DT_MODEL: &str = "BeagleBone_Black_v2.1 on S19J_IO_BOARD_V2_0";

/// Exact compatible token proving the BeagleBone Black AM335x carrier.
pub const S19J_IO_BOARD_V2_0_DT_COMPATIBLE: &str = "ti,am335x-bone-black";

/// Authoritative evidence that permits the S19J_IO_BOARD_V2_0 topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Am3BbIdentityEvidence {
    /// DCENT_OS image-owned board-target marker.
    BoardTargetMarker,
    /// Exact live-captured LuxOS device-tree model + compatible token.
    ExactDeviceTree,
}

/// Where the per-board-target TOML lives on a DCENT_OS rootfs.
pub const BOARD_TARGET_DIR: &str = "/etc/dcentos/board_targets";

/// Path to `/etc/dcentos/board_target` (the file naming the active target).
pub const BOARD_TARGET_NAME_FILE: &str = "/etc/dcentos/board_target";

fn dt_property_is_exact(raw: &[u8], expected: &str) -> bool {
    raw.split(|byte| *byte == 0)
        .next()
        .is_some_and(|value| value == expected.as_bytes())
}

fn dt_compatible_has_exact_token(raw: &[u8], expected: &str) -> bool {
    raw.split(|byte| *byte == 0)
        .any(|value| value == expected.as_bytes())
}

/// Authorize the one production AM3-BeagleBone mining topology.
///
/// An explicit marker is authoritative: it must match exactly and cannot be
/// rescued by unrelated device-tree evidence. With no marker, the exact
/// live-captured LuxOS DT tuple is accepted so reversible host bring-up keeps
/// working. Substring matches are deliberately rejected.
pub fn authorize_am3_bb_identity(
    marker: Option<&str>,
    compatible: &[u8],
    model: &[u8],
) -> Result<Am3BbIdentityEvidence> {
    if let Some(marker) = marker {
        if marker == DEFAULT_BOARD_TARGET_V2_0 {
            return Ok(Am3BbIdentityEvidence::BoardTargetMarker);
        }
        return Err(HalError::Platform(format!(
            "BeagleBone: explicit board-target marker {marker:?} does not authorize {}",
            DEFAULT_BOARD_TARGET_V2_0
        )));
    }

    if dt_compatible_has_exact_token(compatible, S19J_IO_BOARD_V2_0_DT_COMPATIBLE)
        && dt_property_is_exact(model, S19J_IO_BOARD_V2_0_DT_MODEL)
    {
        return Ok(Am3BbIdentityEvidence::ExactDeviceTree);
    }

    Err(HalError::Platform(format!(
        "BeagleBone: {} identity not proven by an exact board-target marker or exact device-tree tuple",
        DEFAULT_BOARD_TARGET_V2_0
    )))
}

fn am3_bb_dspic_slot_for_uart(serial_device: &str) -> Option<usize> {
    match serial_device {
        "/dev/ttyS1" | "/dev/ttyO1" => Some(0),
        "/dev/ttyS2" | "/dev/ttyO2" => Some(1),
        "/dev/ttyS4" | "/dev/ttyO4" => Some(2),
        _ => None,
    }
}

fn bind_am3_bb_dspic_endpoint_from_observations(
    marker: Option<&str>,
    compatible: &[u8],
    model: &[u8],
    serial_device: &str,
    observed_address: u8,
    eeprom_bytes_by_slot: &[Option<Vec<u8>>],
    firmware_reply: &[u8],
) -> Result<VoltageControllerEndpoint> {
    authorize_am3_bb_identity(marker, compatible, model)?;

    let slot = am3_bb_dspic_slot_for_uart(serial_device).ok_or_else(|| {
        HalError::Platform(format!(
            "AM3-BB dsPIC endpoint UART is outside the exact S19J_IO_BOARD_V2_0 topology: {serial_device}"
        ))
    })?;
    let address = 0x20u8 + slot as u8;
    if observed_address != address {
        return Err(HalError::Platform(format!(
            "AM3-BB dsPIC observation address 0x{observed_address:02X} does not match {serial_device} endpoint 0x{address:02X}"
        )));
    }

    let eeprom = eeprom_bytes_by_slot
        .get(slot)
        .and_then(Option::as_deref)
        .ok_or_else(|| {
            HalError::Platform(format!(
                "AM3-BB slot {slot} has no retained pre-energize EEPROM observation"
            ))
        })?;
    if eeprom.len() < 2
        || eeprom.iter().all(|byte| *byte == 0x00)
        || eeprom.iter().all(|byte| *byte == 0xFF)
        || eeprom[..2] != [0x04, 0x11]
    {
        return Err(HalError::Platform(format!(
            "AM3-BB slot {slot} EEPROM is blank, short, or outside the live-proven BHB42 family"
        )));
    }

    // The exact `a lab unit` S19J_IO_BOARD_V2_0 trace proves fw=0x89 on all three
    // hashboard controllers. Accept only reply shapes already admitted by the
    // legacy direct-serial parser; this issuer must not broaden wire grammar.
    // Several distinct BHB56 firmware-reply framings all attest fw 0x89; keeping
    // them as separate arms documents each recognized shape.
    #[allow(clippy::if_same_then_else)]
    let firmware = if firmware_reply.len() >= 3
        && firmware_reply[0] == 0x05
        && firmware_reply[1] == 0x17
        && firmware_reply[2] == 0x89
    {
        0x89
    } else if firmware_reply.len() >= 3 && firmware_reply[0] == 0x17 && firmware_reply[2] == 0x89 {
        0x89
    } else if firmware_reply.first() == Some(&0x89) {
        0x89
    } else {
        return Err(HalError::Platform(format!(
            "AM3-BB dsPIC endpoint did not retain an exact supported fw=0x89 GET_VERSION reply: {firmware_reply:02X?}"
        )));
    };

    Ok(VoltageControllerEndpoint::from_observed_am2(
        address, firmware,
    ))
}

/// Reuse the standard daemon's existing EEPROM and GET_VERSION observations
/// to issue one exact `a lab unit` AM3-BB dsPIC endpoint.
///
/// This function performs no I2C access. It reads only system-owned identity
/// files, then binds the already-retained observations to the live-captured
/// S19J_IO_BOARD_V2_0 UART/slot/address topology. `Ok(None)` means the current
/// system is not AM3-BB; once exact AM3-BB identity is present, any topology,
/// EEPROM, or firmware mismatch fails closed instead of falling back to a raw
/// address constructor.
pub fn try_bind_system_am3_bb_dspic_endpoint(
    serial_device: &str,
    observed_address: u8,
    eeprom_bytes_by_slot: &[Option<Vec<u8>>],
    firmware_reply: &[u8],
) -> Result<Option<VoltageControllerEndpoint>> {
    let marker = read_active_board_target_name()?;
    let compatible = fs::read("/proc/device-tree/compatible").unwrap_or_default();
    let model = fs::read("/proc/device-tree/model").unwrap_or_default();

    let exact_identity = marker.as_deref() == Some(DEFAULT_BOARD_TARGET_V2_0)
        || (marker.is_none()
            && dt_compatible_has_exact_token(&compatible, S19J_IO_BOARD_V2_0_DT_COMPATIBLE)
            && dt_property_is_exact(&model, S19J_IO_BOARD_V2_0_DT_MODEL));
    if !exact_identity {
        return Ok(None);
    }

    bind_am3_bb_dspic_endpoint_from_observations(
        marker.as_deref(),
        &compatible,
        &model,
        serial_device,
        observed_address,
        eeprom_bytes_by_slot,
        firmware_reply,
    )
    .map(Some)
}

// ─── Board-target TOML schema (Phase B) ───
//
// Deserialized from `/etc/dcentos/board_targets/<name>.toml`. Mirrors
// `DCENT_OS_Antminer/etc/board_target/am3-bb-s19jpro.toml`. Every field is
// optional at the schema layer so host tooling can inspect partial files.
// Runtime admission separately requires the complete `[platform]` identity;
// topology fields may then use the `*_V2_0` catalog defaults. Used by
// `BeagleBonePlatform::run_cold_boot` (Phase C wires the `--am3-bb-mining`
// daemon mode to call it).

/// `[platform]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetPlatformSection {
    #[serde(default)]
    pub board_target: Option<String>,
    #[serde(default)]
    pub soc: Option<String>,
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub io_board: Option<String>,
    /// e.g. `"apw12-uart-tunnel"`. Classified via
    /// [`super::subtype::classify_from_board_target`].
    #[serde(default)]
    pub voltage_controller: Option<String>,
}

fn default_board_enable_active() -> String {
    "high".to_string()
}

/// `[gpio]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetGpioSection {
    #[serde(default = "default_board_enable_gpio_v2")]
    pub board_enable: u32,
    /// `"high"` or `"low"` — the electrical level written to `board_enable`
    /// for "ON". BEST-GUESS `"high"`; Phase D may flip it.
    #[serde(default = "default_board_enable_active")]
    pub board_enable_active: String,
    #[serde(default = "default_asic_rst_v2")]
    pub asic_rst: Vec<u32>,
    #[serde(default = "default_plug_detect_v2")]
    pub plug_detect: Vec<u32>,
    #[serde(default = "default_fan_tach_v2")]
    pub fan_tach: Vec<u32>,
    #[serde(default = "default_led_v2")]
    pub led: Vec<u32>,
}

fn default_board_enable_gpio_v2() -> u32 {
    GPIO_BOARD_ENABLE_V2_0
}
fn default_asic_rst_v2() -> Vec<u32> {
    GPIO_ASIC_RST_V2_0.to_vec()
}
fn default_plug_detect_v2() -> Vec<u32> {
    GPIO_PLUG_DETECT_V2_0.to_vec()
}
fn default_fan_tach_v2() -> Vec<u32> {
    GPIO_FAN_TACH_V2_0.to_vec()
}
fn default_led_v2() -> Vec<u32> {
    vec![GPIO_LED_V2_0.0, GPIO_LED_V2_0.1]
}

impl Default for BoardTargetGpioSection {
    fn default() -> Self {
        Self {
            board_enable: GPIO_BOARD_ENABLE_V2_0,
            board_enable_active: default_board_enable_active(),
            asic_rst: GPIO_ASIC_RST_V2_0.to_vec(),
            plug_detect: GPIO_PLUG_DETECT_V2_0.to_vec(),
            fan_tach: GPIO_FAN_TACH_V2_0.to_vec(),
            led: default_led_v2(),
        }
    }
}

/// One `[[uart.chains]]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetUartChain {
    pub index: u8,
    pub device: String,
    #[serde(default)]
    pub base_addr: Option<u64>,
    #[serde(default = "default_uart_base_baud")]
    pub base_baud: u32,
}

fn default_uart_base_baud() -> u32 {
    3_000_000
}
fn default_mining_baud() -> u32 {
    3_000_000
}
fn default_chain_count_v2() -> u8 {
    CHAIN_UARTS_V2_0.len() as u8
}

/// `[uart]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetUartSection {
    #[serde(default = "default_uart_chains_v2")]
    pub chains: Vec<BoardTargetUartChain>,
    #[serde(default = "default_chain_count_v2")]
    pub chain_count: u8,
    /// Fast-baud target after BM1362 register 0x28 handoff.
    ///
    /// LuxOS reports "3 Mbaud" on AM335x. The OMAP UART's 48 MHz clock gives
    /// `base_baud = 3_000_000`, so 3 Mbaud is the exact divisor-1 setting. A
    /// previous 937500 target rounded to an actual 1 Mbaud on this UART and
    /// left the post-baud BM1362 writes at the wrong speed.
    #[serde(default = "default_mining_baud")]
    pub mining_baud: u32,
}

fn default_uart_chains_v2() -> Vec<BoardTargetUartChain> {
    CHAIN_UARTS_V2_0
        .iter()
        .enumerate()
        .map(|(i, &dev)| BoardTargetUartChain {
            index: i as u8,
            device: dev.to_string(),
            base_addr: None,
            base_baud: 3_000_000,
        })
        .collect()
}

impl Default for BoardTargetUartSection {
    fn default() -> Self {
        Self {
            chains: default_uart_chains_v2(),
            chain_count: default_chain_count_v2(),
            mining_baud: default_mining_baud(),
        }
    }
}

fn default_eeprom_bus_v2() -> u8 {
    I2C_BUS_EEPROM_V2_0
}
fn default_psu_bus_v2() -> u8 {
    I2C_BUS_PSU_V2_0
}
fn default_psu_addr_v2() -> u8 {
    I2C_ADDR_PSU_V2_0
}
fn default_eeprom_addrs_v2() -> Vec<u8> {
    vec![0x50, 0x51, 0x52]
}
fn default_psu_kind_v2() -> String {
    "apw12-uart-tunnel".to_string()
}

/// `[i2c]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetI2cSection {
    #[serde(default = "default_eeprom_bus_v2")]
    pub eeprom_bus: u8,
    #[serde(default = "default_eeprom_addrs_v2")]
    pub eeprom_addrs: Vec<u8>,
    #[serde(default = "default_true")]
    pub eeprom_write_deny: bool,
    #[serde(default = "default_psu_bus_v2")]
    pub psu_bus: u8,
    #[serde(default = "default_psu_addr_v2")]
    pub psu_addr: u8,
    #[serde(default = "default_psu_kind_v2")]
    pub psu_kind: String,
}

fn default_true() -> bool {
    true
}

impl Default for BoardTargetI2cSection {
    fn default() -> Self {
        Self {
            eeprom_bus: I2C_BUS_EEPROM_V2_0,
            eeprom_addrs: default_eeprom_addrs_v2(),
            eeprom_write_deny: true,
            psu_bus: I2C_BUS_PSU_V2_0,
            psu_addr: I2C_ADDR_PSU_V2_0,
            psu_kind: default_psu_kind_v2(),
        }
    }
}

fn default_gpio59_settle_ms() -> u64 {
    3000
}
fn default_asic_rst_stagger_ms() -> u64 {
    10
}
fn default_asic_rst_settle_ms() -> u64 {
    1100
}
fn default_asic_rst_retry_chain() -> Option<u8> {
    Some(1)
}
fn default_asic_rst_retry_pulses() -> u8 {
    2
}
fn default_asic_rst_retry_assert_ms() -> u64 {
    200
}
fn default_asic_rst_retry_release_ms() -> u64 {
    100
}
fn default_apw12_open_core_mv() -> u16 {
    15000
}
fn default_apw12_steady_mv() -> u16 {
    13800
}
fn default_initial_freq_mhz() -> u32 {
    400
}
fn default_fan_boot_pwm() -> u8 {
    10
}
fn default_fan_max_pwm() -> u8 {
    30
}

/// `[cold_boot]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardTargetColdBootSection {
    /// PIC1704 enable path → false. AM3 BB uses fw=0x89 dsPIC controllers,
    /// handled by the AM3 mining path rather than this PIC1704 knob.
    #[serde(default)]
    pub enable_pic1704_dc_dc: bool,
    /// PIC1704 heartbeat path → false; AM3 BB mining owns dsPIC heartbeat.
    #[serde(default)]
    pub run_pic_heartbeat: bool,
    /// `0x18 = 0x00C100B0` ×3 — chip-side BM1362, used on this carrier.
    /// Passed through to the BM1362 chip-side init by the caller.
    #[serde(default = "default_true")]
    pub run_miscctrl_triple_write: bool,
    /// ~15.0 V chain rail during open-core. BEST-GUESS.
    #[serde(default = "default_apw12_open_core_mv")]
    pub apw12_rail_open_core_mv: u16,
    /// ~13.8 V chain rail steady. BEST-GUESS.
    #[serde(default = "default_apw12_steady_mv")]
    pub apw12_rail_steady_mv: u16,
    #[serde(default = "default_gpio59_settle_ms")]
    pub gpio59_settle_ms: u64,
    #[serde(default = "default_asic_rst_stagger_ms")]
    pub asic_rst_stagger_ms: u64,
    #[serde(default = "default_asic_rst_settle_ms")]
    pub asic_rst_settle_ms: u64,
    /// Optional post-release reset retry captured from the `a lab unit` LuxOS ftrace.
    /// Default: chain 1, two active-low reset pulses.
    #[serde(default = "default_asic_rst_retry_chain")]
    pub asic_rst_retry_chain: Option<u8>,
    #[serde(default = "default_asic_rst_retry_pulses")]
    pub asic_rst_retry_pulses: u8,
    #[serde(default = "default_asic_rst_retry_assert_ms")]
    pub asic_rst_retry_assert_ms: u64,
    #[serde(default = "default_asic_rst_retry_release_ms")]
    pub asic_rst_retry_release_ms: u64,
    #[serde(default = "default_initial_freq_mhz")]
    pub initial_freq_mhz: u32,
    #[serde(default = "default_fan_boot_pwm")]
    pub fan_boot_pwm: u8,
    #[serde(default = "default_fan_max_pwm")]
    pub fan_max_pwm: u8,
}

impl Default for BoardTargetColdBootSection {
    fn default() -> Self {
        Self {
            enable_pic1704_dc_dc: false,
            run_pic_heartbeat: false,
            run_miscctrl_triple_write: true,
            apw12_rail_open_core_mv: default_apw12_open_core_mv(),
            apw12_rail_steady_mv: default_apw12_steady_mv(),
            gpio59_settle_ms: default_gpio59_settle_ms(),
            asic_rst_stagger_ms: default_asic_rst_stagger_ms(),
            asic_rst_settle_ms: default_asic_rst_settle_ms(),
            asic_rst_retry_chain: default_asic_rst_retry_chain(),
            asic_rst_retry_pulses: default_asic_rst_retry_pulses(),
            asic_rst_retry_assert_ms: default_asic_rst_retry_assert_ms(),
            asic_rst_retry_release_ms: default_asic_rst_retry_release_ms(),
            initial_freq_mhz: default_initial_freq_mhz(),
            fan_boot_pwm: default_fan_boot_pwm(),
            fan_max_pwm: default_fan_max_pwm(),
        }
    }
}

/// Full deserialized `/etc/dcentos/board_targets/<name>.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeagleBoneBoardTarget {
    #[serde(default)]
    pub platform: Option<BoardTargetPlatformSection>,
    #[serde(default)]
    pub gpio: BoardTargetGpioSection,
    #[serde(default)]
    pub uart: BoardTargetUartSection,
    #[serde(default)]
    pub i2c: BoardTargetI2cSection,
    #[serde(default)]
    pub cold_boot: BoardTargetColdBootSection,
}

impl BeagleBoneBoardTarget {
    /// The hardcoded `a lab unit` defaults — identical to what
    /// `am3-bb-s19jpro.toml` declares. Used as the fallback when the TOML
    /// file is absent (LuxOS units don't have `/etc/dcentos/`).
    pub fn hardcoded_v2_0_defaults() -> Self {
        Self::default()
    }

    /// `true` if `board_enable_active` says HIGH = ON (the default).
    pub fn board_enable_active_high(&self) -> bool {
        !self
            .gpio
            .board_enable_active
            .trim()
            .eq_ignore_ascii_case("low")
    }

    /// Classify the voltage-controller from the `psu_kind` /
    /// `voltage_controller` strings. The `a lab unit` board-target now prefers the
    /// explicit `dspic33ep-fw89`; stale configs with `apw12-uart-tunnel` still
    /// classify to [`VoltageControllerKind::Dspic33Ep`] because LuxOS ftrace
    /// proved hashboard controllers on bus 0.
    pub(crate) fn classify_voltage_controller(&self) -> Option<VoltageControllerKind> {
        // Prefer the explicit `[platform].voltage_controller`, then the
        // `[i2c].psu_kind`.
        if let Some(vc) = self
            .platform
            .as_ref()
            .and_then(|p| p.voltage_controller.as_deref())
            .and_then(classify_from_board_target)
        {
            return Some(vc);
        }
        classify_from_board_target(&self.i2c.psu_kind)
    }
}

/// Load `/etc/dcentos/board_targets/<name>.toml` for `name`. Returns the
/// parsed struct, or `Ok(None)` if the file is absent. Unreadable or malformed
/// explicit configuration is an error and never falls back to a topology.
///
/// Side-effect-free apart from a `tracing` line and the file read; safe to
/// call from `BeagleBonePlatform::new`. On non-Linux hosts the path won't
/// exist, so this returns `None` — test harness uses
/// [`parse_board_target_toml`] directly with a string.
pub fn load_board_target(name: &str) -> Result<Option<BeagleBoneBoardTarget>> {
    let path = format!("{}/{}.toml", BOARD_TARGET_DIR, name);
    match fs::read_to_string(&path) {
        Ok(s) => match parse_board_target_toml(&s) {
            Ok(bt) => {
                tracing::info!(
                    path = %path,
                    board_target = %name,
                    "BeagleBone: loaded board-target config"
                );
                Ok(Some(bt))
            }
            Err(e) => Err(HalError::Platform(format!(
                "BeagleBone: board-target TOML {path} is malformed: {e}"
            ))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                path = %path,
                "BeagleBone: board-target config is absent"
            );
            Ok(None)
        }
        Err(e) => Err(HalError::Platform(format!(
            "BeagleBone: cannot read board-target TOML {path}: {e}"
        ))),
    }
}

/// Parse a board-target TOML string into [`BeagleBoneBoardTarget`].
/// Host-safe; used by [`load_board_target`] and by unit tests with a
/// `tempfile`-less in-memory string.
pub fn parse_board_target_toml(s: &str) -> Result<BeagleBoneBoardTarget> {
    toml::from_str::<BeagleBoneBoardTarget>(s)
        .map_err(|e| HalError::Platform(format!("board-target TOML parse error: {}", e)))
}

/// Read `/etc/dcentos/board_target` (the file naming the active target).
/// Returns `Ok(None)` when absent. Empty or unreadable explicit markers are
/// errors; this function never invents a board identity.
pub fn read_active_board_target_name() -> Result<Option<String>> {
    match fs::read_to_string(BOARD_TARGET_NAME_FILE) {
        Ok(s) => {
            let t = s.trim();
            if t.is_empty() {
                Err(HalError::Platform(format!(
                    "BeagleBone: {BOARD_TARGET_NAME_FILE} is present but empty"
                )))
            } else {
                Ok(Some(t.to_string()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HalError::Platform(format!(
            "BeagleBone: cannot read {BOARD_TARGET_NAME_FILE}: {e}"
        ))),
    }
}

fn validate_supported_board_target(name: &str, target: &BeagleBoneBoardTarget) -> Result<()> {
    if name != DEFAULT_BOARD_TARGET_V2_0 {
        return Err(HalError::Platform(format!(
            "BeagleBone: unsupported board-target config {name:?}"
        )));
    }
    let platform = target.platform.as_ref().ok_or_else(|| {
        HalError::Platform("BeagleBone: board-target TOML is missing [platform] identity".into())
    })?;
    let declarations = [
        (
            "board_target",
            platform.board_target.as_deref(),
            DEFAULT_BOARD_TARGET_V2_0,
        ),
        ("soc", platform.soc.as_deref(), "am335x"),
        ("cpu", platform.cpu.as_deref(), "cortex-a8"),
        (
            "io_board",
            platform.io_board.as_deref(),
            "S19J_IO_BOARD_V2_0",
        ),
        (
            "voltage_controller",
            platform.voltage_controller.as_deref(),
            "dspic33ep-fw89",
        ),
    ];
    for (field, observed, expected) in declarations {
        if observed != Some(expected) {
            return Err(HalError::Platform(format!(
                "BeagleBone: board-target [platform].{field} must be exactly {expected:?}, got {observed:?}"
            )));
        }
    }
    match target
        .gpio
        .board_enable_active
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "high" => {}
        "low" => {
            return Err(HalError::Platform(
                "BeagleBone: the supported AM3-BB target requires active-high board-enable; active-low output setup is refused until the GPIO backend can establish an OFF level without a direction-change glitch"
                    .into(),
            ));
        }
        observed => {
            return Err(HalError::Platform(format!(
                "BeagleBone: board-target [gpio].board_enable_active must be exactly \"high\" or \"low\", got {observed:?}"
            )));
        }
    }
    if target.uart.chain_count != 3
        || target.uart.chains.len() != 3
        || !target
            .uart
            .chains
            .iter()
            .enumerate()
            .all(|(index, chain)| usize::from(chain.index) == index)
    {
        return Err(HalError::Platform(format!(
            "BeagleBone: {} UART geometry must declare exactly chains 0,1,2; got chain_count={} entries={:?}",
            DEFAULT_BOARD_TARGET_V2_0,
            target.uart.chain_count,
            target
                .uart
                .chains
                .iter()
                .map(|chain| chain.index)
                .collect::<Vec<_>>()
        )));
    }
    Ok(())
}

// ─── Fan tach (W3.3) ───
//
// Falling-edge tachometer on GPIO 7/20/110/112 was previously a TODO; the
// fan code synthesized RPM from PWM (`900 + (pwm * 40)`), which masked any
// real fan failure on am3-bb. W3.3 replaces that with a real edge counter.
//
// Stock Bitmain BB ships kernel 3.8.13 (.79 capture). The `/dev/gpiochip*`
// chardev v1 ABI is present in 3.8 but the AM335x DTB Bitmain ships does
// NOT enable interrupts on the tach lines. We therefore implement a polling
// sysfs sampler that counts 1→0 transitions over a sliding 1-second window.

/// Sampler tick interval (2 ms = 500 Hz polling).
const FAN_TACH_SAMPLE_INTERVAL: Duration = Duration::from_millis(2);

/// Sliding-window length for RPM averaging (1 s).
const FAN_TACH_WINDOW: Duration = Duration::from_millis(1000);

/// Pulses per revolution. 4-wire PC fans emit 2 pulses per revolution.
const FAN_TACH_PULSES_PER_REV: u32 = 2;

/// Maximum orderly shutdown budget before a wedged sysfs reader is detached
/// in an explicitly unavailable state instead of blocking daemon teardown.
const FAN_TACH_STOP_TIMEOUT: Duration = Duration::from_millis(250);

/// Lab-only override env flag. When set to `1`, the runtime accepts
/// `FanMode::Advanced` / `FanMode::HashrateMax` despite the W3.3
/// "real-tach but uncalibrated on this board" gate. Mirrors the
/// `DCENT_AM2_TRUST_DEGRADED_FW` convention from
/// `dcentrald-asic::dspic::DSPIC_FW86_TRUST_DEGRADED_ENV`.
pub const BB_TACH_ACCEPT_DEGRADED_ENV: &str = "DCENT_AM3_BB_ACCEPT_DEGRADED_TACH";

// ─── Platform ───

/// BeagleBone AM335x platform.
pub struct BeagleBonePlatform {
    config: PlatformConfig,
    /// Authorized board-target name. Runtime construction requires an exact
    /// marker or exact live-captured device-tree identity.
    board_target_name: String,
    /// Parsed target, or built-in `a lab unit` topology only for exact LuxOS DT evidence.
    board_target: BeagleBoneBoardTarget,
}

impl BeagleBonePlatform {
    /// Create a new BeagleBone platform.
    ///
    /// **Side-effect-free apart from a `tracing` line.** No GPIO export, no
    /// I²C open, no cold-boot — the cold-boot sequence is opt-in via
    /// [`Self::run_cold_boot`] (Phase C wires the `--am3-bb-mining` daemon
    /// mode to call it).
    pub fn new() -> Result<Self> {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let is_am335x =
            cpuinfo.contains("AM33XX") || cpuinfo.contains("AM335x") || cpuinfo.contains("am33xx");
        if !is_am335x {
            return Err(HalError::Platform(
                "BeagleBone: /proc/cpuinfo does not contain AM33XX hardware string".to_string(),
            ));
        }

        // Accept either the `a lab unit` LuxOS naming (`/dev/ttyS1`, mainline
        // omap-serial) OR the W4-era naming (`/dev/ttyO1`). At least one
        // chain-0 UART device node must be present.
        if !Path::new("/dev/ttyS1").exists() && !Path::new("/dev/ttyO1").exists() {
            return Err(HalError::Platform(
                "BeagleBone: neither /dev/ttyS1 nor /dev/ttyO1 found — omap-serial not loaded?"
                    .to_string(),
            ));
        }

        let mut config = PlatformConfig::s19j_beaglebone();

        // Resolve identity before accepting any GPIO/UART/I2C topology. DCENT_OS
        // uses the exact image-owned marker; reversible LuxOS bring-up may use
        // the exact live-captured DT tuple. Missing evidence never manufactures
        // `am3-bb-s19jpro`.
        let marker = read_active_board_target_name()?;
        let compatible = fs::read("/proc/device-tree/compatible").unwrap_or_default();
        let dt_model = fs::read("/proc/device-tree/model").unwrap_or_default();
        let identity = authorize_am3_bb_identity(marker.as_deref(), &compatible, &dt_model)?;
        let board_target_name = DEFAULT_BOARD_TARGET_V2_0.to_string();
        let board_target = match load_board_target(&board_target_name)? {
            Some(target) => {
                validate_supported_board_target(&board_target_name, &target)?;
                target
            }
            None if identity == Am3BbIdentityEvidence::ExactDeviceTree => {
                tracing::info!(
                    board_target = %board_target_name,
                    "BeagleBone: exact LuxOS device-tree evidence authorizes built-in .79 topology"
                );
                BeagleBoneBoardTarget::hardcoded_v2_0_defaults()
            }
            None => {
                return Err(HalError::Platform(format!(
                    "BeagleBone: exact marker {} requires its board-target TOML; refusing hardcoded fallback",
                    DEFAULT_BOARD_TARGET_V2_0
                )))
            }
        };

        // Voltage-controller classification. Two routes:
        //   1. `/etc/subtype` + `i2cdetect 0x20` probe (W2A.2 — the W4
        //      BBCtrl dev-kit path: BBCtrl_BHB42XXX → PIC1704 if 0x20 ACKs).
        //   2. board-target `psu_kind` / `voltage_controller` string. The
        //      `a lab unit` S19J_IO_BOARD_V2_0 path is APW12 upstream PSU plus
        //      fw=0x89 dsPIC hashboard controllers on bus 0.
        // The board-target classification takes precedence when it
        // recognizes the PSU-kind string; otherwise we fall through to the
        // subtype/probe path, which fails closed to NoPic when identity is
        // missing or contradictory.
        let kind = if let Some(vc) = board_target.classify_voltage_controller() {
            tracing::info!(
                board_target = %board_target_name,
                voltage_controller = vc.as_str(),
                "BeagleBone: voltage controller from board-target config"
            );
            vc
        } else {
            let subtype = read_subtype();
            let k = classify_with_probe(subtype.as_deref(), BB_I2C_BUS);
            tracing::info!(
                subtype = %subtype.as_deref().unwrap_or("<missing>"),
                voltage_controller = k.as_str(),
                "BeagleBone: voltage controller from /etc/subtype + probe (no board-target override)"
            );
            k
        };
        config.voltage_controller = kind;
        tracing::info!(
            platform = %config.name,
            chains = config.chains.len(),
            board_target = %board_target_name,
            voltage_controller = kind.as_str(),
            psu_bus = board_target.i2c.psu_bus,
            eeprom_bus = board_target.i2c.eeprom_bus,
            "BeagleBone AM335x platform initialized"
        );

        Ok(Self {
            config,
            board_target_name,
            board_target,
        })
    }

    /// Create with explicit config (for testing or operator override).
    /// Uses the hardcoded `a lab unit` board-target defaults.
    pub fn with_config(config: PlatformConfig) -> Self {
        Self {
            config,
            board_target_name: DEFAULT_BOARD_TARGET_V2_0.to_string(),
            board_target: BeagleBoneBoardTarget::hardcoded_v2_0_defaults(),
        }
    }

    /// Create with explicit config + board-target (for testing).
    pub fn with_config_and_board_target(
        config: PlatformConfig,
        board_target_name: impl Into<String>,
        board_target: BeagleBoneBoardTarget,
    ) -> Self {
        Self {
            config,
            board_target_name: board_target_name.into(),
            board_target,
        }
    }

    /// The active board-target name (`am3-bb-s19jpro` by default).
    pub fn board_target_name(&self) -> &str {
        &self.board_target_name
    }

    /// The loaded board-target config (or the hardcoded `a lab unit` defaults).
    pub fn board_target(&self) -> &BeagleBoneBoardTarget {
        &self.board_target
    }

    /// Number of physically populated chains, derived from plug-detect GPIOs.
    pub fn populated_chain_count(&self) -> u8 {
        let gpio = BeagleBoneGpio::new();
        let detect_4 = gpio.read_plug_detect_4();
        detect_4.iter().filter(|&&p| p).count() as u8
    }

    // ── Legacy W4-BBCtrl accessors (unchanged — referenced by the W4
    //    `beaglebone_cold_boot::cold_boot_sequence` path + its tests). The
    //    `a lab unit` S19J_IO_BOARD_V2_0 unit uses the `_v2_0` accessors / the
    //    board-target config instead. ──

    /// W4 BBCtrl I²C bus (the W4 dev-kit board). `a lab unit` uses
    /// [`Self::eeprom_i2c_bus`] / [`Self::psu_i2c_bus`].
    pub const fn i2c_bus_number() -> u8 {
        BB_I2C_BUS
    }

    /// W4 BBCtrl chain UART device names. `a lab unit` uses the board-target
    /// config (`/dev/ttyS1` / `/dev/ttyS2` / `/dev/ttyS4`).
    pub const fn chain_uarts() -> [&'static str; 4] {
        BB_CHAIN_UARTS
    }

    /// W4 BBCtrl plug-detect GPIOs (`[51,48,47,44]`). `a lab unit` uses
    /// [`Self::chain_plug_gpios_v2_0`] (`[51,48,47,46]`).
    pub const fn chain_plug_gpios() -> [u32; 4] {
        GPIO_PLUG_DETECT
    }

    /// W4 BBCtrl per-chain reset GPIOs (`[5,4,27,22]` — S70cgminer capture).
    /// `a lab unit` uses [`Self::chain_reset_gpios_v2_0`] (`[49,60,27,22]` —
    /// `S19J_IO_BOARD_V2_0` DTS labels rst0/rst1/rst2/rst3).
    pub const fn chain_reset_gpios() -> [u32; 4] {
        GPIO_BOARD_RESET
    }

    /// W4 BBCtrl PSU-enable GPIO (`65`). `a lab unit` uses the `enable` GPIO
    /// (`59` — [`Self::board_enable_gpio_v2_0`]).
    pub const fn psu_enable_gpio() -> u32 {
        GPIO_PSU_ENABLE
    }

    /// W14.A6 (R4-CONFIRMED): recovery / front-panel reset button GPIO.
    /// AM335x GPIO13_30 → 446. Input direction. Stock bmminer polls this
    /// for the maintenance / factory-reset hold-down path.
    pub const fn recovery_btn_gpio() -> u32 {
        GPIO_RECOVERY_BTN
    }

    pub const fn led_gpios() -> (u32, u32) {
        (GPIO_LED_GREEN, GPIO_LED_RED)
    }

    pub const fn fan_tach_gpios() -> [u32; 4] {
        GPIO_FAN_TACH
    }

    pub const fn fan_pwm_period_ns() -> u32 {
        BB_PWM_PERIOD_NS
    }

    // ── `a lab unit` S19J_IO_BOARD_V2_0 accessors (Phase B) ──

    /// `a lab unit` board/PSU enable GPIO (DTS label `enable`) — from the loaded
    /// board-target config, default `59`.
    pub fn board_enable_gpio_v2_0(&self) -> u32 {
        self.board_target.gpio.board_enable
    }

    /// `a lab unit` per-chain ASIC reset GPIOs (rst0/rst1/rst2/rst3) — from the
    /// loaded board-target config, default `[49,60,27,22]`.
    pub fn chain_reset_gpios_v2_0(&self) -> Vec<u32> {
        self.board_target.gpio.asic_rst.clone()
    }

    /// `a lab unit` plug-detect GPIO candidates — from the loaded board-target
    /// config, default `[51,48,47,46]`.
    pub fn chain_plug_gpios_v2_0(&self) -> Vec<u32> {
        self.board_target.gpio.plug_detect.clone()
    }

    /// `a lab unit` chain UART device names — from the loaded board-target config,
    /// default `["/dev/ttyS1","/dev/ttyS2","/dev/ttyS4"]`.
    pub fn chain_uarts_v2_0(&self) -> Vec<String> {
        self.board_target
            .uart
            .chains
            .iter()
            .map(|c| c.device.clone())
            .collect()
    }

    /// `a lab unit` hashboard-EEPROM I²C bus — from the loaded board-target
    /// config, default `0`.
    pub fn eeprom_i2c_bus(&self) -> u8 {
        self.board_target.i2c.eeprom_bus
    }

    /// `a lab unit` PSU I²C bus — from the loaded board-target config, default `1`
    /// (the bit-banged i2c-gpio bus on gpio4=SDA/gpio5=SCL).
    pub fn psu_i2c_bus(&self) -> u8 {
        self.board_target.i2c.psu_bus
    }

    /// `a lab unit` PSU I²C slave address — from the loaded board-target config,
    /// default `0x10`.
    pub fn psu_i2c_addr(&self) -> u8 {
        self.board_target.i2c.psu_addr
    }

    /// `a lab unit` fast-baud target after BM1362 FastUART handoff — from the loaded
    /// board-target config, default `3_000_000` (exact AM335x divisor-1 baud).
    pub fn mining_baud_v2_0(&self) -> u32 {
        self.board_target.uart.mining_baud
    }

    /// Run the `a lab unit`-class (`S19J_IO_BOARD_V2_0`) cold-boot sequence.
    ///
    /// Thin wrapper that builds [`super::beaglebone_cold_boot::ColdBootOptsV2`]
    /// from the loaded board-target config and calls
    /// [`super::beaglebone_cold_boot::cold_boot_sequence_s19j_io_v2`]. The
    /// Phase-C `--am3-bb-mining` daemon mode calls this; nothing else does.
    /// `new()` does NOT auto-run it.
    ///
    /// The `pic`/`uarts` arguments are passed through to the cold-boot
    /// function — the daemon constructs the APW UART-tunnel PSU controller
    /// and per-chain UARTs (Phase C). See the cold-boot module docs.
    pub fn run_cold_boot<B: crate::psu_apw_uart_tunnel::ApwUartTunnelBus>(
        &self,
        psu: &mut crate::psu_apw_uart_tunnel::ApwUartTunnel<B>,
        uarts: &mut [crate::serial::DevmemUart],
    ) -> Result<()> {
        let opts =
            super::beaglebone_cold_boot::ColdBootOptsV2::from_board_target(&self.board_target);
        super::beaglebone_cold_boot::cold_boot_sequence_s19j_io_v2(self, psu, uarts, opts)
    }
}

impl Platform for BeagleBonePlatform {
    fn board_type(&self) -> BoardType {
        BoardType::BeagleBone
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

        // BB chain TX/RX always goes through the omap-serial UART
        // (`SerialChain`). The `bitmain_axi.ko` register-shuttle path
        // (mmap-based per RE3 W12.1, IOCTL fallback dev/debug only per
        // W13.B5) is reserved for FPGA register access on stock-FPGA
        // hosts; there is no work-shuttle path on the AM335x BB carrier.
        // The W10 env-gate `DCENT_BB_TRUST_INFERRED_AXI_IOCTL` was
        // retired in W13.B5 (2026-05-10) — the IOCTL ABI is now Cargo-
        // feature-gated (`axi-ioctl-debug`) and not compiled into shipping
        // firmware. See `crate::stock_fpga_axi_mmap::BitmainAxiUnifiedBackend`
        // for the production register-shuttle entry point used by
        // `WorkBackend::AxiBitmain` callers.
        match &chain_config.transport {
            ChainTransport::Serial { device, baud } => {
                let serial = SerialChain::open(device, *baud).map_err(|e| {
                    HalError::Platform(format!(
                        "BeagleBone chain {}: open {} failed ({}). \
                         Is stock cgminer/bmminer still running? \
                         Stop with `/etc/init.d/S70cgminer stop`.",
                        chain_id, device, e
                    ))
                })?;
                Ok(Box::new(BeagleBoneChainAccess {
                    serial: Mutex::new(serial),
                }))
            }
            other => Err(HalError::Platform(format!(
                "unexpected transport for BeagleBone chain {}: {:?}",
                chain_id, other
            ))),
        }
    }

    fn open_i2c(&self, bus: u8) -> Result<I2cBus> {
        // Allowed buses:
        //   - bus 0 (`BB_I2C_BUS` / `eeprom_i2c_bus()`): hashboard EEPROMs
        //     (0x50/0x51/0x52). Always available; the W4 BBCtrl path also
        //     puts the PIC1704 / dsPIC here.
        //   - the board-target PSU bus (`psu_i2c_bus()`, default 1 on the
        //     `a lab unit` S19J_IO_BOARD_V2_0): the bit-banged i2c-gpio bus with
        //     the APW121215f PSU at 0x10. Only allowed when the loaded
        //     board-target declares it (so a stale config can't widen the
        //     surface unexpectedly).
        let eeprom_bus = self.eeprom_i2c_bus();
        let psu_bus = self.psu_i2c_bus();
        if bus != eeprom_bus && bus != psu_bus {
            return Err(HalError::Platform(format!(
                "BeagleBone: /dev/i2c-{} not in the board-target's allowed set \
                 (eeprom_bus={}, psu_bus={})",
                bus, eeprom_bus, psu_bus
            )));
        }
        let mut handle = I2cBus::open(bus)?;
        // W2B (2026-05-09) — defense-in-depth EEPROM write-deny.
        //
        // 0x50..=0x57 are AT24C-class hashboard EEPROMs on every BHB42XXX
        // board, regardless of carrier (Zynq am2, Amlogic am3-aml, BB,
        // CV1835). Mirrors the am2 hybrid path
        // (`spawn_i2c_service_no_register_touch_with_denylist`) and the
        // am3-aml `Amlogic::open_i2c` branch. Per
        //  and the .74 hb2
        // corruption incident (2026-04-29), reads still pass through —
        // only writes are refused at the bus layer.
        //
        // Applied to the EEPROM bus only. The PSU bus has no EEPROM on it,
        // but applying the same denylist there is harmless and keeps the
        // "0x50-0x57 is sacred everywhere" invariant — so register it on
        // both. (`set_write_denylist` is idempotent and read-through.)
        handle.set_write_denylist(&BB_HASHBOARD_EEPROM_DENYLIST);
        Ok(handle)
    }

    fn open_fan(&self) -> Result<Box<dyn FanAccess>> {
        Ok(Box::new(BeagleBoneFan::new()?))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        Ok(Box::new(BeagleBoneGpio::new()))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        // Cached from `new()` / `with_config()`. The daemon reads this
        // and switches between:
        //   - `Pic1704Service` — W4 BBCtrl dev-kit path: subtype
        //     `BBCtrl_BHB42XXX` + 0x20 ACK probe.
        //   - the existing `Pic0x89Service` / dsPIC path — fallback.
        //   - `a lab unit` `S19J_IO_BOARD_V2_0`: APW121215f is controlled through the
        //     UART tunnel on bus 1, while hashboard fw=0x89 dsPIC controllers
        //     at 0x20/0x21/0x22 own per-chain rail enable/heartbeat on bus 0.
        //
        // TODO B2.5: wire daemon-side voltage controller selection.
        //   When `voltage_controller() == VoltageControllerKind::Pic1704`,
        //   the daemon must construct
        //   `dcentrald_asic::pic1704::Pic1704Service::new(
        //       i2c_handle, 0x20,
        //       dcentrald_asic::pic1704::service::platforms::Am335xBbS19jPro,
        //   )` instead of the existing `DspicService::new(i2c_svc, addr)`.
        //   The construction site must be at the daemon layer (not here)
        //   because `dcentrald-hal` does NOT depend on `dcentrald-asic`
        //   (the dependency is the other direction). There are 30+
        //   `DspicService::new` call sites in `daemon.rs` plus
        //   `s19j_hybrid_mining.rs` / `serial_mining.rs` — switching them
        //   in lockstep is out of B2 scope. The platform-layer parts
        //   (subtype detection, runtime probe, classification cache,
        //   `voltage_controller()` accessor, EEPROM denylist) ARE in
        //   place and ready for the daemon-side wiring agent.
        self.config.voltage_controller
    }
}

// ─── Chain access ───

struct BeagleBoneChainAccess {
    serial: Mutex<SerialChain>,
}

impl ChainAccess for BeagleBoneChainAccess {
    fn send_command(&self, data: &[u8]) -> Result<()> {
        let mut s = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("BeagleBone serial mutex poisoned".into()))?;
        s.write_bytes(data)
    }

    fn read_response(&self, buf: &mut [u8]) -> Result<usize> {
        let mut s = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("BeagleBone serial mutex poisoned".into()))?;
        s.read_bytes(buf)
    }

    fn send_work(&self, data: &[u8]) -> Result<()> {
        let mut s = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("BeagleBone serial mutex poisoned".into()))?;
        s.write_bytes(data)
    }

    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize> {
        let mut s = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("BeagleBone serial mutex poisoned".into()))?;
        s.read_bytes(buf)
    }

    fn set_baud(&self, baud: u32) -> Result<()> {
        let mut s = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("BeagleBone serial mutex poisoned".into()))?;
        s.set_baud(baud)
    }

    fn wait_for_nonce(&self) -> Result<()> {
        std::thread::yield_now();
        Ok(())
    }
}

// ─── Fan control ───

/// BeagleBone fan control via sysfs PWM.
///
/// **Tach (W3.3)**: real falling-edge counter on `GPIO 7/20/110/112` via the
/// [`BeagleBoneFanTach`] sampler. The previous synthesized RPM
/// (`900 + pwm * 40`) has been removed — per-fan failure is now detectable.
#[derive(Debug, Clone)]
struct PwmDutyPath {
    label: &'static str,
    path: String,
}

struct BeagleBoneFan {
    duty_paths: Vec<PwmDutyPath>,
    period_ns: u32,
    tach: BeagleBoneFanTach,
}

impl BeagleBoneFan {
    fn new() -> Result<Self> {
        let duty_paths = Self::candidate_duty_paths();

        if !duty_paths.iter().any(|p| Path::new(&p.path).exists()) {
            let candidates = duty_paths
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(HalError::Fan(format!(
                "BeagleBone: no known fan PWM duty path exists ({}) - pwm not exported?",
                candidates
            )));
        }

        // Spin up the per-fan tach sampler. If GPIO export fails (e.g.
        // running unit tests on a non-BB host) the tach degrades to
        // "available=false" so the rest of the runtime still works.
        let tach = BeagleBoneFanTach::start(GPIO_FAN_TACH);

        Ok(Self {
            duty_paths,
            period_ns: BB_PWM_PERIOD_NS,
            tach,
        })
    }

    fn candidate_duty_paths() -> Vec<PwmDutyPath> {
        vec![
            PwmDutyPath {
                label: "front-pwmchip0-pwm0",
                path: "/sys/class/pwm/pwmchip0/pwm0/duty_cycle".to_string(),
            },
            PwmDutyPath {
                label: "front-pwmchip0-pwm1",
                path: "/sys/class/pwm/pwmchip0/pwm1/duty_cycle".to_string(),
            },
            PwmDutyPath {
                label: "rear-pwmchip2-pwm0",
                path: "/sys/class/pwm/pwmchip2/pwm0/duty_cycle".to_string(),
            },
            PwmDutyPath {
                label: "front-legacy-pwm1",
                path: "/sys/class/pwm/pwm1/duty_ns".to_string(),
            },
            PwmDutyPath {
                label: "rear-legacy-pwm2",
                path: "/sys/class/pwm/pwm2/duty_ns".to_string(),
            },
        ]
    }

    fn duty_ns_for_pwm(&self, pwm: u8) -> u32 {
        (u32::from(pwm.min(100)) * self.period_ns) / 100
    }

    /// W3.3 — refuse `FanMode::Advanced` and `FanMode::HashrateMax` on
    /// am3-bb until per-fan tach calibration runs against live hardware.
    /// Operator override: `DCENT_AM3_BB_ACCEPT_DEGRADED_TACH=1`.
    #[allow(dead_code)]
    pub fn check_fan_mode_allowed(mode: FanMode) -> Result<()> {
        Self::check_fan_mode_allowed_with_env(mode, std::env::var(BB_TACH_ACCEPT_DEGRADED_ENV).ok())
    }

    /// Test-friendly version of [`check_fan_mode_allowed`].
    #[allow(dead_code)]
    pub fn check_fan_mode_allowed_with_env(
        mode: FanMode,
        override_env: Option<String>,
    ) -> Result<()> {
        match mode {
            FanMode::QuietHome | FanMode::Home | FanMode::Balanced => Ok(()),
            FanMode::Advanced | FanMode::HashrateMax => {
                let allowed = matches!(override_env.as_deref(), Some("1") | Some("true"));
                if allowed {
                    tracing::warn!(
                        mode = mode.display(),
                        env = BB_TACH_ACCEPT_DEGRADED_ENV,
                        "BeagleBone: fan mode override enabled — running with uncalibrated tach. \
                         Live calibration required before promoting this to default."
                    );
                    Ok(())
                } else {
                    Err(HalError::Fan(format!(
                        "BeagleBone: fan mode {:?} refused on am3-bb until per-fan tach \
                         calibration is live-verified. Set {}=1 for lab override.",
                        mode, BB_TACH_ACCEPT_DEGRADED_ENV
                    )))
                }
            }
        }
    }
}

impl FanAccess for BeagleBoneFan {
    fn set_speed(&self, pwm: u8) {
        if let Err(error) = self.set_speed_checked(pwm) {
            tracing::error!(
                %error,
                "BeagleBone fan PWM command/readback failed"
            );
        }
    }

    fn set_speed_checked(&self, pwm: u8) -> Result<FanCommandReceipt> {
        let duty_ns = self.duty_ns_for_pwm(pwm);
        let duty = duty_ns.to_string();
        let existing_paths = self
            .duty_paths
            .iter()
            .filter(|duty_path| Path::new(&duty_path.path).exists())
            .collect::<Vec<_>>();
        if existing_paths.is_empty() {
            return Err(HalError::Fan(
                "BeagleBone: no fan PWM path remains available for checked command".to_string(),
            ));
        }

        // Every discovered physical command path participates in one command.
        // A partial multi-path write is an error even if another path already
        // reached the requested duty; callers must cut hash power rather than
        // converting partial cooling into positive liveness evidence.
        for duty_path in &existing_paths {
            fs::write(&duty_path.path, &duty).map_err(|error| {
                HalError::Fan(format!(
                    "BeagleBone {} fan PWM write failed at {}: {error}",
                    duty_path.label, duty_path.path
                ))
            })?;
        }
        for duty_path in existing_paths {
            let observed = fs::read_to_string(&duty_path.path).map_err(|error| {
                HalError::Fan(format!(
                    "BeagleBone {} fan PWM readback failed at {}: {error}",
                    duty_path.label, duty_path.path
                ))
            })?;
            let observed = observed.trim().parse::<u32>().map_err(|error| {
                HalError::Fan(format!(
                    "BeagleBone {} fan PWM readback parse failed at {}: {error}",
                    duty_path.label, duty_path.path
                ))
            })?;
            if observed != duty_ns {
                return Err(HalError::Fan(format!(
                    "BeagleBone {} fan PWM readback mismatch at {}: requested {duty_ns}ns, observed {observed}ns",
                    duty_path.label, duty_path.path
                )));
            }
        }

        let observed_pwm = pwm.min(100);
        Ok(FanCommandReceipt {
            requested_pwm: observed_pwm,
            observed_pwm,
        })
    }

    fn get_rpm(&self) -> u32 {
        // Only measured channels participate. Zero is a real observation: it
        // may mean the one-second window is still cold or a fan is stopped.
        // Unavailable channels never become a PWM-derived positive RPM.
        // The aggregate fails unavailable as zero unless all four physical
        // channels are observable; averaging a partial set could mask a dead
        // or disconnected fan.
        let observations = self.tach.read_per_fan_observations();
        if !observations.iter().all(|observation| observation.available) {
            return 0;
        }
        let (sum, count) = observations
            .iter()
            .filter(|observation| observation.available)
            .fold((0_u64, 0_u64), |(sum, count), observation| {
                (sum + u64::from(observation.rpm), count + 1)
            });
        if count == 0 {
            0
        } else {
            (sum / count).min(u64::from(u32::MAX)) as u32
        }
    }

    fn get_speed_pwm(&self) -> u8 {
        let Some(read_path) = self.duty_paths.iter().find(|p| Path::new(&p.path).exists()) else {
            return 0;
        };
        if self.period_ns == 0 {
            return 0;
        }
        fs::read_to_string(&read_path.path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            // Inverse of `set_speed`, which writes duty_ns = (pwm * period_ns) / 100.
            // The readback MUST use the same 0-100 scale (×100/period), NOT the
            // legacy 7-bit ×127 factor — otherwise a commanded PWM 30 reads back
            // as 38 (a ~27% inflation), corrupting telemetry and any "is the fan
            // at its commanded value" verification. Amlogic/CV1835 already use
            // the ×100/period inverse; this aligns BeagleBone with them.
            .map(|duty_ns| (duty_ns.saturating_mul(100) / self.period_ns).min(100) as u8)
            .unwrap_or(0)
    }

    fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        // W3.3 — real per-fan readings from GPIO 7/20/110/112.
        let per_fan = self.tach.read_per_fan_rpm();
        per_fan
            .into_iter()
            .enumerate()
            .map(|(id, rpm)| (id as u8, rpm))
            .collect()
    }

    fn fan_count(&self) -> u8 {
        4
    }

    fn tach_available(&self) -> bool {
        // Real GPIO falling-edge evidence is available only after every
        // channel completes an uninterrupted measurement window.
        self.tach.is_available()
    }
}

// ─── Fan tachometer (W3.3) ───

/// One truthful per-channel tach observation.
///
/// `available=false` and `rpm=0` is distinct from an available channel whose
/// measured pulse window is zero. The public compatibility API still returns
/// RPM values, while `tach_available()` stays false unless every physical
/// channel is observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BeagleBoneFanTachObservation {
    available: bool,
    rpm: u32,
}

/// Atomics shared with the sampler worker. Ownership of the worker itself is
/// deliberately kept out of this `Arc` so cloning sampling state cannot detach
/// or extend the thread lifetime.
struct BeagleBoneFanTachState {
    observations: [AtomicU32; 4],
}

impl BeagleBoneFanTachState {
    const AVAILABLE_FLAG: u32 = 1 << 31;
    const RPM_MASK: u32 = Self::AVAILABLE_FLAG - 1;

    fn new() -> Self {
        Self {
            observations: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    fn mark_all_unavailable(&self) {
        for observation in &self.observations {
            observation.store(0, Ordering::Release);
        }
    }

    fn store_observation(&self, idx: usize, available: bool, rpm: u32) {
        let packed = if available {
            Self::AVAILABLE_FLAG | rpm.min(Self::RPM_MASK)
        } else {
            0
        };
        self.observations[idx].store(packed, Ordering::Release);
    }

    fn observation(&self, idx: usize) -> BeagleBoneFanTachObservation {
        let packed = self.observations[idx].load(Ordering::Acquire);
        let available = packed & Self::AVAILABLE_FLAG != 0;
        BeagleBoneFanTachObservation {
            available,
            rpm: if available {
                packed & Self::RPM_MASK
            } else {
                0
            },
        }
    }
}

/// Clears availability whenever the worker exits, including unwind after a
/// panic. A dead sampler must never leave stale RPM advertised as live.
struct BeagleBoneFanTachExitGuard(Arc<BeagleBoneFanTachState>);

impl Drop for BeagleBoneFanTachExitGuard {
    fn drop(&mut self) {
        self.0.mark_all_unavailable();
    }
}

type BeagleBoneFanTachTask = Box<dyn FnOnce() + Send + 'static>;

/// Per-fan sliding-window tach counter and sole owner of its sampler thread.
/// Readers get lock-free snapshots; dropping the owner requests stop and joins
/// within a fixed budget. A wedged worker is marked unavailable and detached
/// with an error rather than stalling safety teardown indefinitely.
struct BeagleBoneFanTach {
    state: Arc<BeagleBoneFanTachState>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl BeagleBoneFanTach {
    /// Boot the per-fan tach sampler. GPIO preparation or worker-spawn failure
    /// returns an owned, disabled sampler whose observations are unavailable
    /// and zero. Spawn errors are surfaced in logs and never publish readiness.
    fn start(gpio_pins: [u32; 4]) -> Self {
        let value_paths = gpio_pins.map(|gpio| match Self::prepare_gpio_input(gpio) {
            Ok(path) => Some(path),
            Err(err) => {
                tracing::warn!(
                    gpio,
                    error = %err,
                    "BeagleBone fan-tach: GPIO preparation failed; channel unavailable"
                );
                None
            }
        });
        Self::start_prepared(value_paths)
    }

    fn start_prepared(value_paths: [Option<String>; 4]) -> Self {
        Self::start_prepared_with_spawner(value_paths, |task| {
            thread::Builder::new()
                .name("bb-fan-tach".to_string())
                .spawn(task)
        })
    }

    /// Spawner injection keeps failure and ownership tests host-safe without
    /// weakening the production boundary.
    fn start_prepared_with_spawner<F>(value_paths: [Option<String>; 4], spawner: F) -> Self
    where
        F: FnOnce(BeagleBoneFanTachTask) -> std::io::Result<JoinHandle<()>>,
    {
        let state = Arc::new(BeagleBoneFanTachState::new());
        let stop = Arc::new(AtomicBool::new(false));
        if !value_paths.iter().any(Option::is_some) {
            return Self {
                state,
                stop,
                worker: None,
            };
        }

        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        let task: BeagleBoneFanTachTask = Box::new(move || {
            let _exit_guard = BeagleBoneFanTachExitGuard(Arc::clone(&worker_state));
            Self::run_sampler(worker_state, worker_stop, value_paths);
        });

        match spawner(task) {
            Ok(worker) => Self {
                state,
                stop,
                worker: Some(worker),
            },
            Err(error) => {
                tracing::error!(
                    %error,
                    "BeagleBone fan-tach: sampler spawn failed; all channels unavailable"
                );
                state.mark_all_unavailable();
                Self {
                    state,
                    stop,
                    worker: None,
                }
            }
        }
    }

    fn is_available(&self) -> bool {
        self.worker.is_some()
            && self.state.observations.iter().all(|observation| {
                observation.load(Ordering::Acquire) & BeagleBoneFanTachState::AVAILABLE_FLAG != 0
            })
    }

    fn read_per_fan_observations(&self) -> [BeagleBoneFanTachObservation; 4] {
        std::array::from_fn(|idx| self.state.observation(idx))
    }

    fn read_per_fan_rpm(&self) -> [u32; 4] {
        self.read_per_fan_observations()
            .map(|observation| observation.rpm)
    }

    /// Request sampler stop and consume its `JoinHandle` only after bounded
    /// `is_finished()` evidence proves `join()` cannot wait on sysfs I/O. A
    /// timeout fails the receipt and detaches the still-stopping worker rather
    /// than blocking shutdown indefinitely.
    fn stop_and_join(&mut self) -> bool {
        self.stop_and_join_with_timeout(FAN_TACH_STOP_TIMEOUT)
    }

    fn stop_and_join_with_timeout(&mut self, timeout: Duration) -> bool {
        self.stop.store(true, Ordering::Release);
        self.state.mark_all_unavailable();
        let Some(worker) = self.worker.take() else {
            return true;
        };
        let deadline = Instant::now().checked_add(timeout);
        while !worker.is_finished() {
            let Some(deadline) = deadline else {
                break;
            };
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            thread::sleep(FAN_TACH_SAMPLE_INTERVAL.min(deadline.saturating_duration_since(now)));
        }
        if !worker.is_finished() {
            tracing::error!(
                timeout_ms = timeout.as_millis(),
                "BeagleBone fan-tach: sampler stop timed out; detaching unavailable worker"
            );
            return false;
        }
        let joined = match worker.join() {
            Ok(()) => true,
            Err(_) => {
                tracing::error!("BeagleBone fan-tach: sampler thread panicked during join");
                false
            }
        };
        joined
    }

    fn prepare_gpio_input(gpio: u32) -> Result<String> {
        export_gpio_if_needed(gpio)?;
        write_gpio_direction(gpio, "in")?;
        Ok(format!("/sys/class/gpio/gpio{}/value", gpio))
    }

    fn run_sampler(
        state: Arc<BeagleBoneFanTachState>,
        stop: Arc<AtomicBool>,
        value_paths: [Option<String>; 4],
    ) {
        // `None` means no level has been observed yet (or the prior read
        // failed). A first low sample is baseline state, not a falling edge.
        let mut last_levels: [Option<u8>; 4] = [None; 4];
        let mut pulse_counts: [u32; 4] = [0; 4];
        // Per-channel first-success time makes a complete window literal even
        // if the worker is preempted before its first GPIO poll.
        let mut valid_since: [Option<Instant>; 4] = [None; 4];
        let mut window_start: Option<Instant> = None;

        while !stop.load(Ordering::Acquire) {
            for (idx, path_opt) in value_paths.iter().enumerate() {
                let Some(path) = path_opt else { continue };
                match read_gpio_level(path) {
                    Some(level) => {
                        if valid_since[idx].is_none() {
                            valid_since[idx] = Some(Instant::now());
                        }
                        observe_tach_level(&mut last_levels[idx], level, &mut pulse_counts[idx]);
                    }
                    None => {
                        state.store_observation(idx, false, 0);
                        pulse_counts[idx] = 0;
                        last_levels[idx] = None;
                        valid_since[idx] = None;
                    }
                }
            }

            let sample_window_start = window_start.get_or_insert_with(Instant::now);
            if sample_window_start.elapsed() >= FAN_TACH_WINDOW {
                let elapsed_ms = sample_window_start.elapsed().as_millis().max(1) as u32;
                for (idx, pulse_count) in pulse_counts.iter_mut().enumerate() {
                    let available = valid_since[idx]
                        .is_some_and(|since| since.elapsed() >= FAN_TACH_WINDOW)
                        && last_levels[idx].is_some();
                    let rpm = if available {
                        pulses_to_rpm(*pulse_count, elapsed_ms)
                    } else {
                        0
                    };
                    state.store_observation(idx, available, rpm);
                    *pulse_count = 0;
                }
                window_start = Some(Instant::now());
            }

            thread::sleep(FAN_TACH_SAMPLE_INTERVAL);
        }
    }
}

impl Drop for BeagleBoneFanTach {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

/// Incorporate one GPIO level without inventing an edge before a baseline is
/// known. After a read failure the caller resets `last_level` to `None`, so a
/// later low recovery sample is also baseline rather than a fabricated pulse.
fn observe_tach_level(last_level: &mut Option<u8>, level: u8, pulse_count: &mut u32) {
    if matches!(*last_level, Some(1)) && level == 0 {
        *pulse_count = pulse_count.saturating_add(1);
    }
    *last_level = Some(level);
}

/// Convert a falling-edge pulse count over `elapsed_ms` into RPM.
/// `RPM = pulses * 60_000 / elapsed_ms / pulses_per_rev`.
fn pulses_to_rpm(pulses: u32, elapsed_ms: u32) -> u32 {
    if elapsed_ms == 0 || FAN_TACH_PULSES_PER_REV == 0 {
        return 0;
    }
    let scaled = pulses.saturating_mul(60_000);
    scaled / elapsed_ms / FAN_TACH_PULSES_PER_REV
}

/// Read a sysfs gpioN/value file, return 0 or 1, or `None` on read error.
fn read_gpio_level(path: &str) -> Option<u8> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .map(|v| if v == 0 { 0 } else { 1 })
}

// ─── GPIO ───

struct BeagleBoneGpio;

impl BeagleBoneGpio {
    fn new() -> Self {
        Self
    }

    fn read_plug_detect_4(&self) -> [bool; 4] {
        let mut out = [false; 4];
        for (i, &gpio) in GPIO_PLUG_DETECT.iter().enumerate() {
            out[i] = read_gpio_value_active_high(gpio);
        }
        out
    }
}

impl GpioAccess for BeagleBoneGpio {
    fn read_plug_detect(&self) -> [bool; 3] {
        let four = self.read_plug_detect_4();
        [four[0], four[1], four[2]]
    }

    fn set_board_reset(&self, chain: u8, assert_reset: bool) {
        let idx = chain as usize;
        if idx >= GPIO_BOARD_RESET.len() {
            tracing::warn!(chain, "BeagleBone: invalid chain id for reset");
            return;
        }
        let gpio = GPIO_BOARD_RESET[idx];
        let value = if assert_reset { "0" } else { "1" };
        if let Err(e) = write_gpio_value(gpio, value) {
            tracing::error!(gpio, value, error = %e, "BeagleBone reset GPIO write failed");
        }
    }
}

// ─── PSU enable ───

pub fn enable_psu() -> Result<()> {
    export_gpio_if_needed(GPIO_PSU_ENABLE)?;
    write_gpio_direction(GPIO_PSU_ENABLE, "out")?;
    write_gpio_value(GPIO_PSU_ENABLE, "1")?;
    tracing::info!(
        "BeagleBone PSU GPIO {} driven HIGH (PSU enabled)",
        GPIO_PSU_ENABLE
    );
    Ok(())
}

pub fn disable_psu() -> Result<()> {
    write_gpio_value(GPIO_PSU_ENABLE, "0")?;
    tracing::info!(
        "BeagleBone PSU GPIO {} driven LOW (PSU disabled)",
        GPIO_PSU_ENABLE
    );
    Ok(())
}

pub fn set_led_green(on: bool) {
    let _ = write_gpio_value(GPIO_LED_GREEN, if on { "1" } else { "0" });
}

pub fn set_led_red(on: bool) {
    let _ = write_gpio_value(GPIO_LED_RED, if on { "1" } else { "0" });
}

// ─── sysfs GPIO helpers ───

fn export_gpio_if_needed(gpio: u32) -> Result<()> {
    let dir = format!("/sys/class/gpio/gpio{}", gpio);
    if Path::new(&dir).exists() {
        return Ok(());
    }
    fs::write("/sys/class/gpio/export", gpio.to_string())
        .map_err(|e| HalError::Platform(format!("export GPIO {}: {}", gpio, e)))?;
    std::thread::sleep(std::time::Duration::from_millis(20));
    Ok(())
}

fn write_gpio_direction(gpio: u32, dir: &str) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/direction", gpio);
    fs::write(&path, dir).map_err(|e| HalError::Platform(format!("set {} dir: {}", path, e)))
}

fn write_gpio_value(gpio: u32, value: &str) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/value", gpio);
    fs::write(&path, value).map_err(|e| HalError::Platform(format!("write {}: {}", path, e)))
}

fn read_gpio_value_active_high(gpio: u32) -> bool {
    let path = format!("/sys/class/gpio/gpio{}/value", gpio);
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .map(|v| v == 1)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::config::{PicType, VoltageControl};

    fn fan_test_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dcentos-beaglebone-fan-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn wait_for_condition(timeout: Duration, condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        condition()
    }

    fn disabled_test_tach() -> BeagleBoneFanTach {
        BeagleBoneFanTach {
            state: Arc::new(BeagleBoneFanTachState::new()),
            stop: Arc::new(AtomicBool::new(true)),
            worker: None,
        }
    }

    #[test]
    fn pinout_constants_match_s70cgminer_capture() {
        assert_eq!(GPIO_PLUG_DETECT, [51, 48, 47, 44]);
        assert_eq!(GPIO_BOARD_RESET, [5, 4, 27, 22]);
        assert_eq!(GPIO_PSU_ENABLE, 65);
        assert_eq!(GPIO_LED_GREEN, 23);
        assert_eq!(GPIO_LED_RED, 45);
        assert_eq!(GPIO_FAN_TACH, [7, 20, 110, 112]);
        assert_eq!(BB_PWM_PERIOD_NS, 100_000);
    }

    /// W14.A6 — Recovery button GPIO is AM335x GPIO13_30 → 446.
    /// Pinned so a future "consolidation" can't silently change the
    /// number; the front-panel maintenance/factory-reset hold-down
    /// path depends on this.
    #[test]
    fn recovery_btn_gpio_is_446_per_w4_re() {
        assert_eq!(GPIO_RECOVERY_BTN, 446);
        assert_eq!(BeagleBonePlatform::recovery_btn_gpio(), 446);
    }

    #[test]
    fn config_uses_correct_tty_devices_skipping_tty_o3() {
        let cfg = PlatformConfig::s19j_beaglebone();
        let devices: Vec<&str> = cfg
            .chains
            .iter()
            .filter_map(|c| match &c.transport {
                ChainTransport::Serial { device, .. } => Some(device.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            devices,
            vec!["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"]
        );
        for c in &cfg.chains {
            if let ChainTransport::Serial { device, .. } = &c.transport {
                assert_ne!(device, "/dev/ttyO3", "ttyO3 is disabled in BB DTB");
            }
        }
    }

    #[test]
    fn config_uses_dspic_voltage_topology_matching_am2() {
        let cfg = PlatformConfig::s19j_beaglebone();
        assert!(matches!(cfg.pic_type, PicType::DsPic33EP16GS202));
        assert!(matches!(cfg.voltage_control, VoltageControl::DsPic));
        let pic_addrs: Vec<Option<u8>> = cfg.chains.iter().map(|c| c.pic_address).collect();
        assert_eq!(pic_addrs, vec![Some(0x20), Some(0x21), Some(0x22), None]);
    }

    #[test]
    fn config_uses_i2c_bus_zero_only() {
        let cfg = PlatformConfig::s19j_beaglebone();
        for c in &cfg.chains {
            assert_eq!(c.i2c_bus, 0, "chain {} must use /dev/i2c-0", c.chain_id);
        }
    }

    #[test]
    fn plug_and_reset_gpios_match_pinout() {
        let cfg = PlatformConfig::s19j_beaglebone();
        let plug: Vec<Option<u32>> = cfg.chains.iter().map(|c| c.plug_detect_gpio).collect();
        let rst: Vec<Option<u32>> = cfg.chains.iter().map(|c| c.enable_gpio).collect();
        assert_eq!(plug, vec![Some(51), Some(48), Some(47), Some(44)]);
        assert_eq!(rst, vec![Some(5), Some(4), Some(27), Some(22)]);
    }

    #[test]
    fn fan_period_matches_s70cgminer() {
        let cfg = PlatformConfig::s19j_beaglebone();
        assert_eq!(cfg.fan.fan_count, 4);
    }

    #[test]
    fn fan_pwm_candidates_cover_stock_and_luxos_paths() {
        let paths: Vec<String> = BeagleBoneFan::candidate_duty_paths()
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert!(paths.contains(&"/sys/class/pwm/pwmchip0/pwm0/duty_cycle".to_string()));
        assert!(paths.contains(&"/sys/class/pwm/pwmchip0/pwm1/duty_cycle".to_string()));
        assert!(paths.contains(&"/sys/class/pwm/pwmchip2/pwm0/duty_cycle".to_string()));
        assert!(paths.contains(&"/sys/class/pwm/pwm1/duty_ns".to_string()));
        assert!(paths.contains(&"/sys/class/pwm/pwm2/duty_ns".to_string()));
    }

    #[test]
    fn checked_fan_command_writes_and_reads_every_discovered_path() {
        let root = fan_test_dir("checked");
        fs::create_dir_all(&root).expect("create fan fixture");
        let front = root.join("front-duty");
        let rear = root.join("rear-duty");
        fs::write(&front, "0").expect("seed front duty");
        fs::write(&rear, "0").expect("seed rear duty");
        let fan = BeagleBoneFan {
            duty_paths: vec![
                PwmDutyPath {
                    label: "front-test",
                    path: front.to_string_lossy().into_owned(),
                },
                PwmDutyPath {
                    label: "rear-test",
                    path: rear.to_string_lossy().into_owned(),
                },
            ],
            period_ns: BB_PWM_PERIOD_NS,
            tach: disabled_test_tach(),
        };

        let receipt = fan
            .set_speed_checked(30)
            .expect("two-path checked fan command");
        assert_eq!(receipt.requested_pwm(), 30);
        assert_eq!(receipt.observed_pwm(), 30);
        assert_eq!(fs::read_to_string(front).unwrap(), "30000");
        assert_eq!(fs::read_to_string(rear).unwrap(), "30000");
        fs::remove_dir_all(root).expect("remove fan fixture");
    }

    #[test]
    fn checked_fan_command_surfaces_partial_multi_path_write() {
        let root = fan_test_dir("partial");
        fs::create_dir_all(&root).expect("create fan fixture");
        let writable = root.join("writable-duty");
        let failing = root.join("failing-duty");
        fs::write(&writable, "0").expect("seed writable duty");
        fs::create_dir(&failing).expect("create non-writable duty path");
        let fan = BeagleBoneFan {
            duty_paths: vec![
                PwmDutyPath {
                    label: "writable-test",
                    path: writable.to_string_lossy().into_owned(),
                },
                PwmDutyPath {
                    label: "failing-test",
                    path: failing.to_string_lossy().into_owned(),
                },
            ],
            period_ns: BB_PWM_PERIOD_NS,
            tach: disabled_test_tach(),
        };

        let error = fan
            .set_speed_checked(30)
            .expect_err("partial command must not mint a receipt");
        assert!(error.to_string().contains("failing-test"));
        assert_eq!(fs::read_to_string(writable).unwrap(), "30000");
        fs::remove_dir_all(root).expect("remove fan fixture");
    }

    #[test]
    fn beaglebone_platform_with_config_matches_default() {
        let plat = BeagleBonePlatform::with_config(PlatformConfig::s19j_beaglebone());
        assert!(matches!(plat.board_type(), BoardType::BeagleBone));
        assert_eq!(plat.chain_count(), 4);
    }

    #[test]
    fn public_hal_accessors_lock_wave_0e_pinout_contract() {
        assert_eq!(BeagleBonePlatform::i2c_bus_number(), 0);
        assert_eq!(
            BeagleBonePlatform::chain_uarts(),
            ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"]
        );
        assert_eq!(BeagleBonePlatform::chain_plug_gpios(), [51, 48, 47, 44]);
        assert_eq!(BeagleBonePlatform::chain_reset_gpios(), [5, 4, 27, 22]);
        assert_eq!(BeagleBonePlatform::psu_enable_gpio(), 65);
        assert_eq!(BeagleBonePlatform::led_gpios(), (23, 45));
        assert_eq!(BeagleBonePlatform::fan_tach_gpios(), [7, 20, 110, 112]);
        assert_eq!(BeagleBonePlatform::fan_pwm_period_ns(), 100_000);
    }

    #[test]
    fn open_i2c_rejects_bus_outside_board_target_allowed_set() {
        // Phase B: the allowed I²C buses are the board-target's eeprom_bus
        // (default 0) and psu_bus (default 1 on `a lab unit`
        // S19J_IO_BOARD_V2_0). Any other bus must be rejected before
        // touching device files. (Bus 1 itself is now ALLOWED — it's the
        // bit-banged i2c-gpio PSU bus — so we use a clearly-out-of-range
        // bus number here. On a non-Linux host even an allowed bus errors
        // at `I2cBus::open`, but with a *different* message; this test
        // only asserts the pre-open rejection path for an out-of-set bus.)
        let plat = BeagleBonePlatform::with_config(PlatformConfig::s19j_beaglebone());
        let err = match plat.open_i2c(99) {
            Ok(_) => panic!("BB must reject an I²C bus outside the board-target allowed set"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("not in the board-target's allowed set")
                && msg.contains("eeprom_bus=0")
                && msg.contains("psu_bus=1"),
            "unexpected error: {}",
            msg
        );
    }

    // ─── W3.3 — Real GPIO falling-edge tach ───

    #[test]
    fn test_beaglebone_synthetic_rpm_replaced() {
        // W3.3 acceptance: the synthesized `900 + (pwm * 40)` RPM estimator
        // must be gone from the production fan-access path. We prove that
        // by exercising the new pulse-counter math + verifying that the
        // tach struct on a non-BB host (CI/Windows) reports unavailable.

        // (1) Sampler constants are present and sane.
        assert_eq!(FAN_TACH_PULSES_PER_REV, 2);
        assert_eq!(FAN_TACH_WINDOW, Duration::from_millis(1000));
        assert_eq!(FAN_TACH_SAMPLE_INTERVAL, Duration::from_millis(2));

        // (2) The conversion is derivable and sane.
        // 100 pulses, 1000 ms, 2 PPR → 100 * 60_000 / 1000 / 2 = 3000 RPM.
        assert_eq!(pulses_to_rpm(100, 1000), 3000);
        assert_eq!(pulses_to_rpm(50, 500), 3000);
        assert_eq!(pulses_to_rpm(0, 1000), 0);
        // Defensive divisor: elapsed_ms=0 must not panic.
        assert_eq!(pulses_to_rpm(100, 0), 0);

        // (3) The fan-tach struct, when started against GPIO numbers that
        // don't exist on this host, must NOT panic and must report
        // unavailable. The previous synthesized path would always have
        // reported a PWM-derived RPM regardless of hardware.
        let tach = BeagleBoneFanTach::start(GPIO_FAN_TACH);
        assert!(
            !tach.is_available(),
            "tach must report unavailable when /sys/class/gpio export fails on host"
        );
        let per_fan = tach.read_per_fan_rpm();
        assert_eq!(
            per_fan,
            [0, 0, 0, 0],
            "no synthesized fallback — RPM must be 0 when sampler is cold/unavailable"
        );

        // (4) The override env var name is exactly what  /
        // operator docs reference.
        assert_eq!(
            BB_TACH_ACCEPT_DEGRADED_ENV,
            "DCENT_AM3_BB_ACCEPT_DEGRADED_TACH"
        );
    }

    #[test]
    fn first_low_tach_sample_establishes_baseline_without_synthetic_edge() {
        let mut last_level = None;
        let mut pulses = 0;

        observe_tach_level(&mut last_level, 0, &mut pulses);
        assert_eq!(pulses, 0, "first low sample is not an observed 1->0 edge");
        assert_eq!(last_level, Some(0));
        observe_tach_level(&mut last_level, 1, &mut pulses);
        assert_eq!(pulses, 0);
        observe_tach_level(&mut last_level, 0, &mut pulses);
        assert_eq!(pulses, 1, "a subsequently observed 1->0 is one edge");

        // A failed read resets the baseline in the sampler. Pin the recovery
        // behavior here so a low sample after failure cannot invent a pulse.
        last_level = None;
        observe_tach_level(&mut last_level, 0, &mut pulses);
        assert_eq!(pulses, 1);
    }

    #[test]
    fn fan_tach_spawn_failure_never_publishes_availability() {
        let value_paths = std::array::from_fn(|idx| Some(format!("unused-tach-{idx}")));
        let tach = BeagleBoneFanTach::start_prepared_with_spawner(value_paths, |_task| {
            Err(std::io::Error::other("injected sampler spawn failure"))
        });

        assert!(tach.worker.is_none(), "failed spawn must retain no handle");
        assert!(!tach.is_available());
        assert!(
            tach.read_per_fan_observations()
                .iter()
                .all(|observation| !observation.available && observation.rpm == 0),
            "spawn failure must be truthful unavailable/zero, never stale-ready"
        );
    }

    #[test]
    fn fan_tach_owner_stop_consumes_join_handle() {
        let root = fan_test_dir("tach-stop-join");
        fs::create_dir_all(&root).expect("create tach fixture");
        let value = root.join("value");
        fs::write(&value, "1").expect("seed tach level");
        let value_path = value.to_string_lossy().into_owned();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_worker = Arc::clone(&finished);
        let mut tach = BeagleBoneFanTach::start_prepared_with_spawner(
            std::array::from_fn(|_| Some(value_path.clone())),
            move |task| {
                thread::Builder::new()
                    .name("bb-fan-tach-owner-test".to_string())
                    .spawn(move || {
                        task();
                        finished_worker.store(true, Ordering::Release);
                    })
            },
        );

        assert!(tach.worker.is_some());
        assert!(tach.stop_and_join(), "sampler must join without panic");
        assert!(tach.worker.is_none(), "join handle must be consumed");
        assert!(finished.load(Ordering::Acquire));
        assert!(!tach.is_available());
        fs::remove_dir_all(root).expect("remove tach fixture");
    }

    #[test]
    fn fan_tach_stop_timeout_is_bounded_and_fails_receipt() {
        let root = fan_test_dir("tach-stop-timeout");
        fs::create_dir_all(&root).expect("create tach fixture");
        let value = root.join("value");
        fs::write(&value, "1").expect("seed tach level");
        let value_path = value.to_string_lossy().into_owned();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_worker = Arc::clone(&finished);
        let mut tach = BeagleBoneFanTach::start_prepared_with_spawner(
            std::array::from_fn(|_| Some(value_path.clone())),
            move |task| {
                thread::Builder::new()
                    .name("bb-fan-tach-timeout-test".to_string())
                    .spawn(move || {
                        // Deterministically model a sysfs read that does not
                        // react to the owner's stop request within its budget.
                        thread::sleep(Duration::from_millis(80));
                        task();
                        finished_worker.store(true, Ordering::Release);
                    })
            },
        );

        let started = Instant::now();
        assert!(
            !tach.stop_and_join_with_timeout(Duration::from_millis(10)),
            "a non-cooperative worker must not mint a stop receipt"
        );
        assert!(
            started.elapsed() < Duration::from_millis(60),
            "bounded stop must return before the injected worker unblocks"
        );
        assert!(tach.worker.is_none());
        assert!(!tach.is_available());
        assert!(wait_for_condition(Duration::from_millis(200), || finished
            .load(Ordering::Acquire)));
        fs::remove_dir_all(root).expect("remove tach fixture");
    }

    #[test]
    fn fan_tach_owner_drop_stops_and_joins_worker() {
        let root = fan_test_dir("tach-drop-join");
        fs::create_dir_all(&root).expect("create tach fixture");
        let value = root.join("value");
        fs::write(&value, "1").expect("seed tach level");
        let value_path = value.to_string_lossy().into_owned();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_worker = Arc::clone(&finished);
        {
            let tach = BeagleBoneFanTach::start_prepared_with_spawner(
                std::array::from_fn(|_| Some(value_path.clone())),
                move |task| {
                    thread::Builder::new()
                        .name("bb-fan-tach-drop-test".to_string())
                        .spawn(move || {
                            task();
                            finished_worker.store(true, Ordering::Release);
                        })
                },
            );
            assert!(tach.worker.is_some());
        }

        assert!(
            finished.load(Ordering::Acquire),
            "owner drop must not return before sampler exits"
        );
        fs::remove_dir_all(root).expect("remove tach fixture");
    }

    #[test]
    fn positive_pwm_never_synthesizes_nonzero_beaglebone_rpm() {
        let root = fan_test_dir("tach-no-synthesis");
        fs::create_dir_all(&root).expect("create tach fixture");
        let duty = root.join("duty");
        let value = root.join("value");
        fs::write(&duty, "30000").expect("seed PWM 30 duty");
        fs::write(&value, "1").expect("seed tach high level");
        let value_path = value.to_string_lossy().into_owned();
        let fan = BeagleBoneFan {
            duty_paths: vec![PwmDutyPath {
                label: "tach-no-synthesis-test",
                path: duty.to_string_lossy().into_owned(),
            }],
            period_ns: BB_PWM_PERIOD_NS,
            tach: BeagleBoneFanTach::start_prepared(std::array::from_fn(|_| {
                Some(value_path.clone())
            })),
        };

        assert_eq!(fan.get_speed_pwm(), 30);
        assert!(wait_for_condition(
            FAN_TACH_WINDOW + Duration::from_millis(500),
            || fan.tach_available()
        ));
        assert_eq!(fan.get_rpm(), 0, "zero pulses must remain zero RPM");
        assert_eq!(fan.get_per_fan_rpm(), vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
        drop(fan);
        fs::remove_dir_all(root).expect("remove tach fixture");
    }

    #[test]
    fn partial_tach_availability_cannot_mask_missing_fans_in_aggregate() {
        let root = fan_test_dir("tach-partial-unavailable");
        fs::create_dir_all(&root).expect("create tach fixture");
        let duty = root.join("duty");
        let value = root.join("value");
        fs::write(&duty, "30000").expect("seed PWM 30 duty");
        fs::write(&value, "1").expect("seed one tach high level");
        let fan = BeagleBoneFan {
            duty_paths: vec![PwmDutyPath {
                label: "tach-partial-test",
                path: duty.to_string_lossy().into_owned(),
            }],
            period_ns: BB_PWM_PERIOD_NS,
            tach: BeagleBoneFanTach::start_prepared([
                Some(value.to_string_lossy().into_owned()),
                None,
                None,
                None,
            ]),
        };

        assert!(wait_for_condition(
            FAN_TACH_WINDOW + Duration::from_millis(500),
            || fan.tach.read_per_fan_observations()[0].available
        ));
        assert!(
            !fan.tach_available(),
            "aggregate tach readiness requires all four physical channels"
        );
        assert_eq!(
            fan.get_rpm(),
            0,
            "one observable fan must not mask three unavailable fans"
        );
        assert_eq!(fan.get_per_fan_rpm(), vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
        drop(fan);
        fs::remove_dir_all(root).expect("remove tach fixture");
    }

    #[test]
    fn fan_mode_refuses_advanced_without_override() {
        // Default (no override): Advanced and HashrateMax both refused.
        let err = BeagleBoneFan::check_fan_mode_allowed_with_env(FanMode::Advanced, None)
            .expect_err("Advanced must be refused without override");
        assert!(err.to_string().contains(BB_TACH_ACCEPT_DEGRADED_ENV));

        let err = BeagleBoneFan::check_fan_mode_allowed_with_env(FanMode::HashrateMax, None)
            .expect_err("HashrateMax must be refused without override");
        assert!(err.to_string().contains(BB_TACH_ACCEPT_DEGRADED_ENV));

        // Lower modes are always allowed.
        for mode in [FanMode::QuietHome, FanMode::Home, FanMode::Balanced] {
            BeagleBoneFan::check_fan_mode_allowed_with_env(mode, None).unwrap_or_else(|e| {
                panic!(
                    "{:?} must be allowed without override, got error: {}",
                    mode, e
                )
            });
        }
    }

    #[test]
    fn fan_mode_accepts_advanced_with_explicit_override() {
        // Explicit override env=1 unblocks Advanced/HashrateMax (lab path).
        BeagleBoneFan::check_fan_mode_allowed_with_env(FanMode::Advanced, Some("1".to_string()))
            .expect("Advanced must be allowed with override=1");
        BeagleBoneFan::check_fan_mode_allowed_with_env(FanMode::HashrateMax, Some("1".to_string()))
            .expect("HashrateMax must be allowed with override=1");
        BeagleBoneFan::check_fan_mode_allowed_with_env(FanMode::Advanced, Some("true".to_string()))
            .expect("Advanced must be allowed with override=true");

        // Other values still refuse.
        for bad in ["", "0", "no", "false", "yes"] {
            let res = BeagleBoneFan::check_fan_mode_allowed_with_env(
                FanMode::Advanced,
                Some(bad.to_string()),
            );
            assert!(
                res.is_err(),
                "Advanced must stay refused with override={:?}",
                bad
            );
        }
    }

    // ─── W2A.2: PIC1704 wire-up regression guards ───

    #[test]
    fn bhb42601_dspic_subtype_does_not_construct_pic1704() {
        // The static config from `s19j_beaglebone()` defaults to dsPIC33EP
        // because `voltage_controller` is set in the constructor. The
        // PIC1704 upgrade only happens at runtime when both `/etc/subtype`
        // and the 0x20 ACK probe agree. On a host test the probe always
        // returns false, so a `BBCtrl_BHB42XXX` subtype must still fall
        // back to dsPIC33EP — proving the no-regression guarantee.
        let cfg = PlatformConfig::s19j_beaglebone();
        assert_eq!(
            cfg.voltage_controller,
            VoltageControllerKind::Dspic33Ep,
            "static s19j_beaglebone() must default to Dspic33Ep so units \
             without the new subtype + probe stay on the existing path"
        );

        // And the BB platform `with_config` path preserves this default
        // (no auto-upgrade without the runtime probe).
        let bb = BeagleBonePlatform::with_config(cfg);
        assert_eq!(
            bb.voltage_controller(),
            VoltageControllerKind::Dspic33Ep,
            "BeagleBonePlatform::voltage_controller must mirror \
             config.voltage_controller, not silently upgrade"
        );
    }

    #[test]
    fn beaglebone_voltage_controller_reflects_config_override() {
        // Operator override path: `with_config` accepts a pre-classified
        // PlatformConfig. The PIC1704 daemon orchestrator can use this
        // shape during integration tests once B2 lands the daemon-side
        // switching.
        let mut cfg = PlatformConfig::s19j_beaglebone();
        cfg.voltage_controller = VoltageControllerKind::Pic1704;
        let bb = BeagleBonePlatform::with_config(cfg);
        assert_eq!(bb.voltage_controller(), VoltageControllerKind::Pic1704);
    }

    // ─── W2B (2026-05-09): GPIO pinout regression-pin against dev-kit ───

    /// The GPIO map MUST mirror the BBCtrl S70cgminer init script
    /// byte-for-byte. The CV1835 pin numbers in
    /// `DCENT_OS_GAP_ANALYSIS_FINAL.md` (XGPIOC[11/13/15] → 427/429/431,
    /// LED_RED=434, LED_GREEN=435) belong to a DIFFERENT SoC family and
    /// must NEVER be substituted into the AM335x BB platform. This test
    /// pins the correct values so an over-eager refactor that pulls from
    /// the gap-analysis doc fails loudly.
    #[test]
    fn gpio_constants_are_am335x_not_cv1835() {
        // Canonical BBCtrl values (S70cgminer init script).
        assert_eq!(GPIO_BOARD_RESET, [5, 4, 27, 22]);
        assert_eq!(GPIO_LED_GREEN, 23);
        assert_eq!(GPIO_LED_RED, 45);

        // Defensive: CV1835 GPIOs MUST NOT be in the BB map.
        for cv1835_pin in [49, 60, 425, 426, 427, 428, 429, 430, 431, 434, 435] {
            assert_ne!(
                GPIO_PSU_ENABLE, cv1835_pin,
                "PSU enable must not collide with a CV1835 pin"
            );
            assert_ne!(
                GPIO_LED_GREEN, cv1835_pin,
                "LED green must not collide with a CV1835 pin"
            );
            assert_ne!(
                GPIO_LED_RED, cv1835_pin,
                "LED red must not collide with a CV1835 pin"
            );
            for &reset in &GPIO_BOARD_RESET {
                assert_ne!(
                    reset, cv1835_pin,
                    "board reset must not collide with a CV1835 pin"
                );
            }
        }
    }

    // ─── W2B (2026-05-09): EEPROM denylist coverage ───

    #[test]
    fn eeprom_denylist_covers_at24c_range() {
        // and the .74 hb2
        // corruption incident — every BHB42XXX hashboard wires an
        // AT24C-class EEPROM at 0x50..=0x57. The denylist MUST cover
        // the full range with no gaps.
        assert_eq!(BB_HASHBOARD_EEPROM_DENYLIST.len(), 8);
        let expected: Vec<u8> = (0x50u8..=0x57u8).collect();
        assert_eq!(BB_HASHBOARD_EEPROM_DENYLIST.to_vec(), expected);
    }

    // ─── W2B (2026-05-09): voltage controller subtype routing ───

    #[test]
    fn classify_with_probe_fails_closed_when_bb_probe_misses() {
        // On a non-Linux test host the 0x20 ACK probe always returns false,
        // so a `BBCtrl_BHB42XXX` subtype must become non-energizing rather
        // than guessing dsPIC. This is the same invariant the subtype module tests,
        // re-asserted at the BB platform layer for defense in depth.
        // (`classify_with_probe` is already in scope via the module-level
        // `use super::subtype::{...}`.)
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            classify_with_probe(Some("BBCtrl_BHB42XXX"), BB_I2C_BUS),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn classify_with_probe_routes_missing_and_unknown_subtype_to_nopic() {
        // Belt-and-braces: the existing test in `subtype.rs` covers the
        // None-vs-present-unknown split for the shared classifier. This re-runs
        // the contract through the BB-specific I2C bus number to prove the
        // wrapper hasn't been silently re-pointed at a different bus.
        // (`classify_with_probe` is in scope via the module-level `use`.)
        assert_eq!(
            classify_with_probe(None, BB_I2C_BUS),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_with_probe(Some("not-a-real-subtype"), BB_I2C_BUS),
            VoltageControllerKind::NoPic,
        );
    }

    // ─── Phase B (2026-05-12) — S19J_IO_BOARD_V2_0 (`a lab unit`) topology ───

    /// The `a lab unit` GPIO/I²C constants must match the live probe report.
    #[test]
    fn v2_0_constants_match_dot79_probe() {
        // probe-report.md "GPIO map" + "I2C topology".
        assert_eq!(GPIO_BOARD_ENABLE_V2_0, 59);
        assert_eq!(GPIO_ASIC_RST_V2_0, [49, 60, 27, 22]);
        assert_eq!(GPIO_PLUG_DETECT_V2_0, [51, 48, 47, 46]);
        assert_eq!(GPIO_FAN_TACH_V2_0, [7, 20, 110, 112]);
        assert_eq!(GPIO_LED_V2_0, (23, 45));
        assert_eq!(I2C_BUS_EEPROM_V2_0, 0);
        assert_eq!(I2C_BUS_PSU_V2_0, 1);
        assert_eq!(I2C_ADDR_PSU_V2_0, 0x10);
        assert_eq!(CHAIN_UARTS_V2_0, ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4"]);
        assert_eq!(DEFAULT_BOARD_TARGET_V2_0, "am3-bb-s19jpro");
        // The `a lab unit` GPIO map is NOT the W4 BBCtrl map — pin the difference.
        assert_ne!(
            GPIO_BOARD_ENABLE_V2_0, GPIO_PSU_ENABLE,
            "gpio59 != W4 gpio65"
        );
        assert_ne!(
            GPIO_ASIC_RST_V2_0, GPIO_BOARD_RESET,
            "{:?} != W4 {:?}",
            GPIO_ASIC_RST_V2_0, GPIO_BOARD_RESET
        );
        assert_ne!(
            GPIO_PLUG_DETECT_V2_0, GPIO_PLUG_DETECT,
            "plug[3] differs (46 vs 44)"
        );
    }

    /// The hardcoded `a lab unit` defaults must match what `am3-bb-s19jpro.toml`
    /// declares (the TOML file is the human-readable mirror).
    #[test]
    fn hardcoded_defaults_match_v2_0_constants() {
        let bt = BeagleBoneBoardTarget::hardcoded_v2_0_defaults();
        assert_eq!(bt.gpio.board_enable, 59);
        assert!(
            bt.board_enable_active_high(),
            "default board_enable_active = high"
        );
        assert_eq!(bt.gpio.asic_rst, vec![49, 60, 27, 22]);
        assert_eq!(bt.gpio.plug_detect, vec![51, 48, 47, 46]);
        assert_eq!(bt.gpio.fan_tach, vec![7, 20, 110, 112]);
        assert_eq!(bt.gpio.led, vec![23, 45]);
        assert_eq!(bt.uart.chain_count, 3);
        assert_eq!(bt.uart.mining_baud, 3_000_000);
        assert_eq!(
            bt.uart
                .chains
                .iter()
                .map(|c| c.device.clone())
                .collect::<Vec<_>>(),
            vec!["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4"]
        );
        assert_eq!(bt.i2c.eeprom_bus, 0);
        assert_eq!(bt.i2c.psu_bus, 1);
        assert_eq!(bt.i2c.psu_addr, 0x10);
        assert_eq!(bt.i2c.psu_kind, "apw12-uart-tunnel");
        assert!(bt.i2c.eeprom_write_deny);
        assert!(!bt.cold_boot.enable_pic1704_dc_dc, "not a PIC1704 board");
        assert!(
            !bt.cold_boot.run_pic_heartbeat,
            "AM3 BB mining owns dsPIC heartbeat"
        );
        assert!(
            bt.cold_boot.run_miscctrl_triple_write,
            "chip-side BM1362 on this carrier"
        );
        assert_eq!(bt.cold_boot.apw12_rail_open_core_mv, 15000);
        assert_eq!(bt.cold_boot.apw12_rail_steady_mv, 13800);
        assert_eq!(bt.cold_boot.gpio59_settle_ms, 3000);
        assert_eq!(bt.cold_boot.asic_rst_stagger_ms, 10);
        assert_eq!(bt.cold_boot.asic_rst_settle_ms, 1100);
        assert_eq!(bt.cold_boot.asic_rst_retry_chain, Some(1));
        assert_eq!(bt.cold_boot.asic_rst_retry_pulses, 2);
        assert_eq!(bt.cold_boot.asic_rst_retry_assert_ms, 200);
        assert_eq!(bt.cold_boot.asic_rst_retry_release_ms, 100);
        assert_eq!(bt.cold_boot.initial_freq_mhz, 400);
        assert_eq!(bt.cold_boot.fan_boot_pwm, 10);
        assert_eq!(bt.cold_boot.fan_max_pwm, 30);
        // The board-target classifies to dsPIC33EP: the APW is upstream,
        // but LuxOS ftrace proves fw=0x89 per-chain controllers on bus 0.
        assert_eq!(
            bt.classify_voltage_controller(),
            Some(VoltageControllerKind::Dspic33Ep)
        );
    }

    /// Full TOML round-trip — the `am3-bb-s19jpro.toml` content parses and
    /// produces the expected struct. (Mirrors the file; if the file
    /// changes, this string should be updated in lockstep.)
    #[test]
    fn parse_board_target_toml_round_trips_the_dot79_config() {
        let toml_src = r#"
[platform]
board_target = "am3-bb-s19jpro"
soc = "am335x"
cpu = "cortex-a8"
io_board = "S19J_IO_BOARD_V2_0"
voltage_controller = "dspic33ep-fw89"

[gpio]
board_enable = 59
board_enable_active = "high"
asic_rst = [49, 60, 27, 22]
plug_detect = [51, 48, 47, 46]
fan_tach = [7, 20, 110, 112]
led = [23, 45]

[uart]
chains = [
  { index = 0, device = "/dev/ttyS1", base_addr = 0x48022000, base_baud = 3000000 },
  { index = 1, device = "/dev/ttyS2", base_addr = 0x48024000, base_baud = 3000000 },
  { index = 2, device = "/dev/ttyS4", base_addr = 0x481a8000, base_baud = 3000000 },
]
chain_count = 3
mining_baud = 3000000

[i2c]
eeprom_bus = 0
eeprom_addrs = [0x50, 0x51, 0x52]
eeprom_write_deny = true
psu_bus = 1
psu_addr = 0x10
psu_kind = "apw12-uart-tunnel"

[cold_boot]
enable_pic1704_dc_dc = false
run_pic_heartbeat = false
run_miscctrl_triple_write = true
apw12_rail_open_core_mv = 15000
apw12_rail_steady_mv = 13800
gpio59_settle_ms = 3000
asic_rst_stagger_ms = 10
asic_rst_settle_ms = 1100
asic_rst_retry_chain = 1
asic_rst_retry_pulses = 2
asic_rst_retry_assert_ms = 200
asic_rst_retry_release_ms = 100
initial_freq_mhz = 400
fan_boot_pwm = 10
fan_max_pwm = 30
"#;
        let bt = parse_board_target_toml(toml_src).expect("the .79 board-target TOML must parse");
        assert_eq!(
            bt.platform.as_ref().and_then(|p| p.board_target.as_deref()),
            Some("am3-bb-s19jpro")
        );
        assert_eq!(
            bt.platform.as_ref().and_then(|p| p.io_board.as_deref()),
            Some("S19J_IO_BOARD_V2_0")
        );
        assert_eq!(bt.gpio.board_enable, 59);
        assert_eq!(bt.gpio.asic_rst, vec![49, 60, 27, 22]);
        assert_eq!(bt.uart.chains.len(), 3);
        assert_eq!(bt.uart.chains[0].device, "/dev/ttyS1");
        assert_eq!(bt.uart.chains[0].base_addr, Some(0x4802_2000));
        assert_eq!(bt.uart.chains[2].device, "/dev/ttyS4");
        assert_eq!(bt.uart.mining_baud, 3_000_000);
        assert_eq!(bt.i2c.psu_bus, 1);
        assert_eq!(bt.i2c.psu_kind, "apw12-uart-tunnel");
        assert!(!bt.cold_boot.enable_pic1704_dc_dc);
        assert!(bt.cold_boot.run_miscctrl_triple_write);
        assert_eq!(bt.cold_boot.apw12_rail_open_core_mv, 15000);
        assert_eq!(bt.cold_boot.asic_rst_retry_chain, Some(1));
        assert_eq!(bt.cold_boot.asic_rst_retry_pulses, 2);
        assert_eq!(bt.cold_boot.asic_rst_retry_assert_ms, 200);
        assert_eq!(bt.cold_boot.asic_rst_retry_release_ms, 100);
        assert_eq!(
            bt.classify_voltage_controller(),
            Some(VoltageControllerKind::Dspic33Ep)
        );
    }

    /// A partial TOML remains schema-parseable for tooling, while runtime
    /// admission separately rejects it when `[platform]` identity is absent.
    #[test]
    fn parse_board_target_toml_tolerates_partial_config() {
        let bt = parse_board_target_toml("[gpio]\nboard_enable = 7\n")
            .expect("partial TOML must still parse");
        assert_eq!(bt.gpio.board_enable, 7, "the one declared value is used");
        // Everything else falls back to the .79 defaults.
        assert_eq!(bt.gpio.asic_rst, vec![49, 60, 27, 22]);
        assert_eq!(bt.uart.chain_count, 3);
        assert_eq!(bt.i2c.psu_bus, 1);
        assert_eq!(bt.cold_boot.apw12_rail_steady_mv, 13800);
        assert_eq!(bt.cold_boot.asic_rst_retry_chain, Some(1));
        // Empty TOML is also fine.
        let empty = parse_board_target_toml("").expect("empty TOML must parse");
        assert_eq!(empty.gpio.board_enable, 59);
        assert_eq!(
            empty.classify_voltage_controller(),
            Some(VoltageControllerKind::Dspic33Ep)
        );
    }

    /// `board_enable_active` is case/whitespace-tolerant and "low" flips it.
    #[test]
    fn board_enable_active_polarity_parses_both_ways() {
        let hi = parse_board_target_toml("[gpio]\nboard_enable_active = \"high\"\n").unwrap();
        assert!(hi.board_enable_active_high());
        let lo = parse_board_target_toml("[gpio]\nboard_enable_active = \"low\"\n").unwrap();
        assert!(!lo.board_enable_active_high());
        let lo2 = parse_board_target_toml("[gpio]\nboard_enable_active = \"  LOW  \"\n").unwrap();
        assert!(!lo2.board_enable_active_high());
        // Anything not "low" → high (the safe default-ON-is-high posture).
        let dflt = parse_board_target_toml("[gpio]\n").unwrap();
        assert!(dflt.board_enable_active_high());
    }

    /// `load_board_target` on a non-Linux host (Windows CI) returns None —
    /// `/etc/dcentos/board_targets/<name>.toml` doesn't exist there.
    #[test]
    fn load_board_target_returns_none_when_file_absent() {
        // Use a nonce name guaranteed not to exist.
        assert!(
            load_board_target("definitely-not-a-real-board-target-xyzzy")
                .expect("missing board-target is not a parse/read failure")
                .is_none()
        );
    }

    /// Host absence must remain honest; exact DT evidence is evaluated by the
    /// separate identity admission helper.
    #[test]
    fn read_active_board_target_name_does_not_invent_am3_bb() {
        assert_eq!(
            read_active_board_target_name().expect("host absence is valid"),
            None
        );
    }

    #[test]
    fn exact_marker_or_exact_live_dt_authorizes_am3_bb() {
        assert_eq!(
            authorize_am3_bb_identity(Some("am3-bb-s19jpro"), b"", b"").unwrap(),
            Am3BbIdentityEvidence::BoardTargetMarker
        );
        assert_eq!(
            authorize_am3_bb_identity(
                None,
                b"ti,am335x-bone-black\0ti,am335x-bone\0ti,am33xx\0",
                b"BeagleBone_Black_v2.1 on S19J_IO_BOARD_V2_0\0",
            )
            .unwrap(),
            Am3BbIdentityEvidence::ExactDeviceTree
        );
    }

    #[test]
    fn identity_substrings_and_contradictory_markers_fail_closed() {
        assert!(authorize_am3_bb_identity(
            None,
            b"vendor,am335x-compatible-ish\0",
            b"prefix S19J_IO_BOARD_V2_0 suffix\0",
        )
        .is_err());
        assert!(authorize_am3_bb_identity(
            Some("am3-bb-s19jpro-typo"),
            b"ti,am335x-bone-black\0",
            b"BeagleBone_Black_v2.1 on S19J_IO_BOARD_V2_0\0",
        )
        .is_err());
    }

    #[test]
    fn am3_bb_dspic_endpoint_binds_exact_uart_eeprom_and_existing_fw_reply() {
        let eeproms = [
            Some(vec![0x04, 0x11, 0x42, 0x60, 0x01]),
            Some(vec![0x04, 0x11, 0x42, 0x60, 0x02]),
            Some(vec![0x04, 0x11, 0x42, 0x60, 0x03]),
        ];
        let endpoint = bind_am3_bb_dspic_endpoint_from_observations(
            Some(DEFAULT_BOARD_TARGET_V2_0),
            b"",
            b"",
            "/dev/ttyS2",
            0x21,
            &eeproms,
            &[0x05, 0x17, 0x89, 0x00, 0x00],
        )
        .unwrap();
        assert_eq!(endpoint.kind(), VoltageControllerKind::Dspic33Ep);
        assert_eq!(endpoint.bus(), I2C_BUS_EEPROM_V2_0);
        assert_eq!(endpoint.address(), 0x21);
        assert_eq!(endpoint.observed_firmware(), Some(0x89));
    }

    #[test]
    fn am3_bb_dspic_endpoint_refuses_unbound_or_widened_observations() {
        let valid = [
            Some(vec![0x04, 0x11, 0x42]),
            Some(vec![0x04, 0x11, 0x42]),
            Some(vec![0x04, 0x11, 0x42]),
        ];
        let bind = |uart: &str, address: u8, eeproms: &[Option<Vec<u8>>], reply: &[u8]| {
            bind_am3_bb_dspic_endpoint_from_observations(
                Some(DEFAULT_BOARD_TARGET_V2_0),
                b"",
                b"",
                uart,
                address,
                eeproms,
                reply,
            )
        };

        assert!(bind("/dev/ttyS4", 0x23, &valid, &[0x89]).is_err());
        assert!(bind("/dev/ttyS3", 0x22, &valid, &[0x89]).is_err());
        assert!(bind("/dev/ttyS2", 0x21, &[None, None, None], &[0x89]).is_err());
        assert!(bind(
            "/dev/ttyS2",
            0x21,
            &[Some(vec![0x04, 0x11]), Some(vec![0x05, 0x11]), None],
            &[0x89],
        )
        .is_err());
        assert!(bind("/dev/ttyS2", 0x21, &valid, &[0x86]).is_err());
        assert!(bind("/dev/ttyS2", 0x21, &valid, &[0x17, 0x89, 0x00]).is_err());
        assert!(bind("/dev/ttyS2", 0x21, &valid, &[0x44, 0x17, 0x89]).is_err());
    }

    #[test]
    fn runtime_target_validation_rejects_default_minted_partial_toml() {
        let partial = parse_board_target_toml("[gpio]\nboard_enable = 7\n").unwrap();
        assert!(validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &partial).is_err());

        let mut declared = parse_board_target_toml(
            r#"
[platform]
board_target = "am3-bb-s19jpro"
soc = "am335x"
cpu = "cortex-a8"
io_board = "S19J_IO_BOARD_V2_0"
voltage_controller = "dspic33ep-fw89"
"#,
        )
        .unwrap();
        validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &declared)
            .expect("exact supported identity may use schema defaults");
        let mut contradictory = declared.clone();
        contradictory
            .platform
            .as_mut()
            .expect("test target has platform")
            .io_board = Some("S19J_IO_BOARD_V2_0-typo".into());
        assert!(
            validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &contradictory).is_err()
        );
        let mut active_low = declared.clone();
        active_low.gpio.board_enable_active = "low".into();
        let active_low_error =
            validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &active_low)
                .expect_err("active-low board-enable must fail before platform construction")
                .to_string();
        assert!(active_low_error.contains("active-low output setup is refused"));
        let mut invalid_polarity = declared.clone();
        invalid_polarity.gpio.board_enable_active = "lo".into();
        let invalid_polarity_error =
            validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &invalid_polarity)
                .expect_err("malformed board-enable polarity must fail closed")
                .to_string();
        assert!(invalid_polarity_error.contains("must be exactly \"high\" or \"low\""));
        declared.uart.chain_count = 4;
        assert!(validate_supported_board_target(DEFAULT_BOARD_TARGET_V2_0, &declared).is_err());
    }

    /// `with_config_and_board_target` plumbs the `a lab unit` accessors through to
    /// the loaded board-target config.
    #[test]
    fn v2_0_accessors_reflect_loaded_board_target() {
        let bt = BeagleBoneBoardTarget::hardcoded_v2_0_defaults();
        let plat = BeagleBonePlatform::with_config_and_board_target(
            PlatformConfig::s19j_beaglebone(),
            "am3-bb-s19jpro",
            bt,
        );
        assert_eq!(plat.board_target_name(), "am3-bb-s19jpro");
        assert_eq!(plat.board_enable_gpio_v2_0(), 59);
        assert_eq!(plat.chain_reset_gpios_v2_0(), vec![49, 60, 27, 22]);
        assert_eq!(plat.chain_plug_gpios_v2_0(), vec![51, 48, 47, 46]);
        assert_eq!(
            plat.chain_uarts_v2_0(),
            vec![
                "/dev/ttyS1".to_string(),
                "/dev/ttyS2".to_string(),
                "/dev/ttyS4".to_string()
            ]
        );
        assert_eq!(plat.eeprom_i2c_bus(), 0);
        assert_eq!(plat.psu_i2c_bus(), 1);
        assert_eq!(plat.psu_i2c_addr(), 0x10);
        assert_eq!(plat.mining_baud_v2_0(), 3_000_000);
        // voltage_controller() reflects config.voltage_controller — for the
        // `with_config` path it's the static dsPIC default (no `new()`
        // classification ran). Board-target routing happens inside `new()`;
        // here we just assert the W4 accessors still report the
        // legacy values (no regression).
        assert_eq!(BeagleBonePlatform::psu_enable_gpio(), 65);
        assert_eq!(BeagleBonePlatform::chain_reset_gpios(), [5, 4, 27, 22]);
    }

    /// W4 BBCtrl accessors are UNCHANGED — the Phase B `a lab unit` additions must
    /// not have moved the legacy values (the W4 `beaglebone_cold_boot`
    /// path + its tests still depend on them).
    #[test]
    fn w4_bbctrl_accessors_unchanged_by_phase_b() {
        assert_eq!(GPIO_PSU_ENABLE, 65);
        assert_eq!(GPIO_BOARD_RESET, [5, 4, 27, 22]);
        assert_eq!(GPIO_PLUG_DETECT, [51, 48, 47, 44]);
        assert_eq!(BeagleBonePlatform::i2c_bus_number(), 0);
        assert_eq!(BeagleBonePlatform::psu_enable_gpio(), 65);
        assert_eq!(BeagleBonePlatform::chain_reset_gpios(), [5, 4, 27, 22]);
        assert_eq!(BeagleBonePlatform::chain_plug_gpios(), [51, 48, 47, 44]);
        assert_eq!(
            BeagleBonePlatform::chain_uarts(),
            ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"]
        );
    }
}
