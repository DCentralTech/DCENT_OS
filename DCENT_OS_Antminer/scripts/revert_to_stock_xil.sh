#!/bin/sh
#
# revert_to_stock_xil.sh — Revert an am2 Zynq XIL (Antminer S19 / S19a /
# S19j / S19j Pro / S19 Pro / T19 with `awesome` XIL firmware) from
# DCENT_OS / BraiinsOS back to stock XIL Anthill firmware.
#
# ============================================================================
# LOAD-BEARING SAFETY CONTRACT (Phase 4C, 2026-05-15 )
# ============================================================================
#
# This script overwrites NAND on a live miner and is irreversible without a
# physical write to a different image. It refuses to run unless BOTH of the
# following are present:
#
#   1. The `--operator-acknowledged-data-loss` flag is on the command line.
#   2. The env-gate `DCENT_REVERT_AUTHORIZED=1` is set in the calling shell.
#
# `--dry-run` bypasses both gates and prints the planned commands without
# executing. Every invocation emits a JSON manifest under
# `DCENT_OS_Antminer/output/revert-manifests/` capturing timestamp, target,
# firmware SHA256, mode (dry-run|live), and result (planned|executed|refused).
#
# ============================================================================
# WHY THIS IS DISTINCT FROM revert_to_stock_s19_am2.sh
# ============================================================================
#
# `revert_to_stock_s19_am2.sh` assumes the BraiinsOS dual-slot NAND layout
# (mtd7=firmware1, mtd8=firmware2). Stock XIL has a fundamentally different
# layout:
#
#   DTB label              Offset       Size      Purpose
#   ---------              ------       ----      -------
#   BOOT.bin+kernel        0x00000000   40 MiB    FSBL + U-Boot + kernel
#   ramfs (mtd1)           0x02800000   32 MiB    active rootfs FIT
#   configs (mtd2)         0x04800000    8 MiB    firmware configs (UBI)
#   sig                    0x05000000    2 MiB    signature blobs
#   reserve1               0x05200000   14 MiB    reserved
#   upgrade-ramfs          0x06000000   16 MiB    upgrade staging
#   upgrade-file           0x07000000   56 MiB    upgrade staging
#   reserve2               0x0a800000   88 MiB    reserved
#
# SINGLE-SLOT: one ramfs at mtd1, no firmware1/firmware2 split. Recovery to
# stock requires writing the stock `ramfs.bak` (the rootfs FIT that XIL backed
# up to `/mnt/nvdata/anthillos/ramfs.bak` at install time) back to /dev/mtd1.
#
# ============================================================================
# EXPECTED FLEET STATE
# ============================================================================
#
# Before:    Unit is running DCENT_OS on am2-XIL hardware (post installation
#            from stock XIL Anthill firmware).
# After:     Unit boots stock XIL Anthill firmware. All DCENT_OS config in
#            /data/dcent + dashboard credentials are gone.
# Recovery
# if revert
# fails:     The in-RAM rootfs (still resident in RAM during the script run)
#            remains the boot environment until power cycle. If interrupted
#            mid-write, panic-reboot trap forces a sysrq reboot so the operator
#            can re-run the script.
#
# Usage:
#   DCENT_REVERT_AUTHORIZED=1 ./revert_to_stock_xil.sh \
#       --operator-acknowledged-data-loss \
#       --firmware /path/to/ramfs.bak \
#       --target 203.0.113.109 \
#       [--sha256 <expected>]
#
#   ./revert_to_stock_xil.sh --dry-run --firmware /path/to/ramfs.bak \
#       --target 203.0.113.109

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
if [ ! -r "$SCRIPT_DIR/lib/revert_common.sh" ]; then
    echo "ERROR: missing shared revert helper: $SCRIPT_DIR/lib/revert_common.sh" >&2
    exit 1
fi
. "$SCRIPT_DIR/lib/revert_common.sh"

revert_init "xil" "DCENT_REVERT_AUTHORIZED"

REVERT_EXPECTED_SHA256=""

