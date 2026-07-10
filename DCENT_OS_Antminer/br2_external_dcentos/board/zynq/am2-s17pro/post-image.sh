#!/bin/sh
#
# DCENTos post-image script — am2-s17pro (S17 / S17 Pro Zynq am2-s17 variant)
# D-Central Technologies, 2026
#
#  Phase 2E (2026-05-15) — clone of
# board/zynq/am2-s19jpro/post-image.sh adjusted for the BM1397 / S17 Pro
# hashboard family. Produces a sysupgrade tarball with the
# "sysupgrade-am2-s17p/" prefix.
#
# ## RUNTIME-ONLY — NO COLD-BOOT PROOF ########################################
# There is NO live Antminer S17 / S17 Pro on the D-Central fleet, and there is
# NO extracted S17 kernel in the knowledge base. This script therefore CANNOT
# produce a flashable sysupgrade tarball today — it builds the rootfs and, if a
# kernel is supplied via $DCENT_AM2_S17_KERNEL, packages a regression-coverage
# tarball. With no kernel it exits cleanly with a WARN (the squashfs is still
# valid for deploy-only / package-validator workflows). Do NOT claim cold-boot
# proof. See the board README.md.
#
# ## UNCONFIRMED (v2 open question → R11) #####################################
# S17 (BM1396) vs S17 Pro (BM1397) chip-driver dispatch is code-only and never
# live-validated.
#############################################################################
#
# CRITICAL — :
#   BOARD_NAME must be "am2-s17p" (not "am2-s17pro" and not "am2-s19j").
#   Wrong name = brick on flash.
#
set -e

BOARD_DIR="$(dirname "$0")"
BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"

BOARD_NAME="am2-s17p"
BOARD_FAMILY="am2"
OUTPUT_TAR="${BINARIES_DIR}/dcentos-sysupgrade-am2-s17pro.tar"

echo "=== DCENTos Post-Image Builder (am2-s17pro) ==="
echo "    RUNTIME-ONLY scaffold — no live S17 / S17 Pro on the fleet."
echo ""

# -----------------------------------------------------------------------------
# Locate the rootfs produced by Buildroot
# -----------------------------------------------------------------------------
ROOTFS="${BINARIES_DIR}/rootfs.squashfs"
if [ ! -f "$ROOTFS" ]; then
    echo "ERROR: rootfs.squashfs not found in ${BINARIES_DIR}" >&2
    echo "  Enable BR2_TARGET_ROOTFS_SQUASHFS=y in the am2-s17pro defconfig." >&2
    exit 1
fi

ROOTFS_SIZE=$(stat -c%s "$ROOTFS")
ROOTFS_SHA256=$(sha256sum "$ROOTFS" | awk '{print $1}')
echo "Rootfs:  $(basename "$ROOTFS") ($((ROOTFS_SIZE / 1024)) KB)"
echo "  SHA256: ${ROOTFS_SHA256}"

# -----------------------------------------------------------------------------
# Locate a kernel for the am2-s17p sysupgrade package.
# Probe order (most-specific to least):
#   1. $DCENT_AM2_S17_KERNEL (env override)
#   2. <repo>/
#   S9 / s19j fallback is deliberately banned for an am2-s17 production package.
#
# RUNTIME-ONLY reality: there is no extracted S17 kernel in the knowledge base
# (no live unit was ever probed). With no kernel this script emits a WARN and
# exits 0 — the rootfs.squashfs is still produced for package-validator
# regression coverage and deploy-only workflows.
# -----------------------------------------------------------------------------
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
        "${PROJECT_ROOT}/br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/etc/dcentos-version" \
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
if [ -n "${DCENT_AM2_S17_KERNEL:-}" ] && [ -f "${DCENT_AM2_S17_KERNEL}" ]; then
    KERNEL="${DCENT_AM2_S17_KERNEL}"
    KERNEL_SRC="env override"
elif [ -f "${REPO_ROOT}/extractions/s17/kernel.bin" ]; then
    KERNEL="${REPO_ROOT}/extractions/s17/kernel.bin"
    KERNEL_SRC="extractions/s17"
fi

