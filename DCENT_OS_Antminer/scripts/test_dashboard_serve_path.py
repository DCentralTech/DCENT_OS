#!/usr/bin/env python3
"""Regression tests for dashboard HTML serving, gzip, and ETag behavior."""

from __future__ import annotations

import gzip
import hashlib
import http.client
import importlib.util
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Dict, Optional


ROOT = Path(__file__).resolve().parents[1]
SERVER_PATHS = (
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/server.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/server.py",
)
BANNER = b'<script src="/static/diagnostic-banner.js" defer></script>'


def load_module(path: Path):
    name = "dashboard_server_" + "_".join(path.parts[-6:-1])
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def write_dashboard(root: Path, html: bytes, with_sidecars: bool = True) -> Path:
    index = root / "index.html"
    index.write_bytes(html)
    if with_sidecars:
        (root / "index.html.gz").write_bytes(gzip.compress(html, compresslevel=9))
        (root / "index.html.sha256").write_text(
            hashlib.sha256(html).hexdigest() + "\n",
            encoding="utf-8",
        )
    return index


class ServerHarness:
    def __init__(self, module, index: Path, static_dir: Optional[Path] = None):
        self.module = module
        self.index = index
        self.static_dir = static_dir
        self.httpd = None
        self.thread = None

    def __enter__(self):
        self.old_dashboard_index = self.module.DASHBOARD_INDEX
        self.old_static_dir = self.module.STATIC_DIR
        self.module.DASHBOARD_INDEX = self.index
        if self.static_dir is not None:
            self.module.STATIC_DIR = self.static_dir
        self.httpd = self.module.ThreadedHTTPServer(
            ("127.0.0.1", 0),
            self.module.DCENTosHandler,
        )
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, _exc_type, _exc, _tb):
        if self.httpd is not None:
            self.httpd.shutdown()
            self.httpd.server_close()
        if self.thread is not None:
            self.thread.join(timeout=5)
        self.module.DASHBOARD_INDEX = self.old_dashboard_index
        self.module.STATIC_DIR = self.old_static_dir

    @property
    def port(self) -> int:
        return self.httpd.server_address[1]

    def get(self, headers: Optional[Dict[str, str]] = None, path: str = "/"):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        try:
            conn.request("GET", path, headers=headers or {})
            response = conn.getresponse()
            body = response.read()
            return response, body
        finally:
            conn.close()


class DashboardServePathTest(unittest.TestCase):
    def test_gzip_response_has_cache_headers_and_byte_identical_body(self) -> None:
        html = b"<html><body><main>dashboard</main>" + BANNER + b"</body></html>"
        expected_sha = hashlib.sha256(html).hexdigest()
        for path in SERVER_PATHS:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(path)
                index = write_dashboard(Path(tmp), html)
                with ServerHarness(server, index) as harness:
                    response, body = harness.get({"Accept-Encoding": "gzip"})

                self.assertEqual(response.status, 200)
                self.assertEqual(response.getheader("Content-Encoding"), "gzip")
                self.assertEqual(response.getheader("Cache-Control"), "no-cache")
                self.assertEqual(response.getheader("Vary"), "Accept-Encoding")
                self.assertEqual(response.getheader("ETag"), f'"{expected_sha[:16]}-gz"')
                self.assertEqual(gzip.decompress(body), html)

    def test_gzip_q_zero_serves_identity(self) -> None:
        html = b"<html><body><main>dashboard</main>" + BANNER + b"</body></html>"
        expected_sha = hashlib.sha256(html).hexdigest()
        for path in SERVER_PATHS:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(path)
                index = write_dashboard(Path(tmp), html)
                with ServerHarness(server, index) as harness:
                    response, body = harness.get({"Accept-Encoding": "gzip;q=0"})

                self.assertEqual(response.status, 200)
                self.assertIsNone(response.getheader("Content-Encoding"))
                self.assertEqual(response.getheader("ETag"), f'"{expected_sha[:16]}"')
                self.assertEqual(body, html)

    def test_matching_gzip_etag_returns_empty_304(self) -> None:
        html = b"<html><body><main>dashboard</main>" + BANNER + b"</body></html>"
        expected_sha = hashlib.sha256(html).hexdigest()
        for path in SERVER_PATHS:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(path)
                index = write_dashboard(Path(tmp), html)
                etag = f'"{expected_sha[:16]}-gz"'
                with ServerHarness(server, index) as harness:
                    response, body = harness.get({
                        "Accept-Encoding": "gzip",
                        "If-None-Match": etag,
                    })

                self.assertEqual(response.status, 304)
                self.assertEqual(response.getheader("ETag"), etag)
                self.assertEqual(body, b"")

    def test_stale_gzip_sidecar_serves_identity(self) -> None:
        html = b"<html><body><main>dashboard</main>" + BANNER + b"</body></html>"
        expected_sha = hashlib.sha256(html).hexdigest()
        for path in SERVER_PATHS:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(path)
                root = Path(tmp)
                index = write_dashboard(root, html)
                old = index.stat().st_mtime - 60
                os.utime(root / "index.html.gz", (old, old))
                with ServerHarness(server, index) as harness:
                    response, body = harness.get({"Accept-Encoding": "gzip"})

                self.assertEqual(response.status, 200)
                self.assertIsNone(response.getheader("Content-Encoding"))
                self.assertEqual(response.getheader("ETag"), f'"{expected_sha[:16]}"')
                self.assertEqual(body, html)

    def test_legacy_html_is_injected_without_etag(self) -> None:
        html = b"<html><body><main>legacy</main></body></html>"
        for path in SERVER_PATHS:
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(path)
                index = write_dashboard(Path(tmp), html, with_sidecars=False)
                with ServerHarness(server, index) as harness:
                    response, body = harness.get({"Accept-Encoding": "gzip"})

                self.assertEqual(response.status, 200)
                self.assertIsNone(response.getheader("Content-Encoding"))
                self.assertIsNone(response.getheader("ETag"))
                self.assertIn(BANNER, body)

    def test_diagnostic_and_recovery_pages_are_not_gzipped_or_injected(self) -> None:
        html = b"<html><body><main>dashboard</main>" + BANNER + b"</body></html>"
        diagnostic = b"<html><body><main>diagnostic-static</main></body></html>"
        recovery = b"<html><body><main>recovery-static</main></body></html>"

        for server_path in SERVER_PATHS:
            with self.subTest(path=server_path), tempfile.TemporaryDirectory() as tmp:
                server = load_module(server_path)
                root = Path(tmp)
                static = root / "static"
                static.mkdir()
                index = write_dashboard(root, html)
                (static / "diagnostic.html").write_bytes(diagnostic)
                (static / "recovery.html").write_bytes(recovery)

                with ServerHarness(server, index, static_dir=static) as harness:
                    for route, expected in (
                        ("/diagnostic", diagnostic),
                        ("/diagnostic.html", diagnostic),
                        ("/recovery", recovery),
                        ("/recovery.html", recovery),
                    ):
                        response, body = harness.get({"Accept-Encoding": "gzip"}, path=route)

                        self.assertEqual(response.status, 200, route)
                        self.assertIsNone(response.getheader("Content-Encoding"), route)
                        self.assertIsNone(response.getheader("ETag"), route)
                        self.assertEqual(body, expected, route)
                        self.assertNotIn(BANNER, body, route)


if __name__ == "__main__":
    unittest.main()
