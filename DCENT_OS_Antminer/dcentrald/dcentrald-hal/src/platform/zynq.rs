//! Zynq platform implementation.
//!
//! Supports the Xilinx Zynq 7010/7020 control boards used in Antminer S9, S17,
//! and S19 series miners. FPGA UART FIFOs are accessed via UIO devices.
//!
//! Two Zynq sub-platforms exist:
//!
//! **S9 (am1-s9):**
//!   - 3 hash chains (6, 7, 8) with 4 UIO devices each
//!   - 1 fan controller UIO device (uio0)
//!   - 1 glitch monitor UIO device (uio13)
//!   - I2C bus 0 for PIC controllers (0x55-0x57)
//!   - UIO names: "chain6-common", "chain7-cmd", etc.
//!
//! **S19 (am2-s17 control board):**
//!   - 4 hash chains (1, 2, 3, 4) with 4 UIO devices each (only 3 physical boards)
//!   - 1 fan controller UIO device (uio16)
//!   - 1 board-control UIO device (uio17)
//!   - 1 glitch monitor UIO device (uio18)
//!   - I2C bus 0 for PIC controllers (0x88/0x89/0xB9/0xFE)
//!   - UIO names: "chain1-common", "chain2-cmd-rx", etc.
//!   - Additional PL UARTs at 0x41001000-0x41031000
//!
//! Auto-discovery: scan /sys/class/uio/uioN/name for device names.
//! The chain naming pattern ("chain6" vs "chain1") determines the sub-platform.

use std::collections::HashMap;
use std::fs;

use super::{BoardType, ChainAccess, FanAccess, GpioAccess, Platform, VoltageControllerKind};
use crate::board_control::BoardControl;
use crate::fan::{FanController, FanVariant};
use crate::fpga_chain::FpgaChain;
use crate::glitch_monitor::BraiinsGlitchMonitor;
use crate::gpio::GpioController;
use crate::i2c::I2cBus;
use crate::{HalError, Result};

/// am2-s17 family PSU gate GPIO.
///
/// Verified from VNish/BraiinsOS research on S17/S19 class Zynq boards:
/// `gpio907` is the shared PSU/power-control output and is active HIGH.
const AM2_S17_PSU_ENABLE_GPIO: u32 = 907;

/// Chain IDs used on S9 boards (match physical connector labels).
const S9_CHAIN_IDS: [u8; 3] = [6, 7, 8];

/// Chain IDs used on S17 boards. The S17 control board is the original
/// am1-s17 SKU — it physically reuses the S9 18-pin hash-board connector
/// and BraiinsOS s9io-style FPGA UIO map, with chains numbered 6/7/8 to
/// match the silkscreen on the control board. Disambiguation from S9 is
/// done via `/etc/dcentos/board_target` ("am1-s17") + dsPIC33EP16GS202
/// detection, NOT chain numbering.
const S17_CHAIN_IDS: [u8; 3] = [6, 7, 8];

/// Chain IDs used on S19/am2-s17 boards (1-indexed, 4 slots in FPGA).
const S19_CHAIN_IDS: [u8; 4] = [1, 2, 3, 4];

/// Zynq sub-platform type (detected from UIO device names + board_target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZynqVariant {
    /// S9 (am1-s9): 3×63 BM1387 chains numbered 6/7/8.
    /// PIC16F1704 voltage controllers at I2C 0x55-0x57. 512 MB RAM.
    S9,
    /// S17 (`board_target` token "am1-s17"): 3×48 BM1397 chains numbered
    /// 6/7/8. dsPIC33EP16GS202 voltage controllers (NOT PIC16F1704). 228 MB
    /// RAM — significantly less than S9, so the daemon must run with a
    /// tighter tokio worker pool and blocking-thread budget.
    /// Disambiguated from S9 via `/etc/dcentos/board_target` ("am1-s17").
    ///
    /// SoC classification (CANONICAL — see memory rule
    /// ): the S17 control
    /// board is a **Zynq 7007S = `am2`** board, NOT `am1` (7010). The
    /// `am1-s17` `board_target` token does NOT assert an am1 SoC — it is a
    /// legacy label meaning "S9-lineage chain layout / s9io-style FPGA UIO
    /// map" (S17 physically reuses the S9 18-pin hash-board connector and
    /// s9io bitstream lineage, hence chains 6/7/8). The Buildroot side of
    /// S17 belongs to the `am2-s17pro-zynq` variant family — there is
    /// deliberately NO `am1-s17` Buildroot defconfig (that would be the
    /// am1/am2 inversion the canonical rule exists to prevent). The
    /// `am1-s17` string is retained only for backward compatibility with
    /// the post-build evidence file + toolbox route keys + the unit tests
    /// below; renaming it is tracked naming debt, not a correctness bug.
    /// XXX: confirm against live S17 — UIO naming pattern is assumed
    /// identical to S9 (chain6/7/8) based on shared 18-pin connector +
    /// s9io bitstream lineage; live UIO scan needed for first S17 unit.
    S17,
    /// S19 (am2-s17 control board): 4 chains numbered 1/2/3/4, UIO names "chain1-*" through "chain4-*"
    /// (also covers Zynq variant of S19j Pro .139)
    S19,
}

