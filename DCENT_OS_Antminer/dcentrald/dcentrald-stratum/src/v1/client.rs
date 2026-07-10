//! Stratum V1 client: async Tokio task managing pool connection lifecycle.
//!
//! The client runs as a long-lived async task. It:
//! 1. Connects to the primary pool (with failover)
//! 2. Performs the handshake (configure, subscribe, authorize)
//! 3. Receives jobs via mining.notify and sends them to the job dispatcher
//! 4. Receives shares from the share validator and submits them to the pool
//! 5. Handles donation time-switching (transparent, voluntary 2% default)
//! 6. Reconnects with exponential backoff on disconnect
//!
//! Communication with the rest of dcentrald is via typed mpsc channels.

use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use super::connection::{
    parse_pool_endpoint, Backoff, ConnectionError, PoolEndpoint, StratumConnection,
};
use super::messages::*;
use crate::pool_failover::{FailoverAction, PoolFailoverFsm};
use crate::types::*;
use crate::version_mask::{format_version_mask, parse_and_clamp_version_mask, parse_version_mask};
use crate::work::{difficulty_to_target, WorkBuilder};
use dcentrald_api_types::luxos_pool_failover::{LuxosFailoverTrigger, LuxosPoolFailoverConfig};

/// Pool-failover robustness — increment 1: stable-primary-return
/// anti-flap decision (pure → deterministically unit-tested).
///
/// Returns `true` only when ALL hold: we are NOT on the primary
/// (`current_pool_index != 0`); the feature is enabled (`window` > 0);
/// the primary previously failed (so we know when its cool-down
/// started); and that cool-down has fully elapsed. Every primary
/// re-attempt re-arms the cool-down on the next primary failure, so the
/// primary can never oscillate faster than `window` (configured far
/// longer than any reconnect backoff). Conservative by construction.
fn should_prefer_primary_return(
    current_pool_index: usize,
    last_primary_failure_at: Option<Instant>,
    window: Duration,
    now: Instant,
) -> bool {
    if current_pool_index == 0 || window.is_zero() {
        return false;
    }
    match last_primary_failure_at {
        Some(failed_at) => now.saturating_duration_since(failed_at) >= window,
        None => false,
    }
}

/// Pool-failover robustness — increment 2: no-`mining.notify` failover
/// decision (pure → deterministically unit-tested).
///
/// `true` only when the feature is enabled (`timeout` > 0) AND no new
/// job has arrived for at least `timeout`. `last_notify_at` is reset to
/// "now" at handshake completion and on every `mining.notify`, so a
/// quiet-but-healthy pool that still pushes periodic jobs never trips
/// it; the conservative default (≫ stall ≫ normal cadence) prevents
/// false failover. The session-fail it drives reuses the existing
/// consecutive-failure/backoff machinery (no parallel path).
/// G36 observe-only shadow decision (pure; host-tested). Re-syncs the FSM's
/// active pool to `active` ONLY when it changed (preserving the per-pool error
/// accumulation within a pool session), then observes `trigger` and returns the
/// `FailoverAction` the FSM WOULD take. The caller logs the result without
/// acting — the existing failover logic still drives real pool selection.
/// Pure over the `PoolFailoverFsm` so it is unit-testable without constructing a
/// `StratumV1Client`.
fn shadow_failover_observe(
    fsm: &mut PoolFailoverFsm,
    active: usize,
    trigger: LuxosFailoverTrigger,
) -> FailoverAction {
    if fsm.active_pool() != Some(active) {
        fsm.set_active(active);
    }
    fsm.observe(active, trigger)
}

fn no_notify_failover_due(last_notify_at: Instant, timeout: Duration, now: Instant) -> bool {
    !timeout.is_zero() && now.saturating_duration_since(last_notify_at) >= timeout
}

/// SW-03: Bitcoin consensus ntime drift window, in seconds. A block header's
/// timestamp must be within roughly ±2 hours of the network-adjusted time, so a
/// share whose ntime drifts further than this from the pool's job ntime cannot
/// possibly be accepted — submitting it only burns a pool reject slot (and, on
/// reject-rate-failover-enabled setups, risks a false failover). 7200 s = 2 h.
const NTIME_VALIDITY_WINDOW_SECS: u32 = 7200;

/// SW-03: validate a share's ntime is within `window_secs` of the job's ntime
/// (pure → host-tested). `share_ntime_hex` is the 8-hex-char big-endian u32
/// timestamp the ASIC mined with; `job_ntime` is the pool-supplied job ntime.
///
/// Returns:
/// - `Ok(true)`  — within the window, safe to submit.
/// - `Ok(false)` — parsed but drifted beyond ±`window_secs`; drop pre-submit.
/// - `Err(())`   — unparseable ntime hex; the caller treats this as "cannot
///   prove out-of-window" and submits anyway (fail-open: never silently drop a
///   share over a parse quirk — the original behavior was to always submit).
///
/// Ntime rolling is legitimate (BIP for ntime rolling lets the miner advance
/// the timestamp), so we permit forward drift up to the window; the window is
/// symmetric to also catch a stale share that lingered in a queue across a long
/// stall. Saturating arithmetic keeps the comparison panic-free at the u32 ends.
fn ntime_within_window(
    share_ntime_hex: &str,
    job_ntime: u32,
    window_secs: u32,
) -> Result<bool, ()> {
    let share_ntime =
        u32::from_str_radix(share_ntime_hex.trim().trim_start_matches("0x"), 16).map_err(|_| ())?;
    let drift = share_ntime.abs_diff(job_ntime);
    Ok(drift <= window_secs)
}

/// SW-09: build the `PoolEndpoint` for a pool-directed `client.reconnect`,
/// PRESERVING the originating session's TLS flag (pure → host-tested).
///
/// The prior implementation hardcoded `tls: false` here, silently downgrading
/// a TLS pool to plaintext on every reconnect. A pool that redirects us must
/// keep encryption: if the session we were redirected from was TLS, the
/// redirected connection stays TLS.
fn pool_directed_reconnect_endpoint(host: String, port: u16, tls: bool) -> PoolEndpoint {
    PoolEndpoint { host, port, tls }
}

/// SEC: the effective wait before honoring a pool-directed `client.reconnect`
/// (pure → host-tested). Never shorter than the current backoff step
/// (`backoff_floor`), so a hostile / MITM plaintext pool answering every
/// handshake with `client.reconnect [attacker_host, wait=0]` cannot trigger an
/// unthrottled reconnect storm to an attacker-chosen host (a remotely-triggerable
/// mining outage). A legitimate pool's longer requested wait is still honored.
fn reconnect_wait_floor(pool_wait_secs: u32, backoff_floor: Duration) -> Duration {
    Duration::from_secs(pool_wait_secs as u64).max(backoff_floor)
}

/// Pool-failover robustness — increment 3: reject-rate failover
/// decision (pure → deterministically unit-tested). OPT-IN: `false`
/// when `threshold_pct == 0` (disabled — the default, because no
/// universally-safe threshold exists and a wrong one flaps on normal
/// vardiff). Also `false` until ≥ `min_samples` post-handshake shares
/// (no acting on transient warm-up/vardiff blips). Then `true` iff the
/// session reject-rate ≥ `threshold_pct`. Zero-total safe.
///
/// SW-07 (2026-06-02) recommended conservative opt-in values for operators
/// who DO want reject-rate failover: `reject_rate_failover_pct = 20`,
/// `reject_rate_failover_min_samples = 50`. 20% over ≥50 settled shares is a
/// clearly-pathological session (a healthy pool sits near 0-1% reject) while
/// the 50-sample floor avoids reacting to a vardiff step or warm-up blip.
/// **These remain OPT-IN, not the compiled default** — per the live-hardware
/// default principle the shipped default stays `pct = 0` (disabled), because
/// auto-failover on reject rate is an active behavior change and the right
/// threshold is deployment-specific. The `reject_rate_recommended_*` tests
/// below pin that these recommended values behave conservatively.
fn reject_rate_failover_due(
    session_accepted: u64,
    session_rejected: u64,
    min_samples: u64,
    threshold_pct: u8,
) -> bool {
    if threshold_pct == 0 {
        return false;
    }
    let total = session_accepted.saturating_add(session_rejected);
    if total == 0 || total < min_samples.max(1) {
        return false;
    }
    session_rejected.saturating_mul(100) / total >= threshold_pct as u64
}

#[cfg(test)]
mod reject_rate_failover_tests {
    use super::reject_rate_failover_due;

    #[test]
    fn disabled_when_pct_zero() {
        assert!(!reject_rate_failover_due(0, 1000, 1, 0));
    }

    #[test]
    fn false_below_min_samples() {
        // 90% reject but only 10 samples, min 100 → no action
        assert!(!reject_rate_failover_due(1, 9, 100, 50));
    }

    #[test]
    fn false_when_zero_total() {
        assert!(!reject_rate_failover_due(0, 0, 100, 50));
    }

    #[test]
    fn false_below_threshold_with_samples() {
        // 20% reject over 200 samples, threshold 50 → no action
        assert!(!reject_rate_failover_due(160, 40, 100, 50));
    }

    #[test]
    fn true_at_threshold_with_enough_samples() {
        // exactly 50% over 200, threshold 50 → fail over
        assert!(reject_rate_failover_due(100, 100, 100, 50));
        // 80% over 500, threshold 50 → fail over
        assert!(reject_rate_failover_due(100, 400, 100, 50));
    }

    // SW-07: the recommended conservative OPT-IN values (pct=20, min_samples=50)
    // behave conservatively — they never act on a healthy/low-sample session and
    // only trip on a genuinely pathological reject rate. (The compiled DEFAULT
    // stays pct=0/disabled per the live-hardware default principle; these tests
    // pin the recommended-opt-in behavior an operator would configure.)
    const SW07_PCT: u8 = 20;
    const SW07_MIN_SAMPLES: u64 = 50;

    #[test]
    fn sw07_recommended_no_action_on_healthy_session() {
        // 1% reject over 1000 settled shares — healthy pool, no failover.
        assert!(!reject_rate_failover_due(
            990,
            10,
            SW07_MIN_SAMPLES,
            SW07_PCT
        ));
    }

    #[test]
    fn sw07_recommended_no_action_below_min_samples() {
        // 100% reject but only 49 samples (< 50 floor) → hold, avoid reacting to
        // a vardiff step / warm-up blip.
        assert!(!reject_rate_failover_due(0, 49, SW07_MIN_SAMPLES, SW07_PCT));
    }

    #[test]
    fn sw07_recommended_trips_on_pathological_session() {
        // Exactly at the 50-sample floor with 20% reject → trips.
        assert!(reject_rate_failover_due(40, 10, SW07_MIN_SAMPLES, SW07_PCT));
        // 50% reject over 100 → clearly pathological → trips.
        assert!(reject_rate_failover_due(50, 50, SW07_MIN_SAMPLES, SW07_PCT));
    }

    #[test]
    fn sw07_recommended_just_below_threshold_holds() {
        // 19% reject over 100 (19 < 20) → just under threshold, hold.
        assert!(!reject_rate_failover_due(
            81,
            19,
            SW07_MIN_SAMPLES,
            SW07_PCT
        ));
    }
}

#[cfg(test)]
mod ntime_window_tests {
    // SW-03: pure ntime validity-window decision.
    use super::{ntime_within_window, NTIME_VALIDITY_WINDOW_SECS};

    #[test]
    fn exact_match_is_within() {
        // 0x5f5e1000 == 1600000000; same as job → within.
        assert_eq!(
            ntime_within_window("5f5e1000", 0x5f5e1000, NTIME_VALIDITY_WINDOW_SECS),
            Ok(true)
        );
    }

    #[test]
    fn forward_ntime_roll_within_window_ok() {
        // +1800s forward (legitimate ntime rolling) → within the 7200s window.
        let job = 1_600_000_000u32;
        let share = job + 1800;
        assert_eq!(
            ntime_within_window(&format!("{share:08x}"), job, NTIME_VALIDITY_WINDOW_SECS),
            Ok(true)
        );
    }

    #[test]
    fn drift_beyond_window_rejected() {
        // +7201s → just past the 2h window → drop pre-submit.
        let job = 1_600_000_000u32;
        let share = job + NTIME_VALIDITY_WINDOW_SECS + 1;
        assert_eq!(
            ntime_within_window(&format!("{share:08x}"), job, NTIME_VALIDITY_WINDOW_SECS),
            Ok(false)
        );
    }

    #[test]
    fn backward_drift_beyond_window_rejected() {
        // A stale share lingering across a long stall, -7201s behind → drop.
        let job = 1_600_000_000u32;
        let share = job - (NTIME_VALIDITY_WINDOW_SECS + 1);
        assert_eq!(
            ntime_within_window(&format!("{share:08x}"), job, NTIME_VALIDITY_WINDOW_SECS),
            Ok(false)
        );
    }

    #[test]
    fn boundary_is_inclusive() {
        let job = 1_600_000_000u32;
        let share = job + NTIME_VALIDITY_WINDOW_SECS; // exactly at the edge → within
        assert_eq!(
            ntime_within_window(&format!("{share:08x}"), job, NTIME_VALIDITY_WINDOW_SECS),
            Ok(true)
        );
    }

    #[test]
    fn unparseable_ntime_is_fail_open() {
        // Garbage hex → Err(()) → caller submits anyway (never silently drop).
        assert_eq!(
            ntime_within_window("zzzz", 1_600_000_000, NTIME_VALIDITY_WINDOW_SECS),
            Err(())
        );
    }

    #[test]
    fn tolerates_0x_prefix_and_whitespace() {
        assert_eq!(
            ntime_within_window("  0x5f5e1000 ", 0x5f5e1000, NTIME_VALIDITY_WINDOW_SECS),
            Ok(true)
        );
    }

    // SW-11 parity: a share whose ntime came from the bounded host roll helper
    // (`JobTemplate::roll_ntime_within_window`) is, by construction, ALWAYS
    // inside the SW-03 pre-submit consensus window. The submitted ntime hex is
    // exactly the value the work was hashed with (the dispatcher uses the same
    // `WorkEntry::ntime` for both the local hash and `ValidShare::ntime`), so
    // this test pins the end-to-end contract: roll → format as submit hex →
    // SW-03 guard accepts, never burns a reject slot.
    #[test]
    fn sw11_rolled_ntime_is_always_within_sw03_window() {
        use crate::types::{JobTemplate, DEFAULT_NTIME_ROLL_WINDOW_SECS};

        let base_ntime = 1_700_000_000u32;
        let job = JobTemplate {
            job_id: "sw11".to_string(),
            prev_block_hash: [0u8; 32],
            coinbase1: vec![0x01],
            coinbase2: vec![0x02],
            merkle_branches: Vec::new(),
            version: 0x2000_0000,
            nbits: 0x1703_4219,
            ntime: base_ntime,
            clean_jobs: false,
            share_target: [0xFF; 32],
            extranonce1: vec![0xAA, 0xBB, 0xCC, 0xDD],
            extranonce2_size: 4,
            version_mask: 0x1fff_e000,
            merkle_root: [0u8; 32],
            pool_difficulty: 1.0,
        };

        // Every roll across the default budget, plus a deliberately over-window
        // request, must land inside the SW-03 consensus window when submitted.
        for roll in [0u32, 1, 30, DEFAULT_NTIME_ROLL_WINDOW_SECS, u32::MAX] {
            let rolled = job.roll_ntime_within_window(roll, DEFAULT_NTIME_ROLL_WINDOW_SECS);
            // The hashed ntime == the submitted ntime hex (parity invariant).
            let submit_hex = format!("{rolled:08x}");
            assert_eq!(
                ntime_within_window(&submit_hex, base_ntime, NTIME_VALIDITY_WINDOW_SECS),
                Ok(true),
                "rolled ntime (roll={roll}) must be accepted by the SW-03 pre-submit guard"
            );
        }
    }
}

#[cfg(test)]
mod nonce_dedup_tests {
    // SW-04: bounded per-job nonce dedup.
    use super::{NonceDedup, NONCE_DEDUP_JOBS, NONCE_DEDUP_MAX_KEYS_PER_JOB};

    #[test]
    fn first_submission_is_not_duplicate() {
        let mut d = NonceDedup::default();
        assert!(!d.is_duplicate_then_record("job-a", "deadbeef", None));
    }

    #[test]
    fn per_job_set_is_capped_to_bound_memory() {
        // S4-3: a pathological pool reusing one job_id for a long low-difficulty
        // window must not grow the per-job nonce set without bound.
        let mut d = NonceDedup::default();
        for i in 0..(NONCE_DEDUP_MAX_KEYS_PER_JOB + 500) {
            let nonce = format!("{i:08x}");
            // Every nonce is unique, so none is a duplicate.
            assert!(!d.is_duplicate_then_record("job-a", &nonce, None));
        }
        // Memory is bounded to the cap, NOT the ~cap+500 unique nonces submitted.
        assert_eq!(d.tracked_key_count(), NONCE_DEDUP_MAX_KEYS_PER_JOB);
        // A nonce recorded within the cap is still caught as a duplicate.
        assert!(d.is_duplicate_then_record("job-a", &format!("{:08x}", 0), None));
    }

    #[test]
    fn same_job_same_nonce_is_duplicate() {
        let mut d = NonceDedup::default();
        assert!(!d.is_duplicate_then_record("job-a", "deadbeef", None));
        assert!(d.is_duplicate_then_record("job-a", "deadbeef", None));
    }

    #[test]
    fn same_nonce_different_version_bits_not_duplicate() {
        // ASICBoost: one nonce valid at two rolled versions → both submit.
        let mut d = NonceDedup::default();
        assert!(!d.is_duplicate_then_record("job-a", "deadbeef", Some("00002000")));
        assert!(!d.is_duplicate_then_record("job-a", "deadbeef", Some("00004000")));
        // ...but the exact same (nonce, version) repeated IS a duplicate.
        assert!(d.is_duplicate_then_record("job-a", "deadbeef", Some("00002000")));
    }

    #[test]
    fn same_nonce_different_job_not_duplicate() {
        let mut d = NonceDedup::default();
        assert!(!d.is_duplicate_then_record("job-a", "deadbeef", None));
        assert!(!d.is_duplicate_then_record("job-b", "deadbeef", None));
    }

    #[test]
    fn oldest_job_evicted_beyond_capacity() {
        let mut d = NonceDedup::default();
        // Fill capacity with distinct jobs.
        for i in 0..NONCE_DEDUP_JOBS {
            assert!(!d.is_duplicate_then_record(&format!("job-{i}"), "aa", None));
        }
        // A new job evicts job-0.
        assert!(!d.is_duplicate_then_record("job-new", "aa", None));
        // job-0's nonce is forgotten → re-submitting it now reads as NEW, not dup.
        assert!(!d.is_duplicate_then_record("job-0", "aa", None));
        // The most-recent job is still tracked.
        assert!(d.is_duplicate_then_record("job-new", "aa", None));
    }

    #[test]
    fn reset_clears_all_retained_job_history() {
        // D-01: after a pool switch the dedup must forget every job so a
        // recycled job_id from a different pool starts clean.
        let mut d = NonceDedup::default();
        assert!(!d.is_duplicate_then_record("1", "deadbeef", None));
        assert!(d.is_duplicate_then_record("1", "deadbeef", None)); // dup within session
        d.reset();
        // Same (job_id, nonce) on the NEW pool reads as NEW, not a duplicate.
        assert!(!d.is_duplicate_then_record("1", "deadbeef", None));
    }

    #[test]
    fn recycled_short_job_id_across_pool_switch_is_not_dropped() {
        // D-01 regression: short job IDs (e.g. "1") are recycled across pools.
        // A valid nonce mined for the NEW pool's job "1" must NOT be dropped
        // just because the OLD pool's unrelated job "1" recorded that nonce.
        let mut d = NonceDedup::default();
        // Old pool session: submit nonce for job "1".
        assert!(!d.is_duplicate_then_record("1", "00c0ffee", Some("00002000")));
        // Pool switch (donation flip / failover / user-split) resets dedup.
        d.reset();
        // New pool, same recycled job_id + same nonce + same version bits: this
        // is a genuinely-different share for a genuinely-different job and MUST
        // be submittable, not silently deduped.
        assert!(!d.is_duplicate_then_record("1", "00c0ffee", Some("00002000")));
    }
}

#[cfg(test)]
mod pool_directed_reconnect_tests {
    // SW-09: TLS flag must survive a pool-directed reconnect.
    use super::{pool_directed_reconnect_endpoint, reconnect_wait_floor};
    use std::time::Duration;

    #[test]
    fn preserves_tls_true() {
        let ep = pool_directed_reconnect_endpoint("alt.pool.example.com".into(), 9999, true);
        assert_eq!(ep.host, "alt.pool.example.com");
        assert_eq!(ep.port, 9999);
        assert!(
            ep.tls,
            "a TLS pool must NOT be downgraded to plaintext on reconnect"
        );
    }

    #[test]
    fn preserves_tls_false() {
        let ep = pool_directed_reconnect_endpoint("plain.example.com".into(), 3333, false);
        assert!(!ep.tls);
    }

    #[test]
    fn reconnect_wait_is_never_below_the_backoff_floor() {
        // SEC: a hostile / MITM pool answering every handshake with
        // client.reconnect [attacker_host, wait=0] must NOT get an immediate
        // reconnect — the backoff floor is enforced so repeated redirects back off
        // (no unthrottled storm). A legitimate longer pool wait is still honored.
        let floor = Duration::from_millis(100);
        assert_eq!(
            reconnect_wait_floor(0, floor),
            floor,
            "a wait=0 reconnect must be floored to the current backoff step"
        );
        assert_eq!(
            reconnect_wait_floor(5, floor),
            Duration::from_secs(5),
            "a legitimate longer pool-requested wait is honored"
        );
        // A large backoff floor (after repeated redirects) wins over a tiny pool wait.
        let big_floor = Duration::from_secs(60);
        assert!(reconnect_wait_floor(1, big_floor) >= big_floor);
        assert!(reconnect_wait_floor(0, big_floor) >= big_floor);
        // The result is always >= the floor for any pool-supplied wait.
        for w in [0u32, 1, 5, 30, 3600, u32::MAX] {
            assert!(
                reconnect_wait_floor(w, floor) >= floor,
                "wait for pool_wait={w} dropped below the backoff floor"
            );
        }
    }
}

/// Three-way direction label for a vardiff (`mining.set_difficulty`) change.
///
/// A two-way `if new > old { "UP" } else { "DOWN" }` mislabels a no-op re-send
/// — pools commonly re-confirm the same difficulty as a keepalive / vardiff
/// re-confirmation — as "DOWN", which sends operators chasing a phantom
/// difficulty drop in exactly the rapid-set_difficulty window where accurate
/// telemetry matters most. NaN is treated as "UNCHANGED" for logging (the wire
/// parser already rejects non-finite difficulties, so it never reaches here).
fn difficulty_change_direction(old: f64, new: f64) -> &'static str {
    match new.partial_cmp(&old) {
        Some(std::cmp::Ordering::Greater) => "UP",
        Some(std::cmp::Ordering::Less) => "DOWN",
        _ => "UNCHANGED",
    }
}

#[cfg(test)]
mod difficulty_change_direction_tests {
    use super::difficulty_change_direction;

    #[test]
    fn three_way_including_unchanged() {
        assert_eq!(difficulty_change_direction(8192.0, 16384.0), "UP");
        assert_eq!(difficulty_change_direction(16384.0, 8192.0), "DOWN");
        // The bug this guards: a pool re-confirming the SAME difficulty must
        // NOT log "DOWN" (the old two-way split collapsed equal into "DOWN").
        assert_eq!(difficulty_change_direction(8192.0, 8192.0), "UNCHANGED");
    }
}

#[cfg(test)]
mod shadow_failover_tests {
    // G36 observe-only shadow wiring — validates the pure decision helper that the
    // (default-OFF) StratumV1Client::shadow_observe_failover wraps. Host-testable
    // (no client construction needed); runs via `cargo test -p dcentrald-stratum`.
    use super::shadow_failover_observe;
    use crate::pool_failover::{FailoverAction, PoolFailoverFsm};
    use dcentrald_api_types::luxos_pool_failover::{LuxosFailoverTrigger, LuxosPoolFailoverConfig};

    fn fsm(pools: usize) -> PoolFailoverFsm {
        let mut f = PoolFailoverFsm::new(LuxosPoolFailoverConfig::default(), pools);
        f.set_active(0);
        f
    }

    #[test]
    fn inactivity_reconnects_same_pool_without_advancing() {
        // PoolInactivity does NOT increment the error counter → the FSM reconnects
        // the SAME pool (active unchanged) — exactly the decision-divergence vs the
        // shipped logic's "fail the session into failover" that the shadow surfaces.
        let mut f = fsm(3);
        let action = shadow_failover_observe(&mut f, 0, LuxosFailoverTrigger::PoolInactivity);
        assert!(matches!(
            action,
            FailoverAction::Reconnect { pool_index: 0, .. }
        ));
        assert_eq!(f.active_pool(), Some(0));
    }

    #[test]
    fn resyncs_active_when_client_switched_pool_by_other_means() {
        // The client moved to pool 1 (donation/user-split/own-failover) since the
        // last observe → the shadow re-syncs the FSM's active index to match.
        let mut f = fsm(3);
        let _ = shadow_failover_observe(&mut f, 1, LuxosFailoverTrigger::PoolInactivity);
        assert_eq!(f.active_pool(), Some(1));
    }

    #[test]
    fn repeated_io_errors_accumulate_to_next_pool() {
        // IoError increments the error counter; the shadow must NOT spuriously
        // re-sync (which would reset accumulation), so repeated same-pool IoErrors
        // accumulate to a NextPool decision. Pins the "re-sync only on change" rule.
        let mut f = fsm(3);
        let mut advanced = false;
        for _ in 0..64 {
            if matches!(
                shadow_failover_observe(&mut f, 0, LuxosFailoverTrigger::IoError),
                FailoverAction::NextPool { .. }
            ) {
                advanced = true;
                break;
            }
        }
        assert!(
            advanced,
            "repeated same-pool IoError must accumulate to NextPool (no spurious re-sync)"
        );
    }
}

#[cfg(test)]
mod no_notify_failover_tests {
    use super::no_notify_failover_due;
    use std::time::{Duration, Instant};

    #[test]
    fn disabled_when_timeout_zero() {
        let t0 = Instant::now();
        assert!(!no_notify_failover_due(
            t0,
            Duration::from_secs(0),
            t0 + Duration::from_secs(100_000)
        ));
    }

    #[test]
    fn false_before_timeout_elapses() {
        let t0 = Instant::now();
        assert!(!no_notify_failover_due(
            t0,
            Duration::from_secs(300),
            t0 + Duration::from_secs(299)
        ));
    }

    #[test]
    fn true_at_and_after_timeout() {
        let t0 = Instant::now();
        assert!(no_notify_failover_due(
            t0,
            Duration::from_secs(300),
            t0 + Duration::from_secs(300)
        ));
        assert!(no_notify_failover_due(
            t0,
            Duration::from_secs(300),
            t0 + Duration::from_secs(5_000)
        ));
    }

    #[test]
    fn fresh_notify_resets_window() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(10_000);
        // A mining.notify just arrived → last_notify_at is near `now`.
        let last_notify_at = now - Duration::from_secs(5);
        assert!(!no_notify_failover_due(
            last_notify_at,
            Duration::from_secs(300),
            now
        ));
    }
}

#[cfg(test)]
mod stable_primary_return_tests {
    use super::should_prefer_primary_return;
    use std::time::{Duration, Instant};

    #[test]
    fn disabled_when_window_zero_even_if_elapsed() {
        let t0 = Instant::now();
        assert!(!should_prefer_primary_return(
            1,
            Some(t0),
            Duration::from_secs(0),
            t0 + Duration::from_secs(10_000)
        ));
    }

    #[test]
    fn false_when_on_primary() {
        let t0 = Instant::now();
        assert!(!should_prefer_primary_return(
            0,
            Some(t0),
            Duration::from_secs(900),
            t0 + Duration::from_secs(10_000)
        ));
    }

    #[test]
    fn false_when_primary_never_failed() {
        let t0 = Instant::now();
        assert!(!should_prefer_primary_return(
            2,
            None,
            Duration::from_secs(900),
            t0 + Duration::from_secs(10_000)
        ));
    }

    #[test]
    fn false_when_cooldown_not_elapsed() {
        let t0 = Instant::now();
        assert!(!should_prefer_primary_return(
            1,
            Some(t0),
            Duration::from_secs(900),
            t0 + Duration::from_secs(899)
        ));
    }

    #[test]
    fn true_on_backup_after_full_cooldown() {
        let t0 = Instant::now();
        assert!(should_prefer_primary_return(
            1,
            Some(t0),
            Duration::from_secs(900),
            t0 + Duration::from_secs(900)
        ));
        assert!(should_prefer_primary_return(
            2,
            Some(t0),
            Duration::from_secs(900),
            t0 + Duration::from_secs(5_000)
        ));
    }
}

// ---------------------------------------------------------------------------
// Donation time-switching types
// ---------------------------------------------------------------------------

/// Which pool the client is currently mining on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DonationPhase {
    /// Mining on user's configured pool (normal operation).
    User,
    /// Mining on D-Central donation pool.
    Donation,
    /// Donation disabled — never switch.
    Disabled,
}

/// Why a pool session ended — drives the outer loop's next action.
#[derive(Debug)]
enum SessionEndReason {
    /// Normal session end (pool disconnect, channel close).
    Clean,
    /// Donation timer fired — switch to the other pool.
    DonationSwitch,
    /// User weighted split timer fired — switch to the next user pool route.
    UserSplitSwitch,
    /// Auto mode wants to leave V1 fallback and retry SV2.
    AutoRetrySv2,
    /// Pool requested reconnect to a specific endpoint.
    Reconnect {
        host: String,
        port: u16,
        wait_seconds: u32,
    },
}

#[derive(Debug)]
struct PendingSubmit {
    share: ValidShare,
}

/// SW-04: how many recent job IDs the per-job nonce-dedup tracker retains.
/// A nonce is only a meaningful duplicate within the same job, and a job is
/// stale within a few `mining.notify` cadences, so tracking the last 3 jobs is
/// ample to catch a re-submitted nonce (e.g. the same solution arriving twice
/// from the validate/dispatch pipeline) without unbounded growth on a
/// long-running session. Bounded, O(1) eviction.
const NONCE_DEDUP_JOBS: usize = 3;

/// S4-3: upper bound on submitted-nonce keys tracked PER retained job. A normal
/// pool rotates `job_id` every `mining.notify` (whole job sets are evicted via
/// `NONCE_DEDUP_JOBS`), so a job holds at most a few seconds of nonces. This cap
/// only bites on a pathological pool that reuses one `job_id` for a very long
/// low-difficulty window: there the set stops growing (a later brand-new nonce
/// goes un-deduped → at worst a harmless pool reject), keeping memory bounded to
/// `NONCE_DEDUP_JOBS × NONCE_DEDUP_MAX_KEYS_PER_JOB` keys instead of unbounded.
const NONCE_DEDUP_MAX_KEYS_PER_JOB: usize = 100_000;

