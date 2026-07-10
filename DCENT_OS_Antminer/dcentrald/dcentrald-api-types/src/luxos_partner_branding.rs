//!  luxos-I — LuxOS partner / white-label branding system DTOs
//! (HAL-free).
//!
//! Source RE evidence:
//!
//! §8 (lines 324-343).
//!
//! LuxOS supports OEM rebranding via 5 documented files. The runtime
//! contract: an OEM ships LuxOS to their customers with their own
//! `partner_info.toml` baked in (`partner_id = "foundry"`) and their
//! own branding PNGs replacing Luxor logos in the dashboard SPA.
//!
//! HAZARD pinned by tests:
//! - **This is NOT a license check.** There's no signature on
//!   `partner_info.toml`; the unit runs fine if the file is missing.
//!   `uninstall.sh` deletes it freely. The binary handles "Failed to
//!   load partner info" silently — runs as default-LuxOS.
//! - The PNG branding assets are NOT base64-inlined into the React
//!   SPA — they're served as separate files so an OEM can rebrand
//!   in-place without rebuilding the SPA.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// PartnerInfo struct
// ---------------------------------------------------------------------------

/// `PartnerInfo` per RE doc §8a item 4: single-field serde-derived
/// struct (binary strings: `struct PartnerInfo with 1 element struct
/// PartnerInfo partner_id`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LuxosPartnerInfo {
    /// Free-text partner identifier (e.g. "foundry", "compass").
    /// Used for telemetry, fee-routing default, dashboard badge,
    /// support-channel routing per §8b.
    pub partner_id: String,
}

impl LuxosPartnerInfo {
    /// True iff this PartnerInfo is the empty default (i.e. running
    /// as default-LuxOS, no partner branded).
    pub fn is_default_luxos(&self) -> bool {
        self.partner_id.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Branding asset catalog
// ---------------------------------------------------------------------------

/// One of the 5 documented files involved in the branding system per
/// §8a.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosBrandingAsset {
    /// `/config/partner_info.toml` — text config, contains
    /// `partner_id`. Wiped by `uninstall.sh`. Located on `mtd9`.
    PartnerInfoToml,
    /// `/firmware/hi-branding.png` — 294×156 RGBA PNG, 24 KB.
    /// "hi" = "header image".
    HiBrandingPng,
    /// `/firmware/pool-branding.png` — 294×156 RGBA PNG, 24 KB.
    PoolBrandingPng,
}

impl LuxosBrandingAsset {
    /// Filesystem path on a running LuxOS unit.
    pub fn path(&self) -> &'static str {
        match self {
            Self::PartnerInfoToml => "/config/partner_info.toml",
            Self::HiBrandingPng => "/firmware/hi-branding.png",
            Self::PoolBrandingPng => "/firmware/pool-branding.png",
        }
    }

    /// True iff this asset is wiped by `uninstall.sh` (per §8a + §7e).
    pub fn wiped_on_uninstall(&self) -> bool {
        // partner_info.toml is in `/config` and is explicitly listed
        // in uninstall.sh's rm -f sequence. The PNGs are in `/firmware`
        // (the rootfs) which is wiped by `flash_erase /dev/mtd11`.
        match self {
            Self::PartnerInfoToml => true,
            Self::HiBrandingPng | Self::PoolBrandingPng => true,
        }
    }

    /// Pixel dimensions for branding PNGs. Returns `None` for the
    /// TOML config file.
    pub fn png_dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Self::PartnerInfoToml => None,
            Self::HiBrandingPng | Self::PoolBrandingPng => Some((294, 156)),
        }
    }

    /// File-format type ("toml" / "png").
    pub fn format(&self) -> &'static str {
        match self {
            Self::PartnerInfoToml => "toml",
            Self::HiBrandingPng | Self::PoolBrandingPng => "png",
        }
    }
}

/// All 3 documented branding assets in stable iteration order.
pub const ALL_BRANDING_ASSETS: &[LuxosBrandingAsset] = &[
    LuxosBrandingAsset::PartnerInfoToml,
    LuxosBrandingAsset::HiBrandingPng,
    LuxosBrandingAsset::PoolBrandingPng,
];

/// Canonical PNG dimensions per §8a (294×156 RGBA, 24 KB).
pub const LUXOS_BRANDING_PNG_WIDTH: u32 = 294;
pub const LUXOS_BRANDING_PNG_HEIGHT: u32 = 156;

