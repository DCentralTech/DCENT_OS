#!/bin/sh
#
# safe_sysupgrade_cv_emmc.sh — DCENT_OS eMMC sysupgrade for cv1835-s19jpro.
#
# Status: runtime-only / no fleet unit. This script REFUSES to write eMMC
# unless DCENT_CV1835_EMMC_PROVEN=1 is set in the calling environment, OR
# --dry-run is passed. The override gate exists so the code can land,
# parse correctly, and be reviewed without a hardware unit, while keeping
# every destructive code path locked behind a deliberate operator opt-in.
#
# eMMC partition map (CORRECTED 2026-07-02 from held VNish unlock gpt.img +
# stock CVCtrl updateporc.sh —
# CORPUS_MINING_FINDINGS.md cv1835[2]/[3]):
#   p1 = kernel  (GPT LBA 8192, 16 MiB)   <- stock: dd boot.emmc  of=/dev/mmcblk0p1
#   p2 = marker  (LBA 40960, 1 MiB, EMPTY)
#   p3 = minerfs/rootfs (LBA 43008, 32 MiB) <- stock: dd minerfs.gz of=/dev/mmcblk0p3
#   p4 = sig     (LBA 108032, 2 MiB, RSA pubkey/sig)
#   p5=/config p6=/miner p7=/nvdata (1 GiB)
# The earlier "p1=u-boot, p2=kernel, p3=rootfs" assumption was a PHANTOM
# (invented a user-area u-boot partition, shifting everything by one). Writing
# a ~4.6 MB kernel to p2 (the 1 MiB marker) would truncate/overflow = UNBOOTABLE.
# This was never live-caught because cv1835 has no bench unit; corpus mining
# caught it before first flash. A partition-size fit guard now makes the class
# of off-by-one impossible.
#
# Mirrors the dev-kit DOCS/multi_platform_master.md §9 reference flow:
#   1. SHA256 verify upgrade tar contents.
#   2. Backup mmcblk0p1 (kernel) + mmcblk0p3 (rootfs) to /tmp.
#   3. Pre-write CRC check on new payloads.
#   4. dd new kernel + rootfs into the partitions, with conv=fsync.
#   5. Readback CRC verify after each write — auto-restore from backup
#      on mismatch.
#   6. Increment dcent_boot_count via fw_setenv (NOT manual eMMC writes).
#   7. sync + reboot.
#
# Userspace recovery is deliberately MINIMAL — actual auto-revert lives
# in U-Boot (uboot-bootcmd.txt). Userspace only flips the staging flags;
# if the new image fails to come up, U-Boot dd's
# /config/factory_kernel.bin into mmcblk0p1 (kernel) and reboots.
#
# Why this doesn't reuse safe_sysupgrade_*.sh from am2/am3:
#   - am2 / am3-aml / am1-s9 all target raw NAND mtd partitions. CV1835
#     has eMMC, no MTD. Different syscalls, different recovery semantics.
#   - The auto-revert path is U-Boot-side (see uboot-bootcmd.txt), not
#     userspace.
#     the am2 stage-flag model has a known foot-gun; we deliberately
#     pick a different model here.

set -eu

DRY_RUN=0
UPGRADE_TAR=""
BOARD_TARGET="cv1835-s19jpro"

# p1 = kernel, p3 = minerfs/rootfs (see the corrected partition map above).
KERNEL_DEV="/dev/mmcblk0p1"
ROOTFS_DEV="/dev/mmcblk0p3"
BACKUP_DIR="/tmp/dcent-emmc-backup"
STAGE_DIR="/tmp/dcent-emmc-stage"

usage() {
    cat <<EOF
Usage: $(basename "$0") [--dry-run] <upgrade-tar>

Required: <upgrade-tar> path to dcentos-sysupgrade-cv1835-s19jpro.tar
          (produced by post-image.sh).

Options:
  --dry-run     Verify + print actions without writing eMMC.
  -h, --help    Show this help.

Safety gates:
  - DCENT_CV1835_EMMC_PROVEN=1 is REQUIRED for live runs. Without it the
    script aborts with a status message even if --dry-run is not set.
  - /etc/dcentos/board_target MUST equal "${BOARD_TARGET}".
  - The upgrade tar must contain MANIFEST.json with matching board_target.
  - Per-payload SHA256 must match before any dd is run.
  - Each dd is followed by a readback SHA256 verify; mismatch auto-restores
    from /tmp backup.
EOF
}

