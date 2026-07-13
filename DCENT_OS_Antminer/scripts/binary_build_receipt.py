#!/usr/bin/env python3
"""Create and verify post-build receipts and private export snapshot sets.

The receipt detects drift between staged bytes and one recorded post-build
snapshot. It does not prove that the compiler consumed the listed inputs, does
not prove build causality, and is not a reproducible-build attestation.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import secrets
import stat
import sys
import tempfile
from typing import Callable, Iterable


SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIRECTORY))

import release_capsule_lineage  # noqa: E402
import build_input_snapshot  # noqa: E402


SCHEMA_VERSION = 4
HISTORICAL_SCHEMA_VERSION = 3
EXPORT_SET_SCHEMA_VERSION = 2
EXPORT_CAPABILITY_SCHEMA_VERSION = 1
RECEIPT_SUFFIX = ".build-receipt.json"
EXPORT_DESCRIPTOR_NAME = "export-set.json"
EXPORT_CAPABILITY_DIRECTORY = ".dcent-export-capabilities"
EXPORT_CAPABILITY_SUFFIX = ".destroy-capability.json"
HISTORICAL_RECEIPT_CLAIM_V3 = (
    "post-build-snapshot-consistency-not-build-causality-or-reproducibility-proof"
)
RECEIPT_CLAIM_V4 = (
    "declared-release-capsule-and-post-build-snapshot-consistency-"
    "not-build-causality-or-reproducibility-proof"
)
EXCLUDED_PARTS = {".git", "target", "__pycache__"}
BAKED_INPUTS = (
    "",
    "",
    "DCENT_OS_Antminer/scripts/build-dcentrald.sh",
    "DCENT_OS_Antminer/scripts/binary_build_receipt.py",
)
REQUIRED_SOURCE_INPUTS = (
    *BAKED_INPUTS,
    "DCENT_OS_Antminer/scripts/hw-acceptance/skus.conf",
    "DCENT_OS_Antminer/docs/architecture/install_matrix.tsv",
    "DCENT_OS_Antminer/dcentrald/dcentrald_s21xp.toml",
)
BUILD_ENV_KEYS = (
    "DCENT_MANIFEST_PUBLIC_KEY_HEX",
    "DCENT_MANIFEST_KEY_ID",
)
RELEASE_STATUSES = {"release", "production", "stable"}
V4_RECEIPT_KEYS = {
    "binary",
    "build_inputs",
    "build_environment",
    "build_variant",
    "builder",
    "cargo_metadata",
    "claim",
    "compile_environment",
    "git",
    "profile",
    "release_capsule",
    "schema_version",
    "source_inventory",
    "source_inventory_sha256",
    "target_triple",
    "toolchain_context",
}
CARGO_BUILD_INPUT_MANIFEST = "DCENT_OS_Antminer/scripts/build_inputs.manifest"
BUILD_INPUT_CLAIM = (
    "pre-build-external-input-snapshot-consistency-"
    "not-compiler-consumption-or-build-causality-proof"
)
BUILD_INPUT_SELECTION_AUTHORITY = (
    "manifest-from-same-git-authenticated-release-capsule-source-snapshot"
)
BUILD_INPUT_SELECTION_POLICY = "org.dcentral.dcentos.release-build-input-selection.v1"


class ReceiptError(RuntimeError):
    """A build receipt cannot be created or verified."""


# Test-only in-process race injection point. Production callers leave this as
# None. The post-open path-identity check must reject replacement performed by
# a hook rather than silently hashing an unlinked inode under the old pathname.
_AFTER_OPEN_HOOK: Callable[[Path, int], None] | None = None
_AFTER_EXPORT_VERIFY_HOOK: Callable[[], None] | None = None


def lexical_absolute(path: Path) -> Path:
    """Return an absolute lexical path without dereferencing symlinks."""

    return Path(os.path.abspath(os.fspath(path)))


def _is_windows_reparse(status: os.stat_result) -> bool:
    attributes = getattr(status, "st_file_attributes", 0)
    reparse = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse)


def _is_link_or_reparse(status: os.stat_result) -> bool:
    return stat.S_ISLNK(status.st_mode) or _is_windows_reparse(status)


def assert_no_symlink_components(
    path: Path,
    label: str,
    *,
    allow_missing_leaf: bool = False,
    allow_missing_tail: bool = False,
) -> Path:
    """Reject symlinks in every existing component of an absolute path."""

    absolute = lexical_absolute(path)
    parts = absolute.parts
    current = Path(absolute.anchor)
    for index, part in enumerate(parts[1:]):
        current /= part
        is_leaf = index == len(parts) - 2
        try:
            status = os.lstat(current)
        except FileNotFoundError as error:
            if allow_missing_tail:
                return absolute
            if allow_missing_leaf and is_leaf:
                return absolute
            raise ReceiptError(f"{label} is missing: {current}") from error
        except OSError as error:
            raise ReceiptError(
                f"cannot inspect {label} path {current}: {error}"
            ) from error
        if _is_link_or_reparse(status):
            raise ReceiptError(
                f"{label} path contains a symlink or reparse point: {current}"
            )
        if not is_leaf and not stat.S_ISDIR(status.st_mode):
            raise ReceiptError(f"{label} path component is not a directory: {current}")
    return absolute


def _stable_file_fields(status: os.stat_result) -> tuple[int, ...]:
    return (
        status.st_dev,
        status.st_ino,
        status.st_mode,
        status.st_size,
        status.st_mtime_ns,
        status.st_ctime_ns,
    )


def _same_file_identity(left: os.stat_result, right: os.stat_result) -> bool:
    if left.st_ino and right.st_ino:
        return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)
    # Some platforms expose no useful inode number. This is a best-effort
    # fallback; O_NOFOLLOW is also unavailable on some of those platforms.
    return _stable_file_fields(left) == _stable_file_fields(right)


def read_regular_snapshot(path: Path, label: str) -> tuple[bytes, dict[str, object]]:
    """Read a regular file once and bind its bytes to a stable path identity."""

    absolute = assert_no_symlink_components(path, label)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(absolute, flags)
    except OSError as error:
        raise ReceiptError(
            f"cannot open {label} for a regular-file snapshot: {absolute}: {error}"
        ) from error

    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or _is_windows_reparse(before):
            raise ReceiptError(f"{label} is not a non-reparse regular file: {absolute}")
        if _AFTER_OPEN_HOOK is not None:
            _AFTER_OPEN_HOOK(absolute, descriptor)
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        after = os.fstat(descriptor)
    except OSError as error:
        raise ReceiptError(f"cannot read {label}: {absolute}: {error}") from error
    finally:
        os.close(descriptor)

    if _stable_file_fields(before) != _stable_file_fields(after):
        raise ReceiptError(f"{label} changed while it was being read: {absolute}")
    try:
        pathname_status = os.lstat(absolute)
    except OSError as error:
        raise ReceiptError(
            f"{label} path changed after it was opened: {absolute}: {error}"
        ) from error
    if _is_link_or_reparse(pathname_status):
        raise ReceiptError(
            f"{label} was replaced by a symlink or reparse point while being read: {absolute}"
        )
    if not stat.S_ISREG(pathname_status.st_mode) or not _same_file_identity(
        after, pathname_status
    ):
        raise ReceiptError(f"{label} path was replaced while being read: {absolute}")

    content = b"".join(chunks)
    if len(content) != after.st_size:
        raise ReceiptError(f"{label} size changed while it was being read: {absolute}")
    return content, {
        "size": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }


def regular_files(root: Path) -> Iterable[Path]:
    root = assert_no_symlink_components(root, "source root")
    try:
        root_status = os.lstat(root)
    except OSError as error:
        raise ReceiptError(f"cannot inspect source root {root}: {error}") from error
    if not stat.S_ISDIR(root_status.st_mode) or _is_windows_reparse(root_status):
        raise ReceiptError(f"source root is not a non-reparse directory: {root}")

    def walk(directory: Path) -> Iterable[Path]:
        try:
            with os.scandir(directory) as iterator:
                entries = sorted(iterator, key=lambda entry: entry.name)
        except OSError as error:
            raise ReceiptError(
                f"cannot enumerate source directory {directory}: {error}"
            ) from error
        for entry in entries:
            if entry.name in EXCLUDED_PARTS:
                continue
            path = directory / entry.name
            try:
                status = entry.stat(follow_symlinks=False)
            except OSError as error:
                raise ReceiptError(
                    f"cannot inspect source input {path}: {error}"
                ) from error
            if _is_link_or_reparse(status):
                raise ReceiptError(
                    f"source input path contains a symlink or reparse point: {path}"
                )
            if stat.S_ISDIR(status.st_mode):
                yield from walk(path)
            elif stat.S_ISREG(status.st_mode):
                yield path
            else:
                raise ReceiptError(
                    f"source input is not a regular file or directory: {path}"
                )

    yield from walk(root)


def relevant_source_paths(repo_root: Path, workspace: Path) -> list[Path]:
    roots = [workspace]
    for relative in ("projects/dcent-schema", "DCENT_OS_Antminer/configs"):
        candidate = assert_no_symlink_components(
            repo_root / relative, "source root", allow_missing_tail=True
        )
        try:
            candidate_status = os.lstat(candidate)
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(candidate_status.st_mode) or _is_windows_reparse(
            candidate_status
        ):
            raise ReceiptError(
                f"source root is not a non-reparse directory: {candidate}"
            )
        roots.append(candidate)

    paths: set[Path] = set()
    for root in roots:
        paths.update(regular_files(root))
    for relative in REQUIRED_SOURCE_INPUTS:
        candidate = assert_no_symlink_components(
            repo_root / relative, "required source input"
        )
        status = os.lstat(candidate)
        if not stat.S_ISREG(status.st_mode) or _is_windows_reparse(status):
            raise ReceiptError(
                f"required source input is not a non-reparse regular file: {candidate}"
            )
        paths.add(candidate)
    return sorted(paths, key=lambda path: path.relative_to(repo_root).as_posix())


def source_inventory(
    repo_root: Path, workspace: Path
) -> tuple[list[dict[str, object]], str]:
    inventory: list[dict[str, object]] = []
    canonical = hashlib.sha256()
    for path in relevant_source_paths(repo_root, workspace):
        try:
            relative = path.relative_to(repo_root).as_posix()
        except ValueError as error:
            raise ReceiptError(f"source input is outside repo root: {path}") from error
        _, snapshot = read_regular_snapshot(path, "source input")
        size = snapshot["size"]
        digest = snapshot["sha256"]
        inventory.append({"path": relative, "size": size, "sha256": digest})
        canonical.update(f"{relative}\0{size}\0{digest}\n".encode("utf-8"))
    if not inventory:
        raise ReceiptError("source input inventory is empty")
    return inventory, canonical.hexdigest()


def relative_path(path: Path, repo_root: Path, label: str) -> str:
    try:
        return (
            lexical_absolute(path).relative_to(lexical_absolute(repo_root)).as_posix()
        )
    except ValueError as error:
        raise ReceiptError(f"{label} must be inside repo root: {path}") from error


def safe_relative_path(value: object, label: str) -> PurePosixPath:
    if not isinstance(value, str) or not value or "\\" in value:
        raise ReceiptError(f"{label} must be a non-empty canonical POSIX path")
    path = PurePosixPath(value)
    if path.is_absolute() or path.as_posix() != value:
        raise ReceiptError(f"{label} must be a canonical relative POSIX path")
    if any(part in ("", ".", "..") for part in path.parts):
        raise ReceiptError(f"{label} contains an unsafe path component")
    if any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise ReceiptError(f"{label} contains a control character")
    return path


def build_environment() -> dict[str, str]:
    return {key: os.environ.get(key, "") for key in BUILD_ENV_KEYS}


def context_file(path: Path, repo_root: Path, label: str) -> dict[str, object]:
    raw, snapshot = read_regular_snapshot(path, label)
    try:
        content = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ReceiptError(f"{label} file is not UTF-8: {path}: {error}") from error
    if not content.strip():
        raise ReceiptError(f"{label} file is empty: {path}")
    return {
        "path": relative_path(path, repo_root, label),
        **snapshot,
        "lines": content.splitlines(),
    }


def compile_environment_file(path: Path, repo_root: Path) -> dict[str, object]:
    context = context_file(path, repo_root, "compile environment")
    entries: dict[str, str] = {}
    for line in context["lines"]:
        if "=" not in line:
            raise ReceiptError(f"compile environment contains malformed line: {line!r}")
        key, value = line.split("=", 1)
        if not key or key in entries:
            raise ReceiptError(
                f"compile environment contains invalid/duplicate key: {key!r}"
            )
        entries[key] = value
    context["entries"] = dict(sorted(entries.items()))
    return context


def _validate_build_input_file(value: object, label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != {"path", "sha256", "size"}:
        raise ReceiptError(f"{label} has an invalid exact file-evidence schema")
    path = safe_relative_path(value.get("path"), f"{label} path").as_posix()
    digest = value.get("sha256")
    size = value.get("size")
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ReceiptError(f"{label} SHA-256 is invalid")
    if isinstance(size, bool) or not isinstance(size, int) or size < 0:
        raise ReceiptError(f"{label} size is invalid")
    return {"path": path, "sha256": digest, "size": size}


def validate_build_inputs_record(value: object, label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != {
        "claim",
        "evidence",
        "selection_authority",
    }:
        raise ReceiptError(f"{label} has an invalid exact schema")
    if value.get("claim") != BUILD_INPUT_CLAIM:
        raise ReceiptError(f"{label} has an invalid claim")
    if value.get("selection_authority") != BUILD_INPUT_SELECTION_AUTHORITY:
        raise ReceiptError(f"{label} has an invalid selection authority")
    evidence = value.get("evidence")
    if not isinstance(evidence, dict) or set(evidence) != {
        "files",
        "manifest",
        "selection_policy",
        "snapshot",
    }:
        raise ReceiptError(f"{label} evidence has an invalid exact schema")
    manifest = _validate_build_input_file(evidence.get("manifest"), f"{label} manifest")
    if manifest["path"] != CARGO_BUILD_INPUT_MANIFEST:
        raise ReceiptError(
            f"{label} manifest path is not the Cargo selection authority"
        )
    files = evidence.get("files")
    if files != []:
        raise ReceiptError(f"{label} must bind the empty Cargo external-input selection")
    snapshot = evidence.get("snapshot")
    if not isinstance(snapshot, dict) or set(snapshot) != {
        "claim",
        "snapshot_id",
        "target",
    }:
        raise ReceiptError(f"{label} snapshot identity has an invalid exact schema")
    if snapshot.get("claim") != build_input_snapshot.SNAPSHOT_CLAIM:
        raise ReceiptError(f"{label} snapshot claim is invalid")
    snapshot_id = snapshot.get("snapshot_id")
    if (
        not isinstance(snapshot_id, str)
        or re.fullmatch(r"[0-9a-f]{64}", snapshot_id) is None
    ):
        raise ReceiptError(f"{label} snapshot ID is invalid")
    if snapshot.get("target") != "cargo-workspace":
        raise ReceiptError(f"{label} snapshot target is invalid")
    selection_policy = evidence.get("selection_policy")
    if selection_policy != BUILD_INPUT_SELECTION_POLICY:
        raise ReceiptError(f"{label} selection policy is invalid")
    return {
        "claim": BUILD_INPUT_CLAIM,
        "selection_authority": BUILD_INPUT_SELECTION_AUTHORITY,
        "evidence": {
            "manifest": manifest,
            "selection_policy": selection_policy,
            "files": [],
            "snapshot": {
                "snapshot_id": snapshot_id,
                "target": "cargo-workspace",
                "claim": build_input_snapshot.SNAPSHOT_CLAIM,
            },
        },
    }


def cargo_build_inputs(
    snapshot_path: Path,
    source_tree: Path,
    compile_environment: dict[str, object],
) -> dict[str, object]:
    try:
        descriptor = build_input_snapshot.verify_snapshot(
            lexical_absolute(snapshot_path), "cargo-workspace"
        )
    except (
        build_input_snapshot.SnapshotError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        raise ReceiptError(f"Cargo build-input snapshot is invalid: {error}") from error
    if descriptor.get("schema") != build_input_snapshot.SPLIT_AUTHORITY_SCHEMA:
        raise ReceiptError(
            "schema-v4 receipts require a split-authority build-input snapshot"
        )
    if descriptor.get("selection_root") != {
        "kind": build_input_snapshot.SELECTION_ROOT_KIND
    }:
        raise ReceiptError("Cargo build-input snapshot selection authority is invalid")
    record = validate_build_inputs_record(
        {
            "claim": BUILD_INPUT_CLAIM,
            "selection_authority": BUILD_INPUT_SELECTION_AUTHORITY,
            "evidence": build_input_snapshot.snapshot_evidence(descriptor),
        },
        "Cargo build inputs",
    )
    manifest = record["evidence"]["manifest"]
    manifest_path = assert_no_symlink_components(
        source_tree.joinpath(*PurePosixPath(str(manifest["path"])).parts),
        "capsule build-input manifest",
    )
    _, observed_manifest = read_regular_snapshot(
        manifest_path, "capsule build-input manifest"
    )
    if (
        observed_manifest["sha256"] != manifest["sha256"]
        or observed_manifest["size"] != manifest["size"]
    ):
        raise ReceiptError(
            "Cargo build-input manifest does not match the authenticated source snapshot"
        )
    entries = compile_environment.get("entries")
    if not isinstance(entries, dict):
        raise ReceiptError("compile environment entries are invalid")
    legacy_entries = sorted(
        key for key in entries if key.startswith("DCENT_STOCK_FPGA_")
    )
    if legacy_entries:
        raise ReceiptError(
            "compile environment still carries removed stock FPGA authority: "
            + ", ".join(legacy_entries)
        )
    return record


def builder_evidence(entries: dict[str, str]) -> dict[str, str]:
    required = (
        "DCENT_BUILDER_KIND",
        "DCENT_BUILDER_BASE_REFERENCE",
        "DCENT_BUILDER_IMAGE_ID",
        "DCENT_BUILDER_PACKAGE_RESOLUTION",
    )
    missing = [key for key in required if key not in entries]
    if missing:
        raise ReceiptError(f"compile environment omits builder identity: {missing}")
    kind = entries["DCENT_BUILDER_KIND"]
    if kind not in {"docker-cross", "native-host"}:
        raise ReceiptError(
            f"compile environment has unsupported builder kind: {kind!r}"
        )
    return {
        "kind": kind,
        "base_reference": entries["DCENT_BUILDER_BASE_REFERENCE"],
        "image_id": entries["DCENT_BUILDER_IMAGE_ID"],
        "package_resolution": entries["DCENT_BUILDER_PACKAGE_RESOLUTION"],
    }


def require_immutable_docker_builder(builder: object) -> None:
    if not isinstance(builder, dict) or set(builder) != {
        "kind",
        "base_reference",
        "image_id",
        "package_resolution",
    }:
        raise ReceiptError("build receipt builder evidence has an invalid schema")
    base = builder.get("base_reference")
    image = builder.get("image_id")
    if builder.get("kind") != "docker-cross":
        raise ReceiptError("release packaging requires a docker-cross build receipt")
    if not isinstance(base, str) or not re.fullmatch(
        r"(?:[^/@]+/)*[^/@]+@sha256:[0-9a-f]{64}", base
    ):
        raise ReceiptError(
            "release packaging requires an immutable builder base digest"
        )
    if not isinstance(image, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise ReceiptError("release packaging requires an immutable builder image ID")


def receipt_path(binary: Path) -> Path:
    return binary.with_name(binary.name + RECEIPT_SUFFIX)


def common_receipt_data(args: argparse.Namespace) -> dict[str, object]:
    try:
        lineage = release_capsule_lineage.verify_release_capsule_lineage(
            args.git_object_repo,
            args.source_snapshot,
            args.source_commit,
            args.release_invocation,
        )
    except release_capsule_lineage.CapsuleLineageError as error:
        raise ReceiptError(f"release capsule verification failed: {error}") from error
    source_tree = assert_no_symlink_components(
        lineage.source_tree, "snapshot source tree"
    )
    workspace_relative = safe_relative_path(
        args.source_workspace, "snapshot source workspace"
    )
    workspace = assert_no_symlink_components(
        source_tree.joinpath(*workspace_relative.parts), "snapshot source workspace"
    )
    result_root = assert_no_symlink_components(
        args.result_root, "invocation result root"
    )
    if not source_tree.is_dir():
        raise ReceiptError(f"snapshot source tree is not a directory: {source_tree}")
    if not workspace.is_dir():
        raise ReceiptError(f"snapshot source workspace is not a directory: {workspace}")
    if not result_root.is_dir():
        raise ReceiptError(f"invocation result root is not a directory: {result_root}")
    if result_root == source_tree or result_root == lineage.invocation_stage:
        raise ReceiptError(
            "invocation result root must be separate from source and invocation control state"
        )
    metadata = lexical_absolute(args.metadata)
    _, metadata_snapshot = read_regular_snapshot(metadata, "Cargo metadata")
    inventory, inventory_digest = source_inventory(source_tree, workspace)
    compile_environment = compile_environment_file(
        lexical_absolute(args.compile_environment), result_root
    )
    build_inputs = cargo_build_inputs(
        lexical_absolute(args.build_input_snapshot), source_tree, compile_environment
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "claim": RECEIPT_CLAIM_V4,
        "release_capsule": lineage.capsule,
        "build_inputs": build_inputs,
        "target_triple": args.target,
        "profile": args.profile,
        "build_variant": args.build_variant,
        "git": {
            "commit": lineage.source_commit,
            "source_kind": "exact-git-object-snapshot",
        },
        "build_environment": build_environment(),
        "builder": builder_evidence(compile_environment["entries"]),
        "toolchain_context": context_file(
            lexical_absolute(args.toolchain_context), result_root, "toolchain context"
        ),
        "compile_environment": compile_environment,
        "source_inventory_sha256": inventory_digest,
        "source_inventory": inventory,
        "cargo_metadata": {
            "path": relative_path(metadata, result_root, "Cargo metadata"),
            **metadata_snapshot,
        },
    }


def binary_data(binary: Path, result_root: Path) -> dict[str, object]:
    _, snapshot = read_regular_snapshot(binary, "built binary")
    return binary_data_from_snapshot(binary, result_root, snapshot)


def binary_data_from_snapshot(
    binary: Path, result_root: Path, snapshot: dict[str, object]
) -> dict[str, object]:
    return {
        "name": binary.name,
        "path": relative_path(binary, result_root, "binary"),
        **snapshot,
    }


def canonical_json_bytes(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def parse_json_object(raw: bytes, path: Path, label: str) -> dict[str, object]:
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReceiptError(f"cannot parse {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise ReceiptError(f"{label} is not a JSON object: {path}")
    return value


def _write_private_file(path: Path, content: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    creation_mode = 0o400 if os.name == "posix" else 0o600
    descriptor = os.open(path, flags, creation_mode)
    try:
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise ReceiptError(f"short write while exporting snapshot file: {path}")
            view = view[written:]
        os.fsync(descriptor)
        if os.name == "posix" and hasattr(os, "fchmod"):
            os.fchmod(descriptor, 0o400)
    finally:
        os.close(descriptor)


def _fsync_directory(path: Path) -> None:
    if not hasattr(os, "O_DIRECTORY"):
        return
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _set_new_directory_private(path: Path) -> None:
    if os.name != "posix":
        return
    flags = os.O_RDONLY | os.O_DIRECTORY | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        os.fchmod(descriptor, 0o700)
    finally:
        os.close(descriptor)


def export_capability_path(stage: Path) -> Path:
    stage = lexical_absolute(stage)
    return (
        stage.parent
        / EXPORT_CAPABILITY_DIRECTORY
        / f"{stage.name}{EXPORT_CAPABILITY_SUFFIX}"
    )


def _ensure_private_capability_directory(stage_parent: Path) -> Path:
    directory = stage_parent / EXPORT_CAPABILITY_DIRECTORY
    created = False
    try:
        os.mkdir(directory, 0o700)
        created = True
    except FileExistsError:
        pass
    if created:
        _set_new_directory_private(directory)
    directory = assert_no_symlink_components(directory, "capability directory")
    status = os.lstat(directory)
    if not stat.S_ISDIR(status.st_mode):
        raise ReceiptError(f"capability directory is not a directory: {directory}")
    _check_owned_status(directory, status, "capability directory")
    _check_private_mode(directory, 0o700, "capability directory")
    return directory


def _declared_export_pairs(args: argparse.Namespace) -> list[tuple[Path, Path]]:
    pairs: list[tuple[Path, Path]] = []
    seen: dict[Path, str] = {}
    for binary_value, receipt_value in args.pair:
        binary = lexical_absolute(binary_value)
        receipt = lexical_absolute(receipt_value)
        if binary == receipt:
            raise ReceiptError(f"binary and receipt paths must differ: {binary}")
        if binary.name == receipt.name:
            raise ReceiptError(
                "binary and receipt names would collide in the private export pair: "
                f"{binary.name}"
            )
        for path, role in ((binary, "binary"), (receipt, "receipt")):
            previous = seen.get(path)
            if previous is not None:
                raise ReceiptError(
                    f"duplicate declared export path used as {previous} and {role}: {path}"
                )
            seen[path] = role
        pairs.append((binary, receipt))
    if not pairs:
        raise ReceiptError("at least one binary/receipt export pair is required")
    return pairs


def _common_input_paths(
    common: dict[str, object], args: argparse.Namespace, source_tree: Path
) -> set[Path]:
    inputs = {
        lexical_absolute(args.metadata),
        lexical_absolute(args.toolchain_context),
        lexical_absolute(args.compile_environment),
    }
    for item in common["source_inventory"]:
        inputs.add(source_tree / item["path"])
    return inputs


def _capture_verified_export_pairs(
    args: argparse.Namespace,
) -> list[dict[str, object]]:
    result_root = assert_no_symlink_components(
        args.result_root, "invocation result root"
    )
    try:
        lineage = release_capsule_lineage.verify_release_capsule_lineage(
            args.git_object_repo,
            args.source_snapshot,
            args.source_commit,
            args.release_invocation,
        )
    except release_capsule_lineage.CapsuleLineageError as error:
        raise ReceiptError(f"release capsule verification failed: {error}") from error
    declared_pairs = _declared_export_pairs(args)
    common = common_receipt_data(args)
    common_inputs = _common_input_paths(common, args, lineage.source_tree)
    captured: list[dict[str, object]] = []
    for binary, receipt in declared_pairs:
        if binary in common_inputs or receipt in common_inputs:
            raise ReceiptError(
                "declared binary/receipt path overlaps an input already read for the "
                f"snapshot: {binary if binary in common_inputs else receipt}"
            )
        binary_raw, binary_snapshot = read_regular_snapshot(binary, "export binary")
        receipt_raw, receipt_snapshot = read_regular_snapshot(receipt, "export receipt")
        observed = parse_json_object(receipt_raw, receipt, "build receipt")
        if receipt_raw != canonical_json_bytes(observed):
            raise ReceiptError(f"declared build receipt is not canonical: {receipt}")
        validate_v4_receipt_shape(observed, f"declared build receipt {receipt}")
        expected = dict(common)
        expected["binary"] = binary_data_from_snapshot(
            binary, result_root, binary_snapshot
        )
        if observed != expected:
            differing = sorted(
                key
                for key in set(observed) | set(expected)
                if observed.get(key) != expected.get(key)
            )
            raise ReceiptError(
                f"declared build receipt does not match captured binary/snapshot: "
                f"{receipt}; differing sections: {', '.join(differing) or 'unknown'}"
            )
        if getattr(args, "require_immutable_builder", False):
            require_immutable_docker_builder(observed.get("builder"))
        captured.append(
            {
                "binary_path": binary,
                "binary_raw": binary_raw,
                "binary_snapshot": binary_snapshot,
                "receipt_path": receipt,
                "receipt_raw": receipt_raw,
                "receipt_snapshot": receipt_snapshot,
                "release_capsule": observed["release_capsule"],
            }
        )
    captured.sort(
        key=lambda item: relative_path(
            item["binary_path"], result_root, "export binary"
        )
    )
    if _AFTER_EXPORT_VERIFY_HOOK is not None:
        _AFTER_EXPORT_VERIFY_HOOK()
    return captured


def _export_file_record(
    source_path: Path,
    export_path: str,
    snapshot: dict[str, object],
    result_root: Path,
    label: str,
) -> dict[str, object]:
    return {
        "source_path": relative_path(source_path, result_root, label),
        "export_path": export_path,
        **snapshot,
    }


def _export_descriptor(
    captured: list[dict[str, object]], result_root: Path
) -> dict[str, object]:
    artifacts: list[dict[str, object]] = []
    for index, item in enumerate(captured):
        directory = f"artifacts/{index:04d}"
        binary_path = item["binary_path"]
        receipt = item["receipt_path"]
        artifacts.append(
            {
                "binary": _export_file_record(
                    binary_path,
                    f"{directory}/{binary_path.name}",
                    item["binary_snapshot"],
                    result_root,
                    "export binary",
                ),
                "receipt": _export_file_record(
                    receipt,
                    f"{directory}/{receipt.name}",
                    item["receipt_snapshot"],
                    result_root,
                    "export receipt",
                ),
            }
        )
    artifact_bytes = canonical_json_bytes(artifacts)
    return {
        "schema_version": EXPORT_SET_SCHEMA_VERSION,
        "claim": RECEIPT_CLAIM_V4,
        "release_capsule": release_capsule_lineage.validate_release_capsule(
            captured[0]["release_capsule"]
        ),
        "artifacts": artifacts,
        "artifacts_sha256": hashlib.sha256(artifact_bytes).hexdigest(),
    }


def _destruction_capability(
    stage: Path, descriptor: dict[str, object]
) -> dict[str, object]:
    return {
        "schema_version": EXPORT_CAPABILITY_SCHEMA_VERSION,
        "stage_path": str(lexical_absolute(stage)),
        "descriptor_sha256": hashlib.sha256(
            canonical_json_bytes(descriptor)
        ).hexdigest(),
        "token": secrets.token_hex(32),
    }


def export_snapshot_set(args: argparse.Namespace) -> Path:
    result_root = assert_no_symlink_components(
        args.result_root, "invocation result root"
    )
    stage_parent = assert_no_symlink_components(args.stage_parent, "stage parent")
    if not stage_parent.is_dir():
        raise ReceiptError(f"stage parent is not a directory: {stage_parent}")
    captured = _capture_verified_export_pairs(args)
    descriptor = _export_descriptor(captured, result_root)
    capability_directory = _ensure_private_capability_directory(stage_parent)

    suffix = secrets.token_hex(12)
    temporary = Path(
        tempfile.mkdtemp(prefix=f".dcent-export-set-{suffix}-", dir=stage_parent)
    )
    final = stage_parent / f"dcent-export-set-{suffix}"
    capability = export_capability_path(final)
    capability_temporary = capability_directory / f".{capability.name}.{suffix}.tmp"
    published = False
    try:
        assert_no_symlink_components(
            capability, "destruction capability", allow_missing_leaf=True
        )
        assert_no_symlink_components(
            capability_temporary,
            "temporary destruction capability",
            allow_missing_leaf=True,
        )
        _set_new_directory_private(temporary)
        artifacts_directory = temporary / "artifacts"
        artifacts_directory.mkdir(mode=0o700)
        _set_new_directory_private(artifacts_directory)
        for index, item in enumerate(captured):
            pair_directory = artifacts_directory / f"{index:04d}"
            pair_directory.mkdir(mode=0o700)
            _set_new_directory_private(pair_directory)
            binary_export = pair_directory / item["binary_path"].name
            receipt_export = pair_directory / item["receipt_path"].name
            _write_private_file(binary_export, item["binary_raw"])
            _write_private_file(receipt_export, item["receipt_raw"])
            _fsync_directory(pair_directory)
        _fsync_directory(artifacts_directory)
        _write_private_file(
            temporary / EXPORT_DESCRIPTOR_NAME, canonical_json_bytes(descriptor)
        )
        _fsync_directory(temporary)
        os.rename(temporary, final)
        published = True
        verify_export_snapshot_set(final)
        _write_private_file(
            capability_temporary,
            canonical_json_bytes(_destruction_capability(final, descriptor)),
        )
        os.rename(capability_temporary, capability)
        _verify_destruction_capability(final, capability, descriptor)
        _fsync_directory(capability_directory)
        _fsync_directory(stage_parent)
    except (OSError, ReceiptError) as error:
        quarantined = final if published else temporary
        quarantined_paths = [
            path
            for path in (quarantined, capability, capability_temporary)
            if path.exists()
        ]
        detail = ""
        if quarantined_paths:
            detail = "; private export material left quarantined at " + ", ".join(
                str(path) for path in quarantined_paths
            )
        if isinstance(error, ReceiptError):
            raise ReceiptError(f"{error}{detail}") from error
        raise ReceiptError(
            f"cannot export private snapshot set: {error}{detail}"
        ) from error
    return final


def _require_exact_keys(
    value: dict[str, object], expected: set[str], label: str
) -> None:
    if set(value) != expected:
        raise ReceiptError(
            f"{label} has unexpected fields; expected {sorted(expected)}, "
            f"found {sorted(value)}"
        )


def _safe_relative_export_path(value: object, label: str) -> PurePosixPath:
    if (
        not isinstance(value, str)
        or not value
        or "\\" in value
        or any(character in value for character in ("\x00", "\r", "\n"))
    ):
        raise ReceiptError(f"{label} is not a safe POSIX-relative path: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise ReceiptError(f"{label} is not a safe POSIX-relative path: {value!r}")
    if path.as_posix() != value:
        raise ReceiptError(f"{label} is not canonical: {value!r}")
    return path


def _validated_export_file_record(
    value: object, label: str
) -> tuple[dict[str, object], PurePosixPath]:
    if not isinstance(value, dict):
        raise ReceiptError(f"{label} is not an object")
    _require_exact_keys(value, {"source_path", "export_path", "size", "sha256"}, label)
    _safe_relative_export_path(value["source_path"], f"{label} source")
    export_path = _safe_relative_export_path(value["export_path"], f"{label} export")
    if not isinstance(value["size"], int) or isinstance(value["size"], bool):
        raise ReceiptError(f"{label} size is not an integer")
    if value["size"] < 0:
        raise ReceiptError(f"{label} size is negative")
    digest = value["sha256"]
    if (
        not isinstance(digest, str)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        raise ReceiptError(f"{label} sha256 is malformed")
    return value, export_path


def _check_private_mode(path: Path, expected: int, label: str) -> None:
    # Windows chmod does not model POSIX owner/group/other bits. The production
    # Linux boundary enforces exact modes; Windows retains read-only best effort.
    if os.name != "posix":
        return
    observed = stat.S_IMODE(os.lstat(path).st_mode)
    if observed != expected:
        raise ReceiptError(
            f"{label} has mode {observed:04o}, expected exactly {expected:04o}: {path}"
        )


def _check_owned_status(
    path: Path,
    status: os.stat_result,
    label: str,
    *,
    require_single_link: bool = False,
) -> None:
    if os.name == "posix" and status.st_uid != os.geteuid():
        raise ReceiptError(
            f"{label} is not owned by the current effective user: {path}"
        )
    link_count = getattr(status, "st_nlink", 0)
    if require_single_link and link_count and link_count != 1:
        raise ReceiptError(
            f"{label} has {link_count} hard links; exactly one is required: {path}"
        )


def _stage_tree(stage: Path) -> tuple[set[str], set[str]]:
    files: set[str] = set()
    directories: set[str] = set()

    def walk(directory: Path, relative: PurePosixPath | None = None) -> None:
        try:
            with os.scandir(directory) as iterator:
                entries = sorted(iterator, key=lambda entry: entry.name)
        except OSError as error:
            raise ReceiptError(
                f"cannot enumerate export stage {directory}: {error}"
            ) from error
        for entry in entries:
            path = directory / entry.name
            child = (
                PurePosixPath(entry.name) if relative is None else relative / entry.name
            )
            try:
                # os.lstat reports NTFS hardlink counts correctly on Windows;
                # DirEntry.stat may retain a stale enumeration-time count.
                status = os.lstat(path)
            except OSError as error:
                raise ReceiptError(
                    f"cannot inspect export stage entry {path}: {error}"
                ) from error
            if _is_link_or_reparse(status):
                raise ReceiptError(
                    f"export stage contains a symlink or reparse point: {path}"
                )
            if stat.S_ISDIR(status.st_mode):
                _check_owned_status(path, status, "export stage directory")
                directories.add(child.as_posix())
                _check_private_mode(path, 0o700, "export stage directory")
                walk(path, child)
            elif stat.S_ISREG(status.st_mode):
                _check_owned_status(
                    path, status, "export stage file", require_single_link=True
                )
                files.add(child.as_posix())
                _check_private_mode(path, 0o400, "export stage file")
            else:
                raise ReceiptError(f"export stage contains a special file: {path}")

    walk(stage)
    return files, directories


def verify_export_snapshot_set(stage_value: Path) -> dict[str, object]:
    stage = assert_no_symlink_components(stage_value, "export stage")
    stage_status = os.lstat(stage)
    if not stat.S_ISDIR(stage_status.st_mode) or _is_windows_reparse(stage_status):
        raise ReceiptError(f"export stage is not a non-reparse directory: {stage}")
    _check_owned_status(stage, stage_status, "export stage")
    _check_private_mode(stage, 0o700, "export stage")

    descriptor_path = stage / EXPORT_DESCRIPTOR_NAME
    descriptor_raw, _ = read_regular_snapshot(descriptor_path, "export set descriptor")
    descriptor = parse_json_object(
        descriptor_raw, descriptor_path, "export set descriptor"
    )
    if descriptor_raw != canonical_json_bytes(descriptor):
        raise ReceiptError(f"export set descriptor is not canonical: {descriptor_path}")
    _require_exact_keys(
        descriptor,
        {
            "schema_version",
            "claim",
            "release_capsule",
            "artifacts",
            "artifacts_sha256",
        },
        "export set descriptor",
    )
    if (
        type(descriptor["schema_version"]) is not int
        or descriptor["schema_version"] != EXPORT_SET_SCHEMA_VERSION
    ):
        raise ReceiptError("unsupported export set schema version")
    if descriptor["claim"] != RECEIPT_CLAIM_V4:
        raise ReceiptError("export set descriptor has an invalid claim")
    try:
        descriptor_capsule = release_capsule_lineage.validate_release_capsule(
            descriptor["release_capsule"]
        )
    except release_capsule_lineage.CapsuleLineageError as error:
        raise ReceiptError(f"export set release capsule is invalid: {error}") from error
    artifacts = descriptor["artifacts"]
    if not isinstance(artifacts, list) or not artifacts:
        raise ReceiptError("export set descriptor has no artifact pairs")
    expected_artifacts_digest = hashlib.sha256(
        canonical_json_bytes(artifacts)
    ).hexdigest()
    if descriptor["artifacts_sha256"] != expected_artifacts_digest:
        raise ReceiptError("export set artifact descriptor digest does not match")

    expected_files = {EXPORT_DESCRIPTOR_NAME}
    expected_directories = {"artifacts"}
    seen_sources: set[str] = set()
    seen_exports: set[str] = set()
    validated: list[
        tuple[dict[str, object], PurePosixPath, dict[str, object], PurePosixPath]
    ] = []
    for index, pair in enumerate(artifacts):
        label = f"export artifact pair {index}"
        if not isinstance(pair, dict):
            raise ReceiptError(f"{label} is not an object")
        _require_exact_keys(pair, {"binary", "receipt"}, label)
        binary, binary_export = _validated_export_file_record(
            pair["binary"], f"{label} binary"
        )
        receipt, receipt_export = _validated_export_file_record(
            pair["receipt"], f"{label} receipt"
        )
        pair_directory = f"artifacts/{index:04d}"
        expected_binary_export = (
            f"{pair_directory}/{PurePosixPath(binary['source_path']).name}"
        )
        expected_receipt_export = (
            f"{pair_directory}/{PurePosixPath(receipt['source_path']).name}"
        )
        if binary_export.as_posix() != expected_binary_export:
            raise ReceiptError(f"{label} binary export path is not canonical")
        if receipt_export.as_posix() != expected_receipt_export:
            raise ReceiptError(f"{label} receipt export path is not canonical")
        for source in (binary["source_path"], receipt["source_path"]):
            if source in seen_sources:
                raise ReceiptError(f"duplicate source path in export set: {source}")
            seen_sources.add(source)
        for export in (binary_export.as_posix(), receipt_export.as_posix()):
            if export in seen_exports:
                raise ReceiptError(f"duplicate export path in export set: {export}")
            seen_exports.add(export)
            expected_files.add(export)
        expected_directories.add(pair_directory)
        validated.append((binary, binary_export, receipt, receipt_export))

    observed_files, observed_directories = _stage_tree(stage)
    if observed_files != expected_files or observed_directories != expected_directories:
        raise ReceiptError(
            "export stage tree does not exactly match its descriptor; "
            f"files={sorted(observed_files)}, directories={sorted(observed_directories)}"
        )

    for binary, binary_export, receipt, receipt_export in validated:
        binary_path = stage.joinpath(*binary_export.parts)
        receipt_path_value = stage.joinpath(*receipt_export.parts)
        binary_raw, binary_snapshot = read_regular_snapshot(
            binary_path, "exported binary"
        )
        receipt_raw, receipt_snapshot = read_regular_snapshot(
            receipt_path_value, "exported receipt"
        )
        if binary_snapshot != {
            "size": binary["size"],
            "sha256": binary["sha256"],
        }:
            raise ReceiptError(
                f"exported binary does not match descriptor: {binary_path}"
            )
        if receipt_snapshot != {
            "size": receipt["size"],
            "sha256": receipt["sha256"],
        }:
            raise ReceiptError(
                f"exported receipt does not match descriptor: {receipt_path_value}"
            )
        staged_receipt = parse_json_object(
            receipt_raw, receipt_path_value, "exported build receipt"
        )
        if receipt_raw != canonical_json_bytes(staged_receipt):
            raise ReceiptError(
                f"exported build receipt is not canonical: {receipt_path_value}"
            )
        validate_v4_receipt_shape(
            staged_receipt, f"exported build receipt {receipt_path_value}"
        )
        staged_binary = staged_receipt.get("binary")
        expected_binary = {
            "name": PurePosixPath(binary["source_path"]).name,
            "path": binary["source_path"],
            "size": len(binary_raw),
            "sha256": hashlib.sha256(binary_raw).hexdigest(),
        }
        if (
            staged_receipt.get("schema_version") != SCHEMA_VERSION
            or staged_receipt.get("claim") != RECEIPT_CLAIM_V4
            or staged_receipt.get("release_capsule") != descriptor_capsule
            or staged_binary != expected_binary
        ):
            raise ReceiptError(
                f"exported receipt does not bind exported binary: {receipt_path_value}"
            )
    return descriptor


def _validated_binary_query_name(value: str) -> str:
    allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._+-"
    if (
        not value
        or value in {".", ".."}
        or value[0] == "."
        or any(character not in allowed for character in value)
        or any(character in value for character in ("\x00", "\r", "\n"))
    ):
        raise ReceiptError(f"binary query name is unsafe: {value!r}")
    return value


def query_export_snapshot_path(
    stage: Path,
    *,
    binary_name: str | None,
    source_path: str | None,
    artifact: str,
    field: str = "path",
) -> str:
    descriptor = verify_export_snapshot_set(stage)
    if (binary_name is None) == (source_path is None):
        raise ReceiptError(
            "exactly one binary query selector is required: binary name or source path"
        )
    if artifact not in {"binary", "receipt"}:
        raise ReceiptError(f"unsupported export query artifact: {artifact!r}")
    if field not in {"path", "sha256", "path-sha256"}:
        raise ReceiptError(f"unsupported export query field: {field!r}")

    if binary_name is not None:
        selector = _validated_binary_query_name(binary_name)
        matches = [
            pair
            for pair in descriptor["artifacts"]
            if PurePosixPath(pair["binary"]["source_path"]).name == selector
        ]
        description = f"binary name {selector!r}"
    else:
        selector = _safe_relative_export_path(
            source_path, "binary query source path"
        ).as_posix()
        matches = [
            pair
            for pair in descriptor["artifacts"]
            if pair["binary"]["source_path"] == selector
        ]
        description = f"binary source path {selector!r}"

    if len(matches) != 1:
        raise ReceiptError(
            f"export path query for {description} matched {len(matches)} pairs; "
            "exactly one is required"
        )
    if field == "sha256":
        return matches[0][artifact]["sha256"]
    result = matches[0][artifact]["export_path"]
    safe_path = _safe_relative_export_path(result, "queried export path").as_posix()
    if field == "path-sha256":
        return f"{safe_path} {matches[0][artifact]['sha256']}"
    return safe_path


def _atomic_raw_write_new(path: Path, raw: bytes) -> None:
    path = lexical_absolute(path)
    assert_no_symlink_components(path.parent, "retained output directory")
    assert_no_symlink_components(path, "retained output", allow_missing_leaf=True)
    if path.exists() or path.is_symlink():
        raise ReceiptError(f"retained output already exists: {path}")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            # Publish with create-if-absent semantics. os.replace() would
            # overwrite a destination raced into place after the checks above.
            os.link(temporary_path, path, follow_symlinks=False)
        except FileExistsError as error:
            raise ReceiptError(
                f"retained output appeared during copy: {path}"
            ) from error
        except OSError as error:
            raise ReceiptError(
                f"cannot atomically publish retained output {path}: {error}"
            ) from error
        temporary_path.unlink()
        _fsync_directory(path.parent)
    finally:
        if temporary_path.exists():
            temporary_path.unlink()


def retain_export_snapshot_set(
    stage_value: Path, output_dir_value: Path, artifact_prefix: str
) -> dict[str, object]:
    """Retain every verified export pair under release-scoped flat names."""

    stage = assert_no_symlink_components(stage_value, "export stage")
    output_dir = assert_no_symlink_components(
        output_dir_value, "retained output directory"
    )
    if not output_dir.is_dir():
        raise ReceiptError(
            f"retained output directory is not a directory: {output_dir}"
        )
    prefix = _validated_binary_query_name(artifact_prefix)
    descriptor = verify_export_snapshot_set(stage)
    retained: list[dict[str, object]] = []
    destinations: list[Path] = []
    seen_names: set[str] = set()
    try:
        for pair in descriptor["artifacts"]:
            name = PurePosixPath(pair["binary"]["source_path"]).name
            if name in seen_names:
                raise ReceiptError(f"export set has ambiguous binary basename: {name}")
            seen_names.add(name)
            source_raw: dict[str, bytes] = {}
            source_snapshots: dict[str, dict[str, object]] = {}
            for role in ("binary", "receipt"):
                relative = _safe_relative_export_path(
                    pair[role]["export_path"], f"retained {role} export path"
                )
                raw, snapshot = read_regular_snapshot(
                    stage.joinpath(*relative.parts), f"retained export {role}"
                )
                expected = {
                    "size": pair[role]["size"],
                    "sha256": pair[role]["sha256"],
                }
                if snapshot != expected:
                    raise ReceiptError(
                        f"export {role} changed before retention: {relative}"
                    )
                source_raw[role] = raw
                source_snapshots[role] = snapshot
            binary_name = f"{prefix}.prebuilt-rust.{name}.bin"
            receipt_name = f"{prefix}.prebuilt-rust.{name}.build-receipt.json"
            binary_destination = output_dir / binary_name
            receipt_destination = output_dir / receipt_name
            _atomic_raw_write_new(binary_destination, source_raw["binary"])
            destinations.append(binary_destination)
            _atomic_raw_write_new(receipt_destination, source_raw["receipt"])
            destinations.append(receipt_destination)
            retained.append(
                {
                    "name": name,
                    "binary": {
                        "path": binary_name,
                        **source_snapshots["binary"],
                    },
                    "receipt": {
                        "path": receipt_name,
                        **source_snapshots["receipt"],
                    },
                }
            )
        retained.sort(key=lambda item: item["name"])
        return {"artifacts": retained}
    except Exception:
        for destination in reversed(destinations):
            try:
                destination.unlink()
            except FileNotFoundError:
                pass
        raise


def _verify_destruction_capability(
    stage: Path,
    capability_value: Path,
    descriptor: dict[str, object],
) -> Path:
    expected_path = export_capability_path(stage)
    capability = lexical_absolute(capability_value)
    if capability != expected_path:
        raise ReceiptError(
            "destruction capability path is not the out-of-stage capability bound "
            f"to this export set: expected {expected_path}, found {capability}"
        )
    raw, _ = read_regular_snapshot(capability, "destruction capability")
    status = os.lstat(capability)
    _check_owned_status(
        capability, status, "destruction capability", require_single_link=True
    )
    _check_private_mode(capability, 0o400, "destruction capability")
    value = parse_json_object(raw, capability, "destruction capability")
    if raw != canonical_json_bytes(value):
        raise ReceiptError(f"destruction capability is not canonical: {capability}")
    _require_exact_keys(
        value,
        {"schema_version", "stage_path", "descriptor_sha256", "token"},
        "destruction capability",
    )
    if (
        type(value["schema_version"]) is not int
        or value["schema_version"] != EXPORT_CAPABILITY_SCHEMA_VERSION
    ):
        raise ReceiptError("unsupported destruction capability schema version")
    if value["stage_path"] != str(stage):
        raise ReceiptError("destruction capability is bound to a different stage")
    expected_descriptor_digest = hashlib.sha256(
        canonical_json_bytes(descriptor)
    ).hexdigest()
    if value["descriptor_sha256"] != expected_descriptor_digest:
        raise ReceiptError("destruction capability descriptor binding does not match")
    token = value["token"]
    if (
        not isinstance(token, str)
        or len(token) != 64
        or any(character not in "0123456789abcdef" for character in token)
    ):
        raise ReceiptError("destruction capability token is malformed")
    return capability


def _strict_unlink_owned_file(path: Path, label: str) -> None:
    path = assert_no_symlink_components(path, label)
    status = os.lstat(path)
    if not stat.S_ISREG(status.st_mode):
        raise ReceiptError(f"{label} is not a regular file: {path}")
    _check_owned_status(path, status, label, require_single_link=True)
    if os.name == "posix" and os.unlink in os.supports_dir_fd:
        flags = os.O_RDONLY | os.O_DIRECTORY | getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        parent_descriptor = os.open(path.parent, flags)
        try:
            os.unlink(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        os.unlink(path)


def _strict_rmdir_owned(path: Path, label: str) -> None:
    path = assert_no_symlink_components(path, label)
    status = os.lstat(path)
    if not stat.S_ISDIR(status.st_mode):
        raise ReceiptError(f"{label} is not a directory: {path}")
    _check_owned_status(path, status, label)
    if os.name == "posix" and os.rmdir in os.supports_dir_fd:
        flags = os.O_RDONLY | os.O_DIRECTORY | getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        parent_descriptor = os.open(path.parent, flags)
        try:
            os.rmdir(path.name, dir_fd=parent_descriptor)
        finally:
            os.close(parent_descriptor)
    else:
        os.rmdir(path)


def destroy_export_snapshot_set(stage_value: Path, capability_value: Path) -> None:
    stage = lexical_absolute(stage_value)
    descriptor = verify_export_snapshot_set(stage)
    capability = _verify_destruction_capability(stage, capability_value, descriptor)
    artifacts = descriptor["artifacts"]
    artifact_paths: list[Path] = []
    pair_directories: list[Path] = []
    for pair in artifacts:
        pair_directory: Path | None = None
        for role in ("binary", "receipt"):
            relative = _safe_relative_export_path(
                pair[role]["export_path"], f"destroy {role} path"
            )
            artifact_paths.append(stage.joinpath(*relative.parts))
            pair_directory = stage.joinpath(*relative.parts[:-1])
        assert pair_directory is not None
        pair_directories.append(pair_directory)

    try:
        for path in artifact_paths:
            _strict_unlink_owned_file(path, "destroy export artifact")
        descriptor_path = stage / EXPORT_DESCRIPTOR_NAME
        _strict_unlink_owned_file(descriptor_path, "destroy export descriptor")
        for directory in reversed(pair_directories):
            _strict_rmdir_owned(directory, "destroy export pair directory")
        _strict_rmdir_owned(stage / "artifacts", "destroy export artifacts directory")
        _strict_rmdir_owned(stage, "destroy export stage")
        _strict_unlink_owned_file(capability, "destroy export capability")
        _fsync_directory(stage.parent)
    except OSError as error:
        raise ReceiptError(
            f"cannot destroy verified export snapshot set: {error}"
        ) from error


def atomic_json_write(path: Path, value: dict[str, object]) -> None:
    path = lexical_absolute(path)
    assert_no_symlink_components(path.parent, "receipt output directory")
    if not path.parent.is_dir():
        raise ReceiptError(
            f"receipt output directory is not a directory: {path.parent}"
        )
    assert_no_symlink_components(path, "receipt output", allow_missing_leaf=True)
    encoded = canonical_json_bytes(value)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary_path, path)
        if hasattr(os, "O_DIRECTORY"):
            directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    finally:
        if temporary_path.exists():
            temporary_path.unlink()


def create_receipts(args: argparse.Namespace) -> None:
    common = common_receipt_data(args)
    result_root = assert_no_symlink_components(
        args.result_root, "invocation result root"
    )
    for binary in args.binary:
        binary = lexical_absolute(binary)
        receipt = dict(common)
        receipt["binary"] = binary_data(binary, result_root)
        output = receipt_path(binary)
        atomic_json_write(output, receipt)
        print(f"wrote post-build snapshot receipt: {output}")


def load_receipt(path: Path) -> dict[str, object]:
    raw, _ = read_regular_snapshot(path, "build receipt")
    return parse_json_object(raw, path, "build receipt")


def validate_v4_receipt_shape(receipt: object, label: str) -> dict[str, object]:
    if not isinstance(receipt, dict) or set(receipt) != V4_RECEIPT_KEYS:
        raise ReceiptError(f"{label} does not have the exact schema-v4 receipt fields")
    if receipt.get("schema_version") != SCHEMA_VERSION:
        raise ReceiptError(f"{label} is not a schema-v4 build receipt")
    if receipt.get("claim") != RECEIPT_CLAIM_V4:
        raise ReceiptError(f"{label} has an invalid claim")
    try:
        release_capsule_lineage.validate_release_capsule(receipt["release_capsule"])
    except release_capsule_lineage.CapsuleLineageError as error:
        raise ReceiptError(
            f"{label} has an invalid release_capsule: {error}"
        ) from error
    validate_build_inputs_record(receipt["build_inputs"], f"{label} build_inputs")
    compile_environment = receipt.get("compile_environment")
    if not isinstance(compile_environment, dict):
        raise ReceiptError(f"{label} compile environment is invalid")
    entries = compile_environment.get("entries")
    if not isinstance(entries, dict):
        raise ReceiptError(f"{label} compile environment entries are invalid")
    legacy_entries = sorted(
        key for key in entries if key.startswith("DCENT_STOCK_FPGA_")
    )
    if legacy_entries:
        raise ReceiptError(
            f"{label} compile environment still carries removed stock FPGA authority: "
            + ", ".join(legacy_entries)
        )
    return receipt


def inspect_historical_receipt(path: Path, binary: Path | None) -> None:
    raw, _ = read_regular_snapshot(path, "historical build receipt")
    receipt = parse_json_object(raw, path, "historical build receipt")
    if raw != canonical_json_bytes(receipt):
        raise ReceiptError(f"historical build receipt is not canonical: {path}")
    schema = receipt.get("schema_version")
    if type(schema) is not int or schema not in {
        HISTORICAL_SCHEMA_VERSION,
        SCHEMA_VERSION,
    }:
        raise ReceiptError(f"unsupported historical build receipt schema: {schema!r}")
    if schema == SCHEMA_VERSION:
        validate_v4_receipt_shape(receipt, "historical build receipt")
    elif receipt.get("claim") != HISTORICAL_RECEIPT_CLAIM_V3:
        raise ReceiptError("historical schema-v3 build receipt has an invalid claim")
    if binary is not None:
        _, snapshot = read_regular_snapshot(binary, "historical receipt binary")
        evidence = receipt.get("binary")
        if (
            not isinstance(evidence, dict)
            or evidence.get("size") != snapshot["size"]
            or evidence.get("sha256") != snapshot["sha256"]
        ):
            raise ReceiptError(
                "historical build receipt does not match supplied binary bytes"
            )
    print(
        f"verified historical build receipt for inspection only: schema={schema}: {path}"
    )


def verify_receipts(args: argparse.Namespace) -> None:
    expected_common = common_receipt_data(args)
    result_root = assert_no_symlink_components(
        args.result_root, "invocation result root"
    )
    for binary in args.binary:
        binary = lexical_absolute(binary)
        path = receipt_path(binary)
        if not path.exists() and not path.is_symlink():
            raise ReceiptError(f"required build receipt is missing: {path}")
        observed = load_receipt(path)
        expected = dict(expected_common)
        expected["binary"] = binary_data(binary, result_root)
        if observed != expected:
            differing = sorted(
                key
                for key in set(observed) | set(expected)
                if observed.get(key) != expected.get(key)
            )
            raise ReceiptError(
                f"build receipt does not match current post-build snapshot: {path}; "
                f"differing sections: {', '.join(differing) or 'unknown'}"
            )
        print(f"verified post-build snapshot receipt: {path}")


def truthy(value: str) -> bool:
    return value.lower() in {"1", "true", "yes", "y"}


def check_override_policy(args: argparse.Namespace) -> None:
    if not truthy(args.allow_stale):
        return
    release_context = (
        truthy(args.release_provenance)
        or truthy(args.release_image)
        or args.package_status in RELEASE_STATUSES
    )
    if release_context:
        raise ReceiptError(
            "DCENT_ALLOW_STALE_DCENTRALD=1 is forbidden in release provenance/status/image mode"
        )
    print(
        "WARNING: DCENT_ALLOW_STALE_DCENTRALD=1 is a deprecated compatibility "
        "signal; it does not bypass snapshot/export validation or authorize "
        "release claims; remove it from callers",
        file=sys.stderr,
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subparsers = result.add_subparsers(dest="command", required=True)

    def add_snapshot_context(sub: argparse.ArgumentParser) -> None:
        sub.add_argument(
            "--git-object-repo",
            type=Path,
            required=True,
            help="trusted repository used only to authenticate exact Git objects",
        )
        sub.add_argument(
            "--source-snapshot",
            type=Path,
            required=True,
            help="source-snapshot.json in a verified immutable snapshot stage",
        )
        sub.add_argument(
            "--source-commit",
            required=True,
            help="exact full Git commit object id expected in the source snapshot",
        )
        sub.add_argument(
            "--source-workspace",
            required=True,
            help="canonical snapshot-tree-relative source workspace path",
        )
        sub.add_argument(
            "--release-invocation",
            type=Path,
            required=True,
            help="verified release invocation control stage",
        )
        sub.add_argument(
            "--result-root",
            type=Path,
            required=True,
            help="invocation-private root containing evidence and built binaries",
        )
        sub.add_argument(
            "--build-input-snapshot",
            type=Path,
            required=True,
            help="active cargo-workspace v2 external-input snapshot descriptor",
        )
        sub.add_argument("--target", required=True)
        sub.add_argument("--profile", required=True)
        sub.add_argument("--build-variant", required=True)
        sub.add_argument("--metadata", type=Path, required=True)
        sub.add_argument("--toolchain-context", type=Path, required=True)
        sub.add_argument("--compile-environment", type=Path, required=True)

    for command in ("create", "verify"):
        sub = subparsers.add_parser(command)
        add_snapshot_context(sub)
        sub.add_argument("--binary", action="append", type=Path, required=True)
    inspect = subparsers.add_parser(
        "inspect-receipt",
        help="verify canonical v3/v4 receipt evidence without admitting it to a new release",
    )
    inspect.add_argument("--receipt", type=Path, required=True)
    inspect.add_argument("--binary", type=Path)
    export = subparsers.add_parser(
        "export-snapshot-set",
        help="capture verified binary/receipt pairs in an atomic private stage",
    )
    add_snapshot_context(export)
    export.add_argument("--stage-parent", type=Path, required=True)
    export.add_argument(
        "--require-immutable-builder",
        action="store_true",
        help="reject receipts not bound to a digest-pinned Docker base and image ID",
    )
    export.add_argument(
        "--pair",
        action="append",
        nargs=2,
        type=Path,
        metavar=("BINARY", "RECEIPT"),
        required=True,
        help="exact binary and matching receipt pair; repeat for the full set",
    )
    verify_stage = subparsers.add_parser(
        "verify-export-snapshot-set", help="verify a detached private export stage"
    )
    verify_stage.add_argument(
        "--stage", type=Path, required=True, help="private stage path"
    )
    destroy_stage = subparsers.add_parser(
        "destroy-export-snapshot-set",
        help="destroy only a capability-bound, fully verified private export stage",
    )
    destroy_stage.add_argument(
        "--stage", type=Path, required=True, help="private stage path"
    )
    destroy_stage.add_argument(
        "--capability",
        type=Path,
        required=True,
        help="out-of-stage destruction capability returned by the path query",
    )
    capability_query = subparsers.add_parser(
        "export-snapshot-capability-path",
        help="print the deterministic out-of-stage capability path",
    )
    capability_query.add_argument(
        "--stage", type=Path, required=True, help="private stage path"
    )
    path_query = subparsers.add_parser(
        "query-export-snapshot-path",
        help="fully verify a stage and print one canonical relative artifact path",
    )
    path_query.add_argument(
        "--stage", type=Path, required=True, help="private stage path"
    )
    selector = path_query.add_mutually_exclusive_group(required=True)
    selector.add_argument(
        "--binary-name", help="exact safe binary basename; ambiguous names fail"
    )
    selector.add_argument(
        "--source-path", help="exact canonical repository-relative binary source path"
    )
    path_query.add_argument(
        "--artifact",
        choices=("binary", "receipt"),
        default="binary",
        help="paired exported path to print (default: binary)",
    )
    path_query.add_argument(
        "--field",
        choices=("path", "sha256", "path-sha256"),
        default="path",
        help="verified value to print (default: canonical relative path)",
    )
    retain_stage = subparsers.add_parser(
        "retain-export-snapshot-set",
        help="copy every verified export pair to release-scoped flat sidecars",
    )
    retain_stage.add_argument("--stage", type=Path, required=True)
    retain_stage.add_argument("--output-dir", type=Path, required=True)
    retain_stage.add_argument(
        "--artifact-prefix",
        required=True,
        help="safe release artifact basename used to scope retained sidecars",
    )
    policy = subparsers.add_parser("check-override-policy")
    policy.add_argument("--allow-stale", default="0")
    policy.add_argument("--release-provenance", default="0")
    policy.add_argument("--release-image", default="0")
    policy.add_argument("--package-status", default="")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "create":
            create_receipts(args)
        elif args.command == "verify":
            verify_receipts(args)
        elif args.command == "inspect-receipt":
            inspect_historical_receipt(args.receipt, args.binary)
        elif args.command == "export-snapshot-set":
            print(export_snapshot_set(args))
        elif args.command == "verify-export-snapshot-set":
            verify_export_snapshot_set(args.stage)
            print(lexical_absolute(args.stage))
        elif args.command == "destroy-export-snapshot-set":
            destroy_export_snapshot_set(args.stage, args.capability)
        elif args.command == "export-snapshot-capability-path":
            print(export_capability_path(args.stage))
        elif args.command == "query-export-snapshot-path":
            print(
                query_export_snapshot_path(
                    args.stage,
                    binary_name=args.binary_name,
                    source_path=args.source_path,
                    artifact=args.artifact,
                    field=args.field,
                )
            )
        elif args.command == "retain-export-snapshot-set":
            print(
                canonical_json_bytes(
                    retain_export_snapshot_set(
                        args.stage, args.output_dir, args.artifact_prefix
                    )
                ).decode("utf-8"),
                end="",
            )
        else:
            check_override_policy(args)
    except ReceiptError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
