#!/usr/bin/env python3
"""DCENT_OS IP Reporter — broadcasts to Bitmain IP Reporter (UDP 14235) +
DCENT_Toolbox extended format (UDP 14237).

Two trigger modes:
  1. SIGUSR1 (sent by /api/dashboard/report-ip HTTP handler or by a
     button-watcher script in the future)
  2. Foreground one-shot (`./ip_reporter.py --once`) — used as the
     fallback when the daemon isn't running.

Broadcast cadence: every press sends 5 packets at 200ms intervals
(Bitmain IP Reporter occasionally drops the first one).

Bitmain wire format (port 14235):
  Comma-separated text: "Antminer S19j Pro,192.168.x.x,XX:XX:XX:XX:XX:XX"
  This matches the format the stock /usr/bin/ipreport broadcasts so
  Bitmain's desktop IP Reporter tool picks up the unit.

DCENT extended format (port 14237):
  JSON with model, hostname, ip, mac, firmware, dcentrald_status,
  hashrate_th, uptime_s, chains_alive, dspic_fw_at_0x21. dcent-toolbox
  `dcent listen` parses this for full fleet discovery.
"""
import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time

BITMAIN_PORT = 14235
DCENT_PORT = 14237
PACKET_BURST = 5
PACKET_INTERVAL = 0.2

PLATFORM_MODEL_MAP = {
    "am3-aml-s19k": "Antminer S19K Pro",
    "am3-aml-s19xp": "Antminer S19 XP",
    "am3-aml-s21": "Antminer S21",
    "am3-aml": "Antminer S21",
    "am2-s19j": "Antminer S19j Pro",
    "am1-s9": "Antminer S9",
    "s9": "Antminer S9",
}


def run_cmd(cmd, timeout=2):
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return r.stdout.strip()
    except Exception:
        return ""


def model_from_platform_token(token):
    token = (token or "").strip()
    lowered = token.lower()
    if lowered in PLATFORM_MODEL_MAP:
        return PLATFORM_MODEL_MAP[lowered]
    if "am3-aml-s19k" in lowered:
        return PLATFORM_MODEL_MAP["am3-aml-s19k"]
    if "am3-aml-s19xp" in lowered:
        return PLATFORM_MODEL_MAP["am3-aml-s19xp"]
    if "am3-aml-s21" in lowered:
        return PLATFORM_MODEL_MAP["am3-aml-s21"]
    if "am2-s19j" in lowered or "s19j" in lowered:
        return PLATFORM_MODEL_MAP["am2-s19j"]
    if "am1-s9" in lowered or lowered == "s9":
        return PLATFORM_MODEL_MAP["am1-s9"]
    return token


def detect_model():
    """Best-effort model detection: check /etc/dcentos-platform, fall back to
    /proc/device-tree/model, fall back to 'Antminer S19j Pro' (the unit
    we're targeting)."""
    for path in ("/etc/dcentos-platform", "/etc/dcentos-model"):
        try:
            with open(path) as f:
                v = f.read().strip()
            if v:
                # Map our internal codenames to user-facing names
                return model_from_platform_token(v)
        except FileNotFoundError:
            continue
    try:
        with open("/proc/device-tree/model", "rb") as f:
            v = f.read().strip(b"\x00").decode("utf-8", "replace").strip()
        if v:
            return v
    except FileNotFoundError:
        pass
    return "Antminer S19j Pro"


def get_network_info():
    hostname = run_cmd("hostname") or "dcentos"
    ip = ""
    out = run_cmd("ip -4 -o addr show eth0")
    if out:
        parts = out.split()
        if len(parts) >= 4:
            ip = parts[3].split("/")[0]
    if not ip:
        ip = "0.0.0.0"
    mac = ""
    try:
        with open("/sys/class/net/eth0/address") as f:
            mac = f.read().strip().upper()
    except FileNotFoundError:
        mac = "00:00:00:00:00:00"
    return hostname, ip, mac


def get_dcent_extra():
    """Extended fields for the DCENT_Toolbox JSON broadcast. All fields are
    optional; collect best-effort and skip on error."""
    extra = {}
    # Slot (firmware A/B)
    slot = run_cmd("fw_printenv -n firmware 2>/dev/null")
    if slot:
        extra["slot"] = slot
    # dcentrald status
    pid = run_cmd("pidof dcentrald 2>/dev/null")
    extra["dcentrald_status"] = "alive" if pid else "dead"
    if pid:
        extra["dcentrald_pid"] = int(pid.split()[0])
    # dsPIC FW byte at 0x21 (single 1-byte read; non-destructive)
    dspic_raw = run_cmd("i2cget -y 0 0x21 b 2>/dev/null")
    if dspic_raw and dspic_raw.startswith("0x"):
        extra["dspic_fw_at_0x21"] = dspic_raw
    # Uptime
    try:
        with open("/proc/uptime") as f:
            extra["uptime_s"] = int(float(f.read().split()[0]))
    except (FileNotFoundError, ValueError):
        pass
    # Hashrate via dcentrald cgminer-API summary (if dcentrald is alive)
    if pid:
        # Best-effort, fast timeout
        out = run_cmd("(echo '{\"command\":\"summary\"}' | nc -w 1 127.0.0.1 4028) 2>/dev/null")
        if out and "MHS" in out:
            # Naive parse; failure-tolerant
            for tok in out.split(","):
                if tok.startswith("MHS av="):
                    try:
                        mhs = float(tok.split("=", 1)[1])
                        extra["hashrate_th"] = round(mhs / 1_000_000.0, 3)
                    except ValueError:
                        pass
                    break
    return extra


def broadcast(model=None):
    if model is None:
        model = detect_model()
    hostname, ip, mac = get_network_info()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        bm_payload = f"{model},{ip},{mac}".encode()
        dcent_payload = json.dumps({
            "model": model,
            "hostname": hostname,
            "ip": ip,
            "mac": mac,
            "firmware": "DCENT_OS",
            "extra": get_dcent_extra(),
            "ts": int(time.time()),
        }).encode()
        for _ in range(PACKET_BURST):
            try:
                sock.sendto(bm_payload, ("255.255.255.255", BITMAIN_PORT))
            except OSError:
                pass
            try:
                sock.sendto(dcent_payload, ("255.255.255.255", DCENT_PORT))
            except OSError:
                pass
            time.sleep(PACKET_INTERVAL)
        print(f"[ip_reporter] broadcast {model} {ip} {mac} ({PACKET_BURST}x)", flush=True)
    finally:
        sock.close()


def main():
    ap = argparse.ArgumentParser(description="DCENT_OS IP Reporter (Bitmain + DCENT)")
    ap.add_argument("--once", action="store_true", help="Broadcast once and exit")
    ap.add_argument("--model", default=None, help="Override model (default: auto-detect)")
    args = ap.parse_args()

    if args.once:
        broadcast(args.model)
        return

    # Daemon mode: SIGUSR1 → broadcast.
    # Capture model lazily so a model file added after start picks up.
    def _on_usr1(*_):
        try:
            broadcast(args.model)
        except Exception as e:
            print(f"[ip_reporter] broadcast error: {e}", file=sys.stderr, flush=True)

    signal.signal(signal.SIGUSR1, _on_usr1)
    print(f"[ip_reporter] daemon started (PID {os.getpid()}) — send SIGUSR1 to broadcast", flush=True)

    # Sleep loop. We don't want to block signal delivery, so use a long
    # but interruptible sleep.
    while True:
        try:
            time.sleep(3600)
        except KeyboardInterrupt:
            break


if __name__ == "__main__":
    main()
