#!/usr/bin/env python3
"""Atomically publish one exact directory without replacing an existing name.

The source and destination parents are identity-bound before publication.  The
promoted directory remains open across the rename, post-commit verification,
and durability boundary so path substitution cannot turn a successful result
into a claim about different bytes.  Any post-rename fault moves the exact
promoted directory to a private diagnostic quarantine before returning failure.
"""

from __future__ import annotations

from collections.abc import Callable
import ctypes
import errno
import os
from pathlib import Path
import secrets
import stat
import sys


class DirectoryPublishError(ValueError):
    """The requested directory publication was unsafe or incomplete."""


Identity = tuple[int, int]

WINDOWS_PRIVATE_DIRECTORY_SDDL = (
    "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)"
)
WINDOWS_PRIVATE_FILE_SDDL = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)"
WINDOWS_PUBLIC_DIRECTORY_SDDL = (
    "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)"
    "(A;OICI;0x1200a9;;;WD)"
)
WINDOWS_PUBLIC_FILE_SDDL = (
    "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)(A;;0x120089;;;WD)"
)


def _identity(metadata: os.stat_result) -> Identity:
    return metadata.st_dev, metadata.st_ino


def _is_reparse(metadata: os.stat_result) -> bool:
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(metadata, "st_file_attributes", 0) & marker)


def _directory_metadata(path: Path, label: str) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise DirectoryPublishError(f"{label} is unavailable: {path}: {error}") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        raise DirectoryPublishError(
            f"{label} must be a non-reparse directory: {path}"
        )
    return metadata


def _require_identity(
    metadata: os.stat_result, expected: Identity, label: str
) -> None:
    if _identity(metadata) != expected:
        raise DirectoryPublishError(f"{label} identity changed before publication")


def _destination_absent(path: Path) -> None:
    try:
        path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise DirectoryPublishError(
            f"cannot inspect directory publication destination: {error}"
        ) from error
    raise DirectoryPublishError(f"refusing to replace existing destination: {path}")


def _windows_set_directory_acl(handle: int, sddl: str) -> None:
    """Apply one protected DACL to a pinned directory and its inheriting children."""

    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    convert = advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW
    convert.argtypes = (
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_uint32),
    )
    convert.restype = ctypes.c_int
    get_dacl = advapi32.GetSecurityDescriptorDacl
    get_dacl.argtypes = (
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_int),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_int),
    )
    get_dacl.restype = ctypes.c_int
    set_security = advapi32.SetSecurityInfo
    set_security.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_void_p,
    )
    set_security.restype = ctypes.c_uint32
    local_free = kernel32.LocalFree
    local_free.argtypes = (ctypes.c_void_p,)
    local_free.restype = ctypes.c_void_p

    security_descriptor = ctypes.c_void_p()
    if not convert(sddl, 1, ctypes.byref(security_descriptor), None):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        present = ctypes.c_int()
        defaulted = ctypes.c_int()
        dacl = ctypes.c_void_p()
        if not get_dacl(
            security_descriptor,
            ctypes.byref(present),
            ctypes.byref(dacl),
            ctypes.byref(defaulted),
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        if not present.value or not dacl.value:
            raise DirectoryPublishError("canonical Windows publication DACL is absent")
        result = set_security(
            handle,
            1,  # SE_FILE_OBJECT
            0x00000004 | 0x80000000,  # DACL + PROTECTED_DACL_SECURITY_INFORMATION
            None,
            None,
            dacl,
            None,
        )
        if result != 0:
            raise ctypes.WinError(result)
    finally:
        local_free(security_descriptor)


def set_windows_path_acl(path: Path, sddl: str, *, directory: bool) -> None:
    """Apply a canonical DACL to one path-pinned Windows filesystem object."""

    if os.name != "nt":
        raise DirectoryPublishError("Windows directory ACLs require Windows")
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = (
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    )
    create_file.restype = ctypes.c_void_p
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int
    flush = kernel32.FlushFileBuffers
    flush.argtypes = (ctypes.c_void_p,)
    flush.restype = ctypes.c_int
    handle = create_file(
        str(path),
        0x40000000 | 0x00020000 | 0x00040000,  # GENERIC_WRITE + ACL access
        0x00000001 | 0x00000002 | 0x00000004,
        None,
        3,  # OPEN_EXISTING
        (0x02000000 if directory else 0) | 0x00200000,
        None,
    )
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        _windows_set_directory_acl(handle, sddl)
        if not flush(handle):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        close_handle(handle)


def set_windows_directory_acl(path: Path, sddl: str) -> None:
    set_windows_path_acl(path, sddl, directory=True)


def set_windows_file_acl(path: Path, sddl: str) -> None:
    set_windows_path_acl(path, sddl, directory=False)


def _require_private_staging_parent(metadata: os.stat_result, path: Path) -> None:
    if os.name != "posix":
        return
    mode = stat.S_IMODE(metadata.st_mode)
    if mode & 0o077 or mode & 0o700 != 0o700:
        raise DirectoryPublishError(
            "staging parent must be owner-only mode 0700 before final directory "
            f"mode can be prepared safely: {path} has {mode:04o}"
        )


def _linux_rename_noreplace(
    source_parent_fd: int,
    source_name: str,
    destination_parent_fd: int,
    destination_name: str,
) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise DirectoryPublishError(
            "atomic no-replace directory rename is unavailable on this Linux libc"
        )
    renameat2.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    )
    renameat2.restype = ctypes.c_int
    if renameat2(
        source_parent_fd,
        os.fsencode(source_name),
        destination_parent_fd,
        os.fsencode(destination_name),
        1,  # RENAME_NOREPLACE
    ) == 0:
        return
    error_number = ctypes.get_errno()
    if error_number in (errno.EEXIST, errno.ENOTEMPTY):
        raise DirectoryPublishError(
            f"refusing to replace existing destination: {destination_name}"
        )
    raise OSError(error_number, os.strerror(error_number), destination_name)


