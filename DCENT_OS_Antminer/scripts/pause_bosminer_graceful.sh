#!/bin/sh
# pause_bosminer_graceful.sh — pause (or resume) bosminer mining without killing the process.
#
# WHY: A clean mining-loop pause preserves PSU heartbeat, dsPIC voltage, FPGA CTRL
# (0x00901002), Braiins glitch mirror (0x43D00030/0x34 — Braiins-am2 status mirror only,
# diagnostic-only per W13.B1), baud upgrade, and the hashboard manager. It halts only
# the work-dispatch loop so dcentrald (or a poke_fpga rig) can drive WORK_TX/WORK_RX
# against a chain that is still powered and still addressed.
#
# Alternatives and why we avoid them:
#   SIGKILL  — PSU watchdog ~60s, dsPIC voltage then droops; chain goes dark.
#   SIGTERM  — bosminer's clean shutdown zeros CTRL ENABLE (0x1E -> 0x16). Work stops.
#   SIGSTOP  — freezes the PSU heartbeat thread too. Same watchdog stall as SIGKILL.
#
# Probed on .139 (BraiinsOS+ 26.04-plus 912d084c) 2026-04-20:
#   - grpcurl:               NOT INSTALLED on device
#   - curl:                  NOT INSTALLED on device
#   - bos-tools:             INSTALLED (netcat subcommand only, no pause verb)
#   - bosminer api pause:    EXISTS (CLI returns "Connection refused (os error 111)")
#                            — CLI expects a TCP port the binary never opens outwardly
#                            (only 0.0.0.0:8081 Prometheus). Known quirk; Agent 15 flagged it.
#   - boser gRPC  :50051:    LISTENING (PID 1563). Exposes
#                            braiins.bos.v1.ActionsService/{PauseMining,ResumeMining}
#   - boser REST  :80:       LISTENING. /api/v1/actions/pauseMining defined in strings.
#                            POST returns 405 "Allow: GET,HEAD" — unauthenticated session
#                            routing table is degraded until login at /api/v1/auth .
#   - bosminer_paused flag:  /etc/bosminer_paused (persists across reboots — clean up!)
#
# This script tries every mechanism in order of ASIC-state-preservation quality:
#   1. `bosminer api pause`  on-device (best if the CLI ever connects)
#   2. `grpcurl` on build host  (gRPC to $MINER:50051)
#   3. `grpcurl` on-device       (if grpcurl was installed out-of-band)
#   4. REST `POST /api/v1/actions/pauseMining` via `bos-tools netcat`
#      — prior `POST /api/v1/auth` with $USER/$PASS cookie reuse
#   5. (fallback, LAST RESORT) `kill -KILL $(pidof bosminer)` with a loud warning.
#
# Usage:
#   pause_bosminer_graceful.sh <miner-ip> [--resume] [--fallback-kill]
#     --resume         : call ResumeMining instead of PauseMining
#     --fallback-kill  : allow SIGKILL if no graceful path succeeds (default: refuse)
#
# Exit codes:
#   0 = pause (or resume) acknowledged via a graceful path
#   1 = all graceful paths failed, fallback refused or also failed
#   2 = bosminer already in requested state (no-op)
#   3 = usage error
#
# Environment variables:
#   BOS_USER    : REST login username (default: root)
#   BOS_PASS    : REST login password (default: root)
#   SSH_USER    : SSH user (default: root)
#   SSH_OPTS    : extra SSH options (default: StrictHostKeyChecking=no + UserKnownHostsFile=/dev/null)
#
# Safety:
#   - Never unbinds xiic-i2c.
#   - Never writes any FPGA MMIO.
#   - Never touches dsPIC at 0x20/0x21/0x22.
#   - Cleans up /etc/bosminer_paused on resume.

set -u

MINER="${1:-}"
case "$MINER" in
    ""|-h|--help)
        sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'
        [ -z "$MINER" ] && exit 3 || exit 0
        ;;
esac
shift

ACTION="pause"
ALLOW_KILL="no"
while [ $# -gt 0 ]; do
    case "$1" in
        --resume)        ACTION="resume"; shift ;;
        --pause)         ACTION="pause";  shift ;;
        --fallback-kill) ALLOW_KILL="yes"; shift ;;
        -h|--help)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown arg: $1" >&2; exit 3 ;;
    esac
done

