#!/bin/bash
#
# package_sysupgrade.sh — Package DCENTos for the validated S9 sysupgrade/update path
# D-Central Technologies, 2026
#
# Creates a signed firmware package for the validated S9 sysupgrade/update path.
# It may be used via:
#   1. This script with --upload
#   2. SSH upload + on-target sysupgrade verification
#   3. BraiinsOS web interface compatibility upload
#
# BraiinsOS uses OpenWrt's sysupgrade mechanism. The expected tar format is:
#   sysupgrade-<board>/
#     kernel   — raw kernel image (zImage)
#     root     — rootfs image (squashfs)
#
# The web interface performs these checks:
#   1. Valid tar archive
#   2. Contains sysupgrade-* directory
#   3. Contains kernel and root files
#   4. Optional: signature verification (if configured)
#
# IMPORTANT: Live upload should use a signed package. Unsigned packages are for
# controlled lab workflows only, behind explicit overrides.
#
# Prerequisites:
#   - BraiinsOS boot components in extractions/s9/ (kernel)
#   - DCENTos rootfs.squashfs from Buildroot
#   - Release invocations must supply source-bound SOURCE_DATE_EPOCH,
#     DCENT_SOURCE_COMMIT[_EPOCH], clean tree state, build arch and toolchain id.
#     scripts/build_in_docker.sh derives and validates these centrally.
#
# Usage:
#   ./package_sysupgrade.sh                           # Default paths
#   ./package_sysupgrade.sh --output dcentos.tar      # Custom output
#   ./package_sysupgrade.sh --upload <miner_ip>       # Package + upload + sysupgrade request
#

set -euo pipefail

# =============================================================================
# Configuration
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIRMWARE_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(dirname "$FIRMWARE_DIR")"
IMAGES_DIR="$FIRMWARE_DIR/buildroot/output/images"
. "$SCRIPT_DIR/lib/dcentrald_version_gate.sh"
. "$SCRIPT_DIR/lib/release_envelope.sh"

OUTPUT_FILE=""
UPLOAD_IP=""
# CRITICAL (2026-03-24): Board name MUST match platform exactly.
# BraiinsOS sysupgrade extracts from sysupgrade-{board_name}/.
# Wrong name = silent failure = empty UBI = brick.
#   S9:       am1-s9    (14 UIO, chain6-8, 95MB firmware slots)
#   S19j Pro: am2-s19j  (legacy AM2 package board name; NOT am2-s17 or am2-s19jpro)
#   S19 Pro:  not accepted by this legacy packager; use the dedicated S19 Pro path when validated
BOARD_NAME="am1-s9"
BOARD_EXPLICIT=0
EXTRACTIONS_DIR=""
BOARD_FAMILY=""
AM2_S9_PLACEHOLDER=0
SIGNING_KEY="${DCENT_RELEASE_SIGNING_KEY:-}"
VERIFY_PUBKEY="${DCENT_RELEASE_PUBKEY_FILE:-}"
REQUIRE_RELEASE_KEY="${DCENT_REQUIRE_RELEASE_KEY:-0}"
ALLOW_UNSIGNED_UPLOAD=false
PACKAGE_PUBKEY=""
PACKAGE_PUBKEY_HASH=""
PACKAGE_PUBKEY_SIZE=""
PACKAGE_VERSION="${DCENT_PACKAGE_VERSION:-}"
PACKAGE_VERSION_SOURCE=""
PACKAGE_STATUS="${DCENT_PACKAGE_STATUS:-release}"

# Colors
RED='\033[1;31m'
GREEN='\033[1;32m'
YELLOW='\033[1;33m'
CYAN='\033[1;36m'
BOLD='\033[1m'
NC='\033[0m'

info()   { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()  { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }
header() { echo -e "\n${CYAN}${BOLD}=== $* ===${NC}"; }

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

# Read the first 4 bytes of a file as a lowercase hex string (no spaces).
# Used to classify the kernel member: a bare ARM zImage starts with the ARM
# branch/nop stream "0000a0e1..." (the zImage magic 0x016f2818 lives at byte
# offset 0x24, NOT 0x00). A FIT image starts with the DTB magic d00dfeed.
# A legacy U-Boot uImage starts with 27051956.
file_magic_hex() {
    od -An -N4 -tx1 "$1" 2>/dev/null | tr -d ' \n'
}

# Return 0 if the file is already a U-Boot-bootable kernel container that
# `bootm` accepts (FIT d00dfeed or legacy uImage 27051956). Return 1 for a
# bare zImage / anything else (which `bootm` REJECTS on the S9 Zynq path,
# bricking the unit — this is what bricked .135).
kernel_is_bootm_ready() {
    case "$(file_magic_hex "$1")" in
        d00dfeed|27051956) return 0 ;;
        *) return 1 ;;
    esac
}

