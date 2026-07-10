#!/bin/sh
#
# revert_to_stock_am3_aml_s19jpro.sh — Revert an S19j Pro (Amlogic A113D,
# BM1362, BHB42xxx hashboards) from DCENTos back to stock Bitmain firmware.
#
# ============================================================================
# LOAD-BEARING SAFETY CONTRACT (Phase 4C, 2026-05-15 )
# ============================================================================
#
# Refuses to run unless BOTH:
#   1. `--operator-acknowledged-data-loss` is on the command line.
#   2. `DCENT_REVERT_AUTHORIZED=1` is set in the calling shell.
# `--dry-run` bypasses gates and prints planned commands. Emits a JSON
# manifest under DCENT_OS_Antminer/output/revert-manifests/ on every invocation.
#
# ============================================================================
# WHY THIS IS DISTINCT FROM revert_to_stock_am3_aml_s21.sh
# ============================================================================
#
# Same Amlogic A113D silicon as S21 — flash mechanism, uImage magic, NAND
# rootfs window, U-Boot env layout are identical (per
# scripts/lib/am3_geometry.sh). The only family-axis difference is the
# product-family validation string: `s19jpro-amlogic` instead of
# `s21-amlogic`. The BM1362 hashboards (BHB42601/BHB42801/BHB42611) vs S21's
# BM1368 (BHB42611/56902) are hashboard-side and don't change flash
# primitives.
#
# Provenance: cloned from revert_to_stock_am3_aml_s21.sh ( W12-B).
#
# `verified_revertable: false` in PROFILE_TABLE.amlogic-a113d-bm1362 — this
# script is CODE-COMPLETE but NOT live-tested. The office S19j Pro Amlogic
# .78 + s19jpro are the planned bench targets.
#
# ============================================================================
# EXPECTED FLEET STATE
# ============================================================================
#
# Before:    Unit running DCENT_OS on am3-aml-s19jpro hardware (post install
#            from stock Bitmain or VNish AML).
# After:     Unit boots stock Bitmain S19j Pro firmware on the next reboot.
#            DCENT_OS state in mtd5 rootfs window is gone; nvdata stays.
# Recovery
# if revert
# fails:     The previous DCENTos rootfs window may still boot — the operator
#            should restore from /data/restore-backup-<ts>/ (created by the
#            dcentrald destructive path) before rebooting.
#
# Usage:
#   DCENT_REVERT_AUTHORIZED=1 ./revert_to_stock_am3_aml_s19jpro.sh \
#       --operator-acknowledged-data-loss \
#       --firmware Antminer-S19jpro-AML-release-XXXXX.tar.gz \
#       --target 203.0.113.133 \
#       [--sha256 <expected>]
#
#   ./revert_to_stock_am3_aml_s19jpro.sh --dry-run \
#       --firmware ./stock-s19jpro-aml.tar.gz --target 203.0.113.133

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
if [ ! -r "$SCRIPT_DIR/lib/revert_common.sh" ]; then
    echo "ERROR: missing shared revert helper: $SCRIPT_DIR/lib/revert_common.sh" >&2
    exit 1
fi
if [ ! -r "$SCRIPT_DIR/lib/am3_geometry.sh" ]; then
    echo "ERROR: missing shared AM3 geometry file: $SCRIPT_DIR/lib/am3_geometry.sh" >&2
    exit 1
fi
. "$SCRIPT_DIR/lib/revert_common.sh"
. "$SCRIPT_DIR/lib/am3_geometry.sh"

revert_init "am3_aml_s19jpro" "DCENT_REVERT_AUTHORIZED"

PRODUCT_FAMILY="s19jpro-amlogic"
ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"
ROOTFS_OFFSET="$DCENT_AM3_ROOTFS_OFFSET_HEX"
UIMAGE_MAGIC_HEX="27051956"

REVERT_EXPECTED_SHA256=""
MAX_EXTRACTED_KB="${DCENT_STOCK_REVERT_MAX_EXTRACTED_KB:-262144}"

print_help() {
    cat <<EOF
Usage: $0 [options]

  --operator-acknowledged-data-loss  Required for live execution.
  --dry-run                          Print planned commands without executing.
  --target <ip>                      Target miner IP.
  --firmware <path>                  Stock Bitmain S19j Pro AML tarball
                                     (Antminer-S19jpro-AML-release-*.tar.gz).
  --sha256 <hex>                     Optional expected SHA-256 of firmware.
  --help                             This help.

Product family: $PRODUCT_FAMILY
Env-gate (REQUIRED for live execution): DCENT_REVERT_AUTHORIZED=1
EOF
}

