#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
BUILD_ROOT=${BUILD_ROOT:-$ROOT_DIR/build-matrix}
DIST_ROOT=${DIST_ROOT:-$ROOT_DIR/dist}
INCLUDE_INTERNAL_TARGETS=${INCLUDE_INTERNAL_TARGETS:-0}
PACKAGE_SCRIPT="$SCRIPT_DIR/package-firmware.sh"

export CC_xtensa_esp32s3_espidf=${CC_xtensa_esp32s3_espidf:-xtensa-esp32s3-elf-gcc}

cd "$ROOT_DIR"

build_one() {
    feature=$1
    board_target=$2
    cargo_target_dir="$BUILD_ROOT/$board_target"
    release_dir="$cargo_target_dir/xtensa-esp32s3-espidf/release"

    printf '%s\n' "==> Building $board_target ($feature)"
    CARGO_TARGET_DIR="$cargo_target_dir" cargo build --locked --release -p dcentaxe --no-default-features --features "$feature"

    printf '%s\n' "==> Packaging $board_target"
    TARGET_DIR="$release_dir" BOARD_TARGET="$board_target" OUT_DIR="$DIST_ROOT/$board_target" "$PACKAGE_SCRIPT"
}

# Default public release matrix: exactly the six DCENT Toolbox install targets.
build_one bitaxe-max bitaxe-max
build_one bitaxe-ultra bitaxe-ultra
build_one bitaxe-supra bitaxe-supra
build_one bitaxe-gamma bitaxe-gamma
build_one bitaxe-hex-ultra bitaxe-hex-ultra
build_one bitaxe-hex-supra bitaxe-hex-supra

if [ "$INCLUDE_INTERNAL_TARGETS" = "1" ]; then
    build_one bitaxe-gamma-duo bitaxe-gamma-duo
    build_one bitaxe-gt bitaxe-gt
    build_one bitaxe-touch bitaxe-touch
    build_one bitaxe-gt-touch bitaxe-gt-touch
    build_one nerdnos nerdnos
    build_one nerdaxe nerdaxe
    build_one nerdqaxe-plus nerdqaxe-plus
    build_one nerdqaxe-pp nerdqaxe-pp
    build_one dcent-axe-bm1397 dcent-axe-bm1397
    build_one dcent-axe-quad-bm1397 dcent-axe-quad-bm1397
    build_one dcent-axe-hex-bm1397 dcent-axe-hex-bm1397
fi
