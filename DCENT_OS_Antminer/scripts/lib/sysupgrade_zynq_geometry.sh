#!/bin/sh
# Canonical Zynq UBI payload windows used by build-host validation and
# target-side sysupgrade containment.
#
# These are real-target compatibility limits, not values discovered from the
# current test kernel. In particular, nandsim commonly exposes a 129024-byte
# LEB for a 128-KiB PEB while the Xilinx NAND captured from the AM2 fleet uses
# a 126976-byte (124-KiB) LEB. An emulator-derived window is therefore not a
# release authority.
#
# Evidence for the AM2 production profile:
#
#
#
# The live .139 probe recorded a 23/179/210 volume plan and 126976-byte LEBs.
# AM2 S19 Pro and S17 Pro currently reuse the same conservative byte windows,
# but that does not promote their independent hardware-validation maturity.

DCENT_ZYNQ_GEOMETRY_SCHEMA=dcentos-zynq-ubi-geometry-v1
ZYNQ_UBI_LEB_SIZE_BYTES=126976

AM1_S9_KERNEL_PACKAGE_LEBS=32
AM1_S9_ROOTFS_PACKAGE_LEBS=134
AM2_ZYNQ_KERNEL_PACKAGE_LEBS=23
AM2_ZYNQ_ROOTFS_PACKAGE_LEBS=179

# The AM2 runtime layout observer accepts a kernel-volume deviation of four
# LEBs, but release payloads must still fit the 23-LEB stock/factory volume.
# Keep the larger number only as a pre-extraction resource-containment bound.
AM2_ZYNQ_KERNEL_LAYOUT_TOLERANCE_LEBS=4
ZYNQ_SYSUPGRADE_TAR_SLACK_BYTES=$((8 * 1024 * 1024))

AM1_S9_KERNEL_MAX_BYTES=$((AM1_S9_KERNEL_PACKAGE_LEBS * ZYNQ_UBI_LEB_SIZE_BYTES))
AM1_S9_ROOTFS_MAX_BYTES=$((AM1_S9_ROOTFS_PACKAGE_LEBS * ZYNQ_UBI_LEB_SIZE_BYTES))
AM2_ZYNQ_KERNEL_MAX_BYTES=$((AM2_ZYNQ_KERNEL_PACKAGE_LEBS * ZYNQ_UBI_LEB_SIZE_BYTES))
AM2_ZYNQ_KERNEL_TAR_BOUND_BYTES=$(((AM2_ZYNQ_KERNEL_PACKAGE_LEBS + AM2_ZYNQ_KERNEL_LAYOUT_TOLERANCE_LEBS) * ZYNQ_UBI_LEB_SIZE_BYTES))
AM2_ZYNQ_ROOTFS_MAX_BYTES=$((AM2_ZYNQ_ROOTFS_PACKAGE_LEBS * ZYNQ_UBI_LEB_SIZE_BYTES))

dcent_zynq_geometry_fail()
{
    printf '%s\n' "zynq-geometry: ERROR: $*" >&2
    return 1
}

dcent_zynq_geometry_canonical_uint()
{
    case "$1" in
        ''|*[!0-9]*|0[0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

# Select a package-compatibility profile. The returned variables are bounded
# byte capacities; they are not permission to flash a hardware target.
dcent_zynq_geometry_select()
{
    [ "$#" -eq 1 ] || {
        dcent_zynq_geometry_fail "profile selection requires one board identity"
        return 1
    }

    case "$1" in
        am1-s9)
            DCENT_ZYNQ_GEOMETRY_PROFILE=am1-s9
            DCENT_ZYNQ_GEOMETRY_MATURITY=production
            ZYNQ_KERNEL_MAX_BYTES=$AM1_S9_KERNEL_MAX_BYTES
            ZYNQ_ROOTFS_MAX_BYTES=$AM1_S9_ROOTFS_MAX_BYTES
            ;;
        am2-s19j|am2-s19jpro)
            DCENT_ZYNQ_GEOMETRY_PROFILE=am2-s19j
            DCENT_ZYNQ_GEOMETRY_MATURITY=production
            ZYNQ_KERNEL_MAX_BYTES=$AM2_ZYNQ_KERNEL_MAX_BYTES
            ZYNQ_ROOTFS_MAX_BYTES=$AM2_ZYNQ_ROOTFS_MAX_BYTES
            ;;
        am2-s19pro)
            DCENT_ZYNQ_GEOMETRY_PROFILE=am2-s19pro
            DCENT_ZYNQ_GEOMETRY_MATURITY=experimental
            ZYNQ_KERNEL_MAX_BYTES=$AM2_ZYNQ_KERNEL_MAX_BYTES
            ZYNQ_ROOTFS_MAX_BYTES=$AM2_ZYNQ_ROOTFS_MAX_BYTES
            ;;
        am2-s17p|am2-s17pro)
            DCENT_ZYNQ_GEOMETRY_PROFILE=am2-s17p
            DCENT_ZYNQ_GEOMETRY_MATURITY=experimental
            ZYNQ_KERNEL_MAX_BYTES=$AM2_ZYNQ_KERNEL_MAX_BYTES
            ZYNQ_ROOTFS_MAX_BYTES=$AM2_ZYNQ_ROOTFS_MAX_BYTES
            ;;
        *)
            dcent_zynq_geometry_fail "unsupported Zynq board identity: $1"
            return 1
            ;;
    esac
}

