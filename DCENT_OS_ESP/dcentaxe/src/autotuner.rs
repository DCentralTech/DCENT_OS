// DCENT_axe Autotuner — Advanced Frequency/Voltage Optimization
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// The most advanced autotuner for BitAxe: multi-phase silicon profiling
// with real-time power/thermal/error feedback.
//
// Phases:
//   1. WARMUP (30s)   — Let ASIC reach thermal equilibrium at default settings
//   2. PROFILE (5min)  — Step through frequency range, measure hashrate/power/errors
//   3. OPTIMIZE (cont.) — Converge on optimal point based on mode
//   4. MAINTAIN (cont.) — Hold steady, adjust for thermal drift
//
// Modes:
//   MaxHashrate    — Push to highest stable frequency (respects power limits)
//   TargetWatts    — Find max hashrate at a power budget
//   BestEfficiency — Minimize J/TH (sweet spot: usually 60-70% of max freq)
//   TargetTemp     — Max hashrate under a temperature ceiling

use crate::chip_profiles_bitaxe::{
    best_fit_row_in_band, best_point_for_mode, compute_jth, descent_settle_elapsed_ok,
    last_known_good_changed, BestPointMode, BITAXE_MAX_FREQ_MHZ, BM1366_BITAXE_PROFILE,
    BM1368_BITAXE_PROFILE, BM1370_BITAXE_PROFILE, BM1397_BITAXE_PROFILE, MAX_ERROR_RATE,
    SINGLE_CHIP_VDD_MAX, SINGLE_CHIP_VDD_MIN,
};
use crate::config::PowerLimits;
use crate::power_measurement::{PowerSample, PowerWindow};
use crate::shared::{AutotuneMode, SharedState};
use log::*;

/// Canonical list of autotuner modes with human-readable names + descriptions.
/// Served by `GET /api/autotuner/modes` and rendered by the dashboard — keeps
/// the UI from drifting away from the firmware (see feedback memory
/// ).
pub const MODE_DESCRIPTIONS: &[(&str, &str, &str)] = &[
    (
        "max_hashrate",
        "Max hashrate",
        "Push frequency up until power or thermals cap it. Maximum hash power.",
    ),
    (
        "best_efficiency",
        "Best efficiency",
        "Find the lowest J/TH sweet spot, typically 60-70 % of the max frequency.",
    ),
    (
        "target_watts",
        "Target watts",
        "Hit a power budget and squeeze the best hashrate under that ceiling.",
    ),
    (
        "target_temp",
        "Target temperature",
        "Keep the chip at a chosen temperature \u{2014} autotuner raises frequency until it's just warm enough.",
    ),
];

/// Time to wait for thermal stabilization before tuning (seconds).
const WARMUP_SECS: u64 = 30;

/// Time between profiling steps (seconds). Need hashrate to stabilize.
const PROFILE_STEP_INTERVAL: u64 = 20;

/// Time between optimization adjustments (seconds).
const OPTIMIZE_INTERVAL: u64 = 30;

/// Time between maintenance checks (seconds).
const MAINTAIN_INTERVAL: u64 = 60;

/// Frequency step for profiling (MHz). Smaller = more precise but slower.
const PROFILE_FREQ_STEP: f32 = 12.5;

/// Frequency step for fine optimization (MHz).
const FINE_FREQ_STEP: f32 = 6.25;

/// Voltage step for optimization (mV).
const VOLTAGE_STEP: u16 = 10;

// `MAX_ERROR_RATE` (max acceptable HW error rate, errors/nonces; above this =
// unstable) is single-sourced from `chip_profiles_bitaxe::MAX_ERROR_RATE`
// (imported above) so the host-tested `silicon_grade` and this engine share ONE
// 2 % ceiling and can never drift.

/// Minimum hashrate (GH/s) to consider a setting working.
const MIN_HASHRATE_GHS: f64 = 10.0;

/// Hashrate drop threshold to trigger revert (percentage).
const HASHRATE_DROP_THRESHOLD: f64 = 0.10; // 10% drop

/// A single data point from profiling.
#[derive(Debug, Clone)]
struct ProfilePoint {
    frequency: f32,
    voltage: u16,
    hashrate_ghs: f64,
    power_w: f32,
    temp_c: f32,
    jth: f32,
    delta_error_rate: f64,
    stable: bool, // true if no HW errors or hashrate drops detected
}

/// Autotuner phase.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Waiting for ASIC thermal equilibrium
    Warmup,
    /// Stepping through frequency range to build profile
    Profiling,
    /// Bounded binary-search descent to hit `AutotuneMode::TargetWatts`.
    /// Activated only after `Profiling` completes when the operator-selected
    /// mode is `TargetWatts`. Other modes skip this phase entirely so the
    /// existing `MaxHashrate` / `BestEfficiency` / `TargetTemp` paths stay
    /// byte-identical (see `plans/wave4-dcentaxe-wattage-autotune.md` §B).
    /// Algorithm body is implemented by W5-G; this variant is the skeleton.
    WattageDescent,
    /// Fine-tuning around the best point
    Optimizing,
    /// Holding steady, monitoring for drift
    Maintaining,
    /// Autotuner disabled
    Idle,
}

impl Phase {
    /// data-model-fields §7.4(b): stable phase token surfaced on the wire as
    /// `dcentaxe.autotuner.phase`. The human `status` string is freeform/dynamic
    /// ("profiling 525 MHz", "fine-tuning", "power limit", "wattage descent",
    /// "health backoff", …) so the shared phase-ribbon rung CANNOT be cleanly
    /// derived from it by prefix-matching; this enum is the stable source.
    /// Token vocabulary is the canonical autotuner-stage set both UIs map into
    /// the shared phase-ribbon enum
    /// {disabled|idle|characterizing|verifying|thermal_refinement|tuned|partial|
    /// failed|background_adjust}: warmup/profiling→characterizing,
    /// optimizing→verifying, maintaining→tuned, idle→idle/-1.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Warmup => "warmup",
            Phase::Profiling => "profiling",
            Phase::WattageDescent => "wattage_descent",
            Phase::Optimizing => "optimizing",
            Phase::Maintaining => "maintaining",
            Phase::Idle => "idle",
        }
    }
}

/// The autotuner engine.
pub struct Autotuner {
    phase: Phase,
    last_action_time: std::time::Instant,

    // Board limits (from config)
    min_freq: f32,
    max_freq: f32,
    min_voltage: u16,
    max_voltage: u16,

    // Power limits (safe or overclock mode)
    power_limit_w: f32,

    // Profiling data
    profile: Vec<ProfilePoint>,
    profile_freq: f32, // current profiling frequency

    // Best known operating point
    best_point: Option<ProfilePoint>,

