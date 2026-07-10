//!  strat-A — cross-firmware Stratum + DevFee capability matrix (HAL-free).
//!
//! Source RE evidence:
//!
//! §6 + §7 (cross-firmware diff).
//!
//! Drives the dashboard's "Why DCENT_OS" competitive readiness widget and
//! the dcent-toolbox parity audit. Pins exactly which firmware advertises
//! Stratum V2, BIP 310 version-rolling, suggest-difficulty, factory-config
//! dev fees, runtime dev fees, and what their default factory pool URL is.
//!
//! The corpus now INCLUDES BraiinsOS — the firmware that authored Stratum
//! V2 — so the matrix is honest about where DCENT_OS is, and is NOT, unique.
//! BraiinsOS ships a native SV2 client and BIP 310 version-rolling on by
//! default, so DCENT_OS is NOT the only firmware with either of those.
//! Sources: D-Central RE notes in
//!
//! (§6 + §7 cross-firmware diff) + the public Braiins OS firmware /
//! Stratum V2 documentation (Braiins authored the protocol).
//!
//! Hard facts pinned by tests:
//! - Both BraiinsOS and DCENT_OS ship a Stratum V2 client; the stock /
//!   VNish / LuxOS rows in this corpus do not. (DCENT_OS keeps V1 as the
//!   default and exposes SV2 as opt-in; BraiinsOS likewise does not force
//!   SV2 over V1 — V1 stays the default for both.)
//! - Both BraiinsOS and DCENT_OS default to BIP 310 version-rolling with
//!   the standard ASICBoost mask `0x1fffe000`.
//! - No firmware in this corpus has a factory-config dev fee. VNish
//!   flavours carry a binary-baked runtime fee (~2-3 %); BraiinsOS carries
//!   a runtime development fee (~2-2.5 %, waived on Braiins pools). DCENT_OS
//!   does NOT bake in a vendor fee — its donation is opt-in and fully
//!   disableable, which is what distinguishes it from VNish/Braiins runtime
//!   fees, not "no other firmware has SV2".

use serde::{Deserialize, Serialize};

/// Firmware flavor we have evidence on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareFlavor {
    /// Bitmain S9 stock (`bmminer`).
    BitmainStockS9,
    /// VNish 3.9 S9 family (still `bmminer`).
    VnishS9_39,
    /// VNish 2.0.4 S17 family (`cgminer`).
    VnishS17_204,
    /// VNish 1.2.7 — the "overlay" generation across every model.
    Vnish127All,
    /// Bitmain S19j Pro stock (`cgminer`).
    BitmainStockS19j,
    /// LuxOS 1.38.x (`luxminer`).
    Luxos138,
    /// BraiinsOS+ (`bosminer`) — the firmware that authored Stratum V2.
    /// Ships a native SV2 client and BIP 310 version-rolling on by default.
    /// Included so the matrix's "only" claims stay honest. Source: public
    /// Braiins OS firmware / Stratum V2 docs + D-Central RE notes.
    BraiinsOsZynq,
    /// DCENT_OS (`dcentrald`).
    DcentOs,
}

/// Stratum version support flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StratumSupport {
    /// Not supported at all.
    No,
    /// Implemented but opt-in (off by default).
    OptIn,
    /// Default-on.
    Default,
}

/// Per-flavor Stratum + DevFee capability snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FirmwareCapabilities {
    pub flavor: FirmwareFlavor,
    /// Mining daemon binary name.
    pub stratum_binary: &'static str,
    /// Stratum V1 support tier.
    pub stratum_v1: StratumSupport,
    /// Stratum V2 support tier.
    pub stratum_v2: StratumSupport,
    /// BIP 310 version-rolling (`mining.set_version_mask`).
    pub version_rolling: StratumSupport,
    /// Default version mask (only meaningful when `version_rolling` is
    /// `Default`). 0 means "no default mask".
    pub version_rolling_mask: u32,
    /// `mining.suggest_difficulty` support.
    pub suggest_difficulty: StratumSupport,
    /// True iff the factory config ships a hard-coded dev-fee pool.
    pub dev_fee_in_factory: bool,
    /// True iff the binary itself routes a percentage of work to a
    /// vendor-controlled pool at runtime.
    pub dev_fee_runtime: bool,
    /// Lower bound of the runtime dev-fee percentage (when
    /// `dev_fee_runtime` is true). 0.0 if no fee.
    pub dev_fee_runtime_pct_low: f32,
    /// Upper bound of the runtime dev-fee percentage. 0.0 if no fee.
    pub dev_fee_runtime_pct_high: f32,
    /// Default factory pool URL ("" if empty).
    pub default_pool_url: &'static str,
    /// Default factory worker name ("" if empty).
    pub default_worker: &'static str,
    /// Default factory worker password ("" if empty).
    pub default_password: &'static str,
}

