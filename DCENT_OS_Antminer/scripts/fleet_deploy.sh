#!/bin/bash
# DCENTos - Fleet Deploy
# D-Central Technologies, 2026
#
# Wave B (2026-05-19): the --passthrough CLI flag was removed. The
# [mining].passthrough = true knob in /data/dcentrald.toml is the canonical
# way to request passthrough mode; the S82 init script reads it. See
#  G-T8-1.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

DEPLOY_MODE="sequential"
MAX_PARALLEL=3
BATCH_SIZE=0
FILTER_MINERS=""
DEPLOY_ALL=false
VERIFY=false
SKIP_BUILD=false
DRY_RUN=false
OUTPUT_FORMAT="table"
FIRMWARE_FILTER=""
STOP_ON_FAILURE=false
RETRY_FAILED=0
RESUME_FROM_AUDIT=""
PREFLIGHT_ENABLED=false
PREFLIGHT_STRICT=false
SELECTION_REQUESTED=false

RUN_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
RUN_EPOCH_START="$(date +%s)"
ROLL_OUT_STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
ROLL_OUT_ID="fleet-rollout-${ROLL_OUT_STAMP}-$$"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DEPLOY_SCRIPT="$SCRIPT_DIR/deploy_dcentrald.sh"
AUDIT_FILE_DEFAULT="$PROJECT_DIR/docs/dev/${ROLL_OUT_ID}.json"
AUDIT_FILE="$AUDIT_FILE_DEFAULT"

MINERS_TOML=""
for dir in "$PROJECT_DIR" "$(dirname "$PROJECT_DIR")" "$(dirname "$(dirname "$PROJECT_DIR")")"; do
    if [ -f "$dir/miners.toml" ]; then
        MINERS_TOML="$dir/miners.toml"
        break
    fi
done

if [ -z "$MINERS_TOML" ]; then
    echo "ERROR: miners.toml not found. Searched up from $SCRIPT_DIR"
    exit 1
fi

if [ ! -f "$DEPLOY_SCRIPT" ]; then
    echo "ERROR: deploy_dcentrald.sh not found at $DEPLOY_SCRIPT"
    exit 1
fi

usage() {
    cat <<'USAGE'
DCENTos Fleet Deploy - D-Central Technologies

Usage:
  fleet_deploy.sh [OPTIONS]

Target Selection (one required):
  --all                   Deploy to all miners in miners.toml
  --miners NAME[,NAME]    Deploy to specific miners (comma-separated names)
  --firmware FW           Deploy only to miners with given firmware type

Deploy Options:
  --sequential            Deploy one at a time (default)
  --parallel              Deploy in parallel (max 3 concurrent)
  --max-parallel N        Max parallel deploys (default: 3)
  --batch-size N          Deploy and verify in batches of N targets
  --stop-on-failure       Stop before the next batch after a failure
  --retry-failed N        Retry failed deploys up to N extra attempts
  --resume-from-audit P   Target only unfinished miners from prior audit JSON
  --preflight             Run SSH/API reachability checks before deploy
  --preflight-strict      Abort before deploy if any preflight check is not ready
  --skip-build            Skip cargo build, deploy existing binary
  --verify                SSH into each miner after deploy to verify
  --dry-run               Show what would be deployed without doing it
  --json                  Emit structured JSON summary to stdout
  --audit-file PATH       Write rollout audit JSON to this path

General:
  --config PATH           Path to miners.toml
  --help                  Show this help

Examples:
  fleet_deploy.sh --all --verify
  fleet_deploy.sh --miners s9-97,s9-36 --skip-build --json
  fleet_deploy.sh --all --parallel --max-parallel 2
  fleet_deploy.sh --all --parallel --batch-size 2 --verify --stop-on-failure
  fleet_deploy.sh --all --parallel --retry-failed 2 --verify
  fleet_deploy.sh --resume-from-audit docs/dev/fleet-rollout-*.json --verify
  fleet_deploy.sh --firmware dcentos --verify
USAGE
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --all)            DEPLOY_ALL=true; SELECTION_REQUESTED=true; shift ;;
        --miners)         FILTER_MINERS="$2"; shift 2 ;;
        --firmware)       FIRMWARE_FILTER="$2"; SELECTION_REQUESTED=true; shift 2 ;;
        --sequential)     DEPLOY_MODE="sequential"; shift ;;
        --parallel)       DEPLOY_MODE="parallel"; shift ;;
        --max-parallel)   MAX_PARALLEL="$2"; shift 2 ;;
        --batch-size)     BATCH_SIZE="$2"; shift 2 ;;
        --stop-on-failure) STOP_ON_FAILURE=true; shift ;;
        --retry-failed)   RETRY_FAILED="$2"; shift 2 ;;
        --resume-from-audit) RESUME_FROM_AUDIT="$2"; shift 2 ;;
        --preflight)      PREFLIGHT_ENABLED=true; shift ;;
        --preflight-strict) PREFLIGHT_ENABLED=true; PREFLIGHT_STRICT=true; shift ;;
        --skip-build)     SKIP_BUILD=true; shift ;;
        --verify)         VERIFY=true; shift ;;
        --dry-run)        DRY_RUN=true; shift ;;
        --json)           OUTPUT_FORMAT="json"; shift ;;
        --audit-file)     AUDIT_FILE="$2"; shift 2 ;;
        --config)         MINERS_TOML="$2"; shift 2 ;;
        --help|-h)        usage ;;
        *)                echo "Unknown option: $1"; usage ;;
    esac
done

if [ -n "$FILTER_MINERS" ]; then
    SELECTION_REQUESTED=true
fi

if [ "$SELECTION_REQUESTED" = false ] && [ -z "$RESUME_FROM_AUDIT" ]; then
    echo "ERROR: Must specify --all, --miners, --firmware, or --resume-from-audit"
    echo "Run with --help for usage."
    exit 1
fi

is_positive_integer() {
    case "$1" in
        ''|*[!0-9]*|0) return 1 ;;
        *) return 0 ;;
    esac
}

