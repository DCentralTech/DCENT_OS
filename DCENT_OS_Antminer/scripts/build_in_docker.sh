#!/usr/bin/env bash
#
# build_in_docker.sh — One-shot Buildroot + sysupgrade packaging in Docker.
#
# Docker equivalent of build_in_wsl.sh. Uses Dockerfile.build (Ubuntu 22.04
# with Buildroot deps) and a named Docker volume so Buildroot can work on
# a case-sensitive ext4 filesystem — bind-mounting from NTFS breaks the
# build the same way it broke on /mnt/c under WSL.
#
# Release usage (from a bash shell with Docker running):
#
#   cd DCENT_OS_Antminer
#   bash scripts/build_s9_release_capsule.sh
#
# This file is the inner packaging driver. Release admission requires the
# source-snapshot, invocation, sealed Cargo-result, and private release-set
# authorities prepared by build_s9_release_capsule.sh; direct release use is
# rejected.
#
# RELEASE BUILDS (BUG-2, 2026-07-09): export DCENT_MANIFEST_PUBLIC_KEY_HEX
# (64-hex ed25519 verifying key) for BOTH build steps —
# scripts/build-dcentrald.sh (the pin is baked into dcentrald at cargo-build
# time via option_env!) AND this script (validates the env, then Phase 5
# verifies via `strings` that the staged binary actually embeds it).
# Exporting it only here cannot retro-pin an already-built binary;
# build-dcentrald.sh now fails fast if the pin is missing in a release
# context instead of letting Phase 5 fail hours later.
#
# TOOLCHAIN SOURCING (BUG-3, 2026-07-09): releases.linaro.org deleted the
# pinned Linaro 7.2-2017.11 binary releases (HTTP 404). Phase 5b sources the
# toolchain from, in order: the persistent in-volume dl-cache
# (/build/dcentos/dl-cache/, survives Buildroot re-clones), an
# operator/CI-provided local dir (export DCENT_TOOLCHAIN_LOCAL_DIR=<dir
# containing the tarball> — populate with scripts/provision_build_inputs.sh),
# then network (original URL, then a hash-verified mirror). Every source is
# verified against the recorded SHA256 pin (fail-closed on release builds).
#
# Capsule path (in order):
#   1. Build an invocation-tagged image from the authenticated Dockerfile.
#   2. Create a fresh, exactly labelled invocation Buildroot volume.
#   3. Stage Git-object source bytes without CRLF/mode/version rewrites.
#   4. Stage  into the volume at the path
#      package_sysupgrade.sh probes ($FIRMWARE_DIR/extractions/s9).
#   5. Stage receipt-bound ARM binaries from the sealed result stage into the
#      same layout so the Buildroot overlay picks it up.
#   6. Run `make setup` + `make -j$(nproc)` inside a container.
#   7. Run `package_sysupgrade.sh --board am1-s9 --output /build/out.tar`.
#   8. Copy the complete evidence set only to the private release-set stage;
#      the outer driver seals and atomically promotes that directory.
#

set -euo pipefail

# On Windows/Git-Bash, MSYS rewrites /-prefixed paths before they reach docker,
# which breaks `-v "C:/path:/container"`. Disable conversion for the whole run.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

# -------- Target selection (Phase 2 multi-target support) --------
# Default: s9 (am1-s9). Alt: am2-s19jpro (am2-s19j — S19j Pro Zynq variant).
# Keeps the S9 path bit-for-bit identical. Adding `--target am2-s19jpro`
# switches the Buildroot defconfig, package board name, and output filename.
TARGET="s9"
OUTPUT_DIR_OVERRIDE=""
LAB_UNSIGNED=0
while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="$2"
            shift 2
            ;;
        --target=*)
            TARGET="${1#--target=}"
            shift
            ;;
        --output-dir)
            OUTPUT_DIR_OVERRIDE="$2"
            shift 2
            ;;
        --output-dir=*)
            OUTPUT_DIR_OVERRIDE="${1#--output-dir=}"
            shift
            ;;
        --lab-unsigned)
            LAB_UNSIGNED=1
            shift
            ;;
        -h|--help)
            echo "Usage: $(basename "$0") [--target s9|am2-s19jpro|am2-s19pro|am2-s17pro|am3-s19kpro|am3-s21|am3-s19jpro-aml|am3-t21|am3-bb|am3-bb-s19jpro|am3-bb-s19jpro-vnish] [--output-dir DIR] [--lab-unsigned]"
            echo ""
            echo "Options:"
            echo "  --lab-unsigned  Reserved for a future explicit lab capsule; direct"
            echo "                  packaging currently fails closed before build work."
            echo ""
            echo "Targets:"
            echo "  s9               (default) Antminer S9 am1-s9 (armv7) — tarball: dcentos-sysupgrade-118.tar"
            echo "  am2-s19jpro      Antminer S19j Pro Zynq am2-s19jpro (armv7) — tarball: dcentos-sysupgrade-am2-s19jpro.tar"
            # Added by Phase 2K
            echo "  am2-s19pro       Antminer S19 / S19 Pro Zynq am2 BM1398 (armv7) — tarball: dcentos-sysupgrade-am2-s19pro.tar"
            echo "  am2-s17pro       Antminer S17 / S17 Pro Zynq am2-s17 (armv7, RUNTIME-ONLY) — tarball: dcentos-sysupgrade-am2-s17pro.tar"
            # End Phase 2K
            echo "  am3-s19kpro      Antminer S19k Pro Amlogic am3-aml (aarch64) — tarball: dcentos-sysupgrade-am3-s19kpro.tar"
            echo "  am3-s21          Antminer S21 Amlogic am3-aml (aarch64) — tarball: dcentos-sysupgrade-am3-s21.tar"
            # Added by Phase 4B
            echo "  am3-s19jpro-aml  Antminer S19j Pro Amlogic am3-aml PIC1704 (aarch64) — tarball: dcentos-sysupgrade-am3-s19jpro-aml.tar"
            echo "  am3-t21          Antminer T21 Amlogic am3-aml NoPic (aarch64) — tarball: dcentos-sysupgrade-am3-t21.tar"
            # End Phase 4B
            echo "  am3-bb           Antminer S19j Pro BeagleBone am3-bb (armv7) — tarball: dcentos-am3-bb-sdcard.tar"
            echo "  am3-bb-s19jpro   Antminer S19j Pro BeagleBone BB unit (armv7) — tarball: dcentos-am3-bb-s19jpro-sdcard.tar"
            echo "  am3-bb-s19jpro-vnish Phase 1B VNish boot.bin SD prototype (armv7) — image: dcentos-am3-bb-s19jpro-vnish-bootbin.img"
            exit 0
            ;;
        *)
            echo "ERROR: unknown flag: $1 (try --help)" >&2
            exit 1
            ;;
    esac
done

is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}

is_release_status() {
    case "${1:-release}" in
        release|production|stable) return 0 ;;
        *) return 1 ;;
    esac
}

# BR_DEFCONFIG_FRAGMENTS lists the shared fragments to concatenate BEFORE
# the per-product defconfig at Phase 6 setup. Fragments are space-separated
# and resolved relative to br2_external_dcentos/configs/. Order matters:
# upstream (workspace-wide) first, arch-specific second, per-product last.
# When a key is declared more than once, the LAST occurrence wins because
# `make defconfig` processes the merged file top-to-bottom and a later
# `KEY=value` line overrides an earlier one.
case "$TARGET" in
    s9)
        BR_DEFCONFIG="dcentos_s9_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am1-s9"
        TARBALL_NAME="dcentos-sysupgrade-118.tar"
        BOARD_POST_IMAGE="internal"  # uses package_sysupgrade.sh explicitly
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    am2-s19jpro)
        BR_DEFCONFIG="dcentos_am2_s19jpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am2-s19j"
        TARBALL_NAME="dcentos-sysupgrade-am2-s19jpro.tar"
        # For am2, post-image.sh inside the board dir produces the tarball
        # directly (sysupgrade-am2-s19j/ prefix). Phase 7 copies it out.
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    am2-s19jpro-sd)
        #  2026-05-15 Phase 4D: am2 S19j Pro SD-disk
        # variant. Same Buildroot tree as am2-s19jpro (defconfig +
        # BOARD_PKG_NAME + sysupgrade tarball are reused); the post-image
        # step is replaced with build_am2_s19jpro_sd_disk_image.sh which
        # produces a flashable two-partition .img (FAT16 boot + ext2
        # rootfs) for the XIL-class home-lab bring-up flow. Mirrors how
        # am3-bb-s19jpro-vnish wires onto the same defconfig (Phase 1B).
        # Output is the .img in buildroot/output/images/sd_card_am2_s19jpro/.
        BR_DEFCONFIG="dcentos_am2_s19jpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am2-s19j"
        TARBALL_NAME="dcentos-am2-s19jpro.img"
        BOARD_POST_IMAGE="am2-sd-disk"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    # Added by  Phase 2K (2026-05-15, DevOps-F2):
    # am2-s19pro + am2-s17pro Buildroot variants were created (Phase 2D /
    # Phase 2E) but never wired into this builder — unbuildable until now.
    # Both ride the same Linaro 7.2.1 armv7 toolchain + board-script
    # post-image as am2-s19jpro. The board dir's post-image.sh produces the
    # tarball directly (sysupgrade-<board_target>/ prefix); Phase 7 copies
    # it out.
    am2-s19pro)
        # S19 / S19 Pro Zynq am2, BM1398 (114 chips/chain). Distinct from
        # am2-s19jpro (BM1362) — the dcentos_am2_s19pro_defconfig uses
        # board_target "am2-s19pro" so a BM1398-only baked image does not
        # collide with the BM1362 am2-s19jpro image. s19jpro cold-boot
        # mining proven 2026-04-10 via /tmp overlay; baked-image live flash
        # still pending.
        BR_DEFCONFIG="dcentos_am2_s19pro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am2-s19pro"
        TARBALL_NAME="dcentos-sysupgrade-am2-s19pro.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    am2-s17pro)
        # S17 / S17 Pro Zynq am2-s17, BM1396 / BM1397. RUNTIME-ONLY
        # scaffold — no live S17 / S17 Pro on the fleet and no extracted
        # S17 kernel, so the board post-image.sh emits a rootfs-only WARN
        # build unless $DCENT_AM2_S17_KERNEL is supplied. BOARD_PKG_NAME is
        # "am2-s17p" (NOT "am2-s17pro") to match the board post-image.sh
        # BOARD_NAME and the sysupgrade-am2-s17p/ prefix.
        # F-2 (Sweep-v3 PR-079): the canonical defconfig file is
        # `dcentos_am2_s17pro_zynq_defconfig` (its siblings dropped the
        # `_zynq` suffix at creation time; only the S17 Pro one kept it).
        # The arm previously named `dcentos_am2_s17pro_defconfig`, which
        # does not exist — a hard build break for the am2-s17pro target.
        BR_DEFCONFIG="dcentos_am2_s17pro_zynq_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am2-s17p"
        TARBALL_NAME="dcentos-sysupgrade-am2-s17pro.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    # End Phase 2K
    am3-s19kpro)
        BR_DEFCONFIG="dcentos_am3_s19kpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment dcentos_am3_aml_common.fragment"
        BOARD_PKG_NAME="am3-s19k"
        TARBALL_NAME="dcentos-sysupgrade-am3-s19kpro.tar"
        # am3-aml uses board-script post-image (mkimage uImage CPIO + tar
        # with sysupgrade-am3-s19k/ prefix). Phase H.9 board script.
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="aarch64-unknown-linux-musl"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/aarch64-linux-gnu/$TOOLCHAIN_FILE"
        ;;
    am3-s21)
        BR_DEFCONFIG="dcentos_am3_s21_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment dcentos_am3_aml_common.fragment"
        BOARD_PKG_NAME="am3-s21"
        TARBALL_NAME="dcentos-sysupgrade-am3-s21.tar"
        # am3-aml S21 uses board-script post-image (mkimage uImage CPIO
        # + tar with sysupgrade-am3-s21/ prefix).
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="aarch64-unknown-linux-musl"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/aarch64-linux-gnu/$TOOLCHAIN_FILE"
        ;;
    # Added by Phase 4B (2026-05-15): am3-AML Buildroot variant fill-in.
    # New per-product carriers on the same A113D Amlogic SoC and 4.9.113
    # kernel. Reuse the am3-aml common fragment + board-script post-image
    # exactly like the existing am3-s21 / am3-s19kpro arms.
    am3-s19jpro-aml)
        # S19j Pro Amlogic (PIC1704 voltage controller at I²C 0x20, BM1362
        # hashboards). Distinct from am2-s19jpro (Zynq am2 / dsPIC). No
        # live unit yet — sysupgrade tarball is build-only.
        BR_DEFCONFIG="dcentos_am3_s19jpro_aml_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment dcentos_am3_aml_common.fragment"
        BOARD_PKG_NAME="am3-s19jpro-aml"
        TARBALL_NAME="dcentos-sysupgrade-am3-s19jpro-aml.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="aarch64-unknown-linux-musl"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/aarch64-linux-gnu/$TOOLCHAIN_FILE"
        ;;
    am3-t21)
        # T21 — BM1368 NoPic, S21 sibling on the same A113D carrier.
        # Lower-tier hashrate envelope. Same TAS5782M DAC voltage rail.
        BR_DEFCONFIG="dcentos_am3_t21_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment dcentos_am3_aml_common.fragment"
        BOARD_PKG_NAME="am3-t21"
        TARBALL_NAME="dcentos-sysupgrade-am3-t21.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="aarch64-unknown-linux-musl"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/aarch64-linux-gnu/$TOOLCHAIN_FILE"
        ;;
    # End Phase 4B
    am3-bb)
        BR_DEFCONFIG="dcentos_am3_bb_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am3-bb"
        TARBALL_NAME="dcentos-am3-bb-sdcard.tar"
        # am3-bb uses a board-script post-image that stages an SD-card
        # payload tarball only. It is not a NAND/sysupgrade package.
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    am3-bb-s19jpro)
        # W10.11 ( closure): NEW S19j Pro AM335x BB carrier variant
        # per agent B3. NAND install/revert disabled until live /proc/mtd
        # evidence captured (see B3 commit msg). Same A8 toolchain as am3-bb.
        BR_DEFCONFIG="dcentos_am3_bb_s19jpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am3-bb-s19jpro"
        TARBALL_NAME="dcentos-am3-bb-s19jpro-sdcard.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    am3-bb-s19jpro-vnish)
        #  2026-05-15 Phase 1B: VNish boot.bin SD prototype
        # variant. Same Buildroot tree as am3-bb-s19jpro (defconfig +
        # BOARD_PKG_NAME + sdcard tarball are reused); the post-image step
        # is replaced with build_am3_bb_sd_vnish_bootbin_image.sh which
        # mirrors VNish v1.2.6's working SD installer flow (boot.bin SD-stage
        # U-Boot loaded at 0x88000000 chaining uImage/DTB/initramfs at the
        # standard AM335x addresses). Output is the .img + manifest in
        # buildroot/output/images/sd_card_am3_bb_s19jpro/.
        BR_DEFCONFIG="dcentos_am3_bb_s19jpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="am3-bb-s19jpro"
        TARBALL_NAME="dcentos-am3-bb-s19jpro-vnish-bootbin.img"
        BOARD_POST_IMAGE="vnish-bootbin-sd"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    cv1835-s19jpro)
        # W10.11 ( closure): NEW S19j Pro Cvitek CV1835 carrier variant
        # per agent B5. NO LIVE FLEET UNIT — `runtime-only` install routing
        # until 3 successful round-trips on a bench unit. Cortex-A7 + eMMC.
        BR_DEFCONFIG="dcentos_cv1835_s19jpro_defconfig"
        BR_DEFCONFIG_FRAGMENTS="dcentos-common.fragment"
        BOARD_PKG_NAME="cv1835-s19jpro"
        TARBALL_NAME="dcentos-sysupgrade-cv1835-s19jpro.tar"
        BOARD_POST_IMAGE="board-script"
        BUILD_ARCH="armv7-unknown-linux-musleabihf"
        TOOLCHAIN_FILE="gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz"
        TOOLCHAIN_URL="https://releases.linaro.org/components/toolchain/binaries/7.2-2017.11/arm-linux-gnueabihf/$TOOLCHAIN_FILE"
        ;;
    *)
        echo "ERROR: unsupported target: $TARGET (supported: s9, am2-s19jpro, am2-s19jpro-sd, am2-s19pro, am2-s17pro, am3-s19kpro, am3-s21, am3-s19jpro-aml, am3-t21, am3-bb, am3-bb-s19jpro, am3-bb-s19jpro-vnish, cv1835-s19jpro)" >&2
        exit 1
        ;;
esac

# Immutable Buildroot source identity shared by cold- and warm-volume paths.
# A warm checkout is never trusted merely because its Makefile exists.
BUILDROOT_REPOSITORY="https://github.com/buildroot/buildroot.git"
BUILDROOT_COMMIT="7c8edc1b402efcd7bba2dabfe0b3be877adaed7a"

dcent_target_requires_dcentos_init() {
    case "$1" in
        s9|am2-s19jpro|am2-s19jpro-sd|am2-s19pro|am2-s17pro|am3-s19kpro|am3-s21|am3-s19jpro-aml|am3-t21)
            return 0
            ;;
    esac
    return 1
}

dcent_target_requires_dcentos_discovery() {
    case "$1" in
        am3-bb|am3-bb-s19jpro|am3-bb-s19jpro-vnish|cv1835-s19jpro)
            return 0
            ;;
    esac
    return 1
}

dcent_validate_docker_transport_path() {
    python3 - "$1" <<'PY'
import sys

value = sys.argv[1]
if not value or any(character in value for character in ("\0", "\r", "\n")):
    raise SystemExit("unsafe empty/control-character Docker transport path")
windows_drive = (
    len(value) >= 3
    and value[0].isalpha()
    and value[1] == ":"
    and value[2] in ("/", "\\")
)
remainder = value[2:] if windows_drive else value
if ":" in remainder:
    raise SystemExit(f"unsafe colon in Docker transport path: {value!r}")
PY
}

dcent_require_absolute_host_path() {
    case "$1" in
        /*|[A-Za-z]:[\\/]*|\\\\*) return 0 ;;
        *) return 1 ;;
    esac
}

dcent_required_prebuilt_binaries() {
    printf '%s\n' dcentrald
    if dcent_target_requires_dcentos_init "$TARGET"; then
        printf '%s\n' dcentos-init
    fi
    if dcent_target_requires_dcentos_discovery "$TARGET"; then
        printf '%s\n' dcentos-discovery
    fi
}

dcent_expected_build_variant() {
    case "$1" in
        s9|am2-s19jpro|am2-s19jpro-sd|am2-s19pro|am2-s17pro) printf '%s\n' zynq ;;
        am3-s19kpro|am3-s21|am3-s19jpro-aml|am3-t21) printf '%s\n' amlogic ;;
        # BB packaging currently stages the static musl ARMv7 artifact emitted
        # by the zynq build lane (the standalone `beaglebone` builder targets
        # glibc armv7-unknown-linux-gnueabihf and cannot satisfy BUILD_ARCH).
        am3-bb|am3-bb-s19jpro|am3-bb-s19jpro-vnish) printf '%s\n' zynq ;;
        cv1835-s19jpro) printf '%s\n' cvitek ;;
        *) return 1 ;;
    esac
}

dcent_stale_sources_for_binary() {
    binary_path=$1
    binary_name=$2

    find "$PROJECT_DIR/dcentrald" -path '*/target' -prune -o \
        \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
        -newer "$binary_path" -print 2>/dev/null | awk 'NR <= 3 { print }'

    if [ "$binary_name" = "dcentrald" ]; then
        if [ -d "$PROJECT_DIR/dcent-schema" ]; then
            find "$PROJECT_DIR/dcent-schema" -path '*/target' -prune -o \
                \( -name '*.rs' -o -name 'Cargo.toml' \) \
                -newer "$binary_path" -print 2>/dev/null | awk 'NR <= 3 { print }'
        fi
        for baked in \
            "${DCENT_STOCK_MANIFEST_DIR:-$PROJECT_DIR/vendor/stock-manifest}/stock-bitmain-manifest.json" \
            "${DCENT_STOCK_MANIFEST_DIR:-$PROJECT_DIR/vendor/stock-manifest}/stock-bitmain-manifest.json.sig"; do
            if [ -f "$baked" ] && [ "$baked" -nt "$binary_path" ]; then
                printf '%s\n' "$baked"
            fi
        done
    fi
}

