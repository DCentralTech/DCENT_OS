#!/usr/bin/env python3
"""Classify and pin safety-relevant Rust clamp() call sites.

This is intentionally a classification gate, not a blanket ban on clamp().
Only thermal, voltage, frequency, and fan/PWM/duty contexts are load-bearing
for the min>max panic class tracked by the production-readiness plan. Cosmetic
or protocol clamps are left out of this manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from dataclasses import dataclass
from pathlib import Path


EXPECTED_SAFETY_CLAMP_COUNT = 94
EXPECTED_SAFETY_CLAMP_DIGEST = "54ab513bdbe584c4508592bf7d6d70bda46b6f1d507dfed89f2daeaec3862a35"

CLAMP_RE = re.compile(r"\.clamp\s*\(")
COMMENT_PREFIXES = ("//", "///", "//!","/*", "*")

SKIP_CONTEXT_TOKENS = (
    "target_diff",
    "difficulty",
    "donation",
    "version_mask",
    "extranonce",
    "template_refresh_interval_s",
    "heat_reuse_credit",
    "wall_watts.round",
    "state_topic",
)

CATEGORY_TOKENS = (
    ("voltage", ("voltage", "volt", "_mv", " mv", "dac")),
    ("frequency", ("frequency", "freq", "_mhz", "mhz", "pll")),
    ("fan_pwm", ("fan", "pwm", "duty")),
    ("thermal", ("thermal", "temp", "pid", "gain")),
)


@dataclass(frozen=True)
class ClampSite:
    category: str
    path: str
    line: int
    statement: str

    def fingerprint(self) -> str:
        return f"{self.category}|{self.path}|{self.statement}"


def repo_root(script_path: Path) -> Path:
    return script_path.resolve().parents[1]


def normalize_statement(lines: list[str], start: int) -> str:
    chunk: list[str] = []
    for line in lines[start : min(len(lines), start + 10)]:
        stripped = line.strip()
        if stripped:
            chunk.append(stripped)
        if ";" in stripped or stripped.endswith(")") or stripped.endswith(");"):
            break
    return " ".join(" ".join(chunk).split())


def classify(path: str, context: str) -> str | None:
    lowered = context.lower()
    if any(token in lowered for token in SKIP_CONTEXT_TOKENS):
        return None
    for category, tokens in CATEGORY_TOKENS:
        if any(token in lowered for token in tokens):
            return category
    return None


def collect_sites(: Path) -> list[ClampSite]:
    dcentrald_root =  / "dcentrald"
    sites: list[ClampSite] = []

    for source in sorted(dcentrald_root.rglob("*.rs")):
        rel = source.relative_to.as_posix()
        lines = source.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            stripped = line.strip()
            if not CLAMP_RE.search(line) or stripped.startswith(COMMENT_PREFIXES):
                continue
            context_start = max(0, index - 3)
            context_end = min(len(lines), index + 4)
            context = "\n".join(lines[context_start:context_end])
            category = classify(rel, context)
            if category is None:
                continue
            sites.append(
                ClampSite(
                    category=category,
                    path=rel,
                    line=index + 1,
                    statement=normalize_statement(lines, index),
                )
            )
    return sites


def digest_sites(sites: list[ClampSite]) -> str:
    payload = "\n".join(site.fingerprint() for site in sorted(sites, key=ClampSite.fingerprint))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def print_sites(sites: list[ClampSite]) -> None:
    for site in sorted(sites, key=ClampSite.fingerprint):
        line = f"{site.category:9} {site.path}:{site.line}: {site.statement}"
        encoding = sys.stdout.encoding or "utf-8"
        print(line.encode(encoding, errors="replace").decode(encoding))


def verify(sites: list[ClampSite], *, quiet: bool = False) -> bool:
    digest = digest_sites(sites)
    if len(sites) == EXPECTED_SAFETY_CLAMP_COUNT and digest == EXPECTED_SAFETY_CLAMP_DIGEST:
        return True

    if quiet:
        return False

    print(
        "SAFETY_CLAMP_MANIFEST_MISMATCH "
        f"count={len(sites)} expected={EXPECTED_SAFETY_CLAMP_COUNT} "
        f"digest={digest} expected_digest={EXPECTED_SAFETY_CLAMP_DIGEST}",
        file=sys.stderr,
    )
    print_sites(sites)
    return False


def self_test(: Path) -> bool:
    sites = collect_sites
    if verify(sites, quiet=True):
        synthetic = sites + [
            ClampSite(
                category="fan_pwm",
                path="dcentrald/src/synthetic_unclassified.rs",
                line=1,
                statement="let pwm = requested_pwm.clamp(min_pwm, max_pwm);",
            )
        ]
        if verify(synthetic, quiet=True):
            print(
                "SAFETY_CLAMP_SELFTEST_FAILED synthetic unclassified fan/PWM clamp passed",
                file=sys.stderr,
            )
            return False
        print("SAFETY_CLAMP_SELFTEST_OK")
        return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--print-current", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

     = repo_root(Path(__file__))
    sites = collect_sites
    if args.print_current:
        print(f"count={len(sites)}")
        print(f"digest={digest_sites(sites)}")
        print_sites(sites)
        return 0
    if args.self_test:
        return 0 if self_test else 1
    return 0 if verify(sites) else 1


if __name__ == "__main__":
    raise SystemExit(main())
