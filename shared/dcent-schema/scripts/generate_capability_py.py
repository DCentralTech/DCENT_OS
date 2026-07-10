#!/usr/bin/env python3
"""Generate toolbox Python mirrors for dcent-schema capability contracts."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


SCRIPT = Path(__file__).resolve()
SCHEMA_ROOT = SCRIPT.parents[1]
REPO_ROOT = SCRIPT.parents[3]
DEFAULT_INPUT = SCHEMA_ROOT / "src" / "capability.rs"
DEFAULT_OUTPUT = (
    REPO_ROOT
    / "projects"
    / "dcent-toolbox"
    / "src"
    / "dcent_toolbox"
    / "core"
    / "generated_capability.py"
)


def camel_to_kebab(value: str) -> str:
    pieces = re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?=[A-Z]|$)", value)
    return "-".join(piece.lower() for piece in pieces)


def serde_rename_all(attrs: str) -> str | None:
    match = re.search(r'rename_all\s*=\s*"([^"]+)"', attrs)
    return match.group(1) if match else None


def serde_rename(attrs: list[str]) -> str | None:
    joined = "\n".join(attrs)
    match = re.search(r'rename\s*=\s*"([^"]+)"', joined)
    return match.group(1) if match else None


def const_values(source: str, name: str) -> list[str]:
    match = re.search(
        rf"pub const {re.escape(name)}:\s*&\[\&str\]\s*=\s*&\[(.*?)\];",
        source,
        flags=re.S,
    )
    if not match:
        raise SystemExit(f"missing Rust const {name}")
    return re.findall(r'"([^"]+)"', match.group(1))


def enum_values(attrs: str, body: str) -> list[str]:
    rename_all = serde_rename_all(attrs)
    values: list[str] = []
    pending_attrs: list[str] = []
    for raw in body.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith("#["):
            pending_attrs.append(line)
            continue
        match = re.match(r"([A-Za-z][A-Za-z0-9_]*)\s*,", line)
        if not match:
            pending_attrs.clear()
            continue
        variant = match.group(1)
        renamed = serde_rename(pending_attrs)
        if renamed is not None:
            values.append(renamed)
        elif rename_all == "kebab-case":
            values.append(camel_to_kebab(variant))
        elif rename_all == "lowercase":
            values.append(variant.lower())
        else:
            values.append(variant)
        pending_attrs.clear()
    return values


def parse_enums(source: str) -> dict[str, list[str]]:
    enums: dict[str, list[str]] = {}
    pattern = re.compile(
        r"((?:#\[[^\n]+\]\s*)*)pub enum ([A-Za-z][A-Za-z0-9_]*)\s*\{(.*?)\n\}",
        re.S,
    )
    for attrs, name, body in pattern.findall(source):
        values = enum_values(attrs, body)
        if values:
            enums[name] = values
    return enums


def member_name(value: str) -> str:
    rendered = re.sub(r"[^A-Za-z0-9]+", "_", value).strip("_").upper()
    if not rendered:
        raise SystemExit(f"cannot render enum member for value {value!r}")
    if rendered[0].isdigit():
        rendered = f"VALUE_{rendered}"
    return rendered


def py_tuple(values: list[str]) -> str:
    if not values:
        return "()"
    rendered = ", ".join(repr(value) for value in values)
    if len(values) == 1:
        rendered += ","
    return f"({rendered})"


def generate(source: str) -> str:
    constants = {
        "RUNTIME_CAPABILITY_VALUES": const_values(source, "RUNTIME_CAPABILITY_VALUES"),
        "INSTALL_CAPABILITY_VALUES": const_values(source, "INSTALL_CAPABILITY_VALUES"),
        "PLANNER_OUTCOME_VALUES": const_values(source, "PLANNER_OUTCOME_VALUES"),
        "PROOF_SCOPE_VALUES": const_values(source, "PROOF_SCOPE_VALUES"),
    }
    enums = parse_enums(source)
    schema_version_match = re.search(
        r"pub const CAPABILITY_SCHEMA_VERSION:\s*u16\s*=\s*(\d+);",
        source,
    )
    if not schema_version_match:
        raise SystemExit("missing CAPABILITY_SCHEMA_VERSION")

    lines: list[str] = [
        "# SPDX-FileCopyrightText: 2026 D-Central Technologies <dev@d-central.tech>",
        "# SPDX-License-Identifier: GPL-3.0-only",
        "",
        '"""Generated Python mirror for dcent-schema capability contracts."""',
        "",
        "from __future__ import annotations",
        "",
        "from enum import Enum",
        "",
        "",
        "class _VocabEnum(str, Enum):",
        "    def __str__(self) -> str:",
        "        return str.__str__(self)",
        "",
        "    def __format__(self, spec: str) -> str:",
        "        return str.__format__(self, spec)",
        "",
        "",
        f"CAPABILITY_SCHEMA_VERSION = {schema_version_match.group(1)}",
        "",
    ]

    exported: list[str] = ["CAPABILITY_SCHEMA_VERSION"]
    for const_name, values in constants.items():
        lines.append(f"{const_name} = {py_tuple(values)}")
        exported.append(const_name)
    lines.append("")

    const_backed = {
        "RuntimeCapability": "RUNTIME_CAPABILITY_VALUES",
        "InstallCapability": "INSTALL_CAPABILITY_VALUES",
        "PlannerOutcome": "PLANNER_OUTCOME_VALUES",
        "ProofScope": "PROOF_SCOPE_VALUES",
    }

    for enum_name, values in enums.items():
        if enum_name in const_backed:
            values = constants[const_backed[enum_name]]
        lines.append(f"class {enum_name}(_VocabEnum):")
        for value in values:
            lines.append(f"    {member_name(value)} = {value!r}")
        lines.append("")
        exported.append(enum_name)

    lines.append("__all__ = (")
    for name in exported:
        lines.append(f"    {name!r},")
    lines.append(")")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    source = args.input.read_text(encoding="utf-8")
    rendered = generate(source)
    if args.check:
        existing = args.output.read_text(encoding="utf-8") if args.output.exists() else ""
        if existing != rendered:
            print(f"generated capability Python is stale: {args.output}", file=sys.stderr)
            return 1
        print(f"CAPABILITY_PY_OK {args.output}")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(rendered, encoding="utf-8", newline="\n")
    print(f"WROTE {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
