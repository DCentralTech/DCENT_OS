#!/bin/sh
# Offline state-machine tests for the one-time entropy-seed lifecycle.
#
# The test build replaces RNDADDENTROPY with an append-only credit log and
# records /dev/urandom open direction.  No kernel entropy is credited and no
# real random device is required.  Crash failpoints use _exit(90), allowing the
# on-disk state to be inspected exactly where power loss could occur.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE=$PROJECT_ROOT/br2_external_dcentos/packages/seed-entropy/src/seed-entropy.c
PERSIST_SERVICE=$PROJECT_ROOT/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S45persistent
EARLY_INIT=$PROJECT_ROOT/br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-early-init.sh
: "${HOME:?HOME must name a private directory for path-safety tests}"
WORK_ROOT=${SEED_ENTROPY_TEST_WORK_ROOT:-$HOME/.seed-entropy-lifecycle-test.$$}
BIN=$WORK_ROOT/seed-entropy-test
PRODUCTION_BIN=$WORK_ROOT/seed-entropy-production
ROOT_SEED=/seed-entropy-lifecycle-test.$$
ROOT_CONSUMED=/.seed-entropy-lifecycle-test.$$.consumed
ROOT_CREDITED=/.seed-entropy-lifecycle-test.$$.credited
ROOT_BIRTH=/.seed-entropy-lifecycle-test.$$.born
ROOT_BIRTH_TEMP=/.seed-entropy-lifecycle-test.$$.born.new
ROOT_WITNESS=/.seed-entropy-lifecycle-test.$$.new
ASSERTIONS=0
LOCKER_PID=

cleanup()
{
    if [ -n "$LOCKER_PID" ]; then
        kill "$LOCKER_PID" 2>/dev/null || true
        wait "$LOCKER_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_ROOT"
    rm -f "$ROOT_SEED" "$ROOT_CONSUMED" "$ROOT_CREDITED" "$ROOT_BIRTH" \
        "$ROOT_BIRTH_TEMP" "$ROOT_WITNESS"
}
trap cleanup EXIT HUP INT TERM

pass()
{
    ASSERTIONS=$((ASSERTIONS + 1))
}

die()
{
    echo "FAIL: $*" >&2
    exit 1
}

assert_exists()
{
    [ -e "$1" ] || die "expected path to exist: $1"
    pass
}

assert_absent()
{
    [ ! -e "$1" ] && [ ! -L "$1" ] || die "expected path to be absent: $1"
    pass
}

assert_size()
{
    actual=$(stat -c '%s' "$1")
    [ "$actual" = "$2" ] || die "$1 size is $actual, expected $2"
    pass
}

assert_mode()
{
    actual=$(stat -c '%a' "$1")
    [ "$actual" = "$2" ] || die "$1 mode is $actual, expected $2"
    pass
}

assert_same()
{
    cmp -s "$1" "$2" || die "files differ: $1 $2"
    pass
}

assert_text()
{
    actual=$(sed -n '1,$p' "$1")
    expected=$2
    case $1 in
        *.born)
            if [ "${#expected}" -eq 36 ] && [ -f "$SEED" ]; then
                digest=$(sha256sum "$SEED" | awk '{print $1}')
                expected="v1 $expected $digest"
            fi
            ;;
    esac
    [ "$actual" = "$expected" ] || \
        die "$1 contains '$actual', expected '$expected'"
    pass
}

make_bytes()
{
    character=$1
    destination=$2
    awk -v character="$character" 'BEGIN {
        for (i = 0; i < 256; i++)
            printf "%s.", character
    }' > "$destination"
    chmod 600 "$destination"
}

make_birth_marker()
{
    boot_id=$1
    seed_path=$2
    marker_path=$3
    digest=$(sha256sum "$seed_path" | awk '{print $1}')

    printf 'v1 %s %s' "$boot_id" "$digest" >"$marker_path"
    chmod 600 "$marker_path"
}

ensure_creditable_seed()
{
    if [ -f "$SEED" ] && [ ! -e "$BIRTH" ] && [ ! -L "$BIRTH" ] &&
       [ ! -e "$BIRTH_TEMP" ] && [ ! -L "$BIRTH_TEMP" ] &&
       [ ! -e "$CONSUMED" ] && [ ! -L "$CONSUMED" ] &&
       [ ! -e "$CREDITED" ] && [ ! -L "$CREDITED" ] &&
       [ ! -e "$WITNESS" ] && [ ! -L "$WITNESS" ]; then
        make_birth_marker \
            00000000-0000-4000-8000-000000000000 "$SEED" "$BIRTH"
    fi
}

