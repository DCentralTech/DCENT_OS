#!/bin/sh
# Fixture-only tests for exact POSIX mountinfo ownership admission.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
HELPER=$PROJECT_ROOT/scripts/lib/sysupgrade_mount_identity.sh
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcent-sysupgrade-mount-identity-test.XXXXXX") || {
    printf '%s\n' 'FAIL: could not create isolated mount-identity fixture root' >&2
    exit 1
}
MOUNTINFO=$WORK_ROOT/mountinfo
TARGET=$WORK_ROOT/target
OTHER_TARGET=$WORK_ROOT/other-target
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
    _dcent_mount_test_label=$1
    shift
    if "$@"; then pass "$_dcent_mount_test_label"; else fail "$_dcent_mount_test_label"; fi
}
expect_failure()
{
    _dcent_mount_test_label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$_dcent_mount_test_label (unexpected success)"
    else
        pass "$_dcent_mount_test_label"
    fi
}

root_row()
{
    printf '%s\n' '36 25 0:32 / / rw,relatime shared:1 - ext4 /dev/root rw,errors=remount-ro'
}

exact_row()
{
    printf '42 36 250:2 / %s rw,relatime shared:7 master:1 - ubifs ubi1:rootfs_data rw\n' "$TARGET"
}

write_unmounted()
{
    root_row >"$MOUNTINFO"
}

write_exact()
{
    {
        root_row
        exact_row
    } >"$MOUNTINFO"
}

write_candidate()
{
    {
        root_row
        printf '%s\n' "$1"
    } >"$MOUNTINFO"
}

admit_exact()
{
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data \
        "$TARGET" rw 250:2 /
}

readmit_exact()
{
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 42 36 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / rw,relatime rw
}

mkdir -p "$TARGET" "$OTHER_TARGET"

expect_failure "require_absent rejects missing arguments" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO"
expect_failure "admit rejects missing arguments" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO"
expect_failure "readmit rejects missing arguments" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO"
expect_failure "require_released rejects missing arguments" \
    dcent_sysupgrade_mount_require_released "$MOUNTINFO"

write_unmounted
expect_success "an exact canonical target with no mount row is admitted as absent" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"
expect_success "absence publishes no stale mount observation" \
    test "$DCENT_SYSUPGRADE_MOUNT_OBSERVED" = 0

write_exact
expect_failure "pre-mount absence rejects an existing exact target mount" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"

write_unmounted
expect_failure "relative targets are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" relative/target
expect_failure "filesystem root is refused as a transaction mount target" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" /
expect_failure "non-canonical double-slash targets are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$WORK_ROOT//target"
expect_failure "escaped target syntax is refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "${TARGET}\\040suffix"
expect_failure "missing target directories are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$WORK_ROOT/missing"

printf '%s\n' not-a-directory >"$WORK_ROOT/not-directory"
expect_failure "non-directory targets are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$WORK_ROOT/not-directory"
ln -s "$TARGET" "$WORK_ROOT/target-link"
expect_failure "symlink targets are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$WORK_ROOT/target-link"
mkdir -p "$WORK_ROOT/real-parent/child"
ln -s "$WORK_ROOT/real-parent" "$WORK_ROOT/parent-link"
expect_failure "targets with a symlink ancestor are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$WORK_ROOT/parent-link/child"

ln -s "$MOUNTINFO" "$WORK_ROOT/mountinfo-link"
expect_failure "symlink mountinfo sources are refused" \
    dcent_sysupgrade_mount_require_absent "$WORK_ROOT/mountinfo-link" "$TARGET"
expect_failure "relative mountinfo sources are refused" \
    dcent_sysupgrade_mount_require_absent relative-mountinfo "$TARGET"
: >"$MOUNTINFO"
expect_failure "empty mountinfo is refused as ambiguous" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"
printf '%s' '36 25 0:32 / / rw - ext4 /dev/root rw' >"$MOUNTINFO"
expect_failure "an unterminated mountinfo record is refused as torn" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"
printf '36 25 0:32 / / rw - ext4 /dev/root rw\r\n' >"$MOUNTINFO"
expect_failure "CRLF mountinfo is refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"
printf '36 25 0:32 / / rw - ext4 /dev/root rw\000tail\n' >"$MOUNTINFO"
expect_failure "NUL-bearing mountinfo is refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"
{
    root_row
    printf '\n'
} >"$MOUNTINFO"
expect_failure "blank physical records are refused as multiline ambiguity" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"

awk 'BEGIN { for (i = 0; i < 1048576; i++) printf "A"; print "" }' >"$MOUNTINFO"
expect_failure "mountinfo snapshots above the byte ceiling are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"

_dcent_mount_test_row=0
: >"$MOUNTINFO"
while [ "$_dcent_mount_test_row" -lt 4097 ]; do
    root_row >>"$MOUNTINFO"
    _dcent_mount_test_row=$((_dcent_mount_test_row + 1))
done
expect_failure "mountinfo snapshots above the row ceiling are refused" \
    dcent_sysupgrade_mount_require_absent "$MOUNTINFO" "$TARGET"

