//! `bitmain_axi.ko` IOCTL adapter — **DEV/DEBUG ONLY**, NOT a production code path.
//!
//! ## RE3 status (2026-05-10) + W13.B5 demotion (2026-05-10)
//!
//! Per `RE_DELIVERABLES/bitmain_axi_ioctl_report.md` (RE-graded A,
//! DWARF-confirmed), the **production** kernel modules (`bitmain_axi.ko`
//! AM335x and `cv183x_base.ko` CV1835) implement **ZERO** `unlocked_ioctl` /
//! `compat_ioctl` handlers. Production `bmminer` / `bosminer` exclusively
//! use `open("/dev/axi_fpga_dev") + mmap() + volatile u32 R/W` —
//! see [`crate::stock_fpga_axi_mmap::BitmainAxiMmapBackend`].
//!
//! The W11.1 IOCTL ordinals encoded in this module (`magic='X'`,
//! `REG_READ=0x11`, `REG_WRITE=0x12`, `BURST_READ=0x13`, `BURST_WRITE=0x14`)
//! were inferred from a `fake_axi_fpga.c` QEMU rehosting **stub** and a
//! reference `apw12.c` caller — NOT from the production kernel module. RE3
//! DWARF inspection of every shipped `.ko` we hold proves the surface does
//! not exist on production hardware.
//!
//! ## This module is preserved only for QEMU/fake-axi rehosting tests
//!
//! It is gated behind the `axi-ioctl-debug` Cargo feature
//! (`Cargo.toml::[features]`). Production `dcentrald` does NOT enable this
//! feature → this file is **not compiled into shipping firmware**. Calling
//! anything in here from non-feature-gated code is a compile error.
//!
//! Use cases that legitimately need this code path:
//! * QEMU / lab rehosting environments running the `fake_axi_fpga.c` stub
//!   that DOES expose the IOCTL surface.
//! * Future kernel forks that legitimately add an ioctl handler (would
//!   require fresh RE confirmation before flipping back to runtime use).
//! * Diagnostic / parity comparison work where mmap and IOCTL paths must
//!   produce the same observable register values.
//!
//! ## W13.B5 env-gate retirement
//!
//! The W10-era runtime env-gate `DCENT_BB_TRUST_INFERRED_AXI_IOCTL` was
//! RETIRED in W13.B5 (2026-05-10). The IOCTL backend is no longer
//! runtime-selectable; it is only available when this Cargo feature is
//! enabled at build time. Setting that env var has NO effect on backend
//! selection — production builds always pick mmap, period.
//!
//! ## Bitstream compatibility (still in force)
//!
//! Even when this feature is enabled, this backend is for STOCK Bitmain
//! bitstreams ONLY. NEVER use on BraiinsOS / Mujina FPGA bitstreams —
//! different register layout, different DMA model. Same rule as
//! decision #9 ("DCENT_OS MUST use BraiinsOS FPGA bitstream"). See
//! .
//!
//! ## Backend selection contract (when feature is on)
//!
//! [`BitmainAxiBackend::try_open`] returns:
//! * `Ok(Some(_))` only when `/dev/axi_fpga_dev` exists AND no `/dev/uio*`
//!   nodes are present (BB / CV1835 lab path).
//! * `Ok(None)` otherwise.
//!
//! ## Historical note
//!
//! Originally added in W10 (RE-inferred ordinals NR `0x01..=0x04`). W11.1
//! re-keyed to RE2-confirmed ordinals (`0x11..=0x14`) and removed the W10
//! env-gate. W12.1 introduced the canonical mmap backend and reclassified
//! this module as dev/debug. W13.B5 demotes it to a Cargo feature gate so
//! it cannot accidentally compile into shipping firmware.

#![cfg(feature = "axi-ioctl-debug")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::{HalError, Result};