dcent_phase0_stale_binary_guard() {
    failed=0
    if [ "$CAPSULE_MODE" = "1" ]; then
        release_dir="$CAPSULE_RESULT_ROOT/target/$BUILD_ARCH/release"
        metadata_file="$CAPSULE_RESULT_ROOT/target/release-inventory/${BUILD_ARCH}.metadata.json"
        toolchain_context="$CAPSULE_RESULT_ROOT/target/release-inventory/${BUILD_ARCH}.toolchain.txt"
        compile_environment="$CAPSULE_RESULT_ROOT/target/release-inventory/${BUILD_ARCH}.compile-env.txt"
    else
        release_dir="$PROJECT_DIR/dcentrald/target/$BUILD_ARCH/release"
        metadata_file="$PROJECT_DIR/dcentrald/target/release-inventory/${BUILD_ARCH}.metadata.json"
        toolchain_context="$PROJECT_DIR/dcentrald/target/release-inventory/${BUILD_ARCH}.toolchain.txt"
        compile_environment="$PROJECT_DIR/dcentrald/target/release-inventory/${BUILD_ARCH}.compile-env.txt"
    fi
    expected_variant=$(dcent_expected_build_variant "$TARGET") || {
        echo "ERROR: no Rust build variant mapping exists for package target $TARGET" >&2
        return 1
    }

    if ! python3 "$SCRIPT_DIR/binary_build_receipt.py" check-override-policy \
        --allow-stale "${DCENT_ALLOW_STALE_DCENTRALD:-0}" \
        --release-provenance "${DCENT_REQUIRE_RELEASE_PROVENANCE:-0}" \
        --release-image "${DCENT_RELEASE_IMAGE:-0}" \
        --package-status "${DCENT_PACKAGE_STATUS:-}"; then
        return 1
    fi

    if [ "$CAPSULE_MODE" != "1" ]; then
        echo "ERROR: packaging prebuilt Rust binaries now requires an invocation capsule" >&2
        echo "       use scripts/build_s9_release_capsule.sh for the migrated S9 release lane" >&2
        return 1
    fi
    export_args=(
        export-snapshot-set
        --git-object-repo "$GIT_OBJECT_REPO"
        --source-snapshot "$CAPSULE_SOURCE_SNAPSHOT"
        --source-commit "$DCENT_SOURCE_COMMIT"
        --source-workspace DCENT_OS_Antminer/dcentrald
        --release-invocation "$CAPSULE_INVOCATION_STAGE"
        --result-root "$CAPSULE_RESULT_ROOT"
        --build-input-snapshot "$CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT"
        --target "$BUILD_ARCH"
        --profile release
        --build-variant "$expected_variant"
        --metadata "$metadata_file"
        --toolchain-context "$toolchain_context"
        --compile-environment "$compile_environment"
        --stage-parent "$BINARY_EXPORT_PARENT"
    )
    if dcent_release_provenance_required; then
        export_args+=(--require-immutable-builder)
    fi

    for binary_name in $(dcent_required_prebuilt_binaries); do
        binary_path="$release_dir/$binary_name"
        if [ ! -f "$binary_path" ]; then
            echo "ERROR: required prebuilt $binary_name binary is missing." >&2
            echo "  expected: $binary_path" >&2
            echo "  Recompile first: bash DCENT_OS_Antminer/scripts/build-dcentrald.sh $BUILD_ARCH" >&2
            failed=1
            continue
        fi

        export_args+=(--pair "$binary_path" "${binary_path}.build-receipt.json")

        # Capsule source is immutable and Git-authenticated; mtimes belong to
        # materialization, not source freshness, so the legacy advisory is not
        # meaningful in this lane.
    done


    if [ "$failed" -ne 0 ]; then
        return "$failed"
    fi

    if ! BINARY_EXPORT_STAGE="$(
        python3 "$SCRIPT_DIR/binary_build_receipt.py" "${export_args[@]}"
    )"; then
        echo "ERROR: immutable prebuilt Rust binary export failed." >&2
        echo "  Recompile first with: bash DCENT_OS_Antminer/scripts/build-dcentrald.sh $expected_variant" >&2
        if is_truthy "${DCENT_ALLOW_STALE_DCENTRALD:-0}"; then
            echo "  DCENT_ALLOW_STALE_DCENTRALD is deprecated and does not bypass receipt/export validation." >&2
        fi
        return 1
    fi
    if ! BINARY_EXPORT_CAPABILITY="$(
        python3 "$SCRIPT_DIR/binary_build_receipt.py" \
            export-snapshot-capability-path --stage "$BINARY_EXPORT_STAGE"
    )"; then
        echo "ERROR: cannot locate the binary export destruction capability." >&2
        return 1
    fi

    return 0
}

dcent_phase0_stale_binary_guard_selftest() {
    selftest_script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    sh "$selftest_script_dir/test_binary_build_receipt.sh"
}

if [ "${DCENT_STALE_BINARY_GUARD_SELFTEST:-0}" = "1" ]; then
    dcent_phase0_stale_binary_guard_selftest
    exit $?
fi

if [ "$LAB_UNSIGNED" = "1" ]; then
    export DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1
    export DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-lab_unsigned}"
fi
DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"
DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-release}"
if is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" && is_release_status "$DCENT_PACKAGE_STATUS"; then
    echo "ERROR: DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 requires non-release DCENT_PACKAGE_STATUS (for example lab_unsigned)." >&2
    exit 1
fi
# CE-183: a release-status package must not decouple from release-image
# hardening (SSH root lockdown + /etc/dcentos/release-image marker). Fail fast
# here (seconds) instead of after the 30-90 min Buildroot phase.
if is_release_status "$DCENT_PACKAGE_STATUS" && ! is_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
    echo "ERROR: release-status package (DCENT_PACKAGE_STATUS=$DCENT_PACKAGE_STATUS) requires DCENT_RELEASE_IMAGE=1." >&2
    echo "       Release status must not decouple from release-image hardening (CE-183)." >&2
    echo "       Use 'make release', or set DCENT_PACKAGE_STATUS to a non-release lab value (e.g. lab_signed)." >&2
    exit 1
fi
if [ -z "${DCENT_RELEASE_SIGNING_KEY:-}" ] \
    && ! is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then
    echo "ERROR: release sysupgrade builds require DCENT_RELEASE_SIGNING_KEY." >&2
    echo "       Use --lab-unsigned only for controlled lab packages." >&2
    exit 1
fi

# W1.2 (2026-05-07): manifest pin CI gate. dcentrald-api compiles the
# stock-Bitmain manifest at-rest signature pin in via `option_env!()` on
# `DCENT_MANIFEST_PUBLIC_KEY_HEX` (see dcentrald-api/src/ota_signature.rs).
# When unset at build time, the at-rest pin is silently dropped and the
# binary fails-open on manifest verification — fine for dev work, NOT for
# release. Fail closed unless the operator explicitly opts out via the
# documented dev escape hatch (DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1).
# W4.5 (2026-05-07): the am3-bb carve-out is closed. BB targets now
# require DCENT_MANIFEST_PUBLIC_KEY_HEX + DCENT_RELEASE_SIGNING_KEY just
# like every other target. `--lab-unsigned` (or DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1)
# remains the only escape hatch, and DCENT_PACKAGE_STATUS must be
# non-release for either to be accepted.
if [ -z "${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" ] \
    && ! is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then
    echo "ERROR: release builds require DCENT_MANIFEST_PUBLIC_KEY_HEX (64 hex chars, raw" >&2
    echo "       32-byte ed25519 verifying key) so the at-rest stock-Bitmain manifest" >&2
    echo "       signature pin is baked into the dcentrald binary." >&2
    echo "       See DCENT_OS_Antminer/release/README.md for keypair generation." >&2
    echo "       Dev opt-out: DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 (or --lab-unsigned)." >&2
    exit 1
fi
if [ -n "${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" ]; then
    # Light shape validation (length + hex). The Rust verifier will reject
    # anything that doesn't decode to 32 bytes, but failing here gives the
    # operator a much clearer error than a Buildroot mid-pipeline crash.
    case "${#DCENT_MANIFEST_PUBLIC_KEY_HEX}" in
        64) ;;
        *)
            echo "ERROR: DCENT_MANIFEST_PUBLIC_KEY_HEX must be exactly 64 hex chars" >&2
            echo "       (raw 32-byte ed25519 verifying key). Got ${#DCENT_MANIFEST_PUBLIC_KEY_HEX}." >&2
            exit 1
            ;;
    esac
    if ! printf '%s' "$DCENT_MANIFEST_PUBLIC_KEY_HEX" | grep -qE '^[0-9a-fA-F]{64}$'; then
        echo "ERROR: DCENT_MANIFEST_PUBLIC_KEY_HEX must be 64 hex chars (0-9a-fA-F)." >&2
        exit 1
    fi
    export DCENT_MANIFEST_PUBLIC_KEY_HEX
fi
if [ -n "${DCENT_MANIFEST_KEY_ID:-}" ]; then
    export DCENT_MANIFEST_KEY_ID
fi
export DCENT_ALLOW_UNSIGNED_SYSUPGRADE DCENT_PACKAGE_STATUS

# -------- Config --------
IMAGE_NAME="dcentos-build:latest"
VOLUME_NAME="dcentos-build-work"

# -------- Paths --------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"                    # DCENT_OS_Antminer
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"             # DCENT Projects repo root
GIT_OBJECT_REPO="$REPO_ROOT"
CAPSULE_MODE=0
CAPSULE_SOURCE_SNAPSHOT=""
CAPSULE_SOURCE_TREE=""
CAPSULE_SOURCE_ID=""
CAPSULE_SOURCE_DESCRIPTOR_SHA256=""
CAPSULE_INVOCATION_STAGE=""
CAPSULE_INVOCATION_ID=""
CAPSULE_RESULT_STAGE=""
CAPSULE_RESULT_ROOT=""
CAPSULE_EXTERNAL_INPUT_ROOT=""
CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT=""
CAPSULE_RELEASE_SET_CAPABILITY_FILE=""
CAPSULE_RELEASE_SET_STAGE=""
if [ "${DCENT_RELEASE_CAPSULE_MODE:-0}" = "1" ]; then
    CAPSULE_MODE=1
    for capsule_variable in \
        DCENT_CAPSULE_GIT_OBJECT_REPO \
        DCENT_CAPSULE_SOURCE_SNAPSHOT \
        DCENT_CAPSULE_SOURCE_COMMIT \
        DCENT_CAPSULE_INVOCATION_STAGE \
        DCENT_CAPSULE_RESULT_STAGE \
        DCENT_CAPSULE_RESULT_ROOT \
        DCENT_CAPSULE_EXTERNAL_INPUT_ROOT \
        DCENT_CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT \
        DCENT_CAPSULE_RELEASE_SET_CAPABILITY_FILE; do
        if [ -z "${!capsule_variable:-}" ]; then
            echo "ERROR: capsule packaging requires $capsule_variable" >&2
            exit 1
        fi
    done
    GIT_OBJECT_REPO="$DCENT_CAPSULE_GIT_OBJECT_REPO"
    CAPSULE_SOURCE_SNAPSHOT="$DCENT_CAPSULE_SOURCE_SNAPSHOT"
    CAPSULE_INVOCATION_STAGE="$DCENT_CAPSULE_INVOCATION_STAGE"
    CAPSULE_RESULT_STAGE="$DCENT_CAPSULE_RESULT_STAGE"
    CAPSULE_RESULT_ROOT="$DCENT_CAPSULE_RESULT_ROOT"
    CAPSULE_EXTERNAL_INPUT_ROOT="$DCENT_CAPSULE_EXTERNAL_INPUT_ROOT"
    CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT="$DCENT_CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT"
    CAPSULE_RELEASE_SET_CAPABILITY_FILE="$DCENT_CAPSULE_RELEASE_SET_CAPABILITY_FILE"
    CAPSULE_VERIFY_RESULT="$(python3 "$SCRIPT_DIR/source_snapshot.py" \
        verify-against-git \
        --repo-root "$GIT_OBJECT_REPO" \
        --commit "$DCENT_CAPSULE_SOURCE_COMMIT" \
        "$CAPSULE_SOURCE_SNAPSHOT")"
    capsule_verified_field() {
        printf '%s\n' "$CAPSULE_VERIFY_RESULT" \
            | python3 "$SCRIPT_DIR/source_snapshot.py" query-verified --field "$1"
    }
    dcent_capsule_shell_path() {
        if command -v cygpath >/dev/null 2>&1; then
            cygpath -u "$1"
        else
            printf '%s\n' "$1"
        fi
    }
    CAPSULE_SOURCE_TREE="$(dcent_capsule_shell_path "$(capsule_verified_field tree)")"
    CAPSULE_SOURCE_ID="$(capsule_verified_field snapshot_id)"
    CAPSULE_SOURCE_DESCRIPTOR_SHA256="$(capsule_verified_field descriptor_sha256)"
    CAPSULE_INVOCATION_ID="$(python3 "$SCRIPT_DIR/release_invocation.py" query \
        --field invocation_id "$CAPSULE_INVOCATION_STAGE")"
    VOLUME_NAME="$(python3 "$SCRIPT_DIR/release_invocation.py" query \
        --field buildroot_volume "$CAPSULE_INVOCATION_STAGE")"
    IMAGE_NAME="$(python3 "$SCRIPT_DIR/release_docker_resources.py" \
        query-builder-tag "$CAPSULE_INVOCATION_STAGE")"
    EXPECTED_CAPSULE_PROJECT="$(cd "$CAPSULE_SOURCE_TREE/DCENT_OS_Antminer" && pwd)"
    if [ "$PROJECT_DIR" != "$EXPECTED_CAPSULE_PROJECT" ]; then
        echo "ERROR: capsule packaging driver is not executing from the authenticated source snapshot" >&2
        exit 1
    fi
    python3 "$SCRIPT_DIR/release_result_stage.py" verify \
        --invocation-stage "$CAPSULE_INVOCATION_STAGE" \
        "$CAPSULE_RESULT_STAGE" >/dev/null
    VERIFIED_CAPSULE_RESULT_ROOT="$(python3 "$SCRIPT_DIR/release_result_stage.py" query \
        --field result_root \
        --invocation-stage "$CAPSULE_INVOCATION_STAGE" \
        "$CAPSULE_RESULT_STAGE")"
    CAPSULE_RESULT_ROOT="$(dcent_capsule_shell_path "$CAPSULE_RESULT_ROOT")"
    VERIFIED_CAPSULE_RESULT_ROOT="$(dcent_capsule_shell_path "$VERIFIED_CAPSULE_RESULT_ROOT")"
    if [ "$CAPSULE_RESULT_ROOT" != "$VERIFIED_CAPSULE_RESULT_ROOT" ]; then
        echo "ERROR: capsule result root does not match the invocation-bound result stage" >&2
        exit 1
    fi
    CAPSULE_RELEASE_SET_STAGE="$(python3 "$SCRIPT_DIR/release_set_publication.py" query \
        --field stage-path < "$CAPSULE_RELEASE_SET_CAPABILITY_FILE")"
    CAPSULE_RELEASE_SET_STAGE="$(dcent_capsule_shell_path "$CAPSULE_RELEASE_SET_STAGE")"
    python3 "$SCRIPT_DIR/build_input_snapshot.py" verify \
        --target cargo-workspace "$CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT" >/dev/null
    DCENT_SOURCE_COMMIT="$DCENT_CAPSULE_SOURCE_COMMIT"
    DCENT_SOURCE_TREE_STATE="exact_git_object_snapshot"
    SOURCE_DATE_EPOCH="$(git -C "$GIT_OBJECT_REPO" show -s --format=%ct "$DCENT_SOURCE_COMMIT")"
    DCENT_CREATED_AT_UTC="$(python3 - "$SOURCE_DATE_EPOCH" <<'PY'
import datetime
import sys
print(datetime.datetime.fromtimestamp(int(sys.argv[1]), datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))
PY
)"
    DCENT_BUILD_ARCH="$BUILD_ARCH"
    DCENT_TOOLCHAIN_ID="linaro-7.2.1:${TOOLCHAIN_FILE}"
    DCENT_REQUIRE_RELEASE_PROVENANCE=1
    export DCENT_SOURCE_COMMIT DCENT_SOURCE_TREE_STATE SOURCE_DATE_EPOCH
    export DCENT_CREATED_AT_UTC DCENT_BUILD_ARCH DCENT_TOOLCHAIN_ID
fi

# Direct packaging used mutable checkout, Cargo, and Buildroot authorities that
# cannot satisfy receipt-v4 lineage. Reject it before provenance scans,
# snapshots, locks, or Docker access; a future lab lane needs its own explicit
# non-release capsule instead of inheriting this production driver.
if [ "$CAPSULE_MODE" != "1" ]; then
    echo "ERROR: direct Buildroot packaging is disabled until a separate lab capsule exists" >&2
    echo "       use scripts/build_s9_release_capsule.sh for the admitted S9 release lane" >&2
    exit 2
fi
# shellcheck source=lib/sd_image_signing_gate.sh
. "$SCRIPT_DIR/lib/sd_image_signing_gate.sh"
. "$SCRIPT_DIR/lib/release_envelope.sh"
dcent_run_docker_resource_spec() {
    local spec_json=$1
    local -a spec_argv=()
    mapfile -d '' -t spec_argv < <(
        printf '%s\n' "$spec_json" \
            | python3 "$SCRIPT_DIR/release_docker_resources.py" \
                emit-argv0 "$CAPSULE_INVOCATION_STAGE"
    )
    if [ "${#spec_argv[@]}" -eq 0 ] || [ "${spec_argv[0]}" != "docker" ]; then
        echo "ERROR: Docker resource helper emitted no executable exact argv" >&2
        return 1
    fi
    "${spec_argv[@]}"
}
KB_DIR="${DCENT_SOC_BOOT_DIR:-$PROJECT_DIR/vendor/soc-boot}"
if [ "$CAPSULE_MODE" = "1" ]; then
    WIN_BIN="$CAPSULE_RESULT_ROOT/target/$BUILD_ARCH/release/dcentrald"
    RUST_METADATA_FILE="$CAPSULE_RESULT_ROOT/target/release-inventory/${BUILD_ARCH}.metadata.json"
else
    WIN_BIN="$PROJECT_DIR/dcentrald/target/$BUILD_ARCH/release/dcentrald"
    RUST_METADATA_FILE="$PROJECT_DIR/dcentrald/target/release-inventory/${BUILD_ARCH}.metadata.json"
fi

# Snapshot every target-scoped gitignored payload before Docker can consume it.
# cv1835-s19jpro intentionally fails here: its defconfig has no kernel producer
# while its post-image consumer requires uImage, so a warm-volume uImage must
# never silently become a newly signed package input.
command -v python3 >/dev/null 2>&1 || {
    echo "ERROR: python3 is required for out-of-band build-input snapshotting" >&2
    exit 1
}
DCENT_REQUIRE_RELEASE_PROVENANCE="${DCENT_REQUIRE_RELEASE_PROVENANCE:-0}"
if [ "$CAPSULE_MODE" != "1" ]; then
    dcent_prepare_git_release_provenance \
        "$REPO_ROOT" \
        "$DCENT_PACKAGE_STATUS" \
        "$TARGET" \
        "$BUILD_ARCH" \
        "linaro-7.2.1:${TOOLCHAIN_FILE}"
fi
export DCENT_REQUIRE_RELEASE_PROVENANCE
if dcent_release_provenance_required; then
    [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ] || {
        echo "ERROR: release build requires DCENT_RELEASE_SIGNING_KEY" >&2
        exit 1
    }
    [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ] || {
        echo "ERROR: release build requires DCENT_RELEASE_PUBKEY_FILE" >&2
        exit 1
    }
    bash "$SCRIPT_DIR/verify_release_keypair.sh" \
        "$DCENT_RELEASE_SIGNING_KEY" \
        "$DCENT_RELEASE_PUBKEY_FILE" \
        "$DCENT_MANIFEST_PUBLIC_KEY_HEX" >/dev/null
fi
if [ -n "$OUTPUT_DIR_OVERRIDE" ]; then
    case "$OUTPUT_DIR_OVERRIDE" in
        /*|[A-Za-z]:*) OUTPUT_DIR="$OUTPUT_DIR_OVERRIDE" ;;
        *) OUTPUT_DIR="$PROJECT_DIR/$OUTPUT_DIR_OVERRIDE" ;;
    esac
else
    if [ "$CAPSULE_MODE" = "1" ]; then
        echo "ERROR: capsule packaging requires --output-dir for its private release-set stage" >&2
        exit 1
    fi
    OUTPUT_DIR="$PROJECT_DIR/output"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
if [ "$CAPSULE_MODE" = "1" ] && [ "$OUTPUT_DIR" != "$CAPSULE_RELEASE_SET_STAGE" ]; then
    echo "ERROR: capsule output is not the capability-owned release-set stage" >&2
    exit 1
fi

# Legacy packaging still shares one volume/output namespace. Capsule packaging
# owns a fresh volume and private publication stage, so it neither needs nor may
# mutate a lock below the sealed source snapshot.
BUILD_LOCK_DIR="$PROJECT_DIR/output/.dcentos-build.lock"
BUILD_LOCK_HELD=0
if [ "$CAPSULE_MODE" != "1" ]; then
    mkdir -p "$PROJECT_DIR/output"
    dcent_release_build_lock_acquire "$BUILD_LOCK_DIR"
    BUILD_LOCK_HELD=1
fi
dcent_release_early_unlock() {
    status=$?
    trap - EXIT
    if [ "$BUILD_LOCK_HELD" = "1" ]; then
        dcent_release_build_lock_release "$BUILD_LOCK_DIR" >/dev/null 2>&1 || true
    fi
    exit "$status"
}
trap dcent_release_early_unlock EXIT

# Resolve and pre-clean the exact release publication names before any build
# work. These are tracked independently from the legacy artifact sidecars so
# the EXIT trap can retract every release-looking alias and metadata file if a
# later signer or private-stage cleanup fails.
RELEASE_NAME=""
RELEASE_EXT="tar"
case "$TARBALL_NAME" in *.img) RELEASE_EXT="img" ;; esac
RELEASE_GRADE_PATH=""
RELEASE_LAB_PATH=""
RELEASE_METADATA_PATH=""
RELEASE_COPY=""
RELEASE_DATE_STAMP="$(
    printf '%s\n' "$DCENT_CREATED_AT_UTC" \
        | awk -F'T' '{ gsub(/-/, "", $1); print $1 }'
)"
if release_name_candidate="$(
    sh "$SCRIPT_DIR/firmware_release_name.sh" \
        "$TARGET" "${DCENT_RELEASE_CHANNEL:-beta}" "$RELEASE_DATE_STAMP" 2>/dev/null
)"; then
    case "$release_name_candidate" in
        ""|.|..|*/*|*\\*)
            echo "ERROR: firmware release name is not a canonical flat basename" >&2
            exit 1
            ;;
    esac
    RELEASE_NAME="$release_name_candidate"
    RELEASE_GRADE_PATH="$OUTPUT_DIR/${RELEASE_NAME}.${RELEASE_EXT}"
    RELEASE_LAB_PATH="$OUTPUT_DIR/${RELEASE_NAME}-LAB-UNSIGNED-NOT-FOR-RELEASE.${RELEASE_EXT}"
    RELEASE_METADATA_PATH="$OUTPUT_DIR/${RELEASE_NAME}.release.txt"
    dcent_release_require_publication_absent \
        "$OUTPUT_DIR" "$RELEASE_NAME" "$RELEASE_EXT"
