#!/usr/bin/env python3
"""
Tiny preview server that serves dist/index.html for the SPA AND mocks the
dcentrald /api/* endpoints so the UI renders with realistic data without
needing a real backend. Used only for design-handoff visual previews.

Usage:
  python scripts/preview-with-mocks.py [--port 4173]
"""

import argparse, json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

DIST = Path(__file__).resolve().parent.parent / "dist"
INDEX = DIST / "index.html"
CYPRESS_FIXTURES = Path(__file__).resolve().parent / "preview-fixtures.json"

NOW_MS = int(time.time() * 1000)

# Cypress fixtures (statusBody, systemInfoBody, thermalPostureBody,
# miningWorkPostureBody, networkBlockBody, miningPipelineManifestBody)
# dumped by scripts/dump-cypress-fixtures.mjs. These match the shapes
# the dashboard's typed components expect.
try:
    CY = json.loads(CYPRESS_FIXTURES.read_text(encoding="utf-8"))
except Exception:
    CY = {}

# Override the network-block fixture to render the new DCENT_axe-style hero
# with real values instead of "unavailable" so the preview shows the redesign.
LIVE_BLOCK = {
    "status": "ok", "read_only": True, "internet_dependency": False, "available": True,
    "source": "local_node", "source_label": "Bitcoin Core RPC",
    "fetched_at_ms": NOW_MS - 1200, "cache_ttl_ms": 30000,
    "block_height": 893214, "height": 893214,
    "block_hash":  "0000000000000000000234a5c891e5f2b7c47fa9b6e1c4d3e5f6a7b8c9d0e1f2",
    "hash":        "0000000000000000000234a5c891e5f2b7c47fa9b6e1c4d3e5f6a7b8c9d0e1f2",
    "timestamp_ms": NOW_MS - 38000, "age_s": 38,
    "difficulty": 95672703408223,
    "previous_hash": "00000000000000000000fedcba9876543210fedcba9876543210fedcba9876ab",
    "mempool": {"available": True, "source": "local_node",
                "fee_rate_sat_vb": 18, "fastest_fee_sat_vb": 24,
                "half_hour_fee_sat_vb": 18, "hour_fee_sat_vb": 12,
                "reason": "Mempool fees from local Bitcoin Core."},
    "pool_job": {"available": True, "source": "recent_share_history", "job_id": "0x4a",
                  "last_share_timestamp_ms": NOW_MS - 30000, "difficulty": 262144,
                  "protocol_meta_present": True, "reason": "Pool job linked from recent share history."},
    "source_manifest": {
        "local_node": {"enabled": True, "configured": True, "available": True, "live_rpc": True,
                        "endpoint_label": "127.0.0.1:8332", "credential_mode": "cookie_file",
                        "request_timeout_ms": 1500, "reason": "Local node is configured and responded."},
        "public_fallback": {"enabled": False, "available": False, "reason": "Public fallback disabled by default."},
        "cache": {"enabled": True, "ttl_ms": 30000, "age_ms": 1200, "reason": "Fresh cache entry."},
    },
    "reasons": [], "limitations": ["Read-only dashboard surface; no network writes are performed."],
}

# RE-010 per-chip telemetry (GET /api/chips). Honest ChipHealthSnapshot shape
# (chains[].chipmap.cells[]) so the rewired ChipHeatMap renders REAL cells in
# the preview instead of the old sine-wave fabrication. A couple of degraded /
# dead cells per chain exercise the grade/color/error coloring. die_temp_c is
# intentionally omitted (firmware doesn't expose per-chip die temp yet) so the
# temperature mode shows the honest "colored by health grade" note.
def _chip_cell(idx, score, freq, errors):
    if score <= 0.0:
        grade, color = "F", "Gray"
    elif score < 0.50:
        grade, color = "D", "Red"
    elif score < 0.70:
        grade, color = "C", "Orange"
    elif score < 0.90:
        grade, color = "B", "Yellow"
    else:
        grade, color = "A", "Green"
    return {
        "index": idx, "address": (idx * 4) & 0xFF, "health_score": score,
        "grade": grade, "color": color, "frequency_mhz": freq,
        "nonce_count": 0, "crc_errors": errors,
    }

