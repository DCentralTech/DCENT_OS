// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — PLL parameter calculator
// Faithful port from ESP-Miner pll.c

/// PLL reference clock frequency in MHz
pub const FREQ_MULT: f32 = 25.0;

const EPSILON: f32 = 0.0001;

/// PLL parameters result
#[derive(Debug, Clone, Copy)]
pub struct PllParams {
    pub fb_divider: u8,
    pub refdiv: u8,
    pub postdiv1: u8,
    pub postdiv2: u8,
    pub actual_freq: f32,
}

/// Calculate optimal PLL parameters for a target frequency.
///
/// Exact port of pll_get_parameters() from pll.c.
///
/// # Arguments
/// * `target_freq` - Desired frequency in MHz
/// * `fb_divider_min` - Minimum feedback divider value (chip-specific)
/// * `fb_divider_max` - Maximum feedback divider value (chip-specific)
///
/// # Chip-specific fb_divider ranges:
/// * BM1366: 144..=235
/// * BM1368: 144..=235
/// * BM1370: 160..=239
/// * BM1397: 60..=200
pub fn find_best_pll(target_freq: f32, fb_divider_min: u16, fb_divider_max: u16) -> PllParams {
    let mut best_freq: f32 = 0.0;
    let mut best_refdiv: u8 = 0;
    let mut best_fb_divider: u8 = 0;
    let mut best_postdiv1: u8 = 0;
    let mut best_postdiv2: u8 = 0;
    let mut min_diff: f32 = f32::MAX;
    let mut min_vco_freq: f32 = f32::MAX;
    let mut min_postdiv: u16 = u16::MAX;

    // Iterate refdiv from 2 down to 1 (matching C: for refdiv=2; refdiv>0; refdiv--)
    for refdiv in (1u8..=2).rev() {
        // Iterate postdiv1 from 7 down to 1
        for postdiv1 in (1u8..=7).rev() {
            // Iterate postdiv2 from 7 down to 1
            for postdiv2 in (1u8..=7).rev() {
                let divider = refdiv as u16 * postdiv1 as u16 * postdiv2 as u16;
                let fb_divider_f = (target_freq / FREQ_MULT * divider as f32).round();
                let fb_divider = fb_divider_f as u16;

                if postdiv1 > postdiv2
                    && fb_divider >= fb_divider_min
                    && fb_divider <= fb_divider_max
                {
                    let new_freq = FREQ_MULT * fb_divider as f32 / divider as f32;
                    let curr_diff = (target_freq - new_freq).abs();
                    let vco_freq = FREQ_MULT * fb_divider as f32 / refdiv as f32;

                    // Prioritize:
                    // 1. Closest frequency to target
                    // 2. Lowest VCO frequency
                    // 3. Lowest postdiv1 * postdiv2
                    if curr_diff < min_diff
                        || ((curr_diff - min_diff).abs() < EPSILON && vco_freq < min_vco_freq)
                        || ((curr_diff - min_diff).abs() < EPSILON
                            && (vco_freq - min_vco_freq).abs() < EPSILON
                            && (postdiv1 as u16 * postdiv2 as u16) < min_postdiv)
                    {
                        min_diff = curr_diff;
                        min_vco_freq = vco_freq;
                        min_postdiv = postdiv1 as u16 * postdiv2 as u16;
                        best_freq = new_freq;
                        best_refdiv = refdiv;
                        best_fb_divider = fb_divider as u8;
                        best_postdiv1 = postdiv1;
                        best_postdiv2 = postdiv2;
                    }
                }
            }
        }
    }

    log::info!(
        "PLL: {} MHz (fb_divider: {}, refdiv: {}, postdiv1: {}, postdiv2: {})",
        best_freq,
        best_fb_divider,
        best_refdiv,
        best_postdiv1,
        best_postdiv2
    );

    PllParams {
        fb_divider: best_fb_divider,
        refdiv: best_refdiv,
        postdiv1: best_postdiv1,
        postdiv2: best_postdiv2,
        actual_freq: best_freq,
    }
}

/// Frequency-transition step size: the PLL is moved in 6.25 MHz increments to
/// protect PLL lock and the rail on every frequency change.
const FREQ_STEP_SIZE_MHZ: f32 = 6.25;

