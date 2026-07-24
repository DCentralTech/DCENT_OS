//! BM1398 silicon characterization table (Antminer S19 / S19 Pro â€”
//! second-generation BM139x family on Zynq am1/am2).
//!
//! 5 discrete steps from `-2` (eco-low) to `+2` (overclock). Source
//! provenance:
//! - **`mining-bible-v1/_canonical/chip-init-sequences.md` line 95-110**
//!   â€” legacy BM1398 init and operational-baud candidate. Its inherited
//!   672-core claim is superseded by the stock NBP1901 geometry below and is
//!   retained only as a separate experimental nonce/work-time model.
//! - **`baud-switching-analysis.md` line 34** â€” BM1398 op baud
//!   6.25 Mbaud, `MiscCtrl @ 0x18 = 0x00006031`.
//! - **S19 Pro nameplate**: ~110 TH/s @ ~3,250 W (â‰ˆ 29.5 W/TH) with
//!   3 chains Ã— 114 chips = 342 chips total (per S19 Pro maintenance
//!   docs).
//! - **Reconstructed**: linear extrapolation around the operator-known
//!   nameplate point; identical pattern to bm1397/bm1387/bm1485/bm1489.
//!
//! Sweet spot at Step -2 (~2,830 W / 100 TH/s â‰ˆ 28.3 J/TH) â€” slightly
//! better than nameplate efficiency, mirroring the underclock-efficient
//! pattern across every Bitmain SHA-256 chip in this crate.
//!
//! `voltage_v` is the chain-rail voltage. BM1398 cold-boots to
//! ~14.8V open-core overshoot, then trims to 13.8V autotune target.

use crate::{Profile, ProfileSource, SiliconTable};

