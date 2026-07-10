//! Standard Linux serial port for ASIC chain communication.
//!
//! Used on multiple platforms for ASIC command/response and (on non-FPGA
//! platforms) also for work dispatch:
//!
//!   - **Zynq S19 (hybrid):** PL ns16550a UARTs at 0x4100x000 exposed as
//!     /dev/ttyS1-4 handle ASIC command/control. FPGA UIO work FIFOs at
//!     0x43Cx0000 handle mining work separately.
//!   - **Amlogic (S19k, S21):** /dev/ttyS1-3, all traffic on one UART per chain.
//!   - **BeagleBone (S19j):** /dev/ttyS1/ttyS2/ttyS4 (and /dev/ttyS5 on
//!     4-board SKUs), all traffic on one UART per chain. (Earlier W4-era
//!     code used `/dev/ttyO%d`; the live `a lab unit` LuxOS probe (2026-05-12)
//!     confirmed the mainline omap-serial driver names them `/dev/ttyS%d` —
//!     the MMIO base addresses are unchanged.)
//!
//! Key differences from FPGA cmd FIFO path:
//!   - Software must prepend preamble (0x55 0xAA) and append CRC
//!   - Software must parse response preamble (0xAA 0x55) from byte stream
//!   - Baud rate set via termios (standard rates) or BOTHER ioctl (custom rates)
//!   - No IRQ-based notification — must poll with VTIME or epoll
//!   - Supports custom baud rates up to 6.25 Mbaud via BOTHER ioctl
//!
//! The NS16550A PL UARTs on S19 Zynq have base_baud = 6,249,999 Hz, supporting
//! operational rates of 1.5625 Mbaud (div=4) and 3.125 Mbaud (div=2).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

use crate::{HalError, Result};

/// Standard baud rates supported by Bitmain ASICs.
pub const BAUD_115200: u32 = 115_200;
pub const BAUD_1562500: u32 = 1_562_500;
pub const BAUD_3125000: u32 = 3_125_000;
pub const BAUD_3000000: u32 = 3_000_000;
pub const BAUD_6250000: u32 = 6_250_000;

// ---------------------------------------------------------------------------
// BOTHER ioctl constants for custom baud rates on Linux.
//
// When the standard BaudRate enum doesn't include the desired rate (e.g.,
// 1.5625 MHz or 3.125 MHz), we use the BOTHER flag in c_cflag and write
// the exact baud rate into c_ispeed/c_ospeed of struct termios2.
// This is the Linux-specific "custom baud rate" mechanism.
// ---------------------------------------------------------------------------

/// BOTHER cflag value — tells the kernel to use c_ispeed/c_ospeed fields
/// for the actual baud rate instead of the Bfoo enum constants.
/// Value: 0o010017 (octal) = 0x1007.
const BOTHER: u32 = 0o010017;

/// Mask for the baud rate bits in c_cflag (CBAUD).
const CBAUD: u32 = 0o010017;

/// TCSETS2 ioctl number for setting termios2 (includes custom baud).
/// From linux/asm-generic/ioctls.h: _IOW('T', 0x2B, struct termios2)
/// On ARM: 0x402C542B
const TCSETS2: libc::c_ulong = 0x402C542B;

/// TCGETS2 ioctl number for getting termios2.
/// From linux/asm-generic/ioctls.h: _IOR('T', 0x2A, struct termios2)
/// On ARM: 0x802C542A
const TCGETS2: libc::c_ulong = 0x802C542A;

/// Traditional setserial ioctls used by stock bosminer on Zynq am2.
const TIOCGSERIAL: libc::c_ulong = 0x541E;
const TIOCSSERIAL: libc::c_ulong = 0x541F;

// libc 0.2.182 differentiates the ioctl request-parameter type by libc family:
//   musl  -> fn ioctl(fd: c_int, request: c_int,   ...)
//   glibc -> fn ioctl(fd: c_int, request: c_ulong, ...)
// Older libc versions papered over the difference, which is why earlier
// dcentrald builds split on target_pointer_width instead. Use target_env to
// match the right c_* type for whichever libc we're targeting. Both ABIs
// transmit the same wire bits — only the Rust type signature differs.
#[cfg(target_env = "musl")]
type IoctlRequest = libc::c_int;

#[cfg(not(target_env = "musl"))]
type IoctlRequest = libc::c_ulong;

fn ioctl_request(req: libc::c_ulong) -> IoctlRequest {
    req as IoctlRequest
}

/// `serial_struct.flags` bits for custom-divisor mode.
const ASYNC_SPD_CUST: i32 = 0x0030;
const ASYNC_SPD_MASK: i32 = 0x1030;

/// Linux termios2 structure for custom baud rate support.
///
/// This is the kernel's `struct termios2` from <asm/termbits.h>.
/// We define it manually because the nix crate's Termios doesn't expose
/// c_ispeed/c_ospeed fields needed for BOTHER.
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios2 {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 19],
    c_ispeed: u32,
    c_ospeed: u32,
}

/// Kernel `struct serial_struct` used by `TIOCGSERIAL/TIOCSSERIAL`.
///
/// This matches Linux `include/uapi/linux/serial.h` and is only used on the
/// Zynq PL ttyS devices where stock bosminer configures custom divisors off a
/// `B38400` base instead of using `termios2` / `BOTHER`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SerialStruct {
    type_: i32,
    line: i32,
    port: u32,
    irq: i32,
    flags: i32,
    xmit_fifo_size: i32,
    custom_divisor: i32,
    baud_base: i32,
    close_delay: u16,
    io_type: u8,
    reserved_char: u8,
    hub6: i32,
    closing_wait: u16,
    closing_wait2: u16,
    iomem_base: *mut u8,
    iomem_reg_shift: u16,
    port_high: u32,
    iomap_base: libc::c_ulong,
}

/// Linux serial port wrapper for ASIC chain communication.
pub struct SerialChain {
    /// Device path (e.g., /dev/ttyS1).
    path: String,
    /// Open file descriptor.
    file: File,
    /// Current baud rate.
    baud: u32,
}

