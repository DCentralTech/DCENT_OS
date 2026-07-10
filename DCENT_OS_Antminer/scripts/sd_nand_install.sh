#!/bin/sh
#
# sd_nand_install.sh — Install DCENTos from SD card boot to NAND (permanent)
# D-Central Technologies, 2026
#
# This script runs ON THE MINER while booted from SD card. It permanently
# installs DCENTos to NAND flash so the miner boots DCENTos without SD card.
#
# The safest installation path:
#   1. Boot DCENTos from SD card (non-destructive, lower-risk than NAND writes)
#   2. Verify everything works (SSH, fans, temps, hash boards)
#   3. Run this script to make it permanent
#   4. Remove SD card, reboot from NAND
#
# Supports three NAND scenarios:
#   A) BraiinsOS on NAND (UBI volumes) — PREFERRED
#      → Replace ubi0_1 (rootfs) with DCENTos squashfs
#      → Preserves BraiinsOS kernel, FPGA, boot chain
#      → Most reliable, fully reversible
#
#   B) Stock Bitmain on NAND (raw NAND, no UBI)
#      → Create UBI on firmware1 partition (mtd7)
#      → Flash kernel + rootfs + create overlay
#      → Requires BraiinsOS boot components on SD card
#
#   C) VNish/Other on NAND
#      → Detect layout, flash appropriately
#      → May need full boot chain replacement
#
# Usage (run ON the miner via SSH):
#   ./sd_nand_install.sh                    # Interactive mode
#   ./sd_nand_install.sh --yes              # Skip confirmation
#   ./sd_nand_install.sh --slot 1           # Force firmware slot 1 (mtd7)
#   ./sd_nand_install.sh --slot 2           # Force firmware slot 2 (mtd8)
#   ./sd_nand_install.sh --preserve-env     # Keep existing U-Boot env
#   ./sd_nand_install.sh --dry-run          # Show what would happen
#
# Prerequisites:
#   - Running DCENTos from SD card (booted via build_sd_image.sh output)
#   - NAND flash accessible (/dev/mtd* devices present)
#   - Required tools: flash_erase, nandwrite, ubiformat, ubiattach (built into DCENTos)
#

# BusyBox-compatible shell (no bash features)
VERSION="0.1"

# =============================================================================
# Configuration
# =============================================================================

AUTO_YES=false
DRY_RUN=false
FORCE_SLOT=""
PRESERVE_ENV=false
ALLOW_UNSAFE=false

# NAND partition map (S9 standard — verified from live probe)
# These are defaults; the script auto-detects actual layout.
MTD_BOOT="mtd0"         # FSBL (512K)
MTD_UBOOT="mtd1"        # U-Boot (2.5M)
MTD_FPGA1="mtd2"        # FPGA primary (2M)
MTD_FPGA2="mtd3"        # FPGA backup (2M)
MTD_UBOOT_ENV="mtd4"    # U-Boot env (512K)
MTD_MINER_CFG="mtd5"    # Miner config (512K)
MTD_RECOVERY="mtd6"     # Recovery (22M)
MTD_FW1="mtd7"          # Firmware slot 1 (95M)
MTD_FW2="mtd8"          # Firmware slot 2 (95M)
MTD_FACTORY="mtd9"      # Factory data (36M)

# UBI volume sizes
KERNEL_VOL_SIZE="4MiB"   # kernel volume (2.8M actual, 4M with headroom)
ROOTFS_VOL_SIZE="20MiB"  # rootfs volume (need to fit squashfs)

# Colors (BusyBox echo -e compatible)
RED='\033[1;31m'
GRN='\033[1;32m'
YEL='\033[1;33m'
CYN='\033[1;36m'
BLD='\033[1m'
NC='\033[0m'

pass() { printf "${GRN}[OK]${NC}   %s\n" "$1"; }
fail() { printf "${RED}[FAIL]${NC} %s\n" "$1"; }
info() { printf "${CYN}[INFO]${NC} %s\n" "$1"; }
warn() { printf "${YEL}[WARN]${NC} %s\n" "$1"; }

die() {
    printf "${RED}[ERROR]${NC} %s\n" "$1"
    exit 1
}

