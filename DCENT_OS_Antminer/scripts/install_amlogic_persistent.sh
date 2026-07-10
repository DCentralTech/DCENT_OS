#!/bin/bash
# DCENTos - Amlogic persistent NAND install (am3-aml S19K/S21)
# D-Central Technologies, 2026
#
# Bridge-firmware-required workflow: target must be on BraiinsOS+ (or
# LuxOS) with root SSH BEFORE this script runs. Stock S19j Pro Amlogic
# only has miner:miner SSH; that path is not yet adapter-backed.
# See plans/zesty-cooking-bee.md Phase R for full context.
#
# Run from operator's host. Performs:
#   0. Local package-only validation (prefix, manifest, SHA256SUMS, uImage).
#   1. SSH preflight: root shell, required tools (nandwrite, flash_erase,
#      fw_setenv, sha256sum), platform=am3-aml.
#   2. Backup /dev/nand_env + /dev/mtd5 + fw_printenv to --artifact-dir.
#   3. SCP sysupgrade tar to /data, verify SHA256 (uses /data not /tmp --
# for rationale).
#   4. Extract tar, verify SHA256SUMS, validate MANIFEST.json board.
#   5. (--dry-run halts here; mining services are not stopped.)
#   6. Confirm destructive operation.
#   7. Stop bosminer/boser/bos-tools cleanly.
#   8. flash_erase + nandwrite rootfs to mtd5 LOCAL offset 0x05700000.
#   9. nanddump readback, set firstboot, verify env, sync + reboot.
#  10. Print monitoring instructions.
#
# After reboot: operator polls `dcent detect <ip>` for DCENTOS state. If
# anything fails, U-Boot reverts to mtd2 stock_system on the next power
# cycle (graceful rollback, not brick).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/lib/am3_geometry.sh"
ROOTFS_MTD="$DCENT_AM3_ROOTFS_MTD"
ROOTFS_OFFSET_HEX="$DCENT_AM3_ROOTFS_OFFSET_HEX"
ROOTFS_WINDOW_HEX="$DCENT_AM3_ROOTFS_WINDOW_HEX"
ROOTFS_ERASE_COUNT="$DCENT_AM3_ROOTFS_ERASE_COUNT"
ROOTFS_ERASESIZE_EXPECTED="$DCENT_AM3_ROOTFS_ERASESIZE_EXPECTED"
ROOTFS_OFFSET_DEC="$DCENT_AM3_ROOTFS_OFFSET_DEC"
ROOTFS_WINDOW_DEC="$DCENT_AM3_ROOTFS_WINDOW_DEC"
ROOTFS_END_DEC="$DCENT_AM3_ROOTFS_END_DEC"

usage() {
    cat >&2 <<USAGE
Usage: $(basename "$0") <miner_ip> --firmware <sysupgrade.tar> --artifact-dir <dir> [--variant s19kpro|s21] [--dry-run] [--yes]

Required:
  <miner_ip>             Target miner IP (must be on BraiinsOS+/LuxOS with root SSH)
  --firmware <tar>       Path to a DCENT_OS AM3 sysupgrade tar
  --artifact-dir <dir>   Local dir to store nand_env + mtd5 backup + fw_env

Options:
  --variant s19kpro|s21  Package variant to validate/write (default: s19kpro)
  --dry-run              Run preflight + backup + SHA256 verify only; no flash
  --yes                  Skip interactive destructive-step confirmation

Environment:
  DCENT_PASSWORD         Optional SSH password (else SSH agent / keys)
USAGE
    exit 2
}

