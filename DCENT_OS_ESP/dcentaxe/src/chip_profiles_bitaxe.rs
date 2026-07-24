//! Single-chip BitAxe Vdd envelopes — DO NOT confuse with chain-pack
//! catalog volts.
//!
//! BitAxe boards have ONE BM1366/BM1368/BM1370 chip on a single rail.
//! The wave-4 catalog ( §1) reports
//! per-hashboard preset rows like BHB42701 BM1366 at 1.22-1.26 V — those
//! are CHAIN-PACK voltages across 110 chips in series. Copying that into
//! a single-chip BitAxe would put 1.22 V on a chip whose individual rail
//! envelope is 1.10-1.25 V — borderline-overvolt or clear OV.
//!
//! §6 the per-chip envelope
//! is ALWAYS the source-of-truth for single-chip BitAxe. The chain-pack
//! catalog rows are reference data for multi-chip Antminer hashboards
//! and MUST be down-divided before any single-chip use:
//!     single_chip_v ≈ chain_v / chips_in_series
//! For BHB42701 (110 chips at 1.22 V chain), the single-chip equivalent
//! is ~11.1 mV per chip — clearly nonsensical, indicating the chain volts
//! are NOT a per-chip rail measurement but a chain-rail measurement
//! (different voltage domain entirely).
//!
//! The tables below are sourced from BitAxe-platform datasheets +
//! `dcentaxe-asic` driver documentation, NOT from the wave-4 catalog.
//! Wave-4 catalog data is OUT OF SCOPE for single-chip envelopes.

#![allow(dead_code)]

/// Single-chip envelope for BM1366 (used in BitAxe Hex / Ultra variants).
/// Frequency × per-chip Vdd × estimated wall watts.
/// Datasheet-anchored; live-confirmed at home-bench during phase A/A2 work.
pub const BM1366_BITAXE_PROFILE: &[(u32, f32, f32)] = &[
    (400, 1.10, 12.0),
    (450, 1.12, 13.5),
    (500, 1.15, 15.5),
    (525, 1.17, 17.0),
    (550, 1.19, 18.5),
    (575, 1.21, 20.0),
    (600, 1.23, 22.0),
    (625, 1.25, 24.0),
];

/// Single-chip envelope for BM1368 (used in BitAxe variants).
pub const BM1368_BITAXE_PROFILE: &[(u32, f32, f32)] = &[
    (450, 1.10, 14.0),
    (500, 1.13, 16.5),
    (525, 1.15, 18.0),
    (550, 1.17, 19.5),
    (575, 1.19, 21.0),
    (600, 1.21, 22.5),
    (625, 1.23, 24.0),
];

/// Single-chip envelope for BM1370 (used in BitAxe Ultra / GT).
pub const BM1370_BITAXE_PROFILE: &[(u32, f32, f32)] = &[
    (475, 1.10, 14.5),
    (500, 1.12, 16.0),
    (525, 1.14, 17.5),
    (550, 1.16, 19.0),
    (575, 1.18, 20.5),
    (600, 1.20, 22.0),
    (625, 1.22, 23.5),
    (650, 1.24, 25.0),
];

/// Single-chip envelope for BM1397 (used in BitAxe Max).
///
/// AUTOTUNE-3: BM1397 was the only supported BitAxe ASIC with no descent
/// profile table, so `TargetWatts` mode entered `WattageDescent` and never
/// proposed a step (logged "no profile for asic_model, holding" every window
/// and could only exit convergence by luck). This table makes the BitAxe Max
/// `TargetWatts` loop functional.
///
/// The BM1397 is a 7 nm single-chip part (BitAxe Max, ~400 GH/s) whose stock
/// core rail sits ~1.30-1.45 V on the dedicated multi-chip-Antminer hashboard,
/// but on a SINGLE-chip BitAxe the per-chip rail envelope is the source of
/// truth (see the module header) — datasheet-anchored single-chip Vdd is in
/// the same 1.10-1.25 V band the other BitAxe chips use, so every row stays
/// inside [`SINGLE_CHIP_VDD_MIN`, `SINGLE_CHIP_VDD_MAX`]. The BitAxe Max board
/// `safe` envelope caps `max_power_w` at 25 W and `max_frequency` at 400 MHz
/// (config.rs `BitAxeModel::Max` `PowerLimits::safe`), so the wall-watt column
/// tops out conservatively well below the BitAxe Max PSU budget. Frequencies
/// match the BitAxe Max PLL window
/// (60-200 MHz nominal stock; the higher rows here are the autotuner's
/// envelope ceiling, re-clamped live by `qualify_operating_point` to the
/// board's 410 MHz cap before anything is applied — the descent never raises
/// the board limit, it only ever picks a row at or below the closest watts).
pub const BM1397_BITAXE_PROFILE: &[(u32, f32, f32)] = &[
    (200, 1.10, 8.0),
    (300, 1.12, 11.0),
    (400, 1.15, 14.5),
    (450, 1.17, 17.0),
    (500, 1.19, 19.5),
    (550, 1.21, 22.0),
    (600, 1.23, 25.0),
];

/// Safety envelope: max single-chip Vdd. Hard-fail if descent ever
/// proposes voltage above this value.
pub const SINGLE_CHIP_VDD_MAX: f32 = 1.25;

/// Safety envelope: min single-chip Vdd. Below this the chip will
/// fail to lock PLLs.
pub const SINGLE_CHIP_VDD_MIN: f32 = 1.05;

/// Safety envelope: max frequency BitAxe single-chip designs.
pub const BITAXE_MAX_FREQ_MHZ: u32 = 700;

