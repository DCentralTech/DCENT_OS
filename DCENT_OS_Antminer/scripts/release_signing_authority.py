#!/usr/bin/env python3
"""Snapshot one invocation-scoped Ed25519 release signing authority.

The input pathnames are untrusted mutable authorities.  ``create`` opens each
source exactly once with no-follow/non-blocking semantics, proves a bounded
single-link regular-file handle remained stable, and writes private immutable
copies into a capability-owned stage bound to one release invocation. Creation
publishes an identity-bound capability before the descriptor and keys, can emit
a caller-chosen durable recovery result, and resumes exact partial work.

The stage is deliberately not portable release evidence.  It contains the
private key, must never be published, and is destroyed before the invocation
control stage.  The public key used to verify a published release remains an
out-of-band trust authority.

Destruction retains the descriptor as its authenticated deletion plan until the
two key copies are gone, removes the capability last, and is safe to retry after
any completed prefix of that sequence.
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import pathlib
import secrets
import stat
import sys
from typing import Any, Callable, Dict, Iterable, NamedTuple, NoReturn, Optional

import atomic_publish_directory
import release_invocation
import release_set_publication


DESCRIPTOR_SCHEMA = "org.dcentral.dcentos.release-signing-authority.v1"
LEGACY_CAPABILITY_SCHEMA = (
    "org.dcentral.dcentos.release-signing-authority-capability.v1"
)
CAPABILITY_SCHEMA = "org.dcentral.dcentos.release-signing-authority-capability.v2"
RESULT_SCHEMA = "org.dcentral.dcentos.release-signing-authority-result.v1"
STAGE_PREFIX = "dcentos-release-signing-authority-"
CAPABILITY_DIRECTORY = ".dcentos-release-signing-authority-capabilities"
CAPABILITY_SUFFIX = ".capability.json"
DESCRIPTOR_NAME = "authority.json"
PRIVATE_KEY_NAME = "private-key.pem"
PUBLIC_KEY_NAME = "public-key.pem"
MAX_KEY_BYTES = 64 * 1024
MAX_CONTROL_BYTES = 64 * 1024
HEX_64 = frozenset("0123456789abcdef")
CLAIM = "stable-invocation-scoped-copies-of-an-admitted-signing-keypair"
NON_CLAIMS = (
    "private-key-origin-or-custody-before-snapshot",
    "key-confidentiality-outside-the-private-local-stage",
    "secure-erasure-or-storage-media-remanence-after-unlink",
    "cryptographic-keypair-consistency",
    "public-key-trust-or-distribution",
    "build-execution-or-artifact-causality",
    "publication-or-post-cleanup-verification",
)
Hook = Optional[Callable[[], None]]


class SigningAuthorityError(ValueError):
    """A signing-authority source, stage, or capability was unsafe."""


class VerifiedAuthority(NamedTuple):
    stage: pathlib.Path
    descriptor: Dict[str, Any]
    private_key: pathlib.Path
    public_key: pathlib.Path


class CreatedAuthority(NamedTuple):
    stage: pathlib.Path
    descriptor: pathlib.Path
    private_key: pathlib.Path
    public_key: pathlib.Path
    capability: pathlib.Path
    authority_id: str
    invocation_id: str

    def cli_result(self) -> Dict[str, Any]:
        return {
            "authority_id": self.authority_id,
            "capability": str(self.capability),
            "descriptor": str(self.descriptor),
            "invocation_id": self.invocation_id,
            "private_key": str(self.private_key),
            "public_key": str(self.public_key),
            "schema": RESULT_SCHEMA,
            "stage": str(self.stage),
        }


def fail(message: str) -> NoReturn:
    raise SigningAuthorityError(message)


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
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


def _is_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & marker)


def _lexical_absolute(value: pathlib.Path) -> pathlib.Path:
    return pathlib.Path(os.path.abspath(str(value)))


def _assert_no_alias_components(
    value: pathlib.Path, label: str, *, allow_missing_leaf: bool = False
) -> pathlib.Path:
    path = _lexical_absolute(value)
    if not str(path) or any(ord(character) < 0x20 for character in str(path)):
        fail(f"{label} path is empty or contains control characters")
    parts = path.parts
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
            fail(f"{label} contains a symlink or reparse-point component: {cursor}")
    resolved = pathlib.Path(os.path.realpath(str(path)))
    if os.path.normcase(str(path)) != os.path.normcase(str(resolved)):
        fail(f"{label} contains an aliased path component: {path}")
    return path


def _check_directory(metadata: os.stat_result, label: str, *, private: bool) -> None:
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or _is_reparse(metadata)
    ):
        fail(f"{label} must be a non-symlink, non-reparse directory")
    if hasattr(os, "geteuid") and hasattr(metadata, "st_uid"):
        if metadata.st_uid != os.geteuid():
            fail(f"{label} is not owned by the current user")
    if private and os.name == "posix" and stat.S_IMODE(metadata.st_mode) != 0o700:
        fail(f"{label} mode must be 0700")


def _require_private_windows_path(path: pathlib.Path, label: str) -> None:
    if os.name != "nt":
        return
    try:
        release_set_publication.require_private_windows_acl(path, label)
    except release_set_publication.ReleaseSetError as error:
        fail(str(error))


def _check_regular(
    metadata: os.stat_result, label: str, *, bounded: int, owned: bool
) -> None:
    if not stat.S_ISREG(metadata.st_mode) or _is_reparse(metadata):
        fail(f"{label} must be a non-reparse regular file")
    if getattr(metadata, "st_nlink", 1) != 1:
        fail(f"{label} must have exactly one filesystem link")
    if metadata.st_size <= 0 or metadata.st_size > bounded:
        fail(f"{label} must contain 1..{bounded} bytes")
    if owned:
        if hasattr(os, "geteuid") and hasattr(metadata, "st_uid"):
            if metadata.st_uid != os.geteuid():
                fail(f"{label} is not owned by the current user")
        if os.name == "posix" and stat.S_IMODE(metadata.st_mode) != 0o400:
            fail(f"{label} mode must be 0400")


def _stable_signature(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        getattr(metadata, "st_nlink", 1),
        metadata.st_size,
        getattr(metadata, "st_mtime_ns", int(metadata.st_mtime * 1_000_000_000)),
        getattr(metadata, "st_ctime_ns", int(metadata.st_ctime * 1_000_000_000)),
    )


def _read_source_file(
    value: pathlib.Path,
    label: str,
    *,
    private: bool,
    after_open: Hook = None,
) -> bytes:
    path = _assert_no_alias_components(value, label)
    initial = os.lstat(path)
    _check_regular(initial, label, bounded=MAX_KEY_BYTES, owned=False)
    if private and os.name == "posix" and stat.S_IMODE(initial.st_mode) & 0o077:
        fail(f"{label} must not be accessible by group or other users")
    if private:
        _require_private_windows_path(path, label)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if os.name == "posix" and getattr(os, "O_NOFOLLOW", 0) == 0:
        fail(f"{label}: platform lacks O_NOFOLLOW")
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        _check_regular(opened, label, bounded=MAX_KEY_BYTES, owned=False)
        if (initial.st_dev, initial.st_ino) != (opened.st_dev, opened.st_ino):
            fail(f"{label} changed while being opened")
        if after_open is not None:
            after_open()
        raw = b""
        while len(raw) <= MAX_KEY_BYTES:
            chunk = os.read(descriptor, min(65536, MAX_KEY_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        final_handle = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    final_path = os.lstat(path)
    _check_regular(final_handle, label, bounded=MAX_KEY_BYTES, owned=False)
    _check_regular(final_path, label, bounded=MAX_KEY_BYTES, owned=False)
    if len(raw) == 0 or len(raw) > MAX_KEY_BYTES:
        fail(f"{label} must contain 1..{MAX_KEY_BYTES} bytes")
    if _stable_signature(opened) != _stable_signature(final_handle):
        fail(f"{label} changed while being snapshotted")
    if (final_handle.st_dev, final_handle.st_ino) != (
        final_path.st_dev,
        final_path.st_ino,
    ):
        fail(f"{label} pathname was replaced while being snapshotted")
    if private:
        _require_private_windows_path(path, label)
    return raw


def _set_private_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        flags = (
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
        )
        descriptor = os.open(path, flags)
        try:
            os.fchmod(descriptor, 0o700)
        finally:
            os.close(descriptor)
    elif os.name == "nt":
        try:
            atomic_publish_directory.set_windows_directory_acl(
                path, atomic_publish_directory.WINDOWS_PRIVATE_DIRECTORY_SDDL
            )
        except atomic_publish_directory.DirectoryPublishError as error:
            fail(str(error))
        _require_private_windows_path(path, "private signing authority directory")


def _write_exclusive(path: pathlib.Path, raw: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
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
    created_metadata = os.lstat(path)
    try:
        if os.name == "posix":
            os.chmod(path, 0o400, follow_symlinks=False)
        elif os.name == "nt":
            try:
                atomic_publish_directory.set_windows_file_acl(
                    path, atomic_publish_directory.WINDOWS_PRIVATE_FILE_SDDL
                )
            except atomic_publish_directory.DirectoryPublishError as error:
                fail(str(error))
            _require_private_windows_path(path, "private signing authority file")
    except BaseException:
        try:
            _unlink_verified(path, created_metadata, f"failed private file {path.name}")
        except (FileNotFoundError, OSError, SigningAuthorityError):
            pass
        raise


def _fsync_directory(path: pathlib.Path) -> None:
    if os.name == "posix":
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    elif os.name == "nt":
        metadata = os.lstat(path)
        _check_directory(metadata, f"directory durability boundary {path}", private=False)
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


def _read_owned(path: pathlib.Path, label: str) -> bytes:
    path = _assert_no_alias_components(path, label)
    _require_private_windows_path(path, label)
    before = os.lstat(path)
    _check_regular(before, label, bounded=MAX_CONTROL_BYTES, owned=True)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        _check_regular(opened, label, bounded=MAX_CONTROL_BYTES, owned=True)
        if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
            fail(f"{label} changed while being opened")
        raw = b""
        while len(raw) <= MAX_CONTROL_BYTES:
            chunk = os.read(descriptor, min(65536, MAX_CONTROL_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    current = os.lstat(path)
    if (
        len(raw) == 0
        or len(raw) > MAX_CONTROL_BYTES
        or _stable_signature(opened) != _stable_signature(after)
        or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)
    ):
        fail(f"{label} changed while being read")
    _require_private_windows_path(path, label)
    return raw


def _publish_creation_result(path_value: pathlib.Path, raw: bytes) -> pathlib.Path:
    path = _assert_no_alias_components(
        path_value, "release signing authority result output", allow_missing_leaf=True
    )
    parent = _assert_no_alias_components(
        path.parent, "release signing authority result parent"
    )
    _check_directory(
        os.lstat(parent), "release signing authority result parent", private=True
    )
    _require_private_windows_path(parent, "release signing authority result parent")
    if path.name in {"", ".", ".."} or pathlib.PurePath(path.name).name != path.name:
        fail("release signing authority result output name is invalid")

    def existing_matches() -> bool:
        try:
            observed = _read_source_file(
                path, "release signing authority result output", private=True
            )
        except FileNotFoundError:
            return False
        return observed == raw

    if existing_matches():
        _fsync_directory(parent)
        return path
    try:
        release_set_publication.publish_regular_file_noreplace(path, raw, mode=0o600)
    except release_set_publication.ReleaseSetError as error:
        if existing_matches():
            _fsync_directory(parent)
            return path
        fail(f"cannot publish release signing authority result: {error}")
    if not existing_matches():
        fail("published release signing authority result does not match")
    _fsync_directory(parent)
    return path


def _parse_canonical(raw: bytes, label: str) -> Dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    if not isinstance(value, dict) or raw != canonical_bytes(value):
        fail(f"{label} must be a canonical JSON object")
    return value


def _file_evidence(name: str, raw: bytes) -> Dict[str, Any]:
    return {"name": name, "sha256": sha256_bytes(raw), "size": len(raw)}


def _descriptor(
    invocation: release_invocation.VerifiedInvocation,
    private_raw: bytes,
    public_raw: bytes,
) -> Dict[str, Any]:
    invocation_raw = release_invocation.canonical_bytes(invocation.descriptor)
    core = {
        "claim": CLAIM,
        "invocation": {
            "descriptor_sha256": sha256_bytes(invocation_raw),
            "invocation_id": invocation.descriptor["invocation_id"],
        },
        "keys": {
            "private": _file_evidence(PRIVATE_KEY_NAME, private_raw),
            "public": _file_evidence(PUBLIC_KEY_NAME, public_raw),
        },
        "schema": DESCRIPTOR_SCHEMA,
        "scope": {"does_not_claim": list(NON_CLAIMS)},
    }
    return {**core, "authority_id": sha256_bytes(canonical_bytes(core))}


def _validate_descriptor(value: object) -> Dict[str, Any]:
    descriptor = _require_exact_object(
        value,
        "release signing authority descriptor",
        ("authority_id", "claim", "invocation", "keys", "schema", "scope"),
    )
    if descriptor["schema"] != DESCRIPTOR_SCHEMA or descriptor["claim"] != CLAIM:
        fail("unsupported release signing authority descriptor")
    invocation = _require_exact_object(
        descriptor["invocation"],
        "signing authority invocation",
        ("descriptor_sha256", "invocation_id"),
    )
    if not _valid_digest(invocation["descriptor_sha256"]) or not _valid_digest(
        invocation["invocation_id"]
    ):
        fail("release signing authority invocation binding is malformed")
    keys = _require_exact_object(
        descriptor["keys"], "signing authority keys", ("private", "public")
    )
    for key, expected_name in (
        ("private", PRIVATE_KEY_NAME),
        ("public", PUBLIC_KEY_NAME),
    ):
        evidence = _require_exact_object(
            keys[key], f"{key} key evidence", ("name", "sha256", "size")
        )
        if evidence["name"] != expected_name or not _valid_digest(evidence["sha256"]):
            fail(f"{key} key evidence is malformed")
        if isinstance(evidence["size"], bool) or not isinstance(evidence["size"], int):
            fail(f"{key} key size is malformed")
        if evidence["size"] <= 0 or evidence["size"] > MAX_KEY_BYTES:
            fail(f"{key} key size is outside the accepted bound")
    scope = _require_exact_object(
        descriptor["scope"], "signing authority scope", ("does_not_claim",)
    )
    if scope["does_not_claim"] != list(NON_CLAIMS):
        fail("release signing authority non-claims are not exact")
    core = {
        key: descriptor[key]
        for key in ("claim", "invocation", "keys", "schema", "scope")
    }
    if descriptor["authority_id"] != sha256_bytes(canonical_bytes(core)):
        fail("release signing authority identifier does not match its descriptor")
    return descriptor


def _same_identity(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def _check_recoverable_file(
    metadata: os.stat_result, label: str, *, bounded: int
) -> None:
    """Validate an owned stage file, including a crash-shortened empty file."""

    if not stat.S_ISREG(metadata.st_mode) or _is_reparse(metadata):
        fail(f"{label} must be a non-reparse regular file")
    if getattr(metadata, "st_nlink", 1) != 1:
        fail(f"{label} must have exactly one filesystem link")
    if metadata.st_size < 0 or metadata.st_size > bounded:
        fail(f"{label} is larger than the accepted bound")
    if hasattr(os, "geteuid") and hasattr(metadata, "st_uid"):
        if metadata.st_uid != os.geteuid():
            fail(f"{label} is not owned by the current user")
    if os.name == "posix" and stat.S_IMODE(metadata.st_mode) != 0o400:
        fail(f"{label} mode must be 0400")


def _unlink_verified(
    path: pathlib.Path, expected: os.stat_result, label: str
) -> None:
    current = os.lstat(path)
    if not _same_identity(current, expected):
        fail(f"{label} changed before unlink")
    if os.name == "posix" and os.unlink in os.supports_dir_fd:
        flags = (
            os.O_RDONLY
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        parent_descriptor = os.open(path.parent, flags)
        try:
            current_at = os.stat(
                path.name, dir_fd=parent_descriptor, follow_symlinks=False
            )
            if not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative unlink")
            os.unlink(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        if os.name == "nt":
            os.chmod(path, 0o600)
        os.unlink(path)


def _rmdir_verified(
    path: pathlib.Path, expected: os.stat_result, label: str
) -> None:
    current = os.lstat(path)
    if not _same_identity(current, expected):
        fail(f"{label} changed before removal")
    if os.name == "posix" and os.rmdir in os.supports_dir_fd:
        flags = (
            os.O_RDONLY
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        parent_descriptor = os.open(path.parent, flags)
        try:
            current_at = os.stat(
                path.name, dir_fd=parent_descriptor, follow_symlinks=False
            )
            if not _same_identity(current_at, expected):
                fail(f"{label} changed before descriptor-relative removal")
            os.rmdir(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        os.rmdir(path)


def _capability_v2(
    descriptor: Dict[str, Any],
    stage: pathlib.Path,
    stage_metadata: os.stat_result,
    *,
    token: Optional[str] = None,
) -> Dict[str, Any]:
    return {
        "authority_id": descriptor["authority_id"],
        "descriptor_sha256": sha256_bytes(canonical_bytes(descriptor)),
        "invocation_id": descriptor["invocation"]["invocation_id"],
        "schema": CAPABILITY_SCHEMA,
        "stage_dev": stage_metadata.st_dev,
        "stage_ino": stage_metadata.st_ino,
        "stage_name": stage.name,
        "token": token or secrets.token_hex(32),
    }


def _validate_capability_value(value: object) -> Dict[str, Any]:
    if not isinstance(value, dict):
        fail("release signing authority capability must be an object")
    schema = value.get("schema")
    if schema == LEGACY_CAPABILITY_SCHEMA:
        capability = _require_exact_object(
            value,
            "release signing authority capability",
            ("authority_id", "invocation_id", "schema", "stage_name", "token"),
        )
    elif schema == CAPABILITY_SCHEMA:
        capability = _require_exact_object(
            value,
            "release signing authority capability",
            (
                "authority_id",
                "descriptor_sha256",
                "invocation_id",
                "schema",
                "stage_dev",
                "stage_ino",
                "stage_name",
                "token",
            ),
        )
        if not _valid_digest(capability["descriptor_sha256"]):
            fail("release signing authority capability descriptor digest is malformed")
        for field in ("stage_dev", "stage_ino"):
            observed = capability[field]
            if isinstance(observed, bool) or not isinstance(observed, int) or observed < 0:
                fail(f"release signing authority capability {field} is malformed")
    else:
        fail("unsupported release signing authority capability")
    if not _valid_digest(capability["authority_id"]):
        fail("release signing authority capability identifier is malformed")
    if not _valid_digest(capability["invocation_id"]):
        fail("release signing authority capability invocation is malformed")
    if (
        not isinstance(capability["stage_name"], str)
        or not capability["stage_name"].startswith(STAGE_PREFIX)
        or pathlib.PurePath(capability["stage_name"]).name
        != capability["stage_name"]
    ):
        fail("release signing authority capability stage name is malformed")
    if not _valid_digest(capability["token"]):
        fail("release signing authority capability token is malformed")
    return capability


def _capability_matches_descriptor(
    capability: Dict[str, Any],
    descriptor: Dict[str, Any],
    stage: pathlib.Path,
) -> None:
    if capability["stage_name"] != stage.name:
        fail("release signing authority capability is bound to a different stage")
    if capability["authority_id"] != descriptor["authority_id"]:
        fail("release signing authority capability identifier does not match")
    if capability["invocation_id"] != descriptor["invocation"]["invocation_id"]:
        fail("release signing authority capability invocation does not match")
    if capability["schema"] == CAPABILITY_SCHEMA and capability[
        "descriptor_sha256"
    ] != sha256_bytes(canonical_bytes(descriptor)):
        fail("release signing authority capability descriptor digest does not match")


def _replace_owned(path: pathlib.Path, raw: bytes) -> None:
    temporary = path.with_name(f"{path.name}.pending")
    try:
        _write_exclusive(temporary, raw)
    except FileExistsError:
        if _read_owned(temporary, f"pending replacement for {path.name}") != raw:
            fail(f"pending replacement for {path.name} does not match")
    try:
        os.replace(temporary, path)
    except FileNotFoundError:
        if _read_owned(path, f"replacement for {path.name}") != raw:
            raise
    if os.name == "posix":
        os.chmod(path, 0o400, follow_symlinks=False)
    else:
        _require_private_windows_path(path, f"replacement for {path.name}")
    _fsync_directory(path.parent)


def _upgrade_capability(
    path: pathlib.Path,
    legacy: Dict[str, Any],
    descriptor: Dict[str, Any],
    stage: pathlib.Path,
    stage_metadata: os.stat_result,
) -> Dict[str, Any]:
    upgraded = _capability_v2(
        descriptor, stage, stage_metadata, token=legacy["token"]
    )
    _replace_owned(path, canonical_bytes(upgraded))
    return upgraded


def _read_capability_record(path: pathlib.Path, label: str) -> Dict[str, Any]:
    return _validate_capability_value(
        _parse_canonical(_read_owned(path, label), label)
    )


def _publish_capability_noreplace(
    path: pathlib.Path,
    descriptor: Dict[str, Any],
    stage: pathlib.Path,
    stage_metadata: os.stat_result,
) -> Dict[str, Any]:
    """Durably publish the capability without exposing a partial final file."""

    pending = path.with_name(f"{path.name}.pending")
    for _ in range(4):
        try:
            final_metadata = os.lstat(path)
        except FileNotFoundError:
            final_metadata = None
        try:
            pending_metadata = os.lstat(pending)
        except FileNotFoundError:
            pending_metadata = None
        if final_metadata is not None:
            if pending_metadata is not None:
                if _same_identity(final_metadata, pending_metadata):
                    _unlink_verified(
                        pending,
                        pending_metadata,
                        "linked release signing authority capability pending file",
                    )
                    _fsync_directory(path.parent)
                else:
                    final_record = _read_capability_record(
                        path, "release signing authority capability"
                    )
                    if final_record["schema"] == LEGACY_CAPABILITY_SCHEMA:
                        return final_record
                    fail("capability final and pending paths have different identities")
            return _read_capability_record(
                path, "release signing authority capability"
            )
        capability: Optional[Dict[str, Any]] = None
        if pending_metadata is not None:
            _check_recoverable_file(
                pending_metadata,
                "release signing authority capability pending file",
                bounded=MAX_CONTROL_BYTES,
            )
            if pending_metadata.st_size > 0:
                try:
                    capability = _read_capability_record(
                        pending, "release signing authority capability pending file"
                    )
                    _capability_matches_descriptor(capability, descriptor, stage)
                    if capability["schema"] != CAPABILITY_SCHEMA or (
                        capability["stage_dev"], capability["stage_ino"]
                    ) != (stage_metadata.st_dev, stage_metadata.st_ino):
                        fail("pending capability is bound to a different stage")
                except SigningAuthorityError:
                    capability = None
            if capability is None:
                _unlink_verified(
                    pending,
                    pending_metadata,
                    "incomplete release signing authority capability pending file",
                )
                _fsync_directory(path.parent)
                continue
        else:
            capability = _capability_v2(descriptor, stage, stage_metadata)
            _write_exclusive(pending, canonical_bytes(capability))
            _fsync_directory(path.parent)
        try:
            os.link(pending, path, follow_symlinks=False)
        except FileExistsError:
            continue
        final_metadata = os.lstat(path)
        pending_metadata = os.lstat(pending)
        if not _same_identity(final_metadata, pending_metadata):
            fail("published capability identity differs from its pending file")
        _fsync_directory(path.parent)
        _unlink_verified(
            pending,
            pending_metadata,
            "published release signing authority capability pending file",
        )
        _fsync_directory(path.parent)
        return _read_capability_record(path, "release signing authority capability")
    fail("could not converge release signing authority capability publication")


def _expected_stage_name(descriptor: Dict[str, Any]) -> str:
    return f"{STAGE_PREFIX}{descriptor['invocation']['invocation_id']}-{descriptor['authority_id']}"


def capability_path(stage: pathlib.Path) -> pathlib.Path:
    return stage.parent / CAPABILITY_DIRECTORY / f"{stage.name}{CAPABILITY_SUFFIX}"


def _inspect_exact_stage(stage: pathlib.Path) -> None:
    expected = {DESCRIPTOR_NAME, PRIVATE_KEY_NAME, PUBLIC_KEY_NAME}
    actual = set()
    with os.scandir(stage) as entries:
        for entry in entries:
            actual.add(entry.name)
            metadata = os.lstat(stage / entry.name)
            if entry.name in expected:
                _require_private_windows_path(
                    stage / entry.name, f"signing authority {entry.name}"
                )
                _check_regular(
                    metadata,
                    f"signing authority {entry.name}",
                    bounded=MAX_CONTROL_BYTES,
                    owned=True,
                )
            elif stat.S_ISLNK(metadata.st_mode) or _is_reparse(metadata):
                fail(
                    f"release signing authority stage contains an unexpected link: {entry.name}"
                )
            elif not stat.S_ISREG(metadata.st_mode):
                fail(
                    f"release signing authority stage contains an unexpected special entry: {entry.name}"
                )
    if actual != expected:
        fail(
            "release signing authority stage is not exact "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )


def _inspect_recoverable_stage(
    stage: pathlib.Path, expected: Dict[str, bytes], *, repair: bool
) -> None:
    actual: set[str] = set()
    with os.scandir(stage) as entries:
        for entry in entries:
            actual.add(entry.name)
            if entry.name not in expected:
                fail(
                    "release signing authority stage contains an unexpected entry: "
                    f"{entry.name}"
                )
            path = stage / entry.name
            metadata = os.lstat(path)
            _require_private_windows_path(
                path, f"recoverable signing authority {entry.name}"
            )
            _check_recoverable_file(
                metadata,
                f"recoverable signing authority {entry.name}",
                bounded=MAX_CONTROL_BYTES,
            )
            matches = False
            if metadata.st_size > 0:
                try:
                    matches = (
                        _read_owned(path, f"recoverable signing authority {entry.name}")
                        == expected[entry.name]
                    )
                except SigningAuthorityError:
                    matches = False
            if not matches:
                if not repair:
                    fail(
                        "existing signing authority stage file does not match the "
                        f"requested authority: {entry.name}"
                    )
                _unlink_verified(
                    path, metadata, f"recoverable signing authority {entry.name}"
                )
                _fsync_directory(stage)


def _ensure_stage_file(path: pathlib.Path, raw: bytes) -> None:
    try:
        _write_exclusive(path, raw)
    except FileExistsError:
        observed = _read_owned(path, f"existing signing authority {path.name}")
        if observed != raw:
            fail(f"existing signing authority {path.name} does not match")


def verify_authority(
    stage_value: pathlib.Path,
    invocation_stage: Optional[pathlib.Path] = None,
) -> VerifiedAuthority:
    stage = _assert_no_alias_components(stage_value, "release signing authority stage")
    _check_directory(os.lstat(stage), "release signing authority stage", private=True)
    _require_private_windows_path(stage, "release signing authority stage")
    _inspect_exact_stage(stage)
    descriptor_raw = _read_owned(
        stage / DESCRIPTOR_NAME, "release signing authority descriptor"
    )
    descriptor = _validate_descriptor(
        _parse_canonical(descriptor_raw, "release signing authority descriptor")
    )
    if stage.name != _expected_stage_name(descriptor):
        fail("release signing authority stage name is not descriptor-derived")
    for key, filename in (("private", PRIVATE_KEY_NAME), ("public", PUBLIC_KEY_NAME)):
        raw = _read_owned(stage / filename, f"release signing authority {key} key")
        evidence = descriptor["keys"][key]
        if len(raw) != evidence["size"] or sha256_bytes(raw) != evidence["sha256"]:
            fail(f"release signing authority {key} key does not match its descriptor")
    if invocation_stage is not None:
        invocation = release_invocation.verify_invocation(invocation_stage)
        binding = descriptor["invocation"]
        if binding["invocation_id"] != invocation.descriptor["invocation_id"]:
            fail("release signing authority is bound to a different invocation")
        if binding["descriptor_sha256"] != sha256_bytes(
            release_invocation.canonical_bytes(invocation.descriptor)
        ):
            fail(
                "release signing authority invocation descriptor binding does not match"
            )
    _inspect_exact_stage(stage)
    return VerifiedAuthority(
        stage, descriptor, stage / PRIVATE_KEY_NAME, stage / PUBLIC_KEY_NAME
    )


def _ensure_capability_directory(parent: pathlib.Path) -> pathlib.Path:
    directory = parent / CAPABILITY_DIRECTORY
    created = False
    try:
        os.mkdir(directory, 0o700)
        created = True
    except FileExistsError:
        pass
    _set_private_directory(directory)
    directory = _assert_no_alias_components(
        directory, "signing authority capability directory"
    )
    _check_directory(
        os.lstat(directory), "signing authority capability directory", private=True
    )
    _require_private_windows_path(
        directory, "signing authority capability directory"
    )
    if created:
        _fsync_directory(parent)
    return directory


def create_authority(
    stage_parent: pathlib.Path,
    invocation_stage: pathlib.Path,
    private_key: pathlib.Path,
    public_key: pathlib.Path,
    *,
    result_output: Optional[pathlib.Path] = None,
    after_private_open: Hook = None,
    after_public_open: Hook = None,
) -> CreatedAuthority:
    parent = _assert_no_alias_components(
        stage_parent, "release signing authority stage parent"
    )
    _check_directory(
        os.lstat(parent), "release signing authority stage parent", private=False
    )
    invocation = release_invocation.verify_invocation(invocation_stage)
    private_raw = _read_source_file(
        private_key, "release private key", private=True, after_open=after_private_open
    )
    public_raw = _read_source_file(
        public_key, "release public key", private=False, after_open=after_public_open
    )
    descriptor = _descriptor(invocation, private_raw, public_raw)
    stage = parent / _expected_stage_name(descriptor)
    capability_directory = _ensure_capability_directory(parent)
    capability = capability_path(stage)
    _assert_no_alias_components(
        stage, "release signing authority stage", allow_missing_leaf=True
    )
    _assert_no_alias_components(
        capability, "release signing authority capability", allow_missing_leaf=True
    )
    expected = {
        DESCRIPTOR_NAME: canonical_bytes(descriptor),
        PRIVATE_KEY_NAME: private_raw,
        PUBLIC_KEY_NAME: public_raw,
    }
    stage_created = False
    capability_ready = False
    stage_metadata: Optional[os.stat_result] = None
    try:
        try:
            os.mkdir(stage, 0o700)
            stage_created = True
        except FileExistsError:
            pass
        _set_private_directory(stage)
        stage_metadata = os.lstat(stage)
        _check_directory(
            stage_metadata, "release signing authority stage", private=True
        )
        # The directory identity embedded in the capability must itself be
        # durable before that capability can become durable.
        _fsync_directory(parent)
        try:
            os.lstat(capability)
        except FileNotFoundError:
            # A pre-capability stage can only be adopted when every existing
            # expected file already matches the supplied signing inputs.
            _inspect_recoverable_stage(stage, expected, repair=False)
        capability_value = _publish_capability_noreplace(
            capability,
            descriptor,
            stage,
            stage_metadata,
        )
        _capability_matches_descriptor(capability_value, descriptor, stage)
        if capability_value["schema"] == LEGACY_CAPABILITY_SCHEMA:
            capability_value = _upgrade_capability(
                capability,
                capability_value,
                descriptor,
                stage,
                stage_metadata,
            )
        elif (capability_value["stage_dev"], capability_value["stage_ino"]) != (
            stage_metadata.st_dev,
            stage_metadata.st_ino,
        ):
            with os.scandir(stage) as entries:
                empty_stage = next(entries, None) is None
            if not stage_created or not empty_stage:
                fail(
                    "release signing authority capability is bound to a replaced stage"
                )
            # A durable capability can outlive an empty stage direntry after a
            # power loss. Rebind only a newly allocated, still-empty stage;
            # once any descriptor or key exists, identity mismatch is fatal.
            capability_value = _capability_v2(
                descriptor,
                stage,
                stage_metadata,
                token=capability_value["token"],
            )
            _replace_owned(capability, canonical_bytes(capability_value))
        capability_ready = True
        _fsync_directory(capability_directory)
        _fsync_directory(parent)
        created = CreatedAuthority(
            stage,
            stage / DESCRIPTOR_NAME,
            stage / PRIVATE_KEY_NAME,
            stage / PUBLIC_KEY_NAME,
            capability,
            descriptor["authority_id"],
            invocation.descriptor["invocation_id"],
        )
        if result_output is not None:
            _publish_creation_result(
                result_output, canonical_bytes(created.cli_result())
            )
        _inspect_recoverable_stage(stage, expected, repair=True)
        # The descriptor is the durable deletion plan and must exist before a
        # crash can leave either key copy behind.  Destroy retains it until both
        # keys are gone.
        _ensure_stage_file(stage / DESCRIPTOR_NAME, expected[DESCRIPTOR_NAME])
        _fsync_directory(stage)
        for name in (PRIVATE_KEY_NAME, PUBLIC_KEY_NAME):
            _ensure_stage_file(stage / name, expected[name])
            _fsync_directory(stage)
        _fsync_directory(parent)
        verified = verify_authority(stage, invocation.stage)
        verify_capability(stage, capability, verified)
        return created
    except BaseException:
        # Once the capability exists, every partial expected file is bound to
        # the exact stage identity and is intentionally retained for a retry or
        # authenticated destroy.  Before that boundary, only roll back an empty
        # directory allocated by this call.
        if stage_created and not capability_ready and stage_metadata is not None:
            try:
                try:
                    os.lstat(capability)
                except FileNotFoundError:
                    final_capability_exists = False
                else:
                    final_capability_exists = True
                with os.scandir(stage) as entries:
                    empty = next(entries, None) is None
                if empty and not final_capability_exists:
                    _rmdir_verified(
                        stage,
                        stage_metadata,
                        "uncommitted release signing authority stage",
                    )
                    _fsync_directory(parent)
            except (FileNotFoundError, OSError, SigningAuthorityError):
                pass
        raise


def verify_capability(
    stage_value: pathlib.Path,
    capability_value: pathlib.Path,
    verified: Optional[VerifiedAuthority] = None,
) -> Dict[str, Any]:
    stage = _lexical_absolute(stage_value)
    supplied = _lexical_absolute(capability_value)
    expected = capability_path(stage)
    if supplied != expected:
        fail(f"release signing authority capability path must be {expected}")
    record = verified or verify_authority(stage)
    capability = _validate_capability_value(
        _parse_canonical(
            _read_owned(supplied, "release signing authority capability"),
            "release signing authority capability",
        )
    )
    _capability_matches_descriptor(capability, record.descriptor, stage)
    if capability["schema"] == CAPABILITY_SCHEMA:
        stage_metadata = os.lstat(stage)
        if (capability["stage_dev"], capability["stage_ino"]) != (
            stage_metadata.st_dev,
            stage_metadata.st_ino,
        ):
            fail("release signing authority capability is bound to a replaced stage")
    return capability


def destroy_authority(
    stage_value: pathlib.Path, capability_value: pathlib.Path
) -> None:
    stage = _lexical_absolute(stage_value)
    capability = _lexical_absolute(capability_value)
    expected_capability = capability_path(stage)
    if capability != expected_capability:
        fail(f"release signing authority capability path must be {expected_capability}")
    try:
        stage_metadata = os.lstat(stage)
    except FileNotFoundError:
        stage_metadata = None
    try:
        capability_metadata = os.lstat(capability)
    except FileNotFoundError:
        capability_metadata = None
    if stage_metadata is None and capability_metadata is None:
        stage_parent = _assert_no_alias_components(
            stage.parent, "release signing authority stage parent"
        )
        try:
            capability_parent = _assert_no_alias_components(
                capability.parent,
                "release signing authority capability directory",
            )
        except FileNotFoundError:
            capability_parent = None
        if capability_parent is not None:
            _fsync_directory(capability_parent)
        # If the capability directory itself was already retired, flushing its
        # parent proves both that absence and the stage-name absence.
        _fsync_directory(stage_parent)
        return
    if capability_metadata is None:
        fail("release signing authority capability is absent while the stage remains")
    capability = _assert_no_alias_components(
        capability, "release signing authority capability"
    )
    _check_regular(
        capability_metadata,
        "release signing authority capability",
        bounded=MAX_CONTROL_BYTES,
        owned=True,
    )
    capability_record = _validate_capability_value(
        _parse_canonical(
            _read_owned(capability, "release signing authority capability"),
            "release signing authority capability",
        )
    )
    if capability_record["stage_name"] != stage.name:
        fail("release signing authority capability is bound to a different stage")
    if stage_metadata is None:
        _unlink_verified(
            capability,
            capability_metadata,
            "orphaned release signing authority capability",
        )
        _fsync_directory(capability.parent)
        _fsync_directory(stage.parent)
        return

    stage = _assert_no_alias_components(stage, "release signing authority stage")
    _check_directory(stage_metadata, "release signing authority stage", private=True)
    _require_private_windows_path(stage, "release signing authority stage")
    if capability_record["schema"] == CAPABILITY_SCHEMA and (
        capability_record["stage_dev"], capability_record["stage_ino"]
    ) != (stage_metadata.st_dev, stage_metadata.st_ino):
        fail("release signing authority capability is bound to a replaced stage")

    allowed = {DESCRIPTOR_NAME, PRIVATE_KEY_NAME, PUBLIC_KEY_NAME}
    entries: Dict[str, os.stat_result] = {}
    with os.scandir(stage) as scanned:
        for entry in scanned:
            if entry.name not in allowed:
                fail(
                    "release signing authority stage contains an unexpected entry: "
                    f"{entry.name}"
                )
            metadata = os.lstat(stage / entry.name)
            _require_private_windows_path(
                stage / entry.name,
                f"release signing authority cleanup {entry.name}",
            )
            _check_recoverable_file(
                metadata,
                f"release signing authority cleanup {entry.name}",
                bounded=MAX_CONTROL_BYTES,
            )
            entries[entry.name] = metadata

    descriptor: Optional[Dict[str, Any]] = None
    descriptor_metadata = entries.get(DESCRIPTOR_NAME)
    if descriptor_metadata is not None and descriptor_metadata.st_size > 0:
        try:
            descriptor = _validate_descriptor(
                _parse_canonical(
                    _read_owned(
                        stage / DESCRIPTOR_NAME,
                        "release signing authority cleanup descriptor",
                    ),
                    "release signing authority cleanup descriptor",
                )
            )
        except SigningAuthorityError:
            descriptor = None
    key_names = {PRIVATE_KEY_NAME, PUBLIC_KEY_NAME} & set(entries)
    if descriptor is None and key_names:
        fail(
            "release signing authority keys remain without an intact deletion plan"
        )
    if descriptor is not None:
        _capability_matches_descriptor(capability_record, descriptor, stage)
    if capability_record["schema"] == LEGACY_CAPABILITY_SCHEMA:
        if descriptor is None:
            fail("legacy signing authority capability lacks an intact descriptor")
        capability_record = _upgrade_capability(
            capability,
            capability_record,
            descriptor,
            stage,
            stage_metadata,
        )
        capability_metadata = os.lstat(capability)

    for name in (PRIVATE_KEY_NAME, PUBLIC_KEY_NAME):
        metadata = entries.get(name)
        if metadata is not None:
            _unlink_verified(
                stage / name,
                metadata,
                f"release signing authority cleanup {name}",
            )
    _fsync_directory(stage)
    if descriptor_metadata is not None:
        _unlink_verified(
            stage / DESCRIPTOR_NAME,
            descriptor_metadata,
            "release signing authority cleanup descriptor",
        )
        _fsync_directory(stage)
    _rmdir_verified(stage, stage_metadata, "release signing authority stage")
    _fsync_directory(stage.parent)
    _unlink_verified(
        capability,
        capability_metadata,
        "release signing authority capability",
    )
    _fsync_directory(capability.parent)
    _fsync_directory(stage.parent)


QUERY_FIELDS = (
    "authority_id",
    "capability",
    "descriptor",
    "invocation_id",
    "private_key",
    "public_key",
    "stage",
)


def _print_scalar(value: object) -> None:
    if (
        not isinstance(value, str)
        or not value
        or any(ord(character) < 0x20 for character in value)
    ):
        fail("signing authority query result is not a safe scalar")
    print(value)


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    create.add_argument("--stage-parent", required=True)
    create.add_argument("--invocation-stage", required=True)
    create.add_argument("--private-key", required=True)
    create.add_argument("--public-key", required=True)
    create.add_argument("--result-output")
    verify = commands.add_parser("verify")
    verify.add_argument("--invocation-stage")
    verify.add_argument("stage")
    query = commands.add_parser("query-result")
    query.add_argument("--field", required=True, choices=QUERY_FIELDS)
    destroy = commands.add_parser("destroy")
    destroy.add_argument("--capability", required=True)
    destroy.add_argument("stage")
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "create":
            created = create_authority(
                pathlib.Path(args.stage_parent),
                pathlib.Path(args.invocation_stage),
                pathlib.Path(args.private_key),
                pathlib.Path(args.public_key),
                result_output=(
                    pathlib.Path(args.result_output) if args.result_output else None
                ),
            )
            print(canonical_bytes(created.cli_result()).decode("ascii"), end="")
        elif args.command == "verify":
            verified = verify_authority(
                pathlib.Path(args.stage),
                pathlib.Path(args.invocation_stage) if args.invocation_stage else None,
            )
            print(
                "release signing authority verified: "
                f"id={verified.descriptor['authority_id']}"
            )
        elif args.command == "query-result":
            raw = sys.stdin.buffer.read(MAX_CONTROL_BYTES + 1).replace(b"\r\n", b"\n")
            if len(raw) > MAX_CONTROL_BYTES:
                fail("release signing authority creation result is too large")
            result = _require_exact_object(
                _parse_canonical(raw, "release signing authority creation result"),
                "release signing authority creation result",
                (*QUERY_FIELDS, "schema"),
            )
            if result["schema"] != RESULT_SCHEMA:
                fail("unsupported release signing authority result schema")
            _print_scalar(result[args.field])
        else:
            destroy_authority(pathlib.Path(args.stage), pathlib.Path(args.capability))
        return 0
    except (
        SigningAuthorityError,
        release_invocation.InvocationError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        print(f"ERROR: release signing authority: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
