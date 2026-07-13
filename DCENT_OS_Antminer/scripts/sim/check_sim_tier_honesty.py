#!/usr/bin/env python3
"""Fail when a declared simulator tier exceeds its checked evidence."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Sequence


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--matrix",
        type=Path,
        default=Path(__file__).with_name("model_tiers.json"),
    )
    parser.add_argument(
        "--workspace",
        type=Path,
        default=Path(__file__).resolve().parents[2] / "dcentrald",
    )
    args = parser.parse_args(argv)
    data = json.loads(args.matrix.read_text(encoding="utf-8"))
    vectors = args.workspace / "dcentrald-re-catalog" / "vectors"
    failures = []
    for model, claim in data["models"].items():
        tier = int(claim["tier"])
        strictness = claim["strictness"]
        init_vector = vectors / model / "init_sequence.jsonl"
        if tier >= 3 and not init_vector.is_file():
            failures.append(f"{model}: T{tier} has no init_sequence.jsonl")
        if tier >= 3 and strictness not in {"exact", "structural"}:
            failures.append(f"{model}: T3 requires exact or structural evidence")
        integrated_t2 = {
            "s9", "s17", "s17pro", "t17", "s19pro", "s19jpro",
            "s19xp", "s19kpro", "s21", "s21pro",
        }
        if tier >= 2 and model not in integrated_t2:
            failures.append(f"{model}: T2 claimed without a checked integrated model proof")
        if model == "s23" and tier > 1:
            failures.append("s23: no-ground-truth scaffold may not exceed T1")
        if model == "s23" and init_vector.exists():
            failures.append("s23: init vector must not exist before ground truth arrives")
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print(f"SIM_TIER_HONESTY_OK models={len(data['models'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