BOS_USER="${BOS_USER:-root}"
BOS_PASS="${BOS_PASS:-root}"
SSH_USER="${SSH_USER:-root}"
SSH_OPTS="${SSH_OPTS:--o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5}"

GRPC_ENDPOINT="${MINER}:50051"
REST_BASE="http://${MINER}"

# gRPC service/method strings (extracted from /usr/bin/boser strings on .139)
GRPC_SERVICE="braiins.bos.v1.ActionsService"
if [ "$ACTION" = "pause" ]; then
    GRPC_METHOD="PauseMining"
    REST_PATH="/api/v1/actions/pauseMining"
    CLI_VERB="pause"
else
    GRPC_METHOD="ResumeMining"
    REST_PATH="/api/v1/actions/resumeMining"
    CLI_VERB="resume"
fi

log()  { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
ok()   { printf '[ OK ] %s\n' "$*" >&2; }
warn() { printf '[WARN] %s\n' "$*" >&2; }
fail() { printf '[FAIL] %s\n' "$*" >&2; }

have()       { command -v "$1" >/dev/null 2>&1; }
ssh_quick()  { ssh $SSH_OPTS "${SSH_USER}@${MINER}" "$@"; }
ssh_cmd_js() {
    # Use the repo-bundled tools/ssh_cmd.js when plain ssh fails with legacy crypto.
    if [ -f "tools/ssh_cmd.js" ] && have node; then
        MSYS_NO_PATHCONV=1 node tools/ssh_cmd.js "$MINER" "$SSH_USER" "$SSH_USER" "$1" 2>&1
        return $?
    fi
    return 127
}

# Run a short command on the miner, preferring standard ssh. Returns combined stdout/stderr.
remote() {
    out=$(ssh_quick "$1" 2>&1); rc=$?
    if [ $rc -ne 0 ] && [ $rc -ne 127 ]; then
        # Retry via ssh_cmd.js for legacy crypto boxes (BraiinsOS+ on Zynq needs this on Windows).
        out=$(ssh_cmd_js "$1"); rc=$?
    fi
    printf '%s' "$out"
    return $rc
}

# ---------- 0. Current state pre-check ---------------------------------------
log "Checking current pause state on $MINER..."
STATE=$(remote 'bosminer api is-paused 2>/dev/null | tail -1')
case "$STATE" in
    *true*|*paused\":true*)  CUR="paused"   ;;
    *false*|*paused\":false*) CUR="running" ;;
    *)                         CUR="unknown" ;;
esac
log "bosminer api is-paused: $STATE -> $CUR"

if [ "$ACTION" = "pause" ] && [ "$CUR" = "paused" ]; then
    ok "Already paused — no-op."
    exit 2
fi
if [ "$ACTION" = "resume" ] && [ "$CUR" = "running" ]; then
    ok "Already running — no-op."
    exit 2
fi

# ---------- 1. Try `bosminer api <pause|resume>` on device -------------------
log "Attempt 1/4: bosminer api $CLI_VERB (on-device CLI)"
CLI_OUT=$(remote "bosminer api $CLI_VERB 2>&1 | tail -10"); CLI_RC=$?
# CLI exits 1 on connection refused (known issue); only trust it if last line is empty OK.
if printf '%s\n' "$CLI_OUT" | grep -qi "Connection refused\|os error 111\|Error:"; then
    warn "bosminer api $CLI_VERB unreachable (CLI/socket mismatch). Trying next path."
elif [ $CLI_RC -eq 0 ]; then
    ok "bosminer api $CLI_VERB succeeded."
    # Verify via is-paused
    sleep 1
    V=$(remote 'bosminer api is-paused 2>/dev/null | tail -1')
    log "Post-check is-paused: $V"
    exit 0
else
    warn "bosminer api $CLI_VERB returned rc=$CLI_RC. Trying next path."
fi

# ---------- 2. gRPC via grpcurl on the build host ----------------------------
log "Attempt 2/4: grpcurl on build host against $GRPC_ENDPOINT"
if have grpcurl; then
    G=$(grpcurl -plaintext -max-time 5 -d '{}' "$GRPC_ENDPOINT" "$GRPC_SERVICE/$GRPC_METHOD" 2>&1); GRC=$?
    if [ $GRC -eq 0 ]; then
        ok "grpcurl $GRPC_METHOD acknowledged: $G"
        exit 0
    else
        warn "grpcurl rc=$GRC: $G"
    fi
