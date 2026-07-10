#!/usr/bin/env bash
#
# Generate a detailed NAND backup execution plan from a validated MTD manifest.
#
# This script is LOCAL-ONLY. It does NOT contact any miner, read any MTD device,
# write any flash or bootenv, or stop any services.
#
# Safety contract:
#   - Local planning script only. No network access.
#   - No SSH, no miner access, no flash reads/writes.
#   - No bootenv reads/writes.
#   - No service stops or daemon control.
#   - nand_backup_execute_go=0 in all outputs.
#   - Produces commented (non-executable) command templates only.
#
# Input: manifest markdown from am3_bb_mtd_backup_manifest.sh
# Output: detailed backup execution plan (markdown + JSON template)

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am3_bb_nand_backup_plan.sh --manifest <manifest.md> [options]

Options:
  --manifest <file>        Required. Manifest from am3_bb_mtd_backup_manifest.sh.
  --sd-recovery-proof <f>  SD recovery probe evidence file (from am3_bb_sd_recovery_probe.sh).
  --luxos-return-proof <f> Evidence proving return-to-LuxOS is verified.
  --output <file>          Output plan file. Default: <manifest>_backup_plan.md
  --json-template <file>   JSON manifest template path. Default: <manifest>_backup_manifest_template.json
  -h, --help               Show this help.

Pre-flight requirements (all must be provided for a READY plan):
  - SD recovery probe evidence with sd_recovery_probe=pass
  - Return-to-LuxOS proof
  - Manifest with layout_profile_candidate=1

Safety contract:
  - Local evidence parser only. No SSH, no miner access, no flash/env reads.
  - nand_backup_execute_go=0 always. This script produces the plan, not the execution.
USAGE
    exit "$code"
}

MANIFEST=""
SD_RECOVERY_PROOF=""
LUXOS_RETURN_PROOF=""
OUTPUT=""
JSON_TEMPLATE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --manifest)
            MANIFEST="${2:?--manifest requires a file}"
            shift 2
            ;;
        --manifest=*)
            MANIFEST="${1#--manifest=}"
            shift
            ;;
        --sd-recovery-proof)
            SD_RECOVERY_PROOF="${2:?--sd-recovery-proof requires a file}"
            shift 2
            ;;
        --sd-recovery-proof=*)
            SD_RECOVERY_PROOF="${1#--sd-recovery-proof=}"
            shift
            ;;
        --luxos-return-proof)
            LUXOS_RETURN_PROOF="${2:?--luxos-return-proof requires a file}"
            shift 2
            ;;
        --luxos-return-proof=*)
            LUXOS_RETURN_PROOF="${1#--luxos-return-proof=}"
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
        --json-template)
            JSON_TEMPLATE="${2:?--json-template requires a file}"
            shift 2
            ;;
        --json-template=*)
            JSON_TEMPLATE="${1#--json-template=}"
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

[ -n "$MANIFEST" ] || usage
[ -f "$MANIFEST" ] || {
    echo "ERROR: manifest file not found: $MANIFEST" >&2
    exit 1
}

if [ -z "$OUTPUT" ]; then
    case "$MANIFEST" in
        *_mtd_backup_manifest.md)
            OUTPUT="${MANIFEST%_mtd_backup_manifest.md}_backup_plan.md"
            ;;
        *.md)
            OUTPUT="${MANIFEST%.md}_backup_plan.md"
            ;;
        *)
            OUTPUT="${MANIFEST}.backup_plan.md"
            ;;
    esac
fi

if [ -z "$JSON_TEMPLATE" ]; then
    case "$MANIFEST" in
        *_mtd_backup_manifest.md)
            JSON_TEMPLATE="${MANIFEST%_mtd_backup_manifest.md}_backup_manifest_template.json"
            ;;
        *.md)
            JSON_TEMPLATE="${MANIFEST%.md}_backup_manifest_template.json"
            ;;
        *)
            JSON_TEMPLATE="${MANIFEST}.backup_manifest_template.json"
            ;;
    esac
fi