[ $# -ge 1 ] || usage
MINER_IP="$1"
shift

FIRMWARE=""
ARTIFACT_DIR=""
VARIANT="s19kpro"
DRY_RUN=false
SKIP_CONFIRM=false

while [ $# -gt 0 ]; do
    case "$1" in
        --firmware)     FIRMWARE="${2:?--firmware requires path}"; shift 2 ;;
        --artifact-dir) ARTIFACT_DIR="${2:?--artifact-dir requires path}"; shift 2 ;;
        --variant)      VARIANT="${2:?--variant requires s19kpro or s21}"; shift 2 ;;
        --dry-run)      DRY_RUN=true; shift ;;
        --yes)          SKIP_CONFIRM=true; shift ;;
        -h|--help)      usage ;;
        *)              echo "Unknown arg: $1" >&2; usage ;;
    esac
done

[ -n "$FIRMWARE" ]     || { echo "ERROR: --firmware required" >&2; exit 2; }
[ -n "$ARTIFACT_DIR" ] || { echo "ERROR: --artifact-dir required" >&2; exit 2; }
[ -f "$FIRMWARE" ]     || { echo "ERROR: $FIRMWARE not found" >&2; exit 2; }
mkdir -p "$ARTIFACT_DIR"

case "$VARIANT" in
    s19kpro|s19k)
        BOARD_PKG_NAME="am3-s19k"
        PACKAGE_PREFIX="sysupgrade-am3-s19k"
        ;;
    s21)
        BOARD_PKG_NAME="am3-s21"
        PACKAGE_PREFIX="sysupgrade-am3-s21"
        ;;
    *)
        echo "ERROR: unsupported --variant: $VARIANT (supported: s19kpro, s21)" >&2
        exit 2
        ;;
esac
REMOTE_PREFIX="/data/sysupgrade/$PACKAGE_PREFIX"

SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10 -o BatchMode=no"
log() { printf '[install_amlogic_persistent] %s\n' "$*"; }

ssh_run() {
    if [ -n "${DCENT_PASSWORD:-}" ] && command -v sshpass >/dev/null 2>&1; then
        sshpass -p "$DCENT_PASSWORD" ssh $SSH_OPTS "root@${MINER_IP}" "$1"
    else
        ssh $SSH_OPTS "root@${MINER_IP}" "$1"
    fi
}

scp_put() {
    if [ -n "${DCENT_PASSWORD:-}" ] && command -v sshpass >/dev/null 2>&1; then
        sshpass -p "$DCENT_PASSWORD" scp -O $SSH_OPTS "$1" "root@${MINER_IP}:$2"
    else
        scp -O $SSH_OPTS "$1" "root@${MINER_IP}:$2"
    fi
}

scp_get() {
    if [ -n "${DCENT_PASSWORD:-}" ] && command -v sshpass >/dev/null 2>&1; then
        sshpass -p "$DCENT_PASSWORD" scp -O $SSH_OPTS "root@${MINER_IP}:$1" "$2"
    else
        scp -O $SSH_OPTS "root@${MINER_IP}:$1" "$2"
    fi
}

local_sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    elif command -v shasum  >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else echo ""; fi
}

require_uint() {
    case "$2" in
        ''|*[!0-9]*) log "ERROR: $1 is not numeric: '$2'"; exit 1 ;;
    esac
}

normalize_target_signal() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]'
}

