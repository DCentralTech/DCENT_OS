#!/usr/bin/env bash
# WSL-only full-daemon sim proof in private mount, device, and network namespaces.

set -euo pipefail
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)
MODEL=
NO_BUILD=0
EVIDENCE_DIR=
EXPECTED_BINARY_SHA256=

usage() {
    echo "usage: $0 --model MODEL [--no-build --expected-binary-sha256 SHA256] [--evidence-dir DIR]" >&2
    exit 2
}
die() { echo "ERROR: $*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) MODEL=${2:-}; shift 2 ;;
        --no-build) NO_BUILD=1; shift ;;
        --expected-binary-sha256) EXPECTED_BINARY_SHA256=${2:-}; shift 2 ;;
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
[ "$(id -u)" = 0 ] || die "root is required for isolated device and network namespaces"
for command in awk cargo mktemp rustc sed sha256sum timeout unshare; do
    command -v "$command" >/dev/null 2>&1 || die "$command is required"
done

if [ "$NO_BUILD" = 1 ]; then
    [ -n "$EXPECTED_BINARY_SHA256" ] \
        || die "--no-build requires --expected-binary-sha256"
fi
HOST_TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$HOST_TRIPLE" ] || die "could not determine the Rust host target"
TARGET_DIR="$PROJECT_DIR/dcentrald/target/sim-hal-runner"
if [ "$NO_BUILD" != 1 ]; then
    (cd "$PROJECT_DIR/dcentrald" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build --locked --offline --target "$HOST_TRIPLE" -p dcentrald --features sim-hal)
fi
BINARY="$TARGET_DIR/$HOST_TRIPLE/debug/dcentrald"
[ -x "$BINARY" ] || die "sim-hal dcentrald binary missing: $BINARY"
actual_sha256=$(sha256sum "$BINARY" | awk '{print $1}')
if [ -n "$EXPECTED_BINARY_SHA256" ]; then
    [ "$actual_sha256" = "$EXPECTED_BINARY_SHA256" ] \
        || die "sim-hal binary SHA-256 mismatch before launch"
else
    EXPECTED_BINARY_SHA256=$actual_sha256
fi

if [ -z "$EVIDENCE_DIR" ]; then
    evidence_root="$PROJECT_DIR/output/sim-evidence"
    mkdir -p "$evidence_root"
    EVIDENCE_DIR=$(mktemp -d "$evidence_root/${MODEL}-wsl-XXXXXX")
else
    mkdir -p "$EVIDENCE_DIR"
fi
TRANSCRIPT="$EVIDENCE_DIR/wsl-namespace-sim-hal.log"

echo "WSL_NAMESPACE_SIM_HAL_RUN model=$MODEL binary_sha256=$actual_sha256" | tee "$TRANSCRIPT"
set +e
timeout --signal=TERM --kill-after=10s 120s \
    unshare --mount --net --propagation private -- \
    bash "$SCRIPT_DIR/sim_hal_runtime_check.sh" \
    --model "$MODEL" \
    --mode wsl-private \
    --binary-path "$BINARY" \
    --expected-binary-sha256 "$EXPECTED_BINARY_SHA256" \
    2>&1 | tee -a "$TRANSCRIPT"
pipeline_status=("${PIPESTATUS[@]}")
set -e
[ "${pipeline_status[1]}" = 0 ] || die "failed to retain WSL proof transcript: $TRANSCRIPT"
[ "${pipeline_status[0]}" = 0 ] || die "WSL namespace proof failed (see $TRANSCRIPT)"
grep -q "OFFLINE_SIM_HAL_RUNTIME_OK mode=wsl-private model=$MODEL binary_sha256=$actual_sha256" \
    "$TRANSCRIPT" || die "runtime success sentinel missing"
if grep -E 'readback (TIMEOUT|MISMATCH)' "$TRANSCRIPT" >/dev/null; then
    die "simulator PLL readback failure is present in $TRANSCRIPT"
fi
echo "OFFLINE_WSL_NAMESPACE_SIM_HAL_OK model=$MODEL" | tee -a "$TRANSCRIPT"
echo "OFFLINE_WSL_NAMESPACE_SIM_HAL_PROOF_OK model=$MODEL evidence=$TRANSCRIPT"
