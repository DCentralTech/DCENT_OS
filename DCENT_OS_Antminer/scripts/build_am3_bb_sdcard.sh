#!/bin/sh
#
# Build/stage a safe am3-bb SD-card payload.
#
# This helper refuses to write block devices. It wraps Buildroot output and
# verified boot artifacts into a tarball that an operator can inspect before
# imaging an SD card.

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
            echo "Usage: $0 --output dcentos-am3-bb-sdcard.tar [--artifacts /path/to/boot-artifacts]"
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
        echo "ERROR: refusing direct block-device/raw-image output for first am3-bb boot: $OUTPUT" >&2
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
BUILDROOT_DIR="${BUILDROOT_DIR:-$PROJECT_ROOT/buildroot}"
BR2_EXTERNAL="$PROJECT_ROOT/br2_external_dcentos"

if [ -d "$BUILDROOT_DIR" ] && [ -f "$BUILDROOT_DIR/Makefile" ] && command -v make >/dev/null 2>&1; then
    make -C "$BUILDROOT_DIR" BR2_EXTERNAL="$BR2_EXTERNAL" dcentos_am3_bb_defconfig
    make -C "$BUILDROOT_DIR"
elif command -v docker >/dev/null 2>&1; then
    OUTDIR=$(dirname "$OUTPUT")
    "$SCRIPT_DIR/build_in_docker.sh" --target am3-bb --output-dir "$OUTDIR"
    DOCKER_OUTPUT="$OUTDIR/dcentos-am3-bb-sdcard.tar"
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
STAGE="${TMPDIR:-/tmp}/dcentos-am3-bb-sdcard.$$"
rm -rf "$STAGE"
mkdir -p "$STAGE"

if [ -f "$BINARIES_DIR/rootfs.cpio.gz" ]; then
    cp "$BINARIES_DIR/rootfs.cpio.gz" "$STAGE/uramdisk.image.gz"
elif [ -f "$BINARIES_DIR/dcentos-am3-bb-sdcard/uramdisk.image.gz" ]; then
    cp "$BINARIES_DIR/dcentos-am3-bb-sdcard/uramdisk.image.gz" "$STAGE/uramdisk.image.gz"
else
    echo "ERROR: no rootfs.cpio.gz/uramdisk.image.gz found in $BINARIES_DIR" >&2
    exit 1
fi

if [ -n "$ARTIFACT_DIR" ]; then
    for f in MLO u-boot.img uImage am335x-boneblack.dtb bitmain-am335x.dtb; do
        [ -f "$ARTIFACT_DIR/$f" ] && cp "$ARTIFACT_DIR/$f" "$STAGE/$f"
    done
fi

cat > "$STAGE/MANIFEST.txt" <<EOF
DCENT_OS am3-bb SD-card payload
Created: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Rootfs: uramdisk.image.gz
Status: management/bring-up SD-card only
Safety: no NAND writes; no stock cgminer; no uart_trans.ko
NAND: disabled until dated live /proc/mtd evidence exists
EOF

(cd "$STAGE" && tar cf "$OUTPUT" .)
rm -rf "$STAGE"
echo "Wrote $OUTPUT"
