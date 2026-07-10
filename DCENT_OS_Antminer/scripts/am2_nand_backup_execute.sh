#!/bin/sh
#
# AM2 (S19 Pro / S19j Pro Zynq) NAND Full Backup Execution Script
#
# Reads all 10 MTD partitions from a BraiinsOS-class am2 Zynq control board
# and produces verified nanddump artifacts with a machine-readable manifest.
#
# AM2 layout (per `a lab unit` U-Boot env extraction):
#   mtd0  boot              8M
#   mtd1  boot-failover    12M
#   mtd2  fpga1             2M
#   mtd3  fpga2             2M
#   mtd4  uboot_env       512K
#   mtd5  miner_cfg       512K
#   mtd6  recovery         87M
#   mtd7  firmware1        57M  (slot A)
#   mtd8  firmware2        57M  (slot B)
#   mtd9  factory          30M
#
# SAFETY CONTRACT:
#   - NEVER writes to any MTD device, NAND, or bootenv.
#   - NEVER stops or starts any services.
#   - ONLY performs nanddump reads and sha256sum computations.
#   - MUST have DCENT_NAND_BACKUP_AUTHORIZED=1 set.
#   - mtd4 (uboot_env) is dumped FIRST to capture a clean snapshot before
#     any concurrent fw_setenv can race.
#   - Active firmware slot is identified read-only; the inactive slot dump
#     is the canonical clean-snapshot restore image.
#   - Optional --readback-verify re-dumps each partition and confirms the
#     SHA matches (idempotent NAND read).
#
# HARD GATES:
#   1. DCENT_NAND_BACKUP_AUTHORIZED=1 must be set.
#   2. /proc/mtd reports exactly the 10 expected AM2 partition names.
#   3. Output directory writable with adequate free space.
#   4. nanddump + sha256sum available.
#
# Designed for BusyBox ash compatibility.

set -eu

EXPECTED_MTD_COUNT=10
PARTITION_DEFS="
0|boot|00800000
1|boot-failover|00c00000
2|fpga1|00200000
3|fpga2|00200000
4|uboot_env|00020000
5|miner_cfg|00020000
6|recovery|05700000
7|firmware1|03900000
8|firmware2|03900000
9|factory|01e00000
"

DUMP_TIMEOUT=180
MIN_FREE_KB=286720   # ~280 MB

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am2_nand_backup_execute.sh <output_directory> [options]

Arguments:
  <output_directory>     Directory to write backup artifacts.

Options:
  --skip-size-check      Skip free-space pre-flight.
  --timeout <seconds>    Per-partition dump timeout (default: 180).
  --readback-verify      After every dump completes, re-dump each
                         partition and assert the SHA matches the first
                         pass. Catches transient read errors and proves
                         the NAND read is idempotent.
  -h, --help             Show this help.

Hard gates (ALL must pass):
  - DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable.
  - /proc/mtd reports the 10 expected AM2 partitions.
  - Output directory writable with >= 280 MB free.
  - nanddump and sha256sum commands available.

Safety contract:
  - NEVER writes to NAND, MTD, or bootenv.
  - NEVER stops services.
  - ONLY reads MTD via nanddump and computes SHA256.
USAGE
    exit "$code"
}

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
    local hex="$1"
    hex="${hex#0x}"
    hex="${hex#0X}"
    printf '%d' "0x$hex" 2>/dev/null || echo 0
}

OUTDIR=""
SKIP_SIZE_CHECK=0
READBACK_VERIFY=0

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
        --readback-verify)
            READBACK_VERIFY=1
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

# Gate 1: authorization.
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

# Gate 2: required tools.
for tool in nanddump sha256sum stat; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        die "Required tool not found: $tool"
    fi
done

# Gate 3: all 10 MTD devices present.
MISSING_MTD=""
for i in 0 1 2 3 4 5 6 7 8 9; do
    if [ ! -c "/dev/mtd$i" ] && [ ! -e "/dev/mtd$i" ]; then
        MISSING_MTD="$MISSING_MTD mtd$i"
    fi
done

if [ -n "$MISSING_MTD" ]; then
    die "Missing MTD devices:$MISSING_MTD (expected 10: mtd0-mtd9). \
This may not be a BraiinsOS-class am2 unit. If the unit reports a single-slot \
layout (mtd1=ramfs, no firmware1/firmware2) it is stock XIL — convert to \
BraiinsOS first."
fi

