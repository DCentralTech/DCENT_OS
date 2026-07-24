#!/usr/bin/env python3
"""Adversarial offline tests for the invocation-bound release result stage."""

from __future__ import annotations

import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT_DIRECTORY = pathlib.Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

import atomic_publish_directory  # noqa: E402
import release_invocation as invocation  # noqa: E402
import release_result_stage as result_stage  # noqa: E402
import release_set_publication  # noqa: E402


class ReleaseResultStageTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="dcentos result stage tests "
        )
        self.root = pathlib.Path(self.temporary.name)
        self.invocation_parent = self.root / "invocations with spaces"
        self.result_parent = self.root / "private results with spaces"
        self.invocation_parent.mkdir()
        self.result_parent.mkdir()
        self.invocations: list[invocation.CreatedInvocation] = []
        self.result_output_index = 0

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
        self.result_output_index += 1
        return result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=(
                self.root / f"result-stage-create-{self.result_output_index}.json"
            ),
        )

    @staticmethod
    def write(path: pathlib.Path, value: bytes) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)

    @staticmethod
    def interrupt_windows_readonly_flush(
        created: result_stage.CreatedResultStage,
        artifact: pathlib.Path,
    ) -> int:
        metadata = os.lstat(artifact)
        digest, size = result_stage._hash_file(
            artifact, metadata, "interrupted Windows read-only payload"
        )
        descriptor = result_stage._validate_descriptor(
            result_stage._parse_canonical(
                created.descriptor.read_bytes(), "building descriptor"
            ),
            invocation.verify_invocation(created.invocation_stage),
        )
        intent = result_stage._windows_flush_intent(
            created.stage, descriptor, artifact, metadata, digest, size
        )
        result_stage._write_exclusive(
            created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME,
            result_stage.canonical_bytes(intent),
        )
        result_stage._fsync_directory(created.stage)
        handle, close_handle = release_set_publication.open_pinned_windows_file(
            artifact,
            (metadata.st_dev, metadata.st_ino),
            "interrupted Windows read-only payload",
            write_attributes=True,
        )
        try:
            cleared = intent["attributes"] & ~0x1
            release_set_publication.set_windows_handle_file_attributes(
                handle, cleared or 0x80
            )
        finally:
            close_handle()
        return intent["attributes"]

    def test_nested_payload_with_spaces_seals_verifies_queries_and_cleans_exactly(
        self,
    ) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(
            created.result_root / "nested output" / "dcentrald binary", b"ELF\0payload"
        )
        self.write(created.result_root / "manifest pieces" / "one.json", b"{}\n")
        if os.name == "posix":
            os.chmod(created.result_root / "nested output" / "dcentrald binary", 0o755)

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(len(sealed.descriptor["manifest"]["files"]), 2)
        verified = result_stage.verify_result_stage(created.stage, owner.stage)
        self.assertEqual(result_stage._stage_query(verified, "files_count"), 2)
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

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())
        self.assertTrue(
            owner.stage.exists(), "result cleanup must not consume invocation state"
        )

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

    def test_create_publishes_durable_locator_before_stage_allocation(self) -> None:
        owner = self.create_invocation()
        output = self.root / "durable-result-locator.json"
        stale = output.with_name(f".{output.name}.publication-pending.fixture")
        stale.write_bytes(b"torn publication")
        with mock.patch.object(
            result_stage,
            "_ensure_capability_directory",
            side_effect=OSError("injected post-locator interruption"),
        ):
            with self.assertRaisesRegex(OSError, "post-locator interruption"):
                result_stage.create_result_stage(
                    self.result_parent,
                    owner.stage,
                    result_output=output,
                )

        self.assertFalse(stale.exists())
        result = result_stage._validate_create_result(
            result_stage._parse_canonical(
                output.read_bytes(), "durable creation locator"
            )
        )
        self.assertFalse(pathlib.Path(result["stage"]).exists())
        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        self.assertEqual(recovered.stage, pathlib.Path(result["stage"]))

    def test_create_recovers_exact_stage_after_capability_publication(self) -> None:
        owner = self.create_invocation()
        output = self.root / "capability-prefix-result.json"
        original = result_stage._publish_expected_control
        interrupted = False

        def publish_then_fail(path: pathlib.Path, raw: bytes, label: str) -> None:
            nonlocal interrupted
            original(path, raw, label)
            if label == "result-stage capability" and not interrupted:
                interrupted = True
                raise OSError("injected post-capability interruption")

        with mock.patch.object(
            result_stage, "_publish_expected_control", publish_then_fail
        ):
            with self.assertRaisesRegex(OSError, "post-capability interruption"):
                result_stage.create_result_stage(
                    self.result_parent,
                    owner.stage,
                    result_output=output,
                )

        result = result_stage._validate_create_result(
            result_stage._parse_canonical(output.read_bytes(), "capability locator")
        )
        stage = pathlib.Path(result["stage"])
        self.assertTrue(stage.is_dir())
        self.assertTrue(pathlib.Path(result["capability"]).is_file())
        self.assertEqual(list(stage.iterdir()), [])
        descriptor_pending = stage / (
            f".{result_stage.DESCRIPTOR_NAME}.publication-pending.fixture"
        )
        descriptor_pending.write_bytes(b"torn descriptor publication")
        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        self.assertEqual(recovered.stage, stage)
        self.assertFalse(descriptor_pending.exists())

    def test_create_recovers_a_durable_empty_pre_capability_stage(self) -> None:
        owner = self.create_invocation()
        output = self.root / "empty-stage-prefix-result.json"
        original = result_stage._publish_expected_control

        def interrupt_before_capability(
            path: pathlib.Path, raw: bytes, label: str
        ) -> None:
            if label == "result-stage capability":
                raise OSError("injected pre-capability interruption")
            original(path, raw, label)

        with (
            mock.patch.object(
                result_stage,
                "_publish_expected_control",
                interrupt_before_capability,
            ),
            mock.patch.object(
                result_stage,
                "_rmdir_verified",
                side_effect=OSError("simulated process-loss cleanup"),
            ),
        ):
            with self.assertRaisesRegex(OSError, "pre-capability interruption"):
                result_stage.create_result_stage(
                    self.result_parent,
                    owner.stage,
                    result_output=output,
                )

        result = result_stage._validate_create_result(
            result_stage._parse_canonical(output.read_bytes(), "empty-stage locator")
        )
        stage = pathlib.Path(result["stage"])
        self.assertTrue(stage.is_dir())
        self.assertEqual(list(stage.iterdir()), [])
        self.assertFalse(pathlib.Path(result["capability"]).exists())
        capability_path = pathlib.Path(result["capability"])
        capability_pending = capability_path.with_name(
            f".{capability_path.name}.publication-pending.fixture"
        )
        capability_pending.write_bytes(b"torn capability publication")
        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        self.assertEqual(recovered.stage, stage)
        self.assertFalse(capability_pending.exists())

    def test_partial_create_can_be_destroyed_from_durable_locator(self) -> None:
        owner = self.create_invocation()
        output = self.root / "descriptor-prefix-result.json"
        original = result_stage._publish_expected_control
        interrupted = False

        def publish_then_fail(path: pathlib.Path, raw: bytes, label: str) -> None:
            nonlocal interrupted
            original(path, raw, label)
            if label == "result-stage creation descriptor" and not interrupted:
                interrupted = True
                raise OSError("injected post-descriptor interruption")

        with mock.patch.object(
            result_stage, "_publish_expected_control", publish_then_fail
        ):
            with self.assertRaisesRegex(OSError, "post-descriptor interruption"):
                result_stage.create_result_stage(
                    self.result_parent,
                    owner.stage,
                    result_output=output,
                )

        result = result_stage._validate_create_result(
            result_stage._parse_canonical(output.read_bytes(), "partial locator")
        )
        stage = pathlib.Path(result["stage"])
        capability = pathlib.Path(result["capability"])
        self.assertEqual(
            {entry.name for entry in stage.iterdir()},
            {result_stage.DESCRIPTOR_NAME},
        )
        result_stage.destroy_result_stage(stage, capability, owner.stage)
        self.assertFalse(stage.exists())
        self.assertFalse(capability.exists())

    def test_create_retry_preserves_stage_and_locator_with_existing_payload(
        self,
    ) -> None:
        owner = self.create_invocation()
        output = self.root / "idempotent-create-result.json"
        first = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        output_raw = output.read_bytes()
        (first.result_root / "caller-output.bin").write_bytes(b"retained")

        second = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        self.assertEqual(second.stage, first.stage)
        self.assertEqual(output.read_bytes(), output_raw)
        self.assertEqual(
            (second.result_root / "caller-output.bin").read_bytes(), b"retained"
        )
        alias = output.with_name(f".{output.name}.publication-pending.fixture")
        os.link(output, alias)
        capability_alias = first.capability.with_name(
            f".{first.capability.name}.publication-pending.fixture"
        )
        os.link(first.capability, capability_alias)
        third = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=output,
        )
        self.assertEqual(third.stage, first.stage)
        self.assertFalse(alias.exists())
        self.assertFalse(capability_alias.exists())
        self.assertEqual(os.lstat(output).st_nlink, 1)
        self.assertEqual(os.lstat(first.capability).st_nlink, 1)

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
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertTrue(created.stage.exists())
        self.assertEqual(artifact.read_bytes(), b"after!")

    def test_seal_is_idempotent_for_the_same_durable_payload(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"stable")

        first = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        second = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertEqual(second.descriptor, first.descriptor)
        self.assertFalse((created.stage / result_stage.SEAL_PENDING_NAME).exists())

    def test_seal_recovers_a_durable_pending_descriptor(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"recoverable")

        with mock.patch.object(
            result_stage.os, "replace", side_effect=OSError("injected pre-commit fault")
        ):
            with self.assertRaisesRegex(OSError, "injected pre-commit fault"):
                result_stage.seal_result_stage(
                    created.stage, created.capability, owner.stage
                )

        self.assertTrue((created.stage / result_stage.SEAL_PENDING_NAME).is_file())
        self.assertEqual(
            result_stage.verify_result_stage(
                created.stage, owner.stage, _allow_seal_pending=True
            ).descriptor["state"],
            "building",
        )
        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertFalse((created.stage / result_stage.SEAL_PENDING_NAME).exists())

    def test_seal_replaces_an_incomplete_pending_write(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"recoverable")
        pending = created.stage / result_stage.SEAL_PENDING_NAME
        pending.write_bytes(b'{"partial":')
        if os.name == "posix":
            pending.chmod(0o400)

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertFalse(pending.exists())

    def test_seal_preserves_a_canonical_foreign_pending_record(self) -> None:
        owner = self.create_invocation()
        target = self.create_stage(owner)
        foreign = self.create_stage(owner)
        (foreign.result_root / "artifact.bin").write_bytes(b"foreign")
        result_stage.seal_result_stage(foreign.stage, foreign.capability, owner.stage)
        pending = target.stage / result_stage.SEAL_PENDING_NAME
        pending.write_bytes(foreign.descriptor.read_bytes())
        if os.name == "posix":
            pending.chmod(0o400)

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.seal_result_stage(target.stage, target.capability, owner.stage)
        self.assertEqual(pending.read_bytes(), foreign.descriptor.read_bytes())
        self.assertEqual(
            result_stage._parse_canonical(
                target.descriptor.read_bytes(), "target descriptor"
            )["state"],
            "building",
        )

    def test_seal_recovers_when_replace_committed_before_error(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"committed")
        original_replace = result_stage.os.replace

        def replace_then_fail(*args: object, **kwargs: object) -> None:
            original_replace(*args, **kwargs)
            raise OSError("injected post-commit fault")

        with mock.patch.object(result_stage.os, "replace", replace_then_fail):
            with self.assertRaisesRegex(OSError, "injected post-commit fault"):
                result_stage.seal_result_stage(
                    created.stage, created.capability, owner.stage
                )

        self.assertEqual(
            result_stage.verify_result_stage(created.stage, owner.stage).descriptor[
                "state"
            ],
            "sealed",
        )
        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertEqual(sealed.descriptor["state"], "sealed")

    def test_seal_flushes_files_and_directories_before_descriptor_commit(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(created.result_root / "nested" / "artifact.bin", b"durable")
        events: list[str] = []
        original_hash = result_stage._hash_file
        original_sync = result_stage._sync_payload_directories
        original_replace = result_stage.os.replace

        def observed_hash(*args: object, **kwargs: object) -> tuple[str, int]:
            if kwargs.get("require_flush"):
                events.append("file-flush")
            return original_hash(*args, **kwargs)

        def observed_sync(*args: object, **kwargs: object) -> None:
            original_sync(*args, **kwargs)
            events.append("directory-flush")

        def observed_replace(*args: object, **kwargs: object) -> None:
            events.append("descriptor-commit")
            original_replace(*args, **kwargs)

        with (
            mock.patch.object(result_stage, "_hash_file", observed_hash),
            mock.patch.object(result_stage, "_sync_payload_directories", observed_sync),
            mock.patch.object(result_stage.os, "replace", observed_replace),
        ):
            result_stage.seal_result_stage(
                created.stage, created.capability, owner.stage
            )

        self.assertLess(events.index("file-flush"), events.index("directory-flush"))
        self.assertLess(
            events.index("directory-flush"), events.index("descriptor-commit")
        )

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_seal_durably_flushes_and_preserves_a_read_only_payload(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "read-only artifact.bin"
        artifact.write_bytes(b"durable read-only payload")
        os.chmod(artifact, stat.S_IREAD)
        original_attributes = os.lstat(artifact).st_file_attributes
        original_mode = result_stage._mode_string(os.lstat(artifact))

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(os.lstat(artifact).st_file_attributes, original_attributes)
        self.assertEqual(
            sealed.descriptor["manifest"]["files"][0]["mode"], original_mode
        )
        self.assertFalse(
            (created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME).exists()
        )

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_seal_recovers_an_interrupted_read_only_attribute_transition(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "interrupted read-only artifact.bin"
        artifact.write_bytes(b"recoverable read-only payload")
        os.chmod(artifact, stat.S_IREAD)
        original_attributes = self.interrupt_windows_readonly_flush(created, artifact)
        self.assertFalse(os.lstat(artifact).st_file_attributes & 0x1)

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(os.lstat(artifact).st_file_attributes, original_attributes)
        self.assertFalse(
            (created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME).exists()
        )

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_seal_recovers_after_process_exit_with_read_only_attribute_cleared(
        self,
    ) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "process-exit read-only artifact.bin"
        artifact.write_bytes(b"process-exit read-only payload")
        os.chmod(artifact, stat.S_IREAD)
        original_attributes = os.lstat(artifact).st_file_attributes
        program = "\n".join(
            (
                "import os, pathlib, sys",
                "sys.path.insert(0, sys.argv[1])",
                "import release_result_stage as stage",
                "import release_set_publication as publication",
                "original = publication.set_windows_handle_file_attributes",
                "def crash_after_clear(handle, attributes):",
                "    original(handle, attributes)",
                "    if not attributes & 1:",
                "        os._exit(73)",
                "publication.set_windows_handle_file_attributes = crash_after_clear",
                "stage.seal_result_stage(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]), pathlib.Path(sys.argv[4]))",
            )
        )
        completed = subprocess.run(
            (
                sys.executable,
                "-c",
                program,
                str(SCRIPT_DIRECTORY),
                str(created.stage),
                str(created.capability),
                str(owner.stage),
            ),
            check=False,
        )
        self.assertEqual(completed.returncode, 73)
        self.assertFalse(os.lstat(artifact).st_file_attributes & 0x1)
        self.assertTrue(
            (created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME).is_file()
        )

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(os.lstat(artifact).st_file_attributes, original_attributes)
        self.assertFalse(
            (created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME).exists()
        )

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_sealed_retry_recovers_an_interrupted_read_only_transition(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "sealed retry read-only artifact.bin"
        artifact.write_bytes(b"sealed retry read-only payload")
        os.chmod(artifact, stat.S_IREAD)
        result_stage.seal_result_stage(created.stage, created.capability, owner.stage)
        original_attributes = self.interrupt_windows_readonly_flush(created, artifact)
        self.assertFalse(os.lstat(artifact).st_file_attributes & 0x1)

        sealed = result_stage.seal_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertEqual(sealed.descriptor["state"], "sealed")
        self.assertEqual(os.lstat(artifact).st_file_attributes, original_attributes)
        self.assertFalse(
            (created.stage / result_stage.WINDOWS_FLUSH_PENDING_NAME).exists()
        )

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_destroy_recovers_an_interrupted_read_only_flush_intent(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "abandoned read-only artifact.bin"
        artifact.write_bytes(b"abandoned read-only transition")
        os.chmod(artifact, stat.S_IREAD)
        self.interrupt_windows_readonly_flush(created, artifact)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    @unittest.skipUnless(os.name == "nt", "read-only flushing is a Windows boundary")
    def test_destroy_restores_a_sealed_read_only_transition_before_cleanup(
        self,
    ) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "sealed cleanup read-only artifact.bin"
        artifact.write_bytes(b"sealed cleanup read-only payload")
        os.chmod(artifact, stat.S_IREAD)
        result_stage.seal_result_stage(created.stage, created.capability, owner.stage)
        self.interrupt_windows_readonly_flush(created, artifact)
        self.assertFalse(os.lstat(artifact).st_file_attributes & 0x1)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )

        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    @unittest.skipUnless(os.name == "posix", "pathname rotation test needs POSIX")
    def test_seal_refuses_stage_path_rotation_during_descriptor_commit(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"original")
        displaced = created.stage.with_name(f"{created.stage.name}.displaced")
        original_replace = result_stage.os.replace

        def rotate_then_commit(*args: object, **kwargs: object) -> None:
            original_replace(created.stage, displaced)
            created.stage.mkdir(mode=0o700)
            (created.stage / "attacker-marker").write_bytes(b"untouched")
            original_replace(*args, **kwargs)

        with mock.patch.object(result_stage.os, "replace", rotate_then_commit):
            with self.assertRaisesRegex(
                result_stage.ResultStageError, "changed identity"
            ):
                result_stage.seal_result_stage(
                    created.stage, created.capability, owner.stage
                )

        self.assertEqual((created.stage / "attacker-marker").read_bytes(), b"untouched")
        self.assertEqual(
            result_stage._parse_canonical(
                (displaced / result_stage.DESCRIPTOR_NAME).read_bytes(),
                "displaced descriptor",
            )["state"],
            "sealed",
        )

    @unittest.skipUnless(os.name == "posix", "descriptor rotation test needs POSIX")
    def test_control_read_rejects_pathname_rotation(self) -> None:
        created = self.create_stage()
        descriptor = created.descriptor
        displaced = descriptor.with_name("displaced-result-stage.json")
        raw = descriptor.read_bytes()
        original_read = result_stage.os.read
        rotated = False

        def rotate_after_read(file_descriptor: int, size: int) -> bytes:
            nonlocal rotated
            chunk = original_read(file_descriptor, size)
            if not rotated:
                rotated = True
                descriptor.rename(displaced)
                descriptor.write_bytes(raw)
                descriptor.chmod(0o400)
            return chunk

        with mock.patch.object(result_stage.os, "read", rotate_after_read):
            with self.assertRaisesRegex(
                result_stage.ResultStageError, "changed while|pathname was replaced"
            ):
                result_stage._read_control(descriptor, "rotating descriptor")

    @unittest.skipUnless(
        os.name == "posix" and pathlib.Path("/proc/self/fd").is_dir(),
        "descriptor leak check needs procfs",
    )
    def test_exclusive_write_closes_descriptor_when_fdopen_fails(self) -> None:
        output = self.root / "fdopen-failure.json"
        before = len(list(pathlib.Path("/proc/self/fd").iterdir()))
        with mock.patch.object(
            result_stage.os, "fdopen", side_effect=OSError("injected fdopen fault")
        ):
            with self.assertRaisesRegex(OSError, "injected fdopen fault"):
                result_stage._write_exclusive(output, b"{}\n")
        after = len(list(pathlib.Path("/proc/self/fd").iterdir()))
        self.assertEqual(after, before)
        self.assertFalse(output.exists())

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
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
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
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertEqual(external.read_bytes(), b"valuable")
        self.assertTrue(linked.exists())

    @unittest.skipUnless(sys.platform.startswith("linux"), "mount IDs need Linux")
    def test_bind_mount_target_is_untouched_and_unsafe_stage_leaks(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        external = self.root / "external-mounted-output"
        external.mkdir()
        marker = external / "valuable.bin"
        marker.write_bytes(b"valuable")
        mountpoint = created.result_root / "mounted-output"
        mountpoint.mkdir()
        mounted = subprocess.run(
            ["mount", "--bind", str(external), str(mountpoint)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if mounted.returncode:
            self.skipTest(f"bind mount unavailable: {mounted.stderr.strip()}")
        try:
            with self.assertRaisesRegex(
                result_stage.ResultStageError, "mount boundary"
            ):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )
            self.assertEqual(marker.read_bytes(), b"valuable")
            descriptor = result_stage._parse_canonical(
                created.descriptor.read_bytes(), "mount-boundary descriptor"
            )
            self.assertEqual(descriptor["state"], "building")
        finally:
            subprocess.run(
                ["umount", str(mountpoint)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )

    @unittest.skipUnless(sys.platform.startswith("linux"), "mount IDs need Linux")
    def test_payload_root_bind_mount_is_rejected_before_destruction(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        external = self.root / "external-result-root"
        external.mkdir(mode=0o700)
        marker = external / "valuable.bin"
        marker.write_bytes(b"valuable")
        mounted = subprocess.run(
            ["mount", "--bind", str(external), str(created.result_root)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if mounted.returncode:
            self.skipTest(f"bind mount unavailable: {mounted.stderr.strip()}")
        try:
            with self.assertRaisesRegex(
                result_stage.ResultStageError, "mount boundary"
            ):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )
            self.assertEqual(marker.read_bytes(), b"valuable")
            descriptor = result_stage._parse_canonical(
                created.descriptor.read_bytes(), "root-mount descriptor"
            )
            self.assertEqual(descriptor["state"], "building")
        finally:
            subprocess.run(
                ["umount", str(created.result_root)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )

    @unittest.skipUnless(sys.platform.startswith("linux"), "inode flags need Linux")
    def test_immutable_payload_is_rejected_before_destruction(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "immutable.bin"
        artifact.write_bytes(b"valuable")
        try:
            changed = subprocess.run(
                ["chattr", "+i", str(artifact)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
        except FileNotFoundError:
            self.skipTest("chattr is unavailable")
        if changed.returncode:
            self.skipTest(
                f"immutable inode flags unavailable: {changed.stderr.strip()}"
            )
        try:
            with self.assertRaisesRegex(
                result_stage.ResultStageError, "immutable or append-only"
            ):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )
            descriptor = result_stage._parse_canonical(
                created.descriptor.read_bytes(), "immutable-payload descriptor"
            )
            self.assertEqual(descriptor["state"], "building")
        finally:
            subprocess.run(
                ["chattr", "-i", str(artifact)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )

    @unittest.skipUnless(
        os.name == "posix" and hasattr(os, "mkfifo"), "FIFO needs POSIX"
    )
    def test_fifo_is_rejected_without_blocking(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        fifo = created.result_root / "build.fifo"
        os.mkfifo(fifo)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_result_stage(created.stage, owner.stage)
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertTrue(stat.S_ISFIFO(os.lstat(fifo).st_mode))

    @unittest.skipUnless(
        os.name == "posix", "control/case collision needs case-sensitive POSIX"
    )
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
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertEqual(marker.read_text(encoding="utf-8"), "keep")

    @unittest.skipUnless(
        os.name == "nt", "DACL validation is a native Windows boundary"
    )
    def test_windows_public_capability_directory_is_rejected(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        atomic_publish_directory.set_windows_directory_acl(
            created.capability.parent,
            atomic_publish_directory.WINDOWS_PUBLIC_DIRECTORY_SDDL,
        )
        with self.assertRaises(result_stage.ResultStageError):
            result_stage.verify_capability(
                created.stage, created.capability, owner.stage
            )

    @unittest.skipUnless(os.name == "nt", "DACL normalization is a Windows boundary")
    def test_create_normalizes_an_existing_capability_directory_dacl(self) -> None:
        capability_directory = self.result_parent / result_stage.CAPABILITY_DIRECTORY
        capability_directory.mkdir()
        atomic_publish_directory.set_windows_directory_acl(
            capability_directory,
            atomic_publish_directory.WINDOWS_PUBLIC_DIRECTORY_SDDL,
        )
        created = self.create_stage()
        release_set_publication.require_private_windows_acl(
            created.capability.parent, "normalized result capability directory"
        )

    @unittest.skipUnless(os.name == "nt", "read-only attributes are a Windows boundary")
    def test_destroy_removes_a_read_only_payload_file(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        artifact = created.result_root / "readonly.bin"
        artifact.write_bytes(b"read only")
        os.chmod(artifact, stat.S_IREAD)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    @unittest.skipUnless(os.name == "nt", "read-only attributes are a Windows boundary")
    def test_destroy_removes_a_read_only_payload_directory(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        directory = created.result_root / "readonly-directory"
        directory.mkdir()
        os.chmod(directory, stat.S_IREAD)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_building_stage_can_be_destroyed_after_exact_safe_walk(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(created.result_root / "deep" / "more" / "artifact", b"payload")
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    @unittest.skipUnless(os.name == "posix", "mode normalization needs POSIX")
    def test_destroy_normalizes_a_read_only_payload_tree(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        directory = created.result_root / "readonly-directory"
        directory.mkdir()
        artifact = directory / "artifact.bin"
        artifact.write_bytes(b"readonly tree")
        directory.chmod(0o555)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    @unittest.skipUnless(
        sys.platform.startswith("linux")
        and hasattr(os, "geteuid")
        and os.geteuid() == 0,
        "sticky-owner preflight needs a Linux root test driver",
    )
    def test_sticky_nonowner_removal_is_rejected_before_destruction(self) -> None:
        import pwd

        owner = self.create_invocation()
        created = self.create_stage(owner)
        nobody = pwd.getpwnam("nobody")
        self.root.chmod(0o755)
        for base in (self.invocation_parent, self.result_parent):
            os.chown(base, nobody.pw_uid, nobody.pw_gid)
            for path in base.rglob("*"):
                if not path.is_symlink():
                    os.chown(path, nobody.pw_uid, nobody.pw_gid)

        sticky = created.result_root / "sticky"
        sticky.mkdir()
        sticky.chmod(0o1777)
        protected = sticky / "root-owned.bin"
        protected.write_bytes(b"must remain")
        program = "\n".join(
            (
                "import pathlib, sys",
                "sys.path.insert(0, sys.argv[1])",
                "import release_result_stage as result_stage",
                "result_stage.destroy_result_stage(",
                "    pathlib.Path(sys.argv[2]),",
                "    pathlib.Path(sys.argv[3]),",
                "    pathlib.Path(sys.argv[4]),",
                ")",
            )
        )
        try:
            attempted = subprocess.run(
                [
                    "runuser",
                    "-u",
                    "nobody",
                    "--",
                    "python3",
                    "-c",
                    program,
                    str(SCRIPT_DIRECTORY),
                    str(created.stage),
                    str(created.capability),
                    str(owner.stage),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
        except FileNotFoundError:
            self.skipTest("runuser is unavailable")

        self.assertNotEqual(attempted.returncode, 0)
        self.assertIn("sticky result directory", attempted.stderr)
        descriptor = result_stage._parse_canonical(
            created.descriptor.read_bytes(), "sticky-owner descriptor"
        )
        self.assertEqual(descriptor["state"], "building")
        self.assertEqual(protected.read_bytes(), b"must remain")

    def test_destroy_rejects_a_nonprivate_root_before_committing_its_plan(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        if os.name == "posix":
            created.result_root.chmod(0o755)
        else:
            atomic_publish_directory.set_windows_directory_acl(
                created.result_root,
                atomic_publish_directory.WINDOWS_PUBLIC_DIRECTORY_SDDL,
            )

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        descriptor = result_stage._parse_canonical(
            created.descriptor.read_bytes(), "nonprivate-root descriptor"
        )
        self.assertEqual(descriptor["state"], "building")

    def test_destroy_resumes_after_a_payload_unlink(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        self.write(created.result_root / "nested" / "a.bin", b"one")
        self.write(created.result_root / "nested" / "b.bin", b"two")
        original_unlink = result_stage._unlink_verified
        injected = False

        def unlink_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            nonlocal injected
            original_unlink(path, metadata, label)
            if label.startswith("destroy result file") and not injected:
                injected = True
                raise OSError("injected payload-unlink fault")

        with mock.patch.object(result_stage, "_unlink_verified", unlink_then_fail):
            with self.assertRaisesRegex(OSError, "injected payload-unlink fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        descriptor = result_stage._parse_canonical(
            created.descriptor.read_bytes(), "interrupted destroy descriptor"
        )
        self.assertEqual(descriptor["state"], "destroying")
        self.assertTrue(created.capability.exists())
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )

    def test_destroy_rejects_changed_remaining_payload_after_interruption(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        first = created.result_root / "a.bin"
        second = created.result_root / "b.bin"
        first.write_bytes(b"one")
        second.write_bytes(b"two")
        original_unlink = result_stage._unlink_verified
        injected = False

        def unlink_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            nonlocal injected
            original_unlink(path, metadata, label)
            if label.startswith("destroy result file") and not injected:
                injected = True
                raise OSError("injected payload-unlink fault")

        with mock.patch.object(result_stage, "_unlink_verified", unlink_then_fail):
            with self.assertRaises(OSError):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )
        remaining = first if first.exists() else second
        remaining.unlink()
        remaining.write_bytes(b"tampered")

        with self.assertRaises(result_stage.ResultStageError):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertTrue(created.descriptor.exists())
        self.assertTrue(created.capability.exists())

    def test_destroy_resumes_after_descriptor_unlink(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        original_unlink = result_stage._unlink_verified

        def descriptor_unlink_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            original_unlink(path, metadata, label)
            if label == "destroy result-stage descriptor":
                raise OSError("injected descriptor-unlink fault")

        with mock.patch.object(
            result_stage, "_unlink_verified", descriptor_unlink_then_fail
        ):
            with self.assertRaisesRegex(OSError, "injected descriptor-unlink fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        capability = result_stage._parse_canonical(
            created.capability.read_bytes(), "retired-stage capability"
        )
        retirement = result_stage._retirement_path(created.stage, capability)
        self.assertFalse(created.stage.exists())
        self.assertTrue(retirement.is_dir())
        self.assertEqual(list(retirement.iterdir()), [])
        self.assertTrue(created.capability.exists())
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_destroy_resumes_after_stage_retirement_rename(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        original_retire = result_stage._retire_stage_directory

        def retire_then_fail(
            stage: pathlib.Path,
            retirement: pathlib.Path,
            metadata: os.stat_result,
        ) -> os.stat_result:
            original_retire(stage, retirement, metadata)
            raise OSError("injected post-retirement-rename fault")

        with mock.patch.object(
            result_stage, "_retire_stage_directory", retire_then_fail
        ):
            with self.assertRaisesRegex(OSError, "post-retirement-rename fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        capability = result_stage._parse_canonical(
            created.capability.read_bytes(), "retirement-rename capability"
        )
        retirement = result_stage._retirement_path(created.stage, capability)
        self.assertFalse(created.stage.exists())
        self.assertTrue((retirement / result_stage.DESCRIPTOR_NAME).is_file())
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(retirement.exists())
        self.assertFalse(created.capability.exists())

    def test_create_recovers_after_stage_retirement_rename(self) -> None:
        owner = self.create_invocation()
        result_output = self.root / "retirement-create-recovery.json"
        created = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        original_retire = result_stage._retire_stage_directory

        def retire_then_fail(
            stage: pathlib.Path,
            retirement: pathlib.Path,
            metadata: os.stat_result,
        ) -> os.stat_result:
            original_retire(stage, retirement, metadata)
            raise OSError("injected post-retirement-rename create fault")

        with mock.patch.object(
            result_stage, "_retire_stage_directory", retire_then_fail
        ):
            with self.assertRaisesRegex(OSError, "create fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        capability = result_stage._parse_canonical(
            created.capability.read_bytes(), "retired-stage capability"
        )
        retirement = result_stage._retirement_path(created.stage, capability)
        self.assertFalse(created.stage.exists())
        self.assertTrue(retirement.exists())

        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        self.assertFalse(retirement.exists())
        verified = result_stage.verify_result_stage(recovered.stage, owner.stage)
        self.assertEqual(verified.descriptor["state"], "building")
        result_stage.verify_capability(
            recovered.stage, recovered.capability, owner.stage, verified=verified
        )

    def test_create_repairs_legacy_empty_canonical_poison_during_retirement(
        self,
    ) -> None:
        owner = self.create_invocation()
        result_output = self.root / "poisoned-retirement-create-recovery.json"
        created = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        original_retire = result_stage._retire_stage_directory

        def retire_then_fail(
            stage: pathlib.Path,
            retirement: pathlib.Path,
            metadata: os.stat_result,
        ) -> os.stat_result:
            original_retire(stage, retirement, metadata)
            raise OSError("injected poisoned-retirement fault")

        with mock.patch.object(
            result_stage, "_retire_stage_directory", retire_then_fail
        ):
            with self.assertRaisesRegex(OSError, "poisoned-retirement fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        created.stage.mkdir(mode=0o700)
        result_stage._set_private_directory(created.stage)
        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        verified = result_stage.verify_result_stage(recovered.stage, owner.stage)
        result_stage.verify_capability(
            recovered.stage, recovered.capability, owner.stage, verified=verified
        )

    def test_destroy_resumes_after_stage_retirement(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        original_rmdir = result_stage._rmdir_verified

        def stage_rmdir_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            original_rmdir(path, metadata, label)
            if label == "destroy result stage":
                raise OSError("injected stage-retirement fault")

        with mock.patch.object(result_stage, "_rmdir_verified", stage_rmdir_then_fail):
            with self.assertRaisesRegex(OSError, "injected stage-retirement fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        self.assertFalse(created.stage.exists())
        self.assertTrue(created.capability.exists())
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.capability.exists())

    def test_create_recovers_after_stage_rmdir_before_capability_unlink(self) -> None:
        owner = self.create_invocation()
        result_output = self.root / "post-rmdir-create-recovery.json"
        created = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        original_rmdir = result_stage._rmdir_verified

        def stage_rmdir_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            original_rmdir(path, metadata, label)
            if label == "destroy result stage":
                raise OSError("injected post-rmdir create fault")

        with mock.patch.object(result_stage, "_rmdir_verified", stage_rmdir_then_fail):
            with self.assertRaisesRegex(OSError, "post-rmdir create fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        self.assertFalse(created.stage.exists())
        self.assertTrue(created.capability.exists())
        recovered = result_stage.create_result_stage(
            self.result_parent,
            owner.stage,
            result_output=result_output,
        )
        verified = result_stage.verify_result_stage(recovered.stage, owner.stage)
        self.assertEqual(verified.descriptor["state"], "building")
        result_stage.verify_capability(
            recovered.stage, recovered.capability, owner.stage, verified=verified
        )

    def test_capability_rejects_a_recreated_stage_inode(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        original_rmdir = result_stage._rmdir_verified

        def stage_rmdir_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            original_rmdir(path, metadata, label)
            if label == "destroy result stage":
                raise OSError("injected stage-retirement fault")

        with mock.patch.object(result_stage, "_rmdir_verified", stage_rmdir_then_fail):
            with self.assertRaisesRegex(OSError, "stage-retirement fault"):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        created.stage.mkdir(mode=0o700)
        result_stage._set_private_directory(created.stage)
        os.lstat(created.stage)
        with self.assertRaisesRegex(
            result_stage.ResultStageError,
            "replaced stage|empty canonical result stage",
        ):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )

    def test_legacy_capability_remains_destroy_compatible(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        capability = result_stage._parse_canonical(
            created.capability.read_bytes(), "current capability"
        )
        for field in ("descriptor_sha256", "stage_dev", "stage_ino"):
            capability.pop(field)
        capability["schema"] = result_stage.LEGACY_CAPABILITY_SCHEMA
        metadata = os.lstat(created.capability)
        result_stage._unlink_verified(
            created.capability, metadata, "replace capability with legacy fixture"
        )
        result_stage._write_exclusive(
            created.capability, result_stage.canonical_bytes(capability)
        )
        result_stage._fsync_directory(created.capability.parent)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_destroy_resumes_after_remaining_retirement_prefixes(self) -> None:
        for operation, label in (
            ("rmdir", "destroy result directory nested"),
            ("rmdir", "destroy result payload root"),
            ("unlink", "result-stage capability"),
        ):
            with self.subTest(operation=operation, label=label):
                owner = self.create_invocation()
                created = self.create_stage(owner)
                self.write(created.result_root / "nested" / "artifact.bin", b"x")
                original = (
                    result_stage._rmdir_verified
                    if operation == "rmdir"
                    else result_stage._unlink_verified
                )
                injected = False

                def operation_then_fail(
                    path: pathlib.Path, metadata: os.stat_result, observed: str
                ) -> None:
                    nonlocal injected
                    original(path, metadata, observed)
                    if observed == label and not injected:
                        injected = True
                        raise OSError(f"injected {label} fault")

                with mock.patch.object(
                    result_stage,
                    ("_rmdir_verified" if operation == "rmdir" else "_unlink_verified"),
                    operation_then_fail,
                ):
                    with self.assertRaisesRegex(OSError, "injected"):
                        result_stage.destroy_result_stage(
                            created.stage, created.capability, owner.stage
                        )

                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )
                self.assertFalse(created.stage.exists())
                self.assertFalse(created.capability.exists())

    def test_orphan_capability_and_absent_retry_barriers_are_ordered(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        original_rmdir = result_stage._rmdir_verified

        def retire_stage_then_fail(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            original_rmdir(path, metadata, label)
            if label == "destroy result stage":
                raise OSError("injected stage retirement")

        with mock.patch.object(result_stage, "_rmdir_verified", retire_stage_then_fail):
            with self.assertRaises(OSError):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        events: list[str] = []
        original_sync = result_stage._fsync_directory
        original_unlink = result_stage._unlink_verified

        def observed_sync(path: pathlib.Path) -> None:
            if path == created.stage.parent:
                events.append("stage-parent-sync")
            elif path == created.capability.parent:
                events.append("capability-parent-sync")
            original_sync(path)

        def observed_unlink(
            path: pathlib.Path, metadata: os.stat_result, label: str
        ) -> None:
            if label == "orphaned result-stage capability":
                events.append("capability-unlink")
            original_unlink(path, metadata, label)

        with (
            mock.patch.object(result_stage, "_fsync_directory", observed_sync),
            mock.patch.object(result_stage, "_unlink_verified", observed_unlink),
        ):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        unlink_index = events.index("capability-unlink")
        self.assertLess(events.index("stage-parent-sync"), unlink_index)
        self.assertIn("capability-parent-sync", events[unlink_index + 1 :])

        events.clear()
        with mock.patch.object(result_stage, "_fsync_directory", observed_sync):
            result_stage.destroy_result_stage(
                created.stage, created.capability, owner.stage
            )
        self.assertEqual(events[0], "stage-parent-sync")
        self.assertIn("capability-parent-sync", events[1:])

    def test_destroy_recovers_an_interrupted_destroy_descriptor_commit(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"planned")
        original_replace = result_stage.os.replace

        def replace_then_fail(*args: object, **kwargs: object) -> None:
            original_replace(*args, **kwargs)
            raise OSError("injected destroy-descriptor commit fault")

        with mock.patch.object(result_stage.os, "replace", replace_then_fail):
            with self.assertRaisesRegex(
                OSError, "injected destroy-descriptor commit fault"
            ):
                result_stage.destroy_result_stage(
                    created.stage, created.capability, owner.stage
                )

        descriptor = result_stage._parse_canonical(
            created.descriptor.read_bytes(), "committed destroy descriptor"
        )
        self.assertEqual(descriptor["state"], "destroying")
        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_destroy_replaces_an_incomplete_pending_write(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"planned")
        pending = created.stage / result_stage.DESTROY_PENDING_NAME
        pending.write_bytes(b'{"partial":')
        if os.name == "posix":
            pending.chmod(0o400)

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
        self.assertFalse(created.stage.exists())
        self.assertFalse(created.capability.exists())

    def test_destroy_cleans_an_interrupted_seal_record(self) -> None:
        owner = self.create_invocation()
        created = self.create_stage(owner)
        (created.result_root / "artifact.bin").write_bytes(b"pending seal")
        with mock.patch.object(
            result_stage.os, "replace", side_effect=OSError("injected seal fault")
        ):
            with self.assertRaisesRegex(OSError, "injected seal fault"):
                result_stage.seal_result_stage(
                    created.stage, created.capability, owner.stage
                )
        self.assertTrue((created.stage / result_stage.SEAL_PENDING_NAME).exists())

        result_stage.destroy_result_stage(
            created.stage, created.capability, owner.stage
        )
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
