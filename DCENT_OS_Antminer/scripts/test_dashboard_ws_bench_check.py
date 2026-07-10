#!/usr/bin/env python3
"""Tests for the operator-run dashboard WebSocket bench checker."""

from __future__ import annotations

import contextlib
import io
import importlib.util
import json
import socketserver
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from urllib.parse import parse_qs, urlparse


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/dashboard_ws_bench_check.py"


def load_module():
    spec = importlib.util.spec_from_file_location("dashboard_ws_bench_check", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bench = load_module()


def read_headers(request) -> str:
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = request.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
    return data.decode("iso-8859-1")


def server_text_frame(payload: bytes) -> bytes:
    if len(payload) < 126:
        return bytes([0x81, len(payload)]) + payload
    if len(payload) <= 0xFFFF:
        return bytes([0x81, 126]) + len(payload).to_bytes(2, "big") + payload
    return bytes([0x81, 127]) + len(payload).to_bytes(8, "big") + payload


class FakeWebSocketHandler(socketserver.BaseRequestHandler):
    token = "tok-123"
    payload = json.dumps({"type": "stats", "hashrate": 1}).encode("utf-8")

    def handle(self):
        raw = read_headers(self.request)
        lines = raw.split("\r\n")
        first = lines[0].split(" ")
        path = first[1] if len(first) > 1 else "/ws"
        parsed = urlparse(path)
        token = parse_qs(parsed.query).get("token", [None])[0]
        headers = {}
        for line in lines[1:]:
            if ":" in line:
                key, value = line.split(":", 1)
                headers[key.strip().lower()] = value.strip()

        if token != self.token:
            self.request.sendall(
                b"HTTP/1.1 401 Unauthorized\r\n"
                b"Content-Length: 0\r\n"
                b"\r\n"
            )
            return

        accept = bench.expected_accept(headers.get("sec-websocket-key", ""))
        response = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n"
            "\r\n"
        ).encode("ascii")
        self.request.sendall(response + server_text_frame(self.payload))


class ThreadingTcpServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True


class ServerHarness:
    def __enter__(self):
        self.server = ThreadingTcpServer(("127.0.0.1", 0), FakeWebSocketHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, _exc_type, _exc, _tb):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.server.server_address[1]}/ws"


class DashboardWsBenchCheckTest(unittest.TestCase):
    def test_normalizes_bare_host_and_redacts_query_token(self) -> None:
        url = bench.normalize_ws_url("192.0.2.10", token="secret")

        self.assertEqual(url, "ws://192.0.2.10:8080/ws?token=secret")
        self.assertEqual(
            bench.redact_ws_url(url),
            "ws://192.0.2.10:8080/ws?token=%3Credacted%3E",
        )

    def test_passes_when_upgrade_and_frame_arrive(self) -> None:
        with ServerHarness() as server:
            report = bench.run_ws_check(server.url, token=FakeWebSocketHandler.token, timeout=5)

        self.assertTrue(report.ok, [check for check in report.checks if not check.ok])
        self.assertEqual(report.http_status, 101)
        self.assertEqual(report.frames[0].message_type, "stats")
        self.assertIn("ws-frame-received", {check.name for check in report.checks})

    def test_fails_without_valid_token_and_never_reports_raw_token(self) -> None:
        with ServerHarness() as server:
            report = bench.run_ws_check(server.url, token="wrong-token", timeout=5)

        self.assertFalse(report.ok)
        self.assertEqual(report.http_status, 401)
        self.assertNotIn("wrong-token", report.target)
        failures = {check.name for check in report.checks if not check.ok}
        self.assertIn("ws-status-101", failures)

    def test_cli_writes_json_output_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, ServerHarness() as server:
            output = Path(tmp) / "ws.json"
            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([
                    server.url,
                    "--token",
                    FakeWebSocketHandler.token,
                    "--timeout",
                    "5",
                    "--json",
                    "--output",
                    str(output),
                ])
            saved = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(code, 0)
        self.assertTrue(saved["ok"])
        self.assertEqual(saved["http_status"], 101)
        self.assertIn("token=%3Credacted%3E", saved["target"])


if __name__ == "__main__":
    unittest.main()
