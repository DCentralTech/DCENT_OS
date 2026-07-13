//! A44 — Pure no-HAL chip-health analysis math (Laplacian gradient,
//! cross-slot z-score, nonce-deficit). HAL-free, hardware-I/O-free.
//!
//! Source of record (Knowledge Goldmine 2026-06-10, lane s16b "Lane B:
//! WhatsMiner Chip Map Analysis Algorithms"):
//!
//! facts **B05** (gradient), **B06** (z-score), **B07** (nonce-deficit),
//! **B08** (`ChipAnalysis` output struct). Aggregated as candidate **A44**
//! in `IMPLEMENTATION_CANDIDATES.md` (= s16b-IC-B1 + s16b-IC-B3 + s17-WM-C1).
//!
//! Clean-room port from the open-source WhatsMiner chip-map tool RE corpus:
//!
//! (`compute_hot_gradient` L189-200, `compute_mean_std` L203-221,
//! `compute_hot_zscore` L225-241, `compute_slot_avg_nonce` L244-250,
//! `compute_nonce_deficit` L254-269). The math is byte-faithful to that
//! source so the port stays verifiable against the original.
//!
//! This crate is intentionally independent from HAL, async runtimes,
//! sockets, the filesystem, and any miner hardware. It is reusable chip-
//! health math for **any** multi-chip hashboard (Antminer + WhatsMiner) —
//! # Consumers
//!
//! - `dcentrald-diagnostics::chip_analysis_bridge` (2026-07-11) — first dependency
//!   edge; pure host-testable bridge. Live ChipMap enrichment still optional.
//! - Future: autotuner B26 chip-health, toolbox, bench (`dcent-diag-core`).
//!
//! Do not re-implement Laplacian / z-score / nonce-deficit outside this crate.
//! Adding a dependency edge into a live mining/voltage/thermal path remains
//! expert-review-gated.
//!
//! All three primitives share the same "hot-spot only" convention from the
//! source: they report **0.0** when a chip is performing at-or-better-than
//! its reference (cooler than neighbors / not above the cross-slot mean /
//! at-or-above the slot nonce average), and a positive anomaly magnitude
//! otherwise. This makes them safe to sum/threshold without sign handling.

#![forbid(unsafe_code)]

/// Per-chip analysis result.
///
/// Source: s16b fact **B08** (`whatsminer_chip_map/src/analysis.rs:41-46`).
/// All three fields use the "hot-spot only" convention — `0.0` means the
/// chip is not anomalous on that axis; larger positive values are worse.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChipAnalysis {
    /// Laplacian temperature gradient vs in-board neighbors (°C above the
    /// neighbor mean; `0.0` if cooler-or-equal). See [`compute_hot_gradient`].
    pub gradient: f32,
    /// Cross-slot z-score for the same physical chip position across slots
    /// (`0.0` if at-or-below the cross-slot mean). See
    /// [`compute_cross_slot_zscore`].
    pub cross_slot_zscore: f32,
    /// Nonce deficit as a percentage below the slot average (`0.0` if the
    /// chip is at-or-above the slot average). See [`compute_nonce_deficit`].
    pub nonce_deficit: f32,
}

/// Small-std guard threshold (°C of std-dev): below this the slot population
/// is treated as uniform and any positive deviation is significant.
///
/// Source: s16b fact **B06** (`analysis.rs:235`).
const ZSCORE_UNIFORM_STD_THRESHOLD: f32 = 0.5;

/// Z-score cap applied when the population is uniform (`std < 0.5`), so a tiny
/// absolute deviation against a degenerate std cannot blow up to infinity.
///
/// Source: s16b fact **B06** (`analysis.rs:237`).
const ZSCORE_UNIFORM_CAP: f32 = 3.0;

