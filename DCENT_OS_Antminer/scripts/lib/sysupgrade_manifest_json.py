#!/usr/bin/env python3
"""Deterministic JSON and version admission for sysupgrade manifests.

Shell tools must not make mutation-authority decisions by grepping JSON.  This
helper first applies one semantic JSON parse that rejects decoded duplicate
keys and non-canonical (escaped or non-ASCII) member names.  The latter keeps
the existing small POSIX readers safe until every consumer uses typed queries.

The version comparator deliberately avoids awk/IEEE-754 numeric conversion.
It compares bounded digit strings and prerelease identifiers directly, so its
result is identical on hosts and BusyBox-class targets.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

MAX_MANIFEST_BYTES = 256 * 1024
MAX_JSON_DEPTH = 32
MAX_JSON_NODES = 4096
MAX_VERSION_BYTES = 128
MAX_VERSION_PARTS = 16
MAX_VERSION_PART_BYTES = 32

KEY_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_.-]*\Z")
DECIMAL = r"(?:0|[1-9][0-9]*)"
IDENTIFIER = r"[0-9A-Za-z-]+"
VERSION_RE = re.compile(
    rf"v?({DECIMAL}\.{DECIMAL}(?:\.{DECIMAL})?)"
    rf"(?:-({IDENTIFIER}(?:\.{IDENTIFIER})*))?"
    rf"(?:\+({IDENTIFIER}(?:\.{IDENTIFIER})*))?\Z"
)


class AdmissionError(ValueError):
    """Manifest or version input is outside the admitted language."""


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise AdmissionError(f"duplicate decoded JSON member name: {key!r}")
        result[key] = value
    return result


def _reject_constant(token: str) -> None:
    raise AdmissionError(f"non-standard JSON numeric constant: {token}")


def _raw_member_names(text: str) -> list[tuple[str, bool]]:
    """Return raw JSON string tokens followed by ':' and whether they escape.

    ``json.loads`` has already proved the document grammar.  Therefore a JSON
    string token followed by optional whitespace and ``:`` is an object member
    name.  Keeping this lexical check separate lets the semantic parser detect
    decoded duplicates while this pass rejects spelling aliases such as
    ``"versi\\u006fn"`` that byte-oriented shell readers cannot see.
    """

    names: list[tuple[str, bool]] = []
    index = 0
    length = len(text)
    while index < length:
        if text[index] != '"':
            index += 1
            continue
        index += 1
        start = index
        escaped = False
        while index < length:
            char = text[index]
            if char == "\\":
                escaped = True
                index += 2
                continue
            if char == '"':
                break
            index += 1
        if index >= length:
            raise AdmissionError("unterminated JSON string")
        raw = text[start:index]
        index += 1
        probe = index
        while probe < length and text[probe] in " \t\r\n":
            probe += 1
        if probe < length and text[probe] == ":":
            names.append((raw, escaped))
    return names


def _bounded_tree(value: Any, depth: int = 0) -> int:
    if depth > MAX_JSON_DEPTH:
        raise AdmissionError(f"JSON nesting exceeds {MAX_JSON_DEPTH}")
    nodes = 1
    if isinstance(value, dict):
        for child in value.values():
            nodes += _bounded_tree(child, depth + 1)
    elif isinstance(value, list):
        for child in value:
            nodes += _bounded_tree(child, depth + 1)
    if nodes > MAX_JSON_NODES:
        raise AdmissionError(f"JSON node count exceeds {MAX_JSON_NODES}")
    return nodes


def admit_manifest(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise AdmissionError(f"cannot read manifest {path}: {exc}") from exc
    if not raw:
        raise AdmissionError("manifest is empty")
    if len(raw) > MAX_MANIFEST_BYTES:
        raise AdmissionError(
            f"manifest exceeds {MAX_MANIFEST_BYTES} bytes: {len(raw)}"
        )
    try:
        text = raw.decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise AdmissionError("manifest is not strict UTF-8") from exc
    if "\\" in text:
        raise AdmissionError(
            "manifest JSON must not contain escape sequences; authority fields "
            "require literal canonical spelling"
        )
    try:
        value = json.loads(
            text,
            object_pairs_hook=_unique_object,
            parse_constant=_reject_constant,
        )
    except (json.JSONDecodeError, RecursionError, AdmissionError) as exc:
        raise AdmissionError(f"invalid manifest JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise AdmissionError("manifest root must be a JSON object")
    _bounded_tree(value)
    for raw_name, escaped in _raw_member_names(text):
        if escaped:
            raise AdmissionError(
                f"JSON member names must use literal canonical spelling: {raw_name!r}"
            )
        if not KEY_RE.fullmatch(raw_name):
            raise AdmissionError(
                f"JSON member name is outside the canonical ASCII grammar: {raw_name!r}"
            )
    return value


def _bounded_parts(value: str, separator: str) -> list[str]:
    parts = re.split(separator, value)
    if len(parts) > MAX_VERSION_PARTS:
        raise AdmissionError(f"version contains more than {MAX_VERSION_PARTS} parts")
    if any(len(part.encode("ascii")) > MAX_VERSION_PART_BYTES for part in parts):
        raise AdmissionError(
            f"version part exceeds {MAX_VERSION_PART_BYTES} ASCII bytes"
        )
    return parts


def parse_version(value: str) -> tuple[list[str], list[str]]:
    try:
        encoded = value.encode("ascii")
    except UnicodeEncodeError as exc:
        raise AdmissionError("version must be ASCII") from exc
    if not encoded or len(encoded) > MAX_VERSION_BYTES:
        raise AdmissionError(
            f"version length must be 1..{MAX_VERSION_BYTES} ASCII bytes"
        )
    match = VERSION_RE.fullmatch(value)
    if match is None:
        raise AdmissionError(f"version is outside the canonical grammar: {value!r}")
    # The complete value bound limits core-number size. Do not impose the
    # suffix identifier limit here: core numbers are arbitrary-precision
    # canonical decimals and may legitimately exceed 32 digits.
    release = match.group(1).split(".")
    prerelease = (
        _bounded_parts(match.group(2), r"\.") if match.group(2) is not None else []
    )
    if match.group(3) is not None:
        _bounded_parts(match.group(3), r"\.")
    for identifier in prerelease:
        if identifier.isdigit() and len(identifier) > 1 and identifier.startswith("0"):
            raise AdmissionError(
                "numeric prerelease identifiers must not contain leading zeroes"
            )
    return release, prerelease


def _normalize_digits(value: str) -> str:
    normalized = value.lstrip("0")
    return normalized or "0"


def _compare_digits(left: str, right: str) -> int:
    left = _normalize_digits(left)
    right = _normalize_digits(right)
    if len(left) != len(right):
        return 1 if len(left) > len(right) else -1
    return (left > right) - (left < right)


def _compare_release(left: list[str], right: list[str]) -> int:
    width = max(len(left), len(right))
    for index in range(width):
        result = _compare_digits(
            left[index] if index < len(left) else "0",
            right[index] if index < len(right) else "0",
        )
        if result:
            return result
    return 0


def _compare_prerelease(left: list[str], right: list[str]) -> int:
    if not left and not right:
        return 0
    if not left:
        return 1
    if not right:
        return -1
    for left_part, right_part in zip(left, right):
        left_numeric = left_part.isdigit()
        right_numeric = right_part.isdigit()
        if left_numeric and right_numeric:
            result = _compare_digits(left_part, right_part)
        elif left_numeric != right_numeric:
            result = -1 if left_numeric else 1
        else:
            result = (left_part > right_part) - (left_part < right_part)
        if result:
            return result
    return (len(left) > len(right)) - (len(left) < len(right))


def compare_versions(candidate: str, current: str) -> int:
    candidate_release, candidate_pre = parse_version(candidate)
    current_release, current_pre = parse_version(current)
    release_result = _compare_release(candidate_release, current_release)
    if release_result:
        return release_result
    return _compare_prerelease(candidate_pre, current_pre)


def read_version_file(path: Path) -> str:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise AdmissionError(f"cannot read version file {path}: {exc}") from exc
    if b"\r" in raw or b"\0" in raw:
        raise AdmissionError("version file contains CR or NUL bytes")
    if raw.endswith(b"\n"):
        raw = raw[:-1]
    if not raw or b"\n" in raw:
        raise AdmissionError("version file must contain exactly one non-empty line")
    try:
        value = raw.decode("ascii", errors="strict")
    except UnicodeDecodeError as exc:
        raise AdmissionError("version file must contain ASCII") from exc
    parse_version(value)
    return value


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("manifest", type=Path)
    compare_parser = subparsers.add_parser("compare-version")
    compare_parser.add_argument("candidate")
    compare_parser.add_argument("current")
    read_parser = subparsers.add_parser("read-version-file")
    read_parser.add_argument("path", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "validate":
            admit_manifest(args.manifest)
        elif args.command == "compare-version":
            print(compare_versions(args.candidate, args.current))
        else:
            print(read_version_file(args.path))
    except AdmissionError as exc:
        print(f"sysupgrade manifest admission: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
