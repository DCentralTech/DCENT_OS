#!/usr/bin/env python3
"""Validate a saved operator-run dashboard bench evidence packet."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional
from urllib.parse import urlparse


DEFAULT_MAX_GZIP_BYTES = 614_400
DEFAULT_MAX_IDENTITY_BYTES = 2_345_500
MAX_COLD_SPLASH_MS = 1_000
MAX_COLD_INTERACTIVE_MS = 3_000
MAX_WARM_RELOAD_MS = 1_500
MAX_FIRST_SHARE_EVENT_SKEW_SECONDS = 30
MIN_SCREENSHOT_WIDTH = 320
MIN_SCREENSHOT_HEIGHT = 180
EXPECTED_WS_PORT = 8080
EXPECTED_WS_PATH = "/ws"
ACHIEVED_DIFFICULTY_SOURCES = {"live_event", "share_history", "not_reported"}
DEPLOY_DASHBOARD_REMOTE_PATH = "/usr/share/dcentos-dashboard/index.html"
SCREENSHOT_EVIDENCE_FILES = {
    "transport_live_screenshot",
    "recovery_state_screenshot",
    "wizard_review_screenshot",
    "first_share_screenshot",
    "devtools_ws_101_screenshot",
}
BUNDLE_FILENAME = "bench-evidence-bundle.json"
INIT_README_FILENAME = "BENCH_PACKET_README.txt"


REQUIRED_DELIVERY_CHECKS = {
    "gzip-status",
    "gzip-content-encoding",
    "cache-control",
    "vary-accept-encoding",
    "gzip-etag",
    "gzip-body",
    "diagnostic-banner",
    "gzip-q0-status",
    "gzip-q0-identity",
    "gzip-q0-bytes",
    "identity-status",
    "identity-no-encoding",
    "identity-matches-gzip",
    "etag-304-status",
    "etag-304-empty",
    "version-status",
    "version-json",
    "version-sha",
    "version-size",
    "expected-artifact-sha",
    "expected-identity-size",
    "expected-gzip-size",
    "gzip-budget",
    "identity-budget",
}

REQUIRED_WS_CHECKS = {
    "ws-status-101",
    "ws-upgrade-header",
    "ws-connection-header",
    "ws-accept-header",
    "ws-frame-received",
}

REQUIRED_RECOVERY_DOWN_CHECKS = {
    "dashboard-root-status",
    "diagnostic-status",
    "recovery-status",
    "dashboard-health-daemon-down",
    "api-status-disconnected-code",
    "api-status-disconnected-json",
}

REQUIRED_RECOVERY_AFTER_START_CHECKS = {
    "dashboard-root-status",
    "diagnostic-status",
    "recovery-status",
    "api-status-reachable",
    "api-status-not-disconnected",
}

REQUIRED_KIOSK_CHECKS = {
    "sample-count",
    "heap-samples",
    "heap-growth-budget",
    "page-ready",
    "animation-cap",
    "transport-live",
}

REQUIRED_EVIDENCE_FILES = {
    "build_output",
    "deploy_report",
    "transport_live_screenshot",
    "recovery_state_screenshot",
    "wizard_review_screenshot",
    "first_share_screenshot",
    "devtools_ws_101_screenshot",
    "daemon_authorization_notes",
}

REQUIRED_REPORT_SOURCES = {
    "delivery_report",
    "websocket_report",
    "recovery_down_report",
    "recovery_after_start_report",
    "kiosk_report",
}


@dataclass
class EvidenceCheck:
    name: str
    ok: bool
    detail: str


@dataclass
class EvidenceReport:
    bundle: str
    ok: bool
    evidence_files: List["EvidenceFileProof"]
    checks: List[EvidenceCheck]


@dataclass
class EvidenceFileProof:
    name: str
    path: str
    bytes: int
    sha256: str


def read_json(path: Path) -> Dict[str, Any]:
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise ValueError(f"cannot read {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"{path} is not valid JSON: {exc}") from exc
    if not isinstance(parsed, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return parsed


def resolve_json_source(
    checks: List["EvidenceCheck"],
    bundle_dir: Path,
    source: Any,
    label: str,
) -> tuple[Dict[str, Any], Optional["EvidenceFileProof"]]:
    add(checks, f"report-source.{label}.path", isinstance(source, str) and bool(source.strip()), f"value={source!r}")
    if isinstance(source, dict):
        return source, None
    if isinstance(source, str) and source.strip():
        path = Path(source)
        if not path.is_absolute():
            path = bundle_dir / path
        try:
            proof = hash_file(path, f"report.{label}")
            report = read_json(path)
        except ValueError as exc:
            add(checks, f"report-source.{label}.read", False, str(exc))
            return {}, None
        except OSError as exc:
            add(checks, f"report-source.{label}.read", False, f"cannot read {path}: {exc}")
            return {}, None
        add(checks, f"report-source.{label}.read", True, f"path={path}")
        return report, proof
    add(checks, f"report-source.{label}.read", False, "source must be a path to a JSON object")
    return {}, None


def add(checks: List[EvidenceCheck], name: str, ok: bool, detail: str) -> None:
    checks.append(EvidenceCheck(name=name, ok=ok, detail=detail))


def check_map(report: Dict[str, Any]) -> Dict[str, Dict[str, Any]]:
    raw = report.get("checks")
    if not isinstance(raw, list):
        return {}
    mapped: Dict[str, Dict[str, Any]] = {}
    for item in raw:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            mapped[item["name"]] = item
    return mapped


def nested_get(data: Dict[str, Any], dotted: str) -> Any:
    value: Any = data
    for part in dotted.split("."):
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value


def require_report_ok(
    checks: List[EvidenceCheck],
    label: str,
    report: Dict[str, Any],
) -> None:
    add(checks, f"{label}-report-ok", report.get("ok") is True, f"ok={report.get('ok')!r}")


def require_named_checks(
    checks: List[EvidenceCheck],
    label: str,
    report: Dict[str, Any],
    required: set[str],
) -> None:
    mapped = check_map(report)
    missing = sorted(required - set(mapped))
    failed = sorted(name for name in required if mapped.get(name, {}).get("ok") is not True)
    add(
        checks,
        f"{label}-required-checks-present",
        not missing,
        f"missing={missing}",
    )
    add(
        checks,
        f"{label}-required-checks-pass",
        not failed,
        f"failed={failed}",
    )


def require_bool(
    checks: List[EvidenceCheck],
    data: Dict[str, Any],
    dotted: str,
    expected: bool = True,
) -> None:
    value = nested_get(data, dotted)
    add(checks, dotted, value is expected, f"value={value!r} expected={expected!r}")


def require_nonempty(checks: List[EvidenceCheck], data: Dict[str, Any], dotted: str) -> None:
    value = nested_get(data, dotted)
    add(checks, dotted, isinstance(value, str) and bool(value.strip()), f"value={value!r}")


def parse_timestamp(value: Any) -> Optional[dt.datetime]:
    if not isinstance(value, str) or not value.strip():
        return None
    raw = value.strip()
    if raw.endswith("Z"):
        raw = f"{raw[:-1]}+00:00"
    try:
        parsed = dt.datetime.fromisoformat(raw)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)


def first_share_time_delta_seconds(manual: Dict[str, Any], dotted: str) -> Optional[float]:
    accepted = parse_timestamp(nested_get(manual, "first_share.accepted_share_timestamp"))
    observed = parse_timestamp(nested_get(manual, dotted))
    if accepted is None or observed is None:
        return None
    return abs((observed - accepted).total_seconds())


def validate_first_share_timestamps(checks: List[EvidenceCheck], manual: Dict[str, Any]) -> None:
    values = {
        "accepted": parse_timestamp(nested_get(manual, "first_share.accepted_share_timestamp")),
        "ui": parse_timestamp(nested_get(manual, "first_share.ui_event_timestamp")),
        "led": parse_timestamp(nested_get(manual, "first_share.led_event_timestamp")),
    }
    for label, value in values.items():
        add(checks, f"first_share.{label}_timestamp_parseable", value is not None, f"value={value!r}")

    for label, dotted in [
        ("ui", "first_share.ui_event_timestamp"),
        ("led", "first_share.led_event_timestamp"),
    ]:
        delta = first_share_time_delta_seconds(manual, dotted)
        add(
            checks,
            f"first_share.{label}_timestamp_near_accept",
            delta is not None and delta <= MAX_FIRST_SHARE_EVENT_SKEW_SECONDS,
            f"delta_s={delta!r} max={MAX_FIRST_SHARE_EVENT_SKEW_SECONDS}",
        )

    if values["ui"] is not None and values["led"] is not None:
        delta = abs((values["ui"] - values["led"]).total_seconds())
    else:
        delta = None
    add(
        checks,
        "first_share.ui_led_timestamp_near_each_other",
        delta is not None and delta <= MAX_FIRST_SHARE_EVENT_SKEW_SECONDS,
        f"delta_s={delta!r} max={MAX_FIRST_SHARE_EVENT_SKEW_SECONDS}",
    )


def validate_achieved_difficulty(checks: List[EvidenceCheck], manual: Dict[str, Any]) -> None:
    source = nested_get(manual, "first_share.achieved_difficulty_source")
    difficulty = nested_get(manual, "first_share.achieved_difficulty")
    add(
        checks,
        "first_share.achieved_difficulty_source",
        isinstance(source, str) and source in ACHIEVED_DIFFICULTY_SOURCES,
        f"value={source!r}",
    )
    finite_positive = (
        isinstance(difficulty, (int, float))
        and not isinstance(difficulty, bool)
        and math.isfinite(difficulty)
        and difficulty > 0
    )
    if source == "not_reported":
        ok = difficulty is None
        detail = f"value={difficulty!r} source={source!r}"
    else:
        ok = finite_positive
        detail = f"value={difficulty!r} source={source!r}"
    add(checks, "first_share.achieved_difficulty_honest", ok, detail)


def require_number_lte(
    checks: List[EvidenceCheck],
    data: Dict[str, Any],
    dotted: str,
    limit: float,
) -> None:
    value = nested_get(data, dotted)
    ok = isinstance(value, (int, float)) and not isinstance(value, bool) and value <= limit
    add(checks, dotted, ok, f"value={value!r} max={limit}")


def is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(char in "0123456789abcdefABCDEF" for char in value)
    )


def target_host_from_value(value: Any) -> Optional[str]:
    if not isinstance(value, str) or not value.strip():
        return None
    parsed = urlparse(value if "://" in value else f"http://{value}")
    return parsed.hostname.lower() if parsed.hostname else None


def report_target_host(report: Dict[str, Any]) -> Optional[str]:
    return target_host_from_value(report.get("target"))


def validate_target_consistency(
    checks: List[EvidenceCheck],
    reports: Dict[str, Dict[str, Any]],
) -> None:
    hosts: Dict[str, str] = {}
    for label, report in reports.items():
        host = report_target_host(report)
        add(checks, f"target.{label}.present", host is not None, f"target={report.get('target')!r}")
        if host is not None:
            hosts[label] = host

    unique_hosts = sorted(set(hosts.values()))
    add(
        checks,
        "target-consistency",
        len(hosts) == len(reports) and len(unique_hosts) == 1,
        f"hosts={hosts}",
    )


def resolve_evidence_path(bundle_dir: Path, raw: Any) -> Optional[Path]:
    if not isinstance(raw, str) or not raw.strip():
        return None
    path = Path(raw)
    if not path.is_absolute():
        path = bundle_dir / path
    return path


def hash_file(path: Path, name: str) -> EvidenceFileProof:
    body = path.read_bytes()
    return EvidenceFileProof(
        name=name,
        path=str(path),
        bytes=len(body),
        sha256=hashlib.sha256(body).hexdigest(),
    )


def png_is_well_formed(body: bytes) -> bool:
    if not body.startswith(b"\x89PNG\r\n\x1a\n"):
        return False
    offset = 8
    seen_ihdr = False
    while offset + 12 <= len(body):
        length = int.from_bytes(body[offset:offset + 4], "big")
        chunk_type = body[offset + 4:offset + 8]
        next_offset = offset + 12 + length
        if next_offset > len(body):
            return False
        if not seen_ihdr and chunk_type != b"IHDR":
            return False
        if chunk_type == b"IHDR":
            seen_ihdr = True
        offset = next_offset
        if chunk_type == b"IEND":
            return seen_ihdr and offset == len(body)
    return False


def screenshot_image_type(body: bytes) -> Optional[str]:
    if png_is_well_formed(body):
        return "png"
    if len(body) >= 4 and body.startswith(b"\xff\xd8\xff") and body.endswith(b"\xff\xd9"):
        return "jpeg"
    if len(body) >= 12 and body.startswith(b"RIFF") and body[8:12] == b"WEBP":
        declared_size = int.from_bytes(body[4:8], "little") + 8
        if declared_size <= len(body):
            return "webp"
    return None


def png_dimensions(body: bytes) -> Optional[tuple[int, int]]:
    if not png_is_well_formed(body) or len(body) < 24:
        return None
    return int.from_bytes(body[16:20], "big"), int.from_bytes(body[20:24], "big")


def jpeg_dimensions(body: bytes) -> Optional[tuple[int, int]]:
    if len(body) < 4 or not body.startswith(b"\xff\xd8"):
        return None
    offset = 2
    sof_markers = {
        0xC0, 0xC1, 0xC2, 0xC3,
        0xC5, 0xC6, 0xC7,
        0xC9, 0xCA, 0xCB,
        0xCD, 0xCE, 0xCF,
    }
    while offset + 4 < len(body):
        while offset < len(body) and body[offset] != 0xFF:
            offset += 1
        while offset < len(body) and body[offset] == 0xFF:
            offset += 1
        if offset >= len(body):
            return None
        marker = body[offset]
        offset += 1
        if marker in {0xD8, 0xD9}:
            continue
        if marker == 0xDA:
            return None
        if offset + 2 > len(body):
            return None
        segment_length = int.from_bytes(body[offset:offset + 2], "big")
        if segment_length < 2 or offset + segment_length > len(body):
            return None
        if marker in sof_markers and segment_length >= 7:
            height = int.from_bytes(body[offset + 3:offset + 5], "big")
            width = int.from_bytes(body[offset + 5:offset + 7], "big")
            return width, height
        offset += segment_length
    return None


def webp_dimensions(body: bytes) -> Optional[tuple[int, int]]:
    if len(body) < 20 or not body.startswith(b"RIFF") or body[8:12] != b"WEBP":
        return None
    chunk_type = body[12:16]
    if chunk_type == b"VP8X" and len(body) >= 30:
        width = int.from_bytes(body[24:27], "little") + 1
        height = int.from_bytes(body[27:30], "little") + 1
        return width, height
    if chunk_type == b"VP8 " and len(body) >= 30 and body[23:26] == b"\x9d\x01\x2a":
        width = int.from_bytes(body[26:28], "little") & 0x3FFF
        height = int.from_bytes(body[28:30], "little") & 0x3FFF
        return width, height
    if chunk_type == b"VP8L" and len(body) >= 25 and body[20] == 0x2F:
        b0, b1, b2, b3 = body[21], body[22], body[23], body[24]
        width = 1 + (((b1 & 0x3F) << 8) | b0)
        height = 1 + (((b3 & 0x0F) << 10) | (b2 << 2) | ((b1 & 0xC0) >> 6))
        return width, height
    return None


def screenshot_dimensions(body: bytes, image_type: Optional[str]) -> Optional[tuple[int, int]]:
    if image_type == "png":
        return png_dimensions(body)
    if image_type == "jpeg":
        return jpeg_dimensions(body)
    if image_type == "webp":
        return webp_dimensions(body)
    return None


def validate_evidence_files(
    checks: List[EvidenceCheck],
    bundle_dir: Path,
    evidence_files: Dict[str, Any],
) -> List[EvidenceFileProof]:
    proofs: List[EvidenceFileProof] = []
    missing_keys = sorted(REQUIRED_EVIDENCE_FILES - set(evidence_files))
    add(checks, "evidence-files-required-keys", not missing_keys, f"missing={missing_keys}")

    for key in sorted(REQUIRED_EVIDENCE_FILES):
        path = resolve_evidence_path(bundle_dir, evidence_files.get(key))
        if path is None:
            add(checks, f"evidence-file.{key}", False, "path missing")
            continue
        try:
            stat = path.stat()
        except OSError as exc:
            add(checks, f"evidence-file.{key}", False, f"{path}: {exc}")
            continue
        is_file = path.is_file()
        ok = is_file and stat.st_size > 0
        add(
            checks,
            f"evidence-file.{key}",
            ok,
            f"path={path} bytes={stat.st_size if is_file else 'not-file'}",
        )
        if ok and key in SCREENSHOT_EVIDENCE_FILES:
            try:
                body = path.read_bytes()
                image_type = screenshot_image_type(body)
                dimensions = screenshot_dimensions(body, image_type)
            except OSError as exc:
                image_type = None
                dimensions = None
                detail = f"{path}: {exc}"
            else:
                detail = f"type={image_type!r}"
            image_ok = image_type in {"png", "jpeg", "webp"}
            add(checks, f"evidence-file.{key}.image", image_ok, detail)
            width, height = dimensions or (None, None)
            dimensions_ok = (
                isinstance(width, int)
                and isinstance(height, int)
                and width >= MIN_SCREENSHOT_WIDTH
                and height >= MIN_SCREENSHOT_HEIGHT
            )
            add(
                checks,
                f"evidence-file.{key}.dimensions",
                dimensions_ok,
                f"width={width!r} height={height!r} min={MIN_SCREENSHOT_WIDTH}x{MIN_SCREENSHOT_HEIGHT}",
            )
            ok = ok and image_ok and dimensions_ok
        if ok:
            proofs.append(hash_file(path, f"attachment.{key}"))
    return proofs


def validate_delivery(checks: List[EvidenceCheck], report: Dict[str, Any]) -> None:
    require_report_ok(checks, "delivery", report)
    require_named_checks(checks, "delivery", report, REQUIRED_DELIVERY_CHECKS)
    add(
        checks,
        "delivery-gzip-budget-value",
        isinstance(report.get("gzip_bytes"), int)
        and 0 < report["gzip_bytes"] <= DEFAULT_MAX_GZIP_BYTES,
        f"gzip_bytes={report.get('gzip_bytes')!r} max={DEFAULT_MAX_GZIP_BYTES}",
    )
    add(
        checks,
        "delivery-identity-budget-value",
        isinstance(report.get("identity_bytes"), int)
        and 0 < report["identity_bytes"] <= DEFAULT_MAX_IDENTITY_BYTES,
        f"identity_bytes={report.get('identity_bytes')!r} max={DEFAULT_MAX_IDENTITY_BYTES}",
    )
    expected = report.get("expected")
    add(
        checks,
        "delivery-local-dist-bound",
        isinstance(expected, dict)
        and isinstance(expected.get("sha256"), str)
        and isinstance(expected.get("gzip_bytes"), int)
        and isinstance(expected.get("identity_bytes"), int),
        "expected local dist artifact is present",
    )


def validate_deploy_report(
    checks: List[EvidenceCheck],
    bundle_dir: Path,
    evidence_files: Dict[str, Any],
    delivery: Dict[str, Any],
) -> None:
    path = resolve_evidence_path(bundle_dir, evidence_files.get("deploy_report"))
    if path is None:
        add(checks, "deploy-report-json", False, "deploy_report path missing")
        return
    try:
        report = read_json(path)
    except ValueError as exc:
        add(checks, "deploy-report-json", False, str(exc))
        return
    add(checks, "deploy-report-json", True, f"path={path}")

    add(checks, "deploy-report.success", report.get("success") is True, f"value={report.get('success')!r}")
    add(
        checks,
        "deploy-report.deploy-mode",
        report.get("deploy_mode") == "dashboard-only",
        f"value={report.get('deploy_mode')!r}",
    )
    add(
        checks,
        "deploy-report.remote-path",
        report.get("remote_path") == DEPLOY_DASHBOARD_REMOTE_PATH,
        f"value={report.get('remote_path')!r}",
    )
    add(checks, "deploy-report.pid-null", report.get("pid") is None, f"value={report.get('pid')!r}")

    deploy_sha = report.get("sha256")
    delivery_sha = delivery.get("sha256")
    expected = delivery.get("expected") if isinstance(delivery.get("expected"), dict) else {}
    expected_sha = expected.get("sha256") if isinstance(expected, dict) else None
    add(checks, "deploy-report.sha256-format", is_sha256(deploy_sha), f"value={deploy_sha!r}")
    add(
        checks,
        "deploy-report.sha256-served-match",
        is_sha256(deploy_sha) and isinstance(delivery_sha, str) and deploy_sha.lower() == delivery_sha.lower(),
        f"deploy={deploy_sha!r} served={delivery_sha!r}",
    )
    add(
        checks,
        "deploy-report.sha256-local-dist-match",
        is_sha256(deploy_sha) and isinstance(expected_sha, str) and deploy_sha.lower() == expected_sha.lower(),
        f"deploy={deploy_sha!r} expected={expected_sha!r}",
    )

    deploy_size = report.get("binary_size")
    expected_identity = expected.get("identity_bytes") if isinstance(expected, dict) else None
    delivery_identity = delivery.get("identity_bytes")
    add(
        checks,
        "deploy-report.size-positive",
        isinstance(deploy_size, int) and deploy_size > 0,
        f"value={deploy_size!r}",
    )
    add(
        checks,
        "deploy-report.size-served-match",
        isinstance(deploy_size, int)
        and isinstance(delivery_identity, int)
        and deploy_size == delivery_identity,
        f"deploy={deploy_size!r} served={delivery_identity!r}",
    )
    add(
        checks,
        "deploy-report.size-local-dist-match",
        isinstance(deploy_size, int)
        and isinstance(expected_identity, int)
        and deploy_size == expected_identity,
        f"deploy={deploy_size!r} expected={expected_identity!r}",
    )

    deploy_host = target_host_from_value(report.get("miner_ip"))
    delivery_host = report_target_host(delivery)
    add(checks, "deploy-report.target-present", deploy_host is not None, f"miner_ip={report.get('miner_ip')!r}")
    add(
        checks,
        "deploy-report.target-consistency",
        deploy_host is not None and delivery_host is not None and deploy_host == delivery_host,
        f"deploy={deploy_host!r} delivery={delivery_host!r}",
    )


def validate_websocket(checks: List[EvidenceCheck], report: Dict[str, Any]) -> None:
    require_report_ok(checks, "websocket", report)
    require_named_checks(checks, "websocket", report, REQUIRED_WS_CHECKS)
    target = report.get("target")
    parsed = urlparse(target) if isinstance(target, str) else None
    add(
        checks,
        "websocket-target-scheme",
        parsed is not None and parsed.scheme in {"ws", "wss"},
        f"target={target!r}",
    )
    add(
        checks,
        "websocket-target-port-8080",
        parsed is not None and parsed.port == EXPECTED_WS_PORT,
        f"port={parsed.port if parsed is not None else None!r}",
    )
    add(
        checks,
        "websocket-target-path",
        parsed is not None and parsed.path == EXPECTED_WS_PATH,
        f"path={parsed.path if parsed is not None else None!r}",
    )
    frames = report.get("frames")
    data_frames = [
        frame
        for frame in frames
        if isinstance(frame, dict) and frame.get("opcode") in {1, 2} and frame.get("bytes", 0) > 0
    ] if isinstance(frames, list) else []
    add(checks, "websocket-status-101", report.get("http_status") == 101, f"status={report.get('http_status')!r}")
    add(checks, "websocket-data-frame", bool(data_frames), f"data_frames={len(data_frames)}")


def validate_recovery(
    checks: List[EvidenceCheck],
    label: str,
    report: Dict[str, Any],
    required: set[str],
) -> None:
    require_report_ok(checks, label, report)
    require_named_checks(checks, label, report, required)


def validate_kiosk(checks: List[EvidenceCheck], report: Dict[str, Any]) -> None:
    verdict = report.get("verdict")
    add(checks, "kiosk-report-ok", isinstance(verdict, dict) and verdict.get("ok") is True, f"ok={nested_get(report, 'verdict.ok')!r}")
    if not isinstance(verdict, dict):
        add(checks, "kiosk-required-checks-present", False, "missing verdict")
        add(checks, "kiosk-required-checks-pass", False, "missing verdict")
        return
    require_named_checks(checks, "kiosk", verdict, REQUIRED_KIOSK_CHECKS)
    add(
        checks,
        "kiosk-heap-growth-value",
        isinstance(verdict.get("heapGrowthBytes"), int)
        and isinstance(verdict.get("maxHeapGrowthBytes"), int)
        and verdict["heapGrowthBytes"] <= verdict["maxHeapGrowthBytes"],
        f"growth={verdict.get('heapGrowthBytes')!r} max={verdict.get('maxHeapGrowthBytes')!r}",
    )
    samples = report.get("samples")
    add(checks, "kiosk-samples-present", isinstance(samples, list) and len(samples) >= 3, f"samples={len(samples) if isinstance(samples, list) else 'not-list'}")


def validate_manual(checks: List[EvidenceCheck], manual: Dict[str, Any]) -> None:
    for dotted in [
        "deploy.operator_authorized",
        "deploy.uploaded_sidecars",
        "deploy.no_rust_rebuild",
        "deploy.no_daemon_restart",
        "deploy.no_flash_or_reboot",
        "browser_transport.transport_chip_live_after_ws_frame",
        "browser_transport.devtools_ws_101",
        "browser_transport.rest_polling_inactive_when_live",
        "daemon_recovery.operator_authorized_stop",
        "daemon_recovery.operator_authorized_start",
        "daemon_recovery.transport_downgraded_while_stopped",
        "daemon_recovery.recovers_live_after_frames",
        "daemon_recovery.no_backlog_fx_replay",
        "wizard.operator_prepared_wiped_state",
        "wizard.setup_state_visible",
        "wizard.quick_start_reaches_review",
        "wizard.pre_setup_transport_truthful",
        "first_share.observed_real_pool_accept",
        "first_share.ui_event_once",
        "first_share.led_event_same_share",
        "first_share.no_replay_from_existing_count",
    ]:
        require_bool(checks, manual, dotted, True)

    require_bool(checks, manual, "wizard.skipped_optional_backend_calls", False)
    add(
        checks,
        "deploy.exit_code",
        nested_get(manual, "deploy.exit_code") == 0,
        f"value={nested_get(manual, 'deploy.exit_code')!r} expected=0",
    )
    require_number_lte(checks, manual, "browser_transport.cold_splash_ms", MAX_COLD_SPLASH_MS)
    require_number_lte(checks, manual, "browser_transport.cold_interactive_ms", MAX_COLD_INTERACTIVE_MS)
    require_number_lte(checks, manual, "browser_transport.warm_reload_ms", MAX_WARM_RELOAD_MS)
    for dotted in [
        "first_share.pool",
        "first_share.worker",
        "first_share.accepted_share_timestamp",
        "first_share.ui_event_timestamp",
        "first_share.led_event_timestamp",
    ]:
        require_nonempty(checks, manual, dotted)
    validate_first_share_timestamps(checks, manual)
    validate_achieved_difficulty(checks, manual)


def validate_bundle(path: Path) -> EvidenceReport:
    bundle = read_json(path)
    bundle_dir = path.parent
    checks: List[EvidenceCheck] = []
    report_file_proofs: List[EvidenceFileProof] = []

    missing_report_keys = sorted(REQUIRED_REPORT_SOURCES - set(bundle))
    add(checks, "report-sources-required-keys", not missing_report_keys, f"missing={missing_report_keys}")

    delivery, proof = resolve_json_source(
        checks,
        bundle_dir,
        bundle.get("delivery_report"),
        "delivery_report",
    )
    if proof is not None:
        report_file_proofs.append(proof)
    websocket, proof = resolve_json_source(
        checks,
        bundle_dir,
        bundle.get("websocket_report"),
        "websocket_report",
    )
    if proof is not None:
        report_file_proofs.append(proof)
    recovery_down, proof = resolve_json_source(
        checks,
        bundle_dir,
        bundle.get("recovery_down_report"),
        "recovery_down_report",
    )
    if proof is not None:
        report_file_proofs.append(proof)
    recovery_after_start, proof = resolve_json_source(
        checks,
        bundle_dir,
        bundle.get("recovery_after_start_report"),
        "recovery_after_start_report",
    )
    if proof is not None:
        report_file_proofs.append(proof)
    kiosk, proof = resolve_json_source(checks, bundle_dir, bundle.get("kiosk_report"), "kiosk_report")
    if proof is not None:
        report_file_proofs.append(proof)
    manual = bundle.get("manual")
    if not isinstance(manual, dict):
        raise ValueError("manual must be a JSON object")
    evidence_files = bundle.get("evidence_files")
    if not isinstance(evidence_files, dict):
        raise ValueError("evidence_files must be a JSON object")

    validate_target_consistency(
        checks,
        {
            "delivery": delivery,
            "websocket": websocket,
            "recovery_down": recovery_down,
            "recovery_after_start": recovery_after_start,
            "kiosk": kiosk,
        },
    )
    validate_delivery(checks, delivery)
    validate_deploy_report(checks, bundle_dir, evidence_files, delivery)
    validate_websocket(checks, websocket)
    validate_recovery(checks, "recovery-down", recovery_down, REQUIRED_RECOVERY_DOWN_CHECKS)
    validate_recovery(
        checks,
        "recovery-after-start",
        recovery_after_start,
        REQUIRED_RECOVERY_AFTER_START_CHECKS,
    )
    validate_kiosk(checks, kiosk)
    validate_manual(checks, manual)
    evidence_file_proofs = validate_evidence_files(checks, bundle_dir, evidence_files)

    return EvidenceReport(
        bundle=str(path),
        ok=all(check.ok for check in checks),
        evidence_files=report_file_proofs + evidence_file_proofs,
        checks=checks,
    )


def template() -> Dict[str, Any]:
    return {
        "delivery_report": "dashboard_bench_check.json",
        "websocket_report": "dashboard_ws_bench_check.json",
        "recovery_down_report": "dashboard_recovery_down.json",
        "recovery_after_start_report": "dashboard_recovery_after_start.json",
        "kiosk_report": "dashboard_kiosk_soak.json",
        "evidence_files": {
            "build_output": "dashboard-build-output.txt",
            "deploy_report": "dev-deploy-dashboard-only.json",
            "transport_live_screenshot": "transport-live.png",
            "recovery_state_screenshot": "recovery-polling-or-stale.png",
            "wizard_review_screenshot": "wizard-review.png",
            "first_share_screenshot": "first-share-watch.png",
            "devtools_ws_101_screenshot": "devtools-ws-101.png",
            "daemon_authorization_notes": "daemon-stop-start-authorization.txt",
        },
        "manual": {
            "deploy": {
                "operator_authorized": False,
                "exit_code": None,
                "uploaded_sidecars": False,
                "no_rust_rebuild": False,
                "no_daemon_restart": False,
                "no_flash_or_reboot": False,
            },
            "browser_transport": {
                "cold_splash_ms": None,
                "cold_interactive_ms": None,
                "warm_reload_ms": None,
                "transport_chip_live_after_ws_frame": False,
                "devtools_ws_101": False,
                "rest_polling_inactive_when_live": False,
            },
            "daemon_recovery": {
                "operator_authorized_stop": False,
                "operator_authorized_start": False,
                "transport_downgraded_while_stopped": False,
                "recovers_live_after_frames": False,
                "no_backlog_fx_replay": False,
            },
            "wizard": {
                "operator_prepared_wiped_state": False,
                "setup_state_visible": False,
                "quick_start_reaches_review": False,
                "skipped_optional_backend_calls": None,
                "pre_setup_transport_truthful": False,
            },
            "first_share": {
                "observed_real_pool_accept": False,
                "pool": "",
                "worker": "",
                "accepted_share_timestamp": "",
                "ui_event_timestamp": "",
                "led_event_timestamp": "",
                "ui_event_once": False,
                "led_event_same_share": False,
                "no_replay_from_existing_count": False,
                "achieved_difficulty": None,
                "achieved_difficulty_source": "",
            },
        },
    }


def evidence_path_label(directory: Optional[Path], filename: str) -> str:
    if directory is None:
        return f"<evidence-dir>\\{filename}"
    return str(directory / filename)


def command_path_arg(path: str) -> str:
    return f'"{path}"'


def init_readme(target: Optional[str], directory: Optional[Path] = None) -> str:
    target_label = target or "<miner-ip>"
    build_output = evidence_path_label(directory, "dashboard-build-output.txt")
    deploy_output = evidence_path_label(directory, "dev-deploy-dashboard-only.json")
    delivery_output = evidence_path_label(directory, "dashboard_bench_check.json")
    ws_output = evidence_path_label(directory, "dashboard_ws_bench_check.json")
    recovery_down_output = evidence_path_label(directory, "dashboard_recovery_down.json")
    recovery_after_output = evidence_path_label(directory, "dashboard_recovery_after_start.json")
    kiosk_output = evidence_path_label(directory, "dashboard_kiosk_soak.json")
    bundle_output = evidence_path_label(directory, BUNDLE_FILENAME)
    final_output = evidence_path_label(directory, "final-bench-evidence-validation.json")
    return f"""# DCENT_OS Dashboard Bench Evidence Packet

