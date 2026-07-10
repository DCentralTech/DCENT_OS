# accept_parse.sh — pure, hardware-free parsers for the DCENT_OS acceptance harness.
#
# This library is the load-bearing PASS/FAIL logic behind dcent-accept.sh. It is
# deliberately split out and side-effect-free so it can be unit-tested against
# captured fixtures with NO miner attached (test_accept_parse.sh, wired into the
# offline CI gate). If the accepted-share counter parse ever silently breaks, the
# whole acceptance gate would rubber-stamp a dead miner — so every function here
# has fixture coverage.
#
# POSIX sh only (runs under BusyBox ash on the miner and `sh -n` in CI). No bashisms.
#
# The firmware-agnostic accepted-share counter is the CGMiner-compatible API on
# port 4028: `{"command":"summary"}` -> SUMMARY[0].Accepted. This is identical
# across DCENT_OS, BraiinsOS, LuxOS, and stock bmminer/cgminer, so the same parser
# validates a miner regardless of which firmware answered.

# accept_parse_accepted — read a CGMiner summary JSON on stdin, echo the SUMMARY
# Accepted counter as a bare integer. Echoes nothing if absent/malformed.
#
# Precision note: the regex requires a double-quote IMMEDIATELY before `Accepted`,
# so it matches the real "Accepted" key but NOT "Difficulty Accepted" (which CGMiner
# also emits, preceded by a space) and NOT "Rejected". head -n1 guards the POOLS
# section also carrying an Accepted key on other commands.
accept_parse_accepted() {
    grep -oE '"Accepted"[[:space:]]*:[[:space:]]*[0-9]+' 2>/dev/null \
        | head -n1 \
        | grep -oE '[0-9]+$' \
        || true
}

# accept_parse_mhs_av — echo the SUMMARY "MHS av" (5s-avg hashrate) as a decimal.
accept_parse_mhs_av() {
    grep -oE '"MHS av"[[:space:]]*:[[:space:]]*[0-9]+\.?[0-9]*' 2>/dev/null \
        | head -n1 \
        | grep -oE '[0-9]+\.?[0-9]*$' \
        || true
}

# accept_parse_elapsed — echo the SUMMARY "Elapsed" (seconds mining) as an integer.
accept_parse_elapsed() {
    grep -oE '"Elapsed"[[:space:]]*:[[:space:]]*[0-9]+' 2>/dev/null \
        | head -n1 \
        | grep -oE '[0-9]+$' \
        || true
}

# accept_parse_enumerated — echo the enumerated chip count from a dcentrald log line
# or REST /api/status body (e.g. "enumerated 342 chips" or "chips_enumerated":342).
accept_parse_enumerated() {
    body=$(cat)
    n=$(printf '%s' "$body" \
        | grep -oE 'enumerated[[:space:]]+[0-9]+[[:space:]]+chips' 2>/dev/null \
        | head -n1 | grep -oE '[0-9]+' | head -n1 || true)
    if [ -z "$n" ]; then
        n=$(printf '%s' "$body" \
            | grep -oE '"chips?_?enumerated?"[[:space:]]*:[[:space:]]*[0-9]+' 2>/dev/null \
            | head -n1 | grep -oE '[0-9]+$' || true)
    fi
    printf '%s' "$n"
}

# accept_parse_temp_c — echo the highest temperature (deg C) found in a REST
# /api/status body or CGMiner-style stats. Reads `temp_c`, `"temp"`, or
# `chip_temp_c` numeric fields and returns the maximum (so a hot board is never
# masked by a cooler sibling reading). Echoes nothing if no temperature present.
accept_parse_temp_c() {
    body=$(cat)
    # Extract every numeric value attached to a temperature-ish key, take the max.
    printf '%s' "$body" \
        | grep -oiE '"(temp_c|chip_temp_c|temp|board_temp_c|soc_temp_c)"[[:space:]]*:[[:space:]]*-?[0-9]+(\.[0-9]+)?' 2>/dev/null \
        | grep -oE '\-?[0-9]+(\.[0-9]+)?$' \
        | sort -n \
        | tail -n1 \
        || true
}

