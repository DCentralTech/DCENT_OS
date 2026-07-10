//! FPGA interface module for dcentrald mining daemon.
//!
//! Provides the top-level FPGA abstraction that ties together UIO device
//! management, multi-chain orchestration, interrupt-driven I/O, and the
//! complete register interface for the Braiins s9io v1.0.2 FPGA bitstream.
//!
//! This module is the single point of contact between dcentrald's async
//! mining pipeline and the FPGA hardware. It owns all UIO mappings and
//! provides safe, structured access to:
//!   - Chain controllers (3 chains on S9: chain 6/7/8)
//!   - Fan PWM controller
//!   - GPIO (plug detect, board enable, LEDs)
//!   - Glitch monitor
//!
//! # Architecture
//!
//! The S9 FPGA has 14 UIO devices (verified from live probe):
//!
//! ```text
//! uio0:  fan-control       0x42800000  AXI Timer PWM
//! uio1:  chain6-common     0x43C00000  Common registers (VERSION, CTRL, BAUD, etc.)
//! uio2:  chain6-cmd-rx     0x43C01000  CMD RX+TX FIFOs (ASIC register access)
//! uio3:  chain6-work-rx    0x43C02000  Work RX FIFO (nonce responses)
//! uio4:  chain6-work-tx    0x43C03000  Work TX FIFO (job submission)
//! uio5:  chain7-common     0x43C10000
//! uio6:  chain7-cmd-rx     0x43C11000
//! uio7:  chain7-work-rx    0x43C12000
//! uio8:  chain7-work-tx    0x43C13000
//! uio9:  chain8-common     0x43C20000
//! uio10: chain8-cmd-rx     0x43C21000
//! uio11: chain8-work-rx    0x43C22000
//! uio12: chain8-work-tx    0x43C23000
//! uio13: miner-glitch-monitor 0x43D00000
//! ```
//!
//! # FPGA Clock
//!
//! The FPGA fabric runs at 200 MHz (100 MHz FCLK doubled by PL PLL).
//! Baud rate formula: `baud = 200_000_000 / (16 * (BAUD_REG + 1))`
//!
//! # Safety
//!
//! Accessing unmapped FPGA address space causes AXI external abort faults
//! that crash the process. All register access is bounds-checked to the 4 KB
//! UIO mapping. Stay within documented register offsets.

use std::fmt;
use std::fs;
use std::os::fd::RawFd;

use dcentrald_hal::fan::FanController;
use dcentrald_hal::fpga_chain::{self, FpgaChain};
use dcentrald_hal::gpio::GpioController;
use dcentrald_hal::uio::UioDevice;
use dcentrald_hal::HalError;

use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// S9 hardware constants (verified from live probe)
// ---------------------------------------------------------------------------

/// Number of hash chains on a Zynq S9 control board.
pub const S9_CHAIN_COUNT: usize = 3;

/// Chain IDs matching physical connector labels (J6, J7, J8).
pub const S9_CHAIN_IDS: [u8; S9_CHAIN_COUNT] = [6, 7, 8];

/// UIO device base numbers for each chain (4 consecutive UIO devices per chain).
/// Verified from live S9: uio1-4 = chain6, uio5-8 = chain7, uio9-12 = chain8.
pub const S9_UIO_BASES: [u8; S9_CHAIN_COUNT] = [1, 5, 9];

/// UIO device number for the fan controller.
pub const FAN_UIO: u8 = 0;

/// UIO device number for the glitch monitor.
pub const GLITCH_MONITOR_UIO: u8 = 13;

/// FPGA fabric clock frequency in Hz.
/// 100 MHz FCLK doubled by PL PLL = 200 MHz.
pub const FPGA_CLK_HZ: u32 = 200_000_000;

/// PIC I2C addresses for S9 chains 6, 7, 8 (verified from live probe).
pub const S9_PIC_ADDRS: [u8; S9_CHAIN_COUNT] = [0x55, 0x56, 0x57];

// ---------------------------------------------------------------------------
// FPGA version identification
// ---------------------------------------------------------------------------

