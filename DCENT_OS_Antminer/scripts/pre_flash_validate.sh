#!/bin/sh
#
# pre_flash_validate.sh - 8-gate pre-flight before sysupgrade to NAND on am2-s19jpro.
#
# Runs all 8 Phase 3 gates before we commit to flashing DCENT_OS into the
# inactive NAND slot. Any gate failure exits 1 and prints why. Gates are
# intentionally paranoid - a wrong tarball prefix or stale binary is how
# we brick miners.
#
# Usage:
#   scripts/pre_flash_validate.sh <miner-ip> <tarball>
#   scripts/pre_flash_validate.sh --package-only <tarball> [sysupgrade-prefix|board]
#   example: scripts/pre_flash_validate.sh 203.0.113.139 output/dcentos-sysupgrade-am2-s19j.tar
#   example: scripts/pre_flash_validate.sh --package-only output/dcentos-sysupgrade-am3-s19kpro.tar am3-s19k
#
# Exit codes:
#   0  All 8 gates passed - safe to run sysupgrade.
#   1  At least one gate failed - do NOT flash.
#
# This script performs ONE optional mutating operation: temporary ubiattach
# of the INACTIVE NAND slot (read-only mount-style attach) followed by
# immediate ubidetach. The active slot is never touched.
#
# Phase 5B Agent E. D-Central Technologies, 2026.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
. "$SCRIPT_DIR/lib/am3_geometry.sh"

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

pass() {
    echo "PASS: $1"
}

is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

is_release_status() {
    case "${1:-}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

manifest_string_field() {
    key=$1
    file=$2
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -n 1
}

manifest_payload_block() {
    path=$1
    file=$2
    awk -v path="$path" '
        BEGIN { RS = "}" }
        index($0, "\"path\"") && index($0, "\"" path "\"") {
            print $0 "}"
            found = 1
            exit
        }
        END { exit found ? 0 : 1 }
    ' "$file"
}

manifest_payload_number_matches() {
    path=$1
    field=$2
    expected=$3
    file=$4
    block=$(manifest_payload_block "$path" "$file") || return 1
    printf '%s\n' "$block" \
        | grep -Eq '"'$field'"[[:space:]]*:[[:space:]]*'"$expected"'([[:space:]]*[,}])'
}

payload_magic() {
    od -An -N4 -tx1 "$1" 2>/dev/null | tr -d ' \n'
}

ZYNQ_UBI_LEB_SIZE_BYTES=$((124 * 1024))
AM1_S9_KERNEL_MAX_BYTES=$((32 * ZYNQ_UBI_LEB_SIZE_BYTES))
AM1_S9_ROOTFS_MAX_BYTES=$((134 * ZYNQ_UBI_LEB_SIZE_BYTES))
AM2_ZYNQ_KERNEL_MAX_BYTES=$(((23 + 4) * ZYNQ_UBI_LEB_SIZE_BYTES))
AM2_ZYNQ_ROOTFS_MAX_BYTES=$((179 * ZYNQ_UBI_LEB_SIZE_BYTES))

assert_payload_fits_window() {
    label=$1
    size=$2
    max_size=$3
    window_name=$4

    case "$size" in ''|*[!0-9]*|0) fail "$label payload has invalid size (size=$size)" ;; esac
    case "$max_size" in ''|*[!0-9]*|0) fail "$label payload has invalid window (max=$max_size)" ;; esac
    [ "$size" -le "$max_size" ] \
        || fail "$label payload exceeds $window_name (${size}B > ${max_size}B)"
    pass "$label payload fits $window_name (${size}B <= ${max_size}B)"
}

