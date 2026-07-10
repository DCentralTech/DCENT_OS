#!/bin/sh
# verify_sysupgrade_signature.sh - verify a signed DCENT_OS sysupgrade package

set -eu

usage() {
    echo "Usage: $0 <dcentos-sysupgrade.tar> <release-pubkey.pem> [expected-board]" >&2
}

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

[ "$#" -ge 2 ] || { usage; exit 2; }

PACKAGE=$1
PUBKEY=$2
EXPECTED_BOARD=${3:-${DCENT_EXPECTED_BOARD:-}}
EXPECTED_PRODUCT=${DCENT_EXPECTED_PRODUCT:-DCENT_OS}

[ -n "$EXPECTED_BOARD" ] || {
    if [ -r /etc/dcentos/board_target ]; then
        EXPECTED_BOARD=$(cat /etc/dcentos/board_target 2>/dev/null || echo)
    fi
}

require_cmd awk
require_cmd openssl
require_cmd sed
require_cmd sha256sum
require_cmd sort
require_cmd tar

[ -f "$PACKAGE" ] || fail "Package not found: $PACKAGE"
[ -f "$PUBKEY" ] || fail "Public key not found: $PUBKEY"
openssl pkey -pubin -in "$PUBKEY" -noout >/dev/null 2>&1 \
    || fail "Public key is not a valid PEM public key: $PUBKEY"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT INT TERM

TAR_LIST="$TMPDIR/tar.list"
tar tf "$PACKAGE" > "$TAR_LIST" || fail "Package is not a readable tar archive"

while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    case "$entry" in
        /*|../*|*/../*|*/..|..)
            fail "Unsafe tar entry path: $entry"
            ;;
    esac
done < "$TAR_LIST"

SUBDIR_NAME=$(sed -n 's#^\(sysupgrade-[^/][^/]*\)/.*#\1#p' "$TAR_LIST" | sort -u)
SUBDIR_COUNT=$(printf '%s\n' "$SUBDIR_NAME" | sed '/^$/d' | wc -l | tr -d ' ')
[ "$SUBDIR_COUNT" = "1" ] || fail "Package must contain exactly one sysupgrade-* directory"
[ -n "$SUBDIR_NAME" ] || fail "Package missing sysupgrade-* directory"

BOARD_FROM_DIR=${SUBDIR_NAME#sysupgrade-}
[ -n "$BOARD_FROM_DIR" ] && [ "$BOARD_FROM_DIR" != "$SUBDIR_NAME" ] || fail "Invalid sysupgrade directory: $SUBDIR_NAME"

tar xf "$PACKAGE" -C "$TMPDIR" || fail "Failed to extract package"

SUBDIR="$TMPDIR/$SUBDIR_NAME"
MANIFEST="$SUBDIR/MANIFEST.json"
SIG="$SUBDIR/MANIFEST.sig"
PACKAGE_PUBKEY="$SUBDIR/release_ed25519.pub"

[ -d "$SUBDIR" ] || fail "Package missing $SUBDIR_NAME directory"
[ -f "$MANIFEST" ] || fail "Package missing MANIFEST.json"
[ -f "$SIG" ] || fail "Package missing MANIFEST.sig"
[ -f "$PACKAGE_PUBKEY" ] || fail "Package missing release_ed25519.pub"

manifest_string() {
    key=$1
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -n 1
}

payload_block_for_path() {
    path=$1
    awk -v path="$path" '
        BEGIN { RS = "}" }
        index($0, "\"path\"") && index($0, "\"" path "\"") {
            print $0 "}"
            found = 1
            exit
        }
        END {
            if (!found) {
                exit 1
            }
        }
    ' "$MANIFEST" 2>/dev/null || true
}

payload_string_field() {
    path=$1
    field=$2
    block=$(payload_block_for_path "$path")
    [ -n "$block" ] || return 1
    printf '%s\n' "$block" | sed -n 's/.*"'"$field"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

payload_number_field() {
    path=$1
    field=$2
    block=$(payload_block_for_path "$path")
    [ -n "$block" ] || return 1
    printf '%s\n' "$block" | sed -n 's/.*"'"$field"'"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1
}

validate_payload_path() {
    path=$1
    case "$path" in
        "$SUBDIR_NAME"/*) ;;
        *) fail "Manifest payload path outside $SUBDIR_NAME: $path" ;;
    esac
    case "$path" in
        /*|../*|*/../*|*/..|..)
            fail "Manifest payload path is unsafe: $path"
            ;;
    esac
}

validate_payload_hash() {
    path=$1
    label=$2

    validate_payload_path "$path"
    file="$TMPDIR/$path"
    [ -f "$file" ] || fail "Manifest references missing $label payload: $path"

    expected_sha=$(payload_string_field "$path" sha256 || echo)
    [ -n "$expected_sha" ] || fail "Manifest $label payload missing sha256: $path"
    sha_len=$(printf '%s' "$expected_sha" | wc -c | tr -d ' ')
    [ "$sha_len" = "64" ] || fail "Manifest $label sha256 is not 64 hex characters"
    case "$expected_sha" in
        *[!0123456789abcdefABCDEF]*)
            fail "Manifest $label sha256 contains non-hex characters"
            ;;
    esac

    actual_sha=$(sha256sum "$file" | awk '{print $1}')
    expected_sha_lc=$(printf '%s' "$expected_sha" | tr 'A-F' 'a-f')
    [ "$actual_sha" = "$expected_sha_lc" ] || fail "$label sha256 mismatch for $path"

    expected_size=$(payload_number_field "$path" size || echo)
    if [ -n "$expected_size" ]; then
        actual_size=$(wc -c < "$file" | tr -d ' ')
        [ "$actual_size" = "$expected_size" ] || fail "$label size mismatch for $path"
    fi
}

