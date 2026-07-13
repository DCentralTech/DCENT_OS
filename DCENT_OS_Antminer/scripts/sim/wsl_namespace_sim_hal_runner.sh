#!/usr/bin/env bash
# WSL-only full-daemon sim proof in a private mount/device namespace.

set -euo pipefail
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)
MODEL=
NO_BUILD=0
EVIDENCE_DIR=

usage() { echo "usage: $0 --model MODEL [--no-build] [--evidence-dir DIR]" >&2; exit 2; }
shell_quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }
die() { echo "ERROR: $*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) MODEL=${2:-}; shift 2 ;;
        --no-build) NO_BUILD=1; shift ;;
        --evidence-dir) EVIDENCE_DIR=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$MODEL" ] || usage
case "$MODEL" in
    s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro) ;;
    *) die "$MODEL has no integrated T2 runtime proof" ;;
esac
grep -qi microsoft /proc/version || die "this namespace runner is deliberately WSL-only"
[ "$(id -u)" = 0 ] || die "root is required for an isolated tmpfs /dev namespace"
command -v unshare >/dev/null 2>&1 || die "util-linux unshare is required"
command -v mount >/dev/null 2>&1 || die "mount is required"

if [ "$NO_BUILD" != 1 ]; then
    (cd "$PROJECT_DIR/dcentrald" && cargo build -p dcentrald --features sim-hal)
fi
[ -x "$PROJECT_DIR/dcentrald/target/debug/dcentrald" ] || die "sim-hal dcentrald binary missing"
if [ -z "$EVIDENCE_DIR" ]; then EVIDENCE_DIR="$PROJECT_DIR/output/sim-evidence/$MODEL-wsl"; fi
mkdir -p "$EVIDENCE_DIR"
TRANSCRIPT="$EVIDENCE_DIR/wsl-namespace-sim-hal.log"

PROJECT_Q=$(shell_quote "$PROJECT_DIR")
MODEL_Q=$(shell_quote "$MODEL")
CHECK="set -euo pipefail
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
for p in /dev/uio0 /dev/ttyO1 /dev/ttyS1 /dev/uart_trans /dev/uart_trans0; do test ! -e \"\$p\"; done
cd $PROJECT_Q
export DCENT_SIM_HAL=1 DCENT_CONFIRM_SIM_HAL_IS_NOT_REAL_HARDWARE=1 DCENT_SIM_MODEL=$MODEL_Q RUST_LOG=info
log=/tmp/dcentrald-wsl-$MODEL.log
cleanup() { if [ -n \"\${pid:-}\" ]; then kill -TERM \"\$pid\" 2>/dev/null || true; wait \"\$pid\" 2>/dev/null || true; fi; if [ -n \"\${mcp_pid:-}\" ]; then kill -TERM \"\$mcp_pid\" 2>/dev/null || true; wait \"\$mcp_pid\" 2>/dev/null || true; fi; }
trap cleanup EXIT INT TERM
./dcentrald/target/debug/dcentrald --config /tmp/dcent-sim-no-config.toml >\"\$log\" 2>&1 & pid=\$!
ready=0
for _ in \$(seq 1 200); do if ! kill -0 \"\$pid\" 2>/dev/null; then cat \"\$log\"; exit 1; fi; if curl -fsS http://127.0.0.1:8080/api/status >/tmp/status.json 2>/dev/null; then ready=1; break; fi; sleep 0.1; done
test \"\$ready\" = 1
python3 - $MODEL_Q /tmp/status.json <<'PY'
import json, sys
model, path = sys.argv[1:]
d = json.load(open(path, encoding='utf-8'))
chains = d.get('chains') or []
assert d.get('accepted') == 1 and len(chains) == 3, d
assert all(c.get('status') == 'simulated-ready' and c.get('chips', 0) > 0 for c in chains), d
print(f'SIM_WSL_REST_OK model={model} chains=3 chips={chains[0][\"chips\"]} accepted=1')
PY
curl -fsS -H 'Content-Type: application/json' -H 'X-Dcentos-Dashboard-Proxy: 1' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"offline-proof\",\"version\":\"1\"}}}' http://127.0.0.1:8080/mcp >/tmp/rust-mcp.json
python3 - /tmp/rust-mcp.json <<'PY'
import json, sys
d=json.load(open(sys.argv[1], encoding='utf-8'))
assert d.get('result') or d.get('jsonrpc') == '2.0', d
print('SIM_WSL_RUST_MCP_OK port=8080 path=/mcp')
PY
python3 br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py --bind 127.0.0.1 --port 3000 >/tmp/mcp3000.log 2>&1 & mcp_pid=\$!
for _ in \$(seq 1 100); do if curl -fsS http://127.0.0.1:3000/mcp >/tmp/mcp3000.json 2>/dev/null; then break; fi; sleep 0.1; done
python3 - /tmp/mcp3000.json <<'PY'
import json, sys
d=json.load(open(sys.argv[1], encoding='utf-8'))
assert d.get('name') == 'dcentos-mcp' and d.get('tools', 0) > 0, d
print(f'SIM_WSL_ROOTFS_MCP_OK port=3000 tools={d[\"tools\"]}')
PY
kill -TERM \"\$pid\"; wait \"\$pid\"; pid=
grep -q SIM_HAL_RUNTIME_READY \"\$log\"
grep -q SIM_HAL_RUNTIME_STOPPED \"\$log\"
cat \"\$log\"
echo OFFLINE_WSL_NAMESPACE_SIM_HAL_OK model=$MODEL_Q"

echo "WSL_NAMESPACE_SIM_HAL_RUN model=$MODEL" | tee "$TRANSCRIPT"
set +e
unshare --mount --propagation private -- bash -c "$CHECK" 2>&1 | tee -a "$TRANSCRIPT"
rc=${PIPESTATUS[0]}
set -e
[ "$rc" = 0 ] || die "WSL namespace proof failed (see $TRANSCRIPT)"
grep -q "OFFLINE_WSL_NAMESPACE_SIM_HAL_OK model=$MODEL" "$TRANSCRIPT" || die "success sentinel missing"
echo "OFFLINE_WSL_NAMESPACE_SIM_HAL_PROOF_OK model=$MODEL evidence=$TRANSCRIPT"
