#!/usr/bin/env python3
"""Generate dashboard TypeScript for dcent-schema capability contracts."""

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
    / "dcentos"
    / "dashboard"
    / "src"
    / "api"
    / "generated"
    / "capability.ts"
)


def snake_to_camel(value: str) -> str:
    head, *tail = value.split("_")
    return head + "".join(part[:1].upper() + part[1:] for part in tail)


def camel_to_kebab(value: str) -> str:
    pieces = re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?=[A-Z]|$)", value)
    return "-".join(piece.lower() for piece in pieces)


def screaming_snake(value: str) -> str:
    return "_".join(part.upper() for part in re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?=[A-Z]|$)", value))


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


def split_type(value: str) -> str:
    return value.strip().rstrip(",")


def ts_type(rust_type: str) -> str:
    rust_type = split_type(rust_type)
    if rust_type.startswith("Option<") and rust_type.endswith(">"):
        inner = rust_type[len("Option<") : -1]
        return f"{ts_type(inner)} | null"
    if rust_type.startswith("Vec<") and rust_type.endswith(">"):
        inner = rust_type[len("Vec<") : -1]
        inner_ts = ts_type(inner)
        if " | " in inner_ts:
            inner_ts = f"({inner_ts})"
        return f"{inner_ts}[]"
    primitives = {
        "String": "string",
        "str": "string",
        "bool": "boolean",
        "u8": "number",
        "u16": "number",
        "u32": "number",
        "u64": "number",
        "i8": "number",
        "i16": "number",
        "i32": "number",
        "i64": "number",
        "f32": "number",
        "f64": "number",
    }
    return primitives.get(rust_type, rust_type)


def enum_values(name: str, attrs: str, body: str) -> list[str]:
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
    if not values:
        raise SystemExit(f"enum {name} produced no TypeScript values")
    return values


def parse_enums(source: str) -> dict[str, list[str]]:
    enums: dict[str, list[str]] = {}
    pattern = re.compile(r"((?:#\[[^\n]+\]\s*)*)pub enum ([A-Za-z][A-Za-z0-9_]*)\s*\{(.*?)\n\}", re.S)
    for attrs, name, body in pattern.findall(source):
        enums[name] = enum_values(name, attrs, body)
    return enums


def field_name(field: str, attrs: list[str], rename_all: str | None) -> str:
    renamed = serde_rename(attrs)
    if renamed is not None:
        return renamed
    if rename_all == "camelCase":
        return snake_to_camel(field)
    return field


def parse_structs(source: str) -> list[tuple[str, list[tuple[str, str]]]]:
    structs: list[tuple[str, list[tuple[str, str]]]] = []
    pattern = re.compile(r"((?:#\[[^\n]+\]\s*)*)pub struct ([A-Za-z][A-Za-z0-9_]*)\s*\{(.*?)\n\}", re.S)
    for attrs, name, body in pattern.findall(source):
        rename_all = serde_rename_all(attrs)
        fields: list[tuple[str, str]] = []
        pending_attrs: list[str] = []
        for raw in body.splitlines():
            line = raw.strip()
            if not line:
                continue
            if line.startswith("#["):
                pending_attrs.append(line)
                continue
            match = re.match(r"pub\s+([A-Za-z][A-Za-z0-9_]*)\s*:\s*(.+),", line)
            if not match:
                pending_attrs.clear()
                continue
            rust_name, rust_type = match.groups()
            fields.append((field_name(rust_name, pending_attrs, rename_all), ts_type(rust_type)))
            pending_attrs.clear()
        if fields:
            structs.append((name, fields))
    return structs


def ts_const(name: str, values: list[str]) -> str:
    rendered = ", ".join(f"'{value}'" for value in values)
    return f"export const {name} = [{rendered}] as const;\n"


def generate(source: str) -> str:
    constants = {
        "RUNTIME_CAPABILITY_VALUES": const_values(source, "RUNTIME_CAPABILITY_VALUES"),
        "INSTALL_CAPABILITY_VALUES": const_values(source, "INSTALL_CAPABILITY_VALUES"),
        "PLANNER_OUTCOME_VALUES": const_values(source, "PLANNER_OUTCOME_VALUES"),
        "PROOF_SCOPE_VALUES": const_values(source, "PROOF_SCOPE_VALUES"),
    }
    enums = parse_enums(source)
    structs = parse_structs(source)
    schema_version_match = re.search(r"pub const CAPABILITY_SCHEMA_VERSION:\s*u16\s*=\s*(\d+);", source)
    if not schema_version_match:
        raise SystemExit("missing CAPABILITY_SCHEMA_VERSION")

    lines: list[str] = [
        "// Generated by projects/dcent-schema/scripts/generate_capability_ts.py.",
        "// Source of truth: projects/dcent-schema/src/capability.rs.",
        "// Do not edit by hand; run the generator instead.",
        "/* eslint-disable */",
        "",
        f"export const CAPABILITY_SCHEMA_VERSION = {schema_version_match.group(1)} as const;",
        "",
    ]

    for const_name in (
        "RUNTIME_CAPABILITY_VALUES",
        "INSTALL_CAPABILITY_VALUES",
        "PLANNER_OUTCOME_VALUES",
        "PROOF_SCOPE_VALUES",
    ):
        lines.append(ts_const(const_name, constants[const_name]).rstrip())
        type_name = "".join(part.title() for part in const_name.lower().split("_")[:-1])
        if const_name == "RUNTIME_CAPABILITY_VALUES":
            type_name = "RuntimeCapability"
        elif const_name == "INSTALL_CAPABILITY_VALUES":
            type_name = "InstallCapability"
        elif const_name == "PLANNER_OUTCOME_VALUES":
            type_name = "PlannerOutcome"
        elif const_name == "PROOF_SCOPE_VALUES":
            type_name = "ProofScope"
        lines.append(f"export type {type_name} = typeof {const_name}[number];")
        lines.append("")

    const_backed_enums = {
        "RuntimeCapability",
        "InstallCapability",
        "PlannerOutcome",
        "ProofScope",
    }
    for name, values in enums.items():
        if name in const_backed_enums:
            continue
        const_name = f"{screaming_snake(name)}_VALUES"
        lines.append(ts_const(const_name, values).rstrip())
        lines.append(f"export type {name} = typeof {const_name}[number];")
        lines.append("")

    skip_structs = set()
    for name, fields in structs:
        if name in skip_structs:
            continue
        lines.append(f"export interface {name} {{")
        for name_ts, type_ts in fields:
            lines.append(f"  {name_ts}: {type_ts};")
        lines.append("}")
        lines.append("")

    if "pub type HardwareCapabilityDescriptor = DeviceCapabilityDescriptor;" in source:
        lines.append("export type HardwareCapabilityDescriptor = DeviceCapabilityDescriptor;")
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


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
            print(f"generated capability TypeScript is stale: {args.output}", file=sys.stderr)
            return 1
        print(f"CAPABILITY_TS_OK {args.output}")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(rendered, encoding="utf-8", newline="\n")
    print(f"WROTE {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
