#!/bin/sh
# Observe a UBI volume-creation plan without mutating flash or device nodes.
#
# Public API:
#   dcent_ubi_volume_plan_admit SYSFS_UBI_ROOT UBI_NUM EXPECTED_MTD PREFIX
#   dcent_ubi_volume_plan_revalidate \
#       SYSFS_UBI_ROOT UBI_NUM EXPECTED_MTD PREFIX PRIOR_RECEIPT
#
# PREFIX is the exact number of volumes already present.  The only admitted
# states are prefixes of this declarative AM2 plan:
#   0: no volumes
#   1: ID 0 = kernel      (dynamic)
#   2: ID 1 = rootfs      (dynamic)
#   3: ID 2 = rootfs_data (dynamic)
#
# Admission prints a versioned, normalized receipt.  Revalidation rejects if
# the observed semantic prefix differs from that receipt.  Receipts establish
# observation only; they do not establish mutation ownership or authorize a
# subsequent provisioning command.

DCENT_UBI_VOLUME_PLAN_ATTR_MAX_BYTES=4096

dcent_ubi_volume_plan_fail()
{
    printf '%s\n' "ubi-volume-plan: ERROR: $*" >&2
    return 1
}

dcent_ubi_volume_plan_canonical_uint()
{
    case "$1" in
        ''|*[!0-9]*|0[0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

dcent_ubi_volume_plan_prepare_root()
{
    _dcent_ubi_volume_plan_root=$1
    case "$_dcent_ubi_volume_plan_root" in
        /*) ;;
        *)
            dcent_ubi_volume_plan_fail \
                "sysfs UBI root must be an absolute path"
            return 1
            ;;
    esac
    [ -d "$_dcent_ubi_volume_plan_root" ] && \
        [ ! -L "$_dcent_ubi_volume_plan_root" ] || {
        dcent_ubi_volume_plan_fail \
            "sysfs UBI root must be a non-symlink directory"
        return 1
    }
    _dcent_ubi_volume_plan_root_real=$(CDPATH='' cd -P \
        "$_dcent_ubi_volume_plan_root" 2>/dev/null && pwd -P) || {
        dcent_ubi_volume_plan_fail "cannot resolve sysfs UBI root"
        return 1
    }
    [ "$_dcent_ubi_volume_plan_root_real" = \
        "$_dcent_ubi_volume_plan_root" ] || {
        dcent_ubi_volume_plan_fail \
            "sysfs UBI root contains a symlink or non-canonical component"
        return 1
    }
    [ "$_dcent_ubi_volume_plan_root_real" != / ] || {
        dcent_ubi_volume_plan_fail \
            "refusing filesystem root as sysfs UBI root"
        return 1
    }
}

dcent_ubi_volume_plan_read_attr()
{
    _dcent_ubi_volume_plan_attr=$1
    _dcent_ubi_volume_plan_label=$2

    [ -r "$_dcent_ubi_volume_plan_attr" ] && \
        [ -f "$_dcent_ubi_volume_plan_attr" ] && \
        [ ! -L "$_dcent_ubi_volume_plan_attr" ] || {
        dcent_ubi_volume_plan_fail \
            "$_dcent_ubi_volume_plan_label is not a readable non-symlink attribute"
        return 1
    }
    _dcent_ubi_volume_plan_bytes=$(wc -c \
        <"$_dcent_ubi_volume_plan_attr" 2>/dev/null | tr -d '[:space:]') || {
        dcent_ubi_volume_plan_fail \
            "cannot size $_dcent_ubi_volume_plan_label"
        return 1
    }
    dcent_ubi_volume_plan_canonical_uint \
        "$_dcent_ubi_volume_plan_bytes" &&
        [ "$_dcent_ubi_volume_plan_bytes" != 0 ] &&
        [ "${#_dcent_ubi_volume_plan_bytes}" -le 4 ] &&
        [ "$_dcent_ubi_volume_plan_bytes" -le \
            "$DCENT_UBI_VOLUME_PLAN_ATTR_MAX_BYTES" ] || {
        dcent_ubi_volume_plan_fail \
            "$_dcent_ubi_volume_plan_label exceeds the attribute byte ceiling"
        return 1
    }
    _dcent_ubi_volume_plan_lines=$(wc -l \
        <"$_dcent_ubi_volume_plan_attr" 2>/dev/null | tr -d '[:space:]') || {
        dcent_ubi_volume_plan_fail \
            "cannot count lines in $_dcent_ubi_volume_plan_label"
        return 1
    }
    [ "$_dcent_ubi_volume_plan_lines" = 1 ] || {
        dcent_ubi_volume_plan_fail \
            "$_dcent_ubi_volume_plan_label must contain exactly one line"
        return 1
    }
    _dcent_ubi_volume_plan_value=$(cat \
        "$_dcent_ubi_volume_plan_attr" 2>/dev/null) || {
        dcent_ubi_volume_plan_fail \
            "cannot read $_dcent_ubi_volume_plan_label"
        return 1
    }
    [ -n "$_dcent_ubi_volume_plan_value" ] || {
        dcent_ubi_volume_plan_fail \
            "$_dcent_ubi_volume_plan_label is empty"
        return 1
    }
    case "$_dcent_ubi_volume_plan_value" in
        *'
'*)
            dcent_ubi_volume_plan_fail \
                "$_dcent_ubi_volume_plan_label contains embedded newlines"
            return 1
            ;;
    esac
    printf '%s\n' "$_dcent_ubi_volume_plan_value"
}

dcent_ubi_volume_plan_resolve_device()
{
    _dcent_ubi_volume_plan_resolve_root=$1
    _dcent_ubi_volume_plan_resolve_path=$2
    _dcent_ubi_volume_plan_resolve_name=$3
    _dcent_ubi_volume_plan_resolve_mtd=$4

    [ -d "$_dcent_ubi_volume_plan_resolve_path" ] || {
        dcent_ubi_volume_plan_fail \
            "target UBI device class entry is not a directory"
        return 1
    }
    if [ ! -L "$_dcent_ubi_volume_plan_resolve_path" ]; then
        # A direct directory is supported for mount-free test fixtures.  Real
        # Linux class entries use the canonical symlink branch below.
        printf '%s\n' "$_dcent_ubi_volume_plan_resolve_path"
        return 0
    fi

    case "$_dcent_ubi_volume_plan_resolve_root" in
        */class/ubi)
            _dcent_ubi_volume_plan_sysfs_mount=\
${_dcent_ubi_volume_plan_resolve_root%/class/ubi}
            ;;
        *)
            dcent_ubi_volume_plan_fail \
                "cannot validate a class symlink outside SYSFS/class/ubi"
            return 1
            ;;
    esac
    _dcent_ubi_volume_plan_resolved=$(CDPATH='' cd -P \
        "$_dcent_ubi_volume_plan_resolve_path" 2>/dev/null && pwd -P) || {
        dcent_ubi_volume_plan_fail \
            "cannot resolve target UBI device class entry"
        return 1
    }
    case "$_dcent_ubi_volume_plan_resolved" in
        "$_dcent_ubi_volume_plan_sysfs_mount"/devices/*) ;;
        *)
            dcent_ubi_volume_plan_fail \
                "target UBI device class symlink escapes the sysfs devices tree"
            return 1
            ;;
    esac
    [ "${_dcent_ubi_volume_plan_resolved##*/}" = \
        "$_dcent_ubi_volume_plan_resolve_name" ] || {
        dcent_ubi_volume_plan_fail \
            "target UBI device class symlink has the wrong basename"
        return 1
    }
    _dcent_ubi_volume_plan_resolved_parent=\