/// The 5 BM1398 silicon profile rows, ordered by `step`.
///
/// Voltage column is chain-rail voltage in volts. Hashrate column is
/// in TH/s summed across 3 boards Ã— 114 chips.
pub const BM1398_PROFILES: [Profile; 5] = [
    Profile {
        step: -2,
        freq_mhz: 580,
        voltage_v: 13.4,
        wall_watts: Some(2830),
        hashrate_ths: Some(100.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: -1,
        freq_mhz: 615,
        voltage_v: 13.6,
        wall_watts: Some(3050),
        hashrate_ths: Some(105.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 0,
        freq_mhz: 650,
        voltage_v: 13.8,
        wall_watts: Some(3250),
        hashrate_ths: Some(110.0), // S19 Pro nameplate
        source: ProfileSource::OperatorConfirmed,
    },
    Profile {
        step: 1,
        freq_mhz: 690,
        voltage_v: 14.0,
        wall_watts: Some(3550),
        hashrate_ths: Some(117.0),
        source: ProfileSource::Reconstructed,
    },
    Profile {
        step: 2,
        freq_mhz: 730,
        voltage_v: 14.2,
        wall_watts: Some(3870),
        hashrate_ths: Some(124.0),
        source: ProfileSource::Reconstructed,
    },
];

/// Canonical BM1398 silicon table. Default = nameplate (Step 0).
/// Sweet spot at Step -2 (~2,830 W / 100 TH/s â‰ˆ 28.3 J/TH).
pub const BM1398_TABLE: SiliconTable = SiliconTable {
    chip_family: "BM1398",
    profiles: &BM1398_PROFILES,
    default_step: 0,
    sweet_spot_step: -2,
    // S19 Pro live-probed at .129 (114 chips/chain × 3, 13.8 V chain rail);
    // cold-boot mining proven 2026-04-10 (146K nonces, 0 HW errors).
    live_status: crate::ChipStatus::LiveConfirmed,
};

/// Hash-engine groups reported by stock NBP1901 firmware.
pub const BM1398_HASH_ENGINE_GROUPS: u32 = 156;

/// Addressable small cores in each BM1398 hash-engine group.
pub const BM1398_SMALL_CORES_PER_GROUP: u32 = 4;

/// Highest small-core index used by stock NBP1901 firmware.
pub const BM1398_MAX_SMALL_CORE_INDEX: u32 = 623;

/// Evidence-backed physical small-core count per BM1398 chip.
pub const BM1398_CORES_PER_CHIP: u32 = BM1398_HASH_ENGINE_GROUPS * BM1398_SMALL_CORES_PER_GROUP;

/// Standard Antminer S19 Pro chips per chain (114 â€” extended from S17 Pro's 48).
pub const BM1398_CHIPS_PER_CHAIN_S19_PRO: u32 = 114;

/// Standard S19 Pro chain count (3).
pub const BM1398_CHAIN_COUNT_S19_PRO: u32 = 3;

/// Conservative operational baud supported by the current production driver
/// without a PLL3 source-clock transition. The held corpus contains a 6.25
/// Mbaud PLL3-derived candidate, but that belongs to a future admitted
/// board/carrier recipe rather than universal BM1398 chip identity.
pub const BM1398_OPERATIONAL_BAUD: u32 = 3_125_000;

/// Evidence-backed candidate ceiling requiring composition-specific PLL3,
/// host-UART, and FPGA-divider transition proof before runtime promotion.
pub const BM1398_EXPERIMENTAL_PLL3_BAUD: u32 = 6_250_000;

/// Canonical MiscCtrl value to write at register 0x18 to upgrade to
/// the operational baud. Identical to BM1397.
pub const BM1398_MISCCTRL_BAUD_VALUE: u32 = 0x0000_6031;

// ===========================================================================
// BM1398 four-divider PLL contract.
//
// Recovered independently from the stock NBP1901 `bmminer` and the BM1398
// repair jig: 25 MHz reference; refdiv order 2 then 1; fbdiv 16..=250;
// postdivs 1..=7 with postdiv1 >= postdiv2; VCO 2000..=3200 MHz and a
// refdiv-1 ceiling of 3125 MHz. The family stores raw post-divider values in
// register 0x08. The canonical host-safe resolver lives in
// `dcentrald_api_types::bm13xx_pll`; this module keeps the silicon-profile
// compatibility surface while delegating its search there.
// ===========================================================================

/// Reference clock for the BM1398 PLL, in MHz. Stock S19 / S19 Pro
/// crystal. Source: `dcentrald-asic/src/drivers/bm1398.rs:229`
/// (`CLKI_MHZ = 25.0`) +  §3.2.
pub const BM1398_PLL_REF_MHZ: u32 = 25;

/// Inclusive parameter range for one BM1398 PLL divider.
/// `min` and `max` are both achievable values.
///
/// Mirrors `bm1362::PllParamRange` (same crate idiom) so callers that
/// already consume the BM1362 type can read BM1398 ranges identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllParamRange {
    pub min: u16,
    pub max: u16,
}

impl PllParamRange {
    /// Return `true` if `v` is within `[min, max]` inclusive.
    pub const fn contains(self, v: u16) -> bool {
        v >= self.min && v <= self.max
    }
}

/// BM1398 PLL parameter ranges. **Four parameters** — BM1397/1398 has
/// no `user_div` (that is a BM1362/66/68/70-family addition).
///
/// | Parameter  | Min | Max | Source |
/// |------------|-----|-----|--------|
/// | `refdiv`   |   1 |   2 | driver `bm1398.rs:254` (`refdiv ∈ {1, 2}`) |
/// | `fbdiv`    |  16 | 250 | stock NBP1901 + repair-jig binary search |
/// | `postdiv1` |   1 |   7 | driver `bm1398.rs:255` (`1..=7`) |
/// | `postdiv2` |   1 |   7 | driver `bm1398.rs:256` (`1..=7`) |
///
/// The vendor search keeps the first strictly better candidate in the order
/// refdiv 2→1, postdiv2 low→high, postdiv1 low→high, with postdiv1 >=
/// postdiv2. It computes one nearest feedback divider for each post-divider
/// tuple. Values outside the box are never produced by [`pll_compute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllRanges {
    pub refdiv: PllParamRange,
    pub fbdiv: PllParamRange,
    pub postdiv1: PllParamRange,
    pub postdiv2: PllParamRange,
}