// ─── Pure autotuner decision logic (host-testable) ───────────────────────────
//
// The `Autotuner` (autotuner.rs) lives in the `dcentaxe` binary crate, which
// pulls esp-idf at module scope and so cannot host-compile — its `tick()`
// wiring is review-only. This module is dependency-free pure logic, so the
// load-bearing decisions (best-fit profile-row selection under the operator
// band, the per-step settle gate, and the persist-only-on-change gate) are
// extracted here as pure free fns and unit-tested on the host below. The
// espidf-only autotuner calls these so the host-tested decision is the SINGLE
// source of truth (same pattern as `dcentaxe_hal::safety`'s autotuner backoff
// helpers).
//
// To run these tests in CI without the ESP-IDF toolchain, this module is
// re-included into `dcentaxe-core` via `#[path]` (mirroring `config.rs` /
// `ota_signature.rs`); see the dcentaxe-core lib.rs handoff note.

/// AUTOTUNE-3 + XPAUTO-4: pick the profile row whose predicted wattage is
/// closest to `target_watts`, **restricted to the operator's freq/voltage
/// band** `[band_min_freq, band_max_freq]` × `[band_min_mv, band_max_mv]`.
///
/// This is the single source of truth for the WattageDescent best-fit search.
/// It diverges from the old in-line search in exactly one safe way: it ignores
/// rows outside the operator band, so the descent reasons and logs WITHIN the
/// band (matching the DCENT_OS `PowerTargetController` / `DpsWalker.clamp_freq`
/// band-clamp contract). Downstream `qualify_operating_point` still re-clamps
/// the applied point — this only stops the search from wasting iterations on,
/// and emitting misleading logs about, a row it would never apply.
///
/// Tie-break: equal closeness prefers the LOWER voltage (efficiency), then the
/// LOWER frequency — fully deterministic regardless of table order.
///
/// Returns `None` when no row falls inside the band (caller holds setpoint).
/// Pure (no alloc, no I/O): host-tested. `band_min_mv`/`band_max_mv` are the
/// per-chip rail in mV; the table voltages are volts, compared after ×1000.
pub fn best_fit_row_in_band(
    target_watts: f32,
    profile: &[(u32, f32, f32)],
    band_min_freq: f32,
    band_max_freq: f32,
    band_min_mv: u16,
    band_max_mv: u16,
) -> Option<(u32, f32, f32)> {
    let mut best: Option<(u32, f32, f32)> = None;
    let mut best_score = f32::MAX;
    for &(freq, voltage, watts) in profile {
        // Operator-band filter (XPAUTO-4). freq is u32 MHz; the band is f32.
        let freq_f = freq as f32;
        if freq_f < band_min_freq || freq_f > band_max_freq {
            continue;
        }
        let voltage_mv = (voltage * 1000.0) as u16;
        if voltage_mv < band_min_mv || voltage_mv > band_max_mv {
            continue;
        }
        let score = (watts - target_watts).abs();
        let is_better = match best {
            None => true,
            Some((prev_f, prev_v, _)) => {
                if score < best_score - f32::EPSILON {
                    true
                } else if (score - best_score).abs() < f32::EPSILON {
                    // Tie: lower voltage wins; then lower frequency.
                    let prev_v_mv = (prev_v * 1000.0) as u16;
                    voltage_mv < prev_v_mv || (voltage_mv == prev_v_mv && freq < prev_f)
                } else {
                    false
                }
            }
        };
        if is_better {
            best_score = score;
            best = Some((freq, voltage, watts));
        }
    }
    best
}

/// AUTOTUNE-1: per-step settle/dwell gate for `WattageDescent`.
///
/// Every other autotuner phase (Profiling 20 s, Optimizing/Maintaining 30/60 s)
/// gates its decision on `last_action_time.elapsed()`, but `WattageDescent`
/// had none — once the power window filled, it re-commanded a setpoint on every
/// 5 s tick. This returns `true` only after at least `settle_secs` have elapsed
/// since the last action, so the rail (and the rolling power window) can settle
/// before the next convergence/step decision. Pure: host-tested.
pub const fn descent_settle_elapsed_ok(elapsed_secs: u64, settle_secs: u64) -> bool {
    elapsed_secs >= settle_secs
}

/// AUTOTUNE-9: should the Maintaining-phase last-known-good be re-persisted to
/// NVS this tick? Returns `true` only when the operating point has MOVED vs the
/// last value written, so a steady healthy miner stops rewriting an identical
/// NVS record every 60 s (flash-wear). A freq move of >= `freq_eps_mhz` MHz OR
/// any voltage change counts as a real change. The very first persist (no prior
/// record) is always allowed via `had_prior == false`.
///
/// Pure: host-tested. `freq_eps_mhz` is the dead-band (e.g. the fine step) so
/// f32 jitter below the autotuner's own resolution never triggers a write.
pub fn last_known_good_changed(
    had_prior: bool,
    prev_freq: f32,
    prev_voltage_mv: u16,
    new_freq: f32,
    new_voltage_mv: u16,
    freq_eps_mhz: f32,
) -> bool {
    if !had_prior {
        return true;
    }
    new_voltage_mv != prev_voltage_mv || (new_freq - prev_freq).abs() >= freq_eps_mhz
}

/// Maximum acceptable HW error rate (errors / nonces). Above this the chip is
/// "spicy"/unstable. SINGLE source of truth for the autotuner's stability
/// ceiling AND `silicon_grade`'s "normal" upper bound — `autotuner.rs` imports
/// this const so the engine and this host-tested pure layer can never drift.
pub const MAX_ERROR_RATE: f64 = 0.02; // 2%