    // Optimization state
    opt_direction: i32, // +1 = trying higher, -1 = trying lower
    opt_phase_count: u32,
    prev_hashrate: f64,
    prev_power: f32,

    // Error tracking
    prev_nonces: u64,
    prev_errors: u64,

    // XPAUTO-2 (cross-pollinated from DCENT_OS DpsWalker HealthBackoff):
    // count of CONSECUTIVE ticks where `delta_error_rate` has stayed above
    // MAX_ERROR_RATE. Resets to 0 on any healthy/non-finite tick. Only consumed
    // when `AutotunerState::health_backoff_enabled` is set (default-OFF). The
    // actual retreat decision is the host-tested pure fn
    // `dcentaxe_hal::safety::hw_error_backoff_should_retreat` — this is just the
    // debounce counter that survives across `tick()` calls.
    hw_err_over_ceiling_streak: u8,

    // Wattage descent (Phase::WattageDescent) — skeleton fields owned by
    // this struct so the descent state survives across `tick()` calls.
    // Algorithm body is filled in by W5-G; W5-F provides plumbing only.
    // 30-second rolling window over `Telemetry::power_w` samples; 192 B
    // inline (see `power_measurement::PowerWindow` docs).
    power_window: PowerWindow,
    /// Outer-iteration counter for the bounded binary search. Hard cap = 12.
    descent_iter: u8,
    /// Number of consecutive 30 s windows where |measured - target| / target
    /// has been < 1 %. Convergence requires 2 (i.e. 60 s sustained hold).
    descent_converged_windows: u8,
    /// AUTOTUNE-8: the (freq, voltage_mv) the descent last actually applied.
    /// On convergence the last-known-good is persisted from THIS vetted point —
    /// not from the pre-step config or the Profiling-sweep best_point — so the
    /// saved/returned/held values are identical. `None` until a step is taken.
    descent_applied_setpoint: Option<(f32, u16)>,

    // AUTOTUNE-9: the last (freq, voltage_mv) actually written to NVS in the
    // Maintaining phase. Used to skip rewriting an identical last-known-good
    // every 60 s on a steady, healthy miner (flash-wear). `None` until the
    // first persist this session.
    last_persisted_lkg: Option<(f32, u16)>,
}

impl Autotuner {
    pub fn new(min_freq: f32, max_freq: f32, min_voltage: u16, max_voltage: u16) -> Self {
        Self {
            phase: Phase::Idle,
            last_action_time: std::time::Instant::now(),
            min_freq,
            max_freq,
            min_voltage,
            max_voltage,
            power_limit_w: 25.0, // default safe mode
            profile: Vec::with_capacity(32),
            profile_freq: 0.0,
            best_point: None,
            opt_direction: 1,
            opt_phase_count: 0,
            prev_hashrate: 0.0,
            prev_power: 0.0,
            prev_nonces: 0,
            prev_errors: 0,
            hw_err_over_ceiling_streak: 0,
            power_window: PowerWindow::new(),
            descent_iter: 0,
            descent_converged_windows: 0,
            descent_applied_setpoint: None,
            last_persisted_lkg: None,
        }
    }

    /// Update power limits from config (call when overclock mode changes).
    pub fn set_power_limits(&mut self, limits: &PowerLimits) {
        self.power_limit_w = limits.max_power_w;
        self.max_freq = limits.max_frequency;
        self.max_voltage = limits.max_voltage_mv;
    }