impl ZynqVariant {
    /// Fan hardware variant associated with this Zynq sub-platform.
    pub fn fan_variant(self) -> FanVariant {
        match self {
            ZynqVariant::S9 => FanVariant::Am1S9,
            // S17 reuses the s9io fan-control IP (am1-class layout — 2 PWM
            // outputs, 2 tach channels). XXX: confirm against live S17.
            ZynqVariant::S17 => FanVariant::Am1S9,
            ZynqVariant::S19 => FanVariant::Am2Uio16,
        }
    }

    /// Whether this variant has an am2 board-control IP at 0x42810000.
    pub fn has_board_control_ip(self) -> bool {
        matches!(self, ZynqVariant::S19)
    }

    /// Whether this variant has an am2 glitch-monitor IP at 0x43D00000.
    pub fn has_glitch_monitor_ip(self) -> bool {
        matches!(self, ZynqVariant::S19)
    }

    /// Tokio worker-thread count recommended for this variant.
    ///
    /// Derived from `/proc/meminfo` evidence baked into the platform docs:
    /// S9 = 512 MB, S17 = 228 MB, S19 = 512 MB+. The S17 has less than half
    /// the RAM of S9 with the same dual-core CPU, so the daemon should run
    /// with a tighter pool to leave headroom for tmpfs + ASIC drivers +
    /// Stratum + dashboard. Returns 2 for S9/S19 (default for dual-core)
    /// and 2 for S17 (same CPU; the savings comes from the blocking pool).
    pub fn tokio_worker_threads(self) -> usize {
        match self {
            ZynqVariant::S9 | ZynqVariant::S19 => 2,
            ZynqVariant::S17 => 2,
        }
    }

    /// Tokio max-blocking-threads recommended for this variant.
    ///
    /// Tokio's default (512) is wildly out of proportion for a 228 MB device.
    /// Each blocking thread reserves a stack (~2 MB on musl by default), so
    /// the default pool can exhaust virtual memory before any actual blocking
    /// I/O is dispatched. Cap to 4 on S17, leave the default elsewhere.
    pub fn tokio_max_blocking_threads(self) -> usize {
        match self {
            ZynqVariant::S17 => 4,
            // 512 = tokio default. We don't override S9/S19 in this gate.
            ZynqVariant::S9 | ZynqVariant::S19 => 512,
        }
    }
}

/// UIO device info discovered from sysfs.
#[derive(Debug, Clone)]
pub struct UioInfo {
    /// UIO device number (e.g., 0 for /dev/uio0).
    pub number: u8,
    /// Device name from sysfs.
    pub name: String,
}

/// Zynq platform implementation.
pub struct ZynqPlatform {
    /// UIO device number for the fan controller.
    fan_uio: u8,
    /// UIO device number for the am2 board-control IP (None on S9).
    board_control_uio: Option<u8>,
    /// UIO device number for the am2 glitch-monitor IP (None on S9).
    glitch_monitor_uio: Option<u8>,
    /// UIO base numbers for each chain (chain_id -> uio_base).
    chain_uio_bases: HashMap<u8, u8>,
    /// Detected Zynq sub-platform (S9 vs S19).
    variant: ZynqVariant,
}