/// Decoded FPGA version info from the VERSION register.
#[derive(Debug, Clone)]
pub struct FpgaVersion {
    /// Raw register value.
    pub raw: u32,
    /// Miner model identifier (e.g., 0x09 for S9).
    pub model: u8,
    /// Major version.
    pub major: u8,
    /// Minor version.
    pub minor: u8,
    /// Patch level.
    pub patch: u8,
}

impl FpgaVersion {
    /// Decode the VERSION register (format verified from live S9: 0x00901002).
    pub fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            model: ((raw >> 20) & 0xFF) as u8,
            major: ((raw >> 12) & 0x0F) as u8,
            minor: ((raw >> 8) & 0x0F) as u8,
            patch: (raw & 0xFF) as u8,
        }
    }

    /// Check if this is an S9 bitstream.
    pub fn is_s9(&self) -> bool {
        self.model == 0x09 || self.raw == 0x00901002
    }
}

impl fmt::Display for FpgaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Model 0x{:02X} v{}.{}.{} (raw: 0x{:08X})",
            self.model, self.major, self.minor, self.patch, self.raw
        )
    }
}

// ---------------------------------------------------------------------------
// Chain state tracking
// ---------------------------------------------------------------------------

/// State of a single hash chain in the FPGA.
#[derive(Debug, Clone)]
pub struct ChainState {
    /// Chain ID (6, 7, or 8 on S9).
    pub chain_id: u8,
    /// Whether this chain is enabled in the FPGA CTRL_REG.
    pub enabled: bool,
    /// Current BAUD_REG divisor value.
    pub baud_divisor: u32,
    /// Actual baud rate in bps.
    pub baud_rate: u32,
    /// CRC error count from the FPGA.
    pub crc_errors: u32,
    /// FPGA version info.
    pub version: FpgaVersion,
    /// Build ID (unix timestamp of bitstream build).
    pub build_id: u32,
    /// Whether a hash board is plugged into this connector.
    pub board_present: bool,
    /// Whether the board power enable is asserted.
    pub board_enabled: bool,
}

// ---------------------------------------------------------------------------
// FPGA subsystem manager
// ---------------------------------------------------------------------------

/// Top-level FPGA subsystem that manages all hardware interfaces.
///
/// Owns all UIO device mappings and provides structured access to the
/// FPGA chain controllers, fan PWM, GPIO, and glitch monitor.
///
/// This is the central hardware manager for the mining daemon. It is
/// created once during daemon initialization and provides chain handles
/// for the work dispatcher to use during mining.
pub struct FpgaSubsystem {
    /// FPGA chain controllers (one per hash board connector).
    chains: Vec<FpgaChain>,
    /// Chain IDs in the same order as `chains`.
    chain_ids: Vec<u8>,
    /// Fan controller (single PWM for all fans on S9).
    fan: FanController,
    /// GPIO controller for plug detect, board enable, LEDs.
    gpio: Option<GpioController>,
    /// Glitch monitor UIO device.
    glitch_monitor: Option<UioDevice>,
}

