//! AT-3: process-global publish/consume slot for the quiet-window dsPIC 0x3A
//! measured rail voltage.
//!
//! This is the lock-light shared slot the AT-3 design (DESIGN 1 §1.5) calls for:
//! the am2 hybrid mining loop's gated, default-OFF `rail_timer` arm publishes a
//! freshly-measured per-chain rail voltage here, and the `dcentrald-api`
//! per-chain telemetry projection reads it back so a plausible measured 0x3A
//! reading flips `voltage_source` from `commanded_not_measured` to `measured`.
//!
//! Why it lives here (no-HAL leaf crate): both the daemon binary (`dcentrald`,
//! the producer) and the API (`dcentrald-api`, the consumer) depend on
//! `dcentrald-common`, and this crate is host-testable on Windows. The slot is
//! a plain `std`-only process-global (no external deps, in the same spirit as
//! [`crate::MASK_LOGS_ENABLED`]); it carries no hardware dependency.
//!
//! ## Freshness, not a permanent latch
//!
//! Each entry is timestamped. The consumer reads only entries newer than a TTL
//! (default [`DEFAULT_FRESH_TTL`]). A *miss* (busy bus, misframe, implausible
//! reading) writes nothing — the previous reading simply ages out and the
//! projection degrades cleanly to commanded-tagged, exactly the pre-AT-3
//! behaviour. This is the design's "hold the last good value with a short TTL"
//! failure-handling, implemented as the absence of a write rather than an
//! explicit `None` publish.
//!
//! ## Safety scope (READ-ONLY / measure-only)
//!
//! AT-3 is measure-only. This slot is telemetry; nothing in the daemon makes a
//! *control* decision from it (the closed loop is AT-4+, out of scope). The
//! `fw8a_scale_unverified` flag is carried alongside each reading so a future
//! consumer can keep an fw=0x8A reading advisory (RE-ASK-DSPIC-3A-FW8A-SCALE)
//! and never use it for control. The current telemetry projection consumes only
//! the millivolt value via [`snapshot_fresh`].

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default freshness window for a published AT-3 reading.
///
/// The AT-3 cadence floor is 15 s and its default is 30 s (DESIGN 1 §1.5). A
/// 90 s TTL tolerates two consecutive missed reads at the default cadence
/// before the projection falls back to commanded-tagged — generous enough that
/// a single skipped read never flickers the dashboard, tight enough that a
/// genuinely stale reading is not presented as a live `measured` rail.
pub const DEFAULT_FRESH_TTL: Duration = Duration::from_secs(90);

/// A single published measured rail reading.
#[derive(Debug, Clone, Copy)]
struct RailEntry {
    /// When this reading was published (monotonic).
    at: Instant,
    /// The decoded, plausibility-gated measured rail in millivolts.
    mv: u16,
    /// `true` when the producing dsPIC is fw=0x8A, whose 0x3A ADC scale is not
    /// yet live-verified (RE-ASK-DSPIC-3A-FW8A-SCALE). Advisory only — must
    /// never gate a control decision.
    fw8a_scale_unverified: bool,
}

fn registry() -> &'static Mutex<HashMap<u8, RailEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u8, RailEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Freshness predicate: a reading taken at `at` is fresh as of `now` if it is
/// no older than `ttl`. Pure (host-testable without touching the global slot).
#[inline]
fn is_fresh(at: Instant, now: Instant, ttl: Duration) -> bool {
    now.duration_since(at) <= ttl
}

/// Publish a freshly-measured, plausibility-gated per-chain rail voltage.
///
/// Called by the AT-3 `rail_timer` arm on a successful + plausible 0x3A read.
/// A poisoned lock is swallowed (best-effort: AT-3 never blocks mining).
pub fn publish(chain_id: u8, mv: u16, fw8a_scale_unverified: bool) {
    if let Ok(mut map) = registry().lock() {
        map.insert(
            chain_id,
            RailEntry {
                at: Instant::now(),
                mv,
                fw8a_scale_unverified,
            },
        );
    }
}

