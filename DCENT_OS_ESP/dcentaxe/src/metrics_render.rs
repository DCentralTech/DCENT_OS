// DCENT_axe — Prometheus /metrics body renderer (host-pure)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Prometheus text-exposition (`/metrics`) body builder — the host-pure,
//! unit-tested CORE.
//!
//! The `/metrics` body used to be assembled INLINE inside the esp-idf-gated
//! `register_prometheus` HTTP handler in `dcentaxe/src/api.rs`, which made it the
//! only major telemetry surface with ZERO host tests. This module extracts the
//! WHOLE body into a pure function over plain input structs (no esp-idf, no
//! network, no locks) so it host-compiles and unit-tests under
//! `cargo test -p dcentaxe-core` (re-included via `#[path]` in
//! `dcentaxe-core/src/lib.rs`, the same single-source-of-truth pattern used by
//! `mqtt_ha.rs` / `chip_profiles_bitaxe.rs`). The esp-idf transport
//! (`api::register_prometheus`) stays thin: it gathers the live values into these
//! plain structs, calls [`render_metrics_body`], and writes the returned String
//! out with the existing HTTP wiring/headers.
//!
//! Behavior contract: every metric family that the inline body emitted before the
//! extraction renders BYTE-IDENTICAL here (the core block is the verbatim
//! `format!` literal, and the per-pool sections keep their exact text + gating).
//! Two NEW, purely-additive families were added on top:
//!   * `dcentaxe_pool_shares_rejected_by_reason` — the per-reason share-reject
//!     breakdown (B2), already shipped over JSON/MCP but never as a labeled
//!     Prometheus counter.
//!   * `dcentaxe_last_{accepted,response,rejected}_share_timestamp_seconds` — the
//!     share-freshness gauges (B3) that make the headline mining-liveness alert
//!     `time() - dcentaxe_last_accepted_share_timestamp_seconds > N` buildable.
//!
//! SECURITY: this body NEVER emits an operator worker address or a pool URL (the
//! per-pool labels are numeric indices; the only free-text label is the
//! pool-provided reject reason, which carries no credential and is already public
//! over JSON/MCP). Every label value is run through [`escape_label_value`] so a
//! reject reason containing `"`, `\`, or a newline cannot break the exposition
//! format.

/// Escape a Prometheus label value per the text-exposition spec.
///
/// Ported from the Toolbox exporter `_escape_label_value`
/// (`dcent-toolbox/src/dcent_toolbox/core/serve/metrics.py`): backslash → `\\`,
/// double-quote → `\"`, newline → `\n`. Order matters — backslash MUST be escaped
/// first so the backslashes introduced by the quote/newline escapes are not
/// double-escaped.
pub fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Flat, esp-idf-free view of every scalar the core `/metrics` block emits.
///
/// Field types match the live sources EXACTLY (snapshot hashrates/difficulties are
/// `f64`, telemetry temps/power are `f32`, counters are `u64`, …) so that each
/// value's `Display` output is byte-identical to the pre-extraction inline body.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    // ── hashrate (GH/s, rolling windows) ──
    pub hashrate_1m_ghs: f64,
    pub hashrate_5m_ghs: f64,
    pub hashrate_15m_ghs: f64,
    // ── local-validation share counters ──
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    // ── stratum aggregate pending/unresolved ──
    pub stratum_shares_pending: u64,
    pub stratum_shares_unresolved: u64,
    pub oldest_pending_submit_age_ms: u64,
    // ── dispatcher counters ──
    pub stale_nonces: u64,
    pub slot_recoveries: u64,
    pub filtered_shares: u64,
    pub ticket_difficulty: f64,
    pub best_difficulty: f64,
    // ── temperatures (°C) ──
    pub chip_temp_c: f32,
    pub board_temp_c: f32,
    pub vreg_temp_c: f32,
    pub inlet_temp_c: f32,
    pub outlet_temp_c: f32,
    pub chip_temp_min_c: f32,
    pub chip_temp_max_c: f32,
    pub chip_temp_spread_c: f32,
    pub max_temperature_c: f32,
    pub air_delta_c: f32,
    // ── power / electrical ──
    pub power_w: f32,
    pub current_ma: f32,
    pub voltage_mv: f32,
    pub input_voltage_mv: f32,
    pub max_power_w: f32,
    pub max_current_a: f32,
    pub target_frequency: f32,
    // ── fans ──
    pub fan_speed_pct: u8,
    pub fan_rpm: u32,
    pub fan2_rpm: u32,
    // ── flags / misc ──
    pub sensors_ok: bool,
    pub mining_enabled: bool,
    pub uptime_secs: u64,
    pub free_heap: u32,
    pub achievement_count: u32,
    pub lifetime_shares: u32,
}

/// One per-pool pending/unresolved row (the existing conditional stratum section).
/// Types mirror `shared::StratumMetricsSnapshot`.
#[derive(Debug, Clone, Default)]
pub struct PoolPendingRow {
    pub pool_index: u8,
    pub shares_pending: u32,
    pub shares_unresolved: u64,
}

/// One per-pool split-mining row (the existing conditional `pool_stats` section,
/// emitted only when more than one pool is present). Types mirror
/// `dcentaxe_mining::stats::PoolStatsSnapshot`.
#[derive(Debug, Clone, Default)]
pub struct PoolSplitRow {
    pub index: u8,
    pub target_pct: u8,
    pub dispatched_count: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub connected: bool,
}