// ---------------------------------------------------------------------------
// _IOC encoding (Linux generic, asm-generic/ioctl.h)
// ---------------------------------------------------------------------------
//
// On every architecture we ship to (armv7-unknown-linux-musleabihf,
// aarch64-unknown-linux-musl, riscv64-unknown-linux-musl) the kernel uses the
// asm-generic encoding:
//
//     dir   : 2 bits  @ 30
//     size  : 14 bits @ 16
//     type  : 8 bits  @ 8
//     nr    : 8 bits  @ 0
//
// Direction: 0=NONE, 1=WRITE (user→kernel), 2=READ (kernel→user),
// 3=READ|WRITE.

#[allow(dead_code)]
const IOC_NRSHIFT: u32 = 0;
#[allow(dead_code)]
const IOC_TYPESHIFT: u32 = 8;
#[allow(dead_code)]
const IOC_SIZESHIFT: u32 = 16;
#[allow(dead_code)]
const IOC_DIRSHIFT: u32 = 30;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, magic: u8, nr: u32, size: usize) -> libc::c_ulong {
    (((dir as u64) << IOC_DIRSHIFT)
        | ((magic as u64) << IOC_TYPESHIFT)
        | ((nr as u64) << IOC_NRSHIFT)
        | ((size as u64) << IOC_SIZESHIFT)) as libc::c_ulong
}

const fn iow<T>(magic: u8, nr: u32) -> libc::c_ulong {
    ioc(IOC_WRITE, magic, nr, core::mem::size_of::<T>())
}
#[allow(dead_code)]
const fn ior<T>(magic: u8, nr: u32) -> libc::c_ulong {
    ioc(IOC_READ, magic, nr, core::mem::size_of::<T>())
}
const fn iowr<T>(magic: u8, nr: u32) -> libc::c_ulong {
    ioc(IOC_READ | IOC_WRITE, magic, nr, core::mem::size_of::<T>())
}
#[allow(dead_code)]
const fn io_(magic: u8, nr: u32) -> libc::c_ulong {
    ioc(IOC_NONE, magic, nr, 0)
}

// ---------------------------------------------------------------------------
// bitmain_axi.ko ABI — DEV/DEBUG, RE2 W11.1 ordinals
// ---------------------------------------------------------------------------
//
// Source (NOT production):
//   DCENT_OS_DEVELOPMENT_KITRE2/.../DCENT_OS_ §12.3
//   SOURCE_HAL/apw12.c (reference IOCTL caller against `fake_axi_fpga.c` stub)
//
// Stub kernel header used by the QEMU rehosting environment:
//
//     #define AXI_IOC_MAGIC  'X'   // 0x58
//     #define AXI_IOCTL_REG_READ    _IOWR('X', 0x11, struct axi_rw)
//     #define AXI_IOCTL_REG_WRITE   _IOW ('X', 0x12, struct axi_rw)
//     #define AXI_IOCTL_BURST_READ  _IOWR('X', 0x13, struct axi_burst)
//     #define AXI_IOCTL_BURST_WRITE _IOW ('X', 0x14, struct axi_burst)
//
//     struct axi_rw    { uint32_t offset; uint32_t value; uint8_t chain; };
//     struct axi_burst { uint32_t offset; uint32_t *buf; uint32_t count;
//                        uint8_t chain; uint8_t direction; };
//
// RE3 DWARF inspection (W12.1) confirmed these ordinals do NOT exist in any
// production `bitmain_axi.ko` or `cv183x_base.ko`. Kept here for QEMU/fake-
// axi parity only.

/// IOCTL "magic" type byte. `'X'` per `fake_axi_fpga.c` stub (RE2 W11.1).
pub const AXI_IOC_MAGIC: u8 = b'X';

/// Single-register read or write (`struct axi_rw`).
///
/// Layout matches the QEMU stub kernel ABI exactly. 4 + 4 + 1 = 9 payload
/// bytes, padded to 12 on every target we ship (4-byte alignment for `u32`
/// fields). The IOCTL size encoding uses `sizeof(struct axi_rw)` directly,
/// so the padding must match what the stub module's compiler chose —
/// verified by the layout test below.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AxiRw {
    pub offset: u32,
    pub value: u32,
    pub chain: u8,
}

