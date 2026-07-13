//! Fan PWM controller.
//!
//! Controls the custom Braiins fan controller IP at 0x42800000. This is NOT
//! an AXI Timer (confirmed by live probing). Simple PWM with RPS tachometer
//! feedback.
//!
//! ## Two hardware variants:
//!
//! **am1-s9 (S9/T9/S9i/R4) `fan-control` IP @ 0x42800000 uio_N:**
//!   +0x00  FAN0_RPS   R   Fan 0 tach (RPS), always 0 on S9 (not wired)
//!   +0x04  FAN1_RPS   R   Fan 1 tach (RPS), multiply by 60 for RPM
//!   +0x10  FAN_PWM0   RW  PWM channel 0
//!   +0x14  FAN_PWM1   RW  PWM channel 1
//!
//! **am2-s17 (S17/T17/S19/S19j Pro Zynq) `fan-control` IP @ 0x42800000 uio16:**
//!   +0x00  FAN0_RPS   R   Fan 0 tach/feedback (RPS-like)
//!   +0x04  FAN1_RPS   R   Fan 1 tach/feedback (RPS-like)
//!   +0x08  FAN2_RPS   R   Fan 2 tach/feedback (RPS-like)
//!   +0x0C  FAN3_RPS   R   Fan 3 tach/feedback (RPS-like)
//!   +0x10  FAN_PWM0   RW  PWM command channel 0
//!   +0x14  FAN_PWM1   RW  PWM command channel 1 / secondary bank
//!   +0x20..             Mirror / alias region observed on live probes
//!
//! Register layout derived from:
//!   -  (S9 FPGA IP: fan_rps[] + fan_pwm)
//!   - `a lab unit` live probe on 2026-05-18: writes to 0x10/0x14 are command
//!     registers and can make PWM 100 audibly louder; writes to 0x00..0x0C
//!     did not stick and the values tracked the loud physical floor
//!     (~0x29..0x2C => ~2460..2640 RPM if decoded as RPS).
//!   - bosminer.bin string "BUG: PWM is larger than 100%" confirms PWM scale is 0-100
//!
//! ## PWM scaling
//!
//! Both variants accept PWM as 0-100 (BraiinsOS fan::Speed::new asserts <= 100).
//! `PWM_MAX = 100`. A legacy alias `PWM_MAX_LEGACY_7BIT = 127` is retained ONLY
//! for the stock bitstream probe path (never write values > 100 to either variant).
//!
//! ## Safety rules (from ,
//! )
//!
//! - Home-mining default cap: PWM 30. The S9 curve is ~1800 RPM; AM2/XIL
//!   physical RPM/noise must be read from tach because low PWM can still floor high.
//! - Industrial cap (S19j Pro full-power, 100 TH/s): PWM 80 (user-configurable).
//! - NEVER blast to 100 (or the old "127") in any safety path — cap at the
//!   configured profile max.
//! - NEVER return RPM=0 while the fan is physically spinning (15s FanFailure).
//!   On am2 the physical fan count is 4 — all 4 tach channels are exposed.

use std::path::Path;

use crate::uio::UioDevice;
use crate::{HalError, Result};

const ENV_AM2_FRONT_FAN_UIO: &str = "DCENT_AM2_FRONT_FAN_UIO";
const ENV_AM2_FRONT_FAN_PWM_OFFSET: &str = "DCENT_AM2_FRONT_FAN_PWM_OFFSET";

/// Fan controller physical base address (both variants).
pub const FAN_BASE_ADDR: u32 = 0x4280_0000;

/// Fan 0 RPS register.
pub const REG_FAN0_RPS: u32 = 0x00;
/// Fan 1 RPS register.
pub const REG_FAN1_RPS: u32 = 0x04;
/// Fan 2 RPS register (am2 only; reads 0 on am1).
pub const REG_FAN2_RPS: u32 = 0x08;
/// Fan 3 RPS register (am2 only; reads 0 on am1).
pub const REG_FAN3_RPS: u32 = 0x0C;

/// PWM channel 0 register.
pub const REG_FAN_PWM0: u32 = 0x10;
/// PWM channel 1 register.
pub const REG_FAN_PWM1: u32 = 0x14;

/// First byte offset included in the read-only raw diagnostic dump.
pub const RAW_FAN_DUMP_START_OFFSET: u32 = 0x00;
/// Last byte offset included in the read-only raw diagnostic dump.
pub const RAW_FAN_DUMP_END_OFFSET: u32 = 0x7C;
/// Byte stride for the read-only raw diagnostic dump.
pub const RAW_FAN_DUMP_STEP_BYTES: u32 = 4;

/// Maximum PWM value (BraiinsOS fan_ctrl IP is 0-100 scale — both variants).
/// A value > 100 is UB; bosminer asserts and panics.
pub const PWM_MAX: u8 = 100;

/// Legacy 7-bit ceiling used only by the stock Bitmain bitstream path.
/// DO NOT write values in this range to BraiinsOS fan_ctrl.
pub const PWM_MAX_LEGACY_7BIT: u8 = 127;

/// Default boot PWM command for quiet home mode.
pub const PWM_QUIET_BOOT: u8 = 10;

