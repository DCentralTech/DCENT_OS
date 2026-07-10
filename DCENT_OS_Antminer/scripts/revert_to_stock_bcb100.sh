#!/bin/sh
#
# revert_to_stock_bcb100.sh — Publish the Braiins BCB100 stock revert
# recipe to the operator. THIS SCRIPT DOES NOT EXECUTE THE REVERT.
#
# ============================================================================
# LOAD-BEARING SAFETY CONTRACT (Phase 4C, 2026-05-15 )
# ============================================================================
#
# BCB100 (STM32MP157 control board for the BCB100 open-hardware project) is
# a lab-only scaffold.
# There is NO bench unit on the DCENT_OS fleet. Until a unit lands, the only
# documented stock-revert path is the upstream Braiins `dd if=stock.img
# of=/dev/mmcblk0` recipe. This script publishes that recipe (with safety
# preconditions) and waits for operator acknowledgment.
#
# Even though no destructive primitive is invoked, the contract is
# preserved for consistency with the other 4 family revert scripts:
#
#   * `--operator-acknowledged-data-loss` REQUIRED to print the live
#     recipe + emit an "executed=published" JSON manifest.
#   * `DCENT_BCB100_REVERT_AUTHORIZED=1` env-gate REQUIRED.
#   * `--dry-run` prints a redacted recipe and emits a "planned" manifest.
#
# When a BCB100 unit lands on the bench, this script gets retired in favor
# of a real eMMC writer (mirroring the cv1835 recovery-staging pattern).
#
# ============================================================================
# EXPECTED FLEET STATE
# ============================================================================
#
# Before:    No BCB100 unit on the fleet. Script is documentation-only.
# After:     Operator has the verified Braiins recipe printed locally and
#            has acknowledged the destructive nature of the dd path.
#            Operator must run the dd on the bench unit via the STM32MP15
#            U-Boot fastboot / SD card recovery path documented by Braiins.
#
# Recovery
# if revert
# fails:     N/A — this script does not write anything.
#
# Usage:
#   DCENT_BCB100_REVERT_AUTHORIZED=1 ./revert_to_stock_bcb100.sh \
#       --operator-acknowledged-data-loss \
#       --firmware /path/to/braiins-bcb100-stock.img \
#       --target <bcb100-bench-ip-or-serial>
#
#   ./revert_to_stock_bcb100.sh --dry-run --firmware ./braiins-bcb100-stock.img

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
if [ ! -r "$SCRIPT_DIR/lib/revert_common.sh" ]; then
    echo "ERROR: missing shared revert helper: $SCRIPT_DIR/lib/revert_common.sh" >&2
    exit 1
fi
. "$SCRIPT_DIR/lib/revert_common.sh"

revert_init "bcb100" "DCENT_BCB100_REVERT_AUTHORIZED"

PRODUCT_FAMILY="bcb100-stm32mp157"
EMMC_DEVICE="/dev/mmcblk0"
STOCK_DEFAULT_BLOCK="4096"

