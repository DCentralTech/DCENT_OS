#!/usr/bin/env bash
# Boot a disposable virtme/QEMU kernel and prove dcentrald's sim HAL runtime.

set -euo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)
MODEL=
KERNEL=${DCENT_VIRTME_KERNEL:-}
MEMORY=${DCENT_VIRTME_MEMORY:-1536M}
CPUS=${DCENT_VIRTME_CPUS:-2}
EVIDENCE_DIR=
NO_BUILD=0

usage() {
    cat <<'EOF'
Usage: virtme_sim_hal_runner.sh --model MODEL [options]

Options:
  --kernel PATH       virtme kernel (default: installed Ubuntu kernel)
  --memory SIZE       guest memory (default: 1536M)
  --cpus N            guest CPUs (default: 2)
  --evidence-dir DIR  retain the host-side VM transcript
  --no-build          use an existing target/debug/dcentrald binary
EOF
}

die() { echo "ERROR: $*" >&2; exit 1; }
shell_quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) MODEL=${2:-}; shift 2 ;;
        --kernel) KERNEL=${2:-}; shift 2 ;;
        --memory) MEMORY=${2:-}; shift 2 ;;
        --cpus) CPUS=${2:-}; shift 2 ;;
        --evidence-dir) EVIDENCE_DIR=${2:-}; shift 2 ;;
        --no-build) NO_BUILD=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done

[ -n "$MODEL" ] || { usage >&2; die "--model is required"; }
case "$MODEL" in
    s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro) ;;
    *) die "$MODEL has no integrated T2 runtime proof" ;;
esac
command -v vng >/dev/null 2>&1 || die "virtme-ng vng is required"
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v python3 >/dev/null 2>&1 || die "python3 is required"

if [ -z "$KERNEL" ]; then
    # The repository's established nandsim/virtme proof pins .181: the newer
    # WSL-installed .185 image has been observed to stall before virtme init.
    for candidate in /boot/vmlinuz-5.15.0-181-generic /boot/vmlinuz-5.15.0-185-generic /boot/vmlinuz; do
        if [ -f "$candidate" ]; then KERNEL=$candidate; break; fi
    done
fi
[ -f "$KERNEL" ] || die "kernel not found; pass --kernel"

if [ "$NO_BUILD" != 1 ]; then
    (cd "$PROJECT_DIR/dcentrald" && cargo build -p dcentrald --features sim-hal)
fi
BINARY="$PROJECT_DIR/dcentrald/target/debug/dcentrald"
[ -x "$BINARY" ] || die "sim-hal dcentrald binary not found: $BINARY"

if [ -z "$EVIDENCE_DIR" ]; then
    EVIDENCE_DIR="$PROJECT_DIR/output/sim-evidence/$MODEL"
fi
mkdir -p "$EVIDENCE_DIR"
TRANSCRIPT="$EVIDENCE_DIR/virtme-sim-hal.log"

