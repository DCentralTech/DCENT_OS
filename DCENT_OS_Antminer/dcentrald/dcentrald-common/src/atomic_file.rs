//! Crash-safe replacement and deletion of small persistent state files.
//!
//! [`atomic_write`] stages bytes in a uniquely named sibling file, applies the
//! requested metadata policy, fsyncs the file, renames it over the destination,
//! then fsyncs the parent directory. Atomic replacement is promised only for a
//! rename within one Unix filesystem; this module never falls back to
//! copy/delete. Non-Unix callers receive `Unsupported` because Windows rename
//! does not provide the required replace-existing contract.
//!
//! Errors report their stage and whether rename had already published the new
//! target. Pre-rename failures keep the old target and attempt staging cleanup.
//! The parent directory must already exist. Creating it (or its ancestors) and
//! syncing those new directory entries is outside this replacement contract.
//!
//! [`remove_file`] similarly distinguishes a completed unlink from durable
//! parent-directory publication. NotFound is idempotent success only after the
//! existing parent directory has been synced.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{fchown, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicWriteStage {
    Validate,
    InspectTarget,
    CreateTemp,
    Write,
    Flush,
    ApplyOwnership,
    ApplyMode,
    SyncFile,
    Rename,
    SyncDirectory,
    Cleanup,
}

/// Stages in a crash-durable state-file deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicRemoveStage {
    Validate,
    InspectTarget,
    Unlink,
    SyncDirectory,
}

impl fmt::Display for AtomicRemoveStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Successful durable deletion result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicRemoveOutcome {
    /// This call unlinked a regular file and synced its parent directory.
    Removed,
    /// The target was already absent and the existing parent directory was
    /// synced. This is the explicit, idempotent NotFound success contract.
    AlreadyAbsent,
}

/// Stage-aware failure from [`remove_file`].
#[derive(Debug)]
pub struct AtomicRemoveError {
    stage: AtomicRemoveStage,
    target: PathBuf,
    target_unlinked: bool,
    target_absent_observed: bool,
    source: io::Error,
}