rc=0
revert_parse_args "$@" || rc=$?
if [ "$rc" = "10" ]; then
    print_help
    exit 0
elif [ "$rc" != "0" ]; then
    exit "$rc"
fi

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
    echo "ERROR: --firmware <path> is required." >&2
    print_help >&2
    revert_emit_manifest "" "refused"
    exit 1
fi

if ! revert_check_authorization; then
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

echo "==========================================================="
echo "  DCENTos -> Stock Bitmain Firmware Revert"
echo "  family: $REVERT_FAMILY   product: $PRODUCT_FAMILY"
echo "  mode:   $([ "$REVERT_DRY_RUN" = "1" ] && echo dry-run || echo live)"
echo "==========================================================="
echo ""

revert_ssh_preflight "$REVERT_TARGET"

if [ "$REVERT_DRY_RUN" != "1" ]; then
    CURRENT_SLOT=$(fw_printenv -n dcent_boot_slot 2>/dev/null || echo "1")
    echo "Current dcent_boot_slot: $CURRENT_SLOT"
    # Confirm we are on an Amlogic S19j Pro by reading /etc/dcentos/board_target.
    if [ -r /etc/dcentos/board_target ]; then
        BOARD_TARGET=$(cat /etc/dcentos/board_target 2>/dev/null || echo "")
        case "$BOARD_TARGET" in
            am3-aml-s19jpro|amlogic-a113d-bm1362|s19jpro-amlogic)
                echo "Board target verified: $BOARD_TARGET"
                ;;
            *)
                echo "ERROR: /etc/dcentos/board_target='$BOARD_TARGET' does not match $PRODUCT_FAMILY." >&2
                revert_emit_manifest "$REVERT_FIRMWARE" "refused"
                exit 1
                ;;
        esac
    else
        echo "WARN: /etc/dcentos/board_target missing; proceeding on operator authorization alone." >&2
    fi
else
    echo "[dry-run] would verify /etc/dcentos/board_target matches $PRODUCT_FAMILY"
    echo "[dry-run] would read dcent_boot_slot from fw_printenv"
fi

if [ ! -f "$REVERT_FIRMWARE" ]; then
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] firmware $REVERT_FIRMWARE missing on dev host; would-be exit in live mode"
    else
        echo "ERROR: Firmware image not found: $REVERT_FIRMWARE" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
else
    echo "Firmware image: $REVERT_FIRMWARE"
    echo "Image size: $(ls -lh "$REVERT_FIRMWARE" | awk '{print $5}')"
fi

if [ -n "$REVERT_EXPECTED_SHA256" ] && [ -f "$REVERT_FIRMWARE" ]; then
    ACTUAL_SHA256=$(revert_sha256 "$REVERT_FIRMWARE")
    ACTUAL_SHA256=$(printf '%s' "$ACTUAL_SHA256" | tr 'A-F' 'a-f')
    if [ "$ACTUAL_SHA256" != "$REVERT_EXPECTED_SHA256" ]; then
        echo "ERROR: firmware SHA-256 drift before extraction." >&2
        echo "  expected: $REVERT_EXPECTED_SHA256" >&2
        echo "  actual:   $ACTUAL_SHA256" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    echo "Firmware SHA-256 verified at extraction time."
fi

echo ""
echo "WARNING: This will write stock Bitmain firmware to $ROOTFS_MTD"
echo "         offset $ROOTFS_OFFSET (uImage rootfs) and configure"
echo "         U-Boot firstboot=1 to boot it on next reboot."
echo ""
echo "         DCENTos will no longer be the active firmware after reboot."
echo ""
if [ "$REVERT_DRY_RUN" != "1" ]; then
    printf "Type 'REVERT' to proceed: "
    read CONFIRM
    if [ "$CONFIRM" != "REVERT" ]; then
        echo "Aborted."
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 0
    fi
    trap 'echo "INTERRUPTED -- forcing reboot via sysrq to recover into a clean boot state."; echo b > /proc/sysrq-trigger 2>/dev/null || reboot -f' INT TERM HUP
    trap 'rm -rf /tmp/stock_extract 2>/dev/null' EXIT
fi

