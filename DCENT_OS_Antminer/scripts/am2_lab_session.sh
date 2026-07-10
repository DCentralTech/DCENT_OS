#!/bin/bash
# am2_lab_session.sh — Repeatable AM2 lab workflow helper

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
SUBCOMMAND="${1:-}"
shift || true

timestamp() {
    date -u +"%Y%m%dT%H%M%SZ"
}

usage() {
    echo "Usage: $(basename "$0") <subcommand> [args]"
    echo ""
    echo "Subcommands:"
    echo "  capture-baseline <miner_ip> [artifact_dir]"
    echo "  build-sd [build_sd_s19pro.sh args...]"
    echo "  runtime-validate <miner_ip> [--config FILE] [artifact_dir]"
    echo "  capture-rollback <miner_ip> [artifact_dir]"
    echo ""
    echo "This script is lab-only. It does not perform NAND install on AM2."
}

capture_common() {
    local miner_ip="$1"
    local artifact_dir="$2"
    mkdir -p "$artifact_dir"

    ssh $SSH_OPTS "root@$miner_ip" '
        echo "=== identity ==="
        uname -a
        cat /config/CONF_MINER_TYPE 2>/dev/null || true
        cat /config/CONF_HARDWARE_ID 2>/dev/null || true
        echo "=== boot ==="
        fw_printenv firmware upgrade_stage 2>/dev/null || true
        cat /proc/cmdline
        echo "=== storage ==="
        cat /proc/mtd
        echo "=== versions ==="
        cat /etc/bos_version 2>/dev/null || true
        cat /etc/dcentos-version 2>/dev/null || true
        echo "=== logs ==="
        grep -E "dsPIC|temperature|Share ACCEPTED|ERROR|WARN" /tmp/dcentrald.log 2>/dev/null | tail -100 || true
    ' > "$artifact_dir/ssh_snapshot.txt"

    if command -v curl >/dev/null 2>&1; then
        curl -fsS "http://$miner_ip:8080/api/status" > "$artifact_dir/api_status.json" 2>/dev/null || true
        curl -fsS "http://$miner_ip:8080/api/system/info" > "$artifact_dir/api_system_info.json" 2>/dev/null || true
    fi
}

case "$SUBCOMMAND" in
    capture-baseline)
        MINER_IP="${1:?capture-baseline requires <miner_ip>}"
        ARTIFACT_DIR="${2:-$SCRIPT_DIR/../docs/dev/$(timestamp)-am2-baseline}"
        echo "Capturing AM2 baseline to $ARTIFACT_DIR"
        capture_common "$MINER_IP" "$ARTIFACT_DIR"
        ;;

    build-sd)
        exec "$SCRIPT_DIR/build_sd_s19pro.sh" "$@"
        ;;

    runtime-validate)
        MINER_IP="${1:?runtime-validate requires <miner_ip>}"
        shift
        CONFIG_FILE=""
        ARTIFACT_DIR="$SCRIPT_DIR/../docs/dev/$(timestamp)-am2-runtime"
        while [ $# -gt 0 ]; do
            case "$1" in
                --config) CONFIG_FILE="$2"; shift 2 ;;
                *) ARTIFACT_DIR="$1"; shift ;;
            esac
        done
        if [ -n "$CONFIG_FILE" ]; then
            "$SCRIPT_DIR/dev_deploy.sh" "$MINER_IP" --config "$CONFIG_FILE" --verify
        else
            "$SCRIPT_DIR/dev_deploy.sh" "$MINER_IP" --verify
        fi
        capture_common "$MINER_IP" "$ARTIFACT_DIR"
        ;;

    capture-rollback)
        MINER_IP="${1:?capture-rollback requires <miner_ip>}"
        ARTIFACT_DIR="${2:-$SCRIPT_DIR/../docs/dev/$(timestamp)-am2-rollback}"
        echo "Capture rollback proof after removing SD / restoring NAND boot."
        capture_common "$MINER_IP" "$ARTIFACT_DIR"
        ;;

    *)
        usage
        exit 1
        ;;
esac
