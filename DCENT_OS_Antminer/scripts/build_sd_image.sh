#!/bin/bash
#
# build_sd_image.sh -- Build DCENTos SD card boot image for Antminer S9 (Zynq-7010)
# D-Central Technologies, 2026
#
# This script creates a bootable SD card image using:
#   - BraiinsOS boot chain: kernel 4.4.92, FPGA bitstream, device tree
#   - Our DCENTos rootfs: built by Buildroot (ext2.gz or squashfs)
#
# WHY SD card boot instead of NAND flash:
#   - Non-destructive: NAND is never modified, zero brick risk
#   - Fast iteration: rebuild rootfs, copy to SD, reboot (~60s cycle)
#   - Reversible: remove SD card and boot stock/BraiinsOS from NAND
#   - Perfect for the research firmware development phase
#
# WHY BraiinsOS kernel (4.4.92) instead of stock (3.14.0-xilinx):
#   - UIO support eliminates all kernel module dependencies
#   - Kernel, FPGA bitstream, and DTB are a matched, proven set
#   - No bitmain_axi.ko or fpga_mem_driver.ko needed
#
# Two modes:
#   Default: standalone SD boot -- current primary S9 tester/development path
#     Files: BOOT.BIN, uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz (FAT32 SD card)
#     Requires: JP4 jumper moved to SD boot position
#
#   --piggyback: BraiinsOS compatibility path -- requires BraiinsOS on NAND
#     Files: uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz (FAT32 SD card)
#     Enable: ssh root@<ip> 'fw_setenv sd_boot yes'
#
# Usage:
#   ./build_sd_image.sh                # standalone SD boot (default)
#   ./build_sd_image.sh --piggyback    # BraiinsOS compatibility mode
#   ./build_sd_image.sh --standalone   # explicit standalone mode
#   ./build_sd_image.sh --disk-image   # Also create .img file with partition table
#   ./build_sd_image.sh --help
#
# Prerequisites:
#   - Extracted BraiinsOS boot components in extractions/s9/
#     (run: firmware/scripts/extract_boot_components.sh <braiins_miner_ip>)
#   - Built DCENTos rootfs (run: make in firmware/)
#   - Host tools: mkimage, dumpimage (from u-boot-tools), gzip
#   - For --disk-image: mkfs.vfat, sfdisk (from dosfstools, util-linux)
#

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIRMWARE_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FIRMWARE_DIR")"
WORKSPACE_ROOT="$(dirname "$PROJECT_ROOT")"
EXTRACTIONS_DIR="$PROJECT_ROOT/extractions/s9"
BUILDROOT_OUTPUT="$FIRMWARE_DIR/buildroot/output/images"
SD_OUTPUT_DIR="$BUILDROOT_OUTPUT/sd_card"

# Fallback extraction location used by this workspace today
KB_EXTRACTIONS_DIR="$WORKSPACE_ROOT/extractions/s9"

# FIT image input files (extracted from BraiinsOS recovery FIT)
RECOVERY_FIT="$EXTRACTIONS_DIR/mtd6_recovery.bin"
FPGA_BITSTREAM="$EXTRACTIONS_DIR/mtd2_fpga1.bin"
DEVICE_TREE="$EXTRACTIONS_DIR/s9_devicetree.dtb"
EXTRACTED_KERNEL="$EXTRACTIONS_DIR/kernel.bin"

# For standalone BOOT.BIN (default standalone path)
# Prefer the proven BraiinsOS SD recovery BOOT.BIN (~2.5 MB, contains FSBL+FPGA+U-Boot).
# The trimmed-mtd0 approach (FSBL-only, ~79 KB) does NOT work for SD boot because
# the Zynq BootROM loads FSBL from BOOT.BIN, but FSBL cannot chain-load U-Boot
# without it being embedded in the same boot image.
SD_RECOVERY_BOOTBIN="$EXTRACTIONS_DIR/sd_recovery_BOOT.BIN"
KB_SD_RECOVERY_BOOTBIN="$KB_EXTRACTIONS_DIR/sd_recovery_BOOT.BIN"
FSBL_IMAGE="$EXTRACTIONS_DIR/mtd0_boot.bin"
UBOOT_IMAGE="$EXTRACTIONS_DIR/mtd1_uboot.bin"

# Rootfs candidates (prefer Buildroot ext2 if present, otherwise use squashfs artifacts)
ROOTFS_EXT2_GZ="$BUILDROOT_OUTPUT/rootfs.ext2.gz"
ROOTFS_SQUASHFS="$BUILDROOT_OUTPUT/rootfs.squashfs"
ROOTFS_FALLBACK_SQUASHFS="$FIRMWARE_DIR/dcentos_rootfs.squashfs"

ROOTFS_INPUT=""
ROOTFS_STAGE_NAME=""
ROOTFS_BOOTARGS_MODE=""
ROOTFS_FIT_COMPRESSION=""
ROOTFS_DESCRIPTION=""

# Zynq memory map (from BraiinsOS U-Boot env, verified)
KERNEL_LOAD_ADDR="0x00008000"
KERNEL_ENTRY_ADDR="0x00008000"
UBOOT_LOAD_ADDR="0x04000000"
UBOOT_ENTRY_ADDR="0x04000000"

# FSBL binary extraction offsets (verified from mtd0_boot.bin analysis)
# Magic 0x665599AA at 0x20, "XNLX" at 0x24, FSBL data at 0x8C0
FSBL_DATA_OFFSET=2240        # 0x8C0
FSBL_DATA_SIZE=79263         # 0x1359F

# U-Boot binary extraction (64-byte legacy image header)
UBOOT_HEADER_SIZE=64
UBOOT_DATA_SIZE=576788       # 0x8C914

# ARM zImage magic: 0x016F2818 at offset +0x24 from start of kernel.
# xxd prints the raw on-disk byte order, so the expected byte sequence is 18 28 6f 01.
ZIMAGE_MAGIC_OFFSET=36       # 0x24
ZIMAGE_MAGIC_RAW="18286f01"
ZIMAGE_MAGIC_VALUE="016f2818"

# FIT kernel@1 extraction fallback (from docs/research/architecture/SD_CARD_IMAGE_BUILD.md)
FIT_KERNEL_DATA_OFFSET=228
FIT_KERNEL_DATA_SIZE=2826928

# SD card disk image size (only if --disk-image)
DISK_IMAGE_SIZE_MB=64

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
header()  { echo -e "\n${CYAN}${BOLD}=== $* ===${NC}"; }