/// BM1398 PLL parameter ranges recovered independently from the stock NBP1901
/// miner and the BM1398 repair jig.
pub const BM1398_PLL_RANGES: PllRanges = PllRanges {
    refdiv: PllParamRange { min: 1, max: 2 },
    fbdiv: PllParamRange { min: 16, max: 250 },
    postdiv1: PllParamRange { min: 1, max: 7 },
    postdiv2: PllParamRange { min: 1, max: 7 },
};

pub const BM1398_PLL_VCO_MIN_MHZ: u32 = 2_000;
pub const BM1398_PLL_VCO_MAX_MHZ: u32 = 3_200;
pub const BM1398_PLL_REFDIV1_VCO_MAX_MHZ: u32 = 3_125;

/// Resolved BM1398 PLL parameter set returned by [`pll_compute`].
///
/// Field widths fit each parameter's RE'd max:
///   - `refdiv` ≤ 2  → `u8`
///   - `fbdiv`  ≤ 250 → `u16`
///   - `postdiv1`/`postdiv2` ≤ 7 → `u8`
///
/// Use [`PllParams::compute_freq_mhz`] to roundtrip back to the
/// resulting frequency, or [`PllParams::reg_value`] for the raw
/// register-`0x08` word the BM1398 driver writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PllParams {
    pub refdiv: u8,
    pub fbdiv: u16,
    pub postdiv1: u8,
    pub postdiv2: u8,
}

impl PllParams {
    /// Compute the resulting PLL output frequency (MHz, integer
    /// truncated) for these dividers at the given reference clock.
    ///
    /// `f = ref × fbdiv / (refdiv × postdiv1 × postdiv2)`. Returns `0`
    /// if any divider field is zero (defensive — shouldn't happen for
    /// params produced by [`pll_compute`], but keeps this helper
    /// panic-free in const-eval-eligible call sites).
    pub const fn compute_freq_mhz(&self, ref_mhz: u32) -> u32 {
        if self.refdiv == 0 || self.postdiv1 == 0 || self.postdiv2 == 0 {
            return 0;
        }
        let num = ref_mhz * self.fbdiv as u32;
        let den = self.refdiv as u32 * self.postdiv1 as u32 * self.postdiv2 as u32;
        num / den
    }

    pub const fn vco_mhz(&self, ref_mhz: u32) -> u32 {
        if self.refdiv == 0 {
            return 0;
        }
        ref_mhz * self.fbdiv as u32 / self.refdiv as u32
    }

    /// Encode these dividers into the BM1398 PLL0 register-`0x08` word.
    ///
    /// Bit layout (RAW postdiv, constant VCO — **NOT** the
    /// `((p-1)<<4)|(p-1)` BM1362-family encoding). Byte-exact mirror of
    /// the repair-jig encoder at VA `0x29558`:
    ///
    /// ```text
    /// (1<<30) | (FBDIV&0xFFF)<<16 | (REFDIV&0x3F)<<8
    ///         | (POSTDIV1&0x7)<<4 | (POSTDIV2&0x7)
    /// ```
    ///
    /// Bit 31 = LOCKED (read-only on silicon), bit 30 = PLLEN (always
    /// set here). Source:  §2.2 / §3,
    /// `wave6-mining/B2-s17-s19/pll.md` §2.2.
    pub const fn reg_value(&self) -> u32 {
        (1u32 << 30)
            | ((self.fbdiv as u32 & 0x0FFF) << 16)
            | ((self.refdiv as u32 & 0x3F) << 8)
            | ((self.postdiv1 as u32 & 0x7) << 4)
            | (self.postdiv2 as u32 & 0x7)
    }
}

