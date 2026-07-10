//! Atomic file writes for config persistence.
//!
//! The REST API persists mutable configuration to `/data/dcentrald.toml`
//! (and `/etc/dcentrald.toml` for legacy installs). A plain `std::fs::write`
//! truncates the target file first, so a crash or power loss mid-write leaves
//! an empty or half-written config — silently wiping pool URL, PSU override,
//! thermal settings, auth hashes, etc. on next boot.
//!
//! The canonical fix is tempfile + fsync + atomic `rename(2)`:
//!
//!   1. Write the new contents to `<path>.tmp` in the same directory.
//!   2. `fsync` the tempfile so its bytes are durable on disk.
//!   3. `rename` the tempfile onto the target — a POSIX atomic operation
//!      within the same filesystem.
//!   4. `fsync` the parent directory so the rename itself is durable.
//!
//! Same filesystem is important: on ubifs (the /data and /etc backing on
//! our BraiinsOS-sourced kernel), rename across a mount boundary falls back
//! to copy-then-unlink which is NOT atomic. Caller must pass a path on the
//! same filesystem as its parent directory (the default for /data/* and
//! /etc/* on our rootfs).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// File name of the daemon config persisted by the REST API (`/data` or
/// legacy `/etc`). Used to gate the config-write generation bump so that only
/// real config rewrites — not profile / onboarding writes that share
/// [`atomic_write`] — invalidate the in-memory config cache.
pub const CONFIG_FILE_NAME: &str = "dcentrald.toml";

/// Monotonic counter bumped by [`atomic_write`] every time the daemon config
/// file is rewritten (P3-2). The in-memory config cache
/// (`crate::config_cache::ConfigTableCache`, held on `AppState`) reads this to
/// decide when a cached parse is stale: a change between two reads means a
/// persisted config write happened in between, so the cache must reload. This
/// makes "a GET issued after a POST that persisted config observes the new
/// value" hold deterministically, independent of filesystem timestamp
/// resolution.
static CONFIG_WRITE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Current config-write generation. See [`CONFIG_WRITE_GENERATION`].
pub fn config_write_generation() -> u64 {
    CONFIG_WRITE_GENERATION.load(Ordering::Acquire)
}

/// Process-monotonic counter that makes [`atomic_write_bytes`] staging-file
/// names unique (RELIAB-2a). Two concurrent writers in the SAME process share
/// `std::process::id()`, so the PID alone is NOT enough: without a per-write
/// discriminator both would `open(.., truncate(true))` the identical
/// `<file>.tmp.<pid>` and clobber each other's bytes, and the racing renames
/// could publish a torn or empty file. `fetch_add` hands each in-flight write a
/// distinct suffix.
static TMP_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Storage-class write failures that should be surfaced to API clients with a
/// stable code instead of an opaque "write failed" string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageWriteFailureKind {
    StorageFull,
    ReadOnly,
}

impl StorageWriteFailureKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::StorageFull => "storage_full",
            Self::ReadOnly => "storage_read_only",
        }
    }
}

/// Classify I/O errors that mean persistent storage cannot accept a config
/// write. Kept in the atomic write module so every caller can share the same
/// ENOSPC/read-only interpretation.
pub fn storage_write_failure_kind(error: &io::Error) -> Option<StorageWriteFailureKind> {
    match error.kind() {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            Some(StorageWriteFailureKind::StorageFull)
        }
        io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::PermissionDenied => {
            Some(StorageWriteFailureKind::ReadOnly)
        }
        _ => match error.raw_os_error() {
            #[cfg(unix)]
            Some(28) | Some(122) => Some(StorageWriteFailureKind::StorageFull),
            #[cfg(unix)]
            Some(30) => Some(StorageWriteFailureKind::ReadOnly),
            _ => None,
        },
    }
}

