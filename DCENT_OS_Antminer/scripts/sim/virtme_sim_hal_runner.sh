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
EXPECTED_BINARY_SHA256=

usage() {
    cat <<'EOF'
Usage: virtme_sim_hal_runner.sh --model MODEL [options]

Options:
  --kernel PATH       virtme kernel (default: installed Ubuntu kernel)
  --memory SIZE       guest memory (default: 1536M)
  --cpus N            guest CPUs (default: 2)
  --evidence-dir DIR  retain the host-side VM transcript
  --no-build          use the dedicated sim-hal runner target binary
  --expected-binary-sha256 SHA256
                      required with --no-build; pins the launched binary
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
        --expected-binary-sha256) EXPECTED_BINARY_SHA256=${2:-}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
done

[ -n "$MODEL" ] || { usage >&2; die "--model is required"; }
case "$MODEL" in
    s9|s17|s17pro|t17|s19pro|s19jpro|s19xp|s19kpro|s21|s21pro) ;;
    *) die "$MODEL has no integrated T2 runtime proof" ;;
esac
for command in awk cargo curl mktemp python3 rustc sed sha256sum timeout vng; do
    command -v "$command" >/dev/null 2>&1 || die "$command is required"
done

if [ -z "$KERNEL" ]; then
    # The repository's established nandsim/virtme proof pins .181: the newer
    # WSL-installed .185 image has been observed to stall before virtme init.
    for candidate in /boot/vmlinuz-5.15.0-181-generic /boot/vmlinuz-5.15.0-185-generic /boot/vmlinuz; do
        if [ -f "$candidate" ]; then KERNEL=$candidate; break; fi
    done
fi
[ -f "$KERNEL" ] || die "kernel not found; pass --kernel"

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
[ -x "$BINARY" ] || die "sim-hal dcentrald binary not found: $BINARY"
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
    EVIDENCE_DIR=$(mktemp -d "$evidence_root/${MODEL}-virtme-XXXXXX")
else
    mkdir -p "$EVIDENCE_DIR"
fi
TRANSCRIPT="$EVIDENCE_DIR/virtme-sim-hal.log"

HELPER_Q=$(shell_quote "$SCRIPT_DIR/sim_hal_runtime_check.sh")
MODEL_Q=$(shell_quote "$MODEL")
SHA_Q=$(shell_quote "$EXPECTED_BINARY_SHA256")
BINARY_Q=$(shell_quote "$BINARY")
GUEST_CMD="exec unshare --net -- bash $HELPER_Q --model $MODEL_Q --mode virtme-guest --binary-path $BINARY_Q --expected-binary-sha256 $SHA_Q"

echo "VIRTME_SIM_HAL_RUN model=$MODEL kernel=$KERNEL binary_sha256=$actual_sha256" | tee "$TRANSCRIPT"
set +e
timeout --signal=TERM --kill-after=15s 180s \
    vng --run "$KERNEL" --memory "$MEMORY" --cpus "$CPUS" --disable-kvm \
        --exec "$GUEST_CMD" 2>&1 | tee -a "$TRANSCRIPT"
pipeline_status=("${PIPESTATUS[@]}")
set -e
[ "${pipeline_status[1]}" = 0 ] || die "failed to retain virtme proof transcript: $TRANSCRIPT"
[ "${pipeline_status[0]}" = 0 ] || die "virtme sim proof failed (see $TRANSCRIPT)"
grep -q "OFFLINE_SIM_HAL_RUNTIME_OK mode=virtme-guest model=$MODEL binary_sha256=$actual_sha256" \
    "$TRANSCRIPT" || die "runtime success sentinel missing"
if grep -E 'readback (TIMEOUT|MISMATCH)' "$TRANSCRIPT" >/dev/null; then
    die "simulator PLL readback failure is present in $TRANSCRIPT"
fi
echo "OFFLINE_VIRTME_SIM_HAL_OK model=$MODEL" | tee -a "$TRANSCRIPT"
echo "OFFLINE_VIRTME_SIM_HAL_PROOF_OK model=$MODEL evidence=$TRANSCRIPT"