require_exact_amlogic_variant() {
    local variant="$1"
    local platform="$2"
    local identity
    local board_target
    local model
    local hwid
    local normalized
    local board_norm

    identity=$(ssh_run '
        printf "BOARD_TARGET=%s\n" "$(cat /etc/dcentos/board_target 2>/dev/null | head -1 | tr -d "[:space:]")"
        printf "MODEL=%s\n" "$(cat /config/CONF_MINER_TYPE 2>/dev/null | head -1)"
        printf "HWID=%s\n" "$(cat /config/CONF_HARDWARE_ID 2>/dev/null | head -1)"
        printf "BOS_MODEL=%s\n" "$(grep "^model" /etc/bosminer.toml 2>/dev/null | head -1)"
        printf "DT_MODEL=%s\n" "$(tr "\000" "\n" < /proc/device-tree/model 2>/dev/null | head -1)"
    ') || { log "ERROR: unable to read exact Amlogic target identity"; exit 1; }

    board_target=$(printf '%s\n' "$identity" | sed -n 's/^BOARD_TARGET=//p' | head -1)
    model=$(printf '%s\n' "$identity" | sed -n 's/^MODEL=//p' | head -1)
    hwid=$(printf '%s\n' "$identity" | sed -n 's/^HWID=//p' | head -1)
    normalized=$(normalize_target_signal "$identity")
    board_norm=$(normalize_target_signal "$board_target")

    case "$normalized" in
        *s19xp*|*s19jxp*|*t19*|*s17*|*t17*)
            log "ERROR: ${model:-${hwid:-unknown}} is an Experimental feature / In development target for this installer; refusing destructive flash"
            exit 1
            ;;
    esac

    case "$variant" in
        s19kpro|s19k)
            case "$board_norm" in
                am3s19k|amlogics19k)
                    log "  exact target OK: board_target=$board_target"
                    return 0
                    ;;
            esac
            case "$normalized" in
                *s19k*) ;;
                *)
                    log "ERROR: --variant $variant requires exact S19K target identity before flash"
                    log "$identity"
                    exit 1
                    ;;
            esac
            ;;
        s21)
            case "$board_norm" in
                am3s21|amlogics21)
                    log "  exact target OK: board_target=$board_target"
                    return 0
                    ;;
            esac
            case "$normalized" in
                *s21pro*|*s21xp*)
                    log "ERROR: S21 Pro / S21 XP are distinct In development carriers for this installer"
                    exit 1
                    ;;
                *s21*) ;;
                *)
                    log "ERROR: --variant s21 requires exact base-S21 target identity before flash"
                    log "$identity"
                    exit 1
                    ;;
            esac
            ;;
        *)
            log "ERROR: unsupported variant identity gate: $variant"
            exit 1
            ;;
    esac
    log "  exact target OK: ${model:-${hwid:-$platform}}"
}

# --- Step 0: local package-only validation --------------------------------
log "Step 0/10: local package-only validation for $FIRMWARE ($BOARD_PKG_NAME)"
bash "$SCRIPT_DIR/pre_flash_validate.sh" --package-only "$FIRMWARE" "$BOARD_PKG_NAME"

# --- Step 1: SSH + tool preflight ----------------------------------------
log "Step 1/10: SSH preflight on root@$MINER_IP"
ssh_run "echo SSH_OK" >/dev/null || { log "ERROR: SSH failed"; exit 1; }

PLATFORM=$(ssh_run "cat /etc/bos_platform 2>/dev/null || cat /etc/dcentos-platform 2>/dev/null || echo unknown")
log "  platform: $PLATFORM"
case "$PLATFORM" in
    am3-aml*) ;;
    *) log "ERROR: not am3-aml - refusing destructive flash on $PLATFORM"; exit 1 ;;
esac
require_exact_amlogic_variant "$VARIANT" "$PLATFORM"

MISSING=$(ssh_run 'for t in nandwrite flash_erase fw_setenv fw_printenv sha256sum nanddump tar dd; do command -v $t >/dev/null 2>&1 || echo $t; done')
if [ -n "$MISSING" ]; then
    log "ERROR: missing required tools on target: $MISSING"
    exit 1
fi
log "  tools OK: nandwrite flash_erase fw_setenv nanddump tar sha256sum dd"

MTD5_NAME=$(ssh_run "cat /sys/class/mtd/mtd5/name 2>/dev/null || echo unknown")
MTD5_SIZE=$(ssh_run "cat /sys/class/mtd/mtd5/size 2>/dev/null || echo 0")
MTD5_ERASESIZE=$(ssh_run "cat /sys/class/mtd/mtd5/erasesize 2>/dev/null || echo 0")
require_uint "mtd5 size" "$MTD5_SIZE"
require_uint "mtd5 erasesize" "$MTD5_ERASESIZE"
if [ "$MTD5_ERASESIZE" -ne "$ROOTFS_ERASESIZE_EXPECTED" ]; then
    log "ERROR: mtd5 erasesize $MTD5_ERASESIZE != expected $ROOTFS_ERASESIZE_EXPECTED"
    exit 1
