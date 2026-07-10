//!  luxos-F — LuxOS firmware-update flow DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! (515 lines; full `luxupdate` daemon RE).
//!
//! The `luxupdate` binary on LuxOS wears 4 hats per §1: boot-time
//! overlayfs mounter, luxminer supervisor/watchdog, package updater,
//! and HTTP debug server on port 9012. This module ports the typed
//! payload surface for the **updater** hat:
//! - The 4-action enum (Off/Apply/Download/Full) per §2a.
//! - The 4-platform tag enum per §2c.
//! - The lifecycle phase ordering per §2b/§2e/§2f.
//! - The integrity model per §3 — the load-bearing "no signature"
//!   finding (luxupdate ships zero ed25519/RSA/SHA256 verification;
//!   integrity is MD5 + TLS-trust-the-bucket).
//!
//! Integrity model pinned by tests:
//! - `LuxosUpdateIntegrity::Md5Only` documents that the reverse-engineered
//!   update path verifies an MD5 checksum over a TLS-fetched package and
//!   carries no ed25519/RSA/SHA-256 signature. DCENT_OS, by contrast,
//!   enforces ed25519-signed, fail-closed OTA.
//! - The `FallbackFullDownload` lifecycle phase is the brick-survival
//!   path: when patch apply fails or MD5 mismatches, luxupdate falls
//!   back to a full package download.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Action enum
// ---------------------------------------------------------------------------

/// Action applied at startup / timeout / user trigger per
/// I-update-signature.md §2a. Wire form is lowercase
/// (`update.toml` `on_user = "full"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LuxosUpdateAction {
    /// Do nothing.
    Off,
    /// Install previously-downloaded package only.
    Apply,
    /// Fetch package(s) but do not apply.
    Download,
    /// Download + apply + (if needed) reboot.
    Full,
}

// ---------------------------------------------------------------------------
// Platform tag
// ---------------------------------------------------------------------------

/// Platform identifier per §2c (CPU model parsed from `/proc/cpuinfo`).
/// One channel may serve all four platforms with per-package
/// `metadata.toml` declaring which apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LuxosUpdatePlatform {
    /// Bitmain Zynq am1/am2 (S9 / S17 / S19 / S19j Pro Zynq).
    Xilinx,
    /// Bitmain BB platform AM33XX (Cortex-A8 — S19j Pro BB).
    AM33XX,
    /// Bitmain Amlogic A113D (S19j Pro AML / S21).
    AMLOGIC,
    /// Cvitek-based control board (some WhatsMiner units).
    CVITEK,
}

// ---------------------------------------------------------------------------
// Update config (loaded from /config/luxminer.conf.d/update.toml)
// ---------------------------------------------------------------------------

/// `[update]` section of `/config/luxminer.conf.d/update.toml`.
/// Live `a lab unit` values per §2a:
/// - `source = "https://storage.googleapis.com/luxor-firmware/stable"`
/// - `timeout = 60`
/// - `on_startup = "off"`, `on_timeout = "off"`, `on_user = "full"`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LuxosUpdateConfig {
    /// Channel base URL.
    pub source: String,
    /// Per-request HTTP timeout (seconds).
    pub timeout_seconds: u32,
    /// Action at boot.
    pub on_startup: LuxosUpdateAction,
    /// Action when a fetch times out.
    pub on_timeout: LuxosUpdateAction,
    /// Action when a SIGUSR1 (operator trigger) fires.
    pub on_user: LuxosUpdateAction,
}

