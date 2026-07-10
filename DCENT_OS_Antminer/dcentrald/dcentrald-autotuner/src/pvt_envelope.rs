//! W13.C3 (2026-05-10): per-SKU PVT envelope clamp for the autotuner.
//!
//! Wraps [`dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku`] and its
//! [`freq_voltage_table()`] / [`flags()`] surface in a small set of
//! autotuner-facing helpers:
//!
//! - [`validate_freq_volt`] — returns `Err(AutoTunerError::OutsidePvt)` when
//!   a `(freq_mhz, volt_mv)` tuple is outside the SKU's envelope.
//! - [`nearest_valid_volt`] — clamps a target voltage to the nearest valid
//!   voltage for a given freq within the SKU's table.
//! - [`pvt_envelope`] — returns the SKU's full freq/voltage table (proxy
//!   for the silicon-profiles symbol).
//! - [`envelope_freq_range`] / [`envelope_volt_range`] — internal helpers
//!   that compute the inclusive `(min, max)` bounds carried inside
//!   [`AutoTunerError::OutsidePvt`].
//!
//! Cross-references:
//! - `~/
//! - `~/
//! - `~/
//!
//! Design notes:
//!
//! - The PVT table is a "canonical operating point" view — one row per
//!   frequency tier, picking the lowest stable voltage at that frequency.
//!   For SKUs that publish multiple voltage rows per frequency (e.g.
//!   BHB42601 in the C-side `pvt_tables.h` lists 3 voltage rows at
//!   545 MHz), validation walks the canonical view. A future C4 task can
//!   expose `pvt_levels_full()` for fine-grained search.
//! - Validation is **inclusive** at both ends. The PVT table's lowest and
//!   highest frequencies + voltages are valid. Off-by-one rejection at the
//!   edges would silently exclude operator-published nameplate operating
//!   points.
//! - For the inverted-curve SKU (BHB42841), the same envelope check still
//!   applies — the inversion is a *heuristic* concern, not a bounds
//!   concern. Callers MUST consult [`Bm1362SkuFlags::inverted_curve`] when
//!   walking the table; this module's job is the bounds gate, not the walk
//!   direction.
//! - For the fixed-voltage SKU (BHB42803), `voltage_fixed=true` makes the
//!   voltage axis a single value (1530 mV in W13). Validation accepts only
//!   that exact voltage. The W13.C1 voltage_search short-circuit handles
//!   the dispatch-side "don't bounce SET_VOLTAGE" rule; this module
//!   enforces the bounds-side "don't request 1320 mV when the only valid
//!   value is 1530".
//! - `nearest_valid_volt` looks up the *exact-frequency* voltage row first.
//!   When the requested freq isn't in the table, it picks the table row
//!   whose freq is nearest to the requested one (ties broken in favor of
//!   the lower freq, which is the safer choice for over-current). The
//!   nearest-row's voltage is then returned. This matches the autotuner's
//!   step-down preference and stays inside `[min_volt, max_volt]`.

use dcentrald_silicon_profiles::bm1362::{Bm1362FreqVoltRow, Bm1362HashboardSku};

use crate::AutoTunerError;

/// W13.C3: full per-SKU freq/voltage envelope table.
///
/// Thin proxy over [`Bm1362HashboardSku::freq_voltage_table`] so callers
/// that already hold an `AutoTunerError` import don't need to take a
/// direct silicon-profiles dep. Returned slice is `'static` and ordered
/// top-down (highest freq first).
pub fn pvt_envelope(sku: Bm1362HashboardSku) -> &'static [Bm1362FreqVoltRow] {
    sku.freq_voltage_table()
}

/// Inclusive `(min, max)` MHz bounds derived from a SKU's envelope.
/// Internal — used by [`validate_freq_volt`] and [`AutoTunerError::OutsidePvt`].
fn envelope_freq_range(table: &[Bm1362FreqVoltRow]) -> (u16, u16) {
    debug_assert!(!table.is_empty(), "PVT table must not be empty");
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    for (f, _) in table {
        if *f < min {
            min = *f;
        }
        if *f > max {
            max = *f;
        }
    }
    (min, max)
}

/// Inclusive `(min, max)` mV bounds derived from a SKU's envelope.
/// Internal — used by [`validate_freq_volt`] and [`AutoTunerError::OutsidePvt`].
fn envelope_volt_range(table: &[Bm1362FreqVoltRow]) -> (u16, u16) {
    debug_assert!(!table.is_empty(), "PVT table must not be empty");
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    for (_, v) in table {
        if *v < min {
            min = *v;
        }
        if *v > max {
            max = *v;
        }
    }
    (min, max)
}

/// W13.C3: validate a `(freq_mhz, volt_mv)` tuple against a SKU's PVT
/// envelope.
///
/// Returns `Ok(())` when both `freq_mhz` is inside `[min_freq, max_freq]`
/// AND `volt_mv` is inside `[min_volt, max_volt]` for the SKU's
/// freq/voltage table. Returns `Err(AutoTunerError::OutsidePvt)` with the
/// inclusive ranges populated otherwise.
///
/// **Inclusive** at both ends — the PVT table endpoints are valid
/// nameplate operating points and MUST NOT be rejected.
///
/// For BHB42803 (`voltage_fixed=true`), the voltage envelope collapses to
/// a single value (1530 mV in W13). Validation accepts only that exact
/// voltage; any other voltage is OutsidePvt regardless of how close it is.
///
/// Does NOT validate `(freq, volt)` *combinations* — only the marginal
/// bounds. A request for the BHB42801 envelope's freq=585 + volt=1530 is
/// accepted even though the table row at 585 MHz publishes 1600 mV.
/// Combination validation is a follow-up (W14+) and would require the
/// per-step grid (`pvt_levels_full()`).
pub fn validate_freq_volt(
    sku: Bm1362HashboardSku,
    freq_mhz: u16,
    volt_mv: u16,
) -> Result<(), AutoTunerError> {
    let table = pvt_envelope(sku);
    if table.is_empty() {
        // Defensive: empty table = unrecognised SKU. Refuse.
        return Err(AutoTunerError::OutsidePvt {
            sku: sku.hashboard_id().to_string(),
            freq_mhz,
            volt_mv,
            valid_freq_range: (0, 0),
            valid_volt_range: (0, 0),
        });
    }

    let valid_freq_range = envelope_freq_range(table);
    let valid_volt_range = envelope_volt_range(table);

    let freq_ok = freq_mhz >= valid_freq_range.0 && freq_mhz <= valid_freq_range.1;
    let volt_ok = volt_mv >= valid_volt_range.0 && volt_mv <= valid_volt_range.1;

    if freq_ok && volt_ok {
        Ok(())
    } else {
        Err(AutoTunerError::OutsidePvt {
            sku: sku.hashboard_id().to_string(),
            freq_mhz,
            volt_mv,
            valid_freq_range,
            valid_volt_range,
        })
    }
}

