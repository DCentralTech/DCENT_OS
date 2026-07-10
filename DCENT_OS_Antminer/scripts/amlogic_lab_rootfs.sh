#!/bin/bash
# amlogic_lab_rootfs.sh - Lab-only Amlogic rootfs backup/write/readback/restore

set -euo pipefail

SUBCOMMAND="${1:-}"
shift || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/am3_geometry.sh"

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"
# S19k Pro .78 U-Boot env: nandrootfs=0x0B800000, mtd5 base=0x06100000.
ROOTFS_OFFSET_HEX="$DCENT_AM3_ROOTFS_OFFSET_HEX"
ROOTFS_WINDOW_HEX="$DCENT_AM3_ROOTFS_WINDOW_HEX"
ROOTFS_ERASE_COUNT="$DCENT_AM3_ROOTFS_ERASE_COUNT"
ROOTFS_ERASESIZE_EXPECTED="$DCENT_AM3_ROOTFS_ERASESIZE_EXPECTED"
ROOTFS_OFFSET_DEC="$DCENT_AM3_ROOTFS_OFFSET_DEC"
ROOTFS_WINDOW_DEC="$DCENT_AM3_ROOTFS_WINDOW_DEC"
ROOTFS_END_DEC="$DCENT_AM3_ROOTFS_END_DEC"
TARGET_MTD_NAME="unknown"
TARGET_MTD_SIZE="0"
TARGET_MTD_ERASESIZE="0"

usage() {
    echo "Usage: $(basename "$0") <subcommand> [args]"
    echo ""
    echo "Subcommands:"
    echo "  probe <miner_ip> <artifact_dir>"
    echo "  backup <miner_ip> <artifact_dir>"
    echo "  write <miner_ip> <image> <artifact_dir> --lab-only --i-have-recovery"
    echo "  readback <miner_ip> <image> <artifact_dir>"
    echo "  restore <miner_ip> <artifact_dir> --lab-only --i-have-recovery"
    echo "  verify-runtime <miner_ip> <artifact_dir>"
    echo ""
    echo "This script is destructive for write/restore. Use only on sacrificial lab units."
}

require_lab_flags() {
    local allow="false"
    local recovery="false"
    while [ $# -gt 0 ]; do
        case "$1" in
            --lab-only) allow="true" ;;
            --i-have-recovery) recovery="true" ;;
        esac
        shift
    done
    [ "$allow" = "true" ] || { echo "Missing --lab-only" >&2; exit 1; }
    [ "$recovery" = "true" ] || { echo "Missing --i-have-recovery" >&2; exit 1; }
}

require_uint() {
    case "$2" in
        ''|*[!0-9]*) echo "$1 is not numeric: '$2'" >&2; exit 1 ;;
    esac
}

remote_sha() {
    local miner_ip="$1"
    local remote_file="$2"
    ssh $SSH_OPTS "root@$miner_ip" "sha256sum '$remote_file' | awk '{print \$1}'"
}

file_size() {
    stat -c%s "$1" 2>/dev/null || stat -f%z "$1" 2>/dev/null
}

file_sha256() {
    sha256sum "$1" | awk '{print $1}'
}

uimage_magic() {
    od -An -N4 -tx1 "$1" 2>/dev/null | tr -d ' \n'
}

require_uimage_file() {
    local image="$1"
    local label="$2"
    [ -f "$image" ] || { echo "$label not found: $image" >&2; exit 1; }
    local image_size
    image_size=$(file_size "$image")
    [ "$image_size" -le "$ROOTFS_WINDOW_DEC" ] || {
        echo "$label exceeds rootfs window: $image_size > $ROOTFS_WINDOW_DEC" >&2
        exit 1
    }
    local magic
    magic=$(uimage_magic "$image")
    [ "$magic" = "27051956" ] || {
        echo "$label is not a uImage rootfs payload (magic=$magic)" >&2
        exit 1
    }
    echo "$image_size"
}