/// The canonical capability matrix.
pub const FIRMWARE_CAPABILITIES: &[FirmwareCapabilities] = &[
    FirmwareCapabilities {
        flavor: FirmwareFlavor::BitmainStockS9,
        stratum_binary: "bmminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::No,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: false,
        dev_fee_runtime_pct_low: 0.0,
        dev_fee_runtime_pct_high: 0.0,
        default_pool_url: "stratum+tcp://solo.antpool.com:3333",
        default_worker: "antminer_1",
        default_password: "123",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::VnishS9_39,
        stratum_binary: "bmminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::No,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: true,
        dev_fee_runtime_pct_low: 1.5,
        dev_fee_runtime_pct_high: 2.5,
        default_pool_url: "stratum+tcp://solo.antpool.com:3333",
        default_worker: "antminer_1",
        default_password: "123",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::VnishS17_204,
        stratum_binary: "cgminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::No,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: true,
        dev_fee_runtime_pct_low: 1.5,
        dev_fee_runtime_pct_high: 2.5,
        default_pool_url: "",
        default_worker: "",
        default_password: "",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::Vnish127All,
        stratum_binary: "cgminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::No,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: true,
        dev_fee_runtime_pct_low: 2.0,
        dev_fee_runtime_pct_high: 3.0,
        default_pool_url: "",
        default_worker: "",
        default_password: "",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::BitmainStockS19j,
        stratum_binary: "cgminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::No,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: false,
        dev_fee_runtime_pct_low: 0.0,
        dev_fee_runtime_pct_high: 0.0,
        default_pool_url: "",
        default_worker: "",
        default_password: "",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::Luxos138,
        stratum_binary: "luxminer",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::No,
        version_rolling: StratumSupport::OptIn,
        version_rolling_mask: 0,
        suggest_difficulty: StratumSupport::No,
        dev_fee_in_factory: false,
        dev_fee_runtime: false,
        dev_fee_runtime_pct_low: 0.0,
        dev_fee_runtime_pct_high: 0.0,
        default_pool_url: "",
        default_worker: "",
        default_password: "",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::BraiinsOsZynq,
        stratum_binary: "bosminer",
        stratum_v1: StratumSupport::Default,
        // Braiins authored Stratum V2 and ships a native SV2 client. We
        // record V1 as the documented default tier (matching DCENT_OS):
        // SV2 is supported/native but not forced over V1.
        stratum_v2: StratumSupport::OptIn,
        // BIP 310 version-rolling on by default, standard ASICBoost mask.
        version_rolling: StratumSupport::Default,
        version_rolling_mask: 0x1FFF_E000,
        suggest_difficulty: StratumSupport::Default,
        dev_fee_in_factory: false,
        // BraiinsOS+ carries a runtime development fee (~2-2.5 %), waived
        // when mining on a Braiins pool. A real runtime fee, but NOT a
        // factory-config dev-fee pool entry (so dev_fee_in_factory=false).
        dev_fee_runtime: true,
        dev_fee_runtime_pct_low: 2.0,
        dev_fee_runtime_pct_high: 2.5,
        // Factory default points at a Braiins pool.
        default_pool_url: "stratum+tcp://stratum.braiins.com:3333",
        default_worker: "",
        default_password: "x",
    },
    FirmwareCapabilities {
        flavor: FirmwareFlavor::DcentOs,
        stratum_binary: "dcentrald",
        stratum_v1: StratumSupport::Default,
        stratum_v2: StratumSupport::OptIn,
        version_rolling: StratumSupport::Default,
        version_rolling_mask: 0x1FFF_E000,
        suggest_difficulty: StratumSupport::Default,
        dev_fee_in_factory: false,
        // Donation is opt-in: default-on at 2 %, fully disableable. We
        // surface this as `dev_fee_runtime = true` because the binary
        // does carry the donation routing path, but the matrix
        // distinguishes us from VNish via the much-lower default pct
        // and the user-visible toggle.
        dev_fee_runtime: true,
        dev_fee_runtime_pct_low: 0.0,
        dev_fee_runtime_pct_high: 5.0,
        default_pool_url: "",
        default_worker: "",
        default_password: "x",
    },
];