# accept_temp_safe <temp_c> <ceiling_c> — echo SAFE and return 0 when the observed
# temperature is a finite number at or below the ceiling; echo HOT and return 1
# when it exceeds the ceiling. A MISSING/non-numeric reading fails CLOSED (echo
# UNKNOWN, return 1) — a soak that cannot read temperature must not be trusted to
# keep hashing (cut-hash-before-noise / never mask a thermal blind spot).
accept_temp_safe() {
    _t=${1:-}
    _ceil=${2:-75}
    case "$_t" in
        '' | *[!0-9.-]* | *.*.* | -) echo UNKNOWN; return 1 ;;
    esac
    # Integer-compare the truncated degrees (drop any fractional part).
    _ti=${_t%%.*}
    case "$_ti" in '' | -) _ti=0 ;; esac
    _ci=${_ceil%%.*}
    case "$_ci" in '' | -) _ci=75 ;; esac
    # Reject non-integer leftovers defensively.
    case "$_ti" in *[!0-9-]*) echo UNKNOWN; return 1 ;; esac
    case "$_ci" in *[!0-9-]*) _ci=75 ;; esac
    if [ "$_ti" -le "$_ci" ]; then
        echo SAFE
        return 0
    fi
    echo HOT
    return 1
}

# accept_is_uint — return 0 if $1 is a non-empty string of only decimal digits.
accept_is_uint() {
    case "${1:-}" in
        '' | *[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

# accept_verdict <count> <threshold> — echo PASS and return 0 iff count>=threshold
# (both coerced to non-negative ints; junk -> 0 for count, 1 for threshold), else
# echo FAIL and return 1. This is the single source of the accept gate decision.
accept_verdict() {
    _c=${1:-0}
    _t=${2:-1}
    accept_is_uint "$_c" || _c=0
    accept_is_uint "$_t" || _t=1
    if [ "$_c" -ge "$_t" ]; then
        echo PASS
        return 0
    fi
    echo FAIL
    return 1
}

# accept_soak_verdict — SUSTAINED-mining stability verdict from a series of soak
# snapshots on stdin, one per line: "elapsed_s accepted mhs_av temp_c". Args:
#   <temp_ceiling_c> [min_hashrate_retention_pct=70] [min_samples=3]
# Echoes SOAK_PASS and returns 0 when the run is stable; else SOAK_FAIL:<reason>
# and returns 1. This catches what the single-point accept gate CANNOT: a miner
# that produces its first shares then death-spirals, thermally throttles, or
# stalls. A soak is STABLE iff:
#   (1) at least <min_samples> snapshots (a soak needs duration);
#   (2) shares ADVANCED — last Accepted strictly greater than first (mining is
#       still producing shares at the end, not stalled after the first one);
#   (3) hashrate did NOT collapse — the minimum "MHS av" across all samples is at
#       least <retention_pct>% of the maximum (a throttle/death-spiral is a FAIL
#       even if shares kept trickling in);
#   (4) EVERY temperature stayed SAFE (<= ceiling) — one hot OR one unreadable
#       (thermal-blind) sample fails the whole soak (fail-closed, never mask a
#       thermal excursion or a blind spot).
# Malformed / short input fails CLOSED. Iterates via a here-doc (not a pipe) so
# the accumulators survive under BusyBox ash whether stdin is piped or redirected.
accept_soak_verdict() {
    _ceil=${1:-75}
    _ret=${2:-70}
    _minn=${3:-3}
    case "$_ret" in '' | *[!0-9]*) _ret=70 ;; esac
    case "$_minn" in '' | *[!0-9]*) _minn=3 ;; esac

    _sk_data=$(cat)
    _sk_n=0
    _sk_first=''
    _sk_last=''
    _sk_min=''
    _sk_max=''
    _sk_hot=0
    _sk_blind=0
    while read -r _sk_el _sk_acc _sk_mhs _sk_temp _sk_rest; do
        # Skip blank lines (a trailing newline in the here-doc, etc.).
        [ -n "$_sk_el$_sk_acc$_sk_mhs$_sk_temp" ] || continue
        accept_is_uint "$_sk_acc" || { echo "SOAK_FAIL:nonnumeric_accepted"; return 1; }
        _sk_mhi=${_sk_mhs%%.*}
        case "$_sk_mhi" in
            '') _sk_mhi=0 ;;
            *[!0-9]*) echo "SOAK_FAIL:nonnumeric_mhs"; return 1 ;;
        esac
        _sk_n=$(( _sk_n + 1 ))
        [ -n "$_sk_first" ] || _sk_first=$_sk_acc
        _sk_last=$_sk_acc
        if [ -z "$_sk_min" ] || [ "$_sk_mhi" -lt "$_sk_min" ]; then _sk_min=$_sk_mhi; fi
        if [ -z "$_sk_max" ] || [ "$_sk_mhi" -gt "$_sk_max" ]; then _sk_max=$_sk_mhi; fi
        case "$(accept_temp_safe "$_sk_temp" "$_ceil")" in
            SAFE) : ;;
            HOT) _sk_hot=1 ;;
            *) _sk_blind=1 ;;
        esac
    done <<SOAK_EOF
