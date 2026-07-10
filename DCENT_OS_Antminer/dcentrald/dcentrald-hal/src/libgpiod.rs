//! Minimal libgpiod chardev v1 client (no external crate dep).
//!
//! Implements the `/dev/gpiochipN` ABI directly via `ioctl(2)` so we can
//! request GPIO lines as kernel-tracked consumers — the contract bosminer's
//! `gpiod-0.2.3` uses on Zynq XIL (S19j Pro `a lab unit`).
//!
//! ## Why not sysfs / devmem?
//!
//! - **sysfs** triggers Xilinx `xps-gpio`'s 32-bit cache write that clobbers
//!   unexported lines (gpio-901 `PWR_CONTROL` regression on `a lab unit`, 2026-04-24).
//! - **/dev/mem RMW** lands the bit in the FPGA output register but bypasses
//!   the kernel's pinmux / consumer tracking. Per Bible
//!   (Armada A `S11board` audit
//!   across 22 VNish firmwares), Zynq XIL `HBx_RESET` is driven via libgpiod
//!   with DT-resolved labels. Sysfs/devmem do **not** work for chain reset
//!   on XIL; AML uses sysfs `gpio454-456`.
//!
//! ## ABI source
//!
//! Linux `<linux/gpio.h>` v1 (kernel >= 4.8). The Zynq 4.4 vendor kernels we
//! ship on `a lab unit` (BraiinsOS+ kernel 4.4.0-xilinx) include the v1 chardev
//! backport — verified against `/dev/gpiochip0` ABI proven by bosminer.
//!
//! ## Reference
//!
//! -  §4 (Reset / power-cycle dance)
//! -  (2026-04-25, Armada A)
//! - Phase 13D Ghidra: `gpiod-0.2.3` symbol in bosminer.bin
//!

use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::{HalError, Result};

// ---------------------------------------------------------------------------
// chardev v1 ABI (matches `<linux/gpio.h>`)
// ---------------------------------------------------------------------------

const GPIOHANDLES_MAX: usize = 64;