else
    if dcent_release_provenance_required; then
        echo "ERROR: release build has no canonical publication name for target '$TARGET'" >&2
        exit 1
    fi
fi

dcent_remove_prebuilt_rust_sidecars() {
    sidecar_prefix="$1"
    for sidecar_binary in dcentrald dcentos-init dcentos-discovery; do
        rm -f -- \
            "$OUTPUT_DIR/${sidecar_prefix}.prebuilt-rust.${sidecar_binary}.bin" \
            "$OUTPUT_DIR/${sidecar_prefix}.prebuilt-rust.${sidecar_binary}.build-receipt.json"
    done
}

# Evidence sidecars authenticate one exact artifact generation. Clear the
# previous generation before any build or packaging mutation so a failed run
# cannot leave stale legal/Rust/closure evidence next to newly written (or
# partially written) firmware bytes. Failure to remove a non-regular collision
# is fatal under set -e; it must not be silently overwritten as evidence.
rm -f -- \
    "$OUTPUT_DIR/${TARBALL_NAME}.buildroot-legal.json" \
    "$OUTPUT_DIR/${TARBALL_NAME}.rust-dependencies.json" \
    "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json" \
    "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json.sig" \
    "$OUTPUT_DIR/release-packaging-input.json" \
    "$OUTPUT_DIR/${TARBALL_NAME}.sig"
dcent_remove_prebuilt_rust_sidecars "$TARBALL_NAME"

BUILD_INPUT_STAGE=""
BUILD_INPUT_SNAPSHOT=""
BUILD_INPUT_DESTROY_TOKEN=""
BUILD_INPUT_MOUNT_ARGS=()
BINARY_EXPORT_PARENT=""
BINARY_EXPORT_STAGE=""
BINARY_EXPORT_CAPABILITY=""
BINARY_EXPORT_MOUNT_ARGS=()
BINARY_EXPORT_REQUIRED=""
BINARY_EXPORT_DCENTRALD_PATH=""
BINARY_EXPORT_DCENTRALD_SHA256=""
BINARY_EXPORT_INIT_PATH=""
BINARY_EXPORT_INIT_SHA256=""
BINARY_EXPORT_DISCOVERY_PATH=""
BINARY_EXPORT_DISCOVERY_SHA256=""
PREBUILT_RUST_INPUT_ARGS=()
PREBUILT_RUST_INPUT_COUNT=0
CAPSULE_BUILDROOT_VOLUME_CREATED=0
CAPSULE_BUILDROOT_VOLUME_INSPECT=""
CAPSULE_IMAGE_TAG_CREATED=0
CAPSULE_RETAINED_IMAGE_TAG=""

dcent_cleanup_failed_release_evidence() {
    status=$?
    trap - EXIT
    # EXIT cleanup is best-effort across every independent artifact/stage. A
    # single non-removable collision must never let inherited `set -e` abort
    # before canonical publication retraction or private-stage destruction.
    set +e
    if [ "$CAPSULE_BUILDROOT_VOLUME_CREATED" = "1" ]; then
        current_volume_inspect="$(docker volume inspect -- "$VOLUME_NAME" 2>/dev/null)"
        if [ -z "$current_volume_inspect" ] || ! printf '%s\n' "$current_volume_inspect" \
            | python3 "$SCRIPT_DIR/release_docker_resources.py" verify-inspect \
                --role buildroot "$CAPSULE_INVOCATION_STAGE" >/dev/null; then
            echo "ERROR: invocation Buildroot volume identity changed before cleanup" >&2
            [ "$status" -ne 0 ] || status=1
        else
            invocation_cleanup_capability="$(python3 "$SCRIPT_DIR/release_invocation.py" query \
                --field capability "$CAPSULE_INVOCATION_STAGE")"
            destroy_volume_spec="$(printf '%s\n' "$current_volume_inspect" \
                | python3 "$SCRIPT_DIR/release_docker_resources.py" destroy-spec \
                    --role buildroot \
                    --capability "$invocation_cleanup_capability" \
                    --empty-or-disposable disposable \
                    "$CAPSULE_INVOCATION_STAGE")"
            if [ -z "$destroy_volume_spec" ] || ! dcent_run_docker_resource_spec \
                "$destroy_volume_spec" >/dev/null; then
                echo "ERROR: failed to remove exact invocation Buildroot volume" >&2
                [ "$status" -ne 0 ] || status=1
            else
                CAPSULE_BUILDROOT_VOLUME_CREATED=0
            fi
        fi
    fi
    if [ "$CAPSULE_IMAGE_TAG_CREATED" = "1" ]; then
        retained_image_id="$(docker image inspect --format '{{.Id}}' \
            "$CAPSULE_RETAINED_IMAGE_TAG" 2>/dev/null)"
        unique_image_id="$(docker image inspect --format '{{.Id}}' \
            "$IMAGE_NAME" 2>/dev/null)"
        if [ -z "$retained_image_id" ] || [ "$retained_image_id" != "$unique_image_id" ]; then
            echo "ERROR: refusing to remove unique builder tag without its retained content-addressed reference" >&2
            [ "$status" -ne 0 ] || status=1
        elif ! docker image rm "$IMAGE_NAME" >/dev/null 2>&1; then
            echo "ERROR: failed to remove exact invocation builder tag" >&2
            [ "$status" -ne 0 ] || status=1
        else
            CAPSULE_IMAGE_TAG_CREATED=0
        fi
    fi
    if [ "$status" -ne 0 ]; then
        if [ -n "$RELEASE_NAME" ] && ! dcent_release_remove_publication \
            "$OUTPUT_DIR" "$RELEASE_NAME" "$RELEASE_EXT"; then
            echo "ERROR: failed to retract release publication set" >&2
        fi
        rm -f -- \
            "$OUTPUT_DIR/${TARBALL_NAME}.buildroot-legal.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.rust-dependencies.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json.sig" \
            "$OUTPUT_DIR/release-packaging-input.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.sig"
        dcent_remove_prebuilt_rust_sidecars "$TARBALL_NAME"
    fi
    if [ -n "$BINARY_EXPORT_STAGE" ]; then
        if [ -z "$BINARY_EXPORT_CAPABILITY" ]; then
            BINARY_EXPORT_CAPABILITY="$(
                python3 "$SCRIPT_DIR/binary_build_receipt.py" \
                    export-snapshot-capability-path --stage "$BINARY_EXPORT_STAGE" \
                    2>/dev/null
            )" || true
        fi
        if [ -z "$BINARY_EXPORT_CAPABILITY" ] || ! \
            python3 "$SCRIPT_DIR/binary_build_receipt.py" \
                destroy-export-snapshot-set \
                --stage "$BINARY_EXPORT_STAGE" \
                --capability "$BINARY_EXPORT_CAPABILITY" >/dev/null 2>&1; then
            echo "ERROR: failed to remove private binary export set: $BINARY_EXPORT_STAGE" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    if [ -n "$BINARY_EXPORT_PARENT" ]; then
        capability_directory="$BINARY_EXPORT_PARENT/.dcent-export-capabilities"
        if { [ -e "$capability_directory" ] || [ -L "$capability_directory" ]; } && \
            ! rmdir -- "$capability_directory" 2>/dev/null; then
            echo "ERROR: failed to remove binary export capability directory: $capability_directory" >&2
            [ "$status" -ne 0 ] || status=1
        fi
        if ! rmdir -- "$BINARY_EXPORT_PARENT" 2>/dev/null; then
            echo "ERROR: failed to remove private binary export parent: $BINARY_EXPORT_PARENT" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    if [ -n "$BUILD_INPUT_SNAPSHOT" ]; then
        python3 "$SCRIPT_DIR/build_input_snapshot.py" destroy \
            --token "$BUILD_INPUT_DESTROY_TOKEN" "$BUILD_INPUT_SNAPSHOT" \
            >/dev/null 2>&1 || {
            echo "ERROR: failed to remove private build-input snapshot: $BUILD_INPUT_STAGE" >&2
            [ "$status" -ne 0 ] || status=1
        }
    fi
    # Cleanup itself is part of release success. If private-stage destruction
    # changed a previously successful status to failure, remove every evidence
    # sidecar now so a failed invocation cannot leave publishable-looking proof.
    if [ "$status" -ne 0 ]; then
        if [ -n "$RELEASE_NAME" ] && ! dcent_release_remove_publication \
            "$OUTPUT_DIR" "$RELEASE_NAME" "$RELEASE_EXT"; then
            echo "ERROR: failed to retract release publication set" >&2
        fi
        rm -f -- \
            "$OUTPUT_DIR/${TARBALL_NAME}.buildroot-legal.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.rust-dependencies.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json.sig" \
            "$OUTPUT_DIR/release-packaging-input.json" \
            "$OUTPUT_DIR/${TARBALL_NAME}.sig"
        dcent_remove_prebuilt_rust_sidecars "$TARBALL_NAME"
    fi
    # Lock release is the last shared-state mutation. A subsequent invocation
    # must not enter while this trap can still retract aliases or sidecars.
    if [ "$BUILD_LOCK_HELD" = "1" ]; then
        if dcent_release_build_lock_release "$BUILD_LOCK_DIR"; then
            BUILD_LOCK_HELD=0
        else
            echo "ERROR: failed to release shared build/output lock: $BUILD_LOCK_DIR" >&2
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    exit "$status"
}
trap dcent_cleanup_failed_release_evidence EXIT

# Materialize target-selected external bytes once. All supported S9/AM2
# consumers below read this private, read-only stage instead of reopening the
# live repository paths declared by the manifest. This does not snapshot or
# claim closure over the project source tree.
BUILD_INPUT_REPO_ROOT="$REPO_ROOT"
if [ "$CAPSULE_MODE" = "1" ]; then
    BUILD_INPUT_REPO_ROOT="$CAPSULE_EXTERNAL_INPUT_ROOT"
fi
BUILD_INPUT_SELECTION_ARGS=()
if [ "$CAPSULE_MODE" = "1" ]; then
    BUILD_INPUT_SELECTION_ARGS=(--selection-root "$REPO_ROOT")
fi
BUILD_INPUT_CREATE_RESULT="$(python3 "$SCRIPT_DIR/build_input_snapshot.py" create \
    --repo-root "$BUILD_INPUT_REPO_ROOT" \
    --build-input-manifest "$SCRIPT_DIR/build_inputs.manifest" \
    "${BUILD_INPUT_SELECTION_ARGS[@]}" \
    --target "$TARGET")"
snapshot_result_field() {
    printf '%s\n' "$BUILD_INPUT_CREATE_RESULT" \
        | python3 "$SCRIPT_DIR/build_input_snapshot.py" query-result "$@"
}
BUILD_INPUT_STAGE="$(snapshot_result_field --field stage)"
BUILD_INPUT_SNAPSHOT="$(snapshot_result_field --field snapshot)"
BUILD_INPUT_DESTROY_TOKEN="$(snapshot_result_field --field destroy_token)"
if command -v cygpath >/dev/null 2>&1; then
    DOCKER_BUILD_INPUT_STAGE="$(cygpath -w "$BUILD_INPUT_STAGE")"
else
    DOCKER_BUILD_INPUT_STAGE="$BUILD_INPUT_STAGE"
fi
BUILD_INPUT_MOUNT_ARGS=(
    -v "${DOCKER_BUILD_INPUT_STAGE}:/dcent-inputs:ro"
)

# Place the binary export beside the already Docker-shareable out-of-band
# snapshot, but in its own private parent. The export helper owns every entry
# below this directory and cleanup uses its out-of-stage capability.
BINARY_EXPORT_PARENT="$(python3 - "$BUILD_INPUT_STAGE" <<'PY'
import os
from pathlib import Path
import sys
import tempfile

shareable_parent = Path(sys.argv[1]).parent
private_parent = Path(
    tempfile.mkdtemp(prefix="dcent-binary-export-parent-", dir=shareable_parent)
)
os.chmod(private_parent, 0o700)
print(private_parent)
PY
)"
dcent_validate_docker_transport_path "$BINARY_EXPORT_PARENT" || {
    echo "ERROR: private binary export parent is unsafe for Docker transport" >&2
    exit 1
}

# Release signing keys are host files. Docker build stages must see them via
# stable container paths; passing a Windows/WSL host path through the env is not
# enough because Buildroot post-build/post-image scripts run inside containers.
SIGNING_MOUNT_ARGS=()
PUBKEY_MOUNT_ARGS=()
CONTAINER_RELEASE_SIGNING_KEY="${DCENT_RELEASE_SIGNING_KEY:-}"
CONTAINER_RELEASE_PUBKEY_FILE="${DCENT_RELEASE_PUBKEY_FILE:-}"
if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
    dcent_require_absolute_host_path "$DCENT_RELEASE_SIGNING_KEY" || {
        echo "ERROR: DCENT_RELEASE_SIGNING_KEY must be an absolute host path" >&2
        exit 1
    }
    if [ ! -f "$DCENT_RELEASE_SIGNING_KEY" ]; then
        echo "ERROR: DCENT_RELEASE_SIGNING_KEY points to missing file: $DCENT_RELEASE_SIGNING_KEY" >&2
        exit 1
    fi
    if command -v cygpath >/dev/null 2>&1; then
        SIGN_KEY_MOUNT="$(cygpath -w "$DCENT_RELEASE_SIGNING_KEY")"
    else
        SIGN_KEY_MOUNT="$DCENT_RELEASE_SIGNING_KEY"
    fi
    dcent_validate_docker_transport_path "$SIGN_KEY_MOUNT" || {
        echo "ERROR: release signing-key path is unsafe for Docker transport" >&2
        exit 1
    }
    SIGNING_MOUNT_ARGS+=(-v "${SIGN_KEY_MOUNT}:/dcent-release-signing-key.pem:ro")
    CONTAINER_RELEASE_SIGNING_KEY="/dcent-release-signing-key.pem"
fi
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    dcent_require_absolute_host_path "$DCENT_RELEASE_PUBKEY_FILE" || {
        echo "ERROR: DCENT_RELEASE_PUBKEY_FILE must be an absolute host path" >&2
        exit 1
    }
    if [ ! -f "$DCENT_RELEASE_PUBKEY_FILE" ]; then
        echo "ERROR: DCENT_RELEASE_PUBKEY_FILE points to missing file: $DCENT_RELEASE_PUBKEY_FILE" >&2
        exit 1
    fi
    if command -v cygpath >/dev/null 2>&1; then
        RELEASE_PUBKEY_MOUNT="$(cygpath -w "$DCENT_RELEASE_PUBKEY_FILE")"
    else
        RELEASE_PUBKEY_MOUNT="$DCENT_RELEASE_PUBKEY_FILE"
    fi
    dcent_validate_docker_transport_path "$RELEASE_PUBKEY_MOUNT" || {
        echo "ERROR: release public-key path is unsafe for Docker transport" >&2
        exit 1
    }
    SIGNING_MOUNT_ARGS+=(-v "${RELEASE_PUBKEY_MOUNT}:/dcent-release-ed25519.pub:ro")
    PUBKEY_MOUNT_ARGS+=(-v "${RELEASE_PUBKEY_MOUNT}:/dcent-release-ed25519.pub:ro")
    CONTAINER_RELEASE_PUBKEY_FILE="/dcent-release-ed25519.pub"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
fi

# -------- Phase 0: staged binary freshness guard (build-hygiene) --------
# This pipeline stages pre-built Rust binaries (Phase 5) and does not
# recompile them. Fail early if dcentrald, dcentos-init, or BB/CV discovery would
# ship stale or missing.
if ! dcent_phase0_stale_binary_guard; then
    exit 1
fi
dcent_validate_docker_transport_path "$BINARY_EXPORT_STAGE" || {
    echo "ERROR: private binary export stage is unsafe for Docker transport" >&2
    exit 1
}
if command -v cygpath >/dev/null 2>&1; then
    DOCKER_BINARY_EXPORT_STAGE="$(cygpath -w "$BINARY_EXPORT_STAGE")"
else
    DOCKER_BINARY_EXPORT_STAGE="$BINARY_EXPORT_STAGE"
fi
dcent_validate_docker_transport_path "$DOCKER_BINARY_EXPORT_STAGE" || {
    echo "ERROR: converted binary export stage is unsafe for Docker transport" >&2
    exit 1
}
BINARY_EXPORT_MOUNT_ARGS=(
    -v "${DOCKER_BINARY_EXPORT_STAGE}:/dcent-binaries:ro"
)
for binary_name in $(dcent_required_prebuilt_binaries); do
    binary_export_record="$(
        python3 "$SCRIPT_DIR/binary_build_receipt.py" \
            query-export-snapshot-path \
            --stage "$BINARY_EXPORT_STAGE" \
            --binary-name "$binary_name" \
            --artifact binary \
            --field path-sha256
    )"
    IFS=' ' read -r binary_relative_path binary_sha256 binary_record_extra \
        <<<"$binary_export_record"
    if [ -n "$binary_record_extra" ] || \
        [ "$binary_export_record" != "$binary_relative_path $binary_sha256" ]; then
        echo "ERROR: helper returned a malformed export path/digest record for $binary_name" >&2
        exit 1
    fi
    case "$binary_relative_path" in
        artifacts/[0-9][0-9][0-9][0-9]/"$binary_name") ;;
        *)
            echo "ERROR: helper returned unsafe export path for $binary_name: $binary_relative_path" >&2
            exit 1
            ;;
    esac
    case "$binary_sha256" in
        *[!0-9a-f]*|'')
            echo "ERROR: helper returned malformed SHA256 for $binary_name" >&2
            exit 1
            ;;
    esac
    if [ "${#binary_sha256}" -ne 64 ]; then
        echo "ERROR: helper returned non-64-byte SHA256 for $binary_name" >&2
        exit 1
    fi
    case "$binary_name" in
        dcentrald)
            BINARY_EXPORT_DCENTRALD_PATH="$binary_relative_path"
            BINARY_EXPORT_DCENTRALD_SHA256="$binary_sha256"
            ;;
        dcentos-init)
            BINARY_EXPORT_INIT_PATH="$binary_relative_path"
            BINARY_EXPORT_INIT_SHA256="$binary_sha256"
            ;;
        dcentos-discovery)
            BINARY_EXPORT_DISCOVERY_PATH="$binary_relative_path"
            BINARY_EXPORT_DISCOVERY_SHA256="$binary_sha256"
            ;;
        *)
            echo "ERROR: unsupported required prebuilt binary name: $binary_name" >&2
            exit 1
            ;;
    esac
    if [ -n "$BINARY_EXPORT_REQUIRED" ]; then
        BINARY_EXPORT_REQUIRED="$BINARY_EXPORT_REQUIRED $binary_name"
    else
        BINARY_EXPORT_REQUIRED="$binary_name"
    fi
done

echo "==================================================================="
echo "DCENT_OS Docker build pipeline — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "==================================================================="
echo "Project:      $PROJECT_DIR"
echo "Repo root:    $REPO_ROOT"
echo "Image:        $IMAGE_NAME"
echo "Volume:       $VOLUME_NAME"
echo "Prebuilt bin: $WIN_BIN"
echo "Target:       $TARGET"
echo "Defconfig:    $BR_DEFCONFIG"
# F-2 (Sweep-v3 PR-079): fail fast on a TARGET-arm -> defconfig-name
# drift. The per-product defconfig the arm just named must exist under
# the BR2_EXTERNAL configs dir BEFORE the Docker/Buildroot run consumes
# it (the in-Docker fragment merge derives `<base>_full_defconfig` FROM
# it, so a missing base defconfig otherwise fails opaquely deep inside
# Buildroot). POSIX sh (BusyBox ash safe). PROJECT_DIR is resolved above.
if [ ! -f "$PROJECT_DIR/br2_external_dcentos/configs/$BR_DEFCONFIG" ]; then
    echo "ERROR: defconfig '$BR_DEFCONFIG' for target '$TARGET' not found at" >&2
    echo "       $PROJECT_DIR/br2_external_dcentos/configs/$BR_DEFCONFIG" >&2
    echo "       (build_in_docker.sh TARGET arm -> defconfig-name drift; fix the arm)" >&2
    exit 1
