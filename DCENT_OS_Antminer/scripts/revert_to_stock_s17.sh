#!/bin/sh
#
# revert_to_stock_s17.sh — Revert an S17 (am2-s17 control board, BM1397+)
# from DCENTos back to stock Bitmain firmware.  W16 closure
# script — created alongside revert_to_stock_s9.sh / _am335x_bb.sh /
# _am3_aml_s21.sh / _am3_aml_s19k.sh so the
# `dcentrald-api::routes::restore_to_stock::PROFILE_TABLE` entry for
# `zynq-am2-bm1397` can point at /usr/sbin/revert_to_stock_s17.sh on
# the running miner.
#
# This script writes a stock Bitmain S17 firmware image to the inactive
# NAND slot and updates U-Boot environment to boot it on next reboot.
#
# Hardware:
#   - Control board: am2-s17 (Xilinx Zynq XC7Z010, 4-chain FPGA bitstream
#     with 3 physical hashboard slots populated)
#   - ASIC: BM1397+ (3 chains × 48 chips × 672 cores)
#   - Voltage: dsPIC33EP16GS202 framed protocol at I2C 0x20/0x21/0x22
#   - PSU: APW-class with PMBus telemetry
#
# Sysupgrade NAND layout (DCENT_OS Buildroot, mirrors S9 am1):
#   /dev/mtd4 = U-Boot env
#   /dev/mtd7 = firmware slot A
#   /dev/mtd8 = firmware slot B
#
# Usage:
#   ./revert_to_stock_s17.sh [firmware_image.tar.gz]
#
# If no firmware image is provided, the script will attempt to download
# the latest official Bitmain S17 firmware. Operators on units that
# Bitmain has stopped publishing for must supply the image manually.
#
# Safety:
#   - Writes to INACTIVE slot only (never touches running rootfs)
#   - Verifies image integrity before flashing
#   - Prints current and target partition info before proceeding
#   - Requires explicit confirmation
#   - Panic-reboot trap on SIGINT/SIGTERM/SIGHUP during destructive
#     section (forces sysrq reboot into the still-bootable active slot)
#   - Slip-protection tar flags on extraction
#   - Pre-flash UBI# magic check on the staged image
#   - Post-write UBI# magic readback before fw_setenv flips bootslot
#
# This script runs ON the miner (via SSH or serial console).

set -eu
# POSIX-sh (BusyBox ash) compatible. `pipefail` is not POSIX (bash-only);
# explicit `if !` guards around flash_erase / nandwrite / readback.

STOCK_FW_URL="https://download.antminer.com/firmware/Antminer-S17-all-Update-NF.tar.gz"
DOWNLOAD_DIR="/tmp/stock_firmware"
MTD_FIRMWARE_A="/dev/mtd7"
MTD_FIRMWARE_B="/dev/mtd8"
MAX_EXTRACTED_KB="${DCENT_STOCK_REVERT_MAX_EXTRACTED_KB:-262144}"

normalize_target_signal() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]'
}

require_exact_s17_target() {
    BOARD_TARGET=$(cat /etc/dcentos/board_target 2>/dev/null | head -1 | tr -d '[:space:]' || true)
    PLATFORM=$(cat /etc/dcentos/platform 2>/dev/null || cat /etc/bos_platform 2>/dev/null || echo unknown)
    DT_MODEL=$(tr '\000' '\n' < /proc/device-tree/model 2>/dev/null | head -1 || true)
    IDENTITY=$(printf 'board_target=%s platform=%s model=%s' "$BOARD_TARGET" "$PLATFORM" "$DT_MODEL")
    IDENTITY_NORM=$(normalize_target_signal "$IDENTITY")
    BOARD_NORM=$(normalize_target_signal "$BOARD_TARGET")

    case "$IDENTITY_NORM" in
        *s19*|*t19*|*t17*)
            echo "ERROR: $IDENTITY is not an S17 stock-revert target." >&2
            exit 1
            ;;
    esac
    case "$BOARD_NORM" in
        am2s17|am2s17p|am2s17pro|am2s17plus)
            echo "Exact S17 board target verified: $BOARD_TARGET"
            ;;
        *)
            echo "ERROR: /etc/dcentos/board_target must be am2-s17/am2-s17p before destructive S17 revert." >&2
            echo "       Observed: ${BOARD_TARGET:-missing}" >&2
            exit 1
            ;;
    esac
}