${_dcent_ubi_volume_plan_resolved%/*}
    case "$_dcent_ubi_volume_plan_resolved_parent" in
        "$_dcent_ubi_volume_plan_sysfs_mount"/devices/virtual/ubi)
            # Linux 4.4 registers ubiN as a parentless class device, which
            # resolves below devices/virtual/ubi.  mtd_num remains the
            # authoritative attachment identity in that legacy topology.
            ;;
        */mtd"$_dcent_ubi_volume_plan_resolve_mtd")
            # Current Linux parents ubiN below the attached MTD device.
            ;;
        *)
            dcent_ubi_volume_plan_fail \
                "target UBI device class symlink has neither the legacy UBI parent nor expected mtd$_dcent_ubi_volume_plan_resolve_mtd parent"
            return 1
            ;;
    esac
    printf '%s\n' "$_dcent_ubi_volume_plan_resolved"
}

dcent_ubi_volume_plan_resolve_volume()
{
    _dcent_ubi_volume_plan_resolve_root=$1
    _dcent_ubi_volume_plan_resolve_path=$2
    _dcent_ubi_volume_plan_resolve_name=$3
    _dcent_ubi_volume_plan_resolve_parent=$4

    [ -d "$_dcent_ubi_volume_plan_resolve_path" ] || {
        dcent_ubi_volume_plan_fail \
            "target UBI volume class entry is not a directory"
        return 1
    }
    if [ ! -L "$_dcent_ubi_volume_plan_resolve_path" ]; then
        printf '%s\n' "$_dcent_ubi_volume_plan_resolve_path"
        return 0
    fi

    case "$_dcent_ubi_volume_plan_resolve_root" in
        */class/ubi)
            _dcent_ubi_volume_plan_sysfs_mount=\
