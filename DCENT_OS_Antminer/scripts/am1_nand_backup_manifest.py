#!/usr/bin/env python3
"""Generate or validate local-only AM1 NAND backup evidence."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Sequence

from atomic_publish_file import (
    CommitSignalGuard,
    PublishError,
    atomic_publish as publish_staged_file,
    quarantine_failed_staging,
    report_after_commit,
    warn_after_commit,
)
from durable_file_io import mkdir_durable
from validate_am1_nand_backup import (
    BACKUP_SCOPE,
    EXPECTED_LAYOUTS,
    RESTORE_AUTHORITY,
    STRICT_TARGET_FIELDS,
    ValidationError,
    validate_backup,
)


MTD_RE = re.compile(
    r'^mtd(\d+): ([0-9A-Fa-f]{8}) ([0-9A-Fa-f]{8}) "([A-Za-z0-9_.-]+)"$'
)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate", action="store_true")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--local-backup-dir", type=Path)
    parser.add_argument("--expected-target")
    parser.add_argument("--expected-mac")
    parser.add_argument("--expected-hwid")
    parser.add_argument("--expected-model")
    parser.add_argument("--expected-compatible")
    parser.add_argument("--expected-board-target")
    parser.add_argument("--expected-host-key-sha256")
    parser.add_argument("--max-age-seconds", type=int, default=86400)
    args = parser.parse_args(argv)
    if args.validate:
        if args.manifest is None:
            parser.error("--validate requires --manifest")
        if args.evidence is not None or args.output is not None:
            parser.error("--validate cannot be combined with generation arguments")
    elif args.evidence is None or args.manifest is not None:
        parser.error("generation requires --evidence and does not accept --manifest")
    if args.max_age_seconds <= 0:
        parser.error("--max-age-seconds must be positive")
    return args


def regular_file(path: Path, label: str) -> Path:
    if not path.is_file() or path.is_symlink():
        raise ValidationError(f"{label} must be a regular non-symlink file")
    return path


def parse_evidence(path: Path) -> list[tuple[int, str, int, int]]:
    try:
        lines = regular_file(path, "evidence").read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"cannot read evidence: {error}") from error
    sections: list[list[str]] = []
    inside = False
    current: list[str] = []
    for line in lines:
        if line == "=== mtd layout ===":
            if inside:
                raise ValidationError("nested MTD evidence section")
            inside = True
            current = []
            continue
        if inside and line.startswith("=== "):
            sections.append(current)
            inside = False
        elif inside and line.startswith("mtd"):
            current.append(line)
    if inside:
        sections.append(current)
    if len(sections) != 1 or not sections[0]:
        raise ValidationError("evidence must contain exactly one nonempty MTD layout section")
    rows: list[tuple[int, str, int, int]] = []
    for line in sections[0]:
        match = MTD_RE.fullmatch(line)
        if match is None:
            raise ValidationError("MTD evidence row is malformed")
        number, size, erase, name = match.groups()
        rows.append((int(number), name, int(size, 16), int(erase, 16)))
    return rows


def identify_layout(rows: list[tuple[int, str, int, int]]) -> str | None:
    observed = [(number, name, size) for number, name, size, erase in rows if erase == 0x20000]
    if len(observed) != len(rows):
        return None
    for layout, expected in EXPECTED_LAYOUTS.items():
        if observed == list(expected):
            return layout
    return None


def atomic_publish(
    path: Path,
    payload: bytes,
    *,
    before_commit: Callable[[], None] | None = None,
) -> None:
    mkdir_durable(path.parent, parents=True, exist_ok=True)
    if path.exists() or path.is_symlink():
        raise ValidationError(f"refusing to replace existing output: {path}")
    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.publication-pending.",
        dir=path.parent,
    )
    temporary = Path(temporary_name)
    committed = False
    try:
        os.chmod(temporary, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        if before_commit is None:
            _, staged_cleanup = publish_staged_file(
                temporary,
                path,
                require_directory_sync=True,
            )
        else:
            _, staged_cleanup = publish_staged_file(
                temporary,
                path,
                require_directory_sync=True,
                _after_staged_open=before_commit,
            )
        committed = True
        if staged_cleanup != "removed":
            warn_after_commit(
                f"WARN: published {path} but retained staging name {temporary}"
            )
    except (OSError, PublishError, ValidationError) as error:
        try:
            quarantine = quarantine_failed_staging(temporary, path)
        except (OSError, PublishError) as quarantine_error:
            raise ValidationError(
                f"cannot publish manifest: {error}; failed staging could not be "
                f"quarantined or neutralized: {quarantine_error}"
            ) from error
        detail = f"; failed staging retained as {quarantine}" if quarantine else ""
        raise ValidationError(f"cannot publish manifest: {error}{detail}") from error
    finally:
        if committed:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass


def generate(
    args: argparse.Namespace,
    *,
    before_commit: Callable[[], None] | None = None,
) -> tuple[Path, str | None]:
    rows = parse_evidence(args.evidence)
    layout = identify_layout(rows)
    output = args.output
    if output is None:
        suffix = "_mtd_backup_manifest.md"
        output = args.evidence.with_name(args.evidence.stem + suffix)
    profile = 1 if layout is not None else 0
    scheme = layout or "unknown"
    generated = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    table = "\n".join(
        f"| /dev/mtd{number} | 0x{size:08x} | 0x{erase:08x} | {name} | mtd{number}_{name}.nanddump |"
        for number, name, size, erase in rows
    )
    payload = f"""# AM1 (S9 Zynq) MTD Backup Manifest

