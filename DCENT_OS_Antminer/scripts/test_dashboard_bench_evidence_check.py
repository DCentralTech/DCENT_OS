#!/usr/bin/env python3
"""Tests for the offline dashboard bench evidence packet validator."""

from __future__ import annotations

import importlib.util
import binascii
import contextlib
import hashlib
import io
import json
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/dashboard_bench_evidence_check.py"


def load_module():
    spec = importlib.util.spec_from_file_location("dashboard_bench_evidence_check", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


bench = load_module()


def png_chunk(kind, payload):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", binascii.crc32(kind + payload) & 0xFFFFFFFF)
    )


def png_bytes(width=640, height=360):
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    row = b"\x00" + (b"\x00\x00\x00" * width)
    body = b"\x89PNG\r\n\x1a\n"
    body += png_chunk(b"IHDR", ihdr)
    body += png_chunk(b"IDAT", zlib.compress(row * height))
    body += png_chunk(b"IEND", b"")
    return body


PNG_BYTES = png_bytes()


def checks(names):
    return [{"name": name, "ok": True, "detail": "pass"} for name in sorted(names)]


def delivery_report():
    return {
        "target": "http://192.0.2.10",
        "ok": True,
        "sha256": "a" * 64,
        "gzip_bytes": 588_245,
        "identity_bytes": 2_335_121,
        "expected": {
            "sha256": "a" * 64,
            "gzip_bytes": 588_245,
            "identity_bytes": 2_335_121,
        },
        "checks": checks(bench.REQUIRED_DELIVERY_CHECKS),
    }


def deploy_report(delivery=None):
    source = delivery or delivery_report()
    return {
        "success": True,
        "pid": None,
        "binary_size": source["identity_bytes"],
        "deploy_time_seconds": 7,
        "api_healthy": True,
        "miner_ip": "192.0.2.10",
        "platform_family": "any",
        "deploy_mode": "dashboard-only",
        "remote_path": bench.DEPLOY_DASHBOARD_REMOTE_PATH,
        "sha256": source["sha256"],
        "message": "Dashboard-only deploy successful",
    }


def websocket_report():
    return {
        "target": "ws://192.0.2.10:8080/ws?token=%3Credacted%3E",
        "ok": True,
        "http_status": 101,
        "frames": [{"opcode": 1, "kind": "text", "bytes": 42, "message_type": "stats"}],
        "checks": checks(bench.REQUIRED_WS_CHECKS),
    }


def recovery_report(required):
    return {"target": "http://192.0.2.10", "ok": True, "checks": checks(required)}


def kiosk_report():
    return {
        "target": "http://192.0.2.10/",
        "verdict": {
            "ok": True,
            "heapGrowthBytes": 1024,
            "maxHeapGrowthBytes": 10 * 1024 * 1024,
            "checks": checks(bench.REQUIRED_KIOSK_CHECKS),
        },
        "samples": [{}, {}, {}],
    }


def manual_evidence():
    return {
        "deploy": {
            "operator_authorized": True,
            "exit_code": 0,
            "uploaded_sidecars": True,
            "no_rust_rebuild": True,
            "no_daemon_restart": True,
            "no_flash_or_reboot": True,
        },
        "browser_transport": {
            "cold_splash_ms": 750,
            "cold_interactive_ms": 2_500,
            "warm_reload_ms": 900,
            "transport_chip_live_after_ws_frame": True,
            "devtools_ws_101": True,
            "rest_polling_inactive_when_live": True,
        },
        "daemon_recovery": {
            "operator_authorized_stop": True,
            "operator_authorized_start": True,
            "transport_downgraded_while_stopped": True,
            "recovers_live_after_frames": True,
            "no_backlog_fx_replay": True,
        },
        "wizard": {
            "operator_prepared_wiped_state": True,
            "setup_state_visible": True,
            "quick_start_reaches_review": True,
            "skipped_optional_backend_calls": False,
            "pre_setup_transport_truthful": True,
        },
        "first_share": {
            "observed_real_pool_accept": True,
            "pool": "public-pool",
            "worker": "bench-worker",
            "accepted_share_timestamp": "2026-07-04T18:00:00Z",
            "ui_event_timestamp": "2026-07-04T18:00:05Z",
            "led_event_timestamp": "2026-07-04T18:00:06Z",
            "ui_event_once": True,
            "led_event_same_share": True,
            "no_replay_from_existing_count": True,
            "achieved_difficulty": 512,
            "achieved_difficulty_source": "live_event",
        },
    }