${_dcent_ubi_volume_plan_resolve_root%/class/ubi}
            ;;
        *)
            dcent_ubi_volume_plan_fail \
                "cannot validate a volume class symlink outside SYSFS/class/ubi"
            return 1
            ;;
    esac
    _dcent_ubi_volume_plan_resolved=$(CDPATH='' cd -P \
        "$_dcent_ubi_volume_plan_resolve_path" 2>/dev/null && pwd -P) || {
        dcent_ubi_volume_plan_fail \
            "cannot resolve target UBI volume class entry"
        return 1
    }
    case "$_dcent_ubi_volume_plan_resolved" in
        "$_dcent_ubi_volume_plan_sysfs_mount"/devices/*) ;;
        *)
            dcent_ubi_volume_plan_fail \
                "target UBI volume class symlink escapes the sysfs devices tree"
            return 1
            ;;
    esac
    [ "${_dcent_ubi_volume_plan_resolved##*/}" = \
        "$_dcent_ubi_volume_plan_resolve_name" ] || {
        dcent_ubi_volume_plan_fail \
            "target UBI volume class symlink has the wrong basename"
        return 1
    }
    [ "${_dcent_ubi_volume_plan_resolved%/*}" = \
        "$_dcent_ubi_volume_plan_resolve_parent" ] || {
        dcent_ubi_volume_plan_fail \
            "target UBI volume class symlink is not parented by its admitted UBI device"
        return 1
    }
    printf '%s\n' "$_dcent_ubi_volume_plan_resolved"
}

