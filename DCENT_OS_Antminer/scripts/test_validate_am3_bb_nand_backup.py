#!/usr/bin/env python3
"""Adversarial coverage for the AM3-BB backup and recovery evidence boundary."""

from __future__ import annotations

import argparse
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

import am3_bb_nand_backup_execute as execute
import am3_bb_nand_backup_plan as plan
import atomic_publish_file as atomic_file
import durable_file_io as backup_io
from am3_bb_nand_backup_plan import SD_KEYS, RESTORE_KEYS, build
from validate_am1_nand_backup import ValidationError
from validate_am3_bb_nand_backup import run_self_test as run_result_self_test
from validate_am3_bb_nand_backup_plan import (
    fixture_plan,
    run_self_test as run_plan_self_test,
    validate_plan,
)


SCRIPT_DIR = Path(__file__).resolve().parent
MANIFEST_SCRIPT = SCRIPT_DIR / "am3_bb_mtd_backup_manifest.sh"
PROBE_SCRIPT = SCRIPT_DIR / "am3_bb_sd_recovery_probe.sh"
HOST_KEY = "SHA256:" + "A" * 43
MODEL = "BeagleBone_Black_v2.1_on_S19J_IO_BOARD_V2_0"
GEOMETRY = (
    (0, "00020000", "spl"),
    (1, "00020000", "spl_backup1"),
    (2, "00020000", "spl_backup2"),
    (3, "00020000", "spl_backup3"),
    (4, "001c0000", "u-boot"),
    (5, "00020000", "bootenv"),
    (6, "00020000", "fdt"),
    (7, "00500000", "kernel"),
    (8, "01400000", "root"),
    (9, "00200000", "config"),
    (10, "00200000", "sig"),
    (11, "06000000", "nvdata"),
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
    if os.name == "nt" and re.fullmatch(r"[A-Za-z]:[\\/].*", value):
        return wsl_path(Path(value))
    return value


def run_bash(
    script: Path, *arguments: str, environment: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    if os.name == "nt":
        command = [
            "wsl", "-e", "env",
            *(f"{key}={value}" for key, value in (environment or {}).items()),
            "bash", wsl_path(script),
            *(bash_path(argument) for argument in arguments),
        ]
        env = None
    else:
        command = ["bash", str(script), *arguments]
        env = os.environ.copy()
        env.update(environment or {})
    return subprocess.run(command, check=False, capture_output=True, text=True, env=env)


def make_executable(path: Path) -> None:
    if os.name == "nt":
        subprocess.run(["wsl", "-e", "chmod", "+x", wsl_path(path)], check=True)
    else:
        path.chmod(0o700)


def fake_path(path: Path) -> str:
    prefix = wsl_path(path) if os.name == "nt" else str(path)
    return prefix + ":/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"


def evidence_text(geometry: tuple[tuple[int, str, str], ...] = GEOMETRY) -> str:
    rows = "\n".join(
        f'mtd{number}: {size} 00020000 "{name}"'
        for number, size, name in geometry
    )
    return f"=== proc cmdline ===\nconsole=ttyS0 root=/dev/mtdblock11 rw\n=== mtd layout ===\n{rows}\n=== next ===\n"


def manifest_text() -> str:
    rows = "\n".join(
        f"| /dev/mtd{number} | 0x{size} | 0x00020000 | {name} | mtd{number}_{name}.nanddump |"
        for number, size, name in GEOMETRY
    )
    return (
        "# AM3-BB MTD Backup Manifest\n\n"
        "- `layout_profile_candidate=1`\n"
        "- `root_mtdblock11_candidate=1`\n"
        "- `backup_scope=data-only-no-oob`\n"
        "- `restore_authority=none-until-physical-rehearsal`\n"
        "- `nand_backup_execute_go=0`\n"
        "- `nand_write_go=0`\n"
        "- `persistent_install_go=0`\n\n"
        "| Node | Size Hex | Erase Hex | Name | Required Artifact |\n"
        "| --- | --- | --- | --- | --- |\n"
        f"{rows}\n"
    )


def preflight_remote(**overrides: str) -> str:
    values = {
        "mac": "02:00:00:00:00:79",
        "hwid": "AM3-BB-FIXTURE-79",
        "model": MODEL,
        "compatible": "ti_am335x-bone-black",
        "board_target": "am3-bb-s19jpro",
        "boot_id": "12345678-1234-1234-1234-123456789abc",
        "root_source": "/dev/mmcblk0p2",
        "root_removable": "1",
        "nanddump": "/usr/sbin/nanddump",
        "pgrep": "/usr/bin/pgrep",
        "writable_mtd_mounts": "0",
        "miners_status": "no_matches",
        "miners": "",
    }
    values.update(overrides)
    identity = "".join(
        f"{key}={values[key]}\n"
        for key in (
            "mac", "hwid", "model", "compatible", "board_target", "boot_id",
            "root_source", "root_removable",
        )
    )
    rows = "\n".join(
        f'mtd{number}: {size} 00020000 "{name}"'
        for number, size, name in GEOMETRY
    )
    tools = "".join(
        f"{key}={values[key]}\n"
        for key in (
            "nanddump", "pgrep", "writable_mtd_mounts", "miners_status", "miners"
        )
    )
    return f"{identity}mtd_begin\ndev: size erasesize name\n{rows}\nmtd_end\n{tools}"


def runtime_remote(**overrides: str) -> str:
    values = {
        "boot_id": "12345678-1234-1234-1234-123456789abc",
        "root_source": "/dev/mmcblk0p2",
        "root_removable": "1",
        "pgrep": "/usr/bin/pgrep",
        "writable_mtd_mounts": "0",
        "miners_status": "no_matches",
        "miners": "",
    }
    values.update(overrides)
    return "".join(f"{key}={value}\n" for key, value in values.items())


def proof_text(keys: set[str], values: dict[str, str]) -> str:
    if set(values) != keys:
        raise AssertionError("fixture proof keys are not exact")
    return "".join(f"{key}={values[key]}\n" for key in sorted(keys))


class Am3BbEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.now = datetime.now(timezone.utc).replace(microsecond=0)

    def make_plan_inputs(self, root: Path) -> argparse.Namespace:
        stamp = self.now.strftime("%Y-%m-%dT%H:%M:%SZ")
        manifest = root / "manifest.md"
        sd = root / "sd.txt"
        restore = root / "restore.txt"
        manifest.write_text(manifest_text(), encoding="utf-8")
        sd.write_text(
            proof_text(
                SD_KEYS,
                {
                    "schema": "am3_bb_sd_recovery_proof_v1",
                    "timestamp_utc": stamp,
                    "ip": "192.0.2.79",
                    "ssh_host_key_authentication": "verified",
                    "ssh_host_key_sha256": HOST_KEY,
                    "identity_mac": "02:00:00:00:00:79",
                    "identity_hwid": "AM3-BB-FIXTURE-79",
                    "identity_model": MODEL,
                    "identity_compatible": "ti_am335x-bone-black",
                    "identity_target": "am3-bb-s19jpro",
                    "root_source": "/dev/mmcblk0p2",
                    "root_removable": "1",
                    "identity": "pass am3_bb_s19j_io_board_v2_0",
                    "external_boot": "pass root_device_exact_removable_mmc",
                    "mtd_geometry": "pass exact_am3_bb_12_partition",
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
                    "schema": "am3_bb_restore_artifact_proof_v1",
                    "restore_verified": "1",
                    "restore_mac": "02:00:00:00:00:79",
                    "restore_hwid": "AM3-BB-FIXTURE-79",
                    "restore_model": MODEL,
                    "restore_target": "am3-bb-s19jpro",
                    "restore_artifact_sha256": "1" * 64,
                    "restore_verified_utc": stamp,
                },
            ),
            encoding="utf-8",
        )
        return argparse.Namespace(
            manifest=manifest,
            restore_proof=restore,
            sd_recovery_proof=sd,
            expect_ip="192.0.2.79",
            expect_host_key_sha256=HOST_KEY,
            expect_mac="02:00:00:00:00:79",
            expect_hwid="AM3-BB-FIXTURE-79",
            expect_model=MODEL,
            expect_target="am3-bb-s19jpro",
            readback_verify=True,
            output=root / "plan.md",
            json_template=root / "plan.json",
        )

    def test_validator_self_tests(self) -> None:
        self.assertEqual(run_plan_self_test(), 0)
        self.assertEqual(run_result_self_test(), 0)

    def test_plan_builder_emits_strict_fresh_plan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            _, encoded = build(args, self.now)
            plan = json.loads(encoded)
            self.assertEqual(plan["plan_ready"], 1)
            self.assertEqual(len(plan["partitions"]), 12)
            self.assertEqual(plan["partitions"][11]["size_bytes"], 0x06000000)
            validate_plan(plan, now=self.now)

    def test_plan_publishes_executable_json_last(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-publish-") as temp:
            root = Path(temp)
            args = argparse.Namespace(
                output=root / "plan.md",
                json_template=root / "plan.json",
            )
            calls: list[Path] = []

            def fail_second_publish(path: Path, payload: bytes) -> None:
                calls.append(path)
                if len(calls) == 2:
                    raise ValidationError("injected JSON commit publication failure")
                path.write_bytes(payload)

            with (
                mock.patch.object(plan, "parse_args", return_value=args),
                mock.patch.object(
                    plan, "build", return_value=(b"plan_ready=1\n", b"{}\n")
                ),
                mock.patch.object(
                    plan, "atomic_publish", side_effect=fail_second_publish
                ),
            ):
                self.assertEqual(plan.main([]), 1)
            self.assertEqual(calls, [args.output, args.json_template])
            self.assertEqual(args.output.read_bytes(), b"plan_ready=1\n")
            self.assertFalse(args.json_template.exists())

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_plan_signal_after_authority_commit_cannot_revoke_plan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-signal-") as temp:
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
spec = importlib.util.spec_from_file_location("am3_plan_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.parse_args = lambda _argv=None: argparse.Namespace(
    output=Path({str(output)!r}),
    json_template=Path({str(json_template)!r}),
)
module.build = lambda _args, _now: (b"review\\n", b"{{}}\\n")
real_publish = module.atomic_publish
publish_calls = 0

def signal_after_authority(path, payload):
    global publish_calls
    real_publish(path, payload)
    publish_calls += 1
    if publish_calls == 2:
        os.kill(os.getpid(), signal.SIGTERM)

module.atomic_publish = signal_after_authority
raise SystemExit(module.main([]))
"""
            result = subprocess.run(
                [sys.executable, "-c", code],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("ignored signal", result.stderr)
            self.assertEqual(output.read_bytes(), b"review\n")
            self.assertEqual(json_template.read_bytes(), b"{}\n")
            self.assertIn("plan_ready=1", result.stdout)
            self.assertEqual(list(root.glob("*.publication-pending.*")), [])

    def test_plan_rejects_output_path_aliases_before_build(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-alias-") as temp:
            root = Path(temp)
            aliases = (
                (root / "plan.md", root / "plan.md"),
                (root / "nested" / ".." / "plan.md", root / "plan.md"),
                (root / "PLAN.MD", root / "plan.md"),
            )
            for output, json_template in aliases:
                with self.subTest(output=output, json_template=json_template):
                    args = argparse.Namespace(
                        output=output, json_template=json_template
                    )
                    with (
                        mock.patch.object(plan, "parse_args", return_value=args),
                        mock.patch.object(plan, "build") as build_mock,
                    ):
                        self.assertEqual(plan.main([]), 1)
                    build_mock.assert_not_called()

    def test_plan_atomic_publish_is_no_clobber(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-atomic-") as temp:
            destination = Path(temp) / "plan.json"
            plan.atomic_publish(destination, b"first\n")
            with self.assertRaisesRegex(ValidationError, "refusing to replace"):
                plan.atomic_publish(destination, b"second\n")
            self.assertEqual(destination.read_bytes(), b"first\n")

    def test_plan_retained_staging_name_does_not_false_fail(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-cleanup-") as temp:
            destination = Path(temp) / "plan.json"
            real_unlink = Path.unlink

            def publish_with_retained_name(
                staging: Path,
                output: Path,
                *,
                require_directory_sync: bool = False,
            ) -> tuple[str, str]:
                self.assertTrue(require_directory_sync)
                self.assertIn(".publication-pending.", staging.name)
                output.write_bytes(staging.read_bytes())
                return "pass", "retained_errno_13"

            def reject_staging_cleanup(
                path: Path, *args: object, **kwargs: object
            ) -> None:
                if path.name.startswith(".plan.json."):
                    raise PermissionError("injected persistent cleanup failure")
                real_unlink(path, *args, **kwargs)

            with (
                mock.patch.object(
                    plan,
                    "publish_staged_file",
                    side_effect=publish_with_retained_name,
                ),
                mock.patch.object(Path, "unlink", new=reject_staging_cleanup),
            ):
                plan.atomic_publish(destination, b"ready\n")
            self.assertEqual(destination.read_bytes(), b"ready\n")

    def test_plan_prelink_failure_quarantines_pass_shaped_staging(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-failed-") as temp:
            root = Path(temp)
            destination = root / "plan.json"
            real_unlink = Path.unlink

            def reject_old_cleanup(
                path: Path, *args: object, **kwargs: object
            ) -> None:
                if path.name.startswith(".plan.json."):
                    raise PermissionError("injected persistent cleanup failure")
                real_unlink(path, *args, **kwargs)

            with (
                mock.patch.object(
                    plan,
                    "publish_staged_file",
                    side_effect=plan.PublishError("injected pre-link failure"),
                ),
                mock.patch.object(Path, "unlink", new=reject_old_cleanup),
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

    @unittest.skipUnless(os.name == "nt", "Windows path semantics only")
    def test_windows_root_sync_path_remains_anchored(self) -> None:
        root = Path(Path.cwd().anchor)
        target = backup_io._windows_directory_handle_path(root)
        self.assertEqual(target, str(root))
        self.assertTrue(target.endswith("\\"))
        self.assertNotEqual(target, root.drive)

    def test_stale_or_duplicate_proof_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            stale = (self.now - timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%SZ")
            text = args.sd_recovery_proof.read_text(encoding="utf-8")
            args.sd_recovery_proof.write_text(
                re.sub(r"timestamp_utc=.*", f"timestamp_utc={stale}", text),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValidationError, "at least 86400"):
                build(args, self.now)
            args = self.make_plan_inputs(Path(temp))
            with args.sd_recovery_proof.open("a", encoding="utf-8") as handle:
                handle.write("ip=192.0.2.79\n")
            with self.assertRaisesRegex(ValidationError, "duplicate field"):
                build(args, self.now)

    def test_contradictory_manifest_marker_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-plan-") as temp:
            args = self.make_plan_inputs(Path(temp))
            with args.manifest.open("a", encoding="utf-8") as handle:
                handle.write("- `layout_profile_candidate=0`\n")
            with self.assertRaisesRegex(ValidationError, "duplicate contract marker"):
                build(args, self.now)

    def test_manifest_parser_requires_exact_geometry(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-manifest-") as temp:
            root = Path(temp)
            evidence = root / "evidence.txt"
            output = root / "manifest.md"
            evidence.write_text(evidence_text(), encoding="utf-8")
            result = run_bash(MANIFEST_SCRIPT, "--evidence", str(evidence), "--output", str(output))
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            rendered = output.read_text(encoding="utf-8")
            self.assertIn("layout_profile_candidate=1", rendered)
            retained = output.read_bytes()
            collision = run_bash(
                MANIFEST_SCRIPT,
                "--evidence",
                str(evidence),
                "--output",
                str(output),
            )
            self.assertNotEqual(collision.returncode, 0)
            self.assertEqual(output.read_bytes(), retained)
            wrong = list(GEOMETRY)
            wrong[4] = (4, "00020000", "u-boot")
            wrong_output = root / "wrong.md"
            evidence.write_text(evidence_text(tuple(wrong)), encoding="utf-8")
            result = run_bash(MANIFEST_SCRIPT, "--evidence", str(evidence), "--output", str(wrong_output))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("layout_profile_candidate=0", wrong_output.read_text(encoding="utf-8"))

            duplicate_output = root / "duplicate-root.md"
            evidence.write_text(
                evidence_text() + "=== proc cmdline ===\nroot=/dev/mtdblock11 ro\n",
                encoding="utf-8",
            )
            result = run_bash(MANIFEST_SCRIPT, "--evidence", str(evidence), "--output", str(duplicate_output))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("root_mtdblock11_candidate=0", duplicate_output.read_text(encoding="utf-8"))

    def test_probe_uses_pinned_host_and_exact_private_identity(self) -> None:
        source = PROBE_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("StrictHostKeyChecking=yes", source)
        self.assertNotIn("StrictHostKeyChecking=no", source)
        self.assertIn("root_device_exact_removable_mmc", source)
        self.assertIn("exact_am3_bb_12_partition", source)
        self.assertIn("chmod 600", source)
        self.assertIn("atomic_publish_file.py", source)
        self.assertIn("durable_file_io.py", source)
        self.assertIn("--require-directory-sync", source)
        self.assertIn(".publication-pending.", source)
        self.assertIn("trap cleanup_tmp EXIT", source)
        self.assertIn("trap 'exit 1' HUP INT TERM", source)
        self.assertIn("set -o noclobber", source)
        self.assertIn("trap 'ALLOCATION_SIGNAL=1' HUP INT TERM", source)
        self.assertIn("trap '' HUP INT TERM; set -o noclobber", source)
        self.assertNotIn("mktemp", source)
        self.assertNotIn('ln -- "$TMP" "$OUTPUT"', source)
        self.assertNotIn('mv "$TMP" "$OUTPUT"', source)

        manifest_source = MANIFEST_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("atomic_publish_file.py", manifest_source)
        self.assertIn("durable_file_io.py", manifest_source)
        self.assertIn("--require-directory-sync", manifest_source)
        self.assertIn(".publication-pending.", manifest_source)
        self.assertIn("trap cleanup_tmp EXIT", manifest_source)
        self.assertIn("trap 'exit 1' HUP INT TERM", manifest_source)
        self.assertIn("set -o noclobber", manifest_source)
        self.assertIn("trap 'ALLOCATION_SIGNAL=1' HUP INT TERM", manifest_source)
        self.assertIn("trap '' HUP INT TERM; set -o noclobber", manifest_source)
        self.assertNotIn("mktemp", manifest_source)
        self.assertNotIn('ln -- "$TMP" "$OUTPUT"', manifest_source)

    def test_probe_fake_endpoint_publishes_exact_receipt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-probe-") as temp:
            root = Path(temp)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            known_hosts = root / "known_hosts"
            known_hosts.write_text("192.0.2.79 ssh-ed25519 AAAAFIXTURE\n", encoding="utf-8")
            keygen = fake_bin / "ssh-keygen"
            keygen.write_bytes(
                (
                    "#!/bin/sh\ncase \"$1\" in\n"
                    "-F) printf '192.0.2.79 ssh-ed25519 AAAAFIXTURE\\n';;\n"
                    f"-lf) cat >/dev/null; printf '256 {HOST_KEY} fixture (ED25519)\\n';;\n"
                    "*) exit 90;;\nesac\n"
                ).encode("utf-8")
            )
            rows = "\n".join(f'mtd{n}: {s} 00020000 "{name}"' for n, s, name in GEOMETRY)
            ssh = fake_bin / "ssh"
            ssh.write_bytes(
                (
                    "#!/bin/sh\ncat >/dev/null\ncat <<'EOF'\n"
                    "mac=02:00:00:00:00:79\nhwid=AM3-BB-FIXTURE-79\n"
                    f"model={MODEL}\ncompatible=ti_am335x-bone-black\n"
                    f"root_source=/dev/mmcblk0p2\nroot_removable=1\nmtd_begin\ndev: size erasesize name\n{rows}\nmtd_end\nEOF\n"
                ).encode("utf-8")
            )
            make_executable(keygen)
            make_executable(ssh)
            artifacts = root / "private"
            result = run_bash(
                PROBE_SCRIPT, "192.0.2.79", "--artifact-dir", str(artifacts),
                "--known-hosts", str(known_hosts), "--expected-host-key-sha256", HOST_KEY,
                "--expect-mac", "02:00:00:00:00:79", "--expect-hwid", "AM3-BB-FIXTURE-79",
                environment={"PATH": fake_path(fake_bin)},
            )
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            receipts = list(artifacts.glob("*_sd_recovery_proof.txt"))
            self.assertEqual(len(receipts), 1)
            receipt = receipts[0].read_text(encoding="utf-8")
            self.assertIn("identity_mac=02:00:00:00:00:79", receipt)
            self.assertIn("sd_recovery_probe=pass", receipt)

            ssh.write_bytes(
                (
                    "#!/bin/sh\ncat >/dev/null\ncat <<'EOF'\n"
                    "mac=02:00:00:00:00:79\nhwid=AM3-BB-FIXTURE-79\n"
                    f"model={MODEL}\ncompatible=ti_am335x-bone-black\n"
                    f"root_source=/dev/mmcblk0p2\nroot_removable=0\nmtd_begin\ndev: size erasesize name\n{rows}\nmtd_end\nEOF\n"
                ).encode("utf-8")
            )
            rejected = root / "rejected"
            result = run_bash(
                PROBE_SCRIPT, "192.0.2.79", "--artifact-dir", str(rejected),
                "--known-hosts", str(known_hosts), "--expected-host-key-sha256", HOST_KEY,
                "--expect-mac", "02:00:00:00:00:79", "--expect-hwid", "AM3-BB-FIXTURE-79",
                environment={"PATH": fake_path(fake_bin)},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(rejected.glob("*_sd_recovery_proof.txt")), [])

    def test_executor_preflight_rejects_geometry_drift(self) -> None:
        args = argparse.Namespace(
            local_backup_dir=Path("unused"), known_hosts=Path("known_hosts"),
            ssh_user="root", target="192.0.2.79", ssh_password_env="UNSET_TEST_PASSWORD",
            timeout=1, skip_size_check=True,
        )
        executor = execute.Executor(
            args,
            {"mac": "02:00:00:00:00:79", "hwid": "AM3-BB-FIXTURE-79", "model": MODEL, "board_target": "am3-bb-s19jpro", "root_device": "/dev/mmcblk0p2"},
            [],
        )
        remote = (
            "mac=02:00:00:00:00:79\nhwid=AM3-BB-FIXTURE-79\n"
            f"model={MODEL}\ncompatible=ti_am335x-bone-black\nboard_target=am3-bb-s19jpro\n"
            "boot_id=12345678-1234-1234-1234-123456789abc\n"
            "root_source=/dev/mmcblk0p2\nroot_removable=1\nmtd_begin\ndev: size erasesize name\n"
            'mtd0: 00020000 00020000 "wrong"\nmtd_end\nnanddump=/usr/sbin/nanddump\npgrep=/usr/bin/pgrep\nwritable_mtd_mounts=0\nminers_status=no_matches\nminers=\n'
        )
        executor.ssh_text = mock.Mock(return_value=remote)  # type: ignore[method-assign]
        with self.assertRaisesRegex(ValidationError, "geometry mismatch"):
            executor.preflight()
        command = executor.ssh_text.call_args.args[0]
        self.assertIn("^mtd([0-9]+)?:", command)
        self.assertIn('$3 == "jffs2"', command)

    def test_executor_preflight_rejects_unclassified_runtime_state(self) -> None:
        args = argparse.Namespace(
            local_backup_dir=Path("unused"), known_hosts=Path("known_hosts"),
            ssh_user="root", target="192.0.2.79", ssh_password_env="UNSET_TEST_PASSWORD",
            timeout=1, skip_size_check=True,
        )
        expected = {
            "mac": "02:00:00:00:00:79", "hwid": "AM3-BB-FIXTURE-79",
            "model": MODEL, "board_target": "am3-bb-s19jpro",
            "root_device": "/dev/mmcblk0p2",
        }
        for mutation, error in (
            ({"root_removable": "0"}, "removable"),
            ({"miners_status": "error", "miners": "error"}, "process status"),
            ({"writable_mtd_mounts": "1"}, "writable MTD/UBI"),
        ):
            with self.subTest(mutation=mutation):
                runner = execute.Executor(args, expected, [])
                runner.ssh_text = mock.Mock(return_value=preflight_remote(**mutation))  # type: ignore[method-assign]
                with self.assertRaisesRegex(ValidationError, error):
                    runner.preflight()

    def test_runtime_gate_rejects_state_drift(self) -> None:
        args = argparse.Namespace(
            local_backup_dir=Path("unused"), known_hosts=Path("known_hosts"),
            ssh_user="root", target="192.0.2.79", ssh_password_env="UNSET_TEST_PASSWORD",
            timeout=1, skip_size_check=True,
        )
        runner = execute.Executor(
            args,
            {
                "mac": "02:00:00:00:00:79", "hwid": "AM3-BB-FIXTURE-79",
                "model": MODEL, "board_target": "am3-bb-s19jpro",
                "root_device": "/dev/mmcblk0p2",
            },
            [],
        )
        boot_id = "12345678-1234-1234-1234-123456789abc"
        runner.ssh_text = mock.Mock(return_value=runtime_remote())  # type: ignore[method-assign]
        runner.runtime_gate(boot_id)
        for mutation, error in (
            ({"boot_id": "87654321-1234-1234-1234-123456789abc"}, "rebooted"),
            ({"root_source": "/dev/mmcblk1p2"}, "root admission"),
            ({"miners_status": "matches", "miners": "present"}, "miner state"),
            ({"writable_mtd_mounts": "1"}, "writable MTD/UBI"),
            ({"pgrep": ""}, "pgrep"),
        ):
            with self.subTest(mutation=mutation):
                runner.ssh_text = mock.Mock(return_value=runtime_remote(**mutation))  # type: ignore[method-assign]
                with self.assertRaisesRegex(ValidationError, error):
                    runner.runtime_gate(boot_id)

    def test_executor_data_path_requires_two_exact_reads(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-") as temp:
            root = Path(temp)
            args = argparse.Namespace(
                local_backup_dir=root / "backup", known_hosts=root / "known_hosts",
                ssh_user="root", target="192.0.2.79", ssh_password_env="UNSET_TEST_PASSWORD",
                timeout=1, skip_size_check=True,
            )
            rows = ["0|spl|64|mtd0_spl.nanddump", "5|bootenv|32|mtd5_bootenv.nanddump"]
            runner = execute.Executor(
                args,
                {"mac": "02:00:00:00:00:79", "hwid": "AM3-BB-FIXTURE-79", "model": MODEL, "board_target": "am3-bb-s19jpro", "root_device": "/dev/mmcblk0p2"},
                rows,
            )
            runner.preflight = mock.Mock(return_value=("02:00:00:00:00:79", "AM3-BB-FIXTURE-79", MODEL, "ti_am335x-bone-black", "am3-bb-s19jpro", "12345678-1234-1234-1234-123456789abc"))  # type: ignore[method-assign]
            runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
            reads: list[str] = []
            def stream(command: str, destination: object) -> None:
                reads.append(command)
                size = 64 if "/dev/mtd0" in command else 32
                destination.write(bytes([size]) * size)  # type: ignore[attr-defined]
            runner.ssh_stream = stream  # type: ignore[method-assign]
            small = {execute.LAYOUT_NAME: ((0, "spl", 64), (5, "bootenv", 32))}
            with (
                mock.patch.object(execute, "EXPECTED_LAYOUTS", small),
                mock.patch.object(execute, "validate_backup"),
                mock.patch.object(
                    execute,
                    "fsync_directory",
                    wraps=execute.fsync_directory,
                ) as directory_sync,
            ):
                manifest = runner.run()
            self.assertTrue(manifest.is_file())
            self.assertEqual(reads.count("nanddump --bb=padbad --omitoob /dev/mtd0"), 2)
            self.assertEqual(reads.count("nanddump --bb=padbad --omitoob /dev/mtd5"), 2)
            self.assertEqual(runner.runtime_gate.call_count, 5)
            self.assertEqual((args.local_backup_dir / "mtd0_spl.nanddump").stat().st_size, 64)
            self.assertGreaterEqual(directory_sync.call_count, 2)

    def test_executor_publication_is_no_clobber(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-publish-") as temp:
            root = Path(temp)
            destination = root / "artifact.nanddump"
            first = root / ".artifact.first"
            first.write_bytes(b"first")
            execute.Executor.publish(first, destination)
            self.assertEqual(destination.read_bytes(), b"first")
            self.assertFalse(first.exists())

            contender = root / ".artifact.contender"
            contender.write_bytes(b"second")
            with self.assertRaisesRegex(ValidationError, "cannot publish artifact"):
                execute.Executor.publish(contender, destination)
            self.assertEqual(destination.read_bytes(), b"first")
            self.assertEqual(contender.read_bytes(), b"second")

    def test_executor_post_link_sync_fault_quarantines_destination(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-sync-") as temp:
            root = Path(temp)
            destination = root / "result.manifest.json"
            staged = root / ".result.manifest.json.staged"
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
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-commit-") as temp:
            root = Path(temp)
            args = argparse.Namespace(
                local_backup_dir=root / "backup",
                known_hosts=root / "known_hosts",
                ssh_user="root",
                target="192.0.2.79",
                ssh_password_env="UNSET_TEST_PASSWORD",
                timeout=1,
                skip_size_check=True,
            )
            runner = execute.Executor(
                args,
                {
                    "mac": "02:00:00:00:00:79",
                    "hwid": "AM3-BB-FIXTURE-79",
                    "model": MODEL,
                    "board_target": "am3-bb-s19jpro",
                    "root_device": "/dev/mmcblk0p2",
                },
                ["0|spl|64|mtd0_spl.nanddump"],
            )
            runner.preflight = mock.Mock(
                return_value=(
                    "02:00:00:00:00:79",
                    "AM3-BB-FIXTURE-79",
                    MODEL,
                    "ti_am335x-bone-black",
                    "am3-bb-s19jpro",
                    "12345678-1234-1234-1234-123456789abc",
                )
            )  # type: ignore[method-assign]
            runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
            runner.ssh_stream = lambda _command, destination: destination.write(
                b"x" * 64
            )  # type: ignore[method-assign]
            small = {execute.LAYOUT_NAME: ((0, "spl", 64),)}
            real_publish = execute.publish_staged_file
            real_unlink = Path.unlink

            def reject_manifest(
                staging: Path,
                destination: Path,
                *,
                require_directory_sync: bool = False,
                _after_staged_open: object | None = None,
            ) -> object:
                self.assertTrue(require_directory_sync)
                if destination.name == "am3_bb_nand_backup.manifest.json":
                    self.assertIn(".publication-pending.", staging.name)
                    self.assertTrue(callable(_after_staged_open))
                    _after_staged_open()
                    raise execute.PublishError("injected result commit failure")
                return real_publish(
                    staging,
                    destination,
                    require_directory_sync=require_directory_sync,
                )

            def reject_old_manifest_cleanup(
                path: Path, *args: object, **kwargs: object
            ) -> None:
                if path.name.startswith(
                    ".am3_bb_nand_backup.manifest.json.publication-pending."
                ):
                    raise PermissionError("injected persistent cleanup failure")
                real_unlink(path, *args, **kwargs)

            with (
                mock.patch.object(execute, "EXPECTED_LAYOUTS", small),
                mock.patch.object(execute, "validate_backup"),
                mock.patch.object(
                    execute,
                    "publish_staged_file",
                    side_effect=reject_manifest,
                ),
                mock.patch.object(
                    Path,
                    "unlink",
                    new=reject_old_manifest_cleanup,
                ),
            ):
                with self.assertRaisesRegex(ValidationError, "commit failure"):
                    runner.run()
            self.assertFalse(
                (args.local_backup_dir / "am3_bb_nand_backup.manifest.json").exists()
            )
            self.assertEqual(list(args.local_backup_dir.glob(".manifest.*")), [])
            quarantine_directories = list(
                args.local_backup_dir.glob(
                    ".am3_bb_nand_backup.manifest.json.publication-failed.*"
                )
            )
            self.assertEqual(len(quarantine_directories), 1)
            quarantined = json.loads(
                (quarantine_directories[0] / "staged").read_text(encoding="utf-8")
            )
            self.assertEqual(quarantined["nand_backup_complete"], "pass")

    def test_executor_rejects_short_or_divergent_streams(self) -> None:
        for mode, expected_error in (("short", "size mismatch"), ("divergent", "readback mismatch")):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(prefix="dcent-am3-exec-") as temp:
                root = Path(temp)
                args = argparse.Namespace(
                    local_backup_dir=root / "backup", known_hosts=root / "known_hosts",
                    ssh_user="root", target="192.0.2.79", ssh_password_env="UNSET_TEST_PASSWORD",
                    timeout=1, skip_size_check=True,
                )
                runner = execute.Executor(
                    args,
                    {
                        "mac": "02:00:00:00:00:79", "hwid": "AM3-BB-FIXTURE-79",
                        "model": MODEL, "board_target": "am3-bb-s19jpro",
                        "root_device": "/dev/mmcblk0p2",
                    },
                    ["0|spl|64|mtd0_spl.nanddump"],
                )
                runner.preflight = mock.Mock(return_value=("02:00:00:00:00:79", "AM3-BB-FIXTURE-79", MODEL, "ti_am335x-bone-black", "am3-bb-s19jpro", "12345678-1234-1234-1234-123456789abc"))  # type: ignore[method-assign]
                runner.runtime_gate = mock.Mock()  # type: ignore[method-assign]
                calls = 0
                def stream(_command: str, destination: object) -> None:
                    nonlocal calls
                    calls += 1
                    if mode == "short":
                        destination.write(b"short")  # type: ignore[attr-defined]
                    else:
                        destination.write(bytes([calls]) * 64)  # type: ignore[attr-defined]
                runner.ssh_stream = stream  # type: ignore[method-assign]
                small = {execute.LAYOUT_NAME: ((0, "spl", 64),)}
                with mock.patch.object(execute, "EXPECTED_LAYOUTS", small), mock.patch.object(execute, "validate_backup"):
                    with self.assertRaisesRegex(ValidationError, expected_error):
                        runner.run()
                self.assertFalse((args.local_backup_dir / "am3_bb_nand_backup.manifest.json").exists())

    def test_executor_rejects_stale_plan_before_output(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            stamp = (self.now - timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = stamp
            plan["pre_flight"]["restore_verified_utc"] = stamp
            plan["pre_flight"]["sd_recovery_verified_utc"] = stamp
            plan_path = root / "stale.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")
            known_hosts = root / "known_hosts"
            known_hosts.write_text("fixture\n", encoding="utf-8")
            output = root / "must-not-exist"
            with mock.patch.dict(os.environ, {"DCENT_NAND_BACKUP_AUTHORIZED": "1"}), mock.patch.object(execute.shutil, "which", return_value="tool"):
                result = execute.main([
                    "--target", "192.0.2.79", "--plan", str(plan_path),
                    "--local-backup-dir", str(output), "--known-hosts", str(known_hosts),
                    "--expected-host-key-sha256", HOST_KEY,
                    "--operator-authorized-backup", "--readback-verify",
                ])
            self.assertEqual(result, 1)
            self.assertFalse(output.exists())

    def test_executor_rejects_wrong_pinned_key_before_output(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dcent-am3-exec-") as temp:
            root = Path(temp)
            plan = fixture_plan()
            stamp = self.now.strftime("%Y-%m-%dT%H:%M:%SZ")
            plan["generated_utc"] = stamp
            plan["pre_flight"]["restore_verified_utc"] = stamp
            plan["pre_flight"]["sd_recovery_verified_utc"] = stamp
            plan_path = root / "plan.json"
            plan_path.write_text(json.dumps(plan), encoding="utf-8")
            known_hosts = root / "known_hosts"
            known_hosts.write_text("fixture\n", encoding="utf-8")
            output = root / "must-not-exist"
            with (
                mock.patch.dict(os.environ, {"DCENT_NAND_BACKUP_AUTHORIZED": "1"}),
                mock.patch.object(execute.shutil, "which", return_value="tool"),
                mock.patch.object(execute, "pinned_fingerprint", return_value="SHA256:" + "B" * 43),
            ):
                result = execute.main([
                    "--target", "192.0.2.79", "--plan", str(plan_path),
                    "--local-backup-dir", str(output), "--known-hosts", str(known_hosts),
                    "--expected-host-key-sha256", HOST_KEY,
                    "--operator-authorized-backup", "--readback-verify",
                ])
            self.assertEqual(result, 1)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
