//! Deterministic four-divider PLL search primitives shared by evidence-backed
//! BM13xx family profiles.
//!
//! The search engine owns only divider mathematics and deterministic candidate
//! ordering. Register encoding remains family-specific because BM1398 stores
//! raw post-dividers while later families use different encodings.

use serde::{Deserialize, Serialize};

/// One resolved `(refdiv, fbdiv, postdiv1, postdiv2)` tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FourDividerPll {
    pub refdiv: u8,
    pub fbdiv: u16,
    pub postdiv1: u8,
    pub postdiv2: u8,
}

impl FourDividerPll {
    pub const fn output_millimhz(self, reference_mhz: u16) -> Option<u64> {
        let denominator = self.refdiv as u64 * self.postdiv1 as u64 * self.postdiv2 as u64;
        if denominator == 0 {
            return None;
        }
        Some(reference_mhz as u64 * self.fbdiv as u64 * 1_000 / denominator)
    }

    pub const fn output_mhz(self, reference_mhz: u16) -> Option<u32> {
        match self.output_millimhz(reference_mhz) {
            Some(millimhz) => Some((millimhz / 1_000) as u32),
            None => None,
        }
    }

    pub const fn vco_mhz(self, reference_mhz: u16) -> Option<u32> {
        if self.refdiv == 0 {
            return None;
        }
        Some(reference_mhz as u32 * self.fbdiv as u32 / self.refdiv as u32)
    }
}

/// Evidence-derived search envelope for a four-divider PLL.
///
/// Candidate enumeration is exactly `refdiv_order`, then increasing
/// `postdiv2` followed by increasing `postdiv1` constrained to
/// `postdiv1 >= postdiv2`. For each divider tuple the vendor algorithm
/// rounds one feedback divider to the nearest integer. Only a strictly smaller
/// rational frequency error replaces the winner, so ties retain the first
/// candidate without depending on floating-point behavior.
// Deliberately not `Deserialize`: an untrusted document must not manufacture
// a new hardware search envelope. Family modules publish evidence-backed
// constants, while `resolve` still validates every field defensively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FourDividerPllSearchSpec {
    pub reference_mhz: u16,
    pub refdiv_order: [u8; 2],
    pub fbdiv_min: u16,
    pub fbdiv_max: u16,
    pub postdiv_min: u8,
    pub postdiv_max: u8,
    pub vco_min_mhz: u16,
    pub vco_max_mhz: u16,
    pub refdiv_one_vco_max_mhz: u16,
    /// Candidate error must be strictly smaller than this ceiling.
    pub max_error_millimhz_exclusive: u32,
}

impl FourDividerPllSearchSpec {
    pub const fn with_reference_mhz(mut self, reference_mhz: u16) -> Self {
        self.reference_mhz = reference_mhz;
        self
    }

