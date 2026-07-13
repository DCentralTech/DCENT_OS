#!/usr/bin/env python3
"""Derive the release-capsule lineage identity from verified local records.

This module deliberately accepts paths to independently verified authorities,
not caller-supplied snapshot or invocation identifiers.  The resulting object
binds an invocation to one Git-authenticated source snapshot.  It does not
claim that a build ran, consumed the snapshot, or is reproducible.
"""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import re
import sys


SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIRECTORY))

import release_invocation  # noqa: E402
import source_snapshot  # noqa: E402


SCHEMA = "org.dcentral.dcentos.release-capsule-lineage.v2"
CAPSULE_KEYS = (
    "release_invocation_descriptor_sha256",
    "release_invocation_id",
    "schema",
    "source_snapshot_descriptor_sha256",
    "source_snapshot_id",
)
DIGEST_RE = re.compile(r"[0-9a-f]{64}")


class CapsuleLineageError(ValueError):
    """A release-capsule lineage record is malformed or cannot be verified."""


@dataclass(frozen=True)
class VerifiedCapsuleLineage:
    capsule: dict[str, str]
    source_tree: Path
    source_commit: str
    invocation_stage: Path


def _require_digest(value: object, label: str) -> str:
    if not isinstance(value, str) or DIGEST_RE.fullmatch(value) is None:
        raise CapsuleLineageError(f"{label} must be 256 bits of lowercase hexadecimal")
    return value


def validate_release_capsule(value: object) -> dict[str, str]:
    """Validate and return one exact canonical release-capsule object."""

    if not isinstance(value, dict) or set(value) != set(CAPSULE_KEYS):
        raise CapsuleLineageError(
            "release_capsule must contain exactly: " + ", ".join(CAPSULE_KEYS)
        )
    if value.get("schema") != SCHEMA:
        raise CapsuleLineageError("release_capsule has an unsupported schema")
    return {
        "schema": SCHEMA,
        "release_invocation_descriptor_sha256": _require_digest(
            value.get("release_invocation_descriptor_sha256"),
            "release invocation descriptor sha256",
        ),
        "release_invocation_id": _require_digest(
            value.get("release_invocation_id"), "release invocation id"
        ),
        "source_snapshot_id": _require_digest(
            value.get("source_snapshot_id"), "source snapshot id"
        ),
        "source_snapshot_descriptor_sha256": _require_digest(
            value.get("source_snapshot_descriptor_sha256"),
            "source snapshot descriptor sha256",
        ),
    }


def verify_release_capsule_lineage(
    git_object_repo: Path,
    snapshot_descriptor: Path,
    expected_commit: str,
    invocation_stage: Path,
) -> VerifiedCapsuleLineage:
    """Derive lineage only after authenticating both independent authorities."""

    if not isinstance(expected_commit, str) or not expected_commit:
        raise CapsuleLineageError("expected source commit must be an exact object id")
    try:
        snapshot = source_snapshot.verify_against_git(
            Path(git_object_repo), expected_commit, Path(snapshot_descriptor)
        )
    except (source_snapshot.SnapshotError, OSError, ValueError) as error:
        raise CapsuleLineageError(
            f"source snapshot verification failed: {error}"
        ) from error
    try:
        invocation = release_invocation.verify_invocation(Path(invocation_stage))
    except (release_invocation.InvocationError, OSError, ValueError) as error:
        raise CapsuleLineageError(
            f"release invocation verification failed: {error}"
        ) from error

    capsule = validate_release_capsule(
        {
            "schema": SCHEMA,
            "release_invocation_descriptor_sha256": release_invocation.sha256_bytes(
                release_invocation.canonical_bytes(invocation.descriptor)
            ),
            "release_invocation_id": invocation.descriptor["invocation_id"],
            "source_snapshot_id": snapshot["snapshot_id"],
            "source_snapshot_descriptor_sha256": snapshot["descriptor_sha256"],
        }
    )
    return VerifiedCapsuleLineage(
        capsule=capsule,
        source_tree=Path(snapshot["tree"]),
        source_commit=snapshot["commit_oid"],
        invocation_stage=invocation.stage,
    )


def derive_release_capsule(
    git_object_repo: Path,
    snapshot_descriptor: Path,
    expected_commit: str,
    invocation_stage: Path,
) -> dict[str, str]:
    """Return the exact canonical capsule dictionary for receipt embedding."""

    return verify_release_capsule_lineage(
        git_object_repo, snapshot_descriptor, expected_commit, invocation_stage
    ).capsule
