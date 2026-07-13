//! Per-chip health scoring and ChipMap generation.
//!
//! Focused per-chip analysis for a single board. Produces a color-coded
//! ChipMap grid showing health status of each ASIC chip.
//!
//! ChipMap Color Coding:
//!   Green:  health_score >= 0.90 (healthy)
//!   Yellow: health_score >= 0.70 (marginal)
//!   Orange: health_score >= 0.50 (degraded)
//!   Red:    health_score <  0.50 (poor)
//!   Gray:   health_score == 0    (dead)

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Per-chip health test parameters.
pub struct ChipHealthTest {
    /// Target chain ID to test.
    pub chain_id: u8,
    /// Number of 60-second mining windows to run.
    pub window_count: u8,
    /// Test duration in minutes.
    pub duration_minutes: u8,
}

impl ChipHealthTest {
    /// Create a default chip health test (5 minutes, 5 windows).
    pub fn new(chain_id: u8) -> Self {
        Self {
            chain_id,
            window_count: 5,
            duration_minutes: 5,
        }
    }
}

/// ChipMap grid data for visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipMap {
    /// Chain ID this map belongs to.
    pub chain_id: u8,
    /// Total chip count.
    pub chip_count: u16,
    /// Grid columns (for layout).
    pub columns: u8,
    /// Grid rows (for layout).
    pub rows: u8,
    /// Per-chip cell data (ordered by chip index).
    pub cells: Vec<ChipMapCell>,
}

/// One cell in the ChipMap grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipMapCell {
    /// Chip index (0-based).
    pub index: u16,
    /// Chip address (hex).
    pub address: u8,
    /// Health score (0.0 to 1.0+).
    pub health_score: f32,
    /// Health grade: A, B, C, D, or F.
    pub grade: char,
    /// Color for visualization.
    pub color: ChipColor,
    /// Current frequency in MHz.
    pub frequency_mhz: u16,
    /// Total nonce count from test.
    pub nonce_count: u64,
    /// CRC errors attributed to this chip.
    pub crc_errors: u32,

    // ------------------------------------------------------------------
    // RE-010 LOW-1/LOW-2/LOW-3 additive fields (2026-05-21, RE-010 closure
    // follow-up). All three are `Option<_>` with `#[serde(default,
    // skip_serializing_if = "Option::is_none")]` so:
    //   (a) existing JSON consumers (dashboard, pyasic adapters, fleet
    //       tooling) don't break — unknown fields tolerated by serde,
    //       missing fields default to None;
    //   (b) when None, the field is omitted from the wire (cheap on
    //       bandwidth for the ~126-cell BM1362 vector);
    //   (c) future code paths can populate them without another DTO change.
    // ------------------------------------------------------------------
    /// Expected nonce production **rate** (nonces/second) for a fully-healthy
    /// chip at the current `frequency_mhz`, derived from
    /// `frequency_mhz × 1_000_000 × cores_per_chip / 2³²`.
    ///
    /// `None` when the snapshot builder can't resolve `cores_per_chip` for
    /// the chip family (look up in `dcentrald-silicon-profiles`) or when
    /// `frequency_mhz == 0`. Mirrors the semantic of LuxOS's
    /// `expected_hash_count_fully_healthy_chip` but normalized to a rate —
    /// DCENT_OS's snapshot context does not carry per-chip elapsed windows
    /// the way LuxOS's autotuner does, so a fixed count would be dishonest.
    /// Clients multiply by their own observation window (in seconds) to
    /// derive an expected count comparable to `nonce_count`.
    ///
    /// Provenance: RE-010 handoff §3 LOW-1 (2026-05-21).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_nonce_rate_hz: Option<f32>,

    /// Snapshot timestamp (Unix epoch seconds) when this cell was sampled.
    /// All cells produced by the same `build_chip_health_snapshot()` call
    /// share the same value by design (the snapshot is a single atomic
    /// read of runtime state). Mirrors LuxOS's per-record `chip_health_ts`.
    ///
    /// `None` only when the clock is unreadable (system time before the
    /// Unix epoch — should not happen on running miners).
    ///
    /// Provenance: RE-010 handoff §3 LOW-2 (2026-05-21).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_ts: Option<u64>,

    /// Per-chip die temperature (°C). `None` until `dcentrald-thermal`
    /// exposes per-chip BM1362 on-die temperature reads — separate
    /// HW-gated wiring task. LuxOS observes per-chip die temps on-device
    /// but **collapses them to 4-corner per-board area aggregates** before
    /// the API; DCENT_OS could lead here by exposing the per-chip vector
    /// once the data path lands. The field shape ships now so the future
    /// thermal patch doesn't need another DTO change.
    ///
    /// Provenance: RE-010 handoff §3 LOW-3 (2026-05-21).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub die_temp_c: Option<f32>,

    /// Laplacian hot-spot gradient vs neighbors (°C above mean); from
    /// `dcentrald-chip-analysis`. `None` until per-chip temps exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly_gradient: Option<f32>,

    /// Cross-slot hot z-score; `None` until multi-slot temps exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly_cross_slot_zscore: Option<f32>,

    /// Nonce deficit % below slot average; may be filled from live counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly_nonce_deficit: Option<f32>,
}