    /// Main tick — called every 5s from the heartbeat loop.
    /// Returns Some((new_freq, new_voltage)) if an adjustment should be made.
    pub fn tick(&mut self, state: &SharedState) -> Option<(f32, u16)> {
        let autotune_state = state
            .autotuner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // Handle enable/disable transitions
        if !autotune_state.enabled {
            if self.phase != Phase::Idle {
                info!("Autotuner: disabled");
                self.phase = Phase::Idle;
                self.update_status(state, "idle");
            }
            return None;
        }

        if self.phase == Phase::Idle {
            // Just enabled — start warmup
            info!("Autotuner: starting (mode: {:?})", autotune_state.mode);
            self.phase = Phase::Warmup;
            self.last_action_time = std::time::Instant::now();
            self.profile.clear();
            self.best_point = None;
            self.opt_direction = 1;
            self.opt_phase_count = 0;
            self.hw_err_over_ceiling_streak = 0;
            self.update_status(state, "warming up...");
            return None;
        }

        // Gather telemetry
        let stats = state.stats.lock().unwrap_or_else(|e| e.into_inner());
        let snap = stats.snapshot();
        drop(stats);
        let config = state
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let telem = state
            .telemetry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // AUTOTUNE-4: refresh the cached safety envelope from the LIVE config
        // every tick. `set_power_limits` is only called once at startup, but
        // `overclock_enabled` is togglable at runtime (api.rs) — if the tuner
        // kept a stale (higher) ceiling after an overclock-disable it would
        // judge now-over-budget points as "stable" and under-back-off. Sourcing
        // the caps live from the single `config.power_limits()` source-of-truth
        // means the tuner's internal budget can never diverge from config (the
        // applied point is also re-clamped by `qualify_operating_point`, but the
        // SEARCH must reason within the same envelope). This only ever tightens
        // or matches the limits; it never raises a safety cap.
        let live_limits = config.power_limits();
        self.power_limit_w = live_limits.max_power_w;
        self.max_freq = live_limits.max_frequency;
        self.max_voltage = live_limits.max_voltage_mv;

        let hashrate = snap.hashrate_30s_ghs;
        let power_w = telem.power_w;
        let max_temp = [telem.chip_temp_c, telem.board_temp_c, telem.vreg_temp_c]
            .iter()
            .copied()
            .fold(0.0_f32, f32::max);
        // AUTOTUNE-7: PEEK the error rate (cumulative since the last decision
        // point) without consuming the prev counters. The counters are advanced
        // ONLY when a decision is actually recorded (Profiling point / Maintain
        // health check), so the rate baked into a ProfilePoint covers the full
        // settle interval the chip ran at that setting — not just the last 5 s
        // tick. Consuming every tick (the old bug) measured each point over a
        // noisier 5 s window, causing spurious UNSTABLE→rollback / false-STABLE.
        let delta_error_rate = self.peek_delta_error_rate(&snap);
        // AUTOTUNE-A3: J/TH via the host-tested pure fn (same formula; now also
        // guards NaN/negative power → f32::MAX so a bad point never wins
        // BestEfficiency).
        let jth = compute_jth(power_w, hashrate);

        let elapsed = self.last_action_time.elapsed().as_secs();
        let current_freq = config.target_frequency;
        let current_voltage = config.target_voltage_mv;

        match self.phase {
            Phase::Warmup => {
                if elapsed >= WARMUP_SECS && hashrate > MIN_HASHRATE_GHS {
                    // ASIC is warm and hashing — start profiling
                    info!(
                        "Autotuner: warmup complete ({:.0} GH/s, {:.0}C). Starting profile.",
                        hashrate, max_temp
                    );
                    self.phase = Phase::Profiling;
                    // Start profiling from a low frequency
                    self.profile_freq = (current_freq * 0.6).max(self.min_freq);
                    self.last_action_time = std::time::Instant::now();
                    self.update_status(state, &format!("profiling {:.0} MHz", self.profile_freq));
                    return Some((self.profile_freq, current_voltage));
                }
                self.update_status(state, &format!("warmup {}s/{:.0}s", elapsed, WARMUP_SECS));
                None
            }

            Phase::Profiling => {
                if elapsed < PROFILE_STEP_INTERVAL {
                    return None; // Wait for hashrate to stabilize
                }

                // AUTOTUNE-7: this tick records a decision point, so close the
                // error-rate measurement window here. `delta_error_rate` above
                // already captured the cumulative rate over the full settle
                // interval; advancing prev_* now starts the next window fresh.
                self.consume_error_counters(&snap);

                // Record this data point
                let point = ProfilePoint {
                    frequency: current_freq,
                    voltage: current_voltage,
                    hashrate_ghs: hashrate,
                    power_w,
                    temp_c: max_temp,
                    jth,
                    delta_error_rate,
                    stable: hashrate > MIN_HASHRATE_GHS
                        && power_w < self.power_limit_w
                        && delta_error_rate <= MAX_ERROR_RATE,
                };

                info!(
                    "Autotuner: profile {:.0} MHz → {:.0} GH/s, {:.1}W, {:.1} J/TH{}",
                    point.frequency,
                    point.hashrate_ghs,
                    point.power_w,
                    point.jth,
                    if point.stable { "" } else { " [UNSTABLE]" }
                );

                let unstable = !point.stable;
                self.profile.push(point);

                if unstable {
                    self.find_best_point(&autotune_state.mode, autotune_state.target_value);
                    self.phase = Phase::Optimizing;
                    self.last_action_time = std::time::Instant::now();

                    if let Some(ref best) = self.best_point {
                        warn!(
                            "Autotuner: unstable profile point, rolling back to {:.0} MHz",
                            best.frequency
                        );
                        self.update_status(state, &format!("rollback {:.0} MHz", best.frequency));
                        return Some((best.frequency, best.voltage));
                    }

                    let fallback = if autotune_state.last_good_frequency > 0.0
                        && autotune_state.last_good_voltage_mv > 0
                    {
                        Some((
                            autotune_state.last_good_frequency,
                            autotune_state.last_good_voltage_mv,
                        ))
                    } else {
                        None
                    };

                    if let Some((freq, voltage)) = fallback {
                        warn!(
                            "Autotuner: unstable first profile point, rolling back to last known good {:.0} MHz",
                            freq
                        );
                        {
                            let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                            at.enabled = false;
                            at.status = format!("disabled: rollback {:.0} MHz", freq);
                            // §7.4(b): terminal disable → honest idle rung (never a
                            // fake "tuned"). Mirrors `self.phase = Phase::Idle` below.
                            at.phase = Phase::Idle.as_str().to_string();
                        }
                        self.phase = Phase::Idle;
                        return Some((freq, voltage));
                    }

                    warn!(
                        "Autotuner: unstable first profile point and no last known good; disabling"
                    );
                    {
                        let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                        at.enabled = false;
                        at.status = "disabled: unstable profile".to_string();
                        // §7.4(b): terminal disable → honest idle rung. Mirrors
                        // `self.phase = Phase::Idle` below.
                        at.phase = Phase::Idle.as_str().to_string();
                    }
                    self.phase = Phase::Idle;
                    return None;
                }

                // Advance to next frequency
                self.profile_freq += PROFILE_FREQ_STEP;

                if self.profile_freq > self.max_freq || power_w > self.power_limit_w * 0.95 {
                    // Profile complete — find best point
                    self.find_best_point(&autotune_state.mode, autotune_state.target_value);
                    // TargetWatts mode enters the closed-loop WattageDescent
                    // before Optimizing; all other modes preserve the original
                    // Profiling → Optimizing transition (byte-identical).
                    self.phase = if autotune_state.mode == AutotuneMode::TargetWatts {
                        info!(
                            "Autotuner: profile complete, entering WattageDescent (target {:.1} W)",
                            autotune_state.target_value
                        );
                        self.descent_iter = 0;
                        self.descent_converged_windows = 0;
                        self.power_window.clear();
                        // AUTOTUNE-8: clear the applied-setpoint memory on every
                        // fresh descent entry. Otherwise a SECOND descent that
                        // converges on its first settle tick WITHOUT applying a
                        // step would persist/return the STALE setpoint left over
                        // from a PRIOR descent (the unwrap_or fallback below only
                        // fires when this is None). Resetting here guarantees the
                        // convergence-exit either persists THIS descent's real
                        // applied point or falls back to the live current config.
                        self.descent_applied_setpoint = None;
                        Phase::WattageDescent
                    } else {
                        Phase::Optimizing
                    };
                    self.last_action_time = std::time::Instant::now();

                    // AUTOTUNE-A1 (TargetTemp strand fix): a mode/target can
                    // match NO stable point — e.g. a TargetTemp ceiling below
                    // EVERY profiled point (`target_temp=0`) — so
                    // `find_best_point` leaves `best_point == None`. Entering
                    // Optimizing with no best_point made the state machine churn
                    // in Optimizing forever and NEVER persist last-known-good or
                    // reach Maintaining. Handle the empty-result as a clean,
                    // fail-safe outcome: hold the last-known-good (if any) and
                    // settle into the honest Idle rung — mirroring the
                    // unstable-profile fallback above. We never invent a
                    // speculative point and never raise freq/voltage.
                    //
                    // Guarded on `Phase::Optimizing` so the TargetWatts ->
                    // WattageDescent path is untouched (it owns its own
                    // best-fit/no-row handling below).
                    if self.phase == Phase::Optimizing && self.best_point.is_none() {
                        self.phase = Phase::Idle;
                        let fallback = if autotune_state.last_good_frequency > 0.0
                            && autotune_state.last_good_voltage_mv > 0
                        {
                            Some((
                                autotune_state.last_good_frequency,
                                autotune_state.last_good_voltage_mv,
                            ))
                        } else {
                            None
                        };
                        {
                            let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                            at.enabled = false;
                            at.status =
                                "disabled: no operating point satisfies mode/target".to_string();
                            // §7.4(b): terminal disable → honest idle rung.
                            at.phase = Phase::Idle.as_str().to_string();
                        }
                        if let Some((freq, voltage)) = fallback {
                            warn!(
                                "Autotuner: no operating point satisfies mode/target; holding last-known-good {:.0} MHz",
                                freq
                            );
                            return Some((freq, voltage));
                        }
                        warn!(
                            "Autotuner: no operating point satisfies mode/target and no last-known-good; disabling"
                        );
                        return None;
                    }

                    if let Some(ref best) = self.best_point {
                        info!("Autotuner: profile complete ({} points). Best: {:.0} MHz, {:.0} GH/s, {:.1} J/TH",
                            self.profile.len(), best.frequency, best.hashrate_ghs, best.jth);
                        self.update_status(state, &format!("optimizing {:.0} MHz", best.frequency));
                        return Some((best.frequency, best.voltage));
                    }
                    return None;
                }

                self.last_action_time = std::time::Instant::now();
                self.update_status(state, &format!("profiling {:.0} MHz", self.profile_freq));
                Some((self.profile_freq, current_voltage))
            }

            Phase::Optimizing => {
                // XPAUTO-2: chip-health backoff takes precedence over the normal
                // fine-tuning logic (default-OFF gate). Runs every tick (not
                // gated by OPTIMIZE_INTERVAL) so a rising HW-error rate is caught
                // promptly, matching DpsWalker's "ahead of all optimization".
                if let Some(retreat) = self.health_backoff_check(
                    state,
                    autotune_state.health_backoff_enabled,
                    delta_error_rate,
                    autotune_state.last_good_frequency,
                    autotune_state.last_good_voltage_mv,
                ) {
                    return Some(retreat);
                }

                if elapsed < OPTIMIZE_INTERVAL {
                    return None;
                }

                self.opt_phase_count += 1;

                // Fine-tune around the best point
                let improved = hashrate > self.prev_hashrate * (1.0 - HASHRATE_DROP_THRESHOLD);
                let within_power = power_w < self.power_limit_w;

                if improved && within_power {
                    // Update best if this is actually better
                    if let Some(ref mut best) = self.best_point {
                        let is_better = match autotune_state.mode {
                            AutotuneMode::MaxHashrate => hashrate > best.hashrate_ghs,
                            AutotuneMode::BestEfficiency => {
                                jth < best.jth && hashrate > MIN_HASHRATE_GHS
                            }
                            AutotuneMode::TargetWatts => {
                                (power_w - autotune_state.target_value).abs()
                                    < (best.power_w - autotune_state.target_value).abs()
                            }
                            AutotuneMode::TargetTemp => {
                                hashrate > best.hashrate_ghs
                                    && max_temp <= autotune_state.target_value
                            }
                        };
                        if is_better {
                            best.frequency = current_freq;
                            best.voltage = current_voltage;
                            best.hashrate_ghs = hashrate;
                            best.power_w = power_w;
                            best.jth = jth;
                            best.temp_c = max_temp;
                            info!(
                                "Autotuner: new best! {:.0} MHz, {:.0} GH/s, {:.1} J/TH",
                                current_freq, hashrate, jth
                            );
                        }
                    }

                    // Try a small step further
                    let next_freq = current_freq + FINE_FREQ_STEP * self.opt_direction as f32;
                    self.prev_hashrate = hashrate;
                    self.prev_power = power_w;
                    self.last_action_time = std::time::Instant::now();

                    if next_freq >= self.min_freq && next_freq <= self.max_freq {
                        self.update_status(state, &format!("fine-tuning {:.0} MHz", next_freq));
                        return Some((next_freq, current_voltage));
                    }
                }

                // Didn't improve or hit a limit — reverse or settle
                if self.opt_direction > 0 {
                    self.opt_direction = -1;
                    self.last_action_time = std::time::Instant::now();
                    if let Some(ref best) = self.best_point {
                        return Some((best.frequency - FINE_FREQ_STEP, best.voltage));
                    }
                } else {
                    // Tried both directions — settle at best
                    if let Some(ref best) = self.best_point {
                        info!(
                            "Autotuner: settled at {:.0} MHz, {} mV ({:.0} GH/s, {:.1} J/TH)",
                            best.frequency, best.voltage, best.hashrate_ghs, best.jth
                        );
                        self.phase = Phase::Maintaining;
                        self.last_action_time = std::time::Instant::now();
                        self.prev_hashrate = best.hashrate_ghs;
                        self.update_status(
                            state,
                            &format!("settled: {:.0} MHz, {:.1} J/TH", best.frequency, best.jth),
                        );
                        return Some((best.frequency, best.voltage));
                    }
                }
                None
            }

            Phase::Maintaining => {
                // XPAUTO-2: chip-health backoff takes precedence over every
                // drift check below (default-OFF gate). Runs every tick (not
                // gated by MAINTAIN_INTERVAL) so a rising HW-error rate is caught
                // promptly, mirroring DpsWalker's "takes precedence over
                // everything". On retreat it re-enters Profiling at the proven
                // last-known-good — never a higher/speculative point.
                if let Some(retreat) = self.health_backoff_check(
                    state,
                    autotune_state.health_backoff_enabled,
                    delta_error_rate,
                    autotune_state.last_good_frequency,
                    autotune_state.last_good_voltage_mv,
                ) {
                    return Some(retreat);
                }

                if elapsed < MAINTAIN_INTERVAL {
                    return None;
                }
                self.last_action_time = std::time::Instant::now();
                // AUTOTUNE-7: this is a Maintaining decision tick — close the
                // error-rate window so the next interval measures fresh.
                self.consume_error_counters(&snap);

                // Check for drift
                if let Some(ref best) = self.best_point {
                    // Power over budget? Back off frequency
                    if power_w > self.power_limit_w * 1.05 {
                        let new_freq = (current_freq - FINE_FREQ_STEP).max(self.min_freq);
                        warn!(
                            "Autotuner: power drift {:.1}W > {:.1}W limit, reducing to {:.0} MHz",
                            power_w, self.power_limit_w, new_freq
                        );
                        self.update_status(state, &format!("power limit: {:.0} MHz", new_freq));
                        return Some((new_freq, current_voltage));
                    }

                    // Temp over safe limit? Back off
                    if max_temp > 80.0 {
                        let new_freq = (current_freq - FINE_FREQ_STEP).max(self.min_freq);
                        warn!(
                            "Autotuner: thermal drift {:.0}C, reducing to {:.0} MHz",
                            max_temp, new_freq
                        );
                        self.update_status(state, &format!("thermal: {:.0} MHz", new_freq));
                        return Some((new_freq, current_voltage));
                    }

                    // Hashrate dropped significantly? Something changed
                    if hashrate < best.hashrate_ghs * (1.0 - HASHRATE_DROP_THRESHOLD * 2.0)
                        && hashrate > MIN_HASHRATE_GHS
                    {
                        warn!(
                            "Autotuner: hashrate dropped {:.0} → {:.0} GH/s, re-profiling",
                            best.hashrate_ghs, hashrate
                        );
                        self.phase = Phase::Profiling;
                        self.profile.clear();
                        self.profile_freq = (current_freq * 0.8).max(self.min_freq);
                        self.update_status(state, "re-profiling...");
                        return Some((self.profile_freq, current_voltage));
                    }

                    if delta_error_rate <= MAX_ERROR_RATE
                        && hashrate > MIN_HASHRATE_GHS
                        && power_w < self.power_limit_w
                    {
                        let silicon_grade =
                            self.silicon_grade(best.jth, delta_error_rate, current_voltage);
                        {
                            let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                            at.last_good_frequency = current_freq;
                            at.last_good_voltage_mv = current_voltage;
                            at.last_good_jth = jth;
                            at.last_good_error_rate = delta_error_rate as f32;
                            at.silicon_grade = silicon_grade.to_string();
                        }
                        // AUTOTUNE-9: only re-persist the last-known-good when the
                        // operating point has actually MOVED since the last write.
                        // A steady, healthy miner sits in Maintaining for days;
                        // rewriting an identical NVS record every 60 s is pure
                        // flash wear. The change-gate is the host-tested pure fn
                        // (FINE_FREQ_STEP dead-band so f32 jitter below the tuner's
                        // own resolution never triggers a write).
                        let (had_prior, prev_f, prev_v) = match self.last_persisted_lkg {
                            Some((f, v)) => (true, f, v),
                            None => (false, 0.0, 0),
                        };
                        if last_known_good_changed(
                            had_prior,
                            prev_f,
                            prev_v,
                            current_freq,
                            current_voltage,
                            FINE_FREQ_STEP,
                        ) {
                            if let Ok(mut nvs_guard) = state.nvs.lock() {
                                if let Some(ref mut nvs) = *nvs_guard {
                                    crate::nvs_config::save_last_known_good(
                                        nvs,
                                        &crate::nvs_config::LastKnownGoodPoint {
                                            board_version: config.board_version.clone(),
                                            asic_model: config.asic_model.clone(),
                                            overclock_enabled: config.overclock_enabled,
                                            frequency_mhz: current_freq,
                                            voltage_mv: current_voltage,
                                            hashrate_30s_ghs: hashrate as f32,
                                            power_w,
                                            jth,
                                            delta_error_rate: delta_error_rate as f32,
                                        },
                                    );
                                    self.last_persisted_lkg = Some((current_freq, current_voltage));
                                }
                            }
                        }
                    }

                    self.update_status(
                        state,
                        &format!(
                            "{:.0} GH/s @ {:.0}MHz {:.1}J/TH err={:.2}%",
                            hashrate,
                            current_freq,
                            jth,
                            delta_error_rate * 100.0
                        ),
                    );
                }
                None
            }

            Phase::WattageDescent => {
                // Push the latest power sample into the rolling window. The
                // sample fields mirror `Telemetry` so any heartbeat snapshot
                // can populate them directly (see
                // `dcentaxe-hal/src/power.rs:153-202`). The window keeps
                // accumulating every tick; the settle gate below only gates the
                // step/convergence DECISION, not sample collection.
                self.power_window.push(PowerSample {
                    timestamp_ms: crate::shared::unix_time_ms(),
                    power_w,
                    voltage_mv: telem.voltage_mv,
                    current_ma: telem.current_ma,
                });

                // AUTOTUNE-1: per-step settle/dwell gate. Every other phase
                // (Profiling 20 s, Optimizing 30 s, Maintaining 60 s) waits for
                // the rail to settle before deciding; WattageDescent had no such
                // guard, so once the window filled it re-commanded a setpoint on
                // every 5 s tick and evaluated convergence over a window blended
                // across the OLD and NEW setpoints. Hold the decision until
                // OPTIMIZE_INTERVAL has elapsed since the last action; keep
                // collecting samples in the meantime.
                if !descent_settle_elapsed_ok(elapsed, OPTIMIZE_INTERVAL) {
                    return None;
                }

                let target = autotune_state.target_value;
                let asic_model = config.asic_model.as_str();

                // Bounded binary search: pick a (freq, voltage) tuple from
                // the chip profile that's predicted to land closer to the
                // wattage target. Returns None when waiting for window fill,
                // when already converged this window, or when the 12-iter
                // cap is reached.
                let proposed = self.step_wattage_descent_v2(target, asic_model);

                // Convergence check: requires 2 consecutive sub-1 %-error
                // windows (≥60 s sustained hold). On success, exit to
                // Optimizing and persist last-known-good via the existing
                // NVS path the Maintaining branch uses.
                if self.try_exit_wattage_descent_v2(target) {
                    self.last_action_time = std::time::Instant::now();
                    // AUTOTUNE-8: persist (and return) the ACTUAL converged
                    // descent setpoint — the (freq, voltage) the descent last
                    // applied — not the pre-step config or the Profiling-sweep
                    // best_point. This makes the saved LKG, the returned/applied
                    // tuple, and the operating point that actually held the
                    // wattage target all identical, so a reboot restores the
                    // point that hit the budget. Falls back to current config
                    // only if no step was ever applied (degenerate: converged on
                    // entry).
                    let (persist_freq, persist_voltage) = self
                        .descent_applied_setpoint
                        .unwrap_or((current_freq, current_voltage));
                    // Persist last-known-good using the same alloc-free NVS
                    // path the Maintaining branch uses.
                    if let Ok(mut nvs_guard) = state.nvs.lock() {
                        if let Some(ref mut nvs) = *nvs_guard {
                            crate::nvs_config::save_last_known_good(
                                nvs,
                                &crate::nvs_config::LastKnownGoodPoint {
                                    board_version: config.board_version.clone(),
                                    asic_model: config.asic_model.clone(),
                                    overclock_enabled: config.overclock_enabled,
                                    frequency_mhz: persist_freq,
                                    voltage_mv: persist_voltage,
                                    hashrate_30s_ghs: hashrate as f32,
                                    power_w,
                                    jth,
                                    delta_error_rate: delta_error_rate as f32,
                                },
                            );
                            self.last_persisted_lkg = Some((persist_freq, persist_voltage));
                        }
                    }
                    self.update_status(state, "wattage target held");
                    return Some((persist_freq, persist_voltage));
                }

                if let Some((freq, voltage_mv)) = proposed {
                    self.last_action_time = std::time::Instant::now();
                    // AUTOTUNE-8: remember the point we are about to apply so the
                    // convergence-exit can persist/return the real held point.
                    self.descent_applied_setpoint = Some((freq, voltage_mv));
                    // AUTOTUNE-1: a fresh setpoint was just commanded — drop the
                    // stale samples so the next convergence check measures ONLY
                    // the new operating point, not a window blended across the
                    // old and new setpoints.
                    self.power_window.clear();
                    self.update_status(state, "wattage descent");
                    return Some((freq, voltage_mv));
                }
                None
            }

            Phase::Idle => None,
        }
    }

