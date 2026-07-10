//! Rolling-window pool share acceptance tracker.
//!
//! W6.3 (DCENT_QA): the autotuner step-up gate needs a *rolling* picture of
//! pool acceptance, not a cumulative one. Cumulative `shares_accepted /
//! (shares_accepted + shares_rejected)` on the legacy `StratumStats` slowly
//! "forgets" a recent rejection storm — by the time the cumulative number
//! drops below the gate, the autotuner has already over-stepped frequency
//! and made the rejection rate worse.
//!
//! This module owns a fixed time window (default 30 minutes) of
//! `(timestamp, accepted)` samples. The window is rolled forward on every
//! query: any sample older than `window` is dropped before the percentage
//! is computed. The percentage is reported as `0..=100` (`f64`) so the
//! autotuner gate can compare directly against `99.0`.
//!
//! Wired into `StratumV1Client` in `v1/client.rs` — every pool response to
//! `mining.submit` (accept or reject) calls `record_share()` on the
//! tracker. The result is then surfaced via
//! `StratumStats::rolling_acceptance_pct: f64` for the dashboard and the
//! autotuner step-up gate.
//!
//! Notes:
//! - The tracker uses `std::time::Instant` so it's monotonic and immune to
//!   wall-clock jumps. (Stratum reconnect storms used to skew earlier
//!   wall-clock implementations.)
//! - "Empty window returns 100.0" is intentional and documented in tests:
//!   the gate is "no rolling evidence of rejection" not "must have shares
//!   in the last 30 minutes". The latter is enforced separately in the
//!   autotuner via the existing `consecutive_clean_windows` counter.
//! - Sample storage is a `VecDeque<(Instant, bool)>` because pop-front of
//!   expired samples is O(1) per drop, and we only ever push to the back.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Default rolling window: 30 minutes.
///
/// Tuned to match the autotuner's existing `boost_back_threshold` /
/// post-tune validation cadence. A shorter window (e.g. 5 minutes) would
/// be too noisy on high-difficulty pools where one rejected share can
/// take ~2 minutes to "wash out". A longer window (e.g. 1 hour) would
/// hide rapidly-rising rejection storms from the gate.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30 * 60);

/// Rolling pool-acceptance tracker.
///
/// One instance per `StratumV1Client`. Cheap to clone via `Arc<Mutex<_>>`
/// when we need to share with the autotuner gate; the daemon currently
/// surfaces it through `StratumStats` instead, which keeps the API
/// boundary identical to the existing cumulative counters.
#[derive(Debug, Clone)]
pub struct AcceptanceTracker {
    /// Rolling window of `(timestamp, accepted)` samples.
    ///
    /// `accepted == true` for `mining.submit` accepted by the pool,
    /// `false` for any rejection (including `result=false` without an
    /// explicit error payload). The pool *target* difficulty doesn't
    /// enter — only the pool's binary verdict.
    samples: VecDeque<(Instant, bool)>,
    /// Time window over which acceptance is averaged.
    window: Duration,
}

