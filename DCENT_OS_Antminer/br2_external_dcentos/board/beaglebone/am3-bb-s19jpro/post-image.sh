#!/bin/sh
#
# DCENTos post-image script - am3-bb-s19jpro safe SD-card payload staging.
#
# Provenance:
#   Derived from `board/beaglebone/am3-bb/post-image.sh` (renamed and
#   re-tagged for the s19jpro variant). Produces a staging directory
#   and tarball only. It never writes NAND or a host block device.
#
# Wave reference: AGENT B3 wave W10.x (2026-05-09).
#
# NAND install/revert is intentionally disabled until dated live
# /proc/mtd evidence exists for the AM335x BB S19j Pro carrier (per
# ). The output of
# this script is meant to be inspected by an operator and hand-imaged
# onto SD media outside the post-image hook.

set -e

BOARD_NAME="am3-bb-s19jpro"
OUT_DIR="${BINARIES_DIR}/dcentos-${BOARD_NAME}-sdcard"
ROOTFS_CPIO="${BINARIES_DIR}/rootfs.cpio.gz"
MKIMAGE="${HOST_DIR:-}/bin/mkimage"

if [ ! -x "$MKIMAGE" ]; then
    MKIMAGE="$(command -v mkimage 2>/dev/null || true)"
fi

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

if [ -f "$ROOTFS_CPIO" ]; then
    cp "$ROOTFS_CPIO" "$OUT_DIR/uramdisk.image.gz"
else
    echo "ERROR: rootfs.cpio.gz missing; enable BR2_TARGET_ROOTFS_CPIO_GZIP." >&2
    exit 1
fi

if [ -z "$MKIMAGE" ] || [ ! -x "$MKIMAGE" ]; then
    echo "ERROR: mkimage missing; enable BR2_PACKAGE_HOST_UBOOT_TOOLS." >&2
    exit 1
fi

"$MKIMAGE" \
    -A arm \
    -O linux \
    -T ramdisk \
    -C gzip \
    -n "DCENT_OS ${BOARD_NAME} initramfs" \
    -d "$ROOTFS_CPIO" \
    "$OUT_DIR/ramdisk.gz" >/dev/null

"$MKIMAGE" -l "$OUT_DIR/ramdisk.gz" | grep -q 'ARM Linux RAMDisk Image' || {
    echo "ERROR: generated ramdisk.gz is not a U-Boot ARM legacy ramdisk image" >&2
    exit 1
}

cat > "$OUT_DIR/README.txt" <<'EOF'
DCENT_OS am3-bb-s19jpro SD-card payload

Status: native AM3 BB mining SD-card payload.

This directory is intentionally not a NAND flasher. Add verified AM335x boot
artifacts (MLO, u-boot.img, uImage, am335x-s19jpro.dtb or stock Bitmain DTB)
from a lab-approved stock restore bundle, then write a raw SD image outside
this post-image hook. Do not finalize AM335x media with Windows format/copy;
MLO must remain the first real FAT file entry.

Boot media must use ramdisk.gz, the U-Boot legacy ramdisk wrapper around the
raw CPIO. uramdisk.image.gz is kept for inspection and recovery only.
No ext2 rootfs is part of the AM3-BB SD-first boot path.

NAND install/revert is disabled until SD recovery, full NAND backup, and
restore-to-stock are proven on this exact AM335x BB lane.

Cold boot starts dcentrald with --am3-bb-mining, rescue SSH enabled, and MCP
on localhost:3000. ASIC UART mining uses kernel ttyS1/ttyS2/ttyS4 at 115200 by
default; FastUART remains lab-only. DO NOT include uart_trans.ko, stock cgminer,
monitor-ipsig, or daemons in the SD payload.
EOF

# --- CE-204: canonical Ed25519 signature sidecars (MANIFEST.json + MANIFEST.sig
# + release_ed25519.pub + SHA256SUMS) like the zynq sysupgrade path. The tar is a
# deliberately NOT-NAND-installable SD payload (package_type "sdcard_payload"),
# so it must NOT claim the sysupgrade schema. Same fail-closed signing posture as
# every other board: a bare build with no key and no explicit lab override fails.
PROJECT_ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_package_common.sh"

read_first_nonempty_line() {
    sed -n 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/!{p;q;}' "$1"
}
PACKAGE_VERSION=""
if [ -n "${DCENT_PACKAGE_VERSION:-}" ]; then
    PACKAGE_VERSION="${DCENT_PACKAGE_VERSION}"
else
    for candidate in \
        "${TARGET_DIR:-}/etc/dcentos-version" \
        "${PROJECT_ROOT}/br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/dcentos-version" \
        "${PROJECT_ROOT}/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-version"
    do
        if [ -n "$candidate" ] && [ -f "$candidate" ]; then
            PACKAGE_VERSION=$(read_first_nonempty_line "$candidate")
            [ -n "$PACKAGE_VERSION" ] && break
        fi
    done
fi
if [ -z "$PACKAGE_VERSION" ]; then
    echo "ERROR: unable to infer package version; set DCENT_PACKAGE_VERSION or ship /etc/dcentos-version" >&2
    exit 1
fi
case "$PACKAGE_VERSION" in
    *[!A-Za-z0-9._+:-]*)
        echo "ERROR: package version contains unsupported characters: $PACKAGE_VERSION" >&2
        exit 1
        ;;
esac

BOARD_FAMILY="am3-bb"
SUP_DIR="$OUT_DIR"
DCENT_SDCARD_TAR_PREFIX="dcentos-${BOARD_NAME}-sdcard"
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-management_bringup_sdcard_only}"
export DCENT_PACKAGE_STATUS

: > "$SUP_DIR/SHA256SUMS"
DCENT_SDCARD_PAYLOAD_BLOCK=""
for payload in uramdisk.image.gz ramdisk.gz README.txt; do
    [ -f "$SUP_DIR/$payload" ] || continue
    p_size=$(stat -c%s "$SUP_DIR/$payload" 2>/dev/null || stat -f%z "$SUP_DIR/$payload")
    p_sha=$(sha256sum "$SUP_DIR/$payload" | awk '{print $1}')
    echo "${p_sha}  ${payload}" >> "$SUP_DIR/SHA256SUMS"
    sep=""
    [ -n "$DCENT_SDCARD_PAYLOAD_BLOCK" ] && sep=","
    DCENT_SDCARD_PAYLOAD_BLOCK="${DCENT_SDCARD_PAYLOAD_BLOCK}${sep}
    \"${payload}\": {
      \"path\": \"${DCENT_SDCARD_TAR_PREFIX}/${payload}\",
      \"size\": ${p_size},
      \"sha256\": \"${p_sha}\"
    }"
done
export DCENT_SDCARD_PAYLOAD_BLOCK DCENT_SDCARD_TAR_PREFIX

dcent_stage_release_key
dcent_write_sdcard_payload_manifest
dcent_sign_sysupgrade_manifest

(cd "$BINARIES_DIR" && tar cf "dcentos-${BOARD_NAME}-sdcard.tar" "dcentos-${BOARD_NAME}-sdcard")
echo "DCENTos post-image (am3-bb-s19jpro): staged $BINARIES_DIR/dcentos-${BOARD_NAME}-sdcard.tar"
