//! W13.D1: PVT (Process-Voltage-Temperature) table contract for the
//! `/api/miner/pvt-table` endpoint.
//!
//! Mirrors the per-SKU freq/voltage table exposed by
//! `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku::freq_voltage_table()`
//! into a HAL-free DTO so dashboard / CLI / fleet-tool consumers can pull the
//! envelope without taking a HAL dep.
//!
//! # Cross-references
//! - See `~/
//! - See `~/
//! - See `~/
//!
//! # Economic-tier mapping
//! The `grade` string is the Bitcoin-economist-blessed mapping from SKU →
//! human-friendly grade label used by the dashboard / CLI:
//!
//! | SKU                                     | grade                     |
//! |-----------------------------------------|---------------------------|
//! | BHB42601 / BHB42603 / BHB42621 / BHB42641 | `standard`              |
//! | BHB42631 / BHB42632 / BHB42651          | `low-freq-extended`       |
//! | BHB42801 / BHB42811 / BHB42821          | `high-bin`                |
//! | BHB42831                                | `high-bin-extended`       |
//! | BHB42803                                | `single-voltage`          |
//! | BHB42611                                | `mixable`                 |
//! | BHB42701                                | `efficiency`              |
//! | BHB42841                                | `low-power-salvage`       |
//! | (unknown / non-BM1362)                  | `standard`                |

use serde::{Deserialize, Serialize};

/// One row of a per-SKU PVT level table.
///
/// `voltages` is per-chain: a 4-chain SKU has 4 entries, a 3-chain SKU
/// (BHB42803 only) has 3. W13 ships symmetric-only dispatch
/// (`[freq; chain_count]`), so all entries are identical until W14+ wires
/// per-chain mix_levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PvtLevelEntry {
    /// Frequency in MHz.
    pub freq_mhz: u16,
    /// Per-chain chip-rail voltage in millivolts. Length matches
    /// `PvtTableResponse::chain_count`.
    pub voltages_mv: Vec<u16>,
}

/// Response payload for `GET /api/miner/pvt-table`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PvtTableResponse {
    /// Hashboard SKU id (e.g. `BHB42601`). `unknown` when the SKU has not
    /// been detected yet.
    pub sku: String,
    /// Economic-tier grade label. See module docs for the full mapping.
    pub grade: String,
    /// `true` ⇒ single-voltage VRM (BHB42803 only). The dashboard MUST
    /// hide the voltage slider when this is set.
    pub voltage_fixed: bool,
    /// `true` ⇒ per-chain `mix_levels` is supported (BHB42611 only).
    /// W13 ships symmetric-only dispatch.
    pub mix_levels: bool,
    /// `true` ⇒ requires APW12+ register-based PSU protocol (high-bin
    /// + repair-class). The dashboard MUST surface a PSU-class warning
    ///   before any cold-boot when this is set against an APW12 SMBus PSU.
    pub requires_apw12_plus: bool,
    /// `true` ⇒ inverted curve (freq↓ ⇒ volt↑). BHB42841 only. Autotuner
    /// heuristics MUST consult this before walking the table.
    pub inverted_curve: bool,
    /// Number of populated chains (3 for BHB42803, 4 for everything else).
    pub chain_count: u8,
    /// Number of BM1362 ASICs per chain.
    pub asics_per_chain: u8,
    /// Per-tier freq + voltage rows. Ordered top-down (highest freq first
    /// for STANDARD curves; reverse for `inverted_curve`).
    pub levels: Vec<PvtLevelEntry>,
}

