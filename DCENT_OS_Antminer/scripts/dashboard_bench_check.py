#!/usr/bin/env python3
"""Read-only dashboard delivery checks for an operator-run bench unit."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import http.client
import json
import sys
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Dict, Iterable, List, Optional
from urllib.parse import urlparse


BANNER = b'<script src="/static/diagnostic-banner.js" defer></script>'
DEFAULT_DIST_DIR = Path(__file__).resolve().parents[1] / "dashboard" / "dist"
DEFAULT_MAX_GZIP_BYTES = 614_400
DEFAULT_MAX_IDENTITY_BYTES = 2_345_500


@dataclass
class HttpResult:
    status: int
    headers: Dict[str, str]
    body: bytes


@dataclass
class Check:
    name: str
    ok: bool
    detail: str


@dataclass
class DistExpectations:
    dist_dir: str
    sha256: str
    gzip_bytes: int
    identity_bytes: int


@dataclass
class BenchReport:
    target: str
    ok: bool
    sha256: Optional[str]
    gzip_bytes: int
    identity_bytes: int
    etag: Optional[str]
    version: Optional[Dict[str, object]]
    expected: Optional[DistExpectations]
    max_gzip_bytes: Optional[int]
    max_identity_bytes: Optional[int]
    checks: List[Check]


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


def http_get(base_url: str, path: str, headers: Dict[str, str], timeout: float) -> HttpResult:
    parsed = urlparse(base_url)
    conn_cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    conn = conn_cls(parsed.netloc, timeout=timeout)
    try:
        conn.request("GET", path, headers=headers)
        response = conn.getresponse()
        body = response.read()
        return HttpResult(
            status=response.status,
            headers={k.lower(): v for k, v in response.getheaders()},
            body=body,
        )
    finally:
        conn.close()


def add_check(checks: List[Check], name: str, ok: bool, detail: str) -> None:
    checks.append(Check(name=name, ok=ok, detail=detail))


def parse_version(body: bytes) -> Optional[Dict[str, object]]:
    try:
        parsed = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def load_dist_expectations(dist_dir: Path) -> DistExpectations:
    html_path = dist_dir / "index.html"
    gzip_path = dist_dir / "index.html.gz"
    sha_path = dist_dir / "index.html.sha256"

    try:
        html = html_path.read_bytes()
        gzip_body = gzip_path.read_bytes()
        sidecar_sha = sha_path.read_text(encoding="utf-8").strip().split()[0].lower()
    except (OSError, IndexError) as exc:
        raise ValueError(f"cannot read dashboard dist expectations from {dist_dir}: {exc}") from exc

    computed_sha = hashlib.sha256(html).hexdigest()
    if sidecar_sha != computed_sha:
        raise ValueError(
            f"{sha_path} is stale: sidecar {sidecar_sha} != computed {computed_sha}"
        )

    try:
        decompressed = gzip.decompress(gzip_body)
    except OSError as exc:
        raise ValueError(f"{gzip_path} is not a valid gzip stream: {exc}") from exc

    if decompressed != html:
        raise ValueError(f"{gzip_path} does not decompress to {html_path}")

    return DistExpectations(
        dist_dir=str(dist_dir),
        sha256=computed_sha,
        gzip_bytes=len(gzip_body),
        identity_bytes=len(html),
    )


def positive_limit(value: Optional[int]) -> Optional[int]:
    if value is None or value <= 0:
        return None
    return value


def run_checks(
    target: str,
    timeout: float = 10.0,
    expected: Optional[DistExpectations] = None,
    max_gzip_bytes: Optional[int] = DEFAULT_MAX_GZIP_BYTES,
    max_identity_bytes: Optional[int] = DEFAULT_MAX_IDENTITY_BYTES,
) -> BenchReport:
    base_url = normalize_base_url(target)
    checks: List[Check] = []
    served_html: Optional[bytes] = None
    gzip_bytes = 0
    identity_bytes = 0
    etag: Optional[str] = None
    sha: Optional[str] = None
    version: Optional[Dict[str, object]] = None

    try:
        gzip_result = http_get(base_url, "/", {"Accept-Encoding": "gzip"}, timeout)
        gzip_bytes = len(gzip_result.body)
        etag = header(gzip_result.headers, "etag")
        add_check(checks, "gzip-status", gzip_result.status == 200, f"status={gzip_result.status}")
        add_check(
            checks,
            "gzip-content-encoding",
            header(gzip_result.headers, "content-encoding") == "gzip",
            f"content-encoding={header(gzip_result.headers, 'content-encoding')!r}",
        )
        add_check(
            checks,
            "cache-control",
            header(gzip_result.headers, "cache-control") == "no-cache",
            f"cache-control={header(gzip_result.headers, 'cache-control')!r}",
        )
        add_check(
            checks,
            "vary-accept-encoding",
            "accept-encoding" in (header(gzip_result.headers, "vary") or "").lower(),
            f"vary={header(gzip_result.headers, 'vary')!r}",
        )
        add_check(
            checks,
            "gzip-etag",
            bool(etag and etag.startswith('"') and etag.endswith('-gz"')),
            f"etag={etag!r}",
        )
        try:
            served_html = gzip.decompress(gzip_result.body)
            add_check(checks, "gzip-body", True, f"decompressed_bytes={len(served_html)}")
        except OSError as exc:
            add_check(checks, "gzip-body", False, f"decompress failed: {exc}")

        if served_html is not None:
            sha = hashlib.sha256(served_html).hexdigest()
            banner_count = served_html.count(BANNER)
            add_check(
                checks,
                "diagnostic-banner",
                banner_count == 1,
                f"banner_count={banner_count}",
            )

        q_zero = http_get(base_url, "/", {"Accept-Encoding": "gzip;q=0"}, timeout)
        add_check(checks, "gzip-q0-status", q_zero.status == 200, f"status={q_zero.status}")
        add_check(
            checks,
            "gzip-q0-identity",
            header(q_zero.headers, "content-encoding") is None,
            f"content-encoding={header(q_zero.headers, 'content-encoding')!r}",
        )
        if served_html is not None:
            add_check(
                checks,
                "gzip-q0-bytes",
                q_zero.body == served_html,
                f"bytes={len(q_zero.body)} expected={len(served_html)}",
            )

        identity = http_get(base_url, "/", {}, timeout)
        identity_bytes = len(identity.body)
        add_check(checks, "identity-status", identity.status == 200, f"status={identity.status}")
        add_check(
            checks,
            "identity-no-encoding",
            header(identity.headers, "content-encoding") is None,
            f"content-encoding={header(identity.headers, 'content-encoding')!r}",
        )
        if served_html is not None:
            add_check(
                checks,
                "identity-matches-gzip",
                identity.body == served_html,
                f"identity_bytes={len(identity.body)} gzip_html_bytes={len(served_html)}",
            )

        if etag:
            cached = http_get(
                base_url,
                "/",
                {"Accept-Encoding": "gzip", "If-None-Match": etag},
                timeout,
            )
            add_check(checks, "etag-304-status", cached.status == 304, f"status={cached.status}")
            add_check(checks, "etag-304-empty", cached.body == b"", f"bytes={len(cached.body)}")
        else:
            add_check(checks, "etag-304-status", False, "gzip ETag missing")
            add_check(checks, "etag-304-empty", False, "gzip ETag missing")

        version_result = http_get(base_url, "/api/dashboard/version", {}, timeout)
        add_check(
            checks,
            "version-status",
            version_result.status == 200,
            f"status={version_result.status}",
        )
        version = parse_version(version_result.body)
        add_check(checks, "version-json", version is not None, "parsed" if version else "not json")
        if version is not None:
            version_sha = version.get("sha256")
            add_check(
                checks,
                "version-sha",
                isinstance(version_sha, str) and sha is not None and version_sha.lower() == sha,
                f"version_sha={version_sha!r} served_sha={sha!r}",
            )
            version_size = version.get("size_bytes")
            expected_size = len(served_html) if served_html is not None else None
            add_check(
                checks,
                "version-size",
                isinstance(version_size, int)
                and expected_size is not None
                and version_size == expected_size,
                f"version_size={version_size!r} served_size={expected_size!r}",
            )
    except OSError as exc:
        add_check(checks, "http", False, str(exc))

    if expected is not None:
        add_check(
            checks,
            "expected-artifact-sha",
            sha == expected.sha256,
            f"served_sha={sha!r} expected_sha={expected.sha256!r}",
        )
        add_check(
            checks,
            "expected-identity-size",
            identity_bytes == expected.identity_bytes,
            f"identity_bytes={identity_bytes} expected={expected.identity_bytes}",
        )
        add_check(
            checks,
            "expected-gzip-size",
            gzip_bytes == expected.gzip_bytes,
            f"gzip_bytes={gzip_bytes} expected={expected.gzip_bytes}",
        )

    gzip_limit = positive_limit(max_gzip_bytes)
    if gzip_limit is not None:
        add_check(
            checks,
            "gzip-budget",
            0 < gzip_bytes <= gzip_limit,
            f"gzip_bytes={gzip_bytes} max={gzip_limit}",
        )

    identity_limit = positive_limit(max_identity_bytes)
    if identity_limit is not None:
        add_check(
            checks,
            "identity-budget",
            0 < identity_bytes <= identity_limit,
            f"identity_bytes={identity_bytes} max={identity_limit}",
        )

    return BenchReport(
        target=base_url,
        ok=all(check.ok for check in checks),
        sha256=sha,
        gzip_bytes=gzip_bytes,
        identity_bytes=identity_bytes,
        etag=etag,
        version=version,
        expected=expected,
        max_gzip_bytes=gzip_limit,
        max_identity_bytes=identity_limit,
        checks=checks,
    )


def print_text(report: BenchReport) -> None:
    print(f"Dashboard bench delivery check: {'PASS' if report.ok else 'FAIL'}")
    print(f"target: {report.target}")
    if report.sha256:
        print(f"served_sha256: {report.sha256}")
    if report.etag:
        print(f"gzip_etag: {report.etag}")
    print(f"gzip_bytes: {report.gzip_bytes}")
    print(f"identity_bytes: {report.identity_bytes}")
    if report.expected is not None:
        print(f"expected_dist: {report.expected.dist_dir}")
        print(f"expected_sha256: {report.expected.sha256}")
        print(f"expected_gzip_bytes: {report.expected.gzip_bytes}")
        print(f"expected_identity_bytes: {report.expected.identity_bytes}")
    if report.max_gzip_bytes is not None:
        print(f"max_gzip_bytes: {report.max_gzip_bytes}")
    if report.max_identity_bytes is not None:
        print(f"max_identity_bytes: {report.max_identity_bytes}")
    for check in report.checks:
        marker = "PASS" if check.ok else "FAIL"
        print(f"{marker} {check.name}: {check.detail}")


def write_json_report(path: Path, report: BenchReport) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(report), indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main(argv: Optional[Iterable[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Run read-only dashboard gzip, ETag, and version checks against an "
            "operator-supplied bench miner."
        )
    )
    parser.add_argument("target", help="Miner host or base URL, for example 203.0.113.50")
    parser.add_argument("--timeout", type=float, default=10.0, help="HTTP timeout in seconds")
    parser.add_argument(
        "--expect-local-dist",
        action="store_true",
        help="Require served bytes to match the local dashboard/dist artifact",
    )
    parser.add_argument(
        "--dist-dir",
        default=str(DEFAULT_DIST_DIR),
        help="Dashboard dist directory used with --expect-local-dist",
    )
    parser.add_argument(
        "--max-gzip-bytes",
        type=int,
        default=DEFAULT_MAX_GZIP_BYTES,
        help="Fail if the gzip response body exceeds this size; set 0 to disable",
    )
    parser.add_argument(
        "--max-identity-bytes",
        type=int,
        default=DEFAULT_MAX_IDENTITY_BYTES,
        help="Fail if the identity response body exceeds this size; set 0 to disable",
    )
    parser.add_argument("--json", action="store_true", help="Emit a machine-readable report")
    parser.add_argument("--output", help="Write the machine-readable JSON report to this path")
    args = parser.parse_args(list(argv) if argv is not None else None)

    expected: Optional[DistExpectations] = None
    if args.expect_local_dist:
        try:
            expected = load_dist_expectations(Path(args.dist_dir))
        except ValueError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            return 2

    try:
        report = run_checks(
            args.target,
            timeout=args.timeout,
            expected=expected,
            max_gzip_bytes=args.max_gzip_bytes,
            max_identity_bytes=args.max_identity_bytes,
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
