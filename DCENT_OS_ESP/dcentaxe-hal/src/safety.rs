//! Alloc-free, dependency-free fail-closed safety primitives (XPSAFE-1).
//!
//! This module is the SINGLE SOURCE OF TRUTH for buck-enable polarity and the
//! "max cooling" fan-duty bytes used on the panic path. Like `board`, it is
//! deliberately NOT gated to the ESP-IDF target: every function is a `const fn`
//! over plain integers/bools with no `esp-idf-hal` / `log` / heap dependency, so
//! it host-compiles and its truth tables are unit-tested on the host
//! (`cargo test -p dcentaxe-core` / `-p dcentaxe-hal`).
//!
//! The espidf-only fail-closed panic hook (`dcentaxe::install_fail_closed_panic_hook`)
//! and `gpio::GpioController::enable_buck` BOTH route through `buck_level_high`
//! here, so a driver and the hook can never disagree about which GPIO level cuts
//! the rail. The panic hook reads `buck_off_level` to drive the buck-enable pin
//! to its OFF state with a single lock-free `gpio_set_level`.

/// Logical "drive the buck-enable pin HIGH?" decision.
///
/// Mirrors `gpio::GpioController::enable_buck` EXACTLY: active-low boards
/// (DS4432U Max/Ultra) invert (`HIGH = off`); active-high boards drive the pin
/// to match `on`.
pub const fn buck_level_high(active_low: bool, on: bool) -> bool {
    if active_low {
        !on
    } else {
        on
    }
}

/// Raw `gpio_set_level` argument that turns the buck converter OFF.
///
/// Returns `1` (drive HIGH) for active-low boards and `0` (drive LOW) for
/// active-high boards. This is the load-bearing value the fail-closed panic hook
/// writes to cut the ASIC rail before the runtime aborts.
pub const fn buck_off_level(active_low: bool) -> u32 {
    if buck_level_high(active_low, false) {
        1
    } else {
        0
    }
}

/// Map a 0-100% fan request to an 8-bit PWM duty byte for the EMC2302 / EMC2103
/// direct-PWM controllers (FAN_SETTING is a full `0x00..=0xFF` duty).
///
/// Preserves the exact `255 * pct / 100` math used in `emc2302::set_fan_speed`
/// (emc2302.rs) and the EMC2103 boot block / `emc2103::set_fan_speed`. Inputs
/// above 100 are clamped to 100 (never over-driven, never under-driven).
pub const fn pwm_byte_for_pct(pct: u8) -> u8 {
    let clamped = if pct > 100 { 100u16 } else { pct as u16 };
    (255u16 * clamped / 100) as u8
}

/// Map a 0-100% fan request to the EMC2101 FAN_SETTING 6-bit duty (`0..=63`,
/// where `63` == 100%) — distinct from the 8-bit EMC230x `pwm_byte_for_pct`
/// scale used by EMC2302/EMC2103.
///
/// Preserves the exact `63 * pct / 100` math from `temp::Emc2101::set_fan_speed`
/// (temp.rs) so routing that driver through this helper is byte-faithful.
/// Inputs above 100 clamp to 100 (full scale == `emc2101_panic_duty()` == 63);
/// never over-driven, never under-driven.
pub const fn emc2101_duty_for_pct(pct: u8) -> u8 {
    let clamped = if pct > 100 { 100u16 } else { pct as u16 };
    (63u16 * clamped / 100) as u8
}

/// Full-scale ("max cooling") PWM byte for an 8-bit-duty EMC230x fan controller
/// (EMC2302 regs `0x30`/`0x40`, EMC2103 reg `0x40`).
///
/// The panic path ALWAYS commands maximum cooling — this never returns a reduced
/// value (it equals `pwm_byte_for_pct(100)`).
pub const fn fan_safe_panic_duty() -> u8 {
    255
}

/// Full-scale ("max cooling") duty byte for the EMC2101 FAN_SETTING register
/// (`0x4C`), whose direct-mode duty is 6-bit (`0..=63`, 63 == 100%) — distinct
/// from the 8-bit EMC230x duty.
///
/// Mirrors the 63-scale in `temp::Emc2101::set_fan_speed` (temp.rs) so the panic
/// hook commands true full cooling, not a value the chip would re-interpret.
pub const fn emc2101_panic_duty() -> u8 {
    63
}

// ─── XPAUTO-2: chip-health-aware autotuner backoff (cross-pollination) ────────
//
// Faithful port of DCENT_OS's `DpsWalker` HealthBackoff
// (`DCENT_OS_Antminer/dcentrald/dcentrald/src/autotune/dps_walker.rs:670-686`),
// where the chip-health guard "takes precedence over everything" — it runs
// ahead of all optimization and, on a rising HW-error rate, retreats the
// frequency to the last-known-good point and clears the fit window.
//
// These two functions are the SINGLE place the retreat condition is computed,
// so the espidf-only `autotuner` wiring (which cannot host-compile) can never
// disagree with the host-tested decision. They are pure arithmetic — no I/O,
// no alloc — so the truth table is unit-tested on the host.
//
// They are intentionally NOT `const fn`: `f64::is_finite` is not const-stable on
// the esp Rust toolchain. Every other property matches this module's existing
// const-over-plain-integers style.
//
// SAFETY / DEFAULT-PRESERVING CONTRACT: a HW-error backoff only ever signals a
// retreat toward an already-proven last-known-good (freq/voltage that previously
// passed the autotuner's health gate). The caller MUST drive freq/voltage DOWN
// to that proven point — never up. This can never raise frequency, raise
// voltage, lower a fan/thermal limit, or change a magic constant; it only makes
// the converged point MORE conservative.