impl Default for PvtTableResponse {
    fn default() -> Self {
        Self {
            sku: "unknown".to_string(),
            grade: "standard".to_string(),
            voltage_fixed: false,
            mix_levels: false,
            requires_apw12_plus: false,
            inverted_curve: false,
            chain_count: 0,
            asics_per_chain: 0,
            levels: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pvt_table_response_default_is_unknown_sku_standard_grade() {
        let r = PvtTableResponse::default();
        assert_eq!(r.sku, "unknown");
        assert_eq!(r.grade, "standard");
        assert!(!r.voltage_fixed);
        assert!(!r.mix_levels);
        assert!(!r.requires_apw12_plus);
        assert!(!r.inverted_curve);
        assert_eq!(r.chain_count, 0);
        assert_eq!(r.asics_per_chain, 0);
        assert!(r.levels.is_empty());
    }

    #[test]
    fn pvt_table_response_serializes_snake_case_fields() {
        let r = PvtTableResponse {
            sku: "BHB42601".to_string(),
            grade: "standard".to_string(),
            voltage_fixed: false,
            mix_levels: false,
            requires_apw12_plus: false,
            inverted_curve: false,
            chain_count: 4,
            asics_per_chain: 126,
            levels: vec![
                PvtLevelEntry {
                    freq_mhz: 545,
                    voltages_mv: vec![1320, 1320, 1320, 1320],
                },
                PvtLevelEntry {
                    freq_mhz: 525,
                    voltages_mv: vec![1330, 1330, 1330, 1330],
                },
            ],
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["sku"], "BHB42601");
        assert_eq!(j["grade"], "standard");
        assert_eq!(j["voltage_fixed"], false);
        assert_eq!(j["mix_levels"], false);
        assert_eq!(j["requires_apw12_plus"], false);
        assert_eq!(j["inverted_curve"], false);
        assert_eq!(j["chain_count"], 4);
        assert_eq!(j["asics_per_chain"], 126);
        assert_eq!(j["levels"][0]["freq_mhz"], 545);
        assert_eq!(j["levels"][0]["voltages_mv"][0], 1320);
        assert_eq!(j["levels"][0]["voltages_mv"].as_array().unwrap().len(), 4);
        assert_eq!(j["levels"][1]["freq_mhz"], 525);
    }

    #[test]
    fn pvt_table_endpoint_returns_correct_levels_for_bhb42601() {
        // Mirror of dcentrald-silicon-profiles::bm1362::BHB42601_FREQ_VOLT_TABLE
        // (5 tiers @ 545/525/505/485/465 MHz, voltages 1320/1330/1345/1360/1380).
        let r = PvtTableResponse {
            sku: "BHB42601".to_string(),
            grade: "standard".to_string(),
            voltage_fixed: false,
            mix_levels: false,
            requires_apw12_plus: false,
            inverted_curve: false,
            chain_count: 4,
            asics_per_chain: 126,
            levels: vec![
                PvtLevelEntry {
                    freq_mhz: 545,
                    voltages_mv: vec![1320; 4],
                },
                PvtLevelEntry {
                    freq_mhz: 525,
                    voltages_mv: vec![1330; 4],
                },
                PvtLevelEntry {
                    freq_mhz: 505,
                    voltages_mv: vec![1345; 4],
                },
                PvtLevelEntry {
                    freq_mhz: 485,
                    voltages_mv: vec![1360; 4],
                },
                PvtLevelEntry {
                    freq_mhz: 465,
                    voltages_mv: vec![1380; 4],
                },
            ],
        };
        assert_eq!(r.levels.len(), 5);
        assert_eq!(r.levels[0].freq_mhz, 545);
        assert_eq!(r.levels[0].voltages_mv, vec![1320, 1320, 1320, 1320]);
        assert_eq!(r.levels[4].freq_mhz, 465);
        assert_eq!(r.levels[4].voltages_mv[0], 1380);
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["levels"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn pvt_table_response_round_trips() {
        let original = PvtTableResponse {
            sku: "BHB42803".to_string(),
            grade: "single-voltage".to_string(),
            voltage_fixed: true,
            mix_levels: false,
            requires_apw12_plus: true,
            inverted_curve: false,
            chain_count: 3,
            asics_per_chain: 84,
            levels: vec![
                PvtLevelEntry {
                    freq_mhz: 675,
                    voltages_mv: vec![1530; 3],
                },
                PvtLevelEntry {
                    freq_mhz: 645,
                    voltages_mv: vec![1530; 3],
                },
            ],
        };
        let j = serde_json::to_string(&original).unwrap();
        let round: PvtTableResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(original, round);
    }
}