/// Algorithmically search for a `(refdiv, fbdiv, postdiv1, postdiv2)`
/// combination that yields `target_mhz` at the given reference clock
/// `ref_mhz` (typically 25 MHz on stock S19 / S19 Pro hardware).
///
/// This delegates to the canonical no-HAL resolver recovered independently
/// from the stock NBP1901 miner and the BM1398 repair jig. It compares exact
/// rational errors and retains the first strictly better candidate.
///
/// # Formula ( §3.2 / B2 §2.2)
///
/// ```text
/// f_out = (f_refclk × fbdiv) / (refdiv × postdiv1 × postdiv2)
/// ```
///
/// # Search strategy
///
/// Vendor loop order is refdiv `2,1`, then ascending postdiv2 followed by
/// ascending postdiv1, with `postdiv1 >= postdiv2`. One nearest feedback
/// divider is evaluated for
/// each tuple and VCO constraints are enforced.
///
/// # Returns
///
/// - `Some(params)` for the closest permitted candidate.
/// - `None` when either input is zero or cannot fit the wire-sized resolver.
///
/// Pure function. No I/O, no HAL, no platform-specific code. Per the
/// rust-firmware boundary-validation rule a `target_mhz = 0` or
/// `ref_mhz = 0` trivially returns `None`.
pub fn pll_compute(target_mhz: u32, ref_mhz: u32) -> Option<PllParams> {
    let target_mhz = u16::try_from(target_mhz).ok()?;
    let ref_mhz = u16::try_from(ref_mhz).ok()?;
    let resolved = dcentrald_api_types::bm1398_protocol::BM1398_PLL_SEARCH_SPEC
        .with_reference_mhz(ref_mhz)
        .resolve(target_mhz)?;
    Some(PllParams {
        refdiv: resolved.refdiv,
        fbdiv: resolved.fbdiv,
        postdiv1: resolved.postdiv1,
        postdiv2: resolved.postdiv2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_five_steps_in_correct_range() {
        assert_eq!(BM1398_TABLE.profiles.len(), 5);
        assert_eq!(BM1398_TABLE.min_step(), -2);
        assert_eq!(BM1398_TABLE.max_step(), 2);
    }

    #[test]
    fn nameplate_default_step_anchors_s19_pro() {
        // S19 Pro nameplate: 110 TH/s @ 3,250 W â†’ â‰ˆ 29.5 W/TH.
        let default = BM1398_TABLE.default_profile().unwrap();
        assert_eq!(default.wall_watts, Some(3250));
        assert!((default.hashrate_ths.unwrap() - 110.0).abs() < 1e-3);
        let eff = default.watts_per_ths().unwrap();
        assert!(
            (28.0..=31.0).contains(&eff),
            "S19 Pro nameplate efficiency {} W/TH outside [28, 31]",
            eff
        );
    }

    #[test]
    fn pre_baked_sweet_spot_matches_computed_minimum() {
        let pre = BM1398_TABLE.sweet_spot_profile().unwrap();
        let computed = BM1398_TABLE.computed_sweet_spot().unwrap();
        assert_eq!(pre.step, computed.step);
    }

    #[test]
    fn underclocked_steps_beat_default_efficiency() {
        let default = BM1398_TABLE.default_profile().unwrap();
        let eff_def = default.watts_per_ths().unwrap();
        for step in [-2, -1] {
            let s = BM1398_TABLE.by_step(step).unwrap();
            let eff = s.watts_per_ths().unwrap();
            assert!(
                eff < eff_def,
                "step {} efficiency ({}) should beat default ({})",
                step,
                eff,
                eff_def
            );
        }
    }

    #[test]
    fn s19_pro_hardware_constants_match_re_doc() {
        assert_eq!(BM1398_HASH_ENGINE_GROUPS, 156);
        assert_eq!(BM1398_SMALL_CORES_PER_GROUP, 4);
        assert_eq!(BM1398_MAX_SMALL_CORE_INDEX + 1, BM1398_CORES_PER_CHIP);
        assert_eq!(BM1398_CORES_PER_CHIP, 624);
        assert_eq!(BM1398_CHIPS_PER_CHAIN_S19_PRO, 114);
        assert_eq!(BM1398_CHAIN_COUNT_S19_PRO, 3);
        // 114 Ã— 3 = 342 chips total per S19 Pro.
        assert_eq!(
            BM1398_CHIPS_PER_CHAIN_S19_PRO * BM1398_CHAIN_COUNT_S19_PRO,
            342
        );
    }

    #[test]
    fn shares_baud_value_with_bm1397() {
        // chip-init-sequences.md: BM1398 is "identical to BM1397 except
        // CHIP_ID byte. Same opcodes, same registers, same baud upgrade."
        assert_eq!(BM1398_OPERATIONAL_BAUD, 3_125_000);
        assert_eq!(BM1398_EXPERIMENTAL_PLL3_BAUD, 6_250_000);
        assert_eq!(BM1398_MISCCTRL_BAUD_VALUE, 0x0000_6031);
        assert_eq!(BM1398_CORES_PER_CHIP, 624);
    }

    #[test]
    fn step_voltage_increases_with_frequency() {
        for window in BM1398_PROFILES.windows(2) {
            assert!(
                window[1].voltage_v >= window[0].voltage_v,
                "voltage non-monotonic at step {}",
                window[1].step
            );
            assert!(
                window[1].freq_mhz > window[0].freq_mhz,
                "frequency non-monotonic at step {}",
                window[1].step
            );
        }
    }

    #[test]
    fn json_round_trip_preserves_profile_fields() {
        let original = BM1398_TABLE.by_step(0).unwrap();
        let json = serde_json::to_string(original).unwrap();
        let recovered: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(*original, recovered);
    }

    #[test]
    fn nameplate_voltage_sits_at_138v_autotune_target() {
        let default = BM1398_TABLE.default_profile().unwrap();
        assert!((default.voltage_v - 13.8).abs() < 1e-3);
    }

    // -----------------------------------------------------------------
    // PR-059 (2026-05-16): Algorithmic PLL compute tests. Parallel to
    // `bm1362.rs` W12.A1 tests. §3 +
    // `wave6-mining/B2-s17-s19/pll.md` §2 the BM1398 PLL is a runtime
    // 4-parameter search (refdiv/fbdiv/postdiv1/postdiv2), NOT a lookup
    // table. Test discipline (same as bm1362): pin on resulting
    // **frequency + parameter-in-range**, NOT on a specific tuple — the
    // search may resolve any of several mathematically equivalent
    // tuples for one target.
    // -----------------------------------------------------------------

    /// Assert a vendor-envelope solution and at most 1 MHz rational error.
    fn assert_vendor_roundtrip(target_mhz: u32, ref_mhz: u32) {
        let params = pll_compute(target_mhz, ref_mhz).unwrap_or_else(|| {
            panic!(
                "pll_compute({} MHz, {} MHz ref) returned None",
                target_mhz, ref_mhz
            )
        });
        assert!(
            BM1398_PLL_RANGES.refdiv.contains(params.refdiv as u16),
            "refdiv {} out of range",
            params.refdiv
        );
        assert!(
            BM1398_PLL_RANGES.fbdiv.contains(params.fbdiv),
            "fbdiv {} out of range",
            params.fbdiv
        );
        assert!(
            BM1398_PLL_RANGES.postdiv1.contains(params.postdiv1 as u16),
            "postdiv1 {} out of range",
            params.postdiv1
        );
        assert!(
            BM1398_PLL_RANGES.postdiv2.contains(params.postdiv2 as u16),
            "postdiv2 {} out of range",
            params.postdiv2
        );
        assert!(
            params.postdiv1 >= params.postdiv2,
            "postdiv1 {} < postdiv2 {} violates BM1398 vendor search constraint",
            params.postdiv1,
            params.postdiv2
        );
        let denominator = params.refdiv as u64 * params.postdiv1 as u64 * params.postdiv2 as u64;
        let achieved_millimhz = ref_mhz as u64 * params.fbdiv as u64 * 1_000 / denominator;
        assert!(
            achieved_millimhz.abs_diff(target_mhz as u64 * 1_000) <= 1_000,
            "target={target_mhz} MHz resolved outside 1 MHz: params={params:?}, achieved={achieved_millimhz} milli-MHz"
        );
        let vco = params.vco_mhz(ref_mhz);
        assert!((BM1398_PLL_VCO_MIN_MHZ..=BM1398_PLL_VCO_MAX_MHZ).contains(&vco));
        if params.refdiv == 1 {
            assert!(vco <= BM1398_PLL_REFDIV1_VCO_MAX_MHZ);
        }
    }

    // --- Canonical S19 / S19 Pro silicon-profile frequencies ---
    // (BM1398_PROFILES steps -2..+2 = 580/615/650/690/730 MHz @ 25 MHz)

    #[test]
    fn pll_compute_step_minus2_580mhz_exact() {
        assert_vendor_roundtrip(580, BM1398_PLL_REF_MHZ);
    }

    #[test]
    fn pll_compute_step_minus1_615mhz_exact() {
        assert_vendor_roundtrip(615, BM1398_PLL_REF_MHZ);
    }

    #[test]
    fn pll_compute_step0_650mhz_exact() {
        // BM1398_PROFILES step 0 (silicon-profile nameplate row).
        assert_vendor_roundtrip(650, BM1398_PLL_REF_MHZ);
    }

    #[test]
    fn pll_compute_step_plus1_690mhz_uses_closest_vendor_candidate() {
        assert_vendor_roundtrip(690, BM1398_PLL_REF_MHZ);
    }

    #[test]
    fn pll_compute_step_plus2_730mhz_uses_closest_vendor_candidate() {
        assert_vendor_roundtrip(730, BM1398_PLL_REF_MHZ);
    }

    #[test]
    fn pll_compute_s19pro_nameplate_675mhz_exact() {
        assert_vendor_roundtrip(675, BM1398_PLL_REF_MHZ);
        assert_eq!(pll_compute(675, 25).unwrap().reg_value(), 0x40A2_0231);
    }

    #[test]
    fn pll_compute_family_factory_default_400mhz_exact() {
        // BM139x+ family factory default 400 MHz per
        //  (cgminer.conf.factory:27).
        assert_vendor_roundtrip(400, BM1398_PLL_REF_MHZ);
    }

    // --- Out-of-range / edge cases (mirror bm1362) ---

    #[test]
    fn pll_compute_zero_target_returns_none() {
        assert!(pll_compute(0, 25).is_none());
    }

    #[test]
    fn pll_compute_zero_ref_returns_none() {
        assert!(pll_compute(650, 0).is_none());
    }

    #[test]
    fn pll_compute_extreme_above_range_refuses_instead_of_clamping() {
        assert!(pll_compute(9999, 25).is_none());
    }

    #[test]
    fn pll_compute_above_envelope_refuses_when_nearest_feedback_is_invalid() {
        assert!(pll_compute(5003, 25).is_none());
    }

    #[test]
    fn pll_compute_returns_params_inside_documented_ranges() {
        for target in [400_u32, 580, 615, 650, 675, 690, 730] {
            let p = pll_compute(target, 25)
                .unwrap_or_else(|| panic!("missing params for {} MHz", target));
            assert!(BM1398_PLL_RANGES.refdiv.contains(p.refdiv as u16));
            assert!(BM1398_PLL_RANGES.fbdiv.contains(p.fbdiv));
            assert!(BM1398_PLL_RANGES.postdiv1.contains(p.postdiv1 as u16));
            assert!(BM1398_PLL_RANGES.postdiv2.contains(p.postdiv2 as u16));
            assert!(p.postdiv1 >= p.postdiv2);
            let vco = p.vco_mhz(25);
            assert!((BM1398_PLL_VCO_MIN_MHZ..=BM1398_PLL_VCO_MAX_MHZ).contains(&vco));
        }
    }

    #[test]
    fn pll_compute_never_returns_zero_dividers() {
        let p = pll_compute(650, 25).unwrap();
        assert!(p.refdiv >= 1);
        assert!(p.fbdiv >= 1);
        assert!(p.postdiv1 >= 1);
        assert!(p.postdiv2 >= 1);
    }

    // --- Register-0x08 encoding (byte-exact vs the BM1398 driver) ---

    #[test]
    fn reg_value_sets_pllen_and_clears_locked() {
        // Bit 30 (PLLEN) must be set; bit 31 (LOCKED, read-only on
        // silicon) must be clear in the value we *write*. Source:
        //  §2.2 / driver bm1398.rs:285.
        let p = pll_compute(675, 25).unwrap();
        let reg = p.reg_value();
        assert_eq!(reg & (1 << 30), 1 << 30, "PLLEN bit must be set");
        assert_eq!(reg & (1 << 31), 0, "LOCKED bit must be clear");
    }

    #[test]
    fn reg_value_field_layout_matches_bm1398_driver() {
        // Pin the exact bit layout against
        // dcentrald-asic/src/drivers/bm1398.rs:285-289 (RAW postdiv,
        // NOT the BM1362-family `((p-1)<<4)|(p-1)` encoding).
        let p = PllParams {
            refdiv: 2,
            fbdiv: 162,
            postdiv1: 3,
            postdiv2: 1,
        };
        let expected: u32 = (1u32 << 30)
            | ((162u32 & 0x0FFF) << 16)
            | ((2u32 & 0x3F) << 8)
            | ((3u32 & 0x7) << 4)
            | (1u32 & 0x7);
        assert_eq!(p.reg_value(), expected);
        assert_eq!(expected, 0x40A2_0231);
        // Round-trip the encoded fields back out.
        assert_eq!((p.reg_value() >> 16) & 0x0FFF, 162);
        assert_eq!((p.reg_value() >> 8) & 0x3F, 2);
        assert_eq!((p.reg_value() >> 4) & 0x7, 3);
        assert_eq!(p.reg_value() & 0x7, 1);
    }

    #[test]
    fn reg_value_preserves_the_repair_jig_12th_fbdiv_bit() {
        let p = PllParams {
            refdiv: 1,
            fbdiv: 0x0800,
            postdiv1: 1,
            postdiv2: 1,
        };
        assert_eq!((p.reg_value() >> 16) & 0x0FFF, 0x0800);
        assert_eq!(p.reg_value(), 0x4800_0111);
    }

    #[test]
    fn compute_freq_mhz_matches_formula_and_zero_guard() {
        // f = ref × fbdiv / (refdiv × pd1 × pd2). 25×135/(1×5×1)=675.
        let p = PllParams {
            refdiv: 1,
            fbdiv: 135,
            postdiv1: 5,
            postdiv2: 1,
        };
        assert_eq!(p.compute_freq_mhz(25), 675);
        // Zero-divider sentinel — panic-free, returns 0.
        let z = PllParams {
            refdiv: 0,
            fbdiv: 135,
            postdiv1: 5,
            postdiv2: 1,
        };
        assert_eq!(z.compute_freq_mhz(25), 0);
    }

    #[test]
    fn pll_ranges_match_two_independent_vendor_binaries() {
        assert_eq!(BM1398_PLL_RANGES.fbdiv.min, 16);
        assert_eq!(BM1398_PLL_RANGES.fbdiv.max, 250);
        assert_eq!(BM1398_PLL_RANGES.refdiv.min, 1);
        assert_eq!(BM1398_PLL_RANGES.refdiv.max, 2);
        assert_eq!(BM1398_PLL_RANGES.postdiv1.max, 7);
        assert_eq!(BM1398_PLL_RANGES.postdiv2.max, 7);
        assert_eq!(BM1398_PLL_REF_MHZ, 25);
        assert_eq!(BM1398_PLL_VCO_MIN_MHZ, 2000);
        assert_eq!(BM1398_PLL_VCO_MAX_MHZ, 3200);
        assert_eq!(BM1398_PLL_REFDIV1_VCO_MAX_MHZ, 3125);
    }

    #[test]
    fn pll_param_range_contains_inclusive_endpoints() {
        assert!(BM1398_PLL_RANGES.fbdiv.contains(16));
        assert!(BM1398_PLL_RANGES.fbdiv.contains(250));
        assert!(!BM1398_PLL_RANGES.fbdiv.contains(15));
        assert!(!BM1398_PLL_RANGES.fbdiv.contains(251));
        assert!(BM1398_PLL_RANGES.refdiv.contains(1));
        assert!(BM1398_PLL_RANGES.refdiv.contains(2));
        assert!(!BM1398_PLL_RANGES.refdiv.contains(0));
        assert!(!BM1398_PLL_RANGES.refdiv.contains(3));
    }
}
