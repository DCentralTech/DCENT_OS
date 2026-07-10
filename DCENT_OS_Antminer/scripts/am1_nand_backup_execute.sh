#!/usr/bin/env bash
#
# AM1 (S9 Zynq) NAND Full Backup Execution Script — HOST-SIDE WRAPPER.
#
# Drives a NAND backup on a remote S9 (am1 Zynq) target over SSH. Reads
# the partition list from a plan JSON, runs `nanddump` per partition,
# pulls the captured files back to the host via SFTP, and emits a
# result manifest JSON that the operator can later replay through
# `am1_nand_backup_manifest.sh --validate`.
#
# This script is the host-side cousin of `am2_nand_backup_execute.sh`
# (which is designed to be copied ONTO the target and run there).
# It uses the local SSH/SCP tooling pattern from `am2_sd_recovery_probe.sh`.
#
# SAFETY CONTRACT:
#   - NEVER writes to NAND, MTD, or bootenv on the target.
#   - NEVER stops or starts services without an explicit override flag.
#   - ONLY runs `nanddump`, `sha256sum`, `stat`, `cat /proc/mtd` on target.
#   - All files transferred host-bound via SFTP, never the reverse.
#   - Per-partition expected SHA mismatches abort the run.
#   - mtd4 (uboot_env on BraiinsOS) is dumped FIRST to capture a clean
#     snapshot before any concurrent fw_setenv can race.
#
# HARD GATES:
#   1. DCENT_NAND_BACKUP_AUTHORIZED=1 must be set on the host.
#   2. Plan JSON must declare layout_profile=1.
#   3. Target /proc/mtd must agree with the plan's partition map.
#   4. Host backup directory writable with adequate free space.
#   5. Target has nanddump + sha256sum.

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am1_nand_backup_execute.sh --target <ip> --plan <plan.json> [options]

Required:
  --target <ip>               Miner IP to back up.
  --plan <file>               Plan JSON from am1_nand_backup_plan.sh, OR
                              "-" to read from stdin.

Options:
  --user <user>               SSH user (default: root).
  --password-env <name>       Env var with SSH password (default: DCENT_PASSWORD).
                              If unset, BatchMode key auth is used.
  --local-backup-dir <path>   Where to pull artifacts. Default:
                              DCENT_OS_Antminer/output/am1-backups/<ip>-<utc>/
  --output-manifest <file>    Result manifest path. Default:
                              <local-backup-dir>/am1_nand_backup_<ip>_<utc>.manifest.json
  --operator-acknowledged-data-loss
                              REQUIRED. Mirrors the revert_common.sh
                              load-bearing gate. am1 is the only family with
                              *sustained cold-boot NAND mining* proven (the
                              `.39` proof); the operator must consciously
                              acknowledge they are touching a production unit
                              before any nanddump runs. Without this flag the
                              script hard-refuses (the run is read-only, but
                              the gate forces deliberate intent on the most
                              production-precious family).
  --readback-verify           Re-dump each partition after first pass and
                              assert sha matches (idempotent NAND read).
  --skip-size-check           Skip local free-space pre-flight.
  --timeout <seconds>         Per-partition nanddump timeout (default 180).
  -h, --help                  Show this help.

Hard gates (ALL must pass):
  - --operator-acknowledged-data-loss flag (mirrors revert_common.sh).
  - DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable.
  - Plan JSON layout_profile=1.
  - Target /proc/mtd matches plan partition map.
  - Local backup directory writable with >= plan-declared min_free_mb.
  - Target tools available: nanddump, sha256sum.

Safety contract:
  - NEVER writes to NAND, MTD, or bootenv.
  - NEVER stops services.
  - ONLY reads MTD via nanddump and computes SHA256.
  - Two independent hard gates (the explicit ack flag AND the env var)
    must BOTH be present before any nanddump runs — belt-and-suspenders
    on the only sustained-NAND-mining family.
USAGE
    exit "$code"
}

TARGET=""
PLAN_FILE=""
SSH_USER="root"
SSH_PASSWORD_ENV="DCENT_PASSWORD"
LOCAL_BACKUP_DIR=""
OUTPUT_MANIFEST=""
READBACK_VERIFY=0
SKIP_SIZE_CHECK=0
DUMP_TIMEOUT=180
OPERATOR_ACK=0