require_recovery_artifact() {
    local artifact_dir="$1"
    local backup_image="$artifact_dir/backup.uimage"
    local backup_manifest="$artifact_dir/manifest.json"

    require_uimage_file "$backup_image" "Recovery backup image" >/dev/null
    [ -f "$backup_manifest" ] || {
        echo "Recovery manifest not found: $backup_manifest" >&2
        exit 1
    }
    grep -F "\"backup_sha256\"" "$backup_manifest" >/dev/null 2>&1 || {
        echo "Recovery manifest lacks backup_sha256: $backup_manifest" >&2
        exit 1
    }
    local expected_sha
    local actual_sha
    expected_sha=$(sed -n 's/.*"backup_sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]*\)".*/\1/p' "$backup_manifest" | head -1)
    actual_sha=$(file_sha256 "$backup_image")
    [ -n "$expected_sha" ] || {
        echo "Recovery manifest backup_sha256 is unreadable: $backup_manifest" >&2
        exit 1
    }
    [ "$expected_sha" = "$actual_sha" ] || {
        echo "Recovery backup SHA mismatch: manifest $expected_sha actual $actual_sha" >&2
        exit 1
    }
}

normalize_target_signal() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]'
}

require_exact_amlogic_target() {
    local miner_ip="$1"
    local identity
    local board_target
    local model
    local hwid
    local normalized

    identity=$(ssh $SSH_OPTS "root@$miner_ip" '
        printf "BOARD_TARGET=%s\n" "$(cat /etc/dcentos/board_target 2>/dev/null | head -1 | tr -d "[:space:]")"
        printf "MODEL=%s\n" "$(cat /config/CONF_MINER_TYPE 2>/dev/null | head -1)"
        printf "HWID=%s\n" "$(cat /config/CONF_HARDWARE_ID 2>/dev/null | head -1)"
        printf "BOS_MODEL=%s\n" "$(grep "^model" /etc/bosminer.toml 2>/dev/null | head -1)"
        printf "DT_MODEL=%s\n" "$(tr "\000" "\n" < /proc/device-tree/model 2>/dev/null | head -1)"
    ') || {
        echo "Unable to read Amlogic target identity from $miner_ip" >&2
        exit 1
    }
    board_target=$(printf '%s\n' "$identity" | sed -n 's/^BOARD_TARGET=//p' | head -1)
    model=$(printf '%s\n' "$identity" | sed -n 's/^MODEL=//p' | head -1)
    hwid=$(printf '%s\n' "$identity" | sed -n 's/^HWID=//p' | head -1)
    normalized=$(normalize_target_signal "$identity")

    case "$(normalize_target_signal "$board_target")" in
        am3s21|amlogics21|am3s19k|amlogics19k|am3s19j|am3s19jpro|amlogics19j|amlogics19jpro)
            echo "Amlogic exact target verified: board_target=${board_target}"
            return 0
            ;;
    esac

    case "$normalized" in
        *s19xp*|*s19jxp*|*t19*|*s17*|*t17*)
            echo "Refusing Amlogic rootfs-window write: ${model:-${hwid:-unknown}} is an Experimental feature / In development target for this route." >&2
            exit 1
            ;;
    esac
    case "$normalized" in
        *s21*|*s19k*|*s19j*)
            echo "Amlogic exact target verified from model/HWID: ${model:-${hwid:-unknown}}"
            ;;
        *)
            echo "Refusing Amlogic rootfs-window write: exact S21/S19K/S19j target identity was not proven." >&2
            echo "$identity" >&2
            exit 1
            ;;
    esac
}

validate_target_rootfs_geometry() {
    local miner_ip="$1"
    local mtd_name
    local mtd_size
    local mtd_erasesize

    mtd_name=$(ssh $SSH_OPTS "root@$miner_ip" "cat /sys/class/mtd/mtd5/name 2>/dev/null || echo unknown")
    mtd_size=$(ssh $SSH_OPTS "root@$miner_ip" "cat /sys/class/mtd/mtd5/size 2>/dev/null || echo 0")
    mtd_erasesize=$(ssh $SSH_OPTS "root@$miner_ip" "cat /sys/class/mtd/mtd5/erasesize 2>/dev/null || echo 0")

    require_uint "mtd5 size" "$mtd_size"
    require_uint "mtd5 erasesize" "$mtd_erasesize"

    [ "$mtd_erasesize" -eq "$ROOTFS_ERASESIZE_EXPECTED" ] || {
        echo "mtd5 erasesize $mtd_erasesize != expected $ROOTFS_ERASESIZE_EXPECTED" >&2
        exit 1
    }
    [ "$mtd_size" -ge "$ROOTFS_END_DEC" ] || {
        echo "mtd5 size $mtd_size too small for rootfs window end $ROOTFS_END_DEC" >&2
        exit 1
    }

    TARGET_MTD_NAME="$mtd_name"
    TARGET_MTD_SIZE="$mtd_size"
    TARGET_MTD_ERASESIZE="$mtd_erasesize"
    echo "mtd5 geometry OK: name=$mtd_name size=$mtd_size erasesize=$mtd_erasesize window=${ROOTFS_OFFSET_HEX}+${ROOTFS_WINDOW_HEX}"
}

