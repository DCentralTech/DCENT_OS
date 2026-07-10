//! VNish AML cold-boot phase machine (data-only) — W15.D2.
//!
//! Ports the byte-exact phase trace + GPIO/PWM/I²C topology from the
//!  RE deliverables into a Rust data structure that downstream
//! orchestration code can consume without re-reading the C HAL by hand.
//! No phase action method actually drives hardware — this module is
//! intentionally **data-only**, mirroring `cvitek_cold_boot.rs` (W12.5)
//! style. A future wave will wire a real driver behind an env-gated
//! orchestrator entry point once a live VNish AML bench unit is
//! available for verification.
//!
//! ## Source of truth
//!
//! - **** —
//!   VNish rcS sequence (S11board/S70miner/S80dashd/S99vnmon-updater),
//!    Q10 PWR_EN active-HIGH RESOLVED, W4 handoff cold-boot
//!   8-phase enum cite.
//! - **W4 handoff `vnish_aml_board.h` (cited in the boot-chain doc)** —
//!   GPIO defines: PWR_EN=437, CH0/1/2_PLUG=439/440/441,
//!   CH0/1/2_RST=454/455/456, LED_GREEN=453, LED_RED=438,
//!   GPIO_RECOVERY=446, IP_GET=445, FAN_FRONT_SPEED0/1=447/448,
//!   FAN_REAR_SPEED0/1=449/450.
//! - **W4 handoff `vnish_aml_hal_summary.md` (cited in the boot-chain doc)** —
//!   8-phase init enum, PWM mapping (`/sys/class/pwm/pwmchip0/pwm0` and
//!   `pwm1`), I²C bus (`/dev/i2c-0`), 3 chains, NO `uart_trans.ko`.
//! - **W4 handoff `bmminer_init_trace_aml.md` (cited in the boot-chain doc)** —
//!   220-line VNish cgminer init timing trace (t=1.5s..t=5.0s).
//! - **W14.D rule ** —
//!   same A113D silicon, NEVER register a 4th `Platform` enum variant.
//! - **`DCENT_OS_Antminer/ "Two firmware states on identical
//!   hardware" + Amlogic GPIO map** — PWR_EN active-HIGH per  Q10.
//! - **W15.A6** — PWR_EN polarity correction in stock am3-aml platform doc.
//!
//! ## Status
//!
//! **INFERRED — code-only, hardware-gated.** No live VNish AML bench
//! unit on the production fleet (2026-05-10). The constants here are
//! ports of the W4 handoff data, never live-verified end-to-end on a
//! VNish-installed S19j Pro AML chassis under DCENT_OS. Every "INFERRED"
//! marker stays in place until R5+ delivers a bench-unit operator
//! capture matching this trace.
//!
//! Production code paths never read these constants. The env-gate
//! `DCENT_AML_VNISH_ACCEPT_INFERRED` MUST stay default-off until a
//! live VNish AML bench unit operator captures + replays this exact
//! sequence and the markers can be promoted (mirrors
//! `cvitek_cold_boot.rs::ACCEPT_INFERRED_SOC_REGS_ENV` discipline +
//!  for hardware-acq
//! discipline).
//!
//! ## Phase map (matches `vnish_aml_hal_summary.md` + `bmminer_init_trace_aml.md`)
//!
//! Each phase carries the t= timestamp from the trace plus the
//! observable side-effect. Times are advisory — the W4 trace did not
//! pin them with sub-100 ms precision; the cadence is "S11board
//! finishes at t≈1.5s, cgminer settles at t≈5s, first WORK_TX at the
//! tail end".
//!
//! | Phase | t       | Action                                              | Confidence |
//! |------:|---------|-----------------------------------------------------|------------|
//! | 1     | 1.5 s   | GPIO export (15 pins) + PWR_EN HIGH ( Q10).   | INFERRED   |
//! | 2     | 1.5 s   | PWM init: `pwmchip0/pwm0` + `pwm1` enabled.         | INFERRED   |
//! | 3     | 2.0 s   | cgminer launch (NO `sleep 5`, NO bmminer).          | INFERRED   |
//! | 4     | 2.5 s   | cgminer init (config load + chain bring-up).        | INFERRED   |
//! | 5     | 3.0 s   | I²C `/dev/i2c-0` open + per-chain probe (0x20+ch).  | INFERRED   |
//! | 6     | 3.5 s   | PWM front + rear fans → 100 % duty.                 | INFERRED   |
//! | 7     | 4.0 s   | ASIC RST de-assert: 454→455→456 with 10 ms stagger. | INFERRED   |
//! | 8     | 4.5 s   | Chain discovery via PLUG GPIOs 439/440/441.         | INFERRED   |
//! | 9     | 5.0 s   | LED set (453=green ON, 438=red OFF) + first WORK_TX.| INFERRED   |
//!
//! Note: the W4 handoff's 8-phase enum collapses phases 8 and 9 into a
//! single `MINING_START` step. We split them here because the GPIO
//! semantics differ (read-only chain detect vs. write LEDs + first
//! work) and the timeline distinguishes them.
//!
//! ## Memory rules honored
//!
//! -  —
//!   we do NOT define a new `Platform` variant; this module is
//!   namespaced under the existing `amlogic` platform.
//! -  — env-gate stays
//!   default-off until R5+ hardware lands.
//! -  — phase 5's I²C probes
//!   target 0x20+chain (PIC1704 / NoPic surface). EEPROM addresses
//!   0x50..=0x57 are HAL-protected; this module never touches them.
//! -  — phase 6's fan-init
//!   commitment is documented but the actual 100 % duty cycle is
//!   gated by the live PWM driver path (not this data module).

