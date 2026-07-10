// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — Factory self-test suite
//
// Pragmatic port of ESP-Miner `main/self_test/self_test.c` — the subset of
// checks that let us answer "is this board healthy?" end-to-end without
// special test fixtures. Triggered via `POST /api/system/self-test/run`;
// results are surfaced via `GET /api/system/self-test/status`.
//
// Steps (mirrors upstream order where practical):
//  1. Voltage rail — input VIN inside board-spec window
//  2. Core voltage — measured VOUT close to target
//  3. ASIC chain — every configured chip is producing real nonces (AUX-7:
//     evidence-gated — an error-only or dark chip does NOT count as active)
//  4. Temperature sensor — at least one sensor is returning valid data
//  5. Fan tachometer — RPM > 0 whenever the fan is commanded above 30 %
//  6. Mining liveness — at least one accepted share since boot
//
// `passed` means "no step Failed or was Aborted" (a Skip — e.g. fan tach when
// the fan is legitimately commanded ≤30 % on a quiet home unit — does not Fail,
// so it does not block `passed`). `passed` is therefore a LIVENESS signal, not a
// full safety certification: it does not assert thermal-shutdown / voltage-cap
// wiring, and a fan-tach Skip means the fan was not verified this run. The HAL
// enforces the fail-closed thermal/voltage safety limits independently.
//
// AUX-7: this module does NOT write any `factory_sealed` NVS seal (an earlier
// comment here claimed it did — it never existed). The only consumer of
// `snap.passed` is the BAP auto-reboot on a Touch / Turbo Touch variant
// (api.rs), handled by the caller, not this module.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::shared::SharedState;

/// Canonical ordered step list. Keep in sync with `SelfTestRunner::run_all()`.
pub const STEP_NAMES: &[&str] = &[
    "input_voltage",
    "core_voltage",
    "asic_chain",
    "temp_sensor",
    "fan_tach",
    "mining_liveness",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pass,
    Fail,
    Skip,
    Aborted,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelfTestResult {
    pub name: &'static str,
    pub status: StepStatus,
    pub detail: String,
}

impl SelfTestResult {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Pass,
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Fail,
            detail: detail.into(),
        }
    }
    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: StepStatus::Skip,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SelfTestReport {
    pub running: bool,
    pub completed: bool,
    pub passed: bool,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub results: Vec<SelfTestResult>,
}

/// Shared runner state. Held in `SharedState::self_test` so the REST handler
/// can kick a run off and the dashboard / MCP can poll `/status`.
#[derive(Debug, Default)]
pub struct SelfTestRunner {
    report: Mutex<SelfTestReport>,
    /// Set by `request_cancel()` to abort the run at the next step boundary.
    /// Polled by `run_all()` so the cancel lands cleanly between steps rather
    /// than mid-I2C.
    cancel_requested: AtomicBool,
}

impl SelfTestRunner {
    pub fn snapshot(&self) -> SelfTestReport {
        self.report.lock().map(|r| r.clone()).unwrap_or_default()
    }

    pub fn is_running(&self) -> bool {
        self.report.lock().map(|r| r.running).unwrap_or(false)
    }

    /// Begin a new run. Returns `false` if another run is still in flight.
    pub fn begin(&self) -> bool {
        let mut report = match self.report.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if report.running {
            return false;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *report = SelfTestReport {
            running: true,
            completed: false,
            passed: false,
            started_unix_ms: crate::shared::unix_time_ms(),
            finished_unix_ms: 0,
            results: Vec::with_capacity(STEP_NAMES.len()),
        };
        true
    }

    /// Flip the cancel flag. The currently-running step finishes normally
    /// (its I/O is already in flight) but no further steps execute. Final
    /// report reflects `aborted: true` when the run completes.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    fn should_cancel(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    fn push(&self, r: SelfTestResult) {
        if let Ok(mut guard) = self.report.lock() {
            guard.results.push(r);
        }
    }

    fn finish(&self) {
        if let Ok(mut guard) = self.report.lock() {
            guard.running = false;
            guard.completed = true;
            // Aborted runs don't count as a pass even if every completed step
            // was green — we don't know what the missing steps would have said.
            let any_failed_or_aborted = guard
                .results
                .iter()
                .any(|r| matches!(r.status, StepStatus::Fail | StepStatus::Aborted));
            guard.passed = !any_failed_or_aborted;
            guard.finished_unix_ms = crate::shared::unix_time_ms();
        }
    }

