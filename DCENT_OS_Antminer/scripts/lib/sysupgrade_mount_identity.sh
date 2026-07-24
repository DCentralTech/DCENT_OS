#!/bin/sh
# Exact mount ownership evidence for a future Zynq sysupgrade resource ledger.
#
# Public API:
#   dcent_sysupgrade_mount_require_absent MOUNTINFO TARGET
#   dcent_sysupgrade_mount_admit MOUNTINFO SOURCE TARGET MODE MAJOR_MINOR ROOT
#   dcent_sysupgrade_mount_readmit MOUNTINFO MOUNT_ID PARENT_ID SOURCE TARGET MODE
#       MAJOR_MINOR ROOT MOUNT_OPTIONS SUPER_OPTIONS
#   dcent_sysupgrade_mount_require_released MOUNTINFO MOUNT_ID TARGET
#
# MOUNTINFO is a caller-supplied /proc/self/mountinfo-format snapshot source.
# Admission accepts one exact UBIFS row only.  The discovered fields below are
# plain shell variables suitable for an exact ledger receipt; parsing never
# evaluates mountinfo content as shell input.
#
# This helper observes mount namespace state.  It deliberately does not call
# mount(8), umount(8), mkdir(1), or the resource-ledger API.  Caller integration
# must publish a pending receipt before mount, record the admitted mount ID
# after mount, readmit that same ID immediately before unmount, and prove its
# absence afterward.

DCENT_SYSUPGRADE_MOUNT_OBSERVED=0
DCENT_SYSUPGRADE_MOUNT_ID=
DCENT_SYSUPGRADE_MOUNT_PARENT_ID=
DCENT_SYSUPGRADE_MOUNT_MAJOR_MINOR=
DCENT_SYSUPGRADE_MOUNT_ROOT=
DCENT_SYSUPGRADE_MOUNT_SOURCE=
DCENT_SYSUPGRADE_MOUNT_TARGET=
DCENT_SYSUPGRADE_MOUNT_MODE=
DCENT_SYSUPGRADE_MOUNT_FS_TYPE=
DCENT_SYSUPGRADE_MOUNT_OPTIONS=
DCENT_SYSUPGRADE_MOUNT_SUPER_OPTIONS=
DCENT_SYSUPGRADE_MOUNTINFO_MAX_BYTES=1048576
DCENT_SYSUPGRADE_MOUNTINFO_MAX_ROWS=4096

dcent_sysupgrade_mount_fail()
{
    printf '%s\n' "sysupgrade-mount-identity: ERROR: $*" >&2
    return 1
}

dcent_sysupgrade_mount_clear_observation()
{
    DCENT_SYSUPGRADE_MOUNT_OBSERVED=0
    DCENT_SYSUPGRADE_MOUNT_ID=
    DCENT_SYSUPGRADE_MOUNT_PARENT_ID=
    DCENT_SYSUPGRADE_MOUNT_MAJOR_MINOR=
    DCENT_SYSUPGRADE_MOUNT_ROOT=
    DCENT_SYSUPGRADE_MOUNT_SOURCE=
    DCENT_SYSUPGRADE_MOUNT_TARGET=
    DCENT_SYSUPGRADE_MOUNT_MODE=
    DCENT_SYSUPGRADE_MOUNT_FS_TYPE=
    DCENT_SYSUPGRADE_MOUNT_OPTIONS=
    DCENT_SYSUPGRADE_MOUNT_SUPER_OPTIONS=
}

dcent_sysupgrade_mount_uint()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ''|*[!0-9]*|0[0-9]*) return 1 ;;
    esac
}

dcent_sysupgrade_mount_positive_uint()
{
    dcent_sysupgrade_mount_uint "$1" && [ "$1" != 0 ]
}

