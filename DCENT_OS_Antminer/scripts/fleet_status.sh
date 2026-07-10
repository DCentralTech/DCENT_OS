#!/bin/bash
# DCENTos - Fleet Status Check
# D-Central Technologies, 2026

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

OUTPUT_FORMAT="table"
FILTER_MINERS=""
SSH_TIMEOUT=5
API_TIMEOUT=3
RUN_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
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

usage() {
    cat <<'USAGE'
DCENTos Fleet Status - D-Central Technologies

Usage:
  fleet_status.sh [OPTIONS]

Options:
  --json                  Output structured JSON object
  --miners NAME[,NAME]    Check only named miners (comma-separated)
  --timeout SECONDS       SSH connect timeout (default: 5)
  --config PATH           Path to miners.toml (auto-detected by default)
  --help                  Show this help

Examples:
  fleet_status.sh
  fleet_status.sh --json
  fleet_status.sh --miners s9-97,s9-36
  fleet_status.sh --timeout 3
USAGE
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --json)        OUTPUT_FORMAT="json"; shift ;;
        --miners)      FILTER_MINERS="$2"; shift 2 ;;
        --timeout)     SSH_TIMEOUT="$2"; shift 2 ;;
        --config)      MINERS_TOML="$2"; shift 2 ;;
        --help|-h)     usage ;;
        *)             echo "Unknown option: $1"; usage ;;
    esac
done

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