run() {
    if $DRY_RUN; then
        printf "${YEL}[DRY]${NC}  %s\n" "$*"
    else
        "$@"
    fi
}

# =============================================================================
# Parse Arguments
# =============================================================================

while [ $# -gt 0 ]; do
    case "$1" in
        --yes|-y)          AUTO_YES=true; shift ;;
        --dry-run)         DRY_RUN=true; shift ;;
        --slot)            FORCE_SLOT="$2"; shift 2 ;;
        --preserve-env)    PRESERVE_ENV=true; shift ;;
        --force-unsafe)    ALLOW_UNSAFE=true; shift ;;
        --help|-h)
            echo "Usage: $(basename $0) [OPTIONS]"
            echo ""
            echo "Install DCENTos from SD card to NAND (permanent installation)."
            echo "Run this ON the miner while booted from SD card."
            echo ""
            echo "Options:"
            echo "  --yes, -y         Skip confirmation prompts"
            echo "  --dry-run         Show what would happen without writing"
            echo "  --slot <1|2>      Force firmware slot (default: auto-detect inactive)"
            echo "  --preserve-env    Don't modify U-Boot environment"
            echo "  --force-unsafe    Allow emergency raw NAND path on validated S9/AM1 only"
            echo "  --help, -h        Show this help"
            exit 0 ;;
        *) die "Unknown option: $1" ;;
    esac
done

# =============================================================================
# Preflight Checks
# =============================================================================

echo "============================================="
echo "  DCENTos SD → NAND Installer v${VERSION}"
echo "  D-Central Technologies"
echo "  $(date)"
echo "============================================="
echo ""

# Must be root
if [ "$(id -u)" != "0" ]; then
    die "Must run as root"
fi

# Verify we're booted from SD card (not NAND)
BOOT_SOURCE=""
if grep -q "root=/dev/ram0" /proc/cmdline 2>/dev/null; then
    # RAM-based root = FIT image from SD
    BOOT_SOURCE="sd_fit"
    pass "Booted from SD card (FIT ramdisk)"
elif grep -q "/dev/mmcblk" /proc/cmdline 2>/dev/null; then
    BOOT_SOURCE="sd_block"
    pass "Booted from SD card (block device)"
elif grep -q "ubi" /proc/cmdline 2>/dev/null; then
    die "Appears to be booted from NAND (UBI root). Refusing to replace the active rootfs; boot from SD first."
else
    warn "Cannot determine boot source from cmdline"
    BOOT_SOURCE="unknown"
fi

# Verify DCENTos is running
if [ -f /etc/dcentos-version ]; then
    DCENTOS_VER=$(cat /etc/dcentos-version)
    pass "DCENTos $DCENTOS_VER running"
else
    warn "This doesn't appear to be DCENTos (no /etc/dcentos-version)"
fi

# Restrict this installer to validated S9/AM1 boards only.
ARCH=$(uname -m 2>/dev/null || echo unknown)
MODEL=$(cat /config/CONF_MINER_TYPE 2>/dev/null || echo "")
HWID=$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo "")
UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name 'uio*' 2>/dev/null | wc -l)
case "$ARCH:$MODEL:$HWID:$UIO_COUNT" in
    aarch64:*|*:s17*:*|*:s19*:*|*:s21*:*|*:*:am2*:* )
        die "sd_nand_install.sh is validated only for S9/AM1. Refusing this board family."
        ;;
esac
if [ "$UIO_COUNT" -gt 16 ] 2>/dev/null; then
    die "sd_nand_install.sh is validated only for S9/AM1. Refusing this board family."
fi

# Check required tools
MISSING=""
for tool in flash_erase nandwrite nanddump; do
    if ! command -v $tool >/dev/null 2>&1; then
        MISSING="$MISSING $tool"
    fi
done
if [ -n "$MISSING" ]; then
    die "Missing required NAND tools:$MISSING"
fi

# Check for UBI tools
HAS_UBI_TOOLS=false
if command -v ubiformat >/dev/null 2>&1 && command -v ubimkvol >/dev/null 2>&1; then
    HAS_UBI_TOOLS=true
    pass "UBI tools available"
else
    warn "UBI tools not available — raw NAND only"
fi

echo ""

# =============================================================================
# Detect NAND Layout
# =============================================================================

