//!  W5-A: JSON profile-bundle schema constants for downstream
//! consumers (toolbox, dashboard, REST endpoints).
//!
//! The full `ProfileBundle` lives in `dcentrald-silicon-profiles::registry`
//! (HAL-free but depends on the silicon `Profile` row type). This module
//! re-exports the schema-version constant so a no-HAL consumer can
//! validate the wire format without pulling in the silicon-profiles crate.
//!
//! See `plans/wave4-profile-import-infrastructure.md` §A for the full
//! schema definition.

use serde::{Deserialize, Serialize};

/// JSON profile-bundle schema version.
///
/// Pinned at `1` for . Bumps require a downstream-consumer
/// coordination across `dcent-toolbox`, dashboard, and any third-party
/// REST client that reads `/etc/dcentrald/profiles.d/*.json`.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

/// Drop-in profile directory used by `ProfileRegistry::load_from_disk`.
///
/// Per the spec §B "drop-in dir": `/etc/dcentrald/profiles.d/*.json`.
/// Subdirs `vendor/`, `operator/`, and `baked/` are recommended
/// convention but not enforced by the loader.
pub const PROFILE_DROP_IN_DIR: &str = "/etc/dcentrald/profiles.d";

/// Source-class hierarchy ranks (highest authority first), per
/// `plans/wave4-profile-import-infrastructure.md` §A:
///
/// | rank | label              | meaning                                     |
/// |------|--------------------|---------------------------------------------|
/// | 5    | LiveConfirmed      | proven on a real DCENT-tracked unit         |
/// | 4    | OperatorConfirmed  | operator marked it as live-tested           |
/// | 3    | VendorExtracted    | pulled verbatim from stock vendor firmware  |
/// | 2    | Reconstructed      | interpolated/extrapolated                   |
/// | 1    | Datasheet          | datasheet-only, never observed live         |
///
/// Mirrors `dcentrald_silicon_profiles::ProfileSource::rank()`. A
/// no-HAL consumer can use these constants without pulling the full
/// silicon-profiles crate.
pub const SOURCE_RANK_LIVE_CONFIRMED: u8 = 5;
pub const SOURCE_RANK_OPERATOR_CONFIRMED: u8 = 4;
pub const SOURCE_RANK_VENDOR_EXTRACTED: u8 = 3;
pub const SOURCE_RANK_RECONSTRUCTED: u8 = 2;
pub const SOURCE_RANK_DATASHEET: u8 = 1;

/// Validation error categories returned by the loader. Mirrors
/// `dcentrald_silicon_profiles::registry::ProfileLoadError` variants
/// for downstream-consumer-friendly error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileValidationCategory {
    SchemaVersionMismatch,
    VoltageOutOfRange,
    FrequencyOutOfRange,
    DuplicateStep,
    SecureBootSetTainted,
    HashcoreRootHashTainted,
    ChipMismatch,
    LiveConfirmedReplaceAttempt,
}

/// Hard-fail safety markers. Per spec §H, profiles carrying these
/// metadata flags MUST be rejected regardless of source_class.
pub const SAFETY_BLOCKLIST_SECURE_BOOT_SET: &str = "secure_boot_set_seen";
pub const SAFETY_BLOCKLIST_HASHCORE_ROOT_HASH: &str = "hashcore_root_hash_seen";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(PROFILE_SCHEMA_VERSION, 1);
    }

    #[test]
    fn source_rank_constants_match_hierarchy_doc() {
        // The hierarchy is asserted by `ProfileSource::rank()` in
        // dcentrald-silicon-profiles. Pin the constants here so a
        // refactor that swapped two ranks would fail in two places.
        assert_eq!(SOURCE_RANK_LIVE_CONFIRMED, 5);
        assert_eq!(SOURCE_RANK_OPERATOR_CONFIRMED, 4);
        assert_eq!(SOURCE_RANK_VENDOR_EXTRACTED, 3);
        assert_eq!(SOURCE_RANK_RECONSTRUCTED, 2);
        assert_eq!(SOURCE_RANK_DATASHEET, 1);
    }

    #[test]
    fn drop_in_dir_is_etc_dcentrald_profiles_d() {
        assert_eq!(PROFILE_DROP_IN_DIR, "/etc/dcentrald/profiles.d");
    }

    #[test]
    fn safety_blocklist_markers_match_metadata_field_names() {
        // These strings must equal the field names on
        // `ProfileMetadata` so dashboard/toolbox consumers can do a
        // string-keyed lookup without depending on the silicon-profiles
        // crate's own struct.
        assert_eq!(SAFETY_BLOCKLIST_SECURE_BOOT_SET, "secure_boot_set_seen");
        assert_eq!(
            SAFETY_BLOCKLIST_HASHCORE_ROOT_HASH,
            "hashcore_root_hash_seen"
        );
    }

    #[test]
    fn validation_category_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProfileValidationCategory::SchemaVersionMismatch).unwrap(),
            "\"schema_version_mismatch\""
        );
        assert_eq!(
            serde_json::to_string(&ProfileValidationCategory::SecureBootSetTainted).unwrap(),
            "\"secure_boot_set_tainted\""
        );
        assert_eq!(
            serde_json::to_string(&ProfileValidationCategory::LiveConfirmedReplaceAttempt).unwrap(),
            "\"live_confirmed_replace_attempt\""
        );
    }
}