while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:?--target requires an ip}"
            shift 2
            ;;
        --target=*)
            TARGET="${1#--target=}"
            shift
            ;;
        --plan)
            PLAN_FILE="${2:?--plan requires a file or -}"
            shift 2
            ;;
        --plan=*)
            PLAN_FILE="${1#--plan=}"
            shift
            ;;
        --user)
            SSH_USER="${2:?--user requires a user}"
            shift 2
            ;;
        --user=*)
            SSH_USER="${1#--user=}"
            shift
            ;;
        --password-env)
            SSH_PASSWORD_ENV="${2:?--password-env requires an env name}"
            shift 2
            ;;
        --password-env=*)
            SSH_PASSWORD_ENV="${1#--password-env=}"
            shift
            ;;
        --local-backup-dir)
            LOCAL_BACKUP_DIR="${2:?--local-backup-dir requires a path}"
            shift 2
            ;;
        --local-backup-dir=*)
            LOCAL_BACKUP_DIR="${1#--local-backup-dir=}"
            shift
            ;;
        --output-manifest)
            OUTPUT_MANIFEST="${2:?--output-manifest requires a file}"
            shift 2
            ;;
        --output-manifest=*)
            OUTPUT_MANIFEST="${1#--output-manifest=}"
            shift
            ;;
        --operator-acknowledged-data-loss)
            OPERATOR_ACK=1
            shift
            ;;
        --readback-verify)
            READBACK_VERIFY=1
            shift
            ;;
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
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage
            ;;
    esac
done

[ -n "$TARGET" ] || usage
[ -n "$PLAN_FILE" ] || usage

case "$TARGET" in
    *[!A-Za-z0-9_.:-]*)
        echo "ERROR: unsafe target token: $TARGET" >&2
        exit 2
        ;;
esac

# Gate 0: operator-acknowledged-data-loss (mirrors revert_common.sh
# revert_check_authorization — the explicit ack flag is checked BEFORE the
# env-gate, exactly as the proven per-family revert scripts do).
if [ "$OPERATOR_ACK" != "1" ]; then
    cat >&2 <<'GATE'
ERROR: missing --operator-acknowledged-data-loss flag.

am1 (Zynq XC7Z010, S9 family) is the ONLY DCENT_OS platform with
*sustained cold-boot NAND mining* proven (the `.39` proof, 2026-04-19).
This nanddump run is read-only, but the operator must consciously
acknowledge they are touching the most production-precious family on
the fleet before any partition is read.

Re-run with:
    am1_nand_backup_execute.sh --operator-acknowledged-data-loss \
        --target <ip> --plan <plan.json> [other args]

This gate mirrors the load-bearing revert_common.sh contract so the
am1 backup ritual can never drift on the deliberate-intent check.
GATE
    exit 1
fi

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

# Load plan JSON (from file or stdin).
PLAN_JSON=""
if [ "$PLAN_FILE" = "-" ]; then
    PLAN_JSON="$(cat)"
else
    [ -f "$PLAN_FILE" ] || {
        echo "ERROR: plan file not found: $PLAN_FILE" >&2
        exit 1
    }
    PLAN_JSON="$(cat "$PLAN_FILE")"
fi

[ -n "$PLAN_JSON" ] || {
    echo "ERROR: empty plan JSON" >&2
    exit 1
}