dcent_sysupgrade_mount_path_syntax()
{
    [ "$#" -eq 2 ] || return 1
    _dcent_mount_path=$1
    _dcent_mount_allow_root=$2
    case "$_dcent_mount_path" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$_dcent_mount_path" in
        *//*|*/./*|*/../*|*/.|*/..|*[!A-Za-z0-9._/@:+-]*) return 1 ;;
    esac
    if [ "$_dcent_mount_path" = / ]; then
        [ "$_dcent_mount_allow_root" = 1 ] || return 1
    fi
}

dcent_sysupgrade_mount_target()
{
    [ "$#" -eq 1 ] || return 1
    dcent_sysupgrade_mount_path_syntax "$1" 0 || {
        dcent_sysupgrade_mount_fail "mount target is not a safe canonical absolute path"
        return 1
    }
    [ -d "$1" ] && [ ! -L "$1" ] || {
        dcent_sysupgrade_mount_fail "mount target is absent, not a directory, or a symlink"
        return 1
    }
    _dcent_mount_target_real=$(CDPATH='' cd -P -- "$1" 2>/dev/null && pwd -P) || {
        dcent_sysupgrade_mount_fail "cannot canonicalize mount target"
        return 1
    }
    [ "$_dcent_mount_target_real" = "$1" ] || {
        dcent_sysupgrade_mount_fail "mount target contains a symlink or non-canonical component"
        return 1
    }
}

dcent_sysupgrade_mount_source()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ubi*:*) ;;
        *) return 1 ;;
    esac
    _dcent_mount_source_device=${1%%:*}
    _dcent_mount_source_volume=${1#*:}
    [ "$_dcent_mount_source_volume" = "${1##*:}" ] || return 1
    _dcent_mount_source_number=${_dcent_mount_source_device#ubi}
    dcent_sysupgrade_mount_uint "$_dcent_mount_source_number" || return 1
    case "$_dcent_mount_source_volume" in
        ''|.*|*[!A-Za-z0-9._-]*) return 1 ;;
    esac
}

dcent_sysupgrade_mount_major_minor()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        *:*) ;;
        *) return 1 ;;
    esac
    _dcent_mount_major=${1%%:*}
    _dcent_mount_minor=${1#*:}
    [ "$_dcent_mount_minor" = "${1##*:}" ] || return 1
    dcent_sysupgrade_mount_uint "$_dcent_mount_major" &&
        dcent_sysupgrade_mount_uint "$_dcent_mount_minor" || return 1
    [ "${#_dcent_mount_major}" -le 4 ] &&
        [ "${#_dcent_mount_minor}" -le 7 ] &&
        [ "$_dcent_mount_major" -le 4095 ] &&
        [ "$_dcent_mount_minor" -le 1048575 ]
}

dcent_sysupgrade_mount_options_syntax()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        ''|,*|*,|*,,*|*[!A-Za-z0-9._:/+=,-]*) return 1 ;;
    esac
}