validate_board_payload_profile() {
    board=$1
    kernel_path=$2
    root_path=$3
    root_size=$4
    kernel_size=$5

    case "$board" in
        am3-s19k|am3-s21)
            ROOT_MAGIC=$(payload_magic "$root_path")
            KERNEL_MAGIC=$(payload_magic "$kernel_path")
            [ "$ROOT_MAGIC" = "27051956" ] || fail "AM3 root payload is not a uImage (magic=$ROOT_MAGIC)"
            [ "$KERNEL_MAGIC" = "27051956" ] || fail "AM3 kernel payload is not a uImage (magic=$KERNEL_MAGIC)"
            pass "AM3 kernel/root uImage magic valid"
            [ "$root_size" -le "$DCENT_AM3_ROOTFS_WINDOW_DEC" ] \
                || fail "AM3 root payload exceeds am3 rootfs window (${root_size}B > ${DCENT_AM3_ROOTFS_WINDOW_DEC}B)"
            pass "AM3 root payload fits am3 rootfs window (${root_size}B <= ${DCENT_AM3_ROOTFS_WINDOW_DEC}B)"
            ;;
        am1-s9|am2-s19j|am2-s19jpro|am2-s17p)
            ROOT_MAGIC=$(payload_magic "$root_path")
            case "$ROOT_MAGIC" in
                68737173|73717368)
                    pass "squashfs-style root payload magic valid for $board"
                    ;;
                *)
                    fail "root payload for $board is not squashfs-style (magic=$ROOT_MAGIC)"
                    ;;
            esac
            pass "AM3 uImage/rootfs-window checks skipped for squashfs-style $board package"
            # Zynq kernel-container gate (BUG 1 fix, 2026-06-05): the S9/Zynq
            # NAND boot path uses U-Boot `bootm`, which boots ONLY a FIT
            # (d00dfeed) or a legacy uImage (27051956). A BARE ARM zImage
            # (first bytes "0000a0e1...", zImage magic 0x016f2818 at offset
            # 0x24) bricks the unit -- this is what bricked .135. The packager
            # now wraps the bare zImage into a FIT; this gate makes sure a
            # pre-FIT-fix (or hand-assembled) bare-zImage tarball can NEVER
            # reach a live flash.
            KERNEL_MAGIC=$(payload_magic "$kernel_path")
            case "$KERNEL_MAGIC" in
                d00dfeed)
                    pass "$board kernel payload is a bootable FIT (magic=$KERNEL_MAGIC)"
                    ;;
                27051956)
                    pass "$board kernel payload is a legacy uImage (magic=$KERNEL_MAGIC)"
                    ;;
                *)
                    fail "$board kernel payload is NOT a bootm-ready FIT/uImage (magic=$KERNEL_MAGIC) -- a bare zImage will brick the unit"
                    ;;
            esac
            case "$board" in
                am1-s9)
                    ZYNQ_KERNEL_MAX_BYTES=$AM1_S9_KERNEL_MAX_BYTES
                    ZYNQ_ROOTFS_MAX_BYTES=$AM1_S9_ROOTFS_MAX_BYTES
                    ;;
                *)
                    ZYNQ_KERNEL_MAX_BYTES=$AM2_ZYNQ_KERNEL_MAX_BYTES
                    ZYNQ_ROOTFS_MAX_BYTES=$AM2_ZYNQ_ROOTFS_MAX_BYTES
                    ;;
            esac
            assert_payload_fits_window "$board kernel" "$kernel_size" "$ZYNQ_KERNEL_MAX_BYTES" "zynq kernel window"
            assert_payload_fits_window "$board root" "$root_size" "$ZYNQ_ROOTFS_MAX_BYTES" "zynq rootfs window"
            ;;
        *)
            fail "no package payload profile for board '$board'"
            ;;
    esac
}

