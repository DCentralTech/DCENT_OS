#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_DIR="$PROJECT_DIR/br2_external_dcentos/packages/dcentos-receipt/src"
BUILD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-receipt-projection.XXXXXX")
trap 'rm -rf "$BUILD_DIR"' EXIT HUP INT TERM

${CC:-cc} \
    -std=c11 -O2 -Wall -Wextra -Werror -pedantic \
    -I"$SOURCE_DIR" \
    "$SOURCE_DIR/sha256.c" \
    "$SOURCE_DIR/receipt_state.c" \
    "$SOURCE_DIR/receipt_format.c" \
    "$SOURCE_DIR/receipt_storage.c" \
    "$SOURCE_DIR/receipt_projection.c" \
    "$SCRIPT_DIR/test_dcentos_receipt_projection.c" \
    -o "$BUILD_DIR/test_dcentos_receipt_projection"

"$BUILD_DIR/test_dcentos_receipt_projection"
