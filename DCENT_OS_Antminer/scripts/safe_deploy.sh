#!/bin/bash
# DCENTos — Safe Deploy Script (S9 / AM1 PIC-Aware Binary Upgrade)
# D-Central Technologies, 2026
#
# Deploys a new dcentrald binary WITHOUT triggering the PIC watchdog.
#
# IMPORTANT: This script is validated only for S9-style AM1 Zynq boards.
# It must not be used on AM2 or Amlogic boards.
#
# PROBLEM: The PIC16F1704 voltage controllers have a ~10s heartbeat watchdog.
# When dcentrald is stopped for a binary upgrade, no heartbeats are sent,
# the watchdog fires, the DC-DC converter cuts voltage, and the I2C bus
# enters a corrupted state requiring a PSU power cycle to recover.
#
# SOLUTION: This script stops dcentrald, then immediately starts a lightweight
# heartbeat bridge (hb_bridge.sh) on the miner. The bridge sends I2C heartbeats
# via devmem, keeping all 3 PICs alive during the upgrade window. When the new
# dcentrald is started, the bridge is killed just before dcentrald initializes
# its own I2C subsystem (avoiding bus contention).
#
# Sequence:
#   1. Upload hb_bridge.sh to miner (if not already present)
#   2. Send SIGTERM to dcentrald (graceful shutdown)
#   3. Wait up to 5s for dcentrald to exit (leaves 5s margin before PIC watchdog)
#   4. Start hb_bridge.sh in background (takes over PIC heartbeats)
#   5. Wait 3s for bridge to establish heartbeat cadence
#   6. Upload new binary to /usr/local/bin/dcentrald
#   7. Start new dcentrald
#   8. Wait 5s for dcentrald to send its first heartbeat
#   9. Kill hb_bridge.sh (dcentrald now owns the I2C bus)
#   10. Verify dcentrald is running
#
# Usage:
#   ./safe_deploy.sh <miner_ip> [options]
#   ./safe_deploy.sh 203.0.113.36
#   ./safe_deploy.sh 203.0.113.36 --skip-build
#   ./safe_deploy.sh 203.0.113.36 --config dcentrald.toml --verify
#   ./safe_deploy.sh 203.0.113.36 --rollback-on-fail --json
#
# Options:
#   --skip-build         Don't cross-compile, use existing binary
#   --config <file>      Upload a local config file to /data/dcentrald.toml
#   --verify             Poll /api/status for 15s after start, confirm HTTP 200
#   --tail               After deploy, exec ssh tail -f /tmp/dcentrald.log
#   --rollback-on-fail   If health check fails, restore backup and restart
#   --json               Output JSON result (for Claude Code parsing)
#
# Wave B (2026-05-19): the --passthrough CLI flag was removed. The
# [mining].passthrough = true knob in /data/dcentrald.toml is the canonical
# way to request passthrough mode; the S82 init script reads it. See
#  G-T8-1.
#
# SAFETY NOTES:
#   - hb_bridge.sh and dcentrald MUST NOT run I2C simultaneously.
#     The bridge's SOFTR reset would corrupt dcentrald's in-flight I2C.
#     Ordering: stop dcentrald -> start bridge -> upload binary -> start dcentrald -> kill bridge.
#   - The bridge is killed AFTER dcentrald starts but BEFORE dcentrald's first I2C access
#     (dcentrald takes ~3-5s from start to first I2C access via devmem).
#   - If the deploy fails mid-flight, the bridge keeps running (PICs stay alive).
#     Manual cleanup: ssh root@<ip> 'kill $(cat /tmp/hb_bridge.pid)'
#   - The bridge uses the same devmem AXI IIC path as dcentrald, with the same
#     clock timing (1498 divider = ~33 kHz). No kernel driver conflict.

set -euo pipefail

# ── Argument Parsing ────────────────────────────────────────────────────────