validate_package_only() {
    PACKAGE_TARBALL="${1:?usage: pre_flash_validate.sh --package-only <tarball> [sysupgrade-prefix|board]}"
    EXPECTED="${2:-sysupgrade-am3-s19k}"

    case "$EXPECTED" in
        sysupgrade-*) EXPECTED_PREFIX="$EXPECTED" ;;
        *) EXPECTED_PREFIX="sysupgrade-$EXPECTED" ;;
    esac
    EXPECTED_BOARD=${EXPECTED_PREFIX#sysupgrade-}

    echo "=== package-only sysupgrade validation: $PACKAGE_TARBALL (expected $EXPECTED_PREFIX/) ==="
    echo ""

    [ -f "$PACKAGE_TARBALL" ] || fail "tarball '$PACKAGE_TARBALL' not found on this host"
    command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required for package-only validation"

    TMPDIR_P=$(mktemp -d 2>/dev/null || echo "/tmp/preflash-package.$$")
    mkdir -p "$TMPDIR_P"
    trap 'rm -rf "$TMPDIR_P"' EXIT

    TAR_LIST="$TMPDIR_P/tar.list"
    UNSAFE_LIST="$TMPDIR_P/unsafe-tar-paths.txt"
    tar tf "$PACKAGE_TARBALL" > "$TAR_LIST" 2>/dev/null || fail "could not list package tarball"

    if awk '$0 == "" || $0 ~ /^\// || $0 ~ /(^|\/)\.\.(\/|$)/ { print; bad=1 } END { exit bad }' "$TAR_LIST" > "$UNSAFE_LIST"; then
        pass "tar entry paths are relative and traversal-free"
    else
        sed 's/^/  /' "$UNSAFE_LIST" >&2
        fail "tarball contains unsafe path(s)"
    fi

    TAR_TYPES="$TMPDIR_P/tar.types"
    UNSAFE_TYPES="$TMPDIR_P/unsafe-tar-types.txt"
    tar tvf "$PACKAGE_TARBALL" > "$TAR_TYPES" 2>/dev/null || fail "could not inspect package tar entry types"
    if awk '{ t = substr($1, 1, 1); if (t != "-" && t != "d") { print; bad=1 } } END { exit bad }' "$TAR_TYPES" > "$UNSAFE_TYPES"; then
        pass "tar entry types are regular files/directories only"
    else
        sed 's/^/  /' "$UNSAFE_TYPES" >&2
        fail "tarball contains unsupported entry type(s)"
    fi

    PREFIX=$(sed -n '1p' "$TAR_LIST" | cut -d/ -f1)
    if [ "$PREFIX" = "$EXPECTED_PREFIX" ]; then
        pass "tarball prefix = $EXPECTED_PREFIX/"
    else
        fail "tarball prefix '$PREFIX' != '$EXPECTED_PREFIX' - wrong package for this validation"
    fi

    if awk -v p="$EXPECTED_PREFIX/" '$0 !~ "^" p { bad=1 } END { exit bad }' "$TAR_LIST"; then
        pass "all tar entries stay under $EXPECTED_PREFIX/"
    else
        fail "tarball contains entries outside $EXPECTED_PREFIX/"
    fi

    tar -xf "$PACKAGE_TARBALL" -C "$TMPDIR_P" || fail "could not extract package"
    SUP_DIR="$TMPDIR_P/$EXPECTED_PREFIX"
    [ -d "$SUP_DIR" ] || fail "expected package directory missing: $EXPECTED_PREFIX"

    for entry in kernel root METADATA MANIFEST.json SHA256SUMS; do
        [ -f "$SUP_DIR/$entry" ] && [ ! -L "$SUP_DIR/$entry" ] || fail "package is missing regular file $entry"
    done
    pass "required sysupgrade payload files present"

    (cd "$SUP_DIR" && sha256sum -c SHA256SUMS >/dev/null) || fail "SHA256SUMS verification failed"
    pass "SHA256SUMS verifies kernel/root/METADATA"

    grep -F "\"board\": \"$EXPECTED_BOARD\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json board does not match $EXPECTED_BOARD"
    grep -F "\"board_target\": \"$EXPECTED_BOARD\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json board_target does not match $EXPECTED_BOARD"
    pass "MANIFEST.json board/board_target match $EXPECTED_BOARD"

    ROOT_SIZE=$(stat -c%s "$SUP_DIR/root" 2>/dev/null || stat -f%z "$SUP_DIR/root" 2>/dev/null || echo 0)
    KERNEL_SIZE=$(stat -c%s "$SUP_DIR/kernel" 2>/dev/null || stat -f%z "$SUP_DIR/kernel" 2>/dev/null || echo 0)
    case "$ROOT_SIZE:$KERNEL_SIZE" in
        *[!0-9:]*|0:*|*:0) fail "package payload sizes are invalid (kernel=$KERNEL_SIZE root=$ROOT_SIZE)" ;;
    esac
    pass "payload sizes are non-zero (kernel=${KERNEL_SIZE}B root=${ROOT_SIZE}B)"

    validate_board_payload_profile "$EXPECTED_BOARD" "$SUP_DIR/kernel" "$SUP_DIR/root" "$ROOT_SIZE" "$KERNEL_SIZE"

    KERNEL_SHA=$(sha256sum "$SUP_DIR/kernel" | awk '{ print $1 }')
    ROOT_SHA=$(sha256sum "$SUP_DIR/root" | awk '{ print $1 }')
    METADATA_SHA=$(sha256sum "$SUP_DIR/METADATA" | awk '{ print $1 }')
    grep -F "\"path\": \"$EXPECTED_PREFIX/kernel\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json kernel path does not match $EXPECTED_PREFIX/kernel"
    grep -F "\"path\": \"$EXPECTED_PREFIX/root\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json rootfs path does not match $EXPECTED_PREFIX/root"
    grep -F "\"path\": \"$EXPECTED_PREFIX/METADATA\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json metadata path does not match $EXPECTED_PREFIX/METADATA"
    manifest_payload_number_matches "$EXPECTED_PREFIX/kernel" size "$KERNEL_SIZE" "$SUP_DIR/MANIFEST.json" \
        || fail "MANIFEST.json kernel size does not match ${KERNEL_SIZE}"
    manifest_payload_number_matches "$EXPECTED_PREFIX/root" size "$ROOT_SIZE" "$SUP_DIR/MANIFEST.json" \
        || fail "MANIFEST.json rootfs size does not match ${ROOT_SIZE}"
    grep -F "\"sha256\": \"$KERNEL_SHA\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json kernel sha256 does not match $KERNEL_SHA"
    grep -F "\"sha256\": \"$ROOT_SHA\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json rootfs sha256 does not match $ROOT_SHA"
    grep -F "\"sha256\": \"$METADATA_SHA\"" "$SUP_DIR/MANIFEST.json" >/dev/null 2>&1 \
        || fail "MANIFEST.json metadata sha256 does not match $METADATA_SHA"
    pass "MANIFEST.json payload paths/sizes/hashes match actual files"

    MANIFEST_STATUS=$(manifest_string_field status "$SUP_DIR/MANIFEST.json" || echo)
    if is_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
        if [ -f "$SUP_DIR/MANIFEST.sig" ] && [ -f "$SUP_DIR/release_ed25519.pub" ] && [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
            sh "$SCRIPT_DIR/verify_sysupgrade_signature.sh" "$PACKAGE_TARBALL" "$DCENT_RELEASE_PUBKEY_FILE" "$EXPECTED_BOARD" >/dev/null \
                || fail "release signature verification failed"
            pass "release signature verified against trusted key"
        else
            if is_release_status "$MANIFEST_STATUS"; then
                fail "manifest status '$MANIFEST_STATUS' is release; unsigned/generated-key lab override requires a non-release package status"
            fi
            if [ -f "$SUP_DIR/MANIFEST.sig" ] && [ -f "$SUP_DIR/release_ed25519.pub" ]; then
                pass "package-embedded generated-key signature accepted only because DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1"
            else
                pass "unsigned package accepted only because DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1"
            fi
        fi
    else
        [ -f "$SUP_DIR/MANIFEST.sig" ] || fail "MANIFEST.sig missing; set DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages"
        [ -f "$SUP_DIR/release_ed25519.pub" ] || fail "release_ed25519.pub missing; set DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages"
        [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ] || fail "DCENT_RELEASE_PUBKEY_FILE is required for release package validation"
        sh "$SCRIPT_DIR/verify_sysupgrade_signature.sh" "$PACKAGE_TARBALL" "$DCENT_RELEASE_PUBKEY_FILE" "$EXPECTED_BOARD" >/dev/null \
            || fail "release signature verification failed"
        pass "release signature verified against trusted key"
    fi

    echo ""
    echo "PACKAGE-ONLY VALIDATION PASSED"
    exit 0
}