# Wrap a BARE zynq kernel (ARM zImage) into a NAND-boot FIT (kernel + S9 DTB,
# NO ramdisk — the rootfs comes from the UBI volume, not an initramfs). This
# mirrors the proven SD build's dcentos-sd.its MINUS the ramdisk node, and is
# byte-for-byte the recipe behind the hand-built nand-kernel-fit-118 artifact
# (kernel 4.4.92 + s9 dtb, load/entry 0x8000). Emits the .itb path on stdout.
#
# Args: $1 = bare kernel (zImage) path
#       $2 = S9 device tree (.dtb) path
#       $3 = working/output dir for the .its/.itb (must be writable)
#       $4 = FIT description string
build_nand_kernel_fit() {
    bnkf_kernel="$1"
    bnkf_dtb="$2"
    bnkf_workdir="$3"
    bnkf_desc="$4"

    command -v mkimage >/dev/null 2>&1 \
        || error "mkimage not found (u-boot-tools) — required to wrap the bare zImage into a bootable FIT kernel. Install: sudo apt install u-boot-tools"
    [ -f "$bnkf_dtb" ] \
        || error "S9 device tree not found at '$bnkf_dtb' — required to build the NAND kernel FIT (mirrors the SD build's fdt node)."

    mkdir -p "$bnkf_workdir"
    # mkimage reads /incbin/ paths relative to the .its location; stage local
    # copies with stable names so the .its is self-contained and reproducible.
    cp "$bnkf_kernel" "$bnkf_workdir/kernel.bin"
    cp "$bnkf_dtb" "$bnkf_workdir/s9_devicetree.dtb"

    cat > "$bnkf_workdir/nand-kernel.its" << ITS_EOF
/dts-v1/;
/ {
    description = "$bnkf_desc";
    #address-cells = <1>;
    images {
        kernel {
            description = "Linux 4.4.92 (BraiinsOS preserved)";
            data = /incbin/("./kernel.bin");
            type = "kernel";
            arch = "arm";
            os = "linux";
            compression = "none";
            load = <0x00008000>;
            entry = <0x00008000>;
        };
        fdt {
            description = "Antminer S9 Device Tree (BraiinsOS)";
            data = /incbin/("./s9_devicetree.dtb");
            type = "flat_dt";
            arch = "arm";
            compression = "none";
        };
    };
    configurations {
        default = "config";
        config {
            description = "DCENT_OS S9 NAND boot (kernel + fdt, rootfs from UBI)";
            kernel = "kernel";
            fdt = "fdt";
        };
    };
};
ITS_EOF

    ( cd "$bnkf_workdir" && mkimage -f nand-kernel.its nand-kernel.itb >/dev/null ) \
        || error "mkimage failed to build the NAND kernel FIT from the bare zImage."

    # Defensive: the emitted FIT must actually carry the FIT magic.
    if ! kernel_is_bootm_ready "$bnkf_workdir/nand-kernel.itb"; then
        error "Built FIT '$bnkf_workdir/nand-kernel.itb' does not carry FIT magic (d00dfeed) — refusing to package a non-bootable kernel."
    fi
    printf '%s\n' "$bnkf_workdir/nand-kernel.itb"
}

require_unsigned_lab_override() {
    local reason="$1"

    if is_release_status "$PACKAGE_STATUS"; then
        error "Production release package requires trusted release keys/signatures; refusing ${reason}. Set DCENT_PACKAGE_STATUS to a non-release lab value and DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1 only for lab packages."
    fi

    if ! is_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
        error "${reason} requires explicit lab override DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1."
    fi
}

# =============================================================================
# Parse Arguments
# =============================================================================

usage() {
    echo "Usage: $(basename "$0") [OPTIONS]"
    echo ""
    echo "Package DCENTos as a versioned S9 sysupgrade/update tar."
    echo ""
    echo "Options:"
    echo "  --output <file>     Output tar file (default: dcentos-sysupgrade.tar)"
    echo "  --upload <ip>       Upload and stage sysupgrade on a validated am1-s9 target via SSH"
    echo "  --board <name>      Board name for sysupgrade dir (currently safe: am1-s9)"
    echo "  --version <value>   Package version (default: infer from build/rootfs/Cargo metadata)"
    echo "  --signing-key <pem> Ed25519 private key for MANIFEST.sig generation"
    echo "  --verify-pubkey <pem>  Ed25519 public key used to verify MANIFEST.sig locally"
    echo "  --allow-unsigned-upload  Allow unsigned package upload in lab workflows"
    echo "  --images-dir <dir>  Path to Buildroot output (default: buildroot/output/images)"
    echo "  --help              Show this help"
    echo ""
    echo "The output tar can be used with:"
    echo "  1. This script: --upload <ip>"
    echo "  2. SSH: scp dcentos.tar root@<ip>:/data/ && ssh root@<ip> sysupgrade /data/dcentos.tar"
    echo "  3. BraiinsOS web UI compatibility upload"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --output)      OUTPUT_FILE="$2"; shift 2 ;;
        --upload)      UPLOAD_IP="$2"; shift 2 ;;
        --board)       BOARD_NAME="$2"; BOARD_EXPLICIT=1; shift 2 ;;
        --version)     PACKAGE_VERSION="$2"; PACKAGE_VERSION_SOURCE="--version"; shift 2 ;;
        --signing-key) SIGNING_KEY="$2"; shift 2 ;;
        --verify-pubkey) VERIFY_PUBKEY="$2"; shift 2 ;;
        --allow-unsigned-upload) ALLOW_UNSIGNED_UPLOAD=true; shift ;;
        --images-dir)  IMAGES_DIR="$2"; shift 2 ;;
        --help|-h)     usage; exit 0 ;;
        *)             error "Unknown option: $1" ;;
    esac
done

# Default output
if [ -z "$OUTPUT_FILE" ]; then
    OUTPUT_FILE="$IMAGES_DIR/dcentos-sysupgrade.tar"
fi

if [ "$REQUIRE_RELEASE_KEY" = "1" ] && [ -z "$SIGNING_KEY" ]; then
    error "DCENT_REQUIRE_RELEASE_KEY=1 but no signing key was provided. Use --signing-key or DCENT_RELEASE_SIGNING_KEY."
fi