info "Detecting NAND layout..."

# Read MTD partition table
if [ ! -f /proc/mtd ]; then
    die "No /proc/mtd — NAND not accessible"
fi

MTD_COUNT=$(grep -c "^mtd" /proc/mtd)
info "Found $MTD_COUNT MTD partitions:"
echo ""
while read line; do
    case "$line" in
        mtd*) printf "  %s\n" "$line" ;;
    esac
done < /proc/mtd
echo ""

# Detect if UBI is already on any partition
UBI_ATTACHED=false
EXISTING_UBI_MTD=""
if [ -d /sys/class/ubi/ubi0 ]; then
    UBI_ATTACHED=true
    EXISTING_UBI_MTD=$(cat /sys/class/ubi/ubi0/mtd_num 2>/dev/null)
    UBI_VOLS=$(cat /sys/class/ubi/ubi0/volumes_count 2>/dev/null || echo 0)
    pass "UBI already attached on mtd$EXISTING_UBI_MTD ($UBI_VOLS volumes)"
else
    info "No UBI attached — will check for UBI on firmware partitions"

    # Try to attach UBI on mtd7 or mtd8
    if $HAS_UBI_TOOLS; then
        for mtd_num in 7 8; do
            if [ -c "/dev/mtd$mtd_num" ]; then
                if ubiattach -m $mtd_num -d 0 2>/dev/null; then
                    UBI_ATTACHED=true
                    EXISTING_UBI_MTD=$mtd_num
                    UBI_VOLS=$(cat /sys/class/ubi/ubi0/volumes_count 2>/dev/null || echo 0)
                    pass "UBI found on mtd$mtd_num ($UBI_VOLS volumes)"
                    break
                fi
            fi
        done
    fi
fi

# Determine installation target
TARGET_MTD=""
INSTALL_MODE=""

if $UBI_ATTACHED && [ "${UBI_VOLS:-0}" -ge 3 ]; then
    # BraiinsOS-style UBI: kernel + rootfs + rootfs_data
    INSTALL_MODE="ubi_replace"
    TARGET_MTD="$EXISTING_UBI_MTD"
    info "Mode: UBI rootfs replacement (BraiinsOS-compatible)"

elif $UBI_ATTACHED && [ "${UBI_VOLS:-0}" -ge 1 ]; then
    # Partial UBI — might be recoverable
    INSTALL_MODE="ubi_replace"
    TARGET_MTD="$EXISTING_UBI_MTD"
    warn "UBI has only $UBI_VOLS volume(s) — may need volume creation"

elif $HAS_UBI_TOOLS; then
    # No UBI — need to create from scratch
    INSTALL_MODE="ubi_create"

    # Choose which firmware slot to use
    if [ -n "$FORCE_SLOT" ]; then
        case "$FORCE_SLOT" in
            1) TARGET_MTD=7 ;;
            2) TARGET_MTD=8 ;;
            *) die "Invalid slot: $FORCE_SLOT (use 1 or 2)" ;;
        esac
    else
        # Default: use slot 2 (mtd8) to preserve recovery ability
        TARGET_MTD=8
    fi

    info "Mode: Create new UBI on mtd$TARGET_MTD"
    warn "This will ERASE mtd$TARGET_MTD!"

else
    # No UBI tools — raw NAND only
    if ! $ALLOW_UNSAFE; then
        die "Raw NAND install path is disabled by default. Re-run with --force-unsafe only on a validated S9/AM1 recovery bench."
    fi
    INSTALL_MODE="raw_nand"
    TARGET_MTD=8
    info "Mode: Raw NAND write (no UBI)"
    warn "Without UBI, wear-leveling is not available."
fi

echo ""

