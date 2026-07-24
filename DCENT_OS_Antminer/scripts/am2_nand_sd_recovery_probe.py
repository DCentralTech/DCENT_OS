#!/usr/bin/env python3
"""Produce a private exact-unit AM2 external-SD recovery receipt."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Sequence

from am1_nand_backup_execute import (
    ENV_NAME_RE,
    HOST_KEY_RE,
    SAFE_TOKEN_RE,
    pinned_fingerprint,
)
from atomic_publish_file import (
    CommitSignalGuard,
    PublishError,
    atomic_publish as publish_staged_file,
    quarantine_failed_staging,
    report_after_commit,
    warn_after_commit,
)
from durable_file_io import mkdir_durable
from validate_am1_nand_backup import MAC_RE, ValidationError
from validate_am2_nand_backup import EXPECTED_LAYOUTS, LAYOUT_NAME


BOOT_ID_RE = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
ROOT_DEVICE_RE = re.compile(r"/dev/mmcblk\d+p\d+")
AUTHORIZED_COMPATIBLE = "xlnx_zynq-7000"
AUTHORIZED_BOARD_TARGET = "am2-s19jpro-zynq"
AUTHORIZED_MODEL_KEY = "antminers19jpro"
DEFAULT_ARTIFACT_DIR = (
    Path(__file__).resolve().parents[3]
    / "docs"
    / "dev"
    / "2026-05-15-am2-sd-recovery"
    / "evidence"
)

REMOTE_SCRIPT = r"""
printf 'mac='; tr 'A-F' 'a-f' </sys/class/net/eth0/address 2>/dev/null | tr -d '\r\n'; printf '\n'
printf 'hwid='; cat /config/CONF_HARDWARE_ID 2>/dev/null | tr -d '[:space:]'; printf '\n'
printf 'model='; cat /config/CONF_MINER_TYPE 2>/dev/null | tr -d '[:space:]'; printf '\n'
printf 'compatible='; (cat /proc/device-tree/compatible 2>/dev/null || cat /sys/firmware/devicetree/base/compatible 2>/dev/null) | tr '\000' '\n' | sed 's/,/_/g' | sed -n '1p'; printf '\n'
printf 'board_target='; cat /etc/dcentos/board_target 2>/dev/null | tr -d '[:space:]'; printf '\n'
printf 'boot_id='; cat /proc/sys/kernel/random/boot_id 2>/dev/null | tr -d '[:space:]'; printf '\n'
root_source=$(awk '$2 == "/" {print $1; exit}' /proc/mounts 2>/dev/null); printf 'root_source=%s\n' "$root_source"
root_base=${root_source#/dev/}; root_base=${root_base%p[0-9]*}; printf 'root_removable='; cat "/sys/class/block/${root_base}/removable" 2>/dev/null | tr -d '[:space:]'; printf '\n'
echo mtd_begin
cat /proc/mtd 2>/dev/null
echo mtd_end
printf 'nanddump='; command -v nanddump 2>/dev/null || true; printf '\n'
printf 'pgrep='; command -v pgrep 2>/dev/null || true; printf '\n'
printf 'writable_mtd_mounts='; awk 'function isrw(options,n,parts,i) {n=split(options,parts,","); for(i=1;i<=n;i++) if(parts[i]=="rw") return 1; return 0} ($1 ~ /^\/dev\/mtd(block)?[0-9]+$/ || $1 ~ /^mtd([0-9]+)?:/ || $1 ~ /^ubi[0-9]+:/ || $3 == "jffs2" || $3 == "ubifs") && isrw($4) {count++} END {print count+0}' /proc/mounts 2>/dev/null
miner_pids=$(pgrep -f '[b]osminer|[b]mminer|[c]gminer|[d]centrald' 2>/dev/null); miner_rc=$?; case $miner_rc in 0) printf 'miners_status=matches\nminers=present\n';; 1) printf 'miners_status=no_matches\nminers=\n';; *) printf 'miners_status=error\nminers=error\n';; esac
writer_pids=$(pgrep -f '[f]w_setenv|[n]andwrite|[n]andtest|[n]andmarkbad|[f]lash_erase|[f]lashcp|[m]td_debug|[m]tdpart|[u]biformat|[u]biupdatevol|[u]bimkvol|[u]birmvol|[u]birsvol|[u]biattach|[u]bidetach|[s]ysupgrade|[o]pkg|[/]dev/[m]td|[/]dev/[u]bi|(^|/|[[:space:]])[d]d([[:space:]]|$)' 2>/dev/null); writer_rc=$?; case $writer_rc in 0) printf 'writers_status=matches\nwriters=present\n';; 1) printf 'writers_status=no_matches\nwriters=\n';; *) printf 'writers_status=error\nwriters=error\n';; esac
"""


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("--artifact-dir", type=Path, default=DEFAULT_ARTIFACT_DIR)
    parser.add_argument("--known-hosts", type=Path, required=True)
    parser.add_argument("--expected-host-key-sha256", required=True)
    parser.add_argument("--expect-mac", required=True)
    parser.add_argument("--expect-hwid", required=True)
    parser.add_argument("--expect-model", required=True)
    parser.add_argument("--expect-compatible", required=True)
    parser.add_argument("--expect-target", required=True)
    parser.add_argument(
        "--ssh-user",
        default=os.environ.get("DCENT_AM2_RECOVERY_SSH_USER", "root"),
    )
    parser.add_argument("--ssh-password-env", default="DCENT_PASSWORD")
    return parser.parse_args(argv)


def exact_fields(remote: str) -> tuple[dict[str, str], list[str]]:
    values: dict[str, str] = {}
    block: list[str] = []
    inside = False
    block_count = 0
    terminator_count = 0
    for line in remote.splitlines():
        if line == "mtd_begin":
            if inside or block_count != 0:
                raise ValidationError("remote proof must contain exactly one MTD block")
            block_count += 1
            inside = True
            continue
        if line == "mtd_end":
            if not inside or terminator_count != 0:
                raise ValidationError("remote proof must contain exactly one MTD terminator")
            terminator_count += 1
            inside = False
            continue
        if inside:
            block.append(line)
        elif "=" in line:
            key, value = line.split("=", 1)
            if key in values:
                raise ValidationError(f"remote proof field is duplicated: {key}")
            values[key] = value
        elif line:
            raise ValidationError("remote proof contains an unclassified line")
    if inside or block_count != 1 or terminator_count != 1:
        raise ValidationError("remote proof must contain one complete MTD block")
    expected = {
        "mac",
        "hwid",
        "model",
        "compatible",
        "board_target",
        "boot_id",
        "root_source",
        "root_removable",
        "nanddump",
        "pgrep",
        "writable_mtd_mounts",
        "miners_status",
        "miners",
        "writers_status",
        "writers",
    }
    if set(values) != expected:
        raise ValidationError("remote proof fields are not exact")
    return values, block


def validate_geometry(block: list[str]) -> None:
    contract = EXPECTED_LAYOUTS[LAYOUT_NAME]
    if (
        len(block) != len(contract) + 1
        or re.fullmatch(r"dev:\s+size\s+erasesize\s+name", block[0]) is None
    ):
        raise ValidationError("MTD evidence block shape is not exact")
    observed: list[tuple[int, str, int]] = []
    for line in block[1:]:
        match = re.fullmatch(
            r'mtd(\d+): ([0-9A-Fa-f]{8}) ([0-9A-Fa-f]{8}) "([A-Za-z0-9_.-]+)"',
            line,
        )
        if match is None:
            raise ValidationError("MTD evidence row is malformed")
        number, size, erase, name = match.groups()
        if erase.lower() != "00020000":
            raise ValidationError("MTD erase geometry is not 128 KiB")
        observed.append((int(number), name, int(size, 16)))
    if observed != list(contract):
        raise ValidationError("exact ordered AM2 dual-slot MTD geometry mismatch")


def ssh_probe(args: argparse.Namespace) -> str:
    ssh = [
        "ssh",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        f"UserKnownHostsFile={args.known_hosts.resolve()}",
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        "-o",
        "ConnectTimeout=8",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=1",
        "-o",
        "LogLevel=ERROR",
        f"{args.ssh_user}@{args.target}",
        "sh -s",
    ]
    environment = os.environ.copy()
    password = os.environ.get(args.ssh_password_env, "")
    if password:
        if shutil.which("sshpass") is None:
            raise ValidationError("sshpass is required for password authentication")
        environment["SSHPASS"] = password
        ssh = ["sshpass", "-e", *ssh]
    else:
        ssh[1:1] = ["-o", "BatchMode=yes"]
    try:
        result = subprocess.run(
            ssh,
            input=REMOTE_SCRIPT,
            text=True,
            capture_output=True,
            check=False,
            timeout=30,
            env=environment,
        )
    except subprocess.TimeoutExpired as error:
        raise ValidationError("recovery probe SSH timed out") from error
    if result.returncode != 0:
        raise ValidationError(f"recovery probe SSH failed (exit {result.returncode})")
    return result.stdout


def publish_receipt(
    args: argparse.Namespace,
    values: dict[str, str],
    *,
    before_commit: Callable[[], None] | None = None,
) -> Path:
    root = args.artifact_dir
    if root.exists() or root.is_symlink():
        if root.is_symlink() or not root.is_dir():
            raise ValidationError("artifact directory must be a real directory")
    else:
        mkdir_durable(root, mode=0o700, parents=True, exist_ok=False)
    os.chmod(root, 0o700)
    now = datetime.now(timezone.utc)
    stamp = now.strftime("%Y%m%dT%H%M%SZ")
    safe_target = re.sub(r"[^A-Za-z0-9_.=-]", "-", args.target)
    destination = root / (
        f"am2_{safe_target}_{stamp}_{os.getpid()}_sd_recovery_probe.txt"
    )
    fields = {
        "schema": "am2_sd_recovery_proof_v1",
        "timestamp_utc": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "ip": args.target,
        "contract": "read_only_external_boot_recovery_probe",
        "ssh_host_key_authentication": "verified",
        "ssh_host_key_sha256": args.expected_host_key_sha256,
        "identity_mac": values["mac"],
        "identity_hwid": values["hwid"],
        "identity_model": values["model"],
        "identity_compatible": values["compatible"],
        "identity_target": values["board_target"],
        "boot_id": values["boot_id"],
        "root_source": values["root_source"],
        "root_removable": "1",
        "identity": "pass am2_zynq_exact_unit",
        "stock_xil_detected": "0",
        "external_boot": "pass root_device_exact_removable_mmc",
        "mtd_geometry": "pass exact_am2_braiinsos_dual_slot_10_partition",
        "quiescence": "pass_known_writer_scan_clear_no_writable_mtd",
        "nand_backup_execute_go": "0",
        "nand_write_go": "0",
        "persistent_install_go": "0",
        "sd_recovery_probe": "pass",
    }
    payload = "".join(f"{key}={value}\n" for key, value in fields.items()).encode()
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.publication-pending.",
        dir=root,
    )
    temporary = Path(temporary_name)
    committed = False
    try:
        os.chmod(temporary, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        if before_commit is None:
            _, staged_cleanup = publish_staged_file(
                temporary,
                destination,
                require_directory_sync=True,
            )
        else:
            _, staged_cleanup = publish_staged_file(
                temporary,
                destination,
                require_directory_sync=True,
                _after_staged_open=before_commit,
            )
        committed = True
        if staged_cleanup != "removed":
            warn_after_commit(
                f"WARN: published {destination} but retained staging name {temporary}"
            )
    except (OSError, PublishError, ValidationError) as error:
        try:
            quarantine = quarantine_failed_staging(temporary, destination)
        except (OSError, PublishError) as quarantine_error:
            raise ValidationError(
                f"cannot publish recovery proof: {error}; failed staging could not be "
                f"quarantined or neutralized: {quarantine_error}"
            ) from error
        detail = f"; failed staging retained as {quarantine}" if quarantine else ""
        raise ValidationError(f"cannot publish recovery proof: {error}{detail}") from error
    finally:
        if committed:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                pass
    return destination


def report_committed_receipt(destination: Path) -> None:
    """Report an already-durable receipt without retroactively failing it."""
    report_after_commit(
        (
            f"proof={destination}",
            "sd_recovery_probe=pass",
            "nand_write_go=0",
            "persistent_install_go=0",
        )
    )


def publish_and_report_receipt(
    args: argparse.Namespace, values: dict[str, str]
) -> None:
    with CommitSignalGuard(
        "durable AM2 SD recovery proof publication", ValidationError
    ) as termination:
        destination = publish_receipt(
            args,
            values,
            before_commit=termination.refuse_pending_before_commit,
        )
        termination.mark_committed()
        report_committed_receipt(destination)


def validate_remote(args: argparse.Namespace, remote: str) -> dict[str, str]:
    values, block = exact_fields(remote)
    expected = {
        "mac": args.expect_mac,
        "hwid": args.expect_hwid,
        "model": args.expect_model,
        "compatible": args.expect_compatible,
        "board_target": args.expect_target,
    }
    for key, value in expected.items():
        if values[key] != value:
            raise ValidationError(f"recovery {key} does not match expected unit")
    if values["compatible"] != AUTHORIZED_COMPATIBLE:
        raise ValidationError("recovery compatible is not the authorized AM2 Zynq SoC")
    if values["board_target"] != AUTHORIZED_BOARD_TARGET:
        raise ValidationError("recovery board target is outside the authorized backup lane")
    model_key = re.sub(r"[^a-z0-9]", "", values["model"].lower())
    if model_key != AUTHORIZED_MODEL_KEY:
        raise ValidationError("recovery model is outside the authorized target/model map")
    if BOOT_ID_RE.fullmatch(values["boot_id"]) is None:
        raise ValidationError("target boot_id is malformed or unavailable")
    if ROOT_DEVICE_RE.fullmatch(values["root_source"]) is None:
        raise ValidationError("root source is not an exact external mmc partition")
    if values["root_removable"] != "1":
        raise ValidationError("root mmc device is not marked removable/external")
    validate_geometry(block)
    if not values["nanddump"].startswith("/"):
        raise ValidationError("target nanddump tool is unavailable")
    if not values["pgrep"].startswith("/"):
        raise ValidationError("target pgrep tool is unavailable")
    if values["miners_status"] != "no_matches" or values["miners"]:
        raise ValidationError("mining process state is not an exact no-match result")
    if values["writers_status"] != "no_matches" or values["writers"]:
        raise ValidationError("flash/update writer state is not an exact no-match result")
    if values["writable_mtd_mounts"] != "0":
        raise ValidationError("writable MTD/UBI mount is present or unclassified")
    return values


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        for value, label in (
            (args.target, "target"),
            (args.expect_hwid, "expected HWID"),
            (args.expect_model, "expected model"),
            (args.expect_compatible, "expected compatible"),
            (args.expect_target, "expected board target"),
            (args.ssh_user, "SSH user"),
        ):
            if SAFE_TOKEN_RE.fullmatch(value) is None:
                raise ValidationError(f"unsafe {label}")
        args.expect_mac = args.expect_mac.lower()
        if MAC_RE.fullmatch(args.expect_mac) is None:
            raise ValidationError("--expect-mac is malformed")
        if ENV_NAME_RE.fullmatch(args.ssh_password_env) is None:
            raise ValidationError("invalid password environment variable name")
        if HOST_KEY_RE.fullmatch(args.expected_host_key_sha256) is None:
            raise ValidationError("malformed OpenSSH SHA256 fingerprint")
        if args.expect_compatible != AUTHORIZED_COMPATIBLE:
            raise ValidationError(
                f"--expect-compatible must be {AUTHORIZED_COMPATIBLE}"
            )
        if args.expect_target != AUTHORIZED_BOARD_TARGET:
            raise ValidationError(
                f"--expect-target must be {AUTHORIZED_BOARD_TARGET}"
            )
        if re.sub(r"[^a-z0-9]", "", args.expect_model.lower()) != AUTHORIZED_MODEL_KEY:
            raise ValidationError("--expect-model is outside the authorized target/model map")
        if not args.known_hosts.is_file() or args.known_hosts.is_symlink():
            raise ValidationError("--known-hosts must be a regular non-symlink file")
        for tool in ("ssh", "ssh-keygen"):
            if shutil.which(tool) is None:
                raise ValidationError(f"missing host tool: {tool}")
        if (
            pinned_fingerprint(args.target, args.known_hosts)
            != args.expected_host_key_sha256
        ):
            raise ValidationError("known_hosts fingerprint does not match expected key")
        values = validate_remote(args, ssh_probe(args))
        publish_and_report_receipt(args, values)
    except (ValidationError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("sd_recovery_probe=fail", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    raise SystemExit(main())