case "$BOARD_NAME" in
    am1-s9)
        # Probe multiple plausible locations (reorganization 2026-03-13 moved
        # shared research + extractions to repo-root knowledge-base/).
        # Honor DCENT_EXTRACTIONS_DIR override first, then fall through.
        REPO_ROOT="$(dirname "$PROJECT_ROOT")"
        if [ -n "${DCENT_EXTRACTIONS_DIR:-}" ] && [ -d "$DCENT_EXTRACTIONS_DIR" ]; then
            EXTRACTIONS_DIR="$DCENT_EXTRACTIONS_DIR"
        elif [ -d "$FIRMWARE_DIR/extractions/s9" ]; then
            EXTRACTIONS_DIR="$FIRMWARE_DIR/extractions/s9"
        elif [ -d "$PROJECT_ROOT/extractions/s9" ]; then
            EXTRACTIONS_DIR="$PROJECT_ROOT/extractions/s9"
        elif [ -d "$REPO_ROOT/knowledge-base/extractions/s9" ]; then
            EXTRACTIONS_DIR="$REPO_ROOT/knowledge-base/extractions/s9"
        else
            error "No extractions dir for am1-s9. Searched: \$DCENT_EXTRACTIONS_DIR, $FIRMWARE_DIR/extractions/s9, $PROJECT_ROOT/extractions/s9, $REPO_ROOT/knowledge-base/extractions/s9"
        fi
        BOARD_FAMILY="am1"
        ;;
    am2-s19j)
        # S19j Pro Zynq AM2 legacy package lane. Board name MUST be "am2-s19j"
        # (NOT "am2-s17" or "am2-s19jpro") — .
        # Kernel sourcing mirrors post-image.sh probe order; S9 kernel fallback
        # is PLACEHOLDER-only — Phase 3 must supply a real am2 kernel via
        # DCENT_AM2_S19J_KERNEL or the  path.
        REPO_ROOT="$(dirname "$PROJECT_ROOT")"
        if [ -n "${DCENT_EXTRACTIONS_DIR:-}" ] && [ -d "$DCENT_EXTRACTIONS_DIR" ]; then
            EXTRACTIONS_DIR="$DCENT_EXTRACTIONS_DIR"
        elif [ -d "$REPO_ROOT/knowledge-base/extractions/s19j" ]; then
            EXTRACTIONS_DIR="$REPO_ROOT/knowledge-base/extractions/s19j"
        elif [ -d "$REPO_ROOT/knowledge-base/research/s19j/live-probe-139" ]; then
            EXTRACTIONS_DIR="$REPO_ROOT/knowledge-base/research/s19j/live-probe-139"
        elif [ -d "$REPO_ROOT/knowledge-base/extractions/s9" ]; then
            # CE-056: fail closed by default. An S9 kernel/DTB is a PLACEHOLDER
            # for am2-s19j — packaging a wrong-board AM2 image bricks the unit.
            # Only an explicit lab opt-in produces a clearly NON-FLASHABLE build.
            if [ "${DCENT_ALLOW_AM2_S9_PLACEHOLDER:-0}" != "1" ]; then
                error "am2-s19j: no real S19j kernel input (searched \$DCENT_EXTRACTIONS_DIR, $REPO_ROOT/knowledge-base/extractions/s19j, $REPO_ROOT/knowledge-base/research/s19j/live-probe-139). Refusing to package am2-s19j with an S9 placeholder kernel/DTB — a wrong-board AM2 image bricks the unit. Supply DCENT_AM2_S19J_KERNEL / a real s19j extractions dir, or set DCENT_ALLOW_AM2_S9_PLACEHOLDER=1 for an explicitly NON-FLASHABLE lab placeholder build."
            fi
            warn "am2-s19j: DCENT_ALLOW_AM2_S9_PLACEHOLDER=1 — NON-FLASHABLE S9-placeholder lab artifact only (DO NOT FLASH)."
            AM2_S9_PLACEHOLDER=1
            EXTRACTIONS_DIR="$REPO_ROOT/knowledge-base/extractions/s9"
        else
            error "No extractions dir for am2-s19j. Searched: \$DCENT_EXTRACTIONS_DIR, $REPO_ROOT/knowledge-base/extractions/s19j, $REPO_ROOT/knowledge-base/research/s19j/live-probe-139, $REPO_ROOT/knowledge-base/extractions/s9"
        fi
        BOARD_FAMILY="am2"
        ;;
    am2-s17)
        error "am2-s17 is not a valid sysupgrade board name — use 'am2-s19j' for S19j Pro Zynq builds (feedback_sysupgrade_board_name.md)."
        ;;
    *)
        error "Unsupported board: $BOARD_NAME (accepted: am1-s9, am2-s19j)"
        ;;
esac

# =============================================================================
# Version metadata
# =============================================================================

read_first_nonempty_line() {
    sed -n 's/^[[:space:]]*//;s/[[:space:]]*$//;/^$/!{p;q;}' "$1"
}

infer_package_version() {
    for candidate in \
        "$FIRMWARE_DIR/buildroot/output/target/etc/dcentos-version" \
        "$FIRMWARE_DIR/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-version"
    do
        if [ -f "$candidate" ]; then
            value=$(read_first_nonempty_line "$candidate")
            if [ -n "$value" ]; then
                PACKAGE_VERSION="$value"
                PACKAGE_VERSION_SOURCE="$candidate"
                return 0
            fi
        fi
    done

    if [ -f "$FIRMWARE_DIR/dcentrald/Cargo.toml" ]; then
        value=$(awk '
            /^\[workspace.package\]/ { in_pkg = 1; next }
            /^\[/ { in_pkg = 0 }
            in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
                gsub(/"/, "", $0)
                sub(/.*=[[:space:]]*/, "", $0)
                sub(/[[:space:]]*$/, "", $0)
                print $0
                exit
            }
        ' "$FIRMWARE_DIR/dcentrald/Cargo.toml")
        if [ -n "$value" ]; then
            PACKAGE_VERSION="$value"
            PACKAGE_VERSION_SOURCE="$FIRMWARE_DIR/dcentrald/Cargo.toml workspace.package.version"
            return 0
        fi
    fi

    return 1
}

if [ -z "$PACKAGE_VERSION" ]; then
    infer_package_version || true
elif [ -z "$PACKAGE_VERSION_SOURCE" ]; then
    PACKAGE_VERSION_SOURCE="DCENT_PACKAGE_VERSION"
fi

if [ -z "$PACKAGE_VERSION" ]; then
    error "Package version is required for fail-closed release manifests. Use --version or DCENT_PACKAGE_VERSION."
fi

case "$PACKAGE_VERSION" in
    *[!A-Za-z0-9._+:-]*)
        error "Package version contains unsupported characters: $PACKAGE_VERSION"
        ;;
esac

# F5: the OTA rollback gate (dcentrald_api_types::ota_rollback_protection::assess_rollback)
# compares versions by splitting on '.'/'-' and reading the LEADING numeric
# components. A non-semver version (e.g. a date tag "2026-07-02" or a git hash)
# parses to garbage numeric parts, so a firmware shipped with such a version
# poisons every future OTA comparison — a later legit semver OTA looks like a
# DOWNGRADE and is spuriously denied (availability). Require the leading semver
# core to be MAJOR.MINOR[.PATCH] with decimal-integer parts (optional leading
# 'v', optional '-'/'+' pre-release/build suffix), matching what assess_rollback
# can actually compare. The auto-inferred defaults (Cargo workspace version /
# dcentos-version) are already semver; this only rejects a bad --version override.
version_core=${PACKAGE_VERSION#v}
version_core=${version_core%%[-+]*}
case "$version_core" in
    "" | .* | *. | *..* | *[!0-9.]*)
        error "Package version '$PACKAGE_VERSION' is not a valid semver: the leading core must be MAJOR.MINOR[.PATCH] with decimal-integer parts (e.g. 0.9.0). assess_rollback compares numeric components, so a non-semver version can spuriously deny a legit OTA."
        ;;
esac
case "$version_core" in
    *.*) : ;;
    *)
        error "Package version '$PACKAGE_VERSION' must have at least MAJOR.MINOR (e.g. 0.9) so the OTA rollback numeric compare has two or more fields."
        ;;
