#!/bin/sh
# Shared AM3-BB carrier-DTB admission contract. Safe to source from POSIX sh or
# Bash packagers. This inspects bytes only; it never modifies an input artifact.

dcent_am3_bb_dtb_has_sane_fdt_header() {
    _dcent_am3_bb_dtb=$1
    [ "$(od -An -tx1 -N4 "$_dcent_am3_bb_dtb" 2>/dev/null | tr -d '[:space:]')" = "d00dfeed" ] || return 1
    set -- $(od -An -tu1 -j4 -N4 "$_dcent_am3_bb_dtb" 2>/dev/null)
    [ "$#" -eq 4 ] || return 1
    _dcent_am3_bb_total_size=$((($1 * 16777216) + ($2 * 65536) + ($3 * 256) + $4))
    _dcent_am3_bb_file_size=$(wc -c < "$_dcent_am3_bb_dtb" 2>/dev/null) || return 1
    # Product packagers admit an exact DTB file, not a partition dump. Requiring
    # equality makes bytes after the declared FDT boundary invalid, so a marker
    # appended outside the tree can never satisfy carrier policy.
    [ "$_dcent_am3_bb_total_size" -ge 40 ] \
        && [ "$_dcent_am3_bb_total_size" -eq "$_dcent_am3_bb_file_size" ]
}

dcent_am3_bb_dtb_matches_policy() {
    _dcent_am3_bb_dtb=$1
    _dcent_am3_bb_policy=$2
    case "$_dcent_am3_bb_policy" in
        s19j-io-v2)
            grep -a -q 'S19J_IO_BOARD' "$_dcent_am3_bb_dtb"
            ;;
        vnish-btm)
            grep -a -q 'am335x-boneblack-btm' "$_dcent_am3_bb_dtb"
            ;;
        *)
            echo "ERROR: unknown AM3-BB DTB policy: $_dcent_am3_bb_policy" >&2
            return 2
            ;;
    esac
}

dcent_am3_bb_admit_carrier_dtb() {
    _dcent_am3_bb_dtb=$1
    _dcent_am3_bb_policy=$2
    _dcent_am3_bb_allow_unsafe=${3:-0}

    if [ ! -f "$_dcent_am3_bb_dtb" ]; then
        echo "ERROR: AM3-BB carrier DTB is missing: $_dcent_am3_bb_dtb" >&2
        return 1
    fi
    if ! dcent_am3_bb_dtb_has_sane_fdt_header "$_dcent_am3_bb_dtb"; then
        echo "ERROR: AM3-BB DTB has invalid FDT magic/total-size header: $_dcent_am3_bb_dtb" >&2
        return 1
    fi
    case "$_dcent_am3_bb_policy" in
        s19j-io-v2|vnish-btm) ;;
        *)
            echo "ERROR: unknown AM3-BB DTB policy: $_dcent_am3_bb_policy" >&2
            return 1
            ;;
    esac
    if dcent_am3_bb_dtb_matches_policy "$_dcent_am3_bb_dtb" "$_dcent_am3_bb_policy"; then
        echo "[INFO] DTB provenance: carrier policy $_dcent_am3_bb_policy satisfied"
        return 0
    fi
    if [ "$_dcent_am3_bb_allow_unsafe" = "1" ]; then
        echo "[WARN] UNSAFE RE override: accepting valid FDT that violates carrier policy $_dcent_am3_bb_policy: $_dcent_am3_bb_dtb" >&2
        return 0
    fi

    echo "ERROR: refusing AM3-BB DTB that violates carrier policy $_dcent_am3_bb_policy: $_dcent_am3_bb_dtb" >&2
    echo "       S19J_IO_BOARD_V2_0 and VNish BTM are distinct DTB/GPIO lineages." >&2
    return 1
}