is_non_negative_integer() {
    case "$1" in
        ''|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

if ! is_positive_integer "$MAX_PARALLEL"; then
    echo "ERROR: --max-parallel must be a positive integer"
    exit 1
fi

if ! is_non_negative_integer "$BATCH_SIZE"; then
    echo "ERROR: --batch-size must be 0 or a positive integer"
    exit 1
fi

if ! is_non_negative_integer "$RETRY_FAILED"; then
    echo "ERROR: --retry-failed must be 0 or a positive integer"
    exit 1
fi

if [ -n "$RESUME_FROM_AUDIT" ] && [ ! -f "$RESUME_FROM_AUDIT" ]; then
    echo "ERROR: --resume-from-audit file not found: $RESUME_FROM_AUDIT"
    exit 1
fi

json_escape() {
    local value
    value="$1"
    value=${value//\\/\\\\}
    value=${value//"/\\"}
    value=${value//$'\n'/\\n}
    value=${value//$'\r'/\\r}
    value=${value//$'\t'/\\t}
    printf '%s' "$value"
}

json_string() {
    printf '"%s"' "$(json_escape "$1")"
}

json_string_or_null() {
    if [ -n "$1" ] && [ "$1" != "-" ]; then
        json_string "$1"
    else
        printf 'null'
    fi
}

json_number_or_null() {
    if [ -n "$1" ] && [ "$1" != "-" ]; then
        printf '%s' "$1"
    else
        printf 'null'
    fi
}

json_bool() {
    if [ "$1" = "true" ]; then
        printf 'true'
    else
        printf 'false'
    fi
}

parse_audit_pending_miners() {
    local audit_file="$1"

    awk '
        /"miners"[[:space:]]*:[[:space:]]*\[/ {
            in_miners = 1
            next
        }

        in_miners && /^  ]/ {
            exit
        }

        in_miners && /^    \{$/ {
            name = ""
            deploy_status = ""
            in_deploy = 0
            next
        }

        in_miners && /"name"[[:space:]]*:/ {
            if (match($0, /"name"[[:space:]]*:[[:space:]]*"[^"]+"/)) {
                line = substr($0, RSTART, RLENGTH)
                sub(/^.*"name"[[:space:]]*:[[:space:]]*"/, "", line)
                sub(/"$/, "", line)
                name = line
            }
            next
        }

        in_miners && /"deploy"[[:space:]]*:[[:space:]]*\{/ {
            in_deploy = 1
            next
        }

        in_miners && in_deploy && /"status"[[:space:]]*:/ {
            if (match($0, /"status"[[:space:]]*:[[:space:]]*"[^"]+"/)) {
                line = substr($0, RSTART, RLENGTH)
                sub(/^.*"status"[[:space:]]*:[[:space:]]*"/, "", line)
                sub(/"$/, "", line)
                deploy_status = line
            }
            in_deploy = 0
            next
        }

        in_miners && /^    }[,]?$/ {
            if (name != "" && deploy_status != "ok") {
                print name
            }
        }
    ' "$audit_file"
}

build_name_set() {
    local line name name_set="|"

    while IFS= read -r line; do
        [ -z "$line" ] && continue
        name="$line"
        name_set="${name_set}${name}|"
    done

    printf '%s' "$name_set"
}

name_in_set() {
    local name="$1" name_set="$2"
    case "$name_set" in
        *"|${name}|"*) return 0 ;;
        *) return 1 ;;
    esac
}

console_printf() {
    if [ "$OUTPUT_FORMAT" = "json" ]; then
        printf "$@" >&2
    else
        printf "$@"
    fi
}

parse_miners() {
    local current_name=""
    local current_ip="" current_model="" current_fw="" current_notes="" current_user=""

    while IFS= read -r line; do
        line="$(echo "$line" | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        [[ -z "$line" || "$line" == \#* ]] && continue

        if [[ "$line" =~ ^\[miners\.([a-zA-Z0-9_-]+)\]$ ]]; then
            if [ -n "$current_name" ]; then
                echo "${current_name}|${current_ip}|${current_model}|${current_fw}|${current_notes}|${current_user}"
            fi
            current_name="${BASH_REMATCH[1]}"
            current_ip="" current_model="" current_fw="" current_notes="" current_user=""
            continue
        fi

        if [[ "$line" =~ ^\[.*\]$ ]] && [[ ! "$line" =~ ^\[miners\. ]]; then
            if [ -n "$current_name" ]; then
                echo "${current_name}|${current_ip}|${current_model}|${current_fw}|${current_notes}|${current_user}"
            fi
            current_name=""
            continue
        fi

        if [ -n "$current_name" ]; then
            local key val
            key="$(echo "$line" | cut -d'=' -f1 | sed 's/[[:space:]]*$//')"
            val="$(echo "$line" | cut -d'=' -f2- | sed 's/^[[:space:]]*//;s/^"//;s/"$//')"
            case "$key" in
                ip)       current_ip="$val" ;;
                model)    current_model="$val" ;;
                firmware) current_fw="$val" ;;
                notes)    current_notes="$val" ;;
                ssh_user) current_user="$val" ;;
            esac
        fi
    done < "$1"

    if [ -n "$current_name" ]; then
        echo "${current_name}|${current_ip}|${current_model}|${current_fw}|${current_notes}|${current_user}"
    fi
}

deploy_one() {
    local ip="$1" log_file="$2"
    local deploy_args=()
    local start_epoch end_epoch duration_seconds exit_code status started_at finished_at

    deploy_args+=("$ip")
    if [ "$SKIP_BUILD" = true ]; then
        deploy_args+=("--skip-build")
    fi

    start_epoch="$(date +%s)"
    started_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    if bash "$DEPLOY_SCRIPT" "${deploy_args[@]}" >"$log_file" 2>&1; then
        exit_code=0
        status="ok"
    else
        exit_code=$?
        status="failed"
    fi

    end_epoch="$(date +%s)"
    finished_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    duration_seconds=$((end_epoch - start_epoch))

    echo "${status}|${exit_code}|${duration_seconds}|${duration_seconds}s|${started_at}|${finished_at}"
}

deploy_with_retries() {
    local ip="$1" log_file="$2"
    local max_attempts attempt attempt_log attempt_result
    local deploy_status deploy_exit duration_seconds duration_h started_at finished_at
    local first_started_at="" final_finished_at="" final_status="failed" final_exit="1"
    local overall_start overall_end overall_duration retry_count=0 attempts_used=0

    max_attempts=$((RETRY_FAILED + 1))
    overall_start="$(date +%s)"
    : > "$log_file"

    for ((attempt=1; attempt<=max_attempts; attempt++)); do
        attempts_used="$attempt"
        attempt_log="${log_file}.attempt${attempt}"
        attempt_result=$(deploy_one "$ip" "$attempt_log")
        deploy_status=$(echo "$attempt_result" | cut -d'|' -f1)
        deploy_exit=$(echo "$attempt_result" | cut -d'|' -f2)
        duration_seconds=$(echo "$attempt_result" | cut -d'|' -f3)
        duration_h=$(echo "$attempt_result" | cut -d'|' -f4)
        started_at=$(echo "$attempt_result" | cut -d'|' -f5)
        finished_at=$(echo "$attempt_result" | cut -d'|' -f6)

        if [ -z "$first_started_at" ]; then
            first_started_at="$started_at"
        fi
        final_finished_at="$finished_at"
        final_status="$deploy_status"
        final_exit="$deploy_exit"
        retry_count=$((attempt - 1))

        {
            printf '=== Attempt %d/%d ===\n' "$attempt" "$max_attempts"
            cat "$attempt_log"
            printf '\n'
        } >> "$log_file"
        rm -f "$attempt_log"

        if [ "$deploy_status" = "ok" ]; then
            break
        fi
    done

    overall_end="$(date +%s)"
    overall_duration=$((overall_end - overall_start))
    echo "${final_status}|${final_exit}|${overall_duration}|${overall_duration}s|${first_started_at}|${final_finished_at}|${attempts_used}|${retry_count}"
}

verify_one() {
    local ip="$1" user="$2"
    local checked_at pid version
    local ssh_opts="-o StrictHostKeyChecking=no -o ConnectTimeout=10 -o BatchMode=yes"

    checked_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    local info
    info=$(ssh $ssh_opts "${user}@${ip}" '
        PID=$(pidof dcentrald 2>/dev/null || echo "")
        if [ -n "$PID" ]; then
            VER=$(/usr/local/bin/dcentrald --version 2>/dev/null || echo "unknown")
            echo "RUNNING|$PID|$VER"
        else
            echo "NOT_RUNNING||"
        fi
    ' 2>/dev/null) || info="UNREACHABLE||ssh_failed"

    pid=$(echo "$info" | cut -d'|' -f2)
    version=$(echo "$info" | cut -d'|' -f3-)

    local verify_status api_ok health_status miner_state hashrate_ths temp_c max_temp_c fan_rpm fan_pwm pool_status verify_error
    verify_status="$(echo "$info" | cut -d'|' -f1)"
    api_ok=false
    health_status="unknown"
    miner_state="unknown"
    hashrate_ths=""
    temp_c=""
    max_temp_c=""
    fan_rpm=""
    fan_pwm=""
    pool_status=""
    verify_error=""

    if [ "$verify_status" = "RUNNING" ]; then
        local api_resp compact_json hashrate_ghs temp_values
        api_resp=$(curl -s --connect-timeout 5 --max-time 8 "http://${ip}:8080/api/status" 2>/dev/null) || true

        if [ -n "$api_resp" ]; then
            api_ok=true
            health_status="healthy"
            miner_state="idle"
            compact_json="$(printf '%s' "$api_resp" | tr -d '\r\n')"

            hashrate_ths=$(printf '%s' "$api_resp" | grep -o '"hashrate_ths"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$' || true)
            if [ -z "$hashrate_ths" ]; then
                hashrate_ghs=$(printf '%s' "$api_resp" | grep -o '"hashrate_ghs"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$' || true)
                if [ -n "$hashrate_ghs" ]; then
                    hashrate_ths=$(awk "BEGIN { printf \"%.2f\", $hashrate_ghs / 1000 }")
                fi
            fi

            temp_values=$(printf '%s' "$api_resp" | grep -o '"temp_c"[[:space:]]*:[[:space:]]*[0-9.]*' | grep -o '[0-9.]*$' || true)
            temp_c=$(printf '%s\n' "$temp_values" | awk 'NF { print; exit }')
            max_temp_c=$(printf '%s\n' "$temp_values" | awk 'NF { if (max == "" || $1 > max) max = $1 } END { if (max != "") printf "%.1f", max }')

            fan_rpm=$(printf '%s' "$api_resp" | grep -o '"rpm"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$' || true)
            fan_pwm=$(printf '%s' "$api_resp" | grep -o '"pwm"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$' || true)
            pool_status=$(printf '%s' "$compact_json" | grep -o '"pool"[[:space:]]*:[[:space:]]*{[^}]*"status"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)"$/\1/' || true)

            if [ -n "$hashrate_ths" ] && awk "BEGIN { exit !($hashrate_ths > 0) }"; then
                miner_state="mining"
            elif printf '%s' "$compact_json" | grep -qi 'standby\|sleep'; then
                miner_state="standby"
                health_status="standby"
            fi

            if [ -n "$fan_pwm" ] && [ -n "$fan_rpm" ] && awk "BEGIN { exit !(($fan_pwm > 20) && ($fan_rpm == 0)) }"; then
                health_status="cooling_fault"
                verify_error="fan_rpm_zero"
            elif [ -n "$max_temp_c" ] && awk "BEGIN { exit !($max_temp_c >= 70) }"; then
                health_status="hot"
                verify_error="high_temp"
            elif [ "$miner_state" = "idle" ]; then
                health_status="idle"
            fi
        else
            health_status="api_unavailable"
            verify_error="status_api_unreachable"
        fi
    elif [ "$verify_status" = "NOT_RUNNING" ]; then
        health_status="not_running"
        miner_state="stopped"
    else
        health_status="unreachable"
        miner_state="unreachable"
        verify_error="ssh_failed"
    fi

    echo "${verify_status}|${pid}|${version}|${checked_at}|${api_ok}|${health_status}|${miner_state}|${hashrate_ths}|${temp_c}|${max_temp_c}|${fan_rpm}|${fan_pwm}|${pool_status}|${verify_error}"
}

preflight_one() {
    local ip="$1" user="$2"
    local checked_at ssh_ok api_ok status message
    local ssh_opts="-o StrictHostKeyChecking=no -o ConnectTimeout=5 -o BatchMode=yes"

    checked_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    ssh_ok=false
    api_ok=false

    if ssh $ssh_opts "${user}@${ip}" 'exit 0' >/dev/null 2>&1; then
        ssh_ok=true
    fi

    if curl -s --connect-timeout 4 --max-time 6 "http://${ip}:8080/api/status" >/dev/null 2>&1; then
        api_ok=true
    fi

    if [ "$ssh_ok" = true ] && [ "$api_ok" = true ]; then
        status="ready"
        message="ssh+api reachable"
    elif [ "$ssh_ok" = true ]; then
        status="ssh_only"
        message="ssh reachable but api unavailable"
    elif [ "$api_ok" = true ]; then
        status="api_only"
        message="api reachable but ssh unavailable"
    else
        status="unreachable"
        message="ssh and api unreachable"
    fi

    echo "${status}|${ssh_ok}|${api_ok}|${checked_at}|${message}"
}

verify_batch() {
    local batch_results="$1"
    local batch_has_issue=false

    [ -z "$batch_results" ] && return 0

    while IFS='|' read -r name ip model fw notes user deploy_status deploy_exit duration_seconds duration_h started_at finished_at deploy_attempts deploy_retries log_path; do
        local vresult vstatus vpid vversion vchecked_at vapi_ok vhealth_status vminer_state vhashrate_ths vtemp_c vmax_temp_c vfan_rpm vfan_pwm vpool_status verror
        [ -z "$name" ] && continue
        [ -z "$user" ] && user="root"

        console_printf "  Verifying ${CYAN}%s${RESET}..." "$name"
        vresult=$(verify_one "$ip" "$user")
        vstatus=$(echo "$vresult" | cut -d'|' -f1)
        vpid=$(echo "$vresult" | cut -d'|' -f2)
        vversion=$(echo "$vresult" | cut -d'|' -f3)
        vchecked_at=$(echo "$vresult" | cut -d'|' -f4)
        vapi_ok=$(echo "$vresult" | cut -d'|' -f5)
        vhealth_status=$(echo "$vresult" | cut -d'|' -f6)
        vminer_state=$(echo "$vresult" | cut -d'|' -f7)
        vhashrate_ths=$(echo "$vresult" | cut -d'|' -f8)
        vtemp_c=$(echo "$vresult" | cut -d'|' -f9)
        vmax_temp_c=$(echo "$vresult" | cut -d'|' -f10)
        vfan_rpm=$(echo "$vresult" | cut -d'|' -f11)
        vfan_pwm=$(echo "$vresult" | cut -d'|' -f12)
        vpool_status=$(echo "$vresult" | cut -d'|' -f13)
        verror=$(echo "$vresult" | cut -d'|' -f14)

        case "$vstatus" in
            RUNNING)
                case "$vhealth_status" in
                    healthy)
                        console_printf " ${GREEN}HEALTHY${RESET} (%s, %s TH/s, max %sC)\n" "$vpid" "${vhashrate_ths:--}" "${vmax_temp_c:--}"
                        VERIFY_HEALTHY_COUNT=$((VERIFY_HEALTHY_COUNT + 1))
                        ;;
                    standby)
                        console_printf " ${YELLOW}STANDBY${RESET} (%s, API ok)\n" "$vpid"
                        VERIFY_STANDBY_COUNT=$((VERIFY_STANDBY_COUNT + 1))
                        ;;
                    *)
                        console_printf " ${YELLOW}%s${RESET} (%s%s)\n" "${vhealth_status^^}" "$vpid" "$([ -n "$verror" ] && printf ', %s' "$verror")"
                        VERIFY_HEALTH_ISSUE_COUNT=$((VERIFY_HEALTH_ISSUE_COUNT + 1))
                        batch_has_issue=true
                        ;;
                esac
                VERIFY_RUNNING_COUNT=$((VERIFY_RUNNING_COUNT + 1))
                ;;
            NOT_RUNNING)
                console_printf " ${RED}NOT RUNNING${RESET}\n"
                VERIFY_NOT_RUNNING_COUNT=$((VERIFY_NOT_RUNNING_COUNT + 1))
                VERIFY_HEALTH_ISSUE_COUNT=$((VERIFY_HEALTH_ISSUE_COUNT + 1))
                batch_has_issue=true
                ;;
            *)
                console_printf " ${RED}UNREACHABLE${RESET}\n"
                VERIFY_UNREACHABLE_COUNT=$((VERIFY_UNREACHABLE_COUNT + 1))
                VERIFY_HEALTH_ISSUE_COUNT=$((VERIFY_HEALTH_ISSUE_COUNT + 1))
                batch_has_issue=true
                ;;
        esac

        VERIFY_RESULTS+="${name}|${vstatus}|${vpid}|${vversion}|${vchecked_at}|${vapi_ok}|${vhealth_status}|${vminer_state}|${vhashrate_ths}|${vtemp_c}|${vmax_temp_c}|${vfan_rpm}|${vfan_pwm}|${vpool_status}|${verror}"$'\n'
    done <<< "$batch_results"

    [ "$batch_has_issue" = true ] && return 1
    return 0
}

