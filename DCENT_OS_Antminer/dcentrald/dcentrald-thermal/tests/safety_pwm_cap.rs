//! W6.7 — Parameterized fan-PWM-cap CI safety test.
//!
//! and :
//!   - Home mining default cap is PWM 30.
//!   - EVERY safety path (sensor error, tach=0, over-temp, fan failure,
//!     daemon-crash wrapper) MUST cap at the active mode's PWM, never the
//!     legacy 127 value.
//!   - The user has been "burned repeatedly" by past regressions — this
//!     test is the load-bearing CI guard that fails the build if any
//!     platform's safety path leaks above the configured cap.
//!
//! Test surface
//! ============
//! Five platforms × five safety overrides = 25 parameterized cases for
//! Home mode (cap 30). Plus an opt-in matrix that flips `fan_max_pwm = 100`
//! (Advanced) and proves the safety override now correctly clamps at 100,
//! NOT the legacy 127.
//!
//! Why we test at the `safe_fan_pwm` + `ThermalController` boundary
//! ----------------------------------------------------------------
//! `safe_fan_pwm` is the canonical chokepoint helper from
//! `dcentrald-api-types::thermal_model`. It is HAL-free and runs anywhere
//! (Linux/Windows/CI). The `ThermalController` (HAL-free as well) routes
//! every fan write through `safety_capped_pwm`, which delegates to
//! `safe_fan_pwm`. By exercising both layers we get:
//!
//!   * a contract-level proof that the mode-cap math is correct;
//!   * a real-controller proof that no override path bypasses the cap.
//!
//! We deliberately do NOT spin up the per-platform HAL fan drivers
//! (AmlogicFan, ZynqFan, BeagleBoneFan). The HAL is Unix-only; CI runs on
//! mixed hosts. The platform parameterization here models the fan-mode
//! the platform's daemon is expected to load by default. The HAL-side
//! ceiling (e.g. Amlogic clamps to 64 via `enforce_amlogic_tach_safety_policy`)
//! is covered by its own crate's tests; the contract this file pins is
//! "given a profile cap X, no safety trigger leaks PWM > X".
//!
//! S82dcentrald wrapper
//! --------------------
//! The `DaemonCrashFanRecovery` case maps to the init-script post-exit
//! action in
//! `br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S82dcentrald`
//! (Zynq) and its Amlogic/BB siblings. Those scripts hardcode PWM 30 via
//! `devmem`. We assert that the canonical helper returns ≤ 30 in Home
//! mode for every platform, which is the value the wrappers must mirror.

use dcentrald_api_types::thermal_model::{safe_fan_pwm, FanMode, FanSafetyTrigger};
use dcentrald_thermal::controller::{ThermalAction, ThermalController};
use dcentrald_thermal::profiles::ThermalProfile;

// ---------------------------------------------------------------------------
// Test harness model
// ---------------------------------------------------------------------------

/// Platforms whose safety contract this test pins.
///
/// Each variant maps to the dcentrald board-target string used at boot,
/// the fan-mode the platform's daemon defaults to in Home mining, and a
/// human label for failure messages. The list is locked: any new platform
/// must be added here AND in `ALL_PLATFORMS` so CI never silently skips
/// a new control board.
#[derive(Debug, Clone, Copy)]
struct PlatformCase {
    name: &'static str,
    board_target: &'static str,
    /// Mode that the platform's home-default profile maps to.
    home_mode: FanMode,
    /// Profile cap for the home-mode test. Must equal `home_mode.max_pwm()`.
    home_cap_pwm: u8,
}

const PLATFORMS: &[PlatformCase] = &[
    PlatformCase {
        name: "am1-s9 (S9, Zynq XC7Z010, BM1387)",
        board_target: "am1-s9",
        home_mode: FanMode::Home,
        home_cap_pwm: 30,
    },
    PlatformCase {
        name: "am2-s19jpro (S19/S19j Pro, Zynq am2, BM1362)",
        board_target: "am2-s19jpro",
        home_mode: FanMode::Home,
        home_cap_pwm: 30,
    },
    PlatformCase {
        name: "am3-aml-s21 (S21, Amlogic A113D, BM1368)",
        board_target: "am3-aml-s21",
        home_mode: FanMode::Home,
        home_cap_pwm: 30,
    },
    PlatformCase {
        name: "am3-aml-s19kpro (S19k Pro, Amlogic A113D, BM1366)",
        board_target: "am3-aml-s19kpro",
        home_mode: FanMode::Home,
        home_cap_pwm: 30,
    },
    PlatformCase {
        name: "am3-bb (S19j Pro Stock BB, AM335x, BM1362)",
        board_target: "am3-bb",
        home_mode: FanMode::Home,
        home_cap_pwm: 30,
    },
];

