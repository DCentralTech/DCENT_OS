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

use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::config::{
    amlogic_tty_candidate_order, probe_tty_chain_device, ChainTransport, PlatformConfig,
    VoltageControllerKind,
};
use super::subtype::{classify_with_probe, read_subtype};
use super::{BoardType, ChainAccess, FanAccess, GpioAccess, Platform};
use crate::i2c::{spawn_i2c_service_no_register_touch_with_denylist, I2cBus, I2cServiceHandle};
use crate::serial::SerialChain;
use crate::{HalError, Result};

/// I²C bus that carries hashboard EEPROMs on am3-aml (S21, S19j Pro Amlogic,
/// S19K Pro). PSU lives on bus 1 at 0x1f and is intentionally NOT on the
/// denylist — writing to bus 1 is required for `enable_psu_pmbus()`.
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
const GPIO_FAN_TACH_COUNT: u32 = 4;

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

/// Floor RPM the synthesized fallback returns when fans are spinning
/// (PWM > 0) but the GPIO tach window saw zero pulses. Per
/// : NEVER return 0 RPM when fans
/// are physically spinning — that triggers FanFailure in 15 s and a
/// voltage cut. The fallback uses `900 + (pwm * 40)` from the same
/// memory rule. The thermal controller's 3-tick debounce + temperature
/// guard still catches a real fan death (chip temp climbs → throttle
/// → shutdown).
fn synthesized_rpm_floor(pwm: u8) -> u32 {
    if pwm == 0 {
        0
    } else {
        900 + (pwm as u32 * 40)
    }
}

/// Amlogic A113D platform.
pub struct AmlogicPlatform {
    config: PlatformConfig,
}

impl AmlogicPlatform {
    /// Create a new Amlogic platform.
    ///
    /// Auto-detects the specific model (S19 XP, S21, etc.) by probing
    /// hardware characteristics.
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

        // BOS-3528 pinmux fix: export GPIO 476/477 BEFORE any I2C access.
        // This changes the Amlogic pinmux away from PWM/other functions that
        // corrupt the I2C bus. Verified from BraiinsOS S37board_setup on S21.
        init_pinmux();

        // Detect specific model
        let mut config = detect_amlogic_model()?;

        // W2A.2 PIC1704 wire-up (2026-05-09):
        //
        // Amlogic carriers ship two distinct hashboard families:
        //   - `AMLCtrl_BHB42XXX` (S19j Pro Amlogic — the new PIC1704 path)
        //   - `AMLCtrl_BHB56xxx` (S19k Pro / S21 — existing dsPIC33EP path)
        //
        // The default voltage controller chosen by `detect_amlogic_model`
        // (NoPic for S21 NoPic, Dspic33Ep for S19k Pro) is preserved
        // unless `/etc/subtype` says BHB42XXX AND a 0x20 ACK probe
        // succeeds on `AMLOGIC_HASHBOARD_EEPROM_BUS`. This ensures
        // s19jpro (sustained-mining unit running existing dsPIC path)
        // and s21 / .78 are never silently re-routed.
        let subtype = read_subtype();
        let kind = classify_with_probe(subtype.as_deref(), AMLOGIC_HASHBOARD_EEPROM_BUS);
        config.voltage_controller = kind;
        tracing::info!(
            platform = %config.name,
            chains = config.chains.len(),
            has_pic = config.has_pic,
            subtype = %subtype.as_deref().unwrap_or("<missing>"),
            voltage_controller = kind.as_str(),
            "Amlogic platform initialized"
        );

        Ok(Self { config })
    }

    /// Create with explicit config (for testing or manual override).
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
        Ok(Box::new(AmlogicFan::new()?))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        Ok(Box::new(AmlogicGpio))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        // Cached from `new()` / `with_config()`. Defaults preserve the
        // existing s19jpro dsPIC path; only `AMLCtrl_BHB42XXX` +
        // 0x20 ACK upgrades to PIC1704. See `new()` doc comment.
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
    /// Sample falling edges over the given window and return the count.
    /// Must be a non-blocking-style budget: implementations should bound
    /// the call to `window` regardless of whether edges arrive.
    fn sample_falling_edges(&self, window: Duration) -> u32;
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
        let _ = drain_value_fd(fd.as_raw_fd());

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
        let _ = libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
    }
    Ok(())
}

