#!/bin/sh
# Mount- and device-node-free tests for AM2 UBI provisioning-prefix admission.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
HELPER=$PROJECT_ROOT/scripts/lib/sysupgrade_ubi_volume_plan.sh
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcent-sysupgrade-ubi-volume-plan-test.XXXXXX") || {
    printf '%s\n' 'FAIL: could not create isolated UBI volume-plan fixture root' >&2
    exit 1
}
SYSFS_ROOT=$WORK_ROOT/sys/class/ubi
failures=0
tests=0

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# shellcheck source=/dev/null
. "$HELPER"

pass() { tests=$((tests + 1)); printf 'PASS: %s\n' "$1"; }
fail() { tests=$((tests + 1)); failures=$((failures + 1)); printf 'FAIL: %s\n' "$1" >&2; }
expect_success()
{
    _dcent_test_label=$1
    shift
    if "$@" >/dev/null; then
        pass "$_dcent_test_label"
    else
        fail "$_dcent_test_label"
    fi
}
expect_failure()
{
    _dcent_test_label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$_dcent_test_label (unexpected success)"
    else
        pass "$_dcent_test_label"
    fi
}

write_attr()
{
    printf '%s\n' "$2" >"$1"
}

add_volume()
{
    _dcent_test_id=$1
    _dcent_test_name=$2
    mkdir "$SYSFS_ROOT/ubi1_$_dcent_test_id"
    write_attr "$SYSFS_ROOT/ubi1_$_dcent_test_id/name" "$_dcent_test_name"
    write_attr "$SYSFS_ROOT/ubi1_$_dcent_test_id/type" dynamic
}

make_fixture()
{
    _dcent_test_prefix=$1
    rm -rf "$WORK_ROOT"
    mkdir -p "$SYSFS_ROOT/ubi1"
    write_attr "$SYSFS_ROOT/ubi1/mtd_num" 8
    write_attr "$SYSFS_ROOT/ubi1/volumes_count" "$_dcent_test_prefix"
    case "$_dcent_test_prefix" in
        1) add_volume 0 kernel ;;
        2) add_volume 0 kernel; add_volume 1 rootfs ;;
        3)
            add_volume 0 kernel
            add_volume 1 rootfs
            add_volume 2 rootfs_data
            ;;
    esac
}

admit_prefix()
{
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 8 "$1"
}

make_fixture 0
expect_success "factory-blank prefix 0 is admitted without device nodes" \
    admit_prefix 0

make_fixture 1
expect_success "exact kernel-only prefix 1 is admitted" admit_prefix 1

make_fixture 2
expect_success "exact kernel/rootfs prefix 2 is admitted" admit_prefix 2

make_fixture 3
expect_success "complete declarative prefix 3 is admitted" admit_prefix 3

make_fixture 3
receipt=$(admit_prefix 3) || receipt=
expected_receipt='dcentos-ubi-volume-plan-v1|ubi=1|mtd=8|prefix=3|id0=0,kernel,dynamic|id1=1,rootfs,dynamic|id2=2,rootfs_data,dynamic'
if [ "$receipt" = "$expected_receipt" ]; then
    pass "admission emits the normalized semantic receipt"
else
    fail "admission emits the normalized semantic receipt"
fi
expect_success "an unchanged receipt revalidates" \
    dcent_ubi_volume_plan_revalidate "$SYSFS_ROOT" 1 8 3 "$receipt"

write_attr "$SYSFS_ROOT/ubi1_1/name" root
expect_failure "semantic drift from an admitted receipt is refused" \
    dcent_ubi_volume_plan_revalidate "$SYSFS_ROOT" 1 8 3 "$receipt"

make_fixture 2
rm -rf "$SYSFS_ROOT/ubi1_1"
add_volume 2 rootfs_data
expect_failure "a hole followed by a later declarative ID is refused" \
    admit_prefix 2

make_fixture 2
write_attr "$SYSFS_ROOT/ubi1_0/name" rootfs
write_attr "$SYSFS_ROOT/ubi1_1/name" kernel
expect_failure "reordered declarative names are refused" admit_prefix 2

make_fixture 1
write_attr "$SYSFS_ROOT/ubi1_0/type" static
expect_failure "a recognized but non-plan static type is refused" admit_prefix 1

make_fixture 1
write_attr "$SYSFS_ROOT/ubi1_0/type" compressed
expect_failure "an unknown volume type is refused" admit_prefix 1

make_fixture 1
add_volume 1 rootfs
expect_failure "an extra volume is refused despite a stale count" admit_prefix 1

make_fixture 0
mkdir "$SYSFS_ROOT/ubi1_00"
expect_failure "a leading-zero class ID is refused" admit_prefix 0

