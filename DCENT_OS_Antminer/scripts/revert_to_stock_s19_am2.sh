#!/bin/sh
#
# revert_to_stock_s19_am2.sh — Revert an S19 Pro / S19j Pro Zynq am2
# (XC7Z020 control board, BM1398 / BM1362) from DCENTos back to stock
# Bitmain firmware.  W19-followup closure script — created
# alongside revert_to_stock_s9.sh / _s17.sh / _am335x_bb.sh /
# _am3_aml_s21.sh / _am3_aml_s19k.sh so the
# `dcentrald-api::routes::restore_to_stock::PROFILE_TABLE` entry for
# `zynq-am2-bm1398` can point at /usr/sbin/revert_to_stock_s19_am2.sh
# on the running miner. Both S19 Pro am2 and S19j Pro Zynq am2 share
# the same XC7Z020 SoC + control-board NAND topology, so a single
# script + signature covers both miner families.
#
# This script writes a stock Bitmain S19 firmware image to the inactive
# NAND slot and updates U-Boot environment to boot it on next reboot.
#
# Hardware:
#   - Control board: am2 Zynq XC7Z020 (4-chain FPGA bitstream with 3
#     physical hashboard slots populated)
#   - ASIC: BM1398 (S19 Pro, 3 chains × 114 chips) or BM1362
#     (S19j Pro Zynq am2, 3 chains × ~76 chips)
#   - Voltage: dsPIC33EP16GS202 framed protocol at I2C 0x20/0x21/0x22
#   - PSU: APW121215a-class with PMBus telemetry (fw=0x71 etc.)
#
# Sysupgrade NAND layout (DCENT_OS Buildroot, mirrors S9 am1 + S17
# am2-s17). Source: live `a lab unit` U-Boot env extraction at
# :
#   mtdparts=pl35x-nand:8m(boot),12m(boot-failover),2m(fpga1),
#            2m(fpga2),512k(uboot_env),512k(miner_cfg),87m(recovery),
#            57m(firmware1),57m(firmware2),30m(factory)
#   /dev/mtd4 = uboot_env
#   /dev/mtd7 = firmware slot 1 (mapped to "a" for legacy compat)
#   /dev/mtd8 = firmware slot 2 (mapped to "b" for legacy compat)
#
# Bootslot env keys (PROFILE_TABLE.bootslot_env_keys):
#   `firmware` (PRIMARY — `firmware=1` boots mtd7, `firmware=2` boots mtd8;
#                from `firmware_select=if test x${firmware} = x1; then ...
#                firmware_mtd 7; else ... firmware_mtd 8;` in `a lab unit` env)
#   `bootslot` (SECONDARY — DCENT_OS / BraiinsOS Buildroot compat key
#                for any operator tooling pinned to the wave-≤11 path)
#
# Usage:
#   ./revert_to_stock_s19_am2.sh [firmware_image.tar.gz]
#
# If no firmware image is provided, the script will attempt to download
# the latest official Bitmain S19 firmware. Operators on units that
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

STOCK_FW_URL="https://service.bitmain.com/support/download?product=s19-pro"
DOWNLOAD_DIR="/tmp/stock_firmware"
MTD_FIRMWARE_A="/dev/mtd7"
MTD_FIRMWARE_B="/dev/mtd8"
MAX_EXTRACTED_KB="${DCENT_STOCK_REVERT_MAX_EXTRACTED_KB:-262144}"
# CE-408: default-unset lab/manual override. Unset = require hash-bound stock
# image provenance (no unauthenticated no-arg download, no un-hashed supplied
# image). The API restore route always supplies image+SHA and is unaffected.
ALLOW_UNVERIFIED="${DCENT_STOCK_REVERT_ALLOW_UNVERIFIED:-0}"

echo "============================================="
echo "  DCENTos S19 → Stock Bitmain Firmware Revert"
echo "  (am2 / Zynq XC7Z020 / BM1398 + BM1362)"
echo "============================================="
echo ""

# Detect current boot slot from U-Boot env. S19 am2 uses `firmware=1|2`
# as the PRIMARY key (per `a lab unit` U-Boot env extract); the legacy
# `bootslot=a|b` key is checked as a fallback for any older Buildroot
# overlays that wrote it. Map firmware=1 → "a" / firmware=2 → "b" so
# the rest of the script keeps the same a/b mental model as the S9
# and S17 reverts.
FIRMWARE_KEY=$(fw_printenv -n firmware 2>/dev/null || echo "")
if [ "$FIRMWARE_KEY" = "1" ]; then
    CURRENT_SLOT="a"
elif [ "$FIRMWARE_KEY" = "2" ]; then
    CURRENT_SLOT="b"
else
    CURRENT_SLOT=$(fw_printenv -n bootslot 2>/dev/null || echo "a")
fi

if [ "$CURRENT_SLOT" = "a" ]; then
    INACTIVE_MTD="$MTD_FIRMWARE_B"
    INACTIVE_SLOT="b"
    INACTIVE_FIRMWARE="2"
    CURRENT_FIRMWARE="1"
else
    INACTIVE_MTD="$MTD_FIRMWARE_A"
    INACTIVE_SLOT="a"
    INACTIVE_FIRMWARE="1"
    CURRENT_FIRMWARE="2"
fi

echo "Current boot slot: $CURRENT_SLOT (firmware=$CURRENT_FIRMWARE)"
echo "Writing to inactive slot: $INACTIVE_SLOT (firmware=$INACTIVE_FIRMWARE / $INACTIVE_MTD)"
echo ""