echo "============================================="
echo "  DCENTos S17 → Stock Bitmain Firmware Revert"
echo "  (am2-s17 / Zynq / BM1397+)"
echo "============================================="
echo ""

require_exact_s17_target

# Detect current boot slot from a CRC-valid U-Boot env. Do not infer a
# default: a blind fallback can select the wrong inactive slot and turn a
# stock revert into an active-rootfs overwrite.
if ! command -v fw_printenv >/dev/null 2>&1; then
    echo "ERROR: fw_printenv not found; cannot verify the current boot slot."
    exit 1
fi
if ! command -v fw_setenv >/dev/null 2>&1; then
    echo "ERROR: fw_setenv not found; refusing a revert that cannot verify and commit the env flip."
    exit 1
fi
if [ ! -f /etc/fw_env.config ]; then
    echo "ERROR: /etc/fw_env.config missing; fw_printenv/fw_setenv have no U-Boot env map."
    exit 1
fi
if ! grep -q '/dev/mtd4' /etc/fw_env.config; then
    echo "ERROR: /etc/fw_env.config does not point at /dev/mtd4, the S17 U-Boot env partition."
    exit 1
fi
BOOTENV_PRECHECK=$(fw_printenv 2>&1) || {
    echo "ERROR: fw_printenv could not read a CRC-valid U-Boot env."
    echo "$BOOTENV_PRECHECK"
    exit 1
}
case "$BOOTENV_PRECHECK" in
    *"Bad CRC"*|*"using default environment"*)
        echo "ERROR: fw_printenv reports an invalid/default U-Boot env. Refusing to infer active slot."
        exit 1
        ;;
esac

CURRENT_SLOT=$(fw_printenv -n bootslot 2>/dev/null || true)
case "$CURRENT_SLOT" in
    a)
        INACTIVE_MTD="$MTD_FIRMWARE_B"
        INACTIVE_SLOT="b"
        ;;
    b)
        INACTIVE_MTD="$MTD_FIRMWARE_A"
        INACTIVE_SLOT="a"
        ;;
    *)
        echo "ERROR: U-Boot bootslot must be exactly 'a' or 'b' (got: ${CURRENT_SLOT:-<empty>})."
        echo "Refusing to infer active slot for a destructive S17 revert."
        exit 1
        ;;
esac

echo "Current boot slot: $CURRENT_SLOT"
echo "Writing to inactive slot: $INACTIVE_SLOT ($INACTIVE_MTD)"
echo ""

# Get firmware image
FW_IMAGE="${1:-}"
EXPECTED_SHA256=$(printf '%s' "${2:-}" | tr 'A-F' 'a-f')
if [ -z "$FW_IMAGE" ]; then
    echo "No firmware image specified. Downloading official Bitmain S17 firmware..."
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
echo "WARNING: This will write stock Bitmain S17 firmware to NAND slot $INACTIVE_SLOT"
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
# Panic-reboot trap. If flash_erase or nandwrite is interrupted (SIGINT,
# SIGKILL, OOM, parent SSH drops mid-script) the trap fires
# `echo b > /proc/sysrq-trigger` to force an immediate reboot into a
# known boot state. U-Boot will then try the previously-active slot
# since bootslot has not been flipped yet at trap-fire time. Cleared
# at the end of the script after a clean fw_setenv. Mirrors the S9
# revert script's W10-A LuxOS-port pattern.
trap 'echo "INTERRUPTED — forcing reboot via sysrq to recover into a clean boot state."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP

# SIGINT/EXIT cleanup of /tmp/stock_extract so a mid-extract abort
# doesn't leak the unpacked stock tree. Mirrors the S9 W10-A
# VNish-port pattern.
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
# Extract BEFORE flash_erase so a malformed tarball aborts the script
# before the inactive slot is touched. Also use slip-protection flags
# so a tarball with `../` entries can't escape the staging dir during
# the re-extract.
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
# Refuse symlinks pointing OUT of EXTRACT_DIR.
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