mkdir -p "$(dirname "$OUTPUT")"
mkdir -p "$(dirname "$JSON_TEMPLATE")"

# --- Validate manifest content ---

LAYOUT_OK=0
if grep -q 'layout_profile_candidate=1' "$MANIFEST"; then
    LAYOUT_OK=1
fi

ROOT_OK=0
if grep -q 'root_mtdblock11_candidate=1' "$MANIFEST"; then
    ROOT_OK=1
fi

# --- Validate pre-flight proofs ---

SD_RECOVERY_OK=0
SD_RECOVERY_STATUS="not_provided"
if [ -n "$SD_RECOVERY_PROOF" ]; then
    if [ -f "$SD_RECOVERY_PROOF" ]; then
        if grep -q 'sd_recovery_probe=pass' "$SD_RECOVERY_PROOF"; then
            SD_RECOVERY_OK=1
            SD_RECOVERY_STATUS="pass"
        else
            SD_RECOVERY_STATUS="fail_or_not_ready"
        fi
    else
        SD_RECOVERY_STATUS="file_not_found"
    fi
fi

LUXOS_RETURN_OK=0
LUXOS_RETURN_STATUS="not_provided"
if [ -n "$LUXOS_RETURN_PROOF" ]; then
    if [ -f "$LUXOS_RETURN_PROOF" ]; then
        LUXOS_RETURN_OK=1
        LUXOS_RETURN_STATUS="present"
    else
        LUXOS_RETURN_STATUS="file_not_found"
    fi
fi

# --- Extract partition table from manifest ---

# Parse the markdown table for partition details
# Expected format: | /dev/mtdN | 0xSIZE | 0xERASE | name | artifact |
PARTITIONS=""
while IFS='|' read -r _ node size erase name artifact _; do
    node="$(echo "$node" | xargs)"
    size="$(echo "$size" | xargs)"
    erase="$(echo "$erase" | xargs)"
    name="$(echo "$name" | xargs)"
    artifact="$(echo "$artifact" | xargs)"
    case "$node" in
        /dev/mtd*)
            PARTITIONS="${PARTITIONS}${node}|${size}|${erase}|${name}|${artifact}
"
            ;;
    esac
done < "$MANIFEST"

[ -n "$PARTITIONS" ] || {
    echo "ERROR: no partition table found in manifest: $MANIFEST" >&2
    exit 1
}

PARTITION_COUNT="$(printf '%s' "$PARTITIONS" | grep -c '/dev/mtd')"

# --- Compute expected sizes ---

hex_to_dec() {
    printf '%d' "$1" 2>/dev/null || echo 0
}

STAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
MANIFEST_BASENAME="$(basename "$MANIFEST")"

# --- Determine overall plan readiness ---

PLAN_READY=0
if [ "$LAYOUT_OK" = "1" ] && [ "$ROOT_OK" = "1" ] && [ "$SD_RECOVERY_OK" = "1" ] && [ "$LUXOS_RETURN_OK" = "1" ]; then
    PLAN_READY=1
fi

# --- Generate the backup plan ---