use std::time::Duration;

// ---------------------------------------------------------------------------
// Public env-gate
// ---------------------------------------------------------------------------

/// Canonical env-gate that must be `=1` to allow any code path to
/// orchestrate the VNish AML cold-boot routine described by this
/// module's data tables.
///
/// Set by an operator who has accepted the W15.D2 INFERRED risk: every
/// constant in this file ports W4 handoff data that has never been
/// live-verified end-to-end on a VNish-installed S19j Pro AML chassis
/// under DCENT_OS. Production code paths never set this.
///
/// Mirrors `cvitek_cold_boot.rs::ACCEPT_INFERRED_SOC_REGS_ENV` discipline
/// and .
pub const ACCEPT_INFERRED_ENV: &str = "DCENT_AML_VNISH_ACCEPT_INFERRED";

/// Test if the env-gate is currently `=1`. The orchestrator (when one
/// is wired in a future wave) must call this and refuse to proceed if
/// it returns `false`.
pub fn env_gate_is_set() -> bool {
    std::env::var(ACCEPT_INFERRED_ENV).ok().as_deref() == Some("1")
}

// ---------------------------------------------------------------------------
// Firmware cross-verification (2026-06-02)
// ---------------------------------------------------------------------------

/// Source firmware whose **plaintext `S11board` init script** independently
/// confirms this module's S11board-level GPIO/PWM constants byte-for-byte.
///
/// **What changed (2026-06-02):** the GPIO map + PWM channels in this module
/// were originally a port of the *W4 handoff* `vnish_aml_board.h` summary
/// (hence the `XXX: INFERRED` markers — see [`GPIO_MAP`]). They have now been
/// cross-checked against the **actual shipping VNish firmware** by extracting
/// `awesome-s21-aml-nand-v1.2.6-install.tar.gz` from the firmware archive
/// (`uramdisk.image.gz` → uImage(64B) → gzip → cpio rootfs →
/// `etc/init.d/S11board`). Every S11board GPIO export matches:
///
/// ```text
/// pwr_en 437      out, value 1   (active HIGH, NO active_low) → GPIO_PWR_EN
/// ch{0,1,2}_plug  439/440/441 in,  pull down                  → GPIO_CH*_PLUG
/// ch{0,1,2}_rst   454/455/456 out                             → GPIO_CH*_RST
/// led_green 453   out;  led_red 438 out                       → GPIO_LED_*
/// fan_front 447/448 in, falling; fan_rear 449/450 in, falling → GPIO_FAN_*
/// gpio_recovery 446 in; gpio_ip_get 445 in                    → GPIO_RECOVERY/IP_GET
/// pwm0 (rear FAN2/FAN4) + pwm1 (front FAN1/FAN3), 100 µs period→ PWM_*_CHANNEL
/// ```
///
/// This upgrades the S11board-level constants from "W4 handoff doc cite" to
/// "confirmed against the byte-exact init script of shipping firmware". The
/// `XXX: INFERRED` markers DELIBERATELY REMAIN: they specifically mean
/// "not yet live-verified end-to-end **on a bench unit under DCENT_OS**",
/// which is still true (the operator-gated `a lab unit` live A/B is still owed).
/// Firmware cross-verification is a confidence step *short of* that live A/B.
///
/// NOTE the cgminer-internal facts ([`I2C_BUS_PATH`], the ASIC-reset
/// de-assert ORDER + 10 ms stagger, the [`PHASE_TRACE`] timings) are NOT
/// in `S11board` — they remain INFERRED from the W4 cgminer trace until a
/// live capture or a cgminer-binary RE confirms them.
pub const FIRMWARE_XVERIFY_SOURCE: &str = "awesome-s21-aml-nand-v1.2.6-install.tar.gz";

/// SHA256 of [`FIRMWARE_XVERIFY_SOURCE`] in .
pub const FIRMWARE_XVERIFY_TARBALL_SHA256: &str =
    "0546cb9251f496bd1e8b52032bc0a4afc4ef659e605b84abf3ba9df5f3aa3830";

/// SHA256 of the extracted `etc/init.d/S11board` whose GPIO exports were
/// matched against this module's constants.
pub const FIRMWARE_XVERIFY_S11BOARD_SHA256: &str =
    "bbc25a2137fd35ff97d6aa545992d21e4d7d35303fe5650b1b226fdd94b249c4";

/// Date the firmware cross-verification was performed.
pub const FIRMWARE_XVERIFY_DATE: &str = "2026-06-02";