pub const GPIOHANDLE_REQUEST_OUTPUT: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct GpiochipInfo {
    name: [u8; 32],
    label: [u8; 32],
    lines: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpiolineInfo {
    line_offset: u32,
    flags: u32,
    name: [u8; 32],
    consumer: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpiohandleRequest {
    line_offsets: [u32; GPIOHANDLES_MAX],
    flags: u32,
    default_values: [u8; GPIOHANDLES_MAX],
    consumer_label: [u8; 32],
    lines: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpiohandleData {
    values: [u8; GPIOHANDLES_MAX],
}

// ioctl numbers: IOC(dir, type, nr, size) = (dir<<30) | (size<<16) | (type<<8) | nr
//   _IOR  = dir=2 (read)
//   _IOWR = dir=3 (read+write)
//   type  = 0xB4 (GPIO ioctl group)
//
// `libc::ioctl`'s second arg is `Ioctl`, which is `c_ulong` on glibc but
// `c_int` on musl. We store the values as `u32` (their natural form) and
// cast at the call site so the type check passes on both targets.
const GPIO_GET_CHIPINFO_IOCTL: u32 = 0x8044_B401;
const GPIO_GET_LINEINFO_IOCTL: u32 = 0xC048_B402;
const GPIO_GET_LINEHANDLE_IOCTL: u32 = 0xC16C_B403;
const GPIOHANDLE_SET_LINE_VALUES_IOCTL: u32 = 0xC040_B409;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A GPIO line requested as an output via the chardev v1 API.
///
/// Owns the line FD; releases the line on `Drop` by closing it (which
/// kernel-side reverts the line to its previous direction/value).
pub struct RequestedLine {
    line_fd: OwnedFd,
    chip_path: PathBuf,
    offset: u32,
}

impl RequestedLine {
    /// Drive the line `high` or `low`.
    pub fn set_value(&self, high: bool) -> Result<()> {
        let mut data = GpiohandleData {
            values: [0; GPIOHANDLES_MAX],
        };
        data.values[0] = if high { 1 } else { 0 };
        let rc = unsafe {
            libc::ioctl(
                self.line_fd.as_raw_fd(),
                GPIOHANDLE_SET_LINE_VALUES_IOCTL as libc::c_int as _,
                &mut data as *mut _,
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            return Err(HalError::Gpio(format!(
                "GPIOHANDLE_SET_LINE_VALUES on {}:{} failed: {}",
                self.chip_path.display(),
                self.offset,
                err
            )));
        }
        Ok(())
    }
}

/// Request a single output line on the given gpiochip.
///
/// `consumer` becomes the kernel-visible label (limited to 31 ASCII chars).
pub fn request_output(
    chip_path: &Path,
    offset: u32,
    default_high: bool,
    consumer: &str,
) -> Result<RequestedLine> {
    let chip_fd = open_chip(chip_path)?;

    let mut req = GpiohandleRequest {
        line_offsets: [0; GPIOHANDLES_MAX],
        flags: GPIOHANDLE_REQUEST_OUTPUT,
        default_values: [0; GPIOHANDLES_MAX],
        consumer_label: [0; 32],
        lines: 1,
        fd: -1,
    };
    req.line_offsets[0] = offset;
    req.default_values[0] = if default_high { 1 } else { 0 };
    let label_bytes = consumer.as_bytes();
    let copy_len = label_bytes.len().min(31);
    req.consumer_label[..copy_len].copy_from_slice(&label_bytes[..copy_len]);

    let rc = unsafe {
        libc::ioctl(
            chip_fd.as_raw_fd(),
            GPIO_GET_LINEHANDLE_IOCTL as libc::c_int as _,
            &mut req as *mut _,
        )
    };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        return Err(HalError::Gpio(format!(
            "GPIO_GET_LINEHANDLE_IOCTL on {}:{} failed: {}",
            chip_path.display(),
            offset,
            err
        )));
    }
    if req.fd < 0 {
        return Err(HalError::Gpio(format!(
            "GPIO_GET_LINEHANDLE_IOCTL returned negative fd ({}) on {}:{}",
            req.fd,
            chip_path.display(),
            offset
        )));
    }

    // SAFETY: kernel returned a valid fd via the ioctl. We take ownership.
    let line_fd = unsafe { OwnedFd::from_raw_fd(req.fd as RawFd) };
    Ok(RequestedLine {
        line_fd,
        chip_path: chip_path.to_path_buf(),
        offset,
    })
}

/// Convenience: request an output line, drive it `high`, then release.
///
/// The line is held only for the duration of this call; on return the kernel
/// closes the consumer handle.
pub fn pulse_output(
    chip_path: &Path,
    offset: u32,
    consumer: &str,
    duration: std::time::Duration,
    final_high: bool,
) -> Result<()> {
    // Default-LOW request, then drive HIGH after the dwell. This matches
    // bosminer's HBx_RESET pulse (assert reset, dwell ~10-20 ms, release).
    let line = request_output(chip_path, offset, false, consumer)?;
    std::thread::sleep(duration);
    line.set_value(final_high)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Discovery: map a "Linux global GPIO number" → (chardev path, line offset)
//
// /sys/class/gpio/gpiochipN/ has `base` and `ngpio` files; the chardev
// counterpart is /dev/gpiochipM where M is the same N (the kernel
// numbering matches between sysfs and chardev for legacy gpiochip naming).
// We pick the chip whose base ≤ gpio < base+ngpio and compute offset.
// ---------------------------------------------------------------------------

/// Resolve a global GPIO number to its `(chip_path, line_offset)`.
///
/// Returns `Ok(None)` if no chardev backs the line (e.g., gpiolib-only or
/// uninstalled gpiochip nodes).
pub fn resolve_global_gpio(gpio: u32) -> Result<Option<(PathBuf, u32)>> {
    let entries = match fs::read_dir("/sys/class/gpio") {
        Ok(it) => it,
        Err(e) => return Err(HalError::Gpio(format!("list /sys/class/gpio: {}", e))),
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(rest) = name_str.strip_prefix("gpiochip") else {
            continue;
        };
        let chip_idx: u32 = match rest.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        let sysfs_dir = entry.path();
        let base = match read_uint(&sysfs_dir.join("base")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ngpio = match read_uint(&sysfs_dir.join("ngpio")) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if gpio >= base && gpio < base + ngpio {
            let chardev = PathBuf::from(format!("/dev/gpiochip{}", chip_idx));
            if !chardev.exists() {
                return Ok(None);
            }
            return Ok(Some((chardev, gpio - base)));
        }
    }
    Ok(None)
}

/// Look up a line offset by its DT-assigned name on a given chardev.
///
/// Useful when the gpio number isn't known but the DT label is, e.g.
/// `HB0_RESET` on the `0x41210000` PL GPIO bank.
pub fn line_offset_by_name(chip_path: &Path, label: &str) -> Result<Option<u32>> {
    let chip_fd = open_chip(chip_path)?;

    let mut info = GpiochipInfo {
        name: [0; 32],
        label: [0; 32],
        lines: 0,
    };
    let rc = unsafe {
        libc::ioctl(
            chip_fd.as_raw_fd(),
            GPIO_GET_CHIPINFO_IOCTL as libc::c_int as _,
            &mut info as *mut _,
        )
    };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        return Err(HalError::Gpio(format!(
            "GPIO_GET_CHIPINFO on {}: {}",
            chip_path.display(),
            err
        )));
    }

    let label_bytes = label.as_bytes();
    for offset in 0..info.lines {
        let mut line_info = GpiolineInfo {
            line_offset: offset,
            flags: 0,
            name: [0; 32],
            consumer: [0; 32],
        };
        let rc = unsafe {
            libc::ioctl(
                chip_fd.as_raw_fd(),
                GPIO_GET_LINEINFO_IOCTL as libc::c_int as _,
                &mut line_info as *mut _,
            )
        };
        if rc < 0 {
            continue;
        }
        let len = line_info
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(line_info.name.len());
        if &line_info.name[..len] == label_bytes {
            return Ok(Some(offset));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn open_chip(chip_path: &Path) -> Result<OwnedFd> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(chip_path)
        .map(|f| f.into())
        .map_err(|e| HalError::Gpio(format!("open {}: {}", chip_path.display(), e)))
}

fn read_uint(path: &Path) -> Result<u32> {
    let raw = fs::read_to_string(path)
        .map_err(|e| HalError::Gpio(format!("read {}: {}", path.display(), e)))?;
    raw.trim()
        .parse()
        .map_err(|e| HalError::Gpio(format!("parse {}: {}", path.display(), e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_constants_match_linux_gpio_h() {
        // _IOWR(0xB4, 0x03, struct gpiohandle_request) — size 364
        // (3 << 30) | (364 << 16) | (0xB4 << 8) | 0x03
        assert_eq!(GPIO_GET_LINEHANDLE_IOCTL, 0xC16C_B403);
        // _IOWR(0xB4, 0x09, struct gpiohandle_data) — size 64
        assert_eq!(GPIOHANDLE_SET_LINE_VALUES_IOCTL, 0xC040_B409);
        // _IOR(0xB4, 0x01, struct gpiochip_info) — size 68
        assert_eq!(GPIO_GET_CHIPINFO_IOCTL, 0x8044_B401);
        // _IOWR(0xB4, 0x02, struct gpioline_info) — size 72
        assert_eq!(GPIO_GET_LINEINFO_IOCTL, 0xC048_B402);
    }

    #[test]
    fn struct_sizes_match_linux_gpio_h() {
        assert_eq!(std::mem::size_of::<GpiochipInfo>(), 68);
        assert_eq!(std::mem::size_of::<GpiolineInfo>(), 72);
        assert_eq!(std::mem::size_of::<GpiohandleRequest>(), 364);
        assert_eq!(std::mem::size_of::<GpiohandleData>(), 64);
    }
}
