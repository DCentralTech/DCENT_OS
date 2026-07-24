#!/usr/bin/env bash
#
# Generate a detailed AM2 NAND backup execution plan from a validated MTD
# manifest. Modeled after `am3_bb_nand_backup_plan.sh`.
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

set -euo pipefail
umask 077

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
PLAN_VALIDATOR="$SCRIPT_DIR/validate_am2_nand_backup_plan.py"
ATOMIC_PUBLISHER="$SCRIPT_DIR/atomic_publish_file.py"
DURABLE_IO="$SCRIPT_DIR/durable_file_io.py"

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  am2_nand_backup_plan.sh --manifest <manifest.md> [options]

Options:
  --manifest <file>            Required. Manifest from am2_nand_backup_manifest.sh.
  --restore-artifact-proof <f> Restore-verified artifact evidence for this exact unit.
  --sd-recovery-proof <file>   Passing AM2 external-boot recovery probe for the
                               same endpoint.
  --expect-ip <ip>             Endpoint recorded by the SD-recovery proof.
  --expect-host-key-sha256 <f> Pinned SSH host key recorded by the proof.
  --expect-mac <mac>           Physical MAC recorded by the restore proof.
  --expect-hwid <id>           Factory HWID recorded by the restore proof.
  --expect-model <model>       Whitespace-free model identity from the unit.
  --expect-target <target>     Authorized DCENT_OS board target.
  --readback-verify            Mark the plan as requiring a post-dump re-read pass
                               (idempotent nanddump check) before flagging the
                               backup complete.
  --output <file>              Output plan file. Default: <manifest>_backup_plan.md
  --json-template <file>       JSON manifest template path. Default:
                               <manifest>_backup_manifest_template.json
  -h, --help                   Show this help.

Pre-flight requirements (all must be provided for a READY plan):
  - Manifest with the exact ordered ten-partition AM2 geometry
  - Fresh restore artifact evidence with SHA256, timestamp, model, target,
    MAC, and hwid matched to this unit
  - Fresh passing SD-recovery proof matched to --expect-ip
  - --readback-verify

Safety contract:
  - Local evidence parser only. No SSH, no miner access, no flash/env reads.
  - nand_backup_execute_go=0 always. This script produces the plan, not
    the execution.
USAGE
    exit "$code"
}

MANIFEST=""
RESTORE_PROOF=""
SD_RECOVERY_PROOF=""
READBACK_VERIFY=0
OUTPUT=""
JSON_TEMPLATE=""
# CE-374: operator-supplied identity of THIS exact unit. A restore-verified
# marker alone is NOT proof for this unit — it must be bound to the unit's
# MAC/HWID/model/target.
EXPECT_MAC=""
EXPECT_HWID=""
EXPECT_MODEL=""
EXPECT_TARGET=""
EXPECT_IP=""
EXPECT_HOST_KEY_SHA256=""

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
        --sd-recovery-proof)
            SD_RECOVERY_PROOF="${2:?--sd-recovery-proof requires a file}"
            shift 2
            ;;
        --sd-recovery-proof=*)
            SD_RECOVERY_PROOF="${1#--sd-recovery-proof=}"
            shift
            ;;
        --expect-ip)
            EXPECT_IP="${2:?--expect-ip requires a value}"
            shift 2
            ;;
        --expect-ip=*)
            EXPECT_IP="${1#--expect-ip=}"
            shift
            ;;
        --expect-host-key-sha256)
            EXPECT_HOST_KEY_SHA256="${2:?--expect-host-key-sha256 requires a value}"
            shift 2
            ;;
        --expect-host-key-sha256=*)
            EXPECT_HOST_KEY_SHA256="${1#--expect-host-key-sha256=}"
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

PYTHON_BIN="${PYTHON:-}"
if [ -z "$PYTHON_BIN" ]; then
    PYTHON_BIN="$(command -v python3 || command -v python || true)"
fi
[ -n "$PYTHON_BIN" ] || {
    echo "ERROR: Python is required for AM2 plan publication" >&2
    exit 1
}
for required_helper in "$PLAN_VALIDATOR" "$ATOMIC_PUBLISHER" "$DURABLE_IO"; do
    [ -f "$required_helper" ] || {
        echo "ERROR: required AM2 plan helper is missing: $required_helper" >&2
        exit 1
    }