def _chip_chain(chain_id, count=126, freq=525):
    cells = []
    for i in range(count):
        if i == count - 1:
            cells.append(_chip_cell(i, 0.0, 0, 0))         # one dead chip
        elif i % 31 == 0 and i > 0:
            cells.append(_chip_cell(i, 0.62, freq, 4))      # one degraded chip
        else:
            cells.append(_chip_cell(i, 0.96, freq, 0))      # healthy
    return {
        "chain_id": chain_id, "source": "runtime_chip_health", "chip_count": count,
        "responding_chips": count - 1, "board_temp_c": 58.0 + chain_id,
        "board_hashrate_ghs": 32000.0, "board_health_score": 0.95,
        "frequency_mhz": freq, "voltage_mv": 1380, "errors": 4,
        "status": "mining",
        "chipmap": {"chain_id": chain_id, "chip_count": count,
                    "columns": 14, "rows": 9, "cells": cells},
    }

CHIPS_SNAPSHOT = {
    "report_id": "00000000-0000-0000-0000-000000000000",
    "generated_at": "2026-05-22T00:00:00Z",
    "report_type": "chip_health_snapshot", "source": "runtime_chip_health",
    "total_boards": 3, "total_chips": 378, "warnings": [], "recommendations": [],
    "chains": [_chip_chain(0), _chip_chain(1), _chip_chain(2)],
}

