#!/usr/bin/env bash
# Convert retained LuxOS recon evidence into an exact, planning-only AM3 manifest.

set -euo pipefail
umask 077

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ATOMIC_PUBLISHER="$SCRIPT_DIR/atomic_publish_file.py"
DURABLE_IO="$SCRIPT_DIR/durable_file_io.py"

EVIDENCE=""
OUTPUT=""

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage: am3_bb_mtd_backup_manifest.sh --evidence <recon.txt> [--output <manifest.md>]

Local parser only. It performs no network or MTD operation and grants no read
or write authority. The exact 12-row geometry and LuxOS root marker are required.
USAGE
    exit "$code"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --evidence) EVIDENCE="${2:?missing evidence}"; shift 2 ;;
        --evidence=*) EVIDENCE="${1#*=}"; shift ;;
        --output) OUTPUT="${2:?missing output}"; shift 2 ;;
        --output=*) OUTPUT="${1#*=}"; shift ;;
        -h|--help) usage 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage ;;
    esac
done
[ -n "$EVIDENCE" ] && [ -f "$EVIDENCE" ] && [ ! -L "$EVIDENCE" ] || {
    echo "ERROR: --evidence must be a regular non-symlink file" >&2; exit 1;
}
PYTHON_BIN="${PYTHON:-}"
if [ -z "$PYTHON_BIN" ]; then
    PYTHON_BIN="$(command -v python3 || command -v python || true)"
fi
[ -n "$PYTHON_BIN" ] || { echo "ERROR: Python is required for durable manifest publication" >&2; exit 1; }
for helper in "$ATOMIC_PUBLISHER" "$DURABLE_IO"; do
    [ -f "$helper" ] || { echo "ERROR: required publication helper is missing: $helper" >&2; exit 1; }
done
if [ -z "$OUTPUT" ]; then
    case "$EVIDENCE" in *.txt) OUTPUT="${EVIDENCE%.txt}_mtd_backup_manifest.md" ;; *) OUTPUT="$EVIDENCE.mtd-backup-manifest.md" ;; esac
fi
[ ! -e "$OUTPUT" ] && [ ! -L "$OUTPUT" ] || {
    echo "ERROR: refusing to replace existing manifest: $OUTPUT" >&2; exit 1;
}
"$PYTHON_BIN" "$DURABLE_IO" mkdir "$(dirname "$OUTPUT")" \
    --mode 700 --parents --exist-ok >/dev/null || {
    echo "ERROR: cannot durably create manifest parent directory" >&2; exit 1;
}

MTD_LINES="$(awk '
    /^=== mtd layout ===/ {inside=1; next}
    /^=== / && inside {inside=0}
    inside && /^mtd[0-9]+:/ {print}