done
for output_parent in "$(dirname "$OUTPUT")" "$(dirname "$JSON_TEMPLATE")"; do
    "$PYTHON_BIN" "$DURABLE_IO" mkdir "$output_parent" \
        --mode 700 --parents --exist-ok >/dev/null || {
        echo "ERROR: cannot durably create AM2 plan parent: $output_parent" >&2
        exit 1
    }
done
for destination in "$OUTPUT" "$JSON_TEMPLATE"; do
    if [ -e "$destination" ] || [ -L "$destination" ]; then
        echo "ERROR: refusing to replace existing AM2 plan output: $destination" >&2
        exit 1
    fi
done
OUTPUT_CANONICAL="$("$PYTHON_BIN" -c \
    'from pathlib import Path; import sys; print(str(Path(sys.argv[1]).resolve()).casefold())' \
    "$OUTPUT")"
JSON_CANONICAL="$("$PYTHON_BIN" -c \
    'from pathlib import Path; import sys; print(str(Path(sys.argv[1]).resolve()).casefold())' \
    "$JSON_TEMPLATE")"
[ "$OUTPUT_CANONICAL" != "$JSON_CANONICAL" ] || {
    echo "ERROR: Markdown and JSON outputs must be different paths" >&2
    exit 1
}
OUTPUT_TMP="$(mktemp "${OUTPUT}.publication-pending.XXXXXX")" || {
    echo "ERROR: could not allocate AM2 Markdown plan staging file" >&2
    exit 1
}
JSON_TMP="$(mktemp "${JSON_TEMPLATE}.publication-pending.XXXXXX")" || {
    rm -f -- "$OUTPUT_TMP"
    echo "ERROR: could not allocate AM2 JSON plan staging file" >&2
    exit 1
}
cleanup_plan_tmp() {
    if [ -n "${OUTPUT_TMP:-}" ]; then
        rm -f -- "$OUTPUT_TMP"
    fi
    if [ -n "${JSON_TMP:-}" ]; then
        rm -f -- "$JSON_TMP"
    fi
}
trap cleanup_plan_tmp EXIT
trap 'exit 1' HUP INT TERM

# --- Validate the manifest's declared status. Exact geometry is checked below. ---
LAYOUT_MARKER_OK=0
if grep -Fqx -- '- `layout_profile_candidate=1`' "$MANIFEST"; then
    LAYOUT_MARKER_OK=1
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
proof_field_count() {
    awk -v k="$1" '
        $0 ~ "^[[:space:]]*" k "=" { count += 1 }
        END { print count + 0 }
    ' "$2"
}
proof_exact_count() {
    awk -v expected="$1" '
        { sub(/\r$/, "") }
        $0 == expected { count += 1 }
        END { print count + 0 }
    ' "$2"
}
json_or_null() {
    if [ -z "$1" ]; then
        printf 'null'
    else
        printf '"%s"' "$1"
    fi
}

