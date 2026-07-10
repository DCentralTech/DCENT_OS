#!/bin/sh
#
# smoke_am2.sh — am2-s19jpro pre-flight sanity check
#
# Run BEFORE flashing or starting dcentrald on an am2 target. Confirms the
# platform is what we expect, that legacy bosminer isn't still holding the
# hardware, that the FPGA chains respond to a raw devmem read, and that
# /dev/i2c-0 exists for PIC access.
#
# Usage:  scripts/smoke_am2.sh <miner-ip>
#
# Intended for Linux/macOS/Git-Bash/WSL — uses native `ssh`. On Windows,
# run from WSL or Git Bash; dcentrald's Node.js ssh helper is not required
# here because the operator is driving the probe interactively.
#
# Exit codes:
#   0  All probes returned recognizable output.
#   1  Usage error or at least one REQUIRED gate failed.
#
# This script is READ-ONLY. It does not modify the miner. Safe to run on
# a live S19j Pro at any time.
#
# Phase 5B extensions (Agent E):
#   - FPGA CTRL dual-offset check (+0x00 vs +0x08 disambiguates am2/am1 layout)
#   - Braiins glitch mirror verification (am2 Braiins bitstream only — 0x43D00030 / 0x43D00034)
#   - PSU heartbeat delta sampled via Prometheus /metrics
#   - dcentrald gate: /etc/bos_platform, /dev/i2c-0, xiic-i2c kernel binding
#   - Final PASS/FAIL summary table
#
# D-Central Technologies, 2026.

set -u

MINER="${1:-}"
if [ -z "$MINER" ]; then
    echo "usage: $(basename "$0") <miner-ip>" >&2
    echo "  example: $(basename "$0") 203.0.113.139" >&2
    exit 1
fi

# Non-interactive, tolerant of fresh host keys — operators run this against
# many units. -o ConnectTimeout avoids hanging the dashboard for 60s if the
# miner is offline.
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR"
SSH="ssh $SSH_OPTS root@$MINER"

echo "=== am2 smoke test: $MINER ==="
echo ""

# -----------------------------------------------------------------------------
# Result tracking. Each check sets a row in RESULTS (name|status|detail).
# We print the full table at the end and exit non-zero if any REQUIRED row
# failed. Optional/informational rows never trigger failure.
# -----------------------------------------------------------------------------
RESULTS=""
FAIL_COUNT=0

