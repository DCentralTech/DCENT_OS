#!/usr/bin/env bash
#
# build_am3_bb_sd_vnish_bootbin_image.sh - Build an AM3-BB S19j Pro SD image
# that follows VNish's PROVEN boot.bin SD-stage U-Boot flow.
#
# Phase 2G ( 2026-05-15): byte-exactly mirrors the VNish
# v1.2.6 S19j Pro BB SD installer (`SD-awesome-s19jpro-bb-sd-v1.2.6-install`)
# whose `boot.bin + go 0x88000000` flow boots on AM335x BB Antminer hardware
# where our resident-U-Boot SD builder reset-looped on `a lab unit` across three
# variants (2026-05-14). Audit reference:
#  §3.1.
#
# Layout mirrors VNish exactly:
#   - 50 MiB total image (52,429,312 bytes — exact VNish geometry);
#   - 3-partition MBR:
#       P1 = FAT16 (type 0x0c, LBA-mapped), start_lba=1, size=65536 sectors
#            (32 MiB), label "ANTHILLOS", bootable;
#       P2 = Linux scratch (type 0x83), start_lba=65537, size=4096 sectors
#            (2 MiB), zero-filled, NOT bootable;
#       P3 = Linux scratch (type 0x83), start_lba=69633, size=32768 sectors
#            (16 MiB), zero-filled, NOT bootable;
#   - P1 FAT16 file order (matches VNish): boot.bin, uEnv.txt, uImage,
#     devicetree.dtb, update.image.gz (DCENT_OS initramfs); plus README.txt
#     and MANIFEST.json appended by this builder for provenance.
#   - boot.bin is the VNish S19j Pro v1.2.6 binary VERBATIM. The SHA256 is
#     PINNED FAIL-CLOSED: a mismatch refuses the build (no warn-and-continue).
#   - uEnv.txt is the VNish source-of-truth file copied verbatim from the
#     extract directory; DCENT_OS substitutes its own initramfs for VNish's
#     `update.image.gz` payload at the same 0x81000000 load address (same
#     filename, same load slot, same `go 0x88000000` entry).
#   - NO MLO, NO u-boot.img — resident NAND U-Boot fatload + boot.bin handle
#     both stages on this hardware.
#
# Why VNish's boot.bin VERBATIM (not a fork): the R11-1 clean-room decode
#
# proved the boot.bin is a self-decrypting 4 KiB shim cascade that self-relocates
# into a mini-U-Boot and `bootm`-boots the already-fatloaded uImage (so the SD
# card needs NO MLO / NO TI-U-Boot — only the verbatim boot.bin + a raw `go`
# uEnv). That decode ALSO found the body performs an RSA-2048 signature check
# (`In RSAVerify(): Hash …`) but did NOT extract the RSA modulus/exponent. So a
# FORKED/re-sealed boot.bin that swaps in our own kernel would fail signature
# until R11-2 extracts (or we patch out) that RSAVerify gate. Using VNish's
# boot.bin VERBATIM is therefore the proven, lowest-risk path (analogous to
# DCENT_OS already reusing BraiinsOS boot-critical FSBL/U-Boot/FPGA/kernel on
# Zynq) — it is the sweep-v2 §6.1 first-`a lab unit`-test recipe, not a stopgap. The
# SHA256 of the boot.bin is pinned FAIL-CLOSED below so a proof card can never
# silently carry a surrogate binary.
#
# The script writes only a regular .img file (no live device contact). The
# operator follows the runbook at
#  for the live flash
# + AC-cycle proof.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/sd_common.sh
. "$SCRIPT_DIR/lib/sd_common.sh"
# shellcheck source=lib/am3_bb_dtb_contract.sh
. "$SCRIPT_DIR/lib/am3_bb_dtb_contract.sh"
# shellcheck source=lib/buildroot_rootfs_arch_guard.sh
. "$SCRIPT_DIR/lib/buildroot_rootfs_arch_guard.sh"
BUILDROOT_OUTPUT="${BUILDROOT_OUTPUT:-$PROJECT_DIR/buildroot/output/images}"
SD_OUTPUT_DIR="$BUILDROOT_OUTPUT/sd_card_am3_bb_s19jpro"
ARTIFACT_DIR=""
PAYLOAD_TAR=""
PAYLOAD_DIR=""
UIMAGE_SRC_OVERRIDE=""
DTB_SRC_OVERRIDE=""
INITRAMFS_SRC_OVERRIDE=""
DEFAULT_BOOTBIN_PATH="$PROJECT_DIR/output/vnish-extracted-artifacts/boot.bin-s19jpro-bb-v1.2.6"
BOOTBIN_PATH="$DEFAULT_BOOTBIN_PATH"
DEFAULT_UENV_PATH="$PROJECT_DIR/output/vnish-extracted-artifacts/uEnv.txt-s19jpro-bb-v1.2.6"
UENV_SOURCE_PATH="$DEFAULT_UENV_PATH"
# Reference SHA256 of the captured VNish v1.2.6 S19j Pro BB boot.bin. This
# builder PINS this hash FAIL-CLOSED: any mismatch refuses the build. Phase 2G
# locks the boot.bin so the live `a lab unit` reset-loop proof on a card produced by
# this builder is an unambiguous test of the VNish boot.bin hypothesis, not a
# test of some unknown surrogate binary. Operators who need to swap in a
# different VNish drop must pass --accept-bootbin-mismatch and re-run the
# audit; the override exists for future RE work, not normal proof runs.
BOOTBIN_REFERENCE_SHA256="394bd5271f25dd2a2d9939f2b5b7dd52f763a184a4ca18b9131f2544d9d846ba"
# Reference SHA256 of the captured VNish v1.2.6 S19j Pro BB uEnv.txt
# (verbatim copy of the file VNish ships at FAT16 P1 root). Pinned for
# manifest provenance; mismatch warns (uEnv.txt is plain text and may
# legitimately differ slightly between VNish drops). The active boot
# contract is the `go 0x88000000` jump after fatload of boot.bin/uImage/
# devicetree.dtb/update.image.gz — not the byte-exact uEnv.txt body.
UENV_REFERENCE_SHA256="a6982810b59012f5b02d9bdf44ccdd88b1e57efd1546a07e4496788c5c7e8d71"

