#!/usr/bin/env bash
#
# AM1 (S9 Zynq) MTD Backup Manifest tool. Two modes:
#
#   1. Default (generate-planning-manifest):
#        am1_nand_backup_manifest.sh --evidence <am1_recon.txt> [--output <manifest.md>]
#
#      Parses local read-only recon evidence (a /proc/mtd dump in a
#      "=== mtd layout ===" block) and produces a planning manifest in the
#      same shape as am2_nand_backup_manifest.sh. Auto-detects which S9
#      partition scheme is present (BraiinsOS-class 10 partitions or
#      stock Bitmain 3 partitions).
#
#   2. Validate (--validate captured-result manifest):
#        am1_nand_backup_manifest.sh --validate --manifest <result.json>
#                                    [--local-backup-dir <path>]
#                                    [--reprobe-target <ip>]
#                                    [--ssh-user <user>] [--ssh-password-env <env>]
#
#      Validates a captured-result manifest produced by
#      am1_nand_backup_execute.sh: schema check, file existence on host,
#      SHA256 re-verification, optional re-probe of the target's /proc/mtd
#      to confirm the partition map hasn't drifted since capture.
#
# This script does NOT contact a miner unless --reprobe-target is given.
# In all other modes it is local-only.
#
# S9 BraiinsOS/DCENT_OS-class layout (10 partitions):
#   mtd0 boot 512K | mtd1 uboot 2.5M | mtd2 fpga1 2M | mtd3 fpga2 2M
#   mtd4 uboot_env 512K | mtd5 miner_cfg 512K | mtd6 recovery 22M
#   mtd7 firmware1 95M | mtd8 firmware2 95M | mtd9 factory 36M
#
# S9 stock Bitmain layout (3 partitions):
#   mtd0 boot 32M | mtd1 rootfs 144M | mtd2 upgrade 80M

set -euo pipefail

usage() {
    local code="${1:-2}"
    cat >&2 <<'USAGE'
Usage:
  # Generate planning manifest from local evidence:
  am1_nand_backup_manifest.sh --evidence <am1_recon.txt> [--output <manifest.md>]

  # Validate captured-result manifest JSON:
  am1_nand_backup_manifest.sh --validate --manifest <result.json>
                              [--local-backup-dir <path>]
                              [--reprobe-target <ip>]
                              [--ssh-user <user>]
                              [--ssh-password-env <env>]

Options (generate mode):
  --evidence <file>           Required read-only recon evidence file.
  --output <file>             Output markdown manifest. Default:
                              <evidence>_mtd_backup_manifest.md

Options (validate mode):
  --validate                  Switch to result-manifest validation mode.
  --manifest <file>           Required result manifest JSON.
  --local-backup-dir <path>   Path holding the .nanddump artifacts. If
                              not supplied, looked up next to manifest.
  --reprobe-target <ip>       Optional. SSH-probe the target's /proc/mtd
                              and confirm partition map matches manifest.
  --ssh-user <user>           SSH user for --reprobe-target (default: root).
  --ssh-password-env <name>   Env var holding SSH password (default: DCENT_PASSWORD).

Common:
  -h, --help                  Show this help.

Safety contract:
  - Generate mode: local evidence parser only. No SSH, no miner access.
  - Validate mode: local file-system + SHA256 checks. --reprobe-target
    performs SSH-readonly /proc/mtd capture only — no flash reads,
    no writes, no service stops.
USAGE
    exit "$code"
}

MODE="generate"
EVIDENCE=""
OUTPUT=""
MANIFEST_JSON=""
LOCAL_BACKUP_DIR=""
REPROBE_TARGET=""
SSH_USER="${DCENT_AM1_RECOVERY_SSH_USER:-root}"
SSH_PASSWORD_ENV="DCENT_PASSWORD"

while [ $# -gt 0 ]; do
    case "$1" in
        --validate)
            MODE="validate"
            shift
            ;;
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
        --manifest)
            MANIFEST_JSON="${2:?--manifest requires a file}"
            shift 2
            ;;
        --manifest=*)
            MANIFEST_JSON="${1#--manifest=}"
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
        --reprobe-target)
            REPROBE_TARGET="${2:?--reprobe-target requires an ip}"
            shift 2
            ;;
        --reprobe-target=*)
            REPROBE_TARGET="${1#--reprobe-target=}"
            shift
            ;;
        --ssh-user)
            SSH_USER="${2:?--ssh-user requires a user}"
            shift 2
            ;;
        --ssh-user=*)
            SSH_USER="${1#--ssh-user=}"
            shift
            ;;
        --ssh-password-env)
            SSH_PASSWORD_ENV="${2:?--ssh-password-env requires an env name}"
            shift 2
            ;;
        --ssh-password-env=*)
            SSH_PASSWORD_ENV="${1#--ssh-password-env=}"
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

