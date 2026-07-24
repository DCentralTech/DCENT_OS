#!/usr/bin/env python3
"""
DCENTos Web Dashboard — Lightweight HTTP Server + API
D-Central Technologies, 2026

Serves the web dashboard and provides JSON API endpoints for system status,
fan control, FPGA monitoring, and hardware diagnostics.

Runs on port 80 by default (configurable via --port).
Uses ThreadingMixIn for non-blocking request handling.

API Endpoints:
  GET  /api/status    — Full system status (JSON)
  GET  /api/health    — Simple alive check
  GET  /api/fan       — Fan status (PWM + RPM)
  POST /api/fan       — Set fan speed (AM2 home cap 0-30, legacy S9 fallback 0-127)
  GET  /              — Dashboard HTML page
  GET  /static/*      — Static assets

Future: CGMiner-compatible API on port 4028 for pyasic compatibility.
"""

import http.server
import http.client
import hashlib
import hmac
import json
import os
import re
import subprocess
import sys
import time
import socket
import argparse
from http.server import HTTPServer
from socketserver import ThreadingMixIn
from pathlib import Path

# dcentrald reverse proxy target
DCENTRALD_HOST = "127.0.0.1"
DCENTRALD_PORT = 8080
DASHBOARD_PROXY_HEADER = "X-Dcentos-Dashboard-Proxy"
RELEASE_IMAGE_MARKER = "/etc/dcentos/release-image"
AUTH_CHECK_PATH = "/api/debug/log?lines=1"
AUTH_FILE = "/data/dcent/auth.json"

# SEC-W24-1: per-boot dashboard-proxy trust nonce.
#
# The daemon (auth.rs) treats a loopback request carrying this header as a
# trusted same-host proxy and bypasses bearer auth. The header VALUE used to be
# the static string "1", which is forgeable by any LAN client (we have no auth
# of our own on :80). S80dashboard now mints a random per-boot secret into a
# root-only tmpfs file before launching us; we read it and send it as the
# header value. On a PRODUCTION/release image the daemon rejects anything that
# isn't this exact nonce. On a DEV/LAB image (no release marker) the daemon
# still accepts the legacy "1", so when the file is unreadable we fall back to
# "1" and dev behaviour is byte-identical to before.
PROXY_NONCE_FILE = "/run/dcentos/proxy_nonce"
_PROXY_NONCE_CACHE = {"value": None, "loaded": False}


def proxy_header_value():
    """Return the dashboard-proxy header value to forward to dcentrald.

    Reads the per-boot nonce minted by S80dashboard. Cached for the process
    lifetime (the file is stable for the boot). Falls back to the legacy
    static "1" if the file is missing/unreadable so DEV images keep working
    byte-identically — on a release image the daemon rejects "1" anyway.
    """
    if not _PROXY_NONCE_CACHE["loaded"]:
        value = "1"
        try:
            with open(PROXY_NONCE_FILE, "r") as fh:
                nonce = fh.read().strip()
            if nonce:
                value = nonce
        except OSError:
            pass
        _PROXY_NONCE_CACHE["value"] = value
        _PROXY_NONCE_CACHE["loaded"] = True
    return _PROXY_NONCE_CACHE["value"]


def release_image():
    """Return true for production/release images."""
    return os.path.exists(RELEASE_IMAGE_MARKER)


def bearer_auth_header(headers):
    """Extract a Bearer Authorization header from an HTTP header mapping."""
    value = headers.get("Authorization")
    if not value or not value.startswith("Bearer "):
        return None
    return value


def build_dcentrald_proxy_headers(headers, client_ip):
    """Build headers for the dcentrald reverse proxy.

    Do not stamp DASHBOARD_PROXY_HEADER here. server.py is LAN-facing and has no
    auth database of its own; adding the root-only loopback nonce to every
    proxied request turns the dashboard into a confused deputy. Normal browser
    traffic must authenticate with the real Bearer token, which is forwarded
    below when present.
    """
    out = {}
    for name in ("Authorization", "Accept", "Content-Type", "Origin", "Referer", "Sec-Fetch-Site"):
        value = headers.get(name)
        if value:
            out[name] = value
    host = headers.get("Host")
    if host:
        out["Host"] = host
        out["X-Forwarded-Host"] = host
    out["X-Forwarded-For"] = client_ip
    out["X-Forwarded-Proto"] = "http"
    return out