if [ "${1:-}" = "--package-only" ]; then
    validate_package_only "${2:-}" "${3:-sysupgrade-am3-s19k}"
fi

# -----------------------------------------------------------------------------
# AM1 (S9) backup-floor gate.
#
# am1-s9 was previously the only NAND-flashing platform without a backup
# ritual gate (am2 has 5 backup scripts, am3-bb has 4). Phase 1D of the
# DCENT_OS preparedness sweep adds 4 am1 backup scripts and requires the
# operator to run them (or explicitly skip with --skip-am1-backup) before
# any am1 NAND flash is authorized.
#
# This gate runs ONLY when the target is detected as am1-s9 (platform
# string "zynq-bm1-s9" or "am1-s9"). The existing am2 path below is
# untouched for backward compatibility.
# -----------------------------------------------------------------------------
SKIP_AM1_BACKUP=0
for arg in "$@"; do
    if [ "$arg" = "--skip-am1-backup" ]; then
        SKIP_AM1_BACKUP=1
        break
    fi
done

validate_am1_backup_floor() {
    miner="$1"
    echo "=== am1-s9 backup-floor gate ==="

    if [ "$SKIP_AM1_BACKUP" = "1" ]; then
        echo "WARN: --skip-am1-backup is set - am1 backup floor BYPASSED (lab override)"
        echo "      DO NOT use this on production fleets."
        return 0
    fi

    # Look for a fresh result manifest for this exact target IP.
    backup_root="$SCRIPT_DIR/../output/am1-backups"
    safe_ip=$(printf '%s' "$miner" | tr -c 'A-Za-z0-9_.=-' '-')
    candidate_glob="$backup_root/${safe_ip}-*/am1_nand_backup_${safe_ip}_*.manifest.json"
    latest_manifest=$(ls -t $candidate_glob 2>/dev/null | head -n 1)

    if [ -z "$latest_manifest" ] || [ ! -f "$latest_manifest" ]; then
        fail "am1-backup-floor: no result manifest found for $miner under $backup_root.
        Run:  scripts/am1_nand_backup_execute.sh --target $miner --plan <plan.json>
        Or pass --skip-am1-backup for lab-only override (NOT for production)."
    fi

    # Validate the manifest before trusting it.
    if ! sh "$SCRIPT_DIR/am1_nand_backup_manifest.sh" --validate \
            --manifest "$latest_manifest" >/dev/null 2>&1; then
        fail "am1-backup-floor: manifest $latest_manifest failed validation. Re-run the am1 backup ritual."
    fi

    # Reject manifests older than 24 hours - a stale backup is not proof
    # against the post-W14 inactive-slot churn that prompted this gate.
    manifest_mtime=$(stat -c %Y "$latest_manifest" 2>/dev/null || stat -f %m "$latest_manifest" 2>/dev/null || echo 0)
    now_epoch=$(date +%s)
    age_hr=$(( (now_epoch - manifest_mtime) / 3600 ))
    if [ "$age_hr" -ge 24 ]; then
        fail "am1-backup-floor: manifest $latest_manifest is ${age_hr}h old (>= 24h). Re-run am1 backup ritual."
    fi

    echo "PASS am1-backup-floor: validated manifest $latest_manifest (${age_hr}h old)"
}