// ---------------------------------------------------------------------------
// GPIO map (15 pins total)
// ---------------------------------------------------------------------------

/// GPIO direction, in/out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDir {
    /// Input (`echo in > /sys/class/gpio/gpioN/direction`).
    In,
    /// Output (`echo out > /sys/class/gpio/gpioN/direction`).
    Out,
}

/// Logical role of an exported GPIO. Used to disambiguate same-direction
/// pins in tests and (future) orchestration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioRole {
    /// Hashboard PSU enable. Active HIGH.
    PowerEnable,
    /// LED.
    Led,
    /// Hashboard plug-detect input.
    ChainPlug,
    /// Hashboard reset output.
    ChainReset,
    /// Recovery-mode pin (held during boot to enter recovery).
    Recovery,
    /// IP-display pin (held to flash IP via LED morse).
    IpGet,
    /// Fan-tachometer input (falling edge → RPM).
    FanTach,
}

/// One GPIO pin entry from `vnish_aml_board.h`.
#[derive(Debug, Clone, Copy)]
pub struct GpioEntry {
    /// sysfs export number (matches `/sys/class/gpio/gpio<num>/`).
    pub sysfs_number: u32,
    /// Direction (`in` vs `out`).
    pub direction: GpioDir,
    /// Logical role.
    pub role: GpioRole,
    /// Default initial value for `Out` pins after export. `None` for
    /// `In` pins.
    pub initial_value: Option<u8>,
    /// Documentation string (kept short — ports the C `#define` comment).
    pub doc: &'static str,
}

/// PWR_EN GPIO sysfs export number —  Q10 RESOLVED 2026-05-10.
///
/// Active HIGH. `gpio_write(437, 1)` = PSU ON; `gpio_write(437, 0)` =
/// PSU OFF. Confirmed by direct extract of VNish v1.2.7 `S11board` init
/// script (`echo out > direction`, `echo 1 > value`, no `active_low`
/// written).
/// "PWR_EN active level — RESOLVED 2026-05-10 ( Q10)" subsection
/// + W15.A6 stock am3-aml platform doc correction.
pub const GPIO_PWR_EN: u32 = 437;

/// LED GPIO sysfs export numbers.
pub const GPIO_LED_RED: u32 = 438;
pub const GPIO_LED_GREEN: u32 = 453;

/// Per-chain PLUG (board-detect, input).
pub const GPIO_CH0_PLUG: u32 = 439;
pub const GPIO_CH1_PLUG: u32 = 440;
pub const GPIO_CH2_PLUG: u32 = 441;

/// Per-chain RESET (output).
pub const GPIO_CH0_RST: u32 = 454;
pub const GPIO_CH1_RST: u32 = 455;
pub const GPIO_CH2_RST: u32 = 456;

/// Misc utility pins.
pub const GPIO_RECOVERY: u32 = 446;
pub const GPIO_IP_GET: u32 = 445;

/// Fan tach inputs.
pub const GPIO_FAN_FRONT_SPEED0: u32 = 447;
pub const GPIO_FAN_FRONT_SPEED1: u32 = 448;
pub const GPIO_FAN_REAR_SPEED0: u32 = 449;
pub const GPIO_FAN_REAR_SPEED1: u32 = 450;