/// Safety maximum PWM command for the home-mining cap.
///
/// Must stay equal to `dcentrald_common::HOME_FAN_PWM_SAFETY_MAX` (pinned in tests).
pub const PWM_SAFETY_MAX: u8 = 30;

/// Safety maximum PWM for industrial full-power S19j Pro / S19 hashrates.
/// User-configurable via profile.fan_max_pwm; 80 is the upper envelope seen
/// in BraiinsOS on am2 under sustained 64+ TH/s load.
pub const PWM_INDUSTRIAL_MAX: u8 = 80;

/// Fan hardware variant (drives how many tach channels to read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanVariant {
    /// am1-s9 `fan-control` — 2 tach channels; FAN0 typically unwired on S9.
    Am1S9,
    /// am2-s17 `fan-control` — 4 tach channels, all wired.
    Am2Uio16,
}

impl FanVariant {
    /// Physical fan positions managed by this fan-control variant.
    pub const fn physical_fan_count(self) -> u8 {
        match self {
            Self::Am1S9 => 2,
            Self::Am2Uio16 => 4,
        }
    }

    /// Tach/RPS channels exposed by the FPGA fan-control register block.
    pub const fn tach_channel_count(self) -> u8 {
        match self {
            Self::Am1S9 => 2,
            Self::Am2Uio16 => 4,
        }
    }

    /// PWM command channels exposed by the FPGA fan-control register block.
    pub const fn pwm_command_channel_count(self) -> u8 {
        match self {
            Self::Am1S9 => 2,
            Self::Am2Uio16 => 2,
        }
    }

    /// Tach register wired to a physical fan position, when observable.
    ///
    /// AM1/S9 fan position 0 is physically present but its FAN0 tach input is
    /// known to be unwired. Keeping that as `None` is important: callers must
    /// neither mistake the permanent zero for a stalled fan nor silently treat
    /// a max-across-channels reading as evidence for every physical position.
    pub const fn tach_channel_for_physical_fan(self, physical_fan: u8) -> Option<u8> {
        match (self, physical_fan) {
            (Self::Am1S9, 0) => None,
            (Self::Am1S9, 1) => Some(1),
            (Self::Am2Uio16, 0..=3) => Some(physical_fan),
            _ => None,
        }
    }
}

/// Name-based discovery result for the `fan-control` UIO device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanUioDiscovery {
    /// `/dev/uioN` number whose sysfs name is exactly `fan-control`.
    pub uio_number: u8,
    /// Register/tach layout inferred from the sibling UIO set.
    pub variant: FanVariant,
    /// Whether a sibling `board-control` UIO was present.
    pub has_board_control: bool,
}

/// One raw 32-bit fan-control register read from the bounded diagnostic dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanRawRegister {
    /// Byte offset from the fan-control UIO mapping base.
    pub offset: u32,
    /// Raw 32-bit register value read at `offset`.
    pub value: u32,
}

/// One bounded tach read for every physical fan position in a variant.
///
/// `rpm_by_physical_fan.len()` is always [`FanVariant::physical_fan_count`].
/// `None` means the variant explicitly declares that physical position's tach
/// input unwired; `Some(0)` means an observable channel reported no motion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanTachSnapshot {
    /// Hardware layout used to interpret the register block.
    pub variant: FanVariant,
    /// RPM evidence indexed by physical fan position.
    pub rpm_by_physical_fan: Vec<Option<u32>>,
}

impl FanTachSnapshot {
    /// True only when at least one tach is observable and every observable
    /// physical-fan channel reports motion in this snapshot.
    pub fn all_required_channels_moving(&self) -> bool {
        let mut observable = 0usize;
        for rpm in self.rpm_by_physical_fan.iter().flatten() {
            observable += 1;
            if *rpm == 0 {
                return false;
            }
        }
        observable > 0
    }
}

/// Aligned fan-control offsets read by the raw diagnostic dump.
pub fn fan_raw_dump_offsets() -> impl Iterator<Item = u32> {
    (RAW_FAN_DUMP_START_OFFSET..=RAW_FAN_DUMP_END_OFFSET).step_by(RAW_FAN_DUMP_STEP_BYTES as usize)
}

/// Discover the fan-control UIO by sysfs name.
///
/// This deliberately does not trust UIO enumeration order. AM2 units have been
/// observed with `uio16 fan-control` and `uio17 board-control`, but the stable
/// contract is the sysfs `name` file, not the number.
pub fn discover_fan_uio() -> Option<FanUioDiscovery> {
    discover_fan_uio_in_dir("/sys/class/uio")
}

/// Testable implementation of [`discover_fan_uio`].
pub fn discover_fan_uio_in_dir<P: AsRef<Path>>(sys_class_uio: P) -> Option<FanUioDiscovery> {
    let entries = std::fs::read_dir(sys_class_uio).ok()?;
    let mut fan_uio: Option<u8> = None;
    let mut has_board_control = false;

    for entry in entries.flatten() {
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy();
        let Some(num_str) = dir_name.strip_prefix("uio") else {
            continue;
        };
        let Ok(num) = num_str.parse::<u8>() else {
            continue;
        };
        let Ok(name) = std::fs::read_to_string(entry.path().join("name")) else {
            continue;
        };
        match name.trim() {
            "fan-control" => {
                fan_uio = Some(match fan_uio {
                    Some(existing) => existing.min(num),
                    None => num,
                });
            }
            "board-control" => has_board_control = true,
            _ => {}
        }
    }

    let uio_number = fan_uio?;
    let variant = if has_board_control {
        FanVariant::Am2Uio16
    } else {
        FanVariant::Am1S9
    };
    Some(FanUioDiscovery {
        uio_number,
        variant,
        has_board_control,
    })
}