print_help() {
    cat <<EOF
Usage: $0 [options]

  --operator-acknowledged-data-loss  Required for live execution.
  --dry-run                          Print planned commands without executing.
  --target <ip>                      Target miner IP (informational; this
                                     script runs ON the miner over SSH or
                                     serial).
  --firmware <path>                  Stock XIL ramfs.bak file.
  --sha256 <hex>                     Optional expected SHA-256 of ramfs.bak.
  --help                             This help.

Env-gate (REQUIRED for live execution): DCENT_REVERT_AUTHORIZED=1
EOF
}

normalize_target_signal() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]'
}

require_exact_xil_target() {
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] would verify /etc/dcentos/board_target is an exact XIL S19-family target"
        return 0
    fi

    BOARD_TARGET=$(cat /etc/dcentos/board_target 2>/dev/null | head -1 | tr -d '[:space:]' || true)
    PLATFORM=$(cat /etc/dcentos/platform 2>/dev/null || cat /etc/bos_platform 2>/dev/null || echo unknown)
    DT_MODEL=$(tr '\000' '\n' < /proc/device-tree/model 2>/dev/null | head -1 || true)
    IDENTITY=$(printf 'board_target=%s platform=%s model=%s' "$BOARD_TARGET" "$PLATFORM" "$DT_MODEL")
    IDENTITY_NORM=$(normalize_target_signal "$IDENTITY")
    BOARD_NORM=$(normalize_target_signal "$BOARD_TARGET")

    case "$IDENTITY_NORM" in
        *s19xp*|*s19jxp*|*t19*|*s17*|*t17*)
            echo "ERROR: $IDENTITY is an Experimental feature / In development or non-XIL-S19 target for this revert." >&2
            revert_emit_manifest "$REVERT_FIRMWARE" "refused"
            exit 1
            ;;
    esac
    case "$BOARD_NORM" in
        am2s19|am2s19pro|am2s19j|am2s19jpro|am2s19jprozynq|am2s19jproxil25|am2xil25)
            echo "Exact XIL board target verified: $BOARD_TARGET"
            ;;
        *)
            echo "ERROR: /etc/dcentos/board_target must be an exact XIL S19-family target before destructive revert." >&2
            echo "       Observed: ${BOARD_TARGET:-missing}" >&2
            revert_emit_manifest "$REVERT_FIRMWARE" "refused"
            exit 1
            ;;
    esac
}

# Parse common args via the helper. Then walk EXTRA for --sha256.
# Capture the return code without tripping `set -e` on non-zero.
rc=0
revert_parse_args "$@" || rc=$?
if [ "$rc" = "10" ]; then
    print_help
    exit 0
elif [ "$rc" != "0" ]; then
    exit "$rc"
fi

# Extract --sha256 from EXTRA_ARGS (left-overs the common parser didn't claim).
set -- $REVERT_EXTRA_ARGS
while [ $# -gt 0 ]; do
    case "$1" in
        --sha256)
            REVERT_EXPECTED_SHA256=$(printf '%s' "${2:-}" | tr 'A-F' 'a-f')
            shift 2
            ;;
        *)
            echo "WARN: ignoring unknown argument: $1" >&2
            shift
            ;;
    esac
done

if [ -z "$REVERT_FIRMWARE" ]; then
    echo "ERROR: --firmware <ramfs.bak> is required." >&2
    print_help >&2
    revert_emit_manifest "" "refused"
    exit 1
fi

if ! revert_check_authorization; then
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

echo "============================================="
echo "  DCENT_OS XIL Stock Revert (single-slot mtd1)"
echo "  family: $REVERT_FAMILY   mode: $([ "$REVERT_DRY_RUN" = "1" ] && echo dry-run || echo live)"
echo "============================================="
echo ""

revert_ssh_preflight "$REVERT_TARGET"