/// XPAUTO-2: should the autotuner retreat to last-known-good this tick?
///
/// Returns `true` only when the current-tick HW-error fraction exceeds
/// `ceiling` AND that has now held for `required_streak` CONSECUTIVE ticks
/// (counting this tick). `current_streak` is the count of consecutive prior
/// over-ceiling ticks BEFORE this one.
///
/// The N-consecutive-tick debounce (`required_streak`, default 3 at the call
/// site) is load-bearing and deliberately diverges from DpsWalker's instant
/// single-tick trip: the axe samples its error rate as a noisy
/// `rejected_shares / nonces_found` share-delta per tick
/// (`autotuner.rs::sample_delta_error_rate`), which is far noisier than
/// DpsWalker's per-chip HW-error fraction. An instant trip would false-retreat
/// on a transient share-reject burst. Do NOT drop the debounce to 1.
///
/// NaN-safe: a non-finite rate is treated as NOT-over-ceiling, so the autotuner
/// never acts on garbage telemetry. This mirrors the NaN-as-benign precedent in
/// this file's sibling logic (`find_best_point`'s `fcmp`).
pub fn hw_error_backoff_should_retreat(
    delta_error_rate: f64,
    ceiling: f64,
    current_streak: u8,
    required_streak: u8,
) -> bool {
    if !delta_error_rate.is_finite() {
        return false;
    }
    delta_error_rate > ceiling && current_streak.saturating_add(1) >= required_streak
}

/// XPAUTO-2 companion: advance the consecutive-over-ceiling streak counter.
///
/// Increments (saturating) on an over-ceiling finite sample; resets to `0` on
/// any healthy OR non-finite sample — mirroring DpsWalker's `fit_window.clear()`
/// intent (a healthy tick invalidates the fitted retreat pressure). Pure.
pub fn hw_error_streak_next(delta_error_rate: f64, ceiling: f64, streak: u8) -> u8 {
    if delta_error_rate.is_finite() && delta_error_rate > ceiling {
        streak.saturating_add(1)
    } else {
        0
    }
}

// ─── INA260 over-current/over-power backstop (DS4432U boards) ─────────────────
//
// DS4432U boards (Max/Ultra/Supra) have NO PMBus STATUS_WORD
// (R-10 note: DCENT_axe BM1397 is NOT in this class — it carries a TPS546
// PMBus VRM and no INA260, so it uses the TPS546 fault path instead.)
// over-current detection and cannot disable the rail over I2C, so the INA260
// input-rail monitor in the `main.rs` supervisor is their ONLY software OC/OP
// protection — the hard-kill these boards depend on. These three pure fns are the
// SINGLE source of truth for that decision so the host can pin the truth table
// (`main.rs` is not host-buildable). Behavior is byte-identical to the prior
// inline `power_over || current_over` / strike / `>= debounce` logic.
//
// SAFETY: this can only ever ADD a fail-closed hard-kill (cut hash power FIRST).
// A NaN/absent telemetry field is treated as NOT over (benign) — a blanked
// reading must never be the thing that trips OR the thing that suppresses a real
// trip. Pure (no I2C / esp-idf / alloc); host-tested below.

/// Is the measured INA260 input power OR current over the board's rated envelope
/// (× `margin`)? Mirrors the `main.rs` supervisor EXACTLY: `power_over ||
/// current_over`, where each term requires a finite reading (`power_field_available`,
/// NaN-rejection), a positive rated cap (`max > 0`), and a STRICT `>` over
/// `max * margin`. `current_a` is amps (main.rs passes `current_ma / 1000.0`). A
/// non-finite field, a non-positive cap, or a value at/below the margin is NOT over.
pub fn ina260_oc_over_envelope(
    power_w: f32,
    current_a: f32,
    max_power_w: f32,
    max_current_a: f32,
    margin: f32,
) -> bool {
    let power_over =
        power_field_available(power_w) && max_power_w > 0.0 && power_w > max_power_w * margin;
    let current_over = power_field_available(current_a)
        && max_current_a > 0.0
        && current_a > max_current_a * margin;
    power_over || current_over
}

/// Advance the INA260 OC strike counter: saturating `+1` while over the envelope,
/// reset to `0` on a healthy (not-over) tick. Mirrors main.rs's
/// `if over { strikes = strikes.saturating_add(1) } else { strikes = 0 }`.
pub fn ina260_oc_strike_next(over: bool, strikes: u8) -> u8 {
    if over {
        strikes.saturating_add(1)
    } else {
        0
    }
}

/// Should the supervisor cut ASIC power this tick? `true` once the over-envelope
/// condition has held for `debounce` CONSECUTIVE ticks (mirrors main.rs's
/// `ina_oc_strikes >= INA260_OC_DEBOUNCE_TICKS`). The debounce + the
/// `INA260_OC_MARGIN` above the rated envelope keep a transient load-step /
/// measurement spike from nuisance-tripping the hard-kill.
pub fn ina260_oc_should_cut(strikes: u8, debounce: u8) -> bool {
    strikes >= debounce
}