# Verify the UBI image has the UBI# magic BEFORE we erase the inactive
# slot. flash_erase + failed nandwrite would leave the slot blank and
# then `fw_setenv bootslot` would flip to it = unbootable miner.
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

# Post-write UBI magic readback. Refuse to flip bootslot if the freshly-
# written slot doesn't show UBI# magic at offset 0 — a corrupt write
# that flash_erase + nandwrite both returned 0 for would otherwise
# still flip the operator into a bricked slot.
WROTE_MAGIC=$(nanddump -s 0 -l 4 "$INACTIVE_MTD" 2>/dev/null | tail -c 4 | od -An -c | tr -d ' \n')
if [ "$WROTE_MAGIC" != "UBI#" ]; then
    echo "ERROR: post-write readback of $INACTIVE_MTD lacks UBI# magic (got: $WROTE_MAGIC)"
    echo "DO NOT POWER CYCLE — bootslot has NOT been flipped, the active slot is still bootable."
    echo "Recovery: re-run this script (idempotent) OR power-cycle (active slot still works)."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 4: Updating U-Boot environment..."
FW_SETENV_SCRIPT="/tmp/dcent_s17_revert_fw_setenv_$$.env"
cat >"$FW_SETENV_SCRIPT" <<EOF
bootslot=${INACTIVE_SLOT}
upgrade_stage=
EOF
if ! fw_setenv --script "$FW_SETENV_SCRIPT"; then
    rm -f "$FW_SETENV_SCRIPT"
    echo "ERROR: fw_setenv failed to apply the boot-slot flip."
    echo "DO NOT POWER CYCLE until fw_printenv bootslot is checked."
    echo "Recovery: boot serial console and run: fw_setenv bootslot $CURRENT_SLOT"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi
rm -f "$FW_SETENV_SCRIPT"

POST_FLIP_ENV=$(fw_printenv 2>&1) || {
    echo "ERROR: fw_printenv could not verify the post-flip U-Boot env."
    echo "Recovery: boot serial console and run: fw_setenv bootslot $CURRENT_SLOT"
    rm -rf "$EXTRACT_DIR"
    exit 1
}
case "$POST_FLIP_ENV" in
    *"Bad CRC"*|*"using default environment"*)
        echo "ERROR: post-flip U-Boot env readback reports Bad CRC/default environment."
        echo "Recovery: boot serial console and run: fw_setenv bootslot $CURRENT_SLOT"
        rm -rf "$EXTRACT_DIR"
        exit 1
        ;;
esac
POST_FLIP_SLOT=$(fw_printenv -n bootslot 2>/dev/null || true)
POST_FLIP_STAGE=$(fw_printenv -n upgrade_stage 2>/dev/null || true)
if [ "$POST_FLIP_SLOT" != "$INACTIVE_SLOT" ] || [ -n "$POST_FLIP_STAGE" ]; then
    echo "ERROR: post-flip env verification failed (bootslot=$POST_FLIP_SLOT upgrade_stage=$POST_FLIP_STAGE)."
    echo "Recovery: boot serial console and run: fw_setenv bootslot $CURRENT_SLOT"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi
echo "Post-flip boot slot verified via fw_printenv: bootslot=$INACTIVE_SLOT"

# Clear the panic-reboot trap now that the destructive section has
# completed cleanly and the env flip has been verified. Future signals from here onward are
# normal-shutdown, not mid-flash interrupts.
trap - INT TERM HUP

echo ""
echo "============================================="
echo "  S17 Revert complete!"
echo "============================================="
echo ""
echo "  Stock Bitmain S17 firmware written to slot $INACTIVE_SLOT."
echo "  Reboot now to start stock firmware:"
echo ""
echo "    reboot"
echo ""
echo "  To undo (stay on DCENTos), run:"
echo "    fw_setenv bootslot $CURRENT_SLOT"
echo ""

# Cleanup
rm -rf "$EXTRACT_DIR" "$DOWNLOAD_DIR"