# Limited-JSON helpers (matched to the shape produced by am1_nand_backup_plan.sh).
plan_field() {
    local key="$1"
    printf '%s' "$PLAN_JSON" | sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

plan_int() {
    local key="$1"
    printf '%s' "$PLAN_JSON" | sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\([0-9-]\+\).*/\1/p' | head -n 1
}

LAYOUT_PROFILE="$(plan_int layout_profile)"
[ "$LAYOUT_PROFILE" = "1" ] || {
    echo "ERROR: plan layout_profile != 1 (got '$LAYOUT_PROFILE'); refusing to execute" >&2
    exit 1
}

LAYOUT_SCHEME="$(plan_field layout)"
[ -n "$LAYOUT_SCHEME" ] || LAYOUT_SCHEME="unknown"

MIN_FREE_MB="$(plan_int min_free_mb)"
[ -n "$MIN_FREE_MB" ] || MIN_FREE_MB=260

# Parse partitions[] from plan JSON.
# Limited: pulls mtd_number + name + size_bytes + artifact in matched order.
MTD_NUMS="$(printf '%s' "$PLAN_JSON" | grep -oE '"mtd_number"[[:space:]]*:[[:space:]]*[0-9]+' | sed 's/.*: *//')"
PART_NAMES="$(printf '%s' "$PLAN_JSON" | grep -oE '"name"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*: *"\(.*\)"/\1/')"
PART_SIZES="$(printf '%s' "$PLAN_JSON" | grep -oE '"size_bytes"[[:space:]]*:[[:space:]]*[0-9]+' | sed 's/.*: *//')"
PART_ARTIFACTS="$(printf '%s' "$PLAN_JSON" | grep -oE '"artifact"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*: *"\(.*\)"/\1/')"

[ -n "$MTD_NUMS" ] || {
    echo "ERROR: plan has no partitions[]" >&2
    exit 1
}

PARTITION_COUNT="$(printf '%s\n' "$MTD_NUMS" | wc -l | awk '{ print $1 }')"

# Default output paths.
STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
SAFE_TARGET="$(echo "$TARGET" | tr -c 'A-Za-z0-9_.=-' '-')"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PROJECT_ROOT/../.." && pwd)"

if [ -z "$LOCAL_BACKUP_DIR" ]; then
    LOCAL_BACKUP_DIR="$REPO_ROOT/DCENT_OS_Antminer/output/am1-backups/${SAFE_TARGET}-${STAMP}"
fi
mkdir -p "$LOCAL_BACKUP_DIR" || {
    echo "ERROR: cannot create $LOCAL_BACKUP_DIR" >&2
    exit 1
}

if [ -z "$OUTPUT_MANIFEST" ]; then
    OUTPUT_MANIFEST="$LOCAL_BACKUP_DIR/am1_nand_backup_${SAFE_TARGET}_${STAMP}.manifest.json"
fi

LOGFILE="$LOCAL_BACKUP_DIR/backup_${STAMP}.log"
: > "$LOGFILE"

log() {
    local msg="[$(date -u +"%Y-%m-%dT%H:%M:%SZ")] $*"
    echo "$msg"
    echo "$msg" >> "$LOGFILE"
}

die() {
    log "FATAL: $*"
    exit 1
}

warn() {
    log "WARN: $*"
}

# Free-space gate.
if [ "$SKIP_SIZE_CHECK" != "1" ]; then
    REQUIRED_KB=$((MIN_FREE_MB * 1024))
    if [ "$READBACK_VERIFY" = "1" ]; then
        REQUIRED_KB=$((REQUIRED_KB * 2))
    fi
    FREE_KB="$(df -Pk "$LOCAL_BACKUP_DIR" 2>/dev/null | awk 'NR==2 { print $4 }' || echo 0)"
    if [ "$FREE_KB" -lt "$REQUIRED_KB" ] 2>/dev/null; then
        die "Insufficient host free space: ${FREE_KB} KB < ${REQUIRED_KB} KB"
    fi
    log "Host free space: ${FREE_KB} KB (>= ${REQUIRED_KB} KB)"
else
    log "Free space check skipped (--skip-size-check)"
fi

# SSH helpers.
SSH_OPTS=(
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null
    -o ConnectTimeout=10
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=3
    -o LogLevel=ERROR
)

ssh_password_set() {
    [ -n "${!SSH_PASSWORD_ENV:-}" ]
}

ssh_exec() {
    local cmd="$1"
    if ssh_password_set; then
        if ! command -v sshpass >/dev/null 2>&1; then
            die "sshpass not installed; cannot use password env $SSH_PASSWORD_ENV"
        fi
        SSHPASS="${!SSH_PASSWORD_ENV}" sshpass -e ssh "${SSH_OPTS[@]}" "${SSH_USER}@${TARGET}" "$cmd"
    else
        ssh "${SSH_OPTS[@]}" -o BatchMode=yes "${SSH_USER}@${TARGET}" "$cmd"
    fi
}

scp_fetch() {
    local remote="$1"
    local local="$2"
    if ssh_password_set; then
        SSHPASS="${!SSH_PASSWORD_ENV}" sshpass -e scp "${SSH_OPTS[@]}" "${SSH_USER}@${TARGET}:${remote}" "$local"
    else
        scp "${SSH_OPTS[@]}" -o BatchMode=yes "${SSH_USER}@${TARGET}:${remote}" "$local"
    fi
}

log "=== AM1 NAND Backup Execution (host-side wrapper) ==="
log "Target: $TARGET (user=$SSH_USER)"
log "Plan layout: $LAYOUT_SCHEME ($PARTITION_COUNT partitions)"
log "Local backup dir: $LOCAL_BACKUP_DIR"
log "Output manifest: $OUTPUT_MANIFEST"
log "Readback verify: $READBACK_VERIFY"
log "Dump timeout: ${DUMP_TIMEOUT}s"

# Probe target /proc/mtd and verify partition map matches plan.
log "--- Probing target /proc/mtd ---"
TARGET_PROC_MTD="$(ssh_exec 'cat /proc/mtd 2>/dev/null')" || die "Could not reach target /proc/mtd"
echo "$TARGET_PROC_MTD" >> "$LOGFILE"

TARGET_NAMES="$(printf '%s\n' "$TARGET_PROC_MTD" | awk '
    /^mtd[0-9]+:/ {
        name = $4
        gsub(/"/, "", name)
        print name
    }
' | sort -u | tr '\n' ' ')"

PLAN_NAMES_SORTED="$(printf '%s\n' "$PART_NAMES" | sort -u | tr '\n' ' ')"

if [ "$TARGET_NAMES" != "$PLAN_NAMES_SORTED" ]; then
    warn "Target partition map differs from plan:"
    warn "  plan:   $PLAN_NAMES_SORTED"
    warn "  target: $TARGET_NAMES"
    die "Refusing to back up — partition map mismatch (plan was probably built from a different unit)."
fi
log "Target partition map matches plan."

# Verify target tools.
for tool in nanddump sha256sum stat; do
    if ! ssh_exec "command -v $tool >/dev/null 2>&1"; then
        die "Target missing required tool: $tool"
    fi
done
log "Target has nanddump/sha256sum/stat."

# Remote work directory.
REMOTE_OUTDIR="/tmp/am1_nand_${STAMP}"
ssh_exec "mkdir -p '$REMOTE_OUTDIR'" || die "Could not create $REMOTE_OUTDIR on target"
log "Remote work dir: $REMOTE_OUTDIR"

# Build ordered MTD list — for BraiinsOS, mtd4 (uboot_env) FIRST.
ORDER=""
if [ "$LAYOUT_SCHEME" = "braiinsos" ]; then
    ORDER="4 $(printf '%s\n' "$MTD_NUMS" | grep -v '^4$' | tr '\n' ' ')"
else
    ORDER="$(printf '%s\n' "$MTD_NUMS" | tr '\n' ' ')"
fi

# Helper to look up a partition field by mtd_number.
get_part_field() {
    local mtd_num="$1"
    local field="$2"  # name | size | artifact
    local idx=1
    local n
    for n in $MTD_NUMS; do
        if [ "$n" = "$mtd_num" ]; then
            case "$field" in
                name)     printf '%s\n' "$PART_NAMES" | sed -n "${idx}p" ;;
                size)     printf '%s\n' "$PART_SIZES" | sed -n "${idx}p" ;;
                artifact) printf '%s\n' "$PART_ARTIFACTS" | sed -n "${idx}p" ;;
            esac
            return
        fi
        idx=$((idx + 1))
    done
}