impl SerialChain {
    fn open_file_with_stock_flags(path: &str) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| HalError::DeviceOpen {
                path: path.to_string(),
                source: e,
            })?;

        // SAFETY: `file` is a freshly opened tty fd owned by this process, and
        // TIOCEXCL takes no pointer argument; it only marks the tty exclusive.
        let ret = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                ioctl_request(libc::TIOCEXCL as libc::c_ulong),
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TIOCEXCL failed on {}: {}",
                path,
                std::io::Error::last_os_error(),
            )));
        }

        Ok(file)
    }

    /// Open a serial port for ASIC communication.
    ///
    /// Configures the port for 8N1, no flow control, raw mode.
    /// Supports both standard baud rates (via termios) and custom rates
    /// like 1.5625 Mbaud and 3.125 Mbaud (via BOTHER ioctl).
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let file = Self::open_file_with_stock_flags(path)?;

        let mut chain = Self {
            path: path.to_string(),
            file,
            baud,
        };

        chain.configure_port(baud)?;
        tracing::info!(path, baud, "Serial chain opened");
        Ok(chain)
    }

    /// Open a serial port in passthrough mode — preserves existing baud rate.
    ///
    /// Only sets VMIN/VTIME for non-blocking reads. Does NOT change baud rate,
    /// line discipline, or flush buffers. Used when adopting an already-live
    /// ASIC/UART state that was configured before dcentrald attached.
    /// Open a serial port in passthrough mode — preserves baud rate.
    ///
    /// Uses TCGETS2/TCSETS2 to read the current config, modify ONLY VMIN/VTIME,
    /// and write it back. This preserves c_cflag and c_ispeed/c_ospeed exactly.
    pub fn open_passthrough(path: &str) -> Result<Self> {
        let file = Self::open_file_with_stock_flags(path)?;

        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();

        // Read current termios2 (preserves ALL settings including custom baud)
        // SAFETY: Termios2 is a C POD struct and the kernel fills every field
        // through TCGETS2 before we read it.
        let mut t2: Termios2 = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is the live tty fd from `file`; `&mut t2` is valid for
        // the duration of the ioctl and matches the kernel's termios2 layout.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCGETS2), &mut t2 as *mut Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCGETS2 failed on {}: {}",
                path,
                std::io::Error::last_os_error(),
            )));
        }

        let orig_baud = t2.c_ospeed;
        tracing::info!(
            path,
            baud = orig_baud,
            "Passthrough: read existing config (baud={})",
            orig_baud
        );

        // Modify ONLY VMIN and VTIME — everything else stays exactly as the
        // pre-existing runtime configured it.
        t2.c_cc[6] = 0; // VMIN = 0
        t2.c_cc[5] = 1; // VTIME = 1 (100ms timeout)

        // SAFETY: `fd` is live and `t2` remains immutable and valid while the
        // kernel copies the termios2 settings for TCSETS2.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCSETS2), &t2 as *const Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCSETS2 failed on {}: {}",
                path,
                std::io::Error::last_os_error(),
            )));
        }

        {
            use nix::sys::termios::{tcflush, FlushArg};
            tcflush(&file, FlushArg::TCIOFLUSH).ok();
        }

        tracing::info!(
            path,
            "Serial chain opened in passthrough mode (baud {} preserved)",
            orig_baud
        );

        Ok(Self {
            path: path.to_string(),
            file,
            baud: orig_baud,
        })
    }

    /// Configure serial port for raw 8N1 ASIC communication.
    ///
    /// Tries standard termios first for standard baud rates (works on Amlogic
    /// meson_uart). Falls back to BOTHER/termios2 for non-standard rates
    /// (Zynq NS16550A: 1,562,500 / 3,125,000) or when standard path fails
    /// (Zynq PL UARTs return EIO on tcgetattr).
    fn configure_port(&mut self, baud: u32) -> Result<()> {
        if Self::is_zynq_setserial_baud(self.path.as_str(), baud) {
            if self.current_termios2_uses_bother() {
                match self.configure_termios2(baud) {
                    Ok(()) => {
                        self.baud = baud;
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %self.path,
                            baud,
                            error = %e,
                            "Existing UART state used BOTHER but TCSETS2 failed; falling back to stock custom divisor"
                        );
                    }
                }
            }
            self.configure_zynq_custom_divisor(baud)?;
            self.baud = baud;
            return Ok(());
        }

        if Self::is_standard_baud(baud) {
            // Try standard termios first (Amlogic, BeagleBone, etc.)
            match self.configure_termios(baud) {
                Ok(()) => {
                    self.baud = baud;
                    return Ok(());
                }
                Err(e) => {
                    // Zynq PL UARTs fail here with EIO — fall through to BOTHER
                    tracing::debug!(baud, error = %e,
                        "Standard termios failed, falling back to BOTHER/termios2");
                }
            }
        }
        // Non-standard rate (1,562,500 / 3,125,000) or standard path failed
        self.configure_termios2(baud)?;
        self.baud = baud;
        Ok(())
    }

    fn is_zynq_setserial_baud(path: &str, baud: u32) -> bool {
        matches!(
            path,
            "/dev/ttyS1" | "/dev/ttyS2" | "/dev/ttyS3" | "/dev/ttyS4"
        ) && matches!(baud, BAUD_1562500 | BAUD_3125000)
    }

    fn current_termios2_uses_bother(&self) -> bool {
        let fd = self.file.as_raw_fd();
        // SAFETY: Termios2 is a C POD struct and TCGETS2 initializes it before
        // its fields are inspected below.
        let mut t2: Termios2 = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is the live tty fd owned by `self`; `&mut t2` is a valid
        // output buffer with the kernel termios2 layout.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCGETS2), &mut t2 as *mut Termios2) };
        ret == 0 && (t2.c_cflag & BOTHER) == BOTHER
    }

    /// Check if a baud rate is natively supported by the standard termios API.
    fn is_standard_baud(baud: u32) -> bool {
        matches!(
            baud,
            50 | 75
                | 110
                | 134
                | 150
                | 200
                | 300
                | 600
                | 1200
                | 1800
                | 2400
                | 4800
                | 9600
                | 19200
                | 38400
                | 57600
                | 115200
                | 230400
                | 460800
                | 500000
                | 576000
                | 921600
                | 1000000
                | 1152000
                | 1500000
                | 2000000
                | 2500000
                | 3000000
                | 3500000
                | 4000000
        )
    }

    /// Assert DTR+RTS+OUT2 via `TIOCMBIS` on the kernel-owned fd (BLK-1b / DCENT_FPGA F2).
    ///
    /// Goes through the 8250 driver's `set_mctrl`, updating the cached `port->mctrl`
    /// shadow so OUT2 SURVIVES later termios ops — the "purest kernel-side-effect" form of
    /// asserting `a lab unit`'s FPGA UART TX-clock gate (Team M R-13; RE-1 only refuted the
    /// *bosminer-replicates-via-userspace-poke* model, not the kernel route). Best-effort:
    /// some 8250 builds mask OUT2 out of `TIOCMSET`, which is exactly why the caller also
    /// does the `/dev/mem` poke + readback to OBSERVE the actual MCR. Self-gated to
    /// `a lab unit`-fingerprint / `DCENT_AM2_MCR_OUT2=1`; a no-op on every other unit.
    pub(crate) fn set_modem_dtr_rts_out2(&self) -> std::io::Result<()> {
        if matches!(am2_mcr_out2_mode(&self.path), Am2McrOut2Mode::Baseline) {
            return Ok(());
        }
        // Arch-generic Linux values (identical on ARM); defined locally so this does
        // not depend on the musl libc crate exposing TIOCM_OUT2.
        const TIOCM_DTR: libc::c_int = 0x002;
        const TIOCM_RTS: libc::c_int = 0x004;
        const TIOCM_OUT2: libc::c_int = 0x2000;
        const TIOCMBIS: libc::c_ulong = 0x5416;
        let bits: libc::c_int = TIOCM_DTR | TIOCM_RTS | TIOCM_OUT2;
        // SAFETY: standard TIOCMBIS ioctl — set the modem-control bits named by `bits`
        // on our own open fd; `&bits` is a valid pointer to a c_int for the call's duration.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                ioctl_request(TIOCMBIS),
                &bits as *const libc::c_int,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Configure serial port via standard termios (for standard baud rates).
    fn configure_termios(&mut self, baud: u32) -> Result<()> {
        use nix::sys::termios::*;

        let mut termios = tcgetattr(&self.file)
            .map_err(|e| HalError::Platform(format!("tcgetattr {} failed: {}", self.path, e)))?;

        // Raw mode: no echo, no signals, no canonical processing
        cfmakeraw(&mut termios);

        // 8N1, no flow control
        termios.control_flags &=
            !(ControlFlags::CSIZE | ControlFlags::PARENB | ControlFlags::CSTOPB);
        termios.control_flags |= ControlFlags::CS8 | ControlFlags::CLOCAL | ControlFlags::CREAD;
        termios.control_flags &= !ControlFlags::CRTSCTS;

        // Set baud rate
        let speed = match baud {
            // Low-rate arms (BLK-1a, 2026-06-10). Without these, set_baud(9600) — the
            // BM1362 RE-018 Phase-A0 cold port-wake (B9600 -> B115200 divisor-latch reload that
            // re-arms the chip RX clock-recovery) — fell through to the `_` fallback below and
            // logged "Non-standard baud rate, using 115200 as fallback", making the port-wake a
            // logged NO-OP on the kernel `of_serial` transport (live: LIVE_TEST_13..18 all WARN).
            // bosminer holds a real ~1.16 s B9600 dwell before the chain answers. Fleet-safe:
            // B9600/19200/38400/57600 are only ever requested by the `a lab unit` RE-018 path; every
            // proven mining baud (115200+) is unchanged.
            9600 => BaudRate::B9600,
            19200 => BaudRate::B19200,
            38400 => BaudRate::B38400,
            57600 => BaudRate::B57600,
            115200 => BaudRate::B115200,
            230400 => BaudRate::B230400,
            460800 => BaudRate::B460800,
            500000 => BaudRate::B500000,
            576000 => BaudRate::B576000,
            921600 => BaudRate::B921600,
            1000000 => BaudRate::B1000000,
            1152000 => BaudRate::B1152000,
            1500000 => BaudRate::B1500000,
            2000000 => BaudRate::B2000000,
            2500000 => BaudRate::B2500000,
            3000000 => BaudRate::B3000000,
            3500000 => BaudRate::B3500000,
            4000000 => BaudRate::B4000000,
            _ => {
                tracing::warn!(baud, "Non-standard baud rate, using 115200 as fallback");
                BaudRate::B115200
            }
        };
        cfsetispeed(&mut termios, speed)
            .map_err(|e| HalError::Platform(format!("cfsetispeed failed: {}", e)))?;
        cfsetospeed(&mut termios, speed)
            .map_err(|e| HalError::Platform(format!("cfsetospeed failed: {}", e)))?;

        // VMIN=0, VTIME=1: non-blocking read with 100ms timeout.
        // Returns immediately if data is available, times out after 100ms if not.
        termios.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
        termios.control_chars[SpecialCharacterIndices::VTIME as usize] = 1; // 100ms

        tcsetattr(&self.file, SetArg::TCSADRAIN, &termios)
            .map_err(|e| HalError::Platform(format!("tcsetattr {} failed: {}", self.path, e)))?;

        // Flush stale RX only. TX was drained by TCSADRAIN; dropping queued TX
        // during a BM13xx baud handoff can silently lose the FastUART command.
        tcflush(&self.file, FlushArg::TCIFLUSH).ok();

        Ok(())
    }

    /// Configure a Zynq PL UART via stock-style setserial custom divisor.
    fn configure_zynq_custom_divisor(&mut self, baud: u32) -> Result<()> {
        use nix::sys::termios::*;

        let mut termios = tcgetattr(&self.file)
            .map_err(|e| HalError::Platform(format!("tcgetattr {} failed: {}", self.path, e)))?;

        cfmakeraw(&mut termios);
        termios.control_flags &=
            !(ControlFlags::CSIZE | ControlFlags::PARENB | ControlFlags::CSTOPB);
        termios.control_flags |= ControlFlags::CS8 | ControlFlags::CLOCAL | ControlFlags::CREAD;
        termios.control_flags &= !ControlFlags::CRTSCTS;

        cfsetispeed(&mut termios, BaudRate::B38400)
            .map_err(|e| HalError::Platform(format!("cfsetispeed failed: {}", e)))?;
        cfsetospeed(&mut termios, BaudRate::B38400)
            .map_err(|e| HalError::Platform(format!("cfsetospeed failed: {}", e)))?;

        termios.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
        termios.control_chars[SpecialCharacterIndices::VTIME as usize] = 1;

        tcsetattr(&self.file, SetArg::TCSADRAIN, &termios)
            .map_err(|e| HalError::Platform(format!("tcsetattr {} failed: {}", self.path, e)))?;

        let fd = self.file.as_raw_fd();
        // SAFETY: SerialStruct mirrors Linux `struct serial_struct`; the kernel
        // initializes it through TIOCGSERIAL before any field is read.
        let mut serial: SerialStruct = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is the live tty fd and `serial` is a valid writable
        // kernel-layout buffer for the duration of TIOCGSERIAL.
        let ret = unsafe {
            libc::ioctl(
                fd,
                ioctl_request(TIOCGSERIAL),
                &mut serial as *mut SerialStruct,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TIOCGSERIAL failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }

        let custom_divisor = match baud {
            BAUD_3125000 => 2,
            BAUD_1562500 => 4,
            _ => {
                return Err(HalError::Platform(format!(
                    "Unsupported custom divisor baud {} on {}",
                    baud, self.path,
                )))
            }
        };

        serial.flags = (serial.flags & !ASYNC_SPD_MASK) | ASYNC_SPD_CUST;
        serial.custom_divisor = custom_divisor;

        // SAFETY: `fd` is live and `serial` contains the TIOCGSERIAL-populated
        // struct with only documented custom-divisor fields changed.
        let ret = unsafe {
            libc::ioctl(
                fd,
                ioctl_request(TIOCSSERIAL),
                &serial as *const SerialStruct,
            )
        };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TIOCSSERIAL failed on {} for baud {}: {}",
                self.path,
                baud,
                std::io::Error::last_os_error(),
            )));
        }

        tcflush(&self.file, FlushArg::TCIFLUSH).ok();

        tracing::debug!(
            path = %self.path,
            baud,
            divisor = custom_divisor,
            baud_base = serial.baud_base,
            "Configured custom baud via TIOCSSERIAL"
        );

        Ok(())
    }

    /// Configure serial port via BOTHER/termios2 for custom baud rates.
    ///
    /// This handles non-standard rates like 1,562,500 and 3,125,000 baud
    /// that the NS16550A PL UARTs on Zynq S19 support but the standard
    /// termios BaudRate enum does not include.
    ///
    /// The BOTHER mechanism sets c_cflag's baud bits to BOTHER and puts
    /// the exact desired baud rate in c_ispeed and c_ospeed fields of
    /// the kernel's termios2 structure.
    fn configure_termios2(&mut self, baud: u32) -> Result<()> {
        let fd = self.file.as_raw_fd();

        // First, get the current termios2 settings
        // SAFETY: Termios2 is a C POD struct and TCGETS2 initializes it before
        // any field is used.
        let mut t2: Termios2 = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is the live tty fd and `&mut t2` is a valid termios2
        // output buffer for the kernel.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCGETS2), &mut t2 as *mut Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCGETS2 ioctl failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }

        // Raw mode flags (equivalent to cfmakeraw)
        t2.c_iflag &= !(0o000001 | 0o000002 | 0o000010 | 0o000040 |   // IGNBRK|BRKINT|PARMRK|ISTRIP
                         0o000100 | 0o000200 | 0o000400 | 0o002000); // INLCR|IGNCR|ICRNL|IXON
        t2.c_oflag &= !0o000001; // ~OPOST
        t2.c_lflag &= !(0o000010 | 0o000100 | 0o000002 | 0o100000 | 0o000001); // ~(ECHO|ECHONL|ICANON|ISIG|IEXTEN)
                                                                               // 8N1, no flow control
        t2.c_cflag &= !(0o000060 | 0o000400 | 0o000100); // ~(CSIZE|PARENB|CSTOPB)
        t2.c_cflag |= 0o000060 | 0o004000 | 0o000200; // CS8|CLOCAL|CREAD
        t2.c_cflag &= !0o020000000000; // ~CRTSCTS

        // Set BOTHER for custom baud rate
        t2.c_cflag &= !CBAUD;
        t2.c_cflag |= BOTHER;
        t2.c_ispeed = baud;
        t2.c_ospeed = baud;

        // VMIN=0, VTIME=1 (100ms timeout)
        t2.c_cc[6] = 0; // VMIN
        t2.c_cc[5] = 1; // VTIME

        // SAFETY: `fd` is live and `t2` remains a valid termios2 input buffer
        // while TCSETS2 copies it into the tty driver.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCSETS2), &t2 as *const Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCSETS2 ioctl failed on {} for baud {}: {}",
                self.path,
                baud,
                std::io::Error::last_os_error(),
            )));
        }

        // Verify the baud rate was set correctly
        // SAFETY: Termios2 is a C POD struct and TCGETS2 initializes it before
        // the verification read.
        let mut verify: Termios2 = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is live and `&mut verify` is a valid termios2 output
        // buffer for the kernel.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCGETS2), &mut verify as *mut Termios2) };
        if ret == 0 && verify.c_ospeed != baud {
            tracing::warn!(
                requested = baud,
                actual = verify.c_ospeed,
                path = %self.path,
                "Custom baud rate mismatch — kernel rounded to nearest supported value"
            );
        }

        // Flush stale RX only. Callers that are changing baud after a command
        // write must drain TX before TCSETS2; do not discard TX here.
        use nix::sys::termios::*;
        tcflush(&self.file, FlushArg::TCIFLUSH).ok();

        tracing::debug!(
            path = %self.path,
            baud,
            "Configured custom baud rate via BOTHER/termios2"
        );

        Ok(())
    }

    /// Set baud rate (auto-selects standard termios or BOTHER).
    pub fn set_baud(&mut self, baud: u32) -> Result<()> {
        self.configure_port(baud)
    }

    /// Write bytes to the serial port (command or work).
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data).map_err(|e| {
            HalError::Platform(format!("serial write to {} failed: {}", self.path, e))
        })?;
        Ok(())
    }

    /// Read available bytes from serial port (non-blocking with VTIME timeout).
    pub fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.file.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(HalError::Io(e)),
        }
    }

    /// Set the serial port to non-blocking mode (O_NONBLOCK).
    pub fn set_nonblocking(&mut self) -> Result<()> {
        use std::os::unix::io::AsRawFd;
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` is a live tty fd and F_GETFL takes no pointer argument.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(HalError::Platform(format!(
                "fcntl F_GETFL failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }
        // SAFETY: `fd` is live and F_SETFL copies only the integer flags value.
        let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "fcntl F_SETFL O_NONBLOCK failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }
        Ok(())
    }

    /// Read exactly `len` bytes from the serial port with a timeout.
    ///
    /// Retries reads until either `len` bytes are collected or `timeout_ms`
    /// elapses. Returns the number of bytes actually read (may be < len on
    /// timeout).
    pub fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        len: usize,
        timeout_ms: u64,
    ) -> Result<usize> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        let mut pos = 0;

        while pos < len && std::time::Instant::now() < deadline {
            let n = self.file.read(&mut buf[pos..len]).map_err(HalError::Io)?;
            if n > 0 {
                pos += n;
            }
            // VTIME handles the per-read timeout; we just loop until deadline
        }

        Ok(pos)
    }

    /// Flush the transmit buffer (block until all bytes are sent).
    pub fn flush(&mut self) -> Result<()> {
        use nix::sys::termios::*;
        tcdrain(&self.file)
            .map_err(|e| HalError::Platform(format!("tcdrain {} failed: {}", self.path, e)))?;
        Ok(())
    }

    /// Flush both RX and TX buffers (discard all pending data).
    pub fn flush_io(&mut self) -> Result<()> {
        use nix::sys::termios::*;
        tcflush(&self.file, FlushArg::TCIOFLUSH)
            .map_err(|e| HalError::Platform(format!("tcflush {} failed: {}", self.path, e)))?;
        Ok(())
    }

    /// Set VMIN and VTIME for the serial port.
    /// VMIN=0, VTIME=0 = fully non-blocking (return immediately).
    /// VMIN=0, VTIME=N = return after N*100ms timeout or when data available.
    pub fn set_vtime(&mut self, vtime: u8) -> Result<()> {
        let fd = self.file.as_raw_fd();
        // SAFETY: Termios2 is a C POD struct and TCGETS2 initializes it before
        // the VMIN/VTIME fields are modified.
        let mut t2: Termios2 = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is live and `&mut t2` is a valid termios2 output buffer.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCGETS2), &mut t2 as *mut Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCGETS2 failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }
        t2.c_cc[6] = 0; // VMIN = 0
        t2.c_cc[5] = vtime; // VTIME

        // SAFETY: `fd` is live and `t2` is a valid termios2 input buffer while
        // TCSETS2 copies the updated timeout settings.
        let ret = unsafe { libc::ioctl(fd, ioctl_request(TCSETS2), &t2 as *const Termios2) };
        if ret < 0 {
            return Err(HalError::Platform(format!(
                "TCSETS2 failed on {}: {}",
                self.path,
                std::io::Error::last_os_error(),
            )));
        }
        Ok(())
    }

    /// Get the device path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Get the current baud rate.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// Get the raw file descriptor (for epoll/select).
    pub fn raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

// ===========================================================================
// DevmemUart — Direct /dev/mem UART access bypassing the kernel serial driver.
//
// On S19j Pro Zynq, the kernel serial driver (of_serial/8250) fails because
// IRQ 168 is shared with the UIO chain2-work-tx device. Bosminer works around
// this by... being the first to open the port after boot. After reboot with
// bosminer in PAUSED mode (no UART init), the driver is permanently broken.
//
// Solution: bypass the kernel driver entirely. Use /dev/mem to mmap the
// NS16550A registers and do polled I/O. Same pattern as our I2C devmem bypass.
//
// NS16550A PL UARTs on S19j Pro:
//   ttyS1 = 0x41001000, ttyS2 = 0x41011000, ttyS3 = 0x41021000, ttyS4 = 0x41031000
//   base_baud = 6,249,999 Hz (100 MHz PL clock / 16)
//   Divisor 2 → 3,125,000 baud, Divisor 54 → ~115,740 baud (close to 115200)
// ===========================================================================

/// NS16550A register offsets (4-byte aligned on Zynq PL and CV1835).
const UART_RBR_THR: usize = 0x00; // RBR (read) / THR (write) / DLL (DLAB=1)
const UART_IER_DLM: usize = 0x04; // IER (DLAB=0) / DLM (DLAB=1)
const UART_FCR: usize = 0x08; // FCR (write only)
const UART_LCR: usize = 0x0C; // Line Control Register
/// Modem Control Register (4-byte aligned on Zynq PL and CV1835).
///
/// Standard 16550A offset 0x10. On the S19j Pro Zynq am2 ASIC chain UARTs the
/// RS-485 transceiver enable is wired to DTR+RTS — keeping MCR=0x00 leaves the
/// transceiver POWERED OFF, which manifests as UART RX=0 from the BM1362 chain
/// even when the FPGA + ASIC ENABLE rail are healthy. Setting MCR bits 0+1
/// (DTR=1, RTS=1) is what stock bosminer does implicitly via tty open.
/// XXX: R4-CONFIRMED — uart_relay_blocker3_5_analysis.md (root cause #1 of
/// chain UART RX=0 on cold boot per -4 / R4 synthesis).
const UART_MCR: usize = 0x10; // Modem Control Register
const UART_LSR: usize = 0x14; // Line Status Register
/// DesignWare 16550A Divisor Latch Fractional register (CV1835 only).
///
/// Source: DesignWare DW_apb_uart Databook, Synopsys IP. CV1835 ships the
/// DesignWare core; the Zynq Xilinx 16550 PL UART does NOT have this register
/// (writes to 0xC0 on Zynq go into reserved space — harmless but a no-op).
/// The table-driven `has_dlf` flag gates DLF writes so Zynq is left untouched.
const UART_DLF: usize = 0xC0; // Divisor Latch Fractional (DesignWare 16550A)

/// LSR bit masks.
const LSR_DR: u32 = 0x01; // Data Ready (RX has data)
const LSR_THRE: u32 = 0x20; // TX Holding Register Empty
const LSR_TEMT: u32 = 0x40; // TX shift register empty
const LSR_TX_IDLE: u32 = LSR_THRE | LSR_TEMT;

/// LCR bit masks.
const LCR_DLAB: u32 = 0x80; // Divisor Latch Access Bit
const LCR_8N1: u32 = 0x03; // 8 data bits, no parity, 1 stop bit

/// FCR: enable FIFOs, reset TX and RX FIFOs.
///
/// Bits: ENABLE_FIFO (0x01) | RX_RESET (0x02) | TX_RESET (0x04) = 0x07.
/// XXX: R4-CONFIRMED — uart_relay_blocker3_5_analysis.md (root cause #2 of
/// chain UART RX=0 on cold boot — 1-byte non-FIFO mode silently drops bytes).
const FCR_ENABLE_RESET: u32 = 0x07;

/// MCR: assert DTR (bit 0) + RTS (bit 1) — powers the RS-485 transceiver
/// on the S19j Pro Zynq am2 ASIC chain UARTs. Without this, the transceiver
/// stays disabled and chain RX is silent regardless of FPGA / ASIC state.
/// XXX: R4-CONFIRMED — uart_relay_blocker3_5_analysis.md (root cause #1).
const MCR_DTR_RTS: u32 = 0x03;

/// IER value matching bosminer's kernel of_serial driver state on `a lab unit`:
/// `ERBFI` (bit 0, RX-data-ready IRQ enable) + `ELSI` (bit 2, line-status
/// IRQ enable) = `0x05`. Used when `DCENT_AM2_IER_BOSMINER_PARITY=1` is
/// set. Per the 2026-05-23 bosminer strace on `a lab unit`: kernel of_serial
/// driver writes IER=0x05 at port-open time. On some FPGA UART block
/// designs the IER bits gate the TX clock-out indirectly; setting them
/// (even if DCENT_OS runs polled) may be required for the FPGA UART
/// state machine to actually clock bytes onto the wire.
const IER_BOSMINER_PARITY: u32 = 0x05;

/// LCR matching bosminer's kernel of_serial state: `0x13`. Decodes to
/// 8-bit word + 1 stop + parity-disable + even-parity-flag (ignored since
/// parity disabled). Our `LCR_8N1 = 0x03` is functionally identical (8N1)
/// but bit-different. Set when `DCENT_AM2_IER_BOSMINER_PARITY=1`.
const LCR_BOSMINER_PARITY: u32 = 0x13;

/// Read `DCENT_AM2_IER_BOSMINER_PARITY` — when set, DevmemUart writes
/// `IER = 0x05` + `LCR = 0x13` (matching bosminer's kernel of_serial state
/// captured live on `a lab unit`). Default OFF preserves `a lab unit`'s baseline
/// (IER=0x00 polled).
fn am2_ier_bosminer_parity_enabled() -> bool {
    std::env::var("DCENT_AM2_IER_BOSMINER_PARITY")
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// IER baseline for polled-I/O DevmemUart on AM2 Zynq: all interrupts
/// disabled. DCENT_OS DevmemUart polls TX/RX state via LSR; the kernel
/// of_serial driver is not used (different code path). Setting any IER
/// bit on a polled UART risks the FPGA UART block raising spurious GIC
/// IRQs that the kernel doesn't have a handler for.
///
/// 7 tried `IER = 0x05` (ERBFI + ELSI, BOS byte-parity) and it remains
/// env-gated for byte-parity diagnostics. The current `a lab unit` command/init
/// canon is PL UART with MCR OUT2, not FPGA FIFO command transport.
const IER_POLLED_MODE: u32 = 0x00;

/// 6 / xil-`a lab unit` chain-enum-0 fix candidate.
///
/// Live BOS-vs-DCENT_OS UART register snapshot on `a lab unit` (am2 Zynq, PL UART
/// at 0x41001000) showed BraiinsOS sets `MCR = 0x0B = DTR + RTS + OUT2`
/// during active mining, while DCENT_OS's DevmemUart was writing only
/// `MCR = 0x03 = DTR + RTS` (no OUT2). The -4 / R4 fix in 2026-04
/// added DTR+RTS for the RS-485 transceiver but missed the OUT2 bit which
/// on this hashboard's external transceiver appears to act as the
/// transmit-enable / output-enable line. Without OUT2, chain UART writes
/// happen at the FPGA UART register layer but the bytes never make it
/// onto the chain wire — chips never see GetAddress, RX stays silent,
/// chain enum reads 0/126. Adding OUT2 matches BOS byte-for-byte.
///
/// Standard 16550A MCR bits:
///   bit 0: DTR
///   bit 1: RTS
///   bit 2: OUT1 (unused on most embedded UARTs)
///   bit 3: OUT2 (often wired to RS-485 transceiver TX-enable / IRQ-gate)
///
/// Capture evidence: BOS readback `MCR @ 0x41001010 = 0x0000000B` during
/// active bosminer-plus-tuner 0.9.0 mining; DCENT_OS readback after
/// failed daemon was `0x00000000` (kernel default). DCENT_OS's MCR=0x03
/// write was never observed because the daemon bailed before
/// `init_asic_chain` opened the chain UART, but the source intent was
/// `MCR_DTR_RTS = 0x03`.
//  (2026-05-23): live-evidence-driven, used when
// `DCENT_AM2_MCR_OUT2=1` (env gate, default OFF). On `a lab unit`'s bitstream
// the OUT2 bit gates the FPGA UART block's TX clock-out. Without OUT2
// asserted, the TX FIFO accepts bytes but the FPGA UART holds them in
// the shift register indefinitely → chain-enum-0. See
// .
const MCR_DTR_RTS_OUT2: u32 = 0x0B;

const AM2_MCR_OUT2_ENV: &str = "DCENT_AM2_MCR_OUT2";
const DCENT_PLATFORM_FILE: &str = "/etc/dcentos/platform";
const BOS_PLATFORM_FILE: &str = "/etc/bos_platform";
const DCENT_BOARD_TARGET_FILE: &str = "/etc/dcentos/board_target";
const ZYNQ_BM3_AM2_PLATFORM: &str = "zynq-bm3-am2";
const XIL_25_BOARD_TARGET_SUFFIX: &str = "xil";
const XIL_25_FINGERPRINT_OVERRIDE_ENV: &str = "DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE";
// D6-4 (2026-06-13): psu_hardware_variant file — used to keep the serial MCR-OUT2
// fingerprint gate consistent with s19j_hybrid_mining::am2_xil_25_fingerprint_matches,
// which refuses to fire for a declared non-"loki" PSU variant.
const DCENT_PSU_HW_VARIANT_FILE: &str = "/etc/dcentos/psu_hardware_variant";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Am2McrOut2Mode {
    Baseline,
    EnvOverride,
    Xil25Fingerprint,
}

fn am2_env_value_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
}

fn is_zynq_am2_pl_uart_path(path: &str) -> bool {
    matches!(
        path,
        "/dev/ttyS1" | "/dev/ttyS2" | "/dev/ttyS3" | "/dev/ttyS4"
    )
}

fn am2_xil_25_fingerprint_matches(
    platform: Option<&str>,
    board_target: Option<&str>,
    fingerprint_override: Option<&str>,
) -> bool {
    if platform.map(str::trim) != Some(ZYNQ_BM3_AM2_PLATFORM) {
        return false;
    }

    let board_target = board_target.map(str::trim).unwrap_or_default();
    if board_target.ends_with(XIL_25_BOARD_TARGET_SUFFIX) {
        return true;
    }

    board_target == "am2-s19j"
        && fingerprint_override
            .map(am2_env_value_truthy)
            .unwrap_or(false)
}

fn am2_mcr_out2_mode_for_inputs(
    path: &str,
    env_value: Option<&str>,
    platform: Option<&str>,
    board_target: Option<&str>,
    fingerprint_override: Option<&str>,
) -> Am2McrOut2Mode {
    if let Some(value) = env_value {
        return if am2_env_value_truthy(value) {
            Am2McrOut2Mode::EnvOverride
        } else {
            Am2McrOut2Mode::Baseline
        };
    }

    if is_zynq_am2_pl_uart_path(path)
        && am2_xil_25_fingerprint_matches(platform, board_target, fingerprint_override)
    {
        return Am2McrOut2Mode::Xil25Fingerprint;
    }

    Am2McrOut2Mode::Baseline
}

fn read_first_trimmed(paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Decide between MCR = 0x03 (DTR+RTS only) and MCR = 0x0B
/// (DTR+RTS+OUT2).
///
/// `DCENT_AM2_MCR_OUT2=1` remains the explicit lab/operator override.
/// In production, `a lab unit`-class XIL units can now self-select OUT2 from the
/// canonical board fingerprint (`/etc/dcentos/platform = zynq-bm3-am2` and
/// `/etc/dcentos/board_target` ending in `xil`). The  `a lab unit`
/// package-identity bridge also honors
/// `DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE=1`, but only when the package
/// board_target is exactly `am2-s19j`. Non-`a lab unit` AM2 units keep the R4
/// baseline unless the operator sets the explicit MCR env override.
fn am2_mcr_out2_mode(path: &str) -> Am2McrOut2Mode {
    let env_value = match std::env::var(AM2_MCR_OUT2_ENV) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Am2McrOut2Mode::Baseline,
    };
    let fingerprint_override = match std::env::var(XIL_25_FINGERPRINT_OVERRIDE_ENV) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Am2McrOut2Mode::Baseline,
    };
    let platform = read_first_trimmed(&[DCENT_PLATFORM_FILE, BOS_PLATFORM_FILE]);
    let board_target = read_first_trimmed(&[DCENT_BOARD_TARGET_FILE]);

    let mode = am2_mcr_out2_mode_for_inputs(
        path,
        env_value.as_deref(),
        platform.as_deref(),
        board_target.as_deref(),
        fingerprint_override.as_deref(),
    );

    // D6-4 (2026-06-13): keep the MCR-OUT2 *fingerprint* gate consistent with
    // s19j_hybrid_mining::am2_xil_25_fingerprint_matches, which refuses to fire on a
    // declared non-"loki" psu_hardware_variant. Without this, a `a lab unit`-class board
    // declared with a non-Loki PSU variant would get OUT2 from THIS gate while the
    // RE-018 enable path (which keys off the s19j predicate) would NOT — the two
    // halves of the OUT2 story disagreeing. The explicit DCENT_AM2_MCR_OUT2 env
    // override (EnvOverride) stays authoritative; only the fingerprint-derived mode
    // is downgraded. `read_first_trimmed` returns None for an empty/absent file, so
    // unset/empty variant keeps OUT2 (matches the s19j Some("")|None => true rule).
    if mode == Am2McrOut2Mode::Xil25Fingerprint {
        if let Some(variant) = read_first_trimmed(&[DCENT_PSU_HW_VARIANT_FILE]) {
            if !variant.eq_ignore_ascii_case("loki") {
                return Am2McrOut2Mode::Baseline;
            }
        }
    }

    mode
}

/// UART base_baud (PL clock / 16) for legacy compile-time references.
///
/// Prefer `UartMmioEntry::base_baud` from the active platform table — this
/// constant is kept only for backwards compat with any module-private reads.
#[allow(dead_code)]
const UART_BASE_BAUD: u32 = 6_249_999;

/// Typed UART MMIO table entry.
///
/// Allows per-platform UART block descriptions (Zynq Xilinx 16550 vs CV1835
/// DesignWare 16550A) without duplicating divisor math. The `has_dlf` flag is
/// the safety gate that keeps the Zynq path bit-identical: if `has_dlf` is
/// false (Zynq), `set_baud` never touches the DLF register.
#[derive(Debug, Clone, Copy)]
struct UartMmioEntry {
    /// /dev path (e.g. "/dev/ttyS1").
    path: &'static str,
    /// Physical base address of the UART register block.
    base_addr: usize,
    /// XTAL / 16 — used as numerator for divisor calculation.
    /// Zynq PL: 100 MHz / 16 = 6_249_999 (rounded down by Xilinx clock div).
    /// CV1835:  25 MHz  / 16 = 1_562_500 exact.
    base_baud: u32,
    /// mmap page size (page-granular). 0x1000 typical on both ARMv7 and aarch64.
    page_size: usize,
    /// True when the UART block exposes a DesignWare-style Divisor Latch
    /// Fractional register at offset 0xC0. CV1835 = true. Zynq = false.
    has_dlf: bool,
}

/// Zynq am2 / am1 UART MMIO map (Xilinx 16550 PL UART, no DLF support).
///
/// Live-confirmed addresses on .139 / .91 / .106 (S19j Pro Zynq, S9 BraiinsOS).
/// base_baud = 6_249_999 ≈ 100 MHz PL clock / 16 (Xilinx clock divider rounds
/// to floor; the exact 6_250_000 is unreachable but 6_249_999 matches what the
/// stock kernel reports via TIOCGSERIAL).
// `static` (not `const`): `UartPlatform::of_table` identifies the active table
// by `std::ptr::eq` on `.as_ptr()`. A `const` slice is substituted (and its
// promoted backing array may be duplicated) at each use site, so two reads of
// the same `const` map can yield different data pointers → ptr::eq spuriously
// false. A `static` is a single fixed address, making the identity check sound.
static UART_MMIO_MAP_ZYNQ: &[UartMmioEntry] = &[
    UartMmioEntry {
        path: "/dev/ttyS1",
        base_addr: 0x4100_1000,
        base_baud: 6_249_999,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS2",
        base_addr: 0x4101_1000,
        base_baud: 6_249_999,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS3",
        base_addr: 0x4102_1000,
        base_baud: 6_249_999,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS4",
        base_addr: 0x4103_1000,
        base_baud: 6_249_999,
        page_size: 0x1000,
        has_dlf: false,
    },
];

/// Cvitek CV1835 UART MMIO map (DesignWare 16550A, fractional baud support).
///
/// Source: DCENT_OS_DEVELOPMENT_KIT/SOURCE_HAL/devmem_uart.h and
/// DOCS/multi_platform_master.md "Cvitek CV1835 Control Board" section.
/// 4 KB stride per UART; 5 UART blocks (UART0 = console, UART1-4 = ASIC
/// chains 0-3 in the BHB42xxx S19j Pro CV variant). base_baud = 25 MHz / 16
/// = 1_562_500 — this is the dev-kit's documented refclk for the chain
/// UARTs. (NOTE: the dev-kit C reference uses 24 MHz in `baud_to_divisor`,
/// but the multi-platform doc + DesignWare DLF math here use the 25 MHz
/// figure that produces the 0xAB DLF for exact 937500 baud. Live verify
/// against an actual CV1835 unit will pin this down once a unit is on the
/// fleet — at that point either base_baud or the DLF expectation gets
/// corrected in lockstep.)
// `static` (not `const`) — see UART_MMIO_MAP_ZYNQ: required for the ptr::eq
// table-identity check in `UartPlatform::of_table` to be sound.
static UART_MMIO_MAP_CV1835: &[UartMmioEntry] = &[
    UartMmioEntry {
        path: "/dev/ttyS0",
        base_addr: 0x0500_C000,
        base_baud: 1_562_500,
        page_size: 0x1000,
        has_dlf: true,
    },
    UartMmioEntry {
        path: "/dev/ttyS1",
        base_addr: 0x0500_D000,
        base_baud: 1_562_500,
        page_size: 0x1000,
        has_dlf: true,
    },
    UartMmioEntry {
        path: "/dev/ttyS2",
        base_addr: 0x0500_E000,
        base_baud: 1_562_500,
        page_size: 0x1000,
        has_dlf: true,
    },
    UartMmioEntry {
        path: "/dev/ttyS3",
        base_addr: 0x0500_F000,
        base_baud: 1_562_500,
        page_size: 0x1000,
        has_dlf: true,
    },
    UartMmioEntry {
        path: "/dev/ttyS4",
        base_addr: 0x0501_0000,
        base_baud: 1_562_500,
        page_size: 0x1000,
        has_dlf: true,
    },
];

/// AM335x BeagleBone OMAP UART MMIO map (W14.A4 — R4-CONFIRMED; device
/// names corrected to `/dev/ttyS%d` per the live `a lab unit` probe 2026-05-12).
///
/// The AM335x has 6 OMAP UART blocks; the S19j Pro carrier wires 3-4 chain
/// hashboards onto UART1/UART2/UART4 (and UART5 on 4-board SKUs):
/// `/dev/ttyS1` (0x4802_2000), `/dev/ttyS2` (0x4802_4000), `/dev/ttyS4`
/// (0x481A_8000), `/dev/ttyS5` (0x481A_A000). UART0 is the console
/// (`/dev/ttyS0`); UART3 (`/dev/ttyS3`) is unpopulated in the BB DTS.
///
/// **Device-name history**: W4-era code (reconstructed from a different IO
/// board / stock-Bitmain BBCtrl) used `/dev/ttyO%d`. The live `a lab unit` LuxOS
/// probe (`S19J_IO_BOARD_V2_0`, kernel 5.4 omap-serial) showed the mainline
/// driver names the same UART blocks `/dev/ttyS%d` — `dmesg`:
/// `48022000.serial: ttyS1 ... base_baud=3000000`, etc. The MMIO base
/// addresses are unchanged. `DevmemUart` mmaps the base address so the name
/// is cosmetic for that path; a kernel-tty path (`SerialChain`) needs the
/// right name..
///
/// Source: AM335x TRM register-map appendix + the `a lab unit` dmesg + W4 RE
/// `am335x_board_init.c::am335x_uart_init`. Clock comes from the AM335x DTS
/// `clock-frequency = 48000000` (48 MHz functional clock) →
/// `base_baud = 48_000_000 / 16 = 3_000_000`.
///
/// `has_dlf = false` because OMAP UART (= TI OMAP serial driver, ABI
/// 16550-compatible) does NOT have a DesignWare-style fractional-divisor
/// register. Setting DLF on AM335x would write into a reserved region.
/// The W14.A4 audit explicitly pins this to prevent a future
/// "consolidation" copy-paste from CV1835.
///
/// **Runtime preference**: `SerialChain` (kernel termios via OMAP serial
/// driver) is the DCENT_OS default on AM335x. `DevmemUart` against this
/// table is reachable only when env-gated by
/// `DCENT_AM3_BB_USE_DEVMEM_UART=1` (lab/debug). The table exists so
/// `set_baud()` resolves to a real address instead of returning the W13
/// "path not in MMIO map" runtime error when a future bench operator
/// flips that env-gate..
// `static` (not `const`) — see UART_MMIO_MAP_ZYNQ: required for the ptr::eq
// table-identity check in `UartPlatform::of_table` to be sound.
static UART_MMIO_MAP_AM335X: &[UartMmioEntry] = &[
    UartMmioEntry {
        path: "/dev/ttyS1",
        base_addr: 0x4802_2000,
        base_baud: 3_000_000,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS2",
        base_addr: 0x4802_4000,
        base_baud: 3_000_000,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS4",
        base_addr: 0x481A_8000,
        base_baud: 3_000_000,
        page_size: 0x1000,
        has_dlf: false,
    },
    UartMmioEntry {
        path: "/dev/ttyS5",
        base_addr: 0x481A_A000,
        base_baud: 3_000_000,
        page_size: 0x1000,
        has_dlf: false,
    },
];

/// Active UART MMIO table selector.
///
/// Set once at platform init by `set_active_uart_table()` (called from the
/// platform constructors that share `/dev/ttyS1` between Zynq and CV1835).
/// Defaults to Zynq when unset — this keeps the historical S9/S19j-Zynq
/// behavior bit-identical for any caller that runs without explicit
/// platform-side opt-in.
static ACTIVE_UART_TABLE: std::sync::OnceLock<&'static [UartMmioEntry]> =
    std::sync::OnceLock::new();

/// UART platform identity (CE-008).
///
/// Which physical UART block layout this process is running against. Each
/// MMIO table belongs to exactly one of these. Recorded alongside the table
/// so `DevmemUart::open*` can refuse to mmap a table whose addresses don't
/// belong to the running platform — catching the SIGBUS-class bug where an
/// AM335x unit never called `select_uart_table_am335x()` and the Zynq default
/// would mmap `0x4100_1000` (unmapped on AM335x → SIGBUS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UartPlatform {
    Zynq,
    Cv1835,
    Am335x,
}

impl UartPlatform {
    /// The platform a given static table belongs to. Compared by pointer
    /// identity (all three tables are `'static` singletons).
    fn of_table(table: &'static [UartMmioEntry]) -> Option<UartPlatform> {
        if std::ptr::eq(table.as_ptr(), UART_MMIO_MAP_ZYNQ.as_ptr()) {
            Some(UartPlatform::Zynq)
        } else if std::ptr::eq(table.as_ptr(), UART_MMIO_MAP_CV1835.as_ptr()) {
            Some(UartPlatform::Cv1835)
        } else if std::ptr::eq(table.as_ptr(), UART_MMIO_MAP_AM335X.as_ptr()) {
            Some(UartPlatform::Am335x)
        } else {
            None
        }
    }
}

