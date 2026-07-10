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
use std::os::fd::AsRawFd;

use crate::{HalError, Result};

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
}

impl I2cBus {
    /// Open an I2C bus by number.
    ///
    /// Opens `/dev/i2c-{bus}` and returns a raw bus handle. Visibility is
    /// restricted to `pub(crate)` to enforce the **single-I2C-owner**
    /// architecture on am2 S19j Pro: in production, `/dev/i2c-0` is owned
    /// by exactly one `i2c-service` thread (see [`spawn_i2c_service`]).
    /// Two raw `I2cBus` handles racing on the same bus reproduce the
    /// MSSP-parser corruption that bricked the .139/.74 dsPICs (see
    /// ,
    /// ,
    /// ).
    ///
    /// Out-of-HAL callers MUST go through [`I2cServiceHandle`] (constructed
    /// via [`spawn_i2c_service`], [`spawn_i2c_service_no_register_touch`],
    /// or [`spawn_i2c_service_no_register_touch_with_denylist`]) instead.
    /// For one-shot identity probes that legitimately do not need a long
    /// running service, see [`read_eeprom_bytes`].
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
    pub(crate) fn open(bus: u8) -> Result<Self> {
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
        })
    }

    /// Open an I2C bus in devmem mode (bypasses kernel xiic driver).
    /// No /dev/i2c-N file is opened. All operations go through AXI IIC registers.
    pub fn open_devmem() -> Self {
        Self {
            file: None,
            bus: 0,
            current_addr: None,
            devmem: true,
            write_denylist: Vec::new(),
            blocked_write_count: std::sync::atomic::AtomicU64::new(0),
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
        Self::open(bus)
    }
}