# Per-partition capture results, accumulated as TSV in a temp file.
RESULTS_TSV="$LOCAL_BACKUP_DIR/.results.tsv"
: > "$RESULTS_TSV"

PASS_COUNT=0
FAIL_COUNT=0
READBACK_FAIL=0
TOTAL_BYTES=0

for mtd_num in $ORDER; do
    name="$(get_part_field "$mtd_num" name)"
    size_dec="$(get_part_field "$mtd_num" size)"
    artifact="$(get_part_field "$mtd_num" artifact)"
    [ -n "$name" ] || continue

    log "--- mtd$mtd_num ($name) — dumping (max ${size_dec} bytes) ---"

    # nanddump on target.
    if ! ssh_exec "nanddump --bb=skipbad --omitoob -f '$REMOTE_OUTDIR/$artifact' /dev/mtd$mtd_num" >> "$LOGFILE" 2>&1; then
        warn "nanddump failed for mtd$mtd_num"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mtd_num" "$name" "$size_dec" "$artifact" "0" "" "fail_dump" >> "$RESULTS_TSV"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        continue
    fi

    # sha256 + size on target.
    REMOTE_SHA="$(ssh_exec "sha256sum '$REMOTE_OUTDIR/$artifact' 2>/dev/null | awk '{print \$1}'")"
    REMOTE_SIZE="$(ssh_exec "stat -c %s '$REMOTE_OUTDIR/$artifact' 2>/dev/null || wc -c < '$REMOTE_OUTDIR/$artifact'")"

    if [ -z "$REMOTE_SHA" ] || [ -z "$REMOTE_SIZE" ]; then
        warn "Could not read remote sha/size for $artifact"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mtd_num" "$name" "$size_dec" "$artifact" "0" "" "fail_meta" >> "$RESULTS_TSV"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        continue
    fi

    # Pull artifact to host.
    if ! scp_fetch "$REMOTE_OUTDIR/$artifact" "$LOCAL_BACKUP_DIR/$artifact" >> "$LOGFILE" 2>&1; then
        warn "scp fetch failed for $artifact"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mtd_num" "$name" "$size_dec" "$artifact" "0" "" "fail_fetch" >> "$RESULTS_TSV"
        FAIL_COUNT=$((FAIL_COUNT + 1))
        continue
    fi

    # Recompute sha on host and compare.
    HOST_SHA="$(sha256sum "$LOCAL_BACKUP_DIR/$artifact" | awk '{print $1}')"
    if [ "$HOST_SHA" != "$REMOTE_SHA" ]; then
        warn "partition $name mismatch — refusing to continue (remote=$REMOTE_SHA host=$HOST_SHA)"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mtd_num" "$name" "$size_dec" "$artifact" "$REMOTE_SIZE" "$HOST_SHA" "fail_sha_mismatch" >> "$RESULTS_TSV"
        die "partition $name mismatch — aborting backup run; review log $LOGFILE"
    fi

    # Optional readback verify.
    READBACK_STATUS="skipped"
    if [ "$READBACK_VERIFY" = "1" ]; then
        log "  readback re-dump for mtd$mtd_num"
        if ssh_exec "nanddump --bb=skipbad --omitoob -f '$REMOTE_OUTDIR/${artifact}.recheck' /dev/mtd$mtd_num" >> "$LOGFILE" 2>&1; then
            RECHECK_SHA="$(ssh_exec "sha256sum '$REMOTE_OUTDIR/${artifact}.recheck' 2>/dev/null | awk '{print \$1}'")"
            if [ "$RECHECK_SHA" = "$REMOTE_SHA" ]; then
                READBACK_STATUS="match"
                ssh_exec "rm -f '$REMOTE_OUTDIR/${artifact}.recheck'" >> "$LOGFILE" 2>&1 || true
            else
                READBACK_STATUS="mismatch"
                READBACK_FAIL=$((READBACK_FAIL + 1))
                warn "readback MISMATCH for $artifact (recheck=$RECHECK_SHA first=$REMOTE_SHA)"
            fi
        else
            READBACK_STATUS="recheck_dump_failed"
            READBACK_FAIL=$((READBACK_FAIL + 1))
        fi
    fi

    log "  OK $artifact ($REMOTE_SIZE bytes, sha=$REMOTE_SHA, readback=$READBACK_STATUS)"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mtd_num" "$name" "$size_dec" "$artifact" "$REMOTE_SIZE" "$HOST_SHA" "pass" >> "$RESULTS_TSV"
    PASS_COUNT=$((PASS_COUNT + 1))
    TOTAL_BYTES=$((TOTAL_BYTES + REMOTE_SIZE))