def valid_bundle():
    evidence_files = {}
    for key in bench.REQUIRED_EVIDENCE_FILES:
        if key in bench.SCREENSHOT_EVIDENCE_FILES:
            extension = "png"
        elif key == "deploy_report":
            extension = "json"
        else:
            extension = "txt"
        evidence_files[key] = f"{key}.{extension}"
    return {
        "delivery_report": "delivery.json",
        "websocket_report": "ws.json",
        "recovery_down_report": "recovery-down.json",
        "recovery_after_start_report": "recovery-after.json",
        "kiosk_report": "kiosk.json",
        "evidence_files": evidence_files,
        "manual": manual_evidence(),
    }


def default_reports():
    return {
        "delivery.json": delivery_report(),
        "ws.json": websocket_report(),
        "recovery-down.json": recovery_report(bench.REQUIRED_RECOVERY_DOWN_CHECKS),
        "recovery-after.json": recovery_report(bench.REQUIRED_RECOVERY_AFTER_START_CHECKS),
        "kiosk.json": kiosk_report(),
    }


class DashboardBenchEvidenceCheckTest(unittest.TestCase):
    def populate_report_files(self, base: Path, reports=None) -> None:
        for relative, report in (reports or default_reports()).items():
            path = base / relative
            path.write_text(json.dumps(report), encoding="utf-8")

    def populate_evidence_files(self, base: Path, bundle, reports=None) -> None:
        reports = reports or default_reports()
        for key, relative in bundle["evidence_files"].items():
            path = base / relative
            if key in bench.SCREENSHOT_EVIDENCE_FILES:
                path.write_bytes(PNG_BYTES)
            elif key == "deploy_report":
                delivery_key = bundle.get("delivery_report")
                delivery_source = reports.get(delivery_key, delivery_report()) if isinstance(delivery_key, str) else delivery_report()
                path.write_text(json.dumps(deploy_report(delivery_source)), encoding="utf-8")
            else:
                path.write_text("evidence\n", encoding="utf-8")

    def write_bundle(self, bundle, reports=None):
        temp = tempfile.TemporaryDirectory()
        reports = reports or default_reports()
        self.populate_report_files(Path(temp.name), reports)
        self.populate_evidence_files(Path(temp.name), bundle, reports=reports)
        path = Path(temp.name) / "bundle.json"
        path.write_text(json.dumps(bundle), encoding="utf-8")
        return temp, path

    def test_accepts_complete_path_evidence_bundle(self) -> None:
        temp, path = self.write_bundle(valid_bundle())
        with temp:
            report = bench.validate_bundle(path)
            self.assertTrue(report.ok, [check for check in report.checks if not check.ok])
            self.assertIn("first_share.led_event_same_share", {check.name for check in report.checks})
            self.assertIn("first_share.ui_timestamp_near_accept", {check.name for check in report.checks})
            self.assertIn("first_share.achieved_difficulty_honest", {check.name for check in report.checks})
            self.assertEqual(
                {proof.name for proof in report.evidence_files},
                {f"attachment.{name}" for name in bench.REQUIRED_EVIDENCE_FILES}
                | {f"report.{name}" for name in bench.REQUIRED_REPORT_SOURCES},
            )
            for proof in report.evidence_files:
                body = Path(proof.path).read_bytes()
                self.assertEqual(proof.bytes, len(body))
                self.assertEqual(proof.sha256, hashlib.sha256(body).hexdigest())

    def test_accepts_report_paths_relative_to_bundle(self) -> None:
        temp = tempfile.TemporaryDirectory()
        with temp:
            base = Path(temp.name)
            self.populate_report_files(base)
            bundle = {
                "delivery_report": "delivery.json",
                "websocket_report": "ws.json",
                "recovery_down_report": "recovery-down.json",
                "recovery_after_start_report": "recovery-after.json",
                "kiosk_report": "kiosk.json",
                "evidence_files": valid_bundle()["evidence_files"],
                "manual": manual_evidence(),
            }
            self.populate_evidence_files(base, bundle)
            bundle_path = base / "bundle.json"
            bundle_path.write_text(json.dumps(bundle), encoding="utf-8")

            report = bench.validate_bundle(bundle_path)

        self.assertTrue(report.ok, [check for check in report.checks if not check.ok])

    def test_fails_missing_or_false_manual_evidence(self) -> None:
        bundle = valid_bundle()
        bundle["manual"]["first_share"]["led_event_same_share"] = False
        temp, path = self.write_bundle(bundle)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("first_share.led_event_same_share", failures)

    def test_fails_when_required_probe_check_is_missing(self) -> None:
        bundle = valid_bundle()
        reports = default_reports()
        reports["ws.json"] = websocket_report()
        reports["ws.json"]["checks"] = checks(bench.REQUIRED_WS_CHECKS - {"ws-frame-received"})
        temp, path = self.write_bundle(bundle)
        with temp:
            (Path(temp.name) / "ws.json").write_text(json.dumps(reports["ws.json"]), encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("websocket-required-checks-present", failures)

    def test_fails_when_probe_report_is_inline_not_hash_bound(self) -> None:
        bundle = valid_bundle()
        bundle["delivery_report"] = delivery_report()
        temp, path = self.write_bundle(bundle)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("report-source.delivery_report.path", failures)

    def test_fails_missing_probe_report_without_throwing(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            (Path(temp.name) / "ws.json").unlink()
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("report-source.websocket_report.read", failures)
        self.assertIn("websocket-report-ok", failures)

    def test_init_dir_writes_fail_closed_bundle_without_evidence_files(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            bundle_path = bench.init_bundle_dir(base, target="192.0.2.10")

            self.assertEqual(bundle_path, base / bench.BUNDLE_FILENAME)
            self.assertTrue((base / bench.INIT_README_FILENAME).is_file())
            bundle = json.loads(bundle_path.read_text(encoding="utf-8"))
            self.assertEqual(bundle["metadata"]["target"], "192.0.2.10")
            self.assertFalse(bundle["manual"]["deploy"]["operator_authorized"])
            for relative in bundle["evidence_files"].values():
                self.assertFalse((base / relative).exists(), relative)
            readme = (base / bench.INIT_README_FILENAME).read_text(encoding="utf-8")
            self.assertIn(
                f"bash scripts\\dev_deploy.sh 192.0.2.10 --dashboard-only --json --output \"{base / 'dev-deploy-dashboard-only.json'}\"",
                readme,
            )
            self.assertIn(f"--output \"{base / 'dashboard_bench_check.json'}\"", readme)
            self.assertIn(
                f"\"{base / bench.BUNDLE_FILENAME}\" --json --output \"{base / 'final-bench-evidence-validation.json'}\"",
                readme,
            )

    def test_main_writes_init_report_output_file(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            output = base / "init-report.json"

            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([
                    "--init-dir",
                    str(base / "evidence"),
                    "--target",
                    "192.0.2.10",
                    "--json",
                    "--output",
                    str(output),
                ])

            body = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(code, 0)
            self.assertTrue(body["ok"])
            self.assertEqual(body["bundle"], str(base / "evidence" / bench.BUNDLE_FILENAME))
            self.assertEqual(body["readme"], str(base / "evidence" / bench.INIT_README_FILENAME))

    def test_init_dir_refuses_to_overwrite_existing_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            bench.init_bundle_dir(base)

            with self.assertRaises(ValueError):
                bench.init_bundle_dir(base)

    def test_fails_when_probe_reports_(self) -> None:
        bundle = valid_bundle()
        reports = default_reports()
        reports["ws.json"] = websocket_report()
        reports["ws.json"]["target"] = "ws://198.51.100.42:8080/ws?token=%3Credacted%3E"
        temp, path = self.write_bundle(bundle, reports=reports)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("target-consistency", failures)

    def test_fails_when_websocket_report_is_not_direct_8080(self) -> None:
        bundle = valid_bundle()
        reports = default_reports()
        reports["ws.json"] = websocket_report()
        reports["ws.json"]["target"] = "ws://192.0.2.10/ws?token=%3Credacted%3E"
        temp, path = self.write_bundle(bundle, reports=reports)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("websocket-target-port-8080", failures)

    def test_fails_when_websocket_report_uses_wrong_path(self) -> None:
        bundle = valid_bundle()
        reports = default_reports()
        reports["ws.json"] = websocket_report()
        reports["ws.json"]["target"] = "ws://192.0.2.10:8080/socket?token=%3Credacted%3E"
        temp, path = self.write_bundle(bundle, reports=reports)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("websocket-target-path", failures)

    def test_fails_when_deploy_report_is_not_json(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            deploy_path = Path(temp.name) / bundle["evidence_files"]["deploy_report"]
            deploy_path.write_text("dashboard deployed\n", encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("deploy-report-json", failures)

    def test_fails_when_deploy_report_does_not_match_delivery_artifact(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            deploy_path = Path(temp.name) / bundle["evidence_files"]["deploy_report"]
            mutated = deploy_report()
            mutated["sha256"] = "b" * 64
            deploy_path.write_text(json.dumps(mutated), encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("deploy-report.sha256-served-match", failures)
        self.assertIn("deploy-report.sha256-local-dist-match", failures)

    def test_fails_when_deploy_report_targets_a_different_unit(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            deploy_path = Path(temp.name) / bundle["evidence_files"]["deploy_report"]
            mutated = deploy_report()
            mutated["miner_ip"] = "198.51.100.42"
            deploy_path.write_text(json.dumps(mutated), encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("deploy-report.target-consistency", failures)

    def test_main_writes_final_validation_report_output_file(self) -> None:
        temp, path = self.write_bundle(valid_bundle())
        with temp:
            output = Path(temp.name) / "final-report.json"

            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([str(path), "--json", "--output", str(output)])

            body = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(code, 0)
            self.assertTrue(body["ok"])
            self.assertEqual(body["bundle"], str(path))
            self.assertIn("checks", body)
            self.assertIn("evidence_files", body)

    def test_main_writes_failing_validation_report_output_file(self) -> None:
        bundle = valid_bundle()
        bundle["manual"]["browser_transport"]["devtools_ws_101"] = False
        temp, path = self.write_bundle(bundle)
        with temp:
            output = Path(temp.name) / "final-report.json"

            with contextlib.redirect_stdout(io.StringIO()):
                code = bench.main([str(path), "--json", "--output", str(output)])

            body = json.loads(output.read_text(encoding="utf-8"))
            failures = {check["name"] for check in body["checks"] if not check["ok"]}
            self.assertEqual(code, 1)
            self.assertFalse(body["ok"])
            self.assertIn("browser_transport.devtools_ws_101", failures)

    def test_accepts_first_share_when_achieved_difficulty_is_not_reported(self) -> None:
        bundle = valid_bundle()
        bundle["manual"]["first_share"]["achieved_difficulty"] = None
        bundle["manual"]["first_share"]["achieved_difficulty_source"] = "not_reported"
        temp, path = self.write_bundle(bundle)
        with temp:
            report = bench.validate_bundle(path)

        self.assertTrue(report.ok, [check for check in report.checks if not check.ok])

    def test_fails_when_first_share_timestamps_do_not_match_same_event(self) -> None:
        bundle = valid_bundle()
        bundle["manual"]["first_share"]["led_event_timestamp"] = "2026-07-04T18:02:00Z"
        temp, path = self.write_bundle(bundle)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("first_share.led_timestamp_near_accept", failures)
        self.assertIn("first_share.ui_led_timestamp_near_each_other", failures)

    def test_fails_when_first_share_difficulty_source_requires_value(self) -> None:
        bundle = valid_bundle()
        bundle["manual"]["first_share"]["achieved_difficulty"] = None
        bundle["manual"]["first_share"]["achieved_difficulty_source"] = "live_event"
        temp, path = self.write_bundle(bundle)
        with temp:
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("first_share.achieved_difficulty_honest", failures)

    def test_fails_when_required_evidence_file_is_missing(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            missing = Path(temp.name) / bundle["evidence_files"]["devtools_ws_101_screenshot"]
            missing.unlink()
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("evidence-file.devtools_ws_101_screenshot", failures)
        self.assertNotIn(
            "attachment.devtools_ws_101_screenshot",
            {proof.name for proof in report.evidence_files},
        )

    def test_fails_when_required_evidence_file_is_empty(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            empty = Path(temp.name) / bundle["evidence_files"]["first_share_screenshot"]
            empty.write_text("", encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("evidence-file.first_share_screenshot", failures)
        self.assertNotIn(
            "attachment.first_share_screenshot",
            {proof.name for proof in report.evidence_files},
        )

    def test_fails_when_screenshot_evidence_file_is_not_an_image(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            screenshot = Path(temp.name) / bundle["evidence_files"]["transport_live_screenshot"]
            screenshot.write_text("not an image\n", encoding="utf-8")
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("evidence-file.transport_live_screenshot.image", failures)
        self.assertNotIn(
            "attachment.transport_live_screenshot",
            {proof.name for proof in report.evidence_files},
        )

    def test_fails_when_screenshot_evidence_file_is_too_small(self) -> None:
        bundle = valid_bundle()
        temp, path = self.write_bundle(bundle)
        with temp:
            screenshot = Path(temp.name) / bundle["evidence_files"]["transport_live_screenshot"]
            screenshot.write_bytes(png_bytes(1, 1))
            report = bench.validate_bundle(path)

        failures = {check.name for check in report.checks if not check.ok}
        self.assertFalse(report.ok)
        self.assertIn("evidence-file.transport_live_screenshot.dimensions", failures)
        self.assertNotIn(
            "attachment.transport_live_screenshot",
            {proof.name for proof in report.evidence_files},
        )

    def test_template_is_parseable_and_fail_closed(self) -> None:
        bundle = bench.template()
        self.assertIn("manual", bundle)
        self.assertIn("evidence_files", bundle)
        reports = {
            "dashboard_bench_check.json": delivery_report(),
            "dashboard_ws_bench_check.json": websocket_report(),
            "dashboard_recovery_down.json": recovery_report(bench.REQUIRED_RECOVERY_DOWN_CHECKS),
            "dashboard_recovery_after_start.json": recovery_report(bench.REQUIRED_RECOVERY_AFTER_START_CHECKS),
            "dashboard_kiosk_soak.json": kiosk_report(),
        }
        temp, path = self.write_bundle({
            **bundle,
        }, reports=reports)
        with temp:
            report = bench.validate_bundle(path)

        self.assertFalse(report.ok)


if __name__ == "__main__":
    unittest.main()