// `discover_uio_number_by_name` and `discover_uio_number_by_name_in_dir`
// were factored out of this module in  (2026-05-23) into
// `crate::uio_discover` so the FPGA-FIFO chain backend can reuse the same
// lookup pattern for `chain1-common` / `chain1-cmd-rx` /
// `chain1-work-rx` / `chain1-work-tx`. The tests for that function live
// alongside the function in `uio_discover.rs`.

/// Optional secondary control surface for the front-fan pair.
///
/// Kept as a default-off escape hatch for a future board whose front fans are
/// genuinely on a separate PWM surface. `a lab unit` is NOT that case: the live fix
/// is the C49 -> C52 mode switch on `board-control +0x04`, after which the
/// existing `fan-control` 0x10/0x14 channels drive all four fans.
///
/// DISABLED by default — `FanController::front_fan` is `None` unless
/// [`FanController::attach_front_fan_surface`] is called, so behavior is
/// byte-identical to a single-surface command. The UIO number + register
/// offset are NOT hardcoded: they are supplied only after a live PWM sweep
/// confirms (a) which UIO/IP drives the front fans and (b) that it is safely
/// writable (rust-firmware rule: register addresses must match verified probe
/// data, not open-source docs). See
/// .
struct FrontFanSurface {
    /// UIO device mapped to the secondary front-fan controller.
    regs: UioDevice,
    /// Byte offset of the front-fan PWM duty register on that surface.
    pwm_offset: u32,
}

/// Live status for the AM2 C52 fan-mode switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Am2C52FanModeStatus {
    /// `board-control` UIO number used for the mode write.
    pub uio_number: u8,
    /// Raw `board-control +0x04` value before the write.
    pub before: u32,
    /// Raw `board-control +0x04` value after the write/readback.
    pub after: u32,
}

/// Fan controller using FPGA UIO device.
pub struct FanController {
    /// UIO device mapped to the fan controller registers (0x42800000).
    regs: UioDevice,
    /// Fan hardware variant — determines tach channel count and exposure.
    variant: FanVariant,
    /// Optional confirmed secondary front-fan surface (default: none).
    front_fan: Option<FrontFanSurface>,
    /// AM2 control-board mode switch status (C49 -> C52 fan mux).
    am2_c52_fan_mode: Option<Am2C52FanModeStatus>,
    /// Optional profile ceiling applied after the hardware IP ceiling.
    /// Defaults to PWM_MAX for backwards compatibility.
    profile_cap_pwm: u8,
}

/// Clamp a PWM command to the IP ceiling (the BraiinsOS fan_ctrl IP panics on
/// values > 100). This is the final floor-level clamp ONLY — the profile cap
/// (`fan_max_pwm`) is enforced by callers, see [`FanController::set_speed`].
#[inline]
fn clamp_pwm(pwm: u8) -> u8 {
    pwm.min(PWM_MAX)
}

#[inline]
fn clamp_pwm_with_profile_cap(pwm: u8, profile_cap_pwm: u8) -> u8 {
    clamp_pwm(pwm).min(clamp_pwm(profile_cap_pwm))
}

#[inline]
fn open_with_variant_should_enable_am2_c52(variant: FanVariant) -> bool {
    matches!(variant, FanVariant::Am2Uio16)
}

impl FanController {
    /// Create a fan controller from an already-opened UIO device (am1-s9 default).
    ///
    /// Kept for backwards compatibility with S9 call sites. New code should
    /// prefer [`FanController::new_with_variant`].
    pub fn new(regs: UioDevice) -> Self {
        Self {
            regs,
            variant: FanVariant::Am1S9,
            front_fan: None,
            am2_c52_fan_mode: None,
            profile_cap_pwm: PWM_MAX,
        }
    }

    /// Create a fan controller with an explicit hardware variant.
    pub fn new_with_variant(regs: UioDevice, variant: FanVariant) -> Self {
        Self {
            regs,
            variant,
            front_fan: None,
            am2_c52_fan_mode: None,
            profile_cap_pwm: PWM_MAX,
        }
    }

    /// Open the fan controller UIO device by number (am1-s9 default).
    pub fn open(uio_number: u8) -> Result<Self> {
        let regs = UioDevice::open(uio_number)?;
        Ok(Self::new(regs))
    }

    /// Open the fan controller UIO device by number with an explicit variant.
    pub fn open_with_variant(uio_number: u8, variant: FanVariant) -> Result<Self> {
        let regs = UioDevice::open(uio_number)?;
        let mut fan = Self::new_with_variant(regs, variant);
        if open_with_variant_should_enable_am2_c52(variant) {
            fan.enable_am2_c52_fan_mode_from_board_control("FanController::open_with_variant");
        }
        fan.attach_front_fan_surface_from_env("FanController::open_with_variant");
        Ok(fan)
    }

