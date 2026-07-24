#!/usr/bin/env bash
#
# Host-side AM2 (S19 Pro / S19j Pro Zynq 7007S) full-NAND backup producer.
#
# The operator supplies a strictly validated, exact-unit plan. This wrapper
# binds the network endpoint back to the plan's physical MAC/HWID/model,
# re-probes exact live MTD geometry, streams padded fixed-size images directly
# to a fresh host directory, performs a bounded second read, validates
# the complete local evidence, and only then publishes a result manifest.
#
# SAFETY CONTRACT:
#   - Never writes NAND, MTD, bootenv, firmware slots, or miner configuration.
#   - Never stops or starts services.
#   - Only uses read-only identity commands, /proc/mtd, and nanddump on the
#     target. Raw NAND bytes are never staged on the target filesystem.
#   - Requires both explicit environment and command-line authorization.
#   - Requires --readback-verify; every NAND read is host-timeout bounded.
#   - Uses nanddump --bb=padbad so every accepted artifact is exactly the
#     partition size and retains bad-block offsets for restoration.

set -euo pipefail
umask 077

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
PLAN_VALIDATOR="$SCRIPT_DIR/validate_am2_nand_backup_plan.py"
RESULT_VALIDATOR="$SCRIPT_DIR/validate_am2_nand_backup.py"
ATOMIC_PUBLISHER="$SCRIPT_DIR/atomic_publish_file.py"
DURABLE_IO="$SCRIPT_DIR/durable_file_io.py"

DUMP_TIMEOUT=180
SSH_USER=root
SSH_PASSWORD_ENV=MINER_PASSWORD
PLAN_FILE=""
TARGET=""
LOCAL_BACKUP_DIR=""
KNOWN_HOSTS=""
EXPECTED_HOST_KEY_SHA256=""
SKIP_SIZE_CHECK=0
READBACK_VERIFY=0
OPERATOR_AUTHORIZED=0

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am2_nand_backup_execute.sh --target <ip> --plan <plan.json> [options]

Required:
  --target <ip>                 Miner IPv4/hostname used for SSH and evidence.
  --plan <file|->              READY JSON from am2_nand_backup_plan.sh.
  --operator-authorized-backup Confirm the operator reviewed the exact plan.
  --readback-verify            Re-dump every partition and require identical SHA.
  DCENT_NAND_BACKUP_AUTHORIZED=1

Options:
  --local-backup-dir <dir>     Fresh host directory for all evidence. Default:
                               DCENT_OS_Antminer/output/am2-backups/<target>-<utc>
  --known-hosts <file>         REQUIRED pinned OpenSSH known_hosts file.
  --expected-host-key-sha256 <fingerprint>
                               REQUIRED exact SHA256 host-key fingerprint.
  --ssh-user <user>            SSH user (default: root).
  --ssh-password-env <name>    Environment variable containing the password
                               (default: MINER_PASSWORD). Without a password,
                               BatchMode public-key authentication is used.
  --timeout <seconds>          Per NAND read/transfer timeout (default: 180).
  --skip-size-check            Lab-only bypass of the host free-space check.
  -h, --help                   Show this help.

The plan is validated before SSH. Live MAC, HWID, model, optional installed
board_target, and exact ordered /proc/mtd geometry must match before nanddump.
USAGE
    exit "$code"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:?--target requires a value}"
            shift 2
            ;;
        --target=*) TARGET="${1#--target=}"; shift ;;
        --plan)
            PLAN_FILE="${2:?--plan requires a file or -}"
            shift 2
            ;;
        --plan=*) PLAN_FILE="${1#--plan=}"; shift ;;
        --local-backup-dir)
            LOCAL_BACKUP_DIR="${2:?--local-backup-dir requires a directory}"
            shift 2
            ;;
        --local-backup-dir=*) LOCAL_BACKUP_DIR="${1#--local-backup-dir=}"; shift ;;
        --known-hosts)
            KNOWN_HOSTS="${2:?--known-hosts requires a file}"
            shift 2
            ;;
        --known-hosts=*) KNOWN_HOSTS="${1#--known-hosts=}"; shift ;;
        --expected-host-key-sha256)
            EXPECTED_HOST_KEY_SHA256="${2:?--expected-host-key-sha256 requires a fingerprint}"
            shift 2
            ;;
        --expected-host-key-sha256=*)
            EXPECTED_HOST_KEY_SHA256="${1#--expected-host-key-sha256=}"
            shift
            ;;
        --ssh-user)
            SSH_USER="${2:?--ssh-user requires a value}"
            shift 2
            ;;
        --ssh-user=*) SSH_USER="${1#--ssh-user=}"; shift ;;
        --ssh-password-env)
            SSH_PASSWORD_ENV="${2:?--ssh-password-env requires a variable name}"
            shift 2
            ;;
        --ssh-password-env=*) SSH_PASSWORD_ENV="${1#--ssh-password-env=}"; shift ;;
        --timeout)
            DUMP_TIMEOUT="${2:?--timeout requires seconds}"
            shift 2
            ;;
        --timeout=*) DUMP_TIMEOUT="${1#--timeout=}"; shift ;;
        --skip-size-check) SKIP_SIZE_CHECK=1; shift ;;
        --readback-verify) READBACK_VERIFY=1; shift ;;
        --operator-authorized-backup) OPERATOR_AUTHORIZED=1; shift ;;
        -h|--help) usage 0 ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage
            ;;
    esac