make_fixture 0
mkdir "$SYSFS_ROOT/ubi1_bad"
expect_failure "a malformed class ID is refused" admit_prefix 0

make_fixture 0
ln -s "$WORK_ROOT/missing-volume" "$SYSFS_ROOT/ubi1_0"
expect_failure "a dangling extra volume symlink is refused" admit_prefix 0

make_fixture 1
mv "$SYSFS_ROOT/ubi1_0" "$WORK_ROOT/real-volume"
ln -s "$WORK_ROOT/real-volume" "$SYSFS_ROOT/ubi1_0"
expect_failure "a symlinked volume directory is refused" admit_prefix 1

make_fixture 1
mv "$SYSFS_ROOT/ubi1_0/name" "$WORK_ROOT/volume-name"
ln -s "$WORK_ROOT/volume-name" "$SYSFS_ROOT/ubi1_0/name"
expect_failure "a symlinked name attribute is refused" admit_prefix 1

make_fixture 1
mv "$SYSFS_ROOT/ubi1_0/type" "$WORK_ROOT/volume-type"
ln -s "$WORK_ROOT/volume-type" "$SYSFS_ROOT/ubi1_0/type"
expect_failure "a symlinked type attribute is refused" admit_prefix 1

make_fixture 1
mv "$SYSFS_ROOT/ubi1" "$WORK_ROOT/real-device"
ln -s "$WORK_ROOT/real-device" "$SYSFS_ROOT/ubi1"
expect_failure "a substituted UBI device class symlink is refused" admit_prefix 1

make_fixture 1
BACKING_DEVICE=$WORK_ROOT/sys/devices/virtual/mtd/mtd8/ubi1
mkdir -p "${BACKING_DEVICE%/*}"
mv "$SYSFS_ROOT/ubi1" "$BACKING_DEVICE"
mv "$SYSFS_ROOT/ubi1_0" "$BACKING_DEVICE/ubi1_0"
ln -s '../../devices/virtual/mtd/mtd8/ubi1' "$SYSFS_ROOT/ubi1"
ln -s '../../devices/virtual/mtd/mtd8/ubi1/ubi1_0' "$SYSFS_ROOT/ubi1_0"
expect_success "canonical kernel class symlinks under expected mtd8 are admitted" \
    admit_prefix 1

make_fixture 1
BACKING_DEVICE=$WORK_ROOT/sys/devices/virtual/ubi/ubi1
mkdir -p "${BACKING_DEVICE%/*}"
mv "$SYSFS_ROOT/ubi1" "$BACKING_DEVICE"
mv "$SYSFS_ROOT/ubi1_0" "$BACKING_DEVICE/ubi1_0"
ln -s '../../devices/virtual/ubi/ubi1' "$SYSFS_ROOT/ubi1"
ln -s '../../devices/virtual/ubi/ubi1/ubi1_0' "$SYSFS_ROOT/ubi1_0"
expect_success "Linux 4.4 legacy UBI class symlink topology is admitted" \
    admit_prefix 1

make_fixture 0
BACKING_DEVICE=$WORK_ROOT/sys/devices/virtual/mtd/mtd7/ubi1
mkdir -p "${BACKING_DEVICE%/*}"
mv "$SYSFS_ROOT/ubi1" "$BACKING_DEVICE"
ln -s '../../devices/virtual/mtd/mtd7/ubi1' "$SYSFS_ROOT/ubi1"
expect_failure "a class symlink parented by the wrong MTD is refused" \
    admit_prefix 0

make_fixture 1
BACKING_DEVICE=$WORK_ROOT/sys/devices/virtual/mtd/mtd8/ubi1
WRONG_PARENT=$WORK_ROOT/sys/devices/virtual/mtd/mtd8/other
mkdir -p "$BACKING_DEVICE" "$WRONG_PARENT"
mv "$SYSFS_ROOT/ubi1/mtd_num" "$BACKING_DEVICE/mtd_num"
mv "$SYSFS_ROOT/ubi1/volumes_count" "$BACKING_DEVICE/volumes_count"
mv "$SYSFS_ROOT/ubi1_0" "$WRONG_PARENT/ubi1_0"
rm -rf "$SYSFS_ROOT/ubi1"
ln -s '../../devices/virtual/mtd/mtd8/ubi1' "$SYSFS_ROOT/ubi1"
ln -s '../../devices/virtual/mtd/mtd8/other/ubi1_0' "$SYSFS_ROOT/ubi1_0"
expect_failure "a volume class symlink outside its UBI-device parent is refused" \
    admit_prefix 1