/// R-12: validate a `(freq_mhz, volt_mv)` pair against the SKU's FULL stock
/// per-step PVT grid, enforcing the **per-freq** voltage band.
///
/// [`validate_freq_volt`] only checks the marginal envelope, so it accepts
/// 545 MHz @ 1320 mV even though stock's 545 MHz floor is 1340 mV (1320 mV is
/// only a valid voltage at <= 525 MHz). This function closes that gap: it
/// looks up the exact frequency in [`Bm1362HashboardSku::full_pvt_grid`] and
/// requires `volt_mv` to be within `[min_volt_at_freq, max_volt_at_freq]` of
/// the stock grid at that frequency.
///
/// Frequencies that are not published tiers (e.g. an interpolated ramp step at
/// 535 MHz) have no per-freq row, so they fall back to the marginal
/// [`validate_freq_volt`] — this is a strict tightening (on-tier requests get
/// the correct per-freq floor; off-tier requests are no more permissive than
/// before). SKUs without a captured full grid also fall back to marginal.
///
/// Rollout posture mirrors [`strict_pvt_clamp_enabled`]: this is the correct
/// clamp to enable when per-freq PVT enforcement is turned on; it is additive
/// and does not change the default marginal-clamp behavior.
pub fn validate_freq_volt_combination(
    sku: Bm1362HashboardSku,
    freq_mhz: u16,
    volt_mv: u16,
) -> Result<(), AutoTunerError> {
    let grid = sku.full_pvt_grid();
    // Per-freq band at the exact requested frequency.
    let mut vmin = u16::MAX;
    let mut vmax = u16::MIN;
    let mut found = false;
    for (f, v) in grid {
        if *f == freq_mhz {
            found = true;
            if *v < vmin {
                vmin = *v;
            }
            if *v > vmax {
                vmax = *v;
            }
        }
    }
    if !found {
        // Off-tier frequency (or SKU with no full grid): no per-freq row to
        // enforce — defer to the marginal-envelope check (no regression).
        return validate_freq_volt(sku, freq_mhz, volt_mv);
    }
    if volt_mv >= vmin && volt_mv <= vmax {
        Ok(())
    } else {
        // Report the violated per-freq band (more actionable than the marginal
        // envelope for a per-freq under/over-volt).
        let table = pvt_envelope(sku);
        Err(AutoTunerError::OutsidePvt {
            sku: sku.hashboard_id().to_string(),
            freq_mhz,
            volt_mv,
            valid_freq_range: envelope_freq_range(table),
            valid_volt_range: (vmin, vmax),
        })
    }
}

/// W13.C3: clamp a target voltage to the nearest valid voltage at a given
/// frequency for the SKU's PVT table.
///
/// Lookup order:
/// 1. **Exact-frequency hit**: if the table has a row at `freq_mhz`,
///    return its voltage clamped to `[target_volt - tolerance, target_volt + tolerance]`
///    only as a strict snap (no tolerance — this is a clamp, not a search).
///    Concretely: returns the table row's voltage at the exact freq.
/// 2. **Nearest-frequency**: if no exact-freq row exists, pick the row
///    whose freq is closest to `freq_mhz` (ties broken in favor of the
///    *lower* freq for safer current). Return that row's voltage.
/// 3. Final clamp to `[min_volt, max_volt]` of the SKU envelope so a
///    pathologically-out-of-band `target_volt` can't escape the envelope.
///
/// `target_volt` is consulted only by step 3 (the final clamp). Steps 1
/// and 2 are PVT-table-driven, not target-driven — the clamp's job is to
/// snap to a published voltage, not to honor an arbitrary operator
/// request.
pub fn nearest_valid_volt(sku: Bm1362HashboardSku, freq_mhz: u16, target_volt: u16) -> u16 {
    let table = pvt_envelope(sku);
    if table.is_empty() {
        // Defensive: empty table = unrecognised SKU. Echo the target back
        // unchanged; the caller's validate_freq_volt will reject anyway.
        return target_volt;
    }

    let (_min_volt, _max_volt) = envelope_volt_range(table);

    // Step 1: exact-freq hit.
    if let Some((_, v)) = table.iter().find(|(f, _)| *f == freq_mhz) {
        return *v;
    }

    // Step 2: nearest-freq row, ties broken to lower freq.
    let mut best: Option<(u16, u16, u16)> = None; // (delta, freq, volt)
    for (f, v) in table {
        let delta = (*f).abs_diff(freq_mhz);
        match best {
            None => best = Some((delta, *f, *v)),
            Some((best_delta, best_freq, _)) => {
                let take = delta < best_delta || (delta == best_delta && *f < best_freq);
                if take {
                    best = Some((delta, *f, *v));
                }
            }
        }
    }

    let snapped = best.map(|(_, _, v)| v).unwrap_or(target_volt);

    // Step 3: final envelope clamp. Belt-and-suspenders — `snapped` is
    // already from the table so it's already in-envelope, but if a future
    // SKU adds a row outside its own envelope, this clamp catches it.
    let (min_volt, max_volt) = envelope_volt_range(table);
    snapped.max(min_volt).min(max_volt)
}

// ===================================================================
//  B9a — per-write PVT-clamp helper (pure; no mining-path wiring)
// ===================================================================
//
// B2 (`28b1d00f`) resolves a per-chain `SkuBinding { sku: Hashboard, .. }`
// at energize time but does not yet enforce the PVT envelope at each
// voltage write. B9 closes that. This module provides the PURE decision
// helper + the env gate; the call-site lifetime-threading into the three
// voltage-write paths (s19j_hybrid Phase 3, am3_bb, serial) is B9b (a
// separate, carefully-sequenced change — see
// ).
//
// This helper touches NO voltage-write path and cannot affect any unit;
// it is a fully-unit-tested foundation that B9b will call.