- Created: `{generated}`
- Source evidence: `{args.evidence.name}`
- Status: planning-only manifest
- `layout_profile_candidate={profile}`
- `partition_scheme={scheme}`
- `partition_count={len(rows)}`
- `backup_scope={BACKUP_SCOPE}`
- `restore_authority={RESTORE_AUTHORITY}`
- `nand_backup_execute_go=0`
- `nand_write_go=0`
- `persistent_install_go=0`

## Partition Table

| Node | Size Hex | Erase Hex | Name | Required Artifact |
| --- | --- | --- | --- | --- |
{table}

## Decision

This local planning manifest authorizes no NAND read, write, restore, or install.
Use the strict plan builder and pinned-host executor; never stage raw NAND on the miner.
""".encode("utf-8")
    atomic_publish(output, payload, before_commit=before_commit)
    return output, layout


def validate(args: argparse.Namespace) -> None:
    backup_dir = args.local_backup_dir or args.manifest.parent
    expected_metadata = {
        "backup_scope": BACKUP_SCOPE,
        "restore_authority": RESTORE_AUTHORITY,
    }
    for field, value in (
        ("model", args.expected_model),
        ("compatible", args.expected_compatible),
        ("authorized_board_target", args.expected_board_target),
        ("ssh_host_key_sha256", args.expected_host_key_sha256),
    ):
        if value is not None:
            expected_metadata[field] = value
    result = validate_backup(
        regular_file(args.manifest, "manifest"),
        backup_dir,
        args.expected_target,
        args.expected_mac,
        args.expected_hwid,
        required_target_fields=STRICT_TARGET_FIELDS,
        expected_target_metadata=expected_metadata,
        max_age_seconds=args.max_age_seconds,
    )
    for artifact, digest in result.artifacts:
        print(f"PASS: {artifact} (sha={digest})")
    print(f"target_ip={result.target_ip}")
    print("partition_names=" + " ".join(result.partition_names))
    print("partition_geometry=" + " ".join(result.partition_geometry))
    print("manifest_validation=pass")


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.validate:
            validate(args)
        else:
            with CommitSignalGuard(
                "durable AM1 NAND planning manifest publication",
                ValidationError,
            ) as termination:
                output, layout = generate(
                    args,
                    before_commit=termination.refuse_pending_before_commit,
                )
                termination.mark_committed()
                report_after_commit(
                    (
                        f"wrote={output}",
                        f"partition_scheme={layout or 'unknown'}",
                        f"layout_profile_candidate={1 if layout is not None else 0}",
                    )
                )
    except (ValidationError, OSError, json.JSONDecodeError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("manifest_validation=fail" if args.validate else "layout_profile_candidate=0", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main())