impl FpgaSubsystem {
    /// Initialize the FPGA subsystem by opening all UIO devices.
    ///
    /// This is the first step in hardware initialization. It opens and
    /// mmaps all 14 UIO devices, verifies the FPGA is responding by
    /// reading version registers, and sets up the fan controller.
    ///
    /// # Errors
    ///
    /// Returns an error if any UIO device cannot be opened. This typically
    /// means the FPGA bitstream is not loaded or the device tree is wrong.
    pub fn init() -> Result<Self, HalError> {
        info!("Initializing FPGA subsystem");

        // Open fan controller
        let fan = FanController::open(FAN_UIO)?;
        info!(
            uio = FAN_UIO,
            pwm = fan.get_speed_pwm(),
            rpm = fan.get_rpm(),
            "Fan controller opened"
        );

        // Open all chain controllers
        let mut chains = Vec::with_capacity(S9_CHAIN_COUNT);
        let mut chain_ids = Vec::with_capacity(S9_CHAIN_COUNT);

        for i in 0..S9_CHAIN_COUNT {
            let chain_id = S9_CHAIN_IDS[i];
            let uio_base = S9_UIO_BASES[i];

            match FpgaChain::open(chain_id, uio_base) {
                Ok(chain) => {
                    let version = FpgaVersion::from_raw(chain.read_version());
                    let build_id = chain.read_build_id();
                    info!(
                        chain_id,
                        version = %version,
                        build_id = format_args!("0x{:08X}", build_id),
                        "Chain controller opened"
                    );
                    chains.push(chain);
                    chain_ids.push(chain_id);
                }
                Err(e) => {
                    warn!(
                        chain_id,
                        uio_base,
                        error = %e,
                        "Failed to open chain controller (board may not be present)"
                    );
                }
            }
        }

        if chains.is_empty() {
            return Err(HalError::Platform(
                "no FPGA chain controllers could be opened".into(),
            ));
        }

        // Open GPIO controller
        let gpio = match GpioController::new() {
            Ok(gpio) => {
                let plugs = gpio.read_plug_detect();
                info!(
                    j6 = plugs[0],
                    j7 = plugs[1],
                    j8 = plugs[2],
                    "GPIO controller opened, plug detect: J6={}, J7={}, J8={}",
                    plugs[0],
                    plugs[1],
                    plugs[2]
                );
                Some(gpio)
            }
            Err(e) => {
                warn!(error = %e, "GPIO controller unavailable (need /dev/mem access)");
                None
            }
        };

        // Open glitch monitor (optional, non-critical)
        let glitch_monitor = match UioDevice::open(GLITCH_MONITOR_UIO) {
            Ok(dev) => {
                debug!("Glitch monitor opened");
                Some(dev)
            }
            Err(_) => {
                debug!("Glitch monitor not available");
                None
            }
        };

        // Reset all FPGA IP cores to known-good state (matches bosminer's Common::init()).
        // Uses reset_ip_core() (read-modify-write) to preserve MIDSTATE_CNT — see
        // B1 regression note at
        // Writing 0 to CTRL_REG (the old set_enabled(false) path) zeros MIDSTATE_CNT and
        // permanently breaks the UART state machine on hot-start (bosminer recently exited
        // with CTRL=0x0C already set). and the
        // S9 A/B test on 2026-03-12. The chip driver's reconfigure() later writes the
        // chip-family-specific CTRL value (BM1387 wants MIDSTATE_CNT=2 → 0x0C).
        for chain in &chains {
            chain.reset_ip_core();
        }
        info!(
            chains = chains.len(),
            "FPGA subsystem initialized: {} chain(s) ready (IP cores reset)",
            chains.len()
        );

        Ok(Self {
            chains,
            chain_ids,
            fan,
            gpio,
            glitch_monitor,
        })
    }

    /// Get the number of chain controllers that were successfully opened.
    pub fn chain_count(&self) -> usize {
        self.chains.len()
    }

    /// Get the chain IDs that are available.
    pub fn chain_ids(&self) -> &[u8] {
        &self.chain_ids
    }

    /// Take ownership of the FPGA chains (moved to the work dispatcher).
    ///
    /// This transfers the chain controllers out of the subsystem. After
    /// calling this, the subsystem no longer has access to the chains.
    /// The work dispatcher becomes the sole owner of FPGA chain I/O.
    pub fn take_chains(
        self,
    ) -> (
        Vec<FpgaChain>,
        Vec<u8>,
        FanController,
        Option<GpioController>,
    ) {
        (self.chains, self.chain_ids, self.fan, self.gpio)
    }

    /// Get a reference to a specific chain by chain ID.
    pub fn chain(&self, chain_id: u8) -> Option<&FpgaChain> {
        self.chain_ids
            .iter()
            .position(|&id| id == chain_id)
            .map(|idx| &self.chains[idx])
    }

    /// Get a mutable reference to a specific chain by chain ID.
    pub fn chain_mut(&mut self, chain_id: u8) -> Option<&mut FpgaChain> {
        self.chain_ids
            .iter()
            .position(|&id| id == chain_id)
            .and_then(move |idx| self.chains.get_mut(idx))
    }

