#!/usr/bin/env python3
"""Strict local validator for AM3-BB NAND data-plane backup evidence."""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence

from validate_am1_nand_backup import (
    ValidationError,
    ValidatedBackup,
    sha256_file,
    validate_backup as validate_common_backup,
)


RESULT_TYPE = "am3_bb_nand_backup_result"
TARGET_CLASS = "am3-bb AM335x"
LAYOUT_NAME = "luxos-single-slot"
BACKUP_SCOPE = "data-only-no-oob"
RESTORE_AUTHORITY = "none-until-physical-rehearsal"
EXPECTED_LAYOUTS: dict[str, tuple[tuple[int, str, int], ...]] = {
    LAYOUT_NAME: (
        (0, "spl", 0x00020000),
        (1, "spl_backup1", 0x00020000),
        (2, "spl_backup2", 0x00020000),
        (3, "spl_backup3", 0x00020000),
        (4, "u-boot", 0x001C0000),
        (5, "bootenv", 0x00020000),
        (6, "fdt", 0x00020000),
        (7, "kernel", 0x00500000),
        (8, "root", 0x01400000),
        (9, "config", 0x00200000),
        (10, "sig", 0x00200000),
        (11, "nvdata", 0x06000000),
    )
}
SELF_TEST_LAYOUTS = {
    LAYOUT_NAME: (
        (0, "spl", 64),
        (4, "u-boot", 96),
        (11, "nvdata", 80),
    )
}


def validate_backup(
    manifest_path: Path,
    backup_dir: Path,
    expected_target: str | None = None,
    expected_mac: str | None = None,
    expected_hwid: str | None = None,
    expected_model: str | None = None,
    expected_compatible: str | None = None,
    expected_board_target: str | None = None,
    *,
    layout_contracts: dict[str, tuple[tuple[int, str, int], ...]] = EXPECTED_LAYOUTS,
    max_age_seconds: int | None = None,
    now: datetime | None = None,
) -> ValidatedBackup:
    """Validate exact geometry, identity, scope, hashes, and local artifacts."""

    expected_metadata = {
        "backup_scope": BACKUP_SCOPE,
        "restore_authority": RESTORE_AUTHORITY,
    }
    for key, value in {
        "model": expected_model,
        "compatible": expected_compatible,
        "authorized_board_target": expected_board_target,
    }.items():
        if value is not None:
            expected_metadata[key] = value
    return validate_common_backup(
        manifest_path,
        backup_dir,
        expected_target,
        expected_mac,
        expected_hwid,
        layout_contracts=layout_contracts,
        result_type=RESULT_TYPE,
        target_class=TARGET_CLASS,
        target_family="AM3-BB",
        required_target_fields=(
            "model",
            "compatible",
            "authorized_board_target",
            "backup_scope",
            "restore_authority",
        ),
        required_partition_fields=(),
        expected_target_metadata=expected_metadata,
        max_age_seconds=max_age_seconds,
        now=now,
    )


