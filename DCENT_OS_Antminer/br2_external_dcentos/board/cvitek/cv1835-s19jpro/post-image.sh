#!/bin/sh
#
# DCENTos post-image script - cv1835-s19jpro.
#
# Stages a sysupgrade tarball that the eMMC safe-sysupgrade script can
# consume. Layout matches what safe_sysupgrade_cv_emmc.sh expects:
#
#   sysupgrade-cv1835-s19jpro/
#     uImage              <- new kernel image (raw, eMMC mmcblk0p1 payload)
#     rootfs.gz           <- gzip'd cpio rootfs (eMMC mmcblk0p3 payload)
#     MANIFEST.json       <- board target + payload SHAs
#     uImage.sha256
#     rootfs.gz.sha256
#
# We never write a host block device here. We only assemble the staging
# directory and pack it.

set -e

BOARD_NAME="cv1835-s19jpro"
OUT_DIR="${BINARIES_DIR}/dcentos-${BOARD_NAME}-sysupgrade"
ROOTFS_CPIO="${BINARIES_DIR}/rootfs.cpio.gz"
ROOTFS_EXT2="${BINARIES_DIR}/rootfs.ext2"
KERNEL_IMG="${BINARIES_DIR}/uImage"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

if [ -f "$ROOTFS_CPIO" ]; then
    cp "$ROOTFS_CPIO" "$OUT_DIR/rootfs.gz"
elif [ -f "$ROOTFS_EXT2" ]; then
    # CV1835 also accepts ext2/4 rootfs (mounted from mmcblk0p3 directly).
    cp "$ROOTFS_EXT2" "$OUT_DIR/rootfs.ext2"
else
    echo "ERROR (cv1835-s19jpro post-image): no rootfs found at $ROOTFS_CPIO or $ROOTFS_EXT2" >&2
    exit 1
fi

if [ -f "$KERNEL_IMG" ]; then
    cp "$KERNEL_IMG" "$OUT_DIR/uImage"
else
    echo "WARN (cv1835-s19jpro post-image): no uImage at $KERNEL_IMG; sysupgrade will be rootfs-only" >&2
fi

# Per-payload SHA256 (consumed by safe_sysupgrade_cv_emmc.sh pre-write).
if command -v sha256sum > /dev/null 2>&1; then
    [ -f "$OUT_DIR/uImage" ]    && sha256sum "$OUT_DIR/uImage"    | awk '{print $1}' > "$OUT_DIR/uImage.sha256"
    [ -f "$OUT_DIR/rootfs.gz" ] && sha256sum "$OUT_DIR/rootfs.gz" | awk '{print $1}' > "$OUT_DIR/rootfs.gz.sha256"
fi

# Minimal manifest (real signing happens via the global OTA key path; this
# is just the per-board envelope).
cat > "$OUT_DIR/MANIFEST.json" <<EOF
{
    "board_target": "${BOARD_NAME}",
    "platform_family": "cv1835",
    "payloads": {
        "uImage":     "$( [ -f "$OUT_DIR/uImage.sha256" ]    && cat "$OUT_DIR/uImage.sha256" )",
        "rootfs.gz":  "$( [ -f "$OUT_DIR/rootfs.gz.sha256" ] && cat "$OUT_DIR/rootfs.gz.sha256" )"
    },
    "device_target": {
        "kernel_part": "/dev/mmcblk0p1",
        "rootfs_part": "/dev/mmcblk0p3"
    },
    "status": "runtime-only-no-fleet-unit"
}
EOF

# Drop the U-Boot bootcmd recipe alongside so the operator's first manual
# image-build run captures the recovery semantics in one place.
UBOOT_RECIPE="${BR2_EXTERNAL_DCENTOS_PATH}/board/cvitek/cv1835-s19jpro/uboot-bootcmd.txt"
[ -f "$UBOOT_RECIPE" ] && cp "$UBOOT_RECIPE" "$OUT_DIR/uboot-bootcmd.txt"

# === Release signing (CE-091/CE-287) =========================================
# safe_sysupgrade_cv_emmc.sh now REQUIRES an Ed25519 MANIFEST.sig (verified
# against the pinned /etc/dcentos/release_ed25519.pub) before any eMMC write, so
# the produced package must carry MANIFEST.sig + release_ed25519.pub when a
# signing key is set. We sign the CV1835-specific MANIFEST.json written above
# via the SHARED signing helper (dcent_stage_release_key + the sign/verify pass)
# rather than dcent_write_sysupgrade_manifest — the eMMC consumer reads THIS
# board manifest schema (board_target + platform_family + device_target +
# uImage/rootfs.gz payloads), not the shared kernel/root/metadata schema, so the
# manifest must not be overwritten. Unsigned lab packages stay possible (no key
# + DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1) but are labeled by the helper.
SUP_DIR="$OUT_DIR"
# CV1835 is a runtime-only lab board; default its package status to the same
# non-release lab string the manifest carries so the shared signing helper's
# release/lab policy resolves correctly without demanding a production key.
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-runtime-only-no-fleet-unit}"
export DCENT_PACKAGE_STATUS
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/sysupgrade_package_common.sh"
dcent_stage_release_key
dcent_sign_sysupgrade_manifest

(cd "$BINARIES_DIR" && tar cf "dcentos-sysupgrade-${BOARD_NAME}.tar" "dcentos-${BOARD_NAME}-sysupgrade")
echo "DCENTos post-image (${BOARD_NAME}): staged $BINARIES_DIR/dcentos-sysupgrade-${BOARD_NAME}.tar"