record() {
    # $1=name  $2=status (PASS|FAIL|INFO|WARN)  $3=detail
    RESULTS="$RESULTS
$1|$2|$3"
    if [ "$2" = "FAIL" ]; then
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

# ---- identity ----
# /etc/bos_platform is BraiinsOS's source-of-truth (Agent α/δ read this too).
# /etc/dcentos/platform is DCENT_OS's copy (written by our post-build.sh).
# Expected on am2-s19jpro: bos_platform="zynq-bm3-am2" (dcentrald auto-route
# trigger), dcentos platform="zynq-bm3-am2".
BOS_PLATFORM=$($SSH 'cat /etc/bos_platform 2>/dev/null' 2>/dev/null || echo 'missing')
DCENTOS_PLATFORM=$($SSH 'cat /etc/dcentos/platform 2>/dev/null' 2>/dev/null || echo 'missing')
echo "-- /etc/bos_platform:      $BOS_PLATFORM"
echo "-- /etc/dcentos/platform:  $DCENTOS_PLATFORM"
if [ "$BOS_PLATFORM" = "zynq-bm3-am2" ]; then
    record "bos_platform=zynq-bm3-am2" "PASS" "$BOS_PLATFORM"
else
    record "bos_platform=zynq-bm3-am2" "FAIL" "got '$BOS_PLATFORM' (auto-route will not trigger)"
fi

# ---- running miner ----
# bosminer must be stopped before dcentrald can take the FPGA. If this
# reports a PID, sysupgrade hasn't happened yet and you're on stock BraiinsOS.
BOSMINER_PID=$($SSH 'pidof bosminer 2>/dev/null' 2>/dev/null || echo '')
DCENTRALD_PID=$($SSH 'pidof dcentrald 2>/dev/null' 2>/dev/null || echo '')
echo "-- bosminer PID:           ${BOSMINER_PID:-not running}"
echo "-- dcentrald PID:          ${DCENTRALD_PID:-not running}"
if [ -n "$BOSMINER_PID" ] && [ -n "$DCENTRALD_PID" ]; then
    record "single-miner-running" "FAIL" "both bosminer ($BOSMINER_PID) AND dcentrald ($DCENTRALD_PID) alive"
elif [ -n "$DCENTRALD_PID" ]; then
    record "single-miner-running" "PASS" "dcentrald PID=$DCENTRALD_PID (bosminer stopped)"
elif [ -n "$BOSMINER_PID" ]; then
    record "single-miner-running" "INFO" "bosminer PID=$BOSMINER_PID (pre-sysupgrade state)"
else
    record "single-miner-running" "INFO" "no mining daemon running"
fi

# ---- FPGA chain CTRL dual-offset ----
# am2 (s9io-am2) lays CTRL at +0x00 and leaves +0x08 as zero during mining.
# am1 (legacy s9io) uses +0x08 for CTRL and leaves +0x00 as garbage.
# Reading BOTH disambiguates platform at runtime.
#
# Expected on am2 while bosminer or dcentrald is mining:
#   chain1 +0x00 = 0x00901002 (CTRL active)
#   chain1 +0x08 = 0x00000000
# Expected on am1:
#   chain1 +0x00 = 0 or unpredictable
#   chain1 +0x08 = CTRL value (e.g. 0x0000000C)
CTRL_00=$($SSH 'devmem 0x43C00000 32 2>/dev/null' 2>/dev/null || echo 'read failed')
CTRL_08=$($SSH 'devmem 0x43C00008 32 2>/dev/null' 2>/dev/null || echo 'read failed')
echo "-- CTRL chain1 @+0x00:     $CTRL_00 (expect 0x00901002 on am2 live-mining)"
echo "-- CTRL chain1 @+0x08:     $CTRL_08 (expect 0x00000000 on am2; =CTRL on am1)"

case "$CTRL_00" in
    0x00901002|0x00901000)
        record "fpga-ctrl-layout-am2" "PASS" "+0x00=$CTRL_00 (am2 layout, mining-active CTRL)"
        ;;
    0x00000000|0xffffffff|'read failed')
        if [ "$CTRL_08" != "0x00000000" ] && [ "$CTRL_08" != "read failed" ]; then
            record "fpga-ctrl-layout-am2" "FAIL" "+0x00=$CTRL_00, +0x08=$CTRL_08 (looks like am1 layout)"
        else
            record "fpga-ctrl-layout-am2" "INFO" "+0x00=$CTRL_00 +0x08=$CTRL_08 (chain inactive or not yet brought up)"
        fi
        ;;
    *)
        record "fpga-ctrl-layout-am2" "INFO" "+0x00=$CTRL_00 +0x08=$CTRL_08 (unrecognized pattern)"
        ;;
esac

# Also keep the legacy chain4 read — s9io-am2 bitstream programs all 4 chain
# windows even when only 3 boards populated; 0xFFFFFFFF = chain absent.
CH4=$($SSH 'devmem 0x43C30000 32 2>/dev/null' 2>/dev/null || echo 'read failed')
echo "-- CTRL chain4 @+0x00:     $CH4"

