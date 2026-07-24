#!/usr/bin/env python3
"""Seal and atomically publish an exact, capability-owned release directory."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import stat
import sys
import tempfile
import unicodedata
from collections.abc import Callable
from typing import NoReturn

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIRECTORY))

from atomic_publish_file import (  # noqa: E402
    DestinationExistsError,
    PublishError,
    atomic_publish as publish_staged_file,
    report_after_commit,
    warn_after_commit,
)
from atomic_publish_directory import (  # noqa: E402
    DirectoryPublishError,
    WINDOWS_PRIVATE_DIRECTORY_SDDL,
    WINDOWS_PRIVATE_FILE_SDDL,
    WINDOWS_PUBLIC_FILE_SDDL,
    atomic_publish_directory,
    linux_exchange_paths,
    linux_rename_directory_noreplace,
    set_windows_directory_acl,
    set_windows_file_acl,
)
from durable_file_io import fsync_directory  # noqa: E402


CAPABILITY_SCHEMA = "dcentos.release-set-capability.v1"
FILES_SCHEMA = "dcentos.release-set-files.v1"
STAGE_SCHEMA = "dcentos.release-set-stage.v1"
RESULT_SCHEMA = "dcentos.release-set-publication-result.v1"
DESCRIPTOR_NAME = ".dcent-release-set.json"
SEAL_PENDING_NAME = ".dcent-release-set-sealed-descriptor.pending"
DESTROYING_PREFIX = ".dcent-release-set-destroying-"
DELETING_PREFIX = ".dcent-release-set-deleting-"
DELETE_MARKER_PREFIX = ".dcent-release-set-delete-marker-"
LIFECYCLE_MARKER_SCHEMA = "dcentos.release-set-lifecycle-marker.v1"
MAX_JSON_BYTES = 1024 * 1024
MAX_FILES = 4096


class ReleaseSetError(RuntimeError):
    pass


class PublicationCollision(ReleaseSetError):
    """A no-replace file publication lost to an existing destination."""


class PublicationSignal(ReleaseSetError):
    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"received signal {signum} during directory publication")


class PublicationSignalGuard:
    """Turn termination into cleanup before commit and defer it after commit."""

    def __init__(self) -> None:
        self.committed = False
        self.pending: int | None = None
        self.previous: dict[int, object] = {}

    def _handler(self, signum: int, _frame: object) -> None:
        # Never unwind between the native rename returning and Python recording
        # that commit. The primitive is short; defer termination until its
        # identity verification and durability boundary is complete.
        self.pending = signum

    def __enter__(self) -> PublicationSignalGuard:
        for signum in (signal.SIGINT, signal.SIGTERM):
            try:
                self.previous[signum] = signal.getsignal(signum)
                signal.signal(signum, self._handler)
            except (AttributeError, OSError, ValueError):
                self.previous.pop(signum, None)
        return self

    def mark_committed(self) -> None:
        self.committed = True

    def refuse_pending_before_commit(self) -> None:
        if self.pending is not None:
            raise PublicationSignal(self.pending)

    def __exit__(self, kind: object, _value: object, _traceback: object) -> None:
        for signum, handler in self.previous.items():
            try:
                signal.signal(signum, handler)
            except (OSError, ValueError):
                pass
        if self.committed and self.pending is not None:
            warn_after_commit(
                f"WARNING: ignored signal {self.pending} after durable release-set commit"
            )
        elif kind is None and self.pending is not None:
            raise PublicationSignal(self.pending)


class CapabilityDeliverySignalGuard:
    """Defer termination until a new stage is rolled back or its key is delivered."""

    def __init__(self) -> None:
        self.delivered = False
        self.pending: int | None = None
        self.previous: dict[int, object] = {}

    def _handler(self, signum: int, _frame: object) -> None:
        # Python may dispatch a signal immediately after mkdir/write returns but
        # before the following assignment records the side effect. Deferral makes
        # those state transitions explicit and therefore recoverable.
        self.pending = signum

    def __enter__(self) -> CapabilityDeliverySignalGuard:
        for signum in (signal.SIGINT, signal.SIGTERM):
            try:
                self.previous[signum] = signal.getsignal(signum)
                signal.signal(signum, self._handler)
            except (AttributeError, OSError, ValueError):
                self.previous.pop(signum, None)
        return self

    def refuse_pending(self) -> None:
        if self.pending is not None:
            raise PublicationSignal(self.pending)

    def mark_delivered(self) -> None:
        self.delivered = True

    def __exit__(self, kind: object, _value: object, _traceback: object) -> None:
        for signum, handler in self.previous.items():
            try:
                signal.signal(signum, handler)
            except (OSError, ValueError):
                pass
        if self.delivered and self.pending is not None:
            warn_after_commit(
                f"WARNING: ignored signal {self.pending} after stage capability delivery"
            )
        elif kind is None and self.pending is not None:
            raise PublicationSignal(self.pending)


def fail(message: str) -> NoReturn:
    raise ReleaseSetError(message)


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def is_reparse(metadata: os.stat_result) -> bool:
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(metadata, "st_file_attributes", 0) & marker)


def absolute(value: str | os.PathLike[str]) -> Path:
    return Path(os.path.abspath(os.fspath(value)))


def inspect_existing_components(path: Path, label: str) -> Path:
    path = absolute(path)
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except OSError as error:
            fail(f"cannot inspect {label} path {current}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata):
            fail(f"{label} path contains a symlink or reparse point: {current}")
    return path


def safe_directory(
    value: str | os.PathLike[str], label: str
) -> tuple[Path, os.stat_result]:
    path = inspect_existing_components(Path(value), label)
    metadata = os.lstat(path)
    if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
        fail(f"{label} must be a non-reparse directory: {path}")
    return path, metadata


def require_windows_acl(
    path: Path,
    label: str,
    *,
    public_read: bool,
    require_protected: bool,
    directory: bool,
) -> None:
    """Require a canonical private or public-read Windows release DACL."""

    if os.name != "nt":
        return
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class Acl(ctypes.Structure):
        _fields_ = (
            ("revision", ctypes.c_ubyte),
            ("sbz1", ctypes.c_ubyte),
            ("size", ctypes.c_ushort),
            ("ace_count", ctypes.c_ushort),
            ("sbz2", ctypes.c_ushort),
        )

    class AceHeader(ctypes.Structure):
        _fields_ = (
            ("ace_type", ctypes.c_ubyte),
            ("ace_flags", ctypes.c_ubyte),
            ("ace_size", ctypes.c_ushort),
        )

    get_named_security = advapi32.GetNamedSecurityInfoW
    get_named_security.argtypes = (
        ctypes.c_wchar_p,
        ctypes.c_int,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
        ctypes.POINTER(ctypes.c_void_p),
    )
    get_named_security.restype = ctypes.c_uint32
    get_security_control = advapi32.GetSecurityDescriptorControl
    get_security_control.argtypes = (
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_ushort),
        ctypes.POINTER(ctypes.c_uint32),
    )
    get_security_control.restype = ctypes.c_int
    get_ace = advapi32.GetAce
    get_ace.argtypes = (
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
    )
    get_ace.restype = ctypes.c_int
    sid_to_string = advapi32.ConvertSidToStringSidW
    sid_to_string.argtypes = (
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_wchar_p),
    )
    sid_to_string.restype = ctypes.c_int
    local_free = kernel32.LocalFree
    local_free.argtypes = (ctypes.c_void_p,)
    local_free.restype = ctypes.c_void_p
    get_current_process = kernel32.GetCurrentProcess
    get_current_process.argtypes = ()
    get_current_process.restype = ctypes.c_void_p
    open_process_token = advapi32.OpenProcessToken
    open_process_token.argtypes = (
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_void_p),
    )
    open_process_token.restype = ctypes.c_int
    get_token_information = advapi32.GetTokenInformation
    get_token_information.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_uint32),
    )
    get_token_information.restype = ctypes.c_int
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int

    def stringify_sid(pointer: int) -> str:
        value = ctypes.c_wchar_p()
        if not sid_to_string(ctypes.c_void_p(pointer), ctypes.byref(value)):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            return value.value or ""
        finally:
            local_free(ctypes.cast(value, ctypes.c_void_p))

    def current_user_sid() -> str:
        token = ctypes.c_void_p()
        if not open_process_token(get_current_process(), 0x0008, ctypes.byref(token)):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            required = ctypes.c_uint32()
            get_token_information(token, 1, None, 0, ctypes.byref(required))
            if not required.value:
                raise ctypes.WinError(ctypes.get_last_error())
            buffer = ctypes.create_string_buffer(required.value)
            if not get_token_information(
                token,
                1,  # TokenUser
                buffer,
                len(buffer),
                ctypes.byref(required),
            ):
                raise ctypes.WinError(ctypes.get_last_error())
            sid_pointer = ctypes.c_void_p.from_buffer(buffer).value
            if not sid_pointer:
                fail("current process token has no user SID")
            return stringify_sid(sid_pointer)
        finally:
            close_handle(token)

    owner = ctypes.c_void_p()
    dacl = ctypes.c_void_p()
    security_descriptor = ctypes.c_void_p()
    result = get_named_security(
        str(path),
        1,  # SE_FILE_OBJECT
        0x00000001 | 0x00000004,  # OWNER + DACL_SECURITY_INFORMATION
        ctypes.byref(owner),
        None,
        ctypes.byref(dacl),
        None,
        ctypes.byref(security_descriptor),
    )
    if result != 0:
        raise ctypes.WinError(result)
    try:
        if not owner.value or not dacl.value:
            fail(f"{label} must have an owner and a non-null private DACL: {path}")
        control = ctypes.c_ushort()
        revision = ctypes.c_uint32()
        if not get_security_control(
            security_descriptor,
            ctypes.byref(control),
            ctypes.byref(revision),
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        if require_protected and not control.value & 0x1000:  # SE_DACL_PROTECTED
            fail(f"{label} DACL must be protected from future inheritance: {path}")
        owner_sid = stringify_sid(owner.value)
        process_sid = current_user_sid()
        if owner_sid != process_sid:
            fail(
                f"{label} owner {owner_sid} is not current process user "
                f"{process_sid}: {path}"
            )
        trusted = {
            owner_sid,
            "S-1-3-0",  # CREATOR OWNER
            "S-1-3-4",  # OWNER RIGHTS
            "S-1-5-18",  # LOCAL SYSTEM
            "S-1-5-32-544",  # BUILTIN\\Administrators
        }
        everyone_read_aces = 0
        acl = Acl.from_address(dacl.value)
        for index in range(acl.ace_count):
            ace_pointer = ctypes.c_void_p()
            if not get_ace(dacl, index, ctypes.byref(ace_pointer)):
                raise ctypes.WinError(ctypes.get_last_error())
            header = AceHeader.from_address(ace_pointer.value)
            if header.ace_type == 0:  # ACCESS_ALLOWED_ACE_TYPE
                mask = ctypes.c_uint32.from_address(ace_pointer.value + 4).value
                sid = stringify_sid(ace_pointer.value + 8)
                if sid == "S-1-1-0" and public_read:
                    expected_mask = 0x001200A9 if directory else 0x00120089
                    expected_flags = 0x03 if directory else 0x00
                    if mask != expected_mask or header.ace_flags != expected_flags:
                        fail(
                            f"{label} DACL gives Everyone noncanonical access "
                            f"mask=0x{mask:08x},flags=0x{header.ace_flags:02x}: {path}"
                        )
                    everyone_read_aces += 1
                elif mask and sid not in trusted:
                    fail(
                        f"{label} DACL grants access to untrusted SID {sid}: {path}"
                    )
            elif public_read and header.ace_type in {1, 6, 10, 12}:
                fail(f"{label} public DACL contains a deny ACE: {path}")
            elif header.ace_type in {4, 5, 9, 11}:  # compound/object/callback allow
                fail(f"{label} DACL contains an unsupported allow ACE: {path}")
        if public_read and everyone_read_aces != 1:
            fail(f"{label} DACL must grant one canonical Everyone read ACE: {path}")
    finally:
        local_free(security_descriptor)


def require_private_windows_acl(
    path: Path, label: str, *, require_protected: bool = True
) -> None:
    require_windows_acl(
        path,
        label,
        public_read=False,
        require_protected=require_protected,
        directory=path.is_dir(),
    )


def require_public_windows_acl(
    path: Path,
    label: str,
    *,
    directory: bool = False,
    require_protected: bool = True,
) -> None:
    require_windows_acl(
        path,
        label,
        public_read=True,
        require_protected=require_protected,
        directory=directory,
    )


def require_private_directory(metadata: os.stat_result, path: Path, label: str) -> None:
    if os.name == "nt":
        require_private_windows_acl(path, label)
        return
    if os.name != "posix":
        return
    mode = stat.S_IMODE(metadata.st_mode)
    if mode != 0o700:
        fail(f"{label} must use owner-only mode 0700: {path} has {mode:04o}")


def validate_flat_name(
    name: object, label: str, *, allow_descriptor: bool = False
) -> str:
    if not isinstance(name, str) or not name:
        fail(f"{label} must be a non-empty string")
    if name in (".", "..") or name != Path(name).name or "/" in name or "\\" in name:
        fail(f"{label} must be a canonical flat name")
    if name == DESCRIPTOR_NAME and not allow_descriptor:
        fail(f"{label} collides with reserved release-set metadata")
    if unicodedata.normalize("NFC", name) != name:
        fail(f"{label} must use NFC Unicode normalization")
    if len(name.encode("utf-8")) > 200:
        fail(f"{label} exceeds the 200-byte portability bound")
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in name):
        fail(f"{label} contains a control character")
    if any(character in '<>:"|?*' for character in name) or name[-1] in (" ", "."):
        fail(f"{label} is not portable across release hosts")
    return name


def validate_hex(value: object, label: str, length: int) -> str:
    if (
        not isinstance(value, str)
        or len(value) != length
        or any(character not in "0123456789abcdef" for character in value)
    ):
        fail(f"{label} must be {length} lowercase hexadecimal characters")
    return value


def validate_file_entries(files: object, label: str) -> list[dict[str, object]]:
    if not isinstance(files, list) or not files or len(files) > MAX_FILES:
        fail(f"{label} must contain 1..4096 files")
    parsed: list[dict[str, object]] = []
    portable_names: set[str] = set()
    prior = ""
    for index, entry in enumerate(files):
        if not isinstance(entry, dict) or set(entry) != {"name", "sha256", "size"}:
            fail(f"{label} file {index} has an invalid schema")
        name = validate_flat_name(entry["name"], f"{label} file {index} name")
        if name == SEAL_PENDING_NAME:
            fail(f"{label} file name is reserved for seal recovery: {name}")
        folded = name.casefold()
        if folded in portable_names:
            fail(f"{label} file names collide across case-insensitive hosts: {name}")
        portable_names.add(folded)
        if prior and name <= prior:
            fail(f"{label} files must be strictly sorted by name")
        prior = name
        digest = validate_hex(entry["sha256"], f"{label} file {name} digest", 64)
        size = entry["size"]
        if isinstance(size, bool) or not isinstance(size, int) or size < 0:
            fail(f"{label} file {name} size is invalid")
        parsed.append({"name": name, "sha256": digest, "size": size})
    return parsed


def read_regular_json(
    path: Path,
    label: str,
    *,
    final_mode: int | None = None,
    require_flush: bool = False,
    directory_fd: int | None = None,
) -> tuple[dict[str, object], bytes]:
    if directory_fd is None:
        path = inspect_existing_components(path, label)
        initial = os.lstat(path)
    else:
        initial = os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
    ):
        fail(f"{label} must be a single-link non-reparse regular file")
    # O_NONBLOCK is inert for regular files and prevents an attacker from
    # wedging the verifier if the leaf is swapped to a FIFO between lstat and
    # open. fstat below remains the authority for the opened object type.
    flags = (
        (
            os.O_RDWR
            if os.name == "nt" and (require_flush or final_mode is not None)
            else os.O_RDONLY
        )
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(
            path.name if directory_fd is not None else path,
            flags,
            dir_fd=directory_fd,
        )
    except OSError as error:
        fail(f"cannot open {label}: {error}")
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or is_reparse(before)
            or getattr(before, "st_nlink", 1) != 1
        ):
            fail(f"{label} must be a single-link non-reparse regular file")
        if (before.st_dev, before.st_ino) != (initial.st_dev, initial.st_ino):
            fail(f"{label} changed before it could be opened")
        if before.st_size > MAX_JSON_BYTES:
            fail(f"{label} exceeds the one-MiB bound")
        raw = b""
        while len(raw) <= MAX_JSON_BYTES:
            chunk = os.read(descriptor, min(64 * 1024, MAX_JSON_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor)
        current = (
            os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
            if directory_fd is not None
            else os.lstat(path)
        )
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ) or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
            fail(f"{label} changed or was replaced while being read")
        if final_mode is not None and os.name == "posix":
            os.fchmod(descriptor, final_mode)
        if require_flush or final_mode is not None:
            os.fsync(descriptor)
        final = os.fstat(descriptor)
        current = (
            os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
            if directory_fd is not None
            else os.lstat(path)
        )
        if (
            final.st_dev,
            final.st_ino,
            final.st_size,
            final.st_mtime_ns,
        ) != (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) or (final.st_dev, final.st_ino) != (current.st_dev, current.st_ino):
            fail(f"{label} changed while its final mode was prepared")
        if final_mode is not None and os.name == "posix" and stat.S_IMODE(
            final.st_mode
        ) != final_mode:
            fail(f"{label} mode could not be normalized to {final_mode:04o}")
    finally:
        os.close(descriptor)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object")
    return value, raw


def parse_capability(path: Path) -> dict[str, object]:
    value, _ = read_regular_json(path, "capability descriptor")
    required = {
        "schema",
        "stage_id",
        "capability",
        "stage_parent",
        "stage_path",
        "stage_dev",
        "stage_ino",
    }
    if set(value) != required or value.get("schema") != CAPABILITY_SCHEMA:
        fail("capability descriptor has an invalid schema")
    stage_id = validate_hex(value["stage_id"], "stage ID", 32)
    validate_hex(value["capability"], "capability", 64)
    for field in ("stage_parent", "stage_path"):
        if (
            not isinstance(value[field], str)
            or not value[field]
            or value[field] != str(absolute(value[field]))
        ):
            fail(f"capability {field} must be an absolute canonical path")
    for field in ("stage_dev", "stage_ino"):
        if (
            isinstance(value[field], bool)
            or not isinstance(value[field], int)
            or value[field] < 0
        ):
            fail(f"capability {field} is invalid")
    parent = Path(value["stage_parent"])
    stage = Path(value["stage_path"])
    if stage.parent != parent or stage.name != f".dcent-release-set-stage-{stage_id}":
        fail("capability stage path is not its declared direct randomized child")
    return value


def validate_owned_stage(
    capability: dict[str, object],
) -> tuple[Path, Path, os.stat_result, os.stat_result]:
    parent, parent_metadata = safe_directory(
        str(capability["stage_parent"]), "stage parent"
    )
    stage = inspect_existing_components(Path(str(capability["stage_path"])), "stage")
    metadata = os.lstat(stage)
    if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
        fail("owned stage must be a non-reparse directory")
    if (metadata.st_dev, metadata.st_ino) != (
        capability["stage_dev"],
        capability["stage_ino"],
    ):
        fail("owned stage identity does not match the capability")
    return parent, stage, metadata, parent_metadata


def capability_hash(capability: dict[str, object]) -> str:
    return hashlib.sha256(str(capability["capability"]).encode("ascii")).hexdigest()


def parse_files_manifest(path: Path) -> list[dict[str, object]]:
    value, _ = read_regular_json(path, "release-set files manifest")
    if set(value) != {"schema", "files"} or value.get("schema") != FILES_SCHEMA:
        fail("release-set files manifest has an invalid schema")
    return validate_file_entries(value["files"], "release-set manifest")


def parse_stage_descriptor(
    stage: Path,
    capability: dict[str, object],
    state: str,
    *,
    directory_fd: int | None = None,
) -> dict[str, object]:
    value, raw = read_regular_json(
        stage / DESCRIPTOR_NAME,
        "stage descriptor",
        directory_fd=directory_fd,
    )
    common = {"schema", "state", "stage_id", "capability_sha256"}
    expected = common if state == "building" else common | {"output_name", "files"}
    if (
        set(value) != expected
        or value.get("schema") != STAGE_SCHEMA
        or value.get("state") != state
    ):
        fail(f"stage descriptor is not in exact {state} state")
    if value.get("stage_id") != capability["stage_id"]:
        fail("stage descriptor ID does not match the capability")
    if value.get("capability_sha256") != capability_hash(capability):
        fail("stage descriptor is not owned by the supplied capability")
    if raw != canonical_json(value):
        fail("stage descriptor is not canonically encoded")
    if state == "sealed":
        validate_flat_name(value["output_name"], "release-set output name")
        validate_file_entries(value["files"], "sealed stage")
    return value


def measure_stage_file(
    path: Path,
    *,
    final_mode: int | None = None,
    require_flush: bool = False,
    directory_fd: int | None = None,
) -> dict[str, object]:
    initial = (
        os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
        if directory_fd is not None
        else os.lstat(path)
    )
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
    ):
        fail(f"staged file must be a single-link non-reparse regular file: {path.name}")
    # See read_regular_json: never block on a raced FIFO before fstat can
    # reject the opened object.
    flags = (
        (
            os.O_RDWR
            if os.name == "nt" and (require_flush or final_mode is not None)
            else os.O_RDONLY
        )
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(
            path.name if directory_fd is not None else path,
            flags,
            dir_fd=directory_fd,
        )
    except OSError as error:
        fail(f"cannot open staged file {path.name}: {error}")
    digest = hashlib.sha256()
    observed_size = 0
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or is_reparse(before)
            or getattr(before, "st_nlink", 1) != 1
        ):
            fail(
                f"staged file must be a single-link non-reparse regular file: {path.name}"
            )
        if (before.st_dev, before.st_ino) != (initial.st_dev, initial.st_ino):
            fail(f"staged file changed before it could be opened: {path.name}")
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            observed_size += len(chunk)
        after = os.fstat(descriptor)
        current = (
            os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
            if directory_fd is not None
            else os.lstat(path)
        )
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ) or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
            fail(
                f"staged file changed or was replaced while being verified: {path.name}"
            )
        if final_mode is not None and os.name == "posix":
            os.fchmod(descriptor, final_mode)
        if require_flush or final_mode is not None:
            os.fsync(descriptor)
        final = os.fstat(descriptor)
        current = (
            os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
            if directory_fd is not None
            else os.lstat(path)
        )
        if (
            final.st_dev,
            final.st_ino,
            final.st_size,
            final.st_mtime_ns,
        ) != (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) or (final.st_dev, final.st_ino) != (current.st_dev, current.st_ino):
            fail(f"staged file changed while its final mode was prepared: {path.name}")
        if final_mode is not None and os.name == "posix" and stat.S_IMODE(
            final.st_mode
        ) != final_mode:
            fail(f"staged file mode could not be normalized: {path.name}")
    finally:
        os.close(descriptor)
    return {"name": path.name, "sha256": digest.hexdigest(), "size": observed_size}


def hash_stage_file(
    path: Path,
    expected: dict[str, object],
    *,
    final_mode: int | None = None,
    require_flush: bool = False,
    directory_fd: int | None = None,
) -> None:
    observed = measure_stage_file(
        path,
        final_mode=final_mode,
        require_flush=require_flush,
        directory_fd=directory_fd,
    )
    if observed["size"] != expected["size"] or observed["sha256"] != expected["sha256"]:
        fail(f"staged file does not match its declared size and SHA-256: {path.name}")


def validate_new_output(value: str, label: str) -> tuple[Path, Path]:
    output = absolute(value)
    validate_flat_name(output.name, f"{label} name", allow_descriptor=True)
    parent, _ = safe_directory(output.parent, f"{label} parent")
    try:
        os.lstat(output)
    except FileNotFoundError:
        pass
    except OSError as error:
        fail(f"cannot inspect {label} leaf: {error}")
    else:
        fail(f"{label} already exists: {output}")
    return output, parent


def publish_regular_file_noreplace(
    output: Path,
    content: bytes,
    *,
    mode: int = 0o644,
    before_commit: Callable[[], None] | None = None,
) -> None:
    temporary_fd, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.publication-pending.", dir=output.parent
    )
    temporary = Path(temporary_name)
    temporary_stat = os.fstat(temporary_fd)
    temporary_identity = (temporary_stat.st_dev, temporary_stat.st_ino)
    owned_temporary_fd = temporary_fd
    committed = False
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(owned_temporary_fd, mode)
        else:
            os.chmod(temporary, mode)
        if os.name == "nt":
            if mode == 0o600:
                set_windows_file_acl(temporary, WINDOWS_PRIVATE_FILE_SDDL)
            elif mode == 0o644:
                set_windows_file_acl(temporary, WINDOWS_PUBLIC_FILE_SDDL)
            else:
                fail(f"Windows publication mode is unsupported: {mode:04o}")
        destination_file = os.fdopen(owned_temporary_fd, "wb", closefd=True)
        owned_temporary_fd = -1
        with destination_file as destination:
            destination.write(content)
            destination.flush()
            os.fsync(destination.fileno())
        prepared = temporary.lstat()
        if (
            not stat.S_ISREG(prepared.st_mode)
            or is_reparse(prepared)
            or getattr(prepared, "st_nlink", 1) != 1
            or (prepared.st_dev, prepared.st_ino) != temporary_identity
        ):
            fail("prepared publication file identity or type changed")
        if os.name == "posix" and stat.S_IMODE(prepared.st_mode) != mode:
            fail(f"prepared publication file mode is not canonical {mode:04o}")
        if os.name == "nt":
            if mode == 0o600:
                require_private_windows_acl(temporary, "prepared private publication")
            else:
                require_public_windows_acl(temporary, "prepared public publication")
        try:
            publish_staged_file(
                temporary,
                output,
                require_directory_sync=True,
                require_staged_cleanup=True,
                expected_staged_identity=temporary_identity,
                _after_staged_open=before_commit,
            )
            committed = True
        except DestinationExistsError as error:
            raise PublicationCollision(
                f"release publication output already exists: {error}"
            ) from error
        except PublishError as error:
            fail(f"cannot durably publish manifest without replacement: {error}")
    finally:
        if owned_temporary_fd >= 0:
            try:
                os.close(owned_temporary_fd)
            except OSError:
                pass
        if not committed:
            try:
                current = temporary.lstat()
                if (current.st_dev, current.st_ino) == temporary_identity:
                    temporary.unlink()
            except OSError:
                pass


def inspect_stage_entries(
    stage: Path,
    *,
    expected_file_mode: int | None = None,
    directory_fd: int | None = None,
) -> set[str]:
    observed_names: set[str] = set()
    portable_names: set[str] = set()
    with os.scandir(directory_fd if directory_fd is not None else stage) as entries:
        for entry in entries:
            name = validate_flat_name(
                entry.name, "staged entry name", allow_descriptor=True
            )
            if name.casefold() in portable_names:
                fail("staged entries collide across case-insensitive hosts")
            portable_names.add(name.casefold())
            # Windows DirEntry.stat() reports st_nlink=0 on some supported
            # Python versions. lstat() provides the authoritative link count.
            metadata = (
                os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
                if directory_fd is not None
                else os.lstat(stage / name)
            )
            if (
                not stat.S_ISREG(metadata.st_mode)
                or is_reparse(metadata)
                or getattr(metadata, "st_nlink", 1) != 1
            ):
                fail(
                    f"staged entry is not a single-link non-reparse regular file: {name}"
                )
            if (
                expected_file_mode is not None
                and os.name == "posix"
                and stat.S_IMODE(metadata.st_mode) != expected_file_mode
            ):
                fail(
                    f"staged entry does not use canonical mode "
                    f"{expected_file_mode:04o}: {name}"
                )
            observed_names.add(name)
    return observed_names


def verify_exact_stage(
    stage: Path,
    descriptor: dict[str, object],
    *,
    prepare_publication: bool = False,
    require_public_modes: bool = False,
    require_durable: bool = False,
    directory_fd: int | None = None,
) -> None:
    if os.name == "nt":
        if prepare_publication:
            require_private_windows_acl(stage, "release stage")
        elif require_public_modes:
            require_public_windows_acl(
                stage, "published release stage", directory=True
            )
    expected_names = {DESCRIPTOR_NAME} | {
        str(entry["name"]) for entry in descriptor["files"]
    }
    observed_names = inspect_stage_entries(stage, directory_fd=directory_fd)
    missing = sorted(expected_names - observed_names)
    extra = sorted(observed_names - expected_names)
    if missing or extra:
        fail(f"release stage is not exact (missing={missing}, extra={extra})")
    for entry in descriptor["files"]:
        entry_path = stage / str(entry["name"])
        if os.name == "nt":
            if prepare_publication:
                require_private_windows_acl(
                    entry_path,
                    f"staged file {entry['name']}",
                    require_protected=False,
                )
            elif require_public_modes:
                require_public_windows_acl(
                    entry_path,
                    f"published file {entry['name']}",
                )
        hash_stage_file(
            entry_path,
            entry,
            final_mode=0o644 if prepare_publication else None,
            require_flush=prepare_publication or require_durable,
            directory_fd=directory_fd,
        )
    # Flush metadata bytes as well as payload bytes before directory publication.
    descriptor_path = stage / DESCRIPTOR_NAME
    if os.name == "nt":
        if prepare_publication:
            require_private_windows_acl(
                descriptor_path,
                "stage descriptor",
                require_protected=False,
            )
        elif require_public_modes:
            require_public_windows_acl(
                descriptor_path,
                "published stage descriptor",
            )
    stage_value, _ = read_regular_json(
        descriptor_path,
        "stage descriptor",
        final_mode=0o644 if prepare_publication else None,
        require_flush=prepare_publication or require_durable,
        directory_fd=directory_fd,
    )
    if stage_value != descriptor:
        fail("stage descriptor changed during exact-set verification")
    final_names = inspect_stage_entries(
        stage,
        expected_file_mode=0o644
        if prepare_publication or require_public_modes
        else None,
        directory_fd=directory_fd,
    )
    if final_names != expected_names:
        fail("release stage changed during exact-set verification")


def transition_windows_release_access(
    stage: Path, descriptor: dict[str, object], public: bool
) -> None:
    """Set and verify the exact flat release tree's canonical Windows DACLs."""

    if os.name != "nt":
        return
    sddl = WINDOWS_PUBLIC_FILE_SDDL if public else WINDOWS_PRIVATE_FILE_SDDL
    for name in [
        *(str(entry["name"]) for entry in descriptor["files"]),
        DESCRIPTOR_NAME,
    ]:
        set_windows_file_acl(stage / name, sddl)
    if public:
        require_public_windows_acl(stage, "release stage", directory=True)
        for name in [
            *(str(entry["name"]) for entry in descriptor["files"]),
            DESCRIPTOR_NAME,
        ]:
            require_public_windows_acl(
                stage / name,
                f"release file {name}",
            )
    else:
        require_private_windows_acl(stage, "release stage")
        for name in [
            *(str(entry["name"]) for entry in descriptor["files"]),
            DESCRIPTOR_NAME,
        ]:
            require_private_windows_acl(stage / name, f"release file {name}")


