#!/bin/sh
#
# revert_to_stock_cv1835.sh — Revert a Cvitek CV1835 control board
# (Antminer S19j Pro CV1835 variant, BHB42XXX hashboards) from DCENTos
# back to stock Bitmain firmware via the U-Boot bootcount>3 →
# /config/factory_kernel.bin recovery path.
#
# ============================================================================
# LOAD-BEARING SAFETY CONTRACT (Phase 4C, 2026-05-15 )
# ============================================================================
#
# Refuses to run unless BOTH:
#   1. `--operator-acknowledged-data-loss` is on the command line.
#   2. `DCENT_CV1835_REVERT_AUTHORIZED=1` is set in the calling shell.
#      (DISTINCT from the am2/am3-aml DCENT_REVERT_AUTHORIZED gate — CV1835
#      is bench-only, no fleet unit; the family-specific gate keeps a stray
#      DCENT_REVERT_AUTHORIZED=1 from a Zynq revert run from bricking a
#      CV1835 by accident.)
#
# `--dry-run` bypasses both gates and prints planned commands. Emits a JSON
# manifest under DCENT_OS_Antminer/output/revert-manifests/.
#
# ============================================================================
# WHY THIS IS DISTINCT FROM revert_to_stock_am3_aml_*.sh
# ============================================================================
#
# CV1835 is single-slot eMMC (no NAND, no A/B). The recovery path is the
# U-Boot bootcount→/config/factory_kernel.bin chain documented in
# memory rule :
#
#     - U-Boot increments bootcount each (re)boot.
#     - If bootcount > 3 and no successful userspace boot has cleared it,
#       U-Boot falls back to /config/factory_kernel.bin and re-installs.
#     - There is NO `flash_erase` partition-erase; eMMC is dd-replaced by
#       the recovery kernel.
#
# This script does not directly write a stock image to eMMC. It:
#
#   1. Verifies cv1835 platform identity.
#   2. Stages /config/factory_kernel.bin (operator-supplied or already
#      resident from the original install).
#   3. Sets the U-Boot env trigger (bootcount=4) so the next reboot enters
#      recovery mode.
#   4. Optionally `reboot` (only with --operator-acknowledged-data-loss
#      AND --auto-reboot).
#
# `verified_revertable: false` — CV1835 has NO live fleet unit. This script
# is CODE-COMPLETE but NOT live-tested. Requires
# `DCENT_CV1835_EMMC_PROVEN=1` (separate gate, set after 3 successful
# bench round-trips) for production promotion.
#
# ============================================================================
# EXPECTED FLEET STATE
# ============================================================================
#
# Before:    Bench CV1835 carrier running DCENT_OS.
# After
# (reboot):  CV1835 reboots into U-Boot recovery, reads
#            /config/factory_kernel.bin, re-installs stock Bitmain S19j Pro
#            CV1835 firmware to eMMC.
# Recovery
# if revert
# fails:     Re-stage /config/factory_kernel.bin from a known-good source
#            via the U-Boot UART console (microcom on the bench cable).
#
# Usage:
#   DCENT_CV1835_REVERT_AUTHORIZED=1 ./revert_to_stock_cv1835.sh \
#       --operator-acknowledged-data-loss \
#       --firmware /path/to/factory_kernel.bin \
#       --target <cv-bench-ip> \
#       [--auto-reboot] \
#       [--sha256 <expected>]
#
#   ./revert_to_stock_cv1835.sh --dry-run --firmware ./factory_kernel.bin \
#       --target 192.168.x.x

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
if [ ! -r "$SCRIPT_DIR/lib/revert_common.sh" ]; then
    echo "ERROR: missing shared revert helper: $SCRIPT_DIR/lib/revert_common.sh" >&2
    exit 1
fi
. "$SCRIPT_DIR/lib/revert_common.sh"

revert_init "cv1835" "DCENT_CV1835_REVERT_AUTHORIZED"

PRODUCT_FAMILY="cv1835-s19jpro"
FACTORY_KERNEL_PATH="/config/factory_kernel.bin"
REVERT_EXPECTED_SHA256=""
REVERT_AUTO_REBOOT=0

