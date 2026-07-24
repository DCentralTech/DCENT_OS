#!/usr/bin/env python3
"""Strictly validate and normalize an authorized AM2 NAND backup plan."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Sequence, TextIO

from validate_am1_nand_backup import (
    MAC_RE,
    MTD_ERASE_SIZE_BYTES,
    SAFE_TARGET_RE,
    SHA256_RE,
    ValidationError,
    require_exact_keys,
    require_int,
    require_list,
    require_object,
    require_safe_leaf,
    require_string,
    unique_json_object,
)
from validate_am2_nand_backup import (
    EXPECTED_LAYOUTS,
    LAYOUT_NAME,
    TARGET_CLASS,
)


PLAN_TYPE = "am2_nand_backup_manifest"
SCHEMA_VERSION = "1.0.0"
TARGET_FAMILY = "AM2"
MIN_FREE_MB = 280
RESTORE_PROOF_MAX_AGE_SECONDS = 30 * 24 * 60 * 60
PLAN_MAX_AGE_SECONDS = 24 * 60 * 60
OPENSSH_SHA256_RE = re.compile(r"SHA256:[A-Za-z0-9+/]{43}")
ROOT_DEVICE_RE = re.compile(r"/dev/mmcblk\d+p\d+")
BOOT_ID_RE = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
AUTHORIZED_COMPATIBLE = "xlnx_zynq-7000"
AUTHORIZED_BOARD_TARGET = "am2-s19jpro-zynq"
AUTHORIZED_MODEL_KEY = "antminers19jpro"
QUIESCENCE_CONTRACT = "pass_known_writer_scan_clear_no_writable_mtd"
AM2_PREFLIGHT_KEYS = frozenset(
    {
        "sd_recovery_root_device",
        "sd_recovery_proof_sha256",
        "sd_recovery_compatible",
        "sd_recovery_boot_id",
        "sd_recovery_quiescence",
    }
)


def load_plan(path: str, stdin: TextIO) -> dict[str, Any]:
    try:
        raw = stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
        data = json.loads(raw, object_pairs_hook=unique_json_object)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError(f"cannot parse plan JSON: {error}") from error
    return require_object(data, "plan root")


def parse_utc(value: Any, label: str) -> datetime:
    timestamp = require_string(value, label)
    try:
        return datetime.strptime(timestamp, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc
        )
    except ValueError as error:
        raise ValidationError(f"{label} must be a valid UTC timestamp") from error


def validate_plan(
    plan: dict[str, Any],
    *,
    now: datetime | None = None,
    max_age_seconds: int = PLAN_MAX_AGE_SECONDS,
    plan_type: str = PLAN_TYPE,
    target_class: str = TARGET_CLASS,
    target_family: str = TARGET_FAMILY,
    layout_name: str = LAYOUT_NAME,
    expected_layouts: dict[
        str, tuple[tuple[int, str, int], ...]
    ] = EXPECTED_LAYOUTS,
    min_free_mb: int = MIN_FREE_MB,
    pre_flight_extra_keys: frozenset[str] = AM2_PREFLIGHT_KEYS,
) -> tuple[str, int, str, str, str, str, str, str, list[str]]:
    require_exact_keys(
        plan,
        "plan root",
        {
            "schema_version",
            "type",
            "generated_utc",
            "target",
            "nand_backup_execute_go",
            "plan_ready",
            "readback_verify",
            "pre_flight",
            "partitions",
            "verification",
        },
    )
    if plan.get("schema_version") != SCHEMA_VERSION:
        raise ValidationError(f"schema_version must be {SCHEMA_VERSION!r}")
    if plan.get("type") != plan_type:
        raise ValidationError(f"type must be {plan_type!r}")
    generated_time = parse_utc(plan.get("generated_utc"), "generated_utc")
    validation_time = now or datetime.now(timezone.utc)
    if validation_time.tzinfo is None:
        raise ValidationError("validation clock must be timezone-aware")
    if max_age_seconds <= 0:
        raise ValidationError("max_age_seconds must be positive")
    plan_age = (validation_time - generated_time).total_seconds()
    if plan_age < 0:
        raise ValidationError("generated_utc must not be in the future")
    if plan_age >= max_age_seconds:
        raise ValidationError(
            f"plan is at least {max_age_seconds} seconds old"
        )
    if require_int(plan.get("nand_backup_execute_go"), "nand_backup_execute_go") != 0:
        raise ValidationError("nand_backup_execute_go must remain zero")
    if require_int(plan.get("plan_ready"), "plan_ready") != 1:
        raise ValidationError("plan_ready must be one")
    if require_int(plan.get("readback_verify"), "readback_verify") != 1:
        raise ValidationError("readback_verify must be one")

    target = require_object(plan.get("target"), "target")
    require_exact_keys(target, "target", {"class", "layout", "partition_count"})
    if target.get("class") != target_class:
        raise ValidationError(f"target.class must be {target_class!r}")
    if target.get("layout") != layout_name:
        raise ValidationError(f"target.layout must be {layout_name!r}")
    expected_layout = expected_layouts[layout_name]
    if require_int(target.get("partition_count"), "target.partition_count") != len(
        expected_layout
    ):
        raise ValidationError(
            f"target.partition_count disagrees with exact {target_family} layout"
        )

    pre_flight = require_object(plan.get("pre_flight"), "pre_flight")
    require_exact_keys(
        pre_flight,
        "pre_flight",
        {
            "restore_artifact_proof",
            "restore_matched_mac",
            "restore_matched_hwid",
            "restore_matched_model",
            "restore_matched_target",
            "restore_artifact_sha256",
            "restore_verified_utc",
            "sd_recovery_probe",
            "sd_recovery_ip",
            "sd_recovery_verified_utc",
            "sd_recovery_host_key_sha256",
            "layout_profile",
            "operator_approval",
            "mining_stopped",
            "storage_adequate",
            "min_free_mb",
        }
        | pre_flight_extra_keys,
    )
    if pre_flight.get("restore_artifact_proof") != "restore_verified_identity_matched":
        raise ValidationError("restore_artifact_proof is not exact-unit verified")
    expected_mac = require_string(
        pre_flight.get("restore_matched_mac"), "pre_flight.restore_matched_mac"
    )
    if MAC_RE.fullmatch(expected_mac) is None:
        raise ValidationError("restore_matched_mac must be a lowercase physical MAC")
    expected_hwid = require_string(
        pre_flight.get("restore_matched_hwid"), "pre_flight.restore_matched_hwid"
    )
    expected_model = require_string(
        pre_flight.get("restore_matched_model"), "pre_flight.restore_matched_model"
    )
    expected_target = require_string(
        pre_flight.get("restore_matched_target"), "pre_flight.restore_matched_target"
    )
    for value, label in (
        (expected_hwid, "restore_matched_hwid"),
        (expected_model, "restore_matched_model"),
        (expected_target, "restore_matched_target"),
    ):
        if SAFE_TARGET_RE.fullmatch(value) is None:
            raise ValidationError(f"{label} contains unsafe characters")
    if target_family == TARGET_FAMILY:
        if expected_target != AUTHORIZED_BOARD_TARGET:
            raise ValidationError("restore_matched_target is not the canonical AM2 target")
        model_key = re.sub(r"[^a-z0-9]", "", expected_model.lower())
        if model_key != AUTHORIZED_MODEL_KEY:
            raise ValidationError("restore_matched_model is outside the AM2 target/model map")
    restore_sha = require_string(
        pre_flight.get("restore_artifact_sha256"),
        "pre_flight.restore_artifact_sha256",
    )
    if SHA256_RE.fullmatch(restore_sha) is None:
        raise ValidationError("restore_artifact_sha256 must be lowercase SHA256")
    restore_time = parse_utc(
        pre_flight.get("restore_verified_utc"), "pre_flight.restore_verified_utc"
    )
    if restore_time > generated_time:
        raise ValidationError("restore_verified_utc must not be after generated_utc")
    restore_age = (validation_time - restore_time).total_seconds()
    if restore_age < 0:
        raise ValidationError("restore_verified_utc must not be in the future")
    if restore_age >= RESTORE_PROOF_MAX_AGE_SECONDS:
        raise ValidationError("restore proof is at least 30 days old")
    if pre_flight.get("sd_recovery_probe") != "external_boot_identity_matched":
        raise ValidationError("sd_recovery_probe is not exact-endpoint verified")
    expected_endpoint = require_string(
        pre_flight.get("sd_recovery_ip"), "pre_flight.sd_recovery_ip"
    )
    if SAFE_TARGET_RE.fullmatch(expected_endpoint) is None:
        raise ValidationError("sd_recovery_ip contains unsafe characters")
    recovery_time = parse_utc(
        pre_flight.get("sd_recovery_verified_utc"),
        "pre_flight.sd_recovery_verified_utc",
    )
    if recovery_time > generated_time:
        raise ValidationError(
            "sd_recovery_verified_utc must not be after generated_utc"
        )
    recovery_age = (validation_time - recovery_time).total_seconds()
    if recovery_age < 0:
        raise ValidationError("sd_recovery_verified_utc must not be in the future")
    if recovery_age >= PLAN_MAX_AGE_SECONDS:
        raise ValidationError("SD recovery proof is at least 86400 seconds old")
    expected_host_key_sha256 = require_string(
        pre_flight.get("sd_recovery_host_key_sha256"),
        "pre_flight.sd_recovery_host_key_sha256",
    )
    if OPENSSH_SHA256_RE.fullmatch(expected_host_key_sha256) is None:
        raise ValidationError(
            "sd_recovery_host_key_sha256 must be an OpenSSH SHA256 fingerprint"
        )
    expected_root_device = require_string(
        pre_flight.get("sd_recovery_root_device"),
        "pre_flight.sd_recovery_root_device",
    )
    if ROOT_DEVICE_RE.fullmatch(expected_root_device) is None:
        raise ValidationError(
            "sd_recovery_root_device is not an exact mmc partition"
        )
    if target_family == TARGET_FAMILY:
        recovery_sha = require_string(
            pre_flight.get("sd_recovery_proof_sha256"),
            "pre_flight.sd_recovery_proof_sha256",
        )
        if SHA256_RE.fullmatch(recovery_sha) is None:
            raise ValidationError("sd_recovery_proof_sha256 must be lowercase SHA256")
        compatible = require_string(
            pre_flight.get("sd_recovery_compatible"),
            "pre_flight.sd_recovery_compatible",
        )
        if compatible != AUTHORIZED_COMPATIBLE:
            raise ValidationError("sd_recovery_compatible is not the canonical AM2 SoC")
        boot_id = require_string(
            pre_flight.get("sd_recovery_boot_id"),
            "pre_flight.sd_recovery_boot_id",
        )
        if BOOT_ID_RE.fullmatch(boot_id) is None:
            raise ValidationError("sd_recovery_boot_id is malformed")
        if pre_flight.get("sd_recovery_quiescence") != QUIESCENCE_CONTRACT:
            raise ValidationError("sd_recovery_quiescence contract is not exact")
    if require_int(pre_flight.get("layout_profile"), "pre_flight.layout_profile") != 1:
        raise ValidationError("layout_profile must be one")
    for field in ("operator_approval", "mining_stopped", "storage_adequate"):
        if pre_flight.get(field) is not False:
            raise ValidationError(f"pre_flight.{field} must remain false")
    if require_int(
        pre_flight.get("min_free_mb"), "pre_flight.min_free_mb", minimum=1
    ) != min_free_mb:
        raise ValidationError(f"pre_flight.min_free_mb must be {min_free_mb}")

    partitions = require_list(plan.get("partitions"), "partitions")
    if len(partitions) != len(expected_layout):
        raise ValidationError(
            f"partitions length disagrees with exact {target_family} layout"
        )
    normalized: list[str] = []
    total_expected_bytes = 0
    for index, expected in enumerate(expected_layout):
        label = f"partitions[{index}]"
        partition = require_object(partitions[index], label)
        require_exact_keys(
            partition,
            label,
            {
                "device",
                "mtd_number",
                "name",
                "size_hex",
                "size_bytes",
                "erase_size_hex",
                "artifact",
                "sha256",
                "actual_bytes",
                "readback_sha256",
                "status",
            },
        )
        expected_mtd, expected_name, expected_size = expected
        mtd_number = require_int(partition.get("mtd_number"), f"{label}.mtd_number")
        name = require_safe_leaf(partition.get("name"), f"{label}.name")
        size_bytes = require_int(
            partition.get("size_bytes"), f"{label}.size_bytes", minimum=1
        )
        if (mtd_number, name, size_bytes) != expected:
            raise ValidationError(
                f"{label} disagrees with exact {target_family} geometry"
            )
        if partition.get("device") != f"/dev/mtd{expected_mtd}":
            raise ValidationError(f"{label}.device disagrees with mtd_number")
        if partition.get("size_hex") != f"0x{expected_size:08x}":
            raise ValidationError(f"{label}.size_hex disagrees with size_bytes")
        if partition.get("erase_size_hex") != f"0x{MTD_ERASE_SIZE_BYTES:08x}":
            raise ValidationError(f"{label}.erase_size_hex is not 128 KiB")
        artifact = require_safe_leaf(partition.get("artifact"), f"{label}.artifact")
        if artifact != f"mtd{expected_mtd}_{expected_name}.nanddump":
            raise ValidationError(f"{label}.artifact is not canonical")
        for pending_field in ("sha256", "actual_bytes", "readback_sha256"):
            if partition.get(pending_field) is not None:
                raise ValidationError(f"{label}.{pending_field} must remain null")
        if partition.get("status") != "pending":
            raise ValidationError(f"{label}.status must be 'pending'")
        normalized.append(
            f"{expected_mtd}|{expected_name}|{expected_size}|{artifact}"
        )
        total_expected_bytes += expected_size

    verification = require_object(plan.get("verification"), "verification")
    require_exact_keys(
        verification,
        "verification",
        {
            "all_artifacts_exist",
            "all_artifacts_nonempty",
            "all_sha256_match",
            "readback_idempotent",
            "total_expected_bytes",
            "nand_backup_complete",
        },
    )
    for field in (
        "all_artifacts_exist",
        "all_artifacts_nonempty",
        "all_sha256_match",
        "readback_idempotent",
        "nand_backup_complete",
    ):
        if verification.get(field) is not None:
            raise ValidationError(f"verification.{field} must remain null")
    if require_int(
        verification.get("total_expected_bytes"),
        "verification.total_expected_bytes",
    ) != total_expected_bytes:
        raise ValidationError("verification.total_expected_bytes is inconsistent")
    return (
        layout_name,
        min_free_mb,
        expected_endpoint,
        expected_target,
        expected_mac,
        expected_hwid,
        expected_model,
        expected_host_key_sha256,
        normalized,
    )


def fixture_plan(
    *,
    plan_type: str = PLAN_TYPE,
    target_class: str = TARGET_CLASS,
    layout_name: str = LAYOUT_NAME,
    expected_layouts: dict[
        str, tuple[tuple[int, str, int], ...]
    ] = EXPECTED_LAYOUTS,
    min_free_mb: int = MIN_FREE_MB,
) -> dict[str, Any]:
    generated = "2026-07-22T12:00:00Z"
    partitions: list[dict[str, Any]] = []
    total = 0
    for mtd_number, name, size_bytes in expected_layouts[layout_name]:
        partitions.append(
            {
                "device": f"/dev/mtd{mtd_number}",
                "mtd_number": mtd_number,
                "name": name,
                "size_hex": f"0x{size_bytes:08x}",
                "size_bytes": size_bytes,
                "erase_size_hex": f"0x{MTD_ERASE_SIZE_BYTES:08x}",
                "artifact": f"mtd{mtd_number}_{name}.nanddump",
                "sha256": None,
                "actual_bytes": None,
                "readback_sha256": None,
                "status": "pending",
            }
        )
        total += size_bytes
    pre_flight: dict[str, Any] = {
        "restore_artifact_proof": "restore_verified_identity_matched",
        "restore_matched_mac": "02:00:00:00:00:19",
        "restore_matched_hwid": "AM2-FIXTURE-19",
        "restore_matched_model": "Antminer-S19j-Pro",
        "restore_matched_target": "am2-s19jpro-zynq",
        "restore_artifact_sha256": "1" * 64,
        "restore_verified_utc": generated,
        "sd_recovery_probe": "external_boot_identity_matched",
        "sd_recovery_ip": "192.0.2.19",
        "sd_recovery_verified_utc": generated,
        "sd_recovery_host_key_sha256": "SHA256:" + "A" * 43,
        "sd_recovery_root_device": "/dev/mmcblk0p2",
        "layout_profile": 1,
        "operator_approval": False,
        "mining_stopped": False,
        "storage_adequate": False,
        "min_free_mb": min_free_mb,
    }
    if plan_type == PLAN_TYPE:
        pre_flight.update(
            {
                "sd_recovery_proof_sha256": "2" * 64,
                "sd_recovery_compatible": AUTHORIZED_COMPATIBLE,
                "sd_recovery_boot_id": "11111111-2222-3333-4444-555555555555",
                "sd_recovery_quiescence": QUIESCENCE_CONTRACT,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "type": plan_type,
        "generated_utc": generated,
        "target": {
            "class": target_class,
            "layout": layout_name,
            "partition_count": len(partitions),
        },
        "nand_backup_execute_go": 0,
        "plan_ready": 1,
        "readback_verify": 1,
        "pre_flight": pre_flight,
        "partitions": partitions,
        "verification": {
            "all_artifacts_exist": None,
            "all_artifacts_nonempty": None,
            "all_sha256_match": None,
            "readback_idempotent": None,
            "total_expected_bytes": total,
            "nand_backup_complete": None,
        },
    }


def run_self_test() -> int:
    tests = 0
    fixed_now = datetime(2026, 7, 22, 12, 0, 0, tzinfo=timezone.utc)

    def scenario(
        name: str,
        mutation: Callable[[dict[str, Any]], None] | None = None,
        expected_error: str | None = None,
    ) -> None:
        nonlocal tests
        plan = fixture_plan()
        if mutation is not None:
            mutation(plan)
        if expected_error is None:
            validate_plan(plan, now=fixed_now)
        else:
            try:
                validate_plan(plan, now=fixed_now)
            except ValidationError as error:
                if expected_error not in str(error):
                    raise AssertionError(
                        f"{name}: expected {expected_error!r}, got {error!r}"
                    ) from error
            else:
                raise AssertionError(f"{name}: invalid plan was accepted")
        tests += 1

    scenario("valid")
    scenario("not ready", lambda p: p.__setitem__("plan_ready", 0), "plan_ready")
    scenario(
        "readback optional",
        lambda p: p.__setitem__("readback_verify", 0),
        "readback_verify",
    )
    scenario(
        "one row",
        lambda p: p.__setitem__("partitions", p["partitions"][:1]),
        "partitions length",
    )
    scenario(
        "wrong 128 KiB env size",
        lambda p: p["partitions"][4].update(
            {"size_hex": "0x00020000", "size_bytes": 128 * 1024}
        ),
        "exact AM2 geometry",
    )
    scenario(
        "reordered geometry",
        lambda p: p["partitions"].__setitem__(
            slice(0, 2), list(reversed(p["partitions"][:2]))
        ),
        "exact AM2 geometry",
    )
    scenario(
        "missing restore hash",
        lambda p: p["pre_flight"].__setitem__("restore_artifact_sha256", None),
        "nonempty string",
    )
    scenario(
        "unsafe target",
        lambda p: p["pre_flight"].__setitem__(
            "restore_matched_target", "am2;touch injected"
        ),
        "unsafe characters",
    )
    scenario(
        "stale proof",
        lambda p: p["pre_flight"].__setitem__(
            "restore_verified_utc", "2026-06-22T12:00:00Z"
        ),
        "at least 30 days old",
    )
    scenario(
        "future proof",
        lambda p: p["pre_flight"].__setitem__(
            "restore_verified_utc", "2026-07-22T12:00:01Z"
        ),
        "must not be after",
    )
    scenario(
        "missing SD recovery proof",
        lambda p: p["pre_flight"].__setitem__(
            "sd_recovery_probe", "proof_malformed_or_not_ready"
        ),
        "not exact-endpoint verified",
    )
    scenario(
        "stale SD recovery proof",
        lambda p: p["pre_flight"].__setitem__(
            "sd_recovery_verified_utc", "2026-07-21T12:00:00Z"
        ),
        "at least 86400 seconds old",
    )
    scenario(
        "SD proof fresh when stale plan was generated",
        lambda p: (
            p.__setitem__("generated_utc", "2026-07-21T12:00:01Z"),
            p["pre_flight"].__setitem__(
                "restore_verified_utc", "2026-07-21T12:00:01Z"
            ),
            p["pre_flight"].__setitem__(
                "sd_recovery_verified_utc", "2026-07-20T12:00:02Z"
            ),
        ),
        "SD recovery proof is at least 86400 seconds old",
    )
    scenario(
        "malformed SD host key",
        lambda p: p["pre_flight"].__setitem__(
            "sd_recovery_host_key_sha256", "SHA256:not-a-fingerprint"
        ),
        "OpenSSH SHA256 fingerprint",
    )
    scenario(
        "unsafe SD root",
        lambda p: p["pre_flight"].__setitem__(
            "sd_recovery_root_device", "/dev/mtdblock7"
        ),
        "exact mmc partition",
    )
    scenario(
        "operator-selected compatible",
        lambda p: p["pre_flight"].__setitem__("sd_recovery_compatible", "not_zynq"),
        "canonical AM2 SoC",
    )
    scenario(
        "operator-selected AM2 target",
        lambda p: p["pre_flight"].__setitem__("restore_matched_target", "am2-s19pro"),
        "canonical AM2 target",
    )
    scenario(
        "wrong AM2 model family",
        lambda p: p["pre_flight"].__setitem__("restore_matched_model", "Antminer-S19-Pro"),
        "target/model map",
    )
    scenario(
        "malformed recovery proof hash",
        lambda p: p["pre_flight"].__setitem__("sd_recovery_proof_sha256", "bad"),
        "lowercase SHA256",
    )
    scenario(
        "malformed recovery boot id",
        lambda p: p["pre_flight"].__setitem__("sd_recovery_boot_id", "rebooted"),
        "malformed",
    )
    scenario(
        "overclaimed quiescence",
        lambda p: p["pre_flight"].__setitem__("sd_recovery_quiescence", "pass no_writers"),
        "contract is not exact",
    )
    scenario(
        "stale plan",
        lambda p: p.__setitem__("generated_utc", "2026-07-21T12:00:00Z"),
        "at least 86400 seconds old",
    )
    scenario(
        "future plan",
        lambda p: p.__setitem__("generated_utc", "2026-07-22T12:00:01Z"),
        "must not be in the future",
    )
    scenario(
        "unknown key",
        lambda p: p.__setitem__("plan_reddy", 1),
        "keys are not exact",
    )
    scenario(
        "unsafe artifact",
        lambda p: p["partitions"][0].__setitem__(
            "artifact", "mtd0_boot';touch injected;.nanddump"
        ),
        "safe artifact leaf",
    )

    with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
        duplicate_path = Path(temp) / "duplicate.json"
        encoded = json.dumps(fixture_plan())
        duplicate_path.write_text(
            encoded.replace(
                f'"type": "{PLAN_TYPE}"',
                f'"type": "wrong", "type": "{PLAN_TYPE}"',
                1,
            ),
            encoding="utf-8",
        )
        try:
            with duplicate_path.open(encoding="utf-8") as handle:
                load_plan(str(duplicate_path), handle)
        except ValidationError as error:
            if "duplicate JSON key" not in str(error):
                raise
        else:
            raise AssertionError("duplicate JSON key: invalid plan was accepted")
    tests += 1

    print(f"AM2 NAND backup plan validator self-test passed: {tests} scenarios")
    return 0


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        if args.plan is not None:
            parser.error("--self-test cannot be combined with --plan")
    elif args.plan is None:
        parser.error("--plan is required")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    try:
        plan = load_plan(args.plan, sys.stdin)
        (
            layout,
            min_free_mb,
            expected_endpoint,
            expected_target,
            expected_mac,
            expected_hwid,
            expected_model,
            expected_host_key_sha256,
            partitions,
        ) = validate_plan(plan)
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("plan_validation=fail", file=sys.stderr)
        return 1
    print(f"layout={layout}")
    print(f"min_free_mb={min_free_mb}")
    print(f"expected_endpoint={expected_endpoint}")
    print(f"expected_target={expected_target}")
    print(f"expected_mac={expected_mac}")
    print(f"expected_hwid={expected_hwid}")
    print(f"expected_model={expected_model}")
    print(f"expected_host_key_sha256={expected_host_key_sha256}")
    print(
        "expected_root_device="
        f"{plan['pre_flight']['sd_recovery_root_device']}"
    )
    print(f"expected_compatible={plan['pre_flight']['sd_recovery_compatible']}")
    print(f"sd_recovery_proof_sha256={plan['pre_flight']['sd_recovery_proof_sha256']}")
    print(f"sd_recovery_boot_id={plan['pre_flight']['sd_recovery_boot_id']}")
    print(f"sd_recovery_quiescence={plan['pre_flight']['sd_recovery_quiescence']}")
    for partition in partitions:
        mtd_number, name, size_bytes, artifact = partition.split("|", 3)
        print(f"partition={partition}")
        print(
            f"geometry=mtd{mtd_number}:{int(size_bytes):08x}:"
            f"{MTD_ERASE_SIZE_BYTES:08x}:{name}"
        )
        print(f"artifact={artifact}")
    print("plan_validation=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
