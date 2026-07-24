#!/usr/bin/env python3
"""Audit an explicit catalog of offline boot artifacts without executing them.

The report proves exact bytes and narrowly defined lexical observations only.  It
does not discover artifacts, infer hardware identity from paths, or qualify a
boot sequence or electrical state as safe.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple
import zlib


CATALOG_SCHEMA = "dcentos.boot-artifact-catalog.v1"
REPORT_SCHEMA = "dcentos.boot-artifact-report.v1"
MAX_CATALOG_BYTES = 1024 * 1024
MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_ARTIFACTS = 1024

COVERAGE = "declared_inputs_only"
CLAIM = (
    "This report covers only the declared catalog entries listed below. It is "
    "not an exhaustive inventory of the workspace, firmware archive, or a "
    "miner's boot chain."
)
NON_CLAIMS = [
    (
        "Artifact integrity SHA-256 values identify the bytes read during this "
        "run. They do not "
        "authenticate their origin, freshness, active-copy status, acquisition "
        "process, or product association. CRC consistency establishes only "
        "consistency under the explicitly declared layout."
    ),
    (
        "Commands are lexical observations from captured bytes. This report "
        "does not prove command execution, the active U-Boot adapter, Linux "
        "adapter equivalence, wire transaction framing, device acknowledgement, "
        "GPIO polarity, APW output state, pad voltage, rail state, fan behavior, "
        "boot safety, or applicability to another product."
    ),
    (
        "Equal artifact hashes establish byte equality only. Unequal hashes do "
        "not establish independent acquisition, origin, provenance, or "
        "corroboration."
    ),
]

ROOT_NAMES = {"project", "workspace"}
PRESENCE_POLICIES = {"required", "local_optional"}
KINDS = {
    "context_record",
    "opaque_file",
    "script_text",
    "uboot_env_crc32_le",
    "uboot_env_text_export",
}
BOOT_PHASES = {"context", "pre_linux", "early_userspace"}
PROVENANCE_GRADES = {
    "repository_tracked",
    "operator_associated_unsealed",
    "derived_unsealed",
}
ASSOCIATIONS = {
    "tracked_source_current",
    "same_capture_directory_unsealed",
    "path_label_only_unsealed",
    "derived_transcription_unsealed",
}

HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
ID_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,127}$")
ENV_NAME_RE = re.compile(rb"^[A-Za-z0-9_.-]{1,128}$")
RUN_RE = re.compile(
    rb"^run ([A-Za-z0-9_.-]{1,128}(?: [A-Za-z0-9_.-]{1,128}){0,63})$"
)
I2C_MW_RE = re.compile(
    rb"^i2c mw [0-9A-Fa-f]{1,8} "
    rb"[0-9A-Fa-f]{1,8}(?:\.[0-9A-Fa-f]{1,8})? "
    rb"[0-9A-Fa-f]{1,8} [0-9A-Fa-f]{1,8}$"
)
I2C_DEV_RE = re.compile(
    rb"(?<![A-Za-z0-9_.-])i2c\s+dev(?![A-Za-z0-9_.-])"
)
PATH_PART_RE = re.compile(r"^[A-Za-z0-9._+-]{1,255}$")
WINDOWS_RESERVED_BASENAMES = {
    "AUX",
    "CON",
    "NUL",
    "PRN",
    *(f"COM{number}" for number in range(1, 10)),
    *(f"LPT{number}" for number in range(1, 10)),
}


class CatalogError(Exception):
    """The catalog or CLI trust boundary is invalid."""


class ArtifactError(Exception):
    """An artifact could not be safely verified or decoded."""


def canonical_json_bytes(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def reject_duplicate_keys(pairs: Sequence[Tuple[str, object]]) -> Dict[str, object]:
    result: Dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise CatalogError("catalog contains a duplicate JSON key")
        result[key] = value
    return result


def is_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse_flag)


def is_link_or_reparse(metadata: os.stat_result) -> bool:
    return stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata)


def exact_keys(value: object, expected: Iterable[str], label: str) -> Dict[str, Any]:
    if not isinstance(value, dict):
        raise CatalogError(f"{label} must be an object")
    expected_set = set(expected)
    if set(value) != expected_set:
        raise CatalogError(f"{label} has an invalid exact schema")
    return value


def require_string(value: object, label: str, allow_null: bool = False) -> Optional[str]:
    if allow_null and value is None:
        return None
    if not isinstance(value, str) or not value or len(value) > 512:
        raise CatalogError(f"{label} must be a bounded non-empty string")
    return value


def require_int(value: object, label: str, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise CatalogError(f"{label} must be an integer >= {minimum}")
    return value


def require_bool(value: object, label: str) -> bool:
    if not isinstance(value, bool):
        raise CatalogError(f"{label} must be a boolean")
    return value


def require_hash(value: object, label: str) -> str:
    if not isinstance(value, str) or not HEX64_RE.fullmatch(value):
        raise CatalogError(f"{label} must be a lowercase SHA-256 digest")
    return value


def require_sorted_hashes(
    value: object, label: str, require_unique: bool = False
) -> List[str]:
    if not isinstance(value, list) or len(value) > 1024:
        raise CatalogError(f"{label} must be a bounded list")
    result = [require_hash(item, label) for item in value]
    if result != sorted(result) or (require_unique and len(result) != len(set(result))):
        qualifier = "sorted unique" if require_unique else "sorted"
        raise CatalogError(f"{label} must contain {qualifier} digests")
    return result


def validate_relative_path(value: object, label: str) -> str:
    path = require_string(value, label)
    assert path is not None
    if len(path) > 1024 or "\\" in path or ":" in path or "\x00" in path:
        raise CatalogError(f"{label} is not a portable relative POSIX path")
    candidate = PurePosixPath(path)
    parts = candidate.parts
    reserved_component = any(
        part.endswith(".")
        or part.split(".", 1)[0].upper() in WINDOWS_RESERVED_BASENAMES
        for part in parts
    )
    if (
        candidate.is_absolute()
        or not parts
        or any(part in ("", ".", "..") for part in parts)
        or any(not PATH_PART_RE.fullmatch(part) for part in parts)
        or reserved_component
        or path.startswith("//")
        or str(candidate) != path
    ):
        raise CatalogError(f"{label} is not a portable relative POSIX path")
    return path


def validate_safe_i2c_commands(value: object, label: str) -> List[str]:
    if not isinstance(value, list) or len(value) > 64:
        raise CatalogError(f"{label} must be a bounded list")
    commands: List[str] = []
    for item in value:
        command = require_string(item, label)
        assert command is not None
        try:
            encoded = command.encode("ascii")
        except UnicodeEncodeError as error:
            raise CatalogError(f"{label} contains a non-ASCII command") from error
        if not I2C_MW_RE.fullmatch(encoded):
            raise CatalogError(f"{label} contains a non-allowlisted command")
        commands.append(command)
    if commands != sorted(commands):
        raise CatalogError(f"{label} must contain sorted commands")
    return commands


def validate_expected_findings(value: object, kind: str) -> Optional[Dict[str, Any]]:
    if kind not in {"uboot_env_crc32_le", "uboot_env_text_export"}:
        if value is not None:
            raise CatalogError("non-environment expected_findings must be null")
        return None
    fields = {
        "crc_consistent",
        "record_shape_status",
        "record_count",
        "assignment_count",
        "assignment_projection_sha256",
        "non_assignment_record_sha256",
        "preboot_present",
        "preboot_value_length",
        "preboot_value_sha256",
        "preboot_segment_count",
        "direct_run_reference_count",
        "direct_run_target_name_present_count",
        "unresolved_run_target_sha256",
        "literal_i2c_commands",
        "captured_table_literal_i2c_dev_count",
        "command_graph_complete",
        "uboot_adapter_identity",
        "linux_adapter_equivalence",
    }
    expected = exact_keys(value, fields, "expected_findings")
    if kind == "uboot_env_crc32_le":
        if not require_bool(expected["crc_consistent"], "crc_consistent"):
            raise CatalogError("raw environment CRC consistency must be required")
    elif expected["crc_consistent"] is not None:
        raise CatalogError("text-export crc_consistent must be null")
    if kind == "uboot_env_crc32_le" and expected["record_shape_status"] not in {
        "non_assignment_records_present",
        "assignments_only",
    }:
        raise CatalogError("raw environment record_shape_status is invalid")
    if (
        kind == "uboot_env_text_export"
        and expected["record_shape_status"]
        not in {"non_assignment_lines_present", "assignment_lines_only"}
    ):
        raise CatalogError("text export record_shape_status is invalid")
    for field in (
        "record_count",
        "assignment_count",
        "preboot_value_length",
        "preboot_segment_count",
        "direct_run_reference_count",
        "direct_run_target_name_present_count",
        "captured_table_literal_i2c_dev_count",
    ):
        require_int(expected[field], field)
    require_hash(expected["assignment_projection_sha256"], "assignment projection")
    require_sorted_hashes(
        expected["non_assignment_record_sha256"], "non-assignment digests"
    )
    if not require_bool(expected["preboot_present"], "preboot_present"):
        raise CatalogError("the current decoder requires a unique preboot value")
    require_hash(expected["preboot_value_sha256"], "preboot value")
    require_sorted_hashes(
        expected["unresolved_run_target_sha256"],
        "unresolved run-target digests",
        require_unique=True,
    )
    validate_safe_i2c_commands(expected["literal_i2c_commands"], "literal I2C commands")
    require_bool(expected["command_graph_complete"], "command_graph_complete")
    if expected["command_graph_complete"]:
        raise CatalogError("the lexical decoder cannot claim a complete command graph")
    if expected["uboot_adapter_identity"] != "unknown":
        raise CatalogError("uboot_adapter_identity must remain unknown")
    if expected["linux_adapter_equivalence"] != "not_inferred":
        raise CatalogError("linux_adapter_equivalence must remain not_inferred")
    if expected["assignment_count"] > expected["record_count"]:
        raise CatalogError("assignment_count cannot exceed record_count")
    if (
        expected["direct_run_target_name_present_count"]
        > expected["direct_run_reference_count"]
    ):
        raise CatalogError("present direct-run target count cannot exceed total direct-run count")
    return expected


def validate_format_profile(value: object, kind: str) -> Optional[Dict[str, Any]]:
    if kind == "uboot_env_crc32_le":
        profile = exact_keys(
            value,
            {
                "header_bytes",
                "crc32_byte_order",
                "redundancy_flag_bytes",
                "terminator",
                "padding_byte",
            },
            "raw environment format_profile",
        )
        if (
            require_int(profile["header_bytes"], "header_bytes", 1) != 4
            or profile["crc32_byte_order"] != "little"
            or require_int(profile["redundancy_flag_bytes"], "redundancy_flag_bytes") != 0
            or profile["terminator"] != "double_nul"
            or require_int(profile["padding_byte"], "padding_byte") != 0
        ):
            raise CatalogError("unsupported raw environment format_profile")
        return profile
    if kind == "uboot_env_text_export":
        profile = exact_keys(
            value,
            {"encoding", "line_ending", "terminal_line_ending"},
            "text environment format_profile",
        )
        if profile != {
            "encoding": "ascii",
            "line_ending": "lf",
            "terminal_line_ending": "required",
        }:
            raise CatalogError("unsupported text environment format_profile")
        return profile
    if value is not None:
        raise CatalogError("non-environment format_profile must be null")
    return None


def validate_artifact(value: object) -> Dict[str, Any]:
    artifact = exact_keys(
        value,
        {
            "id",
            "location",
            "presence_policy",
            "kind",
            "declared_boot_phase",
            "integrity",
            "declared_context",
            "format_profile",
            "expected_findings",
        },
        "artifact",
    )
    artifact_id = require_string(artifact["id"], "artifact id")
    assert artifact_id is not None
    if not ID_RE.fullmatch(artifact_id):
        raise CatalogError("artifact id is not portable lowercase kebab-case")
    location = exact_keys(artifact["location"], {"root", "path"}, "artifact location")
    if location["root"] not in ROOT_NAMES:
        raise CatalogError("artifact location root is invalid")
    validate_relative_path(location["path"], "artifact path")
    if artifact["presence_policy"] not in PRESENCE_POLICIES:
        raise CatalogError("artifact presence_policy is invalid")
    kind = artifact["kind"]
    if kind not in KINDS:
        raise CatalogError("artifact kind is invalid")
    if artifact["declared_boot_phase"] not in BOOT_PHASES:
        raise CatalogError("artifact declared_boot_phase is invalid")
    integrity = exact_keys(artifact["integrity"], {"size", "sha256"}, "integrity")
    size = require_int(integrity["size"], "artifact size", 1)
    if size > MAX_ARTIFACT_BYTES:
        raise CatalogError("artifact size exceeds the offline bound")
    require_hash(integrity["sha256"], "artifact digest")
    context = exact_keys(
        artifact["declared_context"],
        {
            "product_declaration",
            "firmware_declaration",
            "capture_id",
            "provenance_grade",
            "association",
        },
        "declared_context",
    )
    for field in ("product_declaration", "firmware_declaration", "capture_id"):
        require_string(context[field], field, allow_null=True)
    if context["provenance_grade"] not in PROVENANCE_GRADES:
        raise CatalogError("provenance_grade is invalid")
    if context["association"] not in ASSOCIATIONS:
        raise CatalogError("association is invalid")
    presence_policy = artifact["presence_policy"]
    grade = context["provenance_grade"]
    association = context["association"]
    capture_id = context["capture_id"]
    if association == "tracked_source_current":
        if grade != "repository_tracked" or presence_policy != "required":
            raise CatalogError("tracked-source provenance fields are inconsistent")
    elif association == "same_capture_directory_unsealed":
        if (
            grade != "operator_associated_unsealed"
            or presence_policy != "local_optional"
            or capture_id is None
        ):
            raise CatalogError("same-capture provenance fields are inconsistent")
    elif (
        grade != "derived_unsealed"
        or presence_policy != "local_optional"
    ):
        raise CatalogError("derived provenance fields are inconsistent")
    expected_phase = {
        "context_record": "context",
        "opaque_file": "pre_linux",
        "script_text": "early_userspace",
        "uboot_env_crc32_le": "pre_linux",
        "uboot_env_text_export": "pre_linux",
    }[kind]
    if artifact["declared_boot_phase"] != expected_phase:
        raise CatalogError("artifact kind and declared_boot_phase are inconsistent")
    validate_format_profile(artifact["format_profile"], kind)
    validate_expected_findings(artifact["expected_findings"], kind)
    return artifact


def normalize_catalog(catalog: Dict[str, Any]) -> Dict[str, Any]:
    normalized = dict(catalog)
    normalized["artifacts"] = sorted(catalog["artifacts"], key=lambda item: item["id"])
    return normalized


def inspect_catalog_parent_chain(path: Path) -> Tuple[Path, Tuple[Tuple[int, int, int], ...]]:
    absolute = path.absolute()
    identities: List[Tuple[int, int, int]] = []
    for parent in reversed(absolute.parents):
        try:
            metadata = os.lstat(parent)
        except OSError as error:
            raise CatalogError("catalog parent cannot be inspected") from error
        if not stat.S_ISDIR(metadata.st_mode) or is_link_or_reparse(metadata):
            raise CatalogError("catalog path contains a link or reparse parent")
        identities.append((metadata.st_dev, metadata.st_ino, metadata.st_mode))
    return absolute, tuple(identities)


def close_catalog_descriptor(descriptor: int) -> None:
    try:
        os.close(descriptor)
    except OSError as error:
        raise CatalogError("catalog descriptor could not be closed") from error


def read_catalog(path: Path) -> Tuple[Dict[str, Any], str]:
    absolute, parent_identities = inspect_catalog_parent_chain(path)
    try:
        metadata = os.lstat(absolute)
    except OSError as error:
        raise CatalogError("catalog cannot be inspected") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or is_link_or_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
        or metadata.st_size > MAX_CATALOG_BYTES
    ):
        raise CatalogError("catalog is not a bounded single-link regular file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor: Optional[int] = None
    try:
        descriptor = os.open(absolute, flags)
        opened = os.fstat(descriptor)
        if metadata_identity(opened) != metadata_identity(metadata):
            raise CatalogError("catalog changed before reading")
        chunks: List[bytes] = []
        total = 0
        while True:
            chunk = os.read(
                descriptor, min(64 * 1024, MAX_CATALOG_BYTES + 1 - total)
            )
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_CATALOG_BYTES:
                raise CatalogError("catalog exceeds the one-MiB read bound")
        final_open = os.fstat(descriptor)
    except CatalogError:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise
    except OSError as error:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise CatalogError("catalog descriptor read failed") from error
    assert descriptor is not None
    close_catalog_descriptor(descriptor)
    raw = b"".join(chunks)
    try:
        final_path = os.lstat(absolute)
    except OSError as error:
        raise CatalogError("catalog changed while it was read") from error
    _, final_parent_identities = inspect_catalog_parent_chain(absolute)
    if (
        len(raw) != metadata.st_size
        or metadata_identity(final_open) != metadata_identity(metadata)
        or metadata_identity(final_path) != metadata_identity(metadata)
        or final_parent_identities != parent_identities
    ):
        raise CatalogError("catalog changed while it was read")
    try:
        if raw.startswith(b"\xef\xbb\xbf"):
            raise CatalogError("catalog UTF-8 BOM is forbidden")
        text = raw.decode("utf-8")
        value = json.loads(text, object_pairs_hook=reject_duplicate_keys)
    except CatalogError:
        raise
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CatalogError("catalog is not valid UTF-8 JSON") from error
    catalog = exact_keys(value, {"schema", "coverage", "claim", "non_claims", "artifacts"}, "catalog")
    if catalog["schema"] != CATALOG_SCHEMA:
        raise CatalogError("catalog schema is unsupported")
    if catalog["coverage"] != COVERAGE or catalog["claim"] != CLAIM:
        raise CatalogError("catalog evidence boundary does not match the supported contract")
    if catalog["non_claims"] != NON_CLAIMS:
        raise CatalogError("catalog non-claims do not match the supported contract")
    artifacts = catalog["artifacts"]
    if not isinstance(artifacts, list) or not artifacts or len(artifacts) > MAX_ARTIFACTS:
        raise CatalogError("catalog artifacts must contain 1..1024 entries")
    validated = [validate_artifact(item) for item in artifacts]
    ids = [str(item["id"]).casefold() for item in validated]
    locations = [
        (str(item["location"]["root"]).casefold(), str(item["location"]["path"]).casefold())
        for item in validated
    ]
    if len(ids) != len(set(ids)):
        raise CatalogError("artifact ids collide across case-insensitive hosts")
    if len(locations) != len(set(locations)):
        raise CatalogError("artifact paths collide across case-insensitive hosts")
    catalog["artifacts"] = validated
    normalized = normalize_catalog(catalog)
    semantic_sha256 = sha256_bytes(canonical_json_bytes(normalized))
    return normalized, semantic_sha256


def validate_root(path: Path, label: str) -> Path:
    absolute = path.absolute()
    try:
        metadata = os.lstat(absolute)
    except OSError as error:
        raise CatalogError(f"{label} cannot be inspected") from error
    if not stat.S_ISDIR(metadata.st_mode) or is_link_or_reparse(metadata):
        raise CatalogError(f"{label} is not a non-reparse directory")
    return absolute


def inspect_artifact_components(root: Path, relative: str) -> Path:
    try:
        root_metadata = os.lstat(root)
    except OSError as error:
        raise ArtifactError("artifact root cannot be safely inspected") from error
    if not stat.S_ISDIR(root_metadata.st_mode) or is_link_or_reparse(root_metadata):
        raise ArtifactError("artifact root is not a non-reparse directory")
    candidate = root
    parts = PurePosixPath(relative).parts
    for index, part in enumerate(parts):
        candidate = candidate / part
        try:
            metadata = os.lstat(candidate)
        except FileNotFoundError:
            if index == len(parts) - 1:
                return candidate
            # An absent parent still denotes an absent optional artifact.  No
            # descendant can exist beneath it, so stop without discovery.
            return root.joinpath(*parts)
        except OSError as error:
            raise ArtifactError("artifact path cannot be safely inspected") from error
        if is_link_or_reparse(metadata):
            raise ArtifactError("artifact path contains a link or reparse point")
        if index < len(parts) - 1 and not stat.S_ISDIR(metadata.st_mode):
            raise ArtifactError("artifact path parent is not a directory")
    return candidate


def metadata_identity(metadata: os.stat_result) -> Tuple[int, int, int, int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
        getattr(metadata, "st_mtime_ns", int(metadata.st_mtime * 1_000_000_000)),
        getattr(metadata, "st_ctime_ns", int(metadata.st_ctime * 1_000_000_000)),
        getattr(metadata, "st_nlink", 1),
    )


def read_artifact_once(root: Path, relative: str) -> Optional[bytes]:
    try:
        root_initial = os.lstat(root)
    except OSError as error:
        raise ArtifactError("artifact root cannot be inspected") from error
    path = inspect_artifact_components(root, relative)
    try:
        initial = os.lstat(path)
    except FileNotFoundError:
        try:
            root_final = os.lstat(root)
        except OSError as error:
            raise ArtifactError("artifact root changed during absence check") from error
        if metadata_identity(root_final) != metadata_identity(root_initial):
            raise ArtifactError("artifact root changed during absence check")
        return None
    except OSError as error:
        raise ArtifactError("artifact cannot be inspected") from error
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_link_or_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
        or initial.st_size > MAX_ARTIFACT_BYTES
    ):
        raise ArtifactError("artifact is not a bounded single-link regular file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ArtifactError("artifact cannot be opened safely") from error
    try:
        opened = os.fstat(descriptor)
        if metadata_identity(opened) != metadata_identity(initial):
            raise ArtifactError("artifact changed before reading")
        chunks: List[bytes] = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, MAX_ARTIFACT_BYTES + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_ARTIFACT_BYTES:
                raise ArtifactError("artifact exceeds the offline read bound")
        final_open = os.fstat(descriptor)
    except ArtifactError:
        try:
            os.close(descriptor)
        except OSError:
            pass
        raise
    except OSError as error:
        try:
            os.close(descriptor)
        except OSError:
            pass
        raise ArtifactError("artifact descriptor read failed") from error
    try:
        os.close(descriptor)
    except OSError as error:
        raise ArtifactError("artifact descriptor could not be closed") from error
    try:
        final_path = os.lstat(path)
    except OSError as error:
        raise ArtifactError("artifact disappeared while it was read") from error
    if (
        metadata_identity(initial) != metadata_identity(final_open)
        or metadata_identity(initial) != metadata_identity(final_path)
    ):
        raise ArtifactError("artifact changed while it was read")
    confirmed_path = inspect_artifact_components(root, relative)
    try:
        confirmed = os.lstat(confirmed_path)
        root_final = os.lstat(root)
    except OSError as error:
        raise ArtifactError("artifact path changed after reading") from error
    if (
        metadata_identity(confirmed) != metadata_identity(initial)
        or metadata_identity(root_final) != metadata_identity(root_initial)
    ):
        raise ArtifactError("artifact path changed after reading")
    content = b"".join(chunks)
    if len(content) != initial.st_size:
        raise ArtifactError("artifact read length changed")
    return content


def assignment_projection(entries: Sequence[Tuple[bytes, bytes]]) -> str:
    projection = [
        {
            "name_sha256": sha256_bytes(name),
            "value_length": len(value),
            "value_sha256": sha256_bytes(value),
        }
        for name, value in entries
    ]
    projection.sort(
        key=lambda item: (
            item["name_sha256"], item["value_sha256"], item["value_length"]
        )
    )
    return sha256_bytes(canonical_json_bytes(projection)[:-1])


def parse_records(records: Sequence[bytes]) -> Tuple[List[Tuple[bytes, bytes]], List[str]]:
    entries: List[Tuple[bytes, bytes]] = []
    non_assignments: List[str] = []
    names = set()
    for record in records:
        if b"=" not in record:
            non_assignments.append(sha256_bytes(record))
            continue
        name, value = record.split(b"=", 1)
        if not ENV_NAME_RE.fullmatch(name):
            raise ArtifactError("environment contains an invalid variable name")
        if name in names:
            raise ArtifactError("environment contains a duplicate variable name")
        names.add(name)
        entries.append((name, value))
    return entries, sorted(non_assignments)


def lexical_findings(
    records: Sequence[bytes],
    entries: Sequence[Tuple[bytes, bytes]],
    crc: Optional[bool],
    record_shape_status: str,
) -> Dict[str, Any]:
    values = {name: value for name, value in entries}
    preboot = values.get(b"preboot")
    if preboot is None:
        raise ArtifactError("environment does not contain a unique preboot value")
    try:
        preboot.decode("ascii")
    except UnicodeDecodeError as error:
        raise ArtifactError("preboot is not strict ASCII") from error
    segments = [segment.strip() for segment in preboot.split(b";") if segment.strip()]
    run_targets: List[bytes] = []
    literal_i2c_commands: List[str] = []
    for segment in segments:
        run_match = RUN_RE.fullmatch(segment)
        if run_match:
            run_targets.extend(run_match.group(1).split(b" "))
        if I2C_MW_RE.fullmatch(segment):
            literal_i2c_commands.append(segment.decode("ascii"))
    unresolved = sorted(
        {sha256_bytes(target) for target in run_targets if target not in values}
    )
    i2c_dev_count = sum(len(I2C_DEV_RE.findall(value)) for value in values.values())
    _, non_assignments = parse_records(records)
    return {
        "crc_consistent": crc,
        "record_shape_status": record_shape_status,
        "record_count": len(records),
        "assignment_count": len(entries),
        "assignment_projection_sha256": assignment_projection(entries),
        "non_assignment_record_sha256": non_assignments,
        "preboot_present": True,
        "preboot_value_length": len(preboot),
        "preboot_value_sha256": sha256_bytes(preboot),
        "preboot_segment_count": len(segments),
        "direct_run_reference_count": len(run_targets),
        "direct_run_target_name_present_count": sum(
            target in values for target in run_targets
        ),
        "unresolved_run_target_sha256": unresolved,
        "literal_i2c_commands": sorted(literal_i2c_commands),
        "captured_table_literal_i2c_dev_count": i2c_dev_count,
        # A lexical scan never proves nested hush execution, compiled defaults,
        # variable expansion, conditionals, or dynamic setenv effects.
        "command_graph_complete": False,
        "uboot_adapter_identity": "unknown",
        "linux_adapter_equivalence": "not_inferred",
    }


def decode_raw_environment(content: bytes) -> Dict[str, Any]:
    if len(content) < 6:
        raise ArtifactError("raw environment is too short for its declared layout")
    stored_crc = int.from_bytes(content[:4], "little")
    payload = content[4:]
    crc_consistent = stored_crc == (zlib.crc32(payload) & 0xFFFFFFFF)
    if not crc_consistent:
        raise ArtifactError("raw environment CRC is inconsistent with its declared layout")
    try:
        terminator = payload.index(b"\x00\x00")
    except ValueError as error:
        raise ArtifactError("raw environment lacks its declared double-NUL terminator") from error
    padding = payload[terminator + 2 :]
    if any(padding):
        raise ArtifactError("raw environment has nonzero bytes after its terminator")
    records = payload[:terminator].split(b"\x00")
    entries, non_assignments = parse_records(records)
    record_shape_status = (
        "non_assignment_records_present" if non_assignments else "assignments_only"
    )
    return lexical_findings(records, entries, crc_consistent, record_shape_status)


def decode_text_environment(content: bytes) -> Dict[str, Any]:
    if not content.endswith(b"\n") or b"\r" in content or b"\x00" in content:
        raise ArtifactError("text environment violates its declared LF/ASCII framing")
    try:
        content.decode("ascii")
    except UnicodeDecodeError as error:
        raise ArtifactError("text environment is not strict ASCII") from error
    records = content[:-1].split(b"\n")
    entries, non_assignments = parse_records(records)
    record_shape_status = (
        "non_assignment_lines_present" if non_assignments else "assignment_lines_only"
    )
    return lexical_findings(records, entries, None, record_shape_status)


def context_digest(artifact: Dict[str, Any]) -> str:
    return sha256_bytes(canonical_json_bytes(artifact["declared_context"])[:-1])


def audit_artifact(artifact: Dict[str, Any], roots: Dict[str, Path]) -> Dict[str, Any]:
    base = {
        "id": artifact["id"],
        "root": artifact["location"]["root"],
        "path": artifact["location"]["path"],
        "presence_policy": artifact["presence_policy"],
        "representation": artifact["kind"],
        "declared_context_sha256": context_digest(artifact),
    }
    try:
        content = read_artifact_once(
            roots[artifact["location"]["root"]], artifact["location"]["path"]
        )
    except ArtifactError:
        return {**base, "status": "failed", "failure_reasons": ["unsafe_or_unreadable"]}
    if content is None:
        status = (
            "missing_required"
            if artifact["presence_policy"] == "required"
            else "missing_local_optional"
        )
        return {**base, "status": status}
    actual_size = len(content)
    actual_sha256 = sha256_bytes(content)
    integrity = {"size": actual_size, "sha256": actual_sha256}
    reasons: List[str] = []
    if actual_size != artifact["integrity"]["size"]:
        reasons.append("size_mismatch")
    if actual_sha256 != artifact["integrity"]["sha256"]:
        reasons.append("sha256_mismatch")
    if reasons:
        return {
            **base,
            "status": "failed",
            "integrity": integrity,
            "failure_reasons": reasons,
        }
    kind = artifact["kind"]
    if kind in {"uboot_env_crc32_le", "uboot_env_text_export"}:
        try:
            if kind == "uboot_env_crc32_le":
                findings = decode_raw_environment(content)
            else:
                findings = decode_text_environment(content)
        except ArtifactError:
            return {
                **base,
                "status": "failed",
                "integrity": integrity,
                "failure_reasons": ["decoder_rejected"],
            }
        if findings != artifact["expected_findings"]:
            mismatches = sorted(
                key
                for key in findings
                if findings.get(key) != artifact["expected_findings"].get(key)
            )
            return {
                **base,
                "status": "failed",
                "integrity": integrity,
                "findings": findings,
                "failure_reasons": ["expected_findings_mismatch"],
                "finding_mismatch_fields": mismatches,
            }
        record_shape_status = findings["record_shape_status"]
        if record_shape_status == "non_assignment_records_present":
            status = "verified_exact_bytes_with_non_assignment_records"
        else:
            status = "verified_exact_bytes_and_lexical_findings"
        return {**base, "status": status, "integrity": integrity, "findings": findings}
    if kind == "opaque_file":
        status = "verified_exact_opaque_bytes"
    else:
        status = "verified_exact_bytes"
    return {**base, "status": status, "integrity": integrity}


def build_exact_byte_groups(results: Sequence[Dict[str, Any]]) -> List[Dict[str, Any]]:
    groups: Dict[str, List[str]] = {}
    for result in results:
        integrity = result.get("integrity")
        if isinstance(integrity, dict) and result["status"].startswith("verified"):
            groups.setdefault(str(integrity["sha256"]), []).append(str(result["id"]))
    return [
        {
            "sha256": digest,
            "artifact_count": len(sorted(ids)),
            "artifact_ids": sorted(ids),
        }
        for digest, ids in sorted(groups.items())
    ]


def audit_catalog(
    catalog_path: Path,
    project_root: Path,
    workspace_root: Path,
    require_local_evidence: bool = False,
) -> Tuple[Dict[str, Any], int]:
    catalog, catalog_sha256 = read_catalog(catalog_path)
    roots = {
        "project": validate_root(project_root, "project root"),
        "workspace": validate_root(workspace_root, "workspace root"),
    }
    results = [audit_artifact(artifact, roots) for artifact in catalog["artifacts"]]
    results.sort(key=lambda item: item["id"])
    statuses = [result["status"] for result in results]
    failed = sum(status == "failed" for status in statuses)
    missing_required = sum(status == "missing_required" for status in statuses)
    missing_optional = sum(status == "missing_local_optional" for status in statuses)
    present = len(results) - missing_required - missing_optional
    non_assignment_records = sum(
        status == "verified_exact_bytes_with_non_assignment_records"
        for status in statuses
    )
    opaque = sum(status == "verified_exact_opaque_bytes" for status in statuses)
    policy_failed = failed > 0 or missing_required > 0 or (
        require_local_evidence and missing_optional > 0
    )
    if policy_failed:
        verdict = "failed"
        exit_status = 1
    elif missing_optional:
        verdict = "required_entries_match_declared_local_entries_unavailable"
        exit_status = 0
    else:
        verdict = "declared_inputs_integrity_and_lexical_checks_passed"
        exit_status = 0
    groups = build_exact_byte_groups(results)
    report = {
        "schema": REPORT_SCHEMA,
        "coverage_scope": COVERAGE,
        "claim": CLAIM,
        "non_claims": NON_CLAIMS,
        "catalog_semantic_sha256": catalog_sha256,
        "policy": {"require_local_evidence": require_local_evidence},
        "verdict": verdict,
        "qualification_ready": False,
        "independent_provenance_established": False,
        "coverage": {
            "expected": len(results),
            "present": present,
            "integrity_verified": sum(
                status.startswith("verified") for status in statuses
            ),
            "missing_required": missing_required,
            "missing_local_optional": missing_optional,
            "failed": failed,
            "with_non_assignment_records": non_assignment_records,
            "exact_opaque_bytes": opaque,
            "unique_verified_byte_strings": len(groups),
            "all_declared_local_optional_entries_present": missing_optional == 0,
        },
        "exact_byte_groups": groups,
        "artifacts": results,
    }
    return report, exit_status


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    command.add_argument("--catalog", required=True, type=Path)
    command.add_argument("--project-root", type=Path, default=Path("."))
    command.add_argument("--workspace-root", type=Path, default=Path("../.."))
    command.add_argument("--require-local-evidence", action="store_true")
    command.add_argument(
        "--json", action="store_true", help="emit canonical JSON instead of a summary"
    )
    return command


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parser().parse_args(argv)
    try:
        report, status = audit_catalog(
            args.catalog,
            args.project_root,
            args.workspace_root,
            args.require_local_evidence,
        )
    except CatalogError as error:
        print(f"ERROR: boot-artifact catalog: {error}", file=sys.stderr)
        return 2
    if args.json:
        sys.stdout.buffer.write(canonical_json_bytes(report))
    else:
        coverage = report["coverage"]
        print(
            "boot-artifact audit: "
            f"{report['verdict']} "
            f"(integrity_verified={coverage['integrity_verified']}, "
            f"missing_local_optional={coverage['missing_local_optional']}, "
            f"failed={coverage['failed']})"
        )
    if status:
        print("ERROR: boot-artifact audit policy failed", file=sys.stderr)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