print_help() {
    cat <<EOF
Usage: $0 [options]

  --operator-acknowledged-data-loss  Required for live execution.
  --dry-run                          Print planned commands without executing.
  --target <ip>                      Target CV1835 bench unit IP.
  --firmware <path>                  factory_kernel.bin to stage at $FACTORY_KERNEL_PATH.
  --sha256 <hex>                     Optional expected SHA-256 of factory_kernel.bin.
  --auto-reboot                      Reboot the unit after staging recovery.
  --help                             This help.

Product family: $PRODUCT_FAMILY
Env-gate (REQUIRED for live execution): DCENT_CV1835_REVERT_AUTHORIZED=1
Promotion gate (also required for live production): DCENT_CV1835_EMMC_PROVEN=1
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
        --auto-reboot)
            REVERT_AUTO_REBOOT=1
            shift
            ;;
        *)
            echo "WARN: ignoring unknown argument: $1" >&2
            shift
            ;;
    esac
done

if [ -z "$REVERT_FIRMWARE" ]; then
    echo "ERROR: --firmware <factory_kernel.bin> is required." >&2
    print_help >&2
    revert_emit_manifest "" "refused"
    exit 1
fi

if ! revert_check_authorization; then
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

# Promotion gate: DCENT_CV1835_EMMC_PROVEN guards live execution beyond
# bench. Dry-run + ack + env-gate are sufficient for code review / bench
# validation, but a true bench-to-fleet promotion needs all three.
if [ "$REVERT_DRY_RUN" != "1" ] && [ "${DCENT_CV1835_EMMC_PROVEN:-0}" != "1" ]; then
    echo "WARN: DCENT_CV1835_EMMC_PROVEN=1 is not set." >&2
    echo "       This script proceeds in bench-validation mode. Do NOT use on" >&2
    echo "       production fleet hardware until 3 successful sysupgrade" >&2
    echo "       round-trips have been proven and the env-gate is flipped." >&2
fi

echo "==========================================================="
echo "  DCENTos -> Stock Bitmain CV1835 Recovery Revert"
echo "  family: $REVERT_FAMILY   product: $PRODUCT_FAMILY"
echo "  mode:   $([ "$REVERT_DRY_RUN" = "1" ] && echo dry-run || echo live)"
echo "==========================================================="
echo ""

revert_ssh_preflight "$REVERT_TARGET"

if [ "$REVERT_DRY_RUN" != "1" ]; then
    # Confirm we are running on a cv1835 unit.
    if [ -r /etc/dcentos/board_target ]; then
        BOARD_TARGET=$(cat /etc/dcentos/board_target 2>/dev/null || echo "")
        case "$BOARD_TARGET" in
            cv1835-s19jpro|cvitek-cv1835|cv1835*)
                echo "Board target verified: $BOARD_TARGET"
                ;;
            *)
                echo "ERROR: /etc/dcentos/board_target='$BOARD_TARGET' does not match $PRODUCT_FAMILY." >&2
                revert_emit_manifest "$REVERT_FIRMWARE" "refused"
                exit 1
                ;;
        esac
    else
        echo "WARN: /etc/dcentos/board_target missing; cannot positively confirm CV1835." >&2
    fi
    # Confirm /config mount exists.
    if [ ! -d /config ]; then
        echo "ERROR: /config not mounted — cannot stage factory_kernel.bin." >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
else
    echo "[dry-run] would verify /etc/dcentos/board_target matches $PRODUCT_FAMILY"
    echo "[dry-run] would verify /config mount exists"
fi

if [ ! -f "$REVERT_FIRMWARE" ]; then
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] firmware $REVERT_FIRMWARE missing on dev host; would-be exit in live mode"
    else
        echo "ERROR: factory_kernel.bin not found: $REVERT_FIRMWARE" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
else
    echo "Recovery image: $REVERT_FIRMWARE"
    echo "Image size: $(ls -lh "$REVERT_FIRMWARE" | awk '{print $5}')"
fi