/// AUTOTUNE-A1: pure mirror of the engine's `AutotuneMode` so the host-pure
/// `best_point_for_mode` below stays host-compilable. The real `AutotuneMode`
/// lives in the espidf-only `shared.rs`, which cannot be imported into this
/// re-included pure module, so the engine maps its mode onto this enum at the
/// call site (`Autotuner::find_best_point`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BestPointMode {
    /// Pick the maximum-hashrate point.
    MaxHashrate,
    /// Pick the minimum-J/TH point.
    BestEfficiency,
    /// Power-budget mode. The engine selects TargetWatts with its own
    /// power-distance pick (it needs each point's measured watts, which the
    /// row tuple here does NOT carry, and it is the WattageDescent entry
    /// setpoint), so this fn returns `None` for it and the engine never routes
    /// TargetWatts through here.
    TargetWatts,
    /// Pick the maximum-hashrate point whose temp is at/under `target`.
    TargetTemp,
}

/// AUTOTUNE-A1: choose the best operating-point index from a slice of profiled
/// points for the given mode. This is the SINGLE source of truth for
/// `Autotuner::find_best_point`'s MaxHashrate / BestEfficiency / TargetTemp
/// selection (TargetWatts keeps its own power-distance pick in the engine, so
/// this returns `None` for it).
///
/// `points` rows are `(freq_mhz, voltage_v, hashrate_ghs, jth, temp_c)`; the
/// caller pre-filters to stable points. Returns the chosen index into `points`,
/// or `None` when no point is eligible — empty input, TargetWatts (by design),
/// or a TargetTemp ceiling below EVERY point. The `None` result is
/// load-bearing: the engine must treat it as a clean "hold last-known-good /
/// idle" outcome, never as a reason to churn forever (the A1 bug fix).
///
/// NaN-safe: float compares use `partial_cmp(..).unwrap_or(Equal)` (matching
/// the engine's `fcmp`). Tie semantics are byte-identical to the engine's
/// `Iterator::max_by`/`min_by`: a max-by tie keeps the LAST equal element, a
/// min-by tie keeps the FIRST — reproduced here via `enumerate()`. Pure: no
/// alloc, no I/O — host-tested.
pub fn best_point_for_mode(
    points: &[(f32, f32, f64, f32, f32)],
    mode: BestPointMode,
    target: f32,
) -> Option<usize> {
    use core::cmp::Ordering;
    fn fcmp<T: PartialOrd>(a: &T, b: &T) -> Ordering {
        a.partial_cmp(b).unwrap_or(Ordering::Equal)
    }
    match mode {
        // `max_by`: on equality keeps the LATER element (std semantics).
        BestPointMode::MaxHashrate => points
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| fcmp(&a.2, &b.2))
            .map(|(i, _)| i),
        // `min_by`: on equality keeps the EARLIER element (std semantics).
        BestPointMode::BestEfficiency => points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| fcmp(&a.3, &b.3))
            .map(|(i, _)| i),
        // Max hashrate among points at/under the temp ceiling.
        BestPointMode::TargetTemp => points
            .iter()
            .enumerate()
            .filter(|(_, p)| p.4 <= target)
            .max_by(|(_, a), (_, b)| fcmp(&a.2, &b.2))
            .map(|(i, _)| i),
        // The engine owns the TargetWatts pick (power-distance / best-fit row).
        BestPointMode::TargetWatts => None,
    }
}

/// AUTOTUNE-A3: compute J/TH from wall power (W) and hashrate (GH/s).
///
/// Mirrors the engine's `power_w / hashrate * 1000.0` exactly for the normal
/// case (e.g. 16 W / 1000 GH/s → 16.0), keeping the original guard that a
/// (near-)zero hashrate yields `f32::MAX` (never `0`/`inf`) so a not-yet-hashing
/// point can never look like the most-efficient one.
///
/// A3 safety hardening (NEW vs the inline engine code): a NON-finite (NaN/±inf)
/// or NEGATIVE `power_w` also yields `f32::MAX`. The old inline form would have
/// produced `NaN` (which `partial_cmp` treats as Equal and could leave a stale
/// "best") or a NEGATIVE J/TH (which would spuriously WIN BestEfficiency's
/// `min_by`). A zero-or-negative "best" J/TH must never win selection, so this
/// clamps such inputs to the worst possible score. Pure: host-tested.
pub fn compute_jth(power_w: f32, hashrate_ghs: f64) -> f32 {
    if !power_w.is_finite() || power_w < 0.0 {
        return f32::MAX;
    }
    if hashrate_ghs > 0.001 {
        power_w / hashrate_ghs as f32 * 1000.0
    } else {
        f32::MAX
    }
}

/// AUTOTUNE-A2: classify silicon quality from measured J/TH, HW-error rate and
/// core voltage. SINGLE source of truth for `Autotuner::silicon_grade`. The
/// label is a DERIVED, measured grade (NOT a factory bin) — see the
/// `silicon_grade_is_labeled_derived_with_honest_unknown` dashboard pin.
///
/// Thresholds (verified against the engine, do not paraphrase):
/// * `gold`   — `err <= 0.002 && jth <= 16.5 && voltage_mv <= 1150`
/// * `strong` — `err <= 0.01  && jth <= 18.5`
/// * `normal` — `err <= MAX_ERROR_RATE` (0.02)
/// * `spicy`  — otherwise (error rate above the stability ceiling)
///
/// Pure: host-tested.
pub fn silicon_grade(jth: f32, delta_error_rate: f64, voltage_mv: u16) -> &'static str {
    if delta_error_rate <= 0.002 && jth <= 16.5 && voltage_mv <= 1150 {
        "gold"
    } else if delta_error_rate <= 0.01 && jth <= 18.5 {
        "strong"
    } else if delta_error_rate <= MAX_ERROR_RATE {
        "normal"
    } else {
        "spicy"
    }
}

// ─── A5 / XPAUTO-5: autotune POST target input validation ─────────────────────

