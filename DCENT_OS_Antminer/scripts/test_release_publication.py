#!/usr/bin/env python3
"""Adversarial tests for exact no-replace release publication."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_publication.py")


class PublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-publication-")
        self.root = Path(self.temporary.name)
        self.source = self.root / "source.tar"
        self.source.write_bytes(b"release-bytes")
        self.output = self.root / "release.tar"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def run_copy(
        self, output: Path | None = None, expected_sha256: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        command = [
                sys.executable,
                str(SCRIPT),
                "copy",
                "--source",
                str(self.source),
                "--output",
                str(output or self.output),
            ]
        if expected_sha256 is not None:
            command.extend(["--expected-sha256", expected_sha256])
        return subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_copy_publishes_exact_bytes_and_evidence(self) -> None:
        result = self.run_copy()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())
        evidence = json.loads(result.stdout)
        self.assertEqual(evidence["path"], str(self.output))
        self.assertEqual(evidence["size"], len(b"release-bytes"))
        queried = subprocess.run(
            [sys.executable, str(SCRIPT), "query-result", "--field", "sha256"],
            input=result.stdout,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(queried.returncode, 0, queried.stderr)
        self.assertEqual(queried.stdout.strip(), evidence["sha256"])

    def test_expected_closure_digest_mismatch_publishes_nothing(self) -> None:
        result = self.run_copy(expected_sha256="0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signed closure evidence", result.stderr)
        self.assertFalse(self.output.exists())

    def test_existing_regular_symlink_and_directory_destinations_are_rejected(self) -> None:
        outside = self.root / "outside"
        outside.write_bytes(b"outside")
        for kind in ("regular", "symlink", "directory"):
            with self.subTest(kind=kind):
                output = self.root / f"release-{kind}.tar"
                if kind == "regular":
                    output.write_bytes(b"existing")
                elif kind == "directory":
                    output.mkdir()
                else:
                    try:
                        os.symlink(outside, output)
                    except OSError as error:
                        self.skipTest(f"symlink creation unavailable: {error}")
                before = outside.read_bytes()
                result = self.run_copy(output)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(outside.read_bytes(), before)

    def test_symlinked_output_parent_is_rejected(self) -> None:
        real = self.root / "real-output"
        real.mkdir()
        linked = self.root / "linked-output"
        try:
            os.symlink(real, linked, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"symlink creation unavailable: {error}")
        result = self.run_copy(linked / "release.tar")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((real / "release.tar").exists())

    def test_stdin_is_bounded_and_no_replace(self) -> None:
        command = [
            sys.executable,
            str(SCRIPT),
            "stdin",
            "--output",
            str(self.root / "release.txt"),
        ]
        accepted = subprocess.run(
            command,
            input="release metadata\n",
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        duplicate = subprocess.run(
            command,
            input="replacement\n",
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertEqual((self.root / "release.txt").read_text(), "release metadata\n")
        oversized = subprocess.run(
            command[:-1] + [str(self.root / "oversized.txt")],
            input="x" * (1024 * 1024 + 1),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(oversized.returncode, 0)
        self.assertFalse((self.root / "oversized.txt").exists())

    @unittest.skipUnless(os.name == "nt", "NTFS junction regression is Windows-only")
    def test_windows_junction_parent_is_rejected(self) -> None:
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
            result = self.run_copy(junction / "release.tar")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((real / "release.tar").exists())
        finally:
            os.rmdir(junction)


if __name__ == "__main__":
    unittest.main()
