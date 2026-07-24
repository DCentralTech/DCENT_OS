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

select_manifest_python() {
    if command -v python3 >/dev/null 2>&1 &&
        python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 8))' \
            >/dev/null 2>&1; then
        MANIFEST_PYTHON=python3
        MANIFEST_PYTHON_LAUNCHER=0
    elif command -v python >/dev/null 2>&1 &&
        python -c 'import sys; raise SystemExit(sys.version_info < (3, 8))' \
            >/dev/null 2>&1; then
        MANIFEST_PYTHON=python
        MANIFEST_PYTHON_LAUNCHER=0
    elif command -v py >/dev/null 2>&1 &&
        py -3 -c 'import sys; raise SystemExit(sys.version_info < (3, 8))' \
            >/dev/null 2>&1; then
        MANIFEST_PYTHON=py
        MANIFEST_PYTHON_LAUNCHER=1
    else
        fail "Python 3.8 or newer is required"
    fi
}

run_manifest_python() {
    if [ "$MANIFEST_PYTHON_LAUNCHER" = 1 ]; then
        "$MANIFEST_PYTHON" -3 "$@"
    else
        "$MANIFEST_PYTHON" "$@"
    fi
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
select_manifest_python

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
ARCHIVE_ADMISSION_HELPER="$SCRIPT_DIR/lib/sysupgrade_archive_admission.sh"
MANIFEST_JSON_HELPER="$SCRIPT_DIR/lib/sysupgrade_manifest_json.py"
[ -r "$ARCHIVE_ADMISSION_HELPER" ] || fail "Archive admission helper is missing: $ARCHIVE_ADMISSION_HELPER"
[ -r "$MANIFEST_JSON_HELPER" ] || fail "Manifest JSON helper is missing: $MANIFEST_JSON_HELPER"
# shellcheck source=lib/sysupgrade_archive_admission.sh
. "$ARCHIVE_ADMISSION_HELPER"
command -v dcent_sysupgrade_archive_admit >/dev/null 2>&1 \
    || fail "Archive admission helper did not provide its required API"

[ -f "$PACKAGE" ] || fail "Package not found: $PACKAGE"
[ -f "$PUBKEY" ] || fail "Public key not found: $PUBKEY"
openssl pkey -pubin -in "$PUBKEY" -noout >/dev/null 2>&1 \
    || fail "Public key is not a valid PEM public key: $PUBKEY"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT INT TERM

TAR_LIST="$TMPDIR/tar.list"
dcent_sysupgrade_archive_admit "$PACKAGE" "$EXPECTED_BOARD" "$TMPDIR" \
    || fail "Package failed pre-extraction archive admission"
tar tf "$PACKAGE" > "$TAR_LIST" || fail "Package is not a readable tar archive"

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
run_manifest_python "$MANIFEST_JSON_HELPER" validate "$MANIFEST" \
    || fail "Manifest failed semantic/canonical JSON admission"

manifest_string() {
    key=$1
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -n 1
}

manifest_boolean() {
    key=$1
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\(true\|false\)[[:space:]]*[,}].*/\1/p' "$MANIFEST" | head -n 1
}

manifest_integer() {
    key=$1
    sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\)[[:space:]]*[,}].*/\1/p' "$MANIFEST" | head -n 1
}

manifest_key_count() {
    key=$1
    awk -v needle="\"$key\"" '
        {
            line = $0
            while ((position = index(line, needle)) > 0) {
                count++
                line = substr(line, position + length(needle))
            }
        }
        END { print count + 0 }
    ' "$MANIFEST"
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
    [ "$sha_len" = "64" ] || fail "Manifest $label sha256 is not exactly 64 lowercase hex characters"
    case "$expected_sha" in
        *[!0123456789abcdef]*)
            fail "Manifest $label sha256 is not exactly 64 lowercase hex characters"
            ;;
    esac

    actual_sha=$(sha256sum "$file" | awk '{print $1}')
    [ "$actual_sha" = "$expected_sha" ] || fail "$label sha256 mismatch for $path"

    expected_size=$(payload_number_field "$path" size || echo)
    [ -n "$expected_size" ] || fail "Manifest $label payload missing integer size: $path"
    actual_size=$(wc -c < "$file" | tr -d ' ')
    [ "$actual_size" = "$expected_size" ] || fail "$label size mismatch for $path"
}

PACKAGE_KEY_SHA=$(sha256sum "$PACKAGE_PUBKEY" | awk '{print $1}')
INPUT_KEY_SHA=$(sha256sum "$PUBKEY" | awk '{print $1}')
[ "$PACKAGE_KEY_SHA" = "$INPUT_KEY_SHA" ] || fail "Package release_ed25519.pub does not match $PUBKEY"

for authority_key in schema manifest_profile product package_type installable artifact_maturity board board_target version; do
    [ "$(manifest_key_count "$authority_key")" = "1" ] \
        || fail "Manifest must contain exactly one '$authority_key' authority field"
done
[ "$(manifest_key_count verification_key)" = "1" ] \
    || fail "Manifest authority-v1 must contain exactly one 'verification_key' payload"
for payload_key in kernel rootfs metadata; do
    [ "$(manifest_key_count "$payload_key")" = "1" ] \
        || fail "Manifest authority-v1 must contain exactly one '$payload_key' payload"