# =============================================================================
# CE-105: Prove inactive-slot targeting before any A/B firmware-slot write
# =============================================================================
#
# Design Decision #5 +  ("NEVER flash to the
# ACTIVE NAND slot"): the S9/AM1 A/B firmware slots are mtd7 (firmware=1) and
# mtd8 (firmware=2). Overwriting the *active* slot destroys the known-good
# rollback copy, so a bad new image then has no slot to fall back to = a
# wrong-slot brick. This guard proves the write TARGET_MTD is NOT the currently
# active firmware slot BEFORE any /dev/ubi0_* write, and fails closed (mirroring
# the raw_nand fail-closed pattern above) when the active slot is unreadable.
# It reuses the existing --force-unsafe lab flag as the recovery-bench override.
#
# First, harden the warn-only unknown-boot-source case: the SD-boot check near
# the top only die()s on a *confirmed* NAND-UBI root; an unknown boot source
# could still be a NAND boot, so refuse any NAND write by default.
case "$INSTALL_MODE" in
    ubi_replace|ubi_create|raw_nand)
        if [ "$BOOT_SOURCE" = "unknown" ] && ! $ALLOW_UNSAFE; then
            die "Cannot confirm SD boot (boot source unknown); refusing NAND write. Boot from SD first, or re-run --force-unsafe on a validated S9/AM1 recovery bench."
        fi
        ;;
esac

if [ "$INSTALL_MODE" = "ubi_replace" ]; then
    ACTIVE_FW=""
    if command -v fw_printenv >/dev/null 2>&1; then
        ACTIVE_FW=$(fw_printenv -n firmware 2>/dev/null || echo "")
    fi
    ACTIVE_MTD=""
    case "$ACTIVE_FW" in
        1) ACTIVE_MTD=7 ;;
        2) ACTIVE_MTD=8 ;;
    esac
    case "$TARGET_MTD" in
        7|8)
            # TARGET_MTD is an A/B firmware slot — it MUST be the INACTIVE one.
            if [ -z "$ACTIVE_MTD" ]; then
                $ALLOW_UNSAFE || die "ubi_replace: cannot read active firmware slot (fw_printenv) to prove inactive-slot targeting. Refusing (re-run --force-unsafe on a validated S9/AM1 recovery bench)."
                warn "ubi_replace: active firmware slot unreadable — proceeding under --force-unsafe (recovery bench)."
            elif [ "$TARGET_MTD" = "$ACTIVE_MTD" ]; then
                $ALLOW_UNSAFE || die "ubi_replace: target mtd$TARGET_MTD IS the active firmware slot (firmware=$ACTIVE_FW). Refusing to overwrite the active/rollback slot; boot from SD and install to the inactive slot, or re-run --force-unsafe on a recovery bench."
                warn "ubi_replace: target mtd$TARGET_MTD IS the active firmware slot — proceeding under --force-unsafe (recovery bench)."
            else
                pass "ubi_replace: target mtd$TARGET_MTD is the INACTIVE firmware slot (active=mtd$ACTIVE_MTD, firmware=$ACTIVE_FW) — safe to write."
            fi
            ;;
        *)
            # Non-7/8 UBI targets are not A/B firmware slots (the env-commit
            # below leaves FW_SLOT empty for them), so no active/inactive
            # distinction applies — they correctly pass through.
            info "ubi_replace: target mtd$TARGET_MTD is not an A/B firmware slot — inactive-slot proof not applicable."
            ;;
    esac
fi

echo ""

# =============================================================================
# Find Source Files
# =============================================================================

info "Locating DCENTos images..."

# Look for rootfs on the running system (it IS the rootfs if booted from SD)
ROOTFS_SRC=""
KERNEL_SRC=""

# Check common locations
for path in \
    /boot/rootfs.squashfs \
    /mnt/sd/rootfs.squashfs \
    /tmp/rootfs.squashfs \
    /opt/dcentos/rootfs.squashfs; do
    if [ -f "$path" ]; then
        ROOTFS_SRC="$path"
        break
    fi
done

# If no squashfs found, we can create one from the running rootfs
if [ -z "$ROOTFS_SRC" ]; then
    if command -v mksquashfs >/dev/null 2>&1; then
        info "No pre-built rootfs.squashfs found."
        info "Will create squashfs from running rootfs."
        ROOTFS_SRC="/tmp/rootfs.squashfs"
        NEED_MKSQUASHFS=true
    else
        die "No rootfs.squashfs found and mksquashfs not available."
    fi
else
    pass "Rootfs: $ROOTFS_SRC ($(stat -c%s "$ROOTFS_SRC" 2>/dev/null || echo '?') bytes)"
    NEED_MKSQUASHFS=false
fi

