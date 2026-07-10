#!/usr/bin/env bash
#
# check_profiles_drift.sh — silicon-profile JSON drift-guard (Wave M).
#
# The 24 silicon-profile bundles (Wave K) live in THREE tracked copies:
#   1. canonical source  — dcentrald/etc/dcentrald/profiles.d/        (migration output)
#   2. amlogic overlay    — br2_external_dcentos/board/amlogic/rootfs-overlay/etc/dcentrald/profiles.d/
#   3. zynq overlay       — br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentrald/profiles.d/
#
# Re-running scripts/migrate-baked-profiles.py updates copy 1 but NOT 2/3, so the
# committed canonical bundles can silently diverge from what ships on-device.
# This guard FAILS if the three copies are not byte-identical. It does NOT re-run
# the migration (the migration stamps extraction_date/extracted_by, which would
# false-positive on a fresh run — Wave-K finding); it compares the committed trees.
#
# Run from anywhere; paths resolve relative to this script's DCENT_OS_Antminer root.
# Wired into .github/workflows/dcentos-offline-gates.yml. All paths are quoted so
# the guard works under a checkout dir containing spaces.

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# DCENT_OS_Antminer (scripts/ is directly under it)
DCENTOS_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

SRC="${DCENTOS_ROOT}/dcentrald/etc/dcentrald/profiles.d"
AML_OV="${DCENTOS_ROOT}/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/dcentrald/profiles.d"
ZYNQ_OV="${DCENTOS_ROOT}/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentrald/profiles.d"

remediate() {
    {
        echo "  Remediation: regenerate + re-ship the bundles, then re-run this guard:"
        echo "    python scripts/migrate-baked-profiles.py"
        echo "    for ov in board/amlogic board/zynq; do"
        echo "      rm -rf \"br2_external_dcentos/\$ov/rootfs-overlay/etc/dcentrald/profiles.d\""
        echo "      cp -r dcentrald/etc/dcentrald/profiles.d \\"
        echo "            \"br2_external_dcentos/\$ov/rootfs-overlay/etc/dcentrald/profiles.d\""
        echo "    done"
    } >&2
}

# 1. Canonical source must exist and carry the full bundle set (catch deletion).
if [ ! -d "$SRC" ]; then
    echo "FAIL: canonical silicon-profile source missing: $SRC" >&2
    remediate
    exit 1
fi
SRC_COUNT="$(find "$SRC" -name '*.json' | wc -l | tr -d ' ')"
if [ "$SRC_COUNT" -lt 24 ]; then
    echo "FAIL: canonical source has $SRC_COUNT json (<24 expected — 9 baked + 15 vendor)" >&2
    remediate
    exit 1
fi

# 2. Each shipped overlay must be byte-identical to the canonical source.
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
rc=0

check_overlay() {
    ov="$1"
    name="$2"
    if [ ! -d "$ov" ]; then
        echo "FAIL: shipped overlay missing ($name): $ov" >&2
        return 1
    fi
    if ! diff -r "$SRC" "$ov" >"$TMP" 2>&1; then
        echo "FAIL: silicon-profile DRIFT — $name overlay differs from canonical source:" >&2
        echo "  source:  $SRC" >&2
        echo "  overlay: $ov" >&2
        sed 's/^/    /' "$TMP" >&2
        return 1
    fi
    return 0
}

check_overlay "$AML_OV"  amlogic || rc=1
check_overlay "$ZYNQ_OV" zynq    || rc=1

if [ "$rc" -ne 0 ]; then
    remediate
    exit 1
fi

echo "OK: silicon-profile bundles consistent across source + amlogic + zynq overlays ($SRC_COUNT json each)"
