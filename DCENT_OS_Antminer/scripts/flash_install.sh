#!/bin/bash
#
# flash_install.sh — First-time DCENTos installation on stock Antminer S9
# D-Central Technologies, 2026
#
# This script:
# 1. Connects to a stock S9 via SSH (miner/miner credentials)
# 2. Exploits daemonc command injection for sudo access
# 3. Uploads our ramdisk.itb + signature
# 4. Flashes ramdisk to NAND mtd1
# 5. Patches SHA256 signature in mtd3
# 6. Reboots into DCENTos
#
# Prerequisites:
# - Built ramdisk.itb and ramdisk.sig (run 'make' first)
# - S9 on same network, stock firmware running
# - sshpass installed on build machine
#
# Usage: ./flash_install.sh <miner_ip> [images_dir]
#

set -e

cat >&2 <<'EOF'
ERROR: flash_install.sh is disabled.

This legacy stock-S9 installer erases/writes active NAND partitions mtd1 and
mtd3. Current DCENT_OS install policy forbids active-slot NAND flashing from
helper scripts. Use the SD-boot path or a route from DCENT Toolbox
(`dcent install --list-routes` followed by a dry run) instead.
EOF
exit 2

# Configuration
MINER_IP="${1:?Usage: $0 <miner_ip> [images_dir]}"
IMAGES_DIR="${2:-../buildroot/output/images}"
STOCK_USER="miner"
STOCK_PASS="miner"
DCENTOS_USER="root"
DCENTOS_PASS="dcentral"

# Colors
RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }

# Verify build outputs exist
RAMDISK="$IMAGES_DIR/ramdisk.itb"
SIGNATURE="$IMAGES_DIR/ramdisk.sig"

[ -f "$RAMDISK" ] || error "ramdisk.itb not found at $RAMDISK. Run 'make' first."
[ -f "$SIGNATURE" ] || error "ramdisk.sig not found at $SIGNATURE. Run 'make' first."

RAMDISK_SIZE=$(stat -c%s "$RAMDISK" 2>/dev/null || stat -f%z "$RAMDISK")
info "Ramdisk: $RAMDISK ($((RAMDISK_SIZE / 1024)) KB)"
info "Target: $MINER_IP"

# Check ramdisk fits in mtd1 (32MB)
if [ "$RAMDISK_SIZE" -gt 33554432 ]; then
    error "Ramdisk too large ($RAMDISK_SIZE bytes > 32MB mtd1 partition)"
fi

echo ""
echo -e "${YELLOW}=== DCENTos First-Time Installation ===${NC}"
echo ""
echo "This will:"
echo "  1. Connect to $MINER_IP with stock credentials"
echo "  2. Gain root access via daemonc exploit"
echo "  3. Flash DCENTos ramdisk to NAND"
echo "  4. Patch ramdisk signature"
echo "  5. Reboot into DCENTos"
echo ""
echo -e "${RED}WARNING: This modifies NAND partitions mtd1 and mtd3.${NC}"
echo -e "${RED}Recovery: Flash stock firmware via SD card if needed.${NC}"
echo ""
read -p "Continue? (yes/no): " confirm
[ "$confirm" = "yes" ] || { echo "Aborted."; exit 0; }

# Step 1: Test SSH connectivity
info "Testing SSH connection to $MINER_IP..."
if ! sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
    "$STOCK_USER@$MINER_IP" "echo ok" > /dev/null 2>&1; then
    error "Cannot connect to $MINER_IP with stock credentials ($STOCK_USER/$STOCK_PASS)"
fi
info "SSH connection successful."

# Step 2: Gain sudo access via daemonc exploit
# The daemonc binary on stock firmware allows command injection
# through the miner configuration that gives us sudo
info "Exploiting daemonc for sudo access..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    'daemonc sudo sh -c "echo \"miner ALL=(ALL) NOPASSWD: ALL\" >> /etc/sudoers"' 2>/dev/null || true

# Verify sudo works
if ! sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo id" 2>/dev/null | grep -q "uid=0"; then
    warn "daemonc exploit may have failed. Trying alternative method..."
    # Try direct root SSH (some S9 firmware allows this)
    if sshpass -p "admin" ssh -o StrictHostKeyChecking=no "root@$MINER_IP" \
        "echo ok" > /dev/null 2>&1; then
        STOCK_USER="root"
        STOCK_PASS="admin"
        info "Using root/admin credentials."
    else
        error "Cannot gain root access. Manual intervention needed."
    fi
fi
info "Root access acquired."