impl ChipMapCell {
    /// Attach pure anomaly scores (hot-spot-only ≥ 0) without changing grade.
    pub fn with_anomalies(
        mut self,
        scores: crate::chip_analysis_bridge::ChipAnomalyScores,
    ) -> Self {
        self.anomaly_gradient = Some(scores.gradient);
        self.anomaly_cross_slot_zscore = Some(scores.cross_slot_zscore);
        self.anomaly_nonce_deficit = Some(scores.nonce_deficit);
        self
    }
}

impl ChipMap {
    /// Fill `anomaly_nonce_deficit` from cell `nonce_count` vs map average.
    ///
    /// Safe offline path when die temps are absent (gradient/z-score stay None).
    pub fn enrich_nonce_deficits(&mut self) {
        if self.cells.is_empty() {
            return;
        }
        let nonces: Vec<i64> = self.cells.iter().map(|c| c.nonce_count as i64).collect();
        let slot_avg = crate::chip_analysis_bridge::compute_slot_avg_nonce(&nonces);
        for cell in &mut self.cells {
            cell.anomaly_nonce_deficit = Some(crate::chip_analysis_bridge::compute_nonce_deficit(
                cell.nonce_count as i64,
                slot_avg,
            ));
        }
    }
}

/// Color coding for ChipMap cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChipColor {
    /// health_score >= 0.90
    Green,
    /// health_score >= 0.70
    Yellow,
    /// health_score >= 0.50
    Orange,
    /// health_score < 0.50
    Red,
    /// health_score == 0 (dead chip)
    Gray,
}

impl ChipColor {
    /// Determine color from health score.
    pub fn from_score(score: f32) -> Self {
        // A non-finite score (NaN / ±inf from a corrupt or uninitialized health
        // computation) must NOT fall through the `<` comparisons to the Green
        // "healthy" branch — every `<` is false for NaN, so an unscored/garbage
        // chip would paint GREEN (fail-OPEN: a bad chip shown as fine). Treat any
        // non-finite score as Gray (dead/unknown), matching the score<=0 fail-safe.
        if !score.is_finite() || score <= 0.0 {
            ChipColor::Gray
        } else if score < 0.50 {
            ChipColor::Red
        } else if score < 0.70 {
            ChipColor::Orange
        } else if score < 0.90 {
            ChipColor::Yellow
        } else {
            ChipColor::Green
        }
    }

    /// Get the CSS color code.
    pub fn css_color(&self) -> &'static str {
        match self {
            ChipColor::Green => "#22c55e",
            ChipColor::Yellow => "#eab308",
            ChipColor::Orange => "#f97316",
            ChipColor::Red => "#ef4444",
            ChipColor::Gray => "#6b7280",
        }
    }
}

impl ChipMap {
    /// Create a ChipMap for BM1387 (63 chips per board, 9 columns x 7 rows).
    pub fn bm1387_layout(chain_id: u8) -> Self {
        Self {
            chain_id,
            chip_count: 63,
            columns: 9,
            rows: 7,
            cells: Vec::with_capacity(63),
        }
    }