def validate_bearer_with_dcentrald(auth_header):
    """Check a Bearer token against a protected read-only daemon endpoint."""
    conn = None
    try:
        conn = http.client.HTTPConnection(DCENTRALD_HOST, DCENTRALD_PORT, timeout=3)
        conn.request(
            "GET",
            AUTH_CHECK_PATH,
            headers={"Authorization": auth_header, "Accept": "application/json"},
        )
        resp = conn.getresponse()
        resp.read()
        return 200 <= resp.status < 300
    except Exception:
        return None
    finally:
        if conn is not None:
            conn.close()


def bearer_token(auth_header):
    """Extract the token string from a Bearer Authorization value."""
    if not auth_header or not auth_header.startswith("Bearer "):
        return None
    token = auth_header[len("Bearer "):].strip()
    return token or None


def _session_unexpired(session):
    expires_at = str(session.get("expires_at") or "").strip()
    if not expires_at:
        return True
    try:
        return int(expires_at) > int(time.time())
    except ValueError:
        return False


def bearer_authorized_by_auth_file(auth_header):
    """Fallback token check for local controls when dcentrald is down."""
    token = bearer_token(auth_header)
    if not token:
        return False
    try:
        with open(AUTH_FILE, "r", encoding="utf-8") as f:
            auth = json.load(f)
    except Exception:
        return False

    legacy = str(auth.get("api_token") or "")
    if legacy and hmac.compare_digest(token, legacy):
        return True

    token_hash = hashlib.sha256(token.encode("utf-8")).hexdigest()
    for session in auth.get("sessions") or []:
        if not isinstance(session, dict):
            continue
        if str(session.get("revoked_at") or "").strip():
            continue
        if str(session.get("role") or "admin") != "admin":
            continue
        if not _session_unexpired(session):
            continue
        if hmac.compare_digest(str(session.get("token_hash") or ""), token_hash):
            return True
    return False


def local_control_authorized(headers):
    """Authorize server.py's local POST controls."""
    if not release_image():
        return True
    auth_header = bearer_auth_header(headers)
    if not auth_header:
        return False
    daemon_verdict = validate_bearer_with_dcentrald(auth_header)
    if daemon_verdict is not None:
        return daemon_verdict
    return bearer_authorized_by_auth_file(auth_header)

VERSION_FILE = Path("/etc/dcentos-version")
VERSION = VERSION_FILE.read_text().strip() if VERSION_FILE.exists() else "dev"
WEB_DIR = Path(__file__).parent
STATIC_DIR = WEB_DIR / "static"
# W5.1 (2026-05-07): canonical dashboard SPA install path. The Buildroot
# post-build hook copies DCENT_OS_Antminer/dashboard/dist/index.html here
# instead of into the daemon binary (`include_str!` was retired). Falls
# back to STATIC_DIR / index.html so partial upgrades still serve a UI.
DASHBOARD_DIR = Path("/usr/share/dcentos-dashboard")
DASHBOARD_INDEX = DASHBOARD_DIR / "index.html"
DASHBOARD_BANNER_TAG = b'<script src="/static/diagnostic-banner.js" defer></script>'
DASHBOARD_LEGACY_BANNER_TAG = b'<script src="/static/diagnostic-banner.js"></script>'
START_TIME = time.time()

# dcentrald log file (used by /api/dashboard/health to surface tail to the UI)
DCENTRALD_LOG = "/tmp/dcentrald.log"
DCENTRALD_PIDFILE = "/var/run/dcentrald.pid"
DCENTRALD_CHILD_PIDFILE = "/var/run/dcentrald-child.pid"