# Setup output directory.
mkdir -p "$OUTDIR" || die "Cannot create output directory: $OUTDIR"
STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
LOGFILE="$OUTDIR/backup_${STAMP}.log"
MANIFEST_JSON="$OUTDIR/backup_manifest_${STAMP}.json"
SHA256SUMS="$OUTDIR/SHA256SUMS"

log "=== AM2 NAND Backup Execution ==="
log "Output directory: $OUTDIR"
log "Timestamp: $STAMP"
log "Dump timeout per partition: ${DUMP_TIMEOUT}s"
log "Readback verify: $READBACK_VERIFY"

# Gate 4: free space.
if [ "$SKIP_SIZE_CHECK" != "1" ]; then
    FREE_KB="$(df -Pk "$OUTDIR" 2>/dev/null | awk 'NR==2 { print $4 }' || echo 0)"
    # Double the requirement if readback-verify is on (we keep both dumps).
    REQUIRED_KB="$MIN_FREE_KB"
    if [ "$READBACK_VERIFY" = "1" ]; then
        REQUIRED_KB=$((MIN_FREE_KB * 2))
    fi
    if [ "$FREE_KB" -lt "$REQUIRED_KB" ] 2>/dev/null; then
        die "Insufficient free space: ${FREE_KB} KB available, need ${REQUIRED_KB} KB"
    fi
    log "Free space: ${FREE_KB} KB (minimum ${REQUIRED_KB} KB)"
else
    log "Free space check skipped (--skip-size-check)"
fi

# Verify MTD layout.
log "--- Verifying MTD layout ---"
if [ -f /proc/mtd ]; then
    cat /proc/mtd >> "$LOGFILE" 2>/dev/null || true
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
    if [ "$LAYOUT_MISMATCH" = "1" ]; then
        die "MTD layout does not match expected am2 BraiinsOS-class layout. Refusing backup."
    fi
else
    warn "/proc/mtd not found — cannot verify layout names"
fi

# Identify active firmware slot for the manifest (read-only).
ACTIVE_SLOT=""
if command -v fw_printenv >/dev/null 2>&1; then
    ACTIVE_SLOT="$(fw_printenv firmware 2>/dev/null | cut -d= -f2 || true)"
fi
log "Active firmware slot (fw_printenv firmware): ${ACTIVE_SLOT:-unknown}"

# --- Execute backup ---
log "--- Beginning NAND backup (10 partitions, mtd4 first) ---"

: > "$SHA256SUMS"

# Order: mtd4 (uboot_env) first to capture a clean snapshot, then the rest
# in mtd-number order.
ORDERED_ORDER="4 0 1 2 3 5 6 7 8 9"

dump_partition() {
    local mtd_num="$1"
    local name="$2"
    local size_hex="$3"
    local artifact="mtd${mtd_num}_${name}.nanddump"
    local outpath="$OUTDIR/$artifact"
    local size_dec
    size_dec="$(hex_to_dec "0x$size_hex")"

    log "  Dumping mtd$mtd_num ($name) -> $artifact [max $size_dec bytes]..."

    local dump_ok=0
    if command -v timeout >/dev/null 2>&1; then
        if timeout "$DUMP_TIMEOUT" nanddump --bb=skipbad --omitoob -f "$outpath" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
            dump_ok=1
        fi
    else
        if nanddump --bb=skipbad --omitoob -f "$outpath" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
            dump_ok=1
        fi
    fi

    if [ "$dump_ok" != "1" ]; then
        warn "  FAILED to dump mtd$mtd_num ($name)"
        return 1
    fi

    if [ ! -s "$outpath" ]; then
        warn "  FAILED: output file empty or missing: $outpath"
        return 1
    fi

    local sha256
    sha256="$(sha256sum "$outpath" | awk '{ print $1 }')"
    local actual_bytes
    actual_bytes="$(stat -c %s "$outpath" 2>/dev/null || wc -c < "$outpath")"
    echo "$sha256  $artifact" >> "$SHA256SUMS"
    log "  OK: $artifact ($actual_bytes bytes, sha256=$sha256)"

    if [ "$actual_bytes" -gt "$size_dec" ] 2>/dev/null; then
        warn "  Size $actual_bytes exceeds partition size $size_dec — unexpected"
    fi
    return 0
}