log()   { echo "[$(date '+%H:%M:%S')] $*"; }
err()   { echo "[$(date '+%H:%M:%S')] ERROR: $*" >&2; }
warn()  { echo "[$(date '+%H:%M:%S')] WARN: $*" >&2; }

# === CE-091 + CE-287: pre-extraction validation + signature policy ============
# The upgrade tar is attacker-controlled. Before touching disk we validate its
# members (paths/types) and bound its size + /tmp free space; before any
# backup/write (or a --dry-run "valid" verdict) we require + verify an Ed25519
# MANIFEST.sig against the PINNED /etc/dcentos/release_ed25519.pub. This mirrors
# the zynq/am2 on-target sysupgrade template exactly; the CV1835 eMMC path was
# the outlier that extracted the tar with zero pre-extraction validation and did
# no signature verification (only SHA sidecars carried inside the same tar).

# The producer post-image.sh packs exactly this top-level dir name inside the
# tar (dcentos-<board>-sysupgrade). Pin it: a mismatched/extra top-level is a
# validation failure, not a silently-accepted first directory.
CV1835_SYSUPGRADE_TOPLEVEL="dcentos-cv1835-s19jpro-sysupgrade"

# Pre-extraction package-size ceiling, derived (NOT invented) from the eMMC
# partition-map header above: p1 kernel = 16 MiB, p3 minerfs/rootfs = 32 MiB,
# plus the same 8 MiB slack the zynq template uses.
CV1835_KERNEL_PART_BYTES=$((16 * 1024 * 1024))
CV1835_ROOTFS_PART_BYTES=$((32 * 1024 * 1024))
CV1835_SYSUPGRADE_TAR_SLACK_BYTES=$((8 * 1024 * 1024))

# Same release/lab status semantics as the zynq/am2 sysupgrade: only these three
# statuses are treated as "release"; everything else is a lab value for which
# the unsigned lab escape may apply. The CV1835 manifest status
# ("runtime-only-no-fleet-unit") is non-release, so it qualifies as lab.
is_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

# Reject unsafe tar member paths (empty, absolute, or containing a `..`
# component), any member whose top-level dir is not the pinned name, and any
# member type other than `-` (file) or `d` (dir) — which kills symlink /
# hardlink / device members before extraction. Ported from the zynq template.
validate_sysupgrade_tar_members() {
    tarball=$1
    tar tf "$tarball" | awk -v top="$CV1835_SYSUPGRADE_TOPLEVEL" '
        BEGIN { ok=1 }
        {
            name=$0
            if (name == "" || name ~ /^\// || name ~ /(^|\/)\.\.(\/|$)/) {
                printf "ERROR: unsafe tar member path: %s\n", name > "/dev/stderr"
                ok=0
            }
            if (name !~ "^" top "(/|$)") {
                printf "ERROR: unexpected tar top-level path: %s\n", name > "/dev/stderr"
                ok=0
            }
        }
        END { exit ok ? 0 : 1 }
    ' || return 1

    tar tvf "$tarball" | awk '
        BEGIN { ok=1 }
        {
            type=substr($0, 1, 1)
            if (type != "-" && type != "d") {
                printf "ERROR: unsafe tar member type: %s\n", $0 > "/dev/stderr"
                ok=0
            }
        }
        END { exit ok ? 0 : 1 }
    ' || return 1
    return 0
}

