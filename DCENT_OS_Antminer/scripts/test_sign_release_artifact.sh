#!/bin/sh
# Integration tests for exact durable release-artifact signing.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcentos-artifact-signing.XXXXXX")
cleanup() {
    chmod -R u+w "$TEST_ROOT" 2>/dev/null || true
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

fail_test() {
    echo "release artifact signing test failed: $*" >&2
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

ARTIFACT="$TEST_ROOT/release.tar"
PRIVATE_KEY="$TEST_ROOT/private.pem"
PUBLIC_KEY="$TEST_ROOT/public.pem"
WRONG_KEY="$TEST_ROOT/wrong.pem"
WRONG_PUBLIC="$TEST_ROOT/wrong.pub"
printf 'exact release artifact bytes\n' > "$ARTIFACT"
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
    path = Path(value)
    release_io.set_windows_file_acl(path, release_io.WINDOWS_PRIVATE_FILE_SDDL)
    release_io.require_private_windows_acl(path, "test private key")
PY
fi

if "$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --pubkey "$PUBLIC_KEY" >/dev/null 2>&1; then
    fail_test "missing private key was accepted"
fi
[ ! -e "$ARTIFACT.sig" ] || fail_test "missing-key refusal left a signature"

if "$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --key "$PRIVATE_KEY" >/dev/null 2>&1; then
    fail_test "missing trusted public key was accepted"
fi
[ ! -e "$ARTIFACT.sig" ] || fail_test "missing-pubkey refusal left a signature"

"$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --key "$PRIVATE_KEY" --pubkey "$PUBLIC_KEY" >/dev/null ||
    fail_test "valid artifact signing failed"
[ "$(wc -c < "$ARTIFACT.sig" | tr -d '[:space:]')" = 64 ] ||
    fail_test "signature is not exactly 64 bytes"
openssl pkeyutl -verify -rawin -pubin -inkey "$PUBLIC_KEY" \
    -sigfile "$ARTIFACT.sig" -in "$ARTIFACT" >/dev/null ||
    fail_test "published signature does not verify"

"$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --key "$PRIVATE_KEY" --pubkey "$PUBLIC_KEY" >/dev/null ||
    fail_test "exact retry was not idempotent"

printf 'operator-owned collision\n' > "$TEST_ROOT/collision.sig"
if "$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --key "$PRIVATE_KEY" --pubkey "$PUBLIC_KEY" \
    --output-sig "$TEST_ROOT/collision.sig" >/dev/null 2>&1; then
    fail_test "foreign signature collision was replaced"
fi
[ "$(cat "$TEST_ROOT/collision.sig")" = "operator-owned collision" ] ||
    fail_test "foreign signature collision was mutated"

if "$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" "$ARTIFACT" \
    --key "$PRIVATE_KEY" --pubkey "$WRONG_PUBLIC" \
    --output-sig "$TEST_ROOT/wrong.sig" >/dev/null 2>&1; then
    fail_test "wrong trusted public key was accepted"
fi
[ ! -e "$TEST_ROOT/wrong.sig" ] ||
    fail_test "wrong-key refusal published a signature"

cp "$ARTIFACT" "$TEST_ROOT/hardlinked.tar"
if ln "$TEST_ROOT/hardlinked.tar" "$TEST_ROOT/hardlinked-alias.tar" 2>/dev/null &&
    [ "$(stat -c %h "$TEST_ROOT/hardlinked.tar" 2>/dev/null || echo 1)" -gt 1 ]; then
    if "$PYTHON" "$SCRIPT_DIR/sign_release_artifact.py" \
        "$TEST_ROOT/hardlinked.tar" \
        --key "$PRIVATE_KEY" --pubkey "$PUBLIC_KEY" >/dev/null 2>&1; then
        fail_test "multiply-linked artifact was accepted"
    fi
    [ ! -e "$TEST_ROOT/hardlinked.tar.sig" ] ||
        fail_test "hardlink refusal published a signature"
fi

grep -Fq 'durable_input=True' "$SCRIPT_DIR/sign_release_artifact.py" ||
    fail_test "artifact signer does not request durable input"
grep -Fq 'sign_release_artifact.py' "$SCRIPT_DIR/build_in_docker.sh" ||
    fail_test "AM3 tar path does not invoke the exact artifact signer"
grep -Fq 'sign_release_artifact.py" "$PORTABLE_EVIDENCE_PATH"' \
    "$SCRIPT_DIR/build_s9_release_capsule.sh" ||
    fail_test "portable evidence path does not invoke the exact artifact signer"
grep -Fq 'sign_release_artifact.py' \
    "$SCRIPT_DIR/lib/sysupgrade_package_common.sh" ||
    fail_test "shared sysupgrade manifest path does not invoke the exact signer"
grep -Fq 'sign_release_artifact.py' "$SCRIPT_DIR/package_sysupgrade.sh" ||
    fail_test "standalone sysupgrade manifest path does not invoke the exact signer"
if grep -Fq 'openssl pkey -in /signkey -pubout' \
    "$SCRIPT_DIR/build_in_docker.sh"; then
    fail_test "AM3 tar path still derives its own trust root"
fi
if grep -Fq 'openssl pkeyutl -sign -rawin' \
    "$SCRIPT_DIR/lib/sysupgrade_package_common.sh" ||
    grep -Fq 'openssl pkeyutl -sign -rawin' \
        "$SCRIPT_DIR/package_sysupgrade.sh"; then
    fail_test "sysupgrade manifest path still writes signatures directly"
fi
find "$SCRIPT_DIR/../br2_external_dcentos/board" -name post-image.sh \
    -type f -exec grep -lF 'openssl pkeyutl -sign -rawin' {} + \
    > "$TEST_ROOT/legacy-post-image-signers" || true
if [ -s "$TEST_ROOT/legacy-post-image-signers" ]; then
    fail_test "board post-image path still contains a legacy direct signer"
fi

echo "release artifact signing tests passed"
