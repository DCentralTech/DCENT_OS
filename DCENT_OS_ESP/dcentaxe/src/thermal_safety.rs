// DCENT_axe — thermal sensor-adequacy assessment (host-pure)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Host-pure temperature fold + sensor-adequacy decision for the supervisor loop.
//!
//! The `main.rs` thermal supervisor used to fold every sensor
//! `[chip_temp, gt_temp2, board_temp, inlet_temp, outlet_temp, vreg_temp]`
//! into `max_temp` inline and trip a THERMAL-BLIND kill **only** when EVERY
//! sensor was `None`. That was a fail-OPEN (ES-2): the ASIC-die diode
//! (`chip_temp` / `gt_temp2`) is the hottest, safety-relevant point, while
//! `board_temp` / `inlet_temp` / `outlet_temp` / `vreg_temp` are cooler
//! **proxies** that read the PCB / airflow / regulator ~10-20 C BELOW the die.
//! If the die sensor faulted (`chip_temp = None`) while a cooler proxy stayed
//! valid, `max_temp` was taken from the proxy — so the die could reach ~120 C
//! while `max_temp` read ~100 C and the overtemp cut fired late or never.
//!
//! This module extracts the fold + adequacy decision into a pure function over
//! plain `Option<f32>` inputs (no esp-idf, no locks) so it host-compiles and
//! unit-tests under `cargo test -p dcentaxe-core` — re-included via `#[path]` in
//! `dcentaxe-core/src/lib.rs`, the same single-source-of-truth pattern used by
//! `mqtt_ha.rs` / `metrics_render.rs` / `derived_metrics.rs`. The esp-idf
//! supervisor stays thin: it gathers the live readings and calls
//! [`evaluate_thermal`], then acts on the returned assessment.

/// Outcome of assessing one temperature-sensor snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalAssessment {
    /// Hottest valid reading across ALL sensors (die + proxy). `0.0` when every
    /// input is `None` — identical fold to the legacy inline
    /// `.filter_map(..).fold(0.0, f32::max)`.
    pub max_temp: f32,
    /// At least one sensor returned a reading. Drives the existing all-`None`
    /// THERMAL-BLIND path and the I2C-dead watchdog — same semantics as the
    /// legacy inline `chip_temp.is_some() || .. || vreg_temp.is_some()`.
    pub any_temp_valid: bool,
    /// **ES-2 fail-closed flag.** The board IS expected to report an ASIC-die
    /// temperature, but NO die-class sensor is currently readable while at least
    /// one cooler proxy still is. In that state `max_temp` is proxy-derived and
    /// understates the true die temperature, so the supervisor must treat it as
    /// blind for the die (escalate to fail-closed), NOT trust the proxy max.
    ///
    /// Always `false` when the board is not expected to carry a die sensor
    /// (`chip_die_expected = false`) so a board that legitimately reads only a
    /// regulator/board proxy is never false-killed.
    pub die_reading_blind: bool,
}