# Bound the package size below the kernel+rootfs partition ceiling and confirm
# /tmp has room, refusing BEFORE tar extraction. Ported from the zynq template.
validate_sysupgrade_tar_preextract() {
    tarball=$1
    package_size=$(wc -c < "$tarball" | tr -d '[:space:]')
    case "$package_size" in ''|*[!0-9]*|0) err "cannot validate CV1835 sysupgrade package size (size=$package_size)"; return 1 ;; esac
    ceiling=$((CV1835_KERNEL_PART_BYTES + CV1835_ROOTFS_PART_BYTES + CV1835_SYSUPGRADE_TAR_SLACK_BYTES))
    if [ "$package_size" -gt "$ceiling" ]; then
        err "CV1835 sysupgrade package exceeds pre-extraction ceiling ($package_size bytes > $ceiling bytes)."
        err "Refusing before tar extraction."
        return 1
    fi

    tmp_avail_kb=$(df -Pk /tmp 2>/dev/null | awk 'NR==2 {print $4}')
    case "$tmp_avail_kb" in ''|*[!0-9]*|0) err "cannot validate /tmp free space before tar extraction"; return 1 ;; esac
    tmp_avail_bytes=$((tmp_avail_kb * 1024))
    tmp_required=$((package_size + CV1835_SYSUPGRADE_TAR_SLACK_BYTES))
    if [ "$tmp_avail_bytes" -lt "$tmp_required" ]; then
        err "/tmp has insufficient free space for CV1835 sysupgrade package extraction ($tmp_avail_bytes bytes available < $tmp_required bytes required)."
        err "Refusing before tar extraction."
        return 1
    fi

    log "OK CV1835 sysupgrade package pre-extract size $package_size <= $ceiling bytes; /tmp free $tmp_avail_bytes bytes"
    return 0
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; UPGRADE_TAR="${1:-}"; break ;;
        *) UPGRADE_TAR="$1" ;;
    esac
    shift || true
done

if [ -z "$UPGRADE_TAR" ]; then
    usage
    exit 1
fi

if [ ! -f "$UPGRADE_TAR" ]; then
    err "upgrade tar not found: $UPGRADE_TAR"
    exit 1
fi

# === Safety gate 1: board target match ============================
if [ -f /etc/dcentos/board_target ]; then
    BT=$(cat /etc/dcentos/board_target | tr -d '[:space:]')
    if [ "$BT" != "$BOARD_TARGET" ]; then
        err "running board_target=$BT, expected $BOARD_TARGET. Refusing."
        exit 2
    fi
else
    err "/etc/dcentos/board_target missing — refusing to flash."
    exit 2
fi

# === Safety gate 2: live-proof env variable for destructive runs ===
if [ "$DRY_RUN" -eq 0 ]; then
    if [ "${DCENT_CV1835_EMMC_PROVEN:-}" != "1" ]; then
        err "CV1835 eMMC sysupgrade is gated behind DCENT_CV1835_EMMC_PROVEN=1"
        err "until 3 successful round-trips are proven on a bench unit."
        err "Run with --dry-run to validate the upgrade tar without writing eMMC."
        exit 3
    fi
fi

# === Safety gate 3: factory_kernel.bin present (U-Boot fallback) ====
if [ "$DRY_RUN" -eq 0 ] && [ ! -s /config/factory_kernel.bin ]; then
    err "/config/factory_kernel.bin missing or empty."
    err "U-Boot bootcount recovery cannot restore stock without it."
    err "Refusing to write eMMC."
    exit 4
fi

# === Stage 1: PRE-EXTRACTION tar validation (CE-091/CE-287) =========
# Fail-closed on the attacker-controlled tar BEFORE `tar xf` touches disk.
validate_sysupgrade_tar_members "$UPGRADE_TAR" || exit 14
validate_sysupgrade_tar_preextract "$UPGRADE_TAR" || exit 15

# === Stage 1b: extract upgrade tar to /tmp/dcent-emmc-stage =========
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"
log "Extracting $UPGRADE_TAR -> $STAGE_DIR"
tar xf "$UPGRADE_TAR" -C "$STAGE_DIR"

# Pinned inner staging dir (post-image creates exactly this name). Replaces the
# old `find | head -1` any-directory acceptance — an unexpected/extra top-level
# was already rejected pre-extraction, but this keeps the write path bound to
# the exact validated directory.
INNER="$STAGE_DIR/$CV1835_SYSUPGRADE_TOPLEVEL"
if [ ! -d "$INNER" ]; then
    err "expected staging dir $CV1835_SYSUPGRADE_TOPLEVEL not found after extraction"
    exit 5
fi
cd "$INNER"

# === Stage 2: validate MANIFEST.json + payload SHAs =================
if [ ! -f MANIFEST.json ]; then
    err "MANIFEST.json missing from upgrade tar"
    exit 6