    /// Discover and open the fan controller by UIO name.
    pub fn open_discovered() -> Result<(FanUioDiscovery, Self)> {
        let discovery = discover_fan_uio().ok_or_else(|| {
            HalError::Fan("no 'fan-control' UIO found under /sys/class/uio".to_string())
        })?;
        let fan = Self::open_with_variant(discovery.uio_number, discovery.variant)?;
        Ok((discovery, fan))
    }

    /// Fan hardware variant.
    pub fn variant(&self) -> FanVariant {
        self.variant
    }

    /// Set a controller-local profile cap applied by [`set_speed`] and
    /// [`set_speed_channels`] after the hardware IP ceiling. Controllers default
    /// to `PWM_MAX`, preserving existing call sites until they opt in.
    pub fn set_profile_cap(&mut self, cap_pwm: u8) {
        self.profile_cap_pwm = clamp_pwm(cap_pwm);
    }

    /// Current profile cap enforced by this controller.
    pub fn profile_cap(&self) -> u8 {
        self.profile_cap_pwm
    }

    /// Attach a confirmed secondary front-fan control surface (default: none).
    ///
    /// Opens `uio_number` and, from then on, every [`set_speed`] /
    /// [`set_speed_channels`] call also writes the front-fan PWM to
    /// `pwm_offset` on that surface. This is only for future sweep-confirmed
    /// boards with a genuinely separate front-fan PWM surface. The `a lab unit` fix is
    /// the automatic AM2 C52 mode switch, not this env-gated attachment path.
    ///
    /// SAFETY: call this ONLY with a UIO number + offset that a live PWM sweep
    /// has confirmed (a) drives the front fans and (b) is safely writable. Do
    /// NOT pass an unverified address — `0x42810000` has been flagged as a
    /// possible AXI address-hole that bus-faults. Normal `dcentrald` leaves
    /// this unset; `open_with_variant` calls it only through the explicit env
    /// gate after a sweep has confirmed the UIO+offset. See the reference
    /// memory rule on the `a lab unit` fan root cause.
    pub fn attach_front_fan_surface(&mut self, uio_number: u8, pwm_offset: u32) -> Result<()> {
        // Validate the offset HERE rather than relying on UioDevice::write_reg,
        // whose bounds + 4-byte-alignment guard is a `debug_assert!` that
        // compiles out in the release firmware. A fat-fingered offset would
        // otherwise do a raw out-of-bounds volatile write (SIGBUS) — the exact
        // bus-fault class this gated feature exists to avoid.
        if !pwm_offset.is_multiple_of(4) {
            return Err(HalError::Fan(format!(
                "front-fan pwm_offset 0x{pwm_offset:x} is not 4-byte aligned"
            )));
        }
        if (pwm_offset as usize) + 4 > crate::uio::UIO_MAP_SIZE {
            return Err(HalError::Fan(format!(
                "front-fan pwm_offset 0x{pwm_offset:x} is outside the {}-byte UIO map",
                crate::uio::UIO_MAP_SIZE
            )));
        }
        let regs = UioDevice::open(uio_number)?;
        self.front_fan = Some(FrontFanSurface { regs, pwm_offset });
        Ok(())
    }

    /// Whether a secondary front-fan control surface is attached.
    pub fn has_front_fan_surface(&self) -> bool {
        self.front_fan.is_some()
    }

    /// Status for the AM2 C52 fan-mode switch, if applied.
    pub fn am2_c52_fan_mode_status(&self) -> Option<Am2C52FanModeStatus> {
        self.am2_c52_fan_mode
    }

    /// Switch AM2 `board-control` into C52 2-PWM fan mode.
    ///
    /// This is the live-proven `a lab unit` fix. The unit booted in C49 mode
    /// (`board-control +0x04 = 0x31`), which left only two fans under the
    /// normal `fan-control` tach/PWM path. Writing C52 (`0x34`) exposed all
    /// four tach channels; then `fan-control` 0x10/0x14 at PWM 0 made the unit
    /// quiet. Failure to open or switch board-control is logged but does not
    /// make fan open fatal; the primary fan-control path remains usable.
    pub fn enable_am2_c52_fan_mode_from_board_control(&mut self, reason: &str) -> bool {
        if !matches!(self.variant, FanVariant::Am2Uio16) {
            return false;
        }

        let Some(board_uio) = crate::uio_discover::discover_uio_number_by_name("board-control")
        else {
            tracing::warn!(
                reason,
                "AM2 board-control UIO not found; using primary fan-control without C52 fan mode"
            );
            return false;
        };

        let board = match crate::board_control::BoardControl::open(board_uio) {
            Ok(board) => board,
            Err(e) => {
                tracing::warn!(
                    reason,
                    uio = board_uio,
                    error = %e,
                    "AM2 board-control open failed; using primary fan-control without C52 fan mode"
                );
                return false;
            }
        };

        match board.enable_c52_fan_mode() {
            Ok(status) => {
                self.am2_c52_fan_mode = Some(Am2C52FanModeStatus {
                    uio_number: board_uio,
                    before: status.before,
                    after: status.after,
                });
                tracing::info!(
                    reason,
                    uio = board_uio,
                    before = format!("0x{:08X}", status.before),
                    after = format!("0x{:08X}", status.after),
                    "AM2 board-control C52 2-PWM fan mode enabled"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    reason,
                    uio = board_uio,
                    error = %e,
                    "AM2 board-control C52 fan-mode switch failed; using primary fan-control only"
                );
                false
            }
        }
    }

