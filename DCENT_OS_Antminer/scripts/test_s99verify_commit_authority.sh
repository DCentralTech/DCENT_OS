#!/bin/sh
# Behavioral contract for S99verify's report-only V14 observer.

set -eu

HERE=$(CDPATH= cd "$(dirname "$0")" && pwd)
ROOT=$(CDPATH= cd "$HERE/.." && pwd)
SRC="$ROOT/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99verify"
[ -r "$SRC" ] || { echo "FAIL: missing $SRC" >&2; exit 1; }

if grep -nE '^[[:space:]]*(fw_setenv|nandwrite|flash_erase)([[:space:]]|$)' "$SRC"; then
    echo "FAIL: S99verify contains a durable boot-state mutation command" >&2
    exit 1
fi

fn=$(awk '/^check_upgrade_stage_cleared\(\) \{/{p=1} p{print} p&&/^\}/{exit}' "$SRC")
case "$fn" in
    *"check_upgrade_stage_cleared()"*"UPGRADE_COMMIT_MARKER"*) : ;;
    *) echo "FAIL: could not extract report-only V14 observer" >&2; exit 1 ;;
esac

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
UPGRADE_COMMIT_MARKER="$TMP/commit-marker"
FW_PRINTENV_OUTPUT=""
LAST_ID=""
LAST_PASSED=""
LAST_DETAIL=""

emit_check() {
    LAST_ID=$1
    LAST_PASSED=$2
    LAST_DETAIL=$3
}

fw_printenv() {
    [ -n "$FW_PRINTENV_OUTPUT" ] && printf '%s\n' "$FW_PRINTENV_OUTPUT"
    return 0
}

eval "$fn"

run_case() {
    description=$1
    platform=$2
    stage_output=$3
    marker=$4
    expected_passed=$5
    expected_detail=$6

    PLATFORM=$platform
    FW_PRINTENV_OUTPUT=$stage_output
    LAST_ID=""
    LAST_PASSED=""
    LAST_DETAIL=""
    rm -f "$UPGRADE_COMMIT_MARKER"
    if [ -n "$marker" ]; then
        printf '%s\n' "$marker" > "$UPGRADE_COMMIT_MARKER"
    fi

    check_upgrade_stage_cleared
    if [ "$LAST_ID" != "V14" ] || [ "$LAST_PASSED" != "$expected_passed" ]; then
        echo "FAIL: $description returned id=$LAST_ID passed=$LAST_PASSED detail=$LAST_DETAIL" >&2
        exit 1
    fi
    case "$LAST_DETAIL" in
        *"$expected_detail"*) : ;;
        *) echo "FAIL: $description detail '$LAST_DETAIL' lacks '$expected_detail'" >&2; exit 1 ;;
    esac
}

run_case "already committed" am2 "" "" true "upgrade_stage already absent"
run_case "blocked slot" am2 "upgrade_stage=1" blocked true "auto-recovery remains armed"
run_case "inconsistent committed marker" am2 "upgrade_stage=1" committed false "still SET"
run_case "missing decision marker" am2 "upgrade_stage=1" "" false "decision='missing'"
run_case "Amlogic delegated authority" am3-aml "" "" true "report-only"
run_case "unknown platform" unknown "" "" false "refuses to mutate"

echo "PASS: S99verify V14 observes commit state without owning durable mutation"
