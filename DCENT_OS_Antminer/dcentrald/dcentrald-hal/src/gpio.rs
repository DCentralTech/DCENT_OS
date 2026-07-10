//! AXI GPIO controller.
//!
//! Direct AXI GPIO register access for hash board plug detection and board
//! enable/power control. Uses /dev/mem mmap for direct physical register
//! access — more reliable than sysfs GPIO on the Zynq 4.4.0 kernel.
//!
//! GPIO Pin Map (verified from live S9 probe + bmminer-mix RE):
//!
//! Input Register (0x41200000 + 0x00):
//!   Bit 5: J6 plug detect (HIGH = hash board present)
//!   Bit 6: J7 plug detect
//!   Bit 7: J8 plug detect
//!
//! Output Register (0x41210000 + 0x00):
//!   Bits 0-3: LEDs (D5-D8)
//!   Bits 9-11: Hash board enable (J6, J7, J8)
//!              HIGH = board powered/enabled, LOW = board disabled/reset
//!
//! Output Tristate (0x41210000 + 0x04):
//!   On production bitstream with xlnx,all-outputs=1, this register is
//!   hardware read-only — outputs always drive regardless of TRI value.

use std::num::NonZeroUsize;

use nix::sys::mman::{MapFlags, ProtFlags};

/// Physical base address of the AXI GPIO input controller.
pub const GPIO_INPUT_BASE: u32 = 0x4120_0000;

/// Physical base address of the AXI GPIO output controller.
pub const GPIO_OUTPUT_BASE: u32 = 0x4121_0000;

/// Data register offset.
pub const GPIO_DATA: u32 = 0x00;

/// Tristate control register offset.
pub const GPIO_TRI: u32 = 0x04;

/// Plug detect bit positions (input register).
pub const PLUG_DETECT_J6: u32 = 1 << 5;
pub const PLUG_DETECT_J7: u32 = 1 << 6;
pub const PLUG_DETECT_J8: u32 = 1 << 7;

/// Hash board RESET pin bit positions (output register).
/// HIGH = reset de-asserted (ASICs running), LOW = reset asserted (ASICs held in reset).
///
/// IMPORTANT: These are RESET lines, NOT power enable. The 12V power comes from
/// the PSU 6-pin Molex connectors and is always on. These GPIO pins drive the
/// hash board RESET signal on connector pin 13 (active-LOW). Bosminer's source
/// code calls them "reset_pin" and uses enter_reset()/exit_reset() to toggle them.
///
/// GPIO 893 = Chain 6 (J6), GPIO 894 = Chain 7 (J7), GPIO 895 = Chain 8 (J8)
/// Maps to AXI GPIO output register at 0x41210000, bits 9-11.
pub const BOARD_RESET_J6: u32 = 1 << 9;
pub const BOARD_RESET_J7: u32 = 1 << 10;
pub const BOARD_RESET_J8: u32 = 1 << 11;

/// All board reset bits.
pub const BOARD_RESET_ALL: u32 = BOARD_RESET_J6 | BOARD_RESET_J7 | BOARD_RESET_J8;

// Keep old names as aliases for backward compatibility in daemon.rs
pub const BOARD_ENABLE_J6: u32 = BOARD_RESET_J6;
pub const BOARD_ENABLE_J7: u32 = BOARD_RESET_J7;
pub const BOARD_ENABLE_J8: u32 = BOARD_RESET_J8;
pub const BOARD_ENABLE_ALL: u32 = BOARD_RESET_ALL;

/// LED identifiers.
///
/// S9 LEDs are Linux LED class devices under `/sys/class/leds/`.
/// D8 has no sysfs entry — it may be directly on AXI GPIO or absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    /// Green front LED (D5) — `/sys/class/leds/Green LED/`
    Green,
    /// Red front LED (D6) — `/sys/class/leds/Red LED/`
    Red,
    /// Internal red LED (heartbeat, D7) — `/sys/class/leds/Red LED (inside)/`
    RedInternal,
    /// D8 LED — no sysfs entry, writes are no-ops.
    D8,
}