dcent_ubi_volume_plan_check_entry()
{
    _dcent_ubi_volume_plan_check_root=$1
    _dcent_ubi_volume_plan_check_num=$2
    _dcent_ubi_volume_plan_check_id=$3
    _dcent_ubi_volume_plan_check_name=$4
    _dcent_ubi_volume_plan_check_parent=$5
    _dcent_ubi_volume_plan_check_path=\
$_dcent_ubi_volume_plan_check_root/ubi${_dcent_ubi_volume_plan_check_num}_${_dcent_ubi_volume_plan_check_id}

    _dcent_ubi_volume_plan_check_resolved=$(dcent_ubi_volume_plan_resolve_volume \
        "$_dcent_ubi_volume_plan_check_root" \
        "$_dcent_ubi_volume_plan_check_path" \
        "ubi${_dcent_ubi_volume_plan_check_num}_${_dcent_ubi_volume_plan_check_id}" \
        "$_dcent_ubi_volume_plan_check_parent") || return 1

    _dcent_ubi_volume_plan_observed_name=$(dcent_ubi_volume_plan_read_attr \
        "$_dcent_ubi_volume_plan_check_resolved/name" \
        "ubi${_dcent_ubi_volume_plan_check_num}_${_dcent_ubi_volume_plan_check_id}/name") || return 1
    [ "$_dcent_ubi_volume_plan_observed_name" = \
        "$_dcent_ubi_volume_plan_check_name" ] || {
        dcent_ubi_volume_plan_fail \
            "volume ID $_dcent_ubi_volume_plan_check_id is named '$_dcent_ubi_volume_plan_observed_name', expected '$_dcent_ubi_volume_plan_check_name'"
        return 1
    }

    _dcent_ubi_volume_plan_observed_type=$(dcent_ubi_volume_plan_read_attr \
        "$_dcent_ubi_volume_plan_check_resolved/type" \
        "ubi${_dcent_ubi_volume_plan_check_num}_${_dcent_ubi_volume_plan_check_id}/type") || return 1
    [ "$_dcent_ubi_volume_plan_observed_type" = dynamic ] || {
        dcent_ubi_volume_plan_fail \
            "volume ID $_dcent_ubi_volume_plan_check_id has type '$_dcent_ubi_volume_plan_observed_type', expected 'dynamic'"
        return 1
    }

    printf '%s\n' \
        "|id$_dcent_ubi_volume_plan_check_id=$_dcent_ubi_volume_plan_check_id,$_dcent_ubi_volume_plan_observed_name,$_dcent_ubi_volume_plan_observed_type"
}

dcent_ubi_volume_plan_snapshot()
{
    [ "$#" -eq 4 ] || {
        dcent_ubi_volume_plan_fail \
            "volume-plan admission requires exactly four arguments"
        return 1
    }
    _dcent_ubi_volume_plan_sysfs=$1
    _dcent_ubi_volume_plan_num=$2
    _dcent_ubi_volume_plan_expected_mtd=$3
    _dcent_ubi_volume_plan_prefix=$4

    dcent_ubi_volume_plan_prepare_root \
        "$_dcent_ubi_volume_plan_sysfs" || return 1
    dcent_ubi_volume_plan_canonical_uint \
        "$_dcent_ubi_volume_plan_num" || {
        dcent_ubi_volume_plan_fail \
            "UBI device number is not canonical decimal"
        return 1
    }
    dcent_ubi_volume_plan_canonical_uint \
        "$_dcent_ubi_volume_plan_expected_mtd" || {
        dcent_ubi_volume_plan_fail \
            "expected MTD number is not canonical decimal"
        return 1
    }
    case "$_dcent_ubi_volume_plan_prefix" in
        0|1|2|3) ;;
        *)
            dcent_ubi_volume_plan_fail \
                "prefix must be one of the canonical states 0, 1, 2, or 3"
            return 1
            ;;
    esac

    _dcent_ubi_volume_plan_device=\
