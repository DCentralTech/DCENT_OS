#!/bin/sh
# DCENT_OS — back-to-stock uninstall (Amlogic am3-aml)
#
# Reverts an am3-aml unit running DCENT_OS back to the original Bitmain
# stock firmware that was on it before any aftermarket firmware was
# installed. Mirrors the canonical pattern shipped by LuxOS / BraiinsOS+ /
# VNish — every aftermarket Antminer firmware ships an /uninstall.sh and
# operators expect this exact path.
#
# Mechanism (canonical U-Boot recovery trigger):
#   1. Corrupt the CRC32 prefix of /dev/nand_env so U-Boot's env-load
#      check fails on next boot. U-Boot then falls back to its compiled-
#      in default env, whose `bootcmd` boots the stock partition.
#   2. Schedule a hard reboot via /proc/sysrq-trigger on script EXIT
#      (we cannot rely on systemd / init clean shutdown after pivot_root).
#   3. pivot_root to a tmpfs we built under /tmp/chroot, so we can rm -rf
#      the real rootfs without yanking the script's own files out from
#      under it.
#   4. Wipe /mnt/nvdata/* (the DCENT_OS rootfs on UBI mtd6).
#   5. Recreate the Bitmain stock log directories (stock cgminer expects
#      these and otherwise refuses to start cleanly).
#   6. Sync, exit, trap fires, sysrq 'b' triggers an immediate reboot,
#      U-Boot detects the bad-CRC env, falls back to defaults, boots stock.
#
# and
# Phase O.11 in .
# Pattern is firmware-agnostic — works on any U-Boot device because CRC32
# corruption forcing default-env fallback is a U-Boot universal behavior.
#
# Safety: requires explicit `--confirm-uninstall` flag. Without it the
# script prints what it would do and exits 0. Operators / dcent-toolbox
# pass `--confirm-uninstall` only after explicit user authorization.
#
# Usage:
#   /uninstall.sh                       — dry-run (prints actions, no harm)
#   /uninstall.sh --confirm-uninstall   — DESTRUCTIVE — reboots into stock

set -u

PATH=/usr/bin:/bin:/usr/sbin:/sbin

CONFIRM=""
DRY_RUN=1
for arg in "$@"; do
    case "$arg" in
        --confirm-uninstall)
            CONFIRM="$arg"
            DRY_RUN=0
            ;;
        --dry-run|--plan)
            DRY_RUN=1
            ;;
        --help|-h)
            cat <<EOF
DCENT_OS uninstall — revert to stock Bitmain firmware.

Usage:
  $0                       Dry-run; print actions without executing.
  $0 --confirm-uninstall   DESTRUCTIVE. Wipes DCENT_OS rootfs + reboots
                           into stock Bitmain on next boot. Cannot be
                           undone without re-flashing DCENT_OS.

EOF
            exit 0
            ;;
        *)
            echo "ERROR: unknown flag: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# --- Platform detection ---
PLATFORM=""
if [ -f /etc/dcentos-platform ]; then
    PLATFORM=$(cat /etc/dcentos-platform | tr -d '[:space:]')
fi

case "$PLATFORM" in
    am3-aml-s19k|am3-aml-s19xp|am3-aml-s21|am3-aml)
        ENV_DEV="/dev/nand_env"
        ROOTFS_MOUNT="/mnt/nvdata"
        # Stock cgminer expects these dirs to exist on the rootfs partition
        STOCK_LOG_DIRS="/mnt/nvdata/log/debug /mnt/nvdata/log/fatal /mnt/nvdata/log/nonce_num /mnt/nvdata/log/sensor_temp"
        ;;
    "")
        echo "ERROR: /etc/dcentos-platform not present or empty." >&2
        echo "  uninstall.sh cannot determine the safe revert procedure" >&2
        echo "  for this hardware. Refusing to act." >&2
        exit 3
        ;;
    *)
        echo "ERROR: platform '$PLATFORM' is not yet supported by uninstall.sh." >&2
        echo "  Supported platforms: am3-aml-s19k, am3-aml-s19xp, am3-aml-s21, am3-aml" >&2
        echo "  am1-s9 / am2-* uninstall paths still TODO." >&2
        exit 3
        ;;
esac

# --- Logging ---
LOG=/tmp/uninstall.log
mkdir -p "$(dirname "$LOG")" 2>/dev/null || true

log() {
    # POSIX-portable timestamp (busybox printf doesn't support %(...)T)
    printf '%s  %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$LOG" >&2
}

log "DCENT_OS uninstall.sh — platform=$PLATFORM env_dev=$ENV_DEV"

# --- Pre-flight checks ---
if [ ! -e "$ENV_DEV" ]; then
    echo "ERROR: $ENV_DEV does not exist on this device." >&2
    echo "  uninstall.sh cannot corrupt the U-Boot env CRC. Refusing." >&2
    exit 4
fi