print_help() {
    cat <<EOF
Usage: $0 [options]

  --operator-acknowledged-data-loss  Required to print the live recipe.
  --dry-run                          Print redacted recipe (no operator ack
                                     required, no env-gate required).
  --target <ip-or-serial>            Target BCB100 bench unit (informational).
  --firmware <path>                  Braiins BCB100 stock .img file.
  --help                             This help.

Product family: $PRODUCT_FAMILY
Env-gate (REQUIRED for the live recipe): DCENT_BCB100_REVERT_AUTHORIZED=1

NOTE: This script DOES NOT execute the dd. It publishes the recipe to the
      operator for manual execution on the bench unit. No DCENT_OS state on
      the operator's host is modified.
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

if [ -z "$REVERT_FIRMWARE" ]; then
    echo "ERROR: --firmware <braiins-bcb100-stock.img> is required." >&2
    print_help >&2
    revert_emit_manifest "" "refused"
    exit 1
fi

if ! revert_check_authorization; then
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 1
fi

echo "==========================================================="
echo "  BCB100 Stock Revert Recipe (DOCUMENTATION-ONLY)"
echo "  family: $REVERT_FAMILY   product: $PRODUCT_FAMILY"
echo "  mode:   $([ "$REVERT_DRY_RUN" = "1" ] && echo dry-run || echo live)"
echo "==========================================================="
echo ""

if [ -f "$REVERT_FIRMWARE" ]; then
    fw_sha=$(revert_sha256 "$REVERT_FIRMWARE")
    fw_size=$(ls -lh "$REVERT_FIRMWARE" 2>/dev/null | awk '{print $5}')
    echo "Firmware: $REVERT_FIRMWARE"
    echo "  size:   $fw_size"
    echo "  sha256: $fw_sha"
else
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] firmware $REVERT_FIRMWARE missing on host — recipe still printable"
    else
        echo "ERROR: firmware image not found at: $REVERT_FIRMWARE" >&2
        revert_emit_manifest "$REVERT_FIRMWARE" "refused"
        exit 1
    fi
fi

echo ""
echo "Target unit:   ${REVERT_TARGET:-<unspecified>}"
echo "Target device: $EMMC_DEVICE"
echo ""

cat <<'RECIPE'
============================================================================
  Braiins BCB100 stock recovery — manual dd recipe
============================================================================

Preconditions (operator MUST verify before invoking the dd):

  1. The BCB100 bench unit is reachable via the STM32MP15 U-Boot console
     (UART debug header, typical baud 115200 8N1).
  2. The unit is booted into U-Boot fastboot OR an SD card recovery rootfs
     that has the stock .img file resident at a known path (e.g.
     /run/initramfs/braiins-bcb100-stock.img).
  3. eMMC is NOT mounted by the recovery rootfs (`mount | grep mmcblk0`
     returns nothing).
  4. The BCB100 is powered from a bench supply (not the ASIC PSU) so the
     hashboards are NOT energized during the flash.

dd command (run ON the unit, NOT from this host):

    dd if=braiins-bcb100-stock.img of=/dev/mmcblk0 bs=4M conv=fsync status=progress

Post-flash verification (operator runs after dd completes, BEFORE reboot):

    sync
    sha256sum /dev/mmcblk0 | head -c 32   # confirm first 16 MiB hash matches expected
    fdisk -l /dev/mmcblk0                 # confirm partition table is sane

Reboot procedure:

    Power-cycle the unit (NOT `reboot` — U-Boot expects a cold boot to pick
    up the freshly-written partition table).

If the bench unit fails to come up after dd:

    - Connect the STM32MP15 UART debug header at 115200 8N1.
    - Boot the unit with the boot-select pins in SD-recovery mode (per
      ST AN5677 §4.2 — STM32MP15 BootROM SD-SPL fallback).
    - Re-stage braiins-bcb100-stock.img on the SD card.
    - Re-run the dd from the recovery rootfs.

============================================================================
  Do NOT run the dd above on any other device. /dev/mmcblk0 may also be
  the eMMC of an unrelated control board if the operator is on the wrong
  unit. DCENT_OS takes no responsibility for cross-unit dd.
============================================================================
RECIPE

echo ""
if [ "$REVERT_DRY_RUN" = "1" ]; then
    echo "(dry-run) recipe published; no operator ack consumed."
    revert_emit_manifest "$REVERT_FIRMWARE" "planned"
    exit 0
fi

printf "Type 'ACKNOWLEDGE-BCB100-RECIPE' to confirm receipt: "
read CONFIRM
if [ "$CONFIRM" != "ACKNOWLEDGE-BCB100-RECIPE" ]; then
    echo "Aborted — recipe receipt not acknowledged."
    revert_emit_manifest "$REVERT_FIRMWARE" "refused"
    exit 0
fi

echo ""
echo "Recipe acknowledged. JSON manifest captured for the audit trail."
echo ""
revert_emit_manifest "$REVERT_FIRMWARE" "published"
