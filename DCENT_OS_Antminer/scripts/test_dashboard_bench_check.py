#!/usr/bin/env python3
"""Tests for the operator-run dashboard bench checker."""

from __future__ import annotations

import gzip
import hashlib
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
SCRIPT = ROOT / "scripts/dashboard_bench_check.py"


def load_module():
    spec = importlib.util.spec_from_file_location("dashboard_bench_check", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bench = load_module()


class DashboardBenchHandler(BaseHTTPRequestHandler):
    html = (
        b"<html><body><main>dashboard</main>"
        + bench.BANNER
        + b"</body></html>"
    )
    sha = hashlib.sha256(html).hexdigest()
    version_sha = sha

    def log_message(self, _fmt, *_args):
        return

    def do_GET(self):
        if self.path == "/api/dashboard/version":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "version": "test",
                        "sha256": self.version_sha,
                        "built_at": 1,
                        "size_bytes": len(self.html),
                        "path": "/usr/share/dcentos-dashboard/index.html",
                    }
                ).encode("utf-8")
            )
            return

        if self.path != "/":
            self.send_response(404)
            self.end_headers()
            return

        accept_encoding = self.headers.get("Accept-Encoding", "")
        gzip_ok = "gzip" in accept_encoding and "gzip;q=0" not in accept_encoding
        if_match = self.headers.get("If-None-Match")
        etag = f'"{self.sha[:16]}-gz"' if gzip_ok else f'"{self.sha[:16]}"'
        if if_match == etag:
            self.send_response(304)
            self.send_header("ETag", etag)
            self.end_headers()
            return

        self.send_response(200)
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Vary", "Accept-Encoding")
        self.send_header("ETag", etag)
        if gzip_ok:
            body = gzip.compress(self.html, compresslevel=9)
            self.send_header("Content-Encoding", "gzip")
        else:
            body = self.html
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class ServerHarness:
    def __init__(self, handler_cls):
        self.handler_cls = handler_cls
        self.httpd = None
        self.thread = None

    def __enter__(self):
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), self.handler_cls)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, _exc_type, _exc, _tb):
        if self.httpd is not None:
            self.httpd.shutdown()
            self.httpd.server_close()
        if self.thread is not None:
            self.thread.join(timeout=5)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.httpd.server_address[1]}"


class DashboardBenchCheckTest(unittest.TestCase):
    def test_passes_expected_delivery_contract(self) -> None:
        with ServerHarness(DashboardBenchHandler) as server:
            report = bench.run_checks(server.url, timeout=5)

        self.assertTrue(report.ok, [c for c in report.checks if not c.ok])
        self.assertEqual(report.sha256, DashboardBenchHandler.sha)
        self.assertGreater(report.gzip_bytes, 0)
        self.assertEqual(report.identity_bytes, len(DashboardBenchHandler.html))
        self.assertIn("gzip-q0-bytes", {c.name for c in report.checks})
        self.assertIn("etag-304-status", {c.name for c in report.checks})
        self.assertIn("gzip-budget", {c.name for c in report.checks})
        self.assertIn("identity-budget", {c.name for c in report.checks})

    def test_fails_on_version_sha_mismatch(self) -> None:
        class MismatchHandler(DashboardBenchHandler):
            version_sha = "0" * 64

        with ServerHarness(MismatchHandler) as server:
            report = bench.run_checks(server.url, timeout=5)

        failures = {c.name: c.detail for c in report.checks if not c.ok}
        self.assertFalse(report.ok)
        self.assertIn("version-sha", failures)

    def test_fails_when_served_artifact_does_not_match_expected_dist(self) -> None:
        expected = bench.DistExpectations(
            dist_dir="fixture",
            sha256="0" * 64,
            gzip_bytes=1,
            identity_bytes=1,
        )

        with ServerHarness(DashboardBenchHandler) as server:
            report = bench.run_checks(server.url, timeout=5, expected=expected)

        failures = {c.name: c.detail for c in report.checks if not c.ok}
        self.assertFalse(report.ok)
        self.assertIn("expected-artifact-sha", failures)
        self.assertIn("expected-identity-size", failures)
        self.assertIn("expected-gzip-size", failures)

    def test_fails_when_gzip_budget_is_exceeded(self) -> None:
        with ServerHarness(DashboardBenchHandler) as server:
            report = bench.run_checks(server.url, timeout=5, max_gzip_bytes=1)

        failures = {c.name: c.detail for c in report.checks if not c.ok}
        self.assertFalse(report.ok)
        self.assertIn("gzip-budget", failures)

    def test_loads_local_dist_expectations_and_rejects_stale_sidecars(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            dist_dir = Path(tmp)
            html = DashboardBenchHandler.html
            gzip_body = gzip.compress(html, compresslevel=9)
            sha = hashlib.sha256(html).hexdigest()
            (dist_dir / "index.html").write_bytes(html)
            (dist_dir / "index.html.gz").write_bytes(gzip_body)
            (dist_dir / "index.html.sha256").write_text(f"{sha}\n", encoding="utf-8")

            expected = bench.load_dist_expectations(dist_dir)

            self.assertEqual(expected.sha256, sha)
            self.assertEqual(expected.gzip_bytes, len(gzip_body))
            self.assertEqual(expected.identity_bytes, len(html))

            (dist_dir / "index.html.sha256").write_text("0" * 64, encoding="utf-8")
            with self.assertRaises(ValueError):
                bench.load_dist_expectations(dist_dir)

    def test_normalizes_plain_host_to_http(self) -> None:
        self.assertEqual(bench.normalize_base_url("miner.local"), "http://miner.local")
        self.assertEqual(
            bench.normalize_base_url("http://miner.local/path"),
            "http://miner.local",
        )

    def test_cli_writes_json_output_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, ServerHarness(DashboardBenchHandler) as server:
            output = Path(tmp) / "delivery.json"
            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([server.url, "--timeout", "5", "--json", "--output", str(output)])

            saved = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(code, 0)
        self.assertTrue(saved["ok"])
        self.assertEqual(saved["target"], server.url)
        self.assertIn("checks", saved)


if __name__ == "__main__":
    unittest.main()
