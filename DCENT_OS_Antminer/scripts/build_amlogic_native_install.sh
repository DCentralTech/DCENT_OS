#!/usr/bin/env bash
#
# Build a flashable Amlogic native-install rootfs image from the Buildroot
# sysupgrade package. Destructive flashing remains operator-gated elsewhere.
#
# Usage:
#   scripts/build_amlogic_native_install.sh --variant s19jpro-aml
#   scripts/build_amlogic_native_install.sh --variant s19kpro
#   scripts/build_amlogic_native_install.sh --variant s21
#   scripts/build_amlogic_native_install.sh --variant s21 --lab-unsigned

set -euo pipefail

VARIANT=""
OUTPUT_DIR=""
LAB_UNSIGNED=0
AARCH64_TARGET="aarch64-unknown-linux-musl"

usage() {
    echo "Usage: $(basename "$0") --variant s19jpro-aml|s19kpro|s21 [--output-dir DIR] [--lab-unsigned]" >&2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --variant)
            VARIANT="${2:-}"
            shift 2
            ;;
        --variant=*)
            VARIANT="${1#--variant=}"
            shift
            ;;
        --output-dir)
            OUTPUT_DIR="${2:-}"
            shift 2
            ;;
        --output-dir=*)
            OUTPUT_DIR="${1#--output-dir=}"
            shift
            ;;
        --lab-unsigned)
            LAB_UNSIGNED=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown flag: $1" >&2
            usage
            exit 1
            ;;
    esac
done

