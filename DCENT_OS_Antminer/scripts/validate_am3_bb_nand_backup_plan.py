#!/usr/bin/env python3
"""Strictly validate and normalize an authorized AM3-BB NAND backup plan."""

from __future__ import annotations

import argparse
import copy
import re
import sys
from datetime import datetime, timezone
from typing import Any, Sequence

import validate_am2_nand_backup_plan as common
from validate_am1_nand_backup import (
    MTD_ERASE_SIZE_BYTES,
    SHA256_RE,
    ValidationError,
    require_int,
    require_object,
    require_string,
)
from validate_am3_bb_nand_backup import EXPECTED_LAYOUTS, LAYOUT_NAME, TARGET_CLASS


PLAN_TYPE = "am3_bb_nand_backup_manifest"
MIN_FREE_MB = 280
AM3_PREFLIGHT_KEYS = frozenset(
    {
        "sd_recovery_root_device",
        "sd_recovery_root_removable",
        "sd_recovery_proof_sha256",
        "restore_proof_sha256",
    }
)


def load_plan(path: str, stdin: Any) -> dict[str, Any]:
    return common.load_plan(path, stdin)


def validate_plan(
    plan: dict[str, Any],
    *,
    now: datetime | None = None,
    max_age_seconds: int = common.PLAN_MAX_AGE_SECONDS,
) -> tuple[str, int, str, str, str, str, str, str, list[str]]:
    pre_flight = require_object(plan.get("pre_flight"), "pre_flight")
    root_device = require_string(
        pre_flight.get("sd_recovery_root_device"),
        "pre_flight.sd_recovery_root_device",
    )
    if re.fullmatch(r"/dev/mmcblk\d+p\d+", root_device) is None:
        raise ValidationError("sd_recovery_root_device is not an exact mmc partition")
    if require_int(
        pre_flight.get("sd_recovery_root_removable"),
        "pre_flight.sd_recovery_root_removable",
    ) != 1:
        raise ValidationError("sd_recovery_root_removable must be one")
    proof_sha = require_string(
        pre_flight.get("sd_recovery_proof_sha256"),
        "pre_flight.sd_recovery_proof_sha256",
    )
    if SHA256_RE.fullmatch(proof_sha) is None:
        raise ValidationError("sd_recovery_proof_sha256 must be lowercase SHA256")
    restore_proof_sha = require_string(
        pre_flight.get("restore_proof_sha256"),
        "pre_flight.restore_proof_sha256",
    )
    if SHA256_RE.fullmatch(restore_proof_sha) is None:
        raise ValidationError("restore_proof_sha256 must be lowercase SHA256")
    return common.validate_plan(
        plan,
        now=now,
        max_age_seconds=max_age_seconds,
        plan_type=PLAN_TYPE,
        target_class=TARGET_CLASS,
        target_family="AM3-BB",
        layout_name=LAYOUT_NAME,
        expected_layouts=EXPECTED_LAYOUTS,
        min_free_mb=MIN_FREE_MB,
        pre_flight_extra_keys=AM3_PREFLIGHT_KEYS,
    )


def fixture_plan() -> dict[str, Any]:
    plan = common.fixture_plan(
        plan_type=PLAN_TYPE,
        target_class=TARGET_CLASS,
        layout_name=LAYOUT_NAME,
        expected_layouts=EXPECTED_LAYOUTS,
        min_free_mb=MIN_FREE_MB,
    )
    plan["pre_flight"].update(
        {
            "restore_matched_mac": "02:00:00:00:00:79",
            "restore_matched_hwid": "AM3-BB-FIXTURE-79",
            "restore_matched_model": "BeagleBone_Black_v2.1_on_S19J_IO_BOARD_V2_0",
            "restore_matched_target": "am3-bb-s19jpro",
            "sd_recovery_ip": "192.0.2.79",
            "sd_recovery_root_device": "/dev/mmcblk0p2",
            "sd_recovery_root_removable": 1,
            "sd_recovery_proof_sha256": "2" * 64,
            "restore_proof_sha256": "3" * 64,
        }
    )
    return plan


def run_self_test() -> int:
    fixed_now = datetime(2026, 7, 22, 12, 0, 0, tzinfo=timezone.utc)
    scenarios: list[tuple[str, Any, str | None]] = [
        ("valid", None, None),
        ("not ready", lambda p: p.__setitem__("plan_ready", 0), "plan_ready"),
        ("no readback", lambda p: p.__setitem__("readback_verify", 0), "readback_verify"),
        ("one row", lambda p: p.__setitem__("partitions", p["partitions"][:1]), "partitions length"),
        ("wrong geometry", lambda p: p["partitions"][4].__setitem__("size_bytes", 1), "exact AM3-BB geometry"),
        ("reordered", lambda p: p["partitions"].__setitem__(slice(0, 2), list(reversed(p["partitions"][:2]))), "exact AM3-BB geometry"),
        ("stale restore", lambda p: p["pre_flight"].__setitem__("restore_verified_utc", "2026-06-22T12:00:00Z"), "at least 30 days old"),
        ("stale SD", lambda p: p["pre_flight"].__setitem__("sd_recovery_verified_utc", "2026-07-21T12:00:00Z"), "at least 86400 seconds old"),
        ("unsafe target", lambda p: p["pre_flight"].__setitem__("restore_matched_target", "bad;target"), "unsafe characters"),
        ("unknown key", lambda p: p.__setitem__("ready", 1), "keys are not exact"),
    ]
    for name, mutation, expected_error in scenarios:
        plan = fixture_plan()
        if mutation is not None:
            mutation(plan)
        try:
            validate_plan(plan, now=fixed_now)
        except ValidationError as error:
            if expected_error is None or expected_error not in str(error):
                raise AssertionError(f"{name}: unexpected error: {error}") from error
        else:
            if expected_error is not None:
                raise AssertionError(f"{name}: invalid plan accepted")

    duplicate = copy.deepcopy(fixture_plan())
    duplicate["partitions"][0]["artifact"] = "../escape"
    try:
        validate_plan(duplicate, now=fixed_now)
    except ValidationError as error:
        if "safe artifact leaf" not in str(error):
            raise
    else:
        raise AssertionError("unsafe artifact accepted")
    print(f"AM3-BB NAND backup plan validator self-test passed: {len(scenarios) + 1} scenarios")
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
            endpoint,
            target,
            mac,
            hwid,
            model,
            host_key,
            partitions,
        ) = validate_plan(plan)
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("plan_validation=fail", file=sys.stderr)
        return 1
    print(f"layout={layout}")
    print(f"min_free_mb={min_free_mb}")
    print(f"expected_endpoint={endpoint}")
    print(f"expected_target={target}")
    print(f"expected_mac={mac}")
    print(f"expected_hwid={hwid}")
    print(f"expected_model={model}")
    print(f"expected_host_key_sha256={host_key}")
    for partition in partitions:
        number, name, size, artifact = partition.split("|", 3)
        print(f"partition={partition}")
        print(
            f"geometry=mtd{number}:{int(size):08x}:"
            f"{MTD_ERASE_SIZE_BYTES:08x}:{name}"
        )
        print(f"artifact={artifact}")
    print("plan_validation=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
