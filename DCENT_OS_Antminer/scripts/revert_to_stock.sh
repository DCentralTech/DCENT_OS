#!/bin/sh
#
# revert_to_stock.sh — Revert an S9 from DCENTos back to stock Bitmain firmware.
#
# This script writes a stock Bitmain S9 firmware image to the inactive NAND
# slot and updates U-Boot environment to boot it on next reboot.
#
# Usage:
#   ./revert_to_stock.sh [firmware_image.tar.gz]
#
# If no firmware image is provided, the script will attempt to download
# the latest official Bitmain S9 firmware.
#
# Safety:
#   - Writes to INACTIVE slot only (never touches running rootfs)
#   - Verifies image integrity before flashing
#   - Prints current and target partition info before proceeding
#   - Requires explicit confirmation
#
# This script runs ON the miner (via SSH or serial console).

set -eu
#  W10-A: POSIX-sh (BusyBox ash) compatible. `pipefail` is not POSIX
# (bash-only); the script handles per-command failure explicitly via `if !`
# branches around flash_erase / nandwrite / readback, so dropping pipefail
# does not weaken the recovery guards.

STOCK_FW_URL="https://download.antminer.com/firmware/Antminer-S9-all-201812051512-autofreq-user-Update2UBI-NF.tar.gz"
DOWNLOAD_DIR="/tmp/stock_firmware"
MTD_FIRMWARE_A="/dev/mtd7"
MTD_FIRMWARE_B="/dev/mtd8"
MAX_EXTRACTED_KB="${DCENT_STOCK_REVERT_MAX_EXTRACTED_KB:-262144}"

echo "============================================="
echo "  DCENTos → Stock Bitmain Firmware Revert"
echo "============================================="
echo ""

# Detect current boot slot from U-Boot env
CURRENT_SLOT=$(fw_printenv -n bootslot 2>/dev/null || echo "a")
if [ "$CURRENT_SLOT" = "a" ]; then
    INACTIVE_MTD="$MTD_FIRMWARE_B"
    INACTIVE_SLOT="b"
else
    INACTIVE_MTD="$MTD_FIRMWARE_A"
    INACTIVE_SLOT="a"
fi

echo "Current boot slot: $CURRENT_SLOT"
echo "Writing to inactive slot: $INACTIVE_SLOT ($INACTIVE_MTD)"
echo ""

# Get firmware image
FW_IMAGE="${1:-}"
EXPECTED_SHA256=$(printf '%s' "${2:-}" | tr 'A-F' 'a-f')
if [ -z "$FW_IMAGE" ]; then
    echo "No firmware image specified. Downloading official Bitmain S9 firmware..."
    mkdir -p "$DOWNLOAD_DIR"
    FW_IMAGE="$DOWNLOAD_DIR/stock_firmware.tar.gz"

    if command -v wget >/dev/null 2>&1; then
        wget -O "$FW_IMAGE" "$STOCK_FW_URL" || {
            echo "ERROR: Download failed. Please provide a firmware image manually."
            echo "Usage: $0 /path/to/firmware.tar.gz"
            exit 1
        }
    elif command -v curl >/dev/null 2>&1; then
        curl -L -o "$FW_IMAGE" "$STOCK_FW_URL" || {
            echo "ERROR: Download failed. Please provide a firmware image manually."
            exit 1
        }
    else
        echo "ERROR: Neither wget nor curl available. Please provide firmware image."
        exit 1
    fi
fi

if [ ! -f "$FW_IMAGE" ]; then
    echo "ERROR: Firmware image not found: $FW_IMAGE"
    exit 1
fi

echo "Firmware image: $FW_IMAGE"
echo "Image size: $(ls -lh "$FW_IMAGE" | awk '{print $5}')"
echo ""

# Confirmation
echo "WARNING: This will write stock Bitmain firmware to NAND slot $INACTIVE_SLOT"
echo "         and configure U-Boot to boot it on next reboot."
echo ""
echo "         DCENTos will no longer be the active firmware after reboot."
echo ""
printf "Type 'REVERT' to proceed: "
read CONFIRM
if [ "$CONFIRM" != "REVERT" ]; then
    echo "Aborted."
    exit 0
fi

echo ""
#  A2-LuxOS-port: panic-reboot trap. If flash_erase or
# nandwrite is interrupted (SIGINT, SIGKILL, OOM, parent SSH drops
# mid-script) the trap fires `echo b > /proc/sysrq-trigger` to force
# an immediate reboot into a known boot state. U-Boot will then try
# the previously-active slot since bootslot has not been flipped
# yet at trap-fire time. Cleared at the end of the script after a
# clean fw_setenv. Mirrors LuxOS I-update-signature.md:286 pattern.
trap 'echo "INTERRUPTED — forcing reboot via sysrq to recover into a clean boot state."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP

#  A2-VNish-port: SIGINT/EXIT cleanup of /tmp/stock_extract
# so a mid-extract abort doesn't leak the unpacked stock tree.
trap 'rm -rf /tmp/stock_extract 2>/dev/null' EXIT

if [ -n "$EXPECTED_SHA256" ]; then
    if ! command -v sha256sum >/dev/null 2>&1; then
        echo "ERROR: sha256sum missing; refusing expected-SHA stock revert." >&2
        rm -rf "$DOWNLOAD_DIR"
        exit 1
    fi
    ACTUAL_SHA256=$(sha256sum "$FW_IMAGE" | awk '{print $1}' | tr 'A-F' 'a-f')
    if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
        echo "ERROR: firmware SHA-256 drift before extraction." >&2
        echo "  expected: $EXPECTED_SHA256" >&2
        echo "  actual:   $ACTUAL_SHA256" >&2
        rm -rf "$DOWNLOAD_DIR"
        exit 1
    fi
    echo "Firmware SHA-256 verified at extraction time."