/// Fold the six live supervisor temperatures and decide sensor adequacy.
///
/// Inputs mirror the six `Option<f32>` locals in the `main.rs` loop:
/// * `chip_temp` / `gt_temp2` — **die-class**: the ASIC-junction diode reading(s).
///   `gt_temp2` is the second ASIC-die diode on EMC2103 (GT) boards and is
///   always `None` on every other board.
/// * `board_temp` / `inlet_temp` / `outlet_temp` / `vreg_temp` — cooler
///   **proxies** (EMC internal / TMP1075 airflow / TPS546 regulator), which read
///   ~10-20 C below the die.
///
/// `chip_die_expected` must be `true` for every board that physically carries an
/// ASIC-die / junction-diode sensor (EMC2101 external diode, EMC2103 chip
/// sensor, or Hex TMP1075s — i.e. any board whose configured `temp_sensor` is not
/// `None`, or any EMC2103 board). It must be `false` only for a board whose sole
/// configured thermal source is a cooler proxy (e.g. a custom board with
/// `temp_sensor = None` relying only on the TPS546 regulator temperature): on
/// those boards a missing `chip_temp` is NORMAL and must never be treated as
/// blind.
pub fn evaluate_thermal(
    chip_temp: Option<f32>,
    gt_temp2: Option<f32>,
    board_temp: Option<f32>,
    inlet_temp: Option<f32>,
    outlet_temp: Option<f32>,
    vreg_temp: Option<f32>,
    chip_die_expected: bool,
) -> ThermalAssessment {
    let chip_temp = finite_temperature(chip_temp);
    let gt_temp2 = finite_temperature(gt_temp2);
    let board_temp = finite_temperature(board_temp);
    let inlet_temp = finite_temperature(inlet_temp);
    let outlet_temp = finite_temperature(outlet_temp);
    let vreg_temp = finite_temperature(vreg_temp);

    let all = [
        chip_temp,
        gt_temp2,
        board_temp,
        inlet_temp,
        outlet_temp,
        vreg_temp,
    ];

    // Byte-identical to the legacy inline fold: hottest valid reading, 0.0 when
    // none are valid.
    let max_temp = all.iter().filter_map(|t| *t).fold(0.0_f32, f32::max);

    // Byte-identical to the legacy inline `any_temp_valid`.
    let any_temp_valid = all.iter().any(|t| t.is_some());

    // Die-class readings vs cooler proxies.
    let have_die_reading = chip_temp.is_some() || gt_temp2.is_some();
    let have_proxy_reading = board_temp.is_some()
        || inlet_temp.is_some()
        || outlet_temp.is_some()
        || vreg_temp.is_some();

    // Fail-closed (ES-2): on a die-equipped board, losing EVERY die sensor while
    // a cooler proxy remains means `max_temp` is proxy-derived and can sit
    // 10-20 C BELOW the true die temp. Flag it so the supervisor escalates to
    // the THERMAL-BLIND fail-closed path instead of trusting the proxy max.
    //
    // Note this is mutually exclusive with the all-`None` case: when
    // `!any_temp_valid`, `have_proxy_reading` is false, so `die_reading_blind`
    // is false and the existing all-`None` BLIND path handles it.
    let die_reading_blind = chip_die_expected && !have_die_reading && have_proxy_reading;

    ThermalAssessment {
        max_temp,
        any_temp_valid,
        die_reading_blind,
    }
}

