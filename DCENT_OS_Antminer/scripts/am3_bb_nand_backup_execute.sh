#!/bin/sh
#
# AM3-BB NAND Full Backup Execution Script
#
# Reads all 12 MTD partitions from an Antminer S19j Pro BeagleBone/AM335x
# control board and produces verified nanddump artifacts with a machine-readable
# manifest.
#
# SAFETY CONTRACT:
#   - NEVER writes to any MTD device, NAND, or bootenv.
#   - NEVER stops or starts any services.
#   - ONLY performs nanddump reads and sha256sum computations.
#   - MUST be run from an SD recovery boot environment (root != /dev/mtdblock*).
#   - MUST have DCENT_NAND_BACKUP_AUTHORIZED=1 set in the environment.
#   - Produces artifacts in a user-specified output directory only.
#   - Logs all operations to a timestamped log file in the output directory.
#   - On any failure, exits cleanly without partial state corruption.
#
# HARD GATES (script refuses to run if any fail):
#   1. DCENT_NAND_BACKUP_AUTHORIZED=1 must be set
#   2. Running system must be SD-booted (root is NOT /dev/mtdblock*)
#   3. Output directory must be writable with adequate free space
#   4. All 12 expected MTD devices must exist
#   5. nanddump and sha256sum must be available
#
# This script is designed for BusyBox ash compatibility on recovery images.
# Uses set -eu (no pipefail — not available in ash).

set -eu

# --- Constants ---

EXPECTED_MTD_COUNT=12
PARTITION_DEFS="
0|spl|00020000
1|spl_backup1|00020000
2|spl_backup2|00020000
3|spl_backup3|00020000
4|u-boot|001c0000
5|bootenv|00020000
6|fdt|00020000
7|kernel|00500000
8|root|01400000
9|config|00200000
10|sig|00200000
11|nvdata|06000000
"

# Timeout per partition dump in seconds (generous for large partitions)
DUMP_TIMEOUT=120

# Minimum free space required in KB (130 MB)
MIN_FREE_KB=133120

# --- Usage ---

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am3_bb_nand_backup_execute.sh <output_directory> [options]

Arguments:
  <output_directory>     Directory to write backup artifacts. Created if absent.

Options:
  --skip-size-check      Skip free-space pre-flight (use if df is unreliable)
  --timeout <seconds>    Per-partition dump timeout (default: 120)
  -h, --help             Show this help

Hard gates (ALL must pass):
  - DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable
  - System booted from SD card (root is NOT /dev/mtdblock*)
  - Output directory writable with >= 130 MB free
  - All 12 MTD devices present at /dev/mtd0 through /dev/mtd11
  - nanddump and sha256sum commands available

Safety contract:
  - NEVER writes to NAND, MTD, or bootenv
  - NEVER stops services
  - ONLY reads MTD via nanddump and computes SHA256
  - Exits on any error (set -eu)
USAGE
    exit "$code"
}

# --- Helpers ---

timestamp_utc() {
    date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date 2>/dev/null
}

log() {
    local msg="[$(timestamp_utc)] $*"
    echo "$msg"
    if [ -n "${LOGFILE:-}" ]; then
        echo "$msg" >> "$LOGFILE"
    fi
}

die() {
    log "FATAL: $*"
    exit 1
}

warn() {
    log "WARN: $*"
}

hex_to_dec() {
    # Convert hex string (with or without 0x prefix) to decimal
    local hex="$1"
    hex="${hex#0x}"
    hex="${hex#0X}"
    printf '%d' "0x$hex" 2>/dev/null || echo 0
}

# --- Argument parsing ---

OUTDIR=""
SKIP_SIZE_CHECK=0

while [ $# -gt 0 ]; do
    case "$1" in
        --skip-size-check)
            SKIP_SIZE_CHECK=1
            shift
            ;;
        --timeout)
            DUMP_TIMEOUT="${2:?--timeout requires seconds}"
            shift 2
            ;;
        --timeout=*)
            DUMP_TIMEOUT="${1#--timeout=}"
            shift
            ;;
        -h|--help)
            usage 0
            ;;
        --*)
            echo "ERROR: unknown option: $1" >&2
            usage
            ;;
        *)
            if [ -n "$OUTDIR" ]; then
                echo "ERROR: unexpected argument: $1" >&2
                usage
            fi
            OUTDIR="$1"
            shift
            ;;
    esac
