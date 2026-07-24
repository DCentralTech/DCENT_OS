#!/bin/sh
#
# DCENTos post-image script - am3-s21xp (S21 Amlogic NoPic variant)
#
# Produces dcentos-sysupgrade-am3-s21xp.tar with sysupgrade-am3-s21xp/.
#

set -e

BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"
BOARD_NAME="am3-s21xp"
BOARD_FAMILY="am3"
OUTPUT_TAR="${BINARIES_DIR}/dcentos-sysupgrade-am3-s21xp.tar"

echo "=== DCENTos Post-Image Builder (am3-s21xp) ==="
echo ""

ROOTFS_CPIO="${BINARIES_DIR}/rootfs.cpio.gz"
if [ ! -f "$ROOTFS_CPIO" ]; then
    echo "ERROR: rootfs.cpio.gz not found in ${BINARIES_DIR}" >&2
    echo "  Enable BR2_TARGET_ROOTFS_CPIO=y + BR2_TARGET_ROOTFS_CPIO_GZIP=y" >&2
    echo "  in the am3-s21xp defconfig." >&2
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
    -n "DCENT_OS S21 rootfs" \
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
        "${PROJECT_ROOT}/br2_external_dcentos/board/amlogic/am3-s21xp/rootfs-overlay/etc/dcentos-version" \
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

KERNEL=""
KERNEL_SRC=""
if [ -n "${DCENT_AM3_AML_KERNEL:-}" ] && [ -f "${DCENT_AM3_AML_KERNEL}" ]; then
    KERNEL="${DCENT_AM3_AML_KERNEL}"
    KERNEL_SRC="env override (DCENT_AM3_AML_KERNEL)"
elif [ -f "${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin" ]; then
    KERNEL="${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin"
    KERNEL_SRC="docker-staged extractions/s21"
elif [ -f "${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin" ]; then
    KERNEL="${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin"
    KERNEL_SRC="knowledge-base/extractions/s21"
fi

if [ -z "$KERNEL" ]; then
    echo "ERROR: no S21 kernel_uimage.bin found for am3-s21xp sysupgrade packaging." >&2
    echo "  Expected one of:" >&2
    echo "    \$DCENT_AM3_AML_KERNEL" >&2
    echo "    ${PROJECT_ROOT}/extractions/s21/kernel_uimage.bin" >&2
    echo "    ${REPO_ROOT}/knowledge-base/extractions/s21/kernel_uimage.bin" >&2
    echo "  Refusing to package S21 without a verified S21 kernel." >&2
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
Kernel: Amlogic 4.9.113 (am3-aml / S21 NoPic)
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
    "install_command": "dcent install <ip> -f dcentos-sysupgrade-am3-s21xp.tar --artifact-dir <restore_verified_dir>",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade-am3-s21xp.tar --artifact-dir <restore_verified_dir>",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": false
  }
}
EOF

# Final manifest/signature rewrite through shared AM2/AM3 helper. AM3 packages
# are sysupgrade-shaped artifacts for host-driven rootfs-window tooling only.
DCENT_TOOLBOX_INSTALL_COMMAND="dcent install <ip> -f dcentos-sysupgrade-am3-s21xp.tar --artifact-dir <restore_verified_dir>"
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
=== DCENTos am3-s21xp Build Info ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Board:      ${BOARD_NAME} (S21 Amlogic am3-aml NoPic variant)
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
  scripts/build_amlogic_native_install.sh --variant s21
  output/dcentos-amlogic-s21.bin
  mtd=${DCENT_AM3_ROOTFS_MTD}
  rootfs_offset=${DCENT_AM3_ROOTFS_OFFSET_HEX}
  rootfs_window=${DCENT_AM3_ROOTFS_WINDOW_HEX}

Target fleet:
  s21-135 (Amlogic am3-aml, BM1368) - host-driven lab install target only
EOF

echo ""
echo "=== Build Complete (am3-s21xp) ==="