    /// Get a reference to the fan controller.
    pub fn fan(&self) -> &FanController {
        &self.fan
    }

    /// Get a reference to the GPIO controller, if available.
    pub fn gpio(&self) -> Option<&GpioController> {
        self.gpio.as_ref()
    }

    /// Read the state of all chains.
    pub fn read_chain_states(&self) -> Vec<ChainState> {
        let plug_detect = self
            .gpio
            .as_ref()
            .map(|g| g.read_plug_detect())
            .unwrap_or([false; 3]);

        self.chains
            .iter()
            .enumerate()
            .map(|(idx, chain)| {
                let ctrl = chain.common.read_reg(fpga_chain::REG_CTRL);
                let baud_div = chain.common.read_reg(fpga_chain::REG_BAUD);

                ChainState {
                    chain_id: self.chain_ids[idx],
                    enabled: ctrl & fpga_chain::CTRL_ENABLE != 0,
                    baud_divisor: baud_div,
                    baud_rate: baud_from_divisor(baud_div),
                    crc_errors: chain.read_error_count(),
                    version: FpgaVersion::from_raw(chain.read_version()),
                    build_id: chain.read_build_id(),
                    board_present: plug_detect.get(idx).copied().unwrap_or(false),
                    board_enabled: false, // TODO: read from GPIO output register
                }
            })
            .collect()
    }

    /// Initialize all chains for mining.
    ///
    /// This is the FPGA-level initialization sequence, run before ASIC
    /// chip enumeration. Sets up baud rate, resets FIFOs, and enables
    /// the chain controllers.
    ///
    /// # Arguments
    ///
    /// * `bm139x_mode` - Set true for BM1397+ chips (bit 4 of CTRL_REG).
    /// * `midstate_count` - Number of midstates per work (1, 2, or 4).
    pub fn init_chains_for_mining(&mut self, bm139x_mode: bool, midstate_count: u8) {
        let midstate_bits = match midstate_count {
            1 => 0u32,
            2 => 1u32,
            4 => 2u32,
            _ => {
                warn!(midstate_count, "Invalid midstate count, defaulting to 1");
                0u32
            }
        };

        let ctrl_value = fpga_chain::CTRL_ENABLE
            | if bm139x_mode {
                fpga_chain::CTRL_BM139X
            } else {
                0
            }
            | (midstate_bits << fpga_chain::CTRL_MIDSTATE_SHIFT);

        for (idx, chain) in self.chains.iter_mut().enumerate() {
            let chain_id = self.chain_ids[idx];

            // Reconfigure chain WITHOUT disabling it.
            //
            // BUG FIX (2026-03-12): Writing 0 to CTRL_REG (set_enabled(false))
            // permanently breaks the FPGA UART state machine. After disable+re-enable,
            // ASICs never respond to commands. This was proven by A/B testing on live
            // S9 hardware. Use reconfigure() to safely reset FIFOs, set baud, and
            // write CTRL_REG while keeping the chain enabled.
            chain.reconfigure(ctrl_value, fpga_chain::BAUD_REG_115200);
            debug!(
                chain_id,
                baud = 115200,
                divisor = fpga_chain::BAUD_REG_115200,
                "Baud rate set for enumeration (chain kept enabled)"
            );

            info!(
                chain_id,
                ctrl = format_args!("0x{:08X}", ctrl_value),
                bm139x = bm139x_mode,
                midstates = midstate_count,
                "Chain initialized for mining (no CTRL_REG disable)"
            );
        }
    }

    /// Set the baud rate on all chains simultaneously.
    pub fn set_all_baud(&mut self, baud: u32) {
        let divisor = divisor_from_baud(baud);
        let actual = baud_from_divisor(divisor);

        for chain in &mut self.chains {
            chain.set_baud(divisor);
        }

        info!(
            requested = baud,
            actual, divisor, "Baud rate set on all chains"
        );
    }

    /// Set fan speed (both channels).
    pub fn set_fan_speed(&self, pwm: u8) {
        self.fan.set_speed(pwm);
        debug!(pwm, rpm = self.fan.get_rpm(), "Fan speed set");
    }