# Identity gate: only checked in live mode.
MTD_RAMFS="/dev/mtd1"
NVDATA_RAMFS="/mnt/nvdata/anthillos/ramfs.bak"
if [ "$REVERT_DRY_RUN" != "1" ]; then
    if [ ! -e "$MTD_RAMFS" ]; then
        echo "ERROR: $MTD_RAMFS missing. This script targets stock-XIL single-slot layout." >&2
        echo "       If this is a BraiinsOS-class am2 unit (mtd7/mtd8 firmware A/B)," >&2
        echo "       use scripts/revert_to_stock_s19_am2.sh instead." >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    RAMFS_SIZE=$(cat /sys/class/mtd/mtd1/size 2>/dev/null || echo 0)
    case "$RAMFS_SIZE" in
        33554432|31457280)
            echo "Detected XIL-class /dev/mtd1 size: $RAMFS_SIZE bytes"
            ;;
        *)
            echo "ERROR: /dev/mtd1 size $RAMFS_SIZE does not match XIL ramfs (~32 MiB)." >&2
            echo "       Refusing to flash — wrong layout or wrong partition number." >&2
            revert_emit_manifest "$REVERT_FIRMWARE" "refused"
            exit 1
            ;;
    esac
else
    echo "[dry-run] would verify $MTD_RAMFS exists and size ~= 32 MiB"
    RAMFS_SIZE=33554432
fi
require_exact_xil_target

# Locate ramfs.bak: positional arg first, then nvdata fallback.
RAMFS_BAK="$REVERT_FIRMWARE"
if [ ! -f "$RAMFS_BAK" ] && [ -f "$NVDATA_RAMFS" ]; then
    echo "Firmware not found at $RAMFS_BAK; falling back to nvdata: $NVDATA_RAMFS"
    RAMFS_BAK="$NVDATA_RAMFS"
fi

if [ ! -f "$RAMFS_BAK" ] && [ "$REVERT_DRY_RUN" != "1" ]; then
    cat >&2 <<USAGE
ERROR: stock XIL ramfs.bak not found at: $RAMFS_BAK

Provide it explicitly:
    $0 --firmware /path/to/ramfs.bak --operator-acknowledged-data-loss

Or, restore the unit from a known-good backup containing
/mnt/nvdata/anthillos/ramfs.bak from the original XIL install.
USAGE
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

if [ -f "$RAMFS_BAK" ]; then
    RAMFS_SIZE_BYTES=$(stat -c%s "$RAMFS_BAK" 2>/dev/null || wc -c <"$RAMFS_BAK")
    echo "ramfs.bak: $RAMFS_BAK"
    echo "ramfs.bak size: $RAMFS_SIZE_BYTES bytes"

    if [ "$RAMFS_SIZE_BYTES" -lt 1048576 ]; then
        echo "ERROR: ramfs.bak suspiciously small ($RAMFS_SIZE_BYTES bytes)." >&2
        revert_emit_manifest "$RAMFS_BAK" "refused"
        exit 1
    fi
    if [ "$RAMFS_SIZE_BYTES" -gt "$RAMFS_SIZE" ]; then
        echo "ERROR: ramfs.bak ($RAMFS_SIZE_BYTES bytes) larger than mtd1 ($RAMFS_SIZE bytes)." >&2
        revert_emit_manifest "$RAMFS_BAK" "refused"
        exit 1
    fi

    # Verify FIT magic (0xd00dfeed) at offset 0.
    MAGIC=$(od -An -N4 -tx1 "$RAMFS_BAK" 2>/dev/null | tr -d ' \n' || echo "")
    case "$MAGIC" in
        d00dfeed)
            echo "ramfs.bak FIT magic verified (0xd00dfeed)."
            ;;
        *)
            echo "ERROR: ramfs.bak missing FIT magic at offset 0 (got: $MAGIC)." >&2
            echo "       Expected 0xd00dfeed (U-Boot FIT). Refusing to flash." >&2
            revert_emit_manifest "$RAMFS_BAK" "refused"
            exit 1
            ;;
    esac

    # Optional SHA256 verify.
    if [ -n "$REVERT_EXPECTED_SHA256" ]; then
        ACTUAL_SHA256=$(revert_sha256 "$RAMFS_BAK")
        ACTUAL_SHA256=$(printf '%s' "$ACTUAL_SHA256" | tr 'A-F' 'a-f')
        if [ "$ACTUAL_SHA256" != "$REVERT_EXPECTED_SHA256" ]; then
            echo "ERROR: ramfs.bak SHA-256 drift." >&2
            echo "  expected: $REVERT_EXPECTED_SHA256" >&2
            echo "  actual:   $ACTUAL_SHA256" >&2
            revert_emit_manifest "$RAMFS_BAK" "refused"
            exit 1
        fi
        echo "ramfs.bak SHA-256 verified."
    fi