new_case()
{
    name=$1
    CASE_ROOT=$WORK_ROOT/cases/$name
    KEYS=$CASE_ROOT/keys
    SEED=$KEYS/random-seed
    CONSUMED=$KEYS/.random-seed.consumed
    CREDITED=$KEYS/.random-seed.credited
    BIRTH=$KEYS/.random-seed.born
    BIRTH_TEMP=$KEYS/.random-seed.born.new
    WITNESS=$KEYS/.random-seed.new
    RANDOM_SOURCE=$CASE_ROOT/fake-urandom
    CREDIT_LOG=$CASE_ROOT/credited-seeds
    IOCTL_META_LOG=$CASE_ROOT/ioctl-metadata
    MIX_LOG=$CASE_ROOT/mixed-seeds
    OPEN_LOG=$CASE_ROOT/random-opens

    rm -rf "$CASE_ROOT"
    mkdir -p "$KEYS"
    chmod 700 "$CASE_ROOT" "$KEYS"
    make_bytes A "$SEED"
    make_bytes B "$RANDOM_SOURCE"
    export SEED_ENTROPY_TEST_RANDOM_PATH=$RANDOM_SOURCE
    export SEED_ENTROPY_TEST_IOCTL_LOG=$CREDIT_LOG
    export SEED_ENTROPY_TEST_IOCTL_META_LOG=$IOCTL_META_LOG
    export SEED_ENTROPY_TEST_MIX_LOG=$MIX_LOG
    export SEED_ENTROPY_TEST_RANDOM_OPEN_LOG=$OPEN_LOG
    unset SEED_ENTROPY_TEST_FAILPOINT SEED_ENTROPY_TEST_IOCTL_FAIL \
        SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC \
        SEED_ENTROPY_TEST_FAIL_FILE_SYNC \
        SEED_ENTROPY_TEST_READINESS \
        SEED_ENTROPY_TEST_BOOT_ID \
        SEED_ENTROPY_TEST_RANDOM_DEVICE
}

run_success()
{
    ensure_creditable_seed
    "$BIN" "$SEED" || die "seed-entropy unexpectedly failed for $SEED"
}

run_untrusted_success()
{
    "$BIN" "$SEED" || \
        die "untrusted seed migration unexpectedly failed for $SEED"
}

run_untrusted_failure()
{
    set +e
    "$BIN" "$SEED" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" -ne 0 ] || \
        die "untrusted seed migration unexpectedly succeeded for $SEED"
    pass
}

run_failure()
{
    ensure_creditable_seed
    set +e
    "$BIN" "$SEED" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" -ne 0 ] || die "seed-entropy unexpectedly succeeded for $SEED"
    pass
}

run_status()
{
    expected=$1
    ensure_creditable_seed
    set +e
    "$BIN" "$SEED" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" = "$expected" ] || \
        die "seed-entropy returned $status, expected $expected for $SEED"
    pass
}

run_initialize_success()
{
    "$BIN" --initialize-if-missing "$SEED" || \
        die "seed initializer unexpectedly failed for $SEED"
}

run_initialize_failure()
{
    set +e
    "$BIN" --initialize-if-missing "$SEED" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" -ne 0 ] || die "seed initializer unexpectedly succeeded for $SEED"
    pass
}

run_initialize_status()
{
    expected=$1
    set +e
    "$BIN" --initialize-if-missing "$SEED" >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" = "$expected" ] || \
        die "seed initializer returned $status, expected $expected for $SEED"
    pass
}

run_initialize_at_success()
{
    (exec 9<"$KEYS"; "$BIN" --initialize-if-missing-at 9 random-seed) || \
        die "descriptor-based seed initializer unexpectedly failed for $KEYS"
}

run_initialize_at_failure()
{
    set +e
    (exec 9<"$KEYS"; "$BIN" --initialize-if-missing-at 9 random-seed) \
        >/dev/null 2>&1
    status=$?
    set -e
    [ "$status" -ne 0 ] || \
        die "descriptor-based seed initializer unexpectedly succeeded for $KEYS"
    pass
}

mkdir -p "$WORK_ROOT/cases"
chmod 700 "$WORK_ROOT" "$WORK_ROOT/cases"

CC=${CC:-cc}
COMMON_CFLAGS=${SEED_ENTROPY_TEST_CFLAGS:-"-std=c99 -O2 -Wall -Wextra -Werror"}
# shellcheck disable=SC2086
$CC $COMMON_CFLAGS -o "$PRODUCTION_BIN" "$SOURCE"
# shellcheck disable=SC2086
$CC $COMMON_CFLAGS -DSEED_ENTROPY_TESTING -o "$BIN" "$SOURCE"

# Compile-time hooks must not leak into the production executable.
if strings "$PRODUCTION_BIN" | grep -q 'SEED_ENTROPY_TEST_'; then
    die "test hook environment names leaked into the production binary"
fi
pass

# The shutdown service must use the package binary's non-replacing initializer,
# never a raw /dev/urandom redirection or dd write.
sh -n "$PERSIST_SERVICE"
grep -F -- '--initialize-if-missing' "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent does not invoke secure seed initialization"
grep -F -- 'ubi0:rootfs_data' "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent does not prove the persistent seed filesystem"
grep -F -- 'mounts == 1 && backends == 1 && writable' \
    "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent accepts ambiguous or stacked /data mounts"
grep -F -- 'CRNG readiness' "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent does not document the helper's CRNG readiness proof"
grep -F -- '"$0" stop || exit $?' "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent restart masks a failed stop"
grep -F -- 'exec "$0" start' "$PERSIST_SERVICE" >/dev/null || \
    die "S45persistent restart does not propagate the start result"
