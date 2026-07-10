#!/usr/bin/env bash
#
# build_am3_bb_sd_resident_uboot_image.sh - Build an AM3-BB S19j Pro SD image
# that follows the VNish/Bitmain BB recovery contract.
#
# This is deliberately different from a full BootROM/SPL replacement card:
#   - partition 1 starts at sector 1, matching VNish BB SD images;
#   - MLO and u-boot.img are NOT copied to the FAT filesystem;
#   - resident NAND SPL/U-Boot imports uEnv.txt from SD and boots the payload;
#   - the initramfs is a U-Boot legacy ramdisk, named uramdisk.image.gz to
#     match the stock/LuxOS uEnv contract.
#
# The script writes only a regular .img file. Flashing physical media remains
# an explicit operator action.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/sd_common.sh
. "$SCRIPT_DIR/lib/sd_common.sh"
BUILDROOT_OUTPUT="${BUILDROOT_OUTPUT:-$PROJECT_DIR/buildroot/output/images}"
SD_OUTPUT_DIR="$BUILDROOT_OUTPUT/sd_card_am3_bb_s19jpro"
ARTIFACT_DIR=""
PAYLOAD_TAR=""
PAYLOAD_DIR=""
DISK_IMAGE_SIZE_MB=128
# SD-boot-defect fix 2026-06-10. The captured `a lab unit` NAND boot artifacts are
# PROVABLY WRONG for a bootable card — `uImage` legacy header reads
# "Linux-3.8.13+" (the stale mtd7 Bitmain factory kernel) while the live mining
# kernel is 5.4.242-bone66 (output/am3-bb-s19jpro-boot-artifacts-*/uname.txt),
# and the DTB is generic "am335x-bone/beaglebone" with NO S19J_IO_BOARD/btm
# marker. A card built from those panics (wrong kernel as PID-1 host) or boots
# with dead hashboards (wrong pinmux/gpio/i2c).
#
# Two gates, deliberately split:
#   * DTB gate (DEFAULT-ON): a generic BeagleBone DTB is an UNAMBIGUOUS boot
#     defect — no real S19j Pro BB miner firmware ever ships without the carrier
#     pinmux. Hard-fail by default. This blocks the captured-stale mistake while
#     never false-failing a legitimate proof recipe (VNish/LuxOS DTBs carry the
#     btm/S19J_IO_BOARD marker).
#   * Kernel-version gate (OPT-IN via --strict-kernel): the "Linux-3.8.13+"
#     string is ambiguous — a vendor (VNish) BB kernel could legitimately be
#     3.8.x-based — so it stays a warning unless explicitly requested.
#   * --allow-stale-kernel: master RE/diagnostic opt-out; downgrades BOTH gates
#     to warnings.
STRICT_KERNEL=0
ALLOW_STALE_KERNEL=0
BOOT_LABEL="${BOOT_LABEL:-DCENTOS}"
EXTRA_DTB_PATHS=()
# Use the library's shared cleanup array under a local-friendly alias so
# inline `CLEANUP_PATHS+=("$tmp")` continues to work. The library owns the
# EXIT/INT/TERM trap that walks the array.
declare -n CLEANUP_PATHS=SD_COMMON_CLEANUP_PATHS
sd_common::install_cleanup_trap