def rollback_new_stage(
    parent: Path,
    expected_parent_identity: tuple[int, int],
    stage: Path,
    stage_identity: tuple[int, int] | None,
    descriptor_identity: tuple[int, int] | None,
) -> None:
    """Remove only the exact, otherwise-empty stage created by this invocation."""

    if stage_identity is None:
        fail(
            "new stage identity was never established; retaining ambiguous path "
            "instead of deleting a possible replacement"
        )
    parent_metadata = os.lstat(parent)
    if (
        not stat.S_ISDIR(parent_metadata.st_mode)
        or is_reparse(parent_metadata)
        or (parent_metadata.st_dev, parent_metadata.st_ino)
        != expected_parent_identity
    ):
        fail("stage parent identity changed before creation rollback")
    current = stage.lstat()
    if (
        not stat.S_ISDIR(current.st_mode)
        or is_reparse(current)
        or (
            (current.st_dev, current.st_ino) != stage_identity
        )
    ):
        fail("new stage identity changed before creation rollback")
    names = {entry.name for entry in os.scandir(stage)}
    allowed = {DESCRIPTOR_NAME} if descriptor_identity is not None else set()
    if not names <= allowed:
        fail("new stage gained unexpected entries before creation rollback")
    if DESCRIPTOR_NAME in names:
        descriptor_path = stage / DESCRIPTOR_NAME
        descriptor_metadata = descriptor_path.lstat()
        if (
            not stat.S_ISREG(descriptor_metadata.st_mode)
            or is_reparse(descriptor_metadata)
            or getattr(descriptor_metadata, "st_nlink", 1) != 1
            or (descriptor_metadata.st_dev, descriptor_metadata.st_ino)
            != descriptor_identity
        ):
            fail("new stage descriptor became unsafe before creation rollback")
        descriptor_path.unlink()
    stage.rmdir()
    fsync_directory(parent)


