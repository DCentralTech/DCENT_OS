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
KERNEL_IMG="${BINARIES_DIR}/uImage"
PROJECT_ROOT="$(cd "${BR2_EXTERNAL_DCENTOS_PATH}/.." && pwd)"
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-runtime-only-no-fleet-unit}"
DCENT_BUILD_TARGET="${DCENT_BUILD_TARGET:-$BOARD_NAME}"
export DCENT_PACKAGE_STATUS DCENT_BUILD_TARGET
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_package_common.sh"
dcent_release_provenance_init

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

if [ ! -f "$ROOTFS_CPIO" ]; then
    echo "ERROR (cv1835-s19jpro post-image): required deployable rootfs missing at $ROOTFS_CPIO" >&2
    echo "safe_sysupgrade_cv_emmc.sh accepts rootfs.gz only; refusing an unusable ext2/rootfs-only package." >&2
    exit 1
fi
cp "$ROOTFS_CPIO" "$OUT_DIR/rootfs.gz"

. "${PROJECT_ROOT}/scripts/lib/rootfs_ownership_ledger.sh"
dcent_emit_rootfs_ownership_ledger \
    "$ROOTFS_CPIO" \
    "${BINARIES_DIR}/rootfs-ownership.json" \
    --post-build-script "board=${BR2_EXTERNAL_DCENTOS_PATH}/board/cvitek/cv1835-s19jpro/post-build.sh" \
    --post-build-script "common-prune=${BR2_EXTERNAL_DCENTOS_PATH}/board/common/prune-runtime-research-tools.sh" \
    --overlay-root "amlogic-base=${BR2_EXTERNAL_DCENTOS_PATH}/board/amlogic/rootfs-overlay" \
    --overlay-root "cv1835-s19jpro=${BR2_EXTERNAL_DCENTOS_PATH}/board/cvitek/cv1835-s19jpro/rootfs-overlay"

if [ ! -f "$KERNEL_IMG" ]; then
    echo "ERROR (cv1835-s19jpro post-image): required deployable kernel missing at $KERNEL_IMG" >&2
    echo "safe_sysupgrade_cv_emmc.sh requires uImage; refusing an unusable rootfs-only package." >&2
    exit 1
fi
cp "$KERNEL_IMG" "$OUT_DIR/uImage"

# Per-payload SHA256 (consumed by safe_sysupgrade_cv_emmc.sh pre-write).
command -v sha256sum > /dev/null 2>&1 || {
    echo "ERROR (cv1835-s19jpro post-image): sha256sum is required" >&2
    exit 1
}
sha256sum "$OUT_DIR/uImage" | awk '{print $1}' > "$OUT_DIR/uImage.sha256"
sha256sum "$OUT_DIR/rootfs.gz" | awk '{print $1}' > "$OUT_DIR/rootfs.gz.sha256"
KERNEL_SHA256=$(cat "$OUT_DIR/uImage.sha256")
ROOTFS_SHA256=$(cat "$OUT_DIR/rootfs.gz.sha256")
KERNEL_SIZE=$(wc -c < "$OUT_DIR/uImage" | tr -d '[:space:]')
ROOTFS_SIZE=$(wc -c < "$OUT_DIR/rootfs.gz" | tr -d '[:space:]')

# Minimal manifest (real signing happens via the global OTA key path; this
# is just the per-board envelope).
cat > "$OUT_DIR/MANIFEST.json" <<EOF
{
    "schema": 1,
    "product": "DCENT_OS",
    "package_type": "sysupgrade",
    "board_target": "${BOARD_NAME}",
    "platform_family": "cv1835",
    "created_at_utc": "${DCENT_CREATED_AT_UTC}",
    "status": "${DCENT_PACKAGE_STATUS}",
    "provenance": {
        "source_commit": "${DCENT_SOURCE_COMMIT}",
        "source_tree_state": "${DCENT_SOURCE_TREE_STATE}",
        "source_date_epoch": ${SOURCE_DATE_EPOCH},
        "source_commit_epoch": ${DCENT_SOURCE_COMMIT_EPOCH},
        "build_target": "${DCENT_BUILD_TARGET}",
        "build_arch": "${DCENT_BUILD_ARCH}",
        "toolchain_id": "${DCENT_TOOLCHAIN_ID}"
    },
    "payloads": {
        "kernel": {
            "path": "dcentos-${BOARD_NAME}-sysupgrade/uImage",
            "size": ${KERNEL_SIZE},
            "sha256": "${KERNEL_SHA256}"
        },
        "rootfs": {
            "path": "dcentos-${BOARD_NAME}-sysupgrade/rootfs.gz",
            "size": ${ROOTFS_SIZE},
            "sha256": "${ROOTFS_SHA256}"
        }
    },
    "legacy_payload_sha256": {
        "uImage": "${KERNEL_SHA256}",
        "rootfs.gz": "${ROOTFS_SHA256}"
    },
    "device_target": {
        "kernel_part": "/dev/mmcblk0p1",
        "rootfs_part": "/dev/mmcblk0p3"
    }
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
# canonical kernel/rootfs payload objects mapped to uImage/rootfs.gz), so the
# manifest must not be overwritten. Unsigned lab packages stay possible (no key
# + DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1) but are labeled by the helper.
SUP_DIR="$OUT_DIR"
# CV1835 is a runtime-only lab board; default its package status to the same
# non-release lab string the manifest carries so the shared signing helper's
# release/lab policy resolves correctly without demanding a production key.
dcent_stage_release_key
dcent_sign_sysupgrade_manifest

dcent_create_deterministic_tar \
    "${BINARIES_DIR}/dcentos-sysupgrade-${BOARD_NAME}.tar" \
    "$BINARIES_DIR" \
    "dcentos-${BOARD_NAME}-sysupgrade"
echo "DCENTos post-image (${BOARD_NAME}): staged $BINARIES_DIR/dcentos-sysupgrade-${BOARD_NAME}.tar"
