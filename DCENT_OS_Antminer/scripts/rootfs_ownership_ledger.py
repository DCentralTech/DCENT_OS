#!/usr/bin/env python3
"""Classify final Buildroot target paths using bounded provenance evidence.

This is a final-rootfs ownership ledger, not an SPDX document, SBOM, license
inventory, or proof of causal content provenance. Buildroot package
``.files-list.txt`` files provide path claims only. Declared overlay and hook
roots are stronger evidence only when their final type, mode, symlink target,
and content identity exactly match the final target entry.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import sys
from typing import Any, Dict, Iterable, List, Mapping, MutableMapping, NoReturn, Tuple


SCHEMA = "org.dcentral.dcentos.final-rootfs-ownership-ledger.v1"
HASH_ALGORITHM = "sha256"
CLASS_UNIQUE = "uniquely_attributed"
CLASS_AMBIGUOUS = "ambiguous"
CLASS_STAGE = "overlay_or_hook_owned"
CLASS_UNATTRIBUTED = "unattributed"
CLASSIFICATIONS = (CLASS_UNIQUE, CLASS_AMBIGUOUS, CLASS_STAGE, CLASS_UNATTRIBUTED)


class LedgerError(ValueError):
    """A fail-closed ledger construction or verification error."""


def fail(message: str) -> NoReturn:
    raise LedgerError(message)


def canonical_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode(
        "ascii"
    )


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: pathlib.Path) -> Tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb", buffering=0) as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def utf8_sort_key(value: str) -> bytes:
    try:
        return value.encode("utf-8")
    except UnicodeEncodeError:
        fail(f"filesystem path is not valid UTF-8: {value!r}")


def normalize_claim_path(raw_path: str, source: str) -> str:
    text = raw_path.strip()
    while text.startswith("./"):
        text = text[2:]
    pure = pathlib.PurePosixPath(text)
    if not text or pure.is_absolute() or not pure.parts or any(part in ("", ".", "..") for part in pure.parts):
        fail(f"unsafe package path claim in {source}: {raw_path!r}")
    normalized = "/" + pure.as_posix()
    utf8_sort_key(normalized)
    return normalized


def entry_record(root: pathlib.Path, path: pathlib.Path) -> Dict[str, Any]:
    relative = path.relative_to(root).as_posix()
    ledger_path = "/" + relative
    utf8_sort_key(ledger_path)
    before = path.lstat()
    mode = f"{stat.S_IMODE(before.st_mode):04o}"
    record: Dict[str, Any] = {
        "path": ledger_path,
        "type": "unknown",
        "mode": mode,
        "size": None,
        "sha256": None,
        "symlink_target": None,
        "device_major": None,
        "device_minor": None,
    }
    if stat.S_ISREG(before.st_mode):
        record["type"] = "regular"
        digest, size = sha256_file(path)
        record["size"] = size
        record["sha256"] = digest
    elif stat.S_ISDIR(before.st_mode):
        record["type"] = "directory"
    elif stat.S_ISLNK(before.st_mode):
        record["type"] = "symlink"
        record["symlink_target"] = os.readlink(path)
    elif stat.S_ISCHR(before.st_mode):
        record["type"] = "character_device"
        record["device_major"] = os.major(before.st_rdev)
        record["device_minor"] = os.minor(before.st_rdev)
    elif stat.S_ISBLK(before.st_mode):
        record["type"] = "block_device"
        record["device_major"] = os.major(before.st_rdev)
        record["device_minor"] = os.minor(before.st_rdev)
    elif stat.S_ISFIFO(before.st_mode):
        record["type"] = "fifo"
    elif stat.S_ISSOCK(before.st_mode):
        fail(f"rootfs tree contains a runtime socket: {ledger_path}")
    else:
        fail(f"rootfs tree contains an unsupported entry type: {ledger_path}")

    after = path.lstat()
    stable_fields = ("st_mode", "st_size", "st_mtime_ns", "st_ctime_ns", "st_ino", "st_dev")
    if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
        fail(f"filesystem entry changed while it was read: {ledger_path}")
    return record


def scan_tree_once(root: pathlib.Path) -> List[Dict[str, Any]]:
    entries: List[Dict[str, Any]] = []

    def visit(directory: pathlib.Path) -> None:
        try:
            children = sorted(directory.iterdir(), key=lambda item: utf8_sort_key(item.name))
        except OSError as error:
            fail(f"cannot enumerate rootfs tree {directory}: {error}")
        for child in children:
            record = entry_record(root, child)
            entries.append(record)
            if record["type"] == "directory":
                visit(child)

    visit(root)
    entries.sort(key=lambda item: utf8_sort_key(item["path"]))
    return entries


def stable_tree(root_text: str, label: str) -> Tuple[pathlib.Path, List[Dict[str, Any]]]:
    root = pathlib.Path(root_text)
    if root.is_symlink():
        fail(f"{label} root must not be a symlink: {root}")
    try:
        root = root.resolve(strict=True)
    except OSError as error:
        fail(f"{label} root is unavailable: {error}")
    if not root.is_dir():
        fail(f"{label} root is not a directory: {root}")
    first = scan_tree_once(root)
    second = scan_tree_once(root)
    if first != second:
        fail(f"{label} tree changed during analysis")
    return root, first


def identity(record: Mapping[str, Any]) -> Dict[str, Any]:
    return {
        key: record[key]
        for key in (
            "type",
            "mode",
            "size",
            "sha256",
            "symlink_target",
            "device_major",
            "device_minor",
        )
    }


def tree_digest(entries: Iterable[Mapping[str, Any]]) -> str:
    bound = [{"path": entry["path"], **identity(entry)} for entry in entries]
    return sha256_bytes(canonical_bytes({"entries": bound}))


def parse_named_root(value: str, option: str) -> Tuple[str, str]:
    if "=" not in value:
        fail(f"{option} must use NAME=DIR syntax")
    name, path = value.split("=", 1)
    name = name.strip()
    if not name or any(character in name for character in "\r\n\t"):
        fail(f"{option} has an invalid owner name")
    if not path:
        fail(f"{option} has an empty directory")
    return name, path


def package_claims(build_dir_text: str) -> Tuple[Dict[str, List[str]], Dict[str, Any]]:
    build_dir = pathlib.Path(build_dir_text)
    try:
        build_dir = build_dir.resolve(strict=True)
    except OSError as error:
        fail(f"Buildroot build directory is unavailable: {error}")
    if not build_dir.is_dir():
        fail(f"Buildroot build directory is not a directory: {build_dir}")

    claims: MutableMapping[str, set[str]] = {}
    records = 0
    lists = sorted(build_dir.glob("*/.files-list.txt"), key=lambda path: utf8_sort_key(path.as_posix()))
    if not lists:
        fail(f"Buildroot build directory contains no package .files-list.txt records: {build_dir}")
    for files_list in lists:
        if files_list.is_symlink() or not files_list.is_file():
            fail(f"package files list must be a non-symlink regular file: {files_list}")
        try:
            lines = files_list.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            fail(f"cannot read package files list {files_list}: {error}")
        for line_number, line in enumerate(lines, 1):
            if not line:
                continue
            owner, separator, raw_path = line.partition(",")
            owner = owner.strip()
            if not separator or not owner:
                fail(f"malformed package claim at {files_list}:{line_number}")
            path = normalize_claim_path(raw_path, f"{files_list}:{line_number}")
            claims.setdefault(path, set()).add(owner)
            records += 1

    normalized = {
        path: sorted(owners, key=utf8_sort_key)
        for path, owners in sorted(claims.items(), key=lambda item: utf8_sort_key(item[0]))
    }
    digest_input = [{"path": path, "owners": owners} for path, owners in normalized.items()]
    evidence = {
        "files_list_count": len(lists),
        "raw_claim_count": records,
        "claimed_path_count": len(normalized),
        "sha256": sha256_bytes(canonical_bytes({"claims": digest_input})),
    }
    return normalized, evidence


def regular_file_evidence(path_text: str, label: str) -> Dict[str, Any]:
    path = pathlib.Path(path_text)
    if path.is_symlink():
        fail(f"{label} must not be a symlink: {path}")
    try:
        path = path.resolve(strict=True)
    except OSError as error:
        fail(f"{label} is unavailable: {error}")
    if not path.is_file():
        fail(f"{label} is not a regular file: {path}")
    digest, size = sha256_file(path)
    return {"path": path.name, "size": size, "sha256": digest}


def named_file_evidence(values: List[str], kind: str) -> List[Dict[str, Any]]:
    records: List[Dict[str, Any]] = []
    names = set()
    for value in values:
        name, path = parse_named_root(value, f"--{kind}")
        if name in names:
            fail(f"duplicate {kind} name: {name}")
        names.add(name)
        records.append({"name": name, **regular_file_evidence(path, f"{kind}:{name}")})
    return records


def artifact_evidence(values: List[str]) -> List[Dict[str, Any]]:
    records = [regular_file_evidence(value, "rootfs artifact") for value in values]
    records.sort(key=lambda item: utf8_sort_key(item["path"]))
    if len({record["path"] for record in records}) != len(records):
        fail("rootfs artifacts contain duplicate basenames")
    return records


def stage_roots(values: List[str], kind: str) -> List[Dict[str, Any]]:
    stages: List[Dict[str, Any]] = []
    names = set()
    for order, value in enumerate(values):
        name, root_text = parse_named_root(value, f"--{kind}-root")
        qualified_name = f"{kind}:{name}"
        if qualified_name in names:
            fail(f"duplicate stage owner: {qualified_name}")
        names.add(qualified_name)
        _root, entries = stable_tree(root_text, qualified_name)
        stages.append(
            {
                "kind": kind,
                "name": qualified_name,
                "order": order,
                "sha256": tree_digest(entries),
                "entry_count": len(entries),
                "entries": {entry["path"]: entry for entry in entries},
            }
        )
    return stages


def classify_entry(
    final: Mapping[str, Any],
    claims: Mapping[str, List[str]],
    stages: List[Mapping[str, Any]],
) -> Dict[str, Any]:
    path = final["path"]
    package_owners = claims.get(path, [])
    stage_claims = [stage for stage in stages if path in stage["entries"]]
    result = dict(final)
    result["package_path_claims"] = package_owners
    result["stage_path_claims"] = [stage["name"] for stage in stage_claims]

    if stage_claims:
        last_stage = stage_claims[-1]
        if identity(final) == identity(last_stage["entries"][path]):
            result.update(
                {
                    "classification": CLASS_STAGE,
                    "owner": last_stage["name"],
                    "attribution_basis": "last-declared-stage-exact-final-identity-match",
                }
            )
        else:
            result.update(
                {
                    "classification": CLASS_UNATTRIBUTED,
                    "owner": None,
                    "attribution_basis": "last-declared-stage-identity-mismatch",
                }
            )
    elif len(package_owners) == 1:
        result.update(
            {
                "classification": CLASS_UNIQUE,
                "owner": f"package:{package_owners[0]}",
                "attribution_basis": "single-buildroot-path-claim;final-content-origin-not-proven",
            }
        )
    elif len(package_owners) > 1:
        result.update(
            {
                "classification": CLASS_AMBIGUOUS,
                "owner": None,
                "attribution_basis": "multiple-buildroot-path-claims",
            }
        )
    else:
        result.update(
            {
                "classification": CLASS_UNATTRIBUTED,
                "owner": None,
                "attribution_basis": "no-declared-final-path-evidence",
            }
        )
    return result


def build_ledger(args: argparse.Namespace) -> Dict[str, Any]:
    _target_root, final_entries = stable_tree(args.target_dir, "final target")
    claims, package_evidence = package_claims(args.build_dir)
    overlays = stage_roots(args.overlay_root, "overlay")
    hooks = stage_roots(args.hook_root, "hook")
    stages = overlays + hooks
    direct_mutators = named_file_evidence(args.post_build_script, "post-build-script")
    artifacts = artifact_evidence(args.artifact)
    # Hook order is after every overlay. Preserve CLI order within each phase.
    for order, stage in enumerate(stages):
        stage["order"] = order

    classified = [classify_entry(entry, claims, stages) for entry in final_entries]
    counts = {classification: 0 for classification in CLASSIFICATIONS}
    type_counts: Dict[str, int] = {}
    for entry in classified:
        counts[entry["classification"]] += 1
        type_counts[entry["type"]] = type_counts.get(entry["type"], 0) + 1
    final_paths = {entry["path"] for entry in final_entries}
    package_survivors = sum(1 for path in claims if path in final_paths)

    return {
        "schema": SCHEMA,
        "claim_scope": {
            "kind": "bounded-final-rootfs-path-attribution",
            "is_spdx": False,
            "is_sbom": False,
            "proves_causal_content_origin": False,
        },
        "identity_binding": {
            "path": "absolute POSIX path relative to target root",
            "type": True,
            "permission_mode": True,
            "regular_content": HASH_ALGORITHM,
            "symlink_target": True,
            "device_major_minor": True,
        },
        "inputs": {
            "final_target": {
                "entry_count": len(final_entries),
                "sha256": tree_digest(final_entries),
            },
            "buildroot_package_path_claims": {
                **package_evidence,
                "surviving_final_path_count": package_survivors,
            },
            "stages": [
                {
                    "kind": stage["kind"],
                    "name": stage["name"],
                    "order": stage["order"],
                    "entry_count": stage["entry_count"],
                    "sha256": stage["sha256"],
                }
                for stage in stages
            ],
            "declared_direct_post_build_scripts": direct_mutators,
            "rootfs_artifacts": artifacts,
        },
        "summary": {
            "entry_count": len(classified),
            "classification_counts": counts,
            "type_counts": dict(sorted(type_counts.items())),
        },
        "limitations": [
            "Buildroot .files-list.txt records are path claims and do not bind install-time content or mode.",
            "Only overlay and hook roots explicitly supplied to this invocation are considered.",
            "Declared post-build script hashes bind script definitions only; they do not prove execution or attribute their outputs.",
            "An exact stage identity match is evidence of equality, not proof that no other producer created the same bytes.",
            "Unrecorded post-build hooks can modify a uniquely package-claimed path; package attribution therefore remains path-only.",
            "UID, GID, mtime, xattrs, ACLs, Linux capabilities, and hardlink topology are outside this schema.",
            "Filesystem-image transformations performed after Buildroot target finalization are outside this schema.",
            "This ledger is not an SPDX document, SBOM, license inventory, or complete release source-closure receipt.",
        ],
        "entries": classified,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--target-dir", required=True, help="final Buildroot output/target directory")
    result.add_argument("--build-dir", required=True, help="Buildroot output/build directory")
    result.add_argument(
        "--overlay-root",
        action="append",
        default=[],
        metavar="NAME=DIR",
        help="rootfs-shaped overlay evidence, in Buildroot application order",
    )
    result.add_argument(
        "--hook-root",
        action="append",
        default=[],
        metavar="NAME=DIR",
        help="rootfs-shaped post-build hook evidence, in execution order",
    )
    result.add_argument(
        "--post-build-script",
        action="append",
        default=[],
        metavar="NAME=FILE",
        help="direct TARGET_DIR mutator definition to bind without claiming its output ownership",
    )
    result.add_argument(
        "--artifact",
        action="append",
        default=[],
        help="emitted rootfs payload whose exact bytes this ledger accompanies",
    )
    output = result.add_mutually_exclusive_group(required=True)
    output.add_argument("--output", help="write canonical ledger JSON")
    output.add_argument("--verify", help="verify an existing canonical ledger against current inputs")
    return result


def main() -> int:
    args = parser().parse_args()
    ledger = build_ledger(args)
    encoded = canonical_bytes(ledger)
    if args.verify:
        path = pathlib.Path(args.verify)
        try:
            observed = path.read_bytes()
        except OSError as error:
            fail(f"cannot read ledger for verification: {error}")
        if observed != encoded:
            fail("final rootfs ownership ledger disagrees with current evidence")
        print(f"final rootfs ownership ledger verified: {path}")
        return 0

    path = pathlib.Path(args.output)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(encoded)
    print(f"final rootfs ownership ledger written: {path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except LedgerError as error:
        print(f"ERROR: final rootfs ownership ledger: {error}", file=sys.stderr)
        raise SystemExit(1)
