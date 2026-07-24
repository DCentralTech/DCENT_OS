//! I2C bus driver.
//!
//! Wraps Linux I2C char device (/dev/i2c-N) for PIC microcontroller and
//! temperature sensor communication. Uses ioctl(I2C_SLAVE) to select the
//! target device address before read/write operations.
//!
//! S9 I2C device map (verified from live probe):
//!   0x55 - PIC16F1704 (Chain 6/J6 voltage controller)
//!   0x56 - PIC16F1704 (Chain 7/J7 voltage controller)
//!   0x57 - PIC16F1704 (Chain 8/J8 voltage controller)
//!
//! Note: TMP75 temperature sensors are NOT visible until PIC enables voltage
//! to the hash board. After voltage enable, they appear at 0x48-0x4F.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;

use crate::{HalError, Result};
use dcentrald_fabric_lease::{I2cLeasePurpose, OsI2cFabricLease, PhysicalI2cFabricId};

/// Host-only I2C transport seam used by the `sim-hal` backend.
///
/// This trait is crate-private so production consumers cannot bypass the
/// existing `I2cServiceHandle` single-owner contract. The public `I2cBus`
/// type and all real-hardware constructors remain unchanged; only
/// `platform::sim` can inject an implementation when `sim-hal` is compiled.
#[cfg(feature = "sim-hal")]
pub(crate) trait I2cSimBackend: Send + Sync {
    fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize>;
    fn read(&self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<usize>;
    fn write_read(&self, bus: u8, addr: u8, write_data: &[u8], read_buf: &mut [u8]) -> Result<()>;

    fn set_timeout(&self, _bus: u8, _timeout_jiffies: u32) -> Result<()> {
        Ok(())
    }

    fn bus_recovery(&self, _bus: u8) {}

    /// Deterministic service clock used by scheduled worker jobs. `None`
    /// preserves the host monotonic clock for simple third-party test seams.
    fn service_time(&self, _bus: u8) -> Option<Duration> {
        None
    }

    /// Stable, process-unique virtual-fabric identity across clone wrappers.
    /// Service spawning rejects `None`: pointer identity can be reused after
    /// teardown and is therefore unsafe for persistent quarantine tombstones.
    fn service_identity(&self) -> Option<usize> {
        None
    }
}

/// I2C_SLAVE_FORCE ioctl command number (0x0706).
///
/// Default for DCENT_OS. Uses FORCE variant instead of regular I2C_SLAVE
/// (0x0703) — bypasses the kernel's address ownership check, which
/// prevents "address in use" errors when bosminer's fd was closed
/// uncleanly after kill -9. Without FORCE, the kernel may refuse to set
/// the slave address if it thinks another process still owns it.
const I2C_SLAVE: u32 = 0x0706;

/// I2C_SLAVE ioctl command number (0x0703) — the SAFE variant.
///
/// ** (2026-05-24, `a lab unit` strace evidence):** bosminer uses ONLY
/// `I2C_SLAVE (0x0703)` and `I2C_FUNCS (0x0708)` on `/dev/i2c-0` (per
/// `bosminer-strace-init-full.log` — zero `I2C_SLAVE_FORCE` calls
/// across the entire bosminer init). DCENT_OS uses `I2C_SLAVE_FORCE
/// (0x0706)` defensively. Both produce identical wire bytes, but the
/// Linux xiic-i2c kernel driver may initialize internal bus state
/// differently depending on which variant is used (e.g., FORCE may
/// skip some controller reset path that SLAVE triggers).
///
/// Live evidence ( strace on `a lab unit`): the dsPIC at 0x20 responds
/// to DCENT_OS with CMD echo bytes (`0x07`/`0x06`/`0x45`) instead of
/// the `0x01` ACKs that bosminer reads on the same hardware. After
/// Waves 42/43/44/46 falsified all code-only byte/timing/order
/// hypotheses, the I2C_SLAVE-vs-FORCE divergence is the only
/// remaining DCENT_OS↔bosminer kernel-syscall difference identified.
///
/// When `DCENT_AM2_I2C_SLAVE_SAFE=1`, `set_slave()` issues `I2C_SLAVE
/// (0x0703)` instead of `I2C_SLAVE_FORCE (0x0706)`. Default-OFF.
const I2C_SLAVE_SAFE: u32 = 0x0703;

/// Fixed identity prefix used by hashboard admission and controller binding.
///
/// The AT24C02 contains 256 bytes, but every current safety decision consumes
/// only the leading identity record. Keeping this service operation fixed at
/// 32 bytes bounds queue occupancy and prevents callers from turning a typed
/// identity probe into arbitrary EEPROM access.
const HASHBOARD_EEPROM_PREFIX_LEN: usize = 32;
const HASHBOARD_EEPROM_FIRST_ADDR: u8 = 0x50;
const HASHBOARD_EEPROM_LAST_ADDR: u8 = 0x57;
const LM75_FIRST_ADDR: u8 = 0x48;
const LM75_LAST_ADDR: u8 = 0x4f;

/// Exact LM75/TMP75 temperature-register evidence returned by the serialized
/// service. The signed big-endian register is retained so diagnostics can
/// preserve the device observation instead of losing fractional bits while
/// converting it for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lm75TemperatureRegister {
    raw_be: i16,
}

impl Lm75TemperatureRegister {
    pub(crate) fn from_raw_be(raw_be: i16) -> Self {
        Self { raw_be }
    }

    pub fn raw_be(self) -> i16 {
        self.raw_be
    }

    /// LM75-compatible devices encode temperature in signed 1/256 deg C
    /// units. Less precise variants keep their unused low bits at zero.
    pub fn celsius(self) -> f32 {
        self.raw_be as f32 / 256.0
    }
}

fn map_at24_read_error(bus: u8, addr: u8, path: &str, error: std::io::Error) -> HalError {
    let is_linux_eio = error.raw_os_error() == Some(libc::EIO);
    let kind = error.kind();
    let detail = format!("AT24 identity-prefix read from {path} failed: {error}");
    // Linux reports adapter NACK/I/O readiness failures as EIO. Rust maps
    // EIO to `Uncategorized` on supported toolchains, so match errno exactly
    // instead of widening every uncategorized failure into a retry.
    if is_linux_eio {
        return HalError::I2cEndpointNotReady { bus, addr, detail };
    }
    match kind {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::Interrupted
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::UnexpectedEof
        | std::io::ErrorKind::Other => HalError::I2cEndpointNotReady { bus, addr, detail },
        _ => HalError::I2cEndpointRefused { bus, addr, detail },
    }
}

fn map_lm75_read_error(bus: u8, addr: u8, error: std::io::Error) -> HalError {
    let detail = format!("LM75 temperature-register I2C_RDWR failed: {error}");
    match error.raw_os_error() {
        // Address/remote NACK identifies an unpowered or unpopulated endpoint.
        // Generic EIO and ETIMEDOUT remain transport faults because they are
        // ambiguous and may indicate an unhealthy adapter/controller.
        Some(libc::ENXIO) | Some(libc::EREMOTEIO) => {
            HalError::I2cEndpointNotReady { bus, addr, detail }
        }
        _ => HalError::I2c { bus, addr, detail },
    }
}

/// I2C_RDWR ioctl command number (combined write+read transactions).
const I2C_RDWR: u32 = 0x0707;

/// I2C message read flag.
const I2C_M_RD: u16 = 0x0001;

/// I2C message for I2C_RDWR ioctl.
#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}

/// I2C_RDWR ioctl data structure.
#[repr(C)]
struct I2cRdwrIoctlData {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

/// PIC I2C addresses for each chain on S9.
pub const PIC_ADDR_CHAIN6: u8 = 0x55;
pub const PIC_ADDR_CHAIN7: u8 = 0x56;
pub const PIC_ADDR_CHAIN8: u8 = 0x57;

/// PIC command constants (bmminer firmware — verified 2026-03-12).
pub mod pic_cmd {
    /// Preamble bytes for PIC commands: 0x55 0xAA
    pub const PREAMBLE: [u8; 2] = [0x55, 0xAA];

    /// Jump from bootloader to application firmware (0x06).
    /// ONLY send if raw I2C read returns 0xCC (bootloader mode).
    /// Sending to a PIC in app mode (0x60) puts it BACK in bootloader!
    pub const JUMP_FROM_LOADER: u8 = 0x06;

    /// Read PIC firmware version (bmminer: 0x04).
    /// Returns version (0x56/0x5A/0x5E) in app mode, 0xCC in bootloader.
    pub const GET_VERSION: u8 = 0x04;

    /// Set voltage DAC value (bmminer: 0x03).
    pub const SET_VOLTAGE: u8 = 0x03;

    /// Enable voltage output (bmminer: 0x02, no data byte).
    pub const ENABLE: u8 = 0x02;

    /// Read actual voltage from DC-DC feedback (bmminer: 0x08).
    pub const READ_VOLTAGE: u8 = 0x08;

    /// Stock Bitmain PIC16F1704 heartbeat command (bmminer format).
    /// BraiinsOS PICs use 0x16 instead — see PicController::send_heartbeat().
    /// PIC resets to bootloader after ~5s without heartbeat.
    pub const HEARTBEAT_STOCK: u8 = 0x11;

    /// PIC bootloader state indicator (raw I2C read).
    pub const BOOTLOADER: u8 = 0xCC;

    /// PIC app mode state indicator (raw I2C read).
    pub const APP_MODE: u8 = 0x60;
}

/// I2C bus wrapper.
///
/// Supports two backends:
/// - Kernel mode: uses /dev/i2c-N file handle + ioctl
/// - Devmem mode: bypasses kernel, uses AXI IIC registers via /dev/mem
pub struct I2cBus {
    /// Owned file handle for /dev/i2c-N (None in devmem mode).
    file: Option<fs::File>,
    /// Bus number (e.g., 0 for /dev/i2c-0).
    bus: u8,
    /// Currently selected slave address.
    current_addr: Option<u8>,
    /// If true, all operations go through devmem AXI IIC registers.
    devmem: bool,
    /// Per-bus write protection list. Addresses in this list refuse
    /// `write*` operations but still allow `read*`. Used to defend
    /// hashboard EEPROMs from accidental writes.
    ///
    /// **Platform-scoped**: each platform startup registers its own list
    /// via `set_write_denylist`. By default, the list is EMPTY, so no
    /// existing platform regresses. S9 (PICs at 0x55-0x57) registers
    /// nothing because those addresses are PIC voltage controllers, not
    /// EEPROMs. am2 S19j Pro registers `[0x50..=0x57]` (AT24C-series
    /// hashboard EEPROM range) because those are not used for any
    /// am2 control purpose.
    ///
    /// for the rationale
    /// and the 2026-04-29 .74 hb2 EEPROM corruption incident.
    write_denylist: Vec<u8>,
    /// Counter for blocked-write attempts since the last reset. Surfaced
    /// to the diagnostic dashboard so an operator can see if any code
    /// path is trying to write to a protected address. Atomic so we
    /// can keep `&self` semantics on the write path.
    blocked_write_count: std::sync::atomic::AtomicU64,
    /// Last timeout value successfully applied to this exact transport.
    /// `u32::MAX` means no bounded timeout has been verified since open/reopen.
    verified_timeout_jiffies: std::sync::atomic::AtomicU32,
    /// Feature-gated host simulator transport. Never present in default or
    /// firmware builds and never constructed by a real platform.
    #[cfg(feature = "sim-hal")]
    sim_backend: Option<std::sync::Arc<dyn I2cSimBackend>>,
    /// Exclusive process-local ownership for a raw bus handle. Service-worker
    /// buses validate the already-reserved runtime service entry instead and
    /// therefore store `None` here; the worker lifetime owns that reservation.
    _fabric_lease: Option<I2cRawFabricLease>,
    /// Exact service allocation allowed to reopen this worker transport.
    /// Raw/bootstrap handles store `None`; a weak reference cannot extend the
    /// worker authority lifetime or make a replacement service equivalent.
    service_authority: Option<Weak<I2cSafetyAuthority>>,
}

impl I2cBus {
    fn validate_raw_fabric_owner(&self) -> Result<()> {
        if let Some(lease) = &self._fabric_lease {
            lease.validate_current_process()?;
        }
        Ok(())
    }

    /// Open an I2C bus by number.
    ///
    /// Opens `/dev/i2c-{bus}` and returns a raw bus handle. Visibility is
    /// restricted to `pub(crate)` to enforce the **single-I2C-owner**
    /// architecture on am2 S19j Pro: in production, `/dev/i2c-0` is owned
    /// by exactly one `i2c-service` thread.
    /// Two raw `I2cBus` handles racing on the same bus reproduce the
    /// MSSP-parser corruption that bricked the .139/.74 dsPICs (see
    /// ,
    /// ,
    /// ).
    ///
    /// Out-of-HAL callers MUST go through [`I2cServiceHandle`] (constructed
    /// via [`spawn_am1_s9_i2c0_service`], [`spawn_i2c_service_no_register_touch`],
    /// or [`spawn_i2c_service_no_register_touch_with_denylist`]) instead.
    /// For the fixed one-shot miner identity read that happens before a
    /// running service, see [`read_secondary_bus_miner_identity_eeprom`].
    ///
    /// Recovery binaries (`pic-recovery`, `dspic-flash`) and HAL diagnostic
    /// examples that genuinely need raw bus access opt in via the
    /// `recovery-tool` Cargo feature on `dcentrald-hal`, which exposes
    /// [`I2cBus::open_for_recovery`]. The main `dcentrald` daemon does
    /// NOT enable that feature, so the daemon binary cannot link a raw
    /// `I2cBus::open` call — that's the compile-time half of the lockdown.
    /// The CI grep gate in `scripts/ci_offline_gates.sh` is the
    /// source-level half.
    ///
    /// Direct callers inside `dcentrald-hal` (the platform modules,
    /// `psu.rs`, `adc.rs`, the i2c-service worker itself) keep using
    /// this constructor — they are the legitimate owners.
    ///
    /// ```compile_fail
    /// use dcentrald_hal::i2c::I2cBus;
    /// let _ = I2cBus::open_devmem();
    /// ```
    pub(crate) fn open(bus: u8) -> Result<Self> {
        let fabric_lease = I2cRawFabricLease::reserve(
            I2cFabricRegistryKey::linux_adapter(bus),
            I2cRawLeaseKind::Bootstrap,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(bus),
            detail: format!("raw I2C fabric ownership was refused: {error}"),
        })?;
        Self::open_kernel_owned(bus, Some(fabric_lease))
    }

    /// Open the kernel adapter beneath an already-reserved runtime service.
    /// The exact service authority must still own the process-local fabric
    /// registry entry; a caller cannot use this as a second raw constructor.
    fn open_for_service(bus: u8, authority: &Arc<I2cSafetyAuthority>) -> Result<Self> {
        I2cServiceRegistryLease::validate_service_owner(
            I2cFabricRegistryKey::linux_adapter(bus),
            authority,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(bus),
            detail: format!("I2C service fabric ownership was lost: {error}"),
        })?;
        let mut transport = Self::open_kernel_owned(bus, None)?;
        transport.service_authority = Some(Arc::downgrade(authority));
        Ok(transport)
    }

    fn open_kernel_owned(bus: u8, fabric_lease: Option<I2cRawFabricLease>) -> Result<Self> {
        let path = format!("/dev/i2c-{}", bus);

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| HalError::DeviceOpen {
                path: path.clone(),
                source: e,
            })?;

        // Use default kernel I2C timeout (~1000ms) and retries (1).
        // Previous 100ms timeout was too aggressive — after kill -9, the PIC's
        // MSSP module needs time to recover from the interrupted transaction.
        // With 3 PICs, worst case is 3s which still fits in the 5s stock watchdog.
        let fd = file.as_raw_fd();
        unsafe {
            libc::ioctl(fd, 0x0701 as _, 3 as libc::c_int); // I2C_RETRIES = 3
        }

        tracing::debug!(bus, "Opened I2C bus (timeout=default, retries=3)");

        Ok(Self {
            file: Some(file),
            bus,
            current_addr: None,
            devmem: false,
            write_denylist: Vec::new(),
            blocked_write_count: std::sync::atomic::AtomicU64::new(0),
            verified_timeout_jiffies: std::sync::atomic::AtomicU32::new(u32::MAX),
            #[cfg(feature = "sim-hal")]
            sim_backend: None,
            _fabric_lease: fabric_lease,
            service_authority: None,
        })
    }

    /// Open an I2C bus in devmem mode (bypasses kernel xiic driver).
    /// No /dev/i2c-N file is opened. All operations go through AXI IIC registers.
    fn open_devmem_for_service(authority: &Arc<I2cSafetyAuthority>) -> Result<Self> {
        I2cServiceRegistryLease::validate_service_owner(
            I2cFabricRegistryKey::linux_adapter(0),
            authority,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(0),
            detail: format!("AXI-IIC service fabric ownership was lost: {error}"),
        })?;
        let mut transport = Self::open_devmem_owned(None);
        transport.service_authority = Some(Arc::downgrade(authority));
        Ok(transport)
    }

    fn open_devmem_owned(fabric_lease: Option<I2cRawFabricLease>) -> Self {
        Self {
            file: None,
            bus: 0,
            current_addr: None,
            devmem: true,
            write_denylist: Vec::new(),
            blocked_write_count: std::sync::atomic::AtomicU64::new(0),
            verified_timeout_jiffies: std::sync::atomic::AtomicU32::new(u32::MAX),
            #[cfg(feature = "sim-hal")]
            sim_backend: None,
            _fabric_lease: fabric_lease,
            service_authority: None,
        }
    }

    /// Test-only transport stub. It never registers a real/simulated fabric
    /// because denylist and parser unit tests intentionally perform no device
    /// open or MMIO access.
    #[cfg(test)]
    pub(crate) fn open_devmem() -> Self {
        Self::open_devmem_owned(None)
    }

    /// Construct an `I2cBus` over a host-only simulated backend.
    ///
    /// Kept crate-private for the same reason as [`Self::open`]: callers
    /// outside the HAL still use the serialized service handle rather than
    /// opening competing raw bus owners.
    #[cfg(feature = "sim-hal")]
    pub(crate) fn try_open_sim(
        bus: u8,
        backend: std::sync::Arc<dyn I2cSimBackend>,
    ) -> Result<Self> {
        let backend_identity = backend.service_identity().ok_or_else(|| HalError::I2c {
            bus,
            addr: 0,
            detail: "simulated I2C backend has no stable fabric identity".into(),
        })?;
        let fabric_lease = I2cRawFabricLease::reserve(
            I2cFabricRegistryKey::SimulatedBus {
                bus,
                backend: backend_identity,
            },
            I2cRawLeaseKind::Bootstrap,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(bus),
            detail: format!("raw simulated I2C fabric ownership was refused: {error}"),
        })?;
        Ok(Self::open_sim_owned(bus, backend, Some(fabric_lease)))
    }

    #[cfg(all(feature = "sim-hal", test))]
    pub(crate) fn open_sim(bus: u8, backend: std::sync::Arc<dyn I2cSimBackend>) -> Self {
        // White-box transport tests deliberately compose ad-hoc backends that
        // have no stable fabric identity. They do not represent production
        // ownership; alias enforcement itself is tested through `try_open_sim`.
        Self::open_sim_owned(bus, backend, None)
    }

    #[cfg(feature = "sim-hal")]
    fn open_sim_for_service(
        bus: u8,
        backend: std::sync::Arc<dyn I2cSimBackend>,
        authority: &Arc<I2cSafetyAuthority>,
    ) -> Result<Self> {
        let backend_identity = backend.service_identity().ok_or_else(|| HalError::I2c {
            bus,
            addr: 0,
            detail: "simulated I2C backend has no stable fabric identity".into(),
        })?;
        I2cServiceRegistryLease::validate_service_owner(
            I2cFabricRegistryKey::SimulatedBus {
                bus,
                backend: backend_identity,
            },
            authority,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(bus),
            detail: format!("simulated I2C service fabric ownership was lost: {error}"),
        })?;
        let mut transport = Self::open_sim_owned(bus, backend, None);
        transport.service_authority = Some(Arc::downgrade(authority));
        Ok(transport)
    }

    #[cfg(feature = "sim-hal")]
    fn open_sim_owned(
        bus: u8,
        backend: std::sync::Arc<dyn I2cSimBackend>,
        fabric_lease: Option<I2cRawFabricLease>,
    ) -> Self {
        Self {
            file: None,
            bus,
            current_addr: None,
            devmem: false,
            write_denylist: Vec::new(),
            blocked_write_count: std::sync::atomic::AtomicU64::new(0),
            verified_timeout_jiffies: std::sync::atomic::AtomicU32::new(u32::MAX),
            sim_backend: Some(backend),
            _fabric_lease: fabric_lease,
            service_authority: None,
        }
    }

    /// **Recovery-only** escape hatch around the `pub(crate)` [`Self::open`]
    /// constructor.
    ///
    /// Gated behind the `recovery-tool` Cargo feature on `dcentrald-hal`.
    /// Only enabled by `pic-recovery`, `dspic-flash`, and HAL diagnostic
    /// examples (see `examples/s19j_pic_parser_flush.rs`). The main
    /// `dcentrald` daemon does **not** enable this feature, so any
    /// regression that tries to call `I2cBus::open_for_recovery` from
    /// the daemon path is a hard compile error, not a runtime check.
    ///
    /// In production code paths, use [`I2cServiceHandle`] instead (single
    /// I²C owner contract; see [`Self::open`] doc comment).
    ///
    /// # Safety / contract
    ///
    /// The caller is responsible for ensuring no other process (e.g. a
    /// running `dcentrald`) is already serializing `/dev/i2c-N`. Recovery
    /// binaries are designed to run with the daemon stopped.
    #[cfg(feature = "recovery-tool")]
    pub fn open_for_recovery(bus: u8) -> Result<Self> {
        let fabric_lease = I2cRawFabricLease::reserve(
            I2cFabricRegistryKey::linux_adapter(bus),
            I2cRawLeaseKind::Recovery,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric: PhysicalI2cFabricId::linux_adapter(bus),
            detail: format!("recovery I2C fabric ownership was refused: {error}"),
        })?;
        Self::open_kernel_owned(bus, Some(fabric_lease))
    }
}

/// Read the fixed 32-byte miner identity record from the secondary EEPROM bus.
///
/// Runtime callers cannot choose a bus, address, offset, payload, or length:
/// the only exposed bootstrap operation is bus 1, AT24 address `0x51`, offset
/// zero. The daemon captures this before any runtime service reserves a fabric.
pub fn read_secondary_bus_miner_identity_eeprom() -> Result<Vec<u8>> {
    read_eeprom_bytes_from_bus(I2cBus::open(1)?, 0x51, 0, 32)
}

/// Arbitrary EEPROM access is recovery-tool-only and requires the daemon to be
/// stopped. The process-local recovery lease still refuses a live service or
/// another raw owner of the same physical fabric.
#[cfg(feature = "recovery-tool")]
pub fn read_eeprom_bytes(bus: u8, addr: u8, offset: u8, len: usize) -> Result<Vec<u8>> {
    read_eeprom_bytes_from_bus(I2cBus::open_for_recovery(bus)?, addr, offset, len)
}

fn read_eeprom_bytes_from_bus(
    mut i2c: I2cBus,
    addr: u8,
    offset: u8,
    len: usize,
) -> Result<Vec<u8>> {
    i2c.set_slave(addr)?;

    // Set read pointer to `offset` on the EEPROM.
    i2c.write(&[offset])?;
    std::thread::sleep(std::time::Duration::from_millis(5));

    let mut out = Vec::with_capacity(len);
    let mut remaining = len;
    while remaining > 0 {
        let chunk = remaining.min(32);
        let mut buf = vec![0u8; chunk];
        i2c.read(&mut buf)?;
        out.extend_from_slice(&buf);
        remaining -= chunk;
    }
    Ok(out)
}

impl I2cBus {
    /// Register I²C addresses that this bus must REFUSE writes to.
    /// Reads still work. Used to defend hashboard EEPROMs from accidental
    /// writes by misrouted code paths.
    ///
    /// Default: empty (no addresses blocked). S9 / S19 Pro / S21 platform
    /// startup leaves this empty. am2 S19j Pro registers `[0x50..=0x57]`.
    /// See plan: corruption-prevention lockdown 2026-04-29.
    pub fn set_write_denylist(&mut self, addrs: &[u8]) {
        self.write_denylist = addrs.to_vec();
        if !self.write_denylist.is_empty() {
            tracing::info!(
                bus = self.bus,
                addrs = ?self.write_denylist.iter().map(|a| format!("0x{:02X}", a)).collect::<Vec<_>>(),
                "I2C write denylist registered"
            );
        }
    }

    /// Number of write attempts blocked by the denylist since open.
    pub fn blocked_write_count(&self) -> u64 {
        self.blocked_write_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns true if `addr` is in the write denylist.
    fn is_write_denied(&self, addr: u8) -> bool {
        self.write_denylist.contains(&addr)
    }

    /// Refuse a write attempt at `addr`. Bumps the atomic counter and
    /// returns the standard HAL error so the caller surfaces it to logs.
    /// Takes `&self` so it can be called from the &self write paths.
    fn refuse_write(&self, addr: u8) -> HalError {
        let n = self
            .blocked_write_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        tracing::error!(
            bus = self.bus,
            addr = format_args!("0x{:02X}", addr),
            blocked_count = n,
            "I2C write REFUSED — address is on this bus's write denylist (EEPROM protection). \
             Reads at this address are still allowed; only writes are blocked. If this address \
             needs writes for a new platform feature, update the platform's I2cBus::set_write_denylist \
             call site (do NOT remove the denylist mechanism)."
        );
        HalError::I2c {
            bus: self.bus,
            addr,
            detail: format!(
                "write to 0x{:02X} refused (write denylist; reads still allowed). \
                 EEPROM/protected address. blocked_count={}",
                addr, n
            ),
        }
    }

    /// Set the I2C slave address for subsequent operations.
    ///
    /// In kernel mode: uses ioctl(I2C_SLAVE) to select the target device.
    /// In devmem mode: just stores the address (no ioctl needed).
    ///
    /// For addresses > 0x77 (dsPIC33EP on S17/S19), I2C_SLAVE ioctl returns
    /// EINVAL. These addresses are used with I2C_RDWR in write() instead.
    pub fn set_slave(&mut self, addr: u8) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        if self.current_addr == Some(addr) {
            return Ok(());
        }

        #[cfg(feature = "sim-hal")]
        if self.sim_backend.is_some() {
            self.current_addr = Some(addr);
            return Ok(());
        }

        if self.devmem {
            // Devmem mode: just store the address, no ioctl
            self.current_addr = Some(addr);
            return Ok(());
        }

        // dsPIC addresses (0x88, 0x89, 0xB9) are above the 7-bit range.
        // Skip I2C_SLAVE ioctl — use I2C_RDWR in write() instead.
        if addr > 0x77 {
            self.current_addr = Some(addr);
            return Ok(());
        }

        let fd = self.file.as_ref().unwrap().as_raw_fd();
        //  (2026-05-24): env-gated switch from I2C_SLAVE_FORCE
        // (0x0706, our defensive default) to I2C_SLAVE (0x0703, the
        // bosminer-matching safe variant). See I2C_SLAVE_SAFE doc-comment
        // above for the strace evidence + rationale. Default-OFF →
        // fleet byte-identical when DCENT_AM2_I2C_SLAVE_SAFE is unset.
        let use_safe_slave_ioctl = std::env::var("DCENT_AM2_I2C_SLAVE_SAFE")
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let ioctl_cmd: u32 = if use_safe_slave_ioctl {
            I2C_SLAVE_SAFE
        } else {
            I2C_SLAVE
        };
        // libc::Ioctl is c_ulong on glibc, c_int on musl — cast via as
        let ret = unsafe { libc::ioctl(fd, ioctl_cmd as _, addr as libc::c_int) };

        if ret < 0 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!(
                    "ioctl I2C_SLAVE{} failed: {}",
                    if use_safe_slave_ioctl { "" } else { "_FORCE" },
                    std::io::Error::last_os_error()
                ),
            });
        }

        self.current_addr = Some(addr);
        Ok(())
    }

    /// Write data bytes to the current slave device.
    ///
    /// For standard 7-bit addresses (<= 0x77): uses kernel write() after I2C_SLAVE ioctl.
    /// For extended addresses (> 0x77, e.g. dsPIC): uses I2C_RDWR ioctl which embeds
    /// the address in the message struct, bypassing I2C_SLAVE validation.
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        self.validate_raw_fabric_owner()?;
        let addr = self.current_addr.unwrap_or(0);
        validate_message_len(self.bus, addr, "raw write", data.len())?;
        if self.is_write_denied(addr) {
            return Err(self.refuse_write(addr));
        }
        // Diagnostic audit trail. Tag = `i2c_audit`, off by default (high
        // volume). Opt-in for incident investigation:
        //   RUST_LOG=i2c_audit=info,info /usr/local/bin/dcentrald
        // After a corruption incident, this lets the operator see which
        // address received which bytes — invaluable for narrowing down
        // misrouted-write bugs.
        let preview_len = data.len().min(4);
        tracing::trace!(
            target: "i2c_audit",
            bus = self.bus,
            addr = format_args!("0x{:02X}", addr),
            len = data.len(),
            head = format_args!("{:02X?}", &data[..preview_len]),
            op = "write",
        );
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            return backend.write(self.bus, addr, data);
        }
        if self.devmem {
            devmem_i2c_write(addr, data)?;
            return Ok(data.len());
        }

        // For addresses > 0x77 (dsPIC on S17/S19), use I2C_RDWR ioctl.
        // BraiinsOS uses this approach for dsPIC at 0x88/0x89/0xB9.
        if addr > 0x77 {
            return self.write_rdwr(addr, data);
        }

        let n =
            nix::unistd::write(self.file.as_ref().unwrap(), data).map_err(|e| HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!("write failed: {}", e),
            })?;
        Ok(n)
    }

    /// Write via I2C_RDWR ioctl (for addresses > 0x77 that I2C_SLAVE rejects).
    fn write_rdwr(&self, addr: u8, data: &[u8]) -> Result<usize> {
        if self.is_write_denied(addr) {
            return Err(self.refuse_write(addr));
        }
        let fd = self.file.as_ref().unwrap().as_raw_fd();

        // struct i2c_msg { addr: u16, flags: u16, len: u16, buf: *mut u8 }
        // I2C_M_TEN = 0x0010 — tells kernel to accept addr > 0x77
        let mut buf = data.to_vec();
        #[repr(C)]
        struct I2cMsg {
            addr: u16,
            flags: u16,
            len: u16,
            buf: *mut u8,
        }
        #[repr(C)]
        struct I2cRdwrData {
            msgs: *mut I2cMsg,
            nmsgs: u32,
        }

        let message_len = u16::try_from(buf.len()).map_err(|_| HalError::I2c {
            bus: self.bus,
            addr,
            detail: "I2C_RDWR write length does not fit the kernel u16 field".into(),
        })?;
        let mut msg = I2cMsg {
            addr: addr as u16,
            flags: 0x0010, // I2C_M_TEN — lets kernel accept high addresses
            len: message_len,
            buf: buf.as_mut_ptr(),
        };
        let mut rdwr = I2cRdwrData {
            msgs: &mut msg as *mut I2cMsg,
            nmsgs: 1,
        };

        let ret = unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut rdwr as *mut I2cRdwrData) };

        if ret < 0 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!("I2C_RDWR write failed: {}", std::io::Error::last_os_error()),
            });
        }
        if ret != 1 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!(
                    "I2C_RDWR write completed {ret} message(s); exact completion of 1 message is required"
                ),
            });
        }

        Ok(data.len())
    }

    /// Write data bytes one at a time (BraiinsOS byte-by-byte pattern).
    ///
    /// The PIC16F1704's MSSP I2C slave module has a limited receive buffer.
    /// Multi-byte writes in a single transaction can overflow the buffer if
    /// the PIC firmware ISR is slow to process. BraiinsOS sends each byte as
    /// a separate I2C transaction (START+addr+byte+STOP) with 1ms between
    /// bytes, giving the PIC firmware time to process each byte.
    pub fn write_byte_by_byte(&self, data: &[u8]) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        let addr = self.current_addr.unwrap_or(0);
        validate_message_len(self.bus, addr, "raw bytewise write", data.len())?;
        if self.is_write_denied(addr) {
            return Err(self.refuse_write(addr));
        }
        let preview_len = data.len().min(4);
        tracing::trace!(
            target: "i2c_audit",
            bus = self.bus,
            addr = format_args!("0x{:02X}", addr),
            len = data.len(),
            head = format_args!("{:02X?}", &data[..preview_len]),
            op = "write_byte_by_byte",
        );
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            for &byte in data {
                let written = backend.write(self.bus, addr, &[byte])?;
                require_exact_i2c_write(self.bus, addr, "bytewise write", 1, written)?;
            }
            return Ok(());
        }
        if self.devmem {
            if data.len() <= 15 {
                // PIC commands (3-4 bytes): single multi-byte I2C transaction.
                // AXI IIC TX FIFO = 16 entries (1 addr + 15 data max).
                return devmem_i2c_write(addr, data);
            }
            // Parser flush (16 bytes): byte-by-byte to avoid TX FIFO overflow.
            // Each byte is a separate START/addr/byte/STOP transaction.
            for &byte in data {
                devmem_i2c_write(addr, &[byte])?;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            return Ok(());
        }
        //  (2026-05-23): bosminer-faithful inter-byte gap.
        //
        // Bosminer's i2c-0 strace on `a lab unit` shows ~6 ms between
        // consecutive single-byte writes to slave 0x20 (the dsPIC).
        // DCENT_OS pre- used 1 ms, which may be too fast for
        // the dsPIC's MSSP I2C peripheral to recover between bytes
        // and could be what wedges 0x20. When env gate
        // `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1`, scale up to 6 ms.
        // Default off → byte-identical to prior waves on .79/.109.
        let inter_byte_ms: u64 = if std::env::var("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL")
            .map(|v| v.trim() == "1")
            .unwrap_or(false)
        {
            6
        } else {
            1
        };
        // For addresses > 0x77 (dsPIC), use I2C_RDWR for each byte.
        if addr > 0x77 {
            for &byte in data {
                let written = self.write_rdwr(addr, &[byte])?;
                require_exact_i2c_write(self.bus, addr, "bytewise I2C_RDWR write", 1, written)?;
                std::thread::sleep(std::time::Duration::from_millis(inter_byte_ms));
            }
            return Ok(());
        }
        for &byte in data {
            let written =
                nix::unistd::write(self.file.as_ref().unwrap(), &[byte]).map_err(|e| {
                    HalError::I2c {
                        bus: self.bus,
                        addr,
                        detail: format!("write failed: {}", e),
                    }
                })?;
            require_exact_i2c_write(self.bus, addr, "bytewise write", 1, written)?;
            std::thread::sleep(std::time::Duration::from_millis(inter_byte_ms));
        }
        Ok(())
    }

    /// Read data bytes from the current slave device.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        self.validate_raw_fabric_owner()?;
        let addr = self.current_addr.unwrap_or(0);
        validate_message_len(self.bus, addr, "raw read", buf.len())?;
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            return backend.read(self.bus, addr, buf);
        }
        if self.devmem {
            devmem_i2c_read(addr, buf)?;
            return Ok(buf.len());
        }
        let n = nix::unistd::read(self.file.as_ref().unwrap().as_raw_fd(), buf).map_err(|e| {
            HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!("read failed: {}", e),
            }
        })?;
        Ok(n)
    }

    /// Combined write-then-read using I2C_RDWR ioctl (repeated START).
    ///
    /// This is CRITICAL for PIC communication — separate write() + read()
    /// transactions return garbage (I2C address echo) instead of the actual
    /// PIC response. The I2C_RDWR ioctl sends both messages in one kernel
    /// call with a repeated START condition between write and read.
    pub fn write_read(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        let addr = self.current_addr.unwrap_or(0);
        self.write_read_at(addr, write_data, read_buf, false)
    }

    fn write_read_at(
        &mut self,
        addr: u8,
        write_data: &[u8],
        read_buf: &mut [u8],
        endpoint_readiness: bool,
    ) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        validate_message_len(self.bus, addr, "raw write-read write", write_data.len())?;
        validate_message_len(self.bus, addr, "raw write-read read", read_buf.len())?;
        // write_read writes a command byte before the read; the write half
        // is what the denylist guards.
        if self.is_write_denied(addr) {
            return Err(self.refuse_write(addr));
        }
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            return backend.write_read(self.bus, addr, write_data, read_buf);
        }
        if self.devmem {
            // In devmem mode: write then read as separate transactions.
            // Dynamic mode doesn't support repeated START, so we do write + read.
            devmem_i2c_write(addr, write_data)?;
            std::thread::sleep(std::time::Duration::from_millis(1));
            devmem_i2c_read(addr, read_buf)?;
            return Ok(());
        }
        let fd = self.file.as_ref().unwrap().as_raw_fd();

        // We need mutable copies for the pointers
        let mut write_buf = write_data.to_vec();

        let write_len = u16::try_from(write_buf.len()).map_err(|_| HalError::I2c {
            bus: self.bus,
            addr,
            detail: "I2C_RDWR write length does not fit the kernel u16 field".into(),
        })?;
        let read_len = u16::try_from(read_buf.len()).map_err(|_| HalError::I2c {
            bus: self.bus,
            addr,
            detail: "I2C_RDWR read length does not fit the kernel u16 field".into(),
        })?;
        let mut msgs = [
            I2cMsg {
                addr: addr as u16,
                flags: 0, // write
                len: write_len,
                buf: write_buf.as_mut_ptr(),
            },
            I2cMsg {
                addr: addr as u16,
                flags: I2C_M_RD,
                len: read_len,
                buf: read_buf.as_mut_ptr(),
            },
        ];

        let mut data = I2cRdwrIoctlData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 2,
        };

        let ret = unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut data as *mut I2cRdwrIoctlData) };

        if ret < 0 {
            let error = std::io::Error::last_os_error();
            return Err(if endpoint_readiness {
                map_lm75_read_error(self.bus, addr, error)
            } else {
                HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail: format!("I2C_RDWR ioctl failed: {error}"),
                }
            });
        }
        if ret != 2 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!(
                    "I2C_RDWR write-read completed {ret} message(s); exact completion of 2 messages is required"
                ),
            });
        }

        Ok(())
    }

    /// Read the fixed LM75 temperature register with one repeated-start
    /// I2C_RDWR transfer. This deliberately does not issue I2C_SLAVE or
    /// I2C_SLAVE_FORCE: both messages carry the endpoint address themselves.
    fn read_lm75_temperature_register(&mut self, addr: u8) -> Result<Lm75TemperatureRegister> {
        self.validate_raw_fabric_owner()?;
        if !(LM75_FIRST_ADDR..=LM75_LAST_ADDR).contains(&addr) {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "LM75 temperature address must be within 0x48..=0x4f".into(),
            });
        }
        let mut bytes = [0_u8; 2];
        self.write_read_at(addr, &[0x00], &mut bytes, true)?;
        Ok(Lm75TemperatureRegister::from_raw_be(i16::from_be_bytes(
            bytes,
        )))
    }

    /// Read one fixed AT24 identity prefix through a platform-admitted
    /// protected endpoint.
    ///
    /// Production kernel services use the bound `at24` driver's sysfs file,
    /// so this operation never issues `I2C_SLAVE[_FORCE]` and remains
    /// compatible with an address already owned by the kernel. Simulation
    /// models the equivalent offset-zero transfer explicitly. The service's
    /// platform write-denylist is also the admission policy: an S9 or generic
    /// service with no AT24 policy cannot use this operation at PIC addresses.
    fn read_protected_hashboard_eeprom_prefix(&self, addr: u8) -> Result<Vec<u8>> {
        self.validate_raw_fabric_owner()?;
        if !(HASHBOARD_EEPROM_FIRST_ADDR..=HASHBOARD_EEPROM_LAST_ADDR).contains(&addr) {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "hashboard EEPROM prefix address must be within 0x50..=0x57".into(),
            });
        }
        if !self.is_write_denied(addr) {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "hashboard EEPROM prefix read refused: endpoint is not admitted by this service's protected-address policy".into(),
            });
        }

        let mut prefix = vec![0_u8; HASHBOARD_EEPROM_PREFIX_LEN];
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            backend.write_read(self.bus, addr, &[0], &mut prefix)?;
            return Ok(prefix);
        }

        if self.devmem {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "kernel-bound AT24 identity reads are unavailable on a devmem I2C service"
                    .into(),
            });
        }

        let path = format!("/sys/bus/i2c/devices/{}-{:04x}/eeprom", self.bus, addr);
        let mut file = fs::File::open(&path)
            .map_err(|error| map_at24_read_error(self.bus, addr, &path, error))?;
        file.read_exact(&mut prefix)
            .map_err(|error| map_at24_read_error(self.bus, addr, &path, error))?;
        Ok(prefix)
    }

    /// Send a PIC command to a specific chain's voltage controller.
    ///
    /// PIC commands use preamble 0x55 0xAA followed by the command byte.
    pub fn pic_command(&mut self, addr: u8, cmd: u8) -> Result<()> {
        self.set_slave(addr)?;
        self.write(&[pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], cmd])?;
        Ok(())
    }

    /// Send a PIC command with a data payload.
    pub fn pic_command_with_data(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<()> {
        self.set_slave(addr)?;
        let mut buf = vec![pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], cmd];
        buf.extend_from_slice(data);
        self.write(&buf)?;
        Ok(())
    }

    /// Jump PIC from bootloader to application.
    ///
    /// ONLY call if raw I2C read returns 0xCC (bootloader).
    /// Sending JUMP to a PIC already in app mode (0x60) puts it BACK in bootloader!
    pub fn pic_jump_to_app(&mut self, addr: u8) -> Result<()> {
        tracing::info!(addr = format_args!("0x{:02X}", addr), "PIC: jump to app");
        self.pic_command(addr, pic_cmd::JUMP_FROM_LOADER)
    }

    /// Read raw PIC state (plain I2C read, no command).
    /// Returns 0x60 for app mode, 0xCC for bootloader.
    pub fn pic_read_raw(&mut self, addr: u8) -> Result<u8> {
        self.set_slave(addr)?;
        let mut buf = [0u8; 1];
        self.read(&mut buf)?;
        Ok(buf[0])
    }

    /// Set voltage on a PIC voltage controller (bmminer cmd 0x03).
    ///
    /// PIC voltage formula: voltage_V = (1608.42 - pic_val) / 170.42
    ///   pic_val 75  = 9.0V
    ///   pic_val 100 = 8.85V
    ///   pic_val 150 = 8.56V
    pub fn pic_set_voltage(&mut self, addr: u8, pic_val: u8) -> Result<()> {
        tracing::info!(
            addr = format_args!("0x{:02X}", addr),
            pic_val,
            voltage = format_args!("{:.2}V", (1608.42 - pic_val as f64) / 170.42),
            "PIC: set voltage"
        );
        self.pic_command_with_data(addr, pic_cmd::SET_VOLTAGE, &[pic_val])
    }

    /// Enable hash board power via PIC (bmminer cmd 0x02, no data byte).
    pub fn pic_enable(&mut self, addr: u8) -> Result<()> {
        tracing::info!(
            addr = format_args!("0x{:02X}", addr),
            "PIC: enable voltage output"
        );
        self.pic_command(addr, pic_cmd::ENABLE)
    }

    /// Read actual voltage from DC-DC (bmminer cmd 0x08, I2C_RDWR).
    pub fn pic_read_voltage(&mut self, addr: u8) -> Result<u8> {
        self.set_slave(addr)?;
        let cmd = [
            pic_cmd::PREAMBLE[0],
            pic_cmd::PREAMBLE[1],
            pic_cmd::READ_VOLTAGE,
        ];
        let mut buf = [0u8; 1];
        self.write_read(&cmd, &mut buf)?;
        Ok(buf[0])
    }

    /// Get PIC firmware version (bmminer cmd 0x04, I2C_RDWR).
    pub fn pic_get_version(&mut self, addr: u8) -> Result<u8> {
        self.set_slave(addr)?;
        let cmd = [
            pic_cmd::PREAMBLE[0],
            pic_cmd::PREAMBLE[1],
            pic_cmd::GET_VERSION,
        ];
        let mut buf = [0u8; 1];
        self.write_read(&cmd, &mut buf)?;
        Ok(buf[0])
    }

    /// Get the bus number.
    pub fn bus(&self) -> u8 {
        self.bus
    }

    /// Set I2C transaction timeout.
    ///
    /// The timeout value is in units of 10ms (jiffies at HZ=100, which is
    /// standard for Zynq 4.4 kernels). For example, `timeout_jiffies=10`
    /// gives a 100ms timeout per I2C transaction.
    ///
    /// Default kernel timeout is 1000ms (100 jiffies), which is too long
    /// for heartbeats — a dead PIC blocks the bus for 1s+ per transaction.
    /// BraiinsOS uses short timeouts to prevent cascading failures.
    pub fn set_timeout(&self, timeout_jiffies: u32) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            backend.set_timeout(self.bus, timeout_jiffies)?;
            self.verified_timeout_jiffies
                .store(timeout_jiffies, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        if let Some(ref file) = self.file {
            let fd = file.as_raw_fd();
            // I2C_TIMEOUT = 0x0702
            let ret = unsafe { libc::ioctl(fd, 0x0702 as _, timeout_jiffies as libc::c_int) };
            if ret < 0 {
                return Err(HalError::I2c {
                    bus: self.bus,
                    addr: 0,
                    detail: format!(
                        "ioctl I2C_TIMEOUT failed: {}",
                        std::io::Error::last_os_error()
                    ),
                });
            }
            self.verified_timeout_jiffies
                .store(timeout_jiffies, std::sync::atomic::Ordering::SeqCst);
        }
        // The direct AXI-IIC transport retains its historical no-op success
        // contract here, but its explicit poll/recovery/retry bounds are not
        // equivalent to the kernel I2C_TIMEOUT value. Runtime schedulers must
        // therefore continue to see that transport as unverified.
        Ok(())
    }

    fn timeout_is_verified(&self, timeout_jiffies: u32) -> bool {
        self.verified_timeout_jiffies
            .load(std::sync::atomic::Ordering::SeqCst)
            == timeout_jiffies
    }

    /// Attempt I2C bus recovery by generating 9 SCL clocks.
    ///
    /// In devmem mode: sends 9 dummy read transactions to address 0x03
    /// (which will NACK but generates SCL clocks to unstick slave SDA).
    /// In kernel mode: returns an explicit unsupported error because the
    /// kernel adapter owns its recovery policy.
    pub(crate) fn bus_recovery(&mut self) -> Result<()> {
        self.validate_raw_fabric_owner()?;
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = &self.sim_backend {
            backend.bus_recovery(self.bus);
            return Ok(());
        }
        if self.devmem {
            return bus_recovery_devmem();
        }
        Err(HalError::I2c {
            bus: self.bus,
            addr: 0,
            detail: "explicit whole-fabric recovery is unavailable on a kernel-managed I2C adapter"
                .into(),
        })
    }

    fn service_time(&self) -> Option<Duration> {
        #[cfg(feature = "sim-hal")]
        if let Some(backend) = self.sim_backend.as_ref() {
            return backend.service_time(self.bus);
        }
        None
    }

    /// Map actual wire completion back into the scheduler's monotonic domain.
    /// Simulator transports use virtual time; real transports include elapsed
    /// host time so settle intervals begin only after the frame completed.
    fn scheduled_completion_time(
        &self,
        scheduled_start: Instant,
        host_start: Instant,
        service_start: Option<Duration>,
    ) -> Instant {
        match (service_start, self.service_time()) {
            (Some(before), Some(after)) => scheduled_start + after.saturating_sub(before),
            _ => scheduled_start + host_start.elapsed(),
        }
    }
}

/// AXI IIC controller base address (Xilinx axi_iic IP on S9 Zynq).
const AXI_IIC_BASE: u64 = 0x4160_0000;

/// AXI IIC register offsets (from Xilinx PG090).
const AXI_IIC_ISR: usize = 0x020; // Interrupt Status Register (IPIF space, NOT 0x004!)
const AXI_IIC_SOFTR: usize = 0x040; // Software Reset Register
const AXI_IIC_CR: usize = 0x100; // Control Register
const AXI_IIC_SR: usize = 0x104; // Status Register
const AXI_IIC_TX_FIFO: usize = 0x108; // TX FIFO (write: data to send)
const AXI_IIC_RX_FIFO: usize = 0x10C; // RX FIFO (read: received data)

/// CR bit masks.
const CR_EN: u32 = 0x01; // Enable
const CR_TX_FIFO_RESET: u32 = 0x02; // Reset TX FIFO
const _CR_MSMS: u32 = 0x04; // Master/Slave Mode Select (1=master) — unused in dynamic mode
const _CR_TX: u32 = 0x08; // Transmit mode — unused in dynamic mode

/// SR bit masks.
const SR_BB: u32 = 0x04; // Bus Busy
const SR_RX_FIFO_EMPTY: u32 = 0x40; // RX FIFO Empty (bit 6 of SR)
const SR_TX_FIFO_EMPTY: u32 = 0x80; // TX FIFO Empty

/// ISR bit masks.
const ISR_TX_ERROR: u32 = 0x02; // Bit 1 = TX Error/NACK (NOT bit 0 which is ARB_LOST)

/// TX FIFO control bits.
const TX_START: u32 = 0x100; // Generate START condition
const TX_STOP: u32 = 0x200; // Generate STOP condition

/// Additional AXI IIC register offsets used by persistent devmem I2C.
const AXI_IIC_GIE: usize = 0x01C; // Global Interrupt Enable
const AXI_IIC_IER: usize = 0x028; // Interrupt Enable Register (IPIF space, NOT 0x008!)

/// AXI IIC timing register offsets (from Linux xiic driver + Xilinx PG090).
/// SOFTR resets these to 0 (= max speed), which causes PIC NACKs.
/// Must set after every SOFTR to maintain 100 kHz I2C clock.
const AXI_IIC_TSUSTA: usize = 0x128; // Setup time for repeated START
const AXI_IIC_TSUSTO: usize = 0x12C; // Setup time for STOP
const AXI_IIC_THDSTA: usize = 0x130; // Hold time for (repeated) START
const AXI_IIC_TSUDAT: usize = 0x134; // Data setup time
const AXI_IIC_TBUF: usize = 0x138; // Bus free time between STOP and START
const AXI_IIC_THIGH: usize = 0x13C; // SCL high period
const AXI_IIC_TLOW: usize = 0x140; // SCL low period
const AXI_IIC_THDDAT: usize = 0x144; // Data hold time

/// AXI IIC timing constants — matched to BraiinsOS live capture (s9, 2026-03-26).
///
/// The AXI IIC input clock is FCLK0 (~300 MHz on BraiinsOS FPGA bitstream).
/// Previous value of 993 for all registers caused intermittent PIC NACKs after
/// ~48 seconds of mining — we were running I2C at ~150 kHz instead of 100 kHz.
///
/// BraiinsOS sets DIFFERENT values for each register (not all the same).
/// Each value is from a live devmem capture of the AXI IIC controller on a
/// BraiinsOS S9 actively mining with 0 heartbeat failures.
const IIC_THIGH: u32 = 1498; // 0x5DA — SCL high period (100 kHz at FCLK0)
const IIC_TLOW: u32 = 1498; // 0x5DA — SCL low period
const IIC_TBUF: u32 = 499; // 0x1F3 — Bus free time between STOP and START
const IIC_TSUSTA: u32 = 570; // 0x23A — Setup time for repeated START
const IIC_TSUSTO: u32 = 499; // 0x1F3 — Setup time for STOP
const IIC_THDSTA: u32 = 430; // 0x1AE — Hold time for (repeated) START
const IIC_TSUDAT: u32 = 55; // 0x037 — Data setup time
const IIC_THDDAT: u32 = 1; // 0x001 — Data hold time

/// Reset the Xilinx AXI IIC controller via SOFTR register.
///
/// # Safety: ONLY call when kernel xiic driver is NOT bound
///
/// This function writes directly to AXI IIC hardware registers via /dev/mem.
/// If the kernel xiic driver is bound (i.e., /dev/i2c-0 exists), calling this
/// function WILL desynchronize the kernel driver's internal state machine from
/// the actual hardware state. The kernel driver tracks CR, ISR, and FIFO state
/// internally — a devmem SOFTR resets the hardware behind its back, causing
/// ALL subsequent kernel I2C transactions to fail permanently. Reopening the
/// kernel fd does NOT fix this because open() does not trigger xiic_reinit().
///
/// Valid call sites: devmem_i2c_write/read retry paths (kernel driver unbound).
/// NEVER call from i2c_service_loop or any kernel-mode I2C path.
///
/// History: FIX J (2026-03-14) added this for I2C_RDWR recovery. v0.12.1
/// removed its use from try_reset_and_reopen() after discovering it was the
/// root cause of cascading I2C failures during mining.
pub(crate) fn reset_axi_iic_controller() -> Result<()> {
    use nix::sys::mman::{MapFlags, ProtFlags};
    use std::num::NonZeroUsize;

    let mem_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::DeviceOpen {
            path: "/dev/mem".to_string(),
            source: e,
        })?;

    let page_size = NonZeroUsize::new(4096).unwrap();

    let ptr = unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &mem_file,
            AXI_IIC_BASE as nix::libc::off_t,
        )
        .map_err(|e| HalError::MmapFailed {
            device: format!("axi-iic @ 0x{:08X}", AXI_IIC_BASE),
            source: e,
        })?
    };

    let base = ptr.as_ptr() as *mut u8;

    unsafe {
        // Read SR before reset for diagnostics
        let sr_before = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);
        let cr_before = std::ptr::read_volatile(base.add(AXI_IIC_CR) as *const u32);

        // Step 1: Disable the IIC core entirely (CR = 0x00).
        // This deasserts the bus-busy FSM that SOFTR alone cannot clear.
        // Without this, SR=0xC0 (bus-busy + addressed-as-slave) persists forever.
        std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, 0x0000_0000);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Step 2: SOFTR = 0x0A — clear ISR flags and FIFOs
        std::ptr::write_volatile(base.add(AXI_IIC_SOFTR) as *mut u32, 0x0000_000A);
        std::thread::sleep(std::time::Duration::from_millis(1));

        // Step 2b: Restore ALL I2C timing (SOFTR resets to 0 = max speed = PIC NACKs).
        // Values matched to BraiinsOS live capture (s9, 2026-03-26).
        std::ptr::write_volatile(base.add(AXI_IIC_THIGH) as *mut u32, IIC_THIGH);
        std::ptr::write_volatile(base.add(AXI_IIC_TLOW) as *mut u32, IIC_TLOW);
        std::ptr::write_volatile(base.add(AXI_IIC_TBUF) as *mut u32, IIC_TBUF);
        std::ptr::write_volatile(base.add(AXI_IIC_THDSTA) as *mut u32, IIC_THDSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTA) as *mut u32, IIC_TSUSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTO) as *mut u32, IIC_TSUSTO);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUDAT) as *mut u32, IIC_TSUDAT);
        std::ptr::write_volatile(base.add(AXI_IIC_THDDAT) as *mut u32, IIC_THDDAT);

        // Step 3: Re-enable the IIC controller (CR = 0x01)
        std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, 0x0000_0001);
        std::thread::sleep(std::time::Duration::from_millis(1));

        // Read SR after reset for diagnostics
        let sr_after = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);

        // SR bit map: bit7=TX_FIFO_EMPTY, bit6=RX_FIFO_EMPTY, bit2=BB(bus-busy)
        // SR=0xC0 is NORMAL idle state (both FIFOs empty, bus not busy).
        let bus_busy = (sr_after & 0x04) != 0;
        tracing::info!(
            cr_before = format_args!("0x{:08X}", cr_before),
            sr_before = format_args!("0x{:08X}", sr_before),
            sr_after = format_args!("0x{:08X}", sr_after),
            bus_busy,
            "AXI IIC controller reset — SR: 0x{:08X} → 0x{:08X} (BB={}, TX_EMPTY={}, RX_EMPTY={})",
            sr_before,
            sr_after,
            if bus_busy { "BUSY" } else { "idle" },
            if sr_after & 0x80 != 0 { "yes" } else { "no" },
            if sr_after & 0x40 != 0 { "yes" } else { "no" },
        );

        // Unmap
        let _ = nix::sys::mman::munmap(ptr, 4096);
        if let Some(reason) = axi_iic_stuck_reason(sr_after) {
            return Err(HalError::I2c {
                bus: 0,
                addr: 0,
                detail: format!(
                    "AXI-IIC reset failed its idle postcondition: {reason:?} (SR=0x{sr_after:08X})"
                ),
            });
        }
    }

    Ok(())
}

/// Recover the direct AXI-IIC fabric and prove that recovery reached a sane
/// idle postcondition.
///
/// A NACK from the deliberately unused address 0x03 is the expected outcome:
/// it proves the controller generated START/address/STOP clocks. Setup errors,
/// transfer timeouts, uncleared interrupt state, and a stuck post-state remain
/// errors. The full operation holds one transport lock so ordinary worker I/O
/// cannot interleave between recovery pulses.
pub(crate) fn bus_recovery_devmem() -> Result<()> {
    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&base) => base as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        let status = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);
        if status & SR_BB != 0 {
            std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, 0);
            std::thread::sleep(std::time::Duration::from_millis(10));
            std::ptr::write_volatile(base.add(AXI_IIC_SOFTR) as *mut u32, 0x0000_000A);
            std::thread::sleep(std::time::Duration::from_millis(1));
            std::ptr::write_volatile(base.add(AXI_IIC_THIGH) as *mut u32, IIC_THIGH);
            std::ptr::write_volatile(base.add(AXI_IIC_TLOW) as *mut u32, IIC_TLOW);
            std::ptr::write_volatile(base.add(AXI_IIC_TBUF) as *mut u32, IIC_TBUF);
            std::ptr::write_volatile(base.add(AXI_IIC_THDSTA) as *mut u32, IIC_THDSTA);
            std::ptr::write_volatile(base.add(AXI_IIC_TSUSTA) as *mut u32, IIC_TSUSTA);
            std::ptr::write_volatile(base.add(AXI_IIC_TSUSTO) as *mut u32, IIC_TSUSTO);
            std::ptr::write_volatile(base.add(AXI_IIC_TSUDAT) as *mut u32, IIC_TSUDAT);
            std::ptr::write_volatile(base.add(AXI_IIC_THDDAT) as *mut u32, IIC_THDDAT);
            std::ptr::write_volatile(base.add(AXI_IIC_GIE) as *mut u32, 0);
            std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, CR_EN);
            std::thread::sleep(std::time::Duration::from_millis(1));

            let reset_status = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);
            if let Some(reason) = axi_iic_stuck_reason(reset_status) {
                return Err(HalError::I2c {
                    bus: 0,
                    addr: 0,
                    detail: format!(
                        "AXI-IIC remained unhealthy after recovery reset: {reason:?} (SR=0x{reset_status:08X})"
                    ),
                });
            }
        }
    }

    for _ in 0..9 {
        let mut dummy = [0u8; 1];
        match unsafe { devmem_i2c_read_inner(base, 0x03, &mut dummy) } {
            Ok(()) => {}
            Err(DevmemI2cReadError::ExpectedAddressNack { .. }) => {}
            Err(error) => return Err(error.into_hal_error(0x03)),
        }

        unsafe {
            std::ptr::write_volatile(base.add(AXI_IIC_ISR) as *mut u32, ISR_TX_ERROR);
            let isr = std::ptr::read_volatile(base.add(AXI_IIC_ISR) as *const u32);
            if isr & ISR_TX_ERROR != 0 {
                return Err(HalError::I2c {
                    bus: 0,
                    addr: 0x03,
                    detail: format!("AXI-IIC recovery could not clear TX_ERROR (ISR=0x{isr:08X})"),
                });
            }
        }
    }

    let post_status = unsafe { std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32) };
    if let Some(reason) = axi_iic_stuck_reason(post_status) {
        return Err(HalError::I2c {
            bus: 0,
            addr: 0,
            detail: format!(
                "AXI-IIC recovery pulses completed without an idle postcondition: {reason:?} (SR=0x{post_status:08X})"
            ),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AXI IIC stuck-state detection + escalating recovery (WAVE-0 STABILIZE)
// ---------------------------------------------------------------------------
//
// ROOT CAUSE (CLAUDE "AXI IIC Controller Stuck State", live S9 audit N7/B2):
// the Xilinx axi_iic controller (PG090) can wedge such that SOFTR alone does
// NOT recover it, after which EVERY transaction NACKs/times out (the live S9
// shows all 8 addresses NACKing with 12V present). The pre-WAVE-0 devmem retry
// path only called `bus_recovery_devmem()` (a conditional SOFTR + 9 SCL pulses)
// on each NACK and never escalated to the full controller re-init that
// `reset_axi_iic_controller()` performs (CR=0 -> SOFTR -> timing restore ->
// CR=EN), so a genuinely wedged controller was never brought back — it just
// emitted "I2C bus recovered via SCL clock recovery" forever.
//
// The functions below add (1) a pure SR-bit classification of the stuck
// condition, and (2) a pure escalation policy keyed on the consecutive-failure
// count, both host-testable; plus (3) an in-place full controller reset on the
// already-mapped registers so we can escalate without re-`mmap`ing.

/// Why the AXI IIC controller looks stuck, decoded from the Status Register.
///
/// Bit map (PG090): bit7=TX_FIFO_EMPTY (0x80), bit6=RX_FIFO_EMPTY (0x40),
/// bit2=BB/bus-busy (0x04). The healthy *idle* SR is 0xC0 (both FIFOs empty,
/// bus not busy) — that is explicitly NOT a stuck state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxiIicStuck {
    /// Bus-busy (BB) asserted with no in-flight transaction — the master FSM
    /// is hung holding the bus; only CR=0 + SOFTR clears it.
    BusBusyHung,
    /// TX FIFO reports non-empty while the bus is idle — a transaction's bytes
    /// never drained (controller stalled mid-FIFO).
    TxFifoStalled,
    /// The all-zero SR seen when the controller is disabled/unclocked/wedged
    /// (CR_EN dropped or the IP lost its clock). Needs a full re-init.
    ControllerDown,
}

/// Classify the AXI IIC Status Register. Returns `None` for the healthy idle
/// state (SR & 0xC0 == 0xC0, BB clear), i.e. "not stuck". Pure + host-testable.
pub fn axi_iic_stuck_reason(sr: u32) -> Option<AxiIicStuck> {
    let bus_busy = sr & SR_BB != 0;
    let tx_empty = sr & SR_TX_FIFO_EMPTY != 0;
    let rx_empty = sr & SR_RX_FIFO_EMPTY != 0;

    if bus_busy {
        // Bus busy while we are between transactions => master FSM hung.
        return Some(AxiIicStuck::BusBusyHung);
    }
    if sr == 0 {
        // No FIFO-empty bits at all + not busy: the core is disabled or its
        // clock is gone. (A live, enabled, idle core always reads >= 0xC0.)
        return Some(AxiIicStuck::ControllerDown);
    }
    if !tx_empty {
        // Idle bus but the TX FIFO never drained => stalled transaction.
        return Some(AxiIicStuck::TxFifoStalled);
    }
    let _ = rx_empty; // RX-empty alone is not a fault here.
    None
}

/// One rung of the escalating bus-recovery ladder, chosen from how many
/// consecutive recoveries have already been attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxiIicRecoveryTier {
    /// Light touch: conditional SOFTR-if-busy + 9 SCL clock pulses
    /// (`bus_recovery_devmem`). Clears a stuck *slave* (PIC MSSP) and a
    /// transiently-busy bus without disturbing controller timing.
    SclPulses,
    /// Heavy: full controller re-init (CR=0 -> SOFTR -> restore all 8 timing
    /// regs + IER -> CR=EN). The only thing that recovers a wedged *controller*
    /// when SCL pulses repeatedly fail to.
    FullControllerReset,
    /// We have escalated repeatedly without success — treat the bus/PIC as
    /// dead for now (the daemon's per-PIC back-off then stops hammering it).
    GiveUp,
}

/// How many SCL-pulse-only recoveries to try before escalating to a full
/// controller reset. Small so a genuinely wedged controller is re-inited fast,
/// but >1 so a one-off stuck slave is cleared cheaply first.
pub const AXI_IIC_SCL_TIER_LIMIT: u32 = 3;

/// After this many consecutive recoveries (SCL + full resets) we stop trying.
pub const AXI_IIC_GIVE_UP_AFTER: u32 = 12;

/// Pick the recovery tier for the Nth consecutive failure (`consecutive` is the
/// post-increment count: the 1st recovery passes `1`). Pure + host-testable.
///
/// Ladder: SCL pulses for the first `AXI_IIC_SCL_TIER_LIMIT`, then full
/// controller resets, then give up at `AXI_IIC_GIVE_UP_AFTER`.
pub fn axi_iic_recovery_tier(consecutive: u32) -> AxiIicRecoveryTier {
    if consecutive == 0 || consecutive <= AXI_IIC_SCL_TIER_LIMIT {
        AxiIicRecoveryTier::SclPulses
    } else if consecutive < AXI_IIC_GIVE_UP_AFTER {
        AxiIicRecoveryTier::FullControllerReset
    } else {
        AxiIicRecoveryTier::GiveUp
    }
}

/// Read the current AXI IIC Status Register from the persistent devmem mmap.
///
/// Returns `None` if the persistent mmap has not been established yet (the
/// caller treats that as "cannot assess — assume needs recovery"). LIVE-ONLY:
/// off-hardware there is no mmap and this returns `None`.
pub(crate) fn axi_iic_read_sr() -> Option<u32> {
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    DEVMEM_IIC_MMAP.get().map(|&base_addr| {
        let base = base_addr as *mut u8;
        unsafe { std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32) }
    })
}

/// Full in-place controller re-init on the already-mapped AXI IIC registers.
///
/// This is the persistent-mmap twin of [`reset_axi_iic_controller`] (which
/// opens its own transient mmap). It performs the documented escape from the
/// SR=0xC0-class wedged state: disable the core (CR=0) to deassert the bus-busy
/// FSM that SOFTR alone cannot clear, SOFTR to flush ISR/FIFOs, restore ALL 8
/// timing registers (SOFTR zeros them -> otherwise PIC NACKs at max speed),
/// re-enable IER + the core, then clear any stale TX_ERROR.
///
/// Returns the post-reset SR so the caller can confirm the controller came
/// back to a sane idle state (>= 0xC0, BB clear). LIVE-ONLY.
pub(crate) fn full_controller_reset_devmem() -> Option<u32> {
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base_addr = *DEVMEM_IIC_MMAP.get()?;
    let base = base_addr as *mut u8;
    unsafe {
        let sr_before = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);

        // Step 1: disable the core entirely — deasserts the bus-busy FSM that a
        // bare SOFTR cannot clear (this is the piece bus_recovery_devmem's
        // conditional SOFTR omits when SR_BB is not set but the core is wedged).
        std::ptr::write_volatile(base.add(AXI_IIC_GIE) as *mut u32, 0);
        std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, 0);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Step 2: SOFTR — clear ISR flags + both FIFOs.
        std::ptr::write_volatile(base.add(AXI_IIC_SOFTR) as *mut u32, 0x0000_000A);
        std::thread::sleep(std::time::Duration::from_millis(1));

        // Step 3: restore ALL timing regs (SOFTR zeroed them = max speed = NACKs).
        std::ptr::write_volatile(base.add(AXI_IIC_THIGH) as *mut u32, IIC_THIGH);
        std::ptr::write_volatile(base.add(AXI_IIC_TLOW) as *mut u32, IIC_TLOW);
        std::ptr::write_volatile(base.add(AXI_IIC_TBUF) as *mut u32, IIC_TBUF);
        std::ptr::write_volatile(base.add(AXI_IIC_THDSTA) as *mut u32, IIC_THDSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTA) as *mut u32, IIC_TSUSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTO) as *mut u32, IIC_TSUSTO);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUDAT) as *mut u32, IIC_TSUDAT);
        std::ptr::write_volatile(base.add(AXI_IIC_THDDAT) as *mut u32, IIC_THDDAT);

        // Step 4: re-enable interrupts the devmem path expects + the core.
        std::ptr::write_volatile(base.add(AXI_IIC_IER) as *mut u32, 0x0000_001F);
        std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, CR_EN);
        std::thread::sleep(std::time::Duration::from_millis(1));

        // Step 5: clear any stale TX_ERROR latched during the wedge.
        std::ptr::write_volatile(base.add(AXI_IIC_ISR) as *mut u32, ISR_TX_ERROR);

        let sr_after = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);
        tracing::warn!(
            sr_before = format_args!("0x{:02X}", sr_before),
            sr_after = format_args!("0x{:02X}", sr_after),
            stuck_before = ?axi_iic_stuck_reason(sr_before),
            stuck_after = ?axi_iic_stuck_reason(sr_after),
            "AXI IIC full controller reset (devmem) — CR=0 -> SOFTR -> timing restore -> CR=EN"
        );
        Some(sr_after)
    }
}

/// Escalating AXI IIC bus recovery for the devmem retry path.
///
/// `consecutive` is the running count of consecutive recovery attempts (the
/// caller's `consecutive_resets`, post-increment). It selects a tier via
/// [`axi_iic_recovery_tier`] and applies it:
///   - `SclPulses`            -> `bus_recovery_devmem()` (light)
///   - `FullControllerReset`  -> `full_controller_reset_devmem()` (heavy)
///   - `GiveUp`               -> a final full reset, then stop escalating
///
/// Before choosing, it reads SR and, if the controller is in a `BusBusyHung`
/// or `ControllerDown` state, jumps straight to the full reset regardless of
/// tier (SCL pulses cannot recover a wedged *controller*). LIVE-ONLY for the
/// register effects; the tier/decode logic is unit-tested.
pub(crate) fn axi_iic_escalating_recovery(consecutive: u32) -> Result<AxiIicRecoveryTier> {
    let sr = axi_iic_read_sr();
    let controller_wedged = matches!(
        sr.and_then(axi_iic_stuck_reason),
        Some(AxiIicStuck::BusBusyHung) | Some(AxiIicStuck::ControllerDown)
    );

    let tier = axi_iic_recovery_tier(consecutive);
    // A wedged controller never recovers via SCL pulses alone — promote.
    let effective = if controller_wedged && tier == AxiIicRecoveryTier::SclPulses {
        AxiIicRecoveryTier::FullControllerReset
    } else {
        tier
    };

    match effective {
        AxiIicRecoveryTier::SclPulses => {
            bus_recovery_devmem()?;
        }
        AxiIicRecoveryTier::FullControllerReset => {
            let reset_status = full_controller_reset_devmem().ok_or_else(|| HalError::I2c {
                bus: 0,
                addr: 0,
                detail: "AXI-IIC full recovery reset had no mapped controller".into(),
            })?;
            if let Some(reason) = axi_iic_stuck_reason(reset_status) {
                return Err(HalError::I2c {
                    bus: 0,
                    addr: 0,
                    detail: format!(
                        "AXI-IIC full recovery reset failed its postcondition: {reason:?} (SR=0x{reset_status:08X})"
                    ),
                });
            }
            // Follow the controller reset with SCL pulses to release any slave
            // (PIC MSSP) still holding SDA from the wedge.
            bus_recovery_devmem()?;
        }
        AxiIicRecoveryTier::GiveUp => {
            return Err(HalError::I2c {
                bus: 0,
                addr: 0,
                detail: format!(
                    "AXI-IIC recovery escalation exhausted after {consecutive} consecutive attempts"
                ),
            });
        }
    }
    Ok(effective)
}

/// Restore the AXI IIC interrupt state for the kernel xiic driver.
///
/// CRITICAL: devmem I2C operations during init disable the Global Interrupt Enable
/// (GIE=0) to prevent kernel driver interference. After init, the kernel driver needs
/// GIE re-enabled to receive transaction completion interrupts. Without this, kernel
/// I2C writes timeout because the ISR never fires.
///
/// Call this AFTER all devmem I2C operations are complete and BEFORE starting
/// any kernel I2C operations (heartbeat thread).
pub(crate) fn restore_kernel_i2c_interrupts() -> Result<()> {
    use nix::sys::mman::{MapFlags, ProtFlags};
    use std::num::NonZeroUsize;

    let mem_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::DeviceOpen {
            path: "/dev/mem".to_string(),
            source: e,
        })?;

    let page_size = NonZeroUsize::new(4096).unwrap();
    let ptr = unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &mem_file,
            AXI_IIC_BASE as nix::libc::off_t,
        )
        .map_err(|e| HalError::MmapFailed {
            device: format!("axi-iic @ 0x{:08X}", AXI_IIC_BASE),
            source: e,
        })?
    };

    let base = ptr.as_ptr() as *mut u8;

    unsafe {
        let gie_before = std::ptr::read_volatile(base.add(AXI_IIC_GIE) as *const u32);
        let ier_before = std::ptr::read_volatile(base.add(AXI_IIC_IER) as *const u32);

        // Re-enable Global Interrupt Enable (bit 31) for kernel xiic driver.
        // devmem init sets GIE=0 to prevent kernel ISR interference during
        // direct register manipulation. Now that init is done, the kernel
        // driver needs interrupts to detect transaction completion.
        std::ptr::write_volatile(base.add(AXI_IIC_GIE) as *mut u32, 0x8000_0000);

        // v0.11.3: CRITICAL — Restore IER (Interrupt Enable Register).
        // devmem init's SOFTR zeros IER. The kernel xiic driver needs IER bits
        // set for transaction completion interrupts. With IER=0, the ISR never
        // fires and EVERY kernel I2C transaction times out at ~18ms.
        // Bit 0: ARB_LOST, Bit 1: TX_ERROR, Bit 2: TX_EMPTY, Bit 3: RX_FULL
        // Bit 4: BUS_NOT_BUSY — these are the standard xiic interrupt enables.
        std::ptr::write_volatile(base.add(AXI_IIC_IER) as *mut u32, 0x0000_001F);

        // Ensure ALL I2C timing registers match BraiinsOS (SOFTR during init
        // resets them, and the kernel driver doesn't restore all of them).
        std::ptr::write_volatile(base.add(AXI_IIC_THIGH) as *mut u32, IIC_THIGH);
        std::ptr::write_volatile(base.add(AXI_IIC_TLOW) as *mut u32, IIC_TLOW);
        std::ptr::write_volatile(base.add(AXI_IIC_TBUF) as *mut u32, IIC_TBUF);
        std::ptr::write_volatile(base.add(AXI_IIC_THDSTA) as *mut u32, IIC_THDSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTA) as *mut u32, IIC_TSUSTA);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUSTO) as *mut u32, IIC_TSUSTO);
        std::ptr::write_volatile(base.add(AXI_IIC_TSUDAT) as *mut u32, IIC_TSUDAT);
        std::ptr::write_volatile(base.add(AXI_IIC_THDDAT) as *mut u32, IIC_THDDAT);

        let gie_after = std::ptr::read_volatile(base.add(AXI_IIC_GIE) as *const u32);
        let ier_after = std::ptr::read_volatile(base.add(AXI_IIC_IER) as *const u32);
        let timing_after = [
            std::ptr::read_volatile(base.add(AXI_IIC_THIGH) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_TLOW) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_TBUF) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_THDSTA) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_TSUSTA) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_TSUSTO) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_TSUDAT) as *const u32),
            std::ptr::read_volatile(base.add(AXI_IIC_THDDAT) as *const u32),
        ];
        let expected_timing = [
            IIC_THIGH, IIC_TLOW, IIC_TBUF, IIC_THDSTA, IIC_TSUSTA, IIC_TSUSTO, IIC_TSUDAT,
            IIC_THDDAT,
        ];
        let restoration_verified =
            gie_after == 0x8000_0000 && ier_after == 0x0000_001F && timing_after == expected_timing;

        tracing::info!(
            gie_before = format_args!("0x{:08X}", gie_before),
            ier_before = format_args!("0x{:08X}", ier_before),
            gie_after = format_args!("0x{:08X}", gie_after),
            "AXI IIC GIE restored for kernel xiic driver — interrupts re-enabled",
        );

        let _ = nix::sys::mman::munmap(ptr, 4096);
        if !restoration_verified {
            return Err(HalError::I2c {
                bus: 0,
                addr: 0,
                detail: format!(
                    "AXI-IIC register restoration failed readback: GIE=0x{gie_after:08X}, IER=0x{ier_after:08X}, timing={timing_after:X?}"
                ),
            });
        }
    }

    Ok(())
}

// File descriptor is closed automatically when `self.file` is dropped.

// ---------------------------------------------------------------------------
// Persistent devmem AXI IIC mmap — shared across read/write paths
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::sync::OnceLock;

/// Persistent mmap pointer to AXI IIC registers (set once, never unmapped).
/// Stored as usize for Send/Sync safety (raw pointers are not Send).
static DEVMEM_IIC_MMAP: OnceLock<usize> = OnceLock::new();

/// One-time AXI IIC controller initialization flag.
static DEVMEM_IIC_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global mutex protecting all devmem AXI IIC register access.
/// The AXI IIC controller has a single TX FIFO — concurrent writes from
/// the init heartbeat thread and cold boot init corrupt the bus.
static DEVMEM_IIC_LOCK: Mutex<()> = Mutex::new(());

/// Diagnostic transaction counter — log first 20, then every 50th.
static DEVMEM_DIAG_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Dedicated NACK counter (WAVE-0 audit B3) — NACK WARN logs are rate-limited
/// on this counter (first 20, then every 200th) so a whole-bus-NACK fault
/// cannot flood the persistent log ring at ~33 lines/s.
static DEVMEM_NACK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Initialize the persistent /dev/mem mmap for AXI IIC. Called once.
fn init_devmem_iic_mmap() -> Result<*mut u8> {
    use nix::sys::mman::{MapFlags, ProtFlags};
    use std::num::NonZeroUsize;

    if let Some(&base) = DEVMEM_IIC_MMAP.get() {
        return Ok(base as *mut u8);
    }
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&base) = DEVMEM_IIC_MMAP.get() {
        return Ok(base as *mut u8);
    }

    let mem_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::DeviceOpen {
            path: "/dev/mem".to_string(),
            source: e,
        })?;

    let page_size = NonZeroUsize::new(4096).unwrap();
    let ptr = unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &mem_file,
            AXI_IIC_BASE as nix::libc::off_t,
        )
        .map_err(|e| HalError::MmapFailed {
            device: format!("axi-iic @ 0x{:08X}", AXI_IIC_BASE),
            source: e,
        })?
    };

    let base = ptr.as_ptr() as *mut u8;
    // Store the pointer as usize (never unmapped — persistent for process lifetime)
    if DEVMEM_IIC_MMAP.set(base as usize).is_err() {
        unsafe {
            let _ = nix::sys::mman::munmap(ptr, 4096);
        }
        return DEVMEM_IIC_MMAP
            .get()
            .map(|&existing| existing as *mut u8)
            .ok_or_else(|| HalError::Other("AXI-IIC mapping publication failed".into()));
    }
    tracing::info!(
        "devmem I2C: persistent mmap established at 0x{:08X}",
        AXI_IIC_BASE
    );
    Ok(base)
}

fn verify_devmem_iic_idle() -> Result<()> {
    let base_addr = *DEVMEM_IIC_MMAP
        .get()
        .ok_or_else(|| HalError::Other("AXI-IIC persistent mapping was not established".into()))?;
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let status =
        unsafe { std::ptr::read_volatile((base_addr as *mut u8).add(AXI_IIC_SR) as *const u32) };
    if status & !0xFF != 0 {
        return Err(HalError::I2c {
            bus: 0,
            addr: 0,
            detail: format!("AXI-IIC status register is inaccessible (SR=0x{status:08X})"),
        });
    }
    if let Some(reason) = axi_iic_stuck_reason(status) {
        return Err(HalError::I2c {
            bus: 0,
            addr: 0,
            detail: format!(
                "AXI-IIC is not idle and startup recovery was disabled: {reason:?} (SR=0x{status:08X})"
            ),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KernelI2cDriverDisposition {
    AlreadyUnbound,
    Unbound,
}

fn unbind_kernel_i2c_driver_at(
    sysfs_root: &std::path::Path,
    write_unbind: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
) -> Result<KernelI2cDriverDisposition> {
    const DEVICE: &str = "41600000.i2c";
    const DRIVER: &str = "xiic-i2c";

    let platform_device = sysfs_root.join("bus/platform/devices").join(DEVICE);
    std::fs::metadata(&platform_device).map_err(|error| {
        HalError::Other(format!(
            "cannot prove AXI-IIC kernel-driver state: {} is unavailable: {error}",
            platform_device.display()
        ))
    })?;

    let driver_link = platform_device.join("driver");
    let driver_target = match std::fs::read_link(&driver_link) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(device = DEVICE, "AXI-IIC kernel driver already unbound");
            return Ok(KernelI2cDriverDisposition::AlreadyUnbound);
        }
        Err(error) => {
            return Err(HalError::Other(format!(
                "cannot inspect AXI-IIC driver binding {}: {error}",
                driver_link.display()
            )));
        }
    };
    let bound_driver = driver_target.file_name().and_then(std::ffi::OsStr::to_str);
    if bound_driver != Some(DRIVER) {
        return Err(HalError::Other(format!(
            "refusing AXI-IIC MMIO ownership: {DEVICE} is bound to unexpected driver {}",
            driver_target.display()
        )));
    }

    let unbind = sysfs_root
        .join("bus/platform/drivers")
        .join(DRIVER)
        .join("unbind");
    write_unbind(&unbind, DEVICE.as_bytes()).map_err(|error| {
        HalError::Other(format!(
            "failed to unbind {DEVICE} through {}: {error}",
            unbind.display()
        ))
    })?;

    for _ in 0..20 {
        match std::fs::read_link(&driver_link) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(device = DEVICE, "Unbound kernel xiic-i2c driver");
                return Ok(KernelI2cDriverDisposition::Unbound);
            }
            Err(error) => {
                return Err(HalError::Other(format!(
                    "cannot verify AXI-IIC driver unbind at {}: {error}",
                    driver_link.display()
                )));
            }
            Ok(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }

    Err(HalError::Other(format!(
        "AXI-IIC driver unbind was not observable at {}",
        driver_link.display()
    )))
}

/// Prove the S9 AXI-IIC platform device is unbound before direct MMIO use.
/// A failed attempt is not cached, so a corrected sysfs state can be retried.
fn unbind_kernel_i2c_driver() -> Result<KernelI2cDriverDisposition> {
    unbind_kernel_i2c_driver_at(std::path::Path::new("/sys"), |path, payload| {
        std::fs::write(path, payload)
    })
}

/// Direct AXI IIC master write via /dev/mem (bypasses kernel xiic-i2c driver).
///
/// The BraiinsOS kernel's xiic-i2c driver is broken — it does SOFTR before every
/// transaction, destroying AXI IIC timing registers and causing 2/3 PICs to NACK.
/// This function bypasses the kernel entirely and writes directly to the
/// AXI IIC controller registers via a persistent /dev/mem mmap.
///
/// Uses dynamic mode: TX FIFO START+addr(W) + data + STOP.
/// Same persistent mmap and one-time init as devmem_i2c_read().
pub(crate) fn devmem_i2c_write(addr: u8, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    // Serialize all AXI IIC access — heartbeat thread + init must not interleave
    // v0.16.0: Kernel driver is unbound at daemon startup. All I2C is devmem now.

    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&b) => b as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let result = unsafe { devmem_i2c_write_inner(base, addr, data) };
    if result.is_err() {
        drop(_guard);
        bus_recovery_devmem()?;
        let _guard2 = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::sleep(std::time::Duration::from_millis(5));
        unsafe { devmem_i2c_write_inner(base, addr, data) }
    } else {
        result
    }
}

/// Inner implementation of devmem I2C write (operates on persistent mmapped registers).
///
/// Uses dynamic mode: TX FIFO START+addr(W) + data bytes + STOP.
/// One-time init on first call (shared with read path via DEVMEM_IIC_INITIALIZED).
unsafe fn devmem_i2c_write_inner(base: *mut u8, addr: u8, data: &[u8]) -> Result<()> {
    let read_reg = |off: usize| -> u32 { std::ptr::read_volatile(base.add(off) as *const u32) };
    let write_reg = |off: usize, val: u32| {
        std::ptr::write_volatile(base.add(off) as *mut u32, val);
    };

    // One-time init (shared with read path)
    let already_init = DEVMEM_IIC_INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst);
    if !already_init {
        write_reg(AXI_IIC_GIE, 0x00000000);
        write_reg(AXI_IIC_CR, 0x00000000);
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_reg(AXI_IIC_SOFTR, 0x0000_000A);
        std::thread::sleep(std::time::Duration::from_millis(5));
        // CRITICAL: Set ALL I2C timing AFTER SOFTR (resets to 0 = max speed = PIC NACKs).
        // Values matched to BraiinsOS live capture (s9, 2026-03-26).
        write_reg(AXI_IIC_THIGH, IIC_THIGH);
        write_reg(AXI_IIC_TLOW, IIC_TLOW);
        write_reg(AXI_IIC_TBUF, IIC_TBUF);
        write_reg(AXI_IIC_THDSTA, IIC_THDSTA);
        write_reg(AXI_IIC_TSUSTA, IIC_TSUSTA);
        write_reg(AXI_IIC_TSUSTO, IIC_TSUSTO);
        write_reg(AXI_IIC_TSUDAT, IIC_TSUDAT);
        write_reg(AXI_IIC_THDDAT, IIC_THDDAT);
        write_reg(AXI_IIC_IER, 0x0000_001F);
        write_reg(AXI_IIC_CR, CR_EN);
        std::thread::sleep(std::time::Duration::from_millis(1));
        tracing::info!(
            "devmem I2C: one-time AXI IIC init — THIGH/TLOW={}, TBUF={}, TSUSTA={}",
            IIC_THIGH,
            IIC_TBUF,
            IIC_TSUSTA
        );
    }

    write_reg(AXI_IIC_GIE, 0x00000000);

    // Flush TX FIFO
    write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
    write_reg(AXI_IIC_CR, CR_EN);

    // Wait for bus idle
    let mut bb_wait = 0u32;
    loop {
        let sr = read_reg(AXI_IIC_SR);
        if sr & SR_BB == 0 {
            break;
        }
        if bb_wait >= 500 {
            return Err(HalError::I2c {
                bus: 0,
                addr,
                detail: format!(
                    "devmem write: bus stuck busy (SR=0x{:02X})",
                    read_reg(AXI_IIC_SR)
                ),
            });
        }
        bb_wait += 1;
        std::thread::sleep(std::time::Duration::from_micros(100));
    }

    // Clear stale ISR bits
    let stale_isr = read_reg(AXI_IIC_ISR);
    if stale_isr != 0 {
        write_reg(AXI_IIC_ISR, stale_isr);
    }

    // Dynamic mode write: address byte with W bit + data + STOP on last byte
    let addr_byte = TX_START | ((addr as u32) << 1);
    write_reg(AXI_IIC_TX_FIFO, addr_byte);

    for (i, &byte) in data.iter().enumerate() {
        let mut fifo_val = byte as u32;
        if i == data.len() - 1 {
            fifo_val |= TX_STOP;
        }
        write_reg(AXI_IIC_TX_FIFO, fifo_val);
    }

    // Wait for transfer to start (BB goes high)
    let t0 = std::time::Instant::now();
    let mut started = false;
    loop {
        if read_reg(AXI_IIC_SR) & SR_BB != 0 {
            started = true;
            break;
        }
        if t0.elapsed() >= std::time::Duration::from_millis(10) {
            break;
        }
    }
    if !started {
        write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
        write_reg(AXI_IIC_CR, CR_EN);
        return Err(HalError::I2c {
            bus: 0,
            addr,
            detail: "devmem write: START not generated".into(),
        });
    }

    // Wait for transfer complete (BB goes low)
    let t_start = std::time::Instant::now();
    let mut poll_count = 0u32;
    for _ in 0..2000 {
        poll_count += 1;
        let sr = read_reg(AXI_IIC_SR);
        if sr & SR_BB == 0 {
            let isr = read_reg(AXI_IIC_ISR);
            let sr_final = read_reg(AXI_IIC_SR);
            let elapsed_us = t_start.elapsed().as_micros();
            let is_nack = isr & ISR_TX_ERROR != 0;
            let tx_fifo_empty = sr_final & SR_TX_FIFO_EMPTY != 0;

            // Diagnostic: log transaction details.
            // NACK = WARN level, success = INFO level. This prevents confusion where
            // NACKed transactions appeared as INFO in the log.
            //
            // WAVE-0 (audit B3): the previous gate `|| is_nack` logged EVERY NACK
            // at WARN. On the live S9 (whole-bus NACK with 12V present) that is
            // ~33 lines/s -> ~2M lines/day, and the entire captured log ring was
            // this one storm. NACKs are now RATE-LIMITED on their own counter
            // (first 20, then every 200th) just like the success path — the
            // upstream devmem retry + per-PIC back-off already throttle how often
            // we even reach a NACK, and the operator still sees the onset + a
            // periodic heartbeat of the ongoing fault.
            let diag_n = DEVMEM_DIAG_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let log_this = if is_nack {
                let nack_n = DEVMEM_NACK_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
                nack_n < 20 || nack_n.is_multiple_of(200)
            } else {
                diag_n < 20 || diag_n.is_multiple_of(50)
            };
            if log_this {
                let thigh = read_reg(AXI_IIC_THIGH);
                let tlow = read_reg(AXI_IIC_TLOW);
                if is_nack {
                    // NACK: SR bit 7 (TX_FIFO_EMPTY) clear = data stuck in FIFO = address NACK
                    //       SR bit 7 set = data transmitted but last byte NACKed
                    tracing::warn!(
                        "DIAG_I2C_WRITE: addr=0x{:02X} n={} NACK ISR=0x{:02X} SR=0x{:02X} TX_EMPTY={} THIGH={} TLOW={} polls={} us={} bytes={} (NACK log rate-limited)",
                        addr, diag_n, isr, sr_final, tx_fifo_empty, thigh, tlow, poll_count, elapsed_us, data.len(),
                    );
                } else {
                    tracing::info!(
                        "DIAG_I2C_WRITE: addr=0x{:02X} n={} OK ISR=0x{:02X} SR=0x{:02X} TX_EMPTY={} THIGH={} TLOW={} polls={} us={} bytes={}",
                        addr, diag_n, isr, sr_final, tx_fifo_empty, thigh, tlow, poll_count, elapsed_us, data.len(),
                    );
                }
            }

            if is_nack {
                write_reg(AXI_IIC_ISR, ISR_TX_ERROR);
                write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
                write_reg(AXI_IIC_CR, CR_EN);
                return Err(HalError::I2c {
                    bus: 0,
                    addr,
                    detail: format!(
                        "devmem write: NACK (ISR=0x{:02X} SR=0x{:02X} TX_EMPTY={} polls={} us={})",
                        isr, sr_final, tx_fifo_empty, poll_count, elapsed_us,
                    ),
                });
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_micros(100));
    }

    // Timeout — full register dump for diagnosis
    let sr_to = read_reg(AXI_IIC_SR);
    let isr_to = read_reg(AXI_IIC_ISR);
    let cr_to = read_reg(AXI_IIC_CR);
    let thigh_to = read_reg(AXI_IIC_THIGH);
    let tlow_to = read_reg(AXI_IIC_TLOW);
    let gie_to = read_reg(AXI_IIC_GIE);
    tracing::error!(
        "DIAG_I2C_TIMEOUT: addr=0x{:02X} SR=0x{:02X} ISR=0x{:02X} CR=0x{:02X} THIGH={} TLOW={} GIE=0x{:08X} started={} bytes={}",
        addr, sr_to, isr_to, cr_to, thigh_to, tlow_to, gie_to, started, data.len(),
    );
    write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
    write_reg(AXI_IIC_CR, CR_EN);
    Err(HalError::I2c {
        bus: 0,
        addr,
        detail: format!(
            "devmem I2C write timeout (SR=0x{:02X} ISR=0x{:02X} started={})",
            sr_to, isr_to, started
        ),
    })
}

/// Direct AXI IIC master read via /dev/mem (bypasses kernel xiic-i2c driver).
///
/// Uses dynamic mode: TX FIFO START+addr(R) + byte_count|STOP, then reads RX FIFO.
/// Same persistent mmap and one-time init as devmem_i2c_write().
///
/// On NACK: retries once after a full SOFTR reset + timing restore.
#[derive(Debug)]
enum DevmemI2cReadError {
    ExpectedAddressNack { isr: u32 },
    Transport(HalError),
}

impl From<HalError> for DevmemI2cReadError {
    fn from(error: HalError) -> Self {
        Self::Transport(error)
    }
}

impl DevmemI2cReadError {
    fn into_hal_error(self, addr: u8) -> HalError {
        match self {
            Self::ExpectedAddressNack { isr } => HalError::I2c {
                bus: 0,
                addr,
                detail: format!("devmem read: NACK (ISR=0x{isr:02X})"),
            },
            Self::Transport(error) => error,
        }
    }
}

pub(crate) fn devmem_i2c_read(addr: u8, buf: &mut [u8]) -> Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    if buf.len() > 15 {
        return Err(HalError::I2c {
            bus: 0,
            addr,
            detail: format!("devmem read too large: {} bytes (max 15)", buf.len()),
        });
    }

    // Serialize all AXI IIC access — heartbeat thread + init must not interleave
    // v0.16.0: Kernel driver is unbound at daemon startup. All I2C is devmem now.

    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&b) => b as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let result = unsafe { devmem_i2c_read_inner(base, addr, buf) };
    if result.is_err() {
        // v0.20.1: NEVER SOFTR on NACK — kills PIC MSSP (documented regression).
        // Use bus recovery (SCL clocks) instead.
        drop(_guard);
        bus_recovery_devmem()?;
        let _guard2 = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::sleep(std::time::Duration::from_millis(5));
        unsafe { devmem_i2c_read_inner(base, addr, buf) }
            .map_err(|error| error.into_hal_error(addr))
    } else {
        result.map_err(|error| error.into_hal_error(addr))
    }
}

unsafe fn devmem_i2c_read_inner(
    base: *mut u8,
    addr: u8,
    buf: &mut [u8],
) -> std::result::Result<(), DevmemI2cReadError> {
    let read_reg = |off: usize| -> u32 { std::ptr::read_volatile(base.add(off) as *const u32) };
    let write_reg = |off: usize, val: u32| {
        std::ptr::write_volatile(base.add(off) as *mut u32, val);
    };

    // One-time init (same as write path — reuses DEVMEM_IIC_INITIALIZED flag)
    let already_init = DEVMEM_IIC_INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst);
    if !already_init {
        write_reg(AXI_IIC_GIE, 0x00000000);
        write_reg(AXI_IIC_CR, 0x00000000);
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_reg(AXI_IIC_SOFTR, 0x0000_000A);
        std::thread::sleep(std::time::Duration::from_millis(5));
        // CRITICAL: Set ALL I2C timing AFTER SOFTR (same values as write path).
        write_reg(AXI_IIC_THIGH, IIC_THIGH);
        write_reg(AXI_IIC_TLOW, IIC_TLOW);
        write_reg(AXI_IIC_TBUF, IIC_TBUF);
        write_reg(AXI_IIC_THDSTA, IIC_THDSTA);
        write_reg(AXI_IIC_TSUSTA, IIC_TSUSTA);
        write_reg(AXI_IIC_TSUSTO, IIC_TSUSTO);
        write_reg(AXI_IIC_TSUDAT, IIC_TSUDAT);
        write_reg(AXI_IIC_THDDAT, IIC_THDDAT);
        write_reg(AXI_IIC_IER, 0x0000_001F);
        write_reg(AXI_IIC_CR, CR_EN);
        std::thread::sleep(std::time::Duration::from_millis(1));
        tracing::info!(
            "devmem I2C read: one-time AXI IIC init — THIGH/TLOW={}, TBUF={}",
            IIC_THIGH,
            IIC_TBUF
        );
    }

    write_reg(AXI_IIC_GIE, 0x00000000);

    // Flush TX FIFO
    write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
    write_reg(AXI_IIC_CR, CR_EN);

    // Drain stale RX FIFO data
    for _ in 0..16 {
        let sr = read_reg(AXI_IIC_SR);
        if sr & SR_RX_FIFO_EMPTY != 0 {
            break;
        }
        let _ = read_reg(AXI_IIC_RX_FIFO);
    }

    // Wait for bus idle
    let mut bb_wait = 0u32;
    loop {
        let sr = read_reg(AXI_IIC_SR);
        if sr & SR_BB == 0 {
            break;
        }
        if bb_wait >= 500 {
            return Err(HalError::I2c {
                bus: 0,
                addr,
                detail: format!(
                    "devmem read: bus stuck busy (SR=0x{:02X})",
                    read_reg(AXI_IIC_SR)
                ),
            }
            .into());
        }
        bb_wait += 1;
        std::thread::sleep(std::time::Duration::from_micros(100));
    }

    // Clear stale ISR bits
    let stale_isr = read_reg(AXI_IIC_ISR);
    if stale_isr != 0 {
        write_reg(AXI_IIC_ISR, stale_isr);
    }

    // Dynamic mode read: address byte with R bit + byte count with STOP
    let addr_byte = TX_START | ((addr as u32) << 1) | 0x01;
    write_reg(AXI_IIC_TX_FIFO, addr_byte);
    write_reg(AXI_IIC_TX_FIFO, TX_STOP | (buf.len() as u32));

    // Wait for transfer to start (BB goes high)
    let t0 = std::time::Instant::now();
    let mut started = false;
    loop {
        if read_reg(AXI_IIC_SR) & SR_BB != 0 {
            started = true;
            break;
        }
        if t0.elapsed() >= std::time::Duration::from_millis(10) {
            break;
        }
    }
    if !started {
        write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
        write_reg(AXI_IIC_CR, CR_EN);
        return Err(HalError::I2c {
            bus: 0,
            addr,
            detail: "devmem read: START not generated".into(),
        }
        .into());
    }

    // Wait for transfer complete (BB goes low)
    for _ in 0..2000 {
        let sr = read_reg(AXI_IIC_SR);
        if sr & SR_BB == 0 {
            let isr = read_reg(AXI_IIC_ISR);
            if isr & ISR_TX_ERROR != 0 {
                write_reg(AXI_IIC_ISR, ISR_TX_ERROR);
                write_reg(AXI_IIC_CR, CR_TX_FIFO_RESET | CR_EN);
                write_reg(AXI_IIC_CR, CR_EN);
                return Err(DevmemI2cReadError::ExpectedAddressNack { isr });
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(100));
    }

    // Read RX FIFO
    for byte in buf.iter_mut() {
        let mut rx_wait = 0u32;
        loop {
            let sr = read_reg(AXI_IIC_SR);
            if sr & SR_RX_FIFO_EMPTY == 0 {
                break;
            }
            if rx_wait >= 500 {
                return Err(HalError::I2c {
                    bus: 0,
                    addr,
                    detail: "devmem read: RX FIFO empty after transfer (50ms timeout)".into(),
                }
                .into());
            }
            rx_wait += 1;
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        *byte = (read_reg(AXI_IIC_RX_FIFO) & 0xFF) as u8;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// I2C Service — serialized single-thread I2C bus access
// ---------------------------------------------------------------------------

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::time::{Duration, Instant};

mod pic16_admission;
mod pic16_runtime;

use pic16_admission::{
    hal_busy_error, validate_admitted_batch_for_service, validate_safe_off_handle_for_service,
    Pic16AdmissionDelivery, Pic16AdmissionPlan, Pic16AdmissionReservation, Pic16AdmissionState,
    Pic16BatchAuthority, Pic16BatchSafeOffAttempt, Pic16BatchSafeOffOwnership,
};
pub use pic16_admission::{
    Pic16AdmissionFailure, Pic16AdmissionJob, Pic16AdmissionMode, Pic16AdmissionStage,
    Pic16AdmissionTarget, Pic16AdmittedBatch, Pic16AdmittedEndpoint, Pic16ApplicationEvidence,
    Pic16BatchSafeOffDisposition, Pic16BatchSafeOffOutcome, Pic16CompensationOutcome,
    Pic16CompensationStatus, Pic16HeartbeatRoundOutcome, Pic16RunningEndpoint,
    Pic16RuntimeEndpointId, Pic16SafeOffHandle, Pic16SetVoltageOutcome,
};
use pic16_runtime::{Pic16HeartbeatRoundState, Pic16WorkerJob};

const PIC16_ADMISSION_IDLE: u64 = 0;
const PIC16_ADMISSION_ACTIVE_BIT: u64 = 1 << 63;
const PIC16_ADMISSION_TOKEN_MAX: u64 = PIC16_ADMISSION_ACTIVE_BIT - 1;
const PIC16_RUNTIME_MAX_ENDPOINTS: usize = 3;

/// Firmware type indicator (mirrors PicFirmware enum without depending on dcentrald-asic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cPicFirmware {
    Stock,
    BraiinsOs,
    Unknown,
}

/// Result of the fixed PIC16F1704 bootloader transition request.
///
/// The worker owns both the exact one-byte observation and the conditional
/// transition. Callers cannot provide a predicate or transition bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cPic16JumpOutcome {
    /// A byte other than the sole transition authority (`0xCC`) was observed.
    ObservedNoJump { raw_state: u8 },
    /// The worker observed exact `0xCC`, sent the fixed bytewise JUMP frame,
    /// waited the bounded application-start interval, and then read one exact
    /// post-transition byte.
    JumpSentFromExactBootloader { post_jump_raw_state: u8 },
}

/// Semantic safety class for one serialized I2C operation.
///
/// The service must not infer this from opcodes: generic transactions carry
/// controller-specific protocols whose safety meaning is known only by their
/// typed caller. `UnclassifiedMutation` is the conservative compatibility
/// class while those callers migrate; it is fenced exactly like `Energize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum I2cOperationIntent {
    ReadOnly,
    KeepAlive,
    Energize,
    SafeOff,
    NeutralControl,
    Recovery,
    UnclassifiedMutation,
}

/// Audit classification for a controller-mutating I2C operation.
///
/// Every variant has identical authorization semantics: the operation must
/// belong to the current safety generation, is rejected after terminal
/// safe-off, and counts as an in-flight controller mutation. This label may
/// improve logs, but can never grant `ReadOnly` or `SafeOff` privilege.
///
/// The privileged internal intent is deliberately not part of the public API:
///
/// ```compile_fail
/// use dcentrald_hal::i2c::I2cOperationIntent;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cMutationLabel {
    KeepAlive,
    Energize,
    NeutralControl,
    Recovery,
    QueryPrelude,
    Unclassified,
}

impl I2cMutationLabel {
    fn internal_intent(self) -> I2cOperationIntent {
        match self {
            Self::KeepAlive => I2cOperationIntent::KeepAlive,
            Self::Energize => I2cOperationIntent::Energize,
            Self::NeutralControl => I2cOperationIntent::NeutralControl,
            Self::Recovery => I2cOperationIntent::Recovery,
            // A write-bearing query is a controller mutation even when its
            // protocol-level purpose is observation.
            Self::QueryPrelude | Self::Unclassified => I2cOperationIntent::UnclassifiedMutation,
        }
    }
}

fn pic_voltage_controller_address_is_valid(addr: u8) -> bool {
    matches!(addr, 0x20..=0x22 | 0x55..=0x57)
}

fn validate_pic_voltage_controller_address(bus: u8, addr: u8, operation: &str) -> Result<()> {
    if !pic_voltage_controller_address_is_valid(addr) {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: format!(
                "{operation} requires a PIC voltage-controller address in 0x20..=0x22 or 0x55..=0x57"
            ),
        });
    }
    Ok(())
}

fn validate_pic16_safe_off_address(bus: u8, addr: u8) -> Result<()> {
    if !(0x55..=0x57).contains(&addr) {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: "PIC16 safe-off requires a controller address in 0x55..=0x57; use the protocol-specific dsPIC or PIC1704 disable API for 0x20..=0x22"
                .into(),
        });
    }
    Ok(())
}

fn validate_pic16_boot_transition_endpoint(bus: u8, addr: u8) -> Result<()> {
    if bus != 0 || !(0x55..=0x57).contains(&addr) {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: "PIC16 boot observation/transition requires standard I2C bus 0 and a discovery-bound controller address in 0x55..=0x57"
                .into(),
        });
    }
    Ok(())
}

fn validate_pic16_endpoint_capability(
    service_bus: u8,
    endpoint: &crate::platform::VoltageControllerEndpoint,
    operation: &str,
) -> Result<u8> {
    if endpoint.kind() != crate::platform::VoltageControllerKind::Pic16f1704 {
        return Err(HalError::I2c {
            bus: service_bus,
            addr: endpoint.address(),
            detail: format!(
                "{} endpoint cannot authorize {operation}",
                endpoint.kind().as_str()
            ),
        });
    }
    if endpoint.bus() != service_bus {
        return Err(HalError::I2c {
            bus: service_bus,
            addr: endpoint.address(),
            detail: format!(
                "PIC16 endpoint is bound to I2C bus {}, but service owns bus {}",
                endpoint.bus(),
                service_bus
            ),
        });
    }
    let addr = endpoint.address();
    validate_pic16_boot_transition_endpoint(service_bus, addr)?;
    Ok(addr)
}

fn validate_dspic_voltage_controller_address(bus: u8, addr: u8, operation: &str) -> Result<()> {
    if !(0x20..=0x22).contains(&addr) {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: format!(
                "{operation} requires a dsPIC voltage-controller address in 0x20..=0x22"
            ),
        });
    }
    Ok(())
}

/// Fixed, protocol-validated dsPIC DISABLE frame selection.
///
/// Callers choose a known wire protocol, never bytes or an authorization
/// class. All variants are immutable monotonic safe-off operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cDspicDisableProtocol {
    Bare,
    CanonicalFramed,
    VnishPaddedFramed,
}

impl I2cOperationIntent {
    #[inline]
    fn requires_current_safety_generation(self) -> bool {
        matches!(
            self,
            Self::KeepAlive
                | Self::Energize
                | Self::NeutralControl
                | Self::Recovery
                | Self::UnclassifiedMutation
        )
    }

    #[inline]
    fn touches_controller_state(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }
}

/// Result of closing one I2C service's energizing lifecycle.
///
/// `no_controller_mutation_stage_in_flight` is a point-in-time software-stage
/// observation only. It excludes read-only work, and it is not evidence that a
/// physical rail is off. When false, a controller mutation was still executing
/// at the barrier and its outcome may be unknown; the hardware watchdog remains
/// the independent cutoff mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSafeOffTransition {
    generation: u64,
    no_controller_mutation_stage_in_flight: bool,
}

impl TerminalSafeOffTransition {
    /// Safety-generation observed after terminal mutation admission closed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether every controller-mutating stage had left its execution window
    /// when this receipt was issued.
    pub fn no_controller_mutation_stage_in_flight(&self) -> bool {
        self.no_controller_mutation_stage_in_flight
    }
}

/// Shared, clone-stable authorization for energy-affecting I2C work.
///
/// A safe-off barrier advances `generation`, invalidating already queued and
/// multi-stage energizing work. A terminal barrier first closes admission and
/// then advances the generation, so a racing admission is either rejected or
/// receives the old generation and is rejected at the next worker checkpoint.
#[derive(Debug, Default)]
struct Pic16ServiceAuthorityState {
    active_batch: Option<Arc<Pic16BatchAuthority>>,
    /// Monotonic service-lifetime history. A set bit means the exact address
    /// has entered PIC16 batch management and generic runtime authority may
    /// never resume in this service allocation, even after proven SafeOff.
    managed_addresses: [u64; 4],
}

impl Pic16ServiceAuthorityState {
    fn mark_managed(&mut self, addresses: &[u8]) {
        for &address in addresses {
            let word = usize::from(address / 64);
            let bit = u32::from(address % 64);
            self.managed_addresses[word] |= 1_u64 << bit;
        }
    }

    fn is_managed(&self, address: u8) -> bool {
        let word = usize::from(address / 64);
        let bit = u32::from(address % 64);
        self.managed_addresses[word] & (1_u64 << bit) != 0
    }

    fn has_managed_addresses(&self) -> bool {
        self.managed_addresses.iter().any(|word| *word != 0)
    }
}

fn managed_pic16_address_error(bus: u8, addr: u8) -> HalError {
    HalError::I2cSafetySuperseded {
        bus,
        addr,
        detail: "raw PIC16 access is permanently superseded: this address was adopted by a managed PIC16 batch in the current I2C service lifetime; batch release does not restore legacy authority"
            .into(),
    }
}

fn managed_pic16_fabric_error(bus: u8) -> HalError {
    HalError::I2cSafetySuperseded {
        bus,
        addr: 0,
        detail: "whole-fabric recovery is permanently superseded: this I2C service lifetime has adopted managed PIC16 endpoints"
            .into(),
    }
}

#[derive(Debug)]
struct I2cSafetyAuthority {
    /// Exact process-local fabric allocation assigned before service
    /// publication. Transport reopen must match both this identity and the
    /// retained weak authority; pointer identity alone is not an allocation
    /// protocol.
    fabric_allocation: AtomicU64,
    generation: AtomicU64,
    terminal_safe_off: AtomicBool,
    in_flight_controller_stages: AtomicUsize,
    worker_alive: AtomicBool,
    /// Set before a worker-owned PIC16 job enters the ordinary queue and
    /// cleared only after batch adoption or deterministic compensation.
    /// Atomic reservation ownership: zero is idle, a low-63-bit token is
    /// reserved, and that token with the high bit set is active. Encoding
    /// phase and owner together prevents stale queued requests from activating
    /// a newer caller's reservation.
    pic16_admission_owner: AtomicU64,
    pic16_admission_sequence: AtomicU64,
    /// Non-zero epoch of the one adopted PIC16 batch that remains
    /// authoritative until proven batch SafeOff.
    pic16_active_batch_epoch: AtomicU64,
    pic16_batch_sequence: AtomicU64,
    /// Active batch and monotonic managed-address history share one lock so
    /// worker-start gates cannot observe a half-published authority transfer.
    pic16_service_state: Mutex<Pic16ServiceAuthorityState>,
    /// PIC16 cleanup could not prove the physical rails safe. This is
    /// deliberately separate from the generic terminal lifecycle barrier.
    pic16_shutdown_unresolved: AtomicBool,
}

impl Default for I2cSafetyAuthority {
    fn default() -> Self {
        Self {
            fabric_allocation: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            terminal_safe_off: AtomicBool::new(false),
            in_flight_controller_stages: AtomicUsize::new(0),
            worker_alive: AtomicBool::new(true),
            pic16_admission_owner: AtomicU64::new(PIC16_ADMISSION_IDLE),
            pic16_admission_sequence: AtomicU64::new(0),
            pic16_active_batch_epoch: AtomicU64::new(0),
            pic16_batch_sequence: AtomicU64::new(0),
            pic16_service_state: Mutex::new(Pic16ServiceAuthorityState::default()),
            pic16_shutdown_unresolved: AtomicBool::new(false),
        }
    }
}

impl I2cSafetyAuthority {
    fn capture(&self, intent: I2cOperationIntent) -> std::result::Result<u64, &'static str> {
        loop {
            let before = self.generation.load(Ordering::SeqCst);
            if intent.requires_current_safety_generation()
                && (!self.worker_alive.load(Ordering::SeqCst)
                    || self.terminal_safe_off.load(Ordering::SeqCst))
            {
                return Err(if self.worker_alive.load(Ordering::SeqCst) {
                    "terminal safe-off is latched"
                } else {
                    "I2C service worker is no longer alive"
                });
            }
            let after = self.generation.load(Ordering::SeqCst);
            if before == after {
                return Ok(after);
            }
        }
    }

    fn advance_safe_off_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn latch_terminal_safe_off(&self) -> TerminalSafeOffTransition {
        let generation = if !self.terminal_safe_off.swap(true, Ordering::SeqCst) {
            self.advance_safe_off_generation()
        } else {
            self.generation.load(Ordering::SeqCst)
        };
        TerminalSafeOffTransition {
            generation,
            no_controller_mutation_stage_in_flight: self
                .in_flight_controller_stages
                .load(Ordering::SeqCst)
                == 0,
        }
    }

    fn mark_worker_dead(&self) {
        self.worker_alive.store(false, Ordering::SeqCst);
        let _ = self.latch_terminal_safe_off();
    }

    fn validate(&self, intent: I2cOperationIntent, generation: u64) -> bool {
        !intent.requires_current_safety_generation()
            || (self.worker_alive.load(Ordering::SeqCst)
                && !self.terminal_safe_off.load(Ordering::SeqCst)
                && self.generation.load(Ordering::SeqCst) == generation)
    }

    fn publish_pic16_batch(&self, bus: u8, addresses: Vec<u8>) -> Result<Arc<Pic16BatchAuthority>> {
        let address = addresses.first().copied().unwrap_or(0);
        let mut service_state = self
            .pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if service_state.active_batch.is_some() {
            return Err(HalError::I2cAdmissionBusy {
                bus,
                addr: address,
                detail: "a retained PIC16 batch already owns this service".into(),
            });
        }
        let previous = self
            .pic16_batch_sequence
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| HalError::I2cAdmissionBusy {
                bus,
                addr: address,
                detail: "PIC16 batch epoch space is exhausted".into(),
            })?;
        let epoch = previous + 1;
        self.pic16_active_batch_epoch
            .compare_exchange(0, epoch, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|active| HalError::I2cAdmissionBusy {
                bus,
                addr: address,
                detail: format!("PIC16 batch epoch {active} already owns this I2C service"),
            })?;
        let batch = Arc::new(Pic16BatchAuthority::new(epoch, addresses));
        service_state.mark_managed(batch.addresses());
        service_state.active_batch = Some(Arc::clone(&batch));
        Ok(batch)
    }

    fn active_pic16_batch(&self) -> Option<Arc<Pic16BatchAuthority>> {
        self.pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_batch
            .as_ref()
            .map(Arc::clone)
    }

    fn pic16_address_is_managed(&self, address: u8) -> bool {
        self.pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_managed(address)
    }

    fn pic16_has_managed_addresses(&self) -> bool {
        self.pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_managed_addresses()
    }

    fn pic16_batch_scope_owns_address(
        &self,
        epoch: u64,
        batch: &Arc<Pic16BatchAuthority>,
        address: u8,
    ) -> bool {
        if self.pic16_active_batch_epoch.load(Ordering::SeqCst) != epoch {
            return false;
        }
        self.pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_batch
            .as_ref()
            .is_some_and(|active| {
                Arc::ptr_eq(active, batch) && active.addresses().contains(&address)
            })
    }

    fn release_pic16_batch(&self, epoch: u64) -> std::result::Result<(), u64> {
        let mut service_state = self
            .pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if service_state
            .active_batch
            .as_ref()
            .is_none_or(|batch| batch.epoch() != epoch)
        {
            return Err(self.pic16_active_batch_epoch.load(Ordering::SeqCst));
        }
        self.pic16_active_batch_epoch.compare_exchange(
            epoch,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )?;
        service_state.active_batch.take();
        self.pic16_shutdown_unresolved
            .store(false, Ordering::SeqCst);
        Ok(())
    }

    fn mark_pic16_shutdown_unresolved(&self) {
        self.pic16_shutdown_unresolved.store(true, Ordering::SeqCst);
        let _ = self.latch_terminal_safe_off();
    }
}

/// Opaque identity and safety-generation evidence for one live I2C service.
///
/// Numerical generations are intentionally insufficient: a separately created
/// worker has its own authority allocation, so a token from an earlier worker
/// can never authorize work on the other service even when both report generation
/// zero. Creating that other service does not itself revoke a still-live old
/// service; the lifecycle owner must terminal-close the old authority first.
/// The weak reference prevents a token from extending a dead service lifetime.
/// HAL-owned protocol jobs may inspect old-service-local currentness for
/// diagnostics, but hardware authorization is revalidated by the service worker
/// at each semantic stage.
struct I2cServiceGenerationToken {
    bus: u8,
    generation: u64,
    authority: Weak<I2cSafetyAuthority>,
}

impl std::fmt::Debug for I2cServiceGenerationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("I2cServiceGenerationToken")
            .field("bus", &self.bus)
            .field("generation", &self.generation)
            .field("current", &self.is_current())
            .finish_non_exhaustive()
    }
}

impl I2cServiceGenerationToken {
    /// Point-in-time diagnostic only. Actual command admission is checked again
    /// inside the serialized worker against this exact authority allocation.
    fn is_current(&self) -> bool {
        self.authority.upgrade().is_some_and(|authority| {
            authority.validate(I2cOperationIntent::KeepAlive, self.generation)
        })
    }
}

struct I2cServiceWorkerLifetime {
    authority: Arc<I2cSafetyAuthority>,
    registry: I2cServiceRegistryLease,
}

impl I2cServiceWorkerLifetime {
    fn new(
        authority: Arc<I2cSafetyAuthority>,
        mut registry: I2cServiceRegistryLease,
    ) -> std::io::Result<Self> {
        if let Err(error) = registry.activate_for_worker() {
            registry.quarantine(I2cServiceQuarantineReason::RegistryInvariantLost);
            authority.mark_worker_dead();
            return Err(error);
        }
        Ok(Self {
            authority,
            registry,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum I2cFabricRegistryKey {
    /// One physical master resource. On S9, kernel `/dev/i2c-0` and the
    /// direct AXI-IIC MMIO window at `0x4160_0000` intentionally resolve to
    /// this same key.
    PhysicalFabric(PhysicalI2cFabricId),
    #[cfg(feature = "sim-hal")]
    SimulatedBus { bus: u8, backend: usize },
}

impl I2cFabricRegistryKey {
    const fn linux_adapter(bus: u8) -> Self {
        Self::PhysicalFabric(PhysicalI2cFabricId::linux_adapter(bus))
    }

    fn acquire_os_lease(
        self,
        purpose: I2cLeasePurpose,
    ) -> std::io::Result<Option<OsI2cFabricLease>> {
        match self {
            Self::PhysicalFabric(fabric) => OsI2cFabricLease::acquire(fabric, purpose)
                .map(Some)
                .map_err(|error| error.into_io_error()),
            #[cfg(feature = "sim-hal")]
            Self::SimulatedBus { .. } => Ok(None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum I2cServiceRegistryState {
    /// No driver/MMIO/GPIO side effect has happened. A failed thread spawn may
    /// still release this reservation as a proven-clean abort.
    PreparingClean,
    /// Preparation has started touching driver/controller state. Any abnormal
    /// exit must retain the registry entry and OS descriptor as quarantine.
    PreparingMutated,
    Active,
    Quarantined(I2cServiceQuarantineReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum I2cServiceQuarantineReason {
    PreparationAborted,
    WorkerPanicked,
    UnresolvedPic16State,
    UnexpectedLeaseDrop,
    RegistryInvariantLost,
}

enum I2cFabricRegistryEntry {
    RuntimeService {
        allocation: u64,
        authority: Weak<I2cSafetyAuthority>,
        state: I2cServiceRegistryState,
        /// The registry entry owns the OS descriptor so a quarantine
        /// tombstone remains exclusive until process exit.
        _os_lease: Option<OsI2cFabricLease>,
    },
    Raw {
        allocation: u64,
        kind: I2cRawLeaseKind,
        _os_lease: Option<OsI2cFabricLease>,
    },
}

fn i2c_fabric_registry() -> &'static Mutex<HashMap<I2cFabricRegistryKey, I2cFabricRegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<I2cFabricRegistryKey, I2cFabricRegistryEntry>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum I2cRawLeaseKind {
    Bootstrap,
    BitBang,
    #[cfg(feature = "recovery-tool")]
    Recovery,
}

impl I2cRawLeaseKind {
    const fn os_purpose(self) -> I2cLeasePurpose {
        match self {
            Self::Bootstrap => I2cLeasePurpose::RuntimeRaw,
            Self::BitBang => I2cLeasePurpose::RuntimeRaw,
            #[cfg(feature = "recovery-tool")]
            Self::Recovery => I2cLeasePurpose::RecoveryInspection,
        }
    }
}

pub(crate) struct I2cRawFabricLease {
    key: I2cFabricRegistryKey,
    allocation: u64,
    creator_pid: u32,
}

impl I2cRawFabricLease {
    fn reserve(key: I2cFabricRegistryKey, kind: I2cRawLeaseKind) -> std::io::Result<Self> {
        static NEXT_RAW_ALLOCATION: AtomicU64 = AtomicU64::new(0);
        let previous = NEXT_RAW_ALLOCATION
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| std::io::Error::other("raw I2C fabric allocation space is exhausted"))?;
        let allocation = previous + 1;
        {
            let registry = i2c_fabric_registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            refuse_existing_i2c_fabric_owner(&registry, key)?;
        }
        // Filesystem and flock operations intentionally happen without the
        // process-local registry mutex. The nonblocking OS lease serializes
        // physical contenders; simulated contenders are rejected by the
        // mandatory second local check below.
        let os_lease = key.acquire_os_lease(kind.os_purpose())?;
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = refuse_existing_i2c_fabric_owner(&registry, key) {
            drop(registry);
            drop(os_lease);
            return Err(error);
        }
        registry.insert(
            key,
            I2cFabricRegistryEntry::Raw {
                allocation,
                kind,
                _os_lease: os_lease,
            },
        );
        Ok(Self {
            key,
            allocation,
            creator_pid: std::process::id(),
        })
    }

    /// Reserve a canonical adapter alias before a GPIO/FPGA bit-banged master
    /// touches its pins. Ownership failure is typed as non-transport so callers
    /// cannot treat it as permission to fall back to another master.
    pub(crate) fn reserve_bitbang(fabric: PhysicalI2cFabricId) -> Result<Self> {
        Self::reserve(
            I2cFabricRegistryKey::PhysicalFabric(fabric),
            I2cRawLeaseKind::BitBang,
        )
        .map_err(|error| HalError::I2cFabricUnavailable {
            fabric,
            detail: format!("bit-banged I2C fabric ownership was refused: {error}"),
        })
    }

    /// Revalidate copied raw state before every wire/MMIO/GPIO operation.
    /// `O_CLOEXEC` handles exec, but a fork-only child shares the parent's
    /// open-file description and must never transact through inherited state.
    pub(crate) fn validate_current_process(&self) -> Result<()> {
        let fabric = match self.key {
            I2cFabricRegistryKey::PhysicalFabric(fabric) => fabric,
            #[cfg(feature = "sim-hal")]
            I2cFabricRegistryKey::SimulatedBus { bus, .. } => {
                PhysicalI2cFabricId::linux_adapter(bus)
            }
        };
        let current_pid = std::process::id();
        if current_pid != self.creator_pid {
            return Err(HalError::I2cFabricUnavailable {
                fabric,
                detail: format!(
                    "raw fabric lease belongs to process {}, not forked process {current_pid}",
                    self.creator_pid
                ),
            });
        }

        let registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match registry.get(&self.key) {
            Some(I2cFabricRegistryEntry::Raw {
                allocation,
                _os_lease,
                ..
            }) if *allocation == self.allocation => {
                if let Some(os_lease) = _os_lease {
                    os_lease.validate_current_process().map_err(|error| {
                        HalError::I2cFabricUnavailable {
                            fabric,
                            detail: error.to_string(),
                        }
                    })?;
                }
                Ok(())
            }
            _ => Err(HalError::I2cFabricUnavailable {
                fabric,
                detail: "raw fabric lease no longer owns its exact registry allocation".into(),
            }),
        }
    }
}

impl Drop for I2cRawFabricLease {
    fn drop(&mut self) {
        let removed = {
            let mut registry = i2c_fabric_registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owns_entry = registry.get(&self.key).is_some_and(|entry| {
                matches!(
                    entry,
                    I2cFabricRegistryEntry::Raw { allocation, .. }
                        if *allocation == self.allocation
                )
            });
            owns_entry.then(|| registry.remove(&self.key)).flatten()
        };
        // Closing the OS descriptor can run filesystem code. Keep it outside
        // the registry critical section so Drop cannot block or re-enter while
        // the process-local ownership mutex is held.
        drop(removed);
    }
}

struct I2cServiceRegistryLease {
    key: I2cFabricRegistryKey,
    allocation: u64,
    authority: Weak<I2cSafetyAuthority>,
    worker_finalized: bool,
}

impl I2cServiceRegistryLease {
    fn reserve(
        key: I2cFabricRegistryKey,
        authority: &Arc<I2cSafetyAuthority>,
    ) -> std::io::Result<Self> {
        static NEXT_SERVICE_ALLOCATION: AtomicU64 = AtomicU64::new(0);
        let allocation = NEXT_SERVICE_ALLOCATION
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| std::io::Error::other("I2C service allocation space is exhausted"))?
            + 1;
        {
            let registry = i2c_fabric_registry()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            refuse_existing_i2c_fabric_owner(&registry, key)?;
        }
        let os_lease = key.acquire_os_lease(I2cLeasePurpose::RuntimeService)?;
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = refuse_existing_i2c_fabric_owner(&registry, key) {
            drop(registry);
            drop(os_lease);
            return Err(error);
        }
        let weak = Arc::downgrade(authority);
        authority
            .fabric_allocation
            .store(allocation, Ordering::SeqCst);
        registry.insert(
            key,
            I2cFabricRegistryEntry::RuntimeService {
                allocation,
                authority: Weak::clone(&weak),
                state: I2cServiceRegistryState::PreparingClean,
                _os_lease: os_lease,
            },
        );
        Ok(Self {
            key,
            allocation,
            authority: weak,
            worker_finalized: false,
        })
    }

    /// Mark the point before platform preparation may alter driver, MMIO, or
    /// GPIO state. Any later abnormal Drop retains both local and OS ownership.
    fn mark_fabric_mutation_started(&mut self) -> std::io::Result<()> {
        self.transition_state(
            I2cServiceRegistryState::PreparingClean,
            I2cServiceRegistryState::PreparingMutated,
        )
    }

    fn activate_for_worker(&mut self) -> std::io::Result<()> {
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match registry.get_mut(&self.key) {
            Some(I2cFabricRegistryEntry::RuntimeService {
                allocation,
                authority,
                state,
                ..
            }) if *allocation == self.allocation && authority.ptr_eq(&self.authority) => {
                match state {
                    I2cServiceRegistryState::PreparingClean
                    | I2cServiceRegistryState::PreparingMutated => {
                        *state = I2cServiceRegistryState::Active;
                        Ok(())
                    }
                    _ => Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "I2C service {:?} cannot activate from state {state:?}",
                            self.key
                        ),
                    )),
                }
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("I2C service {:?} lost its preparing allocation", self.key),
            )),
        }
    }

    fn transition_state(
        &mut self,
        expected: I2cServiceRegistryState,
        replacement: I2cServiceRegistryState,
    ) -> std::io::Result<()> {
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match registry.get_mut(&self.key) {
            Some(I2cFabricRegistryEntry::RuntimeService {
                allocation,
                authority,
                state,
                ..
            }) if *allocation == self.allocation
                && authority.ptr_eq(&self.authority)
                && *state == expected =>
            {
                *state = replacement;
                Ok(())
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "I2C service {:?} could not transition {expected:?} -> {replacement:?}",
                    self.key
                ),
            )),
        }
    }

    fn quarantine(&mut self, reason: I2cServiceQuarantineReason) {
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(I2cFabricRegistryEntry::RuntimeService {
            allocation,
            authority,
            state,
            ..
        }) = registry.get_mut(&self.key)
        {
            if *allocation == self.allocation && authority.ptr_eq(&self.authority) {
                *state = I2cServiceRegistryState::Quarantined(reason);
            }
        }
    }

    fn validate_service_owner(
        key: I2cFabricRegistryKey,
        authority: &Arc<I2cSafetyAuthority>,
    ) -> std::io::Result<()> {
        let registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let allocation = authority.fabric_allocation.load(Ordering::SeqCst);
        let authority = Arc::downgrade(authority);
        match registry.get(&key) {
            Some(I2cFabricRegistryEntry::RuntimeService {
                allocation: registered_allocation,
                authority: registered,
                state: I2cServiceRegistryState::Active,
                _os_lease,
            }) if *registered_allocation == allocation
                && allocation != 0
                && registered.ptr_eq(&authority) =>
            {
                if let Some(os_lease) = _os_lease {
                    os_lease
                        .validate_current_process()
                        .map_err(|error| error.into_io_error())?;
                }
                Ok(())
            }
            Some(I2cFabricRegistryEntry::RuntimeService {
                state: I2cServiceRegistryState::Quarantined(reason),
                ..
            }) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{key:?} is quarantined: {reason:?}"),
            )),
            Some(I2cFabricRegistryEntry::RuntimeService {
                state:
                    I2cServiceRegistryState::PreparingClean | I2cServiceRegistryState::PreparingMutated,
                ..
            }) => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("{key:?} service preparation is not active"),
            )),
            Some(I2cFabricRegistryEntry::RuntimeService { .. }) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{key:?} belongs to a different I2C service allocation"),
            )),
            Some(I2cFabricRegistryEntry::Raw { kind, .. }) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{key:?} is owned by a {kind:?} raw handle"),
            )),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{key:?} has no runtime service reservation"),
            )),
        }
    }

    fn finish_worker(&mut self, authority: &I2cSafetyAuthority) {
        let unresolved = authority.pic16_shutdown_unresolved.load(Ordering::SeqCst)
            || authority.pic16_admission_owner.load(Ordering::SeqCst) != PIC16_ADMISSION_IDLE
            || authority.pic16_active_batch_epoch.load(Ordering::SeqCst) != 0;
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_entry = registry.get(&self.key).is_some_and(|registered| {
            matches!(
                registered,
                I2cFabricRegistryEntry::RuntimeService {
                    allocation,
                    authority: registered,
                    ..
                } if *allocation == self.allocation && registered.ptr_eq(&self.authority)
            )
        });
        let mut removed = None;
        if owns_entry {
            let quarantine_reason = if std::thread::panicking() {
                Some(I2cServiceQuarantineReason::WorkerPanicked)
            } else if unresolved {
                Some(I2cServiceQuarantineReason::UnresolvedPic16State)
            } else {
                None
            };
            if let Some(reason) = quarantine_reason {
                if let Some(I2cFabricRegistryEntry::RuntimeService { state, .. }) =
                    registry.get_mut(&self.key)
                {
                    *state = I2cServiceRegistryState::Quarantined(reason);
                }
            } else {
                removed = registry.remove(&self.key);
            }
        }

        // Reservation finalization and worker-death publication share the
        // registry critical section. A replacement can therefore observe
        // only the old live reservation, the final quarantine tombstone, or
        // the fully removed clean reservation -- never a dead interim owner.
        authority.mark_worker_dead();
        self.worker_finalized = true;
        drop(registry);
        // External acquisition cannot succeed until worker-death publication
        // above; only now may the removed entry close its OS descriptor.
        drop(removed);
    }
}

impl Drop for I2cServiceRegistryLease {
    fn drop(&mut self) {
        if self.worker_finalized {
            return;
        }
        let mut removed = None;
        let mut registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(I2cFabricRegistryEntry::RuntimeService {
            allocation,
            authority,
            state,
            ..
        }) = registry.get_mut(&self.key)
        {
            if *allocation == self.allocation && authority.ptr_eq(&self.authority) {
                match state {
                    I2cServiceRegistryState::PreparingClean => {
                        removed = registry.remove(&self.key);
                    }
                    I2cServiceRegistryState::PreparingMutated => {
                        *state = I2cServiceRegistryState::Quarantined(
                            I2cServiceQuarantineReason::PreparationAborted,
                        );
                    }
                    I2cServiceRegistryState::Active => {
                        *state = I2cServiceRegistryState::Quarantined(
                            I2cServiceQuarantineReason::UnexpectedLeaseDrop,
                        );
                    }
                    I2cServiceRegistryState::Quarantined(_) => {}
                }
            }
        }
        drop(registry);
        drop(removed);
    }
}

impl Drop for I2cServiceWorkerLifetime {
    fn drop(&mut self) {
        self.registry.finish_worker(&self.authority);
    }
}

fn run_reserved_i2c_preparation<F>(
    registry: &mut I2cServiceRegistryLease,
    prepare: F,
) -> std::io::Result<()>
where
    F: FnOnce() -> std::io::Result<()>,
{
    registry.mark_fabric_mutation_started()?;
    prepare()
}

fn refuse_existing_i2c_fabric_owner(
    registry: &HashMap<I2cFabricRegistryKey, I2cFabricRegistryEntry>,
    key: I2cFabricRegistryKey,
) -> std::io::Result<()> {
    let Some(entry) = registry.get(&key) else {
        return Ok(());
    };
    let detail = match entry {
        I2cFabricRegistryEntry::RuntimeService {
            state: I2cServiceRegistryState::Quarantined(reason),
            ..
        } => format!("{key:?} is quarantined after {reason:?}"),
        I2cFabricRegistryEntry::RuntimeService { state, .. } => {
            format!("an I2C service in state {state:?} already owns {key:?}")
        }
        I2cFabricRegistryEntry::Raw { kind, .. } => {
            format!("a {kind:?} raw I2C handle already owns {key:?}")
        }
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        detail,
    ))
}

struct Pic16RuntimeBatchSubmission {
    epoch: u64,
    batch: Arc<Pic16BatchAuthority>,
}

impl Pic16RuntimeBatchSubmission {
    fn from_admitted_batch(admitted: &Pic16AdmittedBatch) -> Self {
        Self {
            epoch: admitted.batch_epoch(),
            batch: admitted.batch_for_worker(),
        }
    }
}

#[derive(Clone)]
enum I2cPermitScope {
    Generic,
    /// Address-bound SafeOff accepted before PIC16 batch authority was
    /// published. It remains preemptive if publication races worker dispatch,
    /// but no new instance may be minted once the address is managed.
    Pic16PreManagementSafeOff {
        address: u8,
    },
    Pic16Admission {
        epoch: u64,
        batch: Arc<Pic16BatchAuthority>,
        reservation_token: u64,
    },
    Pic16RuntimeBatch {
        epoch: u64,
        batch: Arc<Pic16BatchAuthority>,
    },
    Pic16BatchSafeOff {
        epoch: u64,
        batch: Arc<Pic16BatchAuthority>,
    },
}

#[derive(Clone)]
struct I2cSafetyPermit {
    authority: Arc<I2cSafetyAuthority>,
    intent: I2cOperationIntent,
    generation: u64,
    scope: I2cPermitScope,
}

impl I2cSafetyPermit {
    fn scope_owns_address(&self, address: u8) -> bool {
        match &self.scope {
            I2cPermitScope::Generic => false,
            I2cPermitScope::Pic16PreManagementSafeOff { address: permitted } => {
                *permitted == address
            }
            I2cPermitScope::Pic16Admission { epoch, batch, .. }
            | I2cPermitScope::Pic16RuntimeBatch { epoch, batch }
            | I2cPermitScope::Pic16BatchSafeOff { epoch, batch } => self
                .authority
                .pic16_batch_scope_owns_address(*epoch, batch, address),
        }
    }

    fn scope_authorizes_address(&self, address: u8) -> bool {
        let managed = self.authority.pic16_address_is_managed(address);
        match &self.scope {
            I2cPermitScope::Generic => !managed,
            I2cPermitScope::Pic16PreManagementSafeOff { address: permitted } => {
                *permitted == address
            }
            I2cPermitScope::Pic16Admission { .. }
            | I2cPermitScope::Pic16RuntimeBatch { .. }
            | I2cPermitScope::Pic16BatchSafeOff { .. } => self.scope_owns_address(address),
        }
    }

    fn address_authorization_error(&self, bus: u8, address: u8) -> Option<HalError> {
        if self.scope_authorizes_address(address) {
            None
        } else if self.authority.pic16_address_is_managed(address)
            && matches!(&self.scope, I2cPermitScope::Generic)
        {
            Some(managed_pic16_address_error(bus, address))
        } else {
            Some(HalError::I2cSafetySuperseded {
                bus,
                addr: address,
                detail: "I2C permit scope does not own this controller address".into(),
            })
        }
    }

    fn scope_is_current(&self) -> bool {
        match &self.scope {
            I2cPermitScope::Generic | I2cPermitScope::Pic16PreManagementSafeOff { .. } => true,
            I2cPermitScope::Pic16Admission {
                epoch,
                batch,
                reservation_token,
            } => {
                let owner = self.authority.pic16_admission_owner.load(Ordering::SeqCst);
                (owner == *reservation_token
                    || owner == (*reservation_token | PIC16_ADMISSION_ACTIVE_BIT))
                    && self.authority.pic16_batch_scope_owns_address(
                        *epoch,
                        batch,
                        batch.addresses().first().copied().unwrap_or(0),
                    )
            }
            I2cPermitScope::Pic16RuntimeBatch { epoch, batch } => {
                batch.epoch() == *epoch
                    && batch.addresses().first().is_some_and(|address| {
                        self.authority
                            .pic16_batch_scope_owns_address(*epoch, batch, *address)
                    })
                    && batch.runtime_liveness_is_current()
            }
            I2cPermitScope::Pic16BatchSafeOff { epoch, batch } => {
                batch.addresses().first().is_some_and(|address| {
                    self.authority
                        .pic16_batch_scope_owns_address(*epoch, batch, *address)
                })
            }
        }
    }

    fn validate_admission(&self, bus: u8, addr: u8) -> Result<()> {
        if let Some(error) = self.address_authorization_error(bus, addr) {
            return Err(error);
        }
        if !self.authority.validate(self.intent, self.generation) || !self.scope_is_current() {
            return Err(HalError::I2cSafetySuperseded {
                bus,
                addr,
                detail: format!(
                    "{:?} request was superseded by a newer safe-off barrier before worker admission",
                    self.intent
                ),
            });
        }
        Ok(())
    }

    fn begin_stage(
        &self,
        bus: u8,
        addr: u8,
        stage: &'static str,
    ) -> Result<I2cControllerStageLease> {
        if self.intent.touches_controller_state() {
            self.authority
                .in_flight_controller_stages
                .fetch_add(1, Ordering::SeqCst);
        }
        if let Some(error) = self.address_authorization_error(bus, addr) {
            if self.intent.touches_controller_state() {
                self.authority
                    .in_flight_controller_stages
                    .fetch_sub(1, Ordering::SeqCst);
            }
            return Err(error);
        }
        if !self.authority.validate(self.intent, self.generation) || !self.scope_is_current() {
            if self.intent.touches_controller_state() {
                self.authority
                    .in_flight_controller_stages
                    .fetch_sub(1, Ordering::SeqCst);
            }
            return Err(HalError::I2cSafetySuperseded {
                bus,
                addr,
                detail: format!(
                    "{:?} request was superseded by a newer safe-off barrier before {stage}",
                    self.intent
                ),
            });
        }
        Ok(I2cControllerStageLease {
            authority: Arc::clone(&self.authority),
            counted: self.intent.touches_controller_state(),
        })
    }

    /// Count controller cleanup that is safe and necessary after a terminal
    /// barrier. Cleanup deliberately does not inherit the request generation:
    /// restoring the service timeout cannot energize hardware, and skipping it
    /// would leak protocol-specific timing into the following SafeOff plan.
    fn begin_terminal_safe_cleanup_stage(&self) -> I2cControllerStageLease {
        self.authority
            .in_flight_controller_stages
            .fetch_add(1, Ordering::SeqCst);
        I2cControllerStageLease {
            authority: Arc::clone(&self.authority),
            counted: true,
        }
    }
}

struct I2cControllerStageLease {
    authority: Arc<I2cSafetyAuthority>,
    counted: bool,
}

impl Drop for I2cControllerStageLease {
    fn drop(&mut self) {
        if self.counted {
            self.authority
                .in_flight_controller_stages
                .fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// One operation in an ordered I2C service transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cTransactionStep {
    /// Write bytes to the selected slave in one transaction.
    Write(Vec<u8>),
    /// Write bytes one-at-a-time with the PIC-safe delay pattern.
    WriteByteByByte(Vec<u8>),
    /// Read this many bytes from the selected slave.
    Read(usize),
    /// Read a framed response with a length byte in the fixed-size header.
    ///
    /// The service reads `header_len` bytes first, computes
    /// `remaining = header[len_index] + remaining_adjust`, then reads that many
    /// more bytes before returning the full frame as one read result. This lets
    /// APW121215a-style `write -> delay -> header -> variable tail` exchanges
    /// stay atomic in the service queue.
    ReadFrame {
        header_len: usize,
        len_index: usize,
        remaining_adjust: i16,
        max_len: usize,
    },
    /// Combined write+read using I2C_RDWR when the backend supports it.
    WriteRead {
        write_data: Vec<u8>,
        read_len: usize,
    },
    /// Sleep inside the service worker before the next step.
    SleepMs(u64),
    /// Set the kernel I2C timeout if the backend exposes one.
    SetTimeout(u32),
}

/// Conservative per-message ceiling for every public/service I2C operation.
/// Current miner protocols use at most hundreds of bytes; bounding at 4 KiB
/// prevents silent `u16` truncation and worker-side attacker-sized allocation
/// while leaving ample room for EEPROM/firmware pages.
const I2C_MAX_MESSAGE_BYTES: usize = 4 * 1024;

/// Maximum number of worker operations in one atomic transaction plan.
///
/// Message sizes and sleeps are bounded separately, but even zero-duration
/// sleeps and timeout changes consume worker time. Keeping the step count
/// finite makes queue-latency bounds independent of caller-controlled `Vec`
/// length. Existing hardware protocols use far fewer than 64 steps.
const I2C_MAX_TRANSACTION_STEPS: usize = 64;

fn validate_message_len(bus: u8, addr: u8, operation: &str, len: usize) -> Result<()> {
    if len > I2C_MAX_MESSAGE_BYTES {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: format!(
                "{operation} length {len} exceeds the {I2C_MAX_MESSAGE_BYTES}-byte service limit"
            ),
        });
    }
    Ok(())
}

fn require_exact_i2c_write(
    bus: u8,
    addr: u8,
    operation: &str,
    expected: usize,
    written: usize,
) -> Result<()> {
    if written == expected {
        return Ok(());
    }
    Err(HalError::I2c {
        bus,
        addr,
        detail: format!(
            "{operation} completed a short write: expected {expected} byte(s), wrote {written}"
        ),
    })
}

fn validate_transaction_message_lengths(
    bus: u8,
    addr: u8,
    steps: &[I2cTransactionStep],
) -> Result<()> {
    if steps.len() > I2C_MAX_TRANSACTION_STEPS {
        return Err(HalError::I2c {
            bus,
            addr,
            detail: format!(
                "transaction has {} steps, exceeding the {}-step service limit",
                steps.len(),
                I2C_MAX_TRANSACTION_STEPS
            ),
        });
    }
    for step in steps {
        match step {
            I2cTransactionStep::Write(data) => {
                validate_message_len(bus, addr, "transaction write", data.len())?
            }
            I2cTransactionStep::WriteByteByByte(data) => {
                validate_message_len(bus, addr, "transaction bytewise write", data.len())?
            }
            I2cTransactionStep::Read(len) => {
                validate_message_len(bus, addr, "transaction read", *len)?
            }
            I2cTransactionStep::ReadFrame {
                header_len,
                max_len,
                ..
            } => {
                validate_message_len(bus, addr, "transaction frame header", *header_len)?;
                validate_message_len(bus, addr, "transaction frame maximum", *max_len)?;
            }
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                validate_message_len(bus, addr, "transaction write-read write", write_data.len())?;
                validate_message_len(bus, addr, "transaction write-read read", *read_len)?;
            }
            I2cTransactionStep::SleepMs(_) | I2cTransactionStep::SetTimeout(_) => {}
        }
    }
    Ok(())
}

/// I2C request types for the serialized service.
#[derive(Debug)]
pub(crate) enum I2cRequest {
    /// Send a heartbeat to a PIC at the given address.
    Heartbeat {
        addr: u8,
        firmware: I2cPicFirmware,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Set voltage DAC on a PIC.
    SetVoltage {
        addr: u8,
        firmware: I2cPicFirmware,
        pic_val: u8,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Program a PIC16 DAC without enabling the rail. This request is only
    /// constructed from an adopted admission batch; ordinary callers use the
    /// compatibility SET+ENABLE request above until they migrate.
    Pic16SetVoltageOnly {
        addr: u8,
        pic_val: u8,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Disable voltage output on a PIC.
    DisableVoltage {
        addr: u8,
        firmware: I2cPicFirmware,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Set voltage in millivolts on a dsPIC33EP (S19 Pro / S17 style).
    SetVoltageMv {
        addr: u8,
        voltage_mv: u16,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Read one PIC16 raw-state byte and send the fixed JUMP frame only when
    /// that same worker observation is exact bootloader state `0xCC`.
    Pic16JumpIfBootloader {
        addr: u8,
        reply_tx: mpsc::SyncSender<Result<I2cPic16JumpOutcome>>,
    },
    /// Install a worker-owned, generation-bound PIC16 cold-boot batch.
    /// Completion is delivered separately because the start acknowledgement
    /// must not block the caller or the worker while scheduled steps advance.
    Pic16Admission {
        plans: Vec<Pic16AdmissionPlan>,
        batch: Arc<Pic16BatchAuthority>,
        cancellation: Arc<AtomicBool>,
        reservation: Pic16AdmissionReservation,
        completion_tx: mpsc::SyncSender<Pic16AdmissionDelivery>,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Send one fixed heartbeat to every endpoint in the exact retained batch.
    /// The worker advances one complete frame per scheduler turn.
    Pic16HeartbeatRound {
        batch: Arc<Pic16BatchAuthority>,
        reply_tx: mpsc::SyncSender<Result<Pic16HeartbeatRoundOutcome>>,
    },
    // --- v0.13.0: Generic I2C operations for init ---
    // Route ALL I2C through the service thread (one fd for lifetime, like BraiinsOS).
    /// Write bytes to an I2C slave (single transaction).
    WriteBytes {
        addr: u8,
        data: Vec<u8>,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Write bytes one-at-a-time (byte-by-byte pattern for PIC init).
    WriteByteByte {
        addr: u8,
        data: Vec<u8>,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Read bytes from an I2C slave.
    ReadBytes {
        addr: u8,
        len: usize,
        reply_tx: mpsc::SyncSender<Result<Vec<u8>>>,
    },
    /// Read the fixed offset-zero identity prefix from one hashboard AT24.
    /// Platform topology resolves the address; no caller-controlled pointer,
    /// payload, or length reaches the worker, and worker policy must admit the
    /// resolved address as a protected endpoint before any I/O.
    ReadHashboardEepromPrefix {
        addr: u8,
        reply_tx: mpsc::SyncSender<Result<Vec<u8>>>,
    },
    /// Read exactly the signed two-byte LM75 temperature register through one
    /// repeated-start transfer. Platform topology supplies the endpoint; no
    /// caller-controlled pointer, payload, or length reaches the worker.
    ReadLm75TemperatureRegister {
        addr: u8,
        reply_tx: mpsc::SyncSender<Result<Lm75TemperatureRegister>>,
    },
    /// Combined write+read (I2C_RDWR with repeated START).
    WriteRead {
        addr: u8,
        write_data: Vec<u8>,
        read_len: usize,
        reply_tx: mpsc::SyncSender<Result<Vec<u8>>>,
    },
    /// Set I2C timeout (units of 10ms jiffies).
    SetTimeout {
        timeout_jiffies: u32,
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Perform controller-level bus recovery only while the entire fabric is
    /// still unmanaged. Once any PIC16 batch has been adopted, even a released
    /// batch permanently fences this service-wide operation.
    RecoverUnmanagedBus {
        reply_tx: mpsc::SyncSender<Result<()>>,
    },
    /// Ordered compound transaction executed as one service-worker request.
    Transaction {
        addr: u8,
        steps: Vec<I2cTransactionStep>,
        reply_tx: mpsc::SyncSender<Result<Vec<Vec<u8>>>>,
    },
    /// Worker-owned conditional SafeOff plan. Production service handles use
    /// the reserved mailbox; this request form preserves the same atomic
    /// semantics for service implementations without that lane and test seams.
    ConditionalSafeOffPlan {
        addr: u8,
        prelude: Vec<I2cTransactionStep>,
        primary: Vec<I2cTransactionStep>,
        compensation: Vec<I2cTransactionStep>,
        reply_tx: mpsc::SyncSender<Result<I2cConditionalSafeOffOutcome>>,
    },
}

/// Handle for sending I2C requests to the service thread.
#[derive(Clone)]
pub struct I2cServiceHandle {
    bus: u8,
    tx: I2cServiceSender,
    safety: Arc<I2cSafetyAuthority>,
    safe_off_mailbox: Option<Arc<I2cSafeOffMailbox>>,
}

/// Tokio-facing facade for the serialized I2C service.
///
/// The synchronous handle remains the correct API for dedicated OS threads.
/// Async lifecycle code should use this facade so a service wait cannot block
/// the Tokio executor. A pre-dispatch cancellation gate also prevents a closure
/// queued in Tokio's bounded blocking pool from submitting stale hardware work
/// after its async caller has already gone away.
#[derive(Clone)]
pub struct AsyncI2cServiceHandle {
    inner: I2cServiceHandle,
}

#[derive(Clone)]
enum I2cServiceSender {
    Deadline(mpsc::SyncSender<I2cServiceEnvelope>),
    #[cfg(test)]
    Raw(mpsc::SyncSender<I2cRequest>),
}

struct I2cServiceEnvelope {
    /// A queued command must never touch hardware after this instant. This is
    /// distinct from the reply deadline: caller timeout does not revoke a
    /// started request, while a newer safe-off generation still revokes it at
    /// its next semantic controller-stage boundary.
    must_start_by: Instant,
    state: Arc<AtomicU8>,
    permit: I2cSafetyPermit,
    request: I2cRequest,
}

struct PendingI2cSafeOff {
    addr: u8,
    operation: I2cSafeOffOperation,
    permit: I2cSafetyPermit,
    waiters: Vec<I2cSafeOffWaiter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum I2cSafeOffKey {
    Pic16BatchDisable {
        epoch: u64,
        addresses: Vec<u8>,
    },
    PicDisable {
        addr: u8,
        firmware: I2cPicFirmware,
    },
    WriteBytes {
        addr: u8,
        data: Vec<u8>,
    },
    WriteByteByByte {
        addr: u8,
        data: Vec<u8>,
    },
    Transaction {
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    },
    ConditionalPlan {
        addr: u8,
        prelude: Vec<I2cTransactionStep>,
        primary: Vec<I2cTransactionStep>,
        compensation: Vec<I2cTransactionStep>,
    },
}

enum I2cSafeOffOperation {
    Pic16BatchDisable {
        epoch: u64,
        addresses: Vec<u8>,
        batch: Arc<Pic16BatchAuthority>,
    },
    PicDisable {
        firmware: I2cPicFirmware,
    },
    WriteBytes {
        data: Vec<u8>,
    },
    WriteByteByByte {
        data: Vec<u8>,
    },
    Transaction {
        steps: Vec<I2cTransactionStep>,
    },
    ConditionalPlan {
        prelude: Vec<I2cTransactionStep>,
        primary: Vec<I2cTransactionStep>,
        compensation: Vec<I2cTransactionStep>,
    },
}

enum I2cSafeOffWaiter {
    Unit(mpsc::SyncSender<Result<()>>),
    Conditional(mpsc::SyncSender<Result<I2cConditionalSafeOffOutcome>>),
    Pic16Batch(mpsc::SyncSender<Result<Pic16BatchSafeOffOutcome>>),
}

enum I2cSafeOffExecution {
    Unit(Result<()>),
    Conditional {
        outcome: I2cConditionalSafeOffOutcome,
        transport_fault: bool,
    },
    Pic16Batch {
        outcome: Pic16BatchSafeOffOutcome,
        transport_fault: bool,
    },
}

impl I2cSafeOffExecution {
    fn requires_transport_recovery(&self) -> bool {
        match self {
            Self::Unit(result) => i2c_result_requires_transport_recovery(result),
            Self::Conditional {
                transport_fault, ..
            }
            | Self::Pic16Batch {
                transport_fault, ..
            } => *transport_fault,
        }
    }

    fn merge_after_recovery(self, retry: Self) -> Self {
        match (self, retry) {
            (
                Self::Pic16Batch { outcome: first, .. },
                Self::Pic16Batch {
                    outcome: second,
                    transport_fault,
                },
            ) if first.batch_epoch() == second.batch_epoch() => {
                let endpoints = first
                    .endpoints()
                    .iter()
                    .zip(second.endpoints())
                    .map(|(earlier, later)| {
                        if matches!(earlier.status(), Pic16CompensationStatus::Disabled)
                            || matches!(later.status(), Pic16CompensationStatus::Disabled)
                        {
                            Pic16CompensationOutcome::new(
                                earlier.address(),
                                Pic16CompensationStatus::Disabled,
                            )
                        } else {
                            later.clone()
                        }
                    })
                    .collect();
                Self::Pic16Batch {
                    outcome: Pic16BatchSafeOffOutcome::disabled(second.batch_epoch(), endpoints),
                    transport_fault,
                }
            }
            (_, retry) => retry,
        }
    }
}

/// Worker-observed result of one phase in a conditional SafeOff plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cSafeOffPhaseOutcome {
    NotAttempted,
    Completed,
    Failed(String),
}

impl I2cSafeOffPhaseOutcome {
    pub fn completed(&self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// Result of an indivisible, worker-owned SafeOff plan.
///
/// The worker always attempts `primary`, even if `prelude` fails. If primary
/// fails it attempts `compensation`; if primary succeeds after a failed
/// prelude it retries the prelude so the final state still converges safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I2cConditionalSafeOffOutcome {
    pub prelude: I2cSafeOffPhaseOutcome,
    pub primary: I2cSafeOffPhaseOutcome,
    pub compensation: I2cSafeOffPhaseOutcome,
    pub prelude_retry: I2cSafeOffPhaseOutcome,
}

#[derive(Default)]
struct I2cSafeOffMailbox {
    // A bounded VecDeque intentionally preserves first-admission order across
    // distinct operations. Duplicate keys coalesce in place and therefore do
    // not jump ahead of an operation that was admitted earlier.
    pending: Mutex<VecDeque<(I2cSafeOffKey, PendingI2cSafeOff)>>,
    /// Worker lifecycle is synchronized with `pending`: enqueue checks this
    /// while holding the queue lock, and close changes it while holding that
    /// same lock, so no command can be inserted behind a completed drain.
    worker_state: AtomicU8,
}

const I2C_SAFE_OFF_WORKER_ACCEPTING: u8 = 0;
const I2C_SAFE_OFF_WORKER_CLOSING: u8 = 1;
const I2C_SAFE_OFF_WORKER_CLOSED: u8 = 2;

const I2C_SAFE_OFF_ENDPOINT_CAPACITY: usize = 64;
const I2C_SAFE_OFF_WAITER_CAPACITY: usize = 16;
const I2C_SAFE_OFF_RECEIPT_BUDGET: Duration = Duration::from_secs(3);
const I2C_SAFE_OFF_POLL_INTERVAL: Duration = Duration::from_millis(5);

fn wait_for_reserved_safe_off_receipt(
    bus: u8,
    addr: u8,
    reply_rx: mpsc::Receiver<Result<()>>,
    operation: &'static str,
) -> Result<()> {
    wait_for_reserved_safe_off_receipt_with_budget(
        bus,
        addr,
        reply_rx,
        operation,
        I2C_SAFE_OFF_RECEIPT_BUDGET,
    )
}

fn wait_for_reserved_safe_off_receipt_with_budget(
    bus: u8,
    addr: u8,
    reply_rx: mpsc::Receiver<Result<()>>,
    operation: &'static str,
    budget: Duration,
) -> Result<()> {
    match reply_rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(HalError::I2cSafeOffOutcomeUnknown {
            bus,
            addr,
            detail: format!(
                "{operation} did not complete within {}ms; it remains queued independently of this caller and physical rail state is unknown",
                budget.as_millis()
            ),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(HalError::I2cSafeOffOutcomeUnknown {
            bus,
            addr,
            detail: format!("{operation} receipt was dropped; hardware outcome is unknown"),
        }),
    }
}

fn wait_for_pic16_batch_safe_off_receipt(
    bus: u8,
    addr: u8,
    reply_rx: mpsc::Receiver<Result<Pic16BatchSafeOffOutcome>>,
    budget: Duration,
) -> Result<Pic16BatchSafeOffOutcome> {
    match reply_rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(HalError::I2cSafeOffOutcomeUnknown {
            bus,
            addr,
            detail: "PIC16 batch SafeOff remains worker-owned after the receipt deadline; batch epoch remains active and rail state is unknown"
                .into(),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(HalError::I2cSafeOffOutcomeUnknown {
            bus,
            addr,
            detail: "PIC16 batch SafeOff receipt was dropped; batch epoch remains active and rail state is unknown"
                .into(),
        }),
    }
}

impl I2cSafeOffMailbox {
    fn lock_pending(
        &self,
    ) -> std::sync::MutexGuard<'_, VecDeque<(I2cSafeOffKey, PendingI2cSafeOff)>> {
        match self.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => {
                tracing::error!(
                    "SafeOff mailbox mutex was poisoned; recovering accepted hardware-safe work"
                );
                poisoned.into_inner()
            }
        }
    }

    fn enqueue_disable(
        &self,
        bus: u8,
        addr: u8,
        firmware: I2cPicFirmware,
        permit: I2cSafetyPermit,
        reply_tx: mpsc::SyncSender<Result<()>>,
    ) -> Result<()> {
        self.enqueue(
            bus,
            addr,
            I2cSafeOffKey::PicDisable { addr, firmware },
            I2cSafeOffOperation::PicDisable { firmware },
            permit,
            I2cSafeOffWaiter::Unit(reply_tx),
        )
    }

    // Keep the batch identity, permit, ownership guard, and receipt channel explicit: this is
    // the atomic boundary where caller-owned shutdown responsibility becomes worker-owned.
    #[allow(clippy::too_many_arguments)]
    fn enqueue_pic16_batch_disable(
        &self,
        bus: u8,
        epoch: u64,
        addresses: Vec<u8>,
        batch: Arc<Pic16BatchAuthority>,
        permit: I2cSafetyPermit,
        attempt: &mut Pic16BatchSafeOffAttempt<'_>,
        reply_tx: mpsc::SyncSender<Result<Pic16BatchSafeOffOutcome>>,
    ) -> Result<()> {
        let addr = addresses.first().copied().unwrap_or(0);
        self.enqueue_with_acceptance(
            bus,
            addr,
            I2cSafeOffKey::Pic16BatchDisable {
                epoch,
                addresses: addresses.clone(),
            },
            I2cSafeOffOperation::Pic16BatchDisable {
                epoch,
                addresses,
                batch,
            },
            permit,
            I2cSafeOffWaiter::Pic16Batch(reply_tx),
            || attempt.handoff_to_worker(),
        )
    }

    fn enqueue_write(
        &self,
        bus: u8,
        addr: u8,
        data: Vec<u8>,
        permit: I2cSafetyPermit,
        reply_tx: mpsc::SyncSender<Result<()>>,
    ) -> Result<()> {
        self.enqueue(
            bus,
            addr,
            I2cSafeOffKey::WriteBytes {
                addr,
                data: data.clone(),
            },
            I2cSafeOffOperation::WriteBytes { data },
            permit,
            I2cSafeOffWaiter::Unit(reply_tx),
        )
    }

    fn enqueue_bytewise_write(
        &self,
        bus: u8,
        addr: u8,
        data: Vec<u8>,
        permit: I2cSafetyPermit,
        reply_tx: mpsc::SyncSender<Result<()>>,
    ) -> Result<()> {
        self.enqueue(
            bus,
            addr,
            I2cSafeOffKey::WriteByteByByte {
                addr,
                data: data.clone(),
            },
            I2cSafeOffOperation::WriteByteByByte { data },
            permit,
            I2cSafeOffWaiter::Unit(reply_tx),
        )
    }

    fn enqueue_transaction(
        &self,
        bus: u8,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
        permit: I2cSafetyPermit,
        reply_tx: mpsc::SyncSender<Result<()>>,
    ) -> Result<()> {
        self.enqueue(
            bus,
            addr,
            I2cSafeOffKey::Transaction {
                addr,
                steps: steps.clone(),
            },
            I2cSafeOffOperation::Transaction { steps },
            permit,
            I2cSafeOffWaiter::Unit(reply_tx),
        )
    }

    fn enqueue_conditional_plan(
        &self,
        bus: u8,
        addr: u8,
        prelude: Vec<I2cTransactionStep>,
        primary: Vec<I2cTransactionStep>,
        compensation: Vec<I2cTransactionStep>,
        permit: I2cSafetyPermit,
        reply_tx: mpsc::SyncSender<Result<I2cConditionalSafeOffOutcome>>,
    ) -> Result<()> {
        self.enqueue(
            bus,
            addr,
            I2cSafeOffKey::ConditionalPlan {
                addr,
                prelude: prelude.clone(),
                primary: primary.clone(),
                compensation: compensation.clone(),
            },
            I2cSafeOffOperation::ConditionalPlan {
                prelude,
                primary,
                compensation,
            },
            permit,
            I2cSafeOffWaiter::Conditional(reply_tx),
        )
    }

    fn enqueue(
        &self,
        bus: u8,
        addr: u8,
        key: I2cSafeOffKey,
        operation: I2cSafeOffOperation,
        permit: I2cSafetyPermit,
        waiter: I2cSafeOffWaiter,
    ) -> Result<()> {
        self.enqueue_with_acceptance(bus, addr, key, operation, permit, waiter, || {})
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_with_acceptance(
        &self,
        bus: u8,
        addr: u8,
        key: I2cSafeOffKey,
        operation: I2cSafeOffOperation,
        permit: I2cSafetyPermit,
        waiter: I2cSafeOffWaiter,
        accepted: impl FnOnce(),
    ) -> Result<()> {
        let mut pending = self.lock_pending();
        if self.worker_state.load(Ordering::SeqCst) != I2C_SAFE_OFF_WORKER_ACCEPTING {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: "safe-off worker is closing or closed; command was not accepted and hardware watchdog cutoff is required".into(),
            });
        }
        if let Some((_, existing)) = pending
            .iter_mut()
            .find(|(pending_key, _)| pending_key == &key)
        {
            if existing.waiters.len() >= I2C_SAFE_OFF_WAITER_CAPACITY {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: format!(
                        "safe-off remains pending, but its receipt capacity of {} waiters is exhausted",
                        I2C_SAFE_OFF_WAITER_CAPACITY
                    ),
                });
            }
            existing.waiters.push(waiter);
            accepted();
            return Ok(());
        }
        if pending.len() >= I2C_SAFE_OFF_ENDPOINT_CAPACITY {
            return Err(HalError::I2c {
                bus,
                addr,
                detail: format!(
                    "safe-off mailbox endpoint capacity {} is exhausted; energizing work remains fenced and hardware watchdog cutoff is required",
                    I2C_SAFE_OFF_ENDPOINT_CAPACITY
                ),
            });
        }
        pending.push_back((
            key,
            PendingI2cSafeOff {
                addr,
                operation,
                permit,
                waiters: vec![waiter],
            },
        ));
        accepted();
        Ok(())
    }

    fn take_next(&self) -> Option<PendingI2cSafeOff> {
        self.lock_pending().pop_front().map(|(_, pending)| pending)
    }

    /// Atomically stop admission. Accepted commands remain in the FIFO so an
    /// unwind guard can still fail anything the worker has not yet removed.
    fn begin_close(&self) {
        let _pending = self.lock_pending();
        self.worker_state
            .store(I2C_SAFE_OFF_WORKER_CLOSING, Ordering::SeqCst);
    }

    fn mark_closed(&self) {
        self.worker_state
            .store(I2C_SAFE_OFF_WORKER_CLOSED, Ordering::SeqCst);
    }

    fn fail_pending_on_worker_exit(&self, bus: u8, detail: &str) {
        self.begin_close();
        while let Some(operation) = self.take_next() {
            let execution = operation.not_executed_on_worker_exit(bus, detail);
            operation.complete(bus, execution);
        }
        self.mark_closed();
    }

    #[cfg(test)]
    fn pending_endpoint_count(&self) -> usize {
        self.lock_pending().len()
    }

    #[cfg(test)]
    fn pending_waiter_count(&self, addr: u8) -> usize {
        self.lock_pending()
            .iter()
            .map(|(_, entry)| entry)
            .filter(|entry| entry.addr == addr)
            .map(|entry| entry.waiters.len())
            .sum()
    }
}

fn active_pic16_batch_shutdown(authority: &Arc<I2cSafetyAuthority>) -> Option<PendingI2cSafeOff> {
    let batch = authority.active_pic16_batch()?;
    if batch.released() {
        return None;
    }
    let epoch = batch.epoch();
    if authority.pic16_active_batch_epoch.load(Ordering::SeqCst) != epoch {
        authority.mark_pic16_shutdown_unresolved();
        return None;
    }
    if !batch.claim_worker_safe_off_attempt() {
        match batch.wait_for_safe_off_attempt(Duration::ZERO) {
            Pic16BatchSafeOffOwnership::WorkerOwned => return None,
            Pic16BatchSafeOffOwnership::CallerClaimed => {
                authority.mark_pic16_shutdown_unresolved();
                return None;
            }
            Pic16BatchSafeOffOwnership::Idle => {
                authority.mark_pic16_shutdown_unresolved();
                return None;
            }
        }
    }
    let addresses = batch.addresses().iter().copied().rev().collect::<Vec<_>>();
    let addr = addresses.first().copied().unwrap_or(0);
    let generation = authority.advance_safe_off_generation();
    Some(PendingI2cSafeOff {
        addr,
        operation: I2cSafeOffOperation::Pic16BatchDisable {
            epoch,
            addresses,
            batch: Arc::clone(&batch),
        },
        permit: I2cSafetyPermit {
            authority: Arc::clone(authority),
            intent: I2cOperationIntent::SafeOff,
            generation,
            scope: I2cPermitScope::Pic16BatchSafeOff { epoch, batch },
        },
        waiters: Vec::new(),
    })
}

struct I2cSafeOffWorkerLifecycle {
    mailbox: Arc<I2cSafeOffMailbox>,
    bus: u8,
    closed: bool,
}

impl I2cSafeOffWorkerLifecycle {
    fn new(mailbox: Arc<I2cSafeOffMailbox>, bus: u8) -> Self {
        Self {
            mailbox,
            bus,
            closed: false,
        }
    }

    fn finish(&mut self) {
        self.mailbox.mark_closed();
        self.closed = true;
    }
}

impl Drop for I2cSafeOffWorkerLifecycle {
    fn drop(&mut self) {
        if !self.closed {
            self.mailbox.fail_pending_on_worker_exit(
                self.bus,
                "I2C service worker exited before accepted SafeOff work executed; hardware watchdog cutoff is required",
            );
        }
    }
}

#[cfg(feature = "sim-hal")]
fn execute_pending_safe_off_with_unwind_boundary(
    pending: PendingI2cSafeOff,
    bus: u8,
    i2c: &mut I2cBus,
) {
    match catch_pending_safe_off_execution(&pending, i2c) {
        Ok(first) => {
            let execution = if first.requires_transport_recovery() {
                let _ = i2c.bus_recovery();
                match catch_pending_safe_off_execution(&pending, i2c) {
                    Ok(second) => first.merge_after_recovery(second),
                    Err(payload) => {
                        let execution = pending.not_executed_on_worker_exit(
                            bus,
                            "I2C service worker panicked during the bounded SafeOff retry; hardware outcome is unknown and watchdog cutoff is required",
                        );
                        pending.complete(bus, execution);
                        std::panic::resume_unwind(payload);
                    }
                }
            } else {
                first
            };
            pending.complete(bus, execution);
        }
        Err(payload) => {
            let execution = pending.not_executed_on_worker_exit(
                bus,
                "I2C service worker panicked while executing accepted SafeOff work; hardware outcome is unknown and watchdog cutoff is required",
            );
            pending.complete(bus, execution);
            std::panic::resume_unwind(payload);
        }
    }
}

fn catch_pending_safe_off_execution(
    pending: &PendingI2cSafeOff,
    i2c: &mut I2cBus,
) -> std::thread::Result<I2cSafeOffExecution> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pending.execute(i2c)))
}

#[allow(clippy::too_many_arguments)]
fn execute_pending_safe_off_with_recovery(
    pending: PendingI2cSafeOff,
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: &[u8],
    i2c_bus: &mut Option<I2cBus>,
    last_reset_time: &mut Instant,
    consecutive_resets: &mut u32,
    authority: &Arc<I2cSafetyAuthority>,
) {
    let first = match i2c_bus
        .as_mut()
        .map(|i2c| catch_pending_safe_off_execution(&pending, i2c))
    {
        Some(Ok(execution)) => execution,
        Some(Err(payload)) => {
            let execution = pending.not_executed_on_worker_exit(
                bus,
                "I2C service worker panicked while executing accepted SafeOff work; hardware outcome is unknown and watchdog cutoff is required",
            );
            pending.complete(bus, execution);
            std::panic::resume_unwind(payload);
        }
        None => {
            let execution = pending.bus_unavailable_execution(bus);
            pending.complete(bus, execution);
            return;
        }
    };

    if !first.requires_transport_recovery() {
        pending.complete(bus, first);
        return;
    }

    recover_i2c_backend(
        bus,
        use_devmem,
        restore_kernel_registers,
        i2c_bus,
        last_reset_time,
        consecutive_resets,
        write_denylist,
    );
    if i2c_bus.is_none() {
        *i2c_bus = reopen_i2c_service_bus(
            bus,
            use_devmem,
            restore_kernel_registers,
            write_denylist,
            authority,
        );
    }
    let retry_execution = match i2c_bus
        .as_mut()
        .map(|i2c| catch_pending_safe_off_execution(&pending, i2c))
    {
        Some(Ok(execution)) => execution,
        Some(Err(payload)) => {
            let execution = pending.not_executed_on_worker_exit(
                bus,
                "I2C service worker panicked during the bounded SafeOff retry; hardware outcome is unknown and watchdog cutoff is required",
            );
            pending.complete(bus, execution);
            std::panic::resume_unwind(payload);
        }
        None => pending.bus_unavailable_execution(bus),
    };
    pending.complete(bus, first.merge_after_recovery(retry_execution));
}

impl PendingI2cSafeOff {
    fn reconciled_pic16_batch_release(&self) -> Option<I2cSafeOffExecution> {
        let I2cSafeOffOperation::Pic16BatchDisable { epoch, batch, .. } = &self.operation else {
            return None;
        };
        (batch.released()
            && self
                .permit
                .authority
                .pic16_active_batch_epoch
                .load(Ordering::SeqCst)
                == 0)
            .then(|| I2cSafeOffExecution::Pic16Batch {
                outcome: Pic16BatchSafeOffOutcome::already_released(*epoch),
                transport_fault: false,
            })
    }

    fn execute(&self, i2c: &mut I2cBus) -> I2cSafeOffExecution {
        if let Some(reconciled) = self.reconciled_pic16_batch_release() {
            return reconciled;
        }
        match &self.operation {
            I2cSafeOffOperation::Pic16BatchDisable {
                epoch, addresses, ..
            } => {
                let mut transport_fault = false;
                let endpoints = addresses
                    .iter()
                    .copied()
                    .map(|address| {
                        let status = match execute_disable_voltage(
                            i2c,
                            address,
                            I2cPicFirmware::Unknown,
                            &self.permit,
                        ) {
                            Ok(()) => Pic16CompensationStatus::Disabled,
                            Err(error) => {
                                transport_fault |= matches!(&error, HalError::I2c { .. });
                                Pic16CompensationStatus::OutcomeUnknown {
                                    detail: error.to_string(),
                                }
                            }
                        };
                        Pic16CompensationOutcome::new(address, status)
                    })
                    .collect();
                I2cSafeOffExecution::Pic16Batch {
                    outcome: Pic16BatchSafeOffOutcome::disabled(*epoch, endpoints),
                    transport_fault,
                }
            }
            I2cSafeOffOperation::PicDisable { firmware } => I2cSafeOffExecution::Unit(
                execute_disable_voltage(i2c, self.addr, *firmware, &self.permit),
            ),
            I2cSafeOffOperation::WriteBytes { data } => {
                let result = self
                    .permit
                    .begin_stage(i2c.bus, self.addr, "reserved safe-off write")
                    .and_then(|_stage| {
                        i2c.set_slave(self.addr)?;
                        i2c.write(data).map(|_| ())
                    });
                I2cSafeOffExecution::Unit(result)
            }
            I2cSafeOffOperation::WriteByteByByte { data } => {
                let result = self
                    .permit
                    .begin_stage(i2c.bus, self.addr, "reserved bytewise safe-off write")
                    .and_then(|_stage| {
                        i2c.set_slave(self.addr)?;
                        i2c.write_byte_by_byte(data)
                    });
                I2cSafeOffExecution::Unit(result)
            }
            I2cSafeOffOperation::Transaction { steps } => I2cSafeOffExecution::Unit(
                execute_transaction(i2c, self.addr, steps.clone(), &self.permit).map(|_| ()),
            ),
            I2cSafeOffOperation::ConditionalPlan {
                prelude,
                primary,
                compensation,
            } => {
                let (outcome, transport_fault) = execute_conditional_safe_off_plan(
                    i2c,
                    self.addr,
                    prelude,
                    primary,
                    compensation,
                    &self.permit,
                );
                I2cSafeOffExecution::Conditional {
                    outcome,
                    transport_fault,
                }
            }
        }
    }

    fn complete(self, bus: u8, execution: I2cSafeOffExecution) {
        match execution {
            I2cSafeOffExecution::Unit(result) => {
                for waiter in self.waiters {
                    let I2cSafeOffWaiter::Unit(waiter) = waiter else {
                        tracing::error!(bus, addr = self.addr, "SafeOff waiter type mismatch");
                        continue;
                    };
                    let reply = match &result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(clone_safe_off_completion_error(error, bus, self.addr)),
                    };
                    let _ = waiter.send(reply);
                }
            }
            I2cSafeOffExecution::Conditional { outcome, .. } => {
                for waiter in self.waiters {
                    let I2cSafeOffWaiter::Conditional(waiter) = waiter else {
                        tracing::error!(
                            bus,
                            addr = self.addr,
                            "conditional SafeOff waiter type mismatch"
                        );
                        continue;
                    };
                    let _ = waiter.send(Ok(outcome.clone()));
                }
            }
            I2cSafeOffExecution::Pic16Batch { outcome, .. } => {
                let (epoch, batch) = match &self.operation {
                    I2cSafeOffOperation::Pic16BatchDisable { epoch, batch, .. } => {
                        (*epoch, Arc::clone(batch))
                    }
                    _ => {
                        tracing::error!(
                            bus,
                            addr = self.addr,
                            "PIC16 batch execution had a mismatched operation"
                        );
                        return;
                    }
                };
                let release_error = match outcome.disposition() {
                    Pic16BatchSafeOffDisposition::AlreadyReleased => None,
                    Pic16BatchSafeOffDisposition::Executed if outcome.all_disabled() => {
                        match self.permit.authority.release_pic16_batch(epoch) {
                            Ok(_) => {
                                batch.mark_released();
                                None
                            }
                            Err(observed) => {
                                self.permit.authority.mark_pic16_shutdown_unresolved();
                                Some(format!(
                                    "PIC16 rails were disabled, but epoch {epoch} could not release from registry state {observed}"
                                ))
                            }
                        }
                    }
                    Pic16BatchSafeOffDisposition::Executed => {
                        self.permit.authority.mark_pic16_shutdown_unresolved();
                        None
                    }
                };
                // Completion, not a caller receipt deadline, is the sole
                // finalizer for an in-flight epoch attempt. Wake retries only
                // after release or unresolved state is durably published.
                batch.finish_safe_off_attempt();
                for waiter in self.waiters {
                    let I2cSafeOffWaiter::Pic16Batch(waiter) = waiter else {
                        tracing::error!(
                            bus,
                            addr = self.addr,
                            "PIC16 batch SafeOff waiter type mismatch"
                        );
                        continue;
                    };
                    let reply = release_error.as_ref().map_or_else(
                        || Ok(outcome.clone()),
                        |detail| {
                            Err(HalError::I2cSafeOffOutcomeUnknown {
                                bus,
                                addr: self.addr,
                                detail: detail.clone(),
                            })
                        },
                    );
                    let _ = waiter.send(reply);
                }
            }
        }
    }

    fn bus_unavailable_execution(&self, bus: u8) -> I2cSafeOffExecution {
        if let Some(reconciled) = self.reconciled_pic16_batch_release() {
            return reconciled;
        }
        let detail = "bus reopen failed while executing reserved safe-off";
        match &self.operation {
            I2cSafeOffOperation::ConditionalPlan { .. } => I2cSafeOffExecution::Conditional {
                outcome: I2cConditionalSafeOffOutcome {
                    prelude: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    primary: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    compensation: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    prelude_retry: I2cSafeOffPhaseOutcome::NotAttempted,
                },
                transport_fault: true,
            },
            I2cSafeOffOperation::Pic16BatchDisable {
                epoch, addresses, ..
            } => I2cSafeOffExecution::Pic16Batch {
                outcome: unknown_pic16_batch_safe_off(*epoch, addresses, detail),
                transport_fault: true,
            },
            _ => I2cSafeOffExecution::Unit(Err(HalError::I2c {
                bus,
                addr: self.addr,
                detail: detail.into(),
            })),
        }
    }

    fn not_executed_on_worker_exit(&self, bus: u8, detail: &str) -> I2cSafeOffExecution {
        if let Some(reconciled) = self.reconciled_pic16_batch_release() {
            return reconciled;
        }
        match &self.operation {
            I2cSafeOffOperation::ConditionalPlan { .. } => I2cSafeOffExecution::Conditional {
                outcome: I2cConditionalSafeOffOutcome {
                    prelude: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    primary: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    compensation: I2cSafeOffPhaseOutcome::Failed(detail.into()),
                    prelude_retry: I2cSafeOffPhaseOutcome::NotAttempted,
                },
                transport_fault: false,
            },
            I2cSafeOffOperation::Pic16BatchDisable {
                epoch, addresses, ..
            } => I2cSafeOffExecution::Pic16Batch {
                outcome: unknown_pic16_batch_safe_off(*epoch, addresses, detail),
                transport_fault: false,
            },
            _ => I2cSafeOffExecution::Unit(Err(HalError::I2c {
                bus,
                addr: self.addr,
                detail: detail.into(),
            })),
        }
    }
}

fn unknown_pic16_batch_safe_off(
    epoch: u64,
    addresses: &[u8],
    detail: &str,
) -> Pic16BatchSafeOffOutcome {
    Pic16BatchSafeOffOutcome::disabled(
        epoch,
        addresses
            .iter()
            .copied()
            .map(|address| {
                Pic16CompensationOutcome::new(
                    address,
                    Pic16CompensationStatus::OutcomeUnknown {
                        detail: detail.into(),
                    },
                )
            })
            .collect(),
    )
}

fn clone_safe_off_completion_error(error: &HalError, bus: u8, addr: u8) -> HalError {
    match error {
        HalError::I2c { detail, .. } => HalError::I2c {
            bus,
            addr,
            detail: format!("coalesced safe-off command failed: {detail}"),
        },
        HalError::I2cSafetySuperseded { detail, .. } => HalError::I2cSafetySuperseded {
            bus,
            addr,
            detail: detail.clone(),
        },
        HalError::I2cSafeOffOutcomeUnknown { detail, .. } => HalError::I2cSafeOffOutcomeUnknown {
            bus,
            addr,
            detail: detail.clone(),
        },
        HalError::PsuProtocol(detail) => HalError::PsuProtocol(*detail),
        HalError::PsuProtocolOwned(detail) => HalError::PsuProtocolOwned(detail.clone()),
        HalError::PsuUnsupported(detail) => HalError::PsuUnsupported(detail.clone()),
        other => HalError::I2c {
            bus,
            addr,
            detail: format!("coalesced safe-off command failed: {other}"),
        },
    }
}

fn phase_outcome(result: Result<Vec<Vec<u8>>>) -> (I2cSafeOffPhaseOutcome, bool) {
    let transport_fault = i2c_result_requires_transport_recovery(&result);
    let outcome = match result {
        Ok(_) => I2cSafeOffPhaseOutcome::Completed,
        Err(error) => I2cSafeOffPhaseOutcome::Failed(error.to_string()),
    };
    (outcome, transport_fault)
}

fn execute_conditional_safe_off_plan(
    i2c: &mut I2cBus,
    addr: u8,
    prelude: &[I2cTransactionStep],
    primary: &[I2cTransactionStep],
    compensation: &[I2cTransactionStep],
    permit: &I2cSafetyPermit,
) -> (I2cConditionalSafeOffOutcome, bool) {
    let (prelude_outcome, prelude_transport_fault) =
        phase_outcome(execute_transaction(i2c, addr, prelude.to_vec(), permit));
    let (primary_outcome, primary_transport_fault) =
        phase_outcome(execute_transaction(i2c, addr, primary.to_vec(), permit));
    let mut compensation_outcome = I2cSafeOffPhaseOutcome::NotAttempted;
    let mut prelude_retry_outcome = I2cSafeOffPhaseOutcome::NotAttempted;
    let mut branch_transport_fault = false;

    if primary_outcome.completed() {
        if !prelude_outcome.completed() {
            (prelude_retry_outcome, branch_transport_fault) =
                phase_outcome(execute_transaction(i2c, addr, prelude.to_vec(), permit));
        }
    } else {
        (compensation_outcome, branch_transport_fault) = phase_outcome(execute_transaction(
            i2c,
            addr,
            compensation.to_vec(),
            permit,
        ));
    }

    let unresolved = !primary_outcome.completed()
        || (!prelude_outcome.completed() && !prelude_retry_outcome.completed());
    (
        I2cConditionalSafeOffOutcome {
            prelude: prelude_outcome,
            primary: primary_outcome,
            compensation: compensation_outcome,
            prelude_retry: prelude_retry_outcome,
        },
        unresolved
            && (prelude_transport_fault || primary_transport_fault || branch_transport_fault),
    )
}

const I2C_REQUEST_QUEUED: u8 = 0;
const I2C_REQUEST_STARTED: u8 = 1;
const I2C_REQUEST_FINISHED: u8 = 2;
const I2C_REQUEST_CANCELLED: u8 = 3;

const I2C_QUEUE_ADMISSION_BUDGET: Duration = Duration::from_millis(100);
const I2C_QUEUE_START_BUDGET: Duration = Duration::from_secs(1);
const I2C_DEFAULT_KERNEL_TIMEOUT: Duration = Duration::from_millis(100);
const I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES: u32 = 10;
const I2C_EXECUTION_HEADROOM: Duration = Duration::from_secs(1);
const I2C_MAX_EXECUTION_BUDGET: Duration = Duration::from_secs(60);
const I2C_ASYNC_DISPATCH_BUDGET: Duration = Duration::from_secs(1);

const I2C_ASYNC_WAITING: u8 = 0;
const I2C_ASYNC_STARTED: u8 = 1;
const I2C_ASYNC_FINISHED: u8 = 2;
const I2C_ASYNC_CANCELLED: u8 = 3;

struct CancelAsyncI2cBeforeDispatch {
    state: Arc<AtomicU8>,
    addr: u8,
    operation: &'static str,
    armed: bool,
}

impl Drop for CancelAsyncI2cBeforeDispatch {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.state.compare_exchange(
            I2C_ASYNC_WAITING,
            I2C_ASYNC_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(I2C_ASYNC_STARTED) => tracing::warn!(
                addr = format_args!("0x{:02X}", self.addr),
                operation = self.operation,
                "async I2C caller was cancelled after execution started; the operation continues and its hardware outcome is unobserved"
            ),
            Err(_) => {}
        }
    }
}

#[derive(Clone, Copy)]
struct I2cRequestBudget {
    admission: Duration,
    start: Duration,
    execution: Duration,
}

impl I2cRequestBudget {
    fn for_request(request: &I2cRequest) -> Option<Self> {
        let execution = request_execution_budget(request);
        (execution <= I2C_MAX_EXECUTION_BUDGET).then_some(Self {
            admission: I2C_QUEUE_ADMISSION_BUDGET,
            start: I2C_QUEUE_START_BUDGET,
            execution,
        })
    }

    /// Bound queue-start plus reply wait by one caller-owned wall-clock
    /// deadline. Admission happens inside the start window, so it is clamped
    /// to the same bound rather than added a second time.
    fn for_request_until(request: &I2cRequest, deadline: Instant) -> Option<Self> {
        let default = Self::for_request(request)?;
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let default_total = default.start.saturating_add(default.execution);
        if remaining >= default_total {
            return Some(default);
        }

        // Leave time for both queue start and the fixed read. A request with
        // no time for both is refused before admission and cannot touch wire.
        let start = default.start.min(remaining / 2);
        let execution = remaining.saturating_sub(start);
        (!start.is_zero() && !execution.is_zero()).then_some(Self {
            admission: default.admission.min(start),
            start,
            execution,
        })
    }
}

fn duration_mul(duration: Duration, count: usize) -> Duration {
    duration
        .checked_mul(u32::try_from(count).unwrap_or(u32::MAX))
        .unwrap_or(Duration::MAX)
}

fn pic16_heartbeat_round_byte_operations(endpoint_count: usize) -> usize {
    endpoint_count.checked_sub(1).map_or(0, |completed| {
        19usize.saturating_add(3usize.saturating_mul(completed))
    })
}

fn pic16_heartbeat_round_execution_budget(endpoint_count: usize) -> Duration {
    let byte_op = I2C_DEFAULT_KERNEL_TIMEOUT + Duration::from_millis(1);
    duration_mul(
        byte_op,
        pic16_heartbeat_round_byte_operations(endpoint_count),
    )
    .saturating_add(I2C_EXECUTION_HEADROOM)
}

fn request_execution_budget(request: &I2cRequest) -> Duration {
    let byte_op = I2C_DEFAULT_KERNEL_TIMEOUT + Duration::from_millis(1);
    let budget = match request {
        // These PIC operations may flush 16 bytes after a NACK. Account for
        // every byte-level transaction, not just the successful command path.
        I2cRequest::Heartbeat { .. } => duration_mul(byte_op, 19),
        I2cRequest::SetVoltage { .. } => duration_mul(byte_op, 24),
        I2cRequest::Pic16SetVoltageOnly { .. } => duration_mul(byte_op, 20),
        I2cRequest::DisableVoltage { .. } => duration_mul(byte_op, 20),
        I2cRequest::SetVoltageMv { .. } => duration_mul(byte_op, 21),
        // One exact raw read, a three-byte JUMP, and the mandatory 16-byte
        // parser flush if the transition write fails.
        I2cRequest::Pic16JumpIfBootloader { .. } => {
            duration_mul(byte_op, 20).saturating_add(Duration::from_millis(500))
        }
        // This request only transfers ownership into the worker. The job has
        // its own bounded wait and cancel-on-drop completion channel.
        I2cRequest::Pic16Admission { .. } => Duration::from_millis(10),
        I2cRequest::Pic16HeartbeatRound { batch, .. } => {
            return pic16_heartbeat_round_execution_budget(batch.addresses().len());
        }
        I2cRequest::WriteBytes { .. }
        | I2cRequest::ReadBytes { .. }
        | I2cRequest::ReadHashboardEepromPrefix { .. }
        | I2cRequest::ReadLm75TemperatureRegister { .. }
        | I2cRequest::WriteRead { .. } => I2C_DEFAULT_KERNEL_TIMEOUT,
        I2cRequest::WriteByteByte { data, .. } => duration_mul(byte_op, data.len()),
        I2cRequest::SetTimeout { .. } => Duration::from_millis(250),
        I2cRequest::RecoverUnmanagedBus { .. } => duration_mul(byte_op, 9),
        I2cRequest::Transaction { steps, .. } => transaction_execution_budget(steps),
        I2cRequest::ConditionalSafeOffPlan {
            prelude,
            primary,
            compensation,
            ..
        } => transaction_execution_budget(prelude)
            .saturating_mul(2)
            .saturating_add(transaction_execution_budget(primary))
            .saturating_add(transaction_execution_budget(compensation)),
    };
    budget.saturating_add(I2C_EXECUTION_HEADROOM)
}

fn transaction_execution_budget(steps: &[I2cTransactionStep]) -> Duration {
    let mut budget = Duration::ZERO;
    let mut kernel_timeout = I2C_DEFAULT_KERNEL_TIMEOUT;

    for step in steps {
        let step_budget = match step {
            I2cTransactionStep::Write(_) | I2cTransactionStep::Read(_) => kernel_timeout,
            I2cTransactionStep::WriteByteByByte(data) => {
                duration_mul(kernel_timeout + Duration::from_millis(1), data.len())
            }
            // ReadFrame performs one header read and, for a non-empty tail, a
            // second read. Budget both because the tail length is data-driven.
            I2cTransactionStep::ReadFrame { .. } => duration_mul(kernel_timeout, 2),
            I2cTransactionStep::WriteRead { .. } => kernel_timeout,
            I2cTransactionStep::SleepMs(ms) => Duration::from_millis(*ms),
            I2cTransactionStep::SetTimeout(timeout_jiffies) => {
                kernel_timeout = Duration::from_millis(u64::from(*timeout_jiffies) * 10);
                Duration::from_millis(10)
            }
        };
        budget = budget.saturating_add(step_budget);
    }
    budget
}

fn request_addr(request: &I2cRequest) -> u8 {
    match request {
        I2cRequest::Heartbeat { addr, .. }
        | I2cRequest::SetVoltage { addr, .. }
        | I2cRequest::Pic16SetVoltageOnly { addr, .. }
        | I2cRequest::DisableVoltage { addr, .. }
        | I2cRequest::SetVoltageMv { addr, .. }
        | I2cRequest::Pic16JumpIfBootloader { addr, .. }
        | I2cRequest::WriteBytes { addr, .. }
        | I2cRequest::WriteByteByte { addr, .. }
        | I2cRequest::ReadBytes { addr, .. }
        | I2cRequest::ReadHashboardEepromPrefix { addr, .. }
        | I2cRequest::ReadLm75TemperatureRegister { addr, .. }
        | I2cRequest::WriteRead { addr, .. }
        | I2cRequest::Transaction { addr, .. }
        | I2cRequest::ConditionalSafeOffPlan { addr, .. } => *addr,
        I2cRequest::Pic16Admission { plans, .. } => {
            plans.first().map_or(0, Pic16AdmissionPlan::address)
        }
        I2cRequest::Pic16HeartbeatRound { batch, .. } => {
            batch.addresses().first().copied().unwrap_or(0)
        }
        I2cRequest::SetTimeout { .. } | I2cRequest::RecoverUnmanagedBus { .. } => 0,
    }
}

fn reply_i2c_request_error(request: I2cRequest, bus: u8, detail: &str) {
    macro_rules! reply {
        ($reply_tx:expr, $addr:expr) => {{
            let _ = $reply_tx.send(Err(HalError::I2c {
                bus,
                addr: $addr,
                detail: detail.into(),
            }));
        }};
    }

    match request {
        I2cRequest::Heartbeat { reply_tx, addr, .. }
        | I2cRequest::SetVoltage { reply_tx, addr, .. }
        | I2cRequest::Pic16SetVoltageOnly { reply_tx, addr, .. }
        | I2cRequest::DisableVoltage { reply_tx, addr, .. } => reply!(reply_tx, addr),
        I2cRequest::SetVoltageMv { reply_tx, addr, .. }
        | I2cRequest::WriteBytes { reply_tx, addr, .. }
        | I2cRequest::WriteByteByte { reply_tx, addr, .. } => reply!(reply_tx, addr),
        I2cRequest::Pic16JumpIfBootloader { reply_tx, addr } => reply!(reply_tx, addr),
        I2cRequest::Pic16Admission {
            reply_tx, plans, ..
        } => reply!(
            reply_tx,
            plans.first().map_or(0, Pic16AdmissionPlan::address)
        ),
        I2cRequest::Pic16HeartbeatRound { batch, reply_tx } => {
            reply!(reply_tx, batch.addresses().first().copied().unwrap_or(0))
        }
        I2cRequest::ReadBytes { reply_tx, addr, .. }
        | I2cRequest::ReadHashboardEepromPrefix { reply_tx, addr }
        | I2cRequest::WriteRead { reply_tx, addr, .. } => reply!(reply_tx, addr),
        I2cRequest::ReadLm75TemperatureRegister { reply_tx, addr } => {
            reply!(reply_tx, addr)
        }
        I2cRequest::SetTimeout { reply_tx, .. } | I2cRequest::RecoverUnmanagedBus { reply_tx } => {
            reply!(reply_tx, 0)
        }
        I2cRequest::Transaction { reply_tx, addr, .. } => reply!(reply_tx, addr),
        I2cRequest::ConditionalSafeOffPlan { reply_tx, addr, .. } => reply!(reply_tx, addr),
    }
}

fn reply_i2c_request_failure(request: I2cRequest, error: HalError) {
    macro_rules! reply {
        ($reply_tx:expr) => {{
            let _ = $reply_tx.send(Err(error));
        }};
    }

    match request {
        I2cRequest::Heartbeat { reply_tx, .. }
        | I2cRequest::SetVoltage { reply_tx, .. }
        | I2cRequest::Pic16SetVoltageOnly { reply_tx, .. }
        | I2cRequest::DisableVoltage { reply_tx, .. }
        | I2cRequest::SetVoltageMv { reply_tx, .. }
        | I2cRequest::WriteBytes { reply_tx, .. }
        | I2cRequest::WriteByteByte { reply_tx, .. }
        | I2cRequest::SetTimeout { reply_tx, .. }
        | I2cRequest::RecoverUnmanagedBus { reply_tx } => reply!(reply_tx),
        I2cRequest::Pic16JumpIfBootloader { reply_tx, .. } => reply!(reply_tx),
        I2cRequest::Pic16Admission { reply_tx, .. } => reply!(reply_tx),
        I2cRequest::Pic16HeartbeatRound { reply_tx, .. } => reply!(reply_tx),
        I2cRequest::ReadBytes { reply_tx, .. }
        | I2cRequest::ReadHashboardEepromPrefix { reply_tx, .. }
        | I2cRequest::WriteRead { reply_tx, .. } => {
            reply!(reply_tx)
        }
        I2cRequest::ReadLm75TemperatureRegister { reply_tx, .. } => reply!(reply_tx),
        I2cRequest::Transaction { reply_tx, .. } => reply!(reply_tx),
        I2cRequest::ConditionalSafeOffPlan { reply_tx, .. } => reply!(reply_tx),
    }
}

fn reject_envelope_during_pic16_job(envelope: I2cServiceEnvelope, bus: u8) {
    let Some((request, state, _permit)) = start_envelope_at(envelope, bus, Instant::now()) else {
        return;
    };
    reply_i2c_request_busy(request, bus);
    state.store(I2C_REQUEST_FINISHED, Ordering::Release);
}

fn reply_i2c_request_busy(request: I2cRequest, bus: u8) {
    macro_rules! busy {
        ($reply_tx:expr, $addr:expr) => {{
            let _ = $reply_tx.send(Err(hal_busy_error(bus, $addr)));
        }};
    }
    match request {
        I2cRequest::Heartbeat { reply_tx, addr, .. }
        | I2cRequest::SetVoltage { reply_tx, addr, .. }
        | I2cRequest::Pic16SetVoltageOnly { reply_tx, addr, .. }
        | I2cRequest::DisableVoltage { reply_tx, addr, .. } => busy!(reply_tx, addr),
        I2cRequest::SetVoltageMv { reply_tx, addr, .. }
        | I2cRequest::WriteBytes { reply_tx, addr, .. }
        | I2cRequest::WriteByteByte { reply_tx, addr, .. } => busy!(reply_tx, addr),
        I2cRequest::Pic16JumpIfBootloader { reply_tx, addr } => busy!(reply_tx, addr),
        I2cRequest::Pic16Admission {
            reply_tx, plans, ..
        } => busy!(
            reply_tx,
            plans.first().map_or(0, Pic16AdmissionPlan::address)
        ),
        I2cRequest::Pic16HeartbeatRound { batch, reply_tx } => {
            busy!(reply_tx, batch.addresses().first().copied().unwrap_or(0))
        }
        I2cRequest::ReadBytes { reply_tx, addr, .. }
        | I2cRequest::ReadHashboardEepromPrefix { reply_tx, addr }
        | I2cRequest::WriteRead { reply_tx, addr, .. } => busy!(reply_tx, addr),
        I2cRequest::ReadLm75TemperatureRegister { reply_tx, addr } => busy!(reply_tx, addr),
        I2cRequest::SetTimeout { reply_tx, .. } | I2cRequest::RecoverUnmanagedBus { reply_tx } => {
            busy!(reply_tx, 0)
        }
        I2cRequest::Transaction { reply_tx, addr, .. } => busy!(reply_tx, addr),
        I2cRequest::ConditionalSafeOffPlan { reply_tx, addr, .. } => busy!(reply_tx, addr),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pic16RequestConflict {
    AdmissionOwned,
    AddressUnauthorized,
    FabricRecoveryUnauthorized,
}

fn pic16_request_conflict(
    request: &I2cRequest,
    permit: &I2cSafetyPermit,
) -> Option<Pic16RequestConflict> {
    let address = request_addr(request);
    if !permit.scope_authorizes_address(address) {
        return Some(Pic16RequestConflict::AddressUnauthorized);
    }
    if matches!(request, I2cRequest::RecoverUnmanagedBus { .. })
        && permit.authority.pic16_has_managed_addresses()
    {
        return Some(Pic16RequestConflict::FabricRecoveryUnauthorized);
    }
    let admission_owned = permit.intent != I2cOperationIntent::SafeOff
        && !matches!(&permit.scope, I2cPermitScope::Pic16Admission { .. })
        && permit
            .authority
            .pic16_admission_owner
            .load(Ordering::SeqCst)
            != PIC16_ADMISSION_IDLE;
    admission_owned.then_some(Pic16RequestConflict::AdmissionOwned)
}

#[cfg(test)]
fn request_conflicts_with_pic16_authority(request: &I2cRequest, permit: &I2cSafetyPermit) -> bool {
    pic16_request_conflict(request, permit).is_some()
}

fn reject_pic16_request_conflict(
    request: I2cRequest,
    permit: &I2cSafetyPermit,
    conflict: Pic16RequestConflict,
    bus: u8,
) {
    match conflict {
        Pic16RequestConflict::AdmissionOwned => reply_i2c_request_busy(request, bus),
        Pic16RequestConflict::AddressUnauthorized => {
            let addr = request_addr(&request);
            let error = permit
                .address_authorization_error(bus, addr)
                .expect("worker conflict must retain its address error");
            reply_i2c_request_failure(request, error);
        }
        Pic16RequestConflict::FabricRecoveryUnauthorized => {
            reply_i2c_request_failure(request, managed_pic16_fabric_error(bus));
        }
    }
}

fn start_envelope_at(
    envelope: I2cServiceEnvelope,
    bus: u8,
    now: Instant,
) -> Option<(I2cRequest, Arc<AtomicU8>, I2cSafetyPermit)> {
    let I2cServiceEnvelope {
        must_start_by,
        state,
        permit,
        request,
    } = envelope;
    if now >= must_start_by {
        let _ = state.compare_exchange(
            I2C_REQUEST_QUEUED,
            I2C_REQUEST_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        reply_i2c_request_error(request, bus, "I2C request expired before execution");
        return None;
    }
    if let Err(error) = permit.validate_admission(bus, request_addr(&request)) {
        let _ = state.compare_exchange(
            I2C_REQUEST_QUEUED,
            I2C_REQUEST_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        reply_i2c_request_failure(request, error);
        return None;
    }
    match state.compare_exchange(
        I2C_REQUEST_QUEUED,
        I2C_REQUEST_STARTED,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Some((request, state, permit)),
        Err(I2C_REQUEST_CANCELLED) => {
            reply_i2c_request_error(request, bus, "I2C request was cancelled before execution");
            None
        }
        Err(other) => {
            tracing::error!(
                bus,
                request_state = other,
                "I2C service rejected an envelope with an invalid lifecycle state"
            );
            reply_i2c_request_error(request, bus, "invalid I2C request lifecycle state");
            None
        }
    }
}

enum I2cSubmissionAuthority<'a> {
    Current,
    Generation(&'a I2cServiceGenerationToken),
    Pic16Admission {
        token: &'a I2cServiceGenerationToken,
        epoch: u64,
        batch: Arc<Pic16BatchAuthority>,
        reservation_token: u64,
    },
    Pic16RuntimeBatch {
        token: &'a I2cServiceGenerationToken,
        epoch: u64,
        batch: Arc<Pic16BatchAuthority>,
    },
}

impl I2cServiceHandle {
    /// I2C bus owned by this serialized service lifetime.
    ///
    /// Exposed so opaque endpoint capabilities can be checked against the
    /// transport before a protocol service is constructed.
    pub fn bus(&self) -> u8 {
        self.bus
    }

    /// Capture opaque service-lifetime and safety-generation evidence for a
    /// HAL-owned multi-stage protocol.
    ///
    /// The returned token is deliberately non-cloneable and cannot be
    /// constructed outside this module. A safe-off barrier, terminal latch,
    /// or worker exit invalidates it. A different service rejects the token by
    /// allocation identity even if its numerical generation is equal.
    fn capture_generation_token(&self) -> Result<I2cServiceGenerationToken> {
        let generation = self
            .safety
            .capture(I2cOperationIntent::KeepAlive)
            .map_err(|detail| HalError::I2c {
                bus: self.bus,
                addr: 0,
                detail: format!("I2C service generation was not captured: {detail}"),
            })?;
        Ok(I2cServiceGenerationToken {
            bus: self.bus,
            generation,
            authority: Arc::downgrade(&self.safety),
        })
    }

    pub fn async_handle(&self) -> AsyncI2cServiceHandle {
        AsyncI2cServiceHandle {
            inner: self.clone(),
        }
    }

    /// Irreversibly close this service lifetime to controller-mutating work
    /// other than `SafeOff`. All handle clones observe the same barrier.
    pub fn latch_terminal_safe_off(&self) -> TerminalSafeOffTransition {
        self.safety.latch_terminal_safe_off()
    }

    pub fn terminal_safe_off_is_latched(&self) -> bool {
        self.safety.terminal_safe_off.load(Ordering::SeqCst)
    }

    pub(crate) fn has_reserved_safe_off_lane(&self) -> bool {
        self.safe_off_mailbox.is_some()
    }

    /// Test-only: construct an `I2cServiceHandle` whose channel is never
    /// served by a worker. Returns the handle plus a drop-guard that holds
    /// the receiver alive (so `submit` blocks instead of erroring on a
    /// closed channel). Calling any I/O method on the returned handle
    /// would block forever — tests must avoid those paths and only use
    /// the handle as a transport-token for state-machine assertions.
    #[cfg(test)]
    pub(crate) fn for_unit_tests() -> (Self, std::sync::mpsc::Receiver<I2cRequest>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        (
            Self {
                bus: 0,
                tx: I2cServiceSender::Raw(tx),
                safety: Arc::new(I2cSafetyAuthority::default()),
                safe_off_mailbox: None,
            },
            rx,
        )
    }

    fn submit<T>(
        &self,
        addr: u8,
        intent: I2cOperationIntent,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
    ) -> Result<T> {
        let budget = I2cRequestBudget::for_request(&req).ok_or_else(|| HalError::I2c {
            bus: self.bus,
            addr,
            detail: format!(
                "I2C request execution budget exceeds the {}s service limit; request was not admitted",
                I2C_MAX_EXECUTION_BUDGET.as_secs()
            ),
        })?;
        self.submit_with_intent_budget(addr, intent, req, reply_rx, budget)
    }

    fn submit_at_generation<T>(
        &self,
        addr: u8,
        intent: I2cOperationIntent,
        token: &I2cServiceGenerationToken,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
    ) -> Result<T> {
        let budget = I2cRequestBudget::for_request(&req).ok_or_else(|| HalError::I2c {
            bus: self.bus,
            addr,
            detail: format!(
                "I2C request execution budget exceeds the {}s service limit; request was not admitted",
                I2C_MAX_EXECUTION_BUDGET.as_secs()
            ),
        })?;
        self.submit_with_intent_budget_at_generation(
            addr,
            intent,
            req,
            reply_rx,
            budget,
            I2cSubmissionAuthority::Generation(token),
        )
    }

    #[cfg(test)]
    fn submit_with_budget<T>(
        &self,
        addr: u8,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
        budget: I2cRequestBudget,
    ) -> Result<T> {
        self.submit_with_intent_budget(
            addr,
            I2cOperationIntent::UnclassifiedMutation,
            req,
            reply_rx,
            budget,
        )
    }

    fn submit_with_intent_budget<T>(
        &self,
        addr: u8,
        intent: I2cOperationIntent,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
        budget: I2cRequestBudget,
    ) -> Result<T> {
        self.submit_with_intent_budget_at_generation(
            addr,
            intent,
            req,
            reply_rx,
            budget,
            I2cSubmissionAuthority::Current,
        )
    }

    fn submit_with_intent_budget_at_generation<T>(
        &self,
        addr: u8,
        intent: I2cOperationIntent,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
        budget: I2cRequestBudget,
        authority: I2cSubmissionAuthority<'_>,
    ) -> Result<T> {
        let is_admission_start = matches!(&req, I2cRequest::Pic16Admission { .. });
        let expected = match &authority {
            I2cSubmissionAuthority::Current => None,
            I2cSubmissionAuthority::Generation(token)
            | I2cSubmissionAuthority::Pic16Admission { token, .. }
            | I2cSubmissionAuthority::Pic16RuntimeBatch { token, .. } => Some(*token),
        };
        let scope = match &authority {
            I2cSubmissionAuthority::Pic16Admission {
                epoch,
                batch,
                reservation_token,
                ..
            } => I2cPermitScope::Pic16Admission {
                epoch: *epoch,
                batch: Arc::clone(batch),
                reservation_token: *reservation_token,
            },
            I2cSubmissionAuthority::Pic16RuntimeBatch { epoch, batch, .. } => {
                I2cPermitScope::Pic16RuntimeBatch {
                    epoch: *epoch,
                    batch: Arc::clone(batch),
                }
            }
            I2cSubmissionAuthority::Current | I2cSubmissionAuthority::Generation(_) => {
                I2cPermitScope::Generic
            }
        };
        let preflight_permit = I2cSafetyPermit {
            authority: Arc::clone(&self.safety),
            intent,
            generation: 0,
            scope: scope.clone(),
        };
        if let Some(error) = preflight_permit.address_authorization_error(self.bus, addr) {
            return Err(error);
        }
        if intent != I2cOperationIntent::SafeOff
            && !is_admission_start
            && self.safety.pic16_admission_owner.load(Ordering::SeqCst) != PIC16_ADMISSION_IDLE
        {
            return Err(hal_busy_error(self.bus, addr));
        }
        if intent == I2cOperationIntent::SafeOff {
            self.safety.advance_safe_off_generation();
        }
        let generation = if let Some(token) = expected {
            if token.bus != self.bus {
                return Err(HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail: format!(
                        "generation token is bound to I2C bus {}, not service bus {}",
                        token.bus, self.bus
                    ),
                });
            }
            let authority =
                token
                    .authority
                    .upgrade()
                    .ok_or_else(|| HalError::I2cSafetySuperseded {
                        bus: self.bus,
                        addr,
                        detail: "generation token belongs to a dropped I2C service lifetime".into(),
                    })?;
            if !Arc::ptr_eq(&authority, &self.safety) {
                return Err(HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr,
                    detail: "generation token belongs to a different I2C service lifetime".into(),
                });
            }
            if !authority.validate(intent, token.generation) {
                return Err(HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr,
                    detail: format!(
                        "{:?} request generation {} is no longer current",
                        intent, token.generation
                    ),
                });
            }
            token.generation
        } else {
            self.safety
                .capture(intent)
                .map_err(|detail| HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail: format!("{:?} request was not admitted: {detail}", intent),
                })?
        };

        #[cfg(test)]
        if let I2cServiceSender::Raw(tx) = &self.tx {
            tx.send(req).map_err(|_| HalError::I2c {
                bus: self.bus,
                addr,
                detail: "I2C unit-test service channel closed".into(),
            })?;
            return reply_rx.recv().unwrap_or(Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "I2C unit-test service reply dropped".into(),
            }));
        }

        let tx = match &self.tx {
            I2cServiceSender::Deadline(tx) => tx,
            #[cfg(test)]
            I2cServiceSender::Raw(_) => unreachable!("raw test sender returned above"),
        };
        let submitted_at = Instant::now();
        let admission_deadline = submitted_at + budget.admission;
        let must_start_by = submitted_at + budget.start;
        let reply_deadline = must_start_by + budget.execution;
        let state = Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED));
        let mut envelope = I2cServiceEnvelope {
            must_start_by,
            state: Arc::clone(&state),
            permit: I2cSafetyPermit {
                authority: Arc::clone(&self.safety),
                intent,
                generation,
                scope,
            },
            request: req,
        };

        loop {
            match tx.try_send(envelope) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    envelope = returned;
                    if Instant::now() >= admission_deadline {
                        return Err(HalError::I2c {
                            bus: self.bus,
                            addr,
                            detail: format!(
                                "I2C service queue remained full for {}ms; request was not admitted",
                                budget.admission.as_millis()
                            ),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(HalError::I2c {
                        bus: self.bus,
                        addr,
                        detail: "I2C service channel closed; request was not admitted".into(),
                    });
                }
            }
        }

        let remaining = reply_deadline.saturating_duration_since(Instant::now());
        match reply_rx.recv_timeout(remaining) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "I2C service reply dropped".into(),
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let observed = state
                    .compare_exchange(
                        I2C_REQUEST_QUEUED,
                        I2C_REQUEST_CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .unwrap_or_else(|current| current);
                let detail = match observed {
                    I2C_REQUEST_QUEUED | I2C_REQUEST_CANCELLED => {
                        "I2C request was cancelled before execution and will not touch the bus"
                            .into()
                    }
                    I2C_REQUEST_STARTED => format!(
                        "I2C request exceeded its {}ms execution budget after starting; hardware outcome is unknown",
                        budget.execution.as_millis()
                    ),
                    I2C_REQUEST_FINISHED => {
                        "I2C request finished but its reply missed the deadline; hardware outcome is unknown"
                            .into()
                    }
                    _ => "I2C request deadline exceeded; hardware outcome is unknown".into(),
                };
                Err(HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail,
                })
            }
        }
    }

    fn submit_pic16_runtime_batch_at_generation<T>(
        &self,
        addr: u8,
        intent: I2cOperationIntent,
        token: &I2cServiceGenerationToken,
        batch_submission: Pic16RuntimeBatchSubmission,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
    ) -> Result<T> {
        let budget = I2cRequestBudget::for_request(&req).ok_or_else(|| HalError::I2c {
            bus: self.bus,
            addr,
            detail: format!(
                "I2C request execution budget exceeds the {}s service limit; request was not admitted",
                I2C_MAX_EXECUTION_BUDGET.as_secs()
            ),
        })?;
        self.submit_with_intent_budget_at_generation(
            addr,
            intent,
            req,
            reply_rx,
            budget,
            I2cSubmissionAuthority::Pic16RuntimeBatch {
                token,
                epoch: batch_submission.epoch,
                batch: batch_submission.batch,
            },
        )
    }

    fn submit_pic16_admission_at_generation<T>(
        &self,
        addr: u8,
        token: &I2cServiceGenerationToken,
        reservation_token: u64,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
    ) -> Result<T> {
        let batch = match &req {
            I2cRequest::Pic16Admission { batch, .. } => Arc::clone(batch),
            _ => {
                return Err(HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail: "PIC16 admission authority requires a typed admission request".into(),
                });
            }
        };
        let budget = I2cRequestBudget::for_request(&req).ok_or_else(|| HalError::I2c {
            bus: self.bus,
            addr,
            detail: format!(
                "I2C request execution budget exceeds the {}s service limit; request was not admitted",
                I2C_MAX_EXECUTION_BUDGET.as_secs()
            ),
        })?;
        self.submit_with_intent_budget_at_generation(
            addr,
            I2cOperationIntent::Energize,
            req,
            reply_rx,
            budget,
            I2cSubmissionAuthority::Pic16Admission {
                token,
                epoch: batch.epoch(),
                batch,
                reservation_token,
            },
        )
    }

    /// Send a heartbeat request. Returns Ok(()) if the heartbeat succeeded.
    pub fn heartbeat(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        validate_pic_voltage_controller_address(self.bus, addr, "PIC heartbeat")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Heartbeat {
            addr,
            firmware,
            reply_tx,
        };
        self.submit(addr, I2cOperationIntent::KeepAlive, req, reply_rx)
    }

    /// Set voltage DAC value on a PIC.
    pub fn set_voltage(&self, addr: u8, firmware: I2cPicFirmware, pic_val: u8) -> Result<()> {
        validate_pic_voltage_controller_address(self.bus, addr, "PIC set-voltage")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetVoltage {
            addr,
            firmware,
            pic_val,
            reply_tx,
        };
        self.submit(addr, I2cOperationIntent::Energize, req, reply_rx)
    }

    /// Disable voltage output on a PIC.
    pub fn disable_voltage(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        if let Some(reply_rx) = self.enqueue_reserved_disable(addr, firmware)? {
            return wait_for_reserved_safe_off_receipt(
                self.bus,
                addr,
                reply_rx,
                "reserved safe-off",
            );
        }

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::DisableVoltage {
            addr,
            firmware,
            reply_tx,
        };
        self.submit(addr, I2cOperationIntent::SafeOff, req, reply_rx)
    }

    /// Linearize a raw SafeOff before managed PIC16 publication.
    ///
    /// The authority-state lock is deliberately held from the unmanaged check
    /// through the generation barrier and reserved-mailbox handoff. Therefore
    /// publication either precedes this operation and refuses it, or follows
    /// a fully committed SafeOff that the worker will prioritize before the
    /// ordinary admission request. The permit is never minted speculatively.
    fn commit_pre_management_safe_off<T>(
        &self,
        addr: u8,
        commit: impl FnOnce(I2cSafetyPermit) -> Result<T>,
    ) -> Result<T> {
        let service_state = self
            .safety
            .pic16_service_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if service_state.is_managed(addr) {
            return Err(managed_pic16_address_error(self.bus, addr));
        }
        self.safety.advance_safe_off_generation();
        let generation = self
            .safety
            .capture(I2cOperationIntent::SafeOff)
            .expect("SafeOff admission is valid in every lifecycle state");
        commit(I2cSafetyPermit {
            authority: Arc::clone(&self.safety),
            intent: I2cOperationIntent::SafeOff,
            generation,
            scope: I2cPermitScope::Pic16PreManagementSafeOff { address: addr },
        })
    }

    /// Admit a PIC disable directly to the reserved SafeOff mailbox without
    /// waiting for its receipt. Async callers use this before touching Tokio's
    /// blocking pool so pool saturation can delay observation, but can never
    /// prevent the hardware-safe command from being accepted by the worker.
    fn enqueue_reserved_disable(
        &self,
        addr: u8,
        firmware: I2cPicFirmware,
    ) -> Result<Option<mpsc::Receiver<Result<()>>>> {
        validate_pic16_safe_off_address(self.bus, addr)?;
        let Some(mailbox) = self.safe_off_mailbox.as_ref() else {
            return Ok(None);
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commit_pre_management_safe_off(addr, |permit| {
            mailbox.enqueue_disable(self.bus, addr, firmware, permit, reply_tx)
        })?;
        Ok(Some(reply_rx))
    }

    /// Set voltage in millivolts on a dsPIC33EP (S19 Pro / S17 style).
    pub fn set_voltage_mv(&self, addr: u8, voltage_mv: u16) -> Result<()> {
        validate_dspic_voltage_controller_address(self.bus, addr, "dsPIC set-voltage")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetVoltageMv {
            addr,
            voltage_mv,
            reply_tx,
        };
        self.submit(addr, I2cOperationIntent::Energize, req, reply_rx)
    }

    /// Atomically observe a discovery-issued PIC16F1704 endpoint and
    /// conditionally leave its bootloader.
    ///
    /// Exact `0xCC` is the only transition authority. The request contains no
    /// caller-provided predicate or bytes, and the worker revalidates the
    /// safety generation immediately before the fixed bytewise JUMP frame.
    pub fn pic16_jump_if_exact_bootloader(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
    ) -> Result<I2cPic16JumpOutcome> {
        let token = self.capture_generation_token()?;
        self.pic16_jump_if_exact_bootloader_with_generation(endpoint, &token)
    }

    /// Generation-bound form of [`Self::pic16_jump_if_exact_bootloader`].
    ///
    /// This is the admission-protocol primitive: unlike an ordinary request it
    /// cannot silently recapture a newer generation after a racing SafeOff.
    fn pic16_jump_if_exact_bootloader_with_generation(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
        token: &I2cServiceGenerationToken,
    ) -> Result<I2cPic16JumpOutcome> {
        let addr =
            validate_pic16_endpoint_capability(self.bus, endpoint, "a PIC16 boot transition")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Pic16JumpIfBootloader { addr, reply_tx };
        self.submit_at_generation(addr, I2cOperationIntent::Recovery, token, req, reply_rx)
    }

    /// Read exactly one raw-state byte from a discovery-issued PIC16 endpoint.
    pub fn pic16_read_raw_exact(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
    ) -> Result<u8> {
        let addr = validate_pic16_endpoint_capability(self.bus, endpoint, "a PIC16 raw read")?;
        let bytes = self.read_bytes(addr, 1)?;
        match bytes.as_slice() {
            [raw_state] => Ok(*raw_state),
            _ => Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!(
                    "PIC16 raw read returned {} byte(s); exact one-byte evidence is required",
                    bytes.len()
                ),
            }),
        }
    }

    /// Send the fixed PIC16 heartbeat through a discovery-issued endpoint.
    pub fn pic16_heartbeat(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
    ) -> Result<()> {
        let token = self.capture_generation_token()?;
        self.pic16_heartbeat_with_generation(endpoint, &token)
    }

    /// Send the fixed PIC16 heartbeat only if the exact service generation
    /// captured by `token` is still current at worker execution time.
    fn pic16_heartbeat_with_generation(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
        token: &I2cServiceGenerationToken,
    ) -> Result<()> {
        let addr = validate_pic16_endpoint_capability(self.bus, endpoint, "a PIC16 heartbeat")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Heartbeat {
            addr,
            firmware: I2cPicFirmware::Unknown,
            reply_tx,
        };
        self.submit_at_generation(addr, I2cOperationIntent::KeepAlive, token, req, reply_rx)
    }

    /// Program the clamped PIC16 DAC and enable its rail as one generation-
    /// fenced request after higher-level admission has established stability.
    pub fn pic16_set_and_enable(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
        pic_val: u8,
    ) -> Result<()> {
        let token = self.capture_generation_token()?;
        self.pic16_set_and_enable_with_generation(endpoint, &token, pic_val)
    }

    /// Generation-bound form of [`Self::pic16_set_and_enable`]. This remains a
    /// low-level protocol primitive; a future admission job must consume
    /// qualification evidence before calling it and mint authority only after
    /// a successful final heartbeat/compensation boundary.
    fn pic16_set_and_enable_with_generation(
        &self,
        endpoint: &crate::platform::VoltageControllerEndpoint,
        token: &I2cServiceGenerationToken,
        pic_val: u8,
    ) -> Result<()> {
        let addr =
            validate_pic16_endpoint_capability(self.bus, endpoint, "PIC16 voltage programming")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetVoltage {
            addr,
            firmware: I2cPicFirmware::Unknown,
            pic_val,
            reply_tx,
        };
        self.submit_at_generation(addr, I2cOperationIntent::Energize, token, req, reply_rx)
    }

    /// Begin one exclusive worker-owned PIC16 cold-boot batch.
    ///
    /// Every endpoint is discovery-issued, sorted into deterministic address
    /// order, and bound to one service generation before any I/O. Dropping the
    /// returned job requests compensation. Ordinary service work is refused
    /// from reservation through batch adoption; reserved SafeOff remains
    /// admissible and preemptive.
    pub fn begin_pic16_admission(
        &self,
        endpoints: impl IntoIterator<Item = crate::platform::VoltageControllerEndpoint>,
        initial_pic_value: u8,
    ) -> Result<Pic16AdmissionJob> {
        self.begin_pic16_admission_batch(
            endpoints.into_iter().map(|endpoint| {
                Pic16AdmissionTarget::program_and_enable(endpoint, initial_pic_value)
            }),
        )
    }

    /// Begin one exclusive worker-owned mixed PIC16 batch.
    ///
    /// Every target participates in the same observation and five-round
    /// heartbeat qualification. Program-and-enable targets alone may leave
    /// bootloader or receive SET/ENABLE; continue-running targets must already
    /// be in application mode and carry a non-forgeable running-endpoint
    /// capability. Any failure after shutdown ownership is registered
    /// compensates the entire batch. Production issuance of running-endpoint
    /// capabilities remains intentionally unavailable until ASIC enumeration
    /// exposes a HAL-owned live-chain lease; the host simulator supplies the
    /// current contract oracle.
    pub fn begin_pic16_admission_batch(
        &self,
        targets: impl IntoIterator<Item = Pic16AdmissionTarget>,
    ) -> Result<Pic16AdmissionJob> {
        let mut plans = targets
            .into_iter()
            .map(|target| {
                let (endpoint, mode, running_fence) = target.into_parts();
                validate_pic16_endpoint_capability(
                    self.bus,
                    &endpoint,
                    "worker-owned PIC16 admission",
                )
                .map(|address| Pic16AdmissionPlan::new(address, mode, running_fence))
            })
            .collect::<Result<Vec<_>>>()?;
        if plans.is_empty() {
            return Err(HalError::I2c {
                bus: self.bus,
                addr: 0,
                detail: "PIC16 admission requires at least one discovery-issued endpoint".into(),
            });
        }
        plans.sort_unstable_by_key(|plan| plan.address());
        if plans
            .windows(2)
            .any(|pair| pair[0].address() == pair[1].address())
        {
            return Err(HalError::I2c {
                bus: self.bus,
                addr: plans
                    .windows(2)
                    .find(|pair| pair[0].address() == pair[1].address())
                    .map_or(0, |pair| pair[0].address()),
                detail: "PIC16 admission endpoint list contains a duplicate capability".into(),
            });
        }
        let first_address = plans[0].address();
        let batch_addresses = plans
            .iter()
            .map(Pic16AdmissionPlan::address)
            .collect::<Vec<_>>();
        if self.safety.pic16_active_batch_epoch.load(Ordering::SeqCst) != 0 {
            return Err(HalError::I2cAdmissionBusy {
                bus: self.bus,
                addr: first_address,
                detail: "an adopted PIC16 batch is still active; perform proven batch SafeOff before another admission"
                    .into(),
            });
        }
        // Linearize this energizing lifecycle before reserving the ordinary
        // lane. A SafeOff racing anywhere after this capture invalidates the
        // token; admission must never recapture the post-barrier generation.
        let token = self.capture_generation_token();
        let reservation =
            Pic16AdmissionReservation::reserve(Arc::clone(&self.safety), self.bus, first_address)?;
        let reservation_token = reservation.token();
        if self.safety.pic16_active_batch_epoch.load(Ordering::SeqCst) != 0 {
            Pic16AdmissionReservation::revoke(&self.safety, reservation_token);
            return Err(HalError::I2cAdmissionBusy {
                bus: self.bus,
                addr: first_address,
                detail: "a PIC16 batch became active while this admission was reserving the worker"
                    .into(),
            });
        }
        let batch = match self.safety.publish_pic16_batch(self.bus, batch_addresses) {
            Ok(batch) => batch,
            Err(error) => {
                Pic16AdmissionReservation::revoke(&self.safety, reservation_token);
                return Err(error);
            }
        };
        let cleanup_handle =
            Pic16SafeOffHandle::for_batch(self.bus, &self.safety, Arc::clone(&batch));
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                Pic16AdmissionReservation::revoke(&self.safety, reservation_token);
                return Err(self.pic16_cleanup_failed_admission_start(
                    first_address,
                    &cleanup_handle,
                    error,
                ));
            }
        };

        let cancellation = Arc::new(AtomicBool::new(false));
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Pic16Admission {
            plans,
            batch: Arc::clone(&batch),
            cancellation: Arc::clone(&cancellation),
            reservation,
            completion_tx,
            reply_tx,
        };
        if let Err(error) = self.submit_pic16_admission_at_generation(
            first_address,
            &token,
            reservation_token,
            req,
            reply_rx,
        ) {
            cancellation.store(true, Ordering::SeqCst);
            Pic16AdmissionReservation::revoke(&self.safety, reservation_token);
            return Err(self.pic16_cleanup_failed_admission_start(
                first_address,
                &cleanup_handle,
                error,
            ));
        }
        Ok(Pic16AdmissionJob::new(
            self.bus,
            cancellation,
            completion_rx,
            self.clone(),
        ))
    }

    fn pic16_cleanup_failed_admission_start(
        &self,
        first_address: u8,
        cleanup_handle: &Pic16SafeOffHandle,
        error: HalError,
    ) -> HalError {
        match self.pic16_safe_off(cleanup_handle) {
            Ok(outcome) if outcome.all_disabled() => error,
            Ok(outcome) => HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: first_address,
                detail: format!(
                    "PIC16 admission start failed ({error}); provisional batch cleanup did not prove every endpoint disabled: {outcome:?}"
                ),
            },
            Err(cleanup_error) => HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: first_address,
                detail: format!(
                    "PIC16 admission start failed ({error}); provisional batch cleanup also failed: {cleanup_error}"
                ),
            },
        }
    }

    /// Send one fixed heartbeat to every endpoint in an admitted batch.
    ///
    /// The non-cloneable batch owner makes partial submission impossible at
    /// the public API. One worker job owns canonical endpoint order, aggregate
    /// Continue-mode liveness checks, the round deadline, and a final
    /// publication turn. Any incomplete round immediately attempts full-batch
    /// SafeOff.
    pub fn pic16_heartbeat_round(
        &self,
        admitted: &mut Pic16AdmittedBatch,
    ) -> Result<Pic16HeartbeatRoundOutcome> {
        if let Err(error) = validate_admitted_batch_for_service(
            self.bus,
            &self.safety,
            admitted,
            I2cOperationIntent::KeepAlive,
        ) {
            return Err(self.pic16_runtime_failure_after_submission(
                admitted,
                "heartbeat admission",
                error,
            ));
        }
        let first_address = admitted
            .endpoints()
            .first()
            .map_or(0, Pic16AdmittedEndpoint::address);
        let token = I2cServiceGenerationToken {
            bus: admitted.bus(),
            generation: admitted.generation(),
            authority: Arc::downgrade(
                &admitted
                    .authority()
                    .expect("validated admitted-batch authority"),
            ),
        };
        let batch = admitted.batch_for_worker();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::Pic16HeartbeatRound {
            batch: Arc::clone(&batch),
            reply_tx,
        };
        let batch_submission = Pic16RuntimeBatchSubmission {
            epoch: batch.epoch(),
            batch,
        };
        let outcome = match self.submit_pic16_runtime_batch_at_generation(
            first_address,
            I2cOperationIntent::KeepAlive,
            &token,
            batch_submission,
            request,
            reply_rx,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(self.pic16_runtime_failure_after_submission(
                    admitted,
                    "heartbeat round",
                    error,
                ));
            }
        };
        if let Err(error) = validate_admitted_batch_for_service(
            self.bus,
            &self.safety,
            admitted,
            I2cOperationIntent::KeepAlive,
        ) {
            return Err(self.pic16_runtime_failure_after_submission(
                admitted,
                "heartbeat final validation",
                error,
            ));
        }
        Ok(outcome)
    }

    /// Program one endpoint in an admitted PIC16 batch without issuing ENABLE.
    ///
    /// Runtime tuning must never reuse the legacy [`Self::set_voltage`] request:
    /// that compatibility operation performs SET followed by ENABLE and could
    /// therefore resurrect a rail after controller-watchdog cutoff without a
    /// fresh qualification. Only the worker-owned admission transaction may
    /// enable an endpoint.
    pub fn pic16_set_voltage_in_batch(
        &self,
        admitted: &mut Pic16AdmittedBatch,
        endpoint_id: &Pic16RuntimeEndpointId,
        pic_val: u8,
    ) -> Result<Pic16SetVoltageOutcome> {
        if let Err(error) = validate_admitted_batch_for_service(
            self.bus,
            &self.safety,
            admitted,
            I2cOperationIntent::Energize,
        ) {
            return Err(self.pic16_runtime_failure_after_submission(
                admitted,
                "runtime SET_VOLTAGE admission",
                error,
            ));
        }
        let Some(endpoint) = admitted.endpoint(endpoint_id) else {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: 0,
                detail: "PIC16 runtime endpoint ID does not belong to this admitted batch".into(),
            });
        };
        let address = endpoint.address();
        if !matches!(endpoint.mode(), Pic16AdmissionMode::ProgramAndEnable { .. }) {
            return Err(HalError::I2cSafetySuperseded {
                bus: self.bus,
                addr: address,
                detail: "a continue-running PIC16 endpoint does not authorize runtime SET".into(),
            });
        }
        let canonical_pic_value = clamp_pic_voltage_dac(pic_val);
        let token = I2cServiceGenerationToken {
            bus: admitted.bus(),
            generation: admitted.generation(),
            authority: Arc::downgrade(
                &admitted
                    .authority()
                    .expect("validated admitted-batch authority"),
            ),
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Pic16SetVoltageOnly {
            addr: address,
            pic_val: canonical_pic_value,
            reply_tx,
        };
        let batch_submission = Pic16RuntimeBatchSubmission::from_admitted_batch(admitted);
        if let Err(error) = self.submit_pic16_runtime_batch_at_generation(
            address,
            I2cOperationIntent::Energize,
            &token,
            batch_submission,
            req,
            reply_rx,
        ) {
            return Err(self.pic16_runtime_failure_after_submission(
                admitted,
                "runtime SET_VOLTAGE",
                error,
            ));
        }
        if let Err(error) = validate_admitted_batch_for_service(
            self.bus,
            &self.safety,
            admitted,
            I2cOperationIntent::Energize,
        ) {
            return Err(self.pic16_runtime_failure_after_submission(
                admitted,
                "runtime SET_VOLTAGE final validation",
                error,
            ));
        }
        Ok(Pic16SetVoltageOutcome::new(
            admitted.batch_epoch(),
            address,
            pic_val,
            canonical_pic_value,
        ))
    }

    /// Disable every endpoint in one admitted atomic batch.
    pub fn pic16_safe_off_admitted_batch(
        &self,
        admitted: &mut Pic16AdmittedBatch,
    ) -> Result<Pic16BatchSafeOffOutcome> {
        validate_admitted_batch_for_service(
            self.bus,
            &self.safety,
            admitted,
            I2cOperationIntent::SafeOff,
        )?;
        self.pic16_safe_off(&admitted.safe_off_handle())
    }

    fn pic16_runtime_failure_after_submission(
        &self,
        admitted: &Pic16AdmittedBatch,
        operation: &'static str,
        primary: HalError,
    ) -> HalError {
        let primary_detail = primary.to_string();
        match self.pic16_safe_off(&admitted.safe_off_handle()) {
            Ok(outcome) if outcome.all_disabled() => primary,
            Ok(outcome) => HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: admitted.endpoints().first().map_or(0, |endpoint| endpoint.address()),
                detail: format!(
                    "PIC16 {operation} failed ({primary_detail}); whole-batch cleanup did not prove a newly executed disable for every endpoint: {outcome:?}"
                ),
            },
            Err(cleanup_error) => HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: admitted.endpoints().first().map_or(0, |endpoint| endpoint.address()),
                detail: format!(
                    "PIC16 {operation} failed ({primary_detail}); whole-batch cleanup also failed: {cleanup_error}"
                ),
            },
        }
    }

    /// Execute batch SafeOff using independently cloneable, disable-only
    /// authority derived from an admitted batch.
    pub fn pic16_safe_off(&self, handle: &Pic16SafeOffHandle) -> Result<Pic16BatchSafeOffOutcome> {
        validate_safe_off_handle_for_service(self.bus, &self.safety, handle)?;
        self.pic16_safe_off_batch(handle.batch())
    }

    /// Recover and shutdown the service-retained active PIC16 batch even when
    /// every batch owner has disappeared. Returns `None` when no batch is
    /// registered.
    pub fn pic16_safe_off_active_batch(&self) -> Result<Option<Pic16BatchSafeOffOutcome>> {
        self.safety
            .active_pic16_batch()
            .map(|batch| self.pic16_safe_off_batch(batch))
            .transpose()
    }

    /// Point-in-time diagnostic view of service-retained PIC16 shutdown
    /// ownership. Addresses do not grant mutation authority.
    pub fn pic16_active_batch_addresses(&self) -> Option<Vec<u8>> {
        self.safety
            .active_pic16_batch()
            .map(|batch| batch.addresses().to_vec())
    }

    fn pic16_safe_off_batch(
        &self,
        batch: Arc<Pic16BatchAuthority>,
    ) -> Result<Pic16BatchSafeOffOutcome> {
        self.pic16_safe_off_batch_with_budget(batch, I2C_SAFE_OFF_RECEIPT_BUDGET)
    }

    /// Transfer an abandoned admitted batch directly to the reserved SafeOff
    /// lane without blocking `Drop` on hardware completion.
    fn enqueue_pic16_batch_safe_off_on_drop(&self, batch: Arc<Pic16BatchAuthority>) {
        if batch.released() {
            return;
        }
        let epoch = batch.epoch();
        let first_address = batch.addresses().first().copied().unwrap_or(0);
        let mut attempt = loop {
            if let Some(attempt) = batch.claim_safe_off_attempt() {
                break attempt;
            }
            match batch.wait_for_safe_off_attempt(Duration::ZERO) {
                Pic16BatchSafeOffOwnership::Idle => continue,
                Pic16BatchSafeOffOwnership::WorkerOwned => return,
                Pic16BatchSafeOffOwnership::CallerClaimed => {
                    self.safety.mark_pic16_shutdown_unresolved();
                    tracing::error!(
                        bus = self.bus,
                        addr = first_address,
                        epoch,
                        "abandoned PIC16 batch raced a caller-claimed SafeOff before worker handoff; terminal mutation admission is latched"
                    );
                    return;
                }
            }
        };
        let active_epoch = self.safety.pic16_active_batch_epoch.load(Ordering::SeqCst);
        if active_epoch != epoch {
            self.safety.mark_pic16_shutdown_unresolved();
            tracing::error!(
                bus = self.bus,
                addr = first_address,
                epoch,
                active_epoch,
                "abandoned PIC16 batch does not match the service registry; terminal mutation admission is latched"
            );
            return;
        }
        let Some(mailbox) = self.safe_off_mailbox.as_ref() else {
            self.safety.mark_pic16_shutdown_unresolved();
            tracing::error!(
                bus = self.bus,
                addr = first_address,
                epoch,
                "abandoned PIC16 batch has no reserved SafeOff mailbox; watchdog cutoff is required"
            );
            return;
        };

        let generation = self.safety.advance_safe_off_generation();
        let addresses = batch.addresses().iter().copied().rev().collect::<Vec<_>>();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if let Err(error) = mailbox.enqueue_pic16_batch_disable(
            self.bus,
            epoch,
            addresses,
            Arc::clone(&batch),
            I2cSafetyPermit {
                authority: Arc::clone(&self.safety),
                intent: I2cOperationIntent::SafeOff,
                generation,
                scope: I2cPermitScope::Pic16BatchSafeOff {
                    epoch,
                    batch: Arc::clone(&batch),
                },
            },
            &mut attempt,
            reply_tx,
        ) {
            self.safety.mark_pic16_shutdown_unresolved();
            tracing::error!(
                bus = self.bus,
                addr = first_address,
                epoch,
                %error,
                "abandoned PIC16 batch could not enter the reserved SafeOff lane; watchdog cutoff is required"
            );
            return;
        }
        // The mailbox now owns completion and registry release. The abandoned
        // owner deliberately does not retain a receiver or wait in `Drop`.
        drop(attempt);
        drop(reply_rx);
    }

    fn pic16_safe_off_batch_with_budget(
        &self,
        batch: Arc<Pic16BatchAuthority>,
        receipt_budget: Duration,
    ) -> Result<Pic16BatchSafeOffOutcome> {
        let epoch = batch.epoch();
        let first_address = batch.addresses().first().copied().unwrap_or(0);
        let deadline = Instant::now() + receipt_budget;
        let mut attempt = loop {
            if let Some(attempt) = batch.claim_safe_off_attempt() {
                break attempt;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match batch.wait_for_safe_off_attempt(remaining) {
                Pic16BatchSafeOffOwnership::Idle => continue,
                Pic16BatchSafeOffOwnership::CallerClaimed => {
                    self.safety.mark_pic16_shutdown_unresolved();
                    return Err(HalError::I2cSafeOffOutcomeUnknown {
                        bus: self.bus,
                        addr: first_address,
                        detail: "another PIC16 SafeOff caller stalled before worker handoff; terminal mutation admission was latched while its claim remains authoritative"
                            .into(),
                    });
                }
                Pic16BatchSafeOffOwnership::WorkerOwned => {
                    return Err(HalError::I2cSafeOffOutcomeUnknown {
                    bus: self.bus,
                    addr: first_address,
                    detail: "another PIC16 batch SafeOff remains worker-owned; no duplicate attempt was enqueued"
                        .into(),
                    });
                }
            }
        };

        let active_epoch = self.safety.pic16_active_batch_epoch.load(Ordering::SeqCst);
        if batch.released() {
            return if active_epoch == 0 {
                Ok(Pic16BatchSafeOffOutcome::already_released(epoch))
            } else {
                Err(HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr: first_address,
                    detail: format!(
                        "released PIC16 batch epoch {epoch} was superseded by active epoch {active_epoch}"
                    ),
                })
            };
        }
        if active_epoch != epoch {
            if active_epoch != 0 {
                return Err(HalError::I2cSafetySuperseded {
                    bus: self.bus,
                    addr: first_address,
                    detail: format!(
                        "PIC16 SafeOff epoch {epoch} was superseded by active epoch {active_epoch}"
                    ),
                });
            }
            self.safety.mark_pic16_shutdown_unresolved();
            return Err(HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: first_address,
                detail: format!(
                    "PIC16 SafeOff epoch {epoch} is unreleased but absent from the service registry"
                ),
            });
        }

        let Some(mailbox) = self.safe_off_mailbox.as_ref() else {
            self.safety.mark_pic16_shutdown_unresolved();
            return Err(HalError::I2cSafeOffOutcomeUnknown {
                bus: self.bus,
                addr: first_address,
                detail: "PIC16 batch SafeOff requires the worker's reserved mailbox".into(),
            });
        };
        let generation = self.safety.advance_safe_off_generation();
        let addresses = batch.addresses().iter().copied().rev().collect::<Vec<_>>();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if let Err(error) = mailbox.enqueue_pic16_batch_disable(
            self.bus,
            epoch,
            addresses,
            Arc::clone(&batch),
            I2cSafetyPermit {
                authority: Arc::clone(&self.safety),
                intent: I2cOperationIntent::SafeOff,
                generation,
                scope: I2cPermitScope::Pic16BatchSafeOff {
                    epoch,
                    batch: Arc::clone(&batch),
                },
            },
            &mut attempt,
            reply_tx,
        ) {
            self.safety.mark_pic16_shutdown_unresolved();
            return Err(error);
        }
        // Queue insertion and CallerClaimed -> WorkerOwned transition happen
        // under the mailbox lock. From this point completion is the sole
        // finalizer, even if this caller times out or unwinds.
        drop(attempt);
        let remaining = deadline.saturating_duration_since(Instant::now());
        let outcome =
            wait_for_pic16_batch_safe_off_receipt(self.bus, first_address, reply_rx, remaining)?;
        if outcome.all_disabled() {
            debug_assert!(batch.released());
            debug_assert_eq!(
                self.safety.pic16_active_batch_epoch.load(Ordering::SeqCst),
                0
            );
        }
        Ok(outcome)
    }

    // --- v0.13.0: Generic I2C operations for init ---

    /// Write bytes to an I2C slave (single transaction).
    pub fn write_bytes(&self, addr: u8, data: &[u8]) -> Result<()> {
        self.write_bytes_with_intent(I2cOperationIntent::UnclassifiedMutation, addr, data)
    }

    pub(crate) fn write_bytes_with_intent(
        &self,
        intent: I2cOperationIntent,
        addr: u8,
        data: &[u8],
    ) -> Result<()> {
        validate_message_len(self.bus, addr, "service write", data.len())?;
        if intent == I2cOperationIntent::SafeOff {
            if let Some(mailbox) = self.safe_off_mailbox.as_ref() {
                let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                self.commit_pre_management_safe_off(addr, |permit| {
                    mailbox.enqueue_write(self.bus, addr, data.to_vec(), permit, reply_tx)
                })?;
                return wait_for_reserved_safe_off_receipt(
                    self.bus,
                    addr,
                    reply_rx,
                    "reserved safe-off write",
                );
            }
        }

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteBytes {
            addr,
            data: data.to_vec(),
            reply_tx,
        };
        self.submit(addr, intent, req, reply_rx)
    }

    /// Write bytes as a normal, terminal-fenced controller mutation.
    pub fn write_bytes_mutating(
        &self,
        label: I2cMutationLabel,
        addr: u8,
        data: &[u8],
    ) -> Result<()> {
        self.write_bytes_with_intent(label.internal_intent(), addr, data)
    }

    /// Write bytes one-at-a-time (byte-by-byte PIC pattern, 1ms between bytes).
    pub fn write_byte_by_byte(&self, addr: u8, data: &[u8]) -> Result<()> {
        self.write_byte_by_byte_with_intent(I2cOperationIntent::UnclassifiedMutation, addr, data)
    }

    pub(crate) fn write_byte_by_byte_with_intent(
        &self,
        intent: I2cOperationIntent,
        addr: u8,
        data: &[u8],
    ) -> Result<()> {
        validate_message_len(self.bus, addr, "service bytewise write", data.len())?;
        if intent == I2cOperationIntent::SafeOff {
            if let Some(mailbox) = self.safe_off_mailbox.as_ref() {
                let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                self.commit_pre_management_safe_off(addr, |permit| {
                    mailbox.enqueue_bytewise_write(self.bus, addr, data.to_vec(), permit, reply_tx)
                })?;
                return wait_for_reserved_safe_off_receipt(
                    self.bus,
                    addr,
                    reply_rx,
                    "reserved bytewise safe-off",
                );
            }
        }

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteByteByte {
            addr,
            data: data.to_vec(),
            reply_tx,
        };
        self.submit(addr, intent, req, reply_rx)
    }

    /// Write byte-by-byte as a normal, terminal-fenced controller mutation.
    pub fn write_byte_by_byte_mutating(
        &self,
        label: I2cMutationLabel,
        addr: u8,
        data: &[u8],
    ) -> Result<()> {
        self.write_byte_by_byte_with_intent(label.internal_intent(), addr, data)
    }

    /// Read bytes from an I2C slave.
    pub fn read_bytes(&self, addr: u8, len: usize) -> Result<Vec<u8>> {
        validate_message_len(self.bus, addr, "service read", len)?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::ReadBytes {
            addr,
            len,
            reply_tx,
        };
        self.submit(addr, I2cOperationIntent::ReadOnly, req, reply_rx)
    }

    /// Read a fixed hashboard AT24 identity prefix through the sole service.
    ///
    /// Platform/topology code supplies the resolved address and an absolute
    /// deadline. The worker admits only the AT24 range and only endpoints in
    /// this service's configured protected-address policy. Production reads
    /// use the bound kernel driver's sysfs endpoint; generic raw write and
    /// write-read APIs remain denied.
    pub fn read_hashboard_eeprom_prefix_at(&self, addr: u8, deadline: Instant) -> Result<Vec<u8>> {
        if !(HASHBOARD_EEPROM_FIRST_ADDR..=HASHBOARD_EEPROM_LAST_ADDR).contains(&addr) {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "hashboard EEPROM prefix address must be within 0x50..=0x57".into(),
            });
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::ReadHashboardEepromPrefix { addr, reply_tx };
        let Some(budget) = I2cRequestBudget::for_request_until(&request, deadline) else {
            return Err(HalError::I2cEndpointNotReady {
                bus: self.bus,
                addr,
                detail: "identity-read deadline elapsed before request admission".into(),
            });
        };
        self.submit_with_intent_budget(
            addr,
            I2cOperationIntent::ReadOnly,
            request,
            reply_rx,
            budget,
        )
    }

    /// Read one fixed LM75-compatible temperature register before an absolute
    /// deadline. The register-pointer write makes this a query prelude, so it
    /// remains fenced by terminal safe-off like every other controller
    /// mutation even though the returned value is telemetry.
    pub fn read_lm75_temperature_register_at(
        &self,
        addr: u8,
        deadline: Instant,
    ) -> Result<Lm75TemperatureRegister> {
        if !(LM75_FIRST_ADDR..=LM75_LAST_ADDR).contains(&addr) {
            return Err(HalError::I2cEndpointRefused {
                bus: self.bus,
                addr,
                detail: "LM75 temperature address must be within 0x48..=0x4f".into(),
            });
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::ReadLm75TemperatureRegister { addr, reply_tx };
        let Some(budget) = I2cRequestBudget::for_request_until(&request, deadline) else {
            return Err(HalError::I2cEndpointNotReady {
                bus: self.bus,
                addr,
                detail: "LM75 read deadline elapsed before request admission".into(),
            });
        };
        self.submit_with_intent_budget(
            addr,
            I2cOperationIntent::UnclassifiedMutation,
            request,
            reply_rx,
            budget,
        )
    }

    /// Combined write+read (I2C_RDWR repeated START).
    pub fn write_read(&self, addr: u8, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        self.write_read_with_intent(
            I2cOperationIntent::UnclassifiedMutation,
            addr,
            write_data,
            read_len,
        )
    }

    /// Combined write+read with an explicit semantic safety class.
    pub(crate) fn write_read_with_intent(
        &self,
        intent: I2cOperationIntent,
        addr: u8,
        write_data: &[u8],
        read_len: usize,
    ) -> Result<Vec<u8>> {
        validate_message_len(self.bus, addr, "service write-read write", write_data.len())?;
        validate_message_len(self.bus, addr, "service write-read read", read_len)?;
        if intent == I2cOperationIntent::SafeOff {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "SafeOff write-read is not admissible: shutdown plans must be write-only and use the reserved compound lane"
                    .into(),
            });
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteRead {
            addr,
            write_data: write_data.to_vec(),
            read_len,
            reply_tx,
        };
        self.submit(addr, intent, req, reply_rx)
    }

    /// Execute write-read as a normal, terminal-fenced controller mutation.
    pub fn write_read_mutating(
        &self,
        label: I2cMutationLabel,
        addr: u8,
        write_data: &[u8],
        read_len: usize,
    ) -> Result<Vec<u8>> {
        self.write_read_with_intent(label.internal_intent(), addr, write_data, read_len)
    }

    /// Set I2C timeout (units of 10ms jiffies).
    pub fn set_timeout(&self, timeout_jiffies: u32) -> Result<()> {
        if timeout_jiffies != I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES {
            return Err(HalError::I2c {
                bus: self.bus,
                addr: 0,
                detail: format!(
                    "standalone I2C timeout must remain at the service default of {} jiffies; use transaction-scoped SetTimeout for protocol-specific timing",
                    I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES
                ),
            });
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetTimeout {
            timeout_jiffies,
            reply_tx,
        };
        self.submit(0, I2cOperationIntent::NeutralControl, req, reply_rx)
    }

    /// Recover the complete I2C fabric before any PIC16 endpoint is managed.
    ///
    /// This operation is permanently refused after the first PIC16 batch is
    /// published in this service lifetime, including after proven SafeOff.
    /// Callers cannot target an address or select recovery bytes.
    pub fn recover_unmanaged_bus(&self) -> Result<()> {
        {
            let service_state = self
                .safety
                .pic16_service_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if service_state.has_managed_addresses() {
                return Err(managed_pic16_fabric_error(self.bus));
            }
            if self.safety.pic16_admission_owner.load(Ordering::SeqCst) != PIC16_ADMISSION_IDLE {
                return Err(hal_busy_error(self.bus, 0));
            }
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::RecoverUnmanagedBus { reply_tx };
        self.submit(0, I2cOperationIntent::Recovery, request, reply_rx)
    }

    /// Execute ordered I2C steps under one service-worker bus/address lock.
    ///
    /// The returned vector contains one entry for each `Read` or `WriteRead`
    /// step, in execution order.
    pub fn transaction(&self, addr: u8, steps: Vec<I2cTransactionStep>) -> Result<Vec<Vec<u8>>> {
        self.transaction_with_intent(I2cOperationIntent::UnclassifiedMutation, addr, steps)
    }

    /// Execute a write-only SafeOff transaction as one reserved worker plan.
    /// Sleeps remain inside the worker, so no unrelated bus request can run
    /// between protocol steps.
    pub(crate) fn safe_off_transaction(
        &self,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<()> {
        validate_transaction_message_lengths(self.bus, addr, &steps)?;
        if steps.iter().any(|step| {
            matches!(
                step,
                I2cTransactionStep::Read(_)
                    | I2cTransactionStep::ReadFrame { .. }
                    | I2cTransactionStep::WriteRead { .. }
            )
        }) {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "reserved SafeOff transaction cannot discard read results".into(),
            });
        }

        let Some(mailbox) = self.safe_off_mailbox.as_ref() else {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            let request = I2cRequest::Transaction {
                addr,
                steps,
                reply_tx,
            };
            return self
                .submit(addr, I2cOperationIntent::SafeOff, request, reply_rx)
                .map(|_| ());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commit_pre_management_safe_off(addr, |permit| {
            mailbox.enqueue_transaction(self.bus, addr, steps, permit, reply_tx)
        })?;
        wait_for_reserved_safe_off_receipt(self.bus, addr, reply_rx, "reserved compound safe-off")
    }

    /// Execute a worker-owned conditional SafeOff plan without releasing the
    /// bus between phases. The compensation steps are an internal safety
    /// backstop, reachable only after the worker observes primary failure;
    /// they are not exposed as a standalone SafeOff operation.
    pub(crate) fn conditional_safe_off_plan(
        &self,
        addr: u8,
        prelude: Vec<I2cTransactionStep>,
        primary: Vec<I2cTransactionStep>,
        compensation: Vec<I2cTransactionStep>,
    ) -> Result<I2cConditionalSafeOffOutcome> {
        let total_steps = prelude
            .len()
            .checked_add(primary.len())
            .and_then(|count| count.checked_add(compensation.len()))
            .ok_or_else(|| HalError::I2c {
                bus: self.bus,
                addr,
                detail: "conditional safe-off plan step count overflow".into(),
            })?;
        if total_steps > I2C_MAX_TRANSACTION_STEPS {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!(
                    "conditional safe-off plan has {total_steps} total steps, exceeding the {I2C_MAX_TRANSACTION_STEPS}-step service limit"
                ),
            });
        }
        validate_transaction_message_lengths(self.bus, addr, &prelude)?;
        validate_transaction_message_lengths(self.bus, addr, &primary)?;
        validate_transaction_message_lengths(self.bus, addr, &compensation)?;
        let Some(mailbox) = self.safe_off_mailbox.as_ref() else {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            let request = I2cRequest::ConditionalSafeOffPlan {
                addr,
                prelude,
                primary,
                compensation,
                reply_tx,
            };
            return self.submit(addr, I2cOperationIntent::SafeOff, request, reply_rx);
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commit_pre_management_safe_off(addr, |permit| {
            mailbox.enqueue_conditional_plan(
                self.bus,
                addr,
                prelude,
                primary,
                compensation,
                permit,
                reply_tx,
            )
        })?;
        match reply_rx.recv_timeout(I2C_SAFE_OFF_RECEIPT_BUDGET) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(HalError::I2cSafeOffOutcomeUnknown {
                    bus: self.bus,
                    addr,
                    detail: format!(
                        "conditional plan did not complete within {}ms; it remains queued and must not be resubmitted",
                        I2C_SAFE_OFF_RECEIPT_BUDGET.as_millis()
                    ),
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(HalError::I2cSafeOffOutcomeUnknown {
                    bus: self.bus,
                    addr,
                    detail: "conditional plan receipt was dropped; hardware outcome is unknown"
                        .into(),
                })
            }
        }
    }

    /// Execute a controller-specific compound transaction with an explicit
    /// semantic safety class. New protocol drivers should use this API rather
    /// than the compatibility `transaction` method.
    pub(crate) fn transaction_with_intent(
        &self,
        intent: I2cOperationIntent,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<Vec<Vec<u8>>> {
        validate_transaction_message_lengths(self.bus, addr, &steps)?;
        if intent == I2cOperationIntent::SafeOff {
            self.safe_off_transaction(addr, steps)?;
            return Ok(Vec::new());
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Transaction {
            addr,
            steps,
            reply_tx,
        };
        self.submit(addr, intent, req, reply_rx)
    }

    /// Execute a compound normal-lane mutation with an audit-only label.
    pub fn transaction_mutating(
        &self,
        label: I2cMutationLabel,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<Vec<Vec<u8>>> {
        self.transaction_with_intent(label.internal_intent(), addr, steps)
    }

    /// Execute a fixed dsPIC voltage-disable plan through the reserved
    /// terminal-safe lane. No caller-provided bytes can acquire SafeOff
    /// authority through this API.
    pub fn disable_dspic_voltage(&self, addr: u8, protocol: I2cDspicDisableProtocol) -> Result<()> {
        if !(0x20..=0x22).contains(&addr) {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "dsPIC safe-off address must be in 0x20..=0x22".into(),
            });
        }
        let frame = match protocol {
            I2cDspicDisableProtocol::Bare => vec![0x55, 0xAA, 0x15, 0x00],
            I2cDspicDisableProtocol::CanonicalFramed => {
                vec![0x55, 0xAA, 0x04, 0x15, 0x00, 0x19]
            }
            I2cDspicDisableProtocol::VnishPaddedFramed => {
                vec![0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
            }
        };
        self.safe_off_transaction(addr, vec![I2cTransactionStep::Write(frame)])
    }

    /// Execute the fixed PIC1704 `REG_CONTROL=DC_DC_OFF` plan through the
    /// reserved terminal-safe lane.
    pub fn disable_pic1704_dc_dc(&self, addr: u8) -> Result<()> {
        if addr != 0x20 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: "PIC1704 safe-off address must be 0x20".into(),
            });
        }
        self.safe_off_transaction(addr, vec![I2cTransactionStep::Write(vec![0x09, 0x00])])
    }
}

impl AsyncI2cServiceHandle {
    async fn offload<T, F>(
        &self,
        addr: u8,
        operation: &'static str,
        dispatch_budget: Duration,
        call: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(I2cServiceHandle) -> Result<T> + Send + 'static,
    {
        let state = Arc::new(AtomicU8::new(I2C_ASYNC_WAITING));
        let mut guard = CancelAsyncI2cBeforeDispatch {
            state: Arc::clone(&state),
            addr,
            operation,
            armed: true,
        };
        let worker_state = Arc::clone(&state);
        let service = self.inner.clone();
        let bus = service.bus;
        let mut join = tokio::task::spawn_blocking(move || {
            if worker_state
                .compare_exchange(
                    I2C_ASYNC_WAITING,
                    I2C_ASYNC_STARTED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: format!(
                        "{operation}: async caller cancelled before dispatch; request was not submitted"
                    ),
                });
            }
            let result = call(service);
            worker_state.store(I2C_ASYNC_FINISHED, Ordering::Release);
            result
        });

        let result = tokio::select! {
            joined = &mut join => map_async_i2c_join(joined, state.load(Ordering::Acquire), bus, addr, operation),
            _ = tokio::time::sleep(dispatch_budget) => {
                match state.compare_exchange(
                    I2C_ASYNC_WAITING,
                    I2C_ASYNC_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => Err(HalError::I2c {
                        bus,
                        addr,
                        detail: format!(
                            "{operation}: Tokio blocking-pool dispatch exceeded {}ms; request was not submitted",
                            dispatch_budget.as_millis()
                        ),
                    }),
                    Err(I2C_ASYNC_STARTED) | Err(I2C_ASYNC_FINISHED) => {
                        let joined = join.await;
                        map_async_i2c_join(joined, state.load(Ordering::Acquire), bus, addr, operation)
                    }
                    Err(_) => Err(HalError::I2c {
                        bus,
                        addr,
                        detail: format!("{operation}: cancelled before service submission"),
                    }),
                }
            }
        };
        guard.armed = false;
        result
    }

    pub async fn heartbeat(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        validate_pic_voltage_controller_address(self.inner.bus, addr, "PIC heartbeat")?;
        self.offload(
            addr,
            "heartbeat",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.heartbeat(addr, firmware),
        )
        .await
    }

    pub async fn set_voltage(&self, addr: u8, firmware: I2cPicFirmware, pic_val: u8) -> Result<()> {
        validate_pic_voltage_controller_address(self.inner.bus, addr, "PIC set-voltage")?;
        self.offload(
            addr,
            "set_voltage",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.set_voltage(addr, firmware, pic_val),
        )
        .await
    }

    pub async fn disable_voltage(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        // SafeOff admission is deliberately synchronous and precedes any use
        // of Tokio's bounded blocking pool. If that pool is saturated, receipt
        // observation may time out, but the accepted disable remains owned by
        // the reserved mailbox and cannot be cancelled with this future.
        if let Some(reply_rx) = self.inner.enqueue_reserved_disable(addr, firmware)? {
            let bus = self.inner.bus;
            let receipt = tokio::task::spawn_blocking(move || {
                wait_for_reserved_safe_off_receipt(bus, addr, reply_rx, "reserved async safe-off")
            });
            return match tokio::time::timeout(
                I2C_SAFE_OFF_RECEIPT_BUDGET + Duration::from_millis(100),
                receipt,
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => Err(HalError::I2cSafeOffOutcomeUnknown {
                    bus,
                    addr,
                    detail: format!(
                        "reserved async safe-off receipt task failed ({error}); hardware outcome is unknown"
                    ),
                }),
                Err(_) => Err(HalError::I2cSafeOffOutcomeUnknown {
                    bus,
                    addr,
                    detail: format!(
                        "reserved async safe-off receipt was not observed within {}ms; the command was already accepted independently of this caller and physical rail state is unknown",
                        (I2C_SAFE_OFF_RECEIPT_BUDGET + Duration::from_millis(100)).as_millis()
                    ),
                }),
            };
        }

        self.offload(
            addr,
            "disable_voltage",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.disable_voltage(addr, firmware),
        )
        .await
    }

    pub async fn set_voltage_mv(&self, addr: u8, voltage_mv: u16) -> Result<()> {
        validate_dspic_voltage_controller_address(self.inner.bus, addr, "dsPIC set-voltage")?;
        self.offload(
            addr,
            "set_voltage_mv",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.set_voltage_mv(addr, voltage_mv),
        )
        .await
    }

    pub async fn write_bytes(&self, addr: u8, data: &[u8]) -> Result<()> {
        let data = data.to_vec();
        self.offload(
            addr,
            "write_bytes",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.write_bytes(addr, &data),
        )
        .await
    }

    pub async fn write_byte_by_byte(&self, addr: u8, data: &[u8]) -> Result<()> {
        let data = data.to_vec();
        self.offload(
            addr,
            "write_byte_by_byte",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.write_byte_by_byte(addr, &data),
        )
        .await
    }

    pub async fn read_bytes(&self, addr: u8, len: usize) -> Result<Vec<u8>> {
        self.offload(
            addr,
            "read_bytes",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.read_bytes(addr, len),
        )
        .await
    }

    pub async fn write_read(
        &self,
        addr: u8,
        write_data: &[u8],
        read_len: usize,
    ) -> Result<Vec<u8>> {
        let write_data = write_data.to_vec();
        self.offload(
            addr,
            "write_read",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.write_read(addr, &write_data, read_len),
        )
        .await
    }

    pub async fn set_timeout(&self, timeout_jiffies: u32) -> Result<()> {
        self.offload(
            0,
            "set_timeout",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.set_timeout(timeout_jiffies),
        )
        .await
    }

    pub async fn transaction(
        &self,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<Vec<Vec<u8>>> {
        self.transaction_with_intent(I2cOperationIntent::UnclassifiedMutation, addr, steps)
            .await
    }

    pub(crate) async fn transaction_with_intent(
        &self,
        intent: I2cOperationIntent,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<Vec<Vec<u8>>> {
        validate_transaction_message_lengths(self.inner.bus, addr, &steps)?;
        self.offload(
            addr,
            "transaction",
            I2C_ASYNC_DISPATCH_BUDGET,
            move |service| service.transaction_with_intent(intent, addr, steps),
        )
        .await
    }

    /// Async wrapper for a normal, terminal-fenced compound mutation.
    pub async fn transaction_mutating(
        &self,
        label: I2cMutationLabel,
        addr: u8,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<Vec<Vec<u8>>> {
        self.transaction_with_intent(label.internal_intent(), addr, steps)
            .await
    }
}

fn map_async_i2c_join<T>(
    joined: std::result::Result<Result<T>, tokio::task::JoinError>,
    state: u8,
    bus: u8,
    addr: u8,
    operation: &'static str,
) -> Result<T> {
    joined.unwrap_or_else(|error| {
        let outcome = if matches!(state, I2C_ASYNC_STARTED | I2C_ASYNC_FINISHED) {
            "hardware outcome is unknown"
        } else {
            "request was not submitted"
        };
        Err(HalError::I2c {
            bus,
            addr,
            detail: format!("{operation}: blocking task failed ({error}); {outcome}"),
        })
    })
}

#[cfg(test)]
mod i2c_service_deadline_tests {
    use super::*;

    #[cfg(feature = "sim-hal")]
    fn next_test_service_identity() -> usize {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        usize::MAX - NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(feature = "sim-hal")]
    type At24Transfer = (u8, u8, Vec<u8>, usize);

    #[cfg(feature = "sim-hal")]
    struct At24IdentityBackend {
        identity: usize,
        transfers: Mutex<Vec<At24Transfer>>,
    }

    #[cfg(feature = "sim-hal")]
    impl At24IdentityBackend {
        fn new() -> Self {
            Self {
                identity: next_test_service_identity(),
                transfers: Mutex::new(Vec::new()),
            }
        }

        fn transfers(&self) -> Vec<At24Transfer> {
            self.transfers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for At24IdentityBackend {
        fn write(&self, bus: u8, addr: u8, _data: &[u8]) -> Result<usize> {
            Err(HalError::I2c {
                bus,
                addr,
                detail: "AT24 identity backend refuses standalone writes".into(),
            })
        }

        fn read(&self, bus: u8, addr: u8, _buf: &mut [u8]) -> Result<usize> {
            Err(HalError::I2c {
                bus,
                addr,
                detail: "AT24 identity backend requires an explicit offset selector".into(),
            })
        }

        fn write_read(
            &self,
            bus: u8,
            addr: u8,
            write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            self.transfers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((bus, addr, write_data.to_vec(), read_buf.len()));
            if write_data != [0] || read_buf.len() != HASHBOARD_EEPROM_PREFIX_LEN {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "unexpected AT24 identity transfer shape".into(),
                });
            }
            read_buf.fill(0xA5);
            read_buf[..2].copy_from_slice(&[0x04, 0x11]);
            Ok(())
        }

        fn service_identity(&self) -> Option<usize> {
            Some(self.identity)
        }
    }

    #[cfg(feature = "sim-hal")]
    struct Lm75Backend {
        identity: usize,
        transfers: Mutex<Vec<(u8, u8, Vec<u8>, usize)>>,
    }

    #[cfg(feature = "sim-hal")]
    impl Lm75Backend {
        fn new() -> Self {
            Self {
                identity: next_test_service_identity(),
                transfers: Mutex::new(Vec::new()),
            }
        }

        fn transfers(&self) -> Vec<(u8, u8, Vec<u8>, usize)> {
            self.transfers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for Lm75Backend {
        fn write(&self, bus: u8, addr: u8, _data: &[u8]) -> Result<usize> {
            Err(HalError::I2c {
                bus,
                addr,
                detail: "LM75 backend refuses standalone writes".into(),
            })
        }

        fn read(&self, bus: u8, addr: u8, _buf: &mut [u8]) -> Result<usize> {
            Err(HalError::I2c {
                bus,
                addr,
                detail: "LM75 backend requires a repeated-start query".into(),
            })
        }

        fn write_read(
            &self,
            bus: u8,
            addr: u8,
            write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            self.transfers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((bus, addr, write_data.to_vec(), read_buf.len()));
            if write_data != [0x00] || read_buf.len() != 2 {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "unexpected LM75 transfer shape".into(),
                });
            }
            let bytes = match addr {
                0x48 => [0x36, 0x10], // 54.0625 C
                0x49 => [0xf5, 0x80], // -10.5 C
                _ => {
                    return Err(HalError::I2cEndpointNotReady {
                        bus,
                        addr,
                        detail: "simulated unpopulated LM75 endpoint".into(),
                    })
                }
            };
            read_buf.copy_from_slice(&bytes);
            Ok(())
        }

        fn service_identity(&self) -> Option<usize> {
            Some(self.identity)
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn typed_lm75_read_preserves_signed_fractional_register_and_wire_shape() {
        let backend = Arc::new(Lm75Backend::new());
        let service = spawn_sim_i2c_service(25, backend.clone(), Vec::new()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);

        let positive = service
            .read_lm75_temperature_register_at(0x48, deadline)
            .unwrap();
        assert_eq!(positive.raw_be(), i16::from_be_bytes([0x36, 0x10]));
        assert_eq!(positive.celsius(), 54.0625);

        let negative = service
            .read_lm75_temperature_register_at(0x49, deadline)
            .unwrap();
        assert_eq!(negative.raw_be(), i16::from_be_bytes([0xf5, 0x80]));
        assert_eq!(negative.celsius(), -10.5);
        assert_eq!(
            backend.transfers(),
            vec![(25, 0x48, vec![0x00], 2), (25, 0x49, vec![0x00], 2),]
        );

        let invalid = service
            .read_lm75_temperature_register_at(0x47, deadline)
            .unwrap_err();
        assert!(matches!(invalid, HalError::I2cEndpointRefused { .. }));
        let expired = service
            .read_lm75_temperature_register_at(0x4a, Instant::now())
            .unwrap_err();
        assert!(matches!(expired, HalError::I2cEndpointNotReady { .. }));
        assert_eq!(backend.transfers().len(), 2);

        service.latch_terminal_safe_off();
        assert!(service
            .read_lm75_temperature_register_at(0x48, deadline)
            .is_err());
        assert_eq!(backend.transfers().len(), 2);
    }

    #[test]
    fn lm75_endpoint_readiness_does_not_trigger_adapter_recovery() {
        for errno in [libc::ENXIO, libc::EREMOTEIO] {
            let error = map_lm75_read_error(1, 0x4e, std::io::Error::from_raw_os_error(errno));
            assert!(matches!(error, HalError::I2cEndpointNotReady { .. }));
            let result: Result<Lm75TemperatureRegister> = Err(error);
            assert!(!i2c_result_requires_transport_recovery(&result));
        }

        for errno in [libc::EIO, libc::ETIMEDOUT] {
            let error = map_lm75_read_error(1, 0x4e, std::io::Error::from_raw_os_error(errno));
            assert!(matches!(error, HalError::I2c { .. }));
            let result: Result<Lm75TemperatureRegister> = Err(error);
            assert!(i2c_result_requires_transport_recovery(&result));
        }

        let source = include_str!("i2c.rs");
        let typed_start = source
            .find("fn read_lm75_temperature_register")
            .expect("typed LM75 transport");
        let typed_end = source[typed_start..]
            .find("/// Read one fixed AT24")
            .map(|offset| typed_start + offset)
            .expect("end of typed LM75 transport");
        let typed = &source[typed_start..typed_end];
        assert!(typed.contains("write_read_at(addr, &[0x00], &mut bytes, true)"));
        assert!(!typed.contains("set_slave"));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn typed_at24_prefix_read_is_serialized_without_weakening_write_denylist() {
        let backend = Arc::new(At24IdentityBackend::new());
        let denylist = (HASHBOARD_EEPROM_FIRST_ADDR..=HASHBOARD_EEPROM_LAST_ADDR).collect();
        let service = spawn_sim_i2c_service(23, backend.clone(), denylist).unwrap();

        let prefix = service
            .read_hashboard_eeprom_prefix_at(0x52, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(prefix.len(), HASHBOARD_EEPROM_PREFIX_LEN);
        assert_eq!(&prefix[..2], &[0x04, 0x11]);
        assert_eq!(
            backend.transfers(),
            vec![(23, 0x52, vec![0], HASHBOARD_EEPROM_PREFIX_LEN)]
        );

        let error = service
            .write_read(0x52, &[0], HASHBOARD_EEPROM_PREFIX_LEN)
            .expect_err("generic write-read must remain denied at an AT24 address");
        assert!(error.to_string().contains("write denylist"));
        assert_eq!(
            backend.transfers().len(),
            1,
            "a denied generic request must not reach the simulated wire"
        );

        let transaction_error = service
            .transaction(
                0x52,
                vec![I2cTransactionStep::WriteRead {
                    write_data: vec![0],
                    read_len: HASHBOARD_EEPROM_PREFIX_LEN,
                }],
            )
            .expect_err("generic compound write-read must remain denied");
        assert!(transaction_error.to_string().contains("write denylist"));
        assert_eq!(backend.transfers().len(), 1);

        assert!(service
            .read_hashboard_eeprom_prefix_at(0x58, Instant::now() + Duration::from_secs(1))
            .is_err());
        assert_eq!(
            backend.transfers().len(),
            1,
            "an invalid AT24 address must fail before queue admission"
        );

        let s9_backend = Arc::new(At24IdentityBackend::new());
        let s9_service = spawn_sim_i2c_service(24, s9_backend.clone(), Vec::new()).unwrap();
        let policy_error = s9_service
            .read_hashboard_eeprom_prefix_at(0x55, Instant::now() + Duration::from_secs(1))
            .expect_err("an S9/no-policy service must not gain an AT24 write-shaped exception");
        assert!(policy_error.to_string().contains("not admitted"));
        assert!(matches!(policy_error, HalError::I2cEndpointRefused { .. }));
        assert!(
            s9_backend.transfers().is_empty(),
            "policy refusal must happen before an S9 PIC address reaches wire"
        );

        let expired_error = service
            .read_hashboard_eeprom_prefix_at(0x52, Instant::now())
            .expect_err("an expired deadline must refuse admission");
        assert!(matches!(
            expired_error,
            HalError::I2cEndpointNotReady { .. }
        ));
        assert_eq!(backend.transfers().len(), 1);
    }

    #[test]
    fn at24_readiness_and_policy_failures_never_trigger_transport_recovery() {
        let eio = map_at24_read_error(
            0,
            0x50,
            "/sys/bus/i2c/devices/0-0050/eeprom",
            std::io::Error::from_raw_os_error(libc::EIO),
        );
        assert!(matches!(eio, HalError::I2cEndpointNotReady { .. }));
        let eio_result: Result<()> = Err(eio);
        assert!(!i2c_result_requires_transport_recovery(&eio_result));

        let permission = map_at24_read_error(
            0,
            0x50,
            "/sys/bus/i2c/devices/0-0050/eeprom",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(permission, HalError::I2cEndpointRefused { .. }));
        let permission_result: Result<()> = Err(permission);
        assert!(!i2c_result_requires_transport_recovery(&permission_result));
    }

    #[test]
    fn service_generation_token_is_invalid_after_terminal_safe_off() {
        let (handle, _receiver) = I2cServiceHandle::for_unit_tests();
        let token = handle.capture_generation_token().unwrap();

        assert!(token.is_current());
        handle.latch_terminal_safe_off();
        assert!(!token.is_current());
        assert!(handle.capture_generation_token().is_err());
    }

    #[test]
    fn replacement_service_rejects_same_numbered_generation_before_queueing() {
        let (old_handle, _old_receiver) = I2cServiceHandle::for_unit_tests();
        let old_token = old_handle.capture_generation_token().unwrap();
        let (replacement, replacement_receiver) = I2cServiceHandle::for_unit_tests();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::Heartbeat {
            addr: 0x55,
            firmware: I2cPicFirmware::Unknown,
            reply_tx,
        };

        let error = replacement
            .submit_at_generation(
                0x55,
                I2cOperationIntent::KeepAlive,
                &old_token,
                request,
                reply_rx,
            )
            .unwrap_err();

        assert_eq!(old_token.generation, 0);
        assert_eq!(replacement.safety.generation.load(Ordering::SeqCst), 0);
        assert!(matches!(error, HalError::I2cSafetySuperseded { .. }));
        assert!(error.to_string().contains("different I2C service lifetime"));
        assert!(replacement_receiver.try_recv().is_err());
    }

    #[test]
    fn worker_death_revokes_tokens_and_future_mutation_admission() {
        let (handle, _receiver) = I2cServiceHandle::for_unit_tests();
        let token = handle.capture_generation_token().unwrap();

        handle.safety.mark_worker_dead();

        assert!(!token.is_current());
        assert!(handle.terminal_safe_off_is_latched());
        let error = handle.capture_generation_token().unwrap_err();
        assert!(error
            .to_string()
            .contains("I2C service worker is no longer alive"));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn pic16_generation_primitives_reject_stale_token_without_bus_io() {
        use crate::platform::sim::{SimModel, SimPlatform};

        let platform = SimPlatform::new(SimModel::S9);
        let handle = platform.open_i2c_service(0).unwrap();
        let endpoint = platform.pic16_endpoint(0, 0x55).unwrap();
        let stale = handle.capture_generation_token().unwrap();

        handle
            .disable_voltage(0x55, I2cPicFirmware::Unknown)
            .unwrap();
        let _safe_off_trace = platform.drain_i2c_trace().unwrap();
        assert!(!stale.is_current());

        let errors = [
            handle
                .pic16_jump_if_exact_bootloader_with_generation(&endpoint, &stale)
                .map(|_| ()),
            handle.pic16_heartbeat_with_generation(&endpoint, &stale),
            handle.pic16_set_and_enable_with_generation(&endpoint, &stale, 0x80),
        ];
        for error in errors.into_iter().map(Result::unwrap_err) {
            assert!(matches!(error, HalError::I2cSafetySuperseded { .. }));
        }
        assert!(
            platform.drain_i2c_trace().unwrap().is_empty(),
            "stale generation-bound protocol primitives must fail before bus I/O"
        );

        let current = handle.capture_generation_token().unwrap();
        assert_eq!(current.generation, stale.generation + 1);
        handle
            .pic16_heartbeat_with_generation(&endpoint, &current)
            .unwrap();
        assert!(!platform.drain_i2c_trace().unwrap().is_empty());
    }

    #[cfg(feature = "sim-hal")]
    #[derive(Default)]
    struct ShortWriteBackend {
        writes: AtomicUsize,
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for ShortWriteBackend {
        fn write(&self, _bus: u8, _addr: u8, data: &[u8]) -> Result<usize> {
            let ordinal = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if ordinal == 2 {
                Ok(0)
            } else {
                Ok(data.len())
            }
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            _read_buf: &mut [u8],
        ) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn bytewise_write_rejects_short_backend_completion() {
        let backend = Arc::new(ShortWriteBackend::default());
        let mut bus = I2cBus::open_sim(0, backend.clone());
        bus.set_slave(0x55).unwrap();

        let error = bus
            .write_byte_by_byte(&[0x55, 0xAA, 0x16])
            .expect_err("zero-byte completion must not prove a PIC16 command");

        assert!(matches!(error, HalError::I2c { .. }));
        assert!(error.to_string().contains("short write"));
        assert_eq!(
            backend.writes.load(Ordering::SeqCst),
            2,
            "bytewise transport must stop at the first unproven byte"
        );
    }

    #[test]
    fn devmem_timeout_noop_cannot_prove_a_kernel_style_bound() {
        let bus = I2cBus::open_devmem();

        bus.set_timeout(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES)
            .unwrap();

        assert!(!bus.timeout_is_verified(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES));
    }

    #[cfg(feature = "sim-hal")]
    struct PanicOnWriteBackend {
        writes: AtomicUsize,
        service_identity: usize,
    }

    #[cfg(feature = "sim-hal")]
    impl Default for PanicOnWriteBackend {
        fn default() -> Self {
            Self {
                writes: AtomicUsize::new(0),
                service_identity: next_test_service_identity(),
            }
        }
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for PanicOnWriteBackend {
        fn service_identity(&self) -> Option<usize> {
            Some(self.service_identity)
        }

        fn write(&self, _bus: u8, _addr: u8, _data: &[u8]) -> Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            panic!("injected simulated I2C worker panic");
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            _read_buf: &mut [u8],
        ) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn actual_worker_unwind_revokes_generation_and_future_mutations() {
        let backend = Arc::new(PanicOnWriteBackend::default());
        let handle = spawn_sim_i2c_service(0, backend.clone(), Vec::new()).unwrap();
        let token = handle.capture_generation_token().unwrap();

        assert!(handle.heartbeat(0x55, I2cPicFirmware::Unknown).is_err());
        for _ in 0..100 {
            if handle.terminal_safe_off_is_latched() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(handle.terminal_safe_off_is_latched());
        assert!(!token.is_current());
        assert!(handle.capture_generation_token().is_err());
        assert!(handle.heartbeat(0x55, I2cPicFirmware::Unknown).is_err());
        assert_eq!(backend.writes.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn lost_heartbeat_reply_receiver_triggers_worker_owned_batch_safe_off() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let handle = spawn_sim_i2c_service(0, Arc::new(backend.clone()), Vec::new()).unwrap();
        handle
            .set_voltage(0x55, I2cPicFirmware::Unknown, 0x80)
            .unwrap();
        assert!(backend.pic16_snapshot(0, 0x55).unwrap().voltage_enabled());

        let batch = handle.safety.publish_pic16_batch(0, vec![0x55]).unwrap();
        batch.install_runtime_liveness(Vec::new()).unwrap();
        let generation = handle.safety.generation.load(Ordering::SeqCst);
        let request_state = Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED));
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        drop(reply_rx);
        let envelope = I2cServiceEnvelope {
            must_start_by: Instant::now() + I2C_QUEUE_START_BUDGET,
            state: Arc::clone(&request_state),
            permit: I2cSafetyPermit {
                authority: Arc::clone(&handle.safety),
                intent: I2cOperationIntent::KeepAlive,
                generation,
                scope: I2cPermitScope::Pic16RuntimeBatch {
                    epoch: batch.epoch(),
                    batch: Arc::clone(&batch),
                },
            },
            request: I2cRequest::Pic16HeartbeatRound {
                batch: Arc::clone(&batch),
                reply_tx,
            },
        };
        match &handle.tx {
            I2cServiceSender::Deadline(tx) => tx.send(envelope).unwrap(),
            I2cServiceSender::Raw(_) => unreachable!("simulated services use deadline envelopes"),
        }

        for _ in 0..2_000 {
            if batch.released() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(batch.released(), "worker did not complete batch SafeOff");
        assert!(!backend.pic16_snapshot(0, 0x55).unwrap().voltage_enabled());
        assert!(handle.safety.generation.load(Ordering::SeqCst) > generation);
        assert_eq!(request_state.load(Ordering::Acquire), I2C_REQUEST_FINISHED);
    }

    #[cfg(feature = "sim-hal")]
    struct TimeoutRecordingBackend {
        timeouts: std::sync::Mutex<Vec<u32>>,
        fail_write: AtomicU8,
        service_identity: usize,
    }

    #[cfg(feature = "sim-hal")]
    impl Default for TimeoutRecordingBackend {
        fn default() -> Self {
            Self {
                timeouts: std::sync::Mutex::new(Vec::new()),
                fail_write: AtomicU8::new(0),
                service_identity: next_test_service_identity(),
            }
        }
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for TimeoutRecordingBackend {
        fn service_identity(&self) -> Option<usize> {
            Some(self.service_identity)
        }

        fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            if self.fail_write.load(Ordering::Acquire) != 0 {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "injected write failure".into(),
                });
            }
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }

        fn set_timeout(&self, _bus: u8, timeout_jiffies: u32) -> Result<()> {
            self.timeouts.lock().unwrap().push(timeout_jiffies);
            Ok(())
        }
    }

    #[cfg(feature = "sim-hal")]
    #[derive(Default)]
    struct ConditionalPlanBackend {
        writes: std::sync::Mutex<Vec<Vec<u8>>>,
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for ConditionalPlanBackend {
        fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            self.writes.lock().unwrap().push(data.to_vec());
            if data.first() == Some(&0xB0) {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "injected primary failure".into(),
                });
            }
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }
    }

    #[cfg(feature = "sim-hal")]
    #[derive(Default)]
    struct RecoverOnceBackend {
        writes: AtomicUsize,
        recoveries: AtomicUsize,
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for RecoverOnceBackend {
        fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.recoveries.load(Ordering::SeqCst) == 0 {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "stale simulated transport".into(),
                });
            }
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            _read_buf: &mut [u8],
        ) -> Result<()> {
            Ok(())
        }

        fn bus_recovery(&self, _bus: u8) {
            self.recoveries.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[cfg(feature = "sim-hal")]
    struct FourthWriteGateBackend {
        write_count: AtomicUsize,
        writes: std::sync::Mutex<Vec<Vec<u8>>>,
        reached_tx: std::sync::Mutex<Option<mpsc::SyncSender<()>>>,
        release_rx: std::sync::Mutex<mpsc::Receiver<()>>,
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for FourthWriteGateBackend {
        fn write(&self, _bus: u8, _addr: u8, data: &[u8]) -> Result<usize> {
            let ordinal = self.write_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.writes.lock().unwrap().push(data.to_vec());
            if ordinal == 4 {
                if let Some(reached_tx) = self.reached_tx.lock().unwrap().take() {
                    reached_tx.send(()).unwrap();
                }
                self.release_rx.lock().unwrap().recv().unwrap();
            }
            Ok(data.len())
        }

        fn read(&self, _bus: u8, _addr: u8, buf: &mut [u8]) -> Result<usize> {
            buf.fill(0);
            Ok(buf.len())
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }
    }

    #[cfg(feature = "sim-hal")]
    struct Pic16BootTransitionBackend {
        raw_state: AtomicU8,
        post_jump_raw_state: AtomicU8,
        read_count: AtomicUsize,
        short_read: AtomicBool,
        short_post_jump_read: AtomicBool,
        fail_read: AtomicBool,
        fail_post_jump_read: AtomicBool,
        fail_write_ordinal: AtomicUsize,
        write_count: AtomicUsize,
        writes: std::sync::Mutex<Vec<Vec<u8>>>,
        read_reached_tx: std::sync::Mutex<Option<mpsc::SyncSender<()>>>,
        read_release_rx: std::sync::Mutex<Option<mpsc::Receiver<()>>>,
        write_reached_tx: std::sync::Mutex<Option<mpsc::SyncSender<()>>>,
        write_release_rx: std::sync::Mutex<Option<mpsc::Receiver<()>>>,
    }

    #[cfg(feature = "sim-hal")]
    impl Pic16BootTransitionBackend {
        fn with_raw_state(raw_state: u8) -> Self {
            Self {
                raw_state: AtomicU8::new(raw_state),
                post_jump_raw_state: AtomicU8::new(raw_state),
                read_count: AtomicUsize::new(0),
                short_read: AtomicBool::new(false),
                short_post_jump_read: AtomicBool::new(false),
                fail_read: AtomicBool::new(false),
                fail_post_jump_read: AtomicBool::new(false),
                fail_write_ordinal: AtomicUsize::new(0),
                write_count: AtomicUsize::new(0),
                writes: std::sync::Mutex::new(Vec::new()),
                read_reached_tx: std::sync::Mutex::new(None),
                read_release_rx: std::sync::Mutex::new(None),
                write_reached_tx: std::sync::Mutex::new(None),
                write_release_rx: std::sync::Mutex::new(None),
            }
        }

        fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
    }

    #[cfg(feature = "sim-hal")]
    impl I2cSimBackend for Pic16BootTransitionBackend {
        fn write(&self, bus: u8, addr: u8, data: &[u8]) -> Result<usize> {
            let ordinal = self.write_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.writes.lock().unwrap().push(data.to_vec());
            if ordinal == 1 {
                if let Some(reached_tx) = self.write_reached_tx.lock().unwrap().take() {
                    reached_tx.send(()).unwrap();
                }
                if let Some(release_rx) = self.write_release_rx.lock().unwrap().take() {
                    release_rx.recv().unwrap();
                }
            }
            if ordinal == self.fail_write_ordinal.load(Ordering::SeqCst) {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: format!("injected PIC16 write failure at byte {ordinal}"),
                });
            }
            Ok(data.len())
        }

        fn read(&self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<usize> {
            let ordinal = self.read_count.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_read.load(Ordering::SeqCst)
                || (ordinal > 1 && self.fail_post_jump_read.load(Ordering::SeqCst))
            {
                return Err(HalError::I2c {
                    bus,
                    addr,
                    detail: "injected PIC16 boot-state read failure".into(),
                });
            }
            if let Some(first) = buf.first_mut() {
                *first = if ordinal == 1 {
                    self.raw_state.load(Ordering::SeqCst)
                } else {
                    self.post_jump_raw_state.load(Ordering::SeqCst)
                };
            }
            if let Some(reached_tx) = self.read_reached_tx.lock().unwrap().take() {
                reached_tx.send(()).unwrap();
            }
            if let Some(release_rx) = self.read_release_rx.lock().unwrap().take() {
                release_rx.recv().unwrap();
            }
            Ok(
                if self.short_read.load(Ordering::SeqCst)
                    || (ordinal > 1 && self.short_post_jump_read.load(Ordering::SeqCst))
                {
                    0
                } else {
                    buf.len()
                },
            )
        }

        fn write_read(
            &self,
            _bus: u8,
            _addr: u8,
            _write_data: &[u8],
            read_buf: &mut [u8],
        ) -> Result<()> {
            read_buf.fill(0);
            Ok(())
        }
    }

    fn heartbeat_request() -> (I2cRequest, mpsc::Receiver<Result<()>>) {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        (
            I2cRequest::Heartbeat {
                addr: 0x55,
                firmware: I2cPicFirmware::Stock,
                reply_tx,
            },
            reply_rx,
        )
    }

    fn test_permit(intent: I2cOperationIntent) -> I2cSafetyPermit {
        I2cSafetyPermit {
            authority: Arc::new(I2cSafetyAuthority::default()),
            intent,
            generation: 0,
            scope: I2cPermitScope::Generic,
        }
    }

    fn envelope(request: I2cRequest, must_start_by: Instant) -> I2cServiceEnvelope {
        I2cServiceEnvelope {
            must_start_by,
            state: Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED)),
            permit: test_permit(I2cOperationIntent::KeepAlive),
            request,
        }
    }

    #[test]
    fn transaction_execution_budget_accounts_for_max_reset_dwell_and_io() {
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::Transaction {
            addr: 0x55,
            steps: vec![
                I2cTransactionStep::SetTimeout(10),
                I2cTransactionStep::WriteByteByByte(vec![0x55, 0xAA, 0x07, 0, 0, 0]),
                I2cTransactionStep::SleepMs(5_000),
                I2cTransactionStep::Read(4),
            ],
            reply_tx,
        };

        let budget = request_execution_budget(&request);
        assert!(budget >= Duration::from_millis(6_716));
        assert!(I2cRequestBudget::for_request(&request).is_some());
    }

    #[test]
    fn transaction_execution_budget_tracks_set_timeout_for_following_steps() {
        let short = transaction_execution_budget(&[
            I2cTransactionStep::SetTimeout(1),
            I2cTransactionStep::Read(1),
        ]);
        let long = transaction_execution_budget(&[
            I2cTransactionStep::SetTimeout(10),
            I2cTransactionStep::Read(1),
        ]);
        assert_eq!(long - short, Duration::from_millis(90));
    }

    #[test]
    fn oversized_transaction_budget_is_rejected_without_instant_overflow() {
        let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
        let request = I2cRequest::Transaction {
            addr: 0x55,
            steps: vec![I2cTransactionStep::SleepMs(u64::MAX)],
            reply_tx,
        };
        assert!(I2cRequestBudget::for_request(&request).is_none());
    }

    #[test]
    fn standalone_timeout_cannot_drift_from_service_default() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let rendered = handle.set_timeout(500).unwrap_err().to_string();
        assert!(
            rendered.contains("transaction-scoped SetTimeout"),
            "{rendered}"
        );
        assert!(
            rx.try_recv().is_err(),
            "invalid timeout must not be submitted"
        );
    }

    #[test]
    fn terminal_safe_off_is_irreversible_and_shared_by_all_handle_clones() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let clone = handle.clone();

        let first = handle.latch_terminal_safe_off();
        let second = clone.latch_terminal_safe_off();

        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, first.generation);
        assert!(first.no_controller_mutation_stage_in_flight);
        assert!(clone.terminal_safe_off_is_latched());

        let rendered = clone
            .set_voltage(0x55, I2cPicFirmware::Stock, 6)
            .unwrap_err()
            .to_string();
        assert!(
            rendered.contains("terminal safe-off is latched"),
            "{rendered}"
        );
        assert!(
            rx.try_recv().is_err(),
            "post-terminal energize must not enter the service queue"
        );
    }

    #[test]
    fn every_public_mutation_label_is_terminal_fenced() {
        for label in [
            I2cMutationLabel::KeepAlive,
            I2cMutationLabel::Energize,
            I2cMutationLabel::NeutralControl,
            I2cMutationLabel::Recovery,
            I2cMutationLabel::QueryPrelude,
            I2cMutationLabel::Unclassified,
        ] {
            let (handle, rx) = I2cServiceHandle::for_unit_tests();
            handle.latch_terminal_safe_off();
            let error = handle
                .transaction_mutating(label, 0x20, vec![I2cTransactionStep::Write(vec![0xFF])])
                .unwrap_err();
            assert!(
                error.to_string().contains("terminal safe-off is latched"),
                "label={label:?}, error={error}"
            );
            assert!(
                rx.try_recv().is_err(),
                "label={label:?} must not enter the queue after terminal safe-off"
            );
        }
    }

    #[test]
    fn caller_supplied_privileged_intent_surface_stays_crate_private() {
        let source = include_str!("i2c.rs");
        let intent_visibility = ["pub(crate)", " enum I2cOperationIntent"].concat();
        assert!(source
            .lines()
            .any(|line| line.trim_start().starts_with(&intent_visibility)));
        for suffix in [
            "fn write_bytes_with_intent",
            "fn write_byte_by_byte_with_intent",
            "fn write_read_with_intent",
            "fn transaction_with_intent",
            "async fn transaction_with_intent",
        ] {
            let signature = format!("pub(crate) {suffix}");
            assert!(
                source
                    .lines()
                    .any(|line| line.trim_start().starts_with(&signature)),
                "missing boundary: {signature}"
            );
        }
        let public_intent = ["pub", " enum I2cOperationIntent"].concat();
        let public_executor = ["pub", " fn transaction_with_intent"].concat();
        assert!(!source
            .lines()
            .any(|line| line.trim_start().starts_with(&public_intent)));
        assert!(!source
            .lines()
            .any(|line| line.trim_start().starts_with(&public_executor)));
    }

    #[test]
    fn pic16_runtime_authority_surface_stays_batch_scoped() {
        let service_source = include_str!("i2c.rs");
        let admission_source = include_str!("i2c/pic16_admission.rs");

        let required_boundaries = [
            ["pub struct Pic16Admitted", "Batch"].concat(),
            ["pub struct Pic16RuntimeEndpoint", "Id"].concat(),
            ["pub fn pic16_heartbeat_", "round("].concat(),
            ["pub fn pic16_set_voltage_", "in_batch("].concat(),
            ["Pic16Heartbeat", "Round {"].concat(),
        ];
        for required in required_boundaries {
            assert!(
                service_source.contains(&required) || admission_source.contains(&required),
                "missing batch-scoped PIC16 runtime boundary: {required}"
            );
        }
        let forbidden_boundaries = [
            ["pub struct Pic16Admission", "Receipt"].concat(),
            ["pub struct Pic16Admission", "Outcome"].concat(),
            ["pub fn receipt", "s("].concat(),
            ["pub fn into_", "receipts("].concat(),
            ["pub fn pic16_heartbeat_with_", "receipt("].concat(),
            ["pub fn pic16_set_voltage_with_", "receipt("].concat(),
            ["fn pic16_heartbeat_", "in_batch("].concat(),
        ];
        for forbidden in forbidden_boundaries {
            assert!(
                !service_source.contains(&forbidden) && !admission_source.contains(&forbidden),
                "fragmentable PIC16 runtime authority resurfaced: {forbidden}"
            );
        }
    }

    #[test]
    fn typed_dspic_safe_off_has_fixed_wire_bytes_and_survives_terminal_latch() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        handle.latch_terminal_safe_off();
        let caller = std::thread::spawn(move || {
            handle.disable_dspic_voltage(0x21, I2cDspicDisableProtocol::VnishPaddedFramed)
        });
        let request = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let I2cRequest::Transaction {
            addr,
            steps,
            reply_tx,
        } = request
        else {
            panic!("expected typed dsPIC safe-off transaction")
        };
        assert_eq!(addr, 0x21);
        assert_eq!(
            steps,
            vec![I2cTransactionStep::Write(vec![
                0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A,
            ])]
        );
        reply_tx.send(Ok(Vec::new())).unwrap();
        caller.join().unwrap().unwrap();
    }

    #[test]
    fn typed_safe_off_rejects_invalid_protocol_addresses_before_queueing() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        assert!(handle.disable_voltage(0x50, I2cPicFirmware::Stock).is_err());
        assert!(handle.disable_voltage(0x20, I2cPicFirmware::Stock).is_err());
        assert!(handle
            .enqueue_reserved_disable(0x50, I2cPicFirmware::Stock)
            .is_err());
        assert!(handle
            .disable_dspic_voltage(0x50, I2cDspicDisableProtocol::Bare)
            .is_err());
        assert!(handle.disable_pic1704_dc_dc(0x21).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn typed_voltage_operations_reject_unrelated_slave_addresses_before_queueing() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();

        assert!(handle.heartbeat(0x50, I2cPicFirmware::Stock).is_err());
        assert!(handle
            .set_voltage(0x50, I2cPicFirmware::Stock, 0x80)
            .is_err());
        assert!(handle.set_voltage_mv(0x50, 1_000).is_err());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let async_handle = handle.async_handle();
        assert!(runtime
            .block_on(async_handle.heartbeat(0x50, I2cPicFirmware::Stock))
            .is_err());
        assert!(runtime
            .block_on(async_handle.set_voltage(0x50, I2cPicFirmware::Stock, 0x80))
            .is_err());
        assert!(runtime
            .block_on(async_handle.set_voltage_mv(0x50, 1_000))
            .is_err());

        assert!(
            rx.try_recv().is_err(),
            "protocol-scoped voltage commands must never reach unrelated I2C slaves"
        );
    }

    #[test]
    fn oversized_public_service_messages_are_rejected_before_queue_admission() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let oversized = I2C_MAX_MESSAGE_BYTES + 1;

        assert!(handle.write_bytes(0x20, &vec![0u8; oversized]).is_err());
        assert!(handle.read_bytes(0x20, oversized).is_err());
        assert!(handle.write_read(0x20, &[0x00], oversized).is_err());
        assert!(handle
            .transaction(0x20, vec![I2cTransactionStep::Read(oversized)])
            .is_err());
        assert!(handle
            .transaction(0x20, vec![I2cTransactionStep::Write(vec![0u8; oversized])],)
            .is_err());
        assert!(handle
            .transaction(
                0x20,
                vec![I2cTransactionStep::WriteRead {
                    write_data: vec![0x00],
                    read_len: oversized,
                }],
            )
            .is_err());
        assert!(
            rx.try_recv().is_err(),
            "oversized service work must never enter the worker queue"
        );
    }

    #[test]
    fn oversized_transaction_step_plans_are_rejected_before_queue_admission() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let oversized = vec![I2cTransactionStep::SleepMs(0); I2C_MAX_TRANSACTION_STEPS + 1];

        assert!(handle.transaction(0x20, oversized.clone()).is_err());
        assert!(handle
            .safe_off_transaction(0x20, oversized.clone())
            .is_err());

        let phase = vec![I2cTransactionStep::SetTimeout(1); 22];
        assert!(handle
            .conditional_safe_off_plan(0x20, phase.clone(), phase.clone(), phase)
            .is_err());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime
            .block_on(handle.async_handle().transaction(0x20, oversized))
            .is_err());

        assert!(
            rx.try_recv().is_err(),
            "oversized transaction plans must never enter the worker queue"
        );
    }

    #[test]
    fn terminal_transition_reports_preauthorized_controller_stage() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Energize,
            generation: authority.capture(I2cOperationIntent::Energize).unwrap(),
            scope: I2cPermitScope::Generic,
        };

        let stage = permit.begin_stage(0, 0x55, "test stage").unwrap();
        let transition = authority.latch_terminal_safe_off();
        assert!(!transition.no_controller_mutation_stage_in_flight);
        drop(stage);
        let rendered = permit
            .begin_stage(0, 0x55, "later stage")
            .err()
            .expect("stale stage must be rejected")
            .to_string();
        assert!(rendered.contains("newer safe-off barrier"), "{rendered}");
    }

    #[test]
    fn safety_supersession_is_not_a_transport_recovery_failure() {
        let superseded: Result<()> = Err(HalError::I2cSafetySuperseded {
            bus: 0,
            addr: 0x55,
            detail: "test generation barrier".into(),
        });
        let transport: Result<()> = Err(HalError::I2c {
            bus: 0,
            addr: 0x55,
            detail: "test NACK".into(),
        });

        assert!(!i2c_result_requires_transport_recovery(&superseded));
        assert!(i2c_result_requires_transport_recovery(&transport));
        assert!(!i2c_result_requires_transport_recovery(&Ok(())));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn exact_pic16_bootloader_observation_emits_only_the_fixed_jump_frame() {
        let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
        backend.post_jump_raw_state.store(0x03, Ordering::SeqCst);
        let mut bus = I2cBus::open_sim(0, backend.clone());
        let permit = test_permit(I2cOperationIntent::Recovery);

        let outcome = execute_pic16_jump_if_bootloader(&mut bus, 0x55, &permit).unwrap();

        assert_eq!(
            outcome,
            I2cPic16JumpOutcome::JumpSentFromExactBootloader {
                post_jump_raw_state: 0x03
            }
        );
        assert_eq!(backend.writes(), vec![vec![0x55], vec![0xAA], vec![0x06]]);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn every_non_bootloader_pic16_byte_is_observation_only() {
        for raw_state in u8::MIN..=u8::MAX {
            if raw_state == 0xCC {
                continue;
            }
            let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(raw_state));
            let mut bus = I2cBus::open_sim(0, backend.clone());
            let permit = test_permit(I2cOperationIntent::Recovery);

            assert_eq!(
                execute_pic16_jump_if_bootloader(&mut bus, 0x56, &permit).unwrap(),
                I2cPic16JumpOutcome::ObservedNoJump { raw_state }
            );
            assert!(
                backend.writes().is_empty(),
                "raw state 0x{raw_state:02X} must not emit any transition byte"
            );
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn short_or_failed_pic16_observation_cannot_emit_jump() {
        for fail_read in [false, true] {
            let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
            backend.short_read.store(!fail_read, Ordering::SeqCst);
            backend.fail_read.store(fail_read, Ordering::SeqCst);
            let mut bus = I2cBus::open_sim(0, backend.clone());
            let permit = test_permit(I2cOperationIntent::Recovery);

            assert!(execute_pic16_jump_if_bootloader(&mut bus, 0x57, &permit).is_err());
            assert!(backend.writes().is_empty());
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn short_or_failed_post_jump_observation_never_returns_transition_success() {
        for fail_read in [false, true] {
            let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
            backend
                .short_post_jump_read
                .store(!fail_read, Ordering::SeqCst);
            backend
                .fail_post_jump_read
                .store(fail_read, Ordering::SeqCst);
            let mut bus = I2cBus::open_sim(0, backend.clone());
            let permit = test_permit(I2cOperationIntent::Recovery);

            assert!(execute_pic16_jump_if_bootloader(&mut bus, 0x57, &permit).is_err());
            assert_eq!(backend.writes(), vec![vec![0x55], vec![0xAA], vec![0x06]]);
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn failed_pic16_jump_flushes_parser_and_never_reports_success() {
        let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
        backend.fail_write_ordinal.store(2, Ordering::SeqCst);
        let mut bus = I2cBus::open_sim(0, backend.clone());
        let permit = test_permit(I2cOperationIntent::Recovery);

        assert!(execute_pic16_jump_if_bootloader(&mut bus, 0x55, &permit).is_err());
        let writes = backend.writes();
        assert_eq!(&writes[..2], &[vec![0x55], vec![0xAA]]);
        assert_eq!(writes[2..], vec![vec![0_u8]; 16]);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn terminal_barrier_during_pic16_observation_suppresses_jump() {
        let (reached_tx, reached_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
        *backend.read_reached_tx.lock().unwrap() = Some(reached_tx);
        *backend.read_release_rx.lock().unwrap() = Some(release_rx);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Recovery,
            generation: authority.capture(I2cOperationIntent::Recovery).unwrap(),
            scope: I2cPermitScope::Generic,
        };
        let worker_backend = backend.clone();
        let worker = std::thread::spawn(move || {
            let mut bus = I2cBus::open_sim(0, worker_backend);
            execute_pic16_jump_if_bootloader(&mut bus, 0x55, &permit)
        });

        reached_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("PIC16 raw read did not reach the injected barrier");
        let transition = authority.latch_terminal_safe_off();
        assert!(!transition.no_controller_mutation_stage_in_flight());
        release_tx.send(()).unwrap();

        let error = worker.join().unwrap().unwrap_err();
        assert!(matches!(error, HalError::I2cSafetySuperseded { .. }));
        assert!(backend.writes().is_empty());
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn terminal_barrier_after_pic16_jump_stage_reports_in_flight_frame() {
        let (reached_tx, reached_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let backend = Arc::new(Pic16BootTransitionBackend::with_raw_state(0xCC));
        *backend.write_reached_tx.lock().unwrap() = Some(reached_tx);
        *backend.write_release_rx.lock().unwrap() = Some(release_rx);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Recovery,
            generation: authority.capture(I2cOperationIntent::Recovery).unwrap(),
            scope: I2cPermitScope::Generic,
        };
        let worker_backend = backend.clone();
        let worker = std::thread::spawn(move || {
            let mut bus = I2cBus::open_sim(0, worker_backend);
            execute_pic16_jump_if_bootloader(&mut bus, 0x55, &permit)
        });

        reached_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("PIC16 JUMP did not reach the injected barrier");
        let transition = authority.latch_terminal_safe_off();
        assert!(!transition.no_controller_mutation_stage_in_flight());
        release_tx.send(()).unwrap();

        let error = worker.join().unwrap().unwrap_err();
        assert!(matches!(error, HalError::I2cSafetySuperseded { .. }));
        assert_eq!(backend.writes(), vec![vec![0x55], vec![0xAA], vec![0x06]]);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn started_set_voltage_cannot_enable_after_terminal_safe_off() {
        let (reached_tx, reached_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let backend = Arc::new(FourthWriteGateBackend {
            write_count: AtomicUsize::new(0),
            writes: std::sync::Mutex::new(Vec::new()),
            reached_tx: std::sync::Mutex::new(Some(reached_tx)),
            release_rx: std::sync::Mutex::new(release_rx),
        });
        let authority = Arc::new(I2cSafetyAuthority::default());
        let permit = I2cSafetyPermit {
            authority: Arc::clone(&authority),
            intent: I2cOperationIntent::Energize,
            generation: authority.capture(I2cOperationIntent::Energize).unwrap(),
            scope: I2cPermitScope::Generic,
        };
        let worker_backend = Arc::clone(&backend);
        let worker = std::thread::spawn(move || {
            let mut bus = I2cBus::open_sim(0, worker_backend);
            execute_set_voltage(&mut bus, 0x55, I2cPicFirmware::Stock, 6, &permit)
        });

        reached_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("SET_VOLTAGE frame did not reach its final byte");
        let transition = authority.latch_terminal_safe_off();
        assert!(
            !transition.no_controller_mutation_stage_in_flight,
            "the first controller stage is still blocked in the backend"
        );
        release_tx.send(()).unwrap();

        let rendered = worker.join().unwrap().unwrap_err().to_string();
        assert!(rendered.contains("before PIC ENABLE frame"), "{rendered}");
        let writes = backend.writes.lock().unwrap().clone();
        assert_eq!(
            writes,
            vec![vec![0x55], vec![0xAA], vec![0x10], vec![6]],
            "no byte of the ENABLE frame may reach the backend after the barrier"
        );
    }

    #[test]
    fn deadline_boundary_rejects_request_before_execution() {
        let now = Instant::now();
        let (request, reply_rx) = heartbeat_request();
        let state = Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED));
        let envelope = I2cServiceEnvelope {
            must_start_by: now,
            state: Arc::clone(&state),
            permit: test_permit(I2cOperationIntent::KeepAlive),
            request,
        };

        assert!(start_envelope_at(envelope, 7, now).is_none());
        assert_eq!(state.load(Ordering::Acquire), I2C_REQUEST_CANCELLED);
        let error = reply_rx.recv().expect("worker must reply").unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("bus 7"), "{rendered}");
        assert!(rendered.contains("before execution"), "{rendered}");
    }

    #[test]
    fn worker_start_preserves_typed_safety_supersession() {
        let authority = Arc::new(I2cSafetyAuthority::default());
        let (request, reply_rx) = heartbeat_request();
        let state = Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED));
        let envelope = I2cServiceEnvelope {
            must_start_by: Instant::now() + Duration::from_secs(1),
            state: Arc::clone(&state),
            permit: I2cSafetyPermit {
                authority: Arc::clone(&authority),
                intent: I2cOperationIntent::KeepAlive,
                generation: 0,
                scope: I2cPermitScope::Generic,
            },
            request,
        };
        authority.advance_safe_off_generation();

        assert!(start_envelope_at(envelope, 0, Instant::now()).is_none());
        assert_eq!(state.load(Ordering::Acquire), I2C_REQUEST_CANCELLED);
        assert!(matches!(
            reply_rx.recv().expect("worker must reply").unwrap_err(),
            HalError::I2cSafetySuperseded { .. }
        ));
    }

    #[test]
    fn full_service_queue_returns_not_admitted_and_preserves_existing_request() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (existing, _existing_reply_rx) = heartbeat_request();
        tx.send(envelope(existing, Instant::now() + Duration::from_secs(1)))
            .unwrap();
        let handle = I2cServiceHandle {
            bus: 7,
            tx: I2cServiceSender::Deadline(tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: None,
        };
        let (request, reply_rx) = heartbeat_request();
        let result = handle.submit_with_budget(
            0x55,
            request,
            reply_rx,
            I2cRequestBudget {
                admission: Duration::from_millis(5),
                start: Duration::from_millis(10),
                execution: Duration::from_millis(10),
            },
        );

        let rendered = result.unwrap_err().to_string();
        assert!(rendered.contains("bus 7"), "{rendered}");
        assert!(rendered.contains("not admitted"), "{rendered}");
        assert!(rx.try_recv().is_ok(), "original request must remain queued");
        assert!(
            rx.try_recv().is_err(),
            "timed-out request must not be queued"
        );
    }

    #[test]
    fn reserved_safe_off_bypasses_full_normal_queue_and_coalesces_waiters() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (existing, _existing_reply_rx) = heartbeat_request();
        tx.send(envelope(existing, Instant::now() + Duration::from_secs(10)))
            .unwrap();

        let mailbox = Arc::new(I2cSafeOffMailbox::default());
        let handle = I2cServiceHandle {
            bus: 7,
            tx: I2cServiceSender::Deadline(tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: Some(Arc::clone(&mailbox)),
        };
        let first_handle = handle.clone();
        let first =
            std::thread::spawn(move || first_handle.disable_voltage(0x55, I2cPicFirmware::Stock));
        let second_handle = handle.clone();
        let second =
            std::thread::spawn(move || second_handle.disable_voltage(0x55, I2cPicFirmware::Stock));

        let deadline = Instant::now() + Duration::from_secs(1);
        while mailbox.pending_waiter_count(0x55) != 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(mailbox.pending_endpoint_count(), 1);
        assert_eq!(mailbox.pending_waiter_count(0x55), 2);
        assert!(
            rx.try_recv().is_ok(),
            "the full normal queue entry must remain independent of reserved safe-off"
        );

        mailbox
            .take_next()
            .unwrap()
            .complete(7, I2cSafeOffExecution::Unit(Ok(())));
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(mailbox.pending_endpoint_count(), 0);
    }

    #[test]
    fn reserved_safe_off_is_fifo_across_distinct_keys_and_coalesces_in_place() {
        let mailbox = I2cSafeOffMailbox::default();
        for (addr, data) in [(0x60, vec![0xF0]), (0x10, vec![0x10]), (0x60, vec![0xF0])] {
            let (reply_tx, _reply_rx) = mpsc::sync_channel(1);
            mailbox
                .enqueue_write(
                    7,
                    addr,
                    data,
                    test_permit(I2cOperationIntent::SafeOff),
                    reply_tx,
                )
                .unwrap();
        }

        assert_eq!(mailbox.pending_endpoint_count(), 2);
        let first = mailbox.take_next().unwrap();
        assert_eq!(first.addr, 0x60, "first admission must not be key-sorted");
        assert_eq!(first.waiters.len(), 2, "duplicate must coalesce in place");
        let second = mailbox.take_next().unwrap();
        assert_eq!(second.addr, 0x10);
    }

    #[test]
    fn reserved_pic_disable_coalescing_distinguishes_firmware_protocols() {
        let (normal_tx, _normal_rx) = mpsc::sync_channel(1);
        let mailbox = Arc::new(I2cSafeOffMailbox::default());
        let handle = I2cServiceHandle {
            bus: 7,
            tx: I2cServiceSender::Deadline(normal_tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: Some(Arc::clone(&mailbox)),
        };

        let stock = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::Stock)
            .unwrap()
            .unwrap();
        let braiins = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::BraiinsOs)
            .unwrap()
            .unwrap();

        assert_eq!(
            mailbox.pending_endpoint_count(),
            2,
            "different PIC protocols must never share one execution"
        );
        for receipt in [stock, braiins] {
            mailbox
                .take_next()
                .unwrap()
                .complete(7, I2cSafeOffExecution::Unit(Ok(())));
            receipt.recv().unwrap().unwrap();
        }
    }

    #[test]
    fn worker_exit_fails_accepted_safe_off_and_closes_admission() {
        let (normal_tx, _normal_rx) = mpsc::sync_channel(1);
        let mailbox = Arc::new(I2cSafeOffMailbox::default());
        let handle = I2cServiceHandle {
            bus: 7,
            tx: I2cServiceSender::Deadline(normal_tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: Some(Arc::clone(&mailbox)),
        };
        let receipt = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::Stock)
            .unwrap()
            .unwrap();

        mailbox.fail_pending_on_worker_exit(7, "injected worker exit");

        let rendered = receipt.recv().unwrap().unwrap_err().to_string();
        assert!(rendered.contains("injected worker exit"), "{rendered}");
        assert_eq!(mailbox.pending_endpoint_count(), 0);
        let rejected = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::Stock)
            .unwrap_err()
            .to_string();
        assert!(rejected.contains("closing or closed"), "{rejected}");
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn ordinary_service_disconnect_does_not_abandon_accepted_safe_off() {
        let backend = Arc::new(TimeoutRecordingBackend::default());
        let handle = spawn_sim_i2c_service(7, backend, Vec::new()).unwrap();
        let receipt = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::Stock)
            .unwrap()
            .unwrap();

        // Dropping the last ordinary sender can wake recv_timeout with
        // Disconnected while the reserved mailbox has independently accepted
        // work. The worker must close admission, recheck, and drain it.
        drop(handle);

        receipt
            .recv_timeout(Duration::from_secs(1))
            .expect("service exit must resolve every accepted SafeOff receipt")
            .unwrap();
    }

    #[test]
    fn poisoned_safe_off_mailbox_recovers_accepted_work() {
        let (normal_tx, _normal_rx) = mpsc::sync_channel(1);
        let mailbox = Arc::new(I2cSafeOffMailbox::default());
        let poisoned = Arc::clone(&mailbox);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.pending.lock().unwrap();
            panic!("intentionally poison SafeOff mailbox");
        });
        let handle = I2cServiceHandle {
            bus: 7,
            tx: I2cServiceSender::Deadline(normal_tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: Some(Arc::clone(&mailbox)),
        };

        let receipt = handle
            .enqueue_reserved_disable(0x55, I2cPicFirmware::Stock)
            .unwrap()
            .unwrap();
        assert_eq!(mailbox.pending_endpoint_count(), 1);
        mailbox
            .take_next()
            .unwrap()
            .complete(7, I2cSafeOffExecution::Unit(Ok(())));
        receipt.recv().unwrap().unwrap();
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn reserved_conditional_plan_executes_late_as_one_fifo_owned_unit() {
        let mailbox = I2cSafeOffMailbox::default();
        let (plan_tx, plan_rx) = mpsc::sync_channel(1);
        mailbox
            .enqueue_conditional_plan(
                7,
                0x58,
                vec![I2cTransactionStep::Write(vec![0xA0])],
                vec![
                    I2cTransactionStep::SleepMs(2),
                    I2cTransactionStep::Write(vec![0xB0]),
                ],
                vec![I2cTransactionStep::Write(vec![0xC0])],
                test_permit(I2cOperationIntent::SafeOff),
                plan_tx,
            )
            .unwrap();
        let (later_tx, later_rx) = mpsc::sync_channel(1);
        mailbox
            .enqueue_write(
                7,
                0x58,
                vec![0xD0],
                test_permit(I2cOperationIntent::SafeOff),
                later_tx,
            )
            .unwrap();

        assert!(matches!(
            plan_rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(plan_rx); // the accepted plan outlives its delayed caller

        let backend = Arc::new(ConditionalPlanBackend::default());
        let mut bus = I2cBus::open_sim(7, backend.clone());
        let plan = mailbox.take_next().expect("plan must remain queued");
        let execution = plan.execute(&mut bus);
        let I2cSafeOffExecution::Conditional { outcome, .. } = &execution else {
            panic!("expected conditional execution")
        };
        assert!(outcome.prelude.completed());
        assert!(!outcome.primary.completed());
        assert!(outcome.compensation.completed());
        plan.complete(7, execution);

        let later = mailbox.take_next().expect("later write must follow plan");
        let execution = later.execute(&mut bus);
        later.complete(7, execution);
        later_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();

        assert_eq!(
            *backend.writes.lock().unwrap(),
            vec![vec![0xA0], vec![0xB0], vec![0xC0], vec![0xD0]],
            "compensation must finish before the next FIFO SafeOff operation"
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn reserved_unit_and_conditional_plans_recover_then_retry_once() {
        let backend = Arc::new(RecoverOnceBackend::default());
        let mut bus = I2cBus::open_sim(7, backend.clone());
        let (unit_tx, unit_rx) = mpsc::sync_channel(1);
        let unit = PendingI2cSafeOff {
            addr: 0x58,
            operation: I2cSafeOffOperation::WriteBytes { data: vec![0xA0] },
            permit: test_permit(I2cOperationIntent::SafeOff),
            waiters: vec![I2cSafeOffWaiter::Unit(unit_tx)],
        };
        execute_pending_safe_off_with_unwind_boundary(unit, 7, &mut bus);
        unit_rx.recv().unwrap().unwrap();
        assert_eq!(backend.writes.load(Ordering::SeqCst), 2);
        assert_eq!(backend.recoveries.load(Ordering::SeqCst), 1);

        backend.recoveries.store(0, Ordering::SeqCst);
        backend.writes.store(0, Ordering::SeqCst);
        let (plan_tx, plan_rx) = mpsc::sync_channel(1);
        let plan = PendingI2cSafeOff {
            addr: 0x58,
            operation: I2cSafeOffOperation::ConditionalPlan {
                prelude: vec![I2cTransactionStep::Write(vec![0xA0])],
                primary: vec![I2cTransactionStep::Write(vec![0xB1])],
                compensation: vec![I2cTransactionStep::Write(vec![0xC0])],
            },
            permit: test_permit(I2cOperationIntent::SafeOff),
            waiters: vec![I2cSafeOffWaiter::Conditional(plan_tx)],
        };
        execute_pending_safe_off_with_unwind_boundary(plan, 7, &mut bus);
        let outcome = plan_rx.recv().unwrap().unwrap();
        assert!(outcome.primary.completed());
        assert!(outcome.prelude.completed());
        assert_eq!(backend.writes.load(Ordering::SeqCst), 5);
        assert_eq!(backend.recoveries.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn reserved_retry_classifier_excludes_safety_and_protocol_outcomes() {
        let safety = I2cSafeOffExecution::Unit(Err(HalError::I2cSafetySuperseded {
            bus: 7,
            addr: 0x58,
            detail: "newer barrier".into(),
        }));
        let protocol =
            I2cSafeOffExecution::Unit(Err(HalError::PsuProtocol("injected protocol rejection")));
        let unknown = I2cSafeOffExecution::Unit(Err(HalError::I2cSafeOffOutcomeUnknown {
            bus: 7,
            addr: 0x58,
            detail: "accepted but unobserved".into(),
        }));
        assert!(!safety.requires_transport_recovery());
        assert!(!protocol.requires_transport_recovery());
        assert!(!unknown.requires_transport_recovery());
    }

    #[test]
    fn explicit_safe_off_transaction_on_raw_handle_submits_once_without_recursion() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let caller = std::thread::spawn(move || {
            handle.transaction_with_intent(
                I2cOperationIntent::SafeOff,
                0x58,
                vec![I2cTransactionStep::Write(vec![0xA0])],
            )
        });
        let request = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let I2cRequest::Transaction {
            steps, reply_tx, ..
        } = request
        else {
            panic!("expected one raw transaction")
        };
        assert_eq!(steps, vec![I2cTransactionStep::Write(vec![0xA0])]);
        reply_tx.send(Ok(Vec::new())).unwrap();
        assert!(caller.join().unwrap().unwrap().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn safe_off_write_read_is_rejected_before_queue_admission() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let error = handle
            .write_read_with_intent(I2cOperationIntent::SafeOff, 0x58, &[0x01], 1)
            .unwrap_err();

        assert!(error.to_string().contains("must be write-only"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn accepted_reserved_receipt_timeout_is_typed_outcome_unknown() {
        let (_reply_tx, reply_rx) = mpsc::sync_channel(1);
        let error = wait_for_reserved_safe_off_receipt_with_budget(
            7,
            0x58,
            reply_rx,
            "test plan",
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(matches!(error, HalError::I2cSafeOffOutcomeUnknown { .. }));
    }

    #[test]
    fn queued_reply_timeout_is_later_rejected_without_execution() {
        let (tx, rx) = mpsc::sync_channel(1);
        let handle = I2cServiceHandle {
            bus: 1,
            tx: I2cServiceSender::Deadline(tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: None,
        };
        let (request, reply_rx) = heartbeat_request();
        let result = handle.submit_with_budget(
            0x55,
            request,
            reply_rx,
            I2cRequestBudget {
                admission: Duration::from_millis(5),
                start: Duration::from_millis(10),
                execution: Duration::from_millis(10),
            },
        );
        let rendered = result.unwrap_err().to_string();
        assert!(rendered.contains("will not touch the bus"), "{rendered}");

        let queued = rx.recv().unwrap();
        assert!(start_envelope_at(queued, 1, Instant::now()).is_none());
    }

    #[test]
    fn started_reply_timeout_reports_unknown_hardware_outcome() {
        let (tx, rx) = mpsc::sync_channel(1);
        let handle = I2cServiceHandle {
            bus: 1,
            tx: I2cServiceSender::Deadline(tx),
            safety: Arc::new(I2cSafetyAuthority::default()),
            safe_off_mailbox: None,
        };
        let worker = std::thread::spawn(move || {
            let queued = rx.recv().unwrap();
            let Some((request, _state, _permit)) = start_envelope_at(queued, 1, Instant::now())
            else {
                panic!("request unexpectedly expired");
            };
            std::thread::sleep(Duration::from_millis(40));
            drop(request);
        });
        let (request, reply_rx) = heartbeat_request();
        let result = handle.submit_with_budget(
            0x55,
            request,
            reply_rx,
            I2cRequestBudget {
                admission: Duration::from_millis(5),
                start: Duration::from_millis(10),
                execution: Duration::from_millis(10),
            },
        );
        let rendered = result.unwrap_err().to_string();
        assert!(rendered.contains("outcome is unknown"), "{rendered}");
        worker.join().unwrap();
    }

    #[test]
    fn raw_unit_test_receiver_contract_remains_compatible() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = std::thread::spawn(move || match rx.recv().unwrap() {
            I2cRequest::Transaction { reply_tx, .. } => {
                reply_tx.send(Ok(vec![vec![0x89]])).unwrap();
            }
            other => panic!("unexpected request: {other:?}"),
        });
        let result = handle
            .transaction(0x55, vec![I2cTransactionStep::Read(1)])
            .unwrap();
        assert_eq!(result, vec![vec![0x89]]);
        worker.join().unwrap();
    }

    #[test]
    fn async_facade_keeps_current_thread_runtime_responsive() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (handle, rx) = I2cServiceHandle::for_unit_tests();
            let responder = std::thread::spawn(move || match rx.recv().unwrap() {
                I2cRequest::Heartbeat { reply_tx, .. } => {
                    std::thread::sleep(Duration::from_millis(75));
                    reply_tx.send(Ok(())).unwrap();
                }
                other => panic!("unexpected request: {other:?}"),
            });
            let async_handle = handle.async_handle();
            let call = async_handle.heartbeat(0x55, I2cPicFirmware::Stock);
            tokio::pin!(call);
            tokio::select! {
                result = &mut call => panic!("I2C completed before timer: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            call.await.unwrap();
            responder.join().unwrap();
        });
    }

    #[test]
    fn cancellation_before_blocking_dispatch_never_submits_request() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
            let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(0);
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            entered_rx.recv().unwrap();

            let (handle, rx) = I2cServiceHandle::for_unit_tests();
            let async_handle = handle.async_handle();
            let task =
                tokio::spawn(
                    async move { async_handle.heartbeat(0x55, I2cPicFirmware::Stock).await },
                );
            tokio::task::yield_now().await;
            task.abort();
            let _ = task.await;

            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                rx.try_recv().is_err(),
                "cancelled async call must not submit later"
            );
        });
    }

    #[test]
    fn async_safe_off_is_admitted_before_blocking_pool_dispatch() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
            let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(0);
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            entered_rx.recv().unwrap();

            let (normal_tx, _normal_rx) = mpsc::sync_channel(1);
            let mailbox = Arc::new(I2cSafeOffMailbox::default());
            let handle = I2cServiceHandle {
                bus: 7,
                tx: I2cServiceSender::Deadline(normal_tx),
                safety: Arc::new(I2cSafetyAuthority::default()),
                safe_off_mailbox: Some(Arc::clone(&mailbox)),
            };
            let async_handle = handle.async_handle();
            let disable = tokio::spawn(async move {
                async_handle
                    .disable_voltage(0x55, I2cPicFirmware::Stock)
                    .await
            });

            let deadline = Instant::now() + Duration::from_secs(1);
            while mailbox.pending_endpoint_count() != 1 && Instant::now() < deadline {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                mailbox.pending_endpoint_count(),
                1,
                "SafeOff must enter the reserved mailbox while the blocking pool is saturated"
            );

            mailbox
                .take_next()
                .unwrap()
                .complete(7, I2cSafeOffExecution::Unit(Ok(())));
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            disable.await.unwrap().unwrap();
        });
    }

    #[test]
    fn async_dispatch_timeout_never_submits_late_request() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
            let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(0);
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            entered_rx.recv().unwrap();

            let (handle, rx) = I2cServiceHandle::for_unit_tests();
            let async_handle = handle.async_handle();
            let result = async_handle
                .offload(
                    0x55,
                    "test-heartbeat",
                    Duration::from_millis(10),
                    move |service| service.heartbeat(0x55, I2cPicFirmware::Stock),
                )
                .await;
            let rendered = result.unwrap_err().to_string();
            assert!(rendered.contains("request was not submitted"), "{rendered}");

            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                rx.try_recv().is_err(),
                "dispatch-timeout closure must not submit when it runs later"
            );
        });
    }

    #[test]
    fn async_started_panic_reports_unknown_outcome() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (handle, _rx) = I2cServiceHandle::for_unit_tests();
            let result = handle
                .async_handle()
                .offload(
                    0x55,
                    "panic-test",
                    Duration::from_secs(1),
                    move |_service| -> Result<()> { panic!("intentional async I2C test panic") },
                )
                .await;
            let rendered = result.unwrap_err().to_string();
            assert!(
                rendered.contains("hardware outcome is unknown"),
                "{rendered}"
            );
        });
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn compound_transaction_restores_default_timeout_on_success_and_error() {
        let permit = test_permit(I2cOperationIntent::Recovery);
        let success_backend = Arc::new(TimeoutRecordingBackend::default());
        let mut success_bus = I2cBus::open_sim(3, success_backend.clone());
        execute_transaction(
            &mut success_bus,
            0x55,
            vec![
                I2cTransactionStep::SetTimeout(50),
                I2cTransactionStep::Write(vec![0x01]),
            ],
            &permit,
        )
        .unwrap();
        assert_eq!(*success_backend.timeouts.lock().unwrap(), vec![50, 10]);

        let failure_backend = Arc::new(TimeoutRecordingBackend::default());
        failure_backend.fail_write.store(1, Ordering::Release);
        let mut failure_bus = I2cBus::open_sim(4, failure_backend.clone());
        assert!(execute_transaction(
            &mut failure_bus,
            0x55,
            vec![
                I2cTransactionStep::SetTimeout(50),
                I2cTransactionStep::Write(vec![0x01]),
            ],
            &permit,
        )
        .is_err());
        assert_eq!(*failure_backend.timeouts.lock().unwrap(), vec![50, 10]);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn timed_out_pic16_batch_safe_off_cannot_enqueue_a_duplicate_epoch_attempt() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let service = spawn_sim_i2c_service(0, Arc::new(backend.clone()), Vec::new()).unwrap();
        let batch = service
            .safety
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish timeout-test batch");
        backend.arm_next_transfer_stall().unwrap();

        let first = service
            .pic16_safe_off_batch_with_budget(Arc::clone(&batch), Duration::from_millis(25))
            .expect_err("stalled worker must exceed caller receipt budget");
        assert!(matches!(first, HalError::I2cSafeOffOutcomeUnknown { .. }));
        assert!(backend
            .wait_for_transfer_stall(Duration::from_secs(1))
            .unwrap());
        let generation_after_first = service.safety.generation.load(Ordering::SeqCst);

        let retry = service
            .pic16_safe_off_batch_with_budget(Arc::clone(&batch), Duration::from_millis(5))
            .expect_err("retry must wait for the worker-owned attempt");
        assert!(matches!(retry, HalError::I2cSafeOffOutcomeUnknown { .. }));
        assert_eq!(
            service.safety.generation.load(Ordering::SeqCst),
            generation_after_first,
            "waiting retry must not advance a second SafeOff generation"
        );

        backend.release_transfer_stall().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !batch.released() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(batch.released(), "worker did not finalize the late SafeOff");
        assert_eq!(
            service
                .pic16_safe_off_batch_with_budget(Arc::clone(&batch), Duration::from_millis(25),)
                .expect("late worker success must remain authoritative")
                .disposition(),
            Pic16BatchSafeOffDisposition::AlreadyReleased
        );
        assert!(!service
            .safety
            .pic16_shutdown_unresolved
            .load(Ordering::SeqCst));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn stalled_pre_handoff_pic16_claim_latches_terminal_until_recovered() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let service = spawn_sim_i2c_service(0, Arc::new(backend), Vec::new()).unwrap();
        let batch = service
            .safety
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish caller-stall batch");
        let stalled_caller = batch
            .claim_safe_off_attempt()
            .expect("claim caller-stall SafeOff");

        let error = service
            .pic16_safe_off_batch_with_budget(Arc::clone(&batch), Duration::from_millis(5))
            .expect_err("second caller must not overtake an unhanded claim");
        assert!(matches!(error, HalError::I2cSafeOffOutcomeUnknown { .. }));
        assert!(service.terminal_safe_off_is_latched());
        assert!(service
            .safety
            .pic16_shutdown_unresolved
            .load(Ordering::SeqCst));

        drop(stalled_caller);
        assert!(service
            .pic16_safe_off_batch_with_budget(Arc::clone(&batch), Duration::from_secs(1))
            .expect("recover caller-stalled batch")
            .all_disabled());
        assert!(!service
            .safety
            .pic16_shutdown_unresolved
            .load(Ordering::SeqCst));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn queued_pic16_safe_off_reconciles_admission_release_before_wire_io() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let authority = Arc::new(I2cSafetyAuthority::default());
        let batch = authority
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish queued-race batch");
        let mut attempt = batch
            .claim_safe_off_attempt()
            .expect("claim queued-race SafeOff");
        let mailbox = I2cSafeOffMailbox::default();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let generation = authority.advance_safe_off_generation();
        mailbox
            .enqueue_pic16_batch_disable(
                0,
                batch.epoch(),
                vec![0x55],
                Arc::clone(&batch),
                I2cSafetyPermit {
                    authority: Arc::clone(&authority),
                    intent: I2cOperationIntent::SafeOff,
                    generation,
                    scope: I2cPermitScope::Pic16BatchSafeOff {
                        epoch: batch.epoch(),
                        batch: Arc::clone(&batch),
                    },
                },
                &mut attempt,
                reply_tx,
            )
            .expect("enqueue queued-race SafeOff");
        drop(attempt);

        authority
            .release_pic16_batch(batch.epoch())
            .expect("admission compensation releases batch first");
        batch.mark_released();
        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let mut i2c = I2cBus::open_sim(0, Arc::new(backend.clone()));
        execute_pending_safe_off_with_unwind_boundary(
            mailbox.take_next().expect("queued SafeOff operation"),
            0,
            &mut i2c,
        );

        assert_eq!(
            reply_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("queued-race completion")
                .expect("released epoch reconciliation")
                .disposition(),
            Pic16BatchSafeOffDisposition::AlreadyReleased
        );
        assert!(backend.drain_trace().unwrap().is_empty());
        assert!(!authority.pic16_shutdown_unresolved.load(Ordering::SeqCst));

        let no_bus_batch = authority
            .publish_pic16_batch(0, vec![0x55])
            .expect("publish no-bus queued-race batch");
        let mut no_bus_attempt = no_bus_batch
            .claim_safe_off_attempt()
            .expect("claim no-bus queued-race SafeOff");
        let no_bus_mailbox = I2cSafeOffMailbox::default();
        let (no_bus_reply_tx, no_bus_reply_rx) = mpsc::sync_channel(1);
        let generation = authority.advance_safe_off_generation();
        no_bus_mailbox
            .enqueue_pic16_batch_disable(
                0,
                no_bus_batch.epoch(),
                vec![0x55],
                Arc::clone(&no_bus_batch),
                I2cSafetyPermit {
                    authority: Arc::clone(&authority),
                    intent: I2cOperationIntent::SafeOff,
                    generation,
                    scope: I2cPermitScope::Pic16BatchSafeOff {
                        epoch: no_bus_batch.epoch(),
                        batch: Arc::clone(&no_bus_batch),
                    },
                },
                &mut no_bus_attempt,
                no_bus_reply_tx,
            )
            .expect("enqueue no-bus queued-race SafeOff");
        drop(no_bus_attempt);
        authority
            .release_pic16_batch(no_bus_batch.epoch())
            .expect("admission compensation releases no-bus batch first");
        no_bus_batch.mark_released();
        let pending = no_bus_mailbox
            .take_next()
            .expect("no-bus queued SafeOff operation");
        let execution = pending.bus_unavailable_execution(0);
        pending.complete(0, execution);
        assert_eq!(
            no_bus_reply_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("no-bus queued-race completion")
                .expect("no-bus released epoch reconciliation")
                .disposition(),
            Pic16BatchSafeOffDisposition::AlreadyReleased
        );
        assert!(!authority.pic16_shutdown_unresolved.load(Ordering::SeqCst));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn stalled_started_request_bounds_callers_and_cancels_queued_mutation() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend, TraceEvent};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        backend
            .configure_controller_watchdog(Duration::from_millis(50))
            .unwrap();
        let mut setup_bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        setup_bus.set_slave(0x20).unwrap();
        setup_bus.write(&[0x55, 0xAA, 0x15, 0x01]).unwrap();
        assert!(backend.voltage_enabled().unwrap());

        let service = spawn_sim_i2c_service(0, Arc::new(backend.clone()), Vec::new()).unwrap();
        backend.arm_next_transfer_stall().unwrap();

        let heartbeat_service = service.clone();
        let heartbeat =
            std::thread::spawn(move || heartbeat_service.write_bytes(0x20, &[0x55, 0xAA, 0x16]));
        assert!(backend
            .wait_for_transfer_stall(Duration::from_secs(1))
            .unwrap());

        let disable_service = service.clone();
        let disable = std::thread::spawn(move || {
            disable_service.write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x00])
        });

        let heartbeat_error = heartbeat.join().unwrap().unwrap_err().to_string();
        let disable_error = disable.join().unwrap().unwrap_err().to_string();
        assert!(
            heartbeat_error.contains("outcome is unknown"),
            "{heartbeat_error}"
        );
        assert!(
            disable_error.contains("cancelled before execution"),
            "{disable_error}"
        );

        backend
            .advance_virtual_time(Duration::from_millis(50))
            .unwrap();
        assert!(backend.controller_watchdog_expired().unwrap());
        assert!(!backend.voltage_enabled().unwrap());

        backend.release_transfer_stall().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let trace = backend.drain_trace().unwrap();
        assert!(trace
            .iter()
            .any(|event| matches!(event, TraceEvent::ControllerWatchdogExpired { .. })));
        assert!(!trace.iter().any(|event| matches!(
            event,
            TraceEvent::I2cWrite { bytes, .. }
                if bytes.as_slice() == [0x55, 0xAA, 0x15, 0x00]
        )));
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn reserved_safe_off_runs_once_after_stalled_request_and_resolves_all_waiters() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend, TraceEvent};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let mut setup_bus = I2cBus::open_sim(0, Arc::new(backend.clone()));
        setup_bus.set_slave(0x20).unwrap();
        setup_bus.write(&[0x55, 0xAA, 0x15, 0x01]).unwrap();
        assert!(backend.voltage_enabled().unwrap());
        backend.drain_trace().unwrap();

        let service = spawn_sim_i2c_service(0, Arc::new(backend.clone()), Vec::new()).unwrap();
        let mailbox = Arc::clone(service.safe_off_mailbox.as_ref().unwrap());
        backend.arm_next_transfer_stall().unwrap();
        let heartbeat_service = service.clone();
        let heartbeat =
            std::thread::spawn(move || heartbeat_service.heartbeat(0x20, I2cPicFirmware::Stock));
        assert!(backend
            .wait_for_transfer_stall(Duration::from_secs(1))
            .unwrap());

        let mut disables = Vec::new();
        for _ in 0..3 {
            let disable_service = service.clone();
            disables.push(std::thread::spawn(move || {
                disable_service.disable_dspic_voltage(0x20, I2cDspicDisableProtocol::Bare)
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while mailbox.pending_waiter_count(0x20) != 3 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(mailbox.pending_endpoint_count(), 1);
        assert_eq!(mailbox.pending_waiter_count(0x20), 3);

        backend.release_transfer_stall().unwrap();
        heartbeat.join().unwrap().unwrap();
        for disable in disables {
            disable.join().unwrap().unwrap();
        }
        assert!(!backend.voltage_enabled().unwrap());

        let wire_bytes: Vec<u8> = backend
            .drain_trace()
            .unwrap()
            .into_iter()
            .filter_map(|event| match event {
                TraceEvent::I2cWrite { bytes, .. } => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            wire_bytes
                .windows(4)
                .filter(|window| *window == [0x55, 0xAA, 0x15, 0x00])
                .count(),
            1,
            "coalesced waiters must produce exactly one physical disable frame"
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn unmanaged_bus_recovery_runs_once_through_the_service_worker() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend, TraceEvent};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Pic16);
        let service = spawn_sim_i2c_service(3, Arc::new(backend.clone()), Vec::new()).unwrap();

        service.recover_unmanaged_bus().unwrap();

        assert_eq!(
            backend
                .drain_trace()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, TraceEvent::I2cRecovery { bus: 3 }))
                .count(),
            1
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn queued_whole_fabric_recovery_loses_to_pic16_batch_publication_without_io() {
        use crate::platform::sim::{SimModel, SimPlatform, TraceEvent};

        let platform = SimPlatform::new(SimModel::S9);
        let service = platform.open_i2c_service(0).unwrap();
        let endpoint = platform.pic16_endpoint(0, 0x55).unwrap();

        platform.arm_next_i2c_transfer_stall().unwrap();
        let blocker_service = service.clone();
        let blocker = std::thread::spawn(move || blocker_service.read_bytes(0x01, 1));
        assert!(platform
            .wait_for_i2c_transfer_stall(Duration::from_secs(1))
            .unwrap());

        let (recovery_tx, recovery_rx) = mpsc::sync_channel(1);
        let recovery_state = Arc::new(AtomicU8::new(I2C_REQUEST_QUEUED));
        let envelope = I2cServiceEnvelope {
            must_start_by: Instant::now() + I2C_QUEUE_START_BUDGET,
            state: recovery_state,
            permit: I2cSafetyPermit {
                authority: Arc::clone(&service.safety),
                intent: I2cOperationIntent::Recovery,
                generation: service.safety.generation.load(Ordering::SeqCst),
                scope: I2cPermitScope::Generic,
            },
            request: I2cRequest::RecoverUnmanagedBus {
                reply_tx: recovery_tx,
            },
        };
        match &service.tx {
            I2cServiceSender::Deadline(tx) => tx.send(envelope).unwrap(),
            I2cServiceSender::Raw(_) => unreachable!("sim service uses deadline envelopes"),
        }

        let admission_service = service.clone();
        let admission = std::thread::spawn(move || {
            admission_service.begin_pic16_admission([endpoint], MIN_SAFE_PIC_DAC_VALUE)
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !service.safety.pic16_has_managed_addresses() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(service.safety.pic16_has_managed_addresses());

        platform.release_i2c_transfer_stall().unwrap();
        let _ = blocker.join().unwrap();
        let error = recovery_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("recovery worker reply")
            .expect_err("managed batch publication must fence queued recovery");
        assert!(matches!(
            error,
            HalError::I2cSafetySuperseded { addr: 0, .. }
        ));
        assert!(!platform
            .drain_i2c_trace()
            .unwrap()
            .iter()
            .any(|event| matches!(event, TraceEvent::I2cRecovery { .. })));

        drop(
            admission
                .join()
                .unwrap()
                .expect("worker accepts the admission job after refusing recovery"),
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn raw_sim_handle_cannot_alias_a_live_service_fabric() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let service = spawn_sim_i2c_service(7, Arc::new(backend.clone()), Vec::new()).unwrap();

        assert!(I2cBus::try_open_sim(7, Arc::new(backend.clone())).is_err());
        drop(service);

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match I2cBus::try_open_sim(7, Arc::new(backend.clone())) {
                Ok(raw) => {
                    drop(raw);
                    break;
                }
                Err(_) if Instant::now() < deadline => std::thread::yield_now(),
                Err(error) => panic!("clean service exit did not release fabric lease: {error}"),
            }
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn live_raw_sim_handle_blocks_service_until_lease_release() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let raw = I2cBus::try_open_sim(8, Arc::new(backend.clone())).unwrap();
        assert!(spawn_sim_i2c_service(8, Arc::new(backend.clone()), Vec::new()).is_err());

        drop(raw);
        let service = spawn_sim_i2c_service(8, Arc::new(backend), Vec::new()).unwrap();
        drop(service);
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn inherited_raw_handle_is_refused_before_simulated_wire_io() {
        use crate::platform::sim::{SimControllerKind, SimI2cBackend};

        let backend = SimI2cBackend::with_controller(SimControllerKind::Dspic);
        let mut raw = I2cBus::try_open_sim(19, Arc::new(backend.clone())).unwrap();
        raw.current_addr = Some(0x20);
        raw._fabric_lease
            .as_mut()
            .expect("raw handle owns a lease")
            .creator_pid ^= 1;

        assert!(matches!(
            raw.write(&[0x55, 0xAA, 0x15, 0x01]),
            Err(HalError::I2cFabricUnavailable { .. })
        ));
        assert!(backend.drain_trace().unwrap().is_empty());
    }

    #[test]
    fn every_raw_wire_entry_revalidates_process_ownership() {
        let source = include_str!("i2c.rs");
        for signature in [
            "pub fn set_slave(",
            "pub fn write(",
            "pub fn write_byte_by_byte(",
            "pub fn read(",
            "pub fn write_read(",
            "pub fn set_timeout(",
            "pub(crate) fn bus_recovery(",
        ] {
            let body = source
                .split_once(signature)
                .unwrap_or_else(|| panic!("missing {signature}"))
                .1
                .split("\n    pub ")
                .next()
                .unwrap();
            assert!(
                body.contains("self.validate_raw_fabric_owner()?;"),
                "{signature} must refuse fork-inherited raw state before wire/MMIO"
            );
        }
    }

    #[cfg(feature = "sim-hal")]
    fn isolated_registry_test_key(bus: u8) -> I2cFabricRegistryKey {
        I2cFabricRegistryKey::SimulatedBus {
            bus,
            backend: next_test_service_identity(),
        }
    }

    #[test]
    fn dedicated_am2_psu_fabric_coexists_with_bus_zero_but_self_conflicts() {
        use crate::psu_gpio_i2c::AM2_PSU_GPIO_I2C_FABRIC;

        let bus_zero = I2cFabricRegistryKey::linux_adapter(0);
        let psu = I2cFabricRegistryKey::PhysicalFabric(AM2_PSU_GPIO_I2C_FABRIC);
        assert_ne!(bus_zero, psu, "dedicated PSU wires are not /dev/i2c-0");

        let mut registry = HashMap::new();
        registry.insert(
            bus_zero,
            I2cFabricRegistryEntry::Raw {
                allocation: 1,
                kind: I2cRawLeaseKind::Bootstrap,
                _os_lease: None,
            },
        );
        assert!(refuse_existing_i2c_fabric_owner(&registry, psu).is_ok());

        registry.insert(
            psu,
            I2cFabricRegistryEntry::Raw {
                allocation: 2,
                kind: I2cRawLeaseKind::BitBang,
                _os_lease: None,
            },
        );
        assert!(refuse_existing_i2c_fabric_owner(&registry, psu).is_err());
    }

    #[cfg(feature = "sim-hal")]
    fn registry_state(key: I2cFabricRegistryKey) -> Option<I2cServiceRegistryState> {
        let registry = i2c_fabric_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match registry.get(&key) {
            Some(I2cFabricRegistryEntry::RuntimeService { state, .. }) => Some(*state),
            _ => None,
        }
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn clean_service_preparation_abort_releases_registry_ownership() {
        let key = isolated_registry_test_key(20);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();

        drop(reservation);

        assert_eq!(registry_state(key), None);
        let replacement_authority = Arc::new(I2cSafetyAuthority::default());
        drop(
            I2cServiceRegistryLease::reserve(key, &replacement_authority)
                .expect("a proven-clean preparation abort must remain retryable"),
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn mutated_service_preparation_abort_is_quarantined() {
        let key = isolated_registry_test_key(21);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let mut reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();
        reservation.mark_fabric_mutation_started().unwrap();

        drop(reservation);

        assert_eq!(
            registry_state(key),
            Some(I2cServiceRegistryState::Quarantined(
                I2cServiceQuarantineReason::PreparationAborted
            ))
        );
        let replacement_authority = Arc::new(I2cSafetyAuthority::default());
        assert!(I2cServiceRegistryLease::reserve(key, &replacement_authority).is_err());
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn reserved_preparation_marks_mutated_before_callback_and_quarantines_error() {
        let key = isolated_registry_test_key(26);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let mut reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();

        let error = run_reserved_i2c_preparation(&mut reservation, || {
            assert_eq!(
                registry_state(key),
                Some(I2cServiceRegistryState::PreparingMutated)
            );
            Err(std::io::Error::other(
                "synthetic platform preparation failure",
            ))
        })
        .unwrap_err();
        assert!(error.to_string().contains("synthetic platform"));
        drop(reservation);

        assert_eq!(
            registry_state(key),
            Some(I2cServiceRegistryState::Quarantined(
                I2cServiceQuarantineReason::PreparationAborted
            ))
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn unexpected_active_service_lease_drop_is_quarantined() {
        let key = isolated_registry_test_key(22);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let mut reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();
        reservation.activate_for_worker().unwrap();

        drop(reservation);

        assert_eq!(
            registry_state(key),
            Some(I2cServiceRegistryState::Quarantined(
                I2cServiceQuarantineReason::UnexpectedLeaseDrop
            ))
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn clean_worker_exit_publishes_death_and_releases_registry_ownership() {
        let key = isolated_registry_test_key(23);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();
        let lifetime = I2cServiceWorkerLifetime::new(Arc::clone(&authority), reservation).unwrap();

        drop(lifetime);

        assert!(!authority.worker_alive.load(Ordering::SeqCst));
        assert_eq!(registry_state(key), None);
        let replacement_authority = Arc::new(I2cSafetyAuthority::default());
        drop(
            I2cServiceRegistryLease::reserve(key, &replacement_authority)
                .expect("clean worker finalization must permit replacement"),
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn panicking_worker_is_quarantined_and_publishes_death() {
        let key = isolated_registry_test_key(24);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();
        let worker_authority = Arc::clone(&authority);

        assert!(std::thread::spawn(move || {
            let _lifetime = I2cServiceWorkerLifetime::new(worker_authority, reservation).unwrap();
            panic!("synthetic I2C worker failure");
        })
        .join()
        .is_err());

        assert!(!authority.worker_alive.load(Ordering::SeqCst));
        assert_eq!(
            registry_state(key),
            Some(I2cServiceRegistryState::Quarantined(
                I2cServiceQuarantineReason::WorkerPanicked
            ))
        );
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn mismatched_service_allocation_cannot_remove_live_owner() {
        let key = isolated_registry_test_key(25);
        let authority = Arc::new(I2cSafetyAuthority::default());
        let reservation = I2cServiceRegistryLease::reserve(key, &authority).unwrap();
        let forged = I2cServiceRegistryLease {
            key,
            allocation: reservation.allocation + 1,
            authority: Arc::downgrade(&authority),
            worker_finalized: false,
        };

        drop(forged);

        assert_eq!(
            registry_state(key),
            Some(I2cServiceRegistryState::PreparingClean)
        );
        let replacement_authority = Arc::new(I2cSafetyAuthority::default());
        assert!(I2cServiceRegistryLease::reserve(key, &replacement_authority).is_err());
        drop(reservation);
        assert_eq!(registry_state(key), None);
    }
}

/// Reserve and prepare the AM1/S9 AXI-IIC fabric, then spawn its sole service.
///
/// Reservation deliberately precedes kernel-driver unbind, controller reset,
/// and SCL recovery. Once preparation starts, no second in-process raw handle
/// can race those mutations or slip in before the worker becomes live.
pub fn spawn_am1_s9_i2c0_service(recover_bus: bool) -> std::io::Result<I2cServiceHandle> {
    let bus = 0;
    let (tx, rx) = mpsc::sync_channel::<I2cServiceEnvelope>(64);
    let safety = Arc::new(I2cSafetyAuthority::default());
    let mut registry =
        I2cServiceRegistryLease::reserve(I2cFabricRegistryKey::linux_adapter(bus), &safety)?;

    // From this point onward even a failed sysfs write may have partially
    // changed platform state. Unexpected preparation/spawn failure must retain
    // both the local tombstone and cross-process descriptor until process exit.
    registry.mark_fabric_mutation_started()?;
    unbind_kernel_i2c_driver().map_err(std::io::Error::other)?;
    init_devmem_iic_mmap().map_err(std::io::Error::other)?;
    if recover_bus {
        tracing::info!(
            bus,
            "recovering reserved AM1/S9 AXI-IIC fabric before service startup"
        );
        std::thread::sleep(Duration::from_millis(50));
        reset_axi_iic_controller().map_err(std::io::Error::other)?;
        std::thread::sleep(Duration::from_millis(10));
        bus_recovery_devmem().map_err(std::io::Error::other)?;
    } else {
        verify_devmem_iic_idle().map_err(std::io::Error::other)?;
    }

    spawn_reserved_i2c_service_with_policy(bus, true, false, Vec::new(), tx, rx, safety, registry)
}

/// Spawn a kernel-fd-only I2C service that never restores AXI IIC registers.
///
/// AM2 keeps the kernel xiic driver bound; this mode recovers only by dropping
/// and reopening `/dev/i2c-N`, avoiding out-of-band `/dev/mem` register writes.
pub fn spawn_i2c_service_no_register_touch(bus: u8) -> std::io::Result<I2cServiceHandle> {
    spawn_i2c_service_with_policy(bus, false, false, Vec::new())
}

/// Same as `spawn_i2c_service_no_register_touch` but also registers a
/// per-bus write denylist. The denylist persists across bus reopens.
///
/// Used by am2 S19j Pro to block writes to AT24C-series hashboard EEPROM
/// addresses (0x50-0x57)..
pub fn spawn_i2c_service_no_register_touch_with_denylist(
    bus: u8,
    write_denylist: Vec<u8>,
) -> std::io::Result<I2cServiceHandle> {
    spawn_i2c_service_with_policy(bus, false, false, write_denylist)
}

/// Reserve the physical adapter before platform code may bind its driver,
/// create a device node, or perform another controller preparation, then spawn
/// the normal kernel-fd-only service with a persistent write denylist.
///
/// The callback executes only after both process-local and cross-process
/// ownership are established. It is conservatively classified as mutating
/// before invocation: callback failure or thread-spawn failure leaves the
/// fabric quarantined in this daemon rather than pretending partial platform
/// preparation was clean.
pub fn spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation<F>(
    bus: u8,
    write_denylist: Vec<u8>,
    prepare: F,
) -> std::io::Result<I2cServiceHandle>
where
    F: FnOnce() -> std::io::Result<()>,
{
    let (tx, rx) = mpsc::sync_channel::<I2cServiceEnvelope>(64);
    let safety = Arc::new(I2cSafetyAuthority::default());
    let mut registry =
        I2cServiceRegistryLease::reserve(I2cFabricRegistryKey::linux_adapter(bus), &safety)?;
    run_reserved_i2c_preparation(&mut registry, prepare)?;
    spawn_reserved_i2c_service_with_policy(
        bus,
        false,
        false,
        write_denylist,
        tx,
        rx,
        safety,
        registry,
    )
}

/// Spawn the normal serialized service API over a host-only simulated bus.
///
/// This is deliberately crate-private: SimPlatform is the only constructor,
/// while daemon/ASIC callers receive the same `I2cServiceHandle` they use on
/// real hardware. Production service construction and recovery logic are not
/// modified by this feature-gated path.
#[cfg(feature = "sim-hal")]
pub(crate) fn spawn_sim_i2c_service(
    bus: u8,
    backend: std::sync::Arc<dyn I2cSimBackend>,
    write_denylist: Vec<u8>,
) -> std::io::Result<I2cServiceHandle> {
    let (tx, rx) = mpsc::sync_channel::<I2cServiceEnvelope>(64);
    let safety = Arc::new(I2cSafetyAuthority::default());
    let backend_identity = backend.service_identity().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "simulated I2C service backend did not provide a stable fabric identity",
        )
    })?;
    let registry = I2cServiceRegistryLease::reserve(
        I2cFabricRegistryKey::SimulatedBus {
            bus,
            backend: backend_identity,
        },
        &safety,
    )?;
    let worker_safety = Arc::clone(&safety);
    let safe_off_mailbox = Arc::new(I2cSafeOffMailbox::default());
    let worker_safe_off_mailbox = Arc::clone(&safe_off_mailbox);
    std::thread::Builder::new()
        .name("sim-i2c-service".to_string())
        .spawn(move || {
            let worker_lifetime = match I2cServiceWorkerLifetime::new(worker_safety, registry) {
                Ok(lifetime) => lifetime,
                Err(error) => {
                    tracing::error!(bus, %error, "simulated I2C worker could not activate its fabric reservation");
                    return;
                }
            };
            let mut lifecycle =
                I2cSafeOffWorkerLifecycle::new(Arc::clone(&worker_safe_off_mailbox), bus);
            let mut i2c = I2cBus::open_sim_for_service(
                bus,
                backend,
                &worker_lifetime.authority,
            )
            .expect("reserved simulated I2C service fabric must open for its worker");
            i2c.set_write_denylist(&write_denylist);
            if let Err(error) = i2c.set_timeout(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES) {
                tracing::error!(
                    bus,
                    %error,
                    "simulated I2C service could not verify its bounded timeout; PIC16 runtime jobs will refuse wire access"
                );
            }
            let host_clock_origin = Instant::now();
            let service_clock_origin = i2c.service_time().unwrap_or(Duration::ZERO);
            let mut pic16_job: Option<Pic16WorkerJob> = None;
            let mut ordinary_disconnected = false;
            loop {
                if let Some(pending) = worker_safe_off_mailbox.take_next() {
                    if let Some(job) = pic16_job.as_ref() {
                        job.request_cancel();
                    }
                    execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                    continue;
                }

                if let Some(job) = pic16_job.as_mut() {
                    let worker_now = host_clock_origin
                        + i2c
                            .service_time()
                            .unwrap_or_else(|| host_clock_origin.elapsed())
                            .saturating_sub(service_clock_origin);
                    let step = job.advance(&mut i2c, worker_now);
                    if step.transport_fault {
                        let _ = i2c.bus_recovery();
                    }
                    if step.shutdown_active_batch {
                        if let Some(pending) =
                            active_pic16_batch_shutdown(&worker_lifetime.authority)
                        {
                            execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                        }
                    }
                    if step.finished {
                        pic16_job = None;
                        continue;
                    }
                    let now = host_clock_origin
                        + i2c
                            .service_time()
                            .unwrap_or_else(|| host_clock_origin.elapsed())
                            .saturating_sub(service_clock_origin);
                    let wait = step
                        .next_due
                        .map(|due| due.saturating_duration_since(now))
                        .unwrap_or(Duration::ZERO)
                        .min(I2C_SAFE_OFF_POLL_INTERVAL);
                    match rx.recv_timeout(wait) {
                        Ok(envelope) => {
                            reject_envelope_during_pic16_job(envelope, bus);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            ordinary_disconnected = true;
                            job.request_cancel();
                        }
                    }
                    continue;
                }

                if ordinary_disconnected {
                    worker_safe_off_mailbox.begin_close();
                    while let Some(pending) = worker_safe_off_mailbox.take_next() {
                        execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                    }
                    if let Some(pending) = active_pic16_batch_shutdown(&worker_lifetime.authority) {
                        execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                    }
                    lifecycle.finish();
                    break;
                }

                let envelope = match rx.recv_timeout(I2C_SAFE_OFF_POLL_INTERVAL) {
                    Ok(envelope) => envelope,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        worker_safe_off_mailbox.begin_close();
                        while let Some(pending) = worker_safe_off_mailbox.take_next() {
                            execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                        }
                        if let Some(pending) =
                            active_pic16_batch_shutdown(&worker_lifetime.authority)
                        {
                            execute_pending_safe_off_with_unwind_boundary(pending, bus, &mut i2c);
                        }
                        lifecycle.finish();
                        break;
                    }
                };
                let Some((request, state, permit)) =
                    start_envelope_at(envelope, bus, Instant::now())
                else {
                    continue;
                };
                if let Some(conflict) = pic16_request_conflict(&request, &permit) {
                    reject_pic16_request_conflict(request, &permit, conflict, bus);
                    state.store(I2C_REQUEST_FINISHED, Ordering::Release);
                    continue;
                }
                match request {
                    I2cRequest::Pic16Admission {
                        plans,
                        batch,
                        cancellation,
                        reservation,
                        completion_tx,
                        reply_tx,
                    } => match reservation.activate() {
                        Ok(reservation_token) => {
                            let job = Pic16AdmissionState::new(
                                bus,
                                permit,
                                cancellation,
                                completion_tx,
                                plans,
                                batch,
                                reservation_token,
                            );
                            if reply_tx.send(Ok(())).is_err() {
                                job.request_cancel();
                            }
                            pic16_job = Some(Pic16WorkerJob::admission(job));
                        }
                        Err(error) => {
                            let _ = reply_tx.send(Err(error));
                        }
                    },
                    I2cRequest::Pic16HeartbeatRound { batch, reply_tx } => {
                        let job =
                            Pic16HeartbeatRoundState::new(bus, permit, batch, reply_tx, state);
                        pic16_job = Some(Pic16WorkerJob::heartbeat_round(job));
                        continue;
                    }
                    request => process_sim_i2c_request(&mut i2c, request, &permit),
                }
                state.store(I2C_REQUEST_FINISHED, Ordering::Release);
            }
        })?;
    Ok(I2cServiceHandle {
        bus,
        tx: I2cServiceSender::Deadline(tx),
        safety,
        safe_off_mailbox: Some(safe_off_mailbox),
    })
}

#[cfg(feature = "sim-hal")]
fn process_sim_i2c_request(i2c: &mut I2cBus, req: I2cRequest, permit: &I2cSafetyPermit) {
    match req {
        I2cRequest::Heartbeat {
            addr,
            firmware,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_heartbeat(i2c, addr, firmware, permit));
        }
        I2cRequest::SetVoltage {
            addr,
            firmware,
            pic_val,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_set_voltage(i2c, addr, firmware, pic_val, permit));
        }
        I2cRequest::Pic16SetVoltageOnly {
            addr,
            pic_val,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_pic16_set_voltage_only(i2c, addr, pic_val, permit));
        }
        I2cRequest::DisableVoltage {
            addr,
            firmware,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_disable_voltage(i2c, addr, firmware, permit));
        }
        I2cRequest::SetVoltageMv {
            addr,
            voltage_mv,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_set_voltage_mv(i2c, addr, voltage_mv, permit));
        }
        I2cRequest::Pic16JumpIfBootloader { addr, reply_tx } => {
            let _ = reply_tx.send(execute_pic16_jump_if_bootloader(i2c, addr, permit));
        }
        I2cRequest::Pic16Admission { reply_tx, .. } => {
            let _ = reply_tx.send(Err(HalError::I2cAdmissionBusy {
                bus: i2c.bus,
                addr: 0,
                detail: "PIC16 admission request bypassed the worker scheduler".into(),
            }));
        }
        I2cRequest::Pic16HeartbeatRound { reply_tx, .. } => {
            let _ = reply_tx.send(Err(HalError::I2cAdmissionBusy {
                bus: i2c.bus,
                addr: 0,
                detail: "PIC16 heartbeat round bypassed the worker scheduler".into(),
            }));
        }
        I2cRequest::WriteBytes {
            addr,
            data,
            reply_tx,
        } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "generic write")
                .and_then(|_stage| {
                    i2c.set_slave(addr)
                        .and_then(|_| i2c.write(&data).map(|_| ()))
                });
            let _ = reply_tx.send(result);
        }
        I2cRequest::WriteByteByte {
            addr,
            data,
            reply_tx,
        } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "generic bytewise write")
                .and_then(|_stage| {
                    i2c.set_slave(addr)
                        .and_then(|_| i2c.write_byte_by_byte(&data))
                });
            let _ = reply_tx.send(result);
        }
        I2cRequest::ReadBytes {
            addr,
            len,
            reply_tx,
        } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "generic read")
                .and_then(|_stage| {
                    i2c.set_slave(addr).and_then(|_| {
                        let mut buf = vec![0_u8; len];
                        i2c.read(&mut buf).map(|count| {
                            buf.truncate(count);
                            buf
                        })
                    })
                });
            let _ = reply_tx.send(result);
        }
        I2cRequest::ReadHashboardEepromPrefix { addr, reply_tx } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "hashboard EEPROM identity-prefix read")
                .and_then(|_stage| i2c.read_protected_hashboard_eeprom_prefix(addr));
            let _ = reply_tx.send(result);
        }
        I2cRequest::ReadLm75TemperatureRegister { addr, reply_tx } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "LM75 temperature-register query prelude")
                .and_then(|_stage| i2c.read_lm75_temperature_register(addr));
            let _ = reply_tx.send(result);
        }
        I2cRequest::WriteRead {
            addr,
            write_data,
            read_len,
            reply_tx,
        } => {
            let result = permit
                .begin_stage(i2c.bus, addr, "generic write-read")
                .and_then(|_stage| {
                    i2c.set_slave(addr).and_then(|_| {
                        let mut buf = vec![0_u8; read_len];
                        i2c.write_read(&write_data, &mut buf).map(|_| buf)
                    })
                });
            let _ = reply_tx.send(result);
        }
        I2cRequest::SetTimeout {
            timeout_jiffies,
            reply_tx,
        } => {
            let result = permit
                .begin_stage(i2c.bus, 0, "set service timeout")
                .and_then(|_stage| i2c.set_timeout(timeout_jiffies));
            let _ = reply_tx.send(result);
        }
        I2cRequest::RecoverUnmanagedBus { reply_tx } => {
            let _ = reply_tx.send(execute_unmanaged_bus_recovery(i2c, permit));
        }
        I2cRequest::Transaction {
            addr,
            steps,
            reply_tx,
        } => {
            let _ = reply_tx.send(execute_transaction(i2c, addr, steps, permit));
        }
        I2cRequest::ConditionalSafeOffPlan {
            addr,
            prelude,
            primary,
            compensation,
            reply_tx,
        } => {
            let (outcome, _) = execute_conditional_safe_off_plan(
                i2c,
                addr,
                &prelude,
                &primary,
                &compensation,
                permit,
            );
            let _ = reply_tx.send(Ok(outcome));
        }
    }
}

fn execute_unmanaged_bus_recovery(i2c: &mut I2cBus, permit: &I2cSafetyPermit) -> Result<()> {
    let _stage = permit.begin_stage(i2c.bus, 0, "whole-fabric recovery")?;
    let service_state = permit
        .authority
        .pic16_service_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if service_state.has_managed_addresses() {
        return Err(managed_pic16_fabric_error(i2c.bus));
    }
    if permit
        .authority
        .pic16_admission_owner
        .load(Ordering::SeqCst)
        != PIC16_ADMISSION_IDLE
    {
        return Err(hal_busy_error(i2c.bus, 0));
    }

    // Publication of a managed batch takes this same lock. Holding it through
    // recovery makes the winner deterministic: either recovery finishes while
    // the fabric is still unmanaged, or admission publishes first and recovery
    // is permanently refused without touching the controller.
    i2c.bus_recovery()?;
    drop(service_state);
    Ok(())
}

fn spawn_i2c_service_with_policy(
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: Vec<u8>,
) -> std::io::Result<I2cServiceHandle> {
    // Bounded channel: 64 slots avoids unbounded growth if callers outpace the bus.
    let (tx, rx) = mpsc::sync_channel::<I2cServiceEnvelope>(64);
    let safety = Arc::new(I2cSafetyAuthority::default());
    let registry =
        I2cServiceRegistryLease::reserve(I2cFabricRegistryKey::linux_adapter(bus), &safety)?;
    spawn_reserved_i2c_service_with_policy(
        bus,
        use_devmem,
        restore_kernel_registers,
        write_denylist,
        tx,
        rx,
        safety,
        registry,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_reserved_i2c_service_with_policy(
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: Vec<u8>,
    tx: mpsc::SyncSender<I2cServiceEnvelope>,
    rx: mpsc::Receiver<I2cServiceEnvelope>,
    safety: Arc<I2cSafetyAuthority>,
    registry: I2cServiceRegistryLease,
) -> std::io::Result<I2cServiceHandle> {
    let worker_safety = Arc::clone(&safety);
    let safe_off_mailbox = Arc::new(I2cSafeOffMailbox::default());
    let worker_safe_off_mailbox = Arc::clone(&safe_off_mailbox);

    std::thread::Builder::new()
        .name("i2c-service".to_string())
        .spawn(move || {
            let loop_safety = Arc::clone(&worker_safety);
            let worker_lifetime = match I2cServiceWorkerLifetime::new(worker_safety, registry) {
                Ok(lifetime) => lifetime,
                Err(error) => {
                    tracing::error!(bus, %error, "I2C worker could not activate its fabric reservation");
                    return;
                }
            };
            i2c_service_loop(
                bus,
                rx,
                use_devmem,
                restore_kernel_registers,
                write_denylist,
                worker_safe_off_mailbox,
                loop_safety,
            );
            drop(worker_lifetime);
        })?;

    Ok(I2cServiceHandle {
        bus,
        tx: I2cServiceSender::Deadline(tx),
        safety,
        safe_off_mailbox: Some(safe_off_mailbox),
    })
}

fn reopen_i2c_service_bus(
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: &[u8],
    authority: &Arc<I2cSafetyAuthority>,
) -> Option<I2cBus> {
    let mut reopened = if use_devmem {
        I2cBus::open_devmem_for_service(authority).ok()
    } else {
        I2cBus::open_for_service(bus, authority).ok()
    };
    if let Some(ref mut i2c) = reopened {
        if !write_denylist.is_empty() {
            i2c.set_write_denylist(write_denylist);
        }
        if let Err(error) = i2c.set_timeout(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES) {
            tracing::error!(
                bus,
                %error,
                "reopened I2C transport could not verify its bounded timeout; PIC16 runtime jobs will refuse wire access"
            );
        }
        if !use_devmem && restore_kernel_registers {
            if let Err(error) = restore_kernel_i2c_interrupts() {
                tracing::warn!(
                    bus,
                    %error,
                    "failed to restore I2C timing registers after service reopen"
                );
            }
        } else if !use_devmem {
            tracing::debug!(
                bus,
                "I2C fd reopened; AXI IIC register restore skipped by policy"
            );
        }
    }
    reopened
}

/// Main loop for the I2C service thread. Owns the bus and processes requests.
fn i2c_service_loop(
    bus: u8,
    rx: mpsc::Receiver<I2cServiceEnvelope>,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: Vec<u8>,
    safe_off_mailbox: Arc<I2cSafeOffMailbox>,
    safety: Arc<I2cSafetyAuthority>,
) {
    let mut lifecycle = I2cSafeOffWorkerLifecycle::new(Arc::clone(&safe_off_mailbox), bus);
    let apply_denylist = |i2c: &mut I2cBus| {
        if !write_denylist.is_empty() {
            i2c.set_write_denylist(&write_denylist);
        }
    };
    let mut i2c_bus: Option<I2cBus> = if use_devmem {
        I2cBus::open_devmem_for_service(&safety)
            .map(|mut bus| {
                apply_denylist(&mut bus);
                bus
            })
            .ok()
    } else {
        match I2cBus::open_for_service(bus, &safety) {
            Ok(mut b) => {
                apply_denylist(&mut b);
                Some(b)
            }
            Err(_) => None,
        }
    };
    // v0.14.0: Set 100ms timeout matching the init heartbeat thread.
    // The init heartbeat uses set_timeout(10) (10 jiffies = 100ms) and works
    // for ALL 3 PICs. The mining heartbeat used the default 1000ms and failed.
    if let Some(ref i2c) = i2c_bus {
        if let Err(error) = i2c.set_timeout(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES) {
            tracing::error!(
                bus,
                %error,
                "I2C service could not verify its bounded timeout; PIC16 runtime jobs will refuse wire access"
            );
        }
    }
    if !use_devmem && restore_kernel_registers {
        if let Err(e) = restore_kernel_i2c_interrupts() {
            tracing::warn!(
                "Failed to restore I2C timing registers on service start: {}",
                e
            );
        }
    } else if !use_devmem {
        tracing::info!(
            bus,
            "I2C service using kernel fd only; AXI IIC register restore disabled by policy"
        );
    }
    let mut last_reset_time = std::time::Instant::now();
    let mut consecutive_resets: u32 = 0;
    let mut pic16_job: Option<Pic16WorkerJob> = None;
    let mut ordinary_disconnected = false;

    tracing::info!(
        bus,
        use_devmem,
        "I2C service thread started — all PIC I/O serialized through this thread (timeout=100ms)"
    );

    loop {
        if let Some(pending) = safe_off_mailbox.take_next() {
            if let Some(job) = pic16_job.as_ref() {
                job.request_cancel();
            }
            if i2c_bus.is_none() {
                i2c_bus = reopen_i2c_service_bus(
                    bus,
                    use_devmem,
                    restore_kernel_registers,
                    &write_denylist,
                    &safety,
                );
            }
            execute_pending_safe_off_with_recovery(
                pending,
                bus,
                use_devmem,
                restore_kernel_registers,
                &write_denylist,
                &mut i2c_bus,
                &mut last_reset_time,
                &mut consecutive_resets,
                &safety,
            );
            continue;
        }

        if let Some(job) = pic16_job.as_mut() {
            if job.requires_bus() && i2c_bus.is_none() {
                i2c_bus = reopen_i2c_service_bus(
                    bus,
                    use_devmem,
                    restore_kernel_registers,
                    &write_denylist,
                    &safety,
                );
                if i2c_bus.is_none() {
                    job.transport_unavailable("I2C bus reopen failed during PIC16 worker job");
                }
            }

            let step = if job.requires_bus() {
                let i2c = i2c_bus.as_mut().expect("admission bus was reopened");
                job.advance(i2c, Instant::now())
            } else {
                job.advance_without_bus(Instant::now())
            };
            if step.transport_fault {
                recover_i2c_backend(
                    bus,
                    use_devmem,
                    restore_kernel_registers,
                    &mut i2c_bus,
                    &mut last_reset_time,
                    &mut consecutive_resets,
                    &write_denylist,
                );
            } else {
                consecutive_resets = 0;
            }
            if step.shutdown_active_batch {
                if let Some(pending) = active_pic16_batch_shutdown(&safety) {
                    execute_pending_safe_off_with_recovery(
                        pending,
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &write_denylist,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &safety,
                    );
                }
            }
            if step.finished {
                pic16_job = None;
                continue;
            }
            let now = Instant::now();
            let wait = step
                .next_due
                .map(|due| due.saturating_duration_since(now))
                .unwrap_or(Duration::ZERO)
                .min(I2C_SAFE_OFF_POLL_INTERVAL);
            match rx.recv_timeout(wait) {
                Ok(envelope) => reject_envelope_during_pic16_job(envelope, bus),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    ordinary_disconnected = true;
                    job.request_cancel();
                }
            }
            continue;
        }

        if ordinary_disconnected {
            safe_off_mailbox.begin_close();
            while let Some(pending) = safe_off_mailbox.take_next() {
                if i2c_bus.is_none() {
                    i2c_bus = reopen_i2c_service_bus(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &write_denylist,
                        &safety,
                    );
                }
                execute_pending_safe_off_with_recovery(
                    pending,
                    bus,
                    use_devmem,
                    restore_kernel_registers,
                    &write_denylist,
                    &mut i2c_bus,
                    &mut last_reset_time,
                    &mut consecutive_resets,
                    &safety,
                );
            }
            if let Some(pending) = active_pic16_batch_shutdown(&safety) {
                if i2c_bus.is_none() {
                    i2c_bus = reopen_i2c_service_bus(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &write_denylist,
                        &safety,
                    );
                }
                execute_pending_safe_off_with_recovery(
                    pending,
                    bus,
                    use_devmem,
                    restore_kernel_registers,
                    &write_denylist,
                    &mut i2c_bus,
                    &mut last_reset_time,
                    &mut consecutive_resets,
                    &safety,
                );
            }
            lifecycle.finish();
            break;
        }

        let envelope = match rx.recv_timeout(I2C_SAFE_OFF_POLL_INTERVAL) {
            Ok(envelope) => envelope,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Stop admission under the mailbox lock, then recheck and
                // drain everything accepted before the ordinary channel was
                // observed disconnected. This closes the check/recv race.
                safe_off_mailbox.begin_close();
                while let Some(pending) = safe_off_mailbox.take_next() {
                    if i2c_bus.is_none() {
                        i2c_bus = reopen_i2c_service_bus(
                            bus,
                            use_devmem,
                            restore_kernel_registers,
                            &write_denylist,
                            &safety,
                        );
                    }
                    execute_pending_safe_off_with_recovery(
                        pending,
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &write_denylist,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &safety,
                    );
                }
                if let Some(pending) = active_pic16_batch_shutdown(&safety) {
                    if i2c_bus.is_none() {
                        i2c_bus = reopen_i2c_service_bus(
                            bus,
                            use_devmem,
                            restore_kernel_registers,
                            &write_denylist,
                            &safety,
                        );
                    }
                    execute_pending_safe_off_with_recovery(
                        pending,
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &write_denylist,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &safety,
                    );
                }
                lifecycle.finish();
                break;
            }
        };
        let Some((req, state, permit)) = start_envelope_at(envelope, bus, Instant::now()) else {
            continue;
        };
        if let Some(conflict) = pic16_request_conflict(&req, &permit) {
            reject_pic16_request_conflict(req, &permit, conflict, bus);
            state.store(I2C_REQUEST_FINISHED, Ordering::Release);
            continue;
        }

        // Reopen bus if previous operations lost the fd
        if i2c_bus.is_none() {
            i2c_bus = reopen_i2c_service_bus(
                bus,
                use_devmem,
                restore_kernel_registers,
                &write_denylist,
                &safety,
            );
            if i2c_bus.is_none() {
                reply_i2c_request_error(req, bus, "bus reopen failed");
                state.store(I2C_REQUEST_FINISHED, Ordering::Release);
                continue;
            }
        }

        let i2c = i2c_bus.as_mut().unwrap();

        match req {
            I2cRequest::Pic16Admission {
                plans,
                batch,
                cancellation,
                reservation,
                completion_tx,
                reply_tx,
            } => match reservation.activate() {
                Ok(reservation_token) => {
                    let job = Pic16AdmissionState::new(
                        bus,
                        permit,
                        cancellation,
                        completion_tx,
                        plans,
                        batch,
                        reservation_token,
                    );
                    if reply_tx.send(Ok(())).is_err() {
                        job.request_cancel();
                    }
                    pic16_job = Some(Pic16WorkerJob::admission(job));
                }
                Err(error) => {
                    let _ = reply_tx.send(Err(error));
                }
            },
            I2cRequest::Pic16HeartbeatRound { batch, reply_tx } => {
                let job = Pic16HeartbeatRoundState::new(bus, permit, batch, reply_tx, state);
                pic16_job = Some(Pic16WorkerJob::heartbeat_round(job));
                continue;
            }
            I2cRequest::Heartbeat {
                addr,
                firmware,
                reply_tx,
            } => {
                let result = execute_heartbeat(i2c, addr, firmware, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::SetVoltage {
                addr,
                firmware,
                pic_val,
                reply_tx,
            } => {
                let result = execute_set_voltage(i2c, addr, firmware, pic_val, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::Pic16SetVoltageOnly {
                addr,
                pic_val,
                reply_tx,
            } => {
                let result = execute_pic16_set_voltage_only(i2c, addr, pic_val, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::DisableVoltage {
                addr,
                firmware,
                reply_tx,
            } => {
                let result = execute_disable_voltage(i2c, addr, firmware, &permit);
                let _ = reply_tx.send(result);
            }
            I2cRequest::SetVoltageMv {
                addr,
                voltage_mv,
                reply_tx,
            } => {
                let result = execute_set_voltage_mv(i2c, addr, voltage_mv, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::Pic16JumpIfBootloader { addr, reply_tx } => {
                let result = execute_pic16_jump_if_bootloader(i2c, addr, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            // --- v0.13.0: Generic I2C operations ---
            I2cRequest::WriteBytes {
                addr,
                data,
                reply_tx,
            } => {
                let result = permit
                    .begin_stage(bus, addr, "generic write")
                    .and_then(|_stage| {
                        i2c.set_slave(addr)?;
                        i2c.write(&data).map(|_| ())
                    });
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::WriteByteByte {
                addr,
                data,
                reply_tx,
            } => {
                let result = permit
                    .begin_stage(bus, addr, "generic bytewise write")
                    .and_then(|_stage| {
                        i2c.set_slave(addr)?;
                        i2c.write_byte_by_byte(&data)
                    });
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::ReadBytes {
                addr,
                len,
                reply_tx,
            } => {
                let result = permit
                    .begin_stage(bus, addr, "generic read")
                    .and_then(|_stage| {
                        i2c.set_slave(addr)?;
                        let mut buf = vec![0u8; len];
                        i2c.read(&mut buf).map(|count| {
                            buf.truncate(count);
                            buf
                        })
                    });
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::ReadHashboardEepromPrefix { addr, reply_tx } => {
                let result = permit
                    .begin_stage(bus, addr, "hashboard EEPROM identity-prefix read")
                    .and_then(|_stage| i2c.read_protected_hashboard_eeprom_prefix(addr));
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::ReadLm75TemperatureRegister { addr, reply_tx } => {
                let result = permit
                    .begin_stage(bus, addr, "LM75 temperature-register query prelude")
                    .and_then(|_stage| i2c.read_lm75_temperature_register(addr));
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::WriteRead {
                addr,
                write_data,
                read_len,
                reply_tx,
            } => {
                let result = permit
                    .begin_stage(bus, addr, "generic write-read")
                    .and_then(|_stage| {
                        i2c.set_slave(addr)?;
                        let mut buf = vec![0u8; read_len];
                        i2c.write_read(&write_data, &mut buf).map(|_| buf)
                    });
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::SetTimeout {
                timeout_jiffies,
                reply_tx,
            } => {
                let result = permit
                    .begin_stage(bus, 0, "set service timeout")
                    .and_then(|_stage| i2c.set_timeout(timeout_jiffies));
                let _ = reply_tx.send(result);
            }
            I2cRequest::RecoverUnmanagedBus { reply_tx } => {
                let _ = reply_tx.send(execute_unmanaged_bus_recovery(i2c, &permit));
            }
            I2cRequest::Transaction {
                addr,
                steps,
                reply_tx,
            } => {
                let result = execute_transaction(i2c, addr, steps, &permit);
                if i2c_result_requires_transport_recovery(&result) {
                    recover_i2c_backend(
                        bus,
                        use_devmem,
                        restore_kernel_registers,
                        &mut i2c_bus,
                        &mut last_reset_time,
                        &mut consecutive_resets,
                        &write_denylist,
                    );
                } else {
                    consecutive_resets = 0;
                }
                let _ = reply_tx.send(result);
            }
            I2cRequest::ConditionalSafeOffPlan {
                addr,
                prelude,
                primary,
                compensation,
                reply_tx,
            } => {
                let (outcome, _) = execute_conditional_safe_off_plan(
                    i2c,
                    addr,
                    &prelude,
                    &primary,
                    &compensation,
                    &permit,
                );
                let _ = reply_tx.send(Ok(outcome));
            }
        }
        state.store(I2C_REQUEST_FINISHED, Ordering::Release);
    }

    tracing::info!("I2C service thread exiting — channel closed");
}

/// Return true only for failures that indicate the Linux/controller transport
/// may be unhealthy. Safety-generation cancellation is an intentional control
/// outcome: resetting the bus for it can delay the reserved shutdown work that
/// caused the cancellation.
fn i2c_result_requires_transport_recovery<T>(result: &Result<T>) -> bool {
    matches!(result, Err(HalError::I2c { .. }))
}

fn recover_i2c_backend(
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    i2c_bus: &mut Option<I2cBus>,
    last_reset: &mut std::time::Instant,
    consecutive_resets: &mut u32,
    write_denylist: &[u8],
) {
    if use_devmem {
        *consecutive_resets += 1;
        // WAVE-0: escalating AXI IIC recovery instead of SCL-pulses-only. The
        // tier is chosen from the consecutive-failure count AND the live SR
        // (a BusBusyHung / ControllerDown state jumps straight to a full
        // controller re-init — the only thing that recovers a wedged
        // controller, which bare SCL pulses never did). See
        // `axi_iic_escalating_recovery`.
        let tier = match axi_iic_escalating_recovery(*consecutive_resets) {
            Ok(tier) => tier,
            Err(error) => {
                tracing::error!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    %error,
                    "I2C transport recovery failed its hardware postcondition"
                );
                *i2c_bus = None;
                return;
            }
        };
        // The worker may recover controller state while its transport is
        // absent, but only `reopen_i2c_service_bus` may recreate that
        // transport because it revalidates the service's fabric authority.

        // Rate-limited logging: log the first few of a streak, the moment we
        // escalate off SCL pulses, the give-up boundary, then go quiet (every
        // 50th) so a dead PIC / wedged bus cannot flood the log ring.
        let log_now = *consecutive_resets <= 3
            || (tier != AxiIicRecoveryTier::SclPulses
                && *consecutive_resets <= AXI_IIC_GIVE_UP_AFTER)
            || (*consecutive_resets).is_multiple_of(50);
        if log_now {
            match tier {
                AxiIicRecoveryTier::SclPulses => tracing::warn!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus recovery: SCL clock pulses"
                ),
                AxiIicRecoveryTier::FullControllerReset => tracing::warn!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus recovery: full AXI IIC controller reset (SCL pulses insufficient — controller wedged)"
                ),
                AxiIicRecoveryTier::GiveUp => tracing::error!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus: {} consecutive recoveries — giving up escalation; PIC likely dead, bus wedged, or 12V absent. Per-PIC back-off now governs reprobe cadence.",
                    *consecutive_resets,
                ),
            }
        }
        return;
    }

    try_reset_and_reopen(
        bus,
        restore_kernel_registers,
        i2c_bus,
        last_reset,
        consecutive_resets,
        write_denylist,
    );
}

/// Execute a heartbeat command via the I2C bus.
fn execute_pic16_jump_if_bootloader(
    i2c: &mut I2cBus,
    addr: u8,
    permit: &I2cSafetyPermit,
) -> Result<I2cPic16JumpOutcome> {
    validate_pic16_boot_transition_endpoint(i2c.bus, addr)?;

    let raw_state = read_pic16_raw_exact_worker(i2c, addr, permit, "PIC16 exact boot-state read")?;

    if raw_state != 0xCC {
        return Ok(I2cPic16JumpOutcome::ObservedNoJump { raw_state });
    }

    execute_pic16_jump_only(i2c, addr, permit)?;

    std::thread::sleep(std::time::Duration::from_millis(500));
    let post_jump_raw_state =
        read_pic16_raw_exact_worker(i2c, addr, permit, "PIC16 exact post-JUMP application read")?;

    Ok(I2cPic16JumpOutcome::JumpSentFromExactBootloader {
        post_jump_raw_state,
    })
}

/// Execute exactly one bounded fixed JUMP wire action. The worker-owned
/// admission scheduler owns the post-JUMP settle deadline.
fn execute_pic16_jump_only(i2c: &mut I2cBus, addr: u8, permit: &I2cSafetyPermit) -> Result<()> {
    validate_pic16_boot_transition_endpoint(i2c.bus, addr)?;
    let _jump_stage = permit.begin_stage(i2c.bus, addr, "PIC16 fixed JUMP frame")?;
    i2c.set_slave(addr)?;
    if let Err(error) = i2c.write_byte_by_byte(&[
        pic_cmd::PREAMBLE[0],
        pic_cmd::PREAMBLE[1],
        pic_cmd::JUMP_FROM_LOADER,
    ]) {
        let _ = i2c.set_slave(addr);
        let _ = i2c.write_byte_by_byte(&[0_u8; 16]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        return Err(error);
    }
    Ok(())
}

fn read_pic16_raw_exact_worker(
    i2c: &mut I2cBus,
    addr: u8,
    permit: &I2cSafetyPermit,
    stage: &'static str,
) -> Result<u8> {
    let _read_stage = permit.begin_stage(i2c.bus, addr, stage)?;
    i2c.set_slave(addr)?;
    let mut raw = [0_u8; 1];
    let count = i2c.read(&mut raw)?;
    if count != raw.len() {
        return Err(HalError::I2c {
            bus: i2c.bus,
            addr,
            detail: format!(
                "PIC16 raw-state read returned {count} byte(s); exact one-byte evidence is required"
            ),
        });
    }
    Ok(raw[0])
}

/// Execute a heartbeat command via the I2C bus.
///
/// v0.15.0: Simple service-fd write. The root cause of heartbeat failures was
/// AXI bus contention during FPGA WORK_TX bursts (open-core), NOT electrical
/// noise. The fix is pausing heartbeats during FPGA bursts (done in daemon.rs).
/// With the pause in place, heartbeats run in AXI-quiet windows and succeed
/// reliably — no aggressive retries needed.
fn execute_heartbeat(
    i2c: &mut I2cBus,
    addr: u8,
    _firmware: I2cPicFirmware,
    permit: &I2cSafetyPermit,
) -> Result<()> {
    let _stage = permit.begin_stage(i2c.bus, addr, "heartbeat frame")?;
    let cmd: [u8; 3] = [pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], 0x16];
    i2c.set_slave(addr)?;
    if let Err(e) = i2c.write_byte_by_byte(&cmd) {
        // Flush PIC MSSP parser — 16 zero bytes push past any partial command state.
        let _ = i2c.set_slave(addr);
        let _ = i2c.write_byte_by_byte(&[0u8; 16]);
        std::thread::sleep(std::time::Duration::from_millis(5));
        return Err(e);
    }
    Ok(())
}

/// Execute a set_voltage command via the I2C bus.
/// All PIC16F1704 app-mode firmwares use SET_VOLTAGE=0x10 followed by ENABLE=0x15.
///
/// SAFETY CLAMP (2026-04-16, flash-readiness review): any caller path that
/// reaches this function is clamped here to the PIC16F1704 minimum-safe DAC
/// value. This is defense-in-depth: the canonical clamp lives at
/// `dcentrald-asic::pic::PicController::set_voltage` (const `MIN_SAFE_PIC_VALUE = 6`
/// → 9.40 V), but `I2cServiceHandle::set_voltage` can reach `execute_set_voltage`
/// without going through that path. A DAC value below 6 (e.g. 0 = 9.44 V) stresses
/// the BM1387's LM27402SQ/TPHR9003NL rail — unsafe even when thermal is in spec.
/// The value is kept in sync with `pic.rs::MIN_SAFE_PIC_VALUE`.
const MIN_SAFE_PIC_DAC_VALUE: u8 = 6; // 9.40 V — see dcentrald-asic::pic

#[inline]
fn clamp_pic_voltage_dac(pic_val: u8) -> u8 {
    pic_val.max(MIN_SAFE_PIC_DAC_VALUE)
}

fn execute_set_voltage(
    i2c: &mut I2cBus,
    addr: u8,
    _firmware: I2cPicFirmware,
    pic_val: u8,
    permit: &I2cSafetyPermit,
) -> Result<()> {
    execute_pic16_set_voltage_only(i2c, addr, pic_val, permit)?;
    std::thread::sleep(std::time::Duration::from_millis(5));
    execute_pic16_enable_only(i2c, addr, permit)
}

/// Execute exactly one bounded fixed SET_VOLTAGE wire action.
fn execute_pic16_set_voltage_only(
    i2c: &mut I2cBus,
    addr: u8,
    pic_val: u8,
    permit: &I2cSafetyPermit,
) -> Result<()> {
    let clamped = clamp_pic_voltage_dac(pic_val);
    if clamped != pic_val {
        tracing::warn!(
            addr = format_args!("0x{:02X}", addr),
            requested = pic_val,
            clamped = clamped,
            "PIC voltage DAC value {} clamped to {} at i2c layer (9.40V safety cap)",
            pic_val,
            clamped,
        );
    }
    {
        let _set_stage = permit.begin_stage(i2c.bus, addr, "PIC SET_VOLTAGE frame")?;
        i2c.set_slave(addr)?;
        if let Err(e) =
            i2c.write_byte_by_byte(&[pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], 0x10, clamped])
        {
            // Flush PIC MSSP parser — 16 zero bytes push past any partial command state.
            let _ = i2c.set_slave(addr);
            let _ = i2c.write_byte_by_byte(&[0u8; 16]);
            std::thread::sleep(std::time::Duration::from_millis(10));
            return Err(e);
        }
    }
    Ok(())
}

/// Execute exactly one bounded fixed ENABLE_VOLTAGE wire action.
fn execute_pic16_enable_only(i2c: &mut I2cBus, addr: u8, permit: &I2cSafetyPermit) -> Result<()> {
    let _enable_stage = permit.begin_stage(i2c.bus, addr, "PIC ENABLE frame")?;
    i2c.set_slave(addr)?;
    if let Err(e) =
        i2c.write_byte_by_byte(&[pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], 0x15, 0x01])
    {
        let _ = i2c.set_slave(addr);
        let _ = i2c.write_byte_by_byte(&[0u8; 16]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        return Err(e);
    }
    Ok(())
}

/// Execute a disable_voltage command via the I2C bus.
fn execute_disable_voltage(
    i2c: &mut I2cBus,
    addr: u8,
    _firmware: I2cPicFirmware,
    permit: &I2cSafetyPermit,
) -> Result<()> {
    let _stage = permit.begin_stage(i2c.bus, addr, "PIC DISABLE frame")?;
    i2c.set_slave(addr)?;
    if let Err(e) =
        i2c.write_byte_by_byte(&[pic_cmd::PREAMBLE[0], pic_cmd::PREAMBLE[1], 0x15, 0x00])
    {
        // Flush PIC MSSP parser — 16 zero bytes push past any partial command
        // state after a NACK (mandatory parser-flush rule; parity with
        // execute_heartbeat / execute_set_voltage). A NACK on the disable frame
        // otherwise leaves the parser corrupted and a subsequent in-session
        // heartbeat/re-enable can NACK in turn.
        let _ = i2c.set_slave(addr);
        let _ = i2c.write_byte_by_byte(&[0u8; 16]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        return Err(e);
    }
    Ok(())
}

/// dsPIC chain-rail voltage hard cap (mV) — the load-bearing <=14500 mV safety
/// rule. Defense-in-depth at the HAL boundary, mirroring `execute_set_voltage`'s
/// MIN clamp: `I2cServiceHandle::set_voltage_mv` is public and reaches
/// `execute_set_voltage_mv` directly, so a caller (or a unit-conversion bug) that
/// exceeds the cap must not program an over-voltage rail with no backstop.
/// (gap-swarm HAL-safety #4)
const DSPIC_RAIL_HARD_CAP_MV: u16 = 14_500;

/// Clamp a requested dsPIC chain-rail voltage to the <=14500 mV hard cap. Pure +
/// host-testable. NOTE: this is the HAL backstop only; the coordinated
/// enforcement at the dsPIC DRIVER (dcentrald-asic, DSPIC_MAX_VOLTAGE_MV=15140)
/// + the autotuner 15000 ceiling + the AMTC 15.0V lab-pre-open override is a
/// separate EE-reviewed pass (see reference_voltage_hard_cap_not_enforced_at_driver_boundaries).
#[inline]
fn clamp_dspic_mv(requested: u16) -> u16 {
    requested.min(DSPIC_RAIL_HARD_CAP_MV)
}

/// Execute a set_voltage_mv command via I2C for dsPIC33EP (S19 Pro / S17 style).
/// Sends a 16-bit millivolt value as two bytes (big-endian) with preamble + SET_VOLTAGE cmd.
fn execute_set_voltage_mv(
    i2c: &mut I2cBus,
    addr: u8,
    voltage_mv: u16,
    permit: &I2cSafetyPermit,
) -> Result<()> {
    let _stage = permit.begin_stage(i2c.bus, addr, "dsPIC SET_VOLTAGE frame")?;
    // Defense-in-depth <=14500 mV hard cap (no live caller passes >13700 today —
    // the asic dsPIC path builds framed bytes + uses transaction() — so this is
    // zero-behavior-change today; it only changes the wire if a future/erroneous
    // caller exceeds the cap, which is the intended safety-tightening).
    let capped = clamp_dspic_mv(voltage_mv);
    if capped != voltage_mv {
        tracing::warn!(
            addr = format_args!("0x{:02X}", addr),
            requested_mv = voltage_mv,
            capped_mv = capped,
            "dsPIC chain-rail voltage {} mV clamped to {} mV at i2c layer (<=14500 mV hard cap)",
            voltage_mv,
            capped,
        );
    }
    let hi = (capped >> 8) as u8;
    let lo = (capped & 0xFF) as u8;
    i2c.set_slave(addr)?;
    // dsPIC SET_VOLTAGE: [preamble, cmd, voltage_hi, voltage_lo]
    if let Err(e) = i2c.write_byte_by_byte(&[0x55, 0xAA, 0x10, hi, lo]) {
        // Flush dsPIC MSSP parser — 16 zero bytes push past any partial command
        // state after a NACK (parity with execute_set_voltage; canonical dsPIC
        // flush per dspic flush_parser). Dead path today (no live caller reaches
        // I2cServiceHandle::set_voltage_mv for a dsPIC), but hardened so a future
        // caller cannot reintroduce the .139/.74 unflushed-parser corruption.
        let _ = i2c.set_slave(addr);
        let _ = i2c.write_byte_by_byte(&[0u8; 16]);
        std::thread::sleep(std::time::Duration::from_millis(10));
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod dspic_mv_clamp_tests {
    use super::clamp_dspic_mv;

    #[test]
    fn clamp_dspic_mv_caps_at_hard_cap() {
        assert_eq!(clamp_dspic_mv(13_700), 13_700); // production target — unchanged
        assert_eq!(clamp_dspic_mv(13_800), 13_800);
        assert_eq!(clamp_dspic_mv(14_500), 14_500); // boundary
        assert_eq!(clamp_dspic_mv(15_000), 14_500); // over-cap → clamped
        assert_eq!(clamp_dspic_mv(15_140), 14_500); // dsPIC envelope max → clamped
    }
}

fn execute_transaction(
    i2c: &mut I2cBus,
    addr: u8,
    steps: Vec<I2cTransactionStep>,
    permit: &I2cSafetyPermit,
) -> Result<Vec<Vec<u8>>> {
    let bus = i2c.bus;
    let timeout_changed = steps
        .iter()
        .any(|step| matches!(step, I2cTransactionStep::SetTimeout(_)));
    let result = execute_transaction_steps(i2c, bus, addr, steps, permit);

    if timeout_changed {
        let _cleanup_stage = permit.begin_terminal_safe_cleanup_stage();
        if let Err(restore_error) = i2c.set_timeout(10) {
            if result.is_ok() {
                return Err(restore_error);
            }
            tracing::warn!(
                bus,
                addr = format_args!("0x{:02X}", addr),
                error = %restore_error,
                "failed to restore the service I2C timeout after a failed compound transaction"
            );
        }
    }
    result
}

fn execute_transaction_steps(
    i2c: &mut I2cBus,
    bus: u8,
    addr: u8,
    steps: Vec<I2cTransactionStep>,
    permit: &I2cSafetyPermit,
) -> Result<Vec<Vec<u8>>> {
    {
        let _select_stage =
            permit.begin_stage(bus, addr, "compound transaction slave selection")?;
        i2c.set_slave(addr)?;
    }
    let mut reads = Vec::new();

    for step in steps {
        // A sleep deliberately carries no authorization into the following
        // controller operation. Every I/O/control step acquires a fresh lease
        // so a terminal barrier during a dwell prevents the next mutation.
        let _stage = if matches!(step, I2cTransactionStep::SleepMs(_)) {
            None
        } else {
            Some(permit.begin_stage(bus, addr, "compound transaction step")?)
        };
        match step {
            I2cTransactionStep::Write(data) => {
                i2c.write(&data)?;
            }
            I2cTransactionStep::WriteByteByByte(data) => {
                i2c.write_byte_by_byte(&data)?;
            }
            I2cTransactionStep::Read(len) => {
                let mut buf = vec![0u8; len];
                let n = i2c.read(&mut buf)?;
                buf.truncate(n);
                reads.push(buf);
            }
            I2cTransactionStep::ReadFrame {
                header_len,
                len_index,
                remaining_adjust,
                max_len,
            } => {
                if header_len == 0 || len_index >= header_len {
                    return Err(HalError::I2c {
                        bus,
                        addr,
                        detail: "transaction ReadFrame invalid header/len index".into(),
                    });
                }

                let mut header = vec![0u8; header_len];
                let n = i2c.read(&mut header)?;
                header.truncate(n);

                // APW NAK may be a one-byte response. Return the short read so
                // the protocol parser can classify it rather than hiding it.
                if header.len() < header_len {
                    reads.push(header);
                    continue;
                }

                let remaining_i = i16::from(header[len_index]).saturating_add(remaining_adjust);
                if remaining_i < 0 {
                    return Err(HalError::I2c {
                        bus,
                        addr,
                        detail: "transaction ReadFrame negative remaining length".into(),
                    });
                }
                let remaining = remaining_i as usize;
                if header.len().saturating_add(remaining) > max_len {
                    return Err(HalError::I2c {
                        bus,
                        addr,
                        detail: format!(
                            "transaction ReadFrame length {} exceeds max {}",
                            header.len() + remaining,
                            max_len
                        ),
                    });
                }

                let mut full = header;
                if remaining > 0 {
                    let start = full.len();
                    full.resize(start + remaining, 0);
                    let n_tail = i2c.read(&mut full[start..])?;
                    full.truncate(start + n_tail);
                }
                reads.push(full);
            }
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                let mut buf = vec![0u8; read_len];
                i2c.write_read(&write_data, &mut buf)?;
                reads.push(buf);
            }
            I2cTransactionStep::SleepMs(ms) => {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            I2cTransactionStep::SetTimeout(timeout_jiffies) => {
                i2c.set_timeout(timeout_jiffies)?;
            }
        }
    }

    Ok(reads)
}

/// Try to recover the I2C bus by closing and reopening the kernel fd.
///
/// v0.12.1: REMOVED reset_axi_iic_controller() call that was here previously.
/// That function writes SOFTR + timing + CR to AXI IIC registers via /dev/mem,
/// which permanently desynchronizes the kernel xiic driver's internal state
/// machine from the hardware. The kernel driver tracks its own CR/ISR/FIFO
/// state and a devmem SOFTR invalidates all of it. Reopening /dev/i2c-0 does
/// NOT trigger xiic_reinit() — it only acquires a usage lock. The actual
/// recovery happens when the kernel's xiic_process() detects errors on the
/// next transaction and runs its internal retry/reinit logic.
///
/// Recovery strategy: drop the fd (releases kernel lock), wait briefly for
/// any in-flight kernel timeout to expire, then reopen. The kernel driver
/// handles hardware-level recovery internally on the next I2C transaction.
///
/// NEVER add devmem register writes here. The kernel xiic driver MUST be the
/// sole owner of AXI IIC registers during mining.
fn try_reset_and_reopen(
    bus: u8,
    restore_kernel_registers: bool,
    i2c_bus: &mut Option<I2cBus>,
    last_reset: &mut std::time::Instant,
    consecutive_resets: &mut u32,
    write_denylist: &[u8],
) {
    if last_reset.elapsed() > std::time::Duration::from_secs(1) {
        *last_reset = std::time::Instant::now();
        *consecutive_resets += 1;

        let service_authority = i2c_bus
            .as_ref()
            .and_then(|transport| transport.service_authority.as_ref())
            .and_then(Weak::upgrade);

        // Drop current fd — releases kernel i2c_adapter lock
        *i2c_bus = None;

        // Wait for any in-flight kernel xiic timeout to complete.
        // The kernel xiic driver uses a 1s timeout per transaction with 3
        // internal retries. 50ms is enough for the driver to finish cleanup.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Reopen — gets a fresh fd but same kernel i2c_adapter underneath.
        // The kernel driver will attempt its own error recovery on next xfer.
        *i2c_bus = service_authority
            .as_ref()
            .and_then(|authority| I2cBus::open_for_service(bus, authority).ok());
        if let Some(ref mut i2c) = i2c_bus {
            // v0.14.0: Restore 100ms timeout on reopened fd
            if let Err(error) = i2c.set_timeout(I2C_SERVICE_DEFAULT_TIMEOUT_JIFFIES) {
                tracing::error!(
                    bus,
                    %error,
                    "recovered I2C fd could not verify its bounded timeout; PIC16 runtime jobs will refuse wire access"
                );
            }
            // Re-apply write denylist after every reopen — denylist must
            // persist across recovery cycles or EEPROM protection silently lapses.
            if !write_denylist.is_empty() {
                i2c.set_write_denylist(write_denylist);
            }
        }

        if i2c_bus.is_some() {
            // CRITICAL (swarm 2026-04-17): Restore AXI IIC timing registers after fd reopen.
            // The kernel xiic driver's internal error recovery calls SOFTR, which zeros
            // THIGH/TLOW/TBUF to 0 (= max I2C speed). PICs NACK at max speed.
            // This was the ROOT CAUSE of 2/3 PIC heartbeat death after ~60s of mining.
            if restore_kernel_registers {
                if let Err(error) = restore_kernel_i2c_interrupts() {
                    tracing::error!(
                        bus,
                        %error,
                        "I2C fd reopened but AXI-IIC register restoration failed; discarding the transport"
                    );
                    *i2c_bus = None;
                    return;
                }
            } else {
                tracing::warn!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus recovered by fd reopen only; AXI IIC register restore skipped by policy"
                );
            }

            if *consecutive_resets <= 3 && restore_kernel_registers {
                tracing::warn!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus recovered — fd reopened + AXI IIC timing restored"
                );
            } else if *consecutive_resets == 10 {
                tracing::error!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus: 10 consecutive resets — PIC may be dead or hash board disconnected"
                );
            } else if *consecutive_resets > 10 && (*consecutive_resets).is_multiple_of(50) {
                // After 10, only log every 50 to avoid spam
                tracing::error!(
                    bus,
                    consecutive_resets = *consecutive_resets,
                    "I2C bus: persistent failures continue"
                );
            }
        } else {
            tracing::error!(bus, "I2C bus reopen FAILED — /dev/i2c-{} unavailable", bus);
        }
    }
}

#[cfg(test)]
mod denylist_tests {
    use super::*;

    /// Helper: build an I2cBus that doesn't actually open hardware so we
    /// can test the denylist gate without real /dev/i2c-N. Uses devmem
    /// stub mode (file=None, devmem=true) — the gate is checked BEFORE
    /// any I/O so devmem never executes.
    fn make_test_bus(denylist: &[u8]) -> I2cBus {
        let mut b = I2cBus::open_devmem();
        b.set_write_denylist(denylist);
        b
    }

    #[test]
    fn empty_denylist_allows_all_writes_at_setup_time() {
        let bus = I2cBus::open_devmem();
        // No denylist registered → no address is_write_denied
        assert!(!bus.is_write_denied(0x10));
        assert!(!bus.is_write_denied(0x21));
        assert!(!bus.is_write_denied(0x50));
        assert!(!bus.is_write_denied(0x55));
        assert!(!bus.is_write_denied(0x57));
    }

    #[test]
    fn am2_eeprom_denylist_blocks_50_through_57_only() {
        let bus = make_test_bus(&[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57]);
        // EEPROM range — blocked
        for addr in 0x50u8..=0x57u8 {
            assert!(bus.is_write_denied(addr), "0x{:02X} should be denied", addr);
        }
        // PSU + dsPIC + LM75A passthrough — NOT in denylist
        assert!(!bus.is_write_denied(0x10), "PSU at 0x10 must not be denied");
        assert!(
            !bus.is_write_denied(0x20),
            "dsPIC at 0x20 must not be denied"
        );
        assert!(
            !bus.is_write_denied(0x21),
            "dsPIC at 0x21 must not be denied"
        );
        assert!(
            !bus.is_write_denied(0x22),
            "dsPIC at 0x22 must not be denied"
        );
        assert!(
            !bus.is_write_denied(0x48),
            "LM75A at 0x48 must not be denied"
        );
        assert!(
            !bus.is_write_denied(0x4F),
            "LM75A at 0x4F must not be denied"
        );
    }

    #[test]
    fn s9_platform_pic_addrs_must_not_be_in_typical_eeprom_denylist() {
        // S9 platform startup leaves the denylist EMPTY because addresses
        // 0x55-0x57 are S9 PIC voltage controllers, NOT EEPROMs. This test
        // documents the contract: if a future change ever applies the am2
        // EEPROM denylist on S9, it would break PIC writes — that's wrong.
        let s9_bus = I2cBus::open_devmem();
        // S9 ships with no denylist — confirms PIC writes are fine.
        assert!(!s9_bus.is_write_denied(PIC_ADDR_CHAIN6));
        assert!(!s9_bus.is_write_denied(PIC_ADDR_CHAIN7));
        assert!(!s9_bus.is_write_denied(PIC_ADDR_CHAIN8));
    }

    #[test]
    fn pic_voltage_dac_clamps_to_min_safe_value_at_i2c_boundary() {
        assert_eq!(MIN_SAFE_PIC_DAC_VALUE, 6);
        for raw in 0..MIN_SAFE_PIC_DAC_VALUE {
            assert_eq!(
                clamp_pic_voltage_dac(raw),
                MIN_SAFE_PIC_DAC_VALUE,
                "PIC DAC {raw} must clamp to the 9.40V-safe boundary"
            );
        }
        assert_eq!(
            clamp_pic_voltage_dac(MIN_SAFE_PIC_DAC_VALUE),
            MIN_SAFE_PIC_DAC_VALUE
        );
        assert_eq!(clamp_pic_voltage_dac(42), 42);
        assert_eq!(clamp_pic_voltage_dac(u8::MAX), u8::MAX);
    }

    #[test]
    fn refused_write_bumps_blocked_count() {
        let bus = make_test_bus(&[0x50]);
        assert_eq!(bus.blocked_write_count(), 0);
        let _ = bus.refuse_write(0x50);
        assert_eq!(bus.blocked_write_count(), 1);
        let _ = bus.refuse_write(0x50);
        let _ = bus.refuse_write(0x50);
        assert_eq!(bus.blocked_write_count(), 3);
    }

    #[test]
    fn every_public_write_path_is_wired_to_the_denylist_not_just_is_write_denied() {
        // is_write_denied is unit-tested above; this proves the GUARD is actually
        // WIRED into each public write method. A future edit deleting the guard from
        // one write path would still pass every is_write_denied test but silently
        // unprotect that path — the exact .74/.139 EEPROM-corruption class. The
        // guards return BEFORE any I2C syscall and blocked_write_count() bumps ONLY
        // inside refuse_write (called ONLY by the in-path guards), so its increment
        // witnesses the guard firing inside the real write path.
        let mut bus = make_test_bus(&[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57]);
        assert_eq!(bus.blocked_write_count(), 0);

        bus.set_slave(0x50).unwrap();
        assert!(
            bus.write(&[0xAB]).is_err(),
            "write() to a denied EEPROM address must fail"
        );
        assert_eq!(
            bus.blocked_write_count(),
            1,
            "write() must hit the denylist guard"
        );

        bus.set_slave(0x51).unwrap();
        assert!(bus.write_byte_by_byte(&[0xAB]).is_err());
        assert_eq!(
            bus.blocked_write_count(),
            2,
            "write_byte_by_byte() must hit the denylist guard"
        );

        bus.set_slave(0x52).unwrap();
        let mut rd = [0u8; 1];
        assert!(bus.write_read(&[0xAB], &mut rd).is_err());
        assert_eq!(
            bus.blocked_write_count(),
            3,
            "write_read() must hit the denylist guard"
        );
    }

    #[test]
    fn denylist_can_be_cleared_then_replaced() {
        let mut bus = make_test_bus(&[0x50, 0x51]);
        assert!(bus.is_write_denied(0x50));
        // Clear (used for testing recovery — production should never do this)
        bus.set_write_denylist(&[]);
        assert!(!bus.is_write_denied(0x50));
        // Re-register
        bus.set_write_denylist(&[0x50, 0x51, 0x52]);
        assert!(bus.is_write_denied(0x52));
    }
}

/// W2.3 lockdown surface tests.
///
/// These tests do **not** open real hardware — they exercise the public API
/// surface of `dcentrald-hal::i2c` to make sure the single-I²C-owner
/// contract is enforced at the type-system level:
///
/// 1. `I2cServiceHandle` and platform-safe service constructors are
///    visible from outside the HAL crate. (Compile-time check by reference.)
/// 2. The only normal one-shot helper is a fixed bus/address/offset/length
///    miner-identity read; arbitrary EEPROM access is recovery-feature-only.
/// 3. `I2cBus::open` is **not** part of the public surface unless
///    `recovery-tool` is enabled. The CI grep gate plus `pub(crate)`
///    visibility together prevent regressions; this test documents the
///    intent in code.
#[cfg(test)]
mod kernel_driver_unbind_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    struct TempSysfs(PathBuf);

    impl TempSysfs {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("dcentos-i2c-unbind-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(root.join("bus/platform/devices/41600000.i2c")).unwrap();
            std::fs::create_dir_all(root.join("bus/platform/drivers/xiic-i2c")).unwrap();
            Self(root)
        }

        fn root(&self) -> &Path {
            &self.0
        }

        fn driver_link(&self) -> PathBuf {
            self.0.join("bus/platform/devices/41600000.i2c/driver")
        }

        fn bind(&self, driver: &str) {
            let target = self.0.join("bus/platform/drivers").join(driver);
            std::fs::create_dir_all(&target).unwrap();
            symlink(target, self.driver_link()).unwrap();
        }
    }

    impl Drop for TempSysfs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn already_unbound_requires_the_platform_device_to_exist() {
        let sysfs = TempSysfs::new();
        let disposition = unbind_kernel_i2c_driver_at(sysfs.root(), |_, _| {
            panic!("already-unbound state must not write sysfs")
        })
        .unwrap();
        assert_eq!(disposition, KernelI2cDriverDisposition::AlreadyUnbound);

        let missing = std::env::temp_dir().join("dcentos-missing-i2c-sysfs");
        assert!(unbind_kernel_i2c_driver_at(&missing, |_, _| Ok(())).is_err());
    }

    #[test]
    fn denied_unbind_is_retryable_and_never_claims_success() {
        let sysfs = TempSysfs::new();
        sysfs.bind("xiic-i2c");

        let denied = unbind_kernel_i2c_driver_at(sysfs.root(), |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected denial",
            ))
        });
        assert!(denied.is_err());
        assert!(sysfs.driver_link().exists());

        let link = sysfs.driver_link();
        let disposition = unbind_kernel_i2c_driver_at(sysfs.root(), move |path, payload| {
            assert!(path.ends_with("bus/platform/drivers/xiic-i2c/unbind"));
            assert_eq!(payload, b"41600000.i2c");
            std::fs::remove_file(link)
        })
        .unwrap();
        assert_eq!(disposition, KernelI2cDriverDisposition::Unbound);
    }

    #[test]
    fn unexpected_bound_driver_is_refused_without_writing() {
        let sysfs = TempSysfs::new();
        sysfs.bind("unexpected-driver");
        let result = unbind_kernel_i2c_driver_at(sysfs.root(), |_, _| {
            panic!("wrong driver binding must not be mutated")
        });
        assert!(result.is_err());
        assert!(sysfs.driver_link().exists());
    }
}

#[cfg(test)]
mod lockdown_surface_tests {
    use super::*;

    #[test]
    fn service_handle_type_is_publicly_constructible_in_principle() {
        // Compile-time existence check: callers outside HAL only see
        // `I2cServiceHandle` and the platform-safe constructors.
        // We don't actually spawn a service here — Linux-only side effects
        // would block on Windows hosts. Type-presence is enough to lock
        // the public surface.
        let _f: fn(bool) -> std::io::Result<I2cServiceHandle> = spawn_am1_s9_i2c0_service;
        let _g: fn(u8) -> std::io::Result<I2cServiceHandle> = spawn_i2c_service_no_register_touch;
        let _h: fn(u8, Vec<u8>) -> std::io::Result<I2cServiceHandle> =
            spawn_i2c_service_no_register_touch_with_denylist;
    }

    #[test]
    fn miner_identity_read_has_no_caller_selected_transport_fields() {
        let _f: fn() -> Result<Vec<u8>> = read_secondary_bus_miner_identity_eeprom;
    }
}

#[cfg(test)]
mod axi_iic_recovery_tests {
    //! WAVE-0 STABILIZE: AXI IIC stuck-state detection + escalation policy.
    //!
    //! The register effects (`full_controller_reset_devmem`,
    //! `axi_iic_escalating_recovery`'s side effects) are LIVE-ONLY and cannot
    //! run off-hardware. These tests pin the PURE decode/policy logic that
    //! drives them: SR classification (so SR=0xC0 idle is never "recovered"),
    //! and the SCL -> full-reset -> give-up escalation ladder.
    use super::{
        axi_iic_recovery_tier, axi_iic_stuck_reason, AxiIicRecoveryTier, AxiIicStuck,
        AXI_IIC_GIVE_UP_AFTER, AXI_IIC_SCL_TIER_LIMIT,
    };

    #[test]
    fn idle_sr_0xc0_is_not_stuck() {
        // SR=0xC0 = TX_FIFO_EMPTY | RX_FIFO_EMPTY, BB clear = healthy idle.
        assert_eq!(axi_iic_stuck_reason(0xC0), None);
    }

    #[test]
    fn bus_busy_is_stuck_even_with_fifos_empty() {
        // BB (0x04) asserted between transactions => master FSM hung.
        assert_eq!(axi_iic_stuck_reason(0xC4), Some(AxiIicStuck::BusBusyHung));
        assert_eq!(axi_iic_stuck_reason(0x04), Some(AxiIicStuck::BusBusyHung));
    }

    #[test]
    fn all_zero_sr_is_controller_down() {
        // A live, enabled, idle core always reads >= 0xC0; SR=0 => core
        // disabled / clock gone => needs a full re-init.
        assert_eq!(
            axi_iic_stuck_reason(0x00),
            Some(AxiIicStuck::ControllerDown)
        );
    }

    #[test]
    fn tx_fifo_not_empty_idle_is_stalled() {
        // RX empty (0x40), TX NOT empty, bus idle => stalled transaction.
        assert_eq!(axi_iic_stuck_reason(0x40), Some(AxiIicStuck::TxFifoStalled));
    }

    #[test]
    fn recovery_tier_starts_with_scl_pulses() {
        for n in 1..=AXI_IIC_SCL_TIER_LIMIT {
            assert_eq!(
                axi_iic_recovery_tier(n),
                AxiIicRecoveryTier::SclPulses,
                "n={n} should be SCL pulses"
            );
        }
    }

    #[test]
    fn recovery_tier_escalates_to_full_reset_after_scl_limit() {
        for n in (AXI_IIC_SCL_TIER_LIMIT + 1)..AXI_IIC_GIVE_UP_AFTER {
            assert_eq!(
                axi_iic_recovery_tier(n),
                AxiIicRecoveryTier::FullControllerReset,
                "n={n} should escalate to a full controller reset"
            );
        }
    }

    #[test]
    fn recovery_tier_gives_up_at_limit() {
        assert_eq!(
            axi_iic_recovery_tier(AXI_IIC_GIVE_UP_AFTER),
            AxiIicRecoveryTier::GiveUp
        );
        assert_eq!(
            axi_iic_recovery_tier(AXI_IIC_GIVE_UP_AFTER + 100),
            AxiIicRecoveryTier::GiveUp
        );
    }

    #[test]
    fn give_up_threshold_is_after_full_reset_band() {
        // Sanity on the constants so a future edit can't invert the ladder.
        assert!(AXI_IIC_SCL_TIER_LIMIT < AXI_IIC_GIVE_UP_AFTER);
    }
}
