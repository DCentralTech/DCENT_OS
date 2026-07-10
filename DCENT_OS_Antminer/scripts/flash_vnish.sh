#!/bin/bash
#
# flash_vnish.sh — Install DCENTos on a miner currently running VNish
# D-Central Technologies, 2026
#
# Installs DCENTos via SSH to a VNish-running miner (S9/S17/S19 Zynq).
#
# VNish v1.2.x only replaces the ramdisk (mtd1) — kernel, FPGA, and U-Boot
# remain from whatever was installed before (stock Bitmain or BraiinsOS).
#
# PROBLEM: If VNish was installed over stock Bitmain firmware, the kernel
# is 3.14.0-xilinx which does NOT have UIO support. DCENTos requires UIO.
# In this case, we must also flash BraiinsOS kernel + FPGA.
#
# SOLUTION: This script detects the kernel version and:
#   A) If BraiinsOS kernel (4.4.x, has UIO): just replace rootfs via UBI
#   B) If stock kernel (3.14.x, no UIO): flash full boot chain from SD or
#      extracted BraiinsOS components, THEN install DCENTos rootfs
#
# Prerequisites:
#   - VNish miner with SSH enabled (enable via web: ssh.cgi POST action=1)
#   - SSH credentials: root / admin (default VNish)
#   - DCENTos rootfs.squashfs (from Buildroot)
#   - Optional: BraiinsOS boot components in extractions/s9/ (for stock kernel case)
#
# Usage:
#   ./flash_vnish.sh <miner_ip> [--password <pass>] [--images-dir <dir>]
#
# Examples:
#   ./flash_vnish.sh 203.0.113.50                  # VNish S9, root/admin
#   ./flash_vnish.sh 203.0.113.50 --password root  # VNish S9, root/root
#   ./flash_vnish.sh 203.0.113.193                 # VNish S17
#

set -e

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIRMWARE_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FIRMWARE_DIR")"
EXTRACTIONS_DIR="$PROJECT_ROOT/extractions/s9"

MINER_IP=""
MINER_PASS="admin"
IMAGES_DIR="$FIRMWARE_DIR/buildroot/output/images"
USE_NODE_SSH=false

# SSH options for legacy crypto (VNish uses old algorithms)
SSH_LEGACY_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o KexAlgorithms=+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o HostKeyAlgorithms=+ssh-rsa,ssh-dss"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o PubkeyAcceptedAlgorithms=+ssh-rsa"
SSH_LEGACY_OPTS="$SSH_LEGACY_OPTS -o Ciphers=+aes128-ctr,aes128-cbc,3des-cbc"

# Colors
RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
CYAN='\033[1;36m'
BOLD='\033[1m'
NC='\033[0m'

info()   { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()  { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }
header() { echo -e "\n${CYAN}${BOLD}=== $* ===${NC}"; }

# =============================================================================
# Parse Arguments
# =============================================================================

usage() {
    echo "Usage: $(basename "$0") <miner_ip> [OPTIONS]"
    echo ""
    echo "Install DCENTos on a VNish-running Antminer via SSH."
    echo ""
    echo "Options:"
    echo "  --password <pass>     SSH password (default: admin)"
    echo "  --images-dir <dir>    Path to DCENTos build images (default: buildroot/output/images)"
    echo "  --node-ssh            Use Node.js ssh2 instead of sshpass (for Windows)"
    echo "  --help                Show this help"
    echo ""
    echo "VNish SSH must be enabled first via web interface:"
    echo "  curl --digest -u root:root 'http://<ip>/cgi-bin/ssh.cgi' -d 'action=1&status=0&port=22'"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --password)    MINER_PASS="$2"; shift 2 ;;
        --images-dir)  IMAGES_DIR="$2"; shift 2 ;;
        --node-ssh)    USE_NODE_SSH=true; shift ;;
        --help|-h)     usage; exit 0 ;;
        -*)            error "Unknown option: $1" ;;
        *)
            if [ -z "$MINER_IP" ]; then
                MINER_IP="$1"
            else
                error "Unexpected argument: $1"
            fi
            shift ;;
    esac
done

[ -n "$MINER_IP" ] || { usage; exit 1; }

error "This legacy VNish installer writes active firmware paths and is disabled. Use a safe sysupgrade or SD migration path instead."

