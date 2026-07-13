//! Bounded crash-durable JSON persistence for autotuner-owned artifacts.
//!
//! Fleet, binning, and per-chain profile files share one publication contract:
//! stage in a unique sibling, fsync the staged file, atomically replace the
//! target, and fsync the parent directory.  The parent directory is created
//! first, but durability of newly created ancestors remains a provisioning
//! responsibility, matching `dcentrald_common::atomic_file`.

use serde::Serialize;
use std::path::Path;

/// Profiles can contain per-chip V/F curves for several chains.  Keep the
/// ceiling explicit and comfortably above current payloads without accepting
/// an unbounded serialization supplied through an API import.
pub(crate) const MAX_AUTOTUNER_JSON_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn write_pretty(target: impl AsRef<Path>, value: &impl Serialize) -> crate::Result<()> {
    let target = target.as_ref();
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(value)?;
    dcentrald_common::atomic_file::atomic_write(
        target,
        bytes,
        dcentrald_common::atomic_file::AtomicWriteOptions::state_file(MAX_AUTOTUNER_JSON_BYTES),
    )
    .map_err(dcentrald_common::atomic_file::AtomicWriteError::into_io_error)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[derive(Serialize)]
    struct Fixture<'a> {
        name: &'a str,
        values: &'a [u8],
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "dcentos-autotuner-durable-json-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn publishes_complete_json_with_private_default_mode() {
        let dir = scratch("roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("nested/profile.json");

        write_pretty(
            &target,
            &Fixture {
                name: "miner-01",
                values: &[1, 2, 3],
            },
        )
        .unwrap();

        let parsed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(parsed["name"], "miner-01");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_symlink_target_without_mutating_referent() {
        let dir = scratch("symlink");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let referent = dir.join("referent.json");
        let target = dir.join("profile.json");
        std::fs::write(&referent, b"outside").unwrap();
        symlink(&referent, &target).unwrap();

        assert!(write_pretty(
            &target,
            &Fixture {
                name: "replacement",
                values: &[],
            },
        )
        .is_err());
        assert_eq!(std::fs::read(&referent).unwrap(), b"outside");
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
