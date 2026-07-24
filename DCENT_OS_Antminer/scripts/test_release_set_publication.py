#!/usr/bin/env python3
"""Adversarial tests for exact atomic release-set publication."""

from __future__ import annotations

import argparse
from contextlib import redirect_stdout
import ctypes
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("release_set_publication.py")
DESCRIPTOR = ".dcent-release-set.json"


class ReleaseSetPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-release-set-")
        self.root = Path(self.temporary.name)
        self.stage_parent = self.root / "stages"
        self.output_parent = self.root / "published"
        self.stage_parent.mkdir(mode=0o700)
        if os.name == "posix":
            self.stage_parent.chmod(0o700)
        self.output_parent.mkdir()
        self.capability_file = self.root / "capability.json"
        self.manifest_file = self.root / "files.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_cli(
        self,
        *arguments: str,
        input_text: str | None = None,
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            input=input_text,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )

    def create_stage(self) -> tuple[dict[str, object], Path]:
        result = self.run_cli("create-stage", "--parent", str(self.stage_parent))
        self.assertEqual(result.returncode, 0, result.stderr)
        capability = json.loads(result.stdout)
        self.capability_file.write_text(result.stdout, encoding="utf-8")
        return capability, Path(str(capability["stage_path"]))

    def load_module(self):
        spec = importlib.util.spec_from_file_location(
            f"release_set_publication_test_{id(self)}", SCRIPT
        )
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def quarantines(self, output_name: str = "dcentos-v1-release") -> list[Path]:
        parent = self.stage_parent if os.name == "nt" else self.output_parent
        return list(parent.glob(f".{output_name}.publication-failed.*"))

    def publish_in_process_with(
        self, hook_name: str, hook
    ) -> tuple[object, BaseException | None]:
        return self.publish_in_process_with_options({hook_name: hook})

    def publish_in_process_with_options(
        self, options: dict[str, object]
    ) -> tuple[object, BaseException | None]:
        module = self.load_module()
        real_publish_directory = module.atomic_publish_directory

        def wrapped(*args, **kwargs):
            kwargs.update(options)
            return real_publish_directory(*args, **kwargs)

        caught: BaseException | None = None
        with mock.patch.object(module, "atomic_publish_directory", wrapped):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as error:
                caught = error
        return module, caught

    def manifest_for(self, files: dict[str, bytes]) -> dict[str, object]:
        return {
            "schema": "dcentos.release-set-files.v1",
            "files": [
                {
                    "name": name,
                    "sha256": hashlib.sha256(content).hexdigest(),
                    "size": len(content),
                }
                for name, content in sorted(files.items())
            ],
        }

    def populate(self, stage: Path, files: dict[str, bytes]) -> None:
        for name, content in files.items():
            (stage / name).write_bytes(content)
        self.manifest_file.write_text(
            json.dumps(self.manifest_for(files), sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )

    def seal(self, output_name: str = "dcentos-v1-release") -> subprocess.CompletedProcess[str]:
        return self.run_cli(
            "seal-stage",
            "--capability-file",
            str(self.capability_file),
            "--manifest",
            str(self.manifest_file),
            "--output-name",
            output_name,
        )

    def manifest_stage(self, output: Path | None = None) -> subprocess.CompletedProcess[str]:
        return self.run_cli(
            "manifest-stage",
            "--capability-file",
            str(self.capability_file),
            "--output",
            str(output or self.manifest_file),
        )

    def publish(self, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return self.run_cli(
            "publish",
            "--capability-file",
            str(self.capability_file),
            "--output-parent",
            str(self.output_parent),
            env=env,
        )

    def create_sealed(
        self, files: dict[str, bytes] | None = None, output_name: str = "dcentos-v1-release"
    ) -> tuple[dict[str, object], Path, dict[str, bytes]]:
        capability, stage = self.create_stage()
        payloads = files or {"firmware.img": b"firmware", "firmware.img.sig": b"signature"}
        self.populate(stage, payloads)
        sealed = self.seal(output_name)
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        return capability, stage, payloads

    def test_success_publishes_one_exact_directory_with_metadata(self) -> None:
        capability, stage, payloads = self.create_sealed()
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        evidence = json.loads(result.stdout)
        output = self.output_parent / "dcentos-v1-release"
        self.assertFalse(stage.exists())
        self.assertEqual(Path(str(evidence["path"])), output)
        self.assertEqual(evidence["release_set_id"], capability["stage_id"])
        self.assertEqual(set(path.name for path in output.iterdir()), set(payloads) | {DESCRIPTOR})
        for name, content in payloads.items():
            self.assertEqual((output / name).read_bytes(), content)
        descriptor = json.loads((output / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "sealed")
        self.assertEqual(descriptor["files"], self.manifest_for(payloads)["files"])
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(os.lstat(output).st_mode), 0o755)
            for path in output.iterdir():
                self.assertEqual(stat.S_IMODE(path.lstat().st_mode), 0o644)
        self.assertEqual(
            evidence["descriptor_sha256"],
            hashlib.sha256((output / DESCRIPTOR).read_bytes()).hexdigest(),
        )
        queried = self.run_cli("query", "--field", "published-path", input_text=result.stdout)
        self.assertEqual(queried.returncode, 0, queried.stderr)
        self.assertEqual(queried.stdout.strip(), str(output))

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_publication_has_protected_public_read_acl(self) -> None:
        self.create_sealed()
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        module = self.load_module()
        output = self.output_parent / "dcentos-v1-release"
        module.require_public_windows_acl(
            output, "published release", directory=True
        )
        for path in output.iterdir():
            module.require_public_windows_acl(path, f"published file {path.name}")

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_public_verifier_rejects_deny_and_inherit_only_access(self) -> None:
        module = self.load_module()
        trusted = "(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)"
        policies = {
            "deny": (
                "D:P(D;;0x1;;;WD)" + trusted + "(A;OICI;0x1200a9;;;WD)"
            ),
            "inherit-only": (
                "D:P" + trusted + "(A;OICIIO;0x1200a9;;;WD)"
            ),
        }
        for name, sddl in policies.items():
            with self.subTest(policy=name):
                target = self.root / f"acl-{name}"
                target.mkdir(mode=0o700)
                module.set_windows_directory_acl(target, sddl)
                try:
                    with self.assertRaises(module.ReleaseSetError):
                        module.require_public_windows_acl(
                            target,
                            f"{name} ACL",
                            directory=True,
                        )
                finally:
                    module.set_windows_directory_acl(
                        target,
                        module.WINDOWS_PRIVATE_DIRECTORY_SDDL,
                    )

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_publication_refuses_public_stage_or_payload_acl(self) -> None:
        for target_name in (None, "firmware.img"):
            with self.subTest(target=target_name or "stage"):
                _, stage, _ = self.create_sealed()
                target = stage if target_name is None else stage / target_name
                acl = subprocess.run(
                    [
                        "icacls",
                        str(target),
                        "/grant",
                        "*S-1-1-0:(RX)",
                    ],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.assertEqual(acl.returncode, 0, acl.stderr)
                result = self.publish()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("untrusted SID S-1-1-0", result.stderr)
                self.assertTrue(stage.exists())
                self.assertFalse(
                    (self.output_parent / "dcentos-v1-release").exists()
                )
                self.capability_file.unlink()

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_public_acl_side_effect_then_error_restores_private_stage(
        self,
    ) -> None:
        _, stage, _ = self.create_sealed()
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_set_acl = primitive_globals["_windows_set_directory_acl"]
        public_sddl = primitive_globals["WINDOWS_PUBLIC_DIRECTORY_SDDL"]
        injected = False

        def set_then_fail(handle: int, sddl: str) -> None:
            nonlocal injected
            real_set_acl(handle, sddl)
            if sddl == public_sddl and not injected:
                injected = True
                raise OSError("injected ACL completion ambiguity")

        published_module, error = self.publish_in_process_with_options(
            {"_set_windows_acl": set_then_fail}
        )
        self.assertTrue(injected)
        self.assertIsInstance(error, published_module.ReleaseSetError)
        self.assertTrue(stage.exists())
        module.require_private_windows_acl(stage, "restored stage")

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_acl_reset_failure_still_quarantines_under_private_parent(
        self,
    ) -> None:
        self.create_sealed()
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_set_acl = primitive_globals["_windows_set_directory_acl"]
        private_sddl = primitive_globals["WINDOWS_PRIVATE_DIRECTORY_SDDL"]

        def fail_private_reset(handle: int, sddl: str) -> None:
            if sddl == private_sddl:
                raise OSError("injected persistent private ACL reset failure")
            real_set_acl(handle, sddl)

        def fail_after_commit() -> None:
            raise OSError("force postcommit quarantine")

        published_module, error = self.publish_in_process_with_options(
            {
                "_set_windows_acl": fail_private_reset,
                "_after_commit": fail_after_commit,
            }
        )
        self.assertIsInstance(error, published_module.ReleaseSetError)
        self.assertIn("private-acl=", str(error))
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertEqual(len(self.quarantines()), 1)
        module.require_private_windows_acl(self.stage_parent, "stage parent")

    @unittest.skipUnless(os.name == "nt", "Windows DACL regression")
    def test_windows_staging_parent_acl_change_at_boundary_is_refused(self) -> None:
        _, stage, _ = self.create_sealed()

        def expose_parent() -> None:
            result = subprocess.run(
                [
                    "icacls",
                    str(self.stage_parent),
                    "/grant",
                    "*S-1-1-0:(OI)(CI)(RX)",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            if result.returncode != 0:
                raise OSError(result.stderr)

        module, error = self.publish_in_process_with(
            "_before_commit", expose_parent
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertIn("untrusted SID S-1-1-0", str(error))
        self.assertTrue(stage.exists())
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())

    @unittest.skipUnless(os.name == "posix", "POSIX mode-boundary regression")
    def test_posix_staging_parent_mode_change_at_boundary_is_refused(self) -> None:
        _, stage, _ = self.create_sealed()

        def expose_parent() -> None:
            self.stage_parent.chmod(0o755)

        module, error = self.publish_in_process_with(
            "_before_commit", expose_parent
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertIn("commit boundary", str(error))
        self.assertTrue(stage.exists())
        self.assertEqual(stat.S_IMODE(stage.lstat().st_mode), 0o700)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())

    def test_create_stage_refuses_a_publicly_searchable_parent(self) -> None:
        if os.name == "posix":
            self.stage_parent.chmod(0o755)
            expected = "owner-only mode 0700"
        elif os.name == "nt":
            acl = subprocess.run(
                [
                    "icacls",
                    str(self.stage_parent),
                    "/grant",
                    "*S-1-1-0:(OI)(CI)(RX)",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(acl.returncode, 0, acl.stderr)
            expected = "untrusted SID S-1-1-0"
        else:
            self.skipTest("private staging policy is Linux/Windows-specific")
        result = self.run_cli("create-stage", "--parent", str(self.stage_parent))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(expected, result.stderr)
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_create_stage_closed_capability_consumer_rolls_back_stage(self) -> None:
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "create-stage",
                    "--parent",
                    str(self.stage_parent),
                ],
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
                text=True,
            )
        finally:
            os.close(write_descriptor)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot create or deliver stage capability", result.stderr)
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_create_stage_durably_publishes_private_capability_file(self) -> None:
        capability_output = self.root / "durable-capability.json"
        result = self.run_cli(
            "create-stage",
            "--parent",
            str(self.stage_parent),
            "--capability-output",
            str(capability_output),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), json.loads(capability_output.read_text()))
        capability = json.loads(capability_output.read_text(encoding="utf-8"))
        self.assertTrue(Path(str(capability["stage_path"])).is_dir())
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(capability_output.lstat().st_mode), 0o600)
        elif os.name == "nt":
            module = self.load_module()
            module.require_private_windows_acl(
                capability_output, "capability output"
            )

    def test_create_stage_capability_collision_rolls_back_stage(self) -> None:
        capability_output = self.root / "existing-capability.json"
        capability_output.write_bytes(b"sentinel")
        result = self.run_cli(
            "create-stage",
            "--parent",
            str(self.stage_parent),
            "--capability-output",
            str(capability_output),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(capability_output.read_bytes(), b"sentinel")
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_durable_capability_survives_closed_stdout_consumer(self) -> None:
        capability_output = self.root / "closed-stdout-capability.json"
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "create-stage",
                    "--parent",
                    str(self.stage_parent),
                    "--capability-output",
                    str(capability_output),
                ],
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
                text=True,
            )
        finally:
            os.close(write_descriptor)
        self.assertEqual(result.returncode, 0, result.stderr)
        capability = json.loads(capability_output.read_text(encoding="utf-8"))
        self.assertTrue(Path(str(capability["stage_path"])).is_dir())

    @unittest.skipUnless(os.name == "nt", "Windows create pinning regression")
    def test_windows_create_pins_new_stage_before_acl_setup(self) -> None:
        module = self.load_module()
        real_set_acl = module.set_windows_directory_acl
        displaced = self.root / "new-stage-displaced"
        capability_output = self.root / "pinned-create-capability.json"
        attempted = False
        blocked = False

        def attempt_stage_swap(path: Path, sddl: str) -> None:
            nonlocal attempted, blocked
            attempted = True
            try:
                path.rename(displaced)
            except OSError:
                blocked = True
            else:
                path.mkdir(mode=0o700)
            real_set_acl(path, sddl)

        with mock.patch.object(module, "set_windows_directory_acl", attempt_stage_swap):
            with redirect_stdout(io.StringIO()):
                module.create_stage(
                    argparse.Namespace(
                        parent=str(self.stage_parent),
                        capability_output=str(capability_output),
                    )
                )
        self.assertTrue(attempted)
        self.assertTrue(blocked)
        capability = json.loads(capability_output.read_text(encoding="utf-8"))
        stage = Path(str(capability["stage_path"]))
        self.assertFalse(displaced.exists())
        self.assertEqual(
            (stage.lstat().st_dev, stage.lstat().st_ino),
            (capability["stage_dev"], capability["stage_ino"]),
        )

    @unittest.skipUnless(os.name == "nt", "Windows rollback handle regression")
    def test_windows_create_acl_failure_rolls_back_pinned_descriptor(self) -> None:
        module = self.load_module()

        def reject_descriptor(path: Path, _sddl: str) -> None:
            if path.name == DESCRIPTOR:
                raise OSError("injected descriptor ACL failure")

        with mock.patch.object(module, "set_windows_file_acl", reject_descriptor):
            with self.assertRaises(module.ReleaseSetError):
                with redirect_stdout(io.StringIO()):
                    module.create_stage(
                        argparse.Namespace(parent=str(self.stage_parent))
                    )
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    @unittest.skipUnless(os.name == "nt", "Windows capability ACL regression")
    def test_capability_acl_fault_before_publication_cannot_dangle(self) -> None:
        module = self.load_module()
        capability_output = self.root / "faulted-capability.json"
        real_require = module.require_private_windows_acl

        def fault_prepared_capability(path: Path, label: str, **kwargs) -> None:
            if label == "prepared private publication":
                raise OSError("injected prepared capability ACL fault")
            real_require(path, label, **kwargs)

        with mock.patch.object(
            module, "require_private_windows_acl", fault_prepared_capability
        ):
            with self.assertRaises(module.ReleaseSetError):
                with redirect_stdout(io.StringIO()):
                    module.create_stage(
                        argparse.Namespace(
                            parent=str(self.stage_parent),
                            capability_output=str(capability_output),
                        )
                    )
        self.assertFalse(capability_output.exists())
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    @unittest.skipUnless(os.name == "posix", "POSIX create pinning regression")
    def test_posix_create_substitution_preserves_foreign_and_exact_stages(self) -> None:
        module = self.load_module()
        real_require = module.require_private_directory
        displaced = self.root / "new-stage-exact-displaced"
        swapped_stage: Path | None = None

        def substitute_after_pin(metadata, path: Path, label: str) -> None:
            nonlocal swapped_stage
            real_require(metadata, path, label)
            if label != "new stage" or swapped_stage is not None:
                return
            swapped_stage = path
            path.rename(displaced)
            path.mkdir(mode=0o700)
            (path / "sentinel").write_bytes(b"foreign")

        with mock.patch.object(
            module, "require_private_directory", substitute_after_pin
        ):
            with self.assertRaises(module.ReleaseSetError):
                with redirect_stdout(io.StringIO()):
                    module.create_stage(
                        argparse.Namespace(parent=str(self.stage_parent))
                    )
        self.assertIsNotNone(swapped_stage)
        assert swapped_stage is not None
        self.assertEqual((swapped_stage / "sentinel").read_bytes(), b"foreign")
        self.assertTrue((displaced / DESCRIPTOR).is_file())

    @unittest.skipUnless(os.name == "posix", "POSIX umask regression")
    def test_create_normalizes_modes_under_restrictive_umask(self) -> None:
        module = self.load_module()
        capability_output = self.root / "umask-capability.json"
        prior_umask = os.umask(0o777)
        try:
            with redirect_stdout(io.StringIO()):
                module.create_stage(
                    argparse.Namespace(
                        parent=str(self.stage_parent),
                        capability_output=str(capability_output),
                    )
                )
        finally:
            os.umask(prior_umask)
        capability = json.loads(capability_output.read_text(encoding="utf-8"))
        stage = Path(str(capability["stage_path"]))
        self.assertEqual(stat.S_IMODE(stage.lstat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((stage / DESCRIPTOR).lstat().st_mode), 0o600)

    @unittest.skipUnless(os.name == "posix", "POSIX early rollback regression")
    def test_create_directory_fchmod_failure_has_exact_rollback_capability(self) -> None:
        module = self.load_module()
        real_fchmod = module.os.fchmod
        injected = False

        def fail_directory_fchmod(fd: int, mode: int) -> None:
            nonlocal injected
            if not injected and stat.S_ISDIR(os.fstat(fd).st_mode):
                injected = True
                raise OSError("injected stage fchmod failure")
            real_fchmod(fd, mode)

        with mock.patch.object(module.os, "fchmod", fail_directory_fchmod):
            with self.assertRaises(module.ReleaseSetError) as caught:
                with redirect_stdout(io.StringIO()):
                    module.create_stage(
                        argparse.Namespace(parent=str(self.stage_parent))
                    )
        self.assertTrue(injected)
        self.assertNotIn("local variable 'capability'", str(caught.exception))
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_create_stage_post_mkdir_lstat_failure_retains_ambiguous_stage(self) -> None:
        module = self.load_module()
        real_lstat = module.os.lstat
        injected = False

        def faulting_lstat(path, *args, **kwargs):
            nonlocal injected
            if (
                not injected
                and Path(path).parent == self.stage_parent
                and Path(path).name.startswith(".dcent-release-set-stage-")
            ):
                injected = True
                raise OSError("injected post-mkdir lstat failure")
            return real_lstat(path, *args, **kwargs)

        with mock.patch.object(module.os, "lstat", faulting_lstat):
            with self.assertRaisesRegex(
                module.ReleaseSetError, "retaining ambiguous path"
            ):
                module.create_stage(argparse.Namespace(parent=str(self.stage_parent)))
        self.assertTrue(injected)
        retained = list(self.stage_parent.iterdir())
        self.assertEqual(len(retained), 1)
        self.assertTrue(retained[0].is_dir())

    @unittest.skipUnless(os.name == "posix", "POSIX namespace-race regression")
    def test_create_unknown_identity_never_deletes_replacement(self) -> None:
        module = self.load_module()
        real_lstat = module.os.lstat
        displaced = self.root / "new-stage-before-lstat-displaced"
        replacement: Path | None = None

        def swap_then_fail(path, *args, **kwargs):
            nonlocal replacement
            candidate = Path(path)
            if (
                replacement is None
                and candidate.parent == self.stage_parent
                and candidate.name.startswith(".dcent-release-set-stage-")
            ):
                candidate.rename(displaced)
                candidate.mkdir(mode=0o700)
                replacement = candidate
                raise OSError("injected post-mkdir identity ambiguity")
            return real_lstat(path, *args, **kwargs)

        with mock.patch.object(module.os, "lstat", swap_then_fail):
            with self.assertRaisesRegex(
                module.ReleaseSetError, "retaining ambiguous path"
            ):
                module.create_stage(argparse.Namespace(parent=str(self.stage_parent)))
        assert replacement is not None
        self.assertTrue(replacement.is_dir())
        self.assertTrue(displaced.is_dir())

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_create_stage_signal_during_descriptor_sync_rolls_back_stage(self) -> None:
        for signum in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=signum):
                code = f"""
import argparse
import importlib.util
import os
import signal
from pathlib import Path

script = Path({str(SCRIPT)!r})
spec = importlib.util.spec_from_file_location("release_set_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
real_fsync = module.os.fsync
sent = False
def faulting_fsync(fd):
    global sent
    real_fsync(fd)
    if not sent:
        sent = True
        os.kill(os.getpid(), {int(signum)})
module.os.fsync = faulting_fsync
module.create_stage(argparse.Namespace(parent={str(self.stage_parent)!r}))
"""
                result = subprocess.run(
                    [sys.executable, "-c", code],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_generated_manifest_seals_and_publishes_the_exact_set(self) -> None:
        capability, stage = self.create_stage()
        payloads = {"firmware.img": b"generated-firmware", "firmware.img.sig": b"generated-signature"}
        for name, content in payloads.items():
            (stage / name).write_bytes(content)
        manifested = self.manifest_stage()
        self.assertEqual(manifested.returncode, 0, manifested.stderr)
        self.assertEqual(self.manifest_file.read_text(encoding="utf-8"), manifested.stdout)
        self.assertEqual(json.loads(manifested.stdout), self.manifest_for(payloads))
        sealed = self.seal("generated-release")
        self.assertEqual(sealed.returncode, 0, sealed.stderr)
        published = self.publish()
        self.assertEqual(published.returncode, 0, published.stderr)
        output = self.output_parent / "generated-release"
        self.assertEqual(json.loads(published.stdout)["release_set_id"], capability["stage_id"])
        self.assertEqual(set(path.name for path in output.iterdir()), set(payloads) | {DESCRIPTOR})

    def test_manifest_output_collision_is_no_replace(self) -> None:
        _, stage = self.create_stage()
        (stage / "firmware.img").write_bytes(b"firmware")
        self.manifest_file.write_bytes(b"prior-manifest")
        result = self.manifest_stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already exists", result.stderr)
        self.assertEqual(self.manifest_file.read_bytes(), b"prior-manifest")

    def test_manifest_commit_survives_closed_result_consumer(self) -> None:
        _, stage = self.create_stage()
        (stage / "firmware.img").write_bytes(b"firmware")
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "manifest-stage",
                    "--capability-file",
                    str(self.capability_file),
                    "--output",
                    str(self.manifest_file),
                ],
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
                text=True,
            )
        finally:
            os.close(write_descriptor)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(self.manifest_file.read_text()),
            self.manifest_for({"firmware.img": b"firmware"}),
        )

    def test_manifest_uses_shared_strict_no_clobber_publication(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("publish_staged_file(", source)
        self.assertIn("require_directory_sync=True", source)
        self.assertIn("require_staged_cleanup=True", source)
        self.assertIn("expected_staged_identity=temporary_identity", source)
        self.assertNotIn("os.link(", source)

    def test_release_directory_uses_identity_bound_shared_publication(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("atomic_publish_directory(", source)
        self.assertIn("expected_staging_parent_identity=", source)
        self.assertIn("expected_destination_parent_identity=", source)
        self.assertIn("post_commit_verify=", source)
        self.assertNotIn("def atomic_rename_noreplace", source)

    def test_manifest_output_inside_stage_is_rejected(self) -> None:
        _, stage = self.create_stage()
        (stage / "firmware.img").write_bytes(b"firmware")
        inside = stage / "manifest.json"
        result = self.manifest_stage(inside)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("never be inside", result.stderr)
        self.assertFalse(inside.exists())

    def test_payload_tamper_after_generated_manifest_blocks_seal(self) -> None:
        _, stage = self.create_stage()
        payload = stage / "firmware.img"
        payload.write_bytes(b"firmware")
        manifested = self.manifest_stage()
        self.assertEqual(manifested.returncode, 0, manifested.stderr)
        payload.write_bytes(b"tampered")
        sealed = self.seal()
        self.assertNotEqual(sealed.returncode, 0)
        self.assertIn("declared size and SHA-256", sealed.stderr)

    def test_seal_rejects_stage_substitution_after_pinning(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        module = self.load_module()
        displaced = self.root / "seal-owned-stage-displaced"
        replacement_created = False

        def substitute() -> None:
            nonlocal replacement_created
            stage.rename(displaced)
            stage.mkdir(mode=0o700)
            (stage / "sentinel").write_bytes(b"foreign")
            replacement_created = True

        caught: BaseException | None = None
        try:
            with redirect_stdout(io.StringIO()):
                module.seal_stage(
                    argparse.Namespace(
                        capability_file=str(self.capability_file),
                        manifest=str(self.manifest_file),
                        output_name="dcentos-v1-release",
                        _after_stage_pinned=substitute,
                    )
                )
        except BaseException as error:
            caught = error
        self.assertIsNotNone(caught)
        owned = displaced if replacement_created else stage
        descriptor = json.loads((owned / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "building")
        if replacement_created:
            self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")

    def test_seal_sync_failure_is_retryable_from_confirmed_sealed_state(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        module = self.load_module()
        args = argparse.Namespace(
            capability_file=str(self.capability_file),
            manifest=str(self.manifest_file),
            output_name="dcentos-v1-release",
        )
        injected = False

        if os.name == "nt":
            real_sync = module.fsync_directory

            def faulting_sync(path: Path) -> None:
                nonlocal injected
                if not injected and path == stage:
                    injected = True
                    raise OSError("injected post-replace stage sync failure")
                real_sync(path)

            patcher = mock.patch.object(module, "fsync_directory", faulting_sync)
        else:
            real_fsync = module.os.fsync

            def faulting_fsync(fd: int) -> None:
                nonlocal injected
                if not injected and stat.S_ISDIR(os.fstat(fd).st_mode):
                    injected = True
                    raise OSError("injected post-replace stage sync failure")
                real_fsync(fd)

            patcher = mock.patch.object(module.os, "fsync", faulting_fsync)

        with patcher:
            with self.assertRaises(OSError):
                with redirect_stdout(io.StringIO()):
                    module.seal_stage(args)
        self.assertTrue(injected)
        descriptor = json.loads((stage / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "sealed")
        with redirect_stdout(io.StringIO()) as output:
            module.seal_stage(args)
        self.assertEqual(json.loads(output.getvalue()), descriptor)
        self.assertEqual(
            [
                path.name
                for path in stage.iterdir()
                if path.name == module.SEAL_PENDING_NAME
            ],
            [],
        )

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_signal_during_seal_sync_completes_durable_transition(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        module = self.load_module()
        real_fsync = module.os.fsync
        delivered = False

        def signal_after_sync(fd: int) -> None:
            nonlocal delivered
            real_fsync(fd)
            if not delivered and stat.S_ISDIR(os.fstat(fd).st_mode):
                delivered = True
                os.kill(os.getpid(), signal.SIGTERM)

        with mock.patch.object(module.os, "fsync", signal_after_sync):
            with redirect_stdout(io.StringIO()):
                module.seal_stage(
                    argparse.Namespace(
                        capability_file=str(self.capability_file),
                        manifest=str(self.manifest_file),
                        output_name="dcentos-v1-release",
                    )
                )
        self.assertTrue(delivered)
        descriptor = json.loads((stage / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "sealed")

    def test_seal_commit_survives_closed_result_consumer(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "seal-stage",
                    "--capability-file",
                    str(self.capability_file),
                    "--manifest",
                    str(self.manifest_file),
                    "--output-name",
                    "dcentos-v1-release",
                ],
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
                text=True,
            )
        finally:
            os.close(write_descriptor)
        self.assertEqual(result.returncode, 0, result.stderr)
        descriptor = json.loads((stage / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "sealed")

    def test_seal_fsyncs_each_verified_payload_before_descriptor_commit(self) -> None:
        _, stage = self.create_stage()
        payload = stage / "firmware.img"
        self.populate(stage, {payload.name: b"firmware"})
        payload_identity = payload.lstat().st_dev, payload.lstat().st_ino
        module = self.load_module()
        real_fsync = module.os.fsync
        payload_synced = False

        def record_sync(fd: int) -> None:
            nonlocal payload_synced
            metadata = os.fstat(fd)
            if (metadata.st_dev, metadata.st_ino) == payload_identity:
                payload_synced = True
            real_fsync(fd)

        with mock.patch.object(module.os, "fsync", record_sync):
            with redirect_stdout(io.StringIO()):
                module.seal_stage(
                    argparse.Namespace(
                        capability_file=str(self.capability_file),
                        manifest=str(self.manifest_file),
                        output_name="dcentos-v1-release",
                    )
                )
        self.assertTrue(payload_synced)

    @unittest.skipUnless(os.name == "posix", "POSIX temp durability regression")
    def test_failed_seal_durably_unlinks_temporary_descriptor(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        module = self.load_module()
        real_fsync = module.os.fsync
        stage_identity = stage.lstat().st_dev, stage.lstat().st_ino
        stage_syncs = 0

        def record_sync(fd: int) -> None:
            nonlocal stage_syncs
            metadata = os.fstat(fd)
            if (metadata.st_dev, metadata.st_ino) == stage_identity:
                stage_syncs += 1
            real_fsync(fd)

        with mock.patch.object(
            module.os, "replace", side_effect=OSError("injected replace failure")
        ):
            with mock.patch.object(module.os, "fsync", record_sync):
                with self.assertRaisesRegex(OSError, "replace failure"):
                    with redirect_stdout(io.StringIO()):
                        module.seal_stage(
                            argparse.Namespace(
                                capability_file=str(self.capability_file),
                                manifest=str(self.manifest_file),
                                output_name="dcentos-v1-release",
                            )
                        )
        self.assertGreaterEqual(stage_syncs, 1)
        self.assertFalse((stage / module.SEAL_PENDING_NAME).exists())

    @unittest.skipUnless(os.name == "posix", "POSIX seal recovery regression")
    def test_seal_retries_after_partial_deterministic_pending_file(self) -> None:
        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        module = self.load_module()
        pending = stage / module.SEAL_PENDING_NAME
        pending.write_bytes(b"partial-crash-state")
        pending.chmod(0o600)
        with pending.open("rb") as handle:
            os.fsync(handle.fileno())

        with redirect_stdout(io.StringIO()):
            module.seal_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    manifest=str(self.manifest_file),
                    output_name="dcentos-v1-release",
                )
            )
        self.assertFalse(pending.exists())
        descriptor = json.loads((stage / DESCRIPTOR).read_text(encoding="utf-8"))
        self.assertEqual(descriptor["state"], "sealed")

    @unittest.skipUnless(os.name == "posix", "POSIX marker cleanup regression")
    def test_marker_creation_sync_failure_closes_and_unlinks_marker(self) -> None:
        module = self.load_module()
        directory_fd = os.open(self.stage_parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with mock.patch.object(
                module.os, "fsync", side_effect=OSError("injected marker sync failure")
            ):
                with self.assertRaisesRegex(OSError, "marker sync failure"):
                    module.create_posix_marker_file(directory_fd)
        finally:
            os.close(directory_fd)
        self.assertEqual(
            list(self.stage_parent.glob(f"{module.DELETE_MARKER_PREFIX}*")), []
        )

    @unittest.skipUnless(os.name == "posix", "POSIX descriptor leak regression")
    def test_pre_fdopen_failures_close_publication_and_seal_descriptors(self) -> None:
        proc_fds = Path("/proc/self/fd")
        if not proc_fds.is_dir():
            self.skipTest("/proc fd accounting is unavailable")
        module = self.load_module()
        baseline = len(list(proc_fds.iterdir()))
        output = self.root / "failed-publication.json"
        with mock.patch.object(
            module.os, "fchmod", side_effect=OSError("injected publication fchmod")
        ):
            with self.assertRaisesRegex(OSError, "publication fchmod"):
                module.publish_regular_file_noreplace(output, b"{}\n")
        self.assertFalse(output.exists())
        self.assertEqual(len(list(proc_fds.iterdir())), baseline)

        _, stage = self.create_stage()
        self.populate(stage, {"firmware.img": b"firmware"})
        real_fchmod = module.os.fchmod

        def fail_seal_mode(fd: int, mode: int) -> None:
            if mode == 0o644:
                raise OSError("injected seal fchmod")
            real_fchmod(fd, mode)

        with mock.patch.object(module.os, "fchmod", fail_seal_mode):
            with self.assertRaisesRegex(OSError, "seal fchmod"):
                with redirect_stdout(io.StringIO()):
                    module.seal_stage(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            manifest=str(self.manifest_file),
                            output_name="dcentos-v1-release",
                        )
                    )
        self.assertFalse((stage / module.SEAL_PENDING_NAME).exists())
        self.assertEqual(len(list(proc_fds.iterdir())), baseline)

    def test_manifest_rejects_symlink_payload_and_output(self) -> None:
        for kind in ("payload", "output"):
            with self.subTest(kind=kind):
                _, stage = self.create_stage()
                outside = self.root / f"manifest-outside-{kind}"
                outside.write_bytes(b"outside")
                try:
                    if kind == "payload":
                        os.symlink(outside, stage / "firmware.img")
                    else:
                        (stage / "firmware.img").write_bytes(b"firmware")
                        os.symlink(outside, self.manifest_file)
                except OSError as error:
                    self.skipTest(f"symlink creation unavailable: {error}")
                result = self.manifest_stage()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(outside.read_bytes(), b"outside")
                self.capability_file.unlink()
                if self.manifest_file.is_symlink():
                    self.manifest_file.unlink()

    def test_manifest_rejects_control_and_casefold_colliding_stage_names(self) -> None:
        for names in (("bad\nname",), ("Straße.img", "strasse.img")):
            with self.subTest(names=names):
                _, stage = self.create_stage()
                try:
                    for name in names:
                        (stage / name).write_bytes(b"payload")
                except OSError as error:
                    self.skipTest(f"adversarial filename creation unavailable: {error}")
                result = self.manifest_stage()
                self.assertNotEqual(result.returncode, 0)
                self.capability_file.unlink()

    def test_query_requires_the_exact_document_schema(self) -> None:
        capability, _ = self.create_stage()
        valid = self.run_cli(
            "query", "--field", "stage-path", input_text=json.dumps(capability)
        )
        self.assertEqual(valid.returncode, 0, valid.stderr)
        capability["unexpected"] = True
        invalid = self.run_cli(
            "query", "--field", "stage-path", input_text=json.dumps(capability)
        )
        self.assertNotEqual(invalid.returncode, 0)
        self.assertIn("exact schema", invalid.stderr)

    def test_existing_output_collision_is_no_replace(self) -> None:
        _, stage, _ = self.create_sealed()
        output = self.output_parent / "dcentos-v1-release"
        output.mkdir()
        sentinel = output / "sentinel"
        sentinel.write_bytes(b"prior-release")
        result = self.publish()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already exists", result.stderr)
        self.assertEqual(sentinel.read_bytes(), b"prior-release")
        self.assertTrue(stage.exists())
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(os.lstat(stage).st_mode), 0o700)

    def test_collision_created_at_commit_preserves_both_stage_and_sentinel(self) -> None:
        _, stage, _ = self.create_sealed()
        output = self.output_parent / "dcentos-v1-release"

        def create_collision() -> None:
            output.mkdir()
            (output / "sentinel").write_bytes(b"prior-release")

        module, error = self.publish_in_process_with(
            "_after_handles_opened", create_collision
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertEqual((output / "sentinel").read_bytes(), b"prior-release")
        self.assertTrue(stage.exists())
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(stage.lstat().st_mode), 0o700)

    def test_output_parent_replacement_cannot_redirect_publication(self) -> None:
        _, stage, _ = self.create_sealed()
        displaced = self.root / "published-displaced"
        replacement_created = False

        def replace_parent() -> None:
            nonlocal replacement_created
            self.output_parent.rename(displaced)
            self.output_parent.mkdir()
            replacement_created = True

        module, error = self.publish_in_process_with(
            "_after_handles_opened", replace_parent
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse(
            (self.output_parent / "dcentos-v1-release").exists()
        )
        self.assertFalse((displaced / "dcentos-v1-release").exists())
        self.assertTrue(stage.exists())
        # Windows pins parents with no FILE_SHARE_DELETE, so the attempted
        # replacement itself is expected to fail before creating a substitute.
        if os.name == "nt":
            self.assertFalse(replacement_created)

    def test_payload_mutation_after_preflight_is_quarantined_not_reported(self) -> None:
        _, stage, _ = self.create_sealed()
        payload = stage / "firmware.img"

        def mutate_verified_payload() -> None:
            payload.write_bytes(b"TAMPERED")  # Same length as b"firmware".

        module, error = self.publish_in_process_with(
            "_after_handles_opened", mutate_verified_payload
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        output = self.output_parent / "dcentos-v1-release"
        self.assertFalse(output.exists())
        quarantines = self.quarantines()
        self.assertEqual(len(quarantines), 1)
        self.assertEqual((quarantines[0] / "firmware.img").read_bytes(), b"TAMPERED")
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(quarantines[0].lstat().st_mode), 0o700)

    def test_post_commit_fault_quarantines_exact_promoted_directory(self) -> None:
        _, _, payloads = self.create_sealed()

        def fail_after_commit() -> None:
            raise OSError("injected post-commit failure")

        module, error = self.publish_in_process_with("_after_commit", fail_after_commit)
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        quarantines = self.quarantines()
        self.assertEqual(len(quarantines), 1)
        for name, content in payloads.items():
            self.assertEqual((quarantines[0] / name).read_bytes(), content)

    def test_precommit_directory_sync_fault_retains_private_stage(self) -> None:
        _, stage, _ = self.create_sealed()
        calls = 0

        def faulting_sync(_handle: int) -> None:
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("injected precommit directory sync failure")

        option = (
            "_flush_windows_handle" if os.name == "nt" else "_sync_fd"
        )
        module, error = self.publish_in_process_with_options(
            {option: faulting_sync}
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertTrue(stage.exists())
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(stage.lstat().st_mode), 0o700)

    def test_postcommit_directory_sync_fault_quarantines_publication(self) -> None:
        self.create_sealed()
        calls = 0
        fail_at = 5

        def faulting_sync(_handle: int) -> None:
            nonlocal calls
            calls += 1
            if calls == fail_at:
                raise OSError("injected postcommit directory sync failure")

        option = (
            "_flush_windows_handle" if os.name == "nt" else "_sync_fd"
        )
        module, error = self.publish_in_process_with_options(
            {option: faulting_sync}
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertEqual(len(self.quarantines()), 1)

    def test_persistent_postcommit_sync_fault_still_quarantines_namespace(self) -> None:
        self.create_sealed()
        calls = 0
        fail_at = 5

        def faulting_sync(_handle: int) -> None:
            nonlocal calls
            calls += 1
            if calls >= fail_at:
                raise OSError("injected persistent postcommit sync failure")

        option = "_flush_windows_handle" if os.name == "nt" else "_sync_fd"
        module, error = self.publish_in_process_with_options(
            {option: faulting_sync}
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertIn("cleanup debt=", str(error))
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertEqual(len(self.quarantines()), 1)

    @unittest.skipUnless(os.name == "posix", "Linux descriptor-bound regression")
    def test_verifier_reads_promoted_fd_not_visible_path_clone(self) -> None:
        self.create_sealed()
        module = self.load_module()
        real_verify = module.verify_exact_stage
        output = self.output_parent / "dcentos-v1-release"
        displaced = self.root / "promoted-real"
        swapped = False

        def swap_during_verify(stage, descriptor, **kwargs):
            nonlocal swapped
            if kwargs.get("directory_fd") is not None and not swapped:
                swapped = True
                output.rename(displaced)
                shutil.copytree(displaced, output)
                (displaced / "firmware.img").write_bytes(b"TAMPERED")
            return real_verify(stage, descriptor, **kwargs)

        caught: BaseException | None = None
        with mock.patch.object(module, "verify_exact_stage", swap_during_verify):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as error:
                caught = error
        self.assertTrue(swapped)
        self.assertIsInstance(caught, module.ReleaseSetError)
        self.assertEqual((output / "firmware.img").read_bytes(), b"firmware")
        self.assertEqual((displaced / "firmware.img").read_bytes(), b"TAMPERED")
        self.assertEqual(stat.S_IMODE(displaced.lstat().st_mode), 0o700)

    @unittest.skipUnless(os.name == "posix", "Linux namespace-race regression")
    def test_postcommit_output_substitution_preserves_foreign_destination(self) -> None:
        self.create_sealed()
        output = self.output_parent / "dcentos-v1-release"
        displaced = self.root / "promoted-displaced"

        def substitute_output() -> None:
            output.rename(displaced)
            output.mkdir()
            (output / "sentinel").write_bytes(b"foreign")

        module, error = self.publish_in_process_with(
            "_after_commit", substitute_output
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertEqual((output / "sentinel").read_bytes(), b"foreign")
        self.assertEqual(stat.S_IMODE(displaced.lstat().st_mode), 0o700)

    @unittest.skipUnless(os.name == "posix", "Linux quarantine-race regression")
    def test_quarantine_name_swap_restores_foreign_official_destination(self) -> None:
        self.create_sealed()
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_rename = primitive_globals["_linux_rename_noreplace"]
        output = self.output_parent / "dcentos-v1-release"
        displaced = self.root / "exact-publication-displaced"
        calls = 0

        def racing_rename(*args):
            nonlocal calls
            calls += 1
            if calls == 2:
                output.rename(displaced)
                output.mkdir()
                (output / "sentinel").write_bytes(b"foreign")
            return real_rename(*args)

        def fail_after_commit() -> None:
            raise OSError("force quarantine")

        caught: BaseException | None = None
        with mock.patch.dict(
            primitive_globals, {"_linux_rename_noreplace": racing_rename}
        ):
            with mock.patch.object(
                module,
                "atomic_publish_directory",
                lambda *args, **kwargs: primitive_globals[
                    "atomic_publish_directory"
                ](*args, _after_commit=fail_after_commit, **kwargs),
            ):
                try:
                    with redirect_stdout(io.StringIO()):
                        module.publish(
                            argparse.Namespace(
                                capability_file=str(self.capability_file),
                                output_parent=str(self.output_parent),
                            )
                        )
                except BaseException as error:
                    caught = error
        self.assertIsInstance(caught, module.ReleaseSetError)
        self.assertEqual((output / "sentinel").read_bytes(), b"foreign")
        self.assertEqual(stat.S_IMODE(displaced.lstat().st_mode), 0o700)
        self.assertIn("raced-destination-restored", str(caught))

    @unittest.skipUnless(os.name == "posix", "Linux quarantine-race regression")
    def test_quarantine_race_keeps_exact_promoted_inode_officially_absent(self) -> None:
        _, stage, _ = self.create_sealed()
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_rename = primitive_globals["_linux_rename_noreplace"]
        output = self.output_parent / "dcentos-v1-release"
        intended_displaced = self.root / "intended-before-source-race"
        foreign_displaced = self.root / "foreign-after-source-race"
        calls = 0

        def racing_rename(*args):
            nonlocal calls
            calls += 1
            if calls == 1:
                stage.rename(intended_displaced)
                stage.mkdir(mode=0o700)
                (stage / "foreign").write_bytes(b"untrusted")
            elif calls == 2:
                output.rename(foreign_displaced)
                intended_displaced.rename(output)
            return real_rename(*args)

        caught: BaseException | None = None
        with mock.patch.dict(
            primitive_globals, {"_linux_rename_noreplace": racing_rename}
        ):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as error:
                caught = error
        self.assertIsInstance(caught, module.ReleaseSetError)
        self.assertFalse(output.exists())
        quarantines = list(
            self.output_parent.glob(".dcentos-v1-release.publication-failed.*")
        )
        self.assertEqual(len(quarantines), 1)
        self.assertEqual((foreign_displaced / "foreign").read_bytes(), b"untrusted")
        self.assertEqual(stat.S_IMODE(quarantines[0].lstat().st_mode), 0o700)
        self.assertIn("raced-exact-publication-quarantined", str(caught))

    @unittest.skipUnless(os.name == "posix", "Linux source-race regression")
    def test_source_swap_at_rename_cannot_leave_untrusted_official_name(self) -> None:
        _, stage, _ = self.create_sealed()
        displaced = self.root / "intended-stage-displaced"
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_rename = primitive_globals["_linux_rename_noreplace"]
        raced = False

        def racing_rename(*args):
            nonlocal raced
            if not raced:
                raced = True
                stage.rename(displaced)
                stage.mkdir(mode=0o700)
                (stage / "foreign").write_bytes(b"untrusted")
            return real_rename(*args)

        error: BaseException | None = None
        with mock.patch.dict(
            primitive_globals, {"_linux_rename_noreplace": racing_rename}
        ):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as caught:
                error = caught
        self.assertTrue(raced)
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        quarantines = list(
            self.output_parent.glob(".dcentos-v1-release.publication-failed.*")
        )
        self.assertEqual(len(quarantines), 1)
        self.assertEqual((quarantines[0] / "foreign").read_bytes(), b"untrusted")
        self.assertEqual(stat.S_IMODE(displaced.lstat().st_mode), 0o700)

    @unittest.skipUnless(os.name == "posix", "Linux failed-rename regression")
    def test_failed_rename_never_quarantines_prior_destination_when_source_vanishes(
        self,
    ) -> None:
        _, stage, _ = self.create_sealed()
        displaced = self.root / "stage-removed-before-failed-rename"
        output = self.output_parent / "dcentos-v1-release"
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_rename = primitive_globals["_linux_rename_noreplace"]
        raced = False

        def racing_failed_rename(*args):
            nonlocal raced
            if not raced:
                raced = True
                output.mkdir()
                (output / "sentinel").write_bytes(b"prior")
                stage.rename(displaced)
            return real_rename(*args)

        error: BaseException | None = None
        with mock.patch.dict(
            primitive_globals, {"_linux_rename_noreplace": racing_failed_rename}
        ):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as caught:
                error = caught
        self.assertTrue(raced)
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertEqual((output / "sentinel").read_bytes(), b"prior")
        self.assertEqual(
            list(self.output_parent.glob(".dcentos-v1-release.publication-failed.*")),
            [],
        )
        self.assertEqual(stat.S_IMODE(displaced.lstat().st_mode), 0o700)

    def test_exception_after_native_rename_is_reconciled_and_quarantined(self) -> None:
        self.create_sealed()

        def fail_after_native_rename() -> None:
            raise OSError("injected wrapper failure after native rename")

        module, error = self.publish_in_process_with(
            "_after_native_rename", fail_after_native_rename
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertEqual(len(self.quarantines()), 1)

    def test_native_success_does_not_depend_on_reconciliation_path_inspection(
        self,
    ) -> None:
        self.create_sealed()
        module = self.load_module()
        primitive_globals = module.atomic_publish_directory.__globals__
        real_metadata = primitive_globals["_directory_metadata"]
        output = self.output_parent / "dcentos-v1-release"
        native_returned = False

        def fail_after_native() -> None:
            nonlocal native_returned
            native_returned = True
            raise OSError("failure after native rename returned")

        def fault_reconciliation(path, label):
            if native_returned and Path(path) == output:
                raise OSError("reconciliation metadata unavailable")
            return real_metadata(path, label)

        with mock.patch.dict(
            primitive_globals, {"_directory_metadata": fault_reconciliation}
        ):
            published_module, error = self.publish_in_process_with(
                "_after_native_rename", fail_after_native
            )
        self.assertTrue(native_returned)
        self.assertIsInstance(error, published_module.ReleaseSetError)
        self.assertFalse(output.exists())
        self.assertEqual(len(self.quarantines()), 1)

    def test_stage_name_reoccupation_after_native_rename_does_not_hide_commit(
        self,
    ) -> None:
        _, stage, _ = self.create_sealed()

        def reoccupy_then_fail() -> None:
            stage.mkdir(mode=0o700)
            (stage / "sentinel").write_bytes(b"foreign")
            raise OSError("injected failure after stage-name reoccupation")

        module, error = self.publish_in_process_with(
            "_after_native_rename", reoccupy_then_fail
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertEqual(len(self.quarantines()), 1)

    @unittest.skipUnless(os.name == "posix", "POSIX mode rollback regression")
    def test_fchmod_side_effect_then_error_restores_private_stage(self) -> None:
        _, stage, _ = self.create_sealed()
        module = self.load_module()
        primitive_os = module.atomic_publish_directory.__globals__["os"]
        real_fchmod = primitive_os.fchmod
        injected = False

        def side_effect_then_error(fd: int, mode: int) -> None:
            nonlocal injected
            real_fchmod(fd, mode)
            if not injected and stat.S_ISDIR(os.fstat(fd).st_mode) and mode == 0o755:
                injected = True
                raise OSError("injected fchmod completion ambiguity")

        caught: BaseException | None = None
        with mock.patch.object(primitive_os, "fchmod", side_effect_then_error):
            try:
                with redirect_stdout(io.StringIO()):
                    module.publish(
                        argparse.Namespace(
                            capability_file=str(self.capability_file),
                            output_parent=str(self.output_parent),
                        )
                    )
            except BaseException as error:
                caught = error
        self.assertIsInstance(caught, module.ReleaseSetError)
        self.assertTrue(injected)
        self.assertTrue(stage.exists())
        self.assertEqual(stat.S_IMODE(stage.lstat().st_mode), 0o700)

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_signal_before_linearization_refuses_and_retains_stage(self) -> None:
        _, stage, _ = self.create_sealed()

        def deliver_signal() -> None:
            os.kill(os.getpid(), signal.SIGTERM)

        module, error = self.publish_in_process_with(
            "_after_handles_opened", deliver_signal
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertTrue(stage.exists())
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_signal_after_linearization_cannot_revoke_commit(self) -> None:
        self.create_sealed()

        def deliver_signal() -> None:
            os.kill(os.getpid(), signal.SIGTERM)

        _, error = self.publish_in_process_with("_after_commit", deliver_signal)
        self.assertIsNone(error)
        self.assertTrue((self.output_parent / "dcentos-v1-release").is_dir())

    def test_stage_name_substitution_before_commit_is_rejected(self) -> None:
        _, stage, _ = self.create_sealed()
        displaced = self.root / "verified-stage-displaced"
        attacker = stage

        def substitute_stage_name() -> None:
            stage.rename(displaced)
            attacker.mkdir()
            (attacker / "foreign").write_bytes(b"foreign")

        module, error = self.publish_in_process_with(
            "_after_handles_opened", substitute_stage_name
        )
        self.assertIsInstance(error, module.ReleaseSetError)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        if os.name == "posix":
            self.assertEqual((attacker / "foreign").read_bytes(), b"foreign")
            self.assertTrue(displaced.exists())
        else:
            # The open Windows directory handle denies the rename itself.
            self.assertTrue(stage.exists())

    def test_noncanonical_payload_modes_are_normalized_before_visibility(self) -> None:
        _, stage, _ = self.create_sealed()
        if os.name != "posix":
            self.skipTest("POSIX release modes are not Windows ACL semantics")
        (stage / "firmware.img").chmod(0o777)
        (stage / "firmware.img.sig").chmod(0o600)
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.output_parent / "dcentos-v1-release"
        for path in output.iterdir():
            self.assertEqual(stat.S_IMODE(path.lstat().st_mode), 0o644)

    def test_injected_pre_promotion_failure_leaves_no_final_directory(self) -> None:
        _, stage, _ = self.create_sealed()
        env = os.environ.copy()
        env["DCENT_RELEASE_SET_TEST_FAIL_BEFORE_PROMOTION"] = "1"
        result = self.publish(env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected failure", result.stderr)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertTrue(stage.exists())

    def test_publish_commit_survives_closed_result_consumer(self) -> None:
        self.create_sealed()
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "publish",
                    "--capability-file",
                    str(self.capability_file),
                    "--output-parent",
                    str(self.output_parent),
                ],
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
                text=True,
            )
        finally:
            os.close(write_descriptor)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.output_parent / "dcentos-v1-release").is_dir())

    def test_payload_tampering_after_seal_is_rejected(self) -> None:
        _, stage, _ = self.create_sealed()
        (stage / "firmware.img").write_bytes(b"tampered")
        result = self.publish()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("declared size and SHA-256", result.stderr)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())

    def test_missing_and_extra_files_are_rejected(self) -> None:
        for kind in ("missing", "extra"):
            with self.subTest(kind=kind):
                _, stage, _ = self.create_sealed(output_name=f"release-{kind}")
                if kind == "missing":
                    (stage / "firmware.img.sig").unlink()
                else:
                    (stage / "unexpected").write_bytes(b"extra")
                result = self.publish()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("not exact", result.stderr)
                self.capability_file.unlink()

    def test_symlink_and_hardlink_payloads_are_rejected(self) -> None:
        for kind in ("symlink", "hardlink"):
            with self.subTest(kind=kind):
                _, stage = self.create_stage()
                outside = self.root / f"outside-{kind}"
                outside.write_bytes(b"outside")
                payload = stage / "firmware.img"
                try:
                    if kind == "symlink":
                        os.symlink(outside, payload)
                    else:
                        os.link(outside, payload)
                except OSError as error:
                    self.skipTest(f"{kind} creation unavailable: {error}")
                self.manifest_file.write_text(
                    json.dumps(self.manifest_for({"firmware.img": b"outside"})) + "\n",
                    encoding="utf-8",
                )
                result = self.seal(f"release-{kind}")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("single-link non-reparse regular", result.stderr)
                self.capability_file.unlink()

    @unittest.skipUnless(os.name == "posix", "FIFO regression is POSIX-only")
    def test_special_file_is_rejected(self) -> None:
        _, stage = self.create_stage()
        fifo = stage / "firmware.img"
        os.mkfifo(fifo)
        result = self.manifest_stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("regular file", result.stderr)

    @unittest.skipUnless(os.name == "posix", "raced FIFO proof is POSIX-only")
    def test_raced_fifo_open_never_blocks_before_type_revalidation(self) -> None:
        # Simulate a regular-file lstat followed by a FIFO at open time. The
        # child timeout is part of the regression: without O_NONBLOCK this
        # exact race waits forever for a FIFO writer and orphans the release
        # verifier/CI job.
        program = r'''
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
from unittest import mock

spec = importlib.util.spec_from_file_location("release_set_publication", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with tempfile.TemporaryDirectory(prefix="dcent-raced-fifo-") as temporary:
    root = Path(temporary)
    regular = root / "regular"
    regular.write_bytes(b"regular")
    regular_metadata = os.lstat(regular)
    fifo = root / "raced"
    os.mkfifo(fifo)
    with mock.patch.object(module.os, "lstat", return_value=regular_metadata):
        try:
            module.measure_stage_file(fifo)
        except module.ReleaseSetError:
            pass
        else:
            raise AssertionError("raced FIFO was accepted as a regular file")
'''
        result = subprocess.run(
            [sys.executable, "-c", program, str(SCRIPT)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_control_and_nonportable_names_are_rejected(self) -> None:
        for name in ("bad\nname", "bad:name", "trailing.", "../escape"):
            with self.subTest(name=repr(name)):
                _, _ = self.create_stage()
                manifest = {
                    "schema": "dcentos.release-set-files.v1",
                    "files": [{"name": name, "sha256": "0" * 64, "size": 0}],
                }
                self.manifest_file.write_text(json.dumps(manifest) + "\n", encoding="utf-8")
                result = self.seal()
                self.assertNotEqual(result.returncode, 0)
                self.capability_file.unlink()

    def test_casefold_collision_and_unsorted_manifest_are_rejected(self) -> None:
        for names in (("A.img", "a.img"), ("z.img", "a.img")):
            with self.subTest(names=names):
                _, stage = self.create_stage()
                for name in names:
                    (stage / name).write_bytes(b"x")
                entries = [
                    {"name": name, "sha256": hashlib.sha256(b"x").hexdigest(), "size": 1}
                    for name in names
                ]
                self.manifest_file.write_text(
                    json.dumps({"schema": "dcentos.release-set-files.v1", "files": entries}) + "\n",
                    encoding="utf-8",
                )
                result = self.seal()
                self.assertNotEqual(result.returncode, 0)
                self.capability_file.unlink()

    def test_capability_cannot_be_retargeted_to_arbitrary_directory(self) -> None:
        capability, stage = self.create_stage()
        arbitrary = self.root / f".dcent-release-set-stage-{capability['stage_id']}"
        arbitrary.mkdir()
        (arbitrary / "valuable").write_bytes(b"keep")
        capability["stage_parent"] = str(self.root)
        capability["stage_path"] = str(arbitrary)
        capability["stage_dev"] = arbitrary.stat().st_dev
        capability["stage_ino"] = arbitrary.stat().st_ino
        self.capability_file.write_text(json.dumps(capability) + "\n", encoding="utf-8")
        result = self.run_cli("destroy-stage", "--capability-file", str(self.capability_file))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stage", result.stderr)
        self.assertEqual((arbitrary / "valuable").read_bytes(), b"keep")
        self.assertTrue(stage.exists())

    def test_destroy_requires_capability_and_refuses_unsafe_entries(self) -> None:
        _, stage = self.create_stage()
        outside = self.root / "outside-cleanup"
        outside.write_bytes(b"keep")
        link = stage / "link"
        try:
            os.symlink(outside, link)
        except OSError as error:
            self.skipTest(f"symlink creation unavailable: {error}")
        result = self.run_cli("destroy-stage", "--capability-file", str(self.capability_file))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outside.read_bytes(), b"keep")
        self.assertTrue(stage.exists())

    def test_destroy_removes_only_a_valid_owned_stage(self) -> None:
        _, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"scratch")
        result = self.run_cli("destroy-stage", "--capability-file", str(self.capability_file))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(stage.exists())
        self.assertTrue(self.stage_parent.exists())
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_destroy_is_idempotent_after_completed_cleanup(self) -> None:
        _, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"scratch")
        first = self.run_cli(
            "destroy-stage", "--capability-file", str(self.capability_file)
        )
        second = self.run_cli(
            "destroy-stage", "--capability-file", str(self.capability_file)
        )
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertFalse(stage.exists())

    def test_destroy_retirement_interruption_is_retryable(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"scratch")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )

        def interrupt_after_retirement() -> None:
            raise OSError("injected interruption after retirement")

        with self.assertRaisesRegex(OSError, "after retirement"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_retire_rename=interrupt_after_retirement,
                )
            )
        self.assertFalse(stage.exists())
        self.assertEqual((retired / "scratch").read_bytes(), b"scratch")

        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertFalse(stage.exists())
        self.assertFalse(retired.exists())

    def test_destroy_partial_entry_cleanup_retains_descriptor_and_is_retryable(
        self,
    ) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch-a").write_bytes(b"a")
        (stage / "scratch-b").write_bytes(b"b")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        removed: list[str] = []

        def interrupt_after_entry(name: str) -> None:
            removed.append(name)
            raise OSError("injected interruption after entry deletion")

        with self.assertRaisesRegex(OSError, "after entry deletion"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_destroy_entry=interrupt_after_entry,
                )
            )
        self.assertEqual(len(removed), 1)
        self.assertNotEqual(removed[0], DESCRIPTOR)
        self.assertTrue((retired / DESCRIPTOR).is_file())

        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertFalse(stage.exists())
        self.assertFalse(retired.exists())

    def test_destroy_removed_directory_before_parent_sync_is_retryable(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"scratch")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )

        def interrupt_before_parent_sync() -> None:
            raise OSError("injected interruption before parent sync")

        with self.assertRaisesRegex(OSError, "before parent sync"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_destroy_rmdir=interrupt_before_parent_sync,
                )
            )
        self.assertFalse(stage.exists())
        if os.name == "posix":
            self.assertTrue(retired.is_file())
        else:
            self.assertFalse(retired.exists())

        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertFalse(stage.exists())

    @unittest.skipUnless(os.name == "posix", "Linux crash-state regression")
    def test_posix_retirement_exchange_crash_state_is_retryable(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )

        def interrupt_after_exchange() -> None:
            raise OSError("injected loss after retirement exchange")

        with self.assertRaisesRegex(OSError, "retirement exchange"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_retire_exchange=interrupt_after_exchange,
                )
            )
        self.assertTrue(stage.is_file())
        self.assertEqual((retired / "scratch").read_bytes(), b"owned")
        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    @unittest.skipUnless(os.name == "posix", "Linux marker publication regression")
    def test_posix_partial_pending_lifecycle_marker_is_recoverable(self) -> None:
        capability, _ = self.create_stage()
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        pending = retired.with_name(f"{retired.name}.pending")
        pending.write_bytes(b"{")
        pending.chmod(0o600)
        with pending.open("rb") as handle:
            os.fsync(handle.fileno())

        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    @unittest.skipUnless(os.name == "posix", "Linux crash-state regression")
    def test_posix_final_exchange_crash_state_is_retryable(self) -> None:
        capability, stage = self.create_stage()
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        deleting = self.stage_parent / (
            f"{module.DELETING_PREFIX}{capability['stage_id']}"
        )

        def interrupt_after_exchange() -> None:
            raise OSError("injected loss after final exchange")

        with self.assertRaisesRegex(OSError, "final exchange"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_delete_exchange=interrupt_after_exchange,
                )
            )
        self.assertFalse(stage.exists())
        self.assertTrue(retired.is_file())
        self.assertTrue(deleting.is_dir())
        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_destroy_retry_preserves_reoccupied_active_name(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )

        def reoccupy_then_interrupt() -> None:
            stage.mkdir(mode=0o700)
            (stage / "sentinel").write_bytes(b"foreign")
            raise OSError("injected retirement reporting interruption")

        with self.assertRaisesRegex(OSError, "reporting interruption"):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_retire_rename=reoccupy_then_interrupt,
                )
            )
        self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")
        self.assertEqual((retired / "scratch").read_bytes(), b"owned")

        module.destroy_stage(
            argparse.Namespace(capability_file=str(self.capability_file))
        )
        self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")
        self.assertFalse(retired.exists())

    def test_destroy_refuses_foreign_retirement_name_collision(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        retired.mkdir(mode=0o700)
        (retired / "sentinel").write_bytes(b"foreign")

        with self.assertRaises(module.ReleaseSetError):
            module.destroy_stage(
                argparse.Namespace(capability_file=str(self.capability_file))
            )
        self.assertEqual((stage / "scratch").read_bytes(), b"owned")
        self.assertEqual((retired / "sentinel").read_bytes(), b"foreign")

    @unittest.skipUnless(os.name == "nt", "Windows handle-delete regression")
    def test_windows_destroy_unlinks_retired_name_while_shared_handle_is_open(
        self,
    ) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        create_file = kernel32.CreateFileW
        create_file.argtypes = (
            ctypes.c_wchar_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_void_p,
        )
        create_file.restype = ctypes.c_void_p
        close_handle = kernel32.CloseHandle
        close_handle.argtypes = (ctypes.c_void_p,)
        close_handle.restype = ctypes.c_int
        held = ctypes.c_void_p()

        def hold_retired_directory() -> None:
            nonlocal held
            held = create_file(
                str(retired),
                0x80000000,  # GENERIC_READ
                0x00000001 | 0x00000002 | 0x00000004,
                None,
                3,  # OPEN_EXISTING
                0x02000000 | 0x00200000,
                None,
            )
            if held == ctypes.c_void_p(-1).value:
                raise ctypes.WinError(ctypes.get_last_error())

        try:
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _before_destroy_rmdir=hold_retired_directory,
                )
            )
            self.assertFalse(os.path.lexists(retired))
            module.destroy_stage(
                argparse.Namespace(capability_file=str(self.capability_file))
            )
            self.assertFalse(os.path.lexists(stage))
        finally:
            if held and held != ctypes.c_void_p(-1).value:
                close_handle(held)

    @unittest.skipUnless(os.name == "nt", "Windows read-only cleanup regression")
    def test_windows_destroy_removes_ordinary_read_only_file(self) -> None:
        _, stage = self.create_stage()
        readonly = stage / "readonly"
        readonly.write_bytes(b"owned")
        readonly.chmod(stat.S_IREAD)
        result = self.run_cli(
            "destroy-stage", "--capability-file", str(self.capability_file)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(stage.exists())

    def test_destroy_stage_substitution_preserves_both_directories(self) -> None:
        _, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        displaced = self.root / "owned-stage-displaced"
        module = self.load_module()
        real_parse = module.parse_stage_descriptor
        swapped = False

        def parse_then_swap(*args, **kwargs):
            nonlocal swapped
            result = real_parse(*args, **kwargs)
            stage.rename(displaced)
            stage.mkdir(mode=0o700)
            (stage / "sentinel").write_bytes(b"foreign")
            swapped = True
            return result

        caught: BaseException | None = None
        with mock.patch.object(module, "parse_stage_descriptor", parse_then_swap):
            try:
                module.destroy_stage(
                    argparse.Namespace(capability_file=str(self.capability_file))
                )
            except BaseException as error:
                caught = error
        self.assertTrue(swapped)
        self.assertIsInstance(caught, module.ReleaseSetError)
        self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")
        self.assertEqual((displaced / "scratch").read_bytes(), b"owned")

    @unittest.skipUnless(os.name == "posix", "Linux exchange regression")
    def test_posix_retirement_exchange_restores_foreign_substitute(self) -> None:
        _, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        displaced = self.root / "retirement-exact-displaced"
        module = self.load_module()

        def substitute_after_pin() -> None:
            stage.rename(displaced)
            stage.mkdir(mode=0o700)
            (stage / "sentinel").write_bytes(b"foreign")

        with self.assertRaises(module.ReleaseSetError):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _after_retire_opened=substitute_after_pin,
                )
            )
        self.assertEqual((stage / "sentinel").read_bytes(), b"foreign")
        self.assertEqual((displaced / "scratch").read_bytes(), b"owned")
        self.assertEqual(
            list(self.stage_parent.glob(f"{module.DESTROYING_PREFIX}*")), []
        )

    @unittest.skipUnless(os.name == "posix", "Linux exchange regression")
    def test_posix_entry_exchange_never_unlinks_foreign_substitute(self) -> None:
        capability, stage = self.create_stage()
        (stage / "scratch").write_bytes(b"owned")
        displaced = self.root / "cleanup-entry-exact-displaced"
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        substituted = False

        def substitute_entry(name: str) -> None:
            nonlocal substituted
            if name != "scratch" or substituted:
                return
            (retired / name).rename(displaced)
            (retired / name).write_bytes(b"foreign")
            substituted = True

        with self.assertRaises(module.ReleaseSetError):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _before_destroy_entry=substitute_entry,
                )
            )
        self.assertTrue(substituted)
        self.assertEqual(displaced.read_bytes(), b"owned")
        self.assertEqual((retired / "scratch").read_bytes(), b"foreign")

    @unittest.skipUnless(os.name == "posix", "Linux exchange regression")
    def test_posix_final_exchange_never_removes_foreign_directory(self) -> None:
        capability, stage = self.create_stage()
        module = self.load_module()
        retired = self.stage_parent / (
            f"{module.DESTROYING_PREFIX}{capability['stage_id']}"
        )
        displaced = self.root / "cleanup-directory-exact-displaced"

        def substitute_directory() -> None:
            retired.rename(displaced)
            retired.mkdir(mode=0o700)
            (retired / "sentinel").write_bytes(b"foreign")

        with self.assertRaises(module.ReleaseSetError):
            module.destroy_stage(
                argparse.Namespace(
                    capability_file=str(self.capability_file),
                    _before_destroy_rmdir=substitute_directory,
                )
            )
        self.assertTrue(displaced.is_dir())
        self.assertEqual((retired / "sentinel").read_bytes(), b"foreign")
        with self.assertRaises(module.ReleaseSetError):
            module.destroy_stage(
                argparse.Namespace(capability_file=str(self.capability_file))
            )
        self.assertTrue(displaced.is_dir())
        self.assertEqual((retired / "sentinel").read_bytes(), b"foreign")

    @unittest.skipUnless(os.name == "posix", "Linux exchange regression")
    def test_posix_native_exchange_success_survives_wrapper_errors(self) -> None:
        # Retirement, descriptor deletion, and final directory isolation each
        # have one exchange. Every native-success/Python-error boundary
        # must reconcile by inode and continue the exact cleanup.
        for fail_at in range(1, 4):
            with self.subTest(exchange=fail_at):
                _, stage = self.create_stage()
                module = self.load_module()
                real_exchange = module.linux_exchange_paths
                calls = 0

                def exchange_then_error(*args) -> None:
                    nonlocal calls
                    calls += 1
                    real_exchange(*args)
                    if calls == fail_at:
                        raise OSError("injected wrapper error after native exchange")

                with mock.patch.object(
                    module, "linux_exchange_paths", exchange_then_error
                ):
                    module.destroy_stage(
                        argparse.Namespace(
                            capability_file=str(self.capability_file)
                        )
                    )
                self.assertGreaterEqual(calls, fail_at)
                self.assertFalse(stage.exists())
                self.assertEqual(list(self.stage_parent.iterdir()), [])
                self.capability_file.unlink()

    def test_symlinked_output_parent_is_rejected(self) -> None:
        self.create_sealed()
        real = self.root / "real-output"
        real.mkdir()
        linked = self.root / "linked-output"
        try:
            os.symlink(real, linked, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"directory symlink creation unavailable: {error}")
        result = self.run_cli(
            "publish",
            "--capability-file",
            str(self.capability_file),
            "--output-parent",
            str(linked),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((real / "dcentos-v1-release").exists())

    @unittest.skipUnless(os.name == "nt", "NTFS junction regression is Windows-only")
    def test_windows_junction_output_parent_is_rejected(self) -> None:
        self.create_sealed()
        real = self.root / "junction-target"
        real.mkdir()
        junction = self.root / "junction"
        created = subprocess.run(
            ["cmd.exe", "/d", "/c", "mklink", "/J", str(junction), str(real)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if created.returncode != 0:
            self.skipTest(f"junction creation unavailable: {created.stderr}")
        try:
            result = self.run_cli(
                "publish",
                "--capability-file",
                str(self.capability_file),
                "--output-parent",
                str(junction),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((real / "dcentos-v1-release").exists())
        finally:
            os.rmdir(junction)


if __name__ == "__main__":
    unittest.main()