// ─── HALPWR-2: per-field power telemetry fallback (independent fallibility) ───
//
// `PowerManager::get_telemetry` (power.rs, espidf-only) previously chained five
// `?`-propagating sub-reads, so ONE transient I2C NACK aborted the whole snapshot
// and main.rs dropped every power field for that tick (including the valid
// voltage/current it DID read). These helpers are the pure decision used by the
// new per-field path: a failed sub-read becomes `f32::NAN` ("field unavailable")
// while the fields that succeeded survive — mirroring `snapshot_status()`'s
// existing per-register `.unwrap_or` style. Consumers MUST treat NaN as
// "unavailable" (skip it / hold last-good) rather than a real 0 or a real reading.
//
// Pure (no I2C / esp-idf / alloc): host-tested below. Not `const fn` because
// `f32::is_nan`/`is_finite` are not const-stable on the esp toolchain.

/// Map a per-field sub-read result to a float, substituting `f32::NAN` for a
/// failed read. The boolean is the read's success: `true` keeps `value`,
/// `false` returns NaN. This is the single source of truth for "a blanked power
/// field is NaN, never a misleading 0.0".
pub fn power_field_or_nan(read_ok: bool, value: f32) -> f32 {
    if read_ok {
        value
    } else {
        f32::NAN
    }
}

/// Is a power-telemetry field a usable reading? `false` for NaN (a blanked
/// sub-read) so consumers (autotuner WattageDescent window, telemetry logs)
/// can skip it instead of pushing NaN into a rolling average and poisoning it.
pub fn power_field_available(value: f32) -> bool {
    value.is_finite()
}

// ─── HALPWR-3: driver-level voltage ceiling (defense-in-depth) ────────────────
//
// `Ds4432u::set_voltage_mv` and `Tps546::set_voltage_mv` (power.rs) rely ENTIRELY
// on the upstream `PowerManager` board clamp (against config min/max_voltage_mv)
// to keep the rail safe. The DS4432U driver's own internal cap was 2000 mV — far
// above any BM1397/1366/1368/1370-safe core voltage (~1.0–1.55 V). No direct
// driver caller exists today, but a future direct call (or an absurd board
// `max_voltage_mv`) would let 2.0 V onto the feedback network, and the DS4432U
// boards cannot disable the rail over I2C, so over-volt recovery is hard.
//
// This is the SINGLE driver-side ceiling. It NEVER raises the existing board
// clamp (the board clamp still runs first and is tighter on every real board);
// it only refuses requests above an absolute chip-safe ceiling regardless of
// caller, so the driver is self-protecting. Pure: host-tested below.

/// Absolute driver-side core-voltage ceiling in mV — last-line defense for the
/// raw regulator drivers. 1600 mV is above every shipped BitAxe board's
/// configured `max_voltage_mv` (Max=1550, Ultra=1400, GT/Hex per-domain ≤ ~1300)
/// yet far below the old 2000 mV DS4432U cap and any chip-damaging voltage. Do
/// NOT lower below the highest board `max_voltage_mv` or legitimate setpoints on
/// that board would be refused.
pub const DRIVER_VOLTAGE_CEILING_MV: u16 = 1600;

/// Returns `true` if a raw per-ASIC core-voltage request (mV) is at or below the
/// absolute driver ceiling AND non-zero-meaningful. The driver should reject
/// (fail-closed, like `PowerManager::set_voltage`) when this is `false`. This is
/// independent of — and ANDed with — the upstream board clamp; it can only make
/// the driver MORE conservative, never less.
pub const fn voltage_within_driver_ceiling(voltage_mv: u16, ceiling_mv: u16) -> bool {
    voltage_mv <= ceiling_mv
}

// ─── HALPWR-6: HAL-level non-zero fan floor while mining ──────────────────────
//
// `Emc2302::set_fan_speed`/`_float` (emc2302.rs) wrote raw PWM with NO lower
// floor, so any caller (autotuner, MCP `set_fan_speed`, space-heater logic)
// commanding 0 % stopped the fans on a powered mining board with no HAL or
// hardware interlock — the only net was in main.rs.  coding standards
// list "fan floor" as a required HAL safety limit. These helpers are the pure
// floor decision the emc2302 driver now applies; a true-zero command is only
// honored under the explicit lab bypass.
//
// The 20 % floor matches main.rs's existing mining floor (`pct.max(20)`). Pure:
// host-tested below.

/// Minimum fan duty (%) the HAL will command on a mining-capable board unless the
/// operator has set `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1`. Matches the main-loop
/// mining floor (`main.rs` `pct.max(20)`) so the HAL and the supervisor agree.
pub const FAN_FLOOR_PCT: u8 = 20;

/// Whether the operator compiled in the explicit unsafe lab safety bypass
/// (`DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` at build time).
///
/// COMPILE-TIME ONLY (XPSAFE-4): there is no runtime `std::env::var` arm. On the
/// ESP32 firmware target `std::env::var` has no backing process environment, so a
/// runtime arm "falsely implied a runtime toggle" and — worse — could make two
/// safety layers DISAGREE about whether the bypass is active in any std-env
/// build. This mirrors `main.rs::unsafe_lab_safety_bypass_enabled`'s
/// compile-time-only form so every safety layer (HAL fan floor, supervisor)
/// reads the SAME gate. Single source of truth for the bypass decision in this
/// crate.
pub fn lab_safety_bypass_enabled() -> bool {
    option_env!("DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS") == Some("1")
}

