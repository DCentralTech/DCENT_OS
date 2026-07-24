#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
. "$SCRIPT_DIR/lib/zynq_nandsim_geometry.sh"
. "$SCRIPT_DIR/lib/sysupgrade_zynq_geometry.sh"

tests=0

check()
{
    tests=$((tests + 1))
    "$@" || {
        printf 'not ok %s: %s\n' "$tests" "$*" >&2
        exit 1
    }
}

check [ "$DCENT_ZYNQ_NANDSIM_PROFILE_SCHEMA" = \
    dcentos-zynq-nandsim-geometry-v1 ]
check dcent_zynq_nandsim_profile_select am2-s19jpro
check [ "$DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE" = am2-live-derived ]
check [ "$DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY" = hardware-evidenced ]
check [ "$DCENT_ZYNQ_NANDSIM_PARTS" = 64,96,16,16,4,4,696,456,456 ]
check [ "$DCENT_ZYNQ_NANDSIM_VID_OFFSET" = 2048 ]
check [ "$DCENT_ZYNQ_NANDSIM_BEB_LIMIT" = 20 ]
check [ "$DCENT_ZYNQ_NANDSIM_SLOT_MTD_SIZE_HEX" = 03900000 ]
check [ "$((0x$DCENT_ZYNQ_NANDSIM_SLOT_MTD_SIZE_HEX))" = \
    "$((DCENT_ZYNQ_NANDSIM_TOTAL_PEBS * DCENT_ZYNQ_NANDSIM_PEB_BYTES))" ]
check [ "$DCENT_ZYNQ_NANDSIM_LEB_BYTES" = "$ZYNQ_UBI_LEB_SIZE_BYTES" ]
check [ "$AM2_ZYNQ_KERNEL_PACKAGE_LEBS" = 23 ]
check [ "$AM2_ZYNQ_ROOTFS_PACKAGE_LEBS" = 179 ]
check dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 412
check [ "$(dcent_zynq_nandsim_expected_tuple)" = \
    'peb=131072 leb=126976 total=456 reserved_for_bad=40 bad=0 available=412' ]
if dcent_zynq_nandsim_expected_tuple unexpected; then exit 1; fi
tests=$((tests + 1))

# Every historically misleading or fastmap-inflated tuple must fail.
if dcent_zynq_nandsim_attached_tuple_matches 131072 129024 456 40 0 412; then exit 1; fi
tests=$((tests + 1))
if dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 410; then exit 1; fi
tests=$((tests + 1))
if dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 38 0 412; then exit 1; fi
tests=$((tests + 1))
if dcent_zynq_nandsim_attached_tuple_matches 131072 126976 455 39 1 412; then exit 1; fi
tests=$((tests + 1))

check dcent_zynq_nandsim_profile_select am2-s17pro
check [ "$DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY" = inherited-experimental ]
check dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 412
check dcent_zynq_nandsim_profile_select am2-s19pro
check [ "$DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY" = inherited-experimental ]
check dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 412

check dcent_zynq_nandsim_profile_select am1-s9
check [ "$DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE" = functional-only ]
check [ "$DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY" = functional-only ]
set +e
dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 412
status=$?
set -e
check [ "$status" = 2 ]

check dcent_zynq_nandsim_profile_select am2-s19jpro
if dcent_zynq_nandsim_profile_select; then exit 1; fi
tests=$((tests + 1))
check [ -z "${DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE:-}" ]

if dcent_zynq_nandsim_profile_select unknown; then exit 1; fi
tests=$((tests + 1))
check [ -z "${DCENT_ZYNQ_NANDSIM_PROFILE_PROVENANCE:-}" ]
check [ -z "${DCENT_ZYNQ_NANDSIM_HARDWARE_MATURITY:-}" ]
set +e
dcent_zynq_nandsim_attached_tuple_matches 131072 126976 456 40 0 412
status=$?
set -e
check [ "$status" = 2 ]
if dcent_zynq_nandsim_attached_tuple_matches 1 2 3 4 5; then exit 1; fi
tests=$((tests + 1))

printf 'Zynq nandsim geometry contract: %s assertions\n' "$tests"
