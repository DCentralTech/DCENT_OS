#!/bin/sh
# Prove the EXPERIMENTAL receipt foundation builds with the exact Zynq ABI.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_ROOT=$PROJECT_ROOT/br2_external_dcentos/packages/dcentos-receipt/src
TOOLCHAIN_NAME=gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz
TOOLCHAIN_ARCHIVE=$PROJECT_ROOT/buildroot/dl/toolchain-external-custom/$TOOLCHAIN_NAME
INPUT_MANIFEST=$SCRIPT_DIR/build_inputs.manifest
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-receipt-cross-test.$$

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

for tool in awk file find grep head sha256sum tar tr wc xz; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FAIL: missing cross-compile proof tool: $tool" >&2
        exit 1
    }
done
[ -f "$TOOLCHAIN_ARCHIVE" ] || {
    echo "FAIL: pinned Zynq toolchain archive is missing: $TOOLCHAIN_ARCHIVE" >&2
    exit 1
}

expected=$(awk -v suffix="DCENT_OS_Antminer/buildroot/dl/toolchain-external-custom/$TOOLCHAIN_NAME" \
    '$2 == suffix { print $1 }' "$INPUT_MANIFEST")
actual=$(sha256sum "$TOOLCHAIN_ARCHIVE" | awk '{ print $1 }')
[ -n "$expected" ] && [ "$actual" = "$expected" ] || {
    echo "FAIL: pinned Zynq toolchain digest mismatch expected=$expected actual=$actual" >&2
    exit 1
}

mkdir -m 700 "$WORK_ROOT"
tar -xJf "$TOOLCHAIN_ARCHIVE" -C "$WORK_ROOT"
TOOLCHAIN_ROOT=$(find "$WORK_ROOT" -mindepth 1 -maxdepth 1 -type d | head -n 1)
CC=$TOOLCHAIN_ROOT/bin/arm-linux-gnueabihf-gcc
READELF=$TOOLCHAIN_ROOT/bin/arm-linux-gnueabihf-readelf
STRIP=$TOOLCHAIN_ROOT/bin/arm-linux-gnueabihf-strip
[ -x "$CC" ] && [ -x "$READELF" ] && [ -x "$STRIP" ] || {
    echo "FAIL: archive does not contain the exact Linaro ARM hard-float tools" >&2
    exit 1
}

"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -Os \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_core.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/dcentos-receipt-core-arm"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -Os \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_parser.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/dcentos-receipt-parser-arm"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -Os \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_store_compile.c" \
    "$SOURCE_ROOT/receipt_store.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/dcentos-receipt-store-arm"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -Os \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_storage.c" \
    "$SOURCE_ROOT/receipt_storage.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/dcentos-receipt-storage-arm"
"$CC" -std=c11 -Wall -Wextra -Werror -pedantic -Os \
    -I"$SOURCE_ROOT" \
    "$SCRIPT_DIR/test_dcentos_receipt_projection.c" \
    "$SOURCE_ROOT/receipt_projection.c" \
    "$SOURCE_ROOT/receipt_storage.c" \
    "$SOURCE_ROOT/receipt_format.c" \
    "$SOURCE_ROOT/receipt_state.c" \
    "$SOURCE_ROOT/sha256.c" \
    -o "$WORK_ROOT/dcentos-receipt-projection-arm"
"$STRIP" "$WORK_ROOT/dcentos-receipt-core-arm"
"$STRIP" "$WORK_ROOT/dcentos-receipt-parser-arm"
"$STRIP" "$WORK_ROOT/dcentos-receipt-store-arm"
"$STRIP" "$WORK_ROOT/dcentos-receipt-storage-arm"
"$STRIP" "$WORK_ROOT/dcentos-receipt-projection-arm"

for binary in dcentos-receipt-core-arm dcentos-receipt-parser-arm \
    dcentos-receipt-store-arm dcentos-receipt-storage-arm \
    dcentos-receipt-projection-arm; do
    file "$WORK_ROOT/$binary" | grep -q 'ELF 32-bit LSB.*ARM.*EABI5' || {
        file "$WORK_ROOT/$binary" >&2
        echo "FAIL: $binary is not an ARM EABI5 executable" >&2
        exit 1
    }
    "$READELF" -l "$WORK_ROOT/$binary" | \
        grep -q '/lib/ld-linux-armhf.so.3' || {
        echo "FAIL: $binary does not use the admitted Zynq glibc hard-float loader" >&2
        exit 1
    }
    if "$READELF" -d "$WORK_ROOT/$binary" | grep -q 'libcrypto'; then
        echo "FAIL: $binary unexpectedly depends on libcrypto" >&2
        exit 1
    fi
    bytes=$(wc -c <"$WORK_ROOT/$binary" | tr -d '[:space:]')
    case "$bytes" in
        ''|*[!0-9]*) echo "FAIL: cannot measure $binary" >&2; exit 1 ;;
    esac
    [ "$bytes" -le 65536 ] || {
        echo "FAIL: $binary unexpectedly exceeds 64 KiB ($bytes bytes)" >&2
        exit 1
    }
    case "$binary" in
        dcentos-receipt-core-arm) core_bytes=$bytes ;;
        dcentos-receipt-parser-arm) parser_bytes=$bytes ;;
        dcentos-receipt-store-arm) store_bytes=$bytes ;;
        dcentos-receipt-storage-arm) storage_bytes=$bytes ;;
        dcentos-receipt-projection-arm) projection_bytes=$bytes ;;
    esac
done

echo "dcentos-receipt exact Zynq cross-compile proof: ARM EABI5/glibc, no libcrypto, core=${core_bytes}B parser=${parser_bytes}B store=${store_bytes}B storage=${storage_bytes}B projection=${projection_bytes}B stripped"