fi
echo "Board name:   $BOARD_PKG_NAME"
echo "Output:       $OUTPUT_DIR/$TARBALL_NAME"
echo "Package mode: status=$DCENT_PACKAGE_STATUS allow_unsigned=$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"
echo ""

# -------- Phase 0: sanity --------
command -v docker >/dev/null 2>&1 || { echo "ERROR: docker not in PATH"; exit 1; }
docker info >/dev/null 2>&1 || { echo "ERROR: Docker daemon not responding"; exit 1; }
[ -f "$WIN_BIN" ] || { echo "ERROR: prebuilt $BUILD_ARCH binary missing: $WIN_BIN"; exit 1; }
if [ "$CAPSULE_MODE" != "1" ]; then
    [ -d "$KB_DIR" ] || { echo "ERROR: SoC boot inputs missing: $KB_DIR"; echo "       A full flashable image needs non-redistributable boot components"; echo "       (kernel / FPGA / U-Boot). See DEVELOPMENT.md, or flash a prebuilt signed release image."; exit 1; }
    # kernel.bin probe is S9-only. am2-s19jpro and am3-s19kpro use a board
    # post-image.sh that probes its own kernel paths (env override, sibling
    # fallback). Don't fail the host-side sanity gate for them.
    if [ "$BOARD_POST_IMAGE" != "board-script" ]; then
        [ -f "$KB_DIR/kernel.bin" ] || { echo "ERROR: kernel.bin missing in $KB_DIR"; exit 1; }
    fi
fi

# -------- Phase 1: build image --------
echo "--- Phase 1: docker build (cached after first run) ---"
# Docker Desktop on Windows + Git-Bash rejects the /c/... POSIX form for the
# BUILD CONTEXT path (paths with spaces fail). Convert to Windows form via
# cygpath when available; fall back to the original on Linux/macOS.
if command -v cygpath >/dev/null 2>&1; then
    DOCKER_BUILD_CTX="$(cygpath -w "$PROJECT_DIR")"
    DOCKER_BUILD_DOCKERFILE="$(cygpath -w "$PROJECT_DIR/Dockerfile.build")"
else
    DOCKER_BUILD_CTX="$PROJECT_DIR"
    DOCKER_BUILD_DOCKERFILE="$PROJECT_DIR/Dockerfile.build"
fi
docker build -f "$DOCKER_BUILD_DOCKERFILE" -t "$IMAGE_NAME" "$DOCKER_BUILD_CTX"
if [ "$CAPSULE_MODE" = "1" ]; then
    CAPSULE_IMAGE_TAG_CREATED=1
fi
BUILD_CONTAINER_ID="$(docker image inspect --format '{{.Id}}' "$IMAGE_NAME")"
case "$BUILD_CONTAINER_ID" in
    sha256:[0-9a-f][0-9a-f]*) ;;
    *)
        echo "ERROR: Docker returned a non-immutable build image identity: $BUILD_CONTAINER_ID" >&2
        exit 1
        ;;
esac
if [ "$CAPSULE_MODE" = "1" ]; then
    CAPSULE_RETAINED_IMAGE_TAG="dcentos-build-cache:${BUILD_CONTAINER_ID#sha256:}"
    docker image tag "$BUILD_CONTAINER_ID" "$CAPSULE_RETAINED_IMAGE_TAG"
    [ "$(docker image inspect --format '{{.Id}}' "$CAPSULE_RETAINED_IMAGE_TAG")" = "$BUILD_CONTAINER_ID" ] || {
        echo "ERROR: retained content-addressed builder tag does not name the inspected image" >&2
        exit 1
    }
fi
echo ""

# -------- Phase 2: volume --------
echo "--- Phase 2: volume ---"
if [ "$CAPSULE_MODE" = "1" ]; then
    if docker volume inspect -- "$VOLUME_NAME" >/dev/null 2>&1; then
        echo "ERROR: invocation-scoped Buildroot volume already exists: $VOLUME_NAME" >&2
        exit 1
    fi
    CAPSULE_VOLUME_CREATE_SPEC="$(python3 "$SCRIPT_DIR/release_docker_resources.py" \
        create-spec --role buildroot "$CAPSULE_INVOCATION_STAGE")"
    dcent_run_docker_resource_spec "$CAPSULE_VOLUME_CREATE_SPEC" >/dev/null
    CAPSULE_BUILDROOT_VOLUME_CREATED=1
    CAPSULE_VOLUME_INSPECT_SPEC="$(python3 "$SCRIPT_DIR/release_docker_resources.py" \
        inspect-spec --role buildroot "$CAPSULE_INVOCATION_STAGE")"
    CAPSULE_BUILDROOT_VOLUME_INSPECT="$(
        dcent_run_docker_resource_spec "$CAPSULE_VOLUME_INSPECT_SPEC"
    )"
    printf '%s\n' "$CAPSULE_BUILDROOT_VOLUME_INSPECT" \
        | python3 "$SCRIPT_DIR/release_docker_resources.py" verify-inspect \
            --role buildroot "$CAPSULE_INVOCATION_STAGE" >/dev/null
    echo "Created fresh invocation-scoped volume: $VOLUME_NAME"
elif docker volume inspect "$VOLUME_NAME" >/dev/null 2>&1; then
    echo "Reusing existing volume: $VOLUME_NAME"
else
    docker volume create "$VOLUME_NAME"
    echo "Created volume: $VOLUME_NAME"
fi
echo ""

# -------- Phase 2b: invalidate cached prebuilt binaries in volume --------
# A persistent volume can hold an older generation from a prior run. Purge the
# Purge every known prebuilt name and receipt before Phase 5 so target switching or a
# partial staging failure cannot expose an undeclared warm-volume generation.
echo "--- Phase 2b: invalidate cached prebuilt binaries in volume ---"
if [ "$CAPSULE_MODE" = "1" ]; then
    echo "Fresh capsule volume has no warm prebuilt state to invalidate"
else
docker run --rm \
    -e BUILD_ARCH="$BUILD_ARCH" \
    -e ALL_PREBUILT_BINARIES="dcentrald dcentos-init dcentos-discovery pic-recovery dspic-flash" \
    -v "${VOLUME_NAME}:/build" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -eu
        case "$BUILD_ARCH" in
            armv7-unknown-linux-musleabihf|aarch64-unknown-linux-musl) ;;
            *) echo "ERROR: unsafe BUILD_ARCH for binary staging: $BUILD_ARCH" >&2; exit 1 ;;
        esac
        release_dir=/build
        [ -d "$release_dir" ] && [ ! -L "$release_dir" ] || {
            echo "ERROR: Docker volume root is not a real directory" >&2
            exit 1
        }
        for component in dcentos dcentrald target "$BUILD_ARCH" release; do
            next="$release_dir/$component"
            if [ -L "$next" ] || { [ -e "$next" ] && [ ! -d "$next" ]; }; then
                echo "ERROR: unsafe persistent binary staging component: $next" >&2
                exit 1
            fi
            if [ ! -d "$next" ]; then
                mkdir -- "$next"
            fi
            [ -d "$next" ] && [ ! -L "$next" ] || {
                echo "ERROR: binary staging component changed during creation: $next" >&2
                exit 1
            }
            release_dir="$next"
        done
        for binary_name in $ALL_PREBUILT_BINARIES; do
            rm -f -- \
                "$release_dir/$binary_name" \
                "$release_dir/${binary_name}.build-receipt.json"
            echo "  $binary_name cache invalidated (or was already absent)"
        done
    '
fi
echo ""

# Docker Desktop on Windows accepts POSIX-style mounts for bash. Convert the
# Windows paths (which may contain spaces) into POSIX paths docker understands.
# In practice bash on Windows passes "/c/Users/..." which Docker Desktop rewrites
# to the host path, so we use the paths as-is.
POSIX_PROJECT_DIR="$PROJECT_DIR"
POSIX_OUTPUT_DIR="$OUTPUT_DIR"
for docker_transport_path in "$POSIX_PROJECT_DIR" "$POSIX_OUTPUT_DIR"; do
    dcent_validate_docker_transport_path "$docker_transport_path" || {
        echo "ERROR: project/output path is unsafe for Docker transport" >&2
        exit 1
    }
done

# -------- Phase 3: stage project tree into volume --------
echo "--- Phase 3: stage project tree into volume ---"
# Mirror the WSL script exclude list. Use rsync --delete so removed files
# disappear from the volume too. CRLF strip only our own shell scripts +
# buildroot external config (Windows editors may save with CRLF and bash
# chokes on \r in shebang lines and inside heredocs).
docker run --rm \
    -e CAPSULE_MODE="$CAPSULE_MODE" \
    -v "${VOLUME_NAME}:/build" \
    -v "${POSIX_PROJECT_DIR}:/src:ro" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -e
        mkdir -p /build/dcentos
        rsync -a --delete \
            --exclude=buildroot/ \
            --exclude=output/ \
            --exclude=dcentrald/target/ \
            --exclude=dashboard/node_modules/ \
            --exclude=.tmp_* \
            --exclude=*.log \
            --exclude=.git/ \
            /src/ /build/dcentos/
        if [ "$CAPSULE_MODE" != "1" ]; then
            # Compatibility-only normalization for the legacy live-tree lane.
            # Capsule releases consume Git-object bytes and modes unchanged.
            find /build/dcentos/scripts -type f -name "*.sh" \
                -exec sed -i "s/\r$//" {} + 2>/dev/null || true
            find /build/dcentos/br2_external_dcentos -type f \
                \( -name "*.sh" -o -name "S[0-9][0-9]*" -o -name "Config.in" -o -name "*.mk" -o -name "*defconfig*" -o -name "*.fragment" \) \
                -exec sed -i "s/\r$//" {} + 2>/dev/null || true
            chmod +x /build/dcentos/scripts/*.sh 2>/dev/null || true
            find /build/dcentos/br2_external_dcentos -type f -name "*.sh" \
                -exec chmod +x {} + 2>/dev/null || true
        fi
        DCENTOS_VERSION_FILE="/build/dcentos/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-version"
        DCENTOS_VERSION="$(sed -n "s/^[[:space:]]*//;s/[[:space:]]*$//;/^$/!{p;q;}" "$DCENTOS_VERSION_FILE")"
        [ -n "$DCENTOS_VERSION" ] || { echo "ERROR: empty DCENTOS_VERSION from $DCENTOS_VERSION_FILE" >&2; exit 1; }
        if [ "$CAPSULE_MODE" != "1" ]; then
            for version_path in \
                /build/dcentos/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/amlogic/am3-s19kpro/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/amlogic/am3-s21/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/amlogic/am3-s19jpro-aml/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/amlogic/am3-t21/rootfs-overlay/etc/dcentos-version \
                /build/dcentos/br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/dcentos-version
            do
                mkdir -p "$(dirname "$version_path")"
                printf "%s\n" "$DCENTOS_VERSION" > "$version_path"
            done
            echo "Stamped dcentos-version=$DCENTOS_VERSION into staged rootfs overlays"
        else
            echo "Capsule source staged without CRLF, mode, or version rewrites (version=$DCENTOS_VERSION)"
        fi
        echo "Staged $(du -sh /build/dcentos | cut -f1) into volume"
    '
echo ""

# -------- Phase 4: stage knowledge-base extractions --------
echo "--- Phase 4: stage knowledge-base/extractions ---"
# Only source-closure-supported targets reach this phase. Every byte comes from
# the exact target snapshot; unpinned fallback directories are intentionally
# unavailable even for lab packaging.
docker run --rm \
    -v "${VOLUME_NAME}:/build" \
    "${BUILD_INPUT_MOUNT_ARGS[@]}" \
    -e TARGET="$TARGET" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -e
        case "$TARGET" in
            s9)
                mkdir -p /build/dcentos/extractions/s9
                rsync -a --delete \
                    /dcent-inputs/files/knowledge-base/extractions/s9/ \
                    /build/dcentos/extractions/s9/
                echo "Staged s9: $(du -sh /build/dcentos/extractions/s9 | cut -f1)"
                ls -la /build/dcentos/extractions/s9/kernel.bin
                ;;
            am2-s19jpro|am2-s19jpro-sd|am2-s19pro)
                mkdir -p /build/dcentos/extractions/s19j
                rsync -a --delete \
                    /dcent-inputs/files/knowledge-base/extractions/s19j/ \
                    /build/dcentos/extractions/s19j/
                echo "Staged s19j: $(du -sh /build/dcentos/extractions/s19j | cut -f1)"
                ;;
            *)
                echo "ERROR: target $TARGET reached Phase 4 without an exact build-input staging policy" >&2
                exit 1
                ;;
        esac
    '
echo ""

# -------- Phase 5: stage pre-built ARM binary --------
echo "--- Phase 5: stage pre-built dcentrald binary ($BUILD_ARCH) ---"
# Consume only the exact target-required binaries captured by Phase 0. The
# mutable host target tree is deliberately absent from this packaging phase.
docker run --rm \
    -e BUILD_ARCH="$BUILD_ARCH" \
    -e TARGET="$TARGET" \
    -e REQUIRED_BINARIES="$BINARY_EXPORT_REQUIRED" \
    -e DCENTRALD_EXPORT_PATH="$BINARY_EXPORT_DCENTRALD_PATH" \
    -e DCENTRALD_EXPORT_SHA256="$BINARY_EXPORT_DCENTRALD_SHA256" \
    -e INIT_EXPORT_PATH="$BINARY_EXPORT_INIT_PATH" \
    -e INIT_EXPORT_SHA256="$BINARY_EXPORT_INIT_SHA256" \
    -e DISCOVERY_EXPORT_PATH="$BINARY_EXPORT_DISCOVERY_PATH" \
    -e DISCOVERY_EXPORT_SHA256="$BINARY_EXPORT_DISCOVERY_SHA256" \
    -e DCENT_MANIFEST_PUBLIC_KEY_HEX="${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" \
    -e DCENT_ALLOW_UNSIGNED_SYSUPGRADE="$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" \
    "${BINARY_EXPORT_MOUNT_ARGS[@]}" \
    -v "${VOLUME_NAME}:/build" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -eu
        case "$BUILD_ARCH" in
            armv7-unknown-linux-musleabihf|aarch64-unknown-linux-musl) ;;
            *) echo "ERROR: unsafe BUILD_ARCH for binary staging: $BUILD_ARCH" >&2; exit 1 ;;
        esac
        release_dir=/build
        [ -d "$release_dir" ] && [ ! -L "$release_dir" ] || {
            echo "ERROR: Docker volume root is not a real directory" >&2
            exit 1
        }
        for component in dcentos dcentrald target "$BUILD_ARCH" release; do
            next="$release_dir/$component"
            [ -d "$next" ] && [ ! -L "$next" ] || {
                echo "ERROR: unsafe persistent binary staging component: $next" >&2
                exit 1
            }
            release_dir="$next"
        done
        for binary_name in $REQUIRED_BINARIES; do
            case "$binary_name" in
                dcentrald)
                    relative_path="$DCENTRALD_EXPORT_PATH"
                    expected_sha256="$DCENTRALD_EXPORT_SHA256"
                    ;;
                dcentos-init)
                    relative_path="$INIT_EXPORT_PATH"
                    expected_sha256="$INIT_EXPORT_SHA256"
                    ;;
                dcentos-discovery)
                    relative_path="$DISCOVERY_EXPORT_PATH"
                    expected_sha256="$DISCOVERY_EXPORT_SHA256"
                    ;;
                *) echo "ERROR: unsafe required binary name: $binary_name" >&2; exit 1 ;;
            esac
            case "$relative_path" in
                artifacts/[0-9][0-9][0-9][0-9]/"$binary_name") ;;
                *) echo "ERROR: unsafe exported path for $binary_name" >&2; exit 1 ;;
            esac
            case "$expected_sha256" in
                *[!0-9a-f]*|'') echo "ERROR: malformed SHA256 for $binary_name" >&2; exit 1 ;;
            esac
            [ "${#expected_sha256}" -eq 64 ] || {
                echo "ERROR: non-64-byte SHA256 for $binary_name" >&2
                exit 1
            }
            source_path="/dcent-binaries/$relative_path"
            destination="$release_dir/$binary_name"
            [ -f "$source_path" ] && [ ! -L "$source_path" ] || {
                echo "ERROR: exported source is missing or linked: $source_path" >&2
                exit 1
            }
            [ ! -e "$destination" ] && [ ! -L "$destination" ] || {
                echo "ERROR: binary destination unexpectedly exists: $destination" >&2
                exit 1
            }
            source_sha256="$(sha256sum "$source_path" | cut -d" " -f1)"
            [ "$source_sha256" = "$expected_sha256" ] || {
                echo "ERROR: exported source digest mismatch for $binary_name" >&2
                exit 1
            }
            install -m 0755 "$source_path" "$destination"
            destination_sha256="$(sha256sum "$destination" | cut -d" " -f1)"
            [ "$destination_sha256" = "$expected_sha256" ] || {
                echo "ERROR: installed destination digest mismatch for $binary_name" >&2
                exit 1
            }
            file "$destination"
            ls -lh "$destination"
        done
        # W1.2 (2026-05-07): when a manifest pubkey was pinned, prove it is
        # actually embedded in the prebuilt binary. dcentrald-api uses
        # `option_env!("DCENT_MANIFEST_PUBLIC_KEY_HEX")` which only embeds
        # the hex string when the env var was set during `cargo build`. If
        # the build host did not export the same hex used here, the binary
        # silently falls back to fail-open. `strings | grep` catches that.
        if [ -n "${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" ]; then
            BIN_PATH="/build/dcentos/dcentrald/target/$BUILD_ARCH/release/dcentrald"
            if strings "$BIN_PATH" | grep -qF "$DCENT_MANIFEST_PUBLIC_KEY_HEX"; then
                echo "  manifest pubkey pin embedded in dcentrald binary (verified via strings)"
            else
                echo "ERROR: DCENT_MANIFEST_PUBLIC_KEY_HEX is set in this build environment" >&2
                echo "       but the prebuilt dcentrald binary does NOT contain that hex" >&2
                echo "       string — the binary was likely cargo-built without the env" >&2
                echo "       var exported. Re-run cargo build with the pin set:" >&2
                echo "       export DCENT_MANIFEST_PUBLIC_KEY_HEX=<hex64>" >&2
                echo "       cargo build --release --target $BUILD_ARCH" >&2
                exit 1
            fi
        fi
        # W13.C4: variant TARGET arms (W10) need version-stamp dispatch.
        # Without these, dcentrald version stamp is silently skipped on
        # these variants (the wildcard `*)` falls through to "" and the
        # `dcent_require_dcentrald_version_match` call below is bypassed),
        # so a stale binary in the variant overlay would ship to
        # sysupgrade tarballs unnoticed.
        case "$TARGET" in
            s9) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/zynq/rootfs-overlay" ;;
            # DEVOPS-011 (2026-06-02): am2-s19jpro-sd reuses the am2-s19jpro
            # defconfig + rootfs-overlay (only the post-image differs — it emits
            # a flashable .img instead of a sysupgrade .tar). Without this arm it
            # fell through to the wildcard "" and the dcentrald version-match
            # gate was silently skipped, so a stale binary in the shared
            # am2-s19jpro overlay could ship in the SD image unnoticed (same
            # W13.C4 hazard the other variants close). Point it at the same
            # overlay the SD build actually stages from.
            am2-s19jpro|am2-s19jpro-sd) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay" ;;
            # Added by Phase 2K — without these am2-s19pro / am2-s17pro fall
            # through to the wildcard "" and the version-match gate is
            # silently skipped, shipping a stale binary in the variant
            # overlay unnoticed (same W13.C4 hazard as the am3 variants).
            am2-s19pro) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay" ;;
            am2-s17pro) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay" ;;
            # End Phase 2K
            am3-s19kpro) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/amlogic/am3-s19kpro/rootfs-overlay" ;;
            am3-s21) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/amlogic/am3-s21/rootfs-overlay" ;;
            # Added by Phase 4B
            am3-s19jpro-aml) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/amlogic/am3-s19jpro-aml/rootfs-overlay" ;;
            am3-t21) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/amlogic/am3-t21/rootfs-overlay" ;;
            # End Phase 4B
            am3-bb) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay" ;;
            am3-bb-s19jpro|am3-bb-s19jpro-vnish) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay" ;;
            cv1835-s19jpro) VERSION_TARGET_DIR="/build/dcentos/br2_external_dcentos/board/cvitek/cv1835-s19jpro/rootfs-overlay" ;;
            *) VERSION_TARGET_DIR="" ;;
        esac
        if [ -n "$VERSION_TARGET_DIR" ]; then
            . /build/dcentos/scripts/lib/dcentrald_version_gate.sh
            dcent_require_dcentrald_version_match \
                "$VERSION_TARGET_DIR" \
                "/build/dcentos/dcentrald/target/$BUILD_ARCH/release/dcentrald" \
                "build_in_docker Phase 5 ($TARGET)" \
                "/build/dcentos/dcentrald/Cargo.toml"
        fi
    '
echo ""

