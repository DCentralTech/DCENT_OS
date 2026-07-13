#!/usr/bin/env python3
"""Create and validate private snapshots of target-scoped external build inputs.

This tool closes the check-then-reopen gap for inputs selected by
``source_closure.py``.  It does not snapshot or claim closure over the source
tree, compiler, container, dependencies, or produced artifacts.
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
import pathlib
import secrets
import stat
import sys
import tempfile
from typing import (
    Any,
    BinaryIO,
    Callable,
    Dict,
    Iterable,
    List,
    NamedTuple,
    NoReturn,
    Optional,
    Tuple,
)


SCHEMA = "org.dcentral.dcentos.build-input-snapshot.v1"
SPLIT_AUTHORITY_SCHEMA = "org.dcentral.dcentos.build-input-snapshot.v2"
SELECTION_ROOT_KIND = "separate-verified-root"
STAGE_PREFIX = "dcentos-build-inputs-"
SENTINEL_NAME = ".dcentos-build-input-snapshot-owner"
SNAPSHOT_NAME = "snapshot.json"
OWNER_SCHEMA = "org.dcentral.dcentos.build-input-snapshot-owner.v1"
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
HEX_64 = frozenset("0123456789abcdef")
Hook = Optional[Callable[[str, BinaryIO], None]]
SNAPSHOT_CLAIM = "selected_manifest_pinned_bytes_copied_from_open_regular_file_handles"
SNAPSHOT_NON_CLAIMS = (
    "complete_source_tree_snapshot",
    "consumer_execution_or_artifact_causality",
    "toolchain_or_dependency_closure_beyond_selected_files",
)


class CreatedSnapshot(NamedTuple):
    """A private stage plus the capability required to destroy it."""

    snapshot: pathlib.Path
    snapshot_id: str
    destroy_token: str
    files: Tuple[Dict[str, Any], ...]

    @property
    def stage(self) -> pathlib.Path:
        return self.snapshot.parent

    def cli_result(self) -> Dict[str, Any]:
        return {
            "destroy_token": self.destroy_token,
            "files": [dict(item) for item in self.files],
            "snapshot": str(self.snapshot),
            "snapshot_id": self.snapshot_id,
            "stage": str(self.stage),
        }


class SnapshotError(ValueError):
    """A build-input snapshot could not be created or validated safely."""


def fail(message: str) -> NoReturn:
    raise SnapshotError(message)


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _valid_digest(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and not (set(value) - HEX_64)


def _require_exact_object(
    value: object, label: str, keys: Iterable[str]
) -> Dict[str, Any]:
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


def _require_size(value: object, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        fail(f"{label} must be a non-negative integer")
    return value


def _require_regular_single_link(metadata: os.stat_result, label: str) -> None:
    if not stat.S_ISREG(metadata.st_mode) or _is_windows_reparse(metadata):
        fail(f"{label} must be a non-reparse regular file")
    if metadata.st_nlink != 1:
        fail(f"{label} must have exactly one filesystem link")


def _validate_relative_path(value: str, label: str) -> Tuple[str, ...]:
    if not isinstance(value, str) or not value or "\\" in value:
        fail(f"{label} is not a canonical POSIX relative path: {value!r}")
    pure = pathlib.PurePosixPath(value)
    if pure.is_absolute() or any(part in ("", ".", "..") for part in pure.parts):
        fail(f"{label} is unsafe: {value}")
    if pure.as_posix() != value:
        fail(f"{label} is not canonical: {value}")
    return pure.parts


def _relative_to_root(root: pathlib.Path, path: pathlib.Path, label: str) -> str:
    root_abs = pathlib.Path(os.path.abspath(str(root)))
    path_abs = pathlib.Path(os.path.abspath(str(path)))
    try:
        relative = path_abs.relative_to(root_abs).as_posix()
    except ValueError:
        fail(f"{label} must be lexically inside its selection root: {path}")
    _validate_relative_path(relative, label)
    return relative


def _is_windows_reparse(stat_result: os.stat_result) -> bool:
    attributes = getattr(stat_result, "st_file_attributes", 0)
    reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse)


def _validate_directory_root(path: pathlib.Path, label: str) -> pathlib.Path:
    root = pathlib.Path(os.path.abspath(str(path)))
    metadata = os.lstat(root)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_windows_reparse(metadata)
    ):
        fail(f"{label} must be a non-symlink directory: {root}")
    resolved = pathlib.Path(os.path.realpath(str(root)))
    if os.path.normcase(str(root)) != os.path.normcase(str(resolved)):
        fail(f"{label} path must not contain symlink or reparse-point aliases: {root}")
    return root


def _stable_file_signature(metadata: os.stat_result) -> Tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_nlink,
        metadata.st_size,
        getattr(metadata, "st_mtime_ns", int(metadata.st_mtime * 1_000_000_000)),
        getattr(metadata, "st_ctime_ns", int(metadata.st_ctime * 1_000_000_000)),
    )


def _opened_regular_metadata(stream: BinaryIO, label: str) -> os.stat_result:
    metadata = os.fstat(stream.fileno())
    _require_regular_single_link(metadata, label)
    return metadata


def _require_unchanged_open_file(
    stream: BinaryIO, before: os.stat_result, label: str
) -> None:
    after = _opened_regular_metadata(stream, label)
    if _stable_file_signature(before) != _stable_file_signature(after):
        fail(f"{label} changed while being snapshotted")


def _open_posix_regular(
    root: pathlib.Path, parts: Iterable[str], label: str
) -> BinaryIO:
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    directory = getattr(os, "O_DIRECTORY", 0)
    if nofollow == 0:
        fail(f"{label}: platform lacks O_NOFOLLOW")
    current_fd = os.open(str(root), os.O_RDONLY | directory | nofollow)
    try:
        part_list = list(parts)
        for component in part_list[:-1]:
            next_fd = os.open(
                component,
                os.O_RDONLY | directory | nofollow,
                dir_fd=current_fd,
            )
            os.close(current_fd)
            current_fd = next_fd
        candidate = os.stat(part_list[-1], dir_fd=current_fd, follow_symlinks=False)
        _require_regular_single_link(candidate, label)
        file_fd = os.open(part_list[-1], os.O_RDONLY | nofollow, dir_fd=current_fd)
    finally:
        os.close(current_fd)

    try:
        opened = os.fstat(file_fd)
        _require_regular_single_link(opened, label)
        if (candidate.st_dev, candidate.st_ino) != (opened.st_dev, opened.st_ino):
            fail(f"{label} changed while it was being opened")
        return os.fdopen(file_fd, "rb", closefd=True)
    except BaseException:
        os.close(file_fd)
        raise


def _normalise_windows_final_path(value: str) -> pathlib.Path:
    if value.startswith("\\\\?\\UNC\\"):
        value = "\\\\" + value[8:]
    elif value.startswith("\\\\?\\"):
        value = value[4:]
    return pathlib.Path(os.path.abspath(value))


def _open_windows_regular(
    root: pathlib.Path, parts: Iterable[str], label: str
) -> BinaryIO:
    # Component lstat rejects ordinary symlinks/junctions.  The final handle is
    # additionally opened as the reparse point itself, share-denying writes and
    # deletes, and its resolved handle path is checked under the repository.
    cursor = root
    root_stat = os.lstat(cursor)
    if _is_windows_reparse(root_stat):
        fail(f"{label}: repository root must not be a Windows reparse point")
    part_list = list(parts)
    for component in part_list:
        cursor = cursor / component
        component_stat = os.lstat(cursor)
        if _is_windows_reparse(component_stat) or stat.S_ISLNK(component_stat.st_mode):
            fail(f"{label} must not contain symlink or reparse-point components")
    candidate = os.lstat(cursor)
    _require_regular_single_link(candidate, label)

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = [
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    ]
    create_file.restype = ctypes.c_void_p
    handle = create_file(
        str(cursor),
        0x80000000,  # GENERIC_READ
        0x00000001,  # FILE_SHARE_READ; deny concurrent write/delete/rename
        None,
        3,  # OPEN_EXISTING
        0x00200000 | 0x08000000,  # OPEN_REPARSE_POINT | SEQUENTIAL_SCAN
        None,
    )
    invalid = ctypes.c_void_p(-1).value
    if handle in (None, invalid):
        error = ctypes.get_last_error()
        raise OSError(error, f"{label}: CreateFileW failed", str(cursor))

    try:

        class FileAttributeTagInfo(ctypes.Structure):
            _fields_ = [
                ("file_attributes", ctypes.c_uint32),
                ("reparse_tag", ctypes.c_uint32),
            ]

        info = FileAttributeTagInfo()
        get_info = kernel32.GetFileInformationByHandleEx
        get_info.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_uint32,
        ]
        get_info.restype = ctypes.c_int
        if not get_info(handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            raise ctypes.WinError(ctypes.get_last_error())
        if info.file_attributes & 0x00000400:
            fail(f"{label} final component is a Windows reparse point")
        if info.file_attributes & 0x00000010:
            fail(f"{label} must be a regular file, not a directory")

        get_final = kernel32.GetFinalPathNameByHandleW
        get_final.argtypes = [
            ctypes.c_void_p,
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
        ]
        get_final.restype = ctypes.c_uint32
        needed = get_final(handle, None, 0, 0)
        if needed == 0:
            raise ctypes.WinError(ctypes.get_last_error())
        buffer = ctypes.create_unicode_buffer(needed + 1)
        if get_final(handle, buffer, len(buffer), 0) == 0:
            raise ctypes.WinError(ctypes.get_last_error())
        final_path = _normalise_windows_final_path(buffer.value)
        root_path = pathlib.Path(os.path.abspath(str(root)))
        common = os.path.normcase(os.path.commonpath((str(root_path), str(final_path))))
        if common != os.path.normcase(str(root_path)):
            fail(f"{label} resolved outside repository root")

        import msvcrt

        fd = msvcrt.open_osfhandle(handle, os.O_RDONLY)
        handle = None
        stream = os.fdopen(fd, "rb", closefd=True)
        opened = os.fstat(stream.fileno())
        if not stat.S_ISREG(opened.st_mode):
            stream.close()
            fail(f"{label} must be a regular file")
        _require_regular_single_link(opened, label)
        if (candidate.st_dev, candidate.st_ino) != (opened.st_dev, opened.st_ino):
            stream.close()
            fail(f"{label} changed while it was being opened")
        return stream
    finally:
        if handle not in (None, invalid):
            close_handle = kernel32.CloseHandle
            close_handle.argtypes = [ctypes.c_void_p]
            close_handle.restype = ctypes.c_int
            close_handle(handle)


def open_regular_no_follow(root: pathlib.Path, relative: str, label: str) -> BinaryIO:
    parts = _validate_relative_path(relative, label)
    root = pathlib.Path(os.path.abspath(str(root)))
    if os.name == "nt":
        return _open_windows_regular(root, parts, label)
    return _open_posix_regular(root, parts, label)


def _read_bounded(stream: BinaryIO, limit: int, label: str) -> bytes:
    value = stream.read(limit + 1)
    if len(value) > limit:
        fail(f"{label} exceeds {limit} bytes")
    return value


def parse_manifest_bytes(raw: bytes) -> Dict[str, str]:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        fail("build-input manifest must contain valid UTF-8 text")
    entries: Dict[str, str] = {}
    for line_number, raw_line in enumerate(text.splitlines(), 1):
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        if not raw_line.isascii():
            fail(f"build-input manifest data line {line_number} must be ASCII")
        fields = raw_line.split(None, 1)
        if len(fields) != 2 or not _valid_digest(fields[0]):
            fail(f"malformed build-input manifest line {line_number}")
        digest, relative = fields
        if relative != relative.strip():
            fail(f"non-canonical build-input path at line {line_number}")
        _validate_relative_path(relative, f"manifest path at line {line_number}")
        if relative in entries:
            fail(
                f"duplicate build-input manifest path at line {line_number}: {relative}"
            )
        entries[relative] = digest
    if not entries:
        fail("build-input manifest contains no entries")
    return entries


def _load_selection_policy(target: str) -> Tuple[str, Tuple[str, ...]]:
    source_path = pathlib.Path(__file__).with_name("source_closure.py")
    spec = importlib.util.spec_from_file_location(
        "dcentos_source_closure_policy", source_path
    )
    if spec is None or spec.loader is None:
        fail("cannot load source-closure build-input selection policy")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    blocked = module.BLOCKED_BUILD_INPUT_TARGETS.get(target)
    if blocked is not None:
        fail(blocked)
    selected = module.TARGET_BUILD_INPUTS.get(target)
    if selected is None:
        fail(f"target {target} has no explicit release build-input policy")
    return module.BUILD_INPUT_SELECTION_POLICY, tuple(selected)


def _mkdir_private(path: pathlib.Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.is_symlink():
        fail(f"snapshot destination contains a symlink: {path}")


def _copy_open_stream(
    source: BinaryIO, destination: pathlib.Path, expected_digest: str, label: str
) -> Tuple[str, int]:
    _mkdir_private(destination.parent)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    fd = os.open(str(destination), flags, 0o600)
    digest = hashlib.sha256()
    size = 0
    try:
        with os.fdopen(fd, "wb", closefd=True) as output:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                output.write(chunk)
                digest.update(chunk)
                size += len(chunk)
            output.flush()
            os.fsync(output.fileno())
    except BaseException:
        try:
            destination.unlink()
        except FileNotFoundError:
            pass
        raise
    actual = digest.hexdigest()
    if actual != expected_digest:
        fail(
            f"{label} SHA256 mismatch while snapshotting: "
            f"expected {expected_digest}, copied {actual}"
        )
    return actual, size


def _write_exclusive(path: pathlib.Path, value: bytes, mode: int = 0o600) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    fd = os.open(str(path), flags, mode)
    with os.fdopen(fd, "wb", closefd=True) as stream:
        stream.write(value)
        stream.flush()
        os.fsync(stream.fileno())


def _fsync_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def _seal_stage(stage: pathlib.Path) -> None:
    directories: List[pathlib.Path] = []
    for directory, dirnames, filenames in os.walk(stage, followlinks=False):
        current = pathlib.Path(directory)
        directories.append(current)
        for name in tuple(dirnames) + tuple(filenames):
            child = current / name
            child_stat = os.lstat(child)
            if stat.S_ISLNK(child_stat.st_mode) or _is_windows_reparse(child_stat):
                fail(
                    f"snapshot unexpectedly contains a symlink or reparse point: {child}"
                )
        if os.name == "posix":
            for filename in filenames:
                os.chmod(current / filename, 0o400)
    if os.name == "posix":
        for directory in reversed(directories):
            os.chmod(directory, 0o500)


def _expected_tree(descriptor: Dict[str, Any]) -> Tuple[set[str], set[str]]:
    files = {SENTINEL_NAME, SNAPSHOT_NAME, descriptor["manifest"]["staged_path"]}
    files.update(item["staged_path"] for item in descriptor["files"])
    directories = {"."}
    for relative in files:
        pure = pathlib.PurePosixPath(relative)
        for parent in pure.parents:
            if parent.as_posix() != ".":
                directories.add(parent.as_posix())
    return files, directories


def _inspect_exact_tree(
    stage: pathlib.Path, descriptor: Dict[str, Any]
) -> Tuple[set[str], set[str]]:
    expected_files, expected_directories = _expected_tree(descriptor)
    actual_files: set[str] = set()
    actual_directories = {"."}
    for directory, dirnames, filenames in os.walk(stage, followlinks=False):
        current = pathlib.Path(directory)
        relative_current = current.relative_to(stage)
        current_name = (
            "." if not relative_current.parts else relative_current.as_posix()
        )
        current_stat = os.lstat(current)
        if (
            not stat.S_ISDIR(current_stat.st_mode)
            or stat.S_ISLNK(current_stat.st_mode)
            or _is_windows_reparse(current_stat)
        ):
            fail(f"snapshot tree contains an unsafe directory: {current_name}")
        for name in list(dirnames):
            child = current / name
            child_stat = os.lstat(child)
            child_relative = child.relative_to(stage).as_posix()
            if (
                not stat.S_ISDIR(child_stat.st_mode)
                or stat.S_ISLNK(child_stat.st_mode)
                or _is_windows_reparse(child_stat)
            ):
                fail(
                    f"snapshot tree contains an unsafe directory entry: {child_relative}"
                )
            actual_directories.add(child_relative)
        for name in filenames:
            child = current / name
            child_relative = child.relative_to(stage).as_posix()
            child_stat = os.lstat(child)
            _require_regular_single_link(
                child_stat, f"snapshot tree file {child_relative}"
            )
            actual_files.add(child_relative)
    if actual_files != expected_files or actual_directories != expected_directories:
        fail(
            "snapshot on-disk tree is not exact "
            f"(missing_files={sorted(expected_files - actual_files)}, "
            f"extra_files={sorted(actual_files - expected_files)}, "
            f"missing_dirs={sorted(expected_directories - actual_directories)}, "
            f"extra_dirs={sorted(actual_directories - expected_directories)})"
        )
    return expected_files, expected_directories


def _remove_exact_tree(stage: pathlib.Path, descriptor: Dict[str, Any]) -> None:
    """Remove only the descriptor-derived exact tree; never traverse discoveries."""

    expected_files, expected_directories = _inspect_exact_tree(stage, descriptor)
    # Files need no chmod on POSIX: unlink permission belongs to the parent
    # directory. Windows stages are deliberately not marked read-only because
    # chmod is not an access-control boundary there.
    if os.name == "posix":
        flags = (
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
        )

        def open_stage_directory(relative: str) -> int:
            descriptor_fd = os.open(stage, flags)
            try:
                if relative != ".":
                    for component in pathlib.PurePosixPath(relative).parts:
                        next_fd = os.open(component, flags, dir_fd=descriptor_fd)
                        os.close(descriptor_fd)
                        descriptor_fd = next_fd
                return descriptor_fd
            except BaseException:
                os.close(descriptor_fd)
                raise

        for relative in sorted(expected_files, key=lambda item: item.encode("utf-8")):
            pure = pathlib.PurePosixPath(relative)
            parent = "." if pure.parent.as_posix() == "." else pure.parent.as_posix()
            parent_fd = open_stage_directory(parent)
            try:
                os.fchmod(parent_fd, 0o700)
                metadata = os.stat(pure.name, dir_fd=parent_fd, follow_symlinks=False)
                _require_regular_single_link(
                    metadata, f"snapshot cleanup file {relative}"
                )
                os.unlink(pure.name, dir_fd=parent_fd)
            finally:
                os.close(parent_fd)
        for relative in sorted(
            (item for item in expected_directories if item != "."),
            key=lambda item: (-item.count("/"), item.encode("utf-8")),
        ):
            pure = pathlib.PurePosixPath(relative)
            parent = "." if pure.parent.as_posix() == "." else pure.parent.as_posix()
            parent_fd = open_stage_directory(parent)
            try:
                os.fchmod(parent_fd, 0o700)
                metadata = os.stat(pure.name, dir_fd=parent_fd, follow_symlinks=False)
                if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
                    fail(f"snapshot cleanup directory became unsafe: {relative}")
                os.rmdir(pure.name, dir_fd=parent_fd)
            finally:
                os.close(parent_fd)
        root_fd = open_stage_directory(".")
        try:
            os.fchmod(root_fd, 0o700)
        finally:
            os.close(root_fd)
    else:
        for relative in sorted(expected_files, key=lambda item: item.encode("utf-8")):
            path = stage.joinpath(*pathlib.PurePosixPath(relative).parts)
            metadata = os.lstat(path)
            _require_regular_single_link(metadata, f"snapshot cleanup file {relative}")
            path.unlink()
        for relative in sorted(
            (item for item in expected_directories if item != "."),
            key=lambda item: (-item.count("/"), item.encode("utf-8")),
        ):
            path = stage.joinpath(*pathlib.PurePosixPath(relative).parts)
            metadata = os.lstat(path)
            if (
                not stat.S_ISDIR(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or _is_windows_reparse(metadata)
            ):
                fail(f"snapshot cleanup directory became unsafe: {relative}")
            path.rmdir()
    stage_metadata = os.lstat(stage)
    if (
        not stat.S_ISDIR(stage_metadata.st_mode)
        or stat.S_ISLNK(stage_metadata.st_mode)
        or _is_windows_reparse(stage_metadata)
    ):
        fail("snapshot cleanup stage became unsafe")
    stage.rmdir()


def _remove_partial_known_stage(
    stage: pathlib.Path, created_files: Iterable[pathlib.Path]
) -> None:
    """Best-effort cleanup of only creator-recorded paths; tampering leaks safely."""

    recorded: List[Tuple[str, ...]] = []
    try:
        for path in set(created_files):
            try:
                relative = path.relative_to(stage).as_posix()
            except ValueError:
                fail(f"partial snapshot cleanup path escaped its stage: {path}")
            recorded.append(
                _validate_relative_path(relative, "partial snapshot cleanup path")
            )

        if os.name == "posix":
            nofollow = getattr(os, "O_NOFOLLOW", 0)
            directory = getattr(os, "O_DIRECTORY", 0)
            if nofollow == 0 or directory == 0:
                fail("partial snapshot cleanup requires O_NOFOLLOW and O_DIRECTORY")
            flags = os.O_RDONLY | directory | nofollow | getattr(os, "O_CLOEXEC", 0)
            parent_fd = os.open(stage.parent, flags)
            stage_fd = -1
            try:
                stage_fd = os.open(stage.name, flags, dir_fd=parent_fd)
                opened_stage = os.fstat(stage_fd)
                if not stat.S_ISDIR(opened_stage.st_mode):
                    fail("partial snapshot stage is not a directory")
                os.fchmod(stage_fd, 0o700)

                def open_recorded_directory(parts: Tuple[str, ...]) -> int:
                    current_fd = os.dup(stage_fd)
                    try:
                        for component in parts:
                            next_fd = os.open(component, flags, dir_fd=current_fd)
                            os.close(current_fd)
                            current_fd = next_fd
                        return current_fd
                    except BaseException:
                        os.close(current_fd)
                        raise

                for parts in sorted(
                    recorded, key=lambda item: (len(item), item), reverse=True
                ):
                    parent_descriptor = open_recorded_directory(parts[:-1])
                    try:
                        os.fchmod(parent_descriptor, 0o700)
                        try:
                            metadata = os.stat(
                                parts[-1],
                                dir_fd=parent_descriptor,
                                follow_symlinks=False,
                            )
                        except FileNotFoundError:
                            continue
                        _require_regular_single_link(
                            metadata, f"partial snapshot file {'/'.join(parts)}"
                        )
                        os.unlink(parts[-1], dir_fd=parent_descriptor)
                    finally:
                        os.close(parent_descriptor)

                directories = {
                    parts[:index]
                    for parts in recorded
                    for index in range(1, len(parts))
                }
                for parts in sorted(
                    directories, key=lambda item: (len(item), item), reverse=True
                ):
                    parent_descriptor = open_recorded_directory(parts[:-1])
                    try:
                        os.fchmod(parent_descriptor, 0o700)
                        metadata = os.stat(
                            parts[-1], dir_fd=parent_descriptor, follow_symlinks=False
                        )
                        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(
                            metadata.st_mode
                        ):
                            fail(
                                "partial snapshot cleanup directory became unsafe: "
                                + "/".join(parts)
                            )
                        os.rmdir(parts[-1], dir_fd=parent_descriptor)
                    finally:
                        os.close(parent_descriptor)

                path_stage = os.stat(
                    stage.name, dir_fd=parent_fd, follow_symlinks=False
                )
                if (path_stage.st_dev, path_stage.st_ino) != (
                    opened_stage.st_dev,
                    opened_stage.st_ino,
                ):
                    fail("partial snapshot stage path changed during cleanup")
                os.rmdir(stage.name, dir_fd=parent_fd)
            finally:
                if stage_fd >= 0:
                    os.close(stage_fd)
                os.close(parent_fd)
        else:
            # Windows lacks portable dirfd-relative unlink. Refuse every
            # symlink/reparse component before touching a recorded leaf; a race
            # or discovered redirection therefore leaks the private stage.
            stage_metadata = os.lstat(stage)
            if (
                not stat.S_ISDIR(stage_metadata.st_mode)
                or stat.S_ISLNK(stage_metadata.st_mode)
                or _is_windows_reparse(stage_metadata)
            ):
                fail("partial snapshot stage is unsafe")
            for parts in sorted(
                recorded, key=lambda item: (len(item), item), reverse=True
            ):
                cursor = stage
                for component in parts[:-1]:
                    cursor /= component
                    metadata = os.lstat(cursor)
                    if (
                        not stat.S_ISDIR(metadata.st_mode)
                        or stat.S_ISLNK(metadata.st_mode)
                        or _is_windows_reparse(metadata)
                    ):
                        fail(
                            f"partial snapshot cleanup directory became unsafe: {cursor}"
                        )
                leaf = cursor / parts[-1]
                try:
                    metadata = os.lstat(leaf)
                except FileNotFoundError:
                    continue
                _require_regular_single_link(metadata, f"partial snapshot file {leaf}")
                leaf.unlink()
            directories = {
                parts[:index] for parts in recorded for index in range(1, len(parts))
            }
            for parts in sorted(
                directories, key=lambda item: (len(item), item), reverse=True
            ):
                directory_path = stage.joinpath(*parts)
                metadata = os.lstat(directory_path)
                if (
                    not stat.S_ISDIR(metadata.st_mode)
                    or stat.S_ISLNK(metadata.st_mode)
                    or _is_windows_reparse(metadata)
                ):
                    fail(
                        "partial snapshot cleanup directory became unsafe: "
                        f"{directory_path}"
                    )
                directory_path.rmdir()
            stage.rmdir()
    except (OSError, SnapshotError):
        # The safe failure mode is a private leaked stage, never recursive
        # traversal of content that changed outside this creator's records.
        return


def create_snapshot(
    repo_root: pathlib.Path,
    manifest_path: pathlib.Path,
    target: str,
    stage_parent: Optional[pathlib.Path] = None,
    after_manifest_open: Hook = None,
    after_input_open: Hook = None,
    selection_root: Optional[pathlib.Path] = None,
) -> CreatedSnapshot:
    root = _validate_directory_root(repo_root, "repository payload root")
    split_authority = selection_root is not None
    manifest_root = (
        _validate_directory_root(selection_root, "build-input selection root")
        if split_authority
        else root
    )
    if split_authority:
        try:
            roots_alias = os.path.samefile(root, manifest_root)
        except OSError:
            roots_alias = False
        if roots_alias:
            fail(
                "repository payload root and build-input selection root must not alias"
            )
    manifest_relative = _relative_to_root(
        manifest_root, manifest_path, "build-input manifest"
    )
    policy, selected = _load_selection_policy(target)

    parent = pathlib.Path(stage_parent or tempfile.gettempdir())
    parent = pathlib.Path(os.path.abspath(str(parent)))
    parent_stat = os.lstat(parent)
    if (
        not stat.S_ISDIR(parent_stat.st_mode)
        or stat.S_ISLNK(parent_stat.st_mode)
        or _is_windows_reparse(parent_stat)
    ):
        fail(f"snapshot parent must be a non-symlink directory: {parent}")
    stage = pathlib.Path(tempfile.mkdtemp(prefix=STAGE_PREFIX, dir=str(parent)))
    os.chmod(stage, 0o700)
    owner_token = secrets.token_hex(32)
    created_files: List[pathlib.Path] = []
    try:
        with open_regular_no_follow(
            manifest_root, manifest_relative, "build-input manifest"
        ) as manifest:
            manifest_before = _opened_regular_metadata(manifest, "build-input manifest")
            if after_manifest_open is not None:
                after_manifest_open(manifest_relative, manifest)
            manifest_raw = _read_bounded(
                manifest, MAX_MANIFEST_BYTES, "build-input manifest"
            )
            _require_unchanged_open_file(
                manifest, manifest_before, "build-input manifest"
            )
        declared = parse_manifest_bytes(manifest_raw)
        manifest_stage = (
            pathlib.PurePosixPath("manifest")
            / pathlib.PurePosixPath(manifest_relative).name
        )
        manifest_destination = stage.joinpath(*manifest_stage.parts)
        _mkdir_private(manifest_destination.parent)
        created_files.append(manifest_destination)
        _write_exclusive(manifest_destination, manifest_raw)
        manifest_evidence = {
            "path": manifest_relative,
            "sha256": sha256_bytes(manifest_raw),
            "size": len(manifest_raw),
            "staged_path": manifest_stage.as_posix(),
        }

        files = []
        for relative in sorted(selected, key=lambda item: item.encode("utf-8")):
            expected = declared.get(relative)
            if expected is None:
                fail(
                    f"target {target} requires an input absent from the manifest: {relative}"
                )
            staged_relative = pathlib.PurePosixPath("files") / pathlib.PurePosixPath(
                relative
            )
            destination = stage.joinpath(*staged_relative.parts)
            created_files.append(destination)
            with open_regular_no_follow(
                root, relative, "out-of-band build input"
            ) as source:
                source_before = _opened_regular_metadata(
                    source, "out-of-band build input"
                )
                if after_input_open is not None:
                    after_input_open(relative, source)
                actual, size = _copy_open_stream(
                    source, destination, expected, f"out-of-band build input {relative}"
                )
                _require_unchanged_open_file(
                    source, source_before, f"out-of-band build input {relative}"
                )
            files.append(
                {
                    "path": relative,
                    "sha256": actual,
                    "size": size,
                    "staged_path": staged_relative.as_posix(),
                }
            )

        body = {
            "schema": SPLIT_AUTHORITY_SCHEMA if split_authority else SCHEMA,
            "target": target,
            "selection_policy": policy,
            "manifest": manifest_evidence,
            "files": files,
            "scope": {
                "claim": SNAPSHOT_CLAIM,
                "does_not_claim": list(SNAPSHOT_NON_CLAIMS),
            },
        }
        if split_authority:
            body["selection_root"] = {"kind": SELECTION_ROOT_KIND}
        snapshot_id = sha256_bytes(canonical_bytes(body))
        descriptor = dict(body)
        descriptor["snapshot_id"] = snapshot_id
        snapshot_path = stage / SNAPSHOT_NAME
        created_files.append(snapshot_path)
        _write_exclusive(snapshot_path, canonical_bytes(descriptor))
        sentinel = {
            "schema": OWNER_SCHEMA,
            "destroy_token_sha256": sha256_bytes(owner_token.encode("ascii")),
            "snapshot_id": snapshot_id,
        }
        sentinel_path = stage / SENTINEL_NAME
        created_files.append(sentinel_path)
        _write_exclusive(sentinel_path, canonical_bytes(sentinel))
        _fsync_directory(manifest_destination.parent)
        for item in files:
            _fsync_directory(
                stage.joinpath(*pathlib.PurePosixPath(item["staged_path"]).parts).parent
            )
        _fsync_directory(stage)
        _seal_stage(stage)
        _inspect_exact_tree(stage, descriptor)
        return CreatedSnapshot(
            snapshot=snapshot_path,
            snapshot_id=snapshot_id,
            destroy_token=owner_token,
            files=tuple(dict(item) for item in files),
        )
    except BaseException:
        _remove_partial_known_stage(stage, created_files)
        raise


def _descriptor_without_id(descriptor: Dict[str, Any]) -> Dict[str, Any]:
    body = dict(descriptor)
    body.pop("snapshot_id", None)
    return body


def _file_evidence(item: Dict[str, Any]) -> Dict[str, Any]:
    return {key: item[key] for key in ("path", "sha256", "size")}


def snapshot_evidence(descriptor: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "manifest": _file_evidence(descriptor["manifest"]),
        "selection_policy": descriptor["selection_policy"],
        "files": [_file_evidence(item) for item in descriptor["files"]],
        "snapshot": {
            "snapshot_id": descriptor["snapshot_id"],
            "target": descriptor["target"],
            "claim": descriptor["scope"]["claim"],
        },
    }


def verify_audit_descriptor(
    descriptor_path: pathlib.Path, expected_target: Optional[str] = None
) -> Dict[str, Any]:
    """Validate a retained descriptor without asserting live payload authority."""

    path = pathlib.Path(os.path.abspath(str(descriptor_path)))
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_windows_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
        or metadata.st_size > MAX_MANIFEST_BYTES
    ):
        fail(
            "retained build-input descriptor must be a bounded single-link regular file"
        )
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor_fd = os.open(path, flags)
    try:
        before = os.fstat(descriptor_fd)
        raw = b""
        while len(raw) <= MAX_MANIFEST_BYTES:
            chunk = os.read(
                descriptor_fd,
                min(64 * 1024, MAX_MANIFEST_BYTES + 1 - len(raw)),
            )
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor_fd)
    finally:
        os.close(descriptor_fd)
    current = os.lstat(path)
    if (
        len(raw) > MAX_MANIFEST_BYTES
        or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)
    ):
        fail("retained build-input descriptor changed while being read")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"retained build-input descriptor is not valid JSON: {error}")
    descriptor = _validate_descriptor(value)
    if raw != canonical_bytes(descriptor):
        fail("retained build-input descriptor is not canonical JSON")
    if expected_target is not None and descriptor["target"] != expected_target:
        fail("retained build-input descriptor target does not match")
    if descriptor["snapshot_id"] != sha256_bytes(
        canonical_bytes(_descriptor_without_id(descriptor))
    ):
        fail("retained build-input descriptor id does not bind its canonical fields")
    return descriptor


def _validate_descriptor(descriptor: object) -> Dict[str, Any]:
    if not isinstance(descriptor, dict):
        fail("snapshot descriptor must be an object")
    schema = descriptor.get("schema")
    top_keys = (
        (
            "schema",
            "target",
            "selection_policy",
            "manifest",
            "files",
            "scope",
            "snapshot_id",
        )
        if schema == SCHEMA
        else (
            "schema",
            "target",
            "selection_policy",
            "manifest",
            "files",
            "scope",
            "selection_root",
            "snapshot_id",
        )
    )
    top = _require_exact_object(
        descriptor,
        "snapshot descriptor",
        top_keys,
    )
    if top["schema"] not in (SCHEMA, SPLIT_AUTHORITY_SCHEMA):
        fail("unsupported build-input snapshot schema")
    if top["schema"] == SPLIT_AUTHORITY_SCHEMA:
        selection = _require_exact_object(
            top["selection_root"], "snapshot selection root", ("kind",)
        )
        if selection["kind"] != SELECTION_ROOT_KIND:
            fail("snapshot selection root kind is invalid")
    if not isinstance(top["target"], str) or not top["target"]:
        fail("snapshot target must be a non-empty string")
    if not isinstance(top["selection_policy"], str) or not top["selection_policy"]:
        fail("snapshot selection policy must be a non-empty string")
    manifest = _require_exact_object(
        top["manifest"],
        "snapshot manifest entry",
        ("path", "sha256", "size", "staged_path"),
    )
    files = top["files"]
    if not isinstance(files, list):
        fail("snapshot files must be an array")
    for index, item in enumerate(files):
        _require_exact_object(
            item,
            f"snapshot file entry {index}",
            ("path", "sha256", "size", "staged_path"),
        )
    scope = _require_exact_object(
        top["scope"], "snapshot scope", ("claim", "does_not_claim")
    )
    if scope != {
        "claim": SNAPSHOT_CLAIM,
        "does_not_claim": list(SNAPSHOT_NON_CLAIMS),
    }:
        fail("snapshot scope is invalid or overstates evidence")
    if not _valid_digest(top["snapshot_id"]):
        fail("build-input snapshot id is invalid")

    entries = [("manifest", manifest)] + [
        (f"file {index}", item) for index, item in enumerate(files)
    ]
    staged_paths: List[str] = []
    source_paths: List[str] = []
    for label, item in entries:
        if not isinstance(item["path"], str):
            fail(f"snapshot {label} source path is missing")
        _validate_relative_path(item["path"], f"snapshot {label} source path")
        if not isinstance(item["staged_path"], str):
            fail(f"snapshot {label} staged path is missing")
        _validate_relative_path(item["staged_path"], f"snapshot {label} staged path")
        if not _valid_digest(item["sha256"]):
            fail(f"snapshot {label} digest is invalid")
        _require_size(item["size"], f"snapshot {label} size")
        staged_paths.append(item["staged_path"])
        source_paths.append(item["path"])
    if len(set(staged_paths)) != len(staged_paths):
        fail("snapshot staged paths must be unique")
    if len(set(source_paths)) != len(source_paths):
        fail("snapshot source paths must be unique")
    reserved = {SENTINEL_NAME, SNAPSHOT_NAME}
    if set(staged_paths) & reserved:
        fail("snapshot staged paths collide with reserved control files")
    pure_staged = [pathlib.PurePosixPath(item) for item in staged_paths]
    for index, left in enumerate(pure_staged):
        for right in pure_staged[index + 1 :]:
            if left in right.parents or right in left.parents:
                fail("snapshot staged file paths must be disjoint")

    expected_manifest_stage = (
        pathlib.PurePosixPath("manifest") / pathlib.PurePosixPath(manifest["path"]).name
    ).as_posix()
    if manifest["staged_path"] != expected_manifest_stage:
        fail("snapshot manifest staged path is not canonical")
    for item in files:
        expected = (
            pathlib.PurePosixPath("files") / pathlib.PurePosixPath(item["path"])
        ).as_posix()
        if item["staged_path"] != expected:
            fail(f"snapshot file staged path is not canonical: {item['path']}")
    return top


def _read_owner_sentinel(stage: pathlib.Path, snapshot_id: str) -> Dict[str, Any]:
    with open_regular_no_follow(
        stage, SENTINEL_NAME, "snapshot owner sentinel"
    ) as stream:
        raw = _read_bounded(stream, 4096, "snapshot owner sentinel")
    try:
        sentinel = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"snapshot owner sentinel is not valid JSON: {error}")
    sentinel = _require_exact_object(
        sentinel,
        "snapshot owner sentinel",
        ("schema", "destroy_token_sha256", "snapshot_id"),
    )
    if raw != canonical_bytes(sentinel):
        fail("snapshot owner sentinel is not canonical JSON")
    if sentinel["schema"] != OWNER_SCHEMA:
        fail("snapshot owner sentinel schema is invalid")
    if not _valid_digest(sentinel["destroy_token_sha256"]):
        fail("snapshot owner sentinel token digest is invalid")
    if sentinel["snapshot_id"] != snapshot_id:
        fail("snapshot owner sentinel does not bind the descriptor")
    return sentinel


def verify_snapshot(
    snapshot_path: pathlib.Path, expected_target: Optional[str] = None
) -> Dict[str, Any]:
    snapshot_path = pathlib.Path(os.path.abspath(str(snapshot_path)))
    stage = snapshot_path.parent
    if (
        stage.name.startswith(STAGE_PREFIX) is False
        or snapshot_path.name != SNAPSHOT_NAME
    ):
        fail("snapshot descriptor is not in an owned build-input stage")
    stage_stat = os.lstat(stage)
    if (
        not stat.S_ISDIR(stage_stat.st_mode)
        or stat.S_ISLNK(stage_stat.st_mode)
        or _is_windows_reparse(stage_stat)
    ):
        fail("snapshot stage must be a non-symlink directory")
    with open_regular_no_follow(stage, SNAPSHOT_NAME, "snapshot descriptor") as stream:
        raw = _read_bounded(stream, MAX_MANIFEST_BYTES, "snapshot descriptor")
    try:
        descriptor = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"snapshot descriptor is not valid JSON: {error}")
    descriptor = _validate_descriptor(descriptor)
    if raw != canonical_bytes(descriptor):
        fail("build-input snapshot descriptor is not canonical JSON")
    snapshot_id = descriptor.get("snapshot_id")
    if not _valid_digest(snapshot_id) or snapshot_id != sha256_bytes(
        canonical_bytes(_descriptor_without_id(descriptor))
    ):
        fail("build-input snapshot id is invalid")
    if expected_target is not None and descriptor.get("target") != expected_target:
        fail(
            f"build-input snapshot target mismatch: expected {expected_target}, "
            f"got {descriptor.get('target')}"
        )
    policy, selected = _load_selection_policy(str(descriptor.get("target", "")))
    if descriptor.get("selection_policy") != policy:
        fail("build-input snapshot selection policy changed")
    files = descriptor.get("files")
    manifest = descriptor.get("manifest")
    if [item.get("path") for item in files] != sorted(
        selected, key=lambda item: item.encode("utf-8")
    ):
        fail("build-input snapshot files do not match target selection policy")
    for label, item in [("manifest", manifest)] + [("file", value) for value in files]:
        relative = item.get("staged_path")
        with open_regular_no_follow(stage, relative, f"snapshot {label}") as stream:
            digest = hashlib.sha256()
            size = 0
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
                size += len(chunk)
        if digest.hexdigest() != item["sha256"] or size != item["size"]:
            fail(f"snapshot {label} bytes changed after staging")

    manifest_relative = manifest.get("staged_path")
    with open_regular_no_follow(
        stage, manifest_relative, "snapshot manifest"
    ) as stream:
        declared = parse_manifest_bytes(
            _read_bounded(stream, MAX_MANIFEST_BYTES, "snapshot manifest")
        )
    for item in files:
        if declared.get(item["path"]) != item["sha256"]:
            fail(f"snapshot file is not bound by staged manifest: {item['path']}")
    _read_owner_sentinel(stage, descriptor["snapshot_id"])
    _inspect_exact_tree(stage, descriptor)
    return descriptor


def destroy_snapshot(snapshot_path: pathlib.Path, destroy_token: str) -> None:
    if (
        not isinstance(destroy_token, str)
        or len(destroy_token) != 64
        or set(destroy_token) - HEX_64
    ):
        fail("snapshot destruction token is invalid")
    descriptor = verify_snapshot(snapshot_path)
    stage = pathlib.Path(os.path.abspath(str(snapshot_path))).parent
    sentinel = _read_owner_sentinel(stage, descriptor["snapshot_id"])
    if not secrets.compare_digest(
        sentinel["destroy_token_sha256"], sha256_bytes(destroy_token.encode("ascii"))
    ):
        fail("snapshot destruction token does not authorize this stage")
    _remove_exact_tree(stage, descriptor)


def _validate_cli_result(value: object) -> Dict[str, Any]:
    result = _require_exact_object(
        value,
        "snapshot creation result",
        ("destroy_token", "files", "snapshot", "snapshot_id", "stage"),
    )
    for key in ("destroy_token", "snapshot_id"):
        if not _valid_digest(result[key]):
            fail(f"snapshot creation result {key} is invalid")
    for key in ("snapshot", "stage"):
        if (
            not isinstance(result[key], str)
            or not result[key]
            or any(character in result[key] for character in ("\0", "\r", "\n"))
        ):
            fail(f"snapshot creation result {key} is unsafe for shell transport")
    files = result["files"]
    if not isinstance(files, list):
        fail("snapshot creation result files must be an array")
    for index, item in enumerate(files):
        item = _require_exact_object(
            item,
            f"snapshot creation result file {index}",
            ("path", "sha256", "size", "staged_path"),
        )
        for key in ("path", "staged_path"):
            _validate_relative_path(
                item[key], f"snapshot creation result file {index} {key}"
            )
        if not _valid_digest(item["sha256"]):
            fail(f"snapshot creation result file {index} digest is invalid")
        _require_size(item["size"], f"snapshot creation result file {index} size")
    return result


def query_cli_result(
    raw: bytes, field: Optional[str], file_path: Optional[str], attribute: Optional[str]
) -> str:
    # Windows text-mode pipes may translate the one record terminator. JSON
    # strings cannot contain a raw CR/LF, so this normalization cannot alter a
    # field value.
    raw = raw.replace(b"\r\n", b"\n")
    try:
        result = _validate_cli_result(json.loads(raw))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"snapshot creation result is not valid JSON: {error}")
    if raw != canonical_bytes(result):
        fail("snapshot creation result is not canonical JSON")
    if field is not None:
        value = result[field]
    else:
        matches = [item for item in result["files"] if item["path"] == file_path]
        if len(matches) != 1:
            fail(
                f"snapshot creation result does not contain exactly one file: {file_path}"
            )
        value = matches[0][attribute]
    if not isinstance(value, (str, int)) or isinstance(value, bool):
        fail("snapshot creation query did not select a scalar")
    text = str(value)
    if any(character in text for character in ("\0", "\r", "\n")):
        fail("snapshot creation query result is unsafe for shell transport")
    return text


def query_verified_snapshot(
    snapshot_path: pathlib.Path,
    target: str,
    field: Optional[str],
    file_path: Optional[str],
    attribute: Optional[str],
) -> str:
    """Return one scalar only after fully re-verifying the staged snapshot."""

    descriptor = verify_snapshot(snapshot_path, target)
    if descriptor.get("schema") != SPLIT_AUTHORITY_SCHEMA or descriptor.get(
        "selection_root"
    ) != {"kind": SELECTION_ROOT_KIND}:
        fail("query-snapshot requires a split-authority v2 snapshot")
    if field == "stage":
        value: object = os.fspath(
            pathlib.Path(os.path.abspath(str(snapshot_path))).parent
        )
    elif field == "snapshot_id":
        value = descriptor["snapshot_id"]
    elif field == "manifest_path":
        value = descriptor["manifest"]["path"]
    else:
        matches = [item for item in descriptor["files"] if item["path"] == file_path]
        if len(matches) != 1:
            fail(f"verified snapshot does not contain exactly one file: {file_path}")
        value = matches[0][attribute]
    if not isinstance(value, (str, int)) or isinstance(value, bool):
        fail("verified snapshot query did not select a scalar")
    text = str(value)
    if not text or any(character in text for character in ("\0", "\r", "\n")):
        fail("verified snapshot query result is unsafe for shell transport")
    return text


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    create = commands.add_parser(
        "create", help="create a private immutable input snapshot"
    )
    create.add_argument("--repo-root", required=True)
    create.add_argument("--build-input-manifest", required=True)
    create.add_argument(
        "--selection-root",
        help=(
            "separately verified root that owns manifest selection authority; "
            "omit only for legacy manifests under --repo-root"
        ),
    )
    create.add_argument("--target", required=True)
    create.add_argument("--stage-parent")
    verify = commands.add_parser("verify", help="verify a staged input snapshot")
    verify.add_argument("--target")
    verify.add_argument("snapshot")
    destroy = commands.add_parser(
        "destroy", help="safely remove an owned input snapshot"
    )
    destroy.add_argument("--token", required=True)
    destroy.add_argument("snapshot")
    query = commands.add_parser(
        "query-result",
        help="safely extract one scalar from canonical create JSON on stdin",
    )
    selection = query.add_mutually_exclusive_group(required=True)
    selection.add_argument(
        "--field", choices=("destroy_token", "snapshot", "snapshot_id", "stage")
    )
    selection.add_argument("--file")
    query.add_argument("--attribute", choices=("sha256", "size", "staged_path"))
    query_snapshot = commands.add_parser(
        "query-snapshot",
        help="fully verify a staged snapshot, then extract one safe scalar",
    )
    query_snapshot.add_argument("--target", required=True)
    snapshot_selection = query_snapshot.add_mutually_exclusive_group(required=True)
    snapshot_selection.add_argument(
        "--field", choices=("stage", "snapshot_id", "manifest_path")
    )
    snapshot_selection.add_argument("--file")
    query_snapshot.add_argument(
        "--attribute", choices=("sha256", "size", "staged_path")
    )
    query_snapshot.add_argument("snapshot")
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "create":
            created = create_snapshot(
                pathlib.Path(args.repo_root),
                pathlib.Path(args.build_input_manifest),
                args.target,
                pathlib.Path(args.stage_parent) if args.stage_parent else None,
                selection_root=(
                    pathlib.Path(args.selection_root) if args.selection_root else None
                ),
            )
            print(canonical_bytes(created.cli_result()).decode("ascii"), end="")
        elif args.command == "verify":
            descriptor = verify_snapshot(pathlib.Path(args.snapshot), args.target)
            print(
                f"build-input snapshot verified: target={descriptor['target']} "
                f"files={len(descriptor['files'])} id={descriptor['snapshot_id']}"
            )
        elif args.command == "destroy":
            destroy_snapshot(pathlib.Path(args.snapshot), args.token)
        elif args.command == "query-snapshot":
            if args.file is not None and args.attribute is None:
                fail("query-snapshot --file requires --attribute")
            if args.file is None and args.attribute is not None:
                fail("query-snapshot --attribute requires --file")
            print(
                query_verified_snapshot(
                    pathlib.Path(args.snapshot),
                    args.target,
                    args.field,
                    args.file,
                    args.attribute,
                )
            )
        else:
            if args.file is not None and args.attribute is None:
                fail("query-result --file requires --attribute")
            if args.file is None and args.attribute is not None:
                fail("query-result --attribute requires --file")
            raw = sys.stdin.buffer.read(MAX_MANIFEST_BYTES + 1)
            if len(raw) > MAX_MANIFEST_BYTES:
                fail("snapshot creation result is too large")
            print(query_cli_result(raw, args.field, args.file, args.attribute))
        return 0
    except (SnapshotError, OSError, KeyError, TypeError, ValueError) as error:
        print(f"ERROR: build-input snapshot: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