impl FanTachSource for SysfsFallingEdgeCounter {
    fn sample_falling_edges(&self, window: Duration) -> u32 {
        let raw_fd = self.fd.as_raw_fd();
        let deadline = Instant::now() + window;
        let mut edges: u32 = 0;

        // Hard cap: even if the kernel storms us with bogus events
        // (e.g. floating tach line), don't loop forever.
        const MAX_EDGES_PER_WINDOW: u32 = 100_000;

        // Make sure we start from a known-clean state.
        let _ = drain_value_fd(raw_fd);

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
                tracing::warn!(gpio = self.gpio, error = %err, "fan tach poll() failed");
                break;
            }
            if rc == 0 {
                // Timeout — window expired, no further edges.
                break;
            }

            if pollfd.revents & (libc::POLLPRI | libc::POLLERR) != 0 {
                edges = edges.saturating_add(1);
                if edges >= MAX_EDGES_PER_WINDOW {
                    tracing::warn!(
                        gpio = self.gpio,
                        edges,
                        "fan tach exceeded MAX_EDGES_PER_WINDOW; aborting sample"
                    );
                    break;
                }
                // Re-arm the latch so the next falling edge fires POLLPRI again.
                let _ = drain_value_fd(raw_fd);
            }
        }

        edges
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
/// Tach (W3.2, 2026-05-07): falling-edge counter on gpio447-450
/// (`SysfsFallingEdgeCounter`). When at least one tach line exports
/// successfully, `tach_available()` flips to true and the thermal
/// controller can act on real fan health (5-deadly-conditions
/// FanFailure path)., when
/// PWM > 0 but the GPIO sample sees zero edges (e.g. floating line on
/// a board with the fan unplugged) we still return the synthesized
/// floor so we don't hand the thermal controller a 0 RPM that would
/// trigger FanFailure within 15 s on a unit that actually has fans
/// spinning but a wiring fault on the tach pin.
struct AmlogicFan {
    /// Rear fans duty_cycle path
    rear_duty_path: String,
    /// Front fans duty_cycle path
    front_duty_path: String,
    /// PWM period in nanoseconds (10kHz = 100000ns)
    period_ns: u32,
    /// One falling-edge counter per fan slot (0..GPIO_FAN_TACH_COUNT).
    /// `None` means the slot's GPIO failed to export — that fan falls
    /// back to the synthesized RPM floor on `get_rpm()`.
    tach_sources: Vec<Option<Box<dyn FanTachSource>>>,
    /// Sample window passed to each `FanTachSource::sample_falling_edges`.
    /// Owned by the struct so tests can shorten it.
    sample_window: Duration,
}