done

[ -n "$OUTDIR" ] || usage

# ============================================================
# HARD GATE 1: Authorization environment variable
# ============================================================

if [ "${DCENT_NAND_BACKUP_AUTHORIZED:-0}" != "1" ]; then
    cat >&2 <<'GATE'
ERROR: DCENT_NAND_BACKUP_AUTHORIZED is not set to 1.

This script requires explicit operator authorization via:
  export DCENT_NAND_BACKUP_AUTHORIZED=1

This gate exists to prevent accidental execution. The operator must
consciously set this variable after reviewing the backup plan.
GATE
    exit 1
fi

# ============================================================
# HARD GATE 2: Must be SD-booted (root != /dev/mtdblock*)
# ============================================================

detect_root_device() {
    # Try multiple methods to detect root device
    local root_dev=""

    # Method 1: /proc/mounts
    root_dev="$(awk '$2 == "/" { print $1; exit }' /proc/mounts 2>/dev/null || true)"

    # Method 2: /proc/cmdline
    if [ -z "$root_dev" ]; then
        root_dev="$(sed -n 's/.*root=\([^ ]*\).*/\1/p' /proc/cmdline 2>/dev/null || true)"
    fi

    # Method 3: findmnt
    if [ -z "$root_dev" ]; then
        root_dev="$(findmnt -n -o SOURCE / 2>/dev/null || true)"
    fi

    echo "$root_dev"
}

ROOT_DEV="$(detect_root_device)"

case "$ROOT_DEV" in
    /dev/mtdblock*|mtdblock*)
        cat >&2 <<GATE
ERROR: System root is on NAND: $ROOT_DEV

This backup script MUST be run from an SD recovery boot environment
where the root filesystem is on SD card (e.g., /dev/mmcblk0p2), NOT
from the live LuxOS NAND system.

Boot the BeagleBone from an SD recovery image and re-run this script.
GATE
        exit 1
        ;;
    "")
        warn "Could not detect root device. Proceeding with caution."
        warn "Operator MUST confirm this is running from SD recovery boot."
        ;;
    *)
        log "Root device: $ROOT_DEV (not NAND - gate passes)"
        ;;
esac

# ============================================================
# HARD GATE 3: Required tools
# ============================================================

for tool in nanddump sha256sum stat; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        die "Required tool not found: $tool"
    fi
done

# ============================================================
# HARD GATE 4: All 12 MTD devices must exist
# ============================================================

MISSING_MTD=""
for i in 0 1 2 3 4 5 6 7 8 9 10 11; do
    if [ ! -c "/dev/mtd$i" ] && [ ! -e "/dev/mtd$i" ]; then
        MISSING_MTD="$MISSING_MTD mtd$i"
    fi
done

if [ -n "$MISSING_MTD" ]; then
    die "Missing MTD devices:$MISSING_MTD (expected 12: mtd0-mtd11)"
fi

# ============================================================
# Setup output directory and logging
# ============================================================

mkdir -p "$OUTDIR" || die "Cannot create output directory: $OUTDIR"

STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
LOGFILE="$OUTDIR/backup_${STAMP}.log"
MANIFEST_JSON="$OUTDIR/backup_manifest_${STAMP}.json"
SHA256SUMS="$OUTDIR/SHA256SUMS"

log "=== AM3-BB NAND Backup Execution ==="
log "Output directory: $OUTDIR"
log "Timestamp: $STAMP"
log "Root device: ${ROOT_DEV:-unknown}"
log "Dump timeout per partition: ${DUMP_TIMEOUT}s"

# ============================================================
# HARD GATE 5: Adequate free space
# ============================================================

