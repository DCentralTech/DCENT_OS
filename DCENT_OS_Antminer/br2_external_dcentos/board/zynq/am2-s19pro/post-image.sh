#!/bin/sh
#
# DCENTos post-image script — am2-s19pro (S19 / S19 Pro Zynq am2, BM1398)
# D-Central Technologies, 2026
#
#  Phase 2D (2026-05-15) — clone of
# board/zynq/am2-s19jpro/post-image.sh retargeted for the BM1398 / S19 Pro
# hashboard family. Produces a sysupgrade tarball with the
# "sysupgrade-am2-s19pro/" prefix that DCENT_OS sysupgrade and BraiinsOS
# web UI both understand. Mirrors the behavior of
# DCENT_OS_Antminer/scripts/package_sysupgrade.sh but scoped to the
# am2-s19pro board so Buildroot emits the tarball directly without a
# second shell invocation.
#
# Output files produced in $BINARIES_DIR:
#   - rootfs.squashfs   (from Buildroot)
#   - kernel            (staged from knowledge-base extractions)
#   - dcentos-sysupgrade-am2-s19pro.tar   (our product)
#   - BUILD_INFO.txt
#
# Signing: if DCENT_RELEASE_SIGNING_KEY is set, sign MANIFEST.json. Otherwise
# emit an unsigned lab-only package (WARN). Matches S9 / am2-s19jpro behavior.
#
# INPUTS THIS SCAFFOLD DOES NOT YET RESOLVE (Phase 3 wiring):
#   - Kernel source: a verified am2-s19 kernel is required. This script
#     refuses to package an S9 placeholder into a flashable am2 sysupgrade.
#     S19 Pro `a lab unit` cold-boot mining was proven 2026-04-10 but only via
#     /tmp overlay — a baked BM1398 image still needs a verified kernel.
#   - FPGA bitstream: am2 uses Braiins-authored s9-io-am2 (2022-12-08) in
#     mtd2/mtd3 — shared with am2-s19jpro. This script stages it from
#      when present. Missing = build
#     continues with a WARN (the rootfs is still valid for deploy-only
#     workflows).
#
# CRITICAL — :
#   BOARD_NAME must be "am2-s19pro" (NOT "am2-s19j" and NOT "am2-s19jpro").
#   The dcentos_am2_s19pro_defconfig deliberately uses board_target
#   "am2-s19pro" so a BM1398-only baked image is distinct from the BM1362
#   am2-s19jpro image. Wrong name = brick on flash.
#
set -e

BOARD_DIR="$(dirname "$0")"
BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"

BOARD_NAME="am2-s19pro"
BOARD_FAMILY="am2"
OUTPUT_TAR="${BINARIES_DIR}/dcentos-sysupgrade-am2-s19pro.tar"

echo "=== DCENTos Post-Image Builder (am2-s19pro) ==="
echo ""

# -----------------------------------------------------------------------------
# Locate the rootfs produced by Buildroot
# -----------------------------------------------------------------------------
ROOTFS="${BINARIES_DIR}/rootfs.squashfs"
if [ ! -f "$ROOTFS" ]; then
    echo "ERROR: rootfs.squashfs not found in ${BINARIES_DIR}" >&2
    echo "  Enable BR2_TARGET_ROOTFS_SQUASHFS=y in the am2-s19pro defconfig." >&2
    exit 1
fi

ROOTFS_SIZE=$(stat -c%s "$ROOTFS")
ROOTFS_SHA256=$(sha256sum "$ROOTFS" | awk '{print $1}')
echo "Rootfs:  $(basename "$ROOTFS") ($((ROOTFS_SIZE / 1024)) KB)"
echo "  SHA256: ${ROOTFS_SHA256}"

