#!/bin/sh
# Prove seed-entropy builds with the exact production Zynq userspace ABI.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE=$PROJECT_ROOT/br2_external_dcentos/packages/seed-entropy/src/seed-entropy.c
TOOLCHAIN_NAME=gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz
TOOLCHAIN_ARCHIVE=$PROJECT_ROOT/buildroot/dl/toolchain-external-custom/$TOOLCHAIN_NAME
INPUT_MANIFEST=$SCRIPT_DIR/build_inputs.manifest
WORK_ROOT=${TMPDIR:-/tmp}/dcentos-seed-entropy-cross-test.$$

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

for tool in awk file find grep head sha256sum strings tar xz; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FAIL: missing seed-entropy cross-compile proof tool: $tool" >&2
        exit 1
    }
done

[ -f "$SOURCE" ] || {
    echo "FAIL: seed-entropy source is missing: $SOURCE" >&2
    exit 1
}
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

OUTPUT=$WORK_ROOT/seed-entropy-arm
"$CC" -std=c99 -Wall -Wextra -Werror -pedantic -Os \
    "$SOURCE" -o "$OUTPUT"
"$STRIP" "$OUTPUT"

file "$OUTPUT" | grep -Eq 'ELF 32-bit.*ARM' || {
    echo "FAIL: seed-entropy output is not a 32-bit ARM ELF" >&2
    file "$OUTPUT" >&2
    exit 1
}
"$READELF" -h "$OUTPUT" | grep -Eq 'Machine:[[:space:]]+ARM' || {
    echo "FAIL: seed-entropy ELF header does not declare the ARM machine ABI" >&2
    exit 1
}
"$READELF" -h "$OUTPUT" | grep -Eq \
    'Flags:.*Version5 EABI' || {
    echo "FAIL: seed-entropy ELF does not declare the required ARM EABI5 ABI" >&2
    exit 1
}
"$READELF" -A "$OUTPUT" | grep -Eq \
    'Tag_ABI_VFP_args:[[:space:]]+VFP registers' || {
    echo "FAIL: seed-entropy ELF does not use the production hard-float calling convention" >&2
    exit 1
}
if strings "$OUTPUT" | grep -q 'SEED_ENTROPY_TEST_'; then
    echo "FAIL: seed-entropy production ARM binary contains test hooks" >&2
    exit 1
fi

bytes=$(wc -c < "$OUTPUT" | tr -d '[:space:]')
echo "PASS: exact Zynq seed-entropy cross compile (${bytes}B stripped ARM ELF)"