dcent_sysupgrade_mountinfo_file()
{
    [ "$#" -eq 1 ] || return 1
    case "$1" in
        /*) ;;
        *) dcent_sysupgrade_mount_fail "mountinfo path must be absolute"; return 1 ;;
    esac
    [ -r "$1" ] && [ -f "$1" ] && [ ! -L "$1" ] || {
        dcent_sysupgrade_mount_fail "mountinfo source is not a readable non-symlink file"
        return 1
    }
    _dcent_mountinfo_bytes=$(wc -c <"$1" 2>/dev/null | tr -d '[:space:]') || return 1
    dcent_sysupgrade_mount_positive_uint "$_dcent_mountinfo_bytes" || {
        dcent_sysupgrade_mount_fail "mountinfo source is empty or has an invalid size"
        return 1
    }
    [ "${#_dcent_mountinfo_bytes}" -le 7 ] &&
        [ "$_dcent_mountinfo_bytes" -le "$DCENT_SYSUPGRADE_MOUNTINFO_MAX_BYTES" ] || {
        dcent_sysupgrade_mount_fail "mountinfo source exceeds the offline admission byte ceiling"
        return 1
    }
    _dcent_mountinfo_rows=$(wc -l <"$1" 2>/dev/null | tr -d '[:space:]') || return 1
    dcent_sysupgrade_mount_positive_uint "$_dcent_mountinfo_rows" &&
        [ "${#_dcent_mountinfo_rows}" -le 4 ] &&
        [ "$_dcent_mountinfo_rows" -le "$DCENT_SYSUPGRADE_MOUNTINFO_MAX_ROWS" ] || {
        dcent_sysupgrade_mount_fail "mountinfo source exceeds the offline admission row ceiling"
        return 1
    }
    _dcent_mountinfo_bad_bytes=$(LC_ALL=C tr -d '\012\040-\176' <"$1" 2>/dev/null |
        wc -c | tr -d '[:space:]') || return 1
    [ "$_dcent_mountinfo_bad_bytes" = 0 ] || {
        dcent_sysupgrade_mount_fail "mountinfo contains non-printable, CR, NUL, or non-ASCII bytes"
        return 1
    }
    _dcent_mountinfo_final_newline=$(tail -c 1 "$1" 2>/dev/null | wc -l |
        tr -d '[:space:]') || return 1
    [ "$_dcent_mountinfo_final_newline" = 1 ] || {
        dcent_sysupgrade_mount_fail "mountinfo has an unterminated final record"
        return 1
    }
}

# Scan output is a fixed ten-line record.  Every exported value is validated
# before assignment, so no eval or shell interpretation of mountinfo is used.
dcent_sysupgrade_mount_scan()
{
    [ "$#" -eq 11 ] || return 1
    _dcent_mount_scan_mode=$1
    _dcent_mount_scan_file=$2
    _dcent_mount_scan_id=$3
    _dcent_mount_scan_source=$4
    _dcent_mount_scan_target=$5
    _dcent_mount_scan_mode_option=$6
    _dcent_mount_scan_major_minor=$7
    _dcent_mount_scan_root=$8
    _dcent_mount_scan_parent_id=$9
    _dcent_mount_scan_mount_options=${10}
    _dcent_mount_scan_super_options=${11}

    _dcent_mount_scan_before=$(cksum "$_dcent_mount_scan_file" 2>/dev/null) || return 1
    _dcent_mount_scan_result=$(LC_ALL=C awk \
        -v operation="$_dcent_mount_scan_mode" \
        -v expected_id="$_dcent_mount_scan_id" \
        -v expected_source="$_dcent_mount_scan_source" \
        -v expected_target="$_dcent_mount_scan_target" \
        -v expected_mode="$_dcent_mount_scan_mode_option" \
        -v expected_dev="$_dcent_mount_scan_major_minor" \
        -v expected_root="$_dcent_mount_scan_root" \
        -v expected_parent_id="$_dcent_mount_scan_parent_id" \
        -v expected_mount_options="$_dcent_mount_scan_mount_options" \
        -v expected_super_options="$_dcent_mount_scan_super_options" '
        function uint(v) {
            return v ~ /^(0|[1-9][0-9]*)$/
        }
        function positive_uint(v) {
            return uint(v) && v != "0"
        }
        function safe_path(v, allow_root) {
            if (substr(v, 1, 1) != "/" || v ~ /\\/ ||
                v ~ /[^A-Za-z0-9._\/@:+-]/ || index(v, "//") ||
                v ~ /\/\.\// || v ~ /\/\.\.\// ||
                v ~ /\/\.$/ || v ~ /\/\.\.$/) return 0
            return allow_root || v != "/"
        }
        function option_count(options, wanted, parts, count, field, fields) {
            fields = split(options, parts, ",")
            count = 0
            for (field = 1; field <= fields; field++) {
                if (parts[field] == wanted) count++
            }
            return count
        }
        function valid_options(options) {
            return options ~ /^[A-Za-z0-9._:\/+==-]+(,[A-Za-z0-9._:\/+==-]+)*$/
        }
        function valid_optional(field, tag, value) {
            if (field !~ /^[A-Za-z][A-Za-z0-9_.-]*(:[A-Za-z0-9_.+-]+)?$/)
                return 0
            tag = field
            sub(/:.*/, "", tag)
            if (optional_seen[tag]++) return 0
            if (tag == "shared" || tag == "master" || tag == "propagate_from") {
                if (index(field, ":") == 0) return 0
                value = substr(field, index(field, ":") + 1)
                return positive_uint(value)
            }
            if (tag == "unbindable" || tag == "idmapped")
                return field == tag
            return 1
        }
        function valid_candidate_mode(mount_options, super_options, mount_modes, super_modes) {
            if (!valid_options(mount_options) || !valid_options(super_options)) return 0
            mount_modes = option_count(mount_options, "ro") + option_count(mount_options, "rw")
            super_modes = option_count(super_options, "ro") + option_count(super_options, "rw")
            if (mount_modes != 1 || super_modes != 1) return 0
            if (option_count(mount_options, expected_mode) != 1) return 0
            return option_count(super_options, expected_mode) == 1
        }
        {
            for (optional_tag in optional_seen) delete optional_seen[optional_tag]
            if (NF < 10 || !positive_uint($1) || !positive_uint($2)) {
                malformed = 1
                next
            }
            device_number_count = split($3, device_numbers, ":")
            if (device_number_count != 2 ||
                !uint(device_numbers[1]) || !uint(device_numbers[2]) ||
                device_numbers[1] > 4095 || device_numbers[2] > 1048575) {
                malformed = 1
                next
            }
            if (mount_id_seen[$1]++) malformed = 1
            separator = 0
            separator_count = 0
            for (field = 7; field <= NF; field++) {
                if ($field == "-") {
                    separator = field
                    separator_count++
                }
            }
            if (separator_count != 1 || separator < 7 || separator + 3 != NF) {
                malformed = 1
                next
            }
            for (field = 7; field < separator; field++) {
                if (!valid_optional($field)) malformed = 1
            }
            if ($4 == "" || $5 == "" || $6 == "" ||
                $(separator + 1) == "" || $(separator + 2) == "" ||
                $(separator + 3) == "") malformed = 1

            if ($5 == expected_target) {
                target_rows++
                candidate_id = $1
                candidate_parent = $2
                candidate_dev = $3
                candidate_root = $4
                candidate_target = $5
                candidate_options = $6
                candidate_fstype = $(separator + 1)
                candidate_source = $(separator + 2)
                candidate_super = $(separator + 3)
                if (!safe_path(candidate_target, 0) ||
                    !safe_path(candidate_root, 1)) candidate_invalid = 1
                if (operation == "admit" || operation == "readmit") {
                    if (candidate_source != expected_source ||
                        candidate_target != expected_target ||
                        candidate_fstype != "ubifs" ||
                        candidate_dev != expected_dev ||
                        candidate_root != expected_root ||
                        !valid_candidate_mode(candidate_options, candidate_super))
                        candidate_invalid = 1
                    if (operation == "readmit" &&
                        (candidate_id != expected_id ||
                         candidate_parent != expected_parent_id ||
                         candidate_options != expected_mount_options ||
                         candidate_super != expected_super_options))
                        candidate_invalid = 1
                }
            }
            if (operation == "released" && $1 == expected_id) id_rows++
        }
        END {
            if (malformed) exit 20
            if (operation == "absent") exit target_rows == 0 ? 0 : 21
            if (operation == "released")
                exit (target_rows == 0 && id_rows == 0) ? 0 : 22
            if ((operation != "admit" && operation != "readmit") ||
                target_rows != 1 || candidate_invalid) exit 23
            print "mount_id=" candidate_id
            print "parent_id=" candidate_parent
            print "major_minor=" candidate_dev
            print "root=" candidate_root
            print "source=" candidate_source
            print "target=" candidate_target
            print "mode=" expected_mode
            print "fs_type=" candidate_fstype
            print "mount_options=" candidate_options
            print "super_options=" candidate_super
        }
    ' "$_dcent_mount_scan_file") || {
        dcent_sysupgrade_mount_fail "mountinfo does not prove the requested exact mount state"
        return 1
    }
    _dcent_mount_scan_after=$(cksum "$_dcent_mount_scan_file" 2>/dev/null) || return 1
    [ "$_dcent_mount_scan_before" = "$_dcent_mount_scan_after" ] || {
        dcent_sysupgrade_mount_fail "mountinfo changed while it was being admitted"
        return 1
    }

    case "$_dcent_mount_scan_mode" in
        absent|released) [ -z "$_dcent_mount_scan_result" ] ;;
        admit|readmit) dcent_sysupgrade_mount_publish "$_dcent_mount_scan_result" ;;
        *) return 1 ;;
    esac
}

