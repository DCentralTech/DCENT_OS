#!/usr/bin/env python3
"""Authorize exact Docker resource operations for one release invocation.

This module is deliberately not a Docker client.  It verifies the immutable
release-invocation control stage, derives names and labels from that stage, and
prints declarative command specifications.  A caller remains responsible for
executing Docker and for feeding the resulting bounded inspect JSON back into
this helper before requesting a destruction specification.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sys
from typing import Any, Dict, Iterable, NoReturn, Tuple

import release_invocation as invocation


SPEC_SCHEMA = "org.dcentral.dcentos.release-docker-resource-spec.v1"
VOLUME_LABEL_SCHEMA = "org.dcentral.dcentos.release-docker-volume.v1"
LABEL_SCHEMA = "org.dcentral.dcentos.release-resource.schema"
LABEL_INVOCATION = "org.dcentral.dcentos.release-resource.invocation-id"
LABEL_ROLE = "org.dcentral.dcentos.release-resource.role"
LABEL_DESCRIPTOR = "org.dcentral.dcentos.release-resource.invocation-descriptor-sha256"
ROLES = ("cargo", "buildroot", "results")
MAX_INSPECT_BYTES = 64 * 1024
MAX_SPEC_BYTES = 64 * 1024
MAX_CREATED_AT_BYTES = 256
MAX_MOUNTPOINT_BYTES = 4096
BUILDER_TAG_REPOSITORY = "dcentos-release-builder"


class DockerResourceError(ValueError):
    """A resource request or Docker inspection failed closed."""


def fail(message: str) -> NoReturn:
    raise DockerResourceError(message)


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("ascii")


def _contains_control(value: str) -> bool:
    return any(ord(character) < 0x20 or ord(character) == 0x7F for character in value)


def _safe_string(value: object, label: str, maximum: int) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > maximum
        or _contains_control(value)
    ):
        fail(f"{label} must be a non-empty, bounded string without control characters")
    return value


def _exact_object(value: object, label: str, keys: Iterable[str]) -> Dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{label} must be an object")
    expected = set(keys)
    actual = set(value)
    if actual != expected:
        fail(
            f"{label} has invalid keys "
            f"(missing={sorted(expected - actual)}, extra={sorted(actual - expected)})"
        )
    return value


def _reject_duplicate_object(pairs: Iterable[Tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"JSON input contains duplicate object key {key!r}")
        result[key] = value
    return result


def _reject_nonfinite_constant(value: str) -> NoReturn:
    fail(f"JSON input contains non-finite number {value!r}")


def _valid_digest(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and not (set(value) - frozenset("0123456789abcdef"))
    )


def _verified(stage: pathlib.Path) -> Tuple[invocation.VerifiedInvocation, str]:
    try:
        record = invocation.verify_invocation(stage)
    except (
        invocation.InvocationError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        fail(f"release invocation verification failed: {error}")
    descriptor_raw = invocation.canonical_bytes(record.descriptor)
    descriptor_digest = hashlib.sha256(descriptor_raw).hexdigest()
    if record.state["descriptor_sha256"] != descriptor_digest:
        fail("verified invocation state is not bound to its canonical descriptor")
    return record, descriptor_digest


def _volume(record: invocation.VerifiedInvocation, role: str) -> str:
    if role not in ROLES:
        fail(f"unsupported Docker volume role: {role!r}")
    volumes = record.descriptor["resources"]["docker_volumes"]
    if role not in volumes:
        fail(f"release invocation does not declare the {role!r} Docker volume role")
    name = volumes[role]
    # The invocation verifier already enforces the canonical derivation.  Keep
    # this independent transport check so command construction never trusts a
    # string merely because a future descriptor schema happens to contain it.
    if not isinstance(name, str) or not invocation.RESOURCE_RE.fullmatch(name):
        fail(f"release invocation {role!r} Docker volume name is unsafe")
    return name


def _labels(
    record: invocation.VerifiedInvocation, descriptor_digest: str, role: str
) -> Dict[str, str]:
    return {
        LABEL_DESCRIPTOR: descriptor_digest,
        LABEL_INVOCATION: record.descriptor["invocation_id"],
        LABEL_ROLE: role,
        LABEL_SCHEMA: VOLUME_LABEL_SCHEMA,
    }


def create_volume_spec(stage: pathlib.Path, role: str) -> Dict[str, Any]:
    record, descriptor_digest = _verified(stage)
    name = _volume(record, role)
    labels = _labels(record, descriptor_digest, role)
    argv = ["docker", "volume", "create", "--driver", "local"]
    for key in sorted(labels):
        argv.extend(("--label", f"{key}={labels[key]}"))
    argv.extend(("--", name))
    return {
        "argv": argv,
        "descriptor_sha256": descriptor_digest,
        "invocation_id": record.descriptor["invocation_id"],
        "labels": labels,
        "name": name,
        "operation": "create-volume",
        "role": role,
        "schema": SPEC_SCHEMA,
    }


def inspect_volume_spec(stage: pathlib.Path, role: str) -> Dict[str, Any]:
    record, descriptor_digest = _verified(stage)
    name = _volume(record, role)
    return {
        "argv": ["docker", "volume", "inspect", "--", name],
        "descriptor_sha256": descriptor_digest,
        "expected_labels": _labels(record, descriptor_digest, role),
        "expected_name": name,
        "operation": "inspect-volume",
        "role": role,
        "schema": SPEC_SCHEMA,
    }


def _validate_mountpoint(value: object, name: str) -> str:
    mountpoint = _safe_string(value, "Docker volume Mountpoint", MAX_MOUNTPOINT_BYTES)
    if not mountpoint.startswith("/") or mountpoint.startswith("//"):
        fail("Docker volume Mountpoint must be a canonical absolute POSIX path")
    if "\\" in mountpoint or mountpoint.endswith("/"):
        fail("Docker volume Mountpoint contains an ambiguous path spelling")
    components = mountpoint.split("/")
    if components[0] != "" or any(
        not component or component in (".", "..") for component in components[1:]
    ):
        fail(
            "Docker volume Mountpoint contains a symlink-like or non-canonical component"
        )
    if len(components) < 5 or components[-3:] != ["volumes", name, "_data"]:
        fail("Docker volume Mountpoint is not bound to the exact inspected volume name")
    return mountpoint


def parse_inspect(raw: bytes) -> Dict[str, Any]:
    if len(raw) > MAX_INSPECT_BYTES:
        fail(f"Docker volume inspect JSON exceeds {MAX_INSPECT_BYTES} bytes")
    if not raw:
        fail("Docker volume inspect JSON is empty")
    try:
        value = json.loads(
            raw,
            object_pairs_hook=_reject_duplicate_object,
            parse_constant=_reject_nonfinite_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        fail(f"Docker volume inspect input is not valid JSON: {error}")
    if not isinstance(value, list) or len(value) != 1:
        fail("Docker volume inspect input must contain exactly one volume object")
    return _exact_object(
        value[0],
        "Docker volume inspect object",
        ("CreatedAt", "Driver", "Labels", "Mountpoint", "Name", "Options", "Scope"),
    )


def verify_inspect(
    stage: pathlib.Path, role: str, raw: bytes
) -> Tuple[Dict[str, Any], Dict[str, Any]]:
    record, descriptor_digest = _verified(stage)
    expected_name = _volume(record, role)
    expected_labels = _labels(record, descriptor_digest, role)
    inspected = parse_inspect(raw)
    if inspected["Name"] != expected_name:
        fail("Docker volume inspect Name does not match the invocation role")
    if inspected["Labels"] != expected_labels:
        fail(
            "Docker volume inspect Labels are not the exact invocation authority labels"
        )
    if inspected["Driver"] != "local" or inspected["Scope"] != "local":
        fail("Docker volume must use the local driver and local scope")
    if inspected["Options"] not in (None, {}):
        fail("Docker volume has local-driver options and may alias external storage")
    created_at = _safe_string(
        inspected["CreatedAt"], "Docker volume CreatedAt", MAX_CREATED_AT_BYTES
    )
    mountpoint = _validate_mountpoint(inspected["Mountpoint"], expected_name)
    inspect_digest = hashlib.sha256(canonical_bytes([inspected])).hexdigest()
    decision = {
        "allowed": True,
        "created_at": created_at,
        "descriptor_sha256": descriptor_digest,
        "inspect_sha256": inspect_digest,
        "invocation_id": record.descriptor["invocation_id"],
        "mountpoint": mountpoint,
        "mountpoint_assurance": "canonical-local-volume-suffix;filesystem-links-not-attested",
        "name": expected_name,
        "operation": "verified-volume-inspect",
        "role": role,
        "schema": SPEC_SCHEMA,
    }
    return decision, inspected


def destroy_volume_spec(
    stage: pathlib.Path,
    capability: pathlib.Path,
    role: str,
    raw: bytes,
    cleanup_state: str,
) -> Dict[str, Any]:
    if cleanup_state not in ("empty", "disposable"):
        fail("volume cleanup state must be explicitly 'empty' or 'disposable'")
    decision, _inspected = verify_inspect(stage, role, raw)
    record, descriptor_digest = _verified(stage)
    try:
        invocation.verify_capability(record.stage, capability, record)
    except (
        invocation.InvocationError,
        OSError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        fail(f"release invocation cleanup capability verification failed: {error}")
    name = _volume(record, role)
    if decision["descriptor_sha256"] != descriptor_digest or decision["name"] != name:
        fail(
            "Docker volume inspection decision changed before destruction authorization"
        )
    return {
        "argv": ["docker", "volume", "rm", "--", name],
        "cleanup_state": cleanup_state,
        "descriptor_sha256": descriptor_digest,
        "inspect_sha256": decision["inspect_sha256"],
        "invocation_id": record.descriptor["invocation_id"],
        "name": name,
        "operation": "destroy-volume",
        "role": role,
        "schema": SPEC_SCHEMA,
        "verified_inspect": True,
    }


def builder_tag_spec(stage: pathlib.Path) -> Dict[str, Any]:
    record, descriptor_digest = _verified(stage)
    invocation_id = record.descriptor["invocation_id"]
    tag = f"{BUILDER_TAG_REPOSITORY}:{invocation_id}"
    return {
        "descriptor_sha256": descriptor_digest,
        "forbidden_removal_targets": ["image-id", "shared-layer"],
        "invocation_id": invocation_id,
        "operation": "builder-image-tag-identity",
        "removal_authority": {
            "exact_tag_only": tag,
            "requires_independent_retained_image_reference": True,
        },
        "schema": SPEC_SCHEMA,
        "tag": tag,
    }


def _parse_canonical_spec(raw: bytes) -> Dict[str, Any]:
    if len(raw) > MAX_SPEC_BYTES:
        fail(f"release Docker resource specification exceeds {MAX_SPEC_BYTES} bytes")
    if not raw:
        fail("release Docker resource specification is empty")
    try:
        value = json.loads(
            raw,
            object_pairs_hook=_reject_duplicate_object,
            parse_constant=_reject_nonfinite_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        fail(f"release Docker resource specification is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail("release Docker resource specification must be an object")
    if raw != canonical_bytes(value):
        fail("release Docker resource specification is not canonical JSON")
    return value


def _validate_destroy_emit_spec(
    stage: pathlib.Path, value: Dict[str, Any]
) -> Dict[str, Any]:
    spec = _exact_object(
        value,
        "destroy-volume specification",
        (
            "argv",
            "cleanup_state",
            "descriptor_sha256",
            "inspect_sha256",
            "invocation_id",
            "name",
            "operation",
            "role",
            "schema",
            "verified_inspect",
        ),
    )
    record, descriptor_digest = _verified(stage)
    role = spec["role"]
    name = _volume(record, role)
    if spec["schema"] != SPEC_SCHEMA or spec["operation"] != "destroy-volume":
        fail("destroy-volume specification schema or operation is invalid")
    if spec["cleanup_state"] not in ("empty", "disposable"):
        fail("destroy-volume specification lacks an explicit cleanup state")
    if spec["descriptor_sha256"] != descriptor_digest:
        fail("destroy-volume specification descriptor binding is invalid")
    if spec["invocation_id"] != record.descriptor["invocation_id"]:
        fail("destroy-volume specification invocation binding is invalid")
    if spec["name"] != name:
        fail("destroy-volume specification name is invalid")
    if not _valid_digest(spec["inspect_sha256"]):
        fail("destroy-volume specification inspect digest is invalid")
    if spec["verified_inspect"] is not True:
        fail("destroy-volume specification lacks verified inspect state")
    if spec["argv"] != ["docker", "volume", "rm", "--", name]:
        fail("destroy-volume specification argv is not canonically derived")
    return spec


def validate_emit_spec(stage: pathlib.Path, raw: bytes) -> Dict[str, Any]:
    value = _parse_canonical_spec(raw)
    if value.get("schema") != SPEC_SCHEMA:
        fail("unsupported release Docker resource specification schema")
    operation = value.get("operation")
    if operation == "create-volume":
        role = value.get("role")
        expected = create_volume_spec(stage, role)
        if value != expected:
            fail("create-volume specification is not the exact descriptor-derived form")
        spec = value
    elif operation == "inspect-volume":
        role = value.get("role")
        expected = inspect_volume_spec(stage, role)
        if value != expected:
            fail(
                "inspect-volume specification is not the exact descriptor-derived form"
            )
        spec = value
    elif operation == "destroy-volume":
        spec = _validate_destroy_emit_spec(stage, value)
    else:
        fail("release Docker resource specification operation is not executable")
    argv = spec.get("argv")
    if (
        not isinstance(argv, list)
        or not argv
        or any(
            not isinstance(argument, str) or not argument or _contains_control(argument)
            for argument in argv
        )
    ):
        fail("release Docker resource argv is unsafe for NUL-delimited transport")
    return spec


def emit_argv0(stage: pathlib.Path, raw: bytes) -> bytes:
    spec = validate_emit_spec(stage, raw)
    return b"".join(argument.encode("ascii") + b"\0" for argument in spec["argv"])


def _read_stdin() -> bytes:
    raw = sys.stdin.buffer.read(MAX_INSPECT_BYTES + 1)
    if len(raw) > MAX_INSPECT_BYTES:
        fail(f"Docker volume inspect JSON exceeds {MAX_INSPECT_BYTES} bytes")
    return raw


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)

    for name, help_text in (
        ("create-spec", "print an exact Docker volume creation specification"),
        ("inspect-spec", "print an exact Docker volume inspection specification"),
        ("verify-inspect", "validate one bounded Docker volume inspect response"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("--role", required=True, choices=ROLES)
        command.add_argument("stage")

    destroy = commands.add_parser(
        "destroy-spec",
        help="authorize exact removal after inspection and cleanup proof",
    )
    destroy.add_argument("--role", required=True, choices=ROLES)
    destroy.add_argument("--capability", required=True)
    destroy.add_argument(
        "--empty-or-disposable", required=True, choices=("empty", "disposable")
    )
    destroy.add_argument("stage")

    tag = commands.add_parser(
        "builder-tag-spec",
        help="derive the invocation-unique builder image tag identity",
    )
    tag.add_argument("stage")

    emit = commands.add_parser(
        "emit-argv0",
        help="revalidate one canonical spec and emit NUL-delimited Docker argv",
    )
    emit.add_argument("stage")

    query_tag = commands.add_parser(
        "query-builder-tag", help="print the strictly derived builder image tag scalar"
    )
    query_tag.add_argument("stage")
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        stage = pathlib.Path(args.stage)
        if args.command == "create-spec":
            result = create_volume_spec(stage, args.role)
        elif args.command == "inspect-spec":
            result = inspect_volume_spec(stage, args.role)
        elif args.command == "verify-inspect":
            result, _inspected = verify_inspect(stage, args.role, _read_stdin())
        elif args.command == "destroy-spec":
            result = destroy_volume_spec(
                stage,
                pathlib.Path(args.capability),
                args.role,
                _read_stdin(),
                args.empty_or_disposable,
            )
        elif args.command == "emit-argv0":
            sys.stdout.buffer.write(
                emit_argv0(stage, sys.stdin.buffer.read(MAX_SPEC_BYTES + 1))
            )
            return 0
        elif args.command == "query-builder-tag":
            tag = builder_tag_spec(stage)["tag"]
            _safe_string(tag, "builder image tag", 255)
            print(tag)
            return 0
        else:
            result = builder_tag_spec(stage)
        sys.stdout.buffer.write(canonical_bytes(result))
        return 0
    except (DockerResourceError, OSError, KeyError, TypeError, ValueError) as error:
        print(f"ERROR: release Docker resource authority: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