check_miner() {
    local name="$1" ip="$2" model="$3" expected_fw="$4" user="$5"
    local checked_at status reachable ssh_ok api_ok daemon api_source fw_version hashrate_num temp_num uptime error
    local hashrate_display temp_display

    checked_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    status="unreachable"
    reachable=false
    ssh_ok=false
    api_ok=false
    daemon="none"
    api_source="none"
    fw_version=""
    hashrate_num=""
    temp_num=""
    uptime=""
    error="ssh_unreachable"
    hashrate_display="-"
    temp_display="-"

    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" -o BatchMode=yes \
        "${user}@${ip}" "echo OK" >/dev/null 2>&1; then

        reachable=true
        ssh_ok=true
        error=""
        status="online"

        local ssh_info
        ssh_info=$(ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" -o BatchMode=yes \
            "${user}@${ip}" '
            if [ -f /etc/dcentos-version ]; then
                echo "FW=DCENTos $(cat /etc/dcentos-version)"
            elif [ -f /etc/bos_version ]; then
                echo "FW=BraiinsOS $(head -1 /etc/bos_version)"
            else
                echo "FW=unknown"
            fi
            echo "UPTIME=$(uptime -p 2>/dev/null || uptime | sed "s/.*up /up /;s/,.*load.*//")"
            BOSMINER=$(pidof bosminer 2>/dev/null || echo "")
            DCENTRALD=$(pidof dcentrald 2>/dev/null || echo "")
            if [ -n "$DCENTRALD" ]; then
                echo "DAEMON=dcentrald"
            elif [ -n "$BOSMINER" ]; then
                echo "DAEMON=bosminer"
            else
                echo "DAEMON=none"
            fi
        ' 2>/dev/null) || true

        fw_version=$(echo "$ssh_info" | grep '^FW=' | cut -d'=' -f2-)
        uptime=$(echo "$ssh_info" | grep '^UPTIME=' | cut -d'=' -f2-)
        daemon=$(echo "$ssh_info" | grep '^DAEMON=' | cut -d'=' -f2-)

        [ -z "$fw_version" ] && fw_version="unknown"
        [ -z "$uptime" ] && uptime="-"
        [ -z "$daemon" ] && daemon="none"

        if [ "$daemon" = "dcentrald" ]; then
            local api_resp
            api_resp=$(curl -s --connect-timeout "$API_TIMEOUT" "http://${ip}:8080/api/status" 2>/dev/null) || true
            if [ -n "$api_resp" ]; then
                api_ok=true
                api_source="dcentrald_rest"
                hashrate_num=$(echo "$api_resp" | grep -o '"hashrate_ths"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$') || true
                temp_num=$(echo "$api_resp" | grep -o '"chip_temp"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$') || true
            fi
            status="mining"
        elif [ "$daemon" = "bosminer" ]; then
            local cgm_resp
            cgm_resp=$(echo '{"command":"summary"}' | nc -w "$API_TIMEOUT" "$ip" 4028 2>/dev/null) || true
            if [ -n "$cgm_resp" ]; then
                api_ok=true
                api_source="cgminer_summary"
                local hashrate_ghs
                hashrate_ghs=$(echo "$cgm_resp" | grep -o '"GHS 5s"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$') || true
                if [ -n "$hashrate_ghs" ]; then
                    hashrate_num=$(awk "BEGIN { printf \"%.2f\", $hashrate_ghs / 1000 }")
                fi
                temp_num=$(echo "$cgm_resp" | grep -o '"Temperature"[[:space:]]*:[[:space:]]*[0-9.]*' | head -1 | grep -o '[0-9.]*$') || true
            fi
            status="mining"
        else
            status="idle"
        fi

        if [ -n "$hashrate_num" ]; then
            hashrate_display="${hashrate_num} TH/s"
        fi
        if [ -n "$temp_num" ]; then
            temp_display="${temp_num}C"
        fi
    fi

    echo "${checked_at}|${status}|${reachable}|${ssh_ok}|${api_ok}|${daemon}|${api_source}|${fw_version}|${hashrate_num}|${hashrate_display}|${temp_num}|${temp_display}|${uptime}|${error}"
}

main() {
    local miners_raw
    miners_raw=$(parse_miners "$MINERS_TOML")

    if [ -n "$FILTER_MINERS" ]; then
        local filtered=""
        IFS=',' read -ra filter_list <<< "$FILTER_MINERS"
        while IFS='|' read -r name rest; do
            local include=false
            for f in "${filter_list[@]}"; do
                if [ "$name" = "$f" ]; then
                    include=true
                    break
                fi
            done
            if [ "$include" = true ]; then
                filtered+="${name}|${rest}"$'\n'
            fi
        done <<< "$miners_raw"
        miners_raw="$(echo "$filtered" | sed '/^$/d')"
    fi

    if [ -z "$miners_raw" ]; then
        echo "No miners found in $MINERS_TOML"
        exit 1
    fi

    local total=0 reachable_count=0 online_count=0 mining_count=0 idle_count=0 unreachable_count=0
    local results=""

    while IFS='|' read -r name ip model fw notes user; do
        [ -z "$name" ] && continue
        [ -z "$user" ] && user="root"
        total=$((total + 1))

        if [ "$OUTPUT_FORMAT" = "table" ]; then
            printf "  Checking ${CYAN}%-10s${RESET} (%s)..." "$name" "$ip" >&2
        fi

        local result
        result=$(check_miner "$name" "$ip" "$model" "$fw" "$user")
        results+="${name}|${ip}|${model}|${fw}|${notes}|${user}|${result}"$'\n'

        local status reachable
        status=$(echo "$result" | cut -d'|' -f2)
        reachable=$(echo "$result" | cut -d'|' -f3)

        if [ "$reachable" = "true" ]; then
            reachable_count=$((reachable_count + 1))
            online_count=$((online_count + 1))
        fi

        case "$status" in
            mining)
                mining_count=$((mining_count + 1))
                [ "$OUTPUT_FORMAT" = "table" ] && printf " ${GREEN}mining${RESET}\n" >&2
                ;;
            idle)
                idle_count=$((idle_count + 1))
                [ "$OUTPUT_FORMAT" = "table" ] && printf " ${YELLOW}idle${RESET}\n" >&2
                ;;
            online)
                [ "$OUTPUT_FORMAT" = "table" ] && printf " ${GREEN}online${RESET}\n" >&2
                ;;
            unreachable)
                unreachable_count=$((unreachable_count + 1))
                [ "$OUTPUT_FORMAT" = "table" ] && printf " ${RED}unreachable${RESET}\n" >&2
                ;;
        esac
    done <<< "$miners_raw"

    results="$(echo "$results" | sed '/^$/d')"

    if [ "$OUTPUT_FORMAT" = "table" ]; then
        echo "" >&2
        printf "\n${BOLD}DCENTos Fleet Status${RESET}  -  $(date '+%Y-%m-%d %H:%M:%S')\n"
        printf "${DIM}Config: %s${RESET}\n\n" "$MINERS_TOML"

        printf "${BOLD}%-12s %-17s %-8s %-13s %-28s %-12s %-8s %s${RESET}\n" \
            "NAME" "IP" "MODEL" "STATUS" "FIRMWARE" "HASHRATE" "TEMP" "UPTIME"
        printf "%-12s %-17s %-8s %-13s %-28s %-12s %-8s %s\n" \
            "────────────" "─────────────────" "────────" "─────────────" "────────────────────────────" "────────────" "────────" "──────────────────────"

        while IFS='|' read -r name ip model expected_fw notes user checked_at status reachable ssh_ok api_ok daemon api_source fw_ver hashrate_num hashrate temp_num temp up error; do
            [ -z "$name" ] && continue
            local status_colored
            case "$status" in
                mining)      status_colored="${GREEN}mining${RESET}      " ;;
                idle)        status_colored="${YELLOW}idle${RESET}        " ;;
                online)      status_colored="${GREEN}online${RESET}      " ;;
                unreachable) status_colored="${RED}unreachable${RESET} " ;;
                *)           status_colored="$status" ;;
            esac
            printf "%-12s %-17s %-8s ${status_colored} %-28s %-12s %-8s %s\n" \
                "$name" "$ip" "$model" "$fw_ver" "$hashrate" "$temp" "$up"
        done <<< "$results"

        printf "\n${BOLD}Summary:${RESET} %d miners - " "$total"
        printf "${GREEN}%d reachable${RESET} " "$reachable_count"
        [ "$mining_count" -gt 0 ] && printf "${GREEN}%d mining${RESET} " "$mining_count"
        [ "$idle_count" -gt 0 ] && printf "${YELLOW}%d idle${RESET} " "$idle_count"
        [ "$unreachable_count" -gt 0 ] && printf "${RED}%d unreachable${RESET} " "$unreachable_count"
        printf "\n\n"
        exit 0
    fi

    printf '{\n'
    printf '  "generated_at": %s,\n' "$(json_string "$(date -u +"%Y-%m-%dT%H:%M:%SZ")")"
    printf '  "started_at": %s,\n' "$(json_string "$RUN_STARTED_AT")"
    printf '  "config_path": %s,\n' "$(json_string "$MINERS_TOML")"
    printf '  "options": {\n'
    printf '    "output": %s,\n' "$(json_string "$OUTPUT_FORMAT")"
    printf '    "miners_filter": %s,\n' "$(json_string_or_null "$FILTER_MINERS")"
    printf '    "ssh_timeout_seconds": %s,\n' "$SSH_TIMEOUT"
    printf '    "api_timeout_seconds": %s\n' "$API_TIMEOUT"
    printf '  },\n'
    printf '  "summary": {\n'
    printf '    "total": %d,\n' "$total"
    printf '    "reachable": %d,\n' "$reachable_count"
    printf '    "online": %d,\n' "$online_count"
    printf '    "mining": %d,\n' "$mining_count"
    printf '    "idle": %d,\n' "$idle_count"
    printf '    "unreachable": %d\n' "$unreachable_count"
    printf '  },\n'
    printf '  "miners": [\n'

    local first=true
    while IFS='|' read -r name ip model expected_fw notes user checked_at status reachable ssh_ok api_ok daemon api_source fw_ver hashrate_num hashrate temp_num temp up error; do
        [ -z "$name" ] && continue
        if [ "$first" = true ]; then
            first=false
        else
            printf ',\n'
        fi
        printf '    {\n'
        printf '      "name": %s,\n' "$(json_string "$name")"
        printf '      "ip": %s,\n' "$(json_string "$ip")"
        printf '      "model": %s,\n' "$(json_string_or_null "$model")"
        printf '      "expected_firmware": %s,\n' "$(json_string_or_null "$expected_fw")"
        printf '      "ssh_user": %s,\n' "$(json_string_or_null "$user")"
        printf '      "notes": %s,\n' "$(json_string_or_null "$notes")"
        printf '      "checked_at": %s,\n' "$(json_string "$checked_at")"
        printf '      "status": %s,\n' "$(json_string "$status")"
        printf '      "reachable": %s,\n' "$(json_bool "$reachable")"
        printf '      "ssh_ok": %s,\n' "$(json_bool "$ssh_ok")"
        printf '      "api_ok": %s,\n' "$(json_bool "$api_ok")"
        printf '      "daemon": %s,\n' "$(json_string "$daemon")"
        printf '      "api_source": %s,\n' "$(json_string "$api_source")"
        printf '      "firmware_detected": %s,\n' "$(json_string_or_null "$fw_ver")"
        printf '      "hashrate_ths": %s,\n' "$(json_number_or_null "$hashrate_num")"
        printf '      "hashrate_display": %s,\n' "$(json_string_or_null "$hashrate")"
        printf '      "temp_c": %s,\n' "$(json_number_or_null "$temp_num")"
        printf '      "temp_display": %s,\n' "$(json_string_or_null "$temp")"
        printf '      "uptime": %s,\n' "$(json_string_or_null "$up")"
        printf '      "error": %s\n' "$(json_string_or_null "$error")"
        printf '    }'
    done <<< "$results"

    printf '\n  ]\n'
    printf '}\n'
}

main