/// Declared running platform identity (CE-008).
///
/// Set by the platform constructor via `declare_uart_platform()` BEFORE any
/// `DevmemUart::open*`. Independent of `ACTIVE_UART_TABLE` so the open path can
/// cross-check: "the active table resolves this path to a Zynq address, but the
/// platform declared itself AM335x" → refuse (clean `Err`) instead of mmap'ing
/// an unmapped region and taking a SIGBUS.
///
/// Unset (the historical case) preserves byte-identical behaviour: with no
/// declared identity there is nothing to cross-check, so the open path proceeds
/// exactly as before.
static DECLARED_UART_PLATFORM: std::sync::OnceLock<UartPlatform> = std::sync::OnceLock::new();

/// Declare the running platform's UART identity (CE-008). Idempotent on repeat
/// calls with the same identity; errors on a conflicting re-declare.
fn declare_uart_platform(platform: UartPlatform) -> Result<()> {
    if let Some(existing) = DECLARED_UART_PLATFORM.get().copied() {
        if existing == platform {
            return Ok(());
        }
        return Err(HalError::Platform(format!(
            "UART platform identity already declared ({existing:?}); \
             refusing to change to {platform:?} mid-run"
        )));
    }
    let _ = DECLARED_UART_PLATFORM.set(platform);
    Ok(())
}

