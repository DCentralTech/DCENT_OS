#!/usr/bin/env bash
# Sign a canonical DCENT_OS release receipt with the pinned Ed25519 keypair.
#
# The signature authenticates the receipt bytes only. Any source-completeness,
# reproducibility, SBOM, SPDX, licensing, or advisory claims remain exactly as
# bounded by the signed receipt's own schema.

set -euo pipefail

usage() {
    echo "Usage: sign_release_receipt.sh <receipt.json> <private-key.pem> <trusted-public-key.pem> [signature.out]" >&2
}

[ "$#" -ge 3 ] && [ "$#" -le 4 ] || {
    usage
    exit 2
}

RECEIPT=$1
PRIVATE_KEY=$2
PUBLIC_KEY=$3
SIGNATURE=${4:-${RECEIPT}.sig}

for pair in "receipt:$RECEIPT" "private key:$PRIVATE_KEY" "trusted public key:$PUBLIC_KEY"; do
    label=${pair%%:*}
    path=${pair#*:}
    [ -f "$path" ] && [ ! -L "$path" ] || {
        echo "ERROR: release receipt signing requires a non-symlink regular $label: $path" >&2
        exit 1
    }
done

case "$SIGNATURE" in
    "$RECEIPT"|"$PRIVATE_KEY"|"$PUBLIC_KEY")
        echo "ERROR: signature output must not overwrite a signing input" >&2
        exit 1
        ;;
esac

command -v openssl >/dev/null 2>&1 || {
    echo "ERROR: openssl is required for release receipt signing" >&2
    exit 1
}

umask 077
TMP_SIGNATURE=$(mktemp "${SIGNATURE}.tmp.XXXXXX")
trap 'rm -f "$TMP_SIGNATURE"' EXIT INT TERM
rm -f "$SIGNATURE"

openssl pkeyutl -sign -rawin \
    -inkey "$PRIVATE_KEY" \
    -in "$RECEIPT" \
    -out "$TMP_SIGNATURE"

openssl pkeyutl -verify -rawin -pubin \
    -inkey "$PUBLIC_KEY" \
    -sigfile "$TMP_SIGNATURE" \
    -in "$RECEIPT" >/dev/null || {
        echo "ERROR: release receipt signature does not verify against the trusted public key" >&2
        exit 1
    }

mv -f "$TMP_SIGNATURE" "$SIGNATURE"
[ -f "$SIGNATURE" ] && [ ! -L "$SIGNATURE" ] || {
    echo "ERROR: published release receipt signature is not a regular file" >&2
    rm -f "$SIGNATURE"
    exit 1
}
openssl pkeyutl -verify -rawin -pubin \
    -inkey "$PUBLIC_KEY" \
    -sigfile "$SIGNATURE" \
    -in "$RECEIPT" >/dev/null || {
        echo "ERROR: published release receipt signature failed final verification" >&2
        rm -f "$SIGNATURE"
        exit 1
    }
trap - EXIT INT TERM
echo "Signed release receipt: $RECEIPT -> $SIGNATURE"