fi
if [ "$MTD5_SIZE" -lt "$ROOTFS_END_DEC" ]; then
    log "ERROR: mtd5 size $MTD5_SIZE too small for rootfs window end $ROOTFS_END_DEC"
    exit 1
fi
log "  mtd5 geometry OK: name=$MTD5_NAME size=$MTD5_SIZE erasesize=$MTD5_ERASESIZE window=${ROOTFS_OFFSET_HEX}+${ROOTFS_WINDOW_HEX}"

# --- Step 2: backup nand_env + mtd5 + fw_env -----------------------------
log "Step 2/10: backup nand_env + mtd5 + fw_printenv to $ARTIFACT_DIR"
ssh_run "fw_printenv" > "$ARTIFACT_DIR/fw_env_pre.txt"
[ -s "$ARTIFACT_DIR/fw_env_pre.txt" ] || { log "ERROR: fw_printenv backup is empty"; exit 1; }
NAND_ENV_REMOTE_SHA=$(ssh_run "dd if=/dev/nand_env of=/tmp/nand_env_pre.bin bs=64K count=1 >/dev/null 2>&1 && sha256sum /tmp/nand_env_pre.bin | awk '{print \$1}'")
scp_get "/tmp/nand_env_pre.bin" "$ARTIFACT_DIR/nand_env.bak"
ssh_run "rm -f /tmp/nand_env_pre.bin"
NAND_ENV_SIZE=$(wc -c < "$ARTIFACT_DIR/nand_env.bak")
require_uint "nand_env backup size" "$NAND_ENV_SIZE"
[ "$NAND_ENV_SIZE" -eq 65536 ] || { log "ERROR: nand_env backup size $NAND_ENV_SIZE != 65536"; exit 1; }
NAND_ENV_LOCAL_SHA=$(local_sha256 "$ARTIFACT_DIR/nand_env.bak")
[ "$NAND_ENV_REMOTE_SHA" = "$NAND_ENV_LOCAL_SHA" ] || {
    log "ERROR: nand_env backup SHA mismatch: remote $NAND_ENV_REMOTE_SHA local $NAND_ENV_LOCAL_SHA"
    exit 1
}
log "  nand_env.bak: $NAND_ENV_SIZE bytes sha256=$NAND_ENV_LOCAL_SHA"

MTD5_REMOTE_SHA=$(ssh_run "nanddump --bb=skipbad -f /tmp/mtd5_pre.bin $ROOTFS_MTD >/dev/null 2>&1 && sha256sum /tmp/mtd5_pre.bin | awk '{print \$1}'")
scp_get "/tmp/mtd5_pre.bin" "$ARTIFACT_DIR/mtd5_pre_install.bin"
ssh_run "rm -f /tmp/mtd5_pre.bin"
MTD5_LOCAL_SHA=$(local_sha256 "$ARTIFACT_DIR/mtd5_pre_install.bin")
[ "$MTD5_REMOTE_SHA" = "$MTD5_LOCAL_SHA" ] || {
    log "ERROR: mtd5 backup SHA mismatch: remote $MTD5_REMOTE_SHA local $MTD5_LOCAL_SHA"
    exit 1
}
MTD5_BACKUP_SIZE=$(wc -c < "$ARTIFACT_DIR/mtd5_pre_install.bin")
require_uint "mtd5 backup size" "$MTD5_BACKUP_SIZE"
[ "$MTD5_BACKUP_SIZE" -gt 0 ] || { log "ERROR: mtd5 backup is empty"; exit 1; }
log "  mtd5_pre_install.bin: $MTD5_BACKUP_SIZE bytes sha256=$MTD5_LOCAL_SHA"

