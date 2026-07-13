#!/usr/bin/env python3
"""Generate and verify a deterministic Rust release dependency inventory.

This is a DCENT_OS-specific inventory, not an SPDX or CycloneDX document. It
uses Cargo's locked, offline resolver and binds the resulting graph to one
release artifact without claiming non-Rust package or license completeness.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import pathlib
import re
import sys
from typing import Any, Dict, List


SCHEMA = "org.dcentral.dcentos.rust-dependency-inventory.v1"
IDENTIFIER = re.compile(r"^[A-Za-z0-9._+:/@-]+$")


class InventoryError(ValueError):
    """A fail-closed inventory generation or verification error."""


def fail(message: str) -> "NoReturn":
    raise InventoryError(message)


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def regular_file(path_text: str, label: str) -> pathlib.Path:
    source = pathlib.Path(path_text)
    if source.is_symlink():
        fail(f"{label} must not be a symlink: {source}")
    path = source.resolve(strict=True)
    if not path.is_file():
        fail(f"{label} must be a regular file: {path}")
    return path


def file_binding(path: pathlib.Path, display_path: str) -> Dict[str, Any]:
    return {
        "path": display_path,
        "sha256": sha256_file(path),
        "size": path.stat().st_size,
    }


def canonical_bytes(value: Dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode(
        "ascii"
    )


def iso_utc(epoch: int) -> str:
    try:
        return dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    except (OverflowError, OSError, ValueError) as error:
        fail(f"source epoch cannot be represented: {error}")


def inventory_scope() -> Dict[str, Any]:
    return {
        "format": "dcentos-custom-rust-inventory",
        "spdx_conformance": "not_claimed",
        "cyclonedx_conformance": "not_claimed",
        "includes": "Cargo-resolved Rust workspace and transitive package graph",
        "resolution_policy": "cargo 1.90.0 metadata --locked --offline --filter-platform <release-target>",
        "artifact_reachability": "not_claimed; target-filtered full workspace resolution, not a shipped-binary reachability proof",
        "license_evidence": "Cargo package declarations only; not independently validated",
        "vulnerability_analysis": "not_performed",
        "unresolved": [
            "Buildroot, Linux kernel, bootloader, container, dashboard, and system packages are excluded",
            "crate archive checksums are not enumerated; the complete Cargo.lock is hash-bound instead",
            "declared license expressions and license files are not audited for correctness or compatibility",
            "inventory authenticity depends on the authenticated release channel",
            "workspace packages not reachable from the shipped daemon binary may be included",
        ],
    }


def cargo_metadata(path: pathlib.Path) -> Dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        fail(f"Cargo metadata evidence is invalid JSON: {error}")
    if not isinstance(value, dict) or not isinstance(value.get("packages"), list):
        fail("Cargo metadata is missing the package graph")
    if not isinstance(value.get("resolve"), dict) or not isinstance(value["resolve"].get("nodes"), list):
        fail("Cargo metadata is missing the resolved dependency graph")
    return value


def metadata_path_maps(values: List[str]) -> List[tuple]:
    mappings = []
    for value in values:
        if "=" not in value:
            fail("metadata path maps must use CONTAINER_PREFIX=HOST_PREFIX")
        container_text, host_text = value.split("=", 1)
        container = pathlib.PurePosixPath(container_text)
        host = pathlib.Path(host_text).resolve(strict=True)
        if not container.is_absolute() or not host.is_dir():
            fail("metadata path maps require an absolute container prefix and host directory")
        mappings.append((container, host))
    mappings.sort(key=lambda item: len(item[0].parts), reverse=True)
    return mappings


def remap_metadata_paths(metadata: Dict[str, Any], mappings: List[tuple]) -> None:
    for package in metadata["packages"]:
        for field in ("manifest_path", "license_file"):
            value = package.get(field)
            if not value:
                continue
            container_path = pathlib.PurePosixPath(value)
            for container_prefix, host_prefix in mappings:
                try:
                    suffix = container_path.relative_to(container_prefix)
                except ValueError:
                    continue
                package[field] = str(host_prefix.joinpath(*suffix.parts))
                break


def relative_inside(root: pathlib.Path, path_text: str, label: str) -> str:
    path = pathlib.Path(path_text).resolve(strict=True)
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        fail(f"{label} is outside the Cargo workspace: {path}")


def component_id(
    package: Dict[str, Any], source_root: pathlib.Path, workspace_member_ids: set
) -> str:
    name = package["name"]
    version = package["version"]
    source = package.get("source")
    if source:
        return f"{source}#{name}@{version}"
    manifest = pathlib.Path(package["manifest_path"]).resolve(strict=True)
    relative = relative_inside(source_root, str(manifest.parent), f"local package {name}")
    kind = "workspace" if package["id"] in workspace_member_ids else "local-path"
    return f"{kind}:{relative}:{name}@{version}"


def normalized_graph(metadata: Dict[str, Any], source_root: pathlib.Path) -> Dict[str, Any]:
    packages_by_cargo_id = {package["id"]: package for package in metadata["packages"]}
    workspace_member_ids = set(metadata.get("workspace_members", []))
    component_by_cargo_id = {
        cargo_id: component_id(package, source_root, workspace_member_ids)
        for cargo_id, package in packages_by_cargo_id.items()
    }
    if len(set(component_by_cargo_id.values())) != len(component_by_cargo_id):
        fail("normalized Cargo component identifiers are not unique")

    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    if set(nodes_by_id) != set(packages_by_cargo_id):
        fail("Cargo package and resolver node sets disagree")

    components: List[Dict[str, Any]] = []
    for cargo_id, package in packages_by_cargo_id.items():
        source = package.get("source")
        local_source = cargo_id in workspace_member_ids
        source_label = source or ("workspace" if local_source else "local-path")
        entry: Dict[str, Any] = {
            "component_id": component_by_cargo_id[cargo_id],
            "name": package["name"],
            "version": package["version"],
            "source": source_label,
            "license_declared": package.get("license"),
            "features_enabled": sorted(nodes_by_id[cargo_id].get("features", [])),
        }
        if source is None:
            entry["manifest_path"] = relative_inside(
                source_root, package["manifest_path"], f"local package {package['name']} manifest"
            )
            license_file = package.get("license_file")
            if license_file:
                entry["license_file"] = relative_inside(
                    source_root, license_file, f"local package {package['name']} license file"
                )
        components.append(entry)
    components.sort(key=lambda item: item["component_id"].encode("utf-8"))

    relationships: List[Dict[str, Any]] = []
    for cargo_id, node in nodes_by_id.items():
        dependencies = []
        for dependency in node.get("deps", []):
            kinds = []
            for kind in dependency.get("dep_kinds", []):
                kinds.append(
                    {
                        "kind": kind.get("kind") or "normal",
                        "target": kind.get("target"),
                    }
                )
            kinds.sort(key=lambda item: ((item["kind"] or ""), (item["target"] or "")))
            dependencies.append(
                {
                    "component_id": component_by_cargo_id[dependency["pkg"]],
                    "dependency_name": dependency["name"],
                    "kinds": kinds,
                }
            )
        dependencies.sort(
            key=lambda item: (item["component_id"].encode("utf-8"), item["dependency_name"])
        )
        relationships.append(
            {
                "component_id": component_by_cargo_id[cargo_id],
                "depends_on": dependencies,
            }
        )
    relationships.sort(key=lambda item: item["component_id"].encode("utf-8"))

    workspace_members = sorted(
        (component_by_cargo_id[cargo_id] for cargo_id in metadata.get("workspace_members", [])),
        key=lambda value: value.encode("utf-8"),
    )
    return {
        "component_count": len(components),
        "relationship_node_count": len(relationships),
        "workspace_members": workspace_members,
        "components": components,
        "relationships": relationships,
    }


def build_inventory(args: argparse.Namespace) -> Dict[str, Any]:
    workspace = pathlib.Path(args.workspace).resolve(strict=True)
    if not workspace.is_dir():
        fail(f"Cargo workspace is not a directory: {workspace}")
    lockfile = regular_file(str(workspace / "Cargo.lock"), "Cargo.lock")
    metadata_path = regular_file(args.metadata_json, "Cargo metadata evidence")
    if not IDENTIFIER.fullmatch(args.target):
        fail("release target is missing or contains non-canonical characters")
    source_root = pathlib.Path(args.source_root).resolve(strict=True)
    if not source_root.is_dir():
        fail(f"source root is not a directory: {source_root}")
    try:
        workspace.relative_to(source_root)
    except ValueError:
        fail("Cargo workspace must be inside the source root")
    artifact = regular_file(args.artifact, "release artifact")
    epoch = int(args.source_date_epoch)
    metadata = cargo_metadata(metadata_path)
    remap_metadata_paths(metadata, metadata_path_maps(args.metadata_path_map))
    return {
        "schema": SCHEMA,
        "created_at_utc": iso_utc(epoch),
        "source_date_epoch": epoch,
        "scope": inventory_scope(),
        "cargo_lock": file_binding(lockfile, "Cargo.lock"),
        "resolver": {
            "target": args.target,
            "command": "cargo metadata --locked --offline --filter-platform <release-target> --format-version 1",
            "execution": "same pinned Rust 1.90 build container, immediately after release build",
            "metadata": file_binding(
                metadata_path,
                f"target/release-inventory/{args.target}.metadata.json",
            ),
        },
        "artifact": file_binding(artifact, artifact.name),
        "graph": normalized_graph(metadata, source_root),
    }


def write_inventory(args: argparse.Namespace) -> None:
    inventory = build_inventory(args)
    output = pathlib.Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp.{os.getpid()}")
    try:
        with temporary.open("xb") as stream:
            stream.write(canonical_bytes(inventory))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, output)
        if os.name == "posix":
            directory_fd = os.open(
                str(output.parent), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
            )
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
    print(output)


def verify_inventory(args: argparse.Namespace) -> None:
    inventory_path = regular_file(args.inventory, "Rust dependency inventory")
    raw = inventory_path.read_bytes()
    try:
        inventory = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"inventory is invalid JSON: {error}")
    if not isinstance(inventory, dict) or inventory.get("schema") != SCHEMA:
        fail("unsupported Rust dependency inventory schema")
    if raw != canonical_bytes(inventory):
        fail("Rust dependency inventory is not canonical JSON")
    if inventory.get("scope") != inventory_scope():
        fail("Rust dependency inventory scope is invalid or overstates available evidence")
    resolver = inventory.get("resolver")
    artifact_section = inventory.get("artifact")
    graph = inventory.get("graph")
    if not all(isinstance(section, dict) for section in (resolver, artifact_section, graph)):
        fail("Rust dependency inventory resolver, artifact, and graph sections must be objects")
    if not IDENTIFIER.fullmatch(str(resolver.get("target", ""))):
        fail("Rust dependency inventory target is invalid")
    if resolver.get("command") != "cargo metadata --locked --offline --filter-platform <release-target> --format-version 1":
        fail("Rust dependency resolver command is weakened")
    if resolver.get("execution") != "same pinned Rust 1.90 build container, immediately after release build":
        fail("Rust dependency resolver execution evidence is invalid")

    workspace = pathlib.Path(args.workspace).resolve(strict=True)
    artifact_dir = pathlib.Path(args.artifact_dir).resolve(strict=True)
    artifact_name = artifact_section.get("path")
    if not isinstance(artifact_name, str) or pathlib.Path(artifact_name).name != artifact_name:
        fail("inventory artifact path must be a basename")
    expected_args = argparse.Namespace(
        workspace=str(workspace),
        source_root=args.source_root,
        metadata_json=args.metadata_json,
        metadata_path_map=args.metadata_path_map,
        target=inventory["resolver"]["target"],
        artifact=str(artifact_dir / artifact_name),
        source_date_epoch=inventory["source_date_epoch"],
    )
    expected = build_inventory(expected_args)
    if expected != inventory:
        fail("Rust dependency inventory no longer matches locked source or artifact bytes")
    print(f"Rust dependency inventory verified: {inventory_path}")


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(description=__doc__)
    commands = top.add_subparsers(dest="command", required=True)
    generate = commands.add_parser("generate", help="generate a release-bound Rust inventory")
    generate.add_argument("--workspace", required=True)
    generate.add_argument("--source-root", required=True)
    generate.add_argument("--metadata-json", required=True)
    generate.add_argument("--metadata-path-map", action="append", default=[])
    generate.add_argument("--target", required=True)
    generate.add_argument("--artifact", required=True)
    generate.add_argument("--source-date-epoch", type=int, required=True)
    generate.add_argument("--output", required=True)
    generate.set_defaults(function=write_inventory)

    verify = commands.add_parser("verify", help="verify inventory against locked source and artifact")
    verify.add_argument("--workspace", required=True)
    verify.add_argument("--source-root", required=True)
    verify.add_argument("--metadata-json", required=True)
    verify.add_argument("--metadata-path-map", action="append", default=[])
    verify.add_argument("--artifact-dir", required=True)
    verify.add_argument("inventory")
    verify.set_defaults(function=verify_inventory)
    return top


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (InventoryError, KeyError, TypeError, OSError) as error:
        print(f"ERROR: Rust dependency inventory: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