# -------- Phase 5b: pre-cache Linaro toolchain --------
# Buildroot's wget pulls the ~104 MB Linaro GCC tarball from releases.linaro.org.
# Their S3 throttles long-running slow connections hard (observed 7 KB/s sticky
# state vs 1+ MB/s fresh). Pre-fetching with curl in a short-lived container
# sidesteps the issue and saves hours.
#
# DEVOPS-002 (supply-chain, 2026-06-02): SHA256 PIN + VERIFICATION.
# Previously the only integrity check was a size heuristic
# (`stat -c%s > 100000000`). A size heuristic cannot detect a tampered or
# corrupted toolchain tarball — releases.linaro.org is plain HTTPS off an S3
# bucket with no in-band signature check, so a MITM / mirror-poisoning /
# silent-corruption of the GCC binary that compiles every shipped dcentrald
# would pass undetected. This adds a real cryptographic pin.
#
# The two distinct toolchain artifacts (one per arch) are pinned by file name:
#   - gcc-linaro-7.2.1-2017.11-x86_64_arm-linux-gnueabihf.tar.xz  (armv7 Zynq/BB)
#   - gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz    (aarch64 Amlogic)
#
# Expected hashes are the published Linaro 7.2-2017.11 release checksums
# (releases.linaro.org/components/toolchain/binaries/7.2-2017.11/<arch>/).
# They are recorded here as the SINGLE SOURCE OF TRUTH; the verification step
# computes `sha256sum` of the downloaded tarball and FAILS CLOSED on mismatch.
#
# Release builds enforce the two values below. Confirm each against the upstream
# published checksum exactly once on a clean fetch (for example,
# `curl -fsSL "$TOOLCHAIN_URL" | sha256sum` from a trusted network), then set
# DCENT_TOOLCHAIN_SHA256_VERIFIED=1 so the release evidence records the
# operator-confirmed pin. Dev/lab builds keep the advisory path for iteration;
# release builds fail closed on a missing or mismatched pin.
# ARMHF pin CORRECTED 2026-07-09 (RC rebuild). The prior value
# ba00410f...d4d4f was a stale placeholder that was never operator-verified
# (DCENT_TOOLCHAIN_SHA256_VERIFIED stayed unset because a clean release build
# would have failed it — and did). Linaro's upstream URL now 404s, so the
# authentic checksum was cross-verified against Buildroot's
# toolchain-external-linaro-arm.hash at tags 2018.02 AND 2018.05 (independent),
# both of which pin this exact filename to cee0087b... — matching the on-disk
# toolchain that built the shipped 20260617 beta (xz-integrity + gcc-7.2.1 OK).
TOOLCHAIN_SHA256_ARMHF="cee0087b1f1205b73996651b99acd3a926d136e71047048f1758ffcec69b1ca2"
# NOTE (follow-up): TOOLCHAIN_SHA256_AARCH64 below was likewise never
# operator-verified and is out of scope for the armv7 S9/S19jPro RC. Re-verify
# it against Buildroot's aarch64 .hash before any Amlogic release build.
# EVIDENCE 2026-07-09: Buildroot 2018.02 toolchain-external-linaro-aarch64.hash
# pins gcc-linaro-7.2.1-2017.11-x86_64_aarch64-linux-gnu.tar.xz to
#   20181f828e1075f1a493947ff91e82dd578ce9f8638fbdfc39e24b62857d8f8d
# and the dotsrc/armbian mirror copy hashes to that SAME value — the recorded
# 40dce3d3... value below is almost certainly a stale placeholder, and any
# aarch64 (Amlogic) release build WILL fail-closed on it. Changing the pin
# VALUE is operator-gated (ratification) — deliberately NOT changed here.
TOOLCHAIN_SHA256_AARCH64="40dce3d35e95a3a92cba27acbb21f30f86a720d320bc2a2e8a48fea423bc16f7"
case "$TOOLCHAIN_FILE" in
    *arm-linux-gnueabihf*) TOOLCHAIN_SHA256="$TOOLCHAIN_SHA256_ARMHF" ;;
    *aarch64-linux-gnu*)   TOOLCHAIN_SHA256="$TOOLCHAIN_SHA256_AARCH64" ;;
    *)                     TOOLCHAIN_SHA256="" ;;
esac
# W4-5: release/verified builds fail closed on missing or mismatched toolchain
# SHA. The advisory path is only for dev/lab builds.
TOOLCHAIN_SHA256_MANDATORY=0
if dcent_release_provenance_required; then
    TOOLCHAIN_SHA256_MANDATORY=1
    if ! is_truthy "${DCENT_TOOLCHAIN_SHA256_VERIFIED:-0}"; then
        echo "WARNING (DEVOPS-002): release build will enforce the recorded Linaro toolchain SHA256 pin," >&2
        echo "  but DCENT_TOOLCHAIN_SHA256_VERIFIED is unset. Confirm the recorded pin against" >&2
        echo "  the upstream checksum and set DCENT_TOOLCHAIN_SHA256_VERIFIED=1 for release evidence." >&2
    fi
elif is_truthy "${DCENT_TOOLCHAIN_SHA256_VERIFIED:-0}"; then
    TOOLCHAIN_SHA256_MANDATORY=1
fi

# BUG-3 (2026-07-09): releases.linaro.org deleted the 7.2-2017.11 binary
# releases (HTTP 404; sources.buildroot.net and snapshots.linaro.org 404 as
# well), so the network download is now a LAST RESORT — tried against the
# original URL (kept for the record) and then a mirror that was hash-verified
# on 2026-07-09: mirrors.dotsrc.org armbian-dl/_toolchain carries the
# byte-identical armhf artifact (sha256 cee0087b... = the ratified pin; the
# aarch64 sibling there matches Buildroot 2018.02 upstream 20181f82...).
# The RELIABLE sources are:
#   1. the persistent in-volume cache /build/dcentos/dl-cache/ (survives
#      Buildroot re-clones — see the BUG-5 restage in Phase 6), and
#   2. an operator/CI-provided local directory:
#        export DCENT_TOOLCHAIN_LOCAL_DIR=<dir containing $TOOLCHAIN_FILE>
#      (populate it with scripts/provision_build_inputs.sh).
# The DEVOPS-002 SHA256 verification below runs on EVERY source (cache,
# local dir, or download) and stays fail-closed for release builds.
TOOLCHAIN_FALLBACK_URL="https://mirrors.dotsrc.org/armbian-dl/_toolchain/${TOOLCHAIN_FILE}"
TOOLCHAIN_LOCAL_MOUNT_ARGS=()
if [ -n "${DCENT_TOOLCHAIN_LOCAL_DIR:-}" ]; then
    dcent_require_absolute_host_path "$DCENT_TOOLCHAIN_LOCAL_DIR" || {
        echo "ERROR: DCENT_TOOLCHAIN_LOCAL_DIR must be an absolute host path" >&2
        exit 1
    }
    if [ -f "$DCENT_TOOLCHAIN_LOCAL_DIR/$TOOLCHAIN_FILE" ]; then
        if command -v cygpath >/dev/null 2>&1; then
            TOOLCHAIN_LOCAL_MOUNT="$(cygpath -w "$DCENT_TOOLCHAIN_LOCAL_DIR")"
        else
            TOOLCHAIN_LOCAL_MOUNT="$DCENT_TOOLCHAIN_LOCAL_DIR"
        fi
        dcent_validate_docker_transport_path "$TOOLCHAIN_LOCAL_MOUNT" || {
            echo "ERROR: local toolchain path is unsafe for Docker transport" >&2
            exit 1
        }
        TOOLCHAIN_LOCAL_MOUNT_ARGS=(-v "${TOOLCHAIN_LOCAL_MOUNT}:/toolchain-local:ro")
        echo "Toolchain local source: $DCENT_TOOLCHAIN_LOCAL_DIR/$TOOLCHAIN_FILE"
    else
        echo "WARNING: DCENT_TOOLCHAIN_LOCAL_DIR is set but does not contain $TOOLCHAIN_FILE:" >&2
        echo "  $DCENT_TOOLCHAIN_LOCAL_DIR" >&2
        echo "  Falling back to the persistent dl-cache / network download." >&2
    fi
fi

echo "--- Phase 5b: pre-cache Linaro toolchain (if missing) ---"
docker run --rm \
    -e TOOLCHAIN_FILE="$TOOLCHAIN_FILE" \
    -e TOOLCHAIN_URL="$TOOLCHAIN_URL" \
    -e TOOLCHAIN_FALLBACK_URL="$TOOLCHAIN_FALLBACK_URL" \
    -e TOOLCHAIN_SHA256="$TOOLCHAIN_SHA256" \
    -e TOOLCHAIN_SHA256_MANDATORY="$TOOLCHAIN_SHA256_MANDATORY" \
    "${TOOLCHAIN_LOCAL_MOUNT_ARGS[@]}" \
    -v "${VOLUME_NAME}:/build" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -e
        # BUG-3 + BUG-5 (2026-07-09): the toolchain is cached in a persistent
        # dir OUTSIDE buildroot/ so the Phase 6 fresh-clone rm -rf buildroot
        # can no longer delete it (it used to wipe the just-staged tarball on
        # every cold-volume run). Phase 6 (re)stages this cache into
        # buildroot/dl/ after any clone. Sourcing order:
        #   1. persistent cache /build/dcentos/dl-cache/toolchain-external-custom
        #   2. legacy warm-volume copy in buildroot/dl (migrated forward)
        #   3. operator-provided DCENT_TOOLCHAIN_LOCAL_DIR (mounted read-only)
        #   4. network download: original Linaro URL (DEAD, 404 — kept for the
        #      record), then the hash-verified dotsrc/armbian mirror.
        PERSIST_DIR="/build/dcentos/dl-cache/toolchain-external-custom"
        LEGACY_DIR="/build/dcentos/buildroot/dl/toolchain-external-custom"
        FILE="$TOOLCHAIN_FILE"
        mkdir -p "$PERSIST_DIR"

        have_file() { [ -f "$1" ] && [ "$(stat -c%s "$1")" -gt 100000000 ]; }

        if have_file "$PERSIST_DIR/$FILE"; then
            echo "Toolchain already cached: $FILE ($(ls -lh "$PERSIST_DIR/$FILE" | awk "{print \$5}"))"
        elif have_file "$LEGACY_DIR/$FILE"; then
            echo "Migrating cached toolchain from legacy buildroot/dl into persistent dl-cache..."
            ln "$LEGACY_DIR/$FILE" "$PERSIST_DIR/$FILE" 2>/dev/null || cp "$LEGACY_DIR/$FILE" "$PERSIST_DIR/$FILE"
            ls -lh "$PERSIST_DIR/$FILE"
        elif [ -f "/toolchain-local/$FILE" ]; then
            echo "Copying toolchain from DCENT_TOOLCHAIN_LOCAL_DIR (mounted at /toolchain-local)..."
            cp "/toolchain-local/$FILE" "$PERSIST_DIR/$FILE.tmp"
            mv "$PERSIST_DIR/$FILE.tmp" "$PERSIST_DIR/$FILE"
            ls -lh "$PERSIST_DIR/$FILE"
        else
            DOWNLOADED=0
            for TRY_URL in "$TOOLCHAIN_URL" "${TOOLCHAIN_FALLBACK_URL:-}"; do
                [ -n "$TRY_URL" ] || continue
                echo "Downloading toolchain via curl..."
                echo "  $TRY_URL"
                if curl -fL --retry 3 --retry-delay 5 --retry-max-time 600 \
                    -o "$PERSIST_DIR/$FILE.tmp" "$TRY_URL"; then
                    mv "$PERSIST_DIR/$FILE.tmp" "$PERSIST_DIR/$FILE"
                    ls -lh "$PERSIST_DIR/$FILE"
                    DOWNLOADED=1
                    break
                fi
                rm -f "$PERSIST_DIR/$FILE.tmp"
                echo "  download FAILED from $TRY_URL" >&2
            done
            if [ "$DOWNLOADED" != "1" ]; then
                echo "" >&2
                echo "*** TOOLCHAIN UNAVAILABLE: all download sources failed ***" >&2
                echo "  releases.linaro.org removed the 7.2-2017.11 binary releases (HTTP 404);" >&2
                echo "  sources.buildroot.net and snapshots.linaro.org 404 as well, and the" >&2
                echo "  dotsrc/armbian mirror did not respond. Provide the tarball locally:" >&2
                echo "    1. obtain $FILE" >&2
                echo "       (another DCENT build machine, a previous dcentos-build-work Docker" >&2
                echo "       volume, or run: bash DCENT_OS_Antminer/scripts/provision_build_inputs.sh)" >&2
                echo "    2. export DCENT_TOOLCHAIN_LOCAL_DIR=<dir containing the tarball>" >&2
                echo "    3. re-run this build." >&2
                echo "  Integrity: the SHA256 pin recorded in build_in_docker.sh was cross-verified" >&2
                echo "  against Buildroot toolchain-external-linaro-arm.hash (tags 2018.02 + 2018.05);" >&2
                echo "  whatever you provide is still verified against that pin below" >&2
                echo "  (fail-closed on release builds)." >&2
                exit 1
            fi
        fi

        # DEVOPS-002: cryptographic integrity verification of the toolchain
        # tarball, whatever the source (persistent cache, legacy migration,
        # DCENT_TOOLCHAIN_LOCAL_DIR, or download). The size heuristic above is
        # a fast freshness check; THIS is the security gate.
        if [ -z "${TOOLCHAIN_SHA256:-}" ]; then
            if [ "${TOOLCHAIN_SHA256_MANDATORY:-0}" = "1" ]; then
                echo "ERROR (DEVOPS-002): no expected SHA256 pinned for $FILE; release/verified build fails closed." >&2
                exit 1
            fi
            echo "WARNING (DEVOPS-002): no expected SHA256 pinned for $FILE — integrity NOT verified." >&2
        else
            ACTUAL_SHA256="$(sha256sum "$PERSIST_DIR/$FILE" | awk "{print \$1}")"
            if [ "$ACTUAL_SHA256" = "$TOOLCHAIN_SHA256" ]; then
                echo "OK (DEVOPS-002): toolchain SHA256 verified: $ACTUAL_SHA256"
                if [ "${TOOLCHAIN_SHA256_MANDATORY:-0}" != "1" ]; then
                    # A2e-3: the pin MATCHED but is operator-UNCONFIRMED on this
                    # build (advisory). Surface the exact action so the residual
                    # closes with one operator step.
                    echo "  NOTE (A2e-3): the toolchain matched the RECORDED pin, but that pin is" >&2
                    echo "  not yet operator-confirmed. Confirm $ACTUAL_SHA256" >&2
                    echo "  equals the upstream Linaro published checksum ONCE, then export" >&2
                    echo "  DCENT_TOOLCHAIN_SHA256_VERIFIED=1 so release builds fail-closed on mismatch." >&2
                fi
            else
                echo "" >&2
                echo "*** DEVOPS-002 TOOLCHAIN SHA256 MISMATCH ***" >&2
                echo "  file:     $FILE" >&2
                echo "  expected: $TOOLCHAIN_SHA256" >&2
                echo "  actual:   $ACTUAL_SHA256" >&2
                if [ "${TOOLCHAIN_SHA256_MANDATORY:-0}" = "1" ]; then
                    echo "  FAIL-CLOSED: release/verified build refuses a toolchain that does not match the pin." >&2
                    echo "  If the recorded pin is stale, update TOOLCHAIN_SHA256_ARMHF /" >&2
                    echo "  TOOLCHAIN_SHA256_AARCH64 in build_in_docker.sh against the upstream" >&2
                    echo "  published checksum, then re-run." >&2
                    # Quarantine the mismatching file so a corrected source
                    # (fixed DCENT_TOOLCHAIN_LOCAL_DIR / re-download) is
                    # re-fetched on the next run instead of re-tripping on the
                    # same cached bad copy.
                    mv "$PERSIST_DIR/$FILE" "$PERSIST_DIR/$FILE.badsha256"
                    echo "  Quarantined the mismatching file as $FILE.badsha256 in the dl-cache." >&2
                    exit 1
                else
                    echo "  WARNING: dev/lab build continues (pin not yet operator-verified)." >&2
                    echo "  Set DCENT_TOOLCHAIN_SHA256_VERIFIED=1 once the pin is confirmed to" >&2
                    echo "  make this a hard fail. See the DEVOPS-002 TODO in build_in_docker.sh." >&2
                fi
            fi
        fi

        # Warm-volume convenience: if a real Buildroot checkout already
        # exists, stage the verified tarball into its dl dir now (hardlink —
        # instant, no extra space; cp fallback). Phase 6 also (re)stages
        # after any fresh clone; both paths are idempotent. Deliberately
        # does NOT create buildroot/ here: the old code did (mkdir -p of the
        # dl dir), which left a Makefile-less buildroot/ stub that Phase 6
        # then rm -rf-ed together with the just-staged toolchain (BUG-5).
        if [ -f /build/dcentos/buildroot/Makefile ]; then
            mkdir -p "$LEGACY_DIR"
            rm -f "$LEGACY_DIR/.lock"
            if ! have_file "$LEGACY_DIR/$FILE"; then
                ln -f "$PERSIST_DIR/$FILE" "$LEGACY_DIR/$FILE" 2>/dev/null || cp "$PERSIST_DIR/$FILE" "$LEGACY_DIR/$FILE"
                echo "Staged toolchain into existing buildroot/dl: $FILE"
            fi
        fi
    '
echo ""

