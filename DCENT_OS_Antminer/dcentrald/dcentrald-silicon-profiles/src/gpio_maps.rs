//! 9 (2026-05-09): Per-control-board GPIO maps.
//!
//! Source-cite: `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/`
//! `DCENT_OS_HARDWARE_CATALOG.md` §8 (lines 591-691).
//!
//! Five control-board platforms ship across the Antminer line:
//!
//! - **CV1835** (Cvitek CV1835, S19 Pro / S19j Pro stock CVCtrl) — §8.1.
//! - **AM335x** (TI BeagleBone-class, VNish bb / stock BBCtrl) — §8.2.
//! - **Amlogic S905** (S19j Pro+ stock AMLCtrl, VNish aml, S21 stock) —
//!   §8.3.
//! - **Zynq 7007S** (VNish xil, S17/S19j Pro Zynq am2 variants) — §8.4.
//! - **Braiins BBB** (Braiins OS BeagleBone Black porting board) —
//!   §8.5.
//!
//! Every platform has its own PSU enable pin, hashboard reset pins,
//! plug-detect pins, fan tachometer pins, and LED pins. RE2 §8.6 cross-
//! references the PSU enable pin specifically; this module is the
//! authoritative table for the rest.
//!
//! Routing rules:
//! - The **stock** CV1835 `S19j Pro` PSU enable is GPIO 412 (PWR_EN).
//! - The **VNish bb** AM335x `S19j Pro` PSU enable is GPIO 65 (psu_en).
//! - The **VNish aml / stock AMLCtrl** Amlogic `S19j Pro+` PSU enable
//!   is GPIO 437 (pwr_en).
//! - The **VNish xil / S21 stock** PSU enable is GPIO 907 (psu_en).
//! - The **Braiins BBB** PSU enable is mediated by the FPGA board
//!   control block at `0x42810000`, NOT a sysfs GPIO.

use serde::{Deserialize, Serialize};

/// Single GPIO assignment. `Some(n)` if the platform exposes the
/// function, `None` if the function lives in FPGA fabric or isn't
/// applicable.
pub type GpioPin = Option<u32>;

// ---------------------------------------------------------------------------
// A34 (goldmine 2026-06-10): fan-RPM soft-limit advisory data.
// ---------------------------------------------------------------------------
//
// Per-fan RPM soft ceilings recorded from the stock fan topology
// (`topol.conf`): index 0 = front fan, index 1 = rear fan.
//
// ⚠️ SOFT-WARNING / advisory telemetry ONLY. `fan_rpm_soft_warning` is a pure
// function that NEVER changes fan PWM, NEVER triggers a cutoff, and is fully
// independent of the home PWM-30 cap and the cut-hash-before-noise safety
// posture. A reading above a ceiling is surfaced as an advisory string, not a
// fault — the thermal/fan controllers remain the only owners of PWM action.

/// Per-fan RPM soft ceilings: `[front, rear]`. Advisory only (see
/// [`fan_rpm_soft_warning`]).
pub const FAN_MAX_RPM: [u16; 2] = [6000, 4300];

/// Pure, side-effect-free fan-RPM sanity check. Returns `Some(advisory)` when
/// `rpm` exceeds the soft ceiling for the given fan position, `None` otherwise.
///
/// `fan_index` 0 = front, 1 = rear; an out-of-range index conservatively uses
/// the lowest ceiling. This is advisory ONLY — it never touches PWM, never cuts
/// power, and is independent of the PWM-30 home cap.
pub fn fan_rpm_soft_warning(fan_index: usize, rpm: u16) -> Option<String> {
    let ceiling = FAN_MAX_RPM
        .get(fan_index)
        .copied()
        .unwrap_or_else(|| FAN_MAX_RPM.iter().copied().min().unwrap_or(u16::MAX));
    if rpm > ceiling {
        Some(format!(
            "fan {} reading {} RPM exceeds soft ceiling {} RPM \
             (advisory only — no PWM action taken)",
            fan_index, rpm, ceiling
        ))
    } else {
        None
    }
}

/// Cvitek CV1835 GPIO map (S19j Pro stock CVCtrl). RE2 §8.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cv1835GpioMap {
    /// GPIO 412 — PSU backstop / hashboard power enable.
    pub pwr_en: u32,
    /// GPIO 406 — IP detection button.
    pub ip_get: u32,
    /// GPIO 427/429/431/433 — chain 0..=3 ASIC reset.
    pub asic_rst: [u32; 4],
    /// GPIO 434/435 — red/green LEDs.
    pub led_red: u32,
    pub led_green: u32,
    /// GPIO 447 — recovery button (active-low factory reset).
    pub recovery_btn: u32,
    /// GPIO 459/461 — bit-bang I²C (SCL/SDA) fallback.
    pub i2c_scl_bb: u32,
    pub i2c_sda_bb: u32,
    /// GPIO 907 — secondary PSU enable on platforms that wire both
    /// pins (some CV1835 carriers ship a secondary control via the
    /// 907 line).
    pub psu_en_secondary: GpioPin,
}

