#!/usr/bin/env python3
"""Offline contracts for semantic manifest and version admission."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

LIB_DIR = Path(__file__).resolve().parent / "lib"
sys.path.insert(0, str(LIB_DIR))

from sysupgrade_manifest_json import (  # noqa: E402
    AdmissionError,
    admit_manifest,
    compare_versions,
    read_version_file,
)


class ManifestAdmissionTests(unittest.TestCase):
    def write_manifest(self, directory: Path, text: str) -> Path:
        path = directory / "MANIFEST.json"
        path.write_text(text, encoding="utf-8")
        return path

    def test_accepts_escape_free_unique_object(self) -> None:
        with tempfile.TemporaryDirectory() as raw_dir:
            path = self.write_manifest(
                Path(raw_dir),
                json.dumps(
                    {
                        "schema": 1,
                        "version": "0.9.0",
                        "payloads": {
                            "kernel": {
                                "path": "sysupgrade-am1-s9/kernel",
                                "size": 6,
                                "sha256": "0" * 64,
                            }
                        },
                    },
                    indent=2,
                ),
            )
            self.assertEqual(admit_manifest(path)["version"], "0.9.0")

    def test_rejects_literal_decoded_duplicate_at_any_depth(self) -> None:
        fixtures = (
            '{"schema":1,"schema":2}',
            '{"payloads":{"kernel":{"size":6,"size":999}}}',
        )
        with tempfile.TemporaryDirectory() as raw_dir:
            directory = Path(raw_dir)
            for index, fixture in enumerate(fixtures):
                with self.subTest(index=index):
                    path = self.write_manifest(directory, fixture)
                    with self.assertRaisesRegex(AdmissionError, "duplicate decoded"):
                        admit_manifest(path)

    def test_rejects_escaped_keys_and_values(self) -> None:
        fixtures = (
            r'{"schema":1,"sche\u006da":2}',
            r'{"status":"rele\u0061se"}',
            r'{"payloads":{"kernel":{"sha\u003256":"00"}}}',
        )
        with tempfile.TemporaryDirectory() as raw_dir:
            directory = Path(raw_dir)
            for index, fixture in enumerate(fixtures):
                with self.subTest(index=index):
                    path = self.write_manifest(directory, fixture)
                    with self.assertRaisesRegex(AdmissionError, "escape sequences"):
                        admit_manifest(path)

    def test_rejects_noncanonical_or_oversized_structure(self) -> None:
        with tempfile.TemporaryDirectory() as raw_dir:
            directory = Path(raw_dir)
            noncanonical = self.write_manifest(directory, '{"bad key":1}')
            with self.assertRaisesRegex(AdmissionError, "canonical ASCII grammar"):
                admit_manifest(noncanonical)

            nested: object = 0
            for _ in range(34):
                nested = {"node": nested}
            too_deep = self.write_manifest(directory, json.dumps(nested))
            with self.assertRaisesRegex(AdmissionError, "nesting exceeds"):
                admit_manifest(too_deep)


class VersionAdmissionTests(unittest.TestCase):
    def test_comparison_vectors(self) -> None:
        cases = (
            ("1.2", "1.2.0", 0),
            ("v1.2.0+build7", "1.2.0", 0),
            ("1.2.0", "1.2.0-rc1", 1),
            ("1.2.0-rc1", "1.2.0-rc2", -1),
            ("1.2.0-rc10", "1.2.0-rc2", -1),
            ("1.2.0-rc.10", "1.2.0-rc.2", 1),
            ("1.2.0-alpha", "1.2.0-alpha.0", -1),
            ("1.2.0-1", "1.2.0-alpha", -1),
            ("1.2.0-ab", "1.2.0-ba", -1),
            ("9007199254740993.0", "9007199254740992.0", 1),
            ("18446744073709551616.0", "18446744073709551615.0", 1),
            (
                "100000000000000000000000000000000.0",
                "99999999999999999999999999999999.999",
                1,
            ),
            ("1.2.0+build-7", "1.2.0+other", 0),
        )
        for candidate, current, expected in cases:
            with self.subTest(candidate=candidate, current=current):
                self.assertEqual(compare_versions(candidate, current), expected)
                self.assertEqual(compare_versions(current, candidate), -expected)

    def test_rejects_noncanonical_versions(self) -> None:
        invalid = (
            "1",
            "1.2.3.4",
            "01.2.0",
            "1.02.0",
            "1.2.0-",
            "1.2.0-alpha..1",
            "1.2.0-alpha.01",
            "1.2.0+",
            "1.2.0_bad",
            "1.2.0:bad",
            " 1.2.0",
            "1.2.0 ",
            "1.2.0-" + ("a" * 33),
        )
        for value in invalid:
            with self.subTest(value=value):
                with self.assertRaises(AdmissionError):
                    compare_versions(value, "1.2.0")

    def test_version_file_has_exactly_one_canonical_line(self) -> None:
        with tempfile.TemporaryDirectory() as raw_dir:
            path = Path(raw_dir) / "dcentos-version"
            path.write_bytes(b"0.9.0\n")
            self.assertEqual(read_version_file(path), "0.9.0")
            for invalid in (b"0.9.0\r\n", b"0.9.0\n1.0.0\n", b"\n", b" 0.9.0\n"):
                with self.subTest(invalid=invalid):
                    path.write_bytes(invalid)
                    with self.assertRaises(AdmissionError):
                        read_version_file(path)


if __name__ == "__main__":
    unittest.main()
