#!/usr/bin/env bash
#
# Generate a detailed AM1 (S9 Zynq) NAND backup execution plan from a
# validated MTD manifest. Modeled after `am2_nand_backup_plan.sh` and
# `am3_bb_nand_backup_plan.sh`.
#
# This script is LOCAL-ONLY. It does NOT contact any miner, read any MTD
# device, write any flash or bootenv, or stop any services.
#
# Safety contract:
#   - Local planning script only. No network access.
#   - No SSH, no miner access, no flash reads/writes.
#   - No bootenv reads/writes.
#   - No service stops or daemon control.
#   - nand_backup_execute_go=0 in all outputs.
#   - Produces commented (non-executable) command templates only.
#
# S9 partition map (am1 — Zynq XC7Z010 BraiinsOS-class):
#   mtd0  boot         512 KiB   FSBL
#   mtd1  uboot       2560 KiB   U-Boot bootloader
#   mtd2  fpga1       2048 KiB   FPGA bitstream (primary)
#   mtd3  fpga2       2048 KiB   FPGA bitstream (backup)
#   mtd4  uboot_env    512 KiB   U-Boot environment (MAC, hwid)
#   mtd5  miner_cfg    512 KiB   Miner configuration
#   mtd6  recovery    22528 KiB  Recovery firmware/ramdisk
#   mtd7  firmware1   97280 KiB  Slot A
#   mtd8  firmware2   97280 KiB  Slot B
#   mtd9  factory     36864 KiB  Factory calibration
#
# S9 stock Bitmain partition map (3 partitions, alternative scheme):
#   mtd0  boot         32 MiB    Monolithic BOOT.bin + DTB + kernel
#   mtd1  rootfs      144 MiB    Primary rootfs (UBI)
#   mtd2  upgrade      80 MiB    Upgrade slot

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am1_nand_backup_plan.sh --manifest <manifest.md> [options]

Options:
  --manifest <file>            Required. Manifest from am1_nand_backup_manifest.sh.
  --restore-artifact-proof <f> Restore-verified artifact evidence for this exact unit.
  --readback-verify            Mark the plan as requiring a post-dump re-read pass
                               (idempotent nanddump check) before flagging the
                               backup complete.
  --output <file>              Output plan file. Default: <manifest>_backup_plan.md
  --json-template <file>       JSON manifest template path. Default:
                               <manifest>_backup_manifest_template.json
  -h, --help                   Show this help.

Pre-flight requirements (all must be provided for a READY plan):
  - Manifest with layout_profile_candidate=1
  - Restore artifact evidence (matched to MAC / hwid of this unit)

Safety contract:
  - Local evidence parser only. No SSH, no miner access, no flash/env reads.
  - nand_backup_execute_go=0 always. This script produces the plan, not
    the execution.
USAGE
    exit "$code"
}

MANIFEST=""
RESTORE_PROOF=""
READBACK_VERIFY=0
OUTPUT=""
JSON_TEMPLATE=""
# CE-374: operator-supplied identity of THIS exact unit. A restore-verified
# marker alone is NOT proof for this unit — it must be bound to the unit's
# MAC/HWID (and optionally model/target).
EXPECT_MAC=""
EXPECT_HWID=""
EXPECT_MODEL=""
EXPECT_TARGET=""

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
        --restore-artifact-proof)
            RESTORE_PROOF="${2:?--restore-artifact-proof requires a file}"
            shift 2
            ;;
        --restore-artifact-proof=*)
            RESTORE_PROOF="${1#--restore-artifact-proof=}"
            shift
            ;;
        --expect-mac)
            EXPECT_MAC="${2:?--expect-mac requires a value}"
            shift 2
            ;;
        --expect-mac=*)
            EXPECT_MAC="${1#--expect-mac=}"
            shift
            ;;
        --expect-hwid)
            EXPECT_HWID="${2:?--expect-hwid requires a value}"
            shift 2
            ;;
        --expect-hwid=*)
            EXPECT_HWID="${1#--expect-hwid=}"
            shift
            ;;
        --expect-model)
            EXPECT_MODEL="${2:?--expect-model requires a value}"
            shift 2
            ;;
        --expect-model=*)
            EXPECT_MODEL="${1#--expect-model=}"
            shift
            ;;
        --expect-target)
            EXPECT_TARGET="${2:?--expect-target requires a value}"
            shift 2
            ;;
        --expect-target=*)
            EXPECT_TARGET="${1#--expect-target=}"
            shift
            ;;
        --readback-verify)
            READBACK_VERIFY=1
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