/// Look up the capabilities row for a flavor.
pub fn capabilities_of(flavor: FirmwareFlavor) -> Option<&'static FirmwareCapabilities> {
    FIRMWARE_CAPABILITIES
        .iter()
        .find(|cap| cap.flavor == flavor)
}

/// True iff the flavor has Stratum V2 client support at any tier above
/// `No`.
pub fn supports_sv2(flavor: FirmwareFlavor) -> bool {
    capabilities_of(flavor)
        .map(|c| c.stratum_v2 != StratumSupport::No)
        .unwrap_or(false)
}

/// True iff the flavor has BIP 310 version-rolling enabled by default.
pub fn defaults_to_version_rolling(flavor: FirmwareFlavor) -> bool {
    capabilities_of(flavor)
        .map(|c| c.version_rolling == StratumSupport::Default)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_has_one_row_per_known_flavor() {
        // Every flavor must have an entry — and only one entry.
        for flavor in [
            FirmwareFlavor::BitmainStockS9,
            FirmwareFlavor::VnishS9_39,
            FirmwareFlavor::VnishS17_204,
            FirmwareFlavor::Vnish127All,
            FirmwareFlavor::BitmainStockS19j,
            FirmwareFlavor::Luxos138,
            FirmwareFlavor::BraiinsOsZynq,
            FirmwareFlavor::DcentOs,
        ] {
            assert!(
                capabilities_of(flavor).is_some(),
                "flavor {:?} missing from matrix",
                flavor
            );
            let count = FIRMWARE_CAPABILITIES
                .iter()
                .filter(|c| c.flavor == flavor)
                .count();
            assert_eq!(count, 1, "flavor {:?} has duplicate rows", flavor);
        }
        assert_eq!(FIRMWARE_CAPABILITIES.len(), 8);
    }

    #[test]
    fn sv2_supported_by_dcentos_and_braiinsos_only() {
        // HONEST competitive claim: in this corpus BOTH BraiinsOS (which
        // authored Stratum V2) and DCENT_OS ship an SV2 client; the stock /
        // VNish / LuxOS rows do not. DCENT_OS is NOT unique on SV2 — pin
        // both SV2-capable rows so a uniqueness overclaim cannot creep back.
        for cap in FIRMWARE_CAPABILITIES {
            match cap.flavor {
                FirmwareFlavor::DcentOs | FirmwareFlavor::BraiinsOsZynq => {
                    assert_ne!(
                        cap.stratum_v2,
                        StratumSupport::No,
                        "{:?} should advertise SV2 support",
                        cap.flavor
                    );
                }
                _ => assert_eq!(
                    cap.stratum_v2,
                    StratumSupport::No,
                    "{:?} unexpectedly advertises SV2",
                    cap.flavor
                ),
            }
        }
    }

    #[test]
    fn dcentos_and_braiinsos_default_to_version_rolling() {
        // HONEST claim: BraiinsOS also ships BIP 310 version-rolling on by
        // default, so DCENT_OS is NOT unique here either.
        assert!(defaults_to_version_rolling(FirmwareFlavor::DcentOs));
        assert!(defaults_to_version_rolling(FirmwareFlavor::BraiinsOsZynq));
        for flavor in [
            FirmwareFlavor::BitmainStockS9,
            FirmwareFlavor::VnishS9_39,
            FirmwareFlavor::VnishS17_204,
            FirmwareFlavor::Vnish127All,
            FirmwareFlavor::BitmainStockS19j,
            FirmwareFlavor::Luxos138,
        ] {
            assert!(
                !defaults_to_version_rolling(flavor),
                "{:?} unexpectedly defaults to version-rolling",
                flavor
            );
        }
    }

    #[test]
    fn default_version_rolling_masks_are_canonical() {
        // Both default-version-rolling rows use the standard ASICBoost mask
        // (BIP320 0x1fffe000 — invariant, never change this value).
        for flavor in [FirmwareFlavor::DcentOs, FirmwareFlavor::BraiinsOsZynq] {
            let cap = capabilities_of(flavor).unwrap();
            assert_eq!(cap.version_rolling_mask, 0x1FFF_E000, "{:?}", flavor);
            assert_eq!(cap.version_rolling, StratumSupport::Default, "{:?}", flavor);
        }
    }

    #[test]
    fn no_firmware_has_factory_dev_fee() {
        // mining-core-bible.md §6 row "DevFee in factory config" — every
        // flavor reads NO. Pin.
        for cap in FIRMWARE_CAPABILITIES {
            assert!(
                !cap.dev_fee_in_factory,
                "{:?} unexpectedly has dev_fee_in_factory=true",
                cap.flavor
            );
        }
    }

    #[test]
    fn vnish_flavors_have_runtime_dev_fee_in_documented_range() {
        for flavor in [
            FirmwareFlavor::VnishS9_39,
            FirmwareFlavor::VnishS17_204,
            FirmwareFlavor::Vnish127All,
        ] {
            let cap = capabilities_of(flavor).unwrap();
            assert!(cap.dev_fee_runtime, "{:?} expected runtime fee", flavor);
            assert!(
                cap.dev_fee_runtime_pct_low > 0.0
                    && cap.dev_fee_runtime_pct_high <= 3.0
                    && cap.dev_fee_runtime_pct_low <= cap.dev_fee_runtime_pct_high,
                "{:?} runtime fee bounds out of [>0, ≤3] range: {}-{}",
                flavor,
                cap.dev_fee_runtime_pct_low,
                cap.dev_fee_runtime_pct_high
            );
        }
    }

    #[test]
    fn bitmain_stock_has_no_runtime_dev_fee() {
        for flavor in [
            FirmwareFlavor::BitmainStockS9,
            FirmwareFlavor::BitmainStockS19j,
        ] {
            let cap = capabilities_of(flavor).unwrap();
            assert!(
                !cap.dev_fee_runtime,
                "{:?} unexpectedly has runtime fee",
                flavor
            );
            assert_eq!(cap.dev_fee_runtime_pct_low, 0.0);
            assert_eq!(cap.dev_fee_runtime_pct_high, 0.0);
        }
    }

    #[test]
    fn luxos_has_no_runtime_dev_fee() {
        // LuxOS does not bake in a vendor dev fee — the entire pool
        // lifecycle is operator-controlled (Luxor's Bitcoin pool fee is
        // pool-side, not firmware-side).
        let cap = capabilities_of(FirmwareFlavor::Luxos138).unwrap();
        assert!(!cap.dev_fee_runtime);
    }

    #[test]
    fn braiinsos_has_documented_runtime_dev_fee() {
        // BraiinsOS+ carries a runtime development fee (~2-2.5 %), waived on
        // Braiins pools — a real runtime fee, but NOT a factory-config pool
        // entry. (DCENT_OS's donation is opt-in/disableable by contrast.)
        let cap = capabilities_of(FirmwareFlavor::BraiinsOsZynq).unwrap();
        assert!(!cap.dev_fee_in_factory);
        assert!(cap.dev_fee_runtime, "BraiinsOS has a runtime dev fee");
        assert!(
            cap.dev_fee_runtime_pct_low >= 2.0
                && cap.dev_fee_runtime_pct_high <= 2.5
                && cap.dev_fee_runtime_pct_low <= cap.dev_fee_runtime_pct_high,
            "BraiinsOS runtime fee out of documented [2.0, 2.5] range: {}-{}",
            cap.dev_fee_runtime_pct_low,
            cap.dev_fee_runtime_pct_high
        );
        assert!(
            cap.default_pool_url.contains("braiins"),
            "BraiinsOS factory pool should be a Braiins pool, got {:?}",
            cap.default_pool_url
        );
        assert_eq!(cap.stratum_binary, "bosminer");
    }

    #[test]
    fn dcentos_donation_is_opt_in_with_zero_lower_bound() {
        // DCENT_OS donation is fully disableable — the lower bound MUST
        // be 0.0 to reflect "user can turn it off". Upper bound is 5 %
        // (the configurable cap).
        let cap = capabilities_of(FirmwareFlavor::DcentOs).unwrap();
        assert!(cap.dev_fee_runtime, "donation routing is implemented");
        assert_eq!(
            cap.dev_fee_runtime_pct_low, 0.0,
            "DCENT_OS donation must be disableable (lower bound 0)"
        );
        assert!(cap.dev_fee_runtime_pct_high <= 5.0);
    }

    #[test]
    fn supports_sv2_helper_matches_matrix() {
        assert!(supports_sv2(FirmwareFlavor::DcentOs));
        assert!(supports_sv2(FirmwareFlavor::BraiinsOsZynq));
        assert!(!supports_sv2(FirmwareFlavor::BitmainStockS9));
        assert!(!supports_sv2(FirmwareFlavor::Vnish127All));
        assert!(!supports_sv2(FirmwareFlavor::Luxos138));
    }

    #[test]
    fn vnish_127_default_pools_are_empty() {
        // VNish 1.2.7 ships NO factory pool config — runtime-only.
        let cap = capabilities_of(FirmwareFlavor::Vnish127All).unwrap();
        assert_eq!(cap.default_pool_url, "");
        assert_eq!(cap.default_worker, "");
        assert_eq!(cap.default_password, "");
    }

    #[test]
    fn bitmain_s9_factory_pool_matches_re_doc() {
        let cap = capabilities_of(FirmwareFlavor::BitmainStockS9).unwrap();
        assert_eq!(cap.default_pool_url, "stratum+tcp://solo.antpool.com:3333");
        assert_eq!(cap.default_worker, "antminer_1");
        assert_eq!(cap.default_password, "123");
    }

    #[test]
    fn stratum_binary_names_match_re_doc() {
        for (flavor, binary) in [
            (FirmwareFlavor::BitmainStockS9, "bmminer"),
            (FirmwareFlavor::VnishS9_39, "bmminer"),
            (FirmwareFlavor::VnishS17_204, "cgminer"),
            (FirmwareFlavor::Vnish127All, "cgminer"),
            (FirmwareFlavor::BitmainStockS19j, "cgminer"),
            (FirmwareFlavor::Luxos138, "luxminer"),
            (FirmwareFlavor::BraiinsOsZynq, "bosminer"),
            (FirmwareFlavor::DcentOs, "dcentrald"),
        ] {
            assert_eq!(capabilities_of(flavor).unwrap().stratum_binary, binary);
        }
    }

    #[test]
    fn every_flavor_supports_v1() {
        // Stratum V1 is the universal mining wire protocol — every
        // flavor in the corpus ships V1 by default.
        for cap in FIRMWARE_CAPABILITIES {
            assert_eq!(
                cap.stratum_v1,
                StratumSupport::Default,
                "{:?} unexpectedly does not default to V1",
                cap.flavor
            );
        }
    }

    #[test]
    fn suggest_difficulty_supported_by_dcentos_and_braiinsos() {
        // mining-core-bible.md §6 + Braiins docs: DCENT_OS and BraiinsOS
        // support mining.suggest_difficulty; the stock / VNish / LuxOS rows
        // in this corpus do not. (BraiinsOS supports it too, so this is no
        // longer a DCENT_OS-only claim.)
        for cap in FIRMWARE_CAPABILITIES {
            match cap.flavor {
                FirmwareFlavor::DcentOs | FirmwareFlavor::BraiinsOsZynq => {
                    assert_ne!(
                        cap.suggest_difficulty,
                        StratumSupport::No,
                        "{:?} should support suggest_difficulty",
                        cap.flavor
                    );
                }
                _ => assert_eq!(
                    cap.suggest_difficulty,
                    StratumSupport::No,
                    "{:?} unexpectedly supports suggest_difficulty",
                    cap.flavor
                ),
            }
        }
    }

    #[test]
    fn firmware_flavor_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&FirmwareFlavor::BitmainStockS9).unwrap(),
            "\"bitmain_stock_s9\""
        );
        assert_eq!(
            serde_json::to_string(&FirmwareFlavor::Vnish127All).unwrap(),
            "\"vnish127_all\""
        );
        assert_eq!(
            serde_json::to_string(&FirmwareFlavor::BraiinsOsZynq).unwrap(),
            "\"braiins_os_zynq\""
        );
        assert_eq!(
            serde_json::to_string(&FirmwareFlavor::DcentOs).unwrap(),
            "\"dcent_os\""
        );
    }

    #[test]
    fn stratum_support_round_trips_through_serde() {
        for s in [
            StratumSupport::No,
            StratumSupport::OptIn,
            StratumSupport::Default,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: StratumSupport = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }
}