def linux_rename_directory_noreplace(
    source_parent_fd: int,
    source_name: str,
    destination_parent_fd: int,
    destination_name: str,
) -> None:
    """Expose the Linux handle-relative no-replace rename for private lifecycle use."""

    if not sys.platform.startswith("linux"):
        raise DirectoryPublishError("Linux directory rename requires Linux")
    _linux_rename_noreplace(
        source_parent_fd,
        source_name,
        destination_parent_fd,
        destination_name,
    )


def linux_exchange_paths(
    first_parent_fd: int,
    first_name: str,
    second_parent_fd: int,
    second_name: str,
) -> None:
    """Atomically exchange two existing Linux directory entries."""

    if not sys.platform.startswith("linux"):
        raise DirectoryPublishError("Linux path exchange requires Linux")
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise DirectoryPublishError(
            "atomic Linux path exchange is unavailable on this libc"
        )
    renameat2.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    )
    renameat2.restype = ctypes.c_int
    if renameat2(
        first_parent_fd,
        os.fsencode(first_name),
        second_parent_fd,
        os.fsencode(second_name),
        2,  # RENAME_EXCHANGE
    ) == 0:
        return
    error_number = ctypes.get_errno()
    raise OSError(error_number, os.strerror(error_number), first_name)


def _linux_relative_directory_metadata(parent_fd: int, name: str) -> os.stat_result:
    metadata = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    if not stat.S_ISDIR(metadata.st_mode):
        raise DirectoryPublishError(
            f"published destination is not the promoted directory: {name}"
        )
    return metadata


def _linux_require_relative_absent(parent_fd: int, name: str, label: str) -> None:
    try:
        os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    raise DirectoryPublishError(f"{label} still exists after directory promotion")