MINER_IP="${1:?Usage: $0 <miner_ip> [--skip-build] [--config FILE] [--verify] [--tail] [--rollback-on-fail] [--json]}"
shift

SKIP_BUILD=false
CONFIG_FILE=""
VERIFY=false
TAIL=false
ROLLBACK_ON_FAIL=false
JSON_OUTPUT=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build)       SKIP_BUILD=true ;;
        --config)           CONFIG_FILE="${2:?--config requires a file argument}"; shift ;;
        --verify)           VERIFY=true ;;
        --tail)             TAIL=true ;;
        --rollback-on-fail) ROLLBACK_ON_FAIL=true ;;
        --json)             JSON_OUTPUT=true ;;
        *)                  echo "Unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

# ── Paths & Constants ───────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
WORKSPACE_DIR="$PROJECT_DIR/dcentrald"
TARGET="armv7-unknown-linux-musleabihf"
BINARY="$WORKSPACE_DIR/target/$TARGET/release/dcentrald"
DEPLOY_PATH="/usr/local/bin/dcentrald"
BACKUP_PATH="/tmp/dcentrald_backup"
STAGING_PATH="/tmp/dcentrald_new"
CONFIG_REMOTE="/data/dcentrald.toml"
LOG_PATH="/tmp/dcentrald.log"
API_PORT=8080

# Heartbeat bridge paths (on the miner)
HB_BRIDGE_LOCAL="$SCRIPT_DIR/hb_bridge.sh"
HB_BRIDGE_REMOTE="/data/hb_bridge.sh"
HB_BRIDGE_PID="/tmp/hb_bridge.pid"
HB_BRIDGE_LOG="/tmp/hb_bridge.log"

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
SSH_CMD="ssh $SSH_OPTS root@$MINER_IP"
SCP_CMD="scp -O $SSH_OPTS"

DEPLOY_START=$(date +%s)

# ── Helpers ─────────────────────────────────────────────────────────────────

log() {
    if [ "$JSON_OUTPUT" = false ]; then
        echo "$@"
    fi
}

log_step() {
    log ""
    log "=== $1 ==="
}

json_exit() {
    local success="$1"
    local pid="${2:-null}"
    local binary_size="${3:-0}"
    local api_healthy="${4:-false}"
    local message="${5:-}"
    local deploy_end
    deploy_end=$(date +%s)
    local deploy_time=$((deploy_end - DEPLOY_START))

    if [ "$JSON_OUTPUT" = true ]; then
        cat <<ENDJSON
{
  "success": $success,
  "pid": $pid,
  "binary_size": $binary_size,
  "deploy_time_seconds": $deploy_time,
  "api_healthy": $api_healthy,
  "miner_ip": "$MINER_IP",
  "message": "$message"
}
ENDJSON
    fi

    if [ "$success" = "true" ]; then
        exit 0
    else
        exit 1
    fi
}

# Kill the heartbeat bridge on the miner (best-effort, no error on failure)
kill_hb_bridge() {
    $SSH_CMD '
        if [ -f /tmp/hb_bridge.pid ]; then
            kill $(cat /tmp/hb_bridge.pid) 2>/dev/null
            rm -f /tmp/hb_bridge.pid
        fi
        # Also kill by name in case PID file is stale
        killall hb_bridge.sh 2>/dev/null
        true
    ' 2>/dev/null || true
}

# ── Step 1: Cross-Compile ──────────────────────────────────────────────────

if [ "$SKIP_BUILD" = false ]; then
    log_step "Building dcentrald (release, $TARGET)"
    cd "$WORKSPACE_DIR"
    if ! cargo build --release --target "$TARGET" 2>&1 | while IFS= read -r line; do log "  $line"; done; then
        log "ERROR: Build failed"
        json_exit false null 0 false "Build failed"
    fi
    log "  Build complete."
else
    log_step "Skipping build (--skip-build)"
fi

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    log "ERROR: Binary not found at $BINARY"
    log "Run without --skip-build to compile first."
    json_exit false null 0 false "Binary not found at $BINARY"
