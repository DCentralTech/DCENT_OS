#!/usr/bin/env python3
"""Adversarial tests for release_invocation.py."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_invocation.py")
SPEC = importlib.util.spec_from_file_location("release_invocation", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
ri = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ri)


class ReleaseInvocationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.parent = Path(self.temporary.name) / "invocations"
        self.parent.mkdir()

    def tearDown(self) -> None:
        # Tests deliberately create read-only control records.
        for root, directories, files in os.walk(
            self.temporary.name, topdown=False, followlinks=False
        ):
            for name in files:
                path = Path(root) / name
                try:
                    if not path.is_symlink():
                        os.chmod(path, 0o600)
                except OSError:
                    pass
            for name in directories:
                path = Path(root) / name
                try:
                    if not path.is_symlink():
                        os.chmod(path, 0o700)
                except OSError:
                    pass
        self.temporary.cleanup()

    def create(self, name: str = "release"):
        return ri.create_invocation(self.parent, name)

    def run_cli(
        self, *arguments: str, stdin: bytes = b""
    ) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            input=stdin,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def make_eligible(self, created, reason: str = "external-resources-retained"):
        return ri.mark_gc_eligible(created.stage, created.capability, reason)

    def test_create_is_canonical_private_and_capability_is_outside_stage(self) -> None:
        created = self.create()
        verified = ri.verify_invocation(created.stage)
        self.assertRegex(created.invocation_id, r"^[0-9a-f]{64}$")
        self.assertEqual(
            set(path.name for path in created.stage.iterdir()),
            {
                ri.DESCRIPTOR_NAME,
                ri.STATE_NAME,
            },
        )
        self.assertEqual(created.capability.parent.parent, created.stage.parent)
        self.assertNotEqual(created.capability.parent, created.stage)
        self.assertEqual(
            created.descriptor.read_bytes(),
            ri.canonical_bytes(verified.descriptor),
        )
        self.assertFalse(verified.state["garbage_collection"]["eligible"])
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(created.stage.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(created.descriptor.stat().st_mode), 0o400)
            self.assertEqual(stat.S_IMODE(created.capability.stat().st_mode), 0o400)

    def test_same_logical_name_allocates_unique_ids_stages_and_resources(self) -> None:
        first = self.create("nightly")
        second = self.create("nightly")
        self.assertNotEqual(first.invocation_id, second.invocation_id)
        self.assertNotEqual(first.stage, second.stage)
        first_names = set(ri._flatten_resources(first.resources).values())
        second_names = set(ri._flatten_resources(second.resources).values())
        self.assertEqual(len(first_names), 5)
        self.assertEqual(len(second_names), 5)
        self.assertTrue(first_names.isdisjoint(second_names))
        for value in first_names:
            self.assertIn(first.invocation_id, value)
            self.assertRegex(value, r"^[a-z0-9][a-z0-9_.-]+$")

    def test_invalid_or_control_bearing_names_are_rejected(self) -> None:
        for name in ("", "Upper", "two words", "../escape", "bad\nname", "a" * 33):
            with self.subTest(name=repr(name)):
                with self.assertRaises(ri.InvocationError):
                    self.create(name)

    def test_query_result_and_verified_query_are_strict(self) -> None:
        created = self.create()
        result = ri.canonical_bytes(created.cli_result())
        for field, expected in (
            ("stage", str(created.stage)),
            ("capability", str(created.capability)),
            ("cargo_volume", created.resources["docker_volumes"]["cargo"]),
            ("output_stage_name", created.resources["output_stage_name"]),
        ):
            queried = self.run_cli("query-result", "--field", field, stdin=result)
            self.assertEqual(queried.returncode, 0, queried.stderr.decode())
            self.assertEqual(
                queried.stdout.decode().replace("\r\n", "\n"), expected + "\n"
            )
            live = self.run_cli("query", "--field", field, str(created.stage))
            self.assertEqual(live.returncode, 0, live.stderr.decode())
            self.assertEqual(
                live.stdout.decode().replace("\r\n", "\n"), expected + "\n"
            )

        noncanonical = json.dumps(created.cli_result()).encode("ascii")
        rejected = self.run_cli("query-result", "--field", "stage", stdin=noncanonical)
        self.assertNotEqual(rejected.returncode, 0)
        tampered = created.cli_result()
        tampered["extra"] = "field"
        rejected = self.run_cli(
            "query-result", "--field", "stage", stdin=ri.canonical_bytes(tampered)
        )
        self.assertNotEqual(rejected.returncode, 0)

    def test_gc_eligibility_is_explicit_capability_authorized_and_one_way(self) -> None:
        created = self.create()
        with self.assertRaisesRegex(ri.InvocationError, "not explicitly GC-eligible"):
            ri.destroy_invocation(created.stage, created.capability)
        other = self.create("other")
        with self.assertRaises(ri.InvocationError):
            ri.mark_gc_eligible(
                created.stage, other.capability, "external-resources-retained"
            )
        with self.assertRaises(ri.InvocationError):
            ri.mark_gc_eligible(created.stage, created.capability, "bad\nreason")
        updated = self.make_eligible(created)
        self.assertTrue(updated.state["garbage_collection"]["eligible"])
        self.assertEqual(
            updated.state["garbage_collection"]["reason"],
            "external-resources-retained",
        )
        with self.assertRaisesRegex(ri.InvocationError, "already explicitly"):
            self.make_eligible(created)

    def test_destroy_removes_only_exact_local_stage_and_capability(self) -> None:
        created = self.create()
        flattened = ri._flatten_resources(created.resources)
        external = self.parent / flattened["output_stage_name"]
        external.mkdir()
        marker = external / "must-survive"
        marker.write_text("retained", encoding="utf-8")
        self.make_eligible(created)
        ri.destroy_invocation(created.stage, created.capability)
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())
        self.assertEqual(marker.read_text(encoding="utf-8"), "retained")
        self.assertTrue(created.capability.parent.is_dir())

    def test_descriptor_state_and_capability_tampering_are_rejected(self) -> None:
        for target in ("descriptor", "state", "capability"):
            with self.subTest(target=target):
                created = self.create(target)
                path = {
                    "descriptor": created.descriptor,
                    "state": created.stage / ri.STATE_NAME,
                    "capability": created.capability,
                }[target]
                os.chmod(path, 0o600)
                value = json.loads(path.read_bytes())
                value["tampered"] = True
                path.write_bytes(ri.canonical_bytes(value))
                os.chmod(path, 0o400)
                with self.assertRaises(ri.InvocationError):
                    if target == "capability":
                        ri.verify_capability(created.stage, created.capability)
                    else:
                        ri.verify_invocation(created.stage)

    def test_symlinked_control_file_and_extra_entry_are_rejected(self) -> None:
        created = self.create("linkfile")
        descriptor = created.descriptor
        raw = descriptor.read_bytes()
        descriptor.unlink()
        outside = self.parent / "outside-descriptor"
        outside.write_bytes(raw)
        try:
            descriptor.symlink_to(outside)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        with self.assertRaises(ri.InvocationError):
            ri.verify_invocation(created.stage)

        second = self.create("extralink")
        extra = second.stage / "extra"
        try:
            extra.symlink_to(outside)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        with self.assertRaises(ri.InvocationError):
            ri.verify_invocation(second.stage)

    def test_hardlinked_descriptor_state_or_capability_is_rejected(self) -> None:
        for target in ("descriptor", "state", "capability"):
            with self.subTest(target=target):
                created = self.create("hard" + target)
                path = {
                    "descriptor": created.descriptor,
                    "state": created.stage / ri.STATE_NAME,
                    "capability": created.capability,
                }[target]
                outside = self.parent / ("outside-" + target)
                try:
                    os.link(path, outside)
                except OSError as error:
                    self.skipTest(f"hardlinks unavailable: {error}")
                with self.assertRaises(ri.InvocationError):
                    if target == "capability":
                        ri.verify_capability(created.stage, created.capability)
                    else:
                        ri.verify_invocation(created.stage)
                outside.unlink()

    @unittest.skipUnless(os.name == "posix", "FIFO proof is POSIX-only")
    def test_special_stage_entry_is_rejected_without_traversal(self) -> None:
        created = self.create("special")
        fifo = created.stage / "unexpected-fifo"
        os.mkfifo(fifo)
        with self.assertRaisesRegex(ri.InvocationError, "special"):
            ri.verify_invocation(created.stage)

    def test_symlinked_stage_or_capability_directory_is_rejected(self) -> None:
        created = self.create("stagelink")
        alias = self.parent / "stage-alias"
        try:
            alias.symlink_to(created.stage, target_is_directory=True)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        with self.assertRaises(ri.InvocationError):
            ri.verify_invocation(alias)

        separate = Path(self.temporary.name) / "separate"
        separate.mkdir()
        outside = Path(self.temporary.name) / "outside-capability"
        outside.mkdir()
        cap_dir = separate / ri.CAPABILITY_DIRECTORY
        try:
            cap_dir.symlink_to(outside, target_is_directory=True)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        with self.assertRaises(ri.InvocationError):
            ri.create_invocation(separate, "blocked")

    def test_wrong_capability_and_forged_stage_cannot_authorize(self) -> None:
        first = self.create("first")
        second = self.create("second")
        with self.assertRaises(ri.InvocationError):
            ri.mark_gc_eligible(first.stage, second.capability, "checked")

        forged = self.parent / first.stage.name.replace("first", "forged", 1)
        shutil.copytree(first.stage, forged)
        if os.name == "posix":
            os.chmod(forged, 0o700)
            for path in forged.iterdir():
                os.chmod(path, 0o400)
        with self.assertRaises(ri.InvocationError):
            ri.verify_invocation(forged)

    def test_source_has_no_time_or_docker_execution_cleanup_path(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("import time", source)
        self.assertNotIn("import datetime", source)
        self.assertNotIn("subprocess", source)
        self.assertNotIn("docker volume rm", source)
        self.assertIn("not explicitly GC-eligible", source)

    def test_cli_lifecycle_and_require_eligible_check(self) -> None:
        created_run = self.run_cli(
            "create", "--stage-parent", str(self.parent), "--name", "candidate"
        )
        self.assertEqual(created_run.returncode, 0, created_run.stderr.decode())
        result = json.loads(created_run.stdout)
        stage = result["stage"]
        capability = result["capability"]
        refused = self.run_cli("verify", "--require-gc-eligible", stage)
        self.assertNotEqual(refused.returncode, 0)
        marked = self.run_cli(
            "mark-gc-eligible",
            "--capability",
            capability,
            "--reason",
            "external-resources-retained",
            stage,
        )
        self.assertEqual(marked.returncode, 0, marked.stderr.decode())
        checked = self.run_cli("verify", "--require-gc-eligible", stage)
        self.assertEqual(checked.returncode, 0, checked.stderr.decode())
        destroyed = self.run_cli("destroy", "--capability", capability, stage)
        self.assertEqual(destroyed.returncode, 0, destroyed.stderr.decode())
        self.assertFalse(Path(stage).exists())
        self.assertFalse(Path(capability).exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
