#!/bin/sh
#
# revert_to_stock_am3_aml_s21.sh — Revert an S21 (Amlogic A113D, BM1368)
# from DCENTos back to stock Bitmain firmware.
#
#  W12-B sibling of revert_to_stock_s9.sh, but Amlogic-aware:
# am3-aml ships kernel + rootfs as a uImage at the shared AM3 rootfs
# window defined in scripts/lib/am3_geometry.sh.
# We therefore use `nandwrite -p -s <shared-offset> /dev/mtd5 <uimage>`,
# NOT `flash_erase` partition-erase. Magic readback uses the U-Boot
# legacy uImage magic (27 05 19 56) instead of UBI#.
#
# verified_revertable: false in PROFILE_TABLE.amlogic-a113d-bm1368 (W23 rename) —
# this script is CODE-COMPLETE but NOT live-tested.  will pull
# the office S21 onto the bench and run the full preflight + NAND
# backup + flash + reboot loop before flipping that flag to true.
# Reused by PROFILE_TABLE.amlogic-a113d-bm1362 (S19j Pro Amlogic, W23
# entry) — AML S11board byte-identical across S19j/S21/L9.
#
# Source for the destructive primitives:
#   - mkimage / nandwrite / fw_setenv layout per Amlogic A113D port
#     work ( "Amlogic A113D Platform Port" section).
#   - U-Boot env partition is /dev/nand_env (per
#     ).
#
# Usage:
#   ./revert_to_stock_am3_aml_s21.sh <firmware_image.tar.gz>
#
# Safety guarantees mirror revert_to_stock_s9.sh (wave-10 W10-A baseline):
#   - panic-reboot trap on INT/TERM/HUP via sysrq trigger
#   - EXIT cleanup of /tmp/stock_extract
#   - tar slip-protection flags (--no-same-owner / --no-overwrite-dir)
#   - readlink -f sanity to refuse symlinks pointing OUT of the
#     extract dir
#   - pre-flash uImage magic check
#   - post-flash uImage magic readback
#   - bootslot env flip ONLY after readback succeeds
#
# This script runs ON the miner (via SSH or serial console).

set -eu
# POSIX-sh (BusyBox ash) compatible. `pipefail` is not POSIX (bash-only);
# every command uses explicit `if ! cmd` branches instead.

DOWNLOAD_DIR="/tmp/stock_firmware_am3_aml_s21"
MAX_EXTRACTED_KB="${DCENT_STOCK_REVERT_MAX_EXTRACTED_KB:-262144}"
SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
if [ ! -r "$SCRIPT_DIR/lib/am3_geometry.sh" ]; then
    echo "ERROR: missing shared AM3 geometry file: $SCRIPT_DIR/lib/am3_geometry.sh" >&2
    echo "Copy scripts/lib/am3_geometry.sh beside this revert helper before using it." >&2
    exit 1
fi
. "$SCRIPT_DIR/lib/am3_geometry.sh"
ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"
ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"
UIMAGE_MAGIC_HEX="27051956"

echo "==========================================================="
echo "  DCENTos -> Stock Bitmain Firmware Revert (S21 / am3-aml)"
echo "==========================================================="
echo ""

# Detect current boot slot from U-Boot env.
# am3-aml uses dcent_boot_slot / firstboot per PROFILE_TABLE.
CURRENT_SLOT=$(fw_printenv -n dcent_boot_slot 2>/dev/null || echo "1")
echo "Current dcent_boot_slot: $CURRENT_SLOT"
echo ""

# Get firmware image (no auto-download for Amlogic — we don't ship
# a vendor-default URL because Bitmain's S21 stock is .bmu only and
# the operator must consciously stage it).
FW_IMAGE="${1:-}"
EXPECTED_SHA256=$(printf '%s' "${2:-}" | tr 'A-F' 'a-f')
if [ -z "$FW_IMAGE" ]; then
    echo "ERROR: No firmware image specified."
    echo "Usage: $0 /path/to/Antminer-S21-AML-release-XXXXX.tar.gz"
    exit 1
fi

if [ ! -f "$FW_IMAGE" ]; then
    echo "ERROR: Firmware image not found: $FW_IMAGE"
    exit 1
fi

echo "Firmware image: $FW_IMAGE"
echo "Image size: $(ls -lh "$FW_IMAGE" | awk '{print $5}')"
echo ""

