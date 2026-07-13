use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PllExpectation {
    pub chip_id: u16,
    pub register: u8,
    pub reference_clock_mhz: u8,
    pub reset_value: Option<u32>,
    pub representative_frequency_mhz: Option<u16>,
    pub representative_value: Option<u32>,
    pub provenance: &'static str,
}

const MASTER: &str = "";

pub static PLL_EXPECTATIONS: &[PllExpectation] = &[
    PllExpectation { chip_id: 0x1387, register: 0x0c, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: Some(650), representative_value: Some(0x0068_0221), provenance: MASTER },
    PllExpectation { chip_id: 0x1391, register: 0x0c, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: None, representative_value: None, provenance: ":freq_pll_1385" },
    PllExpectation { chip_id: 0x1397, register: 0x08, reference_clock_mhz: 25, reset_value: Some(0xc060_0161), representative_frequency_mhz: Some(650), representative_value: None, provenance: MASTER },
    PllExpectation { chip_id: 0x1398, register: 0x08, reference_clock_mhz: 25, reset_value: Some(0xc060_0161), representative_frequency_mhz: Some(650), representative_value: None, provenance: MASTER },
    PllExpectation { chip_id: 0x1362, register: 0x08, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: Some(545), representative_value: Some(0x50da_0141), provenance: MASTER },
    PllExpectation { chip_id: 0x1366, register: 0x08, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: Some(670), representative_value: None, provenance: MASTER },
    PllExpectation { chip_id: 0x1368, register: 0x08, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: Some(525), representative_value: None, provenance: MASTER },
    PllExpectation { chip_id: 0x1370, register: 0x08, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: Some(525), representative_value: None, provenance: ":get_pllparam_divider@0x000cb644" },
    PllExpectation { chip_id: 0x1372, register: 0x08, reference_clock_mhz: 25, reset_value: None, representative_frequency_mhz: None, representative_value: None, provenance: "SCAFFOLD_NO_GROUND_TRUTH" },
];

pub fn pll_expectation(chip_id: u16) -> Option<&'static PllExpectation> {
    PLL_EXPECTATIONS
        .iter()
        .find(|expectation| expectation.chip_id == chip_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_known_values_are_pinned() {
        assert_eq!(
            pll_expectation(0x1387).and_then(|p| p.representative_value),
            Some(0x0068_0221)
        );
        assert_eq!(
            pll_expectation(0x1362).and_then(|p| p.representative_value),
            Some(0x50da_0141)
        );
        assert_eq!(
            pll_expectation(0x1372).and_then(|p| p.representative_value),
            None
        );
    }

    #[test]
    fn bm1368_s21_representative_frequency_is_the_proven_525() {
        // BM1368/S21 proven target is 525 MHz (S21 .135), matching the
        // model_catalog s21 row (Exact) and MASTER_PLL_REGISTER_BIBLE §6.1.
        // Regression pin for the prior 500 MHz transcription error.
        assert_eq!(
            pll_expectation(0x1368).and_then(|p| p.representative_frequency_mhz),
            Some(525)
        );
    }

    /// Every pll_bible representative frequency must be a REAL catalogued board
    /// operating point: it must equal the `default_frequency_mhz` of at least
    /// one `ModelEvidence` with the same chip_id. Catches cross-catalog drift
    /// like the BM1368/S21 500-vs-525 bug (500 matched no board). A chip with
    /// several boards at different freqs (BM1398: s19=650, s19pro=675) is fine
    /// as long as the representative matches ONE of them.
    #[test]
    fn representative_freq_matches_a_catalogued_board() {
        use crate::model_catalog::ANTMINER_MODELS;
        for p in PLL_EXPECTATIONS {
            if let Some(freq) = p.representative_frequency_mhz {
                let matches = ANTMINER_MODELS
                    .iter()
                    .any(|m| m.chip_id == p.chip_id && m.default_frequency_mhz == Some(freq));
                assert!(
                    matches,
                    "pll_bible chip_id {:#06x} representative_frequency_mhz={} matches no \
                     ANTMINER_MODELS board with that chip_id (a representative frequency must be \
                     a real catalogued board operating point)",
                    p.chip_id, freq
                );
            }
        }
    }
}