# -----------------------------------------------------------------------------
# Locate a kernel for the am2-s19pro sysupgrade package.
# Probe order (most-specific to least):
#   1. $DCENT_AM2_S19PRO_KERNEL (env override — Phase 3 will set this)
#   2. <repo>/   (am2 shared)
#   3. <repo>/
#   S9 fallback is deliberately banned for am2 production packages.
# -----------------------------------------------------------------------------
# BR2_EXTERNAL_DCENTOS_PATH = DCENT_OS_Antminer/br2_external_dcentos
# PROJECT_ROOT              = DCENT_OS_Antminer
# REPO_ROOT                 = DCENT Projects
PROJECT_ROOT="$(cd "${BR2_EXTERNAL_DCENTOS_PATH}/.." && pwd)"
REPO_ROOT="$(cd "${PROJECT_ROOT}/../.." && pwd)"
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_package_common.sh"
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_archive_admission.sh"
ZYNQ_GEOMETRY_HELPER="${PROJECT_ROOT}/scripts/lib/sysupgrade_zynq_geometry.sh"
[ -r "$ZYNQ_GEOMETRY_HELPER" ] || {
    echo "ERROR: canonical Zynq geometry helper is missing: $ZYNQ_GEOMETRY_HELPER" >&2
    exit 1
}
# shellcheck source=/dev/null
. "$ZYNQ_GEOMETRY_HELPER"
dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" rootfs "$ROOTFS_SIZE" || exit 1
echo "Geometry: $(dcent_zynq_geometry_receipt "$BOARD_NAME")"

read_first_nonempty_line() {
    sed -n 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/!{p;q;}' "$1"
}

infer_package_version() {
    if [ -n "${DCENT_PACKAGE_VERSION:-}" ]; then
        printf '%s\n' "${DCENT_PACKAGE_VERSION}"
        return 0
    fi

    for candidate in \
        "${TARGET_DIR:-}/etc/dcentos-version" \
        "${PROJECT_ROOT}/br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/etc/dcentos-version" \
        "${PROJECT_ROOT}/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-version"
    do
        if [ -n "$candidate" ] && [ -f "$candidate" ]; then
            value=$(read_first_nonempty_line "$candidate")
            if [ -n "$value" ]; then
                printf '%s\n' "$value"
                return 0
            fi
        fi
    done

    return 1
}

PACKAGE_VERSION=$(infer_package_version || true)
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
echo "Version: ${PACKAGE_VERSION}"

KERNEL=""
if [ -n "${DCENT_AM2_S19PRO_KERNEL:-}" ] && [ -f "${DCENT_AM2_S19PRO_KERNEL}" ]; then
    KERNEL="${DCENT_AM2_S19PRO_KERNEL}"
    KERNEL_SRC="env override"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin"
    KERNEL_SRC="knowledge-base/extractions/s19j"
