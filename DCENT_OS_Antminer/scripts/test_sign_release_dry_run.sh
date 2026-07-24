#!/bin/sh
# End-to-end checks for the host-local release-signing rehearsal.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-signing-rehearsal.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "release signing rehearsal test failed: $*" >&2
    exit 1
}

if DCENT_RELEASE_SIGNING_KEY="$TEST_ROOT/must-not-be-read.pem" \
    sh "$SCRIPT_DIR/sign_release_dry_run.sh" \
        --target am1-s9 --out "$TEST_ROOT/refused" >/dev/null 2>&1; then
    fail_test "rehearsal accepted a configured release signing key"
fi
[ ! -e "$TEST_ROOT/refused/sign-release-dry-run-am1-s9.txt" ] ||
    fail_test "real-key refusal emitted a rehearsal report"

sh "$SCRIPT_DIR/sign_release_dry_run.sh" \
    --target am1-s9 --out "$TEST_ROOT/audit" >/dev/null ||
    fail_test "host-local signing rehearsal failed"
REPORT="$TEST_ROOT/audit/sign-release-dry-run-am1-s9.txt"
[ -f "$REPORT" ] || fail_test "rehearsal report is missing"
grep -Fq 'PASS - dry-run rehearsal complete' "$REPORT" ||
    fail_test "rehearsal report does not record success"
grep -Fq 'sign_release_artifact.py' "$SCRIPT_DIR/sign_release_dry_run.sh" ||
    fail_test "rehearsal does not reuse the exact signer"
if grep -Eq 'pkeyutl[[:space:]]+-sign' \
    "$SCRIPT_DIR/sign_release_dry_run.sh"; then
    fail_test "rehearsal still writes signatures directly"
fi

echo "release signing rehearsal tests passed"
