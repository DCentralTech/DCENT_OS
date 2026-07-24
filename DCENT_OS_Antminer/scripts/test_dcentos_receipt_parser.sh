#!/bin/sh
# Device-free ABI1 byte-parser and semantic-chain validation.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
CORPUS_ROOT=$SCRIPT_DIR/fuzz/corpus/dcentos-receipt
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-parser-test.$$
BIN=$WORK_ROOT/dcentos-receipt-parser-test
CC=${CC:-cc}

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 "$WORK_ROOT"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -O2 \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_parser.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$BIN"

"$BIN"
for kind in binding lock-owner resource-intent resource-status claim-intent claim-status phase-status; do
    "$BIN" --parse "$kind" "$CORPUS_ROOT/$kind" || {
        echo "FAIL: canonical ABI1 fuzz seed does not parse: $kind" >&2
        exit 1
    }
done
echo "dcentos-receipt canonical corpus: 5 ABI1 records, lock-v3 owner, and phase status"