    /// Get fan RPM from tachometer.
    pub fn get_fan_rpm(&self) -> u32 {
        self.fan.get_rpm()
    }

    /// Detect which hash boards are plugged in.
    pub fn detect_boards(&self) -> [bool; 3] {
        self.gpio
            .as_ref()
            .map(|g| g.read_plug_detect())
            .unwrap_or([false; 3])
    }

    /// Enable or disable hash board power for a specific connector.
    ///
    /// `board_index`: 0=J6, 1=J7, 2=J8
    pub fn set_board_enable(&self, board_index: u8, enable: bool) {
        if let Some(ref gpio) = self.gpio {
            gpio.set_board_enable(board_index, enable);
            info!(
                board_index,
                enable,
                chain_id = S9_CHAIN_IDS.get(board_index as usize).copied().unwrap_or(0),
                "Board power {}",
                if enable { "enabled" } else { "disabled" }
            );
        }
    }

    /// Enable all hash board power outputs.
    pub fn enable_all_boards(&self) {
        if let Some(ref gpio) = self.gpio {
            gpio.set_all_boards_enable(true);
            info!("All hash boards enabled");
        }
    }

    /// Disable all hash board power outputs (safe shutdown).
    pub fn disable_all_boards(&self) {
        if let Some(ref gpio) = self.gpio {
            gpio.set_all_boards_enable(false);
            info!("All hash boards disabled");
        }
    }

    /// Enable GPIO outputs (must be called before LED or board enable works).
    pub fn enable_gpio_outputs(&self) {
        if let Some(ref gpio) = self.gpio {
            gpio.enable_outputs();
            debug!("GPIO outputs enabled");
        }
    }
}

// ---------------------------------------------------------------------------
// Baud rate utilities
// ---------------------------------------------------------------------------

/// Calculate baud rate from a divisor value.
///
/// Formula: `baud = FPGA_CLK_HZ / (16 * (divisor + 1))`
///
/// Common values:
///   - 0x6C (108) -> 114,679 baud (~115200, enumeration speed)
///   - 0x07 (7)   -> 1,562,500 baud (operational speed)
///   - 0x03 (3)   -> 3,125,000 baud (maximum tested)
pub fn baud_from_divisor(divisor: u32) -> u32 {
    FPGA_CLK_HZ / (16 * (divisor + 1))
}

/// Calculate divisor from a target baud rate.
///
/// Formula: `divisor = FPGA_CLK_HZ / (16 * baud) - 1`
pub fn divisor_from_baud(baud: u32) -> u32 {
    if baud == 0 {
        return 0xFFFF_FFFF; // prevent division by zero
    }
    (FPGA_CLK_HZ / (16 * baud)).saturating_sub(1)
}

// ---------------------------------------------------------------------------
// UIO IRQ support for async nonce collection
// ---------------------------------------------------------------------------

/// IRQ-capable UIO wrapper for interrupt-driven nonce collection.
///
/// The FPGA fires interrupts when:
/// - Work TX FIFO drops below threshold (ready for more work)
/// - Work RX FIFO has nonce data (nonces found by ASICs)
/// - CMD RX FIFO has command response data
///
/// Using IRQs instead of polling dramatically reduces CPU usage during
/// mining. The UIO kernel driver handles IRQ masking/unmasking via the
/// UIO file descriptor.
pub struct UioIrq {
    /// The UIO device (already mmap'd for register access).
    fd: RawFd,
}

impl UioIrq {
    /// Create an IRQ handle from a UIO device's raw file descriptor.
    ///
    /// The caller retains ownership of the UIO device. This struct
    /// only borrows the file descriptor for IRQ operations.
    pub fn from_raw_fd(fd: RawFd) -> Self {
        Self { fd }
    }