# -------- Phase 6: Buildroot --------
echo "--- Phase 6: Buildroot make setup + make -j$(docker run --rm "$BUILD_CONTAINER_ID" nproc) ---"
echo "(first build downloads ~500 MB of tarballs — expect 30–90 min)"
# Mount knowledge-base into the container so board post-image.sh can find
# kernel + FPGA bitstream (am2 needs this; S9 doesn't). Pass an env override
# so post-image.sh doesn't have to compute REPO_ROOT from /build/dcentos/../..
docker run --rm \
    -e FORCE_UNSAFE_CONFIGURE=1 \
    -e TARGET="$TARGET" \
    -e BUILD_ARCH="$BUILD_ARCH" \
    -e TOOLCHAIN_FILE="$TOOLCHAIN_FILE" \
    -e BR_DEFCONFIG="$BR_DEFCONFIG" \
    -e BR_DEFCONFIG_FRAGMENTS="$BR_DEFCONFIG_FRAGMENTS" \
    -e BUILDROOT_REPOSITORY="$BUILDROOT_REPOSITORY" \
    -e BUILDROOT_COMMIT="$BUILDROOT_COMMIT" \
    -e DCENT_RELEASE_IMAGE="${DCENT_RELEASE_IMAGE:-0}" \
    -e DCENT_ALLOW_UNSIGNED_SYSUPGRADE="$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" \
    -e DCENT_PACKAGE_STATUS="$DCENT_PACKAGE_STATUS" \
    -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    -e DCENT_SOURCE_COMMIT="$DCENT_SOURCE_COMMIT" \
    -e DCENT_SOURCE_COMMIT_EPOCH="$DCENT_SOURCE_COMMIT_EPOCH" \
    -e DCENT_SOURCE_TREE_STATE="$DCENT_SOURCE_TREE_STATE" \
    -e DCENT_BUILD_TARGET="$DCENT_BUILD_TARGET" \
    -e DCENT_BUILD_ARCH="$DCENT_BUILD_ARCH" \
    -e DCENT_TOOLCHAIN_ID="$DCENT_TOOLCHAIN_ID" \
    -e DCENT_REQUIRE_RELEASE_PROVENANCE="$DCENT_REQUIRE_RELEASE_PROVENANCE" \
    -e TOOLCHAIN_SHA256_MANDATORY="$TOOLCHAIN_SHA256_MANDATORY" \
    -e DCENT_RELEASE_SIGNING_KEY="$CONTAINER_RELEASE_SIGNING_KEY" \
    -e DCENT_RELEASE_PUBKEY_FILE="$CONTAINER_RELEASE_PUBKEY_FILE" \
    -e DCENT_REQUIRE_RELEASE_KEY="${DCENT_REQUIRE_RELEASE_KEY:-0}" \
    -e DCENT_AM2_S19J_KERNEL="/dcent-inputs/files/knowledge-base/extractions/s19j/kernel.bin" \
    -e DCENT_AM2_S19J_BITSTREAM="/dcent-inputs/files/knowledge-base/extractions/s19j/fpga_bitstream.bit" \
    -e DCENT_AM2_S19PRO_KERNEL="/dcent-inputs/files/knowledge-base/extractions/s19j/kernel.bin" \
    -e DCENT_AM2_S19PRO_BITSTREAM="/dcent-inputs/files/knowledge-base/extractions/s19j/fpga_bitstream.bit" \
    "${SIGNING_MOUNT_ARGS[@]}" \
    "${BUILD_INPUT_MOUNT_ARGS[@]}" \
    -v "${VOLUME_NAME}:/build" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -e
        # HTTP/1.1 + larger buffer: avoids "RPC failed; curl 92 HTTP/2 stream 0
        # was not closed cleanly" on flaky networks when cloning Buildroot.
        git config --global http.version HTTP/1.1
        git config --global http.postBuffer 524288000
        git config --global http.lowSpeedLimit 1000
        git config --global http.lowSpeedTime 60

        cd /build/dcentos

        # ----- Merge shared fragments + per-product defconfig --------------
        # Buildroot has no native top-level fragment mechanism (only the kernel
        # / U-Boot Kconfig recipes do). We materialize a merged defconfig file
        # in-place inside br2_external_dcentos/configs/ named
        # "<base>_full_defconfig". `make defconfig` then consumes that single
        # file, the same way it would consume any other defconfig under the
        # BR2_EXTERNAL configs dir. The fragments are concatenated in the order
        # listed by BR_DEFCONFIG_FRAGMENTS, with the per-product defconfig last
        # so that later overrides win (Kconfig parses top-to-bottom and the
        # last KEY= line wins on conflict).
        CONFIGS_DIR="/build/dcentos/br2_external_dcentos/configs"
        FULL_DEFCONFIG_NAME="${BR_DEFCONFIG%_defconfig}_full_defconfig"
        FULL_DEFCONFIG_PATH="${CONFIGS_DIR}/${FULL_DEFCONFIG_NAME}"
        echo "[setup] merging defconfig fragments [$BR_DEFCONFIG_FRAGMENTS] + $BR_DEFCONFIG"
        {
            echo "# === GENERATED by build_in_docker.sh — DO NOT EDIT BY HAND ==="
            echo "# Source: ${BR_DEFCONFIG_FRAGMENTS} ${BR_DEFCONFIG}"
            echo "# Edit the upstream fragments / defconfig instead."
            echo "# ============================================================="
            for frag in $BR_DEFCONFIG_FRAGMENTS; do
                FRAG_PATH="${CONFIGS_DIR}/${frag}"
                [ -f "$FRAG_PATH" ] || { echo "ERROR: fragment missing: $FRAG_PATH" >&2; exit 1; }
                echo ""
                echo "# --- begin fragment: $frag ---"
                cat "$FRAG_PATH"
                echo "# --- end fragment: $frag ---"
            done
            BASE_PATH="${CONFIGS_DIR}/${BR_DEFCONFIG}"
            [ -f "$BASE_PATH" ] || { echo "ERROR: defconfig missing: $BASE_PATH" >&2; exit 1; }
            echo ""
            echo "# --- begin per-product defconfig: $BR_DEFCONFIG ---"
            cat "$BASE_PATH"
            echo "# --- end per-product defconfig: $BR_DEFCONFIG ---"
            # Production-readiness matrix §7 #1 (public-image trust boundary):
            # on a PRODUCTION/RELEASE build (DCENT_RELEASE_IMAGE=1) LOCK the
            # root account so NO default SSH password login is possible. This
            # is appended LAST so it overrides the shared dcentos-common.fragment
            # BR2_TARGET_GENERIC_ROOT_PASSWD="dcentral" (Buildroot processes the
            # merged defconfig top-to-bottom; the last KEY= line wins). DEV/LAB
            # builds (flag unset) leave root:dcentral byte-identical — the
            # operator ssh_cmd.js / fleet tooling is unaffected. The matching
            # runtime marker /etc/dcentos/release-image (which disables the
            # dashboard passwordless opt-out) is stamped by the board
            # post-build via scripts/lib/release_image_provision.sh, also keyed
            # on DCENT_RELEASE_IMAGE.
            if case "${DCENT_RELEASE_IMAGE:-0}" in 1|true|TRUE|yes|YES|y|Y) true ;; *) false ;; esac; then
                echo ""
                echo "# --- DCENT_RELEASE_IMAGE=1: lock root SSH password login ---"
                echo "BR2_TARGET_GENERIC_ROOT_PASSWD=\"*\""
            fi
        } > "$FULL_DEFCONFIG_PATH"
        echo "[setup] merged defconfig: $FULL_DEFCONFIG_PATH ($(wc -l < "$FULL_DEFCONFIG_PATH") lines)"

        # BUG-5 (2026-07-09): download artifacts must survive a fresh
        # Buildroot clone. The old code rm -rf-ed buildroot/ whenever the
        # Makefile was missing — which, on a cold volume, DELETED the
        # toolchain Phase 5b had just staged into buildroot/dl/ (it only
        # worked when the volume was warm and the clone was skipped). Now:
        # salvage any existing buildroot/dl into the persistent dl-cache
        # BEFORE deleting (hardlinks: instant, no extra space), and (re)stage
        # the persistent cache into buildroot/dl AFTER the clone. Covers
        # cold-volume first runs, warm re-runs, and target switches (the
        # dl-cache accumulates the per-arch toolchains side by side).
        PERSIST_DL_DIR="/build/dcentos/dl-cache"
        mkdir -p "$PERSIST_DL_DIR"
        if [ ! -d buildroot ] || [ ! -f buildroot/Makefile ]; then
            echo "[setup] cloning Buildroot (retry up to 4x)"
            if [ -d buildroot/dl ]; then
                echo "[setup] preserving existing buildroot/dl into persistent dl-cache"
                cp -aln buildroot/dl/. "$PERSIST_DL_DIR/" 2>/dev/null || true
            fi
            rm -rf buildroot
            N=1; MAX=4
            while [ $N -le $MAX ]; do
                echo "[clone try $N/$MAX]"
                if git clone "$BUILDROOT_REPOSITORY" buildroot; then break; fi
                rm -rf buildroot
                N=$((N + 1)); sleep 5
            done
            [ -d buildroot ] || { echo "ERROR: clone failed after $MAX tries"; exit 1; }
            ( cd buildroot && git checkout --detach "$BUILDROOT_COMMIT" )
        else
            echo "[setup] Buildroot present"
        fi

        # Source-closure boundary: a persistent Docker volume may contain an
        # old or locally modified checkout. Refuse it before Buildroot can
        # consume source bytes that the release receipt would not describe.
        ACTUAL_BUILDROOT_COMMIT="$(git -C buildroot rev-parse HEAD)"
        [ "$ACTUAL_BUILDROOT_COMMIT" = "$BUILDROOT_COMMIT" ] || {
            echo "ERROR: warm Buildroot checkout is not at the pinned commit" >&2
            echo "  expected: $BUILDROOT_COMMIT" >&2
            echo "  actual:   $ACTUAL_BUILDROOT_COMMIT" >&2
            exit 1
        }
        if [ -n "$(git -C buildroot status --porcelain --untracked-files=normal)" ]; then
            echo "ERROR: Buildroot source tree has modified, staged, or untracked inputs; refusing unbound release input" >&2
            exit 1
        fi

        # BUG-5 (2026-07-09): (re)stage the persistent dl-cache into
        # buildroot/dl — the location Buildroot actually reads (default
        # BR2_DL_DIR). Idempotent: hardlink, no-clobber. Then hard-check the
        # toolchain THIS target needs is really there before spending 30-90
        # minutes in make.
        mkdir -p buildroot/dl
        cp -aln "$PERSIST_DL_DIR/." buildroot/dl/ 2>/dev/null || true
        if [ -n "${TOOLCHAIN_FILE:-}" ] && [ ! -f "buildroot/dl/toolchain-external-custom/$TOOLCHAIN_FILE" ]; then
            echo "ERROR: required toolchain tarball missing from buildroot/dl after staging:" >&2
            echo "  buildroot/dl/toolchain-external-custom/$TOOLCHAIN_FILE" >&2
            echo "  Phase 5b should have cached it in $PERSIST_DL_DIR/toolchain-external-custom/." >&2
            echo "  Re-run the build; if the network download fails (releases.linaro.org is dead)," >&2
            echo "  provide it via DCENT_TOOLCHAIN_LOCAL_DIR=<dir> or run" >&2
            echo "  bash DCENT_OS_Antminer/scripts/provision_build_inputs.sh first." >&2
            exit 1
        fi

        mkdir -p buildroot/output
        STAMP_FILE="buildroot/output/.dcentos-build-target-stamp"
        DESIRED_STAMP="target=${TARGET:-unknown}|build_arch=${BUILD_ARCH:-unknown}|defconfig=${BR_DEFCONFIG:-unknown}|fragments=${BR_DEFCONFIG_FRAGMENTS:-}"
        NEED_CLEAN=0
        if [ -f "$STAMP_FILE" ]; then
            CURRENT_STAMP="$(cat "$STAMP_FILE" 2>/dev/null || true)"
            if [ "$CURRENT_STAMP" != "$DESIRED_STAMP" ]; then
                echo "[setup] Buildroot target changed: ${CURRENT_STAMP:-unknown} -> $DESIRED_STAMP"
                NEED_CLEAN=1
            fi
        elif [ -d buildroot/output/target ] || [ -d buildroot/output/build ] || [ -d buildroot/output/images ]; then
            echo "[setup] existing Buildroot output has no DCENT target stamp; cleaning once to prevent cross-target rootfs contamination"
            NEED_CLEAN=1
        fi
        if [ "$NEED_CLEAN" = "1" ]; then
            echo "[setup] running Buildroot clean before applying $FULL_DEFCONFIG_NAME"
            make -C buildroot clean || {
                echo "[setup] Buildroot clean failed; removing generated output subtrees directly" >&2
                rm -rf buildroot/output/build buildroot/output/target buildroot/output/images buildroot/output/staging buildroot/output/host
                mkdir -p buildroot/output
            }
        fi
        printf "%s\n" "$DESIRED_STAMP" > "$STAMP_FILE"

        # The ownership ledger is a per-build receipt. Remove the previous
        # generated sidecar even on a warm same-target build so Phase 7 cannot
        # accidentally collect evidence from an earlier target-tree state if a
        # post-image hook is skipped or regresses.
        rm -f buildroot/output/images/rootfs-ownership.json

        echo "[setup] applying merged defconfig $FULL_DEFCONFIG_NAME"
        make -C buildroot BR2_EXTERNAL=/build/dcentos/br2_external_dcentos \
            "$FULL_DEFCONFIG_NAME"

        echo ""
        echo "[build] make -j$(nproc)"
        time make -j$(nproc)

        # Buildroot legal-info is release evidence, not a reusable build
        # product. Delete any warm-volume copy before regeneration so a
        # skipped or failed producer cannot be mistaken for the current build
        # package/source/license observation. The compact inventory is
        # generated and material-reverified later, after the firmware artifact
        # has its final name.
        if [ "$TOOLCHAIN_SHA256_MANDATORY" = "1" ]; then
            echo ""
            echo "[evidence] regenerating Buildroot legal-info"
            rm -rf buildroot/output/legal-info
            make -C buildroot BR2_EXTERNAL=/build/dcentos/br2_external_dcentos legal-info
        fi
    '
echo ""

# -------- Phase 7: package sysupgrade tarball --------
if [ "$BOARD_POST_IMAGE" = "vnish-bootbin-sd" ]; then
    #  2026-05-15 Phase 1B: VNish boot.bin SD prototype
    # variant. The Buildroot post-image produced the standard am3-bb-s19jpro
    # SD payload tarball (rootfs.cpio.gz wrapped uramdisk.image.gz inside);
    # now invoke the VNish-boot.bin-flavored SD .img builder. Reuses the
    # captured LuxOS live boot artifacts for uImage + DTB so we don't depend
    # on a Buildroot kernel build path that hasn't run on this branch.
    echo "--- Phase 7: building VNish boot.bin SD prototype .img for $TARGET ---"
    docker run --rm \
        -v "${VOLUME_NAME}:/build" \
        -v "${POSIX_PROJECT_DIR}:/project:ro" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            cd /build/dcentos
            BOOTBIN_REF="/project/output/vnish-extracted-artifacts/boot.bin-s19jpro-bb-v1.2.6"
            LIVE_DIR="/project/output/am3-bb-s19jpro-79-boot-artifacts-LIVE-20260514T214500Z"
            BR_PAYLOAD_DIR="/build/dcentos/buildroot/output/images/dcentos-am3-bb-s19jpro-sdcard"
            UIMAGE_PATH=""
            DTB_PATH=""
            INITRAMFS_PATH=""
            if [ -f "$LIVE_DIR/uImage" ]; then
                UIMAGE_PATH="$LIVE_DIR/uImage"
            fi
            if [ -f "$LIVE_DIR/devicetree.dtb" ]; then
                DTB_PATH="$LIVE_DIR/devicetree.dtb"
            fi
            if [ -f "$BR_PAYLOAD_DIR/uramdisk.image.gz" ]; then
                INITRAMFS_PATH="$BR_PAYLOAD_DIR/uramdisk.image.gz"
            elif [ -f "/build/dcentos/buildroot/output/images/rootfs.cpio.gz" ]; then
                INITRAMFS_PATH="/build/dcentos/buildroot/output/images/rootfs.cpio.gz"
            fi
            [ -n "$UIMAGE_PATH" ] || { echo "ERROR: no uImage source (looked at $LIVE_DIR/uImage)"; exit 1; }
            [ -n "$DTB_PATH" ]    || { echo "ERROR: no DTB source (looked at $LIVE_DIR/devicetree.dtb)"; exit 1; }
            [ -n "$INITRAMFS_PATH" ] || { echo "ERROR: no initramfs source (looked at $BR_PAYLOAD_DIR/uramdisk.image.gz, $BR_PAYLOAD_DIR/../rootfs.cpio.gz)"; exit 1; }
            [ -f "$BOOTBIN_REF" ] || { echo "ERROR: VNish boot.bin reference missing: $BOOTBIN_REF"; exit 1; }
            bash scripts/build_am3_bb_sd_vnish_bootbin_image.sh \
                --bootbin "$BOOTBIN_REF" \
                --uimage "$UIMAGE_PATH" \
                --dtb "$DTB_PATH" \
                --initramfs "$INITRAMFS_PATH"
            SD_OUT_DIR="/build/dcentos/buildroot/output/images/sd_card_am3_bb_s19jpro"
            cp "$SD_OUT_DIR/'"$TARBALL_NAME"'" "/build/'"$TARBALL_NAME"'"
            cp "$SD_OUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json" \
               "/build/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json"
            ls -la "/build/'"$TARBALL_NAME"'" "/build/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json"
        '
elif [ "$BOARD_POST_IMAGE" = "am2-sd-disk" ]; then
    #  2026-05-15 Phase 4D: am2 S19j Pro SD-disk variant.
    # The Buildroot post-image produced the standard am2-s19jpro sysupgrade
    # tarball (contains the rootfs squashfs). Now invoke the SD .img builder,
    # which extracts the rootfs from the tarball and stages it into a
    # two-partition .img (FAT16 boot + ext2 rootfs) using sd_common.sh.
    echo "--- Phase 7: building am2-s19jpro SD disk .img for $TARGET ---"
    docker run --rm \
        -v "${VOLUME_NAME}:/build" \
        -v "${POSIX_PROJECT_DIR}:/project:ro" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            cd /build/dcentos
            PAYLOAD_TAR="/build/dcentos/buildroot/output/images/dcentos-sysupgrade-am2-s19jpro.tar"
            if [ ! -f "$PAYLOAD_TAR" ]; then
                echo "ERROR: expected am2 sysupgrade tarball missing: $PAYLOAD_TAR" >&2
                ls -la /build/dcentos/buildroot/output/images/ >&2 || true
                exit 1
            fi
            # --allow-incomplete: a real bootable SD also needs BOOT.bin +
            # uImage + DTB (BraiinsOS am1-s9 SD-recovery feed). The Docker
            # pipeline does NOT carry those artifacts today; the operator
            # composes the bootable card by re-running this script locally
            # with --artifacts <dir> against the captured BraiinsOS feed.
            # This Docker arm produces the rootfs-only staging image so the
            # standard build pipeline always has an artifact to upload.
            bash scripts/build_am2_s19jpro_sd_disk_image.sh \
                --payload-tar "$PAYLOAD_TAR" \
                --allow-incomplete
            SD_OUT_DIR="/build/dcentos/buildroot/output/images/sd_card_am2_s19jpro"
            cp "$SD_OUT_DIR/'"$TARBALL_NAME"'" "/build/'"$TARBALL_NAME"'"
            cp "$SD_OUT_DIR/'"$TARBALL_NAME"'.manifest.json" \
               "/build/'"$TARBALL_NAME"'.manifest.json"
            ls -la "/build/'"$TARBALL_NAME"'" "/build/'"$TARBALL_NAME"'.manifest.json"
        '
elif [ "$BOARD_POST_IMAGE" = "board-script" ]; then
    # am2-s19jpro / am3-s19kpro / am3-s21 / am3-bb: the board's post-image.sh already produced
    # the tarball inside the Buildroot output/images dir. Copy it to /build/
    # so Phase 8 can extract it to Windows uniformly.
    echo "--- Phase 7: collecting $TARGET tarball from Buildroot output ---"
    docker run --rm \
        -v "${VOLUME_NAME}:/build" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            SRC="/build/dcentos/buildroot/output/images/'"$TARBALL_NAME"'"
            if [ ! -f "$SRC" ]; then
                echo "ERROR: expected tarball not produced by post-image.sh: $SRC" >&2
                ls -la /build/dcentos/buildroot/output/images/ >&2 || true
                exit 1
            fi
            cp "$SRC" "/build/'"$TARBALL_NAME"'"
            ls -la "/build/'"$TARBALL_NAME"'"
        '
else
    # Default s9 path: use package_sysupgrade.sh with --board am1-s9
    echo "--- Phase 7: package_sysupgrade.sh (board=$BOARD_PKG_NAME) ---"
    docker run --rm \
        -e DCENT_ALLOW_UNSIGNED_SYSUPGRADE="$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" \
        -e DCENT_PACKAGE_STATUS="$DCENT_PACKAGE_STATUS" \
        -e DCENT_RELEASE_IMAGE="${DCENT_RELEASE_IMAGE:-0}" \
        -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
        -e DCENT_SOURCE_COMMIT="$DCENT_SOURCE_COMMIT" \
        -e DCENT_SOURCE_COMMIT_EPOCH="$DCENT_SOURCE_COMMIT_EPOCH" \
        -e DCENT_SOURCE_TREE_STATE="$DCENT_SOURCE_TREE_STATE" \
        -e DCENT_BUILD_TARGET="$DCENT_BUILD_TARGET" \
        -e DCENT_BUILD_ARCH="$DCENT_BUILD_ARCH" \
        -e DCENT_TOOLCHAIN_ID="$DCENT_TOOLCHAIN_ID" \
        -e DCENT_REQUIRE_RELEASE_PROVENANCE="$DCENT_REQUIRE_RELEASE_PROVENANCE" \
        -e DCENT_EXTRACTIONS_DIR="/dcent-inputs/files/knowledge-base/extractions/s9" \
        -e DCENT_RELEASE_SIGNING_KEY="$CONTAINER_RELEASE_SIGNING_KEY" \
        -e DCENT_RELEASE_PUBKEY_FILE="$CONTAINER_RELEASE_PUBKEY_FILE" \
        -e DCENT_REQUIRE_RELEASE_KEY="${DCENT_REQUIRE_RELEASE_KEY:-0}" \
        -e BOARD_PKG_NAME="$BOARD_PKG_NAME" \
        "${SIGNING_MOUNT_ARGS[@]}" \
        "${BUILD_INPUT_MOUNT_ARGS[@]}" \
        -v "${VOLUME_NAME}:/build" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            cd /build/dcentos
            bash scripts/package_sysupgrade.sh \
                --board "$BOARD_PKG_NAME" \
                --output "/build/'"$TARBALL_NAME"'"
        '
fi

# The S9, AM2 S19j Pro, and CV1835 post-image hooks emit the bounded
# final-rootfs ownership ledger while Buildroot's finalized TARGET_DIR and
# package .files-list.txt evidence are still available. Collect it beside the
# release artifact; absence is a pipeline failure for these integrated lanes.
ROOTFS_OWNERSHIP_LEDGER_PATH=""
case "$TARGET" in
    s9|am2-s19jpro|cv1835-s19jpro)
        ROOTFS_OWNERSHIP_LEDGER_NAME="${TARBALL_NAME}.rootfs-ownership.json"
        docker run --rm \
            -v "${VOLUME_NAME}:/build" \
            "$BUILD_CONTAINER_ID" bash -c '
                set -e
                SRC=/build/dcentos/buildroot/output/images/rootfs-ownership.json
                DST=/build/'"$ROOTFS_OWNERSHIP_LEDGER_NAME"'
                [ -f "$SRC" ] || {
                    echo "ERROR: integrated final-rootfs ownership ledger missing: $SRC" >&2
                    exit 1
                }
                cp "$SRC" "$DST"
            '
        ;;
    *)
        ROOTFS_OWNERSHIP_LEDGER_NAME=""
        ;;
esac
echo ""

