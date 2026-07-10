//! am2-s17 board-control IP (@ 0x42810000, uio17).
//!
//! Exposed on the am2 control board (S17/T17/S19/S19j Pro — Zynq variants).
//! Provides:
//!   - Per-hashboard reset pins (HB0_RESET .. HB3_RESET)
//!   - Plug-detect inputs (read via sysfs GPIO 902..905 on this platform — see
//!     [`read_plug_detect`] for the FPGA-register path plus a sysfs fallback)
//!   - PSU hardware-enable pin (PWR_CONTROL — belt-and-suspenders alongside
//!     the PSU I2C driver's software-disable path)
//!   - PSU I2C bit-bang pinout (PWR_I2C2_SDA / PWR_I2C2_SCL) — the am2 PSU
//!     I2C is routed through this IP and can be bit-banged when the kernel
//!     AXI-IIC is in a bad state. This crate only exposes the register field
//!     constants; the actual bit-bang driver lives in `psu_gpio_i2c.rs`.
//!   - Fan control-board mode selection (C49 1-PWM vs C52 2-PWM).
//!
//! ## Register field names (from bosminer.bin strings, line 3627)
//!
//! `PWR_CONTROL HB0_RESET HB1_RESET HB2_RESET HB3_RESET PWR_I2C2_SDA PWR_I2C2_SCL`
//!
//! These are the bitfield names in the BraiinsOS Rust register block binding
//! for uio17. Exact bit positions are not published in the strings binary;
//! the offsets below are derived from the live probe (2026-04-20, .139):
//!
//!   +0x00 : 0x00000701 — HW revision / straps (board ID register)
//!   +0x04 : 0x00000134 — additional board-ID byte
//!   +0x20..+0x24 : mirrors of 0x00..0x04 (AXI-lite byte-enable mirroring)
//!
//! ## Where reset / enable actually live
//!
//! The reset pin on am2 is **NOT** exposed via uio17's +0x00/+0x04 probe
//! window. The BraiinsOS `+0x00` register is board ID; `+0x04` is the
//! C49/C52 fan-mode selector and is intentionally writable for that low byte.
//! `bosminer-am2-s17/src/hardware/hashboard/power.rs` uses sysfs GPIO 898/899
//! plus MIO pins for reset and enable. PLUG_DETECT is GPIO 902..905.
//!
//! This module therefore uses a **hybrid** approach:
//!   - Read board-ID / fan-mode values from uio17 registers for diagnostics.
//!   - Switch fan mode to C52 before commanding 0x10/0x14 PWM.
//!   - Drive reset / plug-detect / PSU enable via sysfs GPIO (which is the
//!     path BraiinsOS and the Zynq platform probe already use).
//!
//! ## SAFETY
//!
//! Writing to the uio17 window is risky — the bit positions for PWR_CONTROL
//! and HB*_RESET within the register are NOT live-probed yet. Until they are
//! RE'd from the boser-openwrt source or captured via strace on a live
//! BraiinsOS unit, this module intentionally keeps the reset / PSU-enable
//! paths on sysfs GPIO. See the [`pulse_reset`] / [`psu_enable`] implementations.

use std::fs;
use std::num::NonZeroUsize;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use nix::sys::mman::{MapFlags, ProtFlags};

use crate::uio::UioDevice;
use crate::{HalError, Result};

// ---------------------------------------------------------------------------
// Physical map
// ---------------------------------------------------------------------------

/// Board-control IP base (am2-s17 only).
pub const BOARD_CONTROL_BASE_ADDR: u32 = 0x4281_0000;

/// Board-ID straps (read-only, live probe: 0x701 on .139).
pub const REG_BOARD_ID_0: u32 = 0x00;
/// Control-board fan mode register.
///
/// Live-proven on `a lab unit`: low byte `0x31` = C49 1-PWM mode; low byte `0x34` =
/// C52 2-PWM mode. Some boards report extra high bits (e.g. `0x134`), so mode
/// writes preserve everything except the low byte.
pub const REG_CONTROL_BOARD_MODE: u32 = 0x04;
/// Legacy alias retained for older call sites/docs; this register is not just
/// an ID byte.
pub const REG_BOARD_ID_1: u32 = REG_CONTROL_BOARD_MODE;

/// Braiins control-board C49 mode: 1 PWM output.
pub const CONTROL_BOARD_MODE_C49: u32 = 0x31;
/// Braiins control-board C52 mode: 2 PWM outputs.
pub const CONTROL_BOARD_MODE_C52: u32 = 0x34;
/// Mask for the low-byte board mode value.
pub const CONTROL_BOARD_MODE_MASK: u32 = 0xFF;

/// Bit 8 of the control-board mode register (`+0x04`).
///
/// ## Source: live W2 register diff (2026-06-14, `a lab unit`, bosminer-engaged vs
/// DCENT standalone enum=0)
///
/// A paired same-script `dump_fpga_regs_25.sh` capture proved the ONLY
/// persistent-fabric register DCENT never matches is this one:
///   - bosminer ACTIVELY MINING : `+0x04 = 0x00000134`
///   - DCENT standalone enum=0   : `+0x04 = 0x00000034`
/// The delta is bit 8 (`0x100`), consistent across all ~32 AXI-lite aliases of
/// the register (0x...004, 0x...024 … 0x...3e4), and it **LATCHES** — it stayed
/// `0x134` after `killall -9 bosminer`. The original am2 `a lab unit` live probe
/// (2026-04-20) also read `0x134` natively, so `0x134` is a known-good am2 board
/// state. [`enable_c52_fan_mode`] only ever rewrites the low byte, so on a cold
/// `a lab unit` (where `+0x04` powers up at `0x034`) DCENT keeps bit 8 CLEAR and never
/// sets it; bosminer sets it during chain bring-up. Candidate `a lab unit` standalone
/// `enum=0` fix — gated + `a lab unit`-fingerprinted at the call site.
pub const CONTROL_BOARD_MODE_BIT8: u32 = 0x100;

/// Result of switching the AM2 control board fan mux to C52 mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct C52FanModeStatus {
    /// Raw +0x04 value before the write.
    pub before: u32,
    /// Raw +0x04 value after the write/readback.
    pub after: u32,
}

// ---------------------------------------------------------------------------
// sysfs GPIO assignments (am2-s17 control board — BraiinsOS convention)
// ---------------------------------------------------------------------------

/// Plug-detect GPIOs — one per chain slot. `[slot0, slot1, slot2, slot3]`.
/// Live probe on .139: 902=low (empty), 903=high (present), 904=high (present),
/// 905=low (empty) — maps to chain1+chain4 active. slot0/slot3 here mirror the
/// FPGA-chain 1/4 indices.
pub const AM2_PLUG_DETECT_GPIOS: [u32; 4] = [902, 903, 904, 905];

/// Reset-pulse GPIOs (one per chain slot) — write 0 to assert reset, 1 to release.
/// Current best evidence for am2 `a lab unit` points at the `gpio@41210000` bank,
/// where the DT label order is `HB0_RESET .. HB3_RESET, PWR_CONTROL` starting
/// from base 897. Keep the numbered fallback aligned to that live evidence so
/// direct-path experiments have a sane fallback even when the DT label blob is
/// unavailable at runtime.
pub const AM2_RESET_GPIOS: [u32; 4] = [897, 898, 899, 900];

/// PSU hardware-enable GPIO (PWR_CONTROL). Same as am2 platform const in
/// zynq.rs — this is a belt-and-suspenders companion to the PSU I2C driver.
pub const AM2_PSU_ENABLE_GPIO: u32 = 907;