# =============================================================================
# SSH Helper Functions
# =============================================================================
# VNish uses legacy crypto algorithms. We support two SSH backends:
#   1. sshpass + ssh (Linux/macOS)
#   2. Node.js ssh2 (Windows, or when sshpass unavailable)

NODE_SSH="$PROJECT_ROOT/tools/ssh_cmd.js"

ssh_exec() {
    local cmd="$1"
    if $USE_NODE_SSH || ! command -v sshpass >/dev/null 2>&1; then
        if [ -f "$NODE_SSH" ]; then
            node "$NODE_SSH" "$MINER_IP" root "$MINER_PASS" "$cmd" 2>/dev/null
        else
            error "No SSH backend available. Install sshpass or run: npm install ssh2"
        fi
    else
        sshpass -p "$MINER_PASS" ssh $SSH_LEGACY_OPTS "root@$MINER_IP" "$cmd" 2>/dev/null
    fi
}

scp_upload() {
    local src="$1"
    local dst="$2"
    if $USE_NODE_SSH || ! command -v sshpass >/dev/null 2>&1; then
        # Node.js ssh2 doesn't have SCP built in — use sftp or fall back to
        # base64 encoding for small files, dd for large files
        local size
        size=$(stat -c%s "$src" 2>/dev/null || stat -f%z "$src" 2>/dev/null)
        if [ "$size" -lt 1048576 ]; then
            # <1MB: base64 encode and decode on remote
            info "  Uploading via base64 encoding ($((size / 1024)) KB)..."
            local b64
            b64=$(base64 < "$src" | tr -d '\n')
            # Split into chunks for shell argument limit
            local chunk_size=65000
            local offset=0
            local total=${#b64}
            ssh_exec "rm -f $dst"
            while [ $offset -lt $total ]; do
                local chunk="${b64:$offset:$chunk_size}"
                ssh_exec "echo '$chunk' >> ${dst}.b64"
                offset=$((offset + chunk_size))
            done
            ssh_exec "base64 -d ${dst}.b64 > $dst && rm -f ${dst}.b64"
        else
            # >1MB: use sshpass with legacy opts (must be available for large files)
            if command -v sshpass >/dev/null 2>&1; then
                sshpass -p "$MINER_PASS" scp $SSH_LEGACY_OPTS "$src" "root@$MINER_IP:$dst"
            else
                error "Files >1MB require sshpass for upload. Install sshpass or use SD card method."
            fi
        fi
    else
        sshpass -p "$MINER_PASS" scp $SSH_LEGACY_OPTS "$src" "root@$MINER_IP:$dst"
    fi
}

# =============================================================================
# Pre-flight Checks
# =============================================================================

header "DCENTos VNish Installer"

# Check rootfs
ROOTFS="$IMAGES_DIR/rootfs.squashfs"
if [ ! -f "$ROOTFS" ]; then
    # Try alternative names
    for alt in rootfs.ext2.gz ramdisk.itb; do
        if [ -f "$IMAGES_DIR/$alt" ]; then
            ROOTFS="$IMAGES_DIR/$alt"
            break
        fi
    done
fi
[ -f "$ROOTFS" ] || error "No rootfs found in $IMAGES_DIR. Build firmware first."

ROOTFS_SIZE=$(stat -c%s "$ROOTFS" 2>/dev/null || stat -f%z "$ROOTFS" 2>/dev/null)
ROOTFS_NAME=$(basename "$ROOTFS")
info "Rootfs: $ROOTFS_NAME ($((ROOTFS_SIZE / 1024)) KB)"
info "Target: $MINER_IP (VNish, root/$MINER_PASS)"

# Test SSH connectivity
echo ""
info "Testing SSH connection..."
CONNECT_TEST=$(ssh_exec "echo CONNECTED" 2>&1) || true
if echo "$CONNECT_TEST" | grep -q "CONNECTED"; then
    info "SSH connection: OK"
else
    echo ""
    warn "Cannot connect via SSH. VNish SSH may need to be enabled first."
    echo ""
    echo "Enable VNish SSH via web interface:"
    echo "  curl --digest -u root:root 'http://$MINER_IP/cgi-bin/ssh.cgi' \\"
    echo "    -d 'action=1&status=0&port=22'"
    echo ""
    echo "Or via browser: http://$MINER_IP → System → SSH → Enable"
    echo ""

    # Try enabling SSH via HTTP
    read -p "Attempt to enable SSH via HTTP? (yes/no): " enable_ssh
    if [ "$enable_ssh" = "yes" ]; then
        info "Sending SSH enable request..."
        HTTP_RESULT=$(curl -s --digest -u "root:$MINER_PASS" \
            "http://$MINER_IP/cgi-bin/ssh.cgi" \
            -d "action=1&status=0&port=22" 2>&1) || true

        # Also try root:root
        if echo "$HTTP_RESULT" | grep -qi "error\|401\|fail"; then
            HTTP_RESULT=$(curl -s --digest -u "root:root" \
                "http://$MINER_IP/cgi-bin/ssh.cgi" \
                -d "action=1&status=0&port=22" 2>&1) || true
        fi

        sleep 3
        CONNECT_TEST=$(ssh_exec "echo CONNECTED" 2>&1) || true
        if echo "$CONNECT_TEST" | grep -q "CONNECTED"; then
            info "SSH enabled and connected!"
        else
            error "Still cannot connect. Check credentials and network."
        fi
    else
        exit 1
    fi
fi

# =============================================================================
# Detect Miner Environment
# =============================================================================

header "Detecting Miner Environment"

# Gather system info in a single SSH session (VNish is unstable with parallel SSH)
MINER_INFO=$(ssh_exec '
    echo "KERNEL_VER=$(uname -r)"
    echo "HOSTNAME=$(hostname)"
    echo "ARCH=$(uname -m)"

    # Detect firmware type
    if [ -f /etc/bos_version ]; then
        echo "FW_TYPE=braiinsos"
        echo "FW_VER=$(cat /etc/bos_version | head -1)"
    elif pidof dashd >/dev/null 2>&1; then
        echo "FW_TYPE=vnish"
        echo "FW_VER=$(cat /etc/anthill_version 2>/dev/null || echo unknown)"
    elif [ -f /etc/dcentos-version ]; then
        echo "FW_TYPE=dcentos"
        echo "FW_VER=$(cat /etc/dcentos-version)"
    elif pidof bmminer >/dev/null 2>&1; then
        echo "FW_TYPE=stock"
        echo "FW_VER=stock"
    else
        echo "FW_TYPE=unknown"
        echo "FW_VER=unknown"
    fi

    # Check NAND layout
    echo "MTD_COUNT=$(grep -c "^mtd" /proc/mtd 2>/dev/null || echo 0)"
    cat /proc/mtd 2>/dev/null | while read line; do
        case "$line" in mtd*) echo "MTD_LINE=$line" ;; esac
    done

    # Check for UBI
    if [ -d /sys/class/ubi/ubi0 ]; then
        echo "HAS_UBI=yes"
        echo "UBI_VOLS=$(cat /sys/class/ubi/ubi0/volumes_count 2>/dev/null || echo 0)"
        for vol in /sys/class/ubi/ubi0_*; do
            if [ -d "$vol" ]; then
                vname=$(cat $vol/name 2>/dev/null)
                vsize=$(cat $vol/data_bytes 2>/dev/null)
                echo "UBI_VOL=$(basename $vol):$vname:$vsize"
            fi
        done
    else
        echo "HAS_UBI=no"
    fi

    # Check UIO devices
    UIO_COUNT=0
    for u in /sys/class/uio/uio*; do
        [ -d "$u" ] && UIO_COUNT=$((UIO_COUNT + 1))
    done
    echo "UIO_COUNT=$UIO_COUNT"

    # RAM and free space
    echo "RAM_MB=$(awk "/MemTotal/{print int(\$2/1024)}" /proc/meminfo)"
    echo "TMP_FREE_KB=$(df /tmp 2>/dev/null | tail -1 | awk "{print \$4}")"

    # SoC identification
    if [ -f /sys/devices/soc0/soc_id ]; then
        echo "SOC=$(cat /sys/devices/soc0/soc_id)"
    elif grep -q zynq /proc/cpuinfo 2>/dev/null; then
        echo "SOC=zynq"
    fi
') || error "Failed to gather system info"

# Parse results
eval "$(echo "$MINER_INFO" | grep -E "^(KERNEL_VER|HOSTNAME|ARCH|FW_TYPE|FW_VER|MTD_COUNT|HAS_UBI|UBI_VOLS|UIO_COUNT|RAM_MB|TMP_FREE_KB|SOC)=")"

info "Hostname:    $HOSTNAME"
info "Kernel:      $KERNEL_VER"
info "Firmware:    $FW_TYPE ($FW_VER)"
info "SoC:         ${SOC:-unknown}"
info "RAM:         ${RAM_MB:-?} MB"
info "MTD parts:   $MTD_COUNT"
info "UBI:         $HAS_UBI (${UBI_VOLS:-0} volumes)"
info "UIO devices: $UIO_COUNT"
info "/tmp free:   ${TMP_FREE_KB:-?} KB"

# Parse MTD lines
echo ""
info "NAND partition table:"
echo "$MINER_INFO" | grep "^MTD_LINE=" | sed 's/^MTD_LINE=/  /'

# Parse UBI volumes
if [ "$HAS_UBI" = "yes" ]; then
    echo ""
    info "UBI volumes:"
    echo "$MINER_INFO" | grep "^UBI_VOL=" | sed 's/^UBI_VOL=/  /'
fi

# =============================================================================
# Determine Installation Method
# =============================================================================

header "Determining Installation Method"

INSTALL_METHOD=""
NEEDS_KERNEL=false
NEEDS_FPGA=false

# Check kernel UIO support
case "$KERNEL_VER" in
    4.4.*)
        info "BraiinsOS kernel detected — UIO support available"
        ;;
    3.14.*)
        warn "Stock Bitmain kernel detected — NO UIO support"
        warn "DCENTos requires UIO. Must flash BraiinsOS kernel + FPGA."
        NEEDS_KERNEL=true
        NEEDS_FPGA=true
        ;;
    *)
        warn "Unknown kernel version: $KERNEL_VER"
        warn "UIO support uncertain — will attempt to detect"
        if [ "$UIO_COUNT" -gt 0 ]; then
            info "UIO devices present ($UIO_COUNT) — kernel has UIO support"
        else
            warn "No UIO devices — may need kernel replacement"
            NEEDS_KERNEL=true
            NEEDS_FPGA=true
        fi
        ;;