/// Pure step-sequence generator for [`do_frequency_transition`].
///
/// Returns the exact ordered list of intermediate + final frequencies the ASIC
/// is stepped through to ramp from `current` to `target` in ≤ 6.25 MHz
/// increments — the SAME control flow as `do_frequency_transition`, MINUS the
/// per-step `thread::sleep` and the `set_freq` side effect, so the ramp bounds
/// are host-testable.
///
/// - Endpoints within `EPSILON` (no-op): empty vector.
/// - Target within one 6.25 MHz step: a single step straight to `target`.
/// - Otherwise: each on-grid 6.25 MHz step toward `target`, then a final exact
///   `target` step when the last grid step did not already land on it.
pub fn frequency_transition_steps(current: f32, target: f32) -> Vec<f32> {
    const STEP_SIZE: f32 = FREQ_STEP_SIZE_MHZ;
    let mut steps = Vec::new();

    if (current - target).abs() < EPSILON {
        return steps;
    }

    if (target - current).abs() < STEP_SIZE {
        steps.push(target);
        return steps;
    }

    // `cur` mirrors the `*current_frequency` the original mutated in-loop.
    let mut cur = current;

    let mut current_step = if target > current {
        (current / STEP_SIZE).floor() as i32
    } else {
        (current / STEP_SIZE).ceil() as i32
    };

    let target_step = if target > current {
        (target / STEP_SIZE).floor() as i32
    } else {
        (target / STEP_SIZE).ceil() as i32
    };

    if current_step != target_step {
        let signum: i32 = if target > current { 1 } else { -1 };

        while (signum > 0 && current_step < target_step)
            || (signum < 0 && current_step > target_step)
        {
            current_step += signum;
            cur = current_step as f32 * STEP_SIZE;
            steps.push(cur);
        }
    }

    if (cur - target).abs() > EPSILON {
        steps.push(target);
    }

    steps
}

