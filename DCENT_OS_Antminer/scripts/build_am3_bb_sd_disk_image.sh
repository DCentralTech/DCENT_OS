#!/usr/bin/env bash
#
# build_am3_bb_sd_disk_image.sh - Build a flashable .img for the Antminer
# S19j Pro BeagleBone/AM335x control board (am3-bb-s19jpro).
#
# This script creates a regular disk image with a small BootROM partition and
# a second U-Boot payload partition. The stock AM335x U-Boot environment sets
# bootpart=${mmcdev}:2 before importing uEnv.txt, so the runtime boot files
# must be present on partition 2.
# It never writes to a block device. The physical SD write remains an explicit
# operator action with dd, Rufus, or equivalent tooling.
#
# Inputs:
#   --payload-tar <tar>   DCENT_OS rootfs payload tar, for example:
#                         DCENT_OS_Antminer/output/dcentos-am3-bb-s19jpro-sdcard.tar
#   --payload-dir <dir>   Directory containing uramdisk.image.gz and,
#                         optionally, a pre-wrapped ramdisk.gz.
#   default lookup        Buildroot output images directory.
#
# Required boot artifacts:
#   --artifacts <dir> containing MLO, u-boot.img, uImage, and one DTB
#   named am335x-s19jpro.dtb, devicetree.dtb, bitmain-am335x.dtb, or
#   am335x-boneblack.dtb.
#
# The script refuses to create a nonbootable rootfs-only image unless
# --allow-incomplete is passed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILDROOT_OUTPUT="${BUILDROOT_OUTPUT:-$PROJECT_DIR/buildroot/output/images}"
SD_OUTPUT_DIR="$BUILDROOT_OUTPUT/sd_card_am3_bb_s19jpro"
ARTIFACT_DIR=""
PAYLOAD_TAR=""
PAYLOAD_DIR=""
ALLOW_INCOMPLETE=0
DISK_IMAGE_SIZE_MB=128
# W24-SD-1 (2026-05-22): this resident-U-Boot `MLO + u-boot.img + bootm` chain
# boot-LOOPED `a lab unit` across three variants (2026-05-14). The PROVEN AM3-BB SD
# boot path is the VNish `boot.bin + go 0x88000000` flow in
# build_am3_bb_sd_vnish_bootbin_image.sh. This builder now REFUSES by default
# and redirects there; the old chain stays available ONLY for RE/diagnostic
# comparison behind an explicit, loud override flag (it is NOT a flashable
# product artifact).
# reports/w24-install-sd.md Finding 1.
ALLOW_BOOTLOOP_RESIDENT_UBOOT=0
CLEANUP_PATHS=()

cleanup() {
    for p in "${CLEANUP_PATHS[@]:-}"; do
        [ -n "$p" ] && rm -rf "$p"
    done
}
trap cleanup EXIT INT TERM

usage() {
    cat <<EOF
Usage: $(basename "$0") [--payload-tar <tar>|--payload-dir <dir>] --artifacts <dir> [--size-mb N]

Options:
  --artifacts <dir>   Directory containing MLO, u-boot.img, uImage, and DTB.
                      Required unless --allow-incomplete is set.
  --payload-tar <tar> Rootfs payload tar, e.g. dcentos-am3-bb-s19jpro-sdcard.tar.
  --payload-dir <dir> Directory containing uramdisk.image.gz and,
                      optionally, a pre-wrapped ramdisk.gz.
  --size-mb N         Total .img size (default: ${DISK_IMAGE_SIZE_MB}, auto-grown).
  --allow-incomplete  Build a known-nonbootable rootfs-only lab artifact.
  --i-know-this-boot-looped-79
                      RE/diagnostic ESCAPE HATCH. Build the resident-U-Boot
                      bootm chain that boot-looped .79. NOT a flashable product.
  -h, --help          Show this help.

DEFAULT BEHAVIOR (W24-SD-1): this builder REFUSES. The resident-U-Boot
(MLO + u-boot.img + bootm) chain it produces boot-looped the .79 S19j Pro BB
unit. The PROVEN AM3-BB SD boot path is the VNish boot.bin + go 0x88000000 flow:
  DCENT_OS_Antminer/scripts/build_am3_bb_sd_vnish_bootbin_image.sh

Output (only with --i-know-this-boot-looped-79): $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro.img
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
        --allow-incomplete)
            ALLOW_INCOMPLETE=1
            shift
            ;;
        --i-know-this-boot-looped-79)
            # RE/diagnostic ESCAPE HATCH ONLY. Builds the resident-U-Boot
            # `bootm` chain that boot-looped `a lab unit`. NOT a flashable product.
            ALLOW_BOOTLOOP_RESIDENT_UBOOT=1
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