PROJECT_Q=$(shell_quote "$PROJECT_DIR")
MODEL_Q=$(shell_quote "$MODEL")
GUEST_CMD="set -euo pipefail
cd $PROJECT_Q
test -r /proc/version
grep -Eq 'Linux version' /proc/version
# WSL's copy-on-write root may contribute dormant tty nodes. Remove them only
# in this disposable VM namespace; the production detector itself is unchanged.
rm -f /dev/uio0 /dev/ttyO1 /dev/ttyS1 /dev/uart_trans /dev/uart_trans0
for p in /dev/uio0 /dev/ttyO1 /dev/ttyS1 /dev/uart_trans /dev/uart_trans0; do test ! -e \"\$p\"; done
export DCENT_SIM_HAL=1
export DCENT_CONFIRM_SIM_HAL_IS_NOT_REAL_HARDWARE=1
export DCENT_SIM_MODEL=$MODEL_Q
export RUST_LOG=info
DAEMON_LOG=/tmp/dcentrald-sim-$MODEL.log
MCP_LOG=/tmp/dcentrald-mcp-$MODEL.log
cleanup() {
    if [ -n \"\${mcp_pid:-}\" ]; then kill -TERM \"\$mcp_pid\" 2>/dev/null || true; wait \"\$mcp_pid\" 2>/dev/null || true; fi
    if [ -n \"\${daemon_pid:-}\" ]; then kill -TERM \"\$daemon_pid\" 2>/dev/null || true; wait \"\$daemon_pid\" 2>/dev/null || true; fi
}
trap cleanup EXIT INT TERM
./dcentrald/target/debug/dcentrald --config /tmp/dcent-sim-no-config.toml >\"\$DAEMON_LOG\" 2>&1 &
daemon_pid=\$!
ready=0
for _ in \$(seq 1 160); do
    if ! kill -0 \"\$daemon_pid\" 2>/dev/null; then cat \"\$DAEMON_LOG\"; exit 1; fi
    if curl -fsS http://127.0.0.1:8080/api/status >/tmp/dcent-sim-status.json 2>/dev/null; then ready=1; break; fi
    sleep 0.1
done
test \"\$ready\" = 1
python3 - $MODEL_Q /tmp/dcent-sim-status.json <<'PY'
import json, sys
model, path = sys.argv[1:]
data = json.load(open(path, encoding='utf-8'))
assert data.get('accepted') == 1, data
chains = data.get('chains') or []
assert len(chains) == 3, data
assert all(c.get('status') == 'simulated-ready' and c.get('chips', 0) > 0 for c in chains), data
print(f'SIM_REST_OK model={model} chains={len(chains)} chips={chains[0][\"chips\"]} accepted=1')
PY
curl -fsS -H 'Content-Type: application/json' -H 'X-Dcentos-Dashboard-Proxy: 1' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"offline-proof\",\"version\":\"1\"}}}' http://127.0.0.1:8080/mcp >/tmp/dcent-sim-rust-mcp.json
python3 - /tmp/dcent-sim-rust-mcp.json <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding='utf-8'))
assert data.get('result') or data.get('jsonrpc') == '2.0', data
print('SIM_RUST_MCP_OK port=8080 path=/mcp')
PY
python3 br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py --bind 127.0.0.1 --port 3000 >\"\$MCP_LOG\" 2>&1 &
mcp_pid=\$!
mcp_ready=0
for _ in \$(seq 1 80); do
    if curl -fsS http://127.0.0.1:3000/mcp >/tmp/dcent-sim-mcp3000.json 2>/dev/null; then mcp_ready=1; break; fi
    sleep 0.1
done
test \"\$mcp_ready\" = 1
python3 - /tmp/dcent-sim-mcp3000.json <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding='utf-8'))
assert data.get('name') == 'dcentos-mcp' and data.get('tools', 0) > 0, data
print(f'SIM_ROOTFS_MCP_OK port=3000 tools={data[\"tools\"]}')
PY
kill -TERM \"\$daemon_pid\"
wait \"\$daemon_pid\"
daemon_pid=
grep -q 'SIM_HAL_RUNTIME_READY' \"\$DAEMON_LOG\"
grep -q 'SIM_HAL_RUNTIME_STOPPED' \"\$DAEMON_LOG\"
cat \"\$DAEMON_LOG\"
echo OFFLINE_VIRTME_SIM_HAL_OK model=$MODEL_Q"

echo "VIRTME_SIM_HAL_RUN model=$MODEL kernel=$KERNEL" | tee "$TRANSCRIPT"
set +e
vng --run "$KERNEL" --memory "$MEMORY" --cpus "$CPUS" --disable-kvm --exec "$GUEST_CMD" 2>&1 | tee -a "$TRANSCRIPT"
rc=${PIPESTATUS[0]}
set -e
[ "$rc" = 0 ] || die "virtme sim proof failed (see $TRANSCRIPT)"
grep -q "OFFLINE_VIRTME_SIM_HAL_OK model=$MODEL" "$TRANSCRIPT" || die "success sentinel missing"
echo "OFFLINE_VIRTME_SIM_HAL_PROOF_OK model=$MODEL evidence=$TRANSCRIPT"