    /// Run the full 6-step suite by sampling the current `SharedState`.
    ///
    /// All steps here are **non-invasive reads** from shared telemetry — they
    /// assume the mining loop has been running long enough for the relevant
    /// fields to populate. The `wait_for_share_secs` argument lets callers
    /// give the mining liveness step extra headroom after a cold boot.
    pub fn run_all(&self, state: &SharedState, wait_for_share_secs: u64) {
        if !self.begin() {
            return;
        }

        // Shorthand — every step checks this before doing any work, so a
        // `POST /api/system/self-test/cancel` lands cleanly between steps.
        macro_rules! bail_if_cancelled {
            ($self:ident, $name:expr) => {
                if $self.should_cancel() {
                    $self.push(SelfTestResult {
                        name: $name,
                        status: StepStatus::Aborted,
                        detail: "cancelled by operator".into(),
                    });
                    $self.finish();
                    return;
                }
            };
        }
        bail_if_cancelled!(self, "input_voltage");
        // Step 1 — VIN sanity. Covers both 5 V (Max/Ultra/Supra/Gamma) and 12 V
        // (GT/Hex/Touch) rails with an explicit two-band check.
        {
            let vin_mv = state
                .telemetry
                .lock()
                .map(|t| t.input_voltage_mv as u32)
                .unwrap_or(0);
            if vin_mv == 0 {
                self.push(SelfTestResult::skip(
                    "input_voltage",
                    "no INA260 reading — skip (not present on this board?)",
                ));
            } else {
                let in_5v = (4_500..=5_500).contains(&vin_mv);
                let in_12v = (10_800..=13_200).contains(&vin_mv);
                if in_5v || in_12v {
                    self.push(SelfTestResult::pass(
                        "input_voltage",
                        format!("{} mV ({} V rail)", vin_mv, if in_12v { 12 } else { 5 }),
                    ));
                } else {
                    self.push(SelfTestResult::fail(
                        "input_voltage",
                        format!("VIN {} mV outside 5 V or 12 V windows", vin_mv),
                    ));
                }
            }
        }

        bail_if_cancelled!(self, "core_voltage");
        // Step 2 — Core voltage close to target.
        {
            let (measured, target) = {
                let telem = state.telemetry.lock();
                let cfg = state.config.lock();
                let target = cfg.as_ref().map(|c| c.target_voltage_mv).unwrap_or(0);
                let measured = telem.as_ref().map(|t| t.voltage_mv as u32).unwrap_or(0);
                (measured, target as u32)
            };
            if measured == 0 {
                self.push(SelfTestResult::skip("core_voltage", "no VOUT reading"));
            } else if target == 0 {
                self.push(SelfTestResult::skip(
                    "core_voltage",
                    "target_voltage_mv not configured",
                ));
            } else {
                let diff = (measured as i32 - target as i32).abs();
                if diff <= 60 {
                    self.push(SelfTestResult::pass(
                        "core_voltage",
                        format!(
                            "measured {} mV, target {} mV (±{} mV)",
                            measured, target, diff
                        ),
                    ));
                } else {
                    self.push(SelfTestResult::fail(
                        "core_voltage",
                        format!(
                            "measured {} mV off target {} mV by {} mV (>60 mV)",
                            measured, target, diff
                        ),
                    ));
                }
            }
        }

        bail_if_cancelled!(self, "asic_chain");
        // Step 3 — ASIC chain detected-vs-expected.
        //
        // AUX-7: a chip is "active" ONLY if it returned at least one real nonce.
        // The old filter `nonces > 0 || c.errors > 0` counted an error-only chip
        // (errors>0, nonces==0 — i.e. it answers UART but never produces valid
        // work) as healthy, and a dark/hung chip (neither nonces nor errors) was
        // invisible to the `actual < expected` check. Requiring real nonces makes
        // the PASS evidence-gated: it means every expected chip is genuinely
        // hashing, not merely on the bus.
        {
            let (expected, healthy, error_only) = {
                let cfg = state.config.lock();
                let expected = cfg.as_ref().map(|c| c.asic_count).unwrap_or(1) as u32;
                let (healthy, error_only) = state
                    .stats
                    .lock()
                    .map(|s| {
                        let healthy = s.per_chip.iter().filter(|c| c.nonces > 0).count() as u32;
                        // Chips on the bus that have ONLY produced errors so far —
                        // surfaced in the detail so a half-broken chain is visible.
                        let error_only = s
                            .per_chip
                            .iter()
                            .filter(|c| c.nonces == 0 && c.errors > 0)
                            .count() as u32;
                        (healthy, error_only)
                    })
                    .unwrap_or((0, 0));
                (expected, healthy, error_only)
            };
            if asic_chain_is_healthy(expected, healthy) {
                self.push(SelfTestResult::pass(
                    "asic_chain",
                    format!("{healthy} chip(s) producing nonces"),
                ));
            } else if healthy == 0 {
                self.push(SelfTestResult::fail(
                    "asic_chain",
                    format!(
                        "no chips producing nonces (expected {expected}, {error_only} error-only)"
                    ),
                ));
            } else {
                self.push(SelfTestResult::fail(
                    "asic_chain",
                    format!(
                        "only {healthy} of {expected} chips producing nonces \
                         ({error_only} error-only, rest dark)"
                    ),
                ));
            }
        }

        bail_if_cancelled!(self, "temp_sensor");
        // Step 4 — temperature sensor liveness.
        {
            let (sensors_ok, chip, board) = {
                let telem = state.telemetry.lock();
                telem
                    .as_ref()
                    .map(|t| (t.sensors_ok, t.chip_temp_c, t.board_temp_c))
                    .unwrap_or((false, 0.0, 0.0))
            };
            if !sensors_ok {
                self.push(SelfTestResult::fail(
                    "temp_sensor",
                    "no temperature sensor returning valid data",
                ));
            } else {
                self.push(SelfTestResult::pass(
                    "temp_sensor",
                    format!("chip {:.1}°C, board {:.1}°C", chip, board),
                ));
            }
        }

        bail_if_cancelled!(self, "fan_tach");
        // Step 5 — fan tach plausibility.
        //
        // AUX-7: the decision is the pure `fan_tach_verdict`. The improvement over
        // the old logic: even when the fan is commanded ≤30 % we OPPORTUNISTICALLY
        // PASS if the tach already reports RPM > 0 (positive proof the fan spins),
        // instead of always Skipping. We still Skip — never Fail — when the fan is
        // low AND the tach reads 0, because that is genuinely ambiguous on a quiet
        // home unit (could be a legitimately-stopped fan or a stalled one), and a
        // false Fail there would wrongly fail a healthy quiet board. Commanded
        // >30 % with 0 RPM stays a hard Fail (stalled fan under load).
        {
            let (speed_pct, rpm) = {
                let telem = state.telemetry.lock();
                telem
                    .as_ref()
                    .map(|t| (t.fan_speed_pct, t.fan_rpm))
                    .unwrap_or((0, 0))
            };
            match fan_tach_verdict(speed_pct, rpm) {
                FanTachVerdict::Skip => self.push(SelfTestResult::skip(
                    "fan_tach",
                    format!(
                        "fan at {} % with 0 RPM — below tach threshold, not verified",
                        speed_pct
                    ),
                )),
                FanTachVerdict::Fail => self.push(SelfTestResult::fail(
                    "fan_tach",
                    format!("fan commanded to {} % but tach reports 0 RPM", speed_pct),
                )),
                FanTachVerdict::Pass => self.push(SelfTestResult::pass(
                    "fan_tach",
                    format!("{} % → {} RPM", speed_pct, rpm),
                )),
            }
        }

        bail_if_cancelled!(self, "mining_liveness");
        // Step 6 — mining liveness (at least one share since boot). Give the
        // caller-specified grace period by polling in 1 s increments.
        {
            let poll_start = std::time::Instant::now();
            let mut passed = false;
            let mut cancelled = false;
            while poll_start.elapsed().as_secs() <= wait_for_share_secs {
                if self.should_cancel() {
                    cancelled = true;
                    break;
                }
                let accepted = state.stats.lock().map(|s| s.accepted).unwrap_or(0);
                if accepted > 0 {
                    passed = true;
                    self.push(SelfTestResult::pass(
                        "mining_liveness",
                        format!("{} share(s) accepted", accepted),
                    ));
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            if cancelled {
                self.push(SelfTestResult {
                    name: "mining_liveness",
                    status: StepStatus::Aborted,
                    detail: "cancelled by operator".into(),
                });
                self.finish();
                return;
            }
            if !passed {
                self.push(SelfTestResult::fail(
                    "mining_liveness",
                    format!(
                        "no accepted shares in {} s — check pool / ASIC comms",
                        wait_for_share_secs
                    ),
                ));
            }
        }

        self.finish();
    }
}

/// AUX-7: pure fan-tach decision so the evidence-gating is host-testable and the
/// espidf-only `run_all` plumbing can never disagree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FanTachVerdict {
    /// Tach proves the fan spins (RPM > 0). Positive evidence — always a Pass,
    /// even at low commanded speed.
    Pass,
    /// Fan commanded above the tach threshold but tach reads 0 — stalled under
    /// load. Hard Fail (blocks the suite pass).
    Fail,
    /// Fan commanded ≤30 % AND tach reads 0 — genuinely ambiguous on a quiet home
    /// unit. Skip (does NOT fail), but NOT a verified pass either.
    Skip,
}

/// Fan-tach plausibility. `Pass` on any positive RPM; `Fail` only when commanded
/// above the threshold with 0 RPM; `Skip` when low-and-silent (unverifiable).
pub(crate) fn fan_tach_verdict(speed_pct: u8, rpm: u32) -> FanTachVerdict {
    const TACH_THRESHOLD_PCT: u8 = 30;
    if rpm > 0 {
        // Positive evidence the fan physically spins — accept regardless of the
        // commanded duty (covers quiet home units commanded ≤30 % too).
        FanTachVerdict::Pass
    } else if speed_pct > TACH_THRESHOLD_PCT {
        FanTachVerdict::Fail
    } else {
        FanTachVerdict::Skip
    }
}

/// AUX-7: pure ASIC-chain health decision. A chip is healthy ONLY if it produced
/// at least one real nonce; an error-only chip is NOT counted as active and a
/// dark chip is a deficit. `Ok(())` means every expected chip is hashing.
pub(crate) fn asic_chain_is_healthy(expected: u32, healthy_chips: u32) -> bool {
    healthy_chips > 0 && healthy_chips >= expected
}

#[cfg(test)]
mod liveness_contract {
    use super::*;