# W24-SD-1: refuse the boot-looping resident-U-Boot chain by default.
if [ "$ALLOW_BOOTLOOP_RESIDENT_UBOOT" -ne 1 ]; then
    cat >&2 <<'EOF'
================================================================================
REFUSING TO BUILD: this is the resident-U-Boot (MLO + u-boot.img + bootm) SD
chain that BOOT-LOOPED the .79 S19j Pro BB unit across three variants on
2026-05-14. It is NOT a flashable product artifact.

The PROVEN AM3-BB SD boot path is the VNish boot.bin + `go 0x88000000` flow:

    DCENT_OS_Antminer/scripts/build_am3_bb_sd_vnish_bootbin_image.sh

That builder pins the VNish v1.2.6 S19j Pro boot.bin SHA256 fail-closed and
produces dcentos-am3-bb-s19jpro-vnish-bootbin.img. Use it instead.

If you REALLY need this old chain for RE/diagnostic comparison ONLY (it will
boot-loop on real hardware), re-run with:

    --i-know-this-boot-looped-79
================================================================================
EOF
    exit 2
fi

cat >&2 <<'EOF'
WARNING: building the resident-U-Boot bootm chain that boot-looped .79.
WARNING: this is an RE/diagnostic artifact ONLY — do NOT flash it to a miner.
WARNING: the proven path is build_am3_bb_sd_vnish_bootbin_image.sh.
EOF