if [ -z "$KERNEL" ]; then
    echo "WARNING: no kernel.bin found for am2-s17p sysupgrade packaging." >&2
    echo "  Expected one of:" >&2
    echo "    \$DCENT_AM2_S17_KERNEL" >&2
    echo "    ${REPO_ROOT}/extractions/s17/kernel.bin" >&2
    echo "" >&2
    echo "  RUNTIME-ONLY: there is no live S17 / S17 Pro on the fleet and no" >&2
    echo "  extracted S17 kernel in the knowledge base. Skipping sysupgrade" >&2
    echo "  tarball — the rootfs.squashfs is still produced for package-" >&2
    echo "  validator regression coverage and deploy-only workflows." >&2
    echo "  An S17 kernel extraction (via a bench unit) is required before a" >&2
    echo "  flashable am2-s17p package can be built. See README.md." >&2
    cat > "${BINARIES_DIR}/BUILD_INFO.txt" << EOF
=== DCENTos am2-s17pro Build Info (RUNTIME-ONLY, NO KERNEL) ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board:      ${BOARD_NAME} (S17 / S17 Pro Zynq am2-s17 variant)
Board family: ${BOARD_FAMILY}
Status:     ROOTFS-ONLY — no S17 kernel available, no sysupgrade tarball.
            No live S17 / S17 Pro on the fleet. BM1396 vs BM1397 chip-driver
            dispatch UNCONFIRMED (v2 open question -> R11).

Rootfs: rootfs.squashfs
  Size:   ${ROOTFS_SIZE} bytes
  SHA256: ${ROOTFS_SHA256}
EOF
    echo ""
    echo "=== Build Complete (am2-s17pro — rootfs only, no flashable package) ==="
    exit 0
fi

cp "$KERNEL" "${BINARIES_DIR}/kernel"
KERNEL_SIZE=$(stat -c%s "${BINARIES_DIR}/kernel")
KERNEL_SHA256=$(sha256sum "${BINARIES_DIR}/kernel" | awk '{print $1}')
echo "Kernel:  kernel ($((KERNEL_SIZE / 1024)) KB) from ${KERNEL_SRC}"
echo "  SHA256: ${KERNEL_SHA256}"

# -----------------------------------------------------------------------------
# Stage sysupgrade-am2-s17p/ and build tarball
# -----------------------------------------------------------------------------
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
SUP_DIR="$STAGING/sysupgrade-${BOARD_NAME}"
mkdir -p "$SUP_DIR"

cp "${BINARIES_DIR}/kernel" "$SUP_DIR/kernel"
cp "$ROOTFS"                "$SUP_DIR/root"

# METADATA file (OpenWrt convention)
cat > "$SUP_DIR/METADATA" << EOF
DCENT_OS
D-Central Technologies
Build: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board: ${BOARD_NAME}
Kernel: BraiinsOS 4.4.x (am2-s17 / S17 / S17 Pro Zynq variant) — RUNTIME-ONLY
Rootfs: DCENTos (Buildroot)
EOF
METADATA_SHA256=$(sha256sum "$SUP_DIR/METADATA" | awk '{print $1}')
METADATA_SIZE=$(stat -c%s "$SUP_DIR/METADATA")

# SHA256SUMS
{
    echo "${KERNEL_SHA256}  kernel"
    echo "${ROOTFS_SHA256}  root"
    echo "${METADATA_SHA256}  METADATA"
} > "$SUP_DIR/SHA256SUMS"

# MANIFEST.json (matches package_sysupgrade.sh schema 1)
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
    }
  },
  "toolbox": {
    "install_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s17pro.tar",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade-am2-s17pro.tar",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": true
  }
}
EOF

# Optional signing (mirrors package_sysupgrade.sh / am2-s19jpro)
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

# Final manifest/signature rewrite through shared AM2/AM3 helper.
DCENT_TOOLBOX_INSTALL_COMMAND="dcent install <ip> -f dcentos-sysupgrade-am2-s17pro.tar"
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
=== DCENTos am2-s17pro Build Info (RUNTIME-ONLY scaffold) ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board:      ${BOARD_NAME} (S17 / S17 Pro Zynq am2-s17 variant)
Board family: ${BOARD_FAMILY}
Status:     NO live S17 / S17 Pro on the fleet. Kernel supplied via
            ${KERNEL_SRC}. BM1396 vs BM1397 chip-driver dispatch UNCONFIRMED
            (v2 open question -> R11). Do NOT flash a live unit until a bench
            S17 / S17 Pro is acquired and accepted-share + round-trip proof
            is captured.

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

Target fleet:
  (none — no live S17 / S17 Pro unit)
EOF

echo ""
echo "=== Build Complete (am2-s17pro — RUNTIME-ONLY) ==="