esac

case "$PACKAGE_STATUS" in
    *[!A-Za-z0-9._+:-]*)
        error "Package status contains unsupported characters: $PACKAGE_STATUS"
        ;;
esac

# CE-183: a release-status package must not decouple from release-image
# hardening (root SSH lockdown + /etc/dcentos/release-image marker).
if is_release_status "$PACKAGE_STATUS" && ! is_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
    error "release-status package requires DCENT_RELEASE_IMAGE=1 (release-image hardening); set DCENT_PACKAGE_STATUS to a non-release lab value (e.g. lab_signed) for dev/lab packages (CE-183)."
fi

DCENT_BUILD_TARGET="${DCENT_BUILD_TARGET:-$BOARD_NAME}"
DCENT_PACKAGE_STATUS="$PACKAGE_STATUS"
dcent_release_provenance_init || error "Invalid or missing release provenance."
CANONICAL_BUILD_TIME=$(printf '%s' "$DCENT_CREATED_AT_UTC" | sed 's/T/ /; s/Z/ UTC/')

# =============================================================================
# Validate Prerequisites
# =============================================================================

header "DCENTos Sysupgrade Package Builder"

# Find rootfs
ROOTFS=""
for name in rootfs.squashfs; do
    if [ -f "$IMAGES_DIR/$name" ]; then
        ROOTFS="$IMAGES_DIR/$name"
        break
    fi
done
[ -n "$ROOTFS" ] || error "rootfs.squashfs not found in $IMAGES_DIR. Build firmware first."

# Find kernel
KERNEL=""
if [ -f "$EXTRACTIONS_DIR/kernel.bin" ]; then
    KERNEL="$EXTRACTIONS_DIR/kernel.bin"
elif [ -f "$EXTRACTIONS_DIR/mtd6_recovery.bin" ]; then
    # Need to extract kernel from recovery FIT
    info "Extracting kernel from recovery FIT image..."
    if command -v dumpimage >/dev/null 2>&1; then
        dumpimage -T flat_dt -p 0 -o "$IMAGES_DIR/kernel.bin" "$EXTRACTIONS_DIR/mtd6_recovery.bin"
        KERNEL="$IMAGES_DIR/kernel.bin"
    else
        error "dumpimage not found. Install u-boot-tools: sudo apt install u-boot-tools"
    fi
fi
[ -n "$KERNEL" ] || error "Kernel not found. Run extract_boot_components.sh first."

# =============================================================================
# BUG 1 (build) fix: emit a bootable FIT kernel, never a bare zImage.
# =============================================================================
#
# The BraiinsOS-extracted kernel.bin is a BARE ARM zImage (its first bytes are
# the ARM nop/branch stream, e.g. "0000a0e1..."; the zImage magic 0x016f2818
# sits at byte offset 0x24, not 0x00). U-Boot's `bootm` on the S9/Zynq NAND
# path CANNOT boot a raw zImage — it expects a FIT (d00dfeed) or a legacy
# uImage (27051956). Copying the raw zImage straight into sysupgrade-<board>/
# kernel is exactly what bricked .135.
#
# The proven SD build (buildroot/output/images/sd_card/dcentos-sd.its) wraps
# this same kernel as a FIT. Here we wrap it into a NAND-boot FIT that mirrors
# that recipe MINUS the ramdisk node (NAND rootfs comes from the UBI volume,
# not an initramfs) — kernel + S9 DTB, load/entry 0x8000 — so the manual
# mkimage step that produced nand-kernel-fit-118 is never needed again.
#
# Idempotent: if the located kernel is ALREADY a FIT/uImage (e.g. a future
# extraction or a pre-wrapped artifact), we use it as-is.
case "$BOARD_NAME" in
    am1-s9|am2-s19j)
        if kernel_is_bootm_ready "$KERNEL"; then
            info "Kernel '$(basename "$KERNEL")' is already a bootable container (magic=$(file_magic_hex "$KERNEL")) — using as-is."
        else
            info "Kernel '$(basename "$KERNEL")' is a bare zImage (magic=$(file_magic_hex "$KERNEL")) — wrapping into a bootable FIT (kernel + S9 DTB, no ramdisk)..."
            FIT_DTB="$EXTRACTIONS_DIR/s9_devicetree.dtb"
            FIT_WORKDIR="$IMAGES_DIR/nand-kernel-fit-${BOARD_NAME}"
            FIT_KERNEL=$(build_nand_kernel_fit \
                "$KERNEL" \
                "$FIT_DTB" \
                "$FIT_WORKDIR" \
                "DCENT_OS $BOARD_NAME NAND kernel (Linux 4.4.92 BraiinsOS + S9 DTB, no ramdisk)")
            KERNEL="$FIT_KERNEL"
            info "  Wrapped FIT kernel: $KERNEL (magic=$(file_magic_hex "$KERNEL"))"
        fi
        ;;
    *)
        # No other board reaches this builder (the case statement above only
        # accepts am1-s9 / am2-s19j), but fail closed rather than silently
        # shipping an unvalidated kernel container if that ever changes.
        kernel_is_bootm_ready "$KERNEL" \
            || error "Kernel for board '$BOARD_NAME' is not a bootable FIT/uImage (magic=$(file_magic_hex "$KERNEL")) and no FIT-wrap recipe is defined for it."
        ;;
esac

ROOTFS_SIZE=$(stat -c%s "$ROOTFS" 2>/dev/null || stat -f%z "$ROOTFS" 2>/dev/null)
KERNEL_SIZE=$(stat -c%s "$KERNEL" 2>/dev/null || stat -f%z "$KERNEL" 2>/dev/null)

info "Kernel: $(basename "$KERNEL") ($((KERNEL_SIZE / 1024)) KB)"
info "Rootfs: $(basename "$ROOTFS") ($((ROOTFS_SIZE / 1024)) KB)"
info "Board:  $BOARD_NAME"
info "Version: $PACKAGE_VERSION (${PACKAGE_VERSION_SOURCE:-DCENT_PACKAGE_VERSION})"
info "Status: $PACKAGE_STATUS"