impl AcceptanceTracker {
    /// Construct a new tracker with the default 30-minute window.
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW)
    }

    /// Construct a tracker with a custom window.
    ///
    /// Used by tests to drive the rolling-window eviction logic without
    /// waiting 30 real-world minutes.
    pub fn with_window(window: Duration) -> Self {
        Self {
            samples: VecDeque::new(),
            window,
        }
    }

    /// Record one `mining.submit` response.
    ///
    /// `accepted = true` for any pool response of `result == true`,
    /// `false` for explicit-error rejects and `result=false` without
    /// payload. Called from every accepted/rejected branch in
    /// `StratumV1Client::handle_submit_response`.
    pub fn record_share(&mut self, accepted: bool) {
        let now = Instant::now();
        self.evict_older_than(now);
        self.samples.push_back((now, accepted));
    }

    /// Current rolling acceptance percentage in `0..=100`.
    ///
    /// Returns `100.0` when no samples remain in the window — see the
    /// module-level note on why "no rolling evidence of rejection" is the
    /// honest baseline. The gate uses this in conjunction with chip-level
    /// HW error tracking and the existing `consecutive_clean_windows`
    /// counter, so an empty window can never spuriously authorize a
    /// step-up on a freshly-booted miner that has never authorized a
    /// share.
    pub fn rolling_acceptance_pct(&mut self) -> f64 {
        let now = Instant::now();
        self.evict_older_than(now);
        if self.samples.is_empty() {
            return 100.0;
        }
        let total = self.samples.len() as f64;
        let accepted = self.samples.iter().filter(|(_, ok)| *ok).count() as f64;
        (accepted / total) * 100.0
    }

    /// Current rolling counts as `(accepted, total)`.
    ///
    /// Read-only accessor for the dashboard and tests. Stays `&self` by
    /// design so dashboards can poll without fighting the share-recording
    /// write path for the lock — eviction proper is still `&mut self`-gated
    /// in `evict_older_than`.
    ///
    /// STRAT-3 (2026-06-20): this used to count EVERY stored sample, which
    /// over-counted on a session that recorded shares and then went quiet
    /// — expired `(timestamp, accepted)` pairs still sat in the deque until
    /// the next `&mut self` call (`record_share` / `rolling_acceptance_pct`)
    /// rolled them off, so a dashboard polling `rolling_count()` in the gap
    /// saw stale acceptance. We now exclude samples older than `window`
    /// from the count without mutating, mirroring the age check in
    /// `evict_older_than`. The reported `(accepted, total)` therefore always
    /// reflects only the live window, regardless of when eviction last ran.
    pub fn rolling_count(&self) -> (u32, u32) {
        let now = Instant::now();
        let in_window = |ts: &Instant| match now.checked_duration_since(*ts) {
            // Future-dated (clock-jump safety) and within-window samples count.
            Some(age) => age <= self.window,
            None => true,
        };
        let mut accepted = 0u32;
        let mut total = 0u32;
        for (ts, ok) in &self.samples {
            if in_window(ts) {
                total += 1;
                if *ok {
                    accepted += 1;
                }
            }
        }
        (accepted, total)
    }

    /// The configured rolling window.
    pub fn window(&self) -> Duration {
        self.window
    }

    fn evict_older_than(&mut self, now: Instant) {
        while let Some(&(ts, _)) = self.samples.front() {
            // `Instant::checked_duration_since` returns `None` when `ts`
            // is in the future (clock-jump safety even though `Instant`
            // is monotonic per process — we still get robustness across
            // pause/resume tests).
            match now.checked_duration_since(ts) {
                Some(age) if age > self.window => {
                    self.samples.pop_front();
                }
                _ => break,
            }
        }
    }
}

impl Default for AcceptanceTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn acceptance_tracker_returns_100_pct_at_clean_baseline() {
        // Pin the "no rolling evidence of rejection" baseline. The gate
        // uses this as one of two ANDed conditions; an empty tracker
        // alone never authorizes a step-up because the autotuner also
        // requires per-chip HW err < 2% AND consecutive_clean_windows >=
        // boost_back_threshold. Returning 100.0 here keeps the gate
        // composition honest: the tracker tells the gate "no recent
        // rejects", not "definitely safe to step up".
        let mut tracker = AcceptanceTracker::new();
        assert!(
            (tracker.rolling_acceptance_pct() - 100.0).abs() < f64::EPSILON,
            "empty window must report 100.0% (no rolling evidence of rejection)"
        );