' "$EVIDENCE")"
NORMALIZED="$(printf '%s\n' "$MTD_LINES" | awk '
    /^mtd[0-9]+:/ {
        node=$1; sub(/:$/, "", node); name=$4; gsub(/"/, "", name); sub(/\r$/, "", name)
        printf "%s:%s:%s:%s\n", tolower(node), tolower($2), tolower($3), name
    }
')"
EXPECTED='mtd0:00020000:00020000:spl
mtd1:00020000:00020000:spl_backup1
mtd2:00020000:00020000:spl_backup2
mtd3:00020000:00020000:spl_backup3
mtd4:001c0000:00020000:u-boot
mtd5:00020000:00020000:bootenv
mtd6:00020000:00020000:fdt
mtd7:00500000:00020000:kernel
mtd8:01400000:00020000:root
mtd9:00200000:00020000:config
mtd10:00200000:00020000:sig
mtd11:06000000:00020000:nvdata'
LAYOUT_OK=0
[ "$NORMALIZED" = "$EXPECTED" ] && LAYOUT_OK=1
ROOT_OK=0
CMDLINE_SECTION_COUNT="$(grep -Fxc '=== proc cmdline ===' "$EVIDENCE" || true)"
CMDLINE="$(awk '
    $0 == "=== proc cmdline ===" {inside=1; next}
    /^=== / && inside {inside=0}
    inside && length($0) {print}
' "$EVIDENCE")"
CMDLINE_LINE_COUNT="$(printf '%s\n' "$CMDLINE" | sed '/^$/d' | wc -l | awk '{print $1}')"
ROOT_TOKENS="$(printf '%s\n' "$CMDLINE" | awk '{for (i=1; i<=NF; i++) if ($i ~ /^root=/) print $i}')"
ROOT_TOKEN_COUNT="$(printf '%s\n' "$ROOT_TOKENS" | sed '/^$/d' | wc -l | awk '{print $1}')"
if [ "$CMDLINE_SECTION_COUNT" = 1 ] && [ "$CMDLINE_LINE_COUNT" = 1 ] && \
    [ "$ROOT_TOKEN_COUNT" = 1 ] && [ "$ROOT_TOKENS" = 'root=/dev/mtdblock11' ]; then
    ROOT_OK=1
fi

TMP=""
ALLOCATION_SIGNAL=0
cleanup_tmp() {
    [ -z "${TMP:-}" ] || rm -f -- "$TMP"
}
allocate_pending_file() {
    local prefix="$1" attempt candidate
    ALLOCATION_SIGNAL=0
    trap 'ALLOCATION_SIGNAL=1' HUP INT TERM
    for ((attempt = 0; attempt < 32; attempt++)); do
        if [ "$ALLOCATION_SIGNAL" -ne 0 ]; then
            trap 'exit 1' HUP INT TERM
            return 2
        fi
        candidate="${prefix}.$$.$RANDOM$RANDOM"
        if (trap '' HUP INT TERM; set -o noclobber; : >"$candidate") 2>/dev/null; then
            TMP="$candidate"
            trap 'exit 1' HUP INT TERM
            [ "$ALLOCATION_SIGNAL" -eq 0 ] || return 2
            return 0
        fi
    done
    trap 'exit 1' HUP INT TERM
    return 1
}
trap cleanup_tmp EXIT
trap 'exit 1' HUP INT TERM
allocate_pending_file "${OUTPUT}.publication-pending." || {
    echo "ERROR: cannot allocate private manifest staging file" >&2; exit 1;
}
{
    echo '# AM3-BB MTD Backup Manifest'
    echo
    echo "- Created: \`$(date -u +'%Y-%m-%dT%H:%M:%SZ')\`"
    echo "- Source evidence: \`$(basename "$EVIDENCE")\`"
    echo '- Scope: Antminer S19j Pro BeagleBone/AM335x LuxOS'
    echo '- Status: planning-only; no NAND read/write authority'
    echo "- \`layout_profile_candidate=$LAYOUT_OK\`"
    echo "- \`root_mtdblock11_candidate=$ROOT_OK\`"
    echo '- `backup_scope=data-only-no-oob`'
    echo '- `restore_authority=none-until-physical-rehearsal`'
    echo '- `nand_backup_execute_go=0`'
    echo '- `nand_write_go=0`'
    echo '- `persistent_install_go=0`'
    echo
    echo '## Partition Table'
    echo
    echo '| Node | Size Hex | Erase Hex | Name | Required Artifact |'
    echo '| --- | --- | --- | --- | --- |'
    printf '%s\n' "$NORMALIZED" | awk -F: 'NF == 4 {
        node=$1; size=$2; erase=$3; name=$4
        printf "| /dev/%s | 0x%s | 0x%s | %s | %s_%s.nanddump |\n", node, size, erase, name, node, name
    }'
    echo
    echo '## Contract'
    echo
    echo 'Only a fresh exact-unit SD proof, restore-verified stock artifact, strict'
    echo 'JSON plan, explicit operator approval, and two identical bounded host-side'
    echo 'reads may admit a data-plane backup. OOB is omitted, so this manifest is'
    echo 'not restore authority. Physical restoration remains a separate NO-GO gate.'
    echo
    echo 'Future reads use `nanddump --bb=padbad --omitoob`; `skipbad`, short dumps,'
    echo 'stale output directories, target-local staging, and optional readback fail.'
} >"$TMP"
"$PYTHON_BIN" "$ATOMIC_PUBLISHER" --require-directory-sync \
    "$TMP" "$OUTPUT" >/dev/null || {
    echo "ERROR: durable manifest publication failed" >&2
    exit 1
}
TMP=""
trap - EXIT HUP INT TERM
{
    echo "wrote=$OUTPUT"
    echo "layout_profile_candidate=$LAYOUT_OK"
    echo "root_mtdblock11_candidate=$ROOT_OK"
} || true
exit 0
