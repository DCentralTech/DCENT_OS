#!/usr/bin/env python3
"""Fail-closed validation for retained full-daemon simulator evidence."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


MODEL_GEOMETRY: dict[str, tuple[int, int]] = {
    # Keep this evidence contract aligned with SimBoardProfile::for_model.
    "s9": (0x1387, 63),
    "s17": (0x1397, 48),
    "s17pro": (0x1397, 48),
    "t17": (0x1397, 30),
    "s19pro": (0x1398, 114),
    "s19jpro": (0x1362, 126),
    "s19xp": (0x1366, 110),
    "s19kpro": (0x1366, 77),
    "s21": (0x1368, 108),
    "s21pro": (0x1370, 65),
}
MCP_PROTOCOL_VERSION = "2024-11-05"
MCP_PROFILE_ID = "dcent.cross-firmware.minimal.v1"
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
PLL_FAILURE = re.compile(r"readback\s+(?:TIMEOUT|MISMATCH)\b")


class EvidenceError(ValueError):
    """Evidence is missing, malformed, ambiguous, or contradicts the request."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle, object_pairs_hook=_reject_duplicate_keys)


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must be a JSON object")
    return value


def _exact_int(value: Any, expected: int, label: str) -> None:
    if type(value) is not int or value != expected:
        raise EvidenceError(f"{label} must be integer {expected}, got {value!r}")


def _positive_int(value: Any, label: str, *, allow_zero: bool = False) -> int:
    minimum = 0 if allow_zero else 1
    if type(value) is not int or value < minimum:
        qualifier = "non-negative" if allow_zero else "positive"
        raise EvidenceError(f"{label} must be a {qualifier} integer, got {value!r}")
    return value


def _geometry(model: str) -> tuple[int, int]:
    try:
        return MODEL_GEOMETRY[model]
    except KeyError as error:
        raise EvidenceError(f"unsupported integrated simulator model: {model!r}") from error


def validate_status(model: str, path: Path) -> str:
    _, expected_chips = _geometry(model)
    data = _object(_load_json(path), "status response")
    _exact_int(data.get("accepted"), 1, "status.accepted")
    _exact_int(data.get("rejected"), 0, "status.rejected")

    chains = data.get("chains")
    if not isinstance(chains, list) or len(chains) != 3:
        raise EvidenceError("status.chains must contain exactly three chains")
    for expected_id, raw_chain in enumerate(chains):
        chain = _object(raw_chain, f"status.chains[{expected_id}]")
        _exact_int(chain.get("id"), expected_id, f"status.chains[{expected_id}].id")
        _exact_int(
            chain.get("chips"),
            expected_chips,
            f"status.chains[{expected_id}].chips",
        )
        if chain.get("status") != "simulated-ready":
            raise EvidenceError(
                f"status.chains[{expected_id}].status must be 'simulated-ready'"
            )

    pool = _object(data.get("pool"), "status.pool")
    if pool.get("url") != "loopback://mock-v1" or pool.get("status") != "connected":
        raise EvidenceError("status.pool must be the connected loopback mock-v1 pool")
    return f"SIM_REST_EVIDENCE_OK model={model} chains=3 chips={expected_chips} accepted=1"


def validate_rust_mcp(path: Path) -> str:
    data = _object(_load_json(path), "Rust MCP response")
    if "error" in data:
        raise EvidenceError("Rust MCP initialize response contains an error")
    if data.get("jsonrpc") != "2.0":
        raise EvidenceError("Rust MCP jsonrpc must be '2.0'")
    _exact_int(data.get("id"), 1, "Rust MCP id")
    result = _object(data.get("result"), "Rust MCP result")
    if result.get("protocolVersion") != MCP_PROTOCOL_VERSION:
        raise EvidenceError("Rust MCP protocolVersion mismatch")
    if result.get("profile") != MCP_PROFILE_ID:
        raise EvidenceError("Rust MCP profile mismatch")
    if result.get("transport") != "streamable-http" or result.get("readOnly") is not True:
        raise EvidenceError("Rust MCP response must prove read-only streamable-http")
    capabilities = _object(result.get("capabilities"), "Rust MCP capabilities")
    tools = _object(capabilities.get("tools"), "Rust MCP tools capability")
    if tools.get("listChanged") is not False:
        raise EvidenceError("Rust MCP tools.listChanged must be false")
    server = _object(result.get("serverInfo"), "Rust MCP serverInfo")
    if server.get("name") != "dcentos-dcentrald":
        raise EvidenceError("Rust MCP server name mismatch")
    if not isinstance(server.get("version"), str) or not server["version"].strip():
        raise EvidenceError("Rust MCP server version must be non-empty")
    return "SIM_RUST_MCP_EVIDENCE_OK port=8080 path=/mcp"


def validate_rootfs_mcp(path: Path) -> str:
    data = _object(_load_json(path), "rootfs MCP response")
    exact = {
        "name": "dcentos-mcp",
        "protocol": MCP_PROTOCOL_VERSION,
        "transport": "streamable-http",
        "profileId": MCP_PROFILE_ID,
    }
    for key, expected in exact.items():
        if data.get(key) != expected:
            raise EvidenceError(f"rootfs MCP {key} mismatch")
    if not isinstance(data.get("version"), str) or not data["version"].strip():
        raise EvidenceError("rootfs MCP version must be non-empty")
    tools = _positive_int(data.get("tools"), "rootfs MCP tools")
    _positive_int(data.get("resources"), "rootfs MCP resources", allow_zero=True)
    return f"SIM_ROOTFS_MCP_EVIDENCE_OK port=3000 tools={tools}"


def validate_daemon_log(model: str, path: Path) -> str:
    chip_id, expected_chips = _geometry(model)
    text = ANSI_ESCAPE.sub("", path.read_text(encoding="utf-8"))
    failure = PLL_FAILURE.search(text)
    if failure:
        raise EvidenceError(f"simulator PLL {failure.group(0)}")

    ready_lines = [line for line in text.splitlines() if "SIM_HAL_RUNTIME_READY" in line]
    stopped_lines = [line for line in text.splitlines() if "SIM_HAL_RUNTIME_STOPPED" in line]
    if len(ready_lines) != 1 or len(stopped_lines) != 1:
        raise EvidenceError("daemon log must contain exactly one READY and one STOPPED event")

    ready = ready_lines[0]
    stopped = stopped_lines[0]
    required_ready = (
        f'model="{model}"',
        f"chip_id=0x{chip_id:04x}",
        f"chip_count={expected_chips}",
        "accepted_shares=1",
    )
    for field in required_ready:
        if field not in ready:
            raise EvidenceError(f"READY event is missing exact field {field!r}")
    if f'model="{model}"' not in stopped:
        raise EvidenceError("STOPPED event model does not match the request")
    return (
        f"SIM_DAEMON_LOG_EVIDENCE_OK model={model} "
        f"chip_id=0x{chip_id:04x} chips={expected_chips} pll=clean"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("status", "daemon-log"):
        child = subparsers.add_parser(command)
        child.add_argument("--model", required=True)
        child.add_argument("--path", type=Path, required=True)
    for command in ("rust-mcp", "rootfs-mcp"):
        child = subparsers.add_parser(command)
        child.add_argument("--path", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "status":
            message = validate_status(args.model, args.path)
        elif args.command == "rust-mcp":
            message = validate_rust_mcp(args.path)
        elif args.command == "rootfs-mcp":
            message = validate_rootfs_mcp(args.path)
        else:
            message = validate_daemon_log(args.model, args.path)
    except (EvidenceError, OSError, UnicodeError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(message)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