grep -F -- 'GRND_NONBLOCK' "$SOURCE" >/dev/null || \
    die "seed initializer does not use a nonblocking CRNG readiness probe"
if grep -E 'ioctl\([^,]+,[[:space:]]*RNDGETENTCNT' "$SOURCE" >/dev/null; then
    die "seed initializer treats input-pool accounting as a CRNG readiness API"
fi
if grep -E 'dd[[:space:]].*random-seed|/dev/urandom' "$PERSIST_SERVICE" >/dev/null; then
    die "S45persistent retains a raw entropy-seed write"
fi
pass

sh -n "$EARLY_INIT"
grep -F -- '.random-seed.born' "$EARLY_INIT" >/dev/null || \
    die "early init does not recognize the seed birth marker"
grep -F -- '.random-seed.born.new' "$EARLY_INIT" >/dev/null || \
    die "early init does not recognize interrupted birth-marker publication"
pass

# Initialization is idempotent: an existing valid seed is content-, inode-,
# and mode-stable, and neither urandom nor RNDADDENTROPY is touched.
new_case initialize_existing
cp "$SEED" "$CASE_ROOT/expected-a"
touch -a -t 200001010000 "$SEED"
existing_inode=$(stat -c '%d:%i' "$SEED")
existing_atime=$(stat -c '%X' "$SEED")
export SEED_ENTROPY_TEST_READINESS=not-ready
run_initialize_success
unset SEED_ENTROPY_TEST_READINESS
[ "$(stat -c '%d:%i' "$SEED")" = "$existing_inode" ] || \
    die "initializer replaced an existing valid seed"
pass
[ "$(stat -c '%X' "$SEED")" = "$existing_atime" ] || \
    die "initializer changed the existing seed access time"
pass
assert_same "$SEED" "$CASE_ROOT/expected-a"
assert_mode "$SEED" 600
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"
assert_absent "$CONSUMED"
assert_absent "$BIRTH"
assert_absent "$BIRTH_TEMP"
assert_absent "$WITNESS"

# The epoch marker is authoritative lifecycle state.  Orphaned, malformed,
# permissive, or symlink-substituted markers fail before entropy credit.
new_case birth_without_public_seed
rm -f "$SEED"
printf '%s' '11111111-1111-4111-8111-111111111111' >"$BIRTH"
chmod 600 "$BIRTH"
run_failure
assert_exists "$BIRTH"
assert_absent "$CREDIT_LOG"

new_case malformed_birth_marker
printf '%s' 'AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA' >"$BIRTH"
chmod 600 "$BIRTH"
run_failure
assert_exists "$SEED"
assert_absent "$CREDIT_LOG"

new_case permissive_birth_marker
printf '%s' '11111111-1111-4111-8111-111111111111' >"$BIRTH"
chmod 644 "$BIRTH"
run_failure
assert_exists "$SEED"
assert_absent "$CREDIT_LOG"

new_case symlink_birth_marker
printf '%s' '11111111-1111-4111-8111-111111111111' \
    >"$CASE_ROOT/outside-birth"
chmod 600 "$CASE_ROOT/outside-birth"
ln -s "$CASE_ROOT/outside-birth" "$BIRTH"
run_failure
assert_exists "$SEED"
assert_absent "$CREDIT_LOG"

new_case orphan_birth_marker_transaction
printf '%s' '11111111-1111-4111-8111-111111111111' >"$BIRTH_TEMP"
chmod 600 "$BIRTH_TEMP"
run_failure
assert_exists "$SEED"
assert_absent "$CREDIT_LOG"

new_case conflicting_birth_marker_transaction
printf '%s' '11111111-1111-4111-8111-111111111111' >"$BIRTH"
printf '%s' '11111111-1111-4111-8111-111111111111' >"$BIRTH_TEMP"
chmod 600 "$BIRTH" "$BIRTH_TEMP"
run_failure
assert_exists "$SEED"
assert_absent "$CREDIT_LOG"

# The marker binds the boot epoch to the exact 512-byte seed.  Replacing the
# seed, forging the digest, or presenting a current-boot marker around changed
# bytes must fail before either no-credit mixing or entropy accounting.
new_case seed_changed_after_birth_marker
make_birth_marker 00000000-0000-4000-8000-000000000000 "$SEED" "$BIRTH"
make_bytes Z "$SEED"
run_failure
assert_exists "$SEED"
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

new_case forged_birth_digest
printf 'v1 %s %064d' 00000000-0000-4000-8000-000000000000 0 >"$BIRTH"
chmod 600 "$BIRTH"
run_failure
assert_exists "$SEED"
assert_size "$BIRTH" 104
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

new_case unknown_birth_marker_version
make_birth_marker 00000000-0000-4000-8000-000000000000 "$SEED" "$BIRTH"
{
    printf 'v2'
    dd if="$BIRTH" bs=1 skip=2 2>/dev/null
} >"$BIRTH.unknown"
mv "$BIRTH.unknown" "$BIRTH"
chmod 600 "$BIRTH"
run_failure
assert_exists "$SEED"
assert_size "$BIRTH" 104
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

