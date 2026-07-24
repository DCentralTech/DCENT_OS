#!/usr/bin/env bash
# Combine write-path evidence (where an image family exists) with sim runtime.

set -euo pipefail
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/../.." && pwd)
MODEL=
SKIP_NANDSIM=0

usage() { echo "usage: $0 --model MODEL [--skip-nandsim]" >&2; exit 2; }
while [ "$#" -gt 0 ]; do
    case "$1" in
        --model) MODEL=${2:-}; shift 2 ;;
        --skip-nandsim) SKIP_NANDSIM=1; shift ;;
        *) usage ;;
    esac
done
[ -n "$MODEL" ] || usage

case "$MODEL" in
    s9) nand_target=am1-s9 ;;
    s19jpro) nand_target=am2-s19jpro ;;
    s19pro)
        echo "$MODEL has T2 runtime evidence and an experimental init snapshot, but no T3 or model-bound nandsim/rootfs artifact; refusing a T4 claim" >&2
        exit 1
        ;;
    s17|s17pro|t17|s19xp|s19kpro|s21|s21pro)
        echo "$MODEL has T2/T3 runtime evidence but no model-bound nandsim/rootfs artifact; refusing a T4 claim" >&2
        exit 1
        ;;
    *) echo "$MODEL has no integrated T4 runtime proof" >&2; exit 1 ;;
esac

stamp=$(date -u +%Y%m%dT%H%M%SZ)
evidence_root="$PROJECT_DIR/output/sim-evidence"
mkdir -p "$evidence_root"
evidence=$(mktemp -d "$evidence_root/${MODEL}-${stamp}-XXXXXX")

if [ "$SKIP_NANDSIM" != 1 ] && [ -n "$nand_target" ]; then
    "$PROJECT_DIR/scripts/sysupgrade_offline_virtme_nandsim_runner.sh" --target "$nand_target" \
        2>&1 | tee "$evidence/nandsim-write-path.log"
    grep -q 'OFFLINE_NANDSIM_PROOF_OK' "$evidence/nandsim-write-path.log"
else
    printf '%s\n' "NANDSIM_SKIPPED_BY_OPERATOR model=$MODEL" >"$evidence/WRITE_PATH_LIMITATION.txt"
fi

"$SCRIPT_DIR/virtme_sim_hal_runner.sh" --model "$MODEL" --evidence-dir "$evidence"
python3 "$SCRIPT_DIR/check_sim_tier_honesty.py" >"$evidence/tier-honesty.log"
python3 "$SCRIPT_DIR/ladder_matrix.py" >"$evidence/ladder-matrix.txt"

(
    cd "$evidence"
    find . -maxdepth 1 -type f ! -name EVIDENCE_MANIFEST.txt -print0 \
        | sort -z | xargs -0 sha256sum
) >"$evidence/EVIDENCE_MANIFEST.txt"
grep -q 'OFFLINE_VIRTME_SIM_HAL_OK' "$evidence/virtme-sim-hal.log"
if [ "$SKIP_NANDSIM" = 1 ]; then
    echo "OFFLINE_SIM_HAL_RUNTIME_ONLY_OK model=$MODEL evidence=$evidence"
    exit 3
fi
echo "OFFLINE_SIM_HAL_PROOF_OK model=$MODEL evidence=$evidence"
