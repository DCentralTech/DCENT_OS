#!/usr/bin/env bash
#
# build_cv1835_s19jpro.sh — Cross-compile dcentrald + run Buildroot for
# cv1835-s19jpro. Output: dcentos-sysupgrade-cv1835-s19jpro.tar in
# DCENT_OS_Antminer/output/.
#
# Status: runtime-only / no fleet unit. The build runs end-to-end (dcentrald
# is armv7-unknown-linux-musleabihf same as Zynq + AM335x BB), but the
# resulting tarball cannot be flashed live until DCENT_CV1835_EMMC_PROVEN=1
# is set + 3 round-trip bench proof is captured. See
# DCENT_OS_Antminer/br2_external_dcentos/board/cvitek/cv1835-s19jpro/README.md
# for the promotion criteria checklist.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DCENTRALD_DIR="$PROJECT_DIR/dcentrald"
BUILDROOT_DIR="${BUILDROOT_DIR:-$PROJECT_DIR/buildroot}"
BR2_EXTERNAL="$PROJECT_DIR/br2_external_dcentos"
OUTPUT_DIR="$PROJECT_DIR/output"
DEFCONFIG="dcentos_cv1835_s19jpro_defconfig"
BOARD_TARGET="cv1835-s19jpro"

usage() {
    cat <<EOF
Usage: $(basename "$0") [--skip-cargo] [--skip-buildroot]

Builds:
  1. cargo build --release --target armv7-unknown-linux-musleabihf
     (skipped if --skip-cargo)
  2. make $DEFCONFIG && make
     in $BUILDROOT_DIR with BR2_EXTERNAL=$BR2_EXTERNAL
     (skipped if --skip-buildroot)
  3. Copies output/images/dcentos-sysupgrade-${BOARD_TARGET}.tar into
     $OUTPUT_DIR/

Environment overrides:
  BUILDROOT_DIR    Path to buildroot tree (default: $BUILDROOT_DIR)
EOF
}

SKIP_CARGO=0
SKIP_BR=0
for arg in "$@"; do
    case "$arg" in
        --skip-cargo)     SKIP_CARGO=1 ;;
        --skip-buildroot) SKIP_BR=1 ;;
        -h|--help)        usage; exit 0 ;;
        *) echo "ERROR: unknown arg: $arg" >&2; usage; exit 2 ;;
    esac
done

mkdir -p "$OUTPUT_DIR"

if [ "$SKIP_CARGO" -eq 0 ]; then
    echo "==> Cross-compile dcentrald (armv7-unknown-linux-musleabihf)"
    cd "$DCENTRALD_DIR"
    cargo build --release --target armv7-unknown-linux-musleabihf
    BIN="$DCENTRALD_DIR/target/armv7-unknown-linux-musleabihf/release/dcentrald"
    if [ ! -f "$BIN" ]; then
        echo "ERROR: dcentrald binary not produced at $BIN" >&2
        exit 3
    fi
    BIN_SIZE=$(stat -c%s "$BIN" 2>/dev/null || stat -f%z "$BIN")
    echo "    dcentrald: $BIN ($BIN_SIZE bytes)"
fi

if [ "$SKIP_BR" -eq 0 ]; then
    if [ ! -d "$BUILDROOT_DIR" ]; then
        echo "ERROR: BUILDROOT_DIR not found: $BUILDROOT_DIR" >&2
        echo "       Set BUILDROOT_DIR=/path/to/buildroot or place a tree at" >&2
        echo "       $BUILDROOT_DIR" >&2
        exit 4
    fi
    echo "==> Buildroot defconfig: $DEFCONFIG"
    cd "$BUILDROOT_DIR"
    make BR2_EXTERNAL="$BR2_EXTERNAL" "$DEFCONFIG"
    echo "==> Buildroot make"
    make BR2_EXTERNAL="$BR2_EXTERNAL"
fi

TAR_SRC="$BUILDROOT_DIR/output/images/dcentos-sysupgrade-${BOARD_TARGET}.tar"
TAR_DST="$OUTPUT_DIR/dcentos-sysupgrade-${BOARD_TARGET}.tar"
if [ -f "$TAR_SRC" ]; then
    cp "$TAR_SRC" "$TAR_DST"
    TAR_SIZE=$(stat -c%s "$TAR_DST" 2>/dev/null || stat -f%z "$TAR_DST")
    echo "==> Sysupgrade tarball: $TAR_DST ($TAR_SIZE bytes)"
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$TAR_DST" > "${TAR_DST}.sha256"
        echo "    SHA256: $(awk '{print $1}' "${TAR_DST}.sha256")"
    fi
else
    echo "ERROR: sysupgrade tarball not produced at $TAR_SRC" >&2
    exit 5
fi

cat <<EOF

Build complete. Status: runtime-only / no fleet unit.

Next steps before live flash:
  1. Acquire a CV1835 S19j Pro carrier on the bench.
  2. Capture live mmc part output, store at
     docs/dev/<date>-cv1835-emmc-evidence/mmc_part_dump.txt.
  3. Drop the BMU-extracted factory kernel at
     knowledge-base/firmware-archive/cv1835/factory_kernel.bin and
     rebuild so post-build.sh stages /config/factory_kernel.bin.
  4. Run safe_sysupgrade_cv_emmc.sh --dry-run on the bench unit first.
  5. After 3 successful round-trip flashes, set
     DCENT_CV1835_EMMC_PROVEN=1 and land
     feedback_cv1835_emmc_recovery_proven.md.
EOF
