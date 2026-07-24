#!/usr/bin/env python3
"""Adversarial coverage for the AM2 NAND backup evidence boundary."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

import am2_nand_sd_recovery_probe as sd_probe
from am2_nand_sd_recovery_probe import exact_fields
from durable_file_io import run_self_test as run_durable_io_self_test
from validate_am1_nand_backup import ValidationError
from validate_am2_nand_backup import run_self_test as run_result_self_test
from validate_am2_nand_backup_plan import (
    fixture_plan,
    run_self_test as run_plan_self_test,
    validate_plan,
)


SCRIPT_DIR = Path(__file__).resolve().parent
MANIFEST_SCRIPT = SCRIPT_DIR / "am2_nand_backup_manifest.sh"
PLAN_SCRIPT = SCRIPT_DIR / "am2_nand_backup_plan.sh"
EXECUTE_SCRIPT = SCRIPT_DIR / "am2_nand_backup_execute.sh"
SD_RECOVERY_PROBE_SCRIPT = SCRIPT_DIR / "am2_sd_recovery_probe.sh"
SD_RECOVERY_PROBE_TOOL = SCRIPT_DIR / "am2_nand_sd_recovery_probe.py"
HOST_KEY_SHA256 = "SHA256:" + "A" * 43

GEOMETRY = (
    (0, "00800000", "boot"),
    (1, "00c00000", "boot-failover"),
    (2, "00200000", "fpga1"),
    (3, "00200000", "fpga2"),
    (4, "00080000", "uboot_env"),
    (5, "00080000", "miner_cfg"),
    (6, "05700000", "recovery"),
    (7, "03900000", "firmware1"),
    (8, "03900000", "firmware2"),
    (9, "01e00000", "factory"),
)


def wsl_path(path: Path) -> str:
    result = subprocess.run(
        ["wsl", "-e", "wslpath", "-a", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def bash_path(value: str) -> str:
    if os.name != "nt" or re.fullmatch(r"[A-Za-z]:[\\/].*", value) is None:
        return value
    return wsl_path(Path(value))


def run_bash(
    script: Path,
    *arguments: str,
    environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    if os.name == "nt":
        environment_args = [
            f"{key}={value}" for key, value in (environment or {}).items()
        ]
        command = [
            "wsl",
            "-e",
            "env",
            *environment_args,
            "bash",
            wsl_path(script),
            *(bash_path(argument) for argument in arguments),
        ]
        process_environment = None
    else:
        command = ["bash", str(script), *arguments]
        process_environment = os.environ.copy()
        process_environment.update(environment or {})
    return subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
        env=process_environment,
    )


def make_executable(path: Path) -> None:
    if os.name == "nt":
        subprocess.run(
            ["wsl", "-e", "chmod", "+x", wsl_path(path)],
            check=True,
            capture_output=True,
            text=True,
        )
    else:
        path.chmod(0o700)


def fake_path(directory: Path) -> str:
    prefix = wsl_path(directory) if os.name == "nt" else str(directory)
    return prefix + ":/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"


def install_fake_host_key_tools(root: Path, fake_bin: Path) -> Path:
    known_hosts = root / "known_hosts"
    known_hosts.write_text(
        "192.0.2.19 ssh-ed25519 AAAAFIXTURE\n", encoding="utf-8"
    )
    fake_keygen = fake_bin / "ssh-keygen"
    fake_keygen.write_bytes(
        (
            "#!/bin/sh\n"
            'case "$1" in\n'
            "  -F) printf '192.0.2.19 ssh-ed25519 AAAAFIXTURE\\n' ;;\n"
            f"  -lf) cat >/dev/null; printf '256 {HOST_KEY_SHA256} fixture (ED25519)\\n' ;;\n"
            "  *) exit 90 ;;\n"
            "esac\n"
        ).encode("utf-8")
    )
    make_executable(fake_keygen)
    return known_hosts


def evidence_text(geometry: tuple[tuple[int, str, str], ...] = GEOMETRY) -> str:
    rows = "\n".join(
        f'mtd{number}: {size} 00020000 "{name}"'
        for number, size, name in geometry
    )
    return f"=== mtd layout ===\n{rows}\n=== active slot ===\nfirmware=1\n"


def proof_text(*, verified_utc: datetime | None = None) -> str:
    stamp = verified_utc or datetime.now(timezone.utc)
    return "\n".join(
        (
            "restore_verified=1",
            "restore_mac=02:00:00:00:00:19",
            "restore_hwid=AM2-FIXTURE-19",
            "restore_model=Antminer-S19j-Pro",
            "restore_target=am2-s19jpro-zynq",
            f"restore_artifact_sha256={'1' * 64}",
            f"restore_verified_utc={stamp.strftime('%Y-%m-%dT%H:%M:%SZ')}",
            "",
        )
    )


def sd_recovery_proof_text(*, verified_utc: datetime | None = None) -> str:
    stamp = verified_utc or datetime.now(timezone.utc)
    return "\n".join(
        (
            "schema=am2_sd_recovery_proof_v1",
            f"timestamp_utc={stamp.strftime('%Y-%m-%dT%H:%M:%SZ')}",
            "ip=192.0.2.19",
            "contract=read_only_external_boot_recovery_probe",
            "ssh_host_key_authentication=verified",
            f"ssh_host_key_sha256={HOST_KEY_SHA256}",
            "identity_mac=02:00:00:00:00:19",
            "identity_hwid=AM2-FIXTURE-19",
            "identity_model=Antminer-S19j-Pro",
            "identity_compatible=xlnx_zynq-7000",
            "identity_target=am2-s19jpro-zynq",
            "boot_id=12345678-1234-1234-1234-123456789abc",
            "root_source=/dev/mmcblk0p2",
            "root_removable=1",
            "identity=pass am2_zynq_exact_unit",
            "stock_xil_detected=0",
            "external_boot=pass root_device_exact_removable_mmc",
            "mtd_geometry=pass exact_am2_braiinsos_dual_slot_10_partition",
            "quiescence=pass_known_writer_scan_clear_no_writable_mtd",
            "nand_backup_execute_go=0",
            "nand_write_go=0",
            "persistent_install_go=0",
            "sd_recovery_probe=pass",
            "",
        )
    )


def probe_remote_text(
    *,
    geometry: tuple[tuple[int, str, str], ...] = GEOMETRY,
    root_source: str = "/dev/mmcblk0p2",
    root_removable: str = "1",
    writable_mtd_mounts: str = "0",
    miners_status: str = "no_matches",
    miners: str = "",
    writers_status: str = "no_matches",
    writers: str = "",
    compatible: str = "xlnx_zynq-7000",
    board_target: str = "am2-s19jpro-zynq",
    pgrep: str = "/usr/bin/pgrep",
) -> str:
    rows = "\n".join(
        f'mtd{number}: {size} 00020000 "{name}"'
        for number, size, name in geometry
    )
    return "\n".join(
        (
            "mac=02:00:00:00:00:19",
            "hwid=AM2-FIXTURE-19",
            "model=Antminer-S19j-Pro",
            f"compatible={compatible}",
            f"board_target={board_target}",
            "boot_id=12345678-1234-1234-1234-123456789abc",
            f"root_source={root_source}",
            f"root_removable={root_removable}",
            "mtd_begin",
            "dev:    size   erasesize  name",
            rows,
            "mtd_end",
            "nanddump=/usr/sbin/nanddump",
            f"pgrep={pgrep}",
            f"writable_mtd_mounts={writable_mtd_mounts}",
            f"miners_status={miners_status}",
            f"miners={miners}",
            f"writers_status={writers_status}",
            f"writers={writers}",
            "",
        )
    )


def runtime_remote_text(
    *,
    root_source: str = "/dev/mmcblk0p2",
    root_removable: str = "1",
    writable_mtd_mounts: str = "0",
    miners_status: str = "no_matches",
    miners: str = "",
    writers_status: str = "no_matches",
    writers: str = "",
    boot_id: str = "12345678-1234-1234-1234-123456789abc",
    pgrep: str = "/usr/bin/pgrep",
) -> str:
    return "\n".join(
        (
            f"boot_id={boot_id}",
            f"root_source={root_source}",
            f"root_removable={root_removable}",
            f"pgrep={pgrep}",
            f"writable_mtd_mounts={writable_mtd_mounts}",
            f"miners_status={miners_status}",
            f"miners={miners}",
            f"writers_status={writers_status}",
            f"writers={writers}",
            "",
        )
    )
class Am2NandBackupEvidenceTests(unittest.TestCase):
    def test_executor_uses_the_strict_evidence_chain(self) -> None:
        source = EXECUTE_SCRIPT.read_text(encoding="utf-8")
        plan_validation = source.index('"$PYTHON_BIN" "$PLAN_VALIDATOR" --plan -')
        target_contact = source.index('OBSERVED_MAC="$(')
        result_validation = source.index('"$PYTHON_BIN" "$RESULT_VALIDATOR"')
        publication = source.index('"$MANIFEST_TMP" "$OUTPUT_MANIFEST"')
        evidence_flush = source.index(
            '"$LOGFILE" "$SHA256SUMS" "$RUNTIME_EVIDENCE"'
        )
        manifest_staging = source.index('MANIFEST_TMP="$(mktemp')
        scratch_retirement = source.index('rm -f -- "$RESULTS_TSV"')
        self.assertLess(plan_validation, target_contact)
        self.assertLess(result_validation, publication)
        self.assertLess(evidence_flush, manifest_staging)
        self.assertLess(scratch_retirement, publication)
        self.assertIn("nanddump --bb=padbad --omitoob", source)
        self.assertNotIn("nanddump --bb=skipbad", source)
        self.assertIn('[ "$LOCAL_SIZE" != "$size_bytes" ]', source)
        self.assertIn('[ "$READBACK_SIZE" != "$size_bytes" ]', source)
        self.assertNotIn("REMOTE_OUTDIR", source)
        self.assertNotIn("scp_fetch_timeout", source)
        self.assertIn('PASS_COUNT" = "$PARTITION_COUNT', source)
        self.assertIn('"authorized_board_target": "%s"', source)
        self.assertIn("StrictHostKeyChecking=yes", source)
        self.assertNotIn("StrictHostKeyChecking=no", source)
        self.assertIn('EXPECTED_HOST_KEY_SHA256" = "$PLAN_HOST_KEY_SHA256', source)
        self.assertIn('EXPECTED_ROOT_DEVICE="$(plan_value expected_root_device)"', source)
        self.assertGreaterEqual(source.count("runtime_gate"), 5)
        self.assertIn("writable_mtd_mounts", source)
        self.assertIn("miners_status", source)
        self.assertIn("writers_status", source)
        self.assertIn('OBSERVED_COMPATIBLE="$(', source)
        self.assertIn('"runtime_evidence_sha256": "%s"', source)
        self.assertIn("--require-directory-sync", source)
        self.assertIn("durable_file_io.py", source)
        self.assertIn(".publication-pending.", source)
        self.assertIn('log "nand_backup_complete=pass" || FINAL_LOG_STATUS=1', source)
        for writer in ("ubimkvol", "ubirmvol", "nandtest", "mtdpart"):
            scan_token = f"[{writer[0]}]{writer[1:]}"
            self.assertIn(scan_token, source)
            self.assertIn(
                scan_token,
                SD_RECOVERY_PROBE_TOOL.read_text(encoding="utf-8"),
            )
        self.assertNotIn("XC7Z020", source)

    def test_shared_durable_io_self_test(self) -> None:
        self.assertEqual(run_durable_io_self_test(), 0)

    def test_plan_publishes_executable_json_last(self) -> None:
        source = PLAN_SCRIPT.read_text(encoding="utf-8")
        markdown_publication = source.index('"$OUTPUT_TMP" "$OUTPUT"')
        json_publication = source.index('"$JSON_TMP" "$JSON_TEMPLATE"')
        self.assertLess(markdown_publication, json_publication)
        self.assertGreaterEqual(source.count("--require-directory-sync"), 2)
        self.assertIn("durable_file_io.py", source)
        self.assertGreaterEqual(source.count(".publication-pending."), 2)
        self.assertIn("trap cleanup_plan_tmp EXIT", source)
        self.assertIn("trap 'exit 1' HUP INT TERM", source)

    def test_probe_requires_one_exact_mtd_block(self) -> None:
        remote = probe_remote_text()
        second = "mtd_begin\ndev:    size   erasesize  name\nmtd_end\n"
        split = remote.replace("nanddump=", second + "nanddump=", 1)
        with self.assertRaisesRegex(ValidationError, "exactly one MTD block"):
            exact_fields(split)

    def test_sd_recovery_probe_requires_pinned_host_authentication(self) -> None:
        wrapper = SD_RECOVERY_PROBE_SCRIPT.read_text(encoding="utf-8")
        source = SD_RECOVERY_PROBE_TOOL.read_text(encoding="utf-8")
        self.assertIn("am2_nand_sd_recovery_probe.py", wrapper)
        self.assertIn("StrictHostKeyChecking=yes", source)
        self.assertNotIn("StrictHostKeyChecking=no", source)
        self.assertIn('"ssh_host_key_authentication": "verified"', source)
        self.assertIn('"ssh_host_key_sha256": args.expected_host_key_sha256', source)
        self.assertIn("exact ordered AM2 dual-slot MTD geometry mismatch", source)
        self.assertIn("root mmc device is not marked removable/external", source)

    def test_sd_receipt_publish_failure_quarantines_authoritative_staging(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-proof-publish-") as temp:
            root = Path(temp)
            args = argparse.Namespace(
                artifact_dir=root / "private",
                target="192.0.2.19",
                expected_host_key_sha256=HOST_KEY_SHA256,
            )
            values, _block = exact_fields(probe_remote_text())

            def reject_publication(
                staging: Path,
                destination: Path,
                *,
                require_directory_sync: bool = False,
            ) -> tuple[str, str]:
                self.assertTrue(require_directory_sync)
                self.assertIn(".publication-pending.", staging.name)
                raise sd_probe.PublishError("injected receipt commit failure")

            with mock.patch.object(
                sd_probe,
                "publish_staged_file",
                side_effect=reject_publication,
            ):
                with self.assertRaisesRegex(ValidationError, "retained as"):
                    sd_probe.publish_receipt(args, values)
            self.assertFalse(list(args.artifact_dir.glob("*_sd_recovery_probe.txt")))
            quarantine_directories = list(
                args.artifact_dir.glob(".*.publication-failed.*")
            )
            self.assertEqual(len(quarantine_directories), 1)
            self.assertIn(
                "sd_recovery_probe=pass",
                (quarantine_directories[0] / "staged").read_text(encoding="utf-8"),
            )

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_sd_receipt_signals_preserve_pre_and_post_commit_truth(self) -> None:
        for module_name in (
            "am1_nand_sd_recovery_probe",
            "am2_nand_sd_recovery_probe",
        ):
            for phase in ("before", "after"):
                with self.subTest(module=module_name, phase=phase):
                    with tempfile.TemporaryDirectory(
                        prefix=f"dcent-{module_name}-signal-"
                    ) as temp:
                        artifact_dir = Path(temp) / "private"
                        code = f"""