    // AUX-7: fan-tach is Pass on real RPM, Fail only under-load-with-0, Skip when
    // low-and-silent (never a false Fail on a quiet unit).
    #[test]
    fn fan_tach_verdict_is_evidence_gated() {
        // Positive RPM always passes, even commanded low (quiet unit, spinning).
        assert_eq!(fan_tach_verdict(0, 1200), FanTachVerdict::Pass);
        assert_eq!(fan_tach_verdict(30, 800), FanTachVerdict::Pass);
        assert_eq!(fan_tach_verdict(100, 4000), FanTachVerdict::Pass);
        // Commanded above threshold but stalled → hard Fail.
        assert_eq!(fan_tach_verdict(60, 0), FanTachVerdict::Fail);
        assert_eq!(fan_tach_verdict(31, 0), FanTachVerdict::Fail);
        // Low and silent → Skip (ambiguous), NEVER Fail.
        assert_eq!(fan_tach_verdict(0, 0), FanTachVerdict::Skip);
        assert_eq!(fan_tach_verdict(30, 0), FanTachVerdict::Skip);
    }

    // AUX-7: an error-only or dark chip must NOT seal asic_chain as healthy.
    #[test]
    fn asic_chain_requires_real_nonces_from_every_chip() {
        // All expected chips producing nonces → healthy.
        assert!(asic_chain_is_healthy(6, 6));
        assert!(asic_chain_is_healthy(1, 1));
        // No chip producing nonces (e.g. all error-only or all dark) → NOT healthy.
        assert!(!asic_chain_is_healthy(6, 0));
        assert!(!asic_chain_is_healthy(1, 0));
        // Some chips dark (healthy < expected) → NOT healthy.
        assert!(!asic_chain_is_healthy(6, 5));
        assert!(!asic_chain_is_healthy(2, 1));
    }
}
