//! End-to-end contract for artifacts emitted by the Buildroot image-smoke job.
//!
//! This test is ignored in ordinary host runs because it requires a freshly
//! built, CI-signed firmware tar. The image workflow supplies the artifact and
//! its ephemeral pinned public key, then exercises the same public verifier the
//! browser-upload route calls on a miner.

use dcentrald_api::ota_signature::verify_sysupgrade_bundle;
use std::path::{Path, PathBuf};

#[test]
#[ignore = "requires release artifact, public key, and expected board target env"]
fn built_release_artifact_passes_public_ota_contract() {
    let artifact = required_path("DCENT_RELEASE_ARTIFACT");
    let public_key = required_path("DCENT_RELEASE_PUBKEY_FILE");
    let expected_board_target = required_value("DCENT_EXPECTED_BOARD_TARGET");
    let expected_kernel_leaf =
        std::env::var("DCENT_EXPECTED_KERNEL_LEAF").unwrap_or_else(|_| "kernel".to_string());
    let expected_rootfs_leaf =
        std::env::var("DCENT_EXPECTED_ROOTFS_LEAF").unwrap_or_else(|_| "root".to_string());

    let bundle =
        verify_sysupgrade_bundle(&artifact, false, Some(&public_key)).unwrap_or_else(|err| {
            panic!(
                "public OTA verifier rejected built artifact '{}': {err}",
                artifact.display()
            )
        });

    bundle
        .require_authenticated_board_target(&expected_board_target)
        .unwrap_or_else(|err| {
            panic!(
                "public OTA artifact '{}' failed authenticated board-target binding: {err}",
                artifact.display()
            )
        });
    assert_eq!(
        bundle.authenticated_board_target.as_deref(),
        Some(expected_board_target.as_str()),
        "Ed25519-authenticated MANIFEST.json board_target"
    );

    assert_eq!(
        bundle
            .kernel_path
            .file_name()
            .and_then(|name| name.to_str()),
        Some(expected_kernel_leaf.as_str()),
        "manifest-resolved kernel path"
    );
    assert_eq!(
        bundle
            .rootfs_path
            .file_name()
            .and_then(|name| name.to_str()),
        Some(expected_rootfs_leaf.as_str()),
        "manifest-resolved rootfs path"
    );
}

fn required_value(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"));
    assert!(!value.trim().is_empty(), "{name} must not be empty");
    assert_eq!(
        value,
        value.trim(),
        "{name} must not contain outer whitespace"
    );
    value
}

fn required_path(name: &str) -> PathBuf {
    let value = required_value(name);
    let path = Path::new(&value);
    assert!(path.is_file(), "{name} is not a file: {}", path.display());
    path.to_path_buf()
}