/// Burst read or write (`struct axi_burst`).
///
/// `buf` is a userspace pointer the stub kernel module fills (BURST_READ) or
/// reads from (BURST_WRITE). `count` is the number of u32 words.
/// `direction` is set by userspace (0 = read, 1 = write per RE2 reference).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AxiBurst {
    pub offset: u32,
    pub buf: *mut u32,
    pub count: u32,
    pub chain: u8,
    pub direction: u8,
}

impl Default for AxiBurst {
    fn default() -> Self {
        Self {
            offset: 0,
            buf: core::ptr::null_mut(),
            count: 0,
            chain: 0,
            direction: 0,
        }
    }
}

const AXI_IOCTL_REG_READ: libc::c_ulong = iowr::<AxiRw>(AXI_IOC_MAGIC, 0x11);
const AXI_IOCTL_REG_WRITE: libc::c_ulong = iow::<AxiRw>(AXI_IOC_MAGIC, 0x12);
#[allow(dead_code)]
const AXI_IOCTL_BURST_READ: libc::c_ulong = iowr::<AxiBurst>(AXI_IOC_MAGIC, 0x13);
#[allow(dead_code)]
const AXI_IOCTL_BURST_WRITE: libc::c_ulong = iow::<AxiBurst>(AXI_IOC_MAGIC, 0x14);

/// Default device path created by `bitmain_axi.ko` (and the QEMU stub).
pub const DEV_AXI_FPGA: &str = "/dev/axi_fpga_dev";

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// IOCTL-backed FPGA register shuttle for the QEMU `fake_axi_fpga.c` stub.
///
/// Production AM335x BB and Cvitek CV1835 control boards do NOT expose this
/// surface (RE3 DWARF-confirmed). Use [`crate::stock_fpga_axi_mmap::BitmainAxiMmapBackend`]
/// for production. This type only compiles when the `axi-ioctl-debug` Cargo
/// feature is enabled.
pub struct BitmainAxiBackend {
    fd: File,
}

impl BitmainAxiBackend {
    /// Open `/dev/axi_fpga_dev` if it is the active stub-IOCTL path on this host.
    ///
    /// Returns `Ok(None)` when:
    /// * `/dev/axi_fpga_dev` does not exist (unrelated platform, e.g. desktop
    ///   build host or a UIO-only BraiinsOS Zynq image), OR
    /// * `/dev/uio*` nodes ARE present (Zynq integrated PL — keep using the
    ///   existing UIO+devmem path).
    pub fn try_open() -> Result<Option<Self>> {
        // Probe /dev/axi_fpga_dev first. Absent → not our path.
        if !Path::new(DEV_AXI_FPGA).exists() {
            tracing::debug!(
                path = DEV_AXI_FPGA,
                "bitmain_axi.ko IOCTL backend skipped: device node missing"
            );
            return Ok(None);
        }

        // If any /dev/uio* exists, prefer the UIO+devmem path (Zynq).
        // We check for /dev/uio0 specifically because every supported Zynq
        // image we ship guarantees uio0 (FPGA chain 6). Globbing /dev/uio*
        // would be more thorough but std has no glob; checking uio0 mirrors
        // what `crate::uio::Uio::open_index(0)` would attempt anyway.
        if Path::new("/dev/uio0").exists() {
            tracing::debug!(
                "bitmain_axi.ko IOCTL backend skipped: /dev/uio0 present, deferring to UIO path"
            );
            return Ok(None);
        }

        let fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(DEV_AXI_FPGA)
            .map_err(|e| HalError::Platform(format!("open {DEV_AXI_FPGA}: {e}")))?;

        tracing::warn!(
            path = DEV_AXI_FPGA,
            "bitmain_axi.ko IOCTL backend selected (DEV/DEBUG — production kernels have NO IOCTL handler per RE3)"
        );

        Ok(Some(Self { fd }))
    }