impl AtomicRemoveError {
    pub fn stage(&self) -> AtomicRemoveStage {
        self.stage
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    /// True only when this call successfully executed the unlink before a
    /// later parent-directory sync failure.
    pub fn target_unlinked(&self) -> bool {
        self.target_unlinked
    }

    /// True when the target was observed absent (because this call unlinked it
    /// or it was already missing) but durable directory publication failed.
    pub fn target_absent_observed(&self) -> bool {
        self.target_absent_observed
    }

    /// Absence was visible in the running system, but a crash may recover the
    /// prior directory entry because the parent fsync did not complete.
    pub fn deletion_durability_uncertain(&self) -> bool {
        self.stage == AtomicRemoveStage::SyncDirectory && self.target_absent_observed
    }

    pub fn io_error(&self) -> &io::Error {
        &self.source
    }

    pub fn into_io_error(self) -> io::Error {
        let kind = self.source.kind();
        io::Error::new(kind, self)
    }
}

impl fmt::Display for AtomicRemoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "durable removal of {} failed at {}: {}",
            self.target.display(),
            self.stage,
            self.source
        )?;
        if self.target_unlinked {
            write!(
                f,
                "; target was unlinked but deletion durability is uncertain"
            )?;
        } else if self.deletion_durability_uncertain() {
            write!(
                f,
                "; target absence was observed but directory durability is uncertain"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for AtomicRemoveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl fmt::Display for AtomicWriteStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModePolicy {
    /// Preserve an existing mode, or use `fallback_mode` for a new file.
    PreserveExistingOr {
        fallback_mode: u32,
    },
    Exact(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipPolicy {
    /// Strictly preserve an existing uid/gid on Unix.
    PreserveExisting,
    CurrentProcess,
}

/// Bounded write policy. There is intentionally no unbounded default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicWriteOptions {
    max_bytes: usize,
    mode: ModePolicy,
    ownership: OwnershipPolicy,
}

impl AtomicWriteOptions {
    /// Preserve existing metadata and create a missing state file as `0600`.
    pub const fn state_file(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            mode: ModePolicy::PreserveExistingOr {
                fallback_mode: 0o600,
            },
            ownership: OwnershipPolicy::PreserveExisting,
        }
    }

    pub const fn with_mode(mut self, mode: ModePolicy) -> Self {
        self.mode = mode;
        self
    }

    pub const fn with_ownership(mut self, ownership: OwnershipPolicy) -> Self {
        self.ownership = ownership;
        self
    }

    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicWriteOutcome {
    pub bytes_written: usize,
    /// Pre-write observation, not a concurrent-writer serialization guarantee.
    pub replaced_existing: bool,
}

#[derive(Debug)]
pub struct AtomicWriteError {
    stage: AtomicWriteStage,
    target: PathBuf,
    temp_path: Option<PathBuf>,
    target_published: bool,
    cleanup_error: Option<io::Error>,
    source: io::Error,
}

impl AtomicWriteError {
    pub fn stage(&self) -> AtomicWriteStage {
        self.stage
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn temp_path(&self) -> Option<&Path> {
        self.temp_path.as_deref()
    }

    /// True when rename succeeded but parent-directory durability failed.
    pub fn target_published(&self) -> bool {
        self.target_published
    }

    pub fn cleanup_error(&self) -> Option<&io::Error> {
        self.cleanup_error.as_ref()
    }

    pub fn io_error(&self) -> &io::Error {
        &self.source
    }

    /// Wrap this stage-aware error in `io::Error` without discarding its
    /// display text or error-chain evidence.
    pub fn into_io_error(self) -> io::Error {
        let kind = self.source.kind();
        io::Error::new(kind, self)
    }
}

impl fmt::Display for AtomicWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "atomic write of {} failed at {}: {}",
            self.target.display(),
            self.stage,
            self.source
        )?;
        if self.target_published {
            write!(f, "; replacement was published before durability failed")?;
        }
        if let Some(error) = &self.cleanup_error {
            write!(f, "; staging cleanup also failed: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AtomicWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub fn atomic_write(
    target: impl AsRef<Path>,
    bytes: impl AsRef<[u8]>,
    options: AtomicWriteOptions,
) -> Result<AtomicWriteOutcome, AtomicWriteError> {
    #[cfg(unix)]
    {
        return atomic_write_with_hook(target.as_ref(), bytes.as_ref(), options, |_| Ok(()));
    }
    #[cfg(not(unix))]
    {
        let _ = (bytes, options);
        Err(plain_error(
            AtomicWriteStage::Validate,
            target.as_ref(),
            None,
            false,
            io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic replace-existing persistence is supported only on Unix",
            ),
        ))
    }
}

/// Remove a small state file and durably publish its absence.
///
/// On Unix this rejects symlinks and non-regular targets, unlinks a present
/// regular file, then fsyncs the parent directory. An already-missing target is
/// an explicit idempotent success only after the existing parent directory is
/// synced. There is no concurrent-writer serialization guarantee: callers must
/// still provide ownership of the state path.
///
/// Non-Unix callers receive [`io::ErrorKind::Unsupported`] because this module
/// does not claim a replace/delete durability contract it cannot implement.
pub fn remove_file(target: impl AsRef<Path>) -> Result<AtomicRemoveOutcome, AtomicRemoveError> {
    #[cfg(unix)]
    {
        return remove_file_with_hook(target.as_ref(), |_| Ok(()));
    }
    #[cfg(not(unix))]
    {
        Err(remove_error(
            AtomicRemoveStage::Validate,
            target.as_ref(),
            false,
            false,
            io::Error::new(
                io::ErrorKind::Unsupported,
                "crash-durable state deletion is supported only on Unix",
            ),
        ))
    }
}

#[cfg(unix)]
fn remove_file_with_hook<F>(
    target: &Path,
    mut hook: F,
) -> Result<AtomicRemoveOutcome, AtomicRemoveError>
where
    F: FnMut(AtomicRemoveStage) -> io::Result<()>,
{
    hook(AtomicRemoveStage::Validate).map_err(|source| {
        remove_error(AtomicRemoveStage::Validate, target, false, false, source)
    })?;
    target.file_name().ok_or_else(|| {
        remove_error(
            AtomicRemoveStage::Validate,
            target,
            false,
            false,
            io::Error::new(io::ErrorKind::InvalidInput, "target has no file name"),
        )
    })?;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    hook(AtomicRemoveStage::InspectTarget).map_err(|source| {
        remove_error(
            AtomicRemoveStage::InspectTarget,
            target,
            false,
            false,
            source,
        )
    })?;
    let present = match std::fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(remove_error(
                    AtomicRemoveStage::InspectTarget,
                    target,
                    false,
                    false,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "target must be a regular file, not a symlink or directory",
                    ),
                ));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(remove_error(
                AtomicRemoveStage::InspectTarget,
                target,
                false,
                false,
                source,
            ));
        }
    };

    let mut target_unlinked = false;
    if present {
        hook(AtomicRemoveStage::Unlink).map_err(|source| {
            remove_error(AtomicRemoveStage::Unlink, target, false, false, source)
        })?;
        match std::fs::remove_file(target) {
            Ok(()) => target_unlinked = true,
            // A cooperating concurrent owner may have won the deletion race.
            // We still sync the directory below before reporting idempotent
            // AlreadyAbsent success.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(remove_error(
                    AtomicRemoveStage::Unlink,
                    target,
                    false,
                    false,
                    source,
                ));
            }
        }
    }

    if let Err(source) = hook(AtomicRemoveStage::SyncDirectory)
        .and_then(|()| File::open(parent))
        .and_then(|directory| directory.sync_all())
    {
        return Err(remove_error(
            AtomicRemoveStage::SyncDirectory,
            target,
            target_unlinked,
            true,
            source,
        ));
    }

    Ok(if target_unlinked {
        AtomicRemoveOutcome::Removed
    } else {
        AtomicRemoveOutcome::AlreadyAbsent
    })
}

