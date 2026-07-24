#!/bin/sh
# Descriptor-only ABI1 lock/ledger layout and race validation.

set -eu

# The production scanner admits only root-owned objects, and this suite also
# proves rejection after changing a fixture to a foreign UID/GID.  Exercise
# that literal boundary instead of compiling a weaker test-only ownership
# policy.  GitHub's hosted Linux runners provide noninteractive sudo; local
# non-root environments must provide the same narrow capability explicitly.
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        exec sudo -n sh "$0" "$@"
    fi
    echo "FAIL: descriptor-store tests require root or noninteractive sudo" >&2
    exit 1
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-store-test.$$
BIN=$WORK_ROOT/dcentos-receipt-store-test
CC=${CC:-cc}

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 "$WORK_ROOT"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -O2 \
    -DDCENT_RECEIPT_STORE_TESTING \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_store.c" \
    "$SOURCE_ROOT/receipt_store.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$BIN"

"$BIN"
if [ -d /dev/shm ] && [ -w /dev/shm ] && \
   [ "$(stat -f -c %T /dev/shm 2>/dev/null || true)" = tmpfs ]; then
    TMPDIR=/dev/shm "$BIN"
    echo "dcentos-receipt production tmpfs topology lane: passed"
else
    echo "dcentos-receipt production tmpfs topology lane: unavailable"
fi

"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -O2 -fstack-usage \
    -I"$SOURCE_ROOT" -c "$SOURCE_ROOT/receipt_store.c" \
    -o "$WORK_ROOT/receipt-store-stack.o"
STACK_USAGE=$WORK_ROOT/receipt-store-stack.su
[ -f "$STACK_USAGE" ] || {
    echo "FAIL: compiler did not emit receipt-store stack usage" >&2
    exit 1
}
awk -F '\t' '
    {
        count = split($1, part, ":")
        function_name = part[count]
        bytes = $2 + 0
        if (function_name == "dcent_receipt_store_scan_forensic_abi1") {
            public_frame = bytes
            saw_public = 1
        } else if (function_name == "scan_once") {
            scan_frame = bytes
            saw_scan = 1
        } else if (function_name == "scan_resource" ||
                   function_name == "scan_claim") {
            if (bytes > chain_frame)
                chain_frame = bytes
            saw_chain = 1
        } else if (function_name == "scan_inventory") {
            inventory_frame = bytes
            saw_inventory = 1
        }
    }
    END {
        total = public_frame + scan_frame + chain_frame + inventory_frame
        if (!saw_public || !saw_scan || !saw_chain || !saw_inventory ||
            total > 65536)
            exit 1
        printf "dcentos-receipt optimized scanner stack bound: %u bytes\n", total
    }
' "$STACK_USAGE" || {
    echo "FAIL: optimized descriptor scanner exceeds the 64 KiB call-chain stack budget" >&2
    cat "$STACK_USAGE" >&2
    exit 1
}
