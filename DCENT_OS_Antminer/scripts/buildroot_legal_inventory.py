#!/usr/bin/env python3
"""Generate and verify a bounded Buildroot legal-info evidence inventory.

This custom inventory hash-enumerates the files emitted by Buildroot's
``make legal-info`` target and binds that observation to one firmware artifact.
It is deliberately not an SBOM, a source-compliance opinion, or proof that
Buildroot's legal-info output is complete or correct. Traversal rejects visible
symlink ancestry and bounds file count, aggregate material, and inventory size;
it is not a snapshot-isolation proof against a concurrently hostile filesystem.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import tempfile
from typing import Any, Dict, List, Optional


SCHEMA = "org.dcentral.dcentos.buildroot-legal-inventory.v1"
CANONICAL_BUILDROOT_REPOSITORY = "https://github.com/buildroot/buildroot.git"
HEX_COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
IDENTIFIER = re.compile(r"^[A-Za-z0-9._+:/@-]+$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
REQUIRED_FILES = (
    "README",
    "buildroot.config",
    "host-manifest.csv",
    "legal-info.sha256",
    "manifest.csv",
)
MAX_INVENTORY_BYTES = 64 * 1024 * 1024
MAX_LEGAL_INFO_FILES = 20_000
MAX_LEGAL_INFO_MATERIAL_BYTES = 8 * 1024 * 1024 * 1024
MAX_RELATIVE_PATH_BYTES = 1024
MAX_MANIFEST_HEADER_BYTES = 64 * 1024
SAFE_ARTIFACT_BASENAME = re.compile(r"^[A-Za-z0-9._+-]+$")


class InventoryError(ValueError):
    """A fail-closed Buildroot legal inventory error."""


def fail(message: str) -> "NoReturn":
    raise InventoryError(message)


def open_regular_nofollow(path: pathlib.Path, label: str) -> int:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            fail(f"{label} must be a regular file: {path}")
    except Exception:
        os.close(descriptor)
        raise
    return descriptor


def sha256_stream(stream: Any) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def canonical_bytes(value: Dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode(
        "ascii"
    )


def iso_utc(epoch: int) -> str:
    try:
        return dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    except (OverflowError, OSError, ValueError) as error:
        fail(f"source epoch cannot be represented: {error}")


def reject_symlink_ancestry(source: pathlib.Path, label: str) -> None:
    """Reject a path reached through any currently visible symlink component.

    The release builder controls these paths, so accepting a convenience symlink
    is unnecessary and weakens the meaning of the material that was inspected.
    Final-component ``O_NOFOLLOW`` checks remain in place for the open itself.
    """

    absolute = pathlib.Path(os.path.abspath(source))
    for component in (*reversed(absolute.parents), absolute):
        if component.is_symlink():
            fail(f"{label} must not have a symlink in its path ancestry: {component}")


def regular_file(path_text: str, label: str) -> pathlib.Path:
    source = pathlib.Path(path_text)
    reject_symlink_ancestry(source, label)
    path = source.resolve(strict=True)
    if not path.is_file():
        fail(f"{label} must be a regular file: {path}")
    return path


def file_binding(
    path: pathlib.Path, display_path: str, max_bytes: Optional[int] = None
) -> Dict[str, Any]:
    descriptor = open_regular_nofollow(path, display_path)
    with os.fdopen(descriptor, "rb") as stream:
        size = os.fstat(stream.fileno()).st_size
        if max_bytes is not None and size > max_bytes:
            fail(
                f"Buildroot legal-info material exceeds the "
                f"{MAX_LEGAL_INFO_MATERIAL_BYTES} byte aggregate limit at {display_path}"
            )
        digest = sha256_stream(stream)
        if os.fstat(stream.fileno()).st_size != size:
            fail(f"{display_path} changed size while it was being inspected")
    return {"path": display_path, "sha256": digest, "size": size}


def validate_relative_path(path: Any) -> str:
    if not isinstance(path, str) or not path:
        fail("Buildroot legal inventory contains an unsafe file path")
    normalized = pathlib.PurePosixPath(path)
    if (
        normalized.is_absolute()
        or ".." in normalized.parts
        or "\\" in path
        or any(ord(character) < 32 for character in path)
        or str(normalized) != path
        or len(path.encode("utf-8")) > MAX_RELATIVE_PATH_BYTES
    ):
        fail("Buildroot legal inventory contains an unsafe file path")
    return path


def validate_artifact_basename(value: Any) -> str:
    if not isinstance(value, str) or not SAFE_ARTIFACT_BASENAME.fullmatch(value):
        fail("Buildroot legal inventory artifact path must be a safe ASCII basename")
    if value in (".", "..") or len(value.encode("ascii")) > 255:
        fail("Buildroot legal inventory artifact path must be a safe ASCII basename")
    return value


def manifest_columns(path: pathlib.Path) -> set[str]:
    descriptor = open_regular_nofollow(path, f"Buildroot manifest {path.name}")
    with os.fdopen(descriptor, "rb") as stream:
        raw = stream.read(MAX_MANIFEST_HEADER_BYTES + 1)
    newline = raw.find(b"\n")
    if newline < 0 and len(raw) > MAX_MANIFEST_HEADER_BYTES:
        fail(f"Buildroot {path.name} header exceeds {MAX_MANIFEST_HEADER_BYTES} bytes")
    header = raw if newline < 0 else raw[:newline]
    try:
        decoded = header.decode("utf-8", errors="strict")
        row = next(csv.reader([decoded]))
    except (UnicodeDecodeError, csv.Error, StopIteration) as error:
        fail(f"Buildroot {path.name} has an invalid CSV header: {error}")
    return {column.strip().upper() for column in row}


def read_bounded_regular(path: pathlib.Path, label: str, max_bytes: int) -> bytes:
    descriptor = open_regular_nofollow(path, label)
    with os.fdopen(descriptor, "rb") as stream:
        size = os.fstat(stream.fileno()).st_size
        if size > max_bytes:
            fail(f"{label} is {size} bytes; limit is {max_bytes} bytes")
        raw = stream.read(max_bytes + 1)
    if len(raw) > max_bytes:
        fail(f"{label} grew beyond the {max_bytes} byte limit while reading")
    return raw


def validate_identity(value: str, label: str) -> str:
    if not IDENTIFIER.fullmatch(value):
        fail(f"{label} is missing or contains non-canonical characters")
    return value


def tree_digest(entries: List[Dict[str, Any]]) -> str:
    digest = hashlib.sha256()
    for entry in entries:
        digest.update(entry["path"].encode("utf-8"))
        digest.update(b"\0")
        digest.update(entry["sha256"].encode("ascii"))
        digest.update(b"\0")
        digest.update(str(entry["size"]).encode("ascii"))
        digest.update(b"\n")
    return digest.hexdigest()


def scan_legal_info(path_text: str) -> Dict[str, Any]:
    source = pathlib.Path(path_text)
    reject_symlink_ancestry(source, "Buildroot legal-info root")
    root = source.resolve(strict=True)
    if not root.is_dir():
        fail(f"Buildroot legal-info root must be a directory: {root}")

    entries: List[Dict[str, Any]] = []
    material_bytes = 0
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        directory_path = pathlib.Path(directory)
        for name in sorted(dirnames):
            candidate = directory_path / name
            if candidate.is_symlink():
                fail(f"Buildroot legal-info contains a symlink directory: {candidate}")
        for name in sorted(filenames):
            candidate = directory_path / name
            if candidate.is_symlink() or not candidate.is_file():
                fail(f"Buildroot legal-info contains a non-regular file: {candidate}")
            relative = validate_relative_path(candidate.relative_to(root).as_posix())
            binding = file_binding(
                candidate,
                relative,
                max_bytes=MAX_LEGAL_INFO_MATERIAL_BYTES - material_bytes,
            )
            entries.append(binding)
            material_bytes += binding["size"]
            if len(entries) > MAX_LEGAL_INFO_FILES:
                fail(
                    f"Buildroot legal-info contains more than {MAX_LEGAL_INFO_FILES} regular files"
                )
    entries.sort(key=lambda item: item["path"].encode("utf-8"))

    paths = {entry["path"] for entry in entries}
    missing = [name for name in REQUIRED_FILES if name not in paths]
    if missing:
        fail(f"Buildroot legal-info is missing required evidence files: {', '.join(missing)}")
    if not any(path.startswith("sources/") for path in paths):
        fail("Buildroot legal-info contains no package source archive evidence")
    if not any(path.startswith("licenses/") for path in paths):
        fail("Buildroot legal-info contains no target package license evidence")
    for manifest in ("manifest.csv", "host-manifest.csv"):
        columns = manifest_columns(root / manifest)
        if not {"PACKAGE", "VERSION"}.issubset(columns):
            fail(f"Buildroot {manifest} does not expose package and version columns")

    return {
        "digest_algorithm": "sha256-path-content-size-v1",
        "sha256": tree_digest(entries),
        "file_count": len(entries),
        "source_archive_file_count": sum(
            1 for entry in entries if entry["path"].startswith("sources/")
        ),
        "host_source_archive_file_count": sum(
            1 for entry in entries if entry["path"].startswith("host-sources/")
        ),
        "target_license_file_count": sum(
            1 for entry in entries if entry["path"].startswith("licenses/")
        ),
        "host_license_file_count": sum(
            1 for entry in entries if entry["path"].startswith("host-licenses/")
        ),
        "files": entries,
    }


def inventory_scope() -> Dict[str, Any]:
    return {
        "format": "dcentos-custom-buildroot-legal-info-inventory",
        "level": "partial",
        "is_sbom": False,
        "spdx_conformance": "not_claimed",
        "cyclonedx_conformance": "not_claimed",
        "binds": "regular files emitted by Buildroot make legal-info and one firmware artifact",
        "source_archive_availability": "hash-observed-in-ephemeral-build-output; archives are not embedded in this inventory",
        "license_compliance": "not_assessed",
        "vulnerability_analysis": "not_performed",
        "unresolved": [
            "Buildroot legal-info completeness and correctness are not independently established",
            "hash enumeration does not prove upstream source authenticity or long-term source availability",
            "license declarations and copied license texts are not audited for accuracy, compatibility, or obligations",
            "container base packages, Rust crates, firmware blobs, and inputs outside Buildroot legal-info are excluded",
            "kernel and bootloader origin is represented only when Buildroot legal-info emitted corresponding evidence",
            "advisory and vulnerability analysis is not performed",
        ],
    }


def build_inventory(args: argparse.Namespace) -> Dict[str, Any]:
    repository = args.buildroot_repository
    if repository != CANONICAL_BUILDROOT_REPOSITORY:
        fail("Buildroot repository is not the canonical DCENT_OS source")
    commit = args.buildroot_commit.lower()
    if not HEX_COMMIT.fullmatch(commit):
        fail("Buildroot commit must be a full immutable object id")
    artifact = regular_file(args.artifact, "firmware artifact")
    artifact_name = validate_artifact_basename(artifact.name)
    epoch = args.source_date_epoch
    if isinstance(epoch, bool) or not isinstance(epoch, int) or epoch < 0:
        fail("source epoch must be a non-negative integer")
    return {
        "schema": SCHEMA,
        "created_at_utc": iso_utc(epoch),
        "source_date_epoch": epoch,
        "scope": inventory_scope(),
        "build": {
            "target": validate_identity(args.target, "target"),
            "arch": validate_identity(args.arch, "architecture"),
            "buildroot_repository": repository,
            "buildroot_commit": commit,
            "producer": "make legal-info after the release Buildroot build",
        },
        "artifact": file_binding(artifact, artifact_name),
        "legal_info": scan_legal_info(args.legal_info_dir),
    }


def validate_file_entries(section: Dict[str, Any]) -> None:
    expected_section_keys = {
        "digest_algorithm",
        "sha256",
        "file_count",
        "source_archive_file_count",
        "host_source_archive_file_count",
        "target_license_file_count",
        "host_license_file_count",
        "files",
    }
    if set(section) != expected_section_keys:
        fail("Buildroot legal inventory legal_info schema is invalid")
    entries = section.get("files")
    if not isinstance(entries, list) or not entries:
        fail("Buildroot legal inventory files must be a non-empty array")
    previous = None
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "sha256", "size"}:
            fail("Buildroot legal inventory file entry schema is invalid")
        path = validate_relative_path(entry["path"])
        if previous is not None and previous.encode("utf-8") >= path.encode("utf-8"):
            fail("Buildroot legal inventory file paths are duplicate or unsorted")
        previous = path
        if not HEX_64.fullmatch(str(entry["sha256"])):
            fail("Buildroot legal inventory contains an invalid file digest")
        if (
            isinstance(entry["size"], bool)
            or not isinstance(entry["size"], int)
            or entry["size"] < 0
        ):
            fail("Buildroot legal inventory contains an invalid file size")
    if len(entries) > MAX_LEGAL_INFO_FILES:
        fail("Buildroot legal inventory file array exceeds the configured bound")
    if sum(entry["size"] for entry in entries) > MAX_LEGAL_INFO_MATERIAL_BYTES:
        fail("Buildroot legal inventory material exceeds the configured aggregate bound")
    if (
        isinstance(section.get("file_count"), bool)
        or section.get("file_count") != len(entries)
    ):
        fail("Buildroot legal inventory file count is inconsistent")
    if section.get("digest_algorithm") != "sha256-path-content-size-v1":
        fail("Buildroot legal inventory digest algorithm is invalid")
    if section.get("sha256") != tree_digest(entries):
        fail("Buildroot legal inventory tree digest is inconsistent")
    paths = {entry["path"] for entry in entries}
    missing = [name for name in REQUIRED_FILES if name not in paths]
    if missing:
        fail(f"Buildroot legal inventory omits required evidence files: {', '.join(missing)}")
    expected_counts = {
        "source_archive_file_count": sum(1 for entry in entries if entry["path"].startswith("sources/")),
        "host_source_archive_file_count": sum(
            1 for entry in entries if entry["path"].startswith("host-sources/")
        ),
        "target_license_file_count": sum(1 for entry in entries if entry["path"].startswith("licenses/")),
        "host_license_file_count": sum(1 for entry in entries if entry["path"].startswith("host-licenses/")),
    }
    for field, expected in expected_counts.items():
        if isinstance(section.get(field), bool) or section.get(field) != expected:
            fail(f"Buildroot legal inventory {field} is inconsistent")
    if expected_counts["source_archive_file_count"] < 1 or expected_counts["target_license_file_count"] < 1:
        fail("Buildroot legal inventory omits required source or target-license evidence")


def write_inventory(args: argparse.Namespace) -> None:
    inventory = build_inventory(args)
    encoded = canonical_bytes(inventory)
    if len(encoded) > MAX_INVENTORY_BYTES:
        fail(
            f"Buildroot legal inventory is {len(encoded)} bytes; limit is {MAX_INVENTORY_BYTES} bytes"
        )
    output = pathlib.Path(args.output)
    reject_symlink_ancestry(output, "Buildroot legal inventory output")
    output.parent.mkdir(parents=True, exist_ok=True)
    # Re-check after creation so a component replacement between the first
    # check and mkdir cannot silently redirect the atomic publication.
    reject_symlink_ancestry(output, "Buildroot legal inventory output")
    if not output.parent.resolve(strict=True).is_dir():
        fail(f"Buildroot legal inventory output parent must be a directory: {output.parent}")
    descriptor, temporary_text = tempfile.mkstemp(prefix=f".{output.name}.tmp.", dir=output.parent)
    temporary = pathlib.Path(temporary_text)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, output)
        if os.name == "posix":
            directory_fd = os.open(str(output.parent), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
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


def verify_inventory(args: argparse.Namespace) -> None:
    path = regular_file(args.inventory, "Buildroot legal inventory")
    raw = read_bounded_regular(path, "Buildroot legal inventory", MAX_INVENTORY_BYTES)
    try:
        inventory = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"inventory is invalid JSON: {error}")
    if not isinstance(inventory, dict) or inventory.get("schema") != SCHEMA:
        fail("unsupported Buildroot legal inventory schema")
    if set(inventory) != {
        "schema",
        "created_at_utc",
        "source_date_epoch",
        "scope",
        "build",
        "artifact",
        "legal_info",
    }:
        fail("Buildroot legal inventory top-level schema is invalid")
    if raw != canonical_bytes(inventory):
        fail("Buildroot legal inventory is not canonical JSON")
    if inventory.get("scope") != inventory_scope():
        fail("Buildroot legal inventory scope is invalid or overstates available evidence")
    build = inventory.get("build")
    artifact = inventory.get("artifact")
    legal_info = inventory.get("legal_info")
    if not all(isinstance(section, dict) for section in (build, artifact, legal_info)):
        fail("Buildroot legal inventory sections must be objects")
    if set(build) != {
        "target",
        "arch",
        "buildroot_repository",
        "buildroot_commit",
        "producer",
    }:
        fail("Buildroot legal inventory build schema is invalid")
    if not isinstance(build.get("target"), str) or not isinstance(build.get("arch"), str):
        fail("Buildroot legal inventory build identities must be strings")
    validate_identity(build["target"], "target")
    validate_identity(build["arch"], "architecture")
    if build.get("buildroot_repository") != CANONICAL_BUILDROOT_REPOSITORY:
        fail("Buildroot legal inventory repository is not canonical")
    if not isinstance(build.get("buildroot_commit"), str) or not HEX_COMMIT.fullmatch(
        build["buildroot_commit"]
    ):
        fail("Buildroot legal inventory commit is not immutable")
    if build.get("producer") != "make legal-info after the release Buildroot build":
        fail("Buildroot legal inventory producer claim is invalid")
    epoch = inventory.get("source_date_epoch")
    if isinstance(epoch, bool) or not isinstance(epoch, int) or epoch < 0:
        fail("Buildroot legal inventory source epoch is invalid")
    if not isinstance(inventory.get("created_at_utc"), str) or inventory.get(
        "created_at_utc"
    ) != iso_utc(epoch):
        fail("Buildroot legal inventory timestamp disagrees with source epoch")
    validate_file_entries(legal_info)

    if set(artifact) != {"path", "sha256", "size"}:
        fail("Buildroot legal inventory artifact schema is invalid")
    artifact_name = validate_artifact_basename(artifact.get("path"))
    artifact_dir_source = pathlib.Path(args.artifact_dir)
    reject_symlink_ancestry(artifact_dir_source, "firmware artifact directory")
    artifact_dir = artifact_dir_source.resolve(strict=True)
    if not artifact_dir.is_dir():
        fail(f"firmware artifact directory must be a directory: {artifact_dir}")
    expected_artifact = regular_file(str(artifact_dir / artifact_name), "firmware artifact")
    if artifact != file_binding(expected_artifact, artifact_name):
        fail("Buildroot legal inventory firmware artifact binding changed")

    if args.legal_info_dir:
        expected_legal_info = scan_legal_info(args.legal_info_dir)
        if expected_legal_info != legal_info:
            fail("Buildroot legal-info material changed")
        material_status = "reinspected"
    else:
        material_status = "not-reinspected"
    print(f"Buildroot legal inventory verified: {path} (materials={material_status})")


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    generate = commands.add_parser("generate", help="generate a canonical Buildroot legal inventory")
    generate.add_argument("--legal-info-dir", required=True)
    generate.add_argument("--buildroot-repository", required=True)
    generate.add_argument("--buildroot-commit", required=True)
    generate.add_argument("--target", required=True)
    generate.add_argument("--arch", required=True)
    generate.add_argument("--artifact", required=True)
    generate.add_argument("--source-date-epoch", type=int, required=True)
    generate.add_argument("--output", required=True)
    generate.set_defaults(function=write_inventory)

    verify = commands.add_parser("verify", help="verify inventory and artifact binding")
    verify.add_argument("--artifact-dir", required=True)
    verify.add_argument("--legal-info-dir")
    verify.add_argument("inventory")
    verify.set_defaults(function=verify_inventory)
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (InventoryError, KeyError, TypeError, OSError, ValueError) as error:
        print(f"ERROR: Buildroot legal inventory: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