#[cfg(unix)]
fn atomic_write_with_hook<F>(
    target: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
    mut hook: F,
) -> Result<AtomicWriteOutcome, AtomicWriteError>
where
    F: FnMut(AtomicWriteStage) -> io::Result<()>,
{
    run_stage(&mut hook, AtomicWriteStage::Validate)
        .map_err(|source| plain_error(AtomicWriteStage::Validate, target, None, false, source))?;
    if options.max_bytes == 0 || bytes.len() > options.max_bytes {
        return Err(plain_error(
            AtomicWriteStage::Validate,
            target,
            None,
            false,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "payload is {} bytes; configured limit is {} bytes",
                    bytes.len(),
                    options.max_bytes
                ),
            ),
        ));
    }

    let _file_name = target.file_name().ok_or_else(|| {
        plain_error(
            AtomicWriteStage::Validate,
            target,
            None,
            false,
            io::Error::new(io::ErrorKind::InvalidInput, "target has no file name"),
        )
    })?;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    run_stage(&mut hook, AtomicWriteStage::InspectTarget).map_err(|source| {
        plain_error(AtomicWriteStage::InspectTarget, target, None, false, source)
    })?;
    let existing = match std::fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(plain_error(
                    AtomicWriteStage::InspectTarget,
                    target,
                    None,
                    false,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "target must be a regular file, not a symlink or directory",
                    ),
                ));
            }
            Some(metadata)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(plain_error(
                AtomicWriteStage::InspectTarget,
                target,
                None,
                false,
                source,
            ));
        }
    };

    run_stage(&mut hook, AtomicWriteStage::CreateTemp)
        .map_err(|source| plain_error(AtomicWriteStage::CreateTemp, target, None, false, source))?;
    let (temp_path, mut temp) = create_sibling_temp(parent)
        .map_err(|source| plain_error(AtomicWriteStage::CreateTemp, target, None, false, source))?;

    macro_rules! pre_publish {
        ($stage:expr, $operation:expr) => {
            if let Err(source) = run_stage(&mut hook, $stage).and_then(|()| $operation) {
                return Err(cleanup_error($stage, target, &temp_path, source, &mut hook));
            }
        };
    }

    pre_publish!(AtomicWriteStage::Write, temp.write_all(bytes));
    pre_publish!(AtomicWriteStage::Flush, temp.flush());

    #[cfg(unix)]
    if options.ownership == OwnershipPolicy::PreserveExisting {
        if let Some(metadata) = existing.as_ref() {
            pre_publish!(
                AtomicWriteStage::ApplyOwnership,
                fchown(&temp, Some(metadata.uid()), Some(metadata.gid()))
            );
        }
    }
    #[cfg(not(unix))]
    let _ = options.ownership;

    pre_publish!(
        AtomicWriteStage::ApplyMode,
        apply_mode(&temp, existing.as_ref(), options.mode)
    );
    pre_publish!(AtomicWriteStage::SyncFile, temp.sync_all());
    drop(temp);
    pre_publish!(
        AtomicWriteStage::Rename,
        std::fs::rename(&temp_path, target)
    );

    if let Err(source) = run_stage(&mut hook, AtomicWriteStage::SyncDirectory)
        .and_then(|()| File::open(parent))
        .and_then(|directory| directory.sync_all())
    {
        return Err(plain_error(
            AtomicWriteStage::SyncDirectory,
            target,
            None,
            true,
            source,
        ));
    }

    Ok(AtomicWriteOutcome {
        bytes_written: bytes.len(),
        replaced_existing: existing.is_some(),
    })
}

