#!/usr/bin/env python3
"""
DCENTos MCP Server — Model Context Protocol for AI-Assisted Mining
D-Central Technologies, 2026

MCP (Model Context Protocol) is an open standard by Anthropic that allows
AI assistants (Claude, ChatGPT, etc.) to connect to external tools and data.

This server exposes the miner's hardware as MCP "tools" and "resources",
enabling any MCP-compatible AI to:
  - Read real-time sensor data (temperatures, voltages, hash rates)
  - Run hardware diagnostics
  - Control fan speed and mining parameters
  - Troubleshoot issues conversationally
  - Monitor fleet status

Protocol: JSON-RPC 2.0 over HTTP (Streamable HTTP transport)
Port: 3000 (configurable)
Spec: https://modelcontextprotocol.io/

~28 tools exposed — see TOOLS dict for full list.

Resources exposed:
  miner://status       — Live system status (subscribable)
  miner://temperatures — Temperature readings
  miner://chains       — Chain status
  miner://hashrate     — Live hashrate from dcentrald
  miner://config       — Current dcentrald config
"""

import http.server
import json
import os
import re
import subprocess
import sys
import time
import socket
import argparse

VERSION = "0.3.0"
PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "dcentos-mcp"
PROFILE_ID = "dcent.cross-firmware.minimal.v1"

# MCP-3 (P3 hardening): hard cap on the JSON-RPC request body size. do_POST
# reads exactly Content-Length bytes; without a ceiling a single oversized POST
# is a trivial memory-exhaustion vector (especially under --bind 0.0.0.0). MCP
# requests are tiny (the largest legitimate one is a tool call with a few hex
# args), so 1 MiB is generous headroom while closing the DoS surface.
MAX_REQUEST_BODY_BYTES = 1 << 20  # 1 MiB


# =============================================================================
# MCP-2 ( privacy): wallet + pool-credential sanitizer for READ tools.
#
# The READ tools (get_config / tail_log / grep_log / live_stats) are NEVER
# behind the release bearer-token gate (reads stay open on dev AND release), so
# any operator BTC payout address (the Stratum V1 `worker`) or pool URL
# `user:pass@` credential they return would leak unauthenticated to anyone who
# can reach :3000. This mirrors the load-bearing Rust sanitizers
# `dcentrald_common::wallet_mask::{mask_wallet, mask_in_string}` and
# `dcentrald_stratum::pool_api::sanitize_pool_url` so this parallel Python read
# path masks the same surfaces. Detection rules are byte-aligned with
# wallet_mask.rs (bech32/bech32m HRPs, base58 P2PKH/P2SH first-bytes, 32/40/64
# hex runs) and the mask form is `<first6>…<last4>` (U+2026) for inputs >= 12.
# =============================================================================

# bech32 / bech32m HRP set + data charset (wallet_mask.rs BECH32_HRPS /
# BECH32_CHARSET). Word-bounded both sides; length re-validated in _mask_bech32.
_BECH32_RE = re.compile(
    r'(?<![A-Za-z0-9])'
    r'(?:bc|tb|bcrt|bsv|ltc|tltc)1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{6,87}'
    r'(?![A-Za-z0-9])'
)
# base58 P2PKH / P2SH (wallet_mask.rs BASE58_FIRST_BYTES + BASE58_CHARSET,
# total length 25..35 = first byte + 24..34 trailing).
_BASE58_RE = re.compile(
    r'(?<![A-Za-z0-9])'
    r'[123mn][123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]{24,34}'
    r'(?![A-Za-z0-9])'
)
# long hex runs (wallet_mask.rs matches_hex_address: exactly 32/40/64 hex chars).
_HEX_RE = re.compile(
    r'(?<![A-Za-z0-9])'
    r'(?:[0-9a-fA-F]{64}|[0-9a-fA-F]{40}|[0-9a-fA-F]{32})'
    r'(?![A-Za-z0-9])'
)
# scheme://[user[:pass]@]authority — strip the userinfo (sanitize_pool_url). The
# userinfo char class excludes `/ @ whitespace " '` so it never crosses into the
# path/query (mirrors the authority-only stripping in pool_api::sanitize_pool_url).
_URL_CRED_RE = re.compile(r'(://)[^/@\s"\']+@')


def _mask_wallet(addr):
    """Mirror dcentrald_common::wallet_mask::mask_wallet — `<first6>…<last4>`
    for inputs >= 12 chars, otherwise unchanged."""
    if len(addr) < 12:
        return addr
    return addr[:6] + "…" + addr[-4:]


def _mask_bech32(match):
    s = match.group(0)
    # Enforce BIP-173 total length 14..90 (the regex floor is shorter for the
    # short `bc1` HRP); leave anything outside that window untouched.
    return _mask_wallet(s) if 14 <= len(s) <= 90 else s


def sanitize_secrets(text):
    """Strip pool-URL credentials and mask wallet addresses in arbitrary text.

    Mirrors the Rust pair sanitize_pool_url (strip `user:pass@`) +
    wallet_mask::mask_in_string (mask bech32 / base58 / long-hex). Applied to
    every READ tool that can surface the operator's payout address or pool
    credentials. Non-strings pass through unchanged.
    """
    if not isinstance(text, str) or not text:
        return text
    text = _URL_CRED_RE.sub(r"\1", text)
    text = _BECH32_RE.sub(_mask_bech32, text)
    text = _BASE58_RE.sub(lambda m: _mask_wallet(m.group(0)), text)
    text = _HEX_RE.sub(lambda m: _mask_wallet(m.group(0)), text)
    return text


