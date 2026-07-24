#!/usr/bin/env python3
"""Publish one exact file without replacing an existing path.

Exact no-replace commits are implemented for Linux and Windows. Other hosts
fail before publication instead of weakening the inode-identity boundary.
"""

from __future__ import annotations

import argparse
import ctypes
import errno
import os
import secrets
import signal
import stat
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Sequence

from durable_file_io import fsync_directory


class PublishError(ValueError):
    """The requested atomic publication boundary is unsafe or incomplete."""


class DestinationExistsError(PublishError):
    """Atomic no-replace publication found a competing destination."""


def _neutralize_failed_stream(stream: object) -> None:
    """Redirect a failed buffered stream so interpreter shutdown cannot retry it."""
    descriptor: int | None = None
    null_descriptor: int | None = None
    try:
        descriptor = stream.fileno()  # type: ignore[attr-defined]
        null_descriptor = os.open(os.devnull, os.O_WRONLY)
        os.dup2(null_descriptor, descriptor)
    except Exception:
        pass
    finally:
        if null_descriptor is not None and null_descriptor != descriptor:
            try:
                os.close(null_descriptor)
            except OSError:
                pass


def warn_after_commit(message: str) -> None:
    """Emit cleanup debt without turning a durable commit into false failure."""
    try:
        print(message, file=sys.stderr)
    except Exception:
        _neutralize_failed_stream(sys.stderr)


def report_after_commit(lines: Sequence[str]) -> None:
    """Flush committed-result reporting without allowing a false process failure."""
    try:
        for line in lines:
            print(line)
        sys.stdout.flush()
    except Exception:
        _neutralize_failed_stream(sys.stdout)


class CommitSignalGuard:
    """Defer process termination across one caller-defined commit boundary."""

    def __init__(self, boundary: str, error_type: type[Exception]) -> None:
        self.boundary = boundary
        self.error_type = error_type
        self.committed = False
        self.pending: int | None = None
        self.previous: dict[int, object] = {}

    def _handler(self, signum: int, _frame: object) -> None:
        self.pending = signum

    def __enter__(self) -> CommitSignalGuard:
        handled = {
            value
            for name in ("SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK")
            if (value := getattr(signal, name, None)) is not None
        }
        for signum in handled:
            try:
                self.previous[signum] = signal.getsignal(signum)
                signal.signal(signum, self._handler)
            except (AttributeError, OSError, ValueError):
                self.previous.pop(signum, None)
        return self

    def refuse_pending_before_commit(self) -> None:
        if self.pending is not None:
            raise self.error_type(
                f"received signal {self.pending} before {self.boundary}"
            )

    def mark_committed(self) -> None:
        self.committed = True

    def __exit__(self, kind: object, _value: object, _traceback: object) -> None:
        for signum, handler in self.previous.items():
            try:
                signal.signal(signum, handler)
            except (OSError, ValueError):
                pass
        if self.committed and self.pending is not None:
            warn_after_commit(
                f"WARNING: ignored signal {self.pending} after {self.boundary}"
            )
        elif kind is None and self.pending is not None:
            raise self.error_type(
                f"received signal {self.pending} before {self.boundary}"
            )


def _neutralize_failed_staging(staged: Path, reason: OSError) -> Path:
    try:
        with staged.open("r+b") as handle:
            handle.truncate(0)
            handle.flush()
            os.fsync(handle.fileno())
    except OSError as neutralize_error:
        raise PublishError(
            "cannot quarantine or neutralize failed staging: "
            f"quarantine={reason}; truncate={neutralize_error}"
        ) from reason
    return staged