impl Led {
    /// sysfs brightness path for this LED.
    fn sysfs_brightness_path(&self) -> &'static str {
        match self {
            Led::Green => "/sys/class/leds/Green LED/brightness",
            Led::Red => "/sys/class/leds/Red LED/brightness",
            Led::RedInternal => "/sys/class/leds/Red LED (inside)/brightness",
            Led::D8 => "/sys/class/leds/Red LED (inside)/brightness", // fallback, D8 not wired
        }
    }

    /// sysfs trigger path for this LED.
    fn sysfs_trigger_path(&self) -> &'static str {
        match self {
            Led::Green => "/sys/class/leds/Green LED/trigger",
            Led::Red => "/sys/class/leds/Red LED/trigger",
            Led::RedInternal => "/sys/class/leds/Red LED (inside)/trigger",
            Led::D8 => "/sys/class/leds/Red LED (inside)/trigger",
        }
    }
}

/// Size of each mmap region (one page).
const MMAP_SIZE: usize = 4096;

/// GPIO controller for hash board detection, enable/reset, and LED control.
pub struct GpioController {
    /// mmap'd pointer to the input GPIO register block (0x41200000).
    input_base: *mut u32,
    /// mmap'd pointer to the output GPIO register block (0x41210000).
    output_base: *mut u32,
}

// SAFETY: Same as UioDevice -- process-global mmap.
unsafe impl Send for GpioController {}
unsafe impl Sync for GpioController {}

impl GpioController {
    /// Create a new GPIO controller by mmapping the AXI GPIO registers via /dev/mem.
    pub fn new() -> crate::Result<Self> {
        let mem_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|e| crate::HalError::DeviceOpen {
                path: "/dev/mem".to_string(),
                source: e,
            })?;

        let page_size = NonZeroUsize::new(MMAP_SIZE).unwrap();