write_exact
expect_success "one exact UBIFS mount is admitted" admit_exact
expect_success "admission publishes the canonical mount ID" \
    test "$DCENT_SYSUPGRADE_MOUNT_ID" = 42
expect_success "admission publishes the canonical parent mount ID" \
    test "$DCENT_SYSUPGRADE_MOUNT_PARENT_ID" = 36
expect_success "admission publishes exact major:minor" \
    test "$DCENT_SYSUPGRADE_MOUNT_MAJOR_MINOR" = 250:2
expect_success "admission publishes exact mount root" \
    test "$DCENT_SYSUPGRADE_MOUNT_ROOT" = /
expect_success "admission publishes exact ledger source" \
    test "$DCENT_SYSUPGRADE_MOUNT_SOURCE" = ubi1:rootfs_data
expect_success "admission publishes exact ledger target" \
    test "$DCENT_SYSUPGRADE_MOUNT_TARGET" = "$TARGET"
expect_success "admission publishes exact ledger mode" \
    test "$DCENT_SYSUPGRADE_MOUNT_MODE" = rw
expect_success "admission publishes exact filesystem type" \
    test "$DCENT_SYSUPGRADE_MOUNT_FS_TYPE" = ubifs
expect_success "admission preserves exact VFS options" \
    test "$DCENT_SYSUPGRADE_MOUNT_OPTIONS" = rw,relatime
expect_success "admission preserves exact superblock options" \
    test "$DCENT_SYSUPGRADE_MOUNT_SUPER_OPTIONS" = rw
expect_success "admission marks the observation complete only after parsing" \
    test "$DCENT_SYSUPGRADE_MOUNT_OBSERVED" = 1

expect_success "the same canonical mount ID is re-admitted before unmount" readmit_exact
expect_failure "pre-unmount re-admission refuses a different mount ID" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 43 36 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / rw,relatime rw
expect_success "failed re-admission clears stale observation fields" \
    test "$DCENT_SYSUPGRADE_MOUNT_OBSERVED:$DCENT_SYSUPGRADE_MOUNT_ID" = 0:
expect_failure "pre-unmount re-admission refuses a changed parent mount ID" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 42 37 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / rw,relatime rw
expect_failure "pre-unmount re-admission refuses changed VFS options" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 42 36 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / rw rw
expect_failure "pre-unmount re-admission refuses changed superblock options" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 42 36 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / rw,relatime rw,bulk_read
expect_failure "pre-unmount re-admission refuses unsafe expected options" \
    dcent_sysupgrade_mount_readmit "$MOUNTINFO" 42 36 ubi1:rootfs_data \
        "$TARGET" rw 250:2 / 'rw;command' rw

write_candidate "42 36 250:2 / $TARGET rw,relatime shared:7 - ubifs ubi1:rootfs_data rw"
expect_success "one canonical optional propagation field is accepted" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime future_tag:value - ubifs ubi1:rootfs_data rw"
expect_success "a structurally safe unknown optional field remains forward compatible" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime future_tag:value:extra - ubifs ubi1:rootfs_data rw"
expect_failure "multiple optional-field value separators are refused" admit_exact

write_candidate "42 36 250:2 / $TARGET rw,relatime shared:x - ubifs ubi1:rootfs_data rw"
expect_failure "a nonnumeric known optional-field value is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime shared:01 - ubifs ubi1:rootfs_data rw"
expect_failure "a non-canonical known optional-field number is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime shared:7 shared:8 - ubifs ubi1:rootfs_data rw"
expect_failure "duplicate optional-field tags are refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime idmapped:1 - ubifs ubi1:rootfs_data rw"
expect_failure "a valueless optional flag with a value is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,relatime bad\\040tag - ubifs ubi1:rootfs_data rw"
expect_failure "escaped optional fields are refused" admit_exact

write_candidate "042 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "leading-zero mount IDs are refused" admit_exact
write_candidate "0 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "zero mount IDs are refused" admit_exact
write_candidate "mount 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "nonnumeric mount IDs are refused" admit_exact
write_candidate "42 036 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "leading-zero parent mount IDs are refused" admit_exact
write_candidate "42 0 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "zero parent mount IDs are refused" admit_exact
write_candidate "42 parent 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "nonnumeric parent mount IDs are refused" admit_exact
write_candidate "42 36 0250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "leading-zero mount majors are refused" admit_exact
write_candidate "42 36 250:02 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "leading-zero mount minors are refused" admit_exact
write_candidate "42 36 250:2:1 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "multiple major:minor separators are refused" admit_exact

write_candidate "42 36 250:2 / $TARGET rw ubifs ubi1:rootfs_data rw"
expect_failure "a missing mountinfo separator is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - shared:7 - ubifs ubi1:rootfs_data rw"
expect_failure "multiple mountinfo separators are refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data"
expect_failure "a missing post-separator field is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data rw extra"
expect_failure "an extra post-separator field is refused" admit_exact