MOCKS = {
    "/api/network/block": LIVE_BLOCK,  # OVERRIDE: show the new hero with real data
    "/api/chips": CHIPS_SNAPSHOT,      # RE-010: honest per-chip data for ChipHeatMap
    "/api/status": CY.get("statusBody", {}),
    "/api/stats": CY.get("statusBody", {}),
    "/api/system/info": CY.get("systemInfoBody", {}),
    "/api/thermal/posture": CY.get("thermalPostureBody", {}),
    "/api/mining/work/posture": CY.get("miningWorkPostureBody", {}),
    "/api/mining/pipeline/manifest": CY.get("miningPipelineManifestBody", {}),
    "/api/setup/status": {"needs_setup": False},
    "/api/config": {"mode": {"active": "standard"}},
    "/api/system/health": {"mode": "native", "alive": True, "blockers": []},
    "/api/system/upgrade/status": {"stage": "idle", "active": False, "entries": []},
    "/api/system/restore-to-stock/status": {"state": "idle", "last_safety_findings": [],
                                             "transitions": 0, "last_backup_fw_setenv_present": True},
    "/api/pools": {"pools": [], "active": None},
    "/api/pools/failover": {"primary": None, "backup": None, "donation": None},
    "/api/history": {"history": []},
    "/api/history/shares": {"events": []},
    "/api/autotuner/status": {"enabled": False, "live_runtime": False, "stale": True, "age_s": 0},
    "/api/profiles/silicon": [],
    # Basic (Heater) mode endpoints — thermostat, BTU hero, presets, night mode.
    "/api/home/status": CY.get("heaterStatusBody", {}),
    "/api/home/presets": CY.get("heaterPresetsBody", {"presets": []}),
    "/api/home/night-mode": CY.get("nightModeBody", {}),
    "/api/home/history": {"history": []},
    "/api/dashboard/health": {"pid": 1234, "alive": True, "uptime_s": 3600,
                               "last_log_lines": ["dcentrald preview fixture"],
                               "last_health_probe_ts": NOW_MS},

    # ─── System / hardware identity (System, About, Network pages) ──────
    "/api/system/stats": CY.get("systemStatsBody", {}),
    "/api/system/asic": CY.get("systemAsicBody", {"asics": []}),
    "/api/system/boot_timeline": CY.get("systemBootTimelineBody", {}),
    "/api/hardware/pic_info": CY.get("hardwarePicInfoBody", {}),
    "/api/hardware/psu_catalog": CY.get("psuCatalogBody", {}),
    "/api/cgminer/catalog": CY.get("cgminerCatalogBody", {}),
    "/api/re/catalog/index": CY.get("reCatalogIndexBody", {}),
    "/api/network/info": CY.get("networkInfoBody", {}),
    "/api/miner/type": CY.get("minerTypeBody", {}),
    "/api/miner/pvt-table": CY.get("pvtTableBody", {}),
    "/api/boot/phase": CY.get("bootPhaseBody", {}),
    "/api/boot/timeline": CY.get("bootTimelineBody", {}),
    "/api/system/api-compatibility/manifest": CY.get("apiCompatibilityManifestBody", {}),
    "/api/competitive/readiness": CY.get("competitiveReadinessBody", {}),
    "/api/config/backup/manifest": CY.get("configBackupManifestBody", {}),

    # ─── Evidence page (audit, failure modes, recovery, diagnostics) ───
    "/api/history/audit": CY.get("historyAuditBody", {}),
    "/api/diagnostics/failure_modes": CY.get("diagnosticsFailureModesBody", {}),
    "/api/diagnostics/recovery_actions": CY.get("recoveryActionsBody", {}),
    "/api/diagnostics/shares/local_rejects": CY.get("diagnosticsLocalRejectsBody", {}),
    "/api/diagnostics/reports/recent": CY.get("recentDiagnosticReportsBody", {}),
    "/api/diagnostics/logs/manifest": CY.get("logManifestBody", {}),
    "/api/diagnostics/troubleshoot/network": CY.get("troubleshootNetworkBody", {}),
    "/api/diagnostics/troubleshoot/psu": CY.get("troubleshootPsuBody", {}),
    "/api/diagnostics/troubleshoot/fpga": CY.get("troubleshootFpgaBody", {}),

    # ─── Logs page (dcentrald log tail + manifest) ─────────────────────
    "/api/debug/log": CY.get("debugLogBody", {"lines": []}),

    # ─── Debug / Hacker tools (register + i2c probes) ──────────────────
    "/api/debug/registers": CY.get("registerReadBody", {}),
    "/api/debug/i2c": CY.get("i2cReadBody", {}),

    # ─── Mining pipeline snapshot (live publisher default-off) ─────────
    "/api/mining/pipeline/snapshot": CY.get("miningPipelineSnapshotBody", {}),

    # ─── Profiles + autotuner (Tuning page) ────────────────────────────
    "/api/profiles": CY.get("profilesBody", {"profiles": [], "active_profile": None}),
    "/api/autotuner/chip-health": CY.get("autotunerChipHealthBody", {}),
    "/api/autotuner/visibility": CY.get("autotunerVisibilityBody", {}),

    # ─── Off-Grid page (not configured, valid-shaped) ──────────────────
    "/api/offgrid/config": CY.get("offgridConfigBody", {}),
    "/api/offgrid/status": CY.get("offgridStatusBody", {}),
    "/api/offgrid/presets": CY.get("offgridPresetsBody", {"presets": []}),

    # ─── Solar / Green Mining page (not configured, valid-shaped) ──────
    "/api/solar/config": CY.get("solarConfigBody", {}),
    "/api/solar/status": CY.get("solarStatusBody", {}),
    "/api/solar/verification-history": CY.get("solarVerificationHistoryBody", {"generatedAtMs": NOW_MS, "entries": []}),

    # ─── Integrations (MQTT / Webhook config) ──────────────────────────
    "/api/config/mqtt": CY.get("mqttConfigBody", {}),
    "/api/config/webhook": CY.get("webhookConfigBody", {}),

    # ─── SV2 + Job Declaration page (disabled / not configured) ────────
    "/api/pool/sv2/status": CY.get("sv2StatusBody", {"connected": False}),
    "/api/pool/sv2/messages": CY.get("sv2MessagesBody", {"messages": [], "total": 0}),
    "/api/jd/status": CY.get("jdStatusBody", {}),

    # ─── LED control page (idle, not locating) ─────────────────────────
    "/api/led/status": CY.get("ledStatusBody", {}),
    "/api/led/patterns": CY.get("ledPatternsBody", {}),
    "/api/led/config": CY.get("ledConfigBody", {}),

    # ─── Power / PSU / donation / efficiency ───────────────────────────
    "/api/perf/efficiency": CY.get("perfEfficiencyBody", {}),
    "/api/config/power-calibration": CY.get("powerCalibrationBody", {}),
    "/api/config/psu-override": CY.get("psuOverrideBody", {}),
    "/api/config/donation": CY.get("donationConfigBody", {}),
    "/api/donation/info": CY.get("donationInfoBody", {}),
}