def sanitize_obj(obj):
    """Recursively apply sanitize_secrets() to every string in a JSON-ish
    structure (dict / list / str). Used for proxied JSON responses such as
    live_stats so a pool URL or worker nested anywhere is masked."""
    if isinstance(obj, str):
        return sanitize_secrets(obj)
    if isinstance(obj, dict):
        return {k: sanitize_obj(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [sanitize_obj(v) for v in obj]
    return obj


def minimal_profile():
    return {
        "id": PROFILE_ID,
        "protocolVersion": PROTOCOL_VERSION,
        "transport": "streamable-http",
        "tools": [
            {
                "name": "get_status",
                "description": "Read the current miner status summary",
                "legacyAliases": ["get_system_status", "live_stats", "get_hashrate"],
                "write": False,
            },
            {
                "name": "get_device_info",
                "description": "Read the current device identity and ASIC metadata",
                # Aliases MUST stay byte-aligned with the Rust source of truth in
                # projects/dcent-schema/src/mcp.rs::minimal_profile() (the cross-
                # firmware canonical registry, mcp-auth-contract.md §5.1). Both
                # get_asic_info and get_config are accepted inbound, mapped to the
                # canonical get_device_info handler — never emitted as standalone tools.
                "legacyAliases": ["get_asic_info", "get_config"],
                "write": False,
            },
            {
                "name": "get_swarm_status",
                "description": "Read the shared swarm and discovery status",
                # get_swarm is the canonical-as-alias accepted inbound (Rust source
                # of truth: dcent-schema minimal_profile()); never emitted standalone.
                "legacyAliases": ["get_swarm"],
                "write": False,
            },
            {
                "name": "identify_device",
                "description": "Toggle the physical identify signal for the device",
                "legacyAliases": [],
                "write": True,
            },
            {
                "name": "restart_mining",
                "description": "Restart mining without redefining the pool configuration",
                "legacyAliases": ["service_control"],
                "write": True,
            },
            {
                "name": "set_pool",
                "description": "Update the active mining pool target",
                "legacyAliases": ["pool_switch"],
                "write": True,
            },
        ],
    }


def run_cmd(cmd, timeout=5):
    """Run a command and return stdout (stripped), or "" on failure.

    FO-1 (SEC-W24-3, 2026-05-22): `shell=True` is dropped UNCONDITIONALLY. The
    previous implementation passed the whole command string to a shell, which
    was a latent injection footgun (one careless future tool addition = RCE).
    This shell-free runner parses the limited shell features the tools actually
    use — `2>/dev/null` stderr suppression and single `|` pipelines — and runs
    each segment as an argv list via `shell=False`. Pure-argv callers can pass
    a list directly and skip parsing entirely.

    Supported (no shell): plain argv, trailing `2>/dev/null` / `2>&1`, and
    `a | b | c` pipelines (each stage tokenized with shlex). Anything else that
    would need a real shell (`&&`, `;`, `$()`, backticks, globbing) is NOT
    supported by design — no tool uses them, and refusing them keeps the
    injection surface closed.
    """
    try:
        if isinstance(cmd, (list, tuple)):
            result = subprocess.run(
                list(cmd), shell=False, capture_output=True, text=True, timeout=timeout
            )
            return result.stdout.strip()
        out, _ = _run_pipeline(cmd, timeout=timeout)
        return out.strip()
    except Exception:
        return ""


def _parse_redirections(segment):
    """Strip trailing `2>/dev/null` / `2>&1` from one pipeline segment.

    Returns (argv_list, stderr_target) where stderr_target is one of
    subprocess.DEVNULL, subprocess.STDOUT, or None.
    """
    import shlex

    tokens = shlex.split(segment)
    stderr = None
    cleaned = []
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if tok == "2>/dev/null":
            stderr = subprocess.DEVNULL
        elif tok == "2>&1":
            stderr = subprocess.STDOUT
        elif tok == "2>" and i + 1 < len(tokens) and tokens[i + 1] == "/dev/null":
            stderr = subprocess.DEVNULL
            i += 1
        else:
            cleaned.append(tok)
        i += 1
    return cleaned, stderr


def _run_pipeline(cmd, timeout=5):
    """Run a `a | b | c` pipeline with shell=False. Returns (stdout, rc)."""
    segments = [s.strip() for s in cmd.split("|")]
    prev_stdout = None
    procs = []
    for idx, segment in enumerate(segments):
        argv, stderr_target = _parse_redirections(segment)
        if not argv:
            continue
        is_last = idx == len(segments) - 1
        proc = subprocess.Popen(
            argv,
            stdin=prev_stdout,
            stdout=subprocess.PIPE,
            stderr=(stderr_target if stderr_target is not None else None),
            text=True,
        )
        if prev_stdout is not None:
            prev_stdout.close()  # allow upstream to receive SIGPIPE
        prev_stdout = proc.stdout
        procs.append(proc)
    if not procs:
        return "", 0
    try:
        out, _ = procs[-1].communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        for p in procs:
            p.kill()
        return "", 124
    for p in procs[:-1]:
        try:
            p.wait(timeout=1)
        except Exception:
            p.kill()
    return out or "", procs[-1].returncode


def _release_image():
    """FO-1: True when this firmware was built as a PRODUCTION/release image.

    Mirrors the Rust daemon's `auth::is_release_image()` — the marker file
    `/etc/dcentos/release-image` is stamped only for release builds. On a
    DEV/LAB image the file is absent and the MCP server stays open (no token),
    byte-identical to today. On a release image a Bearer token is required for
    write tools.
    """
    return os.path.exists("/etc/dcentos/release-image")


# FO-1: MCP auth token for release images. Read once from a root-only file
# (preferred: the daemon's auth surface; fallback: a dedicated MCP token file).
# DEV images leave this None → no token required (open, as today).
_MCP_TOKEN_CACHE = {"loaded": False, "token": None}


def _mcp_required_token():
    """Return the Bearer token required for write tools on a release image,
    or None if none is provisioned / this is a DEV image."""
    if not _release_image():
        return None
    if _MCP_TOKEN_CACHE["loaded"]:
        return _MCP_TOKEN_CACHE["token"]
    token = None
    for path in ("/run/dcentos/mcp_token", "/data/dcent/mcp_token"):
        try:
            with open(path, "r") as fh:
                candidate = fh.read().strip()
            if candidate:
                token = candidate
                break
        except OSError:
            pass
    _MCP_TOKEN_CACHE["loaded"] = True
    _MCP_TOKEN_CACHE["token"] = token
    return token


def _extract_bearer(headers):
    """Pull the bearer token from an Authorization header, or None."""
    auth = headers.get("Authorization") or headers.get("authorization")
    if not auth:
        return None
    if auth.startswith("Bearer "):
        return auth[len("Bearer "):].strip()
    return None


def _tokens_equal(a, b):
    """Constant-time token comparison (avoids a timing oracle on a release
    image). Both args are strings; returns True only on exact match."""
    if a is None or b is None:
        return False
    try:
        import hmac

        return hmac.compare_digest(a, b)
    except Exception:
        # Fallback constant-ish compare.
        if len(a) != len(b):
            return False
        diff = 0
        for x, y in zip(a, b):
            diff |= ord(x) ^ ord(y)
        return diff == 0


def _write_tool_authorized(auth_token):
    """FO-1 control-tool gate decision. Returns (allowed, reason).

    Operator decision (DCENT design-language §MCP-auth, RESOLVED): DEV builds
    stay open for shop convenience, but a RELEASE/production image must REQUIRE
    a valid Bearer token for every control/write tool — a shipped industrial
    unit refuses control without auth.

    Contract (fail-closed by default — only an explicit DEV marker opens it):
      - DEV image (no `/etc/dcentos/release-image` marker): ALLOW (open, as today).
      - Release image WITH a provisioned token: ALLOW only on constant-time match.
      - Release image with NO provisioned token: REFUSE. This is the load-bearing
        hardening — previously a release unit whose token file was missing/empty
        (provisioning failure, boot race) fell OPEN because the gate was skipped
        when `_mcp_required_token()` returned None. A release/unknown image now
        fails CLOSED.
    Read tools never reach this gate (only WRITE_TOOLS are checked at the call
    site), so reads stay open on both DEV and release.
    """
    if not _release_image():
        return True, "dev-image-open"
    required = _mcp_required_token()
    if required is None:
        # Release image but no token provisioned → fail closed.
        return False, "release-image-no-token-provisioned"
    if _tokens_equal(auth_token, required):
        return True, "release-image-token-ok"
    return False, "release-image-token-mismatch"




def uio_names():
    """Return UIO device names exposed by sysfs."""
    names = []
    try:
        for entry in os.listdir("/sys/class/uio"):
            path = os.path.join("/sys/class/uio", entry, "name")
            try:
                with open(path, "r", encoding="utf-8") as fh:
                    names.append(fh.read().strip())
            except OSError:
                pass
    except OSError:
        pass
    return names




def is_am2_class():
    """True on am2-class control boards (S19j Pro / S19 Pro / S17 Pro Zynq) whose
    hashboard EEPROMs live at I2C 0x50-0x57. Mirrors the dcentrald HAL write-
    denylist so this parallel MCP write path can't bypass the .74 hb2 EEPROM
    corruption-prevention guarantee. S9 is deliberately NOT am2-class: its
    0x55-0x57 are PIC voltage controllers (legitimate write targets), not EEPROMs,
    so they must stay writable."""
    for path in ("/etc/dcentos/board_target", "/etc/dcentos-platform", "/etc/dcentos/model"):
        try:
            with open(path, "r", encoding="utf-8") as fh:
                value = fh.read().strip().lower()
            if "am2" in value or "s19j" in value or "s19pro" in value or "s17pro" in value:
                return True
        except OSError:
            pass
    return False






# =============================================================================
# MCP Tool Implementations
# =============================================================================

def _raw_hardware_unavailable(interface, operation, params=None):
    """Fail closed until a typed daemon-owned snapshot or broker exists."""
    requested = params if isinstance(params, dict) else {}
    return {
        "status": "unavailable",
        "interface": interface,
        "operation": operation,
        "requested": requested,
        "hardware_access_attempted": False,
        "reason": (
            f"Direct {interface} access from the MCP process is retired. "
            "dcentrald is the exclusive runtime hardware owner; add a serialized daemon snapshot "
            "or command broker before restoring this operation."
        ),
    }


def _raw_i2c_unavailable(operation, params=None):
    """Preserve the I2C-specific compatibility helper."""
    return _raw_hardware_unavailable("I2C", operation, params)


def tool_get_system_status(params):
    """Return the daemon-owned runtime status snapshot."""
    return tool_live_stats(params)


def tool_get_temperatures(params):
    """Return daemon-owned per-chain temperature snapshots.

    The MCP process is an API adapter, not a second hardware owner. Direct
    sensor polling would bypass dcentrald's serialized bus service and can
    consume a controller reply intended for the mining runtime.
    """
    status = tool_live_stats({})
    if "error" in status:
        return {
            "status": "unavailable",
            "source": "dcentrald /api/status",
            "sensors": {},
            "count": 0,
            "error": status["error"],
        }

    sensors = {}
    chains = status.get("chains", [])
    if isinstance(chains, list):
        for chain in chains:
            if not isinstance(chain, dict):
                continue
            temp_c = chain.get("temp_c")
            if not isinstance(temp_c, (int, float)) or temp_c <= 0:
                continue
            chain_id = chain.get("id", "unknown")
            sensors[f"chain_{chain_id}"] = {
                "celsius": temp_c,
                "source": chain.get("temp_source") or "dcentrald snapshot",
                "chain_id": chain_id,
            }
    return {
        "status": "ok" if sensors else "unavailable",
        "source": "dcentrald /api/status",
        "sensors": sensors,
        "count": len(sensors),
    }


def tool_get_chain_status(params):
    """Return daemon-owned chain state without touching GPIO."""
    status = tool_live_stats(params)
    if "error" in status:
        return status
    return {
        "status": "ok",
        "source": "dcentrald /api/status",
        "chains": status.get("chains", []),
        "hardware_access_attempted": False,
    }


def tool_get_fpga_registers(params):
    """Preserve the legacy tool name without opening physical memory."""
    return _raw_hardware_unavailable("FPGA register", "read", params)


def tool_get_i2c_scan(params):
    """Preserve the legacy tool name without bypassing daemon ownership."""
    return _raw_i2c_unavailable("scan", params)


def tool_read_i2c_register(params):
    """Preserve the legacy tool name without bypassing daemon ownership."""
    return _raw_i2c_unavailable("read", params)


def tool_get_nand_info(params):
    """Get NAND partition layout."""
    mtd = run_cmd("cat /proc/mtd 2>/dev/null")
    partitions = []
    if mtd:
        for line in mtd.split("\n")[1:]:
            if line.strip():
                parts = line.split()
                if len(parts) >= 4:
                    size_bytes = int(parts[1], 16)
                    partitions.append({
                        "dev": parts[0].rstrip(":"),
                        "size_hex": parts[1],
                        "size_mb": round(size_bytes / 1048576, 1),
                        "name": parts[3].strip('"'),
                    })
    ubi = run_cmd("ubinfo -a 2>/dev/null | head -30")
    return {"partitions": partitions, "ubi_info": ubi or "N/A"}


def tool_get_uio_devices(params):
    """List all UIO (Userspace I/O) devices mapped by the FPGA."""
    devices = []
    uio_base = "/sys/class/uio"
    if os.path.exists(uio_base):
        for uio in sorted(os.listdir(uio_base)):
            uio_path = os.path.join(uio_base, uio)
            name = ""
            addr = ""
            try:
                with open(os.path.join(uio_path, "name")) as f:
                    name = f.read().strip()
            except Exception:
                pass
            try:
                with open(os.path.join(uio_path, "maps", "map0", "addr")) as f:
                    addr = f.read().strip()
            except Exception:
                pass
            devices.append({"id": uio, "name": name, "address": addr})
    return {"devices": devices, "count": len(devices)}


def tool_run_diagnostic(params):
    """Compose diagnostics from daemon snapshots and OS metadata."""
    status = tool_live_stats({})
    return {
        "status": "ok" if "error" not in status else "unavailable",
        "source": "dcentrald snapshots",
        "system": status,
        "temperatures": tool_get_temperatures({}),
        "chains": tool_get_chain_status({}),
        "uio": tool_get_uio_devices({}),
        "hardware_access_attempted": False,
    }


def tool_get_fan_speed(params):
    """Return daemon-published fan telemetry."""
    status = tool_live_stats({})
    if "error" in status:
        return status
    fans = status.get("fans", {})
    return {
        "status": "ok" if isinstance(fans, dict) else "unavailable",
        "source": "dcentrald /api/status",
        "fans": fans if isinstance(fans, dict) else {},
        "hardware_access_attempted": False,
    }


def tool_set_fan_speed(params):
    """Preserve the legacy mutation tool until an authenticated broker exists."""
    return _raw_hardware_unavailable("fan controller", "write", params)


def tool_read_devmem(params):
    """Preserve the legacy tool name without opening physical memory."""
    return _raw_hardware_unavailable("physical memory", "read", params)


def _validate_hex(value, name="value"):
    """Validate a hex string for devmem/I2C commands. Returns error dict or None."""
    if not re.match(r'^0x[0-9a-fA-F]{1,8}$', value):
        return {"error": f"Invalid {name} format (use 0x hex, e.g. 0x1A)"}
    return None


def tool_write_fpga_register(params):
    """Preserve the legacy tool name without mutating FPGA registers."""
    return _raw_hardware_unavailable("FPGA register", "write", params)


def tool_write_devmem(params):
    """Preserve the legacy tool name without mutating physical memory."""
    return _raw_hardware_unavailable("physical memory", "write", params)


def tool_write_i2c_register(params):
    """Refuse the legacy parallel write path."""
    return _raw_i2c_unavailable("write", params)


def tool_pic_status(params):
    """Return the daemon-owned PIC catalog/live-snapshot surface."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/hardware/pic_info", timeout=3)
    if not raw:
        return {
            "status": "unavailable",
            "source": "dcentrald /api/hardware/pic_info",
            "error": "dcentrald PIC snapshot endpoint not responding",
        }
    try:
        return sanitize_obj(json.loads(raw))
    except json.JSONDecodeError:
        return {
            "status": "unavailable",
            "source": "dcentrald /api/hardware/pic_info",
            "error": "invalid JSON from daemon PIC snapshot endpoint",
        }


def tool_gpio_read(params):
    """Preserve the legacy tool name without exporting or reading GPIO."""
    return _raw_hardware_unavailable("GPIO", "read", params)


def tool_gpio_write(params):
    """Preserve the legacy tool name without mutating GPIO."""
    return _raw_hardware_unavailable("GPIO", "write", params)


def tool_board_control(params):
    """Preserve the legacy tool name without mutating board control."""
    return _raw_hardware_unavailable("FPGA board control", "mutate", params)


def tool_get_hashrate(params):
    """Get live hashrate from dcentrald REST API at localhost:8080."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/status", timeout=3)
    if not raw:
        return {"error": "dcentrald API not responding — daemon may not be running"}
    try:
        data = json.loads(raw)
        return {
            "hashrate": data.get("hashrate"),
            "hashrate_5m": data.get("hashrate_5m"),
            "hashrate_1h": data.get("hashrate_1h"),
            "unit": data.get("hashrate_unit", "TH/s"),
            "accepted": data.get("accepted"),
            "rejected": data.get("rejected"),
        }
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from dcentrald API", "raw": raw[:200]}


def tool_get_status(params):
    """Shared cross-firmware status snapshot."""
    return tool_live_stats(params)


def tool_get_device_info(params):
    """Shared cross-firmware device identity snapshot."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/system/info", timeout=3)
    if not raw:
        return {"error": "dcentrald system info endpoint not responding"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from /api/system/info", "raw": raw[:200]}


def tool_get_swarm_status(params):
    """Shared cross-firmware swarm status snapshot."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/swarm", timeout=3)
    if not raw:
        return {"error": "dcentrald swarm endpoint not responding"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from /api/swarm", "raw": raw[:200]}


def tool_identify_device(params):
    """Toggle the identify LED pattern via dcentrald REST API."""
    raw = run_cmd(
        "curl -s -X POST -H 'Content-Type: application/json' http://127.0.0.1:8080/api/system/identify",
        timeout=3,
    )
    if not raw:
        return {"error": "dcentrald identify endpoint not responding"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"status": "sent", "raw_response": raw[:200]}


def tool_restart_mining(params):
    """Restart mining via dcentrald REST API."""
    raw = run_cmd(
        "curl -s -X POST -H 'Content-Type: application/json' http://127.0.0.1:8080/api/action/restart",
        timeout=5,
    )
    if not raw:
        return {"error": "dcentrald restart endpoint not responding"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"status": "sent", "raw_response": raw[:200]}


def tool_set_pool(params):
    """Shared cross-firmware alias for pool switching."""
    return tool_pool_switch(params)


def tool_get_config(params):
    """Read dcentrald config file. Returns raw TOML text."""
    config_path = "/data/dcentrald.toml"
    if not os.path.exists(config_path):
        config_path = "/etc/dcentrald.toml"
    if not os.path.exists(config_path):
        return {"error": "No config file found at /data/dcentrald.toml or /etc/dcentrald.toml"}
    try:
        with open(config_path) as f:
            content = f.read()
        # MCP-2: mask the operator wallet (worker=) + strip any pool URL
        # user:pass@ credentials before returning the raw TOML (read tool, ungated).
        return {"path": config_path, "content": sanitize_secrets(content)}
    except Exception as e:
        return {"error": str(e)}


def tool_set_config(params):
    """Update config key. Args: key, value. Simple text-based TOML update."""
    key = params.get("key", "")
    value = params.get("value", "")
    if not key:
        return {"error": "key is required"}
    # Sanitize key to prevent injection
    if not re.match(r'^[a-zA-Z0-9_.-]+$', key):
        return {"error": "Invalid key format (alphanumeric, underscore, dot, dash only)"}
    config_path = "/data/dcentrald.toml"
    if not os.path.exists(config_path):
        config_path = "/etc/dcentrald.toml"
    if not os.path.exists(config_path):
        return {"error": "No config file found"}
    try:
        with open(config_path) as f:
            lines = f.readlines()
        found = False
        for i, line in enumerate(lines):
            stripped = line.strip()
            if stripped.startswith(f"{key} ") or stripped.startswith(f"{key}="):
                lines[i] = f"{key} = {value}\n"
                found = True
                break
        if not found:
            # Append under [general] if it exists, else at end
            general_idx = None
            for i, line in enumerate(lines):
                if line.strip() == "[general]":
                    general_idx = i
                    break
            if general_idx is not None:
                lines.insert(general_idx + 1, f"{key} = {value}\n")
            else:
                lines.append(f"\n[general]\n{key} = {value}\n")
        with open(config_path, "w") as f:
            f.writelines(lines)
        return {"path": config_path, "key": key, "value": value, "status": "updated" if found else "appended"}
    except Exception as e:
        return {"error": str(e)}


def tool_tail_log(params):
    """Last N lines of /tmp/dcentrald.log. Args: lines (int, default 50)."""
    n = min(int(params.get("lines", 50)), 500)
    output = run_cmd(f"tail -n {n} /tmp/dcentrald.log 2>/dev/null")
    if not output:
        return {"error": "Log file empty or not found at /tmp/dcentrald.log"}
    # MCP-2: mask wallet addresses + pool credentials in returned log lines.
    return {"lines": n, "log": sanitize_secrets(output)}


def tool_grep_log(params):
    """Search log for pattern. Args: pattern (str), lines (int, default 20)."""
    pattern = params.get("pattern", "")
    n = min(int(params.get("lines", 20)), 200)
    # Sanitize pattern to prevent command injection
    if not re.match(r'^[a-zA-Z0-9 .\[\]\|_:/-]+$', pattern):
        return {"error": "Invalid pattern — only alphanumeric, spaces, dots, brackets, pipes, underscores, colons, slashes, dashes allowed"}
    output = run_cmd(f"grep '{pattern}' /tmp/dcentrald.log 2>/dev/null | tail -n {n}")
    if not output:
        return {"pattern": pattern, "matches": 0, "log": ""}
    line_count = len(output.split("\n"))
    # MCP-2: mask wallet addresses + pool credentials in matched log lines.
    return {"pattern": pattern, "matches": line_count, "log": sanitize_secrets(output)}


def tool_check_daemon(params):
    """Check dcentrald daemon status: PID, uptime, API health."""
    pid = run_cmd("pidof dcentrald 2>/dev/null")
    result = {"running": bool(pid), "pid": int(pid) if pid else None}
    if pid:
        # Get uptime from /proc/PID/stat
        stat = run_cmd(f"cat /proc/{pid}/stat 2>/dev/null")
        if stat:
            parts = stat.split()
            if len(parts) > 21:
                try:
                    start_ticks = int(parts[21])
                    uptime_s = run_cmd("cat /proc/uptime 2>/dev/null")
                    clk_tck = int(run_cmd("getconf CLK_TCK 2>/dev/null") or "100")
                    if uptime_s:
                        sys_uptime = float(uptime_s.split()[0])
                        proc_start = start_ticks / clk_tck
                        result["uptime_seconds"] = round(sys_uptime - proc_start)
                except (ValueError, IndexError):
                    pass
        # Check API health
        api_raw = run_cmd("curl -s http://127.0.0.1:8080/api/status", timeout=3)
        result["api_healthy"] = bool(api_raw)
    else:
        result["api_healthy"] = False
    return result


def tool_system_health(params):
    """Combined system health: CPU, memory, disk, NAND, uptime."""
    health = {}
    # CPU load
    loadavg = run_cmd("cat /proc/loadavg 2>/dev/null")
    if loadavg:
        parts = loadavg.split()
        health["cpu_load"] = {"1m": parts[0], "5m": parts[1], "15m": parts[2]}
    # Memory
    mem = run_cmd("free -h 2>/dev/null | grep Mem")
    if mem:
        parts = mem.split()
        health["memory"] = {
            "total": parts[1] if len(parts) > 1 else "?",
            "used": parts[2] if len(parts) > 2 else "?",
            "free": parts[3] if len(parts) > 3 else "?",
        }
    # Disk
    disk = run_cmd("df -h / 2>/dev/null | tail -1")
    if disk:
        parts = disk.split()
        health["disk"] = {
            "size": parts[1] if len(parts) > 1 else "?",
            "used": parts[2] if len(parts) > 2 else "?",
            "avail": parts[3] if len(parts) > 3 else "?",
            "use_pct": parts[4] if len(parts) > 4 else "?",
        }
    # NAND health
    nand = run_cmd("dmesg | grep -i nand 2>/dev/null | tail -5")
    health["nand_dmesg"] = nand or "N/A"
    # Uptime
    health["uptime"] = run_cmd("uptime 2>/dev/null") or "N/A"
    return health


def tool_live_stats(params):
    """Snapshot from dcentrald /api/status. Proxy the full JSON response."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/status", timeout=3)
    if not raw:
        return {"error": "dcentrald API not responding — daemon may not be running"}
    try:
        # MCP-2: recursively mask wallet/pool credentials in the proxied JSON
        # (the pool URL + worker can surface in /api/status). Also covers
        # tool_get_status, which delegates here.
        return sanitize_obj(json.loads(raw))
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from dcentrald API", "raw": sanitize_secrets(raw)[:200]}


def tool_pool_status(params):
    """Pool connection info from dcentrald /api/pools."""
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/pools", timeout=3)
    if not raw:
        return {"error": "dcentrald API not responding — daemon may not be running"}
    try:
        # MCP-2: defense-in-depth — recursively mask wallet/pool credentials in
        # the proxied JSON. /api/pools carries pool URLs (which may embed a
        # user:pass@ credential) and the worker (a BTC payout address in V1
        # solo); the Rust handler masks at source, but this belt-and-suspenders
        # wrap matches tool_live_stats so this higher-risk endpoint can never
        # leak either even if the source masking regresses.
        return sanitize_obj(json.loads(raw))
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from dcentrald API", "raw": sanitize_secrets(raw)[:200]}


def tool_pool_switch(params):
    """Switch active pool. Args: url, worker, password."""
    url = params.get("url", "")
    worker = params.get("worker", "")
    password = params.get("password", "x")
    if not url or not worker:
        return {"error": "url and worker are required"}
    # Sanitize inputs to prevent injection via curl
    for name, val in [("url", url), ("worker", worker), ("password", password)]:
        if not re.match(r'^[a-zA-Z0-9._:/@+-]+$', val):
            return {"error": f"Invalid {name} format — alphanumeric, dots, colons, slashes, @, +, - only"}
    payload = json.dumps({"url": url, "worker": worker, "password": password})
    raw = run_cmd(
        f"curl -s -X POST -H 'Content-Type: application/json' "
        f"-d '{payload}' http://127.0.0.1:8080/api/pools",
        timeout=5,
    )
    if not raw:
        return {"error": "dcentrald API not responding — daemon may not be running"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"status": "sent", "raw_response": raw[:200]}


def tool_service_control(params):
    """Start/stop/restart dcentrald or MCP. Args: service, action."""
    service = params.get("service", "")
    action = params.get("action", "")
    if service not in ("dcentrald", "mcp"):
        return {"error": "service must be 'dcentrald' or 'mcp'"}
    if action not in ("start", "stop", "restart"):
        return {"error": "action must be 'start', 'stop', or 'restart'"}
    if service == "mcp" and action in ("stop", "restart"):
        return {"error": "REFUSED: stopping MCP would kill this server — use SSH instead"}
    if service == "dcentrald":
        for init_script in ("/etc/init.d/S82dcentrald", "/etc/init.d/dcentrald"):
            if os.path.exists(init_script):
                output = run_cmd(f"{init_script} {action} 2>&1", timeout=15)
                return {
                    "service": service,
                    "action": action,
                    "init_script": init_script,
                    "output": output or "ok",
                }
        return {
            "error": "dcentrald service wrapper not found; refusing unsafe direct process control",
            "expected": ["/etc/init.d/S82dcentrald", "/etc/init.d/dcentrald"],
        }
    elif service == "mcp" and action == "start":
        return {"service": service, "action": action, "status": "MCP is already running (this server)"}
    return {"error": "Unknown service/action combination"}


# =============================================================================
# Bosminer-handoff diagnostic tools (read-only; no destructive ops).
# Dev-firmware-only: gated via _release_image(), the same no-auth posture
# as the rest of the dev-firmware MCP surface (intentional for DEV images).
# These tools register only when _release_image() is False; on a release
# image they are NOT in the TOOLS dict at all, so tools/list omits them and
# there is no surface leak.
# =============================================================================

DEV_ONLY_DESCRIPTION_PREFIX = "[DEV firmware only] "


def tool_wave54_recipe_state(params):
    """Read-only view of the bosminer-handoff recipe env state.

    Proxies `GET /api/env/recipe` on the local dcentrald (no auth, dev
    firmware posture). Returns:
      - applied: dict of required env vars that ARE set
      - missing: list of required env vars that are NOT set
      - forbidden_detected: list of forbidden env vars that ARE set
        (each is known to break mining on this hardware class — non-empty
         means the daemon refuses to start on this hardware)
      - fingerprint: platform / board_target / psu_hardware_variant
      - is_xil_25_class: True only on the PSU-spoof handoff hardware class
      - wave54_recipe_intact: True iff all required applied + zero forbidden
    """
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/env/recipe", timeout=3)
    if not raw:
        return {"error": "dcentrald /api/env/recipe not responding"}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from /api/env/recipe", "raw": raw[:200]}


def tool_chain_enum_diff(params):
    """Per-chain chips_responding/chips_expected diff.

    Proxies `GET /api/mining/chain/presence`. Returns per-chain pillar
    data the dashboard's ChainPresencePanel renders, plus the chip-rail
    mV actual-vs-target reading for the ChipRailMvPill. Useful for
    quickly seeing whether the full chip enumeration engaged (or just a
    partial-chain pattern that still produces accepted shares but is not
    a healthy baseline).
    """
    raw = run_cmd("curl -s http://127.0.0.1:8080/api/mining/chain/presence", timeout=3)
    if not raw:
        return {"error": "dcentrald /api/mining/chain/presence not responding"}
    try:
        data = json.loads(raw)
        # Enrich with per-chain ratio for at-a-glance reading.
        for chain in data.get("chains", []):
            resp = chain.get("chips_responding", 0)
            exp = chain.get("chips_expected", 0) or 1
            chain["presence_ratio"] = round(resp / exp, 3)
            chain["presence_verdict"] = (
                "healthy" if resp / exp >= 0.9
                else "partial" if resp / exp >= 0.5
                else "broken"
            )
        return data
    except json.JSONDecodeError:
        return {"error": "Invalid JSON from /api/mining/chain/presence", "raw": raw[:200]}


def tool_psu_loki_state(params):
    """Preserve the lab tool name without opening the daemon-owned bus."""
    return _raw_i2c_unavailable("psu_loki_state", params)


def tool_capture_chain_uart_bytes(params):
    """Preserve the legacy tool name without opening the chain UART."""
    return _raw_hardware_unavailable("chain UART", "capture", params)


# =============================================================================
# MCP Protocol Handler
# =============================================================================

TOOLS = {
    "get_status": {
        "description": "Shared cross-firmware miner status summary.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_status,
    },
    "get_device_info": {
        "description": "Shared cross-firmware device identity and ASIC metadata.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_device_info,
    },
    "get_swarm_status": {
        "description": "Shared cross-firmware swarm and discovery status.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_swarm_status,
    },
    "identify_device": {
        "description": "Toggle the physical identify signal for the miner.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_identify_device,
    },
    "restart_mining": {
        "description": "Restart mining without changing pool configuration.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_restart_mining,
    },
    "set_pool": {
        "description": "Shared cross-firmware pool update alias.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Pool URL (e.g. stratum+tcp://pool.example.com:3333)"},
                "worker": {"type": "string", "description": "Worker name (e.g. bc1q.../worker1)"},
                "password": {"type": "string", "description": "Pool password (default: x)", "default": "x"},
            },
            "required": ["url", "worker"],
        },
        "handler": tool_set_pool,
    },
    "get_system_status": {
        "description": "Return the daemon-owned runtime status snapshot.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_system_status,
    },
    "get_temperatures": {
        "description": "Return daemon-owned per-chain temperature snapshots without opening hardware buses.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_temperatures,
    },
    "get_chain_status": {
        "description": "Return daemon-owned chain state without exporting or reading GPIOs.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_chain_status,
    },
    "get_fpga_registers": {
        "description": "Compatibility stub: raw FPGA register reads are unavailable outside the daemon owner.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "base_addr": {"type": "string", "description": "Hex address (default: 0x43C00000)", "default": "0x43C00000"},
                "count": {"type": "integer", "description": "Number of 32-bit registers to read (max 32)", "default": 6},
            },
        },
        "handler": tool_get_fpga_registers,
    },
    "get_i2c_scan": {
        "description": "Compatibility stub: unavailable until dcentrald exposes a serialized topology snapshot.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {"type": "integer", "description": "I2C bus number (0-7)", "default": 0},
            },
        },
        "handler": tool_get_i2c_scan,
    },
    "read_i2c_register": {
        "description": "Compatibility stub: direct register access is unavailable outside the dcentrald hardware owner.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {"type": "integer", "description": "I2C bus number"},
                "address": {"type": "string", "description": "Device address (hex, e.g. 0x48)"},
                "register": {"type": "string", "description": "Register address (hex, e.g. 0x00)"},
                "mode": {"type": "string", "description": "Read mode: b=byte, w=word", "default": "b"},
            },
            "required": ["bus", "address", "register"],
        },
        "handler": tool_read_i2c_register,
    },
    "get_nand_info": {
        "description": "Get NAND flash partition layout (MTD) and UBI volume information.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_nand_info,
    },
    "get_uio_devices": {
        "description": "List all UIO (Userspace I/O) devices. These are FPGA-mapped register regions for fan control, hash chain communication, and glitch monitoring.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_uio_devices,
    },
    "run_diagnostic": {
        "description": "Compose a diagnostic view from daemon snapshots and read-only OS metadata.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_run_diagnostic,
    },
    "get_fan_speed": {
        "description": "Return daemon-published fan command and RPM telemetry.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_fan_speed,
    },
    # S5 convergence: `set_fan_speed` is the SOLE OS-exposed member of the
    # cross-firmware MCP tuning superset `dcent.cross-firmware.tuning.v1`
    # (Rust source of truth: dcent-schema::mcp::tuning_profile()). The other 5
    # superset extensions are axe-only by design (keep-unique fence,
    # mcp-auth-contract §2.2/§6) — OS reaches frequency/voltage EFFECTS through
    # its OWN low-level tools (set_config / write_i2c_register), which are NOT
    # superset aliases and stay OS-private. The compatibility name remains a
    # WRITE_TOOLS member and routes through _write_tool_authorized(), but fails
    # closed until dcentrald exposes an authenticated serialized fan broker.
    # Drift-pinned by
    # dcent-schema/tests/python_overlay_drift.rs::python_overlays_match_the_tuning_superset.
    "set_fan_speed": {
        "description": "Compatibility stub: fan mutation is unavailable until dcentrald exposes an authenticated serialized broker.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "percent": {"type": "integer", "description": "Fan PWM percentage (0-100)", "minimum": 0, "maximum": 100},
            },
            "required": ["percent"],
        },
        "handler": tool_set_fan_speed,
    },
    "read_devmem": {
        "description": "Compatibility stub: raw physical-memory reads are unavailable in the runtime image.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "address": {"type": "string", "description": "Memory address in hex (e.g. 0x43C00000)"},
            },
            "required": ["address"],
        },
        "handler": tool_read_devmem,
    },
    "write_fpga_register": {
        "description": "Compatibility stub: raw FPGA register writes are unavailable in the runtime image.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "address": {"type": "string", "description": "FPGA register address in hex (e.g. 0x43C00004)"},
                "value": {"type": "string", "description": "Value to write in hex (e.g. 0x0C)"},
            },
            "required": ["address", "value"],
        },
        "handler": tool_write_fpga_register,
    },
    "write_devmem": {
        "description": "Compatibility stub: raw physical-memory writes are unavailable in the runtime image.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "address": {"type": "string", "description": "Physical address in hex (e.g. 0x43C00000)"},
                "value": {"type": "string", "description": "Value to write in hex (e.g. 0x00000007)"},
            },
            "required": ["address", "value"],
        },
        "handler": tool_write_devmem,
    },
    "write_i2c_register": {
        "description": "Retired compatibility stub: parallel I2C mutation is always refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {"type": "integer", "description": "I2C bus number"},
                "address": {"type": "string", "description": "Device address in hex (e.g. 0x48)"},
                "register": {"type": "string", "description": "Register address in hex (e.g. 0x00)"},
                "value": {"type": "string", "description": "Value to write in hex (e.g. 0x0A)"},
                "mode": {"type": "string", "description": "Write mode: b=byte, w=word", "default": "b"},
            },
            "required": ["bus", "address", "register", "value"],
        },
        "handler": tool_write_i2c_register,
    },
    "pic_status": {
        "description": "Return the daemon-owned PIC catalog and live-snapshot status; never probe controllers directly.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_pic_status,
    },
    "gpio_read": {
        "description": "Compatibility stub: raw GPIO reads are unavailable outside the daemon owner.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {"type": "integer", "description": "GPIO pin number"},
            },
            "required": ["pin"],
        },
        "handler": tool_gpio_read,
    },
    "gpio_write": {
        "description": "Compatibility stub: raw GPIO writes are unavailable outside the daemon owner.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {"type": "integer", "description": "GPIO pin number (must be >= 890)"},
                "value": {"type": "integer", "description": "0 or 1", "minimum": 0, "maximum": 1},
            },
            "required": ["pin", "value"],
        },
        "handler": tool_gpio_write,
    },
    "board_control": {
        "description": "Compatibility stub: direct FPGA board-control mutations are unavailable.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "chain_id": {"type": "integer", "description": "Chain ID: 6, 7, or 8", "enum": [6, 7, 8]},
                "action": {"type": "string", "description": "Action: enable, disable, or reset", "enum": ["enable", "disable", "reset"]},
            },
            "required": ["chain_id", "action"],
        },
        "handler": tool_board_control,
    },
    "get_hashrate": {
        "description": "Get live hashrate from dcentrald REST API. Returns hashrate, accepted/rejected shares.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_hashrate,
    },
    "get_config": {
        "description": "Read the current dcentrald configuration file (/data/dcentrald.toml or /etc/dcentrald.toml). Returns raw TOML text.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_get_config,
    },
    "set_config": {
        "description": "Update a single key in the dcentrald config file. Text-based TOML update — appends under [general] if key not found.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "Config key name (e.g. frequency_mhz)"},
                "value": {"type": "string", "description": "New value (e.g. 650)"},
            },
            "required": ["key", "value"],
        },
        "handler": tool_set_config,
    },
    "tail_log": {
        "description": "Get last N lines of /tmp/dcentrald.log. Max 500 lines.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "lines": {"type": "integer", "description": "Number of lines (default 50, max 500)", "default": 50},
            },
        },
        "handler": tool_tail_log,
    },
    "grep_log": {
        "description": "Search dcentrald log for a pattern. Returns matching lines (max 200). Pattern is sanitized to prevent injection.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Search pattern (alphanumeric, spaces, dots, brackets, pipes)"},
                "lines": {"type": "integer", "description": "Max matching lines to return (default 20)", "default": 20},
            },
            "required": ["pattern"],
        },
        "handler": tool_grep_log,
    },
    "check_daemon": {
        "description": "Check dcentrald daemon status: running, PID, uptime in seconds, API health.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_check_daemon,
    },
    "system_health": {
        "description": "Combined system health snapshot: CPU load, memory, disk usage, NAND dmesg, uptime. Single comprehensive call.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_system_health,
    },
    "live_stats": {
        "description": "Full snapshot from dcentrald /api/status. Proxies the complete JSON response.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_live_stats,
    },
    "pool_status": {
        "description": "Get pool connection info from dcentrald /api/pools.",
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_pool_status,
    },
    "pool_switch": {
        "description": "Switch the active mining pool. POSTs new pool config to dcentrald.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Pool URL (e.g. stratum+tcp://pool.example.com:3333)"},
                "worker": {"type": "string", "description": "Worker name (e.g. bc1q.../worker1)"},
                "password": {"type": "string", "description": "Pool password (default: x)", "default": "x"},
            },
            "required": ["url", "worker"],
        },
        "handler": tool_pool_switch,
    },
    "service_control": {
        "description": "Start, stop, or restart dcentrald or MCP service. REFUSES to stop MCP (would kill itself).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "service": {"type": "string", "description": "Service name: dcentrald or mcp", "enum": ["dcentrald", "mcp"]},
                "action": {"type": "string", "description": "Action: start, stop, or restart", "enum": ["start", "stop", "restart"]},
            },
            "required": ["service", "action"],
        },
        "handler": tool_service_control,
    },
}


# Register the bosminer-handoff diagnostic tools ONLY on DEV firmware images.
# _release_image() returns True on a release/production image (stamped via
# /etc/dcentos/release-image at Buildroot post-build); False on DEV/lab. On DEV,
# _mcp_required_token() returns None, so these tools are no-auth, matching the
# rest of the dev-firmware MCP surface (intentional for DEV images). On release,
# they're not in TOOLS at all, so tools/list omits them and there is no leak.
if not _release_image():
    TOOLS["wave54_recipe_state"] = {
        "description": DEV_ONLY_DESCRIPTION_PREFIX + (
            "Read-only view of the bosminer-handoff recipe env state. "
            "Returns required env-var coverage, detection of env vars known "
            "to break mining on this hardware class, a hardware fingerprint, "
            "and a recipe-intact verdict. "
            "Source: GET /api/env/recipe on local dcentrald."
        ),
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_wave54_recipe_state,
    }
    TOOLS["chain_enum_diff"] = {
        "description": DEV_ONLY_DESCRIPTION_PREFIX + (
            "Per-chain chips_responding vs chips_expected diff, plus chip-rail "
            "mV actual vs target. Includes presence_ratio + presence_verdict "
            "(healthy/partial/broken) per chain. Source: "
            "GET /api/mining/chain/presence on local dcentrald."
        ),
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_chain_enum_diff,
    }
    TOOLS["psu_loki_state"] = {
        "description": DEV_ONLY_DESCRIPTION_PREFIX + (
            "Compatibility stub: raw PSU bus reads are unavailable until "
            "dcentrald publishes a serialized PSU diagnostic snapshot."
        ),
        "inputSchema": {"type": "object", "properties": {}},
        "handler": tool_psu_loki_state,
    }
    TOOLS["capture_chain_uart_bytes"] = {
        "description": DEV_ONLY_DESCRIPTION_PREFIX + (
            "Compatibility stub: a parallel UART reader can consume daemon "
            "replies, so capture remains unavailable until repair mode owns "
            "the chain transport exclusively."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "UART device path (default /dev/ttyS1)",
                    "default": "/dev/ttyS1",
                },
                "timeout_s": {
                    "type": "integer",
                    "description": "Capture timeout in seconds (1..10, default 3)",
                    "default": 3,
                    "minimum": 1,
                    "maximum": 10,
                },
                "bytes": {
                    "type": "integer",
                    "description": "Max bytes to capture (16..4096, default 256)",
                    "default": 256,
                    "minimum": 16,
                    "maximum": 4096,
                },
            },
        },
        "handler": tool_capture_chain_uart_bytes,
    }

RESOURCES = [
    {
        "uri": "miner://status",
        "name": "System Status",
        "description": "Live system status including kernel, memory, load, and network information",
        "mimeType": "application/json",
    },
    {
        "uri": "miner://temperatures",
        "name": "Temperature Readings",
        "description": "Current temperature sensor readings from all I2C sensors",
        "mimeType": "application/json",
    },
    {
        "uri": "miner://chains",
        "name": "Chain Status",
        "description": "Hash board chain plug-detect status for chains 6, 7, 8",
        "mimeType": "application/json",
    },
    {
        "uri": "miner://hashrate",
        "name": "Live Hashrate",
        "description": "Live hashrate from dcentrald daemon API",
        "mimeType": "application/json",
    },
    {
        "uri": "miner://config",
        "name": "Miner Config",
        "description": "Current dcentrald configuration",
        "mimeType": "application/json",
    },
]


# FO-1 (SEC-W24-3): tools that mutate hardware/config/mining state. On a
# release image these require a valid Bearer token; read-only tools stay open
# so dashboards/diagnostics keep working. On a DEV image NOTHING requires a
# token (open, byte-identical to today).
WRITE_TOOLS = {
    "identify_device",
    "restart_mining",
    "set_pool",
    "pool_switch",
    # set_fan_speed is the OS-exposed CONTROL tool of the cross-firmware tuning
    # superset `dcent.cross-firmware.tuning.v1` (S5). Membership in WRITE_TOOLS
    # is the load-bearing fail-closed guarantee: on a release image with no
    # token provisioned the tools/call gate REFUSES it (-32001). Drift-pinned
    # against tuning_profile() by tests/python_overlay_drift.rs.
    "set_fan_speed",
    "set_config",
    "write_fpga_register",
    "write_devmem",
    "write_i2c_register",
    "gpio_write",
    "board_control",
    "service_control",
}


# MCP-1 (P1 security, defense-in-depth): explicit read-only allowlist.
#
# `WRITE_TOOLS` is the membership set the release-image bearer gate keys off of.
# A latent risk is that a future edit DROPS or RENAMES a mutating tool out of
# `WRITE_TOOLS` while its handler stays live in `TOOLS` — that tool would then
# skip the gate and be callable WITHOUT a token on a release image under
# `--bind 0.0.0.0`. To make that fail CLOSED instead of fail OPEN, the gate
# (see `_tool_requires_auth`) treats any tool that is NOT in this explicit
# READ_TOOLS allowlist as a write tool on a release image. Reads stay open
# because they are enumerated here; dev images stay fully open regardless.
#
# INVARIANT (pinned by tests/python_overlay_drift.rs): every key in TOOLS is in
# exactly one of READ_TOOLS or WRITE_TOOLS. A new mutating tool added to TOOLS
# but to NEITHER set is denied on release (fail-closed) until it is classified.
READ_TOOLS = {
    "get_status",
    "get_device_info",
    "get_swarm_status",
    "get_system_status",
    "get_temperatures",
    "get_chain_status",
    "get_fpga_registers",
    "get_i2c_scan",
    "read_i2c_register",
    "get_nand_info",
    "get_uio_devices",
    "run_diagnostic",
    "get_fan_speed",
    "read_devmem",
    "pic_status",
    "gpio_read",
    "get_hashrate",
    "get_config",
    "tail_log",
    "grep_log",
    "check_daemon",
    "system_health",
    "live_stats",
    "pool_status",
    #  dev-only diagnostic tools (registered only when not _release_image())
    # are all read-only; they never reach the release auth gate (dev images are
    # open), but list them so the all-classified invariant holds on dev too.
    "wave54_recipe_state",
    "chain_enum_diff",
    "psu_loki_state",
    "capture_chain_uart_bytes",
}


def _tool_requires_auth(tool_name):
    """MCP-1 fail-closed write-classification for the release auth gate.

    A tool requires a bearer token on a release image iff it is NOT in the
    explicit READ_TOOLS allowlist. This means:
      - known write tools (WRITE_TOOLS) → require auth (as before);
      - known read tools (READ_TOOLS)   → stay open;
      - any UNKNOWN tool (e.g. a mutating tool dropped/renamed out of
        WRITE_TOOLS, or a new tool added to TOOLS but never classified) →
        require auth → fail CLOSED rather than silently fall open.
    The dev-image open posture is unchanged: `_write_tool_authorized` returns
    (True, "dev-image-open") for every tool on a DEV image, so reads AND writes
    stay open there.
    """
    return tool_name not in READ_TOOLS


def handle_jsonrpc(request, auth_token=None):
    """Process a JSON-RPC 2.0 request per MCP spec.

    `auth_token` is the Bearer token extracted from the HTTP request (or None).
    On a release image, write tools require it to match the provisioned MCP
    token; on a DEV image it is ignored (open).
    """
    method = request.get("method", "")
    req_id = request.get("id")
    params = request.get("params", {})

    # MCP lifecycle
    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {"listChanged": False},
                    "resources": {"subscribe": False, "listChanged": False},
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": VERSION,
                    "profileId": PROFILE_ID,
                },
                "profile": minimal_profile(),
            },
        }

    if method == "notifications/initialized":
        return None  # No response for notifications

    if method == "ping":
        return {"jsonrpc": "2.0", "id": req_id, "result": {}}

    # Tool listing
    if method == "tools/list":
        tool_list = []
        for name, spec in TOOLS.items():
            tool_list.append({
                "name": name,
                "description": spec["description"],
                "inputSchema": spec["inputSchema"],
            })
        return {"jsonrpc": "2.0", "id": req_id, "result": {"tools": tool_list}}

    # Tool invocation
    if method == "tools/call":
        tool_name = params.get("name", "")
        tool_args = params.get("arguments", {})
        # FO-1 + MCP-1: control/write-tool auth gate. DEV images stay open (shop
        # convenience). Release/production images REQUIRE a valid Bearer token
        # for every mutating tool and fail CLOSED when no token is provisioned
        # (see _write_tool_authorized). Known read tools (READ_TOOLS) never enter
        # this branch and stay open. MCP-1 hardening: classification is now
        # read-allowlist-based (`_tool_requires_auth`) so a mutating tool that is
        # dropped/renamed out of WRITE_TOOLS — or any future unclassified tool —
        # fails CLOSED on a release image instead of silently falling open.
        if _tool_requires_auth(tool_name):
            allowed, _reason = _write_tool_authorized(auth_token)
            if not allowed:
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32001,
                        "message": "Unauthorized: control tools require a valid Bearer token on a release image",
                    },
                }
        if tool_name in TOOLS:
            try:
                result = TOOLS[tool_name]["handler"](tool_args)
                is_error = isinstance(result, dict) and result.get("ok") is False
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [
                            {"type": "text", "text": json.dumps(result, indent=2)}
                        ],
                        "isError": is_error,
                    },
                }
            except Exception as e:
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [{"type": "text", "text": f"Error: {str(e)}"}],
                        "isError": True,
                    },
                }
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
        }

    # Resource listing
    if method == "resources/list":
        return {"jsonrpc": "2.0", "id": req_id, "result": {"resources": RESOURCES}}

    # Resource reading
    if method == "resources/read":
        uri = params.get("uri", "")
        if uri == "miner://status":
            data = tool_get_system_status({})
        elif uri == "miner://temperatures":
            data = tool_get_temperatures({})
        elif uri == "miner://chains":
            data = tool_get_chain_status({})
        elif uri == "miner://hashrate":
            data = tool_get_hashrate({})
        elif uri == "miner://config":
            data = tool_get_config({})
        else:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32602, "message": f"Unknown resource: {uri}"},
            }
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "contents": [
                    {"uri": uri, "mimeType": "application/json", "text": json.dumps(data, indent=2)}
                ]
            },
        }

    return {
        "jsonrpc": "2.0",
        "id": req_id,
        "error": {"code": -32601, "message": f"Method not found: {method}"},
    }


class MCPHandler(http.server.BaseHTTPRequestHandler):
    """HTTP handler for MCP Streamable HTTP transport."""

    def do_POST(self):
        if self.path != "/mcp":
            self.send_error(404)
            return

        # MCP-3: reject a malformed or oversized Content-Length before reading
        # the body, so a single huge POST can't exhaust memory.
        try:
            content_len = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self.send_json_error(-32600, "Invalid Content-Length")
            return
        if content_len < 0 or content_len > MAX_REQUEST_BODY_BYTES:
            self.send_json_error(
                -32600, f"Request body too large (max {MAX_REQUEST_BODY_BYTES} bytes)"
            )
            return
        body = self.rfile.read(content_len)

        try:
            request = json.loads(body)
        except json.JSONDecodeError:
            self.send_json_error(-32700, "Parse error")
            return

        auth_token = _extract_bearer(self.headers)
        response = handle_jsonrpc(request, auth_token=auth_token)

        if response is None:
            # Notification — no response body
            self.send_response(204)
            self.end_headers()
            return

        body_bytes = json.dumps(response).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", len(body_bytes))
        self.end_headers()
        self.wfile.write(body_bytes)

    def do_GET(self):
        if self.path == "/mcp":
            # Info endpoint
            info = {
                "name": SERVER_NAME,
                "version": VERSION,
                "protocol": PROTOCOL_VERSION,
                "transport": "streamable-http",
                "profileId": PROFILE_ID,
                "profile": minimal_profile(),
                "tools": len(TOOLS),
                "resources": len(RESOURCES),
            }
            body = json.dumps(info, indent=2).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", len(body))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def send_json_error(self, code, message):
        resp = json.dumps({
            "jsonrpc": "2.0", "id": None,
            "error": {"code": code, "message": message}
        }).encode("utf-8")
        self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", len(resp))
        self.end_headers()
        self.wfile.write(resp)

    def log_message(self, format, *args):
        pass


def main():
    parser = argparse.ArgumentParser(description="DCENTos MCP Server")
    parser.add_argument("--port", type=int, default=3000, help="MCP port (default: 3000)")
    # FO-1 (SEC-W24-3): default bind is loopback. The MCP server exposes raw
    # hardware access (FPGA registers, I2C, GPIO, pool switch); a safe default
    # must be baked into the program, not just the init script. An operator can
    # still pass --bind 0.0.0.0 explicitly, but the release-image token gate on
    # write tools (see handle_jsonrpc) then protects the mutating surface.
    parser.add_argument("--bind", default="127.0.0.1", help="Bind address (default: 127.0.0.1)")
    args = parser.parse_args()

    server = http.server.HTTPServer((args.bind, args.port), MCPHandler)
    print(f"DCENTos MCP Server v{VERSION}")
    print(f"  Endpoint: http://{args.bind}:{args.port}/mcp")
    print(f"  Protocol: MCP {PROTOCOL_VERSION} (Streamable HTTP)")
    print(f"  Tools:    {len(TOOLS)}")
    print(f"  Resources: {len(RESOURCES)}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nShutting down.")
        server.server_close()


if __name__ == "__main__":
    main()