# -------- Phase 8: extract tarball back to Windows --------
echo "--- Phase 8: extract tarball to $OUTPUT_DIR ---"
docker run --rm \
    -e DCENT_ALLOW_UNSIGNED_SYSUPGRADE="$DCENT_ALLOW_UNSIGNED_SYSUPGRADE" \
    -e DCENT_PACKAGE_STATUS="$DCENT_PACKAGE_STATUS" \
    -e DCENT_RELEASE_PUBKEY_FILE="$CONTAINER_RELEASE_PUBKEY_FILE" \
    -e DCENT_REQUIRE_RELEASE_KEY="${DCENT_REQUIRE_RELEASE_KEY:-0}" \
    -e BOARD_POST_IMAGE="$BOARD_POST_IMAGE" \
    -e ROOTFS_OWNERSHIP_LEDGER_NAME="$ROOTFS_OWNERSHIP_LEDGER_NAME" \
    "${PUBKEY_MOUNT_ARGS[@]}" \
    -v "${VOLUME_NAME}:/build" \
    -v "${POSIX_OUTPUT_DIR}:/out" \
    "$BUILD_CONTAINER_ID" bash -c '
        set -e
        cp /build/'"$TARBALL_NAME"' /out/
        if [ -n "$ROOTFS_OWNERSHIP_LEDGER_NAME" ]; then
            cp "/build/$ROOTFS_OWNERSHIP_LEDGER_NAME" /out/
        fi
        ls -la /out/'"$TARBALL_NAME"'
        echo ""
        echo "SHA256:"
        sha256sum /out/'"$TARBALL_NAME"'
        echo ""
        if [ "$BOARD_POST_IMAGE" = "vnish-bootbin-sd" ]; then
            echo "VNish boot.bin SD image (no tar listing — this is a raw .img):"
            if [ -f /build/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json ]; then
                cp /build/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json /out/
                echo "Manifest:"
                cat /out/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json
            fi
        elif [ "$BOARD_POST_IMAGE" = "am2-sd-disk" ]; then
            echo "am2 S19j Pro SD disk image (no tar listing — this is a raw .img):"
            if [ ! -f /build/'"$TARBALL_NAME"'.manifest.json ]; then
                echo "ERROR: missing am2 SD completeness manifest: /build/'"$TARBALL_NAME"'.manifest.json" >&2
                exit 1
            fi
            cp /build/'"$TARBALL_NAME"'.manifest.json /out/
            echo "Manifest:"
            cat /out/'"$TARBALL_NAME"'.manifest.json
        else
            echo "Contents:"
            tar tf /out/'"$TARBALL_NAME"' | head -20
        fi
        case "'"$TARGET"'" in
            # Added by Phase 4B: am3-s19jpro-aml + am3-t21 inherit the
            # same AM3 sysupgrade-shaped tarball validator wiring. TD-003
            # adds am2-s17pro so its runtime-only tarball is still locally
            # package-validated when a lab kernel is supplied.
            am3-s19kpro|am3-s21|am3-s19jpro-aml|am3-t21|am2-s19jpro|am2-s17pro)
            # End Phase 4B
            # DevOps Q1 finding 4I (2026-05-15): wire pre_flash_validate.sh
            # --package-only for am2-s19jpro so the AM2 signing flow mirrors
            # the AM3 signing flow that was wired in W11.
            echo ""
            echo "Package-only validation:"
            cd /build/dcentos
            DCENT_ALLOW_UNSIGNED_SYSUPGRADE="${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}" \
            DCENT_PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-release}" \
                ./scripts/pre_flash_validate.sh --package-only /out/'"$TARBALL_NAME"' "'"$BOARD_PKG_NAME"'"
            ;;
            am3-bb|am3-bb-s19jpro)
            echo ""
            echo "SD-card payload validation:"
            case "'"$TARGET"'" in
                am3-bb) SD_PREFIX="dcentos-am3-bb-sdcard" ;;
                am3-bb-s19jpro) SD_PREFIX="dcentos-am3-bb-s19jpro-sdcard" ;;
            esac
            tar tf /out/'"$TARBALL_NAME"' | grep -qx "${SD_PREFIX}/uramdisk.image.gz"
            tar tf /out/'"$TARBALL_NAME"' | grep -qx "${SD_PREFIX}/README.txt"
            tar xf /out/'"$TARBALL_NAME"' -C /tmp
            CPIO_LIST="$(gzip -dc "/tmp/${SD_PREFIX}/uramdisk.image.gz" | cpio -it --quiet | sed "s#^\./##")"
            if printf "%s\n" "$CPIO_LIST" \
                | grep -E "(^|/)(uart_trans\.ko|monitor-ipsig|S65monitor-ipsig|daemons|daemonc|update-daemon|S67update-daemon|updateporc\.sh)$"; then
                echo "ERROR: forbidden stock BB daemon/blob present"
                exit 1
            fi
            if [ "'"$TARGET"'" = "am3-bb-s19jpro" ]; then
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/dcentos/board_target"
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/dcentos/board_targets/am3-bb-s19jpro.toml"
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/dcentos/rescue_ssh_enabled"
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/init.d/S50dropbear"
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/init.d/S81mcp"
                printf "%s\n" "$CPIO_LIST" | grep -qx "etc/init.d/S82dcentrald"
            fi
            ROOTFS_CHECK_DIR="/tmp/${SD_PREFIX}-rootfs-check"
            rm -rf "$ROOTFS_CHECK_DIR"
            mkdir -p "$ROOTFS_CHECK_DIR"
            ( cd "$ROOTFS_CHECK_DIR" && gzip -dc "/tmp/${SD_PREFIX}/uramdisk.image.gz" | cpio -idmu --quiet )
            check_armv7_elf() {
                rel="$1"
                path="$ROOTFS_CHECK_DIR/$rel"
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
            check_armv7_elf "bin/busybox"
            check_armv7_elf "sbin/init"
            check_armv7_elf "usr/sbin/dropbear"
            check_armv7_elf "usr/local/bin/dcentrald"
            check_armv7_elf "usr/local/bin/dcentos-discovery"
            BAD_ELF="$(find "$ROOTFS_CHECK_DIR" -type f -exec file {} + | grep -E "ELF 64-bit|aarch64|x86-64" || true)"
            if [ -n "$BAD_ELF" ]; then
                echo "ERROR: AM3-BB rootfs contains non-ARMv7 ELF payloads" >&2
                printf "%s\n" "$BAD_ELF" >&2
                exit 1
            fi
            rm -rf "$ROOTFS_CHECK_DIR"
            rm -rf "/tmp/${SD_PREFIX}"
            echo "PASS: SD payload has uramdisk + README, no forbidden stock BB daemon/blob paths, required management files, and ARMv7 critical binaries"
            ;;
        esac
    '
echo ""

FINAL_BANNER_STATUS="success"
if [ "$BOARD_POST_IMAGE" = "am2-sd-disk" ]; then
    AM2_SD_MANIFEST="$OUTPUT_DIR/$TARBALL_NAME.manifest.json"
    if [ ! -f "$AM2_SD_MANIFEST" ]; then
        echo "ERROR: missing am2 SD completeness manifest after extraction: $AM2_SD_MANIFEST" >&2
        exit 1
    fi
    if ! dcent_sd_manifest_boot_artifacts_complete "$AM2_SD_MANIFEST"; then
        if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
            RENAMED_AM2_IMAGE=$(dcent_sd_mark_incomplete_lab_image "$OUTPUT_DIR/$TARBALL_NAME" "$AM2_SD_MANIFEST")
            TARBALL_NAME="$(basename "$RENAMED_AM2_IMAGE")"
            echo "NOTICE: am2 SD image is incomplete; relabelled to $TARBALL_NAME before signing refusal"
            dcent_sd_require_complete_manifest_for_signing \
                "$OUTPUT_DIR/$TARBALL_NAME" \
                "$OUTPUT_DIR/$TARBALL_NAME.manifest.json"
        elif is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then
            # Applies the -UNSIGNED-LAB-ROOTFS-ONLY.img suffix for incomplete
            # AM2 images so a rootfs-only staging artifact cannot look signed.
            RENAMED_AM2_IMAGE=$(dcent_sd_mark_incomplete_lab_image "$OUTPUT_DIR/$TARBALL_NAME" "$AM2_SD_MANIFEST")
            TARBALL_NAME="$(basename "$RENAMED_AM2_IMAGE")"
            FINAL_BANNER_STATUS="unsigned_lab_rootfs_only"
            echo "NOTICE: am2 SD image is incomplete and unsigned; relabelled to $TARBALL_NAME"
        else
            echo "ERROR: incomplete am2 SD image requires release signing proof or explicit lab unsigned mode" >&2
            exit 1
        fi
    fi
fi

if [ -n "$ROOTFS_OWNERSHIP_LEDGER_NAME" ]; then
    ROOTFS_OWNERSHIP_LEDGER_PATH="$OUTPUT_DIR/$ROOTFS_OWNERSHIP_LEDGER_NAME"
    [ -f "$ROOTFS_OWNERSHIP_LEDGER_PATH" ] || {
        echo "ERROR: final-rootfs ownership ledger was not extracted: $ROOTFS_OWNERSHIP_LEDGER_PATH" >&2
        exit 1
    }
    echo "Final-rootfs ownership ledger: $ROOTFS_OWNERSHIP_LEDGER_PATH"
fi

# Retain the exact Phase-0 export bytes that Phase 5 consumed. These flat,
# release-scoped pairs are semantic inputs to source-closure v4. They remain
# snapshot-consistency evidence only: compiler causality, build execution, and
# equivalence to the installed payload are deliberately not claimed.
if [ "$TOOLCHAIN_SHA256_MANDATORY" = "1" ]; then
    dcent_remove_prebuilt_rust_sidecars "$TARBALL_NAME"
    python3 "$SCRIPT_DIR/binary_build_receipt.py" \
        retain-export-snapshot-set \
        --stage "$BINARY_EXPORT_STAGE" \
        --output-dir "$OUTPUT_DIR" \
        --artifact-prefix "$TARBALL_NAME" >/dev/null
    for binary_name in $(dcent_required_prebuilt_binaries); do
        retained_binary="$OUTPUT_DIR/${TARBALL_NAME}.prebuilt-rust.${binary_name}.bin"
        retained_receipt="$OUTPUT_DIR/${TARBALL_NAME}.prebuilt-rust.${binary_name}.build-receipt.json"
        [ -f "$retained_binary" ] && [ ! -L "$retained_binary" ] || {
            echo "ERROR: retained prebuilt Rust binary is missing or a symlink: $retained_binary" >&2
            exit 1
        }
        [ -f "$retained_receipt" ] && [ ! -L "$retained_receipt" ] || {
            echo "ERROR: retained prebuilt Rust receipt is missing or a symlink: $retained_receipt" >&2
            exit 1
        }
        PREBUILT_RUST_INPUT_ARGS+=(
            --prebuilt-rust-input "$binary_name" "$retained_binary" "$retained_receipt"
        )
        PREBUILT_RUST_INPUT_COUNT=$((PREBUILT_RUST_INPUT_COUNT + 1))
    done
fi

# -------- Phase 8: deterministic partial source-closure receipt --------
# This binds the exact declared source/build definitions and produced artifact
# bytes. It explicitly records that kernel/rootfs byte reproducibility and
# Buildroot legal-info completeness/correctness remain unproven.
SOURCE_CLOSURE_PATH=""
SOURCE_CLOSURE_SIGNATURE_PATH=""
BUILDROOT_LEGAL_INVENTORY_PATH=""
if [ "$TOOLCHAIN_SHA256_MANDATORY" = "1" ]; then
    BUILDROOT_LEGAL_INVENTORY_PATH="$OUTPUT_DIR/${TARBALL_NAME}.buildroot-legal.json"
    docker run --rm \
        -e TARGET="$TARGET" \
        -e BUILD_ARCH="$BUILD_ARCH" \
        -e BUILDROOT_REPOSITORY="$BUILDROOT_REPOSITORY" \
        -e BUILDROOT_COMMIT="$BUILDROOT_COMMIT" \
        -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
        -e ARTIFACT_NAME="$TARBALL_NAME" \
        -v "${VOLUME_NAME}:/build" \
        -v "${POSIX_PROJECT_DIR}:/project:ro" \
        -v "${POSIX_OUTPUT_DIR}:/out" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            INVENTORY="/out/${ARTIFACT_NAME}.buildroot-legal.json"
            python3 /project/scripts/buildroot_legal_inventory.py generate \
                --legal-info-dir /build/dcentos/buildroot/output/legal-info \
                --buildroot-repository "$BUILDROOT_REPOSITORY" \
                --buildroot-commit "$BUILDROOT_COMMIT" \
                --target "$TARGET" \
                --arch "$BUILD_ARCH" \
                --artifact "/out/$ARTIFACT_NAME" \
                --source-date-epoch "$SOURCE_DATE_EPOCH" \
                --output "$INVENTORY" >/dev/null
            python3 /project/scripts/buildroot_legal_inventory.py verify \
                --artifact-dir /out \
                --legal-info-dir /build/dcentos/buildroot/output/legal-info \
                "$INVENTORY" >/dev/null
        '
    [ -f "$BUILDROOT_LEGAL_INVENTORY_PATH" ] || {
        echo "ERROR: Buildroot legal-info inventory was not emitted" >&2
        exit 1
    }
    echo "Buildroot legal-info inventory: $BUILDROOT_LEGAL_INVENTORY_PATH"

    RUST_INVENTORY_PATH="$OUTPUT_DIR/${TARBALL_NAME}.rust-dependencies.json"
    python3 "$SCRIPT_DIR/rust_dependency_inventory.py" generate \
        --workspace "$PROJECT_DIR/dcentrald" \
        --source-root "$REPO_ROOT" \
        --metadata-json "$RUST_METADATA_FILE" \
        --metadata-path-map "/src=$PROJECT_DIR/dcentrald" \
        --metadata-path-map "/dcent-schema=$REPO_ROOT/projects/dcent-schema" \
        --target "$BUILD_ARCH" \
        --artifact "$OUTPUT_DIR/$TARBALL_NAME" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        --output "$RUST_INVENTORY_PATH" >/dev/null
    python3 "$SCRIPT_DIR/rust_dependency_inventory.py" verify \
        --workspace "$PROJECT_DIR/dcentrald" \
        --source-root "$REPO_ROOT" \
        --metadata-json "$RUST_METADATA_FILE" \
        --metadata-path-map "/src=$PROJECT_DIR/dcentrald" \
        --metadata-path-map "/dcent-schema=$REPO_ROOT/projects/dcent-schema" \
        --artifact-dir "$OUTPUT_DIR" \
        "$RUST_INVENTORY_PATH" >/dev/null
    echo "Rust dependency inventory: $RUST_INVENTORY_PATH"

    SOURCE_CLOSURE_PATH="$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json"
    SOURCE_CLOSURE_CONFIG_ARGS=()
    for SOURCE_CLOSURE_FRAGMENT in $BR_DEFCONFIG_FRAGMENTS; do
        SOURCE_CLOSURE_CONFIG_ARGS+=(
            --buildroot-config "$PROJECT_DIR/br2_external_dcentos/configs/$SOURCE_CLOSURE_FRAGMENT"
        )
    done
    SOURCE_CLOSURE_CONFIG_ARGS+=(
        --buildroot-config "$PROJECT_DIR/br2_external_dcentos/configs/$BR_DEFCONFIG"
    )
    SOURCE_CLOSURE_ARTIFACT_ARGS=(
        --artifact "$OUTPUT_DIR/$TARBALL_NAME"
        --artifact "$BUILDROOT_LEGAL_INVENTORY_PATH"
        --artifact "$RUST_INVENTORY_PATH"
    )
    if [ -n "$ROOTFS_OWNERSHIP_LEDGER_PATH" ]; then
        SOURCE_CLOSURE_ARTIFACT_ARGS+=(--artifact "$ROOTFS_OWNERSHIP_LEDGER_PATH")
    fi
    python3 "$SCRIPT_DIR/source_closure.py" generate \
        --repo-root "$GIT_OBJECT_REPO" \
        --source-commit "$DCENT_SOURCE_COMMIT" \
        --source-snapshot "$CAPSULE_SOURCE_SNAPSHOT" \
        --release-invocation "$CAPSULE_INVOCATION_STAGE" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        --target "$TARGET" \
        --arch "$BUILD_ARCH" \
        --cargo-lock "$PROJECT_DIR/dcentrald/Cargo.lock" \
        --cargo-build-script "$SCRIPT_DIR/build-dcentrald.sh" \
        --buildroot-repository "$BUILDROOT_REPOSITORY" \
        --buildroot-commit "$BUILDROOT_COMMIT" \
        "${SOURCE_CLOSURE_CONFIG_ARGS[@]}" \
        --external-tree "$PROJECT_DIR/br2_external_dcentos" \
        --toolchain-id "$DCENT_TOOLCHAIN_ID" \
        --toolchain-sha256 "$TOOLCHAIN_SHA256" \
        --toolchain-verified \
        --container-image-id "$BUILD_CONTAINER_ID" \
        --container-definition "$PROJECT_DIR/Dockerfile.build" \
        --build-input-snapshot "$BUILD_INPUT_SNAPSHOT" \
        "${PREBUILT_RUST_INPUT_ARGS[@]}" \
        "${SOURCE_CLOSURE_ARTIFACT_ARGS[@]}" \
        --receipt-authentication detached_ed25519_required_for_release \
        --output "$SOURCE_CLOSURE_PATH" >/dev/null
    # Authenticate the canonical receipt that binds the firmware artifact,
    # Rust inventory, and (for integrated targets) rootfs ownership ledger.
    # This does not widen the receipt's deliberately partial claim scope.
    [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ] || {
        echo "ERROR: release source closure requires DCENT_RELEASE_SIGNING_KEY" >&2
        exit 1
    }
    [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ] || {
        echo "ERROR: release source closure requires a pinned DCENT_RELEASE_PUBKEY_FILE" >&2
        exit 1
    }
    SOURCE_CLOSURE_SIGNATURE_PATH="${SOURCE_CLOSURE_PATH}.sig"
    docker run --rm \
        -v "${POSIX_OUTPUT_DIR}:/out" \
        -v "${POSIX_PROJECT_DIR}:/project:ro" \
        "${SIGNING_MOUNT_ARGS[@]}" \
        -e RECEIPT_NAME="$(basename "$SOURCE_CLOSURE_PATH")" \
        "$BUILD_CONTAINER_ID" bash -c '
            set -e
            bash /project/scripts/sign_release_receipt.sh \
                "/out/$RECEIPT_NAME" \
                /dcent-release-signing-key.pem \
                /dcent-release-ed25519.pub \
                "/out/$RECEIPT_NAME.sig"
        '
    [ -f "$SOURCE_CLOSURE_SIGNATURE_PATH" ] || {
        echo "ERROR: signed source-closure receipt was not emitted" >&2
        exit 1
    }
    [ "$(wc -c < "$SOURCE_CLOSURE_SIGNATURE_PATH" | tr -d '[:space:]')" = "64" ] || {
        echo "ERROR: Ed25519 source-closure signature must be exactly 64 bytes" >&2
        exit 1
    }
    python3 "$SCRIPT_DIR/source_closure.py" verify \
        --repo-root "$GIT_OBJECT_REPO" \
        --artifact-dir "$OUTPUT_DIR" \
        --source-snapshot "$CAPSULE_SOURCE_SNAPSHOT" \
        --release-invocation "$CAPSULE_INVOCATION_STAGE" \
        --build-input-snapshot "$BUILD_INPUT_SNAPSHOT" \
        --signature "$SOURCE_CLOSURE_SIGNATURE_PATH" \
        --public-key "$DCENT_RELEASE_PUBKEY_FILE" \
        "$SOURCE_CLOSURE_PATH" >/dev/null
    # Retain only the canonical path/hash/ID descriptor. The restricted input
    # bytes remain private and the live snapshot is still required above for
    # release admission; this copy is post-cleanup audit evidence only.
    python3 "$SCRIPT_DIR/release_publication.py" copy \
        --source "$BUILD_INPUT_SNAPSHOT" \
        --output "$OUTPUT_DIR/release-packaging-input.json" >/dev/null
    echo "Source closure: $SOURCE_CLOSURE_PATH"
    echo "Source closure signature: $SOURCE_CLOSURE_SIGNATURE_PATH"
else
    RUST_INVENTORY_PATH=""
    echo "Rust dependency inventory: skipped for non-release lab build"
    echo "Source closure: skipped for non-release lab build with advisory toolchain integrity"
fi

# -------- Phase 8a: standardized release name (operator directive 2026-06-14) --------
# Auto-name the compiled artifact per the canonical DCENT_OS convention
# (DCENTOS_<BOARD><GEN>_<MODEL>_<channel><YYYYMMDD>) via the single-source-of-truth
# helper, so released images are named consistently with no hand-typed names.
# ADDITIVE: the legacy "$TARBALL_NAME" artifact is unchanged; this records the
# standardized name + firmware version + SHA256 next to it and drops a same-named copy.
# Channel override: DCENT_RELEASE_CHANNEL (default beta). See
#  + scripts/firmware_release_name.sh.
if [ -n "$RELEASE_NAME" ]; then
    RELEASE_ARTIFACT_SRC="$OUTPUT_DIR/$TARBALL_NAME"
    if [ -f "$RELEASE_ARTIFACT_SRC" ]; then
        FW_VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$PROJECT_DIR/dcentrald/Cargo.toml" 2>/dev/null || echo unknown)"
        # CE-341: a canonical alias like DCENTOS_XIL1_S9_beta<date>.tar must NOT
        # be emitted for a lab/unsigned build — that would let an unsigned lab
        # artifact carry the exact published beta stem + a release-looking
        # sidecar at the publish step (the internal MANIFEST.json is honest, but
        # this EXTERNAL alias/sidecar was not). Compute release-grade from the
        # same signing/status gates the build already honored; a non-release-grade
        # build gets a loud LAB-UNSIGNED-NOT-FOR-RELEASE marker baked into the
        # alias so it can never masquerade as the clean beta artifact. Stricter-
        # only: the honest internal MANIFEST.json + legacy "$TARBALL_NAME" are
        # untouched. firmware_release_name.sh only accepts beta|dev|rc|stable
        # channels, so the label is a filename suffix, not a lab channel token.
        RELEASE_GRADE=1
        is_release_status "$DCENT_PACKAGE_STATUS" || RELEASE_GRADE=0
        if is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then RELEASE_GRADE=0; fi
        is_truthy "${DCENT_RELEASE_IMAGE:-0}" || RELEASE_GRADE=0
        [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ] || RELEASE_GRADE=0
        if [ "$RELEASE_GRADE" = "1" ]; then
            SIGNATURE_TRUST="signed"
            PROOF_SCOPE="release_grade"
            RELEASE_COPY="$RELEASE_GRADE_PATH"
        else
            SIGNATURE_TRUST="unsigned"
            PROOF_SCOPE="lab_local"
            RELEASE_COPY="$RELEASE_LAB_PATH"
        fi
        RELEASE_EXPECTED_SHA_ARGS=()
        if [ -n "$SOURCE_CLOSURE_PATH" ]; then
            RELEASE_CLOSURE_SHA="$(
                python3 "$SCRIPT_DIR/source_closure.py" query-artifact \
                    --manifest "$SOURCE_CLOSURE_PATH" \
                    --path "$TARBALL_NAME" \
                    --field sha256
            )"
            RELEASE_EXPECTED_SHA_ARGS=(--expected-sha256 "$RELEASE_CLOSURE_SHA")
        fi
        RELEASE_PUBLICATION_RESULT="$(python3 "$SCRIPT_DIR/release_publication.py" copy \
            --source "$RELEASE_ARTIFACT_SRC" \
            --output "$RELEASE_COPY" \
            "${RELEASE_EXPECTED_SHA_ARGS[@]}")"
        RELEASE_SHA="$(
            printf '%s\n' "$RELEASE_PUBLICATION_RESULT" \
                | python3 "$SCRIPT_DIR/release_publication.py" query-result --field sha256
        )"
        RELEASE_METADATA_CONTENT="$({
            echo "release_name=$RELEASE_NAME"
            echo "firmware_version=$FW_VERSION"
            echo "build_target=$TARGET"
            echo "channel=${DCENT_RELEASE_CHANNEL:-beta}"
            echo "artifact=$(basename "$RELEASE_COPY")"
            echo "legacy_artifact=$TARBALL_NAME"
            echo "sha256=$RELEASE_SHA"
            echo "package_status=$DCENT_PACKAGE_STATUS"
            echo "allow_unsigned=$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"
            echo "release_image=${DCENT_RELEASE_IMAGE:-0}"
            echo "created_at_utc=$DCENT_CREATED_AT_UTC"
            echo "source_date_epoch=$SOURCE_DATE_EPOCH"
            echo "source_commit=$DCENT_SOURCE_COMMIT"
            echo "source_tree_state=$DCENT_SOURCE_TREE_STATE"
            echo "release_invocation_id=$CAPSULE_INVOCATION_ID"
            echo "source_snapshot_id=$CAPSULE_SOURCE_ID"
            echo "source_snapshot_descriptor_sha256=$CAPSULE_SOURCE_DESCRIPTOR_SHA256"
            echo "publication_model=private_exact_set_pending_atomic_directory_promotion"
            echo "build_arch=$DCENT_BUILD_ARCH"
            echo "toolchain_id=$DCENT_TOOLCHAIN_ID"
            if [ -n "$SOURCE_CLOSURE_PATH" ]; then
                echo "source_closure=$(basename "$SOURCE_CLOSURE_PATH")"
                echo "source_closure_signature=$(basename "$SOURCE_CLOSURE_SIGNATURE_PATH")"
                echo "prebuilt_rust_inputs=bound_in_signed_source_closure_v4"
                echo "prebuilt_rust_input_count=$PREBUILT_RUST_INPUT_COUNT"
                echo "prebuilt_rust_build_execution_attestation=not_attested"
                echo "prebuilt_rust_installed_payload_equivalence=not_evaluated"
                echo "buildroot_legal_inventory=$(basename "$BUILDROOT_LEGAL_INVENTORY_PATH")"
                echo "rust_dependency_inventory=$(basename "$RUST_INVENTORY_PATH")"
            else
                echo "source_closure=not_emitted_non_release_lab"
                echo "source_closure_signature=not_emitted_non_release_lab"
                echo "prebuilt_rust_inputs=not_retained_non_release_lab"
                echo "prebuilt_rust_input_count=0"
                echo "prebuilt_rust_build_execution_attestation=not_attested"
                echo "prebuilt_rust_installed_payload_equivalence=not_evaluated"
                echo "buildroot_legal_inventory=not_emitted_non_release_lab"
                echo "rust_dependency_inventory=not_emitted_non_release_lab"
            fi
            if [ -n "$ROOTFS_OWNERSHIP_LEDGER_PATH" ]; then
                echo "rootfs_ownership_ledger=$(basename "$ROOTFS_OWNERSHIP_LEDGER_PATH")"
            else
                echo "rootfs_ownership_ledger=not_integrated_for_target"
            fi
            echo "signature_trust=$SIGNATURE_TRUST"
            echo "proof_scope=$PROOF_SCOPE"
            echo "release_grade=$RELEASE_GRADE"
        })"
        echo "--- Standardized release name (Phase 8a) ---"
        echo "  name:     $RELEASE_NAME   (firmware $FW_VERSION)"
        echo "  artifact: $RELEASE_COPY"
        echo "  sha256:   $RELEASE_SHA"
        echo "  trust:    signature_trust=$SIGNATURE_TRUST proof_scope=$PROOF_SCOPE release_grade=$RELEASE_GRADE"
        echo "  metadata: $RELEASE_METADATA_PATH (published after signing stages)"
        echo ""
    fi