new_case changed_seed_in_birth_boot
make_birth_marker 11111111-1111-4111-8111-111111111111 "$SEED" "$BIRTH"
make_bytes Z "$SEED"
run_failure
assert_exists "$SEED"
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

# Markerless seeds from older releases have no CRNG-readiness provenance.
# They are mixed as uncredited input, durably removed, and replaced only from
# an authoritatively ready current CRNG.
new_case markerless_legacy_migration
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
run_untrusted_success
assert_absent "$CREDIT_LOG"
assert_absent "$IOCTL_META_LOG"
assert_size "$MIX_LOG" 512
assert_same "$MIX_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$OPEN_LOG" "W
R"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_size "$BIRTH" 104

new_case markerless_migration_crng_not_ready
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_READINESS=not-ready
run_untrusted_failure
assert_absent "$SEED"
assert_absent "$BIRTH"
assert_absent "$CREDIT_LOG"
assert_absent "$IOCTL_META_LOG"
assert_size "$MIX_LOG" 512
assert_same "$MIX_LOG" "$CASE_ROOT/expected-a"
assert_text "$OPEN_LOG" "W
R"
unset SEED_ENTROPY_TEST_READINESS
run_success
assert_exists "$SEED"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_size "$MIX_LOG" 512
assert_absent "$CREDIT_LOG"

# Sysupgrade callers may already hold a trusted mount directory descriptor
# below a shared path such as /tmp.  Descriptor admission preserves the same
# file-state contract without trusting or re-walking that shared ancestor.
new_case initialize_descriptor_missing
rm -f "$SEED"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
run_initialize_at_success
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_mode "$SEED" 600
assert_absent "$CONSUMED"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
assert_absent "$WITNESS"

# flock() is open-file-description scoped.  Reusing dup(supplied_fd) would
# inherit the caller's lock ownership and silently bypass exclusion; reopening
# "." must obtain a distinct description and therefore fail here.
new_case initialize_descriptor_caller_holds_lock
rm -f "$SEED"
set +e
(exec 9<"$KEYS"; flock -n 9; \
    "$BIN" --initialize-if-missing-at 9 random-seed) >/dev/null 2>&1
status=$?
set -e
[ "$status" -ne 0 ] || \
    die "descriptor initializer bypassed a caller-held directory lock"
pass
assert_absent "$SEED"
assert_absent "$WITNESS"
assert_absent "$OPEN_LOG"

new_case initialize_descriptor_unsafe_directory
rm -f "$SEED"
chmod 770 "$KEYS"
run_initialize_at_failure
assert_absent "$SEED"
assert_absent "$WITNESS"
assert_absent "$OPEN_LOG"
chmod 700 "$KEYS"

new_case initialize_descriptor_unsafe_basename
rm -f "$SEED"
set +e
(exec 9<"$KEYS"; "$BIN" --initialize-if-missing-at 9 ../random-seed) \
    >/dev/null 2>&1
status=$?
set -e
[ "$status" -ne 0 ] || die "descriptor initializer accepted an unsafe basename"
pass
assert_absent "$SEED"
assert_absent "$OPEN_LOG"

# Complete absence is not sufficient authority to persist a seed: the helper
# must independently prove that the kernel CRNG is initialized.  Both a
# negative readiness result and an indeterminate probe fail before any seed
# state is created or credited.
new_case initialize_random_not_ready
rm -f "$SEED"
export SEED_ENTROPY_TEST_READINESS=not-ready
run_initialize_failure
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"
assert_text "$OPEN_LOG" "R"

new_case initialize_readiness_probe_error
rm -f "$SEED"
export SEED_ENTROPY_TEST_READINESS=probe-error
run_initialize_failure
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"
assert_text "$OPEN_LOG" "R"

# Input-pool entropy accounting is not an authoritative substitute for the
# independent nonblocking-CRNG state.  A kernel without getrandom support must
# fail closed regardless of how much input entropy it may report elsewhere.
new_case initialize_no_authoritative_readiness_api
rm -f "$SEED"
export SEED_ENTROPY_TEST_READINESS=unsupported
run_initialize_failure
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"

# Only complete absence authorizes creation.  The initializer reads exactly
# one fake-random seed, installs it durably, and a second call leaves it alone.
new_case initialize_missing
rm -f "$SEED"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
run_initialize_success
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_size "$SEED" 512
assert_mode "$SEED" 600
assert_text "$OPEN_LOG" "R"
assert_absent "$CREDIT_LOG"
assert_absent "$CONSUMED"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
assert_absent "$WITNESS"
initialized_inode=$(stat -c '%d:%i' "$SEED")
make_bytes C "$RANDOM_SOURCE"
run_initialize_success
assert_same "$SEED" "$CASE_ROOT/expected-b"
[ "$(stat -c '%d:%i' "$SEED")" = "$initialized_inode" ] || \
    die "idempotent initialization replaced the seed"
pass
assert_text "$OPEN_LOG" "R"

# Metadata/content failures are not interpreted as absence and do not trigger
# a random read.
new_case initialize_invalid_mode
chmod 644 "$SEED"
run_initialize_failure
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

new_case initialize_degenerate_existing
dd if=/dev/zero of="$SEED" bs=512 count=1 2>/dev/null
chmod 600 "$SEED"
run_initialize_failure
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