probe_common() {
    local miner_ip="$1"
    local artifact_dir="$2"
    mkdir -p "$artifact_dir"
    ssh $SSH_OPTS "root@$miner_ip" '
        echo "=== identity ==="
        uname -a
        cat /proc/device-tree/model 2>/dev/null || true
        cat /etc/bos_version 2>/dev/null || true
        echo "=== storage ==="
        cat /proc/mtd
        for f in /sys/class/mtd/mtd5/erasesize /sys/class/mtd/mtd5/size /sys/class/mtd/mtd5/name; do [ -f "$f" ] && echo "$f=$(cat $f)"; done
        echo "=== runtime ==="
        grep -E "PSU|temperature|Share ACCEPTED|ERROR|WARN" /tmp/dcentrald.log 2>/dev/null | tail -100 || true
    ' > "$artifact_dir/probe.txt"
}

case "$SUBCOMMAND" in
    probe)
        MINER_IP="${1:?probe requires <miner_ip>}"
        ARTIFACT_DIR="${2:?probe requires <artifact_dir>}"
        probe_common "$MINER_IP" "$ARTIFACT_DIR"
        ;;

    backup)
        MINER_IP="${1:?backup requires <miner_ip>}"
        ARTIFACT_DIR="${2:?backup requires <artifact_dir>}"
        mkdir -p "$ARTIFACT_DIR"
        validate_target_rootfs_geometry "$MINER_IP"
        probe_common "$MINER_IP" "$ARTIFACT_DIR"
        REMOTE_BACKUP_SHA=$(ssh $SSH_OPTS "root@$MINER_IP" "nanddump --bb=skipbad -s $ROOTFS_OFFSET_HEX -l $ROOTFS_WINDOW_HEX -q -f /tmp/amlogic_backup.uimage $ROOTFS_MTD && sha256sum /tmp/amlogic_backup.uimage | awk '{print \$1}'")
        scp -O $SSH_OPTS "root@$MINER_IP:/tmp/amlogic_backup.uimage" "$ARTIFACT_DIR/backup.uimage"
        ssh $SSH_OPTS "root@$MINER_IP" "rm -f /tmp/amlogic_backup.uimage"
        BACKUP_SIZE=$(require_uimage_file "$ARTIFACT_DIR/backup.uimage" "Backup image")
        BACKUP_SHA=$(file_sha256 "$ARTIFACT_DIR/backup.uimage")
        [ "$REMOTE_BACKUP_SHA" = "$BACKUP_SHA" ] || {
            echo "Backup transfer SHA mismatch: remote $REMOTE_BACKUP_SHA local $BACKUP_SHA" >&2
            exit 1
        }
        cat > "$ARTIFACT_DIR/manifest.json" <<EOF
{
  "mtd": "$ROOTFS_MTD",
  "offset": "$ROOTFS_OFFSET_HEX",
  "window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$TARGET_MTD_NAME",
  "mtd5_size": $TARGET_MTD_SIZE,
  "mtd5_erasesize": $TARGET_MTD_ERASESIZE,
  "backup_size": $BACKUP_SIZE,
  "backup_sha256": "$BACKUP_SHA",
  "remote_backup_sha256": "$REMOTE_BACKUP_SHA"
}
EOF
        echo "Backup saved to $ARTIFACT_DIR/backup.uimage"
        ;;

    write)
        MINER_IP="${1:?write requires <miner_ip>}"
        IMAGE="${2:?write requires <image>}"
        ARTIFACT_DIR="${3:?write requires <artifact_dir>}"
        shift 3
        require_lab_flags "$@"
        mkdir -p "$ARTIFACT_DIR"
        require_recovery_artifact "$ARTIFACT_DIR"
        IMAGE_SIZE=$(require_uimage_file "$IMAGE" "Candidate image")
        require_exact_amlogic_target "$MINER_IP"
        validate_target_rootfs_geometry "$MINER_IP"
        LOCAL_SHA=$(file_sha256 "$IMAGE")
        scp -O $SSH_OPTS "$IMAGE" "root@$MINER_IP:/tmp/dcentos_candidate.uimage"
        REMOTE_SHA=$(remote_sha "$MINER_IP" "/tmp/dcentos_candidate.uimage")
        [ "$LOCAL_SHA" = "$REMOTE_SHA" ] || {
            echo "Candidate upload SHA mismatch: local $LOCAL_SHA remote $REMOTE_SHA" >&2
            exit 1
        }
        ssh $SSH_OPTS "root@$MINER_IP" "flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX $ROOTFS_ERASE_COUNT && nandwrite -p -s $ROOTFS_OFFSET_HEX $ROOTFS_MTD /tmp/dcentos_candidate.uimage"
        READBACK_SHA=$(ssh $SSH_OPTS "root@$MINER_IP" "nanddump --bb=skipbad -s $ROOTFS_OFFSET_HEX -l $IMAGE_SIZE -q -f /tmp/dcentos_candidate_readback.uimage $ROOTFS_MTD && sha256sum /tmp/dcentos_candidate_readback.uimage | awk '{print \$1}'")
        scp -O $SSH_OPTS "root@$MINER_IP:/tmp/dcentos_candidate_readback.uimage" "$ARTIFACT_DIR/write_readback.uimage"
        ssh $SSH_OPTS "root@$MINER_IP" "rm -f /tmp/dcentos_candidate.uimage /tmp/dcentos_candidate_readback.uimage"
        LOCAL_READBACK_SHA=$(file_sha256 "$ARTIFACT_DIR/write_readback.uimage")
        [ "$READBACK_SHA" = "$LOCAL_SHA" ] && [ "$LOCAL_READBACK_SHA" = "$LOCAL_SHA" ] || {
            echo "Post-write readback SHA mismatch: expected $LOCAL_SHA got remote $READBACK_SHA local $LOCAL_READBACK_SHA" >&2
            exit 1
        }
        cat > "$ARTIFACT_DIR/write_manifest.json" <<EOF
{
  "mtd": "$ROOTFS_MTD",
  "offset": "$ROOTFS_OFFSET_HEX",
  "window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$TARGET_MTD_NAME",
  "mtd5_size": $TARGET_MTD_SIZE,
  "mtd5_erasesize": $TARGET_MTD_ERASESIZE,
  "image_size": $IMAGE_SIZE,
  "image_sha256": "$LOCAL_SHA",
  "remote_upload_sha256": "$REMOTE_SHA",
  "readback_sha256": "$LOCAL_READBACK_SHA",
  "readback_artifact": "write_readback.uimage"
}
EOF
        echo "Write verified by readback: $READBACK_SHA"
        ;;

    readback)
        MINER_IP="${1:?readback requires <miner_ip>}"
        IMAGE="${2:?readback requires <image>}"
        ARTIFACT_DIR="${3:?readback requires <artifact_dir>}"
        mkdir -p "$ARTIFACT_DIR"
        IMAGE_SIZE=$(require_uimage_file "$IMAGE" "Expected image")
        validate_target_rootfs_geometry "$MINER_IP"
        REMOTE_READBACK_SHA=$(ssh $SSH_OPTS "root@$MINER_IP" "nanddump --bb=skipbad -s $ROOTFS_OFFSET_HEX -l $IMAGE_SIZE -q -f /tmp/dcentos_readback.uimage $ROOTFS_MTD && sha256sum /tmp/dcentos_readback.uimage | awk '{print \$1}'")
        scp -O $SSH_OPTS "root@$MINER_IP:/tmp/dcentos_readback.uimage" "$ARTIFACT_DIR/readback.uimage"
        ssh $SSH_OPTS "root@$MINER_IP" "rm -f /tmp/dcentos_readback.uimage"
        EXPECTED_SHA=$(file_sha256 "$IMAGE")
        ACTUAL_SHA=$(file_sha256 "$ARTIFACT_DIR/readback.uimage")
        [ "$EXPECTED_SHA" = "$ACTUAL_SHA" ] && [ "$REMOTE_READBACK_SHA" = "$EXPECTED_SHA" ] || {
            echo "Readback SHA mismatch: expected $EXPECTED_SHA got remote $REMOTE_READBACK_SHA local $ACTUAL_SHA" >&2
            exit 1
        }
        cat > "$ARTIFACT_DIR/readback_manifest.json" <<EOF
{
  "mtd": "$ROOTFS_MTD",
  "offset": "$ROOTFS_OFFSET_HEX",
  "window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$TARGET_MTD_NAME",
  "mtd5_size": $TARGET_MTD_SIZE,
  "mtd5_erasesize": $TARGET_MTD_ERASESIZE,
  "expected_size": $IMAGE_SIZE,
  "expected_sha256": "$EXPECTED_SHA",
  "remote_readback_sha256": "$REMOTE_READBACK_SHA",
  "readback_sha256": "$ACTUAL_SHA",
  "readback_artifact": "readback.uimage"
}
EOF
        echo "Readback verified: $ACTUAL_SHA"
        ;;

    restore)
        MINER_IP="${1:?restore requires <miner_ip>}"
        ARTIFACT_DIR="${2:?restore requires <artifact_dir>}"
        shift 2
        require_lab_flags "$@"
        BACKUP_IMAGE="$ARTIFACT_DIR/backup.uimage"
        BACKUP_SIZE=$(require_uimage_file "$BACKUP_IMAGE" "Backup image")
        require_exact_amlogic_target "$MINER_IP"
        validate_target_rootfs_geometry "$MINER_IP"
        LOCAL_SHA=$(file_sha256 "$BACKUP_IMAGE")
        scp -O $SSH_OPTS "$BACKUP_IMAGE" "root@$MINER_IP:/tmp/amlogic_restore.uimage"
        REMOTE_SHA=$(remote_sha "$MINER_IP" "/tmp/amlogic_restore.uimage")
        [ "$LOCAL_SHA" = "$REMOTE_SHA" ] || {
            echo "Restore upload SHA mismatch: local $LOCAL_SHA remote $REMOTE_SHA" >&2
            exit 1
        }
        ssh $SSH_OPTS "root@$MINER_IP" "flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX $ROOTFS_ERASE_COUNT && nandwrite -p -s $ROOTFS_OFFSET_HEX $ROOTFS_MTD /tmp/amlogic_restore.uimage"
        RESTORE_REMOTE_READBACK_SHA=$(ssh $SSH_OPTS "root@$MINER_IP" "nanddump --bb=skipbad -s $ROOTFS_OFFSET_HEX -l $BACKUP_SIZE -q -f /tmp/amlogic_restore_readback.uimage $ROOTFS_MTD && sha256sum /tmp/amlogic_restore_readback.uimage | awk '{print \$1}'")
        scp -O $SSH_OPTS "root@$MINER_IP:/tmp/amlogic_restore_readback.uimage" "$ARTIFACT_DIR/restore_readback.uimage"
        ssh $SSH_OPTS "root@$MINER_IP" "rm -f /tmp/amlogic_restore.uimage /tmp/amlogic_restore_readback.uimage"
        EXPECTED_SHA=$(file_sha256 "$BACKUP_IMAGE")
        ACTUAL_SHA=$(file_sha256 "$ARTIFACT_DIR/restore_readback.uimage")
        [ "$EXPECTED_SHA" = "$ACTUAL_SHA" ] && [ "$RESTORE_REMOTE_READBACK_SHA" = "$EXPECTED_SHA" ] || {
            echo "Restore SHA mismatch: expected $EXPECTED_SHA got remote $RESTORE_REMOTE_READBACK_SHA local $ACTUAL_SHA" >&2
            exit 1
        }
        cat > "$ARTIFACT_DIR/restore_manifest.json" <<EOF
{
  "mtd": "$ROOTFS_MTD",
  "offset": "$ROOTFS_OFFSET_HEX",
  "window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$TARGET_MTD_NAME",
  "mtd5_size": $TARGET_MTD_SIZE,
  "mtd5_erasesize": $TARGET_MTD_ERASESIZE,
  "backup_size": $BACKUP_SIZE,
  "backup_sha256": "$EXPECTED_SHA",
  "remote_upload_sha256": "$REMOTE_SHA",
  "remote_readback_sha256": "$RESTORE_REMOTE_READBACK_SHA",
  "readback_sha256": "$ACTUAL_SHA",
  "readback_artifact": "restore_readback.uimage"
}
EOF
        echo "Restore verified: $ACTUAL_SHA"
        ;;

    verify-runtime)
        MINER_IP="${1:?verify-runtime requires <miner_ip>}"
        ARTIFACT_DIR="${2:?verify-runtime requires <artifact_dir>}"
        probe_common "$MINER_IP" "$ARTIFACT_DIR"
        if command -v curl >/dev/null 2>&1; then
            curl -fsS "http://$MINER_IP:8080/api/status" > "$ARTIFACT_DIR/api_status.json" 2>/dev/null || true
            curl -fsS "http://$MINER_IP:8080/api/system/info" > "$ARTIFACT_DIR/api_system_info.json" 2>/dev/null || true
        fi
        ;;

    *)
        usage
        exit 1
        ;;
esac
