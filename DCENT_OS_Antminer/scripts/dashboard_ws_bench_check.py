#!/usr/bin/env python3
"""Operator-run WebSocket liveness probe for a bench dashboard unit."""

from __future__ import annotations

import argparse
import base64
import getpass
import hashlib
import json
import os
import socket
import ssl
import struct
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Optional
from urllib.parse import parse_qsl, urlencode, urlparse, urlunparse


WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


@dataclass
class WsCheck:
    name: str
    ok: bool
    detail: str


@dataclass
class WsFrameSummary:
    opcode: int
    kind: str
    bytes: int
    message_type: Optional[str]


@dataclass
class WsReport:
    target: str
    ok: bool
    http_status: Optional[int]
    frames: List[WsFrameSummary]
    checks: List[WsCheck]


def add_check(checks: List[WsCheck], name: str, ok: bool, detail: str) -> None:
    checks.append(WsCheck(name=name, ok=ok, detail=detail))


def normalize_ws_url(target: str, token: Optional[str] = None, default_port: int = 8080) -> str:
    candidate = target.strip()
    if not candidate:
        raise ValueError("target is required")

    if "://" not in candidate:
        candidate = f"ws://{candidate}"

    parsed = urlparse(candidate)
    scheme = parsed.scheme.lower()
    if scheme == "http":
        scheme = "ws"
    elif scheme == "https":
        scheme = "wss"
    if scheme not in {"ws", "wss"}:
        raise ValueError("target must use ws, wss, http, or https")
    if not parsed.hostname:
        raise ValueError("target must include a host")

    netloc = parsed.netloc
    if parsed.port is None:
        host = parsed.hostname
        if ":" in host and not host.startswith("["):
            host = f"[{host}]"
        netloc = f"{host}:{default_port}"

    path = parsed.path or "/ws"
    query = parse_qsl(parsed.query, keep_blank_values=True)
    if token:
        query = [(k, v) for k, v in query if k != "token"]
        query.append(("token", token))

    return urlunparse((scheme, netloc, path, "", urlencode(query), ""))


def redact_ws_url(url: str) -> str:
    parsed = urlparse(url)
    query = [
        (key, "<redacted>" if key == "token" else value)
        for key, value in parse_qsl(parsed.query, keep_blank_values=True)
    ]
    return urlunparse((parsed.scheme, parsed.netloc, parsed.path, "", urlencode(query), ""))


def expected_accept(key: str) -> str:
    digest = hashlib.sha1((key + WS_GUID).encode("ascii")).digest()
    return base64.b64encode(digest).decode("ascii")


def read_until(sock: socket.socket, marker: bytes, deadline: float, limit: int = 65536) -> bytes:
    data = bytearray()
    while marker not in data:
        if time.monotonic() > deadline:
            raise TimeoutError("timed out waiting for WebSocket handshake")
        chunk = sock.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
        if len(data) > limit:
            raise ValueError("WebSocket handshake response exceeded limit")
    return bytes(data)


def read_exact(
    sock: socket.socket,
    size: int,
    deadline: float,
    buffered: Optional[bytearray] = None,
) -> bytes:
    data = bytearray()
    if buffered:
        take = min(size, len(buffered))
        data.extend(buffered[:take])
        del buffered[:take]
    while len(data) < size:
        if time.monotonic() > deadline:
            raise TimeoutError("timed out waiting for WebSocket frame")
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise EOFError("socket closed while reading WebSocket frame")
        data.extend(chunk)
    return bytes(data)


def parse_headers(raw: bytes) -> tuple[int, Dict[str, str]]:
    head = raw.split(b"\r\n\r\n", 1)[0]
    lines = head.decode("iso-8859-1").split("\r\n")
    if not lines or not lines[0].startswith("HTTP/"):
        raise ValueError("invalid HTTP response from WebSocket endpoint")
    parts = lines[0].split(" ", 2)
    if len(parts) < 2 or not parts[1].isdigit():
        raise ValueError(f"invalid HTTP status line: {lines[0]!r}")
    headers: Dict[str, str] = {}
    for line in lines[1:]:
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        headers[key.strip().lower()] = value.strip()
    return int(parts[1]), headers


def frame_message_type(opcode: int, payload: bytes) -> Optional[str]:
    if opcode != 1:
        return None
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    if isinstance(parsed, dict) and isinstance(parsed.get("type"), str):
        return parsed["type"]
    return None


def frame_kind(opcode: int) -> str:
    return {
        1: "text",
        2: "binary",
        8: "close",
        9: "ping",
        10: "pong",
    }.get(opcode, f"opcode-{opcode}")


def send_masked_control(sock: socket.socket, opcode: int, payload: bytes) -> None:
    mask = os.urandom(4)
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    sock.sendall(bytes([0x80 | opcode, 0x80 | len(payload)]) + mask + masked)


def read_frame(
    sock: socket.socket,
    deadline: float,
    buffered: Optional[bytearray] = None,
) -> WsFrameSummary:
    first = read_exact(sock, 2, deadline, buffered)
    opcode = first[0] & 0x0F
    masked = (first[1] & 0x80) != 0
    length = first[1] & 0x7F
    if length == 126:
        length = struct.unpack("!H", read_exact(sock, 2, deadline, buffered))[0]
    elif length == 127:
        length = struct.unpack("!Q", read_exact(sock, 8, deadline, buffered))[0]

    mask = read_exact(sock, 4, deadline, buffered) if masked else b""
    payload = read_exact(sock, length, deadline, buffered) if length else b""
    if masked:
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))

    return WsFrameSummary(
        opcode=opcode,
        kind=frame_kind(opcode),
        bytes=len(payload),
        message_type=frame_message_type(opcode, payload),
    )


