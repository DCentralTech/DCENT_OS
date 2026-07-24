#!/bin/sh
# Native, device-free tests for the EXPERIMENTAL compiled receipt foundation.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-core-test.$$
BIN=$WORK_ROOT/dcentos-receipt-core-test
CC=${CC:-cc}

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 "$WORK_ROOT"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -O2 \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_core.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$BIN"

"$BIN"

for size in 0 1 55 56 63 64 65 127 128 129 1024 1048576; do
    fixture=$WORK_ROOT/bytes-$size
    if [ "$size" -eq 0 ]; then
        : >"$fixture"
    else
        dd if=/dev/zero of="$fixture" bs=1 count="$size" 2>/dev/null
    fi
    expected=$(sha256sum "$fixture" | awk '{ print $1 }')
    actual=$("$BIN" --sha256-file "$fixture")
    [ "$actual" = "$expected" ] || {
        echo "FAIL: SHA-256 differential size=$size expected=$expected actual=$actual" >&2
        exit 1
    }
done

echo "dcentos-receipt SHA-256 differential tests: 12 vectors"
