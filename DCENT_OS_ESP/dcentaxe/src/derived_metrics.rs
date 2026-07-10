// DCENT_axe — derived telemetry metrics (host-pure)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Host-pure derivations for the `/api/system/info` telemetry surface.
//!
//! The derived fields `acceptanceRate`, `efficiency` (J/TH) and
//! `expectedHashrate` used to be computed INLINE inside the esp-idf-gated
//! `register_system_info` handler in `dcentaxe/src/api.rs`, which made them the
//! only telemetry derivations with ZERO host tests. This module extracts the
//! math into pure functions over plain numbers (no esp-idf, no locks) so it
//! host-compiles and unit-tests under `cargo test -p dcentaxe-core` (re-included
//! via `#[path]` in `dcentaxe-core/src/lib.rs`, the same single-source-of-truth
//! pattern used by `mqtt_ha.rs` / `metrics_render.rs` / `chip_profiles_bitaxe.rs`).
//! The esp-idf handler stays thin: it gathers the live values and calls these
//! functions.
//!
//! **Data-honesty (M-dash-1):** [`acceptance_rate_pct`] returns `None` when NO
//! shares have resolved yet (`accepted + rejected == 0`). The legacy inline code
//! fabricated `100.0` in that case, so a freshly-booted miner reported a real-
//! looking `acceptanceRate: 100.0` (a 100% success rate it had not earned). The
//! honest answer at zero samples is "unknown", which the handler surfaces as the
//! additive `acceptanceRateKnown: false` companion plus a documented out-of-range
//! sentinel — never a fabricated 100.0.

/// Share-acceptance rate as a percentage in `[0.0, 100.0]`.
///
/// Returns `None` when no shares have resolved yet (`accepted + rejected == 0`)
/// — the honest zero-sample answer. The caller must NOT substitute a fabricated
/// `100.0` for `None`; it should expose the unknown state explicitly (the
/// `acceptanceRateKnown` companion + an out-of-range sentinel).
pub fn acceptance_rate_pct(accepted: u64, rejected: u64) -> Option<f64> {
    let total = accepted + rejected;
    if total == 0 {
        None
    } else {
        Some(accepted as f64 / total as f64 * 100.0)
    }
}

/// Mining efficiency in joules-per-terahash (J/TH).
///
/// `power_w` is board watts (INA260), `hashrate_ghs` is GH/s. Guarded against
/// divide-by-zero: returns `0.0` when `hashrate_ghs <= 0.0` (idle / pre-mining),
/// matching the legacy inline behavior exactly. The non-zero branch is the
/// byte-identical port of `power_w / (hashrate_ghs / 1000.0)`.
pub fn efficiency_jth(power_w: f64, hashrate_ghs: f64) -> f64 {
    if hashrate_ghs > 0.0 {
        power_w / (hashrate_ghs / 1000.0)
    } else {
        0.0
    }
}

/// Theoretical expected hashrate in GH/s from the chip's small-core count.
///
/// Byte-identical port of the legacy inline calc
/// `target_frequency * small_cores_per_chip * asic_count / 1000.0`. `target_frequency`
/// is MHz; the `/ 1000.0` converts MH/s → GH/s. Inputs match the handler's locals
/// exactly (`config.target_frequency: f32`, the per-model small-core count, and the
/// resolved ASIC count).
pub fn expected_hashrate_ghs(
    target_frequency: f32,
    small_cores_per_chip: u32,
    asic_count: u32,
) -> f64 {
    target_frequency as f64 * small_cores_per_chip as f64 * asic_count as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── acceptance_rate_pct: honest at zero samples ──────────────────────────
    #[test]
    fn acceptance_rate_is_none_at_zero_samples() {
        // The load-bearing honesty case: a freshly-booted miner has resolved no
        // shares, so the answer is unknown — NOT a fabricated 100.0.
        assert_eq!(acceptance_rate_pct(0, 0), None);
    }

    #[test]
    fn acceptance_rate_all_accepted() {
        assert_eq!(acceptance_rate_pct(99, 1), Some(99.0));
        assert_eq!(acceptance_rate_pct(5, 0), Some(100.0));
    }

    #[test]
    fn acceptance_rate_all_rejected() {
        assert_eq!(acceptance_rate_pct(0, 5), Some(0.0));
    }

    #[test]
    fn acceptance_rate_midrange() {
        assert_eq!(acceptance_rate_pct(3, 1), Some(75.0));
    }

    // ── efficiency_jth: div-by-zero guarded + normal value ───────────────────
    #[test]
    fn efficiency_div_by_zero_is_zero() {
        assert_eq!(efficiency_jth(15.0, 0.0), 0.0);
        // Negative/garbage hashrate also takes the guarded branch.
        assert_eq!(efficiency_jth(15.0, -1.0), 0.0);
    }

    #[test]
    fn efficiency_normal_value() {
        // 15 W at 1000 GH/s (= 1 TH/s) → 15 J/TH.
        assert_eq!(efficiency_jth(15.0, 1000.0), 15.0);
        // 30 W at 500 GH/s (= 0.5 TH/s) → 60 J/TH.
        assert_eq!(efficiency_jth(30.0, 500.0), 60.0);
    }

    // ── expected_hashrate_ghs: edge + normal ─────────────────────────────────
    #[test]
    fn expected_hashrate_zero_count() {
        // No ASICs resolved → zero expected hashrate (no NaN/inf).
        assert_eq!(expected_hashrate_ghs(500.0, 2040, 0), 0.0);
    }

    #[test]
    fn expected_hashrate_normal_value() {
        // BM1370 Gamma: 500 MHz * 2040 small cores * 1 ASIC / 1000 = 1020 GH/s.
        assert_eq!(expected_hashrate_ghs(500.0, 2040, 1), 1020.0);
        // BM1397 Max: 425 MHz * 672 * 1 / 1000 = 285.6 GH/s.
        assert!((expected_hashrate_ghs(425.0, 672, 1) - 285.6).abs() < 1e-9);
    }
}
