#!/usr/bin/env python3
"""Adversarial coverage for the AM1 backup and recovery evidence boundary."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

import am1_nand_backup_execute as execute
import am1_nand_backup_manifest as manifest_tool
import am1_nand_backup_plan as plan
import atomic_publish_file as atomic_file
import durable_file_io as backup_io
import am1_nand_sd_recovery_probe as probe
from am1_nand_backup_manifest import generate
from am1_nand_backup_plan import RESTORE_KEYS, SD_KEYS, build
from validate_am1_nand_backup import ValidationError, fixture_manifest, validate_backup
from validate_am1_nand_backup_plan import fixture_plan, validate_plan


HOST_KEY = "SHA256:" + "A" * 43
MODEL = "Xilinx_Zynq_AM1_S9"
COMPATIBLE = "xlnx_zynq-7000"
BOOT_ID = "12345678-1234-1234-1234-123456789abc"
STOCK = (
    (0, "02000000", "boot"),
    (1, "09000000", "rootfs"),
    (2, "05000000", "upgrade"),
)


def evidence_text(rows: tuple[tuple[int, str, str], ...] = STOCK) -> str:
    body = "\n".join(f'mtd{number}: {size} 00020000 "{name}"' for number, size, name in rows)
    return f"=== mtd layout ===\n{body}\n=== next ===\n"


def manifest_text() -> str:
    rows = "\n".join(
        f"| /dev/mtd{number} | 0x{size} | 0x00020000 | {name} | mtd{number}_{name}.nanddump |"
        for number, size, name in STOCK
    )
    return (
        "# AM1 MTD Backup Manifest\n\n"
        "- `layout_profile_candidate=1`\n"
        "- `partition_scheme=stock`\n"
        "- `partition_count=3`\n"
        "- `backup_scope=data-only-no-oob`\n"
        "- `restore_authority=none-until-physical-rehearsal`\n"
        "- `nand_backup_execute_go=0`\n"
        "- `nand_write_go=0`\n"
        "- `persistent_install_go=0`\n\n"
        "| Node | Size Hex | Erase Hex | Name | Required Artifact |\n"
        "| --- | --- | --- | --- | --- |\n"
        f"{rows}\n"
    )


def proof_text(keys: set[str], values: dict[str, str]) -> str:
    if set(values) != keys:
        raise AssertionError("fixture proof fields are not exact")
    return "".join(f"{key}={values[key]}\n" for key in sorted(keys))


def preflight_remote(**overrides: str) -> str:
    values = {
        "mac": "02:00:00:00:00:09",
        "hwid": "AM1-FIXTURE-9",
        "model": MODEL,
        "compatible": COMPATIBLE,
        "board_target": "am1-s9",
        "boot_id": BOOT_ID,
        "root_source": "/dev/mmcblk0p2",
        "root_removable": "1",
        "nanddump": "/usr/sbin/nanddump",
        "pgrep": "/usr/bin/pgrep",
        "writable_mtd_mounts": "0",
        "miners_status": "no_matches",
        "miners": "",
        "writers_status": "no_matches",
        "writers": "",
    }
    values.update(overrides)
    identity = "".join(
        f"{key}={values[key]}\n"
        for key in (
            "mac", "hwid", "model", "compatible", "board_target", "boot_id",
            "root_source", "root_removable",
        )
    )
    rows = "\n".join(f'mtd{number}: {size} 00020000 "{name}"' for number, size, name in STOCK)
    safety = "".join(
        f"{key}={values[key]}\n"
        for key in (
            "nanddump", "pgrep", "writable_mtd_mounts", "miners_status", "miners",
            "writers_status", "writers",
        )
    )
    return f"{identity}mtd_begin\ndev: size erasesize name\n{rows}\nmtd_end\n{safety}"


def runtime_remote(**overrides: str) -> str:
    values = {
        "boot_id": BOOT_ID,
        "root_source": "/dev/mmcblk0p2",
        "root_removable": "1",
        "pgrep": "/usr/bin/pgrep",
        "writable_mtd_mounts": "0",
        "miners_status": "no_matches",
        "miners": "",
        "writers_status": "no_matches",
        "writers": "",
    }
    values.update(overrides)
    return "".join(f"{key}={value}\n" for key, value in values.items())


class Am1EvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.now = datetime.now(timezone.utc).replace(microsecond=0)

    def make_plan_inputs(self, root: Path) -> argparse.Namespace:
        stamp = self.now.strftime("%Y-%m-%dT%H:%M:%SZ")
        manifest = root / "manifest.md"
        sd = root / "sd.txt"
        restore = root / "restore.txt"
        restore_artifact = root / "known-good-restore.bin"
        restore_artifact.write_bytes(b"known-good-am1-restore-fixture")
        restore_sha = hashlib.sha256(restore_artifact.read_bytes()).hexdigest()
        manifest.write_text(manifest_text(), encoding="utf-8")
        sd.write_text(
            proof_text(
                SD_KEYS,
                {
                    "schema": "am1_sd_recovery_proof_v1",
                    "timestamp_utc": stamp,
                    "ip": "192.0.2.9",
                    "ssh_host_key_authentication": "verified",
                    "ssh_host_key_sha256": HOST_KEY,
                    "identity_mac": "02:00:00:00:00:09",
                    "identity_hwid": "AM1-FIXTURE-9",
                    "identity_model": MODEL,
                    "identity_compatible": COMPATIBLE,
                    "identity_target": "am1-s9",
                    "root_source": "/dev/mmcblk0p2",
                    "root_removable": "1",
                    "identity": "pass am1_zynq_s9",
                    "external_boot": "pass root_device_exact_removable_mmc",
                    "mtd_layout": "stock",
                    "mtd_geometry": "pass exact_am1_stock_partition",
                    "nand_backup_execute_go": "0",
                    "nand_write_go": "0",
                    "persistent_install_go": "0",
                    "sd_recovery_probe": "pass",
                },
            ),
            encoding="utf-8",
        )
        restore.write_text(
            proof_text(
                RESTORE_KEYS,
                {
                    "schema": "am1_restore_artifact_proof_v1",
                    "restore_verified": "1",
                    "restore_mac": "02:00:00:00:00:09",
                    "restore_hwid": "AM1-FIXTURE-9",
                    "restore_model": MODEL,
                    "restore_compatible": COMPATIBLE,
                    "restore_target": "am1-s9",
                    "restore_artifact_name": restore_artifact.name,
                    "restore_artifact_sha256": restore_sha,
                    "restore_verified_utc": stamp,
                },
            ),
            encoding="utf-8",
        )
        return argparse.Namespace(
            manifest=manifest,
            restore_proof=restore,
            restore_artifact=restore_artifact,
            sd_recovery_proof=sd,
            expect_layout="stock",
            expect_ip="192.0.2.9",
            expect_host_key_sha256=HOST_KEY,
            expect_mac="02:00:00:00:00:09",
            expect_hwid="AM1-FIXTURE-9",
            expect_model=MODEL,
            expect_compatible=COMPATIBLE,
            expect_target="am1-s9",
            readback_verify=True,
            output=root / "plan.md",
            json_template=root / "plan.json",
        )

    def executor(self, root: Path, rows: list[str] | None = None) -> execute.Executor:
        args = argparse.Namespace(
            local_backup_dir=root / "backup",
            known_hosts=root / "known_hosts",
            ssh_user="root",
            target="192.0.2.9",
            expected_host_key_sha256=HOST_KEY,
            ssh_password_env="UNSET_TEST_PASSWORD",
            timeout=1,
            skip_size_check=True,
        )
        return execute.Executor(
            args,
            {
                "mac": "02:00:00:00:00:09",
                "hwid": "AM1-FIXTURE-9",
                "model": MODEL,
                "compatible": COMPATIBLE,
                "board_target": "am1-s9",
                "root_device": "/dev/mmcblk0p2",
            },
            rows or [],
            "stock",
            1,
        )

    def test_durable_mkdir_persists_each_new_ancestor(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-io-") as temp:
            root = Path(temp)
            target = root / "new-parent" / "new-child"
            with mock.patch.object(backup_io, "fsync_directory") as fsync:
                backup_io.mkdir_durable(target, mode=0o700)
            self.assertTrue(target.is_dir())
            synced = [call.args[0] for call in fsync.call_args_list]
            self.assertIn(root, synced)
            self.assertIn(target.parent, synced)
            self.assertIn(target, synced)

    def test_plan_publishes_executable_json_last(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-order-") as temp:
            root = Path(temp)
            args = argparse.Namespace(
                output=root / "plan.md",
                json_template=root / "plan.json",
            )
            publications: list[Path] = []

            def record_publication(path: Path, _payload: bytes) -> None:
                publications.append(path)

            with (
                mock.patch.object(plan, "parse_args", return_value=args),
                mock.patch.object(plan, "build", return_value=(b"review\n", b"{}\n")),
                mock.patch.object(
                    plan,
                    "atomic_publish",
                    side_effect=record_publication,
                ),
            ):
                self.assertEqual(plan.main([]), 0)
            self.assertEqual(publications, [args.output, args.json_template])

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_plan_signal_after_review_commit_leaves_only_safe_review(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-signal-") as temp:
            root = Path(temp)
            output = root / "plan.md"
            json_template = root / "plan.json"
            code = f"""