use dcentrald_silicon_profiles::hashboards::Hashboard;

/// Map the light cross-chip [`Hashboard`] catalog entry to its rich
/// [`Bm1362HashboardSku`] (which carries the PVT freq/voltage table).
///
/// Returns `None` for any non-BM1362 board (BHB56902 = BM1366; BhbS9/
/// S11/S17 = BM1387/BM1397) **and** for BM1362 SKUs that have no
/// `Bm1362HashboardSku` PVT table (e.g. the low-power-salvage `Bhb42841`)
/// — callers MUST treat `None` as "no PVT table available → skip the
/// clamp", never as an error. The names are a deliberate 1:1 mirror
/// (see the `Hashboard` enum doc-comment).
pub fn hashboard_to_bm1362_sku(hb: Hashboard) -> Option<Bm1362HashboardSku> {
    Some(match hb {
        Hashboard::Bhb42601 => Bm1362HashboardSku::Bhb42601,
        Hashboard::Bhb42603 => Bm1362HashboardSku::Bhb42603,
        Hashboard::Bhb42621 => Bm1362HashboardSku::Bhb42621,
        Hashboard::Bhb42641 => Bm1362HashboardSku::Bhb42641,
        Hashboard::Bhb42631 => Bm1362HashboardSku::Bhb42631,
        Hashboard::Bhb42632 => Bm1362HashboardSku::Bhb42632,
        Hashboard::Bhb42651 => Bm1362HashboardSku::Bhb42651,
        Hashboard::Bhb42801 => Bm1362HashboardSku::Bhb42801,
        Hashboard::Bhb42811 => Bm1362HashboardSku::Bhb42811,
        Hashboard::Bhb42821 => Bm1362HashboardSku::Bhb42821,
        Hashboard::Bhb42831 => Bm1362HashboardSku::Bhb42831,
        Hashboard::Bhb42803 => Bm1362HashboardSku::Bhb42803,
        Hashboard::Bhb42611 => Bm1362HashboardSku::Bhb42611,
        Hashboard::Bhb42701 => Bm1362HashboardSku::Bhb42701,
        // BM1362 boards without a Bm1362HashboardSku PVT table, and all
        // non-BM1362 boards → no table → skip the clamp.
        Hashboard::Bhb42841
        | Hashboard::Bhb56902
        | Hashboard::BhbS9 { .. }
        | Hashboard::BhbS11
        | Hashboard::BhbS17
        | Hashboard::BhbT15 => return None,
    })
}

/// CE-011 (2026-07-08): resolve a **uniform** BM1362 hashboard SKU across a
/// set of energize-gate [`SkuBinding`](dcentrald_silicon_profiles::energize_gate::SkuBinding)s,
/// or `None` when the set does not resolve to a single PVT-bearing
/// [`Bm1362HashboardSku`].
///
/// Returns `Some(sku)` **only** when `bindings` is non-empty AND every
/// binding's `.sku` maps through [`hashboard_to_bm1362_sku`] to the SAME
/// `Bm1362HashboardSku`. Returns `None` for:
/// - an empty binding set (nothing accepted → register nothing),
/// - any binding whose board is non-BM1362 or has no PVT table
///   (`hashboard_to_bm1362_sku` → `None`, e.g. BHB56902/BM1366 or the
///   table-less salvage `Bhb42841`),
/// - a mixed-SKU set (chains disagree → never guess).
///
/// Fail-closed by construction: an unresolvable set yields `None`, so the
/// caller registers no SKU and the autotuner behaves exactly as it does
/// today (no envelope tightening). This is the registration primitive the
/// am2 freq-only tuner spawn uses to feed a CEILING-ONLY PVT clamp — it
/// never widens anything, and the applied am2 545-MHz ceiling is already
/// `<=` every BM1362 SKU envelope max, so `a lab unit`/`a lab unit` stay byte-identical.
pub fn uniform_bm1362_sku_for_bindings(
    bindings: &[dcentrald_silicon_profiles::energize_gate::SkuBinding],
) -> Option<Bm1362HashboardSku> {
    let mut resolved: Option<Bm1362HashboardSku> = None;
    for binding in bindings {
        // Any non-BM1362 / table-less board short-circuits to None: we
        // only ever tighten to a homogeneous, PVT-table-bearing set.
        let sku = hashboard_to_bm1362_sku(binding.sku)?;
        match resolved {
            None => resolved = Some(sku),
            Some(existing) if existing == sku => {}
            // Mixed SKUs across chains — don't guess a ceiling.
            Some(_) => return None,
        }
    }
    resolved
}

/// Outcome of a per-write PVT-clamp check for one chain's `(freq, volt)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PvtClampOutcome {
    /// The tuple is inside the SKU's PVT envelope — proceed with the write.
    InBand,
    /// The tuple is outside the envelope. `valid_freq_range` /
    /// `valid_volt_range` are the inclusive bounds (for the operator log).
    /// B9b decides whether to refuse (strict) or `nearest_valid_volt`-clamp
    /// (lenient) based on the env gate.
    OutOfBand {
        sku: &'static str,
        freq_mhz: u16,
        volt_mv: u16,
        valid_freq_range: (u16, u16),
        valid_volt_range: (u16, u16),
    },
    /// No PVT table for this board (non-BM1362, or a BM1362 SKU without a
    /// table) — the clamp does not apply; proceed unchanged.
    Skipped,
}

