#!/bin/sh
#
# DCENTos post-image script — am3-s19jpro-aml (S19j Pro Amlogic PIC1704 variant)
# D-Central Technologies, Phase 4B (2026-05-15)
#
# Sibling of board/amlogic/am3-s21/post-image.sh. Same overlay-on-overlay
# pattern, same aarch64 toolchain, same uImage+CPIO sysupgrade-shaped tar.
# Differs only in product identity (BM1362 + PIC1704 vs S21's BM1368 + NoPic).
#
# Produces dcentos-sysupgrade-am3-s19jpro-aml.tar with the
# "sysupgrade-am3-s19jpro-aml/" prefix for host-driven package validation
# and rootfs-window install planning. There is no validated target-side AM3
# sysupgrade implementation.
#
# CRITICAL — :
#   BOARD_NAME must be "am3-s19jpro-aml" here. The directory inside the tar
#   MUST be `sysupgrade-am3-s19jpro-aml/`. Wrong name = brick on flash.
#
set -e

BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"
BOARD_NAME="am3-s19jpro-aml"
BOARD_FAMILY="am3"
OUTPUT_TAR="${BINARIES_DIR}/dcentos-sysupgrade-am3-s19jpro-aml.tar"

echo "=== DCENTos Post-Image Builder (am3-s19jpro-aml) ==="
echo ""

ROOTFS_CPIO="${BINARIES_DIR}/rootfs.cpio.gz"
if [ ! -f "$ROOTFS_CPIO" ]; then
    echo "ERROR: rootfs.cpio.gz not found in ${BINARIES_DIR}" >&2
    echo "  Enable BR2_TARGET_ROOTFS_CPIO=y + BR2_TARGET_ROOTFS_CPIO_GZIP=y" >&2
    echo "  in the am3-s19jpro-aml defconfig." >&2
    exit 1
fi

CPIO_SIZE=$(stat -c%s "$ROOTFS_CPIO")
echo "Rootfs:  rootfs.cpio.gz ($((CPIO_SIZE / 1024)) KB)"

ROOTFS_LIST="${BINARIES_DIR}/ROOTFS_FILELIST.txt"
ROOTFS_AUDIT_HITS="${BINARIES_DIR}/ROOTFS_AUDIT_HITS.txt"

if ! gzip -dc "$ROOTFS_CPIO" | cpio -it --quiet | sed 's#^\./##' | sort -u > "$ROOTFS_LIST"; then
    echo "ERROR: failed to list rootfs.cpio.gz for service-surface audit" >&2
    exit 1
fi

require_rootfs_path() {
    if ! grep -qx "$1" "$ROOTFS_LIST"; then
        echo "ERROR: rootfs service-surface audit missing required path: $1" >&2
        exit 1
    fi
}

reject_rootfs_pattern() {
    if grep -Ei "$1" "$ROOTFS_LIST" > "$ROOTFS_AUDIT_HITS"; then
        echo "ERROR: rootfs service-surface audit rejected path(s):" >&2
        sed 's/^/  /' "$ROOTFS_AUDIT_HITS" >&2
        exit 1
    fi
}

require_rootfs_path "etc/init.d/S50dropbear"
require_rootfs_path "etc/init.d/S70ip_reporter"
require_rootfs_path "etc/init.d/S80dashboard"
require_rootfs_path "etc/init.d/S81mcp"
require_rootfs_path "etc/init.d/S82dcentrald"
require_rootfs_path "usr/sbin/dropbear"
require_rootfs_path "usr/local/bin/dcentrald"
require_rootfs_path "root/web/server.py"
require_rootfs_path "root/web/mcp_server.py"
require_rootfs_path "root/web/ip_reporter.py"
require_rootfs_path "uninstall.sh"
require_rootfs_path "etc/dcentos-platform"
require_rootfs_path "etc/fw_env.config"
reject_rootfs_pattern '(^|/)(S50telnet|in\.telnetd|telnetd|telnet)$|telnet'
echo "Rootfs audit: access services present; telnet paths absent"

HOST_MKIMAGE="${HOST_DIR}/bin/mkimage"
if [ ! -x "$HOST_MKIMAGE" ]; then
    HOST_MKIMAGE="$(command -v mkimage 2>/dev/null || true)"
