#!/bin/sh
# Mount/device-free adversarial tests for the Zynq resource-ledger v2 core.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
HELPER=$PROJECT_ROOT/scripts/lib/sysupgrade_resource_ledger.sh
TARGET_RUNTIME_HELPER=$PROJECT_ROOT/br2_external_dcentos/board/zynq/rootfs-overlay/usr/libexec/dcentos/sysupgrade-resource-ledger.sh
WORK_ROOT=${TMPDIR:-/tmp}/dcent-sysupgrade-resource-ledger-v2-test.$$
LOCK=$WORK_ROOT/transaction.lock
LEDGER=$LOCK/ledger
MAINTENANCE_LOCK=$WORK_ROOT/maintenance.lock
TX_ID=tx-0123456789abcdef
BOOT_ID=01234567-89ab-cdef-0123-456789abcdef
OWNER_PID=1234
OWNER_START=567890
OWNER_MNTNS=41:42
EVIDENCE_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
EVIDENCE_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
EVIDENCE_C=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
EVIDENCE_D=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
EVIDENCE_E=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
EVIDENCE_F=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
BINDING_EVAL_SENTINEL=$WORK_ROOT/binding-eval-sentinel
STATUS_EVAL_SENTINEL=$WORK_ROOT/status-eval-sentinel
failures=0
tests=0

if [ ! -r "$HELPER" ]; then
    printf 'FAIL: offline resource-ledger specification is missing: %s\n' "$HELPER" >&2
    exit 1
fi
if [ -e "$TARGET_RUNTIME_HELPER" ]; then
    printf 'FAIL: offline resource-ledger specification leaked into the production overlay: %s\n' "$TARGET_RUNTIME_HELPER" >&2
    exit 1
fi