# =============================================================================
# GENERATE MODE — planning manifest from local recon evidence.
# =============================================================================

generate_planning_manifest() {
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

    local mtd_lines
    mtd_lines="$(awk '
        /^=== mtd layout ===/ { in_mtd = 1; next }
        /^=== / && in_mtd { in_mtd = 0 }
        in_mtd && /^mtd[0-9]+:/ { print }
    ' "$EVIDENCE")"

    [ -n "$mtd_lines" ] || {
        echo "ERROR: no mtd layout block found in $EVIDENCE" >&2
        exit 1
    }

    local partition_count
    partition_count="$(printf '%s\n' "$mtd_lines" | grep -c '^mtd')"

    # AM1 BraiinsOS-class layout (10 partitions).
    local braiinsos_names='boot uboot fpga1 fpga2 uboot_env miner_cfg recovery firmware1 firmware2 factory'
    # AM1 stock Bitmain layout (3 partitions).
    local stock_names='boot rootfs upgrade'

    local layout_ok=0
    local partition_scheme="unknown"
    local missing_names=""

    check_scheme() {
        local scheme="$1"
        local expected="$2"
        local missing=""
        local name
        for name in $expected; do
            if ! printf '%s\n' "$mtd_lines" | grep -q "\"$name\""; then
                missing="$missing $name"
            fi
        done
        if [ -z "$missing" ]; then
            partition_scheme="$scheme"
            layout_ok=1
            return 0
        fi
        missing_names="$missing"
        return 1
    }

    if check_scheme "braiinsos" "$braiinsos_names"; then
        :
    elif check_scheme "stock" "$stock_names"; then
        :
    fi

    # Active slot detection (BraiinsOS only).
    local active_slot=""
    local active_hint
    active_hint="$(grep -m1 'firmware=' "$EVIDENCE" 2>/dev/null | grep -E '^firmware=[12]' || true)"
    case "$active_hint" in
        firmware=1*) active_slot="1" ;;
        firmware=2*) active_slot="2" ;;
    esac
    if [ -z "$active_slot" ]; then
        local boot_hint
        boot_hint="$(grep -m1 '^bootslot=' "$EVIDENCE" 2>/dev/null || true)"
        case "$boot_hint" in
            bootslot=a*) active_slot="1" ;;
            bootslot=b*) active_slot="2" ;;
        esac
    fi

    local stamp
    stamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    local source_basename
    source_basename="$(basename "$EVIDENCE")"

    {
        echo "# AM1 (S9 Zynq) MTD Backup Manifest"
        echo
        echo "- Created: \`$stamp\`"
        echo "- Source evidence: \`$source_basename\`"
        echo "- Scope: Antminer S9 am1 (XC7Z010 BraiinsOS-class or Stock Bitmain)"
        echo "- Status: planning-only manifest"
        echo "- \`layout_profile_candidate=$layout_ok\`"
        echo "- \`partition_scheme=$partition_scheme\`"
        echo "- \`partition_count=$partition_count\`"
        if [ "$partition_scheme" = "braiinsos" ]; then
            echo "- \`active_slot_candidate=${active_slot:-unknown}\`"
        fi
        echo "- \`nand_backup_execute_go=0\`"
        echo "- \`persistent_install_go=0\`"
        echo
        if [ "$layout_ok" != "1" ]; then
            echo "Could not match a known layout. Last attempted scheme missing:"
            echo "$missing_names"
            echo
            echo "Known AM1 layouts:"
            echo "- BraiinsOS / DCENT_OS: 10 partitions (boot, uboot, fpga1, fpga2,"
            echo "  uboot_env, miner_cfg, recovery, firmware1, firmware2, factory)"
            echo "- Stock Bitmain: 3 partitions (boot, rootfs, upgrade)"
            echo
            echo "If this is neither, the unit may be running a non-standard"
            echo "firmware (VNish, custom). Manual operator review required before"
            echo "any backup."
            echo
        fi
        echo "## Partition Table"
        echo
        echo "| Node | Size Hex | Erase Hex | Name | Required Future Artifact |"
        echo "| --- | --- | --- | --- | --- |"
        printf '%s\n' "$mtd_lines" | awk '
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
        echo "- Recovery proof on this exact am1 unit (SD-recovery probe pass,"
        echo "  or known-good factory restore image staged off-box)."
        echo "- Restore-to-stock package/source artifact: either BraiinsOS am1-s9"
        echo "  package (for stock units) OR the matching DCENT_OS sysupgrade"
        echo "  tarball (for BraiinsOS-class units)."
        echo "- Operator-approved maintenance window and pool credential redaction plan."
        echo "- SHA256 manifest for every captured artifact verified BOTH on the miner"
        echo "  AND on the host after transfer."
        echo "- A readback dry-run that proves \`nanddump\` of the active slot produces"
        echo "  a byte-identical re-dump (idempotent read)."
        if [ "$partition_scheme" = "braiinsos" ]; then
            echo "- Particular care for **mtd4 (uboot_env)** and **mtd9 (factory)**:"
            echo "  uboot_env contains MAC, hwid, factory env; factory contains"
            echo "  per-chip silicon binning. Corruption here = brick unless serial"
            echo "  console recovery is available."
        else
            echo "- Particular care for **mtd0 (boot)**: contains BOOT.bin + DTB +"
            echo "  kernel + appended U-Boot env. Corruption here = brick unless"
            echo "  SD-recovery is available."
        fi
        echo
        echo "## Future Command Template"
        echo
        echo "These are templates for a later authorized session, not commands approved by this manifest:"
        echo
        echo '```sh'
        printf '%s\n' "$mtd_lines" | awk '
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
    echo "partition_scheme=$partition_scheme"
    echo "partition_count=$partition_count"
    echo "layout_profile_candidate=$layout_ok"
}