/// Every safety override we require to respect the mode cap.
const SAFETY_TRIGGERS: &[FanSafetyTrigger] = &[
    FanSafetyTrigger::SensorError,
    FanSafetyTrigger::FanFailure,
    FanSafetyTrigger::EmergencyShutdown,
    FanSafetyTrigger::DaemonCrash,
    FanSafetyTrigger::StaleTemp,
];

/// Build a `ThermalProfile` whose `fan_max_pwm` matches the requested cap
/// and whose other fields are reasonable for testing. `fan_min_pwm` is set
/// to 5 so the controller always drives PWM > 0 in `NormalMining`, which
/// is required for the tach-zero debounce path to engage. Hot/dangerous
/// thresholds are left at safe defaults so mid-range temperatures land in
/// `NormalMining`.
fn profile_with_cap(cap: u8) -> ThermalProfile {
    let fan_min = 5_u8.min(cap);
    ThermalProfile {
        target_temp_c: 55,
        hot_temp_c: 65,
        dangerous_temp_c: 75,
        fan_min_pwm: fan_min,
        fan_max_pwm: cap,
        ramp_delay_s: 300,
        hysteresis_c: 3,
    }
}

// ---------------------------------------------------------------------------
// Helper-level assertion (the canonical chokepoint)
// ---------------------------------------------------------------------------