fn finite_temperature(reading: Option<f32>) -> Option<f32> {
    reading.filter(|temp| temp.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHIP_EXPECTED: bool = true;
    const NO_CHIP: bool = false;

    // ── ES-2 core: die sensor faults while a cooler proxy stays valid ────────
    // The reported bug: chip_temp=None + board=70 on a chip-expected board must
    // be BLIND (fail-closed), NOT "70 is fine".
    #[test]
    fn chip_none_with_valid_proxy_on_expected_board_is_die_blind() {
        let a = evaluate_thermal(
            None,       // chip_temp — die sensor faulted
            None,       // gt_temp2
            Some(70.0), // board_temp — cooler proxy, still valid
            None,
            None,
            None,
            CHIP_EXPECTED,
        );
        assert!(
            a.die_reading_blind,
            "must flag die-blind, not trust the proxy"
        );
        assert!(a.any_temp_valid, "a proxy is still valid (not all-None)");
        // The proxy max is deliberately NOT trusted for the die: the flag tells
        // the supervisor to fail closed regardless of this value.
        assert_eq!(a.max_temp, 70.0);
    }

    #[test]
    fn chip_none_with_multiple_cooler_proxies_is_die_blind() {
        // Realistic single-ASIC TPS546 board (e.g. Gamma / Ultra 207): external
        // diode dead, EMC internal (board) + regulator (vreg) still read cool.
        let a = evaluate_thermal(
            None,       // chip_temp (external diode) faulted
            None,       // gt_temp2
            Some(85.0), // board_temp (EMC internal proxy)
            None,       // inlet
            None,       // outlet
            Some(90.0), // vreg_temp (TPS546 proxy)
            CHIP_EXPECTED,
        );
        assert!(a.die_reading_blind);
        assert!(a.any_temp_valid);
        assert_eq!(a.max_temp, 90.0);
    }

    // ── Normal: every sensor present → not blind, correct fold ───────────────
    #[test]
    fn all_sensors_present_is_normal() {
        let a = evaluate_thermal(
            Some(95.0), // chip die (hottest)
            None,
            Some(80.0),
            Some(45.0),
            Some(60.0),
            Some(72.0),
            CHIP_EXPECTED,
        );
        assert!(!a.die_reading_blind, "die reading present → never blind");
        assert!(a.any_temp_valid);
        assert_eq!(a.max_temp, 95.0, "max is the die reading");
    }

    #[test]
    fn die_hotter_than_proxy_folds_to_die() {
        // Proxy cool, die hot — the fold must surface the die.
        let a = evaluate_thermal(
            Some(110.0), // die
            None,
            Some(90.0), // proxy 20 C below
            None,
            None,
            None,
            CHIP_EXPECTED,
        );
        assert!(!a.die_reading_blind);
        assert_eq!(a.max_temp, 110.0);
    }

    // ── No false-kill: a board that legitimately has NO chip diode ───────────
    #[test]
    fn chipless_board_with_proxies_is_not_blind() {
        // e.g. a custom board with temp_sensor=None relying on TPS546 vreg temp.
        // chip_temp is None BY DESIGN here — must NOT be treated as blind.
        let a = evaluate_thermal(
            None,       // chip_temp — normal absence
            None,       // gt_temp2
            None,       // board_temp
            None,       // inlet
            None,       // outlet
            Some(65.0), // vreg_temp — the board's only (proxy) source
            NO_CHIP,
        );
        assert!(
            !a.die_reading_blind,
            "chipless board must never be false-killed"
        );
        assert!(a.any_temp_valid);
        assert_eq!(a.max_temp, 65.0);
    }

    // ── all-None → BLIND via any_temp_valid, NOT via die_reading_blind ───────
    #[test]
    fn all_none_is_all_sensors_failed_blind() {
        let a = evaluate_thermal(None, None, None, None, None, None, CHIP_EXPECTED);
        assert!(!a.any_temp_valid, "all-None → existing THERMAL-BLIND path");
        assert!(
            !a.die_reading_blind,
            "all-None is handled by any_temp_valid, not die_reading_blind (no proxy present)"
        );
        assert_eq!(a.max_temp, 0.0);
    }

    #[test]
    fn all_none_on_chipless_board_is_also_all_sensors_failed() {
        let a = evaluate_thermal(None, None, None, None, None, None, NO_CHIP);
        assert!(!a.any_temp_valid);
        assert!(!a.die_reading_blind);
        assert_eq!(a.max_temp, 0.0);
    }

    #[test]
    fn non_finite_temperatures_do_not_count_as_valid() {
        let a = evaluate_thermal(
            Some(f32::NAN),
            Some(f32::INFINITY),
            Some(f32::NEG_INFINITY),
            None,
            None,
            None,
            CHIP_EXPECTED,
        );
        assert!(
            !a.any_temp_valid,
            "NaN/Inf readings must behave like missing sensors"
        );
        assert!(!a.die_reading_blind);
        assert_eq!(a.max_temp, 0.0);
    }

    #[test]
    fn non_finite_die_with_valid_proxy_is_die_blind() {
        let a = evaluate_thermal(
            Some(f32::NAN),
            Some(f32::INFINITY),
            Some(70.0),
            None,
            None,
            None,
            CHIP_EXPECTED,
        );
        assert!(
            a.die_reading_blind,
            "invalid die readings cannot make a cooler proxy trusted"
        );
        assert!(a.any_temp_valid);
        assert_eq!(a.max_temp, 70.0);
    }

    // ── GT (EMC2103) two-die behavior ────────────────────────────────────────
    #[test]
    fn gt_primary_die_dead_but_secondary_die_valid_is_not_blind() {
        // GT has TWO ASIC-die sensors (chip_temp + gt_temp2). If only the
        // primary faults, we still have a real die reading → not blind, and the
        // secondary die temp feeds max_temp (it reads AT die temp, not a proxy).
        let a = evaluate_thermal(
            None,        // primary chip die dead
            Some(102.0), // secondary die still valid
            None,
            None,
            None,
            Some(80.0), // vreg proxy
            CHIP_EXPECTED,
        );
        assert!(!a.die_reading_blind, "a valid die reading remains");
        assert_eq!(a.max_temp, 102.0);
    }

    #[test]
    fn gt_both_dies_dead_with_vreg_proxy_is_die_blind() {
        // Both EMC2103 die sensors dead, only the TPS546 regulator proxy left.
        let a = evaluate_thermal(
            None, // primary die dead
            None, // secondary die dead
            None,
            None,
            None,
            Some(88.0), // vreg proxy
            CHIP_EXPECTED,
        );
        assert!(a.die_reading_blind);
        assert!(a.any_temp_valid);
        assert_eq!(a.max_temp, 88.0);
    }

    // ── Die present but proxy hotter (rare) still not blind ──────────────────
    #[test]
    fn die_present_with_hotter_proxy_is_not_blind_and_folds_to_proxy() {
        let a = evaluate_thermal(
            Some(70.0), // die reading present
            None,
            None,
            None,
            None,
            Some(75.0), // proxy happens to read higher
            CHIP_EXPECTED,
        );
        assert!(!a.die_reading_blind, "we have a die reading → not blind");
        assert_eq!(a.max_temp, 75.0, "fold still surfaces the hottest reading");
    }
}
