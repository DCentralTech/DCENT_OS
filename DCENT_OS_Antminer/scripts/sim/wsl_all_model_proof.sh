#!/usr/bin/env bash
# Run the full WSL namespace daemon proof for every integrated T2 model.

set -euo pipefail
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)

for command in awk cargo mktemp rustc sed sha256sum timeout; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "ERROR: $command is required" >&2
        exit 1
    }
done

HOST_TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$HOST_TRIPLE" ] || { echo "ERROR: could not determine the Rust host target" >&2; exit 1; }
TARGET_DIR="$PROJECT_DIR/dcentrald/target/sim-hal-runner"
(cd "$PROJECT_DIR/dcentrald" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build --locked --offline --target "$HOST_TRIPLE" -p dcentrald --features sim-hal)
BINARY="$TARGET_DIR/$HOST_TRIPLE/debug/dcentrald"
[ -x "$BINARY" ] || { echo "ERROR: sim-hal dcentrald binary missing" >&2; exit 1; }
binary_sha256=$(sha256sum "$BINARY" | awk '{print $1}')

for model in s9 s17 s17pro t17 s19pro s19jpro s19xp s19kpro s21 s21pro; do
    out=$(mktemp "${TMPDIR:-/tmp}/dcent-wsl-${model}.XXXXXX")
    if ! timeout --signal=TERM --kill-after=15s 150s \
        bash "$SCRIPT_DIR/wsl_namespace_sim_hal_runner.sh" \
        --model "$model" \
        --no-build \
        --expected-binary-sha256 "$binary_sha256" >"$out" 2>&1; then
        tail -80 "$out" >&2
        rm -f -- "$out"
        exit 1
    fi
    if grep -E 'readback (TIMEOUT|MISMATCH)' "$out" >/dev/null; then
        grep -E 'readback (TIMEOUT|MISMATCH)' "$out" >&2
        rm -f -- "$out"
        exit 1
    fi
    grep "OFFLINE_WSL_NAMESPACE_SIM_HAL_PROOF_OK model=$model" "$out"
    rm -f -- "$out"
done

echo "WSL_SIM_ALL_INTEGRATED_MODELS_OK binary_sha256=$binary_sha256"
