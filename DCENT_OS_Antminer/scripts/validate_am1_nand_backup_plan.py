#!/usr/bin/env python3
"""Strictly validate and normalize an authorized AM1 NAND backup plan."""

from __future__ import annotations

import argparse
import copy
import re
import sys
from datetime import datetime, timezone
from typing import Any, Sequence

import validate_am2_nand_backup_plan as common
from validate_am1_nand_backup import (
    AUTHORIZED_BOARD_TARGET,
    EXPECTED_LAYOUTS,
    MTD_ERASE_SIZE_BYTES,
    SAFE_TARGET_RE,
    SHA256_RE,
    ValidationError,
    require_int,
    require_object,
    require_string,
)


PLAN_TYPE = "am1_nand_backup_manifest"
TARGET_CLASS = "am1 Zynq XC7Z010"
TARGET_FAMILY = "AM1"
MIN_FREE_MB = 384
AM1_PREFLIGHT_KEYS = frozenset(
    {
        "restore_matched_compatible",
        "sd_recovery_layout",
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
    target = require_object(plan.get("target"), "target")
    layout = require_string(target.get("layout"), "target.layout")
    if layout not in EXPECTED_LAYOUTS:
        raise ValidationError(f"target.layout is not a known AM1 layout: {layout!r}")

    pre_flight = require_object(plan.get("pre_flight"), "pre_flight")
    restore_target = require_string(
        pre_flight.get("restore_matched_target"),
        "pre_flight.restore_matched_target",
    )
    if restore_target != AUTHORIZED_BOARD_TARGET:
        raise ValidationError(
            "restore_matched_target must be the canonical AM1 target "
            f"{AUTHORIZED_BOARD_TARGET!r}"
        )
    compatible = require_string(
        pre_flight.get("restore_matched_compatible"),
        "pre_flight.restore_matched_compatible",
    )
    if SAFE_TARGET_RE.fullmatch(compatible) is None:
        raise ValidationError("restore_matched_compatible contains unsafe characters")
    if pre_flight.get("sd_recovery_layout") != layout:
        raise ValidationError("sd_recovery_layout does not match target.layout")
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
    for field in ("sd_recovery_proof_sha256", "restore_proof_sha256"):
        digest = require_string(pre_flight.get(field), f"pre_flight.{field}")
        if SHA256_RE.fullmatch(digest) is None:
            raise ValidationError(f"{field} must be lowercase SHA256")

    return common.validate_plan(
        plan,
        now=now,
        max_age_seconds=max_age_seconds,
        plan_type=PLAN_TYPE,
        target_class=TARGET_CLASS,
        target_family=TARGET_FAMILY,
        layout_name=layout,
        expected_layouts=EXPECTED_LAYOUTS,
        min_free_mb=MIN_FREE_MB,
        pre_flight_extra_keys=AM1_PREFLIGHT_KEYS,
    )


def fixture_plan(layout: str = "stock") -> dict[str, Any]:
    if layout not in EXPECTED_LAYOUTS:
        raise ValueError(f"unknown fixture layout: {layout}")
    plan = common.fixture_plan(
        plan_type=PLAN_TYPE,
        target_class=TARGET_CLASS,
        layout_name=layout,
        expected_layouts=EXPECTED_LAYOUTS,
        min_free_mb=MIN_FREE_MB,
    )
    plan["pre_flight"].update(
        {
            "restore_matched_mac": "02:00:00:00:00:09",
            "restore_matched_hwid": "AM1-FIXTURE-9",
            "restore_matched_model": "Xilinx_Zynq_AM1_S9",
            "restore_matched_target": "am1-s9",
            "restore_matched_compatible": "xlnx_zynq-7000",
            "sd_recovery_ip": "192.0.2.9",
            "sd_recovery_layout": layout,
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
        ("valid stock", None, None),
        ("not ready", lambda p: p.__setitem__("plan_ready", 0), "plan_ready"),
        ("no readback", lambda p: p.__setitem__("readback_verify", 0), "readback_verify"),
        ("one row", lambda p: p.__setitem__("partitions", p["partitions"][:1]), "partitions length"),
        ("wrong geometry", lambda p: p["partitions"][1].__setitem__("size_bytes", 1), "exact AM1 geometry"),
        ("stale plan", lambda p: p.__setitem__("generated_utc", "2026-07-21T12:00:00Z"), "at least 86400"),
        ("stale restore", lambda p: p["pre_flight"].__setitem__("restore_verified_utc", "2026-06-22T12:00:00Z"), "at least 30 days old"),
        ("stale SD", lambda p: p["pre_flight"].__setitem__("sd_recovery_verified_utc", "2026-07-21T12:00:00Z"), "at least 86400 seconds old"),
        ("wrong SD layout", lambda p: p["pre_flight"].__setitem__("sd_recovery_layout", "braiinsos"), "does not match"),
        ("non-removable", lambda p: p["pre_flight"].__setitem__("sd_recovery_root_removable", 0), "must be one"),
        ("noncanonical target", lambda p: p["pre_flight"].__setitem__("restore_matched_target", "operator-selected"), "canonical AM1 target"),
        ("unknown key", lambda p: p.__setitem__("ready", 1), "keys are not exact"),
    ]
    for name, mutation, expected_error in scenarios:
        plan = fixture_plan()
        plan["generated_utc"] = "2026-07-22T12:00:00Z"
        plan["pre_flight"]["restore_verified_utc"] = "2026-07-22T11:00:00Z"
        plan["pre_flight"]["sd_recovery_verified_utc"] = "2026-07-22T11:30:00Z"
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

    braiins = fixture_plan("braiinsos")
    braiins["generated_utc"] = "2026-07-22T12:00:00Z"
    braiins["pre_flight"]["restore_verified_utc"] = "2026-07-22T11:00:00Z"
    braiins["pre_flight"]["sd_recovery_verified_utc"] = "2026-07-22T11:30:00Z"
    validate_plan(braiins, now=fixed_now)

    unsafe = copy.deepcopy(fixture_plan())
    unsafe["generated_utc"] = "2026-07-22T12:00:00Z"
    unsafe["pre_flight"]["restore_verified_utc"] = "2026-07-22T11:00:00Z"
    unsafe["pre_flight"]["sd_recovery_verified_utc"] = "2026-07-22T11:30:00Z"
    unsafe["partitions"][0]["artifact"] = "../escape"
    try:
        validate_plan(unsafe, now=fixed_now)
    except ValidationError as error:
        if "safe artifact leaf" not in str(error):
            raise
    else:
        raise AssertionError("unsafe artifact accepted")
    print(f"AM1 NAND backup plan validator self-test passed: {len(scenarios) + 2} scenarios")
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
        layout, minimum, endpoint, target, mac, hwid, model, host_key, rows = validate_plan(plan)
        compatible = plan["pre_flight"]["restore_matched_compatible"]
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("plan_validation=fail", file=sys.stderr)
        return 1
    print(f"layout={layout}")
    print(f"min_free_mb={minimum}")
    print(f"expected_endpoint={endpoint}")
    print(f"expected_target={target}")
    print(f"expected_mac={mac}")
    print(f"expected_hwid={hwid}")
    print(f"expected_model={model}")
    print(f"expected_compatible={compatible}")
    print(f"expected_host_key_sha256={host_key}")
    for row in rows:
        number, name, size, artifact = row.split("|", 3)
        print(f"partition={row}")
        print(f"geometry=mtd{number}:{int(size):08x}:{MTD_ERASE_SIZE_BYTES:08x}:{name}")
        print(f"artifact={artifact}")
    print("plan_validation=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
