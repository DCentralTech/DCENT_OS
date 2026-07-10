#!/bin/bash
#
# extract_boot_components.sh — Extract boot chain components from a BraiinsOS miner
# D-Central Technologies, 2026
#
# Extracts the proven BraiinsOS boot chain (FSBL + U-Boot + FPGA bitstream + kernel)
# so we can build our own SD card image using THEIR boot components + OUR rootfs.
#
# This eliminates the kernel module dependency entirely — BraiinsOS uses UIO
# (Userspace I/O) for all FPGA communication, no kernel modules needed.
#
# Usage: ./extract_boot_components.sh <miner_ip> [output_dir]
#
# Requires: SSH root access (empty password on BraiinsOS)
#

set -e

MINER_IP="${1:?Usage: $0 <miner_ip> [output_dir]}"
OUTPUT_DIR="${2:-../extractions}"

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"

GREEN='\033[1;32m'
YELLOW='\033[1;33m'
RED='\033[1;31m'
NC='\033[0m'
info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

mkdir -p "$OUTPUT_DIR"

# Test connection
info "Testing SSH to $MINER_IP..."
if ! ssh $SSH_OPTS root@"$MINER_IP" "echo ok" > /dev/null 2>&1; then
    error "Cannot connect. Ensure BraiinsOS is installed and SSH is accessible."
    exit 1
fi

# Detect miner type
KVER=$(ssh $SSH_OPTS root@"$MINER_IP" "uname -r")
ARCH=$(ssh $SSH_OPTS root@"$MINER_IP" "uname -m")
info "Kernel: $KVER ($ARCH)"

# Get MTD layout
info "Reading MTD layout..."
MTD_INFO=$(ssh $SSH_OPTS root@"$MINER_IP" "cat /proc/mtd")
echo "$MTD_INFO"
echo ""

# Extract critical MTD partitions
info "Extracting MTD partitions to $MINER_IP:/tmp/..."
ssh $SSH_OPTS root@"$MINER_IP" '
for part in $(cat /proc/mtd | grep -v "^dev" | cut -d: -f1); do
    name=$(grep "$part:" /proc/mtd | cut -d\" -f2)
    echo "  Dumping $part ($name)..."
    dd if=/dev/${part}ro of=/tmp/${part}_${name}.bin bs=131072 2>/dev/null
done
ls -la /tmp/mtd*.bin
'

# Pull MTD dumps locally
info "Pulling MTD dumps to $OUTPUT_DIR/..."
scp -O $SSH_OPTS root@"$MINER_IP":/tmp/mtd*.bin "$OUTPUT_DIR/"

# Extract additional components
info "Extracting kernel config, device tree, bosminer..."
ssh $SSH_OPTS root@"$MINER_IP" '
# Device tree
[ -e /sys/firmware/fdt ] && cp /sys/firmware/fdt /tmp/devicetree.dtb && echo "DTB: ok"
# Kernel config
[ -e /proc/config.gz ] && cp /proc/config.gz /tmp/kernel_config.gz && echo "Config: ok"
# Bosminer binary
[ -e /usr/bin/bosminer ] && cp /usr/bin/bosminer /tmp/bosminer && echo "Bosminer: ok"
'

for f in devicetree.dtb kernel_config.gz bosminer; do
    scp -O $SSH_OPTS root@"$MINER_IP":/tmp/$f "$OUTPUT_DIR/" 2>/dev/null || true
done

# UIO inventory
info "Capturing UIO device inventory..."
ssh $SSH_OPTS root@"$MINER_IP" '
echo "=== UIO Devices ==="
for u in /sys/class/uio/uio*; do
    name=$(cat $u/name 2>/dev/null)
    addr=$(cat $u/maps/map0/addr 2>/dev/null)
    size=$(cat $u/maps/map0/size 2>/dev/null)
    echo "  $(basename $u): $name @ $addr ($size)"
done
' > "$OUTPUT_DIR/uio_inventory.txt"

# Package list
info "Capturing package inventory..."
ssh $SSH_OPTS root@"$MINER_IP" 'opkg list-installed 2>/dev/null || echo "no opkg"' > "$OUTPUT_DIR/packages.txt"

# Summary
info ""
info "=== Extraction Complete ==="
info "Output directory: $OUTPUT_DIR/"
ls -lh "$OUTPUT_DIR/"
echo ""
info "Boot chain components for SD card image:"
info "  FSBL:     mtd0_boot.bin"
info "  U-Boot:   mtd1_uboot.bin"
info "  FPGA:     mtd2_fpga1.bin (gzip-compressed Zynq bitstream)"
info "  Env:      mtd4_uboot_env.bin"
info "  Recovery: mtd6_recovery.bin (complete BraiinsOS FIT image)"
info ""
info "Use these with our Buildroot rootfs to create a bootable SD card."
info "NO kernel modules needed — all hardware access via UIO + devmem."