usage() {
    cat <<EOF
Usage: $(basename "$0") [--payload-tar <tar>|--payload-dir <dir>] --artifacts <dir> [--size-mb N]

Options:
  --artifacts <dir>   Directory containing uImage and DTB. MLO/u-boot.img are
                      intentionally ignored by this resident-U-Boot layout.
  --payload-tar <tar> Rootfs payload tar, e.g. dcentos-am3-bb-s19jpro-sdcard.tar.
  --payload-dir <dir> Directory containing uramdisk.image.gz and optionally
                      ramdisk.gz. ramdisk.gz must be a U-Boot legacy ramdisk
                      if present.
  --size-mb N         Total .img size (default: ${DISK_IMAGE_SIZE_MB}, auto-grown).
  --strict-kernel     ALSO abort if uImage reports the stale Bitmain factory
                      kernel (Linux-3.8.13+). The generic-DTB gate is already
                      default-on (see below); this adds the (opt-in, ambiguous)
                      kernel-version gate on top.
  --allow-stale-kernel
                      Master RE/diagnostic opt-out: downgrade BOTH the
                      default-on generic-DTB gate and the kernel-version gate to
                      loud warnings. A card built this way will NOT boot DCENT_OS
                      on real hardware if it carries a generic BeagleBone DTB
                      (wrong hashboard pinmux) or the Linux-3.8.13+ mtd7 factory
                      kernel.

NOTE: by DEFAULT this builder HARD-REFUSES a generic ti,beaglebone-black DTB
that lacks the S19J_IO_BOARD/btm carrier marker — it guarantees wrong
hashboard pinmux/gpio/i2c. Supply the live LuxOS 5.4 carrier-aware DTB.
  --label <name>      FAT volume label for p1 (default: ${BOOT_LABEL}). Use
                      ANTHILLOS only when reproducing VNish-style media exactly.
  --extra-dtb <path>  Additional DTB to copy onto the SD as a fallback file
                      (renamed by basename). May be passed multiple times.
                      Useful for multi-firmware-ready cards that target more
                      than one S19j Pro BB carrier revision.
  -h, --help          Show this help.

Output: $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-resident-uboot.img
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --artifacts)
            ARTIFACT_DIR="${2:?--artifacts requires a path}"
            shift 2
            ;;
        --artifacts=*)
            ARTIFACT_DIR="${1#--artifacts=}"
            shift
            ;;
        --payload-tar)
            PAYLOAD_TAR="${2:?--payload-tar requires a path}"
            shift 2
            ;;
        --payload-tar=*)
            PAYLOAD_TAR="${1#--payload-tar=}"
            shift
            ;;
        --payload-dir)
            PAYLOAD_DIR="${2:?--payload-dir requires a path}"
            shift 2
            ;;
        --payload-dir=*)
            PAYLOAD_DIR="${1#--payload-dir=}"
            shift
            ;;
        --size-mb)
            DISK_IMAGE_SIZE_MB="${2:?--size-mb requires N}"
            shift 2
            ;;
        --size-mb=*)
            DISK_IMAGE_SIZE_MB="${1#--size-mb=}"
            shift
            ;;
        --strict-kernel)
            STRICT_KERNEL=1
            shift
            ;;
        --allow-stale-kernel)
            ALLOW_STALE_KERNEL=1
            shift
            ;;
        --label)
            BOOT_LABEL="${2:?--label requires a FAT label}"
            shift 2
            ;;
        --label=*)
            BOOT_LABEL="${1#--label=}"
            shift
            ;;
        --extra-dtb)
            EXTRA_DTB_PATHS+=("${2:?--extra-dtb requires a path}")
            shift 2
            ;;
        --extra-dtb=*)
            EXTRA_DTB_PATHS+=("${1#--extra-dtb=}")
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

sd_common::validate_fat_label BOOT_LABEL
sd_common::refuse_block_device "$SD_OUTPUT_DIR"

mkdir -p "$SD_OUTPUT_DIR"

# Thin shim wrappers around sd_common::* — preserved so existing inline
# callers continue to work without churning every call site.
need_tool() { sd_common::need_tool "$@"; }
total_bytes() { sd_common::total_bytes "$@"; }
sha256_file() { sd_common::sha256_file "$@"; }