fi

# Backup /data first (XIL DCENT_OS persistent state).
echo ""
echo "Step 0: Backing up /data persistent state..."
DATA_BACKUP_PATH="/tmp/dcent-xil-data-backup-$(revert_utc_stamp).tar.gz"
revert_run sh -c "tar -czf '$DATA_BACKUP_PATH' /data 2>/dev/null || true"
echo "Data backup: $DATA_BACKUP_PATH"

echo ""
echo "WARNING: This will erase /dev/mtd1 and write the stock XIL ramfs."
echo "         After reboot, the unit will boot stock XIL Anthill firmware."
echo "         DCENT_OS will no longer be the active firmware."
echo ""
if [ "$REVERT_DRY_RUN" != "1" ]; then
    printf "Type 'REVERT-XIL' to proceed: "
    read CONFIRM
    if [ "$CONFIRM" != "REVERT-XIL" ]; then
        echo "Aborted."
        revert_emit_manifest "$RAMFS_BAK" "refused"
        exit 0
    fi
    trap 'echo "INTERRUPTED — forcing reboot via sysrq."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP
fi

echo "Step 1: Erasing /dev/mtd1 (XIL ramfs)..."
if ! revert_run flash_erase "$MTD_RAMFS" 0 0; then
    echo "ERROR: flash_erase failed — /dev/mtd1 may be partially erased." >&2
    echo "       The unit can boot only from the in-RAM rootfs until written." >&2
    echo "       Recovery: re-run this script (idempotent)." >&2
    revert_emit_manifest "$RAMFS_BAK" "refused"
    exit 1
fi

echo "Step 2: Writing stock ramfs to NAND..."
if ! revert_run nandwrite -p "$MTD_RAMFS" "$RAMFS_BAK"; then
    echo "ERROR: nandwrite failed — /dev/mtd1 is BLANK." >&2
    echo "       DO NOT POWER CYCLE without re-flashing. Re-run this script." >&2
    revert_emit_manifest "$RAMFS_BAK" "refused"
    exit 1
fi

if [ "$REVERT_DRY_RUN" != "1" ]; then
    echo "Step 3: Readback verify..."
    WROTE_MAGIC=$(nanddump -s 0 -l 4 "$MTD_RAMFS" 2>/dev/null | tail -c 4 | od -An -tx1 | tr -d ' \n' || echo "")
    case "$WROTE_MAGIC" in
        d00dfeed)
            echo "Post-write readback: FIT magic verified."
            ;;
        *)
            echo "ERROR: post-write readback magic mismatch (got: $WROTE_MAGIC, expected d00dfeed)." >&2
            echo "       DO NOT POWER CYCLE. Re-run the script with the correct ramfs.bak." >&2
            revert_emit_manifest "$RAMFS_BAK" "refused"
            exit 1
            ;;
    esac
    trap - INT TERM HUP
else
    echo "[dry-run] would readback first 4 bytes from $MTD_RAMFS and verify FIT magic 0xd00dfeed"
fi

echo ""
echo "============================================="
echo "  Stock XIL revert complete."
echo "============================================="
echo ""
echo "  Stock XIL ramfs written to /dev/mtd1."
echo "  Reboot now to boot stock XIL firmware:"
echo ""
echo "    reboot"
echo ""
echo "  NOTE: stock XIL configs may be re-initialized from defaults on"
echo "  first boot. Re-enter pool credentials via the stock web UI."
echo ""

if [ "$REVERT_DRY_RUN" = "1" ]; then
    revert_emit_manifest "$RAMFS_BAK" "planned"
else
    revert_emit_manifest "$RAMFS_BAK" "executed"
fi