    /// Write a 32-bit FPGA register on the given chain.
    ///
    /// Issues `AXI_IOCTL_REG_WRITE` (`_IOW('X', 0x12, struct axi_rw)`).
    pub fn write_reg(&self, chain_id: u8, offset: u32, value: u32) -> Result<()> {
        let mut payload = AxiRw {
            offset,
            value,
            chain: chain_id,
        };
        // SAFETY: payload is a #[repr(C)] POD owned by us for the duration
        // of the call; the kernel reads at most `size_of::<AxiRw>()` bytes
        // (encoded in the IOCTL ordinal). The fd is owned by `self` and
        // outlives this call.
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                AXI_IOCTL_REG_WRITE as _,
                &mut payload as *mut AxiRw,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "AXI_IOCTL_REG_WRITE(chain={chain_id}, off=0x{offset:04X}) failed: {}",
                io::Error::last_os_error(),
            )));
        }
        Ok(())
    }

    /// Read a 32-bit FPGA register from the given chain.
    ///
    /// Issues `AXI_IOCTL_REG_READ` (`_IOWR('X', 0x11, struct axi_rw)`).
    /// Userspace fills `offset`+`chain`, kernel populates `value`.
    pub fn read_reg(&self, chain_id: u8, offset: u32) -> Result<u32> {
        let mut payload = AxiRw {
            offset,
            value: 0,
            chain: chain_id,
        };
        // SAFETY: payload owned, fd outlives call. The IOCTL is _IOWR — the
        // kernel reads `offset`+`chain` and writes `value`.
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                AXI_IOCTL_REG_READ as _,
                &mut payload as *mut AxiRw,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "AXI_IOCTL_REG_READ(chain={chain_id}, off=0x{offset:04X}) failed: {}",
                io::Error::last_os_error(),
            )));
        }
        Ok(payload.value)
    }

    /// Burst-read `count` consecutive 32-bit registers into `buf`.
    ///
    /// Issues `AXI_IOCTL_BURST_READ` (`_IOWR('X', 0x13, struct axi_burst)`).
    /// `direction = 0` per RE2 reference. `buf.len()` must be ≥ `count`.
    pub fn burst_read(&self, chain_id: u8, offset: u32, buf: &mut [u32]) -> Result<()> {
        let count = buf.len() as u32;
        let mut payload = AxiBurst {
            offset,
            buf: buf.as_mut_ptr(),
            count,
            chain: chain_id,
            direction: 0,
        };
        // SAFETY: `buf` outlives the call (caller's lifetime). `payload` is
        // POD, the kernel reads its scalar fields and writes `count` u32s
        // into `*buf`. We pass the IOCTL ordinal that encodes
        // `sizeof::<AxiBurst>()` so the kernel's copy_from_user matches.
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                AXI_IOCTL_BURST_READ as _,
                &mut payload as *mut AxiBurst,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "AXI_IOCTL_BURST_READ(chain={chain_id}, off=0x{offset:04X}, count={count}) failed: {}",
                io::Error::last_os_error(),
            )));
        }
        Ok(())
    }

    /// Burst-write `count` consecutive 32-bit registers from `buf`.
    ///
    /// Issues `AXI_IOCTL_BURST_WRITE` (`_IOW('X', 0x14, struct axi_burst)`).
    /// `direction = 1` per RE2 reference.
    pub fn burst_write(&self, chain_id: u8, offset: u32, buf: &[u32]) -> Result<()> {
        let count = buf.len() as u32;
        let mut payload = AxiBurst {
            offset,
            // The kernel module reads from `buf` for BURST_WRITE; cast away
            // const safely since we never let the kernel write through this
            // pointer for direction=1.
            buf: buf.as_ptr() as *mut u32,
            count,
            chain: chain_id,
            direction: 1,
        };
        // SAFETY: `buf` outlives the call. The IOCTL is _IOW so the kernel
        // copies the struct in and reads `count` u32s from `*buf`.
        let ret = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                AXI_IOCTL_BURST_WRITE as _,
                &mut payload as *mut AxiBurst,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "AXI_IOCTL_BURST_WRITE(chain={chain_id}, off=0x{offset:04X}, count={count}) failed: {}",
                io::Error::last_os_error(),
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The `fake_axi_fpga.c` stub IOCTL magic byte is `'X'`.
    #[test]
    fn magic_byte_is_capital_x() {
        assert_eq!(AXI_IOC_MAGIC, b'X');
        assert_eq!(AXI_IOC_MAGIC, 0x58);
    }

    /// `AxiRw` is `4 + 4 + 1 = 9` bytes of payload, padded to 12 on every
    /// target we ship (4-byte alignment for u32 fields). The kernel module
    /// header uses `sizeof(struct axi_rw)` for the IOCTL size field, so this
    /// number MUST match what the module's compiler chose — and on every
    /// Bitmain-supplied kernel (gcc 4.8 / 7.5 / 9.x targeting armv7 / aarch64)
    /// the layout is also 12 bytes. If a future toolchain ever produced 9
    /// or some other size, BURST_* IOCTLs would also shift and the kernel
    /// would return EINVAL — surface that loudly here.
    #[test]
    fn axi_rw_layout_is_12_bytes() {
        assert_eq!(core::mem::size_of::<AxiRw>(), 12);
        assert_eq!(core::mem::align_of::<AxiRw>() % 4, 0);
    }

    /// `AxiBurst` carries `offset:u32 + buf:*mut u32 + count:u32 + chain:u8 +
    /// direction:u8`. On 32-bit targets (armv7) `*mut u32` is 4 bytes →
    /// 4+4+4+1+1 padded to 16. On 64-bit targets (aarch64, x86_64 host)
    /// `*mut u32` is 8 bytes → 4+pad4+8+4+1+1 padded to 24. The kernel
    /// module is compiled for the target SoC, so its `sizeof(struct
    /// axi_burst)` matches the host layout we encode here — no cross-target
    /// drift to worry about.
    #[test]
    fn axi_burst_layout_matches_pointer_width() {
        let sz = core::mem::size_of::<AxiBurst>();
        let expected = if core::mem::size_of::<*const ()>() == 4 {
            16
        } else {
            24
        };
        assert_eq!(sz, expected, "AxiBurst layout drift on this target");
        assert_eq!(core::mem::align_of::<AxiBurst>() % 4, 0);
    }

    /// Pin the bit-encoding of every IOCTL ordinal so a future refactor of
    /// `iow!`/`ior!`/`iowr!` or struct layout doesn't silently shift them.
    /// Values below come from RE2 §12.3 + `SOURCE_HAL/apw12.c`.
    #[test]
    fn ioctl_ordinal_encoding_pins() {
        // Direction is upper 2 bits.
        assert_eq!(
            (AXI_IOCTL_REG_READ >> IOC_DIRSHIFT) & 0b11,
            (IOC_READ | IOC_WRITE) as libc::c_ulong,
            "REG_READ must be _IOWR per RE2"
        );
        assert_eq!(
            (AXI_IOCTL_REG_WRITE >> IOC_DIRSHIFT) & 0b11,
            IOC_WRITE as libc::c_ulong,
            "REG_WRITE must be _IOW per RE2"
        );
        assert_eq!(
            (AXI_IOCTL_BURST_READ >> IOC_DIRSHIFT) & 0b11,
            (IOC_READ | IOC_WRITE) as libc::c_ulong,
            "BURST_READ must be _IOWR per RE2"
        );
        assert_eq!(
            (AXI_IOCTL_BURST_WRITE >> IOC_DIRSHIFT) & 0b11,
            IOC_WRITE as libc::c_ulong,
            "BURST_WRITE must be _IOW per RE2"
        );

        // Magic byte is 'X' for all four.
        for ord in [
            AXI_IOCTL_REG_READ,
            AXI_IOCTL_REG_WRITE,
            AXI_IOCTL_BURST_READ,
            AXI_IOCTL_BURST_WRITE,
        ] {
            let magic = ((ord >> IOC_TYPESHIFT) & 0xFF) as u8;
            assert_eq!(magic, AXI_IOC_MAGIC, "magic byte must be 'X'");
        }

        // NR ordinals match RE2 header.
        assert_eq!((AXI_IOCTL_REG_READ >> IOC_NRSHIFT) & 0xFF, 0x11);
        assert_eq!((AXI_IOCTL_REG_WRITE >> IOC_NRSHIFT) & 0xFF, 0x12);
        assert_eq!((AXI_IOCTL_BURST_READ >> IOC_NRSHIFT) & 0xFF, 0x13);
        assert_eq!((AXI_IOCTL_BURST_WRITE >> IOC_NRSHIFT) & 0xFF, 0x14);

        // Size field is sizeof::<struct>().
        assert_eq!(
            (AXI_IOCTL_REG_READ >> IOC_SIZESHIFT) & 0x3FFF,
            core::mem::size_of::<AxiRw>() as libc::c_ulong,
        );
        assert_eq!(
            (AXI_IOCTL_REG_WRITE >> IOC_SIZESHIFT) & 0x3FFF,
            core::mem::size_of::<AxiRw>() as libc::c_ulong,
        );
        assert_eq!(
            (AXI_IOCTL_BURST_READ >> IOC_SIZESHIFT) & 0x3FFF,
            core::mem::size_of::<AxiBurst>() as libc::c_ulong,
        );
        assert_eq!(
            (AXI_IOCTL_BURST_WRITE >> IOC_SIZESHIFT) & 0x3FFF,
            core::mem::size_of::<AxiBurst>() as libc::c_ulong,
        );
    }

    /// On Windows (and any host where `/dev/axi_fpga_dev` does not exist)
    /// the probe must return `Ok(None)` — never error. This is the contract
    /// `stock_fpga_work::WorkBackend::select` relies on for fallback to the
    /// existing UIO path on Zynq.
    #[test]
    fn try_open_returns_none_when_device_absent() {
        // On Windows host this path simply cannot exist; on Linux dev hosts
        // without bitmain_axi.ko loaded it also won't exist. Both are
        // legitimate "not our backend" cases.
        if !Path::new(DEV_AXI_FPGA).exists() {
            let result = BitmainAxiBackend::try_open();
            match result {
                Ok(None) => { /* expected */ }
                Ok(Some(_)) => panic!(
                    "BitmainAxiBackend::try_open returned Some despite {DEV_AXI_FPGA} not existing"
                ),
                Err(e) => panic!("try_open errored on absent device: {e}"),
            }
        }
        // If the device DOES exist (rare CI case), we deliberately don't
        // assert anything — open may legitimately succeed or fail with EBUSY.
    }

    /// Defensive: REG_WRITE and REG_READ must not collide after encoding.
    /// Same for BURST_*. Paranoia test against future shift bugs.
    #[test]
    fn write_and_read_ordinals_differ() {
        assert_ne!(AXI_IOCTL_REG_WRITE, AXI_IOCTL_REG_READ);
        assert_ne!(AXI_IOCTL_BURST_WRITE, AXI_IOCTL_BURST_READ);
        // And the single-reg vs burst paths must also be distinct.
        assert_ne!(AXI_IOCTL_REG_READ, AXI_IOCTL_BURST_READ);
        assert_ne!(AXI_IOCTL_REG_WRITE, AXI_IOCTL_BURST_WRITE);
    }
}