/// Laplacian / gradient hot-spot anomaly.
///
/// Returns how much hotter `center` is than the mean of its in-board
/// `neighbors`. Returns `0.0` when the chip is the same temperature or
/// cooler (we only care about hot spots), and `0.0` when it has no
/// neighbors (grid corners/edges in the source have fewer neighbors).
///
/// Source: s16b fact **B05** — `compute_hot_gradient`
/// (`whatsminer_chip_map/src/analysis.rs:189-200`). Byte-faithful:
/// `(center - mean(neighbors)).max(0.0)`. Neighbor selection (the
/// up/down/left/right rectangular-grid rule, B10) is the caller's job; this
/// fn takes the already-collected neighbor temperatures.
///
/// Temperatures are integer °C to match the WhatsMiner `Chip.temp: i32` and
/// `Slot` models (s16b facts B01/B02).
pub fn compute_hot_gradient(center: i32, neighbors: &[i32]) -> f32 {
    if neighbors.is_empty() {
        return 0.0;
    }

    let center_f = center as f32;
    let neighbor_avg: f32 =
        neighbors.iter().map(|&t| t as f32).sum::<f32>() / neighbors.len() as f32;

    // Only return positive values (hotter than neighbors).
    (center_f - neighbor_avg).max(0.0)
}

/// Population mean and standard deviation of a temperature sample set.
///
/// Returns `(0.0, 0.0)` for an empty set and `(mean, 0.0)` for a single
/// sample (std is undefined / degenerate). Uses the **population** variance
/// (divide by `n`, not `n-1`) to match the source.
///
/// Source: s16b fact **B06** — `compute_mean_std`
/// (`whatsminer_chip_map/src/analysis.rs:203-221`).
pub fn compute_mean_std(temps: &[i32]) -> (f32, f32) {
    if temps.is_empty() {
        return (0.0, 0.0);
    }

    let n = temps.len() as f32;
    let mean: f32 = temps.iter().map(|&t| t as f32).sum::<f32>() / n;

    if temps.len() == 1 {
        return (mean, 0.0);
    }

    let variance: f32 = temps
        .iter()
        .map(|&t| (t as f32 - mean).powi(2))
        .sum::<f32>()
        / n;
    (mean, variance.sqrt())
}

/// Cross-slot hot z-score against a precomputed `mean`/`std`.
///
/// Returns `0.0` when the chip is at-or-below the cross-slot mean (we only
/// flag systematically *hot* chips). When the population is uniform
/// (`std < 0.5`, [`ZSCORE_UNIFORM_STD_THRESHOLD`]) the raw deviation is
/// returned capped at `3.0` ([`ZSCORE_UNIFORM_CAP`]) so a degenerate std
/// cannot produce a divide-by-tiny blow-up; otherwise the standard
/// `(temp - mean) / std` z-score is returned.
///
/// Source: s16b fact **B06** — `compute_hot_zscore`
/// (`whatsminer_chip_map/src/analysis.rs:225-241`).
pub fn compute_hot_zscore(temp: i32, mean: f32, std: f32) -> f32 {
    let temp_f = temp as f32;
    let deviation = temp_f - mean;

    // Only care about chips hotter than the cross-slot average.
    if deviation <= 0.0 {
        return 0.0;
    }

    // If std is very small, all slots are similar - any deviation is significant.
    if std < ZSCORE_UNIFORM_STD_THRESHOLD {
        return deviation.min(ZSCORE_UNIFORM_CAP);
    }

    deviation / std
}

/// Cross-slot hot z-score directly from the population of same-position
/// temperatures across all slots.
///
/// Convenience over [`compute_mean_std`] + [`compute_hot_zscore`]: pass the
/// temperatures of the chip at the *same physical position* in every slot
/// (s16b fact **B12** — identifies a chip that is systematically hot
/// regardless of slot). `temp` is this chip's reading; it may or may not be
/// included in `cross_slot_samples` (matching the source, which builds the
/// population from all slots).
///
/// Source: s16b facts **B06 + B12**
/// (`whatsminer_chip_map/src/analysis.rs:198-225`).
pub fn compute_cross_slot_zscore(temp: i32, cross_slot_samples: &[i32]) -> f32 {
    let (mean, std) = compute_mean_std(cross_slot_samples);
    compute_hot_zscore(temp, mean, std)
}

/// Average valid-nonce count across the chips in a slot.
///
/// Returns `0.0` for an empty slot. Inputs are `i64` to match the
/// WhatsMiner `Chip.nonce: i64` model (s16b fact **B01**).
///
/// Source: s16b fact **B07** — `compute_slot_avg_nonce`
/// (`whatsminer_chip_map/src/analysis.rs:244-250`).
pub fn compute_slot_avg_nonce(chip_nonces: &[i64]) -> f64 {
    if chip_nonces.is_empty() {
        return 0.0;
    }
    let total: i64 = chip_nonces.iter().sum();
    total as f64 / chip_nonces.len() as f64
}