if [ -n "$REVERT_EXPECTED_SHA256" ] && [ -f "$REVERT_FIRMWARE" ]; then
    ACTUAL_SHA256=$(revert_sha256 "$REVERT_FIRMWARE")
    ACTUAL_SHA256=$(printf '%s' "$ACTUAL_SHA256" | tr 'A-F' 'a-f')
    if [ "$ACTUAL_SHA256" != "$REVERT_EXPECTED_SHA256" ]; then
        echo "ERROR: factory_kernel.bin SHA-256 drift." >&2
        echo "  expected: $REVERT_EXPECTED_SHA256" >&2
        echo "  actual:   $ACTUAL_SHA256" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    echo "factory_kernel.bin SHA-256 verified."
fi

echo ""
echo "WARNING: This will stage factory_kernel.bin at $FACTORY_KERNEL_PATH"
echo "         and set the U-Boot bootcount trigger to force recovery mode"
echo "         on next reboot. DCENT_OS state in eMMC will be replaced by"
echo "         stock Bitmain CV1835 firmware on next boot."
echo ""
if [ "$REVERT_DRY_RUN" != "1" ]; then
    printf "Type 'REVERT-CV1835' to proceed: "
    read CONFIRM
    if [ "$CONFIRM" != "REVERT-CV1835" ]; then
        echo "Aborted."
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 0
    fi
fi

echo "Step 1: Staging factory_kernel.bin at $FACTORY_KERNEL_PATH..."
revert_run cp "$REVERT_FIRMWARE" "$FACTORY_KERNEL_PATH"
revert_run chmod 0644 "$FACTORY_KERNEL_PATH"

echo "Step 2: Verifying staged copy..."
if [ "$REVERT_DRY_RUN" != "1" ]; then
    STAGED_SHA=$(revert_sha256 "$FACTORY_KERNEL_PATH")
    SOURCE_SHA=$(revert_sha256 "$REVERT_FIRMWARE")
    if [ "$STAGED_SHA" != "$SOURCE_SHA" ]; then
        echo "ERROR: staged factory_kernel.bin SHA mismatch after copy." >&2
        echo "  source:  $SOURCE_SHA" >&2
        echo "  staged:  $STAGED_SHA" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
    echo "Staged copy SHA verified: $STAGED_SHA"
else
    echo "[dry-run] would SHA256-verify $FACTORY_KERNEL_PATH matches $REVERT_FIRMWARE"
fi

echo "Step 3: Setting U-Boot bootcount=4 to trigger recovery on next reboot..."
# bootcount > 3 → U-Boot falls back to /config/factory_kernel.bin per
# .
if command -v fw_setenv >/dev/null 2>&1; then
    revert_run fw_setenv bootcount 4
elif [ "$REVERT_DRY_RUN" = "1" ]; then
    echo "[dry-run] fw_setenv bootcount 4"
else
    echo "ERROR: fw_setenv unavailable — cannot set bootcount trigger." >&2
    echo "       Recovery can still be invoked by power-cycling the unit 4+ times" >&2
    echo "       to drive bootcount > 3 organically." >&2
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

echo ""
echo "==========================================================="
echo "  CV1835 recovery staged."
echo "==========================================================="
echo ""
echo "  factory_kernel.bin staged at $FACTORY_KERNEL_PATH."
echo "  U-Boot bootcount=4. Next reboot enters recovery mode."
echo ""

if [ "$REVERT_AUTO_REBOOT" = "1" ] && [ "$REVERT_DRY_RUN" != "1" ]; then
    echo "Step 4: Rebooting into recovery (auto-reboot requested)..."
    revert_run sync
    revert_run reboot
elif [ "$REVERT_AUTO_REBOOT" = "1" ]; then
    echo "[dry-run] would sync; reboot"
else
    echo "  To enter recovery now, run:"
    echo "    reboot"
fi

if [ "$REVERT_DRY_RUN" = "1" ]; then
    revert_emit_manifest "$REVERT_FIRMWARE" "planned"
else
    revert_emit_manifest "$REVERT_FIRMWARE" "executed"
fi
