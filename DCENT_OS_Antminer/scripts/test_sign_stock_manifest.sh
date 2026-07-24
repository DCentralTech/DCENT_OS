#!/bin/sh
# Integration tests for safe stock-manifest signature candidate generation.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-stock-signing.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "stock manifest signing test failed: $*" >&2
    exit 1
}

command -v openssl >/dev/null 2>&1 || fail_test "openssl is unavailable"
PYTHON=''
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 &&
        "$candidate" -c \
            'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
            >/dev/null 2>&1; then
        PYTHON=$candidate
        break
    fi
done
[ -n "$PYTHON" ] || fail_test "Python 3.10 or newer is unavailable"

MANIFEST="$TEST_ROOT/stock-bitmain-manifest.json"
PRIVATE_KEY="$TEST_ROOT/private.pem"
PUBLIC_KEY="$TEST_ROOT/public.pem"
WRONG_KEY="$TEST_ROOT/wrong.pem"
WRONG_PUBLIC="$TEST_ROOT/wrong.pub"
printf '{"schema":1,"images":[]}\n' > "$MANIFEST"
: > "$MANIFEST.sig"
openssl genpkey -algorithm Ed25519 -out "$PRIVATE_KEY" >/dev/null 2>&1
openssl pkey -in "$PRIVATE_KEY" -pubout -out "$PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm Ed25519 -out "$WRONG_KEY" >/dev/null 2>&1
openssl pkey -in "$WRONG_KEY" -pubout -out "$WRONG_PUBLIC" >/dev/null 2>&1

if [ "$("$PYTHON" -c 'import os; print(os.name)')" = nt ]; then
    "$PYTHON" - "$SCRIPT_DIR" "$PRIVATE_KEY" "$WRONG_KEY" <<'PY'
from pathlib import Path
import sys

sys.path.insert(0, sys.argv[1])
import release_set_publication as release_io

for value in sys.argv[2:]:
    release_io.set_windows_file_acl(
        Path(value), release_io.WINDOWS_PRIVATE_FILE_SDDL
    )
PY
fi

if sh "$SCRIPT_DIR/sign_stock_manifest.sh" "$PRIVATE_KEY" "$MANIFEST" \
    >/dev/null 2>&1; then
    fail_test "missing trusted public key was accepted"
fi
[ ! -e "$MANIFEST.sig.candidate" ] ||
    fail_test "missing-pubkey refusal left a candidate"

DCENT_RELEASE_PUBKEY_FILE="$PUBLIC_KEY" \
    sh "$SCRIPT_DIR/sign_stock_manifest.sh" "$PRIVATE_KEY" "$MANIFEST" \
    >/dev/null || fail_test "valid candidate generation failed"
[ "$(wc -c < "$MANIFEST.sig.candidate" | tr -d '[:space:]')" = 64 ] ||
    fail_test "candidate signature is not exactly 64 bytes"
[ ! -s "$MANIFEST.sig" ] ||
    fail_test "tracked-style placeholder was overwritten"
openssl pkeyutl -verify -rawin -pubin -inkey "$PUBLIC_KEY" \
    -sigfile "$MANIFEST.sig.candidate" -in "$MANIFEST" >/dev/null ||
    fail_test "candidate signature does not verify"

DCENT_RELEASE_PUBKEY_FILE="$PUBLIC_KEY" \
    sh "$SCRIPT_DIR/sign_stock_manifest.sh" "$PRIVATE_KEY" "$MANIFEST" \
    >/dev/null || fail_test "exact candidate retry was not idempotent"

if DCENT_RELEASE_PUBKEY_FILE="$WRONG_PUBLIC" \
    sh "$SCRIPT_DIR/sign_stock_manifest.sh" "$PRIVATE_KEY" "$MANIFEST" \
        "$TEST_ROOT/wrong.sig" >/dev/null 2>&1; then
    fail_test "wrong trusted public key was accepted"
fi
[ ! -e "$TEST_ROOT/wrong.sig" ] ||
    fail_test "wrong-key refusal published a candidate"

printf 'operator-owned collision\n' > "$TEST_ROOT/collision.sig"
if DCENT_RELEASE_PUBKEY_FILE="$PUBLIC_KEY" \
    sh "$SCRIPT_DIR/sign_stock_manifest.sh" "$PRIVATE_KEY" "$MANIFEST" \
        "$TEST_ROOT/collision.sig" >/dev/null 2>&1; then
    fail_test "foreign candidate collision was replaced"
fi
[ "$(cat "$TEST_ROOT/collision.sig")" = "operator-owned collision" ] ||
    fail_test "foreign candidate collision was mutated"

grep -Fq 'sign_release_artifact.py' "$SCRIPT_DIR/sign_stock_manifest.sh" ||
    fail_test "stock helper does not reuse the exact signer"
if grep -Eq 'pkeyutl[[:space:]]+-sign' "$SCRIPT_DIR/sign_stock_manifest.sh"; then
    fail_test "stock helper still writes signatures directly"
fi

echo "stock manifest signing tests passed"