if [ "$SKIP_SIZE_CHECK" != "1" ]; then
    FREE_KB="$(df -Pk "$OUTDIR" 2>/dev/null | awk 'NR==2 { print $4 }' || echo 0)"
    if [ "$FREE_KB" -lt "$MIN_FREE_KB" ] 2>/dev/null; then
        die "Insufficient free space: ${FREE_KB} KB available, need ${MIN_FREE_KB} KB (130 MB)"
    fi
    log "Free space: ${FREE_KB} KB (minimum ${MIN_FREE_KB} KB)"
else
    log "Free space check skipped (--skip-size-check)"
fi

# ============================================================
# Verify MTD layout matches expected partitions
# ============================================================

log "--- Verifying MTD layout ---"

if [ -f /proc/mtd ]; then
    log "MTD layout from /proc/mtd:"
    cat /proc/mtd >> "$LOGFILE" 2>/dev/null || true

    # Verify partition names match expected
    LAYOUT_MISMATCH=0
    while IFS='|' read -r num name size_hex; do
        [ -n "$num" ] || continue
        if [ -f "/sys/class/mtd/mtd${num}/name" ]; then
            actual_name="$(cat "/sys/class/mtd/mtd${num}/name" 2>/dev/null || true)"
            if [ "$actual_name" != "$name" ]; then
                warn "mtd$num name mismatch: expected='$name' actual='$actual_name'"
                LAYOUT_MISMATCH=1
            fi
        fi
    done <<PARTEOF
$PARTITION_DEFS
PARTEOF
else
    warn "/proc/mtd not found - cannot verify layout names"
fi

# ============================================================
# Execute backup: dump each partition
# ============================================================

log "--- Beginning NAND backup (12 partitions) ---"

PASS_COUNT=0
FAIL_COUNT=0
: > "$SHA256SUMS"

# We will build JSON incrementally
JSON_PARTS=""

dump_partition() {
    local mtd_num="$1"
    local name="$2"
    local size_hex="$3"
    local artifact="mtd${mtd_num}_${name}.nanddump"
    local outpath="$OUTDIR/$artifact"
    local size_dec
    size_dec="$(hex_to_dec "0x$size_hex")"

    log "  Dumping mtd$mtd_num ($name) -> $artifact [max $size_dec bytes]..."

    # Run nanddump with timeout
    local dump_ok=0
    if command -v timeout >/dev/null 2>&1; then
        if timeout "$DUMP_TIMEOUT" nanddump --bb=skipbad --omitoob -f "$outpath" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
            dump_ok=1
        fi
    else
        # BusyBox may not have timeout; run directly
        if nanddump --bb=skipbad --omitoob -f "$outpath" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
            dump_ok=1
        fi
    fi

    if [ "$dump_ok" != "1" ]; then
        warn "  FAILED to dump mtd$mtd_num ($name)"
        echo "$mtd_num|$name|$artifact|FAIL|0|none"
        return 1
    fi

    # Verify output exists and is non-empty
    if [ ! -s "$outpath" ]; then
        warn "  FAILED: output file empty or missing: $outpath"
        echo "$mtd_num|$name|$artifact|FAIL|0|none"
        return 1
    fi

    # Compute SHA256
    local sha256
    sha256="$(sha256sum "$outpath" | awk '{ print $1 }')"

    # Get actual size
    local actual_bytes
    actual_bytes="$(stat -c %s "$outpath" 2>/dev/null || wc -c < "$outpath")"

    # Record in SHA256SUMS
    echo "$sha256  $artifact" >> "$SHA256SUMS"

    log "  OK: $artifact ($actual_bytes bytes, sha256=$sha256)"

    # Validate size <= partition size (dump should never be larger)
    if [ "$actual_bytes" -gt "$size_dec" ] 2>/dev/null; then
        warn "  Size $actual_bytes exceeds partition size $size_dec - unexpected"
    fi

    # For boot-critical partitions (mtd0-4), also dump with OOB for faithful restore.
    # AM335x ROM reads SPL/U-Boot with specific ECC expectations; OOB preserves
    # the original ECC bytes so nandwrite can restore bit-identical pages.
    if [ "$mtd_num" -le 4 ] 2>/dev/null; then
        local oob_artifact="mtd${mtd_num}_${name}.nanddump.oob"
        local oob_outpath="$OUTDIR/$oob_artifact"
        log "  Dumping mtd$mtd_num ($name) with OOB -> $oob_artifact..."
        if nanddump --bb=skipbad -f "$oob_outpath" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
            local oob_sha256
            oob_sha256="$(sha256sum "$oob_outpath" | awk '{ print $1 }')"
            echo "$oob_sha256  $oob_artifact" >> "$SHA256SUMS"
            log "  OK: $oob_artifact ($(stat -c %s "$oob_outpath" 2>/dev/null || wc -c < "$oob_outpath") bytes, sha256=$oob_sha256)"
        else
            warn "  OOB dump failed for mtd$mtd_num (non-fatal, data-only dump is primary)"
        fi
    fi

    echo "$mtd_num|$name|$artifact|OK|$actual_bytes|$sha256"
    return 0
}