# --- Step 3: SCP tar + SHA256 verify -------------------------------------
# Stage on /data not /tmp -- sysupgrade tarball + extracted squashfs blow the
# 64 MB tmpfs at /tmp on S9 and leave little headroom on Amlogic /tmp either.
#.
log "Step 3/10: /data free-space preflight on $MINER_IP"
DATA_FREE_KB=$(ssh_run "df -Pk /data 2>/dev/null | awk 'NR==2 {print \$4}'")
case "$DATA_FREE_KB" in
    ''|*[!0-9]*) log "ERROR: could not determine /data free space (got '$DATA_FREE_KB')"; exit 1 ;;
esac
log "  /data free: $((DATA_FREE_KB / 1024)) MB"
if [ "$DATA_FREE_KB" -lt 51200 ]; then
    log "ERROR: /data has only $((DATA_FREE_KB / 1024)) MB free; need >= 50 MB for sysupgrade tar + extraction. Clear /data/dcentos-sysupgrade.tar and /data/sysupgrade/ first."
    exit 1
fi
DATA_WRITABLE=$(ssh_run "touch /data/.dcent_stage_check 2>/dev/null && rm -f /data/.dcent_stage_check && echo yes || echo no")
[ "$DATA_WRITABLE" = "yes" ] || { log "ERROR: /data not writable on $MINER_IP"; exit 1; }
log "Step 3/10: SCP $FIRMWARE -> /data/dcentos-sysupgrade.tar"
LOCAL_SHA=$(local_sha256 "$FIRMWARE")
[ -n "$LOCAL_SHA" ] || { log "ERROR: cannot compute local sha256"; exit 1; }
log "  local SHA256: $LOCAL_SHA"
cat > "$ARTIFACT_DIR/install_preflight_manifest.json" <<EOF
{
  "stage": "backup_verified",
  "miner_ip": "$MINER_IP",
  "platform": "$PLATFORM",
  "rootfs_mtd": "$ROOTFS_MTD",
  "rootfs_offset": "$ROOTFS_OFFSET_HEX",
  "rootfs_window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$MTD5_NAME",
  "mtd5_size": $MTD5_SIZE,
  "mtd5_erasesize": $MTD5_ERASESIZE,
  "nand_env_sha256": "$NAND_ENV_LOCAL_SHA",
  "mtd5_backup_sha256": "$MTD5_LOCAL_SHA",
  "firmware_sha256": "$LOCAL_SHA",
  "dry_run": $DRY_RUN
}
EOF
log "  recovery manifest: $ARTIFACT_DIR/install_preflight_manifest.json"

scp_put "$FIRMWARE" "/data/dcentos-sysupgrade.tar"
REMOTE_SHA=$(ssh_run "sha256sum /data/dcentos-sysupgrade.tar | awk '{print \$1}'")
log "  remote SHA256: $REMOTE_SHA"
[ "$LOCAL_SHA" = "$REMOTE_SHA" ] || { log "ERROR: SHA256 mismatch after upload"; exit 1; }

# --- Step 4: extract + verify SHA256SUMS + validate MANIFEST -------------
log "Step 4/10: extract tar + verify SHA256SUMS + check MANIFEST.json"
ssh_run 'rm -rf /data/sysupgrade && mkdir -p /data/sysupgrade && cd /data/sysupgrade && tar xf /data/dcentos-sysupgrade.tar'
ssh_run "cd '$REMOTE_PREFIX' && sha256sum -c SHA256SUMS" \
    || { log "ERROR: SHA256SUMS check failed on target"; exit 1; }

BOARD=$(ssh_run "grep -o '\"board\":[[:space:]]*\"[^\"]*\"' '$REMOTE_PREFIX/MANIFEST.json' | head -1")
log "  manifest: $BOARD"
echo "$BOARD" | grep -q "$BOARD_PKG_NAME" || { log "ERROR: MANIFEST board != $BOARD_PKG_NAME"; exit 1; }

