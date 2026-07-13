#!/usr/bin/env python3
"""Create capability-owned, invocation-scoped release resource identities.

This helper allocates names and a private local control stage.  It does not
create, inspect, or remove Docker resources, build outputs, or release
artifacts.  Local-stage destruction is permitted only after an explicit,
capability-authorized garbage-collection eligibility transition.
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
from typing import Any, Dict, Iterable, NamedTuple, NoReturn, Optional, Tuple


DESCRIPTOR_SCHEMA = "org.dcentral.dcentos.release-invocation.v1"
STATE_SCHEMA = "org.dcentral.dcentos.release-invocation-state.v1"
CAPABILITY_SCHEMA = "org.dcentral.dcentos.release-invocation-capability.v1"
RESULT_SCHEMA = "org.dcentral.dcentos.release-invocation-result.v1"
DESCRIPTOR_NAME = "invocation.json"
STATE_NAME = "gc-state.json"
STAGE_PREFIX = "dcentos-release-invocation-"
CAPABILITY_DIRECTORY = ".dcentos-release-invocation-capabilities"
CAPABILITY_SUFFIX = ".capability.json"
MAX_CONTROL_BYTES = 64 * 1024
HEX_64 = frozenset("0123456789abcdef")
NAME_RE = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,31})\Z")
RESOURCE_RE = re.compile(r"[a-z0-9][a-z0-9_.-]{0,254}\Z")
CLAIM = "invocation-scoped-resource-identities-and-local-control-state"
NON_CLAIMS = (
    "docker-resource-creation-or-liveness",
    "docker-resource-deletion",
    "build-execution-or-artifact-causality",
    "output-stage-creation-or-publication",
)


class InvocationError(ValueError):
    """An invocation control record failed a safety or integrity check."""


class VerifiedInvocation(NamedTuple):
    stage: pathlib.Path
    descriptor: Dict[str, Any]
    state: Dict[str, Any]


class CreatedInvocation(NamedTuple):
    stage: pathlib.Path
    descriptor: pathlib.Path
    capability: pathlib.Path
    invocation_id: str
    resources: Dict[str, Any]

    def cli_result(self) -> Dict[str, Any]:
        return {
            "capability": str(self.capability),
            "descriptor": str(self.descriptor),
            "invocation_id": self.invocation_id,
            "resources": self.resources,
            "schema": RESULT_SCHEMA,
            "stage": str(self.stage),
        }


class AuditVerificationStage(NamedTuple):
    """Exact ephemeral reconstruction used only by post-cleanup verification."""

    stage: pathlib.Path
    descriptor: Dict[str, Any]


def fail(message: str) -> NoReturn:
    raise InvocationError(message)


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("ascii")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def lexical_absolute(value: pathlib.Path) -> pathlib.Path:
    return pathlib.Path(os.path.abspath(str(value)))


def _contains_control(value: str) -> bool:
    return any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)


def _require_safe_scalar(value: str, label: str) -> str:
    if not isinstance(value, str) or not value or _contains_control(value):
        fail(f"{label} contains an empty or control-character value")
    return value


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


def _is_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse)


def _check_owner(metadata: os.stat_result, label: str) -> None:
    if hasattr(os, "geteuid") and hasattr(metadata, "st_uid"):
        if metadata.st_uid != os.geteuid():
            fail(f"{label} is not owned by the current user")


def _check_mode(metadata: os.stat_result, expected: int, label: str) -> None:
    if os.name == "posix" and stat.S_IMODE(metadata.st_mode) != expected:
        fail(
            f"{label} mode must be {expected:04o}, "
            f"found {stat.S_IMODE(metadata.st_mode):04o}"
        )


def _check_directory(metadata: os.stat_result, label: str, private: bool) -> None:
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a non-symlink, non-reparse directory")
    _check_owner(metadata, label)
    if private:
        _check_mode(metadata, 0o700, label)


def _check_file(metadata: os.stat_result, label: str) -> None:
    if not stat.S_ISREG(metadata.st_mode) or _is_reparse(metadata):
        fail(f"{label} must be a non-reparse regular file")
    if metadata.st_nlink != 1:
        fail(f"{label} must have exactly one filesystem link")
    _check_owner(metadata, label)
    _check_mode(metadata, 0o400, label)


def assert_no_symlink_components(
    value: pathlib.Path, label: str, allow_missing_leaf: bool = False
) -> pathlib.Path:
    path = lexical_absolute(value)
    _require_safe_scalar(str(path), label)
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
        flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
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
    if created:
        _set_private_directory(directory)
    directory = assert_no_symlink_components(directory, "capability directory")
    _check_directory(os.lstat(directory), "capability directory", private=True)
    return directory


def _write_exclusive(path: pathlib.Path, raw: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    # POSIX can enforce immutable-control-record intent with 0400.  On Windows,
    # that mode maps to the DOS read-only attribute and prevents the deliberate
    # atomic GC-state replacement (and later unlink), so rely on the private ACL
    # inherited from the stage/capability directory there.
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


def _read_owned_file(path: pathlib.Path, label: str) -> Tuple[bytes, os.stat_result]:
    path = assert_no_symlink_components(path, label)
    before = os.lstat(path)
    _check_file(before, label)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        _check_file(opened, label)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            fail(f"{label} changed while it was opened")
        chunks = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(65536, MAX_CONTROL_BYTES + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_CONTROL_BYTES:
                fail(f"{label} exceeds {MAX_CONTROL_BYTES} bytes")
        return b"".join(chunks), opened
    finally:
        os.close(descriptor)


def _parse_canonical(raw: bytes, label: str) -> Dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must be a JSON object")
    if raw != canonical_bytes(value):
        fail(f"{label} is not canonical JSON")
    return value


def _resource_names(logical_name: str, invocation_id: str) -> Dict[str, Any]:
    stem = f"dcentos-ri-{logical_name}-{invocation_id}"
    resources = {
        "docker_volumes": {
            "buildroot": f"{stem}-buildroot",
            "cargo": f"{stem}-cargo",
            "results": f"{stem}-results",
        },
        "output_stage_name": f"{stem}-output",
        "result_name": f"{stem}-result",
    }
    for label, value in _flatten_resources(resources).items():
        if not RESOURCE_RE.fullmatch(value):
            fail(f"derived {label} is not a canonical Docker/local resource name")
    if len(set(_flatten_resources(resources).values())) != 5:
        fail("derived release resource names are not unique")
    return resources


def _flatten_resources(resources: Dict[str, Any]) -> Dict[str, str]:
    volumes = resources["docker_volumes"]
    return {
        "buildroot_volume": volumes["buildroot"],
        "cargo_volume": volumes["cargo"],
        "output_stage_name": resources["output_stage_name"],
        "result_name": resources["result_name"],
        "results_volume": volumes["results"],
    }


def _descriptor(logical_name: str, invocation_id: str) -> Dict[str, Any]:
    return {
        "claim": CLAIM,
        "invocation_id": invocation_id,
        "logical_name": logical_name,
        "resources": _resource_names(logical_name, invocation_id),
        "schema": DESCRIPTOR_SCHEMA,
        "scope": {"does_not_claim": list(NON_CLAIMS)},
    }


def _initial_state(invocation_id: str, descriptor_digest: str) -> Dict[str, Any]:
    return {
        "descriptor_sha256": descriptor_digest,
        "garbage_collection": {
            "eligible": False,
            "reason": "not-explicitly-marked",
        },
        "invocation_id": invocation_id,
        "schema": STATE_SCHEMA,
    }


def capability_path(stage: pathlib.Path) -> pathlib.Path:
    stage = lexical_absolute(stage)
    return stage.parent / CAPABILITY_DIRECTORY / f"{stage.name}{CAPABILITY_SUFFIX}"


def _capability(
    stage: pathlib.Path, invocation_id: str, descriptor_digest: str
) -> Dict[str, Any]:
    return {
        "descriptor_sha256": descriptor_digest,
        "invocation_id": invocation_id,
        "schema": CAPABILITY_SCHEMA,
        "stage_path": str(stage),
        "token": secrets.token_hex(32),
    }


def _validate_resources(
    value: object, logical_name: str, invocation_id: str
) -> Dict[str, Any]:
    resources = _require_exact_object(
        value,
        "release invocation resources",
        ("docker_volumes", "output_stage_name", "result_name"),
    )
    _require_exact_object(
        resources["docker_volumes"],
        "release invocation Docker volumes",
        ("buildroot", "cargo", "results"),
    )
    for label, item in _flatten_resources(resources).items():
        if not isinstance(item, str) or not RESOURCE_RE.fullmatch(item):
            fail(f"release invocation {label} is unsafe or non-canonical")
    if resources != _resource_names(logical_name, invocation_id):
        fail("release invocation resource names are not canonically derived")
    return resources


def _validate_descriptor(value: object) -> Dict[str, Any]:
    descriptor = _require_exact_object(
        value,
        "release invocation descriptor",
        ("claim", "invocation_id", "logical_name", "resources", "schema", "scope"),
    )
    if descriptor["schema"] != DESCRIPTOR_SCHEMA or descriptor["claim"] != CLAIM:
        fail("unsupported release invocation descriptor schema or claim")
    invocation_id = descriptor["invocation_id"]
    if not _valid_digest(invocation_id):
        fail("release invocation id must be 256 bits of lowercase hexadecimal")
    logical_name = descriptor["logical_name"]
    if not isinstance(logical_name, str) or not NAME_RE.fullmatch(logical_name):
        fail("release invocation logical name is unsafe or non-canonical")
    scope = _require_exact_object(
        descriptor["scope"], "release invocation scope", ("does_not_claim",)
    )
    if scope["does_not_claim"] != list(NON_CLAIMS):
        fail("release invocation scope is invalid or overstated")
    _validate_resources(descriptor["resources"], logical_name, invocation_id)
    return descriptor


def _validate_reason(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 256
        or value != value.strip()
        or _contains_control(value)
        or not value.isascii()
    ):
        fail(f"{label} must be 1..256 canonical printable ASCII characters")
    return value


def _validate_state(
    value: object, descriptor: Dict[str, Any], descriptor_digest: str
) -> Dict[str, Any]:
    state = _require_exact_object(
        value,
        "release invocation GC state",
        ("descriptor_sha256", "garbage_collection", "invocation_id", "schema"),
    )
    if state["schema"] != STATE_SCHEMA:
        fail("unsupported release invocation GC-state schema")
    if state["invocation_id"] != descriptor["invocation_id"]:
        fail("release invocation GC state is bound to a different invocation")
    if state["descriptor_sha256"] != descriptor_digest:
        fail("release invocation GC state descriptor binding does not match")
    gc = _require_exact_object(
        state["garbage_collection"],
        "release invocation garbage_collection state",
        ("eligible", "reason"),
    )
    if type(gc["eligible"]) is not bool:
        fail("release invocation GC eligibility must be a boolean")
    reason = _validate_reason(gc["reason"], "release invocation GC reason")
    if not gc["eligible"] and reason != "not-explicitly-marked":
        fail("ineligible release invocation has a non-canonical GC reason")
    if gc["eligible"] and reason == "not-explicitly-marked":
        fail("eligible release invocation requires an explicit GC reason")
    return state


def _validate_capability(
    value: object,
    stage: pathlib.Path,
    descriptor: Dict[str, Any],
    descriptor_digest: str,
) -> Dict[str, Any]:
    capability = _require_exact_object(
        value,
        "release invocation capability",
        ("descriptor_sha256", "invocation_id", "schema", "stage_path", "token"),
    )
    if capability["schema"] != CAPABILITY_SCHEMA:
        fail("unsupported release invocation capability schema")
    if capability["stage_path"] != str(stage):
        fail("release invocation capability is bound to a different stage")
    if capability["invocation_id"] != descriptor["invocation_id"]:
        fail("release invocation capability is bound to a different invocation")
    if capability["descriptor_sha256"] != descriptor_digest:
        fail("release invocation capability descriptor binding does not match")
    if not _valid_digest(capability["token"]):
        fail("release invocation capability token is malformed")
    return capability


def _expected_stage_name(descriptor: Dict[str, Any]) -> str:
    return f"{STAGE_PREFIX}{descriptor['logical_name']}-{descriptor['invocation_id']}"


def _inspect_exact_stage(stage: pathlib.Path) -> None:
    expected = {DESCRIPTOR_NAME, STATE_NAME}
    actual = set()
    with os.scandir(stage) as entries:
        for entry in entries:
            actual.add(entry.name)
            metadata = os.lstat(stage / entry.name)
            if entry.name not in expected:
                if stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
                    fail(
                        f"release invocation stage contains an unexpected link: {entry.name}"
                    )
                if not stat.S_ISREG(metadata.st_mode):
                    fail(
                        f"release invocation stage contains an unexpected special entry: {entry.name}"
                    )
            else:
                _check_file(metadata, f"release invocation stage file {entry.name}")
    if actual != expected:
        fail(
            "release invocation stage tree is not exact "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )


def create_invocation(
    stage_parent: pathlib.Path, logical_name: str
) -> CreatedInvocation:
    if not isinstance(logical_name, str) or not NAME_RE.fullmatch(logical_name):
        fail(
            "logical name must match [a-z0-9][a-z0-9-]{0,31}; "
            "normalization is intentionally not implicit"
        )
    parent = assert_no_symlink_components(
        stage_parent, "release invocation stage parent"
    )
    _check_directory(os.lstat(parent), "release invocation stage parent", private=False)
    capability_directory = _ensure_capability_directory(parent)

    for _attempt in range(32):
        invocation_id = secrets.token_hex(32)
        if not _valid_digest(invocation_id):
            fail("cryptographic invocation-id generator returned malformed data")
        stage = parent / f"{STAGE_PREFIX}{logical_name}-{invocation_id}"
        capability = capability_directory / f"{stage.name}{CAPABILITY_SUFFIX}"
        assert_no_symlink_components(
            stage, "release invocation stage", allow_missing_leaf=True
        )
        assert_no_symlink_components(
            capability, "release invocation capability", allow_missing_leaf=True
        )
        try:
            os.mkdir(stage, 0o700)
        except FileExistsError:
            continue
        _set_private_directory(stage)
        descriptor_path = stage / DESCRIPTOR_NAME
        state_path = stage / STATE_NAME
        created = []
        try:
            descriptor = _descriptor(logical_name, invocation_id)
            descriptor_raw = canonical_bytes(descriptor)
            descriptor_digest = sha256_bytes(descriptor_raw)
            _write_exclusive(descriptor_path, descriptor_raw)
            created.append(descriptor_path)
            _write_exclusive(
                state_path,
                canonical_bytes(_initial_state(invocation_id, descriptor_digest)),
            )
            created.append(state_path)
            _fsync_directory(stage)
            _write_exclusive(
                capability,
                canonical_bytes(_capability(stage, invocation_id, descriptor_digest)),
            )
            created.append(capability)
            _fsync_directory(capability_directory)
            _fsync_directory(parent)
            verified = verify_invocation(stage)
            verify_capability(stage, capability, verified)
            return CreatedInvocation(
                stage=stage,
                descriptor=descriptor_path,
                capability=capability,
                invocation_id=invocation_id,
                resources=descriptor["resources"],
            )
        except FileExistsError:
            for path in reversed(created):
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass
            try:
                stage.rmdir()
            except OSError:
                pass
            continue
        except BaseException:
            # Remove only files this invocation created by exact path.  An
            # unsafe or externally modified partial stage is deliberately left
            # private for investigation rather than recursively traversed.
            for path in reversed(created):
                try:
                    metadata = os.lstat(path)
                    if stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1:
                        path.unlink()
                except (FileNotFoundError, OSError):
                    pass
            try:
                stage.rmdir()
            except OSError:
                pass
            raise
    fail("could not allocate a unique release invocation after 32 attempts")


def verify_invocation(stage_value: pathlib.Path) -> VerifiedInvocation:
    stage = assert_no_symlink_components(stage_value, "release invocation stage")
    stage_metadata = os.lstat(stage)
    _check_directory(stage_metadata, "release invocation stage", private=True)
    _inspect_exact_stage(stage)

    descriptor_raw, _ = _read_owned_file(
        stage / DESCRIPTOR_NAME, "release invocation descriptor"
    )
    descriptor = _validate_descriptor(
        _parse_canonical(descriptor_raw, "release invocation descriptor")
    )
    if stage.name != _expected_stage_name(descriptor):
        fail("release invocation stage name is not canonically descriptor-derived")
    descriptor_digest = sha256_bytes(descriptor_raw)
    state_raw, _ = _read_owned_file(stage / STATE_NAME, "release invocation GC state")
    state = _validate_state(
        _parse_canonical(state_raw, "release invocation GC state"),
        descriptor,
        descriptor_digest,
    )
    _inspect_exact_stage(stage)
    return VerifiedInvocation(stage=stage, descriptor=descriptor, state=state)


def verify_audit_descriptor(descriptor_value: pathlib.Path) -> Dict[str, Any]:
    """Validate a retained canonical descriptor, without claiming live authority."""

    path = assert_no_symlink_components(
        descriptor_value, "retained release invocation descriptor"
    )
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or _is_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
        or metadata.st_size > MAX_CONTROL_BYTES
    ):
        fail(
            "retained release invocation descriptor must be a bounded single-link regular file"
        )
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor_fd = os.open(path, flags)
    try:
        before = os.fstat(descriptor_fd)
        raw = b""
        while len(raw) <= MAX_CONTROL_BYTES:
            chunk = os.read(
                descriptor_fd,
                min(64 * 1024, MAX_CONTROL_BYTES + 1 - len(raw)),
            )
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor_fd)
    finally:
        os.close(descriptor_fd)
    current = os.lstat(path)
    if (
        len(raw) > MAX_CONTROL_BYTES
        or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)
    ):
        fail("retained release invocation descriptor changed while being read")
    descriptor = _validate_descriptor(
        _parse_canonical(raw, "retained release invocation descriptor")
    )
    if raw != canonical_bytes(descriptor):
        fail("retained release invocation descriptor is not canonical JSON")
    return descriptor


def materialize_audit_verification_stage(
    descriptor: Dict[str, Any], parent_value: pathlib.Path
) -> AuditVerificationStage:
    """Create only the two exact files needed to reuse structural verification.

    This reconstructed directory is not a capability-owned release invocation
    and must never be accepted by a build driver.  It exists solely inside the
    explicit post-cleanup audit path.
    """

    descriptor = _validate_descriptor(descriptor)
    parent = assert_no_symlink_components(parent_value, "audit reconstruction parent")
    _check_directory(os.lstat(parent), "audit reconstruction parent", private=True)
    stage = parent / _expected_stage_name(descriptor)
    assert_no_symlink_components(
        stage, "audit reconstruction stage", allow_missing_leaf=True
    )
    os.mkdir(stage, 0o700)
    _set_private_directory(stage)
    descriptor_raw = canonical_bytes(descriptor)
    digest = sha256_bytes(descriptor_raw)
    created: list[pathlib.Path] = []
    try:
        descriptor_path = stage / DESCRIPTOR_NAME
        state_path = stage / STATE_NAME
        _write_exclusive(descriptor_path, descriptor_raw)
        created.append(descriptor_path)
        _write_exclusive(
            state_path,
            canonical_bytes(_initial_state(descriptor["invocation_id"], digest)),
        )
        created.append(state_path)
        _fsync_directory(stage)
        verify_invocation(stage)
        return AuditVerificationStage(stage, descriptor)
    except BaseException:
        for path in reversed(created):
            try:
                metadata = os.lstat(path)
                if stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1:
                    path.unlink()
            except (FileNotFoundError, OSError):
                pass
        try:
            stage.rmdir()
        except OSError:
            pass
        raise


def destroy_audit_verification_stage(stage_value: pathlib.Path) -> None:
    """Remove only an exact two-file audit reconstruction."""

    verified = verify_invocation(stage_value)
    for name in (STATE_NAME, DESCRIPTOR_NAME):
        path = verified.stage / name
        metadata = os.lstat(path)
        _check_file(metadata, f"audit reconstruction {name}")
        path.unlink()
    verified.stage.rmdir()


def verify_capability(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    verified: Optional[VerifiedInvocation] = None,
) -> Dict[str, Any]:
    stage = lexical_absolute(stage_value)
    expected = capability_path(stage)
    supplied = lexical_absolute(capability_value)
    if supplied != expected:
        fail(
            "release invocation capability path is not the out-of-stage "
            f"capability bound to this stage: expected {expected}, found {supplied}"
        )
    record = verified or verify_invocation(stage)
    raw, _ = _read_owned_file(supplied, "release invocation capability")
    descriptor_digest = sha256_bytes(canonical_bytes(record.descriptor))
    return _validate_capability(
        _parse_canonical(raw, "release invocation capability"),
        record.stage,
        record.descriptor,
        descriptor_digest,
    )


def _replace_state(stage: pathlib.Path, value: Dict[str, Any]) -> None:
    destination = stage / STATE_NAME
    temporary = stage / f".{STATE_NAME}.{secrets.token_hex(16)}.next"
    assert_no_symlink_components(
        temporary, "temporary release invocation GC state", allow_missing_leaf=True
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


def mark_gc_eligible(
    stage_value: pathlib.Path, capability_value: pathlib.Path, reason: str
) -> VerifiedInvocation:
    reason = _validate_reason(reason, "explicit GC eligibility reason")
    if reason == "not-explicitly-marked":
        fail(
            "explicit GC eligibility reason must describe the completed external checks"
        )
    verified = verify_invocation(stage_value)
    verify_capability(verified.stage, capability_value, verified)
    if verified.state["garbage_collection"]["eligible"]:
        fail("release invocation is already explicitly GC-eligible")
    new_state = {
        "descriptor_sha256": verified.state["descriptor_sha256"],
        "garbage_collection": {"eligible": True, "reason": reason},
        "invocation_id": verified.descriptor["invocation_id"],
        "schema": STATE_SCHEMA,
    }
    _replace_state(verified.stage, new_state)
    updated = verify_invocation(verified.stage)
    verify_capability(updated.stage, capability_value, updated)
    return updated


def _same_identity(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def _unlink_verified(path: pathlib.Path, expected: os.stat_result, label: str) -> None:
    current = os.lstat(path)
    _check_file(current, label)
    if not _same_identity(current, expected):
        fail(f"{label} changed before unlink")
    if os.name == "posix" and os.unlink in os.supports_dir_fd:
        flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        parent_descriptor = os.open(path.parent, flags)
        try:
            current_at = os.stat(
                path.name, dir_fd=parent_descriptor, follow_symlinks=False
            )
            _check_file(current_at, label)
            if not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative unlink")
            os.unlink(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        os.unlink(path)


def _rmdir_verified(path: pathlib.Path, expected: os.stat_result, label: str) -> None:
    current = os.lstat(path)
    _check_directory(current, label, private=True)
    if not _same_identity(current, expected):
        fail(f"{label} changed before removal")
    if os.name == "posix" and os.rmdir in os.supports_dir_fd:
        flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        parent_descriptor = os.open(path.parent, flags)
        try:
            current_at = os.stat(
                path.name, dir_fd=parent_descriptor, follow_symlinks=False
            )
            _check_directory(current_at, label, private=True)
            if not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative removal")
            os.rmdir(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        os.rmdir(path)


def destroy_invocation(
    stage_value: pathlib.Path, capability_value: pathlib.Path
) -> None:
    verified = verify_invocation(stage_value)
    if not verified.state["garbage_collection"]["eligible"]:
        fail(
            "release invocation is not explicitly GC-eligible; external resource "
            "liveness and retention checks must complete before local-stage destruction"
        )
    verify_capability(verified.stage, capability_value, verified)
    _inspect_exact_stage(verified.stage)

    descriptor_path = verified.stage / DESCRIPTOR_NAME
    state_path = verified.stage / STATE_NAME
    capability = capability_path(verified.stage)
    descriptor_raw, descriptor_metadata = _read_owned_file(
        descriptor_path, "destroy release invocation descriptor"
    )
    final_descriptor = _validate_descriptor(
        _parse_canonical(descriptor_raw, "destroy release invocation descriptor")
    )
    if verified.stage.name != _expected_stage_name(final_descriptor):
        fail("destroy release invocation stage name is not descriptor-derived")
    descriptor_digest = sha256_bytes(descriptor_raw)
    state_raw, state_metadata = _read_owned_file(
        state_path, "destroy release invocation GC state"
    )
    final_state = _validate_state(
        _parse_canonical(state_raw, "destroy release invocation GC state"),
        final_descriptor,
        descriptor_digest,
    )
    if not final_state["garbage_collection"]["eligible"]:
        fail("release invocation GC eligibility changed before destruction")
    capability_raw, capability_metadata = _read_owned_file(
        capability, "destroy release invocation capability"
    )
    _validate_capability(
        _parse_canonical(capability_raw, "destroy release invocation capability"),
        verified.stage,
        final_descriptor,
        descriptor_digest,
    )
    stage_metadata = os.lstat(verified.stage)
    _check_directory(stage_metadata, "destroy release invocation stage", private=True)

    # This exact, non-recursive list is the entire deletion surface.  Docker
    # volume names and output-stage names are descriptor strings only.
    _unlink_verified(
        descriptor_path, descriptor_metadata, "destroy release invocation descriptor"
    )
    _unlink_verified(state_path, state_metadata, "destroy release invocation GC state")
    _rmdir_verified(verified.stage, stage_metadata, "destroy release invocation stage")
    _unlink_verified(
        capability, capability_metadata, "destroy release invocation capability"
    )
    _fsync_directory(verified.stage.parent)


def _validate_result(value: object) -> Dict[str, Any]:
    result = _require_exact_object(
        value,
        "release invocation creation result",
        ("capability", "descriptor", "invocation_id", "resources", "schema", "stage"),
    )
    if result["schema"] != RESULT_SCHEMA or not _valid_digest(result["invocation_id"]):
        fail("release invocation creation result schema or id is invalid")
    for key in ("capability", "descriptor", "stage"):
        _require_safe_scalar(result[key], f"release invocation result {key}")
        if not pathlib.Path(result[key]).is_absolute():
            fail(f"release invocation result {key} must be absolute")
    stage = lexical_absolute(pathlib.Path(result["stage"]))
    if lexical_absolute(pathlib.Path(result["descriptor"])) != stage / DESCRIPTOR_NAME:
        fail("release invocation result descriptor path is not canonical")
    if lexical_absolute(pathlib.Path(result["capability"])) != capability_path(stage):
        fail("release invocation result capability path is not canonical")
    resources = result["resources"]
    if not isinstance(resources, dict):
        fail("release invocation result resources must be an object")
    # Derive the logical name from the canonical stage basename, then validate
    # the complete resource object rather than trusting arbitrary query data.
    suffix = f"-{result['invocation_id']}"
    if not stage.name.startswith(STAGE_PREFIX) or not stage.name.endswith(suffix):
        fail("release invocation result stage name is not canonical")
    logical_name = stage.name[len(STAGE_PREFIX) : -len(suffix)]
    if not NAME_RE.fullmatch(logical_name):
        fail("release invocation result logical name is invalid")
    _validate_resources(resources, logical_name, result["invocation_id"])
    return result


QUERY_FIELDS = (
    "buildroot_volume",
    "capability",
    "cargo_volume",
    "descriptor",
    "gc_eligible",
    "gc_reason",
    "invocation_id",
    "logical_name",
    "output_stage_name",
    "result_name",
    "results_volume",
    "stage",
)


def _descriptor_query(verified: VerifiedInvocation, field: str) -> object:
    flattened = _flatten_resources(verified.descriptor["resources"])
    values = {
        **flattened,
        "capability": str(capability_path(verified.stage)),
        "descriptor": str(verified.stage / DESCRIPTOR_NAME),
        "gc_eligible": verified.state["garbage_collection"]["eligible"],
        "gc_reason": verified.state["garbage_collection"]["reason"],
        "invocation_id": verified.descriptor["invocation_id"],
        "logical_name": verified.descriptor["logical_name"],
        "stage": str(verified.stage),
    }
    return values[field]


def _result_query(result: Dict[str, Any], field: str) -> object:
    flattened = _flatten_resources(result["resources"])
    values = {
        **flattened,
        "capability": result["capability"],
        "descriptor": result["descriptor"],
        "invocation_id": result["invocation_id"],
        "stage": result["stage"],
    }
    if field not in values:
        fail(f"field {field} is not available in immutable create-result JSON")
    return values[field]


def _print_scalar(value: object) -> None:
    if type(value) is bool:
        text = "true" if value else "false"
    elif isinstance(value, str):
        text = value
    else:
        fail("release invocation query did not select a safe scalar")
    if not text or _contains_control(text):
        fail("release invocation query result is unsafe for shell transport")
    print(text)


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)

    create = commands.add_parser("create", help="allocate an invocation control stage")
    create.add_argument("--stage-parent", required=True)
    create.add_argument("--name", required=True)

    verify = commands.add_parser("verify", help="verify exact invocation control state")
    verify.add_argument("--require-gc-eligible", action="store_true")
    verify.add_argument("stage")

    query = commands.add_parser("query", help="query one verified invocation scalar")
    query.add_argument("--field", required=True, choices=QUERY_FIELDS)
    query.add_argument("stage")

    query_result = commands.add_parser(
        "query-result", help="query canonical create-result JSON from stdin"
    )
    query_result.add_argument(
        "--field",
        required=True,
        choices=(
            "buildroot_volume",
            "capability",
            "cargo_volume",
            "descriptor",
            "invocation_id",
            "output_stage_name",
            "result_name",
            "results_volume",
            "stage",
        ),
    )

    mark = commands.add_parser(
        "mark-gc-eligible",
        help="record explicit external cleanup/retention checks as complete",
    )
    mark.add_argument("--capability", required=True)
    mark.add_argument("--reason", required=True)
    mark.add_argument("stage")

    destroy = commands.add_parser(
        "destroy", help="destroy only an explicitly eligible local control stage"
    )
    destroy.add_argument("--capability", required=True)
    destroy.add_argument("stage")
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "create":
            created = create_invocation(pathlib.Path(args.stage_parent), args.name)
            print(canonical_bytes(created.cli_result()).decode("ascii"), end="")
        elif args.command == "verify":
            verified = verify_invocation(pathlib.Path(args.stage))
            if (
                args.require_gc_eligible
                and not verified.state["garbage_collection"]["eligible"]
            ):
                fail("release invocation is not explicitly GC-eligible")
            print(
                "release invocation verified: "
                f"id={verified.descriptor['invocation_id']} "
                f"gc_eligible={str(verified.state['garbage_collection']['eligible']).lower()}"
            )
        elif args.command == "query":
            verified = verify_invocation(pathlib.Path(args.stage))
            _print_scalar(_descriptor_query(verified, args.field))
        elif args.command == "query-result":
            raw = sys.stdin.buffer.read(MAX_CONTROL_BYTES + 1).replace(b"\r\n", b"\n")
            if len(raw) > MAX_CONTROL_BYTES:
                fail("release invocation creation result is too large")
            result = _validate_result(
                _parse_canonical(raw, "release invocation creation result")
            )
            _print_scalar(_result_query(result, args.field))
        elif args.command == "mark-gc-eligible":
            updated = mark_gc_eligible(
                pathlib.Path(args.stage), pathlib.Path(args.capability), args.reason
            )
            print(
                "release invocation explicitly GC-eligible: "
                f"id={updated.descriptor['invocation_id']}"
            )
        else:
            destroy_invocation(pathlib.Path(args.stage), pathlib.Path(args.capability))
        return 0
    except (InvocationError, OSError, KeyError, TypeError, ValueError) as error:
        print(f"ERROR: release invocation: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