// ─── HALPWR-7: enable-time VOUT readback verification ─────────────────────────
//
// `Tps546::enable` (power.rs) reads READ_VOUT after OPERATION_ON but only LOGGED
// it — it never compared it to the commanded setpoint, so a rail that enables but
// under-delivers (failed phase, marginal supply, ASIC soft-short) returned Ok and
// the boot path silently dispatched work onto a sagging rail (HW errors / zero
// shares with no power-side signal). This pure predicate is the same cmd-vs-read
// tolerance the phantom-OV path (power.rs) already uses, generalized for the
// stacked GT (the read and cmd are both stack totals, so the tolerance scales by
// `voltage_domains`). Under-volt is benign for the silicon, so the caller emits a
// SOFT warning — it does not fail `enable()`. Pure: host-tested below.

/// Default per-domain tolerance (mV) for the enable-time VOUT readback check.
/// 150 mV per domain matches the finding's suggested tolerance and is wider than
/// the phantom-OV path's 100 mV (which runs against a settled rail, not the
/// just-enabled ramp).
pub const VOUT_SETTLE_TOL_PER_DOMAIN_MV: f32 = 150.0;

/// Returns `true` if the just-enabled rail reached its commanded setpoint within
/// tolerance. `read_v` / `cmd_v` are the STACK-TOTAL volts (READ_VOUT /
/// VOUT_COMMAND); the absolute tolerance is `tol_per_domain_mv * voltage_domains`
/// so a 3-domain Hex stack gets 3× the slack of a single-ASIC board.
///
/// NaN/garbage-safe and zero-safe: if either reading is non-finite or `cmd_v`
/// is ~0 (rail commanded off / unread), returns `true` (do NOT warn — there is no
/// meaningful setpoint to miss). This only ever produces a soft diagnostic; it
/// never gates the rail.
pub fn vout_reached_setpoint(
    read_v: f32,
    cmd_v: f32,
    voltage_domains: u16,
    tol_per_domain_mv: f32,
) -> bool {
    if !read_v.is_finite() || !cmd_v.is_finite() || cmd_v <= 0.0 {
        return true;
    }
    let domains = if voltage_domains == 0 {
        1.0
    } else {
        voltage_domains as f32
    };
    let tol_v = (tol_per_domain_mv * domains) / 1000.0;
    (read_v - cmd_v).abs() <= tol_v
}

