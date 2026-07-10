#!/bin/sh
# DCENTos — Live Validation Preflight
#
# Verifies the local workstation and repo have what the next live validation
# session needs before any miner comes back online.

set -eu

TARGETS="s9-106 s9-39 s19pro-129 s21-135"
CHECK_NETWORK=false
STRICT_BINARIES=false

usage() {
    cat <<'EOF'
Usage: validation_preflight.sh [OPTIONS]

Options:
  --targets NAME[,NAME]   Target miners to validate (default: s9-106,s9-39,s19pro-129,s21-135)
  --network               Also check SSH and API reachability for each target
  --strict-binaries       Fail if expected release binaries are missing locally
  --help                  Show this help

Known targets:
  s9-106       S9 PIC16 baseline
  s9-39        203.0.113.39 field-reality S9
  s19pro-129   S19 Pro dsPIC validation target
  s21-135      S21 NoPic validation target
EOF
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --targets)
            TARGETS=$(printf '%s' "${2:?--targets requires a value}" | tr ',' ' ')
            shift 2
            ;;
        --network)
            CHECK_NETWORK=true
            shift
            ;;
        --strict-binaries)
            STRICT_BINARIES=true
            shift
            ;;
        --help|-h)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            ;;
    esac
done

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
PROJECT_DIR=$(dirname "$SCRIPT_DIR")
WORKSPACE_DIR="$PROJECT_DIR/dcentrald"

MINERS_TOML=""
for dir in "$PROJECT_DIR" "$(dirname "$PROJECT_DIR")" "$(dirname "$(dirname "$PROJECT_DIR")")"; do
    if [ -f "$dir/miners.toml" ]; then
        MINERS_TOML="$dir/miners.toml"
        break
    fi
done

if [ -z "$MINERS_TOML" ]; then
    echo "ERROR: miners.toml not found" >&2
    exit 1
fi

FAILURES=0
WARNINGS=0

pass() {
    echo "PASS: $1"
}

warn() {
    echo "WARN: $1"
    WARNINGS=$((WARNINGS + 1))
}

fail() {
    echo "FAIL: $1"
    FAILURES=$((FAILURES + 1))
}

require_command() {
    if command -v "$1" >/dev/null 2>&1 || command -v "$1.exe" >/dev/null 2>&1; then
        pass "host command '$1' available"
    else
        fail "host command '$1' missing"
    fi
}

require_file() {
    if [ -f "$1" ]; then
        pass "$2 present"
    else
        fail "$2 missing ($1)"
    fi
}

target_ip() {
    case "$1" in
        s9-106) printf '%s' '203.0.113.106' ;;
        s9-39) printf '%s' '203.0.113.39' ;;
        s19pro-129) printf '%s' '203.0.113.129' ;;
        s21-135) printf '%s' '203.0.113.135' ;;
        *) return 1 ;;
    esac
}

target_config() {
    case "$1" in
        s9-106) printf '%s' "$WORKSPACE_DIR/dcentrald.toml" ;;
        s9-39) printf '%s' "$WORKSPACE_DIR/dcentrald-s9-home-autotuner-203.0.113.39.toml" ;;
        s19pro-129) printf '%s' "$WORKSPACE_DIR/dcentrald-s19pro.toml" ;;
        s21-135) printf '%s' "$WORKSPACE_DIR/dcentrald_s21.toml" ;;
        *) return 1 ;;
    esac
}

target_triple() {
    case "$1" in
        s21-135) printf '%s' 'aarch64-unknown-linux-musl' ;;
        s9-106|s9-39|s19pro-129) printf '%s' 'armv7-unknown-linux-musleabihf' ;;
        *) return 1 ;;
    esac
}

check_miner_entry() {
    if grep -q "^\[miners\.$1\]" "$MINERS_TOML"; then
        pass "miners.toml entry '$1' present"
    else
        fail "miners.toml entry '$1' missing"
    fi
}

check_binary() {
    binary="$WORKSPACE_DIR/target/$1/release/dcentrald"
    if [ -f "$binary" ]; then
        pass "release binary present for $1"
    elif [ "$STRICT_BINARIES" = true ]; then
        fail "release binary missing for $1 ($binary)"
    else
        warn "release binary missing for $1 ($binary)"
    fi
}

check_network() {
    miner="$1"
    ip=$(target_ip "$miner") || return 1

    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -o BatchMode=yes "root@$ip" "echo OK" >/dev/null 2>&1; then
        pass "$miner SSH reachable"
    else
        warn "$miner SSH unreachable"
        return 0
    fi

    if curl -fsS --connect-timeout 3 "http://$ip/" >/dev/null 2>&1 || \
       curl -fsS --connect-timeout 3 "http://$ip:8080/api/status" >/dev/null 2>&1; then
        pass "$miner HTTP surface reachable"
    else
        warn "$miner HTTP surface unreachable"
    fi
}

echo "=== Host Tooling ==="
require_command cargo
require_command ssh
require_command scp
require_command curl

echo ""
echo "=== Shared Files ==="
require_file "$MINERS_TOML" "miners.toml"
require_file "$SCRIPT_DIR/dev_deploy.sh" "dev_deploy.sh"
require_file "$SCRIPT_DIR/fleet_status.sh" "fleet_status.sh"

for miner in $TARGETS; do
    echo ""
    echo "=== Target: $miner ==="
    check_miner_entry "$miner"
    cfg=$(target_config "$miner" 2>/dev/null || true)
    if [ -n "$cfg" ]; then
        require_file "$cfg" "$miner config"
    else
        fail "no config mapping defined for $miner"
    fi
    triple=$(target_triple "$miner" 2>/dev/null || true)
    if [ -n "$triple" ]; then
        check_binary "$triple"
    else
        fail "no target triple mapping defined for $miner"
    fi
    if [ "$CHECK_NETWORK" = true ]; then
        check_network "$miner"
    fi
done

echo ""
echo "=== Summary ==="
echo "Failures: $FAILURES"
echo "Warnings: $WARNINGS"

if [ "$FAILURES" -ne 0 ]; then
    exit 1
fi

exit 0