# --- Step 4b: in-band ed25519 re-verify (defense-in-depth, sentinel-gated) ----
# wf_c00e5d9e: if the on-device DCENT_OS exposes the verify-bundle capability
# sentinel (dropped by a verb-capable dcentrald at startup -- see main.rs), use its
# OWN pinned-key ed25519 verifier to re-check the INCOMING bundle ON-DEVICE before
# flashing -- defense-in-depth against a compromised install host. The normal case
# is already gated by host-side ed25519 + the on-device SHA256SUMS + MANIFEST checks
# above; this adds the on-device signature authority. The sentinel GUARANTEES the
# verb exists, so invoking it is SAFE (no risk of starting the daemon on a pre-verb
# binary -- the probe-safety problem). Skips gracefully when absent (fresh
# stock->DCENT_OS install, or a dcentrald predating the verb). A failed verify
# ABORTS before any flash (fail-closed).
if ssh_run "[ -f /data/dcentos/caps/verify-bundle ]"; then
    DCENTRALD_BIN=$(ssh_run "command -v dcentrald 2>/dev/null || echo /usr/bin/dcentrald")
    log "Step 4b/10: in-band ed25519 re-verify via on-device $DCENTRALD_BIN --verify-bundle"
    if ssh_run "'$DCENTRALD_BIN' --verify-bundle '$REMOTE_PREFIX'"; then
        log "  in-band ed25519 signature + MANIFEST verified on-device (known-good binary verified the incoming bundle)"
    else
        log "ERROR: in-band ed25519 re-verify FAILED -- aborting before flash (on-device verifier rejected the bundle)"
        exit 1
    fi
else
    log "Step 4b/10: in-band ed25519 re-verify SKIPPED -- no verify-bundle capability sentinel on-device (host-side ed25519 + on-device SHA256SUMS + MANIFEST already verified this bundle)"
fi

ROOT_SIZE=$(ssh_run "stat -c %s '$REMOTE_PREFIX/root'")
require_uint "root payload size" "$ROOT_SIZE"
if [ "$ROOT_SIZE" -gt "$ROOTFS_WINDOW_DEC" ]; then
    log "ERROR: root payload $ROOT_SIZE exceeds rootfs window $ROOTFS_WINDOW_DEC"
    exit 1
fi
ROOT_SHA=$(ssh_run "sha256sum '$REMOTE_PREFIX/root' | awk '{print \$1}'")
log "  root payload: $ROOT_SIZE bytes (expect ~25.6 MB)"
KERNEL_SIZE=$(ssh_run "stat -c %s '$REMOTE_PREFIX/kernel'")
require_uint "kernel payload size" "$KERNEL_SIZE"
KERNEL_SHA=$(ssh_run "sha256sum '$REMOTE_PREFIX/kernel' | awk '{print \$1}'")
log "  kernel payload: $KERNEL_SIZE bytes (expect ~16.5 MB)"
cat > "$ARTIFACT_DIR/install_preflight_manifest.json" <<EOF
{
  "stage": "payload_verified",
  "miner_ip": "$MINER_IP",
  "platform": "$PLATFORM",
  "rootfs_mtd": "$ROOTFS_MTD",
  "rootfs_offset": "$ROOTFS_OFFSET_HEX",
  "rootfs_window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$MTD5_NAME",
  "mtd5_size": $MTD5_SIZE,
  "mtd5_erasesize": $MTD5_ERASESIZE,
  "nand_env_size": $NAND_ENV_SIZE,
  "nand_env_sha256": "$NAND_ENV_LOCAL_SHA",
  "mtd5_backup_size": $MTD5_BACKUP_SIZE,
  "mtd5_backup_sha256": "$MTD5_LOCAL_SHA",
  "firmware_sha256": "$LOCAL_SHA",
  "remote_firmware_sha256": "$REMOTE_SHA",
  "package_board": "$BOARD_PKG_NAME",
  "root_payload_size": $ROOT_SIZE,
  "root_payload_sha256": "$ROOT_SHA",
  "kernel_payload_size": $KERNEL_SIZE,
  "kernel_payload_sha256": "$KERNEL_SHA",
  "dry_run": $DRY_RUN
}
EOF
log "  recovery manifest updated with payload hashes"

