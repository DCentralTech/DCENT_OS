#!/bin/sh
#
# DCENTos post-image script — am2-s19jpro
# D-Central Technologies, 2026
#
# UNTESTED 2026-04-20 Phase 2 initial scaffold — first build will be in Phase 3.
#
# Produces a sysupgrade tarball with the "sysupgrade-am2-s19j/" prefix that
# DCENT_OS sysupgrade and BraiinsOS web UI both understand. Mirrors the
# behavior of DCENT_OS_Antminer/scripts/package_sysupgrade.sh but scoped to
# the am2-s19j board so Buildroot emits the tarball directly without a
# second shell invocation.
#
# Output files produced in $BINARIES_DIR:
#   - rootfs.squashfs   (from Buildroot)
#   - kernel            (staged from knowledge-base extractions)
#   - dcentos-sysupgrade-am2-s19jpro.tar   (our product)
#   - BUILD_INFO.txt
#
# Signing: if DCENT_RELEASE_SIGNING_KEY is set, sign MANIFEST.json. Otherwise
# emit an unsigned lab-only package (WARN). Matches S9 behavior.
#
# INPUTS THIS SCAFFOLD DOES NOT YET RESOLVE (Phase 3 wiring):
#   - Kernel source: a verified am2-s19j kernel is required. This script
#     refuses to package an S9 placeholder into a flashable am2 sysupgrade.
#   - FPGA bitstream: am2 uses Braiins-authored s9-io-am2 (2022-12-08) in
#     mtd2/mtd3. This script stages it from
#     when present. Missing = build continues with a WARN (the rootfs is still
#     valid for deploy-only workflows).
#
# CRITICAL — :
#   BOARD_NAME must be "am2-s19j" (not "am2-s19jpro" and not "am2-s17").
#   Wrong name = brick on flash.
#
set -e

BOARD_DIR="$(dirname "$0")"
BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"

BOARD_NAME="am2-s19j"
BOARD_FAMILY="am2"
OUTPUT_TAR="${BINARIES_DIR}/dcentos-sysupgrade-am2-s19jpro.tar"

echo "=== DCENTos Post-Image Builder (am2-s19jpro) ==="
echo ""

# -----------------------------------------------------------------------------
# Locate the rootfs produced by Buildroot
# -----------------------------------------------------------------------------
ROOTFS="${BINARIES_DIR}/rootfs.squashfs"
if [ ! -f "$ROOTFS" ]; then
    echo "ERROR: rootfs.squashfs not found in ${BINARIES_DIR}" >&2
    echo "  Enable BR2_TARGET_ROOTFS_SQUASHFS=y in the am2-s19jpro defconfig." >&2
    exit 1
fi

ROOTFS_SIZE=$(stat -c%s "$ROOTFS")
ROOTFS_SHA256=$(sha256sum "$ROOTFS" | awk '{print $1}')
echo "Rootfs:  $(basename "$ROOTFS") ($((ROOTFS_SIZE / 1024)) KB)"
echo "  SHA256: ${ROOTFS_SHA256}"

# -----------------------------------------------------------------------------
# Locate a kernel for the am2-s19j sysupgrade package.
# Probe order (most-specific to least):
#   1. $DCENT_AM2_S19J_KERNEL (env override — Phase 3 will set this)
#   2. <repo>/
#   3. <repo>/
#   S9 fallback is deliberately banned for am2 production packages.
# -----------------------------------------------------------------------------
# BR2_EXTERNAL_DCENTOS_PATH = DCENT_OS_Antminer/br2_external_dcentos
# PROJECT_ROOT              = DCENT_OS_Antminer
# REPO_ROOT                 = DCENT Projects
PROJECT_ROOT="$(cd "${BR2_EXTERNAL_DCENTOS_PATH}/.." && pwd)"
REPO_ROOT="$(cd "${PROJECT_ROOT}/../.." && pwd)"
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_package_common.sh"

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
        "${PROJECT_ROOT}/br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/etc/dcentos-version" \
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

DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-unvalidated_target_sysupgrade}"
DCENT_BUILD_TARGET="${DCENT_BUILD_TARGET:-am2-s19jpro}"
dcent_release_provenance_init
CANONICAL_BUILD_TIME=$(printf '%s' "$DCENT_CREATED_AT_UTC" | sed 's/T/ /; s/Z/ UTC/')

. "${PROJECT_ROOT}/scripts/lib/rootfs_ownership_ledger.sh"
dcent_emit_rootfs_ownership_ledger \
    "$ROOTFS" \
    "${BINARIES_DIR}/rootfs-ownership.json" \
    --post-build-script "board=${BR2_EXTERNAL_DCENTOS_PATH}/board/zynq/am2-s19jpro/post-build.sh" \
    --post-build-script "common-prune=${BR2_EXTERNAL_DCENTOS_PATH}/board/common/prune-runtime-research-tools.sh" \
    --overlay-root "zynq-base=${BR2_EXTERNAL_DCENTOS_PATH}/board/zynq/rootfs-overlay" \
    --overlay-root "am2-s19jpro=${BR2_EXTERNAL_DCENTOS_PATH}/board/zynq/am2-s19jpro/rootfs-overlay"

KERNEL=""
if [ -n "${DCENT_AM2_S19J_KERNEL:-}" ] && [ -f "${DCENT_AM2_S19J_KERNEL}" ]; then
    KERNEL="${DCENT_AM2_S19J_KERNEL}"
    KERNEL_SRC="env override"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin"
    KERNEL_SRC="knowledge-base/extractions/s19j"
