#!/usr/bin/env bash
# Run the full WSL namespace daemon proof for every integrated T2 model.

set -euo pipefail
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)

for model in s9 s17 s17pro t17 s19pro s19jpro s19xp s19kpro s21 s21pro; do
    out="${TMPDIR:-/tmp}/dcent-wsl-${model}.out"
    if ! timeout 60 "$SCRIPT_DIR/wsl_namespace_sim_hal_runner.sh" \
        --model "$model" --no-build >"$out" 2>&1; then
        tail -80 "$out" >&2
        exit 1
    fi
    if grep -E 'readback (TIMEOUT|MISMATCH)' "$out" >/dev/null; then
        grep -E 'readback (TIMEOUT|MISMATCH)' "$out" >&2
        exit 1
    fi
    grep 'OFFLINE_WSL_NAMESPACE_SIM_HAL_PROOF_OK' "$out"
done

echo WSL_SIM_ALL_INTEGRATED_MODELS_OK
