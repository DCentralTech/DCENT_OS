#!/usr/bin/env python3
"""Adversarial offline tests for the invocation-bound release result stage."""

from __future__ import annotations

import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPT_DIRECTORY = pathlib.Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

import release_invocation as invocation
import release_result_stage as result_stage


class ReleaseResultStageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcentos result stage tests ")
        self.root = pathlib.Path(self.temporary.name)
        self.invocation_parent = self.root / "invocations with spaces"
        self.result_parent = self.root / "private results with spaces"
        self.invocation_parent.mkdir()
        self.result_parent.mkdir()
        self.invocations: list[invocation.CreatedInvocation] = []

    def tearDown(self) -> None:
        # Tests deliberately leave tampered private stages.  TemporaryDirectory
        # owns the containing test sandbox; make POSIX control records writable
        # only for that final test-fixture cleanup.
        if os.name == "posix":
            for path in self.root.rglob("*"):
                try:
                    if not path.is_symlink():
                        os.chmod(path, 0o700 if path.is_dir() else 0o600)
                except OSError:
                    pass
        self.temporary.cleanup()

    def create_invocation(self, name: str = "s9") -> invocation.CreatedInvocation:
        created = invocation.create_invocation(self.invocation_parent, name)
        self.invocations.append(created)
        return created

    def create_stage(
        self, created_invocation: invocation.CreatedInvocation | None = None
    ) -> result_stage.CreatedResultStage:
        owner = created_invocation or self.create_invocation()
        return result_stage.create_result_stage(self.result_parent, owner.stage)

    @staticmethod
    def write(path: pathlib.Path, value: bytes) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)

    def test_nested_payload_with_spaces_seals_verifies_queries_and_cleans_exactly(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(created.result_root / "nested output" / "dcentrald binary", b"ELF\0payload")
        self.write(created.result_root / "manifest pieces" / "one.json", b"{}\n")
        if os.name == "posix":
            os.chmod(created.result_root / "nested output" / "dcentrald binary", 0o755)

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(len(sealed.descriptor["manifest"]["files"]), 2)
        verified = result_stage.verify_result_stage(created.stage, owner.stage)
        self.assertEqual(
            result_stage._stage_query(verified, "files_count"), 2
        )
        self.assertEqual(
            result_stage._stage_query(verified, "result_root"), str(created.result_root)
        )

        create_raw = result_stage.canonical_bytes(created.cli_result())
        parsed = result_stage._validate_create_result(
            result_stage._parse_canonical(create_raw, "test create result")
        )
        self.assertEqual(
            result_stage._result_query(parsed, "stage_id"), created.stage_id
        )

        result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())
        self.assertTrue(owner.stage.exists(), "result cleanup must not consume invocation state")

    def test_two_invocations_are_isolated_and_swaps_are_rejected(self) -> None:
        first_owner = self.create_invocation("s9")
        second_owner = self.create_invocation("s9")
        first = self.create_stage(first_owner)
        second = self.create_stage(second_owner)
        self.assertNotEqual(first.invocation_id, second.invocation_id)
        self.assertNotEqual(first.stage, second.stage)
        self.assertNotEqual(first.result_name, second.result_name)

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(first.stage, second_owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_capability(
                first.stage, second.capability, first_owner.stage
            )
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(
                first.stage, first.capability, second_owner.stage
            )
        self.assertTrue(first.stage.exists())
        self.assertTrue(second.stage.exists())

    def test_tamper_after_seal_is_detected_and_stage_leaks(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "artifact.bin"
        artifact.write_bytes(b"before")
        result_stage.seal_result_stage(created.stage, created.capability, owner.stage)
        artifact.write_bytes(b"after!")

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertTrue(created.stage.exists())
        self.assertEqual(artifact.read_bytes(), b"after!")

    def test_missing_and_extra_entries_after_seal_are_rejected(self) -> None:
        owner = self.create_invocation()
        missing = self.create_stage(owner)
        missing_file = missing.result_root / "required.bin"
        missing_file.write_bytes(b"required")
        result_stage.seal_result_stage(missing.stage, missing.capability, owner.stage)
        missing_file.unlink()
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(missing.stage, owner.stage)

        extra = self.create_stage(owner)
        (extra.result_root / "known.bin").write_bytes(b"known")
        result_stage.seal_result_stage(extra.stage, extra.capability, owner.stage)
        (extra.result_root / "unexpected.bin").write_bytes(b"unexpected")
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(extra.stage, owner.stage)

    def test_symlink_target_is_untouched_and_unsafe_stage_leaks(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        external = self.root / "external important file"
        external.write_bytes(b"do-not-touch")
        link = created.result_root / "linked-output"
        try:
            os.symlink(external, link)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlink creation unavailable: {error}")

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertEqual(external.read_bytes(), b"do-not-touch")
        self.assertTrue(link.is_symlink())

    def test_hardlink_target_is_untouched_and_unsafe_stage_leaks(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        external = self.root / "external-hardlink-target.bin"
        external.write_bytes(b"valuable")
        linked = created.result_root / "hardlinked.bin"
        try:
            os.link(external, linked)
        except OSError as error:
            self.skipTest(f"hardlink creation unavailable: {error}")

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertEqual(external.read_bytes(), b"valuable")
        self.assertTrue(linked.exists())

    @unittest.skipUnless(os.name == "posix" and hasattr(os, "mkfifo"), "FIFO needs POSIX")
    def test_fifo_is_rejected_without_blocking(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        fifo = created.result_root / "build.fifo"
        os.mkfifo(fifo)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertTrue(stat.S_ISFIFO(os.lstat(fifo).st_mode))

    @unittest.skipUnless(os.name == "posix", "control/case collision needs case-sensitive POSIX")
    def test_control_name_and_portable_case_collision_are_rejected(self) -> None:
        owner = self.create_invocation()
        control = self.create_stage(owner)
        (control.result_root / "bad\nname").write_bytes(b"bad")
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(control.stage, owner.stage)

        collision = self.create_stage(owner)
        (collision.result_root / "Artifact.bin").write_bytes(b"one")
        (collision.result_root / "artifact.bin").write_bytes(b"two")
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(collision.stage, owner.stage)

    @unittest.skipUnless(os.name == "nt", "junction is a native Windows boundary")
    def test_windows_junction_is_rejected_and_target_untouched(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        external = self.root / "junction external target"
        external.mkdir()
        marker = external / "keep.txt"
        marker.write_text("keep", encoding="utf-8")
        junction = created.result_root / "junction"
        completed = subprocess.run(
            ["cmd", "/c", "mklink", "/J", str(junction), str(external)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if completed.returncode:
            self.skipTest(f"junction creation unavailable: {completed.stderr.strip()}")
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertEqual(marker.read_text(encoding="utf-8"), "keep")

    def test_building_stage_can_be_destroyed_after_exact_safe_walk(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(created.result_root / "deep" / "more" / "artifact", b"payload")
        result_stage.destroy_result_stage(created.stage, created.capability, owner.stage)
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_create_result_is_exact_and_rejects_extra_fields(self) -> None:
        created = self.create_stage()
        result = created.cli_result()
        self.assertEqual(
            result_stage._validate_create_result(result)["stage"], str(created.stage)
        )
        result["unexpected"] = True
        with self.assertRaises(result_stage.ResultStageError):
            result_stage._validate_create_result(result)


if __name__ == "__main__":
    unittest.main(verbosity=2)