/// Pure PVT-clamp decision for one chain. Maps the [`Hashboard`] to its
/// PVT table and validates the `(freq_mhz, volt_mv)` tuple. Never panics,
/// never performs I/O, touches no voltage-write path.
///
/// **CRITICAL — VOLTAGE SCALE (load-bearing for B9b):** `volt_mv` here is
/// the BM1362 **per-domain core voltage** (≈1320–1380 mV for BHB42601, i.e.
/// ~1.32–1.38 V), the axis the PVT table publishes — it is **NOT** the
/// ~13.7 V **chain-rail** voltage that `cold_boot_init(target_mv=13700)`
/// programs. These are two different voltages at two different scales.
/// B9b MUST pass the autotuner's per-domain core voltage, NOT the
/// chain-rail target — passing 13700 here would be out-of-band against a
/// ~1340 mV envelope and would false-refuse EVERY healthy chain. (This bug
/// was caught by `b9_pvt_clamp_accepts_109_baseline` during B9a — the
/// reason B9 ships the tested pure helper before any call-site wiring.)
///
/// This is the B9 decision primitive. B9b calls it from each voltage-write
/// site, gated by [`strict_pvt_clamp_enabled`] (default OFF →
/// telemetry-only log; the write proceeds regardless until an operator
/// promotes the gate after live `a lab unit` validation).
pub fn pvt_clamp_check(hb: Hashboard, freq_mhz: u16, volt_mv: u16) -> PvtClampOutcome {
    let Some(sku) = hashboard_to_bm1362_sku(hb) else {
        return PvtClampOutcome::Skipped;
    };
    match validate_freq_volt(sku, freq_mhz, volt_mv) {
        Ok(()) => PvtClampOutcome::InBand,
        Err(AutoTunerError::OutsidePvt {
            sku,
            freq_mhz,
            volt_mv,
            valid_freq_range,
            valid_volt_range,
        }) => PvtClampOutcome::OutOfBand {
            // `sku` here is the owned hashboard-id String from the error;
            // leak-free conversion to a 'static str isn't needed — keep the
            // owned form by re-borrowing the SKU's canonical id.
            sku: sku_static_id(&sku),
            freq_mhz,
            volt_mv,
            valid_freq_range,
            valid_volt_range,
        },
        // validate_freq_volt only ever returns OutsidePvt; any other
        // AutoTunerError variant would be a contract change — treat
        // defensively as out-of-band with unknown bounds rather than panic.
        Err(_) => PvtClampOutcome::OutOfBand {
            sku: "unknown",
            freq_mhz,
            volt_mv,
            valid_freq_range: (0, 0),
            valid_volt_range: (0, 0),
        },
    }
}

/// Best-effort map of an owned hashboard-id string back to a canonical
/// `'static` id for structured logging. Falls back to a generic literal.
fn sku_static_id(id: &str) -> &'static str {
    match id {
        "BHB42601" => "BHB42601",
        "BHB42603" => "BHB42603",
        "BHB42621" => "BHB42621",
        "BHB42641" => "BHB42641",
        "BHB42631" => "BHB42631",
        "BHB42632" => "BHB42632",
        "BHB42651" => "BHB42651",
        "BHB42801" => "BHB42801",
        "BHB42811" => "BHB42811",
        "BHB42821" => "BHB42821",
        "BHB42831" => "BHB42831",
        "BHB42803" => "BHB42803",
        "BHB42611" => "BHB42611",
        "BHB42701" => "BHB42701",
        _ => "bm1362",
    }
}

