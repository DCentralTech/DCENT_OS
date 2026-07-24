#!/bin/sh
# Durable sanitizer, analyzer, and production-stack gates for receipt C code.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-quality.$$
CC=${CC:-cc}

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 "$WORK_ROOT"

for source in sha256 receipt_state receipt_format receipt_store \
    receipt_storage receipt_projection; do
    "$CC" -std=c11 -O2 -Wall -Wextra -Werror -pedantic -fanalyzer \
        -I"$SOURCE_ROOT" -c "$SOURCE_ROOT/$source.c" \
        -o "$WORK_ROOT/$source-analyzer.o"
done
echo "dcentos-receipt GCC analyzer gate: 6 translation units passed"

"$CC" -std=c11 -O1 -g -Wall -Wextra -Werror -pedantic \
    -fno-omit-frame-pointer -fsanitize=address,undefined \
    -I"$SOURCE_ROOT" \
    "$SOURCE_ROOT/sha256.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_storage.c" \
    "$SOURCE_ROOT/receipt_projection.c" \
    "$SCRIPT_DIR/test_dcentos_receipt_projection.c" \
    -o "$WORK_ROOT/dcentos-receipt-projection-sanitized"
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 \
UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1 \
    "$WORK_ROOT/dcentos-receipt-projection-sanitized"
echo "dcentos-receipt ASan/UBSan projection gate: passed"

"$CC" -std=c11 -O2 -Wall -Wextra -Werror -pedantic -fstack-usage \
    -I"$SOURCE_ROOT" -c "$SOURCE_ROOT/receipt_projection.c" \
    -o "$WORK_ROOT/receipt-projection-stack.o"
STACK_USAGE=$WORK_ROOT/receipt-projection-stack.su
[ -f "$STACK_USAGE" ] || {
    echo "FAIL: compiler did not emit receipt-projection stack usage" >&2
    exit 1
}
awk -F '\t' '
    {
        bytes = $2 + 0
        if (bytes > maximum)
            maximum = bytes
    }
    END {
        if (maximum == 0 || maximum > 16384)
            exit 1
        printf "dcentos-receipt optimized projection function stack bound: %u bytes\n", maximum
    }
' "$STACK_USAGE" || {
    echo "FAIL: receipt projection function exceeds its 16 KiB frame budget" >&2
    cat "$STACK_USAGE" >&2
    exit 1
}