    /// Create a ChipMap for BM1368 (108 chips per board).
    pub fn bm1368_layout(chain_id: u8) -> Self {
        Self {
            chain_id,
            chip_count: 108,
            columns: 12,
            rows: 9,
            cells: Vec::with_capacity(108),
        }
    }

    /// Add a cell to the ChipMap.
    pub fn add_cell(&mut self, cell: ChipMapCell) {
        self.cells.push(cell);
    }
}

/// Snapshot-style chip health report built from current runtime data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipHealthSnapshot {
    pub report_id: Uuid,
    pub generated_at: String,
    pub report_type: String,
    pub source: String,
    pub total_boards: u8,
    pub total_chips: u16,
    pub warnings: Vec<String>,
    pub recommendations: Vec<String>,
    pub chains: Vec<ChipHealthChainSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipHealthChainSnapshot {
    pub chain_id: u8,
    pub source: String,
    pub chip_count: u16,
    pub responding_chips: u8,
    pub board_temp_c: f32,
    pub board_hashrate_ghs: f64,
    pub board_health_score: f64,
    pub frequency_mhz: u16,
    pub voltage_mv: u16,
    pub errors: u32,
    pub status: String,
    pub chipmap: ChipMap,
}

#[cfg(test)]
mod chip_map_cell_re010_tests {
    //! RE-010 LOW-1/LOW-2/LOW-3 follow-up (2026-05-21): pin the additive-
    //! field contract for `ChipMapCell`. These tests are load-bearing for
    //! the backward-compatible additive contract — they ensure:
    //!   (a) old JSON without the new fields still deserializes (backward
    //!       compatibility for existing dashboard / pyasic / fleet clients);
    //!   (b) None values are omitted from serialized JSON (cheap wire);
    //!   (c) Some values round-trip exactly.
    //!
    //! Provenance: RE-010 handoff §3 LOW-1 / LOW-2 / LOW-3 + the closure
    //! follow-up directive ("ship the shape now, populate as data lands").
    use super::{ChipColor, ChipMap, ChipMapCell};

    #[test]
    fn chip_color_from_score_fails_safe_on_non_finite_and_maps_thresholds() {
        // Fail-safe: a non-finite score must map to Gray (dead/unknown), NEVER
        // Green — every `<` comparison is false for NaN, so without the guard a
        // garbage-scored chip would paint healthy in the dashboard (fail-open).
        assert_eq!(ChipColor::from_score(f32::NAN), ChipColor::Gray);
        assert_eq!(ChipColor::from_score(f32::INFINITY), ChipColor::Gray);
        assert_eq!(ChipColor::from_score(f32::NEG_INFINITY), ChipColor::Gray);
        // Normal threshold ladder (regression pin).
        assert_eq!(ChipColor::from_score(0.0), ChipColor::Gray);
        assert_eq!(ChipColor::from_score(-0.1), ChipColor::Gray);
        assert_eq!(ChipColor::from_score(0.49), ChipColor::Red);
        assert_eq!(ChipColor::from_score(0.50), ChipColor::Orange);
        assert_eq!(ChipColor::from_score(0.69), ChipColor::Orange);
        assert_eq!(ChipColor::from_score(0.70), ChipColor::Yellow);
        assert_eq!(ChipColor::from_score(0.89), ChipColor::Yellow);
        assert_eq!(ChipColor::from_score(0.90), ChipColor::Green);
        assert_eq!(ChipColor::from_score(1.0), ChipColor::Green);
    }

    fn baseline_cell() -> ChipMapCell {
        ChipMapCell {
            index: 7,
            address: 0x1C,
            health_score: 0.93,
            grade: 'A',
            color: ChipColor::Green,
            frequency_mhz: 525,
            nonce_count: 1234,
            crc_errors: 2,
            expected_nonce_rate_hz: None,
            health_ts: None,
            die_temp_c: None,
            anomaly_gradient: None,
            anomaly_cross_slot_zscore: None,
            anomaly_nonce_deficit: None,
        }
    }