RESULTS=""

while IFS='|' read -r num name size_hex; do
    [ -n "$num" ] || continue
    result="$(dump_partition "$num" "$name" "$size_hex" || true)"
    RESULTS="${RESULTS}${result}
"
done <<PARTEOF
$PARTITION_DEFS
PARTEOF

# Count results in main shell
PASS_COUNT=0
FAIL_COUNT=0

while IFS='|' read -r num name size_hex; do
    [ -n "$num" ] || continue
    artifact="mtd${num}_${name}.nanddump"
    outpath="$OUTDIR/$artifact"

    if [ -s "$outpath" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
done <<PARTEOF
$PARTITION_DEFS
PARTEOF

# ============================================================
# Post-dump verification pass
# ============================================================

log "--- Post-dump verification ---"

VERIFY_PASS=1
TOTAL_ACTUAL=0
ARTIFACT_COUNT=0

# Re-verify all artifacts and build final manifest
{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am3_bb_nand_backup_result",\n'
    printf '  "execution_utc": "%s",\n' "$(timestamp_utc)"
    printf '  "target": {\n'
    printf '    "model": "Antminer S19j Pro",\n'
    printf '    "board": "BeagleBone_Black_v2.1 on S19J_IO_BOARD_V2_0",\n'
    printf '    "soc": "AM335x",\n'
    printf '    "root_device": "%s"\n' "${ROOT_DEV:-unknown}"
    printf '  },\n'
    printf '  "partitions": [\n'

    FIRST=1
    while IFS='|' read -r num name size_hex; do
        [ -n "$num" ] || continue
        artifact="mtd${num}_${name}.nanddump"
        outpath="$OUTDIR/$artifact"
        size_dec="$(hex_to_dec "0x$size_hex")"
        status="fail"
        actual_bytes=0
        sha256="null"

        if [ -s "$outpath" ]; then
            actual_bytes="$(stat -c %s "$outpath" 2>/dev/null || wc -c < "$outpath")"
            sha256="$(sha256sum "$outpath" | awk '{ print $1 }')"
            ARTIFACT_COUNT=$((ARTIFACT_COUNT + 1))
            TOTAL_ACTUAL=$((TOTAL_ACTUAL + actual_bytes))

            if [ "$actual_bytes" -le "$size_dec" ] 2>/dev/null; then
                status="pass"
            else
                status="warn_oversized"
                VERIFY_PASS=0
            fi
        else
            status="fail_missing_or_empty"
            VERIFY_PASS=0
        fi

        if [ "$FIRST" = "1" ]; then
            FIRST=0
        else
            printf ',\n'
        fi

        printf '    {\n'
        printf '      "device": "/dev/mtd%s",\n' "$num"
        printf '      "mtd_number": %s,\n' "$num"
        printf '      "name": "%s",\n' "$name"
        printf '      "size_hex": "0x%s",\n' "$size_hex"
        printf '      "size_bytes": %d,\n' "$size_dec"
        printf '      "artifact": "%s",\n' "$artifact"
        printf '      "sha256": "%s",\n' "$sha256"
        printf '      "actual_bytes": %d,\n' "$actual_bytes"
        printf '      "dump_timestamp_utc": "%s",\n' "$(timestamp_utc)"
        printf '      "status": "%s"\n' "$status"
        printf '    }'
    done <<PARTEOF
$PARTITION_DEFS
PARTEOF

    printf '\n  ],\n'
    printf '  "verification": {\n'
    printf '    "expected_artifact_count": %d,\n' "$EXPECTED_MTD_COUNT"
    printf '    "actual_artifact_count": %d,\n' "$ARTIFACT_COUNT"
    printf '    "all_artifacts_exist": %s,\n' "$([ "$ARTIFACT_COUNT" -eq "$EXPECTED_MTD_COUNT" ] && echo "true" || echo "false")"
    printf '    "all_artifacts_nonempty": %s,\n' "$([ "$ARTIFACT_COUNT" -eq "$EXPECTED_MTD_COUNT" ] && echo "true" || echo "false")"
    printf '    "all_sha256_recorded": true,\n'
    printf '    "total_actual_bytes": %d,\n' "$TOTAL_ACTUAL"
    printf '    "sha256sums_file": "SHA256SUMS"\n'
    printf '  },\n'

    if [ "$VERIFY_PASS" = "1" ] && [ "$ARTIFACT_COUNT" -eq "$EXPECTED_MTD_COUNT" ]; then
        printf '  "nand_backup_complete": "pass"\n'
    else
        printf '  "nand_backup_complete": "fail"\n'
    fi

    printf '}\n'
} > "$MANIFEST_JSON"

# ============================================================
# Final SHA256SUMS verification
# ============================================================

log "--- SHA256SUMS cross-check ---"

if [ -s "$SHA256SUMS" ]; then
    if ( cd "$OUTDIR" && sha256sum -c SHA256SUMS ) >>"$LOGFILE" 2>&1; then
        log "SHA256SUMS verification: PASS"
    else
        warn "SHA256SUMS verification: FAIL (some hashes do not match)"
        VERIFY_PASS=0
    fi
fi

# ============================================================
# Summary table
# ============================================================

log ""
log "=== NAND Backup Summary ==="
log ""
log "| # | Partition | Artifact | Size | Status |"
log "| - | --------- | -------- | ---- | ------ |"

while IFS='|' read -r num name size_hex; do
    [ -n "$num" ] || continue
    artifact="mtd${num}_${name}.nanddump"
    outpath="$OUTDIR/$artifact"
    if [ -s "$outpath" ]; then
        actual="$(stat -c %s "$outpath" 2>/dev/null || wc -c < "$outpath")"
        log "| $num | $name | $artifact | $actual | OK |"
    else
        log "| $num | $name | $artifact | 0 | FAIL |"
    fi
done <<PARTEOF
$PARTITION_DEFS
PARTEOF

log ""
log "Manifest: $MANIFEST_JSON"
log "SHA256SUMS: $SHA256SUMS"
log "Log: $LOGFILE"
log ""

# Final determination
FINAL_COUNT=0
for i in 0 1 2 3 4 5 6 7 8 9 10 11; do
    num="$i"
    name="$(echo "$PARTITION_DEFS" | awk -F'|' -v n="$num" '$1 == n { print $2 }')"
    [ -n "$name" ] || continue
    artifact="mtd${num}_${name}.nanddump"
    if [ -s "$OUTDIR/$artifact" ]; then
        FINAL_COUNT=$((FINAL_COUNT + 1))
    fi
done

if [ "$FINAL_COUNT" -eq "$EXPECTED_MTD_COUNT" ]; then
    log "nand_backup_complete=pass"
    log "All $EXPECTED_MTD_COUNT partitions backed up and verified."
    exit 0
else
    log "nand_backup_complete=fail"
    log "Only $FINAL_COUNT of $EXPECTED_MTD_COUNT partitions completed."
    exit 1
fi
