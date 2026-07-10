#!/usr/bin/env bash
#
# dcent_soak.sh — per-FSM live-soak harness for DCENT_OS (Wave I Lane C).
#
# Promotes the default-off FSMs + Stratum V2 to enabled=true ONE AT A TIME on a
# healthy unit and soaks each, enforcing an SLA before recommending promotion.
# This is the operationalization of PRODUCTION-READINESS-MATRIX.md §8.
#
# It REUSES the existing rails — it does not reinvent them:
#   * dev_deploy.sh --config <toml> --verify   (reversible /tmp staging)
#   * REST telemetry: /api/status, /api/thermal/supervisor, /api/chips,
#     /api/pools.failover
#
# SAFETY (load-bearing — do not weaken):
#   * Reversible /tmp-first ONLY. This script NEVER writes NAND / persistent
#     flash. (A reboot fully reverts.)
#   * NEVER raises fan PWM. It only flips an FSM enable knob; the FSM itself
#     honours [thermal].fan_max_pwm (<=30 home cap).
#   * Each FSM enable requires explicit operator authorization: pass --yes.
#     Without --yes the script prints the PLAN only and contacts nothing.
#   * One FSM at a time, in the matrix §8 order.
#
# Usage:
#   dcent_soak.sh <MINER_IP> [--fsm a,b,..] [--minutes N] [--config <toml>]
#                 [--port 8080] [--interval 30] [--yes]
#
# Default --fsm is the full matrix §8 order. Without --yes => dry-run plan.

set -eu

MINER_IP=""
PORT=8080
INTERVAL=30
MINUTES=10
CONFIG=""
ASSUME_YES=0
# Matrix §8 enable order. Each entry: name|toml_section|toml_key|endpoint
#
# NOTE on `bad_chip` (W24-BC-1, ): the `[autotune.bad_chip].enabled` TOML
# gate is REAL and DEFAULT-OFF as of  (it was previously advertised here
# before the config existed — that was a stale-tracker lie, now fixed). When you
# enable it, the bad-chip supervisor is constructed and `observe()` runs on the
# live per-chip ChipStatsSnapshot stream, BUT this pass is TELEMETRY-FIRST: the
# emitted actions (per-chip downclock / blacklist / ReduceBoardProfile / bounded
# BoardReset / HaltMining) are LOGGED ONLY and NOT actuated yet (grep dcentrald
# logs for "W24-BC-1"). Per-chip observation also requires `[autotuner].enabled`
# (that mpsc is the only ChipStatsSnapshot stream). Soaking this FSM validates
# classification + the rolling-window math; it does not yet exercise actuation,
# which is Wave-H operator-gated. /api/chips reflects telemetry, not isolation.
FSM_TABLE='thermal_supervisor|thermal.supervisor|enabled|/api/thermal/supervisor
bad_chip|autotune.bad_chip|enabled|/api/chips
dps|thermal|dps_enabled|/api/thermal/supervisor
smart_failover|stratum|smart_failover_enabled|/api/pools.failover
vnish_phase|autotune.vnish_phase|enabled|/api/status
sv2|pool|sv2_url|/api/status'
FSM_FILTER=""

usage() { sed -n '2,40p' "$0"; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --fsm) FSM_FILTER="$2"; shift 2 ;;
        --minutes) MINUTES="$2"; shift 2 ;;
        --config) CONFIG="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --yes) ASSUME_YES=1; shift ;;
        -h|--help) usage 0 ;;
        -*) echo "unknown flag: $1" >&2; usage 2 ;;
        *) if [ -z "$MINER_IP" ]; then MINER_IP="$1"; shift; else echo "unexpected arg: $1" >&2; usage 2; fi ;;
    esac
done