elif [ -f "${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin"
    KERNEL_SRC="knowledge-base/research/s19j/live-probe-139"
fi

if [ -z "$KERNEL" ]; then
    echo "ERROR: no kernel.bin found for am2-s19pro sysupgrade packaging." >&2
    echo "  Expected one of:" >&2
    echo "    \$DCENT_AM2_S19PRO_KERNEL" >&2
    echo "    ${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin" >&2
    echo "    ${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin" >&2
    echo "  Refusing to package am2-s19pro with an S9 kernel placeholder." >&2
    exit 1
fi

cp "$KERNEL" "${BINARIES_DIR}/kernel"
KERNEL_SIZE=$(stat -c%s "${BINARIES_DIR}/kernel")
KERNEL_SHA256=$(sha256sum "${BINARIES_DIR}/kernel" | awk '{print $1}')
dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" kernel "$KERNEL_SIZE" || exit 1
echo "Kernel:  kernel ($((KERNEL_SIZE / 1024)) KB) from ${KERNEL_SRC}"
echo "  SHA256: ${KERNEL_SHA256}"

# -----------------------------------------------------------------------------
# Optional FPGA bitstream staging (for future native-first flows).
# mtd2/mtd3 were byte-identical on the .139 probe and shared with
# am2-s19jpro. If we have an extracted am2 bitstream we include it in the
# manifest so Phase 3 can decide whether to flash it alongside the rootfs.
# -----------------------------------------------------------------------------
BITSTREAM=""
if [ -n "${DCENT_AM2_S19PRO_BITSTREAM:-}" ] && [ -f "${DCENT_AM2_S19PRO_BITSTREAM}" ]; then
    BITSTREAM="${DCENT_AM2_S19PRO_BITSTREAM}"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s19j/fpga_bitstream.bit" ]; then
    BITSTREAM="${REPO_ROOT}/knowledge-base/extractions/s19j/fpga_bitstream.bit"
fi

# -----------------------------------------------------------------------------
# Stage sysupgrade-am2-s19pro/ and build tarball
# -----------------------------------------------------------------------------
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
SUP_DIR="$STAGING/sysupgrade-${BOARD_NAME}"
mkdir -p "$SUP_DIR"

cp "${BINARIES_DIR}/kernel" "$SUP_DIR/kernel"
cp "$ROOTFS"                "$SUP_DIR/root"

if [ -n "$BITSTREAM" ]; then
    cp "$BITSTREAM" "$SUP_DIR/fpga_bitstream.bit"
    BITSTREAM_SIZE=$(stat -c%s "$SUP_DIR/fpga_bitstream.bit")
    BITSTREAM_SHA256=$(sha256sum "$SUP_DIR/fpga_bitstream.bit" | awk '{print $1}')
    echo "Bitstream: fpga_bitstream.bit ($((BITSTREAM_SIZE / 1024)) KB)"
    echo "  SHA256:  ${BITSTREAM_SHA256}"
fi

# METADATA file (OpenWrt convention)
cat > "$SUP_DIR/METADATA" << EOF
DCENT_OS
D-Central Technologies
Build: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board: ${BOARD_NAME}
Kernel: BraiinsOS 4.4.x (am2 / S19 Pro Zynq variant, BM1398)
Rootfs: DCENTos (Buildroot)
EOF
METADATA_SHA256=$(sha256sum "$SUP_DIR/METADATA" | awk '{print $1}')
METADATA_SIZE=$(stat -c%s "$SUP_DIR/METADATA")

# SHA256SUMS
{
    echo "${KERNEL_SHA256}  kernel"
    echo "${ROOTFS_SHA256}  root"
    echo "${METADATA_SHA256}  METADATA"
    [ -n "$BITSTREAM" ] && echo "${BITSTREAM_SHA256}  fpga_bitstream.bit"
} > "$SUP_DIR/SHA256SUMS"

# MANIFEST.json (matches package_sysupgrade.sh schema 1)
BITSTREAM_BLOCK=""
if [ -n "$BITSTREAM" ]; then
    BITSTREAM_BLOCK=",
    \"bitstream\": {
      \"path\": \"sysupgrade-${BOARD_NAME}/fpga_bitstream.bit\",
      \"size\": ${BITSTREAM_SIZE},
      \"sha256\": \"${BITSTREAM_SHA256}\"
    }"
fi

cat > "$SUP_DIR/MANIFEST.json" << EOF
{
  "schema": 1,
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sysupgrade",
  "board_family": "${BOARD_FAMILY}",
  "board": "${BOARD_NAME}",
  "board_target": "${BOARD_NAME}",
  "version": "${PACKAGE_VERSION}",
  "created_at_utc": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "payloads": {
    "kernel": {
      "path": "sysupgrade-${BOARD_NAME}/kernel",
      "size": ${KERNEL_SIZE},
      "sha256": "${KERNEL_SHA256}"
    },
    "rootfs": {
      "path": "sysupgrade-${BOARD_NAME}/root",
      "size": ${ROOTFS_SIZE},
      "sha256": "${ROOTFS_SHA256}"
    },
    "metadata": {
      "path": "sysupgrade-${BOARD_NAME}/METADATA",
      "sha256": "${METADATA_SHA256}"
    }${BITSTREAM_BLOCK}
  },
  "toolbox": {
    "install_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s19pro.tar --artifact-dir <restore_verified_dir> --accept-am2-persistent-lab --i-have-recovery",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s19pro.tar --artifact-dir <restore_verified_dir> --accept-am2-persistent-lab --i-have-recovery",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": true
  }
}
EOF

