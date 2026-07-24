#!/bin/sh
# Prove every structured ABI2 storage fuzz seed is an actual valid pair.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
CORPUS_ROOT=$SCRIPT_DIR/fuzz/corpus/dcentos-receipt-storage
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-fuzz-corpus.$$
CC=${CC:-cc}

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 "$WORK_ROOT"
"$CC" -std=c11 -O2 -Wall -Wextra -Werror -pedantic \
    -DDCENT_RECEIPT_STORAGE_FUZZ_CORPUS_MAIN \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/fuzz/dcentos_receipt_storage_fuzz.c" \
    "$SOURCE_ROOT/receipt_storage.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/verify-storage-corpus"

count=0
for seed in "$CORPUS_ROOT"/*; do
    [ -f "$seed" ] || {
        echo "FAIL: structured storage fuzz corpus is empty" >&2
        exit 1
    }
    "$WORK_ROOT/verify-storage-corpus" "$seed"
    count=$((count + 1))
done
echo "dcentos-receipt structured storage fuzz corpus: $count valid pair(s)"
