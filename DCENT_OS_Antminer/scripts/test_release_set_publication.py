#!/usr/bin/env python3
"""Adversarial tests for exact atomic release-set publication."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_set_publication.py")
DESCRIPTOR = ".dcent-release-set.json"


class ReleaseSetPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-release-set-")
        self.root = Path(self.temporary.name)
        self.stage_parent = self.root / "stages"
        self.output_parent = self.root / "published"
        self.stage_parent.mkdir()
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
        queried = self.run_cli("query", "--field", "published-path", input_text=result.stdout)
        self.assertEqual(queried.returncode, 0, queried.stderr)
        self.assertEqual(queried.stdout.strip(), str(output))

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

    def test_injected_pre_promotion_failure_leaves_no_final_directory(self) -> None:
        _, stage, _ = self.create_sealed()
        env = os.environ.copy()
        env["DCENT_RELEASE_SET_TEST_FAIL_BEFORE_PROMOTION"] = "1"
        result = self.publish(env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected failure", result.stderr)
        self.assertFalse((self.output_parent / "dcentos-v1-release").exists())
        self.assertTrue(stage.exists())

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
        self.assertIn("stage descriptor", result.stderr)
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