/// HAL-public one-shot helper for **identity-only** EEPROM reads.
///
/// Used by the daemon's hardware-info gather path (miner serial, hash board
/// type) which runs **before** the main I²C service is spawned. Opens a
/// transient kernel-mode `I2cBus`, points it at `addr`, writes the requested
/// `offset` byte to set the read pointer, sleeps briefly, then reads
/// `len` bytes back.
///
/// This helper is the only sanctioned way for non-platform code to touch
/// the bus directly. It is intentionally **read-only**: there is no
/// `write_eeprom_bytes` companion, because no D-Central code path should
/// write to AT24C-series hashboard EEPROMs (the HAL-level write denylist
/// blocks 0x50-0x57 on am2 anyway — see
/// ).
///
/// On `len > 32`, the helper reads in 32-byte chunks (matches AT24C02 page
/// size). Returns the concatenated bytes on success.
pub fn read_eeprom_bytes(bus: u8, addr: u8, offset: u8, len: usize) -> Result<Vec<u8>> {
    let mut i2c = I2cBus::open(bus)?;
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
        if self.current_addr == Some(addr) {
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
        let addr = self.current_addr.unwrap_or(0);
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

        let mut msg = I2cMsg {
            addr: addr as u16,
            flags: 0x0010, // I2C_M_TEN — lets kernel accept high addresses
            len: buf.len() as u16,
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
        let addr = self.current_addr.unwrap_or(0);
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
        if self.devmem {
            if data.len() <= 15 {
                // PIC commands (3-4 bytes): single multi-byte I2C transaction.
                // AXI IIC TX FIFO = 16 entries (1 addr + 15 data max).
                return devmem_i2c_write(addr, data);
            }
            // Parser flush (16 bytes): byte-by-byte to avoid TX FIFO overflow.
            // Each byte is a separate START/addr/byte/STOP transaction.
            for &byte in data {
                let _ = devmem_i2c_write(addr, &[byte]);
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
                self.write_rdwr(addr, &[byte])?;
                std::thread::sleep(std::time::Duration::from_millis(inter_byte_ms));
            }
            return Ok(());
        }
        for &byte in data {
            nix::unistd::write(self.file.as_ref().unwrap(), &[byte]).map_err(|e| {
                HalError::I2c {
                    bus: self.bus,
                    addr,
                    detail: format!("write failed: {}", e),
                }
            })?;
            std::thread::sleep(std::time::Duration::from_millis(inter_byte_ms));
        }
        Ok(())
    }

    /// Read data bytes from the current slave device.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let addr = self.current_addr.unwrap_or(0);
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
        let addr = self.current_addr.unwrap_or(0);
        // write_read writes a command byte before the read; the write half
        // is what the denylist guards.
        if self.is_write_denied(addr) {
            return Err(self.refuse_write(addr));
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

        let mut msgs = [
            I2cMsg {
                addr: addr as u16,
                flags: 0, // write
                len: write_buf.len() as u16,
                buf: write_buf.as_mut_ptr(),
            },
            I2cMsg {
                addr: addr as u16,
                flags: I2C_M_RD,
                len: read_buf.len() as u16,
                buf: read_buf.as_mut_ptr(),
            },
        ];

        let mut data = I2cRdwrIoctlData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 2,
        };

        let ret = unsafe { libc::ioctl(fd, I2C_RDWR as _, &mut data as *mut I2cRdwrIoctlData) };

        if ret < 0 {
            return Err(HalError::I2c {
                bus: self.bus,
                addr,
                detail: format!("I2C_RDWR ioctl failed: {}", std::io::Error::last_os_error()),
            });
        }

        Ok(())
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
        }
        Ok(())
    }

    /// Attempt I2C bus recovery by generating 9 SCL clocks.
    ///
    /// In devmem mode: sends 9 dummy read transactions to address 0x03
    /// (which will NACK but generates SCL clocks to unstick slave SDA).
    /// In kernel mode: no-op (kernel driver handles recovery internally).
    pub fn bus_recovery(&mut self) {
        if self.devmem {
            for _ in 0..9 {
                let mut dummy = [0u8; 1];
                let _ = devmem_i2c_read(0x03, &mut dummy);
            }
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
pub fn reset_axi_iic_controller() -> Result<()> {
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
    }

    Ok(())
}

/// Clear ISR TX_ERROR bit via devmem.
///
/// Used during bus recovery: after each dummy read (which NACKs), the ISR
/// TX_ERROR flag is set. Clear it before the next recovery clock pulse.
pub fn devmem_clear_isr_tx_error() -> Result<()> {
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
        // Write ISR_TX_ERROR bit to clear it (write-1-to-clear register)
        std::ptr::write_volatile(base.add(AXI_IIC_ISR) as *mut u32, ISR_TX_ERROR);
        let _ = nix::sys::mman::munmap(ptr, 4096);
    }
    Ok(())
}