dcent_zynq_geometry_payload_ceiling()
{
    [ "$#" -eq 2 ] || {
        dcent_zynq_geometry_fail "payload-ceiling lookup requires board and payload kind"
        return 1
    }
    dcent_zynq_geometry_select "$1" || return 1
    case "$2" in
        kernel) printf '%s\n' "$ZYNQ_KERNEL_MAX_BYTES" ;;
        rootfs|root) printf '%s\n' "$ZYNQ_ROOTFS_MAX_BYTES" ;;
        *)
            dcent_zynq_geometry_fail "unsupported payload kind: $2"
            return 1
            ;;
    esac
}

# Bound the archive before extraction. AM2 retains four extra kernel LEBs only
# as scratch/layout tolerance; extracted kernel bytes must still satisfy the
# stricter package ceiling through dcent_zynq_geometry_require_payload_fit.
dcent_zynq_geometry_tar_preextract_ceiling()
{
    [ "$#" -eq 1 ] || {
        dcent_zynq_geometry_fail "tar-ceiling lookup requires one board identity"
        return 1
    }
    dcent_zynq_geometry_select "$1" || return 1
    case "$DCENT_ZYNQ_GEOMETRY_PROFILE" in
        am1-s9)
            printf '%s\n' "$((AM1_S9_KERNEL_MAX_BYTES + AM1_S9_ROOTFS_MAX_BYTES + ZYNQ_SYSUPGRADE_TAR_SLACK_BYTES))"
            ;;
        am2-*)
            printf '%s\n' "$((AM2_ZYNQ_KERNEL_TAR_BOUND_BYTES + AM2_ZYNQ_ROOTFS_MAX_BYTES + ZYNQ_SYSUPGRADE_TAR_SLACK_BYTES))"
            ;;
        *)
            dcent_zynq_geometry_fail "profile has no tar ceiling: $DCENT_ZYNQ_GEOMETRY_PROFILE"
            return 1
            ;;
    esac
}

dcent_zynq_geometry_require_payload_fit()
{
    [ "$#" -eq 3 ] || {
        dcent_zynq_geometry_fail "payload-fit proof requires board, payload kind, and byte size"
        return 1
    }
    _dcent_zynq_geometry_board=$1
    _dcent_zynq_geometry_kind=$2
    _dcent_zynq_geometry_size=$3

    dcent_zynq_geometry_canonical_uint "$_dcent_zynq_geometry_size" &&
        [ "$_dcent_zynq_geometry_size" -gt 0 ] || {
        dcent_zynq_geometry_fail "payload size is not canonical decimal: $_dcent_zynq_geometry_size"
        return 1
    }
    _dcent_zynq_geometry_ceiling=$(dcent_zynq_geometry_payload_ceiling \
        "$_dcent_zynq_geometry_board" "$_dcent_zynq_geometry_kind") || return 1
    [ "$_dcent_zynq_geometry_size" -le "$_dcent_zynq_geometry_ceiling" ] || {
        dcent_zynq_geometry_fail \
            "$_dcent_zynq_geometry_board $_dcent_zynq_geometry_kind payload exceeds the real-target window ($_dcent_zynq_geometry_size bytes > $_dcent_zynq_geometry_ceiling bytes)"
        return 1
    }
}

dcent_zynq_geometry_receipt()
{
    [ "$#" -eq 1 ] || {
        dcent_zynq_geometry_fail "receipt generation requires one board identity"
        return 1
    }
    dcent_zynq_geometry_select "$1" || return 1
    printf '%s\n' \
        "$DCENT_ZYNQ_GEOMETRY_SCHEMA|profile=$DCENT_ZYNQ_GEOMETRY_PROFILE|maturity=$DCENT_ZYNQ_GEOMETRY_MATURITY|leb-size=$ZYNQ_UBI_LEB_SIZE_BYTES|kernel-max=$ZYNQ_KERNEL_MAX_BYTES|rootfs-max=$ZYNQ_ROOTFS_MAX_BYTES"
}