{
    echo "# AM3-BB NAND Backup Execution Plan"
    echo
    echo "- Generated: \`$STAMP\`"
    echo "- Source manifest: \`$MANIFEST_BASENAME\`"
    echo "- Target: Antminer S19j Pro BeagleBone/AM335x (203.0.113.79)"
    echo "- Partitions: $PARTITION_COUNT"
    echo "- \`nand_backup_execute_go=0\`"
    echo "- \`plan_ready=$PLAN_READY\`"
    echo
    echo "## Pre-Flight Checks"
    echo
    echo "All of the following must pass before any NAND backup read is authorized:"
    echo
    if [ "$SD_RECOVERY_OK" = "1" ]; then
        echo "- [x] SD recovery boot proof exists and passes (\`sd_recovery_probe=pass\`)"
    else
        echo "- [ ] SD recovery boot proof exists and passes (status: \`$SD_RECOVERY_STATUS\`)"
    fi
    if [ "$LUXOS_RETURN_OK" = "1" ]; then
        echo "- [x] Return-to-LuxOS proof exists"
    else
        echo "- [ ] Return-to-LuxOS proof exists (status: \`$LUXOS_RETURN_STATUS\`)"
    fi
    if [ "$LAYOUT_OK" = "1" ]; then
        echo "- [x] MTD layout profile validated (\`layout_profile_candidate=1\`)"
    else
        echo "- [ ] MTD layout profile validated (current: \`layout_profile_candidate=$LAYOUT_OK\`)"
    fi
    if [ "$ROOT_OK" = "1" ]; then
        echo "- [x] Root on mtdblock11 confirmed (\`root_mtdblock11_candidate=1\`)"
    else
        echo "- [ ] Root on mtdblock11 confirmed (current: \`root_mtdblock11_candidate=$ROOT_OK\`)"
    fi
    echo "- [ ] Operator explicit written approval for NAND reads"
    echo "- [ ] Mining stopped or maintenance window confirmed"
    echo "- [ ] Adequate storage at backup destination (minimum 130 MB free)"
    echo "- [ ] System booted from SD card (root is NOT /dev/mtdblock*)"
    echo "- [ ] DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable set"
    echo
    echo "## Execution Environment"
    echo
    echo "The backup MUST be executed from an SD recovery boot environment, NOT from"
    echo "the live LuxOS NAND system. This ensures:"
    echo
    echo "1. No filesystem is mounted read-write from NAND during the dump"
    echo "2. JFFS2 garbage collection cannot race with the raw read"
    echo "3. No running processes hold open files on the NAND partitions"
    echo "4. A failed backup cannot corrupt the running OS"
    echo
    echo "## Partition Backup Commands"
    echo
    echo "Each command below uses \`nanddump --bb=skipbad --omitoob\` to produce a"
    echo "clean dump without OOB data, skipping known-bad blocks. Output is written"
    echo "to a specified directory."
    echo
    echo "These commands are COMMENTED and NOT authorized for execution:"
    echo
    echo '```sh'
    echo '# --- AM3-BB NAND Full Backup ---'
    echo '# Environment: SD recovery boot on BeagleBone/AM335x'
    echo '# Target: Antminer S19j Pro at 203.0.113.79'
    echo '# Gate: DCENT_NAND_BACKUP_AUTHORIZED=1 must be set'
    echo '# Date: (fill at execution time)'
    echo '#'
    echo '# OUTDIR="/mnt/backup/am3_bb_nand_$(date -u +%Y%m%dT%H%M%SZ)"'
    echo '# mkdir -p "$OUTDIR"'
    echo '#'

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        mtd_dev="$(echo "$node" | sed 's|/dev/||')"
        size_dec="$(hex_to_dec "$size")"
        echo "#"
        printf '# Partition: %s (%s)\n' "$name" "$node"
        printf '# Expected size: %s (%d bytes)\n' "$size" "$size_dec"
        printf '# Erase block: %s\n' "$erase"
        printf '# nanddump --bb=skipbad --omitoob -f "$OUTDIR/%s" %s\n' "$artifact" "$node"
        printf '# sha256sum "$OUTDIR/%s" >> "$OUTDIR/SHA256SUMS"\n' "$artifact"
        printf '# echo "%s:%s:$(stat -c %%s "$OUTDIR/%s")" >> "$OUTDIR/sizes.txt"\n' "$name" "$artifact" "$artifact"
    done

    echo '#'
    echo '# --- Verification ---'
    echo '# sha256sum -c "$OUTDIR/SHA256SUMS"'
    echo '# wc -c "$OUTDIR"/*.nanddump'
    echo '```'
    echo

    echo "## Expected Artifact Sizes"
    echo
    echo "| Partition | Name | Hex Size | Decimal Bytes | Artifact File |"
    echo "| --- | --- | --- | --- | --- |"

    TOTAL_BYTES=0
    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        printf '| %s | %s | %s | %d | %s |\n' "$node" "$name" "$size" "$size_dec" "$artifact"
    done

    # Compute total
    TOTAL=0
    while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        TOTAL=$((TOTAL + size_dec))
    done <<EOF
$(printf '%s\n' "$PARTITIONS")
EOF
    echo
    printf "**Total expected raw dump size**: %d bytes (~%d MB)\n" "$TOTAL" "$((TOTAL / 1048576))"
    echo
    echo "Note: Actual nanddump output may be smaller if bad blocks are skipped."
    echo "The sizes above represent maximum expected output per partition."
    echo

    echo "## SHA256 Verification Commands"
    echo
    echo "After all partitions are dumped, verify integrity with:"
    echo
    echo '```sh'
    echo '# --- Post-Backup Verification ---'
    echo '# cd "$OUTDIR"'
    echo '#'
    echo '# # Verify each artifact hash matches manifest'
    echo '# sha256sum -c SHA256SUMS'
    echo '#'
    echo '# # Verify all 12 artifacts exist and are non-empty'
    echo '# for f in \'

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        printf '#   %s \\\n' "$artifact"
    done

    echo '#   ; do'
    echo '#     [ -s "$f" ] || echo "FAIL: missing or empty: $f"'
    echo '# done'
    echo '#'
    echo '# # Verify sizes are within expected range'

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        printf '# # %s: expect <= %d bytes\n' "$artifact" "$size_dec"
        printf '# actual=$(stat -c %%s "%s"); [ "$actual" -le %d ] || echo "WARN: %s larger than partition"\n' "$artifact" "$size_dec" "$artifact"
    done

    echo '```'
    echo

    echo "## Restore Verification Procedure"
    echo
    echo "After backup artifacts are captured, verify completeness before relying on them:"
    echo
    echo "1. **File count**: Exactly 12 \`.nanddump\` files must exist in the output directory."
    echo "2. **Non-empty**: Every file must have size > 0 bytes."
    echo "3. **Size bounds**: Each file size must be <= the partition hex size (bad block skip"
    echo "   means output can be smaller, never larger)."
    echo "4. **SHA256 match**: Re-compute SHA256 on the host machine after transfer and compare"
    echo "   to the on-device SHA256SUMS captured at dump time."
    echo "5. **Cross-device verify**: If artifacts are transferred off the SD card to a host PC,"
    echo "   re-run \`sha256sum -c SHA256SUMS\` on the host copy."
    echo "6. **Spot-check headers**: Validate known magic bytes:"
    echo "   - \`mtd0_spl.nanddump\`: Should start with AM335x MLO/SPL header"
    echo "   - \`mtd4_u-boot.nanddump\`: Should contain U-Boot signature strings"
    echo "   - \`mtd7_kernel.nanddump\`: Should start with zImage or uImage header"
    echo "   - \`mtd8_root.nanddump\`: Should contain JFFS2 magic (0x1985)"
    echo "   - \`mtd5_bootenv.nanddump\`: Should contain readable env key=value pairs"
    echo "7. **Idempotent re-dump**: If any artifact fails verification, re-dump that single"
    echo "   partition and compare SHA256. Bit-rot or bad-block progression during backup is"
    echo "   unlikely but detectable this way."
    echo

    echo "## Sensitive Data Handling"
    echo
    echo "The following partitions may contain sensitive data that should be handled carefully:"
    echo
    echo "- \`mtd5_bootenv.nanddump\` (bootenv): May contain network config, MAC addresses"
    echo "- \`mtd9_config.nanddump\` (config): May contain pool credentials, WiFi passwords"
    echo "- \`mtd11_nvdata.nanddump\` (nvdata): May contain operational data, MAC, serial"
    echo
    echo "**Redaction policy**: Do NOT commit raw backup artifacts to git. Store in a"
    echo "local encrypted directory or air-gapped media. Only commit the SHA256SUMS"
    echo "and sizes manifest (no raw content)."
    echo

    echo "## Decision"
    echo
    if [ "$PLAN_READY" = "1" ]; then
        echo "Pre-flight checks PASS. This plan is READY for operator review and explicit"
        echo "authorization. The operator must still:"
        echo
        echo "1. Confirm maintenance window"
        echo "2. Set \`DCENT_NAND_BACKUP_AUTHORIZED=1\`"
        echo "3. Boot from SD recovery image"
        echo "4. Run the backup execution script"
    else
        echo "Pre-flight checks INCOMPLETE. The following gates are not yet satisfied:"
        echo
        [ "$SD_RECOVERY_OK" = "1" ] || echo "- SD recovery boot proof: $SD_RECOVERY_STATUS"
        [ "$LUXOS_RETURN_OK" = "1" ] || echo "- Return-to-LuxOS proof: $LUXOS_RETURN_STATUS"
        [ "$LAYOUT_OK" = "1" ] || echo "- MTD layout validation: not passed"
        [ "$ROOT_OK" = "1" ] || echo "- Root mtdblock11 validation: not passed"
        echo
        echo "Resolve these gaps before requesting operator authorization."
    fi
    echo
    echo "This plan does NOT authorize NAND reads, NAND writes, bootenv changes,"
    echo "persistent install, native mining, or tap mining."
    echo
    echo "\`nand_backup_execute_go=0\`"
} > "$OUTPUT"

