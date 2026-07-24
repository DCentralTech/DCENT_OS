#!/usr/bin/env python3
"""Strict local validator for AM2 NAND backup result evidence."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence

from validate_am1_nand_backup import (
    ValidationError,
    ValidatedBackup,
    require_object,
    require_string,
    sha256_file,
    unique_json_object,
    validate_backup as validate_common_backup,
)


RESULT_TYPE = "am2_nand_backup_result"
TARGET_CLASS = "am2 Zynq 7007S"
LAYOUT_NAME = "braiinsos-dual-slot"
EXPECTED_LAYOUTS: dict[str, tuple[tuple[int, str, int], ...]] = {
    LAYOUT_NAME: (
        (0, "boot", 8 * 1024 * 1024),
        (1, "boot-failover", 12 * 1024 * 1024),
        (2, "fpga1", 2 * 1024 * 1024),
        (3, "fpga2", 2 * 1024 * 1024),
        (4, "uboot_env", 512 * 1024),
        (5, "miner_cfg", 512 * 1024),
        (6, "recovery", 87 * 1024 * 1024),
        (7, "firmware1", 57 * 1024 * 1024),
        (8, "firmware2", 57 * 1024 * 1024),
        (9, "factory", 30 * 1024 * 1024),
    )
}
SELF_TEST_LAYOUTS: dict[str, tuple[tuple[int, str, int], ...]] = {
    LAYOUT_NAME: (
        (0, "boot", 64),
        (1, "boot-failover", 96),
        (2, "fpga1", 80),
    )
}
BOOT_ID_RE = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
ROOT_LEAF_RE = re.compile(r"mmcblk\d+p\d+")
AUTHORIZED_COMPATIBLE = "xlnx_zynq-7000"
AUTHORIZED_BOARD_TARGET = "am2-s19jpro-zynq"
AUTHORIZED_MODEL_KEY = "antminers19jpro"
QUIESCENCE_CONTRACT = "pass_known_writer_scan_clear_no_writable_mtd"
RUNTIME_EVIDENCE_FILE = "runtime_admission.txt"
RUNTIME_FIELDS = (
    "boot_id",
    "root_source",
    "root_removable",
    "pgrep",
    "writable_mtd_mounts",
    "miners_status",
    "miners",
    "writers_status",
    "writers",
)
AM2_TARGET_FIELDS = (
    "model",
    "authorized_board_target",
    "compatible",
    "ssh_host_key_sha256",
    "external_root_device",
    "sd_recovery_proof_sha256",
    "sd_recovery_boot_id",
    "backup_boot_id",
    "sd_recovery_quiescence",
    "runtime_admission",
    "runtime_evidence_file",
    "runtime_evidence_sha256",
)


def validate_runtime_evidence(
    backup_dir: Path,
    target: dict[str, Any],
    expected_gate_count: int,
) -> None:
    """Validate the result-bound transcript for every runtime admission gate."""

    evidence_name = require_string(
        target.get("runtime_evidence_file"), "target.runtime_evidence_file"
    )
    if evidence_name != RUNTIME_EVIDENCE_FILE:
        raise ValidationError("target.runtime_evidence_file is not canonical")
    evidence_path = backup_dir / evidence_name
    if not evidence_path.is_file() or evidence_path.is_symlink():
        raise ValidationError("runtime admission evidence is missing or not a regular file")
    try:
        evidence_size = evidence_path.stat().st_size
    except OSError as error:
        raise ValidationError(f"cannot stat runtime admission evidence: {error}") from error
    if evidence_size > 64 * 1024:
        raise ValidationError("runtime admission evidence exceeds the 64 KiB ceiling")
    observed_hash = sha256_file(evidence_path)
    expected_hash = require_string(
        target.get("runtime_evidence_sha256"), "target.runtime_evidence_sha256"
    )
    if SHA256_RE.fullmatch(expected_hash) is None or observed_hash != expected_hash:
        raise ValidationError("runtime admission evidence SHA256 mismatch")

    boot_id = require_string(target.get("backup_boot_id"), "target.backup_boot_id")
    root_source = "/dev/" + require_string(
        target.get("external_root_device"), "target.external_root_device"
    )
    try:
        payload = evidence_path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"cannot read runtime admission evidence: {error}") from error
    blocks = payload.strip().split("\n\n") if payload.strip() else []
    if len(blocks) != expected_gate_count:
        raise ValidationError("runtime admission transcript gate count is not exact")
    for index, block in enumerate(blocks, start=1):
        lines = block.splitlines()
        if len(lines) != len(RUNTIME_FIELDS) + 1 or lines[0] != f"gate={index}":
            raise ValidationError("runtime admission transcript block shape is not exact")
        values: dict[str, str] = {}
        for expected_key, line in zip(RUNTIME_FIELDS, lines[1:], strict=True):
            key, separator, value = line.partition("=")
            if separator != "=" or key != expected_key or key in values:
                raise ValidationError("runtime admission transcript fields are not exact")
            values[key] = value
        if (
            values["boot_id"] != boot_id
            or values["root_source"] != root_source
            or values["root_removable"] != "1"
            or not values["pgrep"].startswith("/")
            or values["writable_mtd_mounts"] != "0"
            or values["miners_status"] != "no_matches"
            or values["miners"]
            or values["writers_status"] != "no_matches"
            or values["writers"]
        ):
            raise ValidationError("runtime admission transcript contains an unsafe gate")


def validate_backup(
    manifest_path: Path,
    backup_dir: Path,
    expected_target: str | None = None,
    expected_mac: str | None = None,
    expected_hwid: str | None = None,
    expected_model: str | None = None,
    expected_board_target: str | None = None,
    expected_compatible: str | None = None,
    expected_host_key_sha256: str | None = None,
    expected_external_root_device: str | None = None,
    expected_sd_recovery_proof_sha256: str | None = None,
    expected_sd_recovery_boot_id: str | None = None,
    expected_backup_boot_id: str | None = None,
    expected_sd_recovery_quiescence: str | None = None,
    expected_runtime_admission: str | None = None,
    expected_runtime_evidence_file: str | None = None,
    expected_runtime_evidence_sha256: str | None = None,
    *,
    layout_contracts: dict[
        str, tuple[tuple[int, str, int], ...]
    ] = EXPECTED_LAYOUTS,
    max_age_seconds: int | None = None,
    now: datetime | None = None,
) -> ValidatedBackup:
    """Validate one AM2 result against exact geometry and local artifacts."""

    expected_metadata = {
        key: value
        for key, value in {
            "model": expected_model,
            "authorized_board_target": expected_board_target,
            "compatible": expected_compatible,
            "ssh_host_key_sha256": expected_host_key_sha256,
            "external_root_device": expected_external_root_device,
            "sd_recovery_proof_sha256": expected_sd_recovery_proof_sha256,
            "sd_recovery_boot_id": expected_sd_recovery_boot_id,
            "backup_boot_id": expected_backup_boot_id,
            "sd_recovery_quiescence": expected_sd_recovery_quiescence,
            "runtime_admission": expected_runtime_admission,
            "runtime_evidence_file": expected_runtime_evidence_file,
            "runtime_evidence_sha256": expected_runtime_evidence_sha256,
        }.items()
        if value is not None
    }
    validated = validate_common_backup(
        manifest_path,
        backup_dir,
        expected_target,
        expected_mac,
        expected_hwid,
        layout_contracts=layout_contracts,
        result_type=RESULT_TYPE,
        target_class=TARGET_CLASS,
        target_family="AM2",
        required_target_fields=AM2_TARGET_FIELDS,
        required_partition_fields=(),
        expected_target_metadata=expected_metadata,
        max_age_seconds=max_age_seconds,
        now=now,
    )
    try:
        manifest = json.loads(
            manifest_path.read_text(encoding="utf-8"),
            object_pairs_hook=unique_json_object,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError(f"cannot parse manifest JSON: {error}") from error
    target = require_object(require_object(manifest, "manifest root").get("target"), "target")
    if target.get("authorized_board_target") != AUTHORIZED_BOARD_TARGET:
        raise ValidationError("target.authorized_board_target is not canonical")
    if target.get("compatible") != AUTHORIZED_COMPATIBLE:
        raise ValidationError("target.compatible is not the canonical AM2 SoC")
    model_key = re.sub(
        r"[^a-z0-9]",
        "",
        require_string(target.get("model"), "target.model").lower(),
    )
    if model_key != AUTHORIZED_MODEL_KEY:
        raise ValidationError("target.model is outside the canonical AM2 target/model map")
    external_root = require_string(
        target.get("external_root_device"), "target.external_root_device"
    )
    if ROOT_LEAF_RE.fullmatch(external_root) is None:
        raise ValidationError("target.external_root_device is not an mmc partition")
    for field in ("sd_recovery_boot_id", "backup_boot_id"):
        if BOOT_ID_RE.fullmatch(require_string(target.get(field), f"target.{field}")) is None:
            raise ValidationError(f"target.{field} is malformed")
    if SHA256_RE.fullmatch(
        require_string(
            target.get("sd_recovery_proof_sha256"),
            "target.sd_recovery_proof_sha256",
        )
    ) is None:
        raise ValidationError("target.sd_recovery_proof_sha256 is malformed")
    if target.get("sd_recovery_quiescence") != QUIESCENCE_CONTRACT:
        raise ValidationError("target.sd_recovery_quiescence contract is not exact")
    expected_gate_count = 2 + 3 * len(layout_contracts[LAYOUT_NAME])
    admission = f"pass_{expected_gate_count}_exact_gates_single_boot_known_writer_scan_clear"
    if target.get("runtime_admission") != admission:
        raise ValidationError("target.runtime_admission contract is not exact")
    validate_runtime_evidence(backup_dir, target, expected_gate_count)
    return validated


def fixture_manifest(
    backup_dir: Path,
    *,
    layout_contracts: dict[
        str, tuple[tuple[int, str, int], ...]
    ] = SELF_TEST_LAYOUTS,
) -> tuple[Path, dict[str, Any]]:
    partitions: list[dict[str, Any]] = []
    sums: list[str] = []
    total_bytes = 0
    for mtd_number, name, size_bytes in layout_contracts[LAYOUT_NAME]:
        artifact = backup_dir / f"mtd{mtd_number}_{name}.nanddump"
        with artifact.open("wb") as handle:
            handle.write(f"am2-{name}".encode("ascii"))
            handle.truncate(size_bytes)
        digest = sha256_file(artifact)
        partitions.append(
            {
                "device": f"/dev/mtd{mtd_number}",
                "mtd_number": mtd_number,
                "name": name,
                "size_bytes": size_bytes,
                "artifact": artifact.name,
                "sha256": digest,
                "actual_bytes": size_bytes,
                "status": "pass",
            }
        )
        sums.append(f"{digest}  {artifact.name}\n")
        total_bytes += size_bytes
    (backup_dir / "SHA256SUMS").write_text("".join(sums), encoding="ascii")
    (backup_dir / "backup.log").write_text(
        "fixture backup completed\n", encoding="utf-8"
    )
    fixture_boot_id = "11111111-2222-3333-4444-555555555555"
    runtime_blocks: list[str] = []
    for gate in range(1, 2 + 3 * len(partitions) + 1):
        runtime_blocks.append(
            "\n".join(
                (
                    f"gate={gate}",
                    f"boot_id={fixture_boot_id}",
                    "root_source=/dev/mmcblk0p2",
                    "root_removable=1",
                    "pgrep=/usr/bin/pgrep",
                    "writable_mtd_mounts=0",
                    "miners_status=no_matches",
                    "miners=",
                    "writers_status=no_matches",
                    "writers=",
                )
            )
        )
    runtime_path = backup_dir / RUNTIME_EVIDENCE_FILE
    runtime_path.write_text("\n\n".join(runtime_blocks) + "\n", encoding="utf-8")
    runtime_hash = sha256_file(runtime_path)
    runtime_gate_count = 2 + 3 * len(partitions)
    manifest: dict[str, Any] = {
        "schema_version": "1.0.0",
        "type": RESULT_TYPE,
        "execution_utc": datetime.now(timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        ),
        "target": {
            "ip": "192.0.2.19",
            "mac": "02:00:00:00:00:19",
            "hwid": "AM2-FIXTURE-19",
            "model": "Antminer-S19j-Pro",
            "authorized_board_target": "am2-s19jpro-zynq",
            "compatible": AUTHORIZED_COMPATIBLE,
            "ssh_host_key_sha256": "SHA256:" + "A" * 43,
            "external_root_device": "mmcblk0p2",
            "sd_recovery_proof_sha256": "2" * 64,
            "sd_recovery_boot_id": fixture_boot_id,
            "backup_boot_id": fixture_boot_id,
            "sd_recovery_quiescence": QUIESCENCE_CONTRACT,
            "runtime_admission": f"pass_{runtime_gate_count}_exact_gates_single_boot_known_writer_scan_clear",
            "runtime_evidence_file": RUNTIME_EVIDENCE_FILE,
            "runtime_evidence_sha256": runtime_hash,
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

    def rewrite(path: Path, manifest: dict[str, Any]) -> None:
        path.write_text(json.dumps(manifest), encoding="utf-8")

    def scenario(
        name: str,
        mutation: Callable[[Path, Path, dict[str, Any]], None] | None = None,
        expected_error: str | None = None,
    ) -> None:
        nonlocal tests
        with tempfile.TemporaryDirectory(prefix="dcent-am2-validator-") as temp:
            backup_dir = Path(temp) / "backup"
            backup_dir.mkdir()
            manifest_path, manifest = fixture_manifest(backup_dir)
            if mutation is not None:
                mutation(backup_dir, manifest_path, manifest)
            if expected_error is None:
                validate_backup(
                    manifest_path,
                    backup_dir,
                    "192.0.2.19",
                    "02:00:00:00:00:19",
                    "AM2-FIXTURE-19",
                    "Antminer-S19j-Pro",
                    "am2-s19jpro-zynq",
                    layout_contracts=SELF_TEST_LAYOUTS,
                )
            else:
                try:
                    validate_backup(
                        manifest_path,
                        backup_dir,
                        "192.0.2.19",
                        "02:00:00:00:00:19",
                        "AM2-FIXTURE-19",
                        "Antminer-S19j-Pro",
                        "am2-s19jpro-zynq",
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

    scenario("valid")
    scenario(
        "wrong result type",
        lambda _root, path, data: (
            data.__setitem__("type", "am2_nand_backup_manifest"),
            rewrite(path, data),
        ),
        "type must be",
    )
    scenario(
        "wrong target class",
        lambda _root, path, data: (
            data["target"].__setitem__("class", "am2 Zynq XC7Z020"),
            rewrite(path, data),
        ),
        "target.class must be",
    )
    scenario(
        "short artifact",
        lambda root, _path, data: (
            (root / data["partitions"][0]["artifact"]).write_bytes(b"short")
        ),
        "size mismatch",
    )
    def mask_short_artifact(
        root: Path, path: Path, data: dict[str, Any]
    ) -> None:
        partition = data["partitions"][0]
        artifact = root / partition["artifact"]
        artifact.write_bytes(b"short")
        partition["actual_bytes"] = 5
        partition["size_bytes"] = 5
        partition["sha256"] = sha256_file(artifact)
        data["verification"]["total_bytes"] = sum(
            item["actual_bytes"] for item in data["partitions"]
        )
        (root / "SHA256SUMS").write_text(
            "".join(
                f"{item['sha256']}  {item['artifact']}\n"
                for item in data["partitions"]
            ),
            encoding="ascii",
        )
        rewrite(path, data)

    scenario(
        "manifest masks short artifact",
        mask_short_artifact,
        "exact MTD name/size inventory",
    )
    scenario(
        "wrong artifact hash",
        lambda _root, path, data: (
            data["partitions"][0].__setitem__("sha256", "0" * 64),
            rewrite(path, data),
        ),
        "SHA256 mismatch",
    )
    scenario(
        "missing artifact",
        lambda root, _path, data: (
            root / data["partitions"][0]["artifact"]
        ).unlink(),
        "backup artifact is missing",
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
        "readback failure",
        lambda _root, path, data: (
            data.__setitem__("readback_failures", 1),
            data["verification"].__setitem__("readback_failures", 1),
            rewrite(path, data),
        ),
        "readback_failures must be zero",
    )
    scenario(
        "unsafe artifact path",
        lambda _root, path, data: (
            data["partitions"][0].__setitem__("artifact", "../escape"),
            rewrite(path, data),
        ),
        "safe artifact leaf",
    )

    def symlink_artifact(root: Path, path: Path, data: dict[str, Any]) -> None:
        artifact = root / data["partitions"][0]["artifact"]
        artifact.unlink()
        outside = root.parent / "outside.nanddump"
        outside.write_bytes(b"am2-outside")
        os.symlink(outside, artifact)
        rewrite(path, data)

    if os.name == "nt":
        try:
            with tempfile.TemporaryDirectory(prefix="dcent-am2-symlink-probe-") as temp:
                root = Path(temp)
                target = root / "target"
                target.write_bytes(b"target")
                link = root / "link"
                os.symlink(target, link)
        except OSError as error:
            if getattr(error, "winerror", None) != 1314:
                raise
        else:
            scenario("symlink artifact", symlink_artifact, "must not be a symlink")
    else:
        scenario("symlink artifact", symlink_artifact, "must not be a symlink")

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
    scenario(
        "unknown field",
        lambda _root, path, data: (
            data.__setitem__("nand_backup_complet", "pass"),
            rewrite(path, data),
        ),
        "keys are not exact",
    )

    def swap_geometry(_root: Path, path: Path, data: dict[str, Any]) -> None:
        data["partitions"][0]["mtd_number"] = 1
        data["partitions"][0]["device"] = "/dev/mtd1"
        data["partitions"][1]["mtd_number"] = 0
        data["partitions"][1]["device"] = "/dev/mtd0"
        rewrite(path, data)

    scenario("geometry swap", swap_geometry, "artifact must be")
    scenario(
        "checksum set mismatch",
        lambda root, _path, _data: (root / "SHA256SUMS").write_text(
            f"{'0' * 64}  mtd0_boot.nanddump\n", encoding="ascii"
        ),
        "SHA256SUMS entries disagree",
    )
    scenario(
        "MAC mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__("mac", "02:00:00:00:00:20"),
            rewrite(path, data),
        ),
        "does not match expected MAC",
    )
    scenario(
        "HWID mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__("hwid", "AM2-FIXTURE-20"),
            rewrite(path, data),
        ),
        "does not match expected HWID",
    )
    scenario(
        "model mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__("model", "Antminer-S19-Pro"),
            rewrite(path, data),
        ),
        "does not match expected",
    )
    scenario(
        "board target mismatch",
        lambda _root, path, data: (
            data["target"].__setitem__(
                "authorized_board_target", "am2-s19pro-zynq"
            ),
            rewrite(path, data),
        ),
        "does not match expected",
    )
    scenario(
        "wrong compatible",
        lambda _root, path, data: (
            data["target"].__setitem__("compatible", "not_zynq"),
            rewrite(path, data),
        ),
        "canonical AM2 SoC",
    )

    with tempfile.TemporaryDirectory(prefix="dcent-am2-validator-") as temp:
        backup_dir = Path(temp) / "backup"
        backup_dir.mkdir()
        manifest_path, manifest = fixture_manifest(backup_dir)
        manifest["target"]["model"] = "Definitely-Not-AM2"
        rewrite(manifest_path, manifest)
        try:
            validate_backup(
                manifest_path,
                backup_dir,
                layout_contracts=SELF_TEST_LAYOUTS,
            )
        except ValidationError as error:
            if "canonical AM2 target/model map" not in str(error):
                raise AssertionError(
                    f"unbound model: unexpected error: {error}"
                ) from error
        else:
            raise AssertionError("unbound model: noncanonical model was accepted")
    tests += 1
    scenario(
        "runtime transcript changed",
        lambda root, _path, _data: (root / RUNTIME_EVIDENCE_FILE).write_text(
            "changed\n", encoding="utf-8"
        ),
        "SHA256 mismatch",
    )

    def unsafe_runtime_gate(root: Path, path: Path, data: dict[str, Any]) -> None:
        runtime = root / RUNTIME_EVIDENCE_FILE
        payload = runtime.read_text(encoding="utf-8").replace(
            "writers_status=no_matches\nwriters=",
            "writers_status=matches\nwriters=present",
            1,
        )
        runtime.write_text(payload, encoding="utf-8")
        data["target"]["runtime_evidence_sha256"] = sha256_file(runtime)
        rewrite(path, data)

    scenario(
        "unsafe runtime gate",
        unsafe_runtime_gate,
        "unsafe gate",
    )

    fixed_now = datetime(2026, 7, 22, 12, 0, 0, tzinfo=timezone.utc)
    for name, timestamp, expected_error in (
        (
            "stale execution time",
            "2026-07-21T12:00:00Z",
            "at least 86400 seconds old",
        ),
        (
            "future execution time",
            "2026-07-22T12:00:01Z",
            "must not be in the future",
        ),
    ):
        with tempfile.TemporaryDirectory(prefix="dcent-am2-validator-") as temp:
            backup_dir = Path(temp) / "backup"
            backup_dir.mkdir()
            manifest_path, manifest = fixture_manifest(backup_dir)
            manifest["execution_utc"] = timestamp
            rewrite(manifest_path, manifest)
            try:
                validate_backup(
                    manifest_path,
                    backup_dir,
                    layout_contracts=SELF_TEST_LAYOUTS,
                    max_age_seconds=86400,
                    now=fixed_now,
                )
            except ValidationError as error:
                if expected_error not in str(error):
                    raise AssertionError(
                        f"{name}: expected {expected_error!r}, got {error!r}"
                    ) from error
            else:
                raise AssertionError(f"{name}: invalid fixture was accepted")
        tests += 1

    print(f"AM2 NAND backup validator self-test passed: {tests} scenarios")
    return 0


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--local-backup-dir", type=Path)
    parser.add_argument("--expected-target")
    parser.add_argument("--expected-mac")
    parser.add_argument("--expected-hwid")
    parser.add_argument("--expected-model")
    parser.add_argument("--expected-board-target")
    parser.add_argument("--expected-compatible")
    parser.add_argument("--expected-host-key-sha256")
    parser.add_argument("--expected-external-root-device")
    parser.add_argument("--expected-sd-recovery-proof-sha256")
    parser.add_argument("--expected-sd-recovery-boot-id")
    parser.add_argument("--expected-backup-boot-id")
    parser.add_argument("--expected-sd-recovery-quiescence")
    parser.add_argument("--expected-runtime-admission")
    parser.add_argument("--expected-runtime-evidence-file")
    parser.add_argument("--expected-runtime-evidence-sha256")
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
                args.expected_board_target,
                args.expected_compatible,
                args.expected_host_key_sha256,
                args.expected_external_root_device,
                args.expected_sd_recovery_proof_sha256,
                args.expected_sd_recovery_boot_id,
                args.expected_backup_boot_id,
                args.expected_sd_recovery_quiescence,
                args.expected_runtime_admission,
                args.expected_runtime_evidence_file,
                args.expected_runtime_evidence_sha256,
                args.max_age_seconds,
            )
        ):
            parser.error("--self-test cannot be combined with manifest arguments")
    elif args.manifest is None or args.local_backup_dir is None:
        parser.error("--manifest and --local-backup-dir are required")
    elif args.max_age_seconds is not None and args.max_age_seconds <= 0:
        parser.error("--max-age-seconds must be positive")
    if not args.self_test and args.max_age_seconds is None:
        args.max_age_seconds = 24 * 60 * 60
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
            args.expected_board_target,
            args.expected_compatible,
            args.expected_host_key_sha256,
            args.expected_external_root_device,
            args.expected_sd_recovery_proof_sha256,
            args.expected_sd_recovery_boot_id,
            args.expected_backup_boot_id,
            args.expected_sd_recovery_quiescence,
            args.expected_runtime_admission,
            args.expected_runtime_evidence_file,
            args.expected_runtime_evidence_sha256,
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
