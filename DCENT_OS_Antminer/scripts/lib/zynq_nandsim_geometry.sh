#!/bin/sh
# Declarative geometry profiles for the offline Zynq nandsim harness.
#
# The AM2 tuple is derived from retained physical-board evidence.  Matching it
# in nandsim proves only emulator configuration; the emulator is never release
# or physical-NAND authority.  The capability and S9 profiles intentionally
# make no whole-device geometry claim.

DCENT_ZYNQ_NANDSIM_PROFILE_SCHEMA=dcentos-zynq-nandsim-geometry-v1

dcent_zynq_nandsim_profile_clear()
{
    DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE=
    DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY=
    DCENT_ZYNQ_NANDSIM_PARTS=
    DCENT_ZYNQ_NANDSIM_VID_OFFSET=
    DCENT_ZYNQ_NANDSIM_BEB_LIMIT=
    DCENT_ZYNQ_NANDSIM_SLOT_MTD_SIZE_HEX=
    DCENT_ZYNQ_NANDSIM_PEB_BYTES=
    DCENT_ZYNQ_NANDSIM_LEB_BYTES=
    DCENT_ZYNQ_NANDSIM_TOTAL_PEBS=
    DCENT_ZYNQ_NANDSIM_RESERVED_FOR_BAD=
    DCENT_ZYNQ_NANDSIM_BAD_PEBS=
    DCENT_ZYNQ_NANDSIM_AVAILABLE_PEBS=
}

dcent_zynq_nandsim_select_am2_tuple()
{
    DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE=am2-live-derived
    DCENT_ZYNQ_NANDSIM_PARTS=64,96,16,16,4,4,696,456,456
    DCENT_ZYNQ_NANDSIM_VID_OFFSET=2048
    DCENT_ZYNQ_NANDSIM_BEB_LIMIT=20
    DCENT_ZYNQ_NANDSIM_SLOT_MTD_SIZE_HEX=03900000
    DCENT_ZYNQ_NANDSIM_PEB_BYTES=131072
    DCENT_ZYNQ_NANDSIM_LEB_BYTES=126976
    DCENT_ZYNQ_NANDSIM_TOTAL_PEBS=456
    DCENT_ZYNQ_NANDSIM_RESERVED_FOR_BAD=40
    DCENT_ZYNQ_NANDSIM_BAD_PEBS=0
    DCENT_ZYNQ_NANDSIM_AVAILABLE_PEBS=412
}

dcent_zynq_nandsim_profile_select()
{
    dcent_zynq_nandsim_profile_clear
    [ "$#" -eq 1 ] || return 1

    case "$1" in
        am2-s19jpro)
            dcent_zynq_nandsim_select_am2_tuple
            DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY=hardware-evidenced
            ;;
        am2-s17pro|am2-s19pro)
            dcent_zynq_nandsim_select_am2_tuple
            DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY=inherited-experimental
            ;;
        am1-s9|capability-only)
            DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE=functional-only
            DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY=functional-only
            DCENT_ZYNQ_NANDSIM_PARTS=1,1,1,1,4,1,1,900,900
            DCENT_ZYNQ_NANDSIM_VID_OFFSET=-
            DCENT_ZYNQ_NANDSIM_BEB_LIMIT=-
            DCENT_ZYNQ_NANDSIM_SLOT_MTD_SIZE_HEX=-
            DCENT_ZYNQ_NANDSIM_PEB_BYTES=-
            DCENT_ZYNQ_NANDSIM_LEB_BYTES=-
            DCENT_ZYNQ_NANDSIM_TOTAL_PEBS=-
            DCENT_ZYNQ_NANDSIM_RESERVED_FOR_BAD=-
            DCENT_ZYNQ_NANDSIM_BAD_PEBS=-
            DCENT_ZYNQ_NANDSIM_AVAILABLE_PEBS=-
            ;;
        *) return 1 ;;
    esac
}

# Return 0 only for the complete AM2 evidence-derived tuple.  Return 2 when the
# selected profile is deliberately functional-only and 1 for a mismatch.
dcent_zynq_nandsim_attached_tuple_matches()
{
    [ "$#" -eq 6 ] || return 1
    [ "${DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE:-}" = am2-live-derived ] || return 2

    [ "$1" = "$DCENT_ZYNQ_NANDSIM_PEB_BYTES" ] &&
        [ "$2" = "$DCENT_ZYNQ_NANDSIM_LEB_BYTES" ] &&
        [ "$3" = "$DCENT_ZYNQ_NANDSIM_TOTAL_PEBS" ] &&
        [ "$4" = "$DCENT_ZYNQ_NANDSIM_RESERVED_FOR_BAD" ] &&
        [ "$5" = "$DCENT_ZYNQ_NANDSIM_BAD_PEBS" ] &&
        [ "$6" = "$DCENT_ZYNQ_NANDSIM_AVAILABLE_PEBS" ]
}

dcent_zynq_nandsim_expected_tuple()
{
    [ "$#" -eq 0 ] || return 1
    [ "${DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE:-}" = am2-live-derived ] || return 1
    printf 'peb=%s leb=%s total=%s reserved_for_bad=%s bad=%s available=%s\n' \
        "$DCENT_ZYNQ_NANDSIM_PEB_BYTES" \
        "$DCENT_ZYNQ_NANDSIM_LEB_BYTES" \
        "$DCENT_ZYNQ_NANDSIM_TOTAL_PEBS" \
        "$DCENT_ZYNQ_NANDSIM_RESERVED_FOR_BAD" \
        "$DCENT_ZYNQ_NANDSIM_BAD_PEBS" \
        "$DCENT_ZYNQ_NANDSIM_AVAILABLE_PEBS"
}