make_fixture 0
mv "$SYSFS_ROOT" "$WORK_ROOT/real-ubi"
ln -s "$WORK_ROOT/real-ubi" "$SYSFS_ROOT"
expect_failure "a symlinked sysfs root is refused" admit_prefix 0

make_fixture 0
mv "$WORK_ROOT/sys" "$WORK_ROOT/real-sys"
ln -s "$WORK_ROOT/real-sys" "$WORK_ROOT/sys"
expect_failure "a sysfs root with a symlinked ancestor is refused" admit_prefix 0

make_fixture 0
expect_failure "a sysfs root containing dot-dot is refused as non-canonical" \
    dcent_ubi_volume_plan_admit "$WORK_ROOT/sys/class/../class/ubi" 1 8 0

make_fixture 0
expect_failure "a relative sysfs root is refused" \
    dcent_ubi_volume_plan_admit relative/ubi 1 8 0

make_fixture 0
expect_failure "a leading-zero UBI device argument is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 01 8 0

make_fixture 0
expect_failure "a nonnumeric UBI device argument is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" '1/../../tmp' 8 0

make_fixture 0
expect_failure "a leading-zero MTD argument is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 08 0

make_fixture 0
expect_failure "a nonnumeric MTD argument is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 8x 0

make_fixture 0
expect_failure "a leading-zero prefix argument is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 8 00

make_fixture 0
expect_failure "a prefix beyond the three-entry plan is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 8 4

make_fixture 0
write_attr "$SYSFS_ROOT/ubi1/mtd_num" 08
expect_failure "a leading-zero sysfs MTD identity is refused" admit_prefix 0

make_fixture 0
write_attr "$SYSFS_ROOT/ubi1/mtd_num" 7
expect_failure "a wrong attached MTD identity is refused" admit_prefix 0

make_fixture 0
write_attr "$SYSFS_ROOT/ubi1/volumes_count" 00
expect_failure "a leading-zero sysfs volume count is refused" admit_prefix 0

make_fixture 0
write_attr "$SYSFS_ROOT/ubi1/volumes_count" count
expect_failure "a nonnumeric sysfs volume count is refused" admit_prefix 0

make_fixture 0
awk 'BEGIN { for (i = 0; i < 4096; i++) printf "8"; print "" }' \
    >"$SYSFS_ROOT/ubi1/mtd_num"
expect_failure "an oversized single-line sysfs attribute is refused" admit_prefix 0

make_fixture 1
printf 'dynamic\nstatic\n' >"$SYSFS_ROOT/ubi1_0/type"
expect_failure "a multi-line sysfs identity attribute is refused" admit_prefix 1

make_fixture 1
write_attr "$SYSFS_ROOT/ubi1/volumes_count" 0
expect_failure "reported count drift from exposed entries is refused" admit_prefix 1

make_fixture 1
expect_failure "an unsupported receipt schema is refused" \
    dcent_ubi_volume_plan_revalidate "$SYSFS_ROOT" 1 8 1 \
    'dcentos-ubi-volume-plan-v0|ubi=1|mtd=8|prefix=1'

if grep -Eq '(^|[[:space:]])(ubimkvol|ubirmvol|ubiupdatevol|ubiattach|ubidetach|mount|umount|mknod)([[:space:]]|$)' \
    "$HELPER"; then
    fail "helper contains no flash, attachment, mount, or node mutation command"
else
    pass "helper contains no flash, attachment, mount, or node mutation command"
fi

# Force the two internal snapshots to disagree without relying on scheduler
# timing.  Re-sourcing the helper below restores the real snapshot function.
DRIFT_COUNTER=$WORK_ROOT/drift-counter
printf '0\n' >"$DRIFT_COUNTER"
dcent_ubi_volume_plan_snapshot()
{
    _dcent_test_drift=$(cat "$DRIFT_COUNTER")
    if [ "$_dcent_test_drift" = 0 ]; then
        printf '1\n' >"$DRIFT_COUNTER"
        printf '%s\n' \
            'dcentos-ubi-volume-plan-v1|ubi=1|mtd=8|prefix=0'
    else
        printf '%s\n' \
            'dcentos-ubi-volume-plan-v1|ubi=1|mtd=8|prefix=1|id0=0,kernel,dynamic'
    fi
}
expect_failure "drift between the two admission snapshots is refused" \
    dcent_ubi_volume_plan_admit "$SYSFS_ROOT" 1 8 0
# shellcheck source=/dev/null
. "$HELPER"

printf '%s\n' \
    "UBI volume-plan tests: $tests assertions, $failures failures"
[ "$failures" -eq 0 ]