# =============================================================================
# Parse Arguments
# =============================================================================

STANDALONE=true
DISK_IMAGE=false

usage() {
    echo "Usage: $(basename "$0") [OPTIONS]"
    echo ""
    echo "Build a bootable SD card image for DCENTos on Antminer S9."
    echo ""
    echo "Options:"
    echo "  --standalone    Explicitly select standalone SD boot (default)"
    echo "  --piggyback     Select legacy BraiinsOS compatibility mode"
    echo "                  (requires BraiinsOS already on NAND)"
    echo "  --disk-image    Also create a raw .img file with FAT32 partition table"
    echo "                  (can be flashed with dd, balenaEtcher, or Rufus)"
    echo "  --help          Show this help message"
    echo ""
    echo "Output: $SD_OUTPUT_DIR/"
    echo ""
    echo "Standalone SD boot (default):"
    echo "  1. Format SD card as FAT32"
    echo "  2. Copy BOOT.BIN, uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz to SD root"
    echo "  3. Direct SD boot: move JP4 jumper to LEFT, insert SD card, power on"
    echo "     (NOTE: the old 'keep JP4 in NAND + insert SD' fallback does NOT work --"
    echo "      stock 2014.01 nandboot never reads the SD; use JP4-LEFT or piggyback below.)"
    echo ""
    echo "Piggyback compatibility mode (--piggyback, BraiinsOS-NAND S9 only):"
    echo "  1. Format SD card as FAT32"
    echo "  2. Copy uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz to SD root"
    echo "  3. On miner: fw_setenv sd_boot yes"
    echo "  4. Insert SD card, reboot"
}

for arg in "$@"; do
    case "$arg" in
        --standalone)  STANDALONE=true ;;
        --piggyback)   STANDALONE=false ;;
        --disk-image)  DISK_IMAGE=true ;;
        --help|-h)     usage; exit 0 ;;
        *)             echo "Unknown option: $arg"; usage; exit 1 ;;
    esac
done

# =============================================================================
# Phase 1: Validate Prerequisites
# =============================================================================

header "DCENTos SD Card Image Builder"

echo "Mode: $(if $STANDALONE; then echo 'Standalone SD boot (default)'; else echo 'BraiinsOS compatibility piggyback'; fi)"
echo "Disk image: $(if $DISK_IMAGE; then echo 'Yes'; else echo 'No (files only)'; fi)"
echo ""

# Check required host tools
info "Checking host tools..."
MISSING_TOOLS=()

for tool in mkimage dumpimage gzip; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        if [ "$tool" = "gzip" ]; then
            MISSING_TOOLS+=("gzip")
        else
            MISSING_TOOLS+=("$tool (from u-boot-tools)")
        fi
    fi
done

if $DISK_IMAGE; then
    for tool in mkfs.vfat sfdisk; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            MISSING_TOOLS+=("$tool (from dosfstools/util-linux)")
        fi
    done
    if [ "$(id -u)" != "0" ] && ! command -v mcopy >/dev/null 2>&1; then
        MISSING_TOOLS+=("mcopy (from mtools)")
    fi
fi