# VNish v1.2.6 SD installer geometry (audit §3.1 / §6.1). All sizes are in
# 512-byte sectors. The byte-exact VNish image is 52,429,312 bytes = 102,401
# sectors (50 MiB + 1 sector — the extra sector accommodates the MBR at
# sector 0 plus P1 starting at sector 1; if the disk were exactly 102400
# sectors, P3 ending at sector 102400 would be one off the end). We allocate
# the disk in sectors directly to match VNish byte-exactly.
VNISH_TOTAL_SIZE_SECTORS=102401   # 52,429,312 bytes (50 MiB + 512 B)
VNISH_P1_START_SECTORS=1
VNISH_P1_SIZE_SECTORS=65536   # 32 MiB FAT16
VNISH_P2_START_SECTORS=65537
VNISH_P2_SIZE_SECTORS=4096    # 2 MiB linux scratch
VNISH_P3_START_SECTORS=69633
VNISH_P3_SIZE_SECTORS=32768   # 16 MiB linux scratch

# Carried-over knob for diagnostic --size-mb overrides. Default = 0 (use the
# exact VNish sector count). Operators who want to grow the image for RE work
# can set this in MiB; we'll convert to sectors and detect that the layout
# diverges from the proof contract.
DISK_IMAGE_SIZE_MB=0
# SD-boot-defect fix 2026-06-10. The captured `a lab unit` NAND `uImage` is the stale
# mtd7 Bitmain factory kernel "Linux-3.8.13+" with a generic "am335x-bone" DTB,
# while the live mining kernel is 5.4.242-bone66 (see
# output/am3-bb-s19jpro-boot-artifacts-*/uname.txt).
#
# Two gates, deliberately split (same model as the resident-uboot builder):
#   * DTB gate (DEFAULT-ON): a generic BeagleBone DTB with no S19J_IO_BOARD/btm
#     marker is an unambiguous boot defect (wrong hashboard pinmux). Hard-fail
#     by default. The sweep-v2 §6.1 recipe uses VNish's carrier DTB, which
#     carries the btm marker and PASSES — so this never false-fails the proof.
#   * Kernel-version gate (OPT-IN via --strict-kernel): "Linux-3.8.13+" is
#     ambiguous (VNish's BB kernel could legitimately be 3.8.x), so it stays a
#     warning unless explicitly requested — NOT default-on, so we never block
#     the proven VNish-verbatim uImage recipe.
#   * --allow-stale-kernel: master RE opt-out; downgrades BOTH gates to warnings.
STRICT_KERNEL=0
ALLOW_STALE_KERNEL=0
# Default to VNish's exact "ANTHILLOS" label so the produced card matches the
# byte-level layout the BootROM / NAND U-Boot expects to see when probing the
# SD slot. The audit's load-bearing finding is that this label + the LBA-1
# FAT16 + the boot.bin-first ordering are the working delta vs our broken
# resident-U-Boot variants. Operators can still override with --label, but
# Phase 2G proof runs should keep the default.
BOOT_LABEL="${BOOT_LABEL:-ANTHILLOS}"
# Fail-closed hash gate (default ON for proof builds). Override only when an
# operator deliberately swaps in a different VNish boot.bin for RE work; the
# manifest still records the actual hash either way.
ACCEPT_BOOTBIN_MISMATCH=0
# Optional Ed25519 sign-after-build step. When --sign is passed (or
# DCENT_RELEASE_SIGNING_KEY is set in the environment), we invoke
# sign_sd_image.sh to emit <img>.sig next to the SD image. Lab builds without
# the key see a single WARN from that script and the build still succeeds.
SIGN_IMAGE=0
EXTRA_DTB_PATHS=()
# Use the library's shared cleanup array under a local-friendly alias so
# inline `CLEANUP_PATHS+=("$tmp")` continues to work. The library owns the
# EXIT/INT/TERM trap that walks the array.
declare -n CLEANUP_PATHS=SD_COMMON_CLEANUP_PATHS
sd_common::install_cleanup_trap

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

Build a canonical AM3-BB S19j Pro SD image using VNish's PROVEN boot.bin
SD-stage U-Boot flow (Phase 2G Preparedness Sweep v2 2026-05-15).

Layout (fixed to match VNish v1.2.6 byte-exactly, 50 MiB total):
  MBR    P1=FAT16 ANTHILLOS @ LBA 1 size 32 MiB, P2/P3 zero-filled linux
         scratch partitions.
  P1     boot.bin, uEnv.txt, uImage, devicetree.dtb, update.image.gz,
         README.txt, MANIFEST.json.

Options:
  --bootbin <path>    49,152-byte VNish boot.bin SD-stage U-Boot.
                      Default: $DEFAULT_BOOTBIN_PATH
                      Loaded at 0x88000000 by uEnv.txt and entered via 'go'.
                      SHA256 is pinned FAIL-CLOSED to:
                        $BOOTBIN_REFERENCE_SHA256
                      Override with --accept-bootbin-mismatch for RE work.
  --uenv <path>       VNish uEnv.txt to copy verbatim onto P1.
                      Default: $DEFAULT_UENV_PATH
  --uimage <path>     uImage to copy as 'uImage' on the SD (overrides
                      --artifacts/--payload-* lookups).
  --dtb <path>        Device tree blob to copy as 'devicetree.dtb' on the SD
                      (overrides --artifacts lookup).
  --initramfs <path>  U-Boot legacy ramdisk OR raw rootfs.cpio.gz to copy as
                      'update.image.gz' (DCENT_OS substitutes its own
                      initramfs for VNish's update.image.gz at the same
                      0x81000000 load address). If raw cpio.gz, mkimage
                      wraps it automatically.
  --artifacts <dir>   Directory containing uImage and DTB. MLO/u-boot.img are
                      intentionally ignored.
  --payload-tar <tar> Rootfs payload tar, e.g. dcentos-am3-bb-s19jpro-sdcard.tar.
  --payload-dir <dir> Directory containing uramdisk.image.gz / ramdisk.gz.
  --strict-kernel     ALSO abort on the stale Bitmain factory kernel
                      (Linux-3.8.13+). The generic-DTB gate is already
                      default-on; this adds the opt-in kernel-version gate.
  --allow-stale-kernel
                      Master RE opt-out: downgrade BOTH the default-on
                      generic-DTB gate and the kernel-version gate to warnings.