/// Authoritative GPIO map for VNish AML S19j Pro (15 entries total).
///
/// **XXX: INFERRED — W4 handoff `vnish_aml_board.h`, not yet
/// live-verified on a VNish-installed AML bench unit under DCENT_OS.**
pub const GPIO_MAP: &[GpioEntry] = &[
    GpioEntry {
        sysfs_number: GPIO_PWR_EN,
        direction: GpioDir::Out,
        role: GpioRole::PowerEnable,
        initial_value: Some(1), // active HIGH per Wave 5 Q10
        doc: "PWR_EN — hashboard PSU enable (active HIGH, Wave 5 Q10)",
    },
    GpioEntry {
        sysfs_number: GPIO_LED_RED,
        direction: GpioDir::Out,
        role: GpioRole::Led,
        initial_value: Some(0),
        doc: "LED_RED — fault indicator (active HIGH, default OFF)",
    },
    GpioEntry {
        sysfs_number: GPIO_LED_GREEN,
        direction: GpioDir::Out,
        role: GpioRole::Led,
        initial_value: Some(0),
        doc: "LED_GREEN — running indicator (default OFF, ON when mining)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH0_PLUG,
        direction: GpioDir::In,
        role: GpioRole::ChainPlug,
        initial_value: None,
        doc: "CH0_PLUG — chain 0 board detect (active HIGH, pulldown)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH1_PLUG,
        direction: GpioDir::In,
        role: GpioRole::ChainPlug,
        initial_value: None,
        doc: "CH1_PLUG — chain 1 board detect (active HIGH, pulldown)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH2_PLUG,
        direction: GpioDir::In,
        role: GpioRole::ChainPlug,
        initial_value: None,
        doc: "CH2_PLUG — chain 2 board detect (active HIGH, pulldown)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH0_RST,
        direction: GpioDir::Out,
        role: GpioRole::ChainReset,
        initial_value: Some(0), // assert reset at export; de-assert in phase 7
        doc: "CH0_RST — chain 0 reset (active LOW: 0=reset asserted)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH1_RST,
        direction: GpioDir::Out,
        role: GpioRole::ChainReset,
        initial_value: Some(0),
        doc: "CH1_RST — chain 1 reset (active LOW)",
    },
    GpioEntry {
        sysfs_number: GPIO_CH2_RST,
        direction: GpioDir::Out,
        role: GpioRole::ChainReset,
        initial_value: Some(0),
        doc: "CH2_RST — chain 2 reset (active LOW)",
    },
    GpioEntry {
        sysfs_number: GPIO_RECOVERY,
        direction: GpioDir::In,
        role: GpioRole::Recovery,
        initial_value: None,
        doc: "GPIO_RECOVERY — held during boot to enter recovery mode",
    },
    GpioEntry {
        sysfs_number: GPIO_IP_GET,
        direction: GpioDir::In,
        role: GpioRole::IpGet,
        initial_value: None,
        doc: "IP_GET — held to flash IP via LED morse",
    },
    GpioEntry {
        sysfs_number: GPIO_FAN_FRONT_SPEED0,
        direction: GpioDir::In,
        role: GpioRole::FanTach,
        initial_value: None,
        doc: "FAN_FRONT_SPEED0 — front fan tach 0 (falling edge)",
    },
    GpioEntry {
        sysfs_number: GPIO_FAN_FRONT_SPEED1,
        direction: GpioDir::In,
        role: GpioRole::FanTach,
        initial_value: None,
        doc: "FAN_FRONT_SPEED1 — front fan tach 1 (falling edge)",
    },
    GpioEntry {
        sysfs_number: GPIO_FAN_REAR_SPEED0,
        direction: GpioDir::In,
        role: GpioRole::FanTach,
        initial_value: None,
        doc: "FAN_REAR_SPEED0 — rear fan tach 0 (falling edge)",
    },
    GpioEntry {
        sysfs_number: GPIO_FAN_REAR_SPEED1,
        direction: GpioDir::In,
        role: GpioRole::FanTach,
        initial_value: None,
        doc: "FAN_REAR_SPEED1 — rear fan tach 1 (falling edge)",
    },
];

// ---------------------------------------------------------------------------
// PWM map
// ---------------------------------------------------------------------------

/// PWM controller sysfs path.
///
/// **XXX: INFERRED — `vnish_aml_hal_summary.md` cite, not yet live-verified.**
pub const PWM_CHIP_PATH: &str = "/sys/class/pwm/pwmchip0";

/// PWM channel for the rear fan group (per W4 handoff).
pub const PWM_REAR_CHANNEL: &str = "pwm0";

/// PWM channel for the front fan group (per W4 handoff).
pub const PWM_FRONT_CHANNEL: &str = "pwm1";

/// Returns the full sysfs export path for the rear PWM channel,
/// e.g. `/sys/class/pwm/pwmchip0/pwm0`.
pub fn pwm_rear_path() -> String {
    format!("{}/{}", PWM_CHIP_PATH, PWM_REAR_CHANNEL)
}

/// Returns the full sysfs export path for the front PWM channel,
/// e.g. `/sys/class/pwm/pwmchip0/pwm1`.
pub fn pwm_front_path() -> String {
    format!("{}/{}", PWM_CHIP_PATH, PWM_FRONT_CHANNEL)
}

// ---------------------------------------------------------------------------
// I²C + chain count
// ---------------------------------------------------------------------------

/// I²C bus carrying hashboard PIC1704 (`0x20+chain`) and EEPROMs (`0x50..=0x57`,
/// HAL write-denied).
///
/// **XXX: INFERRED — W4 handoff cite, not yet live-verified end-to-end.**
pub const I2C_BUS_PATH: &str = "/dev/i2c-0";

/// VNish AML S19j Pro chassis chain count: 3 (NOT 4).
///
///  ground truth — `vnish_aml_board.h::AML_CHAIN_COUNT = 3`,
/// matching stock am3-aml topology.
pub const CHAIN_COUNT: u8 = 3;

/// Per-chain ASIC reset GPIO de-assert order. Phase 7 walks this in
/// sequence with [`ASIC_RESET_STAGGER`] between each transition.
///
/// **XXX: INFERRED — `bmminer_init_trace_aml.md` cite (cgminer init
/// sequence), not yet live-verified.**
pub const ASIC_RESET_GPIOS_ORDER: [u32; 3] = [GPIO_CH0_RST, GPIO_CH1_RST, GPIO_CH2_RST];

/// Per-chain ASIC reset GPIO de-assert stagger. Mirrors CV1835's 10 ms
/// pattern (`cvitek_cold_boot.rs::ASIC_RESET_STAGGER`); the W4 trace
/// confirms the same 10 ms cadence.
pub const ASIC_RESET_STAGGER: Duration = Duration::from_millis(10);