# --- Step 5: dry-run halts here ------------------------------------------
if [ "$DRY_RUN" = true ]; then
    log "[DRY RUN] preflight + backup + SHA256 + manifest + extract OK"
    log "[DRY RUN] mining services were not stopped."
    log "[DRY RUN] would now: flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX, nandwrite root, readback SHA verify, fw_setenv firstboot=1, reboot"
    log "[DRY RUN] no destructive action taken. Exiting."
    exit 0
fi

# --- Step 6: destructive confirmation gate -------------------------------
if [ "$SKIP_CONFIRM" != true ]; then
    cat >&2 <<EOF

  *** DESTRUCTIVE OPERATION ***
  About to flash DCENT_OS to $MINER_IP ${ROOTFS_MTD} LOCAL offset ${ROOTFS_OFFSET_HEX}.
  Backup of pre-state is in: $ARTIFACT_DIR
  Recovery: U-Boot will revert to mtd2 stock_system if first boot fails.
  Type YES (uppercase) to proceed:
EOF
    read -r CONFIRM
    [ "$CONFIRM" = "YES" ] || { log "User declined. Exiting."; exit 1; }
fi

# --- Step 7: stop bosminer cleanly ---------------------------------------
log "Step 7/10: stop bosminer/boser/bos-tools (after confirmation, graceful TERM, then KILL)"
ssh_run 'for p in bos-tools bosminer boser; do
    pid=$(pidof $p 2>/dev/null || true)
    [ -n "$pid" ] && kill -TERM $pid 2>/dev/null || true
done; sleep 5
for p in bos-tools bosminer boser; do
    pid=$(pidof $p 2>/dev/null || true)
    [ -n "$pid" ] && kill -9 $pid 2>/dev/null || true
done; true'

# --- Step 8: flash_erase + nandwrite -------------------------------------
log "Step 8/10: flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX $ROOTFS_ERASE_COUNT  (${ROOTFS_ERASE_COUNT} erase blocks of ${ROOTFS_ERASESIZE_EXPECTED} bytes)"
ssh_run "flash_erase $ROOTFS_MTD $ROOTFS_OFFSET_HEX $ROOTFS_ERASE_COUNT" || { log "ERROR: flash_erase failed"; exit 1; }

log "Step 9/10: nandwrite -p -s $ROOTFS_OFFSET_HEX $ROOTFS_MTD root  ($ROOT_SIZE bytes)"
ssh_run "nandwrite -p -s $ROOTFS_OFFSET_HEX $ROOTFS_MTD '$REMOTE_PREFIX/root'" \
    || { log "ERROR: nandwrite failed"; exit 1; }

log "Step 9b/10: readback verify written rootfs SHA256"
READBACK_SHA=$(ssh_run "nanddump --bb=skipbad -s $ROOTFS_OFFSET_HEX -l $ROOT_SIZE -q -f /tmp/dcentos_root_readback.uimage $ROOTFS_MTD >/dev/null 2>&1 && sha256sum /tmp/dcentos_root_readback.uimage | awk '{print \$1}'")
scp_get "/tmp/dcentos_root_readback.uimage" "$ARTIFACT_DIR/root_write_readback.uimage"
ssh_run "rm -f /tmp/dcentos_root_readback.uimage"
LOCAL_READBACK_SHA=$(local_sha256 "$ARTIFACT_DIR/root_write_readback.uimage")
[ "$READBACK_SHA" = "$ROOT_SHA" ] && [ "$LOCAL_READBACK_SHA" = "$ROOT_SHA" ] || {
    log "ERROR: rootfs readback SHA mismatch: expected $ROOT_SHA got remote $READBACK_SHA local $LOCAL_READBACK_SHA"
    log "Backup is retained at $ARTIFACT_DIR; firstboot was not set and reboot was not triggered."
    exit 1
}
log "  rootfs readback verified: $READBACK_SHA"