/// PWM period for Amlogic fans: 100000ns = 10kHz (confirmed on S21 probe)
const AMLOGIC_PWM_PERIOD_NS: u32 = 100_000;

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

        // Try to bring up gpio447-450 as falling-edge tach inputs.
        // Each slot can fail independently — a single failed export is
        // logged and the slot falls back to synthesized RPM, but the
        // rest still report real edges.
        let mut tach_sources: Vec<Option<Box<dyn FanTachSource>>> =
            Vec::with_capacity(GPIO_FAN_TACH_COUNT as usize);
        let mut any_tach_alive = false;
        for slot in 0..GPIO_FAN_TACH_COUNT {
            let gpio = GPIO_FAN_TACH_BASE + slot;
            match SysfsFallingEdgeCounter::export(gpio) {
                Ok(counter) => {
                    tach_sources.push(Some(Box::new(counter)));
                    any_tach_alive = true;
                }
                Err(e) => {
                    tracing::warn!(
                        slot,
                        gpio,
                        error = %e,
                        "Amlogic fan tach: GPIO export failed; slot will report synthesized RPM"
                    );
                    tach_sources.push(None);
                }
            }
        }

        if any_tach_alive {
            tracing::info!(
                tach_count = tach_sources.iter().filter(|s| s.is_some()).count(),
                "Amlogic fan tach: GPIO falling-edge counters armed on gpio447-450"
            );
        } else {
            tracing::warn!(
                "Amlogic fan tach: NO GPIO tach lines exported — falling back to synthesized RPM"
            );
        }

        Ok(Self {
            rear_duty_path: rear_path,
            front_duty_path: front_path,
            period_ns: AMLOGIC_PWM_PERIOD_NS,
            tach_sources,
            sample_window: Duration::from_millis(TACH_SAMPLE_MS),
        })
    }

    /// Test-only constructor that injects mock tach sources. Lets unit
    /// tests assert RPM math without poking real GPIO sysfs.
    #[cfg(test)]
    fn for_test(tach_sources: Vec<Option<Box<dyn FanTachSource>>>) -> Self {
        Self {
            rear_duty_path: String::new(),
            front_duty_path: String::new(),
            period_ns: AMLOGIC_PWM_PERIOD_NS,
            tach_sources,
            sample_window: Duration::from_millis(10),
        }
    }

    /// Sample one fan slot. Returns the measured RPM, or the
    /// synthesized floor if the slot has no tach source or the sample
    /// returned zero edges while PWM > 0.
    fn sample_slot_rpm(&self, slot: usize, pwm: u8) -> u32 {
        let measured = match self.tach_sources.get(slot).and_then(|s| s.as_ref()) {
            Some(src) => edges_to_rpm(
                src.sample_falling_edges(self.sample_window),
                self.sample_window,
            ),
            None => 0,
        };

        if measured > 0 {
            return measured;
        }

        //: if PWM > 0, we
        // must NEVER hand 0 RPM to the thermal controller — that
        // path is the FanFailure trigger and would kill the unit
        // within 15 s on a wiring fault that doesn't actually mean
        // the fan stopped. Real fan death still surfaces because
        // chip temp climbs and the temperature path takes over.
        synthesized_rpm_floor(pwm)
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

    fn get_rpm(&self) -> u32 {
        // W3.2 (2026-05-07): real GPIO falling-edge tach on gpio447-450.
        // The legacy `900 + pwm*51` synthesized formula is gone — the
        // thermal controller now sees real fan RPM and can fire the
        // 5-deadly-conditions FanFailure path. The synthesized floor
        // still kicks in for unexported slots and zero-edge windows
        // while PWM > 0.
        //
        // Convention used by the upstream thermal controller: return
        // the slowest non-zero RPM across all slots so a single
        // failing fan can latch the FanFailure debounce. If a slot's
        // tach is unavailable we fall back to synthesized RPM for
        // that slot (and that slot won't dominate the min unless all
        // others are also synthesized).
        let pwm = self.get_speed_pwm();
        if self.tach_sources.is_empty() {
            return synthesized_rpm_floor(pwm);
        }
        // Use min-non-zero across slots. Fall back to synthesized
        // floor so we never return 0 when PWM > 0.
        let mut min_rpm: Option<u32> = None;
        for slot in 0..self.tach_sources.len() {
            let rpm = self.sample_slot_rpm(slot, pwm);
            min_rpm = Some(min_rpm.map_or(rpm, |m| m.min(rpm)));
        }
        match min_rpm {
            Some(0) => synthesized_rpm_floor(pwm),
            Some(r) => r,
            None => synthesized_rpm_floor(pwm),
        }
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
        let pwm = self.get_speed_pwm();
        if self.tach_sources.is_empty() {
            return (0..GPIO_FAN_TACH_COUNT as u8)
                .map(|id| (id, synthesized_rpm_floor(pwm)))
                .collect();
        }
        (0..self.tach_sources.len())
            .map(|slot| (slot as u8, self.sample_slot_rpm(slot, pwm)))
            .collect()
    }

    fn fan_count(&self) -> u8 {
        // S21 / S19j Pro Amlogic / S19K Pro: 4 fans (2 front + 2 rear).
        GPIO_FAN_TACH_COUNT as u8
    }

    fn tach_available(&self) -> bool {
        // True when at least one tach line armed successfully. The
        // thermal controller uses this flag to decide whether to act
        // on RPM=0 (5-deadly-conditions FanFailure) or rely on
        // temperature thresholds alone. W3.2 flips this from the
        // hard-coded `false` to a live check.
        self.tach_sources.iter().any(|slot| slot.is_some())
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
/// GPIO is necessary but not sufficient for native cold boot. BraiinsOS/Amlogic
/// U-Boot also runs an APW enable sequence on I2C bus 1, address 0x1f:
///   `i2c mw 1f 3.1 0 2; i2c mw 1f 1.1 fc 2`
///
/// Keep this GPIO gate first because it is the hard board-level enable.
const GPIO_PSU_ENABLE: u32 = 437;

/// APW PSU I2C bus/address from the extracted S21 U-Boot preboot environment.
const APW_PMBUS_I2C_BUS: u8 = 1;
const APW_PMBUS_ADDR: u8 = 0x1f;

/// Exact payloads represented by U-Boot:
///   `i2c mw 1f 3.1 0 2`  -> register 0x03, two zero bytes
///   `i2c mw 1f 1.1 fc 2` -> register 0x01, two 0xfc bytes
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
pub fn enable_psu() -> Result<()> {
    enable_psu_gpio()?;
    if let Err(e) = enable_psu_pmbus() {
        let _ = disable_psu();
        return Err(e);
    }

    // Wait for PSU to stabilize (APW PSU soft-start is ~1 second).
    std::thread::sleep(Duration::from_secs(2));

    Ok(())
}

fn enable_psu_gpio() -> Result<()> {
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);

    // Ensure GPIO is exported
    let export_path = "/sys/class/gpio/export";
    if !std::path::Path::new(&gpio_path).exists() {
        let _ = fs::write(export_path, format!("{}", GPIO_PSU_ENABLE));
        std::thread::sleep(Duration::from_millis(100));
    }

    // Set as output, drive HIGH to enable PSU (active HIGH,  Q10).
    // Do NOT write the `active_low` sysfs file — stock/VNish leave it at the
    // kernel default (0), so `value=1` means the pin is electrically HIGH.
    let dir_path = format!("/sys/class/gpio/gpio{}/direction", GPIO_PSU_ENABLE);
    fs::write(&dir_path, "out")
        .map_err(|e| HalError::Platform(format!("PSU GPIO direction: {}", e)))?;
    fs::write(&gpio_path, "1")
        .map_err(|e| HalError::Platform(format!("PSU GPIO enable: {}", e)))?;

    std::thread::sleep(Duration::from_millis(50));
    if !is_psu_enabled() {
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

/// Replay the APW I2C/PMBus enable sequence from the Amlogic U-Boot `preboot`.
///
/// Evidence source:
/// `preboot=...;i2c mw 1f 3.1 0 2;i2c mw 1f 1.1 fc 2;...`
///
/// This is intentionally narrower than a generic PMBus driver:
/// - writes only the two live-observed APW registers at 0x1f,
/// - does not touch TAS5782M DAC addresses,
/// - treats generic STATUS_WORD readback as optional because APW firmware
///   telemetry support varies by revision.
pub fn enable_psu_pmbus() -> Result<()> {
    init_pinmux();

    let mut bus = I2cBus::open(APW_PMBUS_I2C_BUS)?;
    bus.set_slave(APW_PMBUS_ADDR)?;
    bus.write(&APW_PMBUS_CLEAR_FAULTS)?;
    std::thread::sleep(Duration::from_millis(10));
    bus.write(&APW_PMBUS_OPERATION_ENABLE)?;
    std::thread::sleep(Duration::from_millis(200));

    match read_pmbus_status_word(&mut bus) {
        Ok(status) => tracing::info!(
            bus = APW_PMBUS_I2C_BUS,
            addr = format_args!("0x{:02X}", APW_PMBUS_ADDR),
            status = format_args!("0x{:04X}", status),
            "Amlogic APW PMBus enable sequence completed"
        ),
        Err(e) => tracing::warn!(
            bus = APW_PMBUS_I2C_BUS,
            addr = format_args!("0x{:02X}", APW_PMBUS_ADDR),
            error = %e,
            "Amlogic APW PMBus enable writes completed; STATUS_WORD read unavailable"
        ),
    }

    Ok(())
}

fn read_pmbus_status_word(bus: &mut I2cBus) -> Result<u16> {
    let mut buf = [0u8; 2];
    bus.write_read(&[PMBUS_STATUS_WORD], &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

/// Disable the APW PSU output (for shutdown/safety).
pub fn disable_psu() -> Result<()> {
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);
    // Drive LOW to disable PSU (active HIGH,  Q10 — corrected 2026-05-21).
    fs::write(&gpio_path, "0")
        .map_err(|e| HalError::Platform(format!("PSU GPIO disable: {}", e)))?;

    std::thread::sleep(Duration::from_millis(50));
    if is_psu_enabled() {
        return Err(HalError::Platform(format!(
            "PSU GPIO {} readback stayed HIGH after disable",
            GPIO_PSU_ENABLE
        )));
    }

    tracing::info!(
        "PSU GPIO {} driven LOW and read back LOW (PSU disabled)",
        GPIO_PSU_ENABLE
    );
    Ok(())
}

/// Check if the PSU is currently enabled.
pub fn is_psu_enabled() -> bool {
    let gpio_path = format!("/sys/class/gpio/gpio{}/value", GPIO_PSU_ENABLE);
    fs::read_to_string(&gpio_path)
        .map(|v| v.trim() == "1") // Active HIGH: 1 = enabled (Wave 5 Q10, corrected 2026-05-21)
        .unwrap_or(false)
}

/// Initialize GPIO pinmux to prevent I2C bus corruption (BOS-3528).
///
/// On Amlogic A113D, GPIO 476 (I2C_SCL) and 477 (I2C_SDA) must be exported
/// before any I2C bus access. Exporting them switches the pinmux from the
/// default function (which conflicts with PWM) to GPIO, allowing the I2C
/// controller to work correctly.
///
/// Verified from BraiinsOS S37board_setup on live S21 at .135 (2026-04-12).
fn init_pinmux() {
    let export_path = "/sys/class/gpio/export";
    for gpio in &GPIO_PINMUX_FIX {
        let gpio_dir = format!("/sys/class/gpio/gpio{}", gpio);
        if !std::path::Path::new(&gpio_dir).exists() {
            if let Err(e) = fs::write(export_path, format!("{}", gpio)) {
                tracing::warn!("Pinmux: failed to export GPIO {}: {}", gpio, e);
            } else {
                // Set as input (matches BraiinsOS behavior)
                let dir_path = format!("{}/direction", gpio_dir);
                let _ = fs::write(&dir_path, "in");
                tracing::debug!("Pinmux: exported GPIO {} as input", gpio);
            }
        }
    }
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
/// `chain_id_count` controls how many chains to probe (1 for S21
/// single-chain layouts, 3 for multi-chain S19K Pro / S19 XP / etc.).
fn temp_sensors_for(chain_id_count: u8) -> Vec<(u8, &'static str)> {
    let mut sensors = Vec::with_capacity(2 * chain_id_count as usize);
    for cid in 0..chain_id_count {
        sensors.push((0x48 + cid, "inlet"));
    }
    for cid in 0..chain_id_count {
        sensors.push((0x4C + cid, "outlet"));
    }
    sensors
}

/// Read board temperatures from LM75-compatible sensors on Amlogic platforms.
///
/// Returns a vector of (sensor_address, temperature_celsius).
/// On read failure, that sensor is silently skipped (may not be present
/// if hash board is not populated or powered).
///
/// Uses raw I2C fd operations (not the I2cBus service which is designed
/// for PIC communication). LM75 register 0x00: 16-bit big-endian temperature.
/// Upper 9 bits = temperature in 0.5°C steps (signed).
pub fn read_board_temps(i2c_bus: u8) -> Vec<(u8, f32)> {
    // Default to 3-chain probe (covers S19K Pro / S19j Pro Amlogic /
    // S19 XP). For S21 single-chain layouts, the extra 2 reads NACK
    // gracefully and add ~10ms of probe time.
    read_board_temps_for_chain_count(i2c_bus, 3)
}

/// Read board temps with explicit chain count (preferred).
pub fn read_board_temps_for_chain_count(i2c_bus: u8, chain_count: u8) -> Vec<(u8, f32)> {
    use std::os::fd::AsRawFd;

    let mut temps = Vec::new();
    let path = format!("/dev/i2c-{}", i2c_bus);
    let fd = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return temps,
    };

    let sensors = temp_sensors_for(chain_count);
    for &(addr, name) in &sensors {
        // Set slave address via ioctl
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), 0x0706, addr as libc::c_ulong) };
        if ret < 0 {
            continue;
        }

        // Write register address 0x00
        let reg: [u8; 1] = [0x00];
        let written =
            unsafe { libc::write(fd.as_raw_fd(), reg.as_ptr() as *const libc::c_void, 1) };
        if written != 1 {
            continue;
        }

        // Read 2 bytes of temperature data
        let mut buf = [0u8; 2];
        let read = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, 2) };
        if read != 2 {
            continue;
        }

        // LM75 format: [MSB, LSB], upper 9 bits = temp in 0.5°C steps
        let raw = ((buf[0] as i16) << 8) | (buf[1] as i16);
        let temp = (raw >> 7) as f32 * 0.5;
        if temp > -40.0 && temp < 125.0 {
            tracing::trace!(addr, name, temp, "Board temp");
            temps.push((addr, temp));
        }
    }

    temps
}