/// I2C bus recovery via 9 SCL clock pulses (devmem mode).
///
/// Sends 9 dummy read transactions to address 0x03 (no device there).
/// Each transaction generates START + 8 SCL clocks (address byte) + NACK
/// + STOP = ~10 SCL edges. 9 iterations = ~90 edges, more than enough
/// to clear any stuck PIC MSSP state (I2C spec requires max 9).
///
/// Uses `devmem_i2c_read_no_retry` to avoid triggering SOFTR on each
/// expected NACK — the whole point is SCL clocks, not controller resets.
pub fn bus_recovery_devmem() {
    // Step 1: If bus is stuck busy (SR_BB=1), SOFTR is the ONLY way to clear
    // the hardware state machine. This is a TARGETED, CONDITIONAL SOFTR — only
    // when the AXI IIC FSM is genuinely stuck. NOT on every NACK (which kills
    // PIC MSSP — the documented regression we must never repeat).
    {
        let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&base_addr) = DEVMEM_IIC_MMAP.get() {
            let base = base_addr as *mut u8;
            unsafe {
                let sr = std::ptr::read_volatile(base.add(AXI_IIC_SR) as *const u32);
                if sr & SR_BB != 0 {
                    // Bus hardware is stuck — CR=0 then SOFTR to reset FSM
                    std::ptr::write_volatile(base.add(AXI_IIC_CR) as *mut u32, 0);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    std::ptr::write_volatile(base.add(AXI_IIC_SOFTR) as *mut u32, 0x0000_000A);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    // SOFTR zeros ALL timing regs — restore immediately
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
                    tracing::warn!(
                        "bus_recovery: SOFTR to clear stuck bus-busy (SR was 0x{:02X}) — timing restored",
                        sr
                    );
                }
            }
        }
    }
    // Step 2: 9 SCL clock pulses via dummy reads to clear any stuck PIC MSSP.
    // Bus is now idle (either was already, or SOFTR cleared it above).
    for _ in 0..9 {
        let mut dummy = [0u8; 1];
        let _ = devmem_i2c_read_no_retry(0x03, &mut dummy);
        let _ = devmem_clear_isr_tx_error();
    }
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
pub fn axi_iic_read_sr() -> Option<u32> {
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
pub fn full_controller_reset_devmem() -> Option<u32> {
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
pub fn axi_iic_escalating_recovery(consecutive: u32) -> AxiIicRecoveryTier {
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
            bus_recovery_devmem();
        }
        AxiIicRecoveryTier::FullControllerReset => {
            let _ = full_controller_reset_devmem();
            // Follow the controller reset with SCL pulses to release any slave
            // (PIC MSSP) still holding SDA from the wedge.
            bus_recovery_devmem();
        }
        AxiIicRecoveryTier::GiveUp => {
            // One last heavy attempt, then we report GiveUp so callers can
            // stop escalating (the daemon's per-PIC back-off takes over).
            let _ = full_controller_reset_devmem();
        }
    }
    effective
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
pub fn restore_kernel_i2c_interrupts() -> Result<()> {
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

        tracing::info!(
            gie_before = format_args!("0x{:08X}", gie_before),
            ier_before = format_args!("0x{:08X}", ier_before),
            gie_after = format_args!("0x{:08X}", gie_after),
            "AXI IIC GIE restored for kernel xiic driver — interrupts re-enabled",
        );

        let _ = nix::sys::mman::munmap(ptr, 4096);
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
    let _ = DEVMEM_IIC_MMAP.set(base as usize);
    tracing::info!(
        "devmem I2C: persistent mmap established at 0x{:08X}",
        AXI_IIC_BASE
    );
    Ok(base)
}

/// Unbind the kernel xiic-i2c driver from the AXI IIC controller.
///
/// The kernel driver's interrupt handler and SOFTR resets interfere with
/// direct devmem I2C access. Call this once before any devmem I2C operations.
pub fn unbind_kernel_i2c_driver() {
    use std::sync::Once;
    static UNBIND: Once = Once::new();
    UNBIND.call_once(|| {
        let _ = std::fs::write("/sys/bus/platform/drivers/xiic-i2c/unbind", "41600000.i2c");
        tracing::info!("Unbound kernel xiic-i2c driver — all I2C via devmem now");
    });
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
pub fn devmem_i2c_write(addr: u8, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    // Serialize all AXI IIC access — heartbeat thread + init must not interleave
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // v0.16.0: Kernel driver is unbound at daemon startup. All I2C is devmem now.

    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&b) => b as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    let result = unsafe { devmem_i2c_write_inner(base, addr, data) };
    if result.is_err() {
        drop(_guard);
        bus_recovery_devmem();
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
pub fn devmem_i2c_read(addr: u8, buf: &mut [u8]) -> Result<()> {
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
    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // v0.16.0: Kernel driver is unbound at daemon startup. All I2C is devmem now.

    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&b) => b as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    let result = unsafe { devmem_i2c_read_inner(base, addr, buf) };
    if result.is_err() {
        // v0.20.1: NEVER SOFTR on NACK — kills PIC MSSP (documented regression).
        // Use bus recovery (SCL clocks) instead.
        drop(_guard);
        bus_recovery_devmem();
        let _guard2 = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::sleep(std::time::Duration::from_millis(5));
        unsafe { devmem_i2c_read_inner(base, addr, buf) }
    } else {
        result
    }
}

/// Raw AXI IIC read WITHOUT SOFTR retry on NACK.
///
/// Used by `bus_recovery_devmem()`: the expected NACKs from address 0x03
/// generate SCL clocks to unstick PIC MSSP. A SOFTR retry would defeat the
/// purpose by resetting the AXI IIC state machine mid-recovery.
pub fn devmem_i2c_read_no_retry(addr: u8, buf: &mut [u8]) -> Result<()> {
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

    let _guard = DEVMEM_IIC_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let base = match DEVMEM_IIC_MMAP.get() {
        Some(&b) => b as *mut u8,
        None => init_devmem_iic_mmap()?,
    };
    unsafe { devmem_i2c_read_inner(base, addr, buf) }
}

unsafe fn devmem_i2c_read_inner(base: *mut u8, addr: u8, buf: &mut [u8]) -> Result<()> {
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
        });
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
                return Err(HalError::I2c {
                    bus: 0,
                    addr,
                    detail: format!("devmem read: NACK (ISR=0x{:02X})", isr),
                });
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
                });
            }
            rx_wait += 1;
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        *byte = (read_reg(AXI_IIC_RX_FIFO) & 0xFF) as u8;
    }

    Ok(())
}

