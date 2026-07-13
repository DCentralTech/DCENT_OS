//! Bridge pure `dcentrald-chip-analysis` math into diagnostics (P2-4 backlog).
//!
//! Keeps scoring **HAL-free** so unit tests run on any host. Live ChipMap
//! builders should call these helpers when per-chip temps/nonces exist —
//! do not re-implement Laplacian / z-score / nonce-deficit elsewhere.

pub use dcentrald_chip_analysis::{
    analyze_chip, compute_cross_slot_zscore, compute_hot_gradient, compute_hot_zscore,
    compute_mean_std, compute_nonce_deficit, compute_slot_avg_nonce, ChipAnalysis,
};

/// Optional anomaly enrichment for API/JSON additive fields (hot-spot-only ≥ 0).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChipAnomalyScores {
    pub gradient: f32,
    pub cross_slot_zscore: f32,
    pub nonce_deficit: f32,
}

impl From<ChipAnalysis> for ChipAnomalyScores {
    fn from(c: ChipAnalysis) -> Self {
        Self {
            gradient: c.gradient,
            cross_slot_zscore: c.cross_slot_zscore,
            nonce_deficit: c.nonce_deficit,
        }
    }
}

/// Convenience wrapper matching diagnostics vocabulary.
pub fn enrich_cell_anomalies(
    center_temp_c: i32,
    neighbor_temps_c: &[i32],
    slot_temps_for_position: &[i32],
    chip_nonces: i64,
    slot_avg_nonces: f64,
) -> ChipAnomalyScores {
    analyze_chip(
        center_temp_c,
        neighbor_temps_c,
        slot_temps_for_position,
        chip_nonces,
        slot_avg_nonces,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_center_has_positive_gradient() {
        let scores = enrich_cell_anomalies(80, &[60, 60, 60, 60], &[70, 72, 80], 100, 100.0);
        assert!(scores.gradient > 0.0);
    }

    #[test]
    fn cool_or_equal_gradient_is_zero() {
        let scores = enrich_cell_anomalies(50, &[60, 60], &[], 50, 100.0);
        assert_eq!(scores.gradient, 0.0);
        assert!(scores.nonce_deficit > 0.0);
    }

    #[test]
    fn chip_analysis_crate_is_no_longer_orphan() {
        let a = analyze_chip(70, &[65], &[70], 10, 10.0);
        assert_eq!(a.nonce_deficit, 0.0);
    }
}
