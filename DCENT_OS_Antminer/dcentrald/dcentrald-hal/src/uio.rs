//! UIO (Userspace I/O) device driver.
//!
//! The foundation of all FPGA register access on Zynq boards. Opens a UIO
//! device node (/dev/uioN), mmaps the register region (4 KB), and provides
//! type-safe register read/write operations.
//!
//! Safety: The mmap pointer is only valid within the 4 KB boundary. All offset
//! access is bounds-checked in debug builds. Accessing unmapped FPGA space
//! causes AXI external abort faults that crash the process.
//!
//! The S9 has 14 UIO devices:
//!   - 12 chain registers (4 per chain x 3 chains)
//!   - 1 fan controller
//!   - 1 glitch monitor

use std::fs;
use std::num::NonZeroUsize;
use std::os::fd::AsRawFd;

use nix::sys::mman::{MapFlags, ProtFlags};

use crate::{HalError, Result};

/// Size of each UIO mmap region (one page = 4096 bytes).
pub const UIO_MAP_SIZE: usize = 4096;

/// FPGA-2: pure, host-testable register-offset validator.
///
/// A 32-bit register access at `offset` is valid only if it is 4-byte aligned
/// AND the whole 4-byte word lies inside the `size`-byte mapped window. Both
/// `read_reg` and `write_reg` gate every access (in every build) on this so an
/// out-of-bounds / misaligned offset can never reach the unsafe volatile access
/// — that would fault the AXI bus (process crash) or be UB in release where the
/// old `debug_assert!` was compiled out.
#[inline]
pub fn offset_in_bounds(offset: u32, size: usize) -> bool {
    // 4-byte alignment, and offset + 4 must fit within the window.
    offset.is_multiple_of(4) && (offset as usize).saturating_add(4) <= size
}

/// A single UIO device with an mmap'd register region.
pub struct UioDevice {
    /// File descriptor for /dev/uioN (owned).
    file: std::fs::File,
    /// mmap'd register base pointer (4 KB region of 32-bit registers).
    regs: *mut u32,
    /// Mapped region size (always 4096 bytes for FPGA register blocks).
    size: usize,
    /// UIO device name from sysfs (e.g., "chain6-common").
    name: String,
}

// SAFETY: UioDevice is safe to send between threads because the mmap'd
// memory is process-global and the fd is valid for the process lifetime.
// Actual thread safety of register access depends on the hardware --
// concurrent access to the same registers is the caller's responsibility.
unsafe impl Send for UioDevice {}
unsafe impl Sync for UioDevice {}

impl UioDevice {
    /// Open a UIO device by number.
    ///
    /// Opens /dev/uioN, reads the device name from sysfs, and mmaps the
    /// first register region (4 KB).
    pub fn open(uio_number: u8) -> Result<Self> {
        let dev_path = format!("/dev/uio{}", uio_number);
        let name_path = format!("/sys/class/uio/uio{}/name", uio_number);

        // Read device name from sysfs
        let name = fs::read_to_string(&name_path)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| format!("uio{}", uio_number));