EXTRACT_DIR="/tmp/stock_extract"
echo "Step 1: Extracting firmware archive..."
if [ "$REVERT_DRY_RUN" != "1" ]; then
    rm -rf "$EXTRACT_DIR"
    mkdir -p "$EXTRACT_DIR"
    tar --no-same-owner --no-same-permissions --no-overwrite-dir \
        -xzf "$REVERT_FIRMWARE" -C "$EXTRACT_DIR"

    EXTRACTED_KB=$(du -sk "$EXTRACT_DIR" | awk '{print $1}')
    if [ "$EXTRACTED_KB" -gt "$MAX_EXTRACTED_KB" ]; then
        echo "ERROR: extracted firmware tree is ${EXTRACTED_KB} KiB, above cap ${MAX_EXTRACTED_KB} KiB" >&2
        rm -rf "$EXTRACT_DIR"
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    if find "$EXTRACT_DIR" -type f -links +1 -print -quit 2>/dev/null | grep -q .; then
        echo "ERROR: firmware archive contains hard-linked files; refusing destructive revert" >&2
        rm -rf "$EXTRACT_DIR"
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi

    UIMAGE=$(find "$EXTRACT_DIR" -type f \( -name 'rootfs_uImage*' -o -name '*uImage*' -o -name 'rootfs*.bin' \) | head -1)
    if [ -z "$UIMAGE" ]; then
        echo "ERROR: No rootfs uImage found in firmware archive" >&2
        rm -rf "$EXTRACT_DIR"
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    UIMAGE_REAL=$(readlink -f "$UIMAGE")
    case "$UIMAGE_REAL" in
        "$EXTRACT_DIR"/*) ;;
        *)
            echo "ERROR: uImage symlink escapes extract dir: $UIMAGE -> $UIMAGE_REAL" >&2
            rm -rf "$EXTRACT_DIR"
            revert_emit_manifest "$REVERT_FIRMWARE" "refused"
            exit 1
            ;;
    esac

    HEAD_HEX=$(head -c 4 "$UIMAGE_REAL" | od -An -tx1 | tr -d ' \n')
    if [ "$HEAD_HEX" != "$UIMAGE_MAGIC_HEX" ]; then
        echo "ERROR: rootfs payload lacks uImage magic 27051956 (got: $HEAD_HEX)" >&2
        rm -rf "$EXTRACT_DIR"
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
else
    echo "[dry-run] tar --no-same-owner --no-same-permissions --no-overwrite-dir -xzf $REVERT_FIRMWARE -C $EXTRACT_DIR"
    echo "[dry-run] find $EXTRACT_DIR for rootfs_uImage*/uImage*/rootfs*.bin"
    echo "[dry-run] verify first 4 bytes == $UIMAGE_MAGIC_HEX"
    UIMAGE_REAL="$EXTRACT_DIR/rootfs_uImage.bin"
fi

echo "Step 2: Writing uImage to $ROOTFS_MTD offset $ROOTFS_OFFSET..."
if ! revert_run nandwrite -p -s "$ROOTFS_OFFSET" "$ROOTFS_MTD" "$UIMAGE_REAL"; then
    echo "ERROR: nandwrite failed -- rootfs slot may be partially overwritten." >&2
    echo "DO NOT POWER CYCLE -- dcent_boot_slot has NOT been flipped." >&2
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

if [ "$REVERT_DRY_RUN" != "1" ]; then
    echo "Step 3: Post-write uImage magic readback..."
    WROTE_HEX=$(nanddump -s "$ROOTFS_OFFSET" -l 4 "$ROOTFS_MTD" 2>/dev/null | tail -c 4 | od -An -tx1 | tr -d ' \n')
    if [ "$WROTE_HEX" != "$UIMAGE_MAGIC_HEX" ]; then
        echo "ERROR: post-write readback at $ROOTFS_OFFSET lacks uImage magic 27051956 (got: $WROTE_HEX)" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
else
    echo "[dry-run] would nanddump $ROOTFS_MTD at $ROOTFS_OFFSET, verify magic $UIMAGE_MAGIC_HEX"
fi

echo "Step 4: Updating U-Boot environment..."
revert_run fw_setenv firstboot 1

if [ "$REVERT_DRY_RUN" != "1" ]; then
    trap - INT TERM HUP
fi

echo ""
echo "==========================================================="
echo "  Revert complete (family: $PRODUCT_FAMILY)"
echo "==========================================================="
echo ""
echo "  Stock Bitmain firmware written to $ROOTFS_MTD offset $ROOTFS_OFFSET."
echo "  firstboot env flipped. Reboot now to start stock firmware:"
echo ""
echo "    reboot"
echo ""

if [ "$REVERT_DRY_RUN" = "1" ]; then
    revert_emit_manifest "$REVERT_FIRMWARE" "planned"
else
    rm -rf "$EXTRACT_DIR" 2>/dev/null || true
    revert_emit_manifest "$REVERT_FIRMWARE" "executed"
fi
