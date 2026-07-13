#!/bin/sh
# DCENTos post-image script
# Produces TWO outputs:
#   1. rootfs.squashfs — LEGACY compatibility artifact for BraiinsOS UBI replacement
#   2. ramdisk.itb    — LEGACY research artifact for stock NAND FIT ramdisk path
set -e

BOARD_DIR="$(dirname $0)"
BINARIES_DIR="${BINARIES_DIR:-${BASE_DIR}/images}"
MKIMAGE="${HOST_DIR}/bin/mkimage"

hex_to_bin() {
    python3 -c 'import binascii, sys; sys.stdout.buffer.write(binascii.unhexlify(sys.stdin.read().strip()))'
}

echo "=== DCENTos Post-Image Builder ==="
echo ""

# ============================================================================
# LEGACY COMPATIBILITY OUTPUT: rootfs.squashfs (BraiinsOS UBI path)
# ============================================================================
echo "--- Compatibility Output: rootfs.squashfs (BraiinsOS UBI) ---"
if [ -f "${BINARIES_DIR}/rootfs.squashfs" ]; then
    SQFS_SIZE=$(stat -c%s "${BINARIES_DIR}/rootfs.squashfs")
    SQFS_SHA256=$(sha256sum "${BINARIES_DIR}/rootfs.squashfs" | awk '{print $1}')
    echo "  Size:   ${SQFS_SIZE} bytes ($((SQFS_SIZE / 1024)) KB)"
    echo "  SHA256: ${SQFS_SHA256}"

    # 2026-06-05 live S9 .135 evidence: BraiinsOS ubi0_1/rootfs is
    # 134 LEBs * 124 KiB = 17,014,784 bytes. Keep this as a warning
    # threshold only; live flash gates still validate the target geometry
    # before writing.
    MAX_UBI_SIZE=$((134 * 124 * 1024))
    if [ "${SQFS_SIZE}" -gt "${MAX_UBI_SIZE}" ]; then
        echo "  WARNING: rootfs.squashfs exceeds live S9 UBI rootfs volume!"
        echo "  Direct BraiinsOS UBI replacement will be blocked; reduce packages or increase squashfs compression."
    else
        HEADROOM=$(( (MAX_UBI_SIZE - SQFS_SIZE) * 100 / MAX_UBI_SIZE ))
        echo "  UBI fit: OK (${HEADROOM}% headroom within live S9 rootfs volume)"
    fi

    . "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/rootfs_ownership_ledger.sh"
    dcent_emit_rootfs_ownership_ledger \
        "${BINARIES_DIR}/rootfs.squashfs" \
        "${BINARIES_DIR}/rootfs-ownership.json" \
        --post-build-script "board=${BR2_EXTERNAL_DCENTOS_PATH}/board/zynq/post-build.sh" \
        --post-build-script "common-prune=${BR2_EXTERNAL_DCENTOS_PATH}/board/common/prune-runtime-research-tools.sh" \
        --overlay-root "zynq-base=${BR2_EXTERNAL_DCENTOS_PATH}/board/zynq/rootfs-overlay"
else
    echo "  WARNING: rootfs.squashfs not found (squashfs not enabled in defconfig?)"
fi
echo ""

# ============================================================================
# LEGACY RESEARCH OUTPUT: ramdisk.itb (stock NAND path)
# ============================================================================
echo "--- Research Output: ramdisk.itb (stock NAND) ---"
if [ -f "${BINARIES_DIR}/rootfs.ext2" ]; then
    # Compress rootfs
    echo "  Compressing rootfs with gzip -9..."
    ROOTFS_SIZE=$(stat -c%s "${BINARIES_DIR}/rootfs.ext2")
    gzip -9 -f "${BINARIES_DIR}/rootfs.ext2"
    GZ_SIZE=$(stat -c%s "${BINARIES_DIR}/rootfs.ext2.gz")
    echo "  Uncompressed: $((ROOTFS_SIZE / 1024 / 1024)) MB"
    echo "  Compressed:   $((GZ_SIZE / 1024 / 1024)) MB ($((GZ_SIZE / 1024)) KB)"

    # Copy FIT image source
    cp "${BOARD_DIR}/ramdisk-fit.its" "${BINARIES_DIR}/"

    # Build FIT image
    echo "  Building FIT image (ramdisk.itb)..."
    cd "${BINARIES_DIR}"
    "${MKIMAGE}" -f ramdisk-fit.its ramdisk.itb
    ITB_SIZE=$(stat -c%s "${BINARIES_DIR}/ramdisk.itb")
    echo "  FIT image: $((ITB_SIZE / 1024 / 1024)) MB ($((ITB_SIZE / 1024)) KB)"

    # Compute SHA256 for NAND signature patching
    RAMDISK_SHA256=$(sha256sum "${BINARIES_DIR}/ramdisk.itb" | awk '{print $1}')
    echo "  SHA256: ${RAMDISK_SHA256}"

    # Generate 256-byte signature file (32 bytes hash + 224 bytes zero padding)
    {
        printf '%s' "${RAMDISK_SHA256}" | hex_to_bin
        dd if=/dev/zero bs=1 count=224 2>/dev/null
    } > "${BINARIES_DIR}/ramdisk.sig"