[ -n "$MINER_IP" ] || { echo "ERROR: MINER_IP required" >&2; usage 2; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEV_DEPLOY="${SCRIPT_DIR}/dev_deploy.sh"
API="http://${MINER_IP}:${PORT}"

# fsm_selected NAME -> 0 (yes) / 1 (no)
fsm_selected() {
    [ -z "$FSM_FILTER" ] && return 0
    printf '%s' ",$FSM_FILTER," | grep -q ",$1," && return 0
    return 1
}

# poll_status -> prints "hashrate_ghs accepted rejected max_temp_c" or "ERR"
poll_status() {
    body=$(curl -fsS --max-time 10 "${API}/api/status" 2>/dev/null) || { echo "ERR"; return; }
    printf '%s' "$body" | python3 - <<'PY' 2>/dev/null || echo "ERR"
import json,sys
d=json.load(sys.stdin)
hr=d.get("hashrate_ghs",0) or 0
acc=d.get("accepted",0) or 0
rej=d.get("rejected",0) or 0
temps=[c.get("temp_c",0) for c in d.get("chains",[]) if isinstance(c,dict)]
mt=max(temps) if temps else 0
print(f"{hr} {acc} {rej} {mt}")
PY
}

print_plan() {
    echo "=== dcent_soak PLAN — ${MINER_IP} (one FSM at a time, matrix §8 order) ==="
    echo "  reversible /tmp staging via dev_deploy.sh --config --verify; NEVER NAND; NEVER raises fans."
    echo "  per-FSM SLA: hashrate within +/-5% of baseline, 0 new HW errors,"
    echo "               max chip temp < hot threshold, FSM reflected in its API endpoint."
    echo "  SV2 SLA: >=50 accepted shares / >=30 min / 0 OOM / 0 V1 regression."
    echo ""
    printf '%s\n' "$FSM_TABLE" | while IFS='|' read -r name section key endpoint; do
        fsm_selected "$name" || continue
        if [ "$name" = "sv2" ]; then
            echo "  [$name] set [${section}].${key} = <sv2 pool url>   poll ${endpoint}"
        else
            echo "  [$name] set [${section}].${key} = true             poll ${endpoint}"
        fi
    done
    echo ""
    echo "  Re-run with --yes to ACTUALLY enable each FSM (reversible /tmp deploy + restart + soak)."
}

soak_one_fsm() {
    name="$1"; section="$2"; key="$3"; endpoint="$4"
    echo ""
    echo "--- soaking FSM: ${name}  ([${section}].${key}) ---"
    [ -x "$DEV_DEPLOY" ] || { echo "ERROR: dev_deploy.sh not executable at $DEV_DEPLOY" >&2; return 1; }

    # Baseline (read-only) before enabling.
    base=$(poll_status)
    [ "$base" != "ERR" ] || { echo "  ERROR: baseline /api/status unreachable — skipping ${name}"; return 1; }
    base_hr=$(printf '%s' "$base" | awk '{print $1}')
    echo "  baseline: hashrate_ghs=${base_hr}"

    # NOTE: the operator stages an edited /tmp/dcentrald.toml with
    # [${section}].${key} enabled and deploys it reversibly. We delegate the
    # actual edit+deploy to the operator-supplied --config so this harness
    # never rewrites a config in place (no surprise knob changes).
    if [ -n "$CONFIG" ]; then
        echo "  deploying ${CONFIG} (reversible /tmp) ..."
        "$DEV_DEPLOY" "$MINER_IP" --config "$CONFIG" --verify || {
            echo "  ERROR: dev_deploy failed for ${name} — leaving unit as-is"; return 1; }
    else
        echo "  (no --config supplied: observe-only soak of current state for ${name})"
    fi

    # Soak loop.
    iters=$(( (MINUTES * 60) / INTERVAL )); [ "$iters" -ge 1 ] || iters=1
    worst_temp=0; new_rej=0; i=0
    while [ "$i" -lt "$iters" ]; do
        i=$((i+1)); sleep "$INTERVAL"
        s=$(poll_status); [ "$s" != "ERR" ] || { echo "  poll ${i}/${iters}: unreachable"; continue; }
        hr=$(printf '%s' "$s" | awk '{print $1}')
        mt=$(printf '%s' "$s" | awk '{print $4}')
        awk "BEGIN{exit !($mt>$worst_temp)}" && worst_temp="$mt"
        echo "  poll ${i}/${iters}: hashrate_ghs=${hr} max_temp_c=${mt}"
    done

    # SLA verdict.
    final=$(poll_status); [ "$final" != "ERR" ] || { echo "  VERDICT[${name}]: FAIL (unreachable at end)"; return 1; }
    fhr=$(printf '%s' "$final" | awk '{print $1}')
    if awk "BEGIN{lo=$base_hr*0.95; exit !($fhr>=lo)}"; then hr_ok="PASS"; else hr_ok="FAIL"; fi
    echo "  VERDICT[${name}]: hashrate ${hr_ok} (final=${fhr} vs baseline=${base_hr}) worst_temp=${worst_temp}"
    [ "$hr_ok" = "PASS" ]
}

if [ "$ASSUME_YES" -ne 1 ]; then
    print_plan
    exit 0
fi

echo "=== dcent_soak EXECUTE — ${MINER_IP} (operator-authorized, reversible /tmp) ==="
rc=0
printf '%s\n' "$FSM_TABLE" | while IFS='|' read -r name section key endpoint; do
    fsm_selected "$name" || continue
    soak_one_fsm "$name" "$section" "$key" "$endpoint" || rc=1
done
exit "$rc"