    /// Attach the optional secondary front-fan surface from the explicit env
    /// gate, if configured.
    ///
    /// Default behavior is unchanged: when either env var is absent, this is a
    /// no-op. If the operator supplies a bad value, we warn and keep using the
    /// primary `fan-control` surface rather than making fan opens fatal.
    pub fn attach_front_fan_surface_from_env(&mut self, reason: &str) -> bool {
        if !matches!(self.variant, FanVariant::Am2Uio16) {
            return false;
        }

        let (Ok(uio_str), Ok(off_str)) = (
            std::env::var(ENV_AM2_FRONT_FAN_UIO),
            std::env::var(ENV_AM2_FRONT_FAN_PWM_OFFSET),
        ) else {
            return false;
        };

        let Ok(uio) = uio_str.trim().parse::<u8>() else {
            tracing::warn!(
                reason,
                env = ENV_AM2_FRONT_FAN_UIO,
                value = %uio_str,
                "AM2 front-fan surface gate has invalid UIO number; using primary fan-control only"
            );
            return false;
        };
        let offset = match parse_front_fan_offset(&off_str) {
            Ok(offset) => offset,
            Err(e) => {
                tracing::warn!(
                    reason,
                    env = ENV_AM2_FRONT_FAN_PWM_OFFSET,
                    value = %off_str,
                    error = %e,
                    "AM2 front-fan surface gate has invalid offset; using primary fan-control only"
                );
                return false;
            }
        };

        match self.attach_front_fan_surface(uio, offset) {
            Ok(()) => {
                tracing::info!(
                    reason,
                    uio,
                    offset = format!("0x{offset:02x}"),
                    "AM2 front-fan secondary control surface attached"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    reason,
                    uio,
                    offset = format!("0x{offset:02x}"),
                    error = %e,
                    "AM2 front-fan surface attach failed; using primary fan-control only"
                );
                false
            }
        }
    }

    /// Set fan speed (both primary PWM channels equal) via 0-100 PWM.
    ///
    /// Values > 100 are clamped to 100 (the BraiinsOS IP crashes on > 100).
    /// A controller-local profile cap can lower that ceiling further; it
    /// defaults to `PWM_MAX`, so existing callers keep historical behavior.
    ///.
    ///
    /// Equivalent to `set_speed_channels(pwm, pwm)` — preserves the historical
    /// behavior of writing both 0x10 and 0x14 to the same value.
    pub fn set_speed(&self, pwm: u8) {
        self.set_speed_channels(pwm, pwm);
    }

    /// Set the rear and front fan groups to independent 0-100 PWM values.
    ///
    /// Writes `pwm_rear` to FAN_PWM0 (0x10) and `pwm_front` to FAN_PWM1 (0x14)
    /// — the two channels of the am2 "C52" 2-PWM fan controller — and, if a
    /// secondary front-fan surface is attached (see
    /// [`attach_front_fan_surface`]), also writes `pwm_front` there. Both
    /// values are clamped to `PWM_MAX` and then to this controller's optional
    /// profile cap.
    ///
    /// On boards where 0x14 is a no-op (single-channel `fan-control`), passing
    /// `pwm_rear == pwm_front` reproduces [`set_speed`] exactly, so this is
    /// safe to use unconditionally.
    pub fn set_speed_channels(&self, pwm_rear: u8, pwm_front: u8) {
        let pwm_rear = clamp_pwm_with_profile_cap(pwm_rear, self.profile_cap_pwm);
        let pwm_front = clamp_pwm_with_profile_cap(pwm_front, self.profile_cap_pwm);
        self.regs.write_reg(REG_FAN_PWM0, pwm_rear as u32);
        self.regs.write_reg(REG_FAN_PWM1, pwm_front as u32);
        if let Some(front) = &self.front_fan {
            front.regs.write_reg(front.pwm_offset, pwm_front as u32);
        }
    }

    /// Get the current fan RPM from the tachometer (max across fans).
    ///
    /// SAFETY: if hardware returns 0
    /// RPS on all channels but the PWM register is non-zero, the thermal
    /// controller must not treat this as instant fan failure — it gets a
    /// 3-strike debounce in `controller.rs`.
    pub fn get_rpm(&self) -> u32 {
        let rps_list = self.read_all_rps();
        let max_rps = rps_list.iter().copied().max().unwrap_or(0);
        max_rps * 60
    }

    /// Read RPM evidence by physical fan position using the variant's explicit
    /// wiring map. Unlike [`get_rpm`], this never hides a stalled required fan
    /// behind another channel's higher RPM.
    pub fn get_tach_snapshot(&self) -> FanTachSnapshot {
        let rps = self.read_all_rps();
        let rpm_by_physical_fan = (0..self.variant.physical_fan_count())
            .map(|physical_fan| {
                self.variant
                    .tach_channel_for_physical_fan(physical_fan)
                    .map(|tach_channel| rps.get(tach_channel as usize).copied().unwrap_or(0) * 60)
            })
            .collect();
        FanTachSnapshot {
            variant: self.variant,
            rpm_by_physical_fan,
        }
    }