# =============================================================================
# VALIDATE MODE — captured-result manifest verification.
# =============================================================================

# Read a string field from a flat JSON object. Not a full parser — handles only
# the shapes produced by am1_nand_backup_execute.sh.
json_string() {
    local key="$1"
    local file="$2"
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -n 1
}

json_int() {
    local key="$1"
    local file="$2"
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\([0-9-]\+\).*/\1/p' "$file" | head -n 1
}

ssh_readonly_probe() {
    local target="$1"
    local password="${!SSH_PASSWORD_ENV:-}"
    local ssh_opts=(
        -o StrictHostKeyChecking=no
        -o UserKnownHostsFile=/dev/null
        -o ConnectTimeout=8
        -o ServerAliveInterval=5
        -o ServerAliveCountMax=1
    )
    if [ -z "$password" ]; then
        ssh_opts+=(-o BatchMode=yes)
        ssh "${ssh_opts[@]}" "${SSH_USER}@${target}" 'cat /proc/mtd 2>/dev/null'
        return
    fi
    if ! command -v sshpass >/dev/null 2>&1; then
        echo "ERROR: sshpass not installed; cannot use password env $SSH_PASSWORD_ENV" >&2
        return 127
    fi
    SSHPASS="$password" sshpass -e ssh "${ssh_opts[@]}" "${SSH_USER}@${target}" 'cat /proc/mtd 2>/dev/null'
}