def create_stage(args: argparse.Namespace) -> None:
    parent, parent_metadata = safe_directory(args.parent, "stage parent")
    require_private_directory(parent_metadata, parent, "stage parent")
    capability_output_value = getattr(args, "capability_output", None)
    capability_output: Path | None = None
    if capability_output_value is not None:
        capability_output, _ = validate_new_output(
            capability_output_value, "capability output"
        )
    parent_identity = parent_metadata.st_dev, parent_metadata.st_ino
    with CapabilityDeliverySignalGuard() as signal_guard:
        for _ in range(32):
            signal_guard.refuse_pending()
            stage_id = secrets.token_hex(16)
            capability_secret = secrets.token_hex(32)
            stage = parent / f".dcent-release-set-stage-{stage_id}"
            stage_created = False
            stage_identity: tuple[int, int] | None = None
            descriptor_identity: tuple[int, int] | None = None
            descriptor_path = stage / DESCRIPTOR_NAME
            parent_handle = -1
            stage_handle = -1
            descriptor_handle = -1
            close_parent: Callable[[], None] | None = None
            close_stage: Callable[[], None] | None = None
            close_descriptor: Callable[[], None] | None = None
            parent_fd = -1
            stage_fd = -1
            try:
                try:
                    os.mkdir(stage, 0o700)
                    stage_created = True
                except FileExistsError:
                    continue
                metadata = os.lstat(stage)
                if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
                    fail("new stage is not a non-reparse directory")
                stage_identity = metadata.st_dev, metadata.st_ino
                capability = {
                    "schema": CAPABILITY_SCHEMA,
                    "stage_id": stage_id,
                    "capability": capability_secret,
                    "stage_parent": str(parent),
                    "stage_path": str(stage),
                    "stage_dev": metadata.st_dev,
                    "stage_ino": metadata.st_ino,
                }
                if os.name == "nt":
                    parent_handle, close_parent = open_pinned_windows_directory(
                        parent, parent_identity, "new stage parent"
                    )
                    stage_handle, close_stage = open_pinned_windows_directory(
                        stage,
                        stage_identity,
                        "new stage",
                        movable=True,
                    )
                    set_windows_directory_acl(stage, WINDOWS_PRIVATE_DIRECTORY_SDDL)
                else:
                    os.chmod(stage, 0o700, follow_symlinks=False)
                    parent_fd, stage_fd = open_pinned_posix_stage(
                        parent,
                        stage,
                        parent_identity,
                        stage_identity,
                    )
                    os.fchmod(stage_fd, 0o700)
                    metadata = os.fstat(stage_fd)
                require_private_directory(metadata, stage, "new stage")
                signal_guard.refuse_pending()
                descriptor = {
                    "schema": STAGE_SCHEMA,
                    "state": "building",
                    "stage_id": stage_id,
                    "capability_sha256": capability_hash(capability),
                }
                flags = (
                    os.O_WRONLY
                    | os.O_CREAT
                    | os.O_EXCL
                    | getattr(os, "O_BINARY", 0)
                )
                fd = os.open(
                    DESCRIPTOR_NAME if stage_fd >= 0 else descriptor_path,
                    flags,
                    0o600,
                    dir_fd=stage_fd if stage_fd >= 0 else None,
                )
                try:
                    if hasattr(os, "fchmod"):
                        os.fchmod(fd, 0o600)
                    descriptor_metadata = os.fstat(fd)
                    descriptor_identity = (
                        descriptor_metadata.st_dev,
                        descriptor_metadata.st_ino,
                    )
                    descriptor_raw = canonical_json(descriptor)
                    written = 0
                    while written < len(descriptor_raw):
                        count = os.write(fd, descriptor_raw[written:])
                        if count <= 0:
                            raise OSError(errno.EIO, "zero-length descriptor write")
                        written += count
                    os.fsync(fd)
                finally:
                    os.close(fd)
                if os.name == "nt":
                    set_windows_file_acl(
                        descriptor_path, WINDOWS_PRIVATE_FILE_SDDL
                    )
                    assert descriptor_identity is not None
                    descriptor_handle, close_descriptor = open_pinned_windows_file(
                        descriptor_path,
                        descriptor_identity,
                        "new stage descriptor",
                    )
                signal_guard.refuse_pending()
                if stage_fd >= 0:
                    os.fsync(stage_fd)
                else:
                    fsync_directory(stage)
                signal_guard.refuse_pending()
                if parent_fd >= 0:
                    os.fsync(parent_fd)
                else:
                    fsync_directory(parent)
                signal_guard.refuse_pending()
                parent_now = os.fstat(parent_fd) if parent_fd >= 0 else os.lstat(parent)
                if (parent_now.st_dev, parent_now.st_ino) != parent_identity:
                    fail("stage parent identity changed before capability delivery")
                stage_now = (
                    os.stat(stage.name, dir_fd=parent_fd, follow_symlinks=False)
                    if parent_fd >= 0
                    else os.lstat(stage)
                )
                if (stage_now.st_dev, stage_now.st_ino) != stage_identity:
                    fail("new stage identity changed before capability delivery")
                descriptor_now = (
                    os.stat(
                        DESCRIPTOR_NAME,
                        dir_fd=stage_fd,
                        follow_symlinks=False,
                    )
                    if stage_fd >= 0
                    else os.lstat(descriptor_path)
                )
                if (
                    descriptor_identity is None
                    or (descriptor_now.st_dev, descriptor_now.st_ino)
                    != descriptor_identity
                ):
                    fail("new stage descriptor changed before capability delivery")
                capability_raw = canonical_json(capability)
                if capability_output is not None:
                    publish_regular_file_noreplace(
                        capability_output,
                        capability_raw,
                        mode=0o600,
                    )
                    # Atomic publication is the delivery boundary. Every mode
                    # and ACL check was completed on the staged inode first,
                    # so no later validation can revoke an exposed capability.
                    signal_guard.mark_delivered()
                else:
                    sys.stdout.buffer.write(capability_raw)
                    sys.stdout.buffer.flush()
                    signal_guard.mark_delivered()
                if capability_output is not None:
                    report_after_commit(
                        (canonical_json(capability).decode("utf-8").removesuffix("\n"),)
                    )
            except BaseException as create_error:
                if signal_guard.delivered:
                    if close_descriptor is not None:
                        close_descriptor()
                        close_descriptor = None
                    if close_stage is not None:
                        close_stage()
                        close_stage = None
                    if close_parent is not None:
                        close_parent()
                        close_parent = None
                    if stage_fd >= 0:
                        os.close(stage_fd)
                        stage_fd = -1
                    if parent_fd >= 0:
                        os.close(parent_fd)
                        parent_fd = -1
                    warn_after_commit(
                        "WARNING: ignored post-delivery stage reporting failure: "
                        f"{create_error}"
                    )
                    return
                cleanup_error: BaseException | None = None
                if stage_created:
                    try:
                        if os.name == "nt" and stage_identity is not None:
                            if stage_handle < 0:
                                stage_handle, close_stage = (
                                    open_pinned_windows_directory(
                                        stage,
                                        stage_identity,
                                        "new stage rollback",
                                        movable=True,
                                    )
                                )
                            if descriptor_identity is not None and descriptor_handle < 0:
                                descriptor_handle, close_descriptor = (
                                    open_pinned_windows_file(
                                        descriptor_path,
                                        descriptor_identity,
                                        "new stage descriptor rollback",
                                    )
                                )
                            if descriptor_handle >= 0:
                                mark_windows_handle_delete(descriptor_handle)
                                assert close_descriptor is not None
                                close_descriptor()
                                close_descriptor = None
                                descriptor_handle = -1
                                if os.path.lexists(descriptor_path):
                                    fail(
                                        "new pinned descriptor remained after rollback"
                                    )
                            names = {entry.name for entry in os.scandir(stage)}
                            if names:
                                fail(
                                    "new pinned stage retained entries before rollback"
                                )
                            mark_windows_handle_delete(stage_handle)
                            assert close_stage is not None
                            close_stage()
                            close_stage = None
                            stage_handle = -1
                            if os.path.lexists(stage):
                                fail("new pinned stage remained after rollback")
                            fsync_directory(parent)
                        elif os.name == "posix" and stage_identity is not None:
                            expected_entries = (
                                {DESCRIPTOR_NAME: descriptor_identity}
                                if descriptor_identity is not None
                                else {}
                            )
                            destroy_stage_posix(
                                parent,
                                stage,
                                parent_identity,
                                stage_identity,
                                capability,
                                expected_entries=expected_entries,
                            )
                        else:
                            rollback_new_stage(
                                parent,
                                parent_identity,
                                stage,
                                stage_identity,
                                descriptor_identity,
                            )
                    except BaseException as error:
                        cleanup_error = error
                if close_descriptor is not None:
                    close_descriptor()
                    close_descriptor = None
                if close_stage is not None:
                    close_stage()
                    close_stage = None
                if close_parent is not None:
                    close_parent()
                    close_parent = None
                if stage_fd >= 0:
                    os.close(stage_fd)
                    stage_fd = -1
                if parent_fd >= 0:
                    os.close(parent_fd)
                    parent_fd = -1
                try:
                    null_descriptor = os.open(os.devnull, os.O_WRONLY)
                    try:
                        os.dup2(null_descriptor, sys.stdout.fileno())
                    finally:
                        os.close(null_descriptor)
                except Exception:
                    pass
                if cleanup_error is not None:
                    raise ReleaseSetError(
                        "cannot create and report stage capability, and exact rollback "
                        f"was incomplete: creation={create_error}; cleanup={cleanup_error}"
                    ) from create_error
                if isinstance(create_error, (KeyboardInterrupt, SystemExit)):
                    raise
                raise ReleaseSetError(
                    f"cannot create or deliver stage capability: {create_error}"
                ) from create_error
            if close_descriptor is not None:
                close_descriptor()
            if close_stage is not None:
                close_stage()
            if close_parent is not None:
                close_parent()
            if stage_fd >= 0:
                os.close(stage_fd)
            if parent_fd >= 0:
                os.close(parent_fd)
            return
    fail("could not allocate a unique release-set stage")