# A consumed state belongs exclusively to boot-time replay prevention.  The
# shutdown initializer must never manufacture a public seed around it.
new_case initialize_consumed_state
mv "$SEED" "$CONSUMED"
run_initialize_failure
assert_absent "$SEED"
assert_exists "$CONSUMED"
assert_absent "$WITNESS"
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"

# A witness without a public name might not have reached file fsync, so it is
# intentionally unavailable and never promoted.
new_case initialize_orphan_witness
mv "$SEED" "$WITNESS"
run_initialize_failure
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$WITNESS"
assert_absent "$OPEN_LOG"

# Public+witness recovery is accepted only with exact same-inode proof.
new_case initialize_valid_witness_recovery
ln "$SEED" "$WITNESS"
run_initialize_success
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"
assert_mode "$SEED" 600

new_case initialize_mismatched_witness
make_bytes Z "$WITNESS"
run_initialize_failure
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_absent "$OPEN_LOG"
assert_absent "$CREDIT_LOG"

# Failure before the initial file is durable leaves only an ambiguous witness;
# retry refuses it and never performs another random read.
new_case initialize_file_sync_failure
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAIL_FILE_SYNC=1
run_initialize_failure
assert_absent "$SEED"
assert_exists "$WITNESS"
assert_text "$OPEN_LOG" "R"
assert_absent "$CREDIT_LOG"
unset SEED_ENTROPY_TEST_FAIL_FILE_SYNC
run_initialize_failure
assert_text "$OPEN_LOG" "R"

# The same conservative rule applies to a crash after the file fsync but before
# atomic installation.
new_case initialize_crash_after_file_sync
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAILPOINT=after_replacement_fsync
run_initialize_status 90
assert_absent "$SEED"
assert_exists "$WITNESS"
assert_text "$OPEN_LOG" "R"
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_failure
assert_text "$OPEN_LOG" "R"

# Once public and witness names refer to the same inode, restart can finish an
# interrupted install without another random read.
new_case initialize_crash_after_install
rm -f "$SEED"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_initialize_status 90
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_exists "$WITNESS"
[ "$(stat -c '%d:%i' "$SEED")" = "$(stat -c '%d:%i' "$WITNESS")" ] || \
    die "initial seed and witness do not share an inode"
pass
assert_text "$OPEN_LOG" "R"
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "R"

# Birth-marker publication is transactional.  A process crash before install,
# after atomic install, or after directory durability retains the same-inode
# seed witness and resumes without another random read.
new_case initialize_crash_after_birth_temporary
rm -f "$SEED"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_birth_marker_temporary_fsync
run_initialize_status 90
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_exists "$WITNESS"
assert_absent "$BIRTH"
assert_exists "$BIRTH_TEMP"
opens_before_retry=$(wc -l <"$OPEN_LOG" | tr -d '[:space:]')
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
assert_absent "$WITNESS"
[ "$(wc -l <"$OPEN_LOG" | tr -d '[:space:]')" = "$opens_before_retry" ] || \
    die "birth-marker temporary recovery reread random data"
pass

new_case initialize_crash_after_birth_install
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAILPOINT=after_birth_marker_install
run_initialize_status 90
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"

new_case initialize_crash_after_birth_fsync
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAILPOINT=after_birth_marker_fsync
run_initialize_status 90
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"

new_case initialize_birth_file_sync_failure
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAIL_FILE_SYNC=birth-marker
run_initialize_failure
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_absent "$BIRTH"
assert_exists "$BIRTH_TEMP"
assert_absent "$CREDIT_LOG"
opens_before_retry=$(wc -l <"$OPEN_LOG" | tr -d '[:space:]')
unset SEED_ENTROPY_TEST_FAIL_FILE_SYNC
run_initialize_success
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
assert_absent "$WITNESS"
[ "$(wc -l <"$OPEN_LOG" | tr -d '[:space:]')" = "$opens_before_retry" ] || \
    die "birth-marker fsync recovery reread random data"
pass

new_case initialize_birth_directory_sync_failure
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC='sync seed birth marker'
run_initialize_failure
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$BIRTH_TEMP"
assert_absent "$CREDIT_LOG"
unset SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC
run_initialize_success
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"

# A checked directory-fsync failure reports failure while retaining a
# recoverable same-inode witness.  Recovery remains non-generating.
new_case initialize_directory_sync_failure
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC='sync initial seed install'
run_initialize_failure
assert_exists "$SEED"
assert_exists "$WITNESS"
assert_text "$OPEN_LOG" "R"
unset SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC
run_initialize_success
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "R"

# A crash after unlinking the witness is safe whether that unlink persists or
# rolls back; the public seed was already fsynced in the directory.
new_case initialize_crash_after_witness_unlink
rm -f "$SEED"
export SEED_ENTROPY_TEST_FAILPOINT=after_initialize_witness_unlink
run_initialize_status 90
assert_exists "$SEED"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "R"
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_exists "$SEED"
assert_text "$OPEN_LOG" "R"