/// SW-04: bounded per-job submitted-nonce tracker. Prevents re-submitting an
/// identical (job_id, nonce[, version_bits]) tuple to the pool — a duplicate
/// share is a guaranteed pool reject that burns a reject slot (and, with
/// reject-rate failover enabled, can nudge a false failover).
///
/// Memory is bounded to the last `NONCE_DEDUP_JOBS` job IDs; when a new job
/// appears the oldest job's nonce set is evicted whole. Within a job, the
/// dedup key is `nonce` plus the version-rolling bits (two shares may legitimately
/// share a nonce at different rolled versions under ASICBoost — those are NOT
/// duplicates and must both be submitted).
#[derive(Debug, Default)]
struct NonceDedup {
    /// FIFO of (job_id, submitted-nonce-keys) — front = oldest, back = newest.
    jobs: VecDeque<(String, HashSet<String>)>,
}

impl NonceDedup {
    /// Dedup key within a job: nonce + version-rolling delta. Under ASICBoost a
    /// single nonce can be valid at multiple rolled versions, so the version
    /// bits are part of the identity — otherwise we'd wrongly drop a distinct
    /// valid share.
    fn key(nonce: &str, version_bits: Option<&str>) -> String {
        match version_bits {
            Some(v) => format!("{nonce}|{v}"),
            None => nonce.to_string(),
        }
    }

    /// Record (job_id, nonce, version_bits). Returns `true` if this exact tuple
    /// was already submitted within the retained window (i.e. it's a duplicate
    /// and should be dropped pre-submit); `false` if it's new (now recorded).
    fn is_duplicate_then_record(
        &mut self,
        job_id: &str,
        nonce: &str,
        version_bits: Option<&str>,
    ) -> bool {
        let key = Self::key(nonce, version_bits);

        if let Some((_, set)) = self.jobs.iter_mut().find(|(jid, _)| jid == job_id) {
            // Known job. Under the per-job cap (S4-3), behave exactly as before:
            // `insert` returns false if the key was already present → duplicate.
            // At the cap, stop inserting new keys so the set can't grow without
            // bound, but still report a key already recorded as a duplicate.
            if set.len() < NONCE_DEDUP_MAX_KEYS_PER_JOB {
                return !set.insert(key);
            }
            return set.contains(&key);
        }

        // New job: evict the oldest if at capacity, then start its nonce set.
        if self.jobs.len() >= NONCE_DEDUP_JOBS {
            self.jobs.pop_front();
        }
        let mut set = HashSet::new();
        set.insert(key);
        self.jobs.push_back((job_id.to_string(), set));
        false
    }

    /// Test-only: total submitted-nonce keys tracked across all retained jobs
    /// (used by the S4-3 per-job cap regression).
    #[cfg(test)]
    fn tracked_key_count(&self) -> usize {
        self.jobs.iter().map(|(_, set)| set.len()).sum()
    }

    /// D-01: forget all retained per-job nonce history.
    ///
    /// MUST be called on every pool switch (donation flip, user-split flip,
    /// failover, pool-directed reconnect, clean session end). Job IDs are pool-
    /// scoped and short numeric/hex job IDs are routinely *recycled* across
    /// different pools — e.g. public-pool.io and a donation pool can both name a
    /// job `"1"`. Carrying dedup state across a pool switch means a legitimate
    /// nonce mined for the NEW pool's `job_id="1"` could collide with a nonce
    /// already recorded for the OLD pool's unrelated `job_id="1"` and be silently
    /// dropped pre-submit — a lost valid share. The dedup window is only meant to
    /// catch a re-submit of the *same* solution within a *single* live pool
    /// session, so clearing it at the session boundary is correct (it never
    /// crosses a `mining.notify` cadence that would matter).
    fn reset(&mut self) {
        self.jobs.clear();
    }
}

#[derive(Debug)]
struct PendingSubmitResponse {
    request_id: u64,
    submitted_at: Instant,
    share: ValidShare,
}

impl PendingSubmitResponse {
    fn new(
        request_id: u64,
        submitted_at: Instant,
        mut share: ValidShare,
        submit_worker: String,
    ) -> Self {
        share.worker_name = submit_worker;
        Self {
            request_id,
            submitted_at,
            share,
        }
    }
}

/// User agent string sent to pools. Resolved from `[workspace.package] version`
/// in `dcentrald/Cargo.toml` so subscribe/notify pool logs always reflect the
/// shipped binary version. Single source of truth — bump via Cargo.toml.
const USER_AGENT: &str = concat!("dcentrald/", env!("CARGO_PKG_VERSION"));

/// Timeout for TCP connection attempts.
#[cfg(test)]
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for Stratum V1 subscribe/authorize handshake.
#[cfg(test)]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Only log the first few disconnected-share drops individually.
const DISCONNECTED_SHARE_LOG_LIMIT: u64 = 3;

/// Cap pending submit correlation records so a sick pool cannot grow this queue forever.
const MAX_PENDING_SUBMITS: usize = 256;

/// POOL-1: the minimum session uptime that, on its own, proves a user-pool
/// session was healthy enough to ZERO the reconnect backoff attempt counter.
///
/// Resetting the backoff on "delivered >=1 job" alone is too weak: a sick
/// primary that hands out a single `mining.notify` then drops the socket would
/// re-zero the counter every reconnect, so the failover gate (`attempt() >= 3`)
/// could never trip and the client would loop forever on the broken primary
/// instead of failing over to a backup. A genuinely healthy session either
/// produces an accepted share or stays connected well past the handshake +
/// settle window, so we require ONE of those before treating the session as
/// healthy. The threshold is deliberately generous (far longer than a
/// deliver-one-then-drop cycle, far shorter than a normal mining session) so it
/// can never false-trigger on a healthy unit — a working session still resets.
///
/// The test value is set above the test-mode `HANDSHAKE_TIMEOUT` (100ms) so a
/// deliver-one-then-drop session — whose entire lifetime is bounded by the
/// handshake — can never accidentally clear the bar regardless of scheduler
/// jitter, while a session that deliberately stays connected longer still does.
#[cfg(test)]
const SESSION_HEALTHY_UPTIME: Duration = Duration::from_millis(150);
#[cfg(not(test))]
const SESSION_HEALTHY_UPTIME: Duration = Duration::from_secs(30);

impl JobTemplate {
    fn flush_only(pool_difficulty: f64) -> Self {
        Self {
            job_id: String::new(),
            prev_block_hash: [0u8; 32],
            coinbase1: Vec::new(),
            coinbase2: Vec::new(),
            merkle_branches: Vec::new(),
            version: 0,
            nbits: 0,
            ntime: 0,
            clean_jobs: true,
            share_target: [0u8; 32],
            extranonce1: Vec::new(),
            extranonce2_size: 0,
            version_mask: 0,
            merkle_root: [0u8; 32],
            pool_difficulty,
        }
    }
}

/// Request IDs for the handshake sequence.
const ID_CONFIGURE: u64 = 1;
const ID_SUBSCRIBE: u64 = 2;
const ID_AUTHORIZE: u64 = 3;
const ID_SUGGEST_DIFF: u64 = 4;
/// W11.13 — Bitmain `mining.extranonce.subscribe` extension. Sent once
/// after handshake completes; the pool may respond with `result=true` to
/// confirm or with an error result if the extension is unsupported.
/// Either way the V1 client keeps mining (the `subscribe-extranonce`
/// capability advertised in `mining.configure` is the parallel path).
const ID_EXTRANONCE_SUBSCRIBE: u64 = 5;

/// The main Stratum V1 client.
///
/// Manages pool connections, handshake, job dispatch, and share submission.
/// Runs as an autonomous async Tokio task.
///
/// ## Donation Time-Switching
///
/// If donation is enabled (2% by default — our autotuner saves more than 2%
/// in energy, so the donation can pay for itself), the client transparently
/// switches between the user's pool and the donation pool based on time.
/// For example, at 2% donation with a 1-hour cycle: mine on user pool for
/// 3528s, then switch to D-Central donation pool for 72s, then back.
/// The switching is invisible to the mining pipeline — ASICs keep running,
/// only the pool connection changes. Fully configurable (0-5%), fully
/// disableable in Settings.
pub struct StratumV1Client {
    config: StratumConfig,
    stats: Arc<Mutex<StratumStats>>,

    // Channel to send new jobs to the mining pipeline
    job_tx: mpsc::Sender<JobTemplate>,

    // Channel to receive valid shares for submission
    share_rx: mpsc::Receiver<ValidShare>,

    // Channel to send status updates to the main daemon
    status_tx: mpsc::Sender<StratumStatus>,

    // Internal state
    work_builder: WorkBuilder,
    request_id_counter: u64,
    current_difficulty: f64,
    extranonce1: Vec<u8>,
    extranonce2_size: usize,
    version_mask: u32,
    current_pool_index: usize,
    last_job: Option<JobTemplate>,
    mining_state_announced: bool,

    /// G36 observe-only shadow (default-OFF behind `[pool].smart_failover_enabled`).
    /// `Some` only after the first failover event when the operator opted in; the
    /// LuxOS `PoolFailoverFsm` is fed the same triggers the existing failover logic
    /// acts on and LOGS what it WOULD decide, without ever acting (the existing
    /// logic still drives pool selection). `None` (the default) → zero footprint,
    /// default path byte-identical. The Stage-B flip (FSM actually drives) is
    /// soak + operator-gated per `pool_failover.rs` module docs.
    failover_fsm: Option<PoolFailoverFsm>,

    /// Tracks submit request IDs -> correlated share metadata until the pool replies.
    pending_submits: Vec<PendingSubmitResponse>,

    /// Pool-directed reconnect target. Consumed on next connection attempt.
    ///
    /// SW-09 (2026-06-02): the third tuple element is the TLS flag of the
    /// session that received the `client.reconnect` directive. It MUST be
    /// preserved — a TLS pool that redirects us must not be silently
    /// downgraded to plaintext. Previously this was hardcoded `tls: false`
    /// at the consumption site, which dropped TLS on every pool-directed
    /// reconnect. We carry the originating session's TLS flag here instead.
    pending_reconnect: Option<(String, u16, bool)>,

    /// Share already received from the dispatcher but not yet written to the pool.
    /// Preserved across reconnects so a socket error during submit does not drop it.
    pending_share: Option<PendingSubmit>,

    /// SW-04: bounded per-job submitted-nonce dedup. Drops an identical
    /// (job_id, nonce, version_bits) tuple before it reaches the pool so a
    /// duplicated share never burns a guaranteed reject slot.
    nonce_dedup: NonceDedup,

    // Donation time-switching state
    donation_phase: DonationPhase,
    donation_cycle_start: Instant,
    donation_user_duration: Duration,
    donation_donation_duration: Duration,
    /// Cumulative seconds spent on donation pool this session.
    donation_total_time_s: u64,
    /// Cumulative shares submitted to donation pool this session.
    donation_total_shares: u64,
    /// Donation endpoint currently selected: 0 = D-Central primary, 1 = fallback.
    donation_pool_index: usize,

    // Weighted user-pool split state. This only routes user hashrate; donation
    // remains a separate transparent override when its own phase is active.
    user_split_enabled: bool,
    user_split_cycle_start: Instant,
    user_split_primary_duration: Duration,
    user_split_secondary_duration: Duration,
    user_split_primary_bps: u16,
    user_split_secondary_bps: u16,
    user_split_switch_count: u64,
    user_split_secondary_shares: u64,

    /// Cumulative user-pool failover switches during this client lifetime.
    failover_switch_count: u64,
    /// Last pool switch reason, if any.
    last_failover_switch_reason: Option<String>,
    /// Last user-pool failure reason, if any.
    last_failover_failure_reason: Option<String>,
    /// Last pool index that produced a user-pool failure.
    last_failover_failure_pool_index: Option<usize>,
    /// When the primary pool (index 0) last failed — arms the
    /// stable-primary-return anti-flap cool-down. `None` until the
    /// primary has failed at least once this client lifetime.
    last_primary_failure_at: Option<Instant>,
    /// Number of pending submit correlations cleared at the last session end.
    last_pending_submit_correlations_cleared: u64,
    /// Cumulative pending submit correlations dropped by the in-memory cap.
    pending_submit_dropped: u64,
    /// Whether the last pool switch emitted a flush-only clean job.
    last_stale_jobs_flushed_on_switch: bool,

    /// W6.3: rolling 30-minute share acceptance tracker.
    ///
    /// Updated on every accept/reject branch in
    /// `handle_submit_response` so the autotuner step-up gate sees
    /// rejection storms before they show up in the cumulative
    /// `shares_accepted / shares_rejected` numbers. Surfaced through
    /// `StratumStats::rolling_acceptance_pct`.
    acceptance_tracker: crate::acceptance_tracker::AcceptanceTracker,
}

struct SessionContext {
    donation_phase_remaining: Duration,
    user_split_remaining: Duration,
    is_donation: bool,
    active_pool_index: usize,
    sv2_retry_remaining: Option<Duration>,
}

impl StratumV1Client {
    /// Create a new Stratum V1 client.
    ///
    /// # Arguments
    /// - `config`: Pool URLs, worker credentials, donation settings
    /// - `job_tx`: Send new job templates to the job dispatcher
    /// - `share_rx`: Receive valid shares from the share validator
    /// - `status_tx`: Send status updates (state changes, share results)
    pub fn new(
        config: StratumConfig,
        job_tx: mpsc::Sender<JobTemplate>,
        share_rx: mpsc::Receiver<ValidShare>,
        status_tx: mpsc::Sender<StratumStatus>,
    ) -> Self {
        // Compute donation timing from config
        let (phase, user_dur, don_dur) = if config.donation.enabled && config.donation.percent > 0.0
        {
            let cycle_s = (config.donation.cycle_duration_s.max(60)) as f64;
            // Defense-in-depth: clamp the donation fraction to [0.0, 1.0] so a
            // config with percent > 100 (or a future caller that bypasses
            // Config::validate's 0-5% check) can never drive `user_s` negative —
            // Duration::from_secs_f64 PANICS on a negative value. Valid 0-5%
            // configs are unaffected.
            let don_frac = ((config.donation.percent as f64) / 100.0).clamp(0.0, 1.0);
            let don_s = cycle_s * don_frac;
            let user_s = cycle_s - don_s;
            (
                DonationPhase::User,
                Duration::from_secs_f64(user_s),
                Duration::from_secs_f64(don_s),
            )
        } else {
            (DonationPhase::Disabled, Duration::ZERO, Duration::ZERO)
        };

        let split_requested = config.routing_mode.eq_ignore_ascii_case("weighted_split");
        let split_primary_bps = config.pool1.split_bps.unwrap_or(8000);
        let split_secondary_bps = config
            .pool2
            .as_ref()
            .and_then(|pool| pool.split_bps)
            .unwrap_or_else(|| 10_000u16.saturating_sub(split_primary_bps));
        let split_valid = split_requested
            && config.pool2.is_some()
            && split_primary_bps > 0
            && split_secondary_bps > 0
            && split_primary_bps.saturating_add(split_secondary_bps) == 10_000;
        let split_cycle_s = config.split_cycle_duration_s.max(120) as f64;
        let (split_enabled, split_primary_duration, split_secondary_duration) = if split_valid {
            let primary_s = split_cycle_s * (split_primary_bps as f64 / 10_000.0);
            let secondary_s = split_cycle_s * (split_secondary_bps as f64 / 10_000.0);
            (
                true,
                Duration::from_secs_f64(primary_s),
                Duration::from_secs_f64(secondary_s),
            )
        } else {
            (false, Duration::ZERO, Duration::ZERO)
        };

        Self {
            config,
            stats: Arc::new(Mutex::new(StratumStats::default())),
            job_tx,
            share_rx,
            status_tx,
            work_builder: WorkBuilder::new(),
            request_id_counter: 10, // Start after handshake IDs
            current_difficulty: 1.0,
            extranonce1: Vec::new(),
            // W5.4: 0 is a sentinel meaning "not yet parsed from
            // mining.subscribe response". Real pool values are 1..=8 (see
            // is_valid_v1_extranonce2_size). parse_subscribe_result writes
            // the pool-provided value before the first job ever flows; any
            // path that reads this before then is a bug, asserted at the
            // build_work / format_extranonce2 sites below.
            extranonce2_size: 0,
            version_mask: 0,
            current_pool_index: 0,
            last_job: None,
            mining_state_announced: false,
            failover_fsm: None,
            pending_submits: Vec::with_capacity(64),
            pending_reconnect: None,
            pending_share: None,
            nonce_dedup: NonceDedup::default(),
            donation_phase: phase,
            donation_cycle_start: Instant::now(),
            donation_user_duration: user_dur,
            donation_donation_duration: don_dur,
            donation_total_time_s: 0,
            donation_total_shares: 0,
            donation_pool_index: 0,
            user_split_enabled: split_enabled,
            user_split_cycle_start: Instant::now(),
            user_split_primary_duration: split_primary_duration,
            user_split_secondary_duration: split_secondary_duration,
            user_split_primary_bps: split_primary_bps,
            user_split_secondary_bps: split_secondary_bps,
            user_split_switch_count: 0,
            user_split_secondary_shares: 0,
            failover_switch_count: 0,
            last_failover_switch_reason: None,
            last_failover_failure_reason: None,
            last_failover_failure_pool_index: None,
            last_primary_failure_at: None,
            last_pending_submit_correlations_cleared: 0,
            pending_submit_dropped: 0,
            last_stale_jobs_flushed_on_switch: false,
            acceptance_tracker: crate::acceptance_tracker::AcceptanceTracker::new(),
        }
    }

    /// Get a clone of the stats handle for external access.
    pub fn stats(&self) -> Arc<Mutex<StratumStats>> {
        Arc::clone(&self.stats)
    }

    /// Whether the operator opted into the SmartSwitch pool-failover FSM
    /// (`[stratum].smart_failover_enabled`). Proves the config toggle is
    /// threaded all the way to the live V1 client. **Telemetry/contract
    /// surface only** — the existing user-pool failover machinery is still
    /// the sole driver of pool selection regardless of this flag (the
    /// FSM-drives-selection promotion is Wave-H operator-soak gated). With
    /// the flag false (default) the runtime behavior is byte-identical to
    /// the pre-toggle daemon.
    pub fn smart_failover_enabled(&self) -> bool {
        self.config.smart_failover_enabled
    }

    /// G36 observe-only shadow: when the operator opted in
    /// (`[pool].smart_failover_enabled`), feed the LuxOS `PoolFailoverFsm` the
    /// `trigger` the existing failover logic is about to act on and LOG what the
    /// FSM WOULD decide — WITHOUT acting on it. Lets an operator compare the FSM's
    /// decisions against the shipped failover logic on a live soak before the
    /// soak-gated + operator-authorized Stage-B flip (see `pool_failover.rs` docs).
    /// No-op + zero allocation when the flag is off (the shipped default) → the
    /// failover path is byte-for-byte identical to today.
    ///
    /// SW-01 (2026-06-02): the FSM is CAPABLE of driving selection. When BOTH
    /// `[stratum].smart_failover_enabled` AND a drive arm (the
    /// `smart_failover_drive` config field OR the `DCENT_POOL_FAILOVER_FSM_DRIVE`
    /// env gate) are set, the FSM's recommended active pool index is applied to
    /// `current_pool_index`. Both arms default OFF, so the shipped daemon runs
    /// observe-only and the legacy backoff failover remains the sole driver until
    /// an operator soak (per `pool_failover.rs` Stage-B docs) authorizes the path.
    ///
    /// PSF-1 (2026-06-20) — KNOWN LIMITATION: in production this method is only
    /// ever called with same-pool triggers — `PoolInactivity` (no-notify) and
    /// `TooManyRejections` (reject-rate). Both classify as `reconnects_same_pool()`,
    /// so the FSM keeps its active index unchanged and the drive arm CANNOT advance
    /// the pool from here even when armed (it reconnects the SAME pool). The
    /// advancing triggers (`TcpConnectTimeout`/`IoError`/`AuthError`/`TlsError`) are
    /// produced by the connect/handshake-failure arm, which drives real failover via
    /// the legacy backoff loop and does NOT route through this method. So today the
    /// drive arm is observe-equivalent for pool *advancement*; do not rely on it to
    /// change pools until an advancing trigger is wired here (itself a soak-gated
    /// behavior change). Pinned by `fov6_production_triggers_do_not_advance_under_drive`.
    fn shadow_observe_failover(&mut self, trigger: LuxosFailoverTrigger) {
        if !self.config.smart_failover_enabled {
            return;
        }
        let active = self.current_pool_index;
        let pool_count = self.pool_count();
        let fsm = self.failover_fsm.get_or_insert_with(|| {
            let mut f = PoolFailoverFsm::new(LuxosPoolFailoverConfig::default(), pool_count);
            f.set_active(active);
            f
        });
        let action = shadow_failover_observe(fsm, active, trigger);

        // SW-01: resolve the index the FSM would make active — prefer the
        // action's explicit target, else the FSM's internally-advanced active
        // pool (NextPool/DropFromList advance inside the FSM and don't carry the
        // target in the action). Bounded to the configured pool count.
        let fsm_recommended = action
            .recommended_active_index()
            .or_else(|| fsm.active_pool())
            .filter(|&idx| idx < pool_count);

        // Drive needs `smart_failover_enabled` (checked above) AND a drive arm:
        // the `[stratum].smart_failover_drive` config field OR the
        // `DCENT_POOL_FAILOVER_FSM_DRIVE` env gate. Both default-OFF → the
        // shipped daemon stays observe-only and behaves byte-identically.
        let drive = self.config.smart_failover_drive || crate::pool_failover::fsm_drive_enabled();
        if drive {
            if let Some(target) = fsm_recommended {
                if target != active {
                    info!(
                        smart_failover = "drive",
                        from_pool_index = active,
                        to_pool_index = target,
                        trigger = ?trigger,
                        fsm_decided = ?action,
                        "PoolFailoverFsm DRIVE (armed via DCENT_POOL_FAILOVER_FSM_DRIVE) — \
                         applying FSM-recommended pool selection to current_pool_index"
                    );
                    self.current_pool_index = target;
                }
            }
            return;
        }

        info!(
            smart_failover = "shadow",
            active_pool_index = active,
            trigger = ?trigger,
            fsm_would_decide = ?action,
            fsm_would_select = ?fsm_recommended,
            "PoolFailoverFsm SHADOW (observe-only, NOT acted on) — logged for live \
             comparison vs the shipped failover logic before the soak-gated Stage-B flip"
        );
    }

    /// Return the owned channels/config so Auto mode can rebuild another client.
    pub fn into_parts(
        self,
    ) -> (
        StratumConfig,
        mpsc::Sender<JobTemplate>,
        mpsc::Receiver<ValidShare>,
        mpsc::Sender<StratumStatus>,
    ) {
        (self.config, self.job_tx, self.share_rx, self.status_tx)
    }

    fn log_disconnected_share_drop(phase: &str, dropped: u64, share: &ValidShare) {
        if dropped <= DISCONNECTED_SHARE_LOG_LIMIT {
            warn!(
                phase,
                job_id = %share.job_id,
                nonce = %share.nonce,
                ntime = %share.ntime,
                dropped,
                "Dropping share while disconnected to keep the mining pipeline flowing"
            );
        }
    }

    fn log_disconnected_share_summary(phase: &str, dropped: u64) {
        if dropped > 0 {
            warn!(
                phase,
                dropped, "Dropped shares while no Stratum session was active"
            );
        }
    }

    fn failover_status(
        &self,
        event: impl Into<String>,
        consecutive_failures: u32,
        backoff_ms: u64,
    ) -> PoolFailoverStatus {
        let configured_pool_count = self.pool_count();
        let active_pool = self.get_pool_config(self.current_pool_index);
        PoolFailoverStatus {
            enabled: configured_pool_count > 1,
            smart_failover_enabled: self.config.smart_failover_enabled,
            configured_pool_count,
            active_pool_index: self.current_pool_index,
            active_pool_priority: self.current_pool_index + 1,
            active_pool_url: active_pool.url,
            consecutive_failures,
            switch_count: self.failover_switch_count,
            last_switch_reason: self.last_failover_switch_reason.clone(),
            last_failure_reason: self.last_failover_failure_reason.clone(),
            last_failure_pool_index: self.last_failover_failure_pool_index,
            last_failure_pool_priority: self
                .last_failover_failure_pool_index
                .map(|index| index + 1),
            stale_jobs_flushed_on_switch: self.last_stale_jobs_flushed_on_switch,
            pending_submit_correlations_cleared: self.last_pending_submit_correlations_cleared,
            shares_unresolved: self.pending_submits.len() as u64,
            pending_submit_dropped: self.pending_submit_dropped,
            pending_share_preserved: self.pending_share.is_some(),
            backoff_ms,
            event: event.into(),
            telemetry_source: "stratum_v1_client".to_string(),
        }
    }

    /// W5.5: refresh donation route fields on the shared StratumStats.
    ///
    /// Called every time the client transitions into or out of a donation
    /// session, or swaps from primary to fallback within a donation window.
    /// The dashboard reads `stats.donating + stats.donation_active_url +
    /// stats.donation_active_worker + stats.donation_pool_index` together
    /// to render "Donating to D-Central primary" vs "Donating via Braiins
    /// Pool fallback".
    async fn refresh_donation_stats(&self) {
        let donating = self.donation_phase == DonationPhase::Donation;
        let (url, worker, idx) = if donating {
            let cfg = self.donation_pool_config(self.donation_pool_index);
            (cfg.url, cfg.worker, self.donation_pool_index)
        } else {
            // Outside the donation window the route fields are cleared so
            // the dashboard never displays a stale donation pool name when
            // the operator is back on their own pool.
            (String::new(), String::new(), 0)
        };
        let mut stats = self.stats.lock().await;
        stats.donating = donating;
        stats.donation_active_url = url;
        // SW-08 (privacy): the donation worker can be a wallet-shaped string
        // (a payout address used as the Stratum username). `StratumStats` is
        // surfaced through REST / WebSocket / support bundles, so mask it at
        // the point it enters the shared stats — every downstream consumer then
        // gets the masked `<first6>…<last4>` form by construction. `mask_wallet`
        // passes through names shorter than 12 chars unchanged.
        stats.donation_active_worker = dcentrald_common::wallet_mask::mask_wallet(&worker);
        stats.donation_pool_index = idx;
    }

    async fn send_failover_status(
        &self,
        event: impl Into<String>,
        consecutive_failures: u32,
        backoff_ms: u64,
    ) {
        let status = self.failover_status(event, consecutive_failures, backoff_ms);
        {
            let mut stats = self.stats.lock().await;
            stats.failover = status.clone();
            stats.active_pool_index = status.active_pool_index;
            stats.shares_unresolved = status.shares_unresolved;
            stats.pending_submit_dropped = status.pending_submit_dropped;
        }
        self.send_status(StratumStatus::PoolFailoverUpdated(status))
            .await;
    }

    fn active_user_split_pool_index(&self) -> usize {
        if !self.user_split_enabled {
            return self.current_pool_index;
        }

        let cycle_total = self.user_split_primary_duration + self.user_split_secondary_duration;
        if cycle_total.is_zero() {
            return 0;
        }

        let elapsed = self.user_split_cycle_start.elapsed();
        let cycle_elapsed_secs = elapsed.as_secs_f64() % cycle_total.as_secs_f64();
        let cycle_elapsed = Duration::from_secs_f64(cycle_elapsed_secs);
        if cycle_elapsed < self.user_split_primary_duration {
            0
        } else {
            1
        }
    }

    fn time_remaining_in_user_split_route(&self) -> Duration {
        if !self.user_split_enabled {
            return Duration::ZERO;
        }

        let cycle_total = self.user_split_primary_duration + self.user_split_secondary_duration;
        if cycle_total.is_zero() {
            return Duration::ZERO;
        }

        let elapsed = self.user_split_cycle_start.elapsed();
        let cycle_elapsed_secs = elapsed.as_secs_f64() % cycle_total.as_secs_f64();
        let cycle_elapsed = Duration::from_secs_f64(cycle_elapsed_secs);

        if cycle_elapsed < self.user_split_primary_duration {
            self.user_split_primary_duration
                .saturating_sub(cycle_elapsed)
        } else {
            let secondary_elapsed = cycle_elapsed.saturating_sub(self.user_split_primary_duration);
            self.user_split_secondary_duration
                .saturating_sub(secondary_elapsed)
        }
    }

    fn hashrate_split_status(&self, donation_override: bool) -> HashrateSplitStatus {
        let active_pool_index = if donation_override {
            0
        } else {
            self.active_user_split_pool_index()
        };
        let active_route = if !self.user_split_enabled {
            "disabled"
        } else if donation_override {
            "donation_override"
        } else if active_pool_index == 1 {
            "secondary"
        } else {
            "primary"
        };

        HashrateSplitStatus {
            enabled: self.user_split_enabled,
            active: self.user_split_enabled && !donation_override,
            active_route: active_route.to_string(),
            active_pool_index,
            active_pool_priority: active_pool_index + 1,
            primary_bps: self.user_split_primary_bps,
            secondary_bps: self.user_split_secondary_bps,
            cycle_duration_s: self.config.split_cycle_duration_s,
            cycle_remaining_s: if self.user_split_enabled && !donation_override {
                self.time_remaining_in_user_split_route().as_secs()
            } else {
                0
            },
            switch_count: self.user_split_switch_count,
            secondary_shares: self.user_split_secondary_shares,
            telemetry_source: "stratum_v1_client".to_string(),
        }
    }

    async fn send_hashrate_split_status(&self, donation_override: bool) {
        let status = self.hashrate_split_status(donation_override);
        {
            let mut stats = self.stats.lock().await;
            stats.hashrate_split = status.clone();
        }
        self.send_status(StratumStatus::HashrateSplitUpdated(status))
            .await;
    }

    async fn connect_endpoint_with_share_drain(
        &mut self,
        endpoint: PoolEndpoint,
        timeout: Duration,
    ) -> Result<StratumConnection, ConnectionError> {
        // V1 inbound-line cap (strat-09): captured before the select so
        // applying it to the new connection needs no extra `self` borrow.
        let v1_line_cap = self.config.v1_max_inbound_line_bytes;
        let connect = StratumConnection::connect_endpoint(endpoint, timeout);
        tokio::pin!(connect);

        let mut dropped = 0u64;
        let mut share_rx_open = true;

        loop {
            tokio::select! {
                result = &mut connect => {
                    Self::log_disconnected_share_summary("connect", dropped);
                    let mut result = result;
                    if let Ok(ref mut conn) = result {
                        // Fail-safe: the connection already defaults to
                        // DEFAULT_V1_MAX_LINE_BYTES; this applies the
                        // operator-configured value (0 → finite backstop).
                        conn.set_max_line_bytes(v1_line_cap);
                    }
                    return result;
                }
                share = self.share_rx.recv(), if share_rx_open => {
                    match share {
                        Some(share) => {
                            dropped += 1;
                            Self::log_disconnected_share_drop("connect", dropped, &share);
                        }
                        None => {
                            share_rx_open = false;
                        }
                    }
                }
            }
        }
    }