# Find kernel
for path in \
    /boot/kernel.bin \
    /mnt/sd/kernel.bin \
    /boot/zImage \
    /tmp/kernel.bin; do
    if [ -f "$path" ]; then
        KERNEL_SRC="$path"
        break
    fi
done

# Kernel may already be in NAND UBI (if BraiinsOS was installed)
if [ -z "$KERNEL_SRC" ] && $UBI_ATTACHED; then
    # Check if ubi0_0 has a kernel
    if [ -c /dev/ubi0_0 ]; then
        info "Will preserve existing kernel in UBI volume 0"
        KERNEL_SRC="UBI_EXISTING"
    fi
fi

if [ -z "$KERNEL_SRC" ]; then
    warn "No kernel found. Installation will only replace rootfs."
    warn "The existing kernel in NAND will be used."
elif [ "$KERNEL_SRC" != "UBI_EXISTING" ]; then
    pass "Kernel: $KERNEL_SRC ($(stat -c%s "$KERNEL_SRC" 2>/dev/null || echo '?') bytes)"
fi

echo ""

# =============================================================================
# Create Squashfs (if needed)
# =============================================================================

if ${NEED_MKSQUASHFS:-false}; then
    info "Creating squashfs from running rootfs..."
    info "This may take a minute on the miner's CPU..."

    # Exclude tmpfs, proc, sys, and the squashfs output itself
    run mksquashfs / "$ROOTFS_SRC" \
        -e proc sys dev tmp run var/run var/tmp \
        -comp xz -b 262144 -noappend 2>/dev/null

    if [ -f "$ROOTFS_SRC" ]; then
        pass "Created: $ROOTFS_SRC ($(stat -c%s "$ROOTFS_SRC") bytes)"
    else
        die "Failed to create squashfs"
    fi
fi

# =============================================================================
# Confirmation
# =============================================================================

echo "============================================="
echo "  INSTALLATION PLAN"
echo "============================================="
echo ""
printf "  Mode:       %s\n" "$INSTALL_MODE"
printf "  Target:     mtd%s (%s)\n" "$TARGET_MTD" \
    "$(grep "mtd${TARGET_MTD}:" /proc/mtd 2>/dev/null | sed 's/.*"\(.*\)".*/\1/')"
printf "  Rootfs:     %s\n" "$ROOTFS_SRC"
if [ -n "$KERNEL_SRC" ] && [ "$KERNEL_SRC" != "UBI_EXISTING" ]; then
    printf "  Kernel:     %s\n" "$KERNEL_SRC"
else
    printf "  Kernel:     (preserved from NAND)\n"
fi
echo ""
printf "  ${YEL}WARNING: This will modify NAND flash.${NC}\n"
printf "  Recovery: SD card boot or recovery partition.\n"
echo ""

if ! $AUTO_YES && ! $DRY_RUN; then
    printf "Proceed with NAND installation? (yes/no): "
    read confirm
    [ "$confirm" = "yes" ] || { echo "Aborted."; exit 0; }
fi

echo ""

# =============================================================================
# Installation
# =============================================================================

