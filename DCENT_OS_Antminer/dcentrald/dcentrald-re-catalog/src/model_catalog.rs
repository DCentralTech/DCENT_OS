use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Exact,
    Structural,
    Scaffold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelEvidence {
    pub slug: &'static str,
    pub chip_id: u16,
    pub chains: u8,
    pub chips_per_chain: Option<u16>,
    pub default_baud: u32,
    pub response_body_len: usize,
    pub default_frequency_mhz: Option<u16>,
    pub voltage_min_mv: Option<u16>,
    pub voltage_max_mv: Option<u16>,
    pub strength: EvidenceStrength,
    pub provenance: &'static [&'static str],
}

const MASTER_MODELS: &str = "";
const MASTER_PLL: &str = "";
const S17_JIG: &str = "";
const S19_JIG: &str = "";
const S21_PRO_JIG: &str = "";

macro_rules! model {
    ($slug:literal, $chip:literal, $chains:literal, $count:expr, $baud:literal,
     $body:literal, $freq:expr, $vmin:expr, $vmax:expr, $strength:ident, $sources:expr) => {
        ModelEvidence {
            slug: $slug,
            chip_id: $chip,
            chains: $chains,
            chips_per_chain: $count,
            default_baud: $baud,
            response_body_len: $body,
            default_frequency_mhz: $freq,
            voltage_min_mv: $vmin,
            voltage_max_mv: $vmax,
            strength: EvidenceStrength::$strength,
            provenance: $sources,
        }
    };
}

/// S9 through S23 coverage rows used by the simulator and tier-honesty gate.
/// Unknown geometry remains `None`; it is never filled from projections.
pub static ANTMINER_MODELS: &[ModelEvidence] = &[
    model!(
        "s9",
        0x1387,
        3,
        Some(63),
        115_200,
        7,
        Some(650),
        Some(8_000),
        Some(9_000),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, ""]
    ),
    model!(
        "s11",
        0x1391,
        3,
        None,
        115_200,
        7,
        None,
        None,
        None,
        Structural,
        &[MASTER_MODELS, ""]
    ),
    model!(
        "s15",
        0x1391,
        3,
        None,
        115_200,
        7,
        None,
        None,
        None,
        Scaffold,
        &[MASTER_MODELS]
    ),
    model!(
        "t15",
        0x1391,
        3,
        None,
        115_200,
        7,
        None,
        None,
        None,
        Scaffold,
        &[MASTER_MODELS]
    ),
    model!(
        "s17",
        0x1397,
        3,
        Some(48),
        115_740,
        7,
        Some(650),
        None,
        None,
        Exact,
        &[MASTER_MODELS, MASTER_PLL, S17_JIG]
    ),
    model!(
        "s17pro",
        0x1397,
        3,
        Some(48),
        115_740,
        7,
        Some(650),
        None,
        None,
        Exact,
        &[MASTER_MODELS, MASTER_PLL, S17_JIG]
    ),
    model!(
        "t17",
        0x1397,
        3,
        Some(30),
        115_740,
        7,
        Some(650),
        None,
        None,
        Exact,
        &[MASTER_MODELS, MASTER_PLL, S17_JIG]
    ),
    model!(
        "s17plus",
        0x1396,
        3,
        Some(65),
        115_740,
        7,
        None,
        None,
        None,
        Structural,
        &[MASTER_MODELS, ""]
    ),
    model!(
        "t17plus",
        0x1396,
        3,
        Some(44),
        115_740,
        7,
        None,
        None,
        None,
        Structural,
        &[MASTER_MODELS, ""]
    ),
    model!(
        "s17e",
        0x1397,
        3,
        None,
        115_740,
        7,
        None,
        None,
        None,
        Structural,
        &[MASTER_MODELS, ""]
    ),
    model!(
        "s19",
        0x1398,
        3,
        None,
        115_740,
        7,
        Some(650),
        None,
        None,
        Structural,
        &[MASTER_MODELS, MASTER_PLL, S19_JIG]
    ),
    model!(
        "s19pro",
        0x1398,
        3,
        Some(114),
        115_740,
        7,
        Some(675),
        Some(13_000),
        Some(14_200),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, S19_JIG, ""]
    ),
    model!(
        "s19jpro",
        0x1362,
        3,
        Some(126),
        115_200,
        9,
        Some(545),
        None,
        None,
        Exact,
        &[MASTER_MODELS, MASTER_PLL, ""]
    ),
    model!(
        "s19xp",
        0x1366,
        3,
        Some(110),
        115_200,
        9,
        Some(675),
        Some(13_400),
        Some(14_200),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, ""]
    ),
    model!(
        "s19kpro",
        0x1366,
        3,
        Some(77),
        115_200,
        9,
        Some(670),
        Some(13_400),
        Some(14_200),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, ""]
    ),
    model!(
        "s21",
        0x1368,
        3,
        Some(108),
        115_200,
        9,
        Some(525),
        Some(13_400),
        Some(14_200),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, ""]
    ),
    model!(
        "s21pro",
        0x1370,
        3,
        Some(65),
        115_200,
        9,
        Some(525),
        Some(13_400),
        Some(14_200),
        Exact,
        &[MASTER_MODELS, MASTER_PLL, S21_PRO_JIG]
    ),
    model!(
        "s21xp",
        0x1370,
        3,
        None,
        115_200,
        9,
        None,
        Some(13_400),
        Some(14_200),
        Structural,
        &[MASTER_MODELS, MASTER_PLL, S21_PRO_JIG]
    ),
    model!(
        "s23",
        0x1372,
        4,
        None,
        115_200,
        9,
        None,
        None,
        None,
        Scaffold,
        &[""]
    ),
];