dcent_sysupgrade_mount_publish()
{
    [ "$#" -eq 1 ] || return 1
    _dcent_mount_record=$1
    [ "$(printf '%s\n' "$_dcent_mount_record" | wc -l | tr -d '[:space:]')" = 10 ] || return 1

    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '1p')
    case "$_dcent_mount_line" in mount_id=*) DCENT_SYSUPGRADE_MOUNT_ID=${_dcent_mount_line#mount_id=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '2p')
    case "$_dcent_mount_line" in parent_id=*) DCENT_SYSUPGRADE_MOUNT_PARENT_ID=${_dcent_mount_line#parent_id=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '3p')
    case "$_dcent_mount_line" in major_minor=*) DCENT_SYSUPGRADE_MOUNT_MAJOR_MINOR=${_dcent_mount_line#major_minor=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '4p')
    case "$_dcent_mount_line" in root=*) DCENT_SYSUPGRADE_MOUNT_ROOT=${_dcent_mount_line#root=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '5p')
    case "$_dcent_mount_line" in source=*) DCENT_SYSUPGRADE_MOUNT_SOURCE=${_dcent_mount_line#source=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '6p')
    case "$_dcent_mount_line" in target=*) DCENT_SYSUPGRADE_MOUNT_TARGET=${_dcent_mount_line#target=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '7p')
    case "$_dcent_mount_line" in mode=*) DCENT_SYSUPGRADE_MOUNT_MODE=${_dcent_mount_line#mode=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '8p')
    case "$_dcent_mount_line" in fs_type=*) DCENT_SYSUPGRADE_MOUNT_FS_TYPE=${_dcent_mount_line#fs_type=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '9p')
    case "$_dcent_mount_line" in mount_options=*) DCENT_SYSUPGRADE_MOUNT_OPTIONS=${_dcent_mount_line#mount_options=} ;; *) return 1 ;; esac
    _dcent_mount_line=$(printf '%s\n' "$_dcent_mount_record" | sed -n '10p')
    case "$_dcent_mount_line" in super_options=*) DCENT_SYSUPGRADE_MOUNT_SUPER_OPTIONS=${_dcent_mount_line#super_options=} ;; *) return 1 ;; esac

    dcent_sysupgrade_mount_positive_uint "$DCENT_SYSUPGRADE_MOUNT_ID" &&
        dcent_sysupgrade_mount_positive_uint "$DCENT_SYSUPGRADE_MOUNT_PARENT_ID" &&
        dcent_sysupgrade_mount_major_minor "$DCENT_SYSUPGRADE_MOUNT_MAJOR_MINOR" &&
        dcent_sysupgrade_mount_path_syntax "$DCENT_SYSUPGRADE_MOUNT_ROOT" 1 &&
        dcent_sysupgrade_mount_source "$DCENT_SYSUPGRADE_MOUNT_SOURCE" &&
        dcent_sysupgrade_mount_path_syntax "$DCENT_SYSUPGRADE_MOUNT_TARGET" 0 || {
        dcent_sysupgrade_mount_clear_observation
        return 1
    }
    case "$DCENT_SYSUPGRADE_MOUNT_MODE" in ro|rw) ;; *) dcent_sysupgrade_mount_clear_observation; return 1 ;; esac
    [ "$DCENT_SYSUPGRADE_MOUNT_FS_TYPE" = ubifs ] || {
        dcent_sysupgrade_mount_clear_observation
        return 1
    }
    DCENT_SYSUPGRADE_MOUNT_OBSERVED=1
}