BUILD_TARGET_DIR="$FIRMWARE_DIR/buildroot/output/target"
if [ -f "$BUILD_TARGET_DIR/usr/local/bin/dcentrald" ]; then
    dcent_require_dcentrald_version_match \
        "$BUILD_TARGET_DIR" \
        "$BUILD_TARGET_DIR/usr/local/bin/dcentrald" \
        "package_sysupgrade" \
        "$FIRMWARE_DIR/dcentrald/Cargo.toml"
fi

# =============================================================================
# Build Sysupgrade Tar
# =============================================================================

header "Building Sysupgrade Package"

# Create temp directory with sysupgrade structure
STAGING=$(mktemp -d)
# CE-056: a NON-FLASHABLE am2-s19j S9-placeholder lab build (opt-in only via
# DCENT_ALLOW_AM2_S9_PLACEHOLDER=1) carries a loud non-flashable marker in BOTH
# the staged sysupgrade subdir name AND the output tar filename, so it can never
# be mistaken for / uploaded as a flashable am2-s19j image (the on-target
# sysupgrade greps for the exact "sysupgrade-<board>/" member and will reject
# the renamed subdir). The real am2-s19j / am1-s9 paths are byte-unchanged.
SYSUPGRADE_SUBDIR="sysupgrade-$BOARD_NAME"
if [ "$AM2_S9_PLACEHOLDER" = "1" ]; then
    SYSUPGRADE_SUBDIR="sysupgrade-$BOARD_NAME-PLACEHOLDER-DO-NOT-FLASH"
    case "$OUTPUT_FILE" in
        *PLACEHOLDER-DO-NOT-FLASH*) : ;;
        *.tar) OUTPUT_FILE="${OUTPUT_FILE%.tar}-PLACEHOLDER-DO-NOT-FLASH.tar" ;;
        *)     OUTPUT_FILE="${OUTPUT_FILE}-PLACEHOLDER-DO-NOT-FLASH" ;;
    esac
fi
SYSUPGRADE_DIR="$STAGING/$SYSUPGRADE_SUBDIR"
mkdir -p "$SYSUPGRADE_DIR"

# Copy kernel and rootfs with expected names
cp "$KERNEL" "$SYSUPGRADE_DIR/kernel"
cp "$ROOTFS" "$SYSUPGRADE_DIR/root"

# Brick-safety gate: the staged kernel member MUST be a bootm-ready container
# (FIT d00dfeed or legacy uImage 27051956). A bare zImage here is the .135
# brick. This is the last line of defense before the tar is sealed.
kernel_is_bootm_ready "$SYSUPGRADE_DIR/kernel" \
    || error "Staged sysupgrade kernel is not bootm-ready (magic=$(file_magic_hex "$SYSUPGRADE_DIR/kernel")). A bare zImage WILL brick the unit — refusing to package."
info "Kernel member is bootm-ready (magic=$(file_magic_hex "$SYSUPGRADE_DIR/kernel"))."

KERNEL_SHA256=$(sha256sum "$SYSUPGRADE_DIR/kernel" | awk '{print $1}')
ROOTFS_SHA256=$(sha256sum "$SYSUPGRADE_DIR/root" | awk '{print $1}')

info "Staging directory:"
info "  $SYSUPGRADE_SUBDIR/"
info "    kernel  ($((KERNEL_SIZE / 1024)) KB)"
info "    root    ($((ROOTFS_SIZE / 1024)) KB)"

# Create metadata file (some OpenWrt builds check this)
cat > "$SYSUPGRADE_DIR/METADATA" << EOF
DCENT_OS
D-Central Technologies
Build: $CANONICAL_BUILD_TIME
Board: $BOARD_NAME
Kernel: BraiinsOS 4.4.x (preserved)
Rootfs: DCENTos (Buildroot)
EOF

METADATA_SIZE=$(stat -c%s "$SYSUPGRADE_DIR/METADATA" 2>/dev/null || stat -f%z "$SYSUPGRADE_DIR/METADATA" 2>/dev/null)
METADATA_SHA256=$(sha256sum "$SYSUPGRADE_DIR/METADATA" | awk '{print $1}')

cat > "$SYSUPGRADE_DIR/SHA256SUMS" << EOF
$KERNEL_SHA256  kernel
$ROOTFS_SHA256  root
$METADATA_SHA256  METADATA
EOF

cat > "$SYSUPGRADE_DIR/MANIFEST.json" << EOF
{
  "schema": 1,
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sysupgrade",
  "board_family": "$BOARD_FAMILY",
  "board": "$BOARD_NAME",
  "board_target": "$BOARD_NAME",
  "version": "$PACKAGE_VERSION",
  "created_at_utc": "$DCENT_CREATED_AT_UTC",
  "status": "$PACKAGE_STATUS",
  "provenance": {
    "source_commit": "$DCENT_SOURCE_COMMIT",
    "source_tree_state": "$DCENT_SOURCE_TREE_STATE",
    "source_date_epoch": $SOURCE_DATE_EPOCH,
    "source_commit_epoch": $DCENT_SOURCE_COMMIT_EPOCH,
    "build_target": "$DCENT_BUILD_TARGET",
    "build_arch": "$DCENT_BUILD_ARCH",
    "toolchain_id": "$DCENT_TOOLCHAIN_ID"
  },
  "payloads": {
    "kernel": {
      "path": "$SYSUPGRADE_SUBDIR/kernel",
      "size": $KERNEL_SIZE,
      "sha256": "$KERNEL_SHA256"
    },
    "rootfs": {
      "path": "$SYSUPGRADE_SUBDIR/root",
      "size": $ROOTFS_SIZE,
      "sha256": "$ROOTFS_SHA256"
    },
    "metadata": {
      "path": "$SYSUPGRADE_SUBDIR/METADATA",
      "size": $METADATA_SIZE,
      "sha256": "$METADATA_SHA256"
    }
  },
  "toolbox": {
    "install_command": "dcent install <ip> -f dcentos-sysupgrade.tar",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade.tar",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": true
  }
}
EOF