# --- Validate the external-boot recovery proof for the same endpoint. ---
SD_RECOVERY_OK=0
SD_RECOVERY_STATUS="not_provided"
SD_RECOVERY_IP=""
SD_RECOVERY_VERIFIED_UTC=""
SD_RECOVERY_HOST_KEY_SHA256=""
SD_RECOVERY_ROOT_DEVICE=""
SD_RECOVERY_PROOF_SHA256=""
SD_RECOVERY_COMPATIBLE=""
SD_RECOVERY_BOOT_ID=""
SD_RECOVERY_QUIESCENCE=""
if [ -n "$SD_RECOVERY_PROOF" ]; then
    if [ -f "$SD_RECOVERY_PROOF" ] && [ ! -L "$SD_RECOVERY_PROOF" ]; then
        SD_INITIAL_SHA256="$("$PYTHON_BIN" -c \
            'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' \
            "$SD_RECOVERY_PROOF")"
        SD_IP="$(proof_field ip "$SD_RECOVERY_PROOF")"
        SD_TIMESTAMP="$(proof_field timestamp_utc "$SD_RECOVERY_PROOF")"
        SD_HOST_KEY_SHA256="$(proof_field ssh_host_key_sha256 "$SD_RECOVERY_PROOF")"
        SD_MAC="$(proof_field identity_mac "$SD_RECOVERY_PROOF" | tr 'A-F' 'a-f')"
        SD_HWID="$(proof_field identity_hwid "$SD_RECOVERY_PROOF")"
        SD_MODEL="$(proof_field identity_model "$SD_RECOVERY_PROOF")"
        SD_COMPATIBLE="$(proof_field identity_compatible "$SD_RECOVERY_PROOF")"
        SD_TARGET="$(proof_field identity_target "$SD_RECOVERY_PROOF")"
        SD_BOOT_ID="$(proof_field boot_id "$SD_RECOVERY_PROOF")"
        SD_ROOT_DEVICE="$(proof_field root_source "$SD_RECOVERY_PROOF")"
        SD_FIELDS_EXACT=1
        for proof_key in schema timestamp_utc ip contract \
            ssh_host_key_authentication ssh_host_key_sha256 identity_mac \
            identity_hwid identity_model identity_compatible identity_target \
            boot_id root_source root_removable identity stock_xil_detected \
            external_boot mtd_geometry quiescence nand_backup_execute_go \
            nand_write_go persistent_install_go sd_recovery_probe; do
            if [ "$(proof_field_count "$proof_key" "$SD_RECOVERY_PROOF")" != "1" ]; then
                SD_FIELDS_EXACT=0
            fi
        done
        SD_LINE_COUNT="$(awk 'END {print NR + 0}' "$SD_RECOVERY_PROOF")"
        if [ "$SD_FIELDS_EXACT" != "1" ] || [ "$SD_LINE_COUNT" != "23" ] \
          || [ "$(proof_exact_count 'schema=am2_sd_recovery_proof_v1' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'contract=read_only_external_boot_recovery_probe' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'ssh_host_key_authentication=verified' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'identity=pass am2_zynq_exact_unit' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'stock_xil_detected=0' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'external_boot=pass root_device_exact_removable_mmc' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'mtd_geometry=pass exact_am2_braiinsos_dual_slot_10_partition' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'quiescence=pass_known_writer_scan_clear_no_writable_mtd' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'root_removable=1' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'nand_backup_execute_go=0' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'nand_write_go=0' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'persistent_install_go=0' "$SD_RECOVERY_PROOF")" != "1" ] \
          || [ "$(proof_exact_count 'sd_recovery_probe=pass' "$SD_RECOVERY_PROOF")" != "1" ]; then
            SD_RECOVERY_STATUS="proof_malformed_or_not_ready"
        elif ! printf '%s\n' "$SD_IP" | grep -Eq '^[A-Za-z0-9_.:-]+$' \
          || ! printf '%s\n' "$SD_TIMESTAMP" | \
                grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' \
          || ! printf '%s\n' "$SD_HOST_KEY_SHA256" | \
                grep -Eq '^SHA256:[A-Za-z0-9+/]{43}$' \
          || ! printf '%s\n' "$SD_MAC" | grep -Eq '^[0-9a-f]{2}(:[0-9a-f]{2}){5}$' \
          || ! printf '%s\n' "$SD_HWID" | grep -Eq '^[A-Za-z0-9_.:-]+$' \
          || ! printf '%s\n' "$SD_MODEL" | grep -Eq '^[A-Za-z0-9_.:-]+$' \
          || ! printf '%s\n' "$SD_COMPATIBLE" | grep -Eq '^[A-Za-z0-9_.:-]+$' \
          || ! printf '%s\n' "$SD_TARGET" | grep -Eq '^[A-Za-z0-9_.:-]+$' \
          || ! printf '%s\n' "$SD_BOOT_ID" | \
                grep -Eq '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$' \
          || ! printf '%s\n' "$SD_ROOT_DEVICE" | \
                grep -Eq '^/dev/mmcblk[0-9]+p[0-9]+$'; then
            SD_RECOVERY_STATUS="proof_malformed"
        elif [ -z "$EXPECT_IP" ] || [ "$SD_IP" != "$EXPECT_IP" ]; then
            SD_RECOVERY_STATUS="endpoint_mismatch"
        elif [ -z "$EXPECT_HOST_KEY_SHA256" ] || \
          [ "$SD_HOST_KEY_SHA256" != "$EXPECT_HOST_KEY_SHA256" ]; then
            SD_RECOVERY_STATUS="host_key_mismatch"
        elif [ "$SD_COMPATIBLE" != "xlnx_zynq-7000" ] \
          || [ "$SD_TARGET" != "am2-s19jpro-zynq" ] \
          || [ "$(printf '%s' "$SD_MODEL" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9')" != "antminers19jpro" ]; then
            SD_RECOVERY_STATUS="unauthorized_identity_map"
        elif [ "$SD_MAC" != "$(printf '%s' "$EXPECT_MAC" | tr 'A-F' 'a-f')" ] \
          || [ "$SD_HWID" != "$EXPECT_HWID" ] \
          || [ "$SD_MODEL" != "$EXPECT_MODEL" ] \
          || [ "$SD_TARGET" != "$EXPECT_TARGET" ]; then
            SD_RECOVERY_STATUS="identity_mismatch"
        elif [ "$("$PYTHON_BIN" -c \
            'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' \
            "$SD_RECOVERY_PROOF")" != "$SD_INITIAL_SHA256" ]; then
            SD_RECOVERY_STATUS="proof_changed_during_validation"
        else
            SD_RECOVERY_OK=1
            SD_RECOVERY_STATUS="external_boot_identity_matched"
            SD_RECOVERY_IP="$SD_IP"
            SD_RECOVERY_VERIFIED_UTC="$SD_TIMESTAMP"
            SD_RECOVERY_HOST_KEY_SHA256="$SD_HOST_KEY_SHA256"
            SD_RECOVERY_ROOT_DEVICE="$SD_ROOT_DEVICE"
            SD_RECOVERY_PROOF_SHA256="$SD_INITIAL_SHA256"
            SD_RECOVERY_COMPATIBLE="$SD_COMPATIBLE"
            SD_RECOVERY_BOOT_ID="$SD_BOOT_ID"
            SD_RECOVERY_QUIESCENCE="pass_known_writer_scan_clear_no_writable_mtd"
        fi
    else
        SD_RECOVERY_STATUS="file_not_found"
    fi