dcent_sysupgrade_mount_require_absent()
{
    [ "$#" -eq 2 ] || {
        dcent_sysupgrade_mount_fail "require_absent requires MOUNTINFO TARGET"
        return 1
    }
    dcent_sysupgrade_mount_clear_observation
    dcent_sysupgrade_mountinfo_file "$1" &&
        dcent_sysupgrade_mount_target "$2" || return 1
    dcent_sysupgrade_mount_scan absent "$1" - - "$2" - - - - - -
}

dcent_sysupgrade_mount_admit()
{
    [ "$#" -eq 6 ] || {
        dcent_sysupgrade_mount_fail "admit requires MOUNTINFO SOURCE TARGET MODE MAJOR_MINOR ROOT"
        return 1
    }
    dcent_sysupgrade_mount_clear_observation
    dcent_sysupgrade_mountinfo_file "$1" &&
        dcent_sysupgrade_mount_source "$2" &&
        dcent_sysupgrade_mount_target "$3" || return 1
    case "$4" in ro|rw) ;; *) return 1 ;; esac
    dcent_sysupgrade_mount_major_minor "$5" &&
        dcent_sysupgrade_mount_path_syntax "$6" 1 || return 1
    dcent_sysupgrade_mount_scan admit "$1" - "$2" "$3" "$4" "$5" "$6" - - -
}

