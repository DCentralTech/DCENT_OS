#!/usr/bin/env python3
"""Manage an invocation-bound, capability-owned private result stage.

The stage is a host-side handoff boundary for Cargo or container outputs.  It
does not execute builds, create Docker resources, or publish releases.  A
sealed descriptor proves only the exact bytes and metadata observed in the
stage; it is not execution causality or reproducibility evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import secrets
import stat
import sys
import unicodedata
from dataclasses import dataclass
from typing import Any, Dict, Iterable, List, NoReturn, Optional, Tuple

import release_invocation


DESCRIPTOR_SCHEMA = "org.dcentral.dcentos.release-result-stage.v1"
CAPABILITY_SCHEMA = "org.dcentral.dcentos.release-result-stage-capability.v1"
RESULT_SCHEMA = "org.dcentral.dcentos.release-result-stage-create-result.v1"
AUDIT_PROJECTION_SCHEMA = (
    "org.dcentral.dcentos.release-result-stage-audit-projection.v1"
)
DESCRIPTOR_NAME = "result-stage.json"
RESULT_ROOT_NAME = "results"
STAGE_PREFIX = "dcentos-release-result-"
CAPABILITY_DIRECTORY = ".dcentos-release-result-capabilities"
CAPABILITY_SUFFIX = ".capability.json"
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
    if created:
        _set_private_directory(directory)
    _check_directory(os.lstat(directory), "result capability directory", private=True)
    return directory


def _write_exclusive(path: pathlib.Path, raw: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags, 0o400 if os.name == "posix" else 0o600)
    try:
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise
    if os.name == "posix":
        os.chmod(path, 0o400, follow_symlinks=False)


def _fsync_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def _read_control(path: pathlib.Path, label: str) -> Tuple[bytes, os.stat_result]:
    path = assert_no_link_components(path, label)
    before = os.lstat(path)
    _check_control_file(before, label)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
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
        return b"".join(chunks), opened
    finally:
        os.close(descriptor)


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


def _hash_file(
    path: pathlib.Path, before: os.stat_result, label: str
) -> Tuple[str, int]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags)
    try:
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
        after = os.fstat(descriptor)
        identity = (before.st_dev, before.st_ino, before.st_size)
        if (after.st_dev, after.st_ino, after.st_size) != identity:
            fail(f"{label} changed while it was hashed")
        for field in ("st_mtime_ns", "st_ctime_ns"):
            if hasattr(before, field) and getattr(before, field) != getattr(
                after, field
            ):
                fail(f"{label} metadata changed while it was hashed")
        if size != before.st_size:
            fail(f"{label} size changed while it was hashed")
        return digest.hexdigest(), size
    finally:
        os.close(descriptor)


def _walk_payload(root: pathlib.Path, *, hash_files: bool) -> Dict[str, Any]:
    _check_directory(os.lstat(root), "result payload root")
    directories: List[Dict[str, Any]] = []
    files: List[Dict[str, Any]] = []
    portable: set[Tuple[str, ...]] = set()
    stack: List[Tuple[pathlib.Path, str]] = [(root, "")]
    while stack:
        directory, prefix = stack.pop()
        _check_directory(os.lstat(directory), f"result directory {prefix or '.'}")
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
                        path, metadata, f"result payload {relative}"
                    )
                files.append(item)
            if len(directories) + len(files) > MAX_ENTRIES:
                fail(f"result payload exceeds {MAX_ENTRIES} entries")
        stack.extend(reversed(child_directories))
    directories.sort(key=lambda item: item["path"].encode("utf-8"))
    files.sort(key=lambda item: item["path"].encode("utf-8"))
    return {"directories": directories, "files": files}


def _manifest(payload: Dict[str, Any]) -> Dict[str, Any]:
    body = {
        "directories": payload["directories"],
        "files": payload["files"],
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


def _expected_stage_name(descriptor: Dict[str, Any]) -> str:
    return (
        f"{STAGE_PREFIX}{descriptor['invocation']['result_name']}-"
        f"{descriptor['stage_id']}"
    )


def capability_path(stage: pathlib.Path) -> pathlib.Path:
    stage = lexical_absolute(stage)
    return stage.parent / CAPABILITY_DIRECTORY / f"{stage.name}{CAPABILITY_SUFFIX}"


def _capability(descriptor: Dict[str, Any], stage: pathlib.Path) -> Dict[str, Any]:
    return {
        "invocation_id": descriptor["invocation"]["invocation_id"],
        "invocation_stage": descriptor["invocation"]["stage"],
        "schema": CAPABILITY_SCHEMA,
        "stage_id": descriptor["stage_id"],
        "stage_path": str(stage),
        "token": secrets.token_hex(32),
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
    else:
        fail("result-stage state must be building or sealed")
    return descriptor


def _validate_capability(
    value: object,
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
) -> Dict[str, Any]:
    capability = _exact_object(
        value,
        "result-stage capability",
        (
            "invocation_id",
            "invocation_stage",
            "schema",
            "stage_id",
            "stage_path",
            "token",
        ),
    )
    expected = {
        "invocation_id": descriptor["invocation"]["invocation_id"],
        "invocation_stage": descriptor["invocation"]["stage"],
        "schema": CAPABILITY_SCHEMA,
        "stage_id": descriptor["stage_id"],
        "stage_path": str(stage),
    }
    for key, expected_value in expected.items():
        if capability[key] != expected_value:
            fail(f"result-stage capability {key} binding does not match")
    if not _is_digest(capability["token"]):
        fail("result-stage capability token is invalid")
    return capability


def _inspect_stage_top(stage: pathlib.Path) -> None:
    expected = {DESCRIPTOR_NAME, RESULT_ROOT_NAME}
    with os.scandir(stage) as entries:
        actual = {entry.name for entry in entries}
    if actual != expected:
        fail(
            "result-stage control tree is not exact "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )
    _check_control_file(os.lstat(stage / DESCRIPTOR_NAME), "result-stage descriptor")
    _check_directory(
        os.lstat(stage / RESULT_ROOT_NAME), "result payload root", private=True
    )


def create_result_stage(
    stage_parent: pathlib.Path, invocation_stage: pathlib.Path
) -> CreatedResultStage:
    parent = assert_no_link_components(stage_parent, "result-stage parent")
    _check_directory(os.lstat(parent), "result-stage parent")
    verified_invocation = release_invocation.verify_invocation(invocation_stage)
    binding = _invocation_binding(verified_invocation)
    capability_directory = _ensure_capability_directory(parent)
    for _attempt in range(32):
        allocation_nonce = secrets.token_hex(32)
        descriptor_value = _descriptor(binding, allocation_nonce, None)
        stage_id = descriptor_value["stage_id"]
        stage = parent / _expected_stage_name(descriptor_value)
        capability = capability_path(stage)
        assert_no_link_components(stage, "result stage", allow_missing_leaf=True)
        assert_no_link_components(
            capability, "result-stage capability", allow_missing_leaf=True
        )
        try:
            os.mkdir(stage, 0o700)
        except FileExistsError:
            continue
        _set_private_directory(stage)
        result_root = stage / RESULT_ROOT_NAME
        descriptor_path = stage / DESCRIPTOR_NAME
        created: List[pathlib.Path] = []
        try:
            os.mkdir(result_root, 0o700)
            _set_private_directory(result_root)
            created.append(result_root)
            _write_exclusive(descriptor_path, canonical_bytes(descriptor_value))
            created.append(descriptor_path)
            _fsync_directory(stage)
            _write_exclusive(
                capability, canonical_bytes(_capability(descriptor_value, stage))
            )
            created.append(capability)
            _fsync_directory(capability_directory)
            _fsync_directory(parent)
            verified = verify_result_stage(stage, verified_invocation.stage)
            verify_capability(
                verified.stage,
                capability,
                verified.invocation.stage,
                verified=verified,
            )
            return CreatedResultStage(
                stage=stage,
                descriptor=descriptor_path,
                capability=capability,
                result_root=result_root,
                invocation_stage=verified_invocation.stage,
                invocation_id=binding["invocation_id"],
                result_name=binding["result_name"],
                stage_id=stage_id,
            )
        except BaseException:
            # Only remove the exact empty/regular objects allocated here.  A
            # modified partial stage is intentionally retained for inspection.
            for path in reversed(created):
                try:
                    metadata = os.lstat(path)
                    if stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1:
                        path.unlink()
                    elif stat.S_ISDIR(metadata.st_mode):
                        path.rmdir()
                except (FileNotFoundError, OSError):
                    pass
            try:
                stage.rmdir()
            except OSError:
                pass
            raise
    fail("could not allocate a unique result stage after 32 attempts")


def verify_result_stage(
    stage_value: pathlib.Path, invocation_stage: pathlib.Path
) -> VerifiedResultStage:
    invocation = release_invocation.verify_invocation(invocation_stage)
    stage = assert_no_link_components(stage_value, "result stage")
    _check_directory(os.lstat(stage), "result stage", private=True)
    _inspect_stage_top(stage)
    raw, _ = _read_control(stage / DESCRIPTOR_NAME, "result-stage descriptor")
    descriptor = _validate_descriptor(
        _parse_canonical(raw, "result-stage descriptor"), invocation
    )
    if stage.name != _expected_stage_name(descriptor):
        fail("result-stage name is not canonically descriptor-derived")
    payload = _walk_payload(
        stage / RESULT_ROOT_NAME, hash_files=descriptor["state"] == "sealed"
    )
    if descriptor["state"] == "sealed":
        observed = _manifest(payload)
        if observed != descriptor["manifest"]:
            fail("sealed result-stage payload differs from its exact manifest")
    _inspect_stage_top(stage)
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
    record = verified or verify_result_stage(stage, invocation_stage)
    if record.invocation.stage != lexical_absolute(invocation_stage):
        fail("result-stage operation supplied a different release invocation")
    raw, _ = _read_control(supplied, "result-stage capability")
    return _validate_capability(
        _parse_canonical(raw, "result-stage capability"),
        record.stage,
        record.descriptor,
    )


def _replace_descriptor(stage: pathlib.Path, value: Dict[str, Any]) -> None:
    destination = stage / DESCRIPTOR_NAME
    temporary = stage / f".{DESCRIPTOR_NAME}.{secrets.token_hex(16)}.next"
    assert_no_link_components(
        temporary, "temporary sealed result descriptor", allow_missing_leaf=True
    )
    try:
        _write_exclusive(temporary, canonical_bytes(value))
        os.replace(temporary, destination)
        if os.name == "posix":
            os.chmod(destination, 0o400, follow_symlinks=False)
        _fsync_directory(stage)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def seal_result_stage(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    invocation_stage: pathlib.Path,
) -> VerifiedResultStage:
    verified = verify_result_stage(stage_value, invocation_stage)
    verify_capability(
        verified.stage,
        capability_value,
        verified.invocation.stage,
        verified=verified,
    )
    if verified.descriptor["state"] != "building":
        fail("result stage is already sealed")
    payload = _walk_payload(verified.stage / RESULT_ROOT_NAME, hash_files=True)
    sealed = dict(verified.descriptor)
    sealed["manifest"] = _manifest(payload)
    sealed["state"] = "sealed"
    _replace_descriptor(verified.stage, sealed)
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


def destroy_result_stage(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    invocation_stage: pathlib.Path,
) -> None:
    verified = verify_result_stage(stage_value, invocation_stage)
    verify_capability(
        verified.stage,
        capability_value,
        verified.invocation.stage,
        verified=verified,
    )
    # Re-walk immediately before deletion.  Unsafe or non-manifest state fails
    # before the first unlink and deliberately leaves the stage for inspection.
    _inspect_stage_top(verified.stage)
    _walk_payload(
        verified.stage / RESULT_ROOT_NAME,
        hash_files=verified.descriptor["state"] == "sealed",
    )
    if verified.descriptor["state"] == "sealed":
        final_payload = _walk_payload(
            verified.stage / RESULT_ROOT_NAME, hash_files=True
        )
        if _manifest(final_payload) != verified.descriptor["manifest"]:
            fail("sealed result-stage payload changed before destruction")
    raw, descriptor_metadata = _read_control(
        verified.stage / DESCRIPTOR_NAME, "destroy result-stage descriptor"
    )
    final_descriptor = _validate_descriptor(
        _parse_canonical(raw, "destroy result-stage descriptor"), verified.invocation
    )
    if final_descriptor != verified.descriptor:
        fail("result-stage descriptor changed before destruction")
    capability = capability_path(verified.stage)
    capability_raw, capability_metadata = _read_control(
        capability, "destroy result-stage capability"
    )
    _validate_capability(
        _parse_canonical(capability_raw, "destroy result-stage capability"),
        verified.stage,
        final_descriptor,
    )

    payload = _walk_payload(verified.stage / RESULT_ROOT_NAME, hash_files=False)
    file_records: List[Tuple[pathlib.Path, os.stat_result, str]] = []
    directory_records: List[Tuple[pathlib.Path, os.stat_result, str]] = []
    for item in payload["files"]:
        path = verified.stage / RESULT_ROOT_NAME / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        if (
            _safe_payload_metadata(metadata, f"destroy result file {item['path']}")
            != "file"
        ):
            fail(f"destroy result file {item['path']} changed type")
        file_records.append((path, metadata, item["path"]))
    for item in payload["directories"]:
        path = verified.stage / RESULT_ROOT_NAME / pathlib.PurePosixPath(item["path"])
        metadata = os.lstat(path)
        _check_directory(metadata, f"destroy result directory {item['path']}")
        directory_records.append((path, metadata, item["path"]))
    root_metadata = os.lstat(verified.stage / RESULT_ROOT_NAME)
    stage_metadata = os.lstat(verified.stage)

    for path, metadata, relative in file_records:
        _unlink_verified(path, metadata, f"destroy result file {relative}")
    for path, metadata, relative in sorted(
        directory_records,
        key=lambda record: (record[2].count("/"), record[2]),
        reverse=True,
    ):
        _rmdir_verified(path, metadata, f"destroy result directory {relative}")
    _rmdir_verified(
        verified.stage / RESULT_ROOT_NAME, root_metadata, "destroy result payload root"
    )
    # The descriptor is a control file with 0400 mode, but the generic safe
    # file remover deliberately accepts it only after exact control validation.
    _unlink_verified(
        verified.stage / DESCRIPTOR_NAME,
        descriptor_metadata,
        "destroy result-stage descriptor",
    )
    _rmdir_verified(verified.stage, stage_metadata, "destroy result stage")
    _unlink_verified(capability, capability_metadata, "destroy result-stage capability")
    _fsync_directory(verified.stage.parent)


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
                pathlib.Path(args.stage_parent), pathlib.Path(args.invocation_stage)
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