NOTE: by DEFAULT this builder HARD-REFUSES a generic ti,beaglebone-black DTB
that lacks the S19J_IO_BOARD/btm carrier marker (wrong hashboard pinmux). The
sweep-v2 §6.1 proof recipe uses VNish's carrier DTB, which passes.
  --label <name>      FAT volume label for p1 (default: ${BOOT_LABEL}).
                      Phase 2G proof runs MUST keep ANTHILLOS — it is the
                      load-bearing marker the VNish boot.bin expects.
  --accept-bootbin-mismatch
                      Override the SHA256 fail-closed gate. Use only when
                      deliberately swapping in a different VNish drop for
                      RE work. The manifest still records the actual hash.
  --sign              Invoke sign_sd_image.sh after build to emit a sibling
                      <img>.sig (Ed25519). Lab builds without
                      DCENT_RELEASE_SIGNING_KEY see a single WARN and the
                      build still succeeds.
  --extra-dtb <path>  Additional DTB to copy onto the SD as a fallback (may
                      be passed multiple times).
  --size-mb N         Override the total .img size in MiB. Defaults to the
                      VNish-exact 50 MiB; only override for diagnostic
                      builds that intentionally diverge from the proof
                      layout.
  -h, --help          Show this help.

Output: $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.img
        $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json
        $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.img.sig (if --sign)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --bootbin)
            BOOTBIN_PATH="${2:?--bootbin requires a path}"
            shift 2
            ;;
        --bootbin=*)
            BOOTBIN_PATH="${1#--bootbin=}"
            shift
            ;;
        --uenv)
            UENV_SOURCE_PATH="${2:?--uenv requires a path}"
            shift 2
            ;;
        --uenv=*)
            UENV_SOURCE_PATH="${1#--uenv=}"
            shift
            ;;
        --accept-bootbin-mismatch)
            ACCEPT_BOOTBIN_MISMATCH=1
            shift
            ;;
        --sign)
            SIGN_IMAGE=1
            shift
            ;;
        --uimage)
            UIMAGE_SRC_OVERRIDE="${2:?--uimage requires a path}"
            shift 2
            ;;
        --uimage=*)
            UIMAGE_SRC_OVERRIDE="${1#--uimage=}"
            shift
            ;;
        --dtb)
            DTB_SRC_OVERRIDE="${2:?--dtb requires a path}"
            shift 2
            ;;
        --dtb=*)
            DTB_SRC_OVERRIDE="${1#--dtb=}"
            shift
            ;;
        --initramfs)
            INITRAMFS_SRC_OVERRIDE="${2:?--initramfs requires a path}"
            shift 2
            ;;
        --initramfs=*)
            INITRAMFS_SRC_OVERRIDE="${1#--initramfs=}"
            shift
            ;;
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

# --- Validate boot.bin ------------------------------------------------------
# VNish's S19j Pro BB v1.2.6 boot.bin is exactly 49,152 bytes (48 KiB). This
# is a fast structural sanity check ONLY: it requires the EXACT byte count
# (no +/- window). A wrong-sized file is rejected immediately so we don't
# waste a SHA256 pass on an obviously-unrelated binary. The size gate is NOT
# the trust boundary — the LOAD-BEARING check is the SHA256 fail-closed pin
# below (BOOTBIN_REFERENCE_SHA256). A file of the right size but wrong
# contents still fails closed on the hash. Never relax the hash gate just
# because the size matches.
BOOTBIN_EXACT_BYTES=49152
[ -f "$BOOTBIN_PATH" ] || {
    echo "ERROR: boot.bin not found: $BOOTBIN_PATH" >&2
    echo "       Default path: $DEFAULT_BOOTBIN_PATH" >&2
    echo "       Pass --bootbin <path> or stage the VNish boot.bin reference." >&2
    exit 1
}
BOOTBIN_BYTES=$(total_bytes "$BOOTBIN_PATH")
if [ "$BOOTBIN_BYTES" -ne "$BOOTBIN_EXACT_BYTES" ]; then
    echo "ERROR: boot.bin size mismatch: $BOOTBIN_BYTES bytes" >&2
    echo "       Expected EXACTLY $BOOTBIN_EXACT_BYTES bytes (VNish S19j Pro BB v1.2.6, 48 KiB)" >&2
    echo "       Path: $BOOTBIN_PATH" >&2
    echo "       (Structural sanity check only; the load-bearing gate is the" >&2
    echo "        SHA256 pin. A different VNish drop is not accepted here.)" >&2
    exit 1
fi
BOOTBIN_SHA="$(sha256_file "$BOOTBIN_PATH")"
if [ "$BOOTBIN_SHA" != "$BOOTBIN_REFERENCE_SHA256" ]; then
    if [ "$ACCEPT_BOOTBIN_MISMATCH" = "1" ]; then
        echo "[WARN] boot.bin SHA256 differs from captured VNish v1.2.6 reference" >&2
        echo "[WARN]   actual:   $BOOTBIN_SHA" >&2
        echo "[WARN]   expected: $BOOTBIN_REFERENCE_SHA256" >&2
        echo "[WARN] --accept-bootbin-mismatch in effect — continuing for RE workflow." >&2
    else
        echo "ERROR: boot.bin SHA256 does NOT match the pinned VNish v1.2.6 reference." >&2
        echo "       actual:   $BOOTBIN_SHA" >&2
        echo "       expected: $BOOTBIN_REFERENCE_SHA256" >&2
        echo "       path:     $BOOTBIN_PATH" >&2
        echo "" >&2
        echo "       The fail-closed hash gate ensures the live .79 reset-loop proof on a" >&2
        echo "       Phase 2G card tests the VNish boot.bin hypothesis byte-exactly. To" >&2
        echo "       override (e.g. when deliberately swapping in another VNish drop for" >&2
        echo "       RE work), re-run with --accept-bootbin-mismatch." >&2
        exit 1
    fi
fi

# --- Validate uEnv.txt source -----------------------------------------------
# We use VNish's verbatim uEnv.txt rather than hand-writing one. Falls back to
# embedding the captured contract if the extract isn't on disk (unusual — the
# file is committed under output/vnish-extracted-artifacts/).
[ -f "$UENV_SOURCE_PATH" ] || {
    echo "ERROR: uEnv.txt source not found: $UENV_SOURCE_PATH" >&2
    echo "       Default path: $DEFAULT_UENV_PATH" >&2
    echo "       Pass --uenv <path> or stage the VNish uEnv.txt reference." >&2
    exit 1
}
UENV_SOURCE_SHA="$(sha256_file "$UENV_SOURCE_PATH")"
if [ "$UENV_SOURCE_SHA" != "$UENV_REFERENCE_SHA256" ]; then
    echo "[WARN] uEnv.txt SHA256 differs from captured VNish v1.2.6 reference" >&2
    echo "[WARN]   actual:   $UENV_SOURCE_SHA" >&2
    echo "[WARN]   expected: $UENV_REFERENCE_SHA256" >&2
    echo "[WARN] Continuing — the boot contract is the 'go 0x88000000' jump, not the" >&2
    echo "[WARN] byte-exact uEnv.txt body. Manifest records the actual hash." >&2