MINER="${1:?usage: pre_flash_validate.sh <miner-ip> <tarball>}"
TARBALL="${2:?missing tarball argument}"

SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 -o LogLevel=ERROR"
SSH="ssh $SSH_OPTS root@$MINER"

echo "=== pre-flash validation: $MINER <- $TARBALL ==="
echo ""

# -----------------------------------------------------------------------------
# Gate 0: Tarball file exists locally (before we even touch the miner).
# Not one of the 8 numbered gates, but a cheap sanity check.
# -----------------------------------------------------------------------------
[ -f "$TARBALL" ] || fail "tarball '$TARBALL' not found on this host"

# -----------------------------------------------------------------------------
# Gate 1: SSH reachable.
# -----------------------------------------------------------------------------
if $SSH 'echo ok' >/dev/null 2>&1; then
    echo "PASS 1/8: SSH reachable to root@$MINER"
else
    fail "1/8 SSH unreachable to root@$MINER - check network, credentials, and that miner is powered on"
fi

# -----------------------------------------------------------------------------
# Gate 2: Platform identity must be zynq-bm3-am2 (Phase 5 auto-route trigger).
#
# am1-s9 short-circuit (Phase 1D, 2026-05-15): if the platform reports as
# am1-s9 / zynq-bm1-s9, we run the dedicated am1 backup-floor gate INSTEAD
# of failing here, but we still don't run the am2-specific gates 3-8 (the
# am1 NAND layout has different mtd numbers and no UBI A/B that we can
# probe with this script's am2-specific LEB template).
# -----------------------------------------------------------------------------
PLATFORM=$($SSH 'cat /etc/bos_platform 2>/dev/null' 2>/dev/null || echo 'missing')
case "$PLATFORM" in
    zynq-bm3-am2)
        echo "PASS 2/8: /etc/bos_platform = zynq-bm3-am2"
        ;;
    zynq-bm1-s9|am1-s9)
        echo "DETECTED: am1-s9 target (/etc/bos_platform=$PLATFORM)"
        validate_am1_backup_floor "$MINER"
        echo ""
        echo "=============================================================="
        echo "  AM1-S9 BACKUP-FLOOR GATE PASSED"
        echo "  miner:   $MINER  (platform=$PLATFORM)"
        echo "  tarball: $TARBALL"
        echo "=============================================================="
        echo "  This script's am2-specific gates 3-8 do not apply to am1-s9."
        echo "  CE-352: validating the am1-s9 package before declaring pre-flash"
        echo "  success (fail-closed precondition, not a manual afterthought)."
        echo "=============================================================="
        # validate_package_only re-derives the sysupgrade-am1-s9/ prefix from
        # "am1-s9" and fails closed (exit 1 via fail()) on wrong prefix / unsafe
        # tar paths or entry types / missing payload files / SHA256SUMS or
        # MANIFEST mismatch / non-FIT/uImage kernel magic / non-squashfs root /
        # oversized kernel-or-rootfs / unsigned-or-untrusted signature. It exits 0
        # only after every am1-s9 package gate passes. Same signature contract as
        # the am2 gate 8a (DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 + DCENT_RELEASE_PUBKEY_FILE
        # for lab overrides).
        validate_package_only "$TARBALL" "am1-s9"
        ;;
    *)
        fail "2/8 platform '$PLATFORM' != zynq-bm3-am2 - wrong miner, wrong firmware, or not an am2 S19j Pro"
        ;;
esac

# -----------------------------------------------------------------------------
# Gate 3: Identify active firmware slot.
# BraiinsOS/DCENT_OS stores the active slot in the U-Boot env var `firmware`
# (value "1" or "2"). mtd7 = slot 1, mtd8 = slot 2.
# -----------------------------------------------------------------------------
ACTIVE=$($SSH 'fw_printenv firmware 2>/dev/null | cut -d= -f2' 2>/dev/null || echo '')
case "$ACTIVE" in
    1) ACTIVE_MTD=7; INACTIVE=2; INACTIVE_MTD=8 ;;
    2) ACTIVE_MTD=8; INACTIVE=1; INACTIVE_MTD=7 ;;
    *) fail "3/8 could not identify active firmware slot (fw_printenv firmware='$ACTIVE')" ;;