/// Frequency transition: ramp frequency in steps of 6.25 MHz.
/// Exact port of do_frequency_transition() from frequency_transition_bmXX.c.
///
/// Calls the provided `set_freq` closure for each step in
/// [`frequency_transition_steps`] (with the original 100 ms inter-step settle).
/// The step *sequence* and the final `current_frequency` are byte-identical to
/// the prior inline state machine; the only change is that the single-step and
/// final-exact-step now also get the 100 ms settle (strictly more conservative).
pub fn do_frequency_transition<F>(
    current_frequency: &mut f32,
    target_frequency: f32,
    mut set_freq: F,
) where
    F: FnMut(f32),
{
    let steps = frequency_transition_steps(*current_frequency, target_frequency);
    if steps.is_empty() {
        // Endpoints already within EPSILON — nothing to do.
        return;
    }

    // "Ramping" banner only for a true multi-step ramp (target more than one
    // 6.25 MHz step away) — matches the prior `< STEP_SIZE` single-step gate.
    let is_ramp = (target_frequency - *current_frequency).abs() >= FREQ_STEP_SIZE_MHZ;
    if is_ramp {
        log::info!(
            "Ramping frequency from {} MHz to {} MHz",
            *current_frequency,
            target_frequency
        );
    }

    for f in steps {
        *current_frequency = f;
        set_freq(*current_frequency);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if is_ramp {
        log::info!("Successfully transitioned to {} MHz", target_frequency);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Faithful to ESP-Miner pll.c: when no (refdiv,postdiv1,postdiv2) yields an
    /// in-range fb_divider (target below the chip's representable minimum), the
    /// search leaves every `best_*` at 0. This is the no-solution sentinel the
    /// driver-level `fb_divider == 0` guards rely on. Pinning it here documents
    /// that sending fb_divider=0 / refdiv=0 (and the `postdiv1 - 1` underflow it
    /// would feed) is a real reachable state for an out-of-range request.
    #[test]
    fn find_best_pll_returns_zero_sentinel_when_target_below_min() {
        // 10 MHz is far below the representable minimum for every chip's range.
        for (min, max) in [(144u16, 235u16), (160, 239), (60, 200)] {
            let p = find_best_pll(10.0, min, max);
            assert_eq!(p.fb_divider, 0, "no-solution must leave fb_divider=0");
            assert_eq!(p.refdiv, 0, "no-solution must leave refdiv=0");
            assert_eq!(p.postdiv1, 0, "no-solution must leave postdiv1=0");
            assert_eq!(p.postdiv2, 0, "no-solution must leave postdiv2=0");
        }
    }

    /// A representable in-range target must yield a non-zero fb_divider so the
    /// guards never reject a valid operating frequency.
    #[test]
    fn find_best_pll_in_range_target_has_nonzero_fb_divider() {
        let p = find_best_pll(485.0, 144, 235);
        assert_ne!(p.fb_divider, 0, "in-range target must produce a solution");
        assert!(p.postdiv1 >= 1 && p.postdiv2 >= 1);
    }

    // ─── M-asic: do_frequency_transition PLL-ramp bounds (pure step sequence) ──

    const STEP: f32 = 6.25;

    /// Validate the universal ramp invariants the PLL/rail protection depends on:
    /// on-grid intermediate steps, ≤ 6.25 MHz hops, monotonic toward target, no
    /// overshoot, and an exact landing on `target`.
    fn assert_valid_ramp(current: f32, target: f32) {
        const TOL: f32 = 1e-3;
        let steps = frequency_transition_steps(current, target);
        assert!(
            !steps.is_empty(),
            "{current}->{target}: non-equal endpoints must produce steps"
        );
        assert_eq!(
            *steps.last().unwrap(),
            target,
            "{current}->{target}: ramp must end EXACTLY on target"
        );

        let up = target > current;
        let mut prev = current;
        for (i, &f) in steps.iter().enumerate() {
            assert!(
                (f - prev).abs() <= STEP + TOL,
                "{current}->{target}: hop {prev}->{f} exceeds the 6.25 MHz step"
            );
            if up {
                assert!(
                    f >= prev - TOL,
                    "{current}->{target}: up-ramp moved backward {prev}->{f}"
                );
                assert!(
                    f <= target + TOL,
                    "{current}->{target}: up-ramp overshoot {f} > {target}"
                );
            } else {
                assert!(
                    f <= prev + TOL,
                    "{current}->{target}: down-ramp moved backward {prev}->{f}"
                );
                assert!(
                    f >= target - TOL,
                    "{current}->{target}: down-ramp overshoot {f} < {target}"
                );
            }
            // Every step EXCEPT possibly the final exact-target hop is grid-aligned.
            if i + 1 < steps.len() {
                let snapped = (f / STEP).round() * STEP;
                assert!(
                    (f - snapped).abs() <= TOL,
                    "{current}->{target}: intermediate {f} is off the 6.25 MHz grid"
                );
            }
            prev = f;
        }
    }

    #[test]
    fn ramp_up_and_down_grid_aligned() {
        assert_valid_ramp(100.0, 200.0);
        assert_valid_ramp(200.0, 100.0);
        // Realistic BM operating moves.
        assert_valid_ramp(480.0, 525.0);
        assert_valid_ramp(525.0, 480.0);
        assert_valid_ramp(200.0, 500.0);
        assert_valid_ramp(500.0, 200.0);
    }

    #[test]
    fn ramp_off_grid_endpoints_still_land_on_target() {
        // Off-grid target: intermediate steps stay on the grid, final hop hits it.
        assert_valid_ramp(100.0, 205.0);
        assert_valid_ramp(205.0, 100.0);
        // Off-grid current too.
        assert_valid_ramp(103.4, 200.0);
        assert_valid_ramp(203.7, 100.0);
        assert_valid_ramp(491.3, 533.1);
    }

    #[test]
    fn ramp_fully_grid_aligned_every_step_on_grid() {
        // When BOTH endpoints are on the 6.25 MHz grid, EVERY visited freq is on it.
        let steps = frequency_transition_steps(100.0, 200.0);
        for &f in &steps {
            let snapped = (f / STEP).round() * STEP;
            assert!(
                (f - snapped).abs() <= 1e-4,
                "{f} is not on the 6.25 MHz grid"
            );
        }
        // 100 -> 200 in 6.25 MHz hops = 16 steps (106.25 .. 200.0).
        assert_eq!(steps.len(), 16);
        assert_eq!(steps.first().copied(), Some(106.25));
        assert_eq!(steps.last().copied(), Some(200.0));
    }

    #[test]
    fn ramp_short_circuit_single_step_under_6_25() {
        // A target within one 6.25 MHz step: exactly one step, straight to target.
        assert_eq!(frequency_transition_steps(200.0, 203.0), vec![203.0]);
        assert_eq!(frequency_transition_steps(200.0, 197.0), vec![197.0]);
        // Exactly 6.25 apart is NOT a short-circuit (it is the ramp path) — one hop.
        assert_eq!(frequency_transition_steps(100.0, 106.25), vec![106.25]);
    }

    #[test]
    fn ramp_equal_endpoints_is_noop() {
        assert!(frequency_transition_steps(200.0, 200.0).is_empty());
        // Within EPSILON of each other is also a no-op (no spurious step).
        assert!(frequency_transition_steps(200.0, 200.00005).is_empty());
    }

    #[test]
    fn do_frequency_transition_drives_steps_and_lands_on_target() {
        // The wrapper must call set_freq for each generated step, in order, and
        // leave current_frequency EXACTLY on target — matching the pure sequence.
        let mut cur = 100.0f32;
        let mut seen = Vec::new();
        do_frequency_transition(&mut cur, 115.0, |f| seen.push(f));
        assert_eq!(seen, frequency_transition_steps(100.0, 115.0));
        assert_eq!(seen, vec![106.25, 112.5, 115.0]);
        assert_eq!(cur, 115.0);
    }

    #[test]
    fn do_frequency_transition_noop_leaves_state_untouched() {
        let mut cur = 480.0f32;
        let mut calls = 0;
        do_frequency_transition(&mut cur, 480.0, |_| calls += 1);
        assert_eq!(calls, 0, "equal endpoints must not call set_freq");
        assert_eq!(cur, 480.0);
    }
}