validate_result_manifest() {
    [ -n "$MANIFEST_JSON" ] || {
        echo "ERROR: --validate requires --manifest <file>" >&2
        usage
    }
    [ -f "$MANIFEST_JSON" ] || {
        echo "ERROR: manifest file not found: $MANIFEST_JSON" >&2
        exit 1
    }
    command -v sha256sum >/dev/null 2>&1 || {
        echo "ERROR: sha256sum required for validation" >&2
        exit 1
    }

    if [ -z "$LOCAL_BACKUP_DIR" ]; then
        LOCAL_BACKUP_DIR="$(dirname "$MANIFEST_JSON")"
    fi

    local schema_version mtype generated active_slot scheme nand_complete
    schema_version="$(json_string schema_version "$MANIFEST_JSON")"
    mtype="$(json_string type "$MANIFEST_JSON")"
    generated="$(json_string execution_utc "$MANIFEST_JSON")"
    [ -n "$generated" ] || generated="$(json_string generated_utc "$MANIFEST_JSON")"
    active_slot="$(json_string active_firmware_slot "$MANIFEST_JSON")"
    scheme="$(json_string layout "$MANIFEST_JSON")"
    nand_complete="$(json_string nand_backup_complete "$MANIFEST_JSON")"

    local fail_count=0
    local issues=""

    record_issue() {
        issues="${issues}- $1
"
        fail_count=$((fail_count + 1))
    }

    [ -n "$schema_version" ] || record_issue "schema_version missing"
    case "$mtype" in
        am1_nand_backup_result|am1_nand_backup_manifest) ;;
        *) record_issue "type is '$mtype' (expected am1_nand_backup_result or am1_nand_backup_manifest)" ;;
    esac
    [ -n "$generated" ] || record_issue "no execution_utc / generated_utc timestamp"

    # Iterate partitions[].artifact entries by extracting all "artifact": "..."
    # lines and their adjacent sha256 + actual_bytes. Limited JSON: assumes the
    # exact shape produced by execute/plan scripts.
    local artifacts shas sizes
    artifacts="$(grep -oE '"artifact"[[:space:]]*:[[:space:]]*"[^"]*"' "$MANIFEST_JSON" | sed 's/.*: *"\(.*\)"/\1/')"
    shas="$(grep -oE '"sha256"[[:space:]]*:[[:space:]]*"[a-fA-F0-9]*"' "$MANIFEST_JSON" | sed 's/.*: *"\(.*\)"/\1/')"
    sizes="$(grep -oE '"actual_bytes"[[:space:]]*:[[:space:]]*[0-9]+' "$MANIFEST_JSON" | sed 's/.*: *//')"

    [ -n "$artifacts" ] || record_issue "no partitions[] artifacts found in manifest"

    local artifact_count=0
    local sha_pass=0
    local sha_fail=0
    local missing=0

    # Pair artifact + expected sha line-by-line (assumes manifest order).
    paste <(printf '%s\n' "$artifacts") <(printf '%s\n' "$shas") <(printf '%s\n' "$sizes") | \
    while IFS=$'\t' read -r artifact expected_sha expected_size; do
        [ -n "$artifact" ] || continue
        artifact_count=$((artifact_count + 1))
        local local_file="$LOCAL_BACKUP_DIR/$artifact"
        if [ ! -f "$local_file" ]; then
            echo "MISSING: $artifact (looked at $local_file)"
            missing=$((missing + 1))
            continue
        fi
        local actual_sha
        actual_sha="$(sha256sum "$local_file" | awk '{ print $1 }')"
        if [ -z "$expected_sha" ] || [ "$expected_sha" = "null" ]; then
            echo "SKIP: $artifact (no expected sha in manifest)"
            continue
        fi
        if [ "$actual_sha" = "$expected_sha" ]; then
            echo "PASS: $artifact (sha=$actual_sha)"
            sha_pass=$((sha_pass + 1))
        else
            echo "FAIL: $artifact (expected=$expected_sha actual=$actual_sha)"
            sha_fail=$((sha_fail + 1))
        fi
    done

    # Re-probe target if requested.
    local reprobe_status="skipped"
    if [ -n "$REPROBE_TARGET" ]; then
        case "$REPROBE_TARGET" in
            ""|*[!A-Za-z0-9_.:-]*)
                record_issue "unsafe ip token in --reprobe-target: $REPROBE_TARGET"
                ;;
            *)
                echo "--- reprobing $REPROBE_TARGET /proc/mtd ---"
                local current_mtd
                if current_mtd="$(ssh_readonly_probe "$REPROBE_TARGET")" && [ -n "$current_mtd" ]; then
                    # Compare partition names from current /proc/mtd to manifest.
                    local current_names
                    current_names="$(printf '%s\n' "$current_mtd" | awk '
                        /^mtd[0-9]+:/ {
                            name = $4
                            gsub(/"/, "", name)
                            print name
                        }
                    ' | sort -u | tr '\n' ' ')"
                    local manifest_names
                    manifest_names="$(grep -oE '"name"[[:space:]]*:[[:space:]]*"[^"]*"' "$MANIFEST_JSON" | sed 's/.*: *"\(.*\)"/\1/' | sort -u | tr '\n' ' ')"
                    if [ "$current_names" = "$manifest_names" ]; then
                        reprobe_status="match"
                        echo "reprobe: partition map matches manifest"
                    else
                        reprobe_status="drift"
                        echo "reprobe: DRIFT — partition map has changed since capture"
                        echo "  manifest: $manifest_names"
                        echo "  current:  $current_names"
                        record_issue "target partition map drifted since capture"
                    fi
                else
                    reprobe_status="probe_failed"
                    record_issue "could not probe $REPROBE_TARGET /proc/mtd"
                fi
                ;;
        esac
    fi

    echo
    echo "=== validation summary ==="
    echo "schema_version=$schema_version"
    echo "type=$mtype"
    echo "execution_utc=$generated"
    echo "layout=$scheme"
    echo "active_slot=$active_slot"
    echo "manifest_nand_backup_complete=$nand_complete"
    echo "reprobe_status=$reprobe_status"
    echo "issues_count=$fail_count"
    if [ -n "$issues" ]; then
        printf '%s' "$issues"
    fi
    if [ "$fail_count" -gt 0 ]; then
        echo "manifest_validation=fail"
        exit 1
    fi
    echo "manifest_validation=pass"
    exit 0
}

# =============================================================================
# Entry point.
# =============================================================================

case "$MODE" in
    generate) generate_planning_manifest ;;
    validate) validate_result_manifest ;;
    *) usage ;;
esac