    /// XPAUTO-2: chip-health-aware backoff (cross-pollinated from DCENT_OS
    /// `DpsWalker` HealthBackoff, dps_walker.rs:670-686). "Takes precedence over
    /// everything" — call this at the TOP of the optimization/maintenance arms,
    /// BEFORE the existing power/temp/hashrate-drift checks.
    ///
    /// Default-OFF: only fires when `health_backoff_enabled` is set (the opt-in
    /// gate, mirroring `DCENT_BM139X_OPEN_CORE`). When the gate is off this is a
    /// no-op and the autotuner behaves byte-identically to before.
    ///
    /// On a sustained (3-consecutive-tick debounced) rise of `delta_error_rate`
    /// above `MAX_ERROR_RATE`, retreats to the existing last-known-good point
    /// (the SAME pair the Maintaining persist at line ~533 already vetted) by
    /// re-entering `Profiling` at that frequency. Returns `Some((freq, voltage))`
    /// to command the retreat, or `None` to fall through to the normal logic.
    ///
    /// SAFETY: this can only ever command the chip DOWN toward a proven point —
    /// never a higher freq/voltage, never a lowered safety limit. If no
    /// last-known-good has been recorded yet (`> 0.0 && > 0`), it does NOTHING
    /// and falls through (commanding a zero/garbage last-good would be a real
    /// regression — same guard as the first-point rollback at lines ~325-334).
    fn health_backoff_check(
        &mut self,
        state: &SharedState,
        enabled: bool,
        delta_error_rate: f64,
        last_good_frequency: f32,
        last_good_voltage_mv: u16,
    ) -> Option<(f32, u16)> {
        if !enabled {
            return None;
        }
        // Advance the consecutive-over-ceiling debounce counter (host-tested fn).
        self.hw_err_over_ceiling_streak = dcentaxe_hal::safety::hw_error_streak_next(
            delta_error_rate,
            MAX_ERROR_RATE,
            self.hw_err_over_ceiling_streak,
        );
        // The decision is computed ONLY in the host-tested pure fn. Pass the
        // pre-increment streak (saturating_sub(1)) so the predicate counts this
        // tick exactly once.
        if !dcentaxe_hal::safety::hw_error_backoff_should_retreat(
            delta_error_rate,
            MAX_ERROR_RATE,
            self.hw_err_over_ceiling_streak.saturating_sub(1),
            3,
        ) {
            return None;
        }
        // Never retreat to an un-vetted point — require a real last-known-good.
        if !(last_good_frequency > 0.0 && last_good_voltage_mv > 0) {
            return None;
        }
        let retreat_freq = last_good_frequency.max(self.min_freq);
        warn!(
            "Autotuner: HW-error backoff (err={:.2}% > {:.2}% for 3 ticks), retreating to last known good {:.0} MHz, {} mV",
            delta_error_rate * 100.0,
            MAX_ERROR_RATE * 100.0,
            retreat_freq,
            last_good_voltage_mv
        );
        // Reset the streak and the optimization state — the fitted curve is no
        // longer trustworthy (mirrors DpsWalker's fit_window.clear()).
        self.hw_err_over_ceiling_streak = 0;
        self.phase = Phase::Profiling;
        self.profile.clear();
        self.profile_freq = retreat_freq;
        self.last_action_time = std::time::Instant::now();
        self.update_status(state, "health backoff");
        Some((retreat_freq, last_good_voltage_mv))
    }