def _dashboard_sidecar(path, suffix):
    return Path(str(path) + suffix)


def _accepts_gzip(header_value):
    for part in str(header_value or "").split(","):
        bits = [item.strip() for item in part.split(";")]
        token = bits[0].lower()
        if token not in ("gzip", "*"):
            continue
        q = 1.0
        for param in bits[1:]:
            if not param.lower().startswith("q="):
                continue
            try:
                q = float(param.split("=", 1)[1])
            except ValueError:
                q = 0.0
        if q > 0:
            return True
    return False


def _dashboard_sha256(path):
    try:
        raw = _dashboard_sidecar(path, ".sha256").read_text().strip().split()[0]
    except (FileNotFoundError, IndexError, OSError):
        return None
    if re.fullmatch(r"[0-9a-fA-F]{64}", raw):
        return raw.lower()
    return None


def _etag_matches(header_value, etag):
    for item in str(header_value or "").split(","):
        token = item.strip()
        if token == "*" or token == etag:
            return True
    return False

def run_cmd(cmd, timeout=3):
    """Run a shell command and return stdout, or empty string on failure."""
    try:
        result = subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return result.stdout.strip()
    except Exception:
        return ""
















def read_sysfs(path):
    """Read a sysfs file, return stripped text or empty string."""
    try:
        with open(path, "r") as f:
            return f.read().strip()
    except Exception:
        return ""














def get_dcentrald_pid():
    """Return dcentrald PID if running, else None.

    Tries the child pidfile first (the actual binary, not the wrapper shell),
    then the wrapper pidfile, then falls back to `pidof`.
    """
    for pidfile in (DCENTRALD_CHILD_PIDFILE, DCENTRALD_PIDFILE):
        try:
            with open(pidfile, "r") as f:
                pid = int(f.read().strip())
            if pid > 0:
                # /proc check — confirms the pid is actually alive
                if os.path.isdir(f"/proc/{pid}"):
                    # Sanity: only count it if cmdline contains "dcentrald".
                    try:
                        with open(f"/proc/{pid}/cmdline", "rb") as cf:
                            cmdline = cf.read().decode("utf-8", "replace")
                        if "dcentrald" in cmdline:
                            return pid
                    except Exception:
                        return pid  # cmdline unreadable — trust the pidfile
        except Exception:
            pass

    # Fallback: pidof
    raw = run_cmd("pidof dcentrald", timeout=2)
    if raw:
        try:
            # pidof can return multiple — take first
            return int(raw.split()[0])
        except (ValueError, IndexError):
            return None
    return None


def get_dcentrald_uptime(pid):
    """Best-effort dcentrald uptime in seconds, derived from /proc/<pid>/stat."""
    if not pid:
        return None
    try:
        with open(f"/proc/{pid}/stat", "r") as f:
            fields = f.read().split()
        # field 22 (1-indexed) = starttime in clock ticks since boot
        starttime_ticks = int(fields[21])
        clk_tck_raw = run_cmd("getconf CLK_TCK 2>/dev/null", timeout=1)
        try:
            clk_tck = int(clk_tck_raw) if clk_tck_raw else 100
        except ValueError:
            clk_tck = 100
        with open("/proc/uptime", "r") as f:
            sys_uptime = float(f.read().split()[0])
        proc_uptime = sys_uptime - (starttime_ticks / clk_tck)
        return max(0, int(proc_uptime))
    except Exception:
        return None


def tail_log(path, lines=20):
    """Cheap log tail. Returns list of last N lines (empty list on failure)."""
    try:
        out = run_cmd(f"tail -n {int(lines)} {path} 2>/dev/null", timeout=2)
        if not out:
            return []
        return [ln for ln in out.split("\n") if ln]
    except Exception:
        return []


