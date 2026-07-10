#!/bin/sh
#
# DCENTos post-image script - am3-bb safe SD-card payload staging.
#
# Produces a staging directory and tarball only. It never writes NAND or a host
# block device.

set -e

BOARD_NAME="am3-bb"
OUT_DIR="${BINARIES_DIR}/dcentos-${BOARD_NAME}-sdcard"
ROOTFS_CPIO="${BINARIES_DIR}/rootfs.cpio.gz"
ROOTFS_EXT2="${BINARIES_DIR}/rootfs.ext2"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

if [ -f "$ROOTFS_CPIO" ]; then
    cp "$ROOTFS_CPIO" "$OUT_DIR/uramdisk.image.gz"
else
    echo "ERROR: rootfs.cpio.gz missing; enable BR2_TARGET_ROOTFS_CPIO_GZIP." >&2
    exit 1
fi

[ -f "$ROOTFS_EXT2" ] && cp "$ROOTFS_EXT2" "$OUT_DIR/rootfs.ext2"

cat > "$OUT_DIR/README.txt" <<'EOF'
DCENT_OS am3-bb SD-card payload

Status: management/bring-up SD-card payload only.

This directory is intentionally not a NAND flasher. Add verified AM335x boot
artifacts (MLO, u-boot.img, uImage, am335x-boneblack.dtb or stock Bitmain DTB)
from a lab-approved stock restore bundle, then write the SD card outside this
post-image hook.

NAND install/revert is disabled until dated live /proc/mtd evidence exists.
Do not include stock cgminer or uart_trans.ko.
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
        "${PROJECT_ROOT}/br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/dcentos-version" \
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
for payload in uramdisk.image.gz rootfs.ext2 README.txt; do
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
echo "DCENTos post-image (am3-bb): staged $BINARIES_DIR/dcentos-${BOARD_NAME}-sdcard.tar"