/// B9 env gate (default OFF) — when `true`, B9b's voltage-write sites
/// REFUSE an `OutOfBand` tuple; when `false` (default), they log
/// `[PVT-CLAMP-REFUSED ...]` telemetry-only and proceed (byte-identical
/// to pre-B9 behavior). Mirrors the B2 `DCENT_AM2_STRICT_SKU_REFUSE`
/// rollout discipline: prove the decision on live `a lab unit` before promoting.
pub fn strict_pvt_clamp_enabled() -> bool {
    std::env::var("DCENT_AM2_STRICT_PVT_CLAMP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// W24-EFF-1 axis-aware gate (default OFF).
///
/// The W13.C3 setpoint clamp in `tuner.rs` runs the PVT envelope check on
/// `raw_mv = SiliconPreset.voltage_v * 1000`. The BM1362 silicon-profile
/// registry legitimately carries rows on **two distinct voltage axes**
/// (registry.rs `chip_voltage_ranges`: chip-rail `0.5..=2.0 V`,
/// chain-rail `7.5..=15.0 V`). The PVT envelope (`freq_voltage_table`)
/// models only the **core / chip-rail mV** axis (~1320–1380 mV). When a
/// preset carries a **chain-rail** voltage (the baked `BM1362_PROFILES`
/// reality, 11.88–14.4 V), `raw_mv` is ~11880–14400 — the wrong axis —
/// so `validate_freq_volt` always reports `OutsidePvt`, emits a scary
/// per-chain "OUTSIDE PVT envelope" WARN on a perfectly healthy chain,
/// and the green W13.C3 fixture tests (hand-crafted 1.34–1.7 V core
/// values the real registry never produces) do not cover the real path.
///
/// When this gate is `true`, `tuner.rs` only runs the PVT *voltage* check
/// on the axis the envelope actually models (core-mV presets,
/// `voltage_v < 5.0`); for chain-rail presets it still clamps frequency
/// to the envelope but skips the voltage-axis check and logs a single
/// INFO instead of the misleading OutsidePvt WARN. **This changes NO
/// voltage delivery**: chain-rail `dispatch_mv` is already suppressed by
/// `derive_silicon_profile_target`'s `< 5.0 V => None` rule and the am2
/// hybrid FreqCommand consumer hard-refuses `SetVoltage` regardless. The
/// gate exists so the log-correctness change can be proven against the
/// live `a lab unit` fw=0x89 / `a lab unit` capture before becoming the default —
/// mirroring `strict_pvt_clamp_enabled`'s rollout discipline.
///
/// Default OFF ⇒ the wired W13.C3 path is byte-identical to pre-W24
/// behavior (still takes the OutsidePvt branch, still freq-snaps, still
/// suppresses voltage, still emits the WARN).
pub fn axis_aware_pvt_clamp_enabled() -> bool {
    std::env::var("DCENT_AM2_AXIS_AWARE_PVT_CLAMP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Voltage-axis classification for a `SiliconPreset.voltage_v * 1000`
/// value, used by the W24-EFF-1 axis-aware gate. Threshold mirrors
/// `tuner.rs` (`voltage_v < 5.0 ⇒ chip/core rail`) and the registry's
/// dual-envelope split (`registry.rs::chip_voltage_ranges`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageAxis {
    /// Per-chip core voltage (~0.5–2.0 V → 500–2000 mV). This is the
    /// axis the BM1362 `freq_voltage_table` PVT envelope models.
    CoreRail,
    /// Multi-volt chain rail set by the APW PSU + dsPIC (~7.5–15.0 V →
    /// 7500–15000 mV). NOT modeled by the core-mV PVT envelope.
    ChainRail,
}

/// Classify `raw_mv` (= `SiliconPreset.voltage_v * 1000`, already rounded
/// and saturated to `u16`) onto the core-rail vs chain-rail axis. The
/// 5000 mV (5.0 V) split matches `derive_silicon_profile_target`'s
/// `voltage_v < 5.0` test and the registry dual envelope.
pub fn classify_voltage_axis(raw_mv: u16) -> VoltageAxis {
    if raw_mv < 5_000 {
        VoltageAxis::CoreRail
    } else {
        VoltageAxis::ChainRail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_valid_volt_never_escapes_the_sku_envelope() {
        use dcentrald_silicon_profiles::bm1362::ALL_BM1362_HASHBOARD_SKUS;
        // Safety property (priority 1: voltage safety). For EVERY recognised SKU,
        // nearest_valid_volt must clamp ANY (freq, target_volt) request — including
        // out-of-range garbage like 0 / u16::MAX — to a voltage WITHIN that SKU's
        // envelope [min,max] mV. This is the per-SKU analogue of the dsPIC hard cap:
        // the autotuner can never command a voltage outside the silicon's safe
        // envelope through this snap, whatever telemetry or config drives the target.
        for &sku in ALL_BM1362_HASHBOARD_SKUS {
            let table = pvt_envelope(sku);
            if table.is_empty() {
                continue; // unrecognised SKU echoes target back; validate rejects it
            }
            let (min_v, max_v) = envelope_volt_range(table);
            for freq in (0u16..=1200).step_by(5) {
                for &tv in &[0u16, 1, 500, 1000, 1200, 1400, 1500, 1600, 2000, u16::MAX] {
                    let v = nearest_valid_volt(sku, freq, tv);
                    assert!(
                        (min_v..=max_v).contains(&v),
                        "sku={sku:?} freq={freq} target={tv} -> {v} mV outside envelope [{min_v},{max_v}]"
                    );
                }
            }
        }
    }
    use dcentrald_silicon_profiles::bm1362::{Bm1362HashboardSku, ALL_BM1362_HASHBOARD_SKUS};
    use dcentrald_silicon_profiles::hashboards::Hashboard;

    // ---- R-12: per-freq combination validator (full stock grid) -----

    #[test]
    fn combination_rejects_545_at_1320_below_stock_floor() {
        // The marginal validate_freq_volt ACCEPTS 545 MHz @ 1320 mV (1320 is
        // in the [1320,1380] marginal envelope); the per-freq combination
        // validator must REJECT it because stock's 545 MHz floor is 1340 mV.
        assert!(validate_freq_volt(Bm1362HashboardSku::Bhb42601, 545, 1320).is_ok());
        assert!(matches!(
            validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 545, 1320),
            Err(AutoTunerError::OutsidePvt { .. })
        ));
    }

    #[test]
    fn combination_accepts_stock_floor_and_valid_lowfreq_1320() {
        // 545 @ 1340 is stock's real 545 floor → OK.
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 545, 1340).is_ok());
        // 1320 mV IS a real stock operating point at <= 525 MHz → must NOT be
        // forbidden (the exact regression a naive envelope-min bump would cause).
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 525, 1320).is_ok());
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 465, 1320).is_ok());
        // Upper bound still enforced at each freq.
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 545, 1400).is_err());
    }

    #[test]
    fn combination_offtier_freq_falls_back_to_marginal() {
        // 535 MHz is not a published tier → marginal fallback accepts an
        // in-envelope voltage (no regression, no invented per-freq floor).
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 535, 1320).is_ok());
        // ...but a truly out-of-envelope voltage is still rejected via marginal.
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42601, 535, 1500).is_err());
    }

    #[test]
    fn combination_extended_low_440_tier_enforced() {
        // BHB42631 has a 440 MHz tier with stock band [1320,1340].
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42631, 440, 1320).is_ok());
        assert!(validate_freq_volt_combination(Bm1362HashboardSku::Bhb42631, 545, 1320).is_err());
    }

    // ----  B9a: per-write PVT-clamp helper tests --------------

    #[test]
    fn b9_pvt_clamp_accepts_109_baseline() {
        // The `a lab unit` operator-confirmed mining point: BHB42601 @ 525 MHz.
        // NOTE THE SCALE: the PVT-table voltage axis is the BM1362
        // PER-DOMAIN CORE voltage (~1320-1380 mV ≈ 1.34 V), NOT the
        // ~13.4 V CHAIN RAIL. So the in-band tuple is (525 MHz, ~1340 mV)
        // — passing the 13400 mV chain rail here would be (correctly)
        // out-of-band. Load-bearing regression pin: B9b must never
        // false-refuse the proven mining unit, AND must pass per-domain
        // core mV (not the chain-rail target). See pvt_clamp_check's
        // "VOLTAGE SCALE" doc-comment.
        let outcome = pvt_clamp_check(Hashboard::Bhb42601, 525, 1_340);
        assert_eq!(
            outcome,
            PvtClampOutcome::InBand,
            ".109 baseline (BHB42601 @ 525 MHz / 1.34 V core) must be in-band"
        );
    }

    #[test]
    fn b9_pvt_clamp_chain_rail_voltage_is_out_of_band_by_construction() {
        // Guard against the B9b unit-confusion bug: feeding the ~13.7 V
        // CHAIN RAIL (13700 mV) where a ~1340 mV CORE voltage is expected
        // MUST be flagged out-of-band — so a mis-wired B9b that passed the
        // chain-rail target would be caught (in strict mode) rather than
        // silently writing a wrong voltage. This documents the scale trap.
        match pvt_clamp_check(Hashboard::Bhb42601, 525, 13_700) {
            PvtClampOutcome::OutOfBand {
                valid_volt_range, ..
            } => {
                assert_eq!(valid_volt_range, (1320, 1380));
            }
            other => panic!("chain-rail mV must be OutOfBand vs core-mV envelope, got {other:?}"),
        }
    }

    #[test]
    fn b9_pvt_clamp_refuses_above_band() {
        // 900 MHz is far above any BHB42601 nameplate row → out of band.
        match pvt_clamp_check(Hashboard::Bhb42601, 900, 1_340) {
            PvtClampOutcome::OutOfBand { freq_mhz, .. } => assert_eq!(freq_mhz, 900),
            other => panic!("expected OutOfBand for 900 MHz, got {other:?}"),
        }
    }

    #[test]
    fn b9_pvt_clamp_refuses_below_band() {
        // 100 MHz / 1.0 V core is below the BHB42601 envelope → out of band.
        match pvt_clamp_check(Hashboard::Bhb42601, 100, 1_000) {
            PvtClampOutcome::OutOfBand { .. } => {}
            other => panic!("expected OutOfBand for 100 MHz / 1.0 V core, got {other:?}"),
        }
    }

    #[test]
    fn b9_pvt_clamp_skips_non_bm1362_sku() {
        // BHB56902 is BM1366 (S19k Pro) — no BM1362 PVT table → Skipped,
        // never an error. Same for the S9/S11/S17 placeholders.
        assert_eq!(
            pvt_clamp_check(Hashboard::Bhb56902, 525, 13_400),
            PvtClampOutcome::Skipped
        );
        assert_eq!(
            pvt_clamp_check(Hashboard::BhbS11, 500, 9_000),
            PvtClampOutcome::Skipped
        );
        assert_eq!(
            pvt_clamp_check(Hashboard::BhbS9 { chain_index: 0 }, 500, 9_000),
            PvtClampOutcome::Skipped
        );
    }

    #[test]
    fn b9_pvt_clamp_skips_bm1362_board_without_table() {
        // BHB42841 (low-power salvage) is BM1362 but has no
        // Bm1362HashboardSku PVT table → Skipped, not an error.
        assert_eq!(hashboard_to_bm1362_sku(Hashboard::Bhb42841), None);
        assert_eq!(
            pvt_clamp_check(Hashboard::Bhb42841, 525, 13_400),
            PvtClampOutcome::Skipped
        );
    }

    #[test]
    fn b9_hashboard_to_sku_maps_known_bm1362_boards() {
        // Every BM1362 board with a table maps to a real Bm1362HashboardSku
        // whose PVT envelope is non-empty (so the clamp can actually run).
        for hb in [
            Hashboard::Bhb42601,
            Hashboard::Bhb42801,
            Hashboard::Bhb42611,
            Hashboard::Bhb42701,
            Hashboard::Bhb42803,
        ] {
            let sku = hashboard_to_bm1362_sku(hb)
                .unwrap_or_else(|| panic!("{hb:?} should map to a Bm1362HashboardSku"));
            assert!(
                !pvt_envelope(sku).is_empty(),
                "{hb:?} → {sku:?} must have a non-empty PVT table"
            );
        }
    }

    // ---- W24-EFF-1: axis-aware PVT clamp + scale verdict pins ------

    #[test]
    fn w24_eff1_chain_rail_preset_mv_is_outofband_vs_core_envelope() {
        // VERDICT REPRO: the BM1362 baked-profile efficiency headline
        // (Step-9 = 320 MHz @ 12.45 V) feeds raw_mv = 12450 to the
        // core-mV PVT envelope (1320..1380). This is the wrong axis →
        // always OutOfBand → voltage suppressed. Pins the contested
        // W24-EFF-1 mechanism with the EXACT headline value.
        match pvt_clamp_check(Hashboard::Bhb42601, 320, 12_450) {
            PvtClampOutcome::OutOfBand {
                valid_volt_range, ..
            } => {
                assert_eq!(valid_volt_range, (1320, 1380));
            }
            other => {
                panic!("chain-rail 12450 mV must be OutOfBand vs core envelope, got {other:?}")
            }
        }
        // The TOP of the baked table (Step+4 = 645 MHz @ 14.4 V) too.
        assert!(matches!(
            pvt_clamp_check(Hashboard::Bhb42601, 545, 14_400),
            PvtClampOutcome::OutOfBand { .. }
        ));
    }

    #[test]
    fn w24_eff1_classify_voltage_axis_splits_at_5v() {
        // Core-rail: BM1362 levels.json values (1320..1380 mV) and the
        // .109 baseline 1340 mV are CoreRail.
        assert_eq!(classify_voltage_axis(1_320), VoltageAxis::CoreRail);
        assert_eq!(classify_voltage_axis(1_340), VoltageAxis::CoreRail);
        assert_eq!(classify_voltage_axis(1_600), VoltageAxis::CoreRail);
        assert_eq!(classify_voltage_axis(4_999), VoltageAxis::CoreRail);
        // Chain-rail: the baked BM1362_PROFILES PSU rail (11.88..14.4 V).
        assert_eq!(classify_voltage_axis(5_000), VoltageAxis::ChainRail);
        assert_eq!(classify_voltage_axis(11_880), VoltageAxis::ChainRail);
        assert_eq!(classify_voltage_axis(12_450), VoltageAxis::ChainRail);
        assert_eq!(classify_voltage_axis(13_800), VoltageAxis::ChainRail);
        assert_eq!(classify_voltage_axis(14_400), VoltageAxis::ChainRail);
    }

    #[test]
    fn w24_eff1_axis_aware_gate_defaults_off() {
        // Default OFF ⇒ tuner.rs keeps its byte-identical OutsidePvt
        // branch. Only assert the unset case to avoid mutating global
        // env in parallel tests.
        std::env::remove_var("DCENT_AM2_AXIS_AWARE_PVT_CLAMP");
        assert!(
            !axis_aware_pvt_clamp_enabled(),
            "axis-aware gate must default OFF when unset (byte-identical wired behavior)"
        );
    }

    #[test]
    fn b9_strict_gate_defaults_off() {
        // The env gate must default OFF when unset (telemetry-only first
        // deploy; byte-identical to pre-B9 behavior). We only assert the
        // unset/!=1 cases to avoid mutating global env in parallel tests.
        // (A set-to-1 case is covered by the env_registry integration.)
        std::env::remove_var("DCENT_AM2_STRICT_PVT_CLAMP");
        assert!(
            !strict_pvt_clamp_enabled(),
            "gate must default OFF when unset"
        );
    }

    // ---------------------------------------------------------------
    // validate_freq_volt
    // ---------------------------------------------------------------

    #[test]
    fn validate_freq_volt_inside_envelope_passes() {
        // BHB42601 envelope: 465-545 MHz @ 1320-1380 mV.
        // 545 MHz @ 1380 mV is at the upper-freq, upper-volt corner —
        // both are valid endpoints of the envelope.
        assert!(validate_freq_volt(Bm1362HashboardSku::Bhb42601, 545, 1380).is_ok());
        assert!(validate_freq_volt(Bm1362HashboardSku::Bhb42601, 465, 1320).is_ok());
        assert!(validate_freq_volt(Bm1362HashboardSku::Bhb42601, 505, 1345).is_ok());
    }

    #[test]
    fn validate_freq_volt_outside_envelope_returns_outside_pvt() {
        // BHB42601 envelope: 465-545 MHz @ 1320-1380 mV.
        // Wildly out-of-band tuple.
        let err = validate_freq_volt(Bm1362HashboardSku::Bhb42601, 700, 1700)
            .expect_err("700 MHz @ 1700 mV must be OutsidePvt for BHB42601");
        match err {
            AutoTunerError::OutsidePvt {
                sku,
                freq_mhz,
                volt_mv,
                valid_freq_range,
                valid_volt_range,
            } => {
                assert_eq!(sku, "BHB42601");
                assert_eq!(freq_mhz, 700);
                assert_eq!(volt_mv, 1700);
                assert_eq!(valid_freq_range, (465, 545));
                assert_eq!(valid_volt_range, (1320, 1380));
            }
            other => panic!("expected OutsidePvt, got {:?}", other),
        }
    }

    #[test]
    fn validate_freq_volt_freq_in_volt_out_returns_outside_pvt() {
        // freq is in-envelope, volt is not — must still reject.
        let err = validate_freq_volt(Bm1362HashboardSku::Bhb42601, 505, 1500)
            .expect_err("505 MHz @ 1500 mV must reject (volt above envelope)");
        assert!(matches!(err, AutoTunerError::OutsidePvt { .. }));
    }

    #[test]
    fn validate_freq_volt_freq_out_volt_in_returns_outside_pvt() {
        // volt is in-envelope, freq is not — must still reject.
        let err = validate_freq_volt(Bm1362HashboardSku::Bhb42601, 600, 1340)
            .expect_err("600 MHz @ 1340 mV must reject (freq above envelope)");
        assert!(matches!(err, AutoTunerError::OutsidePvt { .. }));
    }

    #[test]
    fn validate_freq_volt_endpoints_are_inclusive() {
        // Both endpoints of the freq AND volt range must be valid. An
        // off-by-one rejection here would silently exclude every
        // nameplate operating point.
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            let table = pvt_envelope(*sku);
            let (min_freq, max_freq) = envelope_freq_range(table);
            let (min_volt, max_volt) = envelope_volt_range(table);
            assert!(
                validate_freq_volt(*sku, min_freq, min_volt).is_ok(),
                "{}: ({}, {}) must be in-envelope",
                sku.hashboard_id(),
                min_freq,
                min_volt
            );
            assert!(
                validate_freq_volt(*sku, max_freq, max_volt).is_ok(),
                "{}: ({}, {}) must be in-envelope",
                sku.hashboard_id(),
                max_freq,
                max_volt
            );
        }
    }

    #[test]
    fn outside_pvt_error_carries_sku_and_envelope_info() {
        // Spot-check a high-bin SKU: the error message must name the
        // SKU + the envelope ranges so a dashboard renderer can show
        // "BHB42801 envelope: 585-675 MHz @ 1530-1600 mV; you asked
        // for 700 MHz @ 1700 mV".
        let err = validate_freq_volt(Bm1362HashboardSku::Bhb42801, 700, 1700)
            .expect_err("700 MHz @ 1700 mV must be OutsidePvt for BHB42801");
        let msg = format!("{}", err);
        assert!(msg.contains("BHB42801"), "msg should name SKU: {}", msg);
        assert!(msg.contains("700"), "msg should name freq: {}", msg);
        assert!(msg.contains("1700"), "msg should name volt: {}", msg);
        assert!(msg.contains("585"), "msg should show min freq: {}", msg);
        assert!(msg.contains("675"), "msg should show max freq: {}", msg);
        assert!(msg.contains("1530"), "msg should show min volt: {}", msg);
        assert!(msg.contains("1600"), "msg should show max volt: {}", msg);
    }

    #[test]
    fn validate_freq_volt_bhb42803_voltage_fixed_only_accepts_1530() {
        // BHB42803 has ALL rows at 1530 mV — the volt range collapses
        // to (1530, 1530). Any other voltage MUST reject.
        let sku = Bm1362HashboardSku::Bhb42803;
        assert!(validate_freq_volt(sku, 615, 1530).is_ok());
        assert!(validate_freq_volt(sku, 615, 1320).is_err());
        assert!(validate_freq_volt(sku, 615, 1540).is_err());
        // Pin: the volt range really is the single fixed value.
        let table = pvt_envelope(sku);
        assert_eq!(envelope_volt_range(table), (1530, 1530));
    }

    // ---------------------------------------------------------------
    // pvt_envelope
    // ---------------------------------------------------------------

    #[test]
    fn pvt_envelope_returns_correct_table_for_15_skus() {
        // For every SKU, `pvt_envelope(sku)` must equal the silicon
        // profile's `freq_voltage_table()` byte-for-byte. Pin the table
        // length too so a future PVT shrink doesn't silently shorten the
        // envelope and cause spurious OutsidePvt rejections.
        let expected_len: &[(Bm1362HashboardSku, usize)] = &[
            (Bm1362HashboardSku::Bhb42601, 5),
            (Bm1362HashboardSku::Bhb42603, 5),
            (Bm1362HashboardSku::Bhb42621, 5),
            (Bm1362HashboardSku::Bhb42641, 5),
            (Bm1362HashboardSku::Bhb42631, 6),
            (Bm1362HashboardSku::Bhb42632, 6),
            (Bm1362HashboardSku::Bhb42651, 6),
            (Bm1362HashboardSku::Bhb42801, 4),
            (Bm1362HashboardSku::Bhb42811, 3),
            (Bm1362HashboardSku::Bhb42821, 3),
            (Bm1362HashboardSku::Bhb42831, 4),
            (Bm1362HashboardSku::Bhb42803, 4),
            (Bm1362HashboardSku::Bhb42611, 4),
            (Bm1362HashboardSku::Bhb42701, 4),
            (Bm1362HashboardSku::Bhb42841, 4),
        ];
        assert_eq!(
            expected_len.len(),
            ALL_BM1362_HASHBOARD_SKUS.len(),
            "test must cover every SKU in ALL_BM1362_HASHBOARD_SKUS"
        );
        for (sku, len) in expected_len {
            let table = pvt_envelope(*sku);
            assert_eq!(
                table,
                sku.freq_voltage_table(),
                "table proxy mismatch for {}",
                sku.hashboard_id()
            );
            assert_eq!(
                table.len(),
                *len,
                "{}: envelope length {} expected {}",
                sku.hashboard_id(),
                table.len(),
                len
            );
        }
    }

    // ---------------------------------------------------------------
    // nearest_valid_volt
    // ---------------------------------------------------------------

    #[test]
    fn nearest_valid_volt_clamps_to_envelope() {
        // BHB42601 envelope: 465-545 MHz @ 1320-1380 mV.
        // Exact-freq hit: 505 MHz row publishes 1345 mV. target_volt is
        // ignored when the freq is in the table.
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 505, 9999),
            1345
        );
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 505, 1),
            1345
        );

        // No exact-freq hit: 510 MHz isn't in the table. Nearest is 505
        // (delta=5) or 525 (delta=20) — picks 505.
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 510, 9999),
            1345
        );

        // Tie-break to lower freq: 515 MHz is exactly midway between
        // 505 and 525 (delta=10). Tie goes to lower freq → 505 → 1345 mV.
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 515, 9999),
            1345
        );

        // Above envelope freq: 999 MHz → nearest is 545 → 1320 mV.
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 999, 9999),
            1320
        );

        // Below envelope freq: 100 MHz → nearest is 465 → 1380 mV.
        assert_eq!(
            nearest_valid_volt(Bm1362HashboardSku::Bhb42601, 100, 0),
            1380
        );
    }

    #[test]
    fn nearest_valid_volt_for_voltage_fixed_sku_returns_fixed_value() {
        // BHB42803 is voltage_fixed=true; every row is 1530 mV. The
        // nearest-volt clamp MUST return 1530 regardless of target_volt
        // or the requested freq.
        let sku = Bm1362HashboardSku::Bhb42803;
        for &freq in &[100u16, 585, 615, 645, 675, 9999] {
            for &target in &[0u16, 500, 1320, 1530, 1700, 9999] {
                assert_eq!(
                    nearest_valid_volt(sku, freq, target),
                    1530,
                    "BHB42803 must always snap to 1530 mV (freq={}, target={})",
                    freq,
                    target
                );
            }
        }
    }

    #[test]
    fn nearest_valid_volt_for_inverted_curve_sku_uses_table() {
        // BHB42841 is inverted_curve=true (lower freq → HIGHER volt for
        // stability margin). The clamp logic doesn't care about
        // direction — it just snaps to the table's published voltage at
        // the requested freq. The autotuner's WALK direction is the
        // caller's responsibility (see Bm1362SkuFlags::inverted_curve).
        //
        // BHB42841 table: every row at 1360 mV. Verify each freq snaps
        // to 1360.
        let sku = Bm1362HashboardSku::Bhb42841;
        for (freq, expected) in [(475u16, 1360u16), (450, 1360), (430, 1360), (410, 1360)] {
            assert_eq!(
                nearest_valid_volt(sku, freq, 9999),
                expected,
                "BHB42841 freq={} must snap to {} mV",
                freq,
                expected
            );
        }
    }

    // ---------------------------------------------------------------
    // Cross-cutting checks
    // ---------------------------------------------------------------

    #[test]
    fn pvt_envelope_proxy_matches_silicon_profiles_for_all_skus() {
        // Belt-and-suspenders: the proxy must NEVER drift from the
        // silicon-profiles surface. If a future PVT table edit changes
        // the canonical view, this test fails first and the operator
        // gets to consciously update both sides.
        for sku in ALL_BM1362_HASHBOARD_SKUS {
            assert_eq!(
                pvt_envelope(*sku),
                sku.freq_voltage_table(),
                "envelope proxy drift for {}",
                sku.hashboard_id()
            );
        }
    }

    // ---------------------------------------------------------------
    // CE-011: uniform_bm1362_sku_for_bindings
    // ---------------------------------------------------------------

    fn ce011_binding(
        chain_id: u8,
        hb: Hashboard,
    ) -> dcentrald_silicon_profiles::energize_gate::SkuBinding {
        dcentrald_silicon_profiles::energize_gate::SkuBinding {
            chain_id,
            preamble: [0x04, 0x11],
            sku: hb,
        }
    }

    #[test]
    fn ce011_uniform_bm1362_sku_three_same_boards_resolves_some() {
        // Non-empty AND every binding resolves to the SAME SKU => Some.
        let bindings = [
            ce011_binding(0, Hashboard::Bhb42601),
            ce011_binding(1, Hashboard::Bhb42601),
            ce011_binding(2, Hashboard::Bhb42601),
        ];
        assert_eq!(
            uniform_bm1362_sku_for_bindings(&bindings),
            Some(Bm1362HashboardSku::Bhb42601)
        );
    }

    #[test]
    fn ce011_uniform_bm1362_sku_mixed_boards_is_none() {
        // Two different BM1362 SKUs across chains => never guess a ceiling.
        let bindings = [
            ce011_binding(0, Hashboard::Bhb42601),
            ce011_binding(1, Hashboard::Bhb42801),
        ];
        assert_eq!(uniform_bm1362_sku_for_bindings(&bindings), None);
    }

    #[test]
    fn ce011_uniform_bm1362_sku_empty_is_none() {
        // Empty set => nothing accepted => register nothing.
        let bindings: [dcentrald_silicon_profiles::energize_gate::SkuBinding; 0] = [];
        assert_eq!(uniform_bm1362_sku_for_bindings(&bindings), None);
    }

    #[test]
    fn ce011_uniform_bm1362_sku_bm1366_board_is_none() {
        // BHB56902 is BM1366 => hashboard_to_bm1362_sku None => skip.
        let bindings = [ce011_binding(0, Hashboard::Bhb56902)];
        assert_eq!(uniform_bm1362_sku_for_bindings(&bindings), None);
    }

    #[test]
    fn ce011_uniform_bm1362_sku_table_less_bm1362_board_is_none() {
        // BHB42841 is BM1362 but has no PVT table => None => skip (never guess).
        let salvage_only = [ce011_binding(0, Hashboard::Bhb42841)];
        assert_eq!(uniform_bm1362_sku_for_bindings(&salvage_only), None);
        // A table-bearing board mixed with the table-less salvage board also
        // yields None (the table-less binding short-circuits the whole set).
        let mixed = [
            ce011_binding(0, Hashboard::Bhb42601),
            ce011_binding(1, Hashboard::Bhb42841),
        ];
        assert_eq!(uniform_bm1362_sku_for_bindings(&mixed), None);
    }
}