    pub fn resolve(self, target_mhz: u16) -> Option<FourDividerPll> {
        if target_mhz == 0
            || self.reference_mhz == 0
            || self.fbdiv_min == 0
            || self.fbdiv_min > self.fbdiv_max
            || self.postdiv_min == 0
            || self.postdiv_min > self.postdiv_max
            || self.vco_min_mhz > self.vco_max_mhz
            || self.refdiv_order.contains(&0)
            || self.max_error_millimhz_exclusive == 0
        {
            return None;
        }

        // Store the exact rational error as numerator/denominator. Comparing
        // cross-products keeps the search deterministic across architectures.
        let mut best: Option<(u64, u64, FourDividerPll)> = None;
        for refdiv in self.refdiv_order {
            for postdiv2 in self.postdiv_min..=self.postdiv_max {
                for postdiv1 in postdiv2.max(self.postdiv_min)..=self.postdiv_max {
                    let denominator = refdiv as u64 * postdiv1 as u64 * postdiv2 as u64;
                    let requested_feedback_numerator = target_mhz as u64 * denominator;
                    let reference_mhz = self.reference_mhz as u64;
                    let fbdiv = (requested_feedback_numerator + reference_mhz / 2) / reference_mhz;
                    if fbdiv < self.fbdiv_min as u64 || fbdiv > self.fbdiv_max as u64 {
                        continue;
                    }

                    let vco_numerator = reference_mhz * fbdiv;
                    if vco_numerator < self.vco_min_mhz as u64 * refdiv as u64
                        || vco_numerator > self.vco_max_mhz as u64 * refdiv as u64
                        || (refdiv == 1 && vco_numerator > self.refdiv_one_vco_max_mhz as u64)
                    {
                        continue;
                    }

                    let target_numerator = target_mhz as u64 * denominator;
                    let error_numerator = vco_numerator.abs_diff(target_numerator);
                    let candidate = FourDividerPll {
                        refdiv,
                        fbdiv: fbdiv as u16,
                        postdiv1,
                        postdiv2,
                    };
                    let strictly_better = match best {
                        None => true,
                        Some((best_error, best_denominator, _)) => {
                            (error_numerator as u128) * (best_denominator as u128)
                                < (best_error as u128) * (denominator as u128)
                        }
                    };
                    if strictly_better {
                        best = Some((error_numerator, denominator, candidate));
                    }
                }
            }
        }
        best.and_then(|(error_numerator, denominator, candidate)| {
            let error_millimhz_numerator = error_numerator as u128 * 1_000;
            let ceiling_millimhz_numerator =
                self.max_error_millimhz_exclusive as u128 * denominator as u128;
            (error_millimhz_numerator < ceiling_millimhz_numerator).then_some(candidate)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SPEC: FourDividerPllSearchSpec = FourDividerPllSearchSpec {
        reference_mhz: 25,
        refdiv_order: [2, 1],
        fbdiv_min: 16,
        fbdiv_max: 250,
        postdiv_min: 1,
        postdiv_max: 7,
        vco_min_mhz: 2_000,
        vco_max_mhz: 3_200,
        refdiv_one_vco_max_mhz: 3_125,
        max_error_millimhz_exclusive: 10_000,
    };

    #[test]
    fn first_exact_candidate_is_stable() {
        assert_eq!(
            TEST_SPEC.resolve(525),
            Some(FourDividerPll {
                refdiv: 2,
                fbdiv: 168,
                postdiv1: 4,
                postdiv2: 1,
            })
        );
    }

    #[test]
    fn nearest_feedback_ties_retain_the_first_postdivider_tuple() {
        // 674 MHz rounds to 675 MHz through several tuples. The strict-better
        // rule retains the first one in vendor post-divider order.
        assert_eq!(
            TEST_SPEC.resolve(674),
            Some(FourDividerPll {
                refdiv: 2,
                fbdiv: 162,
                postdiv1: 3,
                postdiv2: 1,
            })
        );
    }

    #[test]
    fn every_resolved_candidate_obeys_the_search_envelope() {
        assert_eq!(TEST_SPEC.resolve(40), None);
        for target in 400..=900 {
            let candidate = TEST_SPEC.resolve(target).unwrap();
            assert!((16..=250).contains(&candidate.fbdiv));
            assert!((1..=7).contains(&candidate.postdiv1));
            assert!((1..=7).contains(&candidate.postdiv2));
            let vco = candidate.vco_mhz(25).unwrap();
            assert!((2_000..=3_200).contains(&vco));
            if candidate.refdiv == 1 {
                assert!(vco <= 3_125);
            }
        }
    }

    #[test]
    fn strict_vendor_error_ceiling_refuses_boundary_candidates() {
        assert!(TEST_SPEC.resolve(1_990).is_none());
        assert!(TEST_SPEC.resolve(1_991).is_some());
        assert!(TEST_SPEC.resolve(3_134).is_some());
        assert!(TEST_SPEC.resolve(3_135).is_none());
    }

    #[test]
    fn invalid_envelopes_fail_closed() {
        assert!(TEST_SPEC.resolve(0).is_none());
        assert!(TEST_SPEC.with_reference_mhz(0).resolve(525).is_none());
        assert!(FourDividerPllSearchSpec {
            refdiv_order: [2, 0],
            ..TEST_SPEC
        }
        .resolve(525)
        .is_none());
        assert!(FourDividerPllSearchSpec {
            max_error_millimhz_exclusive: 0,
            ..TEST_SPEC
        }
        .resolve(525)
        .is_none());
    }
}