_LEGACY_UNUSED = {
    "/api/setup/status": {"needs_setup": False, "has_password": True, "status": "ready"},
    "/api/config": {
        "miner_name": "Preview miner",
        "pool": {"primary": {"url": "stratum+tcp://public-pool.io:21496", "user": "bc1q...preview", "password": "x"}},
        "mining": {"frequency_mhz": 525, "voltage_mv": 1380, "mode": "standard"},
        "thermal": {"target_c": 60, "fan_min_pwm": 10, "fan_max_pwm": 80},
        "donation": {"enabled": True, "percent": 2},
        "setup_complete": True,
    },
    "/api/network/block": {
        "status": "ok", "read_only": True, "internet_dependency": False, "available": True,
        "source": "local_node", "source_label": "Bitcoin Core RPC",
        "fetched_at_ms": NOW_MS - 1200, "cache_ttl_ms": 30000,
        "block_height": 893214, "height": 893214,
        "block_hash":  "0000000000000000000234a5c891e5f2b7c47fa9b6e1c4d3e5f6a7b8c9d0e1f2",
        "hash":        "0000000000000000000234a5c891e5f2b7c47fa9b6e1c4d3e5f6a7b8c9d0e1f2",
        "timestamp_ms": NOW_MS - 142000,
        "age_s": 142,
        "difficulty": 95672703408223,
        "previous_hash": "00000000000000000000fedcba9876543210fedcba9876543210fedcba9876ab",
        "mempool": {"available": True, "source": "local_node",
                     "fee_rate_sat_vb": 18, "fastest_fee_sat_vb": 24,
                     "half_hour_fee_sat_vb": 18, "hour_fee_sat_vb": 12,
                     "reason": "Mempool fees from local Bitcoin Core."},
        "pool_job": {"available": True, "source": "recent_share_history",
                     "job_id": "0x4a", "last_share_timestamp_ms": NOW_MS - 30000,
                     "difficulty": 262144, "protocol_meta_present": True,
                     "reason": "Pool job linked from recent share history."},
        "source_manifest": {
            "local_node": {"enabled": True, "configured": True, "available": True, "live_rpc": True,
                            "endpoint_label": "127.0.0.1:8332", "credential_mode": "cookie_file",
                            "request_timeout_ms": 1500, "reason": "Local node is configured and responded."},
            "public_fallback": {"enabled": False, "available": False,
                                 "reason": "Public fallback disabled by default."},
            "cache": {"enabled": True, "ttl_ms": 30000, "age_ms": 1200,
                      "reason": "Fresh cache entry."},
        },
        "reasons": [],
        "limitations": ["Read-only dashboard surface; no network writes are performed."],
    },
    "/api/status": {
        "hashrate_ghs": 96550, "hashrate_5m_ghs": 95800,
        "power_watts": 3200, "efficiency_jth": 33.2, "uptime_s": 184320,
        "firmware_version": "0.6.0-preview",
        "pool": {"url": "stratum+tcp://public-pool.io:21496", "user": "bc1q...preview",
                  "status": "connected"},
        "chains": [
            {"id": 0, "hashrate_ghs": 32200, "temp_c": 58.4, "chips_alive": 126, "nonces": 12842, "hw_errors": 0},
            {"id": 1, "hashrate_ghs": 32100, "temp_c": 59.1, "chips_alive": 126, "nonces": 12780, "hw_errors": 0},
            {"id": 2, "hashrate_ghs": 32250, "temp_c": 57.8, "chips_alive": 126, "nonces": 12903, "hw_errors": 0},
        ],
        "fans": {"pwm": 32, "rpm": 3120},
        "shares": {"accepted": 47, "rejected": 0, "last_diff": 974},
    },
    "/api/stats": {"hashrate_history": [], "power_history": [], "temp_history": []},
    "/api/system/info": {
        "model": "S19j Pro", "subtype": "BHB42801", "serial": "PREVIEW00001",
        "chip_count": 378, "hashboard_count": 3,
        "soc": "AM335x BeagleBone Black", "version": "0.6.0-preview", "build_date": "2026-05-14",
    },
    "/api/system/health": {"cpu_pct": 8, "mem_pct": 31, "uptime_s": 184320},
    "/api/pools": {"active": {"url": "stratum+tcp://public-pool.io:21496", "status": "connected"},
                    "backups": []},
    "/api/autotuner/status": {"running": False, "mode": "idle"},
    "/api/competitive/readiness": {"ready": True, "fields": {}},
    "/api/donations/config": {"enabled": True, "percent": 2},
    "/api/mining/posture": {"queue_depth": 4, "job_freshness_s": 8, "disconnected": False},
    "/api/mining/manifest": {"accept_rate": 1.0, "nonce_flow": "healthy"},
    "/api/history/shares": {"shares": []},
    "/api/network/info": {"ip": "203.0.113.50", "mac": "aa:bb:cc:dd:ee:ff", "hostname": "dcentos-preview"},
    "/api/stratum/v2/status": {"enabled": False, "encryption": "none"},
}