if [ ${#MISSING_TOOLS[@]} -gt 0 ]; then
    error "Missing required tools:\n$(printf '  - %s\n' "${MISSING_TOOLS[@]}")\n\nInstall with:\n  sudo apt install u-boot-tools gzip dosfstools mtools"
fi

# Check extracted boot components
info "Checking extracted boot components..."
if [ ! -d "$EXTRACTIONS_DIR" ] && [ -d "$KB_EXTRACTIONS_DIR" ]; then
    warn "Using knowledge-base extraction fallback: $KB_EXTRACTIONS_DIR"
    EXTRACTIONS_DIR="$KB_EXTRACTIONS_DIR"
    RECOVERY_FIT="$EXTRACTIONS_DIR/mtd6_recovery.bin"
    FPGA_BITSTREAM="$EXTRACTIONS_DIR/mtd2_fpga1.bin"
    DEVICE_TREE="$EXTRACTIONS_DIR/s9_devicetree.dtb"
    EXTRACTED_KERNEL="$EXTRACTIONS_DIR/kernel.bin"
    SD_RECOVERY_BOOTBIN="$EXTRACTIONS_DIR/sd_recovery_BOOT.BIN"
    FSBL_IMAGE="$EXTRACTIONS_DIR/mtd0_boot.bin"
    UBOOT_IMAGE="$EXTRACTIONS_DIR/mtd1_uboot.bin"
fi

for file in "$FPGA_BITSTREAM" "$RECOVERY_FIT" "$DEVICE_TREE"; do
    if [ ! -f "$file" ]; then
        error "Missing: $file\nRun: firmware/scripts/extract_boot_components.sh <braiins_miner_ip>"
    fi
done

if $STANDALONE; then
    # Resolve the proven SD recovery BOOT.BIN (FSBL+FPGA+U-Boot, ~2.5 MB)
    RESOLVED_SD_BOOTBIN=""
    if [ -f "$SD_RECOVERY_BOOTBIN" ]; then
        RESOLVED_SD_BOOTBIN="$SD_RECOVERY_BOOTBIN"
    elif [ -f "$KB_SD_RECOVERY_BOOTBIN" ]; then
        RESOLVED_SD_BOOTBIN="$KB_SD_RECOVERY_BOOTBIN"
        warn "Using knowledge-base SD recovery BOOT.BIN: $KB_SD_RECOVERY_BOOTBIN"
    fi

    if [ -z "$RESOLVED_SD_BOOTBIN" ]; then
        error "Missing proven SD recovery BOOT.BIN.\n  Checked: $SD_RECOVERY_BOOTBIN\n           $KB_SD_RECOVERY_BOOTBIN\n\nPlace the BraiinsOS SD recovery BOOT.BIN (~2.5 MB) at one of those paths.\nDo NOT use the trimmed mtd0_boot.bin (FSBL-only, ~79 KB) -- it cannot chain-load U-Boot."
    fi
fi

# Resolve rootfs input
if [ -f "$ROOTFS_EXT2_GZ" ]; then
    ROOTFS_INPUT="$ROOTFS_EXT2_GZ"
    ROOTFS_STAGE_NAME="rootfs.ext2.gz"
    ROOTFS_BOOTARGS_MODE="rw rootfstype=ext2"
    ROOTFS_FIT_COMPRESSION="gzip"
    ROOTFS_DESCRIPTION="DCENTos ext2 rootfs"
else
    # Also check for uncompressed rootfs that post-image-ramdisk.sh may have consumed
    if [ -f "$BUILDROOT_OUTPUT/rootfs.ext2" ]; then
        warn "Found rootfs.ext2 but not rootfs.ext2.gz — compressing..."
        gzip -9 -k "$BUILDROOT_OUTPUT/rootfs.ext2"
        ROOTFS_INPUT="$ROOTFS_EXT2_GZ"
        ROOTFS_STAGE_NAME="rootfs.ext2.gz"
        ROOTFS_BOOTARGS_MODE="rw rootfstype=ext2"
        ROOTFS_FIT_COMPRESSION="gzip"
        ROOTFS_DESCRIPTION="DCENTos ext2 rootfs"
    elif [ -f "$ROOTFS_SQUASHFS" ]; then
        ROOTFS_INPUT="$ROOTFS_SQUASHFS"
        ROOTFS_STAGE_NAME="rootfs.squashfs"
        ROOTFS_BOOTARGS_MODE="ro rootfstype=squashfs"
        ROOTFS_FIT_COMPRESSION="none"
        ROOTFS_DESCRIPTION="DCENTos squashfs rootfs"
    elif [ -f "$ROOTFS_FALLBACK_SQUASHFS" ] && [ "${ALLOW_STALE_ROOTFS:-0}" = "1" ]; then
        warn "============================================================"
        warn "Using FALLBACK rootfs (ALLOW_STALE_ROOTFS=1): $ROOTFS_FALLBACK_SQUASHFS"
        warn "DANGER: this in-tree base squashfs is the Mar-2026 v0.8.0 build whose"
        warn "dcentrald has NO management-only gate AND whose baked /etc/dcentrald.toml"
        warn "carries a REAL pool (solo.ckpool) + fan_max_pwm=127 (pass-3 NEW-1)."
        warn "A FRESH SD boot will AUTO-MINE + FAN-BLAST. Lab/diagnostic use ONLY."
        warn "============================================================"
        ROOTFS_INPUT="$ROOTFS_FALLBACK_SQUASHFS"
        ROOTFS_STAGE_NAME="rootfs.squashfs"
        ROOTFS_BOOTARGS_MODE="ro rootfstype=squashfs"
        ROOTFS_FIT_COMPRESSION="none"
        ROOTFS_DESCRIPTION="DCENTos squashfs rootfs (STALE/lab-only)"
    elif [ -f "$ROOTFS_FALLBACK_SQUASHFS" ]; then
        error "REFUSING to ship the stale fallback rootfs $ROOTFS_FALLBACK_SQUASHFS (pass-3 NEW-1 safety gate).\n  It is the Mar-2026 v0.8.0 base: its dcentrald has NO management-only gate and its baked\n  /etc/dcentrald.toml has a REAL pool (solo.ckpool) + fan_max_pwm=127 -> a fresh SD boot would\n  AUTO-MINE + FAN-BLAST on an unconfigured unit (forbidden by the PWM-30 home rule + config.rs:1430).\n  Build a FRESH rootfs (Buildroot make) so the card carries the current gated daemon + a safe config\n  (url=\"\" / [mining] enabled=false / fan_max_pwm=30), then re-run.\n  For a deliberate lab/diagnostic card ONLY, re-run with ALLOW_STALE_ROOTFS=1."
    else
        error "No usable rootfs input found. Checked:\n  - $ROOTFS_EXT2_GZ\n  - $BUILDROOT_OUTPUT/rootfs.ext2\n  - $ROOTFS_SQUASHFS\n  - $ROOTFS_FALLBACK_SQUASHFS\n\nBuild the firmware first or provide an existing squashfs artifact."
    fi
fi

info "All prerequisites satisfied."
info "Resolved rootfs input: $ROOTFS_INPUT"
info "  Stage name: $ROOTFS_STAGE_NAME"
info "  Boot mode:  $ROOTFS_BOOTARGS_MODE"

# =============================================================================
# Phase 2: Extract Kernel from Recovery FIT Image
# =============================================================================
#
# The recovery FIT (mtd6_recovery.bin, ~22 MB) is a U-Boot FIT image containing:
#   Position 0: kernel@1  — Linux 4.4.92 ARM zImage (~2.7 MB)
#   Position 1: ramdisk@1 — BraiinsOS SquashFS rootfs (~3.6 MB) [we REPLACE this]
#   Position 2: fdt@1     — Antminer S9 Device Tree (~12 KB)
#
# We extract only the kernel. The DTB comes from extractions/s9/s9_devicetree.dtb
# (extracted live from /sys/firmware/fdt, which includes U-Boot "chosen" node).
# The ramdisk is our Buildroot rootfs.
#

header "Phase 2: Extract Kernel"

if [ -f "$EXTRACTED_KERNEL" ]; then
    info "Kernel already extracted (cached): $EXTRACTED_KERNEL"
    KERNEL_SIZE=$(stat -c%s "$EXTRACTED_KERNEL" 2>/dev/null || stat -f%z "$EXTRACTED_KERNEL")
    info "  Size: $KERNEL_SIZE bytes ($((KERNEL_SIZE / 1024)) KB)"
else
    info "Extracting kernel from recovery FIT image..."
    info "  Source: $RECOVERY_FIT"
    info "  Using dumpimage to extract position 0 (kernel@1)"

    if dumpimage -T flat_dt -p 0 -o "$EXTRACTED_KERNEL" "$RECOVERY_FIT"; then
        info "  Kernel extracted via dumpimage"
    else
        warn "  dumpimage extraction failed — using documented dd fallback"
        dd if="$RECOVERY_FIT" of="$EXTRACTED_KERNEL" \
            bs=64K iflag=skip_bytes,count_bytes \
            skip="$FIT_KERNEL_DATA_OFFSET" count="$FIT_KERNEL_DATA_SIZE" 2>/dev/null
    fi

    KERNEL_SIZE=$(stat -c%s "$EXTRACTED_KERNEL" 2>/dev/null || stat -f%z "$EXTRACTED_KERNEL")
    info "  Extracted: $KERNEL_SIZE bytes ($((KERNEL_SIZE / 1024)) KB)"
fi

# Verify ARM zImage magic at offset +0x24
# The ARM zImage format has a magic number 0x016F2818 at byte offset 36
# which identifies it as a valid ARM Linux boot executable.
info "Verifying ARM zImage magic..."
# Use `od` (coreutils, always present) not `xxd` (vim-common, absent on minimal/CI
# build hosts). With `set -euo pipefail` an undeclared `xxd` here aborts the WHOLE
# build at exit 127 right after this line — silent "no SD image" failure.
MAGIC=$(od -An -tx1 -j "$ZIMAGE_MAGIC_OFFSET" -N 4 "$EXTRACTED_KERNEL" 2>/dev/null | tr -d ' \n')
if [ "$MAGIC" = "$ZIMAGE_MAGIC_RAW" ]; then
    info "  ARM zImage magic verified: raw bytes 0x$MAGIC (value 0x$ZIMAGE_MAGIC_VALUE) at offset +0x$( printf '%X' $ZIMAGE_MAGIC_OFFSET )"
else
    warn "  Expected ARM zImage magic raw bytes 0x$ZIMAGE_MAGIC_RAW (value 0x$ZIMAGE_MAGIC_VALUE) at offset +0x$( printf '%X' $ZIMAGE_MAGIC_OFFSET ), found: 0x$MAGIC"
    warn "  The kernel may not be a valid ARM zImage. Continuing anyway..."
fi

# Also try to extract kernel description from FIT metadata
info "Checking FIT image metadata..."
FIT_INFO=$(mkimage -l "$RECOVERY_FIT" 2>/dev/null | head -20) || true
if [ -n "$FIT_INFO" ]; then
    echo "$FIT_INFO" | while IFS= read -r line; do
        echo "    $line"
    done
fi

# =============================================================================
# Phase 3: Build DCENTos FIT Image
# =============================================================================
#
# The FIT (Flattened Image Tree) bundles kernel + ramdisk + DTB into one file.
# U-Boot's 'bootm' command parses the FIT and loads each component to the
# correct memory address.
#
# Our FIT contains:
#   kernel@1  — BraiinsOS Linux 4.4.92 ARM zImage (from recovery FIT)
#   ramdisk@1 — DCENTos ext2 rootfs, gzip compressed (from Buildroot)
#   fdt@1     — Antminer S9 device tree (from live system)
#
# The .its (Image Tree Source) is a DTS-like text file that mkimage compiles
# into the .itb (Image Tree Blob) binary.
#

header "Phase 3: Build FIT Image"

mkdir -p "$SD_OUTPUT_DIR"

# CRITICAL (adversarial-pass fix 2026-06-10): the shipped sd_recovery_BOOT.BIN is
# U-Boot 2014.01 (Bitmain lineage) — it has NO `unzip` command and boots LEGACY
# images (uImage + uramdisk + separate DTB via `bootm K R D`), NOT a FIT with
# `unzip`+`fpga loadb`. The DISTRIBUTED 2026-04-15  card uses exactly
# this legacy flow. We
# reproduce that proven, env-coherent flow. A FIT/`unzip` uEnv aborts at the first
# `unzip` -> "Unknown command" -> U-Boot prompt -> card does nothing.

# Kernel as a legacy uImage (load/entry 0x8000)
info "Wrapping kernel as legacy uImage..."
mkimage -A arm -O linux -T kernel -C none -a 0x00008000 -e 0x00008000 \
    -n "DCENT_OS Linux 4.4.92" -d "$EXTRACTED_KERNEL" "$SD_OUTPUT_DIR/uImage" >/dev/null
info "  uImage: $(stat -c%s "$SD_OUTPUT_DIR/uImage") bytes"

# Rootfs as a legacy U-Boot ramdisk (uramdisk.image.gz). bootm decompresses this
# gzip ramdisk ONCE into /dev/ram0; the kernel then mounts it ($ROOTFS_BOOTARGS_MODE).
# CRITICAL (pass-2 regression fix 2026-06-10, P2-1): the ramdisk payload must be a
# SINGLE gzip layer. The rootfs resolver can hand us an ALREADY-gzipped input
# (`rootfs.ext2.gz`, the defconfig default) OR a raw squashfs. Gzipping the former
# again → `gzip(gzip(ext2))` → bootm's one `-C gzip` decompress leaves still-gzipped
# bytes in /dev/ram0 → mount panic. So gzip ONLY when the input isn't already gzip.
info "Wrapping rootfs as legacy ramdisk (uramdisk.image.gz)..."
INPUT_MAGIC=$(od -An -tx1 -N 2 "$ROOTFS_INPUT" 2>/dev/null | tr -d ' \n')
if [ "$INPUT_MAGIC" = "1f8b" ]; then
    info "  rootfs input is already gzip — using directly (no double-gzip)"
    cp "$ROOTFS_INPUT" "$SD_OUTPUT_DIR/rootfs.payload.gz"
else
    info "  rootfs input is uncompressed — gzipping once for the -C gzip ramdisk"
    gzip -n -c "$ROOTFS_INPUT" > "$SD_OUTPUT_DIR/rootfs.payload.gz"
fi
mkimage -A arm -O linux -T ramdisk -C gzip -a 0 -e 0 \
    -n "DCENT_OS rootfs" -d "$SD_OUTPUT_DIR/rootfs.payload.gz" "$SD_OUTPUT_DIR/uramdisk.image.gz" >/dev/null
rm -f "$SD_OUTPUT_DIR/rootfs.payload.gz"
info "  uramdisk.image.gz: $(stat -c%s "$SD_OUTPUT_DIR/uramdisk.image.gz") bytes ($ROOTFS_DESCRIPTION, single gzip layer)"

# Device tree as a standalone file (3rd bootm arg). `install` (not `cp`) so a
# read-only source DTB + a read-only stale dest from a prior run don't EACCES.
install -m 0644 "$DEVICE_TREE" "$SD_OUTPUT_DIR/devicetree.dtb"
info "  devicetree.dtb: $(stat -c%s "$SD_OUTPUT_DIR/devicetree.dtb") bytes"

# =============================================================================
# Phase 4: Copy FPGA Bitstream
# =============================================================================
#
# The FPGA bitstream (system.bit.gz) programs the Zynq PL (Programmable Logic)
# with the design that creates:
#   - UART FIFOs for hash board communication (3 chains on S9)
#   - Hardware CRC5/CRC16 engines
#   - Work dispatch and nonce reception FIFOs
#   - I2C controller for PIC voltage and temperature sensors
#   - 14 UIO devices that userspace can mmap()
#
# The bitstream was extracted from NAND mtd2 on the BraiinsOS S9. The raw NAND
# dump contains a valid gzip member followed by NAND padding. The proven Braiins
# SD image trims that padding, so we re-emit a clean gzip payload here instead
# of copying the padded dump directly.
#
# U-Boot sdboot sequence:
#   load mmc 0 0x2000000 system.bit.gz     # load to RAM
#   unzip 0x2000000 0x2100000              # decompress to 1MB buffer
#   fpga loadb 0 0x2100000 0x200000        # program FPGA (1MB bitstream)
#

header "Phase 4: FPGA Bitstream"

# The proven legacy flow uses RAW system.bit + `fpga load` (NOT system.bit.gz +
# `unzip` + `fpga loadb` — that needs the `unzip` command absent from the 2014.01
# BOOT.BIN). Decompress the NAND-extracted gzip member to a clean raw .bit.
info "Decompressing FPGA bitstream to raw system.bit..."
# The NAND-extracted member has trailing pad after the valid gzip stream, so
# `gzip -cd` decompresses the real bitstream then exits non-zero ("trailing
# garbage"). That is expected -> `|| true`; the size check below catches a real
# failure (empty output).
gzip -cd "$FPGA_BITSTREAM" 2>/dev/null > "$SD_OUTPUT_DIR/system.bit" || true

if [ ! -s "$SD_OUTPUT_DIR/system.bit" ]; then
    error "Failed to decompress FPGA bitstream from $FPGA_BITSTREAM"
fi

FPGA_SIZE=$(stat -c%s "$SD_OUTPUT_DIR/system.bit" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/system.bit")
info "  system.bit: $((FPGA_SIZE / 1024)) KB ($FPGA_SIZE bytes, raw — for 'fpga load')"
if [ "$FPGA_SIZE" -lt 1000000 ]; then
    warn "  system.bit is only $FPGA_SIZE bytes — expected ~2 MB for the S9 Zynq-7010 bitstream."
fi

# =============================================================================
# Phase 5: Create uEnv.txt
# =============================================================================
#
# Braiins sdboot sequence (annotated in
#  "The sdboot Command"):
#
#   run uenv_load;                          # import uEnv.txt as env vars
#   test -n ${bootargs} || setenv bootargs  # only set defaults if NOT already set
#     console=ttyPS0,115200 root=/dev/ram0 r rootfstype=squashfs ${recovery_mtdparts} earlyprintk;
#   if test -n ${sd_uenvcmd}; then run sd_uenvcmd; fi;
#   load system.bit.gz -> unzip -> fpga loadb -> load fit.itb -> bootm
#
# CRITICAL (SD-boot-defect fix 2026-06-10): we must ALWAYS emit a bootargs= line,
# including for the default squashfs rootfs. The Braiins sdboot DEFAULT bootargs
# carry NO ramdisk_size= -- they were sized for the ~3.6 MB BraiinsOS recovery
# squashfs, while our rootfs.squashfs is ~17-20 MB. Without ramdisk_size= the
# kernel's /dev/ram0 (CONFIG_BLK_DEV_RAM_SIZE default) can be too small to hold
# the FIT ramdisk and rd_load_image fails -> no root -> boot failure. Setting
# ramdisk_size in bootargs is the BraiinsOS-proven idiom (their own NAND boot
# uses "ramdisk_size=33554432 root=/dev/ram", see
# :35), and the
# DISTRIBUTED 2026-04-15 S9  card's uEnv.txt carried exactly this
# bootargs line.
# This restores byte-parity with that proven artifact. NOTE: setting bootargs in
# uEnv.txt means ${recovery_mtdparts} from the sdboot default is not appended --
# same as the distributed card; it only affects NAND mtd visibility from the
# SD-booted system, never boot itself.
#
# sd_uenvcmd stays non-squashfs-only: for squashfs the Braiins sdboot load
# sequence itself (system.bit.gz + fit.itb) is the proven path; duplicating it
# in sd_uenvcmd adds no value. uenvcmd is the STOCK-Bitmain-U-Boot override
# (stock U-Boot runs uenvcmd, Braiins sdboot runs sd_uenvcmd).
#

header "Phase 5: U-Boot Environment"

# CRITICAL (adversarial-pass fix 2026-06-10): reproduce the PROVEN distributed-beta
# legacy uEnv exactly.
# The shipped 2014.01 BOOT.BIN has NO `unzip` command and boots LEGACY images, so:
#   - RAW system.bit via `fpga load` (size = the real raw .bit, NOT system.bit.gz/unzip/loadb)
#   - legacy `bootm <uImage> <uramdisk.image.gz> <devicetree.dtb>` (separate images, NOT a FIT)
# uenvcmd  -> run by the 2014.01 standalone BOOT.BIN (the default JP4 card).
# sd_uenvcmd -> run by a BraiinsOS-2016.03 NAND `sdboot` (the piggyback `fw_setenv sd_boot yes` path).
# Both carry the SAME legacy flow, which is valid on BOTH U-Boots. The old FIT/`unzip`
# uEnv aborted at `unzip` -> "Unknown command" -> U-Boot prompt -> card did nothing.
info "Creating uEnv.txt (proven legacy bootm K R D flow)..."
ROOTFS_BOOTARGS_LINE="console=ttyPS0,115200 ramdisk_size=67108864 root=/dev/ram0 $ROOTFS_BOOTARGS_MODE earlyprintk"
SYSTEM_BIT_HEX=$(printf '0x%X' "$FPGA_SIZE")
BOOT_SEQ="dcache off && run fpgacmd && fatload mmc 0 0x2000000 uImage && fatload mmc 0 0x3000000 devicetree.dtb && fatload mmc 0 0x4000000 uramdisk.image.gz && bootm 0x2000000 0x4000000 0x3000000"

cat > "$SD_OUTPUT_DIR/uEnv.txt" << EOF
bootargs=$ROOTFS_BOOTARGS_LINE
fpgacmd=fatload mmc 0 0x2000000 system.bit $SYSTEM_BIT_HEX && fpga load 0 0x2000000 $SYSTEM_BIT_HEX
uenvcmd=$BOOT_SEQ
sd_uenvcmd=$BOOT_SEQ
EOF

info "  uEnv.txt created (legacy flow; raw system.bit $SYSTEM_BIT_HEX bytes via 'fpga load', bootm K R D)"
info "  bootargs: ramdisk_size=64MiB + root=/dev/ram0 $ROOTFS_BOOTARGS_MODE (byte-parity with the distributed beta)"
info "  uenvcmd (2014.01 standalone) + sd_uenvcmd (2016.03 piggyback) — both the legacy no-unzip flow"

# =============================================================================
# Phase 6: Build BOOT.BIN (Standalone Mode Only)
# =============================================================================
#
# BOOT.BIN is only needed for standalone SD card boot. The proven
# BOOT.BIN for standalone SD card boot must be the COMPLETE Zynq Boot Image
# containing FSBL + FPGA bitstream + U-Boot (~2.5 MB). A trimmed mtd0_boot.bin
# (~79 KB) contains only the FSBL and CANNOT chain-load U-Boot -- the Zynq
# BootROM loads FSBL, FSBL has no U-Boot partition to load, and the board
# watchdog-resets in a fan-cycling loop with no LEDs.
#
# The proven BraiinsOS SD recovery BOOT.BIN (SHA256: 4004015bba6ff3...) is
# used by both BraiinsOS and VNish for S9 SD card boot. It is the universal
# known-good artifact for Zynq-7010 SD boot.
#
# Boot sequence with this BOOT.BIN:
#   1. Zynq BootROM reads boot mode pins (JP4=LEFT for SD)
#   2. BootROM reads BOOT.BIN from first FAT partition
#   3. FSBL initializes PS, programs PL with embedded bitstream
#   4. FSBL loads U-Boot from the same boot image
#   5. U-Boot runs 'sdboot': loads system.bit.gz (reprograms FPGA), loads fit.itb
#   6. U-Boot boots kernel from FIT with our rootfs
#

header "Phase 6: BOOT.BIN"

if $STANDALONE; then
    info "Copying proven SD recovery BOOT.BIN (FSBL+FPGA+U-Boot)..."
    cp "$RESOLVED_SD_BOOTBIN" "$SD_OUTPUT_DIR/BOOT.BIN"

    BOOTBIN_SIZE=$(stat -c%s "$SD_OUTPUT_DIR/BOOT.BIN" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/BOOT.BIN")
    if [ "$BOOTBIN_SIZE" -lt 500000 ]; then
        error "BOOT.BIN is only $BOOTBIN_SIZE bytes — too small!\n  A proper Zynq SD boot image with FSBL+FPGA+U-Boot should be ~2.5 MB.\n  Source: $RESOLVED_SD_BOOTBIN"
    fi
    info "  BOOT.BIN: $((BOOTBIN_SIZE / 1024)) KB ($BOOTBIN_SIZE bytes)"
    info "  Source: $RESOLVED_SD_BOOTBIN"
else
    info "Skipping BOOT.BIN (explicit piggyback compatibility mode)"
    rm -f "$SD_OUTPUT_DIR/BOOT.BIN"
    info "  To build BOOT.BIN for standalone boot, use: $(basename "$0") --standalone"
fi

# =============================================================================
# Phase 7: Create Disk Image (Optional)
# =============================================================================
#
# When --disk-image is specified, create a raw .img file that can be flashed
# directly to an SD card using dd, balenaEtcher, or Rufus. This avoids the
# manual "format + copy" step on Windows.
#
# Layout:
#   Sector 0:      MBR (partition table)
#   Sector 2048+:  FAT32 partition (1 MB aligned, contains all boot files)
#
# The image is kept minimal -- just large enough for all files plus some
# headroom for FAT32 metadata.
#

header "Phase 7: Disk Image"

if $DISK_IMAGE; then
    info "Creating raw disk image (${DISK_IMAGE_SIZE_MB}MB)..."

    IMG_FILE="$SD_OUTPUT_DIR/dcentos-sd.img"

    # Calculate total size of SD card files
    TOTAL_FILES_SIZE=0
    for f in "$SD_OUTPUT_DIR"/{BOOT.BIN,uEnv.txt,system.bit,uImage,devicetree.dtb,uramdisk.image.gz}; do
        if [ -f "$f" ]; then
            FSIZE=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f")
            TOTAL_FILES_SIZE=$((TOTAL_FILES_SIZE + FSIZE))
        fi
    done
    info "  Total file payload: $((TOTAL_FILES_SIZE / 1024)) KB"

    # Ensure image is big enough (files + FAT32 overhead + alignment)
    NEEDED_MB=$(( (TOTAL_FILES_SIZE / 1024 / 1024) + 8 ))
    if [ "$NEEDED_MB" -gt "$DISK_IMAGE_SIZE_MB" ]; then
        DISK_IMAGE_SIZE_MB="$NEEDED_MB"
        warn "  Increasing image size to ${DISK_IMAGE_SIZE_MB}MB to fit all files"
    fi

    # Create empty image file
    dd if=/dev/zero of="$IMG_FILE" bs=1M count="$DISK_IMAGE_SIZE_MB" 2>/dev/null
    info "  Created ${DISK_IMAGE_SIZE_MB}MB image: $IMG_FILE"

    # Create MBR partition table with a single FAT32 partition
    # Type 0x0B = Win95 FAT32, bootable flag set
    # Start at sector 2048 (1MB aligned, standard for modern SD cards)
    info "  Writing partition table..."
    sfdisk "$IMG_FILE" << 'SFDISK_EOF' 2>/dev/null
label: dos
unit: sectors

start=2048, type=0b, bootable
SFDISK_EOF

    # Set up loop device for the partition
    # We calculate the partition offset manually: sector 2048 * 512 bytes = 1048576
    PART_OFFSET=$((2048 * 512))
    PART_SIZE=$(( (DISK_IMAGE_SIZE_MB * 1024 * 1024) - PART_OFFSET ))

    # Format the FAT32 partition inside the image
    # Using mtools to avoid requiring root/loop devices
    info "  Formatting FAT32 partition..."
    LOOP_DEV=""

    # Prefer the loop-device method ONLY when running as root AND a loop device is
    # actually attachable. Plain Docker/CI containers run as root but have NO loop
    # devices -> `losetup` fails -> with `set -e` the WHOLE build aborts here. Detect
    # that up front and fall back to the portable mtools method (no root, no loops).
    USE_LOOP=false
    if [ "$(id -u)" = "0" ] && losetup -f >/dev/null 2>&1; then
        USE_LOOP=true
    fi

    if $USE_LOOP; then
        info "  Formatting FAT32 partition (loop device method)..."
        if LOOP_DEV=$(losetup --find --show --offset "$PART_OFFSET" --sizelimit "$PART_SIZE" "$IMG_FILE" 2>/dev/null); then
            mkfs.vfat -F 32 -n "DCENTOS" "$LOOP_DEV" >/dev/null 2>&1
            MOUNT_POINT=$(mktemp -d)
            if mount "$LOOP_DEV" "$MOUNT_POINT" 2>/dev/null; then
                for f in BOOT.BIN uEnv.txt system.bit uImage devicetree.dtb uramdisk.image.gz; do
                    [ -f "$SD_OUTPUT_DIR/$f" ] && { cp "$SD_OUTPUT_DIR/$f" "$MOUNT_POINT/"; info "    Copied $f"; }
                done
                umount "$MOUNT_POINT"
                info "  Disk image created successfully (loop device method)"
            else
                warn "  mount failed; falling back to mtools"; USE_LOOP=false
            fi
            rmdir "$MOUNT_POINT" 2>/dev/null || true
            losetup -d "$LOOP_DEV" 2>/dev/null || true
        else
            warn "  losetup failed; falling back to mtools"; USE_LOOP=false
        fi
    fi

    if ! $USE_LOOP; then
        # Portable fallback: mtools (works as root or non-root, no loop devices --
        # the only method that works in Docker/CI). MTOOLS_SKIP_CHECK=1 bypasses
        # geometry warnings.
        info "  Formatting FAT32 partition (mtools method)..."
        PART_TMP=$(mktemp)
        # bs=1M, NOT bs=1: the partition offset (1 MiB) and size are whole-MB aligned,
        # so block copy is exact. bs=1 copies ~63 MB byte-by-byte and takes minutes.
        dd if="$IMG_FILE" of="$PART_TMP" bs=1M skip=$((PART_OFFSET / 1048576)) count=$((PART_SIZE / 1048576)) 2>/dev/null
        mkfs.vfat -F 32 -n "DCENTOS" "$PART_TMP" >/dev/null 2>&1

        export MTOOLS_SKIP_CHECK=1
        for f in BOOT.BIN uEnv.txt system.bit uImage devicetree.dtb uramdisk.image.gz; do
            [ -f "$SD_OUTPUT_DIR/$f" ] && { mcopy -i "$PART_TMP" "$SD_OUTPUT_DIR/$f" "::"; info "    Copied $f"; }
        done

        dd if="$PART_TMP" of="$IMG_FILE" bs=1M seek=$((PART_OFFSET / 1048576)) conv=notrunc 2>/dev/null
        rm -f "$PART_TMP"
        info "  Disk image created successfully (mtools method)"
    fi

    IMG_SIZE=$(stat -c%s "$IMG_FILE" 2>/dev/null || stat -f%z "$IMG_FILE")
    info "  dcentos-sd.img: $((IMG_SIZE / 1024 / 1024)) MB ($IMG_SIZE bytes)"
    info "  Flash with: dd if=dcentos-sd.img of=/dev/sdX bs=4M status=progress"
    info "  Or use balenaEtcher / Rufus on Windows"
else
    info "Skipping disk image creation (use --disk-image to enable)"
    info "  Copy files manually to a FAT32-formatted SD card instead"
fi

# =============================================================================
# Phase 8: Generate Build Info and Summary
# =============================================================================

header "Build Summary"

# Generate machine-readable build info
BUILD_DATE=$(date -u +"%Y-%m-%d %H:%M:%S UTC")
ROOTFS_SHA256=$(sha256sum "$ROOTFS_INPUT" | awk '{print $1}')
URAMDISK_SHA256=$(sha256sum "$SD_OUTPUT_DIR/uramdisk.image.gz" | awk '{print $1}')

cat > "$SD_OUTPUT_DIR/BUILD_INFO.txt" << EOF
=== DCENT_OS S9 SD Card Boot ===
Build date:       $BUILD_DATE
Build mode:       $(if $STANDALONE; then echo 'Standalone SD boot (default)'; else echo 'BraiinsOS compatibility piggyback'; fi)
Disk image:       $(if $DISK_IMAGE; then echo 'Yes'; else echo 'No'; fi)

=== Component Sizes ===
EOF

for f in BOOT.BIN uEnv.txt system.bit uImage devicetree.dtb uramdisk.image.gz dcentos-sd.img; do
    if [ -f "$SD_OUTPUT_DIR/$f" ]; then
        FSIZE=$(stat -c%s "$SD_OUTPUT_DIR/$f" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/$f")
        printf "%-22s %s bytes (%s KB)\n" "$f:" "$FSIZE" "$((FSIZE / 1024))" >> "$SD_OUTPUT_DIR/BUILD_INFO.txt"
    fi
done

cat >> "$SD_OUTPUT_DIR/BUILD_INFO.txt" << EOF

=== Checksums ===
$ROOTFS_STAGE_NAME SHA256: $ROOTFS_SHA256
uramdisk.image.gz SHA256: $URAMDISK_SHA256

=== Boot Chain ===
BOOT.BIN:   Proven BraiinsOS SD recovery image (FSBL+FPGA+U-Boot, ~2.5 MB)
FPGA:       BraiinsOS bitstream v0x00901002 (from mtd2_fpga1.bin)
Kernel:     Linux 4.4.92 OpenWrt/BraiinsOS (from mtd6_recovery.bin)
DTB:        Antminer S9 (from /sys/firmware/fdt)
Rootfs:     $ROOTFS_DESCRIPTION

=== SD Card Instructions ===
$(if $STANDALONE; then
    echo "MODE: Standalone SD boot (default)"
    echo "  1. Format SD card as FAT32 (or use dcentos-sd.img with dd/balenaEtcher)"
    echo "  2. Copy BOOT.BIN, uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz to SD root"
    echo "  3. Direct SD path: power off S9, move JP4 LEFT, insert SD, power on"
    echo "  4. Piggyback (BraiinsOS-NAND S9 only): fw_setenv sd_boot yes ; insert SD ; reboot"
    echo "     (the old 'keep JP4 in NAND + insert SD' fallback was removed -- 2014.01 nandboot never reads SD)"
    echo "  5. Wait ~60-90 seconds"
    echo "  6. Keep the SD card inserted while using DCENT_OS"
    echo "  7. SSH: root@<miner_ip> (password: dcentral)"
    echo ""
    echo "This image is SD-boot only; removing the SD card stops DCENT_OS from booting."
    echo "To revert: power off, remove SD, restore JP4 to RIGHT (NAND boot)"
else
    echo "MODE: BraiinsOS compatibility piggyback"
    echo "  1. Format SD card as FAT32 (or use dcentos-sd.img with dd/balenaEtcher)"
    echo "  2. Copy uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz to SD root"
    echo "  3. Enable SD boot: ssh root@<miner_ip> 'fw_setenv sd_boot yes'"
    echo "  4. Insert SD card into control board slot"
    echo "  5. Reboot: ssh root@<miner_ip> 'reboot'"
    echo "  6. Wait ~60 seconds"
    echo "  7. Keep the SD card inserted while using DCENT_OS"
    echo "  8. SSH: root@<miner_ip> (password: dcentral)"
    echo ""
    echo "This path is still SD-boot only; removing the SD card stops DCENT_OS from booting."
    echo "To revert: ssh root@<ip> 'fw_setenv sd_boot' OR remove SD and reboot"
fi)
EOF

# Print summary to terminal
echo ""
echo -e "${BOLD}Output directory:${NC} $SD_OUTPUT_DIR/"
echo ""
echo -e "${BOLD}Files for SD card:${NC}"

TOTAL_SIZE=0
for f in BOOT.BIN uEnv.txt system.bit uImage devicetree.dtb uramdisk.image.gz; do
    if [ -f "$SD_OUTPUT_DIR/$f" ]; then
        FSIZE=$(stat -c%s "$SD_OUTPUT_DIR/$f" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/$f")
        TOTAL_SIZE=$((TOTAL_SIZE + FSIZE))
        printf "  ${GREEN}%-22s${NC} %'d bytes (%s KB)\n" "$f" "$FSIZE" "$((FSIZE / 1024))"
    fi
done
echo "  ──────────────────────────────────"
printf "  ${BOLD}%-22s${NC} %'d bytes (%s MB)\n" "TOTAL" "$TOTAL_SIZE" "$((TOTAL_SIZE / 1024 / 1024))"

if [ -f "$SD_OUTPUT_DIR/dcentos-sd.img" ]; then
    IMG_SIZE=$(stat -c%s "$SD_OUTPUT_DIR/dcentos-sd.img" 2>/dev/null || stat -f%z "$SD_OUTPUT_DIR/dcentos-sd.img")
    echo ""
    printf "  ${GREEN}%-22s${NC} %'d bytes (%s MB)\n" "dcentos-sd.img" "$IMG_SIZE" "$((IMG_SIZE / 1024 / 1024))"
fi

# F-6 (Sweep-v3 PR-083): Ed25519-sign the raw S9 SD .img so the *proven*
# S9 card is not the only unsigned DCENT_OS artifact. The sweep doc said
# "wire S9 into build_in_docker.sh Phase-8c" — wrong target: the S9
# Docker arm is BOARD_POST_IMAGE="internal" (tarball, no .img); the
# proven S9 card comes from THIS standalone builder's --disk-image mode,
# which never signed. Mirrors the Phase-8c contract exactly: reuse
# scripts/sign_sd_image.sh (the same Ed25519 release key as
# sysupgrade/OTA — NOT a separate SD key); lab/unsigned builds still
# succeed (sign_sd_image.sh self-WARNs + exits 0 with no key; an
# explicit DCENT_ALLOW_UNSIGNED_SYSUPGRADE skips with a note).
if [ -f "$SD_OUTPUT_DIR/dcentos-sd.img" ]; then
    SIGN_SD="$SCRIPT_DIR/sign_sd_image.sh"
    if [ ! -f "$SIGN_SD" ]; then
        warn "sign_sd_image.sh not found at $SIGN_SD; SD .img left unsigned"
    elif [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
        info "Signing dcentos-sd.img (Ed25519, release key)"
        bash "$SIGN_SD" "$SD_OUTPUT_DIR/dcentos-sd.img" \
            || warn "SD .img signing failed (see sign_sd_image.sh output)"
    elif [ "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}" = "1" ] \
        || [ "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-}" = "true" ]; then
        info "Skipping SD .img signing (DCENT_ALLOW_UNSIGNED_SYSUPGRADE — lab build)"
    else
        # sign_sd_image.sh self-emits a WARN + exit 0 when no key is set.
        bash "$SIGN_SD" "$SD_OUTPUT_DIR/dcentos-sd.img" || true
    fi
fi

echo ""
if $STANDALONE; then
    echo -e "${BOLD}Boot mode: Standalone SD boot (default)${NC}"
    echo "  1. Copy ALL files above to a FAT32-formatted SD card"
    echo "     Or flash dcentos-sd.img directly with dd/balenaEtcher"
    echo "  2. Flash ONLY with raw/DD mode (Rufus DD / Etcher / dd) -- a Windows"
    echo "     'format this disk?' click destroys the squashfs partition."
    echo "  3. Direct SD path: power off the S9, move JP4 LEFT, insert SD, power on"
    echo "  4. Piggyback (BraiinsOS-NAND S9 only): fw_setenv sd_boot yes ; insert SD ; reboot"
    echo "     (the old 'keep JP4 in NAND + insert SD' fallback does NOT work)"
    echo "  5. Wait ~60-90 seconds for boot"
    echo "  5. Keep the SD card inserted while using DCENT_OS"
    echo "     Removing the SD card stops DCENT_OS from booting"
else
    echo -e "${BOLD}Boot mode: BraiinsOS compatibility piggyback${NC}"
    echo "  1. Copy uEnv.txt, system.bit, uImage, devicetree.dtb, uramdisk.image.gz to a FAT32-formatted SD card"
    echo "     Or flash dcentos-sd.img directly with dd/balenaEtcher"
    echo "  2. Enable SD boot on the miner (requires BraiinsOS):"
    echo "       ssh root@<miner_ip> 'fw_setenv sd_boot yes'"
    echo "  3. Insert SD card into S9 control board"
    echo "  4. Reboot the miner"
    echo "  5. Wait ~60 seconds for boot"
    echo "  6. Keep the SD card inserted while using DCENT_OS"
    echo "     Removing the SD card stops DCENT_OS from booting"
fi
echo ""
echo -e "  SSH:  ${GREEN}ssh root@<miner_ip>${NC}  (password: ${BOLD}dcentral${NC})"
echo ""
echo -e "  Write to SD card: ${CYAN}firmware/scripts/write_sd_card.sh${NC}"
echo ""