dcent_sysupgrade_mount_readmit()
{
    [ "$#" -eq 10 ] || {
        dcent_sysupgrade_mount_fail "readmit requires MOUNTINFO MOUNT_ID PARENT_ID SOURCE TARGET MODE MAJOR_MINOR ROOT MOUNT_OPTIONS SUPER_OPTIONS"
        return 1
    }
    dcent_sysupgrade_mount_clear_observation
    dcent_sysupgrade_mountinfo_file "$1" &&
        dcent_sysupgrade_mount_positive_uint "$2" &&
        dcent_sysupgrade_mount_positive_uint "$3" &&
        dcent_sysupgrade_mount_source "$4" &&
        dcent_sysupgrade_mount_target "$5" || return 1
    case "$6" in ro|rw) ;; *) return 1 ;; esac
    dcent_sysupgrade_mount_major_minor "$7" &&
        dcent_sysupgrade_mount_path_syntax "$8" 1 &&
        dcent_sysupgrade_mount_options_syntax "$9" &&
        dcent_sysupgrade_mount_options_syntax "${10}" || return 1
    dcent_sysupgrade_mount_scan readmit "$1" "$2" "$4" "$5" "$6" "$7" "$8" \
        "$3" "$9" "${10}"
}

dcent_sysupgrade_mount_require_released()
{
    [ "$#" -eq 3 ] || {
        dcent_sysupgrade_mount_fail "require_released requires MOUNTINFO MOUNT_ID TARGET"
        return 1
    }
    dcent_sysupgrade_mount_clear_observation
    dcent_sysupgrade_mountinfo_file "$1" &&
        dcent_sysupgrade_mount_positive_uint "$2" &&
        dcent_sysupgrade_mount_target "$3" || return 1
    dcent_sysupgrade_mount_scan released "$1" "$2" - "$3" - - - - - -
}