done

# Optional cleanup of remote work dir (keep by default for operator inspection).
log "Remote artifacts left in $REMOTE_OUTDIR (operator may rm -rf after verification)."

# Build host-side SHA256SUMS file.
SHA256SUMS="$LOCAL_BACKUP_DIR/SHA256SUMS"
: > "$SHA256SUMS"
while IFS=$'\t' read -r mnum mname msize mart mactual msha mstat; do
    if [ "$mstat" = "pass" ] && [ -n "$msha" ]; then
        printf '%s  %s\n' "$msha" "$mart" >> "$SHA256SUMS"
    fi
done < "$RESULTS_TSV"

# Build result manifest JSON.
log "--- Writing result manifest ---"
{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am1_nand_backup_result",\n'
    printf '  "execution_utc": "%s",\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    printf '  "target": {\n'
    printf '    "ip": "%s",\n' "$TARGET"
    printf '    "class": "am1 Zynq XC7Z010",\n'
    printf '    "layout": "%s"\n' "$LAYOUT_SCHEME"
    printf '  },\n'
    printf '  "readback_verify": %d,\n' "$READBACK_VERIFY"
    printf '  "readback_failures": %d,\n' "$READBACK_FAIL"
    printf '  "partitions": [\n'

    FIRST=1
    while IFS=$'\t' read -r mnum mname msize mart mactual msha mstat; do
        [ -n "$mnum" ] || continue
        if [ "$FIRST" = "1" ]; then
            FIRST=0
        else
            printf ',\n'
        fi
        # Quote sha — emit "null" if empty.
        if [ -n "$msha" ]; then
            sha_field="\"$msha\""
        else
            sha_field="null"
        fi
        printf '    {\n'
        printf '      "device": "/dev/mtd%s",\n' "$mnum"
        printf '      "mtd_number": %s,\n' "$mnum"
        printf '      "name": "%s",\n' "$mname"
        printf '      "size_bytes": %s,\n' "$msize"
        printf '      "artifact": "%s",\n' "$mart"
        printf '      "sha256": %s,\n' "$sha_field"
        printf '      "actual_bytes": %s,\n' "${mactual:-0}"
        printf '      "status": "%s"\n' "$mstat"
        printf '    }'
    done < "$RESULTS_TSV"

    printf '\n  ],\n'
    printf '  "verification": {\n'
    printf '    "expected_artifact_count": %d,\n' "$PARTITION_COUNT"
    printf '    "actual_artifact_count": %d,\n' "$PASS_COUNT"
    printf '    "fail_count": %d,\n' "$FAIL_COUNT"
    printf '    "readback_failures": %d,\n' "$READBACK_FAIL"
    printf '    "total_bytes": %d,\n' "$TOTAL_BYTES"
    printf '    "sha256sums_file": "SHA256SUMS",\n'
    printf '    "log_file": "%s"\n' "$(basename "$LOGFILE")"
    printf '  },\n'
    if [ "$PASS_COUNT" -eq "$PARTITION_COUNT" ] && [ "$FAIL_COUNT" -eq 0 ] && [ "$READBACK_FAIL" -eq 0 ]; then
        printf '  "nand_backup_complete": "pass"\n'
    else
        printf '  "nand_backup_complete": "fail"\n'
    fi
    printf '}\n'
} > "$OUTPUT_MANIFEST"

log "Pass=$PASS_COUNT  Fail=$FAIL_COUNT  ReadbackFail=$READBACK_FAIL"
log "Manifest: $OUTPUT_MANIFEST"
log "SHA256SUMS: $SHA256SUMS"
rm -f "$RESULTS_TSV"

if [ "$PASS_COUNT" -eq "$PARTITION_COUNT" ] && [ "$FAIL_COUNT" -eq 0 ] && [ "$READBACK_FAIL" -eq 0 ]; then
    log "nand_backup_complete=pass"
    exit 0
fi
log "nand_backup_complete=fail"
exit 1