    async fn wait_with_share_drain(&mut self, duration: Duration, phase: &'static str) {
        if duration.is_zero() {
            return;
        }

        let timer = tokio::time::sleep(duration);
        tokio::pin!(timer);

        let mut dropped = 0u64;
        let mut share_rx_open = true;

        loop {
            tokio::select! {
                _ = &mut timer => {
                    Self::log_disconnected_share_summary(phase, dropped);
                    return;
                }
                share = self.share_rx.recv(), if share_rx_open => {
                    match share {
                        Some(share) => {
                            dropped += 1;
                            Self::log_disconnected_share_drop(phase, dropped, &share);
                        }
                        None => {
                            share_rx_open = false;
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Donation time-switching helpers
    // -----------------------------------------------------------------------

    /// How much time remains in the current donation phase before switching.
    /// Returns Duration::ZERO when donation is disabled.
    fn time_remaining_in_phase(&self) -> Duration {
        if self.donation_phase == DonationPhase::Disabled {
            return Duration::ZERO;
        }

        let cycle_total = self.donation_user_duration + self.donation_donation_duration;
        if cycle_total.is_zero() {
            return Duration::ZERO;
        }

        let elapsed = self.donation_cycle_start.elapsed();
        let cycle_elapsed_secs = elapsed.as_secs_f64() % cycle_total.as_secs_f64();
        let cycle_elapsed = Duration::from_secs_f64(cycle_elapsed_secs);

        match self.donation_phase {
            DonationPhase::User => self.donation_user_duration.saturating_sub(cycle_elapsed),
            DonationPhase::Donation => {
                let donation_elapsed = cycle_elapsed.saturating_sub(self.donation_user_duration);
                self.donation_donation_duration
                    .saturating_sub(donation_elapsed)
            }
            DonationPhase::Disabled => Duration::ZERO,
        }
    }

    /// Flip between User and Donation phases, log the transition.
    fn flip_donation_phase(&mut self) {
        match self.donation_phase {
            DonationPhase::User => {
                self.donation_phase = DonationPhase::Donation;
                self.donation_pool_index = 0;
                info!(
                    donation_pct = self.config.donation.percent,
                    duration_s = self.donation_donation_duration.as_secs(),
                    "Donation cycle: switching to D-Central donation pool for {}s \
                     ({:.1}% of hashrate). This is voluntary — disable in Settings.",
                    self.donation_donation_duration.as_secs(),
                    self.config.donation.percent,
                );
            }
            DonationPhase::Donation => {
                self.donation_phase = DonationPhase::User;
                self.donation_pool_index = 0;
                info!(
                    donated_total_s = self.donation_total_time_s,
                    donated_shares = self.donation_total_shares,
                    "Donation cycle complete: returning to user pool. \
                     Total donated this session: {}s, {} shares.",
                    self.donation_total_time_s,
                    self.donation_total_shares,
                );
            }
            DonationPhase::Disabled => {} // Should never be called
        }
    }

    /// Number of configured donation endpoints. This is separate from user-pool
    /// failover so the operator's pool priorities are never modified by
    /// donation routing.
    fn donation_pool_count(&self) -> usize {
        if self.config.donation.fallback_enabled
            && !self.config.donation.fallback_pool_url.trim().is_empty()
            && !self.config.donation.fallback_worker.trim().is_empty()
        {
            2
        } else {
            1
        }
    }

    /// Build a PoolConfig for the donation pool from DonationConfig.
    fn donation_pool_config(&self, index: usize) -> PoolConfig {
        if index == 1 && self.donation_pool_count() > 1 {
            return PoolConfig {
                url: self.config.donation.fallback_pool_url.clone(),
                worker: self.config.donation.fallback_worker.clone(),
                password: self.config.donation.fallback_password.clone(),
                sv2_url: None,
                protocol: None,
                split_bps: None,
            };
        }

        PoolConfig {
            url: self.config.donation.pool_url.clone(),
            worker: self.config.donation.worker.clone(),
            password: self.config.donation.password.clone(),
            sv2_url: None,
            protocol: None,
            split_bps: None,
        }
    }

    fn try_switch_to_donation_fallback(&mut self, reason: &str) -> bool {
        if self.donation_pool_index == 0 && self.donation_pool_count() > 1 {
            self.donation_pool_index = 1;
            // W1.4: donation fallback worker is a wallet address — mask it.
            warn!(
                reason,
                // TEL-1: the pool URL can carry `user:pass@` credentials —
                // sanitize before it reaches any log/syslog tunnel.
                fallback_pool = %crate::pool_api::sanitize_pool_url(&self.config.donation.fallback_pool_url),
                fallback_worker = %dcentrald_common::wallet_mask::mask_wallet(&self.config.donation.fallback_worker),
                "Primary donation pool unavailable; trying visible BraiinsPool donation fallback"
            );
            true
        } else {
            false
        }
    }

    fn resume_user_pool_after_donation_failure(&mut self) {
        self.donation_phase = DonationPhase::User;
        self.donation_pool_index = 0;
        self.donation_cycle_start = Instant::now();
    }

    /// Run the client forever. This is the main entry point — spawn as a Tokio task.
    ///
    /// The client will:
    /// 1. Connect to pools in priority order
    /// 2. Perform handshake
    /// 3. Enter the mining loop
    /// 4. Reconnect with backoff on disconnect
    /// 5. Switch between user pool and donation pool based on time
    pub async fn run(mut self) {
        self.run_loop(None).await;
    }

    /// Run V1 fallback until Auto mode should retry SV2.
    pub async fn run_until_sv2_retry(mut self, retry_after: Duration) -> Self {
        self.run_loop(Some(retry_after)).await;
        self
    }

    async fn run_loop(&mut self, sv2_retry_after: Option<Duration>) {
        let mut backoff = Backoff::new();
        let pool_count = self.pool_count();
        let sv2_retry_deadline = sv2_retry_after.map(|delay| Instant::now() + delay);

        // W1.4: worker is the operator's wallet address on Stratum V1 — mask it.
        info!(
            pool_count,
            // TEL-1: pool URL may embed `user:pass@` — sanitize on every surface.
            primary_pool = %crate::pool_api::sanitize_pool_url(&self.config.pool1.url),
            worker = %dcentrald_common::wallet_mask::mask_wallet(&self.config.pool1.worker),
            version_rolling = self.config.version_rolling,
            hash_on_disconnect = self.config.hash_on_disconnect,
            "Stratum V1 client starting — connecting to pool to receive mining jobs and submit shares"
        );

        if self.config.version_rolling {
            info!("ASICBoost (BIP 310 version rolling) is ENABLED — this gives ~20% hashrate boost by rolling bits in the block header version field");
        }

        self.send_failover_status("startup", backoff.attempt(), 0)
            .await;
        self.send_hashrate_split_status(false).await;

        // Log donation configuration
        match self.donation_phase {
            DonationPhase::Disabled => {
                info!(
                    "Donation disabled — 100% of hashrate goes to your pool. \
                       Enable in [donation] config to support open-source development."
                );
            }
            _ => {
                info!(
                    donation_pct = self.config.donation.percent,
                    cycle_s = self.config.donation.cycle_duration_s,
                    user_s = self.donation_user_duration.as_secs(),
                    donation_s = self.donation_donation_duration.as_secs(),
                    // TEL-1: donation pool URLs can carry `user:pass@` — sanitize.
                    donation_pool = %crate::pool_api::sanitize_pool_url(&self.config.donation.pool_url),
                    donation_fallback_enabled = self.config.donation.fallback_enabled,
                    donation_fallback_pool = %crate::pool_api::sanitize_pool_url(&self.config.donation.fallback_pool_url),
                    "Donation enabled: {:.1}% — mining on user pool for {}s, \
                     then donation pool for {}s per {}-second cycle. \
                     Our autotuner saves more than 2% in energy — the donation can pay for itself. \
                     Fully configurable, fully disableable in Settings.",
                    self.config.donation.percent,
                    self.donation_user_duration.as_secs(),
                    self.donation_donation_duration.as_secs(),
                    self.config.donation.cycle_duration_s,
                );
            }
        }

        if self.user_split_enabled {
            info!(
                primary_pct = self.user_split_primary_bps as f32 / 100.0,
                secondary_pct = self.user_split_secondary_bps as f32 / 100.0,
                cycle_s = self.config.split_cycle_duration_s,
                primary_s = self.user_split_primary_duration.as_secs(),
                secondary_s = self.user_split_secondary_duration.as_secs(),
                // TEL-1: pool URLs may carry `user:pass@` — sanitize both.
                primary_pool = %crate::pool_api::sanitize_pool_url(&self.config.pool1.url),
                secondary_pool = %crate::pool_api::sanitize_pool_url(
                    self.config.pool2.as_ref().map(|pool| pool.url.as_str()).unwrap_or("unconfigured")
                ),
                "User hashrate split enabled: {:.2}% pool #1 / {:.2}% pool #2 using V1 time-sliced routing",
                self.user_split_primary_bps as f32 / 100.0,
                self.user_split_secondary_bps as f32 / 100.0,
            );
        } else if self
            .config
            .routing_mode
            .eq_ignore_ascii_case("weighted_split")
        {
            warn!(
                primary_bps = self.user_split_primary_bps,
                secondary_bps = self.user_split_secondary_bps,
                "User hashrate split was requested but not activated; falling back to normal failover routing"
            );
        }

        loop {
            if let Some(deadline) = sv2_retry_deadline {
                if Instant::now() >= deadline {
                    info!(
                        retry_interval_s = sv2_retry_after.map(|delay| delay.as_secs()),
                        "Auto mode: V1 fallback window expired, returning control to SV2"
                    );
                    return;
                }
            }

            // Determine which pool to connect to based on donation and user split routing.
            let donation_active = self.donation_phase == DonationPhase::Donation;
            let active_user_pool_index = if self.user_split_enabled {
                self.active_user_split_pool_index()
            } else {
                self.current_pool_index
            };
            let (pool, is_donation, active_pool_index) = if donation_active {
                (
                    self.donation_pool_config(self.donation_pool_index),
                    true,
                    active_user_pool_index,
                )
            } else {
                (
                    self.get_pool_config(active_user_pool_index),
                    false,
                    active_user_pool_index,
                )
            };
            // W5.5: keep stats donation-route fields fresh on every loop
            // tick so the dashboard can render the active donation route
            // (primary D-Central vs visible Braiins fallback) without
            // waiting for a phase-flip event. Cheap: one mutex acquire.
            self.refresh_donation_stats().await;

            // Calculate remaining time in active route phases for session timers.
            let donation_phase_remaining = self.time_remaining_in_phase();
            let user_split_remaining = if !is_donation && self.user_split_enabled {
                self.time_remaining_in_user_split_route()
            } else {
                Duration::ZERO
            };

            // BUG FIX (2026-04-11): Check for pool-directed reconnect target first.
            // pending_reconnect is set when a pool sends client.reconnect with a
            // specific host:port. take() consumes it — subsequent iterations
            // fall back to normal pool rotation.
            let endpoint = if let Some((h, p, tls)) = self.pending_reconnect.take() {
                // SW-09: preserve the originating session's TLS flag. A TLS pool
                // that sends `client.reconnect` must keep encryption on the
                // redirected connection — the prior hardcoded `tls: false` here
                // silently downgraded a TLS pool to plaintext on every reconnect.
                info!(host = %h, port = p, tls, "Using pool-directed reconnect target");
                pool_directed_reconnect_endpoint(h, p, tls)
            } else {
                match parse_pool_endpoint(&pool.url) {
                    Ok(endpoint) => endpoint,
                    Err(e) => {
                        error!(
                            %e,
                            // TEL-1: a malformed URL can still carry `user:pass@` — sanitize.
                            url = %crate::pool_api::sanitize_pool_url(&pool.url),
                            "Invalid pool URL — format should be 'stratum+tcp://hostname:port' (e.g., stratum+tcp://solo.ckpool.org:3333)"
                        );
                        if is_donation {
                            if self.try_switch_to_donation_fallback("invalid_donation_pool_url") {
                                continue;
                            }
                            // Donation pool URL invalid — return to user pool, no penalty
                            warn!("Donation pool URL invalid — resuming user pool mining");
                            self.resume_user_pool_after_donation_failure();
                            continue;
                        }
                        if !self.user_split_enabled {
                            // Try next user pool
                            self.current_pool_index = (self.current_pool_index + 1) % pool_count;
                        }
                        self.wait_with_share_drain(Duration::from_secs(1), "invalid pool url")
                            .await;
                        continue;
                    }
                }
            };
            let host = endpoint.host.clone();
            let port = endpoint.port;
            // SW-09: capture the TLS flag before `endpoint` is moved into the
            // connect call, so a pool-directed `client.reconnect` can preserve
            // this session's transport security on the redirected connection.
            let session_tls = endpoint.tls;

            // Attempt connection
            if is_donation {
                info!(
                    host = %host, port, tls = endpoint.tls,
                    phase_remaining_s = donation_phase_remaining.as_secs(),
                    "Connecting to donation pool {}:{} for {}s",
                    host, port, donation_phase_remaining.as_secs(),
                );
                self.send_status(StratumStatus::StateChanged(StratumState::Donating))
                    .await;
                self.send_hashrate_split_status(true).await;
            } else {
                // W1.4: mask wallet-shaped worker.
                info!(
                    pool_index = active_pool_index,
                    host = %host,
                    port,
                    tls = endpoint.tls,
                    worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
                    split_route = self.user_split_enabled,
                    split_remaining_s = user_split_remaining.as_secs(),
                    "Connecting to pool {}:{} (pool #{}) — establishing TCP socket for Stratum JSON-RPC",
                    host, port, active_pool_index + 1,
                );
                self.send_status(StratumStatus::StateChanged(StratumState::Connecting))
                    .await;
                self.send_hashrate_split_status(false).await;
            }

            let user_failure_reason: String;

            match self
                .connect_endpoint_with_share_drain(endpoint, CONNECT_TIMEOUT)
                .await
            {
                Ok(conn) => {
                    // POOL-1: snapshot both the job AND accepted-share counters
                    // before the session so we can later tell a genuinely healthy
                    // session (accepted a share, or stayed up) apart from a sick
                    // primary that delivers one job then drops.
                    let (jobs_before_session, shares_accepted_before_session) = {
                        let stats = self.stats.lock().await;
                        (stats.jobs_received, stats.shares_accepted)
                    };
                    let session_started_at = Instant::now();
                    let session_result = self
                        .run_session(
                            conn,
                            &pool,
                            SessionContext {
                                donation_phase_remaining,
                                user_split_remaining,
                                is_donation,
                                active_pool_index,
                                sv2_retry_remaining: sv2_retry_deadline.map(|deadline| {
                                    deadline.saturating_duration_since(Instant::now())
                                }),
                            },
                        )
                        .await;

                    self.last_pending_submit_correlations_cleared = self
                        .clear_orphaned_pending_submits(if is_donation {
                            "donation_session_end"
                        } else {
                            "user_session_end"
                        });
                    let session_uptime = session_started_at.elapsed();
                    let (session_delivered_work, session_accepted_share) = {
                        let mut stats = self.stats.lock().await;
                        stats.shares_unresolved = self.pending_submits.len() as u64;
                        stats.pending_submit_dropped = self.pending_submit_dropped;
                        (
                            stats.jobs_received > jobs_before_session,
                            stats.shares_accepted > shares_accepted_before_session,
                        )
                    };
                    // POOL-1: only zero the reconnect backoff when the session was
                    // actually HEALTHY — it delivered work AND either landed an
                    // accepted share or stayed connected past the handshake/settle
                    // window. Merely receiving one job is not enough: a primary
                    // that delivers one notify then drops would otherwise reset the
                    // counter on every reconnect and the `attempt() >= 3` failover
                    // gate could never trip, stranding the miner on a broken pool.
                    // A genuinely healthy primary still resets (it submits shares
                    // and/or stays up), so normal reconnect/failover is unchanged.
                    let session_was_healthy = session_delivered_work
                        && (session_accepted_share || session_uptime >= SESSION_HEALTHY_UPTIME);
                    if !is_donation && session_was_healthy {
                        backoff.reset();
                    }

                    match session_result {
                        Ok(SessionEndReason::DonationSwitch) => {
                            self.flush_dispatcher_for_pool_switch(is_donation).await;
                            // Timer fired — flip phase and reconnect immediately
                            if is_donation {
                                // Track time spent on donation pool
                                self.donation_total_time_s +=
                                    self.donation_donation_duration.as_secs();
                            }
                            self.flip_donation_phase();
                            // W5.5: keep the stats donation-route fields in
                            // lockstep with phase flips so the dashboard
                            // surface never lags the actual session.
                            self.refresh_donation_stats().await;
                            let donation_now_active =
                                self.donation_phase == DonationPhase::Donation;
                            // W5.5: pull the active route from the donation
                            // config rather than re-resolving so the event
                            // stays consistent with refresh_donation_stats.
                            let (route_url, route_worker, route_idx) = if donation_now_active {
                                let cfg = self.donation_pool_config(self.donation_pool_index);
                                (cfg.url, cfg.worker, self.donation_pool_index)
                            } else {
                                (String::new(), String::new(), 0)
                            };
                            self.send_status(StratumStatus::DonationStateChanged {
                                active: donation_now_active,
                                percent: self.config.donation.percent,
                                cycle_remaining_s: self.time_remaining_in_phase().as_secs(),
                                active_url: route_url,
                                active_worker: route_worker,
                                pool_index: route_idx,
                            })
                            .await;
                            self.send_hashrate_split_status(
                                self.donation_phase == DonationPhase::Donation,
                            )
                            .await;
                            continue; // No backoff on donation switch
                        }
                        Ok(SessionEndReason::UserSplitSwitch) => {
                            self.flush_dispatcher_for_pool_switch(false).await;
                            self.user_split_switch_count =
                                self.user_split_switch_count.saturating_add(1);
                            self.send_hashrate_split_status(false).await;
                            continue; // No backoff on planned user split switch
                        }
                        Ok(SessionEndReason::Clean) => {
                            user_failure_reason = "session_clean_end".to_string();
                            info!(
                                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                                "Pool session ended cleanly — will reconnect"
                            );
                        }
                        Ok(SessionEndReason::AutoRetrySv2) => {
                            user_failure_reason = "auto_retry_sv2".to_string();
                            info!(
                                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                                "Auto mode: leaving V1 fallback to retry preferred SV2 endpoint"
                            );
                        }
                        Ok(SessionEndReason::Reconnect {
                            host,
                            port,
                            wait_seconds,
                        }) => {
                            // BUG FIX (2026-04-11): Honor pool-directed reconnect target.
                            // Store the requested endpoint; the next loop iteration
                            // will use it instead of the configured pool URL.
                            info!(
                                host = %host, port, wait_seconds,
                                "Honoring pool-directed reconnect to {}:{}", host, port,
                            );
                            // SEC: rate-floor pool-directed reconnects. A hostile /
                            // compromised / MITM plaintext pool can answer every
                            // handshake with `client.reconnect [attacker_host, port]`
                            // and omit `wait_seconds` (=> 0); the old `backoff.reset()`
                            // + immediate `continue` was an unthrottled reconnect storm
                            // to an attacker-chosen host — a remotely-triggerable mining
                            // outage. Wait at least the current backoff step (100 ms
                            // floor / 60 s cap, jittered) OR the pool's requested wait,
                            // whichever is longer, and do NOT reset backoff — so
                            // repeated redirects back off geometrically. `next_delay()`
                            // advances the counter; backoff is reset elsewhere only
                            // after a session proves healthy, so a legitimate single
                            // reconnect still proceeds within ~one backoff step.
                            let floor = backoff.next_delay();
                            let wait = reconnect_wait_floor(wait_seconds, floor);
                            self.wait_with_share_drain(wait, "pool-directed reconnect")
                                .await;
                            // SW-09: carry the originating session's TLS flag so a
                            // TLS pool isn't downgraded to plaintext on reconnect.
                            self.pending_reconnect = Some((host, port, session_tls));
                            continue;
                        }
                        Err(SessionError::AuthorizationFailed(ref reason)) => {
                            user_failure_reason = "authorization_failed".to_string();
                            if is_donation {
                                if self.try_switch_to_donation_fallback(
                                    "donation_authorization_failed",
                                ) {
                                    continue;
                                }
                                error!(
                                    reason = %reason,
                                    "Donation pool rejected credentials — disabling donation for this session. \
                                     User mining continues normally."
                                );
                                self.donation_phase = DonationPhase::Disabled;
                                continue;
                            }
                            // W1.4: mask wallet-shaped worker even on auth-fail
                            // path — the failure message itself can ride syslog
                            // tunnels and we don't want the full address there.
                            error!(
                                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                                worker = %dcentrald_common::wallet_mask::mask_wallet(&pool.worker),
                                reason = %reason,
                                "Pool REJECTED our worker credentials. For solo mining pools, the worker name must be a valid bitcoin address. For regular pools, check your account settings."
                            );
                        }
                        Err(SessionError::HandshakeTimeout) => {
                            user_failure_reason = "handshake_timeout".to_string();
                            if is_donation {
                                if self
                                    .try_switch_to_donation_fallback("donation_handshake_timeout")
                                {
                                    continue;
                                }
                                warn!(
                                    "Donation pool handshake timed out — resuming user pool mining"
                                );
                                self.resume_user_pool_after_donation_failure();
                                continue;
                            }
                            warn!(
                                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                                "Pool handshake timed out after 30s — the pool server may be overloaded or not responding. Will try next pool."
                            );
                        }
                        Err(e) => {
                            user_failure_reason = format!("session_error:{}", e);
                            if is_donation {
                                if self.try_switch_to_donation_fallback("donation_session_error") {
                                    continue;
                                }
                                warn!(
                                    error = %e,
                                    "Donation pool session error — resuming user pool mining. \
                                     No penalty, no lost mining time."
                                );
                                self.resume_user_pool_after_donation_failure();
                                continue;
                            }
                            warn!(
                                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                                error = %e,
                                "Pool session ended with error — will reconnect automatically"
                            );
                        }
                    }
                }
                Err(e) => {
                    user_failure_reason = format!("connect_error:{}", e);
                    if is_donation {
                        if self.try_switch_to_donation_fallback("donation_connect_error") {
                            continue;
                        }
                        warn!(
                            %e,
                            // TEL-1: donation URL may carry `user:pass@` — sanitize.
                            donation_url = %crate::pool_api::sanitize_pool_url(&pool.url),
                            "Donation pool unreachable — resuming user pool mining. \
                             No penalty, no lost mining time. Will retry next cycle."
                        );
                        self.resume_user_pool_after_donation_failure();
                        continue;
                    }
                    error!(
                        %e, host = %host, port,
                        "Failed to connect to pool {}:{} — check internet connection, DNS resolution, firewall rules, or pool server status",
                        host, port,
                    );
                }
            }

            // Disconnected (user pool path only — donation failures handled above)
            // FWT-3: surface an authorization failure as a distinct AuthFailed
            // telemetry state so a wrong-worker / banned-wallet operator sees an
            // actionable "check your credentials" signal instead of indefinite
            // connecting/disconnected churn. Computed before user_failure_reason
            // is moved into last_failover_failure_reason below.
            let disconnect_state = if user_failure_reason == "authorization_failed" {
                StratumState::AuthFailed
            } else {
                StratumState::Disconnected
            };
            self.last_failover_failure_reason = Some(user_failure_reason);
            self.last_failover_failure_pool_index = Some(active_pool_index);
            self.send_status(StratumStatus::StateChanged(disconnect_state))
                .await;
            {
                let mut stats = self.stats.lock().await;
                stats.connected = false;
            }

            // POOL-3 (honest framing): on a same-pool reconnect the work
            // dispatcher is NOT flushed (only a pool *switch* flushes it via
            // flush_dispatcher_for_pool_switch), so the ASICs keep hashing the
            // last job across the disconnect REGARDLESS of this flag. The
            // `hash_on_disconnect` flag therefore does NOT act as a safety stop
            // when false — it currently only governs whether we emit the
            // informational note below. This is intentional for the
            // space-heater posture (continuing to hash the last job prevents
            // thermal shock from sudden power changes). Do not read the `false`
            // case as "stops hashing on pool loss"; it does not. The actual
            // "don't spin hot forever" backstop is the thermal supervisor
            // (PID/threshold loop) — chips hashing stale work are bounded by
            // measured temperature, not by pool connection state.
            if self.config.hash_on_disconnect && self.last_job.is_some() {
                info!("Hash-on-disconnect note — ASICs continue hashing the last job while we reconnect. This prevents thermal shock from sudden power changes and doesn't waste electricity. (Note: a same-pool reconnect keeps the last job active regardless of this flag; only a pool switch flushes work.)");
            }

            if matches!(sv2_retry_deadline, Some(deadline) if Instant::now() >= deadline) {
                info!("Auto mode: retrying SV2 after temporary V1 fallback");
                return;
            }

            // Try next pool on repeated failures.
            // Only reset backoff when switching to a pool that hasn't been tried
            // in this failure cycle. When wrapping back to the first pool (full
            // cycle complete), keep the backoff accumulating to avoid rapid cycling
            // when all pools are down.
            if !self.user_split_enabled && backoff.attempt() >= 3 {
                // Arm the stable-primary-return cool-down the moment we
                // fail off the primary itself.
                if self.current_pool_index == 0 {
                    self.last_primary_failure_at = Some(Instant::now());
                }
                let prefer_primary = should_prefer_primary_return(
                    self.current_pool_index,
                    self.last_primary_failure_at,
                    Duration::from_secs(self.config.primary_return_stability_secs),
                    Instant::now(),
                );
                let next = if prefer_primary {
                    0
                } else {
                    (self.current_pool_index + 1) % pool_count
                };
                if prefer_primary {
                    // Anti-flap stable-primary-return: the active backup
                    // faulted and the primary cool-down fully elapsed →
                    // prefer the (recovered) primary over round-robining
                    // onward. Cannot oscillate (window >> backoff).
                    self.flush_dispatcher_for_pool_switch(is_donation).await;
                    self.failover_switch_count = self.failover_switch_count.saturating_add(1);
                    self.last_failover_switch_reason = Some("primary_stable_return".to_string());
                    let primary_pool = self.get_pool_config(0);
                    // TEL-1: pool URL can carry `user:pass@` — sanitize for BOTH the
                    // structured field and the interpolated message body.
                    let primary_pool_display =
                        crate::pool_api::sanitize_pool_url(&primary_pool.url);
                    info!(
                        old_pool = self.current_pool_index + 1,
                        new_url = %primary_pool_display,
                        cooldown_s = self.config.primary_return_stability_secs,
                        "Stable-primary-return: returning to primary pool #1 ({}) after {}s anti-flap cool-down",
                        primary_pool_display, self.config.primary_return_stability_secs,
                    );
                    backoff.reset();
                } else if next == 0 {
                    // Full cycle of all pools — keep backoff growing
                    self.flush_dispatcher_for_pool_switch(is_donation).await;
                    self.failover_switch_count = self.failover_switch_count.saturating_add(1);
                    self.last_failover_switch_reason =
                        Some("all_configured_pools_exhausted".to_string());
                    warn!(
                        "All {} pools exhausted in this cycle — backoff continues",
                        pool_count
                    );
                } else {
                    self.flush_dispatcher_for_pool_switch(is_donation).await;
                    self.failover_switch_count = self.failover_switch_count.saturating_add(1);
                    self.last_failover_switch_reason =
                        Some("consecutive_failure_threshold".to_string());
                    let next_pool = self.get_pool_config(next);
                    // TEL-1: pool URL can carry `user:pass@` — sanitize for BOTH the
                    // structured field and the interpolated message body.
                    let next_pool_display = crate::pool_api::sanitize_pool_url(&next_pool.url);
                    info!(
                        old_pool = self.current_pool_index + 1,
                        new_pool = next + 1,
                        new_url = %next_pool_display,
                        "Failover: switching to pool #{} ({}) after 3 consecutive failures on pool #{}",
                        next + 1, next_pool_display, self.current_pool_index + 1,
                    );
                    backoff.reset();
                }
                self.current_pool_index = next;
                self.send_failover_status("pool_switch", backoff.attempt(), 0)
                    .await;
            }

            // Wait with backoff before reconnecting
            let delay = backoff.next_delay();
            self.send_failover_status(
                "reconnect_backoff",
                backoff.attempt(),
                delay.as_millis() as u64,
            )
            .await;
            info!(
                delay_ms = delay.as_millis() as u64,
                attempt = backoff.attempt(),
                "Waiting {:.1}s before reconnect (exponential backoff, attempt {})",
                delay.as_secs_f32(),
                backoff.attempt(),
            );
            self.wait_with_share_drain(delay, "reconnect backoff").await;
        }
    }

    /// Run a single pool session (connect -> handshake -> mining loop).
    ///
    /// `donation_phase_remaining`: how long until the donation timer fires.
    /// `user_split_remaining`: how long until the weighted user split route changes.
    async fn run_session(
        &mut self,
        mut conn: StratumConnection,
        pool: &PoolConfig,
        context: SessionContext,
    ) -> Result<SessionEndReason, SessionError> {
        let SessionContext {
            donation_phase_remaining,
            user_split_remaining,
            is_donation,
            active_pool_index,
            sv2_retry_remaining,
        } = context;
        self.mining_state_announced = false;

        // === STRATUM V1 HANDSHAKE ===
        // The Stratum V1 handshake is a 3-step JSON-RPC sequence over TCP:
        //   1. mining.configure — negotiate ASICBoost version rolling (optional)
        //   2. mining.subscribe — register with pool, get extranonce1 (our unique session ID)
        //   3. mining.authorize — authenticate with worker name + password
        // After all three succeed, the pool starts sending mining.notify (new jobs).
        info!(
            pool = %crate::pool_api::sanitize_pool_url(&pool.url),
            "Starting Stratum V1 handshake — 3-step sequence to register with pool"
        );

        // Step 1: mining.configure (version rolling / ASICBoost, optional)
        // BIP 310: We tell the pool which version bits we want to roll. The pool
        // responds with the mask it allows. This enables ASICBoost (~20% hashrate gain).
        if self.config.version_rolling {
            info!(
                requested_mask = format_args!("0x{:08X}", self.config.version_rolling_mask),
                "Handshake step 1/3: mining.configure — requesting ASICBoost version rolling"
            );
            let requested_mask = format!("{:08x}", self.config.version_rolling_mask);
            let req = configure_request(
                ID_CONFIGURE,
                &requested_mask,
                self.config.suggest_difficulty,
            );
            conn.write_line(&serialize_request(&req))
                .await
                .map_err(SessionError::Connection)?;
        } else {
            info!("Handshake step 1/3: mining.configure — SKIPPED (version rolling disabled in config)");
        }

        // Step 2: mining.subscribe — register as a new miner session
        // The pool assigns us an extranonce1 (unique session prefix) and tells us
        // how many bytes of extranonce2 we control. Together, extranonce1+extranonce2
        // make each coinbase transaction unique per miner and per work unit.
        info!(
            user_agent = USER_AGENT,
            "Handshake step 2/3: mining.subscribe — registering as '{}' to get our session extranonce",
            USER_AGENT,
        );
        let req = subscribe_request(ID_SUBSCRIBE, USER_AGENT);
        conn.write_line(&serialize_request(&req))
            .await
            .map_err(SessionError::Connection)?;

        // Step 3: mining.authorize — prove we're allowed to mine on this pool
        // Worker name format is typically "username.worker" or a bitcoin address for solo pools.
        // W1.4: mask wallet-shaped worker in BOTH the structured field AND the
        // human-readable message body (the format-string `{}` was emitting the
        // full address before).
        let masked_worker = dcentrald_common::wallet_mask::mask_wallet(&pool.worker);
        info!(
            worker = %masked_worker,
            "Handshake step 3/3: mining.authorize — authenticating worker '{}' with pool",
            masked_worker,
        );
        let req = authorize_request(ID_AUTHORIZE, &pool.worker, &pool.password);
        conn.write_line(&serialize_request(&req))
            .await
            .map_err(SessionError::Connection)?;

        // Step 4: mining.suggest_difficulty (optional)
        // Static startup hint only: the pool may ignore it or later override it
        // with mining.set_difficulty; the pool's live difficulty messages remain
        // authoritative after handshake.
        if let Some(diff) = self.config.suggest_difficulty {
            info!(
                suggested_diff = diff,
                "Suggesting initial difficulty {} to pool (pool may ignore this)", diff,
            );
            let req = suggest_difficulty_request(ID_SUGGEST_DIFF, diff);
            conn.write_line(&serialize_request(&req))
                .await
                .map_err(SessionError::Connection)?;
        }

        // Wait for handshake responses (pool responds asynchronously to all three requests)
        let mut subscribed = false;
        let mut authorized = false;
        // G28 / F-002 (observability): tracks whether the pool answered mining.configure.
        // The handshake loop exits on subscribed && authorized — it does NOT block on
        // configure — so a legacy pool that silently drops mining.configure (e.g. AntPool
        // :3333) already proceeds with version_mask=0 (no ASICBoost). This flag lets us emit
        // a clear "no ASICBoost because the pool didn't answer configure" line at handshake
        // completion instead of leaving the operator to guess why version-rolling is off.
        let mut configure_responded = false;
        let mut pending_notify: Option<PoolMessage> = None;
        let handshake_timeout = Instant::now() + HANDSHAKE_TIMEOUT;

        let mut handshake_share_rx_open = true;
        let mut handshake_dropped_shares = 0u64;

        while !subscribed || !authorized {
            if Instant::now() > handshake_timeout {
                Self::log_disconnected_share_summary("handshake", handshake_dropped_shares);
                return Err(SessionError::HandshakeTimeout);
            }

            let handshake_remaining = handshake_timeout.saturating_duration_since(Instant::now());
            if handshake_remaining.is_zero() {
                Self::log_disconnected_share_summary("handshake", handshake_dropped_shares);
                return Err(SessionError::HandshakeTimeout);
            }

            let line = tokio::select! {
                line = tokio::time::timeout(handshake_remaining, conn.read_line()) => {
                    line
                        .map_err(|_| SessionError::HandshakeTimeout)?
                        .map_err(SessionError::Connection)?
                        .ok_or(SessionError::Disconnected)?
                }
                share = self.share_rx.recv(), if handshake_share_rx_open => {
                    match share {
                        Some(share) => {
                            handshake_dropped_shares += 1;
                            Self::log_disconnected_share_drop(
                                "handshake",
                                handshake_dropped_shares,
                                &share,
                            );
                        }
                        None => {
                            handshake_share_rx_open = false;
                        }
                    }
                    continue;
                }
            };

            let msg =
                parse_pool_message(&line).map_err(|e| SessionError::ParseError(e.to_string()))?;

            match msg {
                PoolMessage::Response { id, result, error } => {
                    if let Some(err) = &error {
                        if !err.is_null() {
                            error!(?err, id, "Handshake error from pool");
                            if id == ID_AUTHORIZE {
                                return Err(SessionError::AuthorizationFailed(format!(
                                    "{:?}",
                                    err
                                )));
                            }
                        }
                    }

                    match id {
                        ID_CONFIGURE => {
                            // G28: the pool answered mining.configure (mask presence is
                            // checked below; this just records that a response arrived).
                            configure_responded = true;
                            // Parse version rolling response — the pool tells us which
                            // bits in the block version field we're allowed to roll.
                            // These rolled bits let ASICs explore more nonce space
                            // (ASICBoost / BIP 310), giving ~20% hashrate boost.
                            if let Some(result) = result {
                                if let Some(mask) =
                                    result.get("version-rolling.mask").and_then(|v| v.as_str())
                                {
                                    let pool_mask = match parse_version_mask(mask) {
                                        Ok(mask) => mask,
                                        Err(error) => {
                                            warn!(
                                                mask,
                                                %error,
                                                "Ignoring invalid version-rolling.mask from mining.configure"
                                            );
                                            continue;
                                        }
                                    };
                                    self.version_mask = parse_and_clamp_version_mask(
                                        mask,
                                        self.config.version_rolling_mask,
                                    )
                                    .unwrap_or(0);
                                    if pool_mask != self.version_mask {
                                        warn!(
                                            pool_mask = %format_version_mask(pool_mask),
                                            requested_mask = %format_version_mask(self.config.version_rolling_mask),
                                            effective_mask = %format_version_mask(self.version_mask),
                                            "Pool version-rolling mask exceeded configured operator mask; clamping to requested bits"
                                        );
                                    }
                                    self.work_builder.set_version_mask(self.version_mask);
                                    let rollable_bits = self.version_mask.count_ones();
                                    info!(
                                        mask = %format_version_mask(self.version_mask),
                                        rollable_bits,
                                        "ASICBoost negotiated — pool allows rolling {} bits in block version (mask 0x{:08X}). This gives ASICs {} extra version combinations per nonce range.",
                                        rollable_bits, self.version_mask, 2u64.pow(rollable_bits),
                                    );
                                } else {
                                    info!("Pool responded to mining.configure but didn't include a version-rolling mask — ASICBoost may not be supported by this pool");
                                }
                                let minimum_difficulty_enabled = result
                                    .get("minimum-difficulty")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let subscribe_extranonce_enabled = result
                                    .get("subscribe-extranonce")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                if minimum_difficulty_enabled || subscribe_extranonce_enabled {
                                    info!(
                                        minimum_difficulty = minimum_difficulty_enabled,
                                        subscribe_extranonce = subscribe_extranonce_enabled,
                                        "Pool accepted additional mining.configure extensions"
                                    );
                                }
                            }
                        }
                        ID_SUBSCRIBE => {
                            // Parse subscribe response: [[subscriptions], extranonce1, extranonce2_size]
                            // extranonce1 is our unique session ID from the pool.
                            // extranonce2 is the part WE control — we increment it for
                            // each new work unit to make every coinbase transaction unique.
                            if let Some(result) = result {
                                self.parse_subscribe_result(&result)?;
                                subscribed = true;
                                let en2_desc = if self.extranonce2_size >= 8 {
                                    "virtually unlimited (2^64+)".to_string()
                                } else {
                                    format!("{}", 256u64.pow(self.extranonce2_size as u32))
                                };
                                info!(
                                    extranonce1 = hex::encode(&self.extranonce1),
                                    extranonce1_bytes = self.extranonce1.len(),
                                    extranonce2_size = self.extranonce2_size,
                                    "Subscribed to pool — our session ID (extranonce1) is 0x{} ({} bytes). We control {} bytes of extranonce2, giving us {} unique work units per job before recycling.",
                                    hex::encode(&self.extranonce1),
                                    self.extranonce1.len(),
                                    self.extranonce2_size,
                                    en2_desc,
                                );
                            }
                        }
                        ID_AUTHORIZE => {
                            if result == Some(Value::Bool(true)) {
                                authorized = true;
                                // W1.4: mask wallet-shaped worker in both fields.
                                let masked =
                                    dcentrald_common::wallet_mask::mask_wallet(&pool.worker);
                                info!(
                                    worker = %masked,
                                    "Authorized with pool — worker '{}' accepted. We can now receive jobs and submit shares.",
                                    masked,
                                );
                            } else {
                                return Err(SessionError::AuthorizationFailed(format!(
                                    "result={:?}",
                                    result
                                )));
                            }
                        }
                        _ => {
                            debug!(id, "Unexpected response during handshake (id={}) — this is usually harmless, could be a pool extension we don't support", id);
                        }
                    }
                }
                PoolMessage::SetDifficulty(diff) => {
                    let old_diff = self.current_difficulty;
                    self.current_difficulty = diff;
                    self.send_status(StratumStatus::DifficultyChanged(diff))
                        .await;
                    info!(
                        difficulty = diff,
                        previous = old_diff,
                        "Initial difficulty set to {} — this controls how hard each share is. Lower difficulty = more shares found = faster feedback. The pool adjusts this based on our hashrate.",
                        diff,
                    );
                }
                PoolMessage::Notify { .. } => {
                    // Sometimes the pool sends the first job before handshake completes.
                    // If we're subscribed (have extranonce), process it immediately.
                    // Otherwise, buffer it — we'll process it after handshake completes.
                    if subscribed {
                        self.handle_notify(msg).await?;
                    } else {
                        info!("Buffering mining.notify received before subscribe — will process after handshake");
                        pending_notify = Some(msg);
                    }
                }
                _ => {
                    debug!(?msg, "Ignoring message during handshake");
                }
            }
        }

        Self::log_disconnected_share_summary("handshake", handshake_dropped_shares);

        // Process any mining.notify that arrived before subscribe completed.
        // This prevents dropping the first job when pools send notify eagerly.
        if let Some(notify_msg) = pending_notify.take() {
            info!("Processing buffered mining.notify from handshake phase");
            self.handle_notify(notify_msg).await?;
        }

        // === HANDSHAKE AUTHORIZED ===
        info!(
            pool = %crate::pool_api::sanitize_pool_url(&pool.url),
            difficulty = self.current_difficulty,
            extranonce1 = hex::encode(&self.extranonce1),
            version_mask = format_args!("0x{:08X}", self.version_mask),
            "=== HANDSHAKE COMPLETE; AUTHORIZED === Pool accepted us. Waiting for mining.notify before reporting Mining; a job must reach the dispatcher before ASICs can hash pool work."
        );

        // G28 / F-002: if we requested ASICBoost version-rolling but the pool never
        // answered mining.configure, say so explicitly. The session already proceeded
        // (the handshake loop only requires subscribe+authorize), so this is operator
        // clarity, not an error — common on legacy pools (e.g. AntPool :3333) that
        // silently drop mining.configure. version_mask stays 0 ⇒ mining without ASICBoost.
        if self.config.version_rolling && !configure_responded {
            info!(
                pool = %crate::pool_api::sanitize_pool_url(&pool.url),
                version_mask = format_args!("0x{:08X}", self.version_mask),
                "Pool did not answer mining.configure — mining WITHOUT ASICBoost version-rolling (version_mask=0). This is normal on legacy pools that don't support the configure method; not an error."
            );
        }

        // W11.13 (Bitmain extension) — explicit `mining.extranonce.subscribe`.
        // We already advertise `subscribe-extranonce` inside `mining.configure`
        // (BIP310 capability bag), but Bitmain-flavored pools often key off the
        // explicit method call instead and silently ignore the bag. Send both
        // so we cover bmminer-aware pools and BIP310-only pools without a
        // config switch.
        //
        // Failure handling: if the pool doesn't implement the extension it
        // returns `error: "Method not found"` (or similar). The response lands
        // in `PoolMessage::Response { id == ID_EXTRANONCE_SUBSCRIBE, ... }`
        // and is debug-logged via the catch-all `_ => debug!(...)` arm. The
        // session is never torn down on a missing extension — extranonce
        // rotation simply won't occur, which matches the previous (no-explicit-
        // request) behavior. Consumers that care can also pin the existing
        // `mining.set_extranonce` handler test.
        let extranonce_sub_req = extranonce_subscribe_request(ID_EXTRANONCE_SUBSCRIBE);
        debug!(
            id = ID_EXTRANONCE_SUBSCRIBE,
            "Sending Bitmain extension `mining.extranonce.subscribe` so the pool can rotate our extranonce1 mid-session without forcing a full reconnect"
        );
        if let Err(e) = conn
            .write_line(&serialize_request(&extranonce_sub_req))
            .await
        {
            // Don't fail the session: this is purely opportunistic. The
            // mining loop will surface any real socket failure on the next
            // read/write cycle.
            warn!(
                ?e,
                "Failed to send mining.extranonce.subscribe — continuing without explicit extension request"
            );
        }

        self.send_status(StratumStatus::StateChanged(StratumState::Authorized))
            .await;
        {
            let mut stats = self.stats.lock().await;
            stats.connected = true;
            stats.active_pool_index = active_pool_index;
            stats.current_difficulty = self.current_difficulty;
        }
        self.send_failover_status("session_mining", 0, 0).await;
        self.send_hashrate_split_status(is_donation).await;

        // Measure session start for uptime tracking
        let session_start = Instant::now();

        // === MINING LOOP ===
        // Process pool messages and submit shares concurrently.
        // This is the steady-state: we receive jobs, build work, and submit shares.

        // Donation timer: created once before the loop, not on every iteration.
        // When donation is disabled (phase_remaining is zero), the conditional
        // guard on the select! branch prevents it from ever firing.
        let phase_remaining = donation_phase_remaining;
        let donation_timer = tokio::time::sleep(if phase_remaining.is_zero() {
            // Use a very long duration that will never fire — the guard prevents polling
            Duration::from_secs(86400 * 365)
        } else {
            phase_remaining
        });
        tokio::pin!(donation_timer);

        let user_split_timer = tokio::time::sleep(if user_split_remaining.is_zero() {
            Duration::from_secs(86400 * 365)
        } else {
            user_split_remaining
        });
        tokio::pin!(user_split_timer);

        let sv2_retry_timer =
            tokio::time::sleep(sv2_retry_remaining.unwrap_or(Duration::from_secs(86400 * 365)));
        tokio::pin!(sv2_retry_timer);

        // Pool-failover increment 2: no-`mining.notify` failover watchdog.
        // last_notify_at is (re)set here — handshake just completed, so we
        // now expect jobs — and on every Notify; the pure predicate
        // decides. Conservative default; reuses the existing failover path.
        let no_notify_failover_secs = self.config.no_notify_failover_secs;
        let mut last_notify_at = Instant::now();
        let mut no_notify_check = tokio::time::interval(Duration::from_secs(15));
        no_notify_check.tick().await; // consume the immediate first tick

        // Pool-failover increment 3: reject-rate failover (opt-in,
        // default-disabled). Session-scoped via a baseline snapshot at
        // handshake-complete; evaluated on the same 15s watchdog tick.
        let reject_rate_failover_pct = self.config.reject_rate_failover_pct;
        let reject_rate_failover_min_samples = self.config.reject_rate_failover_min_samples;
        let (reject_baseline_accepted, reject_baseline_rejected) = {
            let s = self.stats.lock().await;
            (s.shares_accepted, s.shares_rejected)
        };

        loop {
            if let Some(pending) = self.pending_share.take() {
                let share = pending.share;

                // SW-03: ntime validity-window check BEFORE submit. A share whose
                // ntime has drifted beyond ±2h from the pool's current job ntime
                // is consensus-invalid and a guaranteed pool reject — submitting
                // it only burns a reject slot (and can nudge a false reject-rate
                // failover). We check against the most recent dispatched job
                // (`last_job`), which is the freshest pool-supplied ntime we have.
                // Fail-open on parse error or no job reference: never silently
                // drop a share over a parse quirk (preserves prior behavior).
                if let Some(ref job) = self.last_job {
                    match ntime_within_window(&share.ntime, job.ntime, NTIME_VALIDITY_WINDOW_SECS) {
                        Ok(false) => {
                            warn!(
                                target: "stratum_v1",
                                job_id = %share.job_id,
                                share_ntime = %share.ntime,
                                job_ntime = job.ntime,
                                window_secs = NTIME_VALIDITY_WINDOW_SECS,
                                "Dropping share pre-submit: ntime drifted beyond the \
                                 ±2h consensus window vs the current job — it cannot be \
                                 accepted and would only burn a pool reject slot."
                            );
                            continue;
                        }
                        Ok(true) => { /* within window — proceed */ }
                        Err(()) => {
                            debug!(
                                target: "stratum_v1",
                                share_ntime = %share.ntime,
                                "Unparseable share ntime — submitting anyway (fail-open)."
                            );
                        }
                    }
                }

                // SW-04: per-job nonce dedup. A duplicate (job_id, nonce,
                // version_bits) is a guaranteed pool reject; drop it before it
                // reaches the wire. Bounded to the last few jobs (see NonceDedup).
                if self.nonce_dedup.is_duplicate_then_record(
                    &share.job_id,
                    &share.nonce,
                    share.version_bits.as_deref(),
                ) {
                    debug!(
                        target: "stratum_v1",
                        job_id = %share.job_id,
                        nonce = %share.nonce,
                        version_bits = ?share.version_bits,
                        "Dropping duplicate share pre-submit (same job/nonce/version \
                         already submitted) — would be a guaranteed pool reject."
                    );
                    continue;
                }

                // Primary donation submits use the configured donation worker.
                // Donation fallback and user routes submit as their active pool worker.
                let submit_worker = if is_donation && self.donation_pool_index == 0 {
                    self.config.donation.worker.clone()
                } else {
                    pool.worker.clone()
                };

                let submit_id = self.next_request_id();
                let submit_time = Instant::now();
                debug!(
                    id = submit_id,
                    job_id = %share.job_id,
                    nonce = %share.nonce,
                    ntime = %share.ntime,
                    extranonce2 = %share.extranonce2,
                    version_bits = ?share.version_bits,
                    donation = is_donation,
                    active_pool_index,
                    "Submitting share to {} — ASIC found nonce 0x{} for job '{}'.",
                    if is_donation { "donation pool" } else { "pool" },
                    share.nonce, share.job_id,
                );
                let req = submit_request(
                    submit_id,
                    &submit_worker,
                    &share.job_id,
                    &share.extranonce2,
                    &share.ntime,
                    &share.nonce,
                    share.version_bits.as_deref(),
                );
                let json = serialize_request(&req);
                // NOTE: do NOT log the raw serialized submit at INFO — params[0] is
                // the operator's full payout wallet (worker), and INFO/WARN/ERROR
                // must never carry a full address (wallet_mask.rs contract; this is
                // a release-INDEPENDENT PII leak via /tmp/dcentrald.log). The
                // structured debug! above already captures the full safe diagnostic
                // (id/job_id/nonce/ntime/extranonce2/version_bits) without the worker.
                if let Err(e) = conn.write_line(&json).await {
                    self.pending_share = Some(PendingSubmit { share });
                    return Err(SessionError::Connection(e));
                }

                self.pending_submits.push(PendingSubmitResponse::new(
                    submit_id,
                    submit_time,
                    share,
                    submit_worker,
                ));
                let pending_submit_dropped = self.trim_pending_submits();

                let mut stats = self.stats.lock().await;
                stats.shares_submitted += 1;
                stats.shares_unresolved = self.pending_submits.len() as u64;
                if pending_submit_dropped > 0 {
                    stats.pending_submit_dropped = self.pending_submit_dropped;
                }
                if is_donation {
                    stats.donation_shares += 1;
                    self.donation_total_shares += 1;
                } else if self.user_split_enabled && active_pool_index == 1 {
                    stats.hashrate_split.secondary_shares =
                        stats.hashrate_split.secondary_shares.saturating_add(1);
                    self.user_split_secondary_shares =
                        self.user_split_secondary_shares.saturating_add(1);
                }
            }

            tokio::select! {
                // Read from pool
                line = conn.read_line() => {
                    let line = line.map_err(SessionError::Connection)?
                        .ok_or(SessionError::Disconnected)?;

                    // STRAT-2 (2026-06-20): a single non-JSON / malformed inbound
                    // line must NOT tear down a healthy session. Real pools
                    // occasionally emit keepalive junk, blank lines, or
                    // out-of-spec noise; the previous `?` propagated
                    // SessionError::ParseError, which dropped the socket and
                    // forced a full reconnect + work flush for every stray byte.
                    // `parse_pool_message` only returns Err for genuine
                    // un-parseable (non-JSON) input — every protocol-level
                    // anomaly (wrong types, missing fields, unknown methods) is
                    // already routed to `PoolMessage::Unknown` and handled below.
                    // So skipping on Err is exactly "drop un-parseable lines,
                    // keep the session". Transport errors (EOF/IO) are still
                    // fatal — those come from `read_line()` above, not here.
                    let msg = match parse_pool_message(&line) {
                        Ok(msg) => msg,
                        Err(error) => {
                            // Mask: an attacker/pool could embed wallet/url-like
                            // tokens in junk; never echo the raw line. Log the
                            // serde error + line length only.
                            warn!(
                                %error,
                                line_len = line.len(),
                                "Skipping un-parseable (non-JSON) pool line — session continues"
                            );
                            continue;
                        }
                    };

                    match msg {
                        PoolMessage::Notify { .. } => {
                            self.handle_notify(msg).await?;
                            last_notify_at = Instant::now();
                        }
                        PoolMessage::SetDifficulty(diff) => {
                            let old_diff = self.current_difficulty;
                            self.current_difficulty = diff;
                            self.send_status(StratumStatus::DifficultyChanged(diff)).await;
                            {
                                let mut stats = self.stats.lock().await;
                                stats.current_difficulty = diff;
                            }
                            let direction = difficulty_change_direction(old_diff, diff);
                            info!(
                                difficulty = diff,
                                previous = old_diff,
                                direction,
                                "Difficulty {} from {} to {} — {}",
                                direction, old_diff, diff,
                                match direction {
                                    "UP" => "pool raised it based on our hashrate; higher difficulty = fewer but more valuable shares.",
                                    "DOWN" => "pool lowered it based on our hashrate; lower difficulty = more frequent but less valuable shares.",
                                    _ => "pool re-confirmed the current difficulty (no change).",
                                },
                            );
                            // POOL-2: difficulty-only change — recompute the share
                            // target but do NOT restart in-flight work.
                            self.refresh_current_job("set_difficulty", false).await;
                        }
                        PoolMessage::SetExtranonce { extranonce1, extranonce2_size } => {
                            // Mid-session extranonce rotation. Some pools do this periodically
                            // to prevent work overlap between miners. Our session ID changes
                            // but the connection stays alive.
                            if !is_valid_v1_extranonce2_size(extranonce2_size) {
                                warn!(
                                    extranonce2_size,
                                    max_extranonce2_size = MAX_V1_EXTRANONCE2_SIZE,
                                    "Ignoring invalid mining.set_extranonce size from pool"
                                );
                                continue;
                            }
                            let new_extranonce1 = match hex::decode(&extranonce1) {
                                Ok(bytes) => bytes,
                                Err(error) => {
                                    warn!(
                                        extranonce1 = %extranonce1,
                                        %error,
                                        "Ignoring mining.set_extranonce with invalid extranonce1 hex"
                                    );
                                    continue;
                                }
                            };
                            let old_en1 = hex::encode(&self.extranonce1);
                            self.extranonce1 = new_extranonce1;
                            self.extranonce2_size = extranonce2_size;
                            info!(
                                old_extranonce1 = %old_en1,
                                new_extranonce1 = %extranonce1,
                                extranonce2_size,
                                "Extranonce rotated mid-session — pool changed our session ID from 0x{} to 0x{}. This is normal, prevents work duplication between miners.",
                                old_en1, extranonce1,
                            );
                            self.refresh_current_job("set_extranonce", true).await;
                        }
                        PoolMessage::SetVersionMask(mask_str) => {
                            if !self.config.version_rolling {
                                warn!(
                                    mask = %mask_str,
                                    "Ignoring mining.set_version_mask because version rolling is disabled in config"
                                );
                                continue;
                            }
                            let old_mask = self.version_mask;
                            let new_mask = match parse_and_clamp_version_mask(
                                &mask_str,
                                self.config.version_rolling_mask,
                            ) {
                                Ok(mask) => mask,
                                Err(error) => {
                                    warn!(
                                        mask = %mask_str,
                                        %error,
                                        current_mask = %format_version_mask(self.version_mask),
                                        "Ignoring invalid mining.set_version_mask from pool"
                                    );
                                    continue;
                                }
                            };
                            self.version_mask = new_mask;
                            self.work_builder.set_version_mask(self.version_mask);
                            info!(
                                old_mask = %format_version_mask(old_mask),
                                new_mask = %format_version_mask(self.version_mask),
                                "Version rolling mask updated from 0x{:08X} to 0x{} — ASICBoost bit range changed mid-session",
                                old_mask, format_version_mask(self.version_mask),
                            );
                            self.refresh_current_job("set_version_mask", true).await;
                        }
                        PoolMessage::Ping(id) => {
                            // Pool keepalive — respond with pong to prove we're still alive
                            debug!(id, "Pool ping received, responding with pong (keepalive)");
                            conn.write_line(&pong_response(id))
                                .await
                                .map_err(SessionError::Connection)?;
                        }
                        PoolMessage::GetVersion(id) => {
                            debug!(id, user_agent = USER_AGENT, "Pool requested our version string, responding with '{}'", USER_AGENT);
                            conn.write_line(&version_response(id, USER_AGENT))
                                .await
                                .map_err(SessionError::Connection)?;
                        }
                        PoolMessage::ShowMessage(msg) => {
                            // Pool operators can send human-readable messages — could be
                            // maintenance notices, fee changes, or just greetings.
                            info!(message = %msg, "MESSAGE FROM POOL OPERATOR: {}", msg);
                            // W11.13 — append to the per-pool bounded ring buffer
                            // exposed via StratumStats. Capacity 16 (FIFO) and a
                            // 1024-char per-message cap bound memory under a
                            // chatty/buggy pool. Truncated copy is sent to the
                            // dashboard via the same buffer; the daemon-side
                            // StratumStatus::PoolMessage path also gets the raw
                            // string for logging.
                            let truncated = if msg.len() > POOL_MESSAGE_MAX_LEN {
                                // Slice on a UTF-8 char boundary so we never
                                // panic on multi-byte messages from pools that
                                // ship localized notices.
                                let mut end = POOL_MESSAGE_MAX_LEN;
                                while end > 0 && !msg.is_char_boundary(end) {
                                    end -= 1;
                                }
                                msg[..end].to_string()
                            } else {
                                msg.clone()
                            };
                            let entry = PoolMessageEntry {
                                timestamp_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                pool_url: pool.url.clone(),
                                message: truncated,
                            };
                            {
                                let mut stats = self.stats.lock().await;
                                stats.pool_message_log.push(entry);
                                // FIFO eviction once we exceed capacity. Using
                                // a Vec keeps the on-the-wire serde shape simple
                                // (JSON array) at the cost of a single
                                // O(capacity) drain on overflow — fine for a
                                // bound of 16.
                                while stats.pool_message_log.len() > POOL_MESSAGE_LOG_CAPACITY {
                                    stats.pool_message_log.remove(0);
                                }
                            }
                            self.send_status(StratumStatus::PoolMessage(msg)).await;
                        }
                        PoolMessage::Reconnect { host, port, wait_seconds } => {
                            info!(
                                host = %host, port, wait_seconds,
                                "Pool requests we reconnect to {}:{} in {} seconds — this usually means the pool is load-balancing or doing maintenance. Complying.",
                                host, port, wait_seconds,
                            );
                            self.send_status(StratumStatus::ReconnectRequested {
                                host: host.clone(), port, wait_seconds
                            }).await;
                            let uptime = session_start.elapsed();
                            info!(
                                session_secs = uptime.as_secs(),
                                "Session ending after {:.0}s — reconnecting to {}:{} as requested by pool",
                                uptime.as_secs_f32(), host, port,
                            );
                            // BUG FIX (2026-04-11): Was SessionEndReason::Clean, which
                            // ignored the pool's requested host:port. Now passes the
                            // target through so the outer loop honors it.
                            return Ok(SessionEndReason::Reconnect { host, port, wait_seconds });
                        }
                        PoolMessage::Response { id, result, error } => {
                            // W11.13 — `mining.extranonce.subscribe` response.
                            // Intercept before the share-correlation path so we
                            // don't log "Submit response had no correlated
                            // pending share metadata" for what is really a
                            // benign extension ack. Either result=true (pool
                            // honors the extension) or error=Method not found
                            // (pool only honors the BIP310 capability path).
                            // Both are safe to ignore; mining continues.
                            if id == ID_EXTRANONCE_SUBSCRIBE {
                                if let Some(err) = &error {
                                    if !err.is_null() {
                                        debug!(
                                            ?err,
                                            "Pool rejected mining.extranonce.subscribe — extension unsupported. mining.set_extranonce notifications will not arrive on this session; the BIP310 subscribe-extranonce capability advertised in mining.configure is the parallel path."
                                        );
                                    }
                                } else {
                                    debug!(
                                        ?result,
                                        "Pool acknowledged mining.extranonce.subscribe — we will receive mining.set_extranonce mid-session for extranonce1 rotation"
                                    );
                                }
                                // Fall through naturally — no `continue` because
                                // tokio::select! macro internals can confuse the
                                // continue target. The catch-all `_` arm below
                                // covers other unmatched IDs by sending them to
                                // handle_submit_response (which logs a warn but
                                // doesn't tear down the session).
                            } else {
                                let auth_fatal =
                                    self.handle_submit_response(id, result, error).await;
                                if auth_fatal {
                                    // Pool revoked this session's worker authorization
                                    // (e.g. account suspended, worker name changed at
                                    // the pool side). Drop the socket cleanly so the
                                    // outer loop reconnects + re-authorizes from
                                    // scratch instead of burning shares into a dead
                                    // session. "Clean" uses the normal backoff path.
                                    return Ok(SessionEndReason::Clean);
                                }
                            }
                        }
                        PoolMessage::Unknown(raw) => {
                            debug!(raw = %raw, "Unknown pool message — probably a pool extension we don't support yet. Ignoring safely.");
                        }
                    }
                }

                // Submit shares from the mining pipeline.
                // A share is a proof-of-work that meets the pool's difficulty target.
                // The ASIC found a nonce that makes SHA256d(block_header) <= share_target.
                share = self.share_rx.recv() => {
                    match share {
                        Some(share) => {
                            // The active Stratum session is authoritative for the
                            // submit worker. This keeps donation, failover, and
                            // weighted split routes out of the dispatcher.
                            self.pending_share = Some(PendingSubmit {
                                share,
                            });
                        }
                        None => {
                            // Share channel closed — daemon shutting down
                            let uptime = session_start.elapsed();
                            info!(
                                session_secs = uptime.as_secs(),
                                "Share channel closed (daemon shutting down). Pool session lasted {:.0}s.",
                                uptime.as_secs_f32(),
                            );
                            return Ok(SessionEndReason::Clean);
                        }
                    }
                }

                // Donation timer — switch pools when the phase expires.
                // The conditional guard ensures this branch is disabled when
                // donation is off (phase_remaining == ZERO), so the select!
                // operates exactly as before for users who disable donation.
                _ = &mut donation_timer, if !phase_remaining.is_zero() => {
                    let uptime = session_start.elapsed();
                    info!(
                        session_secs = uptime.as_secs(),
                        phase = if is_donation { "donation" } else { "user" },
                        "Donation timer fired after {:.0}s — switching pools. \
                         ASICs continue hashing without interruption.",
                        uptime.as_secs_f32(),
                    );
                    return Ok(SessionEndReason::DonationSwitch);
                }

                _ = &mut user_split_timer, if !user_split_remaining.is_zero() => {
                    let uptime = session_start.elapsed();
                    info!(
                        session_secs = uptime.as_secs(),
                        active_pool_index,
                        "User hashrate split timer fired after {:.0}s — switching user pool route. \
                         Dispatcher work will be flushed before the next route receives jobs.",
                        uptime.as_secs_f32(),
                    );
                    return Ok(SessionEndReason::UserSplitSwitch);
                }

                _ = &mut sv2_retry_timer, if sv2_retry_remaining.is_some() => {
                    let uptime = session_start.elapsed();
                    info!(
                        session_secs = uptime.as_secs(),
                        "Auto mode: V1 fallback dwell elapsed after {:.0}s — retrying SV2",
                        uptime.as_secs_f32(),
                    );
                    return Ok(SessionEndReason::AutoRetrySv2);
                }
                _ = no_notify_check.tick() => {
                    if no_notify_failover_due(
                        last_notify_at,
                        Duration::from_secs(no_notify_failover_secs),
                        Instant::now(),
                    ) {
                        warn!(
                            no_notify_failover_secs,
                            "No mining.notify within the configured window — failing the \
                             session into failover (pool appears stalled)"
                        );
                        // G36 observe-only shadow (default-OFF): log what the LuxOS
                        // FSM would decide for this trigger; the return below still
                        // drives the real failover.
                        self.shadow_observe_failover(LuxosFailoverTrigger::PoolInactivity);
                        return Err(SessionError::NoNotifyTimeout);
                    }
                    if reject_rate_failover_pct > 0 {
                        let (session_acc, session_rej) = {
                            let s = self.stats.lock().await;
                            (
                                s.shares_accepted.saturating_sub(reject_baseline_accepted),
                                s.shares_rejected.saturating_sub(reject_baseline_rejected),
                            )
                        };
                        if reject_rate_failover_due(
                            session_acc,
                            session_rej,
                            reject_rate_failover_min_samples,
                            reject_rate_failover_pct,
                        ) {
                            warn!(
                                reject_rate_failover_pct,
                                session_accepted = session_acc,
                                session_rejected = session_rej,
                                "Session reject-rate exceeded the configured failover \
                                 threshold — failing the session into failover"
                            );
                            // G36 observe-only shadow (default-OFF): log what the
                            // LuxOS FSM would decide; the return below still drives
                            // the real failover.
                            self.shadow_observe_failover(LuxosFailoverTrigger::TooManyRejections);
                            return Err(SessionError::HighRejectRate);
                        }
                    }
                }
            }
        }
    }

    /// Parse the mining.subscribe result to extract extranonce1 and extranonce2_size.
    fn parse_subscribe_result(&mut self, result: &Value) -> Result<(), SessionError> {
        let arr = result
            .as_array()
            .ok_or_else(|| SessionError::ParseError("subscribe result not array".into()))?;

        // result[1] = extranonce1 (hex string)
        let en1_hex = arr
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| SessionError::ParseError("missing extranonce1".into()))?;

        let extranonce1 = hex::decode(en1_hex)
            .map_err(|e| SessionError::ParseError(format!("bad extranonce1 hex: {}", e)))?;

        // result[2] = extranonce2_size (integer)
        let raw_extranonce2_size = arr
            .get(2)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| SessionError::ParseError("missing extranonce2_size".into()))?;
        let extranonce2_size = usize::try_from(raw_extranonce2_size)
            .ok()
            .filter(|size| is_valid_v1_extranonce2_size(*size))
            .ok_or_else(|| {
                SessionError::ParseError(format!(
                    "invalid extranonce2_size {} (expected 1..={})",
                    raw_extranonce2_size, MAX_V1_EXTRANONCE2_SIZE
                ))
            })?;
        self.extranonce1 = extranonce1;
        self.extranonce2_size = extranonce2_size;

        Ok(())
    }

    /// Handle a mining.notify message — convert to JobTemplate and send downstream.
    async fn handle_notify(&mut self, msg: PoolMessage) -> Result<(), SessionError> {
        if let PoolMessage::Notify {
            job_id,
            prev_hash,
            coinbase1,
            coinbase2,
            merkle_branches,
            version,
            nbits,
            ntime,
            clean_jobs,
        } = msg
        {
            // Parse the hex job fields. bug-hunt MED (2026-05-28): a single
            // malformed-hex mining.notify (e.g. version "200000000" = u32 overflow,
            // ntime "zz", odd-length coinbase) must NOT `?`-propagate to
            // SessionError -> session teardown -> reconnect+backoff. That would let
            // one bad job from a flaky pool drop a healthy session. Mirror the
            // deliberate fail-soft posture of parse_set_difficulty + the extranonce2
            // guard below: log a warn, SKIP this one job, and keep the session (and
            // any live hashing on the previous job) alive. A closure collects the
            // parse so a single early-return covers every field uniformly; the
            // happy path is byte-for-byte unchanged when the job is valid.
            let parsed: Result<_, String> = (|| {
                let prev_block_hash =
                    parse_32_bytes(&prev_hash).map_err(|e| format!("prev_hash: {e}"))?;
                let coinbase1_bytes =
                    hex::decode(&coinbase1).map_err(|e| format!("coinbase1: {e}"))?;
                let coinbase2_bytes =
                    hex::decode(&coinbase2).map_err(|e| format!("coinbase2: {e}"))?;
                let mut merkle_branch_bytes = Vec::with_capacity(merkle_branches.len());
                for branch in &merkle_branches {
                    merkle_branch_bytes
                        .push(parse_32_bytes(branch).map_err(|e| format!("merkle_branch: {e}"))?);
                }
                let version_u32 =
                    u32::from_str_radix(&version, 16).map_err(|e| format!("version: {e}"))?;
                let nbits_u32 =
                    u32::from_str_radix(&nbits, 16).map_err(|e| format!("nbits: {e}"))?;
                let ntime_u32 =
                    u32::from_str_radix(&ntime, 16).map_err(|e| format!("ntime: {e}"))?;
                Ok((
                    prev_block_hash,
                    coinbase1_bytes,
                    coinbase2_bytes,
                    merkle_branch_bytes,
                    version_u32,
                    nbits_u32,
                    ntime_u32,
                ))
            })();
            let (
                prev_block_hash,
                coinbase1_bytes,
                coinbase2_bytes,
                merkle_branch_bytes,
                version_u32,
                nbits_u32,
                ntime_u32,
            ) = match parsed {
                Ok(t) => t,
                Err(reason) => {
                    warn!(
                        target: "stratum_v1",
                        job_id = %job_id,
                        reason = %reason,
                        "skipping malformed mining.notify job (keeping session + previous job \
                         alive) — a single bad job must not trigger reconnect+backoff"
                    );
                    return Ok(());
                }
            };

            let share_target = difficulty_to_target(self.current_difficulty);

            // Log first 2 jobs with FULL raw Stratum data for offline share verification
            static NOTIFY_LOG_COUNT: std::sync::atomic::AtomicU32 =
                std::sync::atomic::AtomicU32::new(0);
            let notify_count = NOTIFY_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if notify_count < 2 {
                let branches_hex: Vec<&str> = merkle_branches.iter().map(|s| s.as_str()).collect();
                info!(
                    job_id = %job_id,
                    version = %version,
                    prev_hash = %prev_hash,
                    ntime = %ntime,
                    nbits = %nbits,
                    coinbase1 = %coinbase1,
                    coinbase2 = %coinbase2,
                    extranonce1 = %hex::encode(&self.extranonce1),
                    extranonce2_size = self.extranonce2_size,
                    merkle_branches = ?branches_hex,
                    "RAW_NOTIFY[{}]: Full Stratum job data for offline verification",
                    notify_count,
                );
            }

            if clean_jobs {
                self.work_builder.reset_extranonce2();
            }

            // W5.4: dispatching work before mining.subscribe parsed
            // extranonce2_size is a protocol bug — every share submission
            // would land at offset 0 with an unsized counter. The constructor
            // seeds 0 as a sentinel, parse_subscribe_result writes the real
            // pool-provided 1..=8 value, and is_valid_v1_extranonce2_size
            // gates the parser so this check is the last line of defense
            // against a mining.notify arriving before mining.subscribe ack.
            //
            // W24-CRASH-1 (w24-protocol Finding 1): fail-soft instead of
            // `assert!`. The invariant currently holds — `parse_subscribe_result`
            // returns Err (never sets `subscribed`) outside 1..=MAX, and the
            // session loop only runs handle_notify post-handshake — so this is
            // defense-in-depth. But under `panic = "abort"` a fired assert would
            // abort the WHOLE daemon (not just this task) if a future refactor
            // ever broke the invariant. Dropping the malformed job is the safe
            // degradation: skip this one notify, keep the daemon (and any live
            // hashing on the previous job) alive. Happy-path behavior is
            // unchanged — when valid, control falls through exactly as before.
            if !(self.extranonce2_size > 0 && is_valid_v1_extranonce2_size(self.extranonce2_size)) {
                error!(
                    extranonce2_size = self.extranonce2_size,
                    max = MAX_V1_EXTRANONCE2_SIZE,
                    "mining.notify arrived with extranonce2_size not yet parsed from \
                     mining.subscribe response (sentinel 0 = uninitialized; valid range \
                     is 1..=max) — dropping this malformed job instead of dispatching \
                     work at offset 0. This should never happen post-handshake; \
                     investigate the subscribe/notify ordering if it recurs."
                );
                return Ok(());
            }

            let job = JobTemplate {
                job_id: job_id.clone(),
                prev_block_hash,
                coinbase1: coinbase1_bytes,
                coinbase2: coinbase2_bytes,
                merkle_branches: merkle_branch_bytes,
                version: version_u32,
                nbits: nbits_u32,
                ntime: ntime_u32,
                clean_jobs,
                share_target,
                extranonce1: self.extranonce1.clone(),
                extranonce2_size: self.extranonce2_size,
                version_mask: self.version_mask,
                merkle_root: [0u8; 32], // V1: computed by WorkBuilder from coinbase + branches
                pool_difficulty: self.current_difficulty,
            };

            // Store for hash-on-disconnect — if the pool disconnects, ASICs keep
            // hashing this last job to prevent thermal shock from sudden power changes
            self.last_job = Some(job.clone());

            // Send to job dispatcher (which builds midstates and sends to ASICs)
            let job_dispatched = self.job_tx.send(job).await.is_ok();
            if !job_dispatched {
                warn!("Job channel full or closed — work dispatcher may have crashed or is backed up. New jobs can't reach the ASICs!");
            } else if !self.mining_state_announced {
                self.mining_state_announced = true;
                self.send_status(StratumStatus::StateChanged(StratumState::Mining))
                    .await;
            }

            let mut stats = self.stats.lock().await;
            stats.jobs_received += 1;
            let total_jobs = stats.jobs_received;

            if clean_jobs {
                // clean_jobs=true means a new Bitcoin block was found on the network.
                // All pending work is now stale — we must switch to this new job immediately.
                info!(
                    job_id = %job_id,
                    prev_hash = %prev_hash[..16],
                    merkle_branches = merkle_branches.len(),
                    pool_target_difficulty = self.current_difficulty,
                    total_jobs,
                    "NEW BLOCK DETECTED — pool sent clean job '{}' (new Bitcoin block found on the network!). All previous work is now stale. Switching ASICs to mine on the new block template.",
                    job_id,
                );
            } else {
                // Non-clean job — the pool is just providing an updated template.
                // We can keep working on the previous job until ASICs pick up this one.
                debug!(
                    job_id = %job_id,
                    difficulty = self.current_difficulty,
                    total_jobs,
                    "New job '{}' received (same block, updated template). Jobs received this session: {}",
                    job_id, total_jobs,
                );
            }
        }

        Ok(())
    }

    /// Re-dispatch the current job after a mid-session parameter change.
    ///
    /// `restart_work` selects whether the refresh invalidates queued ASIC work:
    /// - `true` for `mining.set_extranonce` / version-mask changes, which change
    ///   the coinbase / valid version-roll space, so in-flight work is stale and
    ///   must be flushed (clean restart).
    /// - `false` for a difficulty-only `mining.set_difficulty`, which changes ONLY
    ///   the share acceptance threshold, not the block work — forcing a restart
    ///   there would needlessly throw away good in-flight work (POOL-2).
    async fn refresh_current_job(&mut self, reason: &str, restart_work: bool) {
        let Some(mut job) = self.last_job.clone() else {
            return;
        };

        // W5.4: refresh path runs after set_extranonce / set_version_mask /
        // similar mid-session updates. last_job only exists if at least one
        // prior notify was dispatched, which already required extranonce2_size
        // to be valid. Re-check here to keep the contract local to the call
        // site so a future refactor that reorders init can't silently
        // produce a sentinel-sized refresh.
        //
        // W24-CRASH-1 (w24-protocol Finding 1): fail-soft instead of `assert!`.
        // Same rationale as handle_notify — the invariant holds today, but under
        // `panic = "abort"` a fired assert would abort the whole daemon. Skip the
        // refresh (the previous `last_job` stays active for hash-on-disconnect)
        // rather than crash. Happy-path behavior is unchanged.
        if !(self.extranonce2_size > 0 && is_valid_v1_extranonce2_size(self.extranonce2_size)) {
            error!(
                extranonce2_size = self.extranonce2_size,
                reason,
                max = MAX_V1_EXTRANONCE2_SIZE,
                "refresh_current_job invoked with invalid extranonce2_size \
                 (expected 1..=max) — skipping the refresh instead of rebuilding \
                 a sentinel-sized job. The previous job remains active."
            );
            return;
        }

        job.extranonce1 = self.extranonce1.clone();
        job.extranonce2_size = self.extranonce2_size;
        job.version_mask = self.version_mask;
        // POOL-2: the job's share_target / pool_difficulty were computed at
        // notify time from the THEN-current difficulty. A mid-session
        // mining.set_difficulty updates self.current_difficulty but, without this
        // recompute, the re-dispatched job would carry the STALE target — after a
        // difficulty INCREASE that target is too easy, so the pool low-diff-rejects
        // (code 23) every resulting share. Recompute from the live difficulty so
        // the dispatcher validates against the value the pool now expects.
        job.share_target = difficulty_to_target(self.current_difficulty);
        job.pool_difficulty = self.current_difficulty;
        // Extranonce / version-mask changes invalidate queued ASIC work (coinbase
        // / valid version-roll space changed) → restart like a clean job. A
        // difficulty-only refresh must NOT force a restart: the in-flight work is
        // still valid, only the accept threshold moved, and forcing clean_jobs
        // here would cause a wasteful double work-restart.
        job.clean_jobs = restart_work;
        if restart_work {
            self.work_builder.reset_extranonce2();
        }
        self.last_job = Some(job.clone());

        if self.job_tx.send(job).await.is_err() {
            warn!(
                reason,
                "Failed to refresh current job after session parameter change"
            );
        } else {
            info!(
                reason,
                "Rebuilt current job with updated session parameters"
            );
        }
    }

    async fn flush_dispatcher_for_pool_switch(&mut self, is_donation: bool) {
        let old_phase = if is_donation { "donation" } else { "user" };
        self.pending_share = None;
        self.last_job = None;
        self.last_stale_jobs_flushed_on_switch = false;
        self.work_builder.reset_extranonce2();
        // D-01: drop per-job nonce-dedup history at the pool boundary. Job IDs
        // are pool-scoped and short job IDs are recycled across pools, so a
        // recycled `job_id` from the old pool could otherwise wrongly mark a
        // valid nonce mined for the new pool's same-named job as a duplicate and
        // drop it pre-submit. The dedup window only needs to span a single live
        // pool session.
        self.nonce_dedup.reset();

        if self
            .job_tx
            .send(JobTemplate::flush_only(self.current_difficulty))
            .await
            .is_err()
        {
            warn!(old_phase, "Failed to flush dispatcher before pool switch");
        } else {
            self.last_stale_jobs_flushed_on_switch = true;
            info!(
                old_phase,
                "Flushed stale clean jobs and work before pool switch"
            );
        }
    }

    /// Handle a response to a mining.submit request.
    ///
    /// The pool responds to each share submission with either:
    /// - `result: true` — share accepted, we earn credit
    /// - `error: [code, message]` — share rejected, no credit
    ///
    /// Common rejection reasons:
    /// - "Stale" (21): We submitted work for an old block (new block was found first)
    /// - "Duplicate" (22): We already submitted this exact nonce
    /// - "Low difficulty" (23): The share doesn't meet the pool's target
    /// - "Job not found" (20): The job_id is unknown (pool may have pruned it)
    ///   Handle a pool `Response` message to a prior `mining.submit`.
    ///
    /// Returns `true` when the pool's reject signals that the session itself is
    /// dead (worker no longer authorized / not subscribed) — not just a
    /// share-level reject. The caller MUST terminate the session cleanly so the
    /// outer loop reconnects or fails over, rather than burning further shares
    /// into a pool that will never credit them. Returns `false` on share-level
    /// rejects (low-diff, duplicate, stale, unknown job) which are non-fatal.
    async fn handle_submit_response(
        &mut self,
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    ) -> bool {
        let matched_submit = if let Some(pos) = self
            .pending_submits
            .iter()
            .position(|pending| pending.request_id == id)
        {
            Some(self.pending_submits.remove(pos))
        } else {
            warn!(
                id,
                pending_len = self.pending_submits.len(),
                phase = ?self.donation_phase,
                result = ?result,
                error = ?error,
                "Submit response had no correlated pending share metadata"
            );
            None
        };

        // Measure pool latency from submit to response
        let latency_ms = if let Some(pending) = matched_submit.as_ref() {
            let latency = pending.submitted_at.elapsed().as_millis() as u64;

            // Update stats with latency
            //
            // LANE S: the submit→response RTT was previously stored only in the
            // ambiguous `latency_ms` scalar (0 == "never measured" == "0 ms").
            // Surface the same already-measured sample as the honest
            // None-before-sample `last_latency_ms` and the per-pool
            // `per_pool_latency_ms` vector so the dashboard can show latency for
            // the primary and every backup independently. No new measurement is
            // taken here — only the existing `latency` value is propagated.
            {
                let active = self.current_pool_index;
                let pool_count = self.pool_count();
                let mut stats = self.stats.lock().await;
                stats.latency_ms = latency;
                stats.shares_unresolved = self.pending_submits.len() as u64;
                let sample = latency.min(u32::MAX as u64) as u32;
                stats.last_latency_ms = Some(sample);
                if stats.per_pool_latency_ms.len() < pool_count {
                    stats.per_pool_latency_ms.resize(pool_count, None);
                }
                if let Some(slot) = stats.per_pool_latency_ms.get_mut(active) {
                    *slot = Some(sample);
                }
            }

            // Report latency to the daemon
            self.send_status(StratumStatus::Latency(latency)).await;

            Some(latency)
        } else {
            None
        };

        let correlated_job_id = matched_submit
            .as_ref()
            .map(|pending| pending.share.job_id.clone())
            .unwrap_or_else(|| format!("unmatched-submit-{}", id));
        let correlated_meta = matched_submit.as_ref().map(|pending| ShareEventMeta {
            share: pending.share.clone(),
        });

        // SEC + telemetry integrity: a submit response we cannot correlate to a
        // pending share (its `id` is not in `pending_submits` — a duplicate/late
        // ack, a response for an already-`trim_pending_submits`'d submit, or a
        // hostile/MITM pool spamming `{"id":N,"result":true}`) must NOT be counted
        // as an accepted/rejected share. Counting phantom shares (a) inflates
        // proof-of-mining telemetry, (b) lets a bad pool hold its computed
        // reject-rate below `reject_rate_failover_pct` and SUPPRESS reject-rate
        // failover (stranding the miner), and (c) feeds fake accepts into the
        // rolling-acceptance autotuner step-up gate. We already `warn!`'d above.
        // A genuine session-fatal auth revoke is still honored for reconnect, but
        // no share counter / acceptance tracker / status is touched.
        if matched_submit.is_none() {
            if let Some(err) = &error {
                if !err.is_null() {
                    let (code, msg) = parse_error(err);
                    let msg_lower = msg.to_lowercase();
                    let auth_fatal = code == 24
                        || msg_lower.contains("unauthoriz")
                        || msg_lower.contains("not authenticated")
                        || msg_lower.contains("not subscribed");
                    if auth_fatal {
                        error!(
                            id,
                            code,
                            msg = %msg,
                            "Pool sent a session-fatal auth error on an UNCORRELATED submit response — reconnecting"
                        );
                        return true;
                    }
                }
            }
            return false;
        }

        if let Some(err) = &error {
            if !err.is_null() {
                let (code, msg) = parse_error(err);

                // Detect session-fatal "auth revoked" style rejects. Conservative
                // match: Stratum V1 code 24 is the conventional "Unauthorized
                // worker" from bmminer-era pools, and message-pattern matches
                // catch other pools that use different codes. We deliberately
                // do NOT include code 25 here — legacy pools use 25 for
                // low-difficulty, treating it as auth-fatal would disconnect
                // every time difficulty tuning drifts.
                let msg_lower = msg.to_lowercase();
                let auth_fatal = code == 24
                    || msg_lower.contains("unauthoriz")
                    || msg_lower.contains("not authenticated")
                    || msg_lower.contains("not subscribed");

                // Provide educational context based on the error code (gap-swarm F-011:
                // canonical Stratum V1 + BIP310 reject-code table 20-27; pure operator
                // text — does NOT change the auth_fatal reconnect gate above).
                let advice = reject_code_advice(code);

                if auth_fatal {
                    error!(
                        id,
                        job_id = %correlated_job_id,
                        code,
                        msg = %msg,
                        "POOL REVOKED SESSION mid-mining (error {}: '{}'){}",
                        code, msg, advice,
                    );
                } else {
                    warn!(
                        id,
                        job_id = %correlated_job_id,
                        nonce = correlated_meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                        ntime = correlated_meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                        extranonce2 = correlated_meta.as_ref().map(|meta| meta.share.extranonce2.as_str()),
                        version_bits = correlated_meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                        version = correlated_meta.as_ref().map(|meta| meta.share.version),
                        code,
                        msg = %msg,
                        "SHARE REJECTED by pool (error {}: '{}'){} Rejected shares don't earn mining credit.",
                        code, msg, advice,
                    );
                }

                self.send_status(StratumStatus::ShareRejected {
                    job_id: correlated_job_id,
                    error_code: code,
                    error_msg: msg,
                    meta: correlated_meta,
                })
                .await;

                // W6.3: record the rejection in the rolling 30-min
                // acceptance tracker BEFORE locking stats so the
                // dashboard surface (rolling_acceptance_pct on
                // StratumStats) reflects the new sample atomically.
                self.acceptance_tracker.record_share(false);
                let rolling_pct = self.acceptance_tracker.rolling_acceptance_pct();
                let rolling_count = self.acceptance_tracker.rolling_count();

                let mut stats = self.stats.lock().await;
                stats.shares_rejected += 1;
                stats.shares_unresolved = self.pending_submits.len() as u64;
                stats.rolling_acceptance_pct = rolling_pct;
                stats.rolling_acceptance_count = rolling_count;
                let total_accepted = stats.shares_accepted;
                let total_rejected = stats.shares_rejected;
                drop(stats);
                self.send_status(StratumStatus::RollingAcceptanceUpdated {
                    pct: rolling_pct,
                    accepted: rolling_count.0,
                    total: rolling_count.1,
                })
                .await;
                let reject_pct = if total_accepted + total_rejected > 0 {
                    total_rejected as f64 / (total_accepted + total_rejected) as f64 * 100.0
                } else {
                    0.0
                };
                if reject_pct > 5.0 && (total_accepted + total_rejected) > 10 {
                    warn!(
                        reject_pct = format_args!("{:.1}%", reject_pct),
                        accepted = total_accepted,
                        rejected = total_rejected,
                        "Share rejection rate is {:.1}% — above 5% is concerning. Check: network latency to pool, difficulty settings, clock sync, and ASIC health.",
                        reject_pct,
                    );
                }
                return auth_fatal;
            }
        }

        if result == Some(Value::Bool(true)) {
            // W6.3: rolling acceptance is updated on the accept branch
            // too. Without this the percentage would drift toward 0% as
            // rejections roll off the window with no positives to
            // counter them.
            self.acceptance_tracker.record_share(true);
            let rolling_pct = self.acceptance_tracker.rolling_acceptance_pct();
            let rolling_count = self.acceptance_tracker.rolling_count();

            let mut stats = self.stats.lock().await;
            stats.shares_accepted += 1;
            stats.shares_unresolved = self.pending_submits.len() as u64;
            stats.rolling_acceptance_pct = rolling_pct;
            stats.rolling_acceptance_count = rolling_count;
            let total = stats.shares_accepted;
            drop(stats);

            // Log accepted shares at info level every 10th share, debug otherwise
            // This keeps logs readable during steady-state mining while still
            // showing progress for users watching the console
            let latency_str = latency_ms
                .map(|l| format!(" [{}ms]", l))
                .unwrap_or_default();

            if total % 10 == 0 || total <= 3 {
                info!(
                    id,
                    job_id = %correlated_job_id,
                    nonce = correlated_meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                    ntime = correlated_meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                    extranonce2 = correlated_meta.as_ref().map(|meta| meta.share.extranonce2.as_str()),
                    version_bits = correlated_meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                    version = correlated_meta.as_ref().map(|meta| meta.share.version),
                    total_accepted = total,
                    difficulty = self.current_difficulty,
                    latency_ms = latency_ms.unwrap_or(0),
                    "Share ACCEPTED by pool (#{} at pool target difficulty {}{}). Each accepted share earns proportional credit toward block rewards.",
                    total, self.current_difficulty, latency_str,
                );
            } else {
                debug!(
                    id,
                    job_id = %correlated_job_id,
                    nonce = correlated_meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                    ntime = correlated_meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                    version_bits = correlated_meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                    version = correlated_meta.as_ref().map(|meta| meta.share.version),
                    total_accepted = total,
                    latency_ms = latency_ms.unwrap_or(0),
                    "Share accepted (#{} at pool target diff {}{})",
                    total,
                    self.current_difficulty,
                    latency_str,
                );
            }

            let achieved_difficulty = correlated_meta
                .as_ref()
                .and_then(|meta| meta.share.achieved_difficulty);
            self.send_status(StratumStatus::ShareAccepted {
                job_id: correlated_job_id,
                pool_target_difficulty: self.current_difficulty,
                achieved_difficulty,
                meta: correlated_meta,
            })
            .await;
            self.send_status(StratumStatus::RollingAcceptanceUpdated {
                pct: rolling_pct,
                accepted: rolling_count.0,
                total: rolling_count.1,
            })
            .await;
        } else if result == Some(Value::Bool(false)) {
            warn!(
                id,
                job_id = %correlated_job_id,
                nonce = correlated_meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                ntime = correlated_meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                version_bits = correlated_meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                version = correlated_meta.as_ref().map(|meta| meta.share.version),
                "Pool returned result=false for mining.submit without an explicit error payload"
            );

            self.send_status(StratumStatus::ShareRejected {
                job_id: correlated_job_id,
                error_code: -1,
                error_msg: "submit returned false".to_string(),
                meta: correlated_meta,
            })
            .await;

            // W6.3: bare result=false (no error payload) is still a
            // rejection — record it on the rolling tracker so the
            // step-up gate sees it.
            self.acceptance_tracker.record_share(false);
            let rolling_pct = self.acceptance_tracker.rolling_acceptance_pct();
            let rolling_count = self.acceptance_tracker.rolling_count();

            let mut stats = self.stats.lock().await;
            stats.shares_rejected += 1;
            stats.shares_unresolved = self.pending_submits.len() as u64;
            stats.rolling_acceptance_pct = rolling_pct;
            stats.rolling_acceptance_count = rolling_count;
            drop(stats);
            self.send_status(StratumStatus::RollingAcceptanceUpdated {
                pct: rolling_pct,
                accepted: rolling_count.0,
                total: rolling_count.1,
            })
            .await;
        } else {
            // Ambiguous submit response: no error payload AND result is neither
            // `true` nor `false` (e.g. a non-conformant pool that returns
            // `{"id":N,"result":null,"error":null}`, or an unexpected JSON
            // shape). Before this branch the pending share was silently removed
            // from `pending_submits` and counted as NEITHER accepted nor
            // rejected, with NO log line — so an operator watching share
            // accounting during bring-up would see the submitted share simply
            // vanish. Log it (observability-only; we deliberately do NOT count
            // it as accepted/rejected since the pool gave no verdict, and we do
            // NOT tear down the session — mining continues).
            warn!(
                id,
                job_id = %correlated_job_id,
                nonce = correlated_meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                ntime = correlated_meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                result = ?result,
                "Pool returned an ambiguous mining.submit response (no error, result is neither true nor false) — share is unresolved (NOT counted as accepted or rejected). If this repeats, the pool may be non-conformant; check the pool's Stratum V1 implementation."
            );
        }

        // No error branch taken (normal accepted or unknown result) — not auth-fatal.
        false
    }

    fn trim_pending_submits(&mut self) -> u64 {
        if self.pending_submits.len() <= MAX_PENDING_SUBMITS {
            return 0;
        }

        let overflow = self.pending_submits.len() - MAX_PENDING_SUBMITS;
        let dropped: Vec<_> = self.pending_submits.drain(..overflow).collect();
        self.pending_submit_dropped = self.pending_submit_dropped.saturating_add(overflow as u64);
        if let (Some(first), Some(last)) = (dropped.first(), dropped.last()) {
            warn!(
                dropped = overflow,
                dropped_total = self.pending_submit_dropped,
                oldest_request_id = first.request_id,
                newest_request_id = last.request_id,
                oldest_age_ms = first.submitted_at.elapsed().as_millis() as u64,
                newest_age_ms = last.submitted_at.elapsed().as_millis() as u64,
                pending_remaining = self.pending_submits.len(),
                "Trimming oldest pending submit correlation records to cap queue growth"
            );
        }
        overflow as u64
    }

    fn clear_orphaned_pending_submits(&mut self, context: &str) -> u64 {
        if self.pending_submits.is_empty() {
            return 0;
        }

        let orphaned_count = self.pending_submits.len() as u64;
        let oldest_age_ms = self
            .pending_submits
            .first()
            .map(|pending| pending.submitted_at.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let newest_age_ms = self
            .pending_submits
            .last()
            .map(|pending| pending.submitted_at.elapsed().as_millis() as u64)
            .unwrap_or(0);
        warn!(
            context,
            orphaned = self.pending_submits.len(),
            oldest_age_ms,
            newest_age_ms,
            "Clearing orphaned pending submit correlations from an ended session"
        );
        self.pending_submits.clear();
        orphaned_count
    }

    /// Get the pool configuration for the given index.
    fn get_pool_config(&self, index: usize) -> PoolConfig {
        match index {
            0 => self.config.pool1.clone(),
            1 => self
                .config
                .pool2
                .clone()
                .unwrap_or_else(|| self.config.pool1.clone()),
            2 => self
                .config
                .pool3
                .clone()
                .unwrap_or_else(|| self.config.pool1.clone()),
            _ => self.config.pool1.clone(),
        }
    }

    /// Total number of configured pools.
    fn pool_count(&self) -> usize {
        let mut count = 1;
        if self.config.pool2.is_some() {
            count += 1;
        }
        if self.config.pool3.is_some() {
            count += 1;
        }
        count
    }

    /// Get the next unique request ID.
    fn next_request_id(&mut self) -> u64 {
        self.request_id_counter += 1;
        self.request_id_counter
    }

    /// Send a status update to the main daemon.
    async fn send_status(&self, status: StratumStatus) {
        if self.status_tx.send(status).await.is_err() {
            warn!("Status channel closed");
        }
    }
}

/// Parse a 64-char hex string into a 32-byte array.
fn parse_32_bytes(hex_str: &str) -> Result<[u8; 32], SessionError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| SessionError::ParseError(format!("bad hex: {}", e)))?;
    if bytes.len() != 32 {
        return Err(SessionError::ParseError(format!(
            "expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Parse a Stratum error response: [code, message, data].
fn parse_error(err: &Value) -> (i64, String) {
    if let Some(arr) = err.as_array() {
        let code = arr.first().and_then(|v| v.as_i64()).unwrap_or(-1);
        let msg = arr
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error")
            .to_string();
        (code, msg)
    } else {
        (-1, format!("{}", err))
    }
}

/// Operator-facing advice for a Stratum V1 `mining.submit` reject code.
///
/// Canonical reject-code table (Stratum V1 + the BIP310 version-rolling
/// extension), per `00-stratum-v1-spec.md` / `03-share-submission-flow.md`:
/// 20 other/unknown · 21 job-not-found (stale) · 22 duplicate · 23 low-difficulty ·
/// 24 unauthorized · 25 not-subscribed · 26 reserved · 27 invalid-version-mask.
///
/// gap-swarm F-011: codes 26 + 27 previously fell to the generic arm, and 25 was
/// mislabelled "low difficulty" (a duplicate of 23). This is **pure operator
/// text** — it does NOT change the `auth_fatal` reconnect decision, which stays
/// deliberately conservative for code 25 (legacy pools overload 25 for
/// low-difficulty, so treating it as auth-fatal would cause disconnect loops).
fn reject_code_advice(code: i64) -> &'static str {
    match code {
        20 => " — other/unknown pool reject. Check the pool's message text for specifics.",
        21 => " — job not found (stale): the pool no longer recognizes this job ID (it expired, usually during a block transition). Harmless if occasional; reduce stales by improving network latency.",
        22 => " — duplicate share: we already submitted this exact nonce. Shouldn't happen with proper extranonce2 incrementing; may indicate a work-dispatch bug.",
        23 => " — low difficulty: the share hash doesn't meet the pool's target. Could mean our difficulty target is miscalculated or the ASIC reported a false positive.",
        24 => " — unauthorized worker: the pool no longer recognizes this worker. Session is dead; will reconnect and re-authorize.",
        25 => " — canonical 'not subscribed' (auth-class). NOTE: many legacy pools overload code 25 for low-difficulty shares, so we treat it as non-fatal to avoid disconnect loops. If shares are persistently rejected with 25 on a modern pool, the session may need to re-subscribe.",
        26 => " — reserved reject code with no canonical meaning. Check the pool's documentation for its specific use.",
        27 => " — invalid version mask: the pool rejected our BIP310 rolled version bits (ASICBoost mask drift). If persistent, reduce or disable version-rolling for this pool.",
        _ => " — check pool documentation or support for this error code.",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("handshake timeout (30s)")]
    HandshakeTimeout,

    #[error("disconnected from pool")]
    Disconnected,

    #[error("authorization failed: {0}")]
    AuthorizationFailed(String),

    #[error("parse error: {0}")]
    ParseError(String),

    #[error("no mining.notify within the configured no-notify-failover window")]
    NoNotifyTimeout,

    #[error("session share reject-rate exceeded the configured failover threshold")]
    HighRejectRate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    struct MockPool {
        url: String,
        requests_rx: mpsc::Receiver<String>,
        task: JoinHandle<()>,
    }

    fn test_config() -> StratumConfig {
        StratumConfig {
            pool1: PoolConfig {
                url: "stratum+tcp://pool.example.com:3333".to_string(),
                worker: "user.worker".to_string(),
                password: "x".to_string(),
                sv2_url: None,
                protocol: None,
                split_bps: None,
            },
            pool2: None,
            pool3: None,
            routing_mode: "failover".to_string(),
            split_cycle_duration_s: 1800,
            primary_return_stability_secs: 900,
            no_notify_failover_secs: 300,
            reject_rate_failover_pct: 0,
            reject_rate_failover_min_samples: 100,
            smart_failover_enabled: false,
            smart_failover_drive: false,
            sv2_max_inbound_frame_bytes: 1_048_576,
            v1_max_inbound_line_bytes: 65_536,
            donation: DonationConfig::default(),
            version_rolling: true,
            version_rolling_mask: crate::types::default_version_rolling_mask(),
            suggest_difficulty: None,
            hash_on_disconnect: true,
            nominal_hashrate_ghs: 13_500.0,
            sv2_extended_channel: false,
            protocol: None,
        }
    }

    fn test_share(job_id: &str, nonce: &str, version_bits: Option<&str>) -> ValidShare {
        ValidShare {
            worker_name: "worker.original".to_string(),
            job_id: job_id.to_string(),
            extranonce2: "abcd1234".to_string(),
            ntime: "66112233".to_string(),
            nonce: nonce.to_string(),
            version_bits: version_bits.map(str::to_string),
            version: 0x2000_0000,
            achieved_difficulty: Some(65_536.0),
        }
    }

    fn backup_pool(url: &str, worker: &str) -> PoolConfig {
        PoolConfig {
            url: url.to_string(),
            worker: worker.to_string(),
            password: "secret".to_string(),
            sv2_url: None,
            protocol: None,
            split_bps: None,
        }
    }

    fn failover_config() -> StratumConfig {
        let mut config = test_config();
        config.pool2 = Some(backup_pool(
            "stratum+tcp://backup.example.com:4444",
            "user.backup",
        ));
        config.pool3 = Some(backup_pool(
            "stratum+tcp://tertiary.example.com:5555",
            "user.tertiary",
        ));
        config
    }

    fn weighted_split_config(primary_bps: u16, secondary_bps: u16) -> StratumConfig {
        let mut config = failover_config();
        config.pool3 = None;
        config.routing_mode = "weighted_split".to_string();
        config.split_cycle_duration_s = 600;
        config.pool1.split_bps = Some(primary_bps);
        if let Some(pool2) = config.pool2.as_mut() {
            pool2.split_bps = Some(secondary_bps);
        }
        config.donation.enabled = false;
        config
    }

    /// TEL-1 negative regression: the V1 client must NEVER emit a raw pool URL
    /// (which can carry `user:pass@` credentials) into a tracing log field. The
    /// pool URL on a donation fallback is the most credential-prone surface, so
    /// this drives the real `try_switch_to_donation_fallback` warn! site,
    /// captures the formatted log bytes, and asserts the embedded `user:pass@`
    /// is stripped while the host survives. If a future edit drops the
    /// `sanitize_pool_url` wrap on any of the ~15 client.rs pool-URL log fields,
    /// the credential strings reappear here and this test fails closed.
    #[test]
    fn donation_fallback_log_masks_pool_url_credentials() {
        use std::sync::Mutex;
        use tracing_subscriber::fmt::MakeWriter;

        // Minimal in-memory writer so we can inspect the exact formatted log line.
        #[derive(Clone)]
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for BufWriter {
            type Writer = BufWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(BufWriter(buf.clone()))
            .with_ansi(false)
            .finish();

        let captured = tracing::subscriber::with_default(subscriber, || {
            let mut config = test_config();
            config.donation.enabled = true;
            config.donation.fallback_enabled = true;
            config.donation.pool_url =
                "stratum+tcp://primaryuser:primarysecret@donation.example.com:3333".to_string();
            config.donation.fallback_pool_url =
                "stratum+tcp://falluser:fallsecret@fallback.example.com:3333".to_string();
            config.donation.fallback_worker = "DungeonMaster".to_string();

            let (job_tx, _job_rx) = mpsc::channel(4);
            let (_share_tx, share_rx) = mpsc::channel(4);
            let (status_tx, _status_rx) = mpsc::channel(4);
            let mut client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
            client.donation_pool_index = 0;

            // Drives the real `fallback_pool = %sanitize_pool_url(...)` warn! site.
            assert!(client.try_switch_to_donation_fallback("test_reason"));

            String::from_utf8(buf.lock().unwrap().clone()).unwrap()
        });

        // Sanity: the warn! line was actually captured and the host is preserved
        // (a sanitized URL is still useful to the operator).
        assert!(
            captured.contains("fallback.example.com"),
            "expected sanitized fallback host in log output, got: {captured}"
        );
        // The load-bearing assertion: NO embedded credential survives.
        assert!(
            !captured.contains("fallsecret"),
            "raw password leaked into log: {captured}"
        );
        assert!(
            !captured.contains("falluser"),
            "raw username leaked into log: {captured}"
        );
        assert!(
            !captured.contains(":pass@") && !captured.contains(":fallsecret@"),
            "credential separator leaked into log: {captured}"
        );
        assert!(
            !captured.contains("@fallback.example.com"),
            "user:pass@host credential form leaked into log: {captured}"
        );
    }

    /// FOV-6 (): with the config-form drive arm ON
    /// (`smart_failover_drive=true` + `smart_failover_enabled=true`), repeated
    /// connect failures on the active pool must advance the V1 client's
    /// `current_pool_index` to the FSM-selected next pool — the FSM marks the
    /// dead primary and advances `active_pool()`, and the drive path applies it.
    ///
    /// NOTE (PSF-1): this proves the drive arm CAN advance, but it exercises the
    /// `TcpConnectTimeout` advancing trigger, which the production session loop
    /// does NOT currently route through `shadow_observe_failover` (see the method
    /// doc + `fov6_production_triggers_do_not_advance_under_drive`). It documents
    /// FSM capability, not the production wiring.
    #[test]
    fn fov6_config_drive_arm_advances_current_pool_index() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut cfg = failover_config(); // primary + 2 backups
        cfg.smart_failover_enabled = true;
        cfg.smart_failover_drive = true; // config arm — no env var needed
        let mut client = StratumV1Client::new(cfg, job_tx, share_rx, status_tx);
        assert_eq!(client.current_pool_index, 0);

        // Drive connect-timeout failures until the FSM crosses its error
        // threshold and advances. Bounded loop is robust to the exact default
        // `max_errors` in LuxosPoolFailoverConfig.
        let mut advanced = false;
        for _ in 0..16 {
            client.shadow_observe_failover(LuxosFailoverTrigger::TcpConnectTimeout);
            if client.current_pool_index != 0 {
                advanced = true;
                break;
            }
        }
        assert!(
            advanced,
            "drive arm must advance current_pool_index off the dead primary pool"
        );
    }

    /// FOV-6 negative: with drive OFF (`smart_failover_drive=false` and no env
    /// gate) the FSM runs observe-only — the same trigger sequence must NOT
    /// change `current_pool_index`, preserving byte-identical legacy behavior.
    #[test]
    fn fov6_shadow_only_does_not_change_current_pool_index() {
        // Robust to a polluted process env: if the global env drive gate is
        // set, drive would legitimately change selection — skip the negative.
        if std::env::var("DCENT_POOL_FAILOVER_FSM_DRIVE").is_ok() {
            return;
        }
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut cfg = failover_config();
        cfg.smart_failover_enabled = true;
        cfg.smart_failover_drive = false; // shadow only
        let mut client = StratumV1Client::new(cfg, job_tx, share_rx, status_tx);
        assert_eq!(client.current_pool_index, 0);
        for _ in 0..16 {
            client.shadow_observe_failover(LuxosFailoverTrigger::TcpConnectTimeout);
        }
        assert_eq!(
            client.current_pool_index, 0,
            "shadow-only (drive off) must not change pool selection"
        );
    }

    /// PSF-1 (2026-06-20): the two triggers the PRODUCTION session loop actually
    /// feeds `shadow_observe_failover` — `PoolInactivity` (no-notify) and
    /// `TooManyRejections` (reject-rate) — are `reconnects_same_pool()` triggers.
    /// Even with the drive arm fully ON they must NOT advance `current_pool_index`,
    /// because the FSM reconnects the same pool for these. This pins the honest
    /// limitation documented on `shadow_observe_failover`: the drive arm is
    /// observe-equivalent for pool *advancement* until an advancing trigger
    /// (`TcpConnectTimeout`/`IoError`/`AuthError`/`TlsError`) is wired into it.
    /// If a future change makes either production trigger advance under drive, this
    /// test fails and forces the doc/claim to be re-examined under a soak.
    #[test]
    fn fov6_production_triggers_do_not_advance_under_drive() {
        if std::env::var("DCENT_POOL_FAILOVER_FSM_DRIVE").is_ok() {
            return; // env arm could legitimately change behavior; skip
        }
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut cfg = failover_config(); // primary + 2 backups
        cfg.smart_failover_enabled = true;
        cfg.smart_failover_drive = true; // drive fully armed
        let mut client = StratumV1Client::new(cfg, job_tx, share_rx, status_tx);
        assert_eq!(client.current_pool_index, 0);

        // Hammer the exact production triggers well past any error threshold.
        for _ in 0..32 {
            client.shadow_observe_failover(LuxosFailoverTrigger::PoolInactivity);
            client.shadow_observe_failover(LuxosFailoverTrigger::TooManyRejections);
        }
        assert_eq!(
            client.current_pool_index, 0,
            "same-pool production triggers must not advance the pool even with drive ON \
             (the drive arm cannot advance from PoolInactivity/TooManyRejections)"
        );
    }

    #[test]
    fn weighted_split_status_reports_secondary_route() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(
            weighted_split_config(8000, 2000),
            job_tx,
            share_rx,
            status_tx,
        );

        assert!(client.user_split_enabled);
        assert_eq!(client.active_user_split_pool_index(), 0);

        client.user_split_cycle_start = Instant::now() - Duration::from_secs(500);
        assert_eq!(client.active_user_split_pool_index(), 1);
        let status = client.hashrate_split_status(false);

        assert!(status.enabled);
        assert!(status.active);
        assert_eq!(status.active_route, "secondary");
        assert_eq!(status.active_pool_index, 1);
        assert_eq!(status.primary_bps, 8000);
        assert_eq!(status.secondary_bps, 2000);
        assert!(status.cycle_remaining_s <= 100);
    }

    #[test]
    fn weighted_split_invalid_weights_disable_split() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let client = StratumV1Client::new(
            weighted_split_config(7000, 2000),
            job_tx,
            share_rx,
            status_tx,
        );

        assert!(!client.user_split_enabled);
        let status = client.hashrate_split_status(false);
        assert!(!status.enabled);
        assert_eq!(status.active_route, "disabled");
    }

    /// W24-CRASH-1 (w24-protocol Finding 1): the two hot-path
    /// `extranonce2_size` checks in `handle_notify` / `refresh_current_job`
    /// were `assert!`s. Under `panic = "abort"` a fired assert aborts the WHOLE
    /// daemon. They are now fail-soft (log + skip). This pins both behaviors:
    ///   1. happy path (valid extranonce2_size) dispatches the job unchanged,
    ///   2. sentinel-0 path returns Ok(()) and dispatches NOTHING (no abort).
    #[tokio::test]
    async fn handle_notify_fail_soft_on_unparsed_extranonce2_size() {
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        // Build a real parsed mining.notify message.
        let msg =
            parse_pool_message(notify_line("job-1", true).trim()).expect("notify line must parse");

        // --- Happy path: a valid extranonce2_size dispatches the job. ---
        client.extranonce2_size = 4;
        let res = client.handle_notify(msg.clone()).await;
        assert!(res.is_ok(), "valid notify must return Ok");
        let dispatched = job_rx.try_recv();
        assert!(
            dispatched.is_ok(),
            "valid extranonce2_size must dispatch the job (happy path unchanged)"
        );

        // --- Fail-soft path: sentinel 0 must NOT abort; it logs + skips. ---
        client.extranonce2_size = 0; // sentinel = unparsed
        let res = client.handle_notify(msg).await;
        assert!(
            res.is_ok(),
            "unparsed extranonce2_size must fail soft (return Ok), not abort the daemon"
        );
        assert!(
            job_rx.try_recv().is_err(),
            "malformed job must be dropped, not dispatched at offset 0"
        );
    }

    /// bug-hunt MED (2026-05-28): a malformed-hex `mining.notify` (here a
    /// u32-overflow `version`) must fail soft — skip THIS job, keep the session —
    /// instead of `?`-propagating a ParseError to session teardown + reconnect.
    /// One bad job from a flaky pool must not drop a healthy session.
    #[tokio::test]
    async fn handle_notify_fail_soft_on_malformed_version_hex() {
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        // Valid extranonce2_size so we reach (and exercise) the hex-parse path.
        client.extranonce2_size = 4;

        // Start from a valid notify, then corrupt only the `version` to a hex
        // string that overflows u32 (9 hex digits).
        let valid = parse_pool_message(notify_line("job-bad-ver", true).trim())
            .expect("notify line must parse");
        let bad = match valid {
            PoolMessage::Notify {
                job_id,
                prev_hash,
                coinbase1,
                coinbase2,
                merkle_branches,
                nbits,
                ntime,
                clean_jobs,
                ..
            } => PoolMessage::Notify {
                job_id,
                prev_hash,
                coinbase1,
                coinbase2,
                merkle_branches,
                version: "200000000".to_string(),
                nbits,
                ntime,
                clean_jobs,
            },
            other => panic!("expected Notify, got {other:?}"),
        };

        let res = client.handle_notify(bad).await;
        assert!(
            res.is_ok(),
            "one malformed-hex mining.notify must NOT tear down the session"
        );
        assert!(
            job_rx.try_recv().is_err(),
            "malformed job must be skipped, not dispatched"
        );
    }

    /// Fail-soft conversion of the `refresh_current_job` assert: an invalid
    /// extranonce2_size must skip the refresh (no panic), leaving the previous
    /// job active. With no `last_job`, refresh is an early return; the assert
    /// only ever fired AFTER a prior valid notify, so we seed `last_job` then
    /// corrupt the size to exercise the converted check.
    #[tokio::test]
    async fn refresh_current_job_fail_soft_on_invalid_extranonce2_size() {
        let (job_tx, mut job_rx) = mpsc::channel(4);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        // Seed a valid job via the happy notify path so last_job exists.
        client.extranonce2_size = 4;
        let msg =
            parse_pool_message(notify_line("job-1", true).trim()).expect("notify line must parse");
        client
            .handle_notify(msg)
            .await
            .expect("seed notify must succeed");
        // Drain the dispatched seed job.
        let _ = job_rx.try_recv();

        // Now corrupt the size and refresh — must NOT panic/abort.
        client.extranonce2_size = 0;
        client.refresh_current_job("test-invalid-size", true).await; // returns ()
        assert!(
            job_rx.try_recv().is_err(),
            "invalid-size refresh must skip (no rebuilt job dispatched)"
        );
    }

    /// POOL-2 (P2): a mid-session difficulty INCREASE with no following
    /// `mining.notify` must re-dispatch the current job with the NEW (harder)
    /// share target. Pre-fix `refresh_current_job` cloned `last_job` but left the
    /// notify-time `share_target`/`pool_difficulty` in place, so after a
    /// difficulty increase the re-dispatched job carried a too-easy target and the
    /// pool low-diff-rejected (code 23) the resulting shares. It must also NOT
    /// force a work restart (`clean_jobs`) for a difficulty-only refresh.
    #[tokio::test]
    async fn refresh_after_difficulty_increase_redispatches_with_new_target() {
        use crate::work::difficulty_to_target;

        let (job_tx, mut job_rx) = mpsc::channel(4);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        client.extranonce2_size = 4;

        // Seed a job at a LOW difficulty (1.0) via the happy notify path.
        client.current_difficulty = 1.0;
        let msg = parse_pool_message(notify_line("pool2-job", false).trim())
            .expect("notify line must parse");
        client
            .handle_notify(msg)
            .await
            .expect("seed notify must succeed");
        let seeded = job_rx.try_recv().expect("seed job must dispatch");
        assert_eq!(seeded.pool_difficulty, 1.0);
        assert_eq!(seeded.share_target, difficulty_to_target(1.0));

        // Pool raises difficulty mid-session with NO following notify — exactly
        // the SetDifficulty handler's path: bump current_difficulty, then refresh
        // the current job as a difficulty-only change (restart_work=false).
        client.current_difficulty = 8192.0;
        client.refresh_current_job("set_difficulty", false).await;

        let refreshed = job_rx
            .try_recv()
            .expect("difficulty change must re-dispatch the current job");
        assert_eq!(
            refreshed.share_target,
            difficulty_to_target(8192.0),
            "re-dispatched job must carry the NEW (harder) share target, not the stale one"
        );
        assert_eq!(refreshed.pool_difficulty, 8192.0);
        assert!(
            !refreshed.clean_jobs,
            "a difficulty-only refresh must NOT force a work restart"
        );
        assert_ne!(
            seeded.share_target, refreshed.share_target,
            "the harder target must actually differ from the seeded easy target"
        );
    }

    async fn closed_pool_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind closed-pool probe listener");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        format!("stratum+tcp://127.0.0.1:{}", port)
    }

    fn response_line(id: u64, result: Value) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "id": id,
                "result": result,
                "error": serde_json::Value::Null,
            })
        )
    }

