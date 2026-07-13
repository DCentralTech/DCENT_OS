#!/usr/bin/env python3
"""Canonical release-capsule admission policy.

This module is deliberately small: every outer capsule consumer must derive
its Cargo variant, primary artifact, package board, and signed target identity
from one exact record.  Build-driver support alone does not grant release
admission.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import datetime as dt
import re
import sys
from types import MappingProxyType
from typing import NoReturn


@dataclass(frozen=True)
class ReleaseCapsuleTargetPolicy:
    target: str
    cargo_variant: str
    primary_artifact: str
    package_board: str
    release_stem: str
    publication_admitted: bool


POLICIES = {
    "s9": ReleaseCapsuleTargetPolicy(
        target="s9",
        cargo_variant="zynq",
        primary_artifact="dcentos-unit.tar",
        package_board="am1-s9",
        release_stem="DCENTOS_XIL1_S9",
        publication_admitted=True,
    ),
    "am2-s19jpro": ReleaseCapsuleTargetPolicy(
        target="am2-s19jpro",
        cargo_variant="zynq",
        primary_artifact="dcentos-sysupgrade-am2-s19jpro.tar",
        package_board="am2-s19j",
        release_stem="DCENTOS_XIL3_S19jPro",
        publication_admitted=False,
    ),
}

# Portable evidence schemas are long-lived verification contracts.  Adding or
# changing current policy requires a new evidence schema rather than silently
# reinterpreting already-signed v2 indexes.
PORTABLE_EVIDENCE_V2_POLICIES = MappingProxyType(dict(POLICIES))

BLOCKED_TARGETS = {
    "am2-s19jpro-sd": (
        "AM2 SD is incomplete: boot artifacts, completeness enforcement, "
        "rootfs/package validation, and a collision-free release identity are missing"
    ),
}

QUERY_FIELDS = frozenset(
    (
        "target",
        "cargo_variant",
        "primary_artifact",
        "package_board",
        "release_stem",
        "publication_admitted",
    )
)

# Frozen verification semantics for already-published v1 evidence.  Do not
# derive historical meaning from POLICIES: current target policy is allowed to
# evolve without invalidating old signatures.
HISTORICAL_V1_S9_POLICY = ReleaseCapsuleTargetPolicy(
    target="s9",
    cargo_variant="zynq",
    primary_artifact="dcentos-unit.tar",
    package_board="am1-s9",
    release_stem="DCENTOS_XIL1_S9",
    publication_admitted=True,
)


class TargetPolicyError(ValueError):
    """A target has no complete, canonical release-capsule policy."""


def fail(message: str) -> NoReturn:
    raise TargetPolicyError(message)


def policy_for(
    target: object, *, require_publication: bool = False
) -> ReleaseCapsuleTargetPolicy:
    if not isinstance(target, str) or not target:
        fail("release capsule target must be a non-empty string")
    if target in BLOCKED_TARGETS:
        fail(f"release capsule target {target!r} is blocked: {BLOCKED_TARGETS[target]}")
    try:
        result = POLICIES[target]
    except KeyError:
        fail(f"release capsule target {target!r} is not admitted")
    if require_publication and not result.publication_admitted:
        fail(
            f"release capsule target {target!r} has evidence policy but no "
            "admitted outer publication lifecycle"
        )
    return result


def portable_v2_policy_for(target: object) -> ReleaseCapsuleTargetPolicy:
    if not isinstance(target, str) or not target:
        fail("portable v2 target must be a non-empty string")
    try:
        return PORTABLE_EVIDENCE_V2_POLICIES[target]
    except KeyError:
        fail(f"portable v2 target {target!r} is not admitted by its frozen schema")


def validate_output_name(
    target_policy: ReleaseCapsuleTargetPolicy, output_name: object
) -> str:
    if not isinstance(output_name, str):
        fail("release output name must be a string")
    match = re.fullmatch(
        re.escape(target_policy.release_stem)
        + r"_(beta|dev|rc|stable)([0-9]{8})",
        output_name,
    )
    if match is None:
        fail("release output name disagrees with target release identity")
    try:
        dt.datetime.strptime(match.group(2), "%Y%m%d")
    except ValueError:
        fail("release output name contains an invalid calendar date")
    return output_name


def query(target: str, field: str) -> str:
    if field not in QUERY_FIELDS:
        fail(f"unknown release capsule policy field: {field!r}")
    value = getattr(policy_for(target), field)
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    top.add_argument("target")
    top.add_argument("field", choices=sorted(QUERY_FIELDS))
    top.add_argument(
        "--require-publication",
        action="store_true",
        help="refuse evidence-ready targets without an outer publication lifecycle",
    )
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        result = policy_for(args.target, require_publication=args.require_publication)
        value = getattr(result, args.field)
        print("true" if value is True else "false" if value is False else value)
        return 0
    except TargetPolicyError as error:
        print(f"ERROR: release capsule target policy: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
