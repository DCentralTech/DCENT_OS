#!/usr/bin/env python3
"""Build a strict exact-unit AM1 NAND backup plan without target access.

Durable exact-file publication is supported on Linux and Windows hosts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Sequence

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
    AUTHORIZED_BOARD_TARGET,
    EXPECTED_LAYOUTS,
    MAC_RE,
    SAFE_TARGET_RE,
    SHA256_RE,
    ValidationError,
)
from validate_am1_nand_backup_plan import (
    MIN_FREE_MB,
    PLAN_TYPE,
    TARGET_CLASS,
    validate_plan,
)


HOST_KEY_RE = re.compile(r"SHA256:[A-Za-z0-9+/]{43}")
UTC_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
TABLE_RE = re.compile(
    r"^\| (/dev/mtd\d+) \| (0x[0-9a-f]{8}) \| (0x[0-9a-f]{8}) "
    r"\| ([A-Za-z0-9_.-]+) \| ([A-Za-z0-9_.-]+) \|$"
)
SD_KEYS = {
    "schema",
    "timestamp_utc",
    "ip",
    "ssh_host_key_authentication",
    "ssh_host_key_sha256",
    "identity_mac",
    "identity_hwid",
    "identity_model",
    "identity_compatible",
    "identity_target",
    "root_source",
    "root_removable",
    "identity",
    "external_boot",
    "mtd_layout",
    "mtd_geometry",
    "nand_backup_execute_go",
    "nand_write_go",
    "persistent_install_go",
    "sd_recovery_probe",
}
RESTORE_KEYS = {
    "schema",
    "restore_verified",
    "restore_mac",
    "restore_hwid",
    "restore_model",
    "restore_compatible",
    "restore_target",
    "restore_artifact_name",
    "restore_artifact_sha256",
    "restore_verified_utc",
}


def regular_file(path: Path, label: str) -> Path:
    if not path.is_file() or path.is_symlink():
        raise ValidationError(f"{label} must be a regular non-symlink file")
    return path


def fields(path: Path, label: str, expected_keys: set[str]) -> tuple[dict[str, str], bytes]:
    result: dict[str, str] = {}
    try:
        raw = regular_file(path, label).read_bytes()
        lines = raw.decode("utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"cannot read {label}: {error}") from error
    for number, line in enumerate(lines, 1):
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValidationError(f"{label} line {number} is not key=value")
        key, value = line.split("=", 1)
        if key in result:
            raise ValidationError(f"{label} has duplicate field {key!r}")
        result[key] = value
    if set(result) != expected_keys:
        raise ValidationError(f"{label} fields are not exact")
    return result, raw


def parse_utc(value: str, label: str) -> datetime:
    if UTC_RE.fullmatch(value) is None:
        raise ValidationError(f"{label} is not an exact UTC timestamp")
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError as error:
        raise ValidationError(f"{label} is not a valid UTC timestamp") from error


def require_fresh(value: str, label: str, now: datetime, max_age: int) -> None:
    observed = parse_utc(value, label)
    age = (now - observed).total_seconds()
    if age < 0:
        raise ValidationError(f"{label} must not be in the future")
    if age >= max_age:
        raise ValidationError(f"{label} is at least {max_age} seconds old")


def exact_identity(value: str, label: str) -> str:
    if SAFE_TARGET_RE.fullmatch(value) is None:
        raise ValidationError(f"{label} contains unsafe characters")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_manifest(path: Path, expected_layout: str) -> list[tuple[int, str, int]]:
    if expected_layout not in EXPECTED_LAYOUTS:
        raise ValidationError("--expect-layout is not a known AM1 layout")
    try:
        lines = regular_file(path, "manifest").read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise ValidationError(f"cannot read manifest: {error}") from error
    expected_contract = {
        "layout_profile_candidate": "1",
        "partition_scheme": expected_layout,
        "partition_count": str(len(EXPECTED_LAYOUTS[expected_layout])),
        "backup_scope": "data-only-no-oob",
        "restore_authority": "none-until-physical-rehearsal",
        "nand_backup_execute_go": "0",
        "nand_write_go": "0",
        "persistent_install_go": "0",
    }
    observed_contract: dict[str, str] = {}
    for line in lines:
        match = re.fullmatch(r"- `([a-z0-9_]+)=([^`]+)`", line)
        if match is None:
            continue
        key, value = match.groups()
        if key in observed_contract:
            raise ValidationError(f"manifest has duplicate contract marker: {key}")
        observed_contract[key] = value
    if observed_contract != expected_contract:
        raise ValidationError("manifest contract markers are not exact")
    rows: list[tuple[int, str, int]] = []
    for line in lines:
        match = TABLE_RE.fullmatch(line)
        if match is None:
            continue
        device, size_hex, erase_hex, name, artifact = match.groups()
        number = int(device.removeprefix("/dev/mtd"))
        size = int(size_hex, 16)
        if erase_hex != "0x00020000":
            raise ValidationError("manifest erase geometry is not 128 KiB")
        if artifact != f"mtd{number}_{name}.nanddump":
            raise ValidationError("manifest artifact name is not canonical")
        rows.append((number, name, size))
    if rows != list(EXPECTED_LAYOUTS[expected_layout]):
        raise ValidationError(f"manifest does not contain exact ordered {expected_layout} geometry")
    return rows


def atomic_publish(path: Path, payload: bytes) -> None:
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
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        _, staged_cleanup = publish_staged_file(
            temporary,
            path,
            require_directory_sync=True,
        )
        committed = True
        if staged_cleanup != "removed":
            warn_after_commit(
                f"WARN: published {path} but retained staging name {temporary}"
            )
    except (OSError, PublishError) as error:
        try:
            quarantine = quarantine_failed_staging(temporary, path)
        except (OSError, PublishError) as quarantine_error:
            raise ValidationError(
                f"cannot publish {path}: {error}; failed staging could not be "
                f"quarantined or neutralized: {quarantine_error}"
            ) from error
        detail = f"; failed staging retained as {quarantine}" if quarantine else ""
        raise ValidationError(f"cannot publish {path}: {error}{detail}") from error
    finally:
        if committed:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                # A successfully linked destination remains authoritative. A
                # retained hidden staging name is cleanup debt, not false failure.
                pass


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--restore-artifact-proof", dest="restore_proof", type=Path, required=True)
    parser.add_argument("--restore-artifact", type=Path, required=True)
    parser.add_argument("--sd-recovery-proof", type=Path, required=True)
    parser.add_argument("--expect-layout", choices=sorted(EXPECTED_LAYOUTS), required=True)
    parser.add_argument("--expect-ip", required=True)
    parser.add_argument("--expect-host-key-sha256", required=True)
    parser.add_argument("--expect-mac", required=True)
    parser.add_argument("--expect-hwid", required=True)
    parser.add_argument("--expect-model", required=True)
    parser.add_argument("--expect-compatible", required=True)
    parser.add_argument("--expect-target", required=True)
    parser.add_argument("--readback-verify", action="store_true", required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--json-template", type=Path)
    args = parser.parse_args(argv)
    stem = args.manifest
    if args.output is None:
        name = stem.name.removesuffix("_mtd_backup_manifest.md")
        args.output = stem.with_name(name + "_backup_plan.md")
    if args.json_template is None:
        name = stem.name.removesuffix("_mtd_backup_manifest.md")
        args.json_template = stem.with_name(name + "_backup_plan.json")
    return args


def build(args: argparse.Namespace, now: datetime) -> tuple[bytes, bytes]:
    rows = validate_manifest(args.manifest, args.expect_layout)
    sd, sd_receipt = fields(args.sd_recovery_proof, "SD recovery proof", SD_KEYS)
    restore, restore_receipt = fields(args.restore_proof, "restore proof", RESTORE_KEYS)
    expected_mac = args.expect_mac.lower()
    if MAC_RE.fullmatch(expected_mac) is None:
        raise ValidationError("--expect-mac is malformed")
    for value, label in (
        (args.expect_ip, "--expect-ip"),
        (args.expect_hwid, "--expect-hwid"),
        (args.expect_model, "--expect-model"),
        (args.expect_compatible, "--expect-compatible"),
        (args.expect_target, "--expect-target"),
    ):
        exact_identity(value, label)
    if args.expect_target != AUTHORIZED_BOARD_TARGET:
        raise ValidationError(
            f"--expect-target must be the canonical AM1 target {AUTHORIZED_BOARD_TARGET!r}"
        )
    if HOST_KEY_RE.fullmatch(args.expect_host_key_sha256) is None:
        raise ValidationError("--expect-host-key-sha256 is malformed")

    required_sd = {
        "schema": "am1_sd_recovery_proof_v1",
        "ssh_host_key_authentication": "verified",
        "identity": "pass am1_zynq_s9",
        "external_boot": "pass root_device_exact_removable_mmc",
        "mtd_layout": args.expect_layout,
        "mtd_geometry": f"pass exact_am1_{args.expect_layout}_partition",
        "nand_backup_execute_go": "0",
        "nand_write_go": "0",
        "persistent_install_go": "0",
        "sd_recovery_probe": "pass",
        "root_removable": "1",
    }
    for key, value in required_sd.items():
        if sd[key] != value:
            raise ValidationError(f"SD recovery proof field {key!r} is not exact")
    expected_pairs = {
        "ip": args.expect_ip,
        "ssh_host_key_sha256": args.expect_host_key_sha256,
        "identity_mac": expected_mac,
        "identity_hwid": args.expect_hwid,
        "identity_model": args.expect_model,
        "identity_compatible": args.expect_compatible,
        "identity_target": args.expect_target,
    }
    for key, value in expected_pairs.items():
        if sd[key] != value:
            raise ValidationError(f"SD recovery proof {key} does not match expected unit")
    if re.fullmatch(r"/dev/mmcblk\d+p\d+", sd["root_source"]) is None:
        raise ValidationError("SD recovery proof root_source is not an exact mmc partition")
    require_fresh(sd["timestamp_utc"], "SD recovery proof timestamp", now, 86400)

    required_restore = {
        "schema": "am1_restore_artifact_proof_v1",
        "restore_verified": "1",
        "restore_mac": expected_mac,
        "restore_hwid": args.expect_hwid,
        "restore_model": args.expect_model,
        "restore_compatible": args.expect_compatible,
        "restore_target": args.expect_target,
    }
    for key, value in required_restore.items():
        if restore[key] != value:
            raise ValidationError(f"restore proof {key} does not match expected unit")
    if SHA256_RE.fullmatch(restore["restore_artifact_sha256"]) is None:
        raise ValidationError("restore proof SHA256 is malformed")
    restore_artifact = regular_file(args.restore_artifact, "restore artifact")
    if restore["restore_artifact_name"] != restore_artifact.name:
        raise ValidationError("restore proof artifact name does not match supplied artifact")
    if sha256_file(restore_artifact) != restore["restore_artifact_sha256"]:
        raise ValidationError("restore proof SHA256 does not match supplied artifact bytes")
    require_fresh(restore["restore_verified_utc"], "restore proof timestamp", now, 30 * 86400)

    generated = now.strftime("%Y-%m-%dT%H:%M:%SZ")
    partitions = []
    total = 0
    for number, name, size in rows:
        partitions.append(
            {
                "device": f"/dev/mtd{number}",
                "mtd_number": number,
                "name": name,
                "size_hex": f"0x{size:08x}",
                "size_bytes": size,
                "erase_size_hex": "0x00020000",
                "artifact": f"mtd{number}_{name}.nanddump",
                "sha256": None,
                "actual_bytes": None,
                "readback_sha256": None,
                "status": "pending",
            }
        )
        total += size
    plan = {
        "schema_version": "1.0.0",
        "type": PLAN_TYPE,
        "generated_utc": generated,
        "target": {"class": TARGET_CLASS, "layout": args.expect_layout, "partition_count": len(partitions)},
        "nand_backup_execute_go": 0,
        "plan_ready": 1,
        "readback_verify": 1,
        "pre_flight": {
            "restore_artifact_proof": "restore_verified_identity_matched",
            "restore_matched_mac": expected_mac,
            "restore_matched_hwid": args.expect_hwid,
            "restore_matched_model": args.expect_model,
            "restore_matched_target": args.expect_target,
            "restore_matched_compatible": args.expect_compatible,
            "restore_artifact_sha256": restore["restore_artifact_sha256"],
            "restore_proof_sha256": hashlib.sha256(restore_receipt).hexdigest(),
            "restore_verified_utc": restore["restore_verified_utc"],
            "sd_recovery_probe": "external_boot_identity_matched",
            "sd_recovery_ip": args.expect_ip,
            "sd_recovery_verified_utc": sd["timestamp_utc"],
            "sd_recovery_host_key_sha256": args.expect_host_key_sha256,
            "sd_recovery_layout": args.expect_layout,
            "sd_recovery_root_device": sd["root_source"],
            "sd_recovery_root_removable": 1,
            "sd_recovery_proof_sha256": hashlib.sha256(sd_receipt).hexdigest(),
            "layout_profile": 1,
            "operator_approval": False,
            "mining_stopped": False,
            "storage_adequate": False,
            "min_free_mb": MIN_FREE_MB,
        },
        "partitions": partitions,
        "verification": {
            "all_artifacts_exist": None,
            "all_artifacts_nonempty": None,
            "all_sha256_match": None,
            "readback_idempotent": None,
            "total_expected_bytes": total,
            "nand_backup_complete": None,
        },
    }
    validate_plan(plan, now=now)
    encoded = (json.dumps(plan, indent=2) + "\n").encode("utf-8")
    plan_sha = hashlib.sha256(encoded).hexdigest()
    markdown = f"""# AM1 NAND Backup Execution Plan