    /// Enable (re-arm) the interrupt on this UIO device.
    ///
    /// Must be called once before the first wait, and again after
    /// each interrupt fires.
    pub fn enable(&self) -> std::io::Result<()> {
        let val: u32 = 1;
        let ret = unsafe {
            libc::write(
                self.fd,
                &val as *const u32 as *const libc::c_void,
                std::mem::size_of::<u32>(),
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Block until an interrupt fires on this UIO device.
    ///
    /// Returns the cumulative interrupt count since device open.
    /// This is a blocking call -- in an async context, run on a
    /// dedicated thread or use `tokio::task::spawn_blocking`.
    pub fn wait(&self) -> std::io::Result<u32> {
        let mut count: u32 = 0;
        let ret = unsafe {
            libc::read(
                self.fd,
                &mut count as *mut u32 as *mut libc::c_void,
                std::mem::size_of::<u32>(),
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if ret != 4 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("UIO IRQ read returned {} bytes, expected 4", ret),
            ));
        }
        Ok(count)
    }

    /// Get the raw file descriptor for use with poll/epoll/select.
    ///
    /// The fd becomes readable when an interrupt fires. This allows
    /// integration with async I/O frameworks like tokio via
    /// `AsyncFd::new()`.
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }
}

// ---------------------------------------------------------------------------
// UIO device discovery helpers
// ---------------------------------------------------------------------------

/// Information about a discovered UIO device.
#[derive(Debug, Clone)]
pub struct UioInfo {
    /// UIO device number (N in /dev/uioN).
    pub number: u8,
    /// Device name from /sys/class/uio/uioN/name.
    pub name: String,
    /// Physical address from /sys/class/uio/uioN/maps/map0/addr.
    pub phys_addr: Option<u64>,
    /// Mapping size from /sys/class/uio/uioN/maps/map0/size.
    pub map_size: Option<usize>,
}

/// Scan /sys/class/uio/ for all available UIO devices.
///
/// Returns a sorted list of UIO devices with their names and addresses.
/// This is useful for diagnostics and auto-discovery of the FPGA layout.
pub fn scan_uio_devices() -> Vec<UioInfo> {
    let uio_dir = "/sys/class/uio";
    let mut devices = Vec::new();

    let entries = match fs::read_dir(uio_dir) {
        Ok(e) => e,
        Err(_) => return devices,
    };

    for entry in entries.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if let Some(num_str) = dir_name.strip_prefix("uio") {
            if let Ok(number) = num_str.parse::<u8>() {
                let name = fs::read_to_string(format!("{}/{}/name", uio_dir, dir_name))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| format!("uio{}", number));

                let phys_addr =
                    fs::read_to_string(format!("{}/{}/maps/map0/addr", uio_dir, dir_name))
                        .ok()
                        .and_then(|s| {
                            let s = s.trim().trim_start_matches("0x");
                            u64::from_str_radix(s, 16).ok()
                        });

                let map_size =
                    fs::read_to_string(format!("{}/{}/maps/map0/size", uio_dir, dir_name))
                        .ok()
                        .and_then(|s| {
                            let s = s.trim().trim_start_matches("0x");
                            usize::from_str_radix(s, 16).ok()
                        });

                devices.push(UioInfo {
                    number,
                    name,
                    phys_addr,
                    map_size,
                });
            }
        }
    }

    devices.sort_by_key(|d| d.number);
    devices
}

/// Print a diagnostic dump of all UIO devices (for debug/hacker shell).
pub fn dump_uio_devices() {
    let devices = scan_uio_devices();
    info!("=== UIO Device Map ({} devices) ===", devices.len());
    for dev in &devices {
        let addr_str = dev
            .phys_addr
            .map(|a| format!("0x{:08X}", a))
            .unwrap_or_else(|| "???".into());
        let size_str = dev
            .map_size
            .map(|s| format!("0x{:X}", s))
            .unwrap_or_else(|| "???".into());
        info!(
            "  uio{:<3} {:24} addr={} size={}",
            dev.number, dev.name, addr_str, size_str
        );
    }
}

// ---------------------------------------------------------------------------
// FPGA register dump (diagnostics)
// ---------------------------------------------------------------------------