# Detect partition scheme (braiinsos = 10 partitions, stock = 3).
PARTITION_SCHEME="unknown"
if grep -q 'partition_scheme=braiinsos' "$MANIFEST"; then
    PARTITION_SCHEME="braiinsos"
elif grep -q 'partition_scheme=stock' "$MANIFEST"; then
    PARTITION_SCHEME="stock"
fi

# CE-374 helpers: extract a trimmed `key=value` field from the proof file
# (first match wins; awk reads the file directly so there is no SIGPIPE under
# `set -o pipefail`), and emit a JSON string-or-null.
proof_field() {
    awk -v k="$1" '
        $0 ~ "^[[:space:]]*" k "=" {
            sub("^[[:space:]]*" k "=", "", $0)
            gsub(/[[:space:]]/, "", $0)
            print
            exit
        }
    ' "$2"
}
json_or_null() {
    if [ -z "$1" ]; then
        printf 'null'
    else
        printf '"%s"' "$1"
    fi
}

# --- Validate pre-flight proofs (CE-374: identity-bound restore proof) ---
# A restore-verified marker alone is NOT proof for THIS unit — bind it to the
# exact MAC/HWID via the operator --expect-* args. RESTORE_PROOF_OK stays 0
# (so PLAN_READY stays 0) for every advisory/mismatch/malformed case; this only
# TIGHTENS the existing `LAYOUT_OK && RESTORE_PROOF_OK` gate, never relaxes it.
RESTORE_PROOF_OK=0
RESTORE_PROOF_STATUS="not_provided"
RESTORE_MATCHED_MAC=""
RESTORE_MATCHED_HWID=""
RESTORE_MATCHED_MODEL=""
RESTORE_ARTIFACT_SHA256=""
if [ -n "$RESTORE_PROOF" ]; then
    if [ -f "$RESTORE_PROOF" ]; then
        if grep -q 'restore_verified=1\|artifact_state=restore_verified' "$RESTORE_PROOF"; then
            PROOF_MAC="$(proof_field restore_mac "$RESTORE_PROOF" | tr 'A-F' 'a-f')"
            PROOF_HWID="$(proof_field restore_hwid "$RESTORE_PROOF")"
            PROOF_MODEL="$(proof_field restore_model "$RESTORE_PROOF")"
            PROOF_TARGET="$(proof_field restore_target "$RESTORE_PROOF")"
            PROOF_SHA="$(proof_field restore_artifact_sha256 "$RESTORE_PROOF" | tr 'A-F' 'a-f')"
            EXP_MAC_LC="$(printf '%s' "$EXPECT_MAC" | tr -d '[:space:]' | tr 'A-F' 'a-f')"
            EXP_HWID_LC="$(printf '%s' "$EXPECT_HWID" | tr -d '[:space:]')"
            EXP_MODEL_LC="$(printf '%s' "$EXPECT_MODEL" | tr -d '[:space:]')"
            EXP_TARGET_LC="$(printf '%s' "$EXPECT_TARGET" | tr -d '[:space:]')"
            if { grep -q 'restore_mac=' "$RESTORE_PROOF" && [ -z "$PROOF_MAC" ]; } \
               || { grep -q 'restore_hwid=' "$RESTORE_PROOF" && [ -z "$PROOF_HWID" ]; }; then
                RESTORE_PROOF_STATUS="proof_malformed"
            elif [ -z "$PROOF_MAC" ] && [ -z "$PROOF_HWID" ]; then
                RESTORE_PROOF_STATUS="marker_only_advisory"
            elif [ -z "$EXP_MAC_LC" ] && [ -z "$EXP_HWID_LC" ]; then
                RESTORE_PROOF_STATUS="identity_unverified"
            else
                # Require BOTH mac AND hwid present in the proof AND matching the
                # supplied --expect-* (case-insensitive), plus any supplied
                # optional model/target must match a present proof field.
                mismatch=0
                [ -n "$PROOF_MAC" ] && [ -n "$PROOF_HWID" ] || mismatch=1
                [ -n "$EXP_MAC_LC" ] || mismatch=1
                [ -n "$EXP_HWID_LC" ] || mismatch=1
                [ "$EXP_MAC_LC" = "$PROOF_MAC" ] || mismatch=1
                [ "$EXP_HWID_LC" = "$PROOF_HWID" ] || mismatch=1
                if [ -n "$EXP_MODEL_LC" ]; then [ "$EXP_MODEL_LC" = "$PROOF_MODEL" ] || mismatch=1; fi
                if [ -n "$EXP_TARGET_LC" ]; then [ "$EXP_TARGET_LC" = "$PROOF_TARGET" ] || mismatch=1; fi
                if [ "$mismatch" = "0" ]; then
                    RESTORE_PROOF_OK=1
                    RESTORE_PROOF_STATUS="restore_verified_identity_matched"
                    RESTORE_MATCHED_MAC="$PROOF_MAC"
                    RESTORE_MATCHED_HWID="$PROOF_HWID"
                    RESTORE_MATCHED_MODEL="$PROOF_MODEL"
                    RESTORE_ARTIFACT_SHA256="$PROOF_SHA"
                else
                    RESTORE_PROOF_STATUS="identity_mismatch"
                fi
            fi
        else
            RESTORE_PROOF_STATUS="present_but_not_verified"
        fi
    else
        RESTORE_PROOF_STATUS="file_not_found"
    fi