esac
echo "PASS 3/8: active slot=$ACTIVE (mtd$ACTIVE_MTD), inactive=$INACTIVE (mtd$INACTIVE_MTD)"

# -----------------------------------------------------------------------------
# Gate 4: Attach inactive UBI slot read-only and confirm it parses.
# We detach immediately - this is a sanity probe, not a mount. If the inactive
# slot is corrupted, sysupgrade will write into it anyway, but we want to warn
# the operator if something looks suspicious first.
# -----------------------------------------------------------------------------
# ubiattach returns 0 on success. We tolerate "already attached" by detaching
# first. UBI device numbers are arbitrary but we use 3 to avoid collisions
# with the active runtime (usually ubi0/ubi1).
UBI_PROBE_NUM=3
$SSH "ubidetach -d $UBI_PROBE_NUM 2>/dev/null || true" >/dev/null 2>&1
ATTACH_OUT=$($SSH "ubiattach -m $INACTIVE_MTD -d $UBI_PROBE_NUM 2>&1" 2>/dev/null || echo 'attach failed')
if echo "$ATTACH_OUT" | grep -q 'UBI device number'; then
    echo "PASS 4/8: inactive UBI (mtd$INACTIVE_MTD) attached as ubi$UBI_PROBE_NUM"
    # Read volume layout while attached (gate 5 needs this).
    VOL_LIST=$($SSH "ls /sys/class/ubi/ubi${UBI_PROBE_NUM}_* 2>/dev/null | wc -l" 2>/dev/null || echo 0)
    # Capture LEB counts and usable LEB size for each volume for gate 5.
    LEB_REPORT=$($SSH "for v in /sys/class/ubi/ubi${UBI_PROBE_NUM}_*; do [ -d \"\$v\" ] && printf '%s:%s:%s ' \"\$(cat \$v/name 2>/dev/null)\" \"\$(cat \$v/reserved_ebs 2>/dev/null)\" \"\$(cat \$v/usable_eb_size 2>/dev/null)\"; done" 2>/dev/null || echo '')
    $SSH "ubidetach -d $UBI_PROBE_NUM 2>/dev/null || true" >/dev/null 2>&1
else
    fail "4/8 ubiattach -m $INACTIVE_MTD failed: $ATTACH_OUT"
fi

# -----------------------------------------------------------------------------
# Gate 5: UBI volume layout matches 23/179/210 LEB template for am2-s19j.
# Reference values documented in memory: S9 is 25/166/525; am2-s19j is
# 23/179/210 (kernel/rootfs/rootfs_data). Tolerate +/-2 LEBs per volume
# since BraiinsOS versions have shifted by 1-2 LEBs historically.
# -----------------------------------------------------------------------------
if [ -z "$LEB_REPORT" ]; then
    fail "5/8 could not read inactive UBI volume layout"
fi

get_ebs() {
    printf '%s\n' "$LEB_REPORT" | tr ' ' '\n' | awk -F: -v name="$1" '$1 == name { print $2; exit }'
}

get_leb_size() {
    printf '%s\n' "$LEB_REPORT" | tr ' ' '\n' | awk -F: -v name="$1" '$1 == name { print $3; exit }'
}

require_uint() {
    case "$2" in
        ''|*[!0-9]*) fail "5/8 $1 is not numeric ('$2')" ;;
    esac
}

check_leb_range() {
    NAME="$1"
    VALUE="$2"
    EXPECTED="$3"
    TOLERANCE="$4"
    require_uint "$NAME reserved_ebs" "$VALUE"
    MIN=$((EXPECTED - TOLERANCE))
    MAX=$((EXPECTED + TOLERANCE))
    if [ "$VALUE" -lt "$MIN" ] || [ "$VALUE" -gt "$MAX" ]; then
        fail "5/8 inactive UBI $NAME reserved_ebs=$VALUE outside expected range ${MIN}-${MAX}"
    fi
}

KERNEL_EBS=$(get_ebs kernel)
[ -n "$KERNEL_EBS" ] || KERNEL_EBS=$(get_ebs boot)
ROOTFS_EBS=$(get_ebs rootfs)
ROOTFS_LEB_SIZE=$(get_leb_size rootfs)
DATA_EBS=$(get_ebs rootfs_data)
[ -n "$DATA_EBS" ] || DATA_EBS=$(get_ebs fwupdate)

check_leb_range "kernel" "$KERNEL_EBS" 23 2
check_leb_range "rootfs" "$ROOTFS_EBS" 179 2
check_leb_range "rootfs_data" "$DATA_EBS" 210 2
require_uint "rootfs usable_eb_size" "$ROOTFS_LEB_SIZE"