# --- Step 10: fw_setenv + reboot -----------------------------------------
log "Step 10/10: fw_setenv firstboot=1; sync; reboot"
ssh_run 'fw_setenv firstboot 1' || { log "ERROR: fw_setenv failed"; exit 1; }
FIRSTBOOT_AFTER_SET=$(ssh_run "fw_printenv firstboot 2>/dev/null | cut -d= -f2")
[ "$FIRSTBOOT_AFTER_SET" = "1" ] || {
    log "ERROR: fw_setenv firstboot verification failed: firstboot=$FIRSTBOOT_AFTER_SET"
    log "Backup is retained at $ARTIFACT_DIR; reboot was not triggered."
    exit 1
}
log "  firstboot verified: $FIRSTBOOT_AFTER_SET"
cat > "$ARTIFACT_DIR/install_preflight_manifest.json" <<EOF
{
  "stage": "firstboot_verified",
  "miner_ip": "$MINER_IP",
  "platform": "$PLATFORM",
  "rootfs_mtd": "$ROOTFS_MTD",
  "rootfs_offset": "$ROOTFS_OFFSET_HEX",
  "rootfs_window": "$ROOTFS_WINDOW_HEX",
  "mtd5_name": "$MTD5_NAME",
  "mtd5_size": $MTD5_SIZE,
  "mtd5_erasesize": $MTD5_ERASESIZE,
  "nand_env_size": $NAND_ENV_SIZE,
  "nand_env_sha256": "$NAND_ENV_LOCAL_SHA",
  "mtd5_backup_size": $MTD5_BACKUP_SIZE,
  "mtd5_backup_sha256": "$MTD5_LOCAL_SHA",
  "firmware_sha256": "$LOCAL_SHA",
  "remote_firmware_sha256": "$REMOTE_SHA",
  "package_board": "$BOARD_PKG_NAME",
  "root_payload_size": $ROOT_SIZE,
  "root_payload_sha256": "$ROOT_SHA",
  "kernel_payload_size": $KERNEL_SIZE,
  "kernel_payload_sha256": "$KERNEL_SHA",
  "root_write_readback_sha256": "$LOCAL_READBACK_SHA",
  "root_write_readback_artifact": "root_write_readback.uimage",
  "firstboot_after_set": "$FIRSTBOOT_AFTER_SET",
  "dry_run": $DRY_RUN
}
EOF
log "  recovery manifest updated with write/readback/firstboot proof"
ssh_run 'sync; (sleep 2 && reboot) >/dev/null 2>&1 &' || true

log ""
log "=== INSTALL TRIGGERED ==="
log "Reboot fired. Operator monitoring (run from host):"
log ""
log "  # Wait for SSH (90s typical):"
log "  while ! ssh -o ConnectTimeout=3 root@$MINER_IP echo ok 2>/dev/null; do sleep 5; done"
log ""
log "  # Verify state:"
log "  dcent detect $MINER_IP                        # expect DCENTOS / am3-aml-s19k"
log "  curl http://$MINER_IP:8080/api/status         # dcentrald API"
log "  curl http://$MINER_IP/                        # dashboard"
log ""
log "  # MCP via tunnel:"
log "  ssh -L 3000:127.0.0.1:3000 root@$MINER_IP -f -N"
log "  curl -s -X POST http://127.0.0.1:3000/mcp \\"
log "       -H 'Content-Type: application/json' \\"
log "       -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}'"
log ""
log "  # Verify recovery flag committed:"
log "  ssh root@$MINER_IP 'fw_printenv firstboot'    # expect firstboot=0"
log ""
log "Backup retained at: $ARTIFACT_DIR"
log "If install fails: power-cycle .78, U-Boot reverts to mtd2 stock_system."