if [ ! -d /proc/sysrq-trigger ] && [ ! -e /proc/sysrq-trigger ]; then
    echo "ERROR: /proc/sysrq-trigger not present. Cannot schedule reboot." >&2
    echo "  Kernel was built without CONFIG_MAGIC_SYSRQ." >&2
    exit 5
fi

# --- Dry-run path ---
if [ $DRY_RUN -eq 1 ]; then
    log "DRY-RUN: would corrupt $ENV_DEV CRC32 prefix"
    log "DRY-RUN: would mount tmpfs at /tmp/chroot, copy /bin /lib /usr"
    log "DRY-RUN: would pivot_root, then rm -rf $ROOTFS_MOUNT/*"
    log "DRY-RUN: would recreate Bitmain stock dirs: $STOCK_LOG_DIRS"
    log "DRY-RUN: would sysrq-reboot"
    log "DRY-RUN: pass --confirm-uninstall to actually execute."
    log ""
    log "After confirmed uninstall, the device will boot into stock Bitmain"
    log "on the next start. To re-install DCENT_OS later, use:"
    log "  dcent install <ip> -f dcentos-sysupgrade-${PLATFORM##am3-aml-}.tar"
    exit 0
fi

# --- Confirmed path: DESTRUCTIVE ---
log "CONFIRMED uninstall starting. Next boot will be stock Bitmain."

# Step 1: corrupt the U-Boot env CRC32 (first 4 bytes of /dev/nand_env are
# the little-endian CRC32 of the env body). Writing literal "bad crc"
# overwrites those bytes with 0x62 0x61 0x64 0x20 — guaranteed mismatch.
# The trailing newline is intentional; the LuxOS canonical pattern uses it.
if echo "bad crc" > "$ENV_DEV" ; then
    log "Step 1 OK: $ENV_DEV CRC corrupted"
else
    log "Step 1 FAILED: could not write to $ENV_DEV — aborting (safer to leave running)"
    exit 6
fi

# Step 2: schedule a hard reboot for when this script exits. Once we
# pivot_root + rm -rf the rootfs, normal `reboot` won't work — sysrq 'b'
# is the only reliable path back.
trap 'echo b > /proc/sysrq-trigger' EXIT
log "Step 2 OK: sysrq-reboot trap armed"

# Step 3: build a tmpfs chroot and copy a minimal toolset. After
# pivot_root the script's own /bin /lib /usr will be on the tmpfs,
# leaving the real rootfs (now /mnt/nvdata) safely deletable.
CR=/tmp/chroot
mkdir -p "$CR" || { log "ERROR: cannot mkdir $CR"; exit 7; }
mount -t tmpfs tmpfs "$CR" || { log "ERROR: tmpfs mount failed"; exit 7; }
mkdir -p "$CR/mnt/nvdata" "$CR/dev" "$CR/proc" "$CR/sys" "$CR/tmp/chroot" \
    || { log "ERROR: cannot mkdir under $CR"; exit 7; }

cp -r /bin /lib /usr "$CR/" || {
    log "ERROR: failed to copy toolset to tmpfs"
    exit 7
}

mount --bind /dev "$CR/dev" || true
mount -t devtmpfs devtmpfs "$CR/proc" 2>/dev/null || mount -t proc none "$CR/proc"
mount -t sysfs none "$CR/sys" || true
log "Step 3 OK: tmpfs chroot prepared at $CR"

# Step 4: pivot_root. After this the original rootfs is at /mnt/nvdata.
pivot_root "$CR" "$CR/mnt/nvdata" || {
    log "ERROR: pivot_root failed — leaving env corrupted but rootfs intact"
    log "       reboot will still revert to stock; rootfs wipe skipped"
    exit 0
}
log "Step 4 OK: pivot_root succeeded"

# Step 5: lazy unmount of inherited overlays (these no longer matter once
# we wipe the rootfs they live on, but graceful umount avoids kernel
# warnings).
for m in /mnt/nvdata/var/volatile /mnt/nvdata/run /mnt/nvdata/config \
         /mnt/nvdata/sys /mnt/nvdata/proc /mnt/nvdata/dev /mnt/nvdata/etc \
         /mnt/nvdata/mnt/ramdisk /mnt/nvdata/mnt/overlay /mnt/nvdata/mnt/root; do
    umount -l "$m" 2>/dev/null || true
done
log "Step 5 OK: overlays unmounted"

# Step 6: nuke DCENT_OS rootfs.
rm -rf /mnt/nvdata/* 2>/dev/null
log "Step 6 OK: DCENT_OS rootfs wiped"

# Step 7: recreate Bitmain stock log dirs so stock cgminer doesn't crash
# on first boot looking for them.
for d in $STOCK_LOG_DIRS; do
    mkdir -p "$d"
    chmod 755 "$d"
done
log "Step 7 OK: Bitmain stock log dirs recreated"

sync
log "Step 8: sync complete — exiting (sysrq 'b' will fire from EXIT trap)"

# Falling out of the script triggers the EXIT trap → sysrq 'b' → reboot.
# U-Boot at next start: bad env CRC → default env → boot stock.
exit 0
