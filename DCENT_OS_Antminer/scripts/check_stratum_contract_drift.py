#!/usr/bin/env python3
"""Host-only guard for the current cross-family Stratum migration contract."""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def read(rel: str) -> str:
    path = REPO_ROOT / rel
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise SystemExit(f"missing required file: {rel}") from None


def item_body(source: str, keyword: str, name: str) -> str:
    marker = f"pub {keyword} {name}"
    match = re.search(rf"\bpub\s+{keyword}\s+{name}\s*{{", source)
    if match is None:
        raise SystemExit(f"missing item: {marker}")

    brace_start = match.end() - 1

    depth = 0
    for idx in range(brace_start, len(source)):
        char = source[idx]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[brace_start + 1 : idx]

    raise SystemExit(f"unterminated item: {marker}")


def enum_variants(source: str, name: str) -> list[str]:
    body = item_body(source, "enum", name)
    variants: list[str] = []
    depth = 0
    for raw_line in body.splitlines():
        line = raw_line.split("//", 1)[0].strip()
        if depth == 0:
            match = re.match(r"([A-Z][A-Za-z0-9_]*)\s*(?:[({,]|$)", line)
            if match:
                variants.append(match.group(1))
        depth += raw_line.count("{") - raw_line.count("}")
    return variants


def struct_fields(source: str, name: str) -> list[str]:
    body = item_body(source, "struct", name)
    fields: list[str] = []
    depth = 0
    for raw_line in body.splitlines():
        line = raw_line.split("//", 1)[0].strip()
        if depth == 0:
            match = re.match(r"pub\s+([a-z][A-Za-z0-9_]*)\s*:", line)
            if match:
                fields.append(match.group(1))
        depth += raw_line.count("{") - raw_line.count("}")
    return fields


def require_exact(label: str, actual: list[str], expected: list[str]) -> None:
    if actual != expected:
        raise SystemExit(
            f"{label} drifted:\n  actual={actual}\n  expected={expected}"
        )


def require_set(label: str, actual: list[str], expected: set[str]) -> None:
    actual_set = set(actual)
    if actual_set != expected:
        missing = sorted(expected - actual_set)
        extra = sorted(actual_set - expected)
        raise SystemExit(f"{label} drifted: missing={missing} extra={extra}")


def require_contains(label: str, haystack: str, needle: str) -> None:
    if needle not in haystack:
        raise SystemExit(f"{label} missing required text: {needle}")


def main() -> int:
    esp_types = read("DCENT_OS_ESP/dcentaxe-stratum/src/types.rs")
    esp_work = read("DCENT_OS_ESP/dcentaxe-stratum/src/work.rs")
    ant_types = read("DCENT_OS_Antminer/dcentrald/dcentrald-stratum/src/types.rs")
    ant_work = read("DCENT_OS_Antminer/dcentrald/dcentrald-stratum/src/work.rs")

    require_exact(
        "ESP/Avalon StratumEvent variants",
        enum_variants(esp_types, "StratumEvent"),
        [
            "NewJob",
            "DifficultyChanged",
            "VersionMaskChanged",
            "ExtranonceChanged",
            "PrebuiltWork",
            "Disconnected",
            "Reconnected",
        ],
    )
    require_exact(
        "ESP/Avalon MiningEvent variants",
        enum_variants(esp_types, "MiningEvent"),
        ["SubmitShare"],
    )

    require_set(
        "ESP/Avalon StratumJob fields",
        struct_fields(esp_types, "StratumJob"),
        {
            "job_id",
            "prev_hash",
            "coinbase1",
            "coinbase2",
            "merkle_branches",
            "version",
            "nbits",
            "block_height",
            "ntime",
            "clean_jobs",
        },
    )
    require_set(
        "Antminer JobTemplate fields",
        struct_fields(ant_types, "JobTemplate"),
        {
            "job_id",
            "prev_block_hash",
            "coinbase1",
            "coinbase2",
            "merkle_branches",
            "version",
            "nbits",
            "ntime",
            "clean_jobs",
            "share_target",
            "extranonce1",
            "extranonce2_size",
            "version_mask",
            "merkle_root",
            "pool_difficulty",
        },
    )
    require_set(
        "ESP/Avalon ShareSubmission fields",
        struct_fields(esp_types, "ShareSubmission"),
        {
            "job_id",
            "extranonce2",
            "ntime",
            "nonce",
            "version",
            "version_bits",
            "difficulty",
        },
    )
    require_set(
        "Antminer ValidShare fields",
        struct_fields(ant_types, "ValidShare"),
        {
            "worker_name",
            "job_id",
            "extranonce2",
            "ntime",
            "nonce",
            "version_bits",
            "version",
            "achieved_difficulty",
        },
    )

    shared_work_fields = {
        "midstates",
        "merkle4",
        "ntime",
        "nbits",
        "job_id",
        "extranonce2",
        "version",
        "version_mask",
        "share_target",
        "merkle_root",
        "prev_block_hash",
    }
    require_set(
        "ESP/Avalon MiningWork fields",
        struct_fields(esp_work, "MiningWork"),
        shared_work_fields,
    )
    require_set(
        "Antminer MiningWork fields",
        struct_fields(ant_work, "MiningWork"),
        shared_work_fields,
    )

    wm_cargo = read("DCENT_OS_WhatsMiner/dcentrald/dcentrald/Cargo.toml")
    require_contains(
        "Whatsminer Stratum dependency",
        wm_cargo,
        'dcentrald-stratum = { path = "../../../dcentos/dcentrald/dcentrald-stratum"',
    )
    if re.search(r"^\s*dcentaxe-stratum\s*=", wm_cargo, re.MULTILINE):
        raise SystemExit("Whatsminer must stay on dcentrald-stratum for the first migration slice")

    for label, rel in [
        (
            "industrial Avalon Stratum dependency",
            "DCENT_OS_AvalonMiner/dcentrald/dcentrald/Cargo.toml",
        ),
        ("home Avalon Stratum dependency", "projects/dcentaxe-avalon/dcentaxe-nano3s/Cargo.toml"),
    ]:
        cargo = read(rel)
        require_contains(label, cargo, "dcentaxe-stratum")
        require_contains(label, cargo, "dcentaxe-mining")
        if re.search(r"^\s*dcentrald-stratum\s*=", cargo, re.MULTILINE):
            raise SystemExit(f"{label} must not silently switch to dcentrald-stratum")

    print(
        "STRATUM_CONTRACT_DRIFT_OK "
        "esp_events=7 esp_mining_events=1 "
        "shared_work_fields=11 "
        "whatsminer=dcentrald-stratum avalon=dcentaxe-stratum"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