def quarantine_failed_staging(staged: Path, destination: Path) -> Path | None:
    """Move failed pass-shaped staging bytes out of the producer namespace."""
    if not os.path.lexists(staged):
        return None
    require_regular_nonsymlink(staged, "failed staged file")
    try:
        staged_parent = staged.parent.resolve(strict=True)
        destination_parent = destination.parent.resolve(strict=True)
    except OSError as error:
        raise PublishError(f"failed staging parent is unavailable: {error}") from error
    if staged_parent != destination_parent:
        raise PublishError("failed staging must share the destination directory")

    try:
        quarantine_directory = Path(
            tempfile.mkdtemp(
                prefix=f".{destination.name}.publication-failed.",
                dir=destination_parent,
            )
        )
        os.chmod(quarantine_directory, 0o700)
    except OSError as quarantine_error:
        return _neutralize_failed_staging(staged, quarantine_error)
    quarantine = quarantine_directory / "staged"
    try:
        os.rename(staged, quarantine)
    except OSError as rename_error:
        neutralized = _neutralize_failed_staging(staged, rename_error)
        try:
            quarantine_directory.rmdir()
            fsync_directory(destination_parent)
        except OSError:
            pass
        return neutralized
    fsync_directory(quarantine_directory)
    fsync_directory(destination_parent)
    return quarantine


def _directory_sync_status(path: Path) -> str:
    try:
        fsync_directory(path)
    except OSError as error:
        if error.errno not in {errno.EINVAL, errno.ENOTSUP, errno.EBADF}:
            raise
        return f"unsupported_errno_{error.errno}"
    return "pass"


def _quarantine_published_destination(
    destination: Path,
    destination_parent: Path,
    staged_identity: tuple[int, int],
    sync_directory: Callable[[Path], str],
) -> tuple[str, Path]:
    try:
        destination_stat = destination.lstat()
    except OSError as error:
        raise PublishError(
            f"cannot inspect failed published destination: {error}"
        ) from error
    destination_identity = (destination_stat.st_dev, destination_stat.st_ino)
    if destination_identity != staged_identity or not stat.S_ISREG(
        destination_stat.st_mode
    ):
        raise PublishError(
            "failed published destination changed identity; refusing unsafe cleanup"
        )
    quarantine: Path | None = None
    for _attempt in range(16):
        candidate = destination.with_name(
            f".{destination.name}.publication-failed.{secrets.token_hex(8)}"
        )
        if os.path.lexists(candidate):
            continue
        try:
            if os.name == "nt":
                os.rename(destination, candidate)
            else:
                os.link(destination, candidate, follow_symlinks=False)
                candidate_stat = candidate.lstat()
                if (
                    candidate_stat.st_dev,
                    candidate_stat.st_ino,
                ) != staged_identity:
                    candidate.unlink(missing_ok=True)
                    raise PublishError(
                        "failed publication quarantine changed identity"
                    )
                destination.unlink()
        except FileExistsError:
            continue
        except OSError as error:
            raise PublishError(
                f"cannot quarantine failed published destination: {error}"
            ) from error
        quarantine = candidate
        break
    if quarantine is None:
        raise PublishError("cannot allocate a unique failed-publication quarantine")
    quarantine_stat = quarantine.lstat()
    if (
        quarantine_stat.st_dev,
        quarantine_stat.st_ino,
    ) != staged_identity or not stat.S_ISREG(quarantine_stat.st_mode):
        raise PublishError("failed publication quarantine has the wrong identity")
    try:
        return sync_directory(destination_parent), quarantine
    except OSError as error:
        raise PublishError(
            "failed published destination was quarantined, but rollback directory "
            f"sync failed: {error}"
        ) from error


def require_regular_nonsymlink(path: Path, label: str) -> None:
    try:
        mode = path.lstat().st_mode
    except OSError as error:
        raise PublishError(f"{label} is unavailable: {path}: {error}") from error
    if not stat.S_ISREG(mode):
        raise PublishError(f"{label} must be a regular non-symlink file: {path}")


def _lstat(path: Path) -> os.stat_result:
    return path.lstat()