SIGNED_PACKAGE=false
if [ -n "$SIGNING_KEY" ]; then
    command -v openssl >/dev/null 2>&1 || error "openssl is required to sign MANIFEST.json"
    [ -f "$SIGNING_KEY" ] || error "Signing key not found: $SIGNING_KEY"
    if [ -n "$VERIFY_PUBKEY" ]; then
        [ -f "$VERIFY_PUBKEY" ] || error "Verification public key not found: $VERIFY_PUBKEY"
        PACKAGE_PUBKEY="$VERIFY_PUBKEY"
    else
        if [ "$REQUIRE_RELEASE_KEY" = "1" ]; then
            error "DCENT_REQUIRE_RELEASE_KEY=1 but no trusted public key was provided. Use --verify-pubkey or DCENT_RELEASE_PUBKEY_FILE."
        fi
        if is_release_status "$PACKAGE_STATUS"; then
            error "Production release signing requires --verify-pubkey or DCENT_RELEASE_PUBKEY_FILE; refusing self-derived generated-key package."
        fi
        require_unsigned_lab_override "self-derived generated-key package generation"
        PACKAGE_PUBKEY="$STAGING/release_ed25519.pub"
        openssl pkey -in "$SIGNING_KEY" -pubout -out "$PACKAGE_PUBKEY" >/dev/null 2>&1 \
            || error "Failed to derive public key from $SIGNING_KEY"
        warn "Derived release_ed25519.pub from signing key - generated-key package is lab-only."
    fi
    cp "$PACKAGE_PUBKEY" "$SYSUPGRADE_DIR/release_ed25519.pub"
    PACKAGE_PUBKEY_SIZE=$(stat -c%s "$SYSUPGRADE_DIR/release_ed25519.pub" 2>/dev/null || stat -f%z "$SYSUPGRADE_DIR/release_ed25519.pub" 2>/dev/null)
    PACKAGE_PUBKEY_HASH=$(sha256sum "$SYSUPGRADE_DIR/release_ed25519.pub" | awk '{print $1}')
    printf '%s  %s\n' "$PACKAGE_PUBKEY_HASH" "release_ed25519.pub" >> "$SYSUPGRADE_DIR/SHA256SUMS"

    cat > "$SYSUPGRADE_DIR/MANIFEST.json" << EOF
{
  "schema": 1,
  "product": "DCENT_OS",
  "family": "antminer",
  "package_type": "sysupgrade",
  "board_family": "$BOARD_FAMILY",
  "board": "$BOARD_NAME",
  "board_target": "$BOARD_NAME",
  "version": "$PACKAGE_VERSION",
  "created_at_utc": "$DCENT_CREATED_AT_UTC",
  "status": "$PACKAGE_STATUS",
  "provenance": {
    "source_commit": "$DCENT_SOURCE_COMMIT",
    "source_tree_state": "$DCENT_SOURCE_TREE_STATE",
    "source_date_epoch": $SOURCE_DATE_EPOCH,
    "source_commit_epoch": $DCENT_SOURCE_COMMIT_EPOCH,
    "build_target": "$DCENT_BUILD_TARGET",
    "build_arch": "$DCENT_BUILD_ARCH",
    "toolchain_id": "$DCENT_TOOLCHAIN_ID"
  },
  "payloads": {
    "kernel": {
      "path": "$SYSUPGRADE_SUBDIR/kernel",
      "size": $KERNEL_SIZE,
      "sha256": "$KERNEL_SHA256"
    },
    "rootfs": {
      "path": "$SYSUPGRADE_SUBDIR/root",
      "size": $ROOTFS_SIZE,
      "sha256": "$ROOTFS_SHA256"
    },
    "metadata": {
      "path": "$SYSUPGRADE_SUBDIR/METADATA",
      "size": $METADATA_SIZE,
      "sha256": "$METADATA_SHA256"
    },
    "verification_key": {
      "path": "$SYSUPGRADE_SUBDIR/release_ed25519.pub",
      "size": $PACKAGE_PUBKEY_SIZE,
      "sha256": "$PACKAGE_PUBKEY_HASH"
    }
  },
  "toolbox": {
    "install_command": "dcent install <ip> -f dcentos-sysupgrade.tar",
    "update_command": "dcent install <ip> -f dcentos-sysupgrade.tar",
    "upload_endpoint": null,
    "board_target_header": null,
    "requires_inactive_slot": true
  }
}
EOF
    openssl pkeyutl -sign -rawin -inkey "$SIGNING_KEY" -in "$SYSUPGRADE_DIR/MANIFEST.json" -out "$SYSUPGRADE_DIR/MANIFEST.sig" \
        || error "Failed to sign MANIFEST.json with $SIGNING_KEY"
    if [ -n "$PACKAGE_PUBKEY" ]; then
        openssl pkeyutl -verify -rawin -pubin -inkey "$PACKAGE_PUBKEY" -sigfile "$SYSUPGRADE_DIR/MANIFEST.sig" -in "$SYSUPGRADE_DIR/MANIFEST.json" >/dev/null \
            || error "MANIFEST.sig verification failed against package verification key"
    elif [ "$REQUIRE_RELEASE_KEY" = "1" ]; then
        error "DCENT_REQUIRE_RELEASE_KEY=1 but no verification public key was provided. Use --verify-pubkey or DCENT_RELEASE_PUBKEY_FILE."
    fi
    SIGNED_PACKAGE=true
    info "Signed MANIFEST.json -> MANIFEST.sig"
    info "Embedded release_ed25519.pub (sha256: $PACKAGE_PUBKEY_HASH)"
else
    require_unsigned_lab_override "unsigned package generation"
    warn "No signing key configured — package is unsigned and lab-only."
fi

# Build the tar archive
# OpenWrt sysupgrade expects a plain tar (no compression) with the
# sysupgrade-<board>/ directory at the top level.
info "Creating sysupgrade tar..."
dcent_create_deterministic_tar "$OUTPUT_FILE" "$STAGING" "$SYSUPGRADE_SUBDIR"

# Cleanup staging
rm -rf "$STAGING"

OUTPUT_SIZE=$(stat -c%s "$OUTPUT_FILE" 2>/dev/null || stat -f%z "$OUTPUT_FILE" 2>/dev/null)
info "Output: $OUTPUT_FILE"
info "Size:   $((OUTPUT_SIZE / 1024)) KB ($OUTPUT_SIZE bytes)"

# Verify tar contents
info "Verifying tar contents..."
tar tf "$OUTPUT_FILE" | while read -r line; do
    echo "  $line"
done

tar tf "$OUTPUT_FILE" | grep -q "$SYSUPGRADE_SUBDIR/MANIFEST.json" || \
    error "Manifest missing from sysupgrade tar"