def manifest_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    _, stage, _, _ = validate_owned_stage(capability)
    parse_stage_descriptor(stage, capability, "building")
    output, _ = validate_new_output(args.output, "manifest output")
    try:
        common = os.path.commonpath(
            (os.path.normcase(str(stage)), os.path.normcase(str(output)))
        )
    except ValueError:
        common = ""
    if common == os.path.normcase(str(stage)):
        fail("manifest output must never be inside the owned stage")

    initial_names = inspect_stage_entries(stage)
    if DESCRIPTOR_NAME not in initial_names:
        fail("building stage descriptor is missing")
    payload_names = sorted(initial_names - {DESCRIPTOR_NAME})
    if not payload_names or len(payload_names) > MAX_FILES:
        fail("building stage must contain 1..4096 payload files")
    files = [measure_stage_file(stage / name) for name in payload_names]
    if inspect_stage_entries(stage) != initial_names:
        fail("building stage changed while its manifest was generated")
    # The descriptor is re-read after payload hashing so capability ownership
    # cannot be swapped unnoticed during manifest generation.
    parse_stage_descriptor(stage, capability, "building")
    manifest = {"schema": FILES_SCHEMA, "files": files}
    content = canonical_json(manifest)
    if len(content) > MAX_JSON_BYTES:
        fail("generated release-set manifest exceeds the one-MiB bound")
    publish_regular_file_noreplace(output, content)
    report_after_commit((content.decode("utf-8").removesuffix("\n"),))


def open_pinned_windows_directory(
    path: Path,
    expected_identity: tuple[int, int],
    label: str,
    *,
    movable: bool = False,
) -> tuple[int, Callable[[], None]]:
    """Open a Windows directory without delete sharing and verify its file ID."""

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
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int
    handle = create_file(
        str(path),
        0x80000000 | 0x40000000 | (0x00010000 if movable else 0),
        0x00000001 | 0x00000002,  # no FILE_SHARE_DELETE
        None,
        3,  # OPEN_EXISTING
        0x02000000 | 0x00200000,
        None,
    )
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        information = HandleInformation()
        if not get_information(handle, ctypes.byref(information)):
            raise ctypes.WinError(ctypes.get_last_error())
        identity = (
            information.volume_serial,
            (information.file_index_high << 32) | information.file_index_low,
        )
        if identity != expected_identity:
            fail(f"opened {label} identity changed")
        if not information.attributes & 0x10 or information.attributes & 0x400:
            fail(f"opened {label} is not a non-reparse directory")
    except BaseException:
        close_handle(handle)
        raise

    def close() -> None:
        close_handle(handle)

    return handle, close


def open_pinned_windows_file(
    path: Path,
    expected_identity: tuple[int, int],
    label: str,
    *,
    write_attributes: bool = False,
    write_data: bool = False,
    share_write: bool = True,
    request_delete_access: bool = True,
) -> tuple[int, Callable[[], None]]:
    """Open a Windows file without delete sharing and verify its file ID."""

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
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int
    handle = create_file(
        str(path),
        0x80000000
        | (0x40000000 if write_data else 0)
        | (0x00010000 if request_delete_access else 0)
        | (0x00000100 if write_attributes else 0),
        # GENERIC_READ + optional GENERIC_WRITE / DELETE / FILE_WRITE_ATTRIBUTES
        0x00000001 | (0x00000002 if share_write else 0),
        # Always deny delete sharing; signing pins may also deny write sharing.
        None,
        3,  # OPEN_EXISTING
        0x00200000,  # FILE_FLAG_OPEN_REPARSE_POINT
        None,
    )
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        information = HandleInformation()
        if not get_information(handle, ctypes.byref(information)):
            raise ctypes.WinError(ctypes.get_last_error())
        identity = (
            information.volume_serial,
            (information.file_index_high << 32) | information.file_index_low,
        )
        if identity != expected_identity:
            fail(f"opened {label} identity changed")
        if information.attributes & (0x10 | 0x400) or information.link_count != 1:
            fail(f"opened {label} is not a single-link non-reparse file")
    except BaseException:
        close_handle(handle)
        raise

    def close() -> None:
        close_handle(handle)

    return handle, close


def flush_windows_file_handle(handle: int) -> None:
    """Durably flush one already identity-pinned writable Windows file handle."""

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    flush = kernel32.FlushFileBuffers
    flush.argtypes = (ctypes.c_void_p,)
    flush.restype = ctypes.c_int
    if not flush(handle):
        raise ctypes.WinError(ctypes.get_last_error())