impl Cv1835GpioMap {
    /// Canonical CV1835 GPIO map per RE2 §8.1.
    pub const STANDARD: Cv1835GpioMap = Cv1835GpioMap {
        pwr_en: 412,
        ip_get: 406,
        asic_rst: [427, 429, 431, 433],
        led_red: 434,
        led_green: 435,
        recovery_btn: 447,
        i2c_scl_bb: 459,
        i2c_sda_bb: 461,
        psu_en_secondary: Some(907),
    };
}

/// TI AM335x GPIO map (BeagleBone-class — VNish bb / stock BBCtrl).
/// RE2 §8.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Am335xGpioMap {
    /// GPIO 65 — PSU power enable (active-high).
    pub psu_en: u32,
    /// GPIO 23 / 45 — red/green LEDs (from blink script).
    pub led_red: u32,
    pub led_green: u32,
    /// Fan tachometer GPIOs — front fan 0/1, rear fan 0/1.
    /// 7 / 20 / 110 / 112 per RE2 §8.2 lines 619-622.
    pub fan_tachs: [u32; 4],
}

impl Am335xGpioMap {
    /// Canonical AM335x GPIO map per RE2 §8.2.
    pub const STANDARD: Am335xGpioMap = Am335xGpioMap {
        psu_en: 65,
        led_red: 23,
        led_green: 45,
        fan_tachs: [112, 110, 7, 20],
    };
}

/// Amlogic S905 GPIO map (S19j Pro+ stock AMLCtrl / S21 stock / VNish
/// aml). RE2 §8.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmlogicGpioMap {
    /// GPIO 437 — hashboard power enable (active-high).
    pub pwr_en: u32,
    /// GPIO 446 — recovery button.
    pub recovery: u32,
    /// GPIO 445 — IP detection.
    pub ip_get: u32,
    /// GPIO 439/440/441 — chain 0..=2 plug detect (active-high,
    /// pulldown).
    pub plug_detect: [u32; 3],
    /// GPIO 454/455/456 — chain 0..=2 reset.
    pub asic_rst: [u32; 3],
    /// GPIO 447/448/449/450 — front 0/1, rear 0/1 fan tachs.
    pub fan_tachs: [u32; 4],
    /// GPIO 453 — green LED, GPIO 438 — red LED.
    pub led_green: u32,
    pub led_red: u32,
}

impl AmlogicGpioMap {
    /// Canonical Amlogic S905 GPIO map per RE2 §8.3.
    pub const STANDARD: AmlogicGpioMap = AmlogicGpioMap {
        pwr_en: 437,
        recovery: 446,
        ip_get: 445,
        plug_detect: [439, 440, 441],
        asic_rst: [454, 455, 456],
        fan_tachs: [447, 448, 449, 450],
        led_green: 453,
        led_red: 438,
    };
}

/// Xilinx Zynq-7007S GPIO map (VNish xil / S19j Pro am2 variants).
/// RE2 §8.4.
///
/// **NOTE:** RE2 §8.4 line 659 — "All fan control, chain GPIOs,
/// hashboard control are in FPGA fabric (not sysfs GPIOs)". Only the
/// PSU enable + 2 LED pins are sysfs-visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZynqGpioMap {
    /// GPIO 907 — PSU power enable.
    pub psu_en: u32,
    /// GPIO 942 — green LED. (Stock BHB42XXX firmware drives 942=GREEN,
    /// 941=RED; see `STANDARD` for provenance.)
    pub led_green: u32,
    /// GPIO 941 — red LED. (Stock BHB42XXX firmware drives 941=RED.)
    pub led_red: u32,
    /// GPIO 921 — recovery button. Per RE2 §2.1 line 96 (S9k) and
    /// §8.4 (VNish xil); standard across XC7Z007S boards even when
    /// used with VNish.
    pub recovery: u32,
}

impl ZynqGpioMap {
    /// Canonical Zynq-7007S GPIO map per RE2 §8.4.
    ///
    /// LED colors corrected 2026-06-10 (HashSource goldmine): the stock
    /// BHB42XXX firmware (S70cgminer comments, S37bitmainer_setup variable
    /// names, and the bmminer embedded JSON — three independent sources)
    /// drives **GPIO 941 = RED, GPIO 942 = GREEN**. The prior RE2-sourced
    /// values had them inverted (941=green/942=red). LEDs are cosmetic
    /// (no mining/thermal/voltage impact); a live LED check on a `a lab unit`/`a lab unit`
    /// unit can confirm visually if ever in doubt.
    pub const STANDARD: ZynqGpioMap = ZynqGpioMap {
        psu_en: 907,
        led_green: 942,
        led_red: 941,
        recovery: 921,
    };
}