# Directory locking excludes a concurrent initializer before either process
# can read random data or create lifecycle state.
new_case initialize_concurrent_lock
rm -f "$SEED"
LOCK_READY=$CASE_ROOT/lock-ready
LOCK_RELEASE=$CASE_ROOT/lock-release
mkfifo "$LOCK_RELEASE"
flock -n "$KEYS" sh -c "touch '$LOCK_READY'; cat '$LOCK_RELEASE' >/dev/null" &
LOCKER_PID=$!
attempt=0
while [ ! -e "$LOCK_READY" ]; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 200 ] || die "timed out waiting for seed-directory lock"
    sleep 0.01
done
run_initialize_failure
assert_absent "$SEED"
assert_absent "$WITNESS"
assert_absent "$OPEN_LOG"
printf 'release\n' > "$LOCK_RELEASE"
wait "$LOCKER_PID"
LOCKER_PID=
run_initialize_success
assert_exists "$SEED"
assert_text "$OPEN_LOG" "R"

# A seed created during shutdown enters the unchanged one-time boot lifecycle.
new_case initialize_then_credit
rm -f "$SEED"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
run_initialize_success
make_bytes C "$RANDOM_SOURCE"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-c"
export SEED_ENTROPY_TEST_BOOT_ID=22222222-2222-4222-8222-222222222222
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-b"
assert_same "$SEED" "$CASE_ROOT/expected-c"
assert_absent "$CONSUMED"
assert_absent "$CREDITED"
assert_text "$BIRTH" "22222222-2222-4222-8222-222222222222"
assert_absent "$WITNESS"

# Normal lifecycle: credit A once, atomically persist B, then credit B once and
# persist C.  The open trace proves no random read precedes successful credit.
new_case normal
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_size "$MIX_LOG" 512
assert_same "$MIX_LOG" "$CASE_ROOT/expected-a"
assert_text "$IOCTL_META_LOG" "256 512"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_size "$SEED" 512
assert_mode "$SEED" 600
assert_absent "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "W
R"

make_bytes C "$RANDOM_SOURCE"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-c"
# A process retry in the seed's birth boot is idempotent: CRNG output from this
# boot is not credited back into the same kernel as independent entropy.
run_success
assert_size "$CREDIT_LOG" 512
assert_size "$MIX_LOG" 512
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"

# A different boot epoch expires the marker and authorizes the seed's one
# credit, then records the successor's new birth epoch.
export SEED_ENTROPY_TEST_BOOT_ID=22222222-2222-4222-8222-222222222222
run_success
assert_size "$CREDIT_LOG" 1024
assert_size "$MIX_LOG" 1024
dd if="$CREDIT_LOG" of="$CASE_ROOT/credited-a" bs=512 count=1 2>/dev/null
dd if="$CREDIT_LOG" of="$CASE_ROOT/credited-b" bs=512 skip=1 count=1 2>/dev/null
dd if="$MIX_LOG" of="$CASE_ROOT/mixed-a" bs=512 count=1 2>/dev/null
dd if="$MIX_LOG" of="$CASE_ROOT/mixed-b" bs=512 skip=1 count=1 2>/dev/null
assert_same "$CASE_ROOT/credited-a" "$CASE_ROOT/expected-a"
assert_same "$CASE_ROOT/credited-b" "$CASE_ROOT/expected-b"
assert_same "$CASE_ROOT/mixed-a" "$CASE_ROOT/expected-a"
assert_same "$CASE_ROOT/mixed-b" "$CASE_ROOT/expected-b"
assert_text "$IOCTL_META_LOG" "256 512
256 512"
assert_same "$SEED" "$CASE_ROOT/expected-c"
assert_text "$BIRTH" "22222222-2222-4222-8222-222222222222"

# A relative path, unsafe mode, wrong size, hard link, symlink, writable parent,
# or symlinked parent must fail before an entropy credit is recorded.
new_case relative_path
set +e
(cd "$KEYS" && "$BIN" random-seed >/dev/null 2>&1)
status=$?
set -e
[ "$status" -ne 0 ] || die "relative seed path was accepted"
pass
assert_absent "$CREDIT_LOG"

# Exercise the path-walker edge case where the seed's parent is exactly '/'.
if [ "$(id -u)" = 0 ]; then
    new_case root_parent
    make_bytes A "$ROOT_SEED"
    SEED=$ROOT_SEED
    run_success
    assert_same "$SEED" "$RANDOM_SOURCE"
    assert_absent "$ROOT_CONSUMED"
    assert_absent "$ROOT_CREDITED"
    assert_absent "$ROOT_WITNESS"
    rm -f "$ROOT_SEED"
fi

new_case unsafe_mode
chmod 644 "$SEED"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

new_case unsafe_size
dd if=/dev/zero of="$SEED" bs=511 count=1 2>/dev/null
chmod 600 "$SEED"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

new_case degenerate_seed
dd if=/dev/zero of="$SEED" bs=512 count=1 2>/dev/null
chmod 600 "$SEED"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

new_case hard_link
ln "$SEED" "$KEYS/second-name"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"

new_case seed_symlink
mv "$SEED" "$KEYS/target"
ln -s target "$SEED"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$KEYS/target"

new_case writable_parent
chmod 770 "$KEYS"
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$SEED"
chmod 700 "$KEYS"