fi
MANIFEST_BT=$(grep -E '"board_target"' MANIFEST.json | head -1 | sed -E 's/.*"board_target"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
if [ "$MANIFEST_BT" != "$BOARD_TARGET" ]; then
    err "MANIFEST.json board_target=$MANIFEST_BT, expected $BOARD_TARGET"
    exit 7
fi

NEW_KERNEL="$INNER/uImage"
NEW_ROOTFS="$INNER/rootfs.gz"

if [ ! -s "$NEW_KERNEL" ]; then
    err "uImage missing from upgrade tar"
    exit 8
fi
if [ ! -s "$NEW_ROOTFS" ]; then
    err "rootfs.gz missing from upgrade tar"
    exit 9
fi

verify_sha() {
    payload="$1"
    expected_file="$2"
    if [ ! -f "$expected_file" ]; then
        err "expected SHA file missing: $expected_file"
        return 1
    fi
    expected=$(cat "$expected_file" | tr -d '[:space:]')
    actual=$(sha256sum "$payload" | awk '{print $1}')
    if [ "$expected" != "$actual" ]; then
        err "SHA mismatch on $payload: expected=$expected actual=$actual"
        return 1
    fi
    log "OK SHA256 $payload"
    return 0
}

verify_sha "$NEW_KERNEL" "${NEW_KERNEL}.sha256" || exit 10
verify_sha "$NEW_ROOTFS" "${NEW_ROOTFS}.sha256" || exit 11

# === Stage 2.5: MANDATORY Ed25519 release-signature verify (CE-091/CE-287) ===
# The SHA sidecars above are ADDITIONAL integrity checks, NOT a trust anchor:
# they travel inside the same attacker-controlled tar. Require + verify a real
# Ed25519 MANIFEST.sig against the PINNED /etc/dcentos/release_ed25519.pub
# before any backup/write AND before a --dry-run declares the package valid.
# Lab escape (DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1) is honored ONLY for a
# non-release manifest status, reusing the shared is_release_status semantics.
RELEASE_PUBKEY="/etc/dcentos/release_ed25519.pub"
MANIFEST_STATUS=$(grep -E '"status"' MANIFEST.json | head -1 | sed -E 's/.*"status"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
[ -n "$MANIFEST_STATUS" ] || MANIFEST_STATUS=release

if [ ! -f MANIFEST.sig ]; then
    if [ "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}" = "1" ] && ! is_release_status "$MANIFEST_STATUS"; then
        warn "MANIFEST.sig absent — unsigned package allowed by the explicit lab override (status=$MANIFEST_STATUS, non-release lab)"
    else
        err "MANIFEST.sig missing — refusing unsigned CV1835 eMMC sysupgrade (fail-closed)."
        err "A non-release manifest status plus the unsigned lab override is required for controlled lab recovery."
        exit 16
    fi
else
    # Signature present: verify it fail-closed against the pinned release key.
    if [ ! -f release_ed25519.pub ]; then
        err "signed package is missing release_ed25519.pub — refusing."
        exit 17
    fi
    if [ ! -f "$RELEASE_PUBKEY" ]; then
        err "pinned release key $RELEASE_PUBKEY missing — refusing to verify against an absent trust anchor."
        exit 18
    fi
    if ! command -v openssl > /dev/null 2>&1; then
        err "openssl missing — cannot verify MANIFEST.sig, refusing (fail-closed)."
        exit 19
    fi
    PKG_KEY_SHA=$(sha256sum release_ed25519.pub | awk '{print $1}')
    PINNED_KEY_SHA=$(sha256sum "$RELEASE_PUBKEY" | awk '{print $1}')
    if [ "$PKG_KEY_SHA" != "$PINNED_KEY_SHA" ]; then
        err "package release_ed25519.pub does not match pinned $RELEASE_PUBKEY — refusing."
        exit 20
    fi
    if ! openssl pkeyutl -verify -rawin -pubin -inkey "$RELEASE_PUBKEY" -sigfile MANIFEST.sig -in MANIFEST.json > /dev/null 2>&1; then
        err "MANIFEST.sig verification FAILED against pinned $RELEASE_PUBKEY — refusing."
        exit 21
    fi
    log "OK MANIFEST.sig verified against pinned $RELEASE_PUBKEY"
fi

# === Stage 3: backup running kernel + rootfs ========================
mkdir -p "$BACKUP_DIR"
log "Backing up $KERNEL_DEV -> $BACKUP_DIR/kernel.bin"
[ "$DRY_RUN" -eq 0 ] && dd if="$KERNEL_DEV" of="$BACKUP_DIR/kernel.bin" bs=1M 2>/dev/null
log "Backing up $ROOTFS_DEV -> $BACKUP_DIR/rootfs.bin"
[ "$DRY_RUN" -eq 0 ] && dd if="$ROOTFS_DEV" of="$BACKUP_DIR/rootfs.bin" bs=1M 2>/dev/null

if [ "$DRY_RUN" -eq 0 ]; then
    BK_KERNEL_SHA=$(sha256sum "$BACKUP_DIR/kernel.bin" | awk '{print $1}')
    BK_ROOTFS_SHA=$(sha256sum "$BACKUP_DIR/rootfs.bin" | awk '{print $1}')
    log "Backup kernel SHA256: $BK_KERNEL_SHA"
    log "Backup rootfs SHA256: $BK_ROOTFS_SHA"
fi

# === Stage 4: write kernel + readback verify ========================
write_and_verify() {
    src="$1"
    dev="$2"
    expected_sha=$(cat "${src}.sha256" | tr -d '[:space:]')
    label="$3"

    log "Writing $src -> $dev (${label})"

    # === Corruption-prevention guard: payload MUST fit the target partition ===
    # Directly prevents the off-by-one brick (a ~4.6 MB kernel dd'd into the
    # 1 MiB marker partition). POSIX/busybox: blockdev --getsize64.
    SRC_BYTES=$(stat -c%s "$src")
    PART_BYTES=$(blockdev --getsize64 "$dev" 2>/dev/null || echo "")
    if [ -n "$PART_BYTES" ]; then
        if [ "$SRC_BYTES" -gt "$PART_BYTES" ]; then
            err "${label}: payload ${SRC_BYTES}B does NOT fit target $dev (${PART_BYTES}B)."
            err "This is the p1(kernel)/p2(marker) off-by-one guard — refusing to write."
            return 1
        fi
    elif [ "$DRY_RUN" -eq 0 ]; then
        # LIVE run and we cannot read the partition size: fail-closed rather
        # than risk a truncating write into the wrong/absent partition.
        err "${label}: cannot read partition size of $dev — refusing to write (fail-closed)."
        return 1
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        if [ -n "$PART_BYTES" ]; then
            log "  [dry-run] would dd if=$src of=$dev bs=1M conv=fsync (fits: ${SRC_BYTES}B <= ${PART_BYTES}B)"
        else
            log "  [dry-run] would dd if=$src of=$dev bs=1M conv=fsync (fit unverified: $dev not present off-target)"
        fi
        return 0
    fi

    dd if="$src" of="$dev" bs=1M conv=fsync 2>/dev/null
    sync

    SRC_SIZE=$(stat -c%s "$src")
    READBACK_SHA=$(dd if="$dev" bs=1M count=$(( (SRC_SIZE + 1048575) / 1048576 )) 2>/dev/null \
                  | head -c "$SRC_SIZE" \
                  | sha256sum \
                  | awk '{print $1}')

    if [ "$READBACK_SHA" != "$expected_sha" ]; then
        err "${label} readback SHA mismatch: expected=$expected_sha actual=$READBACK_SHA"
        err "Restoring from backup..."
        dd if="$BACKUP_DIR/kernel.bin" of="$KERNEL_DEV" bs=1M conv=fsync 2>/dev/null
        dd if="$BACKUP_DIR/rootfs.bin" of="$ROOTFS_DEV" bs=1M conv=fsync 2>/dev/null
        sync
        return 1
    fi

    log "OK ${label} readback verified"
    return 0
}

write_and_verify "$NEW_KERNEL" "$KERNEL_DEV" "kernel" || exit 12
write_and_verify "$NEW_ROOTFS" "$ROOTFS_DEV" "rootfs" || exit 13

# === Stage 5: arm boot count + reboot ===============================
if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] would: fw_setenv dcent_boot_count 0 && reboot -f"
    log "[dry-run] complete. eMMC NOT touched."
    exit 0
fi

# Reset the U-Boot bootcount so a clean boot doesn't trigger factory revert.
if command -v fw_setenv > /dev/null 2>&1; then
    log "Setting dcent_boot_count=0 via fw_setenv"
    fw_setenv dcent_boot_count 0 || warn "fw_setenv failed — U-Boot env may be stale"
else
    warn "fw_setenv not present; U-Boot bootcount unchanged"
fi

sync
log "sysupgrade complete. Rebooting in 3s..."
sleep 3
reboot -f
