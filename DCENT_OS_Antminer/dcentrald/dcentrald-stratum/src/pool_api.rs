//! Fleet pool-stat aggregation primitives.
//!
//! The REST endpoint will live in `dcentrald-api`, but the aggregation contract
//! is protocol-owned and can be tested without HAL or live miners. This module
//! accepts already-fetched per-miner Stratum snapshots and produces a
//! read-only fleet rollup with no pool credentials.

use serde::{Deserialize, Serialize};

use crate::types::StratumStats;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinerPoolSnapshot {
    pub miner_id: String,
    pub host: String,
    pub model: Option<String>,
    pub active_pool_url: String,
    pub connected: bool,
    pub donating: bool,
    /// W5.5: URL of the active donation pool when `donating == true`.
    /// Empty otherwise. Aggregators can use this to break the
    /// `donating_miners` rollup into "donating to primary" vs "donating via
    /// fallback" for fleet-level dashboards.
    #[serde(default)]
    pub donation_active_url: String,
    /// W5.5: Worker name authenticated with the active donation pool. Empty
    /// when not in a donation window.
    #[serde(default)]
    pub donation_active_worker: String,
    /// W5.5: Zero-based donation route index. 0 = primary D-Central, 1 =
    /// visible Braiins fallback worker. Pair with `donating` to interpret.
    #[serde(default)]
    pub donation_pool_index: usize,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_unresolved: u64,
    pub pending_submit_dropped: u64,
    pub jobs_received: u64,
    pub current_difficulty: f64,
    pub failover_switch_count: u64,
    pub last_seen_s: u64,
}