/// Startup log message emitted when partner_info.toml is found.
/// Per §8a item 5: `"Partner ID loaded: <id>"`.
pub const LUXOS_PARTNER_LOADED_LOG_PREFIX: &str = "Partner ID loaded: ";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partner_info_default_is_empty_string() {
        let info = LuxosPartnerInfo::default();
        assert!(info.partner_id.is_empty());
        assert!(info.is_default_luxos());
    }

    #[test]
    fn partner_info_with_id_is_not_default_luxos() {
        let info = LuxosPartnerInfo {
            partner_id: "foundry".to_string(),
        };
        assert!(!info.is_default_luxos());
        assert_eq!(info.partner_id, "foundry");
    }

    #[test]
    fn partner_info_round_trips_through_serde() {
        let original = LuxosPartnerInfo {
            partner_id: "compass".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosPartnerInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn partner_info_serializes_with_snake_case_field() {
        // Pin the wire-form field name verbatim — luxminer reads
        // `partner_id` from TOML and writes it through serde.
        let info = LuxosPartnerInfo {
            partner_id: "test".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("partner_id").is_some());
        assert!(json.get("partnerId").is_none());
    }

    #[test]
    fn branding_asset_paths_match_re_doc_literals() {
        // §8a item 1-3.
        assert_eq!(
            LuxosBrandingAsset::PartnerInfoToml.path(),
            "/config/partner_info.toml"
        );
        assert_eq!(
            LuxosBrandingAsset::HiBrandingPng.path(),
            "/firmware/hi-branding.png"
        );
        assert_eq!(
            LuxosBrandingAsset::PoolBrandingPng.path(),
            "/firmware/pool-branding.png"
        );
    }

    #[test]
    fn branding_png_dimensions_match_re_doc_anchors() {
        // §8a: 294×156 RGBA PNG, 24 KB.
        assert_eq!(LUXOS_BRANDING_PNG_WIDTH, 294);
        assert_eq!(LUXOS_BRANDING_PNG_HEIGHT, 156);
        for asset in [
            LuxosBrandingAsset::HiBrandingPng,
            LuxosBrandingAsset::PoolBrandingPng,
        ] {
            assert_eq!(
                asset.png_dimensions(),
                Some((LUXOS_BRANDING_PNG_WIDTH, LUXOS_BRANDING_PNG_HEIGHT))
            );
            assert_eq!(asset.format(), "png");
        }
        // The TOML asset has no dimensions.
        assert!(LuxosBrandingAsset::PartnerInfoToml
            .png_dimensions()
            .is_none());
        assert_eq!(LuxosBrandingAsset::PartnerInfoToml.format(), "toml");
    }

    #[test]
    fn all_branding_assets_wiped_on_uninstall() {
        // §7e + §8a: partner_info.toml is rm'd by uninstall.sh; the
        // PNGs are in /firmware which is flash_erase'd at mtd11.
        for asset in ALL_BRANDING_ASSETS.iter().copied() {
            assert!(
                asset.wiped_on_uninstall(),
                "{:?} should be wiped on uninstall",
                asset
            );
        }
    }

    #[test]
    fn catalog_count_matches_re_doc() {
        // §8a documents exactly 3 filesystem assets (the other 2 items
        // — luxminer struct + log message — aren't filesystem-side).
        assert_eq!(ALL_BRANDING_ASSETS.len(), 3);
    }

    #[test]
    fn partner_loaded_log_prefix_pinned() {
        // §8a item 5: `"Partner ID loaded: <id>"`. Pin the prefix
        // verbatim so log-watcher tooling can match it.
        assert_eq!(LUXOS_PARTNER_LOADED_LOG_PREFIX, "Partner ID loaded: ");
    }

    #[test]
    fn missing_partner_info_runs_as_default_luxos() {
        // §8b "Crucially: this is NOT a license check. There is no
        // signature on partner_info.toml; the unit will run fine if
        // partner_info.toml is missing".
        // Pin via the default-equals-empty-id semantics.
        let missing = LuxosPartnerInfo::default();
        assert!(missing.is_default_luxos());
    }

    #[test]
    fn branding_asset_round_trips_through_serde() {
        for asset in ALL_BRANDING_ASSETS.iter().copied() {
            let json = serde_json::to_string(&asset).unwrap();
            let back: LuxosBrandingAsset = serde_json::from_str(&json).unwrap();
            assert_eq!(asset, back);
        }
    }

    #[test]
    fn branding_asset_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosBrandingAsset::PartnerInfoToml).unwrap(),
            "\"partner_info_toml\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosBrandingAsset::HiBrandingPng).unwrap(),
            "\"hi_branding_png\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosBrandingAsset::PoolBrandingPng).unwrap(),
            "\"pool_branding_png\""
        );
    }

    #[test]
    fn config_path_for_toml_lives_under_config_dir() {
        // /config/* is mtd9 jffs2 — partner_info.toml MUST be there.
        // Other documented config files in /config from §7d:
        // luxminer.toml, luxminer.toml.upgraded, last_ipaddress.config.
        assert!(LuxosBrandingAsset::PartnerInfoToml
            .path()
            .starts_with("/config/"));
    }

    #[test]
    fn firmware_path_for_pngs_lives_under_firmware_dir() {
        // /firmware/* is the busybox httpd doc root + part of the
        // rootfs that gets wiped on uninstall.
        for asset in [
            LuxosBrandingAsset::HiBrandingPng,
            LuxosBrandingAsset::PoolBrandingPng,
        ] {
            assert!(
                asset.path().starts_with("/firmware/"),
                "{:?} should be under /firmware/",
                asset
            );
        }
    }
}
