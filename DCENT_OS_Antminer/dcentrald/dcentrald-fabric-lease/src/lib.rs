//! Cross-process ownership for physical DCENT_OS hardware fabrics.
//!
//! This crate is deliberately smaller than the HAL. It contains no device
//! protocol, MMIO, GPIO, or controller operation, so the normal daemon and
//! standalone inspection/recovery tools can share one lock protocol without
//! granting those tools the HAL's mutation surface.
//!
//! A lease is an advisory Linux `flock(2)` on a stable file beneath the
//! root-owned `/run/dcentos/hardware-locks` directory. The file is never
//! unlinked or renamed: deleting a locked file would permit another process to
//! create a different inode and become a second apparent owner. Ownership is
//! released only when the final open-file description is closed, including
//! automatic release after process death.
//!
//! All cooperating owners must see the same `/run` mount and lock inode. The
//! shipped services run in one host mount namespace. A future chroot/container
//! that shares hardware devices must bind the host lock directory into the
//! same path or, preferably, delegate hardware access to one broker; a private
//! `/run` namespace would create a different inode and cannot coordinate.
//!
//! The lock proves cooperation among programs that use this crate. It does not
//! constrain a privileged program that deliberately ignores the protocol, and
//! it does not prove that power rails are safe after an owner crashes.

use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const RUNTIME_ROOT: &str = "/run";
const DCENTOS_RUNTIME_DIR: &CStr = c"dcentos";
const HARDWARE_LOCK_DIR: &CStr = c"hardware-locks";
const PRIVATE_DIRECTORY_MODE: libc::mode_t = 0o700;
const LOCK_FILE_MODE: libc::mode_t = 0o600;
const OWNER_RECORD_LIMIT: usize = 512;

static NEXT_ALLOCATION: AtomicU64 = AtomicU64::new(0);

/// Stable identity for one complete physical I2C master/wire fabric.
///
/// Transport aliases must use the same value. For example, S9 kernel adapter
/// zero and direct AXI-IIC MMIO both use `linux_adapter(0)`. A slave address is
/// never part of this identity because ownership covers the whole bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalI2cFabricId {
    namespace: u8,
    instance: u16,
}

impl PhysicalI2cFabricId {
    /// Canonical identity for a Linux I2C adapter and every transport alias
    /// which topology maps onto that adapter's physical wires.
    pub const fn linux_adapter(bus: u8) -> Self {
        Self {
            namespace: 0,
            instance: bus as u16,
        }
    }

    /// Project-wide identity for a physical fabric that is not a Linux I2C
    /// adapter alias (for example a dedicated FPGA/GPIO PSU bus).
    ///
    /// IDs are topology ABI: once assigned, an ID must never be reused for a
    /// different wire fabric. Platform code should expose named constants
    /// rather than scattering numeric IDs through model-specific call sites.
    const fn topology_defined(instance: u16) -> Self {
        Self {
            namespace: 1,
            instance,
        }
    }

    fn filename_component(self) -> String {
        match self.namespace {
            0 => format!("linux-adapter-{}", self.instance),
            1 => format!("topology-{}", self.instance),
            _ => unreachable!("private fabric namespace is valid by construction"),
        }
    }
}

impl fmt::Display for PhysicalI2cFabricId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.namespace {
            0 => write!(formatter, "linux-adapter-{}", self.instance),
            1 => write!(formatter, "topology-fabric-{}", self.instance),
            _ => unreachable!("private fabric namespace is valid by construction"),
        }
    }
}

/// Canonical project-wide assignments for physical fabrics that have no Linux
/// adapter identity. This is an ABI ledger: add new named entries here, test
/// uniqueness, and never renumber or reuse an existing value.
pub mod topology {
    use super::PhysicalI2cFabricId;

    /// AM2 GPIO895/896 (AXI GPIO `0x4122_0000`) dedicated PSU SMBus.
    pub const AM2_PSU_GPIO: PhysicalI2cFabricId = PhysicalI2cFabricId::topology_defined(1);