# --- Generate JSON manifest template ---

{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am3_bb_nand_backup_manifest",\n'
    printf '  "generated_utc": "%s",\n' "$STAMP"
    printf '  "target": {\n'
    printf '    "model": "Antminer S19j Pro",\n'
    printf '    "board": "BeagleBone_Black_v2.1 on S19J_IO_BOARD_V2_0",\n'
    printf '    "soc": "AM335x",\n'
    printf '    "ip": "203.0.113.79",\n'
    printf '    "firmware": "LuxOS/LUXminer"\n'
    printf '  },\n'
    printf '  "nand_backup_execute_go": 0,\n'
    printf '  "plan_ready": %d,\n' "$PLAN_READY"
    printf '  "pre_flight": {\n'
    printf '    "sd_recovery_proof": "%s",\n' "$SD_RECOVERY_STATUS"
    printf '    "luxos_return_proof": "%s",\n' "$LUXOS_RETURN_STATUS"
    printf '    "layout_profile": %d,\n' "$LAYOUT_OK"
    printf '    "root_mtdblock11": %d,\n' "$ROOT_OK"
    printf '    "operator_approval": false,\n'
    printf '    "mining_stopped": false,\n'
    printf '    "storage_adequate": false,\n'
    printf '    "sd_booted": false\n'
    printf '  },\n'
    printf '  "partitions": [\n'

    FIRST=1
    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        mtd_num="$(echo "$node" | sed 's|/dev/mtd||')"
        if [ "$FIRST" = "1" ]; then
            FIRST=0
        else
            printf ',\n'
        fi
        printf '    {\n'
        printf '      "device": "%s",\n' "$node"
        printf '      "mtd_number": %s,\n' "$mtd_num"
        printf '      "name": "%s",\n' "$name"
        printf '      "size_hex": "%s",\n' "$size"
        printf '      "size_bytes": %d,\n' "$size_dec"
        printf '      "erase_size_hex": "%s",\n' "$erase"
        printf '      "artifact": "%s",\n' "$artifact"
        printf '      "sha256": null,\n'
        printf '      "actual_bytes": null,\n'
        printf '      "dump_timestamp_utc": null,\n'
        printf '      "status": "pending"\n'
        printf '    }'
    done

    printf '\n  ],\n'
    printf '  "verification": {\n'
    printf '    "all_artifacts_exist": null,\n'
    printf '    "all_artifacts_nonempty": null,\n'
    printf '    "all_sha256_match": null,\n'
    printf '    "total_expected_bytes": %d,\n' "$TOTAL"
    printf '    "total_actual_bytes": null,\n'
    printf '    "nand_backup_complete": null\n'
    printf '  }\n'
    printf '}\n'
} > "$JSON_TEMPLATE"

echo "plan=$OUTPUT"
echo "json_template=$JSON_TEMPLATE"
echo "plan_ready=$PLAN_READY"
echo "nand_backup_execute_go=0"