import argparse
import importlib.util
import os
from pathlib import Path
import signal
import sys

script = Path({str(Path(plan.__file__))!r})
sys.path.insert(0, str(script.parent))
spec = importlib.util.spec_from_file_location("am1_plan_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.parse_args = lambda _argv=None: argparse.Namespace(
    output=Path({str(output)!r}),
    json_template=Path({str(json_template)!r}),
)
module.build = lambda _args, _now: (b"review\\n", b"{{}}\\n")
real_publish = module.atomic_publish
publish_calls = 0

def signal_after_review(path, payload):
    global publish_calls
    real_publish(path, payload)
    publish_calls += 1
    if publish_calls == 1:
        os.kill(os.getpid(), signal.SIGTERM)

module.atomic_publish = signal_after_review
raise SystemExit(module.main([]))
"""
            result = subprocess.run(
                [sys.executable, "-c", code],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "before durable AM1 NAND backup plan publication", result.stderr
            )
            self.assertEqual(output.read_bytes(), b"review\n")
            self.assertFalse(json_template.exists())
            self.assertEqual(list(root.glob("*.publication-pending.*")), [])

    def test_plan_rejects_output_path_aliases_before_build(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-alias-") as temp:
            root = Path(temp)
            aliases = (
                (root / "plan.md", root / "plan.md"),
                (root / "nested" / ".." / "plan.md", root / "plan.md"),
                (root / "PLAN.MD", root / "plan.md"),
            )
            for output, json_template in aliases:
                with self.subTest(output=output, json_template=json_template):
                    args = argparse.Namespace(
                        output=output,
                        json_template=json_template,
                    )
                    with (
                        mock.patch.object(plan, "parse_args", return_value=args),
                        mock.patch.object(plan, "build") as build_mock,
                    ):
                        self.assertEqual(plan.main([]), 1)
                    build_mock.assert_not_called()

    def test_plan_atomic_publish_is_no_clobber(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-atomic-") as temp:
            destination = Path(temp) / "plan.json"
            plan.atomic_publish(destination, b"first\n")
            with self.assertRaisesRegex(ValidationError, "refusing to replace"):
                plan.atomic_publish(destination, b"second\n")
            self.assertEqual(destination.read_bytes(), b"first\n")

    def test_plan_prelink_failure_quarantines_authoritative_staging(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-failed-") as temp:
            root = Path(temp)
            destination = root / "plan.json"
            with mock.patch.object(
                plan,
                "publish_staged_file",
                side_effect=plan.PublishError("injected pre-link failure"),
            ):
                with self.assertRaisesRegex(ValidationError, "retained as"):
                    plan.atomic_publish(destination, b'{"plan_ready":1}\n')
            self.assertFalse(destination.exists())
            quarantine_directories = list(
                root.glob(f".{destination.name}.publication-failed.*")
            )
            self.assertEqual(len(quarantine_directories), 1)
            self.assertEqual(
                (quarantine_directories[0] / "staged").read_bytes(),
                b'{"plan_ready":1}\n',
            )

    def test_retained_staging_warning_failure_cannot_revoke_commit(self) -> None:
        for name, reporting_error in (
            ("broken-pipe", BrokenPipeError()),
            ("closed-stream", ValueError("I/O operation on closed file")),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory(
                prefix="dcent-am1-plan-warning-"
            ) as temp:
                destination = Path(temp) / "plan.json"

                def publish_with_retained_name(
                    staging: Path,
                    output: Path,
                    *,
                    require_directory_sync: bool = False,
                ) -> tuple[str, str]:
                    self.assertTrue(require_directory_sync)
                    output.write_bytes(staging.read_bytes())
                    return "pass", "retained_errno_13"

                with (
                    mock.patch.object(
                        plan,
                        "publish_staged_file",
                        side_effect=publish_with_retained_name,
                    ),
                    mock.patch("builtins.print", side_effect=reporting_error),
                ):
                    plan.atomic_publish(destination, b'{"plan_ready":1}\n')
                self.assertEqual(destination.read_bytes(), b'{"plan_ready":1}\n')

    def test_sd_receipt_reporting_failure_cannot_revoke_commit(self) -> None:
        destination = Path("committed-am1-proof.txt")
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
                with mock.patch.object(probe.sys, "stdout", output):
                    probe.report_committed_receipt(destination)
                if name == "broken-pipe":
                    output.flush.assert_called_once_with()

    def test_am1_producers_use_shared_strict_publication(self) -> None:
        for module in (plan, manifest_tool, execute, probe):
            with self.subTest(module=module.__name__):
                source = Path(module.__file__).read_text(encoding="utf-8")
                self.assertIn("require_directory_sync=True", source)
                self.assertIn(".publication-pending.", source)
                self.assertNotIn("os.link(", source)
                if module is manifest_tool:
                    self.assertIn("CommitSignalGuard(", source)
                    self.assertIn("_after_staged_open=before_commit", source)

    def test_executor_log_flushes_each_evidence_write(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-log-") as temp:
            runner = self.executor(Path(temp))
            runner.output.mkdir()
            runner.log_path.touch()
            with mock.patch.object(execute.os, "fsync") as sync:
                runner.log("durable evidence")
            sync.assert_called_once()

    def test_plan_builder_binds_fresh_exact_receipts(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            _, encoded = build(args, self.now)
            plan = json.loads(encoded)
            self.assertEqual(plan["pre_flight"]["sd_recovery_root_removable"], 1)
            self.assertEqual(len(plan["partitions"]), 3)
            validate_plan(plan, now=self.now)

    def test_stale_duplicate_or_contradictory_proof_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            stale = (self.now - timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%SZ")
            args.sd_recovery_proof.write_text(
                args.sd_recovery_proof.read_text(encoding="utf-8").replace(
                    self.now.strftime("%Y-%m-%dT%H:%M:%SZ"), stale
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValidationError, "at least 86400"):
                build(args, self.now)
            args = self.make_plan_inputs(Path(temp))
            with args.restore_proof.open("a", encoding="utf-8") as handle:
                handle.write("restore_verified=0\n")
            with self.assertRaisesRegex(ValidationError, "duplicate field"):
                build(args, self.now)

    def test_restore_proof_hash_must_match_physical_artifact(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            args.restore_artifact.write_bytes(b"different restore bytes")
            with self.assertRaisesRegex(ValidationError, "supplied artifact bytes"):
                build(args, self.now)

    def test_manifest_requires_one_exact_geometry(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-manifest-") as temp:
            root = Path(temp)
            evidence = root / "evidence.txt"
            evidence.write_text(evidence_text(), encoding="utf-8")
            args = argparse.Namespace(evidence=evidence, output=root / "manifest.md")
            output, layout = generate(args)
            self.assertEqual(layout, "stock")
            self.assertIn("backup_scope=data-only-no-oob", output.read_text(encoding="utf-8"))
            evidence.write_text(evidence_text() + evidence_text(), encoding="utf-8")
            args.output = root / "duplicate.md"
            with self.assertRaisesRegex(ValidationError, "exactly one"):
                generate(args)

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_planning_manifest_signals_preserve_commit_truth(self) -> None:
        for phase in ("before", "after"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory(
                prefix=f"dcent-am1-manifest-{phase}-"
            ) as temp:
                root = Path(temp)
                evidence = root / "evidence.txt"
                output = root / "manifest.md"
                evidence.write_text(evidence_text(), encoding="utf-8")
                code = f"""