    /// Machine-readable registry used by collision tests and documentation.
    pub const NAMED_PHYSICAL_I2C_FABRICS: &[(&str, PhysicalI2cFabricId)] =
        &[("am2-psu-gpio", AM2_PSU_GPIO)];
}

/// Diagnostic classification of the cooperative owner.
///
/// Purpose never changes lock authority; every variant takes the same
/// exclusive whole-fabric lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cLeasePurpose {
    BootstrapProbe,
    RuntimeService,
    RuntimeRaw,
    RecoveryInspection,
    Diagnostics,
    Manufacturing,
}

impl I2cLeasePurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BootstrapProbe => "bootstrap-probe",
            Self::RuntimeService => "runtime-service",
            Self::RuntimeRaw => "runtime-raw",
            Self::RecoveryInspection => "recovery-inspection",
            Self::Diagnostics => "diagnostics",
            Self::Manufacturing => "manufacturing",
        }
    }
}

/// Typed acquisition failure. Ownership failures must not be flattened into a
/// device transport error because callers could otherwise retry, reset the
/// controller, or fall back to an unleased bit-banged master.
#[derive(Debug)]
pub enum FabricLeaseError {
    Busy {
        fabric: PhysicalI2cFabricId,
        holder: Option<String>,
    },
    UnsafePath {
        path: PathBuf,
        detail: String,
    },
    Io {
        stage: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    ForkedProcess {
        creator_pid: u32,
        current_pid: u32,
    },
}

impl FabricLeaseError {
    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }

    pub fn io_kind(&self) -> io::ErrorKind {
        match self {
            Self::Busy { .. } => io::ErrorKind::AlreadyExists,
            Self::UnsafePath { .. } | Self::ForkedProcess { .. } => io::ErrorKind::PermissionDenied,
            Self::Io { source, .. } => source.kind(),
        }
    }

    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.io_kind(), self)
    }
}

impl fmt::Display for FabricLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { fabric, holder } => {
                write!(formatter, "{fabric} already has a cross-process owner")?;
                if let Some(holder) = holder {
                    write!(formatter, " ({holder})")?;
                }
                Ok(())
            }
            Self::UnsafePath { path, detail } => {
                write!(
                    formatter,
                    "unsafe hardware-lock path {}: {detail}",
                    path.display()
                )
            }
            Self::Io {
                stage,
                path,
                source,
            } => write!(
                formatter,
                "hardware-lock {stage} failed for {}: {source}",
                path.display()
            ),
            Self::ForkedProcess {
                creator_pid,
                current_pid,
            } => write!(
                formatter,
                "hardware lease belongs to process {creator_pid}, not forked process {current_pid}"
            ),
        }
    }
}

impl std::error::Error for FabricLeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Non-cloneable cross-process lease for one physical I2C fabric.
///
/// Dropping this value closes the descriptor; there is intentionally no
/// explicit unlock and no lock-file deletion operation.
#[derive(Debug)]
pub struct OsI2cFabricLease {
    _file: File,
    fabric: PhysicalI2cFabricId,
    creator_pid: u32,
    allocation: u64,
}

impl OsI2cFabricLease {
    /// Acquire the production lease before opening a device, touching MMIO,
    /// binding/unbinding a driver, or changing GPIOs associated with the bus.
    pub fn acquire(
        fabric: PhysicalI2cFabricId,
        purpose: I2cLeasePurpose,
    ) -> Result<Self, FabricLeaseError> {
        acquire_at(Path::new(RUNTIME_ROOT), fabric, purpose)
    }

    pub const fn fabric(&self) -> PhysicalI2cFabricId {
        self.fabric
    }

    pub const fn allocation(&self) -> u64 {
        self.allocation
    }