fi

# --- Extract partition table from manifest ---
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

hex_to_dec() {
    printf '%d' "$1" 2>/dev/null || echo 0
}

STAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
MANIFEST_BASENAME="$(basename "$MANIFEST")"

# --- Determine overall plan readiness ---
PLAN_READY=0
if [ "$LAYOUT_OK" = "1" ] && [ "$RESTORE_PROOF_OK" = "1" ]; then
    PLAN_READY=1
fi

# Compute min free space requirement based on scheme.
if [ "$PARTITION_SCHEME" = "stock" ]; then
    MIN_FREE_MB=260  # ~256 MiB stock NAND
else
    MIN_FREE_MB=265  # ~260 MiB BraiinsOS layout (sum of all 10 parts)
fi

# --- Generate the backup plan ---
{
    echo "# AM1 (S9 Zynq) NAND Backup Execution Plan"
    echo
    echo "- Generated: \`$STAMP\`"
    echo "- Source manifest: \`$MANIFEST_BASENAME\`"
    echo "- Target class: am1 Zynq XC7Z010 ($PARTITION_SCHEME layout)"
    echo "- Partitions: $PARTITION_COUNT"
    echo "- \`nand_backup_execute_go=0\`"
    echo "- \`plan_ready=$PLAN_READY\`"
    echo "- \`partition_scheme=$PARTITION_SCHEME\`"
    echo "- \`readback_verify=$READBACK_VERIFY\`"
    echo
    echo "## Pre-Flight Checks"
    echo
    echo "All of the following must pass before any NAND backup read is authorized:"
    echo
    if [ "$RESTORE_PROOF_OK" = "1" ]; then
        echo "- [x] Restore artifact evidence verified for this exact unit (identity-bound: mac=\`$RESTORE_MATCHED_MAC\` hwid=\`$RESTORE_MATCHED_HWID\`)"
    else
        echo "- [ ] Restore marker present (advisory, not identity-bound) (status: \`$RESTORE_PROOF_STATUS\`)"
    fi
    if [ "$LAYOUT_OK" = "1" ]; then
        echo "- [x] MTD layout profile validated (\`layout_profile_candidate=1\`, scheme=\`$PARTITION_SCHEME\`)"
    else
        echo "- [ ] MTD layout profile validated (current: \`layout_profile_candidate=$LAYOUT_OK\`)"
    fi
    echo "- [ ] Operator explicit written approval for NAND reads"
    echo "- [ ] Mining stopped or maintenance window confirmed"
    echo "- [ ] Adequate storage at backup destination (minimum ${MIN_FREE_MB} MB free)"
    echo "- [ ] System booted from SD card OR active slot is opposite of any planned write"
    echo "- [ ] \`--operator-acknowledged-data-loss\` flag passed to am1_nand_backup_execute.sh (mirrors revert_common.sh)"
    echo "- [ ] DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable set"
    echo
    echo "## Execution Environment"
    echo
    if [ "$PARTITION_SCHEME" = "braiinsos" ]; then
        echo "The AM1 BraiinsOS-class backup CAN be executed from the live"
        echo "BraiinsOS / DCENT_OS instance because we never write to the active"
        echo "slot. \`nanddump\` is a pure read operation. However:"
        echo
        echo "1. mtd4 (uboot_env) MUST be quiesced — do not run \`fw_setenv\`"
        echo "   during the backup. The recommended order is dump mtd4 FIRST."
        echo "2. Active firmware slot (mtd7 OR mtd8) is mounted read-write via"
        echo "   UBI — \`nanddump\` reads the raw NAND through OOB skip-bad, but"
        echo "   the resulting image is NOT a clean filesystem snapshot. Use it"
        echo "   only as a slot-level restore image."
        echo "3. The inactive slot is a clean snapshot — safe to nanddump anytime."
        echo "4. mtd9 (factory) holds per-unit calibration — preserve it verbatim."
    else
        echo "The AM1 stock Bitmain layout uses only 3 partitions:"
        echo
        echo "- mtd0 = BOOT.bin + DTB + kernel (32 MiB)"
        echo "- mtd1 = primary rootfs UBI (144 MiB)"
        echo "- mtd2 = upgrade slot UBI (80 MiB)"
        echo
        echo "There is no A/B firmware scheme. The unit is single-slot — booting"
        echo "from mtd1 with mtd2 used as an upgrade staging area. \`nanddump\` of"
        echo "the running mtd1 will not be a clean filesystem snapshot. Stop"
        echo "bmminer/cgminer before backing up if possible."
    fi
    echo
    echo "## Partition Backup Commands"
    echo
    echo "Each command below uses \`nanddump --bb=skipbad --omitoob\` to produce"
    echo "a clean dump without OOB data, skipping known-bad blocks."
    echo
    echo "These commands are COMMENTED and NOT authorized for execution:"
    echo
    echo '```sh'
    echo '# --- AM1 NAND Full Backup ---'
    echo '# Environment: BraiinsOS / DCENT_OS / Stock root SSH on am1 Zynq S9'
    echo '# Gate: DCENT_NAND_BACKUP_AUTHORIZED=1 must be set'
    echo '#'
    echo '# OUTDIR="/tmp/am1_nand_$(date -u +%Y%m%dT%H%M%SZ)"'
    echo '# mkdir -p "$OUTDIR"'
    echo '#'

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        echo "#"
        printf '# Partition: %s (%s)\n' "$name" "$node"
        printf '# Expected size: %s (%d bytes)\n' "$size" "$size_dec"
        printf '# Erase block: %s\n' "$erase"
        printf '# nanddump --bb=skipbad --omitoob -f "$OUTDIR/%s" %s\n' "$artifact" "$node"
        printf '# sha256sum "$OUTDIR/%s" >> "$OUTDIR/SHA256SUMS"\n' "$artifact"
    done

    echo '#'
    echo '# --- Verification ---'
    echo '# sha256sum -c "$OUTDIR/SHA256SUMS"'
    echo '# wc -c "$OUTDIR"/*.nanddump'
    if [ "$READBACK_VERIFY" = "1" ]; then
        echo '#'
        echo '# --- Readback re-dump (--readback-verify) ---'
        echo '# For each partition, re-dump and compare the SHA. A clean read'
        echo '# of unchanged NAND MUST be byte-identical (idempotent).'
        printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
            [ -n "$node" ] || continue
            printf '# nanddump --bb=skipbad --omitoob -f "$OUTDIR/%s.recheck" %s\n' "$artifact" "$node"
            printf '# diff -q "$OUTDIR/%s" "$OUTDIR/%s.recheck" || echo "FAIL: re-dump diverges for %s"\n' "$artifact" "$artifact" "$artifact"
        done
    fi
    echo '```'
    echo

    echo "## Expected Artifact Sizes"
    echo
    echo "| Partition | Name | Hex Size | Decimal Bytes | Artifact File |"
    echo "| --- | --- | --- | --- | --- |"

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        printf '| %s | %s | %s | %d | %s |\n' "$node" "$name" "$size" "$size_dec" "$artifact"
    done

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

    echo "## Sensitive Data Handling"
    echo
    echo "The following partitions may contain sensitive data:"
    echo
    if [ "$PARTITION_SCHEME" = "braiinsos" ]; then
        echo "- \`mtd4_uboot_env.nanddump\`: MAC address, hwid, factory env."
        echo "- \`mtd5_miner_cfg.nanddump\`: pool credentials, worker password."
        echo "- \`mtd7_firmware1.nanddump\` / \`mtd8_firmware2.nanddump\`: rootfs"
        echo "  contents — may contain SSH keys, network config, pool history."
        echo "- \`mtd9_factory.nanddump\`: per-chip silicon profile, MAC, model SKU."
    else
        echo "- \`mtd0_boot.nanddump\`: U-Boot env appended (MAC, hwid)."
        echo "- \`mtd1_rootfs.nanddump\` / \`mtd2_upgrade.nanddump\`: pool"
        echo "  credentials, worker password, network config, SSH keys, model SKU."
    fi
    echo
    echo "**Redaction policy**: Do NOT commit raw backup artifacts to git."
    echo "Store in a local encrypted directory or air-gapped media. Only"
    echo "commit the SHA256SUMS and sizes manifest (no raw content)."
    echo

    echo "## Decision"
    echo
    if [ "$PLAN_READY" = "1" ]; then
        echo "Pre-flight checks PASS. This plan is READY for operator review and"
        echo "explicit authorization. The operator must still:"
        echo
        echo "1. Confirm maintenance window"
        echo "2. Set \`DCENT_NAND_BACKUP_AUTHORIZED=1\`"
        echo "3. Run \`am1_nand_backup_execute.sh --operator-acknowledged-data-loss\` (the ack flag is a hard gate, mirrors revert_common.sh)"
    else
        echo "Pre-flight checks INCOMPLETE. The following gates are not yet satisfied:"
        echo
        [ "$RESTORE_PROOF_OK" = "1" ] || echo "- Restore artifact proof: $RESTORE_PROOF_STATUS"
        [ "$LAYOUT_OK" = "1" ] || echo "- MTD layout validation: not passed"
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
    printf '  "type": "am1_nand_backup_manifest",\n'
    printf '  "generated_utc": "%s",\n' "$STAMP"
    printf '  "target": {\n'
    printf '    "class": "am1 Zynq XC7Z010",\n'
    printf '    "layout": "%s",\n' "$PARTITION_SCHEME"
    printf '    "partition_count": %d\n' "$PARTITION_COUNT"
    printf '  },\n'
    printf '  "nand_backup_execute_go": 0,\n'
    printf '  "plan_ready": %d,\n' "$PLAN_READY"
    printf '  "readback_verify": %d,\n' "$READBACK_VERIFY"
    printf '  "pre_flight": {\n'
    printf '    "restore_artifact_proof": "%s",\n' "$RESTORE_PROOF_STATUS"
    printf '    "restore_matched_mac": %s,\n' "$(json_or_null "$RESTORE_MATCHED_MAC")"
    printf '    "restore_matched_hwid": %s,\n' "$(json_or_null "$RESTORE_MATCHED_HWID")"
    printf '    "restore_matched_model": %s,\n' "$(json_or_null "$RESTORE_MATCHED_MODEL")"
    printf '    "restore_artifact_sha256": %s,\n' "$(json_or_null "$RESTORE_ARTIFACT_SHA256")"
    printf '    "layout_profile": %d,\n' "$LAYOUT_OK"
    printf '    "operator_approval": false,\n'
    printf '    "mining_stopped": false,\n'
    printf '    "storage_adequate": false,\n'
    printf '    "min_free_mb": %d\n' "$MIN_FREE_MB"
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
        printf '      "readback_sha256": null,\n'
        printf '      "status": "pending"\n'
        printf '    }'
    done

    printf '\n  ],\n'
    printf '  "verification": {\n'
    printf '    "all_artifacts_exist": null,\n'
    printf '    "all_artifacts_nonempty": null,\n'
    printf '    "all_sha256_match": null,\n'
    printf '    "readback_idempotent": null,\n'
    printf '    "total_expected_bytes": %d,\n' "$TOTAL"
    printf '    "nand_backup_complete": null\n'
    printf '  }\n'
    printf '}\n'
} > "$JSON_TEMPLATE"

echo "plan=$OUTPUT"
echo "json_template=$JSON_TEMPLATE"
echo "plan_ready=$PLAN_READY"
echo "partition_scheme=$PARTITION_SCHEME"
echo "nand_backup_execute_go=0"
