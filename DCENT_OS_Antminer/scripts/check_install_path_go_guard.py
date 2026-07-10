#!/usr/bin/env python3
"""Fail if closeout FINAL_GO_NO_GO claims public install GO without proof.

Reads 
and CAPSTONE_EVIDENCE/. Aggregate GO may only be YES when retained evidence
markers exist. Templates alone are not proof.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
CLOSEOUT = REPO / "docs" / "dev" / "2026-07-09-install-path-production-closeout"
FINAL = CLOSEOUT / "FINAL_GO_NO_GO.md"
CAPSTONE = CLOSEOUT / "CAPSTONE_EVIDENCE"
PROOF_MARKERS = (
    "BETA_XIL_LIVE_CAPSTONE_EVIDENCE_OK",
    "BETA_XIL_PUBLIC_URL_VERIFY_OK mode=https",
    "CEREMONY PASS:",
    "restore_action_proof.verified=true",
)


def main() -> int:
    failures: list[str] = []
    if not FINAL.is_file():
        print(f"FAIL: missing {FINAL}")
        return 1
    text = FINAL.read_text(encoding="utf-8")

    # Detect affirmative public install GO claims.
    go_yes = bool(
        re.search(
            r"(?im)^\s*(\*\*)?Aggregate public install GO(\*\*)?:\s*(\*\*)?YES",
            text,
        )
        or re.search(r"(?im)public install GO.*\bYES\b", text)
    )
    go_no = bool(
        re.search(
            r"(?im)^\s*(\*\*)?Aggregate public install GO(\*\*)?:\s*(\*\*)?NO",
            text,
        )
        or re.search(r"(?im)\*\*NO\*\*", text)
    )

    evidence_files: list[Path] = []
    if CAPSTONE.is_dir():
        evidence_files = [
            p
            for p in CAPSTONE.rglob("*")
            if p.is_file()
            and p.stat().st_size > 0
            and p.name not in {".gitkeep", "README.md"}
            and p.name != "README.md"
        ]

    # Distinct proof axes (not counting the same marker twice in one file).
    axes_hit: set[str] = set()
    for p in evidence_files:
        try:
            body = p.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for marker in PROOF_MARKERS:
            if marker in body:
                axes_hit.add(marker)

    # Structured dirs required for YES (prevents junk-file forgery).
    required_dirs = ("key-ceremony", "https-publish", "s9-xil", "s19jpro-xil")
    present_dirs = {
        d.name for d in CAPSTONE.iterdir() if d.is_dir()
    } if CAPSTONE.is_dir() else set()

    if go_yes:
        missing = [d for d in required_dirs if d not in present_dirs]
        if missing:
            failures.append(
                "Aggregate YES requires CAPSTONE_EVIDENCE subdirs: "
                + ", ".join(required_dirs)
                + f" (missing: {', '.join(missing)})"
            )
        if len(axes_hit) < 3:
            failures.append(
                "Aggregate YES requires ≥3 distinct proof markers across CAPSTONE "
                f"(found {sorted(axes_hit)})"
            )

    if not go_no and not go_yes:
        failures.append(
            "FINAL_GO_NO_GO must state Aggregate public install GO: NO or YES explicitly"
        )

    # Honesty: must not say AM2 SD is Public Beta Supported without cold-boot proof dir.
    if re.search(r"AM2 SD.*Public Beta Supported", text, re.I | re.S):
        cold = list(CAPSTONE.glob("**/am2-sd-coldboot*/**")) if CAPSTONE.is_dir() else []
        if not cold:
            failures.append("FINAL claims AM2 SD Supported without CAPSTONE am2-sd-coldboot evidence")

    if failures:
        print("check_install_path_go_guard: FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1

    print("check_install_path_go_guard: PASS")
    print(
        f"  aggregate_go_yes={go_yes} go_no={go_no} "
        f"evidence_files={len(evidence_files)} axes_hit={len(axes_hit)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