import importlib.util
import os
from pathlib import Path
import signal
import sys

script = Path({str(Path(manifest_tool.__file__))!r})
sys.path.insert(0, str(script.parent))
spec = importlib.util.spec_from_file_location("am1_manifest_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
real_publish = module.publish_staged_file

def inject_signal(*args, **kwargs):
    if {phase!r} == "before":
        refuse_pending = kwargs["_after_staged_open"]
        def before_commit():
            os.kill(os.getpid(), signal.SIGTERM)
            refuse_pending()
        kwargs["_after_staged_open"] = before_commit
        return real_publish(*args, **kwargs)
    result = real_publish(*args, **kwargs)
    os.kill(os.getpid(), signal.SIGTERM)
    return result

module.publish_staged_file = inject_signal
raise SystemExit(module.main([
    "--evidence",
    {str(evidence)!r},
    "--output",
    {str(output)!r},
]))
"""
                result = subprocess.run(
                    [sys.executable, "-c", code],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.assertEqual(
                    list(root.glob(".*.publication-pending.*")), []
                )
                if phase == "before":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("before durable", result.stderr)
                    self.assertFalse(output.exists())
                    self.assertEqual(
                        len(list(root.glob(".*.publication-failed.*"))), 1
                    )
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("ignored signal", result.stderr)
                    self.assertTrue(output.is_file())
                    self.assertIn("layout_profile_candidate=1", result.stdout)

    def test_result_validator_requires_nonrestorable_scope(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-result-") as temp:
            root = Path(temp)
            path, manifest = fixture_manifest(root, layout_contracts={"stock": ((0, "boot", 64), (1, "rootfs", 96), (2, "upgrade", 80))})
            validate_backup(path, root, layout_contracts={"stock": ((0, "boot", 64), (1, "rootfs", 96), (2, "upgrade", 80))})
            manifest["partitions"][0]["readback_sha256"] = "0" * 64
            path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValidationError, "readback_sha256 must equal"):
                validate_backup(path, root, layout_contracts={"stock": ((0, "boot", 64), (1, "rootfs", 96), (2, "upgrade", 80))})
            manifest["partitions"][0]["readback_sha256"] = manifest["partitions"][0]["sha256"]
            manifest["target"]["restore_authority"] = "approved"
            path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValidationError, "does not match expected"):
                validate_backup(path, root, layout_contracts={"stock": ((0, "boot", 64), (1, "rootfs", 96), (2, "upgrade", 80))})

    def test_preflight_rejects_identity_geometry_and_quiescence_drift(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-preflight-") as temp:
            for mutation, error in (
                ({"model": "wrong"}, "live model"),
                ({"root_removable": "0"}, "removable"),
                ({"writers_status": "matches", "writers": "present"}, "writer status"),
                ({"writable_mtd_mounts": "1"}, "writable MTD/UBI"),
            ):
                with self.subTest(mutation=mutation):
                    runner = self.executor(Path(temp))
                    runner.ssh_text = mock.Mock(return_value=preflight_remote(**mutation))  # type: ignore[method-assign]
                    with self.assertRaisesRegex(ValidationError, error):
                        runner.preflight()
            runner = self.executor(Path(temp))
            runner.ssh_text = mock.Mock(return_value=preflight_remote())  # type: ignore[method-assign]
            self.assertEqual(runner.preflight()[-1], BOOT_ID)

    def test_runtime_gate_rejects_boot_root_process_and_mount_drift(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-runtime-") as temp:
            runner = self.executor(Path(temp))
            runner.ssh_text = mock.Mock(return_value=runtime_remote())  # type: ignore[method-assign]
            runner.runtime_gate(BOOT_ID)
            for mutation, error in (
                ({"boot_id": "87654321-1234-1234-1234-123456789abc"}, "rebooted"),
                ({"root_source": "/dev/mmcblk1p2"}, "root admission"),
                ({"miners_status": "error", "miners": "error"}, "miner state"),
                ({"writers_status": "matches", "writers": "present"}, "writer state"),
                ({"writable_mtd_mounts": "1"}, "writable MTD/UBI"),
            ):
                with self.subTest(mutation=mutation):
                    runner.ssh_text = mock.Mock(return_value=runtime_remote(**mutation))  # type: ignore[method-assign]
                    with self.assertRaisesRegex(ValidationError, error):
                        runner.runtime_gate(BOOT_ID)

    def test_data_path_requires_two_host_observed_exact_reads(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-exec-") as temp:
            root = Path(temp)
            rows = ["0|boot|64|mtd0_boot.nanddump", "1|rootfs|32|mtd1_rootfs.nanddump"]
            runner = self.executor(root, rows)
            runner.preflight = mock.Mock(return_value=("02:00:00:00:00:09", "AM1-FIXTURE-9", MODEL, COMPATIBLE, "am1-s9", BOOT_ID))  # type: ignore[method-assign]
            runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
            real_log = runner.log

            def log_with_post_commit_fault(message: str) -> None:
                if message.startswith("nand_backup_complete=pass"):
                    raise OSError("injected final informational log failure")
                real_log(message)

            runner.log = log_with_post_commit_fault  # type: ignore[method-assign]
            reads: list[str] = []

            def stream(command: str, destination: object) -> None:
                reads.append(command)
                size = 64 if "/dev/mtd0" in command else 32
                destination.write(bytes([size]) * size)  # type: ignore[attr-defined]

            runner.ssh_stream = stream  # type: ignore[method-assign]
            small = {"stock": ((0, "boot", 64), (1, "rootfs", 32))}
            real_validate = execute.validate_backup

            def validate_small(*args: object, **kwargs: object) -> object:
                kwargs["layout_contracts"] = small
                return real_validate(*args, **kwargs)

            with mock.patch.object(execute, "EXPECTED_LAYOUTS", small), mock.patch.object(
                execute, "validate_backup", side_effect=validate_small
            ):
                manifest = runner.run()
            self.assertTrue(manifest.is_file())
            self.assertEqual(reads.count("nanddump --bb=padbad --omitoob /dev/mtd0"), 2)
            self.assertEqual(reads.count("nanddump --bb=padbad --omitoob /dev/mtd1"), 2)
            self.assertEqual(runner.runtime_gate.call_count, 5)

    def test_executor_post_link_sync_fault_quarantines_destination(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-exec-sync-") as temp:
            root = Path(temp)
            destination = root / "result.manifest.json"
            staged = root / ".result.manifest.json.publication-pending.fixture"
            staged.write_bytes(b'{"nand_backup_complete":"pass"}\n')
            sync_calls = 0

            def publish_with_sync_fault(
                source: Path,
                output: Path,
                *,
                require_directory_sync: bool = False,
            ) -> tuple[str, str]:
                nonlocal sync_calls

                def sync_directory(_path: Path) -> str:
                    nonlocal sync_calls
                    sync_calls += 1
                    if sync_calls == 2:
                        raise OSError(5, "injected post-link directory sync failure")
                    return "pass"

                return atomic_file.atomic_publish(
                    source,
                    output,
                    require_directory_sync=require_directory_sync,
                    _sync_directory=sync_directory,
                )

            with mock.patch.object(
                execute,
                "publish_staged_file",
                side_effect=publish_with_sync_fault,
            ):
                with self.assertRaisesRegex(ValidationError, "official destination"):
                    execute.Executor.publish(staged, destination)
            self.assertEqual(sync_calls, 3)
            self.assertFalse(destination.exists())
            quarantines = list(
                root.glob(f".{destination.name}.publication-failed.*")
            )
            self.assertEqual(len(quarantines), 1)
            self.assertEqual(
                quarantines[0].read_bytes(),
                b'{"nand_backup_complete":"pass"}\n',
            )

    def test_executor_manifest_publish_failure_leaves_no_pass_manifest(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-exec-commit-") as temp:
            root = Path(temp)
            runner = self.executor(
                root,
                ["0|boot|64|mtd0_boot.nanddump"],
            )
            runner.preflight = mock.Mock(
                return_value=(
                    "02:00:00:00:00:09",
                    "AM1-FIXTURE-9",
                    MODEL,
                    COMPATIBLE,
                    "am1-s9",
                    BOOT_ID,
                )
            )  # type: ignore[method-assign]
            runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
            runner.ssh_stream = lambda _command, destination: destination.write(
                b"x" * 64
            )  # type: ignore[method-assign]
            small = {"stock": ((0, "boot", 64),)}
            real_publish = execute.publish_staged_file

            def reject_manifest(
                staging: Path,
                destination: Path,
                *,
                require_directory_sync: bool = False,
                _after_staged_open: object | None = None,
            ) -> tuple[str, str]:
                self.assertTrue(require_directory_sync)
                if destination.name == "am1_nand_backup.manifest.json":
                    self.assertIn(".publication-pending.", staging.name)
                    self.assertTrue(callable(_after_staged_open))
                    _after_staged_open()
                    raise execute.PublishError("injected result commit failure")
                return real_publish(
                    staging,
                    destination,
                    require_directory_sync=require_directory_sync,
                )

            with (
                mock.patch.object(execute, "EXPECTED_LAYOUTS", small),
                mock.patch.object(execute, "validate_backup"),
                mock.patch.object(
                    execute,
                    "publish_staged_file",
                    side_effect=reject_manifest,
                ),
            ):
                with self.assertRaisesRegex(ValidationError, "commit failure"):
                    runner.run()
            destination = runner.output / "am1_nand_backup.manifest.json"
            self.assertFalse(destination.exists())
            quarantine_directories = list(
                runner.output.glob(
                    ".am1_nand_backup.manifest.json.publication-failed.*"
                )
            )
            self.assertEqual(len(quarantine_directories), 1)
            quarantined = json.loads(
                (quarantine_directories[0] / "staged").read_text(encoding="utf-8")
            )
            self.assertEqual(quarantined["nand_backup_complete"], "pass")

    def test_data_path_rejects_short_or_divergent_streams(self) -> None:
        for mode, error in (("short", "size mismatch"), ("divergent", "readback mismatch")):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="dcent-am1-exec-") as temp:
                root = Path(temp)
                runner = self.executor(root, ["0|boot|64|mtd0_boot.nanddump"])
                runner.preflight = mock.Mock(return_value=("02:00:00:00:00:09", "AM1-FIXTURE-9", MODEL, COMPATIBLE, "am1-s9", BOOT_ID))  # type: ignore[method-assign]
                runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
                calls = 0

                def stream(_command: str, destination: object) -> None:
                    nonlocal calls
                    calls += 1
                    destination.write(b"short" if mode == "short" else bytes([calls]) * 64)  # type: ignore[attr-defined]

                runner.ssh_stream = stream  # type: ignore[method-assign]
                with mock.patch.object(execute, "EXPECTED_LAYOUTS", {"stock": ((0, "boot", 64),)}):
                    with self.assertRaisesRegex(ValidationError, error):
                        runner.run()
                self.assertFalse((runner.output / "am1_nand_backup.manifest.json").exists())

    def test_probe_publishes_only_exact_pinned_external_root(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-probe-") as temp:
            root = Path(temp)
            known_hosts = root / "known_hosts"
            known_hosts.write_text("fixture\n", encoding="utf-8")
            args = [
                "192.0.2.9", "--artifact-dir", str(root / "private"), "--known-hosts", str(known_hosts),
                "--expected-host-key-sha256", HOST_KEY, "--expect-layout", "stock",
                "--expect-mac", "02:00:00:00:00:09", "--expect-hwid", "AM1-FIXTURE-9",
                "--expect-model", MODEL, "--expect-compatible", COMPATIBLE,
            ]
            remote = preflight_remote().split("nanddump=", 1)[0]
            remote = remote.replace(f"boot_id={BOOT_ID}\n", "")
            with mock.patch.object(probe, "pinned_fingerprint", return_value=HOST_KEY), mock.patch.object(probe, "ssh_probe", return_value=remote), mock.patch.object(probe.shutil, "which", return_value="tool"):
                self.assertEqual(probe.main(args), 0)
            receipts = list((root / "private").glob("*_sd_recovery_proof.txt"))
            self.assertEqual(len(receipts), 1)
            self.assertIn("root_removable=1", receipts[0].read_text(encoding="utf-8"))
            rejected = remote.replace("root_removable=1", "root_removable=0")
            args[2] = str(root / "rejected")
            with mock.patch.object(probe, "pinned_fingerprint", return_value=HOST_KEY), mock.patch.object(probe, "ssh_probe", return_value=rejected), mock.patch.object(probe.shutil, "which", return_value="tool"):
                self.assertEqual(probe.main(args), 1)
            self.assertFalse((root / "rejected").exists())

    def test_plan_and_probe_reject_operator_selected_board_class(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-class-") as temp:
            root = Path(temp)
            args = self.make_plan_inputs(root)
            args.expect_target = "operator-selected"
            with self.assertRaisesRegex(ValidationError, "canonical AM1 target"):
                build(args, self.now)

            known_hosts = root / "known_hosts"
            known_hosts.write_text("fixture\n", encoding="utf-8")
            rc = probe.main(
                [
                    "192.0.2.9",
                    "--artifact-dir",
                    str(root / "private"),
                    "--known-hosts",
                    str(known_hosts),
                    "--expected-host-key-sha256",
                    HOST_KEY,
                    "--expect-layout",
                    "stock",
                    "--expect-mac",
                    "02:00:00:00:00:09",
                    "--expect-hwid",
                    "AM1-FIXTURE-9",
                    "--expect-model",
                    MODEL,
                    "--expect-compatible",
                    COMPATIBLE,
                    "--expect-target",
                    "operator-selected",
                ]
            )
            self.assertEqual(rc, 1)

    def test_executor_rejects_stale_plan_before_output(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am1-main-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            stale = (self.now - timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = stale
            plan["pre_flight"]["restore_verified_utc"] = stale
            plan["pre_flight"]["sd_recovery_verified_utc"] = stale
            plan_path = root / "plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")
            known_hosts = root / "known_hosts"
            known_hosts.write_text("fixture\n", encoding="utf-8")
            output = root / "must-not-exist"
            with mock.patch.dict(os.environ, {"DCENT_NAND_BACKUP_AUTHORIZED": "1"}), mock.patch.object(execute.shutil, "which", return_value="tool"):
                result = execute.main([
                    "--target", "192.0.2.9", "--plan", str(plan_path), "--local-backup-dir", str(output),
                    "--known-hosts", str(known_hosts), "--expected-host-key-sha256", HOST_KEY,
                    "--operator-authorized-backup", "--readback-verify",
                ])
            self.assertEqual(result, 1)
            self.assertFalse(output.exists())

    def test_no_unauthenticated_or_target_staged_transport_remains(self) -> None:
        sources = "\n".join(
            path.read_text(encoding="utf-8")
            for path in (
                Path(execute.__file__),
                Path(probe.__file__),
                Path(__file__).with_name("am1_nand_backup_manifest.py"),
            )
        )
        self.assertNotIn("StrictHostKeyChecking=no", sources)
        self.assertNotIn("UserKnownHostsFile=/dev/null", sources)
        self.assertNotIn("--bb=skipbad", sources)
        self.assertNotIn("REMOTE_OUTDIR", sources)
        executor_source = Path(execute.__file__).read_text(encoding="utf-8")
        for writer_pattern in ("[f]lashcp", "[m]td_debug", "[u]biformat", "[d]d"):
            self.assertIn(writer_pattern, executor_source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