import argparse
import importlib
import os
from pathlib import Path
import signal
import sys

module = importlib.import_module({module_name!r})
args = argparse.Namespace(
    artifact_dir=Path({str(artifact_dir)!r}),
    target="192.0.2.19",
    expected_host_key_sha256={HOST_KEY_SHA256!r},
    expect_layout="stock",
)
values = {{
    "mac": "02:00:00:00:00:19",
    "hwid": "SIGNAL-FIXTURE-19",
    "model": "Antminer_S19j_Pro",
    "compatible": "xlnx_zynq-7000",
    "board_target": "am2-s19jpro-zynq",
    "boot_id": "12345678-1234-1234-1234-123456789abc",
    "root_source": "/dev/mmcblk0p2",
}}
real_publish = module.publish_staged_file

def inject_signal(*publish_args, **publish_kwargs):
    if {phase!r} == "before":
        refuse_pending = publish_kwargs["_after_staged_open"]
        def before_commit():
            os.kill(os.getpid(), signal.SIGTERM)
            refuse_pending()
        publish_kwargs["_after_staged_open"] = before_commit
        return real_publish(*publish_args, **publish_kwargs)
    result = real_publish(*publish_args, **publish_kwargs)
    os.kill(os.getpid(), signal.SIGTERM)
    return result