PACKAGE_ROOT_ENTRY=$(tar tf "$TARBALL" 2>/dev/null | awk '/\/root$/ { print; exit }')
[ -n "$PACKAGE_ROOT_ENTRY" ] || fail "5/8 sysupgrade package is missing root payload"
PACKAGE_ROOT_SIZE=$(tar tvf "$TARBALL" "$PACKAGE_ROOT_ENTRY" 2>/dev/null | awk '{ print $3; exit }')
require_uint "packaged rootfs size" "$PACKAGE_ROOT_SIZE"

ROOTFS_CAPACITY=$((ROOTFS_EBS * ROOTFS_LEB_SIZE))
if [ "$PACKAGE_ROOT_SIZE" -gt "$ROOTFS_CAPACITY" ]; then
    fail "5/8 packaged rootfs ($PACKAGE_ROOT_SIZE bytes) exceeds inactive rootfs capacity ($ROOTFS_CAPACITY bytes)"
fi

echo "PASS 5/8: inactive UBI volumes: $LEB_REPORT"
echo "        rootfs payload ${PACKAGE_ROOT_SIZE}B <= inactive capacity ${ROOTFS_CAPACITY}B"
# -----------------------------------------------------------------------------
# Gate 6: Tarball prefix matches `sysupgrade-am2-s19j/`.
# This is the single most important sanity check before flashing - an am1
# or am3 tarball flashed into an am2 slot bricks the unit.
# -----------------------------------------------------------------------------
PREFIX=$(tar tf "$TARBALL" 2>/dev/null | head -1 | cut -d/ -f1)
if [ "$PREFIX" = "sysupgrade-am2-s19j" ]; then
    echo "PASS 6/8: tarball prefix = sysupgrade-am2-s19j/"
else
    fail "6/8 tarball prefix '$PREFIX' != sysupgrade-am2-s19j - WRONG PLATFORM, do NOT flash"
fi

# -----------------------------------------------------------------------------
# Gate 7: Tarball mtime freshness. If it's more than 6 hours old the operator
# probably forgot to rebuild after a code change - warn hard.
# -----------------------------------------------------------------------------
NOW=$(date +%s)
MTIME=$(stat -c %Y "$TARBALL" 2>/dev/null || stat -f %m "$TARBALL" 2>/dev/null || echo 0)
if [ "$MTIME" -eq 0 ]; then
    echo "WARN 7/8: could not stat tarball mtime - skipping freshness check"
else
    AGE_HR=$(( (NOW - MTIME) / 3600 ))
    if [ "$AGE_HR" -lt 6 ]; then
        echo "PASS 7/8: tarball mtime ${AGE_HR}h old (<6h)"
    else
        fail "7/8 tarball is ${AGE_HR}h old - obtain a fresh capsule-built or independently approved signed artifact"
    fi
fi