$_dcent_ubi_volume_plan_root_real/ubi$_dcent_ubi_volume_plan_num
    _dcent_ubi_volume_plan_device_resolved=$(dcent_ubi_volume_plan_resolve_device \
        "$_dcent_ubi_volume_plan_root_real" \
        "$_dcent_ubi_volume_plan_device" \
        "ubi$_dcent_ubi_volume_plan_num" \
        "$_dcent_ubi_volume_plan_expected_mtd") || return 1

    _dcent_ubi_volume_plan_mtd=$(dcent_ubi_volume_plan_read_attr \
        "$_dcent_ubi_volume_plan_device_resolved/mtd_num" \
        "ubi$_dcent_ubi_volume_plan_num/mtd_num") || return 1
    dcent_ubi_volume_plan_canonical_uint \
        "$_dcent_ubi_volume_plan_mtd" || {
        dcent_ubi_volume_plan_fail \
            "attached UBI MTD identity is not canonical decimal"
        return 1
    }
    [ "$_dcent_ubi_volume_plan_mtd" = \
        "$_dcent_ubi_volume_plan_expected_mtd" ] || {
        dcent_ubi_volume_plan_fail \
            "ubi$_dcent_ubi_volume_plan_num is attached to mtd$_dcent_ubi_volume_plan_mtd, expected mtd$_dcent_ubi_volume_plan_expected_mtd"
        return 1
    }

    _dcent_ubi_volume_plan_count=$(dcent_ubi_volume_plan_read_attr \
        "$_dcent_ubi_volume_plan_device_resolved/volumes_count" \
        "ubi$_dcent_ubi_volume_plan_num/volumes_count") || return 1
    dcent_ubi_volume_plan_canonical_uint \
        "$_dcent_ubi_volume_plan_count" || {
        dcent_ubi_volume_plan_fail \
            "reported volume count is not canonical decimal"
        return 1
    }
    [ "$_dcent_ubi_volume_plan_count" = \
        "$_dcent_ubi_volume_plan_prefix" ] || {
        dcent_ubi_volume_plan_fail \
            "ubi$_dcent_ubi_volume_plan_num reports $_dcent_ubi_volume_plan_count volumes, expected prefix $_dcent_ubi_volume_plan_prefix"
        return 1
    }

    _dcent_ubi_volume_plan_seen=0
    for _dcent_ubi_volume_plan_entry in \
        "$_dcent_ubi_volume_plan_root_real"/ubi"$_dcent_ubi_volume_plan_num"_*
    do
        [ -e "$_dcent_ubi_volume_plan_entry" ] || \
            [ -L "$_dcent_ubi_volume_plan_entry" ] || continue
        _dcent_ubi_volume_plan_entry_id=${_dcent_ubi_volume_plan_entry##*/}
        _dcent_ubi_volume_plan_entry_id=${_dcent_ubi_volume_plan_entry_id#ubi"$_dcent_ubi_volume_plan_num"_}
        dcent_ubi_volume_plan_canonical_uint \
            "$_dcent_ubi_volume_plan_entry_id" || {
            dcent_ubi_volume_plan_fail \
                "target volume class entry has a non-canonical ID: ${_dcent_ubi_volume_plan_entry##*/}"
            return 1
        }
        case "$_dcent_ubi_volume_plan_prefix:$_dcent_ubi_volume_plan_entry_id" in
            1:0|2:0|2:1|3:0|3:1|3:2) ;;
            *)
                dcent_ubi_volume_plan_fail \
                    "volume ID $_dcent_ubi_volume_plan_entry_id is outside prefix $_dcent_ubi_volume_plan_prefix"
                return 1
                ;;
        esac
        dcent_ubi_volume_plan_resolve_volume \
            "$_dcent_ubi_volume_plan_root_real" \
            "$_dcent_ubi_volume_plan_entry" \
            "ubi${_dcent_ubi_volume_plan_num}_${_dcent_ubi_volume_plan_entry_id}" \
            "$_dcent_ubi_volume_plan_device_resolved" >/dev/null || return 1
        _dcent_ubi_volume_plan_seen=$((_dcent_ubi_volume_plan_seen + 1))
    done
    [ "$_dcent_ubi_volume_plan_seen" = \
        "$_dcent_ubi_volume_plan_prefix" ] || {
        dcent_ubi_volume_plan_fail \
            "sysfs exposes $_dcent_ubi_volume_plan_seen target volume entries, expected $_dcent_ubi_volume_plan_prefix"
        return 1
    }

    _dcent_ubi_volume_plan_receipt=\