/// Apply the HAL fan floor to a requested duty percent.
///
/// - `lab_bypass == false` (normal/field): a request below `floor_pct` is RAISED
///   to `floor_pct`; a true-zero request is also raised (fans never fully stop on
///   a powered board). Requests at/above the floor pass through unchanged
///   (still clamped to 100 by the caller's `pwm_byte_for_pct`).
/// - `lab_bypass == true` (bench): the request passes through verbatim, including
///   0 % — the only way to fully stop the fans, gated behind the explicit unsafe
///   lab bypass.
///
/// This NEVER reduces a fan command; it only ever raises a too-low one. It cannot
/// lower the panic/max-cooling duty (that path uses `fan_safe_panic_duty`).
pub const fn fan_duty_with_floor(requested_pct: u8, floor_pct: u8, lab_bypass: bool) -> u8 {
    if lab_bypass {
        return requested_pct;
    }
    if requested_pct < floor_pct {
        floor_pct
    } else {
        requested_pct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buck_polarity_truth_table() {
        // active-low board (DS4432U Max/Ultra): HIGH = off, LOW = on.
        assert!(
            buck_level_high(true, false),
            "active-low OFF must drive HIGH"
        );
        assert!(!buck_level_high(true, true), "active-low ON must drive LOW");
        // active-high board: HIGH = on, LOW = off.
        assert!(
            !buck_level_high(false, false),
            "active-high OFF must drive LOW"
        );
        assert!(
            buck_level_high(false, true),
            "active-high ON must drive HIGH"
        );
    }

    #[test]
    fn buck_off_level_cuts_rail() {
        // The panic hook writes buck_off_level(active_low) to gpio_set_level.
        // active-low boards must be driven HIGH (1) to cut; active-high LOW (0).
        assert_eq!(buck_off_level(true), 1, "active-low: OFF == drive HIGH(1)");
        assert_eq!(buck_off_level(false), 0, "active-high: OFF == drive LOW(0)");
    }

    #[test]
    fn buck_off_level_agrees_with_level_high() {
        for &active_low in &[false, true] {
            let expected = if buck_level_high(active_low, false) {
                1
            } else {
                0
            };
            assert_eq!(buck_off_level(active_low), expected);
        }
    }

    #[test]
    fn pwm_byte_duty_math_and_clamp() {
        assert_eq!(pwm_byte_for_pct(0), 0);
        assert_eq!(pwm_byte_for_pct(100), 255);
        // 255*50/100 = 127.5 truncates to 127.
        assert_eq!(pwm_byte_for_pct(50), 127);
        // 255*20/100 = 51.
        assert_eq!(pwm_byte_for_pct(20), 51);
        // Inputs above 100 clamp to full scale (never over-driven).
        assert_eq!(pwm_byte_for_pct(101), 255);
        assert_eq!(pwm_byte_for_pct(200), 255);
        assert_eq!(pwm_byte_for_pct(255), 255);
    }

    #[test]
    fn panic_fan_duty_is_full_scale_never_reduced() {
        // EMC230x: panic duty == 100% full scale.
        assert_eq!(fan_safe_panic_duty(), 255);
        assert_eq!(fan_safe_panic_duty(), pwm_byte_for_pct(100));
        // EMC2101: 6-bit scale, 63 == 100% full scale.
        assert_eq!(emc2101_panic_duty(), 63);
        // Safety invariant: the panic duty must exceed any mining-floor (20%)
        // command for both register scales — the hook never reduces a fan limit.
        assert!(fan_safe_panic_duty() > pwm_byte_for_pct(20));
        assert!(emc2101_panic_duty() > (63u16 * 20 / 100) as u8);
    }

    // ─── XPAUTO-2: chip-health backoff truth table ───────────────────────────
    // MAX_ERROR_RATE in the axe autotuner is 0.02 (2 %). The default debounce at
    // the call site is 3 consecutive over-ceiling ticks.
    const CEIL: f64 = 0.02;
    const REQ: u8 = 3;

    #[test]
    fn below_ceiling_never_retreats() {
        // A healthy 1 % error rate must never trip, at any streak.
        for streak in 0u8..=10 {
            assert!(
                !hw_error_backoff_should_retreat(0.01, CEIL, streak, REQ),
                "below-ceiling rate must not retreat (streak {streak})"
            );
        }
        // Exactly at the ceiling is NOT over it (strict `>`).
        assert!(!hw_error_backoff_should_retreat(CEIL, CEIL, 5, REQ));
    }

    #[test]
    fn single_over_ceiling_tick_is_debounced() {
        // One bad tick with no prior streak must NOT retreat (debounce holds).
        assert!(!hw_error_backoff_should_retreat(0.10, CEIL, 0, REQ));
        // Two consecutive (this is the 2nd) still below the 3-tick requirement.
        assert!(!hw_error_backoff_should_retreat(0.10, CEIL, 1, REQ));
    }

    #[test]
    fn third_consecutive_over_ceiling_retreats() {
        // current_streak=2 prior over-ceiling ticks + this over-ceiling tick = 3.
        assert!(hw_error_backoff_should_retreat(0.10, CEIL, 2, REQ));
        // And it stays tripped for any deeper streak.
        assert!(hw_error_backoff_should_retreat(0.10, CEIL, 9, REQ));
    }

    #[test]
    fn streak_advances_then_trips_via_companion() {
        // Simulate the call-site loop: advance the streak, then test retreat with
        // the PRE-increment streak (caller passes streak.saturating_sub(1) so the
        // companion and the predicate agree on "this tick counts once").
        let mut streak = 0u8;
        let mut tripped_on = None;
        for tick in 1u8..=5 {
            streak = hw_error_streak_next(0.10, CEIL, streak);
            if hw_error_backoff_should_retreat(0.10, CEIL, streak.saturating_sub(1), REQ) {
                tripped_on = Some(tick);
                break;
            }
        }
        assert_eq!(tripped_on, Some(3), "must trip on exactly the 3rd bad tick");
    }

    #[test]
    fn healthy_tick_resets_built_up_streak() {
        // Build a streak, then a healthy tick zeroes it (DpsWalker fit-clear intent).
        let mut streak = hw_error_streak_next(0.10, CEIL, 0); // 1
        streak = hw_error_streak_next(0.10, CEIL, streak); // 2
        assert_eq!(streak, 2);
        streak = hw_error_streak_next(0.005, CEIL, streak); // healthy → 0
        assert_eq!(streak, 0, "a healthy tick must reset the streak");
        // And the predicate must not retreat right after a reset.
        assert!(!hw_error_backoff_should_retreat(0.10, CEIL, 0, REQ));
    }

    #[test]
    fn nan_inf_rate_is_benign() {
        // Garbage telemetry must NEVER trip a retreat and must reset the streak.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                !hw_error_backoff_should_retreat(bad, CEIL, 9, REQ),
                "non-finite rate must not retreat"
            );
            assert_eq!(
                hw_error_streak_next(bad, CEIL, 7),
                0,
                "non-finite rate must reset the streak"
            );
        }
    }

    // ─── HALPWR-2: per-field power telemetry fallback ────────────────────────

    #[test]
    fn power_field_ok_keeps_value_failed_is_nan() {
        // A successful sub-read keeps its value verbatim (including a real 0.0).
        assert_eq!(power_field_or_nan(true, 1234.5), 1234.5);
        assert_eq!(power_field_or_nan(true, 0.0), 0.0);
        // A failed sub-read becomes NaN — NOT a misleading 0.0.
        assert!(power_field_or_nan(false, 1234.5).is_nan());
        assert!(power_field_or_nan(false, 0.0).is_nan());
    }

    #[test]
    fn power_field_availability_distinguishes_blanked_from_real() {
        // A real reading (even 0.0 or negative) is "available".
        assert!(power_field_available(0.0));
        assert!(power_field_available(-1.0));
        assert!(power_field_available(5120.0));
        // A blanked field (NaN) and non-finite garbage are "unavailable" so the
        // WattageDescent window / telemetry skip it instead of poisoning a mean.
        assert!(!power_field_available(f32::NAN));
        assert!(!power_field_available(f32::INFINITY));
        assert!(!power_field_available(f32::NEG_INFINITY));
    }

    #[test]
    fn one_blanked_field_does_not_blank_the_others() {
        // Simulate get_telemetry's per-field path: current sub-read NACKs but the
        // other four succeed. The valid fields MUST survive (the whole-snapshot
        // drop is exactly the HALPWR-2 bug).
        let voltage = power_field_or_nan(true, 1200.0);
        let current = power_field_or_nan(false, 0.0); // NACK this tick
        let power = power_field_or_nan(true, 18.5);
        let vin = power_field_or_nan(true, 5000.0);
        let vreg = power_field_or_nan(true, 52.0);
        assert!(power_field_available(voltage) && voltage == 1200.0);
        assert!(!power_field_available(current)); // only this one is unavailable
        assert!(power_field_available(power) && power == 18.5);
        assert!(power_field_available(vin) && vin == 5000.0);
        assert!(power_field_available(vreg) && vreg == 52.0);
    }

    // ─── HALPWR-3: driver-level voltage ceiling ──────────────────────────────

    #[test]
    fn driver_ceiling_is_chip_safe_not_2000mv() {
        // The whole point: the driver ceiling is far below the old 2000 mV cap.
        assert!(DRIVER_VOLTAGE_CEILING_MV < 2000);
        // ...yet above every shipped board's configured max_voltage_mv so a
        // legitimate setpoint on the highest board is never refused.
        // Max=1550, Ultra=1400 (board.rs ceilings); 1600 covers them.
        assert!(DRIVER_VOLTAGE_CEILING_MV >= 1550);
    }

    #[test]
    fn voltage_within_driver_ceiling_truth_table() {
        let c = DRIVER_VOLTAGE_CEILING_MV;
        // Real mining setpoints pass.
        assert!(voltage_within_driver_ceiling(1200, c));
        assert!(voltage_within_driver_ceiling(1550, c));
        // Exactly at the ceiling is allowed (inclusive).
        assert!(voltage_within_driver_ceiling(c, c));
        // One mV over is refused — the old 2.0 V request now fails closed.
        assert!(!voltage_within_driver_ceiling(c + 1, c));
        assert!(!voltage_within_driver_ceiling(2000, c));
        assert!(!voltage_within_driver_ceiling(u16::MAX, c));
    }

    // ─── HALPWR-6: HAL fan floor ─────────────────────────────────────────────

    #[test]
    fn fan_floor_raises_too_low_requests_when_not_bypassed() {
        let floor = FAN_FLOOR_PCT; // 20
                                   // A true-zero command on a powered board is raised to the floor.
        assert_eq!(fan_duty_with_floor(0, floor, false), floor);
        // Below-floor requests are raised to the floor.
        assert_eq!(fan_duty_with_floor(5, floor, false), floor);
        assert_eq!(fan_duty_with_floor(19, floor, false), floor);
        // At/above the floor pass through unchanged.
        assert_eq!(fan_duty_with_floor(20, floor, false), 20);
        assert_eq!(fan_duty_with_floor(60, floor, false), 60);
        assert_eq!(fan_duty_with_floor(100, floor, false), 100);
    }

    #[test]
    fn fan_floor_lab_bypass_allows_true_zero() {
        let floor = FAN_FLOOR_PCT;
        // Only the explicit unsafe lab bypass lets a 0 % (full stop) through.
        assert_eq!(fan_duty_with_floor(0, floor, true), 0);
        assert_eq!(fan_duty_with_floor(5, floor, true), 5);
        // Bypass passes everything verbatim — it never modifies the request.
        assert_eq!(fan_duty_with_floor(100, floor, true), 100);
    }

    #[test]
    fn fan_floor_never_reduces_a_command() {
        // Invariant: the floor only ever raises a too-low command, never lowers
        // one — so it can never fight the thermal supervisor's high-duty command.
        for req in 0u8..=100 {
            let out = fan_duty_with_floor(req, FAN_FLOOR_PCT, false);
            assert!(out >= req, "floor must never reduce req {req} -> {out}");
            assert!(out >= FAN_FLOOR_PCT, "non-bypass output below floor");
        }
    }

    #[test]
    fn fan_floor_byte_stays_nonzero_through_pwm_conversion() {
        // The floor percent must convert to a non-zero PWM byte (fans actually
        // spin). 20 % -> 255*20/100 = 51, well above 0.
        let floored = fan_duty_with_floor(0, FAN_FLOOR_PCT, false);
        assert!(pwm_byte_for_pct(floored) > 0);
        assert_eq!(pwm_byte_for_pct(floored), 51);
    }

    #[test]
    fn emc2101_duty_math_and_clamp() {
        // EMC2101 FAN_SETTING is a 6-bit duty (0..=63). 63 == full scale.
        assert_eq!(emc2101_duty_for_pct(0), 0);
        assert_eq!(emc2101_duty_for_pct(100), 63);
        assert_eq!(emc2101_duty_for_pct(100), emc2101_panic_duty());
        // 63*20/100 = 12.6 -> 12 (the floored-command duty).
        assert_eq!(emc2101_duty_for_pct(20), 12);
        // 63*50/100 = 31.5 -> 31.
        assert_eq!(emc2101_duty_for_pct(50), 31);
        // Inputs above 100 clamp to full scale (never over-driven).
        assert_eq!(emc2101_duty_for_pct(101), 63);
        assert_eq!(emc2101_duty_for_pct(255), 63);
    }

    #[test]
    fn emc2101_set_fan_speed_floor_keeps_gamma_fan_spinning() {
        // Mirrors temp::Emc2101::set_fan_speed (the Gamma / BM1370 public path):
        //   duty = emc2101_duty_for_pct(fan_duty_with_floor(pct, FLOOR, bypass)).
        // Below-floor and true-zero commands must yield a NON-ZERO 6-bit duty when
        // the lab bypass is OFF — the fan never stops on the powered board.
        for pct in [0u8, 5] {
            let duty = emc2101_duty_for_pct(fan_duty_with_floor(pct, FAN_FLOOR_PCT, false));
            assert!(
                duty >= emc2101_duty_for_pct(FAN_FLOOR_PCT) && duty > 0,
                "EMC2101 pct {pct} (no bypass) must floor to a non-zero duty, got {duty}"
            );
            assert_eq!(
                duty, 12,
                "EMC2101 floored 6-bit duty must be 63*20/100 = 12"
            );
        }
        // Only the explicit lab bypass lets a true-zero (full-stop) duty through.
        assert_eq!(
            emc2101_duty_for_pct(fan_duty_with_floor(0, FAN_FLOOR_PCT, true)),
            0,
            "EMC2101 0% under lab bypass is a true-zero full stop"
        );
    }

    #[test]
    fn emc2103_set_fan_speed_floor_keeps_gt_fan_spinning() {
        // Mirrors emc2103::Emc2103::set_fan_speed (the GT / Gamma Turbo path):
        //   duty = pwm_byte_for_pct(fan_duty_with_floor(pct, FLOOR, bypass)).
        // Below-floor and true-zero commands must yield a NON-ZERO 8-bit duty when
        // the lab bypass is OFF.
        for pct in [0u8, 5] {
            let duty = pwm_byte_for_pct(fan_duty_with_floor(pct, FAN_FLOOR_PCT, false));
            assert!(
                duty >= pwm_byte_for_pct(FAN_FLOOR_PCT) && duty > 0,
                "EMC2103 pct {pct} (no bypass) must floor to a non-zero duty, got {duty}"
            );
            assert_eq!(
                duty, 51,
                "EMC2103 floored 8-bit duty must be 255*20/100 = 51"
            );
        }
        // Only the explicit lab bypass lets a true-zero (full-stop) duty through.
        assert_eq!(
            pwm_byte_for_pct(fan_duty_with_floor(0, FAN_FLOOR_PCT, true)),
            0,
            "EMC2103 0% under lab bypass is a true-zero full stop"
        );
    }

    #[test]
    fn lab_safety_bypass_is_compile_time_only_not_runtime() {
        // XPSAFE-4: `lab_safety_bypass_enabled()` reads a COMPILE-TIME gate
        // (`option_env!`). The test binary is NOT built with the bypass, so it is
        // false — and a RUNTIME env var must NOT flip it (the removed std::env
        // arm). This pins that the HAL and supervisor can never disagree in a
        // std-env build.
        assert!(
            !lab_safety_bypass_enabled(),
            "bypass must be OFF when not compiled in"
        );
        std::env::set_var("DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS", "1");
        assert!(
            !lab_safety_bypass_enabled(),
            "a runtime env var must NOT activate the compile-time-only bypass"
        );
        std::env::remove_var("DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS");
        assert!(
            !lab_safety_bypass_enabled(),
            "bypass stays OFF after the runtime var is cleared"
        );
    }

    // ─── HALPWR-7: enable-time VOUT readback verification ────────────────────

    #[test]
    fn vout_in_tolerance_single_domain_is_reached() {
        let tol = VOUT_SETTLE_TOL_PER_DOMAIN_MV;
        // Single-ASIC: cmd 1.200 V, read 1.146 V (54 mV low) is within 150 mV.
        assert!(vout_reached_setpoint(1.146, 1.200, 1, tol));
        // Clearly inside the 150 mV band (140 mV low) is "reached".
        assert!(vout_reached_setpoint(1.060, 1.200, 1, tol));
        // Clearly OUTSIDE (160 mV low) is NOT reached → caller warns of a sag.
        // (Avoid asserting the exact 150 mV boundary — f32 rounding of 0.15 makes
        // the boundary point non-deterministic; the decision around it is what
        // matters, not the knife-edge.)
        assert!(!vout_reached_setpoint(1.040, 1.200, 1, tol));
        assert!(!vout_reached_setpoint(1.000, 1.200, 1, tol));
    }

    #[test]
    fn vout_tolerance_scales_with_domains_for_stacked_gt() {
        let tol = VOUT_SETTLE_TOL_PER_DOMAIN_MV;
        // 3-domain Hex stack: cmd 3.600 V total. A 0.40 V total sag = 133 mV per
        // domain, within the 3×150 mV = 450 mV stack tolerance.
        assert!(vout_reached_setpoint(3.200, 3.600, 3, tol));
        // The same 0.40 V sag on a SINGLE-domain board (would-be 400 mV) is out
        // of tolerance — confirms the per-domain scaling actually scales.
        assert!(!vout_reached_setpoint(1.200, 1.600, 1, tol));
        // A real stacked under-deliver (0.50 V total = 167 mV/domain) trips.
        assert!(!vout_reached_setpoint(3.000, 3.600, 3, tol));
    }

    #[test]
    fn vout_check_is_garbage_and_zero_safe() {
        let tol = VOUT_SETTLE_TOL_PER_DOMAIN_MV;
        // Non-finite reads or a zero/unread command MUST NOT warn (return true):
        // there is no meaningful setpoint to miss, and this is only a diagnostic.
        assert!(vout_reached_setpoint(f32::NAN, 1.200, 1, tol));
        assert!(vout_reached_setpoint(1.200, f32::NAN, 1, tol));
        assert!(vout_reached_setpoint(f32::INFINITY, 1.200, 1, tol));
        assert!(vout_reached_setpoint(0.0, 0.0, 1, tol)); // rail off / unread cmd
        assert!(vout_reached_setpoint(1.200, 0.0, 1, tol));
        // voltage_domains == 0 is treated as 1 (never divides/scales by zero).
        assert!(vout_reached_setpoint(1.146, 1.200, 0, tol));
    }

    // ─── INA260 over-current/over-power backstop (DS4432U hard-kill) ──────────
    // Mirrors main.rs: INA260_OC_MARGIN = 1.25, INA260_OC_DEBOUNCE_TICKS = 4.
    // Rated 25 W / 5 A envelope → trip thresholds 31.25 W / 6.25 A.
    const OC_MARGIN: f32 = 1.25;
    const OC_DEBOUNCE: u8 = 4;

    #[test]
    fn ina260_below_margin_is_not_over() {
        // Comfortably under both trip thresholds.
        assert!(!ina260_oc_over_envelope(20.0, 4.0, 25.0, 5.0, OC_MARGIN));
        // Right at the RATED envelope (before the margin) is still well below trip.
        assert!(!ina260_oc_over_envelope(25.0, 5.0, 25.0, 5.0, OC_MARGIN));
    }

    #[test]
    fn ina260_exactly_at_margin_is_not_over_strict_gt() {
        // EXACTLY at max*margin is NOT over — main.rs uses strict `>`.
        // 25 * 1.25 = 31.25; 5 * 1.25 = 6.25 (both exact in f32).
        assert!(!ina260_oc_over_envelope(31.25, 0.0, 25.0, 5.0, OC_MARGIN));
        assert!(!ina260_oc_over_envelope(0.0, 6.25, 25.0, 5.0, OC_MARGIN));
    }

    #[test]
    fn ina260_over_margin_trips_either_term() {
        // Power just over (31.26 > 31.25), current benign.
        assert!(ina260_oc_over_envelope(31.26, 0.0, 25.0, 5.0, OC_MARGIN));
        // Current just over (6.26 > 6.25), power benign.
        assert!(ina260_oc_over_envelope(0.0, 6.26, 25.0, 5.0, OC_MARGIN));
        // Either term alone trips the OR.
        assert!(ina260_oc_over_envelope(40.0, 0.0, 25.0, 5.0, OC_MARGIN));
    }

    #[test]
    fn ina260_nan_or_absent_telemetry_is_benign() {
        // A blanked (NaN) field must NEVER trip.
        assert!(!ina260_oc_over_envelope(
            f32::NAN,
            f32::NAN,
            25.0,
            5.0,
            OC_MARGIN
        ));
        // NaN power but valid (under) current → not over.
        assert!(!ina260_oc_over_envelope(
            f32::NAN,
            4.0,
            25.0,
            5.0,
            OC_MARGIN
        ));
        // A blanked field must NOT SUPPRESS a real over-current on the other field.
        assert!(ina260_oc_over_envelope(
            f32::NAN,
            99.0,
            25.0,
            5.0,
            OC_MARGIN
        ));
        // Infinities are non-finite → rejected as benign for that field.
        assert!(!ina260_oc_over_envelope(
            f32::INFINITY,
            f32::NAN,
            25.0,
            5.0,
            OC_MARGIN
        ));
        // A non-positive rated cap (unconfigured envelope) disables that term.
        assert!(!ina260_oc_over_envelope(
            9999.0, 9999.0, 0.0, 0.0, OC_MARGIN
        ));
    }

    #[test]
    fn ina260_single_transient_over_tick_does_not_cut() {
        // One over tick: strike 0 → 1, well under the 4-tick debounce.
        let strikes = ina260_oc_strike_next(true, 0);
        assert_eq!(strikes, 1);
        assert!(!ina260_oc_should_cut(strikes, OC_DEBOUNCE));
        // Even three consecutive over ticks are still below the cut threshold.
        let mut s = 0u8;
        for _ in 0..3 {
            s = ina260_oc_strike_next(true, s);
        }
        assert_eq!(s, 3);
        assert!(!ina260_oc_should_cut(s, OC_DEBOUNCE));
    }

    #[test]
    fn ina260_fourth_consecutive_over_tick_cuts() {
        // The 4th consecutive over tick reaches the debounce and hard-kills.
        let mut s = 0u8;
        let mut cut_on = None;
        for tick in 1u8..=6 {
            s = ina260_oc_strike_next(true, s);
            if ina260_oc_should_cut(s, OC_DEBOUNCE) {
                cut_on = Some(tick);
                break;
            }
        }
        assert_eq!(
            cut_on,
            Some(4),
            "must cut on exactly the 4th sustained over tick"
        );
    }

    #[test]
    fn ina260_healthy_tick_resets_strikes() {
        // Build 3 strikes, then a healthy tick zeroes the counter (no latent trip).
        let mut s = 0u8;
        for _ in 0..3 {
            s = ina260_oc_strike_next(true, s);
        }
        assert_eq!(s, 3);
        s = ina260_oc_strike_next(false, s);
        assert_eq!(
            s, 0,
            "a healthy (not-over) tick must reset the strike counter"
        );
        assert!(!ina260_oc_should_cut(s, OC_DEBOUNCE));
    }

    #[test]
    fn ina260_strike_counter_saturates_not_wraps() {
        // After a cut the loop keeps ticking over; the counter must SATURATE at
        // 255 (never wrap to 0 and momentarily un-arm the kill).
        let mut s = 250u8;
        for _ in 0..20 {
            s = ina260_oc_strike_next(true, s);
        }
        assert_eq!(s, 255);
        assert!(ina260_oc_should_cut(s, OC_DEBOUNCE));
    }
}