/// Pin the `safe_fan_pwm` contract for every (platform, trigger) tuple.
///
/// This is the foundation: if this fails, the whole controller is
/// downstream-broken because `safety_capped_pwm` delegates to this helper.
#[test]
fn home_mode_safety_helper_caps_at_mode_pwm_for_every_platform() {
    for plat in PLATFORMS {
        assert_eq!(
            plat.home_mode.max_pwm(),
            plat.home_cap_pwm,
            "{}: PlatformCase.home_cap_pwm must mirror home_mode.max_pwm()",
            plat.name,
        );
        for trigger in SAFETY_TRIGGERS {
            // Safety trigger ALWAYS forces the mode cap, regardless of
            // the requested value (here we ask for the legacy 127 to
            // simulate a stray "blast" path).
            let pwm = safe_fan_pwm(plat.home_mode, Some(*trigger), 127);
            assert!(
                pwm <= plat.home_cap_pwm,
                "{} / {:?}: safe_fan_pwm leaked PWM {} (cap {})",
                plat.name,
                trigger,
                pwm,
                plat.home_cap_pwm,
            );
            // For Home mode the cap IS exactly the mode max — pin it.
            assert_eq!(
                pwm, plat.home_cap_pwm,
                "{} / {:?}: home-mode safety cap must equal {} (got {})",
                plat.name, trigger, plat.home_cap_pwm, pwm,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Controller-level assertions (one per safety override)
// ---------------------------------------------------------------------------
//
// Each test below feeds the `ThermalController` an input that should
// trigger one of the safety overrides, then asserts the resulting fan
// PWM is at or below the configured cap (30 in Home mode). All five
// cases run for every platform — the inner `for plat in PLATFORMS` loop
// is the parameterization.

/// Helper that extracts the PWM the controller drove fans to from a
/// `ThermalAction`. Returns `None` for actions that don't carry a PWM
/// value (e.g. `RestartInit`).
fn pwm_from_action(action: &ThermalAction) -> Option<u8> {
    match action {
        ThermalAction::SetFanPwm(p) => Some(*p),
        ThermalAction::ThrottleAndFan { pwm, .. } => Some(*pwm),
        // EmergencyShutdown / FanFailure don't carry a PWM in the enum,
        // but the controller has already applied its `current_pwm`
        // internally before returning. The caller compares to
        // `controller.current_pwm()` instead.
        ThermalAction::EmergencyShutdown
        | ThermalAction::FanFailure
        | ThermalAction::RestartInit => None,
    }
}

/// `SensorError` — first tick with no temperatures during the startup
/// grace window forces fans to the safety cap (NOT 127).
#[test]
fn sensor_error_caps_at_home_mode_on_every_platform() {
    for plat in PLATFORMS {
        let mut controller = ThermalController::new(profile_with_cap(plat.home_cap_pwm));
        let action = controller.tick(&[], 1200);

        // During the 60s grace window we get SetFanPwm(cap), not shutdown.
        let pwm = pwm_from_action(&action).unwrap_or(controller.current_pwm());
        assert!(
            pwm <= plat.home_cap_pwm,
            "{}: SensorError leaked PWM {} (cap {}) — action={:?}",
            plat.name,
            pwm,
            plat.home_cap_pwm,
            action,
        );
        assert!(
            controller.current_pwm() <= plat.home_cap_pwm,
            "{}: SensorError current_pwm() = {} > cap {}",
            plat.name,
            controller.current_pwm(),
            plat.home_cap_pwm,
        );
    }
}

/// `TachZero` — fan tachometer reads 0 RPM with PWM > 0 for ≥3 ticks.
/// Should latch a `FanFailure` action with fans capped at the mode max.
#[test]
fn tach_zero_caps_at_home_mode_on_every_platform() {
    for plat in PLATFORMS {
        let mut controller = ThermalController::new(profile_with_cap(plat.home_cap_pwm));

        // Prime the controller with one good sample so it leaves the
        // startup grace path and current_pwm > 0.
        let _ = controller.tick(&[55.0], 1200);

        // Now feed three ticks of tach=0 while temps are normal. The
        // controller's debounce is 3 consecutive zero-RPM reads.
        let mut last_action = controller.tick(&[55.0], 0);
        for _ in 0..2 {
            last_action = controller.tick(&[55.0], 0);
        }

        // After the third zero-RPM tick we expect FanFailure. The
        // controller has already set current_pwm to the safety cap.
        assert!(
            matches!(last_action, ThermalAction::FanFailure),
            "{}: expected FanFailure after tach=0 debounce, got {:?}",
            plat.name,
            last_action,
        );
        assert!(
            controller.current_pwm() <= plat.home_cap_pwm,
            "{}: TachZero / FanFailure leaked PWM {} > cap {}",
            plat.name,
            controller.current_pwm(),
            plat.home_cap_pwm,
        );
        assert_eq!(
            controller.current_pwm(),
            plat.home_cap_pwm,
            "{}: FanFailure must drive PWM to exact cap {} (got {})",
            plat.name,
            plat.home_cap_pwm,
            controller.current_pwm(),
        );
    }
}

/// `OverTemp` — chip temperature crosses `hot_temp_c` (65 °C). Controller
/// enters `HotThrottle` and fans go to the cap. Even though the controller
/// internally requests a "max" PWM here, the safety helper clamps to the
/// profile cap (30 in Home mode), NOT 127.
#[test]
fn over_temp_caps_at_home_mode_on_every_platform() {
    for plat in PLATFORMS {
        let mut controller = ThermalController::new(profile_with_cap(plat.home_cap_pwm));

        // 72 °C > hot_temp_c=65 → HotThrottle + ThrottleAndFan(cap, 10%).
        let action = controller.tick(&[72.0], 1200);

        let pwm = pwm_from_action(&action).expect("HotThrottle must carry a PWM");
        assert!(
            pwm <= plat.home_cap_pwm,
            "{}: OverTemp / HotThrottle leaked PWM {} > cap {}",
            plat.name,
            pwm,
            plat.home_cap_pwm,
        );
        assert_eq!(
            pwm, plat.home_cap_pwm,
            "{}: HotThrottle must drive PWM to exact cap {} (got {})",
            plat.name, plat.home_cap_pwm, pwm,
        );
    }
}

/// `FanFailure` (canonical-helper view) — exercise the helper directly with
/// the `FanFailure` trigger and confirm it returns the mode cap. This is
/// the contract the daemon's heartbeat-fail / dsPIC-fail / stale-fan paths
/// all rely on, even outside the controller.
#[test]
fn fan_failure_helper_caps_at_home_mode_on_every_platform() {
    for plat in PLATFORMS {
        // Pretend the deadly-condition path requested 127 (the legacy
        // bug). The helper must clamp it to the cap.
        let pwm = safe_fan_pwm(plat.home_mode, Some(FanSafetyTrigger::FanFailure), 127);
        assert_eq!(
            pwm, plat.home_cap_pwm,
            "{}: FanFailure leaked PWM {} (cap {}) —  regression",
            plat.name, pwm, plat.home_cap_pwm,
        );
    }
}

/// `DaemonCrashFanRecovery` — the S82dcentrald wrapper hardcodes PWM 30
/// when the daemon exits with hash boards still powered. The CANONICAL
/// helper must agree: in Home mode every platform returns 30 for the
/// `DaemonCrash` trigger, regardless of what the wrapper would've asked
/// for.
#[test]
fn daemon_crash_caps_at_home_mode_on_every_platform() {
    for plat in PLATFORMS {
        // Wrapper emulation: daemon just died, fans must be safe.
        let pwm = safe_fan_pwm(plat.home_mode, Some(FanSafetyTrigger::DaemonCrash), 127);
        assert!(
            pwm <= plat.home_cap_pwm,
            "{}: DaemonCrash wrapper leaked PWM {} > cap {}",
            plat.name,
            pwm,
            plat.home_cap_pwm,
        );
        assert_eq!(
            pwm, 30,
            "{}: S82dcentrald hardcodes PWM 30 — helper must mirror that exactly (got {})",
            plat.name, pwm,
        );
    }
}

// ---------------------------------------------------------------------------
// Advanced / HashrateMax opt-in matrix
// ---------------------------------------------------------------------------
//
// When the operator opts INTO Advanced (cap=100) or HashrateMax (cap=100,
// the fan_ctrl FPGA IP ceiling), the safety overrides must respect THAT cap —
// not the legacy 127 hardcode. The two cases below cover both ends of the
// opt-in. (w24-thermal-safety F-2: HashrateMax was 127, exceeding the IP
// ceiling; corrected to 100. No mode may exceed the IP ceiling of 100.)

/// Advanced opt-in (cap=100). Every safety trigger must clamp at 100, NOT
/// 127. This is the regression that started the entire "fan never blast"
/// rule: the bug shipped 127 even when the user had set 100.
#[test]
fn advanced_mode_safety_helper_caps_at_100_not_legacy_127() {
    for plat in PLATFORMS {
        for trigger in SAFETY_TRIGGERS {
            let pwm = safe_fan_pwm(FanMode::Advanced, Some(*trigger), 200);
            assert_eq!(
                pwm, 100,
                "{} / {:?}: Advanced opt-in must cap at 100 (got {}) — \
                  regression",
                plat.name, trigger, pwm,
            );
        }
    }
}

/// Advanced opt-in via `ThermalController` with `fan_max_pwm = 100`.
/// `OverTemp` should drive PWM to exactly 100 — not 127, not 30.
#[test]
fn advanced_mode_over_temp_caps_at_profile_max_not_127() {
    for plat in PLATFORMS {
        let mut controller = ThermalController::new(profile_with_cap(100));

        let action = controller.tick(&[72.0], 1200);
        let pwm = pwm_from_action(&action).expect("HotThrottle must carry a PWM");

        assert_eq!(
            pwm, 100,
            "{}: Advanced opt-in HotThrottle must cap at 100 (got {})",
            plat.name, pwm,
        );
        // Belt-and-suspenders: never 127 even with explicit Advanced opt-in.
        assert!(
            pwm < 127,
            "{}: PWM 127 must NEVER be reached without explicit HashrateMax opt-in",
            plat.name,
        );
    }
}

/// HashrateMax explicit opt-in (cap=100, the fan_ctrl IP ceiling).
/// w24-thermal-safety F-2: HashrateMax previously returned 127, which exceeds
/// the IP ceiling (the fan_ctrl IP rejects PWM > 100). Even the loudest
/// explicit opt-in is now clamped to 100 — 127 must NEVER be returned by any
/// mode. Confirms all safety triggers return 100 here.
#[test]
fn hashrate_max_opt_in_caps_at_ip_ceiling_100_never_127() {
    for trigger in SAFETY_TRIGGERS {
        let pwm = safe_fan_pwm(FanMode::HashrateMax, Some(*trigger), 50);
        assert_eq!(
            pwm, 100,
            "HashrateMax / {:?}: must cap at the IP ceiling 100 (never 127)",
            trigger,
        );
        assert!(
            pwm <= 100,
            "HashrateMax / {:?}: PWM must never exceed the IP ceiling",
            trigger,
        );
    }
}

// ---------------------------------------------------------------------------
// Total-coverage smoke check
// ---------------------------------------------------------------------------

/// Final no-regression sweep: build the full 25-tuple matrix for Home mode
/// and assert every cell ≤ 30. If any platform or trigger ever escapes the
/// cap, this test prints exactly which cell broke.
#[test]
fn total_coverage_home_mode_cap_30_for_every_platform_and_trigger() {
    let mut failures: Vec<String> = Vec::new();

    for plat in PLATFORMS {
        for trigger in SAFETY_TRIGGERS {
            let pwm = safe_fan_pwm(plat.home_mode, Some(*trigger), 127);
            if pwm > plat.home_cap_pwm {
                failures.push(format!(
                    "[{}] board={} trigger={:?} pwm={} cap={}",
                    plat.name, plat.board_target, trigger, pwm, plat.home_cap_pwm,
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Home-mode PWM cap leaked on {} cell(s):\n  {}\n\
          +  REGRESSION.",
        failures.len(),
        failures.join("\n  "),
    );
}