    // ─── DEFERRED enhancements (markers — do NOT silently implement) ─────────
    //
    // AUTOTUNE-5 (DEFERRED to the shared/api lane): there is no explicit
    // "TargetEfficiency" (hit-a-J/TH-budget) mode. `BestEfficiency` below only
    // globally minimizes J/TH and ignores `target`. Adding the mode requires a
    // new `AutotuneMode::TargetEfficiency` ENUM VARIANT in `shared.rs` plus its
    // `from_api_str`/`as_api_str` mapping and a `MODE_DESCRIPTIONS` row — all
    // outside this lane (autotuner.rs + chip_profiles_bitaxe.rs only). Once the
    // variant exists, the `find_best_point` arm is a one-liner:
    // `max_by(hashrate)` among points with `jth <= target` (mirroring the
    // TargetTemp arm), and the Optimizing `is_better` arm mirrors TargetTemp.
    // Default-preserving: do NOT add it here without the enum.
    //
    // AUTOTUNE-6 / XPAUTO-1 (DEFERRED to live bench): the Profiling/Optimizing
    // search steps FREQUENCY ONLY and always re-passes `current_voltage`, so the
    // V/F surface is explored 1-D and `VOLTAGE_STEP` (line ~75) is currently
    // only referenced by the WattageDescent table path. A voltage-aware
    // (undervolt-at-fixed-freq) search is the biggest remaining J/TH lever, but
    // shipping a speculative undervolt is explicitly bench-gated (XPAUTO-1) —
    // it must be validated on real silicon with the `qualify_operating_point`
    // clamp and a `delta_error_rate > MAX_ERROR_RATE` abort. Do NOT add a
    // speculative voltage-down sweep here; this marker is the scope record.