fn normalize_model_token(model: &str) -> String {
    model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect()
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
        match plat_norm.as_str() {
            "am3-aml-s19k" | "am3-aml-s19xp" => return Ok(PlatformConfig::s19k_amlogic()),
            "am3-aml-s21" | "am3-aml" => return Ok(PlatformConfig::s21_amlogic()),
            _ => tracing::debug!(
                platform = %plat_norm,
                "/etc/dcentos-platform did not match a known token; falling back"
            ),
        }
    }

    // 2. BraiinsOS+ secondary signal: /etc/bosminer.toml `model` field.
    //    This wins over DTB when bosminer is the running rootfs because
    //    it carries the exact factory model name (e.g. "Antminer S19K Pro NoPic").
    if let Ok(toml) = fs::read_to_string("/etc/bosminer.toml") {
        let lower = toml.to_ascii_lowercase();
        if lower.contains("model") && lower.contains("s19k pro") {
            tracing::info!("Detected S19K Pro from /etc/bosminer.toml model field");
            return Ok(PlatformConfig::s19k_amlogic());
        }
        if lower.contains("model") && (lower.contains("s19 xp") || lower.contains("s19xp")) {
            tracing::info!("Detected S19 XP from /etc/bosminer.toml model field");
            return Ok(PlatformConfig::s19k_amlogic());
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

    #[test]
    fn apw_pmbus_enable_sequence_matches_extracted_uboot_preboot() {
        assert_eq!(APW_PMBUS_I2C_BUS, 1);
        assert_eq!(APW_PMBUS_ADDR, 0x1f);
        assert_eq!(APW_PMBUS_CLEAR_FAULTS, [0x03, 0x00, 0x00]);
        assert_eq!(APW_PMBUS_OPERATION_ENABLE, [0x01, 0xfc, 0xfc]);
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
    fn temp_sensors_for_three_chains_matches_live_78_layout() {
        // Live-verified per .78 bosminer.log:
        //   hb1.72 (Inlet=0x48), hb1.76 (Outlet=0x4C)
        //   hb2.73 (Inlet=0x49), hb2.77 (Outlet=0x4D)
        //   hb3.74 (Inlet=0x4A), hb3.78 (Outlet=0x4E)
        let s = temp_sensors_for(3);
        assert_eq!(s.len(), 6);
        assert_eq!(s[0], (0x48, "inlet"));
        assert_eq!(s[1], (0x49, "inlet"));
        assert_eq!(s[2], (0x4A, "inlet"));
        assert_eq!(s[3], (0x4C, "outlet"));
        assert_eq!(s[4], (0x4D, "outlet"));
        assert_eq!(s[5], (0x4E, "outlet"));
    }

    #[test]
    fn temp_sensors_for_one_chain_matches_s21_singlechain_layout() {
        // S21 single-chain: just hb1's pair
        let s = temp_sensors_for(1);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], (0x48, "inlet"));
        assert_eq!(s[1], (0x4C, "outlet"));
    }

    #[test]
    fn temp_sensors_for_zero_chains_returns_empty() {
        // Defensive: chain_count=0 should not panic
        let s = temp_sensors_for(0);
        assert!(s.is_empty());
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
        fn sample_falling_edges(&self, _window: Duration) -> u32 {
            self.0
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
        // 0 edges ⇒ 0 RPM (caller decides whether to fall back to floor).
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
    fn synthesized_rpm_floor_zero_when_fans_off() {
        assert_eq!(synthesized_rpm_floor(0), 0);
    }

    #[test]
    fn synthesized_rpm_floor_nonzero_when_fans_on() {
        // formula: 900 + pwm*40.
        assert_eq!(synthesized_rpm_floor(10), 1300);
        assert_eq!(synthesized_rpm_floor(30), 2100);
        assert_eq!(synthesized_rpm_floor(100), 4900);
    }

    #[test]
    fn test_amlogic_synthetic_rpm_replaced() {
        // Acceptance for W3.2: tach_available flips to true with at least
        // one armed slot, and per-fan RPM matches the mocked edge stream
        // through the edges→RPM conversion. With sample_window = 10 ms
        // (test constructor) and 2 PPR, an edge count of N produces
        // N * 60 / (0.01 * 2) = N * 3000 RPM.
        let sources: Vec<Option<Box<dyn FanTachSource>>> = vec![
            Some(Box::new(FixedEdgeMock(20))), // 60_000 RPM (clamped expectation only in math)
            Some(Box::new(FixedEdgeMock(10))), // 30_000 RPM
            Some(Box::new(FixedEdgeMock(5))),  // 15_000 RPM
            None,                              // unexported slot
        ];
        let fan = AmlogicFan::for_test(sources);

        assert!(
            fan.tach_available(),
            "tach_available must flip to true once any GPIO slot is armed (W3.2 acceptance)"
        );

        // Sample window is 10 ms in the test constructor.
        let window = Duration::from_millis(10);
        let per_fan = fan.get_per_fan_rpm();
        assert_eq!(per_fan.len(), 4, "S21/S19j Pro Amlogic has 4 fan slots");
        assert_eq!(per_fan[0], (0, edges_to_rpm(20, window)));
        assert_eq!(per_fan[1], (1, edges_to_rpm(10, window)));
        assert_eq!(per_fan[2], (2, edges_to_rpm(5, window)));
        // Slot 3 has no tach. PWM is 0 in the test (no real sysfs write),
        // so the synthesized floor returns 0 — the min-non-zero rule
        // ensures we don't false-flag a wiring fault.
        assert_eq!(per_fan[3], (3, 0));
    }

    #[test]
    fn fan_with_no_tach_sources_falls_back_to_synthesized() {
        // Construct an AmlogicFan with zero armed slots. tach_available
        // must be false and the synthesized floor must protect against
        // returning 0 RPM when PWM > 0. Since the test constructor
        // can't drive real PWM, we exercise the empty-vec branch
        // directly via the helper.
        let fan = AmlogicFan::for_test(vec![None, None, None, None]);
        assert!(
            !fan.tach_available(),
            "tach_available must stay false when no slot exported"
        );
        // All slots return synthesized floor; PWM=0 in test sysfs ⇒ 0.
        let per_fan = fan.get_per_fan_rpm();
        assert!(per_fan.iter().all(|&(_, rpm)| rpm == 0));
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
        // Disable path drives the value file LOW.
        assert!(
            src.contains("fs::write(&gpio_path, \"0\")"),
            "disable_psu must write \"0\" to disable (active HIGH, Wave 5 Q10)"
        );
        // Readback treats HIGH as enabled.
        assert!(
            src.contains("v.trim() == \"1\""),
            "is_psu_enabled must treat \"1\" (HIGH) as enabled (active HIGH, Wave 5 Q10)"
        );
        // The old active-LOW readback must be gone.
        assert!(
            !src.contains("|v| v.trim() == \"0\") // Active LOW"),
            "active-LOW readback must not be re-introduced (EE C1 regression guard)"
        );
    }
}