def _linux_quarantine(
    promoted_fd: int,
    source_parent_fd: int,
    destination_parent_fd: int,
    destination_name: str,
    expected_identity: Identity,
    private_mode: int,
    sync_fd: Callable[[int], None],
    *,
    quarantine_any_official: bool = False,
) -> tuple[str, str]:
    cleanup_errors: list[str] = []
    observed_identity: Identity | None = None
    official_absent = False
    try:
        current = _linux_relative_directory_metadata(
            destination_parent_fd, destination_name
        )
        observed_identity = _identity(current)
    except FileNotFoundError:
        current = None
        official_absent = True
    except BaseException as error:
        current = None
        cleanup_errors.append(f"destination-inspection={error}")

    # The retained handle is the only authority that is safe to modify even if
    # the official pathname was displaced or replaced. Restore privacy first,
    # but do not let a mode/sync fault prevent the namespace quarantine attempt.
    try:
        os.fchmod(promoted_fd, private_mode)
    except OSError as error:
        cleanup_errors.append(f"private-mode={error}")

    quarantine_name: str | None = None
    quarantine_status = "already-absent"
    if (
        observed_identity is not None
        and observed_identity != expected_identity
        and not quarantine_any_official
    ):
        quarantine_status = "foreign-destination-retained"
        cleanup_errors.append(
            "foreign-destination-retained-without-rename-or-mode-change"
        )
    elif current is None and not official_absent:
        quarantine_status = "unknown-destination-retained"
        cleanup_errors.append(
            "destination-identity-unknown-retained-without-rename-or-mode-change"
        )
    else:
        for _ in range(32):
            candidate = (
                f".{destination_name}.publication-failed.{secrets.token_hex(8)}"
            )
            try:
                _linux_rename_noreplace(
                    destination_parent_fd,
                    destination_name,
                    destination_parent_fd,
                    candidate,
                )
            except DirectoryPublishError as error:
                if "existing destination" in str(error):
                    continue
                raise
            except FileNotFoundError:
                official_absent = True
                break
            quarantine_name = candidate
            quarantine_status = candidate
            break
        if quarantine_name is None and not official_absent:
            raise DirectoryPublishError(
                "cannot allocate a unique failed-directory-publication quarantine"
            )

    if quarantine_name is not None:
        try:
            quarantined = _linux_relative_directory_metadata(
                destination_parent_fd, quarantine_name
            )
            quarantined_identity = _identity(quarantined)
            if (
                observed_identity is not None
                and quarantined_identity != observed_identity
            ):
                cleanup_errors.append("quarantine-identity-changed-during-rename")
                if quarantined_identity == expected_identity:
                    # A race placed the exact promoted object back at the official
                    # name. Keeping that exact object quarantined is the safe result.
                    cleanup_errors.append("raced-exact-publication-quarantined")
                else:
                    # The inspected name was swapped before rename. Put a raced
                    # foreign object back if the official name is still empty;
                    # never chmod it.
                    try:
                        _linux_rename_noreplace(
                            destination_parent_fd,
                            quarantine_name,
                            destination_parent_fd,
                            destination_name,
                        )
                        quarantine_status = "raced-destination-restored"
                        quarantine_name = None
                    except BaseException as restore_error:
                        cleanup_errors.append(
                            f"raced-destination-restore={restore_error}"
                        )
            elif quarantined_identity != expected_identity:
                cleanup_errors.append(
                    "untrusted-promoted-destination-quarantined-without-mode-change"
                )
        except BaseException as error:
            cleanup_errors.append(f"quarantine-inspection={error}")

    for label, descriptor in (
        ("promoted-sync", promoted_fd),
        ("source-parent-sync", source_parent_fd),
        ("destination-parent-sync", destination_parent_fd),
    ):
        try:
            sync_fd(descriptor)
        except OSError as error:
            cleanup_errors.append(f"{label}={error}")
    return quarantine_status, "; ".join(cleanup_errors) or "none"