This directory was initialized by dashboard_bench_evidence_check.py.

Do not create placeholder evidence files. The final validator must fail until
the operator-run bench commands and manual observations have produced real
files for every path named in {BUNDLE_FILENAME}.

Expected collection flow from DCENT_OS_Antminer:

1. Save dashboard build output to {command_path_arg(build_output)}.
2. Run: bash scripts\\dev_deploy.sh {target_label} --dashboard-only --json --output {command_path_arg(deploy_output)}
   The output file is the dashboard-only deploy report.
3. Run: python scripts\\dashboard_bench_check.py {target_label} --expect-local-dist --json --output {command_path_arg(delivery_output)}
   The output file is the delivery probe report.
4. Run: python scripts\\dashboard_ws_bench_check.py {target_label} --prompt-token --json --output {command_path_arg(ws_output)}
   The output file is the WebSocket probe report. It must target ws://{target_label}:8080/ws.
5. During the operator-authorized daemon stop window, run:
   python scripts\\dashboard_recovery_bench_check.py {target_label} --json --output {command_path_arg(recovery_down_output)}
   The output file is the daemon-down recovery probe report.
6. After the operator-authorized daemon start, run:
   python scripts\\dashboard_recovery_bench_check.py {target_label} --after-start --json --output {command_path_arg(recovery_after_output)}
   The output file is the after-start recovery probe report.
