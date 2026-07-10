#!/usr/bin/env bash
#
# Generate a local AM2 (S19 Pro / S19j Pro Zynq) MTD backup manifest from
# checked-in read-only evidence. Modeled after am3_bb_mtd_backup_manifest.sh.
#
# AM2 partition layout (per `a lab unit` U-Boot env extraction at
# ):
#   mtd0  boot              8 MiB
#   mtd1  boot-failover    12 MiB
#   mtd2  fpga1             2 MiB
#   mtd3  fpga2             2 MiB
#   mtd4  uboot_env       512 KiB
#   mtd5  miner_cfg       512 KiB
#   mtd6  recovery         87 MiB
#   mtd7  firmware1        57 MiB  (slot A)
#   mtd8  firmware2        57 MiB  (slot B)
#   mtd9  factory          30 MiB
#
# This script does not contact a miner and does not read or write MTD
# devices. It converts recon evidence into a reviewed backup checklist for
# the later console/recovery proof ladder.

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am2_nand_backup_manifest.sh --evidence <am2_recon.txt> [--output <manifest.md>]

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

# AM2 BraiinsOS-style layout (10 partitions).
EXPECTED_NAMES='boot boot-failover fpga1 fpga2 uboot_env miner_cfg recovery firmware1 firmware2 factory'
LAYOUT_OK=1
MISSING_NAMES=""
for name in $EXPECTED_NAMES; do
    if ! printf '%s\n' "$MTD_LINES" | grep -q "\"$name\""; then
        LAYOUT_OK=0
        MISSING_NAMES="$MISSING_NAMES $name"
    fi
done

# Detect active firmware slot from evidence (if recorded).
ACTIVE_SLOT=""
ACTIVE_HINT="$(grep -m1 'firmware=' "$EVIDENCE" 2>/dev/null | grep -E '^firmware=[12]' || true)"
case "$ACTIVE_HINT" in
    firmware=1*) ACTIVE_SLOT="1" ;;
    firmware=2*) ACTIVE_SLOT="2" ;;
esac

STAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
SOURCE_BASENAME="$(basename "$EVIDENCE")"

{
    echo "# AM2 (S19 Pro / S19j Pro Zynq) MTD Backup Manifest"
    echo
    echo "- Created: \`$STAMP\`"
    echo "- Source evidence: \`$SOURCE_BASENAME\`"
    echo "- Scope: Antminer S19 Pro / S19j Pro Zynq am2 (XC7Z020 BraiinsOS-class layout)"
    echo "- Status: planning-only manifest"
    echo "- \`layout_profile_candidate=$LAYOUT_OK\`"
    echo "- \`active_slot_candidate=${ACTIVE_SLOT:-unknown}\`"
    echo "- \`nand_backup_execute_go=0\`"
    echo "- \`persistent_install_go=0\`"
    echo
    if [ "$LAYOUT_OK" != "1" ]; then
        echo "Missing expected MTD names:$MISSING_NAMES"
        echo
        echo "If this unit has the **stock XIL single-slot layout** (\`mtd1=ramfs\`,"
        echo "no \`firmware1\`/\`firmware2\`), it is NOT eligible for the BraiinsOS-class"
        echo "dual-slot install. Convert to BraiinsOS first, OR build the stock-XIL"
        echo "recovery procedure (see \`revert_to_stock_xil.sh\` proposal in the DevOps"
        echo "Q1 report)."
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
    echo "- Serial console or equivalent boot recovery proof on this exact am2 unit."
    echo "- Restore-to-stock package/source artifact: either BraiinsOS am1-s9 SSH"
    echo "  package (for stock-XIL units) OR the matching DCENT_OS sysupgrade tarball."
    echo "- Operator-approved maintenance window and pool credential redaction plan."
    echo "- SHA256 manifest for every captured artifact verified BOTH on the miner"
    echo "  AND on the host after transfer."
    echo "- A readback dry-run that proves \`nanddump\` of the active slot produces"
    echo "  a byte-identical re-dump (idempotent read)."
    echo "- Particular care for **mtd4 (uboot_env)**: contains MAC, hwid, and"
    echo "  factory env. Corruption here = brick unless serial console recovery"
    echo "  is available."
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