def _linux_move_noreplace(
    staged: Path,
    destination: Path,
    staged_fd: int,
    unlink_source: Callable[[Path], None],
) -> str:
    libc = ctypes.CDLL(None, use_errno=True)
    # Link through the already-fsynced descriptor, so staged-path substitution
    # cannot change the bytes being published. AT_SYMLINK_FOLLOW dereferences
    # the /proc descriptor link without requiring CAP_DAC_READ_SEARCH.
    linkat = getattr(libc, "linkat", None)
    if linkat is None:
        raise PublishError(
            "atomic no-replace link is unavailable on this Linux libc"
        )
    linkat.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
    )
    linkat.restype = ctypes.c_int
    at_fdcwd = -100
    at_symlink_follow = 0x400
    if linkat(
        at_fdcwd,
        os.fsencode(f"/proc/self/fd/{staged_fd}"),
        at_fdcwd,
        os.fsencode(destination),
        at_symlink_follow,
    ) != 0:
        error_number = ctypes.get_errno()
        if error_number in {errno.EEXIST, errno.ENOTEMPTY}:
            raise DestinationExistsError(
                f"refusing to replace competing destination: {destination}"
            )
        raise OSError(
            error_number,
            os.strerror(error_number),
            str(destination),
        )
    try:
        unlink_source(staged)
    except OSError as error:
        # The destination is already a complete, exact hard link and was not
        # allowed to replace anything. Treat source-name retirement as cleanup,
        # not as a false failure after successful publication.
        return f"retained_errno_{error.errno}"
    return "removed"


def _windows_move_noreplace(
    staged: Path,
    destination: Path,
    *,
    require_single_link: bool,
    expected_identity: tuple[int, int] | None,
    after_open: Callable[[], None] | None,
) -> tuple[tuple[int, int], str]:
    """Move the opened staging inode by handle, never by a re-resolved path."""
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
            ("replace_if_exists", ctypes.c_uint32),
            ("root_directory", ctypes.c_void_p),
            ("file_name_length", ctypes.c_uint32),
            ("file_name", ctypes.c_uint16 * 1),
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
    get_information.argtypes = (ctypes.c_void_p, ctypes.POINTER(HandleInformation))
    get_information.restype = ctypes.c_int
    flush_file = kernel32.FlushFileBuffers
    flush_file.argtypes = (ctypes.c_void_p,)
    flush_file.restype = ctypes.c_int
    set_information = kernel32.SetFileInformationByHandle
    set_information.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    )
    set_information.restype = ctypes.c_int
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int

    generic_read = 0x80000000
    generic_write = 0x40000000
    delete_access = 0x00010000
    file_share_read = 0x00000001
    open_existing = 3
    file_flag_open_reparse_point = 0x00200000
    invalid_handle = ctypes.c_void_p(-1).value
    handle = create_file(
        str(staged),
        generic_read | generic_write | delete_access,
        file_share_read,
        None,
        open_existing,
        file_flag_open_reparse_point,
        None,
    )
    if handle == invalid_handle:
        raise ctypes.WinError(ctypes.get_last_error())
    renamed = False
    close_error: OSError | None = None
    try:
        before = HandleInformation()
        if not get_information(handle, ctypes.byref(before)):
            raise ctypes.WinError(ctypes.get_last_error())
        file_attribute_directory = 0x00000010
        file_attribute_reparse_point = 0x00000400
        if before.attributes & (file_attribute_directory | file_attribute_reparse_point):
            raise PublishError("staged file must be a regular non-reparse file")
        if require_single_link and before.link_count != 1:
            raise PublishError(
                "strict publication requires a single-link staged file"
            )
        staged_stat = staged.lstat()
        identity = (staged_stat.st_dev, staged_stat.st_ino)
        if expected_identity is not None and identity != expected_identity:
            raise PublishError("staged file identity changed before publication")
        if not stat.S_ISREG(staged_stat.st_mode):
            raise PublishError("staged file must remain a regular file")
        if not flush_file(handle):
            raise ctypes.WinError(ctypes.get_last_error())
        if after_open is not None:
            after_open()

        encoded_name = str(destination).encode("utf-16-le")
        name_offset = RenameInformation.file_name.offset
        rename_buffer = ctypes.create_string_buffer(
            name_offset + len(encoded_name) + ctypes.sizeof(ctypes.c_uint16)
        )
        rename = RenameInformation.from_buffer(rename_buffer)
        rename.replace_if_exists = 0
        rename.root_directory = None
        rename.file_name_length = len(encoded_name)
        ctypes.memmove(
            ctypes.addressof(rename_buffer) + name_offset,
            encoded_name,
            len(encoded_name),
        )
        file_rename_info = 3
        if not set_information(
            handle,
            file_rename_info,
            ctypes.addressof(rename_buffer),
            len(rename_buffer),
        ):
            error_number = ctypes.get_last_error()
            if error_number in {80, 183}:
                raise DestinationExistsError(
                    f"refusing to replace competing destination: {destination}"
                )
            raise ctypes.WinError(error_number)
        renamed = True

        staged_cleanup = "removed" if not os.path.lexists(staged) else "retained_or_replaced"
        return identity, staged_cleanup
    finally:
        if not close_handle(handle):
            close_error = ctypes.WinError(ctypes.get_last_error())
        if close_error is not None and not renamed and sys.exc_info()[0] is None:
            raise close_error