/// Explicitly select the CV1835 UART MMIO table.
///
/// Idempotent on repeat calls with the same table (OnceLock returns Err on
/// second `set` attempt; we ignore that as long as the active table matches).
/// Called by `platform::cvitek::CViTekPlatform::new()` once the CV1835 port
/// stands up.
#[allow(dead_code)] // wired by CV1835 platform impl in a follow-up wave (B1).
pub(crate) fn select_uart_table_cv1835() -> Result<()> {
    select_uart_table(UART_MMIO_MAP_CV1835)
}

/// Explicitly select the Zynq UART MMIO table.
///
/// Most callers don't need to call this — Zynq is the default. Provided for
/// symmetry with `select_uart_table_cv1835` and so `platform::zynq` can
/// assert the choice explicitly during cold boot.
#[allow(dead_code)]
pub(crate) fn select_uart_table_zynq() -> Result<()> {
    select_uart_table(UART_MMIO_MAP_ZYNQ)
}

/// Explicitly select the AM335x BB OMAP UART MMIO table (W14.A4; Phase B
/// 2026-05-12 device-name correction to `/dev/ttyS%d`).
///
/// **Call this once before any `DevmemUart::open(...)` on an AM335x BB unit.**
/// The active UART MMIO table is a process-wide `OnceLock` that defaults to
/// Zynq — without this call, `DevmemUart::open("/dev/ttyS1")` resolves to the
/// Zynq PL-UART base `0x4100_1000`, and mmap'ing `/dev/mem` there on AM335x
/// (an unmapped region) faults with SIGBUS. The `--am3-bb-mining` daemon mode
/// (`dcentrald::am3_bb_mining`) calls this before opening the chain UARTs. The
/// S19j Pro `S19J_IO_BOARD_V2_0` wires chains 0/1/2 onto `/dev/ttyS1` /
/// `/dev/ttyS2` / `/dev/ttyS4` → `0x4802_2000` / `0x4802_4000` / `0x481a_8000`
/// (live `a lab unit` probe)..
///
/// Idempotent on repeat calls with the same table.
pub fn select_uart_table_am335x() -> Result<()> {
    select_uart_table(UART_MMIO_MAP_AM335X)
}

/// CE-008: declare this process is running on an AM335x BeagleBone control
/// board, independently of selecting the UART table.
///
/// `select_uart_table_am335x()` already declares this identity as a side
/// effect, so most callers don't need this. It exists so a platform
/// constructor can stamp the identity EARLY (e.g. right after detecting the
/// AM335x SoC) — then if a later code path forgets to call
/// `select_uart_table_am335x()` and the active table is still the Zynq default,
/// `DevmemUart::open*` fails with a clear "table/platform mismatch" error
/// instead of mmap'ing `0x41001000` (unmapped on AM335x) and taking a SIGBUS.
/// Idempotent; errors only on a conflicting re-declare.
pub fn declare_uart_platform_am335x() -> Result<()> {
    declare_uart_platform(UartPlatform::Am335x)
}

/// CE-008: declare this process is running on a Zynq control board (am1/am2),
/// independently of selecting the UART table. Symmetry with the AM335x form;
/// Zynq is the default table, so this is rarely needed.
#[allow(dead_code)]
pub fn declare_uart_platform_zynq() -> Result<()> {
    declare_uart_platform(UartPlatform::Zynq)
}

/// CE-008: declare this process is running on a CV1835 control board,
/// independently of selecting the UART table.
#[allow(dead_code)]
pub fn declare_uart_platform_cv1835() -> Result<()> {
    declare_uart_platform(UartPlatform::Cv1835)
}

fn select_uart_table(table: &'static [UartMmioEntry]) -> Result<()> {
    // OnceLock::set returns Err(rejected_value) on second-set; we have to
    // peek at the currently-stored table via .get() to compare. ptr::eq on
    // the table reference is enough — both candidate tables are 'static
    // singletons.
    if let Some(existing) = ACTIVE_UART_TABLE.get().copied() {
        if std::ptr::eq(existing.as_ptr(), table.as_ptr()) {
            return Ok(());
        }
        return Err(HalError::Platform(format!(
            "UART MMIO table already selected ({} entries) — \
             refusing to change to a {}-entry table mid-run",
            existing.len(),
            table.len(),
        )));
    }
    // CE-008: cross-check the platform identity BEFORE committing the table.
    //
    // Selecting a known table always implies its platform. If a conflicting
    // platform was already pre-declared (e.g. the constructor stamped AM335x via
    // `declare_uart_platform_am335x()` but a later path tries to select the Zynq
    // table), we must REFUSE without committing the table — otherwise the table
    // OnceLock gets set, then `declare_uart_platform` returns `Err`, leaving an
    // inconsistent state (table committed to one platform while the declared
    // identity says another). Checking first keeps the two OnceLocks consistent:
    // on conflict, neither is mutated.
    //
    // An unknown table (should be impossible — all three are 'static singletons)
    // leaves the identity undeclared, preserving the historical no-declaration
    // behaviour byte-for-byte.
    select_table_identity_precheck(
        UartPlatform::of_table(table),
        DECLARED_UART_PLATFORM.get().copied(),
    )?;

    // First-set: ignore the unlikely race where another thread set it
    // between our `get` and `set`; on race-loss the next outer caller will
    // see the post-race state and proceed.
    let _ = ACTIVE_UART_TABLE.set(table);
    // Identity is now consistent with the just-committed table — declare it.
    // `declare_uart_platform` is idempotent for the matching identity (the only
    // conflicting case was rejected above before any mutation).
    if let Some(platform) = UartPlatform::of_table(table) {
        declare_uart_platform(platform)?;
    }
    Ok(())
}

/// Return the active UART MMIO table, defaulting to Zynq when unselected.
fn active_uart_table() -> &'static [UartMmioEntry] {
    ACTIVE_UART_TABLE
        .get()
        .copied()
        .unwrap_or(UART_MMIO_MAP_ZYNQ)
}

/// Look up an entry in the active table by path.
fn lookup_uart_entry(path: &str) -> Option<&'static UartMmioEntry> {
    active_uart_table().iter().find(|e| e.path == path)
}