/// Pool-truth view powering the two NEW families (B2 + B3). Sourced from the
/// active `StratumStatus` (connected pool, else the primary), exactly like the
/// JSON `/api/system/info` `poolTruth` block and the MCP `get_status` surface.
#[derive(Debug, Clone, Default)]
pub struct ShareTruthView {
    /// Per-reason reject tally: `(reason key, count)`. The key is escaped before
    /// it is emitted as a label value.
    pub reject_reason_counts: Vec<(String, u64)>,
    /// Unix-ms of the last pool share response (0 == none yet / "never").
    pub last_share_response_unix_ms: u64,
    /// Unix-ms of the last pool-accepted share (0 == none yet / "never").
    pub last_share_accepted_unix_ms: u64,
    /// Unix-ms of the last pool-rejected share (0 == none yet / "never").
    pub last_share_rejected_unix_ms: u64,
}

/// B3 helper: append one share-freshness gauge family.
///
/// "Never" convention: the SAMPLE is OMITTED when `unix_ms == 0` (no such share
/// has occurred yet), so a `time() - <gauge>` alert never reads a bogus 1970
/// epoch — the series simply has no data until the first such share. The
/// `# HELP`/`# TYPE` header is always emitted so the metric is self-describing and
/// dashboards see it exists.
fn push_freshness_gauge(body: &mut String, name: &str, help: &str, unix_ms: u64) {
    body.push_str(&format!("# HELP {name} {help}\n"));
    body.push_str(&format!("# TYPE {name} gauge\n"));
    if unix_ms > 0 {
        // *_unix_ms / 1000.0 -> seconds, matching the JSON freshness fields.
        let seconds = unix_ms as f64 / 1000.0;
        body.push_str(&format!("{name} {seconds}\n"));
    }
}