validate_armv7_rootfs_cpio() {
    local cpio_gz="$1"
    local cpio_abs
    local check_dir
    case "$cpio_gz" in
        /*) cpio_abs="$cpio_gz" ;;
        *) cpio_abs="$(cd "$(dirname "$cpio_gz")" && pwd)/$(basename "$cpio_gz")" ;;
    esac
    check_dir="$(mktemp -d)"
    CLEANUP_PATHS+=("$check_dir")

    (cd "$check_dir" && gzip -dc "$cpio_abs" | cpio -idmu --quiet)

    require_armv7_elf() {
        local rel="$1"
        local path="$check_dir/$rel"
        local desc
        if [ ! -e "$path" ]; then
            echo "ERROR: missing $rel in AM3-BB rootfs" >&2
            exit 1
        fi
        desc="$(file -Lb "$path" 2>/dev/null || true)"
        case "$desc" in
            *"ELF 32-bit LSB"*ARM*EABI5*) return 0 ;;
        esac
        echo "ERROR: $rel must be ARMv7/EABI5 ELF; got: ${desc:-unknown file type}" >&2
        exit 1
    }

    require_armv7_elf "bin/busybox"
    require_armv7_elf "sbin/init"
    require_armv7_elf "usr/sbin/dropbear"
    require_armv7_elf "usr/local/bin/dcentrald"
    require_armv7_elf "usr/local/bin/dcentos-discovery"

    local bad_elf
    bad_elf="$(find "$check_dir" -type f -exec file {} + | grep -E "ELF 64-bit|aarch64|x86-64" || true)"
    if [ -n "$bad_elf" ]; then
        echo "ERROR: AM3-BB rootfs contains non-ARMv7 ELF payloads:" >&2
        printf "%s\n" "$bad_elf" >&2
        exit 1
    fi

    echo "Validated rootfs critical ELFs: ARMv7/EABI5"
}

ROOTFS_CPIO=""
RAMDISK_UIMAGE_SRC=""
if [ -n "$PAYLOAD_TAR" ]; then
    [ -f "$PAYLOAD_TAR" ] || {
        echo "ERROR: payload tar not found: $PAYLOAD_TAR" >&2
        exit 1
    }
    PAYLOAD_TMP="$(mktemp -d)"
    CLEANUP_PATHS+=("$PAYLOAD_TMP")
    tar -xf "$PAYLOAD_TAR" -C "$PAYLOAD_TMP"
    ROOTFS_CPIO="$(find "$PAYLOAD_TMP" -type f -name uramdisk.image.gz | head -1)"
    RAMDISK_UIMAGE_SRC="$(find "$PAYLOAD_TMP" -type f -name ramdisk.gz | head -1)"
elif [ -n "$PAYLOAD_DIR" ]; then
    for candidate in \
        "$PAYLOAD_DIR/uramdisk.image.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-s19jpro-sdcard/uramdisk.image.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-sdcard/uramdisk.image.gz"
    do
        if [ -f "$candidate" ]; then
            ROOTFS_CPIO="$candidate"
            break
        fi
    done
    for candidate in \
        "$PAYLOAD_DIR/ramdisk.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-s19jpro-sdcard/ramdisk.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-sdcard/ramdisk.gz"
    do
        if [ -f "$candidate" ]; then
            RAMDISK_UIMAGE_SRC="$candidate"
            break
        fi
    done
else
    for candidate in \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-s19jpro-sdcard/uramdisk.image.gz" \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-sdcard/uramdisk.image.gz" \
        "$BUILDROOT_OUTPUT/rootfs.cpio.gz"
    do
        if [ -f "$candidate" ]; then
            ROOTFS_CPIO="$candidate"
            break
        fi
    done
    for candidate in \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-s19jpro-sdcard/ramdisk.gz" \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-sdcard/ramdisk.gz"
    do
        if [ -f "$candidate" ]; then
            RAMDISK_UIMAGE_SRC="$candidate"
            break
        fi
    done
fi

if [ -z "$ROOTFS_CPIO" ]; then
    echo "ERROR: no DCENT_OS AM3-BB rootfs CPIO found." >&2
    echo "       Run Buildroot first, or pass --payload-tar / --payload-dir." >&2
    exit 1
fi

[ -n "$ARTIFACT_DIR" ] || {
    echo "ERROR: --artifacts is required for uImage + DTB provenance" >&2
    exit 1
}
[ -d "$ARTIFACT_DIR" ] || {
    echo "ERROR: artifact directory not found: $ARTIFACT_DIR" >&2
    exit 1
}

UIMAGE_SRC=""
DTB_SRC=""
[ -f "$ARTIFACT_DIR/uImage" ] && UIMAGE_SRC="$ARTIFACT_DIR/uImage"
for dtb in devicetree.dtb am335x-s19jpro.dtb bitmain-am335x.dtb am335x-boneblack.dtb dtb; do
    if [ -f "$ARTIFACT_DIR/$dtb" ]; then
        DTB_SRC="$ARTIFACT_DIR/$dtb"
        break
    fi
done

missing=""
[ -n "$UIMAGE_SRC" ] || missing="$missing uImage"
[ -n "$DTB_SRC" ] || missing="$missing DTB"
if [ -n "$missing" ]; then
    echo "ERROR: missing required resident-U-Boot artifact(s):$missing" >&2
    exit 1
fi

sd_common::check_host_tools_basic

UIMAGE_INFO="$(mkimage -l "$UIMAGE_SRC" 2>/dev/null || true)"
if [ -n "$UIMAGE_INFO" ]; then
    printf "%s\n" "$UIMAGE_INFO" | sed 's/^/[INFO] kernel: /'
fi
KERNEL_STALE=0
if printf "%s\n" "$UIMAGE_INFO" | grep -q 'Linux-3\.8\.13'; then
    KERNEL_STALE=1
    echo "[WARN] uImage reports Linux-3.8.13+; live LuxOS .79 ran Linux 5.4.242 bone66." >&2
    echo "[WARN] The mtd7 NAND kernel slot on LuxOS units holds a stale Bitmain factory kernel." >&2
    echo "[WARN] The active 5.4 kernel lives elsewhere on LuxOS (likely /boot or mtd11 nvdata)." >&2
fi

DTB_STALE=0
if grep -a -q 'am335x-boneblack-btm' "$DTB_SRC" || grep -a -q 'S19J_IO_BOARD' "$DTB_SRC"; then
    echo "[INFO] DTB provenance: Bitmain/BTM BeagleBone-compatible DTB detected"
elif grep -a -q 'am335x-bone' "$DTB_SRC"; then
    DTB_STALE=1
    echo "[WARN] DTB provenance: generic BeagleBone DTB detected, not Bitmain/BTM S19J_IO_BOARD-specific" >&2
    echo "[WARN] Kernel can reach userspace but hashboard pinmux/gpio/i2c nodes will be wrong." >&2
else
    echo "[WARN] DTB provenance: expected AM335x BeagleBone compatibility string not found" >&2
fi

# DEFAULT-ON DTB gate: a generic BeagleBone DTB with no S19J_IO_BOARD/btm
# carrier marker is an unambiguous boot defect (wrong hashboard pinmux/gpio/i2c).
# Hard-refuse unless the operator explicitly opts out for RE work.
if [ "$ALLOW_STALE_KERNEL" != "1" ] && [ "$DTB_STALE" = "1" ]; then
    echo "" >&2
    echo "ERROR: refusing to ship a generic BeagleBone DTB (no S19J_IO_BOARD/btm" >&2
    echo "       carrier marker). It will give the kernel the WRONG hashboard" >&2
    echo "       pinmux/gpio/i2c — the card boots to a dead chain or panics." >&2
    echo "       Capture the live LuxOS 5.4 carrier-aware DTB from a running" >&2
    echo "       .79-class unit (read-only) and pass it via --artifacts:" >&2
    echo "         cp /sys/firmware/fdt -> <artifacts>/devicetree.dtb (via SSH)" >&2
    echo "       For an RE/diagnostic card only, re-run with --allow-stale-kernel." >&2
    exit 1
fi

# OPT-IN kernel-version gate (--strict-kernel): also refuse the stale Bitmain
# 3.8.13 factory kernel. Ambiguous on its own (a vendor BB kernel can be
# 3.8.x), so it is not default-on.
if [ "$STRICT_KERNEL" = "1" ] && [ "$ALLOW_STALE_KERNEL" != "1" ] && [ "$KERNEL_STALE" = "1" ]; then
    echo "" >&2
    echo "ERROR: --strict-kernel: refusing to ship the stale Bitmain factory kernel" >&2
    echo "       (Linux-3.8.13+). Recapture the live LuxOS 5.4 kernel from a running" >&2
    echo "       .79-class unit (read-only) and pass it via --artifacts:" >&2
    echo "         scp root@<luxos-bb>:/boot/uImage <artifacts>/uImage" >&2
    echo "       then re-run with --artifacts <artifacts>." >&2
    exit 1
fi

validate_armv7_rootfs_cpio "$ROOTFS_CPIO"

RAMDISK_UIMAGE_TMP="$(mktemp)"
CLEANUP_PATHS+=("$RAMDISK_UIMAGE_TMP")
if [ -n "$RAMDISK_UIMAGE_SRC" ]; then
    cp "$RAMDISK_UIMAGE_SRC" "$RAMDISK_UIMAGE_TMP"
else
    mkimage \
        -A arm \
        -O linux \
        -T ramdisk \
        -C gzip \
        -n "DCENT_OS am3-bb-s19jpro initramfs" \
        -d "$ROOTFS_CPIO" \
        "$RAMDISK_UIMAGE_TMP" >/dev/null
fi
mkimage -l "$RAMDISK_UIMAGE_TMP" | grep -q 'ARM Linux RAMDisk Image' || {
    echo "ERROR: resident-U-Boot payload must be a U-Boot ARM legacy ramdisk image" >&2
    exit 1
}

IMG_FILE="$SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-resident-uboot.img"
P1_OFFSET_SECTORS=1
P1_OFFSET_BYTES=512
SECTOR_SIZE=512

PAYLOAD_BYTES=0
for f in "$UIMAGE_SRC" "$DTB_SRC" "$RAMDISK_UIMAGE_TMP" "$ROOTFS_CPIO"; do
    PAYLOAD_BYTES=$((PAYLOAD_BYTES + $(total_bytes "$f")))
done
NEEDED_MB=$(( (PAYLOAD_BYTES / 1024 / 1024) + 16 ))
if [ "$NEEDED_MB" -gt "$DISK_IMAGE_SIZE_MB" ]; then
    echo "[INFO] auto-growing disk image to ${NEEDED_MB} MB to fit payload"
    DISK_IMAGE_SIZE_MB="$NEEDED_MB"
fi

echo "=== am3-bb-s19jpro resident-U-Boot SD image ==="
echo "Image:      $IMG_FILE"
echo "Size:       ${DISK_IMAGE_SIZE_MB} MB"
echo "Rootfs:     $ROOTFS_CPIO"
echo "Ramdisk:    $RAMDISK_UIMAGE_TMP"
echo "Artifacts:  $ARTIFACT_DIR"
echo "DTB:        $DTB_SRC"
echo "Layout:     p1 starts at sector 1; no MLO/u-boot.img on SD"
echo "Label:      $BOOT_LABEL"
echo ""

sd_common::create_blank_image "$IMG_FILE" "$DISK_IMAGE_SIZE_MB"

TOTAL_SECTORS=$((DISK_IMAGE_SIZE_MB * 1024 * 1024 / SECTOR_SIZE))
P1_SIZE_SECTORS=$((TOTAL_SECTORS - P1_OFFSET_SECTORS))
sd_common::write_mbr_p1_sector_aligned "$IMG_FILE" "$P1_OFFSET_SECTORS" "$P1_SIZE_SECTORS" "0c"

BOOT_PART_TMP="$(mktemp)"
CLEANUP_PATHS+=("$BOOT_PART_TMP")
sd_common::dd_extract_partition "$IMG_FILE" "$BOOT_PART_TMP" "$P1_OFFSET_SECTORS" "$P1_SIZE_SECTORS"
sd_common::format_fat16_partition "$BOOT_PART_TMP" "$BOOT_LABEL"

export MTOOLS_SKIP_CHECK=1

# Thin shim — kept so existing inline call sites continue to work without
# churning every site. Equivalent to sd_common::copy_one_to_fat.
copy_in() { sd_common::copy_one_to_fat "$@"; }

UENV_TMP="$(mktemp)"
README_TMP="$(mktemp)"
MANIFEST_TMP="$(mktemp)"
CLEANUP_PATHS+=("$UENV_TMP" "$README_TMP" "$MANIFEST_TMP")

#
# Multi-firmware-ready uEnv.txt:
# - Explicit `mmcdev`, `loadaddr`, `fdtaddr`, `initramfsaddr` so this card works
#   on any AM335x BB resident U-Boot regardless of what defaults the resident
#   env happens to ship with (LuxOS, BraiinsOS, VNish, stock-Bitmain BB).
# - Bootargs list both `console=ttyS0,115200n8` (5.4-era 8250-omap driver,
#   what live LuxOS .79 uses) and `console=ttyO0,115200n8` (3.8/3.14-era
#   omap-serial driver, what VNish v1.2.6 and pre-LuxOS stock kernels use).
#   Linux only opens the LAST `console=` for tty1, but emits messages on all
#   listed consoles, so log evidence is captured either way.
# - `panic=0` keeps PID 1 panic from triggering an immediate reboot loop so
#   serial console / status LEDs are observable.
# - The U-Boot 0x88000000 region is intentionally left untouched (it's where
#   VNish loads its `boot.bin` installer payload; if any resident U-Boot does
#   a stale `go 0x88000000` we want random RAM there, not our kernel).
cat > "$UENV_TMP" <<'EOF'
bootfile=uImage
fdtfile=devicetree.dtb
initramfsfile=uramdisk.image.gz
mmcdev=0
mmcpart=1
loadaddr=0x80200000
# pass-2 fix (P2-5): DTB+initramfs were at 0x80F80000/0x81000000 — only ~15.5 MB
# above the 0x80008000 kernel entry, so a ~7.5 MB-compressed 5.x kernel that
# decompresses to ~12-18 MB would overrun the external DTB (the decompressor
# relocates its appended DTB but NOT the external fdt/ramdisk) -> garbage FDT ->
# early panic. Move them HIGH (clear of the decompress window) but below the
# 0x88000000 VNish-boot.bin region: DTB at 0x84000000 (+64 MB), initramfs at
# 0x85000000 (+80 MB; 25.5 MB fits 0x85000000..~0x86900000, < 0x88000000).
fdtaddr=0x84000000
initramfsaddr=0x85000000
bootargs=console=ttyS0,115200n8 console=ttyO0,115200n8 init=/sbin/init panic=0 root=/dev/ram0 rw
loadfdt=load mmc ${mmcdev}:${mmcpart} ${fdtaddr} ${fdtfile}
loadinitramfs=load mmc ${mmcdev}:${mmcpart} ${initramfsaddr} ${initramfsfile}
loaduimage=load mmc ${mmcdev}:${mmcpart} ${loadaddr} ${bootfile}
bootinitramfs=bootm ${loadaddr} ${initramfsaddr} ${fdtaddr}
uenvcmd=echo DCENT_OS SD-first multi-firmware-ready boot; run loaduimage; run loadfdt; run loadinitramfs; run bootinitramfs
EOF

cat > "$README_TMP" <<EOF
DCENT_OS am3-bb-s19jpro resident-U-Boot SD image
Created:  $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Boot:     NAND SPL/U-Boot imports this SD uEnv.txt; no MLO/u-boot.img on SD
Layout:   single active FAT partition starts at sector 1, matching VNish BB SD
Payload:  uImage + devicetree.dtb + U-Boot legacy uramdisk.image.gz
Safety:   no NAND writes; DCENT_OS initramfs only; panic=0 for no-serial debug
EOF

EXTRA_DTB_MANIFEST=""
for extra in "${EXTRA_DTB_PATHS[@]:-}"; do
    [ -n "$extra" ] || continue
    if [ ! -f "$extra" ]; then
        echo "ERROR: --extra-dtb path not found: $extra" >&2
        exit 1
    fi
    EXTRA_DTB_MANIFEST="$EXTRA_DTB_MANIFEST,
    \"$(basename "$extra")\": \"$(sha256_file "$extra")\""
done

cat > "$MANIFEST_TMP" <<EOF
{
  "layout": "am3-bb-s19jpro-resident-uboot-p1-sector1",
  "created_utc": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "image": "$(basename "$IMG_FILE")",
  "image_size_mb": ${DISK_IMAGE_SIZE_MB},
  "strict_kernel": ${STRICT_KERNEL},
  "dtb_gate_bypassed": $([ "$ALLOW_STALE_KERNEL" = "1" ] && echo true || echo false),
  "partition_1": {
    "start_sector": ${P1_OFFSET_SECTORS},
    "type": "0x0c",
    "active": true,
    "filesystem": "FAT16",
    "label": "$BOOT_LABEL"
  },
  "files": {
    "uEnv.txt": "$(sha256_file "$UENV_TMP")",
    "uImage": "$(sha256_file "$UIMAGE_SRC")",
    "devicetree.dtb": "$(sha256_file "$DTB_SRC")",
    "uramdisk.image.gz": "$(sha256_file "$RAMDISK_UIMAGE_TMP")",
    "raw-rootfs.cpio.gz": "$(sha256_file "$ROOTFS_CPIO")"${EXTRA_DTB_MANIFEST}
  }
}
EOF

copy_in "$BOOT_PART_TMP" "$UENV_TMP" "uEnv.txt"
copy_in "$BOOT_PART_TMP" "$UIMAGE_SRC" "uImage"
copy_in "$BOOT_PART_TMP" "$DTB_SRC" "devicetree.dtb"
copy_in "$BOOT_PART_TMP" "$RAMDISK_UIMAGE_TMP" "uramdisk.image.gz"
copy_in "$BOOT_PART_TMP" "$README_TMP" "README.txt"
copy_in "$BOOT_PART_TMP" "$MANIFEST_TMP" "MANIFEST.json"
for extra in "${EXTRA_DTB_PATHS[@]:-}"; do
    [ -n "$extra" ] || continue
    copy_in "$BOOT_PART_TMP" "$extra" "$(basename "$extra")"
done

sd_common::dd_write_partition "$BOOT_PART_TMP" "$IMG_FILE" "$P1_OFFSET_SECTORS"
cp "$MANIFEST_TMP" "$SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-resident-uboot.manifest.json"

IMG_BYTES=$(total_bytes "$IMG_FILE")
IMG_SHA="$(sha256_file "$IMG_FILE")"

echo ""
echo "=== Verification ==="
fdisk -l "$IMG_FILE"
echo ""
mdir -i "$IMG_FILE@@$P1_OFFSET_BYTES" ::
echo ""
mtype -i "$IMG_FILE@@$P1_OFFSET_BYTES" ::uEnv.txt
echo ""
mkimage -l "$RAMDISK_UIMAGE_TMP"

echo ""
echo "=== Done ==="
echo "  Image:    $IMG_FILE ($((IMG_BYTES / 1024 / 1024)) MB)"
echo "  SHA256:   $IMG_SHA"
echo "  Manifest: $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-resident-uboot.manifest.json"
echo "  Flash:    dd if=\"$IMG_FILE\" of=/dev/sdX bs=4M status=progress"
echo "            (or DCENT_OS_Antminer/scripts/write_am3_bb_sd_physical_windows.ps1 as Administrator)"