# -----------------------------------------------------------------------------
# Gate 8: Ed25519 manifest signature + dcentrald binary freshness.
#
# Two sub-checks:
#   (8a) Ed25519 signature on MANIFEST.json - wired through to mirror the
#        existing AM3 signing flow per DevOps Q1 finding 4I (2026-05-15).
#        Uses the same DCENT_ALLOW_UNSIGNED_SYSUPGRADE / DCENT_RELEASE_PUBKEY_FILE
#        contract as `validate_package_only` above.
#   (8b) Dcentrald binary freshness - the original Gate 8 check.
# -----------------------------------------------------------------------------
# gate_8a_package_signed() - Ed25519 sysupgrade signature check.
#
# Phase 1E (2026-05-15) refactor: the inline block that was here before is
# now a named function so Phase 1E's planner gate has a corresponding
# in-script gate to point at. Behavior unchanged.
#
# Args: $1 = tarball path, $2 = prefix dir name (e.g. "sysupgrade-am2-s19j"),
#       $3 = board identifier (e.g. "am2-s19j") for verify_sysupgrade_signature.sh.
gate_8a_package_signed() {
    g8a_tarball="$1"
    g8a_prefix="$2"
    g8a_board="$3"
    echo "Gate 8a: ${g8a_board} sysupgrade Ed25519 signature"
    SIG_TMPDIR=$(mktemp -d 2>/dev/null || echo "/tmp/preflash-sig.$$")
    mkdir -p "$SIG_TMPDIR"
    if ! tar -xf "$g8a_tarball" -C "$SIG_TMPDIR" 2>/dev/null; then
        rm -rf "$SIG_TMPDIR"
        fail "8a/8 could not extract tarball for signature inspection"
    fi
    SIG_DIR="$SIG_TMPDIR/$g8a_prefix"
    if [ ! -d "$SIG_DIR" ]; then
        rm -rf "$SIG_TMPDIR"
        fail "8a/8 sysupgrade prefix dir missing after extract: $g8a_prefix"
    fi
    if is_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
        if [ -f "$SIG_DIR/MANIFEST.sig" ] && [ -f "$SIG_DIR/release_ed25519.pub" ] && \
           [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
            if sh "$SCRIPT_DIR/verify_sysupgrade_signature.sh" "$g8a_tarball" \
                    "$DCENT_RELEASE_PUBKEY_FILE" "$g8a_board" >/dev/null 2>&1; then
                echo "PASS 8a/8: release signature verified against trusted key (${g8a_board})"
            else
                rm -rf "$SIG_TMPDIR"
                fail "8a/8 ${g8a_board} sysupgrade signature verification failed (trusted key)"
            fi
        else
            MAN_STATUS=$(manifest_string_field status "$SIG_DIR/MANIFEST.json" 2>/dev/null || echo)
            if is_release_status "$MAN_STATUS"; then
                rm -rf "$SIG_TMPDIR"
                fail "8a/8 manifest status '$MAN_STATUS' is release; lab override requires non-release status"
            fi
            echo "PASS 8a/8: unsigned ${g8a_board} package accepted (DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1, lab status)"
        fi
    else
        if [ ! -f "$SIG_DIR/MANIFEST.sig" ]; then
            rm -rf "$SIG_TMPDIR"
            fail "8a/8 MANIFEST.sig missing; set DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages"
        fi
        if [ ! -f "$SIG_DIR/release_ed25519.pub" ]; then
            rm -rf "$SIG_TMPDIR"
            fail "8a/8 release_ed25519.pub missing; set DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages"
        fi
        if [ -z "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
            rm -rf "$SIG_TMPDIR"
            fail "8a/8 DCENT_RELEASE_PUBKEY_FILE is required for release ${g8a_board} package validation"
        fi
        if sh "$SCRIPT_DIR/verify_sysupgrade_signature.sh" "$g8a_tarball" \
                "$DCENT_RELEASE_PUBKEY_FILE" "$g8a_board" >/dev/null 2>&1; then
            echo "PASS 8a/8: release signature verified against trusted key (${g8a_board})"
        else
            rm -rf "$SIG_TMPDIR"
            fail "8a/8 ${g8a_board} sysupgrade signature verification failed (trusted key)"
        fi
    fi
    rm -rf "$SIG_TMPDIR"
}

# Phase 1E (2026-05-15) - call the named gate_8a_package_signed function
# between gate 7 (tarball mtime freshness) and gate 8b (binary freshness).
gate_8a_package_signed "$TARBALL" "$PREFIX" "am2-s19j"

echo "Gate 8b: dcentrald binary freshness"
BIN_PATH_IN_TAR=$(tar tf "$TARBALL" 2>/dev/null | grep -E 'usr/local/bin/dcentrald$' | head -1 || true)
if [ -z "$BIN_PATH_IN_TAR" ]; then
    fail "8b/8 dcentrald binary not found in tarball at usr/local/bin/dcentrald"
fi
TMPDIR_P=$(mktemp -d 2>/dev/null || echo "/tmp/preflash.$$")
mkdir -p "$TMPDIR_P"
# Extract just the binary. tar -C changes dir; --strip-components=0 keeps path.
if tar -xf "$TARBALL" -C "$TMPDIR_P" "$BIN_PATH_IN_TAR" 2>/dev/null; then
    BIN_MTIME=$(stat -c %Y "$TMPDIR_P/$BIN_PATH_IN_TAR" 2>/dev/null || stat -f %m "$TMPDIR_P/$BIN_PATH_IN_TAR" 2>/dev/null || echo 0)
    if [ "$BIN_MTIME" -eq 0 ]; then
        echo "WARN 8b/8: could not stat extracted binary mtime"
    else
        BIN_AGE_HR=$(( (NOW - BIN_MTIME) / 3600 ))
        if [ "$BIN_AGE_HR" -lt 24 ]; then
            echo "PASS 8b/8: dcentrald binary mtime ${BIN_AGE_HR}h old (<24h)"
        else
            rm -rf "$TMPDIR_P"
            fail "8b/8 dcentrald binary is ${BIN_AGE_HR}h old - rebuild triggered stale overlay copy (see feedback_buildroot_post_build_dcentrald_hook.md)"
        fi
    fi
    rm -rf "$TMPDIR_P"
else
    rm -rf "$TMPDIR_P"
    fail "8b/8 could not extract $BIN_PATH_IN_TAR from tarball"
fi

echo ""
echo "=============================================================="
echo "  ALL 8 PRE-FLASH GATES PASSED"
echo "  miner:   $MINER  (platform=$PLATFORM)"
echo "  active:  slot $ACTIVE (mtd$ACTIVE_MTD)"
echo "  target:  slot $INACTIVE (mtd$INACTIVE_MTD)"
echo "  tarball: $TARBALL"
echo "=============================================================="
echo "  Safe to run: sysupgrade $TARBALL on $MINER"
echo "=============================================================="
exit 0