/// Chain plug-detect GPIO read order (phase 8).
pub const CHAIN_PLUG_GPIOS: [u32; 3] = [GPIO_CH0_PLUG, GPIO_CH1_PLUG, GPIO_CH2_PLUG];

/// I²C address of the per-chain PIC1704 / NoPic surface. cgminer
/// probes `0x20 + chain`. EEPROM range `0x50..=0x57` is intentionally
/// excluded (writes are HAL-denied).
pub const CHAIN_I2C_BASE: u8 = 0x20;

/// I²C address for `chain` (0..[`CHAIN_COUNT`]).
pub fn chain_i2c_addr(chain: u8) -> u8 {
    CHAIN_I2C_BASE + chain
}

// ---------------------------------------------------------------------------
// Phase machine (data only — no orchestration)
// ---------------------------------------------------------------------------

/// VNish AML cold-boot phase identifier.
///
/// Each variant carries the t= timestamp from the W4 trace; `step` is a
/// stable identifier suitable for matching in tests + future
/// orchestrator dispatch. The variants intentionally split the W4
/// 8-phase enum's `MINING_START` into PHASES 8 + 9 because the GPIO
/// semantics differ (chain detect vs. write LEDs + first work).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VnishAmlPhase {
    /// t=1.5s — Export 15 GPIOs, drive PWR_EN HIGH.
    GpioExportAndPsuOn,
    /// t=1.5s — Init `pwmchip0/pwm0` + `pwm1`.
    PwmInit,
    /// t=2.0s — Launch `cgminer` (replaces stock `bmminer`).
    CgminerLaunch,
    /// t=2.5s — cgminer config load + chain bring-up.
    CgminerInit,
    /// t=3.0s — Open `/dev/i2c-0` + per-chain probe at `0x20+chain`.
    I2cChainProbe,
    /// t=3.5s — Drive PWM front + rear to 100 % duty.
    PwmFans100Pct,
    /// t=4.0s — De-assert ASIC reset on chains 0/1/2 with 10 ms stagger.
    AsicResetDeassert,
    /// t=4.5s — Read PLUG GPIOs to discover live chains.
    ChainPlugDiscovery,
    /// t=5.0s — Set LEDs (green ON, red OFF), dispatch first WORK_TX.
    MiningStart,
}

/// One row of the cold-boot phase trace.
#[derive(Debug, Clone, Copy)]
pub struct PhaseTraceRow {
    /// Phase identifier.
    pub phase: VnishAmlPhase,
    /// Approximate timestamp from `bmminer_init_trace_aml.md` (advisory).
    pub t_approx_ms: u32,
    /// One-line description for logs / docs.
    pub doc: &'static str,
}

/// Full 9-row trace of the VNish AML cold-boot sequence.
///
/// **XXX: INFERRED — every row sources from W4 handoff data
/// (`vnish_aml_board.{h,c}` + `vnish_aml_hal_summary.md` +
/// `bmminer_init_trace_aml.md`), not yet live-verified end-to-end on
/// a VNish-installed S19j Pro AML chassis under DCENT_OS.**
pub const PHASE_TRACE: &[PhaseTraceRow] = &[
    PhaseTraceRow {
        phase: VnishAmlPhase::GpioExportAndPsuOn,
        t_approx_ms: 1500,
        doc: "Export 15 GPIOs via sysfs; drive PWR_EN (437) HIGH (Wave 5 Q10)",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::PwmInit,
        t_approx_ms: 1500,
        doc: "Init pwmchip0/pwm0 (rear) + pwm1 (front), 100% duty",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::CgminerLaunch,
        t_approx_ms: 2000,
        doc: "Launch /usr/bin/cgminer (NO sleep 5, NO bmminer)",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::CgminerInit,
        t_approx_ms: 2500,
        doc: "cgminer config load + chain bring-up",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::I2cChainProbe,
        t_approx_ms: 3000,
        doc: "Open /dev/i2c-0; probe per-chain PIC1704/NoPic at 0x20+chain",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::PwmFans100Pct,
        t_approx_ms: 3500,
        doc: "Drive PWM front + rear fans to 100% duty",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::AsicResetDeassert,
        t_approx_ms: 4000,
        doc: "De-assert ASIC reset 454→455→456 with 10 ms stagger",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::ChainPlugDiscovery,
        t_approx_ms: 4500,
        doc: "Read PLUG GPIOs 439/440/441 to discover live chains",
    },
    PhaseTraceRow {
        phase: VnishAmlPhase::MiningStart,
        t_approx_ms: 5000,
        doc: "Set LEDs (453=green ON, 438=red OFF); dispatch first WORK_TX",
    },
];

// ---------------------------------------------------------------------------
// Architectural NOTEs (also data, but documented as `///` constants for
// regression testing — mirrors `cvitek_cold_boot.rs` discipline of
// keeping load-bearing facts as testable string constants).
// ---------------------------------------------------------------------------