esac

# Determine method based on NAND layout
if [ "$HAS_UBI" = "yes" ] && [ "${UBI_VOLS:-0}" -ge 3 ]; then
    INSTALL_METHOD="ubi"
    info "Method: UBI volume replacement (same as BraiinsOS path)"
    info "  Will replace ubi0_1 (rootfs) with DCENTos squashfs"
elif [ "$MTD_COUNT" -ge 6 ]; then
    INSTALL_METHOD="raw_nand"
    info "Method: Raw NAND flash (ramdisk replacement)"
    info "  Will write DCENTos ramdisk to mtd1 partition"
else
    error "Unrecognized NAND layout ($MTD_COUNT partitions, UBI=$HAS_UBI). Cannot proceed safely."
fi

# Check if we have needed components for kernel/FPGA flash
if $NEEDS_KERNEL; then
    KERNEL_FIT="$EXTRACTIONS_DIR/kernel.bin"
    FPGA_BIT="$EXTRACTIONS_DIR/mtd2_fpga1.bin"
    DTB_FILE="$EXTRACTIONS_DIR/s9_devicetree.dtb"
    RECOVERY_FIT="$EXTRACTIONS_DIR/mtd6_recovery.bin"

    if [ ! -f "$KERNEL_FIT" ] && [ ! -f "$RECOVERY_FIT" ]; then
        echo ""
        warn "BraiinsOS boot components not found in $EXTRACTIONS_DIR"
        warn "These are needed because the stock kernel lacks UIO support."
        echo ""
        echo "Options:"
        echo "  1. Extract components first: ./extract_boot_components.sh <braiins_miner_ip>"
        echo "  2. Install BraiinsOS first, then run flash_braiinsos.sh"
        echo "  3. Use SD card method (build_sd_image.sh + write_sd_card.sh)"
        echo ""
        error "Cannot install DCENTos without UIO-capable kernel."
    fi