$_sk_data
SOAK_EOF

    if [ "$_sk_n" -lt "$_minn" ]; then
        echo "SOAK_FAIL:too_few_samples($_sk_n<$_minn)"
        return 1
    fi
    if [ "$_sk_hot" -ne 0 ]; then
        echo "SOAK_FAIL:thermal_excursion"
        return 1
    fi
    if [ "$_sk_blind" -ne 0 ]; then
        echo "SOAK_FAIL:thermal_blind"
        return 1
    fi
    if [ "$_sk_last" -le "$_sk_first" ]; then
        echo "SOAK_FAIL:shares_not_advancing($_sk_first->$_sk_last)"
        return 1
    fi
    # Divide-first to stay clear of 32-bit overflow on TH/s-scale MHS values.
    _sk_floor=$(( (_sk_max / 100) * _ret ))
    if [ "$_sk_min" -lt "$_sk_floor" ]; then
        echo "SOAK_FAIL:hashrate_collapse(min=$_sk_min<floor=$_sk_floor)"
        return 1
    fi
    echo SOAK_PASS
    return 0
}

# accept_boot_verdict — read a captured boot / UART serial-console log on stdin.
# Echo BOOT_PASS (the unit reached mining) or BOOT_FAIL:<furthest_stage> so a failed
# cold boot is diagnosed at its EXACT stall point instead of "it didn't come up".
# This is the missing analysis step for the deferred SD-first cold-boot blockers
# (am3-bb §"SD-first Cold-Boot Blocker" requires a UART capture — this turns that
# capture into an actionable verdict). Firmware-agnostic: recognizes the boot chain
# (U-Boot -> kernel -> userspace init) plus the DCENT_OS daemon / chip-enum / mining
# markers. Milestones are ordered; the furthest one observed wins (a boot log is
# cumulative, so reaching a later stage implies the earlier banners also appeared).
# Fail-closed: an empty/garbage log yields BOOT_FAIL:none.
accept_boot_verdict() {
    _bv=$(cat)
    _stage=none
    printf '%s' "$_bv" | grep -qiE 'U-Boot (SPL |20)|Hit any key to stop|BOOT_from' && _stage=uboot
    printf '%s' "$_bv" | grep -qiE 'Starting kernel|Booting Linux|Linux version [0-9]|Uncompressing Linux' && _stage=kernel
    printf '%s' "$_bv" | grep -qiE 'Freeing unused kernel|Run /sbin/init|Starting S[0-9]|/etc/init.d/rcS|BusyBox v' && _stage=init
    printf '%s' "$_bv" | grep -qiE 'dcentrald|DCENT_OS v|mining daemon|CGMiner API' && _stage=dcentrald
    printf '%s' "$_bv" | grep -qiE 'enumerated [0-9]+ chips|chips_enumerated' && _stage=enum
    printf '%s' "$_bv" | grep -qiE '[Aa]ccepted share|shares?_accepted|ACCEPT GATE PASS|MHS av|first accepted' && _stage=mining
    if [ "$_stage" = "mining" ]; then
        echo BOOT_PASS
        return 0
    fi
    echo "BOOT_FAIL:$_stage"
    return 1
}

# accept_matrix_verdict — release-readiness roll-up. Reads the acceptance harness
# `--json` result lines (one JSON object per phase per SKU) on stdin, prints a
# per-result "SKU PHASE RESULT" grid, then echoes RELEASE_GO (every result was
# PASS) or RELEASE_NOGO:<sku:phase,...> listing every non-PASS. This is the
# validation-dashboard roll-up: run `dcent-accept.sh <phase> <SKU> <IP> --json`
# for each SKU/phase, collect the JSON lines, pipe them here for one GO/.
# Fail-closed: zero parseable results -> RELEASE_NOGO:no_results. The grid goes to
# stderr so stdout carries ONLY the machine-readable verdict.
accept_matrix_verdict() {
    _mxdata=$(cat)
    _mxfails=""
    _mxtotal=0
    while IFS= read -r _mxline; do
        case "$_mxline" in *'"result"'*) : ;; *) continue ;; esac
        _mxr=$(printf '%s' "$_mxline" \
            | grep -oiE '"result"[[:space:]]*:[[:space:]]*"(PASS|FAIL)"' \
            | grep -oiE 'PASS|FAIL' | head -n1 | tr 'a-z' 'A-Z')
        [ -n "$_mxr" ] || continue
        _mxs=$(printf '%s' "$_mxline" \
            | grep -oE '"sku"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n1 \
            | grep -oE '"[^"]*"$' | tr -d '"')
        _mxm=$(printf '%s' "$_mxline" \
            | grep -oE '"mode"[[:space:]]*:[[:space:]]*"[^"]*"' | head -n1 \
            | grep -oE '"[^"]*"$' | tr -d '"')
        _mxtotal=$((_mxtotal + 1))
        _mxs=${_mxs:-?}
        _mxm=${_mxm:-?}
        printf '  %-10s %-12s %s\n' "$_mxs" "$_mxm" "$_mxr" >&2
        if [ "$_mxr" != "PASS" ]; then
            _mxfails="${_mxfails}${_mxs}:${_mxm} "
        fi
    done <<MATRIX_EOF