7. Run the kiosk soak and save its --output file as {command_path_arg(kiosk_output)}.
8. Add real browser, DevTools, wizard, first-share, and authorization evidence.
9. Fill the manual section in {command_path_arg(bundle_output)}.
10. Run:
    python scripts\\dashboard_bench_evidence_check.py {command_path_arg(bundle_output)} --json --output {command_path_arg(final_output)}

The only passing final result is ok: true.
"""


def init_bundle_dir(directory: Path, target: Optional[str] = None, force: bool = False) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    bundle_path = directory / BUNDLE_FILENAME
    readme_path = directory / INIT_README_FILENAME
    if bundle_path.exists() and not force:
        raise ValueError(f"{bundle_path} already exists; pass --force to overwrite it")

    body = template()
    body["metadata"] = {
        "created_utc": dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "target": target or "",
        "notes": "Fail-closed operator bench evidence bundle template. Required evidence files are not generated.",
    }
    bundle_path.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if force or not readme_path.exists():
        readme_path.write_text(init_readme(target, directory=directory), encoding="utf-8")
    return bundle_path


def write_json_output(path: Path, body: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(body, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def print_text(report: EvidenceReport) -> None:
    print(f"Dashboard bench evidence packet: {'PASS' if report.ok else 'FAIL'}")
    print(f"bundle: {report.bundle}")
    for check in report.checks:
        marker = "PASS" if check.ok else "FAIL"
        print(f"{marker} {check.name}: {check.detail}")


def main(argv: Optional[Iterable[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Validate saved operator-run dashboard bench evidence. This reads local "
            "JSON files only; it does not contact a miner."
        )
    )
    parser.add_argument("bundle", nargs="?", help="Bench evidence bundle JSON")
    parser.add_argument("--print-template", action="store_true", help="Print a bundle JSON template")
    parser.add_argument("--init-dir", help="Create a fail-closed bundle template in this directory")
    parser.add_argument("--target", help="Miner IP/host to record in --init-dir metadata and instructions")
    parser.add_argument("--force", action="store_true", help="Overwrite an existing --init-dir bundle template")
    parser.add_argument("--json", action="store_true", help="Emit a machine-readable validation report")
    parser.add_argument("--output", help="Write the machine-readable report to this JSON file")
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.print_template and args.init_dir:
        print("ERROR: use either --print-template or --init-dir, not both", file=sys.stderr)
        return 2

    if args.print_template:
        print(json.dumps(template(), indent=2, sort_keys=True))
        return 0

    if args.init_dir:
        try:
            bundle_path = init_bundle_dir(Path(args.init_dir), target=args.target, force=args.force)
        except ValueError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 2
        result = {
            "ok": True,
            "bundle": str(bundle_path),
            "readme": str(bundle_path.parent / INIT_README_FILENAME),
        }
        if args.output:
            write_json_output(Path(args.output), result)
        if args.json:
            print(json.dumps(result, indent=2, sort_keys=True))
        else:
            print(f"Initialized fail-closed bench evidence bundle: {bundle_path}")
            print(f"Instructions: {bundle_path.parent / INIT_README_FILENAME}")
        return 0

    if not args.bundle:
        print("ERROR: bundle is required unless --print-template or --init-dir is used", file=sys.stderr)
        return 2

    try:
        report = validate_bundle(Path(args.bundle))
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    result = asdict(report)
    if args.output:
        write_json_output(Path(args.output), result)
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print_text(report)
    return 0 if report.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
