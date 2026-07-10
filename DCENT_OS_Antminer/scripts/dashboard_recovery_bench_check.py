#!/usr/bin/env python3
"""Read-only daemon-down recovery checks for an operator-run bench unit."""

from __future__ import annotations

import argparse
import http.client
import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Optional
from urllib.parse import urlparse


@dataclass
class HttpResult:
    status: int
    headers: Dict[str, str]
    body: bytes


@dataclass
class RecoveryCheck:
    name: str
    ok: bool
    detail: str


@dataclass
class RecoveryReport:
    target: str
    ok: bool
    checks: List[RecoveryCheck]


def normalize_base_url(target: str) -> str:
    candidate = target.strip()
    if not candidate:
        raise ValueError("target is required")
    if "://" not in candidate:
        candidate = f"http://{candidate}"
    parsed = urlparse(candidate)
    if parsed.scheme not in {"http", "https"}:
        raise ValueError("target must use http or https")
    if not parsed.netloc:
        raise ValueError("target must include a host")
    return f"{parsed.scheme}://{parsed.netloc}"


def header(headers: Dict[str, str], name: str) -> Optional[str]:
    return headers.get(name.lower())


def http_get(base_url: str, path: str, timeout: float) -> HttpResult:
    parsed = urlparse(base_url)
    conn_cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(parsed.netloc, timeout=timeout)
    try:
        conn.request("GET", path, headers={"Accept": "application/json,text/html"})
        response = conn.getresponse()
        return HttpResult(
            status=response.status,
            headers={k.lower(): v for k, v in response.getheaders()},
            body=response.read(),
        )
    finally:
        conn.close()


def add_check(checks: List[RecoveryCheck], name: str, ok: bool, detail: str) -> None:
    checks.append(RecoveryCheck(name=name, ok=ok, detail=detail))


def parse_json(body: bytes) -> Optional[Dict[str, object]]:
    try:
        parsed = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def check_html_page(
    checks: List[RecoveryCheck],
    base_url: str,
    path: str,
    name: str,
    timeout: float,
    marker: bytes,
) -> None:
    result = http_get(base_url, path, timeout)
    ctype = header(result.headers, "content-type") or ""
    add_check(checks, f"{name}-status", result.status == 200, f"status={result.status}")
    add_check(checks, f"{name}-html", "text/html" in ctype.lower(), f"content-type={ctype!r}")
    add_check(checks, f"{name}-body", marker in result.body, f"bytes={len(result.body)}")


def run_checks(
    target: str,
    timeout: float = 10.0,
    expect_daemon_down: bool = True,
) -> RecoveryReport:
    base_url = normalize_base_url(target)
    checks: List[RecoveryCheck] = []

    try:
        check_html_page(checks, base_url, "/", "dashboard-root", timeout, b"<html")
        check_html_page(checks, base_url, "/diagnostic", "diagnostic", timeout, b"<html")
        check_html_page(checks, base_url, "/recovery", "recovery", timeout, b"<html")

        health = http_get(base_url, "/api/dashboard/health", timeout)
        health_json = parse_json(health.body)
        add_check(checks, "dashboard-health-status", health.status == 200, f"status={health.status}")
        add_check(
            checks,
            "dashboard-health-json",
            health_json is not None,
            "parsed" if health_json else "not json",
        )
        if health_json is not None and expect_daemon_down:
            alive = health_json.get("alive")
            api_bound = health_json.get("api_bound")
            add_check(
                checks,
                "dashboard-health-daemon-down",
                alive is False or api_bound is False,
                f"alive={alive!r} api_bound={api_bound!r}",
            )

        status = http_get(base_url, "/api/status", timeout)
        status_json = parse_json(status.body)
        if expect_daemon_down:
            add_check(checks, "api-status-disconnected-code", status.status == 503, f"status={status.status}")
            add_check(
                checks,
                "api-status-disconnected-json",
                isinstance(status_json, dict) and status_json.get("_disconnected") is True,
                f"body_keys={sorted(status_json.keys()) if isinstance(status_json, dict) else 'not-json'}",
            )
        else:
            add_check(checks, "api-status-reachable", status.status == 200, f"status={status.status}")
            add_check(
                checks,
                "api-status-not-disconnected",
                not (isinstance(status_json, dict) and status_json.get("_disconnected") is True),
                f"body_keys={sorted(status_json.keys()) if isinstance(status_json, dict) else 'not-json'}",
            )
    except OSError as exc:
        add_check(checks, "http", False, str(exc))

    return RecoveryReport(target=base_url, ok=all(check.ok for check in checks), checks=checks)


def print_text(report: RecoveryReport) -> None:
    print(f"Dashboard recovery bench check: {'PASS' if report.ok else 'FAIL'}")
    print(f"target: {report.target}")
    for check in report.checks:
        marker = "PASS" if check.ok else "FAIL"
        print(f"{marker} {check.name}: {check.detail}")


def write_json_report(path: Path, report: RecoveryReport) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(report), indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main(argv: Optional[Iterable[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Run read-only dashboard recovery checks during an operator-authorized "
            "dcentrald stop/start bench window."
        )
    )
    parser.add_argument("target", help="Miner host or base URL, for example 203.0.113.50")
    parser.add_argument("--timeout", type=float, default=10.0, help="HTTP timeout in seconds")
    parser.add_argument(
        "--after-start",
        action="store_true",
        help="Expect /api/status to be reachable instead of daemon-down",
    )
    parser.add_argument("--json", action="store_true", help="Emit a machine-readable report")
    parser.add_argument("--output", help="Write the machine-readable JSON report to this path")
    args = parser.parse_args(list(argv) if argv is not None else None)

    try:
        report = run_checks(
            args.target,
            timeout=args.timeout,
            expect_daemon_down=not args.after_start,
        )
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    if args.output:
        write_json_report(Path(args.output), report)

    if args.json:
        print(json.dumps(asdict(report), indent=2, sort_keys=True))
    else:
        print_text(report)
    return 0 if report.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