/// CE-008: assert the active UART table belongs to the declared platform before
/// any `DevmemUart::open*` mmaps a physical address.
///
/// Pure decision helper (host-testable): given the active table's platform and
/// the declared platform, returns `Err` when they conflict — the SIGBUS guard.
/// When either is `None` (no declaration, or an unknown table) it returns
/// `Ok(())`, preserving the historical no-declaration behaviour byte-for-byte.
fn check_uart_platform_match(
    active: Option<UartPlatform>,
    declared: Option<UartPlatform>,
) -> Result<()> {
    match (active, declared) {
        (Some(active), Some(declared)) if active != declared => Err(HalError::Platform(format!(
            "UART table/platform mismatch: active table is {active:?} but the platform declared \
             itself {declared:?}. Refusing to mmap (a {declared:?} unit using the {active:?} table \
             would fault — e.g. AM335x without select_uart_table_am335x() SIGBUSes at 0x41001000). \
             Call the matching select_uart_table_* at platform init."
        ))),
        _ => Ok(()),
    }
}

/// Runtime form of [`check_uart_platform_match`] — reads the two OnceLocks.
fn assert_active_table_matches_platform() -> Result<()> {
    let active = UartPlatform::of_table(active_uart_table());
    let declared = DECLARED_UART_PLATFORM.get().copied();
    check_uart_platform_match(active, declared)
}

/// CE-008: pre-`set()` identity cross-check for [`select_uart_table`].
///
/// Pure decision helper (host-testable). Given the platform implied by the
/// candidate table (`candidate`) and any already-declared platform identity
/// (`declared`), returns `Err` when they CONFLICT — so the caller refuses to
/// commit the table OnceLock and the two OnceLocks stay consistent. Returns
/// `Ok(())` when they agree, or when either is `None` (unknown table / no prior
/// declaration), preserving the historical behaviour.
///
/// This MUST run BEFORE `ACTIVE_UART_TABLE.set()` — running it after would
/// commit the table to one platform and only then discover the declared
/// identity says another, leaving the inconsistent state CE-008 fixes.
fn select_table_identity_precheck(
    candidate: Option<UartPlatform>,
    declared: Option<UartPlatform>,
) -> Result<()> {
    match (candidate, declared) {
        (Some(candidate), Some(declared)) if candidate != declared => {
            Err(HalError::Platform(format!(
                "UART platform identity already declared ({declared:?}); \
                 refusing to select the {candidate:?} table mid-run"
            )))
        }
        _ => Ok(()),
    }
}

/// mmap page size for legacy callers that historically used PAGE_SIZE
/// directly. Per-entry `page_size` is preferred — both tables currently use
/// 0x1000 so this is a safe shared default.
#[allow(dead_code)]
const PAGE_SIZE: usize = 0x1000;

/// Compute (integer_divisor, fractional_divisor) for a 16550-family UART.
///
/// Pure, host-safe. Used by `DevmemUart::set_baud` and unit tests.
///
/// When `has_dlf` is false (Zynq), the fractional divisor is always zero
/// and the integer divisor is rounded-half-to-nearest as the existing
/// driver did:  `(base + baud/2) / baud`. This preserves bit-identical
/// behavior for any Zynq am1/am2 caller.
///
/// When `has_dlf` is true (CV1835), the integer divisor is `floor(base /
/// baud)` and the fractional part is `round((base/baud - integer) * 256)`
/// in the [0, 256) range, then clamped to a u8 (256 wraps to 0 with the
/// integer carrying — but at 256 the math is exact so no carry happens
/// for typical mining baud rates).
fn compute_divisor(base_baud: u32, target_baud: u32, has_dlf: bool) -> (u32, u8) {
    if target_baud == 0 {
        return (1, 0);
    }
    if !has_dlf {
        // Legacy Zynq: round-to-nearest integer divisor, no fractional.
        let div = (base_baud + target_baud / 2) / target_baud;
        return (div.max(1), 0);
    }

    // CV1835 DLF path: 8-bit (256-step) fractional latch.
    //
    // Compute scaled = round_to_nearest(base/baud * 256) using 64-bit
    // arithmetic. The +baud/2 numerator term is the integer half-step bias.
    // For 1_562_500/937_500 this evaluates to:
    //   scaled = (1_562_500*256 + 937_500/2) / 937_500
    //          = (400_000_000 + 468_750) / 937_500
    //          = 400_468_750 / 937_500
    //          = 427  (floor — but 427.166 nearest, so 427 is right)
    // → integer = 427 / 256 = 1
    // → fractional = 427 % 256 = 171 = 0xAB  ← matches DesignWare spec for
    //   exact 937500 baud.
    let numerator = (base_baud as u64) * 256 + (target_baud as u64) / 2;
    let scaled = numerator / target_baud as u64;
    let integer_divisor = (scaled / 256) as u32;
    let fractional = (scaled % 256) as u8;
    let integer_divisor = integer_divisor.max(1);
    (integer_divisor, fractional)
}

/// Devmem-based UART for direct NS16550A access.
pub struct DevmemUart {
    /// mmap'd pointer to UART registers.
    regs: *mut u32,
    /// Length of the `regs` mmap (the entry's `page_size`). Stored so `Drop`
    /// can `munmap` exactly what was mapped (CE-004).
    map_size: usize,
    /// Current baud rate.
    baud: u32,
    /// Device path (for logging).
    path: String,
    /// /dev/mem file descriptor (kept open for lifetime of mmap).
    _mem_fd: File,
}

// Safety: DevmemUart is used from a single thread (the serial I/O thread).
unsafe impl Send for DevmemUart {}

fn uart_diag_reg(regs: *mut u32, offset: usize) -> u32 {
    // SAFETY: callers pass the live UART mmap base returned by mmap; offsets
    // are the same 16550 register constants used by DevmemUart::read_reg.
    unsafe {
        let ptr = (regs as *const u8).add(offset) as *const u32;
        std::ptr::read_volatile(ptr)
    }
}

/// One-shot `/dev/mem` read of the PL-UART MCR/IER/LSR for a kernel-managed
/// (`of_serial`) ttyS port (BLK-1b, 2026-06-10).
///
/// The kernel `File` backend (`SerialChain`) cannot report its modem-control
/// register, so on `a lab unit` we could only ever *infer* OUT2 state. This maps the
/// UART register page read-only for a single read (kernel driver keeps owning
/// the port — a one-shot volatile read of three registers is benign) and
/// returns `(MCR, IER, LSR)`. Used for the mid-walk snapshot so the next live
/// run OBSERVES OUT2 instead of inferring it. Returns `None` if `path` is not a
/// known PL-UART or the mmap fails (diagnostic-only, never fatal).
pub(crate) fn pl_uart_diag_registers(path: &str) -> Option<(u32, u32, u32)> {
    // Fleet byte-identity (DCENT_EE HIGH-1): gate EXACTLY like the OUT2 poke — only a
    // `a lab unit`-fingerprint (or `DCENT_AM2_MCR_OUT2=1`) Zynq am2 PL-UART gets this one-shot
    // /dev/mem read. `is_zynq_am2_pl_uart_path()` alone was INSUFFICIENT: it matches by
    // path (ttyS1..S4) only, so Amlogic's `/dev/ttyS2` (which declares no UART platform
    // and so defaults to the Zynq table) would mmap a Zynq physical address on an A113D
    // SoC (SIGBUS/crash risk on a proven live platform), and AM3-BB would read its live
    // OMAP UART regs concurrently with the kernel driver. `am2_mcr_out2_mode()==Baseline`
    // returns None for every non-`a lab unit` unit, so the fleet is genuinely byte-identical.
    if matches!(am2_mcr_out2_mode(path), Am2McrOut2Mode::Baseline) {
        return None;
    }
    if assert_active_table_matches_platform().is_err() {
        return None;
    }
    let entry = lookup_uart_entry(path)?;
    // Read-only (DCENT_EE LOW-2): open /dev/mem + map PROT_READ so the "benign one-shot
    // read" property is structural, not just convention.
    let mem_fd = OpenOptions::new().read(true).open("/dev/mem").ok()?;
    // SAFETY: this diagnostic path is gated to a verified am2 PL-UART table
    // entry; the fd is `/dev/mem`, the mapped length is the entry page size,
    // and MAP_FAILED is checked before any volatile access.
    let regs = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            entry.page_size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            mem_fd.as_raw_fd(),
            entry.base_addr as libc::off_t,
        )
    };
    if regs == libc::MAP_FAILED {
        return None;
    }
    let mcr = uart_diag_reg(regs as *mut u32, UART_MCR);
    let ier = uart_diag_reg(regs as *mut u32, UART_IER_DLM);
    let lsr = uart_diag_reg(regs as *mut u32, UART_LSR);
    // SAFETY: `regs`/`page_size` are the mapping just created above.
    unsafe {
        libc::munmap(regs, entry.page_size);
    }
    Some((mcr, ier, lsr))
}

/// One-shot `/dev/mem` assert of `MCR = DTR+RTS+OUT2` (0x0B) on a kernel-managed
/// PL-UART (BLK-1b, 2026-06-10).
///
/// On `a lab unit`'s bitstream the PL-UART OUT2 bit (0x08) gates the FPGA UART block's
/// TX clock-out — without it the TX FIFO accepts bytes but the wire is never
/// driven, so chips emit ZERO bytes (silence, not garbage = the chain-enum=0
/// symptom). The standing OUT2 write lived ONLY in `DevmemUart::open_inner`, but
/// `a lab unit` was routed to the kernel `of_serial` transport (to escape the UIO
/// IRQ-165 conflict), which may not assert OUT2 (R-11: its state during the walk
/// is UNOBSERVED, hence the readback below) — the `of_serial` 8250 driver asserts
/// OUT2 only with a live IRQ, which the IRQ-165 unbind removes. This writes the
/// ABSOLUTE MCR value `0x0B` (DTR+RTS+OUT2 — DTR/RTS are the R4 transceiver-enable
/// bits, set anyway; OUT2 is the added FPGA TX-clock gate) WHILE the kernel driver
/// owns the port — a benign single-register write; `mcr_before` is captured for
/// observation only. Prefer `SerialChain::set_modem_dtr_rts_out2` (TIOCMBIS) first
/// so the kernel mctrl shadow is updated; this poke is the guarantee + observer.
/// Re-assert AFTER the final `set_baud` (the driver may rewrite MCR on a baud change).
///
/// Gated on `am2_mcr_out2_mode(path)`: only `a lab unit`-fingerprint units (or the
/// explicit `DCENT_AM2_MCR_OUT2=1` override) are poked — every other AM2/fleet
/// unit returns `Ok(None)` and is byte-identical. Returns
/// `Ok(Some((mcr_before, mcr_after)))` when it pokes, `Ok(None)` when the unit
/// is Baseline (not a `a lab unit`/override target).
pub(crate) fn pl_uart_assert_mcr_out2(path: &str) -> Result<Option<(u32, u32)>> {
    match am2_mcr_out2_mode(path) {
        Am2McrOut2Mode::Baseline => return Ok(None),
        Am2McrOut2Mode::EnvOverride | Am2McrOut2Mode::Xil25Fingerprint => {}
    }
    assert_active_table_matches_platform()?;
    let entry = lookup_uart_entry(path).ok_or_else(|| {
        HalError::Platform(format!(
            "pl_uart_assert_mcr_out2: unknown UART path '{}'",
            path
        ))
    })?;
    let mem_fd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::DeviceOpen {
            path: "/dev/mem".to_string(),
            source: e,
        })?;
    // SAFETY: `entry` comes from the active platform UART table after the
    // platform-match guard; the mapping length is the entry page size and
    // MAP_FAILED is handled before register access.
    let regs = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            entry.page_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            mem_fd.as_raw_fd(),
            entry.base_addr as libc::off_t,
        )
    };
    if regs == libc::MAP_FAILED {
        return Err(HalError::Platform(format!(
            "pl_uart_assert_mcr_out2: mmap UART at 0x{:08X} failed: {}",
            entry.base_addr,
            std::io::Error::last_os_error()
        )));
    }
    let mcr_before = uart_diag_reg(regs as *mut u32, UART_MCR);
    // SAFETY: `regs` is the live UART mapping; UART_MCR is the same 16550 offset
    // DevmemUart::write_reg uses. Write the absolute MCR value 0x0B = DTR+RTS+OUT2
    // (DTR/RTS = R4 transceiver-enable bits, OUT2 = the FPGA TX-clock gate).
    unsafe {
        let ptr = (regs as *mut u8).add(UART_MCR) as *mut u32;
        std::ptr::write_volatile(ptr, MCR_DTR_RTS_OUT2);
    }
    let mcr_after = uart_diag_reg(regs as *mut u32, UART_MCR);
    // SAFETY: `regs`/`page_size` are the mapping just created above.
    unsafe {
        libc::munmap(regs, entry.page_size);
    }
    if (mcr_after & MCR_DTR_RTS_OUT2) != MCR_DTR_RTS_OUT2 {
        return Err(HalError::Platform(format!(
            "pl_uart_assert_mcr_out2: MCR readback missing DTR/RTS/OUT2 after write (before=0x{:02X}, after=0x{:02X}, expected mask=0x{:02X})",
            mcr_before,
            mcr_after,
            MCR_DTR_RTS_OUT2
        )));
    }
    Ok(Some((mcr_before, mcr_after)))
}