    /// Reject use of inherited Rust state after `fork()` without immediate
    /// `exec()`. `O_CLOEXEC` handles normal command execution, but a fork-only
    /// child shares the open-file description and must not use copied HAL state.
    pub fn validate_current_process(&self) -> Result<(), FabricLeaseError> {
        let current_pid = std::process::id();
        if current_pid == self.creator_pid {
            Ok(())
        } else {
            Err(FabricLeaseError::ForkedProcess {
                creator_pid: self.creator_pid,
                current_pid,
            })
        }
    }
}

fn acquire_at(
    runtime_root: &Path,
    fabric: PhysicalI2cFabricId,
    purpose: I2cLeasePurpose,
) -> Result<OsI2cFabricLease, FabricLeaseError> {
    let allocation = NEXT_ALLOCATION
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            current.checked_add(1)
        })
        .map_err(|_| FabricLeaseError::Io {
            stage: "allocate identity",
            path: runtime_root.to_path_buf(),
            source: io::Error::other("hardware lease allocation space is exhausted"),
        })?
        + 1;

    let runtime_fd = open_directory_path(runtime_root, "open runtime root")?;
    validate_directory(runtime_fd.as_raw_fd(), runtime_root, false, "runtime root")?;

    let dcentos_path = runtime_root.join(DCENTOS_RUNTIME_DIR.to_string_lossy().as_ref());
    let dcentos_fd = open_or_create_private_directory(
        runtime_fd.as_raw_fd(),
        DCENTOS_RUNTIME_DIR,
        &dcentos_path,
    )?;
    let lock_dir_path = dcentos_path.join(HARDWARE_LOCK_DIR.to_string_lossy().as_ref());
    let lock_dir_fd = open_or_create_private_directory(
        dcentos_fd.as_raw_fd(),
        HARDWARE_LOCK_DIR,
        &lock_dir_path,
    )?;

    let filename = format!("i2c-fabric-{}.lock", fabric.filename_component());
    let filename_c =
        CString::new(filename.as_bytes()).map_err(|_| FabricLeaseError::UnsafePath {
            path: lock_dir_path.join(&filename),
            detail: "lock filename contains a NUL byte".into(),
        })?;
    let lock_path = lock_dir_path.join(&filename);
    let raw_fd = unsafe {
        libc::openat(
            lock_dir_fd.as_raw_fd(),
            filename_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            LOCK_FILE_MODE,
        )
    };
    if raw_fd < 0 {
        return Err(io_failure("open lock file", &lock_path));
    }
    let mut file = unsafe { File::from_raw_fd(raw_fd) };
    validate_lock_file(file.as_raw_fd(), &lock_path)?;

    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked != 0 {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::WouldBlock {
            return Err(FabricLeaseError::Busy {
                fabric,
                holder: read_owner_record(&file),
            });
        }
        return Err(FabricLeaseError::Io {
            stage: "acquire nonblocking flock",
            path: lock_path,
            source,
        });
    }

    let creator_pid = std::process::id();
    let record = owner_record(fabric, purpose, creator_pid, allocation);
    file.set_len(0).map_err(|source| FabricLeaseError::Io {
        stage: "truncate owner metadata after lock",
        path: lock_path.clone(),
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(record.as_bytes()))
        .map_err(|source| FabricLeaseError::Io {
            stage: "write owner metadata after lock",
            path: lock_path,
            source,
        })?;

    Ok(OsI2cFabricLease {
        _file: file,
        fabric,
        creator_pid,
        allocation,
    })
}