    fn notify_line(job_id: &str, clean_jobs: bool) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "id": serde_json::Value::Null,
                "method": "mining.notify",
                "params": [
                    job_id,
                    "00".repeat(32),
                    "01000000",
                    "ffffffff",
                    [],
                    "20000000",
                    "1d00ffff",
                    "66112233",
                    clean_jobs
                ],
            })
        )
    }

    fn set_version_mask_line(mask: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "id": serde_json::Value::Null,
                "method": "mining.set_version_mask",
                "params": [mask],
            })
        )
    }

    async fn spawn_mock_pool(job_id: &'static str) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(32);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    ID_CONFIGURE => {
                        let _ = writer
                            .write_all(
                                response_line(
                                    ID_CONFIGURE,
                                    serde_json::json!({
                                        "version-rolling": true,
                                        "version-rolling.mask": "1fffe000",
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        let _ = writer.write_all(notify_line(job_id, true).as_bytes()).await;
                    }
                    request_id if request_id > ID_SUGGEST_DIFF => {
                        let _ = writer
                            .write_all(response_line(request_id, Value::Bool(true)).as_bytes())
                            .await;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    async fn spawn_set_version_mask_pool(job_id: &'static str, mask: &'static str) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind set-version-mask mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(32);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    ID_CONFIGURE => {
                        let _ = writer
                            .write_all(
                                response_line(
                                    ID_CONFIGURE,
                                    serde_json::json!({
                                        "version-rolling": true,
                                        "version-rolling.mask": mask,
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        let _ = writer
                            .write_all(set_version_mask_line(mask).as_bytes())
                            .await;
                        let _ = writer.write_all(notify_line(job_id, true).as_bytes()).await;
                    }
                    request_id if request_id > ID_SUGGEST_DIFF => {
                        let _ = writer
                            .write_all(response_line(request_id, Value::Bool(true)).as_bytes())
                            .await;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// STRAT-2 harness: a pool that, immediately after authorizing, emits a
    /// malformed (non-JSON) line BEFORE a valid second `mining.notify`. A
    /// session that tears down on the junk line never reads the second notify,
    /// so the second job is never dispatched. Used to prove the session
    /// survives un-parseable lines.
    ///
    /// `first_job_id` is dispatched right after AUTHORIZE (the normal flow);
    /// `second_job_id` arrives only after the junk line — its dispatch is the
    /// positive proof that the session kept running.
    async fn spawn_junk_then_notify_pool(
        first_job_id: &'static str,
        second_job_id: &'static str,
    ) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind junk-then-notify mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(32);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    ID_CONFIGURE => {
                        let _ = writer
                            .write_all(
                                response_line(
                                    ID_CONFIGURE,
                                    serde_json::json!({
                                        "version-rolling": true,
                                        "version-rolling.mask": "1fffe000",
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        // Normal first job.
                        let _ = writer
                            .write_all(notify_line(first_job_id, true).as_bytes())
                            .await;
                        // The malformed, NON-JSON line that used to tear down
                        // the whole session (serde parse error → ParseError).
                        let _ = writer.write_all(b"this is not json\n").await;
                        // A valid second job. The client only ever reads/dispatches
                        // this if it survived the junk line above.
                        let _ = writer
                            .write_all(notify_line(second_job_id, true).as_bytes())
                            .await;
                    }
                    request_id if request_id > ID_SUGGEST_DIFF => {
                        let _ = writer
                            .write_all(response_line(request_id, Value::Bool(true)).as_bytes())
                            .await;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// STRAT-2 (2026-06-20): a single malformed (non-JSON) inbound pool line
    /// must NOT tear down a healthy V1 session. Pre-fix the steady-state loop
    /// did `parse_pool_message(&line).map_err(SessionError::ParseError)?`, so
    /// any junk byte dropped the socket and forced a reconnect + work flush.
    /// This test interleaves a non-JSON line between two valid `mining.notify`
    /// messages and asserts BOTH jobs are dispatched — proving the session
    /// skipped the junk and kept mining. (Pre-fix, only the first job would
    /// ever be dispatched because the session ended on the junk line.)
    #[tokio::test]
    async fn mock_malformed_line_does_not_end_session() {
        let pool = spawn_junk_then_notify_pool("first-job", "second-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;
        drop(returned);

        // Collect every dispatched job within the run window.
        let mut dispatched_jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            dispatched_jobs.push(job.job_id.clone());
        }

        assert!(
            dispatched_jobs.iter().any(|id| id == "first-job"),
            "first job (pre-junk) must dispatch; got {dispatched_jobs:?}"
        );
        assert!(
            dispatched_jobs.iter().any(|id| id == "second-job"),
            "second job (POST-junk) must dispatch — proves the malformed line \
             did NOT end the session; got {dispatched_jobs:?}"
        );

        let _ = finish_mock_pool(pool).await;
    }

    async fn spawn_authorize_reject_pool() -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind auth-reject mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(64);

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    return;
                };
                let requests_tx = requests_tx.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();

                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = requests_tx.send(line.clone()).await;
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                        match id {
                            ID_CONFIGURE => {
                                let _ = writer
                                    .write_all(
                                        response_line(
                                            ID_CONFIGURE,
                                            serde_json::json!({
                                                "version-rolling": true,
                                                "version-rolling.mask": "1fffe000",
                                            }),
                                        )
                                        .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_SUBSCRIBE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4]))
                                            .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_AUTHORIZE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_AUTHORIZE, Value::Bool(false)).as_bytes(),
                                    )
                                    .await;
                            }
                            _ => {}
                        }
                        let _ = writer.flush().await;
                    }
                });
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    async fn spawn_silent_handshake_pool() -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind silent-handshake mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(64);

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    return;
                };
                let requests_tx = requests_tx.clone();
                tokio::spawn(async move {
                    let (reader, _writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = requests_tx.send(line).await;
                    }
                });
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    async fn spawn_fail_then_recover_then_fail_pool(job_id: &'static str) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind recovery-sequence mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(96);
        let connection_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    return;
                };
                let requests_tx = requests_tx.clone();
                let connection_index =
                    connection_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();
                    let authorize_success = connection_index == 2;

                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = requests_tx.send(line.clone()).await;
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                        match id {
                            ID_CONFIGURE => {
                                let _ = writer
                                    .write_all(
                                        response_line(
                                            ID_CONFIGURE,
                                            serde_json::json!({
                                                "version-rolling": true,
                                                "version-rolling.mask": "1fffe000",
                                            }),
                                        )
                                        .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_SUBSCRIBE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4]))
                                            .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_AUTHORIZE if authorize_success => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes(),
                                    )
                                    .await;
                                let _ =
                                    writer.write_all(notify_line(job_id, true).as_bytes()).await;
                                let _ = writer.flush().await;
                                // POOL-1: under the stronger reset gate a session
                                // must be genuinely HEALTHY (stay up past the
                                // settle window, or land an accepted share) to
                                // zero the backoff. Keep the recovered connection
                                // open longer than SESSION_HEALTHY_UPTIME (test:
                                // 150ms) before dropping so this "good session"
                                // qualifies — preserving this test's contract
                                // (a healthy recovered session resets the failure
                                // cycle) rather than the old deliver-one-then-drop
                                // shape, which now (correctly) does NOT reset.
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                return;
                            }
                            ID_AUTHORIZE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_AUTHORIZE, Value::Bool(false)).as_bytes(),
                                    )
                                    .await;
                            }
                            _ => {}
                        }
                        let _ = writer.flush().await;
                    }
                });
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// POOL-1 harness: a primary that, on EVERY connection, completes the
    /// handshake, authorizes, delivers exactly ONE `mining.notify`, then
    /// immediately drops the socket (no settle time, no shares). Pre-fix this
    /// re-zeroed the reconnect backoff each cycle (it "delivered >=1 job"), so
    /// `attempt() >= 3` never tripped and the client looped forever on the broken
    /// primary. Post-fix the session is too short / share-less to count as
    /// healthy, the backoff accumulates, and the client fails over to the backup.
    async fn spawn_deliver_one_then_drop_pool(job_id: &'static str) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind deliver-one-then-drop mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(96);

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    return;
                };
                let requests_tx = requests_tx.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let _ = requests_tx.send(line.clone()).await;
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                        match id {
                            ID_CONFIGURE => {
                                let _ = writer
                                    .write_all(
                                        response_line(
                                            ID_CONFIGURE,
                                            serde_json::json!({
                                                "version-rolling": true,
                                                "version-rolling.mask": "1fffe000",
                                            }),
                                        )
                                        .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_SUBSCRIBE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4]))
                                            .as_bytes(),
                                    )
                                    .await;
                            }
                            ID_AUTHORIZE => {
                                let _ = writer
                                    .write_all(
                                        response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes(),
                                    )
                                    .await;
                                // Exactly one job, then drop the socket immediately.
                                let _ =
                                    writer.write_all(notify_line(job_id, true).as_bytes()).await;
                                let _ = writer.flush().await;
                                return;
                            }
                            _ => {}
                        }
                        let _ = writer.flush().await;
                    }
                });
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    async fn finish_mock_pool(mut mock: MockPool) -> Vec<String> {
        if tokio::time::timeout(Duration::from_secs(1), &mut mock.task)
            .await
            .is_err()
        {
            mock.task.abort();
            let _ = mock.task.await;
        }
        let mut requests = Vec::new();
        while let Ok(request) = mock.requests_rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    async fn run_client_for_mock_wave(
        client: StratumV1Client,
        duration: Duration,
    ) -> StratumV1Client {
        tokio::time::timeout(Duration::from_secs(8), client.run_until_sv2_retry(duration))
            .await
            .expect("client should return before test timeout")
    }

    async fn wait_for_mock_mining(status_rx: &mut mpsc::Receiver<StratumStatus>) {
        let wait = async {
            while let Some(status) = status_rx.recv().await {
                if matches!(status, StratumStatus::StateChanged(StratumState::Mining)) {
                    return;
                }
            }
            panic!("status channel closed before mock pool reached mining state");
        };

        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .expect("mock pool should reach mining state");
    }

    #[tokio::test]
    async fn mock_handshake_uses_configured_version_rolling_mask() {
        let pool = spawn_mock_pool("mask-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        config.version_rolling_mask = 0x00ff_e000;
        config.suggest_difficulty = Some(8192);

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(200)).await;
        drop(returned);

        let job = job_rx.try_recv().expect("mock pool job");
        assert_eq!(job.version_mask, 0x00ff_e000);

        let requests = finish_mock_pool(pool).await;
        let configure = requests
            .iter()
            .find(|request| request.contains("\"method\":\"mining.configure\""))
            .expect("configure request");
        let value: Value = serde_json::from_str(configure).expect("configure JSON");

        assert_eq!(value["params"][1]["version-rolling.mask"], "00ffe000");
        assert_eq!(value["params"][1]["minimum-difficulty.value"], 8192);
        assert_eq!(
            value["params"][0],
            serde_json::json!([
                "version-rolling",
                "minimum-difficulty",
                "subscribe-extranonce"
            ])
        );
    }

    #[tokio::test]
    async fn suggest_difficulty_is_static_startup_hint_not_runtime_floor() {
        let pool = spawn_handshake_then_drive(|mut writer| {
            tokio::spawn(async move {
                let _ = writer
                    .write_all(notify_line("suggest-static-job", true).as_bytes())
                    .await;
                let _ = writer.flush().await;
                tokio::time::sleep(Duration::from_millis(30)).await;

                let set_difficulty = serde_json::json!({
                    "id": null,
                    "method": "mining.set_difficulty",
                    "params": [32768.0],
                })
                .to_string()
                    + "\n";
                let _ = writer.write_all(set_difficulty.as_bytes()).await;
                let _ = writer.flush().await;
                // Hold the socket open past the client's 400 ms run window so
                // this test stays on the steady-state vardiff path rather than
                // racing a reconnect/backoff after the mock writer drops.
                tokio::time::sleep(Duration::from_millis(600)).await;
            })
        })
        .await;

        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        config.suggest_difficulty = Some(8192);

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;
        assert_eq!(
            returned.current_difficulty, 32768.0,
            "pool mining.set_difficulty must remain authoritative after the startup hint"
        );
        drop(returned);

        let requests = finish_mock_pool(pool).await;
        let configure = requests
            .iter()
            .find(|request| request.contains("\"method\":\"mining.configure\""))
            .expect("configure request");
        let configure_json: Value = serde_json::from_str(configure).expect("configure JSON");
        assert_eq!(
            configure_json["params"][1]["minimum-difficulty.value"],
            8192
        );

        let suggest_count = requests
            .iter()
            .filter(|request| request.contains("\"method\":\"mining.suggest_difficulty\""))
            .count();
        assert_eq!(
            suggest_count, 1,
            "suggest_difficulty is a static handshake hint and must not be re-sent mid-session"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.contains("32768") && request.contains("suggest")),
            "pool-driven vardiff must not be echoed back as a new suggest_difficulty request"
        );
    }

    #[tokio::test]
    async fn mock_handshake_reports_authorized_before_dispatched_work_reports_mining() {
        let pool = spawn_mock_pool("state-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(32);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(200)).await;
        drop(returned);

        let job = job_rx.try_recv().expect("mock pool job");
        assert_eq!(job.job_id, "state-job");

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        let authorized_pos = statuses
            .iter()
            .position(|status| {
                matches!(
                    status,
                    StratumStatus::StateChanged(StratumState::Authorized)
                )
            })
            .expect("handshake must report Authorized");
        let mining_pos = statuses
            .iter()
            .position(|status| matches!(status, StratumStatus::StateChanged(StratumState::Mining)))
            .expect("dispatched work must report Mining");
        assert!(
            authorized_pos < mining_pos,
            "Mining must not be reported until after the Authorized handshake state"
        );

        let _requests = finish_mock_pool(pool).await;
    }

    #[tokio::test]
    async fn mining_set_version_mask_is_ignored_when_version_rolling_disabled() {
        let pool = spawn_set_version_mask_pool("no-vr-job", "1fffe000").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        config.version_rolling = false;

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(32);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(250)).await;
        assert_eq!(
            returned.version_mask, 0,
            "disabled version rolling must keep the session mask at zero"
        );
        drop(returned);

        let job = job_rx.try_recv().expect("mock pool job");
        assert_eq!(job.job_id, "no-vr-job");
        assert_eq!(
            job.version_mask, 0,
            "disabled version rolling must not leak mining.set_version_mask into dispatched jobs"
        );

        let requests = finish_mock_pool(pool).await;
        assert!(
            !requests
                .iter()
                .any(|request| request.contains("\"method\":\"mining.configure\"")),
            "version_rolling=false must not send mining.configure"
        );
    }

    async fn collect_until_mock_request(
        mock: &mut MockPool,
        needle: &str,
        requests: &mut Vec<String>,
    ) -> String {
        let wait = async {
            while let Some(request) = mock.requests_rx.recv().await {
                let matched = request.contains(needle);
                requests.push(request.clone());
                if matched {
                    return request;
                }
            }
            panic!("mock pool request channel closed before {needle}");
        };

        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .expect("mock pool should receive expected request")
    }

    #[tokio::test]
    async fn mock_primary_connect_failure_switches_to_backup_and_flushes_work() {
        let backup = spawn_mock_pool("backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = closed_pool_url().await;
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(32);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_secs(3)).await;
        assert_eq!(returned.current_pool_index, 1);
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        let flush_pos = jobs
            .iter()
            .position(JobTemplate::is_flush_only)
            .expect("pool failover should emit flush-only job");
        let backup_job_pos = jobs
            .iter()
            .position(|job| job.job_id == "backup-job")
            .expect("backup pool should deliver work");
        assert!(flush_pos < backup_job_pos);

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        let pool_switch = statuses
            .iter()
            .find_map(|status| match status {
                StratumStatus::PoolFailoverUpdated(failover) if failover.event == "pool_switch" => {
                    Some(failover)
                }
                _ => None,
            })
            .expect("pool switch failover status");
        assert_eq!(pool_switch.active_pool_index, 1);
        assert_eq!(pool_switch.active_pool_priority, 2);
        assert_eq!(pool_switch.switch_count, 1);
        assert_eq!(
            pool_switch.last_switch_reason.as_deref(),
            Some("consecutive_failure_threshold")
        );
        assert!(pool_switch.stale_jobs_flushed_on_switch);

        let requests = finish_mock_pool(backup).await;
        assert!(requests
            .iter()
            .any(|request| request.contains("\"method\":\"mining.authorize\"")));
        assert!(requests
            .iter()
            .any(|request| request.contains("user.backup")));
    }

    #[tokio::test]
    async fn mock_primary_authorization_failure_switches_to_backup_and_flushes_work() {
        let primary = spawn_authorize_reject_pool().await;
        let backup = spawn_mock_pool("auth-backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = primary.url.clone();
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(64);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_secs(4)).await;
        assert_eq!(returned.current_pool_index, 1);
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        let flush_pos = jobs
            .iter()
            .position(JobTemplate::is_flush_only)
            .expect("authorization failover should emit flush-only job");
        let backup_job_pos = jobs
            .iter()
            .position(|job| job.job_id == "auth-backup-job")
            .expect("backup pool should deliver work after authorization failures");
        assert!(flush_pos < backup_job_pos);

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        let pool_switch = statuses
            .iter()
            .find_map(|status| match status {
                StratumStatus::PoolFailoverUpdated(failover) if failover.event == "pool_switch" => {
                    Some(failover)
                }
                _ => None,
            })
            .expect("pool switch failover status");
        assert_eq!(pool_switch.active_pool_index, 1);
        assert_eq!(pool_switch.last_failure_pool_index, Some(0));
        assert_eq!(
            pool_switch.last_failure_reason.as_deref(),
            Some("authorization_failed")
        );
        assert!(pool_switch.stale_jobs_flushed_on_switch);

        let primary_requests = finish_mock_pool(primary).await;
        let backup_requests = finish_mock_pool(backup).await;
        assert!(primary_requests
            .iter()
            .any(|request| request.contains("\"method\":\"mining.authorize\"")));
        assert!(backup_requests
            .iter()
            .any(|request| request.contains("user.backup")));
    }

    #[tokio::test]
    async fn mock_primary_handshake_stall_switches_to_backup_with_timeout() {
        let primary = spawn_silent_handshake_pool().await;
        let backup = spawn_mock_pool("handshake-backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = primary.url.clone();
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(64);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_secs(4)).await;
        assert_eq!(returned.current_pool_index, 1);
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        let flush_pos = jobs
            .iter()
            .position(JobTemplate::is_flush_only)
            .expect("handshake timeout failover should emit flush-only job");
        let backup_job_pos = jobs
            .iter()
            .position(|job| job.job_id == "handshake-backup-job")
            .expect("backup pool should deliver work after handshake timeout");
        assert!(flush_pos < backup_job_pos);

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        let pool_switch = statuses
            .iter()
            .find_map(|status| match status {
                StratumStatus::PoolFailoverUpdated(failover) if failover.event == "pool_switch" => {
                    Some(failover)
                }
                _ => None,
            })
            .expect("pool switch failover status");
        assert_eq!(pool_switch.active_pool_index, 1);
        assert_eq!(pool_switch.last_failure_pool_index, Some(0));
        assert_eq!(
            pool_switch.last_failure_reason.as_deref(),
            Some("handshake_timeout")
        );
        assert!(pool_switch.stale_jobs_flushed_on_switch);

        let primary_requests = finish_mock_pool(primary).await;
        let backup_requests = finish_mock_pool(backup).await;
        assert!(primary_requests
            .iter()
            .any(|request| request.contains("\"method\":\"mining.configure\"")));
        assert!(backup_requests
            .iter()
            .any(|request| request.contains("user.backup")));
    }

    #[tokio::test]
    async fn mock_good_session_resets_user_failover_failure_cycle() {
        let primary = spawn_fail_then_recover_then_fail_pool("recovered-job").await;
        let backup = spawn_mock_pool("should-not-reach-backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = primary.url.clone();
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(64);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        // POOL-1: the recovered session now stays up ~200ms (> the 150ms test
        // health threshold) before dropping, so the wave window is widened to
        // 650ms — long enough to still observe >=4 primary authorize attempts and
        // the recovered job, but short enough that the post-recovery re-failure
        // cycle has NOT yet re-reached the failover threshold (so no spurious
        // pool_switch), exactly as before.
        let returned = run_client_for_mock_wave(client, Duration::from_millis(650)).await;
        assert_eq!(returned.current_pool_index, 0);
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        assert!(jobs.iter().any(|job| job.job_id == "recovered-job"));
        assert!(!jobs
            .iter()
            .any(|job| job.job_id == "should-not-reach-backup-job"));

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        assert!(!statuses.iter().any(|status| {
            matches!(
                status,
                StratumStatus::PoolFailoverUpdated(failover)
                    if failover.event == "pool_switch"
            )
        }));

        let primary_requests = finish_mock_pool(primary).await;
        let backup_requests = finish_mock_pool(backup).await;
        let primary_authorize_count = primary_requests
            .iter()
            .filter(|request| request.contains("\"method\":\"mining.authorize\""))
            .count();
        assert!(primary_authorize_count >= 4);
        assert!(backup_requests.is_empty());
    }

    /// POOL-1 (P2 reliability): a primary that delivers exactly one job then
    /// drops — every reconnect — must EVENTUALLY fail over to the backup. Pre-fix
    /// `backoff.reset()` fired whenever the session "delivered >=1 job", so the
    /// attempt counter was re-zeroed each cycle and the `attempt() >= 3` failover
    /// gate could never trip; the client looped forever on the broken primary.
    #[tokio::test]
    async fn mock_deliver_one_then_drop_primary_eventually_fails_over() {
        let primary = spawn_deliver_one_then_drop_pool("flaky-primary-job").await;
        let backup = spawn_mock_pool("pool1-backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = primary.url.clone();
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(64);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(128);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_secs(3)).await;
        assert_eq!(
            returned.current_pool_index, 1,
            "a deliver-one-then-drop primary must eventually fail over to the backup"
        );
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        assert!(
            jobs.iter().any(|job| job.job_id == "pool1-backup-job"),
            "backup pool must deliver work after failover"
        );

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        assert!(
            statuses.iter().any(|status| matches!(
                status,
                StratumStatus::PoolFailoverUpdated(failover)
                    if failover.event == "pool_switch"
            )),
            "a pool_switch failover status must be emitted for the broken primary"
        );

        let _ = finish_mock_pool(primary).await;
        let backup_requests = finish_mock_pool(backup).await;
        assert!(backup_requests
            .iter()
            .any(|request| request.contains("user.backup")));
    }

    /// POOL-1 negative control: a genuinely HEALTHY primary (stays connected and
    /// keeps delivering work) must STILL reset the backoff — the stronger reset
    /// gate must not cause a spurious failover on a working session.
    #[tokio::test]
    async fn mock_healthy_primary_does_not_spuriously_fail_over() {
        let primary = spawn_mock_pool("healthy-primary-job").await;
        let backup = spawn_mock_pool("unreached-backup-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = primary.url.clone();
        config.pool2 = Some(backup_pool(&backup.url, "user.backup"));

        let (job_tx, mut job_rx) = mpsc::channel(64);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(128);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;
        assert_eq!(
            returned.current_pool_index, 0,
            "a healthy long-lived primary must never fail over"
        );
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        assert!(jobs.iter().any(|job| job.job_id == "healthy-primary-job"));
        assert!(!jobs.iter().any(|job| job.job_id == "unreached-backup-job"));

        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        assert!(
            !statuses.iter().any(|status| matches!(
                status,
                StratumStatus::PoolFailoverUpdated(failover)
                    if failover.event == "pool_switch"
            )),
            "no pool_switch should be emitted for a healthy primary"
        );

        let _ = finish_mock_pool(primary).await;
        let backup_requests = finish_mock_pool(backup).await;
        assert!(
            backup_requests.is_empty(),
            "backup must never be contacted while the primary is healthy"
        );
    }

    #[tokio::test]
    async fn mock_donation_primary_failure_routes_submits_to_braiins_fallback() {
        let mut fallback = spawn_mock_pool("donation-fallback-job").await;
        let mut config = test_config();
        config.donation.enabled = true;
        config.donation.pool_url = closed_pool_url().await;
        config.donation.worker = "DungeonMaster".to_string();
        config.donation.fallback_enabled = true;
        config.donation.fallback_pool_url = fallback.url.clone();
        config.donation.fallback_worker = "DungeonMaster".to_string();
        config.donation.fallback_password = "x".to_string();

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (share_tx, share_rx) = mpsc::channel(4);
        let (status_tx, mut status_rx) = mpsc::channel(32);
        let mut client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
        client.donation_phase = DonationPhase::Donation;
        client.donation_pool_index = 0;

        let task = tokio::spawn(run_client_for_mock_wave(client, Duration::from_secs(30)));
        wait_for_mock_mining(&mut status_rx).await;
        share_tx
            .send(test_share("donation-fallback-job", "00000007", None))
            .await
            .expect("share send");

        // W11.13 (test stability): keep share_tx alive until the mock has
        // observed mining.submit on the wire. Dropping share_tx immediately
        // closes share_rx in the client, which makes share_rx.recv() return
        // None on the very next mining-loop select tick — and that triggers
        // SessionEndReason::Clean, which drops the TCP conn before the
        // current_thread runtime has had time to deliver the submit bytes
        // to the mock. The race is invisible in baseline because there are
        // fewer pre-submit writes; once 13 added an explicit
        // mining.extranonce.subscribe between handshake and the mining
        // loop, the runtime scheduling shifted and the mock saw a TCP
        // RST before reading the submit. Wait first, drop second.
        let mut requests = Vec::new();
        let _ = collect_until_mock_request(
            &mut fallback,
            "\"method\":\"mining.submit\"",
            &mut requests,
        )
        .await;
        drop(share_tx);
        task.abort();
        let _ = task.await;
        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }

        let mut remaining_requests = finish_mock_pool(fallback).await;
        requests.append(&mut remaining_requests);
        let authorize = requests
            .iter()
            .find(|request| request.contains("\"method\":\"mining.authorize\""))
            .expect("authorize request");
        // The primary donation pool is unreachable, so the only pool that
        // could have received this authorize is the Braiins fallback — and it
        // authorizes as D-Central's DungeonMaster worker.
        assert!(authorize.contains("DungeonMaster"));

        let submit = requests
            .iter()
            .find(|request| request.contains("\"method\":\"mining.submit\""))
            .expect("submit request");
        // The share submit lands on the fallback pool as DungeonMaster — proof
        // that a dead primary donation endpoint still routes the donation
        // slice to D-Central via the working Braiins fallback.
        assert!(submit.contains("DungeonMaster"));
        assert!(!statuses.iter().any(|status| {
            matches!(
                status,
                StratumStatus::PoolFailoverUpdated(failover)
                    if failover.event == "pool_switch"
            )
        }));
    }

    #[tokio::test]
    async fn mock_donation_disabled_never_connects_to_donation_routes() {
        let user = spawn_mock_pool("user-job").await;
        let donation = spawn_mock_pool("donation-job").await;
        let fallback = spawn_mock_pool("donation-fallback-job").await;
        let mut config = test_config();
        config.pool1.url = user.url.clone();
        config.donation.enabled = false;
        config.donation.percent = 0.0;
        config.donation.pool_url = donation.url.clone();
        config.donation.fallback_enabled = true;
        config.donation.fallback_pool_url = fallback.url.clone();
        config.donation.fallback_worker = "DungeonMaster".to_string();

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(32);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(300)).await;
        assert_eq!(returned.donation_phase, DonationPhase::Disabled);
        drop(returned);

        let mut jobs = Vec::new();
        while let Ok(job) = job_rx.try_recv() {
            jobs.push(job);
        }
        assert!(jobs.iter().any(|job| job.job_id == "user-job"));

        let user_requests = finish_mock_pool(user).await;
        let donation_requests = finish_mock_pool(donation).await;
        let fallback_requests = finish_mock_pool(fallback).await;
        assert!(user_requests
            .iter()
            .any(|request| request.contains("\"method\":\"mining.authorize\"")));
        assert!(donation_requests.is_empty());
        assert!(fallback_requests.is_empty());
    }

    #[tokio::test]
    async fn donation_fallback_routes_to_braiins_account_without_user_pool_failover() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        assert_eq!(client.donation_pool_count(), 2);
        assert_eq!(client.pool_count(), 1);

        let primary = client.donation_pool_config(0);
        assert_eq!(primary.url, "stratum+tcp://pool.d-central.tech:3333");
        assert_eq!(primary.worker, "DungeonMaster");

        assert!(client.try_switch_to_donation_fallback("unit_test"));
        let fallback = client.donation_pool_config(client.donation_pool_index);
        assert_eq!(client.donation_pool_index, 1);
        assert_eq!(fallback.url, "stratum+tcp://stratum.braiins.com:3333");
        assert_eq!(fallback.worker, "DungeonMaster");
        assert_eq!(client.pool_count(), 1);
        assert!(!client.try_switch_to_donation_fallback("unit_test"));

        client.resume_user_pool_after_donation_failure();
        assert_eq!(client.donation_pool_index, 0);
    }

    #[tokio::test]
    async fn refresh_donation_stats_mirrors_active_route_to_stats() {
        // W5.5: refresh_donation_stats must mirror the active donation
        // route (URL + worker + index) onto the shared StratumStats so the
        // dashboard chip can render "Donating to D-Central primary" /
        // "Donating via Braiins fallback" without
        // racing the daemon's StratumStatus event handler.
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        // Outside donation window: route fields are cleared, donating=false.
        client.donation_phase = DonationPhase::User;
        client.donation_pool_index = 0;
        client.refresh_donation_stats().await;
        {
            let stats = client.stats.lock().await;
            assert!(!stats.donating);
            assert_eq!(stats.donation_active_url, "");
            assert_eq!(stats.donation_active_worker, "");
            assert_eq!(stats.donation_pool_index, 0);
        }

        // Donation window on primary: D-Central primary surfaces.
        client.donation_phase = DonationPhase::Donation;
        client.donation_pool_index = 0;
        client.refresh_donation_stats().await;
        {
            let stats = client.stats.lock().await;
            assert!(stats.donating);
            assert_eq!(
                stats.donation_active_url,
                "stratum+tcp://pool.d-central.tech:3333"
            );
            // SW-08: donation worker is masked before it enters StratumStats.
            // "DungeonMaster" (13 chars) → "<first6>…<last4>".
            assert_eq!(
                stats.donation_active_worker,
                dcentrald_common::wallet_mask::mask_wallet("DungeonMaster")
            );
            assert_eq!(stats.donation_pool_index, 0);
        }

        // Donation window after fallback: visible Braiins worker surfaces.
        assert!(client.try_switch_to_donation_fallback("unit_test"));
        client.refresh_donation_stats().await;
        {
            let stats = client.stats.lock().await;
            assert!(stats.donating);
            assert_eq!(
                stats.donation_active_url,
                "stratum+tcp://stratum.braiins.com:3333"
            );
            // SW-08: masked donation worker (13 chars → "<first6>…<last4>").
            assert_eq!(
                stats.donation_active_worker,
                dcentrald_common::wallet_mask::mask_wallet("DungeonMaster")
            );
            assert_eq!(stats.donation_pool_index, 1);
        }
    }

    #[tokio::test]
    async fn donation_fallback_can_be_disabled() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut config = test_config();
        config.donation.fallback_enabled = false;
        let mut client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        assert_eq!(client.donation_pool_count(), 1);
        assert!(!client.try_switch_to_donation_fallback("unit_test"));
        assert_eq!(client.donation_pool_index, 0);
    }

    #[tokio::test]
    async fn failover_status_reports_active_pool_without_secrets() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(failover_config(), job_tx, share_rx, status_tx);

        client.current_pool_index = 1;
        client.failover_switch_count = 2;
        client.last_failover_switch_reason = Some("consecutive_failure_threshold".to_string());
        client.last_failover_failure_reason = Some("connect_error:connection timeout".to_string());
        client.last_failover_failure_pool_index = Some(0);
        client.last_pending_submit_correlations_cleared = 3;
        client.pending_submit_dropped = 5;
        client.last_stale_jobs_flushed_on_switch = true;
        client.pending_submits.push(PendingSubmitResponse::new(
            61,
            Instant::now() - Duration::from_millis(20),
            test_share("job-unresolved", "0000000a", None),
            "user.worker".to_string(),
        ));

        let status = client.failover_status("pool_switch", 3, 1_500);

        assert!(status.enabled);
        assert_eq!(status.configured_pool_count, 3);
        assert_eq!(status.active_pool_index, 1);
        assert_eq!(status.active_pool_priority, 2);
        assert_eq!(
            status.active_pool_url,
            "stratum+tcp://backup.example.com:4444"
        );
        assert!(!status.active_pool_url.contains("secret"));
        assert_eq!(
            status.last_switch_reason.as_deref(),
            Some("consecutive_failure_threshold")
        );
        assert_eq!(status.last_failure_pool_index, Some(0));
        assert_eq!(status.last_failure_pool_priority, Some(1));
        assert_eq!(status.pending_submit_correlations_cleared, 3);
        assert_eq!(status.shares_unresolved, 1);
        assert_eq!(status.pending_submit_dropped, 5);
        assert!(status.stale_jobs_flushed_on_switch);
        assert_eq!(status.backoff_ms, 1_500);
        assert_eq!(status.event, "pool_switch");
    }

    #[tokio::test]
    async fn uncorrelated_submit_response_is_not_counted_as_a_share() {
        // SEC/telemetry: a submit response whose id matches no pending submit
        // (a duplicate/late ack, a trimmed submit, or a hostile pool spamming
        // {"id":N,"result":true}) must NOT increment accepted/rejected shares.
        // Phantom counts inflate proof-of-mining telemetry and can suppress
        // reject-rate failover.
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        assert!(client.pending_submits.is_empty());

        // Fake "accepted" with no correlated pending submit.
        let fatal = client
            .handle_submit_response(9999, Some(serde_json::Value::Bool(true)), None)
            .await;
        assert!(!fatal, "an uncorrelated accept must not be session-fatal");

        // Fake "rejected" (non-auth-fatal code) with no correlated pending submit.
        client
            .handle_submit_response(
                9998,
                None,
                Some(serde_json::json!({"code": 23, "message": "Low difficulty share"})),
            )
            .await;

        let stats = client.stats.lock().await;
        assert_eq!(stats.shares_accepted, 0, "phantom accept must not count");
        assert_eq!(stats.shares_rejected, 0, "phantom reject must not count");
    }

    #[tokio::test]
    async fn correlated_submit_response_still_counts() {
        // Happy path: a response correlated to a real pending submit is counted,
        // proving the uncorrelated guard did not break normal accounting.
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        client.pending_submits.push(PendingSubmitResponse::new(
            42,
            Instant::now(),
            test_share("job-correlated", "0000000b", None),
            "user.worker".to_string(),
        ));
        client
            .handle_submit_response(42, Some(serde_json::Value::Bool(true)), None)
            .await;
        let stats = client.stats.lock().await;
        assert_eq!(stats.shares_accepted, 1, "correlated accept must count");
    }

    #[tokio::test]
    async fn failover_status_marks_single_pool_as_disabled() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        let status = client.failover_status("startup", 0, 0);

        assert!(!status.enabled);
        assert_eq!(status.configured_pool_count, 1);
        assert_eq!(status.active_pool_index, 0);
        assert_eq!(status.active_pool_priority, 1);
        assert_eq!(status.switch_count, 0);
        assert_eq!(status.telemetry_source, "stratum_v1_client");
    }

    // --- SmartSwitch toggle plumbing (matrix §7 #2 / §6 SmartSwitch row) ---
    //
    // These pin the default-off wiring of `[stratum].smart_failover_enabled`
    // (a.k.a. `[pool].smart_failover_enabled` at the daemon config layer):
    // the knob must reach the live V1 client AND be reported truthfully in
    // `PoolFailoverStatus.smart_failover_enabled`, while the OFF path stays
    // byte-identical to the pre-toggle daemon. The richer FSM-drives-selection
    // behavior is a separate Wave-H-gated V1-client-core change (see the
    // `pool_failover` module docs) — not asserted here on purpose.

    #[test]
    fn smart_failover_default_is_off() {
        // Every existing construction site leaves the knob false; the test
        // config mirrors that. This guards against a default flip.
        let cfg = test_config();
        assert!(
            !cfg.smart_failover_enabled,
            "SmartSwitch must ship default-off; promotion to true is the Wave-H operator soak gate"
        );
    }

    #[tokio::test]
    async fn smart_failover_off_knob_reaches_client_and_telemetry() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        // failover_config() has 3 pools and the default (false) toggle —
        // exercises the multi-pool legacy failover path with SmartSwitch OFF.
        let mut cfg = failover_config();
        cfg.smart_failover_enabled = false;
        let client = StratumV1Client::new(cfg, job_tx, share_rx, status_tx);

        // (a) Knob reaches the live client unchanged.
        assert!(!client.smart_failover_enabled());

        // Telemetry reports OFF truthfully and never claims SmartSwitch is
        // active; the legacy multi-pool failover (`enabled`) is unaffected.
        let status = client.failover_status("startup", 0, 0);
        assert!(!status.smart_failover_enabled);
        assert!(status.enabled, "legacy multi-pool failover still reported");
        assert_eq!(status.configured_pool_count, 3);
        assert_eq!(status.telemetry_source, "stratum_v1_client");
    }

    #[tokio::test]
    async fn smart_failover_on_knob_reaches_client_and_telemetry() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut cfg = failover_config();
        cfg.smart_failover_enabled = true;
        let client = StratumV1Client::new(cfg, job_tx, share_rx, status_tx);

        // (b) When ON, the knob is observable on the live client and surfaced
        // truthfully in telemetry (operator opted in). The legacy `enabled`
        // multi-pool flag and pool selection are unchanged — ON only flips the
        // telemetry bit until the FSM-drives-selection promotion lands.
        assert!(client.smart_failover_enabled());
        let status = client.failover_status("startup", 0, 0);
        assert!(status.smart_failover_enabled);
        assert!(status.enabled);
        assert_eq!(status.configured_pool_count, 3);
        // The active pool selection is still index 0 (legacy default) — ON did
        // NOT change which pool is active.
        assert_eq!(status.active_pool_index, 0);
    }

    #[test]
    fn smart_failover_toggle_does_not_perturb_existing_failover_config() {
        // OFF and ON configs differ ONLY in the new toggle — every other
        // failover-relevant field is byte-identical, proving the OFF path is a
        // no-op delta vs the pre-toggle daemon.
        let off = failover_config();
        let mut on = failover_config();
        on.smart_failover_enabled = true;

        assert_eq!(off.routing_mode, on.routing_mode);
        assert_eq!(
            off.primary_return_stability_secs,
            on.primary_return_stability_secs
        );
        assert_eq!(off.no_notify_failover_secs, on.no_notify_failover_secs);
        assert_eq!(off.reject_rate_failover_pct, on.reject_rate_failover_pct);
        assert_eq!(
            off.reject_rate_failover_min_samples,
            on.reject_rate_failover_min_samples
        );
        assert!(!off.smart_failover_enabled);
        assert!(on.smart_failover_enabled);
    }

    #[tokio::test]
    async fn clear_orphaned_pending_submits_reports_count() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        client.pending_submits.push(PendingSubmitResponse::new(
            71,
            Instant::now() - Duration::from_millis(12),
            test_share("job-e", "00000005", None),
            "user.worker".to_string(),
        ));
        client.pending_submits.push(PendingSubmitResponse::new(
            72,
            Instant::now() - Duration::from_millis(7),
            test_share("job-f", "00000006", Some("0000d000")),
            "user.worker".to_string(),
        ));

        assert_eq!(client.clear_orphaned_pending_submits("unit_test"), 2);
        assert!(client.pending_submits.is_empty());
        assert_eq!(client.clear_orphaned_pending_submits("unit_test"), 0);
    }

    #[tokio::test]
    async fn trim_pending_submits_reports_unresolved_and_dropped_counts() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        for id in 0..(MAX_PENDING_SUBMITS + 2) {
            client.pending_submits.push(PendingSubmitResponse::new(
                100 + id as u64,
                Instant::now() - Duration::from_millis(id as u64),
                test_share("job-cap", &format!("{id:08x}"), None),
                "user.worker".to_string(),
            ));
        }

        assert_eq!(client.trim_pending_submits(), 2);
        assert_eq!(client.pending_submits.len(), MAX_PENDING_SUBMITS);
        assert_eq!(client.pending_submit_dropped, 2);

        let status = client.failover_status("unit_test", 0, 0);
        assert_eq!(status.shares_unresolved, MAX_PENDING_SUBMITS as u64);
        assert_eq!(status.pending_submit_dropped, 2);
        assert_eq!(client.pending_submits[0].request_id, 102);
    }

    #[test]
    fn fresh_client_has_sentinel_extranonce2_size() {
        // W5.4: a freshly constructed StratumV1Client must seed
        // extranonce2_size to 0 (the "not yet parsed from mining.subscribe"
        // sentinel). The build_work / refresh_current_job assertions rely
        // on this being out-of-range so any path that dispatches work
        // before subscribe completes panics in debug AND release builds.
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        assert_eq!(client.extranonce2_size, 0);
        assert!(!is_valid_v1_extranonce2_size(client.extranonce2_size));
    }

    #[test]
    fn parse_subscribe_result_rejects_invalid_v1_extranonce2_sizes() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        client.extranonce1 = vec![0xaa];
        client.extranonce2_size = DEFAULT_V1_EXTRANONCE2_SIZE;

        for size in [
            json!(0),
            json!(MAX_V1_EXTRANONCE2_SIZE + 1),
            json!(u64::MAX),
        ] {
            let result = json!([[], "deadbeef", size]);
            let err = client
                .parse_subscribe_result(&result)
                .expect_err("invalid V1 extranonce2_size should fail subscribe");
            assert!(matches!(
                err,
                SessionError::ParseError(message)
                    if message.contains("invalid extranonce2_size")
            ));
            assert_eq!(client.extranonce1, vec![0xaa]);
            assert_eq!(client.extranonce2_size, DEFAULT_V1_EXTRANONCE2_SIZE);
        }
    }

    #[test]
    fn parse_subscribe_result_accepts_max_v1_extranonce2_size() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(1);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        let result = json!([[], "deadbeef", MAX_V1_EXTRANONCE2_SIZE]);
        client.parse_subscribe_result(&result).unwrap();

        assert_eq!(client.extranonce1, vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(client.extranonce2_size, MAX_V1_EXTRANONCE2_SIZE);
    }

    #[tokio::test]
    async fn submit_response_accept_uses_correlated_share_metadata() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        client.current_difficulty = 8192.0;

        client.pending_submits.push(PendingSubmitResponse::new(
            41,
            Instant::now() - Duration::from_millis(12),
            test_share("job-a", "00000001", Some("00000000")),
            "user.worker".to_string(),
        ));
        client.pending_submits.push(PendingSubmitResponse::new(
            42,
            Instant::now() - Duration::from_millis(8),
            test_share("job-b", "00000002", Some("0000f000")),
            "donation.worker".to_string(),
        ));

        client
            .handle_submit_response(42, Some(Value::Bool(true)), None)
            .await;

        let latency = status_rx.recv().await.expect("latency status");
        assert!(matches!(latency, StratumStatus::Latency(ms) if ms > 0));

        let accepted = status_rx.recv().await.expect("accepted status");
        match accepted {
            StratumStatus::ShareAccepted {
                job_id,
                pool_target_difficulty,
                achieved_difficulty,
                meta,
            } => {
                assert_eq!(job_id, "job-b");
                assert_eq!(pool_target_difficulty, 8192.0);
                assert_eq!(achieved_difficulty, Some(65_536.0));
                let meta = meta.expect("correlated share meta");
                assert_eq!(meta.share.worker_name, "donation.worker");
                assert_eq!(meta.share.job_id, "job-b");
                assert_eq!(meta.share.nonce, "00000002");
                assert_eq!(meta.share.ntime, "66112233");
                assert_eq!(meta.share.extranonce2, "abcd1234");
                assert_eq!(meta.share.version_bits.as_deref(), Some("0000f000"));
            }
            other => panic!("unexpected status: {:?}", other),
        }

        assert_eq!(client.pending_submits.len(), 1);
        assert_eq!(client.pending_submits[0].request_id, 41);

        let stats = client.stats.lock().await.clone();
        assert_eq!(stats.shares_accepted, 1);
        assert_eq!(stats.shares_rejected, 0);
        assert!(stats.latency_ms > 0);
    }

    #[tokio::test]
    async fn submit_response_reject_uses_correlated_share_metadata() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);

        client.pending_submits.push(PendingSubmitResponse::new(
            51,
            Instant::now() - Duration::from_millis(10),
            test_share("job-c", "00000003", None),
            "user.worker".to_string(),
        ));
        client.pending_submits.push(PendingSubmitResponse::new(
            52,
            Instant::now() - Duration::from_millis(7),
            test_share("job-d", "00000004", Some("0000e000")),
            "user.worker".to_string(),
        ));

        client
            .handle_submit_response(51, None, Some(json!([22, "Duplicate", null])))
            .await;

        let latency = status_rx.recv().await.expect("latency status");
        assert!(matches!(latency, StratumStatus::Latency(ms) if ms > 0));

        let rejected = status_rx.recv().await.expect("rejected status");
        match rejected {
            StratumStatus::ShareRejected {
                job_id,
                error_code,
                error_msg,
                meta,
            } => {
                assert_eq!(job_id, "job-c");
                assert_eq!(error_code, 22);
                assert_eq!(error_msg, "Duplicate");
                let meta = meta.expect("correlated share meta");
                assert_eq!(meta.share.worker_name, "user.worker");
                assert_eq!(meta.share.job_id, "job-c");
                assert_eq!(meta.share.nonce, "00000003");
                assert_eq!(meta.share.version_bits, None);
            }
            other => panic!("unexpected status: {:?}", other),
        }

        assert_eq!(client.pending_submits.len(), 1);
        assert_eq!(client.pending_submits[0].request_id, 52);

        let stats = client.stats.lock().await.clone();
        assert_eq!(stats.shares_accepted, 0);
        assert_eq!(stats.shares_rejected, 1);
        assert!(stats.latency_ms > 0);
    }

    // -----------------------------------------------------------------------
    // LANE S — per-pool latency surfacing (already-measured RTT, no new probe).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn latency_sample_populates_option_and_per_pool_fields() {
        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(test_config(), job_tx, share_rx, status_tx);
        client.current_difficulty = 8192.0;

        // Before any submit response is correlated, the honest fields say
        // "never measured" — None — rather than a misleading 0 ms.
        {
            let stats = client.stats.lock().await;
            assert_eq!(stats.last_latency_ms, None);
            assert!(stats.per_pool_latency_ms.iter().all(Option::is_none));
        }

        client.pending_submits.push(PendingSubmitResponse::new(
            41,
            Instant::now() - Duration::from_millis(12),
            test_share("job-a", "00000001", Some("00000000")),
            "user.worker".to_string(),
        ));

        client
            .handle_submit_response(41, Some(Value::Bool(true)), None)
            .await;

        // Drain the Latency status so the assertions below read the stats the
        // same way the daemon does.
        let latency = status_rx.recv().await.expect("latency status");
        assert!(matches!(latency, StratumStatus::Latency(ms) if ms > 0));

        let stats = client.stats.lock().await.clone();
        // The new Option field is populated from the measured sample.
        let active_sample = stats.last_latency_ms.expect("last_latency_ms set");
        assert!(active_sample > 0);
        // It mirrors the legacy scalar (same underlying sample, just honest).
        assert_eq!(active_sample as u64, stats.latency_ms);
        // The active pool (index 0) now has a per-pool sample.
        assert_eq!(
            stats.per_pool_latency_ms.first().copied().flatten(),
            Some(active_sample)
        );
    }

    #[tokio::test]
    async fn per_pool_latency_is_independent_per_pool() {
        // Three configured pools — only the active one (index 1) should record
        // a latency sample; the primary and the third stay None.
        let mut config = test_config();
        config.pool2 = Some(PoolConfig {
            url: "stratum+tcp://backup1.example.com:3333".to_string(),
            worker: "user.worker".to_string(),
            password: "x".to_string(),
            sv2_url: None,
            protocol: None,
            split_bps: None,
        });
        config.pool3 = Some(PoolConfig {
            url: "stratum+tcp://backup2.example.com:3333".to_string(),
            worker: "user.worker".to_string(),
            password: "x".to_string(),
            sv2_url: None,
            protocol: None,
            split_bps: None,
        });

        let (job_tx, _job_rx) = mpsc::channel(1);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(4);
        let mut client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
        client.current_difficulty = 8192.0;
        // Pretend we have failed over onto the first backup pool.
        client.current_pool_index = 1;

        client.pending_submits.push(PendingSubmitResponse::new(
            61,
            Instant::now() - Duration::from_millis(9),
            test_share("job-x", "00000005", None),
            "user.worker".to_string(),
        ));

        client
            .handle_submit_response(61, Some(Value::Bool(true)), None)
            .await;
        let _ = status_rx.recv().await.expect("latency status");

        let stats = client.stats.lock().await.clone();
        // The vector was grown to the configured pool count (3).
        assert_eq!(stats.per_pool_latency_ms.len(), 3);
        // Only the active backup (index 1) has a sample; the others are None.
        assert_eq!(stats.per_pool_latency_ms[0], None);
        assert!(stats.per_pool_latency_ms[1].expect("backup1 sample") > 0);
        assert_eq!(stats.per_pool_latency_ms[2], None);
    }

    // -----------------------------------------------------------------------
    // parse_32_bytes + parse_error pure-helper contracts.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_32_bytes_round_trips_valid_64_char_hex() {
        // 64-char hex → exactly 32 bytes.
        let hex = "0000000000000000000000000000000000000000000000000000000000000042";
        let bytes = parse_32_bytes(hex).unwrap();
        assert_eq!(bytes[31], 0x42);
        assert_eq!(bytes[0..31], [0u8; 31]);
    }

    #[test]
    fn parse_32_bytes_handles_uppercase_hex() {
        // hex::decode is case-insensitive — pin so a refactor doesn't
        // accidentally introduce a case-sensitive parser.
        let hex = "DEADBEEF0000000000000000000000000000000000000000000000000000ABCD";
        let bytes = parse_32_bytes(hex).unwrap();
        assert_eq!(bytes[0], 0xDE);
        assert_eq!(bytes[31], 0xCD);
    }

    #[test]
    fn parse_32_bytes_rejects_odd_length_hex() {
        // 63 chars (odd) — must surface a parse error, NOT panic.
        let baseline =
            parse_32_bytes("0000000000000000000000000000000000000000000000000000000000000042");
        assert!(baseline.is_ok(), "64-char baseline must work");

        let too_odd =
            parse_32_bytes("000000000000000000000000000000000000000000000000000000000000004");
        assert!(matches!(too_odd, Err(SessionError::ParseError(_))));
    }

    #[test]
    fn parse_32_bytes_rejects_too_short_hex() {
        // 62 chars valid hex = 31 bytes. Length check must catch it.
        let too_short = "00000000000000000000000000000000000000000000000000000000000042";
        let result = parse_32_bytes(too_short);
        assert!(
            matches!(result, Err(SessionError::ParseError(msg)) if msg.contains("expected 32"))
        );
    }

    #[test]
    fn parse_32_bytes_rejects_too_long_hex() {
        // 66 chars valid hex = 33 bytes. Length check must catch it.
        let too_long = "0000000000000000000000000000000000000000000000000000000000004242aa";
        let result = parse_32_bytes(too_long);
        assert!(
            matches!(result, Err(SessionError::ParseError(msg)) if msg.contains("expected 32"))
        );
    }

    #[test]
    fn parse_32_bytes_rejects_non_hex_characters() {
        let bad = "ZZ00000000000000000000000000000000000000000000000000000000000042";
        let result = parse_32_bytes(bad);
        assert!(matches!(result, Err(SessionError::ParseError(_))));
    }

    #[test]
    fn parse_32_bytes_rejects_empty_string() {
        let result = parse_32_bytes("");
        assert!(matches!(result, Err(SessionError::ParseError(_))));
    }

    #[test]
    fn parse_error_extracts_code_and_message_from_array() {
        let err = json!([21, "Low difficulty share", null]);
        let (code, msg) = parse_error(&err);
        assert_eq!(code, 21);
        assert_eq!(msg, "Low difficulty share");
    }

    #[test]
    fn parse_error_handles_negative_code() {
        // Stratum errors commonly use negative codes.
        let err = json!([-1, "Invalid request"]);
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        assert_eq!(msg, "Invalid request");
    }

    #[test]
    fn parse_error_falls_back_to_unknown_for_empty_array() {
        let err = json!([]);
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        assert_eq!(msg, "Unknown error");
    }

    #[test]
    fn reject_code_advice_covers_canonical_table_f011() {
        // gap-swarm F-011: codes 26 (reserved) + 27 (invalid version mask) must no
        // longer fall to the generic arm, and 25 must name "not subscribed" (was a
        // wrong duplicate of 23's "low difficulty").
        assert!(reject_code_advice(21).contains("job not found"));
        assert!(reject_code_advice(22).contains("duplicate"));
        assert!(reject_code_advice(24).contains("unauthorized"));
        assert!(reject_code_advice(25).contains("not subscribed"));
        assert!(reject_code_advice(26).contains("reserved"));
        assert!(reject_code_advice(27).contains("version mask"));
        // unknown codes still get the generic fallback
        let generic = reject_code_advice(99);
        assert!(generic.contains("check pool documentation"));
        // every canonical code 20-27 has specific (non-generic) advice
        for c in [20i64, 21, 22, 23, 24, 25, 26, 27] {
            assert_ne!(
                reject_code_advice(c),
                generic,
                "reject code {} must have specific advice, not the generic fallback",
                c
            );
        }
    }

    #[test]
    fn parse_error_falls_back_to_unknown_for_missing_message() {
        // [code] with no second element — message defaults to "Unknown error".
        let err = json!([42]);
        let (code, msg) = parse_error(&err);
        assert_eq!(code, 42);
        assert_eq!(msg, "Unknown error");
    }

    #[test]
    fn parse_error_falls_back_to_unknown_for_non_string_message() {
        let err = json!([42, 999]); // numeric "message"
        let (code, msg) = parse_error(&err);
        assert_eq!(code, 42);
        assert_eq!(msg, "Unknown error");
    }

    #[test]
    fn parse_error_falls_back_to_neg1_code_for_non_numeric_code() {
        let err = json!(["not-a-code", "msg"]);
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        assert_eq!(msg, "msg");
    }

    #[test]
    fn parse_error_for_non_array_value_renders_raw_json() {
        // Non-array Stratum errors get rendered with their JSON form so
        // operators can see the raw bad payload.
        let err = json!({"weird": "object"});
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        assert!(msg.contains("weird"), "got: {msg}");
    }

    #[test]
    fn parse_error_handles_string_value() {
        let err = json!("plain string error");
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        assert!(msg.contains("plain string error"));
    }

    #[test]
    fn parse_error_handles_null_value() {
        let err = Value::Null;
        let (code, msg) = parse_error(&err);
        assert_eq!(code, -1);
        // Null renders as "null" in JSON form.
        assert_eq!(msg, "null");
    }

    // ============================================================
    // W11.13 — Bitmain Stratum V1 extensions (RE2 §16, MASTER_RE §14).
    //
    // Six tests pin the BIP310 version-rolling contract and the four
    // Bitmain-specific server-initiated extensions:
    //   * mining.configure carries version-rolling on subscribe
    //   * mining.configure fail-closed when pool stays silent on the mask
    //   * mining.extranonce.subscribe rotates extranonce1 without restart
    //   * client.get_version responds with the SwVersion string
    //   * client.reconnect triggers a TCP reconnect to pool-supplied target
    //   * client.show_message appends to the per-pool log
    //
    // Implementation notes:
    //   * All six tests reuse the existing MockPool infrastructure where
    //     practical; new spawn_* helpers are added only when an existing
    //     helper would obscure intent.
    //   * `run_client_for_mock_wave` returns `StratumV1Client` so we can
    //     read post-run state (`version_mask`, `extranonce1`, stats) from
    //     the same client that ran against the pool. Some tests run the
    //     client task in the background and abort once a request lands;
    //     in that case we read state through the `stats` Arc.
    //   * Wire format pins for the configure/extranonce.subscribe builders
    //     live in `messages::tests`; these tests pin runtime *behavior*.
    // ============================================================

    /// Mock pool that completes the standard handshake (with version-rolling
    /// mask 0x1fffe000) and then forwards every received line to the
    /// requests channel. Used by the configure/extranonce.subscribe/
    /// get_version/show_message/reconnect tests below.
    async fn spawn_handshake_then_drive(
        on_authorized: impl FnOnce(tokio::net::tcp::OwnedWriteHalf) -> tokio::task::JoinHandle<()>
            + Send
            + 'static,
    ) -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind handshake-driver mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(64);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    ID_CONFIGURE => {
                        let _ = writer
                            .write_all(
                                response_line(
                                    ID_CONFIGURE,
                                    serde_json::json!({
                                        "version-rolling": true,
                                        "version-rolling.mask": "1fffe000",
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        let _ = writer.flush().await;
                        // Hand off to the test-supplied driver so each
                        // test can push its specific server-initiated
                        // notification (set_extranonce, get_version,
                        // show_message, reconnect, ...).
                        let _ = on_authorized(writer);
                        // After handoff, keep draining inbound lines so
                        // mining.extranonce.subscribe etc. are forwarded.
                        while let Ok(Some(line)) = lines.next_line().await {
                            let _ = requests_tx.send(line).await;
                        }
                        return;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// Pool that replies to mining.configure WITHOUT a version-rolling.mask
    /// field — used for the fail-closed test. We acknowledge the request
    /// but omit the mask; per BIP310 the miner must NOT roll any bits in
    /// this case (silent pool implies unsupported).
    async fn spawn_silent_version_rolling_pool() -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind silent-version-rolling mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(64);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    ID_CONFIGURE => {
                        // Acknowledge the configure but DO NOT include a
                        // `version-rolling.mask` key. BIP310: miner MUST
                        // NOT roll any version bits in this case.
                        let _ = writer
                            .write_all(
                                response_line(
                                    ID_CONFIGURE,
                                    serde_json::json!({
                                        "version-rolling": false,
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        let _ = writer
                            .write_all(notify_line("silent-mask-job", true).as_bytes())
                            .await;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// G28 / F-002 mock: answers subscribe + authorize but SILENTLY DROPS
    /// `mining.configure` (legacy AntPool :3333 behavior). The handshake must
    /// still complete (the loop exits on subscribed && authorized) and mining
    /// must proceed with `version_mask == 0` (no ASICBoost) — never hang/retry.
    async fn spawn_configure_dropped_pool() -> MockPool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind configure-dropped mock pool");
        let port = listener.local_addr().expect("mock local addr").port();
        let (requests_tx, requests_rx) = mpsc::channel(64);

        let task = tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = requests_tx.send(line.clone()).await;
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0);
                match id {
                    // ID_CONFIGURE: intentionally NO response (legacy-pool drop).
                    ID_SUBSCRIBE => {
                        let _ = writer
                            .write_all(
                                response_line(ID_SUBSCRIBE, json!([[], "deadbeef", 4])).as_bytes(),
                            )
                            .await;
                    }
                    ID_AUTHORIZE => {
                        let _ = writer
                            .write_all(response_line(ID_AUTHORIZE, Value::Bool(true)).as_bytes())
                            .await;
                        let _ = writer
                            .write_all(notify_line("configure-dropped-job", true).as_bytes())
                            .await;
                    }
                    _ => {}
                }
                let _ = writer.flush().await;
            }
        });

        MockPool {
            url: format!("stratum+tcp://127.0.0.1:{}", port),
            requests_rx,
            task,
        }
    }

    /// G28 / F-002 — a pool that silently drops `mining.configure` must NOT hang
    /// the handshake: the loop completes on subscribe + authorize and mining
    /// proceeds with `version_mask == 0` (no ASICBoost). Pins the
    /// no-regression-by-construction property verified in
    /// .
    #[tokio::test]
    async fn mining_configure_dropped_proceeds_without_version_rolling() {
        let pool = spawn_configure_dropped_pool().await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        config.version_rolling = true;
        config.version_rolling_mask = 0x1fff_e000;

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(200)).await;
        // Configure was never answered → no ASICBoost, but the handshake still
        // completed (we reached the mining wave and returned cleanly, not a hang).
        assert_eq!(
            returned.version_mask, 0,
            "dropped mining.configure must leave version_mask=0 (no ASICBoost), not hang the handshake"
        );
        drop(returned);

        let requests = finish_mock_pool(pool).await;
        // We DID send configure (the pool just didn't answer) and DID complete
        // subscribe + authorize.
        assert!(
            requests
                .iter()
                .any(|r| r.contains("\"method\":\"mining.configure\"")),
            "client must still send mining.configure"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.contains("\"method\":\"mining.subscribe\"")),
            "subscribe must complete"
        );
        assert!(
            requests
                .iter()
                .any(|r| r.contains("\"method\":\"mining.authorize\"")),
            "authorize must complete"
        );
    }

    /// W11.13 test 1 — `mining.configure` carries the BIP310 `version-rolling`
    /// extension on subscribe. Pins both the method name AND the request
    /// order (configure must precede subscribe + authorize on the wire).
    #[tokio::test]
    async fn mining_configure_sends_version_rolling_in_subscribe() {
        let pool = spawn_mock_pool("vr-job").await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        // Use a non-default mask so the test catches a future regression
        // that hardcodes 0x1fffe000 instead of plumbing config through.
        config.version_rolling_mask = 0x1ffe_0000;

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(200)).await;
        // The pool's response carries 0x1fffe000; our requested mask is
        // 0x1ffe0000. The clamp keeps only the bits common to both, which
        // is exactly 0x1ffe0000.
        assert_eq!(
            returned.version_mask, 0x1ffe_0000,
            "configured mask must clamp pool-advertised bits, not be overwritten"
        );
        drop(returned);

        let requests = finish_mock_pool(pool).await;

        // Configure must come BEFORE subscribe + authorize on the wire.
        let configure_pos = requests
            .iter()
            .position(|r| r.contains("\"method\":\"mining.configure\""))
            .expect("configure was sent");
        let subscribe_pos = requests
            .iter()
            .position(|r| r.contains("\"method\":\"mining.subscribe\""))
            .expect("subscribe was sent");
        let authorize_pos = requests
            .iter()
            .position(|r| r.contains("\"method\":\"mining.authorize\""))
            .expect("authorize was sent");
        assert!(
            configure_pos < subscribe_pos,
            "mining.configure must precede mining.subscribe (got {} >= {})",
            configure_pos,
            subscribe_pos
        );
        assert!(
            subscribe_pos < authorize_pos,
            "mining.subscribe must precede mining.authorize"
        );

        let configure: Value =
            serde_json::from_str(&requests[configure_pos]).expect("configure JSON");
        // Extension array must include "version-rolling" first so legacy
        // BIP310-only pools see it before the optional extras.
        assert_eq!(
            configure["params"][0][0], "version-rolling",
            "first extension in mining.configure must be version-rolling"
        );
        // Mask is the operator-configured value, not the pool's preferred
        // mask. The pool clamps; we propose.
        assert_eq!(
            configure["params"][1]["version-rolling.mask"], "1ffe0000",
            "mining.configure must carry the operator-configured mask"
        );
    }

    /// W11.13 test 2 — `mining.configure` fail-closed when the pool
    /// acknowledges the request but does NOT include
    /// `version-rolling.mask`. Per BIP310, the miner MUST NOT roll any
    /// version bits in this case. Crucial — silently allowing unrolled
    /// version submission while the miner thinks it's rolling produces
    /// hardware-error shares the pool will reject.
    #[tokio::test]
    async fn mining_configure_fail_closed_when_pool_silent() {
        let pool = spawn_silent_version_rolling_pool().await;
        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();
        // Operator wants ASICBoost...
        config.version_rolling_mask = 0x1fff_e000;
        config.version_rolling = true;

        let (job_tx, mut job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(200)).await;
        // CRITICAL: version_mask MUST stay 0 when the pool is silent on
        // the mask key. The previous implementation defaulted to the
        // operator's requested mask if the pool didn't clamp — that is
        // unsafe; pools that don't honor BIP310 will reject all rolled
        // shares as `unknown-work` or `bad-version`.
        assert_eq!(
            returned.version_mask, 0,
            "fail-closed: pool silent on version-rolling.mask MUST yield mask=0 (no rolling)"
        );

        // Job templates dispatched to the pipeline must also carry mask=0
        // so the work builder generates straight (un-rolled) midstates.
        let job = job_rx
            .try_recv()
            .expect("at least one job from silent pool");
        assert_eq!(
            job.version_mask, 0,
            "JobTemplate.version_mask must mirror the negotiated (zero) mask, not the operator request"
        );
        drop(returned);

        let _ = finish_mock_pool(pool).await;
    }

    /// W11.13 test 3 — `mining.set_extranonce` rotates extranonce1
    /// without restarting work. Pool sends a mid-session set_extranonce
    /// notification; the V1 client must update extranonce1 +
    /// extranonce2_size in place and refresh the current job rather
    /// than tearing down the connection.
    ///
    /// Also covers the explicit `mining.extranonce.subscribe` extension:
    /// the request is sent after handshake completes so Bitmain-flavored
    /// pools that key off the explicit method send set_extranonce updates.
    #[tokio::test]
    async fn extranonce_subscribe_updates_extranonce1_without_restarting_work() {
        let pool = spawn_handshake_then_drive(|mut writer| {
            tokio::spawn(async move {
                // Send first job to put the client into the mining loop.
                let _ = writer
                    .write_all(notify_line("preroll-job", true).as_bytes())
                    .await;
                let _ = writer.flush().await;
                // Give the client a beat to enter the mining-loop select!
                // and process the explicit mining.extranonce.subscribe.
                tokio::time::sleep(Duration::from_millis(50)).await;
                // Push a server-initiated mining.set_extranonce.
                // Format per Bitmain pools: ["new_extranonce1", new_size]
                let set_extranonce = serde_json::json!({
                    "id": null,
                    "method": "mining.set_extranonce",
                    "params": ["cafef00d", 6],
                })
                .to_string()
                    + "\n";
                let _ = writer.write_all(set_extranonce.as_bytes()).await;
                let _ = writer.flush().await;
                // Hold the connection open long enough for the client to
                // consume the rotation and refresh its job.
                tokio::time::sleep(Duration::from_millis(200)).await;
            })
        })
        .await;

        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, _status_rx) = mpsc::channel(16);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;

        // Extranonce1 must reflect the rotation, not the original
        // subscribe value. extranonce2_size must follow.
        assert_eq!(
            hex::encode(&returned.extranonce1),
            "cafef00d",
            "mid-session extranonce1 rotation must take effect"
        );
        assert_eq!(
            returned.extranonce2_size, 6,
            "mid-session extranonce2_size rotation must take effect"
        );
        drop(returned);

        let requests = finish_mock_pool(pool).await;
        // The explicit Bitmain extension request must also have been sent
        // post-handshake. Pools that don't honor it ignore the request;
        // pools that do honor it use it as the trigger for set_extranonce
        // rotation.
        assert!(
            requests
                .iter()
                .any(|r| r.contains("\"method\":\"mining.extranonce.subscribe\"")),
            "client must send Bitmain mining.extranonce.subscribe after handshake (Bitmain extension parallel path to BIP310 subscribe-extranonce capability)"
        );
    }

    /// Pure coverage for the `client.get_version` reply,
    /// replacing the runtime-fragile integration test below (which hangs under
    /// full-suite concurrency). The client answers `client.get_version` with
    /// `version_response(id, USER_AGENT)` (see the PoolMessage::GetVersion arm),
    /// and USER_AGENT is `dcentrald/<version>`.
    #[test]
    fn get_version_response_uses_dcentrald_user_agent() {
        assert!(
            USER_AGENT.starts_with("dcentrald/"),
            "USER_AGENT must start with `dcentrald/` (got {USER_AGENT:?})"
        );
        assert!(
            USER_AGENT.len() > "dcentrald/".len(),
            "USER_AGENT must include the package version after the slash"
        );
        let line = crate::v1::messages::version_response(7777, USER_AGENT);
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["id"].as_u64(), Some(7777));
        assert_eq!(v["result"].as_str(), Some(USER_AGENT));
        assert!(
            v["error"].is_null(),
            "get_version reply must have null error"
        );
        assert!(
            line.ends_with('\n'),
            "stratum line must be newline-terminated"
        );
    }

    /// W11.13 test 5 — `client.reconnect` triggers a TCP reconnect to the
    /// pool-supplied target. The session ends with `Reconnect{host,port,wait_seconds}`
    /// and the outer loop honors the new endpoint via `pending_reconnect`.
    /// We assert behavior by observing that after `client.reconnect` is
    /// pushed, the client tears down the current TCP socket (so the
    /// mock pool's read loop terminates) and the pending_reconnect target
    /// is queued for the next iteration.
    #[tokio::test]
    async fn client_reconnect_triggers_tcp_reconnect() {
        let pool = spawn_handshake_then_drive(|mut writer| {
            tokio::spawn(async move {
                let _ = writer
                    .write_all(notify_line("reconnect-job", true).as_bytes())
                    .await;
                let _ = writer.flush().await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                // Pool requests reconnect to alt.pool.example.com:9999
                // with wait_seconds=0 (no delay).
                let req = serde_json::json!({
                    "id": null,
                    "method": "client.reconnect",
                    "params": ["alt.pool.example.com", 9999, 0],
                })
                .to_string()
                    + "\n";
                let _ = writer.write_all(req.as_bytes()).await;
                let _ = writer.flush().await;
                tokio::time::sleep(Duration::from_millis(150)).await;
            })
        })
        .await;

        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(64);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;
        drop(returned);

        // The daemon-facing status stream must carry a ReconnectRequested
        // event with the pool-supplied endpoint so the dashboard can
        // surface "pool requested move to alt.pool.example.com:9999".
        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        let reconnect_evt = statuses
            .iter()
            .find_map(|s| match s {
                StratumStatus::ReconnectRequested {
                    host,
                    port,
                    wait_seconds,
                } => Some((host.clone(), *port, *wait_seconds)),
                _ => None,
            })
            .expect("client.reconnect must emit StratumStatus::ReconnectRequested");
        assert_eq!(reconnect_evt.0, "alt.pool.example.com");
        assert_eq!(reconnect_evt.1, 9999);
        assert_eq!(reconnect_evt.2, 0);

        let _ = finish_mock_pool(pool).await;
    }

    /// W11.13 test 6 — `client.show_message` appends to the bounded
    /// per-pool message ring buffer surfaced via StratumStats. Pool sends
    /// a maintenance-window notice; the V1 client appends an entry and
    /// also dispatches StratumStatus::PoolMessage for legacy logging.
    #[tokio::test]
    async fn client_show_message_appends_to_pool_message_log() {
        let pool = spawn_handshake_then_drive(|mut writer| {
            tokio::spawn(async move {
                let _ = writer
                    .write_all(notify_line("showmsg-job", true).as_bytes())
                    .await;
                let _ = writer.flush().await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                let req = serde_json::json!({
                    "id": null,
                    "method": "client.show_message",
                    "params": ["Pool maintenance at 02:00 UTC — expect 5 min downtime"],
                })
                .to_string()
                    + "\n";
                let _ = writer.write_all(req.as_bytes()).await;
                let _ = writer.flush().await;
                tokio::time::sleep(Duration::from_millis(150)).await;
            })
        })
        .await;

        let mut config = test_config();
        config.donation.enabled = false;
        config.pool1.url = pool.url.clone();

        let (job_tx, _job_rx) = mpsc::channel(16);
        let (_share_tx, share_rx) = mpsc::channel(1);
        let (status_tx, mut status_rx) = mpsc::channel(64);
        let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);

        let returned = run_client_for_mock_wave(client, Duration::from_millis(400)).await;

        // The bounded ring buffer (cap 16) must contain the message,
        // tagged with the active pool URL. Timestamps come from
        // SystemTime::now() so we just assert non-zero.
        let stats = returned.stats.lock().await.clone();
        assert!(
            !stats.pool_message_log.is_empty(),
            "client.show_message must append to stats.pool_message_log"
        );
        let entry = stats
            .pool_message_log
            .iter()
            .find(|e| e.message.contains("Pool maintenance at 02:00 UTC"))
            .expect("ring buffer must contain the maintenance notice");
        assert_eq!(
            entry.pool_url, pool.url,
            "log entry must be tagged with the active pool URL"
        );
        assert!(
            entry.timestamp_ms > 0,
            "log entry must carry a wall-clock timestamp"
        );
        assert!(
            entry.message.len() <= POOL_MESSAGE_MAX_LEN,
            "log entry must respect POOL_MESSAGE_MAX_LEN={POOL_MESSAGE_MAX_LEN}"
        );
        drop(returned);

        // Also pin the legacy daemon-facing PoolMessage status so removing
        // it would surface here, not just in the daemon integration test.
        let mut statuses = Vec::new();
        while let Ok(status) = status_rx.try_recv() {
            statuses.push(status);
        }
        assert!(
            statuses.iter().any(|s| matches!(
                s,
                StratumStatus::PoolMessage(m) if m.contains("Pool maintenance at 02:00 UTC")
            )),
            "client.show_message must continue dispatching StratumStatus::PoolMessage for legacy daemon log"
        );

        let _ = finish_mock_pool(pool).await;
    }
}