case "$INSTALL_MODE" in

    ubi_replace)
        # -----------------------------------------------
        # Mode A: Replace rootfs in existing UBI layout
        # -----------------------------------------------
        info "=== UBI Rootfs Replacement ==="

        # Stop any mining daemon
        for proc in bosminer cgminer bmminer dcentrald dashd; do
            pid=$(pidof $proc 2>/dev/null)
            if [ -n "$pid" ]; then
                info "Stopping $proc..."
                run kill $pid
                sleep 1
            fi
        done

        # Flash rootfs to ubi0_1
        info "Writing rootfs to UBI volume 1 (rootfs)..."
        run ubiupdatevol /dev/ubi0_1 "$ROOTFS_SRC"
        pass "Rootfs written to ubi0_1"

        # Flash kernel if we have one (and it's not already in UBI)
        if [ -n "$KERNEL_SRC" ] && [ "$KERNEL_SRC" != "UBI_EXISTING" ]; then
            info "Writing kernel to UBI volume 0 (kernel)..."
            run ubiupdatevol /dev/ubi0_0 "$KERNEL_SRC"
            pass "Kernel written to ubi0_0"
        else
            info "Preserving existing kernel in ubi0_0"
        fi

        # Clear overlay for clean boot
        info "Clearing rootfs_data overlay..."
        if [ -c /dev/ubi0_2 ]; then
            run ubiupdatevol /dev/ubi0_2 -t
            pass "Overlay cleared (ubi0_2)"
        else
            warn "No overlay volume (ubi0_2) — skipping"
        fi
        ;;

    ubi_create)
        # -----------------------------------------------
        # Mode B: Create new UBI from scratch
        # -----------------------------------------------
        info "=== Creating New UBI on mtd$TARGET_MTD ==="

        # Detach any existing UBI
        if $UBI_ATTACHED; then
            info "Detaching existing UBI..."
            run ubidetach -m "$EXISTING_UBI_MTD" 2>/dev/null || true
        fi

        # Format the MTD partition for UBI
        info "Formatting mtd$TARGET_MTD for UBI..."
        warn "This erases ALL data on the partition!"
        run ubiformat /dev/mtd$TARGET_MTD -y -q
        pass "mtd$TARGET_MTD formatted for UBI"

        # Attach UBI
        info "Attaching UBI..."
        run ubiattach -m $TARGET_MTD -d 0
        pass "UBI attached as ubi0"

        # Create volumes
        info "Creating UBI volumes..."

        # Volume 0: kernel
        if [ -n "$KERNEL_SRC" ] && [ "$KERNEL_SRC" != "UBI_EXISTING" ]; then
            KERNEL_SIZE=$(stat -c%s "$KERNEL_SRC")
            run ubimkvol /dev/ubi0 -N kernel -s $KERNEL_VOL_SIZE
            pass "Created kernel volume ($KERNEL_VOL_SIZE)"

            info "Writing kernel..."
            run ubiupdatevol /dev/ubi0_0 "$KERNEL_SRC"
            pass "Kernel written"
        else
            # Create empty kernel volume (will use existing NAND kernel)
            run ubimkvol /dev/ubi0 -N kernel -s $KERNEL_VOL_SIZE
            warn "Empty kernel volume created — existing NAND kernel will need to be copied"
        fi

        # Volume 1: rootfs
        run ubimkvol /dev/ubi0 -N rootfs -s $ROOTFS_VOL_SIZE
        pass "Created rootfs volume ($ROOTFS_VOL_SIZE)"

        info "Writing rootfs..."
        run ubiupdatevol /dev/ubi0_1 "$ROOTFS_SRC"
        pass "Rootfs written"

        # Volume 2: rootfs_data (use remaining space)
        run ubimkvol /dev/ubi0 -N rootfs_data -m
        pass "Created rootfs_data volume (remaining space)"
        ;;

    raw_nand)
        # -----------------------------------------------
        # Mode C: Raw NAND write (no UBI)
        # -----------------------------------------------
        info "=== Raw NAND Write to mtd$TARGET_MTD ==="
        warn "No UBI wear-leveling — for emergency use only."

        # No-brick pre-erase guards (wf_c00e5d9e A/B hardening, 2026-05-29).
        # raw_nand is the last-resort recovery path (--force-unsafe, S9/AM1 bench
        # only); the erase below is IRREVERSIBLE. Both guards run BEFORE the erase
        # and are READ-ONLY (they never write NAND), so they can only make the
        # path safer — never brick.
        #
        # Guard 1 (fail-closed source sanity): refuse to erase the slot if the
        # rootfs source is missing or empty — erasing then writing nothing is a
        # guaranteed brick.
        if [ ! -s "$ROOTFS_SRC" ]; then
            die "Refusing raw_nand erase: rootfs source '$ROOTFS_SRC' is missing or empty (erasing mtd$TARGET_MTD then writing nothing would brick the slot)."
        fi
        # Guard 2 (no-brick restore point): snapshot the current mtd contents via
        # READ-ONLY nanddump before the erase, so a corrupt write can be restored.
        # If the backup cannot be taken, ABORT before erasing (fail-safe — never
        # erase the last-resort slot without a restore point). /tmp is volatile by
        # design (covers an in-session bad write, not power loss); a persistent
        # copy is the operator's call.
        RAW_NAND_BACKUP="/tmp/mtd${TARGET_MTD}.pre-install.bak"
        if $DRY_RUN; then
            info "[DRY] would back up /dev/mtd$TARGET_MTD -> $RAW_NAND_BACKUP before erase"
        else
            info "Backing up current mtd$TARGET_MTD to $RAW_NAND_BACKUP before erase (no-brick restore point)..."
            # --omitoob: dump DATA only (no spare/OOB bytes) so the image
            # round-trips CLEANLY through `nandwrite -p` (which writes data-only;
            # a default OOB-interleaved dump would mis-align every page on restore
            # and corrupt the slot — adversarial review wf review, 2026-05-29).
            # --bb=skipbad: skip bad blocks to match `nandwrite -p`'s skip
            # behaviour (same flag the amlogic installer uses). NOTE: a bit-exact
            # restore is not guaranteed if blocks go bad between dump and restore.
            if nanddump --omitoob --bb=skipbad -f "$RAW_NAND_BACKUP" /dev/mtd$TARGET_MTD 2>/dev/null && [ -s "$RAW_NAND_BACKUP" ]; then
                pass "Backup saved: $RAW_NAND_BACKUP ($(stat -c%s "$RAW_NAND_BACKUP" 2>/dev/null || echo '?') bytes, data-only). Restore with: nandwrite -p /dev/mtd$TARGET_MTD $RAW_NAND_BACKUP"
            else
                die "Pre-write backup of mtd$TARGET_MTD failed (nanddump) — ABORTING before erase: refusing to overwrite the last-resort recovery slot with no restore point."
            fi
        fi

        info "Erasing mtd$TARGET_MTD..."
        run flash_erase /dev/mtd$TARGET_MTD 0 0
        pass "mtd$TARGET_MTD erased"

        info "Writing rootfs..."
        run nandwrite -p /dev/mtd$TARGET_MTD "$ROOTFS_SRC"
        pass "Rootfs written to mtd$TARGET_MTD"
        ;;