def _probe_dcentrald_api():
    """Quick TCP probe of dcentrald HTTP API on DCENTRALD_HOST:DCENTRALD_PORT.

    Returns True only if a TCP connection succeeds within 500ms. The dashboard
    needs the proxy to actually be reachable; a process that's alive but never
    bound :8080 (e.g. S19j hybrid mode short-circuits daemon.rs::run()) should
    NOT be reported as alive — otherwise the React app stops showing the
    graceful-degrade banner and races against /api/* 503 responses.
    """
    try:
        with socket.create_connection((DCENTRALD_HOST, DCENTRALD_PORT), timeout=0.5):
            return True
    except (OSError, socket.timeout):
        return False


def _last_error_line(log_lines):
    """Return the last line containing 'ERROR' or 'error=' from log tail."""
    for ln in reversed(log_lines):
        if "ERROR" in ln or "error=" in ln:
            return ln
    return None


def get_braiins_glitch_mirror():
    """Report unavailable until dcentrald publishes this FPGA snapshot."""
    return {
        "status": "unavailable",
        "source": "dcentrald snapshot",
        "hardware_access_attempted": False,
        "reason": "Braiins glitch-mirror registers are not published by the runtime owner.",
    }


def get_slot_info():
    """U-Boot env: which firmware slot is active + upgrade_stage."""
    out = {}
    for var in ("firmware", "upgrade_stage"):
        v = run_cmd(f"fw_printenv -n {var} 2>/dev/null", timeout=1)
        out[var] = v if v else None
    return out


def get_network_info():
    """Hostname + first non-loopback IPv4 + MAC."""
    hostname = run_cmd("hostname 2>/dev/null", timeout=1) or "unknown"
    ip_out = run_cmd("ip -4 -o addr show eth0 2>/dev/null", timeout=1)
    ip = None
    if ip_out:
        parts = ip_out.split()
        if len(parts) >= 4:
            ip = parts[3].split("/")[0]
    mac = read_sysfs("/sys/class/net/eth0/address").upper() or None
    return {"hostname": hostname, "ip": ip, "mac": mac}


_VERSION_CACHE = {"text": None, "ts": 0}
_VERSION_CACHE_TTL = 30


def get_dcentrald_version():
    """Cached `dcentrald --version` head (refreshes every 30s)."""
    now = time.time()
    if _VERSION_CACHE["text"] and (now - _VERSION_CACHE["ts"]) < _VERSION_CACHE_TTL:
        return _VERSION_CACHE["text"]
    raw = run_cmd("/usr/local/bin/dcentrald --version 2>&1 | head -3", timeout=2)
    if not raw:
        # Try /tmp/dcentrald.new (developer overlay binary)
        raw = run_cmd("/tmp/dcentrald.new --version 2>&1 | head -3", timeout=2)
    _VERSION_CACHE["text"] = raw or None
    _VERSION_CACHE["ts"] = now
    return _VERSION_CACHE["text"]


def get_dashboard_health():
    """Return the daemon-health JSON used by the React banner.

    This endpoint is served by server.py directly (not proxied), so it works
    even when dcentrald is dead — which is exactly when the dashboard needs it.
    Health = (process is up) AND (REST API actually bound and accepting TCP).

    Diagnostic surface (added 2026-04-29 per .74 fw=0x86 live test): when
    dcentrald is dead, the dashboard relies on these fields to show what
    state the unit is in without requiring SSH.
    """
    pid = get_dcentrald_pid()
    uptime_s = get_dcentrald_uptime(pid)
    api_bound = _probe_dcentrald_api() if pid is not None else False
    log_lines = tail_log(DCENTRALD_LOG, lines=50)
    if pid is None:
        dcentrald_status = "dead"
    elif api_bound:
        dcentrald_status = "alive"
    else:
        dcentrald_status = "starting"
    return {
        "alive": pid is not None and api_bound,
        "pid": pid,
        "uptime_s": uptime_s,
        "api_bound": api_bound,
        "dcentrald_status": dcentrald_status,
        "last_log_lines": log_lines,  # legacy field, kept for compat
        "dcentrald_log_tail": log_lines,
        "dcentrald_last_error": _last_error_line(log_lines),
        "slot": get_slot_info(),
        "network": get_network_info(),
        "braiins_glitch_mirror": get_braiins_glitch_mirror(),
        "dcentrald_version": get_dcentrald_version(),
        "last_health_probe_ts": int(time.time()),
        "version": VERSION,
    }