/// Render the COMPLETE Prometheus `/metrics` body from plain inputs.
///
/// Output layout (in order):
///   1. the verbatim core block (every pre-extraction metric family),
///   2. the per-pool stratum pending/unresolved section (only when `pool_pending`
///      is non-empty — exact same gating as the inline body),
///   3. the per-pool split-mining section (only when more than one
///      `pool_split` row exists — exact same `> 1` gating),
///   4. B3 share-freshness gauges (new, additive),
///   5. B2 per-reason share-reject breakdown (new, additive).
pub fn render_metrics_body(
    s: &MetricsSnapshot,
    pool_pending: &[PoolPendingRow],
    pool_split: &[PoolSplitRow],
    share_truth: &ShareTruthView,
) -> String {
    // ── (1) Core block — VERBATIM copy of the former inline `format!` literal.
    // Rust string-continuation (`\` + newline) strips the leading whitespace on
    // each continued line, so the emitted bytes are independent of this source
    // indentation and remain byte-identical to the pre-extraction body.
    let mut body = format!(
        "# HELP dcentaxe_hashrate_ghs Current hashrate in GH/s\n\
             # TYPE dcentaxe_hashrate_ghs gauge\n\
             dcentaxe_hashrate_ghs{{window=\"1m\"}} {}\n\
             dcentaxe_hashrate_ghs{{window=\"5m\"}} {}\n\
             dcentaxe_hashrate_ghs{{window=\"15m\"}} {}\n\
             # HELP dcentaxe_shares_accepted_total Local dispatcher share candidates accepted by local validation; pool-confirmed counters are exported as dcentaxe_pool_shares_accepted/rejected\n\
             # TYPE dcentaxe_shares_accepted_total counter\n\
             dcentaxe_shares_accepted_total {}\n\
             # HELP dcentaxe_shares_rejected_total Local dispatcher share candidates rejected by local validation; pool-confirmed counters are exported as dcentaxe_pool_shares_accepted/rejected\n\
             # TYPE dcentaxe_shares_rejected_total counter\n\
             dcentaxe_shares_rejected_total {}\n\
             # HELP dcentaxe_stratum_shares_pending Shares submitted to pools without a final response\n\
             # TYPE dcentaxe_stratum_shares_pending gauge\n\
             dcentaxe_stratum_shares_pending {}\n\
             # HELP dcentaxe_stratum_shares_unresolved_total Submitted shares whose final pool response was not tracked\n\
             # TYPE dcentaxe_stratum_shares_unresolved_total counter\n\
             dcentaxe_stratum_shares_unresolved_total {}\n\
             # HELP dcentaxe_stratum_oldest_pending_submit_age_ms Age of the oldest pending share submission\n\
             # TYPE dcentaxe_stratum_oldest_pending_submit_age_ms gauge\n\
             dcentaxe_stratum_oldest_pending_submit_age_ms {}\n\
             # HELP dcentaxe_dispatcher_stale_nonces_total Nonces dropped after job slot aliasing or stale work\n\
             # TYPE dcentaxe_dispatcher_stale_nonces_total counter\n\
             dcentaxe_dispatcher_stale_nonces_total {}\n\
             # HELP dcentaxe_dispatcher_slot_recoveries_total Nonces recovered by validating alternate active job slots\n\
             # TYPE dcentaxe_dispatcher_slot_recoveries_total counter\n\
             dcentaxe_dispatcher_slot_recoveries_total {}\n\
             # HELP dcentaxe_dispatcher_filtered_nonces_total Valid ASIC-difficulty nonces below pool difficulty\n\
             # TYPE dcentaxe_dispatcher_filtered_nonces_total counter\n\
             dcentaxe_dispatcher_filtered_nonces_total {}\n\
             # HELP dcentaxe_dispatcher_ticket_difficulty ASIC ticket difficulty used for local nonce validation\n\
             # TYPE dcentaxe_dispatcher_ticket_difficulty gauge\n\
             dcentaxe_dispatcher_ticket_difficulty {}\n\
             # HELP dcentaxe_best_difficulty Best local candidate share difficulty\n\
             # TYPE dcentaxe_best_difficulty gauge\n\
             dcentaxe_best_difficulty {}\n\
             # HELP dcentaxe_temperature_celsius Temperature readings\n\
             # TYPE dcentaxe_temperature_celsius gauge\n\
             dcentaxe_temperature_celsius{{sensor=\"chip\"}} {}\n\
             dcentaxe_temperature_celsius{{sensor=\"board\"}} {}\n\
             dcentaxe_temperature_celsius{{sensor=\"vreg\"}} {}\n\
             dcentaxe_temperature_celsius{{sensor=\"inlet\"}} {}\n\
             dcentaxe_temperature_celsius{{sensor=\"outlet\"}} {}\n\
             # HELP dcentaxe_chip_temperature_summary_celsius Per-chip temperature summary for soak validation\n\
             # TYPE dcentaxe_chip_temperature_summary_celsius gauge\n\
             dcentaxe_chip_temperature_summary_celsius{{stat=\"min\"}} {}\n\
             dcentaxe_chip_temperature_summary_celsius{{stat=\"max\"}} {}\n\
             dcentaxe_chip_temperature_summary_celsius{{stat=\"spread\"}} {}\n\
             # HELP dcentaxe_temperature_max_celsius Maximum observed temperature across reported sensors\n\
             # TYPE dcentaxe_temperature_max_celsius gauge\n\
             dcentaxe_temperature_max_celsius {}\n\
             # HELP dcentaxe_air_temperature_delta_celsius Outlet minus inlet temperature\n\
             # TYPE dcentaxe_air_temperature_delta_celsius gauge\n\
             dcentaxe_air_temperature_delta_celsius {}\n\
             # HELP dcentaxe_power_watts Power consumption\n\
             # TYPE dcentaxe_power_watts gauge\n\
             dcentaxe_power_watts {}\n\
             # HELP dcentaxe_current_ma ASIC rail current in milliamps\n\
             # TYPE dcentaxe_current_ma gauge\n\
             dcentaxe_current_ma {}\n\
             # HELP dcentaxe_voltage_mv Core voltage in millivolts\n\
             # TYPE dcentaxe_voltage_mv gauge\n\
             dcentaxe_voltage_mv {}\n\
             # HELP dcentaxe_input_voltage_mv Input voltage in millivolts\n\
             # TYPE dcentaxe_input_voltage_mv gauge\n\
             dcentaxe_input_voltage_mv {}\n\
             # HELP dcentaxe_power_limit_watts Configured board power limit\n\
             # TYPE dcentaxe_power_limit_watts gauge\n\
             dcentaxe_power_limit_watts {}\n\
             # HELP dcentaxe_current_limit_amps Configured board current limit\n\
             # TYPE dcentaxe_current_limit_amps gauge\n\
             dcentaxe_current_limit_amps {}\n\
             # HELP dcentaxe_frequency_mhz ASIC frequency\n\
             # TYPE dcentaxe_frequency_mhz gauge\n\
             dcentaxe_frequency_mhz {}\n\
             # HELP dcentaxe_fan_speed_pct Fan speed percentage\n\
             # TYPE dcentaxe_fan_speed_pct gauge\n\
             dcentaxe_fan_speed_pct {}\n\
             # HELP dcentaxe_fan_rpm Fan tachometer readings\n\
             # TYPE dcentaxe_fan_rpm gauge\n\
             dcentaxe_fan_rpm{{fan=\"1\"}} {}\n\
             dcentaxe_fan_rpm{{fan=\"2\"}} {}\n\
             # HELP dcentaxe_sensors_ok Temperature sensor validity\n\
             # TYPE dcentaxe_sensors_ok gauge\n\
             dcentaxe_sensors_ok {}\n\
             # HELP dcentaxe_thermal_sensors_ok Aggregate temperature sensor validity\n\
             # TYPE dcentaxe_thermal_sensors_ok gauge\n\
             dcentaxe_thermal_sensors_ok {}\n\
             # HELP dcentaxe_mining_enabled Mining runtime enable state\n\
             # TYPE dcentaxe_mining_enabled gauge\n\
             dcentaxe_mining_enabled {}\n\
             # HELP dcentaxe_uptime_seconds Device uptime\n\
             # TYPE dcentaxe_uptime_seconds counter\n\
             dcentaxe_uptime_seconds {}\n\
             # HELP dcentaxe_free_heap_bytes Free heap memory\n\
             # TYPE dcentaxe_free_heap_bytes gauge\n\
             dcentaxe_free_heap_bytes {}\n\
             # HELP dcentaxe_achievements_unlocked Achievement count\n\
             # TYPE dcentaxe_achievements_unlocked gauge\n\
             dcentaxe_achievements_unlocked {}\n\
             # HELP dcentaxe_lifetime_shares Lifetime shares across reboots\n\
             # TYPE dcentaxe_lifetime_shares counter\n\
             dcentaxe_lifetime_shares {}\n",
        s.hashrate_1m_ghs,
        s.hashrate_5m_ghs,
        s.hashrate_15m_ghs,
        s.accepted_shares,
        s.rejected_shares,
        s.stratum_shares_pending,
        s.stratum_shares_unresolved,
        s.oldest_pending_submit_age_ms,
        s.stale_nonces,
        s.slot_recoveries,
        s.filtered_shares,
        s.ticket_difficulty,
        s.best_difficulty,
        s.chip_temp_c,
        s.board_temp_c,
        s.vreg_temp_c,
        s.inlet_temp_c,
        s.outlet_temp_c,
        s.chip_temp_min_c,
        s.chip_temp_max_c,
        s.chip_temp_spread_c,
        s.max_temperature_c,
        s.air_delta_c,
        s.power_w,
        s.current_ma,
        s.voltage_mv,
        s.input_voltage_mv,
        s.max_power_w,
        s.max_current_a,
        s.target_frequency,
        s.fan_speed_pct,
        s.fan_rpm,
        s.fan2_rpm,
        if s.sensors_ok { 1 } else { 0 },
        if s.sensors_ok { 1 } else { 0 },
        if s.mining_enabled { 1 } else { 0 },
        s.uptime_secs,
        s.free_heap,
        s.achievement_count,
        s.lifetime_shares,
    );

    // ── (2) Per-pool stratum pending/unresolved — same text + gating as inline.
    if !pool_pending.is_empty() {
        body.push_str(
            "# HELP dcentaxe_stratum_pool_shares_pending Per-pool pending submitted shares\n",
        );
        body.push_str("# TYPE dcentaxe_stratum_pool_shares_pending gauge\n");
        for row in pool_pending {
            body.push_str(&format!(
                "dcentaxe_stratum_pool_shares_pending{{pool=\"{}\"}} {}\n",
                row.pool_index, row.shares_pending
            ));
        }
        body.push_str(
            "# HELP dcentaxe_stratum_pool_shares_unresolved_total Per-pool submitted shares with no tracked final pool response\n",
        );
        body.push_str("# TYPE dcentaxe_stratum_pool_shares_unresolved_total counter\n");
        for row in pool_pending {
            body.push_str(&format!(
                "dcentaxe_stratum_pool_shares_unresolved_total{{pool=\"{}\"}} {}\n",
                row.pool_index, row.shares_unresolved
            ));
        }
    }

    // ── (3) Per-pool split-mining — same text + `> 1` gating as inline.
    if pool_split.len() > 1 {
        let total_dispatched: u64 = pool_split.iter().map(|p| p.dispatched_count).sum();
        let mut pool_body = String::new();
        pool_body.push_str("# HELP dcentaxe_pool_shares_accepted Per-pool accepted shares\n");
        pool_body.push_str("# TYPE dcentaxe_pool_shares_accepted counter\n");
        for p in pool_split {
            pool_body.push_str(&format!(
                "dcentaxe_pool_shares_accepted{{pool=\"{}\"}} {}\n",
                p.index, p.shares_accepted
            ));
        }
        pool_body.push_str("# HELP dcentaxe_pool_shares_rejected Per-pool rejected shares\n");
        pool_body.push_str("# TYPE dcentaxe_pool_shares_rejected counter\n");
        for p in pool_split {
            pool_body.push_str(&format!(
                "dcentaxe_pool_shares_rejected{{pool=\"{}\"}} {}\n",
                p.index, p.shares_rejected
            ));
        }
        pool_body
            .push_str("# HELP dcentaxe_pool_hashrate_pct Per-pool actual hashrate percentage\n");
        pool_body.push_str("# TYPE dcentaxe_pool_hashrate_pct gauge\n");
        for p in pool_split {
            // Replicates PoolStatsSnapshot::actual_pct: 0.0 when no dispatch yet.
            let actual_pct = if total_dispatched == 0 {
                0.0
            } else {
                (p.dispatched_count as f64 / total_dispatched as f64) * 100.0
            };
            pool_body.push_str(&format!(
                "dcentaxe_pool_hashrate_pct{{pool=\"{}\",target=\"{}\"}} {:.1}\n",
                p.index, p.target_pct, actual_pct
            ));
        }
        pool_body.push_str("# HELP dcentaxe_pool_connected Per-pool connection status\n");
        pool_body.push_str("# TYPE dcentaxe_pool_connected gauge\n");
        for p in pool_split {
            pool_body.push_str(&format!(
                "dcentaxe_pool_connected{{pool=\"{}\"}} {}\n",
                p.index,
                if p.connected { 1 } else { 0 }
            ));
        }
        body.push_str(&pool_body);
    }

    // ── (4) B3: share-freshness timestamp gauges (NEW, additive). ──
    // Sample omitted when the source *_unix_ms is 0 ("never") — see
    // [`push_freshness_gauge`].
    push_freshness_gauge(
        &mut body,
        "dcentaxe_last_accepted_share_timestamp_seconds",
        "Unix time (seconds) of the last pool-accepted share. Sample omitted until the first accepted share, so time()-gauge alerts never read a bogus 1970 epoch.",
        share_truth.last_share_accepted_unix_ms,
    );
    push_freshness_gauge(
        &mut body,
        "dcentaxe_last_share_response_timestamp_seconds",
        "Unix time (seconds) of the last pool share response (accept or reject). Sample omitted until the first response.",
        share_truth.last_share_response_unix_ms,
    );
    push_freshness_gauge(
        &mut body,
        "dcentaxe_last_share_rejected_timestamp_seconds",
        "Unix time (seconds) of the last pool-rejected share. Sample omitted until the first rejected share.",
        share_truth.last_share_rejected_unix_ms,
    );

    // ── (5) B2: per-reason share-reject breakdown (NEW, additive). ──
    // Single HELP/TYPE header, one sample per reason row. The free-text reason
    // key is escaped so a `"`/`\`/newline can't break the exposition line.
    body.push_str(
        "# HELP dcentaxe_pool_shares_rejected_by_reason Shares rejected by the pool, broken down by reason.\n",
    );
    body.push_str("# TYPE dcentaxe_pool_shares_rejected_by_reason counter\n");
    for (reason, count) in &share_truth.reject_reason_counts {
        body.push_str(&format!(
            "dcentaxe_pool_shares_rejected_by_reason{{reason=\"{}\"}} {}\n",
            escape_label_value(reason),
            count
        ));
    }

    // ES-4: Prometheus rejects `inf`/`-inf` as sample values (the exposition
    // format requires `+Inf`/`-Inf`; `NaN` is already valid). Rust's `{}` renders
    // a non-finite float as `inf`/`-inf`, so one non-finite reading (e.g. a
    // divide-by-zero in a derived metric) would make the WHOLE scrape unparseable.
    // Every metric value is rendered as ` <value>\n`, so coercing that exact
    // pattern is safe: it never matches a label value (inside `{}`) or HELP text.
    body.replace(" inf\n", " +Inf\n")
        .replace(" -inf\n", " -Inf\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative, fully-populated snapshot. Values are chosen to be exactly
    /// representable so their `Display` form is deterministic across f32/f64.
    fn busy_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            hashrate_1m_ghs: 100.5,
            hashrate_5m_ghs: 95.25,
            hashrate_15m_ghs: 90.0,
            accepted_shares: 1234,
            rejected_shares: 5,
            stratum_shares_pending: 2,
            stratum_shares_unresolved: 1,
            oldest_pending_submit_age_ms: 4200,
            stale_nonces: 7,
            slot_recoveries: 3,
            filtered_shares: 11,
            ticket_difficulty: 256.0,
            best_difficulty: 65536.5,
            chip_temp_c: 61.5,
            board_temp_c: 45.0,
            vreg_temp_c: 50.25,
            inlet_temp_c: 24.0,
            outlet_temp_c: 30.0,
            chip_temp_min_c: 60.0,
            chip_temp_max_c: 63.0,
            chip_temp_spread_c: 3.0,
            max_temperature_c: 63.0,
            air_delta_c: 6.0,
            power_w: 18.5,
            current_ma: 9000.0,
            voltage_mv: 1200.0,
            input_voltage_mv: 5000.0,
            max_power_w: 25.0,
            max_current_a: 12.0,
            target_frequency: 525.0,
            fan_speed_pct: 30,
            fan_rpm: 2880,
            fan2_rpm: 0,
            sensors_ok: true,
            mining_enabled: true,
            uptime_secs: 3600,
            free_heap: 123456,
            achievement_count: 4,
            lifetime_shares: 9999,
        }
    }

    /// Assert a full line (terminated by `\n`) is present verbatim in `body`.
    fn assert_line(body: &str, line: &str) {
        assert!(
            body.contains(&format!("{line}\n")),
            "missing/changed exposition line: {line:?}"
        );
    }

    // ── GOLDEN: every pre-extraction family renders BYTE-IDENTICAL ────────────
    // Each `# HELP`/`# TYPE`/sample line of every existing family is asserted
    // verbatim (with its trailing newline so a value can't be a prefix of a
    // longer one). The populated input also carries the two conditional per-pool
    // sections so their exact text + gating is pinned too.
    #[test]
    fn golden_existing_families_are_byte_identical() {
        let snap = busy_snapshot();
        let pool_pending = vec![
            PoolPendingRow {
                pool_index: 0,
                shares_pending: 2,
                shares_unresolved: 1,
            },
            PoolPendingRow {
                pool_index: 1,
                shares_pending: 0,
                shares_unresolved: 0,
            },
        ];
        let pool_split = vec![
            PoolSplitRow {
                index: 0,
                target_pct: 70,
                dispatched_count: 70,
                shares_accepted: 100,
                shares_rejected: 1,
                connected: true,
            },
            PoolSplitRow {
                index: 1,
                target_pct: 30,
                dispatched_count: 30,
                shares_accepted: 40,
                shares_rejected: 0,
                connected: false,
            },
        ];
        let truth = ShareTruthView::default();
        let body = render_metrics_body(&snap, &pool_pending, &pool_split, &truth);

        // (a) the body STARTS with the first core family header (core block first).
        assert!(
            body.starts_with("# HELP dcentaxe_hashrate_ghs Current hashrate in GH/s\n"),
            "core block must lead the body"
        );

        // (b) every core family's HELP/TYPE/sample line, verbatim.
        for line in [
            "# HELP dcentaxe_hashrate_ghs Current hashrate in GH/s",
            "# TYPE dcentaxe_hashrate_ghs gauge",
            "dcentaxe_hashrate_ghs{window=\"1m\"} 100.5",
            "dcentaxe_hashrate_ghs{window=\"5m\"} 95.25",
            "dcentaxe_hashrate_ghs{window=\"15m\"} 90",
            "# HELP dcentaxe_shares_accepted_total Local dispatcher share candidates accepted by local validation; pool-confirmed counters are exported as dcentaxe_pool_shares_accepted/rejected",
            "# TYPE dcentaxe_shares_accepted_total counter",
            "dcentaxe_shares_accepted_total 1234",
            "# HELP dcentaxe_shares_rejected_total Local dispatcher share candidates rejected by local validation; pool-confirmed counters are exported as dcentaxe_pool_shares_accepted/rejected",
            "# TYPE dcentaxe_shares_rejected_total counter",
            "dcentaxe_shares_rejected_total 5",
            "# HELP dcentaxe_stratum_shares_pending Shares submitted to pools without a final response",
            "# TYPE dcentaxe_stratum_shares_pending gauge",
            "dcentaxe_stratum_shares_pending 2",
            "# HELP dcentaxe_stratum_shares_unresolved_total Submitted shares whose final pool response was not tracked",
            "# TYPE dcentaxe_stratum_shares_unresolved_total counter",
            "dcentaxe_stratum_shares_unresolved_total 1",
            "# HELP dcentaxe_stratum_oldest_pending_submit_age_ms Age of the oldest pending share submission",
            "# TYPE dcentaxe_stratum_oldest_pending_submit_age_ms gauge",
            "dcentaxe_stratum_oldest_pending_submit_age_ms 4200",
            "# HELP dcentaxe_dispatcher_stale_nonces_total Nonces dropped after job slot aliasing or stale work",
            "# TYPE dcentaxe_dispatcher_stale_nonces_total counter",
            "dcentaxe_dispatcher_stale_nonces_total 7",
            "# HELP dcentaxe_dispatcher_slot_recoveries_total Nonces recovered by validating alternate active job slots",
            "# TYPE dcentaxe_dispatcher_slot_recoveries_total counter",
            "dcentaxe_dispatcher_slot_recoveries_total 3",
            "# HELP dcentaxe_dispatcher_filtered_nonces_total Valid ASIC-difficulty nonces below pool difficulty",
            "# TYPE dcentaxe_dispatcher_filtered_nonces_total counter",
            "dcentaxe_dispatcher_filtered_nonces_total 11",
            "# HELP dcentaxe_dispatcher_ticket_difficulty ASIC ticket difficulty used for local nonce validation",
            "# TYPE dcentaxe_dispatcher_ticket_difficulty gauge",
            "dcentaxe_dispatcher_ticket_difficulty 256",
            "# HELP dcentaxe_best_difficulty Best local candidate share difficulty",
            "# TYPE dcentaxe_best_difficulty gauge",
            "dcentaxe_best_difficulty 65536.5",
            "# HELP dcentaxe_temperature_celsius Temperature readings",
            "# TYPE dcentaxe_temperature_celsius gauge",
            "dcentaxe_temperature_celsius{sensor=\"chip\"} 61.5",
            "dcentaxe_temperature_celsius{sensor=\"board\"} 45",
            "dcentaxe_temperature_celsius{sensor=\"vreg\"} 50.25",
            "dcentaxe_temperature_celsius{sensor=\"inlet\"} 24",
            "dcentaxe_temperature_celsius{sensor=\"outlet\"} 30",
            "# HELP dcentaxe_chip_temperature_summary_celsius Per-chip temperature summary for soak validation",
            "# TYPE dcentaxe_chip_temperature_summary_celsius gauge",
            "dcentaxe_chip_temperature_summary_celsius{stat=\"min\"} 60",
            "dcentaxe_chip_temperature_summary_celsius{stat=\"max\"} 63",
            "dcentaxe_chip_temperature_summary_celsius{stat=\"spread\"} 3",
            "# HELP dcentaxe_temperature_max_celsius Maximum observed temperature across reported sensors",
            "# TYPE dcentaxe_temperature_max_celsius gauge",
            "dcentaxe_temperature_max_celsius 63",
            "# HELP dcentaxe_air_temperature_delta_celsius Outlet minus inlet temperature",
            "# TYPE dcentaxe_air_temperature_delta_celsius gauge",
            "dcentaxe_air_temperature_delta_celsius 6",
            "# HELP dcentaxe_power_watts Power consumption",
            "# TYPE dcentaxe_power_watts gauge",
            "dcentaxe_power_watts 18.5",
            "# HELP dcentaxe_current_ma ASIC rail current in milliamps",
            "# TYPE dcentaxe_current_ma gauge",
            "dcentaxe_current_ma 9000",
            "# HELP dcentaxe_voltage_mv Core voltage in millivolts",
            "# TYPE dcentaxe_voltage_mv gauge",
            "dcentaxe_voltage_mv 1200",
            "# HELP dcentaxe_input_voltage_mv Input voltage in millivolts",
            "# TYPE dcentaxe_input_voltage_mv gauge",
            "dcentaxe_input_voltage_mv 5000",
            "# HELP dcentaxe_power_limit_watts Configured board power limit",
            "# TYPE dcentaxe_power_limit_watts gauge",
            "dcentaxe_power_limit_watts 25",
            "# HELP dcentaxe_current_limit_amps Configured board current limit",
            "# TYPE dcentaxe_current_limit_amps gauge",
            "dcentaxe_current_limit_amps 12",
            "# HELP dcentaxe_frequency_mhz ASIC frequency",
            "# TYPE dcentaxe_frequency_mhz gauge",
            "dcentaxe_frequency_mhz 525",
            "# HELP dcentaxe_fan_speed_pct Fan speed percentage",
            "# TYPE dcentaxe_fan_speed_pct gauge",
            "dcentaxe_fan_speed_pct 30",
            "# HELP dcentaxe_fan_rpm Fan tachometer readings",
            "# TYPE dcentaxe_fan_rpm gauge",
            "dcentaxe_fan_rpm{fan=\"1\"} 2880",
            "dcentaxe_fan_rpm{fan=\"2\"} 0",
            "# HELP dcentaxe_sensors_ok Temperature sensor validity",
            "# TYPE dcentaxe_sensors_ok gauge",
            "dcentaxe_sensors_ok 1",
            "# HELP dcentaxe_thermal_sensors_ok Aggregate temperature sensor validity",
            "# TYPE dcentaxe_thermal_sensors_ok gauge",
            "dcentaxe_thermal_sensors_ok 1",
            "# HELP dcentaxe_mining_enabled Mining runtime enable state",
            "# TYPE dcentaxe_mining_enabled gauge",
            "dcentaxe_mining_enabled 1",
            "# HELP dcentaxe_uptime_seconds Device uptime",
            "# TYPE dcentaxe_uptime_seconds counter",
            "dcentaxe_uptime_seconds 3600",
            "# HELP dcentaxe_free_heap_bytes Free heap memory",
            "# TYPE dcentaxe_free_heap_bytes gauge",
            "dcentaxe_free_heap_bytes 123456",
            "# HELP dcentaxe_achievements_unlocked Achievement count",
            "# TYPE dcentaxe_achievements_unlocked gauge",
            "dcentaxe_achievements_unlocked 4",
            "# HELP dcentaxe_lifetime_shares Lifetime shares across reboots",
            "# TYPE dcentaxe_lifetime_shares counter",
            "dcentaxe_lifetime_shares 9999",
        ] {
            assert_line(&body, line);
        }

        // (c) the two conditional per-pool sections, verbatim.
        for line in [
            "# HELP dcentaxe_stratum_pool_shares_pending Per-pool pending submitted shares",
            "# TYPE dcentaxe_stratum_pool_shares_pending gauge",
            "dcentaxe_stratum_pool_shares_pending{pool=\"0\"} 2",
            "dcentaxe_stratum_pool_shares_pending{pool=\"1\"} 0",
            "# HELP dcentaxe_stratum_pool_shares_unresolved_total Per-pool submitted shares with no tracked final pool response",
            "# TYPE dcentaxe_stratum_pool_shares_unresolved_total counter",
            "dcentaxe_stratum_pool_shares_unresolved_total{pool=\"0\"} 1",
            "dcentaxe_stratum_pool_shares_unresolved_total{pool=\"1\"} 0",
            "# HELP dcentaxe_pool_shares_accepted Per-pool accepted shares",
            "# TYPE dcentaxe_pool_shares_accepted counter",
            "dcentaxe_pool_shares_accepted{pool=\"0\"} 100",
            "dcentaxe_pool_shares_accepted{pool=\"1\"} 40",
            "# HELP dcentaxe_pool_shares_rejected Per-pool rejected shares",
            "# TYPE dcentaxe_pool_shares_rejected counter",
            "dcentaxe_pool_shares_rejected{pool=\"0\"} 1",
            "dcentaxe_pool_shares_rejected{pool=\"1\"} 0",
            "# HELP dcentaxe_pool_hashrate_pct Per-pool actual hashrate percentage",
            "# TYPE dcentaxe_pool_hashrate_pct gauge",
            "dcentaxe_pool_hashrate_pct{pool=\"0\",target=\"70\"} 70.0",
            "dcentaxe_pool_hashrate_pct{pool=\"1\",target=\"30\"} 30.0",
            "# HELP dcentaxe_pool_connected Per-pool connection status",
            "# TYPE dcentaxe_pool_connected gauge",
            "dcentaxe_pool_connected{pool=\"0\"} 1",
            "dcentaxe_pool_connected{pool=\"1\"} 0",
        ] {
            assert_line(&body, line);
        }

        // (d) every emitted sample line is well-formed (no `{}` placeholder leak,
        //     no double blank lines) and the body ends with a newline.
        assert!(
            !body.contains("{}"),
            "unfilled placeholder leaked into body"
        );
        assert!(
            body.ends_with('\n'),
            "exposition body must end with a newline"
        );
    }

    // ── conditional sections are SUPPRESSED exactly like the inline body ──────
    #[test]
    fn conditional_pool_sections_are_gated() {
        let snap = busy_snapshot();
        // Empty pending -> no stratum per-pool section.
        // Single split row (len !> 1) -> no split section.
        let single = vec![PoolSplitRow {
            index: 0,
            target_pct: 100,
            dispatched_count: 10,
            shares_accepted: 5,
            shares_rejected: 0,
            connected: true,
        }];
        let body = render_metrics_body(&snap, &[], &single, &ShareTruthView::default());
        assert!(
            !body.contains("dcentaxe_stratum_pool_shares_pending{"),
            "empty pool_pending must suppress the stratum per-pool section"
        );
        assert!(
            !body.contains("dcentaxe_pool_shares_accepted{"),
            "a single split row must suppress the split-mining section (gated on > 1)"
        );
    }

    // ── B2: per-reason reject breakdown, single header + escaping ─────────────
    #[test]
    fn b2_reject_breakdown_samples_and_escaping() {
        let snap = busy_snapshot();
        let truth = ShareTruthView {
            reject_reason_counts: vec![
                ("Above target".to_string(), 3),
                // a key containing a quote and a newline must be escaped so it
                // can't break the exposition line.
                ("weird\"reason\nhere".to_string(), 7),
            ],
            ..ShareTruthView::default()
        };
        let body = render_metrics_body(&snap, &[], &[], &truth);

        // Exactly ONE HELP + ONE TYPE header for the family.
        assert_eq!(
            body.matches("# HELP dcentaxe_pool_shares_rejected_by_reason")
                .count(),
            1,
            "B2 must emit a single HELP header"
        );
        assert_eq!(
            body.matches("# TYPE dcentaxe_pool_shares_rejected_by_reason counter")
                .count(),
            1,
            "B2 must emit a single TYPE header"
        );
        assert_line(
            &body,
            "dcentaxe_pool_shares_rejected_by_reason{reason=\"Above target\"} 3",
        );
        // The quote/newline reason is escaped: \" and \n, and the whole sample
        // stays on ONE physical line.
        assert_line(
            &body,
            "dcentaxe_pool_shares_rejected_by_reason{reason=\"weird\\\"reason\\nhere\"} 7",
        );
        // No raw newline inside the label (escaping kept the line intact).
        assert!(
            !body.contains("reason=\"weird\"reason"),
            "an unescaped quote leaked, breaking the label"
        );
    }

    // ── B3: share-freshness gauges (seconds) + omit-when-never convention ─────
    #[test]
    fn b3_freshness_gauges_present_and_omitted_when_never() {
        let snap = busy_snapshot();

        // Populated: accepted gauge == accepted_unix_ms / 1000 (float tolerance).
        let truth = ShareTruthView {
            last_share_accepted_unix_ms: 1_700_000_000_500,
            last_share_response_unix_ms: 1_700_000_000_500,
            last_share_rejected_unix_ms: 0, // never rejected
            ..ShareTruthView::default()
        };
        let body = render_metrics_body(&snap, &[], &[], &truth);

        // The accepted gauge HELP/TYPE always present.
        assert!(body.contains("# TYPE dcentaxe_last_accepted_share_timestamp_seconds gauge\n"));
        // Sample value == ms/1000 = 1700000000.5
        let needle = "dcentaxe_last_accepted_share_timestamp_seconds ";
        let line = body
            .lines()
            .find(|l| l.starts_with(needle) && !l.starts_with("# "))
            .expect("accepted-share gauge sample must be present when ms > 0");
        let value: f64 = line[needle.len()..].trim().parse().expect("numeric gauge");
        assert!(
            (value - 1_700_000_000.5).abs() < 1e-6,
            "accepted gauge must be ms/1000, got {value}"
        );

        // Never-rejected: HELP/TYPE present but the SAMPLE line is OMITTED.
        assert!(body.contains("# TYPE dcentaxe_last_share_rejected_timestamp_seconds gauge\n"));
        assert!(
            !body
                .lines()
                .any(|l| l.starts_with("dcentaxe_last_share_rejected_timestamp_seconds ")),
            "a never-rejected gauge must omit its sample (no 1970 epoch)"
        );
    }

    // ── NaN / empty-input sentinel safety: never panics, stays well-formed ────
    #[test]
    fn empty_and_nan_input_is_panic_safe() {
        // All-default (zero) input: no panic, core families still present, no
        // conditional/new samples beyond the always-on headers.
        let body = render_metrics_body(
            &MetricsSnapshot::default(),
            &[],
            &[],
            &ShareTruthView::default(),
        );
        assert!(body.starts_with("# HELP dcentaxe_hashrate_ghs"));
        assert_line(&body, "dcentaxe_hashrate_ghs{window=\"1m\"} 0");
        assert_line(&body, "dcentaxe_lifetime_shares 0");
        // B2 header present with zero samples (self-describing, Toolbox pattern).
        assert!(body.contains("# TYPE dcentaxe_pool_shares_rejected_by_reason counter\n"));
        assert!(
            !body.contains("dcentaxe_pool_shares_rejected_by_reason{"),
            "no reject samples on empty input"
        );
        // No share-freshness samples when all timestamps are 0.
        for name in [
            "dcentaxe_last_accepted_share_timestamp_seconds",
            "dcentaxe_last_share_response_timestamp_seconds",
            "dcentaxe_last_share_rejected_timestamp_seconds",
        ] {
            assert!(
                !body.lines().any(|l| l.starts_with(&format!("{name} "))),
                "{name} sample must be omitted on zero/never input"
            );
        }

        // ES-4: NaN/Inf telemetry must not panic AND must render as VALID
        // Prometheus. `NaN` is already valid, but `inf`/`-inf` are NOT — the
        // exposition format requires `+Inf`/`-Inf`, so the renderer coerces them
        // (one non-finite reading otherwise makes the whole scrape unparseable).
        let nan = MetricsSnapshot {
            hashrate_1m_ghs: f64::NAN,
            chip_temp_c: f32::INFINITY,
            power_w: f32::NEG_INFINITY,
            best_difficulty: f64::NAN,
            ..MetricsSnapshot::default()
        };
        let body = render_metrics_body(&nan, &[], &[], &ShareTruthView::default());
        assert!(body.contains("# TYPE dcentaxe_hashrate_ghs gauge"));
        assert!(body.ends_with('\n'));
        // Positive infinity → `+Inf`, negative → `-Inf`, NaN stays `NaN`.
        assert!(
            body.contains("dcentaxe_temperature_celsius{sensor=\"chip\"} +Inf\n"),
            "positive inf must render as +Inf: {body}"
        );
        assert!(
            body.contains("dcentaxe_power_watts -Inf\n"),
            "negative inf must render as -Inf: {body}"
        );
        assert!(body.contains("NaN"), "NaN must stay NaN");
        // No invalid bare `inf`/`-inf` metric value may survive anywhere.
        assert!(
            !body.contains(" inf\n") && !body.contains(" -inf\n"),
            "no bare inf/-inf metric value may remain: {body}"
        );
    }

    // ── escaper unit test (ported Toolbox _escape_label_value) ────────────────
    #[test]
    fn escape_label_value_matches_prometheus_spec() {
        assert_eq!(escape_label_value("plain"), "plain");
        assert_eq!(escape_label_value("a\\b"), "a\\\\b");
        assert_eq!(escape_label_value("a\"b"), "a\\\"b");
        assert_eq!(escape_label_value("a\nb"), "a\\nb");
        // backslash escaped FIRST: a literal `\"` must not become `\\\\"`.
        assert_eq!(escape_label_value("\\\""), "\\\\\\\"");
    }
}