esac

echo ""

# =============================================================================
# Update U-Boot Environment
# =============================================================================

if ! $PRESERVE_ENV; then
    info "=== Updating U-Boot Environment ==="

    # Determine which firmware slot we installed to
    FW_SLOT=""
    case "$TARGET_MTD" in
        7) FW_SLOT=1 ;;
        8) FW_SLOT=2 ;;
        *) FW_SLOT="" ;;
    esac

    if [ -n "$FW_SLOT" ] && command -v fw_setenv >/dev/null 2>&1; then
        info "Setting firmware=$FW_SLOT in U-Boot env..."
        run fw_setenv firmware "$FW_SLOT"
        pass "U-Boot firmware=$FW_SLOT"

        # Clear SD boot flag (we want NAND boot now)
        run fw_setenv sd_boot 2>/dev/null || true

        # Enable rollback protection for the first NAND boot. S99upgrade clears
        # this only after health checks pass.
        run fw_setenv upgrade_stage 0
        run fw_setenv first_boot 2>/dev/null || true

        info "U-Boot environment updated"
    elif [ -n "$FW_SLOT" ] && [ -c /dev/mtd4 ]; then
        # No fw_setenv — use the Python script if available.
        #
        # SCOPE: this raw nanddump/flash_erase/nandwrite /dev/mtd4 fallback is
        # am1/S9-ONLY. The board-family gate near the top of this script
        # (`die "sd_nand_install.sh is validated only for S9/AM1"`) HARD-EXITS
        # for any aarch64 / s17 / s19 / s21 / am2 board before NAND detection,
        # so this raw env-flip can never run on an AM2 weak-ECC pl35x-nand env
        # partition (the .139/.74 corruption class). On AM2 the env flip MUST
        # use fw_setenv via the on-device sysupgrade — see
        # . The fw_setenv
        # branch above is always preferred; this is only reached on an am1-s9
        # image that somehow lacks libubootenv.
        SWITCH_FIRMWARE_SCRIPT=""
        if [ -f /usr/sbin/switch_firmware.py ]; then
            SWITCH_FIRMWARE_SCRIPT="/usr/sbin/switch_firmware.py"
        elif [ -f /opt/dcentos/switch_firmware.py ]; then
            SWITCH_FIRMWARE_SCRIPT="/opt/dcentos/switch_firmware.py"
        fi

        if command -v python3 >/dev/null 2>&1 && [ -n "$SWITCH_FIRMWARE_SCRIPT" ]; then
            info "Using switch_firmware.py to set firmware=$FW_SLOT..."
            run nanddump /dev/mtd4 -f /tmp/uboot_env.bin
            # W24-OTA-2: switch_firmware.py now requires explicit
            # acknowledgement that it is NOT fw_setenv. This branch is the
            # documented no-libubootenv last resort (the fw_setenv path above
            # already failed command -v), so the ack is appropriate here.
            run python3 "$SWITCH_FIRMWARE_SCRIPT" "$FW_SLOT" --i-understand-this-is-not-fw-setenv
            run flash_erase /dev/mtd4 0 0
            run nandwrite -p /dev/mtd4 /tmp/uboot_env_patched.bin
            run rm -f /tmp/uboot_env.bin /tmp/uboot_env_patched.bin
            pass "U-Boot env patched via switch_firmware.py"
        else
            warn "Cannot update U-Boot env (no fw_setenv or python3)."
            warn "You may need to set firmware=$FW_SLOT manually."
            echo ""
            echo "  From U-Boot console (serial):"
            echo "    setenv firmware $FW_SLOT"
            echo "    setenv sd_boot"
            echo "    saveenv"
        fi
    else
        info "U-Boot env update skipped (no firmware slot determined)"
    fi