def atomic_publish(
    staged: Path,
    destination: Path,
    *,
    require_directory_sync: bool = False,
    require_staged_cleanup: bool = False,
    expected_staged_identity: tuple[int, int] | None = None,
    _unlink_source: Callable[[Path], None] = os.unlink,
    _sync_directory: Callable[[Path], str] = _directory_sync_status,
    _after_staged_open: Callable[[], None] | None = None,
    _lstat_destination: Callable[[Path], os.stat_result] = _lstat,
) -> tuple[str, str]:
    if require_staged_cleanup and not require_directory_sync:
        raise PublishError(
            "strict staging retirement requires strict destination directory sync"
        )
    require_regular_nonsymlink(staged, "staged file")
    try:
        staged_parent = staged.parent.resolve(strict=True)
        destination_parent = destination.parent.resolve(strict=True)
    except OSError as error:
        raise PublishError(f"publication parent is unavailable: {error}") from error
    if staged_parent != destination_parent:
        raise PublishError("staged file must share the destination directory")
    if destination.name in {"", ".", ".."}:
        raise PublishError("destination must name one exact file")
    if os.path.lexists(destination):
        raise DestinationExistsError(
            f"refusing to replace existing destination: {destination}"
        )

    if require_directory_sync:
        try:
            preflight_sync = _sync_directory(destination_parent)
        except OSError as error:
            raise PublishError(
                f"destination directory sync preflight failed: {error}"
            ) from error
        if preflight_sync != "pass":
            raise PublishError(
                "strict publication requires destination directory sync; "
                f"preflight returned {preflight_sync}"
            )

    staged_identity: tuple[int, int]
    try:
        if os.name == "nt":
            staged_identity, staged_cleanup = _windows_move_noreplace(
                staged,
                destination,
                require_single_link=require_staged_cleanup,
                expected_identity=expected_staged_identity,
                after_open=_after_staged_open,
            )
        else:
            with staged.open("rb") as handle:
                os.fsync(handle.fileno())
                staged_stat = os.fstat(handle.fileno())
                staged_identity = (staged_stat.st_dev, staged_stat.st_ino)
                if (
                    expected_staged_identity is not None
                    and staged_identity != expected_staged_identity
                ):
                    raise PublishError(
                        "staged file identity changed before publication"
                    )
                if require_staged_cleanup and staged_stat.st_nlink != 1:
                    raise PublishError(
                        "strict publication requires a single-link staged file"
                    )
                if _after_staged_open is not None:
                    _after_staged_open()
                if not sys.platform.startswith("linux"):
                    raise PublishError(
                        "atomic no-replace publication is unsupported on platform "
                        f"{sys.platform}"
                    )
                staged_cleanup = _linux_move_noreplace(
                    staged,
                    destination,
                    handle.fileno(),
                    _unlink_source,
                )
    except OSError as error:
        raise PublishError(f"atomic no-replace publication failed: {error}") from error

    try:
        directory_sync = _sync_directory(destination_parent)
    except OSError as error:
        try:
            retirement_sync, quarantine = _quarantine_published_destination(
                destination,
                destination_parent,
                staged_identity,
                _sync_directory,
            )
        except PublishError as retirement_error:
            raise PublishError(
                "destination directory sync failed after publication and "
                f"quarantine was incomplete: {retirement_error}"
            ) from error
        raise PublishError(
            "destination directory sync failed after publication; official "
            f"destination quarantined as {quarantine.name} with rollback sync "
            f"{retirement_sync}: {error}"
        ) from error
    if require_directory_sync and directory_sync != "pass":
        retirement_sync, quarantine = _quarantine_published_destination(
            destination,
            destination_parent,
            staged_identity,
            _sync_directory,
        )
        raise PublishError(
            "strict publication received unsupported post-publication directory "
            f"sync {directory_sync}; official destination quarantined as "
            f"{quarantine.name} with rollback sync {retirement_sync}"
        )
    if require_staged_cleanup:
        try:
            destination_stat = _lstat_destination(destination)
        except OSError as error:
            try:
                retirement_sync, quarantine = _quarantine_published_destination(
                    destination,
                    destination_parent,
                    staged_identity,
                    _sync_directory,
                )
            except PublishError as retirement_error:
                raise PublishError(
                    "cannot verify strict published destination identity and "
                    f"quarantine was incomplete: {retirement_error}"
                ) from error
            raise PublishError(
                "cannot verify strict published destination identity; official "
                f"destination quarantined as {quarantine.name} with rollback sync "
                f"{retirement_sync}: {error}"
            ) from error
        if (
            (destination_stat.st_dev, destination_stat.st_ino) != staged_identity
            or not stat.S_ISREG(destination_stat.st_mode)
            or destination_stat.st_nlink != 1
        ):
            try:
                retirement_sync, quarantine = _quarantine_published_destination(
                    destination,
                    destination_parent,
                    staged_identity,
                    _sync_directory,
                )
            except PublishError as retirement_error:
                raise PublishError(
                    "strict published destination identity is invalid and quarantine "
                    f"was incomplete: {retirement_error}"
                ) from retirement_error
            raise PublishError(
                "strict published destination identity is invalid; official "
                f"destination quarantined as {quarantine.name} with rollback sync "
                f"{retirement_sync}"
            )
    if require_staged_cleanup and staged_cleanup != "removed":
        retirement_sync, quarantine = _quarantine_published_destination(
            destination,
            destination_parent,
            staged_identity,
            _sync_directory,
        )
        raise PublishError(
            "strict publication could not retire the staging name; official "
            f"destination quarantined as {quarantine.name} with rollback sync "
            f"{retirement_sync}; cleanup={staged_cleanup}"
        )
    return directory_sync, staged_cleanup