fi

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
RESTORE_MATCHED_TARGET=""
RESTORE_ARTIFACT_SHA256=""
RESTORE_VERIFIED_UTC=""
if [ -n "$RESTORE_PROOF" ]; then
    if [ -f "$RESTORE_PROOF" ]; then
        RESTORE_MARKER_COUNT="$(awk '
            /^[[:space:]]*restore_verified=1[[:space:]]*$/ { count += 1 }
            /^[[:space:]]*artifact_state=restore_verified[[:space:]]*$/ { count += 1 }
            END { print count + 0 }
        ' "$RESTORE_PROOF")"
        if [ "$RESTORE_MARKER_COUNT" = "1" ]; then
            PROOF_MAC="$(proof_field restore_mac "$RESTORE_PROOF" | tr 'A-F' 'a-f')"
            PROOF_HWID="$(proof_field restore_hwid "$RESTORE_PROOF")"
            PROOF_MODEL="$(proof_field restore_model "$RESTORE_PROOF")"
            PROOF_TARGET="$(proof_field restore_target "$RESTORE_PROOF")"
            PROOF_SHA="$(proof_field restore_artifact_sha256 "$RESTORE_PROOF" | tr 'A-F' 'a-f')"
            PROOF_VERIFIED_UTC="$(proof_field restore_verified_utc "$RESTORE_PROOF")"
            EXP_MAC_LC="$(printf '%s' "$EXPECT_MAC" | tr -d '[:space:]' | tr 'A-F' 'a-f')"
            EXP_HWID_LC="$(printf '%s' "$EXPECT_HWID" | tr -d '[:space:]')"
            EXP_MODEL_LC="$(printf '%s' "$EXPECT_MODEL" | tr -d '[:space:]')"
            EXP_TARGET_LC="$(printf '%s' "$EXPECT_TARGET" | tr -d '[:space:]')"
            PROOF_FIELDS_EXACT=1
            for proof_key in restore_mac restore_hwid restore_model restore_target \
                restore_artifact_sha256 restore_verified_utc; do
                if [ "$(proof_field_count "$proof_key" "$RESTORE_PROOF")" != "1" ]; then
                    PROOF_FIELDS_EXACT=0
                fi
            done
            PROOF_SAFE_FIELDS=1
            for proof_value in "$PROOF_HWID" "$PROOF_MODEL" "$PROOF_TARGET"; do
                if ! printf '%s\n' "$proof_value" | grep -Eq '^[A-Za-z0-9_.:-]+$'; then
                    PROOF_SAFE_FIELDS=0
                fi
            done
            if [ "$PROOF_FIELDS_EXACT" != "1" ]; then
                RESTORE_PROOF_STATUS="proof_malformed"
            elif ! printf '%s\n' "$PROOF_MAC" | grep -Eq '^[0-9a-f]{2}(:[0-9a-f]{2}){5}$' \
              || [ "$PROOF_SAFE_FIELDS" != "1" ] \
              || ! printf '%s\n' "$PROOF_SHA" | grep -Eq '^[0-9a-f]{64}$' \
              || ! printf '%s\n' "$PROOF_VERIFIED_UTC" | \
                    grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'; then
                RESTORE_PROOF_STATUS="proof_malformed"
            elif [ -z "$EXP_MAC_LC" ] || [ -z "$EXP_HWID_LC" ] \
              || [ -z "$EXP_MODEL_LC" ] || [ -z "$EXP_TARGET_LC" ]; then
                RESTORE_PROOF_STATUS="identity_unverified"
            else
                mismatch=0
                [ "$EXP_MAC_LC" = "$PROOF_MAC" ] || mismatch=1
                [ "$EXP_HWID_LC" = "$PROOF_HWID" ] || mismatch=1
                [ "$EXP_MODEL_LC" = "$PROOF_MODEL" ] || mismatch=1
                [ "$EXP_TARGET_LC" = "$PROOF_TARGET" ] || mismatch=1
                if [ "$mismatch" = "0" ]; then
                    RESTORE_PROOF_OK=1
                    RESTORE_PROOF_STATUS="restore_verified_identity_matched"
                    RESTORE_MATCHED_MAC="$PROOF_MAC"
                    RESTORE_MATCHED_HWID="$PROOF_HWID"
                    RESTORE_MATCHED_MODEL="$PROOF_MODEL"
                    RESTORE_MATCHED_TARGET="$PROOF_TARGET"
                    RESTORE_ARTIFACT_SHA256="$PROOF_SHA"
                    RESTORE_VERIFIED_UTC="$PROOF_VERIFIED_UTC"
                else
                    RESTORE_PROOF_STATUS="identity_mismatch"
                fi
            fi
        else
            RESTORE_PROOF_STATUS="missing_or_ambiguous_verified_marker"
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

