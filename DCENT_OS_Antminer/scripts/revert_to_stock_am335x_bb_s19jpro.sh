#!/bin/sh
#
# revert_to_stock_am335x_bb_s19jpro.sh - AM335x BB S19j Pro stock revert
# status gate (variant of revert_to_stock_am335x_bb.sh).
#
# Provenance: cloned from
# `DCENT_OS_Antminer/scripts/revert_to_stock_am335x_bb.sh`. Wave
# reference: AGENT B3 wave W10.x (2026-05-09).
#
# Diffs vs the am3-bb base revert script:
#   1. Hardcoded product family string is "am3-bb-s19jpro" (vs "am3-bb").
#   2. Future enablement also gates on the BHB42xxx hashboard subtype
#      preflight (vs the am3-bb base which is hashboard-agnostic).
#
# The AM335x BB S19j Pro port is SD-card management/bring-up only. The
# exact stock NAND slot map is not checked in with enough evidence to
# safely write an inactive slot, so this script intentionally has no
# destructive code path. It refuses NAND writes until dated live
# /proc/mtd evidence is supplied AND a BHB42xxx hashboard subtype is
# confirmed by preflight.

set -eu

PRODUCT_FAMILY="am3-bb-s19jpro"
EXPECTED_HASHBOARD_SUBTYPE="BHB42XXX"

echo "ERROR: AM335x BB S19j Pro NAND revert is disabled." >&2
echo "  Product family:           $PRODUCT_FAMILY" >&2
echo "  Expected hashboard:       $EXPECTED_HASHBOARD_SUBTYPE (S19j Pro BB carrier)" >&2
echo "" >&2
echo "Live /proc/mtd evidence plus a checked-in exact layout profile are still missing." >&2
echo "Keep using the am3-bb-s19jpro SD-card management/bring-up path until that evidence exists." >&2
echo "Future enablement requires:" >&2
echo "  * DCENT_AM3_BB_PROC_MTD_EVIDENCE      (dated /proc/mtd capture from this carrier)" >&2
echo "  * a reviewed silicon profile with verified_revertable=true" >&2
echo "  * a BHB42xxx hashboard subtype preflight pass" >&2
echo "DCENT_AM3_BB_ENABLE_NAND_REVERT is not accepted as a bypass." >&2
exit 1
