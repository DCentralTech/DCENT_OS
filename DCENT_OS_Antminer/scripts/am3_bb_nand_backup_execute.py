#!/usr/bin/env python3
"""Execute one strictly admitted, host-streamed AM3-BB NAND data backup.

Durable exact-file publication is supported on Linux and Windows hosts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, BinaryIO, Callable, Sequence

from durable_file_io import fsync_directory, mkdir_durable
from atomic_publish_file import (
    CommitSignalGuard,
    PublishError,
    atomic_publish as publish_staged_file,
    quarantine_failed_staging,
    report_after_commit,
    warn_after_commit,
)
from validate_am1_nand_backup import ValidationError
from validate_am3_bb_nand_backup import (
    BACKUP_SCOPE,
    EXPECTED_LAYOUTS,
    LAYOUT_NAME,
    RESTORE_AUTHORITY,
    RESULT_TYPE,
    TARGET_CLASS,
    validate_backup,
)
from validate_am3_bb_nand_backup_plan import load_plan, validate_plan


HOST_KEY_RE = re.compile(r"SHA256:[A-Za-z0-9+/]{43}")
SAFE_TOKEN_RE = re.compile(r"[A-Za-z0-9_.:-]+")
ENV_NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
EXPECTED_COMPATIBLE = "ti_am335x-bone-black"
EXPECTED_GEOMETRY = " ".join(
    f"mtd{number}:{size:08x}:00020000:{name}"
    for number, name, size in EXPECTED_LAYOUTS[LAYOUT_NAME]
)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--local-backup-dir", type=Path, required=True)
    parser.add_argument("--known-hosts", type=Path, required=True)
    parser.add_argument("--expected-host-key-sha256", required=True)
    parser.add_argument("--ssh-user", default="root")
    parser.add_argument("--ssh-password-env", default="MINER_PASSWORD")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--operator-authorized-backup", action="store_true", required=True)
    parser.add_argument("--readback-verify", action="store_true", required=True)
    parser.add_argument("--skip-size-check", action="store_true")
    args = parser.parse_args(argv)
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    return args


def require_regular(path: Path, label: str) -> None:
    if not path.is_file() or path.is_symlink():
        raise ValidationError(f"{label} must be a regular non-symlink file")


def pinned_fingerprint(target: str, known_hosts: Path) -> str:
    lookup = subprocess.run(
        ["ssh-keygen", "-F", target, "-f", str(known_hosts)],
        check=False,
        capture_output=True,
        text=True,
    )
    if lookup.returncode not in (0, 1):
        raise ValidationError("ssh-keygen could not inspect known_hosts")
    fingerprints: list[str] = []
    for line in lookup.stdout.splitlines():
        if not line or line.startswith("#"):
            continue
        result = subprocess.run(
            ["ssh-keygen", "-lf", "-", "-E", "sha256"],
            input=line + "\n",
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise ValidationError("known_hosts contains an unreadable key")
        columns = result.stdout.split()
        if len(columns) < 2:
            raise ValidationError("ssh-keygen returned a malformed fingerprint")
        fingerprints.append(columns[1])
    if len(fingerprints) != 1:
        raise ValidationError("known_hosts must contain exactly one key for target")
    return fingerprints[0]


class Executor:
    def __init__(self, args: argparse.Namespace, expected: dict[str, str], rows: list[str]) -> None:
        self.args = args
        self.expected = expected
        self.rows = rows
        self.output = args.local_backup_dir.resolve()
        self.log_path = self.output / "backup.log"
        self.ssh_base = [
            "ssh",
            "-o", "StrictHostKeyChecking=yes",
            "-o", f"UserKnownHostsFile={args.known_hosts.resolve()}",
            "-o", "GlobalKnownHostsFile=/dev/null",
            "-o", "ConnectTimeout=10",
            "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=3",
            "-o", "LogLevel=ERROR",
            f"{args.ssh_user}@{args.target}",
        ]
        password = os.environ.get(args.ssh_password_env, "")
        self.ssh_env = os.environ.copy()
        if password:
            if shutil.which("sshpass") is None:
                raise ValidationError("sshpass is required for password authentication")
            self.ssh_env["SSHPASS"] = password
            self.ssh_base = ["sshpass", "-e", *self.ssh_base]
        else:
            self.ssh_base[1:1] = ["-o", "BatchMode=yes"]

    def log(self, message: str) -> None:
        line = f"[{datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}] {message}"
        print(line)
        with self.log_path.open("a", encoding="utf-8") as handle:
            handle.write(line + "\n")
            handle.flush()
            os.fsync(handle.fileno())

    def ssh_text(self, command: str, timeout: int = 30) -> str:
        try:
            result = subprocess.run(
                [*self.ssh_base, command],
                check=False,
                capture_output=True,
                text=True,
                env=self.ssh_env,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired as error:
            raise ValidationError("SSH preflight timed out") from error
        if result.returncode != 0:
            raise ValidationError(f"SSH preflight failed (exit {result.returncode})")
        return result.stdout

    def ssh_stream(self, command: str, destination: BinaryIO) -> None:
        try:
            result = subprocess.run(
                [*self.ssh_base, command],
                check=False,
                stdout=destination,
                stderr=subprocess.PIPE,
                env=self.ssh_env,
                timeout=self.args.timeout,
            )
        except subprocess.TimeoutExpired as error:
            raise ValidationError("nanddump stream timed out") from error
        if result.stderr:
            with self.log_path.open("ab") as handle:
                handle.write(result.stderr)
                handle.flush()
                os.fsync(handle.fileno())
        if result.returncode != 0:
            raise ValidationError(f"nanddump stream failed (exit {result.returncode})")

    def preflight(self) -> tuple[str, str, str, str, str, str]:
        remote = self.ssh_text(
            "printf 'mac='; tr 'A-F' 'a-f' </sys/class/net/eth0/address 2>/dev/null | tr -d '\\r\\n'; printf '\\n'; "
            "printf 'hwid='; cat /config/CONF_HARDWARE_ID 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "printf 'model='; (cat /proc/device-tree/model 2>/dev/null || cat /sys/firmware/devicetree/base/model 2>/dev/null) | tr '\\000' '\\n' | sed 's/ /_/g' | sed -n '1p'; printf '\\n'; "
            "printf 'compatible='; (cat /proc/device-tree/compatible 2>/dev/null || cat /sys/firmware/devicetree/base/compatible 2>/dev/null) | tr '\\000' '\\n' | sed 's/,/_/g' | grep -Fx 'ti_am335x-bone-black' | sed -n '1p'; printf '\\n'; "
            "printf 'board_target='; cat /etc/dcentos/board_target 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "printf 'boot_id='; cat /proc/sys/kernel/random/boot_id 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "printf 'root_source='; awk '$2 == \"/\" {print $1; exit}' /proc/mounts 2>/dev/null; "
            "root_source=$(awk '$2 == \"/\" {print $1; exit}' /proc/mounts 2>/dev/null); "
            "root_base=${root_source#/dev/}; root_base=${root_base%p[0-9]*}; "
            "printf 'root_removable='; cat \"/sys/class/block/${root_base}/removable\" 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "echo mtd_begin; cat /proc/mtd 2>/dev/null; echo mtd_end; "
            "printf 'nanddump='; command -v nanddump 2>/dev/null || true; printf '\\n'; "
            "printf 'pgrep='; command -v pgrep 2>/dev/null || true; printf '\\n'; "
            "printf 'writable_mtd_mounts='; awk 'function isrw(options,n,parts,i) {n=split(options,parts,\",\"); for(i=1;i<=n;i++) if(parts[i]==\"rw\") return 1; return 0} ($1 ~ /^\\/dev\\/mtd(block)?[0-9]+$/ || $1 ~ /^mtd([0-9]+)?:/ || $1 ~ /^ubi[0-9]+:/ || $3 == \"jffs2\" || $3 == \"ubifs\") && isrw($4) {count++} END {print count+0}' /proc/mounts 2>/dev/null; "
            "miner_pids=$(pgrep -f '[l]uxminer|[b]osminer|[b]mminer|[c]gminer|[d]centrald' 2>/dev/null); miner_rc=$?; "
            "case $miner_rc in 0) printf 'miners_status=matches\\nminers=present\\n';; 1) printf 'miners_status=no_matches\\nminers=\\n';; *) printf 'miners_status=error\\nminers=error\\n';; esac"
        )
        values: dict[str, str] = {}
        for line in remote.splitlines():
            if "=" in line and not line.startswith("mtd"):
                key, value = line.split("=", 1)
                if key in values:
                    raise ValidationError(f"duplicate remote identity field: {key}")
                values[key] = value
        geometry_parts: list[str] = []
        block_lines: list[str] = []
        inside = False
        for line in remote.splitlines():
            if line == "mtd_begin":
                inside = True
                continue
            if line == "mtd_end":
                inside = False
                continue
            if inside:
                block_lines.append(line)
            match = re.fullmatch(r'mtd(\d+): ([0-9A-Fa-f]{8}) ([0-9A-Fa-f]{8}) "([A-Za-z0-9_.-]+)"', line)
            if inside and match:
                number, size, erase, name = match.groups()
                geometry_parts.append(f"mtd{int(number)}:{size.lower()}:{erase.lower()}:{name}")
        geometry = " ".join(geometry_parts)
        required = {"mac", "hwid", "model", "compatible", "board_target", "boot_id", "root_source", "root_removable", "nanddump", "pgrep", "writable_mtd_mounts", "miners_status", "miners"}
        if set(values) != required:
            raise ValidationError("remote preflight fields are not exact")
        for key in ("mac", "hwid", "model", "board_target"):
            if values[key] != self.expected[key]:
                raise ValidationError(f"live {key} does not match the validated plan")
        if values["compatible"] != EXPECTED_COMPATIBLE:
            raise ValidationError("live compatible token is not exact AM3-BB")
        if values["root_source"] != self.expected["root_device"]:
            raise ValidationError("live root source does not match the plan-bound device")
        if values["root_removable"] != "1":
            raise ValidationError("live root device is not marked removable/external")
        if (
            len(block_lines) != 13
            or re.fullmatch(r"dev:\s+size\s+erasesize\s+name", block_lines[0]) is None
            or len(geometry_parts) != 12
            or geometry != EXPECTED_GEOMETRY
        ):
            raise ValidationError("exact ordered /proc/mtd geometry mismatch")
        if not values["nanddump"].startswith("/"):
            raise ValidationError("target nanddump tool is unavailable")
        if not values["pgrep"].startswith("/"):
            raise ValidationError("target pgrep tool is unavailable")
        if values["miners_status"] != "no_matches" or values["miners"]:
            raise ValidationError("mining process status is not an exact no-match result")
        if values["writable_mtd_mounts"] != "0":
            raise ValidationError("writable MTD/UBI mount is present or unclassified")
        if re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", values["boot_id"]) is None:
            raise ValidationError("target boot_id is malformed or unavailable")
        return values["mac"], values["hwid"], values["model"], values["compatible"], values["board_target"], values["boot_id"]

    def runtime_gate(self, expected_boot_id: str) -> None:
        remote = self.ssh_text(
            "printf 'boot_id='; cat /proc/sys/kernel/random/boot_id 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "root_source=$(awk '$2 == \"/\" {print $1; exit}' /proc/mounts 2>/dev/null); printf 'root_source=%s\\n' \"$root_source\"; "
            "root_base=${root_source#/dev/}; root_base=${root_base%p[0-9]*}; printf 'root_removable='; cat \"/sys/class/block/${root_base}/removable\" 2>/dev/null | tr -d '[:space:]'; printf '\\n'; "
            "printf 'pgrep='; command -v pgrep 2>/dev/null || true; printf '\\n'; "
            "printf 'writable_mtd_mounts='; awk 'function isrw(options,n,parts,i) {n=split(options,parts,\",\"); for(i=1;i<=n;i++) if(parts[i]==\"rw\") return 1; return 0} ($1 ~ /^\\/dev\\/mtd(block)?[0-9]+$/ || $1 ~ /^mtd([0-9]+)?:/ || $1 ~ /^ubi[0-9]+:/ || $3 == \"jffs2\" || $3 == \"ubifs\") && isrw($4) {count++} END {print count+0}' /proc/mounts 2>/dev/null; "
            "miner_pids=$(pgrep -f '[l]uxminer|[b]osminer|[b]mminer|[c]gminer|[d]centrald' 2>/dev/null); miner_rc=$?; "
            "case $miner_rc in 0) printf 'miners_status=matches\\nminers=present\\n';; 1) printf 'miners_status=no_matches\\nminers=\\n';; *) printf 'miners_status=error\\nminers=error\\n';; esac"
        )
        values: dict[str, str] = {}
        for line in remote.splitlines():
            if "=" not in line:
                raise ValidationError("runtime gate returned a malformed line")
            key, value = line.split("=", 1)
            if key in values:
                raise ValidationError(f"duplicate runtime gate field: {key}")
            values[key] = value
        expected_keys = {
            "boot_id", "root_source", "root_removable", "pgrep",
            "writable_mtd_mounts", "miners_status", "miners",
        }
        if set(values) != expected_keys:
            raise ValidationError("runtime gate fields are not exact")
        if values["boot_id"] != expected_boot_id:
            raise ValidationError("target rebooted during backup")
        if values["root_source"] != self.expected["root_device"] or values["root_removable"] != "1":
            raise ValidationError("external removable root admission changed during backup")
        if not values["pgrep"].startswith("/"):
            raise ValidationError("runtime pgrep tool is unavailable")
        if values["miners_status"] != "no_matches" or values["miners"]:
            raise ValidationError("miner state changed or could not be classified")
        if values["writable_mtd_mounts"] != "0":
            raise ValidationError("writable MTD/UBI mount appeared during backup")

    @staticmethod
    def sha256(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def publish(
        staging: Path,
        destination: Path,
        *,
        before_commit: Callable[[], None] | None = None,
    ) -> None:
        try:
            if before_commit is None:
                _, staged_cleanup = publish_staged_file(
                    staging,
                    destination,
                    require_directory_sync=True,
                )
            else:
                _, staged_cleanup = publish_staged_file(
                    staging,
                    destination,
                    require_directory_sync=True,
                    _after_staged_open=before_commit,
                )
        except PublishError as error:
            raise ValidationError(
                f"cannot publish artifact {destination.name}: {error}"
            ) from error
        if staged_cleanup != "removed":
            warn_after_commit(
                f"WARN: published {destination.name} but retained staging "
                f"name {staging}"
            )

    def run(self) -> Path:
        mkdir_durable(self.output, mode=0o700, parents=True, exist_ok=False)
        os.chmod(self.output, 0o700)
        self.log_path.touch(mode=0o600, exist_ok=False)
        fsync_directory(self.output)
        if not self.args.skip_size_check:
            free = shutil.disk_usage(self.output).free
            required = 280 * 1024 * 1024
            if free < required:
                raise ValidationError(f"insufficient host space: {free} < {required} bytes")
        self.log("strict AM3-BB host-side backup started")
        mac, hwid, model, compatible, board_target, boot_id = self.preflight()
        self.log("identity, external-root, stopped-miner, and exact geometry gates passed")

        parsed_rows: dict[int, tuple[str, int, str]] = {}
        for row in self.rows:
            number_text, name, size_text, artifact = row.split("|", 3)
            parsed_rows[int(number_text)] = (name, int(size_text), artifact)
        order = ([5] if 5 in parsed_rows else []) + [
            number for number in sorted(parsed_rows) if number != 5
        ]
        results: dict[int, dict[str, Any]] = {}
        sums: list[str] = []
        for number in order:
            name, expected_size, artifact = parsed_rows[number]
            self.runtime_gate(boot_id)
            self.log(f"reading mtd{number} ({name}), exact bytes={expected_size}")
            first_fd, first_name = tempfile.mkstemp(prefix=f".{artifact}.first.", dir=self.output)
            read_fd, read_name = tempfile.mkstemp(prefix=f".{artifact}.readback.", dir=self.output)
            os.close(first_fd)
            os.close(read_fd)
            first = Path(first_name)
            readback = Path(read_name)
            try:
                with first.open("wb") as handle:
                    self.ssh_stream(f"nanddump --bb=padbad --omitoob /dev/mtd{number}", handle)
                    handle.flush()
                    os.fsync(handle.fileno())
                if first.stat().st_size != expected_size:
                    raise ValidationError(f"first stream size mismatch for {artifact}")
                first_sha = self.sha256(first)
                with readback.open("wb") as handle:
                    self.ssh_stream(f"nanddump --bb=padbad --omitoob /dev/mtd{number}", handle)
                    handle.flush()
                    os.fsync(handle.fileno())
                if readback.stat().st_size != expected_size or self.sha256(readback) != first_sha:
                    raise ValidationError(f"readback mismatch for {artifact}")
                self.runtime_gate(boot_id)
                readback.unlink()
                self.publish(first, self.output / artifact)
            finally:
                first.unlink(missing_ok=True)
                readback.unlink(missing_ok=True)
            sums.append(f"{first_sha}  {artifact}\n")
            results[number] = {
                "device": f"/dev/mtd{number}", "mtd_number": number, "name": name,
                "size_bytes": expected_size, "artifact": artifact, "sha256": first_sha,
                "actual_bytes": expected_size, "status": "pass",
            }
            self.log(f"accepted {artifact} sha256={first_sha}")

        self.runtime_gate(boot_id)
        sums_path = self.output / "SHA256SUMS"
        with sums_path.open("x", encoding="ascii") as handle:
            handle.writelines(sums)
            handle.flush()
            os.fsync(handle.fileno())
        fsync_directory(self.output)
        contract = EXPECTED_LAYOUTS[LAYOUT_NAME]
        total = sum(item[2] for item in contract)
        manifest = {
            "schema_version": "1.0.0",
            "type": RESULT_TYPE,
            "execution_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "target": {
                "ip": self.args.target, "mac": mac, "hwid": hwid, "model": model,
                "compatible": compatible, "authorized_board_target": board_target,
                "backup_scope": BACKUP_SCOPE, "restore_authority": RESTORE_AUTHORITY,
                "class": TARGET_CLASS, "layout": LAYOUT_NAME,
            },
            "readback_verify": 1,
            "readback_failures": 0,
            "partitions": [results[number] for number, _, _ in contract],
            "verification": {
                "expected_artifact_count": len(contract), "actual_artifact_count": len(contract),
                "fail_count": 0, "readback_failures": 0, "total_bytes": total,
                "sha256sums_file": "SHA256SUMS", "log_file": self.log_path.name,
            },
            "nand_backup_complete": "pass",
        }
        destination = self.output / "am3_bb_nand_backup.manifest.json"
        with CommitSignalGuard(
            "durable AM3-BB NAND backup result publication", ValidationError
        ) as termination:
            termination.refuse_pending_before_commit()
            fd, temporary_name = tempfile.mkstemp(
                prefix=f".{destination.name}.publication-pending.",
                dir=self.output,
            )
            temporary = Path(temporary_name)
            owned_fd: int | None = fd
            committed = False
            try:
                handle = os.fdopen(
                    fd, "w", encoding="utf-8", newline="\n"
                )
                owned_fd = None
                with handle:
                    json.dump(manifest, handle, indent=2)
                    handle.write("\n")
                    handle.flush()
                    os.fsync(handle.fileno())
                validate_backup(
                    temporary,
                    self.output,
                    self.args.target,
                    mac,
                    hwid,
                    model,
                    compatible,
                    board_target,
                    max_age_seconds=86400,
                )
                termination.refuse_pending_before_commit()
                self.publish(
                    temporary,
                    destination,
                    before_commit=termination.refuse_pending_before_commit,
                )
                committed = True
                termination.mark_committed()
            except (OSError, ValidationError) as error:
                try:
                    quarantine = quarantine_failed_staging(temporary, destination)
                except (OSError, PublishError) as quarantine_error:
                    raise ValidationError(
                        f"manifest commit failed: {error}; failed staging could not "
                        f"be quarantined or neutralized: {quarantine_error}"
                    ) from error
                detail = (
                    f"; failed staging retained as {quarantine}"
                    if quarantine
                    else ""
                )
                raise ValidationError(
                    f"manifest commit failed: {error}{detail}"
                ) from error
            finally:
                if owned_fd is not None:
                    try:
                        os.close(owned_fd)
                    except OSError:
                        pass
                if committed:
                    try:
                        temporary.unlink(missing_ok=True)
                    except OSError:
                        # The shared publisher treats a retained hidden staging name
                        # as cleanup debt after an authoritative destination commit.
                        pass
            try:
                self.log(
                    "nand_backup_complete=pass "
                    "(data-only-no-oob; restore_authority=none)"
                )
            except Exception as error:
                warn_after_commit(
                    "WARN: committed AM3-BB result, but final informational log did "
                    f"not persist: {error}"
                )
            report_after_commit(
                (
                    f"manifest={destination}",
                    "nand_backup_complete=pass",
                    f"backup_scope={BACKUP_SCOPE}",
                    f"restore_authority={RESTORE_AUTHORITY}",
                )
            )
        return destination


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if os.environ.get("DCENT_NAND_BACKUP_AUTHORIZED") != "1":
            raise ValidationError("DCENT_NAND_BACKUP_AUTHORIZED must equal 1")
        for value, label in ((args.target, "target"), (args.ssh_user, "ssh user")):
            if SAFE_TOKEN_RE.fullmatch(value) is None:
                raise ValidationError(f"unsafe {label}")
        if ENV_NAME_RE.fullmatch(args.ssh_password_env) is None:
            raise ValidationError("unsafe password variable name")
        require_regular(args.plan, "plan")
        require_regular(args.known_hosts, "known_hosts")
        if HOST_KEY_RE.fullmatch(args.expected_host_key_sha256) is None:
            raise ValidationError("malformed expected host-key fingerprint")
        for tool in ("ssh", "ssh-keygen"):
            if shutil.which(tool) is None:
                raise ValidationError(f"required host tool is missing: {tool}")
        with args.plan.open(encoding="utf-8") as handle:
            plan = load_plan(str(args.plan), handle)
        _, _, endpoint, target, mac, hwid, model, plan_host_key, rows = validate_plan(plan)
        if args.target != endpoint:
            raise ValidationError("--target does not match validated plan endpoint")
        if args.expected_host_key_sha256 != plan_host_key:
            raise ValidationError("host-key fingerprint does not match validated plan")
        if pinned_fingerprint(args.target, args.known_hosts) != plan_host_key:
            raise ValidationError("pinned known_hosts fingerprint does not match plan")
        if args.local_backup_dir.exists() or args.local_backup_dir.is_symlink():
            raise ValidationError("backup directory must be fresh and absent")
        pre_flight = plan["pre_flight"]
        executor = Executor(
            args,
            {
                "mac": mac,
                "hwid": hwid,
                "model": model,
                "board_target": target,
                "root_device": pre_flight["sd_recovery_root_device"],
            },
            rows,
        )
        executor.run()
    except (ValidationError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("nand_backup_complete=fail", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