class ThreadedHTTPServer(ThreadingMixIn, HTTPServer):
    """Handle requests in a separate thread — prevents blocking on slow probes."""
    daemon_threads = True


class DCENTosHandler(http.server.SimpleHTTPRequestHandler):
    """HTTP request handler with API endpoints + reverse proxy to dcentrald."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(STATIC_DIR), **kwargs)

    def _proxy_to_dcentrald(self, method="GET", body=None):
        """Forward request to dcentrald on port 8080.

        Returns:
            True  — proxy succeeded, response already written.
            False — could not connect (handled by caller as graceful fallback).

        On unrecoverable proxy errors (read timeout, malformed response after
        connecting), we still return a 200 with `{_disconnected: true,
        _error: ...}` so the React client can detect the daemon-down state
        without throwing.
        """
        try:
            conn = http.client.HTTPConnection(DCENTRALD_HOST, DCENTRALD_PORT, timeout=10)
            headers = build_dcentrald_proxy_headers(self.headers, self.client_address[0])
            conn.request(method, self.path, body=body, headers=headers)
            resp = conn.getresponse()
            resp_body = resp.read()

            self.send_response(resp.status)
            self.send_header("Content-Type", resp.getheader("Content-Type", "application/json"))
            self.send_header("Content-Length", len(resp_body))
            self.end_headers()
            self.wfile.write(resp_body)
            conn.close()
            return True
        except (ConnectionRefusedError, ConnectionResetError, socket.timeout, OSError):
            # dcentrald not listening or hung — let caller handle fallback.
            return False
        except Exception:
            return False

    def _send_disconnect_json(self, reason):
        """Return a 503 with a JSON disconnect marker the React client recognises."""
        body = {
            "_disconnected": True,
            "_error": reason,
            "_path": self.path,
            "_ts": int(time.time()),
        }
        self.send_json(body, status=503)

    def do_GET(self):
        if self.path.startswith("/api/"):
            # ALWAYS-LOCAL endpoints — must work even when dcentrald is dead.
            # That's the whole point of the diagnostic dashboard.
            if self.path == "/api/dashboard/health":
                self.send_json(get_dashboard_health())
                return
            if self.path.startswith("/api/dashboard/log"):
                # /api/dashboard/log?lines=N (default 100, max 1000)
                lines = 100
                if "?" in self.path:
                    qs = self.path.split("?", 1)[1]
                    for kv in qs.split("&"):
                        if kv.startswith("lines="):
                            try:
                                lines = max(1, min(1000, int(kv.split("=", 1)[1])))
                            except ValueError:
                                pass
                self.send_json({
                    "lines": tail_log(DCENTRALD_LOG, lines=lines),
                    "path": DCENTRALD_LOG,
                    "ts": int(time.time()),
                })
                return
            if self.path == "/api/dashboard/probe":
                # Full diagnostic snapshot — same data as health but explicit
                self.send_json({
                    "braiins_glitch_mirror": get_braiins_glitch_mirror(),
                    "slot": get_slot_info(),
                    "network": get_network_info(),
                    "dcentrald_version": get_dcentrald_version(),
                    "ts": int(time.time()),
                })
                return
            # Try forwarding to dcentrald first; fall back to local handlers
            if self._proxy_to_dcentrald("GET"):
                return
            # Fallback: ALL /api/* return a structured disconnect marker.
            # Don't substitute server.py's own /api/status here — its schema
            # (chains as dict, no hashrate, no pool) doesn't match what the
            # dashboard expects from dcentrald (chains as array). The dashboard's
            # graceful-degrade client recognises `_disconnected: true` and
            # renders the DEAD banner; receiving a 200 with the wrong shape
            # causes a TypeError ("chains.find is not a function") that crashes
            # the React tree before the banner can mount.
            if self.path == "/api/health":
                # Cheap liveness check the operator can hit from curl —
                # server.py is up even when dcentrald isn't.
                self.send_json({"status": "alive", "version": VERSION})
            else:
                self._send_disconnect_json("dcentrald not reachable on 127.0.0.1:8080")
        elif self.path == "/" or self.path == "/index.html":
            # W5.1: prefer the canonical /usr/share/dcentos-dashboard/index.html
            # produced by Buildroot post-build (copy of dashboard/dist/index.html).
            # Fall back to the legacy STATIC_DIR copy so a partial overlay
            # (server.py refreshed but no dashboard rebuild yet) still serves
            # a UI rather than 404.
            if DASHBOARD_INDEX.exists():
                self.serve_file(DASHBOARD_INDEX, "text/html")
            else:
                self.serve_file(STATIC_DIR / "index.html", "text/html")
        elif self.path == "/diagnostic" or self.path == "/diagnostic.html":
            self.serve_file(STATIC_DIR / "diagnostic.html", "text/html")
        elif self.path == "/recovery" or self.path == "/recovery.html":
            # GROUP B: static daemon-down recovery page. Served directly by
            # server.py (no dcentrald dependency) so an operator hitting the web
            # port when the daemon is dead gets a self-contained recovery page
            # with SSH/restart/log/rollback guidance instead of a dead
            # connection. The default static handler (rooted at STATIC_DIR) also
            # serves the raw file at /recovery.html, so this route is a clean
            # alias. Mirrors how LuxOS/BraiinsOS serve a static fallback. The
            # banner-injection in serve_file() is skipped for this page (it
            # already shows the daemon-down state statically, no JS required).
            self.serve_file(STATIC_DIR / "recovery.html", "text/html")
        else:
            super().do_GET()

    def do_POST(self):
        content_len = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_len) if content_len > 0 else None

        if self.path.startswith("/api/"):
            # ALWAYS-LOCAL endpoints — work even when dcentrald is dead.
            if self.path == "/api/dashboard/restart-dcentrald":
                if not local_control_authorized(self.headers):
                    self.send_json({
                        "status": "unauthorized",
                        "error": "release image requires a valid Bearer token for dashboard-local control endpoints",
                    }, status=401)
                    return
                self.send_json({
                    "status": "manual_resolution_required",
                    "error": "automatic Amlogic restart is disabled; resolve electrical disposition through /recovery before a guarded start",
                }, status=409)
                return
            if self.path == "/api/dashboard/report-ip":
                if not local_control_authorized(self.headers):
                    self.send_json({
                        "status": "unauthorized",
                        "error": "release image requires a valid Bearer token for dashboard-local control endpoints",
                    }, status=401)
                    return
                # Trigger ip_reporter daemon (SIGUSR1) OR fall back to
                # one-shot subprocess if daemon isn't running.
                pid = None
                try:
                    with open("/var/run/ip_reporter.pid", "r") as f:
                        pid = int(f.read().strip())
                    os.kill(pid, 10)  # SIGUSR1
                    self.send_json({
                        "status": "broadcasting",
                        "method": "signal",
                        "pid": pid,
                        "ts": int(time.time()),
                    })
                    return
                except (FileNotFoundError, ProcessLookupError, ValueError, PermissionError) as e:
                    # Fall back to one-shot
                    try:
                        subprocess.Popen(
                            ["/usr/bin/python3", "/root/web/ip_reporter.py", "--once"],
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            start_new_session=True,
                        )
                        self.send_json({
                            "status": "broadcasting",
                            "method": "one-shot",
                            "fallback_reason": str(e),
                            "ts": int(time.time()),
                        })
                        return
                    except Exception as e2:
                        self.send_json({
                            "status": "error",
                            "error": str(e2),
                            "ts": int(time.time()),
                        }, status=500)
                        return
            # Try forwarding to dcentrald first
            if self._proxy_to_dcentrald("POST", body):
                return
            self._send_disconnect_json("dcentrald not reachable on 127.0.0.1:8080")
        else:
            self.send_error(404)

    def do_OPTIONS(self):
        """Handle CORS preflight for all paths."""
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type, Authorization")
        self.end_headers()

    def send_json(self, data, status=200):
        body = json.dumps(data, indent=2).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", len(body))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def serve_file(self, path, content_type):
        try:
            content = path.read_bytes()
            is_dashboard_html = content_type == "text/html" and path.name == "index.html"
            banner_baked = (not is_dashboard_html) or DASHBOARD_BANNER_TAG in content
            if is_dashboard_html and not banner_baked:
                if DASHBOARD_LEGACY_BANNER_TAG in content:
                    content = content.replace(DASHBOARD_LEGACY_BANNER_TAG, DASHBOARD_BANNER_TAG, 1)
                elif b'</body>' in content:
                    content = content.replace(b'</body>', DASHBOARD_BANNER_TAG + b'</body>', 1)
                else:
                    content = content + DASHBOARD_BANNER_TAG

            etag = None
            if is_dashboard_html and banner_baked:
                sha256 = _dashboard_sha256(path)
                if sha256:
                    gzip_path = _dashboard_sidecar(path, ".gz")
                    gzip_fresh = (
                        gzip_path.exists()
                        and gzip_path.stat().st_mtime >= path.stat().st_mtime
                    )
                    if (
                        gzip_fresh
                        and _accepts_gzip(self.headers.get("Accept-Encoding", ""))
                    ):
                        etag = '"' + sha256[:16] + '-gz"'
                        if _etag_matches(self.headers.get("If-None-Match", ""), etag):
                            self.send_response(304)
                            self.send_header("ETag", etag)
                            self.send_header("Cache-Control", "no-cache")
                            self.send_header("Vary", "Accept-Encoding")
                            self.end_headers()
                            return
                        content = gzip_path.read_bytes()
                        self.send_response(200)
                        self.send_header("Content-Type", content_type)
                        self.send_header("Content-Encoding", "gzip")
                        self.send_header("Content-Length", len(content))
                        self.send_header("ETag", etag)
                        self.send_header("Cache-Control", "no-cache")
                        self.send_header("Vary", "Accept-Encoding")
                        self.end_headers()
                        self.wfile.write(content)
                        return

                    etag = '"' + sha256[:16] + '"'
                    if _etag_matches(self.headers.get("If-None-Match", ""), etag):
                        self.send_response(304)
                        self.send_header("ETag", etag)
                        self.send_header("Cache-Control", "no-cache")
                        self.send_header("Vary", "Accept-Encoding")
                        self.end_headers()
                        return

            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", len(content))
            if is_dashboard_html:
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Vary", "Accept-Encoding")
                if etag:
                    self.send_header("ETag", etag)
            self.end_headers()
            self.wfile.write(content)
        except FileNotFoundError:
            self.send_error(404)

    def log_message(self, format, *args):
        """Suppress default logging to stderr."""
        pass


def main():
    parser = argparse.ArgumentParser(description="DCENTos Web Dashboard")
    parser.add_argument("--port", type=int, default=80, help="HTTP port (default: 80)")
    parser.add_argument("--bind", default="0.0.0.0", help="Bind address")
    args = parser.parse_args()

    server = ThreadedHTTPServer((args.bind, args.port), DCENTosHandler)
    hostname = socket.gethostname()
    print(f"DCENTos Dashboard v{VERSION} — http://{args.bind}:{args.port}/")
    print(f"  Hostname: {hostname}")
    print(f"  API:      http://{args.bind}:{args.port}/api/status")
    print(f"  Fan API:  http://{args.bind}:{args.port}/api/fan")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nShutting down.")
        server.server_close()


if __name__ == "__main__":
    main()