done

[ -n "$TARGET" ] || usage
[ -n "$PLAN_FILE" ] || usage
printf '%s\n' "$TARGET" | grep -Eq '^[A-Za-z0-9_.:-]+$' || {
    echo "ERROR: --target contains unsafe characters" >&2
    exit 1
}
printf '%s\n' "$SSH_USER" | grep -Eq '^[A-Za-z0-9_.-]+$' || {
    echo "ERROR: --ssh-user contains unsafe characters" >&2
    exit 1
}
printf '%s\n' "$SSH_PASSWORD_ENV" | grep -Eq '^[A-Za-z_][A-Za-z0-9_]*$' || {
    echo "ERROR: --ssh-password-env must be a shell variable name" >&2
    exit 1
}
case "$DUMP_TIMEOUT" in
    ''|*[!0-9]*|0)
        echo "ERROR: --timeout must be a positive integer" >&2
        exit 1
        ;;
esac
[ "$OPERATOR_AUTHORIZED" = "1" ] || {
    echo "ERROR: --operator-authorized-backup is required" >&2
    exit 1
}
[ "$READBACK_VERIFY" = "1" ] || {
    echo "ERROR: --readback-verify is required" >&2
    exit 1
}
[ "${DCENT_NAND_BACKUP_AUTHORIZED:-0}" = "1" ] || {
    echo "ERROR: DCENT_NAND_BACKUP_AUTHORIZED must equal 1" >&2
    exit 1
}

PYTHON_BIN="${PYTHON:-}"
if [ -z "$PYTHON_BIN" ]; then
    PYTHON_BIN="$(command -v python3 || command -v python || true)"
fi
[ -n "$PYTHON_BIN" ] || {
    echo "ERROR: Python is required for strict AM2 evidence validation" >&2
    exit 1
}
for helper in "$PLAN_VALIDATOR" "$RESULT_VALIDATOR" "$ATOMIC_PUBLISHER" "$DURABLE_IO"; do
    [ -f "$helper" ] || {
        echo "ERROR: required helper is missing: $helper" >&2
        exit 1
    }
done
for tool in ssh ssh-keygen timeout sha256sum stat mktemp; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: required host tool is missing: $tool" >&2
        exit 1
    }
done

# Validate and normalize the plan before any network contact.
if [ "$PLAN_FILE" = "-" ]; then
    PLAN_JSON="$(cat)"
else
    [ -f "$PLAN_FILE" ] && [ ! -L "$PLAN_FILE" ] || {
        echo "ERROR: plan must be a regular non-symlink file: $PLAN_FILE" >&2
        exit 1
    }
    PLAN_JSON="$(cat "$PLAN_FILE")"
fi
[ -n "$PLAN_JSON" ] || {
    echo "ERROR: plan JSON is empty" >&2
    exit 1
}
if ! PLAN_VALIDATION="$(
    printf '%s' "$PLAN_JSON" |
        "$PYTHON_BIN" "$PLAN_VALIDATOR" --plan - 2>&1
)"; then
    printf '%s\n' "$PLAN_VALIDATION" >&2
    exit 1
fi

plan_value() {
    local key="$1"
    printf '%s\n' "$PLAN_VALIDATION" |
        awk -F= -v expected="$key" \
            '$1 == expected { print substr($0, length($1) + 2); exit }'
}

