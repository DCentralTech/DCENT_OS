#!/bin/bash
#
# write_sd_card.sh -- Write DCENTos SD card files to a physical SD card
# D-Central Technologies, 2026
#
# Safely writes the SD card image (or individual files) to a MicroSD card
# for booting on an Antminer S9.
#
# Two write modes:
#   --image: Flash dcentos-sd.img directly (like balenaEtcher)
#            Overwrites entire card. Requires build_sd_image.sh --disk-image.
#
#   --files: Format card as FAT32 and copy individual files (default)
#            Only writes to first partition. Requires mount access.
#
# Safety:
#   - Refuses to write to devices larger than 64 GB (unlikely to be an SD card)
#   - Refuses to write to devices with mounted partitions
#   - Asks for explicit confirmation with device details
#   - Verifies written data with checksums
#
# Usage:
#   ./write_sd_card.sh /dev/sdX              # Format + copy files (default)
#   ./write_sd_card.sh /dev/sdX --image      # Flash .img file
#   ./write_sd_card.sh --list                 # List candidate SD card devices
#   ./write_sd_card.sh --help
#
# Requires: Root/sudo access (for block device writes)
#

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIRMWARE_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FIRMWARE_DIR")"
SD_OUTPUT_DIR="$FIRMWARE_DIR/buildroot/output/images/sd_card"

# Safety limits
MAX_DEVICE_SIZE_GB=64           # Refuse devices larger than this
MIN_DEVICE_SIZE_MB=32           # Refuse devices smaller than this

# Files needed for --files mode. These match build_sd_image.sh's PROVEN legacy
# bootm K/R/D flow (raw system.bit + fpga load, then bootm uImage
# uramdisk.image.gz devicetree.dtb). The old fit.itb + system.bit.gz FIT/unzip
# flow was retired 2026-06-10 (the shipped 2014.01 BOOT.BIN has no `unzip`
# command), so those files no longer exist in $SD_OUTPUT_DIR.
SD_CARD_FILES=(uEnv.txt system.bit uImage uramdisk.image.gz devicetree.dtb)
# BOOT.BIN is optional (only for Approach B / standalone boot)
OPTIONAL_FILES=(BOOT.BIN)

# Colors
RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
CYAN='\033[1;36m'
BOLD='\033[1m'
NC='\033[0m'

info()    { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }

# =============================================================================
# Parse Arguments
# =============================================================================

MODE="files"    # default mode
DEVICE=""
VERIFY=true
# CE-382: run-scoped marker — set when post-write verification (image SHA or
# per-file/BOOT.BIN checksum) FAILS, so the success banner can fail closed.
VERIFY_FAILED=""

usage() {
    echo "Usage: $(basename "$0") <device> [OPTIONS]"
    echo "       $(basename "$0") --list"
    echo ""
    echo "Write DCENTos boot files to a MicroSD card for Antminer S9."
    echo ""
    echo "Arguments:"
    echo "  <device>       Block device (e.g., /dev/sdb, /dev/mmcblk0)"
    echo ""
    echo "Options:"
    echo "  --image        Flash dcentos-sd.img to device (full overwrite)"
    echo "  --files        Format FAT32 and copy files (default)"
    echo "  --no-verify    Skip post-write verification"
    echo "  --list         List removable block devices (candidate SD cards)"
    echo "  --help         Show this help"
    echo ""
    echo "Examples:"
    echo "  $(basename "$0") --list                    # Find your SD card device"
    echo "  $(basename "$0") /dev/sdb                  # Format + copy boot files"
    echo "  $(basename "$0") /dev/sdb --image          # Flash .img to card"
    echo "  sudo $(basename "$0") /dev/mmcblk0         # Direct on Linux"
}

# Handle --list and --help before device argument
for arg in "$@"; do
    case "$arg" in
        --list)
            echo "=== Removable Block Devices ==="
            echo ""
            if command -v lsblk >/dev/null 2>&1; then
                lsblk -o NAME,SIZE,TYPE,RM,MOUNTPOINT,MODEL | head -1
                lsblk -o NAME,SIZE,TYPE,RM,MOUNTPOINT,MODEL | grep -E "disk\s+1" || \
                    echo "(No removable disks found. Is your SD card reader connected?)"
                echo ""
                echo "Look for removable (RM=1) disk devices."
                echo "Typical SD card devices: /dev/sdb, /dev/sdc, /dev/mmcblk0"
            else
                echo "lsblk not available. Check dmesg for recently attached devices:"
                echo "  dmesg | tail -20"
            fi
            exit 0
            ;;
        --help|-h)
            usage
            exit 0
            ;;
    esac