else
    if dcent_release_provenance_required; then
        echo "ERROR: release build has no canonical publication name for target '$TARGET'" >&2
        exit 1
    fi
    echo "WARN: firmware_release_name.sh produced no name for target '$TARGET'; artifact stays '$TARBALL_NAME' only." >&2
fi

# -------- Phase 8b (am3-bb family): sign the SD-card tarball --------
# W4.5 (2026-05-07): unlike sysupgrade tarballs (whose MANIFEST.json is signed
# inside the archive by the board post-image script), the am3-bb SD-card
# payload is a flat tarball with no inner manifest. Produce a sibling
# `dcentos-am3-bb-sdcard.tar.sig` (raw ed25519 over the .tar bytes) so an
# operator can verify integrity before imaging an SD card.
#
# Skipped for lab/unsigned builds (DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 with no
# signing key). Required when DCENT_RELEASE_SIGNING_KEY is set.
if [ "$TARGET" = "am3-bb" ] || [ "$TARGET" = "am3-bb-s19jpro" ]; then
    if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
        echo "--- Phase 8b: sign $TARGET SD-card tarball (ed25519) ---"
        # Stage the signing key into the container so we don't widen the
        # mount surface. Path inside container: /signkey.
        SIGN_KEY_HOST="${DCENT_RELEASE_SIGNING_KEY}"
        if command -v cygpath >/dev/null 2>&1; then
            SIGN_KEY_MOUNT="$(cygpath -w "$SIGN_KEY_HOST")"
        else
            SIGN_KEY_MOUNT="$SIGN_KEY_HOST"
        fi
        if [ ! -f "$SIGN_KEY_HOST" ]; then
            echo "ERROR: DCENT_RELEASE_SIGNING_KEY points to missing file: $SIGN_KEY_HOST" >&2
            exit 1
        fi
        docker run --rm \
            -v "${POSIX_OUTPUT_DIR}:/out" \
            -v "${SIGN_KEY_MOUNT}:/signkey:ro" \
            "${PUBKEY_MOUNT_ARGS[@]}" \
            -e TARBALL_NAME="$TARBALL_NAME" \
            -e DCENT_RELEASE_PUBKEY_FILE="$CONTAINER_RELEASE_PUBKEY_FILE" \
            "$BUILD_CONTAINER_ID" bash -c '
                set -e
                command -v openssl >/dev/null 2>&1 || { echo "ERROR: openssl required for am3-bb signing"; exit 1; }
                SIG_OUT="/out/${TARBALL_NAME}.sig"
                openssl pkeyutl -sign -rawin \
                    -inkey /signkey \
                    -in "/out/${TARBALL_NAME}" \
                    -out "$SIG_OUT"
                # Verify locally before declaring success. Prefer the pinned
                # trusted public key when supplied (mounted at the container
                # path); fail closed if it was declared but is not present so a
                # wrong/rotated signing key cannot self-verify (CE-271).
                if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
                    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
                        echo "ERROR: trusted release pubkey declared but missing inside signer container: ${DCENT_RELEASE_PUBKEY_FILE}" >&2
                        rm -f "$SIG_OUT"; exit 1
                    fi
                    cp "${DCENT_RELEASE_PUBKEY_FILE}" /tmp/release_ed25519.pub
                else
                    openssl pkey -in /signkey -pubout -out /tmp/release_ed25519.pub >/dev/null 2>&1
                fi
                openssl pkeyutl -verify -rawin -pubin \
                    -inkey /tmp/release_ed25519.pub \
                    -sigfile "$SIG_OUT" \
                    -in "/out/${TARBALL_NAME}" >/dev/null \
                    || { echo "ERROR: ${TARBALL_NAME}.sig verification failed"; rm -f "$SIG_OUT"; exit 1; }
                ls -la "$SIG_OUT"
                echo "Signed ${TARBALL_NAME}.sig"
            '
        if [ "${RELEASE_GRADE:-0}" = "1" ] && [ -n "${RELEASE_COPY:-}" ]; then
            RELEASE_SIGNATURE_PATH="${RELEASE_COPY}.sig"
            python3 "$SCRIPT_DIR/release_publication.py" copy \
                --source "$OUTPUT_DIR/${TARBALL_NAME}.sig" \
                --output "$RELEASE_SIGNATURE_PATH" >/dev/null
            openssl pkeyutl -verify -rawin -pubin \
                -inkey "$DCENT_RELEASE_PUBKEY_FILE" \
                -sigfile "$RELEASE_SIGNATURE_PATH" \
                -in "$RELEASE_COPY" >/dev/null
        fi
    elif is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then
        echo "--- Phase 8b: skipping $TARGET tarball signing (lab/unsigned build) ---"
    fi
fi

# -------- Phase 8c (SD .img variants): sign the raw SD image --------
#  2026-05-15 Phase 4I: SD .img carriers (vnish-bootbin-sd,
# am2-sd-disk) get a sibling Ed25519 `<name>.img.sig` next to the image.
# Uses the same release key as Phase 8b. The sign_sd_image.sh wrapper does
# the openssl invocation; here we just bind-mount the key and invoke it.
# Lab builds without DCENT_RELEASE_SIGNING_KEY get a WARN line (and exit 0)
# from sign_sd_image.sh itself — no Phase 8c gating needed.
if [ "$BOARD_POST_IMAGE" = "vnish-bootbin-sd" ] || [ "$BOARD_POST_IMAGE" = "am2-sd-disk" ]; then
    if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
        echo "--- Phase 8c: sign $TARGET SD .img (ed25519) ---"
        if [ "$BOARD_POST_IMAGE" = "am2-sd-disk" ]; then
            dcent_sd_require_complete_manifest_for_signing \
                "$OUTPUT_DIR/$TARBALL_NAME" \
                "$OUTPUT_DIR/$TARBALL_NAME.manifest.json"
        fi
        SIGN_KEY_HOST="${DCENT_RELEASE_SIGNING_KEY}"
        if command -v cygpath >/dev/null 2>&1; then
            SIGN_KEY_MOUNT="$(cygpath -w "$SIGN_KEY_HOST")"
        else
            SIGN_KEY_MOUNT="$SIGN_KEY_HOST"
        fi
        if [ ! -f "$SIGN_KEY_HOST" ]; then
            echo "ERROR: DCENT_RELEASE_SIGNING_KEY points to missing file: $SIGN_KEY_HOST" >&2
            exit 1
        fi
        docker run --rm \
            -v "${POSIX_OUTPUT_DIR}:/out" \
            -v "${POSIX_PROJECT_DIR}:/project:ro" \
            -v "${SIGN_KEY_MOUNT}:/signkey:ro" \
            "${PUBKEY_MOUNT_ARGS[@]}" \
            -e TARBALL_NAME="$TARBALL_NAME" \
            -e DCENT_RELEASE_PUBKEY_FILE="$CONTAINER_RELEASE_PUBKEY_FILE" \
            "$BUILD_CONTAINER_ID" bash -c '
                set -e
                bash /project/scripts/sign_sd_image.sh "/out/${TARBALL_NAME}" --key /signkey
            '
        if [ "${RELEASE_GRADE:-0}" = "1" ] && [ -n "${RELEASE_COPY:-}" ]; then
            RELEASE_SIGNATURE_PATH="${RELEASE_COPY}.sig"
            python3 "$SCRIPT_DIR/release_publication.py" copy \
                --source "$OUTPUT_DIR/${TARBALL_NAME}.sig" \
                --output "$RELEASE_SIGNATURE_PATH" >/dev/null
            bash "$SCRIPT_DIR/verify_sd_image.sh" "$RELEASE_COPY" \
                --pubkey "$DCENT_RELEASE_PUBKEY_FILE" >/dev/null
        fi
    elif is_truthy "$DCENT_ALLOW_UNSIGNED_SYSUPGRADE"; then
        echo "--- Phase 8c: skipping $TARGET SD .img signing (lab/unsigned build) ---"
    else
        # Run the wrapper anyway so the operator gets the [WARN] line for
        # the missing key (script exits 0 in this case — no stale .sig).
        bash "$SCRIPT_DIR/sign_sd_image.sh" "$OUTPUT_DIR/$TARBALL_NAME" || true
    fi
fi
if [ -n "${RELEASE_METADATA_CONTENT:-}" ]; then
    printf '%s\n' "$RELEASE_METADATA_CONTENT" \
        | python3 "$SCRIPT_DIR/release_publication.py" stdin \
            --output "$RELEASE_METADATA_PATH" >/dev/null
fi
echo ""

echo "==================================================================="
if [ "$FINAL_BANNER_STATUS" = "unsigned_lab_rootfs_only" ]; then
    echo "LAB STAGING ONLY - unsigned rootfs-only SD image ready at:"
else
    echo "SUCCESS - artifact ready at:"
fi
echo "  $OUTPUT_DIR/$TARBALL_NAME"
if [ -f "$OUTPUT_DIR/$TARBALL_NAME.manifest.json" ]; then
    echo "  Manifest: $OUTPUT_DIR/$TARBALL_NAME.manifest.json"
fi
if [ -n "$SOURCE_CLOSURE_PATH" ]; then
    echo "  Source closure: $SOURCE_CLOSURE_PATH"
    echo "  Closure signature: $SOURCE_CLOSURE_SIGNATURE_PATH"
fi
if [ -n "$ROOTFS_OWNERSHIP_LEDGER_PATH" ]; then
    echo "  Rootfs ownership ledger: $ROOTFS_OWNERSHIP_LEDGER_PATH"
fi
echo ""
if [ "$TARGET" = "am2-s19jpro" ]; then
    echo "Next (Phase 3 — S19j Pro .139):"
    echo "  scp -O \"$OUTPUT_DIR/$TARBALL_NAME\" root@203.0.113.139:/data/"
    echo "  ssh root@203.0.113.139 'sysupgrade --test /data/$TARBALL_NAME'"
    echo "  DO NOT flash until: (a) am2-s19j kernel extracted, (b) dcentrald config"
    echo "                      finalized by Agent C, (c) inactive UBI slot volume"
    echo "                      counts verified (see feedback_ubi_inactive_slot_volume_mismatch.md)."
elif [ "$TARGET" = "am3-s21" ]; then
    echo "Next (Wave 0c — S21 .135 dry-run):"
    echo "  dcent install 203.0.113.135 -f \"$OUTPUT_DIR/$TARBALL_NAME\" --plan"
    echo ""
    echo "Then native NAND write is DESTRUCTIVE and operator-gated:"
    echo "  scripts/build_amlogic_native_install.sh --variant s21"
    echo "  flash_erase/nandwrite only after backup, readback, physical access, and reboot plan."
elif [ "$TARGET" = "am3-s19kpro" ]; then
    echo "Next (Phase K — S19k Pro .78 dry-run):"
    echo "  dcent install 203.0.113.78 -f \"$OUTPUT_DIR/$TARBALL_NAME\" --plan"
    echo ""
    echo "Then Phase L (live install, DESTRUCTIVE — operator-gated):"
    echo "  dcent install 203.0.113.78 -f \"$OUTPUT_DIR/$TARBALL_NAME\""
    echo ""
    echo "Preflight gate enforces: (a) BHB56902 0x05 0x11 EEPROM preamble,"
    echo "                          (b) APW121215f PSU fw=0x76,"
    echo "                          (c) Amlogic am3-aml NoPic platform identity."
# Added by Phase 4B (2026-05-15): am3-s19jpro-aml + am3-t21 are scaffold-only
# variants; no install route is registered yet and there is no live unit on
# the fleet. The tarball exists for build-pipeline / package-validator
# regression coverage until a bench unit is acquired and Phase 4C ships the
# matching revert-to-stock script.
elif [ "$TARGET" = "am3-s19jpro-aml" ]; then
    echo "Phase 4B scaffold target (am3-s19jpro-aml — no live unit yet):"
    echo "  Sysupgrade tarball: $OUTPUT_DIR/$TARBALL_NAME"
    echo ""
    echo "No toolbox install route advertises this board target yet."
    echo "Pending: Phase 4C revert-to-stock + bench unit acquisition."
elif [ "$TARGET" = "am3-t21" ]; then
    echo "Phase 4B scaffold target (am3-t21 — no live unit yet):"
    echo "  Sysupgrade tarball: $OUTPUT_DIR/$TARBALL_NAME"
    echo ""
    echo "No toolbox install route advertises this board target yet."
    echo "Pending: Phase 4C revert-to-stock + bench unit acquisition."
# End Phase 4B
elif [ "$TARGET" = "am3-bb" ] || [ "$TARGET" = "am3-bb-s19jpro" ]; then
    echo "Next (AM3 BB .79 SD-card first boot):"
    echo "  Inspect \"$OUTPUT_DIR/$TARBALL_NAME\""
    if [ -f "$OUTPUT_DIR/${TARBALL_NAME}.sig" ]; then
        echo "  Signature:    $OUTPUT_DIR/${TARBALL_NAME}.sig (ed25519)"
        echo "  Verify with:  openssl pkeyutl -verify -rawin -pubin \\"
        echo "                  -inkey release_ed25519.pub \\"
        echo "                  -sigfile \"$OUTPUT_DIR/${TARBALL_NAME}.sig\" \\"
        echo "                  -in \"$OUTPUT_DIR/$TARBALL_NAME\""
    fi
    echo "  Add verified AM335x boot artifacts (MLO, u-boot.img, uImage, DTB)."
    echo "  Image SD media manually with physical recovery available."
    echo ""
    echo "This tarball is NOT a NAND/sysupgrade package and contains no stock uart_trans.ko."
    if [ "$TARGET" = "am3-bb-s19jpro" ]; then
        echo "It should boot dcentrald with --am3-bb-mining plus rescue SSH and localhost MCP."
    fi
    echo "JTAG + serial recovery: see br2_external_dcentos/board/beaglebone/am3-bb/README.md"
elif [ "$TARGET" = "am3-bb-s19jpro-vnish" ]; then
    echo "Next (AM3 BB .79 VNish boot.bin SD prototype — Phase 1B):"
    echo "  Image:    $OUTPUT_DIR/$TARBALL_NAME"
    echo "  Manifest: $OUTPUT_DIR/dcentos-am3-bb-s19jpro-vnish-bootbin.manifest.json"
    echo ""
    echo "This is a Phase 1B prototype (Preparedness Sweep 2026-05-15). The .img"
    echo "embeds the captured VNish v1.2.6 boot.bin SD-stage U-Boot at sector 1"
    echo "of a FAT16 partition (label DCENTOS by default; pass --label ANTHILLOS"
    echo "to mirror VNish exactly)."
    echo ""
    echo "Boot flow at runtime: AM335x BootROM -> NAND U-Boot SPL -> SD fatload"
    echo "boot.bin @ 0x88000000 -> uImage @ 0x80200000 -> DTB @ 0x80f80000 ->"
    echo "uramdisk.image.gz @ 0x81000000 -> go 0x88000000."
    echo ""
    echo "Flash:  dd if=\"$OUTPUT_DIR/$TARBALL_NAME\" of=/dev/sdX bs=4M status=progress"
    echo "        (or DCENT_OS_Antminer/scripts/write_am3_bb_sd_physical_windows.ps1 as Administrator)"
    echo ""
    echo "DO NOT call this image cold-boot-proven until .79 boots it, exposes"
    echo "SSH/MCP/dashboard, and submits accepted shares from S82dcentrald."
else
    echo "Next (from runbook $PROJECT_DIR/docs/reviews/2026-04-16_flash_runbook_203.0.113.118.md):"
    echo "  scp -O \"$OUTPUT_DIR/$TARBALL_NAME\" root@203.0.113.118:/data/"
    echo "  ssh root@203.0.113.118 'sysupgrade --test /data/$TARBALL_NAME'"
fi
echo "==================================================================="
