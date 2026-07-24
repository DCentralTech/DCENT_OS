#!/usr/bin/env python3
"""Validate and atomically publish an exact AM2 SD boot-artifact set."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from typing import NoReturn, Sequence


SCRIPT_DIR = Path(__file__).resolve().parent
RELEASE_SET_TOOL = SCRIPT_DIR / "release_set_publication.py"
STOCK_XIL_BOOT_MD5S = frozenset(
    {
        "f2cb2eaaf757c72946113ad13786afa0",
        "730a6ad1566376381dee8a59ebab55d6",
        "7f100b3b90461e718ac6c1de0eafa888",
        "dbd17ba1a738647540073f29813de92f",
        "acb2fcdbcebfcbb71b02f3d0614363ae",
        "6bc79007d45c3b623756523c6ab903ba",
    }
)


class StageError(RuntimeError):
    """An input or publication boundary failed closed."""


@dataclass(frozen=True)
class Snapshot:
    source_name: str
    output_name: str
    size: int
    sha256: str
    md5: str


@dataclass(frozen=True)
class ExactFile:
    size: int
    sha256: str


def fail(message: str) -> NoReturn:
    raise StageError(message)


def is_reparse(metadata: os.stat_result) -> bool:
    flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(metadata, "st_file_attributes", 0) & flag)


def stable_file_state(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        getattr(metadata, "st_nlink", 1),
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def stable_directory_state(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def require_real_directory(path: Path, label: str) -> os.stat_result:
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        fail(f"{label} is missing: {path}")
    if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
        fail(f"{label} must be a non-reparse directory: {path}")
    return metadata


def lexical_exists(path: Path) -> bool:
    return os.path.lexists(path)


def select_candidate(
    root: Path, names: Sequence[str], label: str, *, required: bool
) -> Path | None:
    # Probe directory entries by their stored spelling. On a case-insensitive
    # host, Path.exists("BOOT.bin"), Path.exists("boot.bin"), and
    # Path.exists("BOOT.BIN") can all describe the same one entry.
    try:
        stored_names = {entry.name for entry in os.scandir(root)}
    except OSError as error:
        fail(f"cannot enumerate artifacts directory {root}: {error}")
    present = [root / name for name in names if name in stored_names]
    if len(present) > 1:
        fail(
            f"{label} is ambiguous; retain exactly one candidate: "
            + ", ".join(path.name for path in present)
        )
    if not present:
        if required:
            fail(f"missing {label}; accepted names: {', '.join(names)}")
        return None
    return present[0]


def write_all(descriptor: int, content: bytes) -> None:
    offset = 0
    while offset < len(content):
        written = os.write(descriptor, content[offset:])
        if written <= 0:
            fail("short write while creating private artifact snapshot")
        offset += written


def snapshot_regular_file(source: Path, destination: Path) -> Snapshot:
    try:
        initial = os.lstat(source)
    except FileNotFoundError:
        fail(f"artifact disappeared before snapshot: {source}")
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
    ):
        fail(f"artifact must be a single-link non-reparse regular file: {source}")

    source_flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    destination_flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_BINARY", 0)
    )
    source_descriptor = -1
    destination_descriptor = -1
    committed = False
    sha256 = hashlib.sha256()
    md5 = hashlib.md5(usedforsecurity=False)
    observed = 0
    try:
        source_descriptor = os.open(source, source_flags)
        opened = os.fstat(source_descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or is_reparse(opened)
            or getattr(opened, "st_nlink", 1) != 1
            or stable_file_state(opened) != stable_file_state(initial)
        ):
            fail(f"artifact changed before it could be pinned: {source}")

        destination_descriptor = os.open(destination, destination_flags, 0o600)
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            write_all(destination_descriptor, chunk)
            sha256.update(chunk)
            md5.update(chunk)
            observed += len(chunk)
        os.fsync(destination_descriptor)

        after = os.fstat(source_descriptor)
        try:
            current = os.lstat(source)
        except FileNotFoundError:
            fail(f"artifact pathname disappeared during snapshot: {source}")
        if (
            stable_file_state(initial) != stable_file_state(after)
            or stable_file_state(initial) != stable_file_state(current)
            or observed != initial.st_size
        ):
            fail(f"artifact changed while it was copied: {source}")

        # Windows exposes creation time as st_ctime and may defer mtime
        # visibility. Re-read the already pinned handle so correctness never
        # depends solely on path timestamps or their filesystem granularity.
        os.lseek(source_descriptor, 0, os.SEEK_SET)
        verification_sha256 = hashlib.sha256()
        verification_md5 = hashlib.md5(usedforsecurity=False)
        verification_size = 0
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            verification_sha256.update(chunk)
            verification_md5.update(chunk)
            verification_size += len(chunk)
        verification_after = os.fstat(source_descriptor)
        try:
            verification_current = os.lstat(source)
        except FileNotFoundError:
            fail(f"artifact pathname disappeared during verification: {source}")
        if (
            verification_size != observed
            or verification_sha256.digest() != sha256.digest()
            or verification_md5.digest() != md5.digest()
            or stable_file_state(after) != stable_file_state(verification_after)
            or stable_file_state(current) != stable_file_state(verification_current)
        ):
            fail(f"artifact bytes changed while its snapshot was verified: {source}")
        staged = os.fstat(destination_descriptor)
        if (
            not stat.S_ISREG(staged.st_mode)
            or getattr(staged, "st_nlink", 1) != 1
            or staged.st_size != observed
        ):
            fail(f"private artifact snapshot is not exact: {destination.name}")
        committed = True
    finally:
        if destination_descriptor >= 0:
            os.close(destination_descriptor)
        if source_descriptor >= 0:
            os.close(source_descriptor)
        if not committed:
            try:
                destination.unlink()
            except FileNotFoundError:
                pass

    return Snapshot(
        source_name=source.name,
        output_name=destination.name,
        size=observed,
        sha256=sha256.hexdigest(),
        md5=md5.hexdigest(),
    )


def require_magic(path: Path, expected: bytes, label: str) -> None:
    with path.open("rb") as handle:
        observed = handle.read(len(expected))
    if observed != expected:
        fail(f"{label} has an invalid magic header: {path.name}")


def validate_snapshot(stage: Path, snapshot: Snapshot) -> None:
    path = stage / snapshot.output_name
    if snapshot.output_name == "BOOT.bin":
        if not 20_000 <= snapshot.size <= 5_000_000:
            fail(
                f"BOOT.bin size {snapshot.size} is outside the 20KiB..5MiB "
                "accepted Zynq range"
            )
        if snapshot.md5 in STOCK_XIL_BOOT_MD5S:
            fail(
                f"BOOT.bin md5 {snapshot.md5} matches a known stock XIL signed image"
            )
    elif snapshot.output_name == "uImage":
        if not 65_536 <= snapshot.size <= 32 * 1024 * 1024:
            fail(f"uImage size {snapshot.size} is outside the accepted range")
        require_magic(path, bytes.fromhex("27051956"), "uImage")
    elif snapshot.output_name == "devicetree.dtb":
        if not 40 <= snapshot.size <= 4 * 1024 * 1024:
            fail(f"DTB size {snapshot.size} is outside the accepted range")
        require_magic(path, bytes.fromhex("d00dfeed"), "DTB")
    elif snapshot.size == 0:
        fail(f"optional artifact is empty: {snapshot.output_name}")


def run_release_set(*arguments: str, input_bytes: bytes | None = None) -> bytes:
    result = subprocess.run(
        [sys.executable, os.fspath(RELEASE_SET_TOOL), *arguments],
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        fail(detail or f"release-set operation failed: {arguments[0]}")
    return result.stdout


def query_capability(capability: Path, field: str) -> str:
    output = run_release_set(
        "query", "--field", field, input_bytes=capability.read_bytes()
    )
    value = output.decode("utf-8", "strict").strip()
    if not value:
        fail(f"release-set capability has no {field}")
    return value


def write_stage_manifest(
    stage: Path, snapshots: dict[str, Snapshot | None]
) -> ExactFile:
    artifacts: dict[str, object] = {}
    for output_name, snapshot in snapshots.items():
        if snapshot is None:
            artifacts[output_name] = {"present": False}
            continue
        entry: dict[str, object] = {
            "bytes": snapshot.size,
            "present": True,
            "sha256": snapshot.sha256,
            "source_name": snapshot.source_name,
        }
        if output_name == "BOOT.bin":
            entry["md5_stock_xil_denylist"] = snapshot.md5
        artifacts[output_name] = entry
    value = {
        "artifacts": artifacts,
        "ready_for_complete_build": True,
        "schema": "dcentos.am2_sd_artifacts_stage.v2",
        "validation": {
            "BOOT.bin": "size_range_and_stock_xil_md5_denylist",
            "devicetree.dtb": "size_range_and_fdt_magic",
            "uImage": "size_range_and_legacy_uimage_magic",
        },
    }
    content = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    path = stage / "artifacts.manifest.json"
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0),
        0o600,
    )
    try:
        write_all(descriptor, content)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    return ExactFile(size=len(content), sha256=hashlib.sha256(content).hexdigest())


def validate_release_set_manifest(
    path: Path,
    snapshots: dict[str, Snapshot | None],
    semantic_manifest: ExactFile,
) -> None:
    try:
        value = json.loads(path.read_bytes())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot parse authoritative release-set manifest: {error}")
    if not isinstance(value, dict) or value.get("schema") != "dcentos.release-set-files.v1":
        fail("authoritative release-set manifest has the wrong schema")
    entries = value.get("files")
    if not isinstance(entries, list):
        fail("authoritative release-set manifest has no file list")
    by_name: dict[str, dict[str, object]] = {}
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
            fail("authoritative release-set manifest has an invalid file entry")
        name = entry["name"]
        if name in by_name:
            fail(f"authoritative release-set manifest repeats {name}")
        by_name[name] = entry
    expected_names = {
        name for name, snapshot in snapshots.items() if snapshot is not None
    } | {"artifacts.manifest.json"}
    if set(by_name) != expected_names:
        fail("authoritative release-set manifest disagrees with the exact staged names")
    semantic_entry = by_name["artifacts.manifest.json"]
    if (
        semantic_entry.get("size") != semantic_manifest.size
        or semantic_entry.get("sha256") != semantic_manifest.sha256
    ):
        fail("sealed semantic manifest bytes disagree with generated staging evidence")
    for name, snapshot in snapshots.items():
        if snapshot is None:
            continue
        entry = by_name[name]
        if entry.get("size") != snapshot.size or entry.get("sha256") != snapshot.sha256:
            fail(f"sealed bytes disagree with semantic staging evidence: {name}")


def cleanup_control(
    control: Path,
    capability: Path,
    files_manifest: Path,
    *,
    destroy_stage: bool,
) -> bool:
    if destroy_stage and capability.is_file():
        try:
            run_release_set("destroy-stage", "--capability-file", os.fspath(capability))
        except StageError as error:
            print(
                "ERROR: AM2 private stage cleanup failed; recover with capability: "
                f"{capability}\n       {error}",
                file=sys.stderr,
            )
            return False
    for path in (files_manifest, capability):
        try:
            path.unlink()
        except FileNotFoundError:
            pass
    try:
        control.rmdir()
    except OSError as error:
        print(f"ERROR: cannot retire AM2 stage control directory: {error}", file=sys.stderr)
        return False
    return True


def absolute_output(source: Path, requested: str | None) -> Path:
    raw = Path(requested).expanduser() if requested else Path(f"{source}.staged")
    if not raw.is_absolute():
        raw = Path.cwd() / raw
    if raw.name in ("", ".", ".."):
        fail("output directory must have a flat final name")
    parent = raw.parent.resolve(strict=True)
    require_real_directory(parent, "output parent")
    return parent / raw.name


def stage_artifacts(args: argparse.Namespace) -> None:
    source_argument = Path(args.artifacts_dir).expanduser()
    if not source_argument.is_absolute():
        source_argument = Path.cwd() / source_argument
    require_real_directory(source_argument, "artifacts directory")
    source = source_argument.resolve(strict=True)
    require_real_directory(source, "artifacts directory")
    output = absolute_output(source, args.output_dir)
    if not args.check_only and output == source:
        fail("in-place staging is forbidden; use --check-only or a distinct output")
    if not args.check_only and lexical_exists(output):
        fail(
            f"atomic output directory already exists: {output}; choose a new output "
            "or archive the prior generation"
        )

    control = Path(
        tempfile.mkdtemp(
            prefix=f".{output.name}.am2-stage-control-", dir=os.fspath(output.parent)
        )
    )
    os.chmod(control, 0o700)
    source_metadata = os.lstat(source)
    capability = control / "capability.json"
    files_manifest = control / "files.json"
    stage: Path | None = None
    stage_owned = False
    cleanup_ok = True
    success_lines: list[str] = []
    try:
        run_release_set(
            "create-stage",
            "--parent",
            os.fspath(control),
            "--capability-output",
            os.fspath(capability),
        )
        # The capability is sufficient destruction authority. Own it before
        # parsing any reported field so a query/reporting failure cannot turn
        # a recoverable stage into an orphan.
        stage_owned = True
        stage = Path(query_capability(capability, "stage-path"))
        require_real_directory(stage, "private release-set stage")

        selected = {
            "BOOT.bin": select_candidate(
                source, ("BOOT.bin", "boot.bin", "BOOT.BIN"), "BOOT.bin", required=True
            ),
            "uImage": select_candidate(source, ("uImage",), "uImage", required=True),
            "devicetree.dtb": select_candidate(
                source,
                (
                    "am2-s19jpro.dtb",
                    "am2-s19j.dtb",
                    "devicetree.dtb",
                    "zynq-am2.dtb",
                    "zynq-s19jpro.dtb",
                ),
                "AM2 device tree",
                required=True,
            ),
            "u-boot.img": select_candidate(
                source, ("u-boot.img",), "u-boot.img", required=False
            ),
            "system.bit": select_candidate(
                source,
                ("system.bit", "bitstream.bit", "fpga_bitstream.bit"),
                "FPGA bitstream",
                required=False,
            ),
            "uEnv.txt": select_candidate(
                source, ("uEnv.txt",), "uEnv.txt", required=False
            ),
        }
        snapshots: dict[str, Snapshot | None] = {}
        for output_name, selected_path in selected.items():
            if selected_path is None:
                snapshots[output_name] = None
                continue
            snapshot = snapshot_regular_file(selected_path, stage / output_name)
            validate_snapshot(stage, snapshot)
            snapshots[output_name] = snapshot

        current_source = os.lstat(source)
        if stable_directory_state(source_metadata) != stable_directory_state(current_source):
            fail("artifacts directory changed while its exact set was captured")

        semantic_manifest = write_stage_manifest(stage, snapshots)
        run_release_set(
            "manifest-stage",
            "--capability-file",
            os.fspath(capability),
            "--output",
            os.fspath(files_manifest),
        )
        validate_release_set_manifest(files_manifest, snapshots, semantic_manifest)
        release_name = output.name if not args.check_only else "am2-artifacts-check-only"
        run_release_set(
            "seal-stage",
            "--capability-file",
            os.fspath(capability),
            "--manifest",
            os.fspath(files_manifest),
            "--output-name",
            release_name,
        )

        if args.check_only:
            run_release_set("destroy-stage", "--capability-file", os.fspath(capability))
            stage_owned = False
            success_lines = [
                "stage_am2_sd_artifacts: READY (exact check-only snapshot validated)"
            ]
        else:
            run_release_set(
                "publish",
                "--capability-file",
                os.fspath(capability),
                "--output-parent",
                os.fspath(output.parent),
            )
            stage_owned = False
            success_lines = [
                "stage_am2_sd_artifacts: READY",
                f"  atomic stage: {output}",
                f"  evidence:     {output / 'artifacts.manifest.json'}",
                f"  release set:  {output / '.dcent-release-set.json'}",
            ]
    finally:
        cleanup_ok = cleanup_control(
            control,
            capability,
            files_manifest,
            destroy_stage=stage_owned,
        )
        if not cleanup_ok and sys.exc_info()[0] is None:
            fail("AM2 stage completed but private control cleanup failed")
    for line in success_lines:
        print(line)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(
        description=(
            "Validate and atomically stage BOOT.bin, uImage, and DTB for an AM2 "
            "S19j Pro SD image build."
        )
    )
    root.add_argument("--artifacts-dir", required=True)
    root.add_argument(
        "--output-dir",
        help="new atomic output directory (default: <artifacts-dir>.staged)",
    )
    root.add_argument(
        "--check-only",
        action="store_true",
        help="validate an exact private snapshot without publishing it",
    )
    return root


def main() -> int:
    try:
        stage_artifacts(parser().parse_args())
        return 0
    except (StageError, OSError, ValueError) as error:
        print(f"ERROR: stage_am2_sd_artifacts: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