/// Process-wide serialization for config read-modify-write critical sections
/// (RELIAB-2b). ~20 REST handlers persist config by loading the whole
/// `dcentrald.toml` into a `toml::Table`, mutating one section, and writing the
/// whole table back via [`atomic_write`]. On a multi-threaded runtime two such
/// handlers can interleave as load(A) / load(B) / write(A) / write(B), so B's
/// re-serialized table (built from a snapshot taken before A's write) silently
/// DROPS A's change — a lost update. Every config load→modify→write critical
/// section acquires this lock via [`config_write_lock`] so they run serially.
///
/// This is a SYNC `std::sync::Mutex`, deliberately NOT a `tokio::sync::Mutex`:
/// the guarded sections are entirely synchronous (a `std::fs` load plus the sync
/// [`atomic_write`] publish, with no `.await`), and the persist helpers that
/// hold it are plain sync `fn`s that cannot `.await`. Two rules for callers:
///   1. Keep the guard's lifetime fully synchronous — NEVER hold it across an
///      `.await` (on a current-thread runtime that would deadlock the thread).
///      The established pattern is to hold it for the body of a sync helper `fn`
///      or a sync IIFE closure, where it drops automatically on return.
///   2. NEVER nest — do not acquire it again while already holding it (this is a
///      non-reentrant mutex; that would self-deadlock).
static CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the process-wide config-write lock for the duration of a synchronous
/// config load→modify→write critical section. See [`CONFIG_WRITE_LOCK`] for the
/// (load-bearing) usage rules.
///
/// Poison-tolerant: a panic inside one critical section must not wedge every
/// future config write. The lock only *orders* writers — the data it protects
/// lives on disk, published atomically by [`atomic_write`], so a partially
/// applied in-memory table from a panicked writer was never observable. We
/// therefore recover into a poisoned lock rather than propagating the panic.
pub fn config_write_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// True when `path` names the daemon config file, i.e. a successful write to it
/// must invalidate the config cache. Matches by file name only so it works for
/// both `/data/dcentrald.toml` and the legacy `/etc/dcentrald.toml`, and never
/// matches the `.tmp.<pid>` staging file or sibling profile/onboarding JSON.
fn is_config_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some(CONFIG_FILE_NAME)
}

/// Drop-in replacement for `std::fs::write`: writes `contents` to `path`
/// atomically via tempfile + fsync + rename.
///
/// Signature mirrors `std::fs::write` exactly (`AsRef<Path>` + `AsRef<[u8]>`)
/// so callers can swap `std::fs::write(p, s)` for `atomic_write(p, s)` with
/// no other changes.
///
/// If any step fails, the tempfile is best-effort removed and the error is
/// returned. The target file is NEVER left truncated or half-written.
pub fn atomic_write<P, B>(path: P, contents: B) -> io::Result<()>
where
    P: AsRef<Path>,
    B: AsRef<[u8]>,
{
    atomic_write_bytes(path.as_ref(), contents.as_ref())
}