def fixture_manifest(root: Path) -> tuple[Path, dict[str, Any]]:
    partitions: list[dict[str, Any]] = []
    sums: list[str] = []
    total = 0
    for number, name, size in SELF_TEST_LAYOUTS[LAYOUT_NAME]:
        artifact = root / f"mtd{number}_{name}.nanddump"
        artifact.write_bytes((f"am3-{name}".encode("ascii") * size)[:size])
        digest = sha256_file(artifact)
        partitions.append(
            {
                "device": f"/dev/mtd{number}",
                "mtd_number": number,
                "name": name,
                "size_bytes": size,
                "artifact": artifact.name,
                "sha256": digest,
                "actual_bytes": size,
                "status": "pass",
            }
        )
        sums.append(f"{digest}  {artifact.name}\n")
        total += size
    (root / "SHA256SUMS").write_text("".join(sums), encoding="ascii")
    (root / "backup.log").write_text("fixture complete\n", encoding="utf-8")
    manifest: dict[str, Any] = {
        "schema_version": "1.0.0",
        "type": RESULT_TYPE,
        "execution_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "target": {
            "ip": "192.0.2.79",
            "mac": "02:00:00:00:00:79",
            "hwid": "AM3-BB-FIXTURE-79",
            "model": "BeagleBone_Black_v2.1_on_S19J_IO_BOARD_V2_0",
            "compatible": "ti_am335x-bone-black",
            "authorized_board_target": "am3-bb-s19jpro",
            "backup_scope": BACKUP_SCOPE,
            "restore_authority": RESTORE_AUTHORITY,
            "class": TARGET_CLASS,
            "layout": LAYOUT_NAME,
        },
        "readback_verify": 1,
        "readback_failures": 0,
        "partitions": partitions,
        "verification": {
            "expected_artifact_count": len(partitions),
            "actual_artifact_count": len(partitions),
            "fail_count": 0,
            "readback_failures": 0,
            "total_bytes": total,
            "sha256sums_file": "SHA256SUMS",
            "log_file": "backup.log",
        },
        "nand_backup_complete": "pass",
    }
    path = root / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    return path, manifest


def run_self_test() -> int:
    tests = 0

    def scenario(field: str | None = None, value: object = None, error: str | None = None) -> None:
        nonlocal tests
        with tempfile.TemporaryDirectory(prefix="dcent-am3-bb-result-") as temp:
            root = Path(temp)
            path, manifest = fixture_manifest(root)
            if field is not None:
                owner: Any = manifest
                parts = field.split(".")
                for part in parts[:-1]:
                    owner = owner[int(part)] if isinstance(owner, list) else owner[part]
                final = parts[-1]
                if isinstance(owner, list):
                    owner[int(final)] = value
                else:
                    owner[final] = value
                path.write_text(json.dumps(manifest), encoding="utf-8")
            try:
                validate_backup(
                    path,
                    root,
                    "192.0.2.79",
                    "02:00:00:00:00:79",
                    "AM3-BB-FIXTURE-79",
                    "BeagleBone_Black_v2.1_on_S19J_IO_BOARD_V2_0",
                    "ti_am335x-bone-black",
                    "am3-bb-s19jpro",
                    layout_contracts=SELF_TEST_LAYOUTS,
                )
            except ValidationError as exc:
                if error is None or error not in str(exc):
                    raise AssertionError(f"unexpected validation failure: {exc}") from exc
            else:
                if error is not None:
                    raise AssertionError(f"invalid {field} value was accepted")
            tests += 1

    scenario()
    scenario("type", "am3_bb_nand_backup_manifest", "type must be")
    scenario("readback_verify", 0, "readback_verify")
    scenario("target.backup_scope", "full-restorable", "does not match expected")
    scenario("target.restore_authority", "approved", "does not match expected")
    scenario("target.model", "wrong", "does not match expected")
    scenario("target.compatible", "wrong", "does not match expected")
    scenario("target.mac", "02:00:00:00:00:80", "does not match expected MAC")
    scenario("partitions.0.size_bytes", 1, "actual_bytes must equal size_bytes")
    print(f"AM3-BB NAND backup validator self-test passed: {tests} scenarios")
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
    parser.add_argument("--max-age-seconds", type=int, default=86400)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return args
    if args.manifest is None or args.local_backup_dir is None:
        parser.error("--manifest and --local-backup-dir are required")
    if args.max_age_seconds <= 0:
        parser.error("--max-age-seconds must be positive")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    try:
        result = validate_backup(
            args.manifest,
            args.local_backup_dir,
            args.expected_target,
            args.expected_mac,
            args.expected_hwid,
            args.expected_model,
            args.expected_compatible,
            args.expected_board_target,
            max_age_seconds=args.max_age_seconds,
        )
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("local_manifest_validation=fail", file=sys.stderr)
        return 1
    for artifact, digest in result.artifacts:
        print(f"PASS: {artifact} (sha={digest})")
    print("partition_geometry=" + " ".join(result.partition_geometry))
    print("local_manifest_validation=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