/// Braiins OS BeagleBone Black GPIO map (Braiins porting board for S9
/// / S17 / S19 via BBB carrier). RE2 §8.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BraiinsBbbGpioMap {
    /// GPIO 51/48/47/44 — chain 0..=3 plug detect.
    pub plug: [u32; 4],
    /// GPIO 5/4/27/22 — chain 0..=3 ASIC reset.
    pub asic_rst: [u32; 4],
    /// GPIO 23 / 45 — red/green LEDs.
    pub led_red: u32,
    pub led_green: u32,
    /// GPIO 26 — IP detection signal (rising edge).
    pub ip_sig: u32,
    /// GPIO 46 — recovery button (rising edge).
    pub recovery: u32,
    /// Fan tachs — front 0/1, rear 0/1. 7 / 20 / 110 / 112 (matches
    /// the AM335x layout one-to-one — same SoC family, same tachs).
    pub fan_tachs: [u32; 4],
}

impl BraiinsBbbGpioMap {
    /// Canonical Braiins BBB GPIO map per RE2 §8.5.
    pub const STANDARD: BraiinsBbbGpioMap = BraiinsBbbGpioMap {
        plug: [51, 48, 47, 44],
        asic_rst: [5, 4, 27, 22],
        led_red: 23,
        led_green: 45,
        ip_sig: 26,
        recovery: 46,
        fan_tachs: [112, 110, 7, 20],
    };
}

/// Tagged enum that wraps the platform-specific GPIO maps so callers
/// holding "any control board" can pattern-match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlBoardGpioMap {
    Cv1835(Cv1835GpioMap),
    Am335x(Am335xGpioMap),
    Amlogic(AmlogicGpioMap),
    Zynq(ZynqGpioMap),
    BraiinsBbb(BraiinsBbbGpioMap),
}

