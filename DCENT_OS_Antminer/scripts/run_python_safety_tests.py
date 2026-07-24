#!/usr/bin/env python3
"""Run DCENTos Python safety suites that are not owned by another gate.

Keep this manifest explicit: each entry represents a release-relevant suite
whose reachability must not depend on filename discovery or a developer's
local test habits. Child processes inherit the caller's console encoding so
the tests continue to exercise Windows console portability.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPT_DIR.parent

SUITES = (
    "test_check_install_path_go_guard.py",
    "test_dashboard_bench_check.py",
    "test_dashboard_bench_evidence_check.py",
    "test_dashboard_proxy_policy.py",
    "test_dashboard_recovery_bench_check.py",
    "test_dashboard_serve_path.py",
    "test_dashboard_ws_bench_check.py",
    "test_python_safety_gate_wiring.py",
    "test_validate_am1_nand_backup.py",
    "test_validate_am2_nand_backup.py",
    "test_validate_am3_bb_nand_backup.py",
    "re/test_sal_to_vectors.py",
    "sim/test_check_sim_tier_honesty.py",
    "sim/test_validate_sim_runtime_evidence.py",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the explicitly owned DCENTos Python safety suites."
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the owned suite paths without running them",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    suite_paths = tuple(SCRIPT_DIR / suite for suite in SUITES)

    missing = tuple(path for path in suite_paths if not path.is_file())
    if missing:
        for path in missing:
            print(f"ERROR: Python safety suite is missing: {path}", file=sys.stderr)
        return 2

    if args.list:
        for path in suite_paths:
            print(path.relative_to(PROJECT_ROOT).as_posix())
        return 0

    failures: list[str] = []
    for path in suite_paths:
        label = path.relative_to(SCRIPT_DIR).as_posix()
        print(f"\n=== PYTHON SAFETY SUITE: {label} ===", flush=True)
        result = subprocess.run(
            [sys.executable, str(path)],
            cwd=PROJECT_ROOT,
            check=False,
        )
        if result.returncode == 0:
            print(f"PYTHON SAFETY PASS: {label}", flush=True)
        else:
            print(
                f"PYTHON SAFETY FAIL: {label} (exit {result.returncode})",
                file=sys.stderr,
                flush=True,
            )
            failures.append(label)

    print("\n========================================", flush=True)
    if failures:
        print(
            "Python safety suites failed: " + ", ".join(failures),
            file=sys.stderr,
            flush=True,
        )
        return 1

    print(f"Python safety suites passed: {len(suite_paths)}/{len(suite_paths)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