build_json_report() {
    local generated_at overall_status temp_log_dir

    generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    temp_log_dir=""
    if [ "$KEEP_TMPDIR" = true ]; then
        temp_log_dir="$TMPDIR_PATH"
    fi

    if [ "$DRY_RUN" = true ]; then
        overall_status="dry_run"
    elif [ "$BUILD_STATUS" = "failed" ]; then
        overall_status="build_failed"
    elif [ "$STOPPED_EARLY" = true ]; then
        overall_status="stopped_on_failure"
    elif [ "$FAIL_COUNT" -gt 0 ]; then
        overall_status="partial_failure"
    else
        overall_status="success"
    fi

    printf '{\n'
    printf '  "rollout_id": %s,\n' "$(json_string "$ROLL_OUT_ID")"
    printf '  "generated_at": %s,\n' "$(json_string "$generated_at")"
    printf '  "started_at": %s,\n' "$(json_string "$RUN_STARTED_AT")"
    printf '  "finished_at": %s,\n' "$(json_string "$RUN_FINISHED_AT")"
    printf '  "status": %s,\n' "$(json_string "$overall_status")"
    printf '  "config_path": %s,\n' "$(json_string "$MINERS_TOML")"
    printf '  "deploy_script": %s,\n' "$(json_string "$DEPLOY_SCRIPT")"
    printf '  "audit_file": %s,\n' "$(json_string "$AUDIT_FILE")"
    printf '  "temp_log_dir": %s,\n' "$(json_string_or_null "$temp_log_dir")"
    printf '  "options": {\n'
    printf '    "output": %s,\n' "$(json_string "$OUTPUT_FORMAT")"
    printf '    "deploy_mode": %s,\n' "$(json_string "$DEPLOY_MODE")"
    printf '    "max_parallel": %s,\n' "$MAX_PARALLEL"
    printf '    "batch_size": %s,\n' "$BATCH_SIZE"
    printf '    "retry_failed": %s,\n' "$RETRY_FAILED"
    printf '    "preflight": %s,\n' "$(json_bool "$PREFLIGHT_ENABLED")"
    printf '    "preflight_strict": %s,\n' "$(json_bool "$PREFLIGHT_STRICT")"
    printf '    "deploy_all": %s,\n' "$(json_bool "$DEPLOY_ALL")"
    printf '    "miners_filter": %s,\n' "$(json_string_or_null "$FILTER_MINERS")"
     printf '    "firmware_filter": %s,\n' "$(json_string_or_null "$FIRMWARE_FILTER")"
    printf '    "resume_from_audit": %s,\n' "$(json_string_or_null "$RESUME_FROM_AUDIT")"
     printf '    "stop_on_failure": %s,\n' "$(json_bool "$STOP_ON_FAILURE")"
    printf '    "verify": %s,\n' "$(json_bool "$VERIFY")"
    printf '    "skip_build_requested": %s,\n' "$(json_bool "$SKIP_BUILD_REQUESTED")"
    printf '    "skip_build_effective": %s,\n' "$(json_bool "$SKIP_BUILD")"
    printf '    "dry_run": %s\n' "$(json_bool "$DRY_RUN")"
    printf '  },\n'
    printf '  "build": {\n'
    printf '    "status": %s,\n' "$(json_string "$BUILD_STATUS")"
    printf '    "started_at": %s,\n' "$(json_string_or_null "$BUILD_STARTED_AT")"
    printf '    "finished_at": %s,\n' "$(json_string_or_null "$BUILD_FINISHED_AT")"
    printf '    "duration_seconds": %s,\n' "$(json_number_or_null "$BUILD_DURATION_SECONDS")"
    printf '    "log_path": %s\n' "$(json_string_or_null "$BUILD_LOG_PATH")"
    printf '  },\n'
    printf '  "summary": {\n'
    printf '    "targets": %s,\n' "$TARGET_COUNT"
    printf '    "deploy_attempted": %s,\n' "$DEPLOY_COUNT"
    printf '    "deploy_retry_attempts": %s,\n' "$RETRY_ATTEMPT_COUNT"
    printf '    "deploy_succeeded": %s,\n' "$SUCCESS_COUNT"
    printf '    "deploy_failed": %s,\n' "$FAIL_COUNT"
    printf '    "deploy_skipped": %s,\n' "$SKIPPED_COUNT"
    printf '    "stopped_early": %s,\n' "$(json_bool "$STOPPED_EARLY")"
    printf '    "stop_reason": %s,\n' "$(json_string_or_null "$STOP_REASON")"
    printf '    "verified_running": %s,\n' "$VERIFY_RUNNING_COUNT"
    printf '    "verified_not_running": %s,\n' "$VERIFY_NOT_RUNNING_COUNT"
    printf '    "verified_unreachable": %s,\n' "$VERIFY_UNREACHABLE_COUNT"
    printf '    "verified_health_ok": %s,\n' "$VERIFY_HEALTHY_COUNT"
    printf '    "verified_standby": %s,\n' "$VERIFY_STANDBY_COUNT"
    printf '    "verified_health_issues": %s,\n' "$VERIFY_HEALTH_ISSUE_COUNT"
    printf '    "preflight_ready": %s,\n' "$PRECHECK_OK_COUNT"
    printf '    "preflight_issues": %s\n' "$PRECHECK_ISSUE_COUNT"
    printf '  },\n'
    printf '  "miners": [\n'

    local first=true
    while IFS='|' read -r name ip model fw notes user deploy_status deploy_exit duration_seconds duration_h started_at finished_at deploy_attempts deploy_retries log_path; do
        [ -z "$name" ] && continue

        local verify_line verify_status verify_pid verify_version verify_checked_at verify_api_ok verify_health_status verify_miner_state verify_hashrate_ths verify_temp_c verify_max_temp_c verify_fan_rpm verify_fan_pwm verify_pool_status verify_error verify_success verify_health_ok
        verify_line="$(echo "$VERIFY_RESULTS" | grep "^${name}|" | head -1 || true)"
        verify_status="$(echo "$verify_line" | cut -d'|' -f2)"
        verify_pid="$(echo "$verify_line" | cut -d'|' -f3)"
        verify_version="$(echo "$verify_line" | cut -d'|' -f4)"
        verify_checked_at="$(echo "$verify_line" | cut -d'|' -f5)"
        verify_api_ok="$(echo "$verify_line" | cut -d'|' -f6)"
        verify_health_status="$(echo "$verify_line" | cut -d'|' -f7)"
        verify_miner_state="$(echo "$verify_line" | cut -d'|' -f8)"
        verify_hashrate_ths="$(echo "$verify_line" | cut -d'|' -f9)"
        verify_temp_c="$(echo "$verify_line" | cut -d'|' -f10)"
        verify_max_temp_c="$(echo "$verify_line" | cut -d'|' -f11)"
        verify_fan_rpm="$(echo "$verify_line" | cut -d'|' -f12)"
        verify_fan_pwm="$(echo "$verify_line" | cut -d'|' -f13)"
        verify_pool_status="$(echo "$verify_line" | cut -d'|' -f14)"
        verify_error="$(echo "$verify_line" | cut -d'|' -f15)"
        verify_success=false
        verify_health_ok=false
        if [ "$verify_status" = "RUNNING" ]; then
            verify_success=true
        fi
        if [ "$verify_health_status" = "healthy" ] || [ "$verify_health_status" = "standby" ]; then
            verify_health_ok=true
        fi

        local preflight_line preflight_status preflight_ssh_ok preflight_api_ok preflight_checked_at preflight_message preflight_ready
        preflight_line="$(echo "$PRECHECK_RESULTS" | grep "^${name}|" | head -1 || true)"
        preflight_status="$(echo "$preflight_line" | cut -d'|' -f2)"
        preflight_ssh_ok="$(echo "$preflight_line" | cut -d'|' -f3)"
        preflight_api_ok="$(echo "$preflight_line" | cut -d'|' -f4)"
        preflight_checked_at="$(echo "$preflight_line" | cut -d'|' -f5)"
        preflight_message="$(echo "$preflight_line" | cut -d'|' -f6-)"
        preflight_ready=false
        if [ "$preflight_status" = "ready" ]; then
            preflight_ready=true
        fi

        if [ "$first" = true ]; then
            first=false
        else
            printf ',\n'
        fi

        printf '    {\n'
        printf '      "name": %s,\n' "$(json_string "$name")"
        printf '      "ip": %s,\n' "$(json_string "$ip")"
        printf '      "model": %s,\n' "$(json_string_or_null "$model")"
        printf '      "expected_firmware": %s,\n' "$(json_string_or_null "$fw")"
        printf '      "ssh_user": %s,\n' "$(json_string_or_null "$user")"
        printf '      "notes": %s,\n' "$(json_string_or_null "$notes")"
        printf '      "preflight": {\n'
        printf '        "status": %s,\n' "$(json_string_or_null "$preflight_status")"
        printf '        "ready": %s,\n' "$(json_bool "$preflight_ready")"
        printf '        "ssh_ok": %s,\n' "$(json_bool "$preflight_ssh_ok")"
        printf '        "api_ok": %s,\n' "$(json_bool "$preflight_api_ok")"
        printf '        "checked_at": %s,\n' "$(json_string_or_null "$preflight_checked_at")"
        printf '        "message": %s\n' "$(json_string_or_null "$preflight_message")"
        printf '      },\n'
        printf '      "deploy": {\n'
        printf '        "status": %s,\n' "$(json_string "$deploy_status")"
        printf '        "success": %s,\n' "$(json_bool "$( [ "$deploy_status" = "ok" ] && echo true || echo false )")"
        printf '        "exit_code": %s,\n' "$(json_number_or_null "$deploy_exit")"
        printf '        "started_at": %s,\n' "$(json_string_or_null "$started_at")"
        printf '        "finished_at": %s,\n' "$(json_string_or_null "$finished_at")"
        printf '        "duration_seconds": %s,\n' "$(json_number_or_null "$duration_seconds")"
        printf '        "duration_human": %s,\n' "$(json_string_or_null "$duration_h")"
        printf '        "attempts": %s,\n' "$(json_number_or_null "$deploy_attempts")"
        printf '        "retry_attempts": %s,\n' "$(json_number_or_null "$deploy_retries")"
        printf '        "log_path": %s\n' "$(json_string_or_null "$log_path")"
        printf '      },\n'
        printf '      "verify": {\n'
        printf '        "status": %s,\n' "$(json_string_or_null "$verify_status")"
        printf '        "success": %s,\n' "$(json_bool "$verify_success")"
        printf '        "api_ok": %s,\n' "$(json_bool "$verify_api_ok")"
        printf '        "health_ok": %s,\n' "$(json_bool "$verify_health_ok")"
        printf '        "health_status": %s,\n' "$(json_string_or_null "$verify_health_status")"
        printf '        "miner_state": %s,\n' "$(json_string_or_null "$verify_miner_state")"
        printf '        "pid": %s,\n' "$(json_string_or_null "$verify_pid")"
        printf '        "version": %s,\n' "$(json_string_or_null "$verify_version")"
        printf '        "checked_at": %s,\n' "$(json_string_or_null "$verify_checked_at")"
        printf '        "hashrate_ths": %s,\n' "$(json_number_or_null "$verify_hashrate_ths")"
        printf '        "temp_c": %s,\n' "$(json_number_or_null "$verify_temp_c")"
        printf '        "max_temp_c": %s,\n' "$(json_number_or_null "$verify_max_temp_c")"
        printf '        "fan_rpm": %s,\n' "$(json_number_or_null "$verify_fan_rpm")"
        printf '        "fan_pwm": %s,\n' "$(json_number_or_null "$verify_fan_pwm")"
        printf '        "pool_status": %s,\n' "$(json_string_or_null "$verify_pool_status")"
        printf '        "error": %s\n' "$(json_string_or_null "$verify_error")"
        printf '      }\n'
        printf '    }'
    done <<< "$RESULTS"

    printf '\n  ]\n'
    printf '}\n'
}