    /// Get the current PWM value (0-100).
    pub fn get_speed_pwm(&self) -> u8 {
        self.get_speed_pwm_channels().0
    }

    /// Get both PWM command channel readbacks (0-100).
    pub fn get_speed_pwm_channels(&self) -> (u8, u8) {
        // Only bottom 7 bits are wired into the IP register (BraiinsOS spec).
        let (raw0, raw1) = self.get_speed_pwm_raw_channels();
        ((raw0 & 0x7F) as u8, (raw1 & 0x7F) as u8)
    }

    /// Get raw 32-bit PWM command register values.
    ///
    /// Use this for diagnostics when a platform appears to hold a physical fan
    /// floor despite valid low PWM commands. The decoded `get_speed_pwm*`
    /// helpers intentionally keep the legacy command mask.
    pub fn get_speed_pwm_raw_channels(&self) -> (u32, u32) {
        (
            self.regs.read_reg(REG_FAN_PWM0),
            self.regs.read_reg(REG_FAN_PWM1),
        )
    }

    /// Read the bounded raw fan-control register window for diagnostics.
    ///
    /// This is intentionally read-only and limited to aligned offsets
    /// `0x00..=0x7C`, so one-shot diagnostics can see mirror/alias registers
    /// such as `0x18`, `0x1C`, and `0x20` without probing unknown space.
    pub fn raw_register_dump(&self) -> Vec<FanRawRegister> {
        fan_raw_dump_offsets()
            .map(|offset| FanRawRegister {
                offset,
                value: self.regs.read_reg(offset),
            })
            .collect()
    }

    /// Get the current speed as a percentage (0-100).
    /// PWM is already in 0-100 units on BraiinsOS fan_ctrl, so this is identity
    /// (clamped to the 0-100 envelope for UI safety).
    pub fn get_speed_percent(&self) -> u8 {
        self.get_speed_pwm().min(PWM_MAX)
    }

    /// Check if any fan is spinning (RPM > 0 on any channel).
    pub fn is_spinning(&self) -> bool {
        self.get_rpm() > 0
    }

    /// Internal: read RPS values for every channel the variant supports.
    fn read_all_rps(&self) -> Vec<u32> {
        match self.variant {
            FanVariant::Am1S9 => vec![
                self.regs.read_reg(REG_FAN0_RPS),
                self.regs.read_reg(REG_FAN1_RPS),
            ],
            FanVariant::Am2Uio16 => vec![
                self.regs.read_reg(REG_FAN0_RPS),
                self.regs.read_reg(REG_FAN1_RPS),
                self.regs.read_reg(REG_FAN2_RPS),
                self.regs.read_reg(REG_FAN3_RPS),
            ],
        }
    }

    /// Get raw tach/RPS-like readings by channel before RPM conversion.
    pub fn get_raw_rps_channels(&self) -> Vec<(u8, u32)> {
        self.read_all_rps()
            .into_iter()
            .enumerate()
            .map(|(idx, rps)| (idx as u8, rps))
            .collect()
    }

    /// Get per-fan RPM readings.
    ///
    /// Returns (fan_id, rpm) for each fan channel that reports a non-zero
    /// tachometer reading. On am1-s9, FAN0 is typically unwired (PCB
    /// limitation) — we suppress it to avoid a dead gauge in the UI.
    /// On am2-s17, all 4 fans are wired and all are exposed, even at 0 RPM
    /// (so a single failed fan shows as 0 without disappearing).
    ///
    /// SAFETY: the am2 path does NOT filter out 0-RPM channels — because if
    /// fan 2 dies while 0/1/3 spin, the gauge must remain visible with
    /// rpm=0 so the thermal controller's FanFailure path fires.
    pub fn get_per_fan_rpm(&self) -> Vec<(u8, u32)> {
        let rps_list = self.read_all_rps();
        per_fan_rpm_from_rps(self.variant, &rps_list)
    }
}

fn per_fan_rpm_from_rps(variant: FanVariant, rps_list: &[u32]) -> Vec<(u8, u32)> {
    let mut fans = Vec::new();
    match variant {
        FanVariant::Am1S9 => {
            // S9 suppresses unwired fan 0 (common hardware configuration).
            if rps_list.first().copied().unwrap_or(0) > 0 {
                fans.push((0, rps_list[0] * 60));
            }
            if let Some(&rps1) = rps_list.get(1) {
                fans.push((1, rps1 * 60));
            }
        }
        FanVariant::Am2Uio16 => {
            // All 4 channels exposed so a failed fan is visible (not hidden).
            for (idx, rps) in rps_list.iter().copied().enumerate() {
                fans.push((idx as u8, rps * 60));
            }
        }
    }
    fans
}

