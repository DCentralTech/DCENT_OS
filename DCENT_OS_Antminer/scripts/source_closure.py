#!/usr/bin/env python3
"""Generate and verify a deterministic, partial release source-closure receipt.

The receipt binds declared source/build definitions, retained packaging-input
snapshots, and produced artifact bytes. It deliberately does not claim build
execution, installed-payload equivalence, or kernel/rootfs reproducibility.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, BinaryIO, Dict, Iterable, List, NoReturn, Tuple

SCRIPT_DIRECTORY = pathlib.Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIRECTORY))

import release_capsule_lineage  # noqa: E402
import release_invocation  # noqa: E402
import source_snapshot  # noqa: E402


SCHEMA = "org.dcentral.dcentos.source-closure.v4"
HISTORICAL_SCHEMA = "org.dcentral.dcentos.source-closure.v3"
LEGACY_SCHEMAS = {
    "org.dcentral.dcentos.source-closure.v1": "legacy source-closure v1 receipts lack required out-of-band input binding",
    "org.dcentral.dcentos.source-closure.v2": "legacy source-closure v2 receipts lack retained prebuilt Rust input binding",
}
CANONICAL_BUILDROOT_REPOSITORY = "https://github.com/buildroot/buildroot.git"
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
HEX_COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
CONTAINER_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
BUILD_INPUT_SELECTION_POLICY = "org.dcentral.dcentos.release-build-input-selection.v1"
PREBUILT_RUST_INPUT_CLAIM = (
    "retained-packaging-input-snapshots-not-build-execution-attestation"
)
PREBUILT_RUST_SELECTION_POLICY = (
    "org.dcentral.dcentos.target-required-prebuilt-rust-inputs.v1"
)
PREBUILT_RUST_RECEIPT_SCHEMA_VERSION = 4
HISTORICAL_PREBUILT_RUST_RECEIPT_SCHEMA_VERSION = 3
PREBUILT_RUST_RECEIPT_CLAIM = "declared-release-capsule-and-post-build-snapshot-consistency-not-build-causality-or-reproducibility-proof"
HISTORICAL_PREBUILT_RUST_RECEIPT_CLAIM = (
    "post-build-snapshot-consistency-not-build-causality-or-reproducibility-proof"
)
PREBUILT_BUILD_INPUT_CLAIM = (
    "pre-build-external-input-snapshot-consistency-"
    "not-compiler-consumption-or-build-causality-proof"
)
PREBUILT_BUILD_INPUT_SELECTION_AUTHORITY = (
    "manifest-from-same-git-authenticated-release-capsule-source-snapshot"
)
PREBUILT_BUILD_INPUT_MANIFEST = "DCENT_OS_Antminer/scripts/build_inputs.manifest"
PREBUILT_RUST_INPUTS_BY_TARGET = {
    "s9": ("dcentos-init", "dcentrald"),
    "am2-s19jpro": ("dcentos-init", "dcentrald"),
    "am2-s19jpro-sd": ("dcentos-init", "dcentrald"),
    "am2-s19pro": ("dcentos-init", "dcentrald"),
}
PREBUILT_RUST_VARIANT_BY_TARGET = {
    target: "zynq" for target in PREBUILT_RUST_INPUTS_BY_TARGET
}
COMMON_CARGO_BUILD_INPUTS = ()
TARGET_BUILD_INPUTS = {
    "cargo-workspace": COMMON_CARGO_BUILD_INPUTS,
    "s9": (
        "",
        "",
    ),
    "am2-s19jpro": (
        "",
        "",
    ),
    # Both aliases use the same S19j extraction staging path and the same
    # am2-s19jpro post-image consumer as the canonical Zynq lane.
    "am2-s19jpro-sd": (
        "",
        "",
    ),
    "am2-s19pro": (
        "",
        "",
    ),
}
# Canonical-manifest entries that are deliberately not direct inputs to the
# currently supported release consumers. Keep these classifications disjoint
# and exhaustively checked by test_build_input_preflight.sh so new manifest
# bytes cannot become silently orphaned or falsely described as consumed.
REFERENCE_ONLY_BUILD_INPUTS = ()
SEPARATELY_VERIFIED_BUILD_INPUTS = (
    "DCENT_OS_Antminer/buildroot/dl/toolchain-external-custom/gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz",
)
BLOCKED_BUILD_INPUT_TARGETS = {
    "am2-s17pro": (
        "am2-s17pro has no pinned S17 kernel input; refusing all packaging, including lab builds, "
        "until every optional extraction fallback is replaced by an exact manifest input"
    ),
    "am3-s19kpro": (
        "am3-s19kpro consumes an unpinned s19k/s21 Amlogic kernel fallback; "
        "refusing all packaging, including lab builds, until the selected kernel is pinned"
    ),
    "am3-s21": (
        "am3-s21 consumes an unpinned S21 Amlogic kernel; refusing all packaging, "
        "including lab builds, until that kernel is pinned"
    ),
    "am3-s19jpro-aml": (
        "am3-s19jpro-aml consumes an unpinned s19j-aml/s21 kernel fallback; "
        "refusing all packaging, including lab builds, until the selected kernel is pinned"
    ),
    "am3-t21": (
        "am3-t21 consumes an unpinned t21/s21 kernel fallback; refusing all "
        "packaging, including lab builds, until the selected kernel is pinned"
    ),
    "am3-bb": (
        "am3-bb boot inputs remain operator-supplied and unpinned; refusing all "
        "packaging, including lab builds, until the complete SD boot input set is pinned"
    ),
    "am3-bb-s19jpro": (
        "am3-bb-s19jpro boot inputs remain operator-supplied and unpinned; refusing all "
        "packaging, including lab builds, until the complete SD boot input set is pinned"
    ),
    "am3-bb-s19jpro-vnish": (
        "am3-bb-s19jpro-vnish consumes unpinned boot.bin, uImage, DTB, and "
        "initramfs inputs from output directories; refusing all packaging, including lab builds"
    ),
    "cv1835-s19jpro": (
        "cv1835-s19jpro has no pinned kernel input or kernel producer; "
        "refusing all packaging, including lab builds, before Cargo/Docker instead of consuming "
        "a stale Buildroot uImage"
    ),
}

BUILD_TARGET_POLICIES = {
    "s9": {
        "arch": "armv7-unknown-linux-musleabihf",
        "configs": (
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment",
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig",
        ),
    },
    "am2-s19jpro": {
        "arch": "armv7-unknown-linux-musleabihf",
        "configs": (
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment",
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig",
        ),
    },
    "am2-s19jpro-sd": {
        "arch": "armv7-unknown-linux-musleabihf",
        "configs": (
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment",
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig",
        ),
    },
    "am2-s19pro": {
        "arch": "armv7-unknown-linux-musleabihf",
        "configs": (
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment",
            "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19pro_defconfig",
        ),
    },
}

BUILD_DRIVER_TARGETS = frozenset(BUILD_TARGET_POLICIES) | frozenset(
    BLOCKED_BUILD_INPUT_TARGETS
)


class ClosureError(ValueError):
    """A fail-closed source-closure validation error."""


def fail(message: str) -> NoReturn:
    raise ClosureError(message)


def is_windows_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse)


def assert_no_link_or_reparse_components(
    path_text: str | os.PathLike[str], label: str
) -> pathlib.Path:
    path = pathlib.Path(path_text).absolute()
    current = pathlib.Path(path.anchor)
    try:
        for part in path.parts[1:]:
            current /= part
            metadata = os.lstat(current)
            if stat.S_ISLNK(metadata.st_mode) or is_windows_reparse(metadata):
                fail(
                    f"{label} path must not contain symlinks or reparse points: "
                    f"{current}"
                )
    except OSError as error:
        fail(f"{label} cannot be inspected: {path}: {error}")
    return path


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def ensure_inside(root: pathlib.Path, path: pathlib.Path, label: str) -> pathlib.Path:
    root = root.resolve(strict=True)
    resolved = path.resolve(strict=True)
    try:
        resolved.relative_to(root)
    except ValueError:
        fail(f"{label} must be inside repository root: {path}")

    cursor = path.absolute()
    while cursor != root:
        metadata = os.lstat(cursor)
        if stat.S_ISLNK(metadata.st_mode) or is_windows_reparse(metadata):
            fail(
                f"{label} must not contain symlink or reparse-point components: {path}"
            )
        if cursor.parent == cursor:
            fail(f"{label} cannot be proven inside repository root: {path}")
        cursor = cursor.parent
    return resolved


def source_file(root: pathlib.Path, path_text: str, label: str) -> Dict[str, Any]:
    path = ensure_inside(root, pathlib.Path(path_text), label)
    if not path.is_file():
        fail(f"{label} must be a regular file: {path}")
    return {
        "path": path.relative_to(root).as_posix(),
        "sha256": sha256_file(path),
        "size": path.stat().st_size,
    }


def parse_build_input_manifest(
    root: pathlib.Path, manifest_text: str
) -> Tuple[Dict[str, Any], Dict[str, str]]:
    manifest_path = ensure_inside(
        root, pathlib.Path(manifest_text), "build-input manifest"
    )
    if not manifest_path.is_file():
        fail(f"build-input manifest must be a regular file: {manifest_path}")
    try:
        raw = manifest_path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        fail("build-input manifest must contain valid UTF-8 text")

    entries: Dict[str, str] = {}
    for line_number, raw_line in enumerate(raw.splitlines(), 1):
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        if not raw_line.isascii():
            fail(f"build-input manifest data line {line_number} must be ASCII")
        match = re.fullmatch(r"([0-9a-f]{64})[ \t]+(.+)", raw_line)
        if match is None:
            fail(f"malformed build-input manifest line {line_number}")
        digest, relative = match.groups()
        if relative != relative.strip() or "\\" in relative:
            fail(f"non-canonical build-input path at line {line_number}")
        pure = pathlib.PurePosixPath(relative)
        if (
            pure.is_absolute()
            or not pure.parts
            or any(part in ("", ".", "..") for part in pure.parts)
        ):
            fail(f"unsafe build-input path at line {line_number}: {relative}")
        if relative in entries:
            fail(
                f"duplicate build-input manifest path at line {line_number}: {relative}"
            )
        entries[relative] = digest
    if not entries:
        fail("build-input manifest contains no entries")
    return source_file(root, str(manifest_path), "build-input manifest"), entries


def build_input_evidence(
    root: pathlib.Path, manifest_text: str, target: str
) -> Dict[str, Any]:
    target = validate_identity(target, "target")
    blocked = BLOCKED_BUILD_INPUT_TARGETS.get(target)
    if blocked is not None:
        fail(blocked)

    manifest, declared = parse_build_input_manifest(root, manifest_text)
    selected = TARGET_BUILD_INPUTS.get(target)
    if selected is None:
        fail(f"target {target} has no explicit release build-input policy")
    policy = BUILD_INPUT_SELECTION_POLICY
    paths = selected

    files = []
    for relative in sorted(paths, key=lambda value: value.encode("utf-8")):
        expected = declared.get(relative)
        if expected is None:
            fail(
                f"target {target} requires an input absent from the manifest: {relative}"
            )
        evidence = source_file(root, str(root / relative), "out-of-band build input")
        if evidence["sha256"] != expected:
            fail(
                f"out-of-band build input SHA256 mismatch for {relative}: "
                f"expected {expected}, actual {evidence['sha256']}"
            )
        files.append(evidence)
    return {
        "manifest": manifest,
        "selection_policy": policy,
        "files": files,
        "snapshot": {
            "snapshot_id": "not-recorded-live-tree-validation",
            "target": target,
            "claim": "live-tree-validation-not-consumer-snapshot",
        },
    }


def build_input_snapshot_evidence(
    snapshot_text: str,
    target: str,
    source_root: pathlib.Path | None = None,
    require_split_authority: bool = False,
) -> Dict[str, Any]:
    """Validate and project a consumed-byte snapshot into receipt evidence."""
    helper_path = pathlib.Path(__file__).with_name("build_input_snapshot.py")
    spec = importlib.util.spec_from_file_location(
        "dcentos_build_input_snapshot_for_closure", helper_path
    )
    if spec is None or spec.loader is None:
        fail("cannot load build-input snapshot validator")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    try:
        descriptor = helper.verify_snapshot(pathlib.Path(snapshot_text), target)
        if require_split_authority:
            if source_root is None:
                fail(
                    "schema-v4 build-input snapshot binding requires a source snapshot tree"
                )
            if descriptor.get("schema") != helper.SPLIT_AUTHORITY_SCHEMA:
                fail("schema-v4 source closure requires build-input snapshot schema v2")
            if descriptor.get("selection_root") != {"kind": helper.SELECTION_ROOT_KIND}:
                fail(
                    "schema-v4 source closure requires a separate verified "
                    "build-input selection root"
                )
            manifest = descriptor.get("manifest")
            if not isinstance(manifest, dict) or not isinstance(
                manifest.get("path"), str
            ):
                fail("schema-v4 build-input snapshot manifest evidence is invalid")
            expected_manifest = source_file(
                source_root,
                str(source_root / manifest["path"]),
                "Git-authenticated build-input selection manifest",
            )
            observed_manifest = {
                key: manifest.get(key) for key in ("path", "sha256", "size")
            }
            if observed_manifest != expected_manifest:
                fail(
                    "build-input snapshot selection manifest disagrees with the "
                    "Git-authenticated capsule source tree"
                )
        return helper.snapshot_evidence(descriptor)
    except (helper.SnapshotError, OSError, KeyError, TypeError, ValueError) as error:
        fail(f"build-input snapshot is invalid: {error}")


def build_input_audit_projection_evidence(
    descriptor_text: str,
    target: str,
    source_root: pathlib.Path,
) -> Dict[str, Any]:
    """Validate a retained path/hash projection, never live input bytes."""

    helper_path = pathlib.Path(__file__).with_name("build_input_snapshot.py")
    spec = importlib.util.spec_from_file_location(
        "dcentos_build_input_snapshot_audit_projection", helper_path
    )
    if spec is None or spec.loader is None:
        fail("cannot load retained build-input descriptor validator")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    try:
        descriptor = helper.verify_audit_descriptor(
            pathlib.Path(descriptor_text), target
        )
        if descriptor.get("schema") != helper.SPLIT_AUTHORITY_SCHEMA:
            fail("portable schema-v4 audit requires build-input descriptor schema v2")
        if descriptor.get("selection_root") != {"kind": helper.SELECTION_ROOT_KIND}:
            fail("portable build-input descriptor has invalid selection authority")
        manifest = descriptor["manifest"]
        expected_manifest = source_file(
            source_root,
            str(source_root / manifest["path"]),
            "Git-authenticated build-input selection manifest",
        )
        observed_manifest = {
            key: manifest.get(key) for key in ("path", "sha256", "size")
        }
        if observed_manifest != expected_manifest:
            fail(
                "retained build-input projection selection manifest disagrees "
                "with the Git-authenticated source"
            )
        return helper.snapshot_evidence(descriptor)
    except (helper.SnapshotError, OSError, KeyError, TypeError, ValueError) as error:
        fail(f"retained build-input projection is invalid: {error}")


def enforce_target_build_policy(
    target: str, arch: str, config_paths: Iterable[str]
) -> None:
    target = validate_identity(target, "target")
    blocked = BLOCKED_BUILD_INPUT_TARGETS.get(target)
    if blocked is not None:
        fail(blocked)
    policy = BUILD_TARGET_POLICIES.get(target)
    if policy is None:
        fail(f"target {target} has no explicit release target policy")
    if arch != policy["arch"]:
        fail(f"target {target} requires architecture {policy['arch']}, got {arch}")
    actual_configs = tuple(config_paths)
    if actual_configs != policy["configs"]:
        fail(
            f"target {target} requires exact Buildroot config merge order "
            f"{policy['configs']}, got {actual_configs}"
        )


def tree_digest(root: pathlib.Path, tree_text: str) -> Dict[str, Any]:
    tree = ensure_inside(root, pathlib.Path(tree_text), "external tree")
    if not tree.is_dir():
        fail(f"external tree must be a directory: {tree}")

    entries: List[Tuple[str, str, int]] = []
    for directory, dirnames, filenames in os.walk(tree, followlinks=False):
        directory_path = pathlib.Path(directory)
        for name in sorted(dirnames):
            candidate = directory_path / name
            metadata = os.lstat(candidate)
            if stat.S_ISLNK(metadata.st_mode) or is_windows_reparse(metadata):
                fail(f"external tree contains a symlink or reparse point: {candidate}")
        for name in sorted(filenames):
            candidate = directory_path / name
            metadata = os.lstat(candidate)
            if (
                stat.S_ISLNK(metadata.st_mode)
                or is_windows_reparse(metadata)
                or not stat.S_ISREG(metadata.st_mode)
            ):
                fail(f"external tree contains a non-regular file: {candidate}")
            relative = candidate.relative_to(tree).as_posix()
            entries.append((relative, sha256_file(candidate), candidate.stat().st_size))

    entries.sort(key=lambda entry: entry[0].encode("utf-8"))
    digest = hashlib.sha256()
    for relative, file_hash, size in entries:
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(file_hash.encode("ascii"))
        digest.update(b"\0")
        digest.update(str(size).encode("ascii"))
        digest.update(b"\n")
    return {
        "path": tree.relative_to(root).as_posix(),
        "digest_algorithm": "sha256-path-content-size-v1",
        "filesystem_mode_scope": "not_bound; Git clean-tree mode and builder normalization are separate evidence",
        "sha256": digest.hexdigest(),
        "file_count": len(entries),
    }


def require_locked_cargo_builder(path: pathlib.Path) -> None:
    commands = []
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8").splitlines(), 1
    ):
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        if re.search(r"(?:^|[;&|()\s])cargo\s+build(?:\s|$)", stripped):
            commands.append((line_number, stripped))
    if not commands:
        fail("Cargo build definition contains no cargo build command")
    unlocked = [
        number for number, command in commands if "--locked" not in command.split()
    ]
    if unlocked:
        fail(
            f"Cargo build definition has mutable dependency resolution at line(s): {unlocked}"
        )


def canonical_bytes(value: Dict[str, Any]) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def iso_utc(epoch: int) -> str:
    try:
        return dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        )
    except (OverflowError, OSError, ValueError) as error:
        fail(f"SOURCE_DATE_EPOCH cannot be represented: {error}")


def safe_tar_members(
    path: pathlib.Path, stream: BinaryIO | None = None
) -> List[Dict[str, Any]]:
    members: List[Dict[str, Any]] = []
    names = set()
    try:
        archive = (
            tarfile.open(fileobj=stream, mode="r:*")
            if stream is not None
            else tarfile.open(path, "r:*")
        )
    except tarfile.TarError:
        return members
    with archive:
        for member in archive:
            normalized = pathlib.PurePosixPath(member.name)
            if normalized.is_absolute() or ".." in normalized.parts or not member.name:
                fail(f"artifact archive contains unsafe member path: {member.name!r}")
            if member.name in names:
                fail(f"artifact archive contains duplicate member: {member.name}")
            names.add(member.name)
            if member.isdir():
                continue
            if not member.isfile():
                fail(
                    f"artifact archive contains unsupported member type: {member.name}"
                )
            validate_archive_member_path(member.name, "artifact archive member path")
            stream = archive.extractfile(member)
            if stream is None:
                fail(f"artifact archive member is unreadable: {member.name}")
            digest = hashlib.sha256()
            size = 0
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
                size += len(chunk)
            if size != member.size:
                fail(
                    f"artifact archive member size changed while reading: {member.name}"
                )
            members.append(
                {"path": member.name, "sha256": digest.hexdigest(), "size": size}
            )
    members.sort(key=lambda item: item["path"].encode("utf-8"))
    return members


def artifact_entry(path_text: str) -> Dict[str, Any]:
    path = assert_no_link_or_reparse_components(path_text, "artifact")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        fail(f"artifact must be an openable non-reparse regular file: {path}: {error}")
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or is_windows_reparse(before):
            fail(f"artifact must be a non-reparse regular file: {path}")
        if getattr(before, "st_nlink", 1) != 1:
            fail(f"artifact must have exactly one hard link: {path}")
        digest = hashlib.sha256()
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
            stream.seek(0)
            archive_members = safe_tar_members(path, stream)
        after = os.fstat(descriptor)
        path_after = os.lstat(path)
    finally:
        os.close(descriptor)

    stable_fields = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    if (
        any(getattr(before, field) != getattr(after, field) for field in stable_fields)
        or not stat.S_ISREG(path_after.st_mode)
        or is_windows_reparse(path_after)
        or getattr(path_after, "st_nlink", 1) != 1
        or (
            after.st_ino
            and (after.st_dev, after.st_ino) != (path_after.st_dev, path_after.st_ino)
        )
        or (
            not after.st_ino
            and any(
                getattr(after, field) != getattr(path_after, field)
                for field in stable_fields
            )
        )
    ):
        fail(f"artifact changed or was replaced while being inspected: {path}")
    return {
        "path": path.name,
        "sha256": digest.hexdigest(),
        "size": after.st_size,
        "archive_regular_members": archive_members,
    }


def git_output(root: pathlib.Path, *arguments: str) -> str:
    process = subprocess.run(
        ["git", "-C", str(root), *arguments],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if process.returncode != 0:
        fail(f"Git inspection failed: {' '.join(arguments)}: {process.stderr.strip()}")
    return process.stdout.strip()


def git_tree_state(root: pathlib.Path) -> str:
    return (
        "dirty"
        if git_output(root, "status", "--porcelain", "--untracked-files=normal")
        else "clean"
    )


def validate_identity(value: str, label: str) -> str:
    if not value or re.search(r"[^A-Za-z0-9._+:/@-]", value):
        fail(f"{label} is missing or contains non-canonical characters")
    return value


def validate_flat_basename(value: str, label: str) -> str:
    if (
        not value
        or pathlib.PurePath(value).name != value
        or value in (".", "..")
        or any(character in value for character in ("/", "\\", "\x00", "\r", "\n"))
    ):
        fail(f"{label} must be a canonical flat basename")
    return value


def validate_archive_member_path(value: str, label: str) -> str:
    if (
        not value
        or any(character in value for character in ("\\", "\x00", "\r", "\n"))
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)
    ):
        fail(f"{label} must be a canonical relative POSIX path")
    normalized = pathlib.PurePosixPath(value)
    if (
        normalized.is_absolute()
        or value != normalized.as_posix()
        or any(part in ("", ".", "..") for part in normalized.parts)
    ):
        fail(f"{label} must be a canonical relative POSIX path")
    return value


RECEIPT_AUTH_UNSIGNED = "not_independently_signed"
RECEIPT_AUTH_DETACHED_REQUIRED = "detached_ed25519_required_for_release"


def closure_scope(receipt_authentication: str, schema: str = SCHEMA) -> Dict[str, Any]:
    unresolved = [
        "Buildroot package download archives are not directly enumerated by this receipt; release builds bind a separate legal-info inventory whose completeness is not established",
        "container base-image package repository snapshot is not reconstructibly pinned",
        "kernel and rootfs byte-for-byte reproducibility has not been demonstrated",
        "offline receipt verification cannot independently re-inspect the ephemeral Buildroot checkout",
    ]
    if schema == SCHEMA:
        unresolved.extend(
            [
                "Git blob executable modes are bound by the source snapshot, but derived-stage permission normalization and other source-to-build transforms are not proven",
                "out-of-band inputs are copied from verified open handles, but same-UID stage mutation and consumer-to-artifact causality are not attested",
                "retained prebuilt Rust bytes and their snapshot-consistency receipts do not prove compiler causality, build execution, or installed-payload equivalence",
                "release-capsule identifier agreement is not build-execution attestation or proof that a compiler consumed the identified snapshot",
                "release-capsule lineage does not establish compiler causality, installed-payload equivalence, or reproducibility",
            ]
        )
        binds = (
            "one declared release invocation identity, one exact Git-object source "
            "snapshot identity, retained packaging input snapshots, and produced "
            "artifact bytes"
        )
    elif schema == HISTORICAL_SCHEMA:
        unresolved.extend(
            [
                "host filesystem permission bits are not hashed; release builds separately require a clean Git tree and normalize staged hook modes",
                "out-of-band inputs are copied from verified open handles, but same-UID stage mutation and consumer-to-artifact causality are not attested",
                "retained prebuilt Rust bytes and their snapshot-consistency receipts do not prove compiler causality, build execution, or installed-payload equivalence",
            ]
        )
        binds = (
            "declared source/build definitions, retained packaging input "
            "snapshots, and produced artifact bytes"
        )
    else:
        fail("unsupported source-closure schema for claim scope")
    if receipt_authentication == RECEIPT_AUTH_UNSIGNED:
        unresolved.append(
            "receipt authenticity depends on the authenticated release channel"
        )
    elif receipt_authentication == RECEIPT_AUTH_DETACHED_REQUIRED:
        unresolved.append(
            "the required detached Ed25519 signature authenticates receipt bytes but does not attest build execution or source completeness"
        )
    else:
        fail("unsupported receipt authentication policy")

    return {
        "level": "partial",
        "binds": binds,
        "payload_byte_reproducibility": "not_evaluated",
        "build_execution_attestation": "not_attested",
        "receipt_authentication": receipt_authentication,
        "unresolved": unresolved,
    }


def require_exact_object(value: Any, label: str, keys: Iterable[str]) -> Dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    expected = set(keys)
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        fail(f"{label} has invalid keys (missing={missing}, extra={extra})")
    return value


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str):
        fail(f"{label} must be a string")
    return value


def require_integer(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        fail(f"{label} must be an integer")
    return value


def require_array(value: Any, label: str) -> List[Any]:
    if not isinstance(value, list):
        fail(f"{label} must be an array")
    return value


def validate_file_evidence(value: Any, label: str) -> None:
    item = require_exact_object(value, label, ("path", "sha256", "size"))
    require_string(item["path"], f"{label}.path")
    if not HEX_64.fullmatch(require_string(item["sha256"], f"{label}.sha256")):
        fail(f"{label}.sha256 must be lowercase SHA-256")
    if require_integer(item["size"], f"{label}.size") < 0:
        fail(f"{label}.size must be non-negative")


def validate_receipt_schema(manifest: Any) -> Dict[str, Any]:
    if not isinstance(manifest, dict):
        fail("source-closure must be an object")
    schema = manifest.get("schema")
    if schema not in (SCHEMA, HISTORICAL_SCHEMA):
        fail("unsupported source-closure schema")
    top_keys = [
        "schema",
        "created_at_utc",
        "source_date_epoch",
        "scope",
        "source",
        "build",
        "prebuilt_rust_inputs",
        "artifacts",
    ]
    if schema == SCHEMA:
        top_keys.append("release_capsule")
    top = require_exact_object(
        manifest,
        "source-closure",
        top_keys,
    )
    require_string(top["schema"], "source-closure.schema")
    require_string(top["created_at_utc"], "source-closure.created_at_utc")
    require_integer(top["source_date_epoch"], "source-closure.source_date_epoch")

    scope = require_exact_object(
        top["scope"],
        "source-closure.scope",
        (
            "level",
            "binds",
            "payload_byte_reproducibility",
            "build_execution_attestation",
            "receipt_authentication",
            "unresolved",
        ),
    )
    for key in (
        "level",
        "binds",
        "payload_byte_reproducibility",
        "build_execution_attestation",
        "receipt_authentication",
    ):
        require_string(scope[key], f"source-closure.scope.{key}")
    for index, item in enumerate(
        require_array(scope["unresolved"], "source-closure.scope.unresolved")
    ):
        require_string(item, f"source-closure.scope.unresolved[{index}]")

    source = require_exact_object(
        top["source"], "source-closure.source", ("commit", "tree_state")
    )
    require_string(source["commit"], "source-closure.source.commit")
    require_string(source["tree_state"], "source-closure.source.tree_state")
    if schema == SCHEMA:
        try:
            release_capsule_lineage.validate_release_capsule(top["release_capsule"])
        except release_capsule_lineage.CapsuleLineageError as error:
            fail(f"source-closure release capsule is invalid: {error}")

    build = require_exact_object(
        top["build"],
        "source-closure.build",
        (
            "target",
            "arch",
            "cargo",
            "buildroot",
            "toolchain",
            "container",
            "out_of_band_inputs",
        ),
    )
    require_string(build["target"], "source-closure.build.target")
    require_string(build["arch"], "source-closure.build.arch")

    cargo = require_exact_object(
        build["cargo"],
        "source-closure.build.cargo",
        ("lockfile", "builder_definition", "dependency_resolution"),
    )
    validate_file_evidence(cargo["lockfile"], "source-closure.build.cargo.lockfile")
    validate_file_evidence(
        cargo["builder_definition"], "source-closure.build.cargo.builder_definition"
    )
    require_string(
        cargo["dependency_resolution"],
        "source-closure.build.cargo.dependency_resolution",
    )

    buildroot = require_exact_object(
        build["buildroot"],
        "source-closure.build.buildroot",
        (
            "repository",
            "commit",
            "checkout_policy",
            "checkout_verification",
            "configs",
            "config_merge_order",
            "external_tree",
        ),
    )
    for key in ("repository", "commit", "checkout_policy", "checkout_verification"):
        require_string(buildroot[key], f"source-closure.build.buildroot.{key}")
    configs = require_array(
        buildroot["configs"], "source-closure.build.buildroot.configs"
    )
    for index, item in enumerate(configs):
        validate_file_evidence(item, f"source-closure.build.buildroot.configs[{index}]")
    merge_order = require_array(
        buildroot["config_merge_order"],
        "source-closure.build.buildroot.config_merge_order",
    )
    for index, item in enumerate(merge_order):
        require_string(
            item, f"source-closure.build.buildroot.config_merge_order[{index}]"
        )
    external_tree = require_exact_object(
        buildroot["external_tree"],
        "source-closure.build.buildroot.external_tree",
        ("path", "digest_algorithm", "filesystem_mode_scope", "sha256", "file_count"),
    )
    for key in ("path", "digest_algorithm", "filesystem_mode_scope", "sha256"):
        require_string(
            external_tree[key], f"source-closure.build.buildroot.external_tree.{key}"
        )
    if (
        require_integer(
            external_tree["file_count"],
            "source-closure.build.buildroot.external_tree.file_count",
        )
        < 0
    ):
        fail(
            "source-closure.build.buildroot.external_tree.file_count must be non-negative"
        )

    toolchain = require_exact_object(
        build["toolchain"],
        "source-closure.build.toolchain",
        ("id", "archive_sha256", "verification"),
    )
    for key in ("id", "archive_sha256", "verification"):
        require_string(toolchain[key], f"source-closure.build.toolchain.{key}")
    container = require_exact_object(
        build["container"],
        "source-closure.build.container",
        ("image_id", "definition"),
    )
    require_string(container["image_id"], "source-closure.build.container.image_id")
    validate_file_evidence(
        container["definition"], "source-closure.build.container.definition"
    )

    inputs = require_exact_object(
        build["out_of_band_inputs"],
        "source-closure.build.out_of_band_inputs",
        ("manifest", "selection_policy", "files", "snapshot"),
    )
    validate_file_evidence(
        inputs["manifest"], "source-closure.build.out_of_band_inputs.manifest"
    )
    require_string(
        inputs["selection_policy"],
        "source-closure.build.out_of_band_inputs.selection_policy",
    )
    for index, item in enumerate(
        require_array(inputs["files"], "source-closure.build.out_of_band_inputs.files")
    ):
        validate_file_evidence(
            item, f"source-closure.build.out_of_band_inputs.files[{index}]"
        )
    snapshot = require_exact_object(
        inputs["snapshot"],
        "source-closure.build.out_of_band_inputs.snapshot",
        ("snapshot_id", "target", "claim"),
    )
    for key in ("snapshot_id", "target", "claim"):
        require_string(
            snapshot[key], f"source-closure.build.out_of_band_inputs.snapshot.{key}"
        )
    recorded = HEX_64.fullmatch(snapshot["snapshot_id"]) is not None
    if recorded:
        if (
            snapshot["claim"]
            != "selected_manifest_pinned_bytes_copied_from_open_regular_file_handles"
        ):
            fail("source-closure recorded build-input snapshot claim is invalid")
    elif snapshot != {
        "snapshot_id": "not-recorded-live-tree-validation",
        "target": build["target"],
        "claim": "live-tree-validation-not-consumer-snapshot",
    }:
        fail("source-closure live build-input validation marker is invalid")
    if snapshot["target"] != build["target"]:
        fail("source-closure build-input snapshot target is invalid")

    artifacts = require_array(top["artifacts"], "source-closure.artifacts")
    if not artifacts:
        fail("source-closure.artifacts must be a non-empty array")
    artifact_paths = set()
    for index, artifact in enumerate(artifacts):
        artifact_object = require_exact_object(
            artifact,
            f"source-closure.artifacts[{index}]",
            ("path", "sha256", "size", "archive_regular_members"),
        )
        artifact_path = validate_flat_basename(
            require_string(
                artifact_object["path"], f"source-closure.artifacts[{index}].path"
            ),
            f"source-closure.artifacts[{index}].path",
        )
        if artifact_path in artifact_paths:
            fail("source-closure artifact paths must be unique")
        artifact_paths.add(artifact_path)
        if not HEX_64.fullmatch(
            require_string(
                artifact_object["sha256"], f"source-closure.artifacts[{index}].sha256"
            )
        ):
            fail(f"source-closure.artifacts[{index}].sha256 must be lowercase SHA-256")
        if (
            require_integer(
                artifact_object["size"], f"source-closure.artifacts[{index}].size"
            )
            < 0
        ):
            fail(f"source-closure.artifacts[{index}].size must be non-negative")
        member_paths = set()
        members = require_array(
            artifact_object["archive_regular_members"],
            f"source-closure.artifacts[{index}].archive_regular_members",
        )
        for member_index, member in enumerate(members):
            member_label = (
                f"source-closure.artifacts[{index}]."
                f"archive_regular_members[{member_index}]"
            )
            validate_file_evidence(
                member,
                member_label,
            )
            member_path = validate_archive_member_path(
                member["path"], f"{member_label}.path"
            )
            if member_path in member_paths:
                fail(
                    f"source-closure.artifacts[{index}] archive member paths "
                    "must be unique"
                )
            member_paths.add(member_path)
    validate_prebuilt_rust_inputs_schema(
        top["prebuilt_rust_inputs"],
        build["target"],
        build["arch"],
        (artifact["path"] for artifact in top["artifacts"]),
        PREBUILT_RUST_RECEIPT_SCHEMA_VERSION
        if schema == SCHEMA
        else HISTORICAL_PREBUILT_RUST_RECEIPT_SCHEMA_VERSION,
    )
    return top


def read_regular_nonsymlink(path_text: str, label: str) -> Tuple[pathlib.Path, bytes]:
    path = assert_no_link_or_reparse_components(path_text, label)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        fail(f"{label} must be an openable non-symlink regular file: {path}: {error}")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or is_windows_reparse(metadata):
            fail(f"{label} must be a non-reparse regular file: {path}")
        if getattr(metadata, "st_nlink", 1) != 1:
            fail(f"{label} must have exactly one hard link: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            raw = stream.read()
        after = os.fstat(descriptor)
        path_after = os.lstat(path)
        stable_fields = (
            "st_dev",
            "st_ino",
            "st_mode",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
        )
        if (
            len(raw) != metadata.st_size
            or any(
                getattr(metadata, field) != getattr(after, field)
                for field in stable_fields
            )
            or not stat.S_ISREG(path_after.st_mode)
            or is_windows_reparse(path_after)
            or getattr(path_after, "st_nlink", 1) != 1
            or (
                metadata.st_ino
                and (metadata.st_dev, metadata.st_ino)
                != (path_after.st_dev, path_after.st_ino)
            )
            or (
                not metadata.st_ino
                and any(
                    getattr(after, field) != getattr(path_after, field)
                    for field in stable_fields
                )
            )
        ):
            fail(f"{label} changed while being read: {path}")
    finally:
        os.close(descriptor)
    return path, raw


def _raw_file_evidence(path: pathlib.Path, raw: bytes) -> Dict[str, Any]:
    return {
        "path": path.name,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "size": len(raw),
    }


def _validate_prebuilt_build_inputs(
    value: Any,
    receipt: Dict[str, Any],
    source_root: pathlib.Path,
    path: pathlib.Path,
) -> None:
    record = require_exact_object(
        value,
        f"retained build receipt {path.name}.build_inputs",
        ("claim", "evidence", "selection_authority"),
    )
    if record["claim"] != PREBUILT_BUILD_INPUT_CLAIM:
        fail(f"retained build receipt build-input claim is invalid: {path}")
    if record["selection_authority"] != PREBUILT_BUILD_INPUT_SELECTION_AUTHORITY:
        fail(
            f"retained build receipt build-input selection authority is invalid: {path}"
        )
    evidence = require_exact_object(
        record["evidence"],
        f"retained build receipt {path.name}.build_inputs.evidence",
        ("files", "manifest", "selection_policy", "snapshot"),
    )
    validate_file_evidence(
        evidence["manifest"],
        f"retained build receipt {path.name}.build_inputs.evidence.manifest",
    )
    if evidence["manifest"]["path"] != PREBUILT_BUILD_INPUT_MANIFEST:
        fail(f"retained build receipt names the wrong build-input manifest: {path}")
    expected_manifest = source_file(
        source_root,
        str(source_root / PREBUILT_BUILD_INPUT_MANIFEST),
        "Git-authenticated retained-receipt build-input manifest",
    )
    if evidence["manifest"] != expected_manifest:
        fail(
            "retained build receipt build-input manifest disagrees with the "
            f"Git-authenticated capsule source tree: {path}"
        )
    if evidence["selection_policy"] != BUILD_INPUT_SELECTION_POLICY:
        fail(f"retained build receipt build-input selection policy is invalid: {path}")
    files = require_array(
        evidence["files"],
        f"retained build receipt {path.name}.build_inputs.evidence.files",
    )
    if files:
        fail(
            f"retained build receipt must bind the empty Cargo external-input selection: {path}"
        )
    snapshot = require_exact_object(
        evidence["snapshot"],
        f"retained build receipt {path.name}.build_inputs.evidence.snapshot",
        ("claim", "snapshot_id", "target"),
    )
    if (
        snapshot["claim"]
        != "selected_manifest_pinned_bytes_copied_from_open_regular_file_handles"
        or snapshot["target"] != "cargo-workspace"
        or not HEX_64.fullmatch(str(snapshot["snapshot_id"]))
    ):
        fail(
            f"retained build receipt Cargo build-input snapshot identity is invalid: {path}"
        )
    compile_environment = receipt.get("compile_environment")
    if not isinstance(compile_environment, dict):
        fail(f"retained build receipt compile environment is invalid: {path}")
    entries = compile_environment.get("entries")
    if not isinstance(entries, dict):
        fail(
            f"retained build receipt compile environment is invalid: {path}"
        )
    legacy_entries = sorted(
        key for key in entries if key.startswith("DCENT_STOCK_FPGA_")
    )
    if legacy_entries:
        fail(
            "retained build receipt compile environment still carries removed "
            f"stock FPGA authority {legacy_entries}: {path}"
        )


def _parse_retained_build_receipt(
    raw: bytes,
    path: pathlib.Path,
    binary_name: str,
    binary_raw: bytes,
    receipt_schema_version: int,
    expected_release_capsule: Dict[str, Any] | None,
    expected_source_commit: str | None,
    source_root: pathlib.Path | None,
) -> Dict[str, Any]:
    try:
        receipt = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"retained build receipt is not valid UTF-8 JSON: {path}: {error}")
    receipt_keys = [
        "schema_version",
        "claim",
        "target_triple",
        "profile",
        "build_variant",
        "git",
        "build_environment",
        "builder",
        "toolchain_context",
        "compile_environment",
        "source_inventory_sha256",
        "source_inventory",
        "cargo_metadata",
        "binary",
    ]
    if receipt_schema_version == PREBUILT_RUST_RECEIPT_SCHEMA_VERSION:
        receipt_keys.extend(("release_capsule", "build_inputs"))
    receipt = require_exact_object(
        receipt,
        f"retained build receipt {path.name}",
        receipt_keys,
    )
    if raw != canonical_bytes(receipt):
        fail(f"retained build receipt is not canonical JSON: {path}")
    if receipt["schema_version"] != receipt_schema_version:
        fail(f"retained build receipt has unsupported schema: {path}")
    expected_claim = (
        PREBUILT_RUST_RECEIPT_CLAIM
        if receipt_schema_version == PREBUILT_RUST_RECEIPT_SCHEMA_VERSION
        else HISTORICAL_PREBUILT_RUST_RECEIPT_CLAIM
    )
    if receipt["claim"] != expected_claim:
        fail(f"retained build receipt has an invalid claim: {path}")
    if receipt_schema_version == PREBUILT_RUST_RECEIPT_SCHEMA_VERSION:
        if expected_release_capsule is None:
            fail("schema-v4 retained build receipt requires verified release capsule")
        try:
            observed_capsule = release_capsule_lineage.validate_release_capsule(
                receipt["release_capsule"]
            )
        except release_capsule_lineage.CapsuleLineageError as error:
            fail(f"retained build receipt release capsule is invalid: {path}: {error}")
        if observed_capsule != expected_release_capsule:
            fail(
                f"retained build receipt release capsule disagrees with source closure: {path}"
            )
        git = require_exact_object(
            receipt["git"],
            f"retained build receipt {path.name}.git",
            ("commit", "source_kind"),
        )
        if git["source_kind"] != "exact-git-object-snapshot":
            fail(
                f"retained build receipt does not identify exact snapshot source: {path}"
            )
        if expected_source_commit is None or git["commit"] != expected_source_commit:
            fail(
                f"retained build receipt source commit disagrees with source closure: {path}"
            )
        if source_root is None:
            fail("schema-v4 retained build receipt requires authenticated source tree")
        _validate_prebuilt_build_inputs(
            receipt["build_inputs"], receipt, source_root, path
        )
    elif expected_release_capsule is not None:
        fail("historical retained build receipt cannot carry a release capsule")
    builder = require_exact_object(
        receipt["builder"],
        f"retained build receipt {path.name}.builder",
        ("kind", "base_reference", "image_id", "package_resolution"),
    )
    if (
        require_string(
            builder["kind"], f"retained build receipt {path.name}.builder.kind"
        )
        != "docker-cross"
    ):
        fail(f"retained build receipt does not identify a cross-builder: {path}")
    base_reference = require_string(
        builder["base_reference"],
        f"retained build receipt {path.name}.builder.base_reference",
    )
    if not re.fullmatch(r"(?:[^/@]+/)*[^/@]+@sha256:[0-9a-f]{64}", base_reference):
        fail(f"retained build receipt builder base is mutable: {path}")
    image_id = require_string(
        builder["image_id"], f"retained build receipt {path.name}.builder.image_id"
    )
    if not CONTAINER_ID.fullmatch(image_id):
        fail(f"retained build receipt builder image ID is invalid: {path}")
    require_string(
        builder["package_resolution"],
        f"retained build receipt {path.name}.builder.package_resolution",
    )
    binary = require_exact_object(
        receipt["binary"],
        f"retained build receipt {path.name}.binary",
        ("name", "path", "size", "sha256"),
    )
    if (
        require_string(
            binary["name"], f"retained build receipt {path.name}.binary.name"
        )
        != binary_name
    ):
        fail(f"retained build receipt names the wrong binary: {path}")
    require_string(binary["path"], f"retained build receipt {path.name}.binary.path")
    if require_integer(
        binary["size"], f"retained build receipt {path.name}.binary.size"
    ) != len(binary_raw):
        fail(f"retained build receipt binary size binding is invalid: {path}")
    digest = require_string(
        binary["sha256"], f"retained build receipt {path.name}.binary.sha256"
    )
    if not HEX_64.fullmatch(digest) or digest != hashlib.sha256(binary_raw).hexdigest():
        fail(f"retained build receipt binary digest binding is invalid: {path}")
    return receipt


def _prebuilt_rust_entry(
    name: str,
    binary_text: str,
    receipt_text: str,
    target: str,
    arch: str,
    packaging_artifact: str,
    receipt_schema_version: int,
    release_capsule: Dict[str, Any] | None,
    source_commit: str | None,
    source_root: pathlib.Path | None,
) -> Dict[str, Any]:
    name = validate_identity(name, "prebuilt Rust binary name")
    if "/" in name or ":" in name or name in (".", ".."):
        fail("prebuilt Rust binary name must be a safe basename")
    binary_path, binary_raw = read_regular_nonsymlink(
        binary_text, f"retained prebuilt Rust binary {name}"
    )
    receipt_path, receipt_raw = read_regular_nonsymlink(
        receipt_text, f"retained prebuilt Rust build receipt {name}"
    )
    if binary_path.parent != receipt_path.parent:
        fail(f"retained prebuilt Rust pair must share one flat directory: {name}")
    expected_binary_name = f"{packaging_artifact}.prebuilt-rust.{name}.bin"
    if binary_path.name != expected_binary_name:
        fail(f"retained prebuilt Rust binary is not release-scoped: {binary_path.name}")
    expected_receipt_name = (
        f"{packaging_artifact}.prebuilt-rust.{name}.build-receipt.json"
    )
    if receipt_path.name != expected_receipt_name:
        fail(f"retained prebuilt Rust pair filenames do not match: {name}")
    receipt = _parse_retained_build_receipt(
        receipt_raw,
        receipt_path,
        name,
        binary_raw,
        receipt_schema_version,
        release_capsule,
        source_commit,
        source_root,
    )
    expected_variant = PREBUILT_RUST_VARIANT_BY_TARGET.get(target)
    if expected_variant is None:
        fail(f"target {target} has no retained prebuilt Rust input policy")
    expected_context = {
        "target_triple": arch,
        "profile": "release",
        "build_variant": expected_variant,
    }
    for key, expected in expected_context.items():
        if receipt[key] != expected:
            fail(
                f"retained build receipt {key} mismatch for {name}: "
                f"expected {expected}, found {receipt[key]}"
            )
    if receipt_schema_version == PREBUILT_RUST_RECEIPT_SCHEMA_VERSION:
        expected_source_path = f"target/{arch}/release/{name}"
    else:
        expected_source_path = (
            f"DCENT_OS_Antminer/dcentrald/target/{arch}/release/{name}"
        )
    if receipt["binary"]["path"] != expected_source_path:
        fail(
            f"retained build receipt binary path mismatch for {name}: "
            f"expected {expected_source_path}, found {receipt['binary']['path']}"
        )
    return {
        "name": name,
        "binary": _raw_file_evidence(binary_path, binary_raw),
        "receipt": _raw_file_evidence(receipt_path, receipt_raw),
        "receipt_schema_version": receipt_schema_version,
        "receipt_claim": receipt["claim"],
        **expected_context,
    }


def build_prebuilt_rust_inputs(
    declared: Iterable[Iterable[str]],
    target: str,
    arch: str,
    packaging_artifact: str,
    packaging_directory: pathlib.Path,
    receipt_schema_version: int = PREBUILT_RUST_RECEIPT_SCHEMA_VERSION,
    release_capsule: Dict[str, Any] | None = None,
    source_commit: str | None = None,
    source_root: pathlib.Path | None = None,
) -> Dict[str, Any]:
    validate_flat_basename(
        packaging_artifact, "retained prebuilt Rust packaging artifact"
    )
    expected_names = PREBUILT_RUST_INPUTS_BY_TARGET.get(target)
    if expected_names is None:
        fail(f"target {target} has no retained prebuilt Rust input policy")
    entries: List[Dict[str, Any]] = []
    seen_names = set()
    seen_paths = set()
    for declared_entry in declared:
        name, binary_text, receipt_text = declared_entry
        if name in seen_names:
            fail(f"duplicate retained prebuilt Rust binary name: {name}")
        entry = _prebuilt_rust_entry(
            name,
            binary_text,
            receipt_text,
            target,
            arch,
            packaging_artifact,
            receipt_schema_version,
            release_capsule,
            source_commit,
            source_root,
        )
        if (
            pathlib.Path(binary_text).absolute().parent
            != packaging_directory.absolute()
        ):
            fail(
                "retained prebuilt Rust inputs must be flat siblings of the packaging artifact"
            )
        seen_names.add(name)
        for role in ("binary", "receipt"):
            path = os.path.normcase(
                str(
                    pathlib.Path(
                        binary_text if role == "binary" else receipt_text
                    ).absolute()
                )
            )
            if path in seen_paths:
                fail(f"duplicate retained prebuilt Rust input path: {path}")
            seen_paths.add(path)
        entries.append(entry)
    entries.sort(key=lambda item: item["name"].encode("utf-8"))
    if tuple(item["name"] for item in entries) != tuple(expected_names):
        fail(
            f"retained prebuilt Rust input set for {target} must be exactly "
            f"{', '.join(expected_names)}"
        )
    expected_sidecars = {
        item[role]["path"] for item in entries for role in ("binary", "receipt")
    }
    observed_sidecars = set()
    try:
        with os.scandir(packaging_directory) as directory_entries:
            for directory_entry in directory_entries:
                if not directory_entry.name.startswith(
                    f"{packaging_artifact}.prebuilt-rust."
                ):
                    continue
                metadata = directory_entry.stat(follow_symlinks=False)
                if not stat.S_ISREG(metadata.st_mode):
                    fail(
                        "retained prebuilt Rust sidecar set contains a symlink, "
                        f"directory, or special file: {directory_entry.name}"
                    )
                observed_sidecars.add(directory_entry.name)
    except OSError as error:
        fail(f"cannot enumerate retained prebuilt Rust sidecars: {error}")
    if observed_sidecars != expected_sidecars:
        fail(
            "retained prebuilt Rust sidecar set has missing or extra entries: "
            f"expected={sorted(expected_sidecars)}, observed={sorted(observed_sidecars)}"
        )
    return {
        "claim": PREBUILT_RUST_INPUT_CLAIM,
        "selection_policy": PREBUILT_RUST_SELECTION_POLICY,
        "packaging_artifact": packaging_artifact,
        "build_execution_attestation": "not_attested",
        "installed_payload_equivalence": "not_evaluated",
        "entries": entries,
    }


def validate_prebuilt_rust_inputs_schema(
    value: Any,
    target: str,
    arch: str,
    artifact_paths: Iterable[str],
    receipt_schema_version: int,
) -> None:
    section = require_exact_object(
        value,
        "source-closure.prebuilt_rust_inputs",
        (
            "claim",
            "selection_policy",
            "packaging_artifact",
            "build_execution_attestation",
            "installed_payload_equivalence",
            "entries",
        ),
    )
    expected_markers = {
        "claim": PREBUILT_RUST_INPUT_CLAIM,
        "selection_policy": PREBUILT_RUST_SELECTION_POLICY,
        "build_execution_attestation": "not_attested",
        "installed_payload_equivalence": "not_evaluated",
    }
    for key, expected in expected_markers.items():
        if section[key] != expected:
            fail(f"source-closure.prebuilt_rust_inputs.{key} is invalid")
    packaging_artifact = require_string(
        section["packaging_artifact"],
        "source-closure.prebuilt_rust_inputs.packaging_artifact",
    )
    validate_flat_basename(
        packaging_artifact,
        "source-closure prebuilt Rust packaging artifact",
    )
    generic_artifact_paths = set(artifact_paths)
    if packaging_artifact not in generic_artifact_paths:
        fail(
            "source-closure prebuilt Rust packaging artifact is not bound as an artifact"
        )
    expected_names = PREBUILT_RUST_INPUTS_BY_TARGET.get(target)
    if expected_names is None:
        fail(f"target {target} has no retained prebuilt Rust input policy")
    expected_variant = PREBUILT_RUST_VARIANT_BY_TARGET[target]
    entries = require_array(
        section["entries"], "source-closure.prebuilt_rust_inputs.entries"
    )
    observed_names = []
    observed_paths = []
    for index, raw_entry in enumerate(entries):
        label = f"source-closure.prebuilt_rust_inputs.entries[{index}]"
        entry = require_exact_object(
            raw_entry,
            label,
            (
                "name",
                "binary",
                "receipt",
                "receipt_schema_version",
                "receipt_claim",
                "target_triple",
                "profile",
                "build_variant",
            ),
        )
        name = require_string(entry["name"], f"{label}.name")
        observed_names.append(name)
        for role in ("binary", "receipt"):
            validate_file_evidence(entry[role], f"{label}.{role}")
            path = entry[role]["path"]
            validate_flat_basename(path, f"{label}.{role}.path")
            observed_paths.append(path)
        expected_binary_path = f"{packaging_artifact}.prebuilt-rust.{name}.bin"
        expected_receipt_path = (
            f"{packaging_artifact}.prebuilt-rust.{name}.build-receipt.json"
        )
        if entry["binary"]["path"] != expected_binary_path:
            fail(f"{label}.binary.path is not scoped to the packaging artifact")
        if entry["receipt"]["path"] != expected_receipt_path:
            fail(f"{label}.receipt.path is not scoped to the packaging artifact")
        if entry["receipt_schema_version"] != receipt_schema_version:
            fail(f"{label}.receipt_schema_version is invalid")
        expected = {
            "receipt_claim": PREBUILT_RUST_RECEIPT_CLAIM
            if receipt_schema_version == PREBUILT_RUST_RECEIPT_SCHEMA_VERSION
            else HISTORICAL_PREBUILT_RUST_RECEIPT_CLAIM,
            "target_triple": arch,
            "profile": "release",
            "build_variant": expected_variant,
        }
        for key, expected_value in expected.items():
            if entry[key] != expected_value:
                fail(f"{label}.{key} is invalid")
    if tuple(observed_names) != tuple(expected_names):
        fail(f"source-closure prebuilt Rust input set for {target} is invalid")
    if len(set(observed_names)) != len(observed_names):
        fail("source-closure prebuilt Rust input names contain duplicates")
    if len(set(observed_paths)) != len(observed_paths):
        fail("source-closure prebuilt Rust input paths contain duplicates")
    if generic_artifact_paths.intersection(observed_paths):
        fail("source-closure semantic prebuilt inputs duplicate generic artifacts")


def verify_receipt_authentication(
    args: argparse.Namespace,
    receipt_authentication: str,
    receipt_bytes: bytes,
) -> None:
    signature_text = getattr(args, "signature", None)
    public_key_text = getattr(args, "public_key", None)
    if receipt_authentication == RECEIPT_AUTH_UNSIGNED:
        if signature_text is not None or public_key_text is not None:
            fail(
                "an unsigned/lab source-closure receipt cannot be promoted to release posture by supplying detached authentication arguments"
            )
        return
    if receipt_authentication != RECEIPT_AUTH_DETACHED_REQUIRED:
        fail("unsupported receipt authentication policy")
    if signature_text is None or public_key_text is None:
        fail(
            "release source-closure verification requires both --signature and --public-key"
        )

    _signature_path, signature_bytes = read_regular_nonsymlink(
        signature_text, "source-closure detached signature"
    )
    _public_key_path, public_key_bytes = read_regular_nonsymlink(
        public_key_text, "source-closure trusted public key"
    )
    if len(signature_bytes) != 64:
        fail("source-closure Ed25519 signature must be exactly 64 bytes")
    if not public_key_bytes or len(public_key_bytes) > 64 * 1024:
        fail("source-closure trusted public key has an invalid size")

    # OpenSSL must verify the same receipt bytes parsed above, not reopen a
    # caller-controlled path that can be swapped between parsing and signature
    # verification. Snapshot all three inputs and verify only private copies.
    with tempfile.TemporaryDirectory(
        prefix="dcent-source-closure-verify-"
    ) as directory:
        snapshot_dir = pathlib.Path(directory)
        receipt_snapshot = snapshot_dir / "receipt.json"
        signature_snapshot = snapshot_dir / "receipt.sig"
        public_key_snapshot = snapshot_dir / "trusted-public-key.pem"
        for path, content in (
            (receipt_snapshot, receipt_bytes),
            (signature_snapshot, signature_bytes),
            (public_key_snapshot, public_key_bytes),
        ):
            with path.open("xb") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
        process = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                str(public_key_snapshot),
                "-sigfile",
                str(signature_snapshot),
                "-in",
                str(receipt_snapshot),
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    if process.returncode != 0:
        fail(
            "source-closure detached Ed25519 signature verification failed: "
            f"{process.stderr.strip() or process.stdout.strip()}"
        )


def build_manifest(args: argparse.Namespace) -> Dict[str, Any]:
    git_object_root = pathlib.Path(args.repo_root).resolve(strict=True)
    expected_commit = args.source_commit.lower()
    try:
        lineage = release_capsule_lineage.verify_release_capsule_lineage(
            git_object_root,
            pathlib.Path(args.source_snapshot),
            expected_commit,
            pathlib.Path(args.release_invocation),
        )
    except release_capsule_lineage.CapsuleLineageError as error:
        fail(f"release capsule lineage verification failed: {error}")
    root = lineage.source_tree.resolve(strict=True)
    commit = lineage.source_commit
    if not HEX_COMMIT.fullmatch(commit):
        fail("source commit must be a full 40- or 64-character hexadecimal object id")
    if args.buildroot_repository != CANONICAL_BUILDROOT_REPOSITORY:
        fail("Buildroot repository is not the canonical DCENT_OS source")

    cargo_lock = source_file(root, args.cargo_lock, "Cargo.lock")
    cargo_builder = source_file(root, args.cargo_build_script, "Cargo build definition")
    require_locked_cargo_builder(root / cargo_builder["path"])
    configs_in_merge_order = [
        source_file(root, config, "Buildroot config")
        for config in args.buildroot_config
    ]
    merge_order = [item["path"] for item in configs_in_merge_order]
    if len(set(merge_order)) != len(merge_order):
        fail("Buildroot config inputs contain duplicates")
    configs = sorted(
        configs_in_merge_order, key=lambda item: item["path"].encode("utf-8")
    )
    target = validate_identity(args.target, "target")
    arch = validate_identity(args.arch, "architecture")
    enforce_target_build_policy(target, arch, merge_order)
    if not args.build_input_snapshot:
        fail("schema-v4 generation requires a retained build-input snapshot")
    out_of_band_inputs = build_input_snapshot_evidence(
        args.build_input_snapshot,
        target,
        root,
        require_split_authority=True,
    )

    toolchain_hash = args.toolchain_sha256.lower()
    if not HEX_64.fullmatch(toolchain_hash):
        fail("toolchain SHA256 must be 64 lowercase hexadecimal characters")
    container_id = args.container_image_id.lower()
    if not CONTAINER_ID.fullmatch(container_id):
        fail("container image identity must be an immutable sha256:<64-hex> ID")
    buildroot_commit = args.buildroot_commit.lower()
    if not HEX_COMMIT.fullmatch(buildroot_commit):
        fail("Buildroot commit must be a full immutable object id")

    artifact_inputs = [
        assert_no_link_or_reparse_components(artifact, "artifact input")
        for artifact in args.artifact
    ]
    primary_artifact_input = artifact_inputs[0]
    artifact_parent = primary_artifact_input.parent
    if any(path.parent != artifact_parent for path in artifact_inputs):
        fail("all artifact inputs must share one exact artifact directory")
    artifacts = [artifact_entry(str(artifact)) for artifact in artifact_inputs]
    artifacts.sort(key=lambda item: item["path"].encode("utf-8"))
    if len({item["path"] for item in artifacts}) != len(artifacts):
        fail("artifact inputs contain duplicate basenames")
    prebuilt_rust_inputs = build_prebuilt_rust_inputs(
        args.prebuilt_rust_input,
        target,
        arch,
        primary_artifact_input.name,
        primary_artifact_input.parent,
        PREBUILT_RUST_RECEIPT_SCHEMA_VERSION,
        lineage.capsule,
        lineage.source_commit,
        root,
    )
    semantic_paths = {
        item[role]["path"]
        for item in prebuilt_rust_inputs["entries"]
        for role in ("binary", "receipt")
    }
    if semantic_paths.intersection(item["path"] for item in artifacts):
        fail("prebuilt Rust semantic inputs must not duplicate generic artifacts")

    epoch = int(args.source_date_epoch)
    return {
        "schema": SCHEMA,
        "created_at_utc": iso_utc(epoch),
        "source_date_epoch": epoch,
        "scope": closure_scope(args.receipt_authentication),
        "release_capsule": lineage.capsule,
        "source": {
            "commit": commit,
            "tree_state": "exact_git_object_snapshot",
        },
        "build": {
            "target": target,
            "arch": arch,
            "cargo": {
                "lockfile": cargo_lock,
                "builder_definition": cargo_builder,
                "dependency_resolution": "--locked-required",
            },
            "buildroot": {
                "repository": args.buildroot_repository,
                "commit": buildroot_commit,
                "checkout_policy": "exact-commit-and-no-untracked-or-modified-source-required",
                "checkout_verification": "build-driver-definition-bound",
                "configs": configs,
                "config_merge_order": merge_order,
                "external_tree": tree_digest(root, args.external_tree),
            },
            "toolchain": {
                "id": validate_identity(args.toolchain_id, "toolchain id"),
                "archive_sha256": toolchain_hash,
                "verification": "sha256-checked-by-build-driver",
            },
            "container": {
                "image_id": container_id,
                "definition": source_file(
                    root, args.container_definition, "container definition"
                ),
            },
            "out_of_band_inputs": out_of_band_inputs,
        },
        "prebuilt_rust_inputs": prebuilt_rust_inputs,
        "artifacts": artifacts,
    }


def write_manifest(args: argparse.Namespace) -> None:
    manifest = build_manifest(args)
    output = pathlib.Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp.{os.getpid()}")
    try:
        with temporary.open("xb") as stream:
            stream.write(canonical_bytes(manifest))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, output)
        if os.name == "posix":
            directory_fd = os.open(
                str(output.parent), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
            )
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
    print(output)


def verify_manifest(args: argparse.Namespace) -> None:
    manifest_path, raw = read_regular_nonsymlink(
        args.manifest, "source-closure manifest"
    )
    manifest = json.loads(raw)
    if isinstance(manifest, dict) and manifest.get("schema") in LEGACY_SCHEMAS:
        fail(LEGACY_SCHEMAS[manifest["schema"]])
    if not isinstance(manifest, dict) or manifest.get("schema") not in (
        SCHEMA,
        HISTORICAL_SCHEMA,
    ):
        fail("unsupported source-closure schema")
    manifest = validate_receipt_schema(manifest)
    schema = manifest["schema"]
    if raw != canonical_bytes(manifest):
        fail("source-closure manifest is not canonical JSON")
    scope = manifest.get("scope")
    if not isinstance(scope, dict):
        fail("source-closure claim scope is missing")
    receipt_authentication = scope.get("receipt_authentication")
    if receipt_authentication not in (
        RECEIPT_AUTH_UNSIGNED,
        RECEIPT_AUTH_DETACHED_REQUIRED,
    ) or scope != closure_scope(str(receipt_authentication), schema):
        fail("source-closure claim scope is invalid or overstates available evidence")
    verify_receipt_authentication(args, str(receipt_authentication), raw)

    source = manifest.get("source")
    build = manifest.get("build")
    if not isinstance(source, dict) or not isinstance(build, dict):
        fail("source-closure source and build sections must be objects")
    if not HEX_COMMIT.fullmatch(str(source.get("commit", ""))):
        fail("source-closure commit is not an immutable full object id")
    if schema == SCHEMA:
        if source.get("tree_state") != "exact_git_object_snapshot":
            fail(
                "schema-v4 source closure does not identify an exact Git-object snapshot"
            )
    elif source.get("tree_state") not in ("clean", "dirty"):
        fail("historical source-closure tree state is invalid")
    validate_identity(str(build.get("target", "")), "target")
    validate_identity(str(build.get("arch", "")), "architecture")
    cargo = build.get("cargo")
    buildroot = build.get("buildroot")
    toolchain = build.get("toolchain")
    container = build.get("container")
    out_of_band_inputs = build.get("out_of_band_inputs")
    if not all(
        isinstance(section, dict)
        for section in (cargo, buildroot, toolchain, container, out_of_band_inputs)
    ):
        fail("source-closure build subsections must be objects")
    if cargo.get("dependency_resolution") != "--locked-required":
        fail("source-closure Cargo dependency policy is not locked")
    if buildroot.get("repository") != CANONICAL_BUILDROOT_REPOSITORY:
        fail("source-closure Buildroot repository is not canonical")
    if not HEX_COMMIT.fullmatch(str(buildroot.get("commit", ""))):
        fail("source-closure Buildroot commit is not an immutable full object id")
    if (
        buildroot.get("checkout_policy")
        != "exact-commit-and-no-untracked-or-modified-source-required"
    ):
        fail("source-closure Buildroot checkout policy is weakened")
    if buildroot.get("checkout_verification") != "build-driver-definition-bound":
        fail("source-closure Buildroot verification scope is invalid")
    validate_identity(str(toolchain.get("id", "")), "toolchain id")
    if not HEX_64.fullmatch(str(toolchain.get("archive_sha256", ""))):
        fail("toolchain archive digest is invalid")
    if toolchain.get("verification") != "sha256-checked-by-build-driver":
        fail("source-closure toolchain verification claim is weakened")
    if not isinstance(buildroot.get("configs"), list):
        fail("source-closure Buildroot configs must be an array")
    if not isinstance(buildroot.get("external_tree"), dict):
        fail("source-closure Buildroot external tree must be an object")
    if not isinstance(manifest.get("artifacts"), list) or not manifest["artifacts"]:
        fail("source-closure artifacts must be a non-empty array")

    git_object_root = pathlib.Path(args.repo_root).resolve(strict=True)
    release_capsule = None
    if schema == SCHEMA:
        if not args.source_snapshot or not args.release_invocation:
            fail(
                "schema-v4 verification requires --source-snapshot and "
                "--release-invocation"
            )
        try:
            lineage = release_capsule_lineage.verify_release_capsule_lineage(
                git_object_root,
                pathlib.Path(args.source_snapshot),
                manifest["source"]["commit"],
                pathlib.Path(args.release_invocation),
            )
        except release_capsule_lineage.CapsuleLineageError as error:
            fail(f"release capsule lineage verification failed: {error}")
        release_capsule = lineage.capsule
        if release_capsule != manifest["release_capsule"]:
            fail("source-closure release capsule disagrees with verified authorities")
        root = lineage.source_tree.resolve(strict=True)
    else:
        if args.source_snapshot or args.release_invocation or args.build_input_snapshot:
            fail("historical schema-v3 verification uses only the live-checkout path")
        root = git_object_root
        if (
            git_output(root, "rev-parse", "HEAD").lower()
            != manifest["source"]["commit"]
        ):
            fail("historical source-closure commit disagrees with Git HEAD")
        if git_tree_state(root) != manifest["source"]["tree_state"]:
            fail("historical source-closure tree state disagrees with Git")
    if manifest["created_at_utc"] != iso_utc(manifest["source_date_epoch"]):
        fail("source-closure timestamp disagrees with source epoch")

    expected_lock = source_file(
        root, str(root / cargo["lockfile"]["path"]), "Cargo.lock"
    )
    expected_builder = source_file(
        root, str(root / cargo["builder_definition"]["path"]), "Cargo build definition"
    )
    require_locked_cargo_builder(root / expected_builder["path"])
    if (
        expected_lock != cargo["lockfile"]
        or expected_builder != cargo["builder_definition"]
    ):
        fail("Cargo source closure changed")

    expected_configs = [
        source_file(root, str(root / item["path"]), "Buildroot config")
        for item in buildroot["configs"]
    ]
    expected_configs.sort(key=lambda item: item["path"].encode("utf-8"))
    if expected_configs != buildroot["configs"]:
        fail("Buildroot config closure changed")
    merge_order = buildroot["config_merge_order"]
    config_paths = [item["path"] for item in expected_configs]
    if (
        not isinstance(merge_order, list)
        or len(set(merge_order)) != len(merge_order)
        or sorted(merge_order) != sorted(config_paths)
    ):
        fail("Buildroot config merge order is invalid")
    enforce_target_build_policy(build["target"], build["arch"], merge_order)
    expected_tree = tree_digest(root, str(root / buildroot["external_tree"]["path"]))
    if expected_tree != buildroot["external_tree"]:
        fail("Buildroot external tree closure changed")

    expected_container = source_file(
        root, str(root / container["definition"]["path"]), "container definition"
    )
    if expected_container != container["definition"]:
        fail("container definition closure changed")
    if not CONTAINER_ID.fullmatch(container["image_id"]):
        fail("container image identity is mutable")
    if not HEX_64.fullmatch(manifest["build"]["toolchain"]["archive_sha256"]):
        fail("toolchain archive digest is invalid")

    input_manifest = out_of_band_inputs.get("manifest")
    if not isinstance(input_manifest, dict) or not isinstance(
        input_manifest.get("path"), str
    ):
        fail("source-closure build-input manifest binding is invalid")
    if schema == SCHEMA:
        projection = getattr(args, "build_input_projection", None)
        portable_audit = bool(getattr(args, "portable_audit", False))
        if projection:
            if not portable_audit or args.build_input_snapshot:
                fail(
                    "retained build-input projections are accepted only by the "
                    "explicit post-cleanup portable-audit verifier"
                )
            expected_inputs = build_input_audit_projection_evidence(
                projection, build["target"], root
            )
        else:
            if portable_audit:
                fail("portable source-closure audit requires --build-input-projection")
            if not args.build_input_snapshot:
                fail("schema-v4 verification requires --build-input-snapshot")
            expected_inputs = build_input_snapshot_evidence(
                args.build_input_snapshot,
                build["target"],
                root,
                require_split_authority=True,
            )
        if expected_inputs != out_of_band_inputs:
            fail("retained out-of-band build-input snapshot closure changed")
    else:
        expected_inputs = build_input_evidence(
            root, str(root / input_manifest["path"]), build["target"]
        )
        expected_core = {
            key: expected_inputs[key]
            for key in ("manifest", "selection_policy", "files")
        }
        observed_core = {
            key: out_of_band_inputs[key]
            for key in ("manifest", "selection_policy", "files")
        }
        if expected_core != observed_core:
            fail("out-of-band build input closure changed")

    artifact_dir = assert_no_link_or_reparse_components(
        args.artifact_dir, "artifact directory"
    )
    artifact_dir_status = os.lstat(artifact_dir)
    if not stat.S_ISDIR(artifact_dir_status.st_mode) or is_windows_reparse(
        artifact_dir_status
    ):
        fail("artifact directory must be a non-reparse directory")
    declared_prebuilt = []
    for entry in manifest["prebuilt_rust_inputs"]["entries"]:
        declared_prebuilt.append(
            (
                entry["name"],
                str(artifact_dir / entry["binary"]["path"]),
                str(artifact_dir / entry["receipt"]["path"]),
            )
        )
    expected_prebuilt = build_prebuilt_rust_inputs(
        declared_prebuilt,
        build["target"],
        build["arch"],
        manifest["prebuilt_rust_inputs"]["packaging_artifact"],
        artifact_dir,
        PREBUILT_RUST_RECEIPT_SCHEMA_VERSION
        if schema == SCHEMA
        else HISTORICAL_PREBUILT_RUST_RECEIPT_SCHEMA_VERSION,
        release_capsule,
        manifest["source"]["commit"] if schema == SCHEMA else None,
        root if schema == SCHEMA else None,
    )
    if expected_prebuilt != manifest["prebuilt_rust_inputs"]:
        fail("retained prebuilt Rust input closure changed")
    expected_artifacts = [
        artifact_entry(str(artifact_dir / item["path"]))
        for item in manifest["artifacts"]
    ]
    expected_artifacts.sort(key=lambda item: item["path"].encode("utf-8"))
    if expected_artifacts != manifest["artifacts"]:
        fail("produced artifact closure changed")
    print(f"source closure verified: {manifest_path}")


def verify_build_inputs(args: argparse.Namespace) -> None:
    root = pathlib.Path(args.repo_root).resolve(strict=True)
    evidence = build_input_evidence(root, args.build_input_manifest, args.target)
    print(
        "out-of-band build inputs verified: "
        f"target={args.target} files={len(evidence['files'])} "
        f"policy={evidence['selection_policy']}"
    )


def _read_invocation_audit_projection(path_text: str) -> Dict[str, Any]:
    try:
        descriptor = release_invocation.verify_audit_descriptor(pathlib.Path(path_text))
    except release_invocation.InvocationError as error:
        fail(f"retained release-invocation descriptor is invalid: {error}")
    return descriptor


def verify_portable_manifest(args: argparse.Namespace) -> None:
    """Recheck v4 after private authorities have been deliberately destroyed."""

    manifest_path, manifest_raw = read_regular_nonsymlink(
        args.manifest, "source-closure manifest"
    )
    try:
        manifest = json.loads(manifest_raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"source-closure manifest is invalid JSON: {error}")
    manifest = validate_receipt_schema(manifest)
    if manifest.get("schema") != SCHEMA:
        fail(
            "portable post-cleanup verification supports current source-closure v4 only"
        )
    if manifest_raw != canonical_bytes(manifest):
        fail(f"source-closure manifest is not canonical JSON: {manifest_path}")
    scope = manifest["scope"]
    if scope != closure_scope(scope["receipt_authentication"], SCHEMA):
        fail("source-closure claim scope is invalid or overstated")
    # Authenticate the cheap bounded receipt before expensive Git reconstruction.
    verify_receipt_authentication(
        args, str(scope["receipt_authentication"]), manifest_raw
    )
    commit = manifest["source"]["commit"]
    try:
        source_projection = source_snapshot.verify_descriptor_against_git(
            pathlib.Path(args.repo_root),
            commit,
            pathlib.Path(args.source_snapshot_projection),
        )
    except (source_snapshot.SnapshotError, OSError, ValueError) as error:
        fail(f"retained source-snapshot projection is invalid: {error}")
    invocation_descriptor = _read_invocation_audit_projection(
        args.release_invocation_projection
    )
    expected_capsule = release_capsule_lineage.validate_release_capsule(
        manifest["release_capsule"]
    )
    projected_capsule = release_capsule_lineage.validate_release_capsule(
        {
            "schema": release_capsule_lineage.SCHEMA,
            "release_invocation_descriptor_sha256": release_invocation.sha256_bytes(
                release_invocation.canonical_bytes(invocation_descriptor)
            ),
            "release_invocation_id": invocation_descriptor["invocation_id"],
            "source_snapshot_id": source_projection["snapshot_id"],
            "source_snapshot_descriptor_sha256": source_projection["descriptor_sha256"],
        }
    )
    if projected_capsule != expected_capsule:
        fail("signed source-closure capsule disagrees with retained projections")

    parent = pathlib.Path(tempfile.mkdtemp(prefix="dcent-source-closure-audit-"))
    if os.name == "posix":
        os.chmod(parent, 0o700)
    try:
        created = source_snapshot.create_snapshot(
            pathlib.Path(args.repo_root), commit, parent
        )
        try:
            materialized = source_snapshot.verify_snapshot(created.snapshot, commit)
            _projected_path, projected_raw = read_regular_nonsymlink(
                args.source_snapshot_projection,
                "retained source-snapshot projection",
            )
            if projected_raw != source_snapshot.canonical_bytes(materialized):
                fail(
                    "materialized trusted-Git source differs from the retained projection"
                )

            audit_invocation = release_invocation.materialize_audit_verification_stage(
                invocation_descriptor, parent
            )

            delegated = argparse.Namespace(**vars(args))
            delegated.source_snapshot = str(created.snapshot)
            delegated.release_invocation = str(audit_invocation.stage)
            delegated.build_input_snapshot = None
            delegated.build_input_projection = args.build_input_projection
            delegated.portable_audit = True
            try:
                verify_manifest(delegated)
            finally:
                release_invocation.destroy_audit_verification_stage(
                    audit_invocation.stage
                )
        finally:
            source_snapshot.destroy_snapshot(created.snapshot, created.destroy_token)
    finally:
        # Never recursively traverse an attacker-modifiable temporary tree. The
        # exact authority helpers above remove only their known files; an
        # unexpected entry deliberately leaves this private directory behind.
        try:
            parent.rmdir()
        except OSError:
            pass
    print(
        "portable source-closure audit verified: "
        f"{manifest_path} (not live-stage, build-causality, or reproducibility proof)"
    )


def query_artifact(args: argparse.Namespace) -> None:
    manifest_path, raw = read_regular_nonsymlink(
        args.manifest, "source-closure manifest"
    )
    manifest = json.loads(raw)
    if not isinstance(manifest, dict) or manifest.get("schema") not in (
        SCHEMA,
        HISTORICAL_SCHEMA,
    ):
        fail("unsupported source-closure schema")
    manifest = validate_receipt_schema(manifest)
    if raw != canonical_bytes(manifest):
        fail(f"source-closure manifest is not canonical JSON: {manifest_path}")
    name = validate_flat_basename(args.path, "artifact query path")
    matches = [
        artifact for artifact in manifest["artifacts"] if artifact["path"] == name
    ]
    if len(matches) != 1:
        fail(f"artifact query matched {len(matches)} entries; exactly one is required")
    print(matches[0][args.field])


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    generate = commands.add_parser(
        "generate", help="write a canonical source-closure receipt"
    )
    generate.add_argument("--repo-root", required=True)
    generate.add_argument("--source-commit", required=True)
    generate.add_argument("--source-snapshot", required=True)
    generate.add_argument("--release-invocation", required=True)
    generate.add_argument("--source-date-epoch", type=int, required=True)
    generate.add_argument("--target", required=True)
    generate.add_argument("--arch", required=True)
    generate.add_argument("--cargo-lock", required=True)
    generate.add_argument("--cargo-build-script", required=True)
    generate.add_argument("--buildroot-repository", required=True)
    generate.add_argument("--buildroot-commit", required=True)
    generate.add_argument("--buildroot-config", action="append", required=True)
    generate.add_argument("--external-tree", required=True)
    generate.add_argument("--toolchain-id", required=True)
    generate.add_argument("--toolchain-sha256", required=True)
    generate.add_argument("--toolchain-verified", action="store_true", required=True)
    generate.add_argument("--container-image-id", required=True)
    generate.add_argument("--container-definition", required=True)
    generate.add_argument("--build-input-snapshot", required=True)
    generate.add_argument("--artifact", action="append", required=True)
    generate.add_argument(
        "--prebuilt-rust-input",
        action="append",
        nargs=3,
        metavar=("NAME", "BINARY", "RECEIPT"),
        required=True,
        help="target-required retained prebuilt binary and canonical build receipt",
    )
    generate.add_argument(
        "--receipt-authentication",
        choices=(RECEIPT_AUTH_UNSIGNED, RECEIPT_AUTH_DETACHED_REQUIRED),
        default=RECEIPT_AUTH_UNSIGNED,
    )
    generate.add_argument("--output", required=True)
    generate.set_defaults(function=write_manifest)

    verify = commands.add_parser(
        "verify", help="verify a receipt against offline source and artifacts"
    )
    verify.add_argument("--repo-root", required=True)
    verify.add_argument("--artifact-dir", required=True)
    verify.add_argument("--source-snapshot")
    verify.add_argument("--release-invocation")
    verify.add_argument("--build-input-snapshot")
    verify.add_argument("--signature")
    verify.add_argument("--public-key")
    verify.add_argument("manifest")
    verify.set_defaults(portable_audit=False, build_input_projection=None)
    verify.set_defaults(function=verify_manifest)

    verify_portable = commands.add_parser(
        "verify-portable",
        help="audit a signed v4 receipt after live private-stage cleanup",
    )
    verify_portable.add_argument("--repo-root", required=True)
    verify_portable.add_argument("--artifact-dir", required=True)
    verify_portable.add_argument("--source-snapshot-projection", required=True)
    verify_portable.add_argument("--release-invocation-projection", required=True)
    verify_portable.add_argument("--build-input-projection", required=True)
    verify_portable.add_argument("--signature", required=True)
    verify_portable.add_argument("--public-key", required=True)
    verify_portable.add_argument("manifest")
    verify_portable.set_defaults(function=verify_portable_manifest)

    verify_inputs = commands.add_parser(
        "verify-inputs", help="verify canonical target-scoped out-of-band build inputs"
    )
    verify_inputs.add_argument("--repo-root", required=True)
    verify_inputs.add_argument("--build-input-manifest", required=True)
    verify_inputs.add_argument("--target", required=True)
    verify_inputs.set_defaults(function=verify_build_inputs)
    query = commands.add_parser(
        "query-artifact", help="print one schema-validated artifact field"
    )
    query.add_argument("--manifest", required=True)
    query.add_argument("--path", required=True)
    query.add_argument("--field", choices=("sha256", "size"), required=True)
    query.set_defaults(function=query_artifact)
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (ClosureError, KeyError, TypeError, json.JSONDecodeError, OSError) as error:
        print(f"ERROR: source closure: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
