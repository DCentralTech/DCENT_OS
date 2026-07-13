#!/usr/bin/env python3
"""Render the honest S9-S23 offline verification grid."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Sequence


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--matrix", type=Path, default=Path(__file__).with_name("model_tiers.json")
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    models = json.loads(args.matrix.read_text(encoding="utf-8"))["models"]
    if args.json:
        print(json.dumps(models, indent=2, sort_keys=True))
        return 0
    print("| model | T0 | T1 | T2 | T3 | T4 | evidence |")
    print("|---|---:|---:|---:|---:|---:|---|")
    for model, claim in models.items():
        tier = int(claim["tier"])
        cells = ["PASS" if tier >= level else "—" for level in range(5)]
        print(f"| {model} | {' | '.join(cells)} | {claim['strictness']} |")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