impl MinerPoolSnapshot {
    pub fn from_stratum_stats(
        miner_id: impl Into<String>,
        host: impl Into<String>,
        model: Option<String>,
        stats: &StratumStats,
        last_seen_s: u64,
    ) -> Self {
        Self {
            miner_id: miner_id.into(),
            host: host.into(),
            model,
            active_pool_url: sanitize_pool_url(&stats.failover.active_pool_url),
            connected: stats.connected,
            donating: stats.donating,
            donation_active_url: sanitize_pool_url(&stats.donation_active_url),
            donation_active_worker: stats.donation_active_worker.clone(),
            donation_pool_index: stats.donation_pool_index,
            shares_submitted: stats.shares_submitted,
            shares_accepted: stats.shares_accepted,
            shares_rejected: stats.shares_rejected,
            shares_unresolved: stats.shares_unresolved,
            pending_submit_dropped: stats.pending_submit_dropped,
            jobs_received: stats.jobs_received,
            current_difficulty: stats.current_difficulty,
            failover_switch_count: stats.failover.switch_count,
            last_seen_s,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetPoolStats {
    pub miner_count: usize,
    pub connected_miners: usize,
    pub stale_miners: usize,
    pub donating_miners: usize,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_unresolved: u64,
    pub pending_submit_dropped: u64,
    pub jobs_received: u64,
    pub failover_switches: u64,
    pub acceptance_rate: Option<f64>,
    pub pools: Vec<PoolRollup>,
    pub miners: Vec<MinerPoolSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PoolRollup {
    pub pool_url: String,
    pub miner_count: usize,
    pub connected_miners: usize,
    pub donating_miners: usize,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_unresolved: u64,
    pub jobs_received: u64,
    pub average_difficulty: Option<f64>,
    pub acceptance_rate: Option<f64>,
}

pub fn aggregate_pool_stats(
    snapshots: impl IntoIterator<Item = MinerPoolSnapshot>,
    now_s: u64,
    stale_after_s: u64,
) -> FleetPoolStats {
    let mut miners: Vec<MinerPoolSnapshot> = snapshots
        .into_iter()
        .map(|mut snapshot| {
            snapshot.active_pool_url = sanitize_pool_url(&snapshot.active_pool_url);
            snapshot
        })
        .collect();
    miners.sort_by(|a, b| a.miner_id.cmp(&b.miner_id));

    let mut pools: Vec<PoolRollup> = Vec::new();
    for snapshot in &miners {
        let pool_url = if snapshot.active_pool_url.trim().is_empty() {
            "unconfigured".to_string()
        } else {
            snapshot.active_pool_url.clone()
        };
        let rollup = match pools.iter_mut().find(|pool| pool.pool_url == pool_url) {
            Some(existing) => existing,
            None => {
                let idx = pools.len();
                pools.push(PoolRollup {
                    pool_url,
                    miner_count: 0,
                    connected_miners: 0,
                    donating_miners: 0,
                    shares_submitted: 0,
                    shares_accepted: 0,
                    shares_rejected: 0,
                    shares_unresolved: 0,
                    jobs_received: 0,
                    average_difficulty: None,
                    acceptance_rate: None,
                });
                &mut pools[idx]
            }
        };

        rollup.miner_count += 1;
        rollup.connected_miners += usize::from(snapshot.connected);
        rollup.donating_miners += usize::from(snapshot.donating);
        rollup.shares_submitted = rollup
            .shares_submitted
            .saturating_add(snapshot.shares_submitted);
        rollup.shares_accepted = rollup
            .shares_accepted
            .saturating_add(snapshot.shares_accepted);
        rollup.shares_rejected = rollup
            .shares_rejected
            .saturating_add(snapshot.shares_rejected);
        rollup.shares_unresolved = rollup
            .shares_unresolved
            .saturating_add(snapshot.shares_unresolved);
        rollup.jobs_received = rollup.jobs_received.saturating_add(snapshot.jobs_received);
        rollup.average_difficulty = weighted_average_difficulty(
            rollup.average_difficulty,
            rollup.miner_count - 1,
            snapshot.current_difficulty,
        );
        rollup.acceptance_rate = acceptance_rate(rollup.shares_accepted, rollup.shares_rejected);
    }
    pools.sort_by(|a, b| a.pool_url.cmp(&b.pool_url));

    let connected_miners = miners.iter().filter(|snapshot| snapshot.connected).count();
    let stale_miners = miners
        .iter()
        .filter(|snapshot| snapshot_is_stale(snapshot, now_s, stale_after_s))
        .count();
    let donating_miners = miners.iter().filter(|snapshot| snapshot.donating).count();
    let shares_submitted = miners
        .iter()
        .map(|snapshot| snapshot.shares_submitted)
        .sum();
    let shares_accepted = miners.iter().map(|snapshot| snapshot.shares_accepted).sum();
    let shares_rejected = miners.iter().map(|snapshot| snapshot.shares_rejected).sum();
    let shares_unresolved = miners
        .iter()
        .map(|snapshot| snapshot.shares_unresolved)
        .sum();
    let pending_submit_dropped = miners
        .iter()
        .map(|snapshot| snapshot.pending_submit_dropped)
        .sum();
    let jobs_received = miners.iter().map(|snapshot| snapshot.jobs_received).sum();
    let failover_switches = miners
        .iter()
        .map(|snapshot| snapshot.failover_switch_count)
        .sum();

    FleetPoolStats {
        miner_count: miners.len(),
        connected_miners,
        stale_miners,
        donating_miners,
        shares_submitted,
        shares_accepted,
        shares_rejected,
        shares_unresolved,
        pending_submit_dropped,
        jobs_received,
        failover_switches,
        acceptance_rate: acceptance_rate(shares_accepted, shares_rejected),
        pools,
        miners,
    }
}

fn snapshot_is_stale(snapshot: &MinerPoolSnapshot, now_s: u64, stale_after_s: u64) -> bool {
    if stale_after_s == 0 {
        return false;
    }
    now_s.saturating_sub(snapshot.last_seen_s) > stale_after_s
}

fn weighted_average_difficulty(
    current_average: Option<f64>,
    current_count: usize,
    next: f64,
) -> Option<f64> {
    if !next.is_finite() || next < 0.0 {
        return current_average;
    }
    let Some(current_average) = current_average else {
        return Some(next);
    };
    let count = current_count as f64;
    Some(((current_average * count) + next) / (count + 1.0))
}

fn acceptance_rate(accepted: u64, rejected: u64) -> Option<f64> {
    let replies = accepted.saturating_add(rejected);
    if replies == 0 {
        None
    } else {
        Some(accepted as f64 / replies as f64)
    }
}

pub fn sanitize_pool_url(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return trimmed.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(authority_end);
    let authority = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    format!("{scheme}://{authority}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str, pool: &str, accepted: u64, rejected: u64) -> MinerPoolSnapshot {
        MinerPoolSnapshot {
            miner_id: id.to_string(),
            host: format!("192.0.2.{id}"),
            model: Some("S9".to_string()),
            active_pool_url: pool.to_string(),
            connected: true,
            donating: false,
            donation_active_url: String::new(),
            donation_active_worker: String::new(),
            donation_pool_index: 0,
            shares_submitted: accepted + rejected,
            shares_accepted: accepted,
            shares_rejected: rejected,
            shares_unresolved: 1,
            pending_submit_dropped: 0,
            jobs_received: 10,
            current_difficulty: 2048.0,
            failover_switch_count: 1,
            last_seen_s: 1_000,
        }
    }

    #[test]
    fn from_stratum_stats_propagates_donation_route_fields() {
        // W5.5: the active donation route (URL + worker + index) must
        // round-trip from StratumStats through MinerPoolSnapshot. Pool
        // URLs are sanitized at the boundary so any embedded
        // user:secret@ credentials get stripped before reaching the
        // dashboard.
        let mut stats = StratumStats::default();
        stats.donating = true;
        stats.donation_active_url =
            "stratum+tcp://user:hunter2@stratum.braiins.com:3333".to_string();
        stats.donation_active_worker = "DungeonMaster".to_string();
        stats.donation_pool_index = 1;

        let snap = MinerPoolSnapshot::from_stratum_stats("m", "192.0.2.1", None, &stats, 1_000);

        assert!(snap.donating);
        assert_eq!(
            snap.donation_active_url,
            "stratum+tcp://stratum.braiins.com:3333"
        );
        assert_eq!(snap.donation_active_worker, "DungeonMaster");
        assert_eq!(snap.donation_pool_index, 1);
    }

    #[test]
    fn aggregates_fleet_pool_stats_by_active_pool() {
        let mut donating = snapshot("39", "stratum+tcp://solo.ckpool.org:3333", 9, 1);
        donating.donating = true;
        donating.current_difficulty = 4096.0;
        let mut stale = snapshot("97", "stratum+tcp://solo.ckpool.org:3333", 5, 0);
        stale.connected = false;
        stale.last_seen_s = 900;
        let backup = snapshot("129", "stratum+tcp://backup.pool:4444", 2, 2);

        let stats = aggregate_pool_stats([donating, stale, backup], 1_000, 60);

        assert_eq!(stats.miner_count, 3);
        assert_eq!(stats.connected_miners, 2);
        assert_eq!(stats.stale_miners, 1);
        assert_eq!(stats.donating_miners, 1);
        assert_eq!(stats.shares_accepted, 16);
        assert_eq!(stats.shares_rejected, 3);
        assert_eq!(stats.shares_unresolved, 3);
        assert_eq!(stats.failover_switches, 3);
        assert!((stats.acceptance_rate.unwrap() - (16.0 / 19.0)).abs() < f64::EPSILON);

        let solo = stats
            .pools
            .iter()
            .find(|pool| pool.pool_url == "stratum+tcp://solo.ckpool.org:3333")
            .expect("solo pool rollup");
        assert_eq!(solo.miner_count, 2);
        assert_eq!(solo.connected_miners, 1);
        assert_eq!(solo.donating_miners, 1);
        assert_eq!(solo.shares_accepted, 14);
        assert_eq!(solo.jobs_received, 20);
        assert!(solo.average_difficulty.unwrap() > 2048.0);
    }

    #[test]
    fn sanitizes_pool_credentials_in_inputs_and_rollups() {
        let stats = aggregate_pool_stats(
            [snapshot(
                "39",
                "stratum+tcp://user:secret@pool.example.com:3333",
                1,
                0,
            )],
            10,
            60,
        );

        assert_eq!(
            stats.miners[0].active_pool_url,
            "stratum+tcp://pool.example.com:3333"
        );
        assert_eq!(
            stats.pools[0].pool_url,
            "stratum+tcp://pool.example.com:3333"
        );
    }

    #[test]
    fn empty_fleet_has_null_acceptance_rate() {
        let stats = aggregate_pool_stats([], 10, 60);

        assert_eq!(stats.miner_count, 0);
        assert!(stats.acceptance_rate.is_none());
        assert!(stats.pools.is_empty());
    }

    // -----------------------------------------------------------------------
    // Pure helper contracts.
    //
    // The existing tests cover the aggregation surface but leave the pure
    // helpers untested in isolation. Pin each one so a refactor of the
    // aggregation layer cannot silently regress credential leakage,
    // staleness classification, or the difficulty rollup math.
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_pool_url_strips_user_pass_credentials() {
        assert_eq!(
            sanitize_pool_url("stratum+tcp://user:secret@pool.example.com:3333"),
            "stratum+tcp://pool.example.com:3333"
        );
    }

    #[test]
    fn sanitize_pool_url_strips_user_only_credentials() {
        assert_eq!(
            sanitize_pool_url("stratum+tcp://user@pool.example.com:3333"),
            "stratum+tcp://pool.example.com:3333"
        );
    }

    #[test]
    fn sanitize_pool_url_preserves_path_and_query() {
        // The fleet API may receive URLs with paths from older configs.
        // The sanitizer must strip credentials but preserve everything
        // after the authority so the operator-facing display still
        // matches the configured form.
        assert_eq!(
            sanitize_pool_url("stratum+tcp://user:pass@pool.example.com:3333/path?worker=foo"),
            "stratum+tcp://pool.example.com:3333/path?worker=foo"
        );
    }

    #[test]
    fn sanitize_pool_url_passes_through_clean_url() {
        let clean = "stratum+tcp://pool.example.com:3333";
        assert_eq!(sanitize_pool_url(clean), clean);
    }

    #[test]
    fn sanitize_pool_url_trims_outer_whitespace() {
        assert_eq!(
            sanitize_pool_url("  stratum+tcp://pool.example.com:3333  "),
            "stratum+tcp://pool.example.com:3333"
        );
    }

    #[test]
    fn sanitize_pool_url_handles_no_scheme_gracefully() {
        // Without a `://` separator, the sanitizer just trims and returns —
        // it is NOT a URL parser, just a credential-stripper.
        assert_eq!(sanitize_pool_url("not-a-url"), "not-a-url");
        assert_eq!(
            sanitize_pool_url("user:pass@bare-host"),
            "user:pass@bare-host"
        );
    }

    #[test]
    fn sanitize_pool_url_handles_empty_string() {
        assert_eq!(sanitize_pool_url(""), "");
        assert_eq!(sanitize_pool_url("   "), "");
    }

    #[test]
    fn weighted_average_difficulty_starts_with_first_value() {
        // First sample (current_count=0) must initialize the running average.
        assert_eq!(weighted_average_difficulty(None, 0, 1024.0), Some(1024.0));
    }

    #[test]
    fn weighted_average_difficulty_walks_running_mean_correctly() {
        // Three samples: 1024, 2048, 4096 → mean = 2389.333...
        let avg = weighted_average_difficulty(None, 0, 1024.0).unwrap();
        let avg = weighted_average_difficulty(Some(avg), 1, 2048.0).unwrap();
        let avg = weighted_average_difficulty(Some(avg), 2, 4096.0).unwrap();
        let expected = (1024.0 + 2048.0 + 4096.0) / 3.0;
        assert!(
            (avg - expected).abs() < 1e-9,
            "avg={avg} expected={expected}"
        );
    }

    #[test]
    fn weighted_average_difficulty_skips_nan_and_negative() {
        // Defensive: NaN/Infinity/negative samples must NOT corrupt the
        // running average. A buggy miner reporting current_difficulty=NaN
        // (e.g. before SetDifficulty arrives) would silently break fleet
        // stats without this guard.
        assert_eq!(
            weighted_average_difficulty(Some(1024.0), 1, f64::NAN),
            Some(1024.0)
        );
        assert_eq!(
            weighted_average_difficulty(Some(1024.0), 1, f64::INFINITY),
            Some(1024.0)
        );
        assert_eq!(
            weighted_average_difficulty(Some(1024.0), 1, -1.0),
            Some(1024.0)
        );
    }

    #[test]
    fn weighted_average_difficulty_first_sample_must_also_be_finite() {
        // First sample is NaN/negative — must NOT initialize the average.
        assert_eq!(weighted_average_difficulty(None, 0, f64::NAN), None);
        assert_eq!(weighted_average_difficulty(None, 0, -1.0), None);
    }

    #[test]
    fn acceptance_rate_returns_none_for_zero_replies() {
        // No accepted or rejected shares → no defined acceptance rate.
        // Returning 0.0 instead of None would imply 0% accepted, which
        // is misleading for a miner that simply hasn't submitted yet.
        assert_eq!(acceptance_rate(0, 0), None);
    }

    #[test]
    fn acceptance_rate_returns_one_for_all_accepted() {
        let rate = acceptance_rate(100, 0).unwrap();
        assert!((rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn acceptance_rate_returns_zero_for_all_rejected() {
        let rate = acceptance_rate(0, 100).unwrap();
        assert!(rate.abs() < f64::EPSILON);
    }

    #[test]
    fn acceptance_rate_handles_typical_distribution() {
        let rate = acceptance_rate(95, 5).unwrap();
        assert!((rate - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn acceptance_rate_saturates_on_overflow() {
        // u64::MAX accepted + u64::MAX rejected would overflow add — the
        // saturating_add caps at u64::MAX, keeping the math defined.
        let rate = acceptance_rate(u64::MAX, u64::MAX).unwrap();
        assert!(rate.is_finite());
        assert!((0.0..=1.0).contains(&rate));
    }

    #[test]
    fn snapshot_is_stale_returns_false_when_threshold_is_zero() {
        // stale_after_s=0 disables staleness classification entirely.
        // A fleet operator running with the feature off still gets
        // sensible miner counts.
        let snap = snapshot("39", "stratum+tcp://pool.example.com:3333", 1, 0);
        assert!(!snapshot_is_stale(&snap, 1_000_000, 0));
    }

    #[test]
    fn snapshot_is_stale_classifies_within_window_as_fresh() {
        let mut snap = snapshot("39", "stratum+tcp://pool.example.com:3333", 1, 0);
        snap.last_seen_s = 1_000;
        // 60s after last_seen, threshold 60s → equal, NOT stale (`>` not `>=`).
        assert!(!snapshot_is_stale(&snap, 1_060, 60));
    }

    #[test]
    fn snapshot_is_stale_classifies_past_window_as_stale() {
        let mut snap = snapshot("39", "stratum+tcp://pool.example.com:3333", 1, 0);
        snap.last_seen_s = 1_000;
        // 61s after last_seen, threshold 60s → stale.
        assert!(snapshot_is_stale(&snap, 1_061, 60));
    }

    #[test]
    fn snapshot_is_stale_handles_clock_skew_safely() {
        // last_seen_s ahead of now_s (clock skew between miner and aggregator).
        // saturating_sub clamps at 0, so the snapshot is NOT stale.
        let mut snap = snapshot("39", "stratum+tcp://pool.example.com:3333", 1, 0);
        snap.last_seen_s = 2_000;
        assert!(!snapshot_is_stale(&snap, 1_000, 60));
    }

    #[test]
    fn aggregate_uses_unconfigured_sentinel_for_empty_pool_url() {
        // A miner with no active_pool_url (empty after sanitization) must
        // be bucketed under the "unconfigured" sentinel so the operator
        // can see it instead of having it hide in a real pool's bucket.
        let mut snap = snapshot("39", "", 0, 0);
        snap.connected = false;
        let stats = aggregate_pool_stats([snap], 100, 60);
        assert_eq!(stats.pools.len(), 1);
        assert_eq!(stats.pools[0].pool_url, "unconfigured");
    }

    #[test]
    fn aggregate_sorts_miners_by_miner_id() {
        // Pin the miner sort order so dashboard rows render consistently
        // across refreshes regardless of input order.
        let snaps = [
            snapshot("97", "stratum+tcp://pool.example.com:3333", 1, 0),
            snapshot("39", "stratum+tcp://pool.example.com:3333", 1, 0),
            snapshot("129", "stratum+tcp://pool.example.com:3333", 1, 0),
        ];
        let stats = aggregate_pool_stats(snaps, 100, 60);
        // Lexicographic ordering: "129" < "39" < "97".
        assert_eq!(stats.miners[0].miner_id, "129");
        assert_eq!(stats.miners[1].miner_id, "39");
        assert_eq!(stats.miners[2].miner_id, "97");
    }

    #[test]
    fn aggregate_sorts_pools_by_url() {
        let snaps = [
            snapshot("a", "stratum+tcp://zeta.pool:3333", 1, 0),
            snapshot("b", "stratum+tcp://alpha.pool:3333", 1, 0),
            snapshot("c", "stratum+tcp://mid.pool:3333", 1, 0),
        ];
        let stats = aggregate_pool_stats(snaps, 100, 60);
        assert_eq!(stats.pools[0].pool_url, "stratum+tcp://alpha.pool:3333");
        assert_eq!(stats.pools[1].pool_url, "stratum+tcp://mid.pool:3333");
        assert_eq!(stats.pools[2].pool_url, "stratum+tcp://zeta.pool:3333");
    }

    #[test]
    fn pool_rollup_acceptance_rate_is_computed_per_pool() {
        // Two miners on the same pool with different accept ratios — the
        // pool rollup must reflect the combined rate, not just the first
        // miner's.
        let mut a = snapshot("a", "stratum+tcp://pool.example.com:3333", 90, 10);
        a.current_difficulty = 1024.0;
        let mut b = snapshot("b", "stratum+tcp://pool.example.com:3333", 80, 20);
        b.current_difficulty = 2048.0;
        let stats = aggregate_pool_stats([a, b], 100, 60);
        assert_eq!(stats.pools.len(), 1);
        let rate = stats.pools[0].acceptance_rate.unwrap();
        // (90+80) / (90+80+10+20) = 170/200 = 0.85
        assert!((rate - 0.85).abs() < f64::EPSILON);
    }
}
