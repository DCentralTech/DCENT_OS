#!/usr/bin/env python3
"""Regression tests for warm Buildroot local-source invalidation."""

from __future__ import annotations

import os
import stat
import tempfile
import unittest
from pathlib import Path

from buildroot_local_source_digest import DigestError, digest_tree


class LocalSourceDigestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "external"
        (self.root / "packages" / "example" / "src").mkdir(parents=True)
        self.source = self.root / "packages" / "example" / "src" / "main.c"
        self.source.write_bytes(b"int main(void) { return 0; }\n")
        self.source.chmod(0o644)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_repeated_digest_is_stable(self) -> None:
        first = digest_tree(self.root)
        os.utime(self.source, (1_000_000_000, 1_000_000_000))
        self.assertEqual(first, digest_tree(self.root))

    def test_content_change_invalidates_warm_tree(self) -> None:
        first = digest_tree(self.root)
        self.source.write_bytes(b"int main(void) { return 1; }\n")
        self.assertNotEqual(first, digest_tree(self.root))

    def test_relative_path_change_invalidates_warm_tree(self) -> None:
        first = digest_tree(self.root)
        self.source.rename(self.source.with_name("renamed.c"))
        self.assertNotEqual(first, digest_tree(self.root))

    def test_empty_directory_changes_invalidate_warm_tree(self) -> None:
        first = digest_tree(self.root)
        (self.root / "board" / "empty-overlay-dir").mkdir(parents=True)
        self.assertNotEqual(first, digest_tree(self.root))

    @unittest.skipIf(os.name == "nt", "Windows does not preserve POSIX execute modes")
    def test_output_relevant_mode_change_invalidates_warm_tree(self) -> None:
        first = digest_tree(self.root)
        self.source.chmod(stat.S_IMODE(self.source.stat().st_mode) | 0o111)
        self.assertNotEqual(first, digest_tree(self.root))

    @unittest.skipIf(os.name == "nt", "Windows does not preserve POSIX directory modes")
    def test_directory_mode_change_invalidates_warm_tree(self) -> None:
        directory = self.source.parent
        first = digest_tree(self.root)
        directory.chmod(0o700)
        self.assertNotEqual(first, digest_tree(self.root))

    @unittest.skipIf(os.name == "nt", "ordinary Windows test users may not create symlinks")
    def test_symlinked_source_is_refused(self) -> None:
        link = self.source.with_name("alias.c")
        link.symlink_to(self.source.name)
        with self.assertRaises(DigestError):
            digest_tree(self.root)


if __name__ == "__main__":
    unittest.main()
