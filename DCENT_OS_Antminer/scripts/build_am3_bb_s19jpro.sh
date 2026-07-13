#!/bin/sh
#
# build_am3_bb_s19jpro.sh - Build/stage the am3-bb-s19jpro variant
# sysupgrade tarball.
#
# Provenance: derived from `build_am3_bb_sdcard.sh`. Wave reference:
# AGENT B3 wave W10.x (2026-05-09).
#
# Routes through `build_in_docker.sh --target am3-bb-s19jpro`, which selects
# `dcentos_am3_bb_s19jpro_defconfig`, the variant
# board/beaglebone/am3-bb-s19jpro post-build.sh + post-image.sh, and
# emits the staged SD-card payload tarball
#   dcentos-am3-bb-s19jpro-sdcard.tar
# (or, when promoted to a NAND/sysupgrade target,
#   dcentos-sysupgrade-am3-bb-s19jpro.tar).
#
# Like build_am3_bb_sdcard.sh, this helper refuses to write block
# devices and outputs only a tar staging payload until live /proc/mtd
# evidence enables NAND install per
# .
#
set -e

OUTPUT=""
ARTIFACT_DIR=""

while [ $# -gt 0 ]; do
    case "$1" in
        --output)
            OUTPUT="$2"
            shift 2
            ;;
        --artifacts)
            ARTIFACT_DIR="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 --output dcentos-sysupgrade-am3-bb-s19jpro.tar [--artifacts /path/to/boot-artifacts]"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

[ -n "$OUTPUT" ] || {
    echo "ERROR: --output is required" >&2
    exit 1
}

case "$OUTPUT" in
    /dev/*|\\\\.\\*|*.img)
        echo "ERROR: refusing direct block-device/raw-image output for am3-bb-s19jpro: $OUTPUT" >&2
        echo "Use a .tar output, inspect it, then image SD media manually with physical recovery available." >&2
        exit 1
        ;;
esac

case "$OUTPUT" in
    /*)
        ;;
    *)
        OUTPUT="$(pwd)/$OUTPUT"
        ;;
esac

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
. "$SCRIPT_DIR/lib/am3_bb_dtb_contract.sh"
BUILDROOT_DIR="${BUILDROOT_DIR:-$PROJECT_ROOT/buildroot}"
BR2_EXTERNAL="$PROJECT_ROOT/br2_external_dcentos"
BUILD_TARGET="am3-bb-s19jpro"
BUILD_DEFCONFIG="dcentos_am3_bb_s19jpro_defconfig"
EXPECTED_TARBALL="dcentos-${BUILD_TARGET}-sdcard.tar"

if [ -d "$BUILDROOT_DIR" ] && [ -f "$BUILDROOT_DIR/Makefile" ] && command -v make >/dev/null 2>&1; then
    make -C "$BUILDROOT_DIR" BR2_EXTERNAL="$BR2_EXTERNAL" "$BUILD_DEFCONFIG"
    make -C "$BUILDROOT_DIR"
elif command -v docker >/dev/null 2>&1; then
    if [ -n "$ARTIFACT_DIR" ]; then
        echo "ERROR: --artifacts is not supported by Docker packaging; refusing to ignore carrier boot artifacts" >&2
        echo "Use a direct Buildroot/staging environment or a dedicated AM3-BB image builder." >&2
        exit 1
    fi
    OUTDIR=$(dirname "$OUTPUT")
    "$SCRIPT_DIR/build_in_docker.sh" --target "$BUILD_TARGET" --output-dir "$OUTDIR"
    DOCKER_OUTPUT="$OUTDIR/$EXPECTED_TARBALL"
    if [ -f "$DOCKER_OUTPUT" ]; then
        if [ "$DOCKER_OUTPUT" != "$OUTPUT" ]; then
            cp "$DOCKER_OUTPUT" "$OUTPUT"
        fi
        echo "Wrote $OUTPUT"
        exit 0
    fi
    echo "ERROR: Docker build did not produce $DOCKER_OUTPUT" >&2
    exit 1
else
    echo "WARN: Buildroot tree or make not found; packaging existing output only." >&2
fi

BINARIES_DIR="${BINARIES_DIR:-$BUILDROOT_DIR/output/images}"
STAGE="${TMPDIR:-/tmp}/dcentos-${BUILD_TARGET}-sdcard.$$"
rm -rf "$STAGE"
mkdir -p "$STAGE"

if [ -f "$BINARIES_DIR/rootfs.cpio.gz" ]; then
    cp "$BINARIES_DIR/rootfs.cpio.gz" "$STAGE/uramdisk.image.gz"
elif [ -f "$BINARIES_DIR/dcentos-${BUILD_TARGET}-sdcard/uramdisk.image.gz" ]; then
    cp "$BINARIES_DIR/dcentos-${BUILD_TARGET}-sdcard/uramdisk.image.gz" "$STAGE/uramdisk.image.gz"
else
    echo "ERROR: no rootfs.cpio.gz/uramdisk.image.gz found in $BINARIES_DIR" >&2
    exit 1
fi

if [ -n "$ARTIFACT_DIR" ]; then
    for f in MLO u-boot.img uImage; do
        [ -f "$ARTIFACT_DIR/$f" ] && cp "$ARTIFACT_DIR/$f" "$STAGE/$f"
    done

    DTB_SOURCE=""
    for f in devicetree.dtb am335x-s19jpro.dtb bitmain-am335x.dtb am335x-boneblack.dtb dtb; do
        if [ -f "$ARTIFACT_DIR/$f" ]; then
            DTB_SOURCE="$ARTIFACT_DIR/$f"
            break
        fi
    done
    [ -n "$DTB_SOURCE" ] || {
        echo "ERROR: --artifacts must contain a carrier-aware AM3-BB DTB" >&2
        exit 1
    }
    dcent_am3_bb_admit_carrier_dtb "$DTB_SOURCE" s19j-io-v2 0
    cp "$DTB_SOURCE" "$STAGE/devicetree.dtb"
fi

cat > "$STAGE/MANIFEST.txt" <<EOF
DCENT_OS am3-bb-s19jpro SD-card payload
Created: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Rootfs: uramdisk.image.gz
Status: management/bring-up SD-card only
Safety: no NAND writes; no stock cgminer; no uart_trans.ko
NAND: disabled until dated live /proc/mtd evidence exists
ASIC UART: routed through userspace serial.rs::DevmemUart (no kernel modules)
EOF

(cd "$STAGE" && tar cf "$OUTPUT" .)
rm -rf "$STAGE"
echo "Wrote $OUTPUT"