PACKAGE_KEY_SHA=$(sha256sum "$PACKAGE_PUBKEY" | awk '{print $1}')
INPUT_KEY_SHA=$(sha256sum "$PUBKEY" | awk '{print $1}')
[ "$PACKAGE_KEY_SHA" = "$INPUT_KEY_SHA" ] || fail "Package release_ed25519.pub does not match $PUBKEY"

PRODUCT=$(manifest_string product || echo)
PACKAGE_TYPE=$(manifest_string package_type || echo)
MANIFEST_BOARD=$(manifest_string board || echo)
MANIFEST_BOARD_TARGET=$(manifest_string board_target || echo)
VERSION=$(manifest_string version || echo)

[ "$PRODUCT" = "$EXPECTED_PRODUCT" ] || fail "Manifest product '$PRODUCT' does not match expected '$EXPECTED_PRODUCT'"
[ "$PACKAGE_TYPE" = "sysupgrade" ] || fail "Manifest package_type '$PACKAGE_TYPE' is not sysupgrade"
[ -n "$MANIFEST_BOARD" ] || fail "Manifest missing board"
[ -n "$MANIFEST_BOARD_TARGET" ] || fail "Manifest missing board_target"
[ "$MANIFEST_BOARD" = "$BOARD_FROM_DIR" ] || fail "Manifest board '$MANIFEST_BOARD' does not match tar board '$BOARD_FROM_DIR'"
[ "$MANIFEST_BOARD_TARGET" = "$BOARD_FROM_DIR" ] || fail "Manifest board_target '$MANIFEST_BOARD_TARGET' does not match tar board '$BOARD_FROM_DIR'"
[ -n "$VERSION" ] || fail "Manifest version must be a non-empty string"

if [ -n "$EXPECTED_BOARD" ]; then
    [ "$MANIFEST_BOARD_TARGET" = "$EXPECTED_BOARD" ] || fail "Manifest board_target '$MANIFEST_BOARD_TARGET' does not match expected '$EXPECTED_BOARD'"
fi

validate_payload_hash "$SUBDIR_NAME/kernel" "kernel"
validate_payload_hash "$SUBDIR_NAME/root" "rootfs"
validate_payload_hash "$SUBDIR_NAME/METADATA" "metadata"
validate_payload_hash "$SUBDIR_NAME/release_ed25519.pub" "verification_key"

openssl pkeyutl -verify -rawin -pubin -inkey "$PUBKEY" -sigfile "$SIG" -in "$MANIFEST" >/dev/null \
    || fail "MANIFEST.sig verification failed"

echo "Package manifest and signature validated: $PACKAGE"