elif [ -f "${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin"
    KERNEL_SRC="knowledge-base/research/s19j/live-probe-139"
fi

if [ -z "$KERNEL" ]; then
    echo "ERROR: no kernel.bin found for am2-s19j sysupgrade packaging." >&2
    echo "  Expected one of:" >&2
    echo "    \$DCENT_AM2_S19J_KERNEL" >&2
    echo "    ${REPO_ROOT}/knowledge-base/extractions/s19j/kernel.bin" >&2
    echo "    ${REPO_ROOT}/knowledge-base/research/s19j/live-probe-139/kernel.bin" >&2
    echo "  Refusing to package am2-s19j with an S9 kernel placeholder." >&2
    exit 1
fi

cp "$KERNEL" "${BINARIES_DIR}/kernel"
KERNEL_SIZE=$(stat -c%s "${BINARIES_DIR}/kernel")
KERNEL_SHA256=$(sha256sum "${BINARIES_DIR}/kernel" | awk '{print $1}')
echo "Kernel:  kernel ($((KERNEL_SIZE / 1024)) KB) from ${KERNEL_SRC}"
echo "  SHA256: ${KERNEL_SHA256}"

# -----------------------------------------------------------------------------
# Optional FPGA bitstream staging (for future native-first flows).
# mtd2/mtd3 were byte-identical on the .139 probe. If we have an extracted
# am2 bitstream we include it in the manifest so Phase 3 can decide whether
# to flash it alongside the rootfs.
# -----------------------------------------------------------------------------
BITSTREAM=""
if [ -n "${DCENT_AM2_S19J_BITSTREAM:-}" ] && [ -f "${DCENT_AM2_S19J_BITSTREAM}" ]; then
    BITSTREAM="${DCENT_AM2_S19J_BITSTREAM}"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s19j/fpga_bitstream.bit" ]; then
    BITSTREAM="${REPO_ROOT}/knowledge-base/extractions/s19j/fpga_bitstream.bit"
fi

# -----------------------------------------------------------------------------
# Stage sysupgrade-am2-s19j/ and build tarball
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
Build: ${CANONICAL_BUILD_TIME}
Board: ${BOARD_NAME}
Kernel: BraiinsOS 4.4.x (am2-s17 / S19j Pro Zynq variant)
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
  "created_at_utc": "${DCENT_CREATED_AT_UTC}",
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
    "install_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": true
  }
}
EOF

# Optional signing (mirrors package_sysupgrade.sh)
if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ] && [ -f "${DCENT_RELEASE_SIGNING_KEY}" ]; then
    if command -v openssl >/dev/null 2>&1; then
        PUBKEY="$STAGING/release_ed25519.pub"
        openssl pkey -in "${DCENT_RELEASE_SIGNING_KEY}" -pubout -out "$PUBKEY" >/dev/null 2>&1 || true
        if [ -f "$PUBKEY" ]; then
            cp "$PUBKEY" "$SUP_DIR/release_ed25519.pub"
            openssl pkeyutl -sign -rawin \
                -inkey "${DCENT_RELEASE_SIGNING_KEY}" \
                -in "$SUP_DIR/MANIFEST.json" \
                -out "$SUP_DIR/MANIFEST.sig" \
                && echo "Signed MANIFEST.json" \
                || echo "WARNING: failed to sign MANIFEST.json (package will be unsigned)"
        fi
    else
        echo "WARNING: openssl not available — package will be unsigned"
    fi
else
    echo "WARNING: no signing key configured — package is unsigned (lab-only)"
fi

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
DCENT_TOOLBOX_INSTALL_COMMAND="dcent install <ip> -f dcentos-sysupgrade-am2-s19jpro.tar"
DCENT_TOOLBOX_UPDATE_COMMAND="$DCENT_TOOLBOX_INSTALL_COMMAND"
DCENT_TOOLBOX_REQUIRES_INACTIVE_SLOT=true
DCENT_TOOLBOX_INSTALL_MODE=target_sysupgrade
DCENT_TARGET_SIDE_SYSUPGRADE=true
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-unvalidated_target_sysupgrade}"

# Brick-safety PSU-config declaration (2026-05-16). post-build.sh bakes the
# G1-proven dcentrald_s19jpro_xil_override.toml ([power.psu_override]
# enabled=true, model=APW3, voltage_v=12.8 — the .109 milestone path with the
# Loki board installed) as /etc/dcentrald/xil_override.toml AND stamps
# /etc/dcentos/psu_config = "override". Surface the same mode in the
# sysupgrade MANIFEST.json so the toolbox XIL install gate G5 sees a matching
# manifest hint (declared --psu-config=override). G5 BLOCKS an install whose
# declared PSU config disagrees with this — that block is the brick-safety
# gate, NOT something to bypass. See xil_install_gates.py + post-build.sh.
DCENT_PSU_CONFIG_MODE=override
export DCENT_PSU_CONFIG_MODE

dcent_stage_release_key
dcent_write_sysupgrade_manifest
dcent_sign_sysupgrade_manifest

# Build tarball
dcent_create_deterministic_tar "$OUTPUT_TAR" "$STAGING" "sysupgrade-${BOARD_NAME}"
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
=== DCENTos am2-s19jpro Build Info ===
Build date: ${CANONICAL_BUILD_TIME}
Board:      ${BOARD_NAME} (S19j Pro AM2 Zynq lane)
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
  s19jpro-139 (Zynq am2-s17) — Phase 3 target
EOF

echo ""
echo "=== Build Complete (am2-s19jpro) ==="
