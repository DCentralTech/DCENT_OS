#!/bin/bash
#
# extract_kernel_modules.sh — Extract stock kernel modules from a running S9
# D-Central Technologies, 2026
#
# The stock kernel modules (bitmain_axi.ko, fpga_mem_driver.ko) are binary
# blobs compiled against the stock 4.6.0-xilinx kernel. We CANNOT recompile
# them because the kernel source is RSA-locked. We must extract them from
# a stock S9 and include them in our firmware.
#
# Usage: ./extract_kernel_modules.sh <miner_ip> [output_dir]
#

set -e

MINER_IP="${1:?Usage: $0 <miner_ip> [output_dir]}"
OUTPUT_DIR="${2:-../br2_external_dcentos/board/zynq/kernel_modules}"

USER="miner"
PASS="miner"

GREEN='\033[1;32m'
NC='\033[0m'
info() { echo -e "${GREEN}[INFO]${NC} $*"; }

mkdir -p "$OUTPUT_DIR"

info "Extracting kernel modules from $MINER_IP..."

# Try stock credentials first, then DCENTos
if ! sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
    "$USER@$MINER_IP" "echo ok" > /dev/null 2>&1; then
    USER="root"
    PASS="dcentral"
    info "Stock credentials failed, trying DCENTos credentials..."
fi

# Find and extract modules
for module in bitmain_axi.ko fpga_mem_driver.ko; do
    info "Searching for $module..."
    REMOTE_PATH=$(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "$USER@$MINER_IP" \
        "find / -name '$module' 2>/dev/null | head -1")

    if [ -n "$REMOTE_PATH" ]; then
        info "Found: $REMOTE_PATH"
        sshpass -p "$PASS" scp -o StrictHostKeyChecking=no \
            "$USER@$MINER_IP:$REMOTE_PATH" "$OUTPUT_DIR/$module"
        info "Saved: $OUTPUT_DIR/$module ($(stat -c%s "$OUTPUT_DIR/$module" 2>/dev/null || stat -f%z "$OUTPUT_DIR/$module") bytes)"
    else
        echo "WARNING: $module not found on device!"
    fi
done

# Verify kernel version
KVER=$(sshpass -p "$PASS" ssh -o StrictHostKeyChecking=no "$USER@$MINER_IP" "uname -r")
info "Kernel version: $KVER"

# Check module info
for module in "$OUTPUT_DIR"/*.ko; do
    if [ -f "$module" ]; then
        info "Module: $(basename $module)"
        file "$module" 2>/dev/null || true
    fi
done

echo ""
info "Extraction complete. Modules saved to: $OUTPUT_DIR/"
info "These modules are compiled for kernel $KVER and must be loaded with insmod."
