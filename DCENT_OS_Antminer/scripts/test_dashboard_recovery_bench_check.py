#!/usr/bin/env python3
"""Tests for the operator-run dashboard recovery bench checker."""

from __future__ import annotations

import contextlib
import io
import importlib.util
import json
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/dashboard_recovery_bench_check.py"


def load_module():
    spec = importlib.util.spec_from_file_location("dashboard_recovery_bench_check", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bench = load_module()


class RecoveryHandler(BaseHTTPRequestHandler):
    daemon_down = True

    def log_message(self, _fmt, *_args):
        return

    def send_json(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def send_html(self, body: bytes) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path in {"/", "/diagnostic", "/recovery"}:
            self.send_html(b"<html><body>dashboard recovery fixture</body></html>")
            return

        if self.path == "/api/dashboard/health":
            self.send_json(
                200,
                {
                    "alive": not self.daemon_down,
                    "api_bound": not self.daemon_down,
                    "status": "running" if not self.daemon_down else "dcentrald not running",
                },
            )
            return

        if self.path == "/api/status":
            if self.daemon_down:
                self.send_json(
                    503,
                    {
                        "_disconnected": True,
                        "_error": "dcentrald not reachable on 127.0.0.1:8080",
                    },
                )
            else:
                self.send_json(200, {"schema": "status", "hashrate": 1})
            return

        self.send_response(404)
        self.end_headers()


class ServerHarness:
    def __init__(self, handler_cls):
        self.handler_cls = handler_cls

    def __enter__(self):
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), self.handler_cls)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, _exc_type, _exc, _tb):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=5)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.httpd.server_address[1]}"


class DashboardRecoveryBenchCheckTest(unittest.TestCase):
    def test_passes_daemon_down_contract(self) -> None:
        class DownHandler(RecoveryHandler):
            daemon_down = True

        with ServerHarness(DownHandler) as server:
            report = bench.run_checks(server.url, timeout=5)

        self.assertTrue(report.ok, [check for check in report.checks if not check.ok])
        names = {check.name for check in report.checks}
        self.assertIn("recovery-status", names)
        self.assertIn("api-status-disconnected-json", names)

    def test_fails_daemon_down_contract_when_api_status_is_live(self) -> None:
        class LiveHandler(RecoveryHandler):
            daemon_down = False

        with ServerHarness(LiveHandler) as server:
            report = bench.run_checks(server.url, timeout=5, expect_daemon_down=True)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("api-status-disconnected-code", failures)

    def test_passes_after_start_contract(self) -> None:
        class LiveHandler(RecoveryHandler):
            daemon_down = False

        with ServerHarness(LiveHandler) as server:
            report = bench.run_checks(server.url, timeout=5, expect_daemon_down=False)

        self.assertTrue(report.ok, [check for check in report.checks if not check.ok])
        self.assertIn("api-status-reachable", {check.name for check in report.checks})

    def test_normalizes_plain_host_to_http(self) -> None:
        self.assertEqual(bench.normalize_base_url("miner.local"), "http://miner.local")
        self.assertEqual(
            bench.normalize_base_url("http://miner.local/path"),
            "http://miner.local",
        )

    def test_cli_writes_json_output_file(self) -> None:
        class DownHandler(RecoveryHandler):
            daemon_down = True

        with tempfile.TemporaryDirectory() as tmp, ServerHarness(DownHandler) as server:
            output = Path(tmp) / "recovery-down.json"
            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([server.url, "--timeout", "5", "--json", "--output", str(output)])
            saved = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(code, 0)
        self.assertTrue(saved["ok"])
        self.assertEqual(saved["target"], server.url)
        self.assertIn("checks", saved)


if __name__ == "__main__":
    unittest.main()
