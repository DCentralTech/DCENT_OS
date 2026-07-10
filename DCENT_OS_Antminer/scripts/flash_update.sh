#!/bin/bash
#
# flash_update.sh — Update DCENTos on an already-installed miner
# D-Central Technologies, 2026
#
# Simpler than first install: no daemonc exploit needed, just
# SSH in as root, flash new ramdisk, patch signature, reboot.
#
# Usage: ./flash_update.sh <miner_ip> [images_dir]
#

set -e

MINER_IP="${1:?Usage: $0 <miner_ip> [images_dir]}"
IMAGES_DIR="${2:-../buildroot/output/images}"
USER="root"
PASS="dcentral"

RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }

error "flash_update.sh is disabled because it bypasses A/B sysupgrade safety. Use the supported inactive-slot sysupgrade flow instead."

RAMDISK="$IMAGES_DIR/ramdisk.itb"
SIGNATURE="$IMAGES_DIR/ramdisk.sig"

[ -f "$RAMDISK" ] || error "ramdisk.itb not found at $RAMDISK"
[ -f "$SIGNATURE" ] || error "ramdisk.sig not found at $SIGNATURE"

RAMDISK_SIZE=$(stat -c%s "$RAMDISK" 2>/dev/null || stat -f%z "$RAMDISK")
info "Updating DCENTos on $MINER_IP"
info "Ramdisk: $((RAMDISK_SIZE / 1024)) KB"

# Test connection
info "Connecting..."
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
    "$USER@$MINER_IP" "echo ok" > /dev/null 2>&1 || \
    error "Cannot connect. Is DCENTos running? Try flash_install.sh for stock firmware."

# Upload
info "Uploading ramdisk..."
sshpass -p "$PASS" scp -o StrictHostKeyChecking=no \
    "$RAMDISK" "$SIGNATURE" "$USER@$MINER_IP:/tmp/"

# Flash
info "Flashing..."
sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "$USER@$MINER_IP" << 'REMOTE'
    set -e

    echo "Erasing mtd1..."
    flash_erase /dev/mtd1 0 0

    echo "Writing ramdisk..."
    nandwrite -p /dev/mtd1 /tmp/ramdisk.itb

    echo "Patching mtd3 signature..."
    nanddump /dev/mtd3 -f /tmp/mtd3.bin
    dd if=/tmp/mtd3.bin of=/tmp/mtd3_head bs=1 count=1024 2>/dev/null
    dd if=/tmp/mtd3.bin of=/tmp/mtd3_tail bs=1 skip=1280 2>/dev/null
    cat /tmp/mtd3_head /tmp/ramdisk.sig /tmp/mtd3_tail > /tmp/mtd3_patched.bin
    flash_erase /dev/mtd3 0 0
    nandwrite -p /dev/mtd3 /tmp/mtd3_patched.bin

    echo "Done. Cleaning up..."
    rm -f /tmp/ramdisk.itb /tmp/ramdisk.sig /tmp/mtd3.bin /tmp/mtd3_head /tmp/mtd3_tail /tmp/mtd3_patched.bin
REMOTE

info "Firmware image write path completed; reboot and rollback verification still pending."
read -p "Reboot now? (yes/no): " reboot
if [ "$reboot" = "yes" ]; then
    sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "$USER@$MINER_IP" "reboot" 2>/dev/null || true
    info "Rebooting. Wait ~60 seconds."
fi