EXPECTED_PARTITIONS='/dev/mtd0|0x00800000|0x00020000|boot|mtd0_boot.nanddump
/dev/mtd1|0x00c00000|0x00020000|boot-failover|mtd1_boot-failover.nanddump
/dev/mtd2|0x00200000|0x00020000|fpga1|mtd2_fpga1.nanddump
/dev/mtd3|0x00200000|0x00020000|fpga2|mtd3_fpga2.nanddump
/dev/mtd4|0x00080000|0x00020000|uboot_env|mtd4_uboot_env.nanddump
/dev/mtd5|0x00080000|0x00020000|miner_cfg|mtd5_miner_cfg.nanddump
/dev/mtd6|0x05700000|0x00020000|recovery|mtd6_recovery.nanddump
/dev/mtd7|0x03900000|0x00020000|firmware1|mtd7_firmware1.nanddump
/dev/mtd8|0x03900000|0x00020000|firmware2|mtd8_firmware2.nanddump
/dev/mtd9|0x01e00000|0x00020000|factory|mtd9_factory.nanddump'
NORMALIZED_PARTITIONS="$(printf '%s' "$PARTITIONS" | sed '/^$/d')"
LAYOUT_OK=0
if [ "$LAYOUT_MARKER_OK" = "1" ] && \
    [ "$NORMALIZED_PARTITIONS" = "$EXPECTED_PARTITIONS" ]; then
    LAYOUT_OK=1
fi

hex_to_dec() {
    printf '%d' "$1" 2>/dev/null || echo 0
}

STAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
MANIFEST_BASENAME="$(basename "$MANIFEST")"

# --- Determine overall plan readiness ---
PLAN_READY=0
if [ "$LAYOUT_OK" = "1" ] && [ "$RESTORE_PROOF_OK" = "1" ] && \
    [ "$SD_RECOVERY_OK" = "1" ] && [ "$READBACK_VERIFY" = "1" ]; then
    PLAN_READY=1
fi