def open_socket(parsed, timeout: float) -> socket.socket:
    port = parsed.port or (443 if parsed.scheme == "wss" else 80)
    raw = socket.create_connection((parsed.hostname, port), timeout=timeout)
    raw.settimeout(min(timeout, 5.0))
    if parsed.scheme == "wss":
        context = ssl.create_default_context()
        return context.wrap_socket(raw, server_hostname=parsed.hostname)
    return raw


def run_ws_check(target: str, token: Optional[str], timeout: float = 10.0, frames: int = 1) -> WsReport:
    url = normalize_ws_url(target, token=token)
    parsed = urlparse(url)
    checks: List[WsCheck] = []
    frame_summaries: List[WsFrameSummary] = []
    status: Optional[int] = None

    key = base64.b64encode(os.urandom(16)).decode("ascii")
    host = parsed.hostname or ""
    if parsed.port:
        host = f"{host}:{parsed.port}"
    path = parsed.path or "/ws"
    if parsed.query:
        path = f"{path}?{parsed.query}"
    request = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Origin: http://{parsed.hostname or 'localhost'}\r\n"
        "\r\n"
    ).encode("ascii")

    deadline = time.monotonic() + timeout
    try:
        with open_socket(parsed, timeout) as sock:
            sock.sendall(request)
            raw_headers = read_until(sock, b"\r\n\r\n", deadline)
            _head, _separator, leftover = raw_headers.partition(b"\r\n\r\n")
            frame_buffer = bytearray(leftover)
            status, headers = parse_headers(raw_headers)
            add_check(checks, "ws-status-101", status == 101, f"status={status}")
            add_check(
                checks,
                "ws-upgrade-header",
                headers.get("upgrade", "").lower() == "websocket",
                f"upgrade={headers.get('upgrade')!r}",
            )
            add_check(
                checks,
                "ws-connection-header",
                "upgrade" in headers.get("connection", "").lower(),
                f"connection={headers.get('connection')!r}",
            )
            add_check(
                checks,
                "ws-accept-header",
                headers.get("sec-websocket-accept") == expected_accept(key),
                "accept header matches challenge",
            )

            if status != 101:
                return WsReport(
                    target=redact_ws_url(url),
                    ok=False,
                    http_status=status,
                    frames=frame_summaries,
                    checks=checks,
                )

            wanted = max(frames, 1)
            while len([f for f in frame_summaries if f.opcode in {1, 2}]) < wanted:
                frame = read_frame(sock, deadline, frame_buffer)
                if frame.opcode == 9:
                    send_masked_control(sock, 10, b"")
                    continue
                frame_summaries.append(frame)
                if frame.opcode == 8:
                    break

            data_frames = [frame for frame in frame_summaries if frame.opcode in {1, 2}]
            add_check(
                checks,
                "ws-frame-received",
                len(data_frames) >= wanted,
                f"data_frames={len(data_frames)} expected={wanted}",
            )
    except (OSError, TimeoutError, EOFError, ValueError) as exc:
        add_check(checks, "ws-probe", False, str(exc))

    return WsReport(
        target=redact_ws_url(url),
        ok=all(check.ok for check in checks),
        http_status=status,
        frames=frame_summaries,
        checks=checks,
    )


def print_text(report: WsReport) -> None:
    print(f"Dashboard WebSocket bench check: {'PASS' if report.ok else 'FAIL'}")
    print(f"target: {report.target}")
    if report.http_status is not None:
        print(f"http_status: {report.http_status}")
    for frame in report.frames:
        label = frame.message_type or frame.kind
        print(f"frame: {label} opcode={frame.opcode} bytes={frame.bytes}")
    for check in report.checks:
        marker = "PASS" if check.ok else "FAIL"
        print(f"{marker} {check.name}: {check.detail}")


def write_json_report(path: Path, report: WsReport) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(report), indent=2, sort_keys=True) + "\n", encoding="utf-8")


def resolve_token(args) -> Optional[str]:
    if args.token:
        return args.token
    if args.token_env:
        token = os.environ.get(args.token_env)
        if token:
            return token
    if args.prompt_token:
        token = getpass.getpass("WebSocket session token: ")
        return token.strip() or None
    return None


def main(argv: Optional[Iterable[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Verify a bench miner accepts a real dashboard WebSocket upgrade and "
            "delivers live frames. This is operator-run live validation."
        )
    )
    parser.add_argument("target", help="Miner host or ws URL, for example 203.0.113.50")
    parser.add_argument("--token", help="Bearer session token; prefer --prompt-token")
    parser.add_argument(
        "--token-env",
        default="DCENTOS_SESSION_TOKEN",
        help="Environment variable containing the bearer token",
    )
    parser.add_argument(
        "--prompt-token",
        action="store_true",
        help="Prompt for the bearer token without echoing it",
    )
    parser.add_argument("--timeout", type=float, default=10.0, help="Probe timeout in seconds")
    parser.add_argument("--frames", type=int, default=1, help="Number of data frames to wait for")
    parser.add_argument("--json", action="store_true", help="Emit a machine-readable report")
    parser.add_argument("--output", help="Write the machine-readable JSON report to this path")
    args = parser.parse_args(list(argv) if argv is not None else None)

    try:
        report = run_ws_check(
            args.target,
            token=resolve_token(args),
            timeout=args.timeout,
            frames=args.frames,
        )
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    if args.output:
        write_json_report(Path(args.output), report)

    if args.json:
        print(json.dumps(asdict(report), indent=2, sort_keys=True))
    else:
        print_text(report)
    return 0 if report.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
