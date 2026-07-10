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

pub mod amlogic;
pub mod beaglebone;
pub mod beaglebone_cold_boot;
pub mod config;
pub mod cvitek;
pub mod cvitek_cold_boot;
pub mod cvitek_pinmux;
pub mod stm32mp15;
pub mod subtype;
pub mod zynq;

use crate::i2c::I2cBus;
use crate::{HalError, Result};

pub use config::VoltageControllerKind;

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

/// Abstract fan access interface.
pub trait FanAccess: Send + Sync {
    /// Set fan speed (PWM value, platform-specific range).
    fn set_speed(&self, pwm: u8);

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
    /// When false, get_rpm() returns synthesized estimates and the thermal
    /// controller should NOT use RPM for fan-failure detection — it must
    /// rely solely on temperature thresholds for safety.
    fn tach_available(&self) -> bool {
        true
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

    /// Voltage controller kind in use on this platform.
    ///
    /// Default impl returns `Dspic33Ep` so existing platforms (S9, S19 Pro,
    /// S21, S19k Pro) keep their behavior unchanged. CV1835, BeagleBone,
    /// and Amlogic override this with the `crate::platform::subtype`
    /// classification result so the daemon can construct a `Pic1704Service`
    /// when the subtype + 0x20 probe both pass at runtime.
    ///
    /// W2A.2 (2026-05-09): introduced as part of the PIC1704 wire-up.
    fn voltage_controller(&self) -> VoltageControllerKind {
        VoltageControllerKind::Dspic33Ep
    }
}

impl BoardType {
    /// Voltage-controller default for a control-board family, used when
    /// no `Platform` instance is constructed (CV1835 stub today, more in
    /// future). Live runtime classification happens in
    /// `crate::platform::subtype::classify_with_probe`; this helper is
    /// only the *static* hint for diagnostic / preflight code.
    pub fn voltage_controller_default(&self) -> VoltageControllerKind {
        match self {
            // CV1835 carriers ship BHB42XXX hashboards in the dev kit
            // (`/etc/subtype = CVCtrl_BHB42XXX`). PIC1704 is the static
            // expectation; runtime probe still gates real construction.
            BoardType::CVitek => VoltageControllerKind::Pic1704,
            // Existing platforms — keep current behavior.
            BoardType::Zynq => VoltageControllerKind::Dspic33Ep,
            BoardType::BeagleBone => VoltageControllerKind::Dspic33Ep,
            BoardType::Amlogic => VoltageControllerKind::Dspic33Ep,
            BoardType::Stm32Mp15 => VoltageControllerKind::Dspic33Ep,
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
    // W2B / B1 (2026-05-09): CV1835 platform fully implemented. The
    // signature check inside `cvitek::CViTekPlatform::new()` is the
    // load-bearing safety gate (refuses to construct on non-Sophgo
    // hardware unless `DCENT_CVITEK_ACCEPT_UNVERIFIED=1` is set).
    if std::path::Path::new("/sys/module/uart_trans").exists() {
        return Ok(Box::new(cvitek::CViTekPlatform::new()?));
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