if $SIGNED_PACKAGE; then
    tar tf "$OUTPUT_FILE" | grep -q "$SYSUPGRADE_SUBDIR/MANIFEST.sig" || \
        error "Signature missing from signed sysupgrade tar"
    tar tf "$OUTPUT_FILE" | grep -q "$SYSUPGRADE_SUBDIR/release_ed25519.pub" || \
        error "Verification key missing from signed sysupgrade tar"
fi

# Generate checksums
SHA256=$(sha256sum "$OUTPUT_FILE" | awk '{print $1}')
MD5=$(md5sum "$OUTPUT_FILE" | awk '{print $1}')
info "SHA256: $SHA256"
info "MD5:    $MD5"

# =============================================================================
# Upload and Flash (Optional)
# =============================================================================

if [ -n "$UPLOAD_IP" ]; then
    header "Uploading to $UPLOAD_IP"

    SSH_OPTS="-o StrictHostKeyChecking=no -o ConnectTimeout=10"
    SSH_CMD="ssh $SSH_OPTS root@$UPLOAD_IP"
    SCP_CMD="scp -O $SSH_OPTS"

    # Test connection
    info "Testing SSH connection..."
    $SSH_CMD "echo OK" >/dev/null 2>&1 || \
        error "Cannot SSH to root@$UPLOAD_IP (BraiinsOS default: empty password)"

    # Verify it's BraiinsOS
    FW_TYPE=$($SSH_CMD "
        if [ -f /etc/bos_version ]; then echo braiinsos;
        elif [ -f /etc/dcentos-version ]; then echo dcentos;
        else echo unknown; fi
    " 2>/dev/null)

    if [ "$FW_TYPE" != "braiinsos" ]; then
        error "Target does not appear to be running BraiinsOS (detected: $FW_TYPE). This sysupgrade packager only supports the validated BraiinsOS sysupgrade path."
    fi

    REMOTE_INFO=$($SSH_CMD '
        MODEL="$(cat /config/CONF_MINER_TYPE 2>/dev/null || echo)"
        HWID="$(cat /config/CONF_HARDWARE_ID 2>/dev/null || echo)"
        SOC="$(cat /sys/devices/soc0/soc_id 2>/dev/null || echo unknown)"
        UIO_COUNT=$(find /sys/class/uio -maxdepth 1 -name "uio*" 2>/dev/null | wc -l)
        echo "MODEL=$MODEL"
        echo "HWID=$HWID"
        echo "SOC=$SOC"
        echo "UIO_COUNT=$UIO_COUNT"
    ' 2>/dev/null) || true
    eval "$REMOTE_INFO" 2>/dev/null || true
    MODEL_LC=$(printf '%s' "${MODEL:-}" | tr '[:upper:]' '[:lower:]')
    HWID_LC=$(printf '%s' "${HWID:-}" | tr '[:upper:]' '[:lower:]')
    case "$BOARD_NAME" in
        am1-s9)
            if [[ "$MODEL_LC" != *"s9"* ]] && [[ "$HWID_LC" != *"am1"* ]] && [ "${UIO_COUNT:-0}" -gt 16 ]; then
                error "Remote target does not look like validated am1-s9 hardware (model=${MODEL:-unknown}, hwid=${HWID:-unknown}, uio=${UIO_COUNT:-0})."
            fi
            ;;
        am2-s19j)
            # am2-s19j live upload is NOT yet validated — refuse unless the
            # operator forces it (Phase 3 will lift this gate). This avoids
            # an unattended write request for an unvalidated package.
            if [ "${DCENT_FORCE_AM2_UPLOAD:-0}" != "1" ]; then
                error "Live --upload of am2-s19j packages is gated pending Phase 3 validation. Set DCENT_FORCE_AM2_UPLOAD=1 to override for a controlled lab sysupgrade request on .139."
            fi
            if [[ "$MODEL_LC" != *"s19"* ]] && [[ "$HWID_LC" != *"am2"* ]]; then
                error "Remote target does not look like am2-s19j hardware (model=${MODEL:-unknown}, hwid=${HWID:-unknown})."
            fi
            ;;
    esac

    if ! $SIGNED_PACKAGE && ! $ALLOW_UNSIGNED_UPLOAD; then
        error "Refusing live upload of an unsigned package. Re-run with --signing-key <pem> or --allow-unsigned-upload for a controlled lab workflow."
    fi

    if $SIGNED_PACKAGE; then
        REMOTE_OPENSSL=$($SSH_CMD "command -v openssl >/dev/null 2>&1 && echo yes || echo no" 2>/dev/null)
        [ "$REMOTE_OPENSSL" = "yes" ] || error "Remote target is missing openssl, so it cannot verify MANIFEST.sig. Rebuild the image with the release verifier first."
        REMOTE_RELEASE_KEY=$($SSH_CMD "[ -f /etc/dcentos/release_ed25519.pub ] && echo yes || echo no" 2>/dev/null)
        [ "$REMOTE_RELEASE_KEY" = "yes" ] || error "Remote target is missing /etc/dcentos/release_ed25519.pub. Rebuild the image with DCENT_RELEASE_PUBKEY_FILE first."
    fi

    # /data free-space preflight — sysupgrade tarballs (~22 MB) plus the
    # extracted squashfs blow the 64 MB tmpfs at /tmp on S9. Stage on /data
    # (per-slot UBI, ~120-500 MB free typically) instead. Refuse upload if
    # /data is missing, not writable, or under 50 MB free.
    info "Checking /data free space (sysupgrade tarball + extraction)..."
    DATA_FREE_KB=$($SSH_CMD "df -Pk /data 2>/dev/null | awk 'NR==2 {print \$4}'" 2>/dev/null) || DATA_FREE_KB=""
    if [ -z "$DATA_FREE_KB" ] || ! printf '%s' "$DATA_FREE_KB" | grep -Eq '^[0-9]+$'; then
        error "Could not determine /data free space on $UPLOAD_IP. /data is required for sysupgrade staging."
    fi
    info "  /data free: $((DATA_FREE_KB / 1024)) MB"
    if [ "$DATA_FREE_KB" -lt 51200 ]; then
        error "/data has only $((DATA_FREE_KB / 1024)) MB free; need at least 50 MB for sysupgrade staging. Clear /data/dcentos-sysupgrade.tar or other staged artifacts first."
    fi
    DATA_WRITABLE=$($SSH_CMD "touch /data/.dcent_stage_check 2>/dev/null && rm -f /data/.dcent_stage_check && echo yes || echo no" 2>/dev/null)
    if [ "$DATA_WRITABLE" != "yes" ]; then
        error "/data is not writable on $UPLOAD_IP. sysupgrade staging requires a writable /data."
    fi

    # Upload to /data (NOT /tmp) —.
    info "Uploading sysupgrade package ($((OUTPUT_SIZE / 1024)) KB) to /data..."
    $SCP_CMD "$OUTPUT_FILE" "root@$UPLOAD_IP:/data/dcentos-sysupgrade.tar"

    # Verify upload
    REMOTE_SIZE=$($SSH_CMD "stat -c%s /data/dcentos-sysupgrade.tar 2>/dev/null || echo 0" 2>/dev/null)
    if [ "$REMOTE_SIZE" != "$OUTPUT_SIZE" ]; then
        error "Upload size mismatch! Local: $OUTPUT_SIZE, Remote: $REMOTE_SIZE"
    fi
    info "Upload size verified."

    # Check if sysupgrade exists
    HAS_SYSUPGRADE=$($SSH_CMD "command -v sysupgrade >/dev/null 2>&1 && echo yes || echo no" 2>/dev/null)

    if [ "$HAS_SYSUPGRADE" = "yes" ]; then
        info "Running target-side sysupgrade verification..."
        $SSH_CMD "sysupgrade --test /data/dcentos-sysupgrade.tar" >/dev/null 2>&1 || \
            error "Target-side sysupgrade verification failed. Refusing to request sysupgrade."

        echo ""
        echo -e "${YELLOW}${BOLD}Ready to request sysupgrade on the validated S9/BraiinsOS path.${NC}"
        echo ""
        echo "This will:"
        echo "  1. Stop bosminer"
        echo "  2. Ask the target sysupgrade flow to stage/write the inactive firmware slot"
        echo "  3. Request reboot into the staged DCENTos image"
        echo ""
        echo -e "${RED}The miner will be unreachable during sysupgrade/reboot (~2 minutes).${NC}"
        echo ""

        read -p "Start sysupgrade now? (yes/no): " FLASH
        if [ "$FLASH" = "yes" ]; then
            info "Stopping bosminer..."
            $SSH_CMD "killall bosminer 2>/dev/null; sleep 1" 2>/dev/null || true

            info "Requesting sysupgrade (this will disconnect SSH)..."
            # sysupgrade -n = no config preservation (clean DCENTos install)
            # The -n flag ensures BraiinsOS config doesn't leak into DCENTos
            $SSH_CMD "sysupgrade -n /data/dcentos-sysupgrade.tar" 2>/dev/null || true

            echo ""
            info "Sysupgrade initiated. The miner will reboot automatically."
            info "Wait ~120 seconds, then reconnect:"
            echo ""
            echo -e "  ${GREEN}ssh root@$UPLOAD_IP${NC}  (bootstrap password: ${BOLD}dcentral${NC})"
            echo "  Then open http://$UPLOAD_IP/ and set the dashboard owner password."
            echo ""

            sleep 120
            # Try connecting
            for i in 1 2 3; do
                RESULT=$(ssh $SSH_OPTS -o ConnectTimeout=5 root@$UPLOAD_IP \
                    "echo ALIVE; cat /etc/dcentos-version 2>/dev/null" 2>/dev/null) || true
                if echo "$RESULT" | grep -q "ALIVE"; then
                    echo ""
                    echo -e "${GREEN}${BOLD}=== MANAGEMENT REACHABLE ===${NC}"
                    echo "Target answered SSH after the sysupgrade request."
                    echo "$RESULT"
                    echo "Management reachability only; package version, rollback commitment, and mining are not proven here."
                    exit 0
                fi
                echo "  Retry $i/3... (waiting 15s)"
                sleep 15
            done

            warn "Could not confirm management reachability. The target may still be booting."
        fi
    else
        warn "sysupgrade command not found on target. Refusing to print unsafe manual active-slot flash commands."
        echo ""
        echo "Install/update a target that already provides /usr/sbin/sysupgrade,"
        echo "or use an SD-card migration path that preserves A/B slot safety."
    fi