module.publish_staged_file = inject_signal
try:
    module.publish_and_report_receipt(args, values)
except module.ValidationError as error:
    print(f"ERROR: {{error}}", file=sys.stderr)
    raise SystemExit(1)
"""
                        result = subprocess.run(
                            [sys.executable, "-c", code],
                            cwd=SCRIPT_DIR,
                            stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE,
                            text=True,
                        )
                        pending = list(
                            artifact_dir.glob(".*.publication-pending.*")
                        )
                        proofs = list(artifact_dir.glob("*.txt"))
                        self.assertEqual(pending, [])
                        if phase == "before":
                            self.assertNotEqual(result.returncode, 0)
                            self.assertIn("before durable", result.stderr)
                            self.assertEqual(proofs, [])
                            self.assertEqual(
                                len(
                                    list(
                                        artifact_dir.glob(
                                            ".*.publication-failed.*"
                                        )
                                    )
                                ),
                                1,
                            )
                        else:
                            self.assertEqual(result.returncode, 0, result.stderr)
                            self.assertIn("ignored signal", result.stderr)
                            self.assertEqual(len(proofs), 1)
                            self.assertIn("sd_recovery_probe=pass", result.stdout)

    def test_sd_receipt_reporting_failure_cannot_revoke_commit(self) -> None:
        destination = Path("committed-am2-proof.txt")
        for name, reporting_error in (
            ("broken-pipe", BrokenPipeError()),
            ("closed-stream", ValueError("I/O operation on closed file")),
        ):
            with self.subTest(name=name):
                output = mock.Mock()
                if name == "broken-pipe":
                    output.flush.side_effect = reporting_error
                else:
                    output.write.side_effect = reporting_error
                with mock.patch.object(sd_probe.sys, "stdout", output):
                    sd_probe.report_committed_receipt(destination)
                if name == "broken-pipe":
                    output.flush.assert_called_once_with()

    def test_sd_receipt_reporting_survives_closed_consumer_finalization(self) -> None:
        for module_name in (
            "am1_nand_sd_recovery_probe",
            "am2_nand_sd_recovery_probe",
        ):
            with self.subTest(module=module_name):
                read_descriptor, write_descriptor = os.pipe()
                os.close(read_descriptor)
                try:
                    result = subprocess.run(
                        [
                            sys.executable,
                            "-c",
                            (
                                "from pathlib import Path; "
                                f"import {module_name} as probe; "
                                "probe.report_committed_receipt(Path('committed.txt'))"
                            ),
                        ],
                        cwd=SCRIPT_DIR,
                        stdout=write_descriptor,
                        stderr=subprocess.PIPE,
                        text=True,
                    )
                finally:
                    os.close(write_descriptor)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_sd_receipt_reporting_survives_closed_stdout_descriptor(self) -> None:
        for module_name in (
            "am1_nand_sd_recovery_probe",
            "am2_nand_sd_recovery_probe",
        ):
            with self.subTest(module=module_name):
                result = subprocess.run(
                    [
                        sys.executable,
                        "-c",
                        (
                            "import os, sys; from pathlib import Path; "
                            f"import {module_name} as probe; "
                            "os.close(sys.stdout.fileno()); "
                            "probe.report_committed_receipt(Path('committed.txt'))"
                        ),
                    ],
                    cwd=SCRIPT_DIR,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_sd_recovery_probe_records_verified_host_key(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-sd-probe-") as temp:
            root = Path(temp)
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            known_hosts = install_fake_host_key_tools(root, fake_bin)
            fake_ssh = fake_bin / "ssh"
            fake_ssh.write_bytes(
                (
                    "#!/bin/sh\ncat >/dev/null\ncat <<'EOF'\n"
                    + probe_remote_text()
                    + "EOF\n"
                ).encode("utf-8")
            )
            make_executable(fake_ssh)
            artifacts = root / "evidence"
            result = run_bash(
                SD_RECOVERY_PROBE_SCRIPT,
                "192.0.2.19",
                "--artifact-dir",
                str(artifacts),
                "--known-hosts",
                str(known_hosts),
                "--expected-host-key-sha256",
                HOST_KEY_SHA256,
                "--expect-mac",
                "02:00:00:00:00:19",
                "--expect-hwid",
                "AM2-FIXTURE-19",
                "--expect-model",
                "Antminer-S19j-Pro",
                "--expect-compatible",
                "xlnx_zynq-7000",
                "--expect-target",
                "am2-s19jpro-zynq",
                environment={"PATH": fake_path(fake_bin)},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            evidence_files = list(artifacts.glob("*_sd_recovery_probe.txt"))
            self.assertEqual(len(evidence_files), 1)
            evidence = evidence_files[0].read_text(encoding="utf-8")
            self.assertIn("ssh_host_key_authentication=verified", evidence)
            self.assertIn(f"ssh_host_key_sha256={HOST_KEY_SHA256}", evidence)
            self.assertIn("root_source=/dev/mmcblk0p2", evidence)
            self.assertIn(
                "mtd_geometry=pass exact_am2_braiinsos_dual_slot_10_partition",
                evidence,
            )
            self.assertIn(
                "quiescence=pass_known_writer_scan_clear_no_writable_mtd",
                evidence,
            )
            self.assertIn("sd_recovery_probe=pass", evidence)

    def test_sd_recovery_probe_rejects_false_green_inputs(self) -> None:
        wrong_geometry = list(GEOMETRY)
        wrong_geometry[4] = (4, "00020000", "uboot_env")
        cases = {
            "nand-root": probe_remote_text(root_source="/dev/ubiblock0_0"),
            "non-removable": probe_remote_text(root_removable="0"),
            "wrong-geometry": probe_remote_text(geometry=tuple(wrong_geometry)),
            "miner-active": probe_remote_text(
                miners_status="matches", miners="present"
            ),
            "writer-active": probe_remote_text(
                writers_status="matches", writers="present"
            ),
            "writable-mtd": probe_remote_text(writable_mtd_mounts="1"),
            "wrong-compatible": probe_remote_text(compatible="not_zynq"),
            "wrong-target": probe_remote_text(board_target="am2-s19pro"),
            "missing-pgrep": probe_remote_text(pgrep=""),
            "miner-scan-error": probe_remote_text(
                miners_status="error", miners="error"
            ),
            "writer-scan-error": probe_remote_text(
                writers_status="error", writers="error"
            ),
            "duplicate-field": probe_remote_text() + "mac=02:00:00:00:00:19\n",
            "unknown-field": probe_remote_text() + "unclassified=1\n",
            "second-mtd-block": probe_remote_text().replace(
                "nanddump=",
                "mtd_begin\ndev:    size   erasesize  name\nmtd_end\nnanddump=",
                1,
            ),
        }
        for name, remote in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory(
                prefix="dcent-am2-sd-probe-"
            ) as temp:
                root = Path(temp)
                fake_bin = root / "fake-bin"
                fake_bin.mkdir()
                known_hosts = install_fake_host_key_tools(root, fake_bin)
                fake_ssh = fake_bin / "ssh"
                fake_ssh.write_bytes(
                    (
                        "#!/bin/sh\ncat >/dev/null\ncat <<'EOF'\n"
                        + remote
                        + "EOF\n"
                    ).encode("utf-8")
                )
                make_executable(fake_ssh)
                artifacts = root / "evidence"
                result = run_bash(
                    SD_RECOVERY_PROBE_SCRIPT,
                    "192.0.2.19",
                    "--artifact-dir",
                    str(artifacts),
                    "--known-hosts",
                    str(known_hosts),
                    "--expected-host-key-sha256",
                    HOST_KEY_SHA256,
                    "--expect-mac",
                    "02:00:00:00:00:19",
                    "--expect-hwid",
                    "AM2-FIXTURE-19",
                    "--expect-model",
                    "Antminer-S19j-Pro",
                    "--expect-compatible",
                    "xlnx_zynq-7000",
                    "--expect-target",
                    "am2-s19jpro-zynq",
                    environment={"PATH": fake_path(fake_bin)},
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("sd_recovery_probe=fail", result.stderr)
                self.assertFalse(
                    list(artifacts.glob("*_sd_recovery_probe.txt"))
                )

    def test_executor_rejects_stale_plan_before_target_contact(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-executor-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            stale = datetime.now(timezone.utc) - timedelta(days=2)
            stamp = stale.strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = stamp
            plan["pre_flight"]["restore_verified_utc"] = stamp
            plan_path = root / "stale-plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")
            output = root / "must-not-exist"
            result = run_bash(
                EXECUTE_SCRIPT,
                "--target",
                "192.0.2.19",
                "--plan",
                str(plan_path),
                "--operator-authorized-backup",
                "--readback-verify",
                "--local-backup-dir",
                str(output),
                environment={"DCENT_NAND_BACKUP_AUTHORIZED": "1"},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("plan is at least 86400 seconds old", result.stderr)
            self.assertFalse(output.exists())

    def test_executor_rejects_host_key_different_from_plan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-executor-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = now
            plan["pre_flight"]["restore_verified_utc"] = now
            plan["pre_flight"]["sd_recovery_verified_utc"] = now
            plan_path = root / "plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")
            known_hosts = root / "known_hosts"
            known_hosts.write_text(
                "192.0.2.19 ssh-ed25519 AAAAFIXTURE\n", encoding="utf-8"
            )
            output = root / "must-not-exist"
            result = run_bash(
                EXECUTE_SCRIPT,
                "--target",
                "192.0.2.19",
                "--plan",
                str(plan_path),
                "--known-hosts",
                str(known_hosts),
                "--expected-host-key-sha256",
                "SHA256:" + "B" * 43,
                "--operator-authorized-backup",
                "--readback-verify",
                "--local-backup-dir",
                str(output),
                environment={"DCENT_NAND_BACKUP_AUTHORIZED": "1"},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "does not match the SD-recovery proof bound into the plan",
                result.stderr,
            )
            self.assertFalse(output.exists())

    def test_executor_rejects_live_identity_mismatch_before_geometry(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-executor-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = now
            plan["pre_flight"]["restore_verified_utc"] = now
            plan["pre_flight"]["sd_recovery_verified_utc"] = now
            plan_path = root / "plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")

            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            fake_ssh = fake_bin / "ssh"
            fake_ssh.write_bytes(
                b"#!/bin/sh\n"
                b"case \"$*\" in\n"
                b"  */sys/class/net/eth0/address*) printf '02:00:00:00:00:20\\n' ;;\n"
                b"  */config/CONF_HARDWARE_ID*) printf 'AM2-FIXTURE-19\\n' ;;\n"
                b"  */config/CONF_MINER_TYPE*) printf 'Antminer-S19j-Pro\\n' ;;\n"
                b"  */proc/device-tree/compatible*) printf 'xlnx_zynq-7000\\n' ;;\n"
                b"  */etc/dcentos/board_target*) printf 'am2-s19jpro-zynq\\n' ;;\n"
                b"  *) exit 91 ;;\n"
                b"esac\n"
            )
            make_executable(fake_ssh)
            known_hosts = install_fake_host_key_tools(root, fake_bin)
            output = root / "identity-failure"
            result = run_bash(
                EXECUTE_SCRIPT,
                "--target",
                "192.0.2.19",
                "--plan",
                str(plan_path),
                "--known-hosts",
                str(known_hosts),
                "--expected-host-key-sha256",
                HOST_KEY_SHA256,
                "--operator-authorized-backup",
                "--readback-verify",
                "--skip-size-check",
                "--local-backup-dir",
                str(output),
                environment={
                    "DCENT_NAND_BACKUP_AUTHORIZED": "1",
                    "PATH": fake_path(fake_bin),
                },
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Target identity mismatch", result.stdout + result.stderr)
            self.assertNotIn("/proc/mtd", result.stdout + result.stderr)

    def test_executor_rejects_live_geometry_drift_before_nanddump(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-executor-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = now
            plan["pre_flight"]["restore_verified_utc"] = now
            plan["pre_flight"]["sd_recovery_verified_utc"] = now
            plan_path = root / "plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")

            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            fake_ssh = fake_bin / "ssh"
            fake_ssh.write_bytes(
                b"#!/bin/sh\n"
                b"case \"$*\" in\n"
                b"  */sys/class/net/eth0/address*) printf '02:00:00:00:00:19\\n' ;;\n"
                b"  */config/CONF_HARDWARE_ID*) printf 'AM2-FIXTURE-19\\n' ;;\n"
                b"  */config/CONF_MINER_TYPE*) printf 'Antminer-S19j-Pro\\n' ;;\n"
                b"  */proc/device-tree/compatible*) printf 'xlnx_zynq-7000\\n' ;;\n"
                b"  */etc/dcentos/board_target*) printf 'am2-s19jpro-zynq\\n' ;;\n"
                b"  *writable_mtd_mounts=*) printf 'boot_id=12345678-1234-1234-1234-123456789abc\\nroot_source=/dev/mmcblk0p2\\nroot_removable=1\\npgrep=/usr/bin/pgrep\\nwritable_mtd_mounts=0\\nminers_status=no_matches\\nminers=\\nwriters_status=no_matches\\nwriters=\\n' ;;\n"
                b"  *'cat /proc/mtd'*) printf 'dev: size erasesize name\\n' ; "
                b"printf 'mtd0: 00800000 00020000 \\\"boot\\\"\\n' ;;\n"
                b"  *nanddump*) printf 'nanddump-was-reached\\n'; exit 92 ;;\n"
                b"  *) exit 91 ;;\n"
                b"esac\n"
            )
            make_executable(fake_ssh)
            known_hosts = install_fake_host_key_tools(root, fake_bin)
            output = root / "geometry-failure"
            result = run_bash(
                EXECUTE_SCRIPT,
                "--target",
                "192.0.2.19",
                "--plan",
                str(plan_path),
                "--known-hosts",
                str(known_hosts),
                "--expected-host-key-sha256",
                HOST_KEY_SHA256,
                "--operator-authorized-backup",
                "--readback-verify",
                "--skip-size-check",
                "--local-backup-dir",
                str(output),
                environment={
                    "DCENT_NAND_BACKUP_AUTHORIZED": "1",
                    "PATH": fake_path(fake_bin),
                },
            )
            combined = result.stdout + result.stderr
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Exact ordered /proc/mtd geometry mismatch", combined)
            self.assertNotIn("nanddump-was-reached", combined)

    def test_executor_rejects_unsafe_runtime_before_nanddump(self) -> None:
        cases = {
            "nand-root": (
                runtime_remote_text(root_source="/dev/ubiblock0_0"),
                "External removable root admission",
            ),
            "non-removable": (
                runtime_remote_text(root_removable="0"),
                "External removable root admission",
            ),
            "writable-mtd": (
                runtime_remote_text(writable_mtd_mounts="1"),
                "Writable MTD/UBI mount",
            ),
            "miner-active": (
                runtime_remote_text(miners_status="matches", miners="present"),
                "Miner process state",
            ),
            "writer-active": (
                runtime_remote_text(
                    writers_status="matches", writers="present"
                ),
                "Flash/update writer state",
            ),
            "pgrep-error": (
                runtime_remote_text(pgrep=""),
                "pgrep tool is unavailable",
            ),
            "miner-scan-error": (
                runtime_remote_text(miners_status="error", miners="error"),
                "Miner process state",
            ),
            "duplicate-runtime-field": (
                runtime_remote_text()
                + "boot_id=12345678-1234-1234-1234-123456789abc\n",
                "fields are not exact",
            ),
        }
        geometry_rows = "\n".join(
            f'mtd{number}: {size} 00020000 "{name}"'
            for number, size, name in GEOMETRY
        )
        for name, (runtime, expected_error) in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory(
                prefix="dcent-am2-executor-"
            ) as temp:
                root = Path(temp)
                plan = fixture_plan()
                now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
                plan["generated_utc"] = now
                plan["pre_flight"]["restore_verified_utc"] = now
                plan["pre_flight"]["sd_recovery_verified_utc"] = now
                plan_path = root / "plan.json"
                plan_path.write_text(json.dumps(plan), encoding="utf-8")
                fake_bin = root / "fake-bin"
                fake_bin.mkdir()
                fake_ssh = fake_bin / "ssh"
                fake_ssh.write_bytes(
                    (
                        "#!/bin/sh\n"
                        "case \"$*\" in\n"
                        "  */sys/class/net/eth0/address*) printf "
                        "'02:00:00:00:00:19\\n' ;;\n"
                        "  */config/CONF_HARDWARE_ID*) printf "
                        "'AM2-FIXTURE-19\\n' ;;\n"
                        "  */config/CONF_MINER_TYPE*) printf "
                        "'Antminer-S19j-Pro\\n' ;;\n"
                        "  */proc/device-tree/compatible*) printf "
                        "'xlnx_zynq-7000\\n' ;;\n"
                        "  */etc/dcentos/board_target*) printf "
                        "'am2-s19jpro-zynq\\n' ;;\n"
                        "  *'cat /proc/mtd'*) cat <<'EOF'\n"
                        "dev:    size   erasesize  name\n"
                        + geometry_rows
                        + "\nEOF\n    ;;\n"
                        "  *'command -v nanddump'*) exit 0 ;;\n"
                        "  *writable_mtd_mounts=*) cat <<'EOF'\n"
                        + runtime
                        + "EOF\n    ;;\n"
                        "  *'nanddump --bb=padbad'*) printf "
                        "'nanddump-was-reached\\n'; exit 92 ;;\n"
                        "  *) exit 91 ;;\n"
                        "esac\n"
                    ).encode("utf-8")
                )
                make_executable(fake_ssh)
                known_hosts = install_fake_host_key_tools(root, fake_bin)
                output = root / "runtime-failure"
                result = run_bash(
                    EXECUTE_SCRIPT,
                    "--target",
                    "192.0.2.19",
                    "--plan",
                    str(plan_path),
                    "--known-hosts",
                    str(known_hosts),
                    "--expected-host-key-sha256",
                    HOST_KEY_SHA256,
                    "--operator-authorized-backup",
                    "--readback-verify",
                    "--skip-size-check",
                    "--local-backup-dir",
                    str(output),
                    environment={
                        "DCENT_NAND_BACKUP_AUTHORIZED": "1",
                        "PATH": fake_path(fake_bin),
                    },
                )
                combined = result.stdout + result.stderr
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, combined)
                self.assertNotIn("nanddump-was-reached", combined)

    def test_executor_rechecks_runtime_between_reads_and_before_publication(self) -> None:
        cases = {
            "boot-after-first-read": ("boot_after_first", 4, "Target rebooted"),
            "writer-after-first-read": (
                "writer_after_first",
                4,
                "Flash/update writer state",
            ),
            "boot-after-readback": ("boot_after_readback", 5, "Target rebooted"),
        }
        geometry_rows = "\n".join(
            f'mtd{number}: {size} 00020000 "{name}"'
            for number, size, name in GEOMETRY
        )
        for name, (mode, expected_gates, expected_error) in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory(
                prefix="dcent-am2-runtime-sequence-"
            ) as temp:
                root = Path(temp)
                plan = fixture_plan()
                now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
                plan["generated_utc"] = now
                plan["pre_flight"]["restore_verified_utc"] = now
                plan["pre_flight"]["sd_recovery_verified_utc"] = now
                plan_path = root / "plan.json"
                plan_path.write_text(json.dumps(plan), encoding="utf-8")
                fake_bin = root / "fake-bin"
                fake_bin.mkdir()
                state = root / "runtime-count"
                state_shell = bash_path(str(state))
                fake_ssh = fake_bin / "ssh"
                fake_ssh.write_bytes(
                    (
                    "#!/bin/sh\n"
                    f"runtime_state='{state_shell}'\n"
                    f"mode='{mode}'\n"
                    "case \"$*\" in\n"
                    "  */sys/class/net/eth0/address*) printf '02:00:00:00:00:19\\n' ;;\n"
                    "  */config/CONF_HARDWARE_ID*) printf 'AM2-FIXTURE-19\\n' ;;\n"
                    "  */config/CONF_MINER_TYPE*) printf 'Antminer-S19j-Pro\\n' ;;\n"
                    "  */proc/device-tree/compatible*) printf 'xlnx_zynq-7000\\n' ;;\n"
                    "  */etc/dcentos/board_target*) printf 'am2-s19jpro-zynq\\n' ;;\n"
                    "  *'cat /proc/mtd'*) cat <<'EOF'\n"
                    "dev:    size   erasesize  name\n"
                    + geometry_rows
                    + "\nEOF\n    ;;\n"
                    "  *'command -v nanddump'*) exit 0 ;;\n"
                    "  *writable_mtd_mounts=*)\n"
                    "    count=0\n"
                    "    [ ! -f \"$runtime_state\" ] || count=$(cat \"$runtime_state\")\n"
                    "    count=$((count + 1))\n"
                    "    printf '%s\\n' \"$count\" > \"$runtime_state\"\n"
                    "    boot_id=12345678-1234-1234-1234-123456789abc\n"
                    "    writers_status=no_matches\n"
                    "    writers=\n"
                    "    if { [ \"$mode\" = boot_after_first ] && [ \"$count\" = 4 ]; } || "
                    "{ [ \"$mode\" = boot_after_readback ] && [ \"$count\" = 5 ]; }; then\n"
                    "      boot_id=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n"
                    "    elif [ \"$mode\" = writer_after_first ] && [ \"$count\" = 4 ]; then\n"
                    "      writers_status=matches\n"
                    "      writers=present\n"
                    "    fi\n"
                    "    printf 'boot_id=%s\\nroot_source=/dev/mmcblk0p2\\nroot_removable=1\\npgrep=/usr/bin/pgrep\\nwritable_mtd_mounts=0\\nminers_status=no_matches\\nminers=\\nwriters_status=%s\\nwriters=%s\\n' \"$boot_id\" \"$writers_status\" \"$writers\"\n"
                    "    ;;\n"
                    "  *'nanddump --bb=padbad'*) dd if=/dev/zero bs=524288 count=1 2>/dev/null ;;\n"
                    "  *) exit 91 ;;\n"
                    "esac\n"
                    ).encode("utf-8")
                )
                make_executable(fake_ssh)
                known_hosts = install_fake_host_key_tools(root, fake_bin)
                output = root / "runtime-sequence-failure"
                result = run_bash(
                    EXECUTE_SCRIPT,
                    "--target",
                    "192.0.2.19",
                    "--plan",
                    str(plan_path),
                    "--known-hosts",
                    str(known_hosts),
                    "--expected-host-key-sha256",
                    HOST_KEY_SHA256,
                    "--operator-authorized-backup",
                    "--readback-verify",
                    "--skip-size-check",
                    "--local-backup-dir",
                    str(output),
                    environment={
                        "DCENT_NAND_BACKUP_AUTHORIZED": "1",
                        "PATH": fake_path(fake_bin),
                    },
                )
                combined = result.stdout + result.stderr
                self.assertNotEqual(result.returncode, 0, combined)
                self.assertIn(expected_error, combined)
                self.assertEqual(int(state.read_text(encoding="utf-8")), expected_gates)
                self.assertFalse((output / "mtd4_uboot_env.nanddump").exists())
                self.assertFalse(list(output.glob("*.manifest.json")))

    def test_result_validator_adversarial_matrix(self) -> None:
        self.assertEqual(run_result_self_test(), 0)

    def test_plan_validator_adversarial_matrix(self) -> None:
        self.assertEqual(run_plan_self_test(), 0)

    def test_manifest_requires_exact_ordered_geometry(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-manifest-") as temp:
            root = Path(temp)
            evidence = root / "evidence.txt"
            manifest = root / "manifest.md"
            evidence.write_text(evidence_text(), encoding="utf-8")
            result = run_bash(
                MANIFEST_SCRIPT,
                "--evidence",
                str(evidence),
                "--output",
                str(manifest),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            rendered = manifest.read_text(encoding="utf-8")
            self.assertIn("layout_profile_candidate=1", rendered)
            self.assertIn("| /dev/mtd4 | 0x00080000 |", rendered)

            wrong = list(GEOMETRY)
            wrong[4] = (4, "00020000", "uboot_env")
            evidence.write_text(evidence_text(tuple(wrong)), encoding="utf-8")
            result = run_bash(
                MANIFEST_SCRIPT,
                "--evidence",
                str(evidence),
                "--output",
                str(manifest),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                "layout_profile_candidate=0",
                manifest.read_text(encoding="utf-8"),
            )

    def make_manifest_and_proof(self, root: Path) -> tuple[Path, Path, Path]:
        evidence = root / "evidence.txt"
        manifest = root / "manifest.md"
        proof = root / "restore-proof.txt"
        sd_proof = root / "sd-recovery-proof.txt"
        evidence.write_text(evidence_text(), encoding="utf-8")
        proof.write_text(proof_text(), encoding="utf-8")
        sd_proof.write_text(sd_recovery_proof_text(), encoding="utf-8")
        result = run_bash(
            MANIFEST_SCRIPT,
            "--evidence",
            str(evidence),
            "--output",
            str(manifest),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return manifest, proof, sd_proof

    def run_plan(
        self,
        root: Path,
        manifest: Path,
        proof: Path,
        sd_proof: Path,
        *,
        readback: bool = True,
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        plan_md = root / "plan.md"
        plan_json = root / "plan.json"
        arguments = [
            "--manifest",
            str(manifest),
            "--restore-artifact-proof",
            str(proof),
            "--sd-recovery-proof",
            str(sd_proof),
            "--expect-ip",
            "192.0.2.19",
            "--expect-host-key-sha256",
            HOST_KEY_SHA256,
            "--expect-mac",
            "02:00:00:00:00:19",
            "--expect-hwid",
            "AM2-FIXTURE-19",
            "--expect-model",
            "Antminer-S19j-Pro",
            "--expect-target",
            "am2-s19jpro-zynq",
            "--output",
            str(plan_md),
            "--json-template",
            str(plan_json),
        ]
        if readback:
            arguments.append("--readback-verify")
        return run_bash(PLAN_SCRIPT, *arguments), plan_json

    def test_ready_plan_is_strict_and_exact(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertEqual(result.returncode, 0, result.stderr)
            plan = json.loads(plan_json.read_text(encoding="utf-8"))
            self.assertEqual(plan["plan_ready"], 1)
            self.assertEqual(plan["target"]["class"], "am2 Zynq 7007S")
            self.assertEqual(plan["partitions"][4]["size_bytes"], 512 * 1024)
            self.assertEqual(
                plan["pre_flight"]["sd_recovery_root_device"],
                "/dev/mmcblk0p2",
            )
            self.assertEqual(
                plan["pre_flight"]["sd_recovery_proof_sha256"],
                hashlib.sha256(sd_proof.read_bytes()).hexdigest(),
            )
            self.assertEqual(
                plan["pre_flight"]["sd_recovery_compatible"],
                "xlnx_zynq-7000",
            )
            self.assertEqual(
                plan["pre_flight"]["sd_recovery_boot_id"],
                "12345678-1234-1234-1234-123456789abc",
            )
            validate_plan(plan)

    def test_readback_is_required_for_ready_plan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            result, plan_json = self.run_plan(
                root, manifest, proof, sd_proof, readback=False
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(plan_json.exists())
            self.assertIn(
                "plan_ready=0", (root / "plan.md").read_text(encoding="utf-8")
            )

    def test_plan_rejects_same_markdown_and_json_path(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            collision = root / "plan-output"
            cases = (
                (collision, collision),
                (root / "alias" / ".." / collision.name, collision),
                (root / "PLAN-OUTPUT", collision),
            )
            for markdown, json_output in cases:
                with self.subTest(markdown=markdown, json_output=json_output):
                    result = run_bash(
                        PLAN_SCRIPT,
                        "--manifest",
                        str(manifest),
                        "--restore-artifact-proof",
                        str(proof),
                        "--sd-recovery-proof",
                        str(sd_proof),
                        "--expect-ip",
                        "192.0.2.19",
                        "--expect-host-key-sha256",
                        HOST_KEY_SHA256,
                        "--expect-mac",
                        "02:00:00:00:00:19",
                        "--expect-hwid",
                        "AM2-FIXTURE-19",
                        "--expect-model",
                        "Antminer-S19j-Pro",
                        "--expect-target",
                        "am2-s19jpro-zynq",
                        "--output",
                        str(markdown),
                        "--json-template",
                        str(json_output),
                        "--readback-verify",
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("must be different paths", result.stderr)
                    self.assertFalse(collision.exists())

    def test_one_row_manifest_cannot_become_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest = root / "manifest.md"
            proof = root / "restore-proof.txt"
            sd_proof = root / "sd-recovery-proof.txt"
            manifest.write_text(
                "layout_profile_candidate=1\n"
                "| Node | Size Hex | Erase Hex | Name | Artifact |\n"
                "| /dev/mtd7 | 0x03900000 | 0x00020000 | firmware1 | "
                "mtd7_firmware1.nanddump |\n",
                encoding="utf-8",
            )
            proof.write_text(proof_text(), encoding="utf-8")
            sd_proof.write_text(sd_recovery_proof_text(), encoding="utf-8")
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(plan_json.exists())
            self.assertIn(
                "plan_ready=0", (root / "plan.md").read_text(encoding="utf-8")
            )

    def test_duplicate_restore_field_cannot_become_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            proof.write_text(
                proof.read_text(encoding="utf-8")
                + "restore_mac=02:00:00:00:00:19\n",
                encoding="utf-8",
            )
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(plan_json.exists())
            self.assertIn(
                "plan_ready=0", (root / "plan.md").read_text(encoding="utf-8")
            )

    def test_contradictory_sd_decision_cannot_become_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            sd_proof.write_text(
                sd_proof.read_text(encoding="utf-8")
                + "identity=fail not_proven_am2_zynq\n",
                encoding="utf-8",
            )
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(plan_json.exists())
            self.assertIn(
                "plan_ready=0", (root / "plan.md").read_text(encoding="utf-8")
            )

    def test_operator_selected_compatible_cannot_become_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            sd_proof.write_text(
                sd_proof.read_text(encoding="utf-8").replace(
                    "identity_compatible=xlnx_zynq-7000",
                    "identity_compatible=not_zynq",
                ),
                encoding="utf-8",
            )
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(plan_json.exists())
            self.assertIn(
                "plan_ready=0", (root / "plan.md").read_text(encoding="utf-8")
            )

    def test_stale_restore_proof_is_not_published(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            proof.write_text(
                proof_text(
                    verified_utc=datetime.now(timezone.utc) - timedelta(days=31)
                ),
                encoding="utf-8",
            )
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(plan_json.exists())
            self.assertFalse((root / "plan.md").exists())

    def test_stale_sd_recovery_proof_is_not_published(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am2-plan-") as temp:
            root = Path(temp)
            manifest, proof, sd_proof = self.make_manifest_and_proof(root)
            sd_proof.write_text(
                sd_recovery_proof_text(
                    verified_utc=datetime.now(timezone.utc) - timedelta(days=2)
                ),
                encoding="utf-8",
            )
            result, plan_json = self.run_plan(root, manifest, proof, sd_proof)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(plan_json.exists())
            self.assertFalse((root / "plan.md").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