LAYOUT="$(plan_value layout)"
MIN_FREE_MB="$(plan_value min_free_mb)"
EXPECTED_ENDPOINT="$(plan_value expected_endpoint)"
EXPECTED_BOARD_TARGET="$(plan_value expected_target)"
EXPECTED_MAC="$(plan_value expected_mac)"
EXPECTED_HWID="$(plan_value expected_hwid)"
EXPECTED_MODEL="$(plan_value expected_model)"
PLAN_HOST_KEY_SHA256="$(plan_value expected_host_key_sha256)"
EXPECTED_ROOT_DEVICE="$(plan_value expected_root_device)"
EXPECTED_COMPATIBLE="$(plan_value expected_compatible)"
SD_RECOVERY_PROOF_SHA256="$(plan_value sd_recovery_proof_sha256)"
SD_RECOVERY_BOOT_ID="$(plan_value sd_recovery_boot_id)"
SD_RECOVERY_QUIESCENCE="$(plan_value sd_recovery_quiescence)"
PARTITION_ROWS="$(printf '%s\n' "$PLAN_VALIDATION" | sed -n 's/^partition=//p')"
PLAN_GEOMETRY="$(
    printf '%s\n' "$PLAN_VALIDATION" |
        sed -n 's/^geometry=//p' |
        paste -sd ' ' -
)"
PARTITION_COUNT="$(printf '%s\n' "$PARTITION_ROWS" | sed '/^$/d' | wc -l | awk '{print $1}')"
[ "$PARTITION_COUNT" = "10" ] || {
    echo "ERROR: normalized plan did not contain exactly ten partitions" >&2
    exit 1
}
[ "$TARGET" = "$EXPECTED_ENDPOINT" ] || {
    echo "ERROR: --target $TARGET does not match plan endpoint $EXPECTED_ENDPOINT" >&2
    exit 1
}
[ -n "$KNOWN_HOSTS" ] && [ -f "$KNOWN_HOSTS" ] && [ ! -L "$KNOWN_HOSTS" ] || {
    echo "ERROR: --known-hosts must be a regular non-symlink file" >&2
    exit 1
}
printf '%s\n' "$EXPECTED_HOST_KEY_SHA256" |
    grep -Eq '^SHA256:[A-Za-z0-9+/]{43}$' || {
    echo "ERROR: --expected-host-key-sha256 is not an OpenSSH SHA256 fingerprint" >&2
    exit 1
}
[ "$EXPECTED_HOST_KEY_SHA256" = "$PLAN_HOST_KEY_SHA256" ] || {
    echo "ERROR: supplied host-key fingerprint does not match the SD-recovery proof bound into the plan" >&2
    exit 1
}
PINNED_FINGERPRINTS="$(
    ssh-keygen -F "$TARGET" -f "$KNOWN_HOSTS" 2>/dev/null |
        awk '!/^#/ && NF >= 3 {print}' |
        while IFS= read -r host_key; do
            printf '%s\n' "$host_key" |
                ssh-keygen -lf - -E sha256 2>/dev/null |
                awk '{print $2}'
        done
)"
PINNED_KEY_COUNT="$(printf '%s\n' "$PINNED_FINGERPRINTS" | sed '/^$/d' | wc -l | awk '{print $1}')"
[ "$PINNED_KEY_COUNT" = "1" ] && \
    [ "$PINNED_FINGERPRINTS" = "$EXPECTED_HOST_KEY_SHA256" ] || {
    echo "ERROR: known_hosts must contain exactly one key for $TARGET with fingerprint $EXPECTED_HOST_KEY_SHA256" >&2
    exit 1
}

STAMP="$(date -u +"%Y%m%dT%H%M%SZ")"
SAFE_TARGET="$(printf '%s' "$TARGET" | tr -c 'A-Za-z0-9_.=-' '-')"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
if [ -z "$LOCAL_BACKUP_DIR" ]; then
    BACKUP_PARENT="$PROJECT_ROOT/output/am2-backups"
    "$PYTHON_BIN" "$DURABLE_IO" mkdir "$BACKUP_PARENT" \
        --mode 700 --parents --exist-ok >/dev/null || {
        echo "ERROR: cannot durably create backup parent: $BACKUP_PARENT" >&2
        exit 1
    }
    LOCAL_BACKUP_DIR="$BACKUP_PARENT/${SAFE_TARGET}-${STAMP}-$$"
else
    BACKUP_PARENT="$(dirname "$LOCAL_BACKUP_DIR")"
    "$PYTHON_BIN" "$DURABLE_IO" mkdir "$BACKUP_PARENT" \
        --mode 700 --parents --exist-ok >/dev/null || {
        echo "ERROR: cannot durably create backup parent: $BACKUP_PARENT" >&2
        exit 1
    }
fi
if [ -e "$LOCAL_BACKUP_DIR" ] || [ -L "$LOCAL_BACKUP_DIR" ]; then
    echo "ERROR: backup directory must be fresh and absent: $LOCAL_BACKUP_DIR" >&2
    exit 1
fi
"$PYTHON_BIN" "$DURABLE_IO" mkdir "$LOCAL_BACKUP_DIR" \
    --mode 700 >/dev/null || {
    echo "ERROR: cannot create backup directory: $LOCAL_BACKUP_DIR" >&2
    exit 1
}

