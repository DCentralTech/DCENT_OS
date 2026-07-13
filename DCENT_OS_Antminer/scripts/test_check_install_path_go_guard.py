#!/usr/bin/env python3
"""Unit tests for check_install_path_go_guard (no CAPSTONE forgery, no NameError)."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_install_path_go_guard.py"


def _load():
    spec = importlib.util.spec_from_file_location("go_guard", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


class GoGuardTests(unittest.TestCase):
    def test_no_claims_pass_with_aggregate_no(self):
        mod = _load()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            final = root / "FINAL_GO_NO_GO.md"
            final.write_text(
                "**Aggregate public install GO:** **NO**\n",
                encoding="utf-8",
            )
            cap = root / "CAPSTONE_EVIDENCE"
            cap.mkdir()
            # Point module paths at temp tree
            mod.CLOSEOUT = root
            mod.FINAL = final
            mod.CAPSTONE = cap
            self.assertEqual(mod.main(), 0)

    def test_yes_without_capstone_fails_without_nameerror(self):
        mod = _load()
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            final = root / "FINAL_GO_NO_GO.md"
            final.write_text(
                "**Aggregate public install GO:** **YES**\n",
                encoding="utf-8",
            )
            cap = root / "CAPSTONE_EVIDENCE"
            cap.mkdir()
            mod.CLOSEOUT = root
            mod.FINAL = final
            mod.CAPSTONE = cap
            # Must not raise NameError (old proof_hits bug)
            rc = mod.main()
            self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