// ---------------------------------------------------------------------------
// am2 PL GPIO bank 0x41210000 — live-probe layout
// ---------------------------------------------------------------------------
//
// The Xilinx xps-gpio-1.00.a bank at 0x41210000 owns lines 897..901 (base=897,
// ngpio=5). On S19j Pro .139 the live DT + xlnx,dout-default produces:
//
//   bit 0  = gpio-897  = HB0_RESET     (sysfs-owned, active-LOW)
//   bit 1  = gpio-898  = HB1_RESET     (sysfs-owned, active-LOW)
//   bit 2  = gpio-899  = HB2_RESET     (sysfs-owned, active-LOW)
//   bit 3  = gpio-900  = HB3_RESET     (not sysfs-exported; DT default LOW)
//   bit 4  = gpio-901  = PWR_CONTROL   (kernel-claimed; FPGA-default HIGH via
//                                       xlnx,dout-default = 0x10)
//
// ## The sysfs RMW clobber bug
//
// Live test on `a lab unit` (2026-04-24): any sysfs write to gpio-897..900 causes the
// Xilinx xps-gpio driver to issue a full 32-bit store to this register using an
// INTERNAL cache that treats unexported lines (gpio-901) as "default 0". Result:
// every sysfs reset pulse on HB*_RESET flips gpio-901 HIGH → LOW, which kills
// PSU PWR_CONTROL for the rest of the session.
//
// Workaround: do the reset pulse via /dev/mem with a read-modify-write that
// preserves bit 4 (gpio-901 HIGH). This keeps PWR_CONTROL asserted across the
// pulse without requiring us to claim gpio-901 from the kernel.
pub const AM2_GPIO_OUT_CHIP_BASE: u32 = 0x4121_0000;
pub const AM2_GPIO_OUT_CHIP_FIRST: u32 = 897;
pub const AM2_GPIO_OUT_CHIP_LAST: u32 = 901;
/// PWR_CONTROL bit within the 0x41210000 output register. Always preserved
/// HIGH by the devmem reset path below.
pub const AM2_GPIO_OUT_PWR_CONTROL_BIT: u32 = 1 << 4;

/// Tristate / direction register offset within the AM2 PL GPIO bank
/// (`0x41210000 + 0x04`). On a Xilinx `axi_gpio` core a SET bit = the line is
/// an INPUT (tristated, high-Z); a CLEAR bit = the line drives as an OUTPUT.
///
/// This MUST stay in sync with [`crate::gpio::GPIO_TRI`] (`0x04`).
pub const AM2_GPIO_OUT_TRI: u32 = 0x04;

/// Default-OFF env gate (H-hbreset-tri, swarm `wf_e0647147` 2026-05-29) for the
/// optional TRI/direction-clear before the [`hold_am2_reset_devmem`] DATA
/// drive.
///
/// ## Why this exists
///
/// The legacy devmem reset path only ever wrote the DATA register at offset
/// `0x00`. If the FPGA bitstream left the HB*_RESET lines tristated (TRI bit
/// SET = input), the DATA write reaches a high-Z pin and the BM1362 reset wire
/// floats — a candidate cause of `a lab unit` standalone `enum=0`. When this gate is
/// set, the devmem path first CLEARS the TRI bits for the reset line +
/// `AM2_GPIO_OUT_PWR_CONTROL_BIT` (forcing OUTPUT) and then reads `+0x04` back
/// so the result is falsifiable: a log line shows the TRI value before/after
/// and whether the requested bits actually cleared. On a production bitstream
/// built with `xlnx,all-outputs=1` the TRI register is hardware read-only, so
/// the readback will show the bits UNCHANGED — that is the diagnostic signal
/// that the float hypothesis is NOT the blocker on this unit.
///
/// ## Safety
///
/// Default-OFF. When unset, [`hold_am2_reset_devmem`] is byte-for-byte
/// identical to before (no TRI access at all) so the proven fleet
/// (`a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/s9) and the  `a lab unit` bosminer-handoff path
/// do not change on the wire. The TRI write only ever clears the SAME bits the
/// DATA RMW already drives (the reset bit + the always-preserved PWR_CONTROL
/// bit); it never widens the set of driven lines and never touches EEPROM I2C.
///
/// NOTE: this fixes the *devmem* drive path specifically. sysfs export of
/// gpio897..900 (needed for the sysfs long-hold path in `s19j_hybrid_mining`)
/// is handled independently by the AM2 branch of `dcentos-early-init.sh`
/// () and is NOT required for this mmap'd register write to drive.
pub const AM2_HB_RESET_SET_TRI_ENV: &str = "DCENT_AM2_HB_RESET_SET_TRI";