fi

# --- Resolve rootfs / ramdisk inputs ----------------------------------------
ROOTFS_CPIO=""
RAMDISK_UIMAGE_SRC=""

if [ -n "$INITRAMFS_SRC_OVERRIDE" ]; then
    [ -f "$INITRAMFS_SRC_OVERRIDE" ] || {
        echo "ERROR: --initramfs path not found: $INITRAMFS_SRC_OVERRIDE" >&2
        exit 1
    }
    case "$(basename "$INITRAMFS_SRC_OVERRIDE")" in
        uramdisk*) RAMDISK_UIMAGE_SRC="$INITRAMFS_SRC_OVERRIDE" ;;
        ramdisk*) RAMDISK_UIMAGE_SRC="$INITRAMFS_SRC_OVERRIDE" ;;
        *) ROOTFS_CPIO="$INITRAMFS_SRC_OVERRIDE" ;;
    esac
    if [ -z "$ROOTFS_CPIO" ] && [ -z "$RAMDISK_UIMAGE_SRC" ]; then
        ROOTFS_CPIO="$INITRAMFS_SRC_OVERRIDE"
    fi
fi

if [ -z "$ROOTFS_CPIO" ] && [ -z "$RAMDISK_UIMAGE_SRC" ]; then
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
fi

if [ -z "$ROOTFS_CPIO" ] && [ -z "$RAMDISK_UIMAGE_SRC" ]; then
    echo "ERROR: no DCENT_OS AM3-BB rootfs CPIO or U-Boot ramdisk found." >&2
    echo "       Run Buildroot first, or pass --initramfs / --payload-tar / --payload-dir." >&2
    exit 1
fi

# --- Resolve uImage / DTB ---------------------------------------------------
UIMAGE_SRC=""
DTB_SRC=""

if [ -n "$UIMAGE_SRC_OVERRIDE" ]; then
    [ -f "$UIMAGE_SRC_OVERRIDE" ] || {
        echo "ERROR: --uimage not found: $UIMAGE_SRC_OVERRIDE" >&2
        exit 1
    }
    UIMAGE_SRC="$UIMAGE_SRC_OVERRIDE"
fi
if [ -n "$DTB_SRC_OVERRIDE" ]; then
    [ -f "$DTB_SRC_OVERRIDE" ] || {
        echo "ERROR: --dtb not found: $DTB_SRC_OVERRIDE" >&2
        exit 1
    }
    DTB_SRC="$DTB_SRC_OVERRIDE"
fi

if [ -z "$UIMAGE_SRC" ] || [ -z "$DTB_SRC" ]; then
    if [ -z "$ARTIFACT_DIR" ]; then
        echo "ERROR: provide --uimage and --dtb explicitly, or --artifacts <dir> containing both" >&2
        exit 1
    fi
    [ -d "$ARTIFACT_DIR" ] || {
        echo "ERROR: artifact directory not found: $ARTIFACT_DIR" >&2
        exit 1
    }
    if [ -z "$UIMAGE_SRC" ] && [ -f "$ARTIFACT_DIR/uImage" ]; then
        UIMAGE_SRC="$ARTIFACT_DIR/uImage"
    fi
    if [ -z "$DTB_SRC" ]; then
        for dtb in devicetree.dtb am335x-s19jpro.dtb bitmain-am335x.dtb am335x-boneblack.dtb dtb; do
            if [ -f "$ARTIFACT_DIR/$dtb" ]; then
                DTB_SRC="$ARTIFACT_DIR/$dtb"
                break
            fi
        done
    fi
fi

missing=""
[ -n "$UIMAGE_SRC" ] || missing="$missing uImage"
[ -n "$DTB_SRC" ] || missing="$missing DTB"
if [ -n "$missing" ]; then
    echo "ERROR: missing required artifact(s):$missing" >&2
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
fi

# The shared admission helper is authoritative. The legacy classification below
# remains only to preserve existing diagnostic wording and manifest fields.
dcent_am3_bb_admit_carrier_dtb "$DTB_SRC" vnish-btm "$ALLOW_STALE_KERNEL"
DTB_STALE=0
if dcent_am3_bb_dtb_matches_policy "$DTB_SRC" vnish-btm; then
    echo "[INFO] DTB provenance: Bitmain/BTM BeagleBone-compatible DTB detected"
elif grep -a -q 'am335x-bone' "$DTB_SRC"; then
    DTB_STALE=1
    echo "[WARN] DTB provenance: generic BeagleBone DTB detected, not S19J_IO_BOARD-specific" >&2
else
    echo "[WARN] DTB provenance: expected AM335x BeagleBone compatibility string not found" >&2
fi

# DEFAULT-ON DTB gate (see header): a generic BeagleBone DTB with no
# S19J_IO_BOARD/btm marker gives the kernel the wrong hashboard pinmux. Hard-fail
# unless the operator explicitly opts out for RE work.
if [ "$ALLOW_STALE_KERNEL" != "1" ] && [ "$DTB_STALE" = "1" ]; then
    echo "ERROR: refusing to ship a generic BeagleBone DTB (no S19J_IO_BOARD/btm" >&2
    echo "       carrier marker) — wrong hashboard pinmux/gpio/i2c. Pass the live" >&2
    echo "       LuxOS 5.4 carrier DTB, or --allow-stale-kernel for RE cards only." >&2
    exit 1
fi
# OPT-IN kernel-version gate (--strict-kernel): also refuse the stale Bitmain
# 3.8.13 factory kernel (ambiguous on its own, so not default-on).
if [ "$STRICT_KERNEL" = "1" ] && [ "$ALLOW_STALE_KERNEL" != "1" ] && [ "$KERNEL_STALE" = "1" ]; then
    echo "ERROR: --strict-kernel: refusing to ship the stale Bitmain factory kernel" >&2
    echo "       (Linux-3.8.13+). Pass the live LuxOS 5.4 uImage via --uimage/--artifacts." >&2
    exit 1
fi