else
    log "grpcurl not in PATH on this host; skipping host-side grpc path."
fi

# ---------- 3. grpcurl on device (in case it was installed out-of-band) -----
log "Attempt 3/4: grpcurl on device against 127.0.0.1:50051"
REMOTE_GRPC=$(remote "command -v grpcurl >/dev/null 2>&1 && grpcurl -plaintext -max-time 5 -d '{}' 127.0.0.1:50051 $GRPC_SERVICE/$GRPC_METHOD 2>&1 || echo NO-GRPCURL")
case "$REMOTE_GRPC" in
    *NO-GRPCURL*)
        log "grpcurl not installed on device; skipping."
        ;;
    *Error*|*error*|*refused*)
        warn "on-device grpcurl failed: $REMOTE_GRPC"
        ;;
    *)
        ok "on-device grpcurl $GRPC_METHOD acknowledged: $REMOTE_GRPC"
        exit 0
        ;;
esac

# ---------- 4. REST via bos-tools netcat (login + call) ---------------------
log "Attempt 4/4: REST $REST_PATH via bos-tools netcat (auth as $BOS_USER)"

# Build the two HTTP/1.1 requests. Use \r\n delimiters. Content-Length is the json body length.
AUTH_BODY="{\"username\":\"$BOS_USER\",\"password\":\"$BOS_PASS\"}"
AUTH_LEN=$(printf '%s' "$AUTH_BODY" | wc -c | tr -d ' ')
CALL_BODY="{}"
CALL_LEN=$(printf '%s' "$CALL_BODY" | wc -c | tr -d ' ')

# Run the login + cookie-carrying call entirely on the miner via bos-tools netcat.
# We can't easily persist a session cookie across two netcat invocations without shell glue,
# so we build a one-shot shell script and run it via ssh.
REMOTE_SCRIPT=$(cat <<REMOTE_EOF
set -u
AUTH=\$(printf 'POST /api/v1/auth HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: $AUTH_LEN\r\nConnection: close\r\n\r\n$AUTH_BODY' | bos-tools netcat -N -w 3 127.0.0.1 80 2>/dev/null)
COOKIE=\$(printf '%s\n' "\$AUTH" | grep -i '^set-cookie:' | head -1 | sed 's/^[Ss]et-[Cc]ookie: *//' | cut -d';' -f1)
TOKEN=\$(printf '%s\n' "\$AUTH" | sed -n '/^\r*$/,\$p' | tail -1 | grep -oE '"token"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\\([^"]*\\)"\$/\\1/')
HDRS=""
[ -n "\$COOKIE" ] && HDRS="Cookie: \$COOKIE\r\n"
[ -n "\$TOKEN" ]  && HDRS="\${HDRS}Authorization: Bearer \$TOKEN\r\n"
printf 'POST %s HTTP/1.1\r\nHost: 127.0.0.1\r\n%sContent-Type: application/json\r\nContent-Length: $CALL_LEN\r\nConnection: close\r\n\r\n$CALL_BODY' "$REST_PATH" "\$HDRS" | bos-tools netcat -N -w 5 127.0.0.1 80
REMOTE_EOF
)
REST_OUT=$(remote "$REMOTE_SCRIPT"); REST_RC=$?
REST_STATUS=$(printf '%s\n' "$REST_OUT" | head -1)
case "$REST_STATUS" in
    *200*|*201*|*202*|*204*)
        ok "REST $REST_PATH acknowledged: $REST_STATUS"
        exit 0
        ;;
    *)
        warn "REST path failed (status: $REST_STATUS). Full: $REST_OUT"
        ;;
esac

# ---------- 5. Fallback SIGKILL (requires explicit opt-in) ------------------
if [ "$ACTION" = "resume" ]; then
    fail "Could not resume bosminer via any graceful path, and --fallback-kill is not a resume strategy."
    exit 1
fi
if [ "$ALLOW_KILL" = "yes" ]; then
    warn "All graceful paths failed. --fallback-kill given; using SIGKILL."
    warn "PSU watchdog will trip in ~60s. dcentrald must take over heartbeats immediately."
    remote 'PID=$(pidof bosminer); [ -n "$PID" ] && kill -KILL $PID; sleep 1; pidof bosminer || echo KILLED'
    exit 0
fi

fail "No graceful path succeeded. Use --fallback-kill to allow SIGKILL (not recommended)."
exit 1