pub fn model_evidence(slug: &str) -> Option<&'static ModelEvidence> {
    ANTMINER_MODELS.iter().find(|model| model.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_rows_are_unique_and_s23_stays_ground_truth_free() {
        for (index, model) in ANTMINER_MODELS.iter().enumerate() {
            assert!(ANTMINER_MODELS[..index]
                .iter()
                .all(|other| other.slug != model.slug));
            assert!(!model.provenance.is_empty());
        }
        let s23 = model_evidence("s23").expect("S23 reservation");
        assert_eq!(s23.strength, EvidenceStrength::Scaffold);
        assert_eq!(s23.chips_per_chain, None);
        assert_eq!(s23.default_frequency_mhz, None);
    }

    /// Voltage bounds must be ordered where both are present. An inverted or
    /// degenerate range (min >= max) would break any consumer that clamps a
    /// commanded voltage into `[voltage_min_mv, voltage_max_mv]` — a real safety
    /// hazard on a catalog that drives voltage envelopes. Guards new rows.
    #[test]
    fn voltage_bounds_are_ordered_when_present() {
        for m in ANTMINER_MODELS {
            if let (Some(lo), Some(hi)) = (m.voltage_min_mv, m.voltage_max_mv) {
                assert!(
                    lo < hi,
                    "{} voltage bounds inverted/degenerate: min_mv={lo} >= max_mv={hi}",
                    m.slug
                );
            }
        }
    }

    /// Every model row with EXACT evidence AND a proven default frequency must
    /// carry PLL facts in the sibling pll_bible (keyed by chip_id): if we have a
    /// proven operating frequency, we must also have the PLL register facts to
    /// program it. Catches a contributor adding an Exact chip to one catalog but
    /// not the other (the class that hid the BM1368 500-vs-525 drift). Chips
    /// without a proven frequency (Structural/Scaffold, freq None) are exempt —
    /// we never fabricate PLL facts we do not have.
    #[test]
    fn exact_models_with_a_frequency_have_pll_facts() {
        use crate::pll_bible::pll_expectation;
        for m in ANTMINER_MODELS {
            if m.strength == EvidenceStrength::Exact && m.default_frequency_mhz.is_some() {
                assert!(
                    pll_expectation(m.chip_id).is_some(),
                    "{} is Exact with a proven frequency ({} MHz) but chip_id {:#06x} has no \
                     pll_bible entry — a proven-frequency chip must carry PLL register facts",
                    m.slug,
                    m.default_frequency_mhz.unwrap(),
                    m.chip_id
                );
            }
        }
    }
}
