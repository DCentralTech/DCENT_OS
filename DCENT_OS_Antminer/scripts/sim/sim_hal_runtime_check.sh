#!/usr/bin/env bash
# Run one full-daemon sim proof inside an already-created private network namespace.

set -euo pipefail
umask 077

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)
MODEL=
MODE=
BINARY=
EXPECTED_BINARY_SHA256=

usage() {
    echo "usage: $0 --model MODEL --mode wsl-private|virtme-guest --binary-path PATH --expected-binary-sha256 SHA256" >&2
    exit 2
}
die() { echo "ERROR: $*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) MODEL=${2:-}; shift 2 ;;
        --mode) MODE=${2:-}; shift 2 ;;
        --binary-path) BINARY=${2:-}; shift 2 ;;
        --expected-binary-sha256) EXPECTED_BINARY_SHA256=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done

[ -n "$MODEL" ] && [ -n "$MODE" ] && [ -n "$BINARY" ] \
    && [ -n "$EXPECTED_BINARY_SHA256" ] || usage
case "$MODEL" in
    s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro) ;;
    *) die "$MODEL has no integrated T2 runtime proof" ;;
esac
case "$MODE" in
    wsl-private|virtme-guest) ;;
    *) die "unsupported runtime isolation mode: $MODE" ;;
esac
case "$EXPECTED_BINARY_SHA256" in
    *[!0-9a-f]*|'') die "expected binary SHA-256 must be 64 lowercase hexadecimal characters" ;;
esac
[ "${#EXPECTED_BINARY_SHA256}" -eq 64 ] || die "expected binary SHA-256 must contain 64 characters"

for command in awk curl ip mktemp python3 realpath seq sha256sum; do
    command -v "$command" >/dev/null 2>&1 || die "$command is required"
done
[ "$(id -u)" = 0 ] || die "root is required for the private simulator runtime"

[ -x "$BINARY" ] || die "sim-hal dcentrald binary not found: $BINARY"
BINARY=$(realpath -e "$BINARY")
expected_binary_root="$PROJECT_DIR/dcentrald/target/sim-hal-runner/"
case "$BINARY" in
    "$expected_binary_root"*/debug/dcentrald) ;;
    *) die "sim-hal binary is outside the dedicated runner target: $BINARY" ;;
esac
actual_sha256=$(sha256sum "$BINARY" | awk '{print $1}')
test "$actual_sha256" = "$EXPECTED_BINARY_SHA256" \
    || die "sim-hal binary SHA-256 mismatch: expected $EXPECTED_BINARY_SHA256, got $actual_sha256"

if [ "$MODE" = wsl-private ]; then
    command -v mount >/dev/null 2>&1 || die "mount is required"
    mount --make-rprivate /
    mount -t tmpfs -o mode=0755,nosuid tmpfs /dev
    mknod -m 666 /dev/null c 1 3
    mknod -m 666 /dev/zero c 1 5
    mknod -m 444 /dev/urandom c 1 9
    mkdir -m 755 /dev/pts /dev/shm
    ln -s /proc/self/fd /dev/fd
    ln -s /proc/self/fd/0 /dev/stdin
    ln -s /proc/self/fd/1 /dev/stdout
    ln -s /proc/self/fd/2 /dev/stderr
else
    # This is confined to the disposable virtme guest's device namespace.
    rm -f /dev/uio0 /dev/ttyO1 /dev/ttyS1 /dev/uart_trans /dev/uart_trans0
fi
for path in /dev/uio0 /dev/ttyO1 /dev/ttyS1 /dev/uart_trans /dev/uart_trans0; do
    test ! -e "$path" || die "real-hardware signature survived isolation: $path"
done

ip link set lo up
python3 - <<'PY'
import socket

for port in (8080, 3000):
    sock = socket.socket()
    try:
        sock.bind(("127.0.0.1", port))
    finally:
        sock.close()
PY

run_dir=$(mktemp -d "${TMPDIR:-/tmp}/dcent-sim-${MODEL}.XXXXXX")
daemon_log="$run_dir/dcentrald.log"
status_json="$run_dir/status.json"
rust_mcp_json="$run_dir/rust-mcp.json"
rootfs_mcp_log="$run_dir/rootfs-mcp.log"
rootfs_mcp_json="$run_dir/rootfs-mcp.json"