case "$SD_OUTPUT_DIR" in
    /dev/*|\\\\.\\*)
        echo "ERROR: refusing to write to a block device path: $SD_OUTPUT_DIR" >&2
        exit 1
        ;;
esac

mkdir -p "$SD_OUTPUT_DIR"

need_tool() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "ERROR: missing host tool: $1" >&2
        echo "       Install with: $2" >&2
        exit 1
    }
}

total_bytes() {
    if [ -f "$1" ]; then
        stat -c%s "$1" 2>/dev/null || stat -f%z "$1"
    else
        echo 0
    fi
}

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
        "$PAYLOAD_DIR/dcentos-am3-bb-sdcard/uramdisk.image.gz" \
    ; do
        if [ -f "$candidate" ]; then
            ROOTFS_CPIO="$candidate"
            break
        fi
    done
    for candidate in \
        "$PAYLOAD_DIR/ramdisk.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-s19jpro-sdcard/ramdisk.gz" \
        "$PAYLOAD_DIR/dcentos-am3-bb-sdcard/ramdisk.gz" \
    ; do
        if [ -f "$candidate" ]; then
            RAMDISK_UIMAGE_SRC="$candidate"
            break
        fi
    done
else
    for candidate in \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-s19jpro-sdcard/uramdisk.image.gz" \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-sdcard/uramdisk.image.gz" \
        "$BUILDROOT_OUTPUT/rootfs.cpio.gz" \
    ; do
        if [ -f "$candidate" ]; then
            ROOTFS_CPIO="$candidate"
            break
        fi
    done
    for candidate in \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-s19jpro-sdcard/ramdisk.gz" \
        "$BUILDROOT_OUTPUT/dcentos-am3-bb-sdcard/ramdisk.gz" \
    ; do
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

MLO_SRC=""
UBOOT_SRC=""
UIMAGE_SRC=""
DTB_SRC=""
if [ -n "$ARTIFACT_DIR" ]; then
    [ -d "$ARTIFACT_DIR" ] || {
        echo "ERROR: artifact directory not found: $ARTIFACT_DIR" >&2
        exit 1
    }
    [ -f "$ARTIFACT_DIR/MLO" ] && MLO_SRC="$ARTIFACT_DIR/MLO"
    [ -f "$ARTIFACT_DIR/u-boot.img" ] && UBOOT_SRC="$ARTIFACT_DIR/u-boot.img"
    [ -f "$ARTIFACT_DIR/uImage" ] && UIMAGE_SRC="$ARTIFACT_DIR/uImage"
    for dtb in am335x-s19jpro.dtb devicetree.dtb bitmain-am335x.dtb am335x-boneblack.dtb; do
        if [ -f "$ARTIFACT_DIR/$dtb" ]; then
            DTB_SRC="$ARTIFACT_DIR/$dtb"
            break
        fi
    done
fi

if [ "$ALLOW_INCOMPLETE" -ne 1 ]; then
    missing=""
    [ -n "$MLO_SRC" ] || missing="$missing MLO"
    [ -n "$UBOOT_SRC" ] || missing="$missing u-boot.img"
    [ -n "$UIMAGE_SRC" ] || missing="$missing uImage"
    [ -n "$DTB_SRC" ] || missing="$missing DTB"
    if [ -n "$missing" ]; then
        echo "ERROR: missing required S19j Pro BB AM335x boot artifact(s):$missing" >&2
        echo "       Pass --artifacts <dir> containing MLO, u-boot.img, uImage, and DTB." >&2
        echo "       Use --allow-incomplete only for a known-nonbootable lab staging image." >&2
        exit 1
    fi
fi

need_tool sfdisk     "sudo apt install util-linux"
need_tool mkfs.vfat  "sudo apt install dosfstools"
need_tool mcopy      "sudo apt install mtools"
need_tool mkimage    "sudo apt install u-boot-tools"
need_tool file       "sudo apt install file"
need_tool gzip       "sudo apt install gzip"
need_tool cpio       "sudo apt install cpio"

validate_armv7_rootfs_cpio "$ROOTFS_CPIO"

IMG_FILE="$SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro.img"
P1_OFFSET_MB=1
P1_SIZE_MB=32
P2_OFFSET_MB=$((P1_OFFSET_MB + P1_SIZE_MB))

PAYLOAD_BYTES=$(total_bytes "$ROOTFS_CPIO")
for f in "$MLO_SRC" "$UBOOT_SRC" "$UIMAGE_SRC" "$DTB_SRC"; do
    [ -n "$f" ] && PAYLOAD_BYTES=$((PAYLOAD_BYTES + $(total_bytes "$f")))
done
# Store both the bootable legacy ramdisk image and the raw cpio.gz payload for
# inspection/recovery. They are nearly the same size, so account for both.
PAYLOAD_BYTES=$((PAYLOAD_BYTES + $(total_bytes "$ROOTFS_CPIO")))
NEEDED_MB=$(( (PAYLOAD_BYTES / 1024 / 1024) + P2_OFFSET_MB + 16 ))
if [ "$NEEDED_MB" -gt "$DISK_IMAGE_SIZE_MB" ]; then
    echo "[INFO] auto-growing disk image to ${NEEDED_MB} MB to fit payload"
    DISK_IMAGE_SIZE_MB="$NEEDED_MB"
fi

if [ "$DISK_IMAGE_SIZE_MB" -le "$P2_OFFSET_MB" ]; then
    echo "ERROR: image size ${DISK_IMAGE_SIZE_MB} MB is too small for AM3-BB two-partition layout" >&2
    exit 1
fi
P2_SIZE_MB=$((DISK_IMAGE_SIZE_MB - P2_OFFSET_MB))

echo "=== am3-bb-s19jpro SD disk image ==="
echo "Image:      $IMG_FILE"
echo "Size:       ${DISK_IMAGE_SIZE_MB} MB"
echo "Rootfs:     $ROOTFS_CPIO"
echo "Artifacts:  ${ARTIFACT_DIR:-none}"
echo "DTB:        ${DTB_SRC:-missing}"
echo ""

dd if=/dev/zero of="$IMG_FILE" bs=1M count="$DISK_IMAGE_SIZE_MB" status=none

sfdisk "$IMG_FILE" >/dev/null <<SFDISK_EOF
label: dos
unit: sectors

start=2048, size=65536, type=0e, bootable
start=67584, type=0c
SFDISK_EOF

BOOT_PART_TMP="$(mktemp)"
ROOT_PART_TMP="$(mktemp)"
CLEANUP_PATHS+=("$BOOT_PART_TMP" "$ROOT_PART_TMP")
dd if="$IMG_FILE" of="$BOOT_PART_TMP" bs=1M skip="$P1_OFFSET_MB" count="$P1_SIZE_MB" status=none
dd if="$IMG_FILE" of="$ROOT_PART_TMP" bs=1M skip="$P2_OFFSET_MB" count="$P2_SIZE_MB" status=none
mkfs.vfat -F 16 -n "DCENTBOOT" "$BOOT_PART_TMP" >/dev/null
mkfs.vfat -F 32 -n "DCENTBB" "$ROOT_PART_TMP" >/dev/null

export MTOOLS_SKIP_CHECK=1

copy_in() {
    local image="$1" src="$2" name="$3"
    [ -f "$src" ] || return 0
    mcopy -i "$image" "$src" "::$name"
    echo "  copied $name ($(total_bytes "$src") bytes)"
}

# Some AM335x revisions expect MLO to be the first FAT directory entry.
copy_in "$BOOT_PART_TMP" "$MLO_SRC" "MLO"
copy_in "$BOOT_PART_TMP" "$UBOOT_SRC" "u-boot.img"
copy_in "$BOOT_PART_TMP" "$UIMAGE_SRC" "uImage"
copy_in "$BOOT_PART_TMP" "$DTB_SRC" "devicetree.dtb"

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
    echo "ERROR: ramdisk.gz is not a U-Boot ARM legacy ramdisk image" >&2
    exit 1
}

if [ -n "$ARTIFACT_DIR" ]; then
    UENV_TMP="$(mktemp)"
    CLEANUP_PATHS+=("$UENV_TMP")
    cat > "$UENV_TMP" <<'EOF'
dcent_bootfile=uImage
dcent_fdtfile=devicetree.dtb
dcent_ramdiskfile=ramdisk.gz
mmcpart=2
#optargs=quiet
kloadaddr=0x80007fc0
rdaddr=0x81000000
fdtaddr=0x80F80000
bootargs_dcent=setenv bootargs console=${console} ${optargs} rdinit=/init
loaduimage_dcent=load mmc ${bootpart} ${kloadaddr} ${dcent_bootfile}
loadfdt_dcent=load mmc ${bootpart} ${fdtaddr} ${dcent_fdtfile}
loadramdisk_dcent=load mmc ${bootpart} ${rdaddr} ${dcent_ramdiskfile}
uenvcmd=echo DCENT_OS SD p2 initramfs boot; setenv bootpart ${mmcdev}:${mmcpart}; if run loaduimage_dcent loadfdt_dcent loadramdisk_dcent bootargs_dcent; then bootm ${kloadaddr} ${rdaddr} ${fdtaddr}; else echo DCENT_OS SD load failed, falling back to NAND; run nandboot; fi
EOF
    copy_in "$BOOT_PART_TMP" "$UENV_TMP" "uEnv.txt"
    copy_in "$ROOT_PART_TMP" "$UENV_TMP" "uEnv.txt"
fi

copy_in "$ROOT_PART_TMP" "$UIMAGE_SRC" "uImage"
copy_in "$ROOT_PART_TMP" "$DTB_SRC" "devicetree.dtb"
copy_in "$ROOT_PART_TMP" "$RAMDISK_UIMAGE_TMP" "ramdisk.gz"
copy_in "$ROOT_PART_TMP" "$ROOTFS_CPIO" "uramdisk.image.gz"

README_TMP="$(mktemp)"
CLEANUP_PATHS+=("$README_TMP")
cat > "$README_TMP" <<EOF
DCENT_OS am3-bb-s19jpro SD-card boot image
Created:  $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Rootfs:   uramdisk.image.gz (DCENT_OS armv7 rootfs CPIO)
Boot:     MLO + u-boot.img + uImage + devicetree.dtb + uEnv.txt + ramdisk.gz
Safety:   no NAND writes; no stock cgminer; no uart_trans.ko
EOF
copy_in "$BOOT_PART_TMP" "$README_TMP" "README.txt"
copy_in "$ROOT_PART_TMP" "$README_TMP" "README.txt"

dd if="$BOOT_PART_TMP" of="$IMG_FILE" bs=1M seek="$P1_OFFSET_MB" conv=notrunc status=none
dd if="$ROOT_PART_TMP" of="$IMG_FILE" bs=1M seek="$P2_OFFSET_MB" conv=notrunc status=none

# CE-204: emit a provenance sidecar and, when a release key is configured, an
# Ed25519 signature over the whole image (mirrors the PROVEN vnish builder's
# sign-after-build block). This builder is refuse-by-default (W24-SD-1); the
# image is a bootloop diagnostic, NOT a product artifact, and the sidecar says so.
IMG_SHA256="$(sha256sum "$IMG_FILE" | awk '{print $1}')"
cat > "$IMG_FILE.manifest.json" <<MANIFEST_EOF
{
  "created_utc": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "image": "$(basename "$IMG_FILE")",
  "sha256": "$IMG_SHA256",
  "bootloop_diagnostic": true,
  "not_a_product_artifact": true
}
MANIFEST_EOF
if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
    if [ -x "$SCRIPT_DIR/sign_sd_image.sh" ]; then
        "$SCRIPT_DIR/sign_sd_image.sh" "$IMG_FILE" || {
            echo "ERROR: sign_sd_image.sh failed for $IMG_FILE" >&2
            exit 1
        }
    else
        echo "[WARN] $SCRIPT_DIR/sign_sd_image.sh not found or not executable; skipping signing." >&2
    fi
fi
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "$SD_OUTPUT_DIR/release_ed25519.pub"
fi

IMG_BYTES=$(total_bytes "$IMG_FILE")
echo ""
echo "=== Done ==="
echo "  Image: $IMG_FILE ($((IMG_BYTES / 1024 / 1024)) MB)"
echo "  Flash: dd if=\"$IMG_FILE\" of=/dev/sdX bs=4M status=progress"
echo "         (or balenaEtcher / Rufus)"

if [ "$ALLOW_INCOMPLETE" -eq 1 ]; then
    echo ""
    echo "[WARN] --allow-incomplete was set. This image may not boot until"
    echo "       verified S19j Pro BB AM335x boot artifacts are added."
fi