def set_windows_handle_file_attributes(handle: int, attributes: int) -> None:
    """Set only basic attributes on an already pinned Windows file handle."""

    if (
        isinstance(attributes, bool)
        or not isinstance(attributes, int)
        or attributes < 0
        or attributes > 0xFFFFFFFF
    ):
        fail("Windows file attributes are invalid")
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class BasicInformation(ctypes.Structure):
        _fields_ = (
            ("creation_time", ctypes.c_int64),
            ("last_access_time", ctypes.c_int64),
            ("last_write_time", ctypes.c_int64),
            ("change_time", ctypes.c_int64),
            ("attributes", ctypes.c_uint32),
        )

    get_information = kernel32.GetFileInformationByHandleEx
    get_information.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    )
    get_information.restype = ctypes.c_int
    set_information = kernel32.SetFileInformationByHandle
    set_information.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    )
    set_information.restype = ctypes.c_int
    information = BasicInformation()
    if not get_information(
        handle,
        0,  # FileBasicInfo
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    information.attributes = attributes
    if not set_information(
        handle,
        0,  # FileBasicInfo
        ctypes.byref(information),
        ctypes.sizeof(information),
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    observed = BasicInformation()
    if not get_information(
        handle,
        0,
        ctypes.byref(observed),
        ctypes.sizeof(observed),
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    if observed.attributes != attributes:
        fail("Windows file attributes did not reach the requested state")


def mark_windows_handle_delete(handle: int) -> None:
    """Unlink one exact Windows handle with immediate POSIX namespace semantics."""

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class DispositionInformationEx(ctypes.Structure):
        _fields_ = (("flags", ctypes.c_uint32),)

    set_information = kernel32.SetFileInformationByHandle
    set_information.argtypes = (
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.c_uint32,
    )
    set_information.restype = ctypes.c_int

    disposition = DispositionInformationEx(
        0x00000001  # FILE_DISPOSITION_DELETE
        | 0x00000002  # FILE_DISPOSITION_POSIX_SEMANTICS
        | 0x00000010  # FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE
    )
    if not set_information(
        handle,
        21,  # FileDispositionInfoEx
        ctypes.byref(disposition),
        ctypes.sizeof(disposition),
    ):
        raise ctypes.WinError(ctypes.get_last_error())


def rename_windows_directory_handle_noreplace(
    directory_handle: int,
    destination_parent_handle: int,
    destination_name: str,
) -> None:
    """Rename one pinned Windows directory relative to a pinned parent."""

    if destination_name in {"", ".", ".."} or Path(destination_name).name != destination_name:
        fail("Windows directory destination must be one flat name")
    ntdll = ctypes.WinDLL("ntdll")

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
    encoded = destination_name.encode("utf-16-le")
    offset = RenameInformation.file_name.offset
    buffer = ctypes.create_string_buffer(
        offset + len(encoded) + ctypes.sizeof(ctypes.c_uint16)
    )
    rename = RenameInformation.from_buffer(buffer)
    rename.replace_if_exists = 0
    rename.root_directory = destination_parent_handle
    rename.file_name_length = len(encoded)
    ctypes.memmove(ctypes.addressof(buffer) + offset, encoded, len(encoded))
    io_status = IoStatusBlock()
    status = set_information(
        directory_handle,
        ctypes.byref(io_status),
        ctypes.addressof(buffer),
        len(buffer),
        10,  # FileRenameInformation
    )
    if status != 0:
        error_number = status_to_dos_error(status)
        if error_number in {80, 183}:
            fail(f"refusing to replace existing destroy retirement: {destination_name}")
        raise ctypes.WinError(error_number)


def open_pinned_posix_stage(
    parent: Path,
    stage: Path,
    expected_parent_identity: tuple[int, int],
    expected_stage_identity: tuple[int, int],
) -> tuple[int, int]:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    parent_fd = os.open(parent, flags)
    try:
        stage_fd = os.open(stage.name, flags, dir_fd=parent_fd)
    except BaseException:
        os.close(parent_fd)
        raise
    if (os.fstat(parent_fd).st_dev, os.fstat(parent_fd).st_ino) != expected_parent_identity:
        os.close(stage_fd)
        os.close(parent_fd)
        fail("opened stage parent identity changed")
    if (os.fstat(stage_fd).st_dev, os.fstat(stage_fd).st_ino) != expected_stage_identity:
        os.close(stage_fd)
        os.close(parent_fd)
        fail("opened stage identity changed")
    return parent_fd, stage_fd


def create_posix_marker_file(
    directory_fd: int,
) -> tuple[str, int, tuple[int, int]]:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
    )
    for _ in range(32):
        name = f"{DELETE_MARKER_PREFIX}{secrets.token_hex(16)}"
        try:
            marker_fd = os.open(name, flags, 0o600, dir_fd=directory_fd)
        except FileExistsError:
            continue
        try:
            os.fchmod(marker_fd, 0o600)
            metadata = os.fstat(marker_fd)
            os.fsync(marker_fd)
            os.fsync(directory_fd)
            return name, marker_fd, (metadata.st_dev, metadata.st_ino)
        except BaseException:
            try:
                created = os.fstat(marker_fd)
                created_identity = created.st_dev, created.st_ino
            except OSError:
                created_identity = None
            os.close(marker_fd)
            try:
                current = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
                if created_identity == (current.st_dev, current.st_ino):
                    os.unlink(name, dir_fd=directory_fd)
                    os.fsync(directory_fd)
            except OSError:
                pass
            raise
    fail("could not allocate a unique lifecycle file marker")


def restore_posix_exchange(
    parent_fd: int,
    first_name: str,
    first_identity: tuple[int, int],
    second_name: str,
    second_identity: tuple[int, int],
) -> bool:
    """Best-effort rollback of an exchange only when both slots are unchanged."""

    try:
        first = os.stat(first_name, dir_fd=parent_fd, follow_symlinks=False)
        second = os.stat(second_name, dir_fd=parent_fd, follow_symlinks=False)
    except OSError:
        return False
    if (first.st_dev, first.st_ino) != first_identity or (
        second.st_dev,
        second.st_ino,
    ) != second_identity:
        return False
    linux_exchange_paths(parent_fd, first_name, parent_fd, second_name)
    return True


def exchange_exact_posix(
    parent_fd: int,
    first_name: str,
    first_identity: tuple[int, int],
    second_name: str,
    second_identity: tuple[int, int],
    label: str,
) -> None:
    """Exchange two names and reconcile ambiguous native completion by identity."""

    exchange_error: BaseException | None = None
    try:
        linux_exchange_paths(parent_fd, first_name, parent_fd, second_name)
    except BaseException as error:
        exchange_error = error
    try:
        first = os.stat(first_name, dir_fd=parent_fd, follow_symlinks=False)
        second = os.stat(second_name, dir_fd=parent_fd, follow_symlinks=False)
    except OSError as inspection_error:
        fail(
            f"{label} outcome is ambiguous after exchange: "
            f"exchange={exchange_error}; inspection={inspection_error}"
        )
    observed_first = first.st_dev, first.st_ino
    observed_second = second.st_dev, second.st_ino
    if observed_first == second_identity and observed_second == first_identity:
        return
    if observed_first == first_identity and observed_second == second_identity:
        if exchange_error is not None:
            raise exchange_error
        fail(f"{label} returned success without exchanging the exact entries")
    restored = False
    if observed_first == second_identity:
        restored = restore_posix_exchange(
            parent_fd,
            first_name,
            observed_first,
            second_name,
            observed_second,
        )
    fail(
        f"{label} exchanged an unexpected identity"
        + ("; foreign name restored" if restored else "; cleanup debt remains")
    )


def lifecycle_marker_value(
    capability: dict[str, object], kind: str
) -> dict[str, object]:
    if kind not in {"retirement", "deletion"}:
        fail(f"invalid lifecycle marker kind: {kind}")
    return {
        "schema": LIFECYCLE_MARKER_SCHEMA,
        "kind": kind,
        "stage_id": capability["stage_id"],
        "capability_sha256": capability_hash(capability),
    }


def ensure_posix_lifecycle_marker(
    parent_fd: int,
    name: str,
    capability: dict[str, object],
    kind: str,
) -> tuple[int, tuple[int, int]]:
    expected = canonical_json(lifecycle_marker_value(capability, kind))
    pending_name = f"{name}.pending"
    write_flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        pending_fd = -1
        try:
            try:
                stale_fd = os.open(
                    pending_name,
                    os.O_RDONLY
                    | getattr(os, "O_NOFOLLOW", 0)
                    | getattr(os, "O_NONBLOCK", 0)
                    | getattr(os, "O_CLOEXEC", 0),
                    dir_fd=parent_fd,
                )
            except FileNotFoundError:
                pass
            else:
                try:
                    stale = os.fstat(stale_fd)
                    current = os.stat(
                        pending_name,
                        dir_fd=parent_fd,
                        follow_symlinks=False,
                    )
                    if (
                        not stat.S_ISREG(stale.st_mode)
                        or is_reparse(stale)
                        or stale.st_nlink != 1
                        or (current.st_dev, current.st_ino)
                        != (stale.st_dev, stale.st_ino)
                    ):
                        fail(f"unsafe pending {kind} lifecycle marker: {pending_name}")
                    os.unlink(pending_name, dir_fd=parent_fd)
                    if os.fstat(stale_fd).st_nlink != 0:
                        fail(f"pending {kind} lifecycle marker remained linked")
                    os.fsync(parent_fd)
                finally:
                    os.close(stale_fd)
            pending_fd = os.open(
                pending_name,
                write_flags,
                0o600,
                dir_fd=parent_fd,
            )
            os.fchmod(pending_fd, 0o600)
            pending = os.fstat(pending_fd)
            pending_identity = pending.st_dev, pending.st_ino
            written = 0
            while written < len(expected):
                count = os.write(pending_fd, expected[written:])
                if count <= 0:
                    raise OSError(errno.EIO, "zero-length lifecycle marker write")
                written += count
            os.fsync(pending_fd)
            os.close(pending_fd)
            pending_fd = -1
            try:
                linux_rename_directory_noreplace(
                    parent_fd,
                    pending_name,
                    parent_fd,
                    name,
                )
            except BaseException as rename_error:
                try:
                    published = os.stat(
                        name, dir_fd=parent_fd, follow_symlinks=False
                    )
                except OSError:
                    raise rename_error
                if (published.st_dev, published.st_ino) != pending_identity:
                    raise rename_error
            os.fsync(parent_fd)
        except BaseException:
            if pending_fd >= 0:
                os.close(pending_fd)
            try:
                current = os.stat(
                    pending_name,
                    dir_fd=parent_fd,
                    follow_symlinks=False,
                )
                if "pending_identity" in locals() and (
                    current.st_dev,
                    current.st_ino,
                ) == pending_identity:
                    os.unlink(pending_name, dir_fd=parent_fd)
                    os.fsync(parent_fd)
            except OSError:
                pass
            raise
    read_flags = (
        os.O_RDONLY
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    marker_fd = os.open(name, read_flags, dir_fd=parent_fd)
    try:
        metadata = os.fstat(marker_fd)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or is_reparse(metadata)
            or metadata.st_nlink != 1
            or stat.S_IMODE(metadata.st_mode) != 0o600
        ):
            fail(f"{kind} lifecycle marker is not a private regular file: {name}")
        raw = b""
        while len(raw) <= MAX_JSON_BYTES:
            chunk = os.read(marker_fd, min(65536, MAX_JSON_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        current = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        if (current.st_dev, current.st_ino) != (metadata.st_dev, metadata.st_ino):
            fail(f"{kind} lifecycle marker changed while being opened: {name}")
        if raw != expected:
            fail(f"{kind} lifecycle marker content is not capability-authenticated")
        os.fsync(marker_fd)
        os.fsync(parent_fd)
        return marker_fd, (metadata.st_dev, metadata.st_ino)
    except BaseException:
        os.close(marker_fd)
        raise


def inspect_posix_lifecycle_marker(
    path: Path,
    metadata: os.stat_result | None,
    capability: dict[str, object],
) -> str | None:
    if metadata is None or not stat.S_ISREG(metadata.st_mode) or is_reparse(metadata):
        return None
    for kind in ("retirement", "deletion"):
        try:
            value, raw = read_regular_json(path, f"{kind} lifecycle marker")
        except (OSError, ReleaseSetError):
            continue
        if value == lifecycle_marker_value(capability, kind) and raw == canonical_json(
            value
        ):
            return kind
    return None


def remove_posix_lifecycle_marker(
    parent_fd: int,
    name: str,
    marker_fd: int,
    marker_identity: tuple[int, int],
) -> None:
    current = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    if (current.st_dev, current.st_ino) != marker_identity:
        fail(f"lifecycle marker identity changed before removal: {name}")
    os.unlink(name, dir_fd=parent_fd)
    if os.fstat(marker_fd).st_nlink != 0:
        fail(f"lifecycle marker remained linked after removal: {name}")
    os.fsync(parent_fd)


def seal_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    parent, stage, stage_metadata, parent_metadata = validate_owned_stage(capability)
    require_private_directory(parent_metadata, parent, "stage parent")
    require_private_directory(stage_metadata, stage, "stage")
    files = parse_files_manifest(Path(args.manifest))
    output_name = validate_flat_name(args.output_name, "release-set output name")
    candidate = {
        "schema": STAGE_SCHEMA,
        "state": "sealed",
        "stage_id": capability["stage_id"],
        "capability_sha256": capability_hash(capability),
        "output_name": output_name,
        "files": files,
    }
    parent_identity = parent_metadata.st_dev, parent_metadata.st_ino
    stage_identity = stage_metadata.st_dev, stage_metadata.st_ino
    parent_fd = -1
    stage_fd: int | None = None
    close_windows: Callable[[], None] | None = None
    if os.name == "nt":
        _, close_windows = open_pinned_windows_directory(
            stage, stage_identity, "stage"
        )
    else:
        parent_fd, stage_fd = open_pinned_posix_stage(
            parent,
            stage,
            parent_identity,
            stage_identity,
        )
    temporary: Path | None = None
    temporary_name: str | None = None
    temporary_fd: int | None = None
    try:
        hook = getattr(args, "_after_stage_pinned", None)
        if hook is not None:
            hook()
        current_stage = os.lstat(stage)
        if (current_stage.st_dev, current_stage.st_ino) != stage_identity:
            fail("stage path identity changed before sealing")
        descriptor_value, _ = read_regular_json(
            stage / DESCRIPTOR_NAME,
            "stage descriptor",
            directory_fd=stage_fd,
        )
        state = descriptor_value.get("state")
        if state == "sealed":
            descriptor = parse_stage_descriptor(
                stage,
                capability,
                "sealed",
                directory_fd=stage_fd,
            )
            if descriptor != candidate:
                fail("stage is already sealed with different declared content")
            verify_exact_stage(
                stage,
                descriptor,
                require_durable=True,
                directory_fd=stage_fd,
            )
            if stage_fd is not None:
                os.fsync(stage_fd)
                os.fsync(parent_fd)
            else:
                fsync_directory(stage)
                fsync_directory(parent)
            report_after_commit(
                (canonical_json(descriptor).decode("utf-8").removesuffix("\n"),)
            )
            return
        parse_stage_descriptor(
            stage,
            capability,
            "building",
            directory_fd=stage_fd,
        )
        try:
            pending_metadata = (
                os.stat(
                    SEAL_PENDING_NAME,
                    dir_fd=stage_fd,
                    follow_symlinks=False,
                )
                if stage_fd is not None
                else os.lstat(stage / SEAL_PENDING_NAME)
            )
        except FileNotFoundError:
            pass
        else:
            if (
                not stat.S_ISREG(pending_metadata.st_mode)
                or is_reparse(pending_metadata)
                or pending_metadata.st_nlink != 1
            ):
                fail("seal recovery marker is not a safe regular file")
            if stage_fd is not None:
                os.unlink(SEAL_PENDING_NAME, dir_fd=stage_fd)
                os.fsync(stage_fd)
            else:
                (stage / SEAL_PENDING_NAME).unlink()
                fsync_directory(stage)
        expected_building = {DESCRIPTOR_NAME} | {
            str(entry["name"]) for entry in files
        }
        building_names = inspect_stage_entries(stage, directory_fd=stage_fd)
        if building_names != expected_building:
            fail(
                f"release stage is not exact before sealing "
                f"(missing={sorted(expected_building - building_names)}, "
                f"extra={sorted(building_names - expected_building)})"
            )
        for entry in files:
            hash_stage_file(
                stage / str(entry["name"]),
                entry,
                require_flush=True,
                directory_fd=stage_fd,
            )
        if stage_fd is not None:
            temporary_name = SEAL_PENDING_NAME
            temporary_fd = os.open(
                temporary_name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
                dir_fd=stage_fd,
            )
        else:
            temporary = stage / SEAL_PENDING_NAME
            temporary_name = temporary.name
            temporary_fd = os.open(
                temporary,
                os.O_WRONLY
                | os.O_CREAT
                | os.O_EXCL
                | getattr(os, "O_BINARY", 0),
                0o600,
            )
            set_windows_file_acl(temporary, WINDOWS_PRIVATE_FILE_SDDL)
        if hasattr(os, "fchmod"):
            os.fchmod(temporary_fd, 0o644)
        temporary_output = os.fdopen(temporary_fd, "wb", closefd=True)
        temporary_fd = None
        with temporary_output as output:
            output.write(canonical_json(candidate))
            output.flush()
            os.fsync(output.fileno())
        if (os.lstat(stage).st_dev, os.lstat(stage).st_ino) != stage_identity:
            fail("stage path identity changed before descriptor commit")
        with PublicationSignalGuard() as signal_guard:
            signal_guard.refuse_pending_before_commit()
            if stage_fd is not None:
                os.replace(
                    temporary_name,
                    DESCRIPTOR_NAME,
                    src_dir_fd=stage_fd,
                    dst_dir_fd=stage_fd,
                )
            else:
                assert temporary is not None
                os.replace(temporary, stage / DESCRIPTOR_NAME)
            temporary = None
            temporary_name = None
            descriptor = parse_stage_descriptor(
                stage,
                capability,
                "sealed",
                directory_fd=stage_fd,
            )
            if descriptor != candidate:
                fail("sealed descriptor differs from the requested transition")
            verify_exact_stage(stage, descriptor, directory_fd=stage_fd)
            final_stage = os.lstat(stage)
            if (final_stage.st_dev, final_stage.st_ino) != stage_identity:
                fail("stage path identity changed after descriptor commit")
            if stage_fd is not None:
                os.fsync(stage_fd)
                os.fsync(parent_fd)
            else:
                fsync_directory(stage)
                fsync_directory(parent)
            signal_guard.mark_committed()
            report_after_commit(
                (canonical_json(descriptor).decode("utf-8").removesuffix("\n"),)
            )
    finally:
        if temporary_fd is not None:
            try:
                os.close(temporary_fd)
            except OSError:
                pass
        removed_temporary = False
        if temporary_name is not None:
            try:
                if stage_fd is not None:
                    os.unlink(temporary_name, dir_fd=stage_fd)
                elif temporary is not None:
                    temporary.unlink()
                removed_temporary = True
            except FileNotFoundError:
                pass
        if removed_temporary:
            if stage_fd is not None:
                os.fsync(stage_fd)
            else:
                fsync_directory(stage)
        if stage_fd is not None:
            try:
                os.close(stage_fd)
            except OSError:
                pass
            try:
                os.close(parent_fd)
            except OSError:
                pass
        if close_windows is not None:
            close_windows()


def publish(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    stage_parent, stage, stage_metadata, stage_parent_metadata = validate_owned_stage(
        capability
    )
    require_private_directory(stage_parent_metadata, stage_parent, "stage parent")
    require_private_directory(stage_metadata, stage, "stage")
    descriptor = parse_stage_descriptor(stage, capability, "sealed")
    output_parent, output_parent_metadata = safe_directory(
        args.output_parent, "output parent"
    )
    if stage_metadata.st_dev != output_parent_metadata.st_dev:
        fail("stage and output parent must be on the same filesystem")
    output = output_parent / str(descriptor["output_name"])
    try:
        os.lstat(output)
    except FileNotFoundError:
        pass
    else:
        fail(f"release-set output already exists: {output}")
    verify_exact_stage(stage, descriptor, prepare_publication=True)
    if os.environ.get("DCENT_RELEASE_SET_TEST_FAIL_BEFORE_PROMOTION") == "1":
        fail("injected failure before release-set promotion")
    descriptor_digest = hashlib.sha256(canonical_json(descriptor)).hexdigest()
    with PublicationSignalGuard() as signal_guard:
        signal_guard.refuse_pending_before_commit()
        try:
            atomic_publish_directory(
                stage,
                output,
                expected_staged_identity=(stage_metadata.st_dev, stage_metadata.st_ino),
                expected_staging_parent_identity=(
                    stage_parent_metadata.st_dev,
                    stage_parent_metadata.st_ino,
                ),
                expected_destination_parent_identity=(
                    output_parent_metadata.st_dev,
                    output_parent_metadata.st_ino,
                ),
                private_mode=0o700,
                final_mode=0o755,
                post_commit_verify=lambda published, directory_fd: verify_exact_stage(
                    published,
                    descriptor,
                    require_public_modes=True,
                    directory_fd=directory_fd,
                ),
                _before_commit=signal_guard.refuse_pending_before_commit,
                _windows_access_transition=lambda path, public: (
                    transition_windows_release_access(path, descriptor, public)
                ),
                _windows_private_access_verify=lambda path: (
                    require_private_windows_acl(path, "pinned staging boundary")
                ),
            )
        except DirectoryPublishError as error:
            fail(str(error))
        signal_guard.mark_committed()
        result = {
            "schema": RESULT_SCHEMA,
            "path": str(output),
            "release_set_id": descriptor["stage_id"],
            "descriptor_sha256": descriptor_digest,
            "files": descriptor["files"],
        }
        report_after_commit(
            (canonical_json(result).decode("utf-8").removesuffix("\n"),)
        )


def retire_stage_posix(
    parent: Path,
    stage: Path,
    retired: Path,
    parent_identity: tuple[int, int],
    stage_identity: tuple[int, int],
    capability: dict[str, object],
    *,
    after_opened: Callable[[], None] | None = None,
    after_exchange: Callable[[], None] | None = None,
    after_rename: Callable[[], None] | None = None,
) -> None:
    parent_fd, stage_fd = open_pinned_posix_stage(
        parent,
        stage,
        parent_identity,
        stage_identity,
    )
    marker_fd = -1
    marker_identity = (0, 0)
    exchanged = False
    try:
        marker_fd, marker_identity = ensure_posix_lifecycle_marker(
            parent_fd,
            retired.name,
            capability,
            "retirement",
        )
        if after_opened is not None:
            after_opened()
        with PublicationSignalGuard() as signal_guard:
            signal_guard.refuse_pending_before_commit()
            exchange_exact_posix(
                parent_fd,
                stage.name,
                stage_identity,
                retired.name,
                marker_identity,
                "deterministic stage retirement",
            )
            exchanged = True
            if after_exchange is not None:
                after_exchange()
            os.fsync(parent_fd)
            signal_guard.mark_committed()
            remove_posix_lifecycle_marker(
                parent_fd,
                stage.name,
                marker_fd,
                marker_identity,
            )
            os.close(marker_fd)
            marker_fd = -1
            if after_rename is not None:
                after_rename()
    finally:
        if marker_fd >= 0:
            if not exchanged:
                try:
                    current = os.stat(
                        retired.name,
                        dir_fd=parent_fd,
                        follow_symlinks=False,
                    )
                    if (current.st_dev, current.st_ino) == marker_identity:
                        remove_posix_lifecycle_marker(
                            parent_fd,
                            retired.name,
                            marker_fd,
                            marker_identity,
                        )
                except OSError:
                    pass
            try:
                os.close(marker_fd)
            except OSError:
                pass
        try:
            os.close(stage_fd)
        except OSError:
            pass
        try:
            os.close(parent_fd)
        except OSError:
            pass


def retire_stage_windows(
    parent: Path,
    stage: Path,
    retired: Path,
    parent_identity: tuple[int, int],
    stage_identity: tuple[int, int],
    *,
    after_opened: Callable[[], None] | None = None,
    after_rename: Callable[[], None] | None = None,
) -> None:
    parent_handle, close_parent = open_pinned_windows_directory(
        parent, parent_identity, "stage parent"
    )
    try:
        stage_handle, close_stage = open_pinned_windows_directory(
            stage,
            stage_identity,
            "stage",
            movable=True,
        )
        try:
            if os.path.lexists(retired):
                fail(f"destroy retirement already exists: {retired}")
            if after_opened is not None:
                after_opened()
            current = stage.lstat()
            if (current.st_dev, current.st_ino) != stage_identity:
                fail("stage name changed before destroy retirement")
            with PublicationSignalGuard() as signal_guard:
                signal_guard.refuse_pending_before_commit()
                rename_windows_directory_handle_noreplace(
                    stage_handle,
                    parent_handle,
                    retired.name,
                )
                if after_rename is not None:
                    after_rename()
                retired_metadata = retired.lstat()
                if (retired_metadata.st_dev, retired_metadata.st_ino) != stage_identity:
                    fail("retired stage identity differs from capability")
                # Preserve any object that concurrently reuses the active
                # stage name; only the handle-retired directory is authorized.
                fsync_directory(parent)
                signal_guard.mark_committed()
        finally:
            close_stage()
    finally:
        close_parent()


def validate_cleanup_entry(metadata: os.stat_result, name: str) -> None:
    if (
        not stat.S_ISREG(metadata.st_mode)
        or is_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
    ):
        fail(f"refusing to destroy stage containing unsafe entry: {name}")


def preflight_cleanup_entries(stage: Path) -> None:
    """Refuse known-unsafe contents before moving an active stage to retirement."""

    with os.scandir(stage) as scanner:
        for entry in scanner:
            name = validate_flat_name(
                entry.name, "cleanup entry name", allow_descriptor=True
            )
            # On supported Windows Python builds DirEntry.stat() can report a
            # synthetic zero link count; os.lstat() supplies the stable file
            # identity metadata used by the rest of this implementation.
            validate_cleanup_entry(os.lstat(stage / name), name)


def delete_exact_posix_entry(
    stage_fd: int,
    name: str,
    expected_identity: tuple[int, int],
) -> None:
    open_flags = (
        getattr(os, "O_PATH", os.O_RDONLY)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    exact_fd = os.open(name, open_flags, dir_fd=stage_fd)
    marker_fd = -1
    marker_name = ""
    marker_identity = (0, 0)
    try:
        exact = os.fstat(exact_fd)
        validate_cleanup_entry(exact, name)
        if (exact.st_dev, exact.st_ino) != expected_identity:
            fail(f"cleanup entry identity changed before isolation: {name}")
        marker_name, marker_fd, marker_identity = create_posix_marker_file(stage_fd)
        try:
            exchange_exact_posix(
                stage_fd,
                name,
                expected_identity,
                marker_name,
                marker_identity,
                f"cleanup entry isolation for {name}",
            )
        except BaseException:
            try:
                marker = os.stat(
                    marker_name, dir_fd=stage_fd, follow_symlinks=False
                )
                if (marker.st_dev, marker.st_ino) == marker_identity:
                    os.unlink(marker_name, dir_fd=stage_fd)
                    if os.fstat(marker_fd).st_nlink != 0:
                        fail(f"cleanup marker remained linked after rollback: {name}")
                    os.fsync(stage_fd)
            except FileNotFoundError:
                pass
            raise
        os.unlink(marker_name, dir_fd=stage_fd)
        if os.fstat(exact_fd).st_nlink != 0:
            fail(f"exact cleanup entry remained linked after deletion: {name}")
        marker = os.stat(name, dir_fd=stage_fd, follow_symlinks=False)
        if (marker.st_dev, marker.st_ino) != marker_identity:
            fail(f"cleanup marker slot was reoccupied: {name}")
        os.unlink(name, dir_fd=stage_fd)
        if os.fstat(marker_fd).st_nlink != 0:
            fail(f"cleanup marker remained linked after deletion: {name}")
        os.fsync(stage_fd)
    finally:
        if marker_fd >= 0:
            os.close(marker_fd)
        os.close(exact_fd)


def destroy_stage_posix(
    parent: Path,
    stage: Path,
    expected_parent_identity: tuple[int, int],
    expected_stage_identity: tuple[int, int],
    capability: dict[str, object],
    *,
    before_entry_unlink: Callable[[str], None] | None = None,
    after_entry_unlink: Callable[[str], None] | None = None,
    before_rmdir: Callable[[], None] | None = None,
    after_rmdir: Callable[[], None] | None = None,
    expected_entries: dict[str, tuple[int, int]] | None = None,
    deletion_marker_path: Path | None = None,
    after_delete_exchange: Callable[[], None] | None = None,
) -> None:
    directory_flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    parent_fd = os.open(parent, directory_flags)
    stage_fd = -1
    try:
        stage_fd = os.open(stage.name, directory_flags, dir_fd=parent_fd)
        parent_metadata = os.fstat(parent_fd)
        stage_metadata = os.fstat(stage_fd)
        if (parent_metadata.st_dev, parent_metadata.st_ino) != expected_parent_identity:
            fail("opened stage parent identity changed before destruction")
        if (stage_metadata.st_dev, stage_metadata.st_ino) != expected_stage_identity:
            fail("opened stage identity changed before destruction")
        require_private_directory(parent_metadata, parent, "opened stage parent")
        require_private_directory(stage_metadata, stage, "opened stage")

        entries: list[tuple[str, tuple[int, int]]] = []
        with os.scandir(stage_fd) as scanner:
            for entry in scanner:
                name = validate_flat_name(
                    entry.name, "cleanup entry name", allow_descriptor=True
                )
                metadata = os.stat(name, dir_fd=stage_fd, follow_symlinks=False)
                validate_cleanup_entry(metadata, name)
                identity = metadata.st_dev, metadata.st_ino
                if expected_entries is not None:
                    if name not in expected_entries:
                        fail(f"new stage gained unexpected rollback entry: {name}")
                    if identity != expected_entries[name]:
                        fail(f"rollback entry identity changed: {name}")
                entries.append((name, identity))
        if expected_entries is not None and {name for name, _ in entries} != set(
            expected_entries
        ):
            fail("new stage rollback entries no longer match the created set")
        # Retain the capability-bearing descriptor until every payload is gone,
        # maximizing the chance that an interrupted destroy remains retryable.
        entries.sort(key=lambda item: item[0] == DESCRIPTOR_NAME)
        for name, expected_identity in entries:
            current = os.stat(name, dir_fd=stage_fd, follow_symlinks=False)
            validate_cleanup_entry(current, name)
            if (current.st_dev, current.st_ino) != expected_identity:
                fail(f"cleanup entry identity changed before deletion: {name}")
            if before_entry_unlink is not None:
                before_entry_unlink(name)
            delete_exact_posix_entry(stage_fd, name, expected_identity)
            if after_entry_unlink is not None:
                after_entry_unlink(name)
        os.fsync(stage_fd)
        if before_rmdir is not None:
            before_rmdir()
        deleting = parent / f"{DELETING_PREFIX}{capability['stage_id']}"
        marker_fd = -1
        marker_identity = (0, 0)
        marker_path = deletion_marker_path
        try:
            if stage != deleting:
                marker_fd, marker_identity = ensure_posix_lifecycle_marker(
                    parent_fd,
                    deleting.name,
                    capability,
                    "deletion",
                )
                exchange_exact_posix(
                    parent_fd,
                    stage.name,
                    expected_stage_identity,
                    deleting.name,
                    marker_identity,
                    "deterministic final-stage isolation",
                )
                os.fsync(parent_fd)
                marker_path = stage
                if after_delete_exchange is not None:
                    after_delete_exchange()
            else:
                if marker_path is None:
                    fail("resumed deletion is missing its deterministic marker path")
                marker_fd, marker_identity = ensure_posix_lifecycle_marker(
                    parent_fd,
                    marker_path.name,
                    capability,
                    "deletion",
                )
            os.rmdir(deleting.name, dir_fd=parent_fd)
            if os.fstat(stage_fd).st_nlink != 0:
                fail("exact retired stage remained linked after deletion")
            if after_rmdir is not None:
                after_rmdir()
            assert marker_path is not None
            remove_posix_lifecycle_marker(
                parent_fd,
                marker_path.name,
                marker_fd,
                marker_identity,
            )
            os.close(marker_fd)
            marker_fd = -1
        finally:
            if marker_fd >= 0:
                os.close(marker_fd)
        os.fsync(parent_fd)
    finally:
        if stage_fd >= 0:
            try:
                os.close(stage_fd)
            except OSError:
                pass
        try:
            os.close(parent_fd)
        except OSError:
            pass


def destroy_stage_windows(
    parent: Path,
    stage: Path,
    expected_parent_identity: tuple[int, int],
    expected_stage_identity: tuple[int, int],
    *,
    before_entry_unlink: Callable[[str], None] | None = None,
    after_entry_unlink: Callable[[str], None] | None = None,
    before_rmdir: Callable[[], None] | None = None,
    after_rmdir: Callable[[], None] | None = None,
) -> None:
    """Delete exact Windows objects while directory handles block name replacement."""

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
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int
    flush_handle = kernel32.FlushFileBuffers
    flush_handle.argtypes = (ctypes.c_void_p,)
    flush_handle.restype = ctypes.c_int

    read_attributes = 0x00000080
    delete_access = 0x00010000
    share_read_write = 0x00000001 | 0x00000002
    share_all = share_read_write | 0x00000004
    open_existing = 3
    backup_semantics = 0x02000000
    open_reparse_point = 0x00200000
    invalid_handle = ctypes.c_void_p(-1).value

    def open_exact(path: Path, *, directory: bool, pin_name: bool) -> int:
        handle = create_file(
            str(path),
            read_attributes | delete_access | (0x40000000 if directory else 0),
            share_read_write if pin_name else share_all,
            None,
            open_existing,
            (backup_semantics if directory else 0) | open_reparse_point,
            None,
        )
        if handle == invalid_handle:
            raise ctypes.WinError(ctypes.get_last_error())
        return handle

    def handle_information(handle: int) -> HandleInformation:
        value = HandleInformation()
        if not get_information(handle, ctypes.byref(value)):
            raise ctypes.WinError(ctypes.get_last_error())
        return value

    def handle_identity(value: HandleInformation) -> tuple[int, int]:
        return value.volume_serial, (value.file_index_high << 32) | value.file_index_low

    parent_handle = open_exact(parent, directory=True, pin_name=True)
    stage_handle = -1
    try:
        parent_info = handle_information(parent_handle)
        if handle_identity(parent_info) != expected_parent_identity:
            fail("opened stage parent identity changed before destruction")
        stage_handle = open_exact(stage, directory=True, pin_name=True)
        stage_info = handle_information(stage_handle)
        if handle_identity(stage_info) != expected_stage_identity:
            fail("opened stage identity changed before destruction")
        if not stage_info.attributes & 0x10 or stage_info.attributes & 0x400:
            fail("opened stage is not a non-reparse directory")

        names = [
            validate_flat_name(entry.name, "cleanup entry name", allow_descriptor=True)
            for entry in os.scandir(stage)
        ]
        names.sort(key=lambda name: name == DESCRIPTOR_NAME)
        for name in names:
            leaf_handle = open_exact(stage / name, directory=False, pin_name=False)
            try:
                leaf_info = handle_information(leaf_handle)
                if leaf_info.attributes & (0x10 | 0x400) or leaf_info.link_count != 1:
                    fail(f"refusing to destroy stage containing unsafe entry: {name}")
                if before_entry_unlink is not None:
                    before_entry_unlink(name)
                mark_windows_handle_delete(leaf_handle)
            finally:
                close_handle(leaf_handle)
            if after_entry_unlink is not None:
                after_entry_unlink(name)
            if not flush_handle(stage_handle):
                raise ctypes.WinError(ctypes.get_last_error())
        if before_rmdir is not None:
            before_rmdir()
        mark_windows_handle_delete(stage_handle)
        close_handle(stage_handle)
        stage_handle = -1
        if after_rmdir is not None:
            after_rmdir()
        try:
            remaining = stage.lstat()
        except FileNotFoundError:
            pass
        else:
            remaining_identity = remaining.st_dev, remaining.st_ino
            if remaining_identity == expected_stage_identity:
                fail("retired stage remains visible after deletion")
            if remaining_identity == (0, 0):
                fail("retired stage deletion has an ambiguous visible identity")
            # A different identity may legitimately reuse the retired name
            # after POSIX-style handle deletion. It is outside this capability.
        fsync_directory(parent)
    finally:
        if stage_handle >= 0:
            close_handle(stage_handle)
        close_handle(parent_handle)


def remove_named_posix_lifecycle_marker(
    parent: Path,
    parent_identity: tuple[int, int],
    marker: Path,
    capability: dict[str, object],
    kind: str,
) -> None:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    parent_fd = os.open(parent, flags)
    marker_fd = -1
    try:
        metadata = os.fstat(parent_fd)
        if (metadata.st_dev, metadata.st_ino) != parent_identity:
            fail("stage parent identity changed before marker cleanup")
        marker_fd, marker_identity = ensure_posix_lifecycle_marker(
            parent_fd,
            marker.name,
            capability,
            kind,
        )
        remove_posix_lifecycle_marker(
            parent_fd,
            marker.name,
            marker_fd,
            marker_identity,
        )
    finally:
        if marker_fd >= 0:
            os.close(marker_fd)
        os.close(parent_fd)


def destroy_stage_posix_lifecycle(
    args: argparse.Namespace,
    capability: dict[str, object],
    parent: Path,
    parent_metadata: os.stat_result,
) -> None:
    active = Path(str(capability["stage_path"]))
    retired = parent / f"{DESTROYING_PREFIX}{capability['stage_id']}"
    deleting = parent / f"{DELETING_PREFIX}{capability['stage_id']}"
    parent_identity = parent_metadata.st_dev, parent_metadata.st_ino
    stage_identity = int(capability["stage_dev"]), int(capability["stage_ino"])
    paths = {"active": active, "retired": retired, "deleting": deleting}
    metadata: dict[str, os.stat_result | None] = {}
    for label, path in paths.items():
        try:
            metadata[label] = path.lstat()
        except FileNotFoundError:
            metadata[label] = None
    exact = [
        label
        for label, value in metadata.items()
        if value is not None
        and stat.S_ISDIR(value.st_mode)
        and not is_reparse(value)
        and (value.st_dev, value.st_ino) == stage_identity
    ]
    if len(exact) > 1:
        fail("capability identity appears under multiple lifecycle names")
    markers = {
        label: inspect_posix_lifecycle_marker(
            paths[label], metadata[label], capability
        )
        for label in paths
    }
    if not exact:
        deletion_markers = [
            paths[label] for label, kind in markers.items() if kind == "deletion"
        ]
        if deletion_markers:
            # A deletion marker without the deterministic deleting directory
            # is the durable post-rmdir state. Remove only authenticated markers.
            if metadata["deleting"] is not None:
                fail("deletion marker exists but deleting-stage identity is ambiguous")
            for marker in deletion_markers:
                remove_named_posix_lifecycle_marker(
                    parent,
                    parent_identity,
                    marker,
                    capability,
                    "deletion",
                )
            fsync_directory(parent)
            return
        if all(value is None for value in metadata.values()):
            fsync_directory(parent)
            return
        fail("no lifecycle name contains the capability-owned stage identity")

    exact_label = exact[0]
    stage = paths[exact_label]
    stage_metadata = metadata[exact_label]
    assert stage_metadata is not None
    require_private_directory(stage_metadata, stage, f"{exact_label} stage")
    cleanup_options = {
        "before_entry_unlink": getattr(args, "_before_destroy_entry", None),
        "after_entry_unlink": getattr(args, "_after_destroy_entry", None),
        "before_rmdir": getattr(args, "_before_destroy_rmdir", None),
        "after_rmdir": getattr(args, "_after_destroy_rmdir", None),
        "after_delete_exchange": getattr(args, "_after_delete_exchange", None),
    }
    if exact_label == "active":
        retired_kind = markers["retired"]
        if metadata["retired"] is not None and retired_kind != "retirement":
            fail(f"destroy retirement name is occupied by a foreign object: {retired}")
        descriptor_value, _ = read_regular_json(
            active / DESCRIPTOR_NAME, "stage descriptor"
        )
        state = descriptor_value.get("state")
        if state not in ("building", "sealed"):
            fail("owned stage descriptor has an invalid cleanup state")
        parse_stage_descriptor(active, capability, str(state))
        preflight_cleanup_entries(active)
        retire_stage_posix(
            parent,
            active,
            retired,
            parent_identity,
            stage_identity,
            capability,
            after_opened=getattr(args, "_after_retire_opened", None),
            after_exchange=getattr(args, "_after_retire_exchange", None),
            after_rename=getattr(args, "_after_retire_rename", None),
        )
        stage = retired
        exact_label = "retired"
    elif exact_label == "retired" and markers["active"] == "retirement":
        remove_named_posix_lifecycle_marker(
            parent,
            parent_identity,
            active,
            capability,
            "retirement",
        )
    deletion_marker_path: Path | None = None
    if exact_label == "deleting":
        candidates = [
            paths[label]
            for label in ("active", "retired")
            if markers[label] == "deletion"
        ]
        if len(candidates) != 1:
            fail("deleting stage does not have one authenticated deletion marker")
        deletion_marker_path = candidates[0]
    destroy_stage_posix(
        parent,
        stage,
        parent_identity,
        stage_identity,
        capability,
        deletion_marker_path=deletion_marker_path,
        **cleanup_options,
    )


def destroy_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    parent, parent_metadata = safe_directory(
        str(capability["stage_parent"]), "stage parent"
    )
    require_private_directory(parent_metadata, parent, "stage parent")
    if os.name != "nt":
        destroy_stage_posix_lifecycle(args, capability, parent, parent_metadata)
        return
    stage = Path(str(capability["stage_path"]))
    retired = parent / f"{DESTROYING_PREFIX}{capability['stage_id']}"
    parent_identity = parent_metadata.st_dev, parent_metadata.st_ino
    stage_identity = int(capability["stage_dev"]), int(capability["stage_ino"])
    try:
        stage_metadata = stage.lstat()
    except FileNotFoundError:
        stage_metadata = None
    try:
        retired_metadata = retired.lstat()
    except FileNotFoundError:
        retired_metadata = None
    if stage_metadata is None and retired_metadata is None:
        fsync_directory(parent)
        return
    active_matches = stage_metadata is not None and (
        stat.S_ISDIR(stage_metadata.st_mode)
        and not is_reparse(stage_metadata)
        and (stage_metadata.st_dev, stage_metadata.st_ino) == stage_identity
    )
    retired_matches = retired_metadata is not None and (
        stat.S_ISDIR(retired_metadata.st_mode)
        and not is_reparse(retired_metadata)
        and (retired_metadata.st_dev, retired_metadata.st_ino) == stage_identity
    )
    if retired_matches:
        if active_matches:
            fail("capability identity appears under both active and retired names")
        assert retired_metadata is not None
        require_private_directory(retired_metadata, retired, "retired stage")
        stage = retired
    elif retired_metadata is not None:
        if active_matches:
            fail(f"destroy retirement name is occupied by a foreign object: {retired}")
        fail("neither active nor retired stage identity matches the capability")
    elif active_matches:
        assert stage_metadata is not None
        require_private_directory(stage_metadata, stage, "stage")
        descriptor_value, _ = read_regular_json(
            stage / DESCRIPTOR_NAME, "stage descriptor"
        )
        state = descriptor_value.get("state")
        if state not in ("building", "sealed"):
            fail("owned stage descriptor has an invalid cleanup state")
        parse_stage_descriptor(stage, capability, str(state))
        # Keep a predictably invalid active stage at its caller-visible name.
        # Destruction repeats every check through pinned objects after retirement,
        # so this usability preflight is never the authorization boundary.
        preflight_cleanup_entries(stage)
        retirement_options = {
            "after_opened": getattr(args, "_after_retire_opened", None),
            "after_rename": getattr(args, "_after_retire_rename", None),
        }
        retire_stage_windows(
            parent,
            stage,
            retired,
            parent_identity,
            stage_identity,
            **retirement_options,
        )
        stage = retired
    else:
        fail("owned stage identity does not match the capability")
    cleanup_options = {
        "before_entry_unlink": getattr(args, "_before_destroy_entry", None),
        "after_entry_unlink": getattr(args, "_after_destroy_entry", None),
        "before_rmdir": getattr(args, "_before_destroy_rmdir", None),
        "after_rmdir": getattr(args, "_after_destroy_rmdir", None),
    }
    destroy_stage_windows(
        parent,
        stage,
        parent_identity,
        stage_identity,
        **cleanup_options,
    )


def query(args: argparse.Namespace) -> None:
    raw = sys.stdin.buffer.read(MAX_JSON_BYTES + 1)
    if len(raw) > MAX_JSON_BYTES:
        fail("query input exceeds the one-MiB bound")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"query input is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail("query input must be an object")
    schema = value.get("schema")
    if schema == CAPABILITY_SCHEMA:
        required = {
            "schema",
            "stage_id",
            "capability",
            "stage_parent",
            "stage_path",
            "stage_dev",
            "stage_ino",
        }
        if set(value) != required:
            fail("query capability has an invalid exact schema")
        stage_id = validate_hex(value["stage_id"], "query stage ID", 32)
        validate_hex(value["capability"], "query capability", 64)
        if any(
            not isinstance(value[field], str)
            or not value[field]
            or value[field] != str(absolute(value[field]))
            for field in ("stage_parent", "stage_path")
        ):
            fail("query capability paths are invalid")
        if any(
            isinstance(value[field], bool)
            or not isinstance(value[field], int)
            or value[field] < 0
            for field in ("stage_dev", "stage_ino")
        ):
            fail("query capability identity is invalid")
        stage_path = Path(str(value["stage_path"]))
        if (
            stage_path.parent != Path(str(value["stage_parent"]))
            or stage_path.name != f".dcent-release-set-stage-{stage_id}"
        ):
            fail("query capability stage relationship is invalid")
    elif schema == STAGE_SCHEMA:
        state = value.get("state")
        expected = {"schema", "state", "stage_id", "capability_sha256"}
        if state == "sealed":
            expected |= {"output_name", "files"}
        elif state != "building":
            fail("query stage state is invalid")
        if set(value) != expected:
            fail("query stage has an invalid exact schema")
        validate_hex(value["stage_id"], "query stage ID", 32)
        validate_hex(value["capability_sha256"], "query capability digest", 64)
        if state == "sealed":
            validate_flat_name(value["output_name"], "query output name")
            validate_file_entries(value["files"], "query stage")
    elif schema == RESULT_SCHEMA:
        if set(value) != {
            "schema",
            "path",
            "release_set_id",
            "descriptor_sha256",
            "files",
        }:
            fail("query publication result has an invalid exact schema")
        if (
            not isinstance(value["path"], str)
            or not value["path"]
            or value["path"] != str(absolute(value["path"]))
        ):
            fail("query publication path is invalid")
        validate_hex(value["release_set_id"], "query release-set ID", 32)
        validate_hex(value["descriptor_sha256"], "query descriptor digest", 64)
        validate_file_entries(value["files"], "query publication result")
    else:
        fail("query input schema is unsupported")
    allowed = {
        CAPABILITY_SCHEMA: {
            "stage-id": "stage_id",
            "stage-path": "stage_path",
            "capability": "capability",
        },
        STAGE_SCHEMA: {"stage-id": "stage_id", "output-name": "output_name"},
        RESULT_SCHEMA: {
            "release-set-id": "release_set_id",
            "published-path": "path",
            "descriptor-sha256": "descriptor_sha256",
        },
    }
    if args.field not in allowed[schema]:
        fail("query field is not valid for the input schema")
    field = allowed[schema][args.field]
    if (
        field not in value
        or not isinstance(value[field], (str, int))
        or isinstance(value[field], bool)
    ):
        fail("query value is absent or invalid")
    print(value[field])


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create-stage")
    create.add_argument("--parent", required=True)
    create.add_argument("--capability-output")
    create.set_defaults(function=create_stage)
    seal = commands.add_parser("seal-stage")
    seal.add_argument("--capability-file", required=True)
    seal.add_argument("--manifest", required=True)
    seal.add_argument("--output-name", required=True)
    seal.set_defaults(function=seal_stage)
    manifest = commands.add_parser("manifest-stage")
    manifest.add_argument("--capability-file", required=True)
    manifest.add_argument("--output", required=True)
    manifest.set_defaults(function=manifest_stage)
    promote = commands.add_parser("publish")
    promote.add_argument("--capability-file", required=True)
    promote.add_argument("--output-parent", required=True)
    promote.set_defaults(function=publish)
    destroy = commands.add_parser("destroy-stage")
    destroy.add_argument("--capability-file", required=True)
    destroy.set_defaults(function=destroy_stage)
    query_command = commands.add_parser("query")
    query_command.add_argument(
        "--field",
        choices=(
            "stage-id",
            "stage-path",
            "capability",
            "output-name",
            "release-set-id",
            "published-path",
            "descriptor-sha256",
        ),
        required=True,
    )
    query_command.set_defaults(function=query)
    return root


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (ReleaseSetError, OSError) as error:
        print(f"ERROR: release-set publication: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