#[cfg(unix)]
fn run_stage<F>(hook: &mut F, stage: AtomicWriteStage) -> io::Result<()>
where
    F: FnMut(AtomicWriteStage) -> io::Result<()>,
{
    hook(stage)
}

#[cfg(unix)]
fn create_sibling_temp(parent: &Path) -> io::Result<(PathBuf, File)> {
    const MAX_COLLISION_RETRIES: usize = 128;
    let pid = std::process::id();
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for _ in 0..MAX_COLLISION_RETRIES {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        // A fixed prefix keeps the sibling name below NAME_MAX even when the
        // destination itself uses the full filename length limit.
        let mut name = OsString::from(".dcent-atomic.");
        name.push(format!("{pid}.{epoch_nanos}.{sequence}.tmp"));
        let path = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique sibling staging file after 128 attempts",
    ))
}

#[cfg(unix)]
fn apply_mode(
    file: &File,
    existing: Option<&std::fs::Metadata>,
    policy: ModePolicy,
) -> io::Result<()> {
    let mode = match policy {
        ModePolicy::PreserveExistingOr { fallback_mode } => existing
            .map(|metadata| metadata.mode() & 0o7777)
            .unwrap_or(fallback_mode),
        ModePolicy::Exact(mode) => mode,
    };
    if mode & !0o7777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid Unix file mode {mode:#o}"),
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(unix)]
fn cleanup_error<F>(
    stage: AtomicWriteStage,
    target: &Path,
    temp_path: &Path,
    source: io::Error,
    hook: &mut F,
) -> AtomicWriteError
where
    F: FnMut(AtomicWriteStage) -> io::Result<()>,
{
    let injected_cleanup_error = hook(AtomicWriteStage::Cleanup).err();
    let actual_cleanup_error = std::fs::remove_file(temp_path).err();
    AtomicWriteError {
        stage,
        target: target.to_path_buf(),
        temp_path: Some(temp_path.to_path_buf()),
        target_published: false,
        cleanup_error: injected_cleanup_error.or(actual_cleanup_error),
        source,
    }
}

fn plain_error(
    stage: AtomicWriteStage,
    target: &Path,
    temp_path: Option<PathBuf>,
    target_published: bool,
    source: io::Error,
) -> AtomicWriteError {
    AtomicWriteError {
        stage,
        target: target.to_path_buf(),
        temp_path,
        target_published,
        cleanup_error: None,
        source,
    }
}

fn remove_error(
    stage: AtomicRemoveStage,
    target: &Path,
    target_unlinked: bool,
    target_absent_observed: bool,
    source: io::Error,
) -> AtomicRemoveError {
    AtomicRemoveError {
        stage,
        target: target.to_path_buf(),
        target_unlinked,
        target_absent_observed,
        source,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_dir(label: &str) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dcentrald-common-atomic-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn options() -> AtomicWriteOptions {
        AtomicWriteOptions::state_file(1024)
    }

    fn staging_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().contains(".dcent-atomic."))
            .collect()
    }

    #[test]
    fn round_trip_and_replace_report_outcome() {
        let dir = test_dir("round-trip");
        let target = dir.join("state.toml");
        let first = atomic_write(&target, b"one", options()).unwrap();
        assert_eq!(first.bytes_written, 3);
        assert!(!first.replaced_existing);
        let second = atomic_write(&target, b"two", options()).unwrap();
        assert!(second.replaced_existing);
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert!(staging_files(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_input_fails_before_touching_target() {
        let dir = test_dir("bounded");
        let target = dir.join("state.toml");
        std::fs::write(&target, b"old").unwrap();
        let error =
            atomic_write(&target, b"too large", AtomicWriteOptions::state_file(3)).unwrap_err();
        assert_eq!(error.stage(), AtomicWriteStage::Validate);
        assert!(!error.target_published());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(staging_files(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_existing_mode_and_ownership() {
        let dir = test_dir("metadata");
        let target = dir.join("state.toml");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let before = std::fs::metadata(&target).unwrap();
        atomic_write(&target, b"new", options()).unwrap();
        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(after.mode() & 0o7777, 0o640);
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_pre_rename_failures_keep_old_target_and_clean_temp() {
        let stages = [
            AtomicWriteStage::Validate,
            AtomicWriteStage::InspectTarget,
            AtomicWriteStage::CreateTemp,
            AtomicWriteStage::Write,
            AtomicWriteStage::Flush,
            AtomicWriteStage::ApplyOwnership,
            AtomicWriteStage::ApplyMode,
            AtomicWriteStage::SyncFile,
            AtomicWriteStage::Rename,
        ];
        for failed_stage in stages {
            let dir = test_dir("fail-stage");
            let target = dir.join("state.toml");
            std::fs::write(&target, b"old").unwrap();
            let error = atomic_write_with_hook(&target, b"new", options(), |stage| {
                if stage == failed_stage {
                    Err(io::Error::other("injected failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.stage(), failed_stage);
            assert!(!error.target_published());
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            assert!(staging_files(&dir).is_empty(), "stage {failed_stage:?}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn directory_sync_failure_reports_already_published_target() {
        let dir = test_dir("dir-sync");
        let target = dir.join("state.toml");
        std::fs::write(&target, b"old").unwrap();
        let error = atomic_write_with_hook(&target, b"new", options(), |stage| {
            if stage == AtomicWriteStage::SyncDirectory {
                Err(io::Error::other("injected directory fsync failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.stage(), AtomicWriteStage::SyncDirectory);
        assert!(error.target_published());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(staging_files(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cleanup_failure_is_attached_to_primary_error() {
        let dir = test_dir("cleanup-evidence");
        let target = dir.join("state.toml");
        let error = atomic_write_with_hook(&target, b"new", options(), |stage| match stage {
            AtomicWriteStage::Write => Err(io::Error::other("primary")),
            AtomicWriteStage::Cleanup => Err(io::Error::other("cleanup")),
            _ => Ok(()),
        })
        .unwrap_err();
        assert_eq!(error.stage(), AtomicWriteStage::Write);
        assert!(error.cleanup_error().is_some());
        assert!(staging_files(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_writers_publish_one_complete_payload() {
        let dir = test_dir("concurrent");
        let target = Arc::new(dir.join("state.toml"));
        let payloads: Vec<Vec<u8>> = (0..12)
            .map(|index| format!("payload-{index:02}-{}", "x".repeat(index + 1)).into_bytes())
            .collect();
        let mut threads = Vec::new();
        for payload in payloads.clone() {
            let target = Arc::clone(&target);
            threads.push(std::thread::spawn(move || {
                atomic_write(&*target, payload, options()).unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let published = std::fs::read(&*target).unwrap();
        assert!(payloads.contains(&published));
        assert!(staging_files(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = test_dir("symlink");
        let outside = dir.join("outside.toml");
        let target = dir.join("state.toml");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &target).unwrap();
        let error = atomic_write(&target, b"new", options()).unwrap_err();
        assert_eq!(error.stage(), AtomicWriteStage::InspectTarget);
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn durable_remove_unlinks_regular_file_and_is_idempotent_for_not_found() {
        let dir = test_dir("remove-round-trip");
        let target = dir.join("state.toml");
        std::fs::write(&target, b"state").unwrap();

        assert_eq!(remove_file(&target).unwrap(), AtomicRemoveOutcome::Removed);
        assert!(!target.exists());
        assert_eq!(
            remove_file(&target).unwrap(),
            AtomicRemoveOutcome::AlreadyAbsent
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_pre_unlink_stage_failures_leave_target_present() {
        for failed_stage in [
            AtomicRemoveStage::Validate,
            AtomicRemoveStage::InspectTarget,
            AtomicRemoveStage::Unlink,
        ] {
            let dir = test_dir("remove-pre-unlink-failure");
            let target = dir.join("state.toml");
            std::fs::write(&target, b"state").unwrap();
            let error = remove_file_with_hook(&target, |stage| {
                if stage == failed_stage {
                    Err(io::Error::other("injected failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.stage(), failed_stage);
            assert!(!error.target_unlinked());
            assert!(!error.target_absent_observed());
            assert!(!error.deletion_durability_uncertain());
            assert_eq!(std::fs::read(&target).unwrap(), b"state");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn directory_sync_failure_reports_unlinked_but_not_durable() {
        let dir = test_dir("remove-dir-sync-failure");
        let target = dir.join("state.toml");
        std::fs::write(&target, b"state").unwrap();
        let error = remove_file_with_hook(&target, |stage| {
            if stage == AtomicRemoveStage::SyncDirectory {
                Err(io::Error::other("injected directory fsync failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.stage(), AtomicRemoveStage::SyncDirectory);
        assert!(error.target_unlinked());
        assert!(error.target_absent_observed());
        assert!(error.deletion_durability_uncertain());
        assert!(!target.exists());
        assert!(error.to_string().contains("unlinked"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_target_sync_failure_reports_absence_without_claiming_unlink() {
        let dir = test_dir("remove-missing-sync-failure");
        let target = dir.join("state.toml");
        let error = remove_file_with_hook(&target, |stage| {
            if stage == AtomicRemoveStage::SyncDirectory {
                Err(io::Error::other("injected directory fsync failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.stage(), AtomicRemoveStage::SyncDirectory);
        assert!(!error.target_unlinked());
        assert!(error.target_absent_observed());
        assert!(error.deletion_durability_uncertain());
        assert!(error.to_string().contains("absence was observed"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn durable_remove_rejects_symlink_and_directory_targets() {
        use std::os::unix::fs::symlink;

        let dir = test_dir("remove-type-refusal");
        let regular = dir.join("regular.toml");
        let link = dir.join("link.toml");
        let nested = dir.join("nested");
        std::fs::write(&regular, b"state").unwrap();
        symlink(&regular, &link).unwrap();
        std::fs::create_dir(&nested).unwrap();

        for target in [&link, &nested] {
            let error = remove_file(target).unwrap_err();
            assert_eq!(error.stage(), AtomicRemoveStage::InspectTarget);
            assert_eq!(error.io_error().kind(), io::ErrorKind::InvalidInput);
            assert!(!error.target_unlinked());
        }
        assert_eq!(std::fs::read(&regular).unwrap(), b"state");
        assert!(link.exists());
        assert!(nested.is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, not(unix)))]
mod non_unix_tests {
    use super::*;

    #[test]
    fn replacement_contract_fails_closed_as_unsupported() {
        let target = Path::new("must-not-be-created-by-unsupported-atomic-write.toml");
        let error = atomic_write(target, b"state", AtomicWriteOptions::state_file(1024))
            .expect_err("non-Unix replacement must be rejected");
        assert_eq!(error.stage(), AtomicWriteStage::Validate);
        assert_eq!(error.io_error().kind(), io::ErrorKind::Unsupported);
        assert!(!error.target_published());
        assert!(!target.exists());
    }

    #[test]
    fn durable_remove_contract_fails_closed_as_unsupported() {
        let target = Path::new("must-not-be-removed-on-unsupported-platform.toml");
        let error = remove_file(target).expect_err("non-Unix durable remove must be rejected");
        assert_eq!(error.stage(), AtomicRemoveStage::Validate);
        assert_eq!(error.io_error().kind(), io::ErrorKind::Unsupported);
        assert!(!error.target_unlinked());
        assert!(!error.target_absent_observed());
    }
}
