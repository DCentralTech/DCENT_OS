#!/usr/bin/env python3
"""Create and verify a signed, portable post-cleanup release audit index.

The signed index authenticates every retained payload and projection except its
own detached signature.  The final release-set descriptor must contain exactly
the signed payload set plus this canonical index and its 64-byte signature.
The trusted Ed25519 public key is deliberately out of band.

This is byte/projection/source-consistency evidence.  It is not build execution
attestation, compiler causality, reproducibility, installed-payload equivalence,
boot proof, or mining proof.  Build admission continues to require live stages.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, Iterable, NoReturn


SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIRECTORY))

import build_input_snapshot  # noqa: E402
import release_capsule_lineage  # noqa: E402
import release_capsule_target_policy  # noqa: E402
import release_invocation  # noqa: E402
import release_result_stage  # noqa: E402
import release_set_publication  # noqa: E402
import source_closure  # noqa: E402
import source_snapshot  # noqa: E402


SCHEMA = "org.dcentral.dcentos.portable-release-evidence.v2"
HISTORICAL_SCHEMA = "org.dcentral.dcentos.portable-release-evidence.v1"
INDEX_NAME = "portable-release-evidence.json"
SIGNATURE_NAME = f"{INDEX_NAME}.sig"
SOURCE_NAME = "release-source-snapshot.json"
INVOCATION_NAME = "release-invocation.json"
CARGO_INPUT_NAME = "release-cargo-input.json"
PACKAGING_INPUT_NAME = "release-packaging-input.json"
RESULT_NAME = "release-result-audit.json"
MAX_FILE_BYTES = 512 * 1024 * 1024
MAX_INDEX_BYTES = 4 * 1024 * 1024
MAX_EMBEDDED_MANIFEST_BYTES = 256 * 1024
HEX = frozenset("0123456789abcdef")
CLAIM = (
    "signed-exact-published-bytes-and-retained-audit-projection-consistency-"
    "not-live-authority-build-causality-or-reproducibility-proof"
)
NON_CLAIMS = (
    "live-private-stage-authority-or-capability",
    "build-execution-or-compiler-consumption",
    "compiler-toolchain-or-container-trust",
    "reproducibility-or-installed-payload-equivalence",
    "boot-runtime-mining-or-hardware-correctness",
)


class PortableEvidenceError(ValueError):
    """Portable release evidence is malformed or inconsistent."""


def fail(message: str) -> NoReturn:
    raise PortableEvidenceError(message)


def canonical_bytes(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("ascii")


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def is_digest(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and not (set(value) - HEX)


def exact_object(value: object, label: str, keys: Iterable[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(keys):
        fail(f"{label} does not have its exact canonical fields")
    return value


def safe_flat_name(value: object, label: str) -> str:
    try:
        return release_set_publication.validate_flat_name(value, label)
    except release_set_publication.ReleaseSetError as error:
        fail(str(error))


def read_regular(path_value: Path, label: str, maximum: int = MAX_FILE_BYTES) -> bytes:
    path = Path(os.path.abspath(os.fspath(path_value)))
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or release_set_publication.is_reparse(metadata)
        or getattr(metadata, "st_nlink", 1) != 1
        or metadata.st_size > maximum
    ):
        fail(f"{label} must be a bounded single-link non-reparse regular file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    handle = os.open(path, flags)
    try:
        before = os.fstat(handle)
        chunks: list[bytes] = []
        size = 0
        while size <= maximum:
            chunk = os.read(handle, min(1024 * 1024, maximum + 1 - size))
            if not chunk:
                break
            chunks.append(chunk)
            size += len(chunk)
        after = os.fstat(handle)
    finally:
        os.close(handle)
    current = os.lstat(path)
    if (
        size > maximum
        or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino)
    ):
        fail(f"{label} changed while being read")
    return b"".join(chunks)


def read_canonical(
    path: Path, label: str, maximum: int = MAX_INDEX_BYTES
) -> dict[str, Any]:
    raw = read_regular(path, label, maximum)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is invalid JSON: {error}")
    if not isinstance(value, dict) or raw != canonical_bytes(value):
        fail(f"{label} is not a canonical JSON object")
    return value


def evidence(path: Path) -> dict[str, object]:
    raw = read_regular(path, f"release payload {path.name}")
    return {"name": path.name, "sha256": digest(raw), "size": len(raw)}


def write_exclusive(path: Path, raw: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
    handle = os.open(path, flags, 0o644)
    try:
        with os.fdopen(handle, "wb", closefd=True) as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise
    if os.name == "posix":
        os.chmod(path, 0o644, follow_symlinks=False)


def copy_projection(source: Path, destination: Path, label: str) -> dict[str, Any]:
    value = read_canonical(source, label)
    write_exclusive(destination, canonical_bytes(value))
    return value


def verify_signature(
    public_key: Path, content: Path, signature: Path, label: str
) -> None:
    signature_raw = read_regular(signature, f"{label} signature", 64)
    if len(signature_raw) != 64:
        fail(f"{label} Ed25519 signature must be exactly 64 bytes")
    read_regular(public_key, "trusted Ed25519 public key", 64 * 1024)
    try:
        process = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                str(public_key),
                "-sigfile",
                str(signature),
                "-in",
                str(content),
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
        )
    except FileNotFoundError as error:
        fail(f"{label} verification requires OpenSSL: {error}")
    except subprocess.TimeoutExpired:
        fail(f"{label} OpenSSL verification exceeded 15 seconds")
    if process.returncode != 0:
        fail(f"{label} detached Ed25519 signature verification failed")


@contextlib.contextmanager
def pinned_public_key(path: Path):
    raw = read_regular(path, "trusted Ed25519 public key", 64 * 1024)
    with tempfile.TemporaryDirectory(prefix="dcent-trusted-release-key-") as temporary:
        snapshot = Path(temporary) / "release.pub"
        write_exclusive(snapshot, raw)
        if os.name == "posix":
            os.chmod(snapshot, 0o400, follow_symlinks=False)
        yield snapshot, raw


def projection_record(directory: Path, name: str) -> dict[str, object]:
    return evidence(directory / name)


def create_live(args: argparse.Namespace) -> None:
    with pinned_public_key(Path(args.public_key)) as (public_key, public_key_raw):
        create_live_with_key(args, public_key, public_key_raw)


def create_live_with_key(
    args: argparse.Namespace, public_key: Path, public_key_raw: bytes
) -> None:
    target_policy = policy_for_schema(SCHEMA, args.target)
    output_name = release_capsule_target_policy.validate_output_name(
        target_policy, args.output_name
    )
    directory, _ = release_set_publication.safe_directory(
        args.artifact_dir, "private release-set stage"
    )
    for name in (
        INDEX_NAME,
        SIGNATURE_NAME,
        SOURCE_NAME,
        INVOCATION_NAME,
        CARGO_INPUT_NAME,
        RESULT_NAME,
    ):
        if (directory / name).exists():
            fail(f"portable evidence output already exists: {name}")
    # build_in_docker.sh deliberately creates this one audit-only handoff after
    # live packaging-input admission and before destroying the restricted-byte
    # stage. It must already exist; create-live never replaces or regenerates it.
    if not (directory / PACKAGING_INPUT_NAME).is_file():
        fail("packaging input audit projection handoff is missing")

    try:
        source_verified = source_snapshot.verify_against_git(
            Path(args.repo_root), args.source_commit, Path(args.source_snapshot)
        )
        invocation = release_invocation.verify_invocation(Path(args.release_invocation))
        capsule = release_capsule_lineage.derive_release_capsule(
            Path(args.repo_root),
            Path(args.source_snapshot),
            args.source_commit,
            Path(args.release_invocation),
        )
        cargo = build_input_snapshot.verify_snapshot(
            Path(args.cargo_input_snapshot), "cargo-workspace"
        )
        if cargo.get("schema") != build_input_snapshot.SPLIT_AUTHORITY_SCHEMA:
            fail("Cargo release input authority must use split-authority schema v2")
        result = release_result_stage.verify_result_stage(
            Path(args.result_stage), Path(args.release_invocation)
        )
    except (
        source_snapshot.SnapshotError,
        release_invocation.InvocationError,
        release_capsule_lineage.CapsuleLineageError,
        build_input_snapshot.SnapshotError,
        release_result_stage.ResultStageError,
        OSError,
        ValueError,
    ) as error:
        fail(f"live release authority verification failed: {error}")
    if result.descriptor["state"] != "sealed":
        fail("live Cargo result stage must be sealed before projection")

    packaging_path = directory / PACKAGING_INPUT_NAME
    packaging = build_input_snapshot.verify_audit_descriptor(
        packaging_path, target_policy.target
    )
    if packaging.get("schema") != build_input_snapshot.SPLIT_AUTHORITY_SCHEMA:
        fail("packaging input projection must use split-authority schema v2")

    closure = Path(args.closure)
    closure_signature = Path(args.closure_signature)
    if closure.parent != directory or closure_signature.parent != directory:
        fail("source closure and signature must be flat members of the release set")
    verify_signature(
        public_key, closure, closure_signature, "source closure"
    )

    source_value = read_canonical(Path(args.source_snapshot), "live source descriptor")
    if digest(canonical_bytes(source_value)) != source_verified["descriptor_sha256"]:
        fail("live source descriptor digest changed after Git verification")
    write_exclusive(directory / SOURCE_NAME, canonical_bytes(source_value))
    write_exclusive(
        directory / INVOCATION_NAME,
        release_invocation.canonical_bytes(invocation.descriptor),
    )
    write_exclusive(
        directory / CARGO_INPUT_NAME, build_input_snapshot.canonical_bytes(cargo)
    )
    write_exclusive(
        directory / RESULT_NAME,
        release_result_stage.canonical_bytes(
            release_result_stage.audit_projection(result)
        ),
    )

    names = set()
    payload_files: list[dict[str, object]] = []
    with os.scandir(directory) as entries:
        for entry in entries:
            name = entry.name
            if name == release_set_publication.DESCRIPTOR_NAME:
                continue
            if name in (INDEX_NAME, SIGNATURE_NAME):
                fail("portable evidence index/signature appeared during creation")
            safe_flat_name(name, "release payload name")
            if name.casefold() in names:
                fail("release payload names collide on case-insensitive hosts")
            names.add(name.casefold())
            payload_files.append(evidence(directory / name))
    payload_files.sort(key=lambda item: str(item["name"]).encode("utf-8"))

    projections = {
        "cargo_input": projection_record(directory, CARGO_INPUT_NAME),
        "invocation": projection_record(directory, INVOCATION_NAME),
        "packaging_input": projection_record(directory, PACKAGING_INPUT_NAME),
        "result": projection_record(directory, RESULT_NAME),
        "source": projection_record(directory, SOURCE_NAME),
    }
    index = {
        "schema": SCHEMA,
        "target": target_policy.target,
        "output_name": output_name,
        "claim": CLAIM,
        "scope": {"does_not_claim": list(NON_CLAIMS)},
        "release_capsule": capsule,
        "source_commit": args.source_commit,
        "source_closure": evidence(closure),
        "source_closure_signature": evidence(closure_signature),
        "projections": projections,
        "payload_files": payload_files,
        "signature_convention": (
            "payload_files excludes exactly portable-release-evidence.json, "
            "portable-release-evidence.json.sig, and .dcent-release-set.json; "
            "the final sealed set requires all three fixed members"
        ),
    }
    if (
        index["release_capsule"]["release_invocation_id"]
        != invocation.descriptor["invocation_id"]
    ):
        fail("portable evidence capsule disagrees with live invocation")
    closure_value = source_closure.validate_receipt_schema(
        read_canonical(closure, "source closure")
    )
    verify_target_bindings(
        SCHEMA,
        target_policy.target,
        invocation.descriptor,
        packaging,
        closure_value,
    )
    verify_target_artifact(
        SCHEMA,
        target_policy.target,
        closure_value,
        directory,
        payload_files,
        public_key,
        public_key_raw,
    )
    validate_index(index)
    write_exclusive(directory / INDEX_NAME, canonical_bytes(index))
    print(directory / INDEX_NAME)


def validate_file_record(value: object, label: str) -> dict[str, object]:
    record = exact_object(value, label, ("name", "sha256", "size"))
    safe_flat_name(record["name"], f"{label} name")
    if not is_digest(record["sha256"]):
        fail(f"{label} digest is invalid")
    if (
        isinstance(record["size"], bool)
        or not isinstance(record["size"], int)
        or record["size"] < 0
    ):
        fail(f"{label} size is invalid")
    return record


def policy_for_schema(
    schema: str, target: object
) -> release_capsule_target_policy.ReleaseCapsuleTargetPolicy:
    if schema == HISTORICAL_SCHEMA:
        if target != "s9":
            fail("historical portable evidence has a fixed S9 identity")
        return release_capsule_target_policy.HISTORICAL_V1_S9_POLICY
    if schema == SCHEMA:
        return release_capsule_target_policy.portable_v2_policy_for(target)
    fail("portable evidence schema is unsupported")


def validate_index(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail("portable evidence index must be an object")
    schema = value.get("schema")
    if schema not in (SCHEMA, HISTORICAL_SCHEMA):
        fail("portable evidence schema is unsupported")
    keys = (
        (
            "schema",
            "target",
            "output_name",
            "claim",
            "scope",
            "release_capsule",
            "source_commit",
            "source_closure",
            "source_closure_signature",
            "projections",
            "payload_files",
            "signature_convention",
        )
        if schema == SCHEMA
        else (
            "schema",
            "claim",
            "scope",
            "release_capsule",
            "source_commit",
            "source_closure",
            "source_closure_signature",
            "projections",
            "payload_files",
            "signature_convention",
        )
    )
    index = exact_object(
        value,
        "portable evidence index",
        keys,
    )
    if index["claim"] != CLAIM:
        fail("portable evidence claim is invalid")
    target = "s9" if schema == HISTORICAL_SCHEMA else index["target"]
    target_policy = policy_for_schema(schema, target)
    if schema == SCHEMA:
        release_capsule_target_policy.validate_output_name(
            target_policy, index["output_name"]
        )
    if index["scope"] != {"does_not_claim": list(NON_CLAIMS)}:
        fail("portable evidence claim scope is invalid or overstated")
    release_capsule_lineage.validate_release_capsule(index["release_capsule"])
    if not isinstance(index["source_commit"], str) or len(
        index["source_commit"]
    ) not in (40, 64):
        fail("portable evidence source commit is invalid")
    validate_file_record(index["source_closure"], "source closure")
    validate_file_record(index["source_closure_signature"], "source closure signature")
    projections = exact_object(
        index["projections"],
        "portable projections",
        ("cargo_input", "invocation", "packaging_input", "result", "source"),
    )
    expected_projection_names = {
        "cargo_input": CARGO_INPUT_NAME,
        "invocation": INVOCATION_NAME,
        "packaging_input": PACKAGING_INPUT_NAME,
        "result": RESULT_NAME,
        "source": SOURCE_NAME,
    }
    for key, name in expected_projection_names.items():
        record = validate_file_record(projections[key], f"{key} projection")
        if record["name"] != name:
            fail(f"{key} projection uses the wrong fixed name")
    payload = index["payload_files"]
    if not isinstance(payload, list) or not payload:
        fail("portable evidence payload set is empty")
    validated = [
        validate_file_record(item, f"payload file {position}")
        for position, item in enumerate(payload)
    ]
    names = [str(item["name"]) for item in validated]
    if names != sorted(names, key=lambda name: name.encode("utf-8")) or len(
        names
    ) != len(set(name.casefold() for name in names)):
        fail("portable evidence payload set is not canonical or unique")
    if target_policy.primary_artifact not in names:
        fail("portable evidence payload set lacks the target's primary artifact")
    known_primary_artifacts = (
        {
            release_capsule_target_policy.HISTORICAL_V1_S9_POLICY.primary_artifact
        }
        if schema == HISTORICAL_SCHEMA
        else {
            record.primary_artifact
            for record in release_capsule_target_policy.PORTABLE_EVIDENCE_V2_POLICIES.values()
        }
    )
    if set(names) & known_primary_artifacts != {target_policy.primary_artifact}:
        fail("portable evidence payload set has a conflicting target artifact")
    if (
        INDEX_NAME in names
        or SIGNATURE_NAME in names
        or release_set_publication.DESCRIPTOR_NAME in names
    ):
        fail(
            "portable evidence payload set violates its exact self-exclusion convention"
        )
    convention = (
        "payload_files excludes exactly portable-release-evidence.json, "
        "portable-release-evidence.json.sig, and .dcent-release-set.json; "
        "the final sealed set requires all three fixed members"
    )
    if index["signature_convention"] != convention:
        fail("portable evidence signature convention is invalid")
    if schema == HISTORICAL_SCHEMA:
        index = dict(index)
        index["target"] = "s9"
    return index


def verify_target_bindings(
    schema: str,
    target: str,
    invocation: dict[str, Any],
    packaging: dict[str, Any],
    closure: dict[str, Any],
) -> None:
    target_policy = policy_for_schema(schema, target)
    if invocation.get("logical_name") != target_policy.target:
        fail("release invocation logical name disagrees with portable target")
    if packaging.get("target") != target_policy.target:
        fail("packaging input projection disagrees with portable target")
    build = closure.get("build")
    if not isinstance(build, dict) or build.get("target") != target_policy.target:
        fail("source closure build target disagrees with portable target")


def verify_embedded_manifest_signature(
    public_key: Path, manifest: bytes, signature: bytes
) -> None:
    if len(signature) != 64:
        fail("primary artifact MANIFEST.sig must be exactly 64 bytes")
    with tempfile.TemporaryDirectory(prefix="dcent-package-manifest-") as temporary:
        root = Path(temporary)
        manifest_path = root / "MANIFEST.json"
        signature_path = root / "MANIFEST.sig"
        manifest_path.write_bytes(manifest)
        signature_path.write_bytes(signature)
        verify_signature(
            public_key,
            manifest_path,
            signature_path,
            "primary artifact manifest",
        )


def verify_target_artifact(
    schema: str,
    target: str,
    closure: dict[str, Any],
    directory: Path,
    payload_files: list[dict[str, object]],
    public_key: Path,
    trusted_key: bytes,
) -> None:
    target_policy = policy_for_schema(schema, target)
    prebuilt = closure.get("prebuilt_rust_inputs")
    if (
        not isinstance(prebuilt, dict)
        or prebuilt.get("packaging_artifact") != target_policy.primary_artifact
    ):
        fail("source closure packaging artifact disagrees with portable target")
    artifacts = closure.get("artifacts")
    if not isinstance(artifacts, list):
        fail("source closure artifacts must be an array")
    matches = [
        item
        for item in artifacts
        if isinstance(item, dict)
        and item.get("path") == target_policy.primary_artifact
    ]
    if len(matches) != 1:
        fail("source closure must contain exactly one target primary artifact")
    closure_artifact = matches[0]

    payload_matches = [
        item
        for item in payload_files
        if item.get("name") == target_policy.primary_artifact
    ]
    if len(payload_matches) != 1:
        fail("signed payload must contain exactly one target primary artifact")
    artifact_path = directory / target_policy.primary_artifact
    artifact_raw = read_regular(artifact_path, "target primary artifact")
    observed = {
        "name": target_policy.primary_artifact,
        "sha256": digest(artifact_raw),
        "size": len(artifact_raw),
    }
    payload = payload_matches[0]
    if payload != observed:
        fail("signed target primary artifact record disagrees with retained bytes")
    if (
        closure_artifact.get("sha256") != observed["sha256"]
        or closure_artifact.get("size") != observed["size"]
    ):
        fail("source closure target artifact disagrees with signed payload bytes")
    observed_closure_artifact = {
        "path": target_policy.primary_artifact,
        "sha256": observed["sha256"],
        "size": observed["size"],
        "archive_regular_members": source_closure.safe_tar_members(
            artifact_path, io.BytesIO(artifact_raw)
        ),
    }
    if observed_closure_artifact != closure_artifact:
        fail("source closure target artifact member inventory is not reproducible")

    prefix = f"sysupgrade-{target_policy.package_board}/"
    members = closure_artifact.get("archive_regular_members")
    if not isinstance(members, list) or not members:
        fail("target primary artifact has no regular archive members")
    paths = [item.get("path") for item in members if isinstance(item, dict)]
    if len(paths) != len(members) or any(
        not isinstance(path, str) or not path.startswith(prefix) for path in paths
    ):
        fail("target primary artifact contains a foreign package-board prefix")

    manifest_name = f"{prefix}MANIFEST.json"
    signature_name = f"{prefix}MANIFEST.sig"
    embedded_key_name = f"{prefix}release_ed25519.pub"
    required = {manifest_name, signature_name, embedded_key_name}
    if not required.issubset(paths):
        fail("target primary artifact lacks signed manifest identity members")
    try:
        with tarfile.open(fileobj=io.BytesIO(artifact_raw), mode="r:*") as archive:
            raw_members: dict[str, bytes] = {}
            for name in required:
                member = archive.getmember(name)
                if not member.isfile():
                    fail(f"target primary artifact member is not regular: {name}")
                limit = (
                    MAX_EMBEDDED_MANIFEST_BYTES
                    if name == manifest_name
                    else 64 * 1024
                )
                if member.size > limit:
                    fail(f"target primary artifact member exceeds size limit: {name}")
                stream = archive.extractfile(member)
                if stream is None:
                    fail(f"target primary artifact member is unreadable: {name}")
                raw = stream.read(limit + 1)
                if len(raw) != member.size:
                    fail(f"target primary artifact member size changed: {name}")
                raw_members[name] = raw
    except (KeyError, tarfile.TarError, OSError) as error:
        fail(f"target primary artifact is not a valid policy-bound tar: {error}")

    if raw_members[embedded_key_name] != trusted_key:
        fail("primary artifact embedded release key disagrees with trusted key")
    verify_embedded_manifest_signature(
        public_key, raw_members[manifest_name], raw_members[signature_name]
    )
    try:
        manifest = json.loads(raw_members[manifest_name])
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"target primary artifact manifest is invalid JSON: {error}")
    if not isinstance(manifest, dict):
        fail("target primary artifact manifest must be an object")
    if (
        manifest.get("product") != "DCENT_OS"
        or manifest.get("package_type") != "sysupgrade"
        or manifest.get("board") != target_policy.package_board
        or manifest.get("board_target") != target_policy.package_board
    ):
        fail("target primary artifact manifest disagrees with package-board policy")
    provenance = manifest.get("provenance")
    if schema == SCHEMA and (
        not isinstance(provenance, dict)
        or provenance.get("build_target") != target_policy.target
    ):
        fail("target primary artifact manifest disagrees with build-target policy")


def verify_release_set(
    directory: Path, index: dict[str, Any], *, require_published_name: bool
) -> None:
    descriptor = read_canonical(
        directory / release_set_publication.DESCRIPTOR_NAME,
        "sealed release-set descriptor",
        release_set_publication.MAX_JSON_BYTES,
    )
    expected_keys = {
        "schema",
        "state",
        "stage_id",
        "capability_sha256",
        "output_name",
        "files",
    }
    if (
        set(descriptor) != expected_keys
        or descriptor["schema"] != release_set_publication.STAGE_SCHEMA
        or descriptor["state"] != "sealed"
        or not is_digest(descriptor["capability_sha256"])
        or not isinstance(descriptor["stage_id"], str)
        or len(descriptor["stage_id"]) != 32
        or set(descriptor["stage_id"]) - HEX
    ):
        fail("published release-set descriptor is invalid")
    target_policy = policy_for_schema(index["schema"], index["target"])
    if index["schema"] == SCHEMA:
        expected_output_name = index["output_name"]
    else:
        expected_output_name = release_capsule_target_policy.validate_output_name(
            target_policy, descriptor["output_name"]
        )
    if descriptor["output_name"] != expected_output_name:
        fail("release-set output name disagrees with signed target identity")
    if require_published_name and directory.name != expected_output_name:
        fail("published directory name disagrees with signed release identity")
    try:
        declared = release_set_publication.validate_file_entries(
            descriptor["files"], "published release set"
        )
    except release_set_publication.ReleaseSetError as error:
        fail(str(error))
    signed_payload = index["payload_files"]
    expected_declared = sorted(
        [
            *signed_payload,
            evidence(directory / INDEX_NAME),
            evidence(directory / SIGNATURE_NAME),
        ],
        key=lambda item: str(item["name"]),
    )
    if declared != expected_declared:
        fail(
            "sealed release-set descriptor disagrees with the signed exact payload set"
        )
    observed = set()
    with os.scandir(directory) as entries:
        for entry in entries:
            safe_flat_name(
                entry.name, "published release-set member"
            ) if entry.name != release_set_publication.DESCRIPTOR_NAME else None
            metadata = os.lstat(directory / entry.name)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or stat.S_ISLNK(metadata.st_mode)
                or release_set_publication.is_reparse(metadata)
                or getattr(metadata, "st_nlink", 1) != 1
            ):
                fail(f"published release-set member is unsafe: {entry.name}")
            observed.add(entry.name)
    expected = {
        release_set_publication.DESCRIPTOR_NAME,
        *(str(item["name"]) for item in declared),
    }
    if observed != expected:
        fail(
            "published release set has missing or extra members: "
            f"missing={sorted(expected - observed)}, extra={sorted(observed - expected)}"
        )
    for item in declared:
        if evidence(directory / str(item["name"])) != item:
            fail(f"published payload hash changed: {item['name']}")


def enforce_verification_mode(
    schema: str, target: str, command: str
) -> None:
    target_policy = policy_for_schema(schema, target)
    if command not in ("verify", "verify-stage"):
        fail("portable evidence verification mode is invalid")
    if not target_policy.publication_admitted:
        fail(
            "target has portable evidence policy but no admitted outer "
            "publication lifecycle in this schema"
        )


def verify_result_projection(value: dict[str, Any], invocation: dict[str, Any]) -> None:
    try:
        release_result_stage.verify_audit_projection(value, invocation)
    except release_result_stage.ResultStageError as error:
        fail(str(error))


def assert_record(directory: Path, record: dict[str, object], label: str) -> Path:
    path = directory / str(record["name"])
    if evidence(path) != record:
        fail(f"{label} projection/file record does not match retained bytes")
    return path


def verify(args: argparse.Namespace) -> None:
    with pinned_public_key(Path(args.public_key)) as (public_key, public_key_raw):
        verify_with_key(args, public_key, public_key_raw)


def verify_with_key(
    args: argparse.Namespace, public_key: Path, public_key_raw: bytes
) -> None:
    directory, _ = release_set_publication.safe_directory(
        args.release_dir, "published release directory"
    )
    index_path = directory / INDEX_NAME
    signature_path = directory / SIGNATURE_NAME
    # Authenticate the bounded index before Git traversal or large payload hashing.
    verify_signature(
        public_key, index_path, signature_path, "portable release evidence"
    )
    index = validate_index(
        read_canonical(index_path, "portable release evidence index")
    )
    require_published_name = args.command == "verify"
    if not require_published_name and index["schema"] != SCHEMA:
        fail("historical v1 evidence has no signed pre-publication output identity")
    enforce_verification_mode(
        index["schema"],
        index["target"],
        args.command,
    )
    verify_release_set(
        directory, index, require_published_name=require_published_name
    )

    projections = index["projections"]
    source_path = assert_record(directory, projections["source"], "source")
    invocation_path = assert_record(directory, projections["invocation"], "invocation")
    cargo_path = assert_record(directory, projections["cargo_input"], "Cargo input")
    packaging_path = assert_record(
        directory, projections["packaging_input"], "packaging input"
    )
    result_path = assert_record(directory, projections["result"], "result")

    try:
        source_verified = source_snapshot.verify_descriptor_against_git(
            Path(args.repo_root), index["source_commit"], source_path
        )
        invocation = release_invocation.verify_audit_descriptor(invocation_path)
        cargo = build_input_snapshot.verify_audit_descriptor(
            cargo_path, "cargo-workspace"
        )
        packaging = build_input_snapshot.verify_audit_descriptor(
            packaging_path, index["target"]
        )
    except (
        source_snapshot.SnapshotError,
        release_invocation.InvocationError,
        build_input_snapshot.SnapshotError,
        OSError,
        ValueError,
    ) as error:
        fail(f"portable projection verification failed: {error}")
    capsule = release_capsule_lineage.validate_release_capsule(
        {
            "schema": release_capsule_lineage.SCHEMA,
            "release_invocation_descriptor_sha256": release_invocation.sha256_bytes(
                release_invocation.canonical_bytes(invocation)
            ),
            "release_invocation_id": invocation["invocation_id"],
            "source_snapshot_id": source_verified["snapshot_id"],
            "source_snapshot_descriptor_sha256": source_verified["descriptor_sha256"],
        }
    )
    if capsule != index["release_capsule"]:
        fail("portable projections disagree with the signed release capsule")
    if (
        cargo.get("schema") != build_input_snapshot.SPLIT_AUTHORITY_SCHEMA
        or packaging.get("schema") != build_input_snapshot.SPLIT_AUTHORITY_SCHEMA
    ):
        fail("portable build-input projections must use split-authority schema v2")
    verify_result_projection(
        read_canonical(result_path, "result-stage audit projection"), invocation
    )

    closure_record = index["source_closure"]
    closure_sig_record = index["source_closure_signature"]
    closure_path = assert_record(directory, closure_record, "source closure")
    closure_signature_path = assert_record(
        directory, closure_sig_record, "source closure signature"
    )
    verify_signature(
        public_key, closure_path, closure_signature_path, "source closure"
    )
    portable_args = argparse.Namespace(
        repo_root=str(args.repo_root),
        artifact_dir=str(directory),
        source_snapshot_projection=str(source_path),
        release_invocation_projection=str(invocation_path),
        build_input_projection=str(packaging_path),
        signature=str(closure_signature_path),
        public_key=str(public_key),
        manifest=str(closure_path),
    )
    source_closure.verify_portable_manifest(portable_args)

    closure = source_closure.validate_receipt_schema(
        read_canonical(closure_path, "source closure")
    )
    verify_target_bindings(
        index["schema"], index["target"], invocation, packaging, closure
    )
    verify_target_artifact(
        index["schema"],
        index["target"],
        closure,
        directory,
        index["payload_files"],
        public_key,
        public_key_raw,
    )
    cargo_evidence = build_input_snapshot.snapshot_evidence(cargo)
    for entry in closure["prebuilt_rust_inputs"]["entries"]:
        receipt_path = directory / entry["receipt"]["path"]
        receipt = read_canonical(
            receipt_path, f"retained {entry['name']} build receipt"
        )
        if receipt.get("build_inputs", {}).get("evidence") != cargo_evidence:
            fail(
                f"retained {entry['name']} receipt disagrees with Cargo input projection"
            )
    print(
        f"portable {index['target']} release evidence verified: "
        f"{directory}\nclaim={CLAIM}"
    )


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    create = commands.add_parser(
        "create-live",
        help="project verified live authorities into a private release set",
    )
    create.add_argument("--repo-root", required=True)
    create.add_argument("--target", required=True)
    create.add_argument("--output-name", required=True)
    create.add_argument("--source-commit", required=True)
    create.add_argument("--source-snapshot", required=True)
    create.add_argument("--release-invocation", required=True)
    create.add_argument("--cargo-input-snapshot", required=True)
    create.add_argument("--result-stage", required=True)
    create.add_argument("--artifact-dir", required=True)
    create.add_argument("--closure", required=True)
    create.add_argument("--closure-signature", required=True)
    create.add_argument("--public-key", required=True)
    create.set_defaults(function=create_live)
    audit = commands.add_parser(
        "verify", help="verify a published set after all private-stage cleanup"
    )
    audit.add_argument("--repo-root", required=True)
    audit.add_argument("--public-key", required=True)
    audit.add_argument("release_dir")
    audit.set_defaults(function=verify)
    stage_audit = commands.add_parser(
        "verify-stage",
        help="verify a sealed private stage before atomic publication",
    )
    stage_audit.add_argument("--repo-root", required=True)
    stage_audit.add_argument("--public-key", required=True)
    stage_audit.add_argument("release_dir")
    stage_audit.set_defaults(function=verify)
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (
        PortableEvidenceError,
        release_set_publication.ReleaseSetError,
        release_capsule_lineage.CapsuleLineageError,
        release_capsule_target_policy.TargetPolicyError,
        source_closure.ClosureError,
        OSError,
        subprocess.SubprocessError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        print(f"ERROR: portable release evidence: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