"dcentos-ubi-volume-plan-v1|ubi=$_dcent_ubi_volume_plan_num|mtd=$_dcent_ubi_volume_plan_mtd|prefix=$_dcent_ubi_volume_plan_prefix"
    if [ "$_dcent_ubi_volume_plan_prefix" != 0 ]; then
        _dcent_ubi_volume_plan_fragment=$(dcent_ubi_volume_plan_check_entry \
            "$_dcent_ubi_volume_plan_root_real" \
            "$_dcent_ubi_volume_plan_num" 0 kernel \
            "$_dcent_ubi_volume_plan_device_resolved") || return 1
        _dcent_ubi_volume_plan_receipt=\
"$_dcent_ubi_volume_plan_receipt$_dcent_ubi_volume_plan_fragment"
    fi
    case "$_dcent_ubi_volume_plan_prefix" in
        2|3)
            _dcent_ubi_volume_plan_fragment=$(dcent_ubi_volume_plan_check_entry \
                "$_dcent_ubi_volume_plan_root_real" \
                "$_dcent_ubi_volume_plan_num" 1 rootfs \
                "$_dcent_ubi_volume_plan_device_resolved") || return 1
            _dcent_ubi_volume_plan_receipt=\
"$_dcent_ubi_volume_plan_receipt$_dcent_ubi_volume_plan_fragment"
            ;;
    esac
    if [ "$_dcent_ubi_volume_plan_prefix" = 3 ]; then
        _dcent_ubi_volume_plan_fragment=$(dcent_ubi_volume_plan_check_entry \
            "$_dcent_ubi_volume_plan_root_real" \
            "$_dcent_ubi_volume_plan_num" 2 rootfs_data \
            "$_dcent_ubi_volume_plan_device_resolved") || return 1
        _dcent_ubi_volume_plan_receipt=\
"$_dcent_ubi_volume_plan_receipt$_dcent_ubi_volume_plan_fragment"
    fi

    printf '%s\n' "$_dcent_ubi_volume_plan_receipt"
}

dcent_ubi_volume_plan_admit()
{
    [ "$#" -eq 4 ] || {
        dcent_ubi_volume_plan_fail \
            "volume-plan admission requires exactly four arguments"
        return 1
    }
    _dcent_ubi_volume_plan_first=$(dcent_ubi_volume_plan_snapshot \
        "$1" "$2" "$3" "$4") || return 1
    _dcent_ubi_volume_plan_second=$(dcent_ubi_volume_plan_snapshot \
        "$1" "$2" "$3" "$4") || return 1
    [ "$_dcent_ubi_volume_plan_first" = \
        "$_dcent_ubi_volume_plan_second" ] || {
        dcent_ubi_volume_plan_fail \
            "sysfs volume identity drifted during admission"
        return 1
    }
    printf '%s\n' "$_dcent_ubi_volume_plan_second"
}

dcent_ubi_volume_plan_revalidate()
{
    [ "$#" -eq 5 ] || {
        dcent_ubi_volume_plan_fail \
            "volume-plan revalidation requires exactly five arguments"
        return 1
    }
    _dcent_ubi_volume_plan_prior=$5
    case "$_dcent_ubi_volume_plan_prior" in
        ''|*'
'*)
            dcent_ubi_volume_plan_fail \
                "prior receipt must be one non-empty line"
            return 1
            ;;
        dcentos-ubi-volume-plan-v1\|*) ;;
        *)
            dcent_ubi_volume_plan_fail \
                "prior receipt has an unsupported schema"
            return 1
            ;;
    esac
    _dcent_ubi_volume_plan_current=$(dcent_ubi_volume_plan_admit \
        "$1" "$2" "$3" "$4") || return 1
    [ "$_dcent_ubi_volume_plan_current" = \
        "$_dcent_ubi_volume_plan_prior" ] || {
        dcent_ubi_volume_plan_fail \
            "semantic volume prefix drifted from the prior receipt"
        return 1
    }
    printf '%s\n' "$_dcent_ubi_volume_plan_current"
}