impl Default for LuxosUpdateConfig {
    fn default() -> Self {
        // Live `a lab unit` defaults.
        Self {
            source: LUXOS_DEFAULT_CHANNEL_URL.to_string(),
            timeout_seconds: 60,
            on_startup: LuxosUpdateAction::Off,
            on_timeout: LuxosUpdateAction::Off,
            on_user: LuxosUpdateAction::Full,
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle phases
// ---------------------------------------------------------------------------

/// One phase of the update lifecycle. Canonical order per §2b/§2e/§2f.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosUpdateLifecyclePhase {
    /// 1. `GET ${source}/channels.toml` — list channels.
    FetchChannels,
    /// 2. `GET ${source}/${channel}/index.toml` — list packages.
    FetchIndex,
    /// 3. `GET ${source}/${channel}/${package}/metadata.toml` —
    ///    platforms/flags.
    FetchMetadata,
    /// 4. `GET ${source}/${channel}/${package}/${version}/checksums`.
    FetchChecksums,
    /// 5. `GET ${source}/${channel}/${package}/${version}/${file}.patch`
    ///    (bidiff binary delta).
    FetchPatch,
    /// 6. Apply the bidiff patch to the local file.
    ApplyPatch,
    /// 7. Verify post-apply MD5 against the `checksums` entry.
    VerifyMd5,
    /// 8. Brick-survival fallback when patch fails — download full
    ///    package directly.
    FallbackFullDownload,
    /// 9. Write file to mtd11 jffs2 rootfs (write-then-rename).
    WriteRootfs,
    /// 10. Atomic rename `.tmp` → final path.
    AtomicRename,
    /// 11. Self-update path: `exec /etc/init.d/luxminer-init restart`.
    SignalRestart,
}

/// All lifecycle phases in canonical order.
pub const LUXOS_UPDATE_LIFECYCLE: &[LuxosUpdateLifecyclePhase] = &[
    LuxosUpdateLifecyclePhase::FetchChannels,
    LuxosUpdateLifecyclePhase::FetchIndex,
    LuxosUpdateLifecyclePhase::FetchMetadata,
    LuxosUpdateLifecyclePhase::FetchChecksums,
    LuxosUpdateLifecyclePhase::FetchPatch,
    LuxosUpdateLifecyclePhase::ApplyPatch,
    LuxosUpdateLifecyclePhase::VerifyMd5,
    LuxosUpdateLifecyclePhase::FallbackFullDownload,
    LuxosUpdateLifecyclePhase::WriteRootfs,
    LuxosUpdateLifecyclePhase::AtomicRename,
    LuxosUpdateLifecyclePhase::SignalRestart,
];

// ---------------------------------------------------------------------------
// Integrity model
// ---------------------------------------------------------------------------

/// Cryptographic integrity model applied to the firmware payload.
/// LuxOS ships only one variant — the load-bearing "no signature"
/// finding from §3 of the RE doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosUpdateIntegrity {
    /// MD5 checksum over a TLS-fetched package; no ed25519, RSA, or
    /// SHA-256 signature on the reverse-engineered update path.
    Md5Only,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Live `a lab unit` channel base URL per §2a literal.
pub const LUXOS_DEFAULT_CHANNEL_URL: &str = "https://storage.googleapis.com/luxor-firmware/stable";

/// HTTP debug server port per §1d.
pub const LUXOS_HTTP_DEBUG_PORT: u16 = 9012;

/// `/mnt/ramdisk` tmpfs size per §1a (per-baseline 128 MB).
pub const LUXUPDATE_RAMDISK_SIZE_MB: u32 = 128;

/// `/mnt/overlay` tmpfs size per §1a (1 MB for upper/work dirs).
pub const LUXUPDATE_OVERLAY_TMPFS_SIZE_MB: u32 = 1;

/// Hash algorithm used for patch and full-download integrity.
/// `Md5` is the only verified-on-wire algorithm per §3.
pub const LUXOS_INTEGRITY_HASH_ALGORITHM: &str = "MD5";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_round_trips_through_serde() {
        for a in [
            LuxosUpdateAction::Off,
            LuxosUpdateAction::Apply,
            LuxosUpdateAction::Download,
            LuxosUpdateAction::Full,
        ] {
            let json = serde_json::to_string(&a).unwrap();
            let back: LuxosUpdateAction = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn action_serializes_in_lowercase() {
        // §2a: `update.toml` `on_user = "full"` — lowercase wire form.
        assert_eq!(
            serde_json::to_string(&LuxosUpdateAction::Off).unwrap(),
            "\"off\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdateAction::Apply).unwrap(),
            "\"apply\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdateAction::Download).unwrap(),
            "\"download\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdateAction::Full).unwrap(),
            "\"full\""
        );
    }

    #[test]
    fn platform_round_trips_through_serde() {
        for p in [
            LuxosUpdatePlatform::Xilinx,
            LuxosUpdatePlatform::AM33XX,
            LuxosUpdatePlatform::AMLOGIC,
            LuxosUpdatePlatform::CVITEK,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: LuxosUpdatePlatform = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn platform_wire_form_matches_re_doc_literals() {
        // §2c lists the four identifiers verbatim: Xilinx, AM33XX,
        // AMLOGIC, CVITEK. Serde defaults to PascalCase / SCREAMING
        // mixing — pin each.
        assert_eq!(
            serde_json::to_string(&LuxosUpdatePlatform::Xilinx).unwrap(),
            "\"Xilinx\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdatePlatform::AM33XX).unwrap(),
            "\"AM33XX\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdatePlatform::AMLOGIC).unwrap(),
            "\"AMLOGIC\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdatePlatform::CVITEK).unwrap(),
            "\"CVITEK\""
        );
    }

    #[test]
    fn default_channel_url_matches_re_doc_literal() {
        // §2a literal string.
        assert_eq!(
            LUXOS_DEFAULT_CHANNEL_URL,
            "https://storage.googleapis.com/luxor-firmware/stable"
        );
    }

    #[test]
    fn http_debug_port_pinned_to_9012() {
        assert_eq!(LUXOS_HTTP_DEBUG_PORT, 9012);
    }

    #[test]
    fn ramdisk_sizes_pinned_per_re_doc() {
        // §1a: per-baseline 128 MB tmpfs at /mnt/ramdisk + 1 MB tmpfs
        // at /mnt/overlay.
        assert_eq!(LUXUPDATE_RAMDISK_SIZE_MB, 128);
        assert_eq!(LUXUPDATE_OVERLAY_TMPFS_SIZE_MB, 1);
    }

    #[test]
    fn config_default_matches_live_79_capture() {
        // §2a live `a lab unit` values.
        let cfg = LuxosUpdateConfig::default();
        assert_eq!(cfg.source, LUXOS_DEFAULT_CHANNEL_URL);
        assert_eq!(cfg.timeout_seconds, 60);
        assert_eq!(cfg.on_startup, LuxosUpdateAction::Off);
        assert_eq!(cfg.on_timeout, LuxosUpdateAction::Off);
        assert_eq!(cfg.on_user, LuxosUpdateAction::Full);
    }

    #[test]
    fn config_round_trips_through_serde() {
        let original = LuxosUpdateConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let back: LuxosUpdateConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn lifecycle_phases_count_and_order_pinned() {
        assert_eq!(LUXOS_UPDATE_LIFECYCLE.len(), 11);
        assert_eq!(
            LUXOS_UPDATE_LIFECYCLE[0],
            LuxosUpdateLifecyclePhase::FetchChannels
        );
        assert_eq!(
            *LUXOS_UPDATE_LIFECYCLE.last().unwrap(),
            LuxosUpdateLifecyclePhase::SignalRestart
        );
    }

    #[test]
    fn fallback_full_download_follows_verify_md5() {
        // §2e: "On md5 mismatch OR patch apply failure: fall back to
        // downloading the full package directly". Pin VerifyMd5 →
        // FallbackFullDownload ordering — this is the brick-survival
        // path.
        let pos = |p: LuxosUpdateLifecyclePhase| {
            LUXOS_UPDATE_LIFECYCLE
                .iter()
                .position(|x| *x == p)
                .expect("phase missing")
        };
        assert!(
            pos(LuxosUpdateLifecyclePhase::VerifyMd5)
                < pos(LuxosUpdateLifecyclePhase::FallbackFullDownload)
        );
    }

    #[test]
    fn fetch_phases_precede_apply_phases() {
        // FetchChannels < FetchIndex < FetchMetadata < FetchChecksums <
        // FetchPatch < ApplyPatch — the canonical fetch-then-apply
        // ordering.
        let pos = |p: LuxosUpdateLifecyclePhase| {
            LUXOS_UPDATE_LIFECYCLE.iter().position(|x| *x == p).unwrap()
        };
        assert!(
            pos(LuxosUpdateLifecyclePhase::FetchChannels)
                < pos(LuxosUpdateLifecyclePhase::FetchIndex)
        );
        assert!(
            pos(LuxosUpdateLifecyclePhase::FetchIndex)
                < pos(LuxosUpdateLifecyclePhase::FetchMetadata)
        );
        assert!(
            pos(LuxosUpdateLifecyclePhase::FetchMetadata)
                < pos(LuxosUpdateLifecyclePhase::FetchChecksums)
        );
        assert!(
            pos(LuxosUpdateLifecyclePhase::FetchChecksums)
                < pos(LuxosUpdateLifecyclePhase::FetchPatch)
        );
        assert!(
            pos(LuxosUpdateLifecyclePhase::FetchPatch) < pos(LuxosUpdateLifecyclePhase::ApplyPatch)
        );
    }

    #[test]
    fn write_phases_precede_signal_restart() {
        let pos = |p: LuxosUpdateLifecyclePhase| {
            LUXOS_UPDATE_LIFECYCLE.iter().position(|x| *x == p).unwrap()
        };
        assert!(
            pos(LuxosUpdateLifecyclePhase::WriteRootfs)
                < pos(LuxosUpdateLifecyclePhase::AtomicRename)
        );
        assert!(
            pos(LuxosUpdateLifecyclePhase::AtomicRename)
                < pos(LuxosUpdateLifecyclePhase::SignalRestart)
        );
    }

    #[test]
    fn integrity_only_md5_only_variant_exists() {
        // §3: the ENTIRE LuxOS integrity story is MD5 + TLS-trust-
        // the-bucket. NO ed25519, NO RSA, NO SHA-256. Pin so a
        // refactor doesn't accidentally invent a "Signed" variant
        // that doesn't exist on the wire.
        let only = LuxosUpdateIntegrity::Md5Only;
        let json = serde_json::to_string(&only).unwrap();
        assert_eq!(json, "\"md5_only\"");
        let back: LuxosUpdateIntegrity = serde_json::from_str(&json).unwrap();
        assert_eq!(only, back);
        // Verify hash-algorithm constant is MD5.
        assert_eq!(LUXOS_INTEGRITY_HASH_ALGORITHM, "MD5");
    }

    #[test]
    fn lifecycle_phase_round_trips_through_serde() {
        for p in LUXOS_UPDATE_LIFECYCLE.iter().copied() {
            let json = serde_json::to_string(&p).unwrap();
            let back: LuxosUpdateLifecyclePhase = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn lifecycle_phase_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosUpdateLifecyclePhase::FetchChannels).unwrap(),
            "\"fetch_channels\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdateLifecyclePhase::FallbackFullDownload).unwrap(),
            "\"fallback_full_download\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosUpdateLifecyclePhase::SignalRestart).unwrap(),
            "\"signal_restart\""
        );
    }

    #[test]
    fn config_decodes_canonical_toml_shape() {
        // Decode a JSON serialization that mirrors the live `a lab unit`
        // update.toml shape — operator-facing config round-trip.
        let raw = r#"{
            "source": "https://storage.googleapis.com/luxor-firmware/stable",
            "timeout_seconds": 60,
            "on_startup": "off",
            "on_timeout": "off",
            "on_user": "full"
        }"#;
        let cfg: LuxosUpdateConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.timeout_seconds, 60);
        assert_eq!(cfg.on_startup, LuxosUpdateAction::Off);
        assert_eq!(cfg.on_timeout, LuxosUpdateAction::Off);
        assert_eq!(cfg.on_user, LuxosUpdateAction::Full);
    }

    #[test]
    fn lifecycle_chain_count_matches_documented_phases() {
        // Sanity: 11 distinct phases, no duplicates.
        use std::collections::HashSet;
        let mut seen: HashSet<LuxosUpdateLifecyclePhase> = HashSet::new();
        for p in LUXOS_UPDATE_LIFECYCLE.iter().copied() {
            assert!(seen.insert(p), "duplicate lifecycle phase {:?}", p);
        }
        assert_eq!(seen.len(), 11);
    }

    #[test]
    fn no_action_is_destructive_by_default() {
        // Every action defaults to `Off` — operator MUST opt in to
        // any update behavior. Pin so a refactor doesn't silently
        // change a default to `Full`.
        let cfg = LuxosUpdateConfig::default();
        assert_eq!(cfg.on_startup, LuxosUpdateAction::Off);
        assert_eq!(cfg.on_timeout, LuxosUpdateAction::Off);
        // on_user defaults to Full because it's operator-triggered
        // (SIGUSR1) — different from the auto-actions.
        assert_eq!(cfg.on_user, LuxosUpdateAction::Full);
    }

    #[test]
    fn platform_count_matches_re_doc_4_identifiers() {
        // §2c: "four platform identifiers: Xilinx, AM33XX, AMLOGIC,
        // CVITEK". Pin the full set.
        let all = [
            LuxosUpdatePlatform::Xilinx,
            LuxosUpdatePlatform::AM33XX,
            LuxosUpdatePlatform::AMLOGIC,
            LuxosUpdatePlatform::CVITEK,
        ];
        assert_eq!(all.len(), 4);
    }
}