impl ControlBoardGpioMap {
    /// PSU enable GPIO pin for this platform.
    pub const fn psu_enable_pin(&self) -> u32 {
        match self {
            ControlBoardGpioMap::Cv1835(m) => m.pwr_en,
            ControlBoardGpioMap::Am335x(m) => m.psu_en,
            ControlBoardGpioMap::Amlogic(m) => m.pwr_en,
            ControlBoardGpioMap::Zynq(m) => m.psu_en,
            // Braiins BBB PSU enable is FPGA-mediated (board control
            // block 0x42810000 per RE2 §8.6 row 5). Return 0 as a
            // sentinel — callers must handle this specially.
            ControlBoardGpioMap::BraiinsBbb(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cv1835_pwr_en_is_412() {
        // RE2 §8.1 — CV1835 PSU enable is GPIO 412.
        assert_eq!(Cv1835GpioMap::STANDARD.pwr_en, 412);
    }

    #[test]
    fn fan_rpm_soft_warning_is_advisory_only() {
        // A34 (goldmine 2026-06-10): a reading above the soft ceiling emits a
        // warning string, NOT a fault/cutoff. Front ceiling = 6000, rear = 4300.
        assert_eq!(FAN_MAX_RPM, [6000, 4300]);
        assert!(fan_rpm_soft_warning(0, 7000).is_some()); // front over 6000
        assert!(fan_rpm_soft_warning(0, 5000).is_none()); // front under 6000
        assert!(fan_rpm_soft_warning(1, 5000).is_some()); // rear over 4300
        assert!(fan_rpm_soft_warning(1, 4000).is_none()); // rear under 4300
                                                          // Out-of-range index uses the most conservative (lowest) ceiling.
        assert!(fan_rpm_soft_warning(9, 7000).is_some());
    }

    #[test]
    fn cv1835_full_pin_map_pinned() {
        // Pin every CV1835 pin per RE2 §8.1 lines 596-607.
        let m = Cv1835GpioMap::STANDARD;
        assert_eq!(m.pwr_en, 412);
        assert_eq!(m.ip_get, 406);
        assert_eq!(m.asic_rst, [427, 429, 431, 433]);
        assert_eq!(m.led_red, 434);
        assert_eq!(m.led_green, 435);
        assert_eq!(m.recovery_btn, 447);
        assert_eq!(m.i2c_scl_bb, 459);
        assert_eq!(m.i2c_sda_bb, 461);
        assert_eq!(m.psu_en_secondary, Some(907));
    }

    #[test]
    fn amlogic_pwr_en_is_437() {
        // RE2 §8.3 — Amlogic S905 PSU enable is GPIO 437.
        assert_eq!(AmlogicGpioMap::STANDARD.pwr_en, 437);
    }

    #[test]
    fn amlogic_chain_resets_pinned() {
        // RE2 §8.3 lines 639-641 — chain reset pins 454/455/456.
        assert_eq!(AmlogicGpioMap::STANDARD.asic_rst, [454, 455, 456]);
    }

    #[test]
    fn amlogic_plug_detect_pinned() {
        // RE2 §8.3 lines 636-638 — chain plug-detect pins 439/440/441.
        assert_eq!(AmlogicGpioMap::STANDARD.plug_detect, [439, 440, 441]);
    }

    #[test]
    fn am335x_psu_en_is_65() {
        // RE2 §8.2 line 617 — AM335x PSU enable is GPIO 65 (active-1).
        assert_eq!(Am335xGpioMap::STANDARD.psu_en, 65);
    }

    #[test]
    fn am335x_fan_tach_pins_pinned() {
        // RE2 §8.2 lines 619-622 — front/rear tachs are 7 / 20 / 110 /
        // 112.
        let tachs = Am335xGpioMap::STANDARD.fan_tachs;
        // Order: front 0, front 1, rear 0, rear 1.
        assert_eq!(tachs, [112, 110, 7, 20]);
    }

    #[test]
    fn zynq_psu_en_is_907() {
        // RE2 §8.4 line 656 — Zynq-7007S PSU enable is GPIO 907.
        assert_eq!(ZynqGpioMap::STANDARD.psu_en, 907);
    }

    #[test]
    fn zynq_led_pins_pinned() {
        // Stock BHB42XXX firmware: 942=GREEN, 941=RED (HashSource goldmine
        // 2026-06-10; corrects the inverted RE2 §8.4 657-658 values).
        assert_eq!(ZynqGpioMap::STANDARD.led_green, 942);
        assert_eq!(ZynqGpioMap::STANDARD.led_red, 941);
    }

    #[test]
    fn zynq_recovery_is_921() {
        assert_eq!(ZynqGpioMap::STANDARD.recovery, 921);
    }

    #[test]
    fn braiins_bbb_plug_pins_pinned() {
        // RE2 §8.5 lines 666-669 — chain plug 51/48/47/44.
        assert_eq!(BraiinsBbbGpioMap::STANDARD.plug, [51, 48, 47, 44]);
    }

    #[test]
    fn braiins_bbb_reset_pins_pinned() {
        // RE2 §8.5 lines 670-673 — chain reset 5/4/27/22.
        assert_eq!(BraiinsBbbGpioMap::STANDARD.asic_rst, [5, 4, 27, 22]);
    }

    #[test]
    fn braiins_bbb_psu_enable_returns_zero_sentinel() {
        // Braiins BBB routes PSU enable through the FPGA board control
        // block; there's no sysfs GPIO. The wrapper returns 0 as a
        // sentinel so callers know to switch paths.
        let bbb = ControlBoardGpioMap::BraiinsBbb(BraiinsBbbGpioMap::STANDARD);
        assert_eq!(bbb.psu_enable_pin(), 0);
    }

    #[test]
    fn psu_enable_pin_routes_correctly_per_platform() {
        // Verify the wrapper picks up the canonical pin from each
        // platform map.
        assert_eq!(
            ControlBoardGpioMap::Cv1835(Cv1835GpioMap::STANDARD).psu_enable_pin(),
            412
        );
        assert_eq!(
            ControlBoardGpioMap::Am335x(Am335xGpioMap::STANDARD).psu_enable_pin(),
            65
        );
        assert_eq!(
            ControlBoardGpioMap::Amlogic(AmlogicGpioMap::STANDARD).psu_enable_pin(),
            437
        );
        assert_eq!(
            ControlBoardGpioMap::Zynq(ZynqGpioMap::STANDARD).psu_enable_pin(),
            907
        );
    }

    #[test]
    fn psu_enable_pins_are_distinct_across_platforms() {
        // Pin the routing-critical fact: every platform's PSU enable
        // pin is different. If any two collide, install preflight
        // can't disambiguate.
        let pins = [
            Cv1835GpioMap::STANDARD.pwr_en,
            Am335xGpioMap::STANDARD.psu_en,
            AmlogicGpioMap::STANDARD.pwr_en,
            ZynqGpioMap::STANDARD.psu_en,
        ];
        let mut seen = std::collections::HashSet::new();
        for p in pins {
            assert!(seen.insert(p), "duplicate PSU enable pin: {}", p);
        }
    }

    #[test]
    fn cv1835_secondary_psu_enable_is_907() {
        // Some CV1835 carriers wire 907 as a secondary; pin so the
        // wiring assumption is documented in code.
        assert_eq!(Cv1835GpioMap::STANDARD.psu_en_secondary, Some(907));
    }
}