# ---- Braiins glitch mirror verification (am2 Braiins-bitstream only) ----
# W13.B1 (2026-05-10) RECLASSIFIED: 0x43D00000 is the Braiins-am2-only
# diagnostic glitch monitor. Registers +0x30 and +0x34 are read-only
# mirrors of BM1362 ASIC reg 0x2C — NOT a control surface. Stock
# CV1835/AM335x/AML/S9 hardware does NOT populate this IP.
GLITCH_MIRROR_30=$($SSH 'devmem 0x43D00030 32 2>/dev/null' 2>/dev/null || echo 'read failed')
GLITCH_MIRROR_34=$($SSH 'devmem 0x43D00034 32 2>/dev/null' 2>/dev/null || echo 'read failed')
echo "-- Braiins glitch mirror +0x30: $GLITCH_MIRROR_30 (mirror of BM1362 0x2C ro_relay_en when mining)"
echo "-- Braiins glitch mirror +0x34: $GLITCH_MIRROR_34 (mirror of BM1362 0x2C ro_relay_en when mining)"
if [ "$GLITCH_MIRROR_30" = "0x00000002" ] && [ "$GLITCH_MIRROR_34" = "0x00000002" ]; then
    record "braiins-glitch-mirror-mining" "PASS" "+0x30=+0x34=0x00000002"
elif [ "$GLITCH_MIRROR_30" = "read failed" ] || [ "$GLITCH_MIRROR_34" = "read failed" ]; then
    record "braiins-glitch-mirror-mining" "FAIL" "devmem read failed — Braiins-am2 bitstream missing?"
else
    record "braiins-glitch-mirror-mining" "INFO" "+0x30=$GLITCH_MIRROR_30 +0x34=$GLITCH_MIRROR_34 (not mining yet — OK pre-handoff)"
fi

# ---- PSU heartbeat delta via Prometheus ----
# dcentrald serves /metrics on 127.0.0.1:8081. shared_psu_heartbeats is a
# monotonic counter incremented ~1/s by the PSU keepalive task. Sampling
# T=0 and T=10 gives us a delta that MUST be ~10 while dcentrald is live.
# If dcentrald is not running, we skip (INFO) rather than fail.
if [ -n "$DCENTRALD_PID" ]; then
    HB1=$($SSH 'curl -sf http://127.0.0.1:8081/metrics 2>/dev/null | grep "^shared_psu_heartbeats " | awk "{print \$NF}"' 2>/dev/null)
    HB1=${HB1:-0}
    echo "-- PSU heartbeats T=0:     $HB1 (sampling for 10s...)"
    sleep 10
    HB2=$($SSH 'curl -sf http://127.0.0.1:8081/metrics 2>/dev/null | grep "^shared_psu_heartbeats " | awk "{print \$NF}"' 2>/dev/null)
    HB2=${HB2:-0}
    DELTA=$((HB2 - HB1))
    echo "-- PSU heartbeats T=10:    $HB2 (delta=$DELTA, expect ~10)"
    if [ "$DELTA" -ge 8 ] && [ "$DELTA" -le 12 ]; then
        record "psu-heartbeat-rate" "PASS" "delta=$DELTA over 10s"
    elif [ "$DELTA" -gt 0 ]; then
        record "psu-heartbeat-rate" "WARN" "delta=$DELTA over 10s (expected ~10)"
    else
        record "psu-heartbeat-rate" "FAIL" "delta=$DELTA — PSU task not heartbeating"
    fi
else
    echo "-- PSU heartbeats:         (dcentrald not running — skipped)"
    record "psu-heartbeat-rate" "INFO" "dcentrald offline, /metrics unreachable"
fi

# ---- I2C device ----
# Must exist before dcentrald starts on am2 (required for PSU + PIC access).
# Our init script's ensure_i2c0_kernel_bound() creates this if the kernel
# driver didn't auto-create it.
I2C0_RAW=$($SSH 'ls -la /dev/i2c-0 2>&1' 2>/dev/null || echo 'ssh failed')
echo "-- /dev/i2c-0:             $I2C0_RAW"
if echo "$I2C0_RAW" | grep -q '^c'; then
    record "/dev/i2c-0-exists" "PASS" "character device present"
else
    record "/dev/i2c-0-exists" "FAIL" "$I2C0_RAW"