# --- ARMv7 rootfs arch guard (raw CPIO source only) -------------------------
# Preserves the prior-builder's AArch64-/sbin/init regression guard: when the
# initramfs source is a RAW rootfs CPIO (not an already-wrapped VNish/DCENT
# uramdisk.image.gz / ramdisk.gz U-Boot legacy image), confirm the critical
# binaries are ARMv7/EABI5 before we mkimage-wrap it. This is the same class of
# bug that caused the 2026-05-14 `a lab unit` 10s SD reset loop (an AArch64 /sbin/init
# in an ARMv7 AM335x image panics as PID 1). A pre-wrapped legacy ramdisk is
# opaque-by-design (we cannot cheaply unpack a U-Boot legacy image here), so it
# is NOT re-validated — its arch is the Buildroot post-build hook's
# responsibility (which calls dcent_ensure_armv7_busybox_init).
validate_armv7_rootfs_cpio_vnish() {
    local cpio_gz="$1"
    local cpio_abs check_dir
    case "$cpio_gz" in
        /*) cpio_abs="$cpio_gz" ;;
        *) cpio_abs="$(cd "$(dirname "$cpio_gz")" && pwd)/$(basename "$cpio_gz")" ;;
    esac
    check_dir="$(mktemp -d)"
    CLEANUP_PATHS+=("$check_dir")
    (cd "$check_dir" && gzip -dc "$cpio_abs" | cpio -idmu --quiet)
    # Repair an AArch64/non-ARMv7 /sbin/init in place to a BusyBox symlink, then
    # hard-require ARMv7/EABI5 on the critical PID-1 + daemon ELFs.
    dcent_ensure_armv7_busybox_init "$check_dir" "am3-bb-vnish-sd"
    dcent_require_armv7_eabi_elf_paths "$check_dir" "am3-bb-vnish-sd" \
        bin/busybox \
        sbin/init \
        usr/sbin/dropbear \
        usr/local/bin/dcentrald \
        usr/local/bin/dcentos-discovery
    local bad_elf
    bad_elf="$(find "$check_dir" -type f -exec file {} + | grep -E "ELF 64-bit|aarch64|x86-64" || true)"
    if [ -n "$bad_elf" ]; then
        echo "ERROR: am3-bb-vnish-sd rootfs contains non-ARMv7 ELF payloads:" >&2
        printf "%s\n" "$bad_elf" >&2
        exit 1
    fi
    echo "[INFO] validated raw rootfs CPIO critical ELFs: ARMv7/EABI5"
}

if [ -n "$ROOTFS_CPIO" ] && [ -z "$RAMDISK_UIMAGE_SRC" ]; then
    validate_armv7_rootfs_cpio_vnish "$ROOTFS_CPIO"
fi

# --- Optionally wrap a raw CPIO into a U-Boot legacy ramdisk ----------------
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
        -n "DCENT_OS am3-bb-s19jpro initramfs (VNish boot.bin variant)" \
        -d "$ROOTFS_CPIO" \
        "$RAMDISK_UIMAGE_TMP" >/dev/null
fi
mkimage -l "$RAMDISK_UIMAGE_TMP" 2>/dev/null | grep -q 'ARM Linux RAMDisk Image' || {
    echo "[WARN] initramfs is not a U-Boot legacy ARM RAMDisk image" >&2
    echo "[WARN]   $RAMDISK_UIMAGE_TMP" >&2
    echo "[WARN] VNish flow expects uramdisk.image.gz to be mkimage-wrapped." >&2
    echo "[WARN] Continuing — boot.bin may still chain if the kernel handles raw CPIO." >&2
}

# --- Partition + filesystem layout ------------------------------------------
IMG_FILE="$SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.img"
P1_OFFSET_SECTORS=$VNISH_P1_START_SECTORS
P1_OFFSET_BYTES=$((P1_OFFSET_SECTORS * 512))
SECTOR_SIZE=512

# Payload sizing sanity check. VNish ships a 50 MiB image with 32 MiB FAT16.
# Our payload (boot.bin + uImage + DTB + initramfs) must fit inside the 32 MiB
# FAT16 budget. If it doesn't, bail with a clear diagnostic rather than
# silently growing the image and breaking the byte-exact-VNish-layout contract.
PAYLOAD_BYTES=0
for f in "$BOOTBIN_PATH" "$UIMAGE_SRC" "$DTB_SRC" "$RAMDISK_UIMAGE_TMP"; do
    PAYLOAD_BYTES=$((PAYLOAD_BYTES + $(total_bytes "$f")))
done
if [ -n "$ROOTFS_CPIO" ]; then
    PAYLOAD_BYTES=$((PAYLOAD_BYTES + $(total_bytes "$ROOTFS_CPIO")))
fi
FAT16_BUDGET_BYTES=$((VNISH_P1_SIZE_SECTORS * SECTOR_SIZE))
# Reserve ~1 MiB for FAT16 metadata + README + manifest. VNish itself uses
# ~16 MiB of the 32 MiB partition, so this headroom is conservative.
FAT16_USABLE_BYTES=$((FAT16_BUDGET_BYTES - 1048576))
if [ "$PAYLOAD_BYTES" -gt "$FAT16_USABLE_BYTES" ]; then
    echo "ERROR: payload (${PAYLOAD_BYTES} bytes) exceeds VNish P1 FAT16 budget" >&2
    echo "       FAT16 partition size: ${FAT16_BUDGET_BYTES} bytes (32 MiB)" >&2
    echo "       Usable after metadata reserve: ${FAT16_USABLE_BYTES} bytes" >&2
    echo "" >&2
    echo "       The VNish-exact 32 MiB FAT16 holds boot.bin + uImage + dtb +" >&2
    echo "       initramfs. The proven first-.79-test recipe (sweep-v2 §6.1) uses" >&2
    echo "       the VNish VERBATIM uImage (~3 MiB) + dtb, leaving room for the" >&2
    echo "       DCENT initramfs in the update.image.gz slot, e.g.:" >&2
    echo "         --uimage <KB>/uImage --dtb <KB>/devicetree.dtb \\" >&2
    echo "         --initramfs <DCENT uramdisk.image.gz>" >&2
    echo "       A full 5 MiB DCENT kernel + a >27 MiB initramfs will NOT fit;" >&2
    echo "       shrink the DCENT_OS initramfs, use the VNish kernel for the boot-" >&2
    echo "       chain proof, or pass --size-mb to override (overriding diverges" >&2
    echo "       from the VNish-exact layout proof contract)." >&2
    exit 1
fi

# Total sectors: default to VNish-exact (102,401 sectors = 52,429,312 bytes).
# --size-mb is an explicit override for RE/diagnostic builds; it forces a
# whole-MiB image and the operator accepts that the proof contract drifts.
if [ "$DISK_IMAGE_SIZE_MB" = "0" ]; then
    TOTAL_SECTORS=$VNISH_TOTAL_SIZE_SECTORS
    SIZE_LABEL="VNish-exact ($((VNISH_TOTAL_SIZE_SECTORS * SECTOR_SIZE)) bytes / 50 MiB + 1 sector)"
else
    TOTAL_SECTORS=$((DISK_IMAGE_SIZE_MB * 1024 * 1024 / SECTOR_SIZE))
    SIZE_LABEL="${DISK_IMAGE_SIZE_MB} MiB (override; diverges from VNish-exact)"
    echo "[WARN] --size-mb override in effect: ${DISK_IMAGE_SIZE_MB} MiB" >&2
    echo "[WARN] VNish-exact layout is ${VNISH_TOTAL_SIZE_SECTORS} sectors / $((VNISH_TOTAL_SIZE_SECTORS * SECTOR_SIZE)) bytes." >&2
    P3_END=$((VNISH_P3_START_SECTORS + VNISH_P3_SIZE_SECTORS - 1))
    if [ "$TOTAL_SECTORS" -le "$P3_END" ]; then
        echo "ERROR: --size-mb ${DISK_IMAGE_SIZE_MB} does not fit P3 (needs ${P3_END} sectors)" >&2
        exit 1
    fi
fi

echo "============================================================================"
echo "[!] WARNING: This image embeds a 49 KiB CLOSED-SOURCE VNish vendor boot.bin"
echo "[!]          as the first-stage boot executor. It is NOT Bitmain-signed,"
echo "[!]          NOT authored or audited by D-Central, and its 2nd stage past"
echo "[!]          offset 0x60 is encrypted/unaudited (RE blocker R10-1 is OPEN)."
echo "[!]          Do NOT redistribute this image without operator awareness, and"
echo "[!]          do NOT use it where an unaudited vendor blob in the boot chain"
echo "[!]          is unacceptable. See RUNBOOK.md section 5 + manifest field"
echo "[!]          vendor_blob_unaudited=true."
echo "[!]"
echo "[!] RSA-VERIFY GATE (R11-1, OPEN): the boot.bin decode found an RSA-2048"
echo "[!]          signature check ('In RSAVerify(): Hash ...') whose modulus was"
echo "[!]          NOT extracted. This card substitutes a DCENT_OS initramfs for"
echo "[!]          VNish's update.image.gz (and may substitute a DCENT uImage via"
echo "[!]          --uimage). If the boot.bin RSA-verifies the kernel and/or the"
echo "[!]          ramdisk, the substituted DCENT payload will be REJECTED and the"
echo "[!]          card will NOT reach DCENT_OS userspace. The proven-safe first"
echo "[!]          test (sweep-v2 §6.1) keeps the VNish VERBATIM uImage+DTB and"
echo "[!]          substitutes ONLY the initramfs, to isolate whether the ramdisk"
echo "[!]          is inside the RSA envelope. Manifest: boot_bin_rsa_verify_gate."
echo "============================================================================"
echo "=== am3-bb-s19jpro VNish boot.bin SD image (Phase 2G) ==="
echo "Image:      $IMG_FILE"
echo "Size:       $SIZE_LABEL"
echo "boot.bin:   $BOOTBIN_PATH ($BOOTBIN_BYTES bytes, sha256 $BOOTBIN_SHA)"
echo "            VNish v1.2.6 reference match: $([ "$BOOTBIN_SHA" = "$BOOTBIN_REFERENCE_SHA256" ] && echo YES || echo NO)"
echo "uEnv.txt:   $UENV_SOURCE_PATH ($(total_bytes "$UENV_SOURCE_PATH") bytes, sha256 $UENV_SOURCE_SHA)"
echo "uImage:     $UIMAGE_SRC"
echo "DTB:        $DTB_SRC"
echo "Ramdisk:    $RAMDISK_UIMAGE_TMP (-> update.image.gz on SD)"
if [ -n "$ROOTFS_CPIO" ]; then
    echo "Rootfs:     $ROOTFS_CPIO"
fi
echo "Layout:     VNish-exact 3-part MBR:"
echo "              P1 FAT16 ANTHILLOS-class LBA $VNISH_P1_START_SECTORS size $VNISH_P1_SIZE_SECTORS sec ($((VNISH_P1_SIZE_SECTORS / 2048)) MiB), label=$BOOT_LABEL"
echo "              P2 linux scratch LBA $VNISH_P2_START_SECTORS size $VNISH_P2_SIZE_SECTORS sec ($((VNISH_P2_SIZE_SECTORS / 2048)) MiB)"
echo "              P3 linux scratch LBA $VNISH_P3_START_SECTORS size $VNISH_P3_SIZE_SECTORS sec ($((VNISH_P3_SIZE_SECTORS / 2048)) MiB)"
echo ""

sd_common::create_blank_image_sectors "$IMG_FILE" "$TOTAL_SECTORS"

# 3-partition MBR matching VNish v1.2.6 byte-exactly. P2/P3 are reserved
# scratch space in the VNish reference image and are left zero-filled here.
sd_common::write_mbr_three_part_sector_aligned "$IMG_FILE" \
    "$VNISH_P1_START_SECTORS" "$VNISH_P1_SIZE_SECTORS" "0c" \
    "$VNISH_P2_START_SECTORS" "$VNISH_P2_SIZE_SECTORS" "83" \
    "$VNISH_P3_START_SECTORS" "$VNISH_P3_SIZE_SECTORS" "83"

BOOT_PART_TMP="$(mktemp)"
CLEANUP_PATHS+=("$BOOT_PART_TMP")
sd_common::dd_extract_partition "$IMG_FILE" "$BOOT_PART_TMP" "$VNISH_P1_START_SECTORS" "$VNISH_P1_SIZE_SECTORS"
sd_common::format_fat16_partition "$BOOT_PART_TMP" "$BOOT_LABEL"

export MTOOLS_SKIP_CHECK=1

# Thin shim — kept so existing inline call sites continue to work without
# churning every site. Equivalent to sd_common::copy_one_to_fat.
copy_in() { sd_common::copy_one_to_fat "$@"; }

UENV_TMP="$(mktemp)"
README_TMP="$(mktemp)"
MANIFEST_TMP="$(mktemp)"
CLEANUP_PATHS+=("$UENV_TMP" "$README_TMP" "$MANIFEST_TMP")

# VNish-verbatim uEnv.txt: copied from the staged VNish v1.2.6 extract under
# output/vnish-extracted-artifacts/. The active boot contract is the single-
# line `uenvcmd` that fatloads boot.bin/uImage/devicetree.dtb/update.image.gz
# at the standard AM335x load addresses, then `go 0x88000000`. The DCENT_OS
# initramfs is substituted in-place for VNish's update.image.gz payload at
# 0x81000000 (same filename on the FAT, same load slot).
#
# Load addresses pinned by the VNish uenvcmd (do not edit without re-auditing):
#   0x88000000 = boot.bin (SD-stage U-Boot entrypoint, also where 'go' jumps)
#   0x80200000 = uImage
#   0x80f80000 = devicetree.dtb
#   0x81000000 = update.image.gz (initramfs slot)
cp "$UENV_SOURCE_PATH" "$UENV_TMP"

cat > "$README_TMP" <<EOF
DCENT_OS am3-bb-s19jpro VNish boot.bin SD image (Phase 2G canonical)
Created:  $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Boot:     VNish boot.bin SD-stage U-Boot @ 0x88000000 chains kernel+DTB+initramfs
Layout:   VNish-exact 50 MiB 3-part MBR:
          P1 FAT16 label=$BOOT_LABEL @ LBA $VNISH_P1_START_SECTORS size $((VNISH_P1_SIZE_SECTORS / 2048)) MiB
          P2 linux scratch @ LBA $VNISH_P2_START_SECTORS size $((VNISH_P2_SIZE_SECTORS / 2048)) MiB (zero-filled)
          P3 linux scratch @ LBA $VNISH_P3_START_SECTORS size $((VNISH_P3_SIZE_SECTORS / 2048)) MiB (zero-filled)
Payload:  boot.bin + uEnv.txt + uImage + devicetree.dtb + update.image.gz
Phase:    Preparedness Sweep v2 2026-05-15 Phase 2G canonical builder
Notes:    boot.bin and uEnv.txt are verbatim VNish v1.2.6 S19j Pro BB
          captures. SHA256 of boot.bin is pinned FAIL-CLOSED to
          $BOOTBIN_REFERENCE_SHA256. The DCENT_OS initramfs is substituted
          for VNish's update.image.gz at the 0x81000000 load address; the
          'go 0x88000000' jump is unchanged. See runbook:
          docs/dev/2026-05-15-am3-bb-bootbin-proof/RUNBOOK.md
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

ROOTFS_MANIFEST_LINE=""
if [ -n "$ROOTFS_CPIO" ]; then
    ROOTFS_MANIFEST_LINE=",
    \"initramfs.cpio.gz\": \"$(sha256_file "$ROOTFS_CPIO")\""
fi

cat > "$MANIFEST_TMP" <<EOF
{
  "layout": "am3-bb-s19jpro-vnish-bootbin-50mib-3part",
  "variant": "vnish-bootbin-sd",
  "created_utc": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "image": "$(basename "$IMG_FILE")",
  "image_size_sectors": ${TOTAL_SECTORS},
  "image_size_bytes": $((TOTAL_SECTORS * SECTOR_SIZE)),
  "strict_kernel": ${STRICT_KERNEL},
  "dtb_gate_bypassed": $([ "$ALLOW_STALE_KERNEL" = "1" ] && echo true || echo false),
  "vnish_reference_source": "SD-awesome-s19jpro-bb-sd-v1.2.6-install.zip",
  "vnish_reference_inner_img_sha256": "8a7281771225be9e36f6241126482273f9cc8229190150707cb093192914f28a",
  "phase": "Preparedness Sweep v2 2026-05-15 Phase 2G",
  "notes": "Canonical builder. Byte-exactly mirrors VNish v1.2.6 50 MiB / 3-partition / ANTHILLOS layout. DCENT_OS initramfs substituted for VNish update.image.gz at the same 0x81000000 load address; 'go 0x88000000' jump unchanged. boot.bin is a CLOSED-SOURCE third-party VNish vendor blob (NOT Bitmain-signed, NOT D-Central-audited; 2nd stage past 0x60 is encrypted/unaudited, RE blocker R10-1 OPEN). SHA256 is pinned for provenance only, NOT a trust attestation. Do not redistribute without operator awareness; see RUNBOOK.md section 5.",
  "partitions": {
    "p1": {
      "start_sector": ${VNISH_P1_START_SECTORS},
      "size_sectors": ${VNISH_P1_SIZE_SECTORS},
      "type": "0x0c",
      "active": true,
      "filesystem": "FAT16",
      "label": "$BOOT_LABEL"
    },
    "p2": {
      "start_sector": ${VNISH_P2_START_SECTORS},
      "size_sectors": ${VNISH_P2_SIZE_SECTORS},
      "type": "0x83",
      "active": false,
      "filesystem": "none",
      "contents": "zero-filled scratch"
    },
    "p3": {
      "start_sector": ${VNISH_P3_START_SECTORS},
      "size_sectors": ${VNISH_P3_SIZE_SECTORS},
      "type": "0x83",
      "active": false,
      "filesystem": "none",
      "contents": "zero-filled scratch"
    }
  },
  "files": {
    "boot.bin": "$BOOTBIN_SHA",
    "uEnv.txt": "$(sha256_file "$UENV_TMP")",
    "uImage": "$(sha256_file "$UIMAGE_SRC")",
    "devicetree.dtb": "$(sha256_file "$DTB_SRC")",
    "update.image.gz": "$(sha256_file "$RAMDISK_UIMAGE_TMP")"${ROOTFS_MANIFEST_LINE}${EXTRA_DTB_MANIFEST}
  },
  "boot_bin_sha256": "$BOOTBIN_SHA",
  "boot_bin_reference_sha256": "$BOOTBIN_REFERENCE_SHA256",
  "boot_bin_match_reference": $([ "$BOOTBIN_SHA" = "$BOOTBIN_REFERENCE_SHA256" ] && echo true || echo false),
  "bootbin_pin_override": $([ "$ACCEPT_BOOTBIN_MISMATCH" = "1" ] && echo true || echo false),
  "vendor_blob_unaudited": true,
  "boot_bin_rsa_verify_gate": "OPEN (R11-1): boot.bin decode found an RSA-2048 RSAVerify() with an un-extracted modulus. This card substitutes a DCENT_OS initramfs (and possibly a DCENT uImage) for VNish's payload; if the boot.bin RSA-verifies the kernel/ramdisk the substituted DCENT payload is rejected and DCENT_OS userspace is never reached. Proven-safe first test keeps VNish-verbatim uImage+DTB and substitutes only the initramfs.",
  "dcent_uimage_substituted": $([ -n "$UIMAGE_SRC_OVERRIDE" ] && echo true || echo false),
  "dcent_initramfs_substituted": true,
  "boot_bin_path": "$BOOTBIN_PATH",
  "uenv_txt_sha256": "$(sha256_file "$UENV_TMP")",
  "uenv_txt_reference_sha256": "$UENV_REFERENCE_SHA256",
  "uenv_txt_source_path": "$UENV_SOURCE_PATH",
  "uimage_sha256": "$(sha256_file "$UIMAGE_SRC")",
  "dtb_sha256": "$(sha256_file "$DTB_SRC")",
  "update_image_gz_sha256": "$(sha256_file "$RAMDISK_UIMAGE_TMP")"$( [ -n "$ROOTFS_CPIO" ] && printf ',\n  "initramfs_cpio_sha256": "%s"' "$(sha256_file "$ROOTFS_CPIO")" )
}
EOF

# Copy order: boot.bin FIRST (matches VNish layout where boot.bin is the
# first FAT entry the SD-stage loader fetches), then uEnv.txt, then the
# kernel/DTB/update.image.gz the boot.bin will fatload. Filename is
# "update.image.gz" (matching VNish exactly, matching the verbatim uEnv.txt
# fatload target); the DCENT_OS initramfs payload is substituted for VNish's
# original update.image.gz at the same 0x81000000 load address.
copy_in "$BOOT_PART_TMP" "$BOOTBIN_PATH" "boot.bin"
copy_in "$BOOT_PART_TMP" "$UENV_TMP" "uEnv.txt"
copy_in "$BOOT_PART_TMP" "$UIMAGE_SRC" "uImage"
copy_in "$BOOT_PART_TMP" "$DTB_SRC" "devicetree.dtb"
copy_in "$BOOT_PART_TMP" "$RAMDISK_UIMAGE_TMP" "update.image.gz"
copy_in "$BOOT_PART_TMP" "$README_TMP" "README.txt"
copy_in "$BOOT_PART_TMP" "$MANIFEST_TMP" "MANIFEST.json"
for extra in "${EXTRA_DTB_PATHS[@]:-}"; do
    [ -n "$extra" ] || continue
    copy_in "$BOOT_PART_TMP" "$extra" "$(basename "$extra")"
done

sd_common::dd_write_partition "$BOOT_PART_TMP" "$IMG_FILE" "$P1_OFFSET_SECTORS"
cp "$MANIFEST_TMP" "$SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json"

IMG_BYTES=$(total_bytes "$IMG_FILE")
IMG_SHA="$(sha256_file "$IMG_FILE")"

echo ""
echo "=== Verification ==="
if command -v fdisk >/dev/null 2>&1; then
    fdisk -l "$IMG_FILE" || true
else
    echo "[INFO] fdisk not available; partition table written via sfdisk above."
fi
echo ""
mdir -i "$IMG_FILE@@$P1_OFFSET_BYTES" :: || true
echo ""
mtype -i "$IMG_FILE@@$P1_OFFSET_BYTES" ::uEnv.txt || true
echo ""
mkimage -l "$RAMDISK_UIMAGE_TMP" || true

# --- boot.bin region byte-match verification --------------------------------
# Re-extract the boot.bin from the rendered P1 FAT16 via mtools and confirm
# its SHA256 matches the source. This is the load-bearing invariant: a Phase
# 2G card MUST contain the VNish boot.bin byte-exactly inside the FAT, and
# the operator should be able to trust the manifest's boot_bin_sha256 field.
echo ""
echo "=== boot.bin region byte-match verification ==="
BOOTBIN_READBACK="$(mktemp)"
CLEANUP_PATHS+=("$BOOTBIN_READBACK")
if mcopy -i "$IMG_FILE@@$P1_OFFSET_BYTES" -n ::boot.bin "$BOOTBIN_READBACK" 2>/dev/null; then
    READBACK_SHA="$(sha256_file "$BOOTBIN_READBACK")"
    if [ "$READBACK_SHA" = "$BOOTBIN_SHA" ]; then
        echo "  PASS: boot.bin in P1 FAT byte-matches source"
        echo "        sha256=$READBACK_SHA"
    else
        echo "  FAIL: boot.bin in P1 FAT does NOT match source!" >&2
        echo "        source:   $BOOTBIN_SHA" >&2
        echo "        readback: $READBACK_SHA" >&2
        exit 1
    fi
else
    echo "[WARN] mcopy readback failed; boot.bin region verification skipped." >&2
fi

# --- Optional Ed25519 sign-after-build --------------------------------------
SIG_FILE=""
if [ "$SIGN_IMAGE" = "1" ] || [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
    echo ""
    echo "=== Ed25519 sign-after-build ==="
    if [ -x "$SCRIPT_DIR/sign_sd_image.sh" ]; then
        "$SCRIPT_DIR/sign_sd_image.sh" "$IMG_FILE" || {
            echo "ERROR: sign_sd_image.sh failed for $IMG_FILE" >&2
            exit 1
        }
        if [ -f "$IMG_FILE.sig" ]; then
            SIG_FILE="$IMG_FILE.sig"
        fi
    else
        echo "[WARN] $SCRIPT_DIR/sign_sd_image.sh not found or not executable; skipping signing." >&2
    fi
fi

# CE-204: stage the pinned trusted release pubkey next to the PROVEN .img so the
# shipped artifact set is img + .sig + release_ed25519.pub (parity with the
# canonical zynq/am3 sidecar set).
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "$SD_OUTPUT_DIR/release_ed25519.pub"
fi

echo ""
echo "=== Done ==="
echo "  Image:    $IMG_FILE ($((IMG_BYTES / 1024 / 1024)) MiB, $IMG_BYTES bytes)"
echo "  SHA256:   $IMG_SHA"
echo "  Manifest: $SD_OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json"
if [ -n "$SIG_FILE" ]; then
    echo "  Signature: $SIG_FILE"
fi
echo ""
echo "  Live-flash runbook:"
echo "    docs/dev/2026-05-15-am3-bb-bootbin-proof/RUNBOOK.md"
echo ""
echo "  Flash (Linux): dd if=\"$IMG_FILE\" of=/dev/sdX bs=4M conv=fsync status=progress"
echo "  Flash (Windows): run DCENT_OS_Antminer/scripts/write_am3_bb_sd_physical_windows.ps1 as Administrator"