def _linux_publish(
    staged: Path,
    destination: Path,
    *,
    expected_staged_identity: Identity,
    expected_staging_parent_identity: Identity,
    expected_destination_parent_identity: Identity,
    private_mode: int,
    final_mode: int,
    post_commit_verify: Callable[[Path, int | None], None] | None,
    after_handles_opened: Callable[[], None] | None,
    before_commit: Callable[[], None] | None,
    after_native_rename: Callable[[], None] | None,
    after_commit: Callable[[], None] | None,
    sync_fd: Callable[[int], None],
) -> tuple[str, str]:
    directory_flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    source_parent_fd = os.open(staged.parent, directory_flags)
    destination_parent_fd = -1
    promoted_fd = -1
    committed = False
    rename_invoked = False
    rename_returned_success = False
    rename_identity_verified = False
    final_mode_prepared = False
    try:
        destination_parent_fd = os.open(destination.parent, directory_flags)
        promoted_fd = os.open(staged.name, directory_flags, dir_fd=source_parent_fd)
        source_parent_status = os.fstat(source_parent_fd)
        destination_parent_status = os.fstat(destination_parent_fd)
        promoted_status = os.fstat(promoted_fd)
        _require_identity(
            source_parent_status,
            expected_staging_parent_identity,
            "opened staging parent",
        )
        _require_identity(
            destination_parent_status,
            expected_destination_parent_identity,
            "opened destination parent",
        )
        _require_identity(
            promoted_status, expected_staged_identity, "opened staged directory"
        )
        if stat.S_IMODE(source_parent_status.st_mode) != private_mode:
            raise DirectoryPublishError(
                f"opened staging parent must retain private mode {private_mode:04o}"
            )
        if not stat.S_ISDIR(promoted_status.st_mode):
            raise DirectoryPublishError("opened staged object is not a directory")
        if stat.S_IMODE(promoted_status.st_mode) != private_mode:
            raise DirectoryPublishError(
                f"staged directory must retain private mode {private_mode:04o}"
            )
        _linux_require_relative_absent(
            destination_parent_fd, destination.name, "destination"
        )
        sync_fd(promoted_fd)
        sync_fd(source_parent_fd)
        sync_fd(destination_parent_fd)
        if after_handles_opened is not None:
            after_handles_opened()

        # Bind both namespace paths to the parent handles admitted above.  A
        # later rename of either parent is detected again after promotion.
        _require_identity(
            _directory_metadata(staged.parent, "staging parent"),
            expected_staging_parent_identity,
            "staging parent path",
        )
        _require_identity(
            _directory_metadata(destination.parent, "destination parent"),
            expected_destination_parent_identity,
            "destination parent path",
        )
        current = _linux_relative_directory_metadata(source_parent_fd, staged.name)
        _require_identity(current, expected_staged_identity, "staged directory name")
        if stat.S_IMODE(os.fstat(source_parent_fd).st_mode) != private_mode:
            raise DirectoryPublishError(
                "staging parent became searchable before final mode preparation"
            )

        # The owner-only parent keeps these release-public modes inaccessible
        # until the atomic rename makes the exact directory official.
        final_mode_prepared = True
        os.fchmod(promoted_fd, final_mode)
        sync_fd(promoted_fd)
        if before_commit is not None:
            before_commit()
        source_parent_status = os.fstat(source_parent_fd)
        _require_identity(
            source_parent_status,
            expected_staging_parent_identity,
            "final staging parent handle",
        )
        if stat.S_IMODE(source_parent_status.st_mode) != private_mode:
            raise DirectoryPublishError(
                "staging parent became searchable at the commit boundary"
            )
        _require_identity(
            _linux_relative_directory_metadata(source_parent_fd, staged.name),
            expected_staged_identity,
            "final staged directory name",
        )
        _linux_require_relative_absent(
            destination_parent_fd, destination.name, "final destination"
        )
        rename_invoked = True
        _linux_rename_noreplace(
            source_parent_fd,
            staged.name,
            destination_parent_fd,
            destination.name,
        )
        rename_returned_success = True
        if after_native_rename is not None:
            after_native_rename()
        committed = True
        published = _linux_relative_directory_metadata(
            destination_parent_fd, destination.name
        )
        _require_identity(
            published, expected_staged_identity, "newly promoted directory"
        )
        rename_identity_verified = True
        if after_commit is not None:
            after_commit()

        published = _linux_relative_directory_metadata(
            destination_parent_fd, destination.name
        )
        _require_identity(published, expected_staged_identity, "published directory")
        _linux_require_relative_absent(
            source_parent_fd, staged.name, "staging directory name"
        )
        _require_identity(
            _directory_metadata(staged.parent, "staging parent"),
            expected_staging_parent_identity,
            "staging parent path",
        )
        _require_identity(
            _directory_metadata(destination.parent, "destination parent"),
            expected_destination_parent_identity,
            "destination parent path",
        )
        if post_commit_verify is not None:
            post_commit_verify(destination, promoted_fd)
        published = _linux_relative_directory_metadata(
            destination_parent_fd, destination.name
        )
        _require_identity(published, expected_staged_identity, "verified publication")
        if stat.S_IMODE(published.st_mode) != final_mode:
            raise DirectoryPublishError(
                f"published directory mode is not canonical {final_mode:04o}"
            )
        sync_fd(promoted_fd)
        sync_fd(source_parent_fd)
        sync_fd(destination_parent_fd)
        _require_identity(
            _directory_metadata(destination.parent, "destination parent"),
            expected_destination_parent_identity,
            "durable destination parent path",
        )
        final = destination.lstat()
        _require_identity(final, expected_staged_identity, "final destination path")
        return "pass", "removed"
    except BaseException as error:
        if not committed and rename_invoked:
            committed = rename_returned_success
            if not committed:
                try:
                    destination_after_error = _linux_relative_directory_metadata(
                        destination_parent_fd, destination.name
                    )
                    committed = (
                        _identity(destination_after_error) == expected_staged_identity
                    )
                except (FileNotFoundError, OSError, DirectoryPublishError):
                    pass
        if committed:
            try:
                quarantine, cleanup_debt = _linux_quarantine(
                    promoted_fd,
                    source_parent_fd,
                    destination_parent_fd,
                    destination.name,
                    expected_staged_identity,
                    private_mode,
                    sync_fd,
                    quarantine_any_official=not rename_identity_verified,
                )
            except BaseException as quarantine_error:
                raise DirectoryPublishError(
                    "directory publication failed after atomic promotion and exact "
                    f"quarantine was incomplete: publication={error}; "
                    f"quarantine={quarantine_error}"
                ) from error
            if isinstance(error, (KeyboardInterrupt, SystemExit)):
                raise
            raise DirectoryPublishError(
                "directory publication failed after atomic promotion; official "
                f"destination disposition={quarantine}; cleanup debt="
                f"{cleanup_debt}: {error}"
            ) from error
        if final_mode_prepared and promoted_fd >= 0:
            try:
                os.fchmod(promoted_fd, private_mode)
                sync_fd(promoted_fd)
                sync_fd(source_parent_fd)
            except OSError as restore_error:
                raise DirectoryPublishError(
                    "directory publication failed before promotion and private mode "
                    f"restoration failed: publication={error}; restore={restore_error}"
                ) from error
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        if isinstance(error, DirectoryPublishError):
            raise
        raise DirectoryPublishError(f"atomic directory publication failed: {error}") from error
    finally:
        if promoted_fd >= 0:
            try:
                os.close(promoted_fd)
            except OSError:
                pass
        if destination_parent_fd >= 0:
            try:
                os.close(destination_parent_fd)
            except OSError:
                pass
        try:
            os.close(source_parent_fd)
        except OSError:
            pass