else
    info "U-Boot env preserved (--preserve-env)"
fi

echo ""

# =============================================================================
# Verification
# =============================================================================

info "=== Verification ==="

# Verify UBI volumes if applicable
if [ "$INSTALL_MODE" = "ubi_replace" ] || [ "$INSTALL_MODE" = "ubi_create" ]; then
    if [ -c /dev/ubi0_1 ]; then
        ROOTFS_STORED=$(cat /sys/class/ubi/ubi0_1/data_bytes 2>/dev/null || echo 0)
        ROOTFS_ORIG=$(stat -c%s "$ROOTFS_SRC" 2>/dev/null || echo 0)
        if [ "$ROOTFS_STORED" = "$ROOTFS_ORIG" ]; then
            pass "Rootfs size verified: $ROOTFS_STORED bytes"
        else
            warn "Rootfs size mismatch: stored=$ROOTFS_STORED, original=$ROOTFS_ORIG"
        fi
    fi
fi

# Check UBI health
if [ -d /sys/class/ubi/ubi0 ]; then
    BAD_PEBS=$(cat /sys/class/ubi/ubi0/bad_peb_count 2>/dev/null || echo "?")
    TOTAL_PEBS=$(cat /sys/class/ubi/ubi0/total_eraseblocks 2>/dev/null || echo "?")
    info "UBI health: $BAD_PEBS bad PEBs / $TOTAL_PEBS total"
fi

echo ""

# =============================================================================
# Complete
# =============================================================================

echo "============================================="
printf "  ${GRN}${BLD}INSTALLATION COMPLETE${NC}\n"
echo "============================================="
echo ""
echo "  DCENTos has been written to NAND (mtd$TARGET_MTD)."
echo ""
echo "  Next steps:"
echo "    1. Power off the miner"
echo "    2. Remove the SD card"
if [ "$BOOT_SOURCE" = "sd_fit" ]; then
    echo "    3. If JP4 was moved for standalone SD boot: move it back to RIGHT (NAND)"
fi
echo "    3. Power on — miner will boot DCENTos from NAND"
echo ""
echo "  SSH:  root@<miner_ip>  (bootstrap password: dcentral)"
echo "  Dashboard/API: set the owner password on first boot"
echo ""
echo "  Recovery options:"
echo "    - Boot from SD card (re-insert + JP4 to LEFT)"
echo "    - Recovery partition (hold reset 10s during power-on)"
echo "    - Reflash via SD card with BraiinsOS or stock firmware"
echo ""

if ! $DRY_RUN; then
    printf "Reboot now? (yes/no): "
    read reboot_confirm
    if [ "$reboot_confirm" = "yes" ]; then
        echo ""
        warn "Remove the SD card BEFORE the miner finishes rebooting!"
        echo "  Rebooting in 5 seconds..."
        sleep 5
        reboot
    else
        info "Reboot manually when ready: reboot"
    fi
fi
