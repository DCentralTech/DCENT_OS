#!/bin/sh
# Offline regression test for the fail-closed S9 stock-restore boundary.

set -eu

CANONICAL=${DCENT_S9_RESTORE_CANONICAL:-scripts/revert_to_stock_s9.sh}
LEGACY=${DCENT_S9_RESTORE_LEGACY:-scripts/revert_to_stock.sh}
POST_BUILD=${DCENT_S9_POST_BUILD:-br2_external_dcentos/board/zynq/post-build.sh}

passes=0
failures=0

pass() {
    passes=$((passes + 1))
    printf 'PASS: %s\n' "$1"
}

fail() {
    failures=$((failures + 1))
    printf 'FAIL: %s\n' "$1" >&2
}

require_fixed() {
    file=$1
    marker=$2
    label=$3
    if grep -Fq -- "$marker" "$file"; then
        pass "$label"
    else
        fail "$label"
    fi
}

reject_fixed() {
    file=$1
    marker=$2
    label=$3
    if grep -Fq -- "$marker" "$file"; then
        fail "$label"
    else
        pass "$label"
    fi
}

for required in "$CANONICAL" "$LEGACY" "$POST_BUILD"; do
    if [ ! -f "$required" ]; then
        printf 'FAIL: required file is missing: %s\n' "$required" >&2
        exit 1
    fi
done

require_fixed "$CANONICAL" 'S9 stock restore is disabled' \
    'canonical entry point reports explicit containment'
require_fixed "$CANONICAL" 'exit 1' \
    'canonical entry point returns failure'

# The containment file must not retain a dead copy of the invalidated engine.
# Search the whole file (including comments): stale recipes are unsafe operator
# documentation even when they are unreachable.
for forbidden in \
    flash_erase nandwrite nanddump fw_setenv fw_printenv \
    bootslot active_slot wget curl tftp reboot sysrq-trigger \
    ubiupdatevol ubidetach ubiattach mtd_debug
do
    reject_fixed "$CANONICAL" "$forbidden" \
        "canonical containment has no stale primitive or selector: $forbidden"
done

require_fixed "$LEGACY" 'exec /bin/sh "$SCRIPT_DIR/revert_to_stock_s9.sh" "$@"' \
    'legacy source entry delegates to the canonical boundary'
for forbidden in flash_erase nandwrite fw_setenv fw_printenv wget curl reboot; do
    reject_fixed "$LEGACY" "$forbidden" \
        "legacy source entry contains no destructive implementation: $forbidden"
done

require_fixed "$POST_BUILD" \
    'ln -s revert_to_stock_s9.sh "${TARGET_DIR}/usr/sbin/revert_to_stock.sh"' \
    'Buildroot ships the legacy name as a canonical-script symlink'
reject_fixed "$POST_BUILD" 'REVERT_LEGACY_SRC=' \
    'Buildroot has no second legacy implementation source'
reject_fixed "$POST_BUILD" 'cp "$REVERT_LEGACY_SRC"' \
    'Buildroot cannot copy a duplicate legacy implementation'

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcent-s9-containment.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM
FAKE_BIN="$TMP_ROOT/bin"
SENTINEL="$TMP_ROOT/mutation-called"
mkdir "$FAKE_BIN"

for tool in \
    flash_erase nandwrite nanddump fw_setenv fw_printenv \
    wget curl tftp reboot ubiupdatevol ubidetach ubiattach mtd_debug dd
do
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'printf "%s\n" "$0" >> "$DCENT_S9_SENTINEL"'
        printf '%s\n' 'exit 97'
    } > "$FAKE_BIN/$tool"
    chmod 0755 "$FAKE_BIN/$tool"
done

run_refusal() {
    entry=$1
    label=$2
    shift 2
    : > "$SENTINEL"
    if PATH="$FAKE_BIN:$PATH" DCENT_S9_SENTINEL="$SENTINEL" \
        /bin/sh "$entry" "$@" >"$TMP_ROOT/stdout" 2>"$TMP_ROOT/stderr"
    then
        fail "$label returns nonzero"
    else
        pass "$label returns nonzero"
    fi
    if [ ! -s "$SENTINEL" ]; then
        pass "$label invokes no sentinel mutation command"
    else
        fail "$label invoked mutation command(s): $(tr '\n' ' ' < "$SENTINEL")"
    fi
    if grep -Fq 'S9 stock restore is disabled' "$TMP_ROOT/stderr"; then
        pass "$label reaches explicit containment response"
    else
        fail "$label did not report explicit containment"
    fi
}

run_refusal "$CANONICAL" 'canonical default invocation'
run_refusal "$CANONICAL" 'canonical direct-image invocation' \
    /tmp/operator-provided-stock.tar.gz \
    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
run_refusal "$LEGACY" 'legacy default invocation'
run_refusal "$LEGACY" 'legacy direct-image invocation' \
    /tmp/operator-provided-stock.tar.gz \
    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

if [ "$failures" -ne 0 ]; then
    printf '\nS9 restore containment failed: %s assertion(s), %s passed\n' \
        "$failures" "$passes" >&2
    exit 1
fi

printf '\nS9 restore containment passed: %s assertions\n' "$passes"