fi

# =============================================================================
# Safety Checks and Confirmation
# =============================================================================

header "Installation Plan"

echo ""
echo -e "  ${BOLD}Target:${NC}     root@$MINER_IP (VNish $FW_VER)"
echo -e "  ${BOLD}Method:${NC}     $INSTALL_METHOD"
echo -e "  ${BOLD}Rootfs:${NC}     $ROOTFS_NAME ($((ROOTFS_SIZE / 1024)) KB)"
if $NEEDS_KERNEL; then
    echo -e "  ${BOLD}Kernel:${NC}     BraiinsOS 4.4.x (replacing stock $KERNEL_VER)"
fi
if $NEEDS_FPGA; then
    echo -e "  ${BOLD}FPGA:${NC}       BraiinsOS bitstream (from extractions)"
fi
echo ""
echo -e "  ${BOLD}After install:${NC}"
echo "    - DCENTos Hacker Shell rootfs"
echo "    - SSH: root / dcentral"
echo "    - Hostname: dcentos"
echo ""
echo -e "  ${BOLD}Revert:${NC}"
echo "    - Reflash VNish via web interface or SD card"
echo "    - Or install BraiinsOS via SD card"
echo ""

read -p "Proceed with installation? (yes/no): " CONFIRM
[ "$CONFIRM" = "yes" ] || { echo "Aborted."; exit 0; }

