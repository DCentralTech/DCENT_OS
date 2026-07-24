#!/bin/sh
# Prove that package windows are pinned to captured Zynq NAND geometry rather
# than to the host or nandsim kernel's UBI geometry.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
HELPER=$PROJECT_ROOT/scripts/lib/sysupgrade_zynq_geometry.sh
tests=0
failures=0

# shellcheck source=/dev/null
. "$HELPER"

pass() { tests=$((tests + 1)); printf 'PASS: %s\n' "$1"; }
fail() { tests=$((tests + 1)); failures=$((failures + 1)); printf 'FAIL: %s\n' "$1" >&2; }

expect_equal()
{
    _label=$1
    _expected=$2
    _actual=$3
    if [ "$_actual" = "$_expected" ]; then pass "$_label"; else fail "$_label (expected=$_expected actual=$_actual)"; fi
}

expect_success()
{
    _label=$1
    shift
    if "$@" >/dev/null 2>&1; then pass "$_label"; else fail "$_label"; fi
}

expect_failure()
{
    _label=$1
    shift
    if "$@" >/dev/null 2>&1; then fail "$_label (unexpected success)"; else pass "$_label"; fi
}

expect_equal "captured Xilinx UBI LEB size is exact bytes, not a host query" \
    126976 "$ZYNQ_UBI_LEB_SIZE_BYTES"
expect_equal "AM2 stock kernel window is exactly 23 captured LEBs" \
    2920448 "$AM2_ZYNQ_KERNEL_MAX_BYTES"
expect_equal "AM2 rootfs window is exactly 179 captured LEBs" \
    22728704 "$AM2_ZYNQ_ROOTFS_MAX_BYTES"
expect_equal "S9 rootfs window is exactly 134 captured LEBs" \
    17014784 "$AM1_S9_ROOTFS_MAX_BYTES"
expect_equal "layout-tolerance tar bound remains distinct from package fit" \
    3428352 "$AM2_ZYNQ_KERNEL_TAR_BOUND_BYTES"

expect_success "exact AM2 kernel boundary is admitted" \
    dcent_zynq_geometry_require_payload_fit am2-s19j kernel 2920448
expect_failure "one byte beyond the AM2 kernel boundary is refused" \
    dcent_zynq_geometry_require_payload_fit am2-s19j kernel 2920449
expect_success "exact AM2 rootfs boundary is admitted" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 22728704
expect_failure "one byte beyond the AM2 rootfs boundary is refused" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 22728705
expect_success "exact S9 kernel boundary is admitted" \
    dcent_zynq_geometry_require_payload_fit am1-s9 kernel 4063232
expect_failure "one byte beyond the S9 kernel boundary is refused" \
    dcent_zynq_geometry_require_payload_fit am1-s9 kernel 4063233

# Preserved beta20260617/root was 22,978,560 bytes.  Nandsim's 129,024-byte
# LEB made that fit 179 emulated LEBs, but it requires 181 real 126,976-byte
# LEBs and must never pass a real-target release gate again.
expect_failure "preserved oversized AM2 beta rootfs regression is refused" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 22978560

expect_equal "AM2 nandsim window is demonstrably larger than real target" \
    23095296 "$((179 * 129024))"
expect_failure "nandsim-only rootfs fit cannot redefine the AM2 package profile" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 23095296

# Preserved XIL1 beta20260617/root was 20,819,968 bytes and its SquashFS
# bytes_used field was 20,816,636.  The content alone needs 164 real LEBs,
# so signature/publication evidence cannot make it fit the 134-LEB S9 ABI.
expect_failure "preserved oversized S9 beta rootfs regression is refused" \
    dcent_zynq_geometry_require_payload_fit am1-s9 rootfs 20819968
expect_equal "preserved S9 SquashFS content needs 164 real LEBs" \
    164 "$(((20816636 + ZYNQ_UBI_LEB_SIZE_BYTES - 1) / ZYNQ_UBI_LEB_SIZE_BYTES))"

expect_success "S19 Pro remains bounded by the conservative AM2 window" \
    dcent_zynq_geometry_require_payload_fit am2-s19pro rootfs 22728704
expect_success "S17 Pro remains bounded without claiming production maturity" \
    dcent_zynq_geometry_require_payload_fit am2-s17p rootfs 22728704
expect_equal "S19j profile is evidence-qualified as production" production \
    "$(dcent_zynq_geometry_select am2-s19j && printf '%s' "$DCENT_ZYNQ_GEOMETRY_MATURITY")"
expect_equal "S17 profile remains explicitly experimental" experimental \
    "$(dcent_zynq_geometry_select am2-s17p && printf '%s' "$DCENT_ZYNQ_GEOMETRY_MATURITY")"
expect_equal "S19 Pro profile remains explicitly experimental" experimental \
    "$(dcent_zynq_geometry_select am2-s19pro && printf '%s' "$DCENT_ZYNQ_GEOMETRY_MATURITY")"

expect_failure "unknown board identity is refused" \
    dcent_zynq_geometry_payload_ceiling am2-unknown rootfs
expect_failure "unknown payload kind is refused" \
    dcent_zynq_geometry_payload_ceiling am2-s19j data
expect_failure "leading-zero payload size is refused" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 022728704
expect_failure "zero-byte payload is refused" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs 0
expect_failure "huge decimal payload fails closed without arithmetic admission" \
    dcent_zynq_geometry_require_payload_fit am2-s19j rootfs \
    999999999999999999999999999999999999999999999999999999999999

expect_equal "S9 tar ceiling includes fixed scratch slack" 29466624 \
    "$(dcent_zynq_geometry_tar_preextract_ceiling am1-s9)"
expect_equal "AM2 tar ceiling keeps layout tolerance separate" 34545664 \
    "$(dcent_zynq_geometry_tar_preextract_ceiling am2-s19j)"
expect_failure "unknown profile has no tar pre-extraction ceiling" \
    dcent_zynq_geometry_tar_preextract_ceiling unknown

receipt=$(dcent_zynq_geometry_receipt am2-s19j) || receipt=
expect_equal "geometry receipt is normalized and versioned" \
    'dcentos-zynq-ubi-geometry-v1|profile=am2-s19j|maturity=production|leb-size=126976|kernel-max=2920448|rootfs-max=22728704' \
    "$receipt"

printf '%s\n' "Zynq sysupgrade geometry tests: $tests assertions, $failures failures"
[ "$failures" -eq 0 ]