/// Read the [`AM2_HB_RESET_SET_TRI_ENV`] gate (default-OFF). Mirrors the truthy
/// set used by `am2_env_flag` in `s19j_hybrid_mining.rs` and the other HAL-scope
/// env gates so operator launchers behave consistently.
fn am2_hb_reset_set_tri_enabled() -> bool {
    matches!(
        std::env::var(AM2_HB_RESET_SET_TRI_ENV).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

/// Default-OFF env gate (R5 sysfs-reset re-point, 2026-05-31) that swings the
/// am2 multi-slot reset drive from the `/dev/mem` DATA RMW at `0x41210000` over
/// to a PLAIN-KERNEL-SYSFS active-LOW assert→hold→release PULSE on the HBx_RESET
/// lines (`/sys/class/gpio/gpio{897..900}/{direction,value}`).
///
/// ## Why this exists (the RE result, NOT a hypothesis)
///
/// A cold bosminer capture on `a lab unit` proved bosminer wakes the BM1362 chain reset
/// via plain kernel sysfs — `direction=out` → `value=0` (assert, active-LOW) →
/// hold ~1.8 s → `value=1` (release), on gpio897 (HB0, slot 0) + gpio899 (HB2,
/// slot 2), the two POPULATED slots on `a lab unit`. The am2 kernel has
/// CONFIG_GPIO_CDEV OFF / only CONFIG_GPIO_SYSFS=y
///, so sysfs is the only viable path.
/// PWR_CONTROL is gpio907 on the SEPARATE Zynq PS bank (e000a000), driven via
/// sysfs by `psu_enable()`, so a sysfs write to gpio897..900 CANNOT clobber it —
/// the old gpio-901-clobber rationale (which motivated the devmem RMW
/// workaround) is moot for the sysfs LOW→hold→HIGH PULSE bosminer does. DCENT
/// historically drove reset via the devmem DATA RMW and a sysfs FORCE-HIGH
/// (release) fallback, but has NEVER done bosminer's active sysfs
/// LOW(assert)→hold→HIGH(release) reset pulse.
///
/// ## Safety
///
/// Default-OFF. When unset, the reset dispatcher
/// ([`BoardControl::hold_resets_devmem`]) runs the existing devmem RMW path
/// byte-for-byte identically, so the proven fleet (`a lab unit`/`a lab unit`/`a lab unit`/`a lab unit`/s9)
/// and the  `a lab unit` bosminer-handoff path are unchanged on the wire. Only
/// the `a lab unit` standalone launcher sets this. The sysfs pulse drives the SAME
/// HBx_RESET lines the devmem path drives; it never touches EEPROM I2C, never
/// touches PWR_CONTROL (gpio907 is a different bank), and `pulse_reset()`'s
/// non-am2 / proven-fleet branches are untouched.
pub const AM2_HB_RESET_REPOINT_ENV: &str = "DCENT_AM2_HB_RESET_REPOINT";

/// Read the [`AM2_HB_RESET_REPOINT_ENV`] gate (default-OFF). Mirrors the truthy
/// set used by `am2_hb_reset_set_tri_enabled` / `am2_env_flag` so operator
/// launchers behave consistently.
fn am2_hb_reset_repoint_enabled() -> bool {
    matches!(
        std::env::var(AM2_HB_RESET_REPOINT_ENV).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

/// Bosminer-faithful cold HBx_RESET LOW dwell for the sysfs re-point pulse
/// (R5, 2026-05-31). The cold bosminer capture held reset LOW ~1.8 s before
/// release; the repoint path uses `max(am2_reset_hold_ms, this)` so a shorter
/// configured hold can never starve the cold-wake pulse.
const AM2_HB_RESET_REPOINT_MIN_HOLD_MS: u64 = 1800;

/// `(dt gpio-line-names path, Linux global GPIO base)` pairs for the am2 board.
/// Shared with the PSU gate helper's live-probed base addresses.
const DT_GPIO_LABEL_SOURCES: &[(&str, u32)] = &[
    (
        "/sys/firmware/devicetree/base/amba/gpio@41220000/gpio-line-names",
        895,
    ),
    (
        "/sys/firmware/devicetree/base/amba/gpio@41210000/gpio-line-names",
        897,
    ),
    (
        "/sys/firmware/devicetree/base/amba/gpio@41200000/gpio-line-names",
        902,
    ),
    (
        "/sys/firmware/devicetree/base/gpio@e000a000/gpio-line-names",
        906,
    ),
    (
        "/sys/firmware/devicetree/base/amba_ps/gpio@e000a000/gpio-line-names",
        906,
    ),
];

const RESET_LABELS: [&str; 4] = ["HB0_RESET", "HB1_RESET", "HB2_RESET", "HB3_RESET"];

// ---------------------------------------------------------------------------
// BoardControl wrapper
// ---------------------------------------------------------------------------

/// am2-s17 board-control IP wrapper.
///
/// Manages uio17 (read-only diagnostic window) plus the sysfs GPIO pins that
/// govern hashboard reset, plug detection, and PSU hardware enable.
pub struct BoardControl {
    /// Mapped uio17 region (board-ID straps, diagnostic only for now).
    regs: UioDevice,
}

impl BoardControl {
    /// Open the board-control device.
    ///
    /// `uio_number` is typically 17 on am2 (see `/sys/class/uio/uio17/name
    /// == board-control`). The actual number is discovered by
    /// `zynq::ZynqPlatform::new()` and passed in.
    pub fn open(uio_number: u8) -> Result<Self> {
        let regs = UioDevice::open(uio_number)?;
        Ok(Self { regs })
    }

    /// Read the two board-ID straps. Useful for telemetry / sanity checks.
    pub fn read_board_id(&self) -> (u32, u32) {
        (
            self.regs.read_reg(REG_BOARD_ID_0),
            self.regs.read_reg(REG_BOARD_ID_1),
        )
    }

    /// Read the raw control-board fan mode register.
    pub fn read_control_board_mode(&self) -> u32 {
        self.regs.read_reg(REG_CONTROL_BOARD_MODE)
    }

    /// Switch the AM2 control board into C52 2-PWM fan mode.
    ///
    /// This is the live-proven `a lab unit` fan fix: C49 mode (`0x31`) leaves the
    /// front pair outside the normal `fan-control` tach/PWM path; C52 mode
    /// (`0x34`) exposes all four tach channels and lets 0x10/0x14 control the
    /// two fan pairs independently. High bits are preserved because some
    /// boards report `0x134` rather than bare `0x34`.
    pub fn enable_c52_fan_mode(&self) -> Result<C52FanModeStatus> {
        let before = self.read_control_board_mode();
        let desired = (before & !CONTROL_BOARD_MODE_MASK)
            | (CONTROL_BOARD_MODE_C52 & CONTROL_BOARD_MODE_MASK);
        self.regs.write_reg(REG_CONTROL_BOARD_MODE, desired);
        let after = self.read_control_board_mode();
        if (after & CONTROL_BOARD_MODE_MASK) != CONTROL_BOARD_MODE_C52 {
            return Err(HalError::Fan(format!(
                "board-control C52 fan-mode write failed: before=0x{before:08X} desired=0x{desired:08X} after=0x{after:08X}"
            )));
        }
        Ok(C52FanModeStatus { before, after })
    }

    /// Set bit 8 ([`CONTROL_BOARD_MODE_BIT8`]) of the control-board mode register
    /// (`+0x04`), preserving every other bit (incl. the C52 fan-mode low byte).
    ///
    /// This is the live W2-diff-justified `a lab unit` standalone candidate: bosminer
    /// drives `+0x04 = 0x134` while DCENT only ever sets `0x034`. The write is a
    /// pure read-modify-write OR of bit 8 — it never clears the fan-mode byte or
    /// any other bit — so calling it after [`enable_c52_fan_mode`] keeps C52 mode
    /// AND adds bit 8 (final `0x134`, byte-identical to bosminer-engaged).
    ///
    /// Returns the before/after raw `+0x04` values and errors (without
    /// retrying) if the readback shows bit 8 did not latch. SAFE: writes only the
    /// register DCENT already controls for the fan mode; never touches EEPROM I2C,
    /// PWR_CONTROL, or any reset line. Default-OFF + `a lab unit`-fingerprint gated at the
    /// call site, so the proven fleet /  handoff path are unchanged on the
    /// wire.
    pub fn set_control_board_mode_bit8(&self) -> Result<C52FanModeStatus> {
        let before = self.read_control_board_mode();
        let desired = before | CONTROL_BOARD_MODE_BIT8;
        self.regs.write_reg(REG_CONTROL_BOARD_MODE, desired);
        let after = self.read_control_board_mode();
        if (after & CONTROL_BOARD_MODE_BIT8) == 0 {
            return Err(HalError::Fan(format!(
                "board-control bit8 set failed: before=0x{before:08X} desired=0x{desired:08X} after=0x{after:08X}"
            )));
        }
        Ok(C52FanModeStatus { before, after })
    }

    /// Read hashboard plug-detect state for the 4 am2 slots.
    ///
    /// Returns `[slot0, slot1, slot2, slot3]`. `true` = board present.
    ///
    /// Uses sysfs GPIO (which BraiinsOS and DCENT_OS platform probe already
    /// rely on). Falls back to `false` for any GPIO that is not exported or
    /// cannot be read.
    pub fn read_plug_detect(&self) -> [bool; 4] {
        let mut out = [false; 4];
        for (i, &gpio) in AM2_PLUG_DETECT_GPIOS.iter().enumerate() {
            out[i] = read_sysfs_gpio(gpio).unwrap_or(false);
        }
        out
    }

    /// Pulse a hashboard reset line on the given slot (0..=3).
    ///
    /// Asserts reset (drives low) for ~20 ms, then releases. Follows the
    /// Mining Bible v1 Phase A2 spec (`mining-bible-v1/1-power-dspic/02-state-machine.md`)
    /// and bosminer's `gpiod-0.2.3` HBx_RESET contract (Phase 13D Ghidra).
    ///
    /// Path selection:
    ///   1. **libgpiod chardev** (`/dev/gpiochipN`) — bosminer's contract on
    ///      Zynq XIL. Resolves the line by DT label `HB[0-3]_RESET` first,
    ///      falling back to global GPIO number. Kernel-tracked consumer
    ///      handle preserves pinmux state and lets the kernel drive the
    ///      output register correctly even when other lines on the same bank
    ///      are kernel-claimed (gpio-901 PWR_CONTROL).
    ///   2. **devmem RMW** (legacy) — drives the AM2 PL GPIO bank @0x41210000
    ///      directly via `/dev/mem`, preserving gpio-901 HIGH across the
    ///      pulse. Used only as fallback when libgpiod is unavailable.
    ///   3. **sysfs** (legacy) — for non-am2 platforms (e.g. AML
    ///      gpio454-456, where sysfs is the canonical path).
    pub fn pulse_reset(&self, slot: u8) -> Result<()> {
        if (slot as usize) >= AM2_RESET_GPIOS.len() {
            return Err(HalError::Platform(format!(
                "board-control: reset slot {} out of range (0..{})",
                slot,
                AM2_RESET_GPIOS.len()
            )));
        }
        let gpio = resolve_reset_gpio(slot).unwrap_or(AM2_RESET_GPIOS[slot as usize]);
        let label = RESET_LABELS[slot as usize];

        // Path 1: libgpiod chardev (Bible-canonical for XIL am2).
        // Try DT-label resolution first (most robust), then global GPIO mapping.
        if let Ok(Some((chip_path, offset))) = lookup_chardev_for_label(label) {
            match crate::libgpiod::pulse_output(
                &chip_path,
                offset,
                "dcentrald-HBx_RESET",
                Duration::from_millis(20),
                true,
            ) {
                Ok(()) => {
                    tracing::info!(
                        slot,
                        gpio,
                        label,
                        chip = %chip_path.display(),
                        offset,
                        method = "libgpiod-label",
                        "am2 hashboard reset pulsed"
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        slot,
                        label,
                        error = %e,
                        "libgpiod-by-label HBx_RESET pulse failed; falling back to gpio-number path"
                    );
                }
            }
        }

        if let Ok(Some((chip_path, offset))) = crate::libgpiod::resolve_global_gpio(gpio) {
            match crate::libgpiod::pulse_output(
                &chip_path,
                offset,
                "dcentrald-HBx_RESET",
                Duration::from_millis(20),
                true,
            ) {
                Ok(()) => {
                    tracing::info!(
                        slot,
                        gpio,
                        chip = %chip_path.display(),
                        offset,
                        method = "libgpiod-gpio",
                        "am2 hashboard reset pulsed"
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        slot,
                        gpio,
                        error = %e,
                        "libgpiod-by-gpio HBx_RESET pulse failed; falling back to devmem RMW"
                    );
                }
            }
        }

        // Path 2: devmem RMW fallback for am2 PL GPIO bank @0x41210000.
        // Preserves gpio-901 PWR_CONTROL HIGH across the pulse (the kernel
        // xps-gpio sysfs driver would clobber it via 32-bit cache write).
        if (AM2_GPIO_OUT_CHIP_FIRST..=AM2_GPIO_OUT_CHIP_LAST).contains(&gpio) {
            let bit = 1u32 << (gpio - AM2_GPIO_OUT_CHIP_FIRST);
            // Preserve the historical 10 ms short-pulse contract for the proven
            // fleet's `pulse_reset()` path ( parameterized the dwell).
            hold_am2_reset_devmem(bit, 10)?;
            tracing::info!(slot, gpio, method = "devmem", "am2 hashboard reset pulsed");
            return Ok(());
        }

        // Path 3: sysfs (legacy / non-am2 chips, e.g. AML gpio454-456).
        ensure_sysfs_gpio_exported(gpio)?;
        write_sysfs_gpio_direction(gpio, "out")?;
        write_sysfs_gpio_value(gpio, false)?; // assert LOW
        sleep(Duration::from_millis(20));
        write_sysfs_gpio_value(gpio, true)?; // release HIGH
        tracing::info!(slot, gpio, method = "sysfs", "am2 hashboard reset pulsed");
        Ok(())
    }

    /// Hold a single hashboard reset line LOW for `hold_ms` via the proven
    /// `/dev/mem` AXI-GPIO bank at `0x41210000`, then release HIGH.
    ///
    /// This is the long-hold companion to [`pulse_reset`] for the `a lab unit`
    /// standalone Phase-2b-extended path (). It deliberately does NOT
    /// use sysfs: on `a lab unit` the kernel claims gpio-898/900, so they are not
    /// `sysfs`-exportable (ENOENT on `/sys/class/gpio/export`) and the sysfs
    /// long-hold never fires — chains never reset → chain enum returns 0. The
    /// devmem RMW path preserves `AM2_GPIO_OUT_PWR_CONTROL_BIT` (gpio-901) HIGH
    /// across the entire hold so the PSU rail is never dropped.
    ///
    /// `slot` is 0..=3, mapped to its reset bit via `AM2_RESET_GPIOS` and
    /// `AM2_GPIO_OUT_CHIP_FIRST` (bit = `1 << (gpio - AM2_GPIO_OUT_CHIP_FIRST)`).
    /// Returns an error if the slot is out of range or its reset GPIO is not on
    /// the PL GPIO bank (897..=901).
    pub fn hold_reset_devmem(&self, slot: u8, hold_ms: u64) -> Result<()> {
        if (slot as usize) >= AM2_RESET_GPIOS.len() {
            return Err(HalError::Platform(format!(
                "board-control: reset slot {} out of range (0..{})",
                slot,
                AM2_RESET_GPIOS.len()
            )));
        }
        let gpio = AM2_RESET_GPIOS[slot as usize];
        if !(AM2_GPIO_OUT_CHIP_FIRST..=AM2_GPIO_OUT_CHIP_LAST).contains(&gpio) {
            return Err(HalError::Platform(format!(
                "board-control: reset slot {} (gpio {}) is not on the PL GPIO bank \
                 0x{:08X} (897..=901) — devmem hold not applicable",
                slot, gpio, AM2_GPIO_OUT_CHIP_BASE
            )));
        }
        let bit = 1u32 << (gpio - AM2_GPIO_OUT_CHIP_FIRST);
        hold_am2_reset_devmem(bit, hold_ms)?;
        tracing::info!(
            slot,
            gpio,
            hold_ms,
            method = "devmem",
            "am2 hashboard reset held (single slot, AXI-GPIO 0x41210000)"
        );
        Ok(())
    }

    /// Hold MULTIPLE hashboard reset lines LOW **simultaneously** for `hold_ms`
    /// via the `/dev/mem` AXI-GPIO bank at `0x41210000`, then release them all
    /// HIGH together.
    ///
    /// This replaces the `s19j_hybrid_mining` Phase-2b-extended sysfs sequence
    /// that held slots 1 AND 3 LOW at the same time (so both BM1362 chains reset
    /// in lockstep, not serialized). The devmem RMW path works on `a lab unit` where
    /// the kernel claims gpio-898/900 and sysfs export fails. PWR_CONTROL
    /// (gpio-901, bit 4) stays HIGH across the whole hold.
    ///
    /// Each slot is 0..=3. Slots whose reset GPIO is not on the PL GPIO bank
    /// (897..=901) — or out of range — cause an error before any write so the
    /// rail is never touched on an invalid request.
    pub fn hold_resets_devmem(&self, slots: &[u8], hold_ms: u64) -> Result<()> {
        if slots.is_empty() {
            return Err(HalError::Platform(
                "board-control: hold_resets_devmem called with no slots".to_string(),
            ));
        }

        // R5 sysfs-reset re-point (default-OFF, `DCENT_AM2_HB_RESET_REPOINT`):
        // when the gate is set (only the `a lab unit` standalone launcher does), drive
        // the reset PULSE via bosminer's plain-kernel-sysfs active-LOW
        // assert→hold→release on gpio897..900 INSTEAD of the devmem DATA RMW.
        // When unset, fall through to the existing devmem path UNCHANGED, so the
        // proven fleet +  handoff are byte-identical on the wire.
        if am2_hb_reset_repoint_enabled() {
            return hold_am2_reset_sysfs(slots, hold_ms);
        }

        let mut union: u32 = 0;
        for &slot in slots {
            if (slot as usize) >= AM2_RESET_GPIOS.len() {
                return Err(HalError::Platform(format!(
                    "board-control: reset slot {} out of range (0..{})",
                    slot,
                    AM2_RESET_GPIOS.len()
                )));
            }
            let gpio = AM2_RESET_GPIOS[slot as usize];
            if !(AM2_GPIO_OUT_CHIP_FIRST..=AM2_GPIO_OUT_CHIP_LAST).contains(&gpio) {
                return Err(HalError::Platform(format!(
                    "board-control: reset slot {} (gpio {}) is not on the PL GPIO bank \
                     0x{:08X} (897..=901) — devmem hold not applicable",
                    slot, gpio, AM2_GPIO_OUT_CHIP_BASE
                )));
            }
            union |= 1u32 << (gpio - AM2_GPIO_OUT_CHIP_FIRST);
        }
        hold_am2_resets_devmem(union, hold_ms)?;
        tracing::info!(
            slots = ?slots,
            reset_bits_union = format!("0x{union:08X}"),
            hold_ms,
            method = "devmem",
            "am2 hashboard resets held (multi-slot simultaneous, AXI-GPIO 0x41210000)"
        );
        Ok(())
    }

    /// Assert (drive LOW, active-low) the given HBx_RESET slots and RETURN
    /// WITHOUT releasing — the held-open half of `hold_resets_devmem`.
    ///
    /// Pairs with [`release_resets_high`]. Used by the `a lab unit` RE-018 standalone
    /// cold-wake to energize the chip rail WHILE reset is held LOW (bosminer's
    /// proven power-on-reset order: ENABLE_VOLTAGE fires inside the reset-LOW
    /// window, reset releases ~0.3-0.55 s AFTER the rail is up). Sysfs-repoint
    /// only — the sole caller is `a lab unit`-fingerprint + RE-018-gated, which always
    /// runs with `DCENT_AM2_HB_RESET_REPOINT=1`. PWR_CONTROL (separate bank) is
    /// untouched, so the rail input the `psu_bypass_gate` asserted stays HIGH.
    pub fn assert_resets_low(&self, slots: &[u8]) -> Result<()> {
        if !am2_hb_reset_repoint_enabled() {
            return Err(HalError::Platform(
                "assert_resets_low requires DCENT_AM2_HB_RESET_REPOINT (sysfs split path)"
                    .to_string(),
            ));
        }
        let mut driven: Vec<(u8, u32)> = Vec::new();
        for &slot in slots {
            let Some(&gpio) = AM2_RESET_GPIOS.get(slot as usize) else {
                return Err(HalError::Platform(format!(
                    "assert_resets_low: reset slot {} out of range (known slots: 0..{})",
                    slot,
                    AM2_RESET_GPIOS.len().saturating_sub(1)
                )));
            };
            if let Err(e) = ensure_sysfs_gpio_exported(gpio) {
                tracing::warn!(slot, gpio, error = %e, "assert_resets_low: export failed (slot may be unpopulated); skipping");
                continue;
            }
            if let Err(e) = write_sysfs_gpio_direction(gpio, "out") {
                tracing::warn!(slot, gpio, error = %e, "assert_resets_low: direction=out failed; skipping");
                continue;
            }
            match write_sysfs_gpio_value(gpio, false)
                .and_then(|_| verify_sysfs_gpio_value(gpio, false))
            {
                Ok(()) => {
                    driven.push((slot, gpio));
                    tracing::info!(
                        slot,
                        gpio,
                        level = "low",
                        mechanism = "sysfs-repoint-split",
                        "HBx_RESET asserted LOW (split assert; rail will energize while held, bosminer order)"
                    );
                }
                Err(e) => {
                    tracing::warn!(slot, gpio, error = %e, "assert_resets_low: drive-LOW failed")
                }
            }
        }
        if driven.is_empty() {
            return Err(HalError::Platform(
                "assert_resets_low: no requested HB_RESET sysfs line was driven LOW".to_string(),
            ));
        }
        if driven.len() != slots.len() {
            return Err(HalError::Platform(format!(
                "assert_resets_low: only drove {} of {} requested HB_RESET slots LOW: {:?}",
                driven.len(),
                slots.len(),
                driven
            )));
        }
        Ok(())
    }

    /// Release (drive HIGH, deassert active-low) the given HBx_RESET slots —
    /// the second half of the split, called AFTER the chip rail is energized so
    /// the BM1362 deassert reset into a LIVE rail. Pairs with
    /// [`assert_resets_low`]; sysfs-repoint only.
    pub fn release_resets_high(&self, slots: &[u8]) -> Result<()> {
        if !am2_hb_reset_repoint_enabled() {
            return Err(HalError::Platform(
                "release_resets_high requires DCENT_AM2_HB_RESET_REPOINT (sysfs split path)"
                    .to_string(),
            ));
        }
        let mut driven: Vec<(u8, u32)> = Vec::new();
        for &slot in slots {
            let Some(&gpio) = AM2_RESET_GPIOS.get(slot as usize) else {
                return Err(HalError::Platform(format!(
                    "release_resets_high: reset slot {} out of range (known slots: 0..{})",
                    slot,
                    AM2_RESET_GPIOS.len().saturating_sub(1)
                )));
            };
            match write_sysfs_gpio_value(gpio, true)
                .and_then(|_| verify_sysfs_gpio_value(gpio, true))
            {
                Ok(()) => {
                    driven.push((slot, gpio));
                    tracing::info!(
                        slot,
                        gpio,
                        level = "high",
                        mechanism = "sysfs-repoint-split",
                        "HBx_RESET released HIGH (split release; AFTER rail energized, bosminer order)"
                    );
                }
                Err(e) => {
                    tracing::warn!(slot, gpio, error = %e, "release_resets_high: drive-HIGH failed")
                }
            }
        }
        if driven.is_empty() {
            return Err(HalError::Platform(
                "release_resets_high: no requested HB_RESET sysfs line was driven HIGH".to_string(),
            ));
        }
        if driven.len() != slots.len() {
            return Err(HalError::Platform(format!(
                "release_resets_high: only drove {} of {} requested HB_RESET slots HIGH: {:?}",
                driven.len(),
                slots.len(),
                driven
            )));
        }
        Ok(())
    }

    /// Enable/disable the PSU via the hardware PWR_CONTROL pin (belt-and-suspenders).
    ///
    /// This is NOT a replacement for the PSU I2C driver's software-disable
    /// path — it is a parallel hardware interlock that physically cuts the
    /// PWM enable line if software has already misbehaved. Safe to call
    /// alongside `psu.disable()` / `psu.enable()`.
    pub fn psu_enable(&self, on: bool) -> Result<()> {
        ensure_sysfs_gpio_exported(AM2_PSU_ENABLE_GPIO)?;
        write_sysfs_gpio_direction(AM2_PSU_ENABLE_GPIO, "out")?;
        write_sysfs_gpio_value(AM2_PSU_ENABLE_GPIO, on)?;
        tracing::info!(
            gpio = AM2_PSU_ENABLE_GPIO,
            state = if on { "enabled" } else { "disabled" },
            "am2 PSU PWR_CONTROL pin"
        );
        Ok(())
    }

    /// Read back the current PSU enable state (non-panicking).
    pub fn is_psu_enabled(&self) -> bool {
        read_sysfs_gpio(AM2_PSU_ENABLE_GPIO).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// sysfs GPIO helpers (duplicated from platform/zynq.rs to keep this module
// self-contained — eventually these should migrate to a shared gpio helper).
// ---------------------------------------------------------------------------

fn ensure_sysfs_gpio_exported(gpio: u32) -> Result<()> {
    let gpio_dir = format!("/sys/class/gpio/gpio{}", gpio);
    if !Path::new(&gpio_dir).exists() {
        fs::write("/sys/class/gpio/export", format!("{}", gpio))
            .map_err(|e| HalError::Platform(format!("export GPIO {}: {}", gpio, e)))?;
        sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn write_sysfs_gpio_direction(gpio: u32, dir: &str) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/direction", gpio);
    fs::write(&path, dir).map_err(|e| HalError::Platform(format!("GPIO {} direction: {}", gpio, e)))
}

fn write_sysfs_gpio_value(gpio: u32, high: bool) -> Result<()> {
    let path = format!("/sys/class/gpio/gpio{}/value", gpio);
    fs::write(&path, if high { "1" } else { "0" })
        .map_err(|e| HalError::Platform(format!("GPIO {} value: {}", gpio, e)))
}

fn read_sysfs_gpio(gpio: u32) -> Result<bool> {
    let path = format!("/sys/class/gpio/gpio{}/value", gpio);
    let raw = fs::read_to_string(&path)
        .map_err(|e| HalError::Platform(format!("GPIO {} read: {}", gpio, e)))?;
    Ok(raw.trim() == "1")
}

fn verify_sysfs_gpio_value(gpio: u32, expected_high: bool) -> Result<()> {
    let observed = read_sysfs_gpio(gpio)?;
    if observed != expected_high {
        return Err(HalError::Platform(format!(
            "GPIO {} readback mismatch after write: expected {}, observed {}",
            gpio,
            if expected_high { 1 } else { 0 },
            if observed { 1 } else { 0 }
        )));
    }
    Ok(())
}

/// devmem-based reset hold on the am2 `0x41210000` PL GPIO bank.
///
/// Maps one 4 KB page of `/dev/mem` at the output-register base and issues:
///   1. RMW clear `reset_bit`  (assert reset, LOW)
///   2. `sleep(hold_ms)`
///   3. RMW set `reset_bit` back HIGH
/// In both writes, `AM2_GPIO_OUT_PWR_CONTROL_BIT` is forced HIGH so gpio-901
/// stays asserted regardless of cached kernel state. Bits outside the reset
/// + PWR_CONTROL set are preserved by the RMW read.
///
/// `hold_ms` parameterizes the LOW dwell. The short `pulse_reset()` path passes
/// the historical 10 ms; the standalone Phase-2b-extended long-hold passes its
/// configured `am2_reset_hold_ms` (). The 0x07 short-pulse contract on
/// the proven fleet is preserved by passing 10 from the existing caller.
fn hold_am2_reset_devmem(reset_bit: u32, hold_ms: u64) -> Result<()> {
    let mem_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::Platform(format!("open /dev/mem: {}", e)))?;

    let page_size = NonZeroUsize::new(4096).expect("4096 is non-zero");
    let mapped = unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &mem_file,
            AM2_GPIO_OUT_CHIP_BASE as nix::libc::off_t,
        )
    }
    .map_err(|e| {
        HalError::Platform(format!(
            "mmap PL GPIO bank at 0x{:08X}: {}",
            AM2_GPIO_OUT_CHIP_BASE, e
        ))
    })?;

    let reg = mapped.as_ptr() as *mut u32;

    // H-hbreset-tri (default-OFF env `DCENT_AM2_HB_RESET_SET_TRI`): before
    // driving the DATA register, clear the TRI/direction bits for `reset_bit`
    // and PWR_CONTROL so the lines actually drive instead of staying tristated
    // (input/high-Z) — a candidate cause of `a lab unit` standalone enum=0. Falsifiable:
    // we read `+0x04` back and log before/after + whether the bits cleared. On a
    // `xlnx,all-outputs=1` bitstream the TRI reg is hardware read-only, so the
    // readback will show the bits UNCHANGED (= float hypothesis ruled out here).
    // When the gate is UNSET this whole block is skipped → byte-for-byte
    // identical wire behaviour to before (no `+0x04` access at all).
    if am2_hb_reset_set_tri_enabled() {
        // SAFETY: same 4 KB mmap'd PL GPIO page; the TRI register lives at
        // `+0x04` = `reg.add(1)` (one u32 stride, in-bounds). We only CLEAR the
        // two bits the DATA RMW below already drives (reset + PWR_CONTROL),
        // preserving all other TRI bits — never widening the driven set.
        let tri_reg = unsafe { reg.add(1) };
        let tri_target = reset_bit | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        let tri_before = unsafe { std::ptr::read_volatile(tri_reg) };
        let tri_desired = tri_before & !tri_target;
        unsafe { std::ptr::write_volatile(tri_reg, tri_desired) };
        let tri_after = unsafe { std::ptr::read_volatile(tri_reg) };
        let bits_cleared = (tri_after & tri_target) == 0;
        tracing::info!(
            tri_offset = AM2_GPIO_OUT_TRI,
            target_bits = format!("0x{tri_target:08X}"),
            tri_before = format!("0x{tri_before:08X}"),
            tri_desired = format!("0x{tri_desired:08X}"),
            tri_after = format!("0x{tri_after:08X}"),
            bits_cleared,
            "am2 HB_RESET TRI/direction clear (DCENT_AM2_HB_RESET_SET_TRI=1): \
             readback +0x04 — bits_cleared=true means lines now drive; false \
             means TRI is hardware read-only (all-outputs bitstream), float \
             hypothesis ruled out on this unit"
        );
    }

    // SAFETY: 4 KB mmap of a PL GPIO register page; volatile 32-bit reads/writes
    // at offset 0 are sound. Bits outside `reset_bit` are preserved by RMW. The
    // `| AM2_GPIO_OUT_PWR_CONTROL_BIT` OR guarantees gpio-901 stays HIGH across
    // the pulse, which is the whole point of this devmem workaround.
    unsafe {
        // Assert reset: clear `reset_bit`, force PWR_CONTROL HIGH.
        let before = std::ptr::read_volatile(reg);
        let low = (before & !reset_bit) | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        std::ptr::write_volatile(reg, low);
        sleep(Duration::from_millis(hold_ms));

        // Release reset: set `reset_bit`, still force PWR_CONTROL HIGH.
        let mid = std::ptr::read_volatile(reg);
        let high = mid | reset_bit | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        std::ptr::write_volatile(reg, high);
    }

    // Release the 4 KB /dev/mem mapping before returning so repeated cold-boot /
    // HB_RESET-faithful retry calls (s19j_hybrid_mining reset->enum loop) don't
    // leak one VMA of physical register space each. Mirrors the i2c.rs inline
    // munmap + psu_gpio_i2c.rs  Drop convention; nix 0.29 mmap returns a
    // raw NonNull, NOT an RAII guard, so the mapping must be released explicitly.
    // SAFETY: `mapped` came from the mmap above with len 4096; we own it and no
    // outstanding pointer into the page (`reg`) is used after this point.
    let _ = unsafe { nix::sys::mman::munmap(mapped, 4096) };
    drop(mem_file);
    Ok(())
}

/// devmem-based multi-slot reset hold on the am2 `0x41210000` PL GPIO bank.
///
/// Holds the reset lines for ALL the given slots LOW **simultaneously** for
/// `hold_ms`, then releases them HIGH together. This mirrors the original
/// `s19j_hybrid_mining` Phase-2b-extended sysfs sequence which held slots 1 AND
/// 3 LOW at the same time (so the BM1362 chains on both hashboards reset in
/// lockstep instead of being serialized), but does it via the proven
/// `/dev/mem` AXI-GPIO RMW path so it works even on `a lab unit` where the kernel
/// claims gpio-898/900 and they are not sysfs-exportable.
///
/// SAFETY/mmap pattern is identical to [`hold_am2_reset_devmem`]:
///   1. RMW clear ALL the given slots' reset bits at once (assert reset, LOW),
///      OR `AM2_GPIO_OUT_PWR_CONTROL_BIT` HIGH.
///   2. `sleep(hold_ms)`.
///   3. RMW set ALL the reset bits back HIGH, OR `AM2_GPIO_OUT_PWR_CONTROL_BIT`
///      HIGH.
/// PWR_CONTROL (gpio-901, bit 4) is forced HIGH in BOTH writes so the PSU rail
/// is never dropped across the hold. Bits outside the union of the slot reset
/// bits + PWR_CONTROL are preserved by the RMW read.
///
/// The optional `DCENT_AM2_HB_RESET_SET_TRI` TRI/direction-clear is honored the
/// same way as the single-slot path: when the gate is set, the union of all the
/// slots' reset bits + PWR_CONTROL is cleared in the TRI register first (forcing
/// OUTPUT) with a falsifiable before/after readback log.
fn hold_am2_resets_devmem(reset_bits_union: u32, hold_ms: u64) -> Result<()> {
    let mem_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/mem")
        .map_err(|e| HalError::Platform(format!("open /dev/mem: {}", e)))?;

    let page_size = NonZeroUsize::new(4096).expect("4096 is non-zero");
    let mapped = unsafe {
        nix::sys::mman::mmap(
            None,
            page_size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &mem_file,
            AM2_GPIO_OUT_CHIP_BASE as nix::libc::off_t,
        )
    }
    .map_err(|e| {
        HalError::Platform(format!(
            "mmap PL GPIO bank at 0x{:08X}: {}",
            AM2_GPIO_OUT_CHIP_BASE, e
        ))
    })?;

    let reg = mapped.as_ptr() as *mut u32;

    // H-hbreset-tri parity with the single-slot path: clear the TRI bits for the
    // union of reset lines + PWR_CONTROL when the gate is set, then read back so
    // the result is falsifiable.
    if am2_hb_reset_set_tri_enabled() {
        // SAFETY: same 4 KB mmap'd PL GPIO page; the TRI register lives at
        // `+0x04` = `reg.add(1)` (one u32 stride, in-bounds). We only CLEAR the
        // bits the DATA RMW below already drives (the reset-bit union +
        // PWR_CONTROL), preserving all other TRI bits — never widening the
        // driven set.
        let tri_reg = unsafe { reg.add(1) };
        let tri_target = reset_bits_union | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        let tri_before = unsafe { std::ptr::read_volatile(tri_reg) };
        let tri_desired = tri_before & !tri_target;
        unsafe { std::ptr::write_volatile(tri_reg, tri_desired) };
        let tri_after = unsafe { std::ptr::read_volatile(tri_reg) };
        let bits_cleared = (tri_after & tri_target) == 0;
        tracing::info!(
            tri_offset = AM2_GPIO_OUT_TRI,
            target_bits = format!("0x{tri_target:08X}"),
            tri_before = format!("0x{tri_before:08X}"),
            tri_desired = format!("0x{tri_desired:08X}"),
            tri_after = format!("0x{tri_after:08X}"),
            bits_cleared,
            "am2 HB_RESET (multi-slot) TRI/direction clear (DCENT_AM2_HB_RESET_SET_TRI=1)"
        );
    }

    // SAFETY: 4 KB mmap of a PL GPIO register page; volatile 32-bit reads/writes
    // at offset 0 are sound. Bits outside `reset_bits_union` are preserved by
    // RMW. The `| AM2_GPIO_OUT_PWR_CONTROL_BIT` OR guarantees gpio-901 stays
    // HIGH across the hold, which is the whole point of this devmem workaround.
    unsafe {
        // Assert reset on ALL slots: clear the reset-bit union, force PWR_CONTROL HIGH.
        let before = std::ptr::read_volatile(reg);
        let low = (before & !reset_bits_union) | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        std::ptr::write_volatile(reg, low);
        sleep(Duration::from_millis(hold_ms));

        // Release reset on ALL slots: set the reset-bit union, still force PWR_CONTROL HIGH.
        let mid = std::ptr::read_volatile(reg);
        let high = mid | reset_bits_union | AM2_GPIO_OUT_PWR_CONTROL_BIT;
        std::ptr::write_volatile(reg, high);
    }

    // Release the 4 KB /dev/mem mapping before returning so repeated cold-boot /
    // HB_RESET-faithful retry calls (s19j_hybrid_mining reset->enum loop) don't
    // leak one VMA of physical register space each. Mirrors the i2c.rs inline
    // munmap + psu_gpio_i2c.rs  Drop convention; nix 0.29 mmap returns a
    // raw NonNull, NOT an RAII guard, so the mapping must be released explicitly.
    // SAFETY: `mapped` came from the mmap above with len 4096; we own it and no
    // outstanding pointer into the page (`reg`) is used after this point.
    let _ = unsafe { nix::sys::mman::munmap(mapped, 4096) };
    drop(mem_file);
    Ok(())
}

/// R5 sysfs-reset re-point (2026-05-31, gated by `DCENT_AM2_HB_RESET_REPOINT`).
///
/// Drive a bosminer-faithful HBx_RESET PULSE via PLAIN KERNEL SYSFS — the exact
/// mechanism a cold bosminer capture proved on `a lab unit`:
///   1. For each slot: `ensure_sysfs_gpio_exported(gpio)` then
///      `write_sysfs_gpio_direction(gpio, "out")`.
///   2. Assert reset LOW on ALL slots (`write_sysfs_gpio_value(gpio, false)` + readback) —
///      active-LOW.
///   3. `sleep(hold_ms)`.
///   4. Release HIGH on ALL slots (`write_sysfs_gpio_value(gpio, true)` + readback).
/// where `gpio = AM2_RESET_GPIOS[slot]`.
///
/// Unlike the devmem RMW path, this needs no PWR_CONTROL preservation: PWR_CONTROL
/// is gpio907 on the SEPARATE Zynq PS bank (e000a000), driven by `psu_enable()`,
/// so a sysfs write to gpio897..900 cannot clobber it. The am2 kernel has
/// CONFIG_GPIO_CDEV OFF / only CONFIG_GPIO_SYSFS=y, so sysfs is the only viable
/// path.
///
/// `hold_ms` is the configured `am2_reset_hold_ms`; the LOW dwell is
/// `max(hold_ms, AM2_HB_RESET_REPOINT_MIN_HOLD_MS)` so a configured value below
/// the bosminer-faithful ~1.8 s can never starve the cold-wake pulse.
///
/// Each slot is validated against `AM2_RESET_GPIOS` before ANY write; an
/// out-of-range slot fails the whole call before touching hardware. Per-slot
/// export/direction failures are logged and skipped (a slot may be unpopulated /
/// unexported); the LOW→hold→HIGH pulse then runs for whichever slots came up as
/// outputs.
fn hold_am2_reset_sysfs(slots: &[u8], hold_ms: u64) -> Result<()> {
    if slots.is_empty() {
        return Err(HalError::Platform(
            "board-control: hold_am2_reset_sysfs called with no slots".to_string(),
        ));
    }

    // Validate + resolve all slots to GPIO numbers BEFORE any write.
    let mut gpios: Vec<(u8, u32)> = Vec::with_capacity(slots.len());
    for &slot in slots {
        if (slot as usize) >= AM2_RESET_GPIOS.len() {
            return Err(HalError::Platform(format!(
                "board-control: reset slot {} out of range (0..{})",
                slot,
                AM2_RESET_GPIOS.len()
            )));
        }
        gpios.push((slot, AM2_RESET_GPIOS[slot as usize]));
    }

    // Bosminer-faithful cold dwell floor.
    let effective_hold_ms = hold_ms.max(AM2_HB_RESET_REPOINT_MIN_HOLD_MS);

    // Step 1: export + direction=out for every slot. Skip (don't fail the whole
    // pulse) on a per-slot error — an unpopulated/unexportable slot is a no-op,
    // matching pulse_reset()'s tolerance of unpopulated slots.
    let mut driven: Vec<(u8, u32)> = Vec::with_capacity(gpios.len());
    for &(slot, gpio) in &gpios {
        if let Err(e) = ensure_sysfs_gpio_exported(gpio) {
            tracing::warn!(
                slot,
                gpio,
                error = %e,
                mechanism = "sysfs-repoint",
                "HBx_RESET sysfs export failed (slot may be unpopulated/kernel-claimed); skipping this slot"
            );
            continue;
        }
        if let Err(e) = write_sysfs_gpio_direction(gpio, "out") {
            tracing::warn!(
                slot,
                gpio,
                error = %e,
                mechanism = "sysfs-repoint",
                "HBx_RESET sysfs direction=out failed; skipping this slot"
            );
            continue;
        }
        driven.push((slot, gpio));
    }

    if driven.is_empty() {
        return Err(HalError::Platform(
            "board-control: hold_am2_reset_sysfs — no slots could be exported as outputs \
             (all export/direction writes failed)"
                .to_string(),
        ));
    }

    if driven.len() != gpios.len() {
        return Err(HalError::Platform(format!(
            "board-control: hold_am2_reset_sysfs only exported/directed {} of {} requested HB_RESET slots: {:?}",
            driven.len(),
            gpios.len(),
            driven
        )));
    }

    // Step 2: assert reset LOW on ALL driven slots (active-LOW).
    let mut low_driven: Vec<(u8, u32)> = Vec::with_capacity(driven.len());
    for &(slot, gpio) in &driven {
        match write_sysfs_gpio_value(gpio, false).and_then(|_| verify_sysfs_gpio_value(gpio, false))
        {
            Ok(()) => {
                low_driven.push((slot, gpio));
                tracing::info!(
                    slot,
                    gpio,
                    level = "low",
                    hold_ms = effective_hold_ms,
                    mechanism = "sysfs-repoint",
                    "HBx_RESET asserted LOW (sysfs active-LOW) — bosminer-faithful cold reset pulse"
                );
            }
            Err(e) => tracing::warn!(
                slot,
                gpio,
                error = %e,
                mechanism = "sysfs-repoint",
                "HBx_RESET sysfs assert-LOW failed"
            ),
        }
    }
    if low_driven.is_empty() {
        return Err(HalError::Platform(
            "board-control: hold_am2_reset_sysfs — no requested HB_RESET sysfs line \
             was actually driven LOW"
                .to_string(),
        ));
    }

    if low_driven.len() != driven.len() {
        return Err(HalError::Platform(format!(
            "board-control: hold_am2_reset_sysfs only drove {} of {} requested HB_RESET slots LOW: {:?}",
            low_driven.len(),
            driven.len(),
            low_driven
        )));
    }

    // Step 3: hold.
    sleep(Duration::from_millis(effective_hold_ms));

    // Step 4: release HIGH on ALL driven slots.
    let mut high_driven: Vec<(u8, u32)> = Vec::with_capacity(low_driven.len());
    for &(slot, gpio) in &low_driven {
        match write_sysfs_gpio_value(gpio, true).and_then(|_| verify_sysfs_gpio_value(gpio, true)) {
            Ok(()) => {
                high_driven.push((slot, gpio));
                tracing::info!(
                    slot,
                    gpio,
                    level = "high",
                    hold_ms = effective_hold_ms,
                    mechanism = "sysfs-repoint",
                    "HBx_RESET released HIGH (sysfs) — cold reset pulse complete"
                );
            }
            Err(e) => tracing::warn!(
                slot,
                gpio,
                error = %e,
                mechanism = "sysfs-repoint",
                "HBx_RESET sysfs release-HIGH failed (reset may remain asserted)"
            ),
        }
    }
    if high_driven.is_empty() {
        return Err(HalError::Platform(
            "board-control: hold_am2_reset_sysfs — HB_RESET asserted LOW but no \
             requested sysfs line was released HIGH"
                .to_string(),
        ));
    }

    if high_driven.len() != low_driven.len() {
        return Err(HalError::Platform(format!(
            "board-control: hold_am2_reset_sysfs only released {} of {} requested HB_RESET slots HIGH: {:?}",
            high_driven.len(),
            low_driven.len(),
            high_driven
        )));
    }

    tracing::info!(
        low_slots = ?low_driven.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        high_slots = ?high_driven.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        requested_hold_ms = hold_ms,
        effective_hold_ms,
        floor_ms = AM2_HB_RESET_REPOINT_MIN_HOLD_MS,
        mechanism = "sysfs-repoint",
        "am2 hashboard resets pulsed via plain-kernel-sysfs LOW->hold->HIGH (R5 re-point, bosminer-faithful)"
    );
    Ok(())
}

fn resolve_reset_gpio(slot: u8) -> Option<u32> {
    let label = *RESET_LABELS.get(slot as usize)?;
    for (path, base) in DT_GPIO_LABEL_SOURCES {
        let dt_path = Path::new(path);
        if !dt_path.exists() {
            continue;
        }
        let blob = fs::read(dt_path).ok()?;
        if let Some(gpio) = gpio_from_dt_blob(&blob, *base, label) {
            tracing::info!(slot, label, gpio, dt_path = %dt_path.display(), "Resolved am2 reset GPIO from DT");
            return Some(gpio);
        }
    }
    None
}

/// Resolve a DT-label (e.g. `HB0_RESET`) to its libgpiod `(chardev, offset)`.
///
/// Strategy:
///   1. Walk DT_GPIO_LABEL_SOURCES to find which gpiochip exposes `label`.
///   2. Map that chip's sysfs base to the chardev via libgpiod helpers.
///   3. Return offset = global_gpio - chardev_base.
///
/// Returns `Ok(None)` when no chip publishes the label (e.g. on AML where
/// HB labels are not used).
fn lookup_chardev_for_label(label: &str) -> Result<Option<(std::path::PathBuf, u32)>> {
    for (path, base) in DT_GPIO_LABEL_SOURCES {
        let dt_path = Path::new(path);
        if !dt_path.exists() {
            continue;
        }
        let blob = match fs::read(dt_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(gpio) = gpio_from_dt_blob(&blob, *base, label) else {
            continue;
        };
        if let Some(chardev_pair) = crate::libgpiod::resolve_global_gpio(gpio)? {
            return Ok(Some(chardev_pair));
        }
    }
    Ok(None)
}

fn gpio_from_dt_blob(blob: &[u8], base: u32, label: &str) -> Option<u32> {
    for (idx, raw_name) in blob.split(|b| *b == 0).enumerate() {
        if raw_name == label.as_bytes() {
            return Some(base + idx as u32);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_addresses_are_am2() {
        // Guard against a future regression that mixes am1/am2 bases.
        assert_eq!(BOARD_CONTROL_BASE_ADDR, 0x4281_0000);
        assert_eq!(REG_CONTROL_BOARD_MODE, 0x04);
        assert_eq!(CONTROL_BOARD_MODE_C49, 0x31);
        assert_eq!(CONTROL_BOARD_MODE_C52, 0x34);
        assert_eq!(AM2_PLUG_DETECT_GPIOS, [902, 903, 904, 905]);
        assert_eq!(AM2_PSU_ENABLE_GPIO, 907);
    }

    #[test]
    fn am2_board_control_uio17_map_keeps_reset_and_power_out_of_probe_window() {
        // The uio17 +0x00/+0x04 window is board-id/fan-mode only. HB_RESET is
        // driven through the separate 0x41210000 PL GPIO DATA/TRI registers, and
        // PWR_CONTROL is the separate gpio907 sysfs line. Do not collapse these
        // into a generic "base/+0x04" UIO abstraction.
        assert_eq!(BOARD_CONTROL_BASE_ADDR, 0x4281_0000);
        assert_eq!(REG_BOARD_ID_0, 0x00);
        assert_eq!(REG_CONTROL_BOARD_MODE, 0x04);
        assert_eq!(REG_BOARD_ID_1, REG_CONTROL_BOARD_MODE);
        assert_eq!(CONTROL_BOARD_MODE_BIT8, 0x100);

        assert_eq!(AM2_RESET_GPIOS, [897, 898, 899, 900]);
        assert_eq!(AM2_GPIO_OUT_CHIP_BASE, 0x4121_0000);
        assert_eq!(AM2_GPIO_OUT_CHIP_FIRST, 897);
        assert_eq!(AM2_GPIO_OUT_CHIP_LAST, 901);
        assert_eq!(AM2_GPIO_OUT_PWR_CONTROL_BIT, 1 << 4);
        assert_eq!(AM2_GPIO_OUT_TRI, 0x04);
        assert_eq!(AM2_PSU_ENABLE_GPIO, 907);
    }

    #[test]
    fn am2_control_board_mode_writes_preserve_live_high_bits() {
        // Live captures show both 0x034 and 0x134. C52 fan-mode writes may only
        // replace the low byte, and the standalone bit8 candidate may only OR
        // bit 8; clearing high bits here would lose live-proven board state.
        let c49_with_bit8 = 0x0000_0131;
        let c52_desired = (c49_with_bit8 & !CONTROL_BOARD_MODE_MASK)
            | (CONTROL_BOARD_MODE_C52 & CONTROL_BOARD_MODE_MASK);
        assert_eq!(c52_desired, 0x0000_0134);

        let c52_without_bit8 = 0x0000_0034;
        assert_eq!(c52_without_bit8 | CONTROL_BOARD_MODE_BIT8, 0x0000_0134);
    }

    #[test]
    fn hb_reset_tri_offset_matches_axi_gpio_tri() {
        // TRI/direction reg is +0x04 within the PL GPIO bank; must stay in sync
        // with the canonical AXI-GPIO TRI offset so the devmem H-hbreset-tri
        // path writes the right register.
        assert_eq!(AM2_GPIO_OUT_TRI, 0x04);
        assert_eq!(AM2_GPIO_OUT_TRI, crate::gpio::GPIO_TRI);
    }

    #[test]
    fn hb_reset_set_tri_env_is_default_off() {
        // Gate must be OFF unless explicitly set, so the proven fleet +
        // .25 path are byte-identical by default. (Guard against a stray env in
        // the test process by asserting the unset case via remove_var.)
        assert_eq!(AM2_HB_RESET_SET_TRI_ENV, "DCENT_AM2_HB_RESET_SET_TRI");
        // SAFETY: single-threaded unit test; we restore nothing because the
        // default contract is "absent => false".
        std::env::remove_var(AM2_HB_RESET_SET_TRI_ENV);
        assert!(!am2_hb_reset_set_tri_enabled());
        std::env::set_var(AM2_HB_RESET_SET_TRI_ENV, "1");
        assert!(am2_hb_reset_set_tri_enabled());
        std::env::set_var(AM2_HB_RESET_SET_TRI_ENV, "0");
        assert!(!am2_hb_reset_set_tri_enabled());
        std::env::remove_var(AM2_HB_RESET_SET_TRI_ENV);
    }

    #[test]
    fn hb_reset_repoint_env_is_default_off() {
        // R5 sysfs-reset re-point gate must be OFF unless explicitly set, so the
        // proven fleet (.79/.109/.139/.135/S9) + the  .25 bosminer-handoff
        // path keep the devmem RMW reset drive byte-identically by default. Only
        // the .25 standalone launcher sets it.
        assert_eq!(AM2_HB_RESET_REPOINT_ENV, "DCENT_AM2_HB_RESET_REPOINT");
        // SAFETY: single-threaded unit test; default contract is "absent => false".
        std::env::remove_var(AM2_HB_RESET_REPOINT_ENV);
        assert!(!am2_hb_reset_repoint_enabled());
        std::env::set_var(AM2_HB_RESET_REPOINT_ENV, "1");
        assert!(am2_hb_reset_repoint_enabled());
        std::env::set_var(AM2_HB_RESET_REPOINT_ENV, "0");
        assert!(!am2_hb_reset_repoint_enabled());
        std::env::remove_var(AM2_HB_RESET_REPOINT_ENV);
    }

    #[test]
    fn hb_reset_repoint_min_hold_is_bosminer_faithful() {
        // The cold bosminer capture held HBx_RESET LOW ~1.8 s; the repoint pulse
        // floors the dwell at this so a shorter configured am2_reset_hold_ms can
        // never starve the cold-wake pulse.
        assert_eq!(AM2_HB_RESET_REPOINT_MIN_HOLD_MS, 1800);
    }

    #[test]
    fn reset_slot_out_of_range() {
        // Cannot instantiate BoardControl without a real UIO device, but we
        // can exercise the out-of-range guard via a direct constant check:
        assert!((4usize) >= AM2_RESET_GPIOS.len());
    }

    #[test]
    fn resolve_reset_label_from_dt_blob() {
        let blob = b"HB0_RESET\0HB1_RESET\0HB2_RESET\0HB3_RESET\0PWR_CONTROL\0";
        assert_eq!(gpio_from_dt_blob(blob, 897, "HB0_RESET"), Some(897));
        assert_eq!(gpio_from_dt_blob(blob, 897, "HB2_RESET"), Some(899));
        assert_eq!(gpio_from_dt_blob(blob, 897, "missing"), None);
    }
}
