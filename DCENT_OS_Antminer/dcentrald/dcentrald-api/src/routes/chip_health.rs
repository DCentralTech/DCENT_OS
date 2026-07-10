//! A06 — read-only per-chip health endpoint (`GET /api/chips/health`).
//!
//! Flat per-chip hashrate-ratio / error-rate array sourced from the live
//! autotuner chip-health snapshot (`AppState::autotuner_chip_health_rx`).
//!
//! This is deliberately distinct from the existing, mode-gated,
//! envelope-shaped `GET /api/autotuner/chip-health` (which wraps the snapshot
//! in freshness metadata and falls back to loading saved profiles). This
//! endpoint is a flat, stable per-chip array intended for fine-grained
//! autotuner / operator / fleet diagnostics — it only ever reflects the
//! already-published runtime snapshot and never probes ASICs.
//!
//! Source (goldmine finding):
//!
//!   — CAND-04: "LuxOS exposes per-chip hash rate and error rate. DCENT has
//!     per-chain telemetry but no per-chip health surface via REST. Needed
//!     for fine-grained autotuner diagnostics."

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use dcentrald_autotuner::LiveChipHealthState;

use crate::AppState;

/// One per-chip health row (flat, dashboard/fleet-friendly).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerChipHealthRow {
    /// Chain this chip belongs to.
    pub chain_id: u8,
    /// Chip index on the chain.
    pub chip_index: u8,
    /// actual nonces / expected nonces (rolling EMA). 1.0 = nominal.
    pub hashrate_ratio: f64,
    /// Rolling-EMA hardware error rate, as a percentage.
    pub error_rate_pct: f64,
    /// Composite health score (0.0 = dead, 1.0 = perfect).
    pub health_score: f64,
    /// Current operating frequency (MHz).
    pub freq_mhz: u16,
    /// Health classification: Healthy / Watch / Warning / Critical / Dead.
    pub status: String,
}

/// Per-chip health response: a flat array plus freshness/availability flags.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerChipHealthResponse {
    /// `true` when a live autotuner chip-health snapshot was available.
    /// `false` means the background monitor has not published yet (the array
    /// is empty — this is honest "no data", not "all chips healthy").
    pub available: bool,
    /// Number of chips in the array.
    pub total_chips: usize,
    /// Seconds-since-epoch of the snapshot (0 when unavailable).
    pub last_update_s: u64,
    /// Per-chip rows (empty when unavailable).
    pub chips: Vec<PerChipHealthRow>,
}

/// Pure mapping of the live snapshot → flat per-chip rows. HAL-free and
/// host-testable.
pub fn rows_from_snapshot(snapshot: Option<&LiveChipHealthState>) -> PerChipHealthResponse {
    match snapshot {
        Some(s) => {
            let chips: Vec<PerChipHealthRow> = s
                .chips
                .iter()
                .map(|c| PerChipHealthRow {
                    chain_id: c.chain_id,
                    chip_index: c.chip_index,
                    hashrate_ratio: c.hashrate_ratio,
                    error_rate_pct: c.error_rate_pct,
                    health_score: c.health_score,
                    freq_mhz: c.freq_mhz,
                    status: c.status.to_string(),
                })
                .collect();
            PerChipHealthResponse {
                available: true,
                total_chips: chips.len(),
                last_update_s: s.last_update_s,
                chips,
            }
        }
        None => PerChipHealthResponse {
            available: false,
            total_chips: 0,
            last_update_s: 0,
            chips: Vec::new(),
        },
    }
}

/// Build the `/api/chips/health` sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/chips/health", get(get_chips_health))
}

async fn get_chips_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let snapshot = state.autotuner_chip_health_rx.borrow().clone();
    Json(rows_from_snapshot(snapshot.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_autotuner::{ChipHealthLevel, ChipHealthStatus};

    fn chip(chain: u8, idx: u8, ratio: f64, err: f64, level: ChipHealthLevel) -> ChipHealthStatus {
        ChipHealthStatus {
            chain_id: chain,
            chip_index: idx,
            health_score: 0.9,
            trend: 0.0,
            estimated_days_to_warning: None,
            error_rate_pct: err,
            freq_mhz: 500,
            backoff_count: 0,
            hashrate_ratio: ratio,
            status: level,
        }
    }

    #[test]
    fn none_snapshot_is_honest_unavailable() {
        let r = rows_from_snapshot(None);
        assert!(!r.available);
        assert_eq!(r.total_chips, 0);
        assert_eq!(r.last_update_s, 0);
        assert!(r.chips.is_empty());
    }

    #[test]
    fn maps_chips_flat_with_status_strings() {
        let state = LiveChipHealthState {
            last_update_s: 1_700_000_000,
            chips: vec![
                chip(7, 0, 1.0, 0.01, ChipHealthLevel::Healthy),
                chip(7, 1, 0.4, 5.0, ChipHealthLevel::Critical),
                chip(8, 2, 0.0, 0.0, ChipHealthLevel::Dead),
            ],
        };
        let r = rows_from_snapshot(Some(&state));
        assert!(r.available);
        assert_eq!(r.total_chips, 3);
        assert_eq!(r.last_update_s, 1_700_000_000);
        assert_eq!(r.chips[0].chain_id, 7);
        assert_eq!(r.chips[0].chip_index, 0);
        assert_eq!(r.chips[0].hashrate_ratio, 1.0);
        assert_eq!(r.chips[0].status, "Healthy");
        assert_eq!(r.chips[1].status, "Critical");
        assert_eq!(r.chips[1].error_rate_pct, 5.0);
        assert_eq!(r.chips[2].status, "Dead");
    }

    #[test]
    fn response_serializes_to_expected_shape() {
        let state = LiveChipHealthState {
            last_update_s: 42,
            chips: vec![chip(6, 3, 0.95, 0.2, ChipHealthLevel::Watch)],
        };
        let r = rows_from_snapshot(Some(&state));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["available"], serde_json::json!(true));
        assert_eq!(v["total_chips"], serde_json::json!(1));
        assert_eq!(v["chips"][0]["chain_id"], serde_json::json!(6));
        assert_eq!(v["chips"][0]["chip_index"], serde_json::json!(3));
        assert_eq!(v["chips"][0]["status"], serde_json::json!("Watch"));
    }
}