# =============================================================================
# Stop Mining
# =============================================================================

header "Step 1: Stop Mining Daemon"

info "Stopping VNish daemons..."
ssh_exec '
    # Stop mining daemon
    for proc in cgminer dashd antminer_monitor; do
        pid=$(pidof $proc 2>/dev/null)
        if [ -n "$pid" ]; then
            kill $pid 2>/dev/null
            echo "Stopped $proc (PID $pid)"
        fi
    done
    sleep 2
    echo "DAEMONS_STOPPED"
' || warn "Could not stop all daemons (continuing anyway)"

# =============================================================================
# Upload Files
# =============================================================================

header "Step 2: Upload DCENTos"

# Check /tmp has enough space
if [ -n "$TMP_FREE_KB" ] && [ "$TMP_FREE_KB" -lt $((ROOTFS_SIZE / 1024 + 1024)) ]; then
    warn "/tmp may not have enough space. Trying /var/tmp..."
    REMOTE_TMP="/var/tmp"
else
    REMOTE_TMP="/tmp"
fi

info "Uploading $ROOTFS_NAME to $REMOTE_TMP..."
scp_upload "$ROOTFS" "$REMOTE_TMP/$ROOTFS_NAME"

# Verify upload
REMOTE_SIZE=$(ssh_exec "stat -c%s $REMOTE_TMP/$ROOTFS_NAME 2>/dev/null || echo 0")
REMOTE_SIZE=$(echo "$REMOTE_SIZE" | tr -d '[:space:]')
if [ "$REMOTE_SIZE" != "$ROOTFS_SIZE" ]; then
    error "Size mismatch! Local: $ROOTFS_SIZE, Remote: $REMOTE_SIZE"
fi
info "Upload verified: $REMOTE_SIZE bytes"

# Upload kernel/FPGA if needed
if $NEEDS_KERNEL && [ -f "$KERNEL_FIT" ]; then
    info "Uploading BraiinsOS kernel..."
    scp_upload "$KERNEL_FIT" "$REMOTE_TMP/kernel.bin"
fi

if $NEEDS_FPGA && [ -f "$FPGA_BIT" ]; then
    info "Uploading FPGA bitstream..."
    scp_upload "$FPGA_BIT" "$REMOTE_TMP/fpga.bin"
fi

# =============================================================================
# Flash Firmware
# =============================================================================

header "Step 3: Flash to NAND"

if [ "$INSTALL_METHOD" = "ubi" ]; then
    # UBI path: same as flash_braiinsos.sh
    info "Flashing rootfs to UBI volume ubi0_1..."
    ssh_exec "ubiupdatevol /dev/ubi0_1 $REMOTE_TMP/$ROOTFS_NAME && echo FLASH_OK" || \
        error "UBI flash failed!"
    info "Rootfs flashed to UBI"

    # Clear overlay for clean boot
    info "Clearing overlay for fresh boot..."
    ssh_exec "ubiupdatevol /dev/ubi0_2 -t 2>/dev/null; echo OVERLAY_DONE" || true