new_case parent_symlink
mv "$KEYS" "$CASE_ROOT/real-keys"
ln -s real-keys "$KEYS"
SEED=$KEYS/random-seed
run_failure
assert_absent "$CREDIT_LOG"
assert_exists "$CASE_ROOT/real-keys/random-seed"

# The early boot script may create /dev/urandom manually, so character-device
# type alone is not an adequate identity check.  Both a regular file and the
# wrong character-device number fail before lifecycle state is consumed.
new_case random_device_wrong_type
export SEED_ENTROPY_TEST_RANDOM_DEVICE=wrong-type
run_failure
assert_exists "$SEED"
assert_exists "$BIRTH"
assert_absent "$CONSUMED"
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

new_case random_device_wrong_rdev
export SEED_ENTROPY_TEST_RANDOM_DEVICE=wrong-rdev
run_failure
assert_exists "$SEED"
assert_exists "$BIRTH"
assert_absent "$CONSUMED"
assert_absent "$MIX_LOG"
assert_absent "$CREDIT_LOG"

# Failed ioctl: the old seed is durably consumed, random is opened write-only,
# no random read occurs, and retry cannot replay the consumed bytes.
new_case ioctl_failure
export SEED_ENTROPY_TEST_IOCTL_FAIL=1
run_failure
assert_absent "$SEED"
assert_exists "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$CREDIT_LOG"
assert_text "$OPEN_LOG" "W"
unset SEED_ENTROPY_TEST_IOCTL_FAIL
run_failure
assert_absent "$CREDIT_LOG"

# Linux 4.4 may accept RNDADDENTROPY without initializing the nonblocking CRNG.
# A durable credited marker prevents replay while allowing a later CRNG-ready
# invocation to generate the successor without another entropy credit.
new_case replacement_crng_not_ready
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_READINESS=not-ready
run_failure
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_size "$CREDIT_LOG" 512
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "W
R"
unset SEED_ENTROPY_TEST_READINESS
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$RANDOM_SOURCE"
assert_absent "$CREDITED"
assert_absent "$WITNESS"

new_case replacement_crng_probe_error
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_READINESS=probe-error
run_failure
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_absent "$WITNESS"
unset SEED_ENTROPY_TEST_READINESS
run_initialize_success
assert_size "$CREDIT_LOG" 512
assert_same "$SEED" "$RANDOM_SOURCE"
assert_absent "$CREDITED"

# A crash after the credit marker is durable has the same resumable meaning.
new_case crash_after_credit_marker
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_FAILPOINT=after_credit_marker_fsync
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_absent "$WITNESS"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$SEED" "$RANDOM_SOURCE"
assert_absent "$CREDITED"

# Shutdown recovery uses the same credited proof.  A crash after removing that
# marker leaves a public+witness pair that initialization can finish without
# another credit or random read.
new_case initialize_credited_cleanup_crash
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_READINESS=not-ready
run_failure
assert_exists "$CREDITED"
unset SEED_ENTROPY_TEST_READINESS
export SEED_ENTROPY_TEST_FAILPOINT=after_initialize_credited_removal
run_initialize_status 90
assert_size "$CREDIT_LOG" 512
assert_exists "$SEED"
assert_absent "$CREDITED"
assert_exists "$WITNESS"
opens_before_retry=$(wc -l < "$OPEN_LOG" | tr -d '[:space:]')
unset SEED_ENTROPY_TEST_FAILPOINT
run_initialize_success
assert_exists "$SEED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
[ "$(wc -l < "$OPEN_LOG" | tr -d '[:space:]')" = "$opens_before_retry" ] || \
    die "initializer reread random data while finishing a proven install"
pass

# Mutually exclusive consumed and credited markers can never coexist in a
# valid transaction and must be rejected without another credit.
new_case conflicting_credit_markers
mv "$SEED" "$CONSUMED"
make_bytes B "$CREDITED"
run_failure
assert_exists "$CONSUMED"
assert_exists "$CREDITED"
assert_absent "$CREDIT_LOG"

# A failed directory fsync at the consume boundary prevents the ioctl.  The
# rename may already be visible, so availability is sacrificed conservatively.
new_case consumed_directory_sync_failure
export SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC='sync consumed seed'
run_failure
assert_absent "$SEED"
assert_exists "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$CREDIT_LOG"
unset SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC
run_failure
assert_absent "$CREDIT_LOG"

# Crash immediately after durable consumption: no credit happened, but replay
# is still refused because durability of the rename is the security boundary.
new_case crash_after_consume
export SEED_ENTROPY_TEST_FAILPOINT=after_consume_fsync
run_status 90
assert_absent "$SEED"
assert_exists "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_absent "$CREDIT_LOG"
unset SEED_ENTROPY_TEST_FAILPOINT
run_failure
assert_absent "$CREDIT_LOG"

# Crash immediately after successful ioctl: A appears exactly once in the
# simulated credit log and can never be credited again.
new_case crash_after_ioctl
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_FAILPOINT=after_ioctl
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_size "$CREDIT_LOG" 512
assert_absent "$SEED"
assert_exists "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_text "$OPEN_LOG" "W"
unset SEED_ENTROPY_TEST_FAILPOINT
run_failure
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"