fi

# ---- kernel driver binding ----
# xiic-i2c must be bound to 41600000.i2c for /dev/i2c-0 to exist naturally.
# Phase 5 auto-route depends on this being healthy BEFORE dcentrald starts.
XIIC=$($SSH 'ls /sys/bus/platform/drivers/xiic-i2c/ 2>/dev/null | grep -c "\.i2c$"' 2>/dev/null || echo '0')
echo "-- xiic-i2c bound devices: $XIIC (expect >=1)"
if [ "$XIIC" -ge 1 ] 2>/dev/null; then
    record "xiic-i2c-driver-bound" "PASS" "$XIIC device(s) bound"
else
    record "xiic-i2c-driver-bound" "FAIL" "no xiic-i2c bindings — kernel driver not loaded"
fi

# Also confirm the i2c-0 sysfs name file exists (defensive — distinguishes
# "device node present but kernel not talking" from "fully wired up").
I2C0_NAME=$($SSH 'cat /sys/bus/i2c/devices/i2c-0/name 2>/dev/null' 2>/dev/null || echo 'missing')
echo "-- i2c-0 sysfs name:       $I2C0_NAME"
if [ "$I2C0_NAME" != "missing" ] && [ -n "$I2C0_NAME" ]; then
    record "i2c-0-sysfs-wired" "PASS" "$I2C0_NAME"
else
    record "i2c-0-sysfs-wired" "FAIL" "sysfs name file missing"
fi

# ---- board identity files ----
BOARD_TARGET=$($SSH 'cat /etc/dcentos/board_target 2>/dev/null' 2>/dev/null || echo 'missing')
BOARD_FAMILY=$($SSH 'cat /etc/dcentos/board_family 2>/dev/null' 2>/dev/null || echo 'missing')
echo "-- board_target:           $BOARD_TARGET  (expect am2-s19j)"
echo "-- board_family:           $BOARD_FAMILY  (expect am2)"

# ---- dcentrald build stamp ----
# Confirms the installed binary matches what post-build.sh stamped.
# Useful for catching "I thought I flashed the new build" mistakes.
STAMPED=$($SSH 'cat /etc/dcentos/dcentrald.md5 2>/dev/null' 2>/dev/null || echo 'missing')
LIVE=$($SSH 'md5sum /usr/local/bin/dcentrald 2>/dev/null | cut -d" " -f1' 2>/dev/null || echo 'missing')
echo "-- dcentrald.md5 stamped:  $STAMPED"
echo "-- dcentrald.md5 on disk:  $LIVE"
if [ "$STAMPED" != "missing" ] && [ "$STAMPED" != "$LIVE" ]; then
    echo "   [WARN] stamped md5 does not match binary on disk — upgrade not clean?"
    record "dcentrald-binary-stamped" "WARN" "stamp=$STAMPED disk=$LIVE mismatch"
elif [ "$STAMPED" = "missing" ]; then
    record "dcentrald-binary-stamped" "INFO" "no post-build.sh stamp (not DCENT_OS build or pre-v0.20)"
else
    record "dcentrald-binary-stamped" "PASS" "md5=$STAMPED"
fi

# -----------------------------------------------------------------------------
# Summary table
# -----------------------------------------------------------------------------
echo ""
echo "=============================================================="
echo "  am2 smoke test summary for $MINER"
echo "=============================================================="
printf "  %-32s %-6s %s\n" "CHECK" "STATUS" "DETAIL"
printf "  %-32s %-6s %s\n" "--------------------------------" "------" "------"
echo "$RESULTS" | while IFS='|' read -r name status detail; do
    [ -z "$name" ] && continue
    printf "  %-32s %-6s %s\n" "$name" "$status" "$detail"
done
echo "=============================================================="

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo "  FAIL: $FAIL_COUNT required check(s) failed"
    echo "=============================================================="
    exit 1
fi

echo "  OK: all required gates passed"
echo "=============================================================="
exit 0