elif [ "$INSTALL_METHOD" = "raw_nand" ]; then
    # Raw NAND path: flash_erase + nandwrite to mtd1
    info "Erasing mtd1 (ramdisk partition)..."
    ssh_exec "flash_erase /dev/mtd1 0 0 && echo ERASE_OK" || \
        error "NAND erase failed! NAND tools may not be available."

    info "Writing $ROOTFS_NAME to mtd1..."
    ssh_exec "nandwrite -p /dev/mtd1 $REMOTE_TMP/$ROOTFS_NAME && echo WRITE_OK" || \
        error "NAND write failed!"
    info "Rootfs written to mtd1"
fi

# Flash kernel if needed
if $NEEDS_KERNEL && [ -f "$KERNEL_FIT" ]; then
    header "Step 3b: Flash Kernel"

    # Determine kernel MTD partition
    KERNEL_MTD=""
    if [ "$HAS_UBI" = "yes" ]; then
        # UBI: update kernel volume
        info "Updating kernel UBI volume..."
        ssh_exec "ubiupdatevol /dev/ubi0_0 $REMOTE_TMP/kernel.bin && echo KERNEL_OK" || \
            warn "Kernel UBI update failed"
    else
        # Raw NAND: need to find kernel partition
        # Stock S9 doesn't separate kernel — it's in the ramdisk FIT
        warn "Kernel replacement on raw NAND is complex."
        warn "Consider using SD card method instead for full boot chain replacement."
    fi
fi

# Flash FPGA if needed
if $NEEDS_FPGA && [ -f "$FPGA_BIT" ]; then
    header "Step 3c: Flash FPGA"
    info "Erasing mtd2 (FPGA partition)..."
    ssh_exec "flash_erase /dev/mtd2 0 0 && echo ERASE_OK" || warn "FPGA erase failed"
    info "Writing FPGA bitstream to mtd2..."
    ssh_exec "nandwrite -p /dev/mtd2 $REMOTE_TMP/fpga.bin && echo WRITE_OK" || warn "FPGA write failed"
    info "FPGA bitstream flashed"
fi

# =============================================================================
# Cleanup and Reboot
# =============================================================================

header "Step 4: Cleanup and Reboot"

info "Cleaning up temp files..."
ssh_exec "rm -f $REMOTE_TMP/$ROOTFS_NAME $REMOTE_TMP/kernel.bin $REMOTE_TMP/fpga.bin" || true

echo ""
echo -e "${GREEN}${BOLD}Installation Complete!${NC}"
echo ""
echo "The miner will now reboot into DCENTos."
echo ""
echo -e "  SSH:  ${GREEN}ssh root@$MINER_IP${NC}  (password: ${BOLD}dcentral${NC})"
echo ""

read -p "Reboot now? (yes/no): " REBOOT
if [ "$REBOOT" = "yes" ]; then
    info "Rebooting in 3 seconds..."
    sleep 3
    ssh_exec "reboot" || true

    echo ""
    info "Miner is rebooting. Waiting 90 seconds..."
    sleep 90

    # Try to connect
    info "Attempting SSH connection to DCENTos..."
    for i in 1 2 3 4 5; do
        # Try DCENTos credentials
        if command -v sshpass >/dev/null 2>&1; then
            RESULT=$(sshpass -p "dcentral" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
                "root@$MINER_IP" "echo ALIVE; cat /etc/dcentos-version 2>/dev/null" 2>/dev/null) || true
        else
            RESULT=$(node "$NODE_SSH" "$MINER_IP" root dcentral "echo ALIVE; cat /etc/dcentos-version 2>/dev/null" 2>/dev/null) || true
        fi

        if echo "$RESULT" | grep -q "ALIVE"; then
            echo ""
            echo -e "${GREEN}${BOLD}=== SUCCESS ===${NC}"
            echo "DCENTos is running on $MINER_IP!"
            echo "$RESULT"
            exit 0
        fi
        echo "  Retry $i/5... (waiting 15s)"
        sleep 15
    done

    echo ""
    warn "Could not connect after reboot."
    echo "Possible causes:"
    echo "  1. DHCP assigned a new IP for hostname 'dcentos'"
    echo "  2. Kernel incompatibility (if stock kernel was preserved)"
    echo "  3. Boot delay — try again in 30 seconds"
    echo ""
    echo "Recovery options:"
    echo "  - Flash VNish via SD card"
    echo "  - Flash BraiinsOS via SD card"
    echo "  - Use recovery partition (hold reset during power-on)"
else
    info "Skipping reboot. Run 'reboot' on the miner when ready."
fi