impl DevmemUart {
    /// Open a UART via /dev/mem, bypassing the kernel serial driver.
    ///
    /// First unbinds the kernel driver (if bound) to avoid conflicts,
    /// then mmaps the UART registers and configures for 8N1 at the given baud.
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        Self::open_inner(path, baud, false)
    }

    /// Open a UART via /dev/mem WITHOUT unbinding the kernel driver.
    ///
    /// On am2-s17, unbinding the kernel serial driver can break PSU I2C
    /// communication and other system state. This mode mmaps the UART
    /// registers directly while leaving the kernel driver bound.
    /// Safe because we use polled I/O (no IRQ conflicts in poll mode).
    pub fn open_no_unbind(path: &str, baud: u32) -> Result<Self> {
        Self::open_inner(path, baud, true)
    }

    /// Open a UART via /dev/mem while preserving inherited UART state.
    ///
    /// This is the true passthrough variant for am2 handoff experiments:
    ///
    /// - does NOT unbind the kernel serial driver
    /// - does NOT disable interrupts
    /// - does NOT reset FIFOs
    /// - does NOT rewrite baud / line settings
    /// - does NOT drain inherited RX bytes
    ///
    /// The caller provides `assumed_baud` for bookkeeping/logging only. This
    /// path exists specifically for inherited-state bring-up on am2 where the
    /// standard devmem open would otherwise perturb the live UART state.
    pub fn open_preserve_state(path: &str, assumed_baud: u32) -> Result<Self> {
        // CE-008: refuse to mmap when the active table doesn't match the
        // declared platform (SIGBUS guard). No-op when nothing was declared.
        assert_active_table_matches_platform()?;
        let entry = lookup_uart_entry(path).ok_or_else(|| {
            HalError::Platform(format!("Unknown UART path '{}' — not in MMIO map", path))
        })?;
        let base_addr = entry.base_addr;
        let page_size = entry.page_size;

        tracing::info!(
            path,
            "DevmemUart: preserving inherited kernel/UART state (no unbind, no reset, no baud rewrite)"
        );

        let mem_fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|e| HalError::DeviceOpen {
                path: "/dev/mem".to_string(),
                source: e,
            })?;

        // SAFETY: `entry` was resolved from the active platform table after
        // `assert_active_table_matches_platform`; `page_size` bounds the UART
        // register page and MAP_FAILED is checked before use.
        let regs = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                mem_fd.as_raw_fd(),
                base_addr as libc::off_t,
            )
        };
        if regs == libc::MAP_FAILED {
            return Err(HalError::Platform(format!(
                "mmap UART at 0x{:08X} failed: {}",
                base_addr,
                std::io::Error::last_os_error()
            )));
        }

        tracing::info!(
            path,
            assumed_baud,
            base_addr = format_args!("0x{:08X}", base_addr),
            has_dlf = entry.has_dlf,
            mcr = format_args!("0x{:02X}", uart_diag_reg(regs as *mut u32, UART_MCR)),
            ier = format_args!("0x{:02X}", uart_diag_reg(regs as *mut u32, UART_IER_DLM)),
            lcr = format_args!("0x{:02X}", uart_diag_reg(regs as *mut u32, UART_LCR)),
            "DevmemUart opened in preserve-state passthrough mode"
        );

        Ok(Self {
            regs: regs as *mut u32,
            map_size: page_size,
            baud: assumed_baud,
            path: path.to_string(),
            _mem_fd: mem_fd,
        })
    }

    fn open_inner(path: &str, baud: u32, skip_unbind: bool) -> Result<Self> {
        // CE-008: refuse to mmap when the active table doesn't match the
        // declared platform (SIGBUS guard). No-op when nothing was declared,
        // so every existing caller is byte-identical unless it mis-declared.
        assert_active_table_matches_platform()?;
        // Look up MMIO entry (per-platform: Zynq Xilinx 16550 vs CV1835
        // DesignWare 16550A) from the active selector table.
        let entry = lookup_uart_entry(path).ok_or_else(|| {
            HalError::Platform(format!("Unknown UART path '{}' — not in MMIO map", path))
        })?;
        let base_addr = entry.base_addr;
        let page_size = entry.page_size;

        // Unbind kernel serial driver to avoid conflicts (unless skip_unbind)
        if !skip_unbind {
            Self::unbind_kernel_driver(path);
        } else {
            tracing::info!(
                path,
                "DevmemUart: skipping kernel driver unbind (am2 safe mode)"
            );
        }

        // Open /dev/mem
        let mem_fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|e| HalError::DeviceOpen {
                path: "/dev/mem".to_string(),
                source: e,
            })?;

        // mmap the UART register page (per-entry page_size; both tables use 0x1000 today).
        // SAFETY: `entry` was resolved from the active platform table after
        // `assert_active_table_matches_platform`; `page_size` bounds the UART
        // register page and MAP_FAILED is checked before use.
        let regs = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                mem_fd.as_raw_fd(),
                base_addr as libc::off_t,
            )
        };
        if regs == libc::MAP_FAILED {
            return Err(HalError::Platform(format!(
                "mmap UART at 0x{:08X} failed: {}",
                base_addr,
                std::io::Error::last_os_error()
            )));
        }

        let mut uart = Self {
            regs: regs as *mut u32,
            map_size: page_size,
            baud: 0,
            path: path.to_string(),
            _mem_fd: mem_fd,
        };

        // Configure: disable interrupts, enable+reset FIFOs, set 8N1, set baud.
        //
        // IER=0x00 and MCR=DTR+RTS (0x03) are the load-bearing R4 baseline
        // proven on `a lab unit` and used by `a lab unit`'s first-shares run. Later
        // `a lab unit` captures corrected the  FIFO hypothesis: bosminer was
        // mining through PL UARTs and its active register state used
        // MCR=0x0B (DTR+RTS+OUT2). Keep IER env-gated and choose MCR below
        // from either the explicit env override or the `a lab unit` board stamp.
        // IER: 0x00 (R4 polled baseline) OR 0x05 (bosminer parity, env-gated)
        let ier_value = if am2_ier_bosminer_parity_enabled() {
            tracing::info!(path, ier = "0x05", "DevmemUart: Wave-27.2 IER=0x05 (DCENT_AM2_IER_BOSMINER_PARITY=1, bosminer-strace parity)");
            IER_BOSMINER_PARITY
        } else {
            IER_POLLED_MODE
        };
        uart.write_reg(UART_IER_DLM, ier_value);
        uart.write_reg(UART_FCR, FCR_ENABLE_RESET); // Enable FIFOs, reset both
        uart.set_baud(baud)?; // also writes LCR=0x03 (8N1) after divisor latch

        // Assert DTR + RTS so the RS-485 chain transceiver is powered on.
        //
        //  (2026-05-23) — env-gated OUT2 override.
        //
        // The Phase-0 live evidence on `a lab unit` 2026-05-23 evening proved the
        //  hypothesis wrong: bosminer on `a lab unit` mines at 26.5 GH/s
        // using `/dev/ttyS1` + `/dev/ttyS3` (PL UART), NOT the FPGA FIFO
        // at `0x43C0Nxxx`. Bosminer's PL UART register state captured live
        // while mining: MCR = 0x0B (DTR + RTS + OUT2) on BOTH active
        // chains. The OUT2 bit gates the FPGA UART block's TX clock-out on
        // `a lab unit`'s bitstream — without OUT2 asserted, the TX FIFO accepts
        // bytes but the FPGA UART block holds them in the shift register
        // indefinitely (matches the chain-enum-0 symptom: 0/126 chip
        // responses, daemon log "Phase 4-7 failed: 0 chips responded to
        // GetAddress at 115200" while UART writes show no error).
        //
        // `a lab unit`'s 2026-05-15 first-shares run worked at MCR = 0x03 (DTR +
        // RTS only). The two units have different FPGA UART OUT2 wiring
        // (likely different bitstream BUILD_IDs, or the same BUILD_ID with
        // different OUT2-gating behaviour — TBD via RE-017).
        //
        // The env gate keeps `a lab unit`'s proven baseline byte-identical while
        // letting `a lab unit` (and any future am2 unit with the OUT2-gated FPGA
        // UART) opt into MCR = 0x0B. `a lab unit` can select this from its board
        // fingerprint; `DCENT_AM2_MCR_OUT2=1` remains the operator override
        // for units that fail chain enumeration with the symptom above.
        //
        //
        // and .
        let mcr_mode = am2_mcr_out2_mode(path);
        let mcr_value = match mcr_mode {
            Am2McrOut2Mode::EnvOverride => {
                tracing::info!(
                    path,
                    mcr = "0x0B",
                    "DevmemUart: MCR=DTR+RTS+OUT2 (DCENT_AM2_MCR_OUT2=1)"
                );
                MCR_DTR_RTS_OUT2
            }
            Am2McrOut2Mode::Xil25Fingerprint => {
                tracing::info!(
                    path,
                    mcr = "0x0B",
                    platform = ZYNQ_BM3_AM2_PLATFORM,
                    board_target_suffix = XIL_25_BOARD_TARGET_SUFFIX,
                    "DevmemUart: MCR=DTR+RTS+OUT2 selected by XIL-25 board fingerprint"
                );
                MCR_DTR_RTS_OUT2
            }
            Am2McrOut2Mode::Baseline => MCR_DTR_RTS,
        };
        uart.write_reg(UART_MCR, mcr_value);

        // Drain any stale RX data
        for _ in 0..256 {
            if uart.read_reg(UART_LSR) & LSR_DR == 0 {
                break;
            }
            let _ = uart.read_reg(UART_RBR_THR);
        }

        tracing::info!(
            path,
            baud,
            base_addr = format_args!("0x{:08X}", base_addr),
            has_dlf = entry.has_dlf,
            mcr = format_args!("0x{:02X}", uart.read_reg(UART_MCR)),
            ier = format_args!("0x{:02X}", uart.read_reg(UART_IER_DLM)),
            lcr = format_args!("0x{:02X}", uart.read_reg(UART_LCR)),
            "DevmemUart opened (bypassing kernel serial driver)"
        );

        Ok(uart)
    }

    /// Try to unbind the kernel serial driver from this UART.
    ///
    /// Both Zynq (Xilinx of_serial) and CV1835 (DesignWare 8250_dw) bind
    /// against `/sys/bus/platform/drivers/of_serial/unbind`. Device names
    /// are derived from the platform DT node — for Zynq the suffix is
    /// `.uart`, for CV1835 the suffix is `.serial` (matches the dts
    /// compatible string the dev-kit ships).
    fn unbind_kernel_driver(path: &str) {
        // Map ttyS path to platform device name. Zynq + CV1835 share /dev/ttyS1
        // but the DT compatible string differs, so this is platform-aware via
        // the active UART table — same selector used by lookup_uart_entry().
        //
        // Zynq am1/am2 (Xilinx 16550 PL UART) DT compat strings end in `.uart`.
        // CV1835 (DesignWare 16550A) DT compat strings end in `.serial`.
        let entry = match lookup_uart_entry(path) {
            Some(e) => e,
            None => return,
        };
        // Linux DT platform nodes use lowercase hex with no leading 0x and no
        // leading zeros — `5000c000.serial`, `41001000.uart`, `50010000.serial`.
        let suffix = if entry.has_dlf { "serial" } else { "uart" };
        let dev_name = format!("{:x}.{}", entry.base_addr, suffix);

        // Unbind of_serial driver
        let unbind_path = "/sys/bus/platform/drivers/of_serial/unbind";
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(unbind_path) {
            use std::io::Write;
            let _ = f.write_all(dev_name.as_bytes());
            tracing::info!(dev_name = %dev_name, "Unbound kernel serial driver");
        }
    }

    /// Read a 32-bit UART register.
    #[inline]
    fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: `self.regs` is the live UART register mapping owned by this
        // DevmemUart, and callers pass 4-byte-aligned UART register constants.
        unsafe {
            let ptr = (self.regs as *const u8).add(offset) as *const u32;
            std::ptr::read_volatile(ptr)
        }
    }

    /// Write a 32-bit UART register.
    #[inline]
    fn write_reg(&self, offset: usize, value: u32) {
        // SAFETY: `self.regs` is the live UART register mapping owned by this
        // DevmemUart, and callers pass 4-byte-aligned UART register constants.
        unsafe {
            let ptr = (self.regs as *mut u8).add(offset) as *mut u32;
            std::ptr::write_volatile(ptr, value);
        }
    }

    /// Read key UART control registers for open-path diagnostics.
    pub fn diagnostic_registers(&self) -> (u32, u32, u32) {
        (
            self.read_reg(UART_MCR),
            self.read_reg(UART_IER_DLM),
            self.read_reg(UART_LSR),
        )
    }

    /// Set baud rate via divisor latch (and DLF on CV1835).
    ///
    /// Zynq Xilinx 16550 PL UART path:
    /// ```text
    ///   divisor = round(base_baud / target_baud)
    ///     e.g. 6_249_999 / 937_500 ≈ 6.66 → integer 7
    ///   actual baud ≈ 6_249_999 / 7 = 892_857  (~5% off — acceptable)
    /// ```
    ///
    /// CV1835 DesignWare 16550A path (with fractional Divisor Latch):
    /// ```text
    ///   true_divisor    = base_baud / target_baud
    ///                   = 1_562_500 / 937_500 = 1.66666...
    ///   integer_divisor = floor(true_divisor) = 1
    ///   fractional      = round((true_divisor - integer_divisor) * 256)
    ///                   = round(0.66666... * 256)
    ///                   = round(170.666...)
    ///                   = 171 = 0xAB
    ///   write order: DLAB=1 → DLL=1 → DLM=0 → DLF=0xAB → DLAB=0
    /// ```
    /// Yields exact 937500 baud at 25 MHz XTAL on CV1835.
    ///
    /// and surrounding rules,
    /// no SOFTR-style soft-reset path is taken here — divisor writes are
    /// the only register writes during a baud change.
    pub fn set_baud(&mut self, baud: u32) -> Result<()> {
        let entry = lookup_uart_entry(&self.path).ok_or_else(|| {
            HalError::Platform(format!(
                "DevmemUart::set_baud: path '{}' not in MMIO map",
                self.path
            ))
        })?;
        let (integer_divisor, fractional) = compute_divisor(entry.base_baud, baud, entry.has_dlf);

        // Set DLAB to access divisor latch
        let lcr = self.read_reg(UART_LCR);
        self.write_reg(UART_LCR, lcr | LCR_DLAB);

        // Write integer divisor (DLL low + DLM high, both alias to RBR/THR + IER under DLAB=1)
        self.write_reg(UART_RBR_THR, integer_divisor & 0xFF);
        self.write_reg(UART_IER_DLM, (integer_divisor >> 8) & 0xFF);

        // CV1835-only: write fractional divisor. has_dlf=false on Zynq makes
        // this a no-op — preserves bit-identical Zynq behavior. The DLF
        // register is documented as DLAB-gated on DesignWare DW_apb_uart, so
        // we keep DLAB asserted here.
        if entry.has_dlf {
            self.write_reg(UART_DLF, fractional as u32 & 0xFF);
        }

        // Clear DLAB, set 8N1 (or bosminer-parity 0x13 when env-gated)
        let lcr_value = if am2_ier_bosminer_parity_enabled() {
            LCR_BOSMINER_PARITY
        } else {
            LCR_8N1
        };
        self.write_reg(UART_LCR, lcr_value);

        // Compute actual baud for logging. On Zynq this is base / int_div; on
        // CV1835 we report the fractional-corrected effective rate.
        let actual_baud = if entry.has_dlf {
            // effective_div = integer + fractional/256
            // baud = base * 256 / (integer*256 + fractional)
            let denom = integer_divisor
                .saturating_mul(256)
                .saturating_add(fractional as u32);
            if denom == 0 {
                0
            } else {
                ((entry.base_baud as u64 * 256) / denom as u64) as u32
            }
        } else if integer_divisor == 0 {
            0
        } else {
            entry.base_baud / integer_divisor
        };

        tracing::debug!(
            baud,
            divisor = integer_divisor,
            fractional,
            has_dlf = entry.has_dlf,
            actual_baud,
            "DevmemUart baud set"
        );
        self.baud = baud;
        Ok(())
    }

    /// Write bytes to the UART (polled TX).
    pub fn write_bytes(&self, data: &[u8]) -> Result<()> {
        for &byte in data {
            // Wait for TX holding register empty
            let mut tries = 0u32;
            while self.read_reg(UART_LSR) & LSR_THRE == 0 {
                tries += 1;
                if tries > 1_000_000 {
                    return Err(HalError::Platform("UART TX timeout".to_string()));
                }
                std::hint::spin_loop();
            }
            self.write_reg(UART_RBR_THR, byte as u32);
        }
        Ok(())
    }

    /// Read available bytes from the UART (polled RX, non-blocking).
    ///
    /// Returns the number of bytes read. Returns 0 immediately if no data.
    pub fn read_bytes(&self, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            if self.read_reg(UART_LSR) & LSR_DR == 0 {
                break;
            }
            buf[n] = (self.read_reg(UART_RBR_THR) & 0xFF) as u8;
            n += 1;
        }
        n
    }

    /// Read bytes with a timeout (milliseconds). Returns number of bytes read.
    pub fn read_bytes_timeout(&self, buf: &mut [u8], timeout_ms: u64) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        let mut n = 0;
        while n < buf.len() {
            if self.read_reg(UART_LSR) & LSR_DR != 0 {
                buf[n] = (self.read_reg(UART_RBR_THR) & 0xFF) as u8;
                n += 1;
            } else if std::time::Instant::now() >= deadline {
                break;
            } else {
                // Brief yield to avoid burning 100% CPU
                std::thread::yield_now();
            }
        }
        n
    }

    /// Flush: drain RX FIFO and wait for TX to complete.
    pub fn flush_io(&self) {
        // Drain RX
        for _ in 0..256 {
            if self.read_reg(UART_LSR) & LSR_DR == 0 {
                break;
            }
            let _ = self.read_reg(UART_RBR_THR);
        }
        // Wait for TX empty
        let mut tries = 0u32;
        while self.read_reg(UART_LSR) & LSR_TX_IDLE != LSR_TX_IDLE {
            tries += 1;
            if tries > 100_000 {
                break;
            }
            std::hint::spin_loop();
        }
    }

    /// Wait for the transmit holding register to empty without touching RX.
    pub fn drain_tx(&self) {
        let mut tries = 0u32;
        while self.read_reg(UART_LSR) & LSR_TX_IDLE != LSR_TX_IDLE {
            tries += 1;
            if tries > 1_000_000 {
                break;
            }
            std::hint::spin_loop();
        }
    }

    /// Get the current baud rate.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// Get the device path.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for DevmemUart {
    fn drop(&mut self) {
        // CE-004: release the `/dev/mem` UART register mapping. Previously this
        // was a deliberate no-op ("let process exit reclaim it"), but
        // `DevmemUart` is opened/closed repeatedly on the am2 cold-boot →
        // enum → retry path (and on every `set_baud`/handoff bring-up attempt),
        // so leaving each VMA mapped leaks one page of physical-register address
        // space per open. Mirror the `board_control.rs` (hold_am2_reset_devmem)
        // and `psu_gpio_i2c.rs`  munmap-on-Drop convention.
        //
        // SAFETY: `regs`/`map_size` came from the `libc::mmap` in
        // `open_inner`/`open_preserve_state` (len = entry.page_size). We own the
        // mapping for the lifetime of this struct and no register pointer
        // derived from `regs` is used after Drop. `_mem_fd` is closed
        // afterwards by its own Drop. A null/MAP_FAILED `regs` never reaches a
        // constructed `DevmemUart` (both constructors return Err first), so the
        // unmap target is always valid.
        if !self.regs.is_null() {
            // SAFETY: see the block comment above; this unmaps the exact
            // mapping owned by this DevmemUart.
            let rc = unsafe { libc::munmap(self.regs as *mut libc::c_void, self.map_size) };
            if rc != 0 {
                tracing::warn!(
                    path = %self.path,
                    map_size = self.map_size,
                    error = %std::io::Error::last_os_error(),
                    "DevmemUart::drop: munmap of UART register page failed"
                );
            }
        }
    }
}