# Step 3: Upload files
info "Uploading ramdisk.itb ($((RAMDISK_SIZE / 1024)) KB)..."
sshpass -p "$STOCK_PASS" scp -o StrictHostKeyChecking=no \
    "$RAMDISK" "$STOCK_USER@$MINER_IP:/tmp/ramdisk.itb"

info "Uploading ramdisk.sig (256 bytes)..."
sshpass -p "$STOCK_PASS" scp -o StrictHostKeyChecking=no \
    "$SIGNATURE" "$STOCK_USER@$MINER_IP:/tmp/ramdisk.sig"

# Step 4: Upload NAND tools (in case stock firmware doesn't have them)
BOARD_DIR="$(dirname "$0")/../br2_external_dcentos/board/zynq"
for tool in nandwrite nanddump flash_erase; do
    if [ -f "$BOARD_DIR/$tool" ]; then
        info "Uploading $tool..."
        sshpass -p "$STOCK_PASS" scp -o StrictHostKeyChecking=no \
            "$BOARD_DIR/$tool" "$STOCK_USER@$MINER_IP:/tmp/$tool"
        sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no \
            "$STOCK_USER@$MINER_IP" "sudo chmod +x /tmp/$tool"
    fi
done

# Step 5: Flash ramdisk to mtd1
info "Erasing mtd1 (ramdisk partition)..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo flash_erase /dev/mtd1 0 0 || sudo /tmp/flash_erase /dev/mtd1 0 0"

info "Writing ramdisk.itb to mtd1..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo nandwrite -p /dev/mtd1 /tmp/ramdisk.itb || sudo /tmp/nandwrite -p /dev/mtd1 /tmp/ramdisk.itb"

# Step 6: Patch mtd3 signature
info "Dumping mtd3 (signature partition)..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo nanddump /dev/mtd3 -f /tmp/mtd3.bin || sudo /tmp/nanddump /dev/mtd3 -f /tmp/mtd3.bin"

info "Patching SHA256 signature at offset 1024-1279..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" << 'REMOTE_SCRIPT'
    # Create patched mtd3:
    # bytes 0-1023:   keep original (kernel RSA signature)
    # bytes 1024-1279: replace with our ramdisk SHA256 (+ zero padding)
    # bytes 1280+:    keep original (other signatures)
    sudo sh -c '
        dd if=/tmp/mtd3.bin of=/tmp/mtd3_head bs=1 count=1024
        dd if=/tmp/mtd3.bin of=/tmp/mtd3_tail bs=1 skip=1280
        cat /tmp/mtd3_head /tmp/ramdisk.sig /tmp/mtd3_tail > /tmp/mtd3_patched.bin
    '
REMOTE_SCRIPT

info "Erasing and writing patched mtd3..."
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo flash_erase /dev/mtd3 0 0 || sudo /tmp/flash_erase /dev/mtd3 0 0"
sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo nandwrite -p /dev/mtd3 /tmp/mtd3_patched.bin || sudo /tmp/nandwrite -p /dev/mtd3 /tmp/mtd3_patched.bin"

# Step 7: Verify
info "Verifying signature patch..."
VERIFY=$(sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
    "sudo dd if=/dev/mtd3 bs=1 skip=1024 count=256 2>/dev/null | md5sum")
SIG_MD5=$(md5sum "$SIGNATURE" | awk '{print $1}')
VERIFY_MD5=$(echo "$VERIFY" | awk '{print $1}')

if [ "$SIG_MD5" = "$VERIFY_MD5" ]; then
    info "Signature verification PASSED."
else
    warn "Signature verification FAILED. md5 mismatch: expected $SIG_MD5, got $VERIFY_MD5"
    warn "The miner may not boot correctly. You can recover via SD card."
fi

# Step 8: Reboot
echo ""
info "Installation complete!"
echo ""
echo "The miner will now reboot into DCENTos."
echo "After reboot, connect with:"
echo ""
echo -e "  ${GREEN}ssh root@$MINER_IP${NC}    (password: dcentral)"
echo ""
read -p "Reboot now? (yes/no): " reboot
if [ "$reboot" = "yes" ]; then
    info "Rebooting..."
    sshpass -p "$STOCK_PASS" ssh -o StrictHostKeyChecking=no "$STOCK_USER@$MINER_IP" \
        "sudo reboot" 2>/dev/null || true
    info "Miner is rebooting. Wait ~60 seconds, then SSH in."
else
    info "Skipping reboot. Reboot manually when ready: sudo reboot"
fi