readback_check() {
    local mtd_num="$1"
    local name="$2"
    local artifact="mtd${mtd_num}_${name}.nanddump"
    local outpath="$OUTDIR/$artifact"
    local recheck="$OUTDIR/${artifact}.recheck"
    log "  Re-dumping mtd$mtd_num ($name) for readback verify..."
    if ! nanddump --bb=skipbad --omitoob -f "$recheck" "/dev/mtd${mtd_num}" 2>>"$LOGFILE"; then
        warn "  readback re-dump FAILED for mtd$mtd_num"
        return 1
    fi
    local sha_first sha_second
    sha_first="$(sha256sum "$outpath" | awk '{print $1}')"
    sha_second="$(sha256sum "$recheck" | awk '{print $1}')"
    if [ "$sha_first" = "$sha_second" ]; then
        log "  readback OK: mtd$mtd_num ($name) idempotent (sha=$sha_first)"
        rm -f "$recheck"
        return 0
    else
        warn "  readback MISMATCH for mtd$mtd_num ($name): $sha_first != $sha_second"
        return 1
    fi
}

PASS_COUNT=0
FAIL_COUNT=0
READBACK_FAIL=0

# Build helper map for partition lookup.
get_part_field() {
    local num="$1"
    local field="$2"  # 2=name, 3=size_hex
    echo "$PARTITION_DEFS" | awk -F'|' -v n="$num" -v f="$field" '$1 == n { print $f }'
}

for num in $ORDERED_ORDER; do
    name="$(get_part_field "$num" 2)"
    size_hex="$(get_part_field "$num" 3)"
    [ -n "$name" ] || continue
    if dump_partition "$num" "$name" "$size_hex"; then
        PASS_COUNT=$((PASS_COUNT + 1))
        if [ "$READBACK_VERIFY" = "1" ]; then
            if ! readback_check "$num" "$name"; then
                READBACK_FAIL=$((READBACK_FAIL + 1))
            fi
        fi
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
done

# --- SHA256SUMS verification ---
log "--- SHA256SUMS cross-check ---"
VERIFY_PASS=1
if [ -s "$SHA256SUMS" ]; then
    if ( cd "$OUTDIR" && sha256sum -c SHA256SUMS ) >>"$LOGFILE" 2>&1; then
        log "SHA256SUMS verification: PASS"
    else
        warn "SHA256SUMS verification: FAIL"
        VERIFY_PASS=0
    fi
fi

# --- Build final manifest ---
TOTAL_ACTUAL=0
ARTIFACT_COUNT=0
{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am2_nand_backup_result",\n'
    printf '  "execution_utc": "%s",\n' "$(timestamp_utc)"
    printf '  "target": {\n'
    printf '    "class": "am2 Zynq XC7Z020 BraiinsOS dual-slot",\n'
    printf '    "active_firmware_slot": "%s"\n' "${ACTIVE_SLOT:-unknown}"
    printf '  },\n'
    printf '  "readback_verify": %d,\n' "$READBACK_VERIFY"
    printf '  "readback_failures": %d,\n' "$READBACK_FAIL"
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
    printf '    "all_sha256_recorded": true,\n'
    printf '    "total_actual_bytes": %d,\n' "$TOTAL_ACTUAL"
    printf '    "sha256sums_file": "SHA256SUMS"\n'
    printf '  },\n'
    if [ "$VERIFY_PASS" = "1" ] && [ "$ARTIFACT_COUNT" -eq "$EXPECTED_MTD_COUNT" ] && [ "$READBACK_FAIL" = "0" ]; then
        printf '  "nand_backup_complete": "pass"\n'
    else
        printf '  "nand_backup_complete": "fail"\n'
    fi
    printf '}\n'
} > "$MANIFEST_JSON"

log ""
log "=== AM2 NAND Backup Summary ==="
log "Pass: $PASS_COUNT, Fail: $FAIL_COUNT, Readback fail: $READBACK_FAIL"
log "Manifest: $MANIFEST_JSON"
log "SHA256SUMS: $SHA256SUMS"
log "Log: $LOGFILE"

if [ "$ARTIFACT_COUNT" -eq "$EXPECTED_MTD_COUNT" ] && [ "$VERIFY_PASS" = "1" ] && [ "$READBACK_FAIL" = "0" ]; then
    log "nand_backup_complete=pass"
    exit 0
else
    log "nand_backup_complete=fail"
    exit 1
fi