$_mxdata
MATRIX_EOF

    if [ "$_mxtotal" -eq 0 ]; then
        echo "RELEASE_NOGO:no_results"
        return 1
    fi
    if [ -n "$_mxfails" ]; then
        echo "RELEASE_NOGO:$(printf '%s' "$_mxfails" | sed 's/ *$//' | tr ' ' ',')"
        return 1
    fi
    echo RELEASE_GO
    return 0
}

# accept_ota_verdict — read a witnessed OTA-capstone transcript on stdin. Echo
# OTA_PASS (the signed OTA update completed end-to-end and the unit is mining the
# new version) or OTA_FAIL:<furthest_stage> so a stalled capstone is diagnosed at
# its exact point. This is the reproducible verdict for the witnessed-OTA blocker:
# run the OTA sequence capturing its output, pipe it here for one PASS/FAIL.
#
# Ordered milestones — deliberately encoding the  OTA truth contracts so a
# weaker signal can NEVER be scored as a stronger one:
#   uploaded -> signature_verified -> scheduled -> rebooted -> version_confirmed
#   -> mining_resumed
# "uploaded" alone is NOT proof; "scheduled" != flashed; only an OBSERVED reboot +
# the EXPECTED version + resumed accepted shares is a real capstone pass. The
# furthest ordered marker present wins (a capstone log is cumulative). Fail-closed:
# a markerless / truncated transcript yields OTA_FAIL:none.
accept_ota_verdict() {
    _ov=$(cat)
    _ostage=none
    printf '%s' "$_ov" | grep -qiE 'uploaded|upload accepted|staged_path|upload_accepted' && _ostage=uploaded
    printf '%s' "$_ov" | grep -qiE 'signature verified|verify_sysupgrade_bundle|Ed25519.*(ok|verified)|MANIFEST\.sig.*(ok|verified)|OTA signature (ok|verified)' && _ostage=signature_verified
    printf '%s' "$_ov" | grep -qiE 'scheduled|sysupgrade -f|upgrade scheduled|staged for install' && _ostage=scheduled
    printf '%s' "$_ov" | grep -qiE 'reboot (observed|confirmed)|rebooted|booted into|boot observed|came back up' && _ostage=rebooted
    printf '%s' "$_ov" | grep -qiE 'version (confirmed|matches)|expected version|running version.*ok|post-reboot version' && _ostage=version_confirmed
    printf '%s' "$_ov" | grep -qiE '[Aa]ccepted share|ACCEPT GATE PASS|mining resumed|shares? accepted after' && _ostage=mining_resumed
    if [ "$_ostage" = "mining_resumed" ]; then
        echo OTA_PASS
        return 0
    fi
    echo "OTA_FAIL:$_ostage"
    return 1
}

# accept_enum_verdict <observed> <expected> — enumeration sanity. Expected 0 means
# UNCONFIRMED (capture-first SKU): any non-zero observed count is a CAPTURE pass.
# Otherwise require observed within +/-10% of expected (binning tolerance) and echo
# PASS / CAPTURE / FAIL accordingly.
accept_enum_verdict() {
    _obs=${1:-0}
    _exp=${2:-0}
    accept_is_uint "$_obs" || _obs=0
    accept_is_uint "$_exp" || _exp=0
    if [ "$_exp" -eq 0 ]; then
        if [ "$_obs" -gt 0 ]; then
            echo CAPTURE
            return 0
        fi
        echo FAIL
        return 1
    fi
    # tolerance band = expected +/- 10% (integer math, floor/ceil-safe)
    _lo=$(( _exp - (_exp / 10) - 1 ))
    _hi=$(( _exp + (_exp / 10) + 1 ))
    if [ "$_obs" -ge "$_lo" ] && [ "$_obs" -le "$_hi" ]; then
        echo PASS
        return 0
    fi
    echo FAIL
    return 1
}
