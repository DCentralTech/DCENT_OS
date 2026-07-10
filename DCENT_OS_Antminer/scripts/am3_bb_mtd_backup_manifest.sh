#!/usr/bin/env bash
#
# Generate a local AM3-BB MTD backup manifest from checked-in read-only evidence.
#
# This script does not contact a miner and does not read or write MTD devices.
# It converts recon evidence into a reviewed backup checklist for the later
# console/recovery proof ladder.

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am3_bb_mtd_backup_manifest.sh --evidence <luxos_bbb_recon.txt> [--output <manifest.md>]

Options:
  --evidence <file>  Required read-only recon evidence file.
  --output <file>    Output markdown manifest. Default: <evidence>_mtd_backup_manifest.md
  -h, --help         Show this help.

Safety contract:
  - Local evidence parser only.
  - No SSH, no miner access, no flash/env reads, and no generated go decision.
USAGE
    exit "$code"
}

EVIDENCE=""
OUTPUT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --evidence)
            EVIDENCE="${2:?--evidence requires a file}"
            shift 2
            ;;
        --evidence=*)
            EVIDENCE="${1#--evidence=}"
            shift
            ;;
        --output)
            OUTPUT="${2:?--output requires a file}"
            shift 2
            ;;
        --output=*)
            OUTPUT="${1#--output=}"
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

[ -n "$EVIDENCE" ] || usage
[ -f "$EVIDENCE" ] || {
    echo "ERROR: evidence file not found: $EVIDENCE" >&2
    exit 1
}

if [ -z "$OUTPUT" ]; then
    case "$EVIDENCE" in
        *.txt)
            OUTPUT="${EVIDENCE%.txt}_mtd_backup_manifest.md"
            ;;
        *)
            OUTPUT="$EVIDENCE.mtd-backup-manifest.md"
            ;;
    esac
fi

mkdir -p "$(dirname "$OUTPUT")"

MTD_LINES="$(awk '
    /^=== mtd layout ===/ { in_mtd = 1; next }
    /^=== / && in_mtd { in_mtd = 0 }
    in_mtd && /^mtd[0-9]+:/ { print }
' "$EVIDENCE")"

[ -n "$MTD_LINES" ] || {
    echo "ERROR: no mtd layout block found in $EVIDENCE" >&2
    exit 1
}

EXPECTED_NAMES='spl spl_backup1 spl_backup2 spl_backup3 u-boot bootenv fdt kernel root config sig nvdata'
LAYOUT_OK=1
MISSING_NAMES=""
for name in $EXPECTED_NAMES; do
    if ! printf '%s\n' "$MTD_LINES" | grep -q "\"$name\""; then
        LAYOUT_OK=0
        MISSING_NAMES="$MISSING_NAMES $name"
    fi
done

ROOT_HINT="$(grep -m1 'root=/dev/mtdblock' "$EVIDENCE" 2>/dev/null || true)"
ROOT_OK=0
case "$ROOT_HINT" in
    *root=/dev/mtdblock11*)
        ROOT_OK=1
        ;;
esac

STAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
SOURCE_BASENAME="$(basename "$EVIDENCE")"

{
    echo "# AM3-BB MTD Backup Manifest"
    echo
    echo "- Created: \`$STAMP\`"
    echo "- Source evidence: \`$SOURCE_BASENAME\`"
    echo "- Scope: Antminer S19j Pro BeagleBone/AM335x LuxOS"
    echo "- Status: planning-only manifest"
    echo "- \`layout_profile_candidate=$LAYOUT_OK\`"
    echo "- \`root_mtdblock11_candidate=$ROOT_OK\`"
    echo "- \`nand_backup_execute_go=0\`"
    echo "- \`persistent_install_go=0\`"
    echo
    if [ "$LAYOUT_OK" != "1" ]; then
        echo "Missing expected MTD names:$MISSING_NAMES"
        echo
    fi
    echo "## Partition Table"
    echo
    echo "| Node | Size Hex | Erase Hex | Name | Required Future Artifact |"
    echo "| --- | --- | --- | --- | --- |"
    printf '%s\n' "$MTD_LINES" | awk '
        /^mtd[0-9]+:/ {
            node = $1
            sub(/:$/, "", node)
            size = $2
            erase = $3
            name = $4
            gsub(/"/, "", name)
            printf "| /dev/%s | 0x%s | 0x%s | %s | %s_%s.nanddump |\n", node, size, erase, name, node, name
        }
    '
    echo
    echo "## Execution Gate"
    echo
    echo "Do not run NAND backup commands yet. The backup ladder first needs:"
    echo
    echo "- Serial console or equivalent boot recovery proof."
    echo "- Restore-to-stock package/source artifact for this exact BeagleBone LuxOS lane."
    echo "- Operator-approved maintenance window and pool credential redaction plan."
    echo "- Hashes for every backup artifact captured on the host, not only on the miner."
    echo "- A dry-run parser that verifies every expected partition artifact exists and is non-empty."
    echo
    echo "## Future Command Template"
    echo
    echo "These are templates for a later authorized session, not commands approved by this manifest:"
    echo
    echo '```sh'
    printf '%s\n' "$MTD_LINES" | awk '
        /^mtd[0-9]+:/ {
            node = $1
            sub(/:$/, "", node)
            name = $4
            gsub(/"/, "", name)
            printf "# nanddump --bb=skipbad --omitoob -f /tmp/%s_%s.nanddump /dev/%s\n", node, name, node
        }
    '
    echo '```'
    echo
    echo "## Decision"
    echo
    echo "This manifest is evidence preparation only. It does not authorize NAND reads,"
    echo "NAND writes, bootenv changes, persistent install, native mining, or tap mining."
} > "$OUTPUT"

echo "wrote=$OUTPUT"