fn parse_front_fan_offset(raw: &str) -> std::result::Result<u32, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty offset".to_string());
    }
    match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16).map_err(|e| e.to_string()),
        None => trimmed.parse::<u32>().map_err(|e| e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Compile-time check that the safety cap is below the IP ceiling.
    #[test]
    fn pwm_safety_cap_below_ip_max() {
        assert!(PWM_SAFETY_MAX <= PWM_MAX);
        assert!(PWM_INDUSTRIAL_MAX <= PWM_MAX);
        assert!(PWM_QUIET_BOOT <= PWM_SAFETY_MAX);
    }

    /// Regression: PWM_MAX must remain 0-100 scale. Reverting to 127 would
    /// break am2 BraiinsOS fan_ctrl (panics on >100).
    #[test]
    fn pwm_max_is_one_hundred() {
        assert_eq!(PWM_MAX, 100);
    }

    /// Regression: the home-safe fan cap is a LOAD-BEARING VALUE (30), not merely
    /// "<= PWM_MAX". The rust-firmware rule mandates PWM 30 (never 127/100) for
    /// home mining, and every safety/emergency fan write funnels through it —
    /// `fan_safety_override` commands `FAN_PWM_SAFETY = PWM_SAFETY_MAX`, and the
    /// daemon's safety paths use `cfg_fan_max_pwm.min(PWM_SAFETY_MAX)`. The
    /// pre-existing `PWM_SAFETY_MAX <= PWM_MAX` check would still pass if someone
    /// bumped this to 90 — which would silently un-cap (blast) every safety fan
    /// path. Pin the exact value so that regression fails here.
    /// Must match `dcentrald_common::HOME_FAN_PWM_SAFETY_MAX` / FanCommand policy.
    #[test]
    fn pwm_safety_max_matches_common_home_cap() {
        assert_eq!(
            PWM_SAFETY_MAX,
            dcentrald_common::HOME_FAN_PWM_SAFETY_MAX,
            "HAL PWM_SAFETY_MAX drifted from dcentrald-common home cap"
        );
    }

    #[test]
    fn pwm_safety_max_is_thirty() {
        assert_eq!(
            PWM_SAFETY_MAX, 30,
            "home-safe fan cap must stay PWM 30 (fan-never-blast rule)"
        );
    }

    /// The final floor-level clamp must cap at the IP ceiling (the BraiinsOS
    /// fan_ctrl IP panics on > 100). `set_speed`/`set_speed_channels` both run
    /// every commanded value through this before writing the register.
    #[test]
    fn clamp_pwm_caps_at_ip_ceiling() {
        assert_eq!(clamp_pwm(0), 0);
        assert_eq!(clamp_pwm(PWM_QUIET_BOOT), PWM_QUIET_BOOT);
        assert_eq!(clamp_pwm(PWM_SAFETY_MAX), PWM_SAFETY_MAX);
        assert_eq!(clamp_pwm(PWM_MAX), PWM_MAX);
        assert_eq!(clamp_pwm(101), PWM_MAX);
        assert_eq!(clamp_pwm(PWM_MAX_LEGACY_7BIT), PWM_MAX);
        assert_eq!(clamp_pwm(u8::MAX), PWM_MAX);
    }

    #[test]
    fn profile_cap_clamp_defaults_to_ip_ceiling_and_can_lower_it() {
        for requested in [0, 10, 30, 64, 100, 101, 127, 200, 255] {
            assert_eq!(
                clamp_pwm_with_profile_cap(requested, PWM_MAX),
                clamp_pwm(requested),
                "default profile cap must preserve the historical IP-ceiling clamp"
            );
            assert!(
                clamp_pwm_with_profile_cap(requested, PWM_SAFETY_MAX) <= PWM_SAFETY_MAX,
                "profile cap must lower requested PWM {requested} to the home safety ceiling"
            );
        }
        assert_eq!(clamp_pwm_with_profile_cap(100, 30), 30);
        assert_eq!(clamp_pwm_with_profile_cap(25, 30), 25);
        assert_eq!(clamp_pwm_with_profile_cap(255, 255), PWM_MAX);
    }

    #[test]
    fn am2_c52_fan_mode_status_records_mode() {
        assert!(
            open_with_variant_should_enable_am2_c52(FanVariant::Am2Uio16),
            "AM2 open_with_variant must attempt the C49->C52 fan-mode switch"
        );
        assert!(
            !open_with_variant_should_enable_am2_c52(FanVariant::Am1S9),
            "S9 must not attempt the AM2 board-control C52 switch"
        );
    }

    #[test]
    fn fan_variant_topology_pins_physical_tach_and_pwm_channels() {
        assert_eq!(FanVariant::Am1S9.physical_fan_count(), 2);
        assert_eq!(FanVariant::Am1S9.tach_channel_count(), 2);
        assert_eq!(FanVariant::Am1S9.pwm_command_channel_count(), 2);
        assert_eq!(FanVariant::Am1S9.tach_channel_for_physical_fan(0), None);
        assert_eq!(FanVariant::Am1S9.tach_channel_for_physical_fan(1), Some(1));

        assert_eq!(FanVariant::Am2Uio16.physical_fan_count(), 4);
        assert_eq!(FanVariant::Am2Uio16.tach_channel_count(), 4);
        assert_eq!(FanVariant::Am2Uio16.pwm_command_channel_count(), 2);
        for physical_fan in 0..4 {
            assert_eq!(
                FanVariant::Am2Uio16.tach_channel_for_physical_fan(physical_fan),
                Some(physical_fan)
            );
        }

        assert_eq!(
            per_fan_rpm_from_rps(FanVariant::Am1S9, &[0, 42]),
            vec![(1, 2520)],
            "S9 suppresses the commonly-unwired fan 0 tach channel"
        );
        assert_eq!(
            per_fan_rpm_from_rps(FanVariant::Am2Uio16, &[0, 42, 0, 1]),
            vec![(0, 0), (1, 2520), (2, 0), (3, 60)],
            "AM2 must expose all four fans, including stalled zero-RPM channels"
        );
    }

    #[test]
    fn tach_snapshot_requires_motion_on_every_observable_physical_channel() {
        let s9 = FanTachSnapshot {
            variant: FanVariant::Am1S9,
            rpm_by_physical_fan: vec![None, Some(2_400)],
        };
        assert!(s9.all_required_channels_moving());

        let one_stalled_am2 = FanTachSnapshot {
            variant: FanVariant::Am2Uio16,
            rpm_by_physical_fan: vec![Some(2_400), Some(2_300), Some(0), Some(2_200)],
        };
        assert!(!one_stalled_am2.all_required_channels_moving());

        let all_moving_am2 = FanTachSnapshot {
            variant: FanVariant::Am2Uio16,
            rpm_by_physical_fan: vec![Some(2_400), Some(2_300), Some(2_100), Some(2_200)],
        };
        assert!(all_moving_am2.all_required_channels_moving());
    }

    #[test]
    fn open_with_variant_calls_am2_c52_switch() {
        let source = include_str!("fan.rs");
        let body = source
            .split("pub fn open_with_variant")
            .nth(1)
            .and_then(|rest| rest.split("pub fn open_discovered").next())
            .expect("open_with_variant body must be visible to the source pin");
        assert!(
            body.contains("enable_am2_c52_fan_mode_from_board_control"),
            "removing the C52 switch from open_with_variant must fail a host test"
        );
    }

    #[test]
    fn front_fan_offset_parser_accepts_decimal_and_hex() {
        assert_eq!(parse_front_fan_offset("16").unwrap(), 16);
        assert_eq!(parse_front_fan_offset("0x10").unwrap(), 0x10);
        assert_eq!(parse_front_fan_offset("0X14").unwrap(), 0x14);
        assert_eq!(parse_front_fan_offset(" 32 ").unwrap(), 32);
        assert!(parse_front_fan_offset("").is_err());
        assert!(parse_front_fan_offset("not-an-offset").is_err());
    }

    /// Regression check for the am2 4-channel layout.
    #[test]
    fn am2_register_offsets() {
        assert_eq!(REG_FAN0_RPS, 0x00);
        assert_eq!(REG_FAN1_RPS, 0x04);
        assert_eq!(REG_FAN2_RPS, 0x08);
        assert_eq!(REG_FAN3_RPS, 0x0C);
        assert_eq!(REG_FAN_PWM0, 0x10);
        assert_eq!(REG_FAN_PWM1, 0x14);
    }

    #[test]
    fn raw_dump_offsets_are_aligned_bounded_and_include_mirrors() {
        let offsets = fan_raw_dump_offsets().collect::<Vec<_>>();
        assert_eq!(offsets.first().copied(), Some(0x00));
        assert_eq!(offsets.last().copied(), Some(0x7C));
        assert_eq!(offsets.len(), 32);
        assert!(offsets.iter().all(|offset| offset % 4 == 0));
        assert!(offsets.contains(&0x18));
        assert!(offsets.contains(&0x1C));
        assert!(offsets.contains(&0x20));
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dcentrald-hal-fan-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn write_uio_name(root: &std::path::Path, number: u8, name: &str) {
        let dir = root.join(format!("uio{}", number));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("name"), name).unwrap();
    }

    #[test]
    fn discover_fan_uio_am1_when_only_fan_control_exists() {
        let root = unique_temp_dir("am1");
        write_uio_name(&root, 4, "fan-control");

        let found = discover_fan_uio_in_dir(&root).expect("fan-control should be found");
        assert_eq!(found.uio_number, 4);
        assert_eq!(found.variant, FanVariant::Am1S9);
        assert!(!found.has_board_control);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discover_fan_uio_am2_when_board_control_exists() {
        let root = unique_temp_dir("am2");
        write_uio_name(&root, 17, "board-control");
        write_uio_name(&root, 16, "fan-control");

        let found = discover_fan_uio_in_dir(&root).expect("fan-control should be found");
        assert_eq!(found.uio_number, 16);
        assert_eq!(found.variant, FanVariant::Am2Uio16);
        assert!(found.has_board_control);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discover_fan_uio_uses_lowest_fan_control_number() {
        let root = unique_temp_dir("lowest");
        write_uio_name(&root, 18, "fan-control");
        write_uio_name(&root, 16, "fan-control");
        write_uio_name(&root, 17, "board-control");

        let found = discover_fan_uio_in_dir(&root).expect("fan-control should be found");
        assert_eq!(found.uio_number, 16);
        assert_eq!(found.variant, FanVariant::Am2Uio16);

        let _ = std::fs::remove_dir_all(root);
    }

    // `discover_uio_number_by_name_uses_lowest_match` was migrated to
    // `crate::uio_discover::tests::finds_lowest_numbered_match` in
    // (2026-05-23) when the function moved to its own module.
}