/// VNish AML talks I²C primary to the per-chain controllers; no
/// `uart_trans.ko` is loaded. Stock am3-aml platform uses direct UART
/// (ttyS1/2/3) to the BM1362 ASICs; VNish replaces that path with the
/// I²C-relay surface used by their `cgminer` build.
///
/// Pinned as a constant so a future "we should port uart_trans.ko"
/// refactor on the VNish AML side surfaces in tests instead of
/// silently regressing.
pub const ARCHITECTURE_NOTE_NO_UART_TRANS_KO: &str =
    "VNish AML uses I²C-primary surface — NO uart_trans.ko module";

/// VNish AML PSU control is GPIO-only (PWR_EN GPIO 437). There is NO
/// APW12 SMBus interaction in the VNish cold-boot trace — the PSU is
/// already up by the time `S11board` toggles PWR_EN, and there is no
/// per-board voltage programming step on the AML platform.
pub const ARCHITECTURE_NOTE_NO_APW12_PSU_CONTROL: &str =
    "VNish AML PSU control is GPIO-only (PWR_EN 437); NO APW12 SMBus";

// ===========================================================================
//  Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the env-mutating tests in this module. They all touch the
    /// single process-global `ACCEPT_INFERRED_ENV` var; cargo runs tests in
    /// parallel by default, so without this lock concurrent set_var/remove_var
    /// race and the gate assertions flake. `unwrap_or_else(|e| e.into_inner())`
    /// so a panic in one test doesn't poison the lock and cascade-fail siblings.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // --- Env-gate -------------------------------------------------------

    #[test]
    fn cold_boot_refuses_without_env_gate() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Snapshot + clear the env var for the duration of the
        // assertion. Restore after.
        let prev = std::env::var(ACCEPT_INFERRED_ENV).ok();
        std::env::remove_var(ACCEPT_INFERRED_ENV);

        let unlocked = env_gate_is_set();

        // Restore.
        match prev {
            Some(v) => std::env::set_var(ACCEPT_INFERRED_ENV, v),
            None => std::env::remove_var(ACCEPT_INFERRED_ENV),
        }

        assert!(
            !unlocked,
            "with {} unset, env_gate_is_set() must return false",
            ACCEPT_INFERRED_ENV
        );
    }

    #[test]
    fn cold_boot_unlocked_by_canonical_env_name() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(ACCEPT_INFERRED_ENV).ok();
        std::env::set_var(ACCEPT_INFERRED_ENV, "1");

        let unlocked = env_gate_is_set();

        match prev {
            Some(v) => std::env::set_var(ACCEPT_INFERRED_ENV, v),
            None => std::env::remove_var(ACCEPT_INFERRED_ENV),
        }

        assert!(
            unlocked,
            "with {}=1, env_gate_is_set() must return true",
            ACCEPT_INFERRED_ENV
        );
    }

    #[test]
    fn env_var_name_is_canonical() {
        // Pinning the env-name literal so a future rename is caught
        // by tests + must update  / memory rules in lockstep.
        assert_eq!(ACCEPT_INFERRED_ENV, "DCENT_AML_VNISH_ACCEPT_INFERRED");
    }

    // --- GPIO map ------------------------------------------------------

    #[test]
    fn gpio_map_has_15_pins() {
        // 1 PWR_EN + 2 LEDs + 3 PLUGs + 3 RSTs + 1 RECOVERY + 1 IP_GET
        // + 4 fan tachs = 15 pins per W4 handoff vnish_aml_board.h.
        assert_eq!(GPIO_MAP.len(), 15);
    }

    #[test]
    fn gpio_pwr_en_is_437_active_high() {
        assert_eq!(GPIO_PWR_EN, 437);
        let pwr_en_entry = GPIO_MAP
            .iter()
            .find(|e| e.role == GpioRole::PowerEnable)
            .expect("PWR_EN entry must exist");
        assert_eq!(pwr_en_entry.sysfs_number, 437);
        assert_eq!(pwr_en_entry.direction, GpioDir::Out);
        assert_eq!(
            pwr_en_entry.initial_value,
            Some(1),
            "Wave 5 Q10: PWR_EN active HIGH — initial value MUST be 1 to enable PSU"
        );
    }

    #[test]
    fn gpio_led_pins_match_w4_handoff() {
        assert_eq!(GPIO_LED_RED, 438);
        assert_eq!(GPIO_LED_GREEN, 453);
    }

    #[test]
    fn gpio_chain_plug_and_reset_match_w4_handoff() {
        assert_eq!(GPIO_CH0_PLUG, 439);
        assert_eq!(GPIO_CH1_PLUG, 440);
        assert_eq!(GPIO_CH2_PLUG, 441);
        assert_eq!(GPIO_CH0_RST, 454);
        assert_eq!(GPIO_CH1_RST, 455);
        assert_eq!(GPIO_CH2_RST, 456);
    }

    #[test]
    fn gpio_fan_tach_pins_match_w4_handoff() {
        assert_eq!(GPIO_FAN_FRONT_SPEED0, 447);
        assert_eq!(GPIO_FAN_FRONT_SPEED1, 448);
        assert_eq!(GPIO_FAN_REAR_SPEED0, 449);
        assert_eq!(GPIO_FAN_REAR_SPEED1, 450);
    }

    #[test]
    fn gpio_misc_pins_match_w4_handoff() {
        assert_eq!(GPIO_RECOVERY, 446);
        assert_eq!(GPIO_IP_GET, 445);
    }

    #[test]
    fn gpio_role_distribution_matches_w4_handoff() {
        let count_role =
            |role: GpioRole| -> usize { GPIO_MAP.iter().filter(|e| e.role == role).count() };
        assert_eq!(count_role(GpioRole::PowerEnable), 1);
        assert_eq!(count_role(GpioRole::Led), 2);
        assert_eq!(count_role(GpioRole::ChainPlug), 3);
        assert_eq!(count_role(GpioRole::ChainReset), 3);
        assert_eq!(count_role(GpioRole::Recovery), 1);
        assert_eq!(count_role(GpioRole::IpGet), 1);
        assert_eq!(count_role(GpioRole::FanTach), 4);
    }

    // --- PWM + I²C + chain ---------------------------------------------

    #[test]
    fn pwm_chip_path_is_pwmchip0() {
        assert_eq!(PWM_CHIP_PATH, "/sys/class/pwm/pwmchip0");
        assert_eq!(pwm_rear_path(), "/sys/class/pwm/pwmchip0/pwm0");
        assert_eq!(pwm_front_path(), "/sys/class/pwm/pwmchip0/pwm1");
    }

    #[test]
    fn i2c_bus_is_dev_i2c_0() {
        assert_eq!(I2C_BUS_PATH, "/dev/i2c-0");
    }

    #[test]
    fn chain_count_is_3() {
        // W4 handoff: vnish_aml_board.h::AML_CHAIN_COUNT = 3.
        assert_eq!(CHAIN_COUNT, 3);
        assert_eq!(ASIC_RESET_GPIOS_ORDER.len(), 3);
        assert_eq!(CHAIN_PLUG_GPIOS.len(), 3);
    }

    #[test]
    fn chain_i2c_addr_is_0x20_plus_chain() {
        assert_eq!(chain_i2c_addr(0), 0x20);
        assert_eq!(chain_i2c_addr(1), 0x21);
        assert_eq!(chain_i2c_addr(2), 0x22);
    }

    #[test]
    fn asic_reset_stagger_matches_w4_trace() {
        assert_eq!(ASIC_RESET_STAGGER, Duration::from_millis(10));
    }

    // --- Phase machine -------------------------------------------------

    #[test]
    fn phase_machine_has_9_phases() {
        // 8 from W4 handoff enum (collapsed MINING_START → 8+9), so 9
        // distinct phase rows when split for clarity.
        assert_eq!(PHASE_TRACE.len(), 9);
    }

    #[test]
    fn phase_trace_timestamps_monotonic_nondecreasing() {
        // The W4 trace allows two phases to share a t= (1.5 s for
        // GPIO export AND PWM init). Strict monotonicity would be
        // wrong; non-decreasing is the contract.
        let mut prev = 0u32;
        for row in PHASE_TRACE {
            assert!(
                row.t_approx_ms >= prev,
                "phase {:?} at t={} ms violates non-decreasing order (prev={})",
                row.phase,
                row.t_approx_ms,
                prev
            );
            prev = row.t_approx_ms;
        }
    }

    #[test]
    fn phase_trace_starts_at_psu_on_and_ends_at_mining_start() {
        assert_eq!(
            PHASE_TRACE.first().unwrap().phase,
            VnishAmlPhase::GpioExportAndPsuOn
        );
        assert_eq!(
            PHASE_TRACE.last().unwrap().phase,
            VnishAmlPhase::MiningStart
        );
    }

    // --- INFERRED-marker discipline ------------------------------------

    /// Mirrors W12.5 cvitek_cold_boot.rs INFERRED-marker pinning. If a
    /// future refactor strips the "XXX: INFERRED" markers without first
    /// promoting the data to live-verified, this test catches it.
    ///
    /// Counted across the source body excluding the test module itself
    /// (so test-internal string literals don't inflate the count).
    /// 5 expected markers as of W15.D2:
    ///   - 1 file-level doc paragraph ("INFERRED — code-only, hardware-gated")
    ///   - 1 GPIO_MAP doc-block
    ///   - 1 PWM_CHIP_PATH doc-block (single XXX: INFERRED line)
    ///   - 1 I2C_BUS_PATH doc-block (single XXX: INFERRED line)
    ///   - 1 ASIC_RESET_GPIOS_ORDER doc-block
    ///   - 1 PHASE_TRACE doc-block
    /// Bumping this count must be paired with either (a) addition of
    /// new INFERRED data, or (b) promotion of one to live-verified +
    /// removal of the marker.
    #[test]
    fn xxx_inferred_marker_count_pinned() {
        let src = include_str!("vnish_cold_boot.rs");
        let test_mod_start = src.find("#[cfg(test)]").expect("test module start missing");
        let content_only = &src[..test_mod_start];
        let count = content_only.matches("XXX: INFERRED").count();
        assert!(
            count >= 5,
            "expected >= 5 XXX: INFERRED markers in W15.D2 source (file-doc \
             + GPIO_MAP + PWM + I²C + ASIC_RESET + PHASE_TRACE doc-blocks); \
             got {}. If you intentionally promoted markers to live-verified, \
             update this floor + the file-level doc + memory rule.",
            count
        );
    }

    // --- Architectural notes -------------------------------------------

    #[test]
    fn architecture_note_no_uart_trans_ko_pinned() {
        assert!(ARCHITECTURE_NOTE_NO_UART_TRANS_KO.contains("NO uart_trans.ko"));
    }

    // --- Firmware cross-verification (2026-06-02) ----------------------

    /// Pins the S11board-level GPIO/PWM constants to the byte-exact init
    /// script extracted from shipping VNish firmware
    /// (`awesome-s21-aml-nand-v1.2.6`). This is the ground-truth half of
    /// the W4-handoff INFERRED data: if any of these constants drifts away
    /// from what the actual firmware's `S11board` does, this test fails.
    ///
    /// Every assertion below mirrors an exact line in the extracted
    /// `etc/init.d/S11board::config_gpios()` (see [`FIRMWARE_XVERIFY_SOURCE`]).
    #[test]
    fn gpio_pwm_map_matches_extracted_vnish_s21_s11board() {
        // pwr_en 437 out, value 1 — active HIGH, NO active_low written.
        assert_eq!(GPIO_PWR_EN, 437);
        let pwr = GPIO_MAP
            .iter()
            .find(|e| e.role == GpioRole::PowerEnable)
            .unwrap();
        assert_eq!(pwr.direction, GpioDir::Out);
        assert_eq!(pwr.initial_value, Some(1));

        // ch{0,1,2}_plug 439/440/441 in, pull down.
        for (i, &g) in [GPIO_CH0_PLUG, GPIO_CH1_PLUG, GPIO_CH2_PLUG]
            .iter()
            .enumerate()
        {
            assert_eq!(g, 439 + i as u32);
            let e = GPIO_MAP.iter().find(|e| e.sysfs_number == g).unwrap();
            assert_eq!(e.direction, GpioDir::In);
            assert_eq!(e.role, GpioRole::ChainPlug);
        }

        // ch{0,1,2}_rst 454/455/456 out.
        for (i, &g) in [GPIO_CH0_RST, GPIO_CH1_RST, GPIO_CH2_RST]
            .iter()
            .enumerate()
        {
            assert_eq!(g, 454 + i as u32);
            let e = GPIO_MAP.iter().find(|e| e.sysfs_number == g).unwrap();
            assert_eq!(e.direction, GpioDir::Out);
            assert_eq!(e.role, GpioRole::ChainReset);
        }

        // led_green 453 out, led_red 438 out.
        assert_eq!(GPIO_LED_GREEN, 453);
        assert_eq!(GPIO_LED_RED, 438);

        // fan_front 447/448 + fan_rear 449/450, all in/falling-edge.
        for &g in &[
            GPIO_FAN_FRONT_SPEED0,
            GPIO_FAN_FRONT_SPEED1,
            GPIO_FAN_REAR_SPEED0,
            GPIO_FAN_REAR_SPEED1,
        ] {
            let e = GPIO_MAP.iter().find(|e| e.sysfs_number == g).unwrap();
            assert_eq!(e.direction, GpioDir::In);
            assert_eq!(e.role, GpioRole::FanTach);
        }
        assert_eq!(
            [
                GPIO_FAN_FRONT_SPEED0,
                GPIO_FAN_FRONT_SPEED1,
                GPIO_FAN_REAR_SPEED0,
                GPIO_FAN_REAR_SPEED1
            ],
            [447, 448, 449, 450]
        );

        // gpio_recovery 446 in, gpio_ip_get 445 in.
        assert_eq!(GPIO_RECOVERY, 446);
        assert_eq!(GPIO_IP_GET, 445);

        // PWM: pwmchip0/pwm0 (rear) + pwm1 (front).
        assert_eq!(PWM_CHIP_PATH, "/sys/class/pwm/pwmchip0");
        assert_eq!(PWM_REAR_CHANNEL, "pwm0");
        assert_eq!(PWM_FRONT_CHANNEL, "pwm1");

        // Provenance literals pinned (so a doc edit can't silently de-cite).
        assert_eq!(
            FIRMWARE_XVERIFY_SOURCE,
            "awesome-s21-aml-nand-v1.2.6-install.tar.gz"
        );
        assert_eq!(FIRMWARE_XVERIFY_TARBALL_SHA256.len(), 64);
        assert_eq!(FIRMWARE_XVERIFY_S11BOARD_SHA256.len(), 64);
        assert_eq!(FIRMWARE_XVERIFY_DATE, "2026-06-02");
    }

    #[test]
    fn architecture_note_no_apw12_psu_control_pinned() {
        assert!(ARCHITECTURE_NOTE_NO_APW12_PSU_CONTROL.contains("NO APW12 SMBus"));
    }
}