fn open_directory_path(path: &Path, stage: &'static str) -> Result<OwnedFd, FabricLeaseError> {
    let path_c = path_to_cstring(path)?;
    let raw_fd = unsafe {
        libc::open(
            path_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw_fd < 0 {
        Err(io_failure(stage, path))
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }
}

fn open_or_create_private_directory(
    parent_fd: libc::c_int,
    name: &CStr,
    path: &Path,
) -> Result<OwnedFd, FabricLeaseError> {
    let created = unsafe { libc::mkdirat(parent_fd, name.as_ptr(), PRIVATE_DIRECTORY_MODE) };
    if created != 0 {
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::AlreadyExists {
            return Err(FabricLeaseError::Io {
                stage: "create private lock directory",
                path: path.to_path_buf(),
                source,
            });
        }
    }

    let raw_fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw_fd < 0 {
        return Err(io_failure("open private lock directory", path));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    validate_directory(fd.as_raw_fd(), path, true, "private lock directory")?;
    Ok(fd)
}

fn validate_directory(
    fd: libc::c_int,
    path: &Path,
    private: bool,
    description: &str,
) -> Result<(), FabricLeaseError> {
    let stat = fstat(fd, path)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: format!("{description} is not a directory"),
        });
    }
    let effective_uid = unsafe { libc::geteuid() };
    if stat.st_uid != effective_uid {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: format!(
                "{description} owner uid {} does not match effective uid {effective_uid}",
                stat.st_uid
            ),
        });
    }
    let mode = stat.st_mode & 0o777;
    let unsafe_mode = if private {
        mode != PRIVATE_DIRECTORY_MODE
    } else {
        mode & 0o022 != 0
    };
    if unsafe_mode {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: if private {
                format!("{description} mode {mode:04o} must be exactly 0700")
            } else {
                format!("{description} mode {mode:04o} permits group/other writes")
            },
        });
    }
    Ok(())
}

fn validate_lock_file(fd: libc::c_int, path: &Path) -> Result<(), FabricLeaseError> {
    let stat = fstat(fd, path)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: "lock target is not a regular file".into(),
        });
    }
    let effective_uid = unsafe { libc::geteuid() };
    if stat.st_uid != effective_uid {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: format!(
                "lock owner uid {} does not match effective uid {effective_uid}",
                stat.st_uid
            ),
        });
    }
    if stat.st_nlink != 1 {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: format!("lock file link count {} must be exactly one", stat.st_nlink),
        });
    }
    let mode = stat.st_mode & 0o777;
    if mode != LOCK_FILE_MODE {
        return Err(FabricLeaseError::UnsafePath {
            path: path.to_path_buf(),
            detail: format!("lock file mode {mode:04o} must be exactly 0600"),
        });
    }
    Ok(())
}

fn fstat(fd: libc::c_int, path: &Path) -> Result<libc::stat, FabricLeaseError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if result != 0 {
        Err(io_failure("inspect opened path", path))
    } else {
        Ok(unsafe { stat.assume_init() })
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, FabricLeaseError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| FabricLeaseError::UnsafePath {
        path: path.to_path_buf(),
        detail: "path contains a NUL byte".into(),
    })
}

fn io_failure(stage: &'static str, path: &Path) -> FabricLeaseError {
    FabricLeaseError::Io {
        stage,
        path: path.to_path_buf(),
        source: io::Error::last_os_error(),
    }
}

fn owner_record(
    fabric: PhysicalI2cFabricId,
    purpose: I2cLeasePurpose,
    pid: u32,
    allocation: u64,
) -> String {
    let boot_id = bounded_single_line(
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_else(|_| "unavailable".into()),
        64,
    );
    let start_ticks = process_start_ticks().unwrap_or_else(|| "unavailable".into());
    format!(
        "schema=1 pid={pid} start_ticks={start_ticks} boot_id={boot_id} role={} fabric={fabric} allocation={allocation}\n",
        purpose.as_str(),
    )
}

fn process_start_ticks() -> Option<String> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after_name = stat.rsplit_once(") ")?.1;
    after_name.split_whitespace().nth(19).map(str::to_owned)
}

fn bounded_single_line(value: String, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() && !character.is_whitespace())
        .take(limit)
        .collect()
}