    /// Find the best operating point from profile data based on mode.
    ///
    /// AUTOTUNE-A1: the MaxHashrate / BestEfficiency / TargetTemp selection is
    /// the host-tested pure `best_point_for_mode` (single source of truth, same
    /// NaN-safe + tie semantics). TargetWatts keeps its own power-distance pick
    /// inline — it needs each point's measured watts (not carried in the pure
    /// fn's row tuple) and is the WattageDescent ENTRY setpoint. Behavior is
    /// byte-identical to the previous inline match for every mode, including the
    /// `best_point = None` overwrite when no point is eligible.
    fn find_best_point(&mut self, mode: &AutotuneMode, target: f32) {
        let stable_points: Vec<&ProfilePoint> = self
            .profile
            .iter()
            .filter(|p| p.stable && p.hashrate_ghs > MIN_HASHRATE_GHS)
            .collect();

        if stable_points.is_empty() {
            warn!("Autotuner: no stable points found in profile");
            return;
        }

        let best: Option<&ProfilePoint> = match mode {
            AutotuneMode::TargetWatts => {
                // NaN-safe comparator: partial_cmp returns None for NaN → Equal.
                fn fcmp<T: PartialOrd>(a: &T, b: &T) -> std::cmp::Ordering {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                }
                stable_points
                    .iter()
                    .min_by(|a, b| fcmp(&(a.power_w - target).abs(), &(b.power_w - target).abs()))
                    .copied()
            }
            AutotuneMode::MaxHashrate | AutotuneMode::BestEfficiency | AutotuneMode::TargetTemp => {
                let rows: Vec<(f32, f32, f64, f32, f32)> = stable_points
                    .iter()
                    .map(|p| {
                        (
                            p.frequency,
                            p.voltage as f32,
                            p.hashrate_ghs,
                            p.jth,
                            p.temp_c,
                        )
                    })
                    .collect();
                // Outer arm already excludes TargetWatts; `_` is TargetTemp.
                // (No `unreachable!` — keep this mining-path fn panic-free.)
                let bpm = match mode {
                    AutotuneMode::MaxHashrate => BestPointMode::MaxHashrate,
                    AutotuneMode::BestEfficiency => BestPointMode::BestEfficiency,
                    _ => BestPointMode::TargetTemp,
                };
                best_point_for_mode(&rows, bpm, target).map(|i| stable_points[i])
            }
        };

        self.best_point = best.map(|p| p.clone());
    }