    #[test]
    fn none_fields_are_omitted_from_serialized_json() {
        let cell = baseline_cell();
        let json = serde_json::to_string(&cell).expect("serialize");
        assert!(
            !json.contains("expected_nonce_rate_hz"),
            "None expected_nonce_rate_hz must not appear on the wire: {json}"
        );
        assert!(
            !json.contains("health_ts"),
            "None health_ts must not appear on the wire: {json}"
        );
        assert!(
            !json.contains("die_temp_c"),
            "None die_temp_c must not appear on the wire: {json}"
        );
        assert!(
            !json.contains("anomaly_"),
            "None anomaly_* fields must not appear on the wire: {json}"
        );
    }

    #[test]
    fn enrich_nonce_deficits_marks_weak_chips() {
        let mut map = ChipMap {
            chain_id: 0,
            chip_count: 2,
            columns: 2,
            rows: 1,
            cells: vec![
                ChipMapCell {
                    index: 0,
                    address: 0,
                    health_score: 1.0,
                    grade: 'A',
                    color: ChipColor::Green,
                    frequency_mhz: 500,
                    nonce_count: 100,
                    crc_errors: 0,
                    expected_nonce_rate_hz: None,
                    health_ts: None,
                    die_temp_c: None,
                    anomaly_gradient: None,
                    anomaly_cross_slot_zscore: None,
                    anomaly_nonce_deficit: None,
                },
                ChipMapCell {
                    index: 1,
                    address: 4,
                    health_score: 0.5,
                    grade: 'D',
                    color: ChipColor::Orange,
                    frequency_mhz: 500,
                    nonce_count: 10,
                    crc_errors: 0,
                    expected_nonce_rate_hz: None,
                    health_ts: None,
                    die_temp_c: None,
                    anomaly_gradient: None,
                    anomaly_cross_slot_zscore: None,
                    anomaly_nonce_deficit: None,
                },
            ],
        };
        map.enrich_nonce_deficits();
        assert_eq!(map.cells[0].anomaly_nonce_deficit, Some(0.0));
        assert!(map.cells[1].anomaly_nonce_deficit.unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn old_clients_can_deserialize_without_new_fields() {
        // The "old client" snapshot — a `ChipMapCell` JSON written before
        // RE-010 LOW-1/2/3 landed. Must still deserialize.
        let old_json = r#"{
            "index": 0,
            "address": 0,
            "health_score": 1.0,
            "grade": "A",
            "color": "Green",
            "frequency_mhz": 650,
            "nonce_count": 7,
            "crc_errors": 0
        }"#;
        let cell: ChipMapCell = serde_json::from_str(old_json).expect("deserialize old shape");
        assert_eq!(cell.index, 0);
        assert_eq!(cell.frequency_mhz, 650);
        assert!(cell.expected_nonce_rate_hz.is_none());
        assert!(cell.health_ts.is_none());
        assert!(cell.die_temp_c.is_none());
    }

    #[test]
    fn some_fields_round_trip_exactly() {
        let mut cell = baseline_cell();
        cell.expected_nonce_rate_hz = Some(17.249_f32);
        cell.health_ts = Some(1_777_500_000);
        cell.die_temp_c = Some(62.5_f32);
        let json = serde_json::to_string(&cell).expect("serialize");
        let round: ChipMapCell = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round.expected_nonce_rate_hz, Some(17.249_f32));
        assert_eq!(round.health_ts, Some(1_777_500_000));
        assert_eq!(round.die_temp_c, Some(62.5_f32));
    }

    #[test]
    fn unknown_fields_on_the_wire_are_tolerated() {
        // Future shape evolution: a newer producer adds a field we don't
        // know about yet. We must still deserialize the old shape we DO
        // know — serde-default Option fields make this safe.
        let future_json = r#"{
            "index": 1,
            "address": 4,
            "health_score": 0.5,
            "grade": "C",
            "color": "Orange",
            "frequency_mhz": 500,
            "nonce_count": 12,
            "crc_errors": 1,
            "expected_nonce_rate_hz": 109.27,
            "health_ts": 1777500000,
            "die_temp_c": null,
            "some_future_field": "v2_only"
        }"#;
        let cell: ChipMapCell = serde_json::from_str(future_json).expect("future shape");
        assert!(cell.expected_nonce_rate_hz.is_some());
        assert_eq!(cell.health_ts, Some(1_777_500_000));
        assert!(cell.die_temp_c.is_none());
    }
}
