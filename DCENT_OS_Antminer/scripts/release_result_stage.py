#!/usr/bin/env python3
"""Manage an invocation-bound, capability-owned private result stage.

The stage is a host-side handoff boundary for Cargo or container outputs.  It
does not execute builds, create Docker resources, or publish releases.  A
sealed descriptor proves only the exact bytes and metadata observed in the
stage; it is not execution causality or reproducibility evidence.
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import unicodedata
from dataclasses import dataclass
from typing import Any, Callable, Dict, Iterable, List, NoReturn, Optional, Tuple

import release_invocation


DESCRIPTOR_SCHEMA = "org.dcentral.dcentos.release-result-stage.v1"
LEGACY_CAPABILITY_SCHEMA = "org.dcentral.dcentos.release-result-stage-capability.v1"
CAPABILITY_SCHEMA = "org.dcentral.dcentos.release-result-stage-capability.v2"
RESULT_SCHEMA = "org.dcentral.dcentos.release-result-stage-create-result.v1"
AUDIT_PROJECTION_SCHEMA = (
    "org.dcentral.dcentos.release-result-stage-audit-projection.v1"
)
WINDOWS_FLUSH_SCHEMA = (
    "org.dcentral.dcentos.release-result-stage-windows-flush-intent.v1"
)
DESCRIPTOR_NAME = "result-stage.json"
SEAL_PENDING_NAME = ".result-stage.json.seal.pending"
DESTROY_PENDING_NAME = ".result-stage.json.destroy.pending"
WINDOWS_FLUSH_PENDING_NAME = ".result-stage.windows-flush.pending"
RESULT_ROOT_NAME = "results"
STAGE_PREFIX = "dcentos-release-result-"
CAPABILITY_DIRECTORY = ".dcentos-release-result-capabilities"
CAPABILITY_SUFFIX = ".capability.json"
RETIRE_PREFIX = ".dcentos-release-result-retiring-"
MAX_CONTROL_BYTES = 64 * 1024 * 1024
MAX_ENTRIES = 1_000_000
HEX_64 = frozenset("0123456789abcdef")
OCTAL_MODE_RE = re.compile(r"[0-7]{4}\Z")
WINDOWS_RESERVED = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}
CLAIM = "invocation-bound-exact-private-result-stage-snapshot"
NON_CLAIMS = (
    "build-execution-or-artifact-causality",
    "toolchain-dependency-or-source-closure",
    "reproducibility-installed-payload-equivalence-or-runtime-correctness",
    "docker-resource-creation-liveness-or-deletion",
    "release-publication-signing-or-atomic-promotion",
    "protection-against-same-uid-concurrent-mutation",
)


class ResultStageError(ValueError):
    """A result stage failed a safety, ownership, or integrity check."""


class IncompletePendingError(ResultStageError):
    """A bounded private pending record ended before canonical JSON completed."""


@dataclass(frozen=True)
class VerifiedResultStage:
    stage: pathlib.Path
    descriptor: Dict[str, Any]
    invocation: release_invocation.VerifiedInvocation


@dataclass(frozen=True)
class CreatedResultStage:
    stage: pathlib.Path
    descriptor: pathlib.Path
    capability: pathlib.Path
    result_root: pathlib.Path
    invocation_stage: pathlib.Path
    invocation_id: str
    result_name: str
    stage_id: str

    def cli_result(self) -> Dict[str, Any]:
        return {
            "capability": str(self.capability),
            "descriptor": str(self.descriptor),
            "invocation_id": self.invocation_id,
            "invocation_stage": str(self.invocation_stage),
            "result_name": self.result_name,
            "result_root": str(self.result_root),
            "schema": RESULT_SCHEMA,
            "stage": str(self.stage),
            "stage_id": self.stage_id,
            "state": "building",
        }


def fail(message: str) -> NoReturn:
    raise ResultStageError(message)


def canonical_bytes(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("ascii")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def lexical_absolute(value: pathlib.Path) -> pathlib.Path:
    return pathlib.Path(os.path.abspath(os.fspath(value)))


def _is_digest(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and not (set(value) - HEX_64)


def _contains_control(value: str) -> bool:
    return any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)


def _exact_object(value: object, label: str, keys: Iterable[str]) -> Dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    expected = set(keys)
    actual = set(value)
    if actual != expected:
        fail(
            f"{label} has invalid keys "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )
    return value


def _is_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & flag)


def _check_owner(metadata: os.stat_result, label: str) -> None:
    if hasattr(os, "geteuid") and hasattr(metadata, "st_uid"):
        if metadata.st_uid != os.geteuid():
            fail(f"{label} is not owned by the current user")


def _check_directory(
    metadata: os.stat_result, label: str, *, private: bool = False
) -> None:
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a non-symlink, non-reparse directory")
    if private:
        _check_owner(metadata, label)
        if os.name == "posix" and stat.S_IMODE(metadata.st_mode) != 0o700:
            fail(f"{label} mode must be 0700")


def _require_private_windows_path(path: pathlib.Path, label: str) -> None:
    if os.name != "nt":
        return
    _atomic_publish_directory, release_set_publication = (
        _load_windows_security_helpers()
    )
    try:
        release_set_publication.require_private_windows_acl(path, label)
    except release_set_publication.ReleaseSetError as error:
        fail(str(error))


def _load_windows_security_helpers() -> Tuple[Any, Any]:
    import atomic_publish_directory
    import release_set_publication

    return atomic_publish_directory, release_set_publication


def _check_private_directory(path: pathlib.Path, label: str) -> os.stat_result:
    metadata = os.lstat(path)
    _check_directory(metadata, label, private=True)
    _require_private_windows_path(path, label)
    return metadata


def _check_control_file(metadata: os.stat_result, label: str) -> None:
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a non-reparse regular file")
    if metadata.st_nlink != 1:
        fail(f"{label} must have exactly one filesystem link")
    _check_owner(metadata, label)
    if os.name == "posix" and stat.S_IMODE(metadata.st_mode) != 0o400:
        fail(f"{label} mode must be 0400")


def assert_no_link_components(
    value: pathlib.Path, label: str, *, allow_missing_leaf: bool = False
) -> pathlib.Path:
    path = lexical_absolute(value)
    if _contains_control(str(path)):
        fail(f"{label} contains a control character")
    parts = path.parts
    if not parts:
        fail(f"{label} is empty")
    cursor = pathlib.Path(parts[0])
    for index, component in enumerate(parts[1:], 1):
        cursor = cursor / component
        try:
            metadata = os.lstat(cursor)
        except FileNotFoundError:
            if allow_missing_leaf and index == len(parts) - 1:
                return path
            raise
        if stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
            fail(f"{label} contains a symlink or reparse component: {cursor}")
    return path


def _set_private_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            os.fchmod(descriptor, 0o700)
        finally:
            os.close(descriptor)
    elif os.name == "nt":
        atomic_publish_directory, _release_set_publication = (
            _load_windows_security_helpers()
        )
        try:
            atomic_publish_directory.set_windows_directory_acl(
                path, atomic_publish_directory.WINDOWS_PRIVATE_DIRECTORY_SDDL
            )
        except atomic_publish_directory.DirectoryPublishError as error:
            fail(str(error))
    _check_private_directory(path, "private result-stage directory")


def _ensure_capability_directory(parent: pathlib.Path) -> pathlib.Path:
    directory = parent / CAPABILITY_DIRECTORY
    created = False
    try:
        os.mkdir(directory, 0o700)
        created = True
    except FileExistsError:
        pass
    directory = assert_no_link_components(directory, "result capability directory")
    metadata = os.lstat(directory)
    _check_directory(metadata, "result capability directory")
    _check_owner(metadata, "result capability directory")
    _set_private_directory(directory)
    if created:
        _fsync_directory(parent)
    _check_private_directory(directory, "result capability directory")
    return directory


def _write_exclusive(path: pathlib.Path, raw: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags, 0o400 if os.name == "posix" else 0o600)
    try:
        stream = os.fdopen(descriptor, "wb", closefd=True)
        descriptor = -1
        with stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    try:
        if os.name == "posix":
            os.chmod(path, 0o400, follow_symlinks=False)
        elif os.name == "nt":
            atomic_publish_directory, _release_set_publication = (
                _load_windows_security_helpers()
            )
            atomic_publish_directory.set_windows_file_acl(
                path, atomic_publish_directory.WINDOWS_PRIVATE_FILE_SDDL
            )
            _require_private_windows_path(path, "private result-stage control file")
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise


def _fsync_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    elif os.name == "nt":
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        metadata = os.lstat(path)
        _check_directory(metadata, f"directory durability boundary {path}")
        try:
            handle, close_handle = (
                release_set_publication.open_pinned_windows_directory(
                    path,
                    (metadata.st_dev, metadata.st_ino),
                    f"directory durability boundary {path}",
                )
            )
        except release_set_publication.ReleaseSetError as error:
            fail(str(error))
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        flush = kernel32.FlushFileBuffers
        flush.argtypes = (ctypes.c_void_p,)
        flush.restype = ctypes.c_int
        try:
            if not flush(handle):
                raise ctypes.WinError(ctypes.get_last_error())
        finally:
            close_handle()


def _stable_signature(metadata: os.stat_result) -> Tuple[int, ...]:
    values = [metadata.st_dev, metadata.st_ino, metadata.st_size]
    for field in ("st_mtime_ns", "st_ctime_ns"):
        if hasattr(metadata, field):
            values.append(getattr(metadata, field))
    return tuple(values)


def _read_control(
    path: pathlib.Path, label: str, *, require_flush: bool = False
) -> Tuple[bytes, os.stat_result]:
    path = assert_no_link_components(path, label)
    _require_private_windows_path(path, label)
    before = os.lstat(path)
    _check_control_file(before, label)
    flags = (
        (os.O_RDWR if os.name == "nt" and require_flush else os.O_RDONLY)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        _check_control_file(opened, label)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            fail(f"{label} changed while it was opened")
        chunks: List[bytes] = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(65536, MAX_CONTROL_BYTES + 1 - total))
            if not chunk:
                break
            total += len(chunk)
            if total > MAX_CONTROL_BYTES:
                fail(f"{label} exceeds {MAX_CONTROL_BYTES} bytes")
            chunks.append(chunk)
        if require_flush:
            os.fsync(descriptor)
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    current = os.lstat(path)
    _check_control_file(after, label)
    _check_control_file(current, label)
    if _stable_signature(opened) != _stable_signature(after):
        fail(f"{label} changed while it was read")
    if (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
        fail(f"{label} pathname was replaced while it was read")
    _require_private_windows_path(path, label)
    return b"".join(chunks), after


def _normalize_recoverable_pending(path: pathlib.Path, label: str) -> os.stat_result:
    path = assert_no_link_components(path, label)
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
        or metadata.st_nlink != 1
        or metadata.st_size > MAX_CONTROL_BYTES
    ):
        fail(f"{label} is not a bounded single-link recovery file")
    _check_owner(metadata, label)
    if os.name == "posix":
        os.chmod(path, 0o400, follow_symlinks=False)
    elif os.name == "nt":
        atomic_publish_directory, _release_set_publication = (
            _load_windows_security_helpers()
        )
        try:
            atomic_publish_directory.set_windows_file_acl(
                path, atomic_publish_directory.WINDOWS_PRIVATE_FILE_SDDL
            )
        except atomic_publish_directory.DirectoryPublishError as error:
            fail(str(error))
    normalized = os.lstat(path)
    _check_control_file(normalized, label)
    _require_private_windows_path(path, label)
    return normalized


def _discard_incomplete_pending(path: pathlib.Path, label: str) -> None:
    normalized = _normalize_recoverable_pending(path, label)
    _unlink_verified(path, normalized, label)
    _fsync_directory(path.parent)


def _parse_canonical(raw: bytes, label: str) -> Dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    if not isinstance(value, dict) or raw != canonical_bytes(value):
        fail(f"{label} is not canonical object JSON")
    return value


def _validate_component(component: str, label: str) -> None:
    if not component or component in (".", ".."):
        fail(f"{label} contains an empty or traversal component")
    if unicodedata.normalize("NFC", component) != component:
        fail(f"{label} is not NFC-normalized")
    if component[-1] in (" ", "."):
        fail(f"{label} has a Windows-ambiguous trailing character")
    if any(character in '<>:"\\|?*' for character in component):
        fail(f"{label} contains a non-portable path character")
    if component.split(".", 1)[0].casefold() in WINDOWS_RESERVED:
        fail(f"{label} contains a reserved Windows device name")
    for character in component:
        category = unicodedata.category(character)
        if (
            ord(character) < 32
            or ord(character) == 127
            or category in ("Zl", "Zp")
            or category.startswith("C")
        ):
            fail(f"{label} contains a control or non-portable Unicode character")


def _validate_relative(path: str, label: str) -> Tuple[str, ...]:
    if not isinstance(path, str) or not path or path.startswith("/") or "\\" in path:
        fail(f"{label} is not a canonical relative POSIX path")
    parts = tuple(path.split("/"))
    for component in parts:
        _validate_component(component, label)
    return parts


def _portable_key(path: str) -> Tuple[str, ...]:
    return tuple(component.casefold() for component in path.split("/"))


def _mode_string(metadata: os.stat_result) -> str:
    return f"{stat.S_IMODE(metadata.st_mode):04o}"


def _safe_payload_metadata(metadata: os.stat_result, label: str) -> str:
    if stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
        fail(f"{label} is a symlink or reparse point")
    if stat.S_ISREG(metadata.st_mode):
        if metadata.st_nlink != 1:
            fail(f"{label} has more than one filesystem link")
        return "file"
    if stat.S_ISDIR(metadata.st_mode):
        return "directory"
    fail(f"{label} is a special filesystem entry")


def _mount_identity(
    path: pathlib.Path, metadata: os.stat_result, label: str
) -> Tuple[str, int]:
    if not sys.platform.startswith("linux"):
        return ("device", metadata.st_dev)
    flags = (
        getattr(os, "O_PATH", os.O_RDONLY)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (metadata.st_dev, metadata.st_ino):
            fail(f"{label} changed while its mount identity was pinned")
        fdinfo = pathlib.Path(f"/proc/self/fdinfo/{descriptor}")
        try:
            raw = fdinfo.read_bytes()
        except OSError as error:
            fail(f"cannot read pinned mount identity for {label}: {error}")
        if len(raw) > 64 * 1024:
            fail(f"pinned mount identity for {label} is unexpectedly large")
        mount_id: Optional[int] = None
        for line in raw.splitlines():
            if line.startswith(b"mnt_id:\t"):
                try:
                    mount_id = int(line.removeprefix(b"mnt_id:\t"))
                except ValueError:
                    fail(f"pinned mount identity for {label} is malformed")
                break
        if mount_id is None or mount_id < 0:
            fail(f"pinned mount identity for {label} is unavailable")
        final = os.fstat(descriptor)
        current = os.lstat(path)
        if not _same_identity(opened, final) or not _same_identity(final, current):
            fail(f"{label} changed while its mount identity was read")
        return ("linux-mount", mount_id)
    finally:
        os.close(descriptor)


def _windows_file_attributes(metadata: os.stat_result) -> int:
    attributes = getattr(metadata, "st_file_attributes", 0)
    if (
        isinstance(attributes, bool)
        or not isinstance(attributes, int)
        or attributes < 0
    ):
        fail("Windows payload file attributes are invalid")
    return attributes


def _windows_flush_intent(
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    path: pathlib.Path,
    metadata: os.stat_result,
    digest: str,
    size: int,
) -> Dict[str, Any]:
    root = stage / RESULT_ROOT_NAME
    try:
        relative = path.relative_to(root).as_posix()
    except ValueError:
        fail("Windows payload flush target escapes the result root")
    _validate_relative(relative, "Windows payload flush path")
    return {
        "attributes": _windows_file_attributes(metadata),
        "descriptor_sha256": _authority_descriptor_sha256(descriptor),
        "dev": metadata.st_dev,
        "ino": metadata.st_ino,
        "path": relative,
        "schema": WINDOWS_FLUSH_SCHEMA,
        "sha256": digest,
        "size": size,
        "stage_id": descriptor["stage_id"],
    }


def _validate_windows_flush_intent(
    value: object, descriptor: Dict[str, Any]
) -> Dict[str, Any]:
    intent = _exact_object(
        value,
        "Windows payload flush intent",
        (
            "attributes",
            "descriptor_sha256",
            "dev",
            "ino",
            "path",
            "schema",
            "sha256",
            "size",
            "stage_id",
        ),
    )
    if intent["schema"] != WINDOWS_FLUSH_SCHEMA:
        fail("Windows payload flush intent schema is invalid")
    if (
        intent["descriptor_sha256"] != _authority_descriptor_sha256(descriptor)
        or intent["stage_id"] != descriptor["stage_id"]
    ):
        fail("Windows payload flush intent belongs to another result authority")
    if not _is_digest(intent["descriptor_sha256"]) or not _is_digest(intent["sha256"]):
        fail("Windows payload flush intent digest is invalid")
    _validate_relative(intent["path"], "Windows payload flush intent path")
    for field in ("attributes", "dev", "ino", "size"):
        observed = intent[field]
        if isinstance(observed, bool) or not isinstance(observed, int) or observed < 0:
            fail(f"Windows payload flush intent {field} is invalid")
    if intent["attributes"] > 0xFFFFFFFF:
        fail("Windows payload flush intent attributes exceed DWORD")
    if not intent["attributes"] & 0x1:
        fail("Windows payload flush intent does not preserve a read-only attribute")
    if intent["attributes"] & (0x10 | 0x400):
        fail("Windows payload flush intent names a directory or reparse point")
    return intent


def _read_windows_flush_pending(
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    *,
    require_flush: bool = False,
) -> Tuple[Dict[str, Any], os.stat_result]:
    pending = stage / WINDOWS_FLUSH_PENDING_NAME
    _normalize_recoverable_pending(pending, "Windows payload flush recovery record")
    raw, metadata = _read_control(
        pending,
        "Windows payload flush recovery record",
        require_flush=require_flush,
    )
    try:
        parsed = _parse_canonical(raw, "Windows payload flush recovery record")
    except ResultStageError as error:
        raise IncompletePendingError(str(error)) from error
    return _validate_windows_flush_intent(parsed, descriptor), metadata


def _remove_windows_flush_pending(
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    expected: Dict[str, Any],
) -> None:
    observed, metadata = _read_windows_flush_pending(stage, descriptor)
    if observed != expected:
        fail("Windows payload flush recovery record changed")
    _unlink_verified(
        stage / WINDOWS_FLUSH_PENDING_NAME,
        metadata,
        "Windows payload flush recovery record",
    )
    _fsync_directory(stage)


def _recover_windows_flush_pending(
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    *,
    restore: bool,
) -> None:
    try:
        intent, metadata = _read_windows_flush_pending(stage, descriptor)
    except FileNotFoundError:
        return
    except IncompletePendingError:
        _discard_incomplete_pending(
            stage / WINDOWS_FLUSH_PENDING_NAME,
            "incomplete Windows payload flush recovery record",
        )
        return
    if restore:
        if os.name != "nt":
            fail("Windows payload flush recovery requires native Windows")
        path = stage / RESULT_ROOT_NAME / pathlib.PurePosixPath(intent["path"])
        path = assert_no_link_components(path, "Windows payload flush recovery target")
        current = os.lstat(path)
        if (
            _safe_payload_metadata(current, "Windows payload flush recovery target")
            != "file"
        ):
            fail("Windows payload flush recovery target is not a regular file")
        if (current.st_dev, current.st_ino) != (intent["dev"], intent["ino"]):
            fail("Windows payload flush recovery target changed identity")
        original_attributes = intent["attributes"]
        cleared_attributes = original_attributes & ~0x1
        if cleared_attributes == 0:
            cleared_attributes = 0x80  # FILE_ATTRIBUTE_NORMAL
        current_attributes = _windows_file_attributes(current)
        if current_attributes == cleared_attributes:
            _atomic_publish_directory, release_set_publication = (
                _load_windows_security_helpers()
            )
            try:
                handle, close_handle = release_set_publication.open_pinned_windows_file(
                    path,
                    (intent["dev"], intent["ino"]),
                    "Windows payload flush recovery target",
                    write_attributes=True,
                )
                try:
                    release_set_publication.set_windows_handle_file_attributes(
                        handle, original_attributes
                    )
                finally:
                    close_handle()
            except release_set_publication.ReleaseSetError as error:
                fail(str(error))
            current = os.lstat(path)
            if (current.st_dev, current.st_ino) != (
                intent["dev"],
                intent["ino"],
            ) or _windows_file_attributes(current) != original_attributes:
                fail("Windows payload flush recovery did not restore the exact file")
        elif current_attributes != original_attributes:
            fail("Windows payload flush recovery target has unexpected attributes")
    _unlink_verified(
        stage / WINDOWS_FLUSH_PENDING_NAME,
        metadata,
        "Windows payload flush recovery record",
    )
    _fsync_directory(stage)


def _open_windows_writable_descriptor_while_pinned(
    path: pathlib.Path,
    expected_identity: Tuple[int, int],
    label: str,
) -> int:
    """Open writable data while a separate no-delete-share handle pins the file."""

    import msvcrt

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
    handle = create_file(
        str(path),
        0x80000000 | 0x40000000,  # GENERIC_READ | GENERIC_WRITE
        0x00000001 | 0x00000002 | 0x00000004,  # share read/write/delete
        None,
        3,  # OPEN_EXISTING
        0x00200000,  # FILE_FLAG_OPEN_REPARSE_POINT
        None,
    )
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        descriptor = msvcrt.open_osfhandle(
            handle, os.O_RDWR | getattr(os, "O_BINARY", 0)
        )
    except BaseException:
        close_handle(handle)
        raise
    try:
        opened = os.fstat(descriptor)
        if _safe_payload_metadata(opened, label) != "file":
            fail(f"{label} is not a regular file")
        if (opened.st_dev, opened.st_ino) != expected_identity:
            fail(f"{label} changed while its writable handle was opened")
    except BaseException:
        os.close(descriptor)
        raise
    return descriptor


def _hash_open_descriptor(
    descriptor: int,
    path: pathlib.Path,
    before: os.stat_result,
    label: str,
    *,
    require_flush: bool,
) -> Tuple[str, int]:
    opened = os.fstat(descriptor)
    if _safe_payload_metadata(opened, label) != "file":
        fail(f"{label} is not a regular file")
    if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
        fail(f"{label} changed while it was opened")
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = os.read(descriptor, 1024 * 1024)
        if not chunk:
            break
        digest.update(chunk)
        size += len(chunk)
    after_read = os.fstat(descriptor)
    if _stable_signature(before) != _stable_signature(after_read):
        fail(f"{label} changed while it was hashed")
    if size != before.st_size:
        fail(f"{label} size changed while it was hashed")
    if require_flush:
        os.fsync(descriptor)
    after_flush = os.fstat(descriptor)
    current = os.lstat(path)
    if _stable_signature(before) != _stable_signature(after_flush):
        fail(f"{label} changed while it was made durable")
    if (after_flush.st_dev, after_flush.st_ino) != (
        current.st_dev,
        current.st_ino,
    ):
        fail(f"{label} pathname was replaced while it was hashed")
    return digest.hexdigest(), size


def _flush_windows_readonly_file(
    path: pathlib.Path,
    before: os.stat_result,
    label: str,
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
) -> Tuple[str, int]:
    digest, size = _hash_file(path, before, label)
    intent = _windows_flush_intent(stage, descriptor, path, before, digest, size)
    _recover_windows_flush_pending(stage, descriptor, restore=True)
    _write_exclusive(
        stage / WINDOWS_FLUSH_PENDING_NAME,
        canonical_bytes(intent),
    )
    _fsync_directory(stage)
    durable_intent, _ = _read_windows_flush_pending(
        stage, descriptor, require_flush=True
    )
    if durable_intent != intent:
        fail("Windows payload flush recovery record differs from its file")

    _atomic_publish_directory, release_set_publication = (
        _load_windows_security_helpers()
    )
    try:
        handle, close_handle = release_set_publication.open_pinned_windows_file(
            path,
            (before.st_dev, before.st_ino),
            label,
            write_attributes=True,
        )
    except release_set_publication.ReleaseSetError as error:
        fail(str(error))
    original_attributes = intent["attributes"]
    cleared_attributes = original_attributes & ~0x1
    if cleared_attributes == 0:
        cleared_attributes = 0x80  # FILE_ATTRIBUTE_NORMAL
    operation_error: Optional[BaseException] = None
    attributes_changed = False
    try:
        release_set_publication.set_windows_handle_file_attributes(
            handle, cleared_attributes
        )
        attributes_changed = True
        writable = os.lstat(path)
        if (writable.st_dev, writable.st_ino) != (before.st_dev, before.st_ino):
            fail(f"{label} changed while its read-only attribute was cleared")
        if _windows_file_attributes(writable) != cleared_attributes:
            fail(f"{label} did not become writable for its durability flush")
        writable_descriptor = _open_windows_writable_descriptor_while_pinned(
            path, (before.st_dev, before.st_ino), label
        )
        try:
            flushed_digest, flushed_size = _hash_open_descriptor(
                writable_descriptor,
                path,
                writable,
                label,
                require_flush=True,
            )
        finally:
            os.close(writable_descriptor)
        if (flushed_digest, flushed_size) != (digest, size):
            fail(f"{label} changed while its read-only bytes were made durable")
    except BaseException as error:
        operation_error = error
    try:
        if attributes_changed:
            release_set_publication.set_windows_handle_file_attributes(
                handle, original_attributes
            )
    finally:
        close_handle()
    if not attributes_changed:
        if operation_error is not None:
            raise operation_error
        fail(f"{label} did not enter its Windows durability transition")
    restored = os.lstat(path)
    if (restored.st_dev, restored.st_ino) != (
        before.st_dev,
        before.st_ino,
    ) or _windows_file_attributes(restored) != original_attributes:
        fail(f"{label} was not restored after its Windows durability flush")
    restored_digest, restored_size = _hash_file(path, restored, label)
    if (restored_digest, restored_size) != (digest, size) and operation_error is None:
        operation_error = ResultStageError(
            f"{label} changed while its Windows attributes were restored"
        )
    _remove_windows_flush_pending(stage, descriptor, intent)
    if operation_error is not None:
        raise operation_error
    return digest, size


def _hash_file(
    path: pathlib.Path,
    before: os.stat_result,
    label: str,
    *,
    require_flush: bool = False,
    durability_stage: Optional[pathlib.Path] = None,
    durability_descriptor: Optional[Dict[str, Any]] = None,
) -> Tuple[str, int]:
    if os.name == "nt" and require_flush and _windows_file_attributes(before) & 0x1:
        if durability_stage is None or durability_descriptor is None:
            fail(f"{label} is read-only without a durable Windows flush authority")
        return _flush_windows_readonly_file(
            path,
            before,
            label,
            durability_stage,
            durability_descriptor,
        )
    flags = (
        (os.O_RDWR if os.name == "nt" and require_flush else os.O_RDONLY)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags)
    try:
        return _hash_open_descriptor(
            descriptor,
            path,
            before,
            label,
            require_flush=require_flush,
        )
    finally:
        os.close(descriptor)


def _walk_payload(
    root: pathlib.Path,
    *,
    hash_files: bool,
    require_flush: bool = False,
    durability_stage: Optional[pathlib.Path] = None,
    durability_descriptor: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    root_metadata = os.lstat(root)
    _check_directory(root_metadata, "result payload root")
    root_mount = _mount_identity(root, root_metadata, "result payload root")
    parent_metadata = os.lstat(root.parent)
    _check_directory(parent_metadata, "result stage containing payload root")
    if (
        _mount_identity(
            root.parent,
            parent_metadata,
            "result stage containing payload root",
        )
        != root_mount
    ):
        fail("result payload root crosses a mount boundary")
    directories: List[Dict[str, Any]] = []
    files: List[Dict[str, Any]] = []
    portable: set[Tuple[str, ...]] = set()
    stack: List[Tuple[pathlib.Path, str]] = [(root, "")]
    while stack:
        directory, prefix = stack.pop()
        directory_metadata = os.lstat(directory)
        _check_directory(directory_metadata, f"result directory {prefix or '.'}")
        if (
            _mount_identity(
                directory,
                directory_metadata,
                f"result directory {prefix or '.'}",
            )
            != root_mount
        ):
            fail(f"result directory {prefix or '.'} crosses a mount boundary")
        with os.scandir(directory) as entries:
            children = list(entries)
        # os.fsencode preserves POSIX surrogate escapes long enough for the
        # explicit portable-name validator below to reject them cleanly.
        children.sort(key=lambda entry: os.fsencode(entry.name))
        child_directories: List[Tuple[pathlib.Path, str]] = []
        for entry in children:
            relative = f"{prefix}/{entry.name}" if prefix else entry.name
            _validate_relative(relative, "result payload path")
            key = _portable_key(relative)
            if key in portable:
                fail(
                    f"result payload has a duplicate or portable path collision: {relative}"
                )
            portable.add(key)
            path = directory / entry.name
            metadata = os.lstat(path)
            kind = _safe_payload_metadata(metadata, f"result payload {relative}")
            if (
                _mount_identity(path, metadata, f"result payload {relative}")
                != root_mount
            ):
                fail(f"result payload {relative} crosses a mount boundary")
            if kind == "directory":
                directories.append({"mode": _mode_string(metadata), "path": relative})
                child_directories.append((path, relative))
            else:
                item: Dict[str, Any] = {
                    "mode": _mode_string(metadata),
                    "path": relative,
                    "size": metadata.st_size,
                }
                if hash_files:
                    item["sha256"], item["size"] = _hash_file(
                        path,
                        metadata,
                        f"result payload {relative}",
                        require_flush=require_flush,
                        durability_stage=durability_stage,
                        durability_descriptor=durability_descriptor,
                    )
                files.append(item)
            if len(directories) + len(files) > MAX_ENTRIES:
                fail(f"result payload exceeds {MAX_ENTRIES} entries")
        stack.extend(reversed(child_directories))
    directories.sort(key=lambda item: item["path"].encode("utf-8"))
    files.sort(key=lambda item: item["path"].encode("utf-8"))
    return {"directories": directories, "files": files}


def _sync_payload_directories(root: pathlib.Path, payload: Dict[str, Any]) -> None:
    paths = [
        root / pathlib.PurePosixPath(item["path"]) for item in payload["directories"]
    ]
    paths.sort(key=lambda path: (len(path.parts), os.fsencode(path)), reverse=True)
    for path in paths:
        _check_directory(os.lstat(path), f"payload durability boundary {path}")
        _fsync_directory(path)
    _check_directory(os.lstat(root), "result payload root durability boundary")
    _fsync_directory(root)


def _durable_payload_snapshot(
    root: pathlib.Path, descriptor: Dict[str, Any]
) -> Dict[str, Any]:
    stage = root.parent
    _recover_windows_flush_pending(stage, descriptor, restore=True)
    payload = _walk_payload(
        root,
        hash_files=True,
        require_flush=True,
        durability_stage=stage,
        durability_descriptor=descriptor,
    )
    _sync_payload_directories(root, payload)
    observed = _walk_payload(root, hash_files=True)
    if observed != payload:
        fail("result payload changed while its durable snapshot was prepared")
    return payload


def _manifest(payload: Dict[str, Any]) -> Dict[str, Any]:
    body = {
        "directories": payload["directories"],
        "files": payload["files"],
    }
    return {**body, "manifest_sha256": sha256_bytes(canonical_bytes(body))}


def _destruction_manifest(
    root: pathlib.Path, payload: Dict[str, Any]
) -> Dict[str, Any]:
    _sync_payload_directories(root, payload)
    observed = _walk_payload(root, hash_files=True)
    if observed != payload:
        fail("result payload changed while its destruction namespace was synchronized")
    root_metadata = os.lstat(root)
    _check_directory(root_metadata, "result payload root destruction identity")
    directories: List[Dict[str, Any]] = []
    files: List[Dict[str, Any]] = []
    for item in observed["directories"]:
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        _check_directory(metadata, f"destruction directory {item['path']}")
        if _mode_string(metadata) != item["mode"]:
            fail(f"destruction directory {item['path']} changed mode")
        directories.append({**item, "dev": metadata.st_dev, "ino": metadata.st_ino})
    for item in observed["files"]:
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if (
            _safe_payload_metadata(metadata, f"destruction file {item['path']}")
            != "file"
        ):
            fail(f"destruction file {item['path']} changed type")
        if _mode_string(metadata) != item["mode"] or metadata.st_size != item["size"]:
            fail(f"destruction file {item['path']} changed metadata")
        files.append({**item, "dev": metadata.st_dev, "ino": metadata.st_ino})
    body = {
        "directories": directories,
        "files": files,
        "root_dev": root_metadata.st_dev,
        "root_ino": root_metadata.st_ino,
    }
    return {**body, "manifest_sha256": sha256_bytes(canonical_bytes(body))}


def _invocation_binding(
    verified: release_invocation.VerifiedInvocation,
) -> Dict[str, str]:
    descriptor = verified.descriptor
    return {
        "descriptor_sha256": sha256_bytes(canonical_bytes(descriptor)),
        "invocation_id": descriptor["invocation_id"],
        "result_name": descriptor["resources"]["result_name"],
        "stage": str(verified.stage),
    }


def _stage_id(binding: Dict[str, str], allocation_nonce: str) -> str:
    return sha256_bytes(
        canonical_bytes({"allocation_nonce": allocation_nonce, "invocation": binding})
    )


def _descriptor(
    binding: Dict[str, str], allocation_nonce: str, manifest: Optional[Dict[str, Any]]
) -> Dict[str, Any]:
    return {
        "allocation_nonce": allocation_nonce,
        "invocation": binding,
        "manifest": manifest,
        "result_root": RESULT_ROOT_NAME,
        "schema": DESCRIPTOR_SCHEMA,
        "scope": {"claim": CLAIM, "does_not_claim": list(NON_CLAIMS)},
        "stage_id": _stage_id(binding, allocation_nonce),
        "state": "sealed" if manifest is not None else "building",
    }


def _authority_descriptor_sha256(descriptor: Dict[str, Any]) -> str:
    building = dict(descriptor)
    building["manifest"] = None
    building["state"] = "building"
    return sha256_bytes(canonical_bytes(building))


def _expected_stage_name(descriptor: Dict[str, Any]) -> str:
    return (
        f"{STAGE_PREFIX}{descriptor['invocation']['result_name']}-"
        f"{descriptor['stage_id']}"
    )


def capability_path(stage: pathlib.Path) -> pathlib.Path:
    stage = lexical_absolute(stage)
    return stage.parent / CAPABILITY_DIRECTORY / f"{stage.name}{CAPABILITY_SUFFIX}"


def _capability(
    descriptor: Dict[str, Any],
    stage: pathlib.Path,
    stage_metadata: os.stat_result,
) -> Dict[str, Any]:
    return {
        "descriptor_sha256": _authority_descriptor_sha256(descriptor),
        "invocation_id": descriptor["invocation"]["invocation_id"],
        "invocation_stage": descriptor["invocation"]["stage"],
        "schema": CAPABILITY_SCHEMA,
        "stage_dev": stage_metadata.st_dev,
        "stage_id": descriptor["stage_id"],
        "stage_ino": stage_metadata.st_ino,
        "stage_path": str(stage),
        "token": sha256_bytes(
            canonical_bytes(
                {
                    "allocation_nonce": descriptor["allocation_nonce"],
                    "domain": CAPABILITY_SCHEMA,
                }
            )
        ),
    }


def _validate_binding(
    value: object, invocation: release_invocation.VerifiedInvocation
) -> Dict[str, str]:
    binding = _exact_object(
        value,
        "result-stage invocation binding",
        ("descriptor_sha256", "invocation_id", "result_name", "stage"),
    )
    expected = _invocation_binding(invocation)
    if binding != expected:
        fail("result stage is bound to a different or changed release invocation")
    return binding  # type: ignore[return-value]


def _validate_manifest(value: object) -> Dict[str, Any]:
    manifest = _exact_object(
        value,
        "result-stage manifest",
        ("directories", "files", "manifest_sha256"),
    )
    if not isinstance(manifest["directories"], list) or not isinstance(
        manifest["files"], list
    ):
        fail("result-stage manifest entry collections must be arrays")
    if len(manifest["directories"]) + len(manifest["files"]) > MAX_ENTRIES:
        fail("result-stage manifest contains too many entries")
    portable: set[Tuple[str, ...]] = set()
    for label, entries, keys in (
        ("directory", manifest["directories"], ("mode", "path")),
        ("file", manifest["files"], ("mode", "path", "sha256", "size")),
    ):
        paths: List[str] = []
        for index, item_value in enumerate(entries):
            item = _exact_object(item_value, f"result-stage {label} {index}", keys)
            _validate_relative(item["path"], f"result-stage {label} {index} path")
            if not isinstance(item["mode"], str) or not OCTAL_MODE_RE.fullmatch(
                item["mode"]
            ):
                fail(f"result-stage {label} {index} mode is invalid")
            if label == "file":
                if not _is_digest(item["sha256"]):
                    fail(f"result-stage file {index} digest is invalid")
                if (
                    not isinstance(item["size"], int)
                    or isinstance(item["size"], bool)
                    or item["size"] < 0
                ):
                    fail(f"result-stage file {index} size is invalid")
            key = _portable_key(item["path"])
            if key in portable:
                fail("result-stage manifest has duplicate or portable-colliding paths")
            portable.add(key)
            paths.append(item["path"])
        if paths != sorted(paths, key=lambda path: path.encode("utf-8")):
            fail(f"result-stage {label} paths are not in canonical byte order")
    body = {
        "directories": manifest["directories"],
        "files": manifest["files"],
    }
    if (
        not _is_digest(manifest["manifest_sha256"])
        or sha256_bytes(canonical_bytes(body)) != manifest["manifest_sha256"]
    ):
        fail("result-stage manifest digest does not bind its canonical entries")
    return manifest


def _validate_destruction_manifest(value: object) -> Dict[str, Any]:
    manifest = _exact_object(
        value,
        "result-stage destruction manifest",
        ("directories", "files", "manifest_sha256", "root_dev", "root_ino"),
    )
    for field in ("root_dev", "root_ino"):
        if (
            not isinstance(manifest[field], int)
            or isinstance(manifest[field], bool)
            or manifest[field] < 0
        ):
            fail(f"result-stage destruction manifest {field} is invalid")
    if not isinstance(manifest["directories"], list) or not isinstance(
        manifest["files"], list
    ):
        fail("result-stage destruction manifest entry collections must be arrays")
    if len(manifest["directories"]) + len(manifest["files"]) > MAX_ENTRIES:
        fail("result-stage destruction manifest contains too many entries")
    portable: set[Tuple[str, ...]] = set()
    for label, entries, keys in (
        ("directory", manifest["directories"], ("dev", "ino", "mode", "path")),
        (
            "file",
            manifest["files"],
            ("dev", "ino", "mode", "path", "sha256", "size"),
        ),
    ):
        paths: List[str] = []
        for index, item_value in enumerate(entries):
            item = _exact_object(
                item_value, f"result-stage destruction {label} {index}", keys
            )
            _validate_relative(
                item["path"], f"result-stage destruction {label} {index} path"
            )
            if not isinstance(item["mode"], str) or not OCTAL_MODE_RE.fullmatch(
                item["mode"]
            ):
                fail(f"result-stage destruction {label} {index} mode is invalid")
            for field in ("dev", "ino"):
                if (
                    not isinstance(item[field], int)
                    or isinstance(item[field], bool)
                    or item[field] < 0
                ):
                    fail(f"result-stage destruction {label} {index} {field} is invalid")
            if label == "file":
                if not _is_digest(item["sha256"]):
                    fail(f"result-stage destruction file {index} digest is invalid")
                if (
                    not isinstance(item["size"], int)
                    or isinstance(item["size"], bool)
                    or item["size"] < 0
                ):
                    fail(f"result-stage destruction file {index} size is invalid")
            key = _portable_key(item["path"])
            if key in portable:
                fail(
                    "result-stage destruction manifest has duplicate or "
                    "portable-colliding paths"
                )
            portable.add(key)
            paths.append(item["path"])
        if paths != sorted(paths, key=lambda path: path.encode("utf-8")):
            fail(
                f"result-stage destruction {label} paths are not in canonical byte order"
            )
    body = {
        "directories": manifest["directories"],
        "files": manifest["files"],
        "root_dev": manifest["root_dev"],
        "root_ino": manifest["root_ino"],
    }
    if (
        not _is_digest(manifest["manifest_sha256"])
        or sha256_bytes(canonical_bytes(body)) != manifest["manifest_sha256"]
    ):
        fail("result-stage destruction manifest digest is invalid")
    return manifest


def _validate_descriptor(
    value: object, invocation: release_invocation.VerifiedInvocation
) -> Dict[str, Any]:
    descriptor = _exact_object(
        value,
        "result-stage descriptor",
        (
            "allocation_nonce",
            "invocation",
            "manifest",
            "result_root",
            "schema",
            "scope",
            "stage_id",
            "state",
        ),
    )
    if descriptor["schema"] != DESCRIPTOR_SCHEMA:
        fail("unsupported result-stage descriptor schema")
    if not _is_digest(descriptor["allocation_nonce"]):
        fail("result-stage allocation nonce is invalid")
    binding = _validate_binding(descriptor["invocation"], invocation)
    if descriptor["result_root"] != RESULT_ROOT_NAME:
        fail("result-stage result root is not canonical")
    if descriptor["scope"] != {
        "claim": CLAIM,
        "does_not_claim": list(NON_CLAIMS),
    }:
        fail("result-stage scope is invalid or overstated")
    if descriptor["stage_id"] != _stage_id(binding, descriptor["allocation_nonce"]):
        fail("result-stage id does not bind its invocation and allocation nonce")
    if descriptor["state"] == "building":
        if descriptor["manifest"] is not None:
            fail("building result stage must not contain a sealed manifest")
    elif descriptor["state"] == "sealed":
        _validate_manifest(descriptor["manifest"])
    elif descriptor["state"] == "destroying":
        _validate_destruction_manifest(descriptor["manifest"])
    else:
        fail("result-stage state must be building, sealed, or destroying")
    return descriptor


def _validate_capability_record(value: object) -> Dict[str, Any]:
    if not isinstance(value, dict):
        fail("result-stage capability must be an object")
    schema = value.get("schema")
    common = (
        "invocation_id",
        "invocation_stage",
        "schema",
        "stage_id",
        "stage_path",
        "token",
    )
    if schema == LEGACY_CAPABILITY_SCHEMA:
        capability = _exact_object(value, "result-stage capability", common)
    elif schema == CAPABILITY_SCHEMA:
        capability = _exact_object(
            value,
            "result-stage capability",
            (
                "descriptor_sha256",
                *common,
                "stage_dev",
                "stage_ino",
            ),
        )
        if not _is_digest(capability["descriptor_sha256"]):
            fail("result-stage capability descriptor digest is invalid")
        for field in ("stage_dev", "stage_ino"):
            observed = capability[field]
            if (
                isinstance(observed, bool)
                or not isinstance(observed, int)
                or observed < 0
            ):
                fail(f"result-stage capability {field} is invalid")
    else:
        fail("result-stage capability schema is invalid")
    for field in ("invocation_id", "stage_id", "token"):
        if not _is_digest(capability[field]):
            fail(f"result-stage capability {field} is invalid")
    for field in ("invocation_stage", "stage_path"):
        if (
            not isinstance(capability[field], str)
            or not pathlib.Path(capability[field]).is_absolute()
            or lexical_absolute(pathlib.Path(capability[field]))
            != pathlib.Path(capability[field])
        ):
            fail(f"result-stage capability {field} is not a lexical absolute path")
    return capability


def _validate_capability_binding(
    value: object,
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
) -> Dict[str, Any]:
    capability = _validate_capability_record(value)
    binding = _invocation_binding(invocation)
    expected = {
        "invocation_id": binding["invocation_id"],
        "invocation_stage": str(invocation.stage),
        "stage_path": str(stage),
    }
    for key, expected_value in expected.items():
        if capability[key] != expected_value:
            fail(f"result-stage capability {key} binding does not match")
    expected_name = f"{STAGE_PREFIX}{binding['result_name']}-{capability['stage_id']}"
    if stage.name != expected_name:
        fail("result-stage capability stage name is not invocation-derived")
    return capability


def _validate_capability_for_invocation(
    value: object,
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
) -> Dict[str, Any]:
    capability = _validate_capability_binding(value, stage, invocation)
    if capability["schema"] == CAPABILITY_SCHEMA:
        try:
            stage_metadata = os.lstat(stage)
        except FileNotFoundError:
            pass
        else:
            _check_directory(
                stage_metadata, "capability-bound result stage", private=True
            )
            if (capability["stage_dev"], capability["stage_ino"]) != (
                stage_metadata.st_dev,
                stage_metadata.st_ino,
            ):
                fail("result-stage capability is bound to a replaced stage")
    return capability


def _validate_capability(
    value: object,
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    *,
    stage_metadata: Optional[os.stat_result] = None,
) -> Dict[str, Any]:
    capability = _validate_capability_record(value)
    expected = {
        "invocation_id": descriptor["invocation"]["invocation_id"],
        "invocation_stage": descriptor["invocation"]["stage"],
        "stage_id": descriptor["stage_id"],
        "stage_path": str(stage),
    }
    for key, expected_value in expected.items():
        if capability[key] != expected_value:
            fail(f"result-stage capability {key} binding does not match")
    if capability["schema"] == CAPABILITY_SCHEMA:
        stage_metadata = stage_metadata or os.lstat(stage)
        if (capability["stage_dev"], capability["stage_ino"]) != (
            stage_metadata.st_dev,
            stage_metadata.st_ino,
        ):
            fail("result-stage capability is bound to a replaced stage")
        if capability["descriptor_sha256"] != _authority_descriptor_sha256(descriptor):
            fail("result-stage capability descriptor digest does not match")
    return capability


def _inspect_stage_top(
    stage: pathlib.Path,
    *,
    allow_seal_pending: bool = False,
    allow_windows_flush_pending: bool = False,
) -> None:
    expected = {DESCRIPTOR_NAME, RESULT_ROOT_NAME}
    allowed = set(expected)
    if allow_seal_pending:
        allowed.add(SEAL_PENDING_NAME)
    if allow_windows_flush_pending:
        allowed.add(WINDOWS_FLUSH_PENDING_NAME)
    with os.scandir(stage) as entries:
        actual = {entry.name for entry in entries}
    if not expected.issubset(actual) or not actual.issubset(allowed):
        fail(
            "result-stage control tree is not exact "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )
    _check_control_file(os.lstat(stage / DESCRIPTOR_NAME), "result-stage descriptor")
    _check_private_directory(stage / RESULT_ROOT_NAME, "result payload root")
    if SEAL_PENDING_NAME in actual:
        _normalize_recoverable_pending(
            stage / SEAL_PENDING_NAME, "result-stage seal recovery record"
        )
    if WINDOWS_FLUSH_PENDING_NAME in actual:
        _normalize_recoverable_pending(
            stage / WINDOWS_FLUSH_PENDING_NAME,
            "Windows payload flush recovery record",
        )


def _result_output_path(path_value: pathlib.Path) -> pathlib.Path:
    path = assert_no_link_components(
        path_value, "result-stage creation result output", allow_missing_leaf=True
    )
    parent = assert_no_link_components(
        path.parent, "result-stage creation result parent"
    )
    _check_private_directory(parent, "result-stage creation result parent")
    if path.name in {"", ".", ".."} or path.parent / path.name != path:
        fail("result-stage creation result output name is invalid")
    return path


def _read_private_result_output(path: pathlib.Path) -> bytes:
    path = assert_no_link_components(path, "result-stage creation result output")
    _require_private_windows_path(path, "result-stage creation result output")
    before = os.lstat(path)
    if (
        not stat.S_ISREG(before.st_mode)
        or stat.S_ISLNK(before.st_mode)
        or _is_reparse(before)
        or before.st_nlink != 1
        or before.st_size > MAX_CONTROL_BYTES
    ):
        fail("result-stage creation result output is not a bounded single-link file")
    _check_owner(before, "result-stage creation result output")
    if os.name == "posix" and stat.S_IMODE(before.st_mode) != 0o600:
        fail("result-stage creation result output mode must be 0600")
    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            fail("result-stage creation result output changed while being opened")
        chunks: List[bytes] = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(65536, MAX_CONTROL_BYTES + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_CONTROL_BYTES:
                fail("result-stage creation result output is too large")
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    current = os.lstat(path)
    if _stable_signature(opened) != _stable_signature(after) or (
        after.st_dev,
        after.st_ino,
    ) != (current.st_dev, current.st_ino):
        fail("result-stage creation result output changed while being read")
    _require_private_windows_path(path, "result-stage creation result output")
    return b"".join(chunks)


def _recover_regular_publication(
    path: pathlib.Path, raw: Optional[bytes], label: str
) -> None:
    prefix = f".{path.name}.publication-pending."
    with os.scandir(path.parent) as entries:
        pending_names = sorted(
            entry.name for entry in entries if entry.name.startswith(prefix)
        )
    changed = False
    for name in pending_names:
        pending = path.parent / name
        metadata = os.lstat(pending)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or _is_reparse(metadata)
            or metadata.st_size > MAX_CONTROL_BYTES
        ):
            fail(f"{label} has an unsafe pending publication")
        _check_owner(metadata, f"{label} pending publication")
        try:
            destination_metadata = os.lstat(path)
        except FileNotFoundError:
            destination_metadata = None
        if destination_metadata is not None and _same_identity(
            metadata, destination_metadata
        ):
            if metadata.st_nlink != 2:
                fail(f"linked {label} publication has unexpected aliases")
            flags = (
                os.O_RDONLY
                | getattr(os, "O_BINARY", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_NONBLOCK", 0)
            )
            descriptor = os.open(pending, flags)
            try:
                opened = os.fstat(descriptor)
                if not _same_identity(opened, metadata):
                    fail(f"pending {label} publication changed")
                observed = b""
                while len(observed) <= MAX_CONTROL_BYTES:
                    chunk = os.read(
                        descriptor,
                        min(65536, MAX_CONTROL_BYTES + 1 - len(observed)),
                    )
                    if not chunk:
                        break
                    observed += chunk
            finally:
                os.close(descriptor)
            if raw is not None and observed != raw:
                fail(f"linked {label} publication has wrong bytes")
            os.unlink(pending)
            changed = True
            continue
        if metadata.st_nlink != 1:
            fail(f"{label} pending publication is multiply linked")
        _unlink_verified(
            pending,
            metadata,
            f"{label} pending publication",
        )
        changed = True
    if changed:
        _fsync_directory(path.parent)


def _publish_creation_result(path: pathlib.Path, raw: bytes) -> None:
    path = _result_output_path(path)
    _recover_regular_publication(path, raw, "result-stage creation result")

    def existing_matches() -> bool:
        try:
            observed = _read_private_result_output(path)
        except FileNotFoundError:
            return False
        return observed == raw

    if existing_matches():
        _fsync_directory(path.parent)
        return
    try:
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        release_set_publication.publish_regular_file_noreplace(path, raw, mode=0o600)
    except release_set_publication.ReleaseSetError as error:
        _recover_regular_publication(path, raw, "result-stage creation result")
        if existing_matches():
            _fsync_directory(path.parent)
            return
        fail(f"cannot publish result-stage creation result: {error}")
    if not existing_matches():
        fail("published result-stage creation result does not match")
    _fsync_directory(path.parent)


def _publish_expected_control(path: pathlib.Path, raw: bytes, label: str) -> None:
    _recover_regular_publication(path, raw, label)

    def existing_matches() -> bool:
        try:
            _normalize_recoverable_pending(path, label)
            observed, _ = _read_control(path, label, require_flush=True)
        except FileNotFoundError:
            return False
        return observed == raw

    if existing_matches():
        _fsync_directory(path.parent)
        return
    try:
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        release_set_publication.publish_regular_file_noreplace(path, raw, mode=0o600)
    except release_set_publication.ReleaseSetError as error:
        _recover_regular_publication(path, raw, label)
        if existing_matches():
            _fsync_directory(path.parent)
            return
        fail(f"cannot publish {label}: {error}")
    if not existing_matches():
        fail(f"published {label} does not match")
    _fsync_directory(path.parent)


def _creation_allocation_nonce(
    binding: Dict[str, str],
    stage_parent: pathlib.Path,
    result_output: pathlib.Path,
) -> str:
    return sha256_bytes(
        canonical_bytes(
            {
                "domain": "org.dcentral.dcentos.release-result-stage-create-intent.v1",
                "invocation": binding,
                "result_output": str(result_output),
                "stage_parent": str(stage_parent),
            }
        )
    )


def _recover_interrupted_destruction_for_create(
    stage: pathlib.Path,
    capability: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    descriptor: Dict[str, Any],
) -> None:
    """Finish an older destruction before recreating the same authority path."""

    retirement = _retirement_path(stage, {"stage_id": descriptor["stage_id"]})
    _recover_regular_publication(
        capability,
        None,
        "result-stage capability before creation recovery",
    )
    try:
        capability_metadata = os.lstat(capability)
    except FileNotFoundError:
        capability_metadata = None
    try:
        retirement_metadata = os.lstat(retirement)
    except FileNotFoundError:
        retirement_metadata = None
    if capability_metadata is None:
        if retirement_metadata is not None:
            fail("result-stage retirement exists without its destruction capability")
        return

    capability_raw, capability_metadata = _read_control(
        capability,
        "result-stage capability before creation recovery",
        require_flush=True,
    )
    _fsync_directory(capability.parent)
    capability_record = _validate_capability_binding(
        _parse_canonical(
            capability_raw, "result-stage capability before creation recovery"
        ),
        stage,
        invocation,
    )
    if capability_record["stage_id"] != descriptor["stage_id"]:
        fail("existing result-stage capability belongs to another creation intent")
    if capability_record["schema"] == CAPABILITY_SCHEMA and capability_record[
        "descriptor_sha256"
    ] != _authority_descriptor_sha256(descriptor):
        fail("existing result-stage capability belongs to another authority")

    try:
        stage_metadata = os.lstat(stage)
    except FileNotFoundError:
        stage_metadata = None
    if retirement_metadata is not None:
        retirement_metadata = _check_private_directory(
            retirement, "retired result stage before creation recovery"
        )
        if capability_record["schema"] != CAPABILITY_SCHEMA:
            fail("legacy result-stage capability cannot authorize a retired stage")
        if (capability_record["stage_dev"], capability_record["stage_ino"]) != (
            retirement_metadata.st_dev,
            retirement_metadata.st_ino,
        ):
            fail("retired result stage does not match its destruction capability")

    if stage_metadata is not None:
        stage_metadata = _check_private_directory(
            stage, "canonical result stage before creation recovery"
        )
        stage_matches = capability_record["schema"] != CAPABILITY_SCHEMA or (
            capability_record["stage_dev"],
            capability_record["stage_ino"],
        ) == (stage_metadata.st_dev, stage_metadata.st_ino)
        if not stage_matches:
            # Older create retry logic could leave exactly this empty, unbound
            # canonical directory after a destruction crash.  Remove only that
            # verified empty inode; any content remains a fail-closed conflict.
            with os.scandir(stage) as entries:
                if next(entries, None) is not None:
                    fail(
                        "unbound canonical result stage is not empty during "
                        "creation recovery"
                    )
            _rmdir_verified(
                stage,
                stage_metadata,
                "unbound canonical result stage during creation recovery",
            )
            _fsync_directory(stage.parent)
            stage_metadata = None

    if stage_metadata is not None and retirement_metadata is not None:
        fail("canonical and retired result stages both match one capability")

    must_finish_destruction = stage_metadata is None or retirement_metadata is not None
    if stage_metadata is not None and not must_finish_destruction:
        with os.scandir(stage) as entries:
            names = {entry.name for entry in entries}
        if DESTROY_PENDING_NAME in names:
            must_finish_destruction = True
        elif DESCRIPTOR_NAME in names:
            descriptor_raw, _ = _read_control(
                stage / DESCRIPTOR_NAME,
                "result-stage descriptor before creation recovery",
                require_flush=True,
            )
            existing_descriptor = _validate_descriptor(
                _parse_canonical(
                    descriptor_raw,
                    "result-stage descriptor before creation recovery",
                ),
                invocation,
            )
            _validate_capability(
                capability_record,
                stage,
                existing_descriptor,
                stage_metadata=stage_metadata,
            )
            must_finish_destruction = existing_descriptor["state"] == "destroying"

    if not must_finish_destruction:
        return
    destroy_result_stage(stage, capability, invocation.stage)
    for path, label in (
        (stage, "canonical result stage"),
        (retirement, "retired result stage"),
        (capability, "result-stage capability"),
    ):
        try:
            os.lstat(path)
        except FileNotFoundError:
            continue
        fail(f"{label} survived creation-time destruction recovery")


def create_result_stage(
    stage_parent: pathlib.Path,
    invocation_stage: pathlib.Path,
    *,
    result_output: pathlib.Path,
) -> CreatedResultStage:
    parent = assert_no_link_components(stage_parent, "result-stage parent")
    _check_directory(os.lstat(parent), "result-stage parent")
    verified_invocation = release_invocation.verify_invocation(invocation_stage)
    binding = _invocation_binding(verified_invocation)
    output = _result_output_path(result_output)
    allocation_nonce = _creation_allocation_nonce(binding, parent, output)
    descriptor_value = _descriptor(binding, allocation_nonce, None)
    stage_id = descriptor_value["stage_id"]
    stage = parent / _expected_stage_name(descriptor_value)
    capability = capability_path(stage)
    result_root = stage / RESULT_ROOT_NAME
    descriptor_path = stage / DESCRIPTOR_NAME
    created = CreatedResultStage(
        stage=stage,
        descriptor=descriptor_path,
        capability=capability,
        result_root=result_root,
        invocation_stage=verified_invocation.stage,
        invocation_id=binding["invocation_id"],
        result_name=binding["result_name"],
        stage_id=stage_id,
    )
    # The durable caller-owned locator precedes every stage-specific object.
    # Every later crash prefix is therefore either empty or discoverable.
    _publish_creation_result(output, canonical_bytes(created.cli_result()))
    capability_directory = _ensure_capability_directory(parent)
    _fsync_directory(capability_directory)
    _recover_interrupted_destruction_for_create(
        stage,
        capability,
        verified_invocation,
        descriptor_value,
    )
    assert_no_link_components(stage, "result stage", allow_missing_leaf=True)
    assert_no_link_components(
        capability, "result-stage capability", allow_missing_leaf=True
    )
    stage_created = False
    capability_ready = False
    stage_metadata: Optional[os.stat_result] = None
    try:
        try:
            os.mkdir(stage, 0o700)
            stage_created = True
        except FileExistsError:
            pass
        stage_metadata = os.lstat(stage)
        _check_directory(stage_metadata, "recoverable result stage")
        _check_owner(stage_metadata, "recoverable result stage")
        _set_private_directory(stage)
        stage_metadata = _check_private_directory(stage, "recoverable result stage")
        _fsync_directory(stage)
        _fsync_directory(parent)
        try:
            os.lstat(capability)
        except FileNotFoundError:
            with os.scandir(stage) as entries:
                if next(entries, None) is not None:
                    fail("pre-capability result stage is not empty")
        capability_raw = canonical_bytes(
            _capability(descriptor_value, stage, stage_metadata)
        )
        _publish_expected_control(capability, capability_raw, "result-stage capability")
        capability_value = _validate_capability(
            _parse_canonical(capability_raw, "result-stage capability"),
            stage,
            descriptor_value,
        )
        if capability_value["schema"] != CAPABILITY_SCHEMA:
            fail("new result-stage capability did not use the current schema")
        capability_ready = True
        _fsync_directory(capability_directory)
        _fsync_directory(parent)

        descriptor_value_raw = canonical_bytes(descriptor_value)
        _recover_regular_publication(
            descriptor_path,
            descriptor_value_raw,
            "result-stage creation descriptor",
        )
        with os.scandir(stage) as entries:
            names = {entry.name for entry in entries}
        allowed = {DESCRIPTOR_NAME, RESULT_ROOT_NAME}
        if not names.issubset(allowed):
            fail(
                "recoverable result stage contains unexpected entries: "
                f"{sorted(names - allowed)}"
            )
        _publish_expected_control(
            descriptor_path,
            descriptor_value_raw,
            "result-stage creation descriptor",
        )
        _fsync_directory(stage)
        try:
            os.mkdir(result_root, 0o700)
        except FileExistsError:
            pass
        root_metadata = os.lstat(result_root)
        _check_directory(root_metadata, "recoverable result payload root")
        _check_owner(root_metadata, "recoverable result payload root")
        _set_private_directory(result_root)
        _check_private_directory(result_root, "recoverable result payload root")
        _fsync_directory(result_root)
        _fsync_directory(stage)
        _fsync_directory(parent)
        verified = verify_result_stage(stage, verified_invocation.stage)
        verify_capability(
            verified.stage,
            capability,
            verified.invocation.stage,
            verified=verified,
        )
        return created
    except BaseException:
        # Once the exact capability is durable, retain all authority-bound
        # partial work for idempotent retry or authenticated destruction.
        if stage_created and not capability_ready and stage_metadata is not None:
            try:
                try:
                    os.lstat(capability)
                except FileNotFoundError:
                    capability_exists = False
                else:
                    capability_exists = True
                with os.scandir(stage) as entries:
                    empty = next(entries, None) is None
                if empty and not capability_exists:
                    _rmdir_verified(stage, stage_metadata, "uncommitted result stage")
                    _fsync_directory(parent)
            except (FileNotFoundError, OSError, ResultStageError):
                pass
        raise


def verify_result_stage(
    stage_value: pathlib.Path,
    invocation_stage: pathlib.Path,
    *,
    _allow_seal_pending: bool = False,
    _allow_windows_flush_pending: bool = False,
) -> VerifiedResultStage:
    invocation = release_invocation.verify_invocation(invocation_stage)
    stage = assert_no_link_components(stage_value, "result stage")
    _check_private_directory(stage, "result stage")
    _inspect_stage_top(
        stage,
        allow_seal_pending=_allow_seal_pending,
        allow_windows_flush_pending=_allow_windows_flush_pending,
    )
    raw, _ = _read_control(stage / DESCRIPTOR_NAME, "result-stage descriptor")
    descriptor = _validate_descriptor(
        _parse_canonical(raw, "result-stage descriptor"), invocation
    )
    if descriptor["state"] == "destroying":
        fail("result stage is already being destroyed")
    if stage.name != _expected_stage_name(descriptor):
        fail("result-stage name is not canonically descriptor-derived")
    if _allow_windows_flush_pending:
        _recover_windows_flush_pending(stage, descriptor, restore=True)
    payload = _walk_payload(
        stage / RESULT_ROOT_NAME, hash_files=descriptor["state"] == "sealed"
    )
    if descriptor["state"] == "sealed":
        observed = _manifest(payload)
        if observed != descriptor["manifest"]:
            fail("sealed result-stage payload differs from its exact manifest")
    _inspect_stage_top(
        stage,
        allow_seal_pending=_allow_seal_pending,
        allow_windows_flush_pending=_allow_windows_flush_pending,
    )
    return VerifiedResultStage(stage, descriptor, invocation)


def verify_capability(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    invocation_stage: pathlib.Path,
    *,
    verified: Optional[VerifiedResultStage] = None,
) -> Dict[str, Any]:
    stage = lexical_absolute(stage_value)
    supplied = lexical_absolute(capability_value)
    expected = capability_path(stage)
    if supplied != expected:
        fail(
            "result-stage capability path is not the external capability bound "
            f"to this stage: expected {expected}, found {supplied}"
        )
    capability_directory = assert_no_link_components(
        expected.parent, "result-stage capability directory"
    )
    _check_private_directory(capability_directory, "result-stage capability directory")
    record = verified or verify_result_stage(stage, invocation_stage)
    if record.invocation.stage != lexical_absolute(invocation_stage):
        fail("result-stage operation supplied a different release invocation")
    _normalize_recoverable_pending(supplied, "result-stage capability")
    raw, _ = _read_control(supplied, "result-stage capability", require_flush=True)
    _fsync_directory(capability_directory)
    return _validate_capability(
        _parse_canonical(raw, "result-stage capability"),
        record.stage,
        record.descriptor,
    )


def _validated_pending_descriptor(
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    *,
    require_flush: bool = False,
) -> Tuple[Dict[str, Any], os.stat_result]:
    _normalize_recoverable_pending(
        stage / SEAL_PENDING_NAME, "result-stage seal recovery record"
    )
    raw, metadata = _read_control(
        stage / SEAL_PENDING_NAME,
        "result-stage seal recovery record",
        require_flush=require_flush,
    )
    try:
        parsed = _parse_canonical(raw, "result-stage seal recovery record")
    except ResultStageError as error:
        raise IncompletePendingError(str(error)) from error
    value = _validate_descriptor(parsed, invocation)
    if value["state"] != "sealed":
        fail("result-stage seal recovery record is not sealed")
    return value, metadata


def _same_seal_authority(left: Dict[str, Any], right: Dict[str, Any]) -> bool:
    return {
        key: value for key, value in left.items() if key not in {"manifest", "state"}
    } == {
        key: value for key, value in right.items() if key not in {"manifest", "state"}
    }


def _open_pinned_stage(
    stage: pathlib.Path,
) -> Tuple[os.stat_result, Optional[int], Callable[[], None]]:
    metadata = _check_private_directory(stage, "result stage during seal commit")
    identity = (metadata.st_dev, metadata.st_ino)
    if os.name == "nt":
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        try:
            _handle, close_handle = (
                release_set_publication.open_pinned_windows_directory(
                    stage, identity, "result stage during seal commit"
                )
            )
        except release_set_publication.ReleaseSetError as error:
            fail(str(error))
        return metadata, None, close_handle
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(stage, flags)
    opened = os.fstat(descriptor)
    if not _same_identity(metadata, opened):
        os.close(descriptor)
        fail("result stage changed while it was pinned for seal commit")
    return metadata, descriptor, lambda: os.close(descriptor)


def _require_stage_path_identity(
    stage: pathlib.Path, expected: os.stat_result, label: str
) -> None:
    current = os.lstat(stage)
    _check_directory(current, label)
    if not _same_identity(current, expected):
        fail(f"{label} changed identity")


def _sync_pinned_stage(stage: pathlib.Path, descriptor: Optional[int]) -> None:
    if descriptor is not None:
        os.fsync(descriptor)
    else:
        _fsync_directory(stage)


def _remove_seal_pending(
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    candidate: Dict[str, Any],
) -> None:
    pending = stage / SEAL_PENDING_NAME
    try:
        observed, metadata = _validated_pending_descriptor(stage, invocation)
    except FileNotFoundError:
        return
    except IncompletePendingError:
        _discard_incomplete_pending(
            pending, "incomplete result-stage seal recovery record"
        )
        return
    if not _same_seal_authority(observed, candidate):
        fail("result-stage seal recovery record belongs to different authority")
    _unlink_verified(pending, metadata, "result-stage seal recovery record")
    _fsync_directory(stage)


def _replace_descriptor(
    stage: pathlib.Path,
    value: Dict[str, Any],
    invocation: release_invocation.VerifiedInvocation,
) -> None:
    destination = stage / DESCRIPTOR_NAME
    temporary = stage / SEAL_PENDING_NAME
    raw = canonical_bytes(value)
    if len(raw) > MAX_CONTROL_BYTES:
        fail("sealed result-stage descriptor exceeds the control-file bound")
    stage_metadata, stage_descriptor, close_stage = _open_pinned_stage(stage)
    try:
        try:
            pending_value, _ = _validated_pending_descriptor(stage, invocation)
        except FileNotFoundError:
            _require_stage_path_identity(
                stage, stage_metadata, "result stage before seal recovery publication"
            )
            _write_exclusive(temporary, raw)
            _sync_pinned_stage(stage, stage_descriptor)
        except IncompletePendingError:
            _discard_incomplete_pending(
                temporary, "incomplete result-stage seal recovery record"
            )
            _require_stage_path_identity(
                stage,
                stage_metadata,
                "result stage before recovered seal publication",
            )
            _write_exclusive(temporary, raw)
            _sync_pinned_stage(stage, stage_descriptor)
        else:
            if pending_value != value:
                if not _same_seal_authority(pending_value, value):
                    fail(
                        "result-stage seal recovery record belongs to different authority"
                    )
                _remove_seal_pending(stage, invocation, value)
                _require_stage_path_identity(
                    stage,
                    stage_metadata,
                    "result stage before replacement seal recovery publication",
                )
                _write_exclusive(temporary, raw)
                _sync_pinned_stage(stage, stage_descriptor)

        pending_value, _ = _validated_pending_descriptor(
            stage, invocation, require_flush=True
        )
        if pending_value != value:
            fail("result-stage seal recovery record differs from durable payload")
        _require_stage_path_identity(
            stage, stage_metadata, "result stage before descriptor replacement"
        )
        if stage_descriptor is not None:
            os.replace(
                temporary.name,
                destination.name,
                src_dir_fd=stage_descriptor,
                dst_dir_fd=stage_descriptor,
            )
        else:
            os.replace(temporary, destination)
        _require_stage_path_identity(
            stage, stage_metadata, "result stage after descriptor replacement"
        )
        _sync_pinned_stage(stage, stage_descriptor)
    finally:
        close_stage()


def seal_result_stage(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    invocation_stage: pathlib.Path,
) -> VerifiedResultStage:
    verified = verify_result_stage(
        stage_value,
        invocation_stage,
        _allow_seal_pending=True,
        _allow_windows_flush_pending=True,
    )
    verify_capability(
        verified.stage,
        capability_value,
        verified.invocation.stage,
        verified=verified,
    )
    payload = _durable_payload_snapshot(
        verified.stage / RESULT_ROOT_NAME, verified.descriptor
    )
    sealed = dict(verified.descriptor)
    sealed["manifest"] = _manifest(payload)
    sealed["state"] = "sealed"
    if verified.descriptor["state"] == "sealed":
        if verified.descriptor != sealed:
            fail("result stage is already sealed with different payload bytes")
        stage_metadata, stage_descriptor, close_stage = _open_pinned_stage(
            verified.stage
        )
        try:
            _remove_seal_pending(verified.stage, verified.invocation, sealed)
            _require_stage_path_identity(
                verified.stage,
                stage_metadata,
                "sealed result stage during retry completion",
            )
            _sync_pinned_stage(verified.stage, stage_descriptor)
        finally:
            close_stage()
        _fsync_directory(verified.stage.parent)
    else:
        _replace_descriptor(verified.stage, sealed, verified.invocation)
        _fsync_directory(verified.stage.parent)
    updated = verify_result_stage(verified.stage, verified.invocation.stage)
    verify_capability(
        updated.stage,
        capability_value,
        updated.invocation.stage,
        verified=updated,
    )
    return updated


def audit_projection(verified: VerifiedResultStage) -> Dict[str, Any]:
    """Return a path-free retained projection of one sealed live authority."""

    descriptor = verified.descriptor
    if descriptor["state"] != "sealed":
        fail("only a sealed result stage may be projected for offline audit")
    invocation = descriptor["invocation"]
    return {
        "schema": AUDIT_PROJECTION_SCHEMA,
        "claim": "retained-result-manifest-consistency-not-live-authority-build-causality-or-reproducibility-proof",
        "source_descriptor_sha256": sha256_bytes(canonical_bytes(descriptor)),
        "invocation": {
            "descriptor_sha256": invocation["descriptor_sha256"],
            "invocation_id": invocation["invocation_id"],
            "result_name": invocation["result_name"],
        },
        "allocation_nonce": descriptor["allocation_nonce"],
        "result_root": descriptor["result_root"],
        "manifest": descriptor["manifest"],
        "scope": descriptor["scope"],
        "stage_id": descriptor["stage_id"],
        "state": "sealed",
    }


def verify_audit_projection(
    value: object, invocation_descriptor: Dict[str, Any]
) -> Dict[str, Any]:
    """Validate a retained path-free result projection against an invocation."""

    projection = _exact_object(
        value,
        "result-stage audit projection",
        (
            "schema",
            "claim",
            "source_descriptor_sha256",
            "invocation",
            "allocation_nonce",
            "result_root",
            "manifest",
            "scope",
            "stage_id",
            "state",
        ),
    )
    if projection["schema"] != AUDIT_PROJECTION_SCHEMA:
        fail("result-stage audit projection has an invalid schema")
    if (
        projection["claim"]
        != "retained-result-manifest-consistency-not-live-authority-build-causality-or-reproducibility-proof"
    ):
        fail("result-stage audit projection overstates its claim")
    binding = _exact_object(
        projection["invocation"],
        "result-stage projected invocation",
        ("descriptor_sha256", "invocation_id", "result_name"),
    )
    expected_binding = {
        "descriptor_sha256": release_invocation.sha256_bytes(
            release_invocation.canonical_bytes(invocation_descriptor)
        ),
        "invocation_id": invocation_descriptor["invocation_id"],
        "result_name": invocation_descriptor["resources"]["result_name"],
    }
    if binding != expected_binding:
        fail("result-stage projection disagrees with the invocation descriptor")
    for field in ("source_descriptor_sha256", "allocation_nonce", "stage_id"):
        if not _is_digest(projection[field]):
            fail(f"result-stage audit projection {field} is invalid")
    if projection["result_root"] != RESULT_ROOT_NAME or projection["state"] != "sealed":
        fail("result-stage audit projection is not a sealed canonical result root")
    _validate_manifest(projection["manifest"])
    if projection["scope"] != {
        "claim": CLAIM,
        "does_not_claim": list(NON_CLAIMS),
    }:
        fail("result-stage audit projection scope is invalid or overstated")
    return projection


def _same_identity(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def _unlink_verified(path: pathlib.Path, expected: os.stat_result, label: str) -> None:
    current = os.lstat(path)
    if _safe_payload_metadata(current, label) != "file":
        fail(f"{label} is no longer a regular file")
    if not _same_identity(current, expected):
        fail(f"{label} changed before unlink")
    if os.name == "nt":
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        try:
            handle, close_handle = release_set_publication.open_pinned_windows_file(
                path, (expected.st_dev, expected.st_ino), label
            )
        except release_set_publication.ReleaseSetError as error:
            fail(str(error))
        try:
            release_set_publication.mark_windows_handle_delete(handle)
        finally:
            close_handle()
        try:
            os.lstat(path)
        except FileNotFoundError:
            return
        fail(f"{label} remained linked after Windows handle deletion")
    if os.name == "posix" and os.unlink in os.supports_dir_fd:
        parent_fd = os.open(
            path.parent,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            current_at = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            if _safe_payload_metadata(
                current_at, label
            ) != "file" or not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative unlink")
            os.unlink(path.name, dir_fd=parent_fd)
        finally:
            os.close(parent_fd)
    else:
        os.unlink(path)


def _rmdir_verified(path: pathlib.Path, expected: os.stat_result, label: str) -> None:
    current = os.lstat(path)
    _check_directory(current, label)
    if not _same_identity(current, expected):
        fail(f"{label} changed before removal")
    if os.name == "nt":
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        try:
            handle, close_handle = (
                release_set_publication.open_pinned_windows_directory(
                    path,
                    (expected.st_dev, expected.st_ino),
                    label,
                    movable=True,
                )
            )
        except release_set_publication.ReleaseSetError as error:
            fail(str(error))
        try:
            release_set_publication.mark_windows_handle_delete(handle)
        finally:
            close_handle()
        try:
            os.lstat(path)
        except FileNotFoundError:
            return
        fail(f"{label} remained linked after Windows handle deletion")
    if os.name == "posix" and os.rmdir in os.supports_dir_fd:
        parent_fd = os.open(
            path.parent,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            current_at = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            _check_directory(current_at, label)
            if not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative removal")
            os.rmdir(path.name, dir_fd=parent_fd)
        finally:
            os.close(parent_fd)
    else:
        os.rmdir(path)


def _retirement_path(stage: pathlib.Path, capability: Dict[str, Any]) -> pathlib.Path:
    return stage.parent / f"{RETIRE_PREFIX}{capability['stage_id']}"


def _retire_stage_directory(
    stage: pathlib.Path,
    retirement: pathlib.Path,
    expected: os.stat_result,
) -> os.stat_result:
    assert_no_link_components(stage, "result stage before retirement")
    assert_no_link_components(
        retirement, "result-stage retirement path", allow_missing_leaf=True
    )
    try:
        os.lstat(retirement)
    except FileNotFoundError:
        pass
    else:
        fail("result-stage retirement path already exists")
    if os.name == "nt":
        _atomic_publish_directory, release_set_publication = (
            _load_windows_security_helpers()
        )
        parent_metadata = os.lstat(stage.parent)
        close_parent: Optional[Callable[[], None]] = None
        close_stage: Optional[Callable[[], None]] = None
        try:
            try:
                parent_handle, close_parent = (
                    release_set_publication.open_pinned_windows_directory(
                        stage.parent,
                        (parent_metadata.st_dev, parent_metadata.st_ino),
                        "result-stage retirement parent",
                    )
                )
                stage_handle, close_stage = (
                    release_set_publication.open_pinned_windows_directory(
                        stage,
                        (expected.st_dev, expected.st_ino),
                        "result stage before retirement",
                        movable=True,
                    )
                )
            except release_set_publication.ReleaseSetError as error:
                fail(str(error))
            release_set_publication.rename_windows_directory_handle_noreplace(
                stage_handle, parent_handle, retirement.name
            )
        finally:
            if close_stage is not None:
                close_stage()
            if close_parent is not None:
                close_parent()
    else:
        parent_descriptor = os.open(
            stage.parent,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            opened = os.stat(
                stage.name, dir_fd=parent_descriptor, follow_symlinks=False
            )
            if not _same_identity(opened, expected):
                fail("result stage changed before retirement rename")
            if sys.platform.startswith("linux"):
                atomic_publish_directory, _release_set_publication = (
                    _load_windows_security_helpers()
                )
                try:
                    atomic_publish_directory.linux_rename_directory_noreplace(
                        parent_descriptor,
                        stage.name,
                        parent_descriptor,
                        retirement.name,
                    )
                except atomic_publish_directory.DirectoryPublishError as error:
                    fail(str(error))
            else:
                os.rename(
                    stage.name,
                    retirement.name,
                    src_dir_fd=parent_descriptor,
                    dst_dir_fd=parent_descriptor,
                )
        finally:
            os.close(parent_descriptor)
    try:
        os.lstat(stage)
    except FileNotFoundError:
        pass
    else:
        fail("result stage remained at its canonical path after retirement")
    retired = os.lstat(retirement)
    _check_directory(retired, "retired result stage", private=True)
    if not _same_identity(retired, expected):
        fail("retired result-stage identity changed during rename")
    _fsync_directory(stage.parent)
    return retired


def _read_destroy_pending(
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    *,
    require_flush: bool = False,
) -> Tuple[Dict[str, Any], os.stat_result]:
    _normalize_recoverable_pending(
        stage / DESTROY_PENDING_NAME, "result-stage destroy recovery record"
    )
    raw, metadata = _read_control(
        stage / DESTROY_PENDING_NAME,
        "result-stage destroy recovery record",
        require_flush=require_flush,
    )
    try:
        parsed = _parse_canonical(raw, "result-stage destroy recovery record")
    except ResultStageError as error:
        raise IncompletePendingError(str(error)) from error
    value = _validate_descriptor(parsed, invocation)
    if value["state"] != "destroying":
        fail("result-stage destroy recovery record is not destroying")
    return value, metadata


def _remove_destroy_pending(
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    candidate: Dict[str, Any],
) -> None:
    try:
        observed, metadata = _read_destroy_pending(stage, invocation)
    except FileNotFoundError:
        return
    except IncompletePendingError:
        _discard_incomplete_pending(
            stage / DESTROY_PENDING_NAME,
            "incomplete result-stage destroy recovery record",
        )
        return
    if not _same_seal_authority(observed, candidate):
        fail("result-stage destroy recovery record belongs to different authority")
    _unlink_verified(
        stage / DESTROY_PENDING_NAME,
        metadata,
        "result-stage destroy recovery record",
    )
    _fsync_directory(stage)


def _prepare_destroying_descriptor(
    stage: pathlib.Path,
    invocation: release_invocation.VerifiedInvocation,
    descriptor: Dict[str, Any],
) -> Dict[str, Any]:
    _recover_windows_flush_pending(stage, descriptor, restore=True)
    if descriptor["state"] == "destroying":
        try:
            seal_pending, _ = _validated_pending_descriptor(stage, invocation)
        except FileNotFoundError:
            pass
        except IncompletePendingError:
            _discard_incomplete_pending(
                stage / SEAL_PENDING_NAME,
                "incomplete result-stage seal recovery record",
            )
        else:
            if not _same_seal_authority(seal_pending, descriptor):
                fail("result-stage seal recovery record belongs to different authority")
            _remove_seal_pending(stage, invocation, seal_pending)
        _remove_destroy_pending(stage, invocation, descriptor)
        _fsync_directory(stage)
        _fsync_directory(stage.parent)
        return descriptor

    if descriptor["state"] not in {"building", "sealed"}:
        fail("result-stage descriptor cannot enter destruction")
    try:
        seal_pending, _ = _validated_pending_descriptor(stage, invocation)
    except FileNotFoundError:
        pass
    except IncompletePendingError:
        _discard_incomplete_pending(
            stage / SEAL_PENDING_NAME,
            "incomplete result-stage seal recovery record",
        )
    else:
        if not _same_seal_authority(seal_pending, descriptor):
            fail("result-stage seal recovery record belongs to different authority")
        _remove_seal_pending(stage, invocation, seal_pending)

    with os.scandir(stage) as scanned:
        names = {entry.name for entry in scanned}
    expected_names = {DESCRIPTOR_NAME, RESULT_ROOT_NAME}
    allowed_names = expected_names | {DESTROY_PENDING_NAME}
    if not expected_names.issubset(names) or not names.issubset(allowed_names):
        fail(
            "result stage is not exact before destruction "
            f"(missing={sorted(expected_names - names)}, "
            f"extra={sorted(names - expected_names)})"
        )

    result_root = stage / RESULT_ROOT_NAME
    _check_private_directory(result_root, "result payload root before destruction")
    payload = _walk_payload(result_root, hash_files=True)
    observed_manifest = _manifest(payload)
    if descriptor["state"] == "sealed" and descriptor["manifest"] != observed_manifest:
        fail("sealed result-stage payload changed before destruction")
    manifest = _destruction_manifest(result_root, payload)
    _preflight_posix_directory_removal(result_root, manifest)
    candidate = dict(descriptor)
    candidate["manifest"] = manifest
    candidate["state"] = "destroying"
    candidate_raw = canonical_bytes(candidate)
    if len(candidate_raw) > MAX_CONTROL_BYTES:
        fail("result-stage destruction descriptor exceeds the control-file bound")

    stage_metadata, stage_descriptor, close_stage = _open_pinned_stage(stage)
    try:
        try:
            pending, _ = _read_destroy_pending(stage, invocation)
        except FileNotFoundError:
            _require_stage_path_identity(
                stage,
                stage_metadata,
                "result stage before destroy recovery publication",
            )
            _write_exclusive(stage / DESTROY_PENDING_NAME, candidate_raw)
            _sync_pinned_stage(stage, stage_descriptor)
        except IncompletePendingError:
            _discard_incomplete_pending(
                stage / DESTROY_PENDING_NAME,
                "incomplete result-stage destroy recovery record",
            )
            _write_exclusive(stage / DESTROY_PENDING_NAME, candidate_raw)
            _sync_pinned_stage(stage, stage_descriptor)
        else:
            if pending != candidate:
                if not _same_seal_authority(pending, candidate):
                    fail(
                        "result-stage destroy recovery record belongs to different authority"
                    )
                _remove_destroy_pending(stage, invocation, candidate)
                _write_exclusive(stage / DESTROY_PENDING_NAME, candidate_raw)
                _sync_pinned_stage(stage, stage_descriptor)

        observed = _walk_payload(stage / RESULT_ROOT_NAME, hash_files=True)
        if _destruction_manifest(stage / RESULT_ROOT_NAME, observed) != manifest:
            fail("result payload changed while destruction was prepared")
        raw, _ = _read_control(
            stage / DESCRIPTOR_NAME, "result-stage descriptor before destruction"
        )
        current_descriptor = _validate_descriptor(
            _parse_canonical(raw, "result-stage descriptor before destruction"),
            invocation,
        )
        if current_descriptor != descriptor:
            fail("result-stage descriptor changed while destruction was prepared")
        pending, _ = _read_destroy_pending(stage, invocation, require_flush=True)
        if pending != candidate:
            fail("result-stage destroy recovery record changed before commit")
        _require_stage_path_identity(
            stage, stage_metadata, "result stage before destroy descriptor commit"
        )
        if stage_descriptor is not None:
            os.replace(
                DESTROY_PENDING_NAME,
                DESCRIPTOR_NAME,
                src_dir_fd=stage_descriptor,
                dst_dir_fd=stage_descriptor,
            )
        else:
            os.replace(
                stage / DESTROY_PENDING_NAME,
                stage / DESCRIPTOR_NAME,
            )
        _require_stage_path_identity(
            stage, stage_metadata, "result stage after destroy descriptor commit"
        )
        _sync_pinned_stage(stage, stage_descriptor)
    finally:
        close_stage()
    _fsync_directory(stage.parent)
    raw, _ = _read_control(
        stage / DESCRIPTOR_NAME, "committed result-stage destruction descriptor"
    )
    committed = _validate_descriptor(
        _parse_canonical(raw, "committed result-stage destruction descriptor"),
        invocation,
    )
    if committed != candidate:
        fail("committed result-stage destruction descriptor differs from its plan")
    return committed


def _destroying_payload_subset(
    stage: pathlib.Path, descriptor: Dict[str, Any]
) -> Dict[str, Any]:
    root = stage / RESULT_ROOT_NAME
    try:
        root_metadata = os.lstat(root)
    except FileNotFoundError:
        return {"directories": [], "files": []}
    _check_directory(root_metadata, "destroying result payload root", private=True)
    _require_private_windows_path(root, "destroying result payload root")
    manifest = descriptor["manifest"]
    if (root_metadata.st_dev, root_metadata.st_ino) != (
        manifest["root_dev"],
        manifest["root_ino"],
    ):
        fail("destroying result payload root identity changed")
    observed = _walk_payload(root, hash_files=False)
    expected_directories = {item["path"]: item for item in manifest["directories"]}
    expected_files = {item["path"]: item for item in manifest["files"]}
    for item in observed["directories"]:
        expected = expected_directories.get(item["path"])
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if expected is None or (metadata.st_dev, metadata.st_ino) != (
            expected["dev"],
            expected["ino"],
        ):
            fail(
                "destroying result stage contains an unplanned or changed directory: "
                f"{item['path']}"
            )
    for item in observed["files"]:
        expected = expected_files.get(item["path"])
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if expected is None or (metadata.st_dev, metadata.st_ino) != (
            expected["dev"],
            expected["ino"],
        ):
            fail(
                "destroying result stage contains an unplanned or changed file: "
                f"{item['path']}"
            )
    return observed


def _flush_existing_directory(path: pathlib.Path) -> None:
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        return
    _check_directory(metadata, f"result-stage cleanup durability boundary {path}")
    _fsync_directory(path)


def _preflight_linux_inode_flags(
    path: pathlib.Path, expected: os.stat_result, label: str
) -> None:
    if not sys.platform.startswith("linux"):
        return
    import array
    import errno
    import fcntl

    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if stat.S_ISDIR(expected.st_mode):
        flags |= getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if not _same_identity(opened, expected):
            fail(f"{label} changed before Linux inode-flag preflight")
        # FS_IOC_GETFLAGS is _IOR('f', 1, long).  Its encoded size follows the
        # native C long size even though the flag word currently uses 32 bits.
        request = 0x80086601 if ctypes.sizeof(ctypes.c_long) == 8 else 0x80046601
        value = array.array("L", [0])
        unsupported = {
            errno.ENOTTY,
            errno.EOPNOTSUPP,
            getattr(errno, "ENOSYS", -1),
        }
        try:
            fcntl.ioctl(descriptor, request, value, True)
        except OSError as error:
            if error.errno not in unsupported:
                raise
            flags_supported = False
        else:
            flags_supported = True
            if value[0] & (0x00000010 | 0x00000020):
                fail(f"{label} has Linux immutable or append-only inode flags")
        # Ownership and the absence of blocking flags are facts consumed by the
        # durable deletion plan.  Flush this exact inode before checking them
        # again so a recently visible chown/chattr transition cannot roll back
        # behind a committed destroying descriptor.
        os.fsync(descriptor)
        if flags_supported:
            value[0] = 0
            fcntl.ioctl(descriptor, request, value, True)
        final = os.fstat(descriptor)
        current = os.lstat(path)
        if not _same_identity(opened, final) or not _same_identity(final, current):
            fail(f"{label} changed during Linux inode-flag preflight")
        if flags_supported and value[0] & (0x00000010 | 0x00000020):
            fail(f"{label} has Linux immutable or append-only inode flags")
    finally:
        os.close(descriptor)


def _preflight_posix_directory_removal(
    root: pathlib.Path, manifest: Dict[str, Any]
) -> None:
    if os.name != "posix":
        return
    records = [(root, manifest["root_dev"], manifest["root_ino"], ".")]
    records.extend(
        (
            root / pathlib.PurePosixPath(item["path"]),
            item["dev"],
            item["ino"],
            item["path"],
        )
        for item in manifest["directories"]
    )
    directories: Dict[pathlib.Path, Tuple[os.stat_result, int]] = {}
    for path, expected_dev, expected_ino, relative in records:
        metadata = os.lstat(path)
        _check_directory(metadata, f"destroy result directory {relative}")
        if (metadata.st_dev, metadata.st_ino) != (expected_dev, expected_ino):
            fail(f"destroy result directory {relative} changed before preflight")
        _preflight_linux_inode_flags(
            path, metadata, f"destroy result directory {relative}"
        )
        effective_mode = stat.S_IMODE(metadata.st_mode)
        if not os.access(path, os.W_OK | os.X_OK):
            _check_owner(metadata, f"non-writable destroy result directory {relative}")
            effective_mode = 0o700
        directories[path] = (metadata, effective_mode)

    for item in manifest["files"]:
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if (metadata.st_dev, metadata.st_ino) != (item["dev"], item["ino"]):
            fail(f"destroy result file {item['path']} changed before preflight")
        _preflight_linux_inode_flags(
            path, metadata, f"destroy result file {item['path']}"
        )

    effective_uid = os.geteuid()
    if effective_uid == 0:
        return
    for item in [*manifest["directories"], *manifest["files"]]:
        path = root / pathlib.PurePosixPath(item["path"])
        parent_metadata, parent_mode = directories[path.parent]
        if not parent_mode & stat.S_ISVTX:
            continue
        entry_metadata = os.lstat(path)
        if (
            parent_metadata.st_uid != effective_uid
            and entry_metadata.st_uid != effective_uid
        ):
            fail(
                "sticky result directory does not permit removal of planned entry: "
                f"{item['path']}"
            )


def _prepare_posix_directories_for_removal(
    root: pathlib.Path,
    directory_records: List[Tuple[pathlib.Path, os.stat_result, str]],
) -> None:
    if os.name != "posix":
        return
    root_metadata = os.lstat(root)
    records = [(root, root_metadata, "."), *directory_records]
    records.sort(key=lambda record: (record[2].count("/"), record[2]))
    for path, expected, relative in records:
        if os.access(path, os.W_OK | os.X_OK):
            continue
        _check_owner(expected, f"destroy result directory {relative}")
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            opened = os.fstat(descriptor)
            _check_directory(opened, f"destroy result directory {relative}")
            if not _same_identity(opened, expected):
                fail(f"destroy result directory {relative} changed before chmod")
            os.fchmod(descriptor, 0o700)
            os.fsync(descriptor)
            final = os.fstat(descriptor)
            if (
                not _same_identity(final, expected)
                or stat.S_IMODE(final.st_mode) != 0o700
            ):
                fail(f"destroy result directory {relative} was not made removable")
        finally:
            os.close(descriptor)


def destroy_result_stage(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    invocation_stage: pathlib.Path,
) -> None:
    stage = lexical_absolute(stage_value)
    logical_stage = stage
    capability = lexical_absolute(capability_value)
    expected_capability = capability_path(stage)
    if capability != expected_capability:
        fail(f"result-stage capability path must be {expected_capability}")

    try:
        stage_metadata = os.lstat(stage)
    except FileNotFoundError:
        stage_metadata = None
    try:
        capability_metadata = os.lstat(capability)
    except FileNotFoundError:
        capability_metadata = None
    if stage_metadata is None and capability_metadata is None:
        stage_parent = assert_no_link_components(
            stage.parent, "result-stage cleanup parent"
        )
        _fsync_directory(stage_parent)
        _flush_existing_directory(capability.parent)
        _fsync_directory(stage_parent)
        return
    if capability_metadata is None:
        fail("result-stage capability is absent while the stage remains")

    invocation = release_invocation.verify_invocation(invocation_stage)
    capability_directory = assert_no_link_components(
        capability.parent, "result-stage capability directory"
    )
    _check_private_directory(capability_directory, "result-stage capability directory")
    capability_metadata = _normalize_recoverable_pending(
        capability, "destroy result-stage capability"
    )
    capability_raw, capability_metadata = _read_control(
        capability, "destroy result-stage capability", require_flush=True
    )
    _fsync_directory(capability_directory)
    capability_record = _validate_capability_for_invocation(
        _parse_canonical(capability_raw, "destroy result-stage capability"),
        logical_stage,
        invocation,
    )
    retirement = _retirement_path(logical_stage, capability_record)
    try:
        retirement_metadata = os.lstat(retirement)
    except FileNotFoundError:
        retirement_metadata = None
    if stage_metadata is not None and retirement_metadata is not None:
        fail("canonical and retired result stages both exist")
    retired = retirement_metadata is not None
    if stage_metadata is None and retirement_metadata is None:
        _fsync_directory(logical_stage.parent)
        _unlink_verified(
            capability,
            capability_metadata,
            "orphaned result-stage capability",
        )
        _fsync_directory(capability_directory)
        _fsync_directory(logical_stage.parent)
        return
    if retired:
        if capability_record["schema"] != CAPABILITY_SCHEMA:
            fail("legacy result-stage capability cannot authorize a retired stage")
        stage = retirement
        stage_metadata = retirement_metadata
        if (capability_record["stage_dev"], capability_record["stage_ino"]) != (
            stage_metadata.st_dev,
            stage_metadata.st_ino,
        ):
            fail("retired result stage does not match its capability identity")

    stage = assert_no_link_components(stage, "result stage during destruction")
    stage_metadata = _check_private_directory(stage, "result stage during destruction")
    with os.scandir(stage) as scanned:
        initial_names = {entry.name for entry in scanned}
    allowed_names = {
        DESCRIPTOR_NAME,
        RESULT_ROOT_NAME,
        SEAL_PENDING_NAME,
        DESTROY_PENDING_NAME,
        WINDOWS_FLUSH_PENDING_NAME,
    }
    if not initial_names.issubset(allowed_names):
        fail(
            "result stage contains an unexpected entry during destruction: "
            f"{sorted(initial_names - allowed_names)}"
        )
    for path, metadata, label in (
        (stage.parent, os.lstat(stage.parent), "result-stage parent"),
        (stage, stage_metadata, "result stage"),
        (capability_directory, os.lstat(capability_directory), "capability parent"),
        (capability, capability_metadata, "result-stage capability"),
        *(
            (
                stage / name,
                os.lstat(stage / name),
                f"result-stage entry {name}",
            )
            for name in sorted(initial_names)
        ),
    ):
        _preflight_linux_inode_flags(path, metadata, label)
    if DESCRIPTOR_NAME not in initial_names:
        if initial_names:
            fail("result stage has cleanup entries but no intact deletion plan")
        if capability_record["schema"] == CAPABILITY_SCHEMA and not retired:
            fail("empty canonical result stage requires creation recovery")
        _rmdir_verified(stage, stage_metadata, "empty result stage during destruction")
        _fsync_directory(stage.parent)
        _unlink_verified(capability, capability_metadata, "result-stage capability")
        _fsync_directory(capability_directory)
        _fsync_directory(stage.parent)
        return

    _normalize_recoverable_pending(
        stage / DESCRIPTOR_NAME, "destroy result-stage descriptor"
    )
    descriptor_raw, descriptor_metadata = _read_control(
        stage / DESCRIPTOR_NAME,
        "destroy result-stage descriptor",
        require_flush=True,
    )
    _fsync_directory(stage)
    descriptor = _validate_descriptor(
        _parse_canonical(descriptor_raw, "destroy result-stage descriptor"), invocation
    )
    if logical_stage.name != _expected_stage_name(descriptor):
        fail("result-stage name is not canonically descriptor-derived")
    _validate_capability(
        capability_record,
        logical_stage,
        descriptor,
        stage_metadata=stage_metadata,
    )
    if RESULT_ROOT_NAME not in initial_names and descriptor["state"] == "building":
        if (
            initial_names != {DESCRIPTOR_NAME}
            or descriptor["state"] != "building"
            or descriptor["manifest"] is not None
        ):
            fail("partial result-stage creation is not safely recoverable")
        if not retired and capability_record["schema"] == CAPABILITY_SCHEMA:
            stage_metadata = _retire_stage_directory(stage, retirement, stage_metadata)
            stage = retirement
            retired = True
        _unlink_verified(
            stage / DESCRIPTOR_NAME,
            descriptor_metadata,
            "partial result-stage creation descriptor",
        )
        _fsync_directory(stage)
        final_stage_metadata = os.lstat(stage)
        _check_directory(
            final_stage_metadata, "empty partial result stage before retirement"
        )
        _rmdir_verified(
            stage, final_stage_metadata, "partial result-stage creation stage"
        )
        _fsync_directory(stage.parent)
        _unlink_verified(
            capability,
            capability_metadata,
            "partial result-stage creation capability",
        )
        _fsync_directory(capability_directory)
        _fsync_directory(stage.parent)
        return
    descriptor = _prepare_destroying_descriptor(stage, invocation, descriptor)
    payload = _destroying_payload_subset(stage, descriptor)

    file_records: List[Tuple[pathlib.Path, os.stat_result, str]] = []
    directory_records: List[Tuple[pathlib.Path, os.stat_result, str]] = []
    root = stage / RESULT_ROOT_NAME
    for item in payload["files"]:
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if (
            _safe_payload_metadata(metadata, f"destroy result file {item['path']}")
            != "file"
        ):
            fail(f"destroy result file {item['path']} changed type")
        file_records.append((path, metadata, item["path"]))
    for item in payload["directories"]:
        path = root / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        _check_directory(metadata, f"destroy result directory {item['path']}")
        directory_records.append((path, metadata, item["path"]))

    if payload["directories"] or payload["files"]:
        _prepare_posix_directories_for_removal(root, directory_records)
    for path, metadata, relative in file_records:
        _unlink_verified(path, metadata, f"destroy result file {relative}")
        _fsync_directory(path.parent)
    for path, metadata, relative in sorted(
        directory_records,
        key=lambda record: (record[2].count("/"), record[2]),
        reverse=True,
    ):
        _rmdir_verified(path, metadata, f"destroy result directory {relative}")
        _fsync_directory(path.parent)
    try:
        root_metadata = os.lstat(root)
    except FileNotFoundError:
        pass
    else:
        _check_directory(root_metadata, "destroy result payload root")
        _rmdir_verified(root, root_metadata, "destroy result payload root")
        _fsync_directory(stage)

    if not retired and capability_record["schema"] == CAPABILITY_SCHEMA:
        pre_retirement_metadata = os.lstat(stage)
        _check_directory(
            pre_retirement_metadata, "result stage before retirement rename"
        )
        with os.scandir(stage) as scanned:
            remaining_names = {entry.name for entry in scanned}
        if remaining_names != {DESCRIPTOR_NAME}:
            fail(
                "result stage is not descriptor-only before retirement rename: "
                f"{sorted(remaining_names)}"
            )
        stage_metadata = _retire_stage_directory(
            stage, retirement, pre_retirement_metadata
        )
        stage = retirement
        retired = True

    descriptor_raw, descriptor_metadata = _read_control(
        stage / DESCRIPTOR_NAME, "final result-stage destruction descriptor"
    )
    final_descriptor = _validate_descriptor(
        _parse_canonical(descriptor_raw, "final result-stage destruction descriptor"),
        invocation,
    )
    if final_descriptor != descriptor:
        fail("result-stage destruction descriptor changed before retirement")
    _unlink_verified(
        stage / DESCRIPTOR_NAME,
        descriptor_metadata,
        "destroy result-stage descriptor",
    )
    _fsync_directory(stage)
    final_stage_metadata = os.lstat(stage)
    _check_directory(final_stage_metadata, "empty result stage before retirement")
    with os.scandir(stage) as scanned:
        if next(scanned, None) is not None:
            fail("result stage is not empty before retirement")
    _rmdir_verified(stage, final_stage_metadata, "destroy result stage")
    _fsync_directory(stage.parent)
    _unlink_verified(capability, capability_metadata, "result-stage capability")
    _fsync_directory(capability_directory)
    _fsync_directory(stage.parent)


def _validate_create_result(value: object) -> Dict[str, Any]:
    result = _exact_object(
        value,
        "result-stage create result",
        (
            "capability",
            "descriptor",
            "invocation_id",
            "invocation_stage",
            "result_name",
            "result_root",
            "schema",
            "stage",
            "stage_id",
            "state",
        ),
    )
    if (
        result["schema"] != RESULT_SCHEMA
        or result["state"] != "building"
        or not _is_digest(result["invocation_id"])
        or not _is_digest(result["stage_id"])
    ):
        fail("result-stage create result schema, state, or identity is invalid")
    for key in (
        "capability",
        "descriptor",
        "invocation_stage",
        "result_name",
        "result_root",
        "stage",
    ):
        if (
            not isinstance(result[key], str)
            or not result[key]
            or _contains_control(result[key])
        ):
            fail(f"result-stage create result {key} is unsafe")
    stage = lexical_absolute(pathlib.Path(result["stage"]))
    invocation_stage = lexical_absolute(pathlib.Path(result["invocation_stage"]))
    if (
        not pathlib.Path(result["stage"]).is_absolute()
        or not pathlib.Path(result["invocation_stage"]).is_absolute()
    ):
        fail("result-stage create result stage paths must be absolute")
    expected_name = f"{STAGE_PREFIX}{result['result_name']}-{result['stage_id']}"
    if stage.name != expected_name:
        fail("result-stage create result stage name is not canonical")
    if lexical_absolute(pathlib.Path(result["descriptor"])) != stage / DESCRIPTOR_NAME:
        fail("result-stage create result descriptor path is not canonical")
    if (
        lexical_absolute(pathlib.Path(result["result_root"]))
        != stage / RESULT_ROOT_NAME
    ):
        fail("result-stage create result payload root path is not canonical")
    if lexical_absolute(pathlib.Path(result["capability"])) != capability_path(stage):
        fail("result-stage create result capability path is not canonical")
    prefix = "dcentos-ri-"
    suffix = f"-{result['invocation_id']}-result"
    logical_name = result["result_name"][len(prefix) : -len(suffix)]
    if (
        not release_invocation.RESOURCE_RE.fullmatch(result["result_name"])
        or not result["result_name"].startswith(prefix)
        or not result["result_name"].endswith(suffix)
        or not release_invocation.NAME_RE.fullmatch(logical_name)
        or release_invocation._resource_names(logical_name, result["invocation_id"])[
            "result_name"
        ]
        != result["result_name"]
    ):
        fail("result-stage create result name is not invocation-derived")
    if invocation_stage == stage or stage in invocation_stage.parents:
        fail("result-stage create result has inconsistent stage paths")
    return result


QUERY_FIELDS = (
    "capability",
    "descriptor",
    "files_count",
    "invocation_id",
    "invocation_stage",
    "manifest_sha256",
    "result_name",
    "result_root",
    "stage",
    "stage_id",
    "state",
)


def _stage_query(verified: VerifiedResultStage, field: str) -> object:
    manifest = verified.descriptor["manifest"]
    values: Dict[str, object] = {
        "capability": str(capability_path(verified.stage)),
        "descriptor": str(verified.stage / DESCRIPTOR_NAME),
        "invocation_id": verified.descriptor["invocation"]["invocation_id"],
        "invocation_stage": verified.descriptor["invocation"]["stage"],
        "result_name": verified.descriptor["invocation"]["result_name"],
        "result_root": str(verified.stage / RESULT_ROOT_NAME),
        "stage": str(verified.stage),
        "stage_id": verified.descriptor["stage_id"],
        "state": verified.descriptor["state"],
    }
    if manifest is not None:
        values["files_count"] = len(manifest["files"])
        values["manifest_sha256"] = manifest["manifest_sha256"]
    if field not in values:
        fail(f"field {field} is unavailable until the result stage is sealed")
    return values[field]


def _result_query(result: Dict[str, Any], field: str) -> object:
    if field not in result:
        fail(f"field {field} is unavailable in immutable create-result JSON")
    return result[field]


def _print_scalar(value: object) -> None:
    if isinstance(value, str):
        text = value
    elif isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        text = str(value)
    else:
        fail("result-stage query did not select a safe scalar")
    if not text or _contains_control(text):
        fail("result-stage query result is unsafe for shell transport")
    print(text)


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)

    create = commands.add_parser(
        "create", help="allocate an invocation-bound result stage"
    )
    create.add_argument("--stage-parent", required=True)
    create.add_argument("--invocation-stage", required=True)
    create.add_argument("--result-output", required=True)

    verify = commands.add_parser(
        "verify", help="verify result-stage structure and integrity"
    )
    verify.add_argument("--invocation-stage", required=True)
    verify.add_argument("stage")

    seal = commands.add_parser("seal", help="seal the exact result payload manifest")
    seal.add_argument("--capability", required=True)
    seal.add_argument("--invocation-stage", required=True)
    seal.add_argument("stage")

    project = commands.add_parser(
        "project-audit",
        help="emit a path-free audit projection from a sealed live result stage",
    )
    project.add_argument("--invocation-stage", required=True)
    project.add_argument("stage")

    query = commands.add_parser("query", help="query one verified result-stage scalar")
    query.add_argument("--field", required=True, choices=QUERY_FIELDS)
    query.add_argument("--invocation-stage", required=True)
    query.add_argument("stage")

    query_result = commands.add_parser(
        "query-result", help="query canonical create-result JSON from stdin"
    )
    query_result.add_argument(
        "--field",
        required=True,
        choices=tuple(
            field
            for field in QUERY_FIELDS
            if field not in ("files_count", "manifest_sha256")
        ),
    )

    destroy = commands.add_parser(
        "destroy", help="destroy one safe result stage using its external capability"
    )
    destroy.add_argument("--capability", required=True)
    destroy.add_argument("--invocation-stage", required=True)
    destroy.add_argument("stage")
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "create":
            created = create_result_stage(
                pathlib.Path(args.stage_parent),
                pathlib.Path(args.invocation_stage),
                result_output=pathlib.Path(args.result_output),
            )
            print(canonical_bytes(created.cli_result()).decode("ascii"), end="")
        elif args.command == "verify":
            verified = verify_result_stage(
                pathlib.Path(args.stage), pathlib.Path(args.invocation_stage)
            )
            print(
                "release result stage verified: "
                f"id={verified.descriptor['stage_id']} "
                f"state={verified.descriptor['state']}"
            )
        elif args.command == "seal":
            sealed = seal_result_stage(
                pathlib.Path(args.stage),
                pathlib.Path(args.capability),
                pathlib.Path(args.invocation_stage),
            )
            print(
                "release result stage sealed: "
                f"id={sealed.descriptor['stage_id']} "
                f"manifest={sealed.descriptor['manifest']['manifest_sha256']}"
            )
        elif args.command == "project-audit":
            verified = verify_result_stage(
                pathlib.Path(args.stage), pathlib.Path(args.invocation_stage)
            )
            print(canonical_bytes(audit_projection(verified)).decode("ascii"), end="")
        elif args.command == "query":
            verified = verify_result_stage(
                pathlib.Path(args.stage), pathlib.Path(args.invocation_stage)
            )
            _print_scalar(_stage_query(verified, args.field))
        elif args.command == "query-result":
            raw = sys.stdin.buffer.read(MAX_CONTROL_BYTES + 1).replace(b"\r\n", b"\n")
            if len(raw) > MAX_CONTROL_BYTES:
                fail("result-stage create result is too large")
            result = _validate_create_result(
                _parse_canonical(raw, "result-stage create result")
            )
            _print_scalar(_result_query(result, args.field))
        else:
            destroy_result_stage(
                pathlib.Path(args.stage),
                pathlib.Path(args.capability),
                pathlib.Path(args.invocation_stage),
            )
        return 0
    except (
        ResultStageError,
        release_invocation.InvocationError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        print(f"ERROR: release result stage: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