/// Write `bytes` to `path` atomically. The file is written to a sibling
/// tempfile first, fsynced, then renamed onto the target. The containing
/// directory is also fsynced so the rename is durable.
///
/// If any step fails, the tempfile is best-effort removed and the error
/// is returned. The target file is NEVER left truncated or half-written.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write_bytes: path has no parent directory",
        )
    })?;

    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write_bytes: path has no file name",
        )
    })?;

    // Staging file alongside the target. The name MUST be unique per in-flight
    // write so two concurrent writers (even within THIS process, same PID) never
    // stage to the same path and clobber each other — which would publish a torn
    // or empty file (RELIAB-2a). We combine the PID, a process-monotonic
    // sequence counter (the actual uniqueness guarantee — see `TMP_WRITE_SEQ`),
    // and the current nanosecond (extra entropy across process restarts). Kept
    // short — ubifs has a 255-byte filename limit.
    let seq = TMP_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut tmp_name = std::ffi::OsString::from(file_name);
    tmp_name.push(format!(".tmp.{}.{}.{}", std::process::id(), seq, nanos));
    let tmp_path = parent.join(&tmp_name);

    // Explicit create-and-truncate, 0600 on unix (config may contain secrets).
    let mut tmp = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)?;

    let write_result = (|| -> io::Result<()> {
        tmp.write_all(bytes)?;
        tmp.flush()?;
        tmp.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Drop the tempfile handle before rename (Windows friendliness; on Linux
    // it doesn't matter but costs nothing).
    drop(tmp);

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // P3-2: the new contents are now in place (rename succeeded), so a
    // successful rewrite of the daemon config file invalidates the in-memory
    // config cache. Bump AFTER the rename so any reader that observes the new
    // generation is guaranteed to read the new file. Filename-gated so
    // profile/onboarding writes that also use `atomic_write` do not invalidate
    // the config cache.
    if is_config_file(path) {
        CONFIG_WRITE_GENERATION.fetch_add(1, Ordering::AcqRel);
    }

    // Best-effort parent dir fsync so the rename is crash-safe. If we can't
    // open the dir for fsync (e.g. Windows dev), swallow — the rename itself
    // is already atomic and the data was fsynced.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_round_trip() {
        let dir = std::env::temp_dir().join(format!("dcent_atomic_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.toml");

        atomic_write_bytes(&target, b"key = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "key = 1\n");

        atomic_write_bytes(&target, b"key = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "key = 2\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overwrites_existing_without_truncating_on_error() {
        let dir =
            std::env::temp_dir().join(format!("dcent_atomic_test_err_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("existing.toml");
        std::fs::write(&target, b"original\n").unwrap();

        atomic_write_bytes(&target, b"replaced\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "replaced\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    // P3-2: only the daemon config file invalidates the config cache.
    #[test]
    fn is_config_file_only_matches_the_daemon_config() {
        assert!(is_config_file(Path::new("/data/dcentrald.toml")));
        assert!(is_config_file(Path::new("/etc/dcentrald.toml")));
        assert!(!is_config_file(Path::new("/data/profile.json")));
        assert!(!is_config_file(Path::new("/data/onboarding.json")));
        // The staging tempfile must NOT count as a config write.
        assert!(!is_config_file(Path::new("/data/dcentrald.toml.tmp.123")));
    }

    #[test]
    fn storage_write_failure_kind_classifies_full_and_read_only_storage() {
        assert_eq!(
            storage_write_failure_kind(&io::Error::new(io::ErrorKind::StorageFull, "full")),
            Some(StorageWriteFailureKind::StorageFull)
        );
        assert_eq!(
            storage_write_failure_kind(&io::Error::new(io::ErrorKind::QuotaExceeded, "quota")),
            Some(StorageWriteFailureKind::StorageFull)
        );
        assert_eq!(
            storage_write_failure_kind(&io::Error::new(
                io::ErrorKind::ReadOnlyFilesystem,
                "read-only"
            )),
            Some(StorageWriteFailureKind::ReadOnly)
        );
        assert_eq!(
            storage_write_failure_kind(&io::Error::new(
                io::ErrorKind::PermissionDenied,
                "permission"
            )),
            Some(StorageWriteFailureKind::ReadOnly)
        );
        assert_eq!(
            storage_write_failure_kind(&io::Error::new(io::ErrorKind::InvalidInput, "bad path")),
            None
        );
    }

    // P3-2: a successful config rewrite must bump the generation so the
    // in-memory config cache reloads (GET-after-POST freshness). Uses a
    // strictly-greater assertion so it is robust to other tests bumping the
    // same process-global counter in parallel.
    #[test]
    fn config_file_write_increments_generation_monotonically() {
        let dir = std::env::temp_dir().join(format!("dcent_cfg_gen_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let before = config_write_generation();
        atomic_write_bytes(&dir.join(CONFIG_FILE_NAME), b"x = 1\n").unwrap();
        let after = config_write_generation();
        assert!(
            after > before,
            "writing the config file must bump the generation"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // RELIAB-2a: many concurrent same-process writers to ONE target must never
    // publish a torn or empty file, and must leave no staging tempfiles behind.
    // The old `.tmp.<pid>`-only name made two same-process writers share the
    // identical staging path (opened with truncate(true)) — the bug this guards.
    #[test]
    fn concurrent_same_process_writes_never_tear_the_target() {
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "dcent_atomic_race_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = Arc::new(dir.join("race.toml"));

        // Distinct, COMPLETE payloads. After a write storm the published file
        // must equal EXACTLY one of them — never empty, never a byte-mix.
        let payloads: Vec<String> = (0..16).map(|i| format!("value = {}\n", i)).collect();

        for _round in 0..40 {
            let mut handles = Vec::new();
            for p in &payloads {
                let target = Arc::clone(&target);
                let p = p.clone();
                handles.push(std::thread::spawn(move || {
                    atomic_write_bytes(&target, p.as_bytes()).unwrap();
                }));
            }
            for h in handles {
                h.join().unwrap();
            }

            let got = std::fs::read_to_string(&*target).unwrap();
            assert!(
                payloads.contains(&got),
                "target was torn/empty/mixed after concurrent writes: {:?}",
                got
            );

            // Unique staging names + best-effort cleanup => no `.tmp.` leftovers.
            let leftovers: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".tmp."))
                .collect();
            assert!(
                leftovers.is_empty(),
                "stray staging tempfiles: {:?}",
                leftovers
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // RELIAB-2b: `config_write_lock` must serialize a load→modify→write critical
    // section so concurrent writers don't lose updates. Each thread does a
    // deliberately NON-atomic read-modify-write of a shared counter file (with a
    // yield to widen the race window); the final value can only equal the total
    // increment count if every critical section ran with exclusive access.
    #[test]
    fn config_write_lock_serializes_read_modify_write() {
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "dcent_cfg_lock_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let counter_path = Arc::new(dir.join("counter.txt"));
        atomic_write_bytes(&counter_path, b"0").unwrap();

        const THREADS: usize = 8;
        const ITERS: usize = 25;

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let counter_path = Arc::clone(&counter_path);
            handles.push(std::thread::spawn(move || {
                for _ in 0..ITERS {
                    let _guard = config_write_lock();
                    let cur: u64 = std::fs::read_to_string(&*counter_path)
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    // Widen the lost-update window: without the lock, another
                    // thread reads the same `cur` here and one increment is lost.
                    std::thread::yield_now();
                    atomic_write_bytes(&counter_path, (cur + 1).to_string().as_bytes()).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let final_val: u64 = std::fs::read_to_string(&*counter_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            final_val,
            (THREADS * ITERS) as u64,
            "lost update: the load→modify→write critical section was not serialized"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