fi
if [ -z "$HOST_MKIMAGE" ] || [ ! -x "$HOST_MKIMAGE" ]; then
    echo "ERROR: mkimage not found in HOST_DIR/bin or PATH" >&2
    echo "  Enable BR2_PACKAGE_HOST_UBOOT_TOOLS=y in the defconfig." >&2
    exit 1
fi

ROOTFS_UIMAGE="${BINARIES_DIR}/uImage_rootfs.bin"
"$HOST_MKIMAGE" \
    -A arm64 \
    -O linux \
    -T ramdisk \
    -C gzip \
    -n "DCENT_OS S19j Pro Amlogic rootfs" \
    -d "$ROOTFS_CPIO" \
    "$ROOTFS_UIMAGE"

ROOTFS_SIZE=$(stat -c%s "$ROOTFS_UIMAGE")
ROOTFS_SHA256=$(sha256sum "$ROOTFS_UIMAGE" | awk '{print $1}')
echo "uImage:  uImage_rootfs.bin ($((ROOTFS_SIZE / 1024)) KB)"
echo "  SHA256: ${ROOTFS_SHA256}"

PROJECT_ROOT="$(cd "${BR2_EXTERNAL_DCENTOS_PATH}/.." && pwd)"
REPO_ROOT="$(cd "${PROJECT_ROOT}/../.." && pwd)"
. "${PROJECT_ROOT}/scripts/lib/sysupgrade_package_common.sh"
. "${PROJECT_ROOT}/scripts/lib/am3_geometry.sh"

case "$ROOTFS_SIZE" in
    ''|*[!0-9]*)
        echo "ERROR: rootfs uImage size is not numeric: $ROOTFS_SIZE" >&2
        exit 1
        ;;
esac
if [ "$ROOTFS_SIZE" -gt "$DCENT_AM3_ROOTFS_WINDOW_DEC" ]; then
    echo "ERROR: rootfs uImage exceeds Amlogic rootfs window: ${ROOTFS_SIZE} > ${DCENT_AM3_ROOTFS_WINDOW_DEC}" >&2
    exit 1
fi
echo "Rootfs window: ${ROOTFS_SIZE} <= ${DCENT_AM3_ROOTFS_WINDOW_DEC} bytes"

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
        "${PROJECT_ROOT}/br2_external_dcentos/board/amlogic/am3-s19jpro-aml/rootfs-overlay/etc/dcentos-version" \
        "${PROJECT_ROOT}/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/dcentos-version" \
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

# Probe order for the am3-aml kernel uImage. Same set the S21 + S19k Pro
# variants use; S19j Pro Amlogic kernel extraction is hardware-gated until
# a bench unit lands. Until then we fall back to the verified-working AXG
# S21 kernel (same A113D SoC, same 4.9.113 base).
KERNEL=""
KERNEL_SRC=""
if [ -n "${DCENT_AM3_AML_KERNEL:-}" ] && [ -f "${DCENT_AM3_AML_KERNEL}" ]; then
    KERNEL="${DCENT_AM3_AML_KERNEL}"
    KERNEL_SRC="env override (DCENT_AM3_AML_KERNEL)"
elif [ -f "${PROJECT_ROOT}/extractions/s19j-aml/kernel_uimage.bin" ]; then
    KERNEL="${PROJECT_ROOT}/extractions/s19j-aml/kernel_uimage.bin"
    KERNEL_SRC="docker-staged extractions/s19j-aml"
elif [ -f "${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin" ]; then
    KERNEL="${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin"
    KERNEL_SRC="docker-staged extractions/s21 (verified AXG fallback)"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s19j-aml/kernel_uimage.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/extractions/s19j-aml/kernel_uimage.bin"
    KERNEL_SRC="knowledge-base/extractions/s19j-aml"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin"
    KERNEL_SRC="knowledge-base/extractions/s21 (verified AXG fallback)"
fi

if [ -z "$KERNEL" ]; then
    echo "ERROR: no kernel_uimage.bin found for am3-s19jpro-aml sysupgrade packaging." >&2
    echo "  Expected one of (in probe order):" >&2
    echo "    \$DCENT_AM3_AML_KERNEL" >&2
    echo "    ${PROJECT_ROOT}/extractions/s19j-aml/kernel_uimage.bin" >&2
    echo "    ${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin" >&2
    echo "    ${REPO_ROOT}/knowledge-base/extractions/s19j-aml/kernel_uimage.bin" >&2
    echo "    ${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin" >&2
    echo "  Refusing to package am3-s19jpro-aml without a verified am3-family kernel." >&2
    exit 1
