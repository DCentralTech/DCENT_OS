#!/usr/bin/env python3
"""Materialize a capability-owned snapshot from one exact Git commit.

The working tree is never a byte source.  Commit, tree, and blob objects are
read with ``git cat-file --batch`` and every object is re-hashed before use.
Only regular Git blobs (100644/100755) are supported in v1; links, gitlinks,
and non-portable paths fail closed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import secrets
import stat
import subprocess
import sys
import tempfile
import unicodedata
from dataclasses import dataclass
from typing import (
    Any,
    BinaryIO,
    Callable,
    Dict,
    Iterable,
    List,
    Optional,
    Sequence,
    Tuple,
)


SCHEMA = "org.dcentral.dcentos.source-snapshot.v1"
OWNER_SCHEMA = "org.dcentral.dcentos.source-snapshot-owner.v1"
GIT_VERIFICATION_SCHEMA = "org.dcentral.dcentos.source-snapshot-git-verification.v1"
SNAPSHOT_NAME = "snapshot.json"
SENTINEL_NAME = ".dcentos-source-snapshot-owner"
STAGE_PREFIX = "dcentos-source-snapshot-"
SNAPSHOT_CLAIM = "exact-git-commit-tree-regular-blob-snapshot-not-build-execution-or-reproducibility-proof"
SNAPSHOT_NON_CLAIMS = (
    "live-working-tree-or-index-state",
    "untracked-ignored-symlink-or-submodule-content",
    "git-config-hooks-alternates-or-object-store-provenance",
    "dependency-toolchain-container-or-external-input-closure",
    "build-execution-causality-installed-payload-equivalence-or-reproducibility",
    "power-loss-durable-or-atomic-release-publication",
    "protection-against-same-uid-process-or-namespace-mutation",
)
HEX = frozenset("0123456789abcdef")
MAX_DESCRIPTOR_BYTES = 64 * 1024 * 1024
MAX_OBJECT_BYTES = 2 * 1024 * 1024 * 1024
MAX_FILES = 1_000_000
WINDOWS_RESERVED = {
    "con",
    "prn",
    "aux",
    "nul",
    *(f"com{i}" for i in range(1, 10)),
    *(f"lpt{i}" for i in range(1, 10)),
}
AfterGraphHook = Optional[Callable[[str], None]]
AfterMaterializeHook = Optional[Callable[[pathlib.Path], None]]


class SnapshotError(RuntimeError):
    """The requested source snapshot could not be proven safe."""


def fail(message: str) -> None:
    raise SnapshotError(message)


def canonical_bytes(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _is_digest(value: object, length: int = 64) -> bool:
    return isinstance(value, str) and len(value) == length and not (set(value) - HEX)


def _exact_object(value: object, label: str, keys: Sequence[str]) -> Dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(keys):
        fail(f"{label} must contain exactly: {', '.join(keys)}")
    return value


def _is_reparse(metadata: os.stat_result) -> bool:
    flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    return bool(flag and getattr(metadata, "st_file_attributes", 0) & flag)


def _require_real_directory(path: pathlib.Path, label: str) -> os.stat_result:
    metadata = os.lstat(path)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a non-link directory: {path}")
    return metadata


def _require_regular(metadata: os.stat_result, label: str) -> None:
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a regular non-link file")
    if metadata.st_nlink != 1:
        fail(f"{label} must have exactly one hard link")


def _validate_component(component: str, label: str) -> None:
    if not component or component in (".", ".."):
        fail(f"{label} contains an empty or traversal component")
    if unicodedata.normalize("NFC", component) != component:
        fail(f"{label} is not NFC-normalized")
    if component.casefold() == ".git":
        fail(f"{label} contains reserved Git metadata component .git")
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


def _portable_collision_key(path: str) -> Tuple[str, ...]:
    return tuple(component.casefold() for component in path.split("/"))


def _object_digest(object_format: str, object_type: str, raw: bytes) -> str:
    algorithm = {"sha1": "sha1", "sha256": "sha256"}.get(object_format)
    if algorithm is None:
        fail(f"unsupported Git object format: {object_format}")
    digest = hashlib.new(algorithm)
    digest.update(f"{object_type} {len(raw)}\0".encode("ascii"))
    digest.update(raw)
    return digest.hexdigest()


class GitObjects:
    def __init__(self, repo_root: pathlib.Path):
        self.root = pathlib.Path(os.path.abspath(os.fspath(repo_root)))
        _require_real_directory(self.root, "repository root")
        self.env = os.environ.copy()
        self.env.update(
            {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "GIT_NO_REPLACE_OBJECTS": "1",
                "GIT_OPTIONAL_LOCKS": "0",
                "LC_ALL": "C",
            }
        )
        top = (
            self._run(("rev-parse", "--show-toplevel"))
            .decode(sys.getfilesystemencoding(), "strict")
            .strip()
        )
        try:
            same_root = os.path.samefile(self.root, pathlib.Path(top))
        except OSError:
            same_root = False
        if not same_root:
            fail("repository root must be the exact Git top-level directory")
        self.object_format = (
            self._run(("rev-parse", "--show-object-format")).decode("ascii").strip()
        )
        self.oid_bytes = {"sha1": 20, "sha256": 32}.get(self.object_format, 0)
        if not self.oid_bytes:
            fail(f"unsupported Git object format: {self.object_format}")
        self.process: Optional[subprocess.Popen[bytes]] = None

    def _run(self, arguments: Sequence[str]) -> bytes:
        completed = subprocess.run(
            ("git", "-C", os.fspath(self.root), *arguments),
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode:
            message = completed.stderr.decode("utf-8", "replace").strip()
            fail(f"Git plumbing failed ({' '.join(arguments)}): {message}")
        return completed.stdout

    def __enter__(self) -> "GitObjects":
        self.process = subprocess.Popen(
            ("git", "-C", os.fspath(self.root), "cat-file", "--batch"),
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return self

    def __exit__(self, *_: object) -> None:
        if self.process is not None:
            if self.process.stdin:
                self.process.stdin.close()
            self.process.wait(timeout=10)
            if self.process.stdout:
                self.process.stdout.close()
            if self.process.stderr:
                self.process.stderr.close()
            self.process = None

    def read(self, oid: str, expected_type: str) -> bytes:
        return self.read_many(((oid, expected_type),))[0]

    def read_many(self, requests: Sequence[Tuple[str, str]]) -> List[bytes]:
        if (
            self.process is None
            or self.process.stdin is None
            or self.process.stdout is None
        ):
            fail("Git object reader is not active")
        if not requests:
            return []
        for oid, _ in requests:
            if not _is_digest(oid, self.oid_bytes * 2):
                fail(f"invalid exact Git object id: {oid}")
        results: List[bytes] = []
        # Bound each write below common pipe capacities.  Sending a whole
        # repository's requests before reading responses can deadlock when
        # cat-file blocks on its stdout while the parent blocks on stdin.
        for start in range(0, len(requests), 32):
            batch = requests[start : start + 32]
            self.process.stdin.write(
                b"".join(oid.encode("ascii") + b"\n" for oid, _ in batch)
            )
            self.process.stdin.flush()
            for oid, expected_type in batch:
                header = self.process.stdout.readline(256)
                if not header.endswith(b"\n") or len(header) >= 256:
                    fail("Git cat-file returned an invalid object header")
                fields = header[:-1].split(b" ")
                if len(fields) == 2 and fields[1] == b"missing":
                    fail(f"Git object is missing: {oid}")
                if len(fields) != 3:
                    fail("Git cat-file returned a malformed object header")
                returned_oid, object_type, size_raw = fields
                if (
                    returned_oid.decode("ascii", "strict") != oid
                    or object_type.decode("ascii", "strict") != expected_type
                ):
                    fail(f"Git object {oid} is not the expected {expected_type}")
                try:
                    size = int(size_raw)
                except ValueError:
                    fail("Git cat-file returned a non-integer object size")
                if size < 0 or size > MAX_OBJECT_BYTES:
                    fail(f"Git object has an unsafe size: {size}")
                raw = self.process.stdout.read(size)
                terminator = self.process.stdout.read(1)
                if len(raw) != size or terminator != b"\n":
                    fail("Git cat-file truncated an object")
                if _object_digest(self.object_format, expected_type, raw) != oid:
                    fail(f"Git object bytes do not match their id: {oid}")
                results.append(raw)
        return results


def _commit_tree(raw: bytes, oid_bytes: int) -> str:
    header = raw.split(b"\n\n", 1)[0]
    trees = [line[5:] for line in header.splitlines() if line.startswith(b"tree ")]
    if len(trees) != 1 or not header.startswith(b"tree "):
        fail("commit object does not contain exactly one leading tree header")
    try:
        tree = trees[0].decode("ascii", "strict")
    except UnicodeDecodeError:
        fail("commit tree id is not ASCII")
    if not _is_digest(tree, oid_bytes * 2):
        fail("commit tree id is invalid")
    return tree


def _parse_tree(raw: bytes, oid_bytes: int, prefix: str) -> List[Tuple[str, str, str]]:
    entries: List[Tuple[str, str, str]] = []
    seen_names: set[bytes] = set()
    offset = 0
    while offset < len(raw):
        space = raw.find(b" ", offset)
        nul = raw.find(b"\0", space + 1) if space >= 0 else -1
        if space < 0 or nul < 0 or nul + 1 + oid_bytes > len(raw):
            fail("tree object contains a truncated entry")
        mode_raw = raw[offset:space]
        name_raw = raw[space + 1 : nul]
        oid_raw = raw[nul + 1 : nul + 1 + oid_bytes]
        offset = nul + 1 + oid_bytes
        if name_raw in seen_names:
            fail("tree object contains a duplicate entry name")
        seen_names.add(name_raw)
        try:
            mode = mode_raw.decode("ascii", "strict")
            name = name_raw.decode("utf-8", "strict")
        except UnicodeDecodeError:
            fail("tree entry mode/name is not canonical ASCII/UTF-8")
        if name.encode("utf-8") != name_raw:
            fail("tree entry name is not canonical UTF-8")
        _validate_component(name, "Git tree path")
        path = f"{prefix}/{name}" if prefix else name
        oid = oid_raw.hex()
        normalized_mode = "040000" if mode == "40000" else mode
        entries.append((normalized_mode, oid, path))
    return entries


def _read_graph(
    git: GitObjects, commit_oid: str
) -> Tuple[str, bytes, List[Dict[str, Any]], List[Dict[str, Any]], Dict[str, bytes]]:
    commit_raw = git.read(commit_oid, "commit")
    root_tree = _commit_tree(commit_raw, git.oid_bytes)
    pending: List[Tuple[str, str]] = [("", root_tree)]
    directories: List[Dict[str, Any]] = []
    files: List[Dict[str, Any]] = []
    blobs: Dict[str, bytes] = {}
    paths: set[str] = set()
    portable: set[Tuple[str, ...]] = set()
    blob_entries: List[Tuple[str, str, str]] = []
    while pending:
        current = pending
        pending = []
        tree_objects = git.read_many(
            tuple((tree_oid, "tree") for _, tree_oid in current)
        )
        for (prefix, _), tree_raw in zip(current, tree_objects):
            for mode, oid, path in _parse_tree(tree_raw, git.oid_bytes, prefix):
                if path in paths or _portable_collision_key(path) in portable:
                    fail(
                        f"Git tree contains a duplicate or portable path collision: {path}"
                    )
                paths.add(path)
                portable.add(_portable_collision_key(path))
                if mode == "040000":
                    directories.append(
                        {
                            "path": path,
                            "git_mode": mode,
                            "tree_oid": oid,
                            "staged_path": f"tree/{path}",
                        }
                    )
                    pending.append((path, oid))
                elif mode in ("100644", "100755"):
                    if len(blob_entries) >= MAX_FILES:
                        fail("Git tree exceeds the source snapshot file-count bound")
                    blob_entries.append((path, mode, oid))
                elif mode == "120000":
                    fail(f"Git symlinks are unsupported by source snapshot v1: {path}")
                elif mode == "160000":
                    fail(
                        f"Gitlinks/submodules are unsupported by source snapshot v1: {path}"
                    )
                else:
                    fail(f"unsupported Git tree mode {mode} at {path}")
    unique_blob_oids = sorted({oid for _, _, oid in blob_entries})
    blob_objects = git.read_many(tuple((oid, "blob") for oid in unique_blob_oids))
    blobs_by_oid = dict(zip(unique_blob_oids, blob_objects))
    for path, mode, oid in blob_entries:
        raw = blobs_by_oid[oid]
        blobs[path] = raw
        files.append(
            {
                "path": path,
                "git_mode": mode,
                "blob_oid": oid,
                "sha256": sha256_bytes(raw),
                "size": len(raw),
                "staged_path": f"tree/{path}",
            }
        )
    directories.sort(key=lambda item: item["path"].encode("utf-8"))
    files.sort(key=lambda item: item["path"].encode("utf-8"))
    return root_tree, commit_raw, directories, files, blobs


def _mkdir(path: pathlib.Path) -> None:
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        _require_real_directory(path, "snapshot directory")


def _write_exclusive(path: pathlib.Path, raw: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_BINARY", 0)
    )
    descriptor = os.open(path, flags, 0o600)
    try:
        view = memoryview(raw)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"short write while materializing {path}")
            view = view[written:]
    finally:
        os.close(descriptor)


def _chmod_nofollow(path: pathlib.Path, mode: int) -> None:
    """chmod a proved non-link path on POSIX and Windows Python."""
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
        fail(f"refusing to chmod a link or reparse point: {path}")
    if os.name == "nt":
        os.chmod(path, mode)
    else:
        os.chmod(path, mode, follow_symlinks=False)


def _seal(
    stage: pathlib.Path,
    directories: Sequence[Dict[str, Any]],
    files: Sequence[Dict[str, Any]],
) -> None:
    for item in files:
        mode = 0o555 if item["git_mode"] == "100755" else 0o444
        _chmod_nofollow(stage.joinpath(*item["staged_path"].split("/")), mode)
    _chmod_nofollow(stage / SNAPSHOT_NAME, 0o400)
    _chmod_nofollow(stage / SENTINEL_NAME, 0o400)
    for item in sorted(
        directories, key=lambda value: value["path"].count("/"), reverse=True
    ):
        _chmod_nofollow(stage.joinpath(*item["staged_path"].split("/")), 0o500)
    _chmod_nofollow(stage / "tree", 0o500)
    _chmod_nofollow(stage, 0o500)


def _open_nofollow(stage: pathlib.Path, relative: str) -> BinaryIO:
    parts = _validate_relative(relative, "snapshot staged path")
    cursor = stage
    for component in parts[:-1]:
        cursor /= component
        _require_real_directory(cursor, "snapshot path component")
    path = cursor / parts[-1]
    before = os.lstat(path)
    _require_regular(before, f"snapshot file {relative}")
    descriptor = os.open(
        path,
        os.O_RDONLY
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_BINARY", 0),
    )
    try:
        opened = os.fstat(descriptor)
        after = os.lstat(path)
        _require_regular(opened, f"snapshot file {relative}")
        if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino) or (
            after.st_dev,
            after.st_ino,
        ) != (opened.st_dev, opened.st_ino):
            fail(f"snapshot file changed identity while opening: {relative}")
        return os.fdopen(descriptor, "rb", closefd=True)
    except BaseException:
        os.close(descriptor)
        raise


def _expected_tree(descriptor: Dict[str, Any]) -> Tuple[set[str], set[str]]:
    files = {
        SNAPSHOT_NAME,
        SENTINEL_NAME,
        *(item["staged_path"] for item in descriptor["files"]),
    }
    directories = {"tree", *(item["staged_path"] for item in descriptor["directories"])}
    for relative in list(files) + list(directories):
        pure = pathlib.PurePosixPath(relative)
        directories.update(
            parent.as_posix() for parent in pure.parents if parent.as_posix() != "."
        )
    return files, directories


def _inspect_exact_tree(stage: pathlib.Path, descriptor: Dict[str, Any]) -> None:
    stage_meta = _require_real_directory(stage, "source snapshot stage")
    expected_files, expected_dirs = _expected_tree(descriptor)
    actual_files: set[str] = set()
    actual_dirs: set[str] = set()
    for root, dirs, files in os.walk(stage, topdown=True, followlinks=False):
        root_path = pathlib.Path(root)
        for name in dirs:
            child = root_path / name
            metadata = os.lstat(child)
            if (
                not stat.S_ISDIR(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or _is_reparse(metadata)
            ):
                fail(f"snapshot tree contains an unsafe directory: {child}")
            relative = child.relative_to(stage).as_posix()
            _validate_relative(relative, "snapshot directory")
            actual_dirs.add(relative)
        for name in files:
            child = root_path / name
            metadata = os.lstat(child)
            _require_regular(metadata, f"snapshot tree file {child}")
            relative = child.relative_to(stage).as_posix()
            _validate_relative(relative, "snapshot file")
            actual_files.add(relative)
            if hasattr(os, "getuid") and metadata.st_uid != stage_meta.st_uid:
                fail(f"snapshot file ownership differs from its stage: {relative}")
    if actual_files != expected_files or actual_dirs != expected_dirs:
        fail("source snapshot on-disk tree is not exact")
    if os.name != "nt":
        for item in descriptor["files"]:
            metadata = os.lstat(stage.joinpath(*item["staged_path"].split("/")))
            expected = 0o555 if item["git_mode"] == "100755" else 0o444
            if stat.S_IMODE(metadata.st_mode) != expected:
                fail(f"snapshot executable classification changed: {item['path']}")


def _descriptor_body(
    object_format: str,
    commit_oid: str,
    commit_raw: bytes,
    root_tree: str,
    directories: List[Dict[str, Any]],
    files: List[Dict[str, Any]],
) -> Dict[str, Any]:
    return {
        "schema": SCHEMA,
        "object_format": object_format,
        "commit": {
            "oid": commit_oid,
            "sha256": sha256_bytes(commit_raw),
            "size": len(commit_raw),
            "tree_oid": root_tree,
        },
        "root_tree": {
            "git_mode": "040000",
            "staged_path": "tree",
            "tree_oid": root_tree,
        },
        "directories": directories,
        "files": files,
        "scope": {"claim": SNAPSHOT_CLAIM, "does_not_claim": list(SNAPSHOT_NON_CLAIMS)},
    }


@dataclass(frozen=True)
class CreatedSnapshot:
    snapshot: pathlib.Path
    snapshot_id: str
    destroy_token: str
    commit_oid: str
    files_count: int

    @property
    def stage(self) -> pathlib.Path:
        return self.snapshot.parent

    @property
    def tree(self) -> pathlib.Path:
        return self.stage / "tree"

    def cli_result(self) -> Dict[str, Any]:
        return {
            "commit_oid": self.commit_oid,
            "destroy_token": self.destroy_token,
            "files_count": self.files_count,
            "snapshot": os.fspath(self.snapshot),
            "snapshot_id": self.snapshot_id,
            "stage": os.fspath(self.stage),
            "tree": os.fspath(self.tree),
        }


def _remove_partial(
    stage: pathlib.Path,
    files: Iterable[pathlib.Path],
    directories: Iterable[pathlib.Path],
) -> None:
    """Remove only creator-recorded real entries; tampering deliberately leaks."""
    try:
        for path in reversed(list(files)):
            if path.exists() or path.is_symlink():
                metadata = os.lstat(path)
                _require_regular(metadata, "partial snapshot cleanup file")
                _chmod_nofollow(path, 0o600)
                path.unlink()
        for path in sorted(
            set(directories), key=lambda item: len(item.parts), reverse=True
        ):
            if path.exists():
                _require_real_directory(path, "partial snapshot cleanup directory")
                _chmod_nofollow(path, 0o700)
                path.rmdir()
        if stage.exists():
            _require_real_directory(stage, "partial snapshot stage")
            _chmod_nofollow(stage, 0o700)
            stage.rmdir()
    except (OSError, SnapshotError):
        return


def create_snapshot(
    repo_root: pathlib.Path,
    commit_oid: str,
    stage_parent: Optional[pathlib.Path] = None,
    after_graph_read: AfterGraphHook = None,
    after_materialize: AfterMaterializeHook = None,
) -> CreatedSnapshot:
    parent = pathlib.Path(
        os.path.abspath(os.fspath(stage_parent or tempfile.gettempdir()))
    )
    _require_real_directory(parent, "source snapshot parent")
    stage = pathlib.Path(tempfile.mkdtemp(prefix=STAGE_PREFIX, dir=os.fspath(parent)))
    os.chmod(stage, 0o700)
    created_files: List[pathlib.Path] = []
    created_dirs: List[pathlib.Path] = []
    token = secrets.token_hex(32)
    try:
        git = GitObjects(repo_root)
        if not _is_digest(commit_oid, git.oid_bytes * 2):
            fail(
                "commit must be one full lowercase object id, not a ref or abbreviation"
            )
        with git:
            root_tree, commit_raw, directories, files, blobs = _read_graph(
                git, commit_oid
            )
        if after_graph_read:
            after_graph_read(commit_oid)
        tree_root = stage / "tree"
        _mkdir(tree_root)
        created_dirs.append(tree_root)
        for item in sorted(
            directories,
            key=lambda value: (value["path"].count("/"), value["path"].encode("utf-8")),
        ):
            destination = stage.joinpath(*item["staged_path"].split("/"))
            _mkdir(destination)
            created_dirs.append(destination)
        for item in files:
            destination = stage.joinpath(*item["staged_path"].split("/"))
            _write_exclusive(destination, blobs[item["path"]])
            created_files.append(destination)
        body = _descriptor_body(
            git.object_format, commit_oid, commit_raw, root_tree, directories, files
        )
        snapshot_id = sha256_bytes(canonical_bytes(body))
        descriptor = dict(body)
        descriptor["snapshot_id"] = snapshot_id
        snapshot_path = stage / SNAPSHOT_NAME
        _write_exclusive(snapshot_path, canonical_bytes(descriptor))
        created_files.append(snapshot_path)
        sentinel = {
            "schema": OWNER_SCHEMA,
            "destroy_token_sha256": sha256_bytes(token.encode("ascii")),
            "snapshot_id": snapshot_id,
        }
        sentinel_path = stage / SENTINEL_NAME
        _write_exclusive(sentinel_path, canonical_bytes(sentinel))
        created_files.append(sentinel_path)
        if after_materialize:
            after_materialize(stage)
        _seal(stage, directories, files)
        _inspect_exact_tree(stage, descriptor)
        return CreatedSnapshot(
            snapshot_path, snapshot_id, token, commit_oid, len(files)
        )
    except BaseException:
        _remove_partial(stage, created_files, created_dirs)
        raise


def _validate_descriptor(value: object) -> Dict[str, Any]:
    top = _exact_object(
        value,
        "source snapshot descriptor",
        (
            "schema",
            "object_format",
            "commit",
            "root_tree",
            "directories",
            "files",
            "scope",
            "snapshot_id",
        ),
    )
    if top["schema"] != SCHEMA or top["object_format"] not in ("sha1", "sha256"):
        fail("unsupported source snapshot schema or object format")
    oid_len = 40 if top["object_format"] == "sha1" else 64
    commit = _exact_object(
        top["commit"], "snapshot commit", ("oid", "sha256", "size", "tree_oid")
    )
    root = _exact_object(
        top["root_tree"], "snapshot root tree", ("git_mode", "staged_path", "tree_oid")
    )
    if (
        not _is_digest(commit["oid"], oid_len)
        or not _is_digest(commit["tree_oid"], oid_len)
        or not _is_digest(commit["sha256"])
    ):
        fail("snapshot commit evidence is invalid")
    if (
        not isinstance(commit["size"], int)
        or isinstance(commit["size"], bool)
        or commit["size"] < 0
    ):
        fail("snapshot commit size is invalid")
    if root != {
        "git_mode": "040000",
        "staged_path": "tree",
        "tree_oid": commit["tree_oid"],
    }:
        fail("snapshot root tree evidence is invalid")
    if top["scope"] != {
        "claim": SNAPSHOT_CLAIM,
        "does_not_claim": list(SNAPSHOT_NON_CLAIMS),
    }:
        fail("snapshot scope is invalid or overstated")
    if not _is_digest(top["snapshot_id"]):
        fail("snapshot id is invalid")
    directories = top["directories"]
    files = top["files"]
    if (
        not isinstance(directories, list)
        or not isinstance(files, list)
        or len(files) > MAX_FILES
    ):
        fail("snapshot directory/file arrays are invalid")
    source_paths: List[str] = []
    staged_paths: List[str] = []
    portable: set[Tuple[str, ...]] = set()
    for index, item in enumerate(directories):
        item = _exact_object(
            item,
            f"snapshot directory {index}",
            ("path", "git_mode", "tree_oid", "staged_path"),
        )
        _validate_relative(item["path"], f"snapshot directory {index} path")
        _validate_relative(
            item["staged_path"], f"snapshot directory {index} staged path"
        )
        if (
            item["git_mode"] != "040000"
            or not _is_digest(item["tree_oid"], oid_len)
            or item["staged_path"] != f"tree/{item['path']}"
        ):
            fail(f"snapshot directory {index} evidence is invalid")
        source_paths.append(item["path"])
        staged_paths.append(item["staged_path"])
    for index, item in enumerate(files):
        item = _exact_object(
            item,
            f"snapshot file {index}",
            ("path", "git_mode", "blob_oid", "sha256", "size", "staged_path"),
        )
        _validate_relative(item["path"], f"snapshot file {index} path")
        _validate_relative(item["staged_path"], f"snapshot file {index} staged path")
        if (
            item["git_mode"] not in ("100644", "100755")
            or not _is_digest(item["blob_oid"], oid_len)
            or not _is_digest(item["sha256"])
            or item["staged_path"] != f"tree/{item['path']}"
        ):
            fail(f"snapshot file {index} evidence is invalid")
        if (
            not isinstance(item["size"], int)
            or isinstance(item["size"], bool)
            or item["size"] < 0
        ):
            fail(f"snapshot file {index} size is invalid")
        source_paths.append(item["path"])
        staged_paths.append(item["staged_path"])
    directory_paths = [item["path"] for item in directories]
    file_paths = [item["path"] for item in files]
    if directory_paths != sorted(
        directory_paths, key=lambda item: item.encode("utf-8")
    ) or file_paths != sorted(file_paths, key=lambda item: item.encode("utf-8")):
        fail("snapshot source entries are not in canonical byte order")
    for path in source_paths:
        key = _portable_collision_key(path)
        if key in portable:
            fail("snapshot contains duplicate or portable-colliding paths")
        portable.add(key)
    if len(staged_paths) != len(set(staged_paths)):
        fail("snapshot contains duplicate staged paths")
    body = dict(top)
    body.pop("snapshot_id")
    if sha256_bytes(canonical_bytes(body)) != top["snapshot_id"]:
        fail("snapshot id does not bind the canonical descriptor")
    return top


def _read_json_file(
    stage: pathlib.Path, relative: str, maximum: int, label: str
) -> Tuple[bytes, object]:
    with _open_nofollow(stage, relative) as stream:
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        fail(f"{label} exceeds its size bound")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    return raw, value


def verify_snapshot(
    snapshot_path: pathlib.Path, expected_commit: Optional[str] = None
) -> Dict[str, Any]:
    snapshot = pathlib.Path(os.path.abspath(os.fspath(snapshot_path)))
    stage = snapshot.parent
    if snapshot.name != SNAPSHOT_NAME or not stage.name.startswith(STAGE_PREFIX):
        fail("descriptor is not in an owned source-snapshot stage")
    _require_real_directory(stage, "source snapshot stage")
    raw, value = _read_json_file(
        stage, SNAPSHOT_NAME, MAX_DESCRIPTOR_BYTES, "source snapshot descriptor"
    )
    descriptor = _validate_descriptor(value)
    if raw != canonical_bytes(descriptor):
        fail("source snapshot descriptor is not canonical JSON")
    if expected_commit is not None and descriptor["commit"]["oid"] != expected_commit:
        fail("source snapshot commit does not match the expected exact commit")
    sentinel_raw, sentinel_value = _read_json_file(
        stage, SENTINEL_NAME, 4096, "source snapshot owner sentinel"
    )
    sentinel = _exact_object(
        sentinel_value,
        "source snapshot owner sentinel",
        ("schema", "destroy_token_sha256", "snapshot_id"),
    )
    if (
        sentinel_raw != canonical_bytes(sentinel)
        or sentinel["schema"] != OWNER_SCHEMA
        or not _is_digest(sentinel["destroy_token_sha256"])
        or sentinel["snapshot_id"] != descriptor["snapshot_id"]
    ):
        fail("source snapshot owner sentinel is invalid")
    for item in descriptor["files"]:
        digest = hashlib.sha256()
        object_digest = hashlib.new(descriptor["object_format"])
        size = 0
        # The Git object header depends on the known descriptor size.  Feed it
        # before the bytes so local verification also proves blob-id binding.
        object_digest.update(f"blob {item['size']}\0".encode("ascii"))
        with _open_nofollow(stage, item["staged_path"]) as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
                object_digest.update(chunk)
                size += len(chunk)
        if (
            digest.hexdigest() != item["sha256"]
            or object_digest.hexdigest() != item["blob_oid"]
            or size != item["size"]
        ):
            fail(f"source snapshot file bytes changed: {item['path']}")
    _inspect_exact_tree(stage, descriptor)
    return descriptor


def verify_against_git(
    repo_root: pathlib.Path, commit_oid: str, snapshot_path: pathlib.Path
) -> Dict[str, Any]:
    """Authenticate the complete descriptor by reconstructing it from Git."""
    descriptor = verify_snapshot(snapshot_path, commit_oid)
    git = GitObjects(repo_root)
    if not _is_digest(commit_oid, git.oid_bytes * 2):
        fail("commit must be one full lowercase object id, not a ref or abbreviation")
    with git:
        root_tree, commit_raw, directories, files, _ = _read_graph(git, commit_oid)
    body = _descriptor_body(
        git.object_format, commit_oid, commit_raw, root_tree, directories, files
    )
    expected_snapshot_id = sha256_bytes(canonical_bytes(body))
    reconstructed = dict(body)
    reconstructed["snapshot_id"] = expected_snapshot_id
    if reconstructed != descriptor or canonical_bytes(reconstructed) != canonical_bytes(
        descriptor
    ):
        fail(
            "source snapshot descriptor does not exactly match authenticated Git objects"
        )
    snapshot = pathlib.Path(os.path.abspath(os.fspath(snapshot_path)))
    return {
        "schema": GIT_VERIFICATION_SCHEMA,
        "claim": "complete-source-snapshot-descriptor-reconstructed-from-authenticated-git-objects",
        "commit_oid": commit_oid,
        "descriptor_sha256": sha256_bytes(canonical_bytes(descriptor)),
        "snapshot_id": descriptor["snapshot_id"],
        "tree": os.fspath(snapshot.parent / "tree"),
    }


def verify_descriptor_against_git(
    repo_root: pathlib.Path, commit_oid: str, descriptor_path: pathlib.Path
) -> Dict[str, Any]:
    """Authenticate a retained descriptor without treating it as a live stage.

    This is a post-cleanup audit projection.  It reconstructs every descriptor
    field from a trusted Git object database, but it does not establish that a
    materialized snapshot existed, remained immutable, or was consumed by a
    compiler.
    """

    path = pathlib.Path(os.path.abspath(os.fspath(descriptor_path)))
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
        or metadata.st_size > MAX_DESCRIPTOR_BYTES
    ):
        fail("retained source descriptor must be a bounded single-link regular file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor_fd = os.open(path, flags)
    try:
        opened = os.fstat(descriptor_fd)
        if (opened.st_dev, opened.st_ino) != (metadata.st_dev, metadata.st_ino):
            fail("retained source descriptor changed before it could be opened")
        raw = b""
        while len(raw) <= MAX_DESCRIPTOR_BYTES:
            chunk = os.read(
                descriptor_fd, min(64 * 1024, MAX_DESCRIPTOR_BYTES + 1 - len(raw))
            )
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor_fd)
    finally:
        os.close(descriptor_fd)
    current = os.lstat(path)
    if (
        len(raw) > MAX_DESCRIPTOR_BYTES
        or (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)
    ):
        fail("retained source descriptor changed while being read")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"retained source descriptor is not valid JSON: {error}")
    descriptor = _validate_descriptor(value)
    if raw != canonical_bytes(descriptor):
        fail("retained source descriptor is not canonical JSON")

    git = GitObjects(repo_root)
    if not _is_digest(commit_oid, git.oid_bytes * 2):
        fail("commit must be one full lowercase object id, not a ref or abbreviation")
    with git:
        root_tree, commit_raw, directories, files, _ = _read_graph(git, commit_oid)
    body = _descriptor_body(
        git.object_format, commit_oid, commit_raw, root_tree, directories, files
    )
    reconstructed = dict(body)
    reconstructed["snapshot_id"] = sha256_bytes(canonical_bytes(body))
    if descriptor != reconstructed or raw != canonical_bytes(reconstructed):
        fail("retained source descriptor does not exactly match trusted Git objects")
    return {
        "schema": GIT_VERIFICATION_SCHEMA,
        "claim": "retained-descriptor-reconstructed-from-authenticated-git-objects-not-live-stage-or-build-causality-proof",
        "commit_oid": commit_oid,
        "descriptor_sha256": sha256_bytes(raw),
        "snapshot_id": descriptor["snapshot_id"],
    }


def _remove_exact(stage: pathlib.Path, descriptor: Dict[str, Any]) -> None:
    expected_files, expected_dirs = _expected_tree(descriptor)
    _inspect_exact_tree(stage, descriptor)
    for relative in sorted(
        expected_files, key=lambda item: item.count("/"), reverse=True
    ):
        path = stage.joinpath(*relative.split("/"))
        metadata = os.lstat(path)
        _require_regular(metadata, f"snapshot cleanup file {relative}")
        _chmod_nofollow(path, 0o600)
        path.unlink()
    for relative in sorted(
        expected_dirs, key=lambda item: (item.count("/"), item), reverse=True
    ):
        path = stage.joinpath(*relative.split("/"))
        _require_real_directory(path, f"snapshot cleanup directory {relative}")
        _chmod_nofollow(path, 0o700)
        path.rmdir()
    _require_real_directory(stage, "snapshot cleanup stage")
    _chmod_nofollow(stage, 0o700)
    stage.rmdir()


def destroy_snapshot(snapshot_path: pathlib.Path, token: str) -> None:
    if not _is_digest(token):
        fail("source snapshot destruction token is invalid")
    descriptor = verify_snapshot(snapshot_path)
    stage = pathlib.Path(os.path.abspath(os.fspath(snapshot_path))).parent
    _, sentinel_value = _read_json_file(
        stage, SENTINEL_NAME, 4096, "source snapshot owner sentinel"
    )
    sentinel = _exact_object(
        sentinel_value,
        "source snapshot owner sentinel",
        ("schema", "destroy_token_sha256", "snapshot_id"),
    )
    if not secrets.compare_digest(
        sentinel["destroy_token_sha256"], sha256_bytes(token.encode("ascii"))
    ):
        fail("source snapshot destruction token does not authorize this stage")
    _remove_exact(stage, descriptor)


def _validate_result(value: object) -> Dict[str, Any]:
    result = _exact_object(
        value,
        "source snapshot create result",
        (
            "commit_oid",
            "destroy_token",
            "files_count",
            "snapshot",
            "snapshot_id",
            "stage",
            "tree",
        ),
    )
    if (
        not (
            _is_digest(result["commit_oid"], 40) or _is_digest(result["commit_oid"], 64)
        )
        or not _is_digest(result["destroy_token"])
        or not _is_digest(result["snapshot_id"])
    ):
        fail("source snapshot create result contains an invalid digest")
    if (
        not isinstance(result["files_count"], int)
        or isinstance(result["files_count"], bool)
        or result["files_count"] < 0
    ):
        fail("source snapshot create result file count is invalid")
    for key in ("snapshot", "stage", "tree"):
        if (
            not isinstance(result[key], str)
            or not result[key]
            or any(character in result[key] for character in "\0\r\n")
        ):
            fail(f"source snapshot create result {key} is unsafe for shell transport")
    if pathlib.Path(result["snapshot"]).parent != pathlib.Path(
        result["stage"]
    ) or pathlib.Path(result["tree"]).parent != pathlib.Path(result["stage"]):
        fail("source snapshot create result paths are inconsistent")
    return result


def query_result(raw: bytes, field: str) -> str:
    raw = raw.replace(b"\r\n", b"\n")
    try:
        result = _validate_result(json.loads(raw))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"source snapshot create result is not valid JSON: {error}")
    if raw != canonical_bytes(result):
        fail("source snapshot create result is not canonical JSON")
    text = str(result[field])
    if any(character in text for character in "\0\r\n"):
        fail("source snapshot query result is unsafe for shell transport")
    return text


def _validate_git_verification(value: object) -> Dict[str, Any]:
    result = _exact_object(
        value,
        "source snapshot Git verification result",
        ("schema", "claim", "commit_oid", "descriptor_sha256", "snapshot_id", "tree"),
    )
    if (
        result["schema"] != GIT_VERIFICATION_SCHEMA
        or result["claim"]
        != "complete-source-snapshot-descriptor-reconstructed-from-authenticated-git-objects"
    ):
        fail("source snapshot Git verification result has an invalid claim")
    if (
        not (
            _is_digest(result["commit_oid"], 40) or _is_digest(result["commit_oid"], 64)
        )
        or not _is_digest(result["descriptor_sha256"])
        or not _is_digest(result["snapshot_id"])
    ):
        fail("source snapshot Git verification result contains an invalid digest")
    if (
        not isinstance(result["tree"], str)
        or not result["tree"]
        or any(character in result["tree"] for character in "\0\r\n")
    ):
        fail("source snapshot Git verification tree is unsafe for shell transport")
    return result


def query_git_verification(raw: bytes, field: str) -> str:
    raw = raw.replace(b"\r\n", b"\n")
    try:
        result = _validate_git_verification(json.loads(raw))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"source snapshot Git verification result is not valid JSON: {error}")
    if raw != canonical_bytes(result):
        fail("source snapshot Git verification result is not canonical JSON")
    text = str(result[field])
    if any(character in text for character in "\0\r\n"):
        fail("source snapshot Git verification query is unsafe for shell transport")
    return text


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create", help="materialize one exact Git commit")
    create.add_argument("--repo-root", required=True)
    create.add_argument("--commit", required=True)
    create.add_argument("--stage-parent")
    verify = commands.add_parser("verify", help="verify a materialized snapshot")
    verify.add_argument("--commit")
    verify.add_argument("snapshot")
    verify_git = commands.add_parser(
        "verify-against-git",
        help="reconstruct and authenticate the complete descriptor from Git objects",
    )
    verify_git.add_argument("--repo-root", required=True)
    verify_git.add_argument("--commit", required=True)
    verify_git.add_argument("snapshot")
    verify_projection = commands.add_parser(
        "verify-descriptor-against-git",
        help="authenticate a retained audit descriptor from trusted Git objects",
    )
    verify_projection.add_argument("--repo-root", required=True)
    verify_projection.add_argument("--commit", required=True)
    verify_projection.add_argument("descriptor")
    destroy = commands.add_parser("destroy", help="destroy an owned exact snapshot")
    destroy.add_argument("--token", required=True)
    destroy.add_argument("snapshot")
    query = commands.add_parser(
        "query-result",
        help="extract one safe scalar from canonical create JSON on stdin",
    )
    query.add_argument(
        "--field",
        required=True,
        choices=(
            "commit_oid",
            "destroy_token",
            "files_count",
            "snapshot",
            "snapshot_id",
            "stage",
            "tree",
        ),
    )
    query_verified = commands.add_parser(
        "query-verified",
        help="extract one safe scalar from canonical Git-verification JSON on stdin",
    )
    query_verified.add_argument(
        "--field",
        required=True,
        choices=("commit_oid", "descriptor_sha256", "snapshot_id", "tree"),
    )
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "create":
            created = create_snapshot(
                pathlib.Path(args.repo_root),
                args.commit,
                pathlib.Path(args.stage_parent) if args.stage_parent else None,
            )
            print(canonical_bytes(created.cli_result()).decode("ascii"), end="")
        elif args.command == "verify":
            descriptor = verify_snapshot(pathlib.Path(args.snapshot), args.commit)
            print(
                f"source snapshot verified: commit={descriptor['commit']['oid']} files={len(descriptor['files'])} id={descriptor['snapshot_id']}"
            )
        elif args.command == "verify-against-git":
            result = verify_against_git(
                pathlib.Path(args.repo_root), args.commit, pathlib.Path(args.snapshot)
            )
            print(canonical_bytes(result).decode("ascii"), end="")
        elif args.command == "verify-descriptor-against-git":
            result = verify_descriptor_against_git(
                pathlib.Path(args.repo_root), args.commit, pathlib.Path(args.descriptor)
            )
            print(canonical_bytes(result).decode("ascii"), end="")
        elif args.command == "destroy":
            destroy_snapshot(pathlib.Path(args.snapshot), args.token)
        elif args.command in ("query-result", "query-verified"):
            raw = sys.stdin.buffer.read(MAX_DESCRIPTOR_BYTES + 1)
            if len(raw) > MAX_DESCRIPTOR_BYTES:
                fail("source snapshot create result exceeds its size bound")
            if args.command == "query-result":
                print(query_result(raw, args.field))
            else:
                print(query_git_verification(raw, args.field))
        return 0
    except (
        SnapshotError,
        OSError,
        subprocess.SubprocessError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        print(f"ERROR: source snapshot: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