impl ZynqPlatform {
    /// Create a new Zynq platform instance.
    ///
    /// Scans UIO devices and builds the device map. Auto-detects whether this
    /// is an S9 (am1-s9) or S19 (am2-s17) control board based on UIO device
    /// naming patterns and device count.
    pub fn new() -> Result<Self> {
        let devices = scan_uio_devices()?;
        tracing::info!(count = devices.len(), "Discovered UIO devices");

        for dev in &devices {
            tracing::debug!(uio = dev.number, name = %dev.name, "UIO device");
        }

        // Detect Zynq sub-platform from UIO naming pattern.
        // S19/am2-s17 uses 1-indexed chain names ("chain1-common", "chain2-cmd-rx")
        // S9 uses connector-numbered chain names ("chain6-common", "chain7-cmd")
        // Also: S19 has 19 UIO devices, S9 has ~14.
        let variant = detect_zynq_variant(&devices).ok_or_else(|| {
            HalError::Platform(
                "Zynq variant detection inconclusive; refusing to default to S9".into(),
            )
        })?;
        tracing::info!(variant = ?variant, "Detected Zynq sub-platform");

        let chain_ids: &[u8] = match variant {
            ZynqVariant::S9 => &S9_CHAIN_IDS,
            ZynqVariant::S17 => &S17_CHAIN_IDS,
            ZynqVariant::S19 => &S19_CHAIN_IDS,
        };

        // Find fan controller
        let fan_uio = find_uio_by_pattern(&devices, "fan")
            .ok_or_else(|| HalError::Platform("fan controller UIO not found".into()))?;

        // am2-only IP blocks — optional (absent on S9 am1-s9 bitstream).
        let board_control_uio = find_uio_by_pattern(&devices, "board-control");
        let glitch_monitor_uio = find_uio_by_pattern(&devices, "glitch-monitor")
            .or_else(|| find_uio_by_pattern(&devices, "glitch"));

        if variant == ZynqVariant::S19 {
            if board_control_uio.is_none() {
                tracing::warn!(
                    "am2 variant detected but no 'board-control' UIO device found — \
                     reset pulses and PSU hardware-enable will be unavailable"
                );
            }
            if glitch_monitor_uio.is_none() {
                tracing::warn!(
                    "am2 variant detected but no 'miner-glitch-monitor' UIO device found — \
                     UART/I2C glitch telemetry will be unavailable"
                );
            }
        }

        // Find chain UIO bases.
        // UIO names contain "chain<N>" where N is the chain ID.
        // Each chain has 4 UIO devices: common, cmd(-rx), work-rx, work-tx.
        // The lowest UIO number in the group is the base.
        let mut chain_uio_bases = HashMap::new();

        for &chain_id in chain_ids {
            let pattern = format!("chain{}", chain_id);
            let chain_devices: Vec<&UioInfo> = devices
                .iter()
                .filter(|d| d.name.contains(&pattern))
                .collect();

            if chain_devices.len() >= 4 {
                let mut sorted: Vec<u8> = chain_devices.iter().map(|d| d.number).collect();
                sorted.sort();
                chain_uio_bases.insert(chain_id, sorted[0]);
                tracing::info!(
                    chain_id,
                    uio_base = sorted[0],
                    "Mapped chain to UIO devices"
                );
            } else if !chain_devices.is_empty() {
                tracing::warn!(
                    chain_id,
                    found = chain_devices.len(),
                    "Incomplete chain UIO devices"
                );
            }
        }

        // Fallback: if name-based discovery fails, use positional mapping
        if chain_uio_bases.is_empty() {
            match variant {
                ZynqVariant::S9 if devices.len() >= 12 => {
                    tracing::warn!("Name-based UIO discovery failed, using S9 positional fallback");
                    chain_uio_bases.insert(6, 0);
                    chain_uio_bases.insert(7, 4);
                    chain_uio_bases.insert(8, 8);
                }
                // S17 shares the s9io am1-class UIO layout (3 chains × 4 UIO
                // devices each). XXX: confirm against live S17 — fallback
                // assumes identical positional mapping to S9.
                ZynqVariant::S17 if devices.len() >= 12 => {
                    tracing::warn!(
                        "Name-based UIO discovery failed, using S17 positional fallback"
                    );
                    chain_uio_bases.insert(6, 0);
                    chain_uio_bases.insert(7, 4);
                    chain_uio_bases.insert(8, 8);
                }
                ZynqVariant::S19 if devices.len() >= 16 => {
                    tracing::warn!(
                        "Name-based UIO discovery failed, using S19 positional fallback"
                    );
                    chain_uio_bases.insert(1, 0);
                    chain_uio_bases.insert(2, 4);
                    chain_uio_bases.insert(3, 8);
                    chain_uio_bases.insert(4, 12);
                }
                _ => {}
            }
        }

        if chain_uio_bases.is_empty() {
            return Err(HalError::Platform("no hash chain UIO devices found".into()));
        }

        Ok(Self {
            fan_uio,
            board_control_uio,
            glitch_monitor_uio,
            chain_uio_bases,
            variant,
        })
    }

    /// Get the detected Zynq sub-platform variant.
    pub fn variant(&self) -> ZynqVariant {
        self.variant
    }

    /// Open the am2 board-control IP (hashboard reset, plug-detect, PSU enable).
    ///
    /// Returns `None` on am1-s9 (this IP does not exist on S9 bitstream).
    pub fn open_board_control(&self) -> Result<Option<BoardControl>> {
        match self.board_control_uio {
            Some(n) => BoardControl::open(n).map(Some),
            None => Ok(None),
        }
    }

    /// Open the am2 glitch-monitor IP (passive read-only telemetry).
    ///
    /// Returns `None` on am1-s9 (this IP does not exist on S9 bitstream).
    pub fn open_glitch_monitor(&self) -> Result<Option<BraiinsGlitchMonitor>> {
        match self.glitch_monitor_uio {
            Some(n) => BraiinsGlitchMonitor::open(n).map(Some),
            None => Ok(None),
        }
    }

    /// UIO device numbers for the am2 IP blocks, for diagnostics.
    pub fn am2_uio_numbers(&self) -> (Option<u8>, Option<u8>) {
        (self.board_control_uio, self.glitch_monitor_uio)
    }
}