# --- Generate the backup plan ---
{
    echo "# AM2 (S19 Pro / S19j Pro Zynq) NAND Backup Execution Plan"
    echo
    echo "- Generated: \`$STAMP\`"
    echo "- Source manifest: \`$MANIFEST_BASENAME\`"
    echo "- Target class: am2 Zynq 7007S BraiinsOS dual-slot layout"
    echo "- Partitions: $PARTITION_COUNT"
    echo "- \`nand_backup_execute_go=0\`"
    echo "- \`plan_ready=$PLAN_READY\`"
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
    if [ "$SD_RECOVERY_OK" = "1" ]; then
        echo "- [x] Exact-unit external-boot probe matched endpoint \`$SD_RECOVERY_IP\`, removable root \`$SD_RECOVERY_ROOT_DEVICE\`, and pinned host key \`$SD_RECOVERY_HOST_KEY_SHA256\`"
    else
        echo "- [ ] External-boot recovery probe (status: \`$SD_RECOVERY_STATUS\`)"
    fi
    if [ "$LAYOUT_OK" = "1" ]; then
        echo "- [x] MTD layout profile validated (\`layout_profile_candidate=1\`)"
    else
        echo "- [ ] MTD layout profile validated (current: \`layout_profile_candidate=$LAYOUT_OK\`)"
    fi
    echo "- [ ] Operator explicit written approval for NAND reads"
    echo "- [ ] Mining stopped or maintenance window confirmed"
    echo "- [ ] Adequate storage at backup destination (minimum 280 MB free)"
    echo "- [ ] System booted from SD card OR active slot is opposite of any planned write"
    echo "- [ ] DCENT_NAND_BACKUP_AUTHORIZED=1 environment variable set"
    echo
    echo "## Execution Environment"
    echo
    echo "Execute this plan only against the exact endpoint proven by the fresh"
    echo "external-SD recovery evidence. The executor rechecks physical identity,"
    echo "pinned SSH host key, and live MTD geometry before any raw NAND read."
    echo "However:"
    echo
    echo "1. mtd4 (uboot_env) MUST be quiesced — do not run \`fw_setenv\` during"
    echo "   the backup. The recommended order is dump mtd4 FIRST, before any"
    echo "   other partition, to capture a clean snapshot."
    echo "2. Active firmware slot (mtd7 OR mtd8) is mounted read-write via UBI"
    echo "   — \`nanddump --bb=padbad\` preserves partition offsets, but a live"
    echo "   filesystem image is not transactionally quiesced. Use it only as a"
    echo "   restore artifact under the documented recovery procedure."
    echo "3. The inactive slot is a clean snapshot — safe to nanddump anytime."
    echo
    echo "## Partition Backup Commands"
    echo
    echo "Each command below uses \`nanddump --bb=padbad --omitoob\` to produce"
    echo "a fixed-size dump without OOB data while retaining bad-block offsets."
    echo
    echo "These commands are COMMENTED and NOT authorized for execution:"
    echo
    echo '```sh'
    echo '# --- AM2 NAND Full Backup ---'
    echo '# Environment: BraiinsOS / DCENT_OS root SSH on am2 Zynq unit'
    echo '# Gate: DCENT_NAND_BACKUP_AUTHORIZED=1 must be set'
    echo '#'
    echo '# OUTDIR="/tmp/am2_nand_$(date -u +%Y%m%dT%H%M%SZ)"'
    echo '# mkdir -p "$OUTDIR"'
    echo '#'

    printf '%s\n' "$PARTITIONS" | while IFS='|' read -r node size erase name artifact; do
        [ -n "$node" ] || continue
        size_dec="$(hex_to_dec "$size")"
        echo "#"
        printf '# Partition: %s (%s)\n' "$name" "$node"
        printf '# Expected size: %s (%d bytes)\n' "$size" "$size_dec"
        printf '# Erase block: %s\n' "$erase"
        printf '# nanddump --bb=padbad --omitoob -f "$OUTDIR/%s" %s\n' "$artifact" "$node"
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
            printf '# nanddump --bb=padbad --omitoob -f "$OUTDIR/%s.recheck" %s\n' "$artifact" "$node"
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
    echo "- \`mtd4_uboot_env.nanddump\`: MAC address, hwid, factory env."
    echo "- \`mtd5_miner_cfg.nanddump\`: pool credentials, worker password."
    echo "- \`mtd7_firmware1.nanddump\` / \`mtd8_firmware2.nanddump\`: rootfs"
    echo "  contents — may contain SSH keys, network config."
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
        echo "3. From the operator host, run \`am2_nand_backup_execute.sh --target <ip> --plan <json> --known-hosts <pinned-file> --expected-host-key-sha256 $SD_RECOVERY_HOST_KEY_SHA256 --operator-authorized-backup --readback-verify\`"
    else
        echo "Pre-flight checks INCOMPLETE. The following gates are not yet satisfied:"
        echo
        [ "$RESTORE_PROOF_OK" = "1" ] || echo "- Restore artifact proof: $RESTORE_PROOF_STATUS"
        [ "$SD_RECOVERY_OK" = "1" ] || echo "- SD recovery proof: $SD_RECOVERY_STATUS"
        [ "$LAYOUT_OK" = "1" ] || echo "- MTD layout validation: not passed"
        echo
        echo "Resolve these gaps before requesting operator authorization."
    fi
    echo
    echo "This plan does NOT authorize NAND reads, NAND writes, bootenv changes,"
    echo "persistent install, native mining, or tap mining."
    echo
    echo "\`nand_backup_execute_go=0\`"
} > "$OUTPUT_TMP"