process_is_live() {
    local pid state
    pid=$1
    [ -r "/proc/$pid/stat" ] || return 1
    state=$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null) || return 1
    case "$state" in
        Z|X) return 1 ;;
    esac
    kill -0 "$pid" 2>/dev/null
}

bounded_stop() {
    local label pid
    pid=$1
    label=$2
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 50); do
        process_is_live "$pid" || break
        sleep 0.1
    done
    if process_is_live "$pid"; then
        echo "WARN: $label ignored SIGTERM; sending SIGKILL" >&2
        kill -KILL "$pid" 2>/dev/null || true
        for _ in $(seq 1 20); do
            process_is_live "$pid" || break
            sleep 0.1
        done
    fi
    process_is_live "$pid" && return 1
    wait "$pid" 2>/dev/null || true
}

cleanup() {
    rc=$?
    trap - EXIT INT TERM
    set +e
    if [ -n "${rootfs_mcp_pid:-}" ]; then
        bounded_stop "$rootfs_mcp_pid" "rootfs MCP" \
            || echo "ERROR: rootfs MCP process did not stop" >&2
    fi
    if [ -n "${daemon_pid:-}" ]; then
        bounded_stop "$daemon_pid" dcentrald \
            || echo "ERROR: dcentrald process did not stop" >&2
    fi
    rm -f -- "$daemon_log" "$status_json" "$rust_mcp_json" \
        "$rootfs_mcp_log" "$rootfs_mcp_json"
    rmdir -- "$run_dir" 2>/dev/null
    exit "$rc"
}
trap cleanup EXIT INT TERM

cd "$PROJECT_DIR"
export DCENT_SIM_HAL=1
export DCENT_CONFIRM_SIM_HAL_IS_NOT_REAL_HARDWARE=1
export DCENT_SIM_MODEL="$MODEL"
export RUST_LOG=info
export NO_COLOR=1

"$BINARY" --config "$run_dir/no-config.toml" >"$daemon_log" 2>&1 &
daemon_pid=$!
ready=0
for _ in $(seq 1 200); do
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        cat "$daemon_log"
        die "dcentrald exited before REST readiness"
    fi
    if curl --connect-timeout 1 --max-time 2 -fsS \
        http://127.0.0.1:8080/api/status >"$status_json" 2>/dev/null; then
        ready=1
        break
    fi
    sleep 0.1
done
[ "$ready" = 1 ] || die "dcentrald REST readiness timed out"
python3 "$SCRIPT_DIR/validate_sim_runtime_evidence.py" status \
    --model "$MODEL" --path "$status_json"

curl --connect-timeout 1 --max-time 3 -fsS \
    -H 'Content-Type: application/json' \
    -H 'X-Dcentos-Dashboard-Proxy: 1' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"offline-proof","version":"1"}}}' \
    http://127.0.0.1:8080/mcp >"$rust_mcp_json"
python3 "$SCRIPT_DIR/validate_sim_runtime_evidence.py" rust-mcp \
    --path "$rust_mcp_json"

python3 "$PROJECT_DIR/br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py" \
    --bind 127.0.0.1 --port 3000 >"$rootfs_mcp_log" 2>&1 &
rootfs_mcp_pid=$!
rootfs_ready=0
for _ in $(seq 1 100); do
    if ! kill -0 "$rootfs_mcp_pid" 2>/dev/null; then
        cat "$rootfs_mcp_log"
        die "rootfs MCP server exited before readiness"
    fi
    if curl --connect-timeout 1 --max-time 2 -fsS \
        http://127.0.0.1:3000/mcp >"$rootfs_mcp_json" 2>/dev/null; then
        rootfs_ready=1
        break
    fi
    sleep 0.1
done
[ "$rootfs_ready" = 1 ] || die "rootfs MCP readiness timed out"
python3 "$SCRIPT_DIR/validate_sim_runtime_evidence.py" rootfs-mcp \
    --path "$rootfs_mcp_json"

bounded_stop "$rootfs_mcp_pid" "rootfs MCP" || die "rootfs MCP shutdown timed out"
rootfs_mcp_pid=
bounded_stop "$daemon_pid" dcentrald || die "dcentrald shutdown timed out"
daemon_pid=

python3 "$SCRIPT_DIR/validate_sim_runtime_evidence.py" daemon-log \
    --model "$MODEL" --path "$daemon_log"
cat "$daemon_log"
echo "OFFLINE_SIM_HAL_RUNTIME_OK mode=$MODE model=$MODEL binary_sha256=$actual_sha256"