/// Write data bytes one at a time via devmem AXI IIC (byte-by-byte pattern).
///
/// Each byte is sent as a separate I2C transaction (START+addr+byte+STOP)
/// with 1ms between bytes, matching the BraiinsOS pattern for PIC communication.
pub fn devmem_i2c_write_byte_by_byte(addr: u8, data: &[u8]) -> Result<()> {
    for &byte in data {
        devmem_i2c_write(addr, &[byte])?;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// I2C Service — serialized single-thread I2C bus access
// ---------------------------------------------------------------------------

use std::sync::mpsc;

/// Firmware type indicator (mirrors PicFirmware enum without depending on dcentrald-asic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cPicFirmware {
    Stock,
    BraiinsOs,
    Unknown,
}

/// One operation in an ordered I2C service transaction.
#[derive(Debug, Clone)]
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

/// I2C request types for the serialized service.
#[derive(Debug)]
pub enum I2cRequest {
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
    /// Ordered compound transaction executed as one service-worker request.
    Transaction {
        addr: u8,
        steps: Vec<I2cTransactionStep>,
        reply_tx: mpsc::SyncSender<Result<Vec<Vec<u8>>>>,
    },
}

/// Handle for sending I2C requests to the service thread.
#[derive(Clone)]
pub struct I2cServiceHandle {
    tx: mpsc::SyncSender<I2cRequest>,
}

impl I2cServiceHandle {
    /// Test-only: construct an `I2cServiceHandle` whose channel is never
    /// served by a worker. Returns the handle plus a drop-guard that holds
    /// the receiver alive (so `submit` blocks instead of erroring on a
    /// closed channel). Calling any I/O method on the returned handle
    /// would block forever — tests must avoid those paths and only use
    /// the handle as a transport-token for state-machine assertions.
    #[cfg(test)]
    pub fn for_unit_tests() -> (Self, std::sync::mpsc::Receiver<I2cRequest>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        (Self { tx }, rx)
    }

    fn submit<T>(
        &self,
        addr: u8,
        req: I2cRequest,
        reply_rx: mpsc::Receiver<Result<T>>,
    ) -> Result<T> {
        self.tx.send(req).map_err(|_| HalError::I2c {
            bus: 0,
            addr,
            detail: "I2C service channel closed".into(),
        })?;
        reply_rx.recv().unwrap_or(Err(HalError::I2c {
            bus: 0,
            addr,
            detail: "I2C service reply dropped".into(),
        }))
    }

    /// Send a heartbeat request. Returns Ok(()) if the heartbeat succeeded.
    pub fn heartbeat(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Heartbeat {
            addr,
            firmware,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Set voltage DAC value on a PIC.
    pub fn set_voltage(&self, addr: u8, firmware: I2cPicFirmware, pic_val: u8) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetVoltage {
            addr,
            firmware,
            pic_val,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Disable voltage output on a PIC.
    pub fn disable_voltage(&self, addr: u8, firmware: I2cPicFirmware) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::DisableVoltage {
            addr,
            firmware,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Set voltage in millivolts on a dsPIC33EP (S19 Pro / S17 style).
    pub fn set_voltage_mv(&self, addr: u8, voltage_mv: u16) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetVoltageMv {
            addr,
            voltage_mv,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    // --- v0.13.0: Generic I2C operations for init ---

    /// Write bytes to an I2C slave (single transaction).
    pub fn write_bytes(&self, addr: u8, data: &[u8]) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteBytes {
            addr,
            data: data.to_vec(),
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Write bytes one-at-a-time (byte-by-byte PIC pattern, 1ms between bytes).
    pub fn write_byte_by_byte(&self, addr: u8, data: &[u8]) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteByteByte {
            addr,
            data: data.to_vec(),
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Read bytes from an I2C slave.
    pub fn read_bytes(&self, addr: u8, len: usize) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::ReadBytes {
            addr,
            len,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Combined write+read (I2C_RDWR repeated START).
    pub fn write_read(&self, addr: u8, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::WriteRead {
            addr,
            write_data: write_data.to_vec(),
            read_len,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }

    /// Set I2C timeout (units of 10ms jiffies).
    pub fn set_timeout(&self, timeout_jiffies: u32) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::SetTimeout {
            timeout_jiffies,
            reply_tx,
        };
        self.submit(0, req, reply_rx)
    }

    /// Execute ordered I2C steps under one service-worker bus/address lock.
    ///
    /// The returned vector contains one entry for each `Read` or `WriteRead`
    /// step, in execution order.
    pub fn transaction(&self, addr: u8, steps: Vec<I2cTransactionStep>) -> Result<Vec<Vec<u8>>> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = I2cRequest::Transaction {
            addr,
            steps,
            reply_tx,
        };
        self.submit(addr, req, reply_rx)
    }
}

/// Spawn the I2C service thread that serializes all I2C bus access.
///
/// Returns a handle that can be cloned and shared across threads.
/// The service thread owns the I2C bus fd and processes requests sequentially,
/// eliminating bus contention from concurrent heartbeat/voltage/thermal threads.
pub fn spawn_i2c_service(bus: u8, use_devmem: bool) -> std::io::Result<I2cServiceHandle> {
    spawn_i2c_service_with_policy(bus, use_devmem, !use_devmem, Vec::new())
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

fn spawn_i2c_service_with_policy(
    bus: u8,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: Vec<u8>,
) -> std::io::Result<I2cServiceHandle> {
    // Bounded channel: 64 slots avoids unbounded growth if callers outpace the bus.
    let (tx, rx) = mpsc::sync_channel::<I2cRequest>(64);

    std::thread::Builder::new()
        .name("i2c-service".to_string())
        .spawn(move || {
            i2c_service_loop(
                bus,
                rx,
                use_devmem,
                restore_kernel_registers,
                write_denylist,
            );
        })?;

    Ok(I2cServiceHandle { tx })
}

/// Main loop for the I2C service thread. Owns the bus and processes requests.
fn i2c_service_loop(
    bus: u8,
    rx: mpsc::Receiver<I2cRequest>,
    use_devmem: bool,
    restore_kernel_registers: bool,
    write_denylist: Vec<u8>,
) {
    let apply_denylist = |i2c: &mut I2cBus| {
        if !write_denylist.is_empty() {
            i2c.set_write_denylist(&write_denylist);
        }
    };
    let mut i2c_bus: Option<I2cBus> = if use_devmem {
        let mut b = I2cBus::open_devmem();
        apply_denylist(&mut b);
        Some(b)
    } else {
        match I2cBus::open(bus) {
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
        let _ = i2c.set_timeout(10); // 10 jiffies = 100ms
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

    tracing::info!(
        bus,
        use_devmem,
        "I2C service thread started — all PIC I/O serialized through this thread (timeout=100ms)"
    );

    for req in rx.iter() {
        // Reopen bus if previous operations lost the fd
        if i2c_bus.is_none() {
            i2c_bus = if use_devmem {
                let mut b = I2cBus::open_devmem();
                apply_denylist(&mut b);
                Some(b)
            } else {
                match I2cBus::open(bus) {
                    Ok(mut b) => {
                        apply_denylist(&mut b);
                        Some(b)
                    }
                    Err(_) => None,
                }
            };
            if let Some(ref i2c) = i2c_bus {
                let _ = i2c.set_timeout(10);
                if !use_devmem && restore_kernel_registers {
                    if let Err(e) = restore_kernel_i2c_interrupts() {
                        tracing::warn!(
                            "Failed to restore I2C timing registers after reopen: {}",
                            e
                        );
                    }
                } else if !use_devmem {
                    tracing::debug!(
                        bus,
                        "I2C fd reopened; AXI IIC register restore skipped by policy"
                    );
                }
            }
            if i2c_bus.is_none() {
                // Reply with error and continue
                match req {
                    I2cRequest::Heartbeat { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::SetVoltage { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::DisableVoltage { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::SetVoltageMv { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::WriteBytes { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::WriteByteByte { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::ReadBytes { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::WriteRead { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::SetTimeout { reply_tx, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr: 0,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                    I2cRequest::Transaction { reply_tx, addr, .. } => {
                        let _ = reply_tx.send(Err(HalError::I2c {
                            bus,
                            addr,
                            detail: "bus reopen failed".into(),
                        }));
                    }
                }
                continue;
            }
        }

        let i2c = i2c_bus.as_mut().unwrap();

        match req {
            I2cRequest::Heartbeat {
                addr,
                firmware,
                reply_tx,
            } => {
                let result = execute_heartbeat(i2c, addr, firmware);
                if result.is_err() {
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
                let result = execute_set_voltage(i2c, addr, firmware, pic_val);
                if result.is_err() {
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
                let result = execute_disable_voltage(i2c, addr, firmware);
                let _ = reply_tx.send(result);
            }
            I2cRequest::SetVoltageMv {
                addr,
                voltage_mv,
                reply_tx,
            } => {
                let result = execute_set_voltage_mv(i2c, addr, voltage_mv);
                if result.is_err() {
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
                let _ = i2c.set_slave(addr);
                let result = i2c.write(&data).map(|_| ());
                if result.is_err() {
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
                let _ = i2c.set_slave(addr);
                let result = i2c.write_byte_by_byte(&data);
                if result.is_err() {
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
                let _ = i2c.set_slave(addr);
                let mut buf = vec![0u8; len];
                let result = i2c.read(&mut buf).map(|_| buf);
                if result.is_err() {
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
                let _ = i2c.set_slave(addr);
                let mut buf = vec![0u8; read_len];
                let result = i2c.write_read(&write_data, &mut buf).map(|_| buf);
                if result.is_err() {
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
                let result = i2c.set_timeout(timeout_jiffies);
                let _ = reply_tx.send(result);
            }
            I2cRequest::Transaction {
                addr,
                steps,
                reply_tx,
            } => {
                let result = execute_transaction(i2c, addr, steps);
                if result.is_err() {
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
        }
    }

    tracing::info!("I2C service thread exiting — channel closed");
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
        let tier = axi_iic_escalating_recovery(*consecutive_resets);
        if i2c_bus.is_none() {
            let mut b = I2cBus::open_devmem();
            if !write_denylist.is_empty() {
                b.set_write_denylist(write_denylist);
            }
            *i2c_bus = Some(b);
        }

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
///
/// v0.15.0: Simple service-fd write. The root cause of heartbeat failures was
/// AXI bus contention during FPGA WORK_TX bursts (open-core), NOT electrical
/// noise. The fix is pausing heartbeats during FPGA bursts (done in daemon.rs).
/// With the pause in place, heartbeats run in AXI-quiet windows and succeed
/// reliably — no aggressive retries needed.
fn execute_heartbeat(i2c: &mut I2cBus, addr: u8, _firmware: I2cPicFirmware) -> Result<()> {
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
    std::thread::sleep(std::time::Duration::from_millis(5));
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
fn execute_disable_voltage(i2c: &mut I2cBus, addr: u8, _firmware: I2cPicFirmware) -> Result<()> {
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
fn execute_set_voltage_mv(i2c: &mut I2cBus, addr: u8, voltage_mv: u16) -> Result<()> {
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
) -> Result<Vec<Vec<u8>>> {
    i2c.set_slave(addr)?;
    let mut reads = Vec::new();

    for step in steps {
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
                        bus: 0,
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
                        bus: 0,
                        addr,
                        detail: "transaction ReadFrame negative remaining length".into(),
                    });
                }
                let remaining = remaining_i as usize;
                if header.len().saturating_add(remaining) > max_len {
                    return Err(HalError::I2c {
                        bus: 0,
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

        // Drop current fd — releases kernel i2c_adapter lock
        *i2c_bus = None;

        // Wait for any in-flight kernel xiic timeout to complete.
        // The kernel xiic driver uses a 1s timeout per transaction with 3
        // internal retries. 50ms is enough for the driver to finish cleanup.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Reopen — gets a fresh fd but same kernel i2c_adapter underneath.
        // The kernel driver will attempt its own error recovery on next xfer.
        *i2c_bus = I2cBus::open(bus).ok();
        if let Some(ref mut i2c) = i2c_bus {
            // v0.14.0: Restore 100ms timeout on reopened fd
            let _ = i2c.set_timeout(10);
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
                let _ = restore_kernel_i2c_interrupts();
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
/// 1. `I2cServiceHandle` and the `spawn_i2c_service*` constructors are
///    visible from outside the HAL crate. (Compile-time check by reference.)
/// 2. `read_eeprom_bytes` is the only one-shot bus helper exposed to
///    out-of-HAL callers (compiles from any module path).
/// 3. `I2cBus::open` is **not** part of the public surface unless
///    `recovery-tool` is enabled. The CI grep gate plus `pub(crate)`
///    visibility together prevent regressions; this test documents the
///    intent in code.
#[cfg(test)]
mod lockdown_surface_tests {
    use super::*;

    #[test]
    fn service_handle_type_is_publicly_constructible_in_principle() {
        // Compile-time existence check: callers outside HAL only see
        // `I2cServiceHandle` and the `spawn_i2c_service*` constructors.
        // We don't actually spawn a service here — Linux-only side effects
        // would block on Windows hosts. Type-presence is enough to lock
        // the public surface.
        let _f: fn(u8, bool) -> std::io::Result<I2cServiceHandle> = spawn_i2c_service;
        let _g: fn(u8) -> std::io::Result<I2cServiceHandle> = spawn_i2c_service_no_register_touch;
        let _h: fn(u8, Vec<u8>) -> std::io::Result<I2cServiceHandle> =
            spawn_i2c_service_no_register_touch_with_denylist;
    }

    #[test]
    fn read_eeprom_bytes_signature_is_public_one_shot_helper() {
        // The daemon hardware-info path uses this helper instead of
        // `I2cBus::open(...)`. Test pins the signature.
        let _f: fn(u8, u8, u8, usize) -> Result<Vec<u8>> = read_eeprom_bytes;
    }

    /// Runtime smoke: confirm the read_eeprom_bytes signature returns a
    /// proper HalError when /dev/i2c-N is absent (cross-platform host
    /// side-effect-free path). On Linux without the bus this returns
    /// `DeviceOpen`; on Windows the same path returns an open error.
    /// Either way, no panic, no UB — the helper is a sound public API.
    #[test]
    fn read_eeprom_bytes_returns_err_when_bus_absent() {
        // Pick a bus number that won't exist as /dev/i2c-N on a CI host.
        let r = read_eeprom_bytes(0xFE, 0x51, 0, 1);
        assert!(r.is_err(), "expected Err for /dev/i2c-254 absent host");
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