write_audit_artifact() {
    local audit_dir audit_json
    audit_dir="$(dirname "$AUDIT_FILE")"
    mkdir -p "$audit_dir"
    audit_json="$(build_json_report)"
    printf '%s\n' "$audit_json" > "$AUDIT_FILE"
    printf '%s\n' "$audit_json"
}

main() {
    local miners_raw filtered target_line resume_pending resume_name_set
    local -a TARGET_LINES=()

    BUILD_STATUS="skipped"
    BUILD_STARTED_AT=""
    BUILD_FINISHED_AT=""
    BUILD_DURATION_SECONDS=""
    BUILD_LOG_PATH=""
    RESULTS=""
    VERIFY_RESULTS=""
    TARGET_COUNT=0
    DEPLOY_COUNT=0
    SUCCESS_COUNT=0
    FAIL_COUNT=0
    SKIPPED_COUNT=0
    RETRY_ATTEMPT_COUNT=0
    PRECHECK_RESULTS=""
    PRECHECK_OK_COUNT=0
    PRECHECK_ISSUE_COUNT=0
    VERIFY_RUNNING_COUNT=0
    VERIFY_NOT_RUNNING_COUNT=0
    VERIFY_UNREACHABLE_COUNT=0
    VERIFY_HEALTHY_COUNT=0
    VERIFY_STANDBY_COUNT=0
    VERIFY_HEALTH_ISSUE_COUNT=0
    STOPPED_EARLY=false
    STOP_REASON=""
    KEEP_TMPDIR=false
    SKIP_BUILD_REQUESTED="$SKIP_BUILD"

    miners_raw=$(parse_miners "$MINERS_TOML")
    filtered=""
    while IFS='|' read -r name ip model fw notes user; do
        [ -z "$name" ] && continue

        local include=false
        if [ "$SELECTION_REQUESTED" = false ] && [ -n "$RESUME_FROM_AUDIT" ]; then
            include=true
        fi
        if [ "$DEPLOY_ALL" = true ]; then
            include=true
        fi
        if [ -n "$FILTER_MINERS" ]; then
            IFS=',' read -ra filter_list <<< "$FILTER_MINERS"
            for f in "${filter_list[@]}"; do
                if [ "$name" = "$f" ]; then
                    include=true
                    break
                fi
            done
        fi
        if [ -n "$FIRMWARE_FILTER" ] && [ "$fw" = "$FIRMWARE_FILTER" ]; then
            include=true
        fi

        if [ "$include" = true ]; then
            filtered+="${name}|${ip}|${model}|${fw}|${notes}|${user}"$'\n'
        fi
    done <<< "$miners_raw"
    filtered="$(echo "$filtered" | sed '/^$/d')"

    if [ -n "$RESUME_FROM_AUDIT" ]; then
        resume_pending="$(parse_audit_pending_miners "$RESUME_FROM_AUDIT")"
        resume_pending="$(echo "$resume_pending" | sed '/^$/d')"
        if [ -z "$resume_pending" ]; then
            echo "No unfinished miners found in audit artifact: $RESUME_FROM_AUDIT"
            exit 1
        fi

        resume_name_set="$(build_name_set <<< "$resume_pending")"
        local resumed_filtered=""
        while IFS='|' read -r name ip model fw notes user; do
            [ -z "$name" ] && continue
            if name_in_set "$name" "$resume_name_set"; then
                resumed_filtered+="${name}|${ip}|${model}|${fw}|${notes}|${user}"$'\n'
            fi
        done <<< "$filtered"
        filtered="$(echo "$resumed_filtered" | sed '/^$/d')"
    fi

    if [ -z "$filtered" ]; then
        if [ -n "$RESUME_FROM_AUDIT" ]; then
            echo "No miners match the selection criteria after applying --resume-from-audit."
        else
            echo "No miners match the selection criteria."
        fi
        exit 1
    fi

    mapfile -t TARGET_LINES <<< "$filtered"
    TARGET_COUNT=${#TARGET_LINES[@]}

    TMPDIR_PATH=$(mktemp -d)
    trap 'if [ "$KEEP_TMPDIR" = true ]; then console_printf "\n${YELLOW}Deploy logs saved in: %s${RESET}\n" "$TMPDIR_PATH"; else rm -rf "$TMPDIR_PATH"; fi' EXIT

    console_printf "\n${BOLD}DCENTos Fleet Deploy${RESET}\n"
    console_printf "${DIM}Config: %s${RESET}\n" "$MINERS_TOML"
    console_printf "${DIM}Mode:   %s${RESET}" "$DEPLOY_MODE"
    [ "$DEPLOY_MODE" = "parallel" ] && console_printf " (max %d)" "$MAX_PARALLEL"
    console_printf "\n"
    [ "$BATCH_SIZE" -gt 0 ] && console_printf "${DIM}Batch:  %d targets${RESET}\n" "$BATCH_SIZE"
    [ "$STOP_ON_FAILURE" = true ] && console_printf "${DIM}Gate:   stop on failed batch${RESET}\n"
    [ "$RETRY_FAILED" -gt 0 ] && console_printf "${DIM}Retry:  %d extra attempt(s) per failed deploy${RESET}\n" "$RETRY_FAILED"
    [ -n "$RESUME_FROM_AUDIT" ] && console_printf "${DIM}Resume: %s${RESET}\n" "$RESUME_FROM_AUDIT"
    console_printf "${DIM}Build:  %s${RESET}\n" "$([ "$SKIP_BUILD" = true ] && echo 'skip (--skip-build)' || echo 'yes')"
    [ "$VERIFY" = true ] && console_printf "${DIM}Verify: yes${RESET}\n"
    console_printf "${DIM}Audit:  %s${RESET}\n\n" "$AUDIT_FILE"

    console_printf "${BOLD}Targets (%d miners):${RESET}\n" "$TARGET_COUNT"
    for target_line in "${TARGET_LINES[@]}"; do
        IFS='|' read -r name ip model fw notes user <<< "$target_line"
        [ -z "$name" ] && continue
        console_printf "  ${CYAN}%-12s${RESET} %s (%s, %s)\n" "$name" "$ip" "$model" "$fw"
    done
    console_printf "\n"

    if [ "$PREFLIGHT_ENABLED" = true ]; then
        console_printf "${BOLD}=== Preflight ===${RESET}\n"
        for target_line in "${TARGET_LINES[@]}"; do
            local name ip model fw notes user pfresult pfstatus pfssh pfapi pfchecked pfmessage
            IFS='|' read -r name ip model fw notes user <<< "$target_line"
            [ -z "$name" ] && continue
            [ -z "$user" ] && user="root"

            console_printf "  Checking ${CYAN}%s${RESET} (%s)..." "$name" "$ip"
            pfresult="$(preflight_one "$ip" "$user")"
            pfstatus="$(echo "$pfresult" | cut -d'|' -f1)"
            pfssh="$(echo "$pfresult" | cut -d'|' -f2)"
            pfapi="$(echo "$pfresult" | cut -d'|' -f3)"
            pfchecked="$(echo "$pfresult" | cut -d'|' -f4)"
            pfmessage="$(echo "$pfresult" | cut -d'|' -f5-)"
            PRECHECK_RESULTS+="${name}|${pfstatus}|${pfssh}|${pfapi}|${pfchecked}|${pfmessage}"$'\n'

            if [ "$pfstatus" = "ready" ]; then
                PRECHECK_OK_COUNT=$((PRECHECK_OK_COUNT + 1))
                console_printf " ${GREEN}READY${RESET}\n"
            else
                PRECHECK_ISSUE_COUNT=$((PRECHECK_ISSUE_COUNT + 1))
                console_printf " ${YELLOW}%s${RESET} (%s)\n" "${pfstatus^^}" "$pfmessage"
            fi
        done
        console_printf "\n"

        if [ "$PREFLIGHT_STRICT" = true ] && [ "$PRECHECK_ISSUE_COUNT" -gt 0 ]; then
            STOPPED_EARLY=true
            STOP_REASON="preflight_failed"
            RUN_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
            console_printf "${RED}Preflight strict mode failed — aborting before deploy.${RESET}\n"
            local preflight_json
            preflight_json="$(write_audit_artifact)"
            if [ "$OUTPUT_FORMAT" = "json" ]; then
                printf '%s\n' "$preflight_json"
            fi
            exit 1
        fi
    fi

    if [ "$DRY_RUN" = true ]; then
        for target_line in "${TARGET_LINES[@]}"; do
            IFS='|' read -r name ip model fw notes user <<< "$target_line"
            [ -z "$name" ] && continue
            [ -z "$user" ] && user="root"
            RESULTS+="${name}|${ip}|${model}|${fw}|${notes}|${user}|planned||0|0s|${RUN_STARTED_AT}|${RUN_STARTED_AT}|0|0|"$'\n'
        done
        RUN_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
        local dry_json
        dry_json="$(write_audit_artifact)"
        console_printf "${YELLOW}Dry run - no changes made.${RESET}\n\n"
        if [ "$OUTPUT_FORMAT" = "json" ]; then
            printf '%s\n' "$dry_json"
        fi
        exit 0
    fi

    if [ "$SKIP_BUILD" = false ] && [ "$TARGET_COUNT" -gt 1 ]; then
        local build_start_epoch build_end_epoch workspace_dir target
        BUILD_STATUS="running"
        BUILD_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
        BUILD_LOG_PATH="$TMPDIR_PATH/build.log"
        build_start_epoch="$(date +%s)"

        console_printf "${BOLD}=== Building dcentrald (once for all targets) ===${RESET}\n"
        workspace_dir="$PROJECT_DIR/dcentrald"
        target="armv7-unknown-linux-musleabihf"
        if (cd "$workspace_dir" && cargo build --release --target "$target" >"$BUILD_LOG_PATH" 2>&1); then
            BUILD_STATUS="ok"
            console_printf "${GREEN}Build complete.${RESET}\n\n"
            SKIP_BUILD=true
        else
            BUILD_STATUS="failed"
            KEEP_TMPDIR=true
            console_printf "${RED}Build failed. Aborting fleet deploy.${RESET}\n"
            tail -5 "$BUILD_LOG_PATH" 2>/dev/null | sed 's/^/  /' >&2
            build_end_epoch="$(date +%s)"
            BUILD_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
            BUILD_DURATION_SECONDS=$((build_end_epoch - build_start_epoch))
            RUN_FINISHED_AT="$BUILD_FINISHED_AT"
            local failed_json
            failed_json="$(write_audit_artifact)"
            if [ "$OUTPUT_FORMAT" = "json" ]; then
                printf '%s\n' "$failed_json"
            fi
            exit 1
        fi

        build_end_epoch="$(date +%s)"
        BUILD_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
        BUILD_DURATION_SECONDS=$((build_end_epoch - build_start_epoch))
        BUILD_LOG_PATH=""
    fi

    console_printf "${BOLD}=== Deploying ===${RESET}\n"

    local effective_batch_size batch_start batch_end batch_number total_batches batch_results batch_failed batch_verify_failed idx
    effective_batch_size="$BATCH_SIZE"
    if [ "$effective_batch_size" -le 0 ] || [ "$effective_batch_size" -gt "$TARGET_COUNT" ]; then
        effective_batch_size="$TARGET_COUNT"
    fi
    total_batches=$(( (TARGET_COUNT + effective_batch_size - 1) / effective_batch_size ))
    batch_start=0
    batch_number=0

    while [ "$batch_start" -lt "$TARGET_COUNT" ]; do
        batch_end=$((batch_start + effective_batch_size))
        [ "$batch_end" -gt "$TARGET_COUNT" ] && batch_end="$TARGET_COUNT"
        batch_number=$((batch_number + 1))
        batch_results=""
        batch_failed=false
        batch_verify_failed=false

        if [ "$total_batches" -gt 1 ]; then
            console_printf "\n${BOLD}--- Batch %d/%d (%d targets) ---${RESET}\n" "$batch_number" "$total_batches" "$((batch_end - batch_start))"
        fi

        if [ "$DEPLOY_MODE" = "sequential" ]; then
            for ((idx=batch_start; idx<batch_end; idx++)); do
                local name ip model fw notes user log_file result deploy_status deploy_exit duration_seconds duration_h started_at finished_at deploy_attempts deploy_retries stored_log
                target_line="${TARGET_LINES[$idx]}"
                IFS='|' read -r name ip model fw notes user <<< "$target_line"
                [ -z "$name" ] && continue
                [ -z "$user" ] && user="root"
                DEPLOY_COUNT=$((DEPLOY_COUNT + 1))

                console_printf "\n[%d/%d] ${CYAN}%s${RESET} (%s)...\n" "$DEPLOY_COUNT" "$TARGET_COUNT" "$name" "$ip"

                log_file="$TMPDIR_PATH/${name}.log"
                result=$(deploy_with_retries "$ip" "$log_file")
                deploy_status=$(echo "$result" | cut -d'|' -f1)
                deploy_exit=$(echo "$result" | cut -d'|' -f2)
                duration_seconds=$(echo "$result" | cut -d'|' -f3)
                duration_h=$(echo "$result" | cut -d'|' -f4)
                started_at=$(echo "$result" | cut -d'|' -f5)
                finished_at=$(echo "$result" | cut -d'|' -f6)
                deploy_attempts=$(echo "$result" | cut -d'|' -f7)
                deploy_retries=$(echo "$result" | cut -d'|' -f8)
                stored_log=""
                RETRY_ATTEMPT_COUNT=$((RETRY_ATTEMPT_COUNT + deploy_retries))

                if [ "$deploy_status" = "ok" ]; then
                    if [ "$deploy_retries" -gt 0 ]; then
                        console_printf "  ${GREEN}SUCCESS${RESET} (%s, attempts=%s)\n" "$duration_h" "$deploy_attempts"
                    else
                        console_printf "  ${GREEN}SUCCESS${RESET} (%s)\n" "$duration_h"
                    fi
                    SUCCESS_COUNT=$((SUCCESS_COUNT + 1))
                else
                    if [ "$deploy_retries" -gt 0 ]; then
                        console_printf "  ${RED}FAILED${RESET} (%s, attempts=%s) - see %s\n" "$duration_h" "$deploy_attempts" "$log_file"
                    else
                        console_printf "  ${RED}FAILED${RESET} (%s) - see %s\n" "$duration_h" "$log_file"
                    fi
                    tail -5 "$log_file" 2>/dev/null | sed 's/^/    /' >&2
                    FAIL_COUNT=$((FAIL_COUNT + 1))
                    KEEP_TMPDIR=true
                    stored_log="$log_file"
                    batch_failed=true
                fi

                result="${name}|${ip}|${model}|${fw}|${notes}|${user}|${deploy_status}|${deploy_exit}|${duration_seconds}|${duration_h}|${started_at}|${finished_at}|${deploy_attempts}|${deploy_retries}|${stored_log}"
                RESULTS+="${result}"$'\n'
                batch_results+="${result}"$'\n'
            done
        else
            declare -A pids=()
            local running=0 pname

            for ((idx=batch_start; idx<batch_end; idx++)); do
                local name ip model fw notes user log_file result_file
                target_line="${TARGET_LINES[$idx]}"
                IFS='|' read -r name ip model fw notes user <<< "$target_line"
                [ -z "$name" ] && continue
                [ -z "$user" ] && user="root"
                DEPLOY_COUNT=$((DEPLOY_COUNT + 1))

                while [ "$running" -ge "$MAX_PARALLEL" ]; do
                    for pname in "${!pids[@]}"; do
                        local pid
                        pid="${pids[$pname]}"
                        if ! kill -0 "$pid" 2>/dev/null; then
                            wait "$pid" 2>/dev/null || true
                            running=$((running - 1))
                            unset "pids[$pname]"
                            break
                        fi
                    done
                    [ "$running" -ge "$MAX_PARALLEL" ] && sleep 1
                done

                console_printf "  Starting ${CYAN}%s${RESET} (%s)...\n" "$name" "$ip"

                log_file="$TMPDIR_PATH/${name}.log"
                result_file="$TMPDIR_PATH/${name}.result"
                (
                    local result deploy_status deploy_exit duration_seconds duration_h started_at finished_at deploy_attempts deploy_retries stored_log
                    result=$(deploy_with_retries "$ip" "$log_file")
                    deploy_status=$(echo "$result" | cut -d'|' -f1)
                    deploy_exit=$(echo "$result" | cut -d'|' -f2)
                    duration_seconds=$(echo "$result" | cut -d'|' -f3)
                    duration_h=$(echo "$result" | cut -d'|' -f4)
                    started_at=$(echo "$result" | cut -d'|' -f5)
                    finished_at=$(echo "$result" | cut -d'|' -f6)
                    deploy_attempts=$(echo "$result" | cut -d'|' -f7)
                    deploy_retries=$(echo "$result" | cut -d'|' -f8)
                    stored_log=""
                    if [ "$deploy_status" != "ok" ]; then
                        stored_log="$log_file"
                    fi
                    echo "${name}|${ip}|${model}|${fw}|${notes}|${user}|${deploy_status}|${deploy_exit}|${duration_seconds}|${duration_h}|${started_at}|${finished_at}|${deploy_attempts}|${deploy_retries}|${stored_log}" > "$result_file"
                ) &
                pids["$name"]=$!
                running=$((running + 1))
            done

            console_printf "\n  Waiting for batch deploys...\n"
            for pname in "${!pids[@]}"; do
                wait "${pids[$pname]}" 2>/dev/null || true
            done

            for ((idx=batch_start; idx<batch_end; idx++)); do
                local name ip model fw notes user result_file line deploy_status log_path deploy_retries deploy_attempts
                target_line="${TARGET_LINES[$idx]}"
                IFS='|' read -r name ip model fw notes user <<< "$target_line"
                [ -z "$name" ] && continue
                [ -z "$user" ] && user="root"
                result_file="$TMPDIR_PATH/${name}.result"
                log_path=""
                if [ -f "$result_file" ]; then
                    line=$(<"$result_file")
                    deploy_status=$(echo "$line" | cut -d'|' -f7)
                    deploy_attempts=$(echo "$line" | cut -d'|' -f13)
                    deploy_retries=$(echo "$line" | cut -d'|' -f14)
                    log_path=$(echo "$line" | cut -d'|' -f15)
                    RETRY_ATTEMPT_COUNT=$((RETRY_ATTEMPT_COUNT + deploy_retries))
                    if [ "$deploy_status" = "ok" ]; then
                        SUCCESS_COUNT=$((SUCCESS_COUNT + 1))
                    else
                        FAIL_COUNT=$((FAIL_COUNT + 1))
                        KEEP_TMPDIR=true
                        batch_failed=true
                    fi
                    RESULTS+="${line}"$'\n'
                    batch_results+="${line}"$'\n'
                else
                    FAIL_COUNT=$((FAIL_COUNT + 1))
                    KEEP_TMPDIR=true
                    batch_failed=true
                    line="${name}|${ip}|${model}|${fw}|${notes}|${user}|failed|1|||||1|0|"
                    RESULTS+="${line}"$'\n'
                    batch_results+="${line}"$'\n'
                fi
                if [ -n "$log_path" ]; then
                    if [ -n "$deploy_attempts" ] && [ "$deploy_attempts" -gt 1 ]; then
                        console_printf "  ${RED}%s failed${RESET} (attempts=%s) - see %s\n" "$name" "$deploy_attempts" "$log_path"
                    else
                        console_printf "  ${RED}%s failed${RESET} - see %s\n" "$name" "$log_path"
                    fi
                fi
            done
        fi

        if [ "$VERIFY" = true ]; then
            console_printf "\n${BOLD}=== Verifying Batch %d/%d ===${RESET}\n" "$batch_number" "$total_batches"
            sleep 3
            if ! verify_batch "$batch_results"; then
                batch_verify_failed=true
            fi
            VERIFY_RESULTS="$(echo "$VERIFY_RESULTS" | sed '/^$/d')"
        fi

        batch_start="$batch_end"
        if [ "$STOP_ON_FAILURE" = true ] && { [ "$batch_failed" = true ] || [ "$batch_verify_failed" = true ]; }; then
            STOPPED_EARLY=true
            if [ "$batch_verify_failed" = true ]; then
                STOP_REASON="batch_${batch_number}_verification_failed"
            else
                STOP_REASON="batch_${batch_number}_deploy_failed"
            fi
            console_printf "\n${YELLOW}Stopping rollout before next batch: %s${RESET}\n" "$STOP_REASON"
            break
        fi
    done

    RESULTS="$(echo "$RESULTS" | sed '/^$/d')"
    if [ "$STOPPED_EARLY" = true ] && [ "$batch_start" -lt "$TARGET_COUNT" ]; then
        for ((idx=batch_start; idx<TARGET_COUNT; idx++)); do
            local name ip model fw notes user
            target_line="${TARGET_LINES[$idx]}"
            IFS='|' read -r name ip model fw notes user <<< "$target_line"
            [ -z "$name" ] && continue
            [ -z "$user" ] && user="root"
            RESULTS+="${name}|${ip}|${model}|${fw}|${notes}|${user}|skipped||||||0|0|"$'\n'
            SKIPPED_COUNT=$((SKIPPED_COUNT + 1))
        done
        RESULTS="$(echo "$RESULTS" | sed '/^$/d')"
    fi

    RUN_FINISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    console_printf "\n${BOLD}═══════════════════════════════════════════════════════════════${RESET}\n"
    console_printf "${BOLD}Fleet Deploy Summary${RESET}  -  $(date '+%Y-%m-%d %H:%M:%S')\n"
    console_printf "${BOLD}═══════════════════════════════════════════════════════════════${RESET}\n\n"

    console_printf "${BOLD}%-14s %-17s %-10s %-10s${RESET}" "NAME" "IP" "DEPLOY" "DURATION"
    [ "$VERIFY" = true ] && console_printf "${BOLD} %-12s${RESET}" "HEALTH"
    console_printf "\n"
    console_printf "%-14s %-17s %-10s %-10s" "──────────────" "─────────────────" "──────────" "──────────"
    [ "$VERIFY" = true ] && console_printf " %-12s" "────────────"
    console_printf "\n"

    while IFS='|' read -r name ip model fw notes user deploy_status deploy_exit duration_seconds duration_h started_at finished_at deploy_attempts deploy_retries log_path; do
        local status_colored verify_line verify_status verify_health_status
        [ -z "$name" ] && continue
        case "$deploy_status" in
            ok)      status_colored="${GREEN}OK${RESET}        " ;;
            planned) status_colored="${YELLOW}PLANNED${RESET}   " ;;
            skipped) status_colored="${YELLOW}SKIPPED${RESET}   " ;;
            *)       status_colored="${RED}FAILED${RESET}    " ;;
        esac

        console_printf "%-14s %-17s ${status_colored} %-10s" "$name" "$ip" "$duration_h"
        if [ -n "$deploy_attempts" ] && [ "$deploy_attempts" -gt 1 ]; then
            console_printf " %s" "(x${deploy_attempts})"
        fi

        if [ "$VERIFY" = true ]; then
            verify_line="$(echo "$VERIFY_RESULTS" | grep "^${name}|" | head -1 || true)"
            verify_status="$(echo "$verify_line" | cut -d'|' -f2)"
            verify_health_status="$(echo "$verify_line" | cut -d'|' -f7)"
            case "$verify_status:$verify_health_status" in
                RUNNING:healthy)      console_printf " ${GREEN}%-12s${RESET}" "HEALTHY" ;;
                RUNNING:standby)      console_printf " ${YELLOW}%-12s${RESET}" "STANDBY" ;;
                RUNNING:*)            console_printf " ${YELLOW}%-12s${RESET}" "${verify_health_status^^}" ;;
                NOT_RUNNING:*)        console_printf " ${RED}%-12s${RESET}" "NOT RUNNING" ;;
                *)                    console_printf " ${RED}%-12s${RESET}" "UNREACHABLE" ;;
            esac
        fi

        console_printf "\n"
    done <<< "$RESULTS"

    console_printf "\n${BOLD}Total:${RESET} %d targeted - " "$TARGET_COUNT"
    [ "$SUCCESS_COUNT" -gt 0 ] && console_printf "${GREEN}%d succeeded${RESET} " "$SUCCESS_COUNT"
    [ "$FAIL_COUNT" -gt 0 ] && console_printf "${RED}%d failed${RESET} " "$FAIL_COUNT"
    [ "$SKIPPED_COUNT" -gt 0 ] && console_printf "${YELLOW}%d skipped${RESET} " "$SKIPPED_COUNT"
    if [ "$VERIFY" = true ]; then
        console_printf "${GREEN}%d running${RESET} " "$VERIFY_RUNNING_COUNT"
        [ "$VERIFY_HEALTHY_COUNT" -gt 0 ] && console_printf "${GREEN}%d healthy${RESET} " "$VERIFY_HEALTHY_COUNT"
        [ "$VERIFY_STANDBY_COUNT" -gt 0 ] && console_printf "${YELLOW}%d standby${RESET} " "$VERIFY_STANDBY_COUNT"
        [ "$VERIFY_HEALTH_ISSUE_COUNT" -gt 0 ] && console_printf "${RED}%d health issues${RESET} " "$VERIFY_HEALTH_ISSUE_COUNT"
    fi
    [ "$STOPPED_EARLY" = true ] && console_printf "${YELLOW}stopped early (%s)${RESET} " "$STOP_REASON"
    console_printf "\n${DIM}Audit artifact: %s${RESET}\n\n" "$AUDIT_FILE"

    local final_json
    final_json="$(write_audit_artifact)"
    if [ "$OUTPUT_FORMAT" = "json" ]; then
        printf '%s\n' "$final_json"
    fi

    if [ "$FAIL_COUNT" -gt 0 ] || [ "$STOPPED_EARLY" = true ]; then
        exit 1
    fi
    exit 0
}

main