        // 10 clean accepts -> still 100%.
        for _ in 0..10 {
            tracker.record_share(true);
        }
        assert!(
            (tracker.rolling_acceptance_pct() - 100.0).abs() < f64::EPSILON,
            "10/10 accepts must report 100.0%"
        );
        let (accepted, total) = tracker.rolling_count();
        assert_eq!(accepted, 10);
        assert_eq!(total, 10);
    }

    #[test]
    fn acceptance_tracker_drops_to_50_pct_with_alternating_rejects() {
        // Pin the rolling math under realistic mixed traffic. 50/50 is
        // the canonical "this miner is in trouble" signal that the
        // step-up gate must trip on.
        let mut tracker = AcceptanceTracker::new();
        for i in 0..20 {
            tracker.record_share(i % 2 == 0); // 10 accepts, 10 rejects
        }
        let pct = tracker.rolling_acceptance_pct();
        assert!(
            (pct - 50.0).abs() < f64::EPSILON,
            "10 accepts + 10 rejects must report 50.0%, got {pct}"
        );
        let (accepted, total) = tracker.rolling_count();
        assert_eq!(accepted, 10);
        assert_eq!(total, 20);

        // The autotuner step-up threshold is 99.0%. Pin that 50%
        // strictly fails the gate.
        assert!(
            pct < 99.0,
            "50% acceptance must fail the autotuner 99.0% step-up gate"
        );
    }

    #[test]
    fn acceptance_tracker_rolling_window_evicts_old_samples() {
        // Pin the eviction contract. Use a 200ms window so the test
        // doesn't take forever, but the contract is identical at 30
        // minutes: anything older than `window` rolls off.
        let window = Duration::from_millis(200);
        let mut tracker = AcceptanceTracker::with_window(window);

        // Three rejects ~410ms in the past.
        for _ in 0..3 {
            tracker.record_share(false);
        }
        // Advance well past the window.
        sleep(Duration::from_millis(410));

        // Two clean accepts inside the window.
        tracker.record_share(true);
        tracker.record_share(true);

        let pct = tracker.rolling_acceptance_pct();
        assert!(
            (pct - 100.0).abs() < f64::EPSILON,
            "old rejects must roll off; recent 2/2 accepts must report 100%, got {pct}"
        );
        let (accepted, total) = tracker.rolling_count();
        assert_eq!(
            accepted, 2,
            "old rejects must be evicted from accepted count"
        );
        assert_eq!(total, 2, "old rejects must be evicted from total count");
    }

    #[test]
    fn acceptance_tracker_rolling_count_excludes_expired_samples_without_prior_eviction() {
        // STRAT-3 regression: BEFORE the fix `rolling_count()` counted every
        // stored sample, so a session that recorded shares then went quiet
        // reported stale acceptance until the next `&mut self` call rolled the
        // expired samples off. This test never calls `record_share()` or
        // `rolling_acceptance_pct()` after the window elapses, so the only way
        // (accepted, total) can read as the live window is if `rolling_count()`
        // itself excludes out-of-window samples. Pre-fix: (1, 3). Post-fix: (0, 0).
        let window = Duration::from_millis(150);
        let mut tracker = AcceptanceTracker::with_window(window);

        // One accept + two rejects, all recorded "now".
        tracker.record_share(true);
        tracker.record_share(false);
        tracker.record_share(false);
        assert_eq!(
            tracker.rolling_count(),
            (1, 3),
            "fresh samples must all count inside the window"
        );

        // Let every sample age out of the window. Crucially, do NOT record any
        // new share and do NOT call rolling_acceptance_pct() — nothing mutates
        // the deque, so the stale samples are still physically stored.
        sleep(Duration::from_millis(300));

        assert_eq!(
            tracker.rolling_count(),
            (0, 0),
            "expired samples must be excluded from rolling_count even when no \
             &mut self eviction has run since they aged out"
        );
    }

    #[test]
    fn acceptance_tracker_rolling_count_partial_window_eviction() {
        // Pin that rolling_count() excludes ONLY the expired samples, not the
        // live ones, with no intervening &mut self eviction.
        let window = Duration::from_millis(200);
        let mut tracker = AcceptanceTracker::with_window(window);

        // Two old rejects.
        tracker.record_share(false);
        tracker.record_share(false);
        // Age them out.
        sleep(Duration::from_millis(260));
        // Two fresh accepts (this record_share DOES evict the old two, but we
        // re-prove read-only correctness via the &self accessor below).
        tracker.record_share(true);
        tracker.record_share(true);

        assert_eq!(
            tracker.rolling_count(),
            (2, 2),
            "only the two in-window accepts must count; the two expired rejects \
             must be excluded"
        );
    }

    #[test]
    fn acceptance_tracker_count_accessor_is_read_only() {
        // Pin that `rolling_count()` is `&self` and does not mutate the
        // window. Important because the dashboard reads it on a hot
        // poll loop — making it `&mut self` would deadlock against the
        // share-recording write path.
        let mut tracker = AcceptanceTracker::new();
        tracker.record_share(true);
        tracker.record_share(false);
        let before = tracker.rolling_count();
        let again = tracker.rolling_count();
        assert_eq!(before, again, "rolling_count must be idempotent");
        assert_eq!(before, (1, 2));
    }

    #[test]
    fn acceptance_tracker_empty_window_count_is_zero() {
        let tracker = AcceptanceTracker::new();
        assert_eq!(tracker.rolling_count(), (0, 0));
    }

    #[test]
    fn acceptance_tracker_default_window_is_30_minutes() {
        // A refactor that silently shortened the window would make the
        // step-up gate over-eager (rejection storm forgotten too fast).
        // Pin the constant so a reviewer has to think about it.
        assert_eq!(
            AcceptanceTracker::new().window(),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(DEFAULT_WINDOW, Duration::from_secs(1800));
    }

    #[test]
    fn acceptance_tracker_default_impl_matches_new() {
        let a = AcceptanceTracker::default();
        let b = AcceptanceTracker::new();
        assert_eq!(a.window(), b.window());
        assert_eq!(a.rolling_count(), b.rolling_count());
    }
}