def _windows_publish(
    staged: Path,
    destination: Path,
    *,
    expected_staged_identity: Identity,
    expected_staging_parent_identity: Identity,
    expected_destination_parent_identity: Identity,
    post_commit_verify: Callable[[Path, int | None], None] | None,
    after_handles_opened: Callable[[], None] | None,
    before_commit: Callable[[], None] | None,
    after_native_rename: Callable[[], None] | None,
    after_commit: Callable[[], None] | None,
    flush_override: Callable[[int], None] | None,
    acl_override: Callable[[int, str], None] | None,
    access_transition: Callable[[Path, bool], None] | None,
    private_access_verify: Callable[[Path], None] | None,
) -> tuple[str, str]:
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class FileTime(ctypes.Structure):
        _fields_ = (("low", ctypes.c_uint32), ("high", ctypes.c_uint32))

    class HandleInformation(ctypes.Structure):
        _fields_ = (
            ("attributes", ctypes.c_uint32),
            ("creation_time", FileTime),
            ("last_access_time", FileTime),
            ("last_write_time", FileTime),
            ("volume_serial", ctypes.c_uint32),
            ("file_size_high", ctypes.c_uint32),
            ("file_size_low", ctypes.c_uint32),
            ("link_count", ctypes.c_uint32),
            ("file_index_high", ctypes.c_uint32),
            ("file_index_low", ctypes.c_uint32),
        )

    class RenameInformation(ctypes.Structure):
        _fields_ = (
            ("replace_if_exists", ctypes.c_ubyte),
            ("root_directory", ctypes.c_void_p),
            ("file_name_length", ctypes.c_uint32),
            ("file_name", ctypes.c_uint16 * 1),
        )

    class IoStatusBlock(ctypes.Structure):
        _fields_ = (
            ("status", ctypes.c_void_p),
            ("information", ctypes.c_size_t),
        )

    create_file = kernel32.CreateFileW
    create_file.argtypes = (
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    )
    create_file.restype = ctypes.c_void_p
    get_information = kernel32.GetFileInformationByHandle
    get_information.argtypes = (
        ctypes.c_void_p,
        ctypes.POINTER(HandleInformation),
    )
    get_information.restype = ctypes.c_int
    flush = kernel32.FlushFileBuffers
    flush.argtypes = (ctypes.c_void_p,)
    flush.restype = ctypes.c_int
    ntdll = ctypes.WinDLL("ntdll")
    set_information = ntdll.NtSetInformationFile
    set_information.argtypes = (
        ctypes.c_void_p,
        ctypes.POINTER(IoStatusBlock),
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_int,
    )
    set_information.restype = ctypes.c_long
    status_to_dos_error = ntdll.RtlNtStatusToDosError
    status_to_dos_error.argtypes = (ctypes.c_long,)
    status_to_dos_error.restype = ctypes.c_uint32
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int

    generic_read = 0x80000000
    generic_write = 0x40000000
    delete_access = 0x00010000
    read_control = 0x00020000
    write_dac = 0x00040000
    share_read_write = 0x00000001 | 0x00000002
    share_read = 0x00000001
    open_existing = 3
    backup_semantics = 0x02000000
    open_reparse_point = 0x00200000
    invalid_handle = ctypes.c_void_p(-1).value

    def open_directory(path: Path, *, movable: bool) -> int:
        access = (
            generic_read
            | generic_write
            | read_control
            | (delete_access | write_dac if movable else 0)
        )
        sharing = share_read if movable else share_read_write
        handle = create_file(
            str(path),
            access,
            sharing,
            None,
            open_existing,
            backup_semantics | open_reparse_point,
            None,
        )
        if handle == invalid_handle:
            raise ctypes.WinError(ctypes.get_last_error())
        return handle

    def information(handle: int, label: str) -> tuple[HandleInformation, Identity]:
        value = HandleInformation()
        if not get_information(handle, ctypes.byref(value)):
            raise ctypes.WinError(ctypes.get_last_error())
        directory_attribute = 0x00000010
        reparse_attribute = 0x00000400
        if not value.attributes & directory_attribute or value.attributes & reparse_attribute:
            raise DirectoryPublishError(f"{label} is not a non-reparse directory")
        return value, (
            value.volume_serial,
            (value.file_index_high << 32) | value.file_index_low,
        )

    def flush_handle(handle: int) -> None:
        if flush_override is not None:
            flush_override(handle)
            return
        if not flush(handle):
            raise ctypes.WinError(ctypes.get_last_error())

    def set_acl(handle: int, sddl: str) -> None:
        (acl_override or _windows_set_directory_acl)(handle, sddl)

    def rename_by_handle(
        handle: int, destination_parent: int, destination_name: str
    ) -> None:
        if destination_name in {"", ".", ".."} or Path(destination_name).name != destination_name:
            raise DirectoryPublishError("Windows destination must be one flat name")
        encoded_name = destination_name.encode("utf-16-le")
        offset = RenameInformation.file_name.offset
        buffer = ctypes.create_string_buffer(
            offset + len(encoded_name) + ctypes.sizeof(ctypes.c_uint16)
        )
        rename = RenameInformation.from_buffer(buffer)
        rename.replace_if_exists = 0
        rename.root_directory = destination_parent
        rename.file_name_length = len(encoded_name)
        ctypes.memmove(
            ctypes.addressof(buffer) + offset,
            encoded_name,
            len(encoded_name),
        )
        io_status = IoStatusBlock()
        status = set_information(
            handle,
            ctypes.byref(io_status),
            ctypes.addressof(buffer),
            len(buffer),
            10,  # FileRenameInformation
        )
        if status != 0:
            error_number = status_to_dos_error(status)
            if error_number in (80, 183):
                raise DirectoryPublishError(
                    f"refusing to replace existing destination: {destination_name}"
                )
            raise ctypes.WinError(error_number)

    source_parent_handle = open_directory(staged.parent, movable=False)
    destination_parent_handle = -1
    promoted_handle = -1
    committed = False
    rename_invoked = False
    rename_returned_success = False
    public_acl_prepared = False
    try:
        destination_parent_handle = open_directory(destination.parent, movable=False)
        promoted_handle = open_directory(staged, movable=True)
        _, source_parent_identity = information(
            source_parent_handle, "opened staging parent"
        )
        _, destination_parent_identity = information(
            destination_parent_handle, "opened destination parent"
        )
        _, promoted_identity = information(promoted_handle, "opened staged directory")
        if source_parent_identity != expected_staging_parent_identity:
            raise DirectoryPublishError("opened staging parent identity changed")
        if destination_parent_identity != expected_destination_parent_identity:
            raise DirectoryPublishError("opened destination parent identity changed")
        if promoted_identity != expected_staged_identity:
            raise DirectoryPublishError("opened staged directory identity changed")
        _destination_absent(destination)
        flush_handle(promoted_handle)
        flush_handle(source_parent_handle)
        flush_handle(destination_parent_handle)
        if after_handles_opened is not None:
            after_handles_opened()
        _require_identity(
            _directory_metadata(staged.parent, "staging parent"),
            expected_staging_parent_identity,
            "staging parent path",
        )
        _require_identity(
            _directory_metadata(destination.parent, "destination parent"),
            expected_destination_parent_identity,
            "destination parent path",
        )
        _require_identity(
            _directory_metadata(staged, "staged directory"),
            expected_staged_identity,
            "staged directory path",
        )
        if private_access_verify is not None:
            private_access_verify(staged.parent)
            private_access_verify(staged)
        # Prepare Windows-equivalent public read/execute access while the
        # protected staging parent still makes the tree unreachable. The DACL
        # contains inheritable ACEs, so ordinary payloads transition together.
        public_acl_prepared = True
        set_acl(promoted_handle, WINDOWS_PUBLIC_DIRECTORY_SDDL)
        if access_transition is not None:
            access_transition(staged, True)
        flush_handle(promoted_handle)
        if before_commit is not None:
            before_commit()
        _require_identity(
            _directory_metadata(staged.parent, "final staging parent"),
            expected_staging_parent_identity,
            "final staging parent path",
        )
        _require_identity(
            _directory_metadata(staged, "final staged directory"),
            expected_staged_identity,
            "final staged directory path",
        )
        if private_access_verify is not None:
            private_access_verify(staged.parent)
        _destination_absent(destination)
        rename_invoked = True
        rename_by_handle(
            promoted_handle, destination_parent_handle, destination.name
        )
        rename_returned_success = True
        if after_native_rename is not None:
            after_native_rename()
        committed = True
        _require_identity(
            _directory_metadata(destination, "newly promoted directory"),
            expected_staged_identity,
            "newly promoted directory path",
        )
        if after_commit is not None:
            after_commit()
        _require_identity(
            _directory_metadata(destination, "published directory"),
            expected_staged_identity,
            "published directory path",
        )
        if os.path.lexists(staged):
            raise DirectoryPublishError(
                "staging directory name still exists after directory promotion"
            )
        if post_commit_verify is not None:
            # The native handle denies rename/delete sharing for the promoted
            # directory, so the admitted Windows path remains handle-bound.
            post_commit_verify(destination, None)
        _require_identity(
            _directory_metadata(destination, "verified publication"),
            expected_staged_identity,
            "verified publication path",
        )
        flush_handle(promoted_handle)
        flush_handle(source_parent_handle)
        flush_handle(destination_parent_handle)
        _require_identity(
            _directory_metadata(destination.parent, "destination parent"),
            expected_destination_parent_identity,
            "durable destination parent path",
        )
        return "pass", "removed"
    except BaseException as error:
        if not committed and rename_invoked:
            committed = rename_returned_success
            if not committed:
                try:
                    destination_after_error = _directory_metadata(
                        destination, "possibly promoted directory"
                    )
                    committed = (
                        _identity(destination_after_error) == expected_staged_identity
                    )
                except (OSError, DirectoryPublishError):
                    pass
        if committed:
            cleanup_errors: list[str] = []
            try:
                set_acl(promoted_handle, WINDOWS_PRIVATE_DIRECTORY_SDDL)
                if access_transition is not None:
                    access_transition(destination, False)
            except BaseException as acl_error:
                cleanup_errors.append(f"private-acl={acl_error}")
            try:
                quarantine: Path | None = None
                for _ in range(32):
                    candidate = staged.parent / (
                        f".{destination.name}.publication-failed."
                        f"{secrets.token_hex(8)}"
                    )
                    try:
                        rename_by_handle(
                            promoted_handle,
                            source_parent_handle,
                            candidate.name,
                        )
                    except DirectoryPublishError as collision:
                        if "existing destination" in str(collision):
                            continue
                        raise
                    quarantine = candidate
                    break
                if quarantine is None:
                    raise DirectoryPublishError(
                        "cannot allocate a unique failed-publication quarantine"
                    )
            except BaseException as quarantine_error:
                raise DirectoryPublishError(
                    "directory publication failed after atomic promotion and exact "
                    f"quarantine was incomplete: publication={error}; "
                    f"quarantine={quarantine_error}"
                ) from error
            for label, handle in (
                ("promoted-sync", promoted_handle),
                ("source-parent-sync", source_parent_handle),
                ("destination-parent-sync", destination_parent_handle),
            ):
                try:
                    flush_handle(handle)
                except OSError as sync_error:
                    cleanup_errors.append(f"{label}={sync_error}")
            if isinstance(error, (KeyboardInterrupt, SystemExit)):
                raise
            raise DirectoryPublishError(
                "directory publication failed after atomic promotion; official "
                f"destination quarantined privately as {quarantine}; cleanup debt="
                f"{'; '.join(cleanup_errors) or 'none'}: {error}"
            ) from error
        if public_acl_prepared:
            try:
                set_acl(promoted_handle, WINDOWS_PRIVATE_DIRECTORY_SDDL)
                if access_transition is not None:
                    access_transition(staged, False)
                flush_handle(promoted_handle)
                flush_handle(source_parent_handle)
            except BaseException as restore_error:
                raise DirectoryPublishError(
                    "directory publication failed before promotion and private ACL "
                    f"restoration failed: publication={error}; restore={restore_error}"
                ) from error
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        if isinstance(error, DirectoryPublishError):
            raise
        raise DirectoryPublishError(f"atomic directory publication failed: {error}") from error
    finally:
        # A durable commit must not be relabelled as failure by a close error.
        if promoted_handle >= 0:
            close_handle(promoted_handle)
        if destination_parent_handle >= 0:
            close_handle(destination_parent_handle)
        close_handle(source_parent_handle)