/// A5: board-AGNOSTIC absolute ceiling for an accepted `TargetWatts` autotune
/// setpoint, in wall watts.
///
/// `POST /api/mining/autotune` is mode-aware but NOT board-aware (it only knows
/// the requested target), so this ceiling must sit at or above the highest
/// per-model overclock PSU budget across EVERY supported board so a legitimate
/// high-power-board target is never rejected: the maximum is the DCENT_axe Hex
/// BM1397 at 195 W (130 W `PowerLimits::safe` × 1.5 overclock; next-highest is
/// the Hex Supra overclock at 150 W — see `config.rs` `PowerLimits`). 200 W is
/// just above that, so any request above it cannot be a legitimate single-board
/// target and is rejected at the API door. This is defense-in-depth, NOT the
/// operating limit: the board-specific `PowerLimits` clamp in
/// `qualify_operating_point` still applies downstream and bounds the actual
/// applied point per model.
pub const MAX_AUTOTUNE_TARGET_WATTS: f32 = 200.0;

/// A5: minimum accepted `TargetTemp` autotune setpoint, in °C. Below this no
/// operating point can ever run that cold (ambient + silicon floor), so the
/// autotuner's eligible-point filter empties and the tuner is stranded — an
/// explicit `TargetTemp=0` is the concrete strand this rejects at the door.
pub const MIN_AUTOTUNE_TARGET_TEMP_C: f32 = 40.0;

/// A5: maximum accepted `TargetTemp` autotune setpoint, in °C. Above this is
/// past the BM-series thermal-safety ceiling (the fan curve / thermal shutdown
/// guard miners well below this), so values like 96 or 500 are rejected.
pub const MAX_AUTOTUNE_TARGET_TEMP_C: f32 = 95.0;

/// A5 / XPAUTO-5: validate the `POST /api/mining/autotune` `target` for the
/// selected mode, returning the value to store on success or a 400 message on
/// rejection.
///
/// The handler previously stored ANY `target` as `target_value` with no bounds
/// or mode-awareness, so `TargetTemp=0`/`500` and `TargetWatts<=0` were all
/// accepted. A `TargetTemp=0` then makes the autotuner's eligible-point filter
/// empty and strands the tuner (a separate fix makes that fail safe, but the
/// value should be rejected at the door — defense in depth). This is the SINGLE
/// host-tested source of truth for that boundary check; the espidf-only handler
/// maps its `AutotuneMode` onto the pure `BestPointMode` and calls this, exactly
/// like the `best_point_for_mode` delegation. It mirrors the SPIRIT of the
/// fan-curve `target_temp=0 is ambiguous` guard, not its code.
///
/// * `TargetWatts`   — require `0.0 < target <= MAX_AUTOTUNE_TARGET_WATTS`;
///   reject NaN/inf and non-positive / over-budget values.
/// * `TargetTemp`    — require `MIN_AUTOTUNE_TARGET_TEMP_C ..= MAX_AUTOTUNE_TARGET_TEMP_C`;
///   reject NaN/inf, `0`, `39`, `96`, `500`, etc.
/// * `MaxHashrate` / `BestEfficiency` — `target` is ignored by these modes, so
///   any value (including `0`/NaN) is accepted unchanged (`Ok(target)`).
///
/// Pure: no alloc, no I/O — host-tested.
impl BestPointMode {
    /// Parse an autotuner-mode API/config string (the values used by the REST
    /// API, MCP, and power-schedule slots) into the safety-envelope mode. Returns
    /// `None` for an unknown string so callers can fail closed.
    pub fn from_api_str(s: &str) -> Option<Self> {
        match s {
            "max_hashrate" => Some(Self::MaxHashrate),
            "best_efficiency" => Some(Self::BestEfficiency),
            "target_watts" => Some(Self::TargetWatts),
            "target_temp" => Some(Self::TargetTemp),
            _ => None,
        }
    }
}