fn read_owner_record(file: &File) -> Option<String> {
    let mut buffer = [0_u8; OWNER_RECORD_LIMIT];
    let count = unsafe {
        libc::pread(
            file.as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    if count <= 0 {
        return None;
    }
    let count = usize::try_from(count).ok()?;
    let record = String::from_utf8_lossy(buffer.get(..count)?);
    let sanitized: String = record
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() || character == ' ' {
                character
            } else {
                ' '
            }
        })
        .take(OWNER_RECORD_LIMIT)
        .collect();
    let sanitized = sanitized.trim();
    (!sanitized.is_empty()).then(|| sanitized.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct TempRuntimeRoot(PathBuf);

    impl TempRuntimeRoot {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "dcentos-fabric-lease-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&root).expect("create isolated runtime root");
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("secure runtime root mode");
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn lock_dir(&self) -> PathBuf {
            self.0.join("dcentos/hardware-locks")
        }

        fn lock_file(&self, fabric: PhysicalI2cFabricId) -> PathBuf {
            self.lock_dir()
                .join(format!("i2c-fabric-{}.lock", fabric.filename_component()))
        }
    }

    impl Drop for TempRuntimeRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn acquire_test(
        root: &TempRuntimeRoot,
        fabric: PhysicalI2cFabricId,
    ) -> Result<OsI2cFabricLease, FabricLeaseError> {
        acquire_at(root.path(), fabric, I2cLeasePurpose::RuntimeService)
    }

    #[test]
    fn independent_opens_exclude_and_close_releases_without_unlink() {
        let root = TempRuntimeRoot::new("exclusive");
        let fabric = PhysicalI2cFabricId::linux_adapter(0);
        let first = acquire_test(&root, fabric).expect("first owner");
        let error = acquire_test(&root, fabric).expect_err("second owner must lose");
        assert!(error.is_busy());
        let lock_file = root.lock_file(fabric);
        assert!(lock_file.is_file());

        drop(first);
        let replacement = acquire_test(&root, fabric).expect("released lock is reusable");
        assert!(
            lock_file.is_file(),
            "lease release must never unlink the file"
        );
        drop(replacement);
    }

    #[test]
    fn different_physical_fabrics_do_not_alias() {
        let root = TempRuntimeRoot::new("different");
        let zero = acquire_test(&root, PhysicalI2cFabricId::linux_adapter(0)).unwrap();
        let one = acquire_test(&root, PhysicalI2cFabricId::linux_adapter(1)).unwrap();
        let dedicated = acquire_test(&root, PhysicalI2cFabricId::topology_defined(1)).unwrap();
        drop((zero, one, dedicated));
    }

    #[test]
    fn topology_fabric_registry_is_unique_and_stable() {
        let mut identities = std::collections::HashSet::new();
        for (name, identity) in topology::NAMED_PHYSICAL_I2C_FABRICS {
            assert!(
                identities.insert(*identity),
                "duplicate topology ID for {name}"
            );
        }
        assert_eq!(topology::AM2_PSU_GPIO.to_string(), "topology-fabric-1");
    }

    #[test]
    fn stale_metadata_without_a_live_flock_is_not_authority() {
        let root = TempRuntimeRoot::new("stale");
        let fabric = PhysicalI2cFabricId::linux_adapter(2);
        let first = acquire_test(&root, fabric).unwrap();
        drop(first);
        std::fs::write(root.lock_file(fabric), b"schema=1 pid=stale\n").unwrap();

        let replacement = acquire_test(&root, fabric).expect("stale text cannot own a lock");
        let record = std::fs::read_to_string(root.lock_file(fabric)).unwrap();
        assert!(record.contains(&format!("pid={}", std::process::id())));
        drop(replacement);
    }

    #[test]
    fn losing_contender_cannot_truncate_owner_metadata() {
        let root = TempRuntimeRoot::new("metadata");
        let fabric = PhysicalI2cFabricId::linux_adapter(3);
        let owner = acquire_test(&root, fabric).unwrap();
        let before = std::fs::read(root.lock_file(fabric)).unwrap();
        assert!(acquire_test(&root, fabric).is_err());
        let after = std::fs::read(root.lock_file(fabric)).unwrap();
        assert_eq!(before, after);
        drop(owner);
    }

    #[test]
    fn symlink_and_hardlink_lock_targets_are_refused() {
        let root = TempRuntimeRoot::new("links");
        let symlink_fabric = PhysicalI2cFabricId::linux_adapter(4);
        let bootstrap = acquire_test(&root, symlink_fabric).unwrap();
        drop(bootstrap);
        let symlink_path = root.lock_file(symlink_fabric);
        std::fs::remove_file(&symlink_path).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &symlink_path).unwrap();
        assert!(matches!(
            acquire_test(&root, symlink_fabric),
            Err(FabricLeaseError::Io { .. }) | Err(FabricLeaseError::UnsafePath { .. })
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");

        std::fs::remove_file(&symlink_path).unwrap();
        let hardlink_fabric = PhysicalI2cFabricId::linux_adapter(5);
        let hardlink_path = root.lock_file(hardlink_fabric);
        std::fs::hard_link(&outside, &hardlink_path).unwrap();
        assert!(matches!(
            acquire_test(&root, hardlink_fabric),
            Err(FabricLeaseError::UnsafePath { .. })
        ));
    }

    #[test]
    fn symlink_directory_components_are_refused() {
        let dcentos_link_root = TempRuntimeRoot::new("dcentos-dir-link");
        let outside = dcentos_link_root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&outside, dcentos_link_root.path().join("dcentos")).unwrap();
        assert!(acquire_test(&dcentos_link_root, PhysicalI2cFabricId::linux_adapter(10)).is_err());

        let lock_dir_link_root = TempRuntimeRoot::new("lock-dir-link");
        let dcentos = lock_dir_link_root.path().join("dcentos");
        std::fs::create_dir(&dcentos).unwrap();
        std::fs::set_permissions(&dcentos, std::fs::Permissions::from_mode(0o700)).unwrap();
        let outside = lock_dir_link_root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&outside, dcentos.join("hardware-locks")).unwrap();
        assert!(acquire_test(&lock_dir_link_root, PhysicalI2cFabricId::linux_adapter(11)).is_err());
    }

    #[test]
    fn concurrent_first_creation_produces_exactly_one_owner() {
        const THREADS: usize = 8;
        let root = TempRuntimeRoot::new("first-creation-race");
        let start = Arc::new(Barrier::new(THREADS + 1));
        let attempted = Arc::new(Barrier::new(THREADS + 1));
        let mut workers = Vec::new();
        for _ in 0..THREADS {
            let root = root.path().to_path_buf();
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            workers.push(std::thread::spawn(move || {
                start.wait();
                let lease = acquire_at(
                    &root,
                    PhysicalI2cFabricId::linux_adapter(12),
                    I2cLeasePurpose::Diagnostics,
                );
                attempted.wait();
                lease
            }));
        }
        start.wait();
        attempted.wait();

        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().expect("creation-race worker"))
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, FabricLeaseError::Busy { .. })));
    }

    #[test]
    fn insecure_directory_and_lock_modes_fail_closed() {
        let root = TempRuntimeRoot::new("modes");
        std::fs::create_dir(root.path().join("dcentos")).unwrap();
        std::fs::set_permissions(
            root.path().join("dcentos"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(matches!(
            acquire_test(&root, PhysicalI2cFabricId::linux_adapter(6)),
            Err(FabricLeaseError::UnsafePath { .. })
        ));

        std::fs::set_permissions(
            root.path().join("dcentos"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let fabric = PhysicalI2cFabricId::linux_adapter(7);
        let lease = acquire_test(&root, fabric).unwrap();
        drop(lease);
        std::fs::set_permissions(
            root.lock_file(fabric),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        assert!(matches!(
            acquire_test(&root, fabric),
            Err(FabricLeaseError::UnsafePath { .. })
        ));
    }

    #[test]
    fn lease_descriptor_is_close_on_exec() {
        let root = TempRuntimeRoot::new("cloexec");
        let lease = acquire_test(&root, PhysicalI2cFabricId::linux_adapter(8)).unwrap();
        let flags = unsafe { libc::fcntl(lease._file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        drop(lease);
    }

    #[test]
    fn copied_lease_state_rejects_a_different_process_identity() {
        let root = TempRuntimeRoot::new("fork-identity");
        let mut lease = acquire_test(&root, PhysicalI2cFabricId::linux_adapter(14)).unwrap();
        lease.creator_pid ^= 1;
        assert!(matches!(
            lease.validate_current_process(),
            Err(FabricLeaseError::ForkedProcess { .. })
        ));
    }

    #[test]
    fn exec_child_does_not_retain_parent_lease() {
        const CHILD_READY: &str = "DCENT_FABRIC_LEASE_EXEC_CHILD_READY";
        if let Ok(ready) = std::env::var(CHILD_READY) {
            std::fs::write(ready, b"ready").expect("publish exec-child readiness");
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }

        let root = TempRuntimeRoot::new("exec-cloexec");
        let ready = root.path().join("exec-ready");
        let fabric = PhysicalI2cFabricId::linux_adapter(13);
        let lease = acquire_test(&root, fabric).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("exec_child_does_not_retain_parent_lease")
            .arg("--nocapture")
            .env(CHILD_READY, &ready)
            .spawn()
            .expect("exec waiting child");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("exec child exited before readiness: {status}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.is_file(), "exec child did not become ready");

        drop(lease);
        let replacement = acquire_test(&root, fabric);
        child.kill().expect("stop exec child");
        child.wait().expect("reap exec child");
        drop(replacement.expect("CLOEXEC child must not retain the parent lease"));
    }

    #[test]
    fn subprocess_contention_and_sigkill_release_are_kernel_proven() {
        const CHILD_ROOT: &str = "DCENT_FABRIC_LEASE_TEST_CHILD_ROOT";
        const CHILD_READY: &str = "DCENT_FABRIC_LEASE_TEST_CHILD_READY";

        if let (Ok(root), Ok(ready)) = (std::env::var(CHILD_ROOT), std::env::var(CHILD_READY)) {
            let lease = acquire_at(
                Path::new(&root),
                PhysicalI2cFabricId::linux_adapter(9),
                I2cLeasePurpose::Diagnostics,
            )
            .expect("child acquires cross-process lease");
            std::fs::write(ready, b"ready").expect("publish child readiness");
            let _retain = lease;
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }

        let root = TempRuntimeRoot::new("subprocess");
        let ready = root.path().join("ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("subprocess_contention_and_sigkill_release_are_kernel_proven")
            .arg("--nocapture")
            .env(CHILD_ROOT, root.path())
            .env(CHILD_READY, &ready)
            .spawn()
            .expect("spawn isolated lock owner");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("lock-owner child exited before readiness: {status}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.is_file(), "lock-owner child did not become ready");
        let fabric = PhysicalI2cFabricId::linux_adapter(9);
        assert!(matches!(
            acquire_test(&root, fabric),
            Err(FabricLeaseError::Busy { .. })
        ));

        child.kill().expect("SIGKILL lock owner");
        child.wait().expect("reap lock owner");
        let replacement = acquire_test(&root, fabric).expect("kernel releases lock on SIGKILL");
        drop(replacement);
    }

    #[test]
    fn source_contract_never_unlinks_or_explicitly_unlocks() {
        let source = include_str!("lib.rs");
        let explicit_unlock = ["LOCK", "_UN"].concat();
        let remove_lock = ["remove_file", "(lock"].concat();
        let rename_lock = ["rename", "(lock"].concat();
        assert!(!source.contains(&explicit_unlock));
        assert!(!source.contains(&remove_lock));
        assert!(!source.contains(&rename_lock));
    }
}