/// Snapshot the currently-fresh measured rails (chain id → millivolts) for the
/// telemetry projection. Entries older than `ttl` are excluded so a stale
/// reading is never presented as a live `measured` rail.
///
/// A poisoned lock yields an empty map — the projection then degrades cleanly
/// to the commanded-tagged path (byte-identical to pre-AT-3 behaviour).
pub fn snapshot_fresh(ttl: Duration) -> HashMap<u8, u16> {
    let now = Instant::now();
    match registry().lock() {
        Ok(map) => map
            .iter()
            .filter(|(_, e)| is_fresh(e.at, now, ttl))
            .map(|(id, e)| (*id, e.mv))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

/// [`snapshot_fresh`] with the default TTL ([`DEFAULT_FRESH_TTL`]).
pub fn snapshot_fresh_default() -> HashMap<u8, u16> {
    snapshot_fresh(DEFAULT_FRESH_TTL)
}

/// Snapshot the currently-fresh measured rails with their advisory flag
/// (chain id → (millivolts, fw8a_scale_unverified)). For any future consumer
/// that must keep an fw=0x8A reading advisory; the telemetry projection uses
/// [`snapshot_fresh`] (millivolts only).
pub fn snapshot_fresh_advisory(ttl: Duration) -> HashMap<u8, (u16, bool)> {
    let now = Instant::now();
    match registry().lock() {
        Ok(map) => map
            .iter()
            .filter(|(_, e)| is_fresh(e.at, now, ttl))
            .map(|(id, e)| (*id, (e.mv, e.fw8a_scale_unverified)))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: the slot is a process-global; tests run in parallel. Each test uses
    // a UNIQUE chain id so they never interfere, and they assert only on their
    // own key. No test depends on a global reset.

    #[test]
    fn is_fresh_math_is_inclusive_of_the_ttl_boundary() {
        let base = Instant::now();
        let ttl = Duration::from_secs(2);
        // within ttl -> fresh
        assert!(is_fresh(base, base + Duration::from_secs(1), ttl));
        // exactly at ttl -> still fresh (inclusive)
        assert!(is_fresh(base, base + Duration::from_secs(2), ttl));
        // past ttl -> stale
        assert!(!is_fresh(base, base + Duration::from_secs(3), ttl));
    }

    #[test]
    fn publish_then_snapshot_returns_the_value_under_a_generous_ttl() {
        publish(201, 13_702, false);
        let snap = snapshot_fresh(Duration::from_secs(3600));
        assert_eq!(snap.get(&201).copied(), Some(13_702));
    }

    #[test]
    fn snapshot_default_ttl_sees_a_just_published_reading() {
        publish(202, 13_650, false);
        let snap = snapshot_fresh_default();
        assert_eq!(snap.get(&202).copied(), Some(13_650));
    }

    #[test]
    fn a_stale_reading_is_excluded_from_the_snapshot() {
        // Publish, then read back with a TTL shorter than the elapsed wall time.
        publish(203, 13_700, false);
        std::thread::sleep(Duration::from_millis(25));
        let snap = snapshot_fresh(Duration::from_millis(5));
        assert!(
            snap.get(&203).is_none(),
            "a reading older than the TTL must not be presented as fresh"
        );
        // ...but it is still there under a generous TTL (it was not deleted,
        // just aged out — a subsequent publish refreshes it).
        let fresh = snapshot_fresh(Duration::from_secs(3600));
        assert_eq!(fresh.get(&203).copied(), Some(13_700));
    }

    #[test]
    fn republish_refreshes_the_timestamp_and_value() {
        publish(204, 13_700, false);
        std::thread::sleep(Duration::from_millis(25));
        // Stale under a tight TTL...
        assert!(snapshot_fresh(Duration::from_millis(5)).get(&204).is_none());
        // ...republish makes it fresh again with the new value.
        publish(204, 13_800, false);
        let snap = snapshot_fresh(Duration::from_secs(3600));
        assert_eq!(snap.get(&204).copied(), Some(13_800));
    }

    #[test]
    fn advisory_snapshot_carries_the_fw8a_flag() {
        publish(205, 13_700, true);
        let snap = snapshot_fresh_advisory(Duration::from_secs(3600));
        assert_eq!(snap.get(&205).copied(), Some((13_700, true)));
        // The mv-only snapshot still exposes the value (telemetry path).
        assert_eq!(
            snapshot_fresh(Duration::from_secs(3600)).get(&205).copied(),
            Some(13_700)
        );
    }
}