class Handler(BaseHTTPRequestHandler):
    def _send_json(self, body, code=200):
        data = json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def _send_index(self, wizard=False):
        try:
            html = INDEX.read_text(encoding="utf-8")
        except OSError as e:
            self.send_error(500, f"dist/index.html not built: {e}")
            return
        # Inject a pre-boot script that swallows any unmocked /api/* fetch with
        # an empty {} JSON and seeds setupComplete=true so the dashboard skips
        # the wizard during the preview. This runs BEFORE the bundled React
        # bundle parses, so even module-init fetches get caught.
        #
        # PREVIEW-ONLY dev toggle: `?wizard=1` makes the dashboard show the
        # First-Time Setup Wizard instead (does NOT seed setupComplete and
        # forces /api/setup/status -> needs_setup:true). This is a preview
        # QA aid in scripts/ — it is NOT shipped to firmware.
        seed_settings = (
            ""
            if wizard
            else "try { localStorage.setItem('dcentos-settings', JSON.stringify({setupComplete:true,mode:'standard',minerName:'Preview miner',electricityRate:0.10,btcPrice:62000,btcPriceAuto:true,temperatureUnit:'C'})); } catch (_) {}"
        )
        wiz_clear = (
            "try { localStorage.removeItem('dcentos-settings'); } catch (_) {}"
            if wizard
            else ""
        )
        wiz_status = "true" if wizard else "false"
        boot = """
<script>
(function () {
  var __WIZ__ = %WIZ%;
  %WIZ_CLEAR%
  %SEED%
  try { localStorage.setItem('dcentos-current-page','dashboard'); localStorage.setItem('dcentos-nav-standard','dashboard'); } catch (_) {}
  var _origFetch = window.fetch.bind(window);
  window.fetch = function (input, init) {
    try {
      var url = typeof input === 'string' ? input : (input && input.url) || '';
      // Wizard preview: force the first-run setup state regardless of mock.
      if (__WIZ__ && url.indexOf('/api/setup/status') !== -1) {
        return Promise.resolve(new Response(
          JSON.stringify({needs_setup:true, has_password:false, status:'needs_setup', auth:{password_set:false}}),
          {status: 200, headers: {'Content-Type':'application/json'}}));
      }
      // Only catch /api/* requests; everything else passes through.
      if (url.indexOf('/api/') !== -1) {
        return _origFetch(input, init).catch(function () {
          // network-level failure -> fall back to {}
          return new Response('{}', {status: 200, headers: {'Content-Type':'application/json'}});
        });
      }
    } catch (_) {}
    return _origFetch(input, init);
  };
  // Wrap WebSocket so it never throws on connect (we have no backend).
  var _OrigWS = window.WebSocket;
  window.WebSocket = function (url, protocols) {
    var fake = { url: url, readyState: 0, send: function(){}, close: function(){}, addEventListener: function(){}, removeEventListener: function(){} };
    // Defer the open event so listeners attach first; we never actually fire it
    // because the dashboard falls back to REST polling on WS failure.
    setTimeout(function(){ if (typeof fake.onerror === 'function') fake.onerror({}); }, 50);
    return fake;
  };
})();
</script>
"""
        boot = (
            boot.replace("%WIZ%", wiz_status)
                .replace("%WIZ_CLEAR%", wiz_clear)
                .replace("%SEED%", seed_settings)
        )
        # Inject as the first child of <head>
        if "<head>" in html:
            html = html.replace("<head>", "<head>" + boot, 1)
        else:
            html = boot + html
        data = html.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/api/"):
            for key, body in MOCKS.items():
                if path == key or path.startswith(key + "/"):
                    return self._send_json(body)
            # Unknown /api/* — return 404 so client code treats endpoint as missing
            # and uses its synthesized/endpointMissing fallback. Returning 200 + {}
            # causes components like BootPhaseBanner to crash on undefined fields.
            return self._send_json({"error": "endpoint not mocked"}, code=404)
        return self._send_index(wizard=("wizard=1" in self.path))

    def do_POST(self):
        # Echo a generic ack for action endpoints
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        return self._send_json({"ok": True, "scheduled": True, "reason": "preview mock"})

    def log_message(self, fmt, *args):
        # Quieter logs
        sys.stderr.write("[preview] %s - %s\n" % (self.address_string(), fmt % args))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=4173)
    args = ap.parse_args()
    if not INDEX.exists():
        sys.stderr.write("dist/index.html missing - run `npm run build` first\n")
        sys.exit(1)
    srv = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    sys.stderr.write(f"[preview] serving DCENT_OS dashboard preview on http://127.0.0.1:{args.port}/\n")
    sys.stderr.write(f"[preview] mocking {len(MOCKS)} /api/* endpoints\n")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