fn ensure_sysfs_gpio_exported(gpio: u32) -> Result<()> {
    let gpio_dir = format!("/sys/class/gpio/gpio{}", gpio);
    if !std::path::Path::new(&gpio_dir).exists() {
        fs::write("/sys/class/gpio/export", format!("{}", gpio))
            .map_err(|e| HalError::Platform(format!("failed to export GPIO {}: {}", gpio, e)))?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

fn set_sysfs_gpio_value(gpio: u32, high: bool) -> Result<()> {
    ensure_sysfs_gpio_exported(gpio)?;
    let dir_path = format!("/sys/class/gpio/gpio{}/direction", gpio);
    let value_path = format!("/sys/class/gpio/gpio{}/value", gpio);
    fs::write(&dir_path, "out")
        .map_err(|e| HalError::Platform(format!("GPIO {} direction: {}", gpio, e)))?;
    fs::write(&value_path, if high { "1" } else { "0" })
        .map_err(|e| HalError::Platform(format!("GPIO {} value: {}", gpio, e)))?;
    Ok(())
}

/// Enable the shared APW output gate on am2-s17 family Zynq boards.
pub fn enable_psu_output() -> Result<()> {
    set_sysfs_gpio_value(AM2_S17_PSU_ENABLE_GPIO, true)?;
    tracing::info!(
        gpio = AM2_S17_PSU_ENABLE_GPIO,
        "am2-s17 PSU output gate enabled"
    );
    Ok(())
}

/// Disable the shared APW output gate on am2-s17 family Zynq boards.
pub fn disable_psu_output() -> Result<()> {
    set_sysfs_gpio_value(AM2_S17_PSU_ENABLE_GPIO, false)?;
    tracing::info!(
        gpio = AM2_S17_PSU_ENABLE_GPIO,
        "am2-s17 PSU output gate disabled"
    );
    Ok(())
}

/// Read the current state of the shared APW output gate on am2-s17 boards.
pub fn is_psu_output_enabled() -> bool {
    let value_path = format!("/sys/class/gpio/gpio{}/value", AM2_S17_PSU_ENABLE_GPIO);
    fs::read_to_string(&value_path)
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

impl Platform for ZynqPlatform {
    fn board_type(&self) -> BoardType {
        BoardType::Zynq
    }

    fn chain_count(&self) -> u8 {
        self.chain_uio_bases.len() as u8
    }

    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>> {
        let uio_base = self
            .chain_uio_bases
            .get(&chain_id)
            .ok_or_else(|| HalError::Platform(format!("chain {} not found", chain_id)))?;

        let chain = FpgaChain::open(chain_id, *uio_base)?;
        Ok(Box::new(ZynqChainAccess { chain }))
    }

    fn open_i2c(&self, bus: u8) -> Result<I2cBus> {
        I2cBus::open(bus)
    }

    fn open_fan(&self) -> Result<Box<dyn FanAccess>> {
        // Route to the correct fan backend: am1-s9 uses the integrated s9io
        // 2-channel layout; am2-s17 uses the dedicated 4-channel fan-control
        // IP at 0x42800000 uio16.
        let fan_variant = self.variant.fan_variant();
        let fan = FanController::open_with_variant(self.fan_uio, fan_variant)?;
        tracing::info!(
            uio = self.fan_uio,
            variant = ?fan_variant,
            "Opened fan controller with detected variant"
        );
        Ok(Box::new(fan))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        // CE-005: wrap the existing AXI-GPIO `/dev/mem` controller (gpio.rs).
        // The Zynq bitstream exposes hash-board plug-detect (input bank
        // 0x41200000 bits 5-7) and per-chain RESET (output bank 0x41210000
        // bits 9-11) as memory-mapped AXI GPIO. `GpioController::new()` mmaps
        // both banks; this just adapts it to the `GpioAccess` trait. Reads are
        // always safe; the only write path is `set_board_reset`, which RMWs a
        // single reset bit (never touches the PWR_CONTROL/PSU gate — that lives
        // on a different bank and is owned by the PSU module).
        let controller = GpioController::new()?;
        Ok(Box::new(ZynqGpioAccess { controller }))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        // `new()` refuses an inconclusive UIO/board-target identity, so this
        // explicit compatibility value does not rely on the fail-closed trait
        // default or on control-board family alone.
        match self.variant {
            ZynqVariant::S9 => VoltageControllerKind::Pic16f1704,
            ZynqVariant::S17 | ZynqVariant::S19 => VoltageControllerKind::Dspic33Ep,
        }
    }
}

/// `GpioAccess` adapter over the AXI-GPIO `/dev/mem` controller.
///
/// Bridges the platform-neutral [`GpioAccess`] trait to [`GpioController`]
/// (gpio.rs). The chain index is the GpioController 0/1/2 convention (0=J6,
/// 1=J7, 2=J8), matching the `set_board_enable` mapping — NOT the FPGA UIO
/// chain IDs (6/7/8 on S9, 1-4 on am2).
struct ZynqGpioAccess {
    controller: GpioController,
}

impl GpioAccess for ZynqGpioAccess {
    fn read_plug_detect(&self) -> [bool; 3] {
        self.controller.read_plug_detect()
    }

    fn set_board_reset(&self, chain: u8, assert_reset: bool) {
        // Trait semantics: `assert_reset == true` holds the ASICs in reset.
        // GpioController::set_board_enable takes `enable` (true = release
        // reset / run), so invert: enable = !assert_reset.
        self.controller.set_board_enable(chain, !assert_reset);
    }
}

/// Wrapper to implement ChainAccess trait for FpgaChain.
struct ZynqChainAccess {
    chain: FpgaChain,
}

impl ChainAccess for ZynqChainAccess {
    fn send_command(&self, data: &[u8]) -> Result<()> {
        // Pack bytes into 32-bit words (little-endian, LSB-first)
        for chunk in data.chunks(4) {
            let mut word = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                word |= (byte as u32) << (i * 8);
            }
            self.chain.write_cmd(word);
        }
        Ok(())
    }

    fn read_response(&self, buf: &mut [u8]) -> Result<usize> {
        let mut pos = 0;
        while pos < buf.len() {
            if let Some(word) = self.chain.read_cmd_response() {
                let bytes = word.to_le_bytes();
                let remaining = buf.len() - pos;
                let copy_len = remaining.min(4);
                buf[pos..pos + copy_len].copy_from_slice(&bytes[..copy_len]);
                pos += copy_len;
            } else {
                break; // FIFO empty
            }
        }
        Ok(pos)
    }

    fn send_work(&self, data: &[u8]) -> Result<()> {
        // Pack bytes into 32-bit words
        let mut words = Vec::with_capacity(data.len() / 4 + 1);
        for chunk in data.chunks(4) {
            let mut word = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                word |= (byte as u32) << (i * 8);
            }
            words.push(word);
        }
        self.chain.write_work(&words);
        Ok(())
    }

    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize> {
        if let Some((word0, word1)) = self.chain.read_nonce() {
            let w0_bytes = word0.to_le_bytes();
            let w1_bytes = word1.to_le_bytes();
            let copy_len = buf.len().min(8);
            if copy_len >= 4 {
                buf[0..4].copy_from_slice(&w0_bytes);
            }
            if copy_len >= 8 {
                buf[4..8].copy_from_slice(&w1_bytes);
            }
            Ok(copy_len)
        } else {
            Ok(0)
        }
    }

    fn set_baud(&self, baud: u32) -> Result<()> {
        let divisor = FpgaChain::divisor_from_baud(baud);
        self.chain.set_baud(divisor);
        tracing::debug!(
            baud,
            divisor,
            actual = FpgaChain::baud_from_divisor(divisor),
            "Set chain baud rate"
        );
        Ok(())
    }

    fn wait_for_nonce(&self) -> Result<()> {
        // Poll work RX FIFO status
        // In a production implementation, this would use IRQ via UIO
        while !self.chain.work_rx_has_data() {
            std::thread::yield_now();
        }
        Ok(())
    }
}

// NOTE: ZynqHybridChainAccess (serial commands + FPGA work) will be added
// when S19 hybrid transport support is implemented. For now, S19 uses the
// same FpgaUio transport as S9 (the BraiinsOS FPGA bitstream provides
// both cmd FIFOs and work FIFOs for all chain slots).

/// Implement FanAccess for FanController.
impl FanAccess for FanController {
    fn set_speed(&self, pwm: u8) {
        FanController::set_speed(self, pwm);
    }

    fn get_rpm(&self) -> u32 {
        FanController::get_rpm(self)
    }

    fn get_speed_pwm(&self) -> u8 {
        FanController::get_speed_pwm(self)
    }

    fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        FanController::get_per_fan_rpm(self)
    }

    fn fan_count(&self) -> u8 {
        // Dynamic: matches get_per_fan_rpm() which skips Fan 0 if not connected
        self.get_per_fan_rpm().len() as u8
    }
}

