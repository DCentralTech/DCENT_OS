//! Hardware Abstraction Layer (HAL) for dcentrald.
//!
//! Provides safe Rust wrappers around Linux hardware interfaces used to
//! control Antminer hardware. This crate has NO dependencies on other
//! dcentrald crates -- it is pure hardware access.
//!
//! Modules:
//! - `uio`              - UIO device: open, mmap, IRQ wait (foundation of BraiinsOS FPGA access)
//! - `fpga_chain`       - BraiinsOS FPGA chain register blocks (common, cmd, work-rx, work-tx)
//! - `i2c`              - I2C bus: open /dev/i2c-N, ioctl, read/write
//! - `gpio`             - AXI GPIO: direct register access for plug detect + board reset
//! - `fan`              - Fan PWM controller (7-bit PWM, 7-bit tach)
//! - `watchdog`         - /dev/watchdog open, periodic kick
//! - `xadc`             - XADC IIO sysfs: die temp, VCCINT, VCCAUX
//! - `led`              - LED control via GPIO output register
//! - `platform`         - Platform trait and auto-detection (Zynq S9/S19, Amlogic, BeagleBone)
//! - `platform::config` - Runtime platform config (replaces hardcoded S9 constants)
//! - `serial`           - Standard Linux serial port with custom baud rate support
//! - `serial_chain`     - Serial-based chain transport (NS16550A UART backend for ASIC comms)
//! - `psu`              - APW PSU controller via I2C bus 1 (PMBus-like protocol, watchdog)
//! - `psu_apw12_smbus`  - APW12 SMBus opcode driver for CV1835/AM335x BB/Amlogic S19j Pro (NOT for Zynq am2 — that uses `psu::Apw121215a`)
//! - `psu_apw12_plus`   - APW12+ register-based PSU driver for S21 family (Amlogic, NoPic). DIFFERENT protocol from APW12 SMBus.
//! - `psu_apw_uart_tunnel` - APW121215f framed UART-tunnel driver for the AM335x BB S19j Pro on S19J_IO_BOARD_V2_0 (the `a lab unit` unit). DIFFERENT framing from `psu_apw12_smbus`. Frame format LIVE-CONFIRMED (2026-05-12 ftrace on `a lab unit`); calibration read + watchdog-disable opcodes Ghidra-confirmed (2026-05-31); set-voltage payload still TODO.
//! - `stock_fpga`       - Stock Bitmain FPGA register access (/dev/axi_fpga_dev, 352-byte flat space)
//! - `stock_fpga_iic`   - Stock FPGA PIC I2C via IIC_COMMAND register (no /dev/i2c-*)
//! - `stock_fpga_work`  - Stock FPGA DMA work dispatch + nonce FIFO (DHASH accelerator)
//! - `stock_fpga_axi_ioctl` - bitmain_axi.ko IOCTL adapter (DEV/DEBUG ONLY; gated behind `axi-ioctl-debug` Cargo feature per W13.B5; QEMU rehosting helper, NOT compiled into shipping firmware)
//! - `stock_fpga_axi_mmap`  - bitmain_axi.ko mmap adapter (production canonical per RE3 — `BitmainAxiMmapBackend` + `BitmainAxiUnifiedBackend`)
//! - `uio_discover`     - UIO device discovery by kernel-published name (shared by fan + FPGA chain backend)
//! - `chain_backend`    - `Bm1397PlusChainBackend` trait abstracting BM1362-family chain transport (PL UART vs FPGA FIFO)
//! - `fpga_chain_backend` - `FpgaChainBackend` impl of `Bm1397PlusChainBackend` over the FPGA FIFO IP blocks (am2 chain1 at 0x43C0Nxxx)

#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