- Generated: `{generated}`
- Exact endpoint: `{args.expect_ip}`
- Exact MAC/HWID: `{expected_mac}` / `{args.expect_hwid}`
- Pinned SSH host key: `{args.expect_host_key_sha256}`
- Exact layout: `{args.expect_layout}` ({len(partitions)} partitions, {total} bytes)
- JSON plan SHA256: `{plan_sha}`
- `plan_ready=1`
- `nand_backup_execute_go=0`
- `nand_write_go=0`
- `persistent_install_go=0`

This plan permits only an explicitly authorized, host-streamed, timeout-bounded
data-plane NAND read with mandatory identical readback. It omits OOB data and
therefore grants no restoration, NAND-write, or persistent-install authority.
Physical restore rehearsal remains required before any restore claim.
""".encode("utf-8")
    return markdown, encoded


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        output_key = str(args.output.resolve()).casefold()
        json_key = str(args.json_template.resolve()).casefold()
        if output_key == json_key:
            raise ValidationError("Markdown and JSON outputs must be different paths")
        for destination in (args.output, args.json_template):
            if destination.exists() or destination.is_symlink():
                raise ValidationError(f"refusing to replace existing output: {destination}")
        markdown, encoded = build(args, datetime.now(timezone.utc))
        # The validated JSON is executable authority and the commit marker.
        # Publish the non-executable review view before making that authority
        # discoverable, so interruption cannot leave JSON without Markdown.
        with CommitSignalGuard(
            "durable AM1 NAND backup plan publication", ValidationError
        ) as termination:
            termination.refuse_pending_before_commit()
            atomic_publish(args.output, markdown)
            termination.refuse_pending_before_commit()
            atomic_publish(args.json_template, encoded)
            termination.mark_committed()
            report_after_commit(
                (
                    f"wrote={args.output}",
                    f"json_template={args.json_template}",
                    "plan_ready=1",
                    "nand_backup_execute_go=0",
                )
            )
    except ValidationError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("plan_ready=0", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main())