fi

echo "Step 1: Extracting firmware archive..."
#  A1-CRITICAL-1: extract BEFORE flash_erase so a malformed
# tarball aborts the script before the inactive slot is touched.
# Also use slip-protection flags so a tarball with `../` entries
# can't escape the staging dir during the re-extract.
EXTRACT_DIR="/tmp/stock_extract"
rm -rf "$EXTRACT_DIR"
mkdir -p "$EXTRACT_DIR"
tar --no-same-owner --no-same-permissions --no-overwrite-dir \
    -xzf "$FW_IMAGE" -C "$EXTRACT_DIR"

EXTRACTED_KB=$(du -sk "$EXTRACT_DIR" | awk '{print $1}')
if [ "$EXTRACTED_KB" -gt "$MAX_EXTRACTED_KB" ]; then
    echo "ERROR: extracted firmware tree is ${EXTRACTED_KB} KiB, above cap ${MAX_EXTRACTED_KB} KiB"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi
if find "$EXTRACT_DIR" -type f -links +1 -print -quit 2>/dev/null | grep -q .; then
    echo "ERROR: firmware archive contains hard-linked files; refusing destructive revert"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

# Find the rootfs UBI image; reject if `find` returns nothing.
#  A1-CRITICAL-1: refuse symlinks pointing OUT of EXTRACT_DIR.
UBI_IMAGE=$(find "$EXTRACT_DIR" -type f \( -name "*.ubi" -o -name "rootfs*" \) | head -1)
if [ -z "$UBI_IMAGE" ]; then
    echo "ERROR: No UBI/rootfs image found in firmware archive"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi
# Resolve symlinks and verify the realpath is still inside EXTRACT_DIR.
UBI_REAL=$(readlink -f "$UBI_IMAGE")
case "$UBI_REAL" in
    "$EXTRACT_DIR"/*) ;;
    *)
        echo "ERROR: UBI image symlink escapes extract dir: $UBI_IMAGE -> $UBI_REAL"
        rm -rf "$EXTRACT_DIR"
        exit 1
        ;;
esac

#  A1-CRITICAL-3: verify the UBI image has the UBI# magic
# BEFORE we erase the inactive slot. flash_erase + failed nandwrite
# would leave the slot blank and then `fw_setenv bootslot` would
# flip to it = unbootable miner.
UBI_MAGIC=$(head -c 4 "$UBI_REAL" | od -An -c | tr -d ' \n')
if [ "$UBI_MAGIC" != "UBI#" ]; then
    echo "ERROR: UBI image lacks UBI# magic (got: $UBI_MAGIC)"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 2: Erasing inactive NAND slot ($INACTIVE_MTD)..."
if ! flash_erase "$INACTIVE_MTD" 0 0; then
    echo "ERROR: flash_erase failed — inactive slot may be partially erased."
    echo "Recovery: re-run this script (idempotent) OR boot serial console + manual fw_setenv bootslot $CURRENT_SLOT"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 3: Writing firmware to NAND..."
if ! nandwrite -p "$INACTIVE_MTD" "$UBI_REAL"; then
    echo "ERROR: nandwrite failed — inactive slot is BLANK."
    echo "DO NOT POWER CYCLE — bootslot has NOT been flipped, the active slot is still bootable."
    echo "Recovery: re-run this script (idempotent) OR power-cycle (active slot still works)."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

#  A1-CRITICAL-3: post-write UBI magic readback. Refuse to
# flip bootslot if the freshly-written slot doesn't show UBI# magic
# at offset 0 — a corrupt write that flash_erase + nandwrite both
# returned 0 for would otherwise still flip the operator into a
# bricked slot.
WROTE_MAGIC=$(nanddump -s 0 -l 4 "$INACTIVE_MTD" 2>/dev/null | tail -c 4 | od -An -c | tr -d ' \n')
if [ "$WROTE_MAGIC" != "UBI#" ]; then
    echo "ERROR: post-write readback of $INACTIVE_MTD lacks UBI# magic (got: $WROTE_MAGIC)"
    echo "DO NOT POWER CYCLE — bootslot has NOT been flipped, the active slot is still bootable."
    echo "Recovery: re-run this script (idempotent) OR power-cycle (active slot still works)."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 4: Updating U-Boot environment..."
fw_setenv bootslot "$INACTIVE_SLOT"
fw_setenv upgrade_stage ""

#  A2-LuxOS-port: clear the panic-reboot trap now that the
# destructive section has completed cleanly. Future signals from
# here onward are normal-shutdown, not mid-flash interrupts.
trap - INT TERM HUP

echo ""
echo "============================================="
echo "  Revert complete!"
echo "============================================="
echo ""
echo "  Stock Bitmain firmware written to slot $INACTIVE_SLOT."
echo "  Reboot now to start stock firmware:"
echo ""
echo "    reboot"
echo ""
echo "  To undo (stay on DCENTos), run:"
echo "    fw_setenv bootslot $CURRENT_SLOT"
echo ""

# Cleanup
rm -rf "$EXTRACT_DIR" "$DOWNLOAD_DIR"