/// Nonce deficit: how far below the slot average (in percent) this chip's
/// nonce count is.
///
/// `0.0` = at-or-above the slot average (no deficit); `100.0` = zero nonces
/// while the slot average is non-zero. Returns `0.0` when the slot average
/// is non-positive (no nonces on the slot — deficit is undefined).
///
/// Source: s16b fact **B07** — `compute_nonce_deficit`
/// (`whatsminer_chip_map/src/analysis.rs:254-269`). Byte-faithful:
/// `(slot_avg - chip_nonce) / slot_avg * 100`.
pub fn compute_nonce_deficit(chip_nonce: i64, slot_avg: f64) -> f32 {
    if slot_avg <= 0.0 {
        // No nonces on slot, can't compute deficit.
        return 0.0;
    }

    let chip_nonce_f = chip_nonce as f64;
    if chip_nonce_f >= slot_avg {
        // At or above average - no deficit.
        return 0.0;
    }

    // Deficit as percentage: (avg - chip) / avg * 100.
    let deficit = (slot_avg - chip_nonce_f) / slot_avg * 100.0;
    deficit as f32
}

/// Compute all three chip-health axes for one chip into a [`ChipAnalysis`].
///
/// Convenience aggregator over the three primitives; the inputs mirror the
/// per-slot/per-chip data the WhatsMiner `analyze_all_slots` walk feeds each
/// function (s16b fact **B09**). Pure — it only calls the primitives above.
pub fn analyze_chip(
    chip_temp: i32,
    neighbors: &[i32],
    cross_slot_samples: &[i32],
    chip_nonce: i64,
    slot_avg_nonce: f64,
) -> ChipAnalysis {
    ChipAnalysis {
        gradient: compute_hot_gradient(chip_temp, neighbors),
        cross_slot_zscore: compute_cross_slot_zscore(chip_temp, cross_slot_samples),
        nonce_deficit: compute_nonce_deficit(chip_nonce, slot_avg_nonce),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Float comparison tolerance for the worked examples below.
    const EPS: f32 = 1e-4;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < EPS, "expected {b}, got {a}");
    }

    fn approx64(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    // ---- compute_hot_gradient (s16b B05) ----

    #[test]
    fn gradient_no_neighbors_is_zero() {
        approx(compute_hot_gradient(80, &[]), 0.0);
    }

    #[test]
    fn gradient_hot_chip_returns_positive_delta() {
        // center 60 vs neighbor mean 50 -> +10.0
        approx(compute_hot_gradient(60, &[50, 50]), 10.0);
        // center 65 vs mean of [50,60,70] = 60 -> +5.0
        approx(compute_hot_gradient(65, &[50, 60, 70]), 5.0);
    }

    #[test]
    fn gradient_cool_or_equal_chip_is_zero() {
        // cooler than neighbors -> clamped to 0.0
        approx(compute_hot_gradient(40, &[50, 50]), 0.0);
        // exactly equal to neighbor mean -> 0.0
        approx(compute_hot_gradient(50, &[50, 50]), 0.0);
    }

    // ---- compute_mean_std (s16b B06 helper) ----

    #[test]
    fn mean_std_empty_and_single() {
        assert_eq!(compute_mean_std(&[]), (0.0, 0.0));
        assert_eq!(compute_mean_std(&[70]), (70.0, 0.0));
    }

    #[test]
    fn mean_std_uniform_population() {
        let (mean, std) = compute_mean_std(&[50, 50, 50]);
        approx(mean, 50.0);
        approx(std, 0.0);
    }

    #[test]
    fn mean_std_worked_example() {
        // [10,20,30,40,50]: mean 30; population variance
        // = (400+100+0+100+400)/5 = 200; std = sqrt(200) = 14.142135...
        let (mean, std) = compute_mean_std(&[10, 20, 30, 40, 50]);
        approx(mean, 30.0);
        approx(std, 200.0_f32.sqrt());
        approx(std, 14.142_136);
    }

    // ---- compute_hot_zscore (s16b B06) ----

    #[test]
    fn zscore_standard_division_path() {
        // temp 80, mean 60, std 10 -> deviation 20 / 10 = 2.0
        approx(compute_hot_zscore(80, 60.0, 10.0), 2.0);
    }

    #[test]
    fn zscore_at_or_below_mean_is_zero() {
        approx(compute_hot_zscore(50, 60.0, 10.0), 0.0); // below
        approx(compute_hot_zscore(60, 60.0, 10.0), 0.0); // equal
    }

    #[test]
    fn zscore_uniform_population_returns_capped_deviation() {
        // std < 0.5 -> return raw deviation, capped at 3.0
        approx(compute_hot_zscore(62, 60.0, 0.0), 2.0); // dev 2 -> 2.0
        approx(compute_hot_zscore(70, 60.0, 0.0), 3.0); // dev 10 -> capped 3.0
    }

    // ---- compute_cross_slot_zscore (s16b B06 + B12) ----

    #[test]
    fn cross_slot_zscore_division_path_worked_example() {
        // samples [40,60,80]: mean 60, variance 800/3 = 266.6667,
        // std = 16.329932; temp 80 -> dev 20 / std = 1.224745
        approx(compute_cross_slot_zscore(80, &[40, 60, 80]), 1.224_745);
    }

    #[test]
    fn cross_slot_zscore_uniform_population_caps() {
        // mean 60, std 0 -> uniform path; dev 20 -> capped at 3.0
        approx(compute_cross_slot_zscore(80, &[60, 60, 60, 60]), 3.0);
    }

    #[test]
    fn cross_slot_zscore_cool_chip_is_zero() {
        approx(compute_cross_slot_zscore(55, &[60, 60, 60, 60]), 0.0);
    }

    // ---- compute_slot_avg_nonce (s16b B07 helper) ----

    #[test]
    fn slot_avg_nonce_empty_is_zero() {
        approx64(compute_slot_avg_nonce(&[]), 0.0);
    }

    #[test]
    fn slot_avg_nonce_worked_example() {
        approx64(compute_slot_avg_nonce(&[100, 200, 300]), 200.0);
        approx64(compute_slot_avg_nonce(&[0, 0, 0, 400]), 100.0);
    }

    // ---- compute_nonce_deficit (s16b B07) ----

    #[test]
    fn nonce_deficit_no_slot_nonces_is_zero() {
        approx(compute_nonce_deficit(0, 0.0), 0.0);
        approx(compute_nonce_deficit(50, -1.0), 0.0);
    }

    #[test]
    fn nonce_deficit_at_or_above_average_is_zero() {
        approx(compute_nonce_deficit(250, 200.0), 0.0); // above
        approx(compute_nonce_deficit(200, 200.0), 0.0); // equal
    }

    #[test]
    fn nonce_deficit_worked_examples() {
        // chip 100 vs avg 200 -> (100/200)*100 = 50%
        approx(compute_nonce_deficit(100, 200.0), 50.0);
        // chip 150 vs avg 200 -> (50/200)*100 = 25%
        approx(compute_nonce_deficit(150, 200.0), 25.0);
        // dead chip (0 nonces) vs avg 200 -> 100%
        approx(compute_nonce_deficit(0, 200.0), 100.0);
    }

    // ---- analyze_chip aggregator (s16b B08 + B09) ----

    #[test]
    fn analyze_chip_combines_all_three_axes() {
        let result = analyze_chip(
            80,            // chip temp
            &[60, 60],     // in-board neighbors (mean 60)
            &[40, 60, 80], // same-position samples across slots
            100,           // this chip's nonce count
            200.0,         // slot average nonce
        );
        approx(result.gradient, 20.0); // 80 - 60
        approx(result.cross_slot_zscore, 1.224_745); // dev 20 / std 16.32993
        approx(result.nonce_deficit, 50.0); // (100/200)*100
    }

    #[test]
    fn analyze_chip_healthy_chip_is_all_zero() {
        // cooler than neighbors, below cross-slot mean, above slot nonce avg
        let result = analyze_chip(50, &[60, 60], &[60, 60, 60], 300, 200.0);
        assert_eq!(result, ChipAnalysis::default());
    }
}