# --- Generate JSON manifest template ---
{
    printf '{\n'
    printf '  "schema_version": "1.0.0",\n'
    printf '  "type": "am2_nand_backup_manifest",\n'
    printf '  "generated_utc": "%s",\n' "$STAMP"
    printf '  "target": {\n'
    printf '    "class": "am2 Zynq 7007S",\n'
    printf '    "layout": "braiinsos-dual-slot",\n'
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
    printf '    "restore_matched_target": %s,\n' "$(json_or_null "$RESTORE_MATCHED_TARGET")"
    printf '    "restore_artifact_sha256": %s,\n' "$(json_or_null "$RESTORE_ARTIFACT_SHA256")"
    printf '    "restore_verified_utc": %s,\n' "$(json_or_null "$RESTORE_VERIFIED_UTC")"
    printf '    "sd_recovery_probe": "%s",\n' "$SD_RECOVERY_STATUS"
    printf '    "sd_recovery_ip": %s,\n' "$(json_or_null "$SD_RECOVERY_IP")"
    printf '    "sd_recovery_verified_utc": %s,\n' "$(json_or_null "$SD_RECOVERY_VERIFIED_UTC")"
    printf '    "sd_recovery_host_key_sha256": %s,\n' "$(json_or_null "$SD_RECOVERY_HOST_KEY_SHA256")"
    printf '    "sd_recovery_root_device": %s,\n' "$(json_or_null "$SD_RECOVERY_ROOT_DEVICE")"
    printf '    "sd_recovery_proof_sha256": %s,\n' "$(json_or_null "$SD_RECOVERY_PROOF_SHA256")"
    printf '    "sd_recovery_compatible": %s,\n' "$(json_or_null "$SD_RECOVERY_COMPATIBLE")"
    printf '    "sd_recovery_boot_id": %s,\n' "$(json_or_null "$SD_RECOVERY_BOOT_ID")"
    printf '    "sd_recovery_quiescence": %s,\n' "$(json_or_null "$SD_RECOVERY_QUIESCENCE")"
    printf '    "layout_profile": %d,\n' "$LAYOUT_OK"
    printf '    "operator_approval": false,\n'
    printf '    "mining_stopped": false,\n'
    printf '    "storage_adequate": false,\n'
    printf '    "min_free_mb": 280\n'
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
} > "$JSON_TMP"

if [ "$PLAN_READY" = "1" ]; then
    "$PYTHON_BIN" "$PLAN_VALIDATOR" --plan "$JSON_TMP" >/dev/null
else
    rm -f -- "$JSON_TMP"
    JSON_TMP=""
fi
"$PYTHON_BIN" "$ATOMIC_PUBLISHER" --require-directory-sync \
    "$OUTPUT_TMP" "$OUTPUT" >/dev/null
OUTPUT_TMP=""
if [ "$PLAN_READY" = "1" ]; then
    "$PYTHON_BIN" "$ATOMIC_PUBLISHER" --require-directory-sync \
        "$JSON_TMP" "$JSON_TEMPLATE" >/dev/null
    JSON_TMP=""
fi
trap - EXIT HUP INT TERM

echo "plan=$OUTPUT"
if [ "$PLAN_READY" = "1" ]; then
    echo "json_template=$JSON_TEMPLATE"
else
    echo "json_template=not_published"
fi
echo "plan_ready=$PLAN_READY"
echo "nand_backup_execute_go=0"