def atomic_publish_directory(
    staged: Path,
    destination: Path,
    *,
    expected_staged_identity: Identity,
    expected_staging_parent_identity: Identity,
    expected_destination_parent_identity: Identity,
    private_mode: int = 0o700,
    final_mode: int = 0o755,
    post_commit_verify: Callable[[Path, int | None], None] | None = None,
    _after_handles_opened: Callable[[], None] | None = None,
    _before_commit: Callable[[], None] | None = None,
    _after_native_rename: Callable[[], None] | None = None,
    _after_commit: Callable[[], None] | None = None,
    _sync_fd: Callable[[int], None] = os.fsync,
    _flush_windows_handle: Callable[[int], None] | None = None,
    _set_windows_acl: Callable[[int, str], None] | None = None,
    _windows_access_transition: Callable[[Path, bool], None] | None = None,
    _windows_private_access_verify: Callable[[Path], None] | None = None,
) -> tuple[str, str]:
    """Publish ``staged`` as ``destination`` with strict no-replace semantics."""
    staged = Path(os.path.abspath(staged))
    destination = Path(os.path.abspath(destination))
    if staged.name in {"", ".", ".."} or destination.name in {"", ".", ".."}:
        raise DirectoryPublishError("publication paths must name exact directories")
    staged_status = _directory_metadata(staged, "staged directory")
    staging_parent_status = _directory_metadata(staged.parent, "staging parent")
    destination_parent_status = _directory_metadata(
        destination.parent, "destination parent"
    )
    _require_identity(
        staged_status, expected_staged_identity, "staged directory"
    )
    _require_identity(
        staging_parent_status,
        expected_staging_parent_identity,
        "staging parent",
    )
    _require_identity(
        destination_parent_status,
        expected_destination_parent_identity,
        "destination parent",
    )
    _require_private_staging_parent(staging_parent_status, staged.parent)
    if staged_status.st_dev != destination_parent_status.st_dev:
        raise DirectoryPublishError(
            "staged directory and destination parent must share one filesystem"
        )
    _destination_absent(destination)
    if os.name == "nt":
        return _windows_publish(
            staged,
            destination,
            expected_staged_identity=expected_staged_identity,
            expected_staging_parent_identity=expected_staging_parent_identity,
            expected_destination_parent_identity=expected_destination_parent_identity,
            post_commit_verify=post_commit_verify,
            after_handles_opened=_after_handles_opened,
            before_commit=_before_commit,
            after_native_rename=_after_native_rename,
            after_commit=_after_commit,
            flush_override=_flush_windows_handle,
            acl_override=_set_windows_acl,
            access_transition=_windows_access_transition,
            private_access_verify=_windows_private_access_verify,
        )
    if not sys.platform.startswith("linux"):
        raise DirectoryPublishError(
            f"atomic no-replace directory publication is unsupported on {sys.platform}"
        )
    return _linux_publish(
        staged,
        destination,
        expected_staged_identity=expected_staged_identity,
        expected_staging_parent_identity=expected_staging_parent_identity,
        expected_destination_parent_identity=expected_destination_parent_identity,
        private_mode=private_mode,
        final_mode=final_mode,
        post_commit_verify=post_commit_verify,
        after_handles_opened=_after_handles_opened,
        before_commit=_before_commit,
        after_native_rename=_after_native_rename,
        after_commit=_after_commit,
        sync_fd=_sync_fd,
    )