/// Read and log all common registers for a chain (diagnostic dump).
pub fn dump_chain_registers(chain: &FpgaChain) {
    let version = chain.common.read_reg(fpga_chain::REG_VERSION);
    let build_id = chain.common.read_reg(fpga_chain::REG_BUILD_ID);
    let ctrl = chain.common.read_reg(fpga_chain::REG_CTRL);
    let stat = chain.common.read_reg(fpga_chain::REG_STAT);
    let baud = chain.common.read_reg(fpga_chain::REG_BAUD);
    let work_time = chain.common.read_reg(fpga_chain::REG_WORK_TIME);
    let err_count = chain.common.read_reg(fpga_chain::REG_ERR_COUNTER);

    let cmd_stat = chain.cmd.read_reg(fpga_chain::REG_CMD_STAT);
    let work_rx_stat = chain.work_rx.read_reg(fpga_chain::REG_WORK_RX_STAT);
    let work_tx_stat = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_STAT);
    let work_tx_last = chain.work_tx.read_reg(fpga_chain::REG_WORK_TX_LAST);

    info!(
        chain_id = chain.chain_id,
        "Chain {} register dump:", chain.chain_id
    );
    info!(
        "  VERSION:     0x{:08X} ({})",
        version,
        FpgaVersion::from_raw(version)
    );
    info!("  BUILD_ID:    0x{:08X}", build_id);
    info!(
        "  CTRL_REG:    0x{:08X} [ENABLE={}, BM139X={}, MIDSTATE={}, ERR_CLR={}]",
        ctrl,
        (ctrl >> 3) & 1,
        (ctrl >> 4) & 1,
        (ctrl >> 1) & 3,
        ctrl & 1
    );
    info!("  STAT_REG:    0x{:08X}", stat);
    info!(
        "  BAUD_REG:    0x{:08X} ({} baud)",
        baud,
        baud_from_divisor(baud)
    );
    info!("  WORK_TIME:   0x{:08X}", work_time);
    info!("  ERR_COUNTER: {}", err_count);
    info!(
        "  CMD_STAT:    0x{:08X} [IRQ={} TX_FULL={} TX_EMPTY={} RX_FULL={} RX_EMPTY={}]",
        cmd_stat,
        (cmd_stat >> 4) & 1,
        (cmd_stat >> 3) & 1,
        (cmd_stat >> 2) & 1,
        (cmd_stat >> 1) & 1,
        cmd_stat & 1
    );
    info!(
        "  WORK_RX_STAT: 0x{:08X} [IRQ={} RX_FULL={} RX_EMPTY={}]",
        work_rx_stat,
        (work_rx_stat >> 4) & 1,
        (work_rx_stat >> 1) & 1,
        work_rx_stat & 1
    );
    info!(
        "  WORK_TX_STAT: 0x{:08X} [IRQ={} TX_FULL={} TX_EMPTY={}]",
        work_tx_stat,
        (work_tx_stat >> 4) & 1,
        (work_tx_stat >> 3) & 1,
        (work_tx_stat >> 2) & 1
    );
    info!("  WORK_TX_LAST: 0x{:08X}", work_tx_last);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_baud_from_divisor() {
        // 0x6C = 108 -> 200_000_000 / (16 * 109) = 114,679
        assert_eq!(baud_from_divisor(0x6C), 114_678); // integer division
                                                      // 0x07 = 7 -> 200_000_000 / (16 * 8) = 1_562_500
        assert_eq!(baud_from_divisor(0x07), 1_562_500);
        // 0x03 = 3 -> 200_000_000 / (16 * 4) = 3_125_000
        assert_eq!(baud_from_divisor(0x03), 3_125_000);
    }

    #[test]
    fn test_divisor_from_baud() {
        assert_eq!(divisor_from_baud(115200), 107); // rounds to nearest
        assert_eq!(divisor_from_baud(1_562_500), 7);
        assert_eq!(divisor_from_baud(3_125_000), 3);
    }

    #[test]
    fn test_fpga_version_decode() {
        let v = FpgaVersion::from_raw(0x00901002);
        assert!(v.is_s9());
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 2);
    }

    #[test]
    fn test_divisor_from_baud_zero() {
        // Should not panic on zero baud
        let d = divisor_from_baud(0);
        assert_eq!(d, 0xFFFF_FFFF);
    }
}