# Confirmation
echo "WARNING: This will write stock Bitmain firmware to $ROOTFS_MTD"
echo "         offset $ROOTFS_OFFSET (uImage rootfs) and configure"
echo "         U-Boot to boot it on next reboot."
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
#  A2-LuxOS-port: panic-reboot trap. If nandwrite is interrupted
# (SIGINT, SIGKILL, OOM, parent SSH drops mid-script) the trap fires
# `echo b > /proc/sysrq-trigger` to force an immediate reboot into a
# known boot state. U-Boot will then try the previously-active slot
# since dcent_boot_slot has not been flipped yet at trap-fire time.
trap 'echo "INTERRUPTED -- forcing reboot via sysrq to recover into a clean boot state."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP

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

# Find the rootfs uImage; reject if `find` returns nothing.
# Amlogic stock tarballs typically ship `rootfs_uImage.bin` (per
#  "Boot Chain"); fall back to any *.bin or *uImage* match.
UIMAGE=$(find "$EXTRACT_DIR" -type f \( -name 'rootfs_uImage*' -o -name '*uImage*' -o -name 'rootfs*.bin' \) | head -1)
if [ -z "$UIMAGE" ]; then
    echo "ERROR: No rootfs uImage found in firmware archive"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi
# Resolve symlinks and verify the realpath is still inside EXTRACT_DIR.
UIMAGE_REAL=$(readlink -f "$UIMAGE")
case "$UIMAGE_REAL" in
    "$EXTRACT_DIR"/*) ;;
    *)
        echo "ERROR: uImage symlink escapes extract dir: $UIMAGE -> $UIMAGE_REAL"
        rm -rf "$EXTRACT_DIR"
        exit 1
        ;;
esac

# Pre-flash uImage magic check. uImage magic is 0x27 0x05 0x19 0x56
# (big-endian). nandwrite of a non-uImage payload would brick the
# slot, so refuse early.
HEAD_HEX=$(head -c 4 "$UIMAGE_REAL" | od -An -tx1 | tr -d ' \n')
if [ "$HEAD_HEX" != "$UIMAGE_MAGIC_HEX" ]; then
    echo "ERROR: rootfs payload lacks uImage magic 27051956 (got: $HEAD_HEX)"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 2: Writing uImage to $ROOTFS_MTD offset $ROOTFS_OFFSET..."
# am3-aml: NO `flash_erase` partition-erase. nandwrite writes pages
# directly with -p (skip-bad-block) at the given offset; the erase
# region is bounded by the offset+payload size.
if ! nandwrite -p -s "$ROOTFS_OFFSET" "$ROOTFS_MTD" "$UIMAGE_REAL"; then
    echo "ERROR: nandwrite failed -- rootfs slot may be partially overwritten."
    echo "DO NOT POWER CYCLE -- dcent_boot_slot has NOT been flipped, the previous slot may still boot."
    echo "Recovery: re-run this script (idempotent) OR power-cycle (previous slot may still work)."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 3: Post-write uImage magic readback..."
# Read 4 bytes back at the same offset; nanddump streams MTD bytes.
WROTE_HEX=$(nanddump -s "$ROOTFS_OFFSET" -l 4 "$ROOTFS_MTD" 2>/dev/null | tail -c 4 | od -An -tx1 | tr -d ' \n')
if [ "$WROTE_HEX" != "$UIMAGE_MAGIC_HEX" ]; then
    echo "ERROR: post-write readback at $ROOTFS_OFFSET lacks uImage magic 27051956 (got: $WROTE_HEX)"
    echo "DO NOT POWER CYCLE -- dcent_boot_slot has NOT been flipped, the previous slot may still boot."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 4: Updating U-Boot environment..."
# Amlogic stock uses dcent_boot_slot + firstboot per
# . firstboot=1 tells stock to
# run its first-boot init; cleared to 0 after stock takes over.
fw_setenv firstboot 1
# We don't toggle dcent_boot_slot itself: am3-aml uses a single rootfs
# slot at mtd5 + offset, not A/B. The flip is encoded as
# firstboot=1, the same flag stock uses to detect it owns the slot.

#  A2-LuxOS-port: clear the panic-reboot trap now that the
# destructive section has completed cleanly.
trap - INT TERM HUP

echo ""
echo "==========================================================="
echo "  Revert complete!"
echo "==========================================================="
echo ""
echo "  Stock Bitmain firmware written to $ROOTFS_MTD offset $ROOTFS_OFFSET."
echo "  firstboot env flipped. Reboot now to start stock firmware:"
echo ""
echo "    reboot"
echo ""
echo "  To undo (stay on DCENTos), the operator must restore from"
echo "  the NAND backup created by the dcentrald destructive path"
echo "  (/data/restore-backup-<ts>/) before rebooting."
echo ""

# Cleanup
rm -rf "$EXTRACT_DIR" "$DOWNLOAD_DIR"