done

# Parse device and options
for arg in "$@"; do
    case "$arg" in
        --image)     MODE="image" ;;
        --files)     MODE="files" ;;
        --no-verify) VERIFY=false ;;
        --list|--help|-h) ;; # already handled
        /dev/*)      DEVICE="$arg" ;;
        *)           error "Unknown argument: $arg\nRun with --help for usage." ;;
    esac
done

if [ -z "$DEVICE" ]; then
    echo "Error: No device specified."
    echo ""
    usage
    exit 1
fi

# =============================================================================
# Safety Checks
# =============================================================================

echo -e "${CYAN}${BOLD}=== DCENTos SD Card Writer ===${NC}"
echo ""

# Must be root for block device operations
if [ "$(id -u)" != "0" ]; then
    error "This script must be run as root (or with sudo).\n  sudo $(basename "$0") $*"
fi

# Device must exist
if [ ! -b "$DEVICE" ]; then
    error "Device $DEVICE does not exist or is not a block device.\nRun '$(basename "$0") --list' to find your SD card."
fi

# Device must be a whole disk, not a partition
# /dev/sdb is ok, /dev/sdb1 is not. /dev/mmcblk0 is ok, /dev/mmcblk0p1 is not.
case "$DEVICE" in
    /dev/sd[a-z])       ;; # SCSI/USB disk
    /dev/mmcblk[0-9])   ;; # MMC device
    /dev/nvme*n[0-9])   ;; # NVMe (unlikely for SD but handle it)
    *)
        # Check if it looks like a partition
        if echo "$DEVICE" | grep -qE '[0-9]$'; then
            PARENT=$(echo "$DEVICE" | sed 's/[0-9]*$//' | sed 's/p$//')
            warn "Warning: $DEVICE looks like a partition, not a whole disk."
            warn "Did you mean $PARENT?"
            read -p "Continue with $DEVICE anyway? (yes/no): " confirm
            [ "$confirm" = "yes" ] || exit 0
        fi
        ;;
esac

# Get device size
DEVICE_SIZE_BYTES=0
if command -v blockdev >/dev/null 2>&1; then
    DEVICE_SIZE_BYTES=$(blockdev --getsize64 "$DEVICE" 2>/dev/null || echo 0)
elif [ -f "/sys/block/$(basename "$DEVICE")/size" ]; then
    SECTORS=$(cat "/sys/block/$(basename "$DEVICE")/size" 2>/dev/null || echo 0)
    DEVICE_SIZE_BYTES=$((SECTORS * 512))
fi

if [ "$DEVICE_SIZE_BYTES" -eq 0 ]; then
    warn "Could not determine device size. Proceeding with caution."
else
    DEVICE_SIZE_MB=$((DEVICE_SIZE_BYTES / 1024 / 1024))
    DEVICE_SIZE_GB=$((DEVICE_SIZE_BYTES / 1024 / 1024 / 1024))

    # Refuse if too small
    if [ "$DEVICE_SIZE_MB" -lt "$MIN_DEVICE_SIZE_MB" ]; then
        error "Device $DEVICE is too small (${DEVICE_SIZE_MB}MB).\nMinimum ${MIN_DEVICE_SIZE_MB}MB required."
    fi

    # Refuse if too large (probably not an SD card)
    if [ "$DEVICE_SIZE_GB" -gt "$MAX_DEVICE_SIZE_GB" ]; then
        error "Device $DEVICE is ${DEVICE_SIZE_GB}GB -- too large to be an SD card.\nMaximum ${MAX_DEVICE_SIZE_GB}GB allowed.\n\nIf this really is your SD card, this safety check may need adjusting.\nThis is to prevent accidentally overwriting your hard drive."
    fi

    info "Device: $DEVICE (${DEVICE_SIZE_MB}MB / ${DEVICE_SIZE_GB}GB)"
fi

# Check for mounted partitions
MOUNTED_PARTS=$(mount | grep "^${DEVICE}" | awk '{print $1 " on " $3}' || true)
if [ -n "$MOUNTED_PARTS" ]; then
    warn "WARNING: Device has mounted partitions:"
    echo "$MOUNTED_PARTS" | while IFS= read -r line; do
        echo "    $line"
    done
    echo ""
    read -p "Unmount all partitions and continue? (yes/no): " confirm
    if [ "$confirm" = "yes" ]; then
        mount | grep "^${DEVICE}" | awk '{print $1}' | while IFS= read -r part; do
            info "Unmounting $part..."
            umount "$part" 2>/dev/null || umount -l "$part" 2>/dev/null || true
        done
    else
        error "Aborted. Unmount partitions manually and retry."
    fi
fi

# Get device model for display
DEVICE_MODEL=""
if command -v lsblk >/dev/null 2>&1; then
    DEVICE_MODEL=$(lsblk -dno MODEL "$DEVICE" 2>/dev/null | xargs || true)
fi

# =============================================================================
# Check Source Files
# =============================================================================

info "Checking source files in $SD_OUTPUT_DIR/..."

if [ "$MODE" = "image" ]; then
    IMG_FILE="$SD_OUTPUT_DIR/dcentos-sd.img"
    if [ ! -f "$IMG_FILE" ]; then
        error "Disk image not found: $IMG_FILE\nBuild with: firmware/scripts/build_sd_image.sh --disk-image"
    fi
    IMG_SIZE=$(stat -c%s "$IMG_FILE" 2>/dev/null || stat -f%z "$IMG_FILE")
    info "  dcentos-sd.img: $((IMG_SIZE / 1024 / 1024))MB"
else
    MISSING=()
    for f in "${SD_CARD_FILES[@]}"; do
        if [ ! -f "$SD_OUTPUT_DIR/$f" ]; then
            MISSING+=("$f")
        fi
    done
    if [ ${#MISSING[@]} -gt 0 ]; then
        error "Missing required files in $SD_OUTPUT_DIR/:\n$(printf '  - %s\n' "${MISSING[@]}")\n\nBuild with: firmware/scripts/build_sd_image.sh"
    fi

    HAS_BOOTBIN=false
    if [ -f "$SD_OUTPUT_DIR/BOOT.BIN" ]; then
        HAS_BOOTBIN=true
    fi
fi

# =============================================================================
# Confirmation
# =============================================================================

echo ""
echo -e "${RED}${BOLD}========================================${NC}"
echo -e "${RED}${BOLD}  WARNING: DESTRUCTIVE OPERATION${NC}"
echo -e "${RED}${BOLD}========================================${NC}"
echo ""
echo "  Device:     $DEVICE"
if [ -n "$DEVICE_MODEL" ]; then
    echo "  Model:      $DEVICE_MODEL"
fi
if [ "$DEVICE_SIZE_BYTES" -gt 0 ]; then
    echo "  Size:       ${DEVICE_SIZE_MB}MB (${DEVICE_SIZE_GB}GB)"
fi
echo "  Mode:       $MODE"
if [ "$MODE" = "image" ]; then
    echo "  Image:      dcentos-sd.img ($((IMG_SIZE / 1024 / 1024))MB)"
else
    echo "  Files:      ${SD_CARD_FILES[*]}$(if $HAS_BOOTBIN; then echo ' BOOT.BIN'; fi)"
fi
echo ""
echo -e "  ${RED}ALL DATA ON $DEVICE WILL BE DESTROYED${NC}"
echo ""
read -p "Type 'YES' (uppercase) to confirm: " confirm
if [ "$confirm" != "YES" ]; then
    echo "Aborted."
    exit 0
fi
echo ""

# =============================================================================
# Write to SD Card
# =============================================================================

if [ "$MODE" = "image" ]; then
    # =========================================================================
    # Image mode: dd the .img file directly
    # =========================================================================

    info "Flashing dcentos-sd.img to $DEVICE..."
    info "  This will take a few seconds..."

    dd if="$IMG_FILE" of="$DEVICE" bs=4M status=progress conv=fsync 2>&1

    # Ensure all writes are flushed
    sync

    info "Image written successfully."

    # Verification
    if $VERIFY; then
        info "Verifying written data..."
        EXPECTED_SHA=$(sha256sum "$IMG_FILE" | awk '{print $1}')
        WRITTEN_SHA=$(dd if="$DEVICE" bs=4M count="$((IMG_SIZE / 4194304 + 1))" 2>/dev/null | \
            head -c "$IMG_SIZE" | sha256sum | awk '{print $1}')

        if [ "$EXPECTED_SHA" = "$WRITTEN_SHA" ]; then
            info "  Verification PASSED (SHA256 match)"
        else
            warn "  Verification FAILED"
            warn "    Expected: $EXPECTED_SHA"
            warn "    Written:  $WRITTEN_SHA"
            warn "  The SD card may be defective. Try a different card."
            VERIFY_FAILED=1
        fi
    fi

else
    # =========================================================================
    # Files mode: Format FAT32 + copy files
    # =========================================================================

    # Step 1: Create partition table
    info "Creating partition table on $DEVICE..."
    # Wipe first 1MB to clear old partition tables / boot sectors
    dd if=/dev/zero of="$DEVICE" bs=1M count=1 2>/dev/null

    # Create a single FAT32 partition (type 0x0B, bootable)
    # Start at sector 2048 (1MB aligned) for compatibility
    sfdisk "$DEVICE" << 'SFDISK_EOF' 2>/dev/null
label: dos
unit: sectors

start=2048, type=0b, bootable
SFDISK_EOF

    # Wait for kernel to re-read partition table
    partprobe "$DEVICE" 2>/dev/null || true
    sleep 1

    # Determine partition device name
    # /dev/sdb  -> /dev/sdb1
    # /dev/mmcblk0 -> /dev/mmcblk0p1
    PART_DEVICE=""
    if [ -b "${DEVICE}1" ]; then
        PART_DEVICE="${DEVICE}1"
    elif [ -b "${DEVICE}p1" ]; then
        PART_DEVICE="${DEVICE}p1"
    else
        # Wait a moment and try again (udev may need time)
        sleep 2
        partprobe "$DEVICE" 2>/dev/null || true
        if [ -b "${DEVICE}1" ]; then
            PART_DEVICE="${DEVICE}1"
        elif [ -b "${DEVICE}p1" ]; then
            PART_DEVICE="${DEVICE}p1"
        else
            error "Could not find partition device after creating partition table.\nExpected ${DEVICE}1 or ${DEVICE}p1"
        fi
    fi

    info "  Partition: $PART_DEVICE"

    # Step 2: Format as FAT32
    info "Formatting $PART_DEVICE as FAT32..."
    mkfs.vfat -F 32 -n "DCENTOS" "$PART_DEVICE" >/dev/null

    # Step 3: Mount and copy files
    MOUNT_POINT=$(mktemp -d -t dcentos-sd-XXXXXX)
    info "Mounting $PART_DEVICE at $MOUNT_POINT..."
    mount "$PART_DEVICE" "$MOUNT_POINT"

    info "Copying files..."
    COPIED=0

    # Copy BOOT.BIN first if present (must be first file on FAT for BootROM)
    if $HAS_BOOTBIN; then
        cp "$SD_OUTPUT_DIR/BOOT.BIN" "$MOUNT_POINT/"
        FSIZE=$(stat -c%s "$SD_OUTPUT_DIR/BOOT.BIN" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/BOOT.BIN")
        info "  BOOT.BIN         ($((FSIZE / 1024)) KB)"
        COPIED=$((COPIED + 1))
    fi

    # Copy required files
    for f in "${SD_CARD_FILES[@]}"; do
        cp "$SD_OUTPUT_DIR/$f" "$MOUNT_POINT/"
        FSIZE=$(stat -c%s "$SD_OUTPUT_DIR/$f" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/$f")
        printf "  %-18s (%s KB)\n" "$f" "$((FSIZE / 1024))"
        COPIED=$((COPIED + 1))
    done

    # Flush and unmount
    sync
    umount "$MOUNT_POINT"
    rmdir "$MOUNT_POINT"

    info "$COPIED files written."

    # Verification
    if $VERIFY; then
        info "Verifying written files..."
        VERIFY_MOUNT=$(mktemp -d -t dcentos-verify-XXXXXX)
        mount -o ro "$PART_DEVICE" "$VERIFY_MOUNT"

        ALL_OK=true
        for f in "${SD_CARD_FILES[@]}"; do
            if [ -f "$VERIFY_MOUNT/$f" ]; then
                ORIG_SHA=$(sha256sum "$SD_OUTPUT_DIR/$f" | awk '{print $1}')
                CARD_SHA=$(sha256sum "$VERIFY_MOUNT/$f" | awk '{print $1}')
                if [ "$ORIG_SHA" = "$CARD_SHA" ]; then
                    info "  $f: OK"
                else
                    warn "  $f: MISMATCH"
                    ALL_OK=false
                fi
            else
                warn "  $f: NOT FOUND ON CARD"
                ALL_OK=false
            fi
        done

        if $HAS_BOOTBIN; then
            if [ -f "$VERIFY_MOUNT/BOOT.BIN" ]; then
                ORIG_SHA=$(sha256sum "$SD_OUTPUT_DIR/BOOT.BIN" | awk '{print $1}')
                CARD_SHA=$(sha256sum "$VERIFY_MOUNT/BOOT.BIN" | awk '{print $1}')
                if [ "$ORIG_SHA" = "$CARD_SHA" ]; then
                    info "  BOOT.BIN: OK"
                else
                    warn "  BOOT.BIN: MISMATCH"
                    ALL_OK=false
                fi
            else
                # BOOT.BIN was present in the source (HAS_BOOTBIN) but is
                # absent on the card — the copy silently failed. Fail closed.
                warn "  BOOT.BIN: NOT FOUND ON CARD"
                ALL_OK=false
            fi
        fi

        umount "$VERIFY_MOUNT"
        rmdir "$VERIFY_MOUNT"

        if $ALL_OK; then
            info "  Verification PASSED (all checksums match)"
        else
            warn "  Verification FAILED for some files. Try a different SD card."
            VERIFY_FAILED=1
        fi
    fi
fi

# =============================================================================
# CE-382: fail closed on a post-write verification failure
# =============================================================================
#
# Both write modes above only warn() on a checksum mismatch / missing file and
# fall through, so the SD-Card-Ready banner + boot/next-steps used to
# print UNCONDITIONALLY even after a silently-failed verify — and the Approach-A
# next-steps then tell the operator to `fw_setenv sd_boot yes` + reboot into a
# corrupt SD image. When verification RAN and FAILED, refuse here so the success
# banner never prints. The explicit --no-verify lab opt-out is preserved (its
# banner wording is downgraded below); the happy path (verify PASSED) is
# byte-unchanged.
if $VERIFY && [ -n "$VERIFY_FAILED" ]; then
    error "Post-write verification FAILED — SD card is NOT ready; do not boot from it. Try a different card, or re-run. (Lab override: --no-verify skips verification entirely.)"
fi

# =============================================================================
# Success Summary
# =============================================================================

echo ""
if $VERIFY; then
    echo -e "${GREEN}${BOLD}=== SD Card Ready ===${NC}"
else
    echo -e "${YELLOW}${BOLD}=== SD Card Written (UNVERIFIED — --no-verify, checksum not confirmed) ===${NC}"
fi
echo ""
echo "  Device:  $DEVICE"

if [ -f "$SD_OUTPUT_DIR/BOOT.BIN" ]; then
    echo ""
    echo -e "  ${BOLD}Boot mode: Standalone (Approach B)${NC}"
    echo "  Next steps:"
    echo "    1. Remove SD card from reader"
    echo "    2. Power off the Antminer S9"
    echo "    3. Move JP4 jumper to LEFT position (SD boot mode)"
    echo "    4. Insert SD card into control board MicroSD slot"
    echo "    5. Power on and wait ~60 seconds"
    echo "    6. SSH: root@<miner_ip> (password: dcentral)"
    echo ""
    echo "  To revert to NAND boot:"
    echo "    Power off, remove SD card, restore JP4 to RIGHT"
else
    echo ""
    echo -e "  ${BOLD}Boot mode: BraiinsOS Piggyback (Approach A)${NC}"
    echo "  Next steps:"
    echo "    1. Remove SD card from reader"
    echo "    2. Insert SD card into S9 control board MicroSD slot"
    echo "    3. Enable SD boot via SSH:"
    echo "         ssh root@<miner_ip> 'fw_setenv sd_boot yes'"
    echo "    4. Reboot the miner:"
    echo "         ssh root@<miner_ip> 'reboot'"
    echo "    5. Wait ~60 seconds"
    echo "    6. SSH: root@<miner_ip> (password: dcentral)"
    echo ""
    echo "  To revert to BraiinsOS:"
    echo "    ssh root@<ip> 'fw_setenv sd_boot'  OR  remove SD card and reboot"
fi
echo ""
