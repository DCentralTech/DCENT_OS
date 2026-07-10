//! W13.D1 — `GET /api/miner/pvt-table` route.
//!
//! Returns the full per-SKU PVT (Process-Voltage-Temperature) freq/voltage
//! table for the detected hashboard. Sourced from
//! `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku::freq_voltage_table()`
//! + `flags()` + `chain_count()` + `asics_per_chain()`.
//!
//! Cross-references:
//!   - See `~/
//!   - See `~/
//!   - See `~/

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use dcentrald_api_types::pvt_table::{PvtLevelEntry, PvtTableResponse};
use dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku;

use crate::AppState;

/// Build the `/api/miner/pvt-table` sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/miner/pvt-table", get(get_pvt_table))
}

async fn get_pvt_table(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let hw = state
        .hardware_info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let sku_id = hw.hb_type.clone().unwrap_or_else(|| "unknown".to_string());
    Json(build_pvt_response(&sku_id))
}

/// Build a `PvtTableResponse` from the SKU id string. Returns the
/// defaulted (`unknown`, empty levels) response when the id is not a
/// recognised BM1362 SKU.
pub(crate) fn build_pvt_response(sku_id: &str) -> PvtTableResponse {
    let Some(sku) = Bm1362HashboardSku::from_id(sku_id) else {
        return PvtTableResponse::default();
    };
    let chain_count = sku.chain_count();
    let flags = sku.flags();
    let levels: Vec<PvtLevelEntry> = sku
        .freq_voltage_table()
        .iter()
        .map(|(freq, volt)| PvtLevelEntry {
            freq_mhz: *freq,
            // W13 ships symmetric-only dispatch — every chain runs at the
            // same voltage. Per-chain asymmetric dispatch (mix_levels)
            // deferred to W14+.
            voltages_mv: vec![*volt; chain_count as usize],
        })
        .collect();
    PvtTableResponse {
        sku: sku.hashboard_id().to_string(),
        grade: grade_for(sku),
        voltage_fixed: flags.voltage_fixed,
        mix_levels: flags.mix_levels,
        requires_apw12_plus: flags.requires_apw12_plus,
        inverted_curve: flags.inverted_curve,
        chain_count,
        asics_per_chain: sku.asics_per_chain(),
        levels,
    }
}

fn grade_for(sku: Bm1362HashboardSku) -> String {
    use Bm1362HashboardSku as Sku;
    match sku {
        Sku::Bhb42601 | Sku::Bhb42603 | Sku::Bhb42621 | Sku::Bhb42641 => "standard",
        Sku::Bhb42631 | Sku::Bhb42632 | Sku::Bhb42651 => "low-freq-extended",
        Sku::Bhb42801 | Sku::Bhb42811 | Sku::Bhb42821 => "high-bin",
        Sku::Bhb42831 => "high-bin-extended",
        Sku::Bhb42803 => "single-voltage",
        Sku::Bhb42611 => "mixable",
        Sku::Bhb42701 => "efficiency",
        Sku::Bhb42841 => "low-power-salvage",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pvt_table_endpoint_returns_correct_levels_for_bhb42601() {
        let r = build_pvt_response("BHB42601");
        assert_eq!(r.sku, "BHB42601");
        assert_eq!(r.grade, "standard");
        assert!(!r.voltage_fixed);
        assert!(!r.mix_levels);
        assert!(!r.requires_apw12_plus);
        assert!(!r.inverted_curve);
        assert_eq!(r.chain_count, 4);
        assert_eq!(r.asics_per_chain, 126);
        assert_eq!(r.levels.len(), 5);
        // Top tier: 545 MHz @ 1320 mV across 4 chains.
        assert_eq!(r.levels[0].freq_mhz, 545);
        assert_eq!(r.levels[0].voltages_mv, vec![1320, 1320, 1320, 1320]);
        // Bottom tier: 465 MHz @ 1380 mV.
        assert_eq!(r.levels[4].freq_mhz, 465);
        assert_eq!(r.levels[4].voltages_mv[0], 1380);
    }

    #[test]
    fn pvt_table_unknown_sku_returns_default() {
        let r = build_pvt_response("BHB99999");
        assert_eq!(r.sku, "unknown");
        assert_eq!(r.grade, "standard");
        assert!(r.levels.is_empty());
        assert_eq!(r.chain_count, 0);
        assert_eq!(r.asics_per_chain, 0);
    }

    #[test]
    fn pvt_table_voltage_fixed_sku_flagged() {
        let r = build_pvt_response("BHB42803");
        assert!(r.voltage_fixed);
        assert!(r.requires_apw12_plus);
        assert_eq!(r.grade, "single-voltage");
        assert_eq!(r.chain_count, 3);
        assert_eq!(r.asics_per_chain, 84);
        // Single-voltage table: every level at 1530 mV.
        for entry in &r.levels {
            assert!(entry.voltages_mv.iter().all(|v| *v == 1530));
            assert_eq!(entry.voltages_mv.len(), 3);
        }
    }

    #[test]
    fn pvt_table_high_bin_requires_apw12_plus() {
        let r = build_pvt_response("BHB42801");
        assert_eq!(r.grade, "high-bin");
        assert!(r.requires_apw12_plus);
        assert!(!r.voltage_fixed);
        assert_eq!(r.asics_per_chain, 88);
    }

    #[test]
    fn pvt_table_low_power_salvage_inverted_curve() {
        let r = build_pvt_response("BHB42841");
        assert_eq!(r.grade, "low-power-salvage");
        assert!(r.inverted_curve);
    }

    #[test]
    fn pvt_table_mixable_sku_flag() {
        let r = build_pvt_response("BHB42611");
        assert_eq!(r.grade, "mixable");
        assert!(r.mix_levels);
        assert_eq!(r.asics_per_chain, 120);
    }
}