LOGFILE="$LOCAL_BACKUP_DIR/backup_${STAMP}.log"
RESULTS_TSV="$LOCAL_BACKUP_DIR/.results.tsv"
SHA256SUMS="$LOCAL_BACKUP_DIR/SHA256SUMS"
RUNTIME_EVIDENCE="$LOCAL_BACKUP_DIR/runtime_admission.txt"
OUTPUT_MANIFEST="$LOCAL_BACKUP_DIR/am2_nand_backup_${SAFE_TARGET}_${STAMP}.manifest.json"
: > "$LOGFILE"
: > "$RESULTS_TSV"
: > "$SHA256SUMS"
: > "$RUNTIME_EVIDENCE"
chmod 600 "$RUNTIME_EVIDENCE"
"$PYTHON_BIN" "$DURABLE_IO" fsync-directories \
    "$LOCAL_BACKUP_DIR" >/dev/null || {
    echo "ERROR: cannot durably record initial AM2 evidence files" >&2
    exit 1
}

log() {
    local message="[$(date -u +"%Y-%m-%dT%H:%M:%SZ")] $*"
    printf '%s\n' "$message"
    printf '%s\n' "$message" >> "$LOGFILE"
}
die() {
    log "FATAL: $*"
    exit 1
}

if [ "$SKIP_SIZE_CHECK" != "1" ]; then
    REQUIRED_KB=$((MIN_FREE_MB * 1024 * 2))
    FREE_KB="$(df -Pk "$LOCAL_BACKUP_DIR" 2>/dev/null | awk 'NR==2 {print $4}')"
    case "$FREE_KB" in ''|*[!0-9]*) die "Could not establish host free space" ;; esac
    [ "$FREE_KB" -ge "$REQUIRED_KB" ] || \
        die "Insufficient host space: ${FREE_KB} KB < ${REQUIRED_KB} KB"
else
    log "Host free-space check skipped (lab-only override)"
fi

