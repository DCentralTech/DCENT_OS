#!/usr/bin/env python3
"""Strict local validator for AM1 NAND backup result evidence."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import sys
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence


SCHEMA_VERSION = "1.0.0"
RESULT_TYPE = "am1_nand_backup_result"
TARGET_CLASS = "am1 Zynq XC7Z010"
AUTHORIZED_BOARD_TARGET = "am1-s9"
BACKUP_SCOPE = "data-only-no-oob"
RESTORE_AUTHORITY = "none-until-physical-rehearsal"
STRICT_TARGET_FIELDS = (
    "model",
    "compatible",
    "authorized_board_target",
    "backup_scope",
    "restore_authority",
    "ssh_host_key_sha256",
)
MTD_ERASE_SIZE_BYTES = 128 * 1024
SHA256_RE = re.compile(r"[0-9a-f]{64}")
SAFE_LEAF_RE = re.compile(r"[A-Za-z0-9._-]+")
SAFE_TARGET_RE = re.compile(r"[A-Za-z0-9_.:-]+")
MAC_RE = re.compile(r"[0-9a-f]{2}(?::[0-9a-f]{2}){5}")
OPENSSH_SHA256_RE = re.compile(r"SHA256:[A-Za-z0-9+/]{43}")
UTC_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
EXPECTED_LAYOUTS: dict[str, tuple[tuple[int, str, int], ...]] = {
    "stock": (
        (0, "boot", 32 * 1024 * 1024),
        (1, "rootfs", 144 * 1024 * 1024),
        (2, "upgrade", 80 * 1024 * 1024),
    ),
    "braiinsos": (
        (0, "boot", 512 * 1024),
        (1, "uboot", 2560 * 1024),
        (2, "fpga1", 2 * 1024 * 1024),
        (3, "fpga2", 2 * 1024 * 1024),
        (4, "uboot_env", 512 * 1024),
        (5, "miner_cfg", 512 * 1024),
        (6, "recovery", 22 * 1024 * 1024),
        (7, "firmware1", 95 * 1024 * 1024),
        (8, "firmware2", 95 * 1024 * 1024),
        (9, "factory", 36 * 1024 * 1024),
    ),
}
SELF_TEST_LAYOUTS: dict[str, tuple[tuple[int, str, int], ...]] = {
    "stock": (
        (0, "boot", 64),
        (1, "rootfs", 96),
        (2, "upgrade", 80),
    )
}


class ValidationError(ValueError):
    """The backup evidence is incomplete, ambiguous, or inconsistent."""


@dataclass(frozen=True)
class ValidatedBackup:
    target_ip: str
    target_mac: str
    target_hwid: str
    partition_names: tuple[str, ...]
    partition_geometry: tuple[str, ...]
    artifacts: tuple[tuple[str, str], ...]


def unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValidationError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValidationError(f"{label} must be a JSON object")
    return value


def require_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise ValidationError(f"{label} must be a JSON array")
    return value


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValidationError(f"{label} must be a nonempty string")
    return value


def require_int(value: Any, label: str, *, minimum: int = 0) -> int:
    if type(value) is not int or value < minimum:
        raise ValidationError(f"{label} must be an integer >= {minimum}")
    return value


def require_exact_keys(
    value: dict[str, Any], label: str, expected: set[str]
) -> None:
    actual = set(value)
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        raise ValidationError(
            f"{label} keys are not exact "
            f"(missing={missing!r} unexpected={unexpected!r})"
        )


def require_safe_leaf(value: Any, label: str) -> str:
    leaf = require_string(value, label)
    if leaf in {".", ".."} or SAFE_LEAF_RE.fullmatch(leaf) is None:
        raise ValidationError(f"{label} is not a safe artifact leaf: {leaf!r}")
    return leaf


def load_json(path: Path) -> dict[str, Any]:
    if path.is_symlink():
        raise ValidationError("manifest must not be a symlink")
    try:
        raw = path.read_text(encoding="utf-8")
        data = json.loads(raw, object_pairs_hook=unique_json_object)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError(f"cannot parse manifest JSON: {error}") from error
    return require_object(data, "manifest root")


def resolve_regular_leaf(root: Path, leaf: str, label: str) -> Path:
    candidate = root / leaf
    if candidate.is_symlink():
        raise ValidationError(f"{label} must not be a symlink: {leaf}")
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise ValidationError(f"{label} is missing: {leaf}") from error
    if resolved.parent != root:
        raise ValidationError(f"{label} escapes the backup directory: {leaf}")
    if not resolved.is_file():
        raise ValidationError(f"{label} is not a regular file: {leaf}")
    return resolved


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def parse_sha256sums(path: Path) -> dict[str, str]:
    entries: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"cannot read SHA256SUMS: {error}") from error
    if not lines:
        raise ValidationError("SHA256SUMS is empty")
    for line_number, line in enumerate(lines, start=1):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._-]+)", line)
        if match is None:
            raise ValidationError(f"SHA256SUMS line {line_number} is malformed")
        digest, artifact = match.groups()
        if artifact in entries:
            raise ValidationError(f"SHA256SUMS repeats artifact {artifact!r}")
        entries[artifact] = digest
    return entries


def validate_backup(
    manifest_path: Path,
    backup_dir: Path,
    expected_target: str | None = None,
    expected_mac: str | None = None,
    expected_hwid: str | None = None,
    *,
    layout_contracts: dict[str, tuple[tuple[int, str, int], ...]] = EXPECTED_LAYOUTS,
    result_type: str = RESULT_TYPE,
    target_class: str = TARGET_CLASS,
    target_family: str = "AM1",
    required_target_fields: tuple[str, ...] = STRICT_TARGET_FIELDS,
    required_partition_fields: tuple[str, ...] = ("readback_sha256",),
    expected_target_metadata: dict[str, str] | None = None,
    max_age_seconds: int | None = None,
    now: datetime | None = None,
) -> ValidatedBackup:
    unsupported_partition_fields = set(required_partition_fields) - {"readback_sha256"}
    if unsupported_partition_fields:
        raise ValidationError(
            f"unsupported required partition fields: {sorted(unsupported_partition_fields)!r}"
        )
    try:
        root = backup_dir.resolve(strict=True)
    except OSError as error:
        raise ValidationError(f"backup directory is missing: {backup_dir}") from error
    if not root.is_dir():
        raise ValidationError(f"backup directory is not a directory: {backup_dir}")

    manifest = load_json(manifest_path)
    require_exact_keys(
        manifest,
        "manifest root",
        {
            "schema_version",
            "type",
            "execution_utc",
            "target",
            "readback_verify",
            "readback_failures",
            "partitions",
            "verification",
            "nand_backup_complete",
        },
    )
    if manifest.get("schema_version") != SCHEMA_VERSION:
        raise ValidationError(f"schema_version must be {SCHEMA_VERSION!r}")
    if manifest.get("type") != result_type:
        raise ValidationError(f"type must be {result_type!r}")
    execution_utc = require_string(manifest.get("execution_utc"), "execution_utc")
    if UTC_RE.fullmatch(execution_utc) is None:
        raise ValidationError("execution_utc must be an RFC3339 UTC second timestamp")
    try:
        execution_time = datetime.strptime(
            execution_utc, "%Y-%m-%dT%H:%M:%SZ"
        ).replace(tzinfo=timezone.utc)
    except ValueError as error:
        raise ValidationError("execution_utc is not a valid UTC timestamp") from error
    if max_age_seconds is not None:
        if max_age_seconds <= 0:
            raise ValidationError("max_age_seconds must be positive")
        now_utc = now or datetime.now(timezone.utc)
        if now_utc.tzinfo is None:
            raise ValidationError("validation clock must be timezone-aware")
        age_seconds = (now_utc - execution_time).total_seconds()
        if age_seconds < 0:
            raise ValidationError("execution_utc must not be in the future")
        if age_seconds >= max_age_seconds:
            raise ValidationError(
                f"backup evidence is at least {max_age_seconds} seconds old"
            )
    if manifest.get("nand_backup_complete") != "pass":
        raise ValidationError("nand_backup_complete must be 'pass'")

    target = require_object(manifest.get("target"), "target")
    require_exact_keys(
        target,
        "target",
        {"ip", "mac", "hwid", "class", "layout", *required_target_fields},
    )
    target_ip = require_string(target.get("ip"), "target.ip")
    if SAFE_TARGET_RE.fullmatch(target_ip) is None:
        raise ValidationError(f"target.ip contains unsafe characters: {target_ip!r}")
    if target.get("class") != target_class:
        raise ValidationError(f"target.class must be {target_class!r}")
    layout = require_string(target.get("layout"), "target.layout")
    expected_layout = layout_contracts.get(layout)
    if expected_layout is None:
        raise ValidationError(
            f"target.layout is not a known {target_family} layout: {layout!r}"
        )
    if expected_target is not None and target_ip != expected_target:
        raise ValidationError(
            f"target.ip {target_ip!r} does not match "
            f"expected target {expected_target!r}"
        )
    target_mac = require_string(target.get("mac"), "target.mac")
    if MAC_RE.fullmatch(target_mac) is None:
        raise ValidationError("target.mac must be a lowercase colon-delimited MAC")
    target_hwid = require_string(target.get("hwid"), "target.hwid")
    if SAFE_TARGET_RE.fullmatch(target_hwid) is None:
        raise ValidationError("target.hwid contains unsafe characters")
    if expected_mac is not None and target_mac != expected_mac:
        raise ValidationError(
            f"target.mac {target_mac!r} does not match expected MAC {expected_mac!r}"
        )
    if expected_hwid is not None and target_hwid != expected_hwid:
        raise ValidationError(
            f"target.hwid {target_hwid!r} does not match "
            f"expected HWID {expected_hwid!r}"
        )
    if expected_target_metadata is None and result_type == RESULT_TYPE:
        expected_metadata = {
            "backup_scope": BACKUP_SCOPE,
            "restore_authority": RESTORE_AUTHORITY,
        }
    else:
        expected_metadata = expected_target_metadata or {}
    unexpected_expected_fields = set(expected_metadata) - set(required_target_fields)
    if unexpected_expected_fields:
        raise ValidationError(
            "expected target metadata contains undeclared fields: "
            f"{sorted(unexpected_expected_fields)!r}"
        )
    for field in required_target_fields:
        observed = require_string(target.get(field), f"target.{field}")
        if field == "ssh_host_key_sha256":
            if OPENSSH_SHA256_RE.fullmatch(observed) is None:
                raise ValidationError("target.ssh_host_key_sha256 is malformed")
        elif SAFE_TARGET_RE.fullmatch(observed) is None:
            raise ValidationError(f"target.{field} contains unsafe characters")
        expected = expected_metadata.get(field)
        if expected is not None and observed != expected:
            raise ValidationError(
                f"target.{field} {observed!r} does not match expected {expected!r}"
            )
    if (
        target_family == "AM1"
        and "authorized_board_target" in required_target_fields
        and target["authorized_board_target"] != AUTHORIZED_BOARD_TARGET
    ):
        raise ValidationError(
            "target.authorized_board_target must be the canonical AM1 target "
            f"{AUTHORIZED_BOARD_TARGET!r}"
        )

    readback_verify = require_int(manifest.get("readback_verify"), "readback_verify")
    if readback_verify != 1:
        raise ValidationError("readback_verify must be 1")
    top_readback_failures = require_int(
        manifest.get("readback_failures"), "readback_failures"
    )
    if top_readback_failures != 0:
        raise ValidationError("readback_failures must be zero")

    partitions = require_list(manifest.get("partitions"), "partitions")
    if not partitions:
        raise ValidationError("partitions must not be empty")

    seen_mtd: set[int] = set()
    seen_names: set[str] = set()
    seen_artifacts: set[str] = set()
    expected_sums: dict[str, str] = {}
    partition_names: list[str] = []
    total_bytes = 0

    for index, raw_partition in enumerate(partitions):
        label = f"partitions[{index}]"
        partition = require_object(raw_partition, label)
        require_exact_keys(
            partition,
            label,
            {
                "device",
                "mtd_number",
                "name",
                "size_bytes",
                "artifact",
                "sha256",
                "actual_bytes",
                "status",
            }
            | set(required_partition_fields),
        )
        mtd_number = require_int(partition.get("mtd_number"), f"{label}.mtd_number")
        if mtd_number in seen_mtd:
            raise ValidationError(f"duplicate mtd_number {mtd_number}")
        seen_mtd.add(mtd_number)
        if partition.get("device") != f"/dev/mtd{mtd_number}":
            raise ValidationError(f"{label}.device disagrees with mtd_number")

        name = require_safe_leaf(partition.get("name"), f"{label}.name")
        if name in seen_names:
            raise ValidationError(f"duplicate partition name {name!r}")
        seen_names.add(name)
        partition_names.append(name)

        artifact = require_safe_leaf(partition.get("artifact"), f"{label}.artifact")
        if artifact in seen_artifacts:
            raise ValidationError(f"duplicate artifact {artifact!r}")
        seen_artifacts.add(artifact)
        expected_artifact = f"mtd{mtd_number}_{name}.nanddump"
        if artifact != expected_artifact:
            raise ValidationError(
                f"{label}.artifact must be {expected_artifact!r}"
            )
        if partition.get("status") != "pass":
            raise ValidationError(f"{label}.status must be 'pass'")

        size_bytes = require_int(
            partition.get("size_bytes"), f"{label}.size_bytes", minimum=1
        )
        actual_bytes = require_int(
            partition.get("actual_bytes"), f"{label}.actual_bytes", minimum=1
        )
        if actual_bytes != size_bytes:
            raise ValidationError(f"{label}.actual_bytes must equal size_bytes")
        expected_sha = require_string(partition.get("sha256"), f"{label}.sha256")
        if SHA256_RE.fullmatch(expected_sha) is None:
            raise ValidationError(f"{label}.sha256 must be 64 lowercase hex characters")
        if "readback_sha256" in required_partition_fields:
            readback_sha = require_string(
                partition.get("readback_sha256"), f"{label}.readback_sha256"
            )
            if SHA256_RE.fullmatch(readback_sha) is None:
                raise ValidationError(
                    f"{label}.readback_sha256 must be 64 lowercase hex characters"
                )
            if readback_sha != expected_sha:
                raise ValidationError(f"{label}.readback_sha256 must equal sha256")

        artifact_path = resolve_regular_leaf(root, artifact, "backup artifact")
        observed_size = artifact_path.stat().st_size
        if observed_size != actual_bytes:
            raise ValidationError(
                f"{artifact}: size mismatch "
                f"(manifest={actual_bytes} actual={observed_size})"
            )
        observed_sha = sha256_file(artifact_path)
        if observed_sha != expected_sha:
            raise ValidationError(
                f"{artifact}: SHA256 mismatch "
                f"(manifest={expected_sha} actual={observed_sha})"
            )
        expected_sums[artifact] = expected_sha
        total_bytes += actual_bytes

    observed_layout = tuple(
        sorted(
            (
                require_int(partition["mtd_number"], "partition mtd_number"),
                require_string(partition["name"], "partition name"),
                require_int(partition["size_bytes"], "partition size_bytes"),
            )
            for partition in partitions
        )
    )
    if observed_layout != expected_layout:
        raise ValidationError(
            f"layout {layout} requires its exact MTD name/size inventory"
        )

    verification = require_object(manifest.get("verification"), "verification")
    require_exact_keys(
        verification,
        "verification",
        {
            "expected_artifact_count",
            "actual_artifact_count",
            "fail_count",
            "readback_failures",
            "total_bytes",
            "sha256sums_file",
            "log_file",
        },
    )
    expected_count = require_int(
        verification.get("expected_artifact_count"),
        "verification.expected_artifact_count",
        minimum=1,
    )
    actual_count = require_int(
        verification.get("actual_artifact_count"),
        "verification.actual_artifact_count",
        minimum=1,
    )
    if expected_count != len(partitions) or actual_count != len(partitions):
        raise ValidationError(
            "verification artifact counts must equal partitions length"
        )
    if require_int(verification.get("fail_count"), "verification.fail_count") != 0:
        raise ValidationError("verification.fail_count must be zero")
    verification_readback = require_int(
        verification.get("readback_failures"), "verification.readback_failures"
    )
    if verification_readback != 0 or verification_readback != top_readback_failures:
        raise ValidationError(
            "verification readback failures must consistently be zero"
        )
    if (
        require_int(verification.get("total_bytes"), "verification.total_bytes")
        != total_bytes
    ):
        raise ValidationError("verification.total_bytes disagrees with partition bytes")

    sums_leaf = require_safe_leaf(
        verification.get("sha256sums_file"), "verification.sha256sums_file"
    )
    sums_path = resolve_regular_leaf(root, sums_leaf, "SHA256SUMS evidence")
    if parse_sha256sums(sums_path) != expected_sums:
        raise ValidationError("SHA256SUMS entries disagree with partition evidence")
    log_leaf = require_safe_leaf(verification.get("log_file"), "verification.log_file")
    log_path = resolve_regular_leaf(root, log_leaf, "backup log evidence")
    if log_path.stat().st_size == 0:
        raise ValidationError("backup log evidence is empty")

    return ValidatedBackup(
        target_ip=target_ip,
        target_mac=target_mac,
        target_hwid=target_hwid,
        partition_names=tuple(sorted(partition_names)),
        partition_geometry=tuple(
            f"mtd{mtd_number}:{size_bytes:08x}:"
            f"{MTD_ERASE_SIZE_BYTES:08x}:{name}"
            for mtd_number, name, size_bytes in expected_layout
        ),
        artifacts=tuple(sorted(expected_sums.items())),
    )


def fixture_manifest(
    backup_dir: Path,
    *,
    layout_contracts: dict[
        str, tuple[tuple[int, str, int], ...]
    ] = EXPECTED_LAYOUTS,
) -> tuple[Path, dict[str, Any]]:
    partitions: list[dict[str, Any]] = []
    sums: list[str] = []
    total_bytes = 0
    for mtd_number, name, size_bytes in layout_contracts["stock"]:
        artifact = backup_dir / f"mtd{mtd_number}_{name}.nanddump"
        with artifact.open("wb") as handle:
            handle.write(f"am1-{name}".encode("ascii"))
            handle.truncate(size_bytes)
        digest = sha256_file(artifact)
        actual_bytes = artifact.stat().st_size
        partitions.append(
            {
                "device": f"/dev/mtd{mtd_number}",
                "mtd_number": mtd_number,
                "name": name,
                "size_bytes": size_bytes,
                "artifact": artifact.name,
                "sha256": digest,
                "readback_sha256": digest,
                "actual_bytes": actual_bytes,
                "status": "pass",
            }
        )
        sums.append(f"{digest}  {artifact.name}\n")
        total_bytes += actual_bytes
    (backup_dir / "SHA256SUMS").write_text(
        "".join(sums), encoding="ascii"
    )
    (backup_dir / "backup.log").write_text(
        "fixture backup completed\n", encoding="utf-8"
    )
    manifest: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "type": RESULT_TYPE,
        "execution_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "target": {
            "ip": "192.0.2.9",
            "mac": "02:00:00:00:00:09",
            "hwid": "AM1-FIXTURE-9",
            "model": "Xilinx_Zynq_AM1_S9",
            "compatible": "xlnx_zynq-7000",
            "authorized_board_target": "am1-s9",
            "backup_scope": BACKUP_SCOPE,
            "restore_authority": RESTORE_AUTHORITY,
            "ssh_host_key_sha256": "SHA256:" + "A" * 43,
            "class": TARGET_CLASS,
            "layout": "stock",
        },
        "readback_verify": 1,
        "readback_failures": 0,
        "partitions": partitions,
        "verification": {
            "expected_artifact_count": len(partitions),
            "actual_artifact_count": len(partitions),
            "fail_count": 0,
            "readback_failures": 0,
            "total_bytes": total_bytes,
            "sha256sums_file": "SHA256SUMS",
            "log_file": "backup.log",
        },
        "nand_backup_complete": "pass",
    }
    manifest_path = backup_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    return manifest_path, manifest


def run_self_test() -> int:
    tests = 0

    def scenario(
        name: str,
        mutation: Callable[[Path, Path, dict[str, Any]], None] | None = None,
        expected_error: str | None = None,
    ) -> None:
        nonlocal tests
        with tempfile.TemporaryDirectory(prefix="dcent-am1-validator-") as temporary:
            backup_dir = Path(temporary) / "backup"
            backup_dir.mkdir()
            manifest_path, manifest = fixture_manifest(
                backup_dir, layout_contracts=SELF_TEST_LAYOUTS
            )
            if mutation is not None:
                mutation(backup_dir, manifest_path, manifest)
            if expected_error is None:
                validate_backup(
                    manifest_path,
                    backup_dir,
                    "192.0.2.9",
                    "02:00:00:00:00:09",
                    "AM1-FIXTURE-9",
                    layout_contracts=SELF_TEST_LAYOUTS,
                )
            else:
                try:
                    validate_backup(
                        manifest_path,
                        backup_dir,
                        "192.0.2.9",
                        "02:00:00:00:00:09",
                        "AM1-FIXTURE-9",
                        layout_contracts=SELF_TEST_LAYOUTS,
                    )
                except ValidationError as error:
                    if expected_error not in str(error):
                        raise AssertionError(
                            f"{name}: expected {expected_error!r}, got {error!r}"
                        ) from error
                else:
                    raise AssertionError(f"{name}: invalid fixture was accepted")
        tests += 1

    def rewrite(path: Path, manifest: dict[str, Any]) -> None:
        path.write_text(json.dumps(manifest), encoding="utf-8")

    scenario("valid")
    scenario(
        "wrong hash",
        lambda _root, path, data: (
            data["partitions"][0].__setitem__("sha256", "0" * 64),
            data["partitions"][0].__setitem__("readback_sha256", "0" * 64),
            rewrite(path, data),
        ),
        "SHA256 mismatch",
    )
    scenario(
        "missing artifact",
        lambda root, _path, _data: (root / "mtd0_boot.nanddump").unlink(),
        "backup artifact is missing",
    )
    scenario(
        "truncated JSON",
        lambda _root, path, _data: path.write_text('{"type":', encoding="utf-8"),
        "cannot parse manifest JSON",
    )
    scenario(
        "incomplete",
        lambda _root, path, data: (
            data.__setitem__("nand_backup_complete", "fail"),
            rewrite(path, data),
        ),
        "nand_backup_complete",
    )
    scenario(
        "readback disabled",
        lambda _root, path, data: (
            data.__setitem__("readback_verify", 0),
            rewrite(path, data),
        ),
        "readback_verify must be 1",
    )
    scenario(
        "planning type",
        lambda _root, path, data: (
            data.__setitem__("type", "am1_nand_backup_manifest"),
            rewrite(path, data),
        ),
        "type must be",
    )
    scenario(
        "size mismatch",
        lambda _root, path, data: (
            data["partitions"][0].__setitem__("actual_bytes", 1),
            data["verification"].__setitem__("total_bytes", 1),
            rewrite(path, data),
        ),
        "actual_bytes must equal size_bytes",
    )
    scenario(
        "traversal",
        lambda _root, path, data: (
            data["partitions"][0].__setitem__("artifact", "../escape"),
            rewrite(path, data),
        ),
        "safe artifact leaf",
    )

    def make_symlink(root: Path, path: Path, data: dict[str, Any]) -> None:
        artifact = root / data["partitions"][0]["artifact"]
        artifact.unlink()
        outside = root.parent / "outside.nanddump"
        outside.write_bytes(b"am1-nand-backup-fixture")
        os.symlink(outside, artifact)
        rewrite(path, data)

    if os.name == "nt":
        print("AM1 NAND backup validator self-test: symlink scenario skipped on Windows")
    else:
        scenario("symlink", make_symlink, "must not be a symlink")

    def duplicate_artifact(_root: Path, path: Path, data: dict[str, Any]) -> None:
        duplicate = copy.deepcopy(data["partitions"][0])
        duplicate["mtd_number"] = 3
        duplicate["device"] = "/dev/mtd3"
        duplicate["name"] = "duplicate"
        data["partitions"].append(duplicate)
        data["verification"]["expected_artifact_count"] = 4
        data["verification"]["actual_artifact_count"] = 4
        data["verification"]["total_bytes"] += duplicate["actual_bytes"]
        rewrite(path, data)

    scenario("duplicate artifact", duplicate_artifact, "duplicate artifact")

    def duplicate_key(_root: Path, path: Path, data: dict[str, Any]) -> None:
        encoded = json.dumps(data)
        path.write_text(
            encoded.replace(
                f'"type": "{RESULT_TYPE}"',
                f'"type": "wrong", "type": "{RESULT_TYPE}"',
                1,
            ),
            encoding="utf-8",
        )

    scenario("duplicate JSON key", duplicate_key, "duplicate JSON key")

    def incomplete_layout(_root: Path, path: Path, data: dict[str, Any]) -> None:
        removed = data["partitions"].pop()
        data["verification"]["expected_artifact_count"] = 2
        data["verification"]["actual_artifact_count"] = 2
        data["verification"]["total_bytes"] -= removed["actual_bytes"]
        rewrite(path, data)

    scenario("incomplete layout", incomplete_layout, "exact MTD name/size inventory")
    scenario(
        "unexpected field",
        lambda _root, path, data: (
            data.__setitem__("nand_backup_complet", "pass"),
            rewrite(path, data),
        ),
        "keys are not exact",
    )
    scenario(
        "invalid timestamp",
        lambda _root, path, data: (
            data.__setitem__("execution_utc", "2026-99-22T00:00:00Z"),
            rewrite(path, data),
        ),
        "not a valid UTC timestamp",
    )
    scenario(
        "MAC mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__("mac", "02:00:00:00:00:10"),
            rewrite(path, data),
        ),
        "does not match expected MAC",
    )
    scenario(
        "HWID mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__("hwid", "AM1-FIXTURE-10"),
            rewrite(path, data),
        ),
        "does not match expected HWID",
    )
    scenario(
        "noncanonical board target",
        lambda _root, path, data: (
            data["target"].__setitem__("authorized_board_target", "operator-selected"),
            rewrite(path, data),
        ),
        "canonical AM1 target",
    )

    # The target-mismatch case needs a different expected identity.
    with tempfile.TemporaryDirectory(prefix="dcent-am1-validator-") as temporary:
        backup_dir = Path(temporary) / "backup"
        backup_dir.mkdir()
        manifest_path, _ = fixture_manifest(
            backup_dir, layout_contracts=SELF_TEST_LAYOUTS
        )
        try:
            validate_backup(
                manifest_path,
                backup_dir,
                "192.0.2.10",
                "02:00:00:00:00:09",
                "AM1-FIXTURE-9",
                layout_contracts=SELF_TEST_LAYOUTS,
            )
        except ValidationError as error:
            if "does not match expected target" not in str(error):
                raise
        else:
            raise AssertionError("target mismatch: invalid fixture was accepted")
    tests += 1

    def wrong_sums(root: Path, _path: Path, _data: dict[str, Any]) -> None:
        (root / "SHA256SUMS").write_text(
            f"{'0' * 64}  mtd0_boot.nanddump\n", encoding="ascii"
        )

    scenario("SHA256SUMS mismatch", wrong_sums, "SHA256SUMS entries disagree")

    def age_scenario(
        name: str, execution_utc: str, now: datetime, expected_error: str
    ) -> None:
        nonlocal tests
        with tempfile.TemporaryDirectory(prefix="dcent-am1-validator-") as temporary:
            backup_dir = Path(temporary) / "backup"
            backup_dir.mkdir()
            manifest_path, manifest = fixture_manifest(
                backup_dir, layout_contracts=SELF_TEST_LAYOUTS
            )
            manifest["execution_utc"] = execution_utc
            rewrite(manifest_path, manifest)
            try:
                validate_backup(
                    manifest_path,
                    backup_dir,
                    "192.0.2.9",
                    "02:00:00:00:00:09",
                    "AM1-FIXTURE-9",
                    layout_contracts=SELF_TEST_LAYOUTS,
                    max_age_seconds=24 * 60 * 60,
                    now=now,
                )
            except ValidationError as error:
                if expected_error not in str(error):
                    raise AssertionError(
                        f"{name}: expected {expected_error!r}, got {error!r}"
                    ) from error
            else:
                raise AssertionError(f"{name}: invalid fixture was accepted")
        tests += 1

    fixed_now = datetime(2026, 7, 22, 12, 0, 0, tzinfo=timezone.utc)
    age_scenario(
        "stale execution time",
        "2026-07-21T12:00:00Z",
        fixed_now,
        "at least 86400 seconds old",
    )
    age_scenario(
        "future execution time",
        "2026-07-22T12:00:01Z",
        fixed_now,
        "must not be in the future",
    )
    print(f"AM1 NAND backup validator self-test passed: {tests} scenarios")
    return 0


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--local-backup-dir", type=Path)
    parser.add_argument("--expected-target")
    parser.add_argument("--expected-mac")
    parser.add_argument("--expected-hwid")
    parser.add_argument("--expected-model")
    parser.add_argument("--expected-compatible")
    parser.add_argument("--expected-board-target")
    parser.add_argument("--expected-host-key-sha256")
    parser.add_argument("--max-age-seconds", type=int)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        if any(
            value is not None
            for value in (
                args.manifest,
                args.local_backup_dir,
                args.expected_target,
                args.expected_mac,
                args.expected_hwid,
                args.expected_model,
                args.expected_compatible,
                args.expected_board_target,
                args.expected_host_key_sha256,
                args.max_age_seconds,
            )
        ):
            parser.error("--self-test cannot be combined with manifest arguments")
    elif args.manifest is None or args.local_backup_dir is None:
        parser.error("--manifest and --local-backup-dir are required")
    elif args.max_age_seconds is not None and args.max_age_seconds <= 0:
        parser.error("--max-age-seconds must be positive")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    try:
        expected_metadata = {
            "backup_scope": BACKUP_SCOPE,
            "restore_authority": RESTORE_AUTHORITY,
        }
        for field, value in (
            ("model", args.expected_model),
            ("compatible", args.expected_compatible),
            ("authorized_board_target", args.expected_board_target),
            ("ssh_host_key_sha256", args.expected_host_key_sha256),
        ):
            if value is not None:
                expected_metadata[field] = value
        result = validate_backup(
            args.manifest,
            args.local_backup_dir,
            args.expected_target,
            args.expected_mac,
            args.expected_hwid,
            expected_target_metadata=expected_metadata,
            max_age_seconds=args.max_age_seconds,
        )
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("local_manifest_validation=fail", file=sys.stderr)
        return 1
    for artifact, digest in result.artifacts:
        print(f"PASS: {artifact} (sha={digest})")
    print(f"target_ip={result.target_ip}")
    print(f"target_mac={result.target_mac}")
    print(f"target_hwid={result.target_hwid}")
    print("partition_names=" + " ".join(result.partition_names))
    print("partition_geometry=" + " ".join(result.partition_geometry))
    print("local_manifest_validation=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