        // Open the device file for read/write
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&dev_path)
            .map_err(|e| HalError::DeviceOpen {
                path: dev_path.clone(),
                source: e,
            })?;

        // mmap the first region (offset 0, size 4096)
        let size = UIO_MAP_SIZE;
        let regs = unsafe {
            nix::sys::mman::mmap(
                None,
                NonZeroUsize::new(size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &file,
                0, // offset 0 = first region
            )
            .map_err(|e| HalError::MmapFailed {
                device: dev_path.clone(),
                source: e,
            })?
        };

        tracing::debug!(
            uio = uio_number,
            name = %name,
            "Opened UIO device"
        );

        Ok(Self {
            file,
            regs: regs.as_ptr() as *mut u32,
            size,
            name,
        })
    }

    /// Read a 32-bit register at the given byte offset.
    ///
    /// FPGA-2 safety: the bounds + 4-byte-alignment check is a **real runtime
    /// guard in every build**, not a `debug_assert!`. An out-of-bounds or
    /// misaligned offset would dereference past the 4 KB mmap window and trigger
    /// an AXI external-abort fault (process crash). In release the check used to
    /// be compiled out entirely, so a bad offset was undefined behavior. We now
    /// always validate and, on a bad offset, log and return `0` (skip) rather
    /// than perform the access. The fast path is two integer comparisons.
    #[inline]
    pub fn read_reg(&self, offset: u32) -> u32 {
        if !offset_in_bounds(offset, self.size) {
            tracing::error!(
                device = %self.name,
                offset = format_args!("0x{:04X}", offset),
                size = self.size,
                "UIO read_reg refused: offset out of bounds or not 4-byte aligned (returning 0)"
            );
            return 0;
        }

        unsafe {
            let ptr = self.regs.add((offset / 4) as usize);
            std::ptr::read_volatile(ptr)
        }
    }

    /// Write a 32-bit value to a register at the given byte offset.
    ///
    /// FPGA-2 safety: the bounds + 4-byte-alignment check is a **real runtime
    /// guard in every build** (see `read_reg`). An out-of-bounds or misaligned
    /// write would corrupt unmapped FPGA space / fault the process; in release
    /// the old `debug_assert!` was compiled out. We now always validate and, on
    /// a bad offset, log and skip the write.
    #[inline]
    pub fn write_reg(&self, offset: u32, value: u32) {
        if !offset_in_bounds(offset, self.size) {
            tracing::error!(
                device = %self.name,
                offset = format_args!("0x{:04X}", offset),
                size = self.size,
                value = format_args!("0x{:08X}", value),
                "UIO write_reg refused: offset out of bounds or not 4-byte aligned (skipping write)"
            );
            return;
        }

        unsafe {
            let ptr = self.regs.add((offset / 4) as usize);
            std::ptr::write_volatile(ptr, value);
        }
    }

    /// Block until an IRQ fires on this UIO device.
    ///
    /// Returns the IRQ count (number of interrupts since device open).
    /// Blocking read of 4 bytes from the UIO fd returns the IRQ count.
    pub fn wait_irq(&self) -> Result<u32> {
        let mut buf = [0u8; 4];

        // Use nix::unistd::read for blocking read on UIO fd (read takes RawFd)
        let n = nix::unistd::read(self.file.as_raw_fd(), &mut buf).map_err(|e| {
            HalError::Io(std::io::Error::other(format!("UIO IRQ wait failed: {}", e)))
        })?;

        if n != 4 {
            return Err(HalError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("UIO IRQ read returned {} bytes, expected 4", n),
            )));
        }

        Ok(u32::from_ne_bytes(buf))
    }

    /// Enable (re-arm) IRQ on this UIO device.
    ///
    /// Must be called after each wait_irq() to re-enable the interrupt.
    /// Write 1u32 to the UIO fd to re-enable interrupts.
    pub fn enable_irq(&self) -> Result<()> {
        let val: u32 = 1;

        nix::unistd::write(&self.file, &val.to_ne_bytes()).map_err(|e| {
            HalError::Io(std::io::Error::other(format!(
                "UIO IRQ enable failed: {}",
                e
            )))
        })?;

        Ok(())
    }

    /// Get the device name (from sysfs).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the raw file descriptor (for select/poll).
    pub fn raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

impl Drop for UioDevice {
    fn drop(&mut self) {
        // Late-exit stability matters more than eager unmapping here. These
        // device mmaps live for the lifetime of the daemon, and the kernel
        // tears them down automatically when the process exits. Avoiding an
        // explicit `munmap` sidesteps late-shutdown crashes on flashed units.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FPGA-2 regression: the offset validator is a real runtime guard (not a
    /// debug_assert), so the same logic must reject out-of-bounds + misaligned
    /// offsets in every build. `read_reg`/`write_reg` route every access through
    /// this; a `false` here means they log-and-skip instead of faulting the AXI
    /// bus / hitting release-mode UB. `UioDevice` itself needs a live /dev/uioN
    /// mmap, so we test the pure extracted helper.
    #[test]
    fn fpga2_offset_in_bounds_accepts_valid_register_offsets() {
        // Real FPGA register offsets used across the HAL: aligned, well inside
        // the 4 KB window. These MUST stay valid (no valid-path regression).
        for off in [0x00u32, 0x04, 0x08, 0x0C, 0x10, 0x14, 0x18] {
            assert!(
                offset_in_bounds(off, UIO_MAP_SIZE),
                "0x{off:04X} must be a valid register offset"
            );
        }
        // Last fully-in-bounds aligned word of a 4 KB page is 0xFFC..0xFFF.
        assert!(offset_in_bounds(0xFFC, UIO_MAP_SIZE));
    }

    #[test]
    fn fpga2_offset_in_bounds_rejects_out_of_bounds_and_misaligned() {
        // At/just past the end of the window.
        assert!(!offset_in_bounds(UIO_MAP_SIZE as u32, UIO_MAP_SIZE)); // 0x1000
        assert!(!offset_in_bounds(0x1004, UIO_MAP_SIZE));
        // A word that STARTS in-bounds but whose last byte overruns the window.
        assert!(!offset_in_bounds(0xFFD, UIO_MAP_SIZE));
        assert!(!offset_in_bounds(0xFFF, UIO_MAP_SIZE));
        // Misaligned offsets (a non-4-byte-aligned volatile u32 access is UB).
        assert!(!offset_in_bounds(0x01, UIO_MAP_SIZE));
        assert!(!offset_in_bounds(0x02, UIO_MAP_SIZE));
        assert!(!offset_in_bounds(0x06, UIO_MAP_SIZE));
        // The extreme value that would wrap a naive `offset + 4` — saturating
        // add keeps it rejected rather than overflowing to a small number.
        assert!(!offset_in_bounds(u32::MAX, UIO_MAP_SIZE));
    }
}