pub fn validate_autotune_target(mode: BestPointMode, target: f32) -> Result<f32, &'static str> {
    match mode {
        BestPointMode::TargetWatts => {
            if !target.is_finite() {
                return Err("TargetWatts target must be a finite number");
            }
            if target <= 0.0 {
                return Err("TargetWatts target must be greater than 0 W");
            }
            if target > MAX_AUTOTUNE_TARGET_WATTS {
                return Err("TargetWatts target exceeds the maximum board power budget");
            }
            Ok(target)
        }
        BestPointMode::TargetTemp => {
            if !target.is_finite() {
                return Err("TargetTemp target must be a finite number");
            }
            if !(MIN_AUTOTUNE_TARGET_TEMP_C..=MAX_AUTOTUNE_TARGET_TEMP_C).contains(&target) {
                return Err("TargetTemp target must be between 40 and 95 \u{b0}C");
            }
            Ok(target)
        }
        // MaxHashrate / BestEfficiency ignore the target entirely — never reject.
        BestPointMode::MaxHashrate | BestPointMode::BestEfficiency => Ok(target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All shipped single-chip profile tables, incl. the AUTOTUNE-3 BM1397.
    const ALL_PROFILES: &[&[(u32, f32, f32)]] = &[
        BM1366_BITAXE_PROFILE,
        BM1368_BITAXE_PROFILE,
        BM1370_BITAXE_PROFILE,
        BM1397_BITAXE_PROFILE,
    ];

    #[test]
    fn all_voltages_within_envelope() {
        for &(freq, voltage, _watts) in BM1366_BITAXE_PROFILE
            .iter()
            .chain(BM1368_BITAXE_PROFILE.iter())
            .chain(BM1370_BITAXE_PROFILE.iter())
            .chain(BM1397_BITAXE_PROFILE.iter())
        {
            assert!(
                voltage >= SINGLE_CHIP_VDD_MIN,
                "freq {} V {} below min",
                freq,
                voltage
            );
            assert!(
                voltage <= SINGLE_CHIP_VDD_MAX,
                "freq {} V {} above max",
                freq,
                voltage
            );
            assert!(freq <= BITAXE_MAX_FREQ_MHZ, "freq {} above max", freq);
        }
    }

    #[test]
    fn chain_pack_volt_rejected_implicitly() {
        // 1.32 V chain-pack from BHB42601 must be REJECTED for single-chip use.
        // This test documents the invariant — no actual rejection logic, but
        // any future code that copies chain volts will violate the assertion.
        let chain_pack_voltage = 1.32_f32;
        assert!(
            chain_pack_voltage > SINGLE_CHIP_VDD_MAX,
            "1.32 V chain-pack must NOT pass single-chip Vdd ceiling"
        );
    }

    #[test]
    fn profiles_are_monotonic_in_voltage() {
        // Sanity: voltage must be non-decreasing as frequency rises within a
        // profile. (Higher freq → at least equal voltage, never lower.)
        for table in ALL_PROFILES {
            for pair in table.windows(2) {
                assert!(
                    pair[1].1 >= pair[0].1,
                    "voltage regression: ({}, {}) -> ({}, {})",
                    pair[0].0,
                    pair[0].1,
                    pair[1].0,
                    pair[1].1
                );
            }
        }
    }

    #[test]
    fn profiles_are_monotonic_in_watts() {
        // Higher freq + higher voltage → must predict higher (or equal) wall
        // watts. If this ever regresses, the binary-search tie-breaker breaks.
        for table in ALL_PROFILES {
            for pair in table.windows(2) {
                assert!(
                    pair[1].2 >= pair[0].2,
                    "watts regression: ({}, _, {}) -> ({}, _, {})",
                    pair[0].0,
                    pair[0].2,
                    pair[1].0,
                    pair[1].2
                );
            }
        }
    }

    // ─── AUTOTUNE-3: BM1397 (BitAxe Max) profile now exists + is usable ──────

    #[test]
    fn bm1397_profile_is_non_empty_and_in_envelope() {
        assert!(
            !BM1397_BITAXE_PROFILE.is_empty(),
            "BM1397 must have a descent profile so TargetWatts is functional"
        );
        for &(freq, voltage, watts) in BM1397_BITAXE_PROFILE {
            assert!(
                voltage >= SINGLE_CHIP_VDD_MIN,
                "BM1397 {freq} V {voltage} < min"
            );
            assert!(
                voltage <= SINGLE_CHIP_VDD_MAX,
                "BM1397 {freq} V {voltage} > max"
            );
            assert!(freq <= BITAXE_MAX_FREQ_MHZ, "BM1397 {freq} > max freq");
            assert!(watts > 0.0, "BM1397 {freq} watts must be positive");
        }
    }

    // ─── AUTOTUNE-3 + XPAUTO-4: best_fit_row_in_band ────────────────────────

    /// A wide band that excludes nothing (mirrors the global envelope) — used
    /// to confirm the new fn reproduces the OLD closest-watts behavior.
    fn wide_band() -> (f32, f32, u16, u16) {
        (
            0.0,
            BITAXE_MAX_FREQ_MHZ as f32,
            SINGLE_CHIP_VDD_MIN as u16 * 1000,
            u16::MAX,
        )
    }

    #[test]
    fn best_fit_matches_old_closest_watts_wide_band() {
        // With a non-restricting band, the result must equal the legacy
        // closest-watts pick (parity with the now-retired hand-mirror that
        // lived in dcentaxe/tests/test_wattage_autotune.rs).
        let (lf, hf, lv, hv) = wide_band();
        // Target 15.0 W on BM1366 → (500, 1.15, 15.5) (score 0.5).
        assert_eq!(
            best_fit_row_in_band(15.0, BM1366_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((500, 1.15, 15.5))
        );
        // Target 20.0 W on BM1366 → exact (575, 1.21, 20.0).
        assert_eq!(
            best_fit_row_in_band(20.0, BM1366_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((575, 1.21, 20.0))
        );
        // Target 15.0 W on BM1370 → (475, 1.10, 14.5) (score 0.5 < 1.0).
        assert_eq!(
            best_fit_row_in_band(15.0, BM1370_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((475, 1.10, 14.5))
        );
    }

    #[test]
    fn best_fit_lower_voltage_tie_breaker_is_order_independent() {
        let (lf, hf, lv, hv) = wide_band();
        // Two rows hit 15 W exactly; lower voltage (1.19) must win regardless
        // of insertion order.
        let fwd: &[(u32, f32, f32)] = &[(575, 1.19, 15.0), (600, 1.21, 15.0)];
        let rev: &[(u32, f32, f32)] = &[(600, 1.21, 15.0), (575, 1.19, 15.0)];
        assert_eq!(
            best_fit_row_in_band(15.0, fwd, lf, hf, lv, hv),
            Some((575, 1.19, 15.0))
        );
        assert_eq!(
            best_fit_row_in_band(15.0, rev, lf, hf, lv, hv),
            Some((575, 1.19, 15.0))
        );
    }

    #[test]
    fn best_fit_respects_operator_freq_band() {
        // XPAUTO-4: operator narrowed freq to [400, 525] (quiet profile). Even
        // though 575 MHz is the exact 20 W match on BM1366, it is OUT of band,
        // so the closest IN-BAND row (525, 1.17, 17.0) must be chosen instead —
        // the descent must NOT reason about a row it cannot apply.
        let pick = best_fit_row_in_band(20.0, BM1366_BITAXE_PROFILE, 400.0, 525.0, 1000, u16::MAX);
        assert_eq!(pick, Some((525, 1.17, 17.0)));
    }

    #[test]
    fn best_fit_respects_operator_voltage_band() {
        // Operator capped voltage at 1.15 V (1150 mV). The best <=20 W in-band
        // row is now (500, 1.15, 15.5) — anything above 1.15 V is filtered out.
        let pick = best_fit_row_in_band(20.0, BM1366_BITAXE_PROFILE, 0.0, 700.0, 1000, 1150);
        assert_eq!(pick, Some((500, 1.15, 15.5)));
    }

    #[test]
    fn best_fit_returns_none_when_band_excludes_all_rows() {
        // An impossible band (below every row) yields None → caller holds.
        let pick = best_fit_row_in_band(15.0, BM1366_BITAXE_PROFILE, 0.0, 100.0, 1000, u16::MAX);
        assert_eq!(pick, None);
    }

    // ─── AUTOTUNE-1: per-step settle gate ───────────────────────────────────

    #[test]
    fn descent_settle_gate_holds_until_dwell_elapsed() {
        // Must NOT step before the settle window (e.g. 30 s) elapses.
        assert!(!descent_settle_elapsed_ok(0, 30));
        assert!(!descent_settle_elapsed_ok(29, 30));
        // At/after the dwell it is allowed.
        assert!(descent_settle_elapsed_ok(30, 30));
        assert!(descent_settle_elapsed_ok(45, 30));
    }

    // ─── AUTOTUNE-9: persist-only-on-change gate ────────────────────────────

    #[test]
    fn lkg_first_persist_always_allowed() {
        // No prior record → always write the first time.
        assert!(last_known_good_changed(false, 0.0, 0, 550.0, 1200, 6.25));
    }

    #[test]
    fn lkg_unchanged_point_skips_write() {
        // Identical point (within the freq dead-band, same voltage) → skip.
        assert!(!last_known_good_changed(
            true, 550.0, 1200, 550.0, 1200, 6.25
        ));
        // Sub-dead-band f32 jitter with identical voltage → still skip.
        assert!(!last_known_good_changed(
            true, 550.0, 1200, 552.0, 1200, 6.25
        ));
    }

    #[test]
    fn lkg_real_move_triggers_write() {
        // A voltage change always counts.
        assert!(last_known_good_changed(
            true, 550.0, 1200, 550.0, 1210, 6.25
        ));
        // A freq move >= the dead-band counts.
        assert!(last_known_good_changed(
            true, 550.0, 1200, 560.0, 1200, 6.25
        ));
    }

    // ─── AUTOTUNE-A1: best_point_for_mode (find_best_point extraction) ───────

    /// Rows: (freq, voltage_v, hashrate_ghs, jth, temp_c). Mirrors the engine's
    /// stable-point tuples. Indices: 0..=3.
    fn sample_points() -> Vec<(f32, f32, f64, f32, f32)> {
        vec![
            (400.0, 1.10, 800.0, 18.0, 55.0),  // 0
            (500.0, 1.15, 1000.0, 16.0, 65.0), // 1 — max hashrate, min jth
            (550.0, 1.19, 950.0, 17.5, 72.0),  // 2
            (450.0, 1.12, 900.0, 16.5, 60.0),  // 3
        ]
    }

    #[test]
    fn best_point_max_hashrate_picks_highest_hashrate() {
        let p = sample_points();
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::MaxHashrate, 0.0),
            Some(1)
        );
    }

    #[test]
    fn best_point_best_efficiency_picks_min_jth() {
        let p = sample_points();
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::BestEfficiency, 0.0),
            Some(1)
        );
    }

    #[test]
    fn best_point_target_temp_picks_max_hashrate_under_ceiling() {
        let p = sample_points();
        // Ceiling 66 C admits indices 0 (55), 1 (65), 3 (60). Among those the
        // max hashrate is index 1 (1000 GH/s @ 65 C).
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::TargetTemp, 66.0),
            Some(1)
        );
        // Tighter ceiling 61 C admits 0 (55) + 3 (60); max hashrate is 3 (900).
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::TargetTemp, 61.0),
            Some(3)
        );
    }

    #[test]
    fn best_point_target_temp_below_all_returns_none() {
        // THE A1 BUG STRAND: a temp ceiling (e.g. 0 C) below every profiled
        // point matches NOTHING → None. The engine must treat this as a clean
        // "hold last-known-good / idle", NOT churn forever in Optimizing.
        let p = sample_points();
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::TargetTemp, 0.0),
            None
        );
    }

    #[test]
    fn best_point_empty_input_returns_none() {
        let empty: &[(f32, f32, f64, f32, f32)] = &[];
        assert_eq!(
            best_point_for_mode(empty, BestPointMode::MaxHashrate, 0.0),
            None
        );
        assert_eq!(
            best_point_for_mode(empty, BestPointMode::BestEfficiency, 0.0),
            None
        );
        assert_eq!(
            best_point_for_mode(empty, BestPointMode::TargetTemp, 100.0),
            None
        );
    }

    #[test]
    fn best_point_target_watts_is_engine_owned_returns_none() {
        // The engine owns the TargetWatts pick (power-distance / best-fit row);
        // this pure fn deliberately returns None so it is never silently used.
        let p = sample_points();
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::TargetWatts, 15.0),
            None
        );
    }

    #[test]
    fn best_point_max_hashrate_tie_keeps_last_like_max_by() {
        // Two points tie on hashrate; std `max_by` returns the LAST equal one,
        // which the engine relied on. Index 2 (last) must win.
        let p: Vec<(f32, f32, f64, f32, f32)> = vec![
            (400.0, 1.10, 900.0, 18.0, 55.0),
            (450.0, 1.12, 1000.0, 17.0, 60.0),
            (500.0, 1.15, 1000.0, 16.0, 65.0),
        ];
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::MaxHashrate, 0.0),
            Some(2)
        );
    }

    #[test]
    fn best_point_best_efficiency_tie_keeps_first_like_min_by() {
        // Two points tie on jth; std `min_by` returns the FIRST equal one.
        let p: Vec<(f32, f32, f64, f32, f32)> = vec![
            (450.0, 1.12, 1000.0, 16.0, 60.0),
            (500.0, 1.15, 1000.0, 16.0, 65.0),
        ];
        assert_eq!(
            best_point_for_mode(&p, BestPointMode::BestEfficiency, 0.0),
            Some(0)
        );
    }

    #[test]
    fn best_point_nan_jth_behaves_exactly_like_engine_min_by() {
        // Behavior-preservation pin: this mirrors the engine's `min_by` + `fcmp`
        // EXACTLY (partial_cmp(NaN) → Equal, min_by keeps the FIRST on a tie).
        // A real value placed BEFORE a NaN is therefore kept (the NaN can never
        // beat it): index 0 wins.
        let real_first: Vec<(f32, f32, f64, f32, f32)> = vec![
            (500.0, 1.15, 1000.0, 16.0, 65.0),
            (400.0, 1.10, 900.0, f32::NAN, 55.0),
        ];
        assert_eq!(
            best_point_for_mode(&real_first, BestPointMode::BestEfficiency, 0.0),
            Some(0)
        );
        // The system-level guarantee that a NaN J/TH never reaches this fn is
        // `compute_jth` (A3): it clamps NaN/negative/zero-hashrate inputs to
        // `f32::MAX`, so a real profiled point can never carry a NaN J/TH here.
        assert_eq!(compute_jth(f32::NAN, 1000.0), f32::MAX);
    }

    // ─── AUTOTUNE-A3: compute_jth + divide-by-zero / NaN / negative guard ────

    #[test]
    fn compute_jth_normal_point() {
        // 16 W / 1000 GH/s × 1000 = 16.0 J/TH (the engine's unit convention).
        assert!((compute_jth(16.0, 1000.0) - 16.0).abs() < 1e-3);
    }

    #[test]
    fn compute_jth_zero_hashrate_is_max_not_zero() {
        assert_eq!(compute_jth(16.0, 0.0), f32::MAX);
    }

    #[test]
    fn compute_jth_below_guard_hashrate_is_max() {
        // 0.0005 GH/s is below the 0.001 guard → f32::MAX (never a tiny win).
        assert_eq!(compute_jth(16.0, 0.0005), f32::MAX);
    }

    #[test]
    fn compute_jth_nan_power_is_max_not_nan() {
        // A3 hardening: NaN power must clamp to f32::MAX (was NaN inline).
        assert_eq!(compute_jth(f32::NAN, 1000.0), f32::MAX);
    }

    #[test]
    fn compute_jth_negative_power_is_max_not_negative() {
        // A3 hardening: negative power must clamp to f32::MAX so it can never
        // spuriously WIN BestEfficiency's min-by (was a negative score inline).
        assert_eq!(compute_jth(-5.0, 1000.0), f32::MAX);
    }

    #[test]
    fn compute_jth_infinite_power_is_max() {
        assert_eq!(compute_jth(f32::INFINITY, 1000.0), f32::MAX);
    }

    // ─── AUTOTUNE-A2: silicon_grade boundaries ──────────────────────────────

    #[test]
    fn silicon_grade_gold_just_inside_all_three_bounds() {
        // gold: err <= 0.002 && jth <= 16.5 && V <= 1150.
        assert_eq!(silicon_grade(16.5, 0.002, 1150), "gold");
        assert_eq!(silicon_grade(16.0, 0.0, 1100), "gold");
    }

    #[test]
    fn silicon_grade_just_outside_gold_falls_to_strong() {
        // jth 16.51 (> 16.5) drops gold but is still <= 18.5 & err <= 0.01.
        assert_eq!(silicon_grade(16.51, 0.002, 1150), "strong");
        // err 0.0021 (> 0.002) drops gold but is still <= 0.01.
        assert_eq!(silicon_grade(16.5, 0.0021, 1150), "strong");
        // V 1151 (> 1150) drops gold; jth/err still strong.
        assert_eq!(silicon_grade(16.5, 0.002, 1151), "strong");
    }

    #[test]
    fn silicon_grade_strong_boundary() {
        // strong: err <= 0.01 && jth <= 18.5 (voltage irrelevant here).
        assert_eq!(silicon_grade(18.5, 0.01, 1300), "strong");
    }

    #[test]
    fn silicon_grade_just_outside_strong_falls_to_normal() {
        // jth 18.51 (> 18.5) drops strong; err still <= MAX_ERROR_RATE.
        assert_eq!(silicon_grade(18.51, 0.01, 1150), "normal");
        // err 0.0101 (> 0.01) drops strong; still <= MAX_ERROR_RATE.
        assert_eq!(silicon_grade(16.0, 0.0101, 1100), "normal");
    }

    #[test]
    fn silicon_grade_normal_boundary_and_spicy_fall_through() {
        // normal upper edge: err exactly == MAX_ERROR_RATE (0.02).
        assert_eq!(silicon_grade(30.0, MAX_ERROR_RATE, 1300), "normal");
        // Just above MAX_ERROR_RATE → spicy.
        assert_eq!(silicon_grade(16.0, MAX_ERROR_RATE + 0.0001, 1100), "spicy");
        assert_eq!(silicon_grade(40.0, 0.05, 1400), "spicy");
    }

    // ─── A4: closest-step coverage migrated off the retired test mirror ──────
    //
    // These assert against the REAL `best_fit_row_in_band` (NOT a hand-copied
    // `pick_closest_step` mirror), so a real-table edit can no longer leave a
    // stale standalone copy green. They replace the deleted
    // `dcentaxe/tests/test_wattage_autotune.rs` integration mirror + its CI
    // ban-gate, which could not host-run (the `dcentaxe` test package pulls
    // esp-idf-sys). Genuine coverage preserved: closest-watts selection incl.
    // the lower-bound clamp + the operator-band filter the mirror dropped.

    #[test]
    fn best_fit_lower_bound_clamp_bm1366_10w() {
        // Migrated from `binary_search_converges_at_10w_bm1366`: target 10 W on
        // BM1366 has no row below the 12 W floor, so the closest (lowest) row
        // (400, 1.10, 12.0) is chosen — exercises the lower-bound clamp.
        let (lf, hf, lv, hv) = wide_band();
        assert_eq!(
            best_fit_row_in_band(10.0, BM1366_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((400, 1.10, 12.0))
        );
    }

    #[test]
    fn best_fit_closest_watts_bm1397_max_table() {
        // The retired mirror never covered the AUTOTUNE-3 BM1397 table at all.
        // Target 18 W → (450, 1.17, 17.0) (score 1.0) beats (500, 1.19, 19.5)
        // (score 1.5); exact 14.5 W → (400, 1.15, 14.5).
        let (lf, hf, lv, hv) = wide_band();
        assert_eq!(
            best_fit_row_in_band(18.0, BM1397_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((450, 1.17, 17.0))
        );
        assert_eq!(
            best_fit_row_in_band(14.5, BM1397_BITAXE_PROFILE, lf, hf, lv, hv),
            Some((400, 1.15, 14.5))
        );
    }

    #[test]
    fn best_fit_combined_operator_band_filters_out_exact_match() {
        // The OLD `pick_closest_step` mirror had NO band filter, so it would
        // have returned the exact 20 W row (575, 1.21, 20.0). With the operator
        // band narrowed to [400, 550] MHz AND <= 1.17 V, that row is out of band
        // on BOTH axes, so the closest IN-BAND row (525, 1.17, 17.0) wins —
        // proving the shipped algorithm honours the band the mirror ignored.
        let pick = best_fit_row_in_band(20.0, BM1366_BITAXE_PROFILE, 400.0, 550.0, 1000, 1170);
        assert_eq!(pick, Some((525, 1.17, 17.0)));
    }

    // ─── A5 / XPAUTO-5: validate_autotune_target accept/reject matrix ────────

    #[test]
    fn validate_target_watts_rejects_non_positive() {
        assert!(validate_autotune_target(BestPointMode::TargetWatts, 0.0).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetWatts, -1.0).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetWatts, -0.001).is_err());
    }

    #[test]
    fn validate_target_watts_rejects_nan_inf() {
        assert!(validate_autotune_target(BestPointMode::TargetWatts, f32::NAN).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetWatts, f32::INFINITY).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetWatts, f32::NEG_INFINITY).is_err());
    }

    #[test]
    fn validate_target_watts_accepts_normal_value() {
        assert_eq!(
            validate_autotune_target(BestPointMode::TargetWatts, 15.0),
            Ok(15.0)
        );
        // The board-agnostic ceiling itself is accepted (inclusive bound).
        assert_eq!(
            validate_autotune_target(BestPointMode::TargetWatts, MAX_AUTOTUNE_TARGET_WATTS),
            Ok(MAX_AUTOTUNE_TARGET_WATTS)
        );
    }

    #[test]
    fn validate_target_watts_rejects_over_ceiling() {
        assert!(validate_autotune_target(
            BestPointMode::TargetWatts,
            MAX_AUTOTUNE_TARGET_WATTS + 0.1
        )
        .is_err());
        assert!(validate_autotune_target(BestPointMode::TargetWatts, 500.0).is_err());
    }

    #[test]
    fn validate_target_temp_rejects_zero_and_below_band() {
        // The stranding case: an explicit TargetTemp=0 must be rejected.
        assert!(validate_autotune_target(BestPointMode::TargetTemp, 0.0).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetTemp, 39.0).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetTemp, 39.9).is_err());
    }

    #[test]
    fn validate_target_temp_accepts_in_band() {
        assert_eq!(
            validate_autotune_target(BestPointMode::TargetTemp, 40.0),
            Ok(40.0)
        );
        assert_eq!(
            validate_autotune_target(BestPointMode::TargetTemp, 65.0),
            Ok(65.0)
        );
        assert_eq!(
            validate_autotune_target(BestPointMode::TargetTemp, 95.0),
            Ok(95.0)
        );
    }

    #[test]
    fn validate_target_temp_rejects_above_band() {
        assert!(validate_autotune_target(BestPointMode::TargetTemp, 96.0).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetTemp, 500.0).is_err());
    }

    #[test]
    fn validate_target_temp_rejects_nan_inf() {
        assert!(validate_autotune_target(BestPointMode::TargetTemp, f32::NAN).is_err());
        assert!(validate_autotune_target(BestPointMode::TargetTemp, f32::INFINITY).is_err());
    }

    #[test]
    fn validate_target_ignored_modes_accept_anything() {
        // MaxHashrate / BestEfficiency ignore the target — even 0 / NaN / huge
        // values pass through unchanged (the engine never reads target_value in
        // these modes, so rejecting would be a spurious 400).
        for mode in [BestPointMode::MaxHashrate, BestPointMode::BestEfficiency] {
            assert_eq!(validate_autotune_target(mode, 0.0), Ok(0.0));
            assert_eq!(validate_autotune_target(mode, 999.0), Ok(999.0));
            assert_eq!(validate_autotune_target(mode, -5.0), Ok(-5.0));
            assert!(validate_autotune_target(mode, f32::NAN)
                .map(|v| v.is_nan())
                .unwrap_or(false));
        }
    }
}