# Final manifest/signature rewrite through shared AM2/AM3 helper. This keeps
# the emitted package schema aligned even if the legacy scaffold above changes.
DCENT_EXTRA_PAYLOAD_BLOCK=""
if [ -n "$BITSTREAM" ]; then
    DCENT_EXTRA_PAYLOAD_BLOCK=",
    \"bitstream\": {
      \"path\": \"sysupgrade-${BOARD_NAME}/fpga_bitstream.bit\",
      \"size\": ${BITSTREAM_SIZE},
      \"sha256\": \"${BITSTREAM_SHA256}\"
    }"
fi
DCENT_TOOLBOX_INSTALL_COMMAND="dcent install <ip> -f dcentos-sysupgrade-am2-s19pro.tar --artifact-dir <restore_verified_dir> --accept-am2-persistent-lab --i-have-recovery"
DCENT_TOOLBOX_UPDATE_COMMAND="$DCENT_TOOLBOX_INSTALL_COMMAND"
DCENT_TOOLBOX_REQUIRES_INACTIVE_SLOT=true
DCENT_TOOLBOX_INSTALL_MODE=target_sysupgrade
DCENT_TARGET_SIDE_SYSUPGRADE=true
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-unvalidated_target_sysupgrade}"
dcent_stage_release_key
dcent_write_sysupgrade_manifest
dcent_sign_sysupgrade_manifest

# Build tarball
(cd "$STAGING" && tar cf "$OUTPUT_TAR" "sysupgrade-${BOARD_NAME}/")
dcent_sysupgrade_archive_admit "$OUTPUT_TAR" "$BOARD_NAME" "$STAGING" || {
    echo "ERROR: generated sysupgrade archive failed canonical admission" >&2
    exit 1
}
OUTPUT_SIZE=$(stat -c%s "$OUTPUT_TAR")
OUTPUT_SHA256=$(sha256sum "$OUTPUT_TAR" | awk '{print $1}')

echo ""
echo "Sysupgrade tarball:"
echo "  Path:   ${OUTPUT_TAR}"
echo "  Size:   $((OUTPUT_SIZE / 1024)) KB"
echo "  SHA256: ${OUTPUT_SHA256}"
echo ""
tar tf "$OUTPUT_TAR" | sed 's/^/  /'

# -----------------------------------------------------------------------------
# Build info file
# -----------------------------------------------------------------------------
cat > "${BINARIES_DIR}/BUILD_INFO.txt" << EOF
=== DCENTos am2-s19pro Build Info ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board:      ${BOARD_NAME} (S19 / S19 Pro Zynq am2 variant, BM1398)
Board family: ${BOARD_FAMILY}

Sysupgrade tarball:
  File:   $(basename "${OUTPUT_TAR}")
  Size:   ${OUTPUT_SIZE} bytes
  SHA256: ${OUTPUT_SHA256}

Kernel: ${KERNEL_SRC}
  Size:   ${KERNEL_SIZE} bytes
  SHA256: ${KERNEL_SHA256}

Rootfs: rootfs.squashfs
  Size:   ${ROOTFS_SIZE} bytes
  SHA256: ${ROOTFS_SHA256}

Flash (via miner-side sysupgrade):
  scp -O $(basename "${OUTPUT_TAR}") root@<miner_ip>:/tmp/
  ssh root@<miner_ip> 'sysupgrade --test /tmp/$(basename "${OUTPUT_TAR}")'
  ssh root@<miner_ip> 'sysupgrade /tmp/$(basename "${OUTPUT_TAR}")'

Target fleet:
  s19pro-129 (Zynq am2, BM1398) — Phase 3 target (cold-boot mining proven
  2026-04-10 via /tmp overlay; baked-image live flash still pending)
EOF

echo ""
echo "=== Build Complete (am2-s19pro) ==="
