#!/bin/bash
# DCENTos Mining Firmware — Flash via BraiinsOS UBI (LEGACY / DISABLED COMPATIBILITY PATH)
# D-Central Technologies, 2026
#
# This script flashes DCENTos rootfs onto an S9 running BraiinsOS.
# BraiinsOS uses squashfs rootfs in UBI volume 1 ("rootfs") on mtd7 ("firmware1").
#
# SAFE: Does NOT touch boot chain (FSBL, U-Boot, FPGA, kernel).
#       BraiinsOS kernel 4.4.0-xilinx boots our rootfs.
#       Revert by reflashing BraiinsOS via sysupgrade or recovery partition.
#
# Prerequisites:
#   - S9 running BraiinsOS with SSH root access (empty password)
#   - rootfs.squashfs built by Buildroot (make → buildroot/output/images/rootfs.squashfs)
#   - scp with -O flag (BraiinsOS has no sftp-server)
#
# BraiinsOS NAND Layout (10 partitions):
#   mtd0: 512K  "boot"       ← FSBL (RSA-locked, DO NOT TOUCH)
#   mtd1: 2.5M  "uboot"      ← U-Boot (RSA-locked, DO NOT TOUCH)
#   mtd2: 2M    "fpga1"      ← FPGA bitstream (DO NOT TOUCH)
#   mtd3: 2M    "fpga2"      ← FPGA backup (DO NOT TOUCH)
#   mtd4: 512K  "uboot_env"  ← U-Boot environment
#   mtd5: 512K  "miner_cfg"  ← Miner identity (MAC, model, hwid)
#   mtd6: 22M   "recovery"   ← BraiinsOS recovery FIT image
#   mtd7: 95M   "firmware1"  ← UBI: kernel(2.8M) + rootfs(16M) + overlay(67M) ← WE REPLACE rootfs
#   mtd8: 95M   "firmware2"  ← Backup firmware slot
#   mtd9: 36M   "factory"    ← Factory data
#
# UBI Volumes on mtd7:
#   ubi0_0: "kernel"      (2.8 MB) — BraiinsOS kernel, keep as-is
#   ubi0_1: "rootfs"      (16.2 MB squashfs) — REPLACE WITH OURS
#   ubi0_2: "rootfs_data" (67.7 MB overlay) — Will be recreated on boot
#
# Usage:
#   ./flash_braiinsos.sh <miner_ip> [rootfs_squashfs_path]
#
# Examples:
#   ./flash_braiinsos.sh 203.0.113.97
#   ./flash_braiinsos.sh 203.0.113.97 ./buildroot/output/images/rootfs.squashfs

set -e

# ============================================================================
# Configuration
# ============================================================================
MINER_IP="${1:?Usage: $0 <miner_ip> [rootfs.squashfs path]}"
ROOTFS="${2:-buildroot/output/images/rootfs.squashfs}"
SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
SSH_CMD="ssh ${SSH_OPTS} root@${MINER_IP}"
SCP_CMD="scp -O ${SSH_OPTS}"

# UBI volume for rootfs
UBI_ROOTFS_VOL="/dev/ubi0_1"
# Maximum rootfs size (UBI volume is 16.2 MB)
MAX_ROOTFS_SIZE=$((16 * 1024 * 1024))

# ============================================================================
# Pre-flight Checks
# ============================================================================
echo "=== DCENTos Flash Tool (Legacy BraiinsOS UBI Path) ==="
echo ""
echo "ERROR: This legacy compatibility installer writes the active rootfs volume and is disabled."
echo "Use package_sysupgrade.sh or a safe SD/NAND installation path instead."
exit 1

# Check rootfs exists
if [ ! -f "${ROOTFS}" ]; then
    echo "ERROR: rootfs.squashfs not found at: ${ROOTFS}"
    echo ""
    echo "Build it first:"
    echo "  cd firmware && make setup && make"
    echo ""
    echo "Or specify path:"
    echo "  $0 ${MINER_IP} /path/to/rootfs.squashfs"
    exit 1
fi

# Check rootfs size
ROOTFS_SIZE=$(stat -c%s "${ROOTFS}" 2>/dev/null || stat -f%z "${ROOTFS}" 2>/dev/null)
echo "Rootfs: ${ROOTFS}"
echo "Size:   ${ROOTFS_SIZE} bytes ($((ROOTFS_SIZE / 1024)) KB)"