# Failure to fsync the replacement file occurs after A was credited but before
# installation.  The uninstalled temporary has no authority: retry discards it
# durably and regenerates a successor without crediting A again.
new_case replacement_file_sync_failure
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAIL_FILE_SYNC=1
run_failure
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_exists "$WITNESS"
unset SEED_ENTROPY_TEST_FAIL_FILE_SYNC
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"

# A durable but uninstalled replacement is recovered by the same discard and
# regenerate rule.  The credited seed remains the sole replay authority.
new_case crash_after_replacement
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_replacement_fsync
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_exists "$WITNESS"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"

# A crash after durable atomic install leaves public and witness names on the
# same fresh inode.  Same-boot recovery finishes cleanup and records B's birth
# epoch without circularly crediting B back into its generating kernel.
new_case crash_after_install
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_exists "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_exists "$WITNESS"
[ "$(stat -c '%d:%i' "$SEED")" = "$(stat -c '%d:%i' "$WITNESS")" ] || \
    die "installed seed and witness are not the same inode"
pass
make_bytes C "$RANDOM_SOURCE"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-c"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"

export SEED_ENTROPY_TEST_BOOT_ID=22222222-2222-4222-8222-222222222222
run_success
assert_size "$CREDIT_LOG" 1024
dd if="$CREDIT_LOG" of="$CASE_ROOT/credited-b" bs=512 skip=1 count=1 2>/dev/null
assert_same "$CASE_ROOT/credited-b" "$CASE_ROOT/expected-b"
assert_same "$SEED" "$CASE_ROOT/expected-c"

# Recovery must publish the successor's boot epoch while D/T proof still
# exists.  Crashes during marker publication or either proof cleanup must never
# expose P-only/no-epoch state to a same-boot retry.
new_case recovery_crash_during_birth_marker
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_status 90
export SEED_ENTROPY_TEST_FAILPOINT=after_birth_marker_temporary_fsync
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_exists "$CREDITED"
assert_exists "$WITNESS"
assert_absent "$BIRTH"
assert_exists "$BIRTH_TEMP"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_absent "$BIRTH_TEMP"

new_case recovery_crash_after_credited_removal
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_status 90
export SEED_ENTROPY_TEST_FAILPOINT=after_recovery_credited_removal
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_absent "$CREDITED"
assert_exists "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$WITNESS"

new_case recovery_crash_after_witness_removal
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_status 90
export SEED_ENTROPY_TEST_FAILPOINT=after_recovery_witness_removal
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_absent "$CREDITED"
assert_absent "$WITNESS"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"

# Once credited-seed removal is durable, a remaining witness alone is enough
# to prove and finish the interrupted install.
new_case crash_after_credited_removal
cp "$SEED" "$CASE_ROOT/expected-a"
cp "$RANDOM_SOURCE" "$CASE_ROOT/expected-b"
export SEED_ENTROPY_TEST_FAILPOINT=after_credited_removal
run_status 90
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_exists "$SEED"
assert_absent "$CONSUMED"
assert_absent "$CREDITED"
assert_exists "$WITNESS"
make_bytes C "$RANDOM_SOURCE"
unset SEED_ENTROPY_TEST_FAILPOINT
run_success
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_same "$SEED" "$CASE_ROOT/expected-b"
assert_text "$BIRTH" "11111111-1111-4111-8111-111111111111"
assert_absent "$CONSUMED"
assert_absent "$CREDITED"
assert_absent "$WITNESS"

# Replacing the public file while a credited seed exists destroys the
# same-inode proof and is rejected without another credit.
new_case tampered_install
cp "$SEED" "$CASE_ROOT/expected-a"
export SEED_ENTROPY_TEST_FAILPOINT=after_install_fsync
run_status 90
assert_size "$CREDIT_LOG" 512
rm -f "$SEED"
make_bytes Z "$SEED"
unset SEED_ENTROPY_TEST_FAILPOINT
run_failure
assert_size "$CREDIT_LOG" 512
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_exists "$WITNESS"

# Obvious zero-filled replacement corruption is never installed as a seed.
new_case degenerate_random_source
cp "$SEED" "$CASE_ROOT/expected-a"
dd if=/dev/zero of="$RANDOM_SOURCE" bs=512 count=1 2>/dev/null
chmod 600 "$RANDOM_SOURCE"
run_failure
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
assert_absent "$WITNESS"
run_failure
assert_size "$CREDIT_LOG" 512

# Even a broken random source cannot make the already-credited seed replay.
new_case short_random_source
cp "$SEED" "$CASE_ROOT/expected-a"
dd if=/dev/zero of="$RANDOM_SOURCE" bs=16 count=1 2>/dev/null
chmod 600 "$RANDOM_SOURCE"
run_failure
assert_same "$CREDIT_LOG" "$CASE_ROOT/expected-a"
assert_absent "$SEED"
assert_absent "$CONSUMED"
assert_exists "$CREDITED"
run_failure
assert_size "$CREDIT_LOG" 512

echo "PASS: seed-entropy one-time lifecycle ($ASSERTIONS assertions)"