else
    echo "  Skipped (rootfs.ext2 not found — ext4 may not be enabled in defconfig)"
    ROOTFS_SIZE="N/A"
    GZ_SIZE="N/A"
    ITB_SIZE="N/A"
    RAMDISK_SHA256="N/A"
fi
echo ""

# ============================================================================
# Build Info
# ============================================================================
cat > "${BINARIES_DIR}/BUILD_INFO.txt" << EOF
=== DCENTos Hacker Shell Build Info ===
Build date: $(date -u +"%Y-%m-%d %H:%M:%S UTC")

=== LEGACY COMPATIBILITY: BraiinsOS UBI Path ===
File: rootfs.squashfs
Size: ${SQFS_SIZE:-N/A} bytes
SHA256: ${SQFS_SHA256:-N/A}
Flash command:
  make flash-braiinsos MINER_IP=203.0.113.97
  (or: scp -O rootfs.squashfs root@MINER:/tmp/ && ssh root@MINER "ubiupdatevol /dev/ubi0_1 /tmp/rootfs.squashfs")
Status: lab/compatibility only — not the primary tester/support flow
Revert: sysupgrade with BraiinsOS, or recovery partition (hold reset on power-on)

=== LEGACY RESEARCH: Stock NAND Path ===
File: ramdisk.itb
Rootfs (uncompressed): ${ROOTFS_SIZE} bytes
Rootfs (compressed): ${GZ_SIZE} bytes
FIT image: ${ITB_SIZE} bytes
SHA256: ${RAMDISK_SHA256}
Flash: flash_erase /dev/mtd1 0 0 && nandwrite -p /dev/mtd1 /tmp/ramdisk.itb
Signature: patch mtd3 offset 1024-1279 with ramdisk.sig

=== Boot Result ===
After flash, miner boots with:
  - BraiinsOS kernel 4.4.0-xilinx (preserved)
  - BraiinsOS FPGA bitstream (14 UIO devices, preserved)
  - DCENTos runtime/rootfs (our Buildroot userspace)
  - SSH bootstrap: root / dcentral
  - Dashboard/API: owner password must be set on first boot
  - Hostname: dcentos
  - Serial console: ttyPS0 @ 115200

Current S9 tester truth:
  - Preferred path today is standalone SD boot for clean DCENT_OS runtime bring-up.
  - Remaining BraiinsOS reuse on Zynq is boot-chain-level, not the intended runtime contract.
EOF

echo "=== Build Complete ==="
echo ""
echo "Output files:"
if [ -f "${BINARIES_DIR}/rootfs.squashfs" ]; then
    echo "  ${BINARIES_DIR}/rootfs.squashfs   - Legacy BraiinsOS UBI compatibility artifact"
fi
if [ -f "${BINARIES_DIR}/ramdisk.itb" ]; then
    echo "  ${BINARIES_DIR}/ramdisk.itb       - Legacy stock NAND research artifact"
    echo "  ${BINARIES_DIR}/ramdisk.sig       - SHA256 for mtd3 patch"
fi
echo "  ${BINARIES_DIR}/BUILD_INFO.txt    - Build info and flash instructions"
echo ""
echo "=== Legacy Compatibility Flash (BraiinsOS S9) ==="
echo "  make flash-braiinsos MINER_IP=203.0.113.97   # lab/compatibility only"
echo ""
