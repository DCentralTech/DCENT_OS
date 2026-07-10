#!/usr/bin/env python3
"""Regression tests for dashboard reverse-proxy auth policy."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SERVER_PATHS = (
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/server.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/server.py",
)


def load_module(path: Path):
    spec = importlib.util.spec_from_file_location(f"dashboard_server_{path.parent.parent.name}", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class DashboardProxyPolicyTest(unittest.TestCase):
    def test_proxy_headers_forward_bearer_but_never_trusted_nonce(self) -> None:
        for path in SERVER_PATHS:
            with self.subTest(path=path):
                server = load_module(path)
                headers = {
                    "Authorization": "Bearer test-token",
                    "Accept": "application/json",
                    "Host": "miner.local",
                    server.DASHBOARD_PROXY_HEADER: "attacker-supplied",
                }

                out = server.build_dcentrald_proxy_headers(headers, "192.0.2.10")

                self.assertEqual(out["Authorization"], "Bearer test-token")
                self.assertEqual(out["Accept"], "application/json")
                self.assertEqual(out["Host"], "miner.local")
                self.assertEqual(out["X-Forwarded-Host"], "miner.local")
                self.assertEqual(out["X-Forwarded-For"], "192.0.2.10")
                self.assertNotIn(server.DASHBOARD_PROXY_HEADER, out)

    def test_local_controls_are_open_only_on_non_release_images(self) -> None:
        for path in SERVER_PATHS:
            with self.subTest(path=path):
                server = load_module(path)
                original_release_image = server.release_image
                try:
                    server.release_image = lambda: False
                    self.assertTrue(server.local_control_authorized({}))
                finally:
                    server.release_image = original_release_image

    def test_release_local_controls_require_valid_bearer(self) -> None:
        for path in SERVER_PATHS:
            with self.subTest(path=path):
                server = load_module(path)
                original_release_image = server.release_image
                original_validate = server.validate_bearer_with_dcentrald
                try:
                    server.release_image = lambda: True
                    server.validate_bearer_with_dcentrald = lambda auth: auth == "Bearer good"

                    self.assertFalse(server.local_control_authorized({}))
                    self.assertFalse(
                        server.local_control_authorized({"Authorization": "Basic bad"})
                    )
                    self.assertFalse(
                        server.local_control_authorized({"Authorization": "Bearer bad"})
                    )
                    self.assertTrue(
                        server.local_control_authorized({"Authorization": "Bearer good"})
                    )
                finally:
                    server.release_image = original_release_image
                    server.validate_bearer_with_dcentrald = original_validate

    def test_release_local_controls_fallback_to_auth_file_when_daemon_down(self) -> None:
        for path in SERVER_PATHS:
            with self.subTest(path=path):
                server = load_module(path)
                original_release_image = server.release_image
                original_validate = server.validate_bearer_with_dcentrald
                original_auth_file = server.AUTH_FILE
                try:
                    with tempfile.TemporaryDirectory() as tmp:
                        token = "good-local-token"
                        read_only = "read-only-token"
                        auth_file = Path(tmp) / "auth.json"
                        auth_file.write_text(
                            json.dumps(
                                {
                                    "version": 2,
                                    "password_hash": "argon2id-placeholder",
                                    "sessions": [
                                        {
                                            "id": "admin",
                                            "token_hash": hashlib.sha256(
                                                token.encode("utf-8")
                                            ).hexdigest(),
                                            "role": "admin",
                                            "expires_at": str(int(time.time()) + 60),
                                        },
                                        {
                                            "id": "read-only",
                                            "token_hash": hashlib.sha256(
                                                read_only.encode("utf-8")
                                            ).hexdigest(),
                                            "role": "read_only",
                                        },
                                    ],
                                }
                            )
                        )
                        server.release_image = lambda: True
                        server.validate_bearer_with_dcentrald = lambda _auth: None
                        server.AUTH_FILE = str(auth_file)

                        self.assertTrue(
                            server.local_control_authorized(
                                {"Authorization": f"Bearer {token}"}
                            )
                        )
                        self.assertFalse(
                            server.local_control_authorized(
                                {"Authorization": f"Bearer {read_only}"}
                            )
                        )
                        self.assertFalse(
                            server.local_control_authorized(
                                {"Authorization": "Bearer wrong"}
                            )
                        )
                finally:
                    server.release_image = original_release_image
                    server.validate_bearer_with_dcentrald = original_validate
                    server.AUTH_FILE = original_auth_file


if __name__ == "__main__":
    unittest.main()