fi

# =============================================================================
# Summary
# =============================================================================

if [ -z "$UPLOAD_IP" ]; then
    header "Package Ready"

    echo ""
    echo -e "${BOLD}Output:${NC} $OUTPUT_FILE ($((OUTPUT_SIZE / 1024)) KB)"
    echo -e "${BOLD}Integrity:${NC} MANIFEST.json + SHA256SUMS embedded in package"
    echo ""
    echo -e "${BOLD}Installation methods:${NC}"
    echo ""
    echo "  1. This script with --upload (S9 / am1-s9 only):"
    echo "     $(basename "$0") --upload <miner_ip>"
    echo ""
    echo "  2. SSH + sysupgrade (stage on /data — /tmp is 64 MB tmpfs on S9):"
    echo "     scp -O $OUTPUT_FILE root@<miner_ip>:/data/"
    echo "     ssh root@<miner_ip> 'sysupgrade -n /data/$(basename "$OUTPUT_FILE")'"
    echo ""
    echo "  3. BraiinsOS Web UI compatibility upload:"
    echo "     Open http://<miner_ip> → System → Firmware → Flash Image"
    echo "     Upload: $(basename "$OUTPUT_FILE")"
    echo "     Check 'Do not preserve settings' → Flash"
    echo ""
    echo -e "  After target reboot and management reachability: ${GREEN}ssh root@<miner_ip>${NC} (bootstrap password: ${BOLD}dcentral${NC})"
    echo "  First reachable boot: open the dashboard and set the owner password before using the API."
    echo ""

    # Note about signature verification
     echo -e "${YELLOW}NOTE:${NC} This package path is currently validated only for am1-s9."
     if $SIGNED_PACKAGE; then
          echo -e "${GREEN}Signing:${NC} MANIFEST.sig included (signed package)"
          echo "Verification key: release_ed25519.pub embedded in package"
      else
          echo -e "${YELLOW}Signing:${NC} unsigned lab-only package"
      fi
     echo "AM2 and Amlogic install flows remain blocked until board-specific artifacts, recovery,"
     echo "and rollback semantics are validated."
     echo ""
fi
