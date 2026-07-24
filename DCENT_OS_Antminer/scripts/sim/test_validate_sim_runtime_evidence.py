#!/usr/bin/env python3
"""Adversarial tests for simulator runtime evidence and runner contracts."""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import validate_sim_runtime_evidence as evidence


SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SCRIPT_DIR.parent.parent


class SimRuntimeEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write_json(self, name: str, payload: object) -> Path:
        path = self.root / name
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    @staticmethod
    def status(model: str) -> dict[str, object]:
        chips = evidence.MODEL_GEOMETRY[model][1]
        return {
            "accepted": 1,
            "rejected": 0,
            "chains": [
                {"id": chain_id, "chips": chips, "status": "simulated-ready"}
                for chain_id in range(3)
            ],
            "pool": {"url": "loopback://mock-v1", "status": "connected"},
        }

    def test_every_integrated_model_has_exact_geometry(self) -> None:
        for model, (_, chips) in evidence.MODEL_GEOMETRY.items():
            path = self.write_json(f"{model}.json", self.status(model))
            self.assertIn(f"chips={chips}", evidence.validate_status(model, path))

    def test_geometry_contract_matches_canonical_hal_profiles(self) -> None:
        source = (
            PROJECT_DIR
            / "dcentrald/dcentrald-hal/src/platform/sim/mod.rs"
        ).read_text(encoding="utf-8")
        variants = {
            "s9": "S9",
            "s17": "S17",
            "s17pro": "S17Pro",
            "t17": "T17",
            "s19pro": "S19Pro",
            "s19jpro": "S19jPro",
            "s19xp": "S19Xp",
            "s19kpro": "S19kPro",
            "s21": "S21",
            "s21pro": "S21Pro",
        }
        for model, (chip_id, chips) in evidence.MODEL_GEOMETRY.items():
            variant = variants[model]
            pattern = re.compile(
                rf"(?m)^[^\n]*\b{variant}\b[^\n]*=>\s*"
                rf"\([^\n]*0x{chip_id:04x},\s*Some\({chips}\),"
            )
            self.assertRegex(
                source,
                pattern,
                f"validator geometry for {model} drifted from SimBoardProfile",
            )

    def test_status_rejects_wrong_geometry_and_boolean_share_count(self) -> None:
        payload = self.status("s9")
        payload["chains"][1]["chips"] = 48  # type: ignore[index]
        with self.assertRaises(evidence.EvidenceError):
            evidence.validate_status("s9", self.write_json("wrong-chips.json", payload))
        payload = self.status("s9")
        payload["accepted"] = True
        with self.assertRaises(evidence.EvidenceError):
            evidence.validate_status("s9", self.write_json("bool.json", payload))

    def test_status_rejects_duplicate_keys(self) -> None:
        path = self.root / "duplicate.json"
        path.write_text(
            '{"accepted":1,"accepted":0,"rejected":0,"chains":[],"pool":{}}',
            encoding="utf-8",
        )
        with self.assertRaises(evidence.EvidenceError):
            evidence.validate_status("s9", path)

    @staticmethod
    def rust_mcp() -> dict[str, object]:
        return {
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": evidence.MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "dcentos-dcentrald", "version": "1.0.0"},
                "profile": evidence.MCP_PROFILE_ID,
                "transport": "streamable-http",
                "readOnly": True,
            },
        }

    def test_rust_mcp_requires_a_matching_success_result(self) -> None:
        valid = self.write_json("rust-valid.json", self.rust_mcp())
        self.assertIn("SIM_RUST_MCP_EVIDENCE_OK", evidence.validate_rust_mcp(valid))
        for name, mutation in (
            ("error", lambda d: d.update(error={"code": -1})),
            ("wrong-id", lambda d: d.update(id=2)),
            ("bool-id", lambda d: d.update(id=True)),
            ("writable", lambda d: d["result"].update(readOnly=False)),
            ("wrong-server", lambda d: d["result"]["serverInfo"].update(name="stale")),
        ):
            payload = self.rust_mcp()
            mutation(payload)
            with self.subTest(name=name), self.assertRaises(evidence.EvidenceError):
                evidence.validate_rust_mcp(
                    self.write_json(f"rust-{name}.json", payload)
                )

    def test_rootfs_mcp_requires_exact_protocol_identity(self) -> None:
        payload = {
            "name": "dcentos-mcp",
            "version": "1.0.0",
            "protocol": evidence.MCP_PROTOCOL_VERSION,
            "transport": "streamable-http",
            "profileId": evidence.MCP_PROFILE_ID,
            "tools": 28,
            "resources": 3,
        }
        path = self.write_json("rootfs-valid.json", payload)
        self.assertIn("tools=28", evidence.validate_rootfs_mcp(path))
        payload["tools"] = True
        with self.assertRaises(evidence.EvidenceError):
            evidence.validate_rootfs_mcp(self.write_json("rootfs-bool.json", payload))

    def daemon_log(self, model: str) -> str:
        chip_id, chips = evidence.MODEL_GEOMETRY[model]
        return (
            "\x1b[32mINFO\x1b[0m SIM_HAL_RUNTIME_READY "
            f'model="{model}" chip_id=0x{chip_id:04x} chip_count={chips} '
            "accepted_shares=1\n"
            f'INFO SIM_HAL_RUNTIME_STOPPED model="{model}"\n'
        )

    def test_daemon_log_binds_model_geometry_and_clean_pll(self) -> None:
        path = self.root / "daemon.log"
        path.write_text(self.daemon_log("s19kpro"), encoding="utf-8")
        self.assertIn(
            "chip_id=0x1366 chips=77 pll=clean",
            evidence.validate_daemon_log("s19kpro", path),
        )

    def test_daemon_log_rejects_pll_failures_and_stale_model(self) -> None:
        for failure in ("readback TIMEOUT", "readback MISMATCH"):
            path = self.root / f"{failure.split()[-1].lower()}.log"
            path.write_text(self.daemon_log("s9") + failure, encoding="utf-8")
            with self.subTest(failure=failure), self.assertRaises(evidence.EvidenceError):
                evidence.validate_daemon_log("s9", path)
        stale = self.root / "stale.log"
        stale.write_text(self.daemon_log("s17"), encoding="utf-8")
        with self.assertRaises(evidence.EvidenceError):
            evidence.validate_daemon_log("s17pro", stale)

    def test_runner_sources_pin_isolation_hash_and_offline_builds(self) -> None:
        wsl = (SCRIPT_DIR / "wsl_namespace_sim_hal_runner.sh").read_text(
            encoding="utf-8"
        )
        virtme = (SCRIPT_DIR / "virtme_sim_hal_runner.sh").read_text(
            encoding="utf-8"
        )
        all_models = (SCRIPT_DIR / "wsl_all_model_proof.sh").read_text(
            encoding="utf-8"
        )
        runtime = (SCRIPT_DIR / "sim_hal_runtime_check.sh").read_text(
            encoding="utf-8"
        )
        build = (
            'CARGO_TARGET_DIR="$TARGET_DIR" cargo build --locked --offline '
            '--target "$HOST_TRIPLE" -p dcentrald --features sim-hal'
        )
        self.assertIn(build, wsl)
        self.assertIn(build, virtme)
        self.assertIn(build, all_models)
        self.assertIn("unshare --mount --net", wsl)
        self.assertIn("unshare --net", virtme)
        self.assertIn('bash "$SCRIPT_DIR/wsl_namespace_sim_hal_runner.sh"', all_models)
        for source in (wsl, virtme, all_models):
            self.assertIn("--expected-binary-sha256", source)
            self.assertIn('target/sim-hal-runner', source)
        self.assertIn('test "$actual_sha256" = "$EXPECTED_BINARY_SHA256"', runtime)
        self.assertIn('"$expected_binary_root"*/debug/dcentrald', runtime)
        self.assertIn('mktemp -d "${TMPDIR:-/tmp}/dcent-sim-${MODEL}.XXXXXX"', runtime)
        self.assertIn("validate_sim_runtime_evidence.py", runtime)
        self.assertIn("bounded_stop", runtime)
        self.assertIn('pipeline_status=("${PIPESTATUS[@]}")', wsl)
        self.assertIn('pipeline_status=("${PIPESTATUS[@]}")', virtme)
        self.assertIn('--kill-after=10s 120s', wsl)
        self.assertIn('--kill-after=15s 180s', virtme)

    def test_all_sim_runner_shell_sources_pass_bash_syntax(self) -> None:
        scripts = (
            "scripts/sim/sim_hal_runtime_check.sh",
            "scripts/sim/wsl_namespace_sim_hal_runner.sh",
            "scripts/sim/virtme_sim_hal_runner.sh",
            "scripts/sim/wsl_all_model_proof.sh",
        )
        if os.name == "nt":
            command = ["wsl.exe", "--cd", str(PROJECT_DIR), "--", "bash", "-n", *scripts]
            cwd = None
        else:
            command = ["bash", "-n", *scripts]
            cwd = PROJECT_DIR
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.assertEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