    // ─── WattageDescent skeleton (W5-F) ───────────────────────────────
    //
    // Skeleton plumbing for the bounded binary-search WattageDescent
    // algorithm. The full algorithm lives in W5-G and writes into
    // `step_wattage_descent` and `wattage_converged`.
    //
    //: NO `format!`,
    // `String::new()`, or `Vec::new()` in any of these helpers — they may
    // be reached from a panicking tick. State lives on the `Autotuner`
    // struct (pre-allocated at `new()`).

    /// Skeleton: try to enter `WattageDescent` from `Profiling` when the
    /// operator-selected mode is `TargetWatts`. Returns `true` on
    /// transition. Currently unused (the transition is inlined in the
    /// `Profiling` complete branch); kept as a public seam for W5-G.
    #[allow(dead_code)]
    fn try_enter_wattage_descent(&mut self, mode: AutotuneMode, target: f32) -> bool {
        if mode == AutotuneMode::TargetWatts && matches!(self.phase, Phase::Profiling) {
            self.phase = Phase::WattageDescent;
            self.descent_iter = 0;
            self.descent_converged_windows = 0;
            self.power_window.clear();
            // AUTOTUNE-8: clear the applied-setpoint memory on entry (see the
            // live Profiling-complete branch) so a converge-on-entry descent can
            // never persist a stale setpoint from a prior descent.
            self.descent_applied_setpoint = None;
            info!("WattageDescent entered (target {:.1} W)", target);
            return true;
        }
        false
    }

    /// W5-F skeleton retained for the public seam — delegates to the
    /// W5-G binary-search implementation when an asic_model is known.
    /// Kept for any downstream caller that doesn't have the model in
    /// hand; callers from `tick()` use `step_wattage_descent_v2` so the
    /// chip-profile lookup can succeed.
    #[allow(dead_code)]
    fn step_wattage_descent(&mut self, _target_watts: f32) -> Option<(f32, u16)> {
        // No asic_model context — the bisect cannot resolve a profile.
        // The live `tick()` path calls `step_wattage_descent_v2` instead.
        None
    }

    /// W5-G: bounded binary-search step.
    ///
    /// Picks a (freq_mhz, voltage_mv) tuple from the asic-model-specific
    /// chip profile whose predicted wattage is closest to `target_watts`.
    /// Lower-voltage tie-breaker on equal predicted wattage.
    ///
    /// Returns `None` when:
    ///   - The rolling window is not yet full (need ≥30 s of samples).
    ///   - The current window is already within 1 % of target — caller's
    ///     `try_exit_wattage_descent_v2` handles convergence latching.
    ///   - The 12 outer-iteration cap has been reached — hold setpoint.
    ///   - The asic_model has no profile table.
    ///   - The proposed point falls outside the single-chip envelope.
    ///
    ///: no `format!`,
    /// `String`, or `Vec` allocation. The `tracing` calls are on the
    /// `log` crate and only allocate inside the log backend itself.
    fn step_wattage_descent_v2(
        &mut self,
        target_watts: f32,
        asic_model: &str,
    ) -> Option<(f32, u16)> {
        if !self.power_window.is_full() {
            // Wait for full ≥30 s window before deciding.
            return None;
        }

        let measured = self.power_window.mean_power_w();
        let error = (measured - target_watts).abs() / target_watts.max(0.001);

        if error < 0.01 {
            // Converged this window; try_exit handles confirmation latching.
            return None;
        }

        if self.descent_iter >= 12 {
            // Hit iteration cap — stop searching, hold current setpoint.
            warn!(
                "WattageDescent: 12-iter cap reached, holding (measured {:.2} W, target {:.2} W)",
                measured, target_watts
            );
            return None;
        }

        self.descent_iter = self.descent_iter.saturating_add(1);

        // Resolve chip profile by asic_model.
        let profile_table = match Self::lookup_bitaxe_chip_profile(asic_model) {
            Some(t) => t,
            None => {
                warn!(
                    "WattageDescent: no profile for asic_model, holding (iter {})",
                    self.descent_iter
                );
                return None;
            }
        };

        // XPAUTO-4: pick the closest-watts row WITHIN the operator's
        // freq/voltage band (host-tested pure fn). DCENT_OS controllers always
        // clamp proposals to the operator band (PowerTargetController /
        // DpsWalker.clamp_freq); the axe descent previously reasoned about and
        // logged rows the downstream `qualify_operating_point` would re-clamp,
        // wasting iterations and emitting misleading "→ {freq} MHz" logs. The
        // band-restricted search reasons within the band the operator actually
        // selected. Lower-voltage (then lower-freq) tie-breaker, deterministic.
        let best = best_fit_row_in_band(
            target_watts,
            profile_table,
            self.min_freq,
            self.max_freq,
            self.min_voltage,
            self.max_voltage,
        );

        let (target_freq, target_voltage, _watts) = match best {
            Some(row) => row,
            None => {
                warn!(
                    "WattageDescent: no in-band profile row for target {:.2} W (band {:.0}-{:.0} MHz / {}-{} mV), holding (iter {})",
                    target_watts,
                    self.min_freq,
                    self.max_freq,
                    self.min_voltage,
                    self.max_voltage,
                    self.descent_iter
                );
                return None;
            }
        };

        // Hard envelope guard. Refuse anything outside per-chip bounds even
        // if the table somehow contains it (defence-in-depth — the table
        // tests already reject this, but the runtime check stays as the
        // last line before HAL.set_voltage).
        if target_voltage > SINGLE_CHIP_VDD_MAX || target_voltage < SINGLE_CHIP_VDD_MIN {
            error!(
                "WattageDescent: voltage {:.3} V out of single-chip envelope [{:.2}, {:.2}], refusing",
                target_voltage, SINGLE_CHIP_VDD_MIN, SINGLE_CHIP_VDD_MAX
            );
            return None;
        }
        if target_freq > BITAXE_MAX_FREQ_MHZ {
            error!(
                "WattageDescent: freq {} MHz above BITAXE_MAX_FREQ_MHZ {}, refusing",
                target_freq, BITAXE_MAX_FREQ_MHZ
            );
            return None;
        }

        let target_voltage_mv = (target_voltage * 1000.0) as u16;

        info!(
            "WattageDescent step iter={} target={:.2} W measured={:.2} W → {} MHz, {:.3} V",
            self.descent_iter, target_watts, measured, target_freq, target_voltage
        );

        Some((target_freq as f32, target_voltage_mv))
    }

