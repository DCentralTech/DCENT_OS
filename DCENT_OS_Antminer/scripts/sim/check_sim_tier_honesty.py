#!/usr/bin/env python3
"""Fail when a declared simulator tier exceeds its checked evidence."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Sequence


INTEGRATED_T2 = {
    "s9",
    "s17",
    "s17pro",
    "t17",
    "s19pro",
    "s19jpro",
    "s19xp",
    "s19kpro",
    "s21",
    "s21pro",
}
EXPECTED_MODELS = {
    "s9",
    "s11",
    "s15",
    "t15",
    "s17",
    "s17pro",
    "t17",
    "s17plus",
    "t17plus",
    "s17e",
    "s19",
    "s19pro",
    "s19jpro",
    "s19xp",
    "s19kpro",
    "s21",
    "s21pro",
    "s21xp",
    "s23",
}
MATRIX_SCHEMA = "dcent-sim-tier-matrix-v1"
KNOWN_STRICTNESS = {"exact", "structural", "implementation_snapshot", "scaffold"}


def unique_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def load_tier_matrix(path: Path) -> tuple[dict[str, dict[str, Any]], list[str]]:
    failures: list[str] = []
    try:
        data = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=unique_json_object,
        )
    except (OSError, ValueError) as error:
        return {}, [f"cannot load tier matrix: {error}"]
    if not isinstance(data, dict):
        return {}, ["tier matrix root must be a JSON object"]
    if data.get("schema") != MATRIX_SCHEMA:
        failures.append(f"matrix schema must be {MATRIX_SCHEMA!r}")
    raw_models = data.get("models")
    if not isinstance(raw_models, dict):
        failures.append("matrix models must be a JSON object")
        return {}, failures
    declared_models = set(raw_models)
    for model in sorted(EXPECTED_MODELS - declared_models):
        failures.append(f"matrix is missing required model {model}")
    for model in sorted(declared_models - EXPECTED_MODELS):
        failures.append(f"matrix declares unexpected model {model}")

    models: dict[str, dict[str, Any]] = {}
    for model, claim in raw_models.items():
        if not isinstance(claim, dict):
            failures.append(f"{model}: tier claim must be a JSON object")
            continue
        tier = claim.get("tier")
        strictness = claim.get("strictness")
        if type(tier) is not int:
            failures.append(f"{model}: declared tier must be an integer")
            continue
        if tier not in range(5):
            failures.append(f"{model}: declared tier {tier} is outside T0-T4")
            continue
        if tier == 4:
            failures.append(
                f"{model}: T4 requires per-run runtime and write-path evidence; "
                "it cannot be declared statically"
            )
            continue
        if not isinstance(strictness, str) or strictness not in KNOWN_STRICTNESS:
            failures.append(f"{model}: unknown evidence strictness {strictness!r}")
            continue
        if tier >= 3 and strictness not in {"exact", "structural"}:
            failures.append(f"{model}: T3 requires exact or structural evidence")
            continue
        models[model] = claim
    return models, failures


def read_vector_header(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        first_line = handle.readline()
    if not first_line:
        raise ValueError("file is empty")
    header = json.loads(first_line, object_pairs_hook=unique_json_object)
    if not isinstance(header, dict):
        raise ValueError("first line is not a JSON object")
    return header


def validate_tier_evidence(
    models: dict[str, dict[str, Any]], workspace: Path
) -> list[str]:
    failures: list[str] = []
    vectors = workspace / "dcentrald-re-catalog" / "vectors"
    for model, claim in models.items():
        tier = claim["tier"]
        strictness = claim["strictness"]
        init_vector = vectors / model / "init_sequence.jsonl"
        if tier >= 3 and not init_vector.is_file():
            failures.append(f"{model}: T{tier} has no init_sequence.jsonl")
        elif init_vector.is_file():
            try:
                header = read_vector_header(init_vector)
            except (OSError, ValueError) as error:
                failures.append(f"{model}: invalid init vector header: {error}")
            else:
                if header.get("schema") != "dcent-init-trace-v1":
                    failures.append(
                        f"{model}: init vector header has an invalid schema"
                    )
                if header.get("model") != model:
                    failures.append(
                        f"{model}: init vector header names model "
                        f"{header.get('model')!r}"
                    )
                if header.get("strictness") != strictness:
                    failures.append(
                        f"{model}: matrix strictness {strictness!r} disagrees with "
                        f"vector strictness {header.get('strictness')!r}"
                    )
                if tier >= 3 and header.get("maturity") == "experimental":
                    failures.append(f"{model}: experimental vector may not claim T3")
        if tier >= 2 and model not in INTEGRATED_T2:
            failures.append(
                f"{model}: T2 claimed without a checked integrated model proof"
            )
        if model == "s23" and tier > 1:
            failures.append("s23: no-ground-truth scaffold may not exceed T1")
        if model == "s23" and init_vector.exists():
            failures.append(
                "s23: init vector must not exist before ground truth arrives"
            )
    return failures


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
    parser.add_argument("--model", help="validate that this model supports --tier")
    parser.add_argument("--tier", type=int, choices=range(5))
    args = parser.parse_args(argv)
    if (args.model is None) != (args.tier is None):
        parser.error("--model and --tier must be provided together")

    models, failures = load_tier_matrix(args.matrix)

    failures.extend(validate_tier_evidence(models, args.workspace))

    if args.model is not None:
        if args.model not in models:
            failures.append(f"{args.model}: model is not declared in the tier matrix")
        elif args.tier > models[args.model]["tier"]:
            failures.append(
                f"{args.model}: requested T{args.tier} exceeds declared "
                f"T{models[args.model]['tier']}"
            )

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    request = (
        f" request={args.model}:T{args.tier}" if args.model is not None else ""
    )
    print(f"SIM_TIER_HONESTY_OK models={len(models)}{request}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