# Get firmware image
FW_IMAGE="${1:-}"
EXPECTED_SHA256=$(printf '%s' "${2:-}" | tr 'A-F' 'a-f')

# CE-408: hash-bound stock-image provenance. The unauthenticated runtime
# download (no hash) and a supplied image with no expected SHA both reach
# flash_erase/nandwrite with only the UBI# magic check as an integrity gate — a
# MITM / wrong-tarball / compromised-DNS response carrying UBI# magic would
# flash straight through on this beta-tier board. Fail closed by default; the
# API restore route always supplies image+SHA so it is byte-for-byte unchanged.
# Lab/manual override: DCENT_STOCK_REVERT_ALLOW_UNVERIFIED=1.
if [ -z "$FW_IMAGE" ] && [ "$ALLOW_UNVERIFIED" != "1" ]; then
    echo "ERROR: refusing stock revert with an unauthenticated runtime download (no local image, no SHA)." >&2
    echo "  Supply a verified image + expected SHA-256:  $0 /path/to/firmware.tar.gz <sha256>" >&2
    echo "  (lab/manual override: DCENT_STOCK_REVERT_ALLOW_UNVERIFIED=1)" >&2
    exit 1
fi
if [ -n "$FW_IMAGE" ] && [ -z "$EXPECTED_SHA256" ] && [ "$ALLOW_UNVERIFIED" != "1" ]; then
    echo "ERROR: refusing stock revert without expected SHA-256 for the supplied image." >&2
    echo "  Provide the expected hash:  $0 $FW_IMAGE <sha256>" >&2
    echo "  (lab/manual override: DCENT_STOCK_REVERT_ALLOW_UNVERIFIED=1)" >&2
    exit 1
fi

if [ -z "$FW_IMAGE" ]; then
    echo "No firmware image specified. Downloading official Bitmain S19 firmware..."
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
echo "WARNING: This will write stock Bitmain S19 firmware to NAND slot $INACTIVE_SLOT"
echo "         (firmware=$INACTIVE_FIRMWARE / $INACTIVE_MTD) and configure U-Boot to"
echo "         boot it on next reboot."
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
# since the `firmware`/`bootslot` env keys have NOT been flipped yet
# at trap-fire time. Cleared at the end of the script after a clean
# fw_setenv. Mirrors the S9 / S17 revert script W10-A LuxOS-port pattern.
trap 'echo "INTERRUPTED — forcing reboot via sysrq to recover into a clean boot state."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP

# SIGINT/EXIT cleanup of /tmp/stock_extract so a mid-extract abort
# doesn't leak the unpacked stock tree. Mirrors the S9 / S17 W10-A
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
# then `fw_setenv firmware`/`bootslot` would flip to it = unbootable miner.
UBI_MAGIC=$(head -c 4 "$UBI_REAL" | od -An -c | tr -d ' \n')
if [ "$UBI_MAGIC" != "UBI#" ]; then
    echo "ERROR: UBI image lacks UBI# magic (got: $UBI_MAGIC)"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 2: Erasing inactive NAND slot ($INACTIVE_MTD)..."
if ! flash_erase "$INACTIVE_MTD" 0 0; then
    echo "ERROR: flash_erase failed — inactive slot may be partially erased."
    echo "Recovery: re-run this script (idempotent) OR boot serial console + manual"
    echo "  fw_setenv firmware $CURRENT_FIRMWARE"
    echo "  fw_setenv bootslot $CURRENT_SLOT"
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 3: Writing firmware to NAND..."
if ! nandwrite -p "$INACTIVE_MTD" "$UBI_REAL"; then
    echo "ERROR: nandwrite failed — inactive slot is BLANK."
    echo "DO NOT POWER CYCLE — firmware/bootslot env has NOT been flipped, the active slot is still bootable."
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
    echo "DO NOT POWER CYCLE — firmware/bootslot env has NOT been flipped, the active slot is still bootable."
    echo "Recovery: re-run this script (idempotent) OR power-cycle (active slot still works)."
    rm -rf "$EXTRACT_DIR"
    exit 1
fi

echo "Step 4: Updating U-Boot environment..."
# S19 am2 uses `firmware=1|2` as the PRIMARY key; mirror to the legacy
# `bootslot=a|b` key for any operator tooling that reads it. Clear
# `upgrade_stage` so the U-Boot auto_recovery / S99upgrade path does
# not bounce us back to the active slot on the next boot.
fw_setenv firmware "$INACTIVE_FIRMWARE"
fw_setenv bootslot "$INACTIVE_SLOT"
fw_setenv upgrade_stage ""

# Clear the panic-reboot trap now that the destructive section has
# completed cleanly. Future signals from here onward are
# normal-shutdown, not mid-flash interrupts.
trap - INT TERM HUP

echo ""
echo "============================================="
echo "  S19 am2 Revert complete!"
echo "============================================="
echo ""
echo "  Stock Bitmain S19 firmware written to slot $INACTIVE_SLOT (firmware=$INACTIVE_FIRMWARE)."
echo "  Reboot now to start stock firmware:"
echo ""
echo "    reboot"
echo ""
echo "  To undo (stay on DCENTos), run:"
echo "    fw_setenv firmware $CURRENT_FIRMWARE"
echo "    fw_setenv bootslot $CURRENT_SLOT"
echo ""

# Cleanup
rm -rf "$EXTRACT_DIR" "$DOWNLOAD_DIR"