    /// Resolve the per-chip BitAxe profile for an asic_model string.
    /// Returns `None` for unknown models so the caller can refuse the
    /// step rather than guess at envelopes.
    fn lookup_bitaxe_chip_profile(asic_model: &str) -> Option<&'static [(u32, f32, f32)]> {
        match asic_model {
            "BM1366" => Some(BM1366_BITAXE_PROFILE),
            "BM1368" => Some(BM1368_BITAXE_PROFILE),
            "BM1370" => Some(BM1370_BITAXE_PROFILE),
            // AUTOTUNE-3: BitAxe Max (BM1397) now has a descent profile, so
            // TargetWatts is a functional closed loop instead of logging
            // "no profile, holding" every window and exiting only by luck.
            "BM1397" => Some(BM1397_BITAXE_PROFILE),
            _ => None,
        }
    }

    /// W5-F skeleton retained for the public seam — delegates to v2.
    #[allow(dead_code)]
    fn wattage_converged(&self, _target_watts: f32) -> bool {
        false
    }

    /// W5-G: convergence test for one window.
    ///
    /// Returns true iff the rolling window is full AND the mean wattage
    /// is within 1 % of target. Caller is responsible for tracking the
    /// 2-consecutive-window confirmation requirement (see
    /// `try_exit_wattage_descent_v2`).
    fn wattage_converged_v2(&self, target_watts: f32) -> bool {
        if !self.power_window.is_full() {
            return false;
        }
        let measured = self.power_window.mean_power_w();
        let error = (measured - target_watts).abs() / target_watts.max(0.001);
        error < 0.01
    }

    /// W5-F skeleton retained for the public seam.
    #[allow(dead_code)]
    fn try_exit_wattage_descent(&mut self, target_watts: f32) -> bool {
        self.try_exit_wattage_descent_v2(target_watts)
    }

    /// W5-G: exit `WattageDescent` to `Optimizing` only after 2 consecutive
    /// 30 s windows have stayed within 1 % of target (≥60 s sustained hold).
    ///
    /// Resets `descent_converged_windows` to 0 on any non-convergent window.
    /// Returns `true` exactly once — the tick that completes the 2-window
    /// confirmation. The phase transition reuses the existing
    /// `Optimizing → Maintaining → last_known_good NVS persist` pipeline
    /// (autotuner.rs Maintaining branch lines 526-541) — no new path.
    fn try_exit_wattage_descent_v2(&mut self, target_watts: f32) -> bool {
        if !matches!(self.phase, Phase::WattageDescent) {
            return false;
        }
        if !self.wattage_converged_v2(target_watts) {
            self.descent_converged_windows = 0;
            return false;
        }
        self.descent_converged_windows = self.descent_converged_windows.saturating_add(1);
        if self.descent_converged_windows >= 2 {
            self.phase = Phase::Optimizing;
            info!(
                "WattageDescent converged at {:.2} W (target {:.2} W) after {} iterations",
                self.power_window.mean_power_w(),
                target_watts,
                self.descent_iter
            );
            return true;
        }
        false
    }

    /// Return power limit as a display string for the OLED carousel.
    pub fn power_limit_display(&self) -> String {
        format!("{:.0}W max", self.power_limit_w)
    }

    fn update_status(&self, state: &SharedState, status: &str) {
        let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
        at.status = status.to_string();
        // data-model-fields §7.4(b): keep the stable phase token in lockstep with
        // every human-status update. `self.phase` is the engine's authoritative
        // stage at this call site, so writing it here covers every update_status()
        // caller automatically (warmup/profiling/optimizing/maintaining/idle).
        at.phase = self.phase.as_str().to_string();
        if let Some(ref best) = self.best_point {
            at.current_frequency = best.frequency;
            at.current_voltage_mv = best.voltage;
            at.best_efficiency = best.jth;
        }
    }

    /// AUTOTUNE-7: compute the HW-error rate over the window since the LAST
    /// consumed decision point WITHOUT advancing the prev counters. Called every
    /// tick to surface a current estimate, but it is non-destructive — the
    /// counters are only advanced by `consume_error_counters` at an actual
    /// decision (Profiling record / Maintaining health check), so the rate baked
    /// into a recorded point spans the full settle interval, not the last 5 s.
    fn peek_delta_error_rate(&self, snap: &dcentaxe_mining::stats::MiningStatsSnapshot) -> f64 {
        let delta_nonces = snap.nonces_found.saturating_sub(self.prev_nonces);
        let delta_errors = snap.rejected_shares.saturating_sub(self.prev_errors);
        if delta_nonces == 0 {
            0.0
        } else {
            delta_errors as f64 / delta_nonces as f64
        }
    }

    /// AUTOTUNE-7: advance the prev nonce/error counters to "now", closing the
    /// measurement window. Call this exactly once at each decision point that
    /// CONSUMED the peeked rate (records a ProfilePoint or runs the Maintaining
    /// health check), so the next window starts fresh from this point.
    fn consume_error_counters(&mut self, snap: &dcentaxe_mining::stats::MiningStatsSnapshot) {
        self.prev_nonces = snap.nonces_found;
        self.prev_errors = snap.rejected_shares;
    }

    /// AUTOTUNE-A2: thin delegate to the host-tested pure
    /// `chip_profiles_bitaxe::silicon_grade` (single source of truth for the
    /// gold/strong/normal/spicy thresholds). Kept as a method so the call site
    /// stays `self.silicon_grade(..)`.
    fn silicon_grade(&self, jth: f32, delta_error_rate: f64, voltage_mv: u16) -> &'static str {
        crate::chip_profiles_bitaxe::silicon_grade(jth, delta_error_rate, voltage_mv)
    }
}