pub mod adc;
pub mod board_control;
pub mod chain_backend;
pub mod fan;
pub mod fpga_chain;
pub mod fpga_chain_backend;
pub mod fpga_uart_relay;
pub mod glitch_monitor;
pub mod gpio;
pub mod i2c;
pub mod ina226;
pub mod led;
pub mod led_patterns;
pub mod libgpiod;
pub mod platform;
pub mod psu;
pub mod psu_apw12_plus;
pub mod psu_apw12_smbus;
pub mod psu_apw_uart_tunnel;
pub mod psu_bypass_gate;
pub mod psu_gpio_gate;
pub mod psu_gpio_i2c;
pub mod serial;
pub mod serial_chain;
pub mod stock_fpga;
/// W13.B5 (2026-05-10): IOCTL adapter is gated behind the `axi-ioctl-debug`
/// Cargo feature. Production `dcentrald` does NOT enable this feature, so
/// this module does not compile into shipping firmware. RE3 DWARF-confirms
/// production kernels have ZERO IOCTL handlers — the mmap path
/// (`stock_fpga_axi_mmap`) is canonical. This module is preserved only for
/// QEMU `fake_axi_fpga.c` rehosting.
#[cfg(feature = "axi-ioctl-debug")]
pub mod stock_fpga_axi_ioctl;
pub mod stock_fpga_axi_mmap;
pub mod stock_fpga_iic;
pub mod stock_fpga_work;
// W13.B1 (2026-05-10): `uart_relay` module DELETED. The `UartRelayReg`
// typed bitfield moved to `dcentrald_asic::bm1362::uart_relay` (BM1362
// ASIC reg 0x2C candidate evidence; production writes remain lab-gated
// pending R6-7). The `UartRelay` mmap struct merged into
// `glitch_monitor::BraiinsGlitchMonitor` (the FPGA `0x43D000xx` window is
// now correctly classified as a Braiins-am2 diagnostic mirror, NOT a
// control surface).
pub mod uio;
pub mod uio_discover;
pub mod watchdog;
pub mod xadc;

use thiserror::Error;

/// HAL error type.
#[derive(Debug, Error)]
pub enum HalError {
    /// Failed to open a device file.
    #[error("failed to open device {path}: {source}")]
    DeviceOpen {
        path: String,
        source: std::io::Error,
    },

    /// Memory mapping failed.
    #[error("mmap failed for {device}: {source}")]
    MmapFailed { device: String, source: nix::Error },

    /// Register access out of bounds.
    #[error("register offset 0x{offset:04X} out of bounds for {device} (size={size})")]
    RegisterOutOfBounds {
        device: String,
        offset: u32,
        size: usize,
    },

    /// I2C transaction failed.
    #[error("I2C error on bus {bus} addr 0x{addr:02X}: {detail}")]
    I2c { bus: u8, addr: u8, detail: String },

    /// GPIO operation failed.
    #[error("GPIO error: {0}")]
    Gpio(String),

    /// Fan controller error.
    #[error("fan controller error: {0}")]
    Fan(String),

    /// Watchdog error.
    #[error("watchdog error: {0}")]
    Watchdog(String),

    /// XADC read error.
    #[error("XADC error: {0}")]
    Xadc(String),

    /// Platform detection failed.
    #[error("platform error: {0}")]
    Platform(String),

    /// PSU reported over-current / OCP latched (V1/V2 error class, non-retryable).
    #[error("PSU over-current protection latched")]
    PsuOverCurrent,

    /// PSU model is not supported or FW byte unknown (P5/P6/P7).
    #[error("PSU unsupported: {0}")]
    PsuUnsupported(String),

    /// PSU framed-I2C protocol error (bad preamble, LEN, CMD echo, or checksum).
    #[error("PSU protocol error: {0}")]
    PsuProtocol(&'static str),

    /// PSU framed-I2C protocol error with runtime context.
    #[error("PSU protocol error: {0}")]
    PsuProtocolOwned(String),

    /// PSU telemetry is not yet characterized for this FW byte.
    ///
    /// Returned by `Apw121215a::read_voltage()` / `read_power()` /
    /// `read_calibration()` when the detected `PsuModel` has telemetry
    /// capability marked as **unknown** (e.g. `Apw121215f` fw=0x76, live-
    /// confirmed on `a lab unit` but not yet probed for ADC behavior). This is
    /// **distinct** from `Ok(None)` returned for known-no-feedback variants
    /// such as `Apw121215a` fw=0x71. Callers that ignore `Ok(None)` get
    /// silent zero-telemetry; callers that ignore this error get a hard
    /// fail — the explicit fail-closed semantics for "we don't know yet".
    ///
    /// Recovery: an operator probe via `i2cget -y <bus> <addr> 0x8B w`
    /// (READ_VOUT) etc. against a live unit. See `Apw121215a::probe()`
    /// log lines and the `PsuModel::Apw121215f` doc comment.
    #[error("PSU telemetry unavailable: {0}")]
    PsuTelemetryUnavailable(String),

    /// Generic I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Trait method or feature is declared but not yet implemented.
    ///
    ///  (2026-05-23) Phase-1 skeleton uses this for the
    /// `FpgaChainBackend` trait methods. Phase 2 replaces every
    /// `NotImplemented` site with the real FPGA-FIFO body.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// Other error (catch-all for /dev/mem mmap failures, etc.).
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HalError>;