SSH_OPTS=(
    -o StrictHostKeyChecking=yes
    -o "UserKnownHostsFile=$KNOWN_HOSTS"
    -o GlobalKnownHostsFile=/dev/null
    -o ConnectTimeout=10
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=3
    -o LogLevel=ERROR
)
ssh_password_set() { [ -n "${!SSH_PASSWORD_ENV:-}" ]; }
ssh_exec_timeout() {
    local seconds="$1"
    local command="$2"
    if ssh_password_set; then
        command -v sshpass >/dev/null 2>&1 || die "sshpass is required for password authentication"
        SSHPASS="${!SSH_PASSWORD_ENV}" timeout "$seconds" sshpass -e ssh \
            "${SSH_OPTS[@]}" "${SSH_USER}@${TARGET}" "$command"
    else
        timeout "$seconds" ssh "${SSH_OPTS[@]}" -o BatchMode=yes \
            "${SSH_USER}@${TARGET}" "$command"
    fi
}
RUNTIME_COMMAND="$(cat <<'REMOTE'
printf 'boot_id='; cat /proc/sys/kernel/random/boot_id 2>/dev/null | tr -d '[:space:]'; printf '\n'
root_source=$(awk '$2 == "/" {print $1; exit}' /proc/mounts 2>/dev/null); printf 'root_source=%s\n' "$root_source"
root_base=${root_source#/dev/}; root_base=${root_base%p[0-9]*}; printf 'root_removable='; cat "/sys/class/block/${root_base}/removable" 2>/dev/null | tr -d '[:space:]'; printf '\n'
printf 'pgrep='; command -v pgrep 2>/dev/null || true; printf '\n'
printf 'writable_mtd_mounts='; awk 'function isrw(options,n,parts,i) {n=split(options,parts,","); for(i=1;i<=n;i++) if(parts[i]=="rw") return 1; return 0} ($1 ~ /^\/dev\/mtd(block)?[0-9]+$/ || $1 ~ /^mtd([0-9]+)?:/ || $1 ~ /^ubi[0-9]+:/ || $3 == "jffs2" || $3 == "ubifs") && isrw($4) {count++} END {print count+0}' /proc/mounts 2>/dev/null
miner_pids=$(pgrep -f '[b]osminer|[b]mminer|[c]gminer|[d]centrald' 2>/dev/null); miner_rc=$?; case $miner_rc in 0) printf 'miners_status=matches\nminers=present\n';; 1) printf 'miners_status=no_matches\nminers=\n';; *) printf 'miners_status=error\nminers=error\n';; esac
writer_pids=$(pgrep -f '[f]w_setenv|[n]andwrite|[n]andtest|[n]andmarkbad|[f]lash_erase|[f]lashcp|[m]td_debug|[m]tdpart|[u]biformat|[u]biupdatevol|[u]bimkvol|[u]birmvol|[u]birsvol|[u]biattach|[u]bidetach|[s]ysupgrade|[o]pkg|[/]dev/[m]td|[/]dev/[u]bi|(^|/|[[:space:]])[d]d([[:space:]]|$)' 2>/dev/null); writer_rc=$?; case $writer_rc in 0) printf 'writers_status=matches\nwriters=present\n';; 1) printf 'writers_status=no_matches\nwriters=\n';; *) printf 'writers_status=error\nwriters=error\n';; esac
REMOTE
)"
BACKUP_BOOT_ID=""
RUNTIME_GATE_COUNT=0
runtime_field() {
    local evidence="$1"
    local key="$2"
    printf '%s\n' "$evidence" | awk -F= -v wanted="$key" \
        '$1 == wanted {print substr($0, length($1) + 2); exit}'
}
runtime_gate() {
    local evidence shape boot_id root_source root_removable pgrep_path
    local writable_mtd_mounts miners_status miners writers_status writers
    evidence="$(ssh_exec_timeout 30 "$RUNTIME_COMMAND")" || \
        die "Could not classify target runtime safety state"
    shape="$(printf '%s\n' "$evidence" | awk -F= '
        BEGIN {
            n = split("boot_id root_source root_removable pgrep writable_mtd_mounts miners_status miners writers_status writers", expected, " ")
            for (i = 1; i <= n; i++) allowed[expected[i]] = 1
        }
        {
            key = $1
            if (!(key in allowed)) bad = 1
            count[key] += 1
        }
        END {
            for (i = 1; i <= n; i++) if (count[expected[i]] != 1) bad = 1
            print bad + 0
        }
    ')"
    [ "$shape" = "0" ] || die "Runtime safety evidence fields are not exact"
    boot_id="$(runtime_field "$evidence" boot_id)"
    root_source="$(runtime_field "$evidence" root_source)"
    root_removable="$(runtime_field "$evidence" root_removable)"
    pgrep_path="$(runtime_field "$evidence" pgrep)"
    writable_mtd_mounts="$(runtime_field "$evidence" writable_mtd_mounts)"
    miners_status="$(runtime_field "$evidence" miners_status)"
    miners="$(runtime_field "$evidence" miners)"
    writers_status="$(runtime_field "$evidence" writers_status)"
    writers="$(runtime_field "$evidence" writers)"
    printf '%s\n' "$boot_id" | \
        grep -Eq '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$' || \
        die "Target boot_id is malformed or unavailable"
    if [ -z "$BACKUP_BOOT_ID" ]; then
        BACKUP_BOOT_ID="$boot_id"
    elif [ "$boot_id" != "$BACKUP_BOOT_ID" ]; then
        die "Target rebooted during backup"
    fi
    [ "$root_source" = "$EXPECTED_ROOT_DEVICE" ] && \
        [ "$root_removable" = "1" ] || \
        die "External removable root admission changed or mismatched the plan"
    case "$pgrep_path" in /*) ;; *) die "Target pgrep tool is unavailable" ;; esac
    [ "$writable_mtd_mounts" = "0" ] || \
        die "Writable MTD/UBI mount is present or unclassified"
    [ "$miners_status" = "no_matches" ] && [ -z "$miners" ] || \
        die "Miner process state is active or unclassified"
    [ "$writers_status" = "no_matches" ] && [ -z "$writers" ] || \
        die "Flash/update writer state is active or unclassified"
    RUNTIME_GATE_COUNT=$((RUNTIME_GATE_COUNT + 1))
    {
        printf 'gate=%s\n' "$RUNTIME_GATE_COUNT"
        printf '%s\n' "$evidence"
        printf '\n'
    } >> "$RUNTIME_EVIDENCE"
}
LOCAL_TMP=""
READBACK_TMP=""
MANIFEST_TMP=""
cleanup_staging() {
    if [ -n "${LOCAL_TMP:-}" ]; then
        rm -f -- "$LOCAL_TMP"
    fi
    if [ -n "${MANIFEST_TMP:-}" ]; then
        rm -f -- "$MANIFEST_TMP"
    fi
    if [ -n "${READBACK_TMP:-}" ]; then
        rm -f -- "$READBACK_TMP"
    fi
}
trap cleanup_staging EXIT
trap 'exit 1' HUP INT TERM

log "=== AM2 NAND Backup Execution (host-side) ==="
log "Target endpoint: $TARGET"
log "Authorized board target: $EXPECTED_BOARD_TARGET"
log "Layout: $LAYOUT ($PARTITION_COUNT partitions)"
log "Per-read stream timeout: ${DUMP_TIMEOUT}s"

OBSERVED_MAC="$(
    ssh_exec_timeout 30 'tr "A-F" "a-f" </sys/class/net/eth0/address 2>/dev/null | tr -d "\r\n"'
)" || die "Could not read target MAC"
OBSERVED_HWID="$(
    ssh_exec_timeout 30 'cat /config/CONF_HARDWARE_ID 2>/dev/null | tr -d "\r\n"'
)" || die "Could not read target HWID"
OBSERVED_MODEL="$(
    ssh_exec_timeout 30 'cat /config/CONF_MINER_TYPE 2>/dev/null | tr -d "[:space:]"'
)" || die "Could not read target model"
OBSERVED_COMPATIBLE="$(
    ssh_exec_timeout 30 '(cat /proc/device-tree/compatible 2>/dev/null || cat /sys/firmware/devicetree/base/compatible 2>/dev/null) | tr "\000" "\n" | sed "s/,/_/g" | sed -n "1p"'
)" || die "Could not inspect target compatible identity"
OBSERVED_BOARD_TARGET="$(
    ssh_exec_timeout 30 'cat /etc/dcentos/board_target 2>/dev/null | tr -d "[:space:]" || true'
)" || die "Could not inspect installed board target"
if [ "$OBSERVED_MAC" != "$EXPECTED_MAC" ] || \
    [ "$OBSERVED_HWID" != "$EXPECTED_HWID" ] || \
    [ "$OBSERVED_MODEL" != "$EXPECTED_MODEL" ]; then
    die "Target identity mismatch (expected mac=$EXPECTED_MAC hwid=$EXPECTED_HWID model=$EXPECTED_MODEL; observed mac=$OBSERVED_MAC hwid=$OBSERVED_HWID model=$OBSERVED_MODEL)"
fi
if [ "$OBSERVED_COMPATIBLE" != "$EXPECTED_COMPATIBLE" ]; then
    die "Compatible identity mismatch (expected=$EXPECTED_COMPATIBLE observed=$OBSERVED_COMPATIBLE)"
fi
if [ "$OBSERVED_BOARD_TARGET" != "$EXPECTED_BOARD_TARGET" ]; then
    die "Installed board_target mismatch (expected=$EXPECTED_BOARD_TARGET observed=$OBSERVED_BOARD_TARGET)"
fi
log "Physical identity matches plan (mac=$OBSERVED_MAC hwid=$OBSERVED_HWID model=$OBSERVED_MODEL compatible=$OBSERVED_COMPATIBLE)"

runtime_gate
TARGET_PROC_MTD="$(ssh_exec_timeout 30 'cat /proc/mtd 2>/dev/null')" || \
    die "Could not read target /proc/mtd"
printf '%s\n' "$TARGET_PROC_MTD" >> "$LOGFILE"
TARGET_GEOMETRY="$(printf '%s\n' "$TARGET_PROC_MTD" | awk '
    /^mtd[0-9]+:/ {
        mtd = $1
        sub(/:$/, "", mtd)
        name = $4
        gsub(/"/, "", name)
        sub(/\r$/, "", name)
        printf "%s:%s:%s:%s\n", tolower(mtd), tolower($2), tolower($3), name
    }
' | paste -sd ' ' -)"
[ "$TARGET_GEOMETRY" = "$PLAN_GEOMETRY" ] || {
    log "Plan geometry:   $PLAN_GEOMETRY"
    log "Target geometry: $TARGET_GEOMETRY"
    die "Exact ordered /proc/mtd geometry mismatch"
}
log "Exact ordered /proc/mtd geometry matches the plan"

ssh_exec_timeout 30 "command -v nanddump >/dev/null 2>&1" || \
    die "Target is missing required tool: nanddump"
runtime_gate
log "External removable root and quiescent runtime gates passed"

get_row() {
    local wanted="$1"
    printf '%s\n' "$PARTITION_ROWS" | awk -F'|' -v n="$wanted" '$1 == n {print; exit}'
}
ORDER="4 0 1 2 3 5 6 7 8 9"
PASS_COUNT=0
TOTAL_BYTES=0

for mtd_number in $ORDER; do
    row="$(get_row "$mtd_number")"
    [ -n "$row" ] || die "Plan lost mtd$mtd_number"
    IFS='|' read -r _ name size_bytes artifact <<< "$row"
    log "Dumping mtd$mtd_number ($name), exact bytes=$size_bytes"
    runtime_gate

    LOCAL_TMP="$(mktemp "$LOCAL_BACKUP_DIR/.${artifact}.part.XXXXXX")" || \
        die "Could not allocate local staging file for $artifact"
    if ! ssh_exec_timeout "$DUMP_TIMEOUT" \
            "nanddump --bb=padbad --omitoob /dev/mtd$mtd_number" \
            > "$LOCAL_TMP" 2>> "$LOGFILE"; then
        rm -f -- "$LOCAL_TMP"
        LOCAL_TMP=""
        die "First nanddump stream failed or timed out for mtd$mtd_number"
    fi
    LOCAL_SIZE="$(stat -c %s "$LOCAL_TMP")"
    LOCAL_SHA="$(sha256sum "$LOCAL_TMP" | awk '{print $1}')"
    if [ "$LOCAL_SIZE" != "$size_bytes" ]; then
        rm -f -- "$LOCAL_TMP"
        LOCAL_TMP=""
        die "First stream size mismatch for $artifact: $LOCAL_SIZE != $size_bytes"
    fi

    READBACK_TMP="$(mktemp "$LOCAL_BACKUP_DIR/.${artifact}.readback.XXXXXX")" || {
        rm -f -- "$LOCAL_TMP"
        LOCAL_TMP=""
        die "Could not allocate local readback staging file for $artifact"
    }
    runtime_gate
    if ! ssh_exec_timeout "$DUMP_TIMEOUT" \
            "nanddump --bb=padbad --omitoob /dev/mtd$mtd_number" \
            > "$READBACK_TMP" 2>> "$LOGFILE"; then
        rm -f -- "$LOCAL_TMP"
        rm -f -- "$READBACK_TMP"
        LOCAL_TMP=""
        READBACK_TMP=""
        die "Readback nanddump stream failed or timed out for mtd$mtd_number"
    fi
    READBACK_SIZE="$(stat -c %s "$READBACK_TMP")"
    READBACK_SHA="$(sha256sum "$READBACK_TMP" | awk '{print $1}')"
    if [ "$READBACK_SIZE" != "$size_bytes" ] || [ "$READBACK_SHA" != "$LOCAL_SHA" ]; then
        rm -f -- "$LOCAL_TMP"
        rm -f -- "$READBACK_TMP"
        LOCAL_TMP=""
        READBACK_TMP=""
        die "Readback mismatch for $artifact"
    fi
    runtime_gate
    rm -f -- "$READBACK_TMP"
    READBACK_TMP=""

    "$PYTHON_BIN" "$ATOMIC_PUBLISHER" --require-directory-sync "$LOCAL_TMP" \
        "$LOCAL_BACKUP_DIR/$artifact" >/dev/null || {
        rm -f -- "$LOCAL_TMP"
        die "Could not publish local artifact: $artifact"
    }
    LOCAL_TMP=""
    printf '%s  %s\n' "$LOCAL_SHA" "$artifact" >> "$SHA256SUMS"
    printf '%s\t%s\t%s\t%s\t%s\t%s\tpass\n' \
        "$mtd_number" "$name" "$size_bytes" "$artifact" \
        "$LOCAL_SIZE" "$LOCAL_SHA" >> "$RESULTS_TSV"
    PASS_COUNT=$((PASS_COUNT + 1))
    TOTAL_BYTES=$((TOTAL_BYTES + LOCAL_SIZE))
    log "Accepted $artifact (sha256=$LOCAL_SHA)"
done

[ "$PASS_COUNT" = "$PARTITION_COUNT" ] || die "Incomplete per-run artifact set"
EXPECTED_RUNTIME_GATE_COUNT=$((2 + 3 * PARTITION_COUNT))
[ "$RUNTIME_GATE_COUNT" = "$EXPECTED_RUNTIME_GATE_COUNT" ] || \
    die "Runtime admission transcript is incomplete"
(cd "$LOCAL_BACKUP_DIR" && sha256sum -c SHA256SUMS) >> "$LOGFILE" 2>&1 || \
    die "Host SHA256SUMS verification failed"
"$PYTHON_BIN" "$DURABLE_IO" fsync-files \
    "$LOGFILE" "$SHA256SUMS" "$RUNTIME_EVIDENCE" >/dev/null || \
    die "Could not durably flush AM2 result evidence"
RUNTIME_EVIDENCE_SHA256="$(sha256sum "$RUNTIME_EVIDENCE" | awk '{print $1}')"
EXTERNAL_ROOT_DEVICE="${EXPECTED_ROOT_DEVICE#/dev/}"

MANIFEST_TMP="$(mktemp "${OUTPUT_MANIFEST}.publication-pending.XXXXXX")" || \
    die "Could not allocate result-manifest staging file"
{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am2_nand_backup_result",\n'
    printf '  "execution_utc": "%s",\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    printf '  "target": {\n'
    printf '    "ip": "%s",\n' "$TARGET"
    printf '    "mac": "%s",\n' "$OBSERVED_MAC"
    printf '    "hwid": "%s",\n' "$OBSERVED_HWID"
    printf '    "model": "%s",\n' "$OBSERVED_MODEL"
    printf '    "authorized_board_target": "%s",\n' "$EXPECTED_BOARD_TARGET"
    printf '    "compatible": "%s",\n' "$OBSERVED_COMPATIBLE"
    printf '    "ssh_host_key_sha256": "%s",\n' "$EXPECTED_HOST_KEY_SHA256"
    printf '    "external_root_device": "%s",\n' "$EXTERNAL_ROOT_DEVICE"
    printf '    "sd_recovery_proof_sha256": "%s",\n' "$SD_RECOVERY_PROOF_SHA256"
    printf '    "sd_recovery_boot_id": "%s",\n' "$SD_RECOVERY_BOOT_ID"
    printf '    "backup_boot_id": "%s",\n' "$BACKUP_BOOT_ID"
    printf '    "sd_recovery_quiescence": "%s",\n' "$SD_RECOVERY_QUIESCENCE"
    printf '    "runtime_admission": "pass_%s_exact_gates_single_boot_known_writer_scan_clear",\n' "$RUNTIME_GATE_COUNT"
    printf '    "runtime_evidence_file": "%s",\n' "$(basename "$RUNTIME_EVIDENCE")"
    printf '    "runtime_evidence_sha256": "%s",\n' "$RUNTIME_EVIDENCE_SHA256"
    printf '    "class": "am2 Zynq 7007S",\n'
    printf '    "layout": "braiinsos-dual-slot"\n'
    printf '  },\n'
    printf '  "readback_verify": 1,\n'
    printf '  "readback_failures": 0,\n'
    printf '  "partitions": [\n'
    first=1
    while IFS=$'\t' read -r number name size artifact actual digest status; do
        [ -n "$number" ] || continue
        if [ "$first" = "1" ]; then first=0; else printf ',\n'; fi
        printf '    {\n'
        printf '      "device": "/dev/mtd%s",\n' "$number"
        printf '      "mtd_number": %s,\n' "$number"
        printf '      "name": "%s",\n' "$name"
        printf '      "size_bytes": %s,\n' "$size"
        printf '      "artifact": "%s",\n' "$artifact"
        printf '      "sha256": "%s",\n' "$digest"
        printf '      "actual_bytes": %s,\n' "$actual"
        printf '      "status": "%s"\n' "$status"
        printf '    }'
    done < "$RESULTS_TSV"
    printf '\n  ],\n'
    printf '  "verification": {\n'
    printf '    "expected_artifact_count": %s,\n' "$PARTITION_COUNT"
    printf '    "actual_artifact_count": %s,\n' "$PASS_COUNT"
    printf '    "fail_count": 0,\n'
    printf '    "readback_failures": 0,\n'
    printf '    "total_bytes": %s,\n' "$TOTAL_BYTES"
    printf '    "sha256sums_file": "SHA256SUMS",\n'
    printf '    "log_file": "%s"\n' "$(basename "$LOGFILE")"
    printf '  },\n'
    printf '  "nand_backup_complete": "pass"\n'
    printf '}\n'
} > "$MANIFEST_TMP"

"$PYTHON_BIN" "$RESULT_VALIDATOR" \
    --manifest "$MANIFEST_TMP" \
    --local-backup-dir "$LOCAL_BACKUP_DIR" \
    --expected-target "$TARGET" \
    --expected-mac "$EXPECTED_MAC" \
    --expected-hwid "$EXPECTED_HWID" \
    --expected-model "$EXPECTED_MODEL" \
    --expected-board-target "$EXPECTED_BOARD_TARGET" \
    --expected-compatible "$EXPECTED_COMPATIBLE" \
    --expected-host-key-sha256 "$EXPECTED_HOST_KEY_SHA256" \
    --expected-external-root-device "$EXTERNAL_ROOT_DEVICE" \
    --expected-sd-recovery-proof-sha256 "$SD_RECOVERY_PROOF_SHA256" \
    --expected-sd-recovery-boot-id "$SD_RECOVERY_BOOT_ID" \
    --expected-backup-boot-id "$BACKUP_BOOT_ID" \
    --expected-sd-recovery-quiescence "$SD_RECOVERY_QUIESCENCE" \
    --expected-runtime-admission "pass_${RUNTIME_GATE_COUNT}_exact_gates_single_boot_known_writer_scan_clear" \
    --expected-runtime-evidence-file "$(basename "$RUNTIME_EVIDENCE")" \
    --expected-runtime-evidence-sha256 "$RUNTIME_EVIDENCE_SHA256" \
    --max-age-seconds 86400 >/dev/null || \
    die "Strict AM2 result admission failed before publication"
rm -f -- "$RESULTS_TSV" || \
    die "Could not retire private AM2 result scratch data"
"$PYTHON_BIN" "$ATOMIC_PUBLISHER" --require-directory-sync \
    "$MANIFEST_TMP" "$OUTPUT_MANIFEST" >/dev/null || \
    die "Could not atomically publish AM2 result manifest"
MANIFEST_TMP=""

trap - EXIT HUP INT TERM

FINAL_LOG_STATUS=0
log "nand_backup_complete=pass" || FINAL_LOG_STATUS=1
log "manifest=$OUTPUT_MANIFEST" || FINAL_LOG_STATUS=1
log "backup_dir=$LOCAL_BACKUP_DIR" || FINAL_LOG_STATUS=1
[ "$FINAL_LOG_STATUS" = "0" ] || \
    echo "WARN: committed AM2 result, but final informational log writes were incomplete" >&2
"$PYTHON_BIN" "$DURABLE_IO" fsync-files "$LOGFILE" >/dev/null || \
    echo "WARN: committed AM2 result, but final informational log lines did not flush" >&2
exit 0