write_candidate "42 36 250:2 / $TARGET rw - ext4 ubi1:rootfs_data rw"
expect_failure "a non-UBIFS filesystem type is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - ubifs ubi2:rootfs_data rw"
expect_failure "a different UBI source is refused" admit_exact
write_candidate "42 36 250:3 / $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "a different canonical major:minor is refused" admit_exact
write_candidate "42 36 250:2 /subdir $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "a different canonical mount root is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET ro - ubifs ubi1:rootfs_data ro"
expect_failure "a read-only mount is refused when read-write was expected" admit_exact
expect_success "the same exact UBIFS row is admitted when read-only was expected" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" ro 250:2 /
expect_success "read-only admission publishes the exact ledger mode" \
    test "$DCENT_SYSUPGRADE_MOUNT_MODE" = ro
write_candidate "42 36 250:2 / $TARGET rw,ro - ubifs ubi1:rootfs_data rw"
expect_failure "ambiguous VFS ro/rw options are refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,rw - ubifs ubi1:rootfs_data rw"
expect_failure "duplicate VFS mode options are refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw,,relatime - ubifs ubi1:rootfs_data rw"
expect_failure "empty VFS option tokens are refused" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - ubifs ubi1:rootfs_data ro"
expect_failure "a superblock mode mismatch is refused" admit_exact
write_candidate "42 36 250:2 / $TARGET relatime - ubifs ubi1:rootfs_data rw"
expect_failure "a missing VFS mode is refused" admit_exact
write_candidate "42 36 250:2 /\\040 $TARGET rw - ubifs ubi1:rootfs_data rw"
expect_failure "an escaped candidate root is refused" admit_exact
write_candidate "42 36 250:2 / ${TARGET}\\040suffix rw - ubifs ubi1:rootfs_data rw"
expect_failure "an escaped candidate target cannot be admitted" admit_exact
write_candidate "42 36 250:2 / $TARGET rw - ubifs ubi1:rootfs\\040data rw"
expect_failure "an escaped candidate source cannot be admitted" admit_exact

write_exact
expect_failure "non-canonical source device numbers are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi01:rootfs_data "$TARGET" rw 250:2 /
expect_failure "empty UBI volume names are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1: "$TARGET" rw 250:2 /
expect_failure "source volume path syntax is refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:../rootfs "$TARGET" rw 250:2 /
expect_failure "non-canonical expected major numbers are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" rw 0250:2 /
expect_failure "out-of-range expected major numbers are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" rw 4096:2 /
expect_failure "out-of-range expected minor numbers are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" rw 250:1048576 /
expect_failure "multiple expected major:minor separators are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" rw 250:2:1 /
expect_failure "unsafe expected roots are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" rw 250:2 /../root
expect_failure "unknown expected access modes are refused" \
    dcent_sysupgrade_mount_admit "$MOUNTINFO" ubi1:rootfs_data "$TARGET" auto 250:2 /

{
    root_row
    exact_row
    printf '43 36 250:3 / %s rw - ubifs ubi1:other rw\n' "$TARGET"
} >"$MOUNTINFO"
expect_failure "duplicate or stacked rows at the exact target are refused" admit_exact

{
    root_row
    exact_row
    printf '42 36 250:3 / %s rw - ubifs ubi1:other rw\n' "$OTHER_TARGET"
} >"$MOUNTINFO"
expect_failure "duplicate mount IDs anywhere in the snapshot are refused" admit_exact

{
    printf '%s\n' '036 25 0:32 / / rw - ext4 /dev/root rw'
    exact_row
} >"$MOUNTINFO"
expect_failure "a malformed unrelated row makes the whole snapshot ambiguous" admit_exact

write_exact
expect_failure "release proof refuses while the exact mount remains" \
    dcent_sysupgrade_mount_require_released "$MOUNTINFO" 42 "$TARGET"
{
    root_row
    printf '43 36 250:3 / %s rw - ubifs ubi1:other rw\n' "$TARGET"
} >"$MOUNTINFO"
expect_failure "release proof refuses a replacement mount at the same target" \
    dcent_sysupgrade_mount_require_released "$MOUNTINFO" 42 "$TARGET"
{
    root_row
    printf '42 36 250:3 / %s rw - ubifs ubi1:other rw\n' "$OTHER_TARGET"
} >"$MOUNTINFO"
expect_failure "release proof refuses the same mount ID observed elsewhere" \
    dcent_sysupgrade_mount_require_released "$MOUNTINFO" 42 "$TARGET"

write_unmounted
expect_success "post-unmount proof requires both mount ID and target to be absent" \
    dcent_sysupgrade_mount_require_released "$MOUNTINFO" 42 "$TARGET"
expect_success "post-unmount proof leaves no admitted observation" \
    test "$DCENT_SYSUPGRADE_MOUNT_OBSERVED:$DCENT_SYSUPGRADE_MOUNT_ID" = 0:

printf '%s\n' "mount identity tests: $tests assertions, $failures failures"
[ "$failures" -eq 0 ]