done
if [ -f "$SUBDIR/fpga_bitstream.bit" ]; then
    [ "$(manifest_key_count bitstream)" = "1" ] \
        || fail "Manifest authority-v1 must contain exactly one 'bitstream' payload when the FPGA leaf is present"
else
    [ "$(manifest_key_count bitstream)" = "0" ] \
        || fail "Manifest authority-v1 must not declare a bitstream payload when the FPGA leaf is absent"
fi
STATUS_COUNT=$(manifest_key_count status)
[ "$STATUS_COUNT" = "1" ] \
    || fail "Manifest authority-v1 must contain exactly one 'status' field"
for unsupported_chain_key in ota_intermediate_cert ota_revoked_intermediates; do
    [ "$(manifest_key_count "$unsupported_chain_key")" = "0" ] \
        || fail "Manifest authority-v1 requires a direct release-root signature and forbids '$unsupported_chain_key'; certificate validity has no trusted-time authority on Zynq"
done

SCHEMA=$(manifest_integer schema || echo)
MANIFEST_PROFILE=$(manifest_string manifest_profile || echo)
PRODUCT=$(manifest_string product || echo)
PACKAGE_TYPE=$(manifest_string package_type || echo)
INSTALLABLE=$(manifest_boolean installable || echo)
ARTIFACT_MATURITY=$(manifest_string artifact_maturity || echo)
MANIFEST_BOARD=$(manifest_string board || echo)
MANIFEST_BOARD_TARGET=$(manifest_string board_target || echo)
VERSION=$(manifest_string version || echo)
MANIFEST_STATUS=$(manifest_string status || echo)
[ -n "$MANIFEST_STATUS" ] || fail "Manifest status must be a non-empty string"
MANIFEST_STATUS_TRIMMED=$(printf '%s' "$MANIFEST_STATUS" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
[ "$MANIFEST_STATUS" = "$MANIFEST_STATUS_TRIMMED" ] \
    || fail "Manifest status must not contain surrounding whitespace"

[ "$SCHEMA" = "1" ] || fail "Manifest schema '$SCHEMA' is unsupported; expected integer 1"
[ "$MANIFEST_PROFILE" = "dcentos.sysupgrade-authority/v1" ] || fail "Manifest profile '$MANIFEST_PROFILE' is unsupported"
[ "$MANIFEST_STATUS" != "lab_unsigned" ] || fail "Manifest authority-v1 forbids status=lab_unsigned"
[ "$PRODUCT" = "$EXPECTED_PRODUCT" ] || fail "Manifest product '$PRODUCT' does not match expected '$EXPECTED_PRODUCT'"
[ "$PACKAGE_TYPE" = "sysupgrade" ] || fail "Manifest package_type '$PACKAGE_TYPE' is not sysupgrade"
[ "$INSTALLABLE" = "true" ] || fail "Manifest must explicitly declare installable=true"
[ "$ARTIFACT_MATURITY" = "experimental" ] || fail "Manifest artifact_maturity '$ARTIFACT_MATURITY' does not match the experimental sysupgrade policy"
[ -n "$MANIFEST_BOARD" ] || fail "Manifest missing board"
[ -n "$MANIFEST_BOARD_TARGET" ] || fail "Manifest missing board_target"
[ "$MANIFEST_BOARD" = "$MANIFEST_BOARD_TARGET" ] || fail "Manifest board '$MANIFEST_BOARD' conflicts with board_target '$MANIFEST_BOARD_TARGET'"
[ "$MANIFEST_BOARD" = "$BOARD_FROM_DIR" ] || fail "Manifest board '$MANIFEST_BOARD' does not match tar board '$BOARD_FROM_DIR'"
[ "$MANIFEST_BOARD_TARGET" = "$BOARD_FROM_DIR" ] || fail "Manifest board_target '$MANIFEST_BOARD_TARGET' does not match tar board '$BOARD_FROM_DIR'"
[ -n "$VERSION" ] || fail "Manifest version must be a non-empty string"
VERSION_TRIMMED=$(printf '%s' "$VERSION" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
[ "$VERSION" = "$VERSION_TRIMMED" ] || fail "Manifest version must not contain surrounding whitespace"

if [ -n "$EXPECTED_BOARD" ]; then
    [ "$MANIFEST_BOARD_TARGET" = "$EXPECTED_BOARD" ] || fail "Manifest board_target '$MANIFEST_BOARD_TARGET' does not match expected '$EXPECTED_BOARD'"
fi

validate_payload_hash "$SUBDIR_NAME/kernel" "kernel"
validate_payload_hash "$SUBDIR_NAME/root" "rootfs"
validate_payload_hash "$SUBDIR_NAME/METADATA" "metadata"
validate_payload_hash "$SUBDIR_NAME/release_ed25519.pub" "verification_key"
if [ -f "$SUBDIR/fpga_bitstream.bit" ]; then
    validate_payload_hash "$SUBDIR_NAME/fpga_bitstream.bit" "bitstream"
fi

openssl pkeyutl -verify -rawin -pubin -inkey "$PUBKEY" -sigfile "$SIG" -in "$MANIFEST" >/dev/null \
    || fail "MANIFEST.sig verification failed"

echo "Package manifest and signature validated: $PACKAGE"