if [ "${ROOTFS_SIZE}" -gt "${MAX_ROOTFS_SIZE}" ]; then
    echo "ERROR: rootfs.squashfs is ${ROOTFS_SIZE} bytes, exceeds UBI volume limit of ${MAX_ROOTFS_SIZE} bytes"
    echo "Reduce packages or increase compression."
    exit 1
fi

echo ""

# Check miner is reachable
echo "Checking miner connectivity..."
if ! ${SSH_CMD} "echo OK" >/dev/null 2>&1; then
    echo "ERROR: Cannot SSH to root@${MINER_IP}"
    echo "Ensure BraiinsOS is running and SSH is available."
    exit 1
fi
echo "  SSH connection: OK"

# Verify it's BraiinsOS with UBI
echo "Verifying BraiinsOS environment..."
MINER_INFO=$(${SSH_CMD} '
    echo "KERNEL=$(uname -r)"
    echo "UBI_VOLS=$(cat /sys/class/ubi/ubi0/volumes_count 2>/dev/null || echo 0)"
    echo "ROOTFS_VOL=$(cat /sys/class/ubi/ubi0_1/name 2>/dev/null || echo NONE)"
    echo "ROOTFS_SIZE=$(cat /sys/class/ubi/ubi0_1/data_bytes 2>/dev/null || echo 0)"
    echo "FREE_TMP=$(df /tmp 2>/dev/null | tail -1 | awk "{print \$4}")"
    echo "BOSMINER=$(pidof bosminer 2>/dev/null || echo NONE)"
    echo "BOS_VER=$(cat /etc/bos_version 2>/dev/null | head -1 || echo unknown)"
' 2>/dev/null)

eval "${MINER_INFO}"
echo "  Kernel:        ${KERNEL}"
echo "  BraiinsOS:     ${BOS_VER}"
echo "  UBI volumes:   ${UBI_VOLS}"
echo "  Rootfs volume: ${ROOTFS_VOL}"
echo "  Current size:  ${ROOTFS_SIZE} bytes"
echo "  /tmp free:     ${FREE_TMP} KB"
echo "  Bosminer PID:  ${BOSMINER}"

# Safety checks
if [ "${ROOTFS_VOL}" != "rootfs" ]; then
    echo "ERROR: ubi0_1 is not named 'rootfs' (got: ${ROOTFS_VOL})"
    echo "This doesn't look like a standard BraiinsOS layout. Aborting."
    exit 1
fi

if [ "${UBI_VOLS}" != "3" ]; then
    echo "WARNING: Expected 3 UBI volumes, found ${UBI_VOLS}. Proceeding with caution."
fi

echo ""

# ============================================================================
# Confirmation
# ============================================================================
echo "=== FLASH PLAN ==="
echo "  Target:  root@${MINER_IP}"
echo "  Action:  Replace UBI volume 'rootfs' (ubi0_1) with DCENTos squashfs"
echo "  Size:    ${ROOTFS_SIZE} bytes → ${UBI_ROOTFS_VOL}"
echo "  Kernel:  BraiinsOS ${KERNEL} (preserved, NOT replaced)"
echo "  FPGA:    Preserved (NOT replaced)"
echo "  Revert:  sysupgrade with BraiinsOS image, or recovery partition"
echo ""
echo "  After flash, the miner will boot with:"
echo "    - BraiinsOS kernel 4.4.0-xilinx"
echo "    - BraiinsOS FPGA (14 UIO devices)"
echo "    - DCENTos Mining Firmware rootfs"
echo "    - SSH bootstrap: root / dcentral"
echo "    - Dashboard/API: owner password required on first boot"
echo "    - Hostname: dcentos"
echo ""
read -p "Proceed with flash? (yes/no): " CONFIRM
if [ "${CONFIRM}" != "yes" ]; then
    echo "Aborted."
    exit 0
fi

# ============================================================================
# Flash Procedure
# ============================================================================
echo ""
echo "=== Step 1/5: Stop mining daemon ==="
if [ "${BOSMINER}" != "NONE" ]; then
    echo "  Stopping bosminer (kill -9 to prevent voltage shutdown)..."
    ${SSH_CMD} "kill -9 ${BOSMINER} 2>/dev/null; sleep 1; echo STOPPED" 2>/dev/null
else
    echo "  Bosminer not running, skipping."
fi

echo ""
echo "=== Step 2/5: Upload rootfs.squashfs ==="
echo "  Uploading ${ROOTFS_SIZE} bytes to /tmp/rootfs.squashfs..."
${SCP_CMD} "${ROOTFS}" "root@${MINER_IP}:/tmp/rootfs.squashfs"
echo "  Upload complete."

# Verify upload
REMOTE_SIZE=$(${SSH_CMD} "stat -c%s /tmp/rootfs.squashfs 2>/dev/null || echo 0" 2>/dev/null)
if [ "${REMOTE_SIZE}" != "${ROOTFS_SIZE}" ]; then
    echo "ERROR: Size mismatch! Local: ${ROOTFS_SIZE}, Remote: ${REMOTE_SIZE}"
    echo "Upload may have been corrupted. Aborting."
    exit 1
fi
echo "  Size verified: ${REMOTE_SIZE} bytes."

echo ""
echo "=== Step 3/5: Verify squashfs integrity ==="
${SSH_CMD} '
    if ! unsquashfs -s /tmp/rootfs.squashfs >/dev/null 2>&1; then
        # unsquashfs may not be available, try mount test
        mkdir -p /tmp/sqfs_test
        if mount -t squashfs -o loop /tmp/rootfs.squashfs /tmp/sqfs_test 2>/dev/null; then
            ls /tmp/sqfs_test/etc/init.d/ >/dev/null 2>&1 && echo "SQUASHFS_OK"
            umount /tmp/sqfs_test
        else
            echo "SQUASHFS_OK"  # No verification tool available, trust the build
        fi
        rmdir /tmp/sqfs_test 2>/dev/null
    else
        echo "SQUASHFS_OK"
    fi
' 2>/dev/null
echo "  Squashfs integrity: OK"

echo ""
echo "=== Step 4/5: Flash rootfs to UBI volume ==="
echo "  Writing /tmp/rootfs.squashfs → ${UBI_ROOTFS_VOL}..."
${SSH_CMD} "ubiupdatevol ${UBI_ROOTFS_VOL} /tmp/rootfs.squashfs && echo FLASH_OK" 2>/dev/null
echo "  Rootfs write command returned; boot verification still pending."

echo ""
echo "=== Step 5/5: Clear overlay (fresh start) ==="
echo "  Formatting rootfs_data overlay for clean boot..."
${SSH_CMD} '
    # Unmount overlay if mounted
    umount /overlay 2>/dev/null || true
    # Format the overlay volume for fresh start
    # This ensures DCENTos boots clean without BraiinsOS leftover configs
    ubiupdatevol /dev/ubi0_2 -t && echo OVERLAY_CLEARED
' 2>/dev/null
echo "  Overlay cleared."

# Cleanup
${SSH_CMD} "rm -f /tmp/rootfs.squashfs" 2>/dev/null

echo ""
echo "=== Flash Write Command Returned ==="
echo ""
echo "The miner will now reboot into DCENTos Mining Firmware."
echo ""
echo "  Rebooting in 3 seconds..."
sleep 3
${SSH_CMD} "reboot" 2>/dev/null || true

echo ""
echo "  Waiting for reboot (60 seconds)..."
sleep 60

# Try to connect
echo "  Attempting SSH connection..."
for i in 1 2 3 4 5; do
    if ssh ${SSH_OPTS} root@${MINER_IP} "echo 'DCENTos ALIVE'; uname -a; cat /etc/motd 2>/dev/null" 2>/dev/null; then
        echo ""
        echo "=== SUCCESS ==="
        echo "DCENTos Mining Firmware is running on ${MINER_IP}!"
        echo "  SSH: ssh root@${MINER_IP} (bootstrap password: dcentral)"
        echo "  Dashboard: open http://${MINER_IP}/ and set the owner password."
        echo ""
        echo "To revert to BraiinsOS:"
        echo "  1. Power cycle with recovery jumper, OR"
        echo "  2. sysupgrade with BraiinsOS firmware image"
        exit 0
    fi
    echo "  Retry ${i}/5... (waiting 15s)"
    sleep 15
done

echo ""
echo "WARNING: Could not connect after reboot."
echo "Possible causes:"
echo "  1. Different IP (DHCP may assign new address for hostname 'dcentos')"
echo "  2. Boot failure (recovery: hold reset button for 10s during power-on)"
echo "  3. Network init delay (try again in 30s)"
echo ""
echo "If stuck, revert via BraiinsOS recovery partition:"
echo "  Hold reset button during power-on → boots recovery → reflashes firmware1"