// ===========================================================================
// Host-safe unit tests for the table-driven UART config (1 / A3).
//
// These tests do NOT touch /dev/mem, mmap, ioctl, or any kernel state. They
// validate the pure DLF math, the static table contents, and that path
// lookup against an explicit table picks the correct platform entry.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// Pure helper used only by tests — looks up a path against a specific
    /// table without touching the global `ACTIVE_UART_TABLE` OnceLock. This
    /// is what lets the path-collision test prove that the per-platform
    /// selector resolves `/dev/ttyS1` to different physical addresses on
    /// Zynq vs CV1835.
    fn lookup_in(table: &'static [UartMmioEntry], path: &str) -> Option<&'static UartMmioEntry> {
        table.iter().find(|e| e.path == path)
    }

    #[test]
    fn dlf_for_937500_baud_at_25mhz_xtal_is_0xab() {
        // Per the DesignWare DW_apb_uart Databook (DLF_SIZE=8) and the
        // dev-kit multi_platform_master.md "Cvitek CV1835 Control Board"
        // section: at base_baud = 1_562_500 (25 MHz XTAL / 16) and target
        // 937500 baud, the integer divisor is 1 and the fractional latch
        // value is 0xAB (171).
        let (integer, fractional) = compute_divisor(
            /* base */ 1_562_500, /* target */ 937_500, /* has_dlf */ true,
        );
        assert_eq!(integer, 1, "CV1835 integer divisor for 937500 baud");
        assert_eq!(
            fractional, 0xAB,
            "CV1835 DLF for exact 937500 baud at 25 MHz XTAL must be 0xAB (171)"
        );
    }

    // -----------------------------------------------------------------------
    // CE-008: UART platform-identity SIGBUS guard (pure decision helper).
    // -----------------------------------------------------------------------

    #[test]
    fn uart_platform_of_table_maps_each_static_table() {
        assert_eq!(
            UartPlatform::of_table(UART_MMIO_MAP_ZYNQ),
            Some(UartPlatform::Zynq)
        );
        assert_eq!(
            UartPlatform::of_table(UART_MMIO_MAP_CV1835),
            Some(UartPlatform::Cv1835)
        );
        assert_eq!(
            UartPlatform::of_table(UART_MMIO_MAP_AM335X),
            Some(UartPlatform::Am335x)
        );
    }

    #[test]
    fn check_uart_platform_match_refuses_conflict() {
        // The cited SIGBUS bug: platform declared AM335x but the active table
        // is still the Zynq default → must be a clean Err, never a mmap.
        assert!(
            check_uart_platform_match(Some(UartPlatform::Zynq), Some(UartPlatform::Am335x))
                .is_err()
        );
        // Symmetric: declared Zynq, active CV1835.
        assert!(
            check_uart_platform_match(Some(UartPlatform::Cv1835), Some(UartPlatform::Zynq))
                .is_err()
        );
    }

    #[test]
    fn check_uart_platform_match_ok_when_aligned_or_undeclared() {
        // Aligned: no error.
        assert!(
            check_uart_platform_match(Some(UartPlatform::Am335x), Some(UartPlatform::Am335x))
                .is_ok()
        );
        // Undeclared (None) preserves the historical no-check behaviour —
        // every existing caller that never declares a platform is unaffected.
        assert!(check_uart_platform_match(Some(UartPlatform::Zynq), None).is_ok());
        assert!(check_uart_platform_match(None, Some(UartPlatform::Zynq)).is_ok());
        assert!(check_uart_platform_match(None, None).is_ok());
    }

    #[test]
    fn select_table_identity_precheck_refuses_conflicting_predeclare() {
        // CE-008: a platform was already declared (e.g. AM335x stamped by the
        // constructor) but a later path tries to select the Zynq table. The
        // precheck MUST return Err so `select_uart_table` refuses BEFORE it
        // commits ACTIVE_UART_TABLE — otherwise the table OnceLock would be set
        // and only then the identity conflict surfaced, an inconsistent state.
        assert!(select_table_identity_precheck(
            Some(UartPlatform::Zynq),
            Some(UartPlatform::Am335x)
        )
        .is_err());
        // Symmetric direction.
        assert!(select_table_identity_precheck(
            Some(UartPlatform::Am335x),
            Some(UartPlatform::Zynq)
        )
        .is_err());
    }

    #[test]
    fn select_table_identity_precheck_ok_when_aligned_or_unknown() {
        // Same platform pre-declared → fine (declare_uart_platform is idempotent).
        assert!(select_table_identity_precheck(
            Some(UartPlatform::Cv1835),
            Some(UartPlatform::Cv1835)
        )
        .is_ok());
        // No prior declaration → historical behaviour, no check.
        assert!(select_table_identity_precheck(Some(UartPlatform::Zynq), None).is_ok());
        // Unknown candidate table (None) → undeclared, preserves old behaviour.
        assert!(select_table_identity_precheck(None, Some(UartPlatform::Zynq)).is_ok());
        assert!(select_table_identity_precheck(None, None).is_ok());
    }

    #[test]
    fn dlf_skipped_when_has_dlf_false_preserves_zynq_behavior() {
        // Zynq Xilinx 16550 PL UART: base_baud = 6_249_999, target = 937500.
        // The legacy formula (base + baud/2) / baud with no fractional component
        // should produce divisor=7 (closest integer to 6.66). Fractional MUST
        // be zero — this is the load-bearing safety property: the Zynq path
        // never writes the DLF register.
        let (integer, fractional) = compute_divisor(6_249_999, 937_500, /* has_dlf */ false);
        let expected = (6_249_999 + 937_500 / 2) / 937_500; // = 7
        assert_eq!(
            integer, expected,
            "Zynq integer divisor unchanged from legacy"
        );
        assert_eq!(integer, 7, "Sanity: 6_249_999/937500 ≈ 6.66 rounds to 7");
        assert_eq!(
            fractional, 0,
            "has_dlf=false must produce zero fractional — DLF write must be a no-op on Zynq"
        );
    }

    #[test]
    fn cv1835_table_has_5_uart_entries_at_correct_bases() {
        // The dev-kit's devmem_uart.h documents 5 UART blocks at 4 KB stride
        // starting at 0x0500_C000. This sanity-check guards against accidental
        // address-table edits that would silently route ASIC chains to wrong
        // physical UARTs (catastrophic if it ever shipped to a CV1835 unit).
        assert_eq!(UART_MMIO_MAP_CV1835.len(), 5);
        let expected: [(&str, usize); 5] = [
            ("/dev/ttyS0", 0x0500_C000),
            ("/dev/ttyS1", 0x0500_D000),
            ("/dev/ttyS2", 0x0500_E000),
            ("/dev/ttyS3", 0x0500_F000),
            ("/dev/ttyS4", 0x0501_0000),
        ];
        for (i, (path, base)) in expected.iter().enumerate() {
            assert_eq!(UART_MMIO_MAP_CV1835[i].path, *path, "entry {} path", i);
            assert_eq!(UART_MMIO_MAP_CV1835[i].base_addr, *base, "entry {} base", i);
            assert_eq!(
                UART_MMIO_MAP_CV1835[i].base_baud, 1_562_500,
                "entry {} base_baud (25 MHz / 16)",
                i
            );
            assert!(
                UART_MMIO_MAP_CV1835[i].has_dlf,
                "entry {} must have DLF (DesignWare 16550A)",
                i
            );
            assert_eq!(UART_MMIO_MAP_CV1835[i].page_size, 0x1000);
        }
    }

    #[test]
    fn path_collision_resolved_by_platform_selector() {
        // /dev/ttyS1 exists in BOTH the Zynq and CV1835 tables but maps to
        // wildly different physical addresses (0x4100_1000 vs 0x0500_D000).
        // The platform-injected table is what disambiguates — without that,
        // a CV1835 unit would mmap the wrong address and either segfault or
        // (worse) corrupt unrelated MMIO space.
        let zynq_entry =
            lookup_in(UART_MMIO_MAP_ZYNQ, "/dev/ttyS1").expect("Zynq /dev/ttyS1 must resolve");
        let cv_entry =
            lookup_in(UART_MMIO_MAP_CV1835, "/dev/ttyS1").expect("CV1835 /dev/ttyS1 must resolve");

        assert_eq!(
            zynq_entry.base_addr, 0x4100_1000,
            "Zynq /dev/ttyS1 → 0x41001000 (Xilinx 16550 PL UART)"
        );
        assert!(
            !zynq_entry.has_dlf,
            "Zynq /dev/ttyS1 must NOT have DLF (Xilinx 16550 has no fractional latch)"
        );

        assert_eq!(
            cv_entry.base_addr, 0x0500_D000,
            "CV1835 /dev/ttyS1 → 0x0500D000 (DesignWare 16550A, ASIC chain 0)"
        );
        assert!(
            cv_entry.has_dlf,
            "CV1835 /dev/ttyS1 must have DLF (DesignWare 16550A)"
        );

        // Spot-check: same path, different base_baud (different XTAL).
        assert_ne!(zynq_entry.base_baud, cv_entry.base_baud);
        assert_eq!(zynq_entry.base_baud, 6_249_999);
        assert_eq!(cv_entry.base_baud, 1_562_500);
    }

    #[test]
    fn zynq_table_has_4_uart_entries_at_correct_bases() {
        // Anti-regression for the existing Zynq am1/am2 path. Any change to
        // these addresses or has_dlf=false invariant breaks live S9 / S19j Pro
        // Zynq mining. Don't touch.
        assert_eq!(UART_MMIO_MAP_ZYNQ.len(), 4);
        let expected: [(&str, usize); 4] = [
            ("/dev/ttyS1", 0x4100_1000),
            ("/dev/ttyS2", 0x4101_1000),
            ("/dev/ttyS3", 0x4102_1000),
            ("/dev/ttyS4", 0x4103_1000),
        ];
        for (i, (path, base)) in expected.iter().enumerate() {
            assert_eq!(UART_MMIO_MAP_ZYNQ[i].path, *path);
            assert_eq!(UART_MMIO_MAP_ZYNQ[i].base_addr, *base);
            assert_eq!(UART_MMIO_MAP_ZYNQ[i].base_baud, 6_249_999);
            assert!(
                !UART_MMIO_MAP_ZYNQ[i].has_dlf,
                "Zynq Xilinx 16550 PL UART has no DLF — must remain false"
            );
        }
    }

    // ---------------------------------------------------------------------
    // W14.A4 — AM335x BB OMAP UART MMIO map anti-regression pins.
    // (Phase B 2026-05-12: device names corrected to `/dev/ttyS%d` per the
    // live `a lab unit` LuxOS probe; MMIO base addresses unchanged.)
    //
    // Source: AM335x TRM register-map appendix + the `a lab unit` dmesg
    // (`48022000.serial: ttyS1 ... base_baud=3000000`, etc.) + W4 RE
    // `am335x_board_init.c::am335x_uart_init`. Clock is 48 MHz → base_baud
    // = 3_000_000. NO DLF (OMAP UART is 16550-compatible, NOT DesignWare).
    // The W13 cold-boot bug was that DevmemUart had only Zynq + CV1835
    // tables and `set_baud()` returned "path not in MMIO map" for the
    // AM335x chain UART paths — these tests pin the addresses + fail-fast
    // if a future "consolidation" tries to give AM335x DesignWare DLF.
    // ---------------------------------------------------------------------

    #[test]
    fn am335x_table_has_4_omap_uart_entries_at_correct_bases() {
        // AM335x BB ships 3-4 chain UARTs (UART1/2/4 + UART5 on 4-board
        // SKUs; UART3 unpopulated in the BB DTS). UART0 is the console and
        // is NOT in this table. Device names are `/dev/ttyS%d` (mainline
        // omap-serial driver) per the live `a lab unit` probe.
        assert_eq!(UART_MMIO_MAP_AM335X.len(), 4);
        let expected: [(&str, usize); 4] = [
            ("/dev/ttyS1", 0x4802_2000),
            ("/dev/ttyS2", 0x4802_4000),
            ("/dev/ttyS4", 0x481A_8000),
            ("/dev/ttyS5", 0x481A_A000),
        ];
        for (i, (path, base)) in expected.iter().enumerate() {
            assert_eq!(UART_MMIO_MAP_AM335X[i].path, *path, "entry {} path", i);
            assert_eq!(UART_MMIO_MAP_AM335X[i].base_addr, *base, "entry {} base", i);
        }
    }

    #[test]
    fn am335x_base_baud_matches_48mhz_omap_clock() {
        // AM335x DTS `clock-frequency = 48000000` (48 MHz) → 48M / 16
        // = 3_000_000. Live-pinned so a future refactor that copies the
        // CV1835 25 MHz figure or the Zynq 100 MHz figure is rejected.
        for entry in UART_MMIO_MAP_AM335X {
            assert_eq!(
                entry.base_baud, 3_000_000,
                "AM335x {} base_baud must be 3_000_000 (48 MHz / 16)",
                entry.path
            );
        }
    }

    #[test]
    fn am335x_table_has_no_dlf() {
        // OMAP UART is 16550-compatible, NOT DesignWare. Setting DLF would
        // write to a reserved register. R4 binary scan + W14.A4 audit pin
        // this so a future consolidation copy-paste from CV1835 fails CI.
        for entry in UART_MMIO_MAP_AM335X {
            assert!(
                !entry.has_dlf,
                "AM335x {} must NOT have DLF — OMAP UART is 16550-compat, NOT DesignWare",
                entry.path
            );
        }
    }

    #[test]
    fn am335x_table_has_no_tty_s3_entry() {
        // The BB DTS leaves UART3 (`/dev/ttyS3`) unpopulated. Any entry for
        // ttyS3 here would silently route a chain UART into an unused
        // device. Anti-regression for that class of mistake. (Also pins
        // that the legacy `/dev/ttyO%d` names are gone — the live `a lab unit`
        // probe confirmed `/dev/ttyS%d`.)
        for entry in UART_MMIO_MAP_AM335X {
            assert_ne!(
                entry.path, "/dev/ttyS3",
                "/dev/ttyS3 is unpopulated in the BB DTS — must not appear in the AM335x UART table"
            );
            assert!(
                !entry.path.starts_with("/dev/ttyO"),
                "AM335x UART device names are `/dev/ttyS%d` (live .79 probe), not the W4-era `/dev/ttyO%d`"
            );
        }
        // Also pin the BB DTS-enabled set explicitly.
        let paths: Vec<&str> = UART_MMIO_MAP_AM335X.iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            vec!["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4", "/dev/ttyS5"]
        );
    }

    #[test]
    fn am335x_table_lookup_resolves_all_four_chain_paths() {
        // `lookup_in()` MUST resolve every chain UART path against the
        // AM335x table. Anti-regression for the W13 runtime bug where
        // DevmemUart had only Zynq + CV1835 tables and `set_baud()`
        // returned "path not in MMIO map" on the AM335x chain UART paths.
        for path in ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS4", "/dev/ttyS5"] {
            let entry = lookup_in(UART_MMIO_MAP_AM335X, path)
                .unwrap_or_else(|| panic!("AM335x lookup must resolve {}", path));
            assert_eq!(entry.path, path);
            assert_eq!(entry.base_baud, 3_000_000);
            assert!(!entry.has_dlf);
        }
        // Negative: ttyS3 is unpopulated in the BB DTS — must NOT resolve.
        assert!(
            lookup_in(UART_MMIO_MAP_AM335X, "/dev/ttyS3").is_none(),
            "/dev/ttyS3 must NOT resolve (BB DTS leaves UART3 unpopulated)"
        );
        // Negative: the legacy `/dev/ttyO%d` names must NOT resolve anymore.
        for legacy in ["/dev/ttyO1", "/dev/ttyO2", "/dev/ttyO4", "/dev/ttyO5"] {
            assert!(
                lookup_in(UART_MMIO_MAP_AM335X, legacy).is_none(),
                "legacy `{}` must NOT resolve — the .79 probe confirmed `/dev/ttyS%d`",
                legacy
            );
        }
    }

    #[test]
    fn compute_divisor_zero_target_is_safe() {
        // Defensive: target_baud=0 must not divide-by-zero.
        let (integer, fractional) = compute_divisor(1_562_500, 0, true);
        assert_eq!(integer, 1);
        assert_eq!(fractional, 0);
        let (integer, fractional) = compute_divisor(6_249_999, 0, false);
        assert_eq!(integer, 1);
        assert_eq!(fractional, 0);
    }

    // ---------------------------------------------------------------------
    // R4 cold-boot UART RX=0 anti-regression pins.
    //
    // These tests are constant-pinning only — they don't open /dev/mem or
    // mmap. They ensure that the four register values DevmemUart::open_inner
    // writes during init match what stock bosminer / RE3 cold-boot trace
    // expects, so future refactors can't silently regress to the W1-4
    // "FPGA UART relay reg" dead end.
    //
    // Source: uart_relay_blocker3_5_analysis.md (R4 synthesis) — root causes
    //   #1 (MCR=0x00 leaves RS-485 transceiver disabled) and
    //   #2 (FCR=0x00 leaves UART in 1-byte non-FIFO mode that drops bytes).
    // ---------------------------------------------------------------------

    #[test]
    fn mcr_is_0x03_after_init() {
        // DTR (bit 0) + RTS (bit 1) must be asserted to power the off-chip
        // RS-485 transceiver on the BM1362 chain UARTs. MCR=0x00 = chain RX
        // silent on cold boot. Anti-regression for R4 root cause #1.
        assert_eq!(
            MCR_DTR_RTS, 0x03,
            "MCR_DTR_RTS must be 0x03 (DTR+RTS) — anything else leaves the \
             RS-485 transceiver disabled and the BM1362 chain UART silent"
        );
        // Pin offset too — 16550 standard puts MCR at 0x10. Moving it would
        // hit a reserved register and silently break.
        assert_eq!(UART_MCR, 0x10, "MCR offset must be 16550 standard 0x10");
    }

    #[test]
    fn fcr_is_0x07_after_init() {
        // ENABLE (0x01) | RX_RESET (0x02) | TX_RESET (0x04) = 0x07.
        // Without ENABLE, the 16550A operates in 1-byte non-FIFO mode — bytes
        // arrive faster than poll loop can drain → silent drops → "RX=0" on
        // cold boot. Anti-regression for R4 root cause #2.
        assert_eq!(
            FCR_ENABLE_RESET, 0x07,
            "FCR_ENABLE_RESET must be 0x07 (ENABLE|RX_RESET|TX_RESET) — \
             1-byte non-FIFO mode silently drops chain UART bytes"
        );
        assert_eq!(UART_FCR, 0x08, "FCR offset must be 16550 standard 0x08");
    }

    #[test]
    fn lcr_8n1_after_init() {
        // 8 data bits + no parity + 1 stop bit. BM1362 chain UART protocol
        // requires 8N1; any other framing produces 0 valid frames.
        assert_eq!(
            LCR_8N1, 0x03,
            "LCR_8N1 must be 0x03 — BM1362 chain UART requires 8N1 framing"
        );
        assert_eq!(UART_LCR, 0x0C, "LCR offset must be 16550 standard 0x0C");
        // DLAB stays bit 7 — set_baud raises it to access divisor latches,
        // then clears it by writing LCR=LCR_8N1.
        assert_eq!(LCR_DLAB, 0x80, "DLAB must be bit 7 of LCR");
    }

    #[test]
    fn dlf_is_0xab_after_init_at_25mhz_xtal() {
        // CV1835-only: DesignWare DW_apb_uart fractional latch at offset 0xC0.
        // For exact 937500 baud at the 25 MHz XTAL refclk used by the chain
        // UARTs, DLF must be 0xAB (171). This test pins both the value and
        // the offset so a future "consolidation" can't accidentally regress
        // to integer-only baud (which on CV1835 produces ~5% baud error,
        // enough to corrupt every byte in the 9-byte BM1362 frame format).
        let (integer, fractional) = compute_divisor(1_562_500, 937_500, /* has_dlf */ true);
        assert_eq!(integer, 1, "CV1835 integer divisor for 937500 baud");
        assert_eq!(
            fractional, 0xAB,
            "DLF for exact 937500 baud at 25 MHz XTAL must be 0xAB (171) — \
             integer-only divisor produces ~5% baud error"
        );
        assert_eq!(UART_DLF, 0xC0, "DLF offset must be DesignWare 0xC0");

        // Sanity: every CV1835 entry in the active table must opt into DLF.
        for entry in UART_MMIO_MAP_CV1835 {
            assert!(
                entry.has_dlf,
                "CV1835 entry {} must have has_dlf=true",
                entry.path
            );
            assert_eq!(
                entry.base_baud, 1_562_500,
                "CV1835 base_baud is 25 MHz / 16"
            );
        }
    }

    // ---------------------------------------------------------------------
    // AM2 PL UART MCR regression pins.
    //
    // MCR=0x03 remains the default because `a lab unit` first-shares proved the
    // R4 baseline. MCR=0x0B is also production-relevant: later `a lab unit` live
    // captures proved that XIL-25 PL UART TX is OUT2-gated, so correctly
    // stamped `a lab unit` units and explicit env overrides must select OUT2.
    // ---------------------------------------------------------------------

    #[test]
    fn wave25x_rollback_preserves_baseline_constants() {
        // IER baseline for polled-I/O DevmemUart: all interrupts disabled.
        assert_eq!(
            IER_POLLED_MODE, 0x00,
            "IER_POLLED_MODE must be 0x00 for the polled DevmemUart baseline; \
             IER=0x05 remains env-gated for bosminer parity diagnostics"
        );
        // MCR baseline: DTR+RTS only (no OUT2). Anti-regression for the
        // proven `a lab unit` first-shares state.
        assert_eq!(
            MCR_DTR_RTS, 0x03,
            "MCR_DTR_RTS must be 0x03 for the `a lab unit` SerialChainBackend baseline"
        );
        assert_eq!(
            MCR_DTR_RTS_OUT2, 0x0B,
            "MCR_DTR_RTS_OUT2 constant value pinned at 0x0B for XIL-25 \
             OUT2-gated PL UARTs and explicit operator overrides"
        );
    }

    #[test]
    fn xil25_board_fingerprint_selects_mcr_out2_without_env() {
        for board_target in ["am2-xil", "am2-s19jpro-xil", "am2-zynq-xil"] {
            assert_eq!(
                am2_mcr_out2_mode_for_inputs(
                    "/dev/ttyS1",
                    None,
                    Some("zynq-bm3-am2"),
                    Some(board_target),
                    None,
                ),
                Am2McrOut2Mode::Xil25Fingerprint,
                "board_target={board_target} must select OUT2 on Zynq AM2 PL UARTs"
            );
        }
    }

    #[test]
    fn non_xil25_boards_keep_mcr_baseline_without_env() {
        let cases = [
            ("zynq-bm3-am2", "am2-s19jpro"),
            ("zynq-bm3-am2", "am2-s19pro"),
            ("zynq-bm1-s9", "am1-s9"),
            ("am3-bb", "am3-bb-s19jpro"),
            ("amlogic-a113d", "am3-s19k"),
        ];

        for (platform, board_target) in cases {
            assert_eq!(
                am2_mcr_out2_mode_for_inputs(
                    "/dev/ttyS1",
                    None,
                    Some(platform),
                    Some(board_target),
                    None,
                ),
                Am2McrOut2Mode::Baseline,
                "platform={platform} board_target={board_target} must keep MCR=0x03"
            );
        }
    }

    #[test]
    fn mcr_out2_env_override_wins_both_directions() {
        assert_eq!(
            am2_mcr_out2_mode_for_inputs(
                "/dev/ttyS1",
                Some("1"),
                Some("zynq-bm3-am2"),
                Some("am2-s19jpro"),
                None,
            ),
            Am2McrOut2Mode::EnvOverride,
            "truthy env must force OUT2 on non-XIL-25 units"
        );

        for value in ["0", "off", "false", "FALSE", ""] {
            assert_eq!(
                am2_mcr_out2_mode_for_inputs(
                    "/dev/ttyS1",
                    Some(value),
                    Some("zynq-bm3-am2"),
                    Some("am2-xil"),
                    None,
                ),
                Am2McrOut2Mode::Baseline,
                "explicit env value {value:?} must force the baseline even on XIL-25"
            );
        }
    }

    #[test]
    fn xil25_fingerprint_only_applies_to_zynq_pl_uart_paths() {
        assert_eq!(
            am2_mcr_out2_mode_for_inputs(
                "/dev/ttyS0",
                None,
                Some("zynq-bm3-am2"),
                Some("am2-xil"),
                None,
            ),
            Am2McrOut2Mode::Baseline,
            "console UART must not inherit chain-UART OUT2 behavior"
        );
    }

    #[test]
    fn xil25_fingerprint_override_bridges_am2_s19j_package_identity() {
        assert_eq!(
            am2_mcr_out2_mode_for_inputs(
                "/dev/ttyS1",
                None,
                Some("zynq-bm3-am2"),
                Some("am2-s19j"),
                Some("1"),
            ),
            Am2McrOut2Mode::Xil25Fingerprint,
            "Wave56 .25 package-identity bridge must select OUT2 for am2-s19j only under explicit override"
        );
    }

    #[test]
    fn xil25_fingerprint_override_requires_zynq_am2_and_exact_package_identity() {
        for (platform, board_target, override_value) in [
            ("zynq-bm1-s9", "am2-s19j", "1"),
            ("zynq-bm3-am2", "am2-s19jpro", "1"),
            ("zynq-bm3-am2", "am2-s19j", "0"),
        ] {
            assert_eq!(
                am2_mcr_out2_mode_for_inputs(
                    "/dev/ttyS1",
                    None,
                    Some(platform),
                    Some(board_target),
                    Some(override_value),
                ),
                Am2McrOut2Mode::Baseline,
                "platform={platform} board_target={board_target} override={override_value} must not leak the XIL-25 bridge"
            );
        }
    }
}