        // mmap GPIO input register block (0x41200000)
        let input_ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                page_size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &mem_file,
                GPIO_INPUT_BASE as nix::libc::off_t,
            )
            .map_err(|e| crate::HalError::MmapFailed {
                device: format!("gpio-input @ 0x{:08X}", GPIO_INPUT_BASE),
                source: e,
            })?
        };

        // mmap GPIO output register block (0x41210000)
        let output_ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                page_size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &mem_file,
                GPIO_OUTPUT_BASE as nix::libc::off_t,
            )
            .map_err(|e| crate::HalError::MmapFailed {
                device: format!("gpio-output @ 0x{:08X}", GPIO_OUTPUT_BASE),
                source: e,
            })?
        };

        tracing::debug!(
            "GPIO controller initialized via /dev/mem (input @ 0x{:08X}, output @ 0x{:08X})",
            GPIO_INPUT_BASE,
            GPIO_OUTPUT_BASE,
        );

        Ok(Self {
            input_base: input_ptr.as_ptr() as *mut u32,
            output_base: output_ptr.as_ptr() as *mut u32,
        })
    }

    /// Read hash board plug detect state.
    ///
    /// Returns [J6_present, J7_present, J8_present].
    pub fn read_plug_detect(&self) -> [bool; 3] {
        let data = unsafe { std::ptr::read_volatile(self.input_base) };
        [
            data & PLUG_DETECT_J6 != 0,
            data & PLUG_DETECT_J7 != 0,
            data & PLUG_DETECT_J8 != 0,
        ]
    }

    /// Assert or release the hash board RESET line.
    ///
    /// `chain`: chain index (0=J6, 1=J7, 2=J8)
    /// `enable`: true = release reset / enable (HIGH), false = assert reset (LOW)
    ///
    /// This controls the ASIC chain RESET signal, NOT power. The 12V power is
    /// always present from the PSU. Asserting reset (LOW) holds all ASIC chips
    /// in their default state. Releasing reset (HIGH) lets them boot.
    pub fn set_board_enable(&self, chain: u8, enable: bool) {
        let bit = match chain {
            0 => BOARD_RESET_J6,
            1 => BOARD_RESET_J7,
            2 => BOARD_RESET_J8,
            _ => return,
        };

        let current = unsafe { std::ptr::read_volatile(self.output_base) };
        let new = if enable {
            current | bit
        } else {
            current & !bit
        };
        unsafe { std::ptr::write_volatile(self.output_base, new) };
    }

    /// Assert or release ALL hash board RESET lines at once.
    pub fn set_all_boards_enable(&self, enable: bool) {
        let current = unsafe { std::ptr::read_volatile(self.output_base) };
        let new = if enable {
            current | BOARD_RESET_ALL
        } else {
            current & !BOARD_RESET_ALL
        };
        unsafe { std::ptr::write_volatile(self.output_base, new) };
    }

    /// Assert reset on a hash board (active LOW — holds ASICs in reset state).
    ///
    /// This is the enter_reset() equivalent from bosminer. The ASIC chips
    /// are held in reset but 12V power remains on. PIC voltage controllers
    /// continue running on the 3.3V rail.
    pub fn enter_reset(&self, chain: u8) {
        self.set_board_enable(chain, false);
    }

    /// Release reset on a hash board (HIGH — ASICs begin booting).
    ///
    /// This is the exit_reset() equivalent from bosminer. After releasing
    /// reset, ASICs need ~1 second to boot and become responsive to UART
    /// commands at 115200 baud.
    pub fn exit_reset(&self, chain: u8) {
        self.set_board_enable(chain, true);
    }

    /// Assert reset on ALL hash boards.
    pub fn enter_reset_all(&self) {
        self.set_all_boards_enable(false);
    }

    /// Release reset on ALL hash boards.
    pub fn exit_reset_all(&self) {
        self.set_all_boards_enable(true);
    }

    /// Read the raw output register value.
    pub fn read_output(&self) -> u32 {
        unsafe { std::ptr::read_volatile(self.output_base) }
    }

    /// Read the raw input register value.
    pub fn read_input(&self) -> u32 {
        unsafe { std::ptr::read_volatile(self.input_base) }
    }

    /// Set an LED on or off via Linux LED sysfs interface.
    ///
    /// S9 LEDs are registered as `/sys/class/leds/{name}/brightness`.
    /// Writing 0 = OFF, 255 = ON. The trigger must be set to "none"
    /// first to take manual control (done once in `init_leds()`).
    pub fn set_led(&self, led: Led, on: bool) {
        let path = led.sysfs_brightness_path();
        let val = if on { b"255" as &[u8] } else { b"0" };
        // Best-effort write — LED control is non-critical
        let _ = std::fs::write(path, val);
    }

    /// Read the current logical state of an LED (true = lit).
    pub fn read_led(&self, led: Led) -> bool {
        let path = led.sysfs_brightness_path();
        match std::fs::read_to_string(path) {
            Ok(s) => s.trim().parse::<u32>().unwrap_or(0) > 0,
            Err(_) => false,
        }
    }

    /// Toggle an LED (read current state, write the opposite).
    pub fn toggle_led(&self, led: Led) {
        let on = self.read_led(led);
        self.set_led(led, !on);
    }

    /// Take manual control of all LEDs by setting trigger to "none".
    /// Must be called once at startup before any set_led() calls.
    pub fn init_leds(&self) {
        for led in &[Led::Green, Led::Red, Led::RedInternal] {
            let path = led.sysfs_trigger_path();
            let _ = std::fs::write(path, b"none");
        }
        // Start with all LEDs off
        self.set_led(Led::Green, false);
        self.set_led(Led::Red, false);
        self.set_led(Led::RedInternal, false);
        tracing::info!("LED sysfs control initialized (triggers set to none)");
    }

    /// Enable all GPIO outputs by clearing the tristate register.
    ///
    /// NOTE: On the production S9 bitstream (xlnx,all-outputs=1), the TRI
    /// register is hardware read-only and outputs always drive. This call
    /// is a no-op on those boards but included for completeness.
    pub fn enable_outputs(&self) {
        let tri_reg = unsafe { self.output_base.add(1) }; // +0x04 = TRI register
        unsafe { std::ptr::write_volatile(tri_reg, 0x0000_0000) };
    }
}

impl Drop for GpioController {
    fn drop(&mut self) {
        // GPIO mmaps are process-lifetime resources. Let the kernel reclaim
        // them at exit instead of touching device-backed mappings during the
        // very end of shutdown.
    }
}