fi

BINARY_SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY" 2>/dev/null || echo 0)
BINARY_MB=$(( BINARY_SIZE / 1024 / 1024 ))
log "  Binary: $BINARY_SIZE bytes (${BINARY_MB} MB)"

# ── Step 2: Connectivity Check ─────────────────────────────────────────────

log_step "Connecting to $MINER_IP"
if ! $SSH_CMD "echo OK" >/dev/null 2>&1; then
    log "ERROR: Cannot SSH to root@$MINER_IP"
    json_exit false null "$BINARY_SIZE" false "SSH connection failed"
fi
log "  SSH OK"

log_step "Validating S9 / AM1 target"
PLATFORM_INFO=$($SSH_CMD '
    echo "ARCH=$(uname -m 2>/dev/null || echo unknown)"
    if [ -f /sys/devices/soc0/soc_id ]; then
        echo "SOC=$(cat /sys/devices/soc0/soc_id 2>/dev/null)"
    elif grep -q zynq /proc/cpuinfo 2>/dev/null; then
        echo "SOC=zynq"
    else
        echo "SOC=unknown"
    fi
    MODEL="$(cat /config/CONF_MINER_TYPE 2>/dev/null || cat /proc/device-tree/model 2>/dev/null || echo)"
    echo "MODEL=$MODEL"
    HWID="$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo)"
    echo "HWID=$HWID"
    echo "UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name "uio*" 2>/dev/null | wc -l)"
' 2>/dev/null) || true
eval "$PLATFORM_INFO" 2>/dev/null || true
ARCH_LC=$(printf '%s' "${ARCH:-unknown}" | tr '[:upper:]' '[:lower:]')
SOC_LC=$(printf '%s' "${SOC:-unknown}" | tr '[:upper:]' '[:lower:]')
MODEL_LC=$(printf '%s' "${MODEL:-}" | tr '[:upper:]' '[:lower:]')
HWID_LC=$(printf '%s' "${HWID:-}" | tr '[:upper:]' '[:lower:]')

if [[ "$ARCH_LC" == *"aarch64"* ]] || [[ "$SOC_LC" == *"amlogic"* ]] || [[ "$MODEL_LC" == *"amlogic"* ]] || [[ "$MODEL_LC" == *"s17"* ]] || [[ "$MODEL_LC" == *"s19"* ]] || [[ "$MODEL_LC" == *"s21"* ]] || [[ "$HWID_LC" == *"am2"* ]] || [ "${UIO_COUNT:-0}" -ge 19 ]; then
    log "ERROR: safe_deploy.sh is S9/AM1-only. Detected arch=${ARCH:-unknown}, soc=${SOC:-unknown}, model=${MODEL:-unknown}, hwid=${HWID:-unknown}, uio_count=${UIO_COUNT:-0}."
    json_exit false null "$BINARY_SIZE" false "safe_deploy.sh is only supported on validated S9/AM1 targets"
fi
log "  Target validated as S9/AM1-style Zynq board"

# ── Step 3: Detect Running State ───────────────────────────────────────────

log_step "Detecting miner state"
MINER_INFO=$($SSH_CMD '
    echo "BOSMINER_PID=$(pidof bosminer 2>/dev/null || echo NONE)"
    echo "DCENTRALD_PID=$(pidof dcentrald 2>/dev/null || echo NONE)"
    echo "HB_BRIDGE_PID=$(cat /tmp/hb_bridge.pid 2>/dev/null || echo NONE)"
    echo "OS_VER=$(cat /etc/dcentos-version 2>/dev/null || echo NONE)"
    echo "BOS_VER=$(cat /etc/bos_version 2>/dev/null | head -1 || echo NONE)"
    echo "HAS_DEVMEM=$(command -v devmem >/dev/null 2>&1 && echo YES || echo NO)"
' 2>/dev/null) || true

BOSMINER_PID="NONE"
DCENTRALD_PID="NONE"
HB_BRIDGE_PID="NONE"
OS_VER="NONE"
BOS_VER="NONE"
HAS_DEVMEM="NO"
eval "$MINER_INFO" 2>/dev/null || true

log "  BraiinsOS: $BOS_VER"
log "  DCENTos:   $OS_VER"
log "  bosminer:  PID=$BOSMINER_PID"
log "  dcentrald: PID=$DCENTRALD_PID"
log "  hb_bridge: PID=$HB_BRIDGE_PID"
log "  devmem:    $HAS_DEVMEM"

if [ "$MINER_IP" = "203.0.113.109" ] || [[ "${CONFIG_FILE:-}" == *"dcentrald_s19jpro_xil.toml"* ]]; then
    log "ERROR: XIL is a home-quiet target. safe_deploy.sh still contains raw bosminer stop/kill paths and is blocked for this unit."
    log "See the home-quiet handoff procedure in the project documentation."
    json_exit false null "$BINARY_SIZE" false "XIL requires guarded quiet handoff; generic safe_deploy blocked"
fi

if [ "$HAS_DEVMEM" = "NO" ]; then
    log "ERROR: devmem not available on miner -- heartbeat bridge cannot function"
    json_exit false null "$BINARY_SIZE" false "devmem not available on miner"
fi

# ── Step 4: Upload Config (if requested) ───────────────────────────────────

if [ -n "$CONFIG_FILE" ]; then
    log_step "Uploading config: $CONFIG_FILE"
    if [ ! -f "$CONFIG_FILE" ]; then
        log "ERROR: Config file not found: $CONFIG_FILE"
        json_exit false null "$BINARY_SIZE" false "Config file not found: $CONFIG_FILE"
    fi
    $SCP_CMD "$CONFIG_FILE" "root@$MINER_IP:$CONFIG_REMOTE"
    log "  Deployed to $CONFIG_REMOTE"
fi

# ── Step 5: Upload Heartbeat Bridge ────────────────────────────────────────

log_step "Uploading heartbeat bridge"

if [ ! -f "$HB_BRIDGE_LOCAL" ]; then
    log "ERROR: hb_bridge.sh not found at $HB_BRIDGE_LOCAL"
    json_exit false null "$BINARY_SIZE" false "hb_bridge.sh not found at $HB_BRIDGE_LOCAL"
fi

$SCP_CMD "$HB_BRIDGE_LOCAL" "root@$MINER_IP:$HB_BRIDGE_REMOTE"
$SSH_CMD "chmod +x $HB_BRIDGE_REMOTE" 2>/dev/null
log "  Deployed hb_bridge.sh to $HB_BRIDGE_REMOTE"

# ── Step 6: Stop dcentrald + Start Bridge (atomic handoff) ─────────────────

log_step "Stopping dcentrald and starting heartbeat bridge"

# CRITICAL ORDERING:
# The bridge and dcentrald MUST NOT access the AXI IIC controller simultaneously.
# The bridge does a SOFTR reset on init which would corrupt dcentrald's in-flight
# I2C transactions (no cross-process lock on the hardware registers).
#
# Correct sequence (stop-then-bridge):
#   T+0.0s: SIGTERM to dcentrald
#   T+0-3s: dcentrald exits (graceful shutdown: lowers voltage, disables boards)
#   T+3.0s: Start bridge (first heartbeat within ~1s of starting)
#   T+4.0s: Bridge sends first heartbeat round
#
# The gap between dcentrald's LAST heartbeat (~T-1s before SIGTERM) and the
# bridge's FIRST heartbeat (~T+4s) is ~5s. The PIC watchdog is 10s on
# BraiinsOS PICs and ~60s on stock PICs. We have comfortable margin.
#
# If dcentrald takes longer than 5s to exit, we still have 5s of margin
# before the 10s watchdog fires. The SIGKILL fallback at 10s ensures we
# don't wait forever.

# Kill any existing bridge first
kill_hb_bridge

if [ "$DCENTRALD_PID" != "NONE" ]; then
    log "  Sending SIGTERM to dcentrald (PID $DCENTRALD_PID)..."
    $SSH_CMD "kill -TERM $DCENTRALD_PID 2>/dev/null" 2>/dev/null || true

    # Wait up to 5s for graceful shutdown (leaves 5s margin before PIC watchdog)
    WAIT_COUNT=0
    while [ $WAIT_COUNT -lt 5 ]; do
        STILL_RUNNING=$($SSH_CMD "pidof dcentrald 2>/dev/null || echo DEAD" 2>/dev/null) || STILL_RUNNING="DEAD"
        if [ "$STILL_RUNNING" = "DEAD" ]; then
            log "  dcentrald exited cleanly after ${WAIT_COUNT}s"
            break
        fi
        WAIT_COUNT=$((WAIT_COUNT + 1))
        sleep 1
    done

    # Force kill if still running after 5s (can't wait longer -- PIC watchdog)
    if [ $WAIT_COUNT -ge 5 ]; then
        log "  dcentrald did not exit in 5s, sending SIGKILL (PIC watchdog pressure)..."
        $SSH_CMD "kill -9 $DCENTRALD_PID 2>/dev/null" 2>/dev/null || true
        sleep 1
    fi
elif [ "$BOSMINER_PID" != "NONE" ]; then
    # If upgrading from bosminer, stop it first
    log "  Stopping bosminer (PID $BOSMINER_PID)..."
    # CRITICAL: SIGTERM for clean xiic handoff. Wait up to 5s (not 10s -- PIC watchdog).
    $SSH_CMD "kill -TERM $BOSMINER_PID 2>/dev/null" 2>/dev/null || true
    WAIT_COUNT=0
    while [ $WAIT_COUNT -lt 5 ]; do
        BOS_RUNNING=$($SSH_CMD "pidof bosminer 2>/dev/null || echo DEAD" 2>/dev/null) || BOS_RUNNING="DEAD"
        if [ "$BOS_RUNNING" = "DEAD" ]; then
            log "  bosminer exited cleanly after ${WAIT_COUNT}s"
            break
        fi
        WAIT_COUNT=$((WAIT_COUNT + 1))
        sleep 1
    done
    if [ $WAIT_COUNT -ge 5 ]; then
        $SSH_CMD "kill -9 $BOSMINER_PID 2>/dev/null" 2>/dev/null || true
        sleep 1
    fi
else
    log "  No mining daemon running."
fi

# ── Step 7: Start Heartbeat Bridge (immediately after daemon exit) ──────────

log_step "Starting heartbeat bridge (keeping PICs alive)"

# Start the bridge NOW -- the daemon is dead, AXI IIC is free.
# The bridge will re-initialize the AXI IIC controller and begin
# heartbeating all 3 PICs every 2 seconds.
$SSH_CMD "nohup $HB_BRIDGE_REMOTE >/dev/null 2>&1 &" 2>/dev/null

# Wait for bridge to start and send first heartbeat round (~2s)
sleep 3

# Verify bridge is running
BRIDGE_RUNNING=$($SSH_CMD 'cat /tmp/hb_bridge.pid 2>/dev/null && echo OK || echo FAIL' 2>/dev/null)
if echo "$BRIDGE_RUNNING" | grep -q "FAIL"; then
    log "ERROR: Heartbeat bridge failed to start!"
    log "  WARNING: PICs may trip watchdog -- power cycle may be needed."
    log "  Bridge log:"
    $SSH_CMD "cat $HB_BRIDGE_LOG 2>/dev/null" 2>/dev/null || true
    json_exit false null "$BINARY_SIZE" false "Heartbeat bridge failed to start"
fi
log "  Bridge running (PID from miner: $(echo "$BRIDGE_RUNNING" | head -1))"
log "  PICs are being kept alive independently of dcentrald"

# ── Step 8: Backup + Upload New Binary ──────────────────────────────────────

log_step "Deploying binary"

# Backup existing binary
$SSH_CMD "[ -f $DEPLOY_PATH ] && cp $DEPLOY_PATH $BACKUP_PATH || true" 2>/dev/null
log "  Backed up $DEPLOY_PATH -> $BACKUP_PATH"

# Upload new binary
log "  Uploading binary ($BINARY_SIZE bytes)..."
$SCP_CMD "$BINARY" "root@$MINER_IP:$STAGING_PATH"
$SSH_CMD "chmod +x $STAGING_PATH && mv $STAGING_PATH $DEPLOY_PATH" 2>/dev/null
log "  Deployed to $DEPLOY_PATH"

# ── Step 9: Start New dcentrald ─────────────────────────────────────────────

log_step "Starting dcentrald (heartbeat bridge still active)"

# Build the start command with config detection
START_SCRIPT='
CONFIG=""
if [ -f /data/dcentrald.toml ]; then
    CONFIG="--config /data/dcentrald.toml"
    echo "CONFIG_USED=/data/dcentrald.toml"
elif [ -f /etc/dcentrald.toml ]; then
    CONFIG="--config /etc/dcentrald.toml"
    echo "CONFIG_USED=/etc/dcentrald.toml"
else
    echo "CONFIG_USED=builtin"
fi
'

START_SCRIPT+='
nohup /usr/local/bin/dcentrald $CONFIG >'"$LOG_PATH"' 2>&1 &
echo "NEW_PID=$!"
'

START_OUTPUT=$($SSH_CMD "$START_SCRIPT" 2>/dev/null) || true
eval "$START_OUTPUT" 2>/dev/null || true

CONFIG_USED="${CONFIG_USED:-unknown}"
NEW_PID="${NEW_PID:-null}"

log "  Config: $CONFIG_USED"
log "  PID: $NEW_PID"

# ── Step 10: Kill Bridge + Wait for dcentrald to Take Over Heartbeats ──────

log_step "Handoff: bridge -> dcentrald I2C ownership"

# CRITICAL TIMING for the bridge-to-dcentrald handoff:
#
# dcentrald startup sequence (from process start):
#   T+0.0s: Process starts, parses config, opens UIO devices
#   T+1.0s: Spawns I2C service thread, mmaps AXI IIC controller
#   T+1.5s: Initializes AXI IIC (SOFTR reset + clock timing)
#   T+2.0s: PIC init begins (JUMP, detect firmware, set voltage)
#   T+3.0s: First heartbeat sent
#
# The bridge and dcentrald CANNOT share the AXI IIC controller.
# Both do SOFTR resets and TX FIFO writes with no cross-process lock.
# Overlapping access = corrupted I2C transactions = PIC NACKs.
#
# Strategy: Kill the bridge at T+1s (before dcentrald touches I2C).
# The gap between the bridge's last heartbeat and dcentrald's first
# heartbeat is ~2s (bridge killed at T+1, dcentrald heartbeat at T+3).
# This is well within the 10s PIC watchdog.
#
# If dcentrald crashes during init, we detect it at T+5s and leave
# a warning (the bridge is already dead, but we have ~7s of PIC
# watchdog margin to restart it or take corrective action).

# Wait 1s for dcentrald to start (process alive but not yet touching I2C)
sleep 1

# Kill the bridge NOW -- dcentrald's I2C init is imminent
log "  Killing heartbeat bridge (dcentrald about to init I2C)..."
kill_hb_bridge
sleep 1

# Verify bridge is gone
BRIDGE_AFTER=$($SSH_CMD 'pidof hb_bridge.sh 2>/dev/null || echo DEAD' 2>/dev/null) || BRIDGE_AFTER="DEAD"
if [ "$BRIDGE_AFTER" != "DEAD" ]; then
    log "  WARNING: Bridge still running (PID $BRIDGE_AFTER), force killing..."
    $SSH_CMD "kill -9 $BRIDGE_AFTER 2>/dev/null" 2>/dev/null || true
fi
log "  Bridge stopped. AXI IIC controller is free for dcentrald."

# Wait 3 more seconds for dcentrald to complete I2C init and send first heartbeat
sleep 3

# Verify dcentrald is still running after init
RUNNING_PID=$($SSH_CMD "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || RUNNING_PID="NONE"
if [ "$RUNNING_PID" = "NONE" ]; then
    log "  WARNING: dcentrald exited during startup!"
    log "  WARNING: Bridge already killed -- PICs have ~7s before watchdog fires!"
    log "  Last 20 lines of log:"
    if [ "$JSON_OUTPUT" = false ]; then
        $SSH_CMD "tail -20 $LOG_PATH 2>/dev/null" 2>/dev/null || true
    fi

    # Rollback if requested -- restart bridge to keep PICs alive during rollback
    if [ "$ROLLBACK_ON_FAIL" = true ]; then
        log ""
        log "=== Rolling back (restarting bridge for safety) ==="
        # Restart bridge to keep PICs alive during rollback
        $SSH_CMD "nohup $HB_BRIDGE_REMOTE >/dev/null 2>&1 &" 2>/dev/null
        sleep 2
        $SSH_CMD "[ -f $BACKUP_PATH ] && mv $BACKUP_PATH $DEPLOY_PATH && chmod +x $DEPLOY_PATH" 2>/dev/null || true
        # Start old binary
        # Kill bridge first so it doesn't conflict with the old dcentrald's I2C
        kill_hb_bridge
        $SSH_CMD "nohup $DEPLOY_PATH --config $CONFIG_REMOTE >$LOG_PATH 2>&1 &" 2>/dev/null || true
        sleep 5
        ROLLBACK_PID=$($SSH_CMD "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || ROLLBACK_PID="NONE"
        log "  Rollback PID: $ROLLBACK_PID"
        json_exit false null "$BINARY_SIZE" false "dcentrald exited during startup, rolled back (PID=$ROLLBACK_PID)"
    fi

    # No rollback requested -- restart bridge to prevent PIC watchdog
    log "  Restarting heartbeat bridge to prevent PIC watchdog..."
    $SSH_CMD "nohup $HB_BRIDGE_REMOTE >/dev/null 2>&1 &" 2>/dev/null
    log "  NOTE: Bridge restarted. Kill manually after fixing dcentrald:"
    log "    ssh root@$MINER_IP 'kill \$(cat /tmp/hb_bridge.pid)'"
    json_exit false null "$BINARY_SIZE" false "dcentrald exited during startup (bridge restarted)"
fi

NEW_PID="$RUNNING_PID"

# ── Step 11: Health Check (if --verify) ─────────────────────────────────────

API_HEALTHY=false

if [ "$VERIFY" = true ]; then
    log_step "Verifying API health (polling for 15s)"
    VERIFY_DEADLINE=$(($(date +%s) + 15))

    while [ "$(date +%s)" -lt "$VERIFY_DEADLINE" ]; do
        HTTP_CODE=$($SSH_CMD "wget -q -O /dev/null -S http://127.0.0.1:$API_PORT/api/status 2>&1 | grep 'HTTP/' | tail -1 | awk '{print \$2}'" 2>/dev/null) || HTTP_CODE=""

        if [ "$HTTP_CODE" = "200" ]; then
            API_HEALTHY=true
            log "  API healthy (HTTP 200)"
            break
        fi

        log "  Waiting... (HTTP=$HTTP_CODE)"
        sleep 2
    done

    if [ "$API_HEALTHY" = false ]; then
        log "  WARNING: API did not respond with 200 within 15s"

        if [ "$ROLLBACK_ON_FAIL" = true ]; then
            log ""
            log "=== Rolling back (health check failed) ==="
            # Start bridge to keep PICs alive during rollback
            $SSH_CMD "nohup $HB_BRIDGE_REMOTE >/dev/null 2>&1 &" 2>/dev/null
            sleep 3
            # Stop bad dcentrald
            $SSH_CMD "kill $NEW_PID 2>/dev/null; sleep 1; kill -9 $NEW_PID 2>/dev/null" 2>/dev/null || true
            # Restore backup
            $SSH_CMD "[ -f $BACKUP_PATH ] && mv $BACKUP_PATH $DEPLOY_PATH && chmod +x $DEPLOY_PATH" 2>/dev/null || true
            # Start old binary
            $SSH_CMD "nohup $DEPLOY_PATH --config $CONFIG_REMOTE >$LOG_PATH 2>&1 &" 2>/dev/null || true
            sleep 5
            # Kill bridge
            kill_hb_bridge
            ROLLBACK_PID=$($SSH_CMD "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || ROLLBACK_PID="NONE"
            log "  Restored backup, PID=$ROLLBACK_PID"
            json_exit false "$ROLLBACK_PID" "$BINARY_SIZE" false "API health check failed, rolled back to backup"
        fi
    fi
fi

# ── Step 12: Final Verification ─────────────────────────────────────────────

FINAL_PID=$($SSH_CMD "pidof dcentrald 2>/dev/null || echo NONE" 2>/dev/null) || FINAL_PID="NONE"
FINAL_BRIDGE=$($SSH_CMD "pidof hb_bridge.sh 2>/dev/null || echo NONE" 2>/dev/null) || FINAL_BRIDGE="NONE"

if [ "$FINAL_PID" = "NONE" ]; then
    log ""
    log "ERROR: dcentrald is not running after deploy!"
    json_exit false null "$BINARY_SIZE" "$API_HEALTHY" "dcentrald not running after deploy"
fi

if [ "$FINAL_BRIDGE" != "NONE" ]; then
    log "  WARNING: hb_bridge.sh still running (PID $FINAL_BRIDGE) — killing"
    $SSH_CMD "kill -9 $FINAL_BRIDGE 2>/dev/null" 2>/dev/null || true
fi

# ── Step 13: Final Report ───────────────────────────────────────────────────

DEPLOY_END=$(date +%s)
DEPLOY_TIME=$((DEPLOY_END - DEPLOY_START))

log_step "Safe Deploy Complete"
log "  Target:      root@$MINER_IP"
log "  Binary:      $DEPLOY_PATH ($BINARY_SIZE bytes)"
log "  Config:      $CONFIG_USED"
log "  PID:         $FINAL_PID"
log "  API health:  $API_HEALTHY"
log "  Deploy time: ${DEPLOY_TIME}s"
log "  PIC bridge:  used (no watchdog trip)"
log ""
log "  Log:       ssh root@$MINER_IP 'tail -f $LOG_PATH'"
log "  REST API:  http://$MINER_IP:$API_PORT/"
log "  CGMiner:   http://$MINER_IP:4028/"
log ""

# JSON output
if [ "$JSON_OUTPUT" = true ]; then
    cat <<ENDJSON
{
  "success": true,
  "pid": $FINAL_PID,
  "binary_size": $BINARY_SIZE,
  "deploy_time_seconds": $DEPLOY_TIME,
  "api_healthy": $API_HEALTHY,
  "miner_ip": "$MINER_IP",
  "config": "$CONFIG_USED",
  "pic_bridge_used": true,
  "message": "Safe deploy successful (PIC heartbeats maintained)"
}
ENDJSON
fi

# ── Step 14: Tail Log (if --tail) ──────────────────────────────────────────

if [ "$TAIL" = true ]; then
    log "=== Tailing log (Ctrl+C to stop) ==="
    exec $SSH_CMD "tail -f $LOG_PATH"
fi