/// Scan /sys/class/uio/ for all available UIO devices.
fn scan_uio_devices() -> Result<Vec<UioInfo>> {
    let uio_dir = "/sys/class/uio";
    let mut devices = Vec::new();

    let entries = fs::read_dir(uio_dir).map_err(|e| HalError::DeviceOpen {
        path: uio_dir.to_string(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(HalError::Io)?;
        let dir_name = entry.file_name().to_string_lossy().to_string();

        // Parse "uioN" to get the number
        if let Some(num_str) = dir_name.strip_prefix("uio") {
            if let Ok(number) = num_str.parse::<u8>() {
                let name_path = format!("{}/{}/name", uio_dir, dir_name);
                let name = fs::read_to_string(&name_path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| format!("uio{}", number));

                devices.push(UioInfo { number, name });
            }
        }
    }

    devices.sort_by_key(|d| d.number);
    Ok(devices)
}

/// Find a UIO device number by name pattern.
fn find_uio_by_pattern(devices: &[UioInfo], pattern: &str) -> Option<u8> {
    devices
        .iter()
        .find(|d| d.name.to_lowercase().contains(pattern))
        .map(|d| d.number)
}

fn normalize_model_token(model: &str) -> String {
    model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect()
}

/// Detect whether this Zynq board is an S9 (am1-s9), S17 (am1-s17),
/// or S19 (am2-s17 control board, includes Zynq-variant S19j Pro).
///
/// Detection strategy (in priority order):
/// 1. `/etc/dcentos/board_target` (Buildroot post-build evidence — most
///    authoritative; `am1-s9` / `am1-s17` / `am2-s19j`).
/// 2. UIO device names + count: S19 has "chain1-*" through "chain4-*"
///    (≥19 UIO devices); S9 / S17 have "chain6-*"/"chain7-*"/"chain8-*"
///    (~12-14 UIO devices). UIO names alone CANNOT distinguish S9 from S17
///    because both reuse the same s9io bitstream layout — the disambiguator
///    is `board_target` or the device-tree model.
/// 3. Device-tree model string: "am1-s9", "am1-s17", "am2-s17", etc.
///
/// Returns `None` when detection is ambiguous so the constructor refuses
/// chain init instead of routing to a default fan/voltage backend.
fn detect_zynq_variant(devices: &[UioInfo]) -> Option<ZynqVariant> {
    let board_target = read_board_target_string();
    let dt_model = std::fs::read_to_string("/proc/device-tree/model").ok();
    detect_zynq_variant_from_evidence(devices, board_target.as_deref(), dt_model.as_deref())
}

fn detect_zynq_variant_from_evidence(
    devices: &[UioInfo],
    board_target: Option<&str>,
    dt_model: Option<&str>,
) -> Option<ZynqVariant> {
    if let Some(v) = detect_zynq_variant_from_board_target(board_target) {
        tracing::info!(variant = ?v, source = "board_target", "Zynq variant detected");
        return Some(v);
    }

    // Check UIO names: S19 uses "chain1-*", S9/S17 use "chain6-*".
    let has_chain1 = devices.iter().any(|d| d.name.contains("chain1"));
    let has_chain6 = devices.iter().any(|d| d.name.contains("chain6"));

    if has_chain1 && !has_chain6 {
        tracing::info!(source = "uio_chain1", "Zynq variant detected: S19");
        return Some(ZynqVariant::S19);
    }

    if has_chain1 {
        tracing::warn!(
            source = "uio_chain1_conflict",
            "Zynq variant detected as S19 from am2 chain UIO names despite mixed chain evidence"
        );
        return Some(ZynqVariant::S19);
    }

    // Check device count: S19 has 19 UIO devices (4 chains * 4 + 3 system).
    if devices.len() >= 19 {
        tracing::info!(
            source = "uio_count",
            count = devices.len(),
            "Zynq variant detected: S19"
        );
        return Some(ZynqVariant::S19);
    }

    // Try device tree model string. Note: am2-s17 control boards (S19/S19j)
    // also report "S17" so we must check the am2/am1 prefix BEFORE the
    // chip-family token. Order matters here.
    if let Some(model) = dt_model {
        let model = model.trim().trim_end_matches('\0');
        let normalized = normalize_model_token(model);
        if let Some(v) = detect_zynq_variant_from_dt_model(&normalized) {
            tracing::info!(variant = ?v, source = "device_tree", model = %model, "Zynq variant detected");
            return Some(v);
        }
    }

    if has_chain6 {
        tracing::info!(source = "uio_chain6", "Zynq variant detected: S9");
        return Some(ZynqVariant::S9);
    }
    tracing::warn!("Zynq variant detection inconclusive; refusing default route");
    None
}

/// Read `/etc/dcentos/board_target` if present. Trimmed string or None.
fn read_board_target_string() -> Option<String> {
    fs::read_to_string("/etc/dcentos/board_target")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Pure helper: map a `board_target` value to a `ZynqVariant`.
///
/// Returns `None` for non-Zynq targets (am3-aml / am3-bb / am3-s19k / am3-s21
/// / etc.) so the platform constructor falls through to UIO/DT detection
/// rather than misclassifying.
///
/// SoC-mapping note (CANONICAL, ):
/// `am1` = Zynq 7010 (S9 family), `am2` = Zynq 7007S (S17/S19/S19j family).
///
/// Token contract — read before touching the match arm:
/// - `"am2-s17"` is the LIVE legacy control-board identifier for the
///   **S19-family** am2 boards (the silkscreen on this control board says
///   "S17", but the silicon is a 7007S running S19/S19 Pro/S19j Pro hash
///   boards). It is a live, tested, used token and is pinned to
///   `ZynqVariant::S19` below — never repurpose it for the S17 *miner*.
/// - `"am1-s17"` is a legacy chain-layout label (S9-lineage s9io UIO map),
///   NOT an am1 SoC assertion — S17 silicon is a 7007S = am2. Pinned to
///   `ZynqVariant::S17`.
/// - The S17 *miner* (BM1397, the actual S17/S17 Pro chassis) uses the
///   variant key `"am2-s17p"` — the Phase 2E `am2-s17pro-zynq` Buildroot
///   variant's `board_target` — never plain `"am2-s17"`. Pinned to
///   `ZynqVariant::S17` (Phase 2K / DevOps-F3).
/// Do NOT remap `am1-s17` or `am2-s17` to a new defconfig here: those
/// tokens are the contract written by the post-build overlays, consumed by
/// toolbox route keys, and pinned by the unit tests in this module.
fn detect_zynq_variant_from_board_target(target: Option<&str>) -> Option<ZynqVariant> {
    let token = target?.trim();
    match token {
        "am1-s9" => Some(ZynqVariant::S9),
        // "am1-s17" = S9-lineage chain layout label; "am2-s17p" = the S17
        // miner (BM1397) Phase 2E am2-s17pro-zynq board_target. Both map to
        // ZynqVariant::S17. NOTE: plain "am2-s17" below is the S19-family
        // legacy control-board id and stays ZynqVariant::S19.
        "am1-s17"
        | "am2-s17p"
        | "am2-s17plus"
        | "am2-t17"
        | "am2-t17plus"
        | "x17-s17e-dspic-planned"
        | "x17-t17e-pic16-planned" => Some(ZynqVariant::S17),
        // "am2-s19" = Antminer S19 BASE (S19 Standard) board_target. It is the
        // toolbox `stock-am2-s19-*` route's board_target and rides the same
        // am2/BM1398/ZynqVariant::S19 path as S19 Pro (differing only in
        // binning, which dcentrald enumerates at runtime). Made first-class
        // here 2026-07-02 so a base-S19 resolves via the authoritative
        // board_target dispatch, not only the weaker DT-model heuristic.
        // `am2-s19jpro-zynq` is the CANONICAL beta S19j Pro board_target (
        // skus.conf, the beta gate) — it MUST resolve here, not fall through to the
        // S9 fail-safe. The overlay currently stamps the shorter `am2-s19j` (also
        // matched), but a unit stamping the canonical string would otherwise be
        // mis-routed to the S9 variant (wrong chain init on a BM1362 board -> no
        // mining). Additive: cannot affect the proven `am2-s19j` path.
        "am2-s17" | "am2-s19" | "am2-s19j" | "am2-s19jpro" | "am2-s19jpro-zynq" | "am2-s19pro"
        | "am2-t19" => Some(ZynqVariant::S19),
        _ => None,
    }
}

/// Pure helper: map a normalized device-tree model token to a `ZynqVariant`.
///
/// `model` is expected to be lowercased + alnum-only (see `normalize_model_token`).
/// Order: am2 prefix wins (it's the S19-class control board even when the
/// silkscreen says "S17"); then am1-s17; then am1-s9; then chip-family
/// fallbacks (S19/T19 → S19, S17/T17 → S17, S9/T9 → S9).
fn detect_zynq_variant_from_dt_model(model: &str) -> Option<ZynqVariant> {
    if model.contains("am2") {
        return Some(ZynqVariant::S19);
    }
    if model.contains("am1s17") {
        return Some(ZynqVariant::S17);
    }
    if model.contains("am1s9") {
        return Some(ZynqVariant::S9);
    }
    // Chip-family fallbacks. The am2/am1 prefix is preferred above; these
    // run only when no platform prefix is present in the model string.
    if model.contains("s19") || model.contains("t19") {
        return Some(ZynqVariant::S19);
    }
    if model.contains("s17") || model.contains("t17") {
        return Some(ZynqVariant::S17);
    }
    if model.contains("s9") || model.contains("t9") {
        return Some(ZynqVariant::S9);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uio(number: u8, name: &str) -> UioInfo {
        UioInfo {
            number,
            name: name.to_string(),
        }
    }

    #[test]
    fn test_zynq_variant_s17_disambiguation() {
        // board_target file is the most authoritative — assert all three
        // canonical strings disambiguate cleanly.
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am1-s9")),
            Some(ZynqVariant::S9)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am1-s17")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s17")),
            Some(ZynqVariant::S19)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s19j")),
            Some(ZynqVariant::S19)
        );
        // The CANONICAL beta S19j Pro board_target (skus.conf /  /
        // beta gate) MUST resolve to S19, not fall through to the S9 fail-safe.
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s19jpro-zynq")),
            Some(ZynqVariant::S19)
        );
        // Phase 2D am2-s19pro-zynq variant board_target — pins to S19.
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s19pro")),
            Some(ZynqVariant::S19)
        );
        // Antminer S19 BASE (S19 Standard) board_target — first-class 2026-07-02
        // (was previously None → resolved only via the DT-model heuristic).
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s19")),
            Some(ZynqVariant::S19)
        );
        // Phase 2E am2-s17pro-zynq variant board_target — the S17 miner
        // (BM1397) MUST route to ZynqVariant::S17, NOT fall through to the
        // S19 DT-model branch (DevOps-F3 / Phase 2K). "am2-s17p" is the
        // literal string written by board/zynq/am2-s17pro/post-build.sh.
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s17p")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-s17plus")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-t17")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-t17plus")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("x17-s17e-dspic-planned")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("x17-t17e-pic16-planned")),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am2-t19")),
            Some(ZynqVariant::S19)
        );

        // Trim whitespace (Buildroot post-build writes a trailing newline).
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("  am1-s17\n")),
            Some(ZynqVariant::S17)
        );

        // Non-Zynq platforms must return None so the platform constructor
        // can fall through to UIO/DT detection rather than misclassify.
        assert_eq!(detect_zynq_variant_from_board_target(Some("am3-bb")), None);
        assert_eq!(detect_zynq_variant_from_board_target(Some("am3-s21")), None);
        assert_eq!(
            detect_zynq_variant_from_board_target(Some("am3-s19k")),
            None
        );
        assert_eq!(detect_zynq_variant_from_board_target(Some("")), None);
        assert_eq!(detect_zynq_variant_from_board_target(None), None);
    }

    #[test]
    fn board_target_resolution_fails_closed_on_garbage() {
        // A corrupt / malformed / partial board_target read (bit-rot, a truncated
        // file, a hand-edit typo, a near-miss non-canonical token) MUST resolve to
        // None so the caller falls through to UIO/DT detection and ultimately
        // refuses if no independent evidence exists. A garbage token must never
        // be silently accepted as a known variant — accepting one
        // that mapped an S9 board to ZynqVariant::S19 would command S19's ~13.7 V
        // onto S9 (~9.1 V) silicon.
        // NOTE: `am2-s19jpro-zynq` is NOT garbage — it is the canonical beta S19j
        // Pro board_target and MUST route to S19 (asserted positively below). It was
        // wrongly listed here before; a genuine near-miss like `am2-s19jZZZ` covers
        // the fail-closed case instead.
        for bad in [
            "garbage",
            "am2-xyz",
            "am1-s99",
            "s19",
            "am2",
            "am1",
            "am2-s19jZZZ",
            "AM1-S9",
            "am1_s9",
            "am2-s19jpro-zynqXX",
            "0x1387",
            "-",
            "  \t ",
            "\0",
        ] {
            assert_eq!(
                detect_zynq_variant_from_board_target(Some(bad)),
                None,
                "garbage board_target '{bad}' must fail closed (None), not route to a variant"
            );
        }
    }

    #[test]
    fn test_zynq_variant_dt_model_disambiguation() {
        // am2 prefix wins over chip family — S19/S19j control boards report
        // both "am2" and "S17" in their device-tree model strings.
        assert_eq!(
            detect_zynq_variant_from_dt_model("am2s17minercontrolboard"),
            Some(ZynqVariant::S19)
        );
        assert_eq!(
            detect_zynq_variant_from_dt_model("am1s17minercontrolboard"),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_dt_model("am1s9minercontrolboard"),
            Some(ZynqVariant::S9)
        );

        // Chip-family fallbacks (no am1/am2 prefix in DT).
        assert_eq!(
            detect_zynq_variant_from_dt_model("antminers17pro"),
            Some(ZynqVariant::S17)
        );
        assert_eq!(
            detect_zynq_variant_from_dt_model("antminers9"),
            Some(ZynqVariant::S9)
        );
    }

    #[test]
    fn zynq_uio_chain1_routes_to_s19_fan_controller() {
        let devices = [uio(1, "chain1-common"), uio(16, "fan-control")];

        let variant = detect_zynq_variant_from_evidence(&devices, None, None);

        assert_eq!(variant, Some(ZynqVariant::S19));
        assert_eq!(variant.unwrap().fan_variant(), FanVariant::Am2Uio16);
    }

    #[test]
    fn zynq_mixed_chain_evidence_prefers_am2_over_s9_default() {
        let devices = [
            uio(1, "chain1-common"),
            uio(6, "chain6-common"),
            uio(16, "fan-control"),
        ];

        assert_eq!(
            detect_zynq_variant_from_evidence(&devices, None, None),
            Some(ZynqVariant::S19)
        );
    }

    #[test]
    fn zynq_chain6_or_dt_can_still_route_s9_s17_explicitly() {
        assert_eq!(
            detect_zynq_variant_from_evidence(&[uio(6, "chain6-common")], None, None),
            Some(ZynqVariant::S9)
        );
        assert_eq!(
            detect_zynq_variant_from_evidence(
                &[uio(6, "chain6-common")],
                None,
                Some("Antminer S17 Pro")
            ),
            Some(ZynqVariant::S17)
        );
    }

    #[test]
    fn zynq_inconclusive_uio_detection_refuses_default_variant() {
        assert_eq!(detect_zynq_variant_from_evidence(&[], None, None), None);
        assert_eq!(
            detect_zynq_variant_from_evidence(&[uio(0, "fan-control")], None, None),
            None
        );
    }

    #[test]
    fn test_tokio_config_per_variant() {
        // S17's smaller RAM (228 MB vs 512 MB on S9) demands a tighter
        // blocking pool. Workers stay at 2 (dual-core CPU shared with S9/S19).
        assert_eq!(ZynqVariant::S17.tokio_worker_threads(), 2);
        assert_eq!(ZynqVariant::S17.tokio_max_blocking_threads(), 4);
        assert_eq!(ZynqVariant::S9.tokio_worker_threads(), 2);
        assert_eq!(ZynqVariant::S9.tokio_max_blocking_threads(), 512);
        assert_eq!(ZynqVariant::S19.tokio_worker_threads(), 2);
        assert_eq!(ZynqVariant::S19.tokio_max_blocking_threads(), 512);
    }

    #[test]
    fn test_fan_variant_per_zynq_variant() {
        // S17 reuses am1-class fan-control IP (s9io bitstream lineage).
        // XXX: confirm against live S17.
        assert_eq!(ZynqVariant::S9.fan_variant(), FanVariant::Am1S9);
        assert_eq!(ZynqVariant::S17.fan_variant(), FanVariant::Am1S9);
        assert_eq!(ZynqVariant::S19.fan_variant(), FanVariant::Am2Uio16);
    }

    #[test]
    fn test_am2_specific_ips_only_on_s19() {
        // board-control + glitch-monitor IPs are am2 bitstream-only.
        assert!(!ZynqVariant::S9.has_board_control_ip());
        assert!(!ZynqVariant::S17.has_board_control_ip());
        assert!(ZynqVariant::S19.has_board_control_ip());

        assert!(!ZynqVariant::S9.has_glitch_monitor_ip());
        assert!(!ZynqVariant::S17.has_glitch_monitor_ip());
        assert!(ZynqVariant::S19.has_glitch_monitor_ip());
    }
}