[ -n "$VARIANT" ] || { echo "ERROR: missing --variant" >&2; usage; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
. "$SCRIPT_DIR/lib/am3_geometry.sh"
if [ -n "$OUTPUT_DIR" ]; then
    case "$OUTPUT_DIR" in
        /*|[A-Za-z]:*) ;;
        *) OUTPUT_DIR="$PROJECT_DIR/$OUTPUT_DIR" ;;
    esac
else
    OUTPUT_DIR="$PROJECT_DIR/output"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

case "$VARIANT" in
    s19jpro-aml|s19jpro|s19j)
        TARGET="am3-s19jpro-aml"
        BOARD_PKG_NAME="am3-s19jpro-aml"
        TAR_NAME="dcentos-sysupgrade-am3-s19jpro-aml.tar"
        ROOT_MEMBER="sysupgrade-am3-s19jpro-aml/root"
        BIN_NAME="dcentos-amlogic-s19jpro-aml.bin"
        ;;
    s19kpro|s19k)
        TARGET="am3-s19kpro"
        BOARD_PKG_NAME="am3-s19k"
        TAR_NAME="dcentos-sysupgrade-am3-s19kpro.tar"
        ROOT_MEMBER="sysupgrade-am3-s19k/root"
        BIN_NAME="dcentos-amlogic-s19kpro.bin"
        ;;
    s21)
        TARGET="am3-s21"
        BOARD_PKG_NAME="am3-s21"
        TAR_NAME="dcentos-sysupgrade-am3-s21.tar"
        ROOT_MEMBER="sysupgrade-am3-s21/root"
        BIN_NAME="dcentos-amlogic-s21.bin"
        ;;
    *)
        echo "ERROR: unsupported Amlogic variant: $VARIANT (supported: s19jpro-aml, s19kpro, s21)" >&2
        exit 1
        ;;
esac

is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

is_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

if [ "$LAB_UNSIGNED" = "1" ]; then
    export DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1
    export DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-lab_unsigned}"
fi
DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-release}"
if is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" && is_release_status "$DCENT_PACKAGE_STATUS"; then
    echo "ERROR: DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 requires non-release DCENT_PACKAGE_STATUS (for example lab_unsigned)." >&2
    exit 1
fi
export DCENT_ALLOW_UNSIGNED_SYSUPGRADE DCENT_PACKAGE_STATUS

ensure_aarch64_dcentrald() {
    local bin="$PROJECT_DIR/dcentrald/target/$AARCH64_TARGET/release/dcentrald"
    if [ -f "$bin" ]; then
        echo "Using existing dcentrald binary: $bin"
        return 0
    fi

    command -v cargo >/dev/null 2>&1 || {
        echo "ERROR: cargo not found and $bin does not exist" >&2
        exit 1
    }

    echo "Building dcentrald for $AARCH64_TARGET..."
    (
        cd "$PROJECT_DIR/dcentrald"
        if [ -z "${CC_aarch64_unknown_linux_musl:-}" ] && [ -f "zig-cc-aarch64.bat" ]; then
            export CC_aarch64_unknown_linux_musl="$PWD/zig-cc-aarch64.bat"
        fi
        if [ -z "${AR_aarch64_unknown_linux_musl:-}" ] && [ -f "zig-ar-aarch64.bat" ]; then
            export AR_aarch64_unknown_linux_musl="$PWD/zig-ar-aarch64.bat"
        fi
        cargo build --release --target "$AARCH64_TARGET" -p dcentrald
    )
}

echo "=== DCENT_OS Amlogic native-install build ==="
echo "Variant: $VARIANT"
echo "Target:  $TARGET"
echo "Output:  $OUTPUT_DIR/$BIN_NAME"
echo ""

ensure_aarch64_dcentrald
BUILD_ARGS=(--target "$TARGET" --output-dir "$OUTPUT_DIR")
if [ "$LAB_UNSIGNED" = "1" ]; then
    BUILD_ARGS+=(--lab-unsigned)
fi
bash "$SCRIPT_DIR/build_in_docker.sh" "${BUILD_ARGS[@]}"

TARBALL="$OUTPUT_DIR/$TAR_NAME"
[ -f "$TARBALL" ] || { echo "ERROR: expected tarball missing: $TARBALL" >&2; exit 1; }

DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}" \
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-release}" \
    bash "$SCRIPT_DIR/pre_flash_validate.sh" --package-only "$TARBALL" "$BOARD_PKG_NAME"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

tar -xf "$TARBALL" -C "$TMPDIR" "$ROOT_MEMBER"
cp "$TMPDIR/$ROOT_MEMBER" "$OUTPUT_DIR/$BIN_NAME"

ROOT_SIZE=$(stat -c%s "$OUTPUT_DIR/$BIN_NAME" 2>/dev/null || stat -f%z "$OUTPUT_DIR/$BIN_NAME")
case "$ROOT_SIZE" in
    ''|*[!0-9]*) echo "ERROR: extracted rootfs size is not numeric: $ROOT_SIZE" >&2; exit 1 ;;
esac
[ "$ROOT_SIZE" -le "$DCENT_AM3_ROOTFS_WINDOW_DEC" ] || {
    echo "ERROR: extracted rootfs exceeds Amlogic rootfs window: $ROOT_SIZE > $DCENT_AM3_ROOTFS_WINDOW_DEC" >&2
    exit 1
}
ROOT_MAGIC=$(od -An -N4 -tx1 "$OUTPUT_DIR/$BIN_NAME" 2>/dev/null | tr -d ' \n')
[ "$ROOT_MAGIC" = "27051956" ] || {
    echo "ERROR: extracted rootfs is not a uImage payload (magic=$ROOT_MAGIC)" >&2
    exit 1
}
ROOT_SHA=$(sha256sum "$OUTPUT_DIR/$BIN_NAME" | awk '{print $1}')

echo ""
echo "Flashable rootfs image:"
echo "  Path:   $OUTPUT_DIR/$BIN_NAME"
echo "  Size:   $ROOT_SIZE bytes"
echo "  Magic:  $ROOT_MAGIC"
echo "  SHA256: $ROOT_SHA"
echo ""
echo "Live NAND write is intentionally not performed by this build script."
echo "Operator-gated install still requires recovery path, physical access, readback, and reboot proof."
