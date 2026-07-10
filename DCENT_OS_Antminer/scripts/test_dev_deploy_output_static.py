#!/usr/bin/env python3
"""Static checks for dev_deploy.sh evidence-output support."""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/dev_deploy.sh"


class DevDeployOutputStaticTest(unittest.TestCase):
    def test_bash_syntax(self) -> None:
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("bash is not available")
        subprocess.run([bash, "-n", "scripts/dev_deploy.sh"], cwd=ROOT, check=True)

    def test_json_output_file_option_is_wired(self) -> None:
        text = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("JSON_OUTPUT_FILE=\"\"", text)
        self.assertIn("--output)           JSON_OUTPUT_FILE=", text)
        self.assertIn("--output=*)         JSON_OUTPUT_FILE=", text)
        self.assertIn("write_json_payload()", text)
        self.assertIn("write_json_payload \"$DASHBOARD_DEPLOY_JSON\"", text)
        self.assertIn("write_json_payload \"$DEPLOY_JSON\"", text)


if __name__ == "__main__":
    unittest.main()