cleanup()
{
    rm -rf "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

# shellcheck source=/dev/null
. "$HELPER"

pass() { tests=$((tests + 1)); printf 'PASS: %s\n' "$1"; }
fail() { tests=$((tests + 1)); failures=$((failures + 1)); printf 'FAIL: %s\n' "$1" >&2; }
expect_success()
{
    _test_label=$1
    shift
    if "$@"; then pass "$_test_label"; else fail "$_test_label"; fi
}
expect_failure()
{
    _test_label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$_test_label (unexpected success)"
    else
        pass "$_test_label"
    fi
}
reset_binding()
{
    DCENT_SYSUPGRADE_LEDGER_BOUND=0
    DCENT_SYSUPGRADE_LEDGER_ACTOR=
    DCENT_SYSUPGRADE_LEDGER_DIR=
    DCENT_SYSUPGRADE_LEDGER_TRANSACTION_ID=
    DCENT_SYSUPGRADE_LEDGER_BOOT_ID=
    DCENT_SYSUPGRADE_LEDGER_OWNER_PID=
    DCENT_SYSUPGRADE_LEDGER_OWNER_STARTTIME=
    DCENT_SYSUPGRADE_LEDGER_OWNER_MOUNT_NAMESPACE=
    DCENT_SYSUPGRADE_LEDGER_LOCK_PATH=
    DCENT_SYSUPGRADE_LEDGER_LOCK_DEVICE_INODE=
    DCENT_SYSUPGRADE_LEDGER_BINDING_SHA256=
    DCENT_SYSUPGRADE_LEDGER_CLAIM_ID=
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_BOOT_ID=
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_PID=
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_STARTTIME=
    DCENT_SYSUPGRADE_LEDGER_RECONCILER_MOUNT_NAMESPACE=
}
digest_file()
{
    _test_digest_output=$(sha256sum "$1") || return 1
    printf '%s\n' "${_test_digest_output%% *}"
}

TEST_UID=$(id -u)
expect_success "deployed ledger policy admits only uid 0" \
    test "$(dcent_sysupgrade_ledger_expected_uid)" = 0
# Test-local override; the deployed helper remains root-only.
dcent_sysupgrade_ledger_expected_uid()
{
    printf '%s\n' "$TEST_UID"
}

mkdir -p "$LOCK" "$MAINTENANCE_LOCK"
chmod 700 "$WORK_ROOT" "$LOCK" "$MAINTENANCE_LOCK"
LOCK_DEVINO=$(stat -c '%d:%i' "$LOCK")
MAINTENANCE_DEVINO=$(stat -c '%d:%i' "$MAINTENANCE_LOCK")

# Binding creation is intentionally incompatible with v1.
expect_failure "v1 five-argument create calls fail closed" \
    dcent_sysupgrade_ledger_create "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START"
expect_failure "relative ledger paths are refused" \
    dcent_sysupgrade_ledger_create relative-ledger "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"
expect_failure "ledger must be the exact transaction-lock child" \
    dcent_sysupgrade_ledger_create "$LOCK/not-ledger" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"
expect_failure "transaction identifiers cannot traverse" \
    dcent_sysupgrade_ledger_create "$LEDGER" ../tx "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"
expect_failure "mount namespace identity is mandatory" \
    dcent_sysupgrade_ledger_create "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" not-a-devino "$LOCK" "$LOCK_DEVINO"
expect_failure "recorded lock identity must match the directory" \
    dcent_sysupgrade_ledger_create "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" 1:2

mkdir "$WORK_ROOT/symlink-lock-real"
chmod 700 "$WORK_ROOT/symlink-lock-real"
ln -s "$WORK_ROOT/symlink-lock-real" "$WORK_ROOT/symlink-lock"
SYMLINK_LOCK_DEVINO=$(stat -Lc '%d:%i' "$WORK_ROOT/symlink-lock")
expect_failure "symlink transaction-lock directories are refused" \
    dcent_sysupgrade_ledger_create "$WORK_ROOT/symlink-lock/ledger" \
        "$TX_ID" "$BOOT_ID" "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" \
        "$WORK_ROOT/symlink-lock" "$SYMLINK_LOCK_DEVINO"

expect_success "a transaction creates a v2 resource ledger" \
    dcent_sysupgrade_ledger_create "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"
expect_success "ledger directory mode is exactly 0700" \
    test "$(stat -c %a "$LEDGER")" = 700
expect_success "resource directory mode is exactly 0700" \
    test "$(stat -c %a "$LEDGER/resources")" = 700
expect_success "binding mode is exactly 0600" \
    test "$(stat -c %a "$LEDGER/binding")" = 600
expect_success "binding has exactly one hard link" \
    test "$(stat -c %h "$LEDGER/binding")" = 1
expect_success "binding is exactly the incompatible v2 schema" \
    grep -q '^schema=dcentos-sysupgrade-resource-ledger-v2$' "$LEDGER/binding"
expect_success "binding records owner mount namespace identity" \
    grep -q "^owner_mount_namespace=$OWNER_MNTNS\$" "$LEDGER/binding"
expect_success "binding records exact transaction lock path" \
    grep -q "^transaction_lock_path=$LOCK\$" "$LEDGER/binding"
expect_success "binding records exact lock device and inode" \
    grep -q "^transaction_lock_device_inode=$LOCK_DEVINO\$" "$LEDGER/binding"
expect_success "binding records canonical ledger path" \
    grep -q "^ledger_path=$LEDGER\$" "$LEDGER/binding"
expect_success "fresh ledger reads back exactly" dcent_sysupgrade_ledger_verify_owned
expect_failure "a bound process cannot create a second ledger" \
    dcent_sysupgrade_ledger_create "$LOCK/ledger2" tx-2 "$BOOT_ID" 2 2 \
        "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"

# A syntactically complete v1 ledger is evidence, never an implicit migration.
LEGACY_LOCK=$WORK_ROOT/legacy.lock
LEGACY_LEDGER=$LEGACY_LOCK/ledger
mkdir -p "$LEGACY_LEDGER/resources"
chmod 700 "$LEGACY_LOCK" "$LEGACY_LEDGER" "$LEGACY_LEDGER/resources"
LEGACY_DEVINO=$(stat -c '%d:%i' "$LEGACY_LOCK")
printf '%s\n' \
    'schema=dcentos-sysupgrade-resource-ledger-v1' \
    "transaction_id=$TX_ID" \
    "boot_id=$BOOT_ID" \
    "owner_pid=$OWNER_PID" \
    "owner_starttime=$OWNER_START" \
    'owner=zynq-sysupgrade' >"$LEGACY_LEDGER/binding"
chmod 600 "$LEGACY_LEDGER/binding"
reset_binding
expect_failure "legacy v1 ledgers are refused rather than migrated" \
    dcent_sysupgrade_ledger_open_owned "$LEGACY_LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LEGACY_LOCK" "$LEGACY_DEVINO"
expect_success "refused legacy binding remains byte-exact evidence" \
    grep -q '^schema=dcentos-sysupgrade-resource-ledger-v1$' "$LEGACY_LEDGER/binding"

expect_failure "open refuses a different transaction" \
    dcent_sysupgrade_ledger_open_owned "$LEDGER" wrong "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"
expect_failure "open refuses a different owner mount namespace" \
    dcent_sysupgrade_ledger_open_owned "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" 99:99 "$LOCK" "$LOCK_DEVINO"
expect_failure "open refuses a different lock identity" \
    dcent_sysupgrade_ledger_open_owned "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" 1:2
expect_success "exact owner binding reopens" \
    dcent_sysupgrade_ledger_open_owned "$LEDGER" "$TX_ID" "$BOOT_ID" \
        "$OWNER_PID" "$OWNER_START" "$OWNER_MNTNS" "$LOCK" "$LOCK_DEVINO"

# Typed intent admission and created/borrowed provenance.
expect_failure "v1 five-argument resource calls fail closed" \
    dcent_sysupgrade_ledger_resource_pending attachment old 7 1 -
expect_failure "unknown resource kinds are refused" \
    dcent_sysupgrade_ledger_resource_pending volume rootfs created 1 2 3
expect_failure "unknown provenance is refused" \
    dcent_sysupgrade_ledger_resource_pending attachment bad adopted 7 1 -
expect_failure "attachment numbers reject leading zeroes" \
    dcent_sysupgrade_ledger_resource_pending attachment bad created 07 1 -
expect_failure "node paths must be absolute" \
    dcent_sysupgrade_ledger_resource_pending node bad created dev/ubi1 250:0 -
expect_failure "node rdev identities reject leading zeroes" \
    dcent_sysupgrade_ledger_resource_pending node bad created /dev/ubi1 250:00 -
expect_failure "mount targets reject traversal" \
    dcent_sysupgrade_ledger_resource_pending mount bad created \
        /dev/ubi1_2 /tmp/../data rw
expect_failure "workspace intents require parent device/inode" \
    dcent_sysupgrade_ledger_resource_pending workspace bad created \
        /tmp/sysupgrade not-devino -
expect_success "invalid intents reserve no ledger operation" \
    test ! -e "$LEDGER/.operation"

expect_success "created attachment intent is published pending" \
    dcent_sysupgrade_ledger_resource_pending attachment inactive created 7 1 -
ATTACH_DIR=$LEDGER/resources/attachment--inactive
ATTACH_INTENT=$(digest_file "$ATTACH_DIR/intent")
expect_success "resource directory is private" test "$(stat -c %a "$ATTACH_DIR")" = 700
expect_success "immutable intent is a distinct 0600 receipt" \
    test "$(stat -c %a "$ATTACH_DIR/intent")" = 600
expect_success "initial status is a distinct 0600 receipt" \
    test "$(stat -c %a "$ATTACH_DIR/status.1")" = 600
expect_success "intent records created provenance" \
    grep -q '^provenance=created$' "$ATTACH_DIR/intent"
expect_success "pending status authenticates exact intent digest" \
    grep -q "^intent_sha256=$ATTACH_INTENT\$" "$ATTACH_DIR/status.1"
expect_success "pending status uses intent digest as precondition evidence" \
    grep -q "^evidence_sha256=$ATTACH_INTENT\$" "$ATTACH_DIR/status.1"
expect_success "pending created resource reads back exactly" \
    dcent_sysupgrade_ledger_resource_expect attachment inactive created pending \
        "$ATTACH_INTENT" 7 1 -
expect_failure "ordinary release cannot skip pending mutation ambiguity" \
    dcent_sysupgrade_ledger_resource_released attachment inactive "$EVIDENCE_A"
expect_success "proven no-mutation path releases pending resource" \
    dcent_sysupgrade_ledger_resource_absent_released attachment inactive "$EVIDENCE_A"
expect_success "pending-to-released is exactly revision two" \
    grep -q '^revision=2$' "$ATTACH_DIR/status.2"
expect_failure "released resources are terminal" \
    dcent_sysupgrade_ledger_resource_active attachment inactive "$EVIDENCE_B"

expect_success "second attachment is pending independently" \
    dcent_sysupgrade_ledger_resource_pending attachment full created 8 2 -
expect_success "pending attachment advances active" \
    dcent_sysupgrade_ledger_resource_active attachment full "$EVIDENCE_A"
expect_success "active attachment requires release intent" \
    dcent_sysupgrade_ledger_resource_release_pending attachment full "$EVIDENCE_B"
expect_success "release-pending attachment advances released" \
    dcent_sysupgrade_ledger_resource_released attachment full "$EVIDENCE_C"
FULL_DIR=$LEDGER/resources/attachment--full
expect_success "full lifecycle appends all four status revisions" \
    test "$(find "$FULL_DIR" -maxdepth 1 -name 'status.*' -type f | wc -l | tr -d ' ')" = 4
STATUS1_DIGEST=$(digest_file "$FULL_DIR/status.1")
STATUS2_DIGEST=$(digest_file "$FULL_DIR/status.2")
STATUS3_DIGEST=$(digest_file "$FULL_DIR/status.3")
expect_success "revision two chains revision one digest" \
    grep -q "^previous_status_sha256=$STATUS1_DIGEST\$" "$FULL_DIR/status.2"
expect_success "revision three chains revision two digest" \
    grep -q "^previous_status_sha256=$STATUS2_DIGEST\$" "$FULL_DIR/status.3"
expect_success "revision four chains revision three digest" \
    grep -q "^previous_status_sha256=$STATUS3_DIGEST\$" "$FULL_DIR/status.4"
expect_success "released resource retains final observation digest" \
    dcent_sysupgrade_ledger_resource_expect attachment full created released \
        "$EVIDENCE_C" 8 2 -

expect_success "borrowed node intent is distinct from created ownership" \
    dcent_sysupgrade_ledger_resource_pending node borrowed-control borrowed \
        /dev/ubi1 250:0 -
BORROWED_DIR=$LEDGER/resources/node--borrowed-control
expect_success "borrowed provenance is immutable intent" \
    grep -q '^provenance=borrowed$' "$BORROWED_DIR/intent"
expect_success "borrowed resource can become active" \
    dcent_sysupgrade_ledger_resource_active node borrowed-control "$EVIDENCE_A"
expect_success "borrowed resource can record relinquishment intent" \
    dcent_sysupgrade_ledger_resource_release_pending node borrowed-control "$EVIDENCE_B"
expect_success "borrowed resource can be relinquished without changing provenance" \
    dcent_sysupgrade_ledger_resource_released node borrowed-control "$EVIDENCE_C"
expect_success "borrowed provenance survives the lifecycle" \
    grep -q '^provenance=borrowed$' "$BORROWED_DIR/intent"

expect_success "workspace planned path can be recorded before mkdir" \
    dcent_sysupgrade_ledger_resource_pending workspace work created \
        /tmp/dcentos-sysupgrade.abc 51:52 -
expect_success "workspace ambiguity may become terminal conflict" \
    dcent_sysupgrade_ledger_resource_conflict workspace work "$EVIDENCE_D"
expect_failure "conflict is terminal" \
    dcent_sysupgrade_ledger_resource_absent_released workspace work "$EVIDENCE_E"
expect_failure "duplicate resource identities cannot replace intent" \
    dcent_sysupgrade_ledger_resource_pending workspace work created \
        /tmp/dcentos-sysupgrade.abc 51:52 -
expect_success "complete transitions leave no hidden temporary receipts" \
    test -z "$(find "$LEDGER/resources" -name '.*' -print)"
expect_success "complete transitions release global operation reservation" \
    test ! -e "$LEDGER/.operation"

# Fixed-order parsing, intent authentication, append-only rollback detection.
BINDING_SAVE=$WORK_ROOT/binding.good
cp "$LEDGER/binding" "$BINDING_SAVE"
printf '%s\n' extra=torn >>"$LEDGER/binding"
expect_failure "binding with an extra line fails closed" dcent_sysupgrade_ledger_verify_owned
cp "$BINDING_SAVE" "$LEDGER/binding"
chmod 600 "$LEDGER/binding"

printf '%s' extra=unterminated >>"$LEDGER/binding"
expect_failure "binding with unterminated trailing data fails closed" \
    dcent_sysupgrade_ledger_verify_owned
cp "$BINDING_SAVE" "$LEDGER/binding"
chmod 600 "$LEDGER/binding"

sed "2s|^transaction_id=.*|transaction_id=\$(touch $BINDING_EVAL_SENTINEL)|" \
    "$BINDING_SAVE" >"$LEDGER/binding"
chmod 600 "$LEDGER/binding"
expect_failure "shell-like binding text is fixed-order data and refused" \
    dcent_sysupgrade_ledger_verify_owned
expect_success "binding parser never evaluates shell-like data" \
    test ! -e "$BINDING_EVAL_SENTINEL"
cp "$BINDING_SAVE" "$LEDGER/binding"
chmod 600 "$LEDGER/binding"

ln "$LEDGER/binding" "$WORK_ROOT/binding-hardlink"
expect_failure "hard-linked binding receipts are refused" dcent_sysupgrade_ledger_verify_owned
rm -f "$WORK_ROOT/binding-hardlink"
expect_success "binding is admitted after link ambiguity clears" \
    dcent_sysupgrade_ledger_verify_owned

FULL_INTENT_SAVE=$WORK_ROOT/full-intent.good
cp "$FULL_DIR/intent" "$FULL_INTENT_SAVE"
sed 's/^identity_a=8$/identity_a=9/' "$FULL_INTENT_SAVE" >"$FULL_DIR/intent"
chmod 600 "$FULL_DIR/intent"
expect_failure "status digest detects rewritten immutable intent" \
    dcent_sysupgrade_ledger_verify_owned
cp "$FULL_INTENT_SAVE" "$FULL_DIR/intent"
chmod 600 "$FULL_DIR/intent"
expect_success "byte-exact intent restoration is admitted" \
    dcent_sysupgrade_ledger_verify_owned

mv "$FULL_DIR/status.3" "$WORK_ROOT/status.3.save"
expect_failure "missing intermediate status revision is rollback/torn evidence" \
    dcent_sysupgrade_ledger_verify_owned
mv "$WORK_ROOT/status.3.save" "$FULL_DIR/status.3"
expect_success "restored digest-chain status is admitted" dcent_sysupgrade_ledger_verify_owned

STATUS2_SAVE=$WORK_ROOT/status.2.good
cp "$FULL_DIR/status.2" "$STATUS2_SAVE"
sed "s|^phase=active$|phase=\$(touch $STATUS_EVAL_SENTINEL)|" \
    "$STATUS2_SAVE" >"$FULL_DIR/status.2"
chmod 600 "$FULL_DIR/status.2"
expect_failure "shell-like status data is refused without evaluation" \
    dcent_sysupgrade_ledger_verify_owned
expect_success "status parser never evaluates shell-like data" \
    test ! -e "$STATUS_EVAL_SENTINEL"
cp "$STATUS2_SAVE" "$FULL_DIR/status.2"
chmod 600 "$FULL_DIR/status.2"

STATUS4_SAVE=$WORK_ROOT/status.4.good
cp "$FULL_DIR/status.4" "$STATUS4_SAVE"
printf '%s' extra=unterminated >>"$FULL_DIR/status.4"
expect_failure "latest status with unterminated trailing data fails closed" \
    dcent_sysupgrade_ledger_verify_owned
cp "$STATUS4_SAVE" "$FULL_DIR/status.4"
chmod 600 "$FULL_DIR/status.4"

printf '%s\n' partial >"$FULL_DIR/.status.new.5.foreign"
expect_failure "orphaned resource publication fails closed" \
    dcent_sysupgrade_ledger_verify_owned
expect_success "orphan remains for explicit inspection" \
    grep -q '^partial$' "$FULL_DIR/.status.new.5.foreign"
rm -f "$FULL_DIR/.status.new.5.foreign"

# Build a stale transaction with both pending and active resources for a
# claimed reconciler.  It uses a separate transaction lock and binding.
reset_binding
CLAIM_LOCK=$WORK_ROOT/claim-transaction.lock
CLAIM_LEDGER=$CLAIM_LOCK/ledger
mkdir "$CLAIM_LOCK"
chmod 700 "$CLAIM_LOCK"
CLAIM_LOCK_DEVINO=$(stat -c '%d:%i' "$CLAIM_LOCK")
expect_success "stale transaction ledger is created" \
    dcent_sysupgrade_ledger_create "$CLAIM_LEDGER" stale-tx "$BOOT_ID" \
        4321 7654321 61:62 "$CLAIM_LOCK" "$CLAIM_LOCK_DEVINO"
expect_success "stale ledger retains pending ambiguity" \
    dcent_sysupgrade_ledger_resource_pending attachment maybe-attached created 9 1 -
expect_success "stale ledger retains active ownership" \
    dcent_sysupgrade_ledger_resource_pending node active-node created /dev/ubi1 250:0 -
expect_success "owner records active node before becoming stale" \
    dcent_sysupgrade_ledger_resource_active node active-node "$EVIDENCE_A"
reset_binding

expect_failure "claim refuses a different target transaction" \
    dcent_sysupgrade_ledger_reconcile_claim "$CLAIM_LEDGER" wrong claimant \
        "$BOOT_ID" 9001 111 71:72 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO"
expect_success "wrong-transaction refusal publishes no claim" \
    test ! -e "$CLAIM_LEDGER/reconcile.claim"
expect_failure "claim refuses non-digest owner-death evidence" \
    dcent_sysupgrade_ledger_reconcile_claim "$CLAIM_LEDGER" stale-tx claimant \
        "$BOOT_ID" 9001 111 71:72 owner-is-dead \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO"
expect_failure "claim refuses substituted maintenance lock identity" \
    dcent_sysupgrade_ledger_reconcile_claim "$CLAIM_LEDGER" stale-tx claimant \
        "$BOOT_ID" 9001 111 71:72 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" 1:2

RUNNER=$WORK_ROOT/claim-runner.sh
printf '%s\n' \
    '#!/bin/sh' \
    '. "$1"' \
    'TEST_UID=$2' \
    'dcent_sysupgrade_ledger_expected_uid() { printf "%s\\n" "$TEST_UID"; }' \
    'dcent_sysupgrade_ledger_reconcile_claim "$3" "$4" "$5" "$6" "$7" "$8" "$9" "${10}" "${11}" "${12}"' \
    >"$RUNNER"
chmod 700 "$RUNNER"
(
    if sh "$RUNNER" "$HELPER" "$TEST_UID" "$CLAIM_LEDGER" stale-tx \
        contender-a "$BOOT_ID" 9101 111 81:82 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO" >/dev/null 2>&1; then
        printf '%s\n' success >"$WORK_ROOT/claim-a.result"
    else
        printf '%s\n' failure >"$WORK_ROOT/claim-a.result"
    fi
) &
CLAIM_A_PID=$!
(
    if sh "$RUNNER" "$HELPER" "$TEST_UID" "$CLAIM_LEDGER" stale-tx \
        contender-b "$BOOT_ID" 9102 222 83:84 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO" >/dev/null 2>&1; then
        printf '%s\n' success >"$WORK_ROOT/claim-b.result"
    else
        printf '%s\n' failure >"$WORK_ROOT/claim-b.result"
    fi
) &
CLAIM_B_PID=$!
wait "$CLAIM_A_PID"
wait "$CLAIM_B_PID"
CLAIM_WINNERS=$(grep -l '^success$' "$WORK_ROOT/claim-a.result" \
    "$WORK_ROOT/claim-b.result" | wc -l | tr -d ' ')
if [ "$CLAIM_WINNERS" = 1 ]; then
    pass "exactly one concurrent reconciler wins"
else
    fail "exactly one concurrent reconciler wins"
fi
if grep -q '^success$' "$WORK_ROOT/claim-a.result"; then
    CLAIM_ID=contender-a
    CLAIM_PID=9101
    CLAIM_START=111
    CLAIM_MNTNS=81:82
else
    CLAIM_ID=contender-b
    CLAIM_PID=9102
    CLAIM_START=222
    CLAIM_MNTNS=83:84
fi
CLAIM_DIR=$CLAIM_LEDGER/reconcile.claim
expect_success "claim has immutable intent separate from status" \
    test -f "$CLAIM_DIR/intent" -a -f "$CLAIM_DIR/status.1"
expect_success "claim intent records owner-death evidence digest" \
    grep -q "^owner_death_evidence_sha256=$EVIDENCE_D\$" "$CLAIM_DIR/intent"
expect_success "claim intent records reconciler mount namespace" \
    grep -q "^reconciler_mount_namespace=$CLAIM_MNTNS\$" "$CLAIM_DIR/intent"
expect_success "claim intent records maintenance lock identity" \
    grep -q "^maintenance_lock_device_inode=$MAINTENANCE_DEVINO\$" "$CLAIM_DIR/intent"
expect_success "claim begins at claimed revision one" \
    grep -q '^phase=claimed$' "$CLAIM_DIR/status.1"
expect_failure "claimed ledger cannot reopen under stale owner" \
    dcent_sysupgrade_ledger_open_owned "$CLAIM_LEDGER" stale-tx "$BOOT_ID" \
        4321 7654321 61:62 "$CLAIM_LOCK" "$CLAIM_LOCK_DEVINO"
expect_failure "claimed ledger cannot be claimed a second time" \
    dcent_sysupgrade_ledger_reconcile_claim "$CLAIM_LEDGER" stale-tx third \
        "$BOOT_ID" 9200 333 91:92 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO"

reset_binding
expect_failure "reconciler open refuses wrong claimant namespace" \
    dcent_sysupgrade_ledger_reconcile_open "$CLAIM_LEDGER" stale-tx \
        "$CLAIM_ID" "$BOOT_ID" "$CLAIM_PID" "$CLAIM_START" 99:99
expect_success "exact winning reconciler reopens claim" \
    dcent_sysupgrade_ledger_reconcile_open "$CLAIM_LEDGER" stale-tx \
        "$CLAIM_ID" "$BOOT_ID" "$CLAIM_PID" "$CLAIM_START" "$CLAIM_MNTNS"
expect_failure "claimed phase cannot mutate resources before quiescence" \
    dcent_sysupgrade_ledger_resource_absent_released attachment maybe-attached "$EVIDENCE_B"
expect_failure "quiescence requires a byte-exact digest" \
    dcent_sysupgrade_ledger_reconcile_quiescent "$CLAIM_ID" not-a-digest
expect_success "claim advances claimed to quiescent" \
    dcent_sysupgrade_ledger_reconcile_quiescent "$CLAIM_ID" "$EVIDENCE_E"
expect_success "quiescence digest is persisted" \
    grep -q "^quiescence_sha256=$EVIDENCE_E\$" "$CLAIM_DIR/status.2"
expect_failure "reconciliation refuses a different quiescence digest" \
    dcent_sysupgrade_ledger_reconcile_begin "$CLAIM_ID" "$EVIDENCE_F"
expect_success "claim advances quiescent to reconciling" \
    dcent_sysupgrade_ledger_reconcile_begin "$CLAIM_ID" "$EVIDENCE_E"
expect_failure "completion refuses nonterminal resources" \
    dcent_sysupgrade_ledger_reconcile_complete "$CLAIM_ID" "$EVIDENCE_E" "$EVIDENCE_F"
expect_success "reconciler proves pending resource never mutated" \
    dcent_sysupgrade_ledger_resource_absent_released attachment maybe-attached "$EVIDENCE_B"
expect_success "reconciler records active resource release intent" \
    dcent_sysupgrade_ledger_resource_release_pending node active-node "$EVIDENCE_B"
expect_success "reconciler records exact resource release" \
    dcent_sysupgrade_ledger_resource_released node active-node "$EVIDENCE_C"
expect_success "reconciler actor is recorded in appended resource status" \
    grep -q '^actor_kind=reconciler$' \
        "$CLAIM_LEDGER/resources/node--active-node/status.3"
expect_success "reconciler actor ID is exact winning claim" \
    grep -q "^actor_id=$CLAIM_ID\$" \
        "$CLAIM_LEDGER/resources/node--active-node/status.3"
expect_success "fully released ledger permits claim completion" \
    dcent_sysupgrade_ledger_reconcile_complete "$CLAIM_ID" "$EVIDENCE_E" "$EVIDENCE_F"
expect_success "claim completion is explicit terminal revision four" \
    grep -q '^phase=complete$' "$CLAIM_DIR/status.4"
expect_success "completion records independent outcome digest" \
    grep -q "^outcome_sha256=$EVIDENCE_F\$" "$CLAIM_DIR/status.4"
expect_failure "complete claimant cannot mutate resources" \
    dcent_sysupgrade_ledger_resource_conflict node active-node "$EVIDENCE_D"
expect_failure "complete claim cannot complete twice" \
    dcent_sysupgrade_ledger_reconcile_complete "$CLAIM_ID" "$EVIDENCE_E" "$EVIDENCE_F"

# Rollback of claim status is detected by its contiguous digest chain.
mv "$CLAIM_DIR/status.3" "$WORK_ROOT/claim-status.3.save"
reset_binding
expect_failure "missing claim revision blocks exact reconciler reopen" \
    dcent_sysupgrade_ledger_reconcile_open "$CLAIM_LEDGER" stale-tx \
        "$CLAIM_ID" "$BOOT_ID" "$CLAIM_PID" "$CLAIM_START" "$CLAIM_MNTNS"
mv "$WORK_ROOT/claim-status.3.save" "$CLAIM_DIR/status.3"
expect_success "restored claim chain reopens for terminal inspection" \
    dcent_sysupgrade_ledger_reconcile_open "$CLAIM_LEDGER" stale-tx \
        "$CLAIM_ID" "$BOOT_ID" "$CLAIM_PID" "$CLAIM_START" "$CLAIM_MNTNS"

# A blocked claim is terminal and distinct from successful completion.
reset_binding
BLOCK_LOCK=$WORK_ROOT/block-transaction.lock
BLOCK_LEDGER=$BLOCK_LOCK/ledger
mkdir "$BLOCK_LOCK"
chmod 700 "$BLOCK_LOCK"
BLOCK_LOCK_DEVINO=$(stat -c '%d:%i' "$BLOCK_LOCK")
expect_success "independent ledger for blocked claim is created" \
    dcent_sysupgrade_ledger_create "$BLOCK_LEDGER" block-tx "$BOOT_ID" \
        5001 501 101:102 "$BLOCK_LOCK" "$BLOCK_LOCK_DEVINO"
reset_binding
expect_success "independent reconciler claims blocked ledger" \
    dcent_sysupgrade_ledger_reconcile_claim "$BLOCK_LEDGER" block-tx blocker \
        "$BOOT_ID" 5002 502 103:104 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO"
expect_success "claim may terminate blocked from claimed" \
    dcent_sysupgrade_ledger_reconcile_block blocker "$EVIDENCE_F"
expect_success "blocked outcome is explicit" \
    grep -q '^phase=blocked$' "$BLOCK_LEDGER/reconcile.claim/status.2"
expect_failure "blocked claim cannot advance to quiescent" \
    dcent_sysupgrade_ledger_reconcile_quiescent blocker "$EVIDENCE_E"
reset_binding
expect_failure "blocked ledger remains unavailable to old owner" \
    dcent_sysupgrade_ledger_open_owned "$BLOCK_LEDGER" block-tx "$BOOT_ID" \
        5001 501 101:102 "$BLOCK_LOCK" "$BLOCK_LOCK_DEVINO"

# A torn claim is permanent blocking evidence; no contender fabricates state.
TORN_LOCK=$WORK_ROOT/torn-transaction.lock
TORN_LEDGER=$TORN_LOCK/ledger
mkdir "$TORN_LOCK"
chmod 700 "$TORN_LOCK"
TORN_LOCK_DEVINO=$(stat -c '%d:%i' "$TORN_LOCK")
expect_success "independent ledger for torn claim is created" \
    dcent_sysupgrade_ledger_create "$TORN_LEDGER" torn-tx "$BOOT_ID" \
        6001 601 111:112 "$TORN_LOCK" "$TORN_LOCK_DEVINO"
reset_binding
mkdir "$TORN_LEDGER/reconcile.claim"
chmod 700 "$TORN_LEDGER/reconcile.claim"
expect_failure "torn claim cannot be stolen" \
    dcent_sysupgrade_ledger_reconcile_claim "$TORN_LEDGER" torn-tx replacement \
        "$BOOT_ID" 6002 602 113:114 "$EVIDENCE_D" \
        "$MAINTENANCE_LOCK" "$MAINTENANCE_DEVINO"
expect_success "torn claim directory remains for inspection" \
    test -d "$TORN_LEDGER/reconcile.claim"
expect_success "torn claim has no fabricated status" \
    test ! -e "$TORN_LEDGER/reconcile.claim/status.1"

if [ "$failures" -ne 0 ]; then
    printf '\nsysupgrade resource-ledger v2 tests failed: %s/%s failed\n' \
        "$failures" "$tests" >&2
    exit 1
fi
printf '\nsysupgrade resource-ledger v2 tests passed: %s assertions\n' "$tests"