def run_self_test() -> int:
    tests = 0
    with tempfile.TemporaryDirectory(prefix="dcent-atomic-publish-") as temporary:
        root = Path(temporary)
        destination = root / "manifest.json"
        staged = root / "manifest.json.tmp.first"
        staged.write_text("first", encoding="utf-8")
        atomic_publish(staged, destination)
        assert destination.read_text(encoding="utf-8") == "first"
        assert not staged.exists()
        tests += 1

        sync_fault_destination = root / "sync-fault.json"
        staged = root / "sync-fault.json.tmp.fixture"
        staged.write_text("must not remain official", encoding="utf-8")
        sync_calls = 0

        def fail_post_publish_sync(_path: Path) -> str:
            nonlocal sync_calls
            sync_calls += 1
            if sync_calls == 2:
                raise OSError(errno.EIO, "injected post-publication sync failure")
            return "pass"

        try:
            atomic_publish(
                staged,
                sync_fault_destination,
                require_directory_sync=True,
                _sync_directory=fail_post_publish_sync,
            )
        except PublishError:
            pass
        else:
            raise AssertionError("post-publication directory sync fault was accepted")
        assert sync_calls == 3
        assert not sync_fault_destination.exists()
        quarantines = list(
            root.glob(f".{sync_fault_destination.name}.publication-failed.*")
        )
        assert len(quarantines) == 1
        assert quarantines[0].read_text(encoding="utf-8") == (
            "must not remain official"
        )
        tests += 1

        verification_fault_destination = root / "verification-fault.json"
        staged = root / "verification-fault.json.tmp.fixture"
        staged.write_text("must be quarantined", encoding="utf-8")

        def fail_final_identity_read(_path: Path) -> os.stat_result:
            raise OSError(errno.EIO, "injected final identity read failure")

        try:
            atomic_publish(
                staged,
                verification_fault_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
                _lstat_destination=fail_final_identity_read,
            )
        except PublishError:
            pass
        else:
            raise AssertionError("final identity-read fault was accepted")
        assert not verification_fault_destination.exists()
        verification_quarantines = list(
            root.glob(f".{verification_fault_destination.name}.publication-failed.*")
        )
        assert len(verification_quarantines) == 1
        assert verification_quarantines[0].read_text(encoding="utf-8") == (
            "must be quarantined"
        )
        tests += 1

        unsupported_destination = root / "unsupported-sync.json"
        staged = root / "unsupported-sync.json.tmp.fixture"
        staged.write_text("must remain staged", encoding="utf-8")
        try:
            atomic_publish(
                staged,
                unsupported_destination,
                require_directory_sync=True,
                _sync_directory=lambda _path: "unsupported_fixture",
            )
        except PublishError:
            pass
        else:
            raise AssertionError("unsupported strict directory sync was accepted")
        assert staged.read_text(encoding="utf-8") == "must remain staged"
        assert not unsupported_destination.exists()
        tests += 1

        identity_destination = root / "identity-bound.json"
        staged = root / "identity-bound.json.tmp.fixture"
        staged.write_text("GOOD", encoding="utf-8")
        staged_status = staged.stat()
        expected_identity = (staged_status.st_dev, staged_status.st_ino)
        identity_parked = root / "identity-bound.json.tmp.parked"
        staged.rename(identity_parked)
        staged.write_text("EVIL", encoding="utf-8")
        try:
            atomic_publish(
                staged,
                identity_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
                expected_staged_identity=expected_identity,
            )
        except PublishError:
            pass
        else:
            raise AssertionError("strict publication accepted replaced staging")
        assert not identity_destination.exists()
        assert identity_parked.read_text(encoding="utf-8") == "GOOD"
        assert staged.read_text(encoding="utf-8") == "EVIL"
        tests += 1

        raced_destination = root / "raced-destination.json"
        staged = root / "raced-destination.json.tmp.fixture"
        staged.write_text("publisher-bytes", encoding="utf-8")
        staged_status = staged.stat()

        def create_raced_destination() -> None:
            raced_destination.write_text("operator-bytes", encoding="utf-8")

        try:
            atomic_publish(
                staged,
                raced_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
                expected_staged_identity=(staged_status.st_dev, staged_status.st_ino),
                _after_staged_open=create_raced_destination,
            )
        except DestinationExistsError:
            pass
        else:
            raise AssertionError("destination race replaced operator bytes")
        assert raced_destination.read_text(encoding="utf-8") == "operator-bytes"
        assert staged.read_text(encoding="utf-8") == "publisher-bytes"
        tests += 1

        callback_error_destination = root / "callback-error.json"
        staged = root / "callback-error.json.tmp.fixture"
        staged.write_text("must remain staged", encoding="utf-8")

        def raise_noncollision_file_exists() -> None:
            raise FileExistsError(errno.EEXIST, "injected callback failure")

        try:
            atomic_publish(
                staged,
                callback_error_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
                _after_staged_open=raise_noncollision_file_exists,
            )
        except DestinationExistsError as error:
            raise AssertionError(
                "callback FileExistsError was misclassified as a collision"
            ) from error
        except PublishError:
            pass
        else:
            raise AssertionError("callback FileExistsError was accepted")
        assert not callback_error_destination.exists()
        assert staged.read_text(encoding="utf-8") == "must remain staged"
        tests += 1

        unicode_destination = root / "dest-\N{LOCK}.json"
        staged = root / "unicode-destination.tmp.fixture"
        staged.write_text("unicode destination", encoding="utf-8")
        staged_status = staged.stat()
        atomic_publish(
            staged,
            unicode_destination,
            require_directory_sync=True,
            require_staged_cleanup=True,
            expected_staged_identity=(staged_status.st_dev, staged_status.st_ino),
        )
        assert unicode_destination.read_text(encoding="utf-8") == "unicode destination"
        assert not staged.exists()
        tests += 1

        linked_destination = root / "linked-staging.json"
        staged = root / "linked-staging.json.tmp.fixture"
        staged_alias = root / "linked-staging.json.tmp.alias"
        staged.write_text("single-link required", encoding="utf-8")
        os.link(staged, staged_alias)
        try:
            atomic_publish(
                staged,
                linked_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
            )
        except PublishError:
            pass
        else:
            raise AssertionError("strict publication accepted linked staging")
        assert not linked_destination.exists()
        assert staged.read_text(encoding="utf-8") == "single-link required"
        assert staged_alias.read_text(encoding="utf-8") == "single-link required"
        tests += 1

        substitution_destination = root / "substitution.json"
        staged = root / "substitution.json.tmp.fixture"
        parked = root / "substitution.json.tmp.parked"
        replacement = root / "substitution.json.tmp.replacement"
        staged.write_text("GOOD", encoding="utf-8")
        replacement.write_text("EVIL", encoding="utf-8")

        def substitute_staged_path() -> None:
            os.rename(staged, parked)
            os.rename(replacement, staged)

        try:
            atomic_publish(
                staged,
                substitution_destination,
                require_directory_sync=True,
                require_staged_cleanup=True,
                _after_staged_open=substitute_staged_path,
            )
        except (OSError, PublishError):
            pass
        else:
            raise AssertionError("strict publication accepted staging substitution")
        assert not substitution_destination.exists()
        assert not substitution_destination.is_symlink()
        assert any(
            candidate.exists() and candidate.read_text(encoding="utf-8") == "GOOD"
            for candidate in (staged, parked)
        )
        tests += 1

        staged = root / "manifest.json.tmp.second"
        staged.write_text("second", encoding="utf-8")
        try:
            atomic_publish(staged, destination)
        except DestinationExistsError:
            pass
        else:
            raise AssertionError("existing destination was replaced")
        assert destination.read_text(encoding="utf-8") == "first"
        assert staged.read_text(encoding="utf-8") == "second"
        tests += 1

        if sys.platform.startswith("linux"):
            retained_destination = root / "retained.json"
            staged = root / "retained.json.tmp.fixture"
            staged.write_text("published despite cleanup fault", encoding="utf-8")

            def reject_source_unlink(_path: Path) -> None:
                raise PermissionError(
                    errno.EACCES, "injected source unlink failure"
                )

            _, staged_cleanup = atomic_publish(
                staged,
                retained_destination,
                _unlink_source=reject_source_unlink,
            )
            assert staged_cleanup == f"retained_errno_{errno.EACCES}"
            assert retained_destination.read_text(encoding="utf-8") == (
                "published despite cleanup fault"
            )
            assert staged.read_text(encoding="utf-8") == (
                "published despite cleanup fault"
            )
            tests += 1

            strict_retirement_destination = root / "strict-retirement.json"
            staged = root / "strict-retirement.json.tmp.fixture"
            staged.write_text("must not remain official", encoding="utf-8")
            try:
                atomic_publish(
                    staged,
                    strict_retirement_destination,
                    require_directory_sync=True,
                    require_staged_cleanup=True,
                    _unlink_source=reject_source_unlink,
                )
            except PublishError:
                pass
            else:
                raise AssertionError("strict staging-retirement fault was accepted")
            assert not strict_retirement_destination.exists()
            assert staged.read_text(encoding="utf-8") == "must not remain official"
            strict_quarantines = list(
                root.glob(
                    f".{strict_retirement_destination.name}.publication-failed.*"
                )
            )
            assert len(strict_quarantines) == 1
            assert strict_quarantines[0].read_text(encoding="utf-8") == (
                "must not remain official"
            )
            tests += 1

        if os.name != "nt":
            dangling_destination = root / "dangling.json"
            dangling_destination.symlink_to(root / "missing-target.json")
            staged = root / "dangling.json.tmp.fixture"
            staged.write_text("must remain staged", encoding="utf-8")
            try:
                atomic_publish(staged, dangling_destination)
            except PublishError:
                pass
            else:
                raise AssertionError("dangling destination symlink was accepted")
            assert staged.read_text(encoding="utf-8") == "must remain staged"
            assert dangling_destination.is_symlink()
            tests += 1

        directory_destination = root / "directory.json"
        directory_destination.mkdir()
        staged = root / "directory.json.tmp.fixture"
        staged.write_text("never nested", encoding="utf-8")
        try:
            atomic_publish(staged, directory_destination)
        except PublishError:
            pass
        else:
            raise AssertionError("directory destination was accepted")
        assert staged.is_file()
        assert list(directory_destination.iterdir()) == []
        tests += 1

        outside = root / "outside"
        outside.mkdir()
        staged = outside / "manifest.tmp"
        staged.write_text("wrong parent", encoding="utf-8")
        try:
            atomic_publish(staged, destination)
        except PublishError:
            pass
        else:
            raise AssertionError("cross-directory staged file was accepted")
        assert staged.is_file()
        tests += 1

    print(f"atomic file publisher self-test passed: {tests} scenarios")
    return 0


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("staged", nargs="?", type=Path)
    parser.add_argument("destination", nargs="?", type=Path)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--require-directory-sync", action="store_true")
    parser.add_argument("--require-staged-cleanup", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        if (
            args.staged is not None
            or args.destination is not None
            or args.require_directory_sync
            or args.require_staged_cleanup
        ):
            parser.error("--self-test cannot be combined with publication arguments")
    elif args.staged is None or args.destination is None:
        parser.error("staged and destination paths are required")
    elif args.require_staged_cleanup and not args.require_directory_sync:
        parser.error("--require-staged-cleanup requires --require-directory-sync")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    try:
        directory_sync, staged_cleanup = atomic_publish(
            args.staged,
            args.destination,
            require_directory_sync=args.require_directory_sync,
            require_staged_cleanup=args.require_staged_cleanup,
        )
    except PublishError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("atomic_publish=fail", file=sys.stderr)
        return 1
    report_after_commit(
        (
            f"directory_sync={directory_sync}",
            f"staged_cleanup={staged_cleanup}",
            "atomic_publish=pass",
        )
    )
    if staged_cleanup != "removed":
        warn_after_commit(
            "WARN: published destination is complete but staged name remains: "
            f"{args.staged}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
