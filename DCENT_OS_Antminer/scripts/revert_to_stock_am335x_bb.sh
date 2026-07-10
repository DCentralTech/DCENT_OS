#!/bin/sh
#
# revert_to_stock_am335x_bb.sh - AM335x BB stock revert status gate.
#
# The AM335x BB S19j Pro port is SD-card management/bring-up only. The exact
# stock NAND slot map is not checked in with enough evidence to safely write an
# inactive slot, so this script intentionally has no destructive code path.

set -eu

echo "ERROR: AM335x BB NAND revert is disabled." >&2
echo "Live /proc/mtd evidence plus a checked-in exact layout profile are still missing." >&2
echo "Keep using the am3-bb SD-card management/bring-up path until that evidence exists." >&2
echo "Future enablement requires DCENT_AM3_BB_PROC_MTD_EVIDENCE and a reviewed profile with verified_revertable=true." >&2
echo "DCENT_AM3_BB_ENABLE_NAND_REVERT is not accepted as a bypass." >&2
exit 1