fi

cp "$KERNEL" "${BINARIES_DIR}/kernel"
KERNEL_SIZE=$(stat -c%s "${BINARIES_DIR}/kernel")
KERNEL_SHA256=$(sha256sum "${BINARIES_DIR}/kernel" | awk '{print $1}')
echo "Kernel:  kernel ($((KERNEL_SIZE / 1024)) KB) from ${KERNEL_SRC}"
echo "  SHA256: ${KERNEL_SHA256}"

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
SUP_DIR="$STAGING/sysupgrade-${BOARD_NAME}"
mkdir -p "$SUP_DIR"

cp "${BINARIES_DIR}/kernel" "$SUP_DIR/kernel"
cp "$ROOTFS_UIMAGE" "$SUP_DIR/root"

cat > "$SUP_DIR/METADATA" << EOF
DCENT_OS
D-Central Technologies
Build: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board: ${BOARD_NAME}
Kernel: Amlogic 4.9.113 (am3-aml / S19j Pro PIC1704 variant)
Rootfs: DCENTos (Buildroot, uImage-wrapped gzip CPIO)
Install contract: host-driven rootfs-window only; target-side AM3 sysupgrade unsupported
EOF
METADATA_SHA256=$(sha256sum "$SUP_DIR/METADATA" | awk '{print $1}')
METADATA_SIZE=$(stat -c%s "$SUP_DIR/METADATA")

{
    echo "${KERNEL_SHA256}  kernel"
    echo "${ROOTFS_SHA256}  root"
    echo "${METADATA_SHA256}  METADATA"
} > "$SUP_DIR/SHA256SUMS"

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
    "install_command": "dcent install <ip> -f dcentos-sysupgrade-am3-s19jpro-aml.tar",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade-am3-s19jpro-aml.tar",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": false
  }
}
EOF

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
        echo "WARNING: openssl not available - package will be unsigned"
    fi
else
    echo "WARNING: no signing key configured - package is unsigned (lab-only)"
fi

# Final manifest/signature rewrite through shared AM2/AM3 helper. AM3 packages
# are sysupgrade-shaped artifacts for host-driven rootfs-window tooling only.
DCENT_TOOLBOX_INSTALL_COMMAND="dcent install <ip> -f dcentos-sysupgrade-am3-s19jpro-aml.tar"
DCENT_TOOLBOX_UPDATE_COMMAND=""
DCENT_TOOLBOX_REQUIRES_INACTIVE_SLOT=false
DCENT_TOOLBOX_INSTALL_MODE=host_driven_rootfs_window_lab
DCENT_TARGET_SIDE_SYSUPGRADE=false
DCENT_PACKAGE_STATUS=host_driven_only
dcent_stage_release_key
dcent_write_sysupgrade_manifest
dcent_sign_sysupgrade_manifest

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

cat > "${BINARIES_DIR}/BUILD_INFO.txt" << EOF
=== DCENTos am3-s19jpro-aml Build Info ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board:      ${BOARD_NAME} (S19j Pro Amlogic am3-aml PIC1704 variant)
Board family: ${BOARD_FAMILY}

Sysupgrade tarball:
  File:   $(basename "${OUTPUT_TAR}")
  Size:   ${OUTPUT_SIZE} bytes
  SHA256: ${OUTPUT_SHA256}

Kernel: ${KERNEL_SRC}
  Size:   ${KERNEL_SIZE} bytes
  SHA256: ${KERNEL_SHA256}

Rootfs (uImage-wrapped gzip CPIO): uImage_rootfs.bin
  Size:   ${ROOTFS_SIZE} bytes
  SHA256: ${ROOTFS_SHA256}

Install contract:
  Host-driven rootfs-window package only. Target-side AM3 sysupgrade is not
  validated or claimed by this package.
  scripts/build_amlogic_native_install.sh --variant s19jpro-aml (not yet shipped)
  output/dcentos-amlogic-s19jpro-aml.bin
  mtd=${DCENT_AM3_ROOTFS_MTD}
  rootfs_offset=${DCENT_AM3_ROOTFS_OFFSET_HEX}
  rootfs_window=${DCENT_AM3_ROOTFS_WINDOW_HEX}

Target fleet:
  (no live S19j Pro Amlogic unit yet; .133 is currently VNish-locked)
EOF

echo ""
echo "=== Build Complete (am3-s19jpro-aml) ==="
